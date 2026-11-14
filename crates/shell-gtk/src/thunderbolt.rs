//! Thunderbolt: GNOME Shell 51's `js/ui/status/thunderbolt.js` (tag 51.0)
//! over boltd (`org.freedesktop.bolt`, interfaces as in gnome-shell's
//! `data/dbus-interfaces/org.freedesktop.bolt1.Manager.xml` and
//! `org.freedesktop.bolt1.Device.xml`).
//!
//! - `Client`: a proxy for `org.freedesktop.bolt1.Manager` at
//!   `/org/freedesktop/bolt` on the system bus, created with
//!   `DO_NOT_AUTO_START`. Without boltd on the bus the proxy simply has no
//!   owner: no `DeviceAdded` arrives, `Probing` reads false and nothing is
//!   shown, posted or logged. GDBus follows the name, so a boltd started
//!   later (udev starts `bolt.service` when Thunderbolt hardware appears)
//!   is picked up.
//! - `AuthRobot`: boltd emits `DeviceAdded` for every device it has not
//!   stored. A device whose `Status` is `connected`, while the manager's
//!   `AuthMode` (a `|`-separated flag list) contains `enabled`, goes to
//!   `_onEnrollDevice`; an approved one joins a queue that enrols one
//!   device per idle callback with `EnrollDevice(uid, "default", "none")`.
//!   After an `EnrollDevice` error `_enrollDevicesIdle` leaves `_enrolling`
//!   set and clears nothing: later approved devices only join the queue,
//!   no idle run is scheduled again, and neither a new `DeviceAdded` nor
//!   locking or unlocking restarts it; only a new robot (a shell restart)
//!   does. Notices for devices it would not enrol are still posted.
//! - `Indicator._onEnrollDevice`: enrol only in an unlocked user session
//!   when the `org.freedesktop.bolt.enroll` polkit permission is already
//!   allowed (never trigger a polkit dialog). Otherwise post "Unknown
//!   Thunderbolt device" while locked, or "Unauthorized Thunderbolt
//!   device" while unlocked; an `EnrollDevice` error posts "Thunderbolt
//!   authorization error" with the stripped D-Bus message. One notice at a
//!   time (a new one destroys the last), from the System source at high
//!   urgency; activating it opens `gnome-thunderbolt-panel.desktop` when
//!   that app is installed.
//! - The panel indicator shows `thunderbolt-acquiring-symbolic` while
//!   boltd is `Probing` and the session is unlocked (`_sync`, `_onProbing`).
//!
//! Divergences, stated plainly:
//! - The polkit permission (GNOME's `Polkit.Permission` for the shell
//!   process) is asked of the authority with a `system-bus-name` subject
//!   for this shell's system-bus connection (polkit resolves it to this
//!   process), re-checked on the authority's `Changed` signal as
//!   `PolkitPermission` does.
//! - `Probing` is re-read from the proxy cache on every property change,
//!   including invalidation when boltd leaves the bus (upstream looks only
//!   at the changed dictionary, keeping a stale `true`).
//! - The session mode is the compositor's lock state; until it is known
//!   the session counts as locked, so nothing is enrolled before the shell
//!   knows the user is present. There is no greeter mode in this shell.
//!
//! The decisions ([`device_added`], [`EnrollQueue`], [`SessionLock`],
//! [`indicator_icon`], [`Notice`]) are pure; [`Thunderbolt`] feeds them
//! from boltd and polkit and carries them out on the GLib main loop
//! without blocking.

use std::collections::VecDeque;

