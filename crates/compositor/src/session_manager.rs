//! `org.gnome.SessionManager` inhibitors, session activity and the end of
//! the session: the part of gnome-session 51's manager that GNOME
//! Settings Daemon's plugins use.
//!
//! The Tuna session starts gnome-settings-daemon's own plugin units
//! (`share/systemd/user/tuna-session.target`). gsd-power reads two manager
//! properties before it arms any idle timer: `InhibitedActions` (no
//! dimming, blanking or automatic suspend while an idle or suspend
//! inhibitor is held) and `SessionIsActive` (only the foreground session
//! dims). gsd-screensaver-proxy forwards `org.freedesktop.ScreenSaver`
//! `Inhibit` to `Inhibit` here with the idle flag. The idle lock honours
//! the same idle inhibitors, as GNOME Shell's screen shield does through
//! gnome-session's presence (an idle inhibitor keeps the session from
//! becoming idle). gsd-media-keys calls `Shutdown` for the power key under
//! `power-button-action` interactive and for its shutdown key, `Reboot`
//! and `Logout(0)` for its reboot and logout keys; they open the shell's
//! `org.gnome.SessionManager.EndSessionDialog` as gnome-session does.
//!
//! Semantics follow gnome-session 51 `gsm-manager.c`
//! (`gsm_manager_inhibit`, `_uninhibit`, `_is_inhibited`,
//! `_get_inhibitors`, `update_inhibited_actions`, `_shutdown`, `_reboot`,
//! `_logout`, `end_session_or_show_shell_dialog`), `gsm-shell.c` (the
//! dialog's `Open(type, 0, 60, inhibitors)` and its answers),
//! `gsm-inhibitor.c` (object paths
//! `/org/gnome/SessionManager/Inhibitor<serial>`, removal when the
//! caller's bus name vanishes) and `gsm-systemd.c` (the logind session's
//! `Active`; `PowerOff(true)`/`Reboot(true)`). gnome-session asks its
//! registered clients before the dialog (QueryEndSession); Tuna has no
//! client registration, so the logout inhibitors held here are the whole
//! answer. `RestoreSupported` is false, as gnome-session 51 sets it ("For
//! GNOME 50, we've disabled the save/restore feature",
//! `_gsm_manager_set_active_session`): GNOME Settings then hides its
//! "Reopen Windows After Login" switch, and window restoration is what
//! clients opt into through `xdg_session_manager_v1`
//! ([`crate::xdg_session`]). Not provided: client registration, presence,
//! the weak logind inhibitors gnome-session takes for the logout and
//! suspend flags, and the `RegisterRestore` family that only a session
//! supporting restore is asked.

use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use zbus::object_server::SignalEmitter;
use zbus::{interface, ObjectServer};

pub const NAME: &str = "org.gnome.SessionManager";
pub const PATH: &str = "/org/gnome/SessionManager";
const INHIBITOR_PREFIX: &str = "/org/gnome/SessionManager/Inhibitor";

/// `GsmInhibitorFlag` (gnome-session 51 `gsm-inhibitor-flag.h`).
pub const FLAG_LOGOUT: u32 = 1 << 0;
pub const FLAG_SWITCH_USER: u32 = 1 << 1;
pub const FLAG_SUSPEND: u32 = 1 << 2;
pub const FLAG_IDLE: u32 = 1 << 3;
pub const FLAG_AUTOMOUNT: u32 = 1 << 4;

/// One held inhibitor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inhibitor {
    pub path: String,
    pub cookie: u32,
    pub app_id: String,
    pub reason: String,
    pub flags: u32,
    /// Unique name of the caller; the inhibitor goes when it does.
    pub bus_name: String,
}

/// The inhibitor store, without D-Bus.
#[derive(Debug)]
pub struct Registry {
    inhibitors: Vec<Inhibitor>,
    /// `inhibitor_serial` in `gsm-inhibitor.c`: starts at 1, back to 1
    /// when it would turn negative as a signed 32-bit integer.
    next_serial: u32,
    seed: u64,
}

impl Default for Registry {
    fn default() -> Self {
        Self {
            inhibitors: Vec::new(),
            next_serial: 1,
            seed: std::collections::hash_map::RandomState::new()
                .build_hasher()
                .finish(),
        }
    }
}

impl Registry {
    /// `gsm_manager_inhibit`: an empty reason or zero flags is refused
    /// with gnome-session's messages; the app id may be empty.
    pub fn inhibit(
        &mut self,
        app_id: &str,
        reason: &str,
        flags: u32,
        bus_name: &str,
    ) -> Result<Inhibitor, &'static str> {
        if reason.is_empty() {
            return Err("Reason not specified");
        }
        if flags == 0 {
            return Err("Invalid inhibit flags");
        }
        let serial = self.next_serial;
        self.next_serial = if serial >= i32::MAX as u32 {
            1
        } else {
            serial + 1
        };
        let inhibitor = Inhibitor {
            path: format!("{INHIBITOR_PREFIX}{serial}"),
            cookie: self.unique_cookie(),
            app_id: app_id.to_owned(),
            reason: reason.to_owned(),
            flags,
            bus_name: bus_name.to_owned(),
        };
        self.inhibitors.push(inhibitor.clone());
        Ok(inhibitor)
    }

    /// `_generate_unique_cookie`: random in `[1, G_MAXINT32)`, never one
    /// already held.
    fn unique_cookie(&mut self) -> u32 {
        loop {
            self.seed = self
                .seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let cookie = 1 + ((self.seed >> 33) as u32) % (i32::MAX as u32 - 1);
            if !self.inhibitors.iter().any(|i| i.cookie == cookie) {
                return cookie;
            }
        }
    }

    /// `gsm_manager_uninhibit`: any caller may release any cookie.
    pub fn uninhibit(&mut self, cookie: u32) -> Option<Inhibitor> {
        let at = self.inhibitors.iter().position(|i| i.cookie == cookie)?;
        Some(self.inhibitors.remove(at))
    }

    /// A caller left the bus: drop everything it held.
    pub fn vanished(&mut self, bus_name: &str) -> Vec<Inhibitor> {
        let (gone, kept) = std::mem::take(&mut self.inhibitors)
            .into_iter()
            .partition(|i| i.bus_name == bus_name);
        self.inhibitors = kept;
        gone
    }

    /// `gsm_manager_is_inhibited`: any held inhibitor sharing a flag.
    pub fn is_inhibited(&self, flags: u32) -> bool {
        self.inhibitors.iter().any(|i| i.flags & flags != 0)
    }

    /// `InhibitedActions`: the union of every held inhibitor's flags.
    pub fn inhibited_actions(&self) -> u32 {
        self.inhibitors.iter().fold(0, |all, i| all | i.flags)
    }

    pub fn paths(&self) -> Vec<String> {
        self.inhibitors.iter().map(|i| i.path.clone()).collect()
    }

    fn holds(&self, bus_name: &str) -> bool {
        self.inhibitors.iter().any(|i| i.bus_name == bus_name)
    }
}

