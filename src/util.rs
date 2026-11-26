//! Pequenos utilitários: processos, diretórios, notificações e widgets comuns.

use gtk::prelude::*;
use gtk::{glib, pango};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

fn command(prog: &str, args: &[&str]) -> Command {
    let mut c = Command::new(prog);
    c.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        // Grupo de processos próprio: jogos/apps não morrem junto com o Orbit.
        .process_group(0);
    c
}

/// Inicia um processo desacoplado. Uma thread leve só espera ele terminar (evita zumbis).
pub fn spawn(prog: &str, args: &[&str]) -> bool {
    match command(prog, args).spawn() {
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            true
        }
        Err(e) => {
            eprintln!("orbit: falha ao executar {prog}: {e}");
            false
        }
    }
}

/// Inicia um processo e devolve o handle (para quem precisa sinalizar/parar depois).
pub fn spawn_child(prog: &str, args: &[&str]) -> Option<Child> {
    command(prog, args)
        .spawn()
        .map_err(|e| eprintln!("orbit: falha ao executar {prog}: {e}"))
        .ok()
}

/// Executa e captura stdout (locale C para saída previsível).
pub fn output(prog: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(prog)
        .args(args)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Executa e espera terminar.
pub fn run(prog: &str, args: &[&str]) -> bool {
    Command::new(prog)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn which(prog: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(prog).is_file()))
        .unwrap_or(false)
}


pub fn notify(summary: &str, body: &str) {
    spawn(
        "notify-send",
        &["-a", "Orbit", "-i", "applications-games", summary, body],
    );
}

/// Existe algum processo com esse nome (/proc/*/comm)?
pub fn process_running(name: &str) -> bool {
    let Ok(rd) = std::fs::read_dir("/proc") else {
        return false;
    };
    rd.flatten().any(|e| {
        e.file_name().to_string_lossy().bytes().all(|b| b.is_ascii_digit())
            && std::fs::read_to_string(e.path().join("comm"))
                .map(|c| c.trim_end() == name)
                .unwrap_or(false)
    })
}

pub fn timestamp() -> String {
    glib::DateTime::now_local()
        .ok()
        .and_then(|d| d.format("%Y-%m-%d_%H-%M-%S").ok())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "agora".into())
}

/// Diretório XDG do usuário (Imagens, Vídeos...) + subpasta, criado se preciso.
pub fn user_dir(kind: glib::UserDirectory, fallback: &str, sub: &str) -> PathBuf {
    let base = glib::user_special_dir(kind).unwrap_or_else(|| glib::home_dir().join(fallback));
    let dir = base.join(sub);
    let _ = std::fs::create_dir_all(&dir);
    dir
}

pub fn clear_box(b: &gtk::Box) {
    while let Some(c) = b.first_child() {
        b.remove(&c);
    }
}

pub fn section(title: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(title));
    l.set_xalign(0.0);
    l.add_css_class("section-title");
    l
}

/// Botão "tile": ícone + título + subtítulo. Devolve o subtítulo para atualizações.
pub fn tile(icon: &str, title: &str, sub: &str) -> (gtk::Button, gtk::Label) {
    let btn = gtk::Button::new();
    btn.add_css_class("tile");
    btn.set_hexpand(true);

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 14);
    let img = gtk::Image::from_icon_name(icon);
    img.set_pixel_size(26);
    img.add_css_class("tile-icon");

    let col = gtk::Box::new(gtk::Orientation::Vertical, 2);
    col.set_valign(gtk::Align::Center);
    col.set_hexpand(true);
    let t = gtk::Label::new(Some(title));
    t.set_xalign(0.0);
    t.set_ellipsize(pango::EllipsizeMode::End);
    t.set_max_width_chars(1);
    t.add_css_class("tile-title");
    let s = gtk::Label::new(Some(sub));
    s.set_xalign(0.0);
    s.set_ellipsize(pango::EllipsizeMode::End);
    s.set_max_width_chars(1);
    s.add_css_class("tile-sub");
    col.append(&t);
    col.append(&s);

    row.append(&img);
    row.append(&col);
    btn.set_child(Some(&row));
    (btn, s)
}

/// Grid homogênea de tiles.
pub fn tile_grid(cols: usize, tiles: &[gtk::Button]) -> gtk::Grid {
    let g = gtk::Grid::builder()
        .column_spacing(12)
        .row_spacing(12)
        .column_homogeneous(true)
        .build();
    for (i, t) in tiles.iter().enumerate() {
        g.attach(t, (i % cols) as i32, (i / cols) as i32, 1, 1);
    }
    g
}

/// Área de trabalho atual (XDG_CURRENT_DESKTOP em minúsculas, ex.: "cosmic", "kde", "gnome", "hyprland").
pub fn desktop() -> String {
    std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default().to_lowercase()
}

pub fn is_desktop(name: &str) -> bool {
    desktop().split(':').any(|d| d == name)
}

pub fn is_wayland() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty())
}
