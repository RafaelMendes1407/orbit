//! Capturas de tela, gravação e Instant Replay (gpu-screen-recorder).

use crate::{util, Ctl};
use gtk::glib;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::process::Child;
use std::rc::Rc;
use std::time::{Duration, Instant};

const GSR: &str = "gpu-screen-recorder";
const REPLAY_SECS: &str = "30";

fn shots_dir() -> PathBuf {
    util::user_dir(glib::UserDirectory::Pictures, "Pictures", "Screenshots")
}
fn videos_dir() -> PathBuf {
    util::user_dir(glib::UserDirectory::Videos, "Videos", "Orbit")
}
fn replay_dir() -> PathBuf {
    util::user_dir(glib::UserDirectory::Videos, "Videos", "Orbit/Replays")
}

pub fn screenshot(region: bool) {
    let dir = shots_dir();
    let dir_s = dir.to_string_lossy().into_owned();
    let file = dir.join(format!("Orbit_{}.png", util::timestamp()));
    let file_s = file.to_string_lossy().into_owned();

    let notify = r#"notify-send -a Orbit -i "$1" "Captura salva" "$1""#;
    let sh = |script: &str| util::spawn("sh", &["-c", script, "sh", &file_s]);

    if util::is_desktop("cosmic") && util::which("cosmic-screenshot") {
        if region {
            util::spawn("cosmic-screenshot", &["--interactive=true", "-s", &dir_s]);
        } else {
            util::spawn(
                "cosmic-screenshot",
                &["--interactive=false", "--modal=false", "--notify=true", "-s", &dir_s],
            );
        }
    } else if util::is_desktop("kde") && util::which("spectacle") {
        let mode = if region { "-r" } else { "-f" };
        util::spawn("spectacle", &["-b", "-n", mode, "-o", &file_s]);
    } else if util::is_desktop("gnome") && util::which("gnome-screenshot") {
        let area = if region { "-a " } else { "" };
        sh(&format!(r#"gnome-screenshot {area}-f "$1" && {notify}"#));
    } else if util::is_wayland() && util::which("grim") && !util::is_desktop("gnome") && !util::is_desktop("kde") {
        // Compositores wlroots e afins (Hyprland, Sway, niri, labwc, Wayfire…).
        if region {
            sh(&format!(r#"g=$(slurp) && grim -g "$g" "$1" && {notify}"#));
        } else {
            sh(&format!(r#"grim "$1" && {notify}"#));
        }
    } else if !util::is_wayland() && util::which("maim") {
        let sel = if region { "-s " } else { "" };
        sh(&format!(r#"maim {sel}"$1" && {notify}"#));
    } else if !util::is_wayland() && util::which("scrot") {
        let sel = if region { "-s " } else { "" };
        sh(&format!(r#"scrot -o {sel}"$1" && {notify}"#));
    } else {
        // Universal: portal XDG (o próprio ambiente salva e avisa).
        portal_screenshot(region);
    }
}

/// Pede a captura ao xdg-desktop-portal (funciona em GNOME, KDE, COSMIC, Hyprland…).
fn portal_screenshot(interactive: bool) {
    use gtk::gio;
    glib::spawn_future_local(async move {
        let Ok(conn) = gio::bus_get_future(gio::BusType::Session).await else {
            util::notify("Captura indisponível", "Instale grim, spectacle, gnome-screenshot ou maim");
            return;
        };
        let opts = glib::VariantDict::new(None);
        opts.insert("interactive", interactive);
        opts.insert("modal", false);
        let r = conn
            .call_future(
                Some("org.freedesktop.portal.Desktop"),
                "/org/freedesktop/portal/desktop",
                "org.freedesktop.portal.Screenshot",
                "Screenshot",
                Some(&("", opts.end()).to_variant()),
                None,
                gio::DBusCallFlags::NONE,
                10_000,
            )
            .await;
        if r.is_err() {
            util::notify("Captura indisponível", "Instale grim, spectacle, gnome-screenshot ou maim");
        }
    });
}

fn signal(child: &Child, sig: libc::c_int) {
    // SAFETY: kill(2) num PID que ainda é nosso filho (não foi aguardado).
    unsafe {
        libc::kill(child.id() as libc::pid_t, sig);
    }
}

/// SIGINT faz o gpu-screen-recorder finalizar o arquivo; espera numa thread.
fn stop(mut child: Child, done_msg: Option<(String, String)>) {
    signal(&child, libc::SIGINT);
    std::thread::spawn(move || {
        let _ = child.wait();
        if let Some((s, b)) = done_msg {
            util::notify(&s, &b);
        }
    });
}

fn fmt_elapsed(d: Duration) -> String {
    let s = d.as_secs();
    format!("{:02}:{:02}", s / 60, s % 60)
}

pub struct CapturePage {
    pub root: gtk::Box,
    ctl: Ctl,
    rec_btn: gtk::Button,
    rec_sub: gtk::Label,
    replay_btn: gtk::Button,
    replay_sub: gtk::Label,
    save_btn: gtk::Button,
    save_sub: gtk::Label,
    rec: RefCell<Option<(Child, Instant, PathBuf)>>,
    replay: RefCell<Option<Child>>,
    has_gsr: Cell<bool>,
    tick: RefCell<Option<glib::SourceId>>,
}

impl CapturePage {
    pub fn new(ctl: Ctl) -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
        root.add_css_class("page");

        let (shot, _) = util::tile("camera-photo-symbolic", "Captura de tela", "Tela inteira");
        let (region, _) = util::tile("edit-select-all-symbolic", "Capturar região", "Selecionar área");
        let (open_shots, _) = util::tile("folder-pictures-symbolic", "Abrir capturas", "Pasta de imagens");

        let (rec_btn, rec_sub) = util::tile("media-record-symbolic", "Gravar", "Iniciar gravação");
        let (replay_btn, replay_sub) = util::tile("media-playlist-repeat-symbolic", "Instant Replay", "Desligado");
        let (save_btn, save_sub) = util::tile("document-save-symbolic", "Salvar replay", "Últimos 30 segundos");
        let (open_vids, _) = util::tile("folder-videos-symbolic", "Abrir gravações", "Pasta de vídeos");

        root.append(&util::section("CAPTURA DE TELA"));
        root.append(&util::tile_grid(3, &[shot.clone(), region.clone(), open_shots.clone()]));
        root.append(&util::section("GRAVAÇÃO"));
        root.append(&util::tile_grid(
            2,
            &[rec_btn.clone(), replay_btn.clone(), save_btn.clone(), open_vids.clone()],
        ));

        let this = Rc::new(Self {
            root,
            ctl,
            rec_btn,
            rec_sub,
            replay_btn,
            replay_sub,
            save_btn,
            save_sub,
            rec: RefCell::default(),
            replay: RefCell::default(),
            has_gsr: Cell::new(util::which(GSR)),
            tick: RefCell::default(),
        });

        for (btn, region) in [(shot, false), (region, true)] {
            let ctl = this.ctl.clone();
            btn.connect_clicked(move |_| {
                // Esconde o overlay antes, para ele não sair na foto.
                ctl.hide();
                glib::timeout_add_local_once(Duration::from_millis(350), move || screenshot(region));
            });
        }
        let ctl = this.ctl.clone();
        open_shots.connect_clicked(move |_| {
            util::spawn("xdg-open", &[&shots_dir().to_string_lossy()]);
            ctl.hide();
        });
        let ctl = this.ctl.clone();
        open_vids.connect_clicked(move |_| {
            util::spawn("xdg-open", &[&videos_dir().to_string_lossy()]);
            ctl.hide();
        });

        let w = Rc::downgrade(&this);
        this.rec_btn.connect_clicked(move |_| {
            if let Some(t) = w.upgrade() {
                t.toggle_record();
            }
        });
        let w = Rc::downgrade(&this);
        this.replay_btn.connect_clicked(move |_| {
            if let Some(t) = w.upgrade() {
                t.toggle_replay();
            }
        });
        let w = Rc::downgrade(&this);
        this.save_btn.connect_clicked(move |_| {
            if let Some(t) = w.upgrade() {
                t.save_replay();
            }
        });
        this
    }

    fn toggle_record(self: &Rc<Self>) {
        if let Some((child, _, path)) = self.rec.borrow_mut().take() {
            stop(child, Some(("Gravação salva".into(), path.to_string_lossy().into_owned())));
            self.refresh();
            return;
        }
        if !self.has_gsr.get() {
            util::notify("Gravação indisponível", "Instale o pacote gpu-screen-recorder");
            return;
        }
        self.ctl.hide();
        let this = self.clone();
        glib::timeout_add_local_once(Duration::from_millis(300), move || {
            let file = videos_dir().join(format!("Gravacao_{}.mp4", util::timestamp()));
            let f = file.to_string_lossy().into_owned();
            match util::spawn_child(GSR, &["-w", "screen", "-f", "60", "-a", "default_output", "-o", &f]) {
                Some(c) => {
                    *this.rec.borrow_mut() = Some((c, Instant::now(), file));
                    util::notify("Gravando", "Abra o Orbit → Captura para parar");
                }
                None => util::notify("Falha ao gravar", "Não foi possível iniciar o gpu-screen-recorder"),
            }
        });
    }

    fn toggle_replay(self: &Rc<Self>) {
        if let Some(child) = self.replay.borrow_mut().take() {
            stop(child, None);
            util::notify("Instant Replay desligado", "");
            self.refresh();
            return;
        }
        if !self.has_gsr.get() {
            util::notify("Instant Replay indisponível", "Instale o pacote gpu-screen-recorder");
            return;
        }
        let dir = replay_dir().to_string_lossy().into_owned();
        let args = ["-w", "screen", "-f", "60", "-a", "default_output", "-c", "mp4", "-r", REPLAY_SECS, "-o", &dir];
        match util::spawn_child(GSR, &args) {
            Some(c) => {
                *self.replay.borrow_mut() = Some(c);
                util::notify("Instant Replay ligado", "Os últimos 30 segundos ficam guardados");
            }
            None => util::notify("Falha no Instant Replay", "Não foi possível iniciar o gpu-screen-recorder"),
        }
        self.refresh();
    }

    fn save_replay(self: &Rc<Self>) {
        match self.replay.borrow().as_ref() {
            Some(c) => {
                signal(c, libc::SIGUSR1);
                util::notify("Replay salvo", &replay_dir().to_string_lossy());
                self.ctl.hide();
            }
            None => util::notify("Instant Replay desligado", "Ligue o Instant Replay primeiro"),
        }
    }

    /// Detecta gravadores que morreram sozinhos e atualiza os textos.
    pub fn refresh(self: &Rc<Self>) {
        self.has_gsr.set(util::which(GSR));
        let rec_dead = matches!(self.rec.borrow_mut().as_mut().map(|r| r.0.try_wait()), Some(Ok(Some(_))));
        if rec_dead {
            self.rec.borrow_mut().take();
            util::notify("A gravação parou", "O gpu-screen-recorder encerrou inesperadamente");
        }
        let replay_dead = matches!(self.replay.borrow_mut().as_mut().map(|c| c.try_wait()), Some(Ok(Some(_))));
        if replay_dead {
            self.replay.borrow_mut().take();
            util::notify("O Instant Replay parou", "O gpu-screen-recorder encerrou inesperadamente");
        }

        let gsr = self.has_gsr.get();
        match self.rec.borrow().as_ref() {
            Some((_, t, _)) => {
                self.rec_sub.set_text(&format!("● {} — pressione para parar", fmt_elapsed(t.elapsed())));
                self.rec_btn.add_css_class("recording");
            }
            None => {
                self.rec_sub.set_text(if gsr { "Iniciar gravação" } else { "Requer gpu-screen-recorder" });
                self.rec_btn.remove_css_class("recording");
            }
        }
        if self.replay.borrow().is_some() {
            self.replay_sub.set_text("Ligado — pressione para desligar");
            self.replay_btn.add_css_class("recording");
            self.save_btn.set_sensitive(true);
            self.save_sub.set_text("Últimos 30 segundos");
        } else {
            self.replay_sub.set_text(if gsr { "Desligado" } else { "Requer gpu-screen-recorder" });
            self.replay_btn.remove_css_class("recording");
            self.save_btn.set_sensitive(false);
            self.save_sub.set_text("Ligue o Instant Replay");
        }

        // Cronômetro só roda enquanto grava E o overlay está visível.
        if self.rec.borrow().is_some() && self.tick.borrow().is_none() {
            let w = Rc::downgrade(self);
            let id = glib::timeout_add_local(Duration::from_secs(1), move || {
                let Some(t) = w.upgrade() else { return glib::ControlFlow::Break };
                if t.rec.borrow().is_none() || !t.ctl.window.is_visible() {
                    t.tick.borrow_mut().take();
                    return glib::ControlFlow::Break;
                }
                if let Some((_, start, _)) = t.rec.borrow().as_ref() {
                    t.rec_sub.set_text(&format!("● {} — pressione para parar", fmt_elapsed(start.elapsed())));
                }
                glib::ControlFlow::Continue
            });
            *self.tick.borrow_mut() = Some(id);
        }
    }

    /// Indicador para o cabeçalho.
    pub fn badge(&self) -> Option<&'static str> {
        match (self.rec.borrow().is_some(), self.replay.borrow().is_some()) {
            (true, _) => Some("● REC"),
            (false, true) => Some("● REPLAY"),
            _ => None,
        }
    }
}
