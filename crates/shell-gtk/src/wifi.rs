//! GNOME's Wi-Fi menu (network.js `NMWirelessToggle`): the networks
//! NetworkManager sees, grouped and sorted as GNOME does, scanned when
//! the menu opens and every 15 s while it stays open. A saved network
//! activates its connection; any other gets a new connection (the
//! shell's secret agent asks for a password); 802.1X goes to Settings. The
//! toggle shows the connected network's name and signal.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gio::prelude::*;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::logic::{self, AccessPoint, WifiNetwork, WifiSecurity};
use crate::services::Tile;

const NM: &str = "org.freedesktop.NetworkManager";
const NM_PATH: &str = "/org/freedesktop/NetworkManager";
const PROPS: &str = "org.freedesktop.DBus.Properties";
const DEVICE: &str = "org.freedesktop.NetworkManager.Device";
const WIRELESS: &str = "org.freedesktop.NetworkManager.Device.Wireless";
const AP: &str = "org.freedesktop.NetworkManager.AccessPoint";
const CONNECTION: &str = "org.freedesktop.NetworkManager.Settings.Connection";
/// GNOME rescans this often while the menu is open (`WIFI_SCAN_FREQUENCY`).
const SCAN_SECONDS: u32 = 15;

#[derive(Default)]
struct Seen {
    aps: Vec<AccessPoint>,
    active: Option<String>,
    known: Vec<(Vec<u8>, String)>,
}

/// The Wi-Fi tile's menu and the state behind it.
pub struct WifiMenu {
    list: gtk::Box,
    tile: RefCell<Option<Tile>>,
    conn: RefCell<Option<gio::DBusConnection>>,
    device: RefCell<Option<String>>,
    networks: RefCell<Vec<WifiNetwork>>,
    pending: Cell<bool>,
    scan: RefCell<Option<glib::SourceId>>,
    enabled: Cell<bool>,
}

impl WifiMenu {
    /// The menu's network list goes into `list`.
    pub fn new(list: gtk::Box) -> Rc<Self> {
        Rc::new(Self {
            list,
            tile: RefCell::default(),
            conn: RefCell::default(),
            device: RefCell::default(),
            networks: RefCell::default(),
            pending: Cell::new(false),
            scan: RefCell::default(),
            enabled: Cell::new(false),
        })
    }

    /// The toggle whose subtitle and icon follow the connection.
    pub fn set_tile(&self, tile: Tile) {
        *self.tile.borrow_mut() = Some(tile);
    }

