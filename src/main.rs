//! Orbit — overlay estilo Game Bar para Linux.
//!
//! Fica residente com a janela já construída (abrir = só mapear a superfície, instantâneo).
//! `orbit --daemon` inicia escondido; `orbit` alterna (útil para um atalho de teclado);
//! `orbit --quit` encerra. O botão Guide do controle também alterna.

mod audio;
mod bluetooth;
mod capture;
mod gamepad;
mod games;
mod system;
mod util;
mod windows;

use gamepad::{Dir, PadEvent};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

const APP_ID: &str = "io.github.orbit";
const CSS: &str = include_str!("style.css");
const TABS: &[(&str, &str)] = &[
    ("games", "Jogos"),
    ("windows", "Programas"),
    ("audio", "Áudio"),
    ("bluetooth", "Bluetooth"),
    ("capture", "Captura"),
    ("system", "Sistema"),
];

/// O que as páginas precisam para fechar o overlay.
#[derive(Clone)]
pub struct Ctl {
    pub window: gtk::ApplicationWindow,
    panel: gtk::Box,
    grab: gamepad::Grabber,
}

impl Ctl {
    pub fn hide(&self) {
        self.window.set_visible(false);
        self.panel.remove_css_class("enter");
        self.grab.set(false);
    }
}

struct Ui {
    ctl: Ctl,
    layer: bool,
    stack: gtk::Stack,
    tabs: Vec<(&'static str, gtk::Button)>,
    clock: gtk::Label,
    battery: gtk::Label,
    badge: gtk::Label,
    games: games::GamesPage,
    audio: Rc<audio::AudioPage>,
    capture: Rc<capture::CapturePage>,
    windows: Rc<windows::WindowsPage>,
    bluetooth: Rc<bluetooth::BluetoothPage>,
    close_hint: gtk::Box,
    close_hint_text: gtk::Label,
    repeat: RefCell<Option<glib::SourceId>>,
    last_guide: Cell<Option<Instant>>,
    /// Último widget focado em cada aba, para voltar ao mesmo lugar.
    last_focus: RefCell<HashMap<String, glib::WeakRef<gtk::Widget>>>,
    /// Já existe uma recuperação de foco agendada.
    refocus_pending: Cell<bool>,
}

thread_local! {
    static UI: RefCell<Option<Rc<Ui>>> = const { RefCell::new(None) };
}

impl Ui {
    fn visible(&self) -> bool {
        self.ctl.window.is_visible()
    }

    fn show(&self) {
        // Atualiza dados dinâmicos (o áudio é assíncrono, não atrasa a abertura).
        self.games.refresh();
        self.audio.refresh();
        self.capture.refresh();
        self.update_header();

        let w = &self.ctl.window;
        if !self.layer {
            w.fullscreen();
        }
        w.set_visible(true);
        w.present();
        self.ctl.panel.add_css_class("enter");
        w.set_focus_visible(true);
        if self.focus_in_page().is_none() {
            self.restore_focus();
        }
        self.ctl.grab.set(true);
        self.bluetooth
            .set_active(self.stack.visible_child_name().as_deref() == Some("bluetooth"));
    }

    fn hide(&self) {
        self.stop_repeat();
        self.ctl.hide();
        self.bluetooth.set_active(false);
    }

    fn toggle(&self) {
        if self.visible() {
            self.hide();
        } else {
            self.show();
        }
    }

