//! Bluetooth: ligar/desligar, procurar, parear, conectar e esquecer dispositivos.
//!
//! Fala direto com o BlueZ pelo D-Bus do sistema (sem bluetoothctl). A busca só roda
//! enquanto a aba está aberta. O Orbit registra um agente de pareamento próprio, usado
//! apenas nos pareamentos que ele inicia: códigos de confirmação aparecem num aviso
//! com Confirmar/Cancelar (navegável pelo controle) e códigos para digitar num teclado
//! são mostrados na tela.

use crate::util;
use gtk::prelude::*;
use gtk::{gio, glib, pango};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

const BLUEZ: &str = "org.bluez";
const ADAPTER: &str = "org.bluez.Adapter1";
const DEVICE: &str = "org.bluez.Device1";
const PROPS: &str = "org.freedesktop.DBus.Properties";
const AGENT_PATH: &str = "/io/github/orbit/agent";

const AGENT_XML: &str = r#"
<node>
  <interface name="org.bluez.Agent1">
    <method name="Release"/>
    <method name="RequestPinCode"><arg type="o" direction="in"/><arg type="s" direction="out"/></method>
    <method name="DisplayPinCode"><arg type="o" direction="in"/><arg type="s" direction="in"/></method>
    <method name="RequestPasskey"><arg type="o" direction="in"/><arg type="u" direction="out"/></method>
    <method name="DisplayPasskey"><arg type="o" direction="in"/><arg type="u" direction="in"/><arg type="q" direction="in"/></method>
    <method name="RequestConfirmation"><arg type="o" direction="in"/><arg type="u" direction="in"/></method>
    <method name="RequestAuthorization"><arg type="o" direction="in"/></method>
    <method name="AuthorizeService"><arg type="o" direction="in"/><arg type="s" direction="in"/></method>
    <method name="Cancel"/>
  </interface>
</node>"#;

#[derive(Clone, Default, Debug)]
struct Device {
    address: String,
    name: Option<String>,
    alias: Option<String>,
    icon: Option<String>,
    adapter: String,
    paired: bool,
    connected: bool,
    rssi: Option<i16>,
}

impl Device {
    fn update(&mut self, props: &glib::Variant) {
        let d = glib::VariantDict::new(Some(props));
        let get_s = |k: &str| d.lookup::<String>(k).ok().flatten();
        if let Some(v) = get_s("Address") {
            self.address = v;
        }
        if let Some(v) = get_s("Name") {
            self.name = Some(v);
        }
        if let Some(v) = get_s("Alias") {
            self.alias = Some(v);
        }
        if let Some(v) = get_s("Icon") {
            self.icon = Some(v);
        }
        if let Some(v) = d.lookup_value("Adapter", None).and_then(|v| v.str().map(String::from)) {
            self.adapter = v;
        }
        if let Some(v) = d.lookup::<bool>("Paired").ok().flatten() {
            self.paired = v;
        }
        if let Some(v) = d.lookup::<bool>("Connected").ok().flatten() {
            self.connected = v;
        }
        if let Some(v) = d.lookup::<i16>("RSSI").ok().flatten() {
            self.rssi = Some(v);
        }
    }

    fn label(&self) -> String {
        self.alias
            .clone()
            .or_else(|| self.name.clone())
            .unwrap_or_else(|| self.address.clone())
    }

    /// Tem um nome de verdade (o alias padrão do BlueZ é o endereço com '-').
    fn has_name(&self) -> bool {
        self.name.is_some()
            || self
                .alias
                .as_ref()
                .is_some_and(|a| a.replace('-', ":") != self.address)
    }

    fn icon_name(&self) -> String {
        let base = match self.icon.as_deref() {
            Some("input-gaming") => "input-gaming",
            Some("input-keyboard") => "input-keyboard",
            Some("input-mouse") => "input-mouse",
            Some("input-tablet") => "input-tablet",
            Some("audio-headset") => "audio-headset",
            Some("audio-headphones") => "audio-headphones",
            Some("audio-card") => "audio-speakers",
            Some("phone") => "phone",
            Some("computer") => "computer",
            _ => "bluetooth",
        };
        format!("{base}-symbolic")
    }
}

