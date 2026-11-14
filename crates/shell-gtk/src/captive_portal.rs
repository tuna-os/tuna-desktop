//! GNOME's captive-portal sign-in (network.js 51.0 `CaptivePortalHandler`
//! and the indicator's `_syncConnectivity`, lines 38-45 and 1987-2251).
//!
//! When NetworkManager's `Connectivity` is PORTAL for the main connection
//! (`PrimaryConnection`, else `ActivatingConnection`) and that connection
//! is ACTIVATED, the shell posts a "Sign in to Network" notification
//! whose body is the connection's name, once per connection path.
//! Activating it calls `org.gnome.Shell.PortalHelper.Authenticate(path,
//! ConnectivityCheckUri, timestamp)` on the session bus and queues the
//! path. Any other state (no main connection, not activated, or a
//! connectivity other than PORTAL) clears: every queued path gets
//! `Close(path)` and every offer is withdrawn. A second portal connection
//! is added beside the first; GNOME does not close the first then. The
//! helper's `StatusChanged(path, result)`: CANCELLED (0) keeps the path
//! queued so no new offer appears until the queue is flushed, COMPLETED
//! (1) removes the connection (Close plus withdrawal), RECHECK (2) asks
//! NetworkManager's `CheckConnectivity` and removes the connection when
//! it answers FULL; any other code is logged. `GNOME_SHELL_CONNECTIVITY_TEST`
//! treats any connectivity below FULL as PORTAL, as in GNOME. GNOME 51
//! consults no setting here (not `ConnectivityCheckEnabled` either).
//!
//! Divergences: GNOME built with the helper posts the offer and fails at
//! launch when the helper is missing (built without it, it opens the
//! check URI in a browser); Tuna asks the session bus whether
//! `org.gnome.Shell.PortalHelper` is activatable before offering, and
//! when it is not, logs once and does nothing else. The shell runs only
//! in the user session, so GNOME's greeter check has no counterpart. The
//! notification store does not report a user's dismissal, so a dismissed
//! offer counts as live until the connection is removed or cleared.
//! Activation does not hide the overview or close the calendar. The
//! timestamp is the monotonic clock in milliseconds (as `providers.rs`),
//! not a compositor event time. NetworkManager's `PropertiesChanged`
//! signals are coalesced for 250 ms (as `wired.rs`), and the sync runs
//! when connectivity, the main connection or its state changed: the
//! properties GNOME's `notify::` handlers follow.
//!
//! The decisions are pure ([`Handler`], [`sync_connectivity`]); the
//! runtime feeds them from D-Bus and carries out their [`Effect`]s with
//! asynchronous calls only.

/// `NM_CONNECTIVITY_PORTAL`.
pub const CONNECTIVITY_PORTAL: u32 = 2;
/// `NM_CONNECTIVITY_FULL`.
pub const CONNECTIVITY_FULL: u32 = 4;
/// `NM_ACTIVE_CONNECTION_STATE_ACTIVATED`.
pub const ACTIVATED: u32 = 2;

/// The indicator's `_mainConnection`: the active connection's object
/// path, its `Id` and `NMActiveConnectionState`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MainConnection {
    pub path: String,
    pub id: String,
    pub state: u32,
}

/// What `_syncConnectivity` reads from NetworkManager.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NmSnapshot {
    pub connectivity: u32,
    pub main: Option<MainConnection>,
}

/// `_syncMainConnection`: the primary connection, else the activating one.
pub fn main_connection(primary: Option<String>, activating: Option<String>) -> Option<String> {
    primary.or(activating)
}

/// The portal half of `_syncConnectivity`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortalSync {
    /// `addConnection(name, path)`.
    Add { name: String, path: String },
    /// `clear()`.
    Clear,
}

/// `_syncConnectivity`: offer sign-in for an activated main connection
/// behind a portal; anything else clears. `test` is
/// `GNOME_SHELL_CONNECTIVITY_TEST`: any connectivity below FULL counts.
pub fn sync_connectivity(nm: &NmSnapshot, test: bool) -> PortalSync {
    let Some(main) = nm.main.as_ref().filter(|m| m.state == ACTIVATED) else {
        return PortalSync::Clear;
    };
    let portal =
        nm.connectivity == CONNECTIVITY_PORTAL || (test && nm.connectivity < CONNECTIVITY_FULL);
    if portal {
        PortalSync::Add {
            name: main.id.clone(),
            path: main.path.clone(),
        }
    } else {
        PortalSync::Clear
    }
}