    fn switch_tab(&self, name: &str) {
        let same = self.stack.visible_child_name().as_deref() == Some(name);
        if !same {
            // Tira o foco antes da troca: senão o GtkStack move o foco sozinho e,
            // durante a animação, a página que está saindo ainda aceita foco.
            gtk::prelude::GtkWindowExt::set_focus(&self.ctl.window, None::<&gtk::Widget>);
            self.stack.set_visible_child_name(name);
        }
        // Botão X: fecha programa / esquece dispositivo, conforme a aba.
        match name {
            "windows" => self.close_hint_text.set_text("Fechar programa"),
            "bluetooth" => self.close_hint_text.set_text("Esquecer dispositivo"),
            _ => {}
        }
        self.close_hint.set_visible(matches!(name, "windows" | "bluetooth"));
        self.bluetooth.set_active(self.visible() && name == "bluetooth");
        for (n, b) in &self.tabs {
            if *n == name {
                b.add_css_class("active");
            } else {
                b.remove_css_class("active");
            }
        }
        if !same || self.focus_in_page().is_none() {
            self.restore_focus();
        }
    }

    fn cycle_tab(&self, delta: i32) {
        let cur = self.stack.visible_child_name().unwrap_or_default();
        let idx = TABS.iter().position(|(n, _)| *n == cur.as_str()).unwrap_or(0) as i32;
        let next = (idx + delta).rem_euclid(TABS.len() as i32) as usize;
        self.switch_tab(TABS[next].0);
    }

    /// Widget focado, se ele estiver na página visível e puder receber foco.
    fn focus_in_page(&self) -> Option<gtk::Widget> {
        let page = self.stack.visible_child()?;
        let f = gtk::prelude::GtkWindowExt::focus(&self.ctl.window)?;
        (f.is_ancestor(&page) && usable(&f)).then_some(f)
    }

    /// Volta ao último item focado desta aba; se ele não existir mais, foca o primeiro.
    fn restore_focus(&self) {
        let (Some(page), Some(name)) = (self.stack.visible_child(), self.stack.visible_child_name()) else {
            return;
        };
        let saved = self.last_focus.borrow().get(name.as_str()).and_then(|w| w.upgrade());
        if let Some(w) = saved {
            if w.is_ancestor(&page) && usable(&w) && w.grab_focus() {
                return;
            }
        }
        self.focus_first();
    }

    fn focus_first(&self) {
        if let Some(page) = self.stack.visible_child() {
            gtk::prelude::GtkWindowExt::set_focus(&self.ctl.window, None::<&gtk::Widget>);
            page.child_focus(gtk::DirectionType::TabForward);
        }
    }

