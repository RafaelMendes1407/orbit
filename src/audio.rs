//! Saídas e entradas de áudio, volume e mudo via pactl (PipeWire/PulseAudio).
//!
//! Lista **todas** as saídas/entradas: além dos dispositivos ativos (sinks/sources),
//! inclui as portas das placas que só existem com outro perfil ativo — por exemplo o
//! segundo monitor HDMI numa placa de vídeo, que o PipeWire só expõe trocando o perfil.
//! Escolher uma delas troca o perfil da placa, a porta e o padrão de uma vez.

use crate::util;
use gtk::prelude::*;
use gtk::{gio, glib, pango};
use serde_json::Value;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dir {
    Out,
    In,
}

impl Dir {
    fn list_cmd(self) -> &'static str {
        match self {
            Dir::Out => "sinks",
            Dir::In => "sources",
        }
    }
    fn word(self) -> &'static str {
        match self {
            Dir::Out => "sink",
            Dir::In => "source",
        }
    }
}

#[derive(Clone, Debug)]
enum Pick {
    /// Dispositivo que já existe: só vira o padrão.
    Node(String),
    /// Porta de uma placa: troca o perfil (se preciso), a porta e vira o padrão.
    Port { card: String, profile: Option<String>, port: String },
}

#[derive(Clone, Debug)]
pub struct Endpoint {
    key: String,
    title: String,
    sub: String,
    icon: &'static str,
    available: bool,
    current: bool,
    pick: Pick,
}

pub struct AudioState {
    outputs: Vec<Endpoint>,
    inputs: Vec<Endpoint>,
    volume: u32,
    muted: bool,
    mic_volume: Option<u32>,
    mic_muted: Option<bool>,
}

// ---------------------------------------------------------------- Consulta

fn pactl_json(args: &[&str]) -> Vec<Value> {
    let mut a = vec!["-f", "json"];
    a.extend_from_slice(args);
    util::output("pactl", &a)
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or_default()
}

fn prop<'a>(v: &'a Value, k: &str) -> &'a str {
    v["properties"][k].as_str().unwrap_or_default()
}

fn is_monitor(node: &Value) -> bool {
    s(node, "name").ends_with(".monitor") || prop(node, "device.class") == "monitor"
}

fn percent(node: &Value) -> u32 {
    node["volume"]
        .as_object()
        .and_then(|o| o.values().next())
        .and_then(|c| c["value_percent"].as_str())
        .and_then(|p| p.trim().trim_end_matches('%').parse().ok())
        .unwrap_or(0)
}

fn port_dir(name: &str) -> Option<Dir> {
    if name.contains("output") {
        Some(Dir::Out)
    } else if name.contains("input") {
        Some(Dir::In)
    } else {
        None
    }
}

fn icon_for(dir: Dir, kind: &str, text: &str) -> &'static str {
    let k = format!("{kind} {text}").to_lowercase();
    match dir {
        Dir::Out => {
            if k.contains("hdmi") || k.contains("displayport") {
                "video-display-symbolic"
            } else if k.contains("headphone") || k.contains("headset") || k.contains("fone") || k.contains("bluez") {
                "audio-headphones-symbolic"
            } else {
                "audio-speakers-symbolic"
            }
        }
        Dir::In => {
            if k.contains("camera") || k.contains("webcam") {
                "camera-web-symbolic"
            } else if k.contains("headset") || k.contains("bluez") {
                "audio-headset-symbolic"
            } else {
                "audio-input-microphone-symbolic"
            }
        }
    }
}

/// Lado `input:`/`output:` de um perfil ("output:hdmi-stereo+input:mono" → ("hdmi-stereo","mono")).
fn profile_part<'a>(profile: &'a str, dir: Dir) -> Option<&'a str> {
    let tag = match dir {
        Dir::Out => "output:",
        Dir::In => "input:",
    };
    profile.split('+').find_map(|p| p.strip_prefix(tag))
}

