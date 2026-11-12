//! Switch User… in the power menu (GNOME 51 shape, #1039).
//!
//! The row reproduces gnome-shell 51.0 `js/misc/systemActions.js`
//! `_updateSwitchUser`: it shows while `disable-user-switching` allows
//! it, AccountsService's user manager can switch and has more than one
//! user, and the session is neither locked nor a greeter. "Can switch"
//! is accountsservice's `act_user_manager_can_switch`
//! (`src/libaccountsservice/act-user-manager.c`): the manager is loaded
//! (the daemon answers), the user's graphical session sits on a seat
//! (`_find_graphical_systemd_session`, the newest `wayland`/`x11`/`mir`
//! session that is `active` or `online`) and logind says that seat
//! `CanMultiSession`. "More than one user" is the daemon's
//! `HasMultipleUsers` property, which `ActUserManager:has-multiple-users`
//! is bound to. The facts are re-read when the daemon's object signals
//! (PropertiesChanged, UserAdded, UserDeleted) or its bus name changes
//! hands, as `_updateMultiUser` follows `has-multiple-users`,
//! `user-added`, `user-removed` and `is-loaded`. The row sits last,
//! under Log Out…, as `js/ui/status/system.js` ShutdownItem adds it.
//!
//! Activation is `activateSwitchUser`: lock the screen
//! (`Main.screenShield.lock(false)`, which declines while
//! `disable-lock-screen` is set), then libgdm's
//! `gdm_goto_login_session_sync` (gdm 51.0
//! `libgdm/gdm-user-switching.c`, implemented by `common/gdm-common.c`
//! `goto_login_session`) over logind's and GDM's D-Bus APIs: this
//! process's session (`gdm_find_display_session`: `GetSessionByPID`,
//! else the user's first graphical session that is active or online),
//! its seat, an existing login window on that seat
//! (`gdm_get_login_window_session_id`: class `greeter`, state not
//! `closing`, service `gdm-launch-environment`) activated with
//! `ActivateSessionOnSeat`, and otherwise, on `seat0` only, GDM's
//! `LocalDisplayFactory.CreateTransientDisplay`. A failure is logged and
//! nothing unlocks: the session stays locked.
//!
//! Recorded divergences:
//! - GNOME switches from a post-paint repaint callback once the shield
//!   is drawn; Tuna starts the switch right after the lock command is on
//!   the control socket. The compositor engages its lock on receipt,
//!   before the first of the switch's D-Bus round trips returns.
//! - A lock request that cannot reach the compositor cancels the switch
//!   (GNOME's shield cannot fail that way), so the seat never changes
//!   hands with this session unlocked by accident. A lock declined by
//!   `disable-lock-screen` still switches, as in GNOME.
//! - The shell has no greeter mode; only the locked state hides the row.
//! - libsystemd's `sd_*` reads are logind D-Bus properties here: a
//!   session that vanished between the list and the read (`-ENXIO`)
//!   answers `UnknownObject`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gio::prelude::*;

/// The row's label (status/system.js ShutdownItem).
pub const LABEL: &str = "Switch User…";

const LOGIND: &str = "org.freedesktop.login1";
const LOGIND_PATH: &str = "/org/freedesktop/login1";
const LOGIND_MANAGER: &str = "org.freedesktop.login1.Manager";
const LOGIND_SESSION: &str = "org.freedesktop.login1.Session";
const LOGIND_SEAT: &str = "org.freedesktop.login1.Seat";
const LOGIND_USER: &str = "org.freedesktop.login1.User";
const ACCOUNTS: &str = "org.freedesktop.Accounts";
const ACCOUNTS_PATH: &str = "/org/freedesktop/Accounts";
/// libgdm's `GDM_DBUS_NAME` and local display factory (gdm-common.c).
const GDM: &str = "org.gnome.DisplayManager";
const GDM_FACTORY_PATH: &str = "/org/gnome/DisplayManager/LocalDisplayFactory";
const GDM_FACTORY: &str = "org.gnome.DisplayManager.LocalDisplayFactory";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
/// Property reads; the switch's own calls use the bus default (libgdm's -1).
const TIMEOUT_MS: i32 = 5000;
const DEFAULT_TIMEOUT: i32 = -1;