/// network.js `PortalHelperResult`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortalResult {
    Cancelled,
    Completed,
    Recheck,
}

impl PortalResult {
    /// `None` for a code GNOME logs as invalid.
    pub fn from_code(code: u32) -> Option<Self> {
        match code {
            0 => Some(Self::Cancelled),
            1 => Some(Self::Completed),
            2 => Some(Self::Recheck),
            _ => None,
        }
    }
}

/// What the runtime does for a decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Post "Sign in to Network" for `path` (after checking the helper
    /// is installed), its body `name`.
    Offer { name: String, path: String },
    /// Withdraw the posted offer with this notification id.
    Withdraw(u64),
    /// `PortalHelper.Authenticate(path, check URI, timestamp)`.
    Authenticate(String),
    /// `PortalHelper.Close(path)`.
    Close(String),
    /// The `recheck` signal: NetworkManager `CheckConnectivity`, then
    /// [`Handler::recheck_done`] with its answer.
    CheckConnectivity(String),
}

/// `CaptivePortalHandler`'s `_connectivityQueue` and `_notifications`.
#[derive(Debug, Default)]
pub struct Handler {
    /// Paths handed to the helper, in insertion order (a JS `Set`).
    queue: Vec<String>,
    /// Offered paths and their notification id (`None` until posted).
    notifications: Vec<(String, Option<u64>)>,
    /// The connectivity and main connection the last sync ran for.
    last: Option<(u32, Option<(String, u32)>)>,
}

impl Handler {
    /// NetworkManager changed: run `_syncConnectivity` when one of the
    /// properties GNOME follows (connectivity, the main connection, its
    /// state) differs from the last sync.
    pub fn update(&mut self, nm: &NmSnapshot, test: bool) -> Vec<Effect> {
        let key = (
            nm.connectivity,
            nm.main.as_ref().map(|m| (m.path.clone(), m.state)),
        );
        if self.last.as_ref() == Some(&key) {
            return Vec::new();
        }
        self.last = Some(key);
        match sync_connectivity(nm, test) {
            PortalSync::Add { name, path } => self.add_connection(name, path),
            PortalSync::Clear => self.clear(),
        }
    }

    /// `addConnection`: one offer per path not already queued or offered.
    pub fn add_connection(&mut self, name: String, path: String) -> Vec<Effect> {
        if self.queue.contains(&path) || self.offer(&path).is_some() {
            return Vec::new();
        }
        self.notifications.push((path.clone(), None));
        vec![Effect::Offer { name, path }]
    }

    /// Whether `path`'s offer is still wanted and not yet posted.
    pub fn wants_offer(&self, path: &str) -> bool {
        self.offer(path) == Some(None)
    }

    /// `path`'s offer was posted as notification `id`.
    pub fn offered(&mut self, path: &str, id: u64) {
        if let Some((_, slot)) = self.notifications.iter_mut().find(|(p, _)| p == path) {
            *slot = Some(id);
        }
    }

    /// The helper is not installed: forget the unposted offer.
    pub fn drop_offer(&mut self, path: &str) {
        self.notifications
            .retain(|(p, id)| p != path || id.is_some());
    }

    /// `_onNotificationActivated` with the helper: the activated
    /// notification is destroyed, the helper authenticates and the path
    /// joins the queue.
    pub fn activated(&mut self, path: &str) -> Vec<Effect> {
        self.notifications.retain(|(p, _)| p != path);
        if !self.queue.iter().any(|p| p == path) {
            self.queue.push(path.to_owned());
        }
        vec![Effect::Authenticate(path.to_owned())]
    }

    /// `_portalHelperStatusChanged`.
    pub fn status_changed(&mut self, path: &str, result: PortalResult) -> Vec<Effect> {
        match result {
            // Kept queued: no new offer until the queue is flushed.
            PortalResult::Cancelled => Vec::new(),
            PortalResult::Completed => self.remove_connection(path),
            PortalResult::Recheck => vec![Effect::CheckConnectivity(path.to_owned())],
        }
    }

    /// The `recheck` handler: NetworkManager answered `connectivity`.
    pub fn recheck_done(&mut self, path: &str, connectivity: u32) -> Vec<Effect> {
        if connectivity >= CONNECTIVITY_FULL {
            self.remove_connection(path)
        } else {
            Vec::new()
        }
    }