fn endpoints(dir: Dir, cards: &[Value], nodes: &[Value], default: &str) -> Vec<Endpoint> {
    let other = match dir {
        Dir::Out => Dir::In,
        Dir::In => Dir::Out,
    };
    let card_by_name = |n: &str| cards.iter().find(|c| s(c, "name") == n);
    let mut out: Vec<Endpoint> = Vec::new();

    // 1) Dispositivos que existem agora.
    for n in nodes.iter().filter(|n| !is_monitor(n)) {
        let name = s(n, "name").to_string();
        let card_name = prop(n, "device.name");
        let card = card_by_name(card_name);
        let active_port = s(n, "active_port");
        let port = card.and_then(|c| c["ports"].get(active_port));
        let product = port.map(|p| prop(p, "device.product.name")).unwrap_or_default();
        let port_desc = port.map(|p| s(p, "description")).unwrap_or_default();
        let card_desc = card.map(|c| prop(c, "device.description")).unwrap_or_default();
        let desc = s(n, "description");

        let title = [product, desc].into_iter().find(|t| !t.is_empty()).unwrap_or(&name).to_string();
        let sub = if !product.is_empty() {
            [port_desc, card_desc].iter().filter(|t| !t.is_empty()).cloned().collect::<Vec<_>>().join(" · ")
        } else {
            String::new()
        };
        let key = if card.is_some() && !active_port.is_empty() {
            format!("{card_name}:{active_port}")
        } else {
            name.clone()
        };
        let kind = port.map(|p| s(p, "type")).unwrap_or_default();
        out.push(Endpoint {
            key,
            icon: icon_for(dir, kind, &format!("{name} {title} {port_desc}")),
            title,
            sub,
            available: true,
            current: name == default,
            pick: Pick::Node(name),
        });
    }

    // 2) Portas das placas que não estão ativas (outra porta ou outro perfil).
    for c in cards {
        let card_name = s(c, "name");
        let active_profile = s(c, "active_profile");
        // No "pro-audio" os nós já cobrem tudo; listar perfis duplicaria.
        if active_profile == "pro-audio" {
            continue;
        }
        let card_desc = prop(c, "device.description");
        let Some(ports) = c["ports"].as_object() else { continue };
        for (pname, p) in ports {
            if port_dir(pname) != Some(dir) {
                continue;
            }
            let key = format!("{card_name}:{pname}");
            if out.iter().any(|e| e.key == key) {
                continue;
            }
            let profiles: Vec<&str> = p["profiles"]
                .as_array()
                .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
                .unwrap_or_default();
            if profiles.is_empty() {
                continue;
            }
            // Um nó desta placa já oferece a porta? Então basta trocar a porta.
            let node_has = nodes.iter().any(|n| {
                prop(n, "device.name") == card_name
                    && n["ports"].as_array().is_some_and(|a| a.iter().any(|x| s(x, "name") == pname))
            });
            let profile = if node_has || profiles.contains(&active_profile) {
                None
            } else {
                // Prefere perfis que mantêm o outro lado (ex.: não derrubar o microfone).
                let keep = profile_part(active_profile, other);
                let best = profiles
                    .iter()
                    .filter_map(|pr| c["profiles"].get(*pr).map(|info| (*pr, info)))
                    .max_by_key(|(pr, info)| {
                        (
                            info["available"].as_bool().unwrap_or(true),
                            keep.is_some() && profile_part(pr, other) == keep,
                            info["priority"].as_u64().unwrap_or(0),
                        )
                    });
                match best {
                    Some((pr, _)) => Some(pr.to_string()),
                    None => continue,
                }
            };
            let product = prop(p, "device.product.name");
            let port_desc = s(p, "description");
            let (title, sub) = if product.is_empty() {
                (port_desc.to_string(), card_desc.to_string())
            } else {
                (product.to_string(), format!("{port_desc} · {card_desc}"))
            };
            out.push(Endpoint {
                key,
                icon: icon_for(dir, s(p, "type"), &format!("{pname} {title} {port_desc}")),
                title,
                sub,
                available: s(p, "availability") != "not available",
                current: false,
                pick: Pick::Port { card: card_name.to_string(), profile, port: pname.to_string() },
            });
        }
    }
    out
}