/// What AccountsService's user manager knows about switching.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SeatFacts {
    /// The daemon answered (`ActUserManager:is-loaded`).
    pub accounts_loaded: bool,
    /// The daemon's `HasMultipleUsers`.
    pub has_multiple_users: bool,
    /// The seat of the user's graphical session, when it has one.
    pub seat_id: Option<String>,
    /// logind's `Seat.CanMultiSession` for that seat.
    pub can_multi_session: bool,
}

/// `act_user_manager_can_switch`: loaded, on a seat, and the seat can
/// hold more than one session.
pub fn can_switch(facts: &SeatFacts) -> bool {
    facts.accounts_loaded
        && facts
            .seat_id
            .as_deref()
            .is_some_and(|seat| !seat.is_empty())
        && facts.can_multi_session
}

/// `_updateSwitchUser`: the lockdown key allows it (`allowed`, the
/// lockdown snapshot's `user_switch`), the user manager can switch and
/// has several users, and the session is not locked.
pub fn switch_user_visible(allowed: bool, facts: &SeatFacts, locked: bool) -> bool {
    allowed && can_switch(facts) && facts.has_multiple_users && !locked
}

/// The logind session properties libgdm and accountsservice read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionProps {
    pub id: String,
    /// `Class` (`user`, `greeter`, …).
    pub class: String,
    /// `State` (`online`, `active`, `closing`).
    pub state: String,
    /// `Service`, the PAM service that opened it.
    pub service: String,
    /// `Type` (`wayland`, `x11`, `tty`, …).
    pub kind: String,
    /// The seat id, empty when seatless.
    pub seat: String,
}

/// One session's read: the properties, gone since it was listed
/// (`-ENXIO` in libsystemd), or another failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionProbe {
    Read(SessionProps),
    Vanished,
    Failed,
}

impl SessionProbe {
    fn read(&self) -> Option<&SessionProps> {
        match self {
            SessionProbe::Read(session) => Some(session),
            SessionProbe::Vanished | SessionProbe::Failed => None,
        }
    }
}

/// `_systemd_session_is_graphical` and `_systemd_session_is_active`
/// (identical in gdm-common.c and act-user-manager.c); an unreadable
/// session is skipped.
fn graphical_and_active(session: &SessionProps) -> bool {
    matches!(session.kind.as_str(), "wayland" | "x11" | "mir")
        && matches!(session.state.as_str(), "active" | "online")
}

/// accountsservice's `_find_graphical_systemd_session`: of the user's
/// sessions (newest first), the last graphical active one.
pub fn accounts_session(sessions: &[SessionProbe]) -> Option<&SessionProps> {
    sessions
        .iter()
        .rev()
        .filter_map(SessionProbe::read)
        .find(|session| graphical_and_active(session))
}

/// libgdm's `gdm_find_display_session` fallback (`find_graphical_sessions`
/// then `sessions[0]`): the first graphical active session.
pub fn gdm_display_session(sessions: &[SessionProbe]) -> Option<&SessionProps> {
    sessions
        .iter()
        .filter_map(SessionProbe::read)
        .find(|session| graphical_and_active(session))
}

/// libgdm's `gdm_get_login_window_session_id` over a seat's sessions:
/// the first `greeter` session not `closing` that GDM launched
/// (`gdm-launch-environment`). A vanished session is skipped; any other
/// read failure ends the search with none.
pub fn login_window_session(sessions: &[SessionProbe]) -> Option<&str> {
    for probe in sessions {
        match probe {
            SessionProbe::Vanished => continue,
            SessionProbe::Failed => return None,
            SessionProbe::Read(session) => {
                if session.class == "greeter"
                    && session.state != "closing"
                    && session.service == "gdm-launch-environment"
                {
                    return Some(session.id.as_str());
                }
            }
        }
    }
    None
}

/// What libgdm's `goto_login_session` tries, in order: activate the
/// existing login window (`activate`), and only when there is none or
/// its activation fails, ask GDM for a transient display, which it does
/// on `seat0` alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GotoPlan {
    pub activate: Option<String>,
    pub create_transient_display: bool,
}

pub fn goto_login_plan(seat_id: &str, login_window: Option<&str>) -> GotoPlan {
    GotoPlan {
        activate: login_window.map(str::to_owned),
        create_transient_display: seat_id == "seat0",
    }
}

/// One method call on the system bus.
#[allow(clippy::too_many_arguments)]
async fn call(
    bus: &gio::DBusConnection,
    name: &str,
    path: &str,
    iface: &str,
    method: &str,
    args: Option<glib::Variant>,
    reply: &str,
    timeout: i32,
) -> Result<glib::Variant, glib::Error> {
    bus.call_future(
        Some(name),
        path,
        iface,
        method,
        args.as_ref(),
        glib::VariantTy::new(reply).ok(),
        gio::DBusCallFlags::NONE,
        timeout,
    )
    .await
}