/// Nome curto do erro D-Bus (ex.: "AlreadyExists" de org.bluez.Error.AlreadyExists).
fn remote_name(e: &glib::Error) -> String {
    gio::DBusError::remote_error(e)
        .map(|s| s.rsplit('.').next().unwrap_or_default().to_string())
        .unwrap_or_default()
}

fn is_rfkill(e: &glib::Error) -> bool {
    remote_name(e) == "Blocked" || e.message().to_lowercase().contains("rfkill")
}

fn err_text(e: &glib::Error) -> String {
    let msg = match remote_name(e).as_str() {
        "AuthenticationFailed" => "falha na autenticação",
        "AuthenticationCanceled" | "AuthenticationRejected" | "Canceled" => "pareamento cancelado",
        "AuthenticationTimeout" => "o dispositivo não confirmou a tempo",
        "ConnectionAttemptFailed" | "PageTimeout" => "não respondeu — coloque-o em modo de pareamento",
        "InProgress" => "já em andamento",
        "NotReady" => "Bluetooth não está pronto",
        _ if is_rfkill(e) => "bloqueado pelo sistema (rfkill)",
        _ => "",
    };
    if !msg.is_empty() {
        return msg.to_string();
    }
    let s = e.message().to_string();
    s.rsplit(": ").next().unwrap_or(&s).to_string()
}

enum Prompt {
    /// Aguardando Confirmar/Cancelar.
    Confirm(gio::DBusMethodInvocation),
    /// Só mostrando um código.
    Show,
}

pub struct BluetoothPage {
    pub root: gtk::ScrolledWindow,
    power: gtk::Button,
    power_sub: gtk::Label,
    banner: gtk::Box,
    banner_text: gtk::Label,
    banner_yes: gtk::Button,
    banner_no: gtk::Button,
    paired_title: gtk::Label,
    paired: gtk::Box,
    nearby_title: gtk::Label,
    nearby: gtk::Box,
    nearby_empty: gtk::Label,

    conn: RefCell<Option<gio::DBusConnection>>,
    adapter: RefCell<Option<String>>,
    powered: Cell<bool>,
    discovering: Cell<bool>,
    we_discover: Cell<bool>,
    devices: RefCell<HashMap<String, Device>>,
    busy: RefCell<HashMap<String, String>>,
    errors: RefCell<HashMap<String, String>>,
    prompt: RefCell<Option<Prompt>>,
    active: Cell<bool>,
    rows: RefCell<Vec<(String, gtk::Button, Option<gtk::Button>)>>,
    sig: RefCell<String>,
    render_pending: Cell<bool>,
    subs: RefCell<Vec<gio::SignalSubscription>>,
}

impl BluetoothPage {
    pub fn new() -> Rc<Self> {
        let page = gtk::Box::new(gtk::Orientation::Vertical, 10);
        page.add_css_class("page");

        let (power, power_sub) = util::tile("bluetooth-active-symbolic", "Bluetooth", "Verificando…");
        page.append(&util::tile_grid(1, &[power.clone()]));

        // Aviso de pareamento (código para confirmar ou digitar).
        let banner = gtk::Box::new(gtk::Orientation::Vertical, 10);
        banner.add_css_class("bt-banner");
        banner.set_visible(false);
        let banner_text = gtk::Label::new(None);
        banner_text.set_wrap(true);
        banner_text.set_xalign(0.0);
        banner_text.add_css_class("tile-title");
        let (banner_yes, _) = util::tile("object-select-symbolic", "Confirmar", "O código é o mesmo");
        let (banner_no, _) = util::tile("window-close-symbolic", "Cancelar", "Não parear");
        let btns = util::tile_grid(2, &[banner_yes.clone(), banner_no.clone()]);
        banner.append(&banner_text);
        banner.append(&btns);
        page.append(&banner);

        let paired_title = util::section("MEUS DISPOSITIVOS");
        let paired = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let nearby_title = util::section("DISPOSITIVOS PRÓXIMOS");
        let nearby = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let nearby_empty = gtk::Label::new(Some("Procurando… coloque o dispositivo em modo de pareamento"));
        nearby_empty.add_css_class("dim");
        nearby_empty.set_xalign(0.0);
        nearby_empty.set_margin_start(4);
        page.append(&paired_title);
        page.append(&paired);
        page.append(&nearby_title);
        page.append(&nearby);
        page.append(&nearby_empty);

        let root = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&page)
            .build();