/// What the compositor loop reads: the published `InhibitedActions`.
#[derive(Debug, Clone, Default)]
pub struct Share {
    inhibited: Arc<AtomicU32>,
}

impl Share {
    /// An idle inhibitor is held (Tuna's idle lock and fade wait).
    pub fn idle_inhibited(&self) -> bool {
        self.inhibited.load(Ordering::Acquire) & FLAG_IDLE != 0
    }
}

#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.gnome.SessionManager")]
enum ManagerError {
    /// `GSM_MANAGER_ERROR_GENERAL`.
    GeneralError(String),
}

#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.gnome.SessionManager.Inhibitor")]
enum InhibitorError {
    /// `GSM_INHIBITOR_ERROR_NOT_SET`: D-Bus callers have no client id.
    NotSet(String),
}

struct Manager {
    registry: Arc<Mutex<Registry>>,
    share: Share,
    /// Atomic so the logind follower never waits for the interface's
    /// write lock behind an end-session request awaiting its dialog.
    active: AtomicBool,
    /// `(disable-log-out, logout-prompt)`, read per request.
    policy: Policy,
}

/// Reads the end-session policy: `(locked_down, logout_prompt)`.
type Policy = Arc<dyn Fn() -> (bool, bool) + Send + Sync>;

/// The live policy: org.gnome.desktop.lockdown `disable-log-out` and
/// org.gnome.SessionManager `logout-prompt`, GNOME's defaults when unread.
fn read_policy() -> (bool, bool) {
    (
        read_setting(
            crate::lockdown::SCHEMA,
            crate::lockdown::KEY_DISABLE_LOG_OUT,
        )
        .unwrap_or(false),
        read_setting(SESSION_SCHEMA, KEY_LOGOUT_PROMPT).unwrap_or(true),
    )
}

impl Manager {
    fn publish(&self, registry: &Registry) -> bool {
        let actions = registry.inhibited_actions();
        self.share.inhibited.swap(actions, Ordering::AcqRel) != actions
    }

    /// `Shutdown`, `Reboot` and `Logout`: gnome-session 51
    /// `gsm_manager_shutdown`, `_reboot`, `_logout` then
    /// `end_session_or_show_shell_dialog`. The dialog's confirmation
    /// carries the action out in the shell (through logind, as its power
    /// menu does); without a dialog the manager does it here.
    async fn end_session(
        &self,
        conn: &zbus::Connection,
        kind: EndSession,
        mode: u32,
    ) -> Result<(), EndSessionError> {
        let (logout_inhibited, inhibitors) = self
            .registry
            .lock()
            .map(|r| (r.is_inhibited(FLAG_LOGOUT), r.paths()))
            .unwrap_or_default();
        let policy = self.policy.clone();
        let (locked_down, prompt) = off_executor(move || policy())
            .await
            .unwrap_or((false, true));
        let route = route(mode, locked_down, prompt, logout_inhibited)?;
        eprintln!("tuna-compositor: session manager: {kind:?} (mode {mode}) -> {route:?}");
        // The nested preview's logind session is the host's: only the
        // shell's dialog, which ends just this compositor, may act there.
        if route == Route::Now && crate::session_services::hardware_session() {
            off_executor(move || perform(kind)).await;
            return Ok(());
        }
        // gsm_manager_logout replies at once; Shutdown and Reboot answer
        // when the end of the session is confirmed or cancelled.
        dialog(conn, kind, inhibitors, kind != EndSession::Logout).await
    }
}

