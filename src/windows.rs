//! Programas abertos: lista as janelas do ambiente gráfico e permite alternar ou fechar.
//!
//! Backends, escolhidos automaticamente:
//! - **COSMIC**: `ext_foreign_toplevel_list_v1` + `zcosmic_toplevel_info/manager_v1`
//! - **Hyprland, Sway, niri, labwc, Wayfire, river…**: `zwlr_foreign_toplevel_manager_v1`
//! - **KDE Plasma**: `org_kde_plasma_window_management`
//! - **Sessões X11** (qualquer gerenciador EWMH: XFCE, Cinnamon, MATE, KDE/GNOME em X11…)
//! - Só `ext_foreign_toplevel_list_v1`: lista, mas sem poder fechar.
//!
//! As conexões rodam numa thread que dorme até o servidor gráfico mandar algo
//! (custo ocioso zero). "Fechar" é o mesmo pedido do botão X da janela: o programa
//! pode perguntar se quer salvar antes.

use crate::{util, Ctl};
use cosmic_protocols::toplevel_info::v1::client::{
    zcosmic_toplevel_handle_v1::{self, ZcosmicToplevelHandleV1},
    zcosmic_toplevel_info_v1::{self, ZcosmicToplevelInfoV1},
};
use cosmic_protocols::toplevel_management::v1::client::zcosmic_toplevel_manager_v1::{
    self, ZcosmicToplevelManagerV1,
};
use gtk::prelude::*;
use gtk::{gio, glib, pango};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use wayland_client::backend::ObjectId;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{wl_registry, wl_seat};
use wayland_client::{event_created_child, Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::{
    ext_foreign_toplevel_handle_v1::{self, ExtForeignToplevelHandleV1},
    ext_foreign_toplevel_list_v1::{self, ExtForeignToplevelListV1},
};
use wayland_protocols_plasma::plasma_window_management::client::{
    org_kde_plasma_window::{self, OrgKdePlasmaWindow},
    org_kde_plasma_window_management::{self, OrgKdePlasmaWindowManagement},
};
use wayland_protocols_wlr::foreign_toplevel::v1::client::{
    zwlr_foreign_toplevel_handle_v1::{self, ZwlrForeignToplevelHandleV1},
    zwlr_foreign_toplevel_manager_v1::{self, ZwlrForeignToplevelManagerV1},
};

// ---------------------------------------------------------------- Modelo

#[derive(Clone)]
enum Handle {
    None,
    Cosmic(ZcosmicToplevelHandleV1),
    Wlr(ZwlrForeignToplevelHandleV1),
    Plasma(OrgKdePlasmaWindow),
    X11(u32),
}

#[derive(Clone)]
pub struct Toplevel {
    /// Ordem de abertura (estável enquanto a janela existir).
    pub id: u64,
    pub title: String,
    pub app_id: String,
    pub activated: bool,
    pub minimized: bool,
    handle: Handle,
}

impl Toplevel {
    fn closable(&self) -> bool {
        !matches!(self.handle, Handle::None)
    }
}

/// Envia pedidos ao servidor gráfico (usado da thread da interface).
pub enum Control {
    Wayland {
        conn: Connection,
        cosmic: Option<ZcosmicToplevelManagerV1>,
        seat: Option<wl_seat::WlSeat>,
    },
    X11(Arc<x11::X11>),
}

const PLASMA_ACTIVE: u32 = 0x1;
const PLASMA_MINIMIZED: u32 = 0x2;
const PLASMA_SKIP_TASKBAR: u32 = 0x1000;

impl Control {
    pub fn close(&self, t: &Toplevel) {
        match (self, &t.handle) {
            (Control::Wayland { conn, cosmic, .. }, h) => {
                match h {
                    Handle::Cosmic(h) => {
                        if let Some(m) = cosmic {
                            m.close(h);
                        }
                    }
                    Handle::Wlr(h) => h.close(),
                    Handle::Plasma(w) => w.close(),
                    _ => {}
                }
                let _ = conn.flush();
            }
            (Control::X11(x), Handle::X11(win)) => x.close(*win),
            _ => {}
        }
    }

    pub fn activate(&self, t: &Toplevel) {
        match (self, &t.handle) {
            (Control::Wayland { conn, cosmic, seat }, h) => {
                match (h, seat) {
                    (Handle::Cosmic(h), Some(s)) => {
                        if let Some(m) = cosmic {
                            m.activate(h, s);
                        }
                    }
                    (Handle::Wlr(h), Some(s)) => h.activate(s),
                    (Handle::Plasma(w), _) => {
                        w.set_state(PLASMA_ACTIVE | PLASMA_MINIMIZED, PLASMA_ACTIVE)
                    }
                    _ => {}
                }
                let _ = conn.flush();
            }
            (Control::X11(x), Handle::X11(win)) => x.activate(*win),
            _ => {}
        }
    }
}

/// Escolhe o backend do ambiente atual. `None` se não há como listar janelas
/// (ex.: GNOME no Wayland, que não expõe isso a aplicativos).
pub fn start(tx: async_channel::Sender<Vec<Toplevel>>) -> Option<Control> {
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty());
    if wayland {
        if let Some(c) = start_wayland(tx.clone()) {
            return Some(c);
        }
    }
    let x11_session = std::env::var("XDG_SESSION_TYPE").map(|s| s == "x11").unwrap_or(!wayland);
    if x11_session {
        return x11::start(tx).map(Control::X11);
    }
    None
}