    /// `removeConnection`: close the helper for a queued path and
    /// withdraw its offer.
    pub fn remove_connection(&mut self, path: &str) -> Vec<Effect> {
        let mut effects = Vec::new();
        if let Some(i) = self.queue.iter().position(|p| p == path) {
            self.queue.remove(i);
            effects.push(Effect::Close(path.to_owned()));
        }
        if let Some(i) = self.notifications.iter().position(|(p, _)| p == path) {
            if let (_, Some(id)) = self.notifications.remove(i) {
                effects.push(Effect::Withdraw(id));
            }
        }
        effects
    }

    /// `clear`: close every queued path, withdraw every offer.
    pub fn clear(&mut self) -> Vec<Effect> {
        let mut effects: Vec<Effect> = self.queue.drain(..).map(Effect::Close).collect();
        effects.extend(
            self.notifications
                .drain(..)
                .filter_map(|(_, id)| id.map(Effect::Withdraw)),
        );
        effects
    }

    fn offer(&self, path: &str) -> Option<Option<u64>> {
        self.notifications
            .iter()
            .find(|(p, _)| p == path)
            .map(|(_, id)| *id)
    }
}

pub use runtime::start;

mod runtime {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    use gio::prelude::*;

    use crate::notify::NotifyUi;
    use tuna_shell_host::notifications::Urgency;

    const NM: &str = "org.freedesktop.NetworkManager";
    const NM_PATH: &str = "/org/freedesktop/NetworkManager";
    const PROPS: &str = "org.freedesktop.DBus.Properties";
    const ACTIVE: &str = "org.freedesktop.NetworkManager.Connection.Active";
    const HELPER_NAME: &str = "org.gnome.Shell.PortalHelper";
    const HELPER_PATH: &str = "/org/gnome/Shell/PortalHelper";
    const HELPER_IFACE: &str = "org.gnome.Shell.PortalHelper";
    const DBUS: &str = "org.freedesktop.DBus";
    const DBUS_PATH: &str = "/org/freedesktop/DBus";
    /// GNOME posts through `MessageTray.getSystemSource()`.
    const SYSTEM_SOURCE: &str = "System";
    const SYSTEM_ICON: &str = "cog-wheel-symbolic";
    const TEST_ENV: &str = "GNOME_SHELL_CONNECTIVITY_TEST";

    struct CaptivePortal {
        notify: Rc<NotifyUi>,
        handler: RefCell<Handler>,
        system: RefCell<Option<gio::DBusConnection>>,
        session: RefCell<Option<gio::DBusConnection>>,
        /// NetworkManager's `ConnectivityCheckUri`.
        check_uri: RefCell<String>,
        /// Bumped on every reload and when NetworkManager leaves, so a
        /// slower earlier read cannot win.
        generation: Cell<u64>,
        pending: Cell<bool>,
        /// `StatusChanged` is subscribed (GNOME's proxy exists).
        helper_watched: Cell<bool>,
        absent_logged: Cell<bool>,
        test: bool,
    }

    /// Follow NetworkManager on the system bus and offer portal sign-in
    /// through `notify`.
    pub fn start(notify: Rc<NotifyUi>) {
        let me = Rc::new(CaptivePortal {
            notify,
            handler: RefCell::default(),
            system: RefCell::default(),
            session: RefCell::default(),
            check_uri: RefCell::default(),
            generation: Cell::new(0),
            pending: Cell::new(false),
            helper_watched: Cell::new(false),
            absent_logged: Cell::new(false),
            test: std::env::var_os(TEST_ENV).is_some(),
        });
        {
            let me = me.clone();
            gio::bus_get(
                gio::BusType::Session,
                gio::Cancellable::NONE,
                move |conn| match conn {
                    Ok(conn) => *me.session.borrow_mut() = Some(conn),
                    Err(e) => eprintln!("tuna-shell-gtk: captive portal: no session bus: {e}"),
                },
            );
        }
        gio::bus_get(gio::BusType::System, gio::Cancellable::NONE, move |conn| {
            let conn = match conn {
                Ok(conn) => conn,
                Err(e) => {
                    eprintln!("tuna-shell-gtk: captive portal: no system bus: {e}");
                    return;
                }
            };
            *me.system.borrow_mut() = Some(conn.clone());
            // The manager's own properties, and every active connection's
            // (the main connection's State).
            for (path, iface) in [(Some(NM_PATH), NM), (None, ACTIVE)] {
                let me = me.clone();
                let sub = conn.subscribe_to_signal(
                    Some(NM),
                    Some(PROPS),
                    Some("PropertiesChanged"),
                    path,
                    Some(iface),
                    gio::DBusSignalFlags::NONE,
                    move |_| me.schedule_reload(),
                );
                // The subscriptions live as long as the shell.
                std::mem::forget(sub);
            }
            crate::services::watch_name(&conn, NM, move |owned| {
                if owned {
                    me.schedule_reload();
                } else {
                    // No client: no main connection, so clear.
                    me.generation.set(me.generation.get() + 1);
                    me.apply(NmSnapshot::default());
                }
            });
        });
    }