pub fn query() -> Option<AudioState> {
    let cards = pactl_json(&["list", "cards"]);
    let sinks = pactl_json(&["list", "sinks"]);
    let sources = pactl_json(&["list", "sources"]);
    if sinks.is_empty() && sources.is_empty() && cards.is_empty() {
        return None;
    }
    let def = |w: &str| util::output("pactl", &[w]).unwrap_or_default().trim().to_string();
    let default_sink = def("get-default-sink");
    let default_source = def("get-default-source");

    let sink = sinks.iter().find(|n| s(n, "name") == default_sink);
    let source = sources.iter().find(|n| s(n, "name") == default_source && !is_monitor(n));
    Some(AudioState {
        outputs: endpoints(Dir::Out, &cards, &sinks, &default_sink),
        inputs: endpoints(Dir::In, &cards, &sources, &default_source),
        volume: sink.map(percent).unwrap_or(0),
        muted: sink.and_then(|n| n["mute"].as_bool()).unwrap_or(false),
        mic_volume: source.map(percent),
        mic_muted: source.and_then(|n| n["mute"].as_bool()),
    })
}

/// Aplica a escolha (roda fora da thread da interface).
fn select(dir: Dir, pick: &Pick) {
    let set_default = format!("set-default-{}", dir.word());
    match pick {
        Pick::Node(name) => {
            util::run("pactl", &[&set_default, name]);
        }
        Pick::Port { card, profile, port } => {
            if let Some(p) = profile {
                util::run("pactl", &["set-card-profile", card, p]);
            }
            // O nó novo aparece alguns ms depois da troca de perfil.
            for _ in 0..30 {
                let node = pactl_json(&["list", dir.list_cmd()]).into_iter().find(|n| {
                    prop(n, "device.name") == card
                        && n["ports"].as_array().is_some_and(|a| a.iter().any(|x| s(x, "name") == port))
                });
                if let Some(n) = node {
                    let name = s(&n, "name").to_string();
                    if s(&n, "active_port") != port {
                        util::run("pactl", &[&format!("set-{}-port", dir.word()), &name, port]);
                    }
                    util::run("pactl", &[&set_default, &name]);
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

// ---------------------------------------------------------------- UI: lista de saídas/entradas

/// Lista de saídas ou entradas, com as desconectadas escondidas atrás de um botão.
struct EndpointList {
    dir: Dir,
    root: gtk::Box,
    main: gtk::Box,
    more: gtk::Button,
    more_label: gtk::Label,
    extra: gtk::Box,
    show_extra: Cell<bool>,
    rows: RefCell<Vec<(String, gtk::Button)>>,
    sig: RefCell<String>,
}

impl EndpointList {
    fn new(dir: Dir) -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let main = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let extra = gtk::Box::new(gtk::Orientation::Vertical, 8);
        extra.set_visible(false);

        let more = gtk::Button::new();
        more.add_css_class("tile");
        more.add_css_class("subtle");
        let more_label = gtk::Label::new(None);
        more_label.set_xalign(0.0);
        more_label.add_css_class("tile-sub");
        more.set_child(Some(&more_label));
        more.set_visible(false);

        root.append(&main);
        root.append(&more);
        root.append(&extra);
        let this = Rc::new(Self {
            dir,
            root,
            main,
            more,
            more_label,
            extra,
            show_extra: Cell::new(false),
            rows: RefCell::default(),
            sig: RefCell::default(),
        });
        let w = Rc::downgrade(&this);
        this.more.connect_clicked(move |_| {
            if let Some(t) = w.upgrade() {
                t.show_extra.set(!t.show_extra.get());
                t.extra.set_visible(t.show_extra.get());
                t.update_more_label(t.extra_count());
            }
        });
        this
    }

    fn extra_count(&self) -> usize {
        let mut n = 0;
        let mut c = self.extra.first_child();
        while let Some(w) = c {
            n += 1;
            c = w.next_sibling();
        }
        n
    }

    fn update_more_label(&self, n: usize) {
        let what = match self.dir {
            Dir::Out => "saídas",
            Dir::In => "entradas",
        };
        self.more_label.set_text(&if self.show_extra.get() {
            format!("▴  Esconder {what} desconectadas")
        } else {
            format!("▾  Mostrar {what} desconectadas ({n})")
        });
    }

    fn apply(&self, eps: &[Endpoint], on_pick: impl Fn(Dir, Pick) + Clone + 'static) {
        let sig = eps
            .iter()
            .map(|e| format!("{}\u{1}{}\u{1}{}\u{1}{}\u{1}{}", e.key, e.title, e.sub, e.available, e.current))
            .collect::<Vec<_>>()
            .join("\u{2}");
        if *self.sig.borrow() == sig {
            return;
        }
        *self.sig.borrow_mut() = sig;

        // Lembra em qual item estava o cursor.
        let focus = self.root.root().and_then(|r| r.focus());
        let focused_key = focus.as_ref().and_then(|f| {
            self.rows
                .borrow()
                .iter()
                .find(|(_, b)| f == b.upcast_ref::<gtk::Widget>() || f.is_ancestor(b))
                .map(|(k, _)| k.clone())
        });
        let focus_was_here = focus.as_ref().is_some_and(|f| f.is_ancestor(&self.root));

        util::clear_box(&self.main);
        util::clear_box(&self.extra);
        let mut rows = Vec::new();
        for e in eps {
            let btn = endpoint_row(e);
            let (dir, pick, cb) = (self.dir, e.pick.clone(), on_pick.clone());
            btn.connect_clicked(move |_| cb(dir, pick.clone()));
            if e.available || e.current {
                self.main.append(&btn);
            } else {
                self.extra.append(&btn);
            }
            rows.push((e.key.clone(), btn));
        }
        let n_extra = self.extra_count();
        self.more.set_visible(n_extra > 0);
        if n_extra == 0 {
            self.show_extra.set(false);
        }
        self.extra.set_visible(self.show_extra.get());
        self.update_more_label(n_extra);

        if focus_was_here {
            let target = focused_key
                .and_then(|k| rows.iter().find(|(rk, _)| *rk == k).map(|(_, b)| b.clone().upcast::<gtk::Widget>()))
                .filter(|w| w.is_visible() && w.parent().is_some_and(|p| p.is_visible()))
                .or_else(|| self.more.is_visible().then(|| self.more.clone().upcast()))
                .or_else(|| rows.first().map(|(_, b)| b.clone().upcast()));
            if let Some(t) = target {
                t.grab_focus();
            }
        }
        *self.rows.borrow_mut() = rows;
    }
}

fn endpoint_row(e: &Endpoint) -> gtk::Button {
    let btn = gtk::Button::new();
    btn.add_css_class("tile");
    if e.current {
        btn.add_css_class("selected");
    }
    if !e.available {
        btn.add_css_class("unplugged");
    }
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 14);
    let icon = gtk::Image::from_icon_name(e.icon);
    icon.set_pixel_size(22);
    icon.add_css_class("tile-icon");

    let col = gtk::Box::new(gtk::Orientation::Vertical, 2);
    col.set_valign(gtk::Align::Center);
    col.set_hexpand(true);
    let t = gtk::Label::new(Some(&e.title));
    t.set_xalign(0.0);
    t.set_ellipsize(pango::EllipsizeMode::End);
    t.set_max_width_chars(1);
    t.add_css_class("tile-title");
    col.append(&t);
    let mut sub = e.sub.clone();
    if !e.available {
        sub = if sub.is_empty() { "Desconectado".into() } else { format!("Desconectado · {sub}") };
    }
    if !sub.is_empty() {
        let s = gtk::Label::new(Some(&sub));
        s.set_xalign(0.0);
        s.set_ellipsize(pango::EllipsizeMode::End);
        s.set_max_width_chars(1);
        s.add_css_class("tile-sub");
        col.append(&s);
    }

    row.append(&icon);
    row.append(&col);
    if e.current {
        let check = gtk::Image::from_icon_name("object-select-symbolic");
        check.add_css_class("check");
        row.append(&check);
    }
    btn.set_child(Some(&row));
    btn
}

// ---------------------------------------------------------------- UI: página

fn volume_row(css: &str) -> (gtk::Box, gtk::Scale, gtk::Label) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 14);
    let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 1.0);
    scale.set_hexpand(true);
    scale.set_valign(gtk::Align::Center);
    scale.add_css_class("volume");
    scale.add_css_class(css);
    let label = gtk::Label::new(Some("0%"));
    label.set_width_chars(4);
    label.add_css_class("vol-label");
    row.append(&scale);
    row.append(&label);
    (row, scale, label)
}