/// `Status.CONNECTED`.
pub const STATUS_CONNECTED: &str = "connected";
/// `AuthMode.ENABLED`.
pub const AUTH_MODE_ENABLED: &str = "enabled";
/// `Policy.DEFAULT`: boltd applies its configured default policy.
pub const POLICY_DEFAULT: &str = "default";
/// `AuthCtrl.NONE`: `EnrollDevice`'s flags.
pub const AUTH_CTRL_NONE: &str = "none";
/// The polkit action `_createPermission` asks for.
pub const ENROLL_ACTION: &str = "org.freedesktop.bolt.enroll";
/// The indicator's and notifications' icon.
pub const ICON: &str = "thunderbolt-symbolic";
/// `_onProbing(true)`.
pub const ACQUIRING_ICON: &str = "thunderbolt-acquiring-symbolic";
/// The app a notice opens (`_notify`'s `activated`).
pub const PANEL_APP: &str = "gnome-thunderbolt-panel.desktop";
/// "Unknown Thunderbolt device"'s body.
pub const UNKNOWN_BODY: &str = "New device has been detected while you were away. Please disconnect and reconnect the device to start using it.";
/// "Unauthorized Thunderbolt device"'s body.
pub const UNAUTHORIZED_BODY: &str =
    "New device has been detected and needs to be authorized by an administrator";

/// The notices `Indicator` posts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notice {
    /// A new device while the session is locked.
    UnknownWhileAway,
    /// A new device the user may not enrol.
    Unauthorized,
    /// `EnrollDevice` failed with this message.
    AuthError(String),
}

impl Notice {
    pub fn title(&self) -> &'static str {
        match self {
            Self::UnknownWhileAway => "Unknown Thunderbolt device",
            Self::Unauthorized => "Unauthorized Thunderbolt device",
            Self::AuthError(_) => "Thunderbolt authorization error",
        }
    }

    pub fn body(&self) -> String {
        match self {
            Self::UnknownWhileAway => UNKNOWN_BODY.to_owned(),
            Self::Unauthorized => UNAUTHORIZED_BODY.to_owned(),
            Self::AuthError(message) => {
                format!("Could not authorize the Thunderbolt device: {message}")
            }
        }
    }
}

/// What a `DeviceAdded` leads to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Not the robot's business (not connected, authorization disabled,
    /// boltd's state unknown).
    Ignore,
    /// Queue `EnrollDevice` ([`enroll_args`]).
    Enroll,
    /// Leave the device alone and tell the user.
    Notify(Notice),
}

/// `AuthRobot._onDeviceAdded`'s gates: a `connected` device while the
/// daemon's `AuthMode` contains `enabled`. A missing property (no boltd,
/// nothing cached) passes neither.
pub fn robot_wants(status: Option<&str>, auth_mode: Option<&str>) -> bool {
    status == Some(STATUS_CONNECTED)
        && auth_mode.is_some_and(|mode| mode.split('|').any(|flag| flag == AUTH_MODE_ENABLED))
}

/// `Indicator._onEnrollDevice`: enrol only when unlocked and the polkit
/// permission is already allowed; otherwise say why not.
pub fn enroll_decision(unlocked: bool, allowed: bool) -> Verdict {
    if unlocked && allowed {
        Verdict::Enroll
    } else if !unlocked {
        Verdict::Notify(Notice::UnknownWhileAway)
    } else {
        Verdict::Notify(Notice::Unauthorized)
    }
}

/// A `DeviceAdded` with the device's `Status`, the manager's `AuthMode`,
/// the session's lock state and the polkit permission.
pub fn device_added(
    status: Option<&str>,
    auth_mode: Option<&str>,
    unlocked: bool,
    allowed: bool,
) -> Verdict {
    if robot_wants(status, auth_mode) {
        enroll_decision(unlocked, allowed)
    } else {
        Verdict::Ignore
    }
}

/// `Client.enrollDevice(uid, Policy.DEFAULT)`'s `EnrollDevice` arguments.
pub fn enroll_args(uid: &str) -> (&str, &'static str, &'static str) {
    (uid, POLICY_DEFAULT, AUTH_CTRL_NONE)
}

/// `Indicator._sync` and `_onProbing`: the icon to show, `None` hidden.
pub fn indicator_icon(unlocked: bool, probing: bool) -> Option<&'static str> {
    (unlocked && probing).then_some(ACQUIRING_ICON)
}

/// `!Main.sessionMode.isLocked && !Main.sessionMode.isGreeter`, from the
/// compositor's lock state. Unknown counts as locked.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SessionLock(Option<bool>);

