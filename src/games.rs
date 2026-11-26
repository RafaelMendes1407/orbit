//! Biblioteca de jogos: Steam (appmanifests + arte local) e apps .desktop da categoria Game.

use crate::{util, Ctl};
use gtk::prelude::*;
use gtk::{gdk, gdk_pixbuf, gio, glib, pango};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

const COLS: usize = 6;
const CARD_W: i32 = 128;
const CARD_H: i32 = 192;

/// Entradas da Steam que não são jogos.
const STEAM_TOOLS: &[&str] = &[
    "Proton",
    "Steam Linux Runtime",
    "Steamworks Common",
    "Steam Runtime",
];

#[derive(Clone)]
pub enum Launch {
    BigPicture,
    Steam(String),
    App(gio_unix::DesktopAppInfo),
}

#[derive(Clone)]
pub struct Game {
    pub name: String,
    pub art: Option<PathBuf>,
    pub icon: Option<gio::Icon>,
    pub launch: Launch,
    pub last_played: u64,
}

// ---------------------------------------------------------------- Steam

fn steam_roots() -> Vec<PathBuf> {
    let home = glib::home_dir();
    let mut seen = HashSet::new();
    [
        ".local/share/Steam",
        ".steam/steam",
        ".var/app/com.valvesoftware.Steam/.local/share/Steam",
    ]
    .iter()
    .map(|p| home.join(p))
    .filter(|p| p.join("steamapps").is_dir())
    .filter(|p| seen.insert(fs::canonicalize(p).unwrap_or_else(|_| p.clone())))
    .collect()
}

/// Linha VDF `"chave"  "valor"`.
fn kv(line: &str) -> Option<(&str, &str)> {
    let mut it = line.split('"');
    it.next()?;
    let k = it.next()?;
    it.next()?;
    let v = it.next()?;
    Some((k, v))
}

fn steam_art(root: &Path, id: &str) -> Option<PathBuf> {
    let cache = root.join("appcache/librarycache");
    let dir = cache.join(id);
    let direct = dir.join("library_600x900.jpg");
    if direct.is_file() {
        return Some(direct);
    }
    let legacy = cache.join(format!("{id}_library_600x900.jpg"));
    if legacy.is_file() {
        return Some(legacy);
    }
    // Layout novo: subpastas com hash.
    let mut header = None;
    if let Ok(rd) = fs::read_dir(&dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                let c = p.join("library_600x900.jpg");
                if c.is_file() {
                    return Some(c);
                }
                let h = p.join("header.jpg");
                if h.is_file() {
                    header = Some(h);
                }
            }
        }
    }
    header.or_else(|| Some(dir.join("header.jpg")).filter(|h| h.is_file()))
}

fn steam_games(root: &Path, out: &mut Vec<Game>, ids: &mut HashSet<String>) {
    let mut libs = vec![root.to_path_buf()];
    if let Ok(s) = fs::read_to_string(root.join("steamapps/libraryfolders.vdf")) {
        for l in s.lines() {
            if let Some(("path", v)) = kv(l.trim()) {
                let p = PathBuf::from(v);
                if !libs.contains(&p) {
                    libs.push(p);
                }
            }
        }
    }
    for lib in libs {
        let Ok(rd) = fs::read_dir(lib.join("steamapps")) else { continue };
        for e in rd.flatten() {
            let fname = e.file_name();
            let fname = fname.to_string_lossy();
            if !(fname.starts_with("appmanifest_") && fname.ends_with(".acf")) {
                continue;
            }
            let Ok(s) = fs::read_to_string(e.path()) else { continue };
            let (mut id, mut name, mut last, mut flags) = (None, None, 0u64, 0u32);
            for l in s.lines() {
                match kv(l.trim()) {
                    Some(("appid", v)) if id.is_none() => id = Some(v.to_string()),
                    Some(("name", v)) if name.is_none() => name = Some(v.to_string()),
                    Some(("LastPlayed", v)) => last = v.parse().unwrap_or(0),
                    Some(("StateFlags", v)) => flags = v.parse().unwrap_or(0),
                    _ => {}
                }
            }
            let (Some(id), Some(name)) = (id, name) else { continue };
            let installed = flags & 4 != 0;
            if !installed || STEAM_TOOLS.iter().any(|t| name.starts_with(t)) {
                continue;
            }
            if !ids.insert(id.clone()) {
                continue;
            }
            out.push(Game {
                art: steam_art(root, &id),
                name,
                icon: None,
                launch: Launch::Steam(id),
                last_played: last,
            });
        }
    }
}

fn steam_cmd(args: &[&str]) {
    if util::which("steam") {
        util::spawn("steam", args);
    } else {
        let mut a = vec!["run", "com.valvesoftware.Steam"];
        a.extend_from_slice(args);
        util::spawn("flatpak", &a);
    }
}

// ---------------------------------------------------------------- .desktop

fn desktop_games(out: &mut Vec<Game>) {
    for app in gio::AppInfo::all() {
        let Ok(d) = app.downcast::<gio_unix::DesktopAppInfo>() else { continue };
        if !d.should_show() {
            continue;
        }
        let cats = d.categories().map(|c| c.to_string()).unwrap_or_default();
        if !cats.split(';').any(|c| c == "Game") {
            continue;
        }
        let id = d.id().map(|s| s.to_lowercase()).unwrap_or_default();
        let cmd = d
            .commandline()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        // Atalhos da Steam já aparecem pela biblioteca; a própria Steam vira o card Big Picture.
        if cmd.contains("steam://") || id == "steam.desktop" || id.contains("valvesoftware.steam") {
            continue;
        }
        out.push(Game {
            name: d.display_name().to_string(),
            art: None,
            icon: d.icon(),
            launch: Launch::App(d),
            last_played: 0,
        });
    }
}

