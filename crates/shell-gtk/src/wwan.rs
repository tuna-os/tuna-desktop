//! GNOME's mobile broadband (the Network panel's WWAN section): the
//! modem devices NetworkManager lists, each enriched with its
//! ModemManager modem (signal quality, operator, SIM lock). The toggle
//! is NetworkManager's `WwanEnabled` master switch; a modem row opens
//! Mobile Network Settings, where the SIM PIN, APN and roaming controls
//! live (GNOME Settings owns those dialogs, as with 802.1X Wi-Fi).
//!
//! Absent daemons hide the tile: no modem device means no WWAN, and a
//! missing ModemManager only blanks the signal and operator lines.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gio::prelude::*;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::services::Tile;

const NM: &str = "org.freedesktop.NetworkManager";
const NM_PATH: &str = "/org/freedesktop/NetworkManager";
const PROPS: &str = "org.freedesktop.DBus.Properties";
const DEVICE: &str = "org.freedesktop.NetworkManager.Device";
const NM_MODEM: &str = "org.freedesktop.NetworkManager.Device.Modem";
const CONNECTION: &str = "org.freedesktop.NetworkManager.Settings.Connection";
/// `NM_DEVICE_TYPE_MODEM`.
pub const NM_DEVICE_MODEM: u32 = 8;
/// `NM_DEVICE_STATE_ACTIVATED`.
const ACTIVATED: u32 = 100;

const MM: &str = "org.freedesktop.ModemManager1";
const MM_MODEM_IFACE: &str = "org.freedesktop.ModemManager1.Modem";
/// 3GPP registration: `OperatorName`/`OperatorCode` live here, not on `Modem`.
const MM_MODEM_3GPP_IFACE: &str = "org.freedesktop.ModemManager1.Modem.Modem3gpp";
/// `Device.Udi` values that are ModemManager modem paths start here.
const MM_MODEM_PREFIX: &str = "/org/freedesktop/ModemManager1/";

/// ModemManager states (`MMModemState` in `include/ModemManager-enums.h`).
pub const MM_STATE_FAILED: i32 = -1;
pub const MM_STATE_UNKNOWN: i32 = 0;
pub const MM_STATE_INITIALIZING: i32 = 1;
pub const MM_STATE_LOCKED: i32 = 2;
pub const MM_STATE_DISABLED: i32 = 3;
pub const MM_STATE_DISABLING: i32 = 4;
pub const MM_STATE_ENABLING: i32 = 5;
pub const MM_STATE_ENABLED: i32 = 6;
pub const MM_STATE_SEARCHING: i32 = 7;
pub const MM_STATE_REGISTERED: i32 = 8;
pub const MM_STATE_DISCONNECTING: i32 = 9;
pub const MM_STATE_CONNECTING: i32 = 10;
pub const MM_STATE_CONNECTED: i32 = 11;

/// SIM locks that ask for a code (`MMModemLock`).
pub const MM_LOCK_NONE: u32 = 1;
pub const MM_LOCK_SIM_PIN: u32 = 2;
pub const MM_LOCK_SIM_PUK: u32 = 4;

/// One modem: NetworkManager's device row plus its ModemManager modem.
#[derive(Clone, Debug, Default)]
pub struct WwanDevice {
    pub path: String,
    pub interface: String,
    /// NetworkManager device state.
    pub state: u32,
    /// ModemManager modem path from `Device.Udi` (`None` when absent).
    pub modem: Option<String>,
    /// Signal percent and whether the reading is recent.
    pub quality: Option<(u32, bool)>,
    /// `Modem3gpp.OperatorName` (empty until the 3GPP read lands).
    pub operator: String,
    pub modem_state: i32,
    /// `MMModemLock` (`UnlockRequired`).
    pub sim_lock: u32,
    /// The active connection's APN, when its settings say so.
    pub apn: Option<String>,
    /// `Device.Modem` identity: `DeviceId` and `OperatorCode` (MCCMNC).
    pub device_id: String,
    pub operator_code: String,
    /// `Device.Modem` capabilities (generic access-technology families).
    pub modem_caps: u32,
    pub current_caps: u32,
    /// `Device.Modem.Apn` fallback, used only when the gsm settings
    /// carry no APN (combined at reload finish to avoid a read race).
    pub nm_apn: Option<String>,
}

impl WwanDevice {
    pub fn connected(&self) -> bool {
        self.state == ACTIVATED
    }

