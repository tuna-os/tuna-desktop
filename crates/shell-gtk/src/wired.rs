//! GNOME's wired toggle and its menu (network.js `NMWiredToggle`): the
//! ethernet devices NetworkManager lists, each with its saved profiles.
//! The toggle disconnects whatever is up, or brings up the most recently
//! used profile (a new automatic one when there is none); the menu picks
//! a profile, and Wired Settings opens GNOME Settings.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gio::prelude::*;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::logic::{self, WiredAction, WiredConnection, WiredDevice, WiredRow};
use crate::services::Tile;

const NM: &str = "org.freedesktop.NetworkManager";
const NM_PATH: &str = "/org/freedesktop/NetworkManager";
const PROPS: &str = "org.freedesktop.DBus.Properties";
const DEVICE: &str = "org.freedesktop.NetworkManager.Device";
const ACTIVE: &str = "org.freedesktop.NetworkManager.Connection.Active";
const CONNECTION: &str = "org.freedesktop.NetworkManager.Settings.Connection";
/// `NM_DEVICE_TYPE_ETHERNET`.
const ETHERNET: u32 = 1;
/// libnm's generic name for an ethernet device (`disambiguate_names`).
const DEVICE_NAME: &str = "Wired";

/// The wired toggle's menu and the state behind it.
pub struct WiredMenu {
    list: gtk::Box,
    tile: RefCell<Option<Tile>>,
    conn: RefCell<Option<gio::DBusConnection>>,
    devices: RefCell<Vec<WiredDevice>>,
    connectivity: Cell<u32>,
    pending: Cell<bool>,
    /// Bumped on every reload, so a slower earlier one cannot win.
    generation: Cell<u64>,
}

impl WiredMenu {
    /// The menu's rows go into `list`.
    pub fn new(list: gtk::Box) -> Rc<Self> {
        Rc::new(Self {
            list,
            tile: RefCell::default(),
            conn: RefCell::default(),
            devices: RefCell::default(),
            connectivity: Cell::new(0),
            pending: Cell::new(false),
            generation: Cell::new(0),
        })
    }