pub fn scan() -> Vec<Game> {
    let roots = steam_roots();
    let mut games = Vec::new();

    if !roots.is_empty() || util::which("steam") {
        games.push(Game {
            name: "Steam Big Picture".into(),
            art: None,
            icon: Some(gio::ThemedIcon::new("steam").upcast()),
            launch: Launch::BigPicture,
            last_played: u64::MAX,
        });
    }

    let mut steam = Vec::new();
    let mut ids = HashSet::new();
    for r in &roots {
        steam_games(r, &mut steam, &mut ids);
    }
    // Jogados recentemente primeiro, como na Game Bar.
    steam.sort_by(|a, b| b.last_played.cmp(&a.last_played).then(a.name.cmp(&b.name)));
    games.extend(steam);

    let mut other = Vec::new();
    desktop_games(&mut other);
    other.sort_by_key(|g| g.name.to_lowercase());
    games.extend(other);
    games
}

pub fn launch(l: &Launch) {
    match l {
        Launch::BigPicture => {
            if util::process_running("steam") {
                steam_cmd(&["steam://open/bigpicture"]);
            } else {
                steam_cmd(&["-gamepadui"]);
            }
        }
        Launch::Steam(id) => steam_cmd(&[&format!("steam://rungameid/{id}")]),
        Launch::App(d) => {
            let ctx = gdk::Display::default().map(|d| d.app_launch_context());
            if let Err(e) = d.launch(&[] as &[gio::File], ctx.as_ref()) {
                util::notify("Não foi possível abrir", &e.to_string());
            }
        }
    }
}

// ---------------------------------------------------------------- UI

pub struct GamesPage {
    pub root: gtk::ScrolledWindow,
    holder: gtk::Box,
    sig: RefCell<String>,
    textures: RefCell<HashMap<PathBuf, gdk::Texture>>,
    ctl: Ctl,
}

impl GamesPage {
    pub fn new(ctl: Ctl) -> Self {
        let holder = gtk::Box::new(gtk::Orientation::Vertical, 0);
        holder.add_css_class("page");
        let root = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&holder)
            .build();
        Self {
            root,
            holder,
            sig: RefCell::default(),
            textures: RefCell::default(),
            ctl,
        }
    }

    /// Reescaneia (barato: alguns arquivos pequenos) e só reconstrói se algo mudou.
    pub fn refresh(&self) {
        let games = scan();
        let sig = games.iter().map(|g| g.name.as_str()).collect::<Vec<_>>().join("\n");
        if *self.sig.borrow() == sig {
            return;
        }
        *self.sig.borrow_mut() = sig;

        util::clear_box(&self.holder);
        if games.is_empty() {
            let l = gtk::Label::new(Some("Nenhum jogo encontrado"));
            l.add_css_class("dim");
            l.set_vexpand(true);
            self.holder.append(&l);
            return;
        }
        let grid = gtk::Grid::builder()
            .column_spacing(14)
            .row_spacing(14)
            .halign(gtk::Align::Center)
            .build();
        for (i, g) in games.iter().enumerate() {
            grid.attach(&self.card(g), (i % COLS) as i32, (i / COLS) as i32, 1, 1);
        }
        self.holder.append(&grid);
    }

    fn card(&self, g: &Game) -> gtk::Button {
        let btn = gtk::Button::new();
        btn.add_css_class("card");
        btn.set_tooltip_text(Some(&g.name));

        let col = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let art = gtk::Box::new(gtk::Orientation::Vertical, 0);
        art.add_css_class("art");
        art.set_overflow(gtk::Overflow::Hidden);
        art.set_size_request(CARD_W, CARD_H);

        match g.art.as_deref().and_then(|p| self.texture(p)) {
            Some(tex) => {
                let pic = gtk::Picture::for_paintable(&tex);
                pic.set_content_fit(gtk::ContentFit::Cover);
                pic.set_can_shrink(true);
                pic.set_vexpand(true);
                art.append(&pic);
            }
            None => {
                art.add_css_class("art-icon");
                let img = match &g.icon {
                    Some(i) => gtk::Image::from_gicon(i),
                    None => gtk::Image::from_icon_name("applications-games-symbolic"),
                };
                img.set_pixel_size(72);
                img.set_vexpand(true);
                img.set_valign(gtk::Align::Center);
                art.append(&img);
            }
        }

        let label = gtk::Label::new(Some(&g.name));
        label.set_ellipsize(pango::EllipsizeMode::End);
        label.set_max_width_chars(1);
        label.add_css_class("card-title");

        col.append(&art);
        col.append(&label);
        btn.set_child(Some(&col));

        let (l, ctl) = (g.launch.clone(), self.ctl.clone());
        btn.connect_clicked(move |_| {
            launch(&l);
            ctl.hide();
        });
        btn
    }

    /// Decodifica a capa já no tamanho final (pouca RAM) e guarda em cache.
    #[allow(deprecated)]
    fn texture(&self, p: &Path) -> Option<gdk::Texture> {
        if let Some(t) = self.textures.borrow().get(p) {
            return Some(t.clone());
        }
        let scale = self.ctl.window.scale_factor().max(1);
        let pb = gdk_pixbuf::Pixbuf::from_file_at_scale(p, -1, CARD_H * scale, true).ok()?;
        let tex = gdk::Texture::for_pixbuf(&pb);
        self.textures.borrow_mut().insert(p.to_path_buf(), tex.clone());
        Some(tex)
    }
}