        let this = Rc::new(Self {
            root,
            power,
            power_sub,
            banner,
            banner_text,
            banner_yes,
            banner_no,
            paired_title,
            paired,
            nearby_title,
            nearby,
            nearby_empty,
            conn: RefCell::default(),
            adapter: RefCell::default(),
            powered: Cell::new(false),
            discovering: Cell::new(false),
            we_discover: Cell::new(false),
            devices: RefCell::default(),
            busy: RefCell::default(),
            errors: RefCell::default(),
            prompt: RefCell::default(),
            active: Cell::new(false),
            rows: RefCell::default(),
            sig: RefCell::default(),
            render_pending: Cell::new(false),
            subs: RefCell::default(),
        });

        let w = Rc::downgrade(&this);
        this.power.connect_clicked(move |_| {
            if let Some(t) = w.upgrade() {
                t.toggle_power();
            }
        });
        let w = Rc::downgrade(&this);
        this.banner_yes.connect_clicked(move |_| {
            if let Some(t) = w.upgrade() {
                t.answer_prompt(true);
            }
        });
        let w = Rc::downgrade(&this);
        this.banner_no.connect_clicked(move |_| {
            if let Some(t) = w.upgrade() {
                t.answer_prompt(false);
            }
        });

        let w = Rc::downgrade(&this);
        glib::spawn_future_local(async move {
            if let Some(t) = w.upgrade() {
                t.init().await;
            }
        });
        this
    }

    // ------------------------------------------------------------ D-Bus

    async fn init(self: Rc<Self>) {
        let conn = match gio::bus_get_future(gio::BusType::System).await {
            Ok(c) => c,
            Err(_) => return self.unavailable(),
        };
        *self.conn.borrow_mut() = Some(conn.clone());

        // Sinais do BlueZ: dispositivos aparecendo/sumindo e mudanças de propriedade.
        let mut subs = Vec::new();
        let w = Rc::downgrade(&self);
        subs.push(conn.subscribe_to_signal(
            Some(BLUEZ),
            Some(PROPS),
            Some("PropertiesChanged"),
            None,
            None,
            gio::DBusSignalFlags::NONE,
            move |s| {
                if let Some(t) = w.upgrade() {
                    t.on_props_changed(s.object_path, s.parameters);
                }
            },
        ));
        let w = Rc::downgrade(&self);
        subs.push(conn.subscribe_to_signal(
            Some(BLUEZ),
            Some("org.freedesktop.DBus.ObjectManager"),
            None,
            None,
            None,
            gio::DBusSignalFlags::NONE,
            move |s| {
                let Some(t) = w.upgrade() else { return };
                match s.signal_name {
                    "InterfacesAdded" => {
                        let path = s.parameters.child_value(0);
                        t.on_object(path.str().unwrap_or_default(), &s.parameters.child_value(1));
                    }
                    "InterfacesRemoved" => {
                        let path = s.parameters.child_value(0).str().unwrap_or_default().to_string();
                        let ifaces: Vec<String> = s.parameters.child_value(1).get().unwrap_or_default();
                        if ifaces.iter().any(|i| i == DEVICE) {
                            t.devices.borrow_mut().remove(&path);
                            t.busy.borrow_mut().remove(&path);
                            t.errors.borrow_mut().remove(&path);
                        }
                        if ifaces.iter().any(|i| i == ADAPTER) && t.adapter.borrow().as_deref() == Some(path.as_str()) {
                            *t.adapter.borrow_mut() = None;
                        }
                        t.schedule_render();
                    }
                    _ => {}
                }
            },
        ));
        *self.subs.borrow_mut() = subs;

        let objs = conn
            .call_future(
                Some(BLUEZ),
                "/",
                "org.freedesktop.DBus.ObjectManager",
                "GetManagedObjects",
                None,
                None,
                gio::DBusCallFlags::NONE,
                5000,
            )
            .await;
        let Ok(objs) = objs else { return self.unavailable() };
        for entry in objs.child_value(0).iter() {
            let path = entry.child_value(0);
            self.on_object(path.str().unwrap_or_default(), &entry.child_value(1));
        }
        self.register_agent(&conn).await;
        self.schedule_render();
        if self.active.get() {
            self.update_discovery();
        }
    }

    fn unavailable(&self) {
        self.power_sub.set_text("Bluetooth indisponível neste computador");
        self.power.set_sensitive(false);
        self.paired_title.set_visible(false);
        self.nearby_title.set_visible(false);
        self.nearby_empty.set_visible(false);
    }

    /// Objeto novo/existente: `ifaces` é a{sa{sv}}.
    fn on_object(self: &Rc<Self>, path: &str, ifaces: &glib::Variant) {
        for ie in ifaces.iter() {
            let name = ie.child_value(0);
            let props = ie.child_value(1);
            match name.str() {
                Some(ADAPTER) => {
                    if self.adapter.borrow().is_none() {
                        *self.adapter.borrow_mut() = Some(path.to_string());
                    }
                    if self.adapter.borrow().as_deref() == Some(path) {
                        self.adapter_props(&props);
                    }
                }
                Some(DEVICE) => {
                    let mut devs = self.devices.borrow_mut();
                    devs.entry(path.to_string()).or_default().update(&props);
                }
                _ => {}
            }
        }
        self.schedule_render();
    }

    fn adapter_props(&self, props: &glib::Variant) {
        let d = glib::VariantDict::new(Some(props));
        if let Some(p) = d.lookup::<bool>("Powered").ok().flatten() {
            self.powered.set(p);
        }
        if let Some(p) = d.lookup::<bool>("Discovering").ok().flatten() {
            self.discovering.set(p);
        }
    }

    fn on_props_changed(self: &Rc<Self>, path: &str, params: &glib::Variant) {
        let iface = params.child_value(0);
        let changed = params.child_value(1);
        match iface.str() {
            Some(ADAPTER) if self.adapter.borrow().as_deref() == Some(path) => {
                let was = self.powered.get();
                self.adapter_props(&changed);
                if was != self.powered.get() {
                    self.update_discovery();
                }
            }
            Some(DEVICE) => {
                if let Some(d) = self.devices.borrow_mut().get_mut(path) {
                    d.update(&changed);
                }
            }
            _ => return,
        }
        self.schedule_render();
    }

    async fn call(
        &self,
        path: &str,
        iface: &str,
        method: &str,
        args: Option<glib::Variant>,
        timeout: i32,
    ) -> Result<glib::Variant, glib::Error> {
        let conn = self.conn.borrow().clone().ok_or_else(|| {
            glib::Error::new(gio::IOErrorEnum::NotConnected, "sem D-Bus")
        })?;
        conn.call_future(Some(BLUEZ), path, iface, method, args.as_ref(), None, gio::DBusCallFlags::NONE, timeout)
            .await
    }

    async fn set_prop(&self, path: &str, iface: &str, name: &str, value: glib::Variant) -> Result<(), glib::Error> {
        self.call(path, PROPS, "Set", Some((iface, name, value).to_variant()), 5000)
            .await
            .map(|_| ())
    }

    // ------------------------------------------------------------ Agente de pareamento

    async fn register_agent(self: &Rc<Self>, conn: &gio::DBusConnection) {
        let Ok(node) = gio::DBusNodeInfo::for_xml(AGENT_XML) else { return };
        let Some(iface) = node.lookup_interface("org.bluez.Agent1") else { return };
        let w = Rc::downgrade(self);
        let reg = conn
            .register_object(AGENT_PATH, &iface)
            .method_call(move |_, _, _, _, method, params, inv| on_agent_call(&w, method, &params, inv))
            .build();
        if reg.is_err() {
            return;
        }
        let Ok(path) = glib::variant::ObjectPath::try_from(AGENT_PATH.to_string()) else { return };
        // DisplayYesNo: mostra códigos e pede confirmação — cobre fones, controles e teclados.
        let _ = self
            .call("/org/bluez", "org.bluez.AgentManager1", "RegisterAgent", Some((path, "DisplayYesNo").to_variant()), 5000)
            .await;
    }

    fn device_label(&self, path: &str) -> String {
        self.devices
            .borrow()
            .get(path)
            .map(|d| d.label())
            .unwrap_or_else(|| "o dispositivo".into())
    }

    fn show_prompt(&self, text: &str, confirm: Option<gio::DBusMethodInvocation>) {
        self.cancel_prompt();
        self.banner_text.set_text(text);
        self.banner.set_visible(true);
        let ask = confirm.is_some();
        self.banner_yes.set_visible(ask);
        self.banner_no.set_visible(ask);
        *self.prompt.borrow_mut() = Some(match confirm {
            Some(inv) => Prompt::Confirm(inv),
            None => Prompt::Show,
        });
        if ask {
            self.banner_yes.grab_focus();
        }
    }

    /// Fecha o aviso; se havia uma pergunta pendente, responde "não".
    fn cancel_prompt(&self) {
        if let Some(Prompt::Confirm(inv)) = self.prompt.borrow_mut().take() {
            inv.return_dbus_error("org.bluez.Error.Rejected", "Cancelado");
        }
        self.banner.set_visible(false);
    }

    fn answer_prompt(&self, yes: bool) {
        if let Some(Prompt::Confirm(inv)) = self.prompt.borrow_mut().take() {
            if yes {
                inv.return_value(None);
            } else {
                inv.return_dbus_error("org.bluez.Error.Rejected", "Recusado");
            }
        }
        self.banner.set_visible(false);
        if let Some((_, b, _)) = self.rows.borrow().first() {
            b.grab_focus();
        }
    }

    // ------------------------------------------------------------ Ações

    fn toggle_power(self: &Rc<Self>) {
        let Some(adapter) = self.adapter.borrow().clone() else { return };
        let this = self.clone();
        let on = !self.powered.get();
        glib::spawn_future_local(async move {
            let mut r = this.set_prop(&adapter, ADAPTER, "Powered", on.to_variant()).await;
            if on && r.as_ref().is_err_and(is_rfkill) {
                // Bloqueio por software: o usuário costuma poder desfazer sem root.
                let _ = gio::spawn_blocking(|| util::run("rfkill", &["unblock", "bluetooth"])).await;
                glib::timeout_future(std::time::Duration::from_millis(400)).await;
                r = this.set_prop(&adapter, ADAPTER, "Powered", on.to_variant()).await;
            }
            if let Err(e) = r {
                this.power_sub.set_text(&format!("Não foi possível: {}", err_text(&e)));
            }
        });
    }

    /// Liga a busca enquanto a aba está visível; desliga ao sair.
    pub fn set_active(self: &Rc<Self>, on: bool) {
        if self.active.replace(on) == on {
            return;
        }
        if !on {
            self.cancel_prompt();
        }
        self.update_discovery();
        self.schedule_render();
    }

    fn update_discovery(self: &Rc<Self>) {
        let Some(adapter) = self.adapter.borrow().clone() else { return };
        let want = self.active.get() && self.powered.get();
        if want == self.we_discover.get() {
            return;
        }
        self.we_discover.set(want);
        let this = self.clone();
        glib::spawn_future_local(async move {
            let method = if want { "StartDiscovery" } else { "StopDiscovery" };
            let _ = this.call(&adapter, ADAPTER, method, None, 5000).await;
        });
    }

    fn activate_device(self: &Rc<Self>, path: String) {
        if self.busy.borrow().contains_key(&path) {
            return;
        }
        let Some(dev) = self.devices.borrow().get(&path).cloned() else { return };
        self.errors.borrow_mut().remove(&path);
        let this = self.clone();
        glib::spawn_future_local(async move {
            let set_busy = |s: &str| {
                this.busy.borrow_mut().insert(path.clone(), s.to_string());
                this.schedule_render();
            };
            let result = if dev.connected {
                set_busy("Desconectando…");
                this.call(&path, DEVICE, "Disconnect", None, 20_000).await.map(|_| ())
            } else {
                let mut r = Ok(());
                if !dev.paired {
                    set_busy("Pareando…");
                    // A busca atrapalha o pareamento em alguns adaptadores.
                    if let Some(a) = this.adapter.borrow().clone() {
                        if this.we_discover.get() {
                            let _ = this.call(&a, ADAPTER, "StopDiscovery", None, 3000).await;
                            this.we_discover.set(false);
                        }
                    }
                    r = this.call(&path, DEVICE, "Pair", None, 90_000).await.map(|_| ());
                    if r.as_ref().is_err_and(|e| remote_name(e) == "AlreadyExists") {
                        r = Ok(());
                    }
                    if r.is_ok() {
                        let _ = this.set_prop(&path, DEVICE, "Trusted", true.to_variant()).await;
                    }
                }
                if r.is_ok() && !this.devices.borrow().get(&path).is_some_and(|d| d.connected) {
                    set_busy("Conectando…");
                    r = this.call(&path, DEVICE, "Connect", None, 30_000).await.map(|_| ());
                    // Controles costumam conectar sozinhos logo após parear.
                    if r.is_err() && this.devices.borrow().get(&path).is_some_and(|d| d.connected) {
                        r = Ok(());
                    }
                }
                r
            };
            this.busy.borrow_mut().remove(&path);
            if let Err(e) = result {
                this.errors.borrow_mut().insert(path.clone(), err_text(&e));
            }
            // Fecha o aviso de código, se ainda estiver na tela.
            if matches!(*this.prompt.borrow(), Some(Prompt::Show)) {
                this.prompt.borrow_mut().take();
                this.banner.set_visible(false);
            }
            this.update_discovery();
            this.schedule_render();
        });
    }

    fn forget(self: &Rc<Self>, path: String) {
        let Some(adapter) = self.devices.borrow().get(&path).map(|d| d.adapter.clone()) else { return };
        let Ok(op) = glib::variant::ObjectPath::try_from(path.clone()) else { return };
        let this = self.clone();
        this.busy.borrow_mut().insert(path.clone(), "Esquecendo…".into());
        this.schedule_render();
        glib::spawn_future_local(async move {
            let r = this.call(&adapter, ADAPTER, "RemoveDevice", Some((op,).to_variant()), 10_000).await;
            this.busy.borrow_mut().remove(&path);
            if let Err(e) = r {
                this.errors.borrow_mut().insert(path, err_text(&e));
            }
            this.schedule_render();
        });
    }

    /// Botão X do controle: esquece o dispositivo pareado onde está o cursor.
    pub fn forget_focused(self: &Rc<Self>) {
        let Some(f) = self.root.root().and_then(|r| r.focus()) else { return };
        let path = self.rows.borrow().iter().find_map(|(p, main, forget)| {
            let hit = |b: &gtk::Button| f == *b.upcast_ref::<gtk::Widget>() || f.is_ancestor(b);
            (forget.is_some() && (hit(main) || forget.as_ref().is_some_and(hit))).then(|| p.clone())
        });
        if let Some(p) = path {
            self.forget(p);
        }
    }

    // ------------------------------------------------------------ Interface

    fn schedule_render(self: &Rc<Self>) {
        if self.render_pending.replace(true) {
            return;
        }
        let w = Rc::downgrade(self);
        glib::timeout_add_local_once(std::time::Duration::from_millis(80), move || {
            if let Some(t) = w.upgrade() {
                t.render_pending.set(false);
                t.render();
            }
        });
    }

    fn render(self: &Rc<Self>) {
        let has_adapter = self.adapter.borrow().is_some();
        let powered = self.powered.get();
        self.power.set_sensitive(has_adapter);
        self.power_sub.set_text(match (has_adapter, powered) {
            (false, _) => "Nenhum adaptador Bluetooth encontrado",
            (true, true) => "Ligado — pressione para desligar",
            (true, false) => "Desligado — pressione para ligar",
        });
        if powered {
            self.power.add_css_class("selected");
            self.power.remove_css_class("off");
        } else {
            self.power.remove_css_class("selected");
            self.power.add_css_class("off");
        }

        let adapter = self.adapter.borrow().clone().unwrap_or_default();
        let devs = self.devices.borrow();
        let busy = self.busy.borrow();
        let errors = self.errors.borrow();
        let mut paired: Vec<(&String, &Device)> =
            devs.iter().filter(|(_, d)| d.adapter == adapter && d.paired).collect();
        paired.sort_by_key(|(_, d)| (!d.connected, d.label().to_lowercase()));
        let mut nearby: Vec<(&String, &Device)> = devs
            .iter()
            .filter(|(p, d)| d.adapter == adapter && !d.paired && (d.has_name() || busy.contains_key(*p)))
            .collect();
        nearby.sort_by_key(|(_, d)| (std::cmp::Reverse(d.rssi.unwrap_or(i16::MIN)), d.label().to_lowercase()));
        nearby.truncate(24);

        let status = |p: &str, d: &Device| -> String {
            if let Some(b) = busy.get(p) {
                return b.clone();
            }
            if let Some(e) = errors.get(p) {
                return format!("Falhou: {e}");
            }
            match (d.paired, d.connected) {
                (_, true) => "Conectado — pressione para desconectar".into(),
                (true, false) => "Pareado — pressione para conectar".into(),
                (false, _) => "Pressione para parear".into(),
            }
        };
        let sig = format!(
            "{powered}|{}|{}",
            paired.iter().map(|(p, d)| format!("{p}{}{}", d.label(), status(p, d))).collect::<String>(),
            nearby.iter().map(|(p, d)| format!("{p}{}{}", d.label(), status(p, d))).collect::<String>(),
        );
        if *self.sig.borrow() == sig {
            return;
        }
        *self.sig.borrow_mut() = sig;

        // Lembra onde estava o cursor (linha e botão).
        let focus = self.root.root().and_then(|r| r.focus());
        let prev = focus.as_ref().and_then(|f| {
            self.rows.borrow().iter().enumerate().find_map(|(i, (p, main, forget))| {
                if f == main.upcast_ref::<gtk::Widget>() || f.is_ancestor(main) {
                    Some((i, p.clone(), false))
                } else if forget.as_ref().is_some_and(|b| f == b.upcast_ref::<gtk::Widget>()) {
                    Some((i, p.clone(), true))
                } else {
                    None
                }
            })
        });

        util::clear_box(&self.paired);
        util::clear_box(&self.nearby);
        let mut rows = Vec::new();
        for (p, d) in &paired {
            let (main, forget) = self.row(p, d, &status(p, d), true);
            rows.push(((*p).clone(), main, forget));
        }
        for (p, d) in &nearby {
            let (main, forget) = self.row(p, d, &status(p, d), false);
            rows.push(((*p).clone(), main, forget));
        }
        self.paired_title.set_visible(powered && !paired.is_empty());
        self.paired.set_visible(powered);
        self.nearby_title.set_visible(powered);
        self.nearby.set_visible(powered);
        self.nearby_empty.set_visible(powered && nearby.is_empty());
        self.nearby_empty.set_text(if self.active.get() {
            "Procurando… coloque o dispositivo em modo de pareamento"
        } else {
            "Nenhum dispositivo encontrado"
        });

        if let Some((idx, path, on_forget)) = prev {
            let target = rows
                .iter()
                .find(|(p, _, _)| *p == path)
                .map(|(_, m, f)| if on_forget { f.clone().unwrap_or(m.clone()) } else { m.clone() })
                .or_else(|| rows.get(idx.min(rows.len().saturating_sub(1))).map(|(_, m, _)| m.clone()));
            match target {
                Some(b) => {
                    b.grab_focus();
                }
                None => {
                    self.power.grab_focus();
                }
            }
        }
        drop((devs, busy, errors));
        *self.rows.borrow_mut() = rows;
    }

    fn row(self: &Rc<Self>, path: &str, d: &Device, status: &str, paired: bool) -> (gtk::Button, Option<gtk::Button>) {
        let main = gtk::Button::new();
        main.add_css_class("tile");
        main.set_hexpand(true);
        if d.connected {
            main.add_css_class("selected");
        }
        if status.starts_with("Falhou") {
            main.add_css_class("off");
        }
        let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 14);
        let img = gtk::Image::from_icon_name(&d.icon_name());
        img.set_pixel_size(26);
        img.add_css_class("tile-icon");
        let col = gtk::Box::new(gtk::Orientation::Vertical, 2);
        col.set_valign(gtk::Align::Center);
        col.set_hexpand(true);
        let t = gtk::Label::new(Some(&d.label()));
        t.set_xalign(0.0);
        t.set_ellipsize(pango::EllipsizeMode::End);
        t.set_max_width_chars(1);
        t.add_css_class("tile-title");
        let s = gtk::Label::new(Some(status));
        s.set_xalign(0.0);
        s.set_ellipsize(pango::EllipsizeMode::End);
        s.set_max_width_chars(1);
        s.add_css_class("tile-sub");
        col.append(&t);
        col.append(&s);
        hbox.append(&img);
        hbox.append(&col);
        if d.connected {
            let check = gtk::Image::from_icon_name("object-select-symbolic");
            check.add_css_class("check");
            hbox.append(&check);
        }
        main.set_child(Some(&hbox));

        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        row.append(&main);
        let w = Rc::downgrade(self);
        let p = path.to_string();
        main.connect_clicked(move |_| {
            if let Some(t) = w.upgrade() {
                t.activate_device(p.clone());
            }
        });

        let forget = paired.then(|| {
            let b = gtk::Button::from_icon_name("user-trash-symbolic");
            b.add_css_class("tile");
            b.add_css_class("close-win");
            b.set_tooltip_text(Some("Esquecer"));
            let w = Rc::downgrade(self);
            let p = path.to_string();
            b.connect_clicked(move |_| {
                if let Some(t) = w.upgrade() {
                    t.forget(p.clone());
                }
            });
            row.append(&b);
            b
        });
        if paired {
            self.paired.append(&row);
        } else {
            self.nearby.append(&row);
        }
        (main, forget)
    }
}