impl SessionLock {
    /// An authoritative lock state; `None` (not known yet) changes nothing.
    pub fn sync(&mut self, locked: Option<bool>) {
        if locked.is_some() {
            self.0 = locked;
        }
    }

    pub fn unlocked(&self) -> bool {
        self.0 == Some(false)
    }
}

/// `AuthRobot`'s enrolment queue: devices join at the back and are
/// enrolled one at a time, each from its own idle callback.
#[derive(Debug)]
pub struct EnrollQueue<D> {
    pending: VecDeque<D>,
    enrolling: bool,
}

impl<D> Default for EnrollQueue<D> {
    fn default() -> Self {
        Self {
            pending: VecDeque::new(),
            enrolling: false,
        }
    }
}

impl<D> EnrollQueue<D> {
    /// `_devicesToEnroll.push` then `_enrollDevices`: true when the caller
    /// must schedule an idle run (none is pending yet).
    pub fn push(&mut self, device: D) -> bool {
        self.pending.push_back(device);
        if self.enrolling {
            return false;
        }
        self.enrolling = true;
        true
    }

    /// `_enrollDevicesIdle`'s `shift`: the device to enrol now.
    pub fn next(&mut self) -> Option<D> {
        self.pending.pop_front()
    }

    /// The device's `EnrollDevice` returned: true when another idle run
    /// must be scheduled. On failure `_enrolling` stays set, so the robot
    /// never runs again and later [`Self::push`]es only queue.
    pub fn finished(&mut self, ok: bool) -> bool {
        if !ok {
            return false;
        }
        self.enrolling = !self.pending.is_empty();
        self.enrolling
    }
}

pub use runtime::Thunderbolt;

mod runtime {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;
    use std::rc::Rc;

    use gtk4::prelude::*;
    use gtk4::{gio, glib};

    use crate::notify::NotifyUi;
    use tuna_shell_host::notifications::Urgency;

    const BOLT_NAME: &str = "org.freedesktop.bolt";
    const BOLT_PATH: &str = "/org/freedesktop/bolt";
    const MANAGER_IFACE: &str = "org.freedesktop.bolt1.Manager";
    const DEVICE_IFACE: &str = "org.freedesktop.bolt1.Device";
    const AUTHORITY: &str = "org.freedesktop.PolicyKit1";
    const AUTHORITY_PATH: &str = "/org/freedesktop/PolicyKit1/Authority";
    const AUTHORITY_IFACE: &str = "org.freedesktop.PolicyKit1.Authority";
    /// GNOME's system notification source (`messageTray.js` `getSystemSource`).
    const SYSTEM_SOURCE: &str = "System";

    /// What the robot keeps of a `BoltDeviceProxy`.
    struct Device {
        uid: String,
        name: String,
    }

    pub struct Thunderbolt {
        notify: Rc<NotifyUi>,
        indicator: gtk4::Image,
        conn: RefCell<Option<gio::DBusConnection>>,
        manager: RefCell<Option<gio::DBusProxy>>,
        lock: Cell<SessionLock>,
        queue: RefCell<EnrollQueue<Device>>,
        /// `this._perm.allowed`; `None` while there is no permission.
        allowed: Cell<Option<bool>>,
        /// The `CheckAuthorization` answer last asked for (older replies lose).
        perm_query: Cell<u64>,
        /// `this._notification`.
        notice: Cell<Option<u64>>,
    }

    impl Thunderbolt {
        /// Start the client, robot and permission; `indicator` is the
        /// panel's status icon, hidden until boltd probes.
        pub fn start(notify: Rc<NotifyUi>, indicator: gtk4::Image) -> Rc<Self> {
            indicator.set_visible(false);
            let me = Rc::new(Self {
                notify,
                indicator,
                conn: RefCell::new(None),
                manager: RefCell::new(None),
                lock: Cell::new(SessionLock::default()),
                queue: RefCell::new(EnrollQueue::default()),
                allowed: Cell::new(None),
                perm_query: Cell::new(0),
                notice: Cell::new(None),
            });
            let weak = Rc::downgrade(&me);
            gio::bus_get(gio::BusType::System, gio::Cancellable::NONE, move |conn| {
                let Some(me) = weak.upgrade() else { return };
                match conn {
                    Ok(conn) => {
                        *me.conn.borrow_mut() = Some(conn.clone());
                        me.connect_manager(&conn);
                        me.watch_permission(&conn);
                    }
                    Err(e) => {
                        eprintln!("tuna-shell-gtk: thunderbolt: error creating bolt proxy: {e}")
                    }
                }
            });
            me
        }