fn odd_reply() -> glib::Error {
    glib::Error::new(gio::IOErrorEnum::InvalidData, "unexpected D-Bus reply")
}

/// The object path of an `(o)` reply.
fn object_path(reply: glib::Variant) -> Result<String, glib::Error> {
    reply
        .child_value(0)
        .str()
        .map(str::to_owned)
        .ok_or_else(odd_reply)
}

/// One D-Bus property's value.
async fn property(
    bus: &gio::DBusConnection,
    name: &str,
    path: &str,
    iface: &str,
    key: &str,
) -> Result<glib::Variant, glib::Error> {
    let reply = call(
        bus,
        name,
        path,
        PROPERTIES,
        "Get",
        Some((iface, key).to_variant()),
        "(v)",
        TIMEOUT_MS,
    )
    .await?;
    reply.child_value(0).as_variant().ok_or_else(odd_reply)
}

/// Whether a failed read means the object went away (libsystemd's
/// `-ENXIO`).
fn vanished(error: &glib::Error) -> bool {
    gio::DBusError::remote_error(error).is_some_and(|name| {
        matches!(
            name.as_str(),
            "org.freedesktop.DBus.Error.UnknownObject" | "org.freedesktop.login1.NoSuchSession"
        )
    })
}

/// Read a logind session object's properties.
async fn probe_session(bus: &gio::DBusConnection, path: &str) -> SessionProbe {
    let reply = call(
        bus,
        LOGIND,
        path,
        PROPERTIES,
        "GetAll",
        Some((LOGIND_SESSION,).to_variant()),
        "(a{sv})",
        TIMEOUT_MS,
    )
    .await;
    match reply {
        Ok(reply) => {
            let dict = glib::VariantDict::new(Some(&reply.child_value(0)));
            let text = |key: &str| {
                dict.lookup_value(key, None)
                    .and_then(|value| value.str().map(str::to_owned))
                    .unwrap_or_default()
            };
            // `Seat` is `(so)`: the id and the seat object.
            let seat = dict
                .lookup_value("Seat", None)
                .and_then(|value| value.try_child_value(0))
                .and_then(|id| id.str().map(str::to_owned))
                .unwrap_or_default();
            SessionProbe::Read(SessionProps {
                id: text("Id"),
                class: text("Class"),
                state: text("State"),
                service: text("Service"),
                kind: text("Type"),
                seat,
            })
        }
        Err(e) if vanished(&e) => SessionProbe::Vanished,
        Err(e) => {
            eprintln!("tuna-shell-gtk: switch user: reading session {path} failed: {e}");
            SessionProbe::Failed
        }
    }
}

/// The session object paths of an `a(so)` list.
fn session_paths(list: &glib::Variant) -> Vec<String> {
    if !list.is_container() {
        return Vec::new();
    }
    list.iter()
        .filter_map(|entry| entry.try_child_value(1))
        .filter_map(|path| path.str().map(str::to_owned))
        .collect()
}

async fn probe_all(bus: &gio::DBusConnection, paths: &[String]) -> Vec<SessionProbe> {
    let mut probes = Vec::with_capacity(paths.len());
    for path in paths {
        probes.push(probe_session(bus, path).await);
    }
    probes
}

/// The user's sessions, newest first (`sd_uid_get_sessions`, which is
/// logind's `User.Sessions` list).
async fn user_sessions(bus: &gio::DBusConnection) -> Result<Vec<SessionProbe>, glib::Error> {
    let uid = unsafe { libc::getuid() };
    let user = object_path(
        call(
            bus,
            LOGIND,
            LOGIND_PATH,
            LOGIND_MANAGER,
            "GetUser",
            Some((uid,).to_variant()),
            "(o)",
            TIMEOUT_MS,
        )
        .await?,
    )?;
    let sessions = property(bus, LOGIND, &user, LOGIND_USER, "Sessions").await?;
    Ok(probe_all(bus, &session_paths(&sessions)).await)
}

async fn seat_path(bus: &gio::DBusConnection, seat_id: &str) -> Result<String, glib::Error> {
    object_path(
        call(
            bus,
            LOGIND,
            LOGIND_PATH,
            LOGIND_MANAGER,
            "GetSeat",
            Some((seat_id,).to_variant()),
            "(o)",
            TIMEOUT_MS,
        )
        .await?,
    )
}