#[interface(name = "org.gnome.SessionManager")]
impl Manager {
    /// The four wire arguments plus zbus's injected header, server and emitter.
    #[allow(clippy::too_many_arguments)]
    async fn inhibit(
        &self,
        app_id: &str,
        _toplevel_xid: u32,
        reason: &str,
        flags: u32,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> Result<u32, ManagerError> {
        let sender = header.sender().map(|s| s.to_string()).unwrap_or_default();
        let (inhibitor, changed) = {
            let mut registry = self
                .registry
                .lock()
                .map_err(|_| ManagerError::GeneralError("inhibitor store unavailable".into()))?;
            let inhibitor = registry
                .inhibit(app_id, reason, flags, &sender)
                .map_err(|e| ManagerError::GeneralError(e.into()))?;
            (inhibitor, self.publish(&registry))
        };
        let path = inhibitor.path.clone();
        let cookie = inhibitor.cookie;
        let _ = server.at(path.as_str(), InhibitorNode(inhibitor)).await;
        if let Ok(path) = zbus::zvariant::ObjectPath::try_from(path.as_str()) {
            let _ = Manager::inhibitor_added(&emitter, path).await;
        }
        if changed {
            let _ = self.inhibited_actions_changed(&emitter).await;
        }
        Ok(cookie)
    }

    async fn uninhibit(
        &self,
        cookie: u32,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> Result<(), ManagerError> {
        let (gone, changed) = {
            let mut registry = self
                .registry
                .lock()
                .map_err(|_| ManagerError::GeneralError("inhibitor store unavailable".into()))?;
            let gone = registry.uninhibit(cookie).ok_or_else(|| {
                ManagerError::GeneralError("Unable to uninhibit: Invalid cookie".into())
            })?;
            (gone, self.publish(&registry))
        };
        retire(server, &emitter, &[gone]).await;
        if changed {
            let _ = self.inhibited_actions_changed(&emitter).await;
        }
        Ok(())
    }

    fn is_inhibited(&self, flags: u32) -> bool {
        self.registry
            .lock()
            .map(|r| r.is_inhibited(flags))
            .unwrap_or(false)
    }

    fn get_inhibitors(&self) -> Vec<zbus::zvariant::OwnedObjectPath> {
        self.registry
            .lock()
            .map(|r| r.paths())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|p| zbus::zvariant::OwnedObjectPath::try_from(p).ok())
            .collect()
    }

    /// gnome-session 51 `Shutdown`: the end-session dialog, or a direct
    /// power off when `logout-prompt` is off and nothing inhibits logout.
    async fn shutdown(
        &self,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> Result<(), EndSessionError> {
        self.end_session(conn, EndSession::Shutdown, LOGOUT_MODE_NORMAL)
            .await
    }

    /// gnome-session 51 `Reboot`.
    async fn reboot(
        &self,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> Result<(), EndSessionError> {
        self.end_session(conn, EndSession::Reboot, LOGOUT_MODE_NORMAL)
            .await
    }

    /// gnome-session 51 `Logout(mode)`: 0 normal, 1 no confirmation,
    /// 2 forced.
    async fn logout(
        &self,
        mode: u32,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> Result<(), EndSessionError> {
        self.end_session(conn, EndSession::Logout, mode).await
    }

    #[zbus(property)]
    fn session_is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    #[zbus(property)]
    fn inhibited_actions(&self) -> u32 {
        self.share.inhibited.load(Ordering::Acquire)
    }

    /// gnome-session 51 reports no app save and restore; GNOME Settings
    /// reads this before showing "Reopen Windows After Login".
    #[zbus(property)]
    fn restore_supported(&self) -> bool {
        false
    }

    #[zbus(signal)]
    async fn inhibitor_added(
        emitter: &SignalEmitter<'_>,
        id: zbus::zvariant::ObjectPath<'_>,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn inhibitor_removed(
        emitter: &SignalEmitter<'_>,
        id: zbus::zvariant::ObjectPath<'_>,
    ) -> zbus::Result<()>;
}

/// Unexport released inhibitors and announce their removal.
async fn retire(server: &ObjectServer, emitter: &SignalEmitter<'_>, gone: &[Inhibitor]) {
    for inhibitor in gone {
        let _ = server
            .remove::<InhibitorNode, _>(inhibitor.path.as_str())
            .await;
        if let Ok(path) = zbus::zvariant::ObjectPath::try_from(inhibitor.path.as_str()) {
            let _ = Manager::inhibitor_removed(emitter, path).await;
        }
    }
}

/// gnome-session's settings schema and its confirmation key.
const SESSION_SCHEMA: &str = "org.gnome.SessionManager";
const KEY_LOGOUT_PROMPT: &str = "logout-prompt";
/// GNOME Shell's bus name and its end-session dialog (`gsm-shell.c`).
const SHELL_NAME: &str = "org.gnome.Shell";
pub const END_SESSION_DIALOG_PATH: &str = "/org/gnome/SessionManager/EndSessionDialog";
const END_SESSION_DIALOG_IFACE: &str = "org.gnome.SessionManager.EndSessionDialog";
/// `AUTOMATIC_ACTION_TIMEOUT` in `gsm-shell.c`: the dialog's countdown.
pub const AUTOMATIC_ACTION_TIMEOUT: u32 = 60;

/// `GsmManagerLogoutMode`.
pub const LOGOUT_MODE_NORMAL: u32 = 0;
pub const LOGOUT_MODE_NO_CONFIRMATION: u32 = 1;
pub const LOGOUT_MODE_FORCE: u32 = 2;

/// What a caller asked to end (`GsmManagerLogoutType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndSession {
    Logout,
    Shutdown,
    Reboot,
}

impl EndSession {
    /// `GsmShellEndSessionDialogType` for `Open`.
    pub fn dialog_type(self) -> u32 {
        match self {
            EndSession::Logout => 0,
            EndSession::Shutdown => 1,
            EndSession::Reboot => 2,
        }
    }
}

/// How a request ends the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// The shell's end-session dialog asks first.
    Dialog,
    /// No confirmation: the session ends at once.
    Now,
}

/// `gsm_manager_logout`'s checks, then `end_session_or_show_shell_dialog`:
/// `disable-log-out` refuses every request (Shutdown and Reboot too), an
/// unknown mode is refused, and the dialog shows when a normal request
/// meets `logout-prompt` or a logout inhibitor, or a no-confirmation one
/// meets an inhibitor; a forced logout never asks.
pub fn route(
    mode: u32,
    locked_down: bool,
    logout_prompt: bool,
    logout_inhibited: bool,
) -> Result<Route, EndSessionError> {
    if locked_down {
        return Err(EndSessionError::LockedDown);
    }
    let ask = match mode {
        LOGOUT_MODE_NORMAL => logout_inhibited || logout_prompt,
        LOGOUT_MODE_NO_CONFIRMATION => logout_inhibited,
        LOGOUT_MODE_FORCE => false,
        _ => return Err(EndSessionError::InvalidOption),
    };
    Ok(if ask { Route::Dialog } else { Route::Now })
}

/// gnome-session's answers to an end-of-session request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndSessionError {
    /// `GSM_MANAGER_ERROR_LOCKED_DOWN`.
    LockedDown,
    /// `GSM_MANAGER_ERROR_INVALID_OPTION`.
    InvalidOption,
    /// The dialog was cancelled: gnome-session fails the pending task
    /// with `G_IO_ERROR_CANCELLED`, which GDBus sends unmapped.
    Cancelled,
}

impl zbus::DBusError for EndSessionError {
    fn create_reply(&self, call: &zbus::message::Header<'_>) -> zbus::Result<zbus::Message> {
        zbus::Message::error(call, self.name())?.build(&(self.description().unwrap_or(""),))
    }