        /// `Main.sessionMode` updated: `None` (no authoritative lock
        /// state yet) changes nothing.
        pub fn sync_lock(&self, locked: Option<bool>) {
            let mut lock = self.lock.get();
            lock.sync(locked);
            self.lock.set(lock);
            self.sync_indicator();
        }

        fn sync_indicator(&self) {
            let probing = self
                .manager
                .borrow()
                .as_ref()
                .and_then(|m| m.cached_property("Probing"))
                .and_then(|v| v.get::<bool>())
                .unwrap_or(false);
            match indicator_icon(self.lock.get().unlocked(), probing) {
                Some(icon) => {
                    if self.indicator.icon_name().as_deref() != Some(icon) {
                        self.indicator.set_icon_name(Some(icon));
                    }
                    self.indicator.set_visible(true);
                }
                None => self.indicator.set_visible(false),
            }
        }

        /// `Client._getProxy`.
        fn connect_manager(self: &Rc<Self>, conn: &gio::DBusConnection) {
            let weak = Rc::downgrade(self);
            gio::DBusProxy::new(
                conn,
                gio::DBusProxyFlags::DO_NOT_AUTO_START,
                None,
                Some(BOLT_NAME),
                BOLT_PATH,
                MANAGER_IFACE,
                gio::Cancellable::NONE,
                move |res| {
                    let Some(me) = weak.upgrade() else { return };
                    let proxy = match res {
                        Ok(proxy) => proxy,
                        Err(e) => {
                            eprintln!(
                                "tuna-shell-gtk: thunderbolt: error creating bolt proxy: {e}"
                            );
                            return;
                        }
                    };
                    let weak = Rc::downgrade(&me);
                    proxy.connect_local("g-properties-changed", false, move |_| {
                        if let Some(me) = weak.upgrade() {
                            me.sync_indicator();
                        }
                        None
                    });
                    let weak = Rc::downgrade(&me);
                    proxy.connect_local("g-signal", false, move |args| {
                        let signal = args.get(2).and_then(|v| v.get::<String>().ok());
                        if signal.as_deref() != Some("DeviceAdded") {
                            return None;
                        }
                        let path = args
                            .get(3)
                            .and_then(|v| v.get::<glib::Variant>().ok())
                            .and_then(|p| p.try_child_value(0))
                            .and_then(|p| p.str().map(str::to_owned));
                        if let (Some(me), Some(path)) = (weak.upgrade(), path) {
                            me.device_proxy(&path);
                        }
                        None
                    });
                    *me.manager.borrow_mut() = Some(proxy);
                    me.sync_indicator();
                },
            );
        }

        /// `Client._onDeviceAdded`: a `BoltDeviceProxy` for `path`.
        fn device_proxy(self: &Rc<Self>, path: &str) {
            let Some(conn) = self.conn.borrow().clone() else {
                return;
            };
            let weak = Rc::downgrade(self);
            gio::DBusProxy::new(
                &conn,
                gio::DBusProxyFlags::NONE,
                None,
                Some(BOLT_NAME),
                path,
                DEVICE_IFACE,
                gio::Cancellable::NONE,
                move |res| {
                    let Some(me) = weak.upgrade() else { return };
                    match res {
                        Ok(device) => me.device_added(&device),
                        Err(e) => eprintln!("tuna-shell-gtk: thunderbolt: device proxy: {e}"),
                    }
                },
            );
        }