    impl CaptivePortal {
        fn schedule_reload(self: &Rc<Self>) {
            if self.pending.replace(true) {
                return;
            }
            let me = self.clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(250), move || {
                me.pending.set(false);
                me.reload();
            });
        }

        /// Read connectivity, the check URI and the main connection.
        fn reload(self: &Rc<Self>) {
            let Some(conn) = self.system.borrow().clone() else {
                return;
            };
            let generation = self.generation.get() + 1;
            self.generation.set(generation);
            let me = self.clone();
            let conn2 = conn.clone();
            get_all(&conn, NM_PATH, NM, move |nm| {
                let Some(nm) = nm else { return };
                if me.generation.get() != generation {
                    return;
                }
                let connectivity = nm
                    .lookup_value("Connectivity", None)
                    .and_then(|v| v.get::<u32>())
                    .unwrap_or(0);
                if let Some(uri) = nm
                    .lookup_value("ConnectivityCheckUri", None)
                    .and_then(|v| v.get::<String>())
                {
                    *me.check_uri.borrow_mut() = uri;
                }
                let Some(path) = main_connection(
                    object_path(&nm, "PrimaryConnection"),
                    object_path(&nm, "ActivatingConnection"),
                ) else {
                    me.apply(NmSnapshot {
                        connectivity,
                        main: None,
                    });
                    return;
                };
                let object = path.clone();
                get_all(&conn2, &object, ACTIVE, move |active| {
                    if me.generation.get() != generation {
                        return;
                    }
                    let main = active.map(|a| MainConnection {
                        id: a
                            .lookup_value("Id", None)
                            .and_then(|v| v.get::<String>())
                            .unwrap_or_default(),
                        state: a
                            .lookup_value("State", None)
                            .and_then(|v| v.get::<u32>())
                            .unwrap_or(0),
                        path,
                    });
                    me.apply(NmSnapshot { connectivity, main });
                });
            });
        }

        fn apply(self: &Rc<Self>, nm: NmSnapshot) {
            let effects = self.handler.borrow_mut().update(&nm, self.test);
            self.run(effects);
        }

        fn run(self: &Rc<Self>, effects: Vec<Effect>) {
            for effect in effects {
                match effect {
                    Effect::Offer { name, path } => self.offer(name, path),
                    Effect::Withdraw(id) => self.notify.remove_notification(id),
                    Effect::Authenticate(path) => self.authenticate(&path),
                    Effect::Close(path) => self.close(&path),
                    Effect::CheckConnectivity(path) => self.check_connectivity(path),
                }
            }
        }