/// The user manager's view: `HasMultipleUsers`, then the seat of the
/// user's graphical session and whether it can hold several sessions.
/// The daemon not answering leaves the manager unloaded (no switching).
pub async fn load_facts() -> SeatFacts {
    let mut facts = SeatFacts::default();
    let Ok(bus) = gio::bus_get_future(gio::BusType::System).await else {
        return facts;
    };
    let has_multiple_users = property(&bus, ACCOUNTS, ACCOUNTS_PATH, ACCOUNTS, "HasMultipleUsers")
        .await
        .ok()
        .and_then(|value| value.get::<bool>());
    let Some(has_multiple_users) = has_multiple_users else {
        return facts;
    };
    facts.accounts_loaded = true;
    facts.has_multiple_users = has_multiple_users;
    let Ok(sessions) = user_sessions(&bus).await else {
        return facts;
    };
    let Some(seat) = accounts_session(&sessions)
        .map(|session| session.seat.clone())
        .filter(|seat| !seat.is_empty())
    else {
        return facts;
    };
    facts.can_multi_session = match seat_path(&bus, &seat).await {
        Ok(path) => property(&bus, LOGIND, &path, LOGIND_SEAT, "CanMultiSession")
            .await
            .ok()
            .and_then(|value| value.get::<bool>())
            .unwrap_or(false),
        Err(_) => false,
    };
    facts.seat_id = Some(seat);
    facts
}

/// libgdm's `gdm_find_display_session (0, getuid ())`: this process's
/// session, else the user's first graphical active one.
async fn display_session(bus: &gio::DBusConnection) -> Result<SessionProps, String> {
    let by_pid = call(
        bus,
        LOGIND,
        LOGIND_PATH,
        LOGIND_MANAGER,
        "GetSessionByPID",
        Some((std::process::id(),).to_variant()),
        "(o)",
        TIMEOUT_MS,
    )
    .await
    .and_then(object_path);
    if let Ok(path) = by_pid {
        // `sd_session_get_seat` on the found session.
        return match probe_session(bus, &path).await {
            SessionProbe::Read(session) => Ok(session),
            SessionProbe::Vanished | SessionProbe::Failed => {
                Err("Could not identify the current seat.".to_owned())
            }
        };
    }
    let sessions = user_sessions(bus)
        .await
        .map_err(|e| format!("Could not identify the current session: {e}"))?;
    gdm_display_session(&sessions).cloned().ok_or_else(|| {
        "Could not identify the current session: no graphical session for this user".to_owned()
    })
}

/// The sessions on `seat_id` (`sd_seat_get_sessions`); `None` when the
/// seat cannot be read.
async fn seat_sessions(bus: &gio::DBusConnection, seat_id: &str) -> Option<Vec<SessionProbe>> {
    let path = seat_path(bus, seat_id).await.ok()?;
    let list = property(bus, LOGIND, &path, LOGIND_SEAT, "Sessions")
        .await
        .ok()?;
    Some(probe_all(bus, &session_paths(&list)).await)
}

/// libgdm's `goto_login_session`, answering what it did.
async fn goto_login_session_async() -> Result<String, String> {
    let bus = gio::bus_get_future(gio::BusType::System)
        .await
        .map_err(|e| format!("Failed to connect to the D-Bus daemon: {e}"))?;
    let ours = display_session(&bus).await?;
    if ours.seat.is_empty() {
        return Err("Could not identify the current seat.".to_owned());
    }
    let seat_sessions = seat_sessions(&bus, &ours.seat).await;
    let login_window = seat_sessions.as_deref().and_then(login_window_session);
    let plan = goto_login_plan(&ours.seat, login_window);
    if let Some(session) = plan.activate.as_deref() {
        match call(
            &bus,
            LOGIND,
            LOGIND_PATH,
            LOGIND_MANAGER,
            "ActivateSessionOnSeat",
            Some((session, ours.seat.as_str()).to_variant()),
            "()",
            DEFAULT_TIMEOUT,
        )
        .await
        {
            Ok(_) => return Ok(format!("activated login session {session}")),
            Err(e) => eprintln!("tuna-shell-gtk: switch user: Unable to activate session: {e}"),
        }
    }
    if !plan.create_transient_display {
        return Err(format!("no login screen to switch to on {}", ours.seat));
    }
    let reply = call(
        &bus,
        GDM,
        GDM_FACTORY_PATH,
        GDM_FACTORY,
        "CreateTransientDisplay",
        None,
        "(o)",
        DEFAULT_TIMEOUT,
    )
    .await
    .map_err(|e| format!("Unable to create transient display: {e}"))?;
    Ok(format!(
        "started {}",
        reply.child_value(0).str().unwrap_or("a transient display")
    ))
}