        fn manager_string(&self, property: &str) -> Option<String> {
            self.manager
                .borrow()
                .as_ref()
                .and_then(|m| m.cached_property(property))
                .and_then(|v| v.get::<String>())
        }

        /// `AuthRobot._onDeviceAdded` with `Indicator._onEnrollDevice`.
        fn device_added(self: &Rc<Self>, device: &gio::DBusProxy) {
            let string = |property: &str| {
                device
                    .cached_property(property)
                    .and_then(|v| v.get::<String>())
            };
            let status = string("Status");
            let auth_mode = self.manager_string("AuthMode");
            let unlocked = self.lock.get().unlocked();
            let allowed = self.allowed.get().unwrap_or(false);
            let verdict = device_added(status.as_deref(), auth_mode.as_deref(), unlocked, allowed);
            let name = string("Name").unwrap_or_default();
            if verdict != Verdict::Ignore {
                eprintln!(
                    "tuna-shell-gtk: thunderbolt: [{name}] auto enrollment: {} (allowed: {})",
                    if verdict == Verdict::Enroll {
                        "yes"
                    } else {
                        "no"
                    },
                    if allowed { "yes" } else { "no" },
                );
            }
            match verdict {
                Verdict::Ignore => {}
                Verdict::Notify(notice) => self.post(notice),
                Verdict::Enroll => {
                    let uid = string("Uid").unwrap_or_default();
                    if self.queue.borrow_mut().push(Device { uid, name }) {
                        self.schedule_enroll();
                    }
                }
            }
        }

        fn schedule_enroll(self: &Rc<Self>) {
            let weak = Rc::downgrade(self);
            glib::idle_add_local_once(move || {
                if let Some(me) = weak.upgrade() {
                    me.enroll_next();
                }
            });
        }

        /// `_enrollDevicesIdle`.
        fn enroll_next(self: &Rc<Self>) {
            let Some(device) = self.queue.borrow_mut().next() else {
                return;
            };
            let Some(manager) = self.manager.borrow().clone() else {
                return;
            };
            let weak = Rc::downgrade(self);
            manager.call(
                "EnrollDevice",
                Some(&enroll_args(&device.uid).to_variant()),
                gio::DBusCallFlags::NONE,
                -1,
                gio::Cancellable::NONE,
                move |res| {
                    let Some(me) = weak.upgrade() else { return };
                    let ok = match res {
                        Ok(_) => true,
                        Err(mut e) => {
                            gio::DBusError::strip_remote_error(&mut e);
                            eprintln!(
                                "tuna-shell-gtk: thunderbolt: [{}] enrollment failed: {}",
                                device.name,
                                e.message()
                            );
                            me.post(Notice::AuthError(e.message().to_owned()));
                            false
                        }
                    };
                    if me.queue.borrow_mut().finished(ok) {
                        me.schedule_enroll();
                    }
                },
            );
        }

        /// `Indicator._notify`: replaces the previous notice.
        fn post(self: &Rc<Self>, notice: Notice) {
            if let Some(previous) = self.notice.take() {
                self.notify.remove_notification(previous);
            }
            // Activating the notification opens the Thunderbolt panel.
            let actions = [("default".to_owned(), String::new())];
            let Some(id) = self.notify.post_with_actions(
                SYSTEM_SOURCE,
                ICON,
                notice.title(),
                &notice.body(),
                &actions,
                Urgency::High,
                None,
            ) else {
                return;
            };
            self.notify
                .on_notification_action(id, "default", Rc::new(open_panel));
            self.notice.set(Some(id));
        }

        /// `_createPermission`, kept current on the authority's `Changed`.
        fn watch_permission(self: &Rc<Self>, conn: &gio::DBusConnection) {
            let weak = Rc::downgrade(self);
            let sub = conn.subscribe_to_signal(
                Some(AUTHORITY),
                Some(AUTHORITY_IFACE),
                Some("Changed"),
                Some(AUTHORITY_PATH),
                None,
                gio::DBusSignalFlags::NONE,
                move |_| {
                    if let Some(me) = weak.upgrade() {
                        me.check_permission();
                    }
                },
            );
            // Held for the shell's lifetime, as the indicator is.
            std::mem::forget(sub);
            self.check_permission();
        }