    /// The row's second line: the operator while the NetworkManager
    /// device is up, its ModemManager modem is present and registered,
    /// else the modem state (a SIM lock wins over both).
    pub fn subtitle(&self) -> String {
        if let Some(lock) = sim_lock_label(self.sim_lock) {
            return lock.to_owned();
        }
        if self.modem.is_some()
            && self.connected()
            && modem_registered(self.modem_state)
            && !self.operator.is_empty()
        {
            return self.operator.clone();
        }
        modem_state_label(self.modem_state).to_owned()
    }

    pub fn icon(&self) -> &'static str {
        match self.quality {
            Some((percent, recent)) => cellular_signal_icon(percent, recent),
            None => cellular_signal_icon(0, false),
        }
    }
}

/// GNOME's modem-state line, from the `MMModemState` docs.
pub fn modem_state_label(state: i32) -> &'static str {
    match state {
        MM_STATE_FAILED => "Modem failed",
        MM_STATE_INITIALIZING => "Initializing",
        MM_STATE_LOCKED => "Locked",
        MM_STATE_DISABLED | MM_STATE_DISABLING => "Disabled",
        MM_STATE_ENABLING => "Enabling",
        MM_STATE_ENABLED => "Enabled",
        MM_STATE_SEARCHING => "Searching",
        MM_STATE_REGISTERED => "Registered",
        MM_STATE_DISCONNECTING => "Disconnecting",
        MM_STATE_CONNECTING => "Connecting",
        MM_STATE_CONNECTED => "Connected",
        _ => "Unknown",
    }
}

/// Whether the modem can carry data (registered or beyond).
pub fn modem_registered(state: i32) -> bool {
    matches!(
        state,
        MM_STATE_REGISTERED | MM_STATE_DISCONNECTING | MM_STATE_CONNECTING | MM_STATE_CONNECTED
    )
}

/// The SIM-lock line, if the modem needs a code. `UnlockRequired` is
/// `MM_MODEM_LOCK_NONE` (1) when unlocked and 0 when unknown.
pub fn sim_lock_label(lock: u32) -> Option<&'static str> {
    match lock {
        MM_LOCK_SIM_PIN => Some("SIM PIN required"),
        MM_LOCK_SIM_PUK => Some("SIM PUK required"),
        0 | MM_LOCK_NONE => None,
        _ => Some("SIM locked"),
    }
}

/// GNOME's cellular signal icon: the wireless thresholds on the
/// modem's percent, blank when the reading is stale.
pub fn cellular_signal_icon(percent: u32, recent: bool) -> &'static str {
    if !recent {
        return "network-cellular-signal-none-symbolic";
    }
    match percent {
        0..=19 => "network-cellular-signal-none-symbolic",
        20..=39 => "network-cellular-signal-weak-symbolic",
        40..=49 => "network-cellular-signal-ok-symbolic",
        50..=79 => "network-cellular-signal-good-symbolic",
        _ => "network-cellular-signal-excellent-symbolic",
    }
}

/// The tile: visible with any modem, checked with `WwanEnabled`, named
/// for the connected modem's operator.
pub struct WwanToggle {
    pub visible: bool,
    pub checked: bool,
    pub subtitle: Option<String>,
    pub icon: &'static str,
}

pub fn wwan_toggle(devices: &[WwanDevice], wwan_enabled: bool) -> WwanToggle {
    let connected = devices.iter().find(|d| d.connected());
    let first = connected.or_else(|| devices.first());
    WwanToggle {
        visible: !devices.is_empty(),
        checked: wwan_enabled,
        subtitle: first.map(|d| d.subtitle()),
        icon: first
            .map(|d| d.icon())
            .unwrap_or("network-cellular-signal-none-symbolic"),
    }
}

/// The "gsm" section's APN in a connection's settings dict.
pub fn gsm_apn<'a>(
    mut sections: impl Iterator<Item = (&'a str, glib::VariantDict)>,
) -> Option<String> {
    let (_, gsm) = sections.find(|(name, _)| *name == "gsm")?;
    gsm.lookup::<String>("apn")
        .ok()
        .flatten()
        .filter(|apn| !apn.is_empty())
}