    /// The toggle that follows the devices; a click runs GNOME's
    /// `NMToggle.activate`.
    pub fn set_tile(self: &Rc<Self>, tile: Tile) {
        let weak = Rc::downgrade(self);
        tile.on_user_toggle(move |_| {
            if let Some(menu) = weak.upgrade() {
                for (device, action) in logic::wired_click(&menu.devices.borrow()) {
                    menu.run(&device, &action);
                }
                // The tile shows NetworkManager's state, not the click.
                menu.sync_tile();
            }
        });
        *self.tile.borrow_mut() = Some(tile);
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
        }
        self.schedule_reload();
    }

    /// NetworkManager went away: no devices.
    pub fn detach(self: &Rc<Self>) {
        self.generation.set(self.generation.get() + 1);
        self.devices.borrow_mut().clear();
        self.render();
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

    /// Read connectivity, the ethernet devices, their profiles and
    /// active connections, then rebuild.
    fn reload(self: &Rc<Self>) {
        let Some(conn) = self.conn.borrow().clone() else {
            return;
        };
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        let weak = Rc::downgrade(self);
        let conn2 = conn.clone();
        get_all(&conn, NM_PATH, NM, move |nm| {
            let Some(nm) = nm else { return };
            let connectivity = nm
                .lookup_value("Connectivity", None)
                .and_then(|v| v.get::<u32>())
                .unwrap_or(0);
            let primary = object_path(&nm, "PrimaryConnection");
            let networking = nm
                .lookup_value("NetworkingEnabled", None)
                .and_then(|v| v.get::<bool>())
                .unwrap_or(true);
            let conn3 = conn2.clone();
            conn2.call(
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
                    let paths: Vec<String> = if networking {
                        reply
                            .ok()
                            .and_then(|v| v.child_value(0).get::<Vec<glib::variant::ObjectPath>>())
                            .map(|p| p.into_iter().map(|p| p.as_str().to_owned()).collect())
                            .unwrap_or_default()
                    } else {
                        Vec::new()
                    };
                    let found: Rc<RefCell<Vec<WiredDevice>>> = Rc::default();
                    let done = Countdown::new(paths.len() + 1, {
                        let (weak, found) = (weak.clone(), found.clone());
                        move || {
                            let Some(menu) = weak.upgrade() else { return };
                            if menu.generation.get() != generation {
                                return;
                            }
                            let mut devices = found.take();
                            devices.sort_by(|a, b| a.path.cmp(&b.path));
                            menu.connectivity.set(connectivity);
                            *menu.devices.borrow_mut() = devices;
                            menu.render();
                        }
                    });
                    for path in paths {
                        read_device(&conn3, path, primary.clone(), found.clone(), done.clone());
                    }
                    done.tick();
                },
            );
        });
    }

    fn render(self: &Rc<Self>) {
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        let devices = self.devices.borrow().clone();
        let connectivity = self.connectivity.get();
        let shown: Vec<_> = devices.iter().filter(|d| d.shown()).collect();
        for dev in &shown {
            if shown.len() > 1 {
                // NMSectionItem's use-submenu: each adapter has its own
                // expandable connection menu, rather than flattening rows.
                let label = submenu_label(dev);
                let rows = gtk::Box::new(gtk::Orientation::Vertical, 0);
                for row in dev.menu(DEVICE_NAME) {
                    rows.append(&self.row(dev, &row, connectivity));
                }
                // GNOME's PopupSubMenuMenuItem (`network.js:243`,
                // NMSectionItem): the submenu grows open, its arrow turns.
                let submenu = crate::transient::PopupSubmenu::new(&label, &rows);
                submenu.add_css_class("nm-wired-submenu");
                submenu.header().add_css_class("qs-menu-item");
                self.list.append(&submenu);
            } else {
                for row in dev.menu(DEVICE_NAME) {
                    self.list.append(&self.row(dev, &row, connectivity));
                }
            }
        }
        self.sync_tile();
    }

    /// One menu row as GNOME draws it (`NMConnectionItem`): the device's
    /// icon, its name and a Connect/Disconnect subtitle; radio rows are
    /// the profile's name with a dot ornament; Turn Off is plain.
    fn row(self: &Rc<Self>, dev: &WiredDevice, row: &WiredRow, connectivity: u32) -> gtk::Button {
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let label = |text: &str| {
            let l = gtk::Label::new(Some(text));
            l.set_xalign(0.0);
            l.set_hexpand(true);
            l.set_ellipsize(gtk::pango::EllipsizeMode::End);
            l
        };
        let (name, action) = match row {
            WiredRow::AutoConnect { label: text } => {
                line.append(&gtk::Image::from_icon_name(dev.icon(connectivity)));
                line.append(&label(text));
                (text.clone(), Some(WiredAction::AutoConnect))
            }
            WiredRow::Single {
                label: text,
                active,
                action,
            } => {
                line.append(&gtk::Image::from_icon_name(dev.icon(connectivity)));
                line.append(&label(text));
                let subtitle =
                    gtk::Label::new(Some(if *active { "Disconnect" } else { "Connect" }));
                subtitle.add_css_class("device-subtitle");
                line.append(&subtitle);
                let name = if *active {
                    format!("Disconnect {text}")
                } else {
                    format!("Connect to {text}")
                };
                (name, Some(action.clone()))
            }
            WiredRow::Radio {
                label: text,
                active,
                action,
            } => {
                let dot = gtk::Image::from_icon_name(if *active {
                    "ornament-dot-checked-symbolic"
                } else {
                    "ornament-dot-unchecked-symbolic"
                });
                dot.add_css_class("popup-menu-ornament");
                line.append(&dot);
                line.append(&label(text));
                (text.clone(), action.clone())
            }
            WiredRow::TurnOff => {
                line.append(&label("Turn Off"));
                ("Turn Off".to_owned(), Some(WiredAction::Disconnect))
            }
        };
        let button = gtk::Button::builder().child(&line).build();
        button.add_css_class("qs-menu-item");
        button.add_css_class("nm-wired-item");
        button.update_property(&[gtk::accessible::Property::Label(&name)]);
        if let WiredRow::Radio { active, .. } = row {
            button.update_state(&[gtk::accessible::State::Checked(if *active {
                gtk::AccessibleTristate::True
            } else {
                gtk::AccessibleTristate::False
            })]);
        }
        let weak = Rc::downgrade(self);
        let device = dev.path.clone();
        button.connect_clicked(move |b| {
            // Picking an item closes the panel (PopupMenu activation).
            if let Some(popover) = b
                .ancestor(gtk::Popover::static_type())
                .and_downcast::<gtk::Popover>()
            {
                popover.popdown();
            }
            if let (Some(menu), Some(action)) = (weak.upgrade(), action.as_ref()) {
                menu.run(&device, action);
            }
        });
        button
    }

    fn sync_tile(&self) {
        let Some(tile) = self.tile.borrow().clone() else {
            return;
        };
        let t = logic::wired_toggle(&self.devices.borrow(), self.connectivity.get());
        tile.present(t.visible);
        tile.show_state(t.checked, t.subtitle.as_deref());
        tile.icon.set_icon_name(Some(t.icon));
    }

    /// Ask NetworkManager for `action` on `device`.
    fn run(&self, device: &str, action: &WiredAction) {
        let Some(conn) = self.conn.borrow().clone() else {
            return;
        };
        let path = |p: &str| glib::variant::ObjectPath::try_from(p.to_owned()).ok();
        let (Some(dev), Some(none)) = (path(device), path("/")) else {
            return;
        };
        let (object, iface, method, args) = match action {
            WiredAction::Disconnect => (device.to_owned(), DEVICE, "Disconnect", None),
            WiredAction::Activate(saved) => {
                let Some(saved) = path(saved) else { return };
                (
                    NM_PATH.to_owned(),
                    NM,
                    "ActivateConnection",
                    Some((saved, dev, none).to_variant()),
                )
            }
            WiredAction::AutoConnect => {
                let empty: std::collections::HashMap<
                    String,
                    std::collections::HashMap<String, glib::Variant>,
                > = Default::default();
                (
                    NM_PATH.to_owned(),
                    NM,
                    "AddAndActivateConnection",
                    Some((empty, dev, none).to_variant()),
                )
            }
        };
        conn.call(
            Some(NM),
            &object,
            iface,
            method,
            args.as_ref(),
            None,
            gio::DBusCallFlags::NONE,
            -1,
            None::<&gio::Cancellable>,
            |_| {},
        );
    }
}