pub struct AudioPage {
    pub root: gtk::ScrolledWindow,
    outputs: Rc<EndpointList>,
    inputs: Rc<EndpointList>,
    volume: gtk::Scale,
    vol_label: gtk::Label,
    mic_row: gtk::Box,
    mic_volume: gtk::Scale,
    mic_label: gtk::Label,
    mute: gtk::Button,
    mute_sub: gtk::Label,
    mic: gtk::Button,
    mic_sub: gtk::Label,
    updating: Rc<Cell<bool>>,
}

impl AudioPage {
    pub fn new() -> Rc<Self> {
        let page = gtk::Box::new(gtk::Orientation::Vertical, 10);
        page.add_css_class("page");

        let outputs = EndpointList::new(Dir::Out);
        let inputs = EndpointList::new(Dir::In);
        let (vol_row, volume, vol_label) = volume_row("out");
        let (mic_row, mic_volume, mic_label) = volume_row("in");
        let (mute, mute_sub) = util::tile("audio-volume-high-symbolic", "Som", "Ativo");
        let (mic, mic_sub) = util::tile("audio-input-microphone-symbolic", "Microfone", "Ativo");

        page.append(&util::section("SAÍDA DE ÁUDIO"));
        page.append(&outputs.root);
        page.append(&util::section("VOLUME"));
        page.append(&vol_row);
        page.append(&util::section("ENTRADA DE ÁUDIO"));
        page.append(&inputs.root);
        page.append(&util::section("VOLUME DO MICROFONE"));
        page.append(&mic_row);
        page.append(&util::tile_grid(2, &[mute.clone(), mic.clone()]));

        let root = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&page)
            .build();