        /// `CheckAuthorization` without user interaction: `allowed` is
        /// its `is_authorized`; a failure leaves no permission.
        fn check_permission(self: &Rc<Self>) {
            let Some(conn) = self.conn.borrow().clone() else {
                return;
            };
            let Some(unique) = conn.unique_name() else {
                return;
            };
            let serial = self.perm_query.get() + 1;
            self.perm_query.set(serial);
            let subject: HashMap<String, glib::Variant> =
                HashMap::from([("name".to_owned(), unique.as_str().to_variant())]);
            let details: HashMap<String, String> = HashMap::new();
            let params = (
                ("system-bus-name", subject),
                ENROLL_ACTION,
                details,
                0u32,
                "",
            )
                .to_variant();
            let weak = Rc::downgrade(self);
            conn.call(
                Some(AUTHORITY),
                AUTHORITY_PATH,
                AUTHORITY_IFACE,
                "CheckAuthorization",
                Some(&params),
                None,
                gio::DBusCallFlags::NONE,
                -1,
                gio::Cancellable::NONE,
                move |res| {
                    let Some(me) = weak.upgrade() else { return };
                    if me.perm_query.get() != serial {
                        return;
                    }
                    match res {
                        Ok(reply) => {
                            let allowed = reply
                                .try_child_value(0)
                                .and_then(|r| r.try_child_value(0))
                                .and_then(|a| a.get::<bool>())
                                .unwrap_or(false);
                            me.allowed.set(Some(allowed));
                        }
                        Err(e) => {
                            me.allowed.set(None);
                            eprintln!(
                                "tuna-shell-gtk: thunderbolt: Failed to get PolKit permission: {e}"
                            );
                        }
                    }
                },
            );
        }
    }