    fn name(&self) -> zbus::names::ErrorName<'_> {
        zbus::names::ErrorName::from_static_str_unchecked(match self {
            EndSessionError::LockedDown => "org.gnome.SessionManager.LockedDown",
            EndSessionError::InvalidOption => "org.gnome.SessionManager.InvalidOption",
            EndSessionError::Cancelled => {
                "org.gtk.GDBus.UnmappedGError.Quark._g_2dio_2derror_2dquark.Code19"
            }
        })
    }

    fn description(&self) -> Option<&str> {
        Some(match self {
            EndSessionError::LockedDown => "Logout has been locked down",
            EndSessionError::InvalidOption => "Unknown logout mode flag",
            EndSessionError::Cancelled => "Operation was cancelled",
        })
    }
}

impl std::fmt::Display for EndSessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use zbus::DBusError;
        write!(f, "{}: {}", self.name(), self.description().unwrap_or(""))
    }
}

impl std::error::Error for EndSessionError {}

/// Run blocking work (a `gsettings` read, a logind call) on its own
/// thread so the connection's executor keeps serving inhibitors.
async fn off_executor<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("tuna-session-end".into())
        .spawn(move || {
            let _ = tx.send(work());
        })
        .ok()?;
    loop {
        match rx.try_recv() {
            Ok(value) => return Some(value),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => return None,
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                async_io::Timer::after(std::time::Duration::from_millis(5)).await;
            }
        }
    }
}

fn read_setting(schema: &str, key: &str) -> Option<bool> {
    crate::lockdown::read_bool_via_command(
        "gsettings",
        &["get", schema, key],
        crate::lockdown::LOCKDOWN_READ_TIMEOUT,
    )
}

/// End the session without asking: `gsm-systemd.c` powers off and
/// reboots through logind interactively; a logout ends this logind
/// session, as the shell's Log Out does.
fn perform(kind: EndSession) {
    let result = zbus::blocking::Connection::system().and_then(|system| {
        match kind {
            EndSession::Shutdown | EndSession::Reboot => system.call_method(
                Some("org.freedesktop.login1"),
                "/org/freedesktop/login1",
                Some("org.freedesktop.login1.Manager"),
                if kind == EndSession::Shutdown {
                    "PowerOff"
                } else {
                    "Reboot"
                },
                &(true,),
            ),
            EndSession::Logout => system.call_method(
                Some("org.freedesktop.login1"),
                "/org/freedesktop/login1/session/auto",
                Some("org.freedesktop.login1.Session"),
                "Terminate",
                &(),
            ),
        }
        .map(|_| ())
    });
    if let Err(e) = result {
        eprintln!("tuna-compositor: session manager: {kind:?} failed: {e}");
    }
}

/// Whether the dialog's answer confirmed the end of the session.
fn dialog_answer(member: &str) -> Option<bool> {
    match member {
        // gnome-session follows a shutdown the shell promoted to a reboot.
        "ConfirmedLogout" | "ConfirmedReboot" | "ConfirmedShutdown" => Some(true),
        // Tuna's dialog emits Closed alone only for `Close`; no end of
        // the session follows, so the request is answered as cancelled.
        "Canceled" | "Closed" => Some(false),
        _ => None,
    }
}

/// `gsm_shell_open_end_session_dialog`, then (when `wait`) the answer:
/// a confirmation completes the request, a cancel, a failed `Open` or
/// the shell leaving the bus fails it as cancelled.
async fn dialog(
    conn: &zbus::Connection,
    kind: EndSession,
    inhibitors: Vec<String>,
    wait: bool,
) -> Result<(), EndSessionError> {
    let dbus = zbus::fdo::DBusProxy::new(conn)
        .await
        .map_err(|_| EndSessionError::Cancelled)?;
    let shell = zbus::names::BusName::WellKnown(
        zbus::names::WellKnownName::from_static_str_unchecked(SHELL_NAME),
    );
    let Ok(owner) = dbus.get_name_owner(shell.clone()).await else {
        eprintln!("tuna-compositor: session manager: no shell for the end-session dialog");
        return Err(EndSessionError::Cancelled);
    };
    let owner = owner.to_string();
    // Subscribe before Open so no answer can slip past.
    let answers = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface(END_SESSION_DIALOG_IFACE)
        .and_then(|b| b.path(END_SESSION_DIALOG_PATH))
        .map(|b| b.build())
        .map_err(|_| EndSessionError::Cancelled)?;
    let mut answers = zbus::MessageStream::for_match_rule(answers, conn, Some(16))
        .await
        .map_err(|_| EndSessionError::Cancelled)?;
    let mut owners = dbus
        .receive_name_owner_changed_with_args(&[(0, SHELL_NAME)])
        .await
        .map_err(|_| EndSessionError::Cancelled)?;
    let paths: Vec<zbus::zvariant::ObjectPath<'_>> = inhibitors
        .iter()
        .filter_map(|p| zbus::zvariant::ObjectPath::try_from(p.as_str()).ok())
        .collect();
    if let Err(e) = conn
        .call_method(
            Some(shell),
            END_SESSION_DIALOG_PATH,
            Some(END_SESSION_DIALOG_IFACE),
            "Open",
            &(kind.dialog_type(), 0u32, AUTOMATIC_ACTION_TIMEOUT, paths),
        )
        .await
    {
        eprintln!("tuna-compositor: session manager: end-session dialog did not open: {e}");
        return Err(EndSessionError::Cancelled);
    }
    if !wait {
        return Ok(());
    }
    use futures_lite::StreamExt;
    let answered = async {
        while let Some(Ok(message)) = answers.next().await {
            let header = message.header();
            if header.sender().map(|s| s.as_str()) != Some(owner.as_str()) {
                continue;
            }
            if let Some(answer) = header.member().and_then(|m| dialog_answer(m.as_str())) {
                return answer;
            }
        }
        false
    };
    let shell_left = async {
        while let Some(change) = owners.next().await {
            if change
                .args()
                .map(|a| a.new_owner().as_ref().map(|n| n.as_str()) != Some(owner.as_str()))
                .unwrap_or(false)
            {
                return false;
            }
        }
        false
    };
    if futures_lite::future::or(answered, shell_left).await {
        Ok(())
    } else {
        Err(EndSessionError::Cancelled)
    }
}