/// Go to the login screen after the lock (`activateSwitchUser`'s
/// `Gdm.goto_login_session_sync`), off the main loop. Failures are
/// logged; nothing here unlocks.
pub fn goto_login_session() {
    glib::spawn_future_local(async {
        match goto_login_session_async().await {
            Ok(done) => eprintln!("tuna-shell-gtk: switch user: {done}"),
            Err(e) => eprintln!("tuna-shell-gtk: switch user failed: {e}; staying in this session"),
        }
    });
}

/// The user manager's live facts for the row, re-read as the daemon
/// changes; `connect_changed` hears each change.
pub struct Availability {
    facts: RefCell<SeatFacts>,
    /// Counts reads, so a stale one never lands after a newer one.
    serial: Cell<u64>,
    changed: RefCell<Option<Rc<dyn Fn()>>>,
}

impl Availability {
    pub fn new() -> Rc<Self> {
        Rc::new(Self {
            facts: RefCell::new(SeatFacts::default()),
            serial: Cell::new(0),
            changed: RefCell::new(None),
        })
    }

    pub fn connect_changed(&self, changed: Rc<dyn Fn()>) {
        *self.changed.borrow_mut() = Some(changed);
    }

    pub fn facts(&self) -> SeatFacts {
        self.facts.borrow().clone()
    }