/// The ModemManager modem behind a NetworkManager modem device: its
/// base `Device.Udi` string, which NetworkManager sets to the modem's
/// D-Bus path (`nm_modem_get_path`). Anything else (a sysfs path, an
/// empty string) means no modem.
pub fn modem_path_from_udi(udi: &str) -> Option<String> {
    if udi.starts_with(MM_MODEM_PREFIX) && udi.len() > MM_MODEM_PREFIX.len() {
        Some(udi.to_owned())
    } else {
        None
    }
}

/// `Device.Modem`'s real properties (`ModemCapabilities`,
/// `CurrentCapabilities`, `DeviceId`, `OperatorCode`, `Apn`); there is
/// no `Modem` object path on this interface.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct NmModemProps {
    pub modem_caps: u32,
    pub current_caps: u32,
    pub device_id: String,
    pub operator_code: String,
    pub apn: Option<String>,
}

pub fn parse_nm_modem(props: &glib::VariantDict) -> NmModemProps {
    let u = |k: &str| {
        props
            .lookup_value(k, None)
            .and_then(|v| v.get::<u32>())
            .unwrap_or(0)
    };
    let s = |k: &str| {
        props
            .lookup_value(k, None)
            .and_then(|v| v.get::<String>())
            .unwrap_or_default()
    };
    NmModemProps {
        modem_caps: u("ModemCapabilities"),
        current_caps: u("CurrentCapabilities"),
        device_id: s("DeviceId"),
        operator_code: s("OperatorCode"),
        apn: props
            .lookup_value("Apn", None)
            .and_then(|v| v.get::<String>())
            .filter(|a| !a.is_empty()),
    }
}

/// `Modem3gpp.OperatorName` (the registered operator's name); empty
/// while unregistered. Deliberately separate from the base `Modem`
/// read, where no such property exists.
pub fn operator_from_3gpp(props: &glib::VariantDict) -> String {
    props
        .lookup_value("OperatorName", None)
        .and_then(|v| v.get::<String>())
        .unwrap_or_default()
}

/// The base `Modem` read: state, signal quality and SIM lock. An
/// `OperatorName` on this interface would be an invented shape and is
/// ignored here (it is read from `Modem3gpp` instead).
pub fn parse_mm_modem(props: &glib::VariantDict) -> (i32, Option<(u32, bool)>, u32) {
    let state = props
        .lookup_value("State", None)
        .and_then(|v| v.get::<i32>())
        .unwrap_or(MM_STATE_UNKNOWN);
    let quality = props
        .lookup_value("SignalQuality", None)
        .and_then(|v| v.get::<(u32, bool)>());
    let lock = props
        .lookup_value("UnlockRequired", None)
        .and_then(|v| v.get::<u32>())
        .unwrap_or(0);
    (state, quality, lock)
}

/// The row's APN: the gsm settings win, `Device.Modem.Apn` fills in.
pub fn combine_apn(gsm: Option<String>, nm: Option<String>) -> Option<String> {
    gsm.or(nm)
}

/// The WWAN tile's menu and the state behind it.
pub struct WwanMenu {
    list: gtk::Box,
    tile: RefCell<Option<Tile>>,
    conn: RefCell<Option<gio::DBusConnection>>,
    devices: RefCell<Vec<WwanDevice>>,
    enabled: Cell<bool>,
    pending: Cell<bool>,
    /// Bumped on every reload, so a slower earlier one cannot win.
    generation: Cell<u64>,
}

impl WwanMenu {
    /// The menu's rows go into `list`.
    pub fn new(list: gtk::Box) -> Rc<Self> {
        Rc::new(Self {
            list,
            tile: RefCell::default(),
            conn: RefCell::default(),
            devices: RefCell::default(),
            enabled: Cell::new(false),
            pending: Cell::new(false),
            generation: Cell::new(0),
        })
    }

    /// The toggle that follows the modems; a click flips
    /// NetworkManager's `WwanEnabled`, which enables every modem (and
    /// disabling it powers them down).
    pub fn set_tile(self: &Rc<Self>, tile: Tile) {
        let this = Rc::downgrade(self);
        tile.on_user_toggle(move |on| {
            let Some(menu) = this.upgrade() else { return };
            let Some(conn) = menu.conn.borrow().clone() else {
                return;
            };
            conn.call(
                Some(NM),
                NM_PATH,
                PROPS,
                "Set",
                Some(&(NM, "WwanEnabled", on.to_variant()).to_variant()),
                None,
                gio::DBusCallFlags::NONE,
                5000,
                None::<&gio::Cancellable>,
                |_| {},
            );
            // The tile shows NetworkManager's state, not the click.
            menu.sync_tile();
        });
        *self.tile.borrow_mut() = Some(tile);
        self.sync_tile();
    }