        let this = Rc::new(Self {
            root,
            outputs,
            inputs,
            volume,
            vol_label,
            mic_row,
            mic_volume,
            mic_label,
            mute,
            mute_sub,
            mic,
            mic_sub,
            updating: Rc::new(Cell::new(false)),
        });

        for (scale, label, target) in [
            (&this.volume, &this.vol_label, "set-sink-volume @DEFAULT_SINK@"),
            (&this.mic_volume, &this.mic_label, "set-source-volume @DEFAULT_SOURCE@"),
        ] {
            let (updating, label) = (this.updating.clone(), label.clone());
            scale.connect_value_changed(move |s| {
                let v = s.value().round() as u32;
                label.set_text(&format!("{v}%"));
                if !updating.get() {
                    let mut a: Vec<String> = target.split(' ').map(String::from).collect();
                    a.push(format!("{v}%"));
                    let a: Vec<&str> = a.iter().map(|s| s.as_str()).collect();
                    util::spawn("pactl", &a);
                }
            });
        }
        let w = Rc::downgrade(&this);
        this.mute.connect_clicked(move |_| {
            if let Some(t) = w.upgrade() {
                t.run_then_refresh(&["set-sink-mute", "@DEFAULT_SINK@", "toggle"]);
            }
        });
        let w = Rc::downgrade(&this);
        this.mic.connect_clicked(move |_| {
            if let Some(t) = w.upgrade() {
                t.run_then_refresh(&["set-source-mute", "@DEFAULT_SOURCE@", "toggle"]);
            }
        });
        this
    }

    fn run_then_refresh(self: &Rc<Self>, args: &[&str]) {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let this = self.clone();
        glib::spawn_future_local(async move {
            let _ = gio::spawn_blocking(move || {
                let a: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                util::run("pactl", &a)
            })
            .await;
            this.refresh();
        });
    }

    fn pick(self: &Rc<Self>, dir: Dir, pick: Pick) {
        let this = self.clone();
        glib::spawn_future_local(async move {
            let _ = gio::spawn_blocking(move || select(dir, &pick)).await;
            this.refresh();
        });
    }

    /// Consulta o pactl fora da thread da UI; a janela abre sem esperar.
    pub fn refresh(self: &Rc<Self>) {
        let this = self.clone();
        glib::spawn_future_local(async move {
            if let Ok(Some(st)) = gio::spawn_blocking(query).await {
                this.apply(st);
            }
        });
    }

    fn apply(self: &Rc<Self>, st: AudioState) {
        let w = Rc::downgrade(self);
        let on_pick = move |dir, pick| {
            if let Some(t) = w.upgrade() {
                t.pick(dir, pick);
            }
        };
        self.outputs.apply(&st.outputs, on_pick.clone());
        self.inputs.apply(&st.inputs, on_pick);

        self.updating.set(true);
        self.volume.set_value(st.volume.min(100) as f64);
        self.vol_label.set_text(&format!("{}%", st.volume));
        if let Some(v) = st.mic_volume {
            self.mic_volume.set_value(v.min(100) as f64);
            self.mic_label.set_text(&format!("{v}%"));
        }
        self.updating.set(false);
        self.mic_row.set_sensitive(st.mic_volume.is_some());

        self.mute_sub.set_text(if st.muted { "Mudo — pressione para ativar" } else { "Ativo — pressione para silenciar" });
        set_tile_icon(&self.mute, if st.muted { "audio-volume-muted-symbolic" } else { "audio-volume-high-symbolic" });
        if st.muted {
            self.mute.add_css_class("off");
        } else {
            self.mute.remove_css_class("off");
        }

        match st.mic_muted {
            Some(m) => {
                self.mic.set_sensitive(true);
                self.mic_sub.set_text(if m { "Mudo — pressione para ativar" } else { "Ativo — pressione para silenciar" });
                set_tile_icon(&self.mic, if m { "microphone-sensitivity-muted-symbolic" } else { "audio-input-microphone-symbolic" });
                if m {
                    self.mic.add_css_class("off");
                } else {
                    self.mic.remove_css_class("off");
                }
            }
            None => {
                self.mic.set_sensitive(false);
                self.mic_sub.set_text("Nenhum microfone");
            }
        }
    }
}

fn set_tile_icon(btn: &gtk::Button, icon: &str) {
    if let Some(img) = btn
        .child()
        .and_then(|row| row.first_child())
        .and_then(|w| w.downcast::<gtk::Image>().ok())
    {
        img.set_icon_name(Some(icon));
    }
}

/// `orbit --list-audio`: imprime o que a aba Áudio mostraria (diagnóstico).
pub fn print_list() {
    let Some(st) = query() else {
        eprintln!("pactl indisponível");
        return;
    };
    for (name, list) in [("SAÍDAS", &st.outputs), ("ENTRADAS", &st.inputs)] {
        println!("{name}");
        for e in list {
            let mark = if e.current { "*" } else if e.available { " " } else { "-" };
            println!("{mark} {:<28} {:<50} {:?}", e.title, e.sub, e.pick);
        }
    }
}