    /// `lookup_app('gnome-thunderbolt-panel.desktop')?.activate()`
    /// (gio 0.22 has no `DesktopAppInfo`, so scan the installed apps).
    fn open_panel() {
        let Some(app) = gio::AppInfo::all()
            .into_iter()
            .find(|app| app.id().as_deref() == Some(PANEL_APP))
        else {
            return;
        };
        let context = tuna_shell_host::apps::application_launch_context();
        if let Err(e) = app.launch(&[], Some(&context)) {
            eprintln!("tuna-shell-gtk: thunderbolt: unable to launch {PANEL_APP}: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bolt_absent_does_nothing() {
        // Without boltd nothing is cached: no AuthMode, no Status, no
        // Probing. Nothing is enrolled, posted or shown.
        for status in [None, Some(STATUS_CONNECTED)] {
            assert_eq!(device_added(status, None, true, true), Verdict::Ignore);
            assert_eq!(device_added(status, None, false, false), Verdict::Ignore);
        }
        assert_eq!(indicator_icon(true, false), None);
        assert_eq!(indicator_icon(false, false), None);
    }

    #[test]
    fn auto_policy_enrols_when_the_user_may() {
        assert_eq!(
            device_added(Some("connected"), Some("enabled"), true, true),
            Verdict::Enroll
        );
        // Policy.DEFAULT lets boltd apply its configured (auto) policy;
        // AuthCtrl.NONE are the flags.
        assert_eq!(
            enroll_args("884c6edd-7118-4b21-b186-b02d396ecca0"),
            ("884c6edd-7118-4b21-b186-b02d396ecca0", "default", "none")
        );
    }

    #[test]
    fn unauthorized_device_while_locked_notifies_without_enrolling() {
        for allowed in [true, false] {
            let verdict = device_added(Some("connected"), Some("enabled"), false, allowed);
            assert_eq!(verdict, Verdict::Notify(Notice::UnknownWhileAway));
        }
        let notice = Notice::UnknownWhileAway;
        assert_eq!(notice.title(), "Unknown Thunderbolt device");
        assert_eq!(notice.body(), UNKNOWN_BODY);
        assert_eq!(
            UNKNOWN_BODY,
            "New device has been detected while you were away. \
             Please disconnect and reconnect the device to start using it."
        );
    }

    #[test]
    fn unlocked_without_permission_notifies_unauthorized() {
        let verdict = device_added(Some("connected"), Some("enabled"), true, false);
        assert_eq!(verdict, Verdict::Notify(Notice::Unauthorized));
        assert_eq!(
            Notice::Unauthorized.title(),
            "Unauthorized Thunderbolt device"
        );
        assert_eq!(
            Notice::Unauthorized.body(),
            "New device has been detected and needs to be authorized by an administrator"
        );
    }

    #[test]
    fn only_connected_devices_with_authorization_enabled_are_considered() {
        for status in [
            "disconnected",
            "connecting",
            "authorizing",
            "auth-error",
            "authorized",
        ] {
            assert_eq!(
                device_added(Some(status), Some("enabled"), true, true),
                Verdict::Ignore,
                "{status}"
            );
        }
        // AuthMode is a flag list that must contain `enabled`; a locked
        // session is not even told about a device the daemon cannot
        // authorize.
        assert_eq!(
            device_added(Some("connected"), Some("disabled"), false, true),
            Verdict::Ignore
        );
        assert_eq!(
            device_added(Some("connected"), Some("enabledx"), true, true),
            Verdict::Ignore
        );
        assert!(robot_wants(Some("connected"), Some("future|enabled")));
    }

    #[test]
    fn enrolment_failure_names_the_error() {
        let notice = Notice::AuthError("device is not connected".into());
        assert_eq!(notice.title(), "Thunderbolt authorization error");
        assert_eq!(
            notice.body(),
            "Could not authorize the Thunderbolt device: device is not connected"
        );
    }

    #[test]
    fn unknown_lock_state_counts_as_locked() {
        let mut lock = SessionLock::default();
        assert!(!lock.unlocked());
        lock.sync(Some(false));
        assert!(lock.unlocked());
        lock.sync(None);
        assert!(lock.unlocked());
        lock.sync(Some(true));
        assert!(!lock.unlocked());
    }

    #[test]
    fn enrolment_queue_runs_one_device_at_a_time() {
        let mut queue = EnrollQueue::default();
        // The first device schedules the idle run; the next waits for it.
        assert!(queue.push("a"));
        assert!(!queue.push("b"));
        assert_eq!(queue.next(), Some("a"));
        // A device arriving while "a" is enrolling only queues.
        assert!(!queue.push("c"));
        assert!(queue.finished(true));
        assert_eq!(queue.next(), Some("b"));
        assert!(queue.finished(true));
        assert_eq!(queue.next(), Some("c"));
        assert!(!queue.finished(true));
        // Drained: the next arrival schedules again.
        assert!(queue.push("d"));
    }

    #[test]
    fn failed_enrolment_stops_auto_enrolment_until_restart() {
        let mut queue = EnrollQueue::default();
        assert!(queue.push(1));
        assert!(!queue.push(2));
        assert_eq!(queue.next(), Some(1));
        // EnrollDevice failed for 1: no further idle run, 2 stays queued.
        assert!(!queue.finished(false));
        // Later approved devices only queue; nothing schedules a run again
        // (the lock state is not consulted by the queue at all).
        assert!(!queue.push(3));
        assert!(!queue.push(4));
        // Devices the robot would not enrol are still announced.
        assert_eq!(
            device_added(Some("connected"), Some("enabled"), false, true),
            Verdict::Notify(Notice::UnknownWhileAway)
        );
        // Only a fresh robot (a shell restart) enrols again.
        let mut restarted = EnrollQueue::default();
        assert!(restarted.push(5));
    }

    #[test]
    fn probing_shows_the_acquiring_icon_only_while_unlocked() {
        assert_eq!(
            indicator_icon(true, true),
            Some("thunderbolt-acquiring-symbolic")
        );
        assert_eq!(indicator_icon(false, true), None);
        assert_eq!(indicator_icon(true, false), None);
    }
}