    /// `WwanEnabled`, as the tile shows it.
    pub fn set_enabled(self: &Rc<Self>, on: bool) {
        self.enabled.set(on);
        self.sync_tile();
    }

    /// NetworkManager on `conn`: read it now and on every change.
    pub fn attach(self: &Rc<Self>, conn: &gio::DBusConnection) {
        if self.conn.replace(Some(conn.clone())).is_none() {
            for (iface, member) in [
                (PROPS, "PropertiesChanged"),
                (NM, "DeviceAdded"),
                (NM, "DeviceRemoved"),
            ] {
                let weak = Rc::downgrade(self);
                let sub = conn.subscribe_to_signal(
                    Some(NM),
                    Some(iface),
                    Some(member),
                    None,
                    None,
                    gio::DBusSignalFlags::NONE,
                    move |_| {
                        if let Some(menu) = weak.upgrade() {
                            menu.schedule_reload();
                        }
                    },
                );
                // The subscriptions live as long as the shell.
                std::mem::forget(sub);
            }
            // ModemManager coming or going re-enriches the rows.
            let weak = Rc::downgrade(self);
            crate::services::watch_name(conn, MM, move |_| {
                if let Some(menu) = weak.upgrade() {
                    menu.schedule_reload();
                }
            });
        }
        self.schedule_reload();
    }

    /// NetworkManager went away: no modems.
    pub fn detach(self: &Rc<Self>) {
        self.generation.set(self.generation.get() + 1);
        self.devices.borrow_mut().clear();
        self.render();
    }

    /// Re-read the modems now (the menu calls this when it opens, so
    /// a freshly appeared modem lists without waiting for a signal).
    pub fn refresh(self: &Rc<Self>) {
        self.schedule_reload();
    }

    fn schedule_reload(self: &Rc<Self>) {
        if self.pending.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(std::time::Duration::from_millis(250), move || {
            if let Some(menu) = weak.upgrade() {
                menu.pending.set(false);
                menu.reload();
            }
        });
    }

    /// Read the modem devices and their ModemManager modems, then
    /// rebuild.
    fn reload(self: &Rc<Self>) {
        let Some(conn) = self.conn.borrow().clone() else {
            return;
        };
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        let weak = Rc::downgrade(self);
        let conn2 = conn.clone();
        conn.call(
            Some(NM),
            NM_PATH,
            NM,
            "GetDevices",
            None,
            glib::VariantTy::new("(ao)").ok(),
            gio::DBusCallFlags::NONE,
            5000,
            None::<&gio::Cancellable>,
            move |reply| {
                let paths: Vec<String> = reply
                    .ok()
                    .and_then(|v| v.child_value(0).get::<Vec<glib::variant::ObjectPath>>())
                    .map(|p| p.into_iter().map(|p| p.as_str().to_owned()).collect())
                    .unwrap_or_default();
                let found: Rc<RefCell<Vec<WwanDevice>>> = Rc::default();
                let done = Countdown::new(paths.len() + 1, {
                    let (weak, found) = (weak.clone(), found.clone());
                    move || {
                        let Some(menu) = weak.upgrade() else { return };
                        if menu.generation.get() != generation {
                            return;
                        }
                        let mut devices = found.take();
                        devices.sort_by(|a, b| a.path.cmp(&b.path));
                        *menu.devices.borrow_mut() = devices;
                        menu.render();
                    }
                });
                for path in paths {
                    read_device(&conn2, path, found.clone(), done.clone());
                }
                done.tick();
            },
        );
    }

    fn render(self: &Rc<Self>) {
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        for dev in self.devices.borrow().clone() {
            self.list.append(&self.row(&dev));
        }
        self.sync_tile();
    }

