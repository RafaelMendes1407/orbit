//! Atalhos do sistema: configurações, arquivos, bloquear, suspender, reiniciar, desligar.

use crate::{util, Ctl};
use gtk::prelude::*;
use gtk::glib;
use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

enum Action {
    Spawn(&'static [&'static str]),
    /// Esconde o overlay antes (ex.: bloquear/suspender).
    HideThen(&'static [&'static str]),
    /// Exige apertar duas vezes.
    Confirm(&'static [&'static str]),
}

/// Primeiro comando disponível, preferindo o da área de trabalho atual.
fn pick(candidates: &[(&str, &'static [&'static str])], fallback: &'static [&'static str]) -> &'static [&'static str] {
    let native = candidates
        .iter()
        .find(|(de, cmd)| util::is_desktop(de) && util::which(cmd[0]));
    let any = || candidates.iter().find(|(_, cmd)| util::which(cmd[0]));
    native.or_else(any).map(|(_, c)| *c).unwrap_or(fallback)
}

fn settings_cmd() -> &'static [&'static str] {
    pick(
        &[
            ("cosmic", &["cosmic-settings"]),
            ("gnome", &["gnome-control-center"]),
            ("kde", &["systemsettings"]),
            ("xfce", &["xfce4-settings-manager"]),
            ("x-cinnamon", &["cinnamon-settings"]),
            ("mate", &["mate-control-center"]),
            ("lxqt", &["lxqt-config"]),
            ("budgie", &["budgie-control-center"]),
        ],
        &["xdg-open", "settings://"],
    )
}

fn files_cmd() -> &'static [&'static str] {
    pick(
        &[
            ("cosmic", &["cosmic-files"]),
            ("gnome", &["nautilus"]),
            ("kde", &["dolphin"]),
            ("xfce", &["thunar"]),
            ("x-cinnamon", &["nemo"]),
            ("mate", &["caja"]),
            ("lxqt", &["pcmanfm-qt"]),
        ],
        // Gerenciador de arquivos padrão do usuário.
        &["sh", "-c", "xdg-open \"$HOME\""],
    )
}

fn exec(cmd: &[&str]) {
    if let Some((prog, args)) = cmd.split_first() {
        util::spawn(prog, args);
    }
}

pub fn page(ctl: Ctl, app: &gtk::Application) -> gtk::Box {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
    root.add_css_class("page");

    let items: Vec<(&str, &str, &str, Action)> = vec![
        ("preferences-system-symbolic", "Configurações", "Ajustes do sistema", Action::Spawn(settings_cmd())),
        ("system-file-manager-symbolic", "Arquivos", "Gerenciador de arquivos", Action::Spawn(files_cmd())),
        ("system-lock-screen-symbolic", "Bloquear", "Bloquear a tela", Action::HideThen(&["loginctl", "lock-session"])),
        ("weather-clear-night-symbolic", "Suspender", "Dormir agora", Action::HideThen(&["systemctl", "suspend"])),
        ("system-reboot-symbolic", "Reiniciar", "Reiniciar o computador", Action::Confirm(&["systemctl", "reboot"])),
        ("system-shutdown-symbolic", "Desligar", "Desligar o computador", Action::Confirm(&["systemctl", "poweroff"])),
    ];

    let mut tiles = Vec::new();
    for (icon, title, sub_text, action) in items {
        let (btn, sub) = util::tile(icon, title, sub_text);
        let ctl = ctl.clone();
        let sub_text = sub_text.to_string();
        let armed = Rc::new(Cell::new(false));
        btn.connect_clicked(move |b| match &action {
            Action::Spawn(cmd) => {
                exec(cmd);
                ctl.hide();
            }
            Action::HideThen(cmd) => {
                let cmd = *cmd;
                ctl.hide();
                glib::timeout_add_local_once(Duration::from_millis(300), move || exec(cmd));
            }
            Action::Confirm(cmd) => {
                if armed.get() {
                    exec(cmd);
                    return;
                }
                armed.set(true);
                b.add_css_class("armed");
                sub.set_text("Pressione de novo para confirmar");
                let (armed, b, sub, txt) = (armed.clone(), b.clone(), sub.clone(), sub_text.clone());
                glib::timeout_add_local_once(Duration::from_secs(3), move || {
                    armed.set(false);
                    b.remove_css_class("armed");
                    sub.set_text(&txt);
                });
            }
        });
        tiles.push(btn);
    }

    root.append(&util::section("SISTEMA"));
    root.append(&util::tile_grid(3, &tiles[..3]));
    root.append(&util::section("ENERGIA"));
    root.append(&util::tile_grid(3, &tiles[3..]));

    let (quit, _) = util::tile("application-exit-symbolic", "Fechar o Orbit", "Encerra o app em segundo plano");
    quit.add_css_class("subtle");
    let app = app.clone();
    quit.connect_clicked(move |_| app.quit());
    root.append(&util::section("ORBIT"));
    root.append(&util::tile_grid(3, &[quit]));
    root
}