        /// Post "Sign in to Network" when the helper is installed.
        fn offer(self: &Rc<Self>, name: String, path: String) {
            let Some(session) = self.session.borrow().clone() else {
                self.handler.borrow_mut().drop_offer(&path);
                return;
            };
            let me = self.clone();
            session.call(
                Some(DBUS),
                DBUS_PATH,
                DBUS,
                "ListActivatableNames",
                None,
                glib::VariantTy::new("(as)").ok(),
                gio::DBusCallFlags::NONE,
                5000,
                None::<&gio::Cancellable>,
                move |reply| {
                    let installed = reply
                        .ok()
                        .and_then(|v| v.child_value(0).get::<Vec<String>>())
                        .is_some_and(|names| names.iter().any(|n| n == HELPER_NAME));
                    if !installed {
                        me.handler.borrow_mut().drop_offer(&path);
                        if !me.absent_logged.replace(true) {
                            eprintln!(
                                "tuna-shell-gtk: captive portal: {HELPER_NAME} is not installed \
                                 (it ships with gnome-shell); no sign-in offered"
                            );
                        }
                        return;
                    }
                    if !me.handler.borrow().wants_offer(&path) {
                        return;
                    }
                    let actions = [("default".to_owned(), String::new())];
                    let Some(id) = me.notify.post_with_actions(
                        SYSTEM_SOURCE,
                        SYSTEM_ICON,
                        "Sign in to Network",
                        &name,
                        &actions,
                        Urgency::Normal,
                        None,
                    ) else {
                        me.handler.borrow_mut().drop_offer(&path);
                        return;
                    };
                    me.handler.borrow_mut().offered(&path, id);
                    let weak = Rc::downgrade(&me);
                    // Runs inside the notification store's dispatch, so it
                    // must not call back into it: activation only yields
                    // Authenticate.
                    me.notify.on_notification_action(
                        id,
                        "default",
                        Rc::new(move || {
                            if let Some(me) = weak.upgrade() {
                                let effects = me.handler.borrow_mut().activated(&path);
                                me.run(effects);
                            }
                        }),
                    );
                },
            );
        }

        /// `_launchPortalHelper`: subscribe to `StatusChanged` the first
        /// time, then `Authenticate`.
        fn authenticate(self: &Rc<Self>, path: &str) {
            let Some(session) = self.session.borrow().clone() else {
                eprintln!("tuna-shell-gtk: captive portal: no session bus for the portal helper");
                return;
            };
            if !self.helper_watched.replace(true) {
                let weak = Rc::downgrade(self);
                let sub = session.subscribe_to_signal(
                    Some(HELPER_NAME),
                    Some(HELPER_IFACE),
                    Some("StatusChanged"),
                    Some(HELPER_PATH),
                    None,
                    gio::DBusSignalFlags::NONE,
                    move |signal| {
                        let Some(me) = weak.upgrade() else { return };
                        let params = signal.parameters;
                        let path = params
                            .try_child_value(0)
                            .and_then(|v| v.get::<glib::variant::ObjectPath>());
                        let code = params.try_child_value(1).and_then(|v| v.get::<u32>());
                        let (Some(path), Some(code)) = (path, code) else {
                            return;
                        };
                        let Some(result) = PortalResult::from_code(code) else {
                            eprintln!("tuna-shell-gtk: invalid result from portal helper: {code}");
                            return;
                        };
                        let effects = me
                            .handler
                            .borrow_mut()
                            .status_changed(path.as_str(), result);
                        me.run(effects);
                    },
                );
                // The subscription lives as long as the shell.
                std::mem::forget(sub);
            }
            let Ok(object) = glib::variant::ObjectPath::try_from(path.to_owned()) else {
                return;
            };
            let args = (object, self.check_uri.borrow().clone(), timestamp()).to_variant();
            helper_call(&session, "Authenticate", args);
        }

        fn close(&self, path: &str) {
            let Some(session) = self.session.borrow().clone() else {
                return;
            };
            if let Ok(object) = glib::variant::ObjectPath::try_from(path.to_owned()) {
                helper_call(&session, "Close", (object,).to_variant());
            }
        }