    /// Chamado quando o foco da janela muda: guarda a posição por aba e, se o foco
    /// se perder (widget removido/desabilitado), recupera na próxima volta do loop.
    fn on_focus_changed(self: &Rc<Self>) {
        if !self.visible() {
            return;
        }
        if let (Some(f), Some(name)) = (self.focus_in_page(), self.stack.visible_child_name()) {
            self.last_focus.borrow_mut().insert(name.to_string(), f.downgrade());
            return;
        }
        if self.refocus_pending.replace(true) {
            return;
        }
        let w = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            let Some(ui) = w.upgrade() else { return };
            ui.refocus_pending.set(false);
            if ui.visible() && ui.focus_in_page().is_none() {
                ui.restore_focus();
            }
        });
    }

    fn move_dir(&self, d: Dir) {
        let w = &self.ctl.window;
        if !w.is_visible() {
            return;
        }
        // Cursor perdido: reaparece no lugar certo em vez de "pular" para qualquer canto.
        let Some(focus) = self.focus_in_page() else {
            self.restore_focus();
            return;
        };
        // Em sliders, esquerda/direita ajustam o valor.
        if let Some(scale) = focus.downcast_ref::<gtk::Scale>() {
            if matches!(d, Dir::Left | Dir::Right) {
                let step = if d == Dir::Right { 5.0 } else { -5.0 };
                scale.set_value((scale.value() + step).clamp(0.0, 100.0));
                return;
            }
        }
        let dt = match d {
            Dir::Up => gtk::DirectionType::Up,
            Dir::Down => gtk::DirectionType::Down,
            Dir::Left => gtk::DirectionType::Left,
            Dir::Right => gtk::DirectionType::Right,
        };
        // Navega só dentro da página visível (nunca na que está saindo na animação).
        if let Some(page) = self.stack.visible_child() {
            page.child_focus(dt);
        }
    }

    fn stop_repeat(&self) {
        if let Some(id) = self.repeat.borrow_mut().take() {
            id.remove();
        }
    }

    /// Move uma vez e, se segurar, repete (380 ms de atraso, depois a cada 110 ms).
    fn on_nav(self: &Rc<Self>, d: Dir, pressed: bool) {
        self.stop_repeat();
        if !pressed {
            return;
        }
        self.move_dir(d);
        let w = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(Duration::from_millis(380), move || {
            let Some(ui) = w.upgrade() else { return };
            ui.repeat.borrow_mut().take(); // este timeout já terminou
            let w2 = Rc::downgrade(&ui);
            let id = glib::timeout_add_local(Duration::from_millis(110), move || match w2.upgrade() {
                Some(ui) => {
                    ui.move_dir(d);
                    glib::ControlFlow::Continue
                }
                None => glib::ControlFlow::Break,
            });
            *ui.repeat.borrow_mut() = Some(id);
        });
        *self.repeat.borrow_mut() = Some(id);
    }

    fn on_pad(self: &Rc<Self>, ev: PadEvent) {
        if let PadEvent::Guide = ev {
            // Debounce: evita abrir/fechar duas vezes se o evento vier duplicado.
            let now = Instant::now();
            if let Some(t) = self.last_guide.get() {
                if now.duration_since(t) < Duration::from_millis(300) {
                    return;
                }
            }
            self.last_guide.set(Some(now));
            self.toggle();
            return;
        }
        if !self.visible() {
            return;
        }
        // Mouse/clique pode apagar o anel de foco; o controle sempre o mostra de volta.
        self.ctl.window.set_focus_visible(true);
        match ev {
            PadEvent::Nav(d, p) => self.on_nav(d, p),
            PadEvent::Accept => match self.focus_in_page() {
                Some(f) => {
                    f.activate();
                }
                None => self.restore_focus(),
            },
            PadEvent::Back => self.hide(),
            PadEvent::PrevTab => self.cycle_tab(-1),
            PadEvent::NextTab => self.cycle_tab(1),
            PadEvent::Close => match self.stack.visible_child_name().as_deref() {
                Some("windows") => self.windows.close_focused(),
                Some("bluetooth") => self.bluetooth.forget_focused(),
                _ => {}
            },
            PadEvent::Guide => {}
        }
    }

    fn update_header(&self) {
        if let Ok(s) = glib::DateTime::now_local().and_then(|d| d.format("%H:%M")) {
            self.clock.set_text(&s);
        }
        match controller_battery() {
            Some(p) => {
                self.battery.set_text(&format!("🎮 {p}%"));
                self.battery.set_visible(true);
            }
            None => self.battery.set_visible(false),
        }
        match self.capture.badge() {
            Some(b) => {
                self.badge.set_text(b);
                self.badge.set_visible(true);
            }
            None => self.badge.set_visible(false),
        }
    }
}

/// Pode receber o cursor agora (visível, mapeado e habilitado).
fn usable(w: &gtk::Widget) -> bool {
    w.is_visible() && w.is_mapped() && w.is_sensitive() && w.is_focusable()
}

/// Bateria de controle (power_supply com scope=Device), se o driver expuser.
fn controller_battery() -> Option<u32> {
    std::fs::read_dir("/sys/class/power_supply").ok()?.flatten().find_map(|e| {
        let p = e.path();
        let scope = std::fs::read_to_string(p.join("scope")).ok()?;
        if scope.trim() != "Device" {
            return None;
        }
        std::fs::read_to_string(p.join("capacity")).ok()?.trim().parse().ok()
    })
}