    /// Follow AccountsService on the system bus and read the facts.
    pub fn start(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            if let Ok(bus) = gio::bus_get_future(gio::BusType::System).await {
                let accounts = {
                    let weak = weak.clone();
                    bus.subscribe_to_signal(
                        Some(ACCOUNTS),
                        None,
                        None,
                        Some(ACCOUNTS_PATH),
                        None,
                        gio::DBusSignalFlags::NONE,
                        move |_| {
                            if let Some(me) = weak.upgrade() {
                                me.refresh();
                            }
                        },
                    )
                };
                let owner = {
                    let weak = weak.clone();
                    bus.subscribe_to_signal(
                        Some("org.freedesktop.DBus"),
                        Some("org.freedesktop.DBus"),
                        Some("NameOwnerChanged"),
                        Some("/org/freedesktop/DBus"),
                        Some(ACCOUNTS),
                        gio::DBusSignalFlags::NONE,
                        move |_| {
                            if let Some(me) = weak.upgrade() {
                                me.refresh();
                            }
                        },
                    )
                };
                // Held for the session, with the subscriptions.
                std::mem::forget(accounts);
                std::mem::forget(owner);
                std::mem::forget(bus);
            }
            if let Some(me) = weak.upgrade() {
                me.refresh();
            }
        });
    }

    /// Re-read the facts; a change notifies.
    pub fn refresh(self: &Rc<Self>) {
        let serial = self.serial.get() + 1;
        self.serial.set(serial);
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let facts = load_facts().await;
            let Some(me) = weak.upgrade() else {
                return;
            };
            if me.serial.get() != serial {
                return;
            }
            if me.facts.replace(facts.clone()) == facts {
                return;
            }
            let changed = me.changed.borrow().clone();
            if let Some(changed) = changed {
                changed();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lockdown::LockdownPolicy;

    fn switchable() -> SeatFacts {
        SeatFacts {
            accounts_loaded: true,
            has_multiple_users: true,
            seat_id: Some("seat0".to_owned()),
            can_multi_session: true,
        }
    }

    fn session(id: &str, class: &str, state: &str, service: &str, kind: &str) -> SessionProbe {
        SessionProbe::Read(SessionProps {
            id: id.to_owned(),
            class: class.to_owned(),
            state: state.to_owned(),
            service: service.to_owned(),
            kind: kind.to_owned(),
            seat: "seat0".to_owned(),
        })
    }

    fn greeter(id: &str, state: &str) -> SessionProbe {
        session(id, "greeter", state, "gdm-launch-environment", "wayland")
    }

    #[test]
    fn switch_user_needs_multiple_users_and_multi_session() {
        assert!(switch_user_visible(true, &switchable(), false));
        let single_user = SeatFacts {
            has_multiple_users: false,
            ..switchable()
        };
        assert!(!switch_user_visible(true, &single_user, false));
        let single_session_seat = SeatFacts {
            can_multi_session: false,
            ..switchable()
        };
        assert!(!switch_user_visible(true, &single_session_seat, false));
    }

    #[test]
    fn can_switch_needs_a_loaded_manager_and_a_seat() {
        assert!(can_switch(&switchable()));
        let unloaded = SeatFacts {
            accounts_loaded: false,
            ..switchable()
        };
        assert!(!can_switch(&unloaded));
        let seatless = SeatFacts {
            seat_id: None,
            ..switchable()
        };
        assert!(!can_switch(&seatless));
        let empty_seat = SeatFacts {
            seat_id: Some(String::new()),
            ..switchable()
        };
        assert!(!can_switch(&empty_seat));
        assert!(!switch_user_visible(true, &SeatFacts::default(), false));
    }

    #[test]
    fn lockdown_hides_switch_user() {
        let denied = LockdownPolicy {
            disable_user_switching: true,
            ..LockdownPolicy::default()
        };
        assert!(!switch_user_visible(
            denied.entry_visibility().user_switch,
            &switchable(),
            false
        ));
        let allowed = LockdownPolicy::default().entry_visibility().user_switch;
        assert!(switch_user_visible(allowed, &switchable(), false));
        // Another key leaves the row alone.
        let no_log_out = LockdownPolicy {
            disable_log_out: true,
            ..LockdownPolicy::default()
        };
        assert!(switch_user_visible(
            no_log_out.entry_visibility().user_switch,
            &switchable(),
            false
        ));
    }

    #[test]
    fn a_locked_session_offers_no_switch_user() {
        assert!(!switch_user_visible(true, &switchable(), true));
    }

    #[test]
    fn an_existing_greeter_session_is_activated_before_creating_one() {
        let seat = [
            session("2", "user", "active", "gdm-password", "wayland"),
            greeter("c1", "online"),
        ];
        let plan = goto_login_plan("seat0", login_window_session(&seat));
        assert_eq!(plan.activate.as_deref(), Some("c1"));
        // The transient display stays the fallback for a failed activation.
        assert!(plan.create_transient_display);
        let without = goto_login_plan("seat0", login_window_session(&seat[..1]));
        assert_eq!(without.activate, None);
        assert!(without.create_transient_display);
    }

    #[test]
    fn only_seat0_falls_back_to_a_transient_display() {
        assert_eq!(
            goto_login_plan("seat1", None),
            GotoPlan {
                activate: None,
                create_transient_display: false,
            }
        );
        assert_eq!(
            goto_login_plan("seat1", Some("c4")),
            GotoPlan {
                activate: Some("c4".to_owned()),
                create_transient_display: false,
            }
        );
    }

    #[test]
    fn the_login_window_is_a_live_gdm_greeter() {
        let closing = greeter("c1", "closing");
        let other_greeter = session("c2", "greeter", "online", "lightdm-greeter", "x11");
        assert_eq!(
            login_window_session(&[
                closing.clone(),
                other_greeter.clone(),
                greeter("c3", "online")
            ]),
            Some("c3")
        );
        assert_eq!(login_window_session(&[closing, other_greeter]), None);
        // A session gone since the list is skipped; any other failed read
        // ends the search, as libgdm's non-ENXIO errors do.
        assert_eq!(
            login_window_session(&[SessionProbe::Vanished, greeter("c5", "online")]),
            Some("c5")
        );
        assert_eq!(
            login_window_session(&[SessionProbe::Failed, greeter("c5", "online")]),
            None
        );
    }

    #[test]
    fn accountsservice_takes_the_last_graphical_session_and_libgdm_the_first() {
        let sessions = [
            session("3", "user", "active", "gdm-password", "wayland"),
            session("tty", "user", "active", "login", "tty"),
            session("old", "user", "closing", "gdm-password", "x11"),
            SessionProbe::Failed,
            session("1", "user", "online", "gdm-password", "x11"),
        ];
        assert_eq!(
            accounts_session(&sessions).map(|s| s.id.as_str()),
            Some("1")
        );
        assert_eq!(
            gdm_display_session(&sessions).map(|s| s.id.as_str()),
            Some("3")
        );
        assert_eq!(accounts_session(&sessions[1..4]), None);
        assert_eq!(gdm_display_session(&sessions[1..4]), None);
    }
}