fn on_agent_call(w: &Weak<BluetoothPage>, method: &str, params: &glib::Variant, inv: gio::DBusMethodInvocation) {
    let Some(t) = w.upgrade() else {
        inv.return_dbus_error("org.bluez.Error.Rejected", "Orbit encerrado");
        return;
    };
    let dev = || params.child_value(0).str().unwrap_or_default().to_string();
    match method {
        "RequestConfirmation" => {
            let code: u32 = params.child_value(1).get().unwrap_or(0);
            let text = format!(
                "Confirme que o código {code:06} aparece em {}",
                t.device_label(&dev())
            );
            t.show_prompt(&text, Some(inv));
        }
        "DisplayPasskey" => {
            let code: u32 = params.child_value(1).get().unwrap_or(0);
            t.show_prompt(&format!("Digite {code:06} em {} e aperte Enter", t.device_label(&dev())), None);
            inv.return_value(None);
        }
        "DisplayPinCode" => {
            let pin: String = params.child_value(1).get().unwrap_or_default();
            t.show_prompt(&format!("Digite {pin} em {} e aperte Enter", t.device_label(&dev())), None);
            inv.return_value(None);
        }
        // Dispositivos antigos com PIN fixo usam quase sempre 0000.
        "RequestPinCode" => inv.return_value(Some(&("0000",).to_variant())),
        "RequestPasskey" => inv.return_value(Some(&(0u32,).to_variant())),
        // Só recebemos estes nos pareamentos iniciados pelo próprio Orbit.
        "RequestAuthorization" | "AuthorizeService" => inv.return_value(None),
        "Cancel" | "Release" => {
            if let Some(Prompt::Confirm(i)) = t.prompt.borrow_mut().take() {
                i.return_dbus_error("org.bluez.Error.Canceled", "Cancelado");
            }
            t.banner.set_visible(false);
            inv.return_value(None);
        }
        _ => inv.return_dbus_error("org.bluez.Error.Rejected", "Método desconhecido"),
    }
}