    /// One modem row: its signal icon, operator (or state) and SIM/APN
    /// line. A click opens Mobile Network Settings, where the PIN,
    /// APN and roaming controls live.
    fn row(self: &Rc<Self>, dev: &WwanDevice) -> gtk::Button {
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        line.append(&gtk::Image::from_icon_name(dev.icon()));
        let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let name = if dev.operator.is_empty() {
            dev.interface.clone()
        } else {
            dev.operator.clone()
        };
        let label = gtk::Label::new(Some(&name));
        label.set_xalign(0.0);
        label.set_hexpand(true);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        text.append(&label);
        let mut detail = dev.subtitle();
        if let Some(apn) = dev.apn.as_ref().filter(|a| !a.is_empty()) {
            detail = format!("{detail} — APN {apn}");
        }
        let subtitle = gtk::Label::new(Some(&detail));
        subtitle.set_xalign(0.0);
        subtitle.add_css_class("device-subtitle");
        text.append(&subtitle);
        line.append(&text);
        let button = gtk::Button::builder().child(&line).build();
        button.add_css_class("qs-menu-item");
        button.add_css_class("nm-wwan-item");
        button.update_property(&[gtk::accessible::Property::Label(&format!(
            "{name}, {detail}"
        ))]);
        button.connect_clicked(|_| {
            crate::open_settings_panel("wwan");
        });
        button
    }

    fn sync_tile(&self) {
        let Some(tile) = self.tile.borrow().clone() else {
            return;
        };
        let t = wwan_toggle(&self.devices.borrow(), self.enabled.get());
        tile.present(t.visible);
        tile.show_state(t.checked, t.subtitle.as_deref());
        tile.icon.set_icon_name(Some(t.icon));
    }
}

/// Read one device; a modem one joins `found` with its ModemManager
/// modem and active APN.
fn read_device(
    conn: &gio::DBusConnection,
    path: String,
    found: Rc<RefCell<Vec<WwanDevice>>>,
    done: Rc<Countdown>,
) {
    let conn2 = conn.clone();
    get_all(conn, NM, &path.clone(), DEVICE, move |props| {
        let Some(props) = props else {
            done.tick();
            return;
        };
        let u = |k: &str| {
            props
                .lookup_value(k, None)
                .and_then(|v| v.get::<u32>())
                .unwrap_or(0)
        };
        if u("DeviceType") != NM_DEVICE_MODEM {
            done.tick();
            return;
        }
        // The MM modem path is the base `Device.Udi` string, not a
        // `Device.Modem` object path (that interface has no `Modem`).
        let udi = props
            .lookup_value("Udi", None)
            .and_then(|v| v.get::<String>())
            .unwrap_or_default();
        let modem = modem_path_from_udi(&udi);
        let device = Rc::new(RefCell::new(WwanDevice {
            path,
            interface: props
                .lookup_value("Interface", None)
                .and_then(|v| v.get::<String>())
                .unwrap_or_default(),
            state: u("State"),
            modem: modem.clone(),
            ..WwanDevice::default()
        }));
        let finish = Countdown::new(4, {
            let device = device.clone();
            move || {
                let mut dev = device.take();
                dev.apn = combine_apn(dev.apn.take(), dev.nm_apn.take());
                found.borrow_mut().push(dev);
                done.tick();
            }
        });
        // The modem's live state: base `Modem` first, then `Modem3gpp`
        // for the operator name (one tick for the chain).
        match modem {
            Some(modem) => {
                let (device2, finish2) = (device.clone(), finish.clone());
                let conn3 = conn2.clone();
                let modem2 = modem.clone();
                get_all(&conn2, MM, &modem, MM_MODEM_IFACE, move |props| {
                    if let Some(props) = props {
                        let (state, quality, lock) = parse_mm_modem(&props);
                        let mut dev = device2.borrow_mut();
                        dev.modem_state = state;
                        dev.quality = quality;
                        dev.sim_lock = lock;
                    }
                    let (device2, finish2) = (device2.clone(), finish2.clone());
                    get_all(&conn3, MM, &modem2, MM_MODEM_3GPP_IFACE, move |props| {
                        if let Some(props) = props {
                            device2.borrow_mut().operator = operator_from_3gpp(&props);
                        }
                        finish2.tick();
                    });
                });
            }
            None => finish.tick(),
        }
        // `Device.Modem` identity and APN fallback (real properties;
        // an invented `Modem` path here would be ignored).
        {
            let (device_nm, finish_nm) = (device.clone(), finish.clone());
            let nm_path = device.borrow().path.clone();
            get_all(&conn2, NM, &nm_path, NM_MODEM, move |props| {
                if let Some(props) = props {
                    let nm = parse_nm_modem(&props);
                    let mut dev = device_nm.borrow_mut();
                    dev.device_id = nm.device_id;
                    dev.operator_code = nm.operator_code;
                    dev.modem_caps = nm.modem_caps;
                    dev.current_caps = nm.current_caps;
                    dev.nm_apn = nm.apn;
                }
                finish_nm.tick();
            });
        }
        // The active connection's APN, when there is one.
        match object_path(&props, "ActiveConnection") {
            Some(active) => {
                let (device, finish) = (device.clone(), finish.clone());
                let conn4 = conn2.clone();
                get_all(
                    &conn2,
                    NM,
                    &active.clone(),
                    "org.freedesktop.NetworkManager.Connection.Active",
                    move |props| {
                        let profile = props.as_ref().and_then(|p| object_path(p, "Connection"));
                        match profile {
                            Some(profile) => conn4.call(
                                Some(NM),
                                &profile,
                                CONNECTION,
                                "GetSettings",
                                None,
                                glib::VariantTy::new("(a{sa{sv}})").ok(),
                                gio::DBusCallFlags::NONE,
                                5000,
                                None::<&gio::Cancellable>,
                                move |reply| {
                                    if let Ok(v) = reply {
                                        let sections = v.child_value(0).iter().filter_map(|e| {
                                            let name = e.child_value(0).str()?.to_owned();
                                            let dict =
                                                glib::VariantDict::new(Some(&e.child_value(1)));
                                            Some((name, dict))
                                        });
                                        // `gsm_apn` borrows the names; collect first.
                                        let owned: Vec<(String, glib::VariantDict)> =
                                            sections.collect();
                                        let refs =
                                            owned.iter().map(|(n, d)| (n.as_str(), d.clone()));
                                        device.borrow_mut().apn = gsm_apn(refs);
                                    }
                                    finish.tick();
                                },
                            ),
                            None => finish.tick(),
                        }
                    },
                );
            }
            None => finish.tick(),
        }
        finish.tick();
    });
}