    /// NetworkManager's Wi-Fi device (or none), on `conn`.
    pub fn attach(self: &Rc<Self>, conn: &gio::DBusConnection, device: Option<String>) {
        let first = self.conn.borrow().is_none();
        *self.conn.borrow_mut() = Some(conn.clone());
        *self.device.borrow_mut() = device;
        if first {
            // Any NetworkManager property change or access point coming
            // or going: reload (debounced).
            for (iface, member) in [
                (PROPS, "PropertiesChanged"),
                (WIRELESS, "AccessPointAdded"),
                (WIRELESS, "AccessPointRemoved"),
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
        }
        self.schedule_reload();
    }

    /// WirelessEnabled, as the tile shows it.
    pub fn set_enabled(self: &Rc<Self>, on: bool) {
        self.enabled.set(on);
        self.sync_tile();
    }

    /// The menu opened (scan now and every 15 s) or closed.
    pub fn set_open(self: &Rc<Self>, open: bool) {
        if let Some(id) = self.scan.borrow_mut().take() {
            id.remove();
        }
        if !open {
            return;
        }
        self.request_scan();
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_seconds_local(SCAN_SECONDS, move || match weak.upgrade() {
            Some(menu) => {
                menu.request_scan();
                glib::ControlFlow::Continue
            }
            None => glib::ControlFlow::Break,
        });
        *self.scan.borrow_mut() = Some(id);
    }

    fn request_scan(&self) {
        let (Some(conn), Some(device)) = (self.conn.borrow().clone(), self.device.borrow().clone())
        else {
            return;
        };
        if !self.enabled.get() {
            return;
        }
        let options = glib::VariantDict::new(None).end();
        conn.call(
            Some(NM),
            &device,
            WIRELESS,
            "RequestScan",
            Some(&glib::Variant::tuple_from_iter([options])),
            None,
            gio::DBusCallFlags::NONE,
            5000,
            None::<&gio::Cancellable>,
            |_| {},
        );
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

    /// Read the device's access points, the active one and the saved
    /// connections it can use, then rebuild.
    fn reload(self: &Rc<Self>) {
        let (Some(conn), Some(device)) = (self.conn.borrow().clone(), self.device.borrow().clone())
        else {
            self.show(Vec::new());
            return;
        };
        let weak = Rc::downgrade(self);
        let conn2 = conn.clone();
        let device_path = device.clone();
        get_all(&conn, &device_path, WIRELESS, move |props| {
            let Some(props) = props else { return };
            let (ap_paths, active) = wireless_state(&props);
            let seen = Rc::new(RefCell::new(Seen {
                active,
                ..Seen::default()
            }));
            let conn3 = conn2.clone();
            let device2 = device.clone();
            get_all(&conn2, &device2, DEVICE, move |dev| {
                let conns = dev
                    .map(|d| object_paths(&d, "AvailableConnections"))
                    .unwrap_or_default();
                let left = Rc::new(Cell::new(ap_paths.len() + conns.len() + 1));
                let done = {
                    let (seen, left, weak) = (seen.clone(), left.clone(), weak.clone());
                    Rc::new(move || {
                        left.set(left.get() - 1);
                        if left.get() == 0 {
                            if let Some(menu) = weak.upgrade() {
                                let s = seen.borrow();
                                menu.show(logic::wifi_networks(
                                    &s.aps,
                                    s.active.as_deref(),
                                    &s.known,
                                ));
                            }
                        }
                    })
                };
                for path in ap_paths {
                    let (seen, done) = (seen.clone(), done.clone());
                    let p = path.clone();
                    get_all(&conn3, &path, AP, move |props| {
                        if let Some(props) = props {
                            seen.borrow_mut().aps.push(access_point(p, &props));
                        }
                        done();
                    });
                }
                for path in conns {
                    let (seen, done) = (seen.clone(), done.clone());
                    let p = path.clone();
                    conn3.call(
                        Some(NM),
                        &path,
                        CONNECTION,
                        "GetSettings",
                        None,
                        glib::VariantTy::new("(a{sa{sv}})").ok(),
                        gio::DBusCallFlags::NONE,
                        5000,
                        None::<&gio::Cancellable>,
                        move |reply| {
                            if let Some(ssid) = reply.ok().as_ref().and_then(saved_ssid) {
                                seen.borrow_mut().known.push((ssid, p));
                            }
                            done();
                        },
                    );
                }
                done();
            });
        });
    }

    fn show(self: &Rc<Self>, networks: Vec<WifiNetwork>) {
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        for net in &networks {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            let icons = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            icons.append(&gtk::Image::from_icon_name(logic::signal_icon(
                net.strength,
            )));
            if net.security.secure() {
                let lock = gtk::Image::from_icon_name("network-wireless-encrypted-symbolic");
                lock.add_css_class("wireless-secure-icon");
                // `.nm-network-item .wireless-secure-icon { icon-size:
                // 0.5455em }`: 8px at the 11pt menu font.
                lock.set_pixel_size(8);
                lock.set_valign(gtk::Align::End);
                icons.append(&lock);
            }
            row.append(&icons);
            let label = gtk::Label::new(Some(&net.name));
            label.set_xalign(0.0);
            label.set_hexpand(true);
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            row.append(&label);
            let check = gtk::Image::from_icon_name("object-select-symbolic");
            check.set_visible(net.active);
            row.append(&check);
            let button = gtk::Button::builder().child(&row).build();
            button.add_css_class("qs-menu-item");
            button.add_css_class("nm-network-item");
            // GNOME's accessible name: "<name>, Secure|Not secure,
            // Signal strength N%".
            button.update_property(&[gtk::accessible::Property::Label(&format!(
                "{}, {}, Signal strength {}%",
                net.name,
                if net.security.secure() {
                    "Secure"
                } else {
                    "Not secure"
                },
                net.strength
            ))]);
            let weak = Rc::downgrade(self);
            let net2 = net.clone();
            button.connect_clicked(move |b| {
                // Picking an item closes the panel (PopupMenu activation).
                if let Some(popover) = b
                    .ancestor(gtk::Popover::static_type())
                    .and_downcast::<gtk::Popover>()
                {
                    popover.popdown();
                }
                if let Some(menu) = weak.upgrade() {
                    menu.activate(&net2);
                }
            });
            self.list.append(&button);
        }
        *self.networks.borrow_mut() = networks;
        self.sync_tile();
    }

    /// GNOME's subtitle (the connected network's name, else none) and
    /// icon (its signal, or disabled / none).
    fn sync_tile(&self) {
        let Some(tile) = self.tile.borrow().clone() else {
            return;
        };
        let networks = self.networks.borrow();
        let active = networks.iter().find(|n| n.active);
        let on = self.enabled.get();
        tile.show_state(on, active.filter(|_| on).map(|n| n.name.as_str()));
        tile.icon.set_icon_name(Some(tile_icon(on, active)));
    }

    /// A saved network activates its connection; an open one gets a
    /// new connection; anything needing secrets goes to Settings (GNOME
    /// asks for a password through its own agent).
    fn activate(&self, net: &WifiNetwork) {
        let (Some(conn), Some(device)) = (self.conn.borrow().clone(), self.device.borrow().clone())
        else {
            return;
        };
        let path = |p: &str| glib::variant::ObjectPath::try_from(p.to_owned()).ok();
        let (Some(dev), Some(ap)) = (path(&device), path(&net.ap)) else {
            return;
        };
        match activation(net) {
            Activation::Saved(saved) => {
                let (Some(saved), Some(none)) = (path(saved), path("/")) else {
                    return;
                };
                conn.call(
                    Some(NM),
                    NM_PATH,
                    NM,
                    "ActivateConnection",
                    Some(&(saved, dev, none).to_variant()),
                    None,
                    gio::DBusCallFlags::NONE,
                    -1,
                    None::<&gio::Cancellable>,
                    |_| {},
                );
            }
            // GNOME's agent (network_agent.rs) asks for the password.
            Activation::New => {
                let empty: std::collections::HashMap<
                    String,
                    std::collections::HashMap<String, glib::Variant>,
                > = Default::default();
                conn.call(
                    Some(NM),
                    NM_PATH,
                    NM,
                    "AddAndActivateConnection",
                    Some(&(empty, dev, ap).to_variant()),
                    None,
                    gio::DBusCallFlags::NONE,
                    -1,
                    None::<&gio::Cancellable>,
                    |_| {},
                );
            }
            Activation::Settings => {
                let _ = tuna_shell_host::apps::application_command("gnome-control-center")
                    .args(["wifi", "connect-8021x-wifi", &device, &net.ap])
                    .spawn();
            }
        }
    }
}

/// The tile's icon: disabled, the connected network's signal, or none.
fn tile_icon(enabled: bool, active: Option<&WifiNetwork>) -> &'static str {
    match (enabled, active) {
        (false, _) => "network-wireless-disabled-symbolic",
        (true, Some(net)) => logic::signal_icon(net.strength),
        (true, None) => "network-wireless-signal-none-symbolic",
    }
}

/// What picking a network asks for (network.js `WirelessNetwork.activate`).
#[derive(Debug, PartialEq, Eq)]
enum Activation<'a> {
    /// `ActivateConnection` of its saved connection.
    Saved(&'a str),
    /// `AddAndActivateConnection`; the secret agent asks for a password.
    New,
    /// 802.1X without a saved connection: Settings' connect dialog.
    Settings,
}

fn activation(net: &WifiNetwork) -> Activation<'_> {
    match (&net.connection, net.security) {
        (Some(saved), _) => Activation::Saved(saved),
        (None, WifiSecurity::Open | WifiSecurity::Personal) => Activation::New,
        (None, WifiSecurity::Enterprise) => Activation::Settings,
    }
}