        /// The `recheck` handler: `check_connectivity_async`, errors ignored.
        fn check_connectivity(self: &Rc<Self>, path: String) {
            let Some(conn) = self.system.borrow().clone() else {
                return;
            };
            let me = self.clone();
            conn.call(
                Some(NM),
                NM_PATH,
                NM,
                "CheckConnectivity",
                None,
                glib::VariantTy::new("(u)").ok(),
                gio::DBusCallFlags::NONE,
                -1,
                None::<&gio::Cancellable>,
                move |reply| {
                    let Some(state) = reply.ok().and_then(|v| v.child_value(0).get::<u32>()) else {
                        return;
                    };
                    let effects = me.handler.borrow_mut().recheck_done(&path, state);
                    me.run(effects);
                },
            );
        }
    }

    /// One `org.gnome.Shell.PortalHelper` method; failures are logged
    /// (GNOME's `.catch(logError)`).
    fn helper_call(session: &gio::DBusConnection, method: &'static str, args: glib::Variant) {
        session.call(
            Some(HELPER_NAME),
            HELPER_PATH,
            HELPER_IFACE,
            method,
            Some(&args),
            None,
            gio::DBusCallFlags::NONE,
            -1,
            None::<&gio::Cancellable>,
            move |reply| {
                if let Err(e) = reply {
                    eprintln!("tuna-shell-gtk: portal helper {method}: {e}");
                }
            },
        );
    }

    fn timestamp() -> u32 {
        (glib::monotonic_time() / 1000) as u32
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
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIFI: &str = "/org/freedesktop/NetworkManager/ActiveConnection/3";
    const WIRED: &str = "/org/freedesktop/NetworkManager/ActiveConnection/4";
    const NONE: [Effect; 0] = [];

    fn nm(connectivity: u32, path: &str, state: u32) -> NmSnapshot {
        NmSnapshot {
            connectivity,
            main: Some(MainConnection {
                path: path.to_owned(),
                id: "Cafe".to_owned(),
                state,
            }),
        }
    }

    fn offer(path: &str) -> Effect {
        Effect::Offer {
            name: "Cafe".to_owned(),
            path: path.to_owned(),
        }
    }

    /// A handler whose offer for `path` was posted as notification 7.
    fn offered(path: &str) -> Handler {
        let mut h = Handler::default();
        assert_eq!(
            h.update(&nm(CONNECTIVITY_PORTAL, path, ACTIVATED), false),
            [offer(path)]
        );
        assert!(h.wants_offer(path));
        h.offered(path, 7);
        h
    }

    #[test]
    fn portal_state_opens_the_helper_for_the_primary_connection() {
        let mut h = offered(WIFI);
        assert!(!h.wants_offer(WIFI));
        // The same state again offers nothing more.
        assert_eq!(
            h.update(&nm(CONNECTIVITY_PORTAL, WIFI, ACTIVATED), false),
            NONE
        );
        // Activating the offer authenticates that connection.
        assert_eq!(h.activated(WIFI), [Effect::Authenticate(WIFI.to_owned())]);
        // Queued: no second offer for it.
        assert_eq!(h.add_connection("Cafe".to_owned(), WIFI.to_owned()), NONE);
    }

    #[test]
    fn primary_connection_wins_over_the_activating_one() {
        assert_eq!(
            main_connection(Some(WIFI.to_owned()), Some(WIRED.to_owned())),
            Some(WIFI.to_owned())
        );
        assert_eq!(
            main_connection(None, Some(WIRED.to_owned())),
            Some(WIRED.to_owned())
        );
        assert_eq!(main_connection(None, None), None);
    }

    #[test]
    fn only_an_activated_main_connection_behind_a_portal_is_offered() {
        // Still activating (1), limited (3), or no main connection: clear.
        assert_eq!(
            sync_connectivity(&nm(CONNECTIVITY_PORTAL, WIFI, 1), false),
            PortalSync::Clear
        );
        assert_eq!(
            sync_connectivity(&nm(3, WIFI, ACTIVATED), false),
            PortalSync::Clear
        );
        assert_eq!(
            sync_connectivity(
                &NmSnapshot {
                    connectivity: CONNECTIVITY_PORTAL,
                    main: None
                },
                false
            ),
            PortalSync::Clear
        );
        // The test variable counts LIMITED as a portal, never FULL.
        assert_eq!(
            sync_connectivity(&nm(3, WIFI, ACTIVATED), true),
            PortalSync::Add {
                name: "Cafe".to_owned(),
                path: WIFI.to_owned()
            }
        );
        assert_eq!(
            sync_connectivity(&nm(CONNECTIVITY_FULL, WIFI, ACTIVATED), true),
            PortalSync::Clear
        );
    }

    #[test]
    fn full_connectivity_closes_an_open_helper() {
        let mut h = offered(WIFI);
        h.activated(WIFI);
        assert_eq!(
            h.update(&nm(CONNECTIVITY_FULL, WIFI, ACTIVATED), false),
            [Effect::Close(WIFI.to_owned())]
        );
        // Back behind a portal, it is offered afresh.
        assert_eq!(
            h.update(&nm(CONNECTIVITY_PORTAL, WIFI, ACTIVATED), false),
            [offer(WIFI)]
        );
    }

    #[test]
    fn full_connectivity_withdraws_an_unanswered_offer() {
        let mut h = offered(WIFI);
        assert_eq!(
            h.update(&nm(CONNECTIVITY_FULL, WIFI, ACTIVATED), false),
            [Effect::Withdraw(7)]
        );
    }

    #[test]
    fn losing_the_main_connection_closes_the_helper() {
        let mut h = offered(WIFI);
        h.activated(WIFI);
        assert_eq!(
            h.update(&NmSnapshot::default(), false),
            [Effect::Close(WIFI.to_owned())]
        );
        let mut h = offered(WIFI);
        h.activated(WIFI);
        // Deactivating (3) clears as well.
        assert_eq!(
            h.update(&nm(CONNECTIVITY_PORTAL, WIFI, 3), false),
            [Effect::Close(WIFI.to_owned())]
        );
    }

    #[test]
    fn a_second_portal_connection_is_added_beside_the_first() {
        let mut h = offered(WIFI);
        h.activated(WIFI);
        // GNOME closes nothing when the main connection moves to another
        // portal connection.
        assert_eq!(
            h.update(&nm(CONNECTIVITY_PORTAL, WIRED, ACTIVATED), false),
            [offer(WIRED)]
        );
        h.offered(WIRED, 8);
        assert_eq!(
            h.clear(),
            [Effect::Close(WIFI.to_owned()), Effect::Withdraw(8)]
        );
    }

    #[test]
    fn done_with_retry_rechecks_connectivity() {
        let mut h = offered(WIFI);
        h.activated(WIFI);
        assert_eq!(
            h.status_changed(WIFI, PortalResult::Recheck),
            [Effect::CheckConnectivity(WIFI.to_owned())]
        );
        // Still behind the portal: the helper stays.
        assert_eq!(h.recheck_done(WIFI, CONNECTIVITY_PORTAL), NONE);
        assert_eq!(
            h.recheck_done(WIFI, CONNECTIVITY_FULL),
            [Effect::Close(WIFI.to_owned())]
        );
        // Removed once: a later FULL answer has nothing to close.
        assert_eq!(h.recheck_done(WIFI, CONNECTIVITY_FULL), NONE);
    }

    #[test]
    fn completed_closes_the_helper_and_cancelled_keeps_it_queued() {
        let mut h = offered(WIFI);
        h.activated(WIFI);
        assert_eq!(h.status_changed(WIFI, PortalResult::Cancelled), NONE);
        // Cancelled stays queued: no new offer for it.
        assert_eq!(h.add_connection("Cafe".to_owned(), WIFI.to_owned()), NONE);
        assert_eq!(
            h.status_changed(WIFI, PortalResult::Completed),
            [Effect::Close(WIFI.to_owned())]
        );
        // Removed: the next portal sync offers it again.
        assert_eq!(
            h.add_connection("Cafe".to_owned(), WIFI.to_owned()),
            [offer(WIFI)]
        );
    }

    #[test]
    fn portal_helper_result_codes_follow_gnome() {
        assert_eq!(PortalResult::from_code(0), Some(PortalResult::Cancelled));
        assert_eq!(PortalResult::from_code(1), Some(PortalResult::Completed));
        assert_eq!(PortalResult::from_code(2), Some(PortalResult::Recheck));
        assert_eq!(PortalResult::from_code(3), None);
    }

    #[test]
    fn a_missing_helper_offers_nothing_until_the_state_changes() {
        let mut h = Handler::default();
        assert_eq!(
            h.update(&nm(CONNECTIVITY_PORTAL, WIFI, ACTIVATED), false),
            [offer(WIFI)]
        );
        h.drop_offer(WIFI);
        assert!(!h.wants_offer(WIFI));
        // The same state asks nothing more; clearing has nothing to undo.
        assert_eq!(
            h.update(&nm(CONNECTIVITY_PORTAL, WIFI, ACTIVATED), false),
            NONE
        );
        assert_eq!(
            h.update(&nm(CONNECTIVITY_FULL, WIFI, ACTIVATED), false),
            NONE
        );
    }

    #[test]
    fn an_offer_cleared_before_posting_is_not_posted() {
        let mut h = Handler::default();
        h.update(&nm(CONNECTIVITY_PORTAL, WIFI, ACTIVATED), false);
        // FULL arrives while the helper check is in flight.
        assert_eq!(
            h.update(&nm(CONNECTIVITY_FULL, WIFI, ACTIVATED), false),
            NONE
        );
        assert!(!h.wants_offer(WIFI));
    }
}