/// Read one device; an ethernet one joins `found` with its profiles
/// and active connection.
fn read_device(
    conn: &gio::DBusConnection,
    path: String,
    primary: Option<String>,
    found: Rc<RefCell<Vec<WiredDevice>>>,
    done: Rc<Countdown>,
) {
    let conn2 = conn.clone();
    get_all(conn, &path.clone(), DEVICE, move |props| {
        let Some((device, profiles, active)) = props
            .as_ref()
            .and_then(|props| ethernet_device(path, props, primary.as_deref()))
        else {
            done.tick();
            return;
        };
        let device = Rc::new(RefCell::new(device));
        let finish = Countdown::new(profiles.len() + 2, {
            let device = device.clone();
            move || {
                found.borrow_mut().push(device.take());
                done.tick();
            }
        });
        for profile in profiles {
            let (device, finish) = (device.clone(), finish.clone());
            let p = profile.clone();
            conn2.call(
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
                    if let Some(profile) = reply.ok().and_then(|v| saved_profile(p, &v)) {
                        device.borrow_mut().connections.push(profile);
                    }
                    finish.tick();
                },
            );
        }
        match active {
            Some(active) => {
                let (device, finish) = (device.clone(), finish.clone());
                get_all(&conn2, &active, ACTIVE, move |props| {
                    if let Some(props) = props {
                        device.borrow_mut().active = Some(active_connection(&props));
                    }
                    finish.tick();
                });
            }
            None => finish.tick(),
        }
        finish.tick();
    });
}

/// A listed adapter's submenu title when there are several: libnm's
/// generic name plus the interface (the object path's tail without one).
fn submenu_label(dev: &WiredDevice) -> String {
    if dev.interface.is_empty() {
        format!(
            "{DEVICE_NAME} ({})",
            dev.path.rsplit('/').next().unwrap_or_default()
        )
    } else {
        format!("{DEVICE_NAME} ({})", dev.interface)
    }
}