#[cfg(feature = "layer-shell")]
fn setup_layer(w: &gtk::ApplicationWindow) -> bool {
    use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
    if render_docs_dir().is_some() || !gtk4_layer_shell::is_supported() {
        return false;
    }
    w.init_layer_shell();
    w.set_layer(Layer::Overlay);
    w.set_namespace(Some("orbit"));
    for e in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
        w.set_anchor(e, true);
    }
    w.set_exclusive_zone(-1);
    w.set_keyboard_mode(KeyboardMode::Exclusive);
    true
}

#[cfg(not(feature = "layer-shell"))]
fn setup_layer(_: &gtk::ApplicationWindow) -> bool {
    false
}

fn hint(text: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.add_css_class("hint-key");
    l
}

fn footer_item(key: &str, text: &str) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    b.append(&hint(key));
    let l = gtk::Label::new(Some(text));
    l.add_css_class("dim");
    b.append(&l);
    b
}

fn build_ui(app: &gtk::Application) -> Rc<Ui> {
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title("Orbit")
        .build();
    window.add_css_class("orbit");
    let layer = setup_layer(&window);
    if !layer {
        eprintln!("orbit: layer-shell indisponível, usando janela comum em tela cheia");
        window.set_decorated(false);
        window.add_css_class("fallback");
    }

    // Fundo escurecido em tela cheia + painel central.
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.set_hexpand(true);
    root.set_vexpand(true);

    let panel = gtk::Box::new(gtk::Orientation::Vertical, 14);
    panel.add_css_class("panel");
    panel.set_halign(gtk::Align::Center);
    panel.set_valign(gtk::Align::Center);
    panel.set_vexpand(true);
    panel.set_size_request(960, 620);
    root.append(&panel);

    let ctl = Ctl {
        window: window.clone(),
        panel: panel.clone(),
        grab: gamepad::Grabber::default(),
    };

    // Cabeçalho
    let header = gtk::CenterBox::new();
    let brand = gtk::Label::new(None);
    brand.set_markup("<span foreground='#8b7bff'>◉</span>  ORBIT");
    brand.add_css_class("brand");
    header.set_start_widget(Some(&brand));

    let tabs_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    tabs_box.append(&hint("LB"));
    let mut tabs = Vec::new();
    for (name, title) in TABS {
        let b = gtk::Button::with_label(title);
        b.add_css_class("tab");
        b.set_can_focus(false);
        b.set_focus_on_click(false);
        tabs_box.append(&b);
        tabs.push((*name, b));
    }
    tabs_box.append(&hint("RB"));
    header.set_center_widget(Some(&tabs_box));

    let right = gtk::Box::new(gtk::Orientation::Horizontal, 14);
    let badge = gtk::Label::new(None);
    badge.add_css_class("rec");
    badge.set_visible(false);
    let battery = gtk::Label::new(None);
    battery.add_css_class("dim");
    let clock = gtk::Label::new(None);
    clock.add_css_class("clock");
    right.append(&badge);
    right.append(&battery);
    right.append(&clock);
    header.set_end_widget(Some(&right));
    panel.append(&header);

    // Páginas
    let stack = gtk::Stack::new();
    stack.set_transition_type(gtk::StackTransitionType::SlideLeftRight);
    stack.set_transition_duration(160);
    stack.set_vexpand(true);

    let games = games::GamesPage::new(ctl.clone());
    let audio = audio::AudioPage::new();
    let capture = capture::CapturePage::new(ctl.clone());
    let system = system::page(ctl.clone(), app);
    let windows = windows::WindowsPage::new(ctl.clone());
    let bluetooth = bluetooth::BluetoothPage::new();
    stack.add_named(&games.root, Some("games"));
    stack.add_named(&windows.root, Some("windows"));
    stack.add_named(&audio.root, Some("audio"));
    stack.add_named(&bluetooth.root, Some("bluetooth"));
    stack.add_named(&capture.root, Some("capture"));
    stack.add_named(&system, Some("system"));
    panel.append(&stack);

    // Rodapé
    let footer = gtk::Box::new(gtk::Orientation::Horizontal, 22);
    footer.add_css_class("footer");
    footer.set_halign(gtk::Align::End);
    footer.append(&footer_item("A", "Selecionar"));
    let close_hint = footer_item("X", "Fechar programa");
    let close_hint_text = close_hint.last_child().and_downcast::<gtk::Label>().unwrap();
    close_hint.set_visible(false);
    footer.append(&close_hint);
    footer.append(&footer_item("B", "Fechar"));
    footer.append(&footer_item("LB / RB", "Trocar aba"));
    footer.append(&footer_item("Guide", "Abrir / fechar"));
    panel.append(&footer);

    window.set_child(Some(&root));

    let ui = Rc::new(Ui {
        ctl,
        layer,
        stack,
        tabs,
        clock,
        battery,
        badge,
        games,
        audio,
        capture,
        windows,
        bluetooth,
        close_hint,
        close_hint_text,
        repeat: RefCell::default(),
        last_guide: Cell::new(None),
        last_focus: RefCell::default(),
        refocus_pending: Cell::new(false),
    });
    ui.switch_tab("games");

    // Qualquer caminho que esconda a janela (ex.: abrir um jogo) para a busca Bluetooth.
    let w = Rc::downgrade(&ui);
    window.connect_visible_notify(move |win| {
        if !win.is_visible() {
            if let Some(ui) = w.upgrade() {
                ui.bluetooth.set_active(false);
            }
        }
    });

    let w = Rc::downgrade(&ui);
    window.connect_focus_widget_notify(move |_| {
        if let Some(ui) = w.upgrade() {
            ui.on_focus_changed();
        }
    });

    for (name, b) in &ui.tabs {
        let (w, name) = (Rc::downgrade(&ui), *name);
        b.connect_clicked(move |_| {
            if let Some(ui) = w.upgrade() {
                ui.switch_tab(name);
            }
        });
    }

    // Esc fecha
    let keys = gtk::EventControllerKey::new();
    let w = Rc::downgrade(&ui);
    keys.connect_key_pressed(move |_, key, _, _| {
        if key == gdk::Key::Escape {
            if let Some(ui) = w.upgrade() {
                ui.hide();
            }
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    window.add_controller(keys);

    // Clique fora do painel fecha
    let click = gtk::GestureClick::new();
    let (w, root_c) = (Rc::downgrade(&ui), root.clone());
    click.connect_pressed(move |_, _, x, y| {
        let Some(ui) = w.upgrade() else { return };
        let inside = root_c
            .pick(x, y, gtk::PickFlags::DEFAULT)
            .map(|t| t == ui.ctl.panel.clone().upcast::<gtk::Widget>() || t.is_ancestor(&ui.ctl.panel))
            .unwrap_or(false);
        if !inside {
            ui.hide();
        }
    });
    root.add_controller(click);

    // Relógio: só trabalha com o overlay aberto.
    let w = Rc::downgrade(&ui);
    glib::timeout_add_seconds_local(15, move || match w.upgrade() {
        Some(ui) => {
            if ui.visible() {
                ui.update_header();
            }
            glib::ControlFlow::Continue
        }
        None => glib::ControlFlow::Break,
    });

    // Controle
    let (tx, rx) = async_channel::unbounded();
    gamepad::start(tx, ui.ctl.grab.clone());
    let w = Rc::downgrade(&ui);
    glib::spawn_future_local(async move {
        while let Ok(ev) = rx.recv().await {
            match w.upgrade() {
                Some(ui) => ui.on_pad(ev),
                None => break,
            }
        }
    });

    ui
}

/// `orbit --render-docs DIR`: renderiza o painel de cada aba em DIR/<aba>.png (para a documentação).
fn render_docs_dir() -> Option<std::path::PathBuf> {
    let args: Vec<String> = std::env::args().collect();
    let i = args.iter().position(|a| a == "--render-docs")?;
    args.get(i + 1).map(std::path::PathBuf::from)
}

fn render_png(widget: &gtk::Widget, path: &std::path::Path) -> Result<(), String> {
    let (w, h) = (widget.width() as f32, widget.height() as f32);
    let paintable = gtk::WidgetPaintable::new(Some(widget));
    let snap = gtk::Snapshot::new();
    paintable.snapshot(&snap, w as f64, h as f64);
    let node = snap.to_node().ok_or("nada para renderizar")?;
    let renderer = widget.native().and_then(|n| n.renderer()).ok_or("sem renderer")?;
    let rect = gtk::graphene::Rect::new(0.0, 0.0, w, h);
    renderer
        .render_texture(&node, Some(&rect))
        .save_to_png(path)
        .map_err(|e| e.to_string())
}

fn render_docs(app: &gtk::Application, ui: Rc<Ui>, dir: std::path::PathBuf) {
    let _ = std::fs::create_dir_all(&dir);
    ui.stack.set_transition_type(gtk::StackTransitionType::None);
    ui.show();
    let app = app.clone();
    glib::spawn_future_local(async move {
        // Espera áudio, janelas e Bluetooth carregarem.
        glib::timeout_future(Duration::from_millis(2500)).await;
        for (name, _) in TABS {
            ui.switch_tab(name);
            ui.ctl.window.set_focus_visible(true);
            glib::timeout_future(Duration::from_millis(500)).await;
            let path = dir.join(format!("{name}.png"));
            match render_png(ui.ctl.panel.upcast_ref(), &path) {
                Ok(()) => println!("{}", path.display()),
                Err(e) => eprintln!("{name}: {e}"),
            }
        }
        ui.hide();
        app.quit();
    });
}

fn load_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(CSS);
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

fn main() -> glib::ExitCode {
    if std::env::args().any(|a| a == "--help" || a == "-h") {
        println!(
            "Orbit — overlay estilo Game Bar\n\n\
             uso: orbit [--daemon | --quit]\n\n  \
             (sem argumentos)  abre/fecha o overlay (inicia se preciso)\n  \
             --daemon          inicia em segundo plano, escondido\n  \
             --quit            encerra a instância em execução\n  \
             --list-windows    mostra as janelas detectadas (diagnóstico)\n\n\
             O botão Guide do controle abre/fecha o overlay."
        );
        return glib::ExitCode::SUCCESS;
    }

    if std::env::args().any(|a| a == "--list-audio") {
        audio::print_list();
        return glib::ExitCode::SUCCESS;
    }
    if std::env::args().any(|a| a == "--list-windows") {
        return if windows::print_list() { glib::ExitCode::SUCCESS } else { glib::ExitCode::FAILURE };
    }

    let docs = render_docs_dir();
    let mut flags = gio::ApplicationFlags::HANDLES_COMMAND_LINE;
    if docs.is_some() {
        // Não conversa com a instância residente.
        flags |= gio::ApplicationFlags::NON_UNIQUE;
    }
    let app = gtk::Application::builder()
        .application_id(APP_ID)
        .flags(flags)
        .build();

    app.connect_startup(|app| {
        load_css();
        let ui = build_ui(app);
        if let Some(dir) = render_docs_dir() {
            render_docs(app, ui.clone(), dir);
        }
        UI.with(|u| *u.borrow_mut() = Some(ui));
        // Fica residente mesmo sem janela visível.
        std::mem::forget(app.hold());
    });

    app.connect_command_line(|app, cl| {
        let args: Vec<String> = cl
            .arguments()
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        if args.iter().any(|a| a == "--quit") {
            app.quit();
        } else if !args.iter().any(|a| a == "--daemon" || a == "--render-docs") {
            UI.with(|u| {
                if let Some(ui) = u.borrow().as_ref() {
                    ui.toggle();
                }
            });
        }
        glib::ExitCode::SUCCESS
    });

    app.run()
}