/// Runs its closure once `n` ticks have come in.
struct Countdown {
    left: Cell<usize>,
    then: RefCell<Option<Box<dyn FnOnce()>>>,
}

impl Countdown {
    fn new(n: usize, then: impl FnOnce() + 'static) -> Rc<Self> {
        Rc::new(Self {
            left: Cell::new(n),
            then: RefCell::new(Some(Box::new(then))),
        })
    }

    fn tick(&self) {
        let left = self.left.get().saturating_sub(1);
        self.left.set(left);
        if left == 0 {
            if let Some(then) = self.then.borrow_mut().take() {
                then();
            }
        }
    }
}

/// `org.freedesktop.DBus.Properties.GetAll` as a dict, or `None`.
fn get_all(
    conn: &gio::DBusConnection,
    name: &str,
    path: &str,
    iface: &str,
    done: impl FnOnce(Option<glib::VariantDict>) + 'static,
) {
    conn.call(
        Some(name),
        path,
        PROPS,
        "GetAll",
        Some(&(iface,).to_variant()),
        glib::VariantTy::new("(a{sv})").ok(),
        gio::DBusCallFlags::NONE,
        5000,
        None::<&gio::Cancellable>,
        move |reply| {
            done(
                reply
                    .ok()
                    .map(|v| glib::VariantDict::new(Some(&v.child_value(0)))),
            )
        },
    );
}