/// An ethernet device from its `Device` properties, with its usable
/// profiles and active connection; `None` for any other device type.
fn ethernet_device(
    path: String,
    props: &glib::VariantDict,
    primary: Option<&str>,
) -> Option<(WiredDevice, Vec<String>, Option<String>)> {
    let u = |k: &str| {
        props
            .lookup_value(k, None)
            .and_then(|v| v.get::<u32>())
            .unwrap_or(0)
    };
    if u("DeviceType") != ETHERNET {
        return None;
    }
    let profiles: Vec<String> = props
        .lookup_value("AvailableConnections", None)
        .and_then(|v| v.get::<Vec<glib::variant::ObjectPath>>())
        .map(|p| p.into_iter().map(|p| p.as_str().to_owned()).collect())
        .unwrap_or_default();
    let active = object_path(props, "ActiveConnection");
    let device = WiredDevice {
        primary: active.is_some() && active.as_deref() == primary,
        path,
        interface: props
            .lookup_value("Interface", None)
            .and_then(|v| v.get::<String>())
            .unwrap_or_default(),
        state: u("State"),
        ..WiredDevice::default()
    };
    Some((device, profiles, active))
}

/// A saved profile from `GetSettings`' `(a{sa{sv}})`: the "connection"
/// section's id (required) and timestamp (0 when never up).
fn saved_profile(path: String, reply: &glib::Variant) -> Option<WiredConnection> {
    let settings = reply
        .try_child_value(0)
        .filter(|v| v.type_().as_str() == "a{sa{sv}}")?;
    let section = settings.iter().find_map(|entry| {
        (entry.child_value(0).str() == Some("connection"))
            .then(|| glib::VariantDict::new(Some(&entry.child_value(1))))
    })?;
    let name = section
        .lookup_value("id", None)
        .and_then(|v| v.get::<String>())?;
    Some(WiredConnection {
        path,
        name,
        timestamp: section
            .lookup_value("timestamp", None)
            .and_then(|v| v.get::<u64>())
            .unwrap_or(0),
    })
}