/// `orbit --list-windows`: imprime o backend e as janelas vistas (diagnóstico).
pub fn print_list() -> bool {
    let (tx, rx) = async_channel::unbounded();
    let Some(c) = start(tx) else {
        eprintln!("nenhum backend de janelas disponível neste ambiente");
        return false;
    };
    let kind = match &c {
        Control::Wayland { cosmic: Some(_), .. } => "wayland (cosmic)",
        Control::Wayland { .. } => "wayland",
        Control::X11(_) => "x11",
    };
    println!("backend: {kind}");
    // Espera as atualizações iniciais assentarem.
    let mut last = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(700);
    while std::time::Instant::now() < deadline {
        while let Ok(v) = rx.try_recv() {
            last = v;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    for t in &last {
        let flag = if t.activated { "*" } else if t.minimized { "_" } else { " " };
        println!("{flag} [{}] {}{}", t.app_id, t.title, if t.closable() { "" } else { "  (sem fechar)" });
    }
    true
}

// ---------------------------------------------------------------- Wayland

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Cosmic,
    Wlr,
    Plasma,
    ListOnly,
}

struct Entry {
    id: u64,
    title: String,
    app_id: String,
    activated: bool,
    minimized: bool,
    /// Já recebeu o estado inicial completo.
    ready: bool,
    skip: bool,
    handle: Handle,
    ext: Option<ExtForeignToplevelHandleV1>,
}

struct State {
    kind: Kind,
    entries: HashMap<ObjectId, Entry>,
    next_id: u64,
    info: Option<ZcosmicToplevelInfoV1>,
    tx: async_channel::Sender<Vec<Toplevel>>,
}

impl State {
    fn add(&mut self, key: ObjectId, handle: Handle, ext: Option<ExtForeignToplevelHandleV1>) {
        self.next_id += 1;
        self.entries.insert(
            key,
            Entry {
                id: self.next_id,
                title: String::new(),
                app_id: String::new(),
                activated: false,
                minimized: false,
                ready: false,
                skip: false,
                handle,
                ext,
            },
        );
    }

    fn entry(&mut self, key: &ObjectId) -> Option<&mut Entry> {
        self.entries.get_mut(key)
    }

    fn publish(&self) {
        let mut v: Vec<Toplevel> = self
            .entries
            .values()
            .filter(|e| e.ready && !e.skip)
            .map(|e| Toplevel {
                id: e.id,
                title: e.title.clone(),
                app_id: e.app_id.clone(),
                activated: e.activated,
                minimized: e.minimized,
                handle: e.handle.clone(),
            })
            .collect();
        v.sort_by_key(|t| t.id);
        let _ = self.tx.try_send(v);
    }
}

fn states_u32(bytes: &[u8]) -> Vec<u32> {
    bytes
        .chunks_exact(4)
        .map(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

fn start_wayland(tx: async_channel::Sender<Vec<Toplevel>>) -> Option<Control> {
    let conn = Connection::connect_to_env().ok()?;
    let (globals, mut queue) = registry_queue_init::<State>(&conn).ok()?;
    let qh = queue.handle();
    let has = |name: &str| globals.contents().with_list(|l| l.iter().any(|g| g.interface == name));

    let seat: Option<wl_seat::WlSeat> = globals.bind(&qh, 1..=1, ()).ok();
    let (mut info, mut cosmic) = (None, None);
    let kind = if has("ext_foreign_toplevel_list_v1")
        && has("zcosmic_toplevel_info_v1")
        && has("zcosmic_toplevel_manager_v1")
    {
        info = Some(globals.bind::<ZcosmicToplevelInfoV1, _, _>(&qh, 2..=3, ()).ok()?);
        cosmic = Some(globals.bind::<ZcosmicToplevelManagerV1, _, _>(&qh, 1..=4, ()).ok()?);
        globals.bind::<ExtForeignToplevelListV1, _, _>(&qh, 1..=1, ()).ok()?;
        Kind::Cosmic
    } else if has("zwlr_foreign_toplevel_manager_v1") {
        globals.bind::<ZwlrForeignToplevelManagerV1, _, _>(&qh, 1..=3, ()).ok()?;
        Kind::Wlr
    } else if has("org_kde_plasma_window_management") {
        globals.bind::<OrgKdePlasmaWindowManagement, _, _>(&qh, 1..=16, ()).ok()?;
        Kind::Plasma
    } else if has("ext_foreign_toplevel_list_v1") {
        globals.bind::<ExtForeignToplevelListV1, _, _>(&qh, 1..=1, ()).ok()?;
        Kind::ListOnly
    } else {
        return None;
    };

    let mut state = State { kind, entries: HashMap::new(), next_id: 0, info, tx };
    std::thread::Builder::new()
        .name("orbit-windows".into())
        .spawn(move || loop {
            if state.tx.is_closed() || queue.blocking_dispatch(&mut state).is_err() {
                eprintln!("orbit: conexão de janelas encerrada");
                return;
            }
        })
        .ok()?;
    Some(Control::Wayland { conn, cosmic, seat })
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(_: &mut Self, _: &wl_registry::WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_seat::WlSeat, ()> for State {
    fn event(_: &mut Self, _: &wl_seat::WlSeat, _: wl_seat::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

// --- ext-foreign-toplevel-list (COSMIC e genérico)

impl Dispatch<ExtForeignToplevelListV1, ()> for State {
    fn event(
        st: &mut Self,
        _: &ExtForeignToplevelListV1,
        ev: ext_foreign_toplevel_list_v1::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let ext_foreign_toplevel_list_v1::Event::Toplevel { toplevel } = ev {
            let handle = match (&st.info, st.kind) {
                (Some(info), Kind::Cosmic) => {
                    Handle::Cosmic(info.get_cosmic_toplevel(&toplevel, qh, toplevel.id()))
                }
                _ => Handle::None,
            };
            st.add(toplevel.id(), handle, Some(toplevel));
        }
    }

    event_created_child!(State, ExtForeignToplevelListV1, [
        ext_foreign_toplevel_list_v1::EVT_TOPLEVEL_OPCODE => (ExtForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ExtForeignToplevelHandleV1, ()> for State {
    fn event(
        st: &mut Self,
        h: &ExtForeignToplevelHandleV1,
        ev: ext_foreign_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use ext_foreign_toplevel_handle_v1::Event;
        match ev {
            Event::Title { title } => {
                if let Some(e) = st.entry(&h.id()) {
                    e.title = title;
                }
            }
            Event::AppId { app_id } => {
                if let Some(e) = st.entry(&h.id()) {
                    e.app_id = app_id;
                }
            }
            Event::Done => {
                if let Some(e) = st.entry(&h.id()) {
                    e.ready = true;
                }
                st.publish();
            }
            Event::Closed => {
                if let Some(e) = st.entries.remove(&h.id()) {
                    if let Handle::Cosmic(c) = e.handle {
                        c.destroy();
                    }
                    if let Some(x) = e.ext {
                        x.destroy();
                    }
                }
                st.publish();
            }
            _ => {}
        }
    }
}

// --- COSMIC

impl Dispatch<ZcosmicToplevelManagerV1, ()> for State {
    fn event(_: &mut Self, _: &ZcosmicToplevelManagerV1, _: zcosmic_toplevel_manager_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<ZcosmicToplevelInfoV1, ()> for State {
    fn event(
        st: &mut Self,
        _: &ZcosmicToplevelInfoV1,
        ev: zcosmic_toplevel_info_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zcosmic_toplevel_info_v1::Event::Done = ev {
            st.publish();
        }
    }

    // Evento antigo (v1); não é enviado porque pedimos v2+, mas precisa ser declarado.
    event_created_child!(State, ZcosmicToplevelInfoV1, [
        zcosmic_toplevel_info_v1::EVT_TOPLEVEL_OPCODE => (ZcosmicToplevelHandleV1, ObjectId::null()),
    ]);
}

impl Dispatch<ZcosmicToplevelHandleV1, ObjectId> for State {
    fn event(
        st: &mut Self,
        _: &ZcosmicToplevelHandleV1,
        ev: zcosmic_toplevel_handle_v1::Event,
        ext_id: &ObjectId,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zcosmic_toplevel_handle_v1::Event::State { state } = ev {
            if let Some(e) = st.entry(ext_id) {
                let s = states_u32(&state);
                e.activated = s.contains(&(zcosmic_toplevel_handle_v1::State::Activated as u32));
                e.minimized = s.contains(&(zcosmic_toplevel_handle_v1::State::Minimized as u32));
            }
        }
    }
}

// --- wlr (Hyprland, Sway, niri, labwc, Wayfire, river…)

impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for State {
    fn event(
        st: &mut Self,
        _: &ZwlrForeignToplevelManagerV1,
        ev: zwlr_foreign_toplevel_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwlr_foreign_toplevel_manager_v1::Event::Toplevel { toplevel } = ev {
            st.add(toplevel.id(), Handle::Wlr(toplevel), None);
        }
    }

    event_created_child!(State, ZwlrForeignToplevelManagerV1, [
        zwlr_foreign_toplevel_manager_v1::EVT_TOPLEVEL_OPCODE => (ZwlrForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ZwlrForeignToplevelHandleV1, ()> for State {
    fn event(
        st: &mut Self,
        h: &ZwlrForeignToplevelHandleV1,
        ev: zwlr_foreign_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use zwlr_foreign_toplevel_handle_v1::Event;
        match ev {
            Event::Title { title } => {
                if let Some(e) = st.entry(&h.id()) {
                    e.title = title;
                }
            }
            Event::AppId { app_id } => {
                if let Some(e) = st.entry(&h.id()) {
                    e.app_id = app_id;
                }
            }
            Event::State { state } => {
                if let Some(e) = st.entry(&h.id()) {
                    let s = states_u32(&state);
                    e.activated = s.contains(&(zwlr_foreign_toplevel_handle_v1::State::Activated as u32));
                    e.minimized = s.contains(&(zwlr_foreign_toplevel_handle_v1::State::Minimized as u32));
                }
            }
            Event::Done => {
                if let Some(e) = st.entry(&h.id()) {
                    e.ready = true;
                }
                st.publish();
            }
            Event::Closed => {
                st.entries.remove(&h.id());
                h.destroy();
                st.publish();
            }
            _ => {}
        }
    }
}

// --- KDE Plasma

impl Dispatch<OrgKdePlasmaWindowManagement, ()> for State {
    fn event(
        _: &mut Self,
        mgr: &OrgKdePlasmaWindowManagement,
        ev: org_kde_plasma_window_management::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        use org_kde_plasma_window_management::Event;
        match ev {
            Event::WindowWithUuid { uuid, .. } => {
                mgr.get_window_by_uuid(uuid, qh, ());
            }
            // Versões antigas (< 13) só anunciam pelo id interno.
            Event::Window { id } if mgr.version() < 13 => {
                mgr.get_window(id, qh, ());
            }
            _ => {}
        }
    }
}

impl Dispatch<OrgKdePlasmaWindow, ()> for State {
    fn event(
        st: &mut Self,
        w: &OrgKdePlasmaWindow,
        ev: org_kde_plasma_window::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use org_kde_plasma_window::Event;
        if !st.entries.contains_key(&w.id()) {
            st.add(w.id(), Handle::Plasma(w.clone()), None);
            // Antes da v4 não existe initial_state: considera pronto já.
            if w.version() < 4 {
                if let Some(e) = st.entry(&w.id()) {
                    e.ready = true;
                }
            }
        }
        let ready = st.entry(&w.id()).map(|e| e.ready).unwrap_or(false);
        match ev {
            Event::TitleChanged { title } => {
                if let Some(e) = st.entry(&w.id()) {
                    e.title = title;
                }
            }
            Event::AppIdChanged { app_id } => {
                if let Some(e) = st.entry(&w.id()) {
                    e.app_id = app_id;
                }
            }
            Event::StateChanged { flags } => {
                if let Some(e) = st.entry(&w.id()) {
                    e.activated = flags & PLASMA_ACTIVE != 0;
                    e.minimized = flags & PLASMA_MINIMIZED != 0;
                    e.skip = flags & PLASMA_SKIP_TASKBAR != 0;
                }
            }
            Event::InitialState => {
                if let Some(e) = st.entry(&w.id()) {
                    e.ready = true;
                }
                st.publish();
                return;
            }
            Event::Unmapped => {
                st.entries.remove(&w.id());
                if w.version() >= 4 {
                    w.destroy();
                }
                st.publish();
                return;
            }
            _ => return,
        }
        if ready {
            st.publish();
        }
    }
}

// ---------------------------------------------------------------- X11 (EWMH)

pub mod x11 {
    use super::{Handle, Toplevel};
    use std::collections::HashSet;
    use std::sync::Arc;
    use x11rb::connection::Connection as _;
    use x11rb::protocol::xproto::{
        AtomEnum, ChangeWindowAttributesAux, ClientMessageEvent, ConnectionExt as _, EventMask,
    };
    use x11rb::protocol::Event;
    use x11rb::rust_connection::RustConnection;

    x11rb::atom_manager! {
        pub Atoms: AtomsCookie {
            _NET_CLIENT_LIST,
            _NET_ACTIVE_WINDOW,
            _NET_CLOSE_WINDOW,
            _NET_WM_NAME,
            _NET_WM_PID,
            _NET_WM_STATE,
            _NET_WM_STATE_HIDDEN,
            _NET_WM_STATE_SKIP_TASKBAR,
            _NET_WM_WINDOW_TYPE,
            _NET_WM_WINDOW_TYPE_NORMAL,
            _NET_WM_WINDOW_TYPE_DIALOG,
            UTF8_STRING,
        }
    }

    pub struct X11 {
        conn: RustConnection,
        root: u32,
        atoms: Atoms,
    }

    pub fn start(tx: async_channel::Sender<Vec<Toplevel>>) -> Option<Arc<X11>> {
        let (conn, screen) = x11rb::connect(None).ok()?;
        let root = conn.setup().roots.get(screen)?.root;
        let atoms = Atoms::new(&conn).ok()?.reply().ok()?;
        conn.change_window_attributes(root, &ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE))
            .ok()?;
        conn.flush().ok()?;
        let x = Arc::new(X11 { conn, root, atoms });
        let xt = x.clone();
        std::thread::Builder::new()
            .name("orbit-x11".into())
            .spawn(move || {
                let mut watched = HashSet::new();
                loop {
                    if tx.is_closed() {
                        return;
                    }
                    let _ = tx.try_send(xt.snapshot(&mut watched));
                    let Ok(ev) = xt.conn.wait_for_event() else { return };
                    let mut relevant = xt.relevant(&ev);
                    // Junta rajadas de eventos.
                    while let Ok(Some(ev)) = xt.conn.poll_for_event() {
                        relevant |= xt.relevant(&ev);
                    }
                    while !relevant {
                        let Ok(ev) = xt.conn.wait_for_event() else { return };
                        relevant = xt.relevant(&ev);
                    }
                }
            })
            .ok()?;
        Some(x)
    }

    impl X11 {
        fn relevant(&self, ev: &Event) -> bool {
            let a = &self.atoms;
            match ev {
                Event::PropertyNotify(p) => [
                    a._NET_CLIENT_LIST,
                    a._NET_ACTIVE_WINDOW,
                    a._NET_WM_NAME,
                    a._NET_WM_STATE,
                    AtomEnum::WM_NAME.into(),
                ]
                .contains(&p.atom),
                _ => false,
            }
        }

        fn prop32(&self, win: u32, atom: u32, ty: impl Into<u32>) -> Vec<u32> {
            self.conn
                .get_property(false, win, atom, ty, 0, 1024)
                .ok()
                .and_then(|c| c.reply().ok())
                .and_then(|r| r.value32().map(|v| v.collect()))
                .unwrap_or_default()
        }

        fn prop_bytes(&self, win: u32, atom: u32, ty: impl Into<u32>) -> Option<Vec<u8>> {
            let r = self.conn.get_property(false, win, atom, ty, 0, 4096).ok()?.reply().ok()?;
            (!r.value.is_empty()).then_some(r.value)
        }

        fn snapshot(&self, watched: &mut HashSet<u32>) -> Vec<Toplevel> {
            let a = &self.atoms;
            let me = std::process::id();
            let active = self.prop32(self.root, a._NET_ACTIVE_WINDOW, AtomEnum::WINDOW).first().copied();
            let list = self.prop32(self.root, a._NET_CLIENT_LIST, AtomEnum::WINDOW);
            watched.retain(|w| list.contains(w));
            let mut out = Vec::new();
            for (i, &win) in list.iter().enumerate() {
                if watched.insert(win) {
                    let _ = self.conn.change_window_attributes(
                        win,
                        &ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE),
                    );
                }
                if self.prop32(win, a._NET_WM_PID, AtomEnum::CARDINAL).first() == Some(&me) {
                    continue; // o próprio Orbit
                }
                let types = self.prop32(win, a._NET_WM_WINDOW_TYPE, AtomEnum::ATOM);
                if !types.is_empty()
                    && !types.iter().any(|t| *t == a._NET_WM_WINDOW_TYPE_NORMAL || *t == a._NET_WM_WINDOW_TYPE_DIALOG)
                {
                    continue;
                }
                let state = self.prop32(win, a._NET_WM_STATE, AtomEnum::ATOM);
                if state.contains(&a._NET_WM_STATE_SKIP_TASKBAR) {
                    continue;
                }
                let title = self
                    .prop_bytes(win, a._NET_WM_NAME, a.UTF8_STRING)
                    .or_else(|| self.prop_bytes(win, AtomEnum::WM_NAME.into(), AtomEnum::STRING))
                    .map(|b| String::from_utf8_lossy(&b).into_owned())
                    .unwrap_or_default();
                // WM_CLASS = "instância\0classe\0"
                let app_id = self
                    .prop_bytes(win, AtomEnum::WM_CLASS.into(), AtomEnum::STRING)
                    .map(|b| {
                        let parts: Vec<String> = b
                            .split(|c| *c == 0)
                            .filter(|s| !s.is_empty())
                            .map(|s| String::from_utf8_lossy(s).into_owned())
                            .collect();
                        parts.get(1).or(parts.first()).cloned().unwrap_or_default()
                    })
                    .unwrap_or_default();
                out.push(Toplevel {
                    id: i as u64,
                    title,
                    app_id,
                    activated: Some(win) == active,
                    minimized: state.contains(&a._NET_WM_STATE_HIDDEN),
                    handle: Handle::X11(win),
                });
            }
            let _ = self.conn.flush();
            out
        }

        fn send(&self, win: u32, atom: u32, data: [u32; 5]) {
            let ev = ClientMessageEvent::new(32, win, atom, data);
            let _ = self.conn.send_event(
                false,
                self.root,
                EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
                ev,
            );
            let _ = self.conn.flush();
        }

        pub fn close(&self, win: u32) {
            // fonte = 2 (pager/painel): o gerenciador trata como pedido do usuário.
            self.send(win, self.atoms._NET_CLOSE_WINDOW, [0, 2, 0, 0, 0]);
        }

        pub fn activate(&self, win: u32) {
            self.send(win, self.atoms._NET_ACTIVE_WINDOW, [2, 0, 0, 0, 0]);
        }
    }
}

// ---------------------------------------------------------------- Ícone/nome do app

#[derive(Clone)]
struct AppMeta {
    name: Option<String>,
    icon: Option<gio::Icon>,
}

fn lookup_app(app_id: &str) -> AppMeta {
    let none = AppMeta { name: None, icon: None };
    if app_id.is_empty() {
        return none;
    }
    let from = |d: gio_unix::DesktopAppInfo| AppMeta {
        name: Some(d.display_name().to_string()),
        icon: d.icon(),
    };
    for id in [app_id.to_string(), app_id.to_lowercase()] {
        if let Some(d) = gio_unix::DesktopAppInfo::new(&format!("{id}.desktop")) {
            return from(d);
        }
    }
    // Procura por StartupWMClass ou pelo final do id (ex.: "firefox" ↔ org.mozilla.firefox).
    let low = app_id.to_lowercase();
    for app in gio::AppInfo::all() {
        let Ok(d) = app.downcast::<gio_unix::DesktopAppInfo>() else { continue };
        let wm = d.startup_wm_class().map(|s| s.to_lowercase());
        let id = d.id().map(|s| s.to_lowercase()).unwrap_or_default();
        let id = id.trim_end_matches(".desktop");
        if wm.as_deref() == Some(low.as_str()) || id.rsplit('.').next() == Some(low.as_str()) {
            return from(d);
        }
    }
    // Jogos da Steam rodam como "steam_app_<id>"; a Steam instala ícones steam_icon_<id>.
    if let Some(n) = app_id.strip_prefix("steam_app_") {
        return AppMeta {
            name: None,
            icon: Some(gio::ThemedIcon::with_default_fallbacks(&format!("steam_icon_{n}")).upcast()),
        };
    }
    none
}

// ---------------------------------------------------------------- UI

#[derive(Clone, Copy, PartialEq, Eq)]
enum Part {
    Main,
    Close,
}

pub struct WindowsPage {
    pub root: gtk::ScrolledWindow,
    list: gtk::Box,
    empty: gtk::Label,
    rows: RefCell<Vec<(Toplevel, gtk::Button, gtk::Button)>>,
    sig: RefCell<String>,
    meta: RefCell<HashMap<String, AppMeta>>,
    control: Option<Rc<Control>>,
    ctl: Ctl,
}

impl WindowsPage {
    pub fn new(ctl: Ctl) -> Rc<Self> {
        let page = gtk::Box::new(gtk::Orientation::Vertical, 10);
        page.add_css_class("page");
        page.append(&util::section("PROGRAMAS ABERTOS"));
        let list = gtk::Box::new(gtk::Orientation::Vertical, 8);
        page.append(&list);
        let empty = gtk::Label::new(Some("Nenhum programa aberto"));
        empty.add_css_class("dim");
        empty.set_vexpand(true);
        empty.set_margin_top(40);
        page.append(&empty);

        let root = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&page)
            .build();

        let (tx, rx) = async_channel::unbounded();
        let control = start(tx).map(Rc::new);
        if control.is_none() {
            empty.set_text("Este ambiente gráfico não permite listar as janelas abertas");
        }

        let this = Rc::new(Self {
            root,
            list,
            empty,
            rows: RefCell::default(),
            sig: RefCell::default(),
            meta: RefCell::default(),
            control,
            ctl,
        });

        let w = Rc::downgrade(&this);
        glib::spawn_future_local(async move {
            while let Ok(mut v) = rx.recv().await {
                // Junta rajadas de eventos numa atualização só.
                while let Ok(newer) = rx.try_recv() {
                    v = newer;
                }
                match w.upgrade() {
                    Some(t) => t.apply(v),
                    None => break,
                }
            }
        });
        this
    }

    fn meta(&self, app_id: &str) -> AppMeta {
        if let Some(m) = self.meta.borrow().get(app_id) {
            return m.clone();
        }
        let m = lookup_app(app_id);
        self.meta.borrow_mut().insert(app_id.to_string(), m.clone());
        m
    }

    fn apply(self: &Rc<Self>, wins: Vec<Toplevel>) {
        // Ignora janelas sem título e sem app (ainda inicializando) e o próprio Orbit
        // (no modo janela, em GNOME/X11, ele é uma janela comum).
        let wins: Vec<Toplevel> = wins
            .into_iter()
            .filter(|t| !(t.title.is_empty() && t.app_id.is_empty()))
            .filter(|t| t.app_id != crate::APP_ID)
            .collect();
        let sig = wins
            .iter()
            .map(|t| format!("{}\u{1}{}\u{1}{}\u{1}{}{}", t.id, t.title, t.app_id, t.activated, t.minimized))
            .collect::<Vec<_>>()
            .join("\u{2}");
        if *self.sig.borrow() == sig {
            return;
        }
        *self.sig.borrow_mut() = sig;
        self.rebuild(wins);
    }

    /// Onde está o cursor: (índice da linha, id da janela, botão).
    fn focused(&self) -> Option<(usize, u64, Part)> {
        let f = self.list.root().and_then(|r| r.focus())?;
        self.rows.borrow().iter().enumerate().find_map(|(i, (t, main, close))| {
            if f == *main.upcast_ref::<gtk::Widget>() || f.is_ancestor(main) {
                Some((i, t.id, Part::Main))
            } else if f == *close.upcast_ref::<gtk::Widget>() || f.is_ancestor(close) {
                Some((i, t.id, Part::Close))
            } else {
                None
            }
        })
    }

    fn rebuild(self: &Rc<Self>, wins: Vec<Toplevel>) {
        let prev = self.focused();

        util::clear_box(&self.list);
        let mut rows = Vec::new();
        for t in wins {
            let (main, close) = self.row(&t);
            rows.push((t, main, close));
        }
        self.empty.set_visible(rows.is_empty());

        // Mantém o cursor na mesma janela; se ela fechou, vai para a da mesma posição.
        if let Some((idx, id, part)) = prev {
            let target = rows
                .iter()
                .find(|(t, _, _)| t.id == id)
                .or_else(|| rows.get(idx.min(rows.len().saturating_sub(1))))
                .map(|(_, m, c)| if part == Part::Close && rows.iter().any(|(t, _, _)| t.id == id) { c.clone() } else { m.clone() });
            if let Some(b) = target {
                b.grab_focus();
            }
        }
        *self.rows.borrow_mut() = rows;
    }

    fn row(self: &Rc<Self>, t: &Toplevel) -> (gtk::Button, gtk::Button) {
        let meta = self.meta(&t.app_id);
        let app_name = meta.name.clone().unwrap_or_else(|| {
            if t.app_id.is_empty() { "Programa".into() } else { t.app_id.clone() }
        });
        let title = if t.title.is_empty() { app_name.clone() } else { t.title.clone() };
        let mut sub = app_name;
        if t.activated {
            sub.push_str(" · em uso");
        } else if t.minimized {
            sub.push_str(" · minimizado");
        }

        let main = gtk::Button::new();
        main.add_css_class("tile");
        main.set_hexpand(true);
        if t.activated {
            main.add_css_class("selected");
        }
        let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 14);
        let img = match &meta.icon {
            Some(i) => gtk::Image::from_gicon(i),
            None => gtk::Image::from_icon_name("application-x-executable-symbolic"),
        };
        img.set_pixel_size(32);
        img.add_css_class("tile-icon");
        let col = gtk::Box::new(gtk::Orientation::Vertical, 2);
        col.set_valign(gtk::Align::Center);
        col.set_hexpand(true);
        let tl = gtk::Label::new(Some(&title));
        tl.set_xalign(0.0);
        tl.set_ellipsize(pango::EllipsizeMode::End);
        tl.set_max_width_chars(1);
        tl.add_css_class("tile-title");
        let sl = gtk::Label::new(Some(&sub));
        sl.set_xalign(0.0);
        sl.set_ellipsize(pango::EllipsizeMode::End);
        sl.set_max_width_chars(1);
        sl.add_css_class("tile-sub");
        col.append(&tl);
        col.append(&sl);
        hbox.append(&img);
        hbox.append(&col);
        main.set_child(Some(&hbox));

        let close = gtk::Button::from_icon_name("window-close-symbolic");
        close.add_css_class("tile");
        close.add_css_class("close-win");
        close.set_tooltip_text(Some("Fechar"));
        close.set_sensitive(self.control.is_some() && t.closable());

        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        row.append(&main);
        row.append(&close);
        self.list.append(&row);

        let (w, tt) = (Rc::downgrade(self), t.clone());
        main.connect_clicked(move |_| {
            if let Some(p) = w.upgrade() {
                p.ctl.hide();
                if let Some(c) = &p.control {
                    c.activate(&tt);
                }
            }
        });
        let (w, tt) = (Rc::downgrade(self), t.clone());
        close.connect_clicked(move |_| {
            if let Some(p) = w.upgrade() {
                p.close(&tt);
            }
        });
        (main, close)
    }

    fn close(&self, t: &Toplevel) {
        if let Some(c) = &self.control {
            c.close(t);
        }
    }

    /// Botão X do controle: fecha o programa da linha onde está o cursor.
    pub fn close_focused(&self) {
        let Some((_, id, _)) = self.focused() else { return };
        let t = self.rows.borrow().iter().find(|(t, _, _)| t.id == id).map(|(t, _, _)| t.clone());
        if let Some(t) = t {
            self.close(&t);
        }
    }
}