struct InhibitorNode(Inhibitor);

#[interface(name = "org.gnome.SessionManager.Inhibitor")]
impl InhibitorNode {
    fn get_app_id(&self) -> String {
        self.0.app_id.clone()
    }

    fn get_client_id(&self) -> Result<zbus::zvariant::OwnedObjectPath, InhibitorError> {
        Err(InhibitorError::NotSet("Value is not set".into()))
    }

    fn get_reason(&self) -> String {
        self.0.reason.clone()
    }

    fn get_flags(&self) -> u32 {
        self.0.flags
    }
}

/// Serve the manager on `conn` and drop each caller's inhibitors when
/// its unique name leaves the bus. Returns the thread watching for that.
fn serve_on(
    conn: &zbus::blocking::Connection,
    registry: Arc<Mutex<Registry>>,
    share: Share,
    active: bool,
) -> zbus::Result<std::thread::JoinHandle<()>> {
    serve_with(conn, registry, share, active, Arc::new(read_policy))
}

/// [`serve_on`] with the end-session policy reader (tests pin it).
fn serve_with(
    conn: &zbus::blocking::Connection,
    registry: Arc<Mutex<Registry>>,
    share: Share,
    active: bool,
    policy: Policy,
) -> zbus::Result<std::thread::JoinHandle<()>> {
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender("org.freedesktop.DBus")?
        .interface("org.freedesktop.DBus")?
        .member("NameOwnerChanged")?
        .path("/org/freedesktop/DBus")?
        .build();
    let departures = zbus::blocking::MessageIterator::for_match_rule(rule, conn, Some(64))?;
    conn.object_server().at(
        PATH,
        Manager {
            registry: registry.clone(),
            share: share.clone(),
            active: AtomicBool::new(active),
            policy,
        },
    )?;
    let conn = conn.clone();
    std::thread::Builder::new()
        .name("tuna-session-inhibitors".into())
        .spawn(move || {
            for message in departures {
                let Ok(message) = message else {
                    continue;
                };
                let Ok((name, _old, new)) =
                    message.body().deserialize::<(String, String, String)>()
                else {
                    continue;
                };
                if !new.is_empty() || !name.starts_with(':') {
                    continue;
                }
                let (gone, changed) = {
                    let Ok(mut registry) = registry.lock() else {
                        return;
                    };
                    if !registry.holds(&name) {
                        continue;
                    }
                    let gone = registry.vanished(&name);
                    let actions = registry.inhibited_actions();
                    (
                        gone,
                        share.inhibited.swap(actions, Ordering::AcqRel) != actions,
                    )
                };
                let Ok(manager) = conn.object_server().interface::<_, Manager>(PATH) else {
                    return;
                };
                let emitter = manager.signal_emitter().clone();
                zbus::block_on(async {
                    retire(conn.object_server().inner(), &emitter, &gone).await;
                    if changed {
                        let _ = manager.get().inhibited_actions_changed(&emitter).await;
                    }
                });
            }
        })
        .map_err(|e| zbus::Error::Failure(e.to_string()))
}

/// Whether this process's logind session is the active one
/// (`gsm-systemd.c`); without logind the session counts as active, as
/// gnome-session's null system does. Follows later changes into the
/// served `SessionIsActive`.
fn follow_logind_session(session: zbus::blocking::Connection) -> bool {
    let Ok(system) = zbus::blocking::Connection::system() else {
        return true;
    };
    let Ok(manager) = zbus::blocking::Proxy::new(
        &system,
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
    ) else {
        return true;
    };
    let path: zbus::Result<zbus::zvariant::OwnedObjectPath> = match std::env::var("XDG_SESSION_ID")
    {
        Ok(id) if !id.is_empty() => manager.call("GetSession", &(id.as_str(),)),
        _ => manager.call("GetSessionByPID", &(std::process::id(),)),
    };
    let Ok(path) = path else {
        return true;
    };
    let Ok(logind_session) = zbus::blocking::Proxy::new(
        &system,
        "org.freedesktop.login1",
        path,
        "org.freedesktop.login1.Session",
    ) else {
        return true;
    };
    let active = logind_session
        .get_property::<bool>("Active")
        .unwrap_or(true);
    let changes = logind_session.receive_property_changed::<bool>("Active");
    let _ = std::thread::Builder::new()
        .name("tuna-session-active".into())
        .spawn(move || {
            let _system = system;
            for change in changes {
                let Ok(active) = change.get() else {
                    continue;
                };
                let Ok(manager) = session.object_server().interface::<_, Manager>(PATH) else {
                    return;
                };
                zbus::block_on(async {
                    let node = manager.get();
                    if node.active.swap(active, Ordering::AcqRel) != active {
                        let _ = node
                            .session_is_active_changed(manager.signal_emitter())
                            .await;
                    }
                });
            }
        });
    active
}