/// An object-path property, `None` for "/" or absent.
fn object_path(props: &glib::VariantDict, key: &str) -> Option<String> {
    props
        .lookup_value(key, None)
        .and_then(|v| v.get::<glib::variant::ObjectPath>())
        .map(|p| p.as_str().to_owned())
        .filter(|p| p != "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modem_states_label_like_mm_docs() {
        assert_eq!(modem_state_label(MM_STATE_FAILED), "Modem failed");
        assert_eq!(modem_state_label(MM_STATE_LOCKED), "Locked");
        assert_eq!(modem_state_label(MM_STATE_DISABLED), "Disabled");
        assert_eq!(modem_state_label(MM_STATE_SEARCHING), "Searching");
        assert_eq!(modem_state_label(MM_STATE_REGISTERED), "Registered");
        assert_eq!(modem_state_label(MM_STATE_CONNECTED), "Connected");
        assert_eq!(modem_state_label(99), "Unknown");
        assert!(modem_registered(MM_STATE_CONNECTED));
        assert!(modem_registered(MM_STATE_REGISTERED));
        assert!(!modem_registered(MM_STATE_SEARCHING));
        assert!(!modem_registered(MM_STATE_LOCKED));
    }

    #[test]
    fn sim_locks_ask_for_pin_or_puk() {
        assert_eq!(sim_lock_label(MM_LOCK_SIM_PIN), Some("SIM PIN required"));
        assert_eq!(sim_lock_label(MM_LOCK_SIM_PUK), Some("SIM PUK required"));
        assert_eq!(sim_lock_label(MM_LOCK_NONE), None);
        assert_eq!(sim_lock_label(0), None);
        assert_eq!(sim_lock_label(6), Some("SIM locked"));
    }

    #[test]
    fn cellular_icons_follow_wireless_thresholds() {
        assert_eq!(
            cellular_signal_icon(80, true),
            "network-cellular-signal-excellent-symbolic"
        );
        assert_eq!(
            cellular_signal_icon(60, true),
            "network-cellular-signal-good-symbolic"
        );
        assert_eq!(
            cellular_signal_icon(45, true),
            "network-cellular-signal-ok-symbolic"
        );
        assert_eq!(
            cellular_signal_icon(20, true),
            "network-cellular-signal-weak-symbolic"
        );
        assert_eq!(
            cellular_signal_icon(10, true),
            "network-cellular-signal-none-symbolic"
        );
        // A stale reading shows no bars, however high it was.
        assert_eq!(
            cellular_signal_icon(100, false),
            "network-cellular-signal-none-symbolic"
        );
    }

    #[test]
    fn toggle_hides_without_modems_and_names_the_operator() {
        let hidden = wwan_toggle(&[], true);
        assert!(!hidden.visible);
        let on = wwan_toggle(
            &[WwanDevice {
                state: ACTIVATED,
                modem: Some("/org/freedesktop/ModemManager1/Modem/0".into()),
                operator: "ExampleTel".into(),
                quality: Some((80, true)),
                modem_state: MM_STATE_CONNECTED,
                sim_lock: MM_LOCK_NONE,
                ..WwanDevice::default()
            }],
            true,
        );
        assert!(on.visible && on.checked);
        assert_eq!(on.subtitle.as_deref(), Some("ExampleTel"));
        assert_eq!(on.icon, "network-cellular-signal-excellent-symbolic");
        // A SIM lock wins over the operator line.
        let locked = wwan_toggle(
            &[WwanDevice {
                modem_state: MM_STATE_LOCKED,
                sim_lock: MM_LOCK_SIM_PIN,
                operator: "ExampleTel".into(),
                ..WwanDevice::default()
            }],
            true,
        );
        assert_eq!(locked.subtitle.as_deref(), Some("SIM PIN required"));
        // The master switch reads back off.
        let off = wwan_toggle(
            &[WwanDevice {
                modem_state: MM_STATE_DISABLED,
                sim_lock: MM_LOCK_NONE,
                ..WwanDevice::default()
            }],
            false,
        );
        assert!(off.visible && !off.checked);
        assert_eq!(off.subtitle.as_deref(), Some("Disabled"));
    }

    #[test]
    fn gsm_apn_comes_from_the_gsm_section() {
        let gsm = glib::VariantDict::new(None);
        gsm.insert_value("apn", &"internet.example".to_variant());
        let conn = glib::VariantDict::new(None);
        conn.insert_value("id", &"ExampleTel".to_variant());
        let apn = gsm_apn([("connection", conn), ("gsm", gsm)].into_iter());
        assert_eq!(apn.as_deref(), Some("internet.example"));
        let empty = glib::VariantDict::new(None);
        assert_eq!(gsm_apn([("ipv4", empty)].into_iter()), None);
    }

    #[test]
    fn modem_path_comes_from_device_udi() {
        // The exact official linkage: base Device.Udi is the MM path.
        assert_eq!(
            modem_path_from_udi("/org/freedesktop/ModemManager1/Modem/0").as_deref(),
            Some("/org/freedesktop/ModemManager1/Modem/0")
        );
        // Anything else means no modem behind this device.
        assert_eq!(modem_path_from_udi("/sys/devices/pci0000:00/wwan0"), None);
        assert_eq!(modem_path_from_udi(""), None);
        assert_eq!(modem_path_from_udi("/"), None);
        assert_eq!(modem_path_from_udi("/org/freedesktop/ModemManager1/"), None);
        // Interfaces queried are the official ones.
        assert_eq!(DEVICE, "org.freedesktop.NetworkManager.Device");
        assert_eq!(NM_MODEM, "org.freedesktop.NetworkManager.Device.Modem");
        assert_eq!(MM_MODEM_IFACE, "org.freedesktop.ModemManager1.Modem");
        assert_eq!(
            MM_MODEM_3GPP_IFACE,
            "org.freedesktop.ModemManager1.Modem.Modem3gpp"
        );
    }

    #[test]
    fn nm_modem_reads_real_properties_not_an_invented_path() {
        let props = glib::VariantDict::new(None);
        props.insert_value("ModemCapabilities", &4u32.to_variant());
        props.insert_value("CurrentCapabilities", &12u32.to_variant());
        props.insert_value("DeviceId", &"359762080000001".to_variant());
        props.insert_value("OperatorCode", &"310260".to_variant());
        props.insert_value("Apn", &"internet.example".to_variant());
        let parsed = parse_nm_modem(&props);
        assert_eq!(
            parsed,
            NmModemProps {
                modem_caps: 4,
                current_caps: 12,
                device_id: "359762080000001".into(),
                operator_code: "310260".into(),
                apn: Some("internet.example".into()),
            }
        );
        // An invented `Modem` on this interface is ignored: the
        // parser reads only the documented properties.
        let invented = glib::VariantDict::new(None);
        invented.insert_value(
            "Modem",
            &"/org/freedesktop/ModemManager1/Modem/0".to_variant(),
        );
        assert_eq!(parse_nm_modem(&invented), NmModemProps::default());
        // Empty APN reads as absent.
        let empty_apn = glib::VariantDict::new(None);
        empty_apn.insert_value("Apn", &"".to_variant());
        assert_eq!(parse_nm_modem(&empty_apn).apn, None);
    }

    #[test]
    fn operator_name_comes_from_modem3gpp_only() {
        let gpp = glib::VariantDict::new(None);
        gpp.insert_value("OperatorName", &"ExampleTel".to_variant());
        assert_eq!(operator_from_3gpp(&gpp), "ExampleTel");
        let missing = glib::VariantDict::new(None);
        assert_eq!(operator_from_3gpp(&missing), "");
        // The base Modem read ignores an OperatorName even if a stub
        // invents one there: state, quality and lock only.
        let modem = glib::VariantDict::new(None);
        modem.insert_value("State", &11i32.to_variant());
        modem.insert_value("SignalQuality", &(82u32, true).to_variant());
        modem.insert_value("UnlockRequired", &1u32.to_variant());
        modem.insert_value("OperatorName", &"InventedTel".to_variant());
        let (state, quality, lock) = parse_mm_modem(&modem);
        assert_eq!(state, MM_STATE_CONNECTED);
        assert_eq!(quality, Some((82, true)));
        assert_eq!(lock, MM_LOCK_NONE);
    }

    #[test]
    fn apn_prefers_gsm_settings_over_nm_modem() {
        assert_eq!(
            combine_apn(Some("gsm.example".into()), Some("nm.example".into())).as_deref(),
            Some("gsm.example")
        );
        assert_eq!(
            combine_apn(None, Some("nm.example".into())).as_deref(),
            Some("nm.example")
        );
        assert_eq!(
            combine_apn(Some("gsm.example".into()), None).as_deref(),
            Some("gsm.example")
        );
        assert_eq!(combine_apn(None, None), None);
    }

    #[test]
    fn operator_shows_only_while_registered() {
        let searching = WwanDevice {
            state: ACTIVATED,
            modem: Some("/org/freedesktop/ModemManager1/Modem/0".into()),
            operator: "ExampleTel".into(),
            modem_state: MM_STATE_SEARCHING,
            sim_lock: MM_LOCK_NONE,
            ..WwanDevice::default()
        };
        assert_eq!(searching.subtitle(), "Searching");
        let registered = WwanDevice {
            modem_state: MM_STATE_REGISTERED,
            ..searching.clone()
        };
        assert_eq!(registered.subtitle(), "ExampleTel");
        // Without the ModemManager modem the state shows even with a
        // cached operator name (ModemManager gone blanks the row).
        let gone = WwanDevice {
            modem: None,
            ..registered.clone()
        };
        assert_eq!(gone.subtitle(), "Registered");
    }
}