/// An active connection's profile and `NMActiveConnectionState`.
fn active_connection(props: &glib::VariantDict) -> (String, u32) {
    let profile = object_path(props, "Connection").unwrap_or_default();
    let state = props
        .lookup_value("State", None)
        .and_then(|v| v.get::<u32>())
        .unwrap_or(0);
    (profile, state)
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

/// An object-path property, `None` for "/" or absent.
fn object_path(d: &glib::VariantDict, key: &str) -> Option<String> {
    d.lookup_value(key, None)
        .and_then(|v| v.get::<glib::variant::ObjectPath>())
        .map(|p| p.as_str().to_owned())
        .filter(|p| p != "/")
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

    fn opath(p: &str) -> glib::Variant {
        glib::variant::ObjectPath::try_from(p).unwrap().to_variant()
    }

    fn device_props(
        kind: u32,
        state: u32,
        iface: &str,
        profiles: &[&str],
        active: &str,
    ) -> glib::VariantDict {
        let props = glib::VariantDict::new(None);
        props.insert_value("DeviceType", &kind.to_variant());
        props.insert_value("State", &state.to_variant());
        props.insert_value("Interface", &iface.to_variant());
        let paths: Vec<glib::variant::ObjectPath> = profiles
            .iter()
            .map(|p| glib::variant::ObjectPath::try_from(*p).unwrap())
            .collect();
        props.insert_value("AvailableConnections", &paths.to_variant());
        props.insert_value("ActiveConnection", &opath(active));
        props
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

    fn conn(path: &str, name: &str, timestamp: u64) -> WiredConnection {
        WiredConnection {
            path: path.into(),
            name: name.into(),
            timestamp,
        }
    }

    #[test]
    fn only_ethernet_devices_are_read_with_profiles_and_primary() {
        let props = device_props(ETHERNET, 100, "enp3s0", &["/s/1", "/s/2"], "/ac/1");
        let (dev, profiles, active) =
            ethernet_device("/d/2".into(), &props, Some("/ac/1")).unwrap();
        assert_eq!(dev.path, "/d/2");
        assert_eq!(dev.interface, "enp3s0");
        assert_eq!(dev.state, 100);
        assert!(dev.primary);
        assert_eq!(profiles, ["/s/1", "/s/2"]);
        assert_eq!(active.as_deref(), Some("/ac/1"));
        // Another device's primary connection is not this one's.
        let (dev, ..) = ethernet_device("/d/2".into(), &props, Some("/ac/9")).unwrap();
        assert!(!dev.primary);
        // Not connected ("/"): no active connection, never primary, even
        // when NetworkManager has no primary connection either.
        let idle = device_props(ETHERNET, 30, "enp3s0", &[], "/");
        let (dev, profiles, active) = ethernet_device("/d/2".into(), &idle, None).unwrap();
        assert!(!dev.primary && active.is_none() && profiles.is_empty());
        // Wi-Fi (2), bridges (13), and devices without a type are not wired.
        for kind in [2, 13, 0] {
            let other = device_props(kind, 100, "wlan0", &[], "/");
            assert!(
                ethernet_device("/d/3".into(), &other, None).is_none(),
                "{kind}"
            );
        }
        let untyped = glib::VariantDict::new(None);
        untyped.insert_value("DeviceType", &"ethernet".to_variant());
        assert!(ethernet_device("/d/4".into(), &untyped, None).is_none());
    }

    #[test]
    fn profiles_need_a_connection_id_and_default_to_never_used() {
        let reply = settings(&[
            (
                "connection",
                &[
                    ("id", "Office".to_variant()),
                    ("timestamp", 1_700_000_000u64.to_variant()),
                ],
            ),
            ("802-3-ethernet", &[]),
        ]);
        assert_eq!(
            saved_profile("/s/1".into(), &reply),
            Some(conn("/s/1", "Office", 1_700_000_000))
        );
        let never = settings(&[("connection", &[("id", "Fresh".to_variant())])]);
        assert_eq!(
            saved_profile("/s/2".into(), &never),
            Some(conn("/s/2", "Fresh", 0))
        );
        let odd_stamp = settings(&[(
            "connection",
            &[
                ("id", "Odd".to_variant()),
                ("timestamp", "yesterday".to_variant()),
            ],
        )]);
        assert_eq!(
            saved_profile("/s/3".into(), &odd_stamp).unwrap().timestamp,
            0
        );
        // No id, no connection section, or the wrong shape: not a row.
        let unnamed = settings(&[("connection", &[("uuid", "x".to_variant())])]);
        let bare = settings(&[("802-3-ethernet", &[])]);
        for reply in [unnamed, bare, ("x",).to_variant(), 3u32.to_variant()] {
            assert_eq!(saved_profile("/s/4".into(), &reply), None, "{reply}");
        }
    }

    #[test]
    fn active_connection_reads_profile_and_state() {
        let props = glib::VariantDict::new(None);
        props.insert_value("Connection", &opath("/s/1"));
        props.insert_value("State", &1u32.to_variant());
        assert_eq!(active_connection(&props), ("/s/1".into(), 1));
    }

    #[test]
    fn nm_device_states_decide_visibility_like_gnome() {
        // NMDeviceState: unmanaged and unavailable (no cable) are hidden,
        // every state from disconnected to failed is listed.
        let shown = |state| {
            WiredDevice {
                state,
                ..WiredDevice::default()
            }
            .shown()
        };
        for state in [0, 10, 20, 25, 130] {
            assert!(!shown(state), "{state}");
        }
        for state in [30, 40, 50, 60, 70, 80, 90, 100, 110, 120] {
            assert!(shown(state), "{state}");
        }
    }

    #[test]
    fn active_connection_states_drive_icon_and_click() {
        let mut dev = WiredDevice {
            path: "/d/1".into(),
            state: 100,
            connections: vec![conn("/s/1", "Wired connection 1", 5)],
            active: Some(("/s/1".into(), 1)),
            primary: true,
            ..WiredDevice::default()
        };
        assert_eq!(dev.icon(4), "network-wired-acquiring-symbolic");
        assert!(dev.is_active());
        dev.active = Some(("/s/1".into(), 2));
        assert_eq!(dev.icon(4), "network-wired-symbolic");
        assert_eq!(
            dev.icon(2),
            "network-wired-no-route-symbolic",
            "portal/limited"
        );
        dev.primary = false;
        assert_eq!(
            dev.icon(2),
            "network-wired-symbolic",
            "not the default route"
        );
        // Deactivating (3) and deactivated (4) read as off, but a click
        // still disconnects what NetworkManager holds.
        for state in [3, 4] {
            dev.active = Some(("/s/1".into(), state));
            assert!(!dev.is_active());
            assert_eq!(dev.icon(4), "network-wired-disconnected-symbolic");
            assert_eq!(dev.click(), WiredAction::Disconnect);
        }
    }

    #[test]
    fn profile_selection_prefers_the_most_recent_then_the_first_by_name() {
        let mut dev = WiredDevice {
            path: "/d/1".into(),
            state: 30,
            connections: vec![
                conn("/s/1", "Zulu", 0),
                conn("/s/2", "Home", 10),
                conn("/s/3", "Office", 20),
            ],
            ..WiredDevice::default()
        };
        assert_eq!(dev.click(), WiredAction::Activate("/s/3".into()));
        // None ever used: the first by name.
        for c in &mut dev.connections {
            c.timestamp = 0;
        }
        assert_eq!(dev.click(), WiredAction::Activate("/s/2".into()));
        dev.connections.clear();
        assert_eq!(dev.click(), WiredAction::AutoConnect);
    }

    #[test]
    fn several_adapters_are_told_apart_and_the_toggle_acts_on_all() {
        let usb = WiredDevice {
            path: "/org/freedesktop/NetworkManager/Devices/7".into(),
            state: 100,
            connections: vec![conn("/s/2", "Dock", 50)],
            active: Some(("/s/2".into(), 2)),
            ..WiredDevice::default()
        };
        let builtin = WiredDevice {
            path: "/org/freedesktop/NetworkManager/Devices/3".into(),
            interface: "enp3s0".into(),
            state: 100,
            connections: vec![conn("/s/1", "Office", 90)],
            active: Some(("/s/1".into(), 2)),
            primary: true,
            ..WiredDevice::default()
        };
        assert_eq!(submenu_label(&builtin), "Wired (enp3s0)");
        assert_eq!(
            submenu_label(&usb),
            "Wired (7)",
            "no interface: the path's tail"
        );
        let devices = [usb.clone(), builtin.clone()];
        let toggle = logic::wired_toggle(&devices, 4);
        assert!(toggle.visible && toggle.checked);
        assert_eq!(toggle.subtitle.as_deref(), Some("2 connected"));
        // The toggle turns every active adapter off.
        assert_eq!(
            logic::wired_click(&devices),
            [
                (usb.path.clone(), WiredAction::Disconnect),
                (builtin.path.clone(), WiredAction::Disconnect)
            ]
        );
        // Both down: the most recently used adapter comes up.
        let down: Vec<WiredDevice> = devices
            .iter()
            .cloned()
            .map(|mut d| {
                d.active = None;
                d.state = 30;
                d
            })
            .collect();
        assert_eq!(
            logic::wired_click(&down),
            [(builtin.path.clone(), WiredAction::Activate("/s/1".into()))]
        );
        // An unplugged adapter is not counted or toggled.
        let mut unplugged = down.clone();
        unplugged[1].state = 20;
        assert_eq!(
            logic::wired_click(&unplugged),
            [(usb.path.clone(), WiredAction::Activate("/s/2".into()))]
        );
    }

    #[test]
    fn countdown_fires_once_after_every_tick() {
        let fired = Rc::new(Cell::new(0));
        let counter = fired.clone();
        let done = Countdown::new(3, move || counter.set(counter.get() + 1));
        done.tick();
        done.tick();
        assert_eq!(fired.get(), 0, "not before every reply is in");
        done.tick();
        assert_eq!(fired.get(), 1);
        // Late or duplicate replies do not rebuild twice.
        done.tick();
        done.tick();
        assert_eq!(fired.get(), 1);
    }
}