/// An object-path array property (absent or mistyped: none).
fn object_paths(props: &glib::VariantDict, key: &str) -> Vec<String> {
    props
        .lookup_value(key, None)
        .and_then(|v| v.get::<Vec<glib::variant::ObjectPath>>())
        .map(|p| p.into_iter().map(|p| p.as_str().to_owned()).collect())
        .unwrap_or_default()
}

/// The Wi-Fi device's access points and its active one ("/" is none).
fn wireless_state(props: &glib::VariantDict) -> (Vec<String>, Option<String>) {
    let active = props
        .lookup_value("ActiveAccessPoint", None)
        .and_then(|v| v.get::<glib::variant::ObjectPath>())
        .map(|p| p.as_str().to_owned())
        .filter(|p| p != "/");
    (object_paths(props, "AccessPoints"), active)
}

/// One access point from its properties; absent or mistyped ones read
/// as zero (an empty SSID is a hidden network the menu skips).
fn access_point(path: String, props: &glib::VariantDict) -> AccessPoint {
    let u = |k: &str| {
        props
            .lookup_value(k, None)
            .and_then(|v| v.get::<u32>())
            .unwrap_or(0)
    };
    AccessPoint {
        path,
        ssid: props
            .lookup_value("Ssid", None)
            .and_then(|v| v.get::<Vec<u8>>())
            .unwrap_or_default(),
        strength: props
            .lookup_value("Strength", None)
            .and_then(|v| v.get::<u8>())
            .unwrap_or(0),
        flags: u("Flags"),
        wpa_flags: u("WpaFlags"),
        rsn_flags: u("RsnFlags"),
        mode: u("Mode"),
    }
}