/// Serve `org.gnome.SessionManager` from a thread. When a real session
/// manager already owns the name (a nested run inside GNOME) the service
/// stays off and nothing is ever inhibited here.
pub fn start() -> Share {
    let share = Share::default();
    let served = share.clone();
    let _ = std::thread::Builder::new()
        .name("tuna-session-manager".into())
        .spawn(move || {
            let conn = match zbus::blocking::Connection::session() {
                Ok(conn) => conn,
                Err(e) => {
                    eprintln!("tuna-compositor: session manager: no session bus: {e}");
                    return;
                }
            };
            let active = follow_logind_session(conn.clone());
            let registry = Arc::new(Mutex::new(Registry::default()));
            let watcher = match serve_on(&conn, registry, served, active) {
                Ok(watcher) => watcher,
                Err(e) => {
                    eprintln!("tuna-compositor: session manager: not served: {e}");
                    return;
                }
            };
            let flags = zbus::fdo::RequestNameFlags::DoNotQueue.into();
            match conn.request_name_with_flags(NAME, flags) {
                Ok(zbus::fdo::RequestNameReply::PrimaryOwner) => {
                    eprintln!("tuna-compositor: serving {NAME} (inhibitors, session activity)");
                    let _ = watcher.join();
                }
                _ => eprintln!("tuna-compositor: {NAME} is taken"),
            }
        });
    share
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    #[test]
    fn inhibit_validates_like_gnome_session_and_unions_flags() {
        let mut registry = Registry::default();
        assert_eq!(
            registry.inhibit("app", "", FLAG_IDLE, ":1.1"),
            Err("Reason not specified")
        );
        assert_eq!(
            registry.inhibit("app", "playing", 0, ":1.1"),
            Err("Invalid inhibit flags")
        );
        // gnome-session accepts an empty app id.
        let video = registry.inhibit("", "playing", FLAG_IDLE, ":1.1").unwrap();
        let copy = registry
            .inhibit(
                "org.gnome.Nautilus",
                "copying",
                FLAG_SUSPEND | FLAG_LOGOUT,
                ":1.2",
            )
            .unwrap();
        assert_eq!(video.path, "/org/gnome/SessionManager/Inhibitor1");
        assert_eq!(copy.path, "/org/gnome/SessionManager/Inhibitor2");
        assert_ne!(video.cookie, copy.cookie);
        assert!(video.cookie >= 1 && video.cookie < i32::MAX as u32);
        assert_eq!(
            registry.inhibited_actions(),
            FLAG_IDLE | FLAG_SUSPEND | FLAG_LOGOUT
        );
        assert!(registry.is_inhibited(FLAG_IDLE));
        assert!(registry.is_inhibited(FLAG_IDLE | FLAG_AUTOMOUNT));
        assert!(!registry.is_inhibited(FLAG_SWITCH_USER | FLAG_AUTOMOUNT));
        assert_eq!(registry.uninhibit(video.cookie), Some(video.clone()));
        assert_eq!(registry.uninhibit(video.cookie), None);
        assert_eq!(registry.inhibited_actions(), FLAG_SUSPEND | FLAG_LOGOUT);
        assert_eq!(registry.paths(), vec![copy.path.clone()]);
    }

    #[test]
    fn a_departed_caller_releases_only_its_own_inhibitors() {
        let mut registry = Registry::default();
        let a = registry.inhibit("a", "r", FLAG_IDLE, ":1.7").unwrap();
        let b = registry.inhibit("b", "r", FLAG_SUSPEND, ":1.8").unwrap();
        let c = registry.inhibit("c", "r", FLAG_LOGOUT, ":1.7").unwrap();
        assert_eq!(registry.vanished(":1.7"), vec![a, c]);
        assert_eq!(registry.inhibited_actions(), FLAG_SUSPEND);
        assert_eq!(registry.paths(), vec![b.path]);
        assert!(registry.vanished(":1.7").is_empty());
    }

    #[test]
    fn serials_wrap_back_to_one_before_turning_negative() {
        let mut registry = Registry {
            next_serial: i32::MAX as u32,
            ..Registry::default()
        };
        let last = registry.inhibit("a", "r", FLAG_IDLE, ":1.1").unwrap();
        let next = registry.inhibit("a", "r", FLAG_IDLE, ":1.1").unwrap();
        assert_eq!(last.path, format!("{INHIBITOR_PREFIX}{}", i32::MAX));
        assert_eq!(next.path, format!("{INHIBITOR_PREFIX}1"));
    }

    /// A private dbus-daemon for one test, killed on drop.
    struct TestBus {
        child: Child,
        address: String,
    }

    impl TestBus {
        fn spawn() -> Self {
            let mut child = Command::new("dbus-daemon")
                .args(["--session", "--nofork", "--print-address=1"])
                .stdout(Stdio::piped())
                .spawn()
                .expect("private test bus starts");
            let mut address = String::new();
            BufReader::new(child.stdout.take().expect("bus prints its address"))
                .read_line(&mut address)
                .expect("bus address line readable");
            Self {
                child,
                address: address.trim().to_owned(),
            }
        }

        fn connect(&self) -> zbus::blocking::Connection {
            zbus::blocking::connection::Builder::address(self.address.as_str())
                .expect("test address parses")
                .method_timeout(Duration::from_secs(5))
                .build()
                .expect("test peer connects")
        }
    }

    impl Drop for TestBus {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Uncached, so every property read asks the service.
    fn proxy<'a>(
        conn: &zbus::blocking::Connection,
        path: &'a str,
        interface: &'a str,
    ) -> zbus::blocking::Proxy<'a> {
        zbus::blocking::proxy::Builder::new(conn)
            .destination(NAME)
            .and_then(|b| b.path(path))
            .and_then(|b| b.interface(interface))
            .map(|b| b.cache_properties(zbus::proxy::CacheProperties::No))
            .and_then(|b| b.build())
            .unwrap()
    }

    #[test]
    fn bus_callers_inhibit_and_their_departure_releases_the_idle_lock() {
        let bus = TestBus::spawn();
        let service = bus.connect();
        let share = Share::default();
        let registry = Arc::new(Mutex::new(Registry::default()));
        let _watcher = serve_on(&service, registry, share.clone(), true).unwrap();
        service.request_name(NAME).unwrap();

        let observer = bus.connect();
        let manager = proxy(&observer, PATH, "org.gnome.SessionManager");
        assert!(manager.get_property::<bool>("SessionIsActive").unwrap());
        assert_eq!(manager.get_property::<u32>("InhibitedActions").unwrap(), 0);
        // GNOME Settings hides "Reopen Windows After Login" on this.
        assert!(!manager.get_property::<bool>("RestoreSupported").unwrap());
        let added = manager.receive_signal("InhibitorAdded").unwrap();

        // gsd-screensaver-proxy's call for a video player's
        // org.freedesktop.ScreenSaver.Inhibit.
        let player = bus.connect();
        let as_player = proxy(&player, PATH, "org.gnome.SessionManager");
        let cookie: u32 = as_player
            .call(
                "Inhibit",
                &("org.example.Player", 0u32, "Playing video", FLAG_IDLE),
            )
            .unwrap();
        assert_ne!(cookie, 0);
        let refused = as_player
            .call::<_, _, u32>("Inhibit", &("org.example.Player", 0u32, "", FLAG_IDLE))
            .unwrap_err();
        match refused {
            zbus::Error::MethodError(name, Some(message), _) => {
                assert_eq!(name.as_str(), "org.gnome.SessionManager.GeneralError");
                assert_eq!(message, "Reason not specified");
            }
            other => panic!("unexpected refusal {other:?}"),
        }
        assert!(share.idle_inhibited());
        let inhibited: bool = manager.call("IsInhibited", &(FLAG_IDLE,)).unwrap();
        assert!(inhibited);
        let inhibited: bool = manager.call("IsInhibited", &(FLAG_SUSPEND,)).unwrap();
        assert!(!inhibited);
        let paths: Vec<zbus::zvariant::OwnedObjectPath> =
            manager.call("GetInhibitors", &()).unwrap();
        assert_eq!(paths.len(), 1);
        let announced = added.into_iter().next().unwrap();
        let (announced,): (zbus::zvariant::OwnedObjectPath,) =
            announced.body().deserialize().unwrap();
        assert_eq!(announced, paths[0]);
        let node = proxy(
            &observer,
            paths[0].as_str(),
            "org.gnome.SessionManager.Inhibitor",
        );
        let reason: String = node.call("GetReason", &()).unwrap();
        let flags: u32 = node.call("GetFlags", &()).unwrap();
        let app: String = node.call("GetAppId", &()).unwrap();
        assert_eq!(
            (reason.as_str(), flags, app.as_str()),
            ("Playing video", FLAG_IDLE, "org.example.Player")
        );
        // Another caller's suspend inhibitor survives the player leaving.
        let other: u32 = manager
            .call(
                "Inhibit",
                &("org.example.Copy", 0u32, "Copying", FLAG_SUSPEND),
            )
            .unwrap();
        assert_eq!(
            manager.get_property::<u32>("InhibitedActions").unwrap(),
            FLAG_IDLE | FLAG_SUSPEND
        );

        drop(as_player);
        drop(player);
        wait_until("the player's inhibitor to go", || !share.idle_inhibited());
        let paths: Vec<zbus::zvariant::OwnedObjectPath> =
            manager.call("GetInhibitors", &()).unwrap();
        assert_eq!(paths.len(), 1);
        assert!(node.call::<_, _, String>("GetReason", &()).is_err());

        manager.call::<_, _, ()>("Uninhibit", &(other,)).unwrap();
        assert_eq!(manager.get_property::<u32>("InhibitedActions").unwrap(), 0);
        let invalid = manager
            .call::<_, _, ()>("Uninhibit", &(other,))
            .unwrap_err();
        assert!(
            matches!(&invalid, zbus::Error::MethodError(name, Some(message), _)
                if name.as_str() == "org.gnome.SessionManager.GeneralError"
                    && message == "Unable to uninhibit: Invalid cookie"),
            "{invalid:?}"
        );
    }

    #[test]
    fn end_of_session_routes_like_gnome_session_51() {
        use Route::{Dialog, Now};
        // disable-log-out refuses every request before the mode is read.
        assert_eq!(
            route(7, true, true, false),
            Err(EndSessionError::LockedDown)
        );
        assert_eq!(
            route(LOGOUT_MODE_FORCE, true, false, false),
            Err(EndSessionError::LockedDown)
        );
        assert_eq!(
            route(3, false, true, false),
            Err(EndSessionError::InvalidOption)
        );
        // Normal: the dialog unless logout-prompt is off and nothing inhibits.
        assert_eq!(route(LOGOUT_MODE_NORMAL, false, true, false), Ok(Dialog));
        assert_eq!(route(LOGOUT_MODE_NORMAL, false, false, true), Ok(Dialog));
        assert_eq!(route(LOGOUT_MODE_NORMAL, false, false, false), Ok(Now));
        // No confirmation: only an inhibitor asks.
        assert_eq!(
            route(LOGOUT_MODE_NO_CONFIRMATION, false, true, false),
            Ok(Now)
        );
        assert_eq!(
            route(LOGOUT_MODE_NO_CONFIRMATION, false, true, true),
            Ok(Dialog)
        );
        // Forced: never.
        assert_eq!(route(LOGOUT_MODE_FORCE, false, true, true), Ok(Now));
        // GsmShellEndSessionDialogType.
        assert_eq!(EndSession::Logout.dialog_type(), 0);
        assert_eq!(EndSession::Shutdown.dialog_type(), 1);
        assert_eq!(EndSession::Reboot.dialog_type(), 2);
        // The shell's answers (a shutdown promoted to a reboot counts).
        for confirmed in ["ConfirmedShutdown", "ConfirmedReboot", "ConfirmedLogout"] {
            assert_eq!(dialog_answer(confirmed), Some(true));
        }
        assert_eq!(dialog_answer("Canceled"), Some(false));
        assert_eq!(dialog_answer("Closed"), Some(false));
        assert_eq!(dialog_answer("Open"), None);
    }

    #[test]
    fn end_session_errors_carry_gnome_session_names() {
        use zbus::DBusError;
        assert_eq!(
            EndSessionError::LockedDown.name().as_str(),
            "org.gnome.SessionManager.LockedDown"
        );
        assert_eq!(
            EndSessionError::LockedDown.description(),
            Some("Logout has been locked down")
        );
        assert_eq!(
            EndSessionError::InvalidOption.name().as_str(),
            "org.gnome.SessionManager.InvalidOption"
        );
        assert_eq!(
            EndSessionError::Cancelled.name().as_str(),
            "org.gtk.GDBus.UnmappedGError.Quark._g_2dio_2derror_2dquark.Code19"
        );
    }

    type Opened = Arc<Mutex<Vec<(u32, u32, u32, Vec<String>)>>>;

    /// GNOME Shell's end-session dialog, recording each `Open`.
    struct FakeDialog(Opened);

    #[interface(name = "org.gnome.SessionManager.EndSessionDialog")]
    impl FakeDialog {
        fn open(
            &self,
            kind: u32,
            timestamp: u32,
            seconds: u32,
            inhibitors: Vec<zbus::zvariant::OwnedObjectPath>,
        ) {
            self.0.lock().unwrap().push((
                kind,
                timestamp,
                seconds,
                inhibitors.iter().map(|p| p.to_string()).collect(),
            ));
        }
    }

    fn answer(shell: &zbus::blocking::Connection, signal: &str) {
        shell
            .emit_signal(
                None::<&str>,
                END_SESSION_DIALOG_PATH,
                END_SESSION_DIALOG_IFACE,
                signal,
                &(),
            )
            .unwrap();
    }

    fn call_in_thread(
        bus: &TestBus,
        method: &'static str,
        mode: Option<u32>,
    ) -> std::thread::JoinHandle<zbus::Result<()>> {
        let caller = bus.connect();
        std::thread::spawn(move || {
            let manager = proxy(&caller, PATH, "org.gnome.SessionManager");
            match mode {
                Some(mode) => manager.call::<_, _, ()>(method, &(mode,)),
                None => manager.call::<_, _, ()>(method, &()),
            }
        })
    }

    fn error_name(result: zbus::Result<()>) -> String {
        match result {
            Err(zbus::Error::MethodError(name, _, _)) => name.to_string(),
            other => panic!("expected a D-Bus error, got {other:?}"),
        }
    }

    #[test]
    fn gsd_power_key_shutdown_opens_the_shells_dialog_and_follows_its_answer() {
        let bus = TestBus::spawn();
        let service = bus.connect();
        let locked_down = Arc::new(AtomicBool::new(false));
        let prompt = Arc::new(AtomicBool::new(true));
        let policy: Policy = {
            let (locked_down, prompt) = (locked_down.clone(), prompt.clone());
            Arc::new(move || {
                (
                    locked_down.load(Ordering::Acquire),
                    prompt.load(Ordering::Acquire),
                )
            })
        };
        let registry = Arc::new(Mutex::new(Registry::default()));
        let _watcher = serve_with(&service, registry, Share::default(), true, policy).unwrap();
        service.request_name(NAME).unwrap();

        let shell = bus.connect();
        let opened: Opened = Arc::default();
        shell
            .object_server()
            .at(END_SESSION_DIALOG_PATH, FakeDialog(opened.clone()))
            .unwrap();
        shell.request_name(SHELL_NAME).unwrap();

        // An application holds a logout inhibitor: its path reaches Open.
        let app = bus.connect();
        let as_app = proxy(&app, PATH, "org.gnome.SessionManager");
        let _cookie: u32 = as_app
            .call(
                "Inhibit",
                &("org.example.Editor", 0u32, "Unsaved", FLAG_LOGOUT),
            )
            .unwrap();

        // gsd-media-keys' Shutdown (power-button-action interactive): the
        // dialog opens with gsm-shell.c's arguments and the call waits.
        let shutdown = call_in_thread(&bus, "Shutdown", None);
        wait_until("Shutdown's dialog", || opened.lock().unwrap().len() == 1);
        assert_eq!(
            opened.lock().unwrap()[0],
            (
                1,
                0,
                AUTOMATIC_ACTION_TIMEOUT,
                vec![format!("{INHIBITOR_PREFIX}1")]
            )
        );
        std::thread::sleep(Duration::from_millis(200));
        assert!(
            !shutdown.is_finished(),
            "Shutdown answered before the dialog"
        );
        answer(&shell, "ConfirmedShutdown");
        shutdown.join().unwrap().unwrap();

        // Reboot, cancelled: gnome-session fails the task as cancelled.
        let reboot = call_in_thread(&bus, "Reboot", None);
        wait_until("Reboot's dialog", || opened.lock().unwrap().len() == 2);
        assert_eq!(opened.lock().unwrap()[1].0, 2);
        answer(&shell, "Canceled");
        answer(&shell, "Closed");
        assert_eq!(
            error_name(reboot.join().unwrap()),
            "org.gtk.GDBus.UnmappedGError.Quark._g_2dio_2derror_2dquark.Code19"
        );

        // Logout(0) answers once the dialog is open, as gsm_manager_logout.
        call_in_thread(&bus, "Logout", Some(LOGOUT_MODE_NORMAL))
            .join()
            .unwrap()
            .unwrap();
        assert_eq!(opened.lock().unwrap()[2].0, 0);
        assert_eq!(
            error_name(call_in_thread(&bus, "Logout", Some(9)).join().unwrap()),
            "org.gnome.SessionManager.InvalidOption"
        );

        // The nested preview (no hardware session) never ends a session
        // without the dialog, even with logout-prompt off.
        prompt.store(false, Ordering::Release);
        let forced = call_in_thread(&bus, "Logout", Some(LOGOUT_MODE_FORCE));
        forced.join().unwrap().unwrap();
        assert_eq!(opened.lock().unwrap()[3].0, 0);

        // A shell leaving the bus fails a waiting Shutdown.
        let pending = call_in_thread(&bus, "Shutdown", None);
        wait_until("the last dialog", || opened.lock().unwrap().len() == 5);
        shell.release_name(SHELL_NAME).unwrap();
        assert_eq!(
            error_name(pending.join().unwrap()),
            "org.gtk.GDBus.UnmappedGError.Quark._g_2dio_2derror_2dquark.Code19"
        );

        // disable-log-out refuses Shutdown before any dialog.
        locked_down.store(true, Ordering::Release);
        assert_eq!(
            error_name(call_in_thread(&bus, "Shutdown", None).join().unwrap()),
            "org.gnome.SessionManager.LockedDown"
        );
        assert_eq!(opened.lock().unwrap().len(), 5);
    }
}