/// A saved connection's Wi-Fi SSID from `GetSettings`' `(a{sa{sv}})`:
/// the 802-11-wireless section's ssid; none for other connections.
fn saved_ssid(reply: &glib::Variant) -> Option<Vec<u8>> {
    let settings = reply
        .try_child_value(0)
        .filter(|v| v.type_().as_str() == "a{sa{sv}}")?;
    let wifi = settings.iter().find_map(|entry| {
        (entry.child_value(0).str() == Some("802-11-wireless")).then(|| entry.child_value(1))
    })?;
    glib::VariantDict::new(Some(&wifi))
        .lookup_value("ssid", None)?
        .get::<Vec<u8>>()
}

/// `org.freedesktop.DBus.Properties.GetAll` as a dict, or `None`.
fn get_all(
    conn: &gio::DBusConnection,
    path: &str,
    iface: &str,
    done: impl FnOnce(Option<glib::VariantDict>) + 'static,
) {
    conn.call(
        Some(NM),
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

#[cfg(test)]
mod tests {
    use super::*;

    const PSK: u32 = 0x188; // pair/group CCMP + PSK
    const EAP: u32 = 0x288; // pair/group CCMP + 802.1X
    const PRIVACY: u32 = 0x1;

    fn ap_props(ssid: &[u8], strength: u8, flags: u32, wpa: u32, rsn: u32) -> glib::VariantDict {
        let props = glib::VariantDict::new(None);
        props.insert_value("Ssid", &ssid.to_variant());
        props.insert_value("Strength", &strength.to_variant());
        props.insert_value("Flags", &flags.to_variant());
        props.insert_value("WpaFlags", &wpa.to_variant());
        props.insert_value("RsnFlags", &rsn.to_variant());
        props.insert_value("Mode", &2u32.to_variant());
        props
    }

    fn ap(path: &str, ssid: &[u8], strength: u8, rsn: u32) -> AccessPoint {
        let flags = if rsn != 0 { PRIVACY } else { 0 };
        access_point(path.into(), &ap_props(ssid, strength, flags, 0, rsn))
    }

    fn settings(sections: &[(&str, &[(&str, glib::Variant)])]) -> glib::Variant {
        let map: std::collections::HashMap<
            String,
            std::collections::HashMap<String, glib::Variant>,
        > = sections
            .iter()
            .map(|(name, keys)| {
                (
                    (*name).to_owned(),
                    keys.iter()
                        .map(|(k, v)| ((*k).to_owned(), v.clone()))
                        .collect(),
                )
            })
            .collect();
        (map,).to_variant()
    }

    fn names(nets: &[WifiNetwork]) -> Vec<&str> {
        nets.iter().map(|n| n.name.as_str()).collect()
    }

    #[test]
    fn access_point_properties_parse_and_default_to_zero() {
        let parsed = ap("/ap/1", b"Cafe", 64, PSK);
        assert_eq!(
            parsed,
            AccessPoint {
                path: "/ap/1".into(),
                ssid: b"Cafe".to_vec(),
                strength: 64,
                flags: PRIVACY,
                wpa_flags: 0,
                rsn_flags: PSK,
                mode: 2,
            }
        );
        // Missing or mistyped properties (a broken NetworkManager) read
        // as a hidden, open, zero-strength access point: never shown.
        let junk = glib::VariantDict::new(None);
        junk.insert_value("Ssid", &"Cafe".to_variant());
        junk.insert_value("Strength", &64u32.to_variant());
        let junk = access_point("/ap/2".into(), &junk);
        assert!(junk.ssid.is_empty());
        assert_eq!(junk.strength, 0);
        assert!(logic::wifi_networks(&[junk], None, &[]).is_empty());
    }

    #[test]
    fn device_state_reads_active_access_point_and_list() {
        let props = glib::VariantDict::new(None);
        let paths = |p: &[&str]| -> Vec<glib::variant::ObjectPath> {
            p.iter()
                .map(|p| glib::variant::ObjectPath::try_from(*p).unwrap())
                .collect()
        };
        props.insert_value("AccessPoints", &paths(&["/ap/1", "/ap/2"]).to_variant());
        props.insert_value(
            "ActiveAccessPoint",
            &glib::variant::ObjectPath::try_from("/ap/2")
                .unwrap()
                .to_variant(),
        );
        assert_eq!(
            wireless_state(&props),
            (vec!["/ap/1".into(), "/ap/2".into()], Some("/ap/2".into()))
        );
        // NetworkManager's "/" means not connected.
        props.insert_value(
            "ActiveAccessPoint",
            &glib::variant::ObjectPath::try_from("/")
                .unwrap()
                .to_variant(),
        );
        assert_eq!(wireless_state(&props).1, None);
        // Strings where object paths belong are not access points.
        props.insert_value("AccessPoints", &vec!["/ap/1"].to_variant());
        props.insert_value("ActiveAccessPoint", &"/ap/1".to_variant());
        assert_eq!(wireless_state(&props), (Vec::new(), None));
        assert!(wireless_state(&glib::VariantDict::new(None)).0.is_empty());
    }

    #[test]
    fn saved_connections_match_by_wireless_ssid_only() {
        let wifi = settings(&[
            ("connection", &[("id", "Home".to_variant())]),
            (
                "802-11-wireless",
                &[("ssid", b"Home\xff".to_vec().to_variant())],
            ),
        ]);
        assert_eq!(saved_ssid(&wifi), Some(b"Home\xff".to_vec()));
        // An ethernet profile, a wifi section without an SSID, a
        // string SSID, or a wrong-shaped reply match no network.
        let ethernet = settings(&[
            ("connection", &[("id", "Home".to_variant())]),
            ("802-3-ethernet", &[("ssid", b"Home".to_vec().to_variant())]),
        ]);
        let no_ssid = settings(&[(
            "802-11-wireless",
            &[("mode", "infrastructure".to_variant())],
        )]);
        let string_ssid = settings(&[("802-11-wireless", &[("ssid", "Home".to_variant())])]);
        for reply in [
            ethernet,
            no_ssid,
            string_ssid,
            ("x",).to_variant(),
            true.to_variant(),
        ] {
            assert_eq!(saved_ssid(&reply), None, "{reply}");
        }
    }

    #[test]
    fn networks_dedupe_by_ssid_keeping_the_strongest_access_point() {
        let aps = [
            ap("/ap/1", b"Mesh", 30, PSK),
            ap("/ap/2", b"Mesh", 85, PSK),
            ap("/ap/3", b"Mesh", 60, PSK),
            // Same name, different security: GNOME lists it separately.
            ap("/ap/4", b"Mesh", 50, 0),
            // Hidden networks are not listed.
            ap("/ap/5", b"", 99, PSK),
        ];
        let nets = logic::wifi_networks(&aps, Some("/ap/1"), &[]);
        assert_eq!(names(&nets), ["Mesh", "Mesh"]);
        let secure = &nets[0];
        assert_eq!((secure.ap.as_str(), secure.strength), ("/ap/2", 85));
        assert!(secure.active, "connected through a weaker access point");
        assert_eq!(
            signal_icon_of(secure),
            "network-wireless-signal-excellent-symbolic"
        );
        assert_eq!(nets[1].security, WifiSecurity::Open);
        assert!(!nets[1].active);
    }

    fn signal_icon_of(net: &WifiNetwork) -> &'static str {
        tile_icon(true, Some(net))
    }

    #[test]
    fn networks_sort_saved_then_strength_then_secure_then_name() {
        let aps = [
            ap("/ap/1", b"zeta", 40, 0),
            ap("/ap/2", b"Alpha", 40, 0),
            ap("/ap/3", b"Strong", 90, PSK),
            ap("/ap/4", b"Locked", 40, PSK),
            ap("/ap/5", b"Saved", 10, PSK),
        ];
        let known = [(b"Saved".to_vec(), "/conn/1".to_owned())];
        let nets = logic::wifi_networks(&aps, None, &known);
        // network.js `WirelessNetwork.compare`.
        assert_eq!(names(&nets), ["Saved", "Strong", "Locked", "Alpha", "zeta"]);
        assert_eq!(nets[0].connection.as_deref(), Some("/conn/1"));
    }

    #[test]
    fn non_utf8_ssids_are_shown_and_grouped_by_their_bytes() {
        let latin1 = b"Caf\xe9".to_vec();
        let other = b"Caf\xe8".to_vec();
        let aps = [
            ap("/ap/1", &latin1, 50, PSK),
            ap("/ap/2", &other, 40, PSK),
            ap("/ap/3", &latin1, 70, PSK),
        ];
        let known = [(latin1.clone(), "/conn/1".to_owned())];
        let nets = logic::wifi_networks(&aps, None, &known);
        assert_eq!(nets.len(), 2, "two SSIDs, even if they render alike");
        assert_eq!(nets[0].ssid, latin1);
        assert_eq!(nets[0].ap, "/ap/3");
        assert_eq!(nets[0].connection.as_deref(), Some("/conn/1"));
        assert!(nets[0].name.starts_with("Caf") && !nets[0].name.is_empty());
        assert!(
            nets[1].connection.is_none(),
            "a saved profile names exact bytes"
        );
    }

    #[test]
    fn security_classification_follows_nm_flags() {
        let of = |flags, wpa, rsn| WifiSecurity::of(flags, wpa, rsn);
        assert_eq!(of(0, 0, 0), WifiSecurity::Open);
        assert_eq!(of(PRIVACY, 0, 0), WifiSecurity::Personal, "WEP");
        assert_eq!(of(PRIVACY, 0x14a, 0), WifiSecurity::Personal, "WPA1-PSK");
        assert_eq!(of(PRIVACY, 0, PSK), WifiSecurity::Personal, "WPA2-PSK");
        assert_eq!(of(PRIVACY, 0, 0x488), WifiSecurity::Personal, "WPA3-SAE");
        assert_eq!(of(PRIVACY, 0x24a, 0), WifiSecurity::Enterprise, "WPA1-EAP");
        assert_eq!(of(PRIVACY, 0, EAP), WifiSecurity::Enterprise, "WPA2-EAP");
        assert_eq!(
            of(PRIVACY, 0, 0x388),
            WifiSecurity::Enterprise,
            "PSK+EAP mixed"
        );
        assert_eq!(of(PRIVACY, 0, 0x888), WifiSecurity::Open, "OWE");
        assert_eq!(of(0, 0, 0x1088), WifiSecurity::Open, "OWE transition");
        assert_eq!(
            of(PRIVACY, 0, 0x988),
            WifiSecurity::Personal,
            "OWE with PSK"
        );
        assert!(!WifiSecurity::Open.secure());
        assert!(WifiSecurity::Personal.secure() && WifiSecurity::Enterprise.secure());
    }

    #[test]
    fn signal_icons_switch_at_gnomes_thresholds() {
        // network.js `signalToIcon`: <20 none, <40 weak, <50 ok,
        // <80 good, else excellent.
        let cases = [
            (0, "none"),
            (19, "none"),
            (20, "weak"),
            (39, "weak"),
            (40, "ok"),
            (49, "ok"),
            (50, "good"),
            (79, "good"),
            (80, "excellent"),
            (100, "excellent"),
            (255, "excellent"),
        ];
        for (strength, level) in cases {
            assert_eq!(
                logic::signal_icon(strength),
                format!("network-wireless-signal-{level}-symbolic"),
                "{strength}"
            );
        }
    }

    #[test]
    fn tile_icon_follows_radio_and_connection() {
        let net = |strength| WifiNetwork {
            name: "Home".into(),
            ssid: b"Home".to_vec(),
            ap: "/ap/1".into(),
            strength,
            security: WifiSecurity::Personal,
            active: true,
            connection: None,
        };
        assert_eq!(
            tile_icon(false, Some(&net(90))),
            "network-wireless-disabled-symbolic"
        );
        assert_eq!(tile_icon(false, None), "network-wireless-disabled-symbolic");
        assert_eq!(
            tile_icon(true, None),
            "network-wireless-signal-none-symbolic"
        );
        assert_eq!(
            tile_icon(true, Some(&net(45))),
            "network-wireless-signal-ok-symbolic"
        );
    }

    #[test]
    fn picking_a_network_activates_saved_creates_new_or_opens_settings() {
        let net = |security, connection: Option<&str>| WifiNetwork {
            name: "Net".into(),
            ssid: b"Net".to_vec(),
            ap: "/ap/1".into(),
            strength: 50,
            security,
            active: false,
            connection: connection.map(Into::into),
        };
        // A saved profile always activates, 802.1X included.
        for security in [
            WifiSecurity::Open,
            WifiSecurity::Personal,
            WifiSecurity::Enterprise,
        ] {
            assert_eq!(
                activation(&net(security, Some("/conn/1"))),
                Activation::Saved("/conn/1")
            );
        }
        assert_eq!(activation(&net(WifiSecurity::Open, None)), Activation::New);
        assert_eq!(
            activation(&net(WifiSecurity::Personal, None)),
            Activation::New
        );
        assert_eq!(
            activation(&net(WifiSecurity::Enterprise, None)),
            Activation::Settings
        );
    }
}
