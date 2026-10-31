//! Power menu and end-session dialog (GNOME 51 shape, #62, #589).
//!
//! The quick-settings power button opens an in-panel menu: Suspend,
//! Restart…, Power Off…, Log Out. As GNOME Shell 51's `systemActions.js`,
//! each row asks the session manager on the session bus
//! (`org.gnome.SessionManager`: the compositor's `session_manager`, or
//! gnome-session where it runs): `Suspend()`, `Reboot()`, `Shutdown()`
//! and `Logout(0)`. The manager suspends at once; for the rest it checks
//! `disable-log-out`, `logout-prompt` and its logout inhibitors and opens
//! the dialog below, and it carries the confirmed end of the session out
//! itself (logind's `PowerOff`/`Reboot`, or the session leader's exit).
//!
//! The dialog is GNOME Shell 51's `org.gnome.SessionManager.EndSessionDialog`
//! on the shell's bus connection (endSessionDialog.js): a dimmed
//! full-screen layer with the 60-second countdown (shown in steps of ten
//! until the last ten seconds), the applications whose logout inhibitors
//! the manager passed ("Some applications are busy or have unsaved
//! work"), for Restart and Power Off the other users' logind sessions
//! ("Other users are logged in", greeter, closing and this session left
//! out), Cancel, and the action. Log Out names the user by their
//! AccountsService real name. When gnome-software has an offline update
//! prepared or scheduled, Restart and Power Off offer "Install pending
//! software updates" (a scheduled one turns Restart into "Restart &
//! Install Updates"), and with the battery discharging below 30 % the
//! dialog warns to plug in first. Holding Alt on Restart shows "Boot
//! Options" where logind can reboot into the boot loader menu. The
//! manager opens it for the menu and
//! for gnome-settings-daemon's power, logout, reboot and shutdown keys.
//! As in GNOME, Cancel emits `Canceled` then `Closed`, a confirmation or
//! the countdown's end emits `ConfirmedShutdown`, `ConfirmedReboot` or
//! `ConfirmedLogout`, and `Close` emits `Closed`; the shell itself never
//! ends the session.
//!
//! In the nested preview (`TUNA_SESSION_KIND=nested`) logind's session
//! is the host's: only Log Out is offered. It goes through the session
//! manager when that is this compositor's (the compositor's manager ends
//! only itself there); inside a host desktop whose session manager owns
//! the name, the dialog opens locally and Log Out ends this compositor,
//! never the host session.
//!
//! Before sleep, logind's `PrepareForSleep(true)` locks the screen, so
//! the session resumes locked from any suspend (menu, lid, idle).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gio::prelude::*;
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

const LOGIND: &str = "org.freedesktop.login1";
const MANAGER_PATH: &str = "/org/freedesktop/login1";
const MANAGER_IFACE: &str = "org.freedesktop.login1.Manager";
/// GNOME's end-session countdown.
pub const COUNTDOWN_S: u32 = 60;

/// The session manager the menu asks (systemActions.js `_session`).
const SESSION_MANAGER: &str = "org.gnome.SessionManager";
const SESSION_MANAGER_PATH: &str = "/org/gnome/SessionManager";
const INHIBITOR_IFACE: &str = "org.gnome.SessionManager.Inhibitor";
/// `GnomeSession.InhibitFlags.LOGOUT`.
const INHIBIT_LOGOUT: u32 = 1;
/// GNOME's application section title (endSessionDialog.js).
pub const BUSY_APPLICATIONS: &str = "Some applications are busy or have unsaved work";
/// GNOME's session section title (endSessionDialog.js `_sessionSection`).
pub const OTHER_USERS: &str = "Other users are logged in";
/// endSessionDialog.js `MAX_USERS_IN_SESSION_DIALOG`.
const MAX_USERS_IN_SESSION_DIALOG: usize = 5;
/// endSessionDialog.js `LOW_BATTERY_THRESHOLD` (percent).
pub const LOW_BATTERY_THRESHOLD: f64 = 30.0;
/// endSessionDialog.js `_batteryWarning`.
pub const LOW_BATTERY_WARNING: &str = "Low battery power: please plug in before installing updates";
/// endSessionDialog.js `checkBoxText` of Restart and Power Off.
pub const INSTALL_UPDATES: &str = "Install pending software updates";
/// The Restart button's Alt alternative (endSessionDialog.js `_updateButtons`).
pub const BOOT_OPTIONS: &str = "Boot Options";
/// loginManager.js `setRebootToBootLoaderMenu`: show the menu for 60 s.
const BOOT_LOADER_MENU_TIMEOUT_US: u64 = 60_000_000;
const ACCOUNTS: &str = "org.freedesktop.Accounts";
const UPOWER: &str = "org.freedesktop.UPower";
const DISPLAY_DEVICE: &str = "/org/freedesktop/UPower/devices/DisplayDevice";
/// gnome-software's offline updates (endSessionDialog.js
/// `SoftwareOfflineUpdatesProxy`).
const SOFTWARE: &str = "org.gnome.Software";
const OFFLINE_UPDATES_PATH: &str = "/org/gnome/Software/OfflineUpdates";
const OFFLINE_UPDATES_IFACE: &str = "org.gnome.Software.OfflineUpdates";

/// UPower's DisplayDevice as endSessionDialog.js `_isBatteryLow` reads
/// it: present, neither charging (1) nor fully charged (4), and below
/// [`LOW_BATTERY_THRESHOLD`].
pub fn battery_low(present: bool, state: u32, percentage: f64) -> bool {
    present && state != 1 && state != 4 && percentage < LOW_BATTERY_THRESHOLD
}

/// gnome-software's `GetState` answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateState {
    None,
    Prepared,
    Scheduled,
}

impl UpdateState {
    pub fn parse(state: &str) -> Self {
        match state {
            "prepared" => Self::Prepared,
            "scheduled" => Self::Scheduled,
            _ => Self::None,
        }
    }

    fn pending(self) -> bool {
        self != Self::None
    }
}

/// What one open dialog shows, endSessionDialog.js `DialogContent` with
/// the offline-update state it was opened with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Content {
    pub action: Action,
    /// `DialogType.UPDATE_RESTART`.
    pub update_restart: bool,
    /// gnome-software's state; `None` when it is not running.
    pub updates: Option<UpdateState>,
}

impl Content {
    /// Whether "Install pending software updates" shows (`OpenAsync`).
    pub fn checkbox_visible(self) -> bool {
        matches!(self.action, Action::Restart | Action::PowerOff)
            && !self.update_restart
            && self.updates.is_some_and(UpdateState::pending)
    }

    /// `_shouldShowLowBatteryWarning`.
    pub fn battery_warning(self, low: bool, checked: bool) -> bool {
        self.action != Action::LogOut
            && low
            && (checked || self.updates == Some(UpdateState::Scheduled))
    }

    /// `showOtherSessions`.
    pub fn shows_other_sessions(self) -> bool {
        matches!(self.action, Action::Restart | Action::PowerOff)
    }

    /// Title, countdown sentence and confirm label (`_sync`, with the
    /// user's real name once AccountsService has loaded it).
    pub fn text(
        self,
        checked: bool,
        real_name: Option<&str>,
        seconds: u32,
    ) -> (String, String, &'static str) {
        let unit = if seconds == 1 { "second" } else { "seconds" };
        match (self.action, real_name) {
            (Action::Restart, _) if self.update_restart => (
                "Restart & Install Updates".into(),
                format!(
                    "The system will automatically restart and install updates in {seconds} {unit}"
                ),
                "Restart & Install",
            ),
            (Action::Restart, _) if checked => (
                "Install Updates & Restart".into(),
                self.action.countdown_text(seconds),
                "Restart",
            ),
            (Action::PowerOff, _) if checked => (
                "Install Updates & Power Off".into(),
                self.action.countdown_text(seconds),
                "Power Off",
            ),
            (Action::LogOut, Some(name)) => (
                format!("Log Out {name}"),
                format!("{name} will be logged out automatically in {seconds} {unit}"),
                "Log Out",
            ),
            (action, _) => (
                action.title().into(),
                action.countdown_text(seconds),
                action.title(),
            ),
        }
    }
}

/// What a confirmation tells gnome-software first (endSessionDialog.js
/// `_confirm`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostUpdate {
    Nothing,
    /// `SetAction(action)`.
    SetAction(&'static str),
    /// `SetAction("shutdown")`; when it takes, reboot to install instead.
    ShutdownAfterInstall,
    /// `Cancel()`: the prepared update is declined.
    Cancel,
}

pub fn post_update(content: Content, checked: bool, signal: &str) -> PostUpdate {
    if content.checkbox_visible() {
        if !checked {
            return PostUpdate::Cancel;
        }
        return match signal {
            "ConfirmedReboot" => PostUpdate::SetAction("reboot"),
            "ConfirmedShutdown" => PostUpdate::ShutdownAfterInstall,
            _ => PostUpdate::Nothing,
        };
    }
    if content.update_restart && content.updates.is_some_and(UpdateState::pending) {
        return PostUpdate::SetAction("reboot");
    }
    PostUpdate::Nothing
}

/// libaccountsservice `act_user_get_real_name`: `RealName`, or the user
/// name when it is empty.
pub fn real_name(real: &str, user_name: &str) -> String {
    if real.is_empty() {
        user_name.to_owned()
    } else {
        real.to_owned()
    }
}

/// Whether endSessionDialog.js `_loadSessions` lists a logind session:
/// a user session (no greeter), not closing, and not this one.
pub fn lists_session(class: &str, state: &str, id: &str, own: &str) -> bool {
    class == "user" && state != "closing" && id != own
}

/// A listed session's title (`_loadSessions`): the real name (else the
/// user name) marked remote or console, and otherwise the user name
/// itself, as GNOME 51 labels it.
pub fn session_label(user_name: &str, real_name: Option<&str>, remote: bool, kind: &str) -> String {
    let display = real_name.unwrap_or(user_name);
    if remote {
        format!("{display} (remote)")
    } else if kind == "tty" {
        format!("{display} (console)")
    } else {
        user_name.to_owned()
    }
}

/// loginManager.js `canRebootToBootLoaderMenu`: `yes` or `challenge`.
pub fn boot_loader_menu_available(answer: &str) -> bool {
    matches!(answer, "yes" | "challenge")
}

/// The seconds the countdown shows with `left` of `total` remaining:
/// endSessionDialog.js `_roundSecondsToInterval(total, left, 10)`, which
/// steps by ten (rounding up) until the last ten seconds.
pub fn display_seconds(total: u32, left: u32) -> u32 {
    const INTERVAL: u32 = 10;
    if left <= INTERVAL {
        return left;
    }
    let time = left + INTERVAL - 1;
    if time > total {
        total
    } else {
        time - time % INTERVAL
    }
}

/// GNOME Shell's end-session dialog object on the shell's connection.
pub const END_SESSION_DIALOG_PATH: &str = "/org/gnome/SessionManager/EndSessionDialog";
const END_SESSION_DIALOG_IFACE: &str = "org.gnome.SessionManager.EndSessionDialog";
const END_SESSION_DIALOG_XML: &str = r#"<node>
  <interface name="org.gnome.SessionManager.EndSessionDialog">
    <method name="Open">
      <arg type="u" direction="in"/>
      <arg type="u" direction="in"/>
      <arg type="u" direction="in"/>
      <arg type="ao" direction="in"/>
    </method>
    <method name="Close"/>
    <signal name="ConfirmedLogout"/>
    <signal name="ConfirmedReboot"/>
    <signal name="ConfirmedShutdown"/>
    <signal name="Canceled"/>
    <signal name="Closed"/>
  </interface>
</node>"#;

/// The action an `Open` dialog type asks for: gnome-session's
/// `GsmShellEndSessionDialogType` (0 log out, 1 shutdown, 2 restart).
/// GNOME Shell's own type 3, the offline-update restart, is
/// [`open_type`]'s.
pub fn dialog_action(kind: u32) -> Option<Action> {
    match kind {
        0 => Some(Action::LogOut),
        1 => Some(Action::PowerOff),
        2 => Some(Action::Restart),
        _ => None,
    }
}

/// An `Open` type and whether it is GNOME Shell's `UPDATE_RESTART` (3).
pub fn open_type(kind: u32) -> Option<(Action, bool)> {
    match kind {
        3 => Some((Action::Restart, true)),
        kind => dialog_action(kind).map(|action| (action, false)),
    }
}

/// The signal a confirmed `action` emits (endSessionDialog.js `DialogContent`).
pub fn confirmed_signal(action: Action) -> Option<&'static str> {
    match action {
        Action::LogOut => Some("ConfirmedLogout"),
        Action::PowerOff => Some("ConfirmedShutdown"),
        Action::Restart => Some("ConfirmedReboot"),
        Action::Suspend => None,
    }
}

/// One end-session action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Suspend,
    Restart,
    PowerOff,
    LogOut,
}

impl Action {
    /// Dialog title and confirm button label.
    pub fn title(self) -> &'static str {
        match self {
            Action::Suspend => "Suspend",
            Action::Restart => "Restart",
            Action::PowerOff => "Power Off",
            Action::LogOut => "Log Out",
        }
    }

    /// GNOME's countdown sentence for `seconds` left.
    pub fn countdown_text(self, seconds: u32) -> String {
        let unit = if seconds == 1 { "second" } else { "seconds" };
        match self {
            Action::Restart => {
                format!("The system will restart automatically in {seconds} {unit}")
            }
            Action::PowerOff => {
                format!("The system will power off automatically in {seconds} {unit}")
            }
            // GNOME 51's strings, without a closing period.
            Action::LogOut => format!("You will be logged out automatically in {seconds} {unit}"),
            Action::Suspend => String::new(),
        }
    }
}

/// Which menu rows a session offers.
pub fn menu_actions(nested: bool) -> &'static [Action] {
    if nested {
        &[Action::LogOut]
    } else {
        &[
            Action::Suspend,
            Action::Restart,
            Action::PowerOff,
            Action::LogOut,
        ]
    }
}

/// Whether the power menu itself shows under a lockdown policy: any
/// visible row keeps it (GNOME 51 `status/system.js` ShutdownItem
/// `_sync`), so a nested session (whose only row is Log Out) shows no
/// menu while `disable-log-out` denies, and hardware keeps Suspend.
pub fn power_menu_visible_for(actions: &[Action], logout_allowed: bool) -> bool {
    actions
        .iter()
        .any(|action| action_allowed(*action, logout_allowed))
}

/// Whether `action` may run while `logout_allowed` reflects the live
/// `disable-log-out` key. GNOME 51 gates every session-ending action
/// on it: gnome-session 51.0 `gsm-manager.c` answers `CanShutdown` and
/// `CanReboot` `UNAVAILABLE` and refuses `Shutdown`, `Reboot` and
/// `Logout` ("Logout has been locked down") while it is set, and
/// gnome-shell's `systemActions.js` re-queries both on the key's
/// change, hiding Restart… and Power Off… with Log Out…. Suspend
/// (`CanSuspend`) ignores the key and always runs.
pub fn action_allowed(action: Action, logout_allowed: bool) -> bool {
    action == Action::Suspend || logout_allowed
}

/// Whether a pending end-session dialog must die under the live policy:
/// a Log Out, Restart or Power Off countdown open when `disable-log-out`
/// engages (gnome-session refuses to leave the Running phase then).
/// An idle dialog never cancels.
pub fn should_cancel_pending(pending: Option<Action>, logout_allowed: bool) -> bool {
    matches!(pending, Some(action) if !action_allowed(action, logout_allowed))
}

/// Whether this shell runs inside another desktop.
pub fn is_nested() -> bool {
    std::env::var("TUNA_SESSION_KIND").map_or(true, |kind| kind != "hardware")
}

/// The session manager method a menu row calls and its arguments
/// (systemActions.js `activateSuspend`, `activateRestart`,
/// `activatePowerOff`, `activateLogout`).
pub fn manager_call(action: Action) -> (&'static str, Option<glib::Variant>) {
    match action {
        Action::Suspend => ("Suspend", None),
        Action::Restart => ("Reboot", None),
        Action::PowerOff => ("Shutdown", None),
        Action::LogOut => ("Logout", Some((0u32,).to_variant())),
    }
}

/// Ask the session manager for `action`. `Shutdown` and `Reboot` answer
/// once the dialog is confirmed or cancelled, so the call never times
/// out; a cancelled request is quiet (`logErrorUnlessCancelled`).
fn call_manager(bus: &gio::DBusConnection, action: Action) {
    let (method, args) = manager_call(action);
    eprintln!("tuna-shell-gtk: power menu: {method} via {SESSION_MANAGER}");
    bus.call(
        Some(SESSION_MANAGER),
        SESSION_MANAGER_PATH,
        SESSION_MANAGER,
        method,
        args.as_ref(),
        None,
        gio::DBusCallFlags::NONE,
        i32::MAX,
        None::<&gio::Cancellable>,
        move |reply| match reply {
            Ok(_) => {}
            Err(e) if e.matches(gio::IOErrorEnum::Cancelled) => {
                eprintln!("tuna-shell-gtk: power menu: {method} cancelled");
            }
            Err(e) => eprintln!("tuna-shell-gtk: power menu: {method} failed: {e}"),
        },
    );
}

/// Whether the session manager on `bus` is this shell's compositor
/// (the parent process), whose manager ends only itself in a nested
/// preview; a host desktop's would end the host session.
async fn manager_is_compositors(bus: &gio::DBusConnection) -> bool {
    let call = |method: &'static str, arg: String| {
        bus.call_future(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            method,
            Some(&(arg,).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            5000,
        )
    };
    let Ok(owner) = call("GetNameOwner", SESSION_MANAGER.to_owned()).await else {
        return false;
    };
    let Some(owner) = owner.child_value(0).get::<String>() else {
        return false;
    };
    let Ok(pid) = call("GetConnectionUnixProcessID", owner).await else {
        return false;
    };
    pid.child_value(0).get::<u32>() == Some(unsafe { libc::getppid() } as u32)
}

/// End the supervising compositor, this process's parent, which shuts
/// down cleanly on SIGTERM. An adoptive parent is never signalled.
fn end_compositor() {
    let parent = unsafe { libc::getppid() };
    let comm = std::fs::read_to_string(format!("/proc/{parent}/comm")).unwrap_or_default();
    if parent > 1 && is_compositor_comm(&comm) {
        unsafe {
            libc::kill(parent, libc::SIGTERM);
        }
    } else {
        eprintln!("tuna-shell-gtk: parent {parent} is not the compositor; Log Out not sent");
    }
}

/// Whether a `/proc/<pid>/comm` value names the compositor. The kernel
/// keeps the first 15 bytes of the executable name, which is all of
/// `tuna-compositor`.
fn is_compositor_comm(comm: &str) -> bool {
    comm.trim_end() == "tuna-compositor"
}

/// The nested preview's own Log Out, when the session manager on its bus
/// is a host desktop's: end this compositor, never the host session.
/// Nothing else is offered there.
fn perform_local(action: Action) {
    if action == Action::LogOut {
        end_compositor();
    }
}

/// One inhibitor the manager passed `Open`, as endSessionDialog.js
/// `_onInhibitorLoaded` lists it: an installed application (its app id
/// names a desktop file) holding a logout inhibitor, with its reason.
/// Services and other flags are left out.
async fn inhibiting_app(
    bus: &gio::DBusConnection,
    path: &str,
) -> Option<(tuna_shell_host::apps::AppEntry, String)> {
    let get = |method: &'static str| {
        bus.call_future(
            Some(SESSION_MANAGER),
            path,
            INHIBITOR_IFACE,
            method,
            None,
            None,
            gio::DBusCallFlags::NONE,
            5000,
        )
    };
    let app_id = get("GetAppId").await.ok()?.child_value(0).get::<String>()?;
    let app = crate::notify::desktop_app(app_id.trim_end_matches(".desktop"))?;
    let flags = get("GetFlags").await.ok()?.child_value(0).get::<u32>()?;
    if flags & INHIBIT_LOGOUT == 0 {
        return None;
    }
    let reason = get("GetReason")
        .await
        .ok()?
        .child_value(0)
        .get::<String>()?;
    Some((app, reason))
}

/// One D-Bus call with a 5 s timeout.
async fn call(
    bus: &gio::DBusConnection,
    name: &str,
    path: &str,
    iface: &str,
    method: &str,
    args: Option<glib::Variant>,
    flags: gio::DBusCallFlags,
) -> Result<glib::Variant, glib::Error> {
    bus.call_future(
        Some(name),
        path,
        iface,
        method,
        args.as_ref(),
        None,
        flags,
        5000,
    )
    .await
}

/// Every property of `iface` on `path`.
async fn properties(
    bus: &gio::DBusConnection,
    name: &str,
    path: &str,
    iface: &str,
) -> Option<glib::VariantDict> {
    let all = call(
        bus,
        name,
        path,
        "org.freedesktop.DBus.Properties",
        "GetAll",
        Some((iface,).to_variant()),
        gio::DBusCallFlags::NONE,
    )
    .await
    .ok()?;
    Some(glib::VariantDict::new(Some(&all.child_value(0))))
}

fn dict_string(dict: &glib::VariantDict, key: &str) -> String {
    dict.lookup::<String>(key)
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// gnome-software's offline update state (endSessionDialog.js
/// `_getUpdateState`), or `None` when gnome-software is not running
/// (it is never started for the dialog).
async fn offline_update_state(bus: &gio::DBusConnection) -> Option<UpdateState> {
    let state = call(
        bus,
        SOFTWARE,
        OFFLINE_UPDATES_PATH,
        OFFLINE_UPDATES_IFACE,
        "GetState",
        None,
        gio::DBusCallFlags::NO_AUTO_START,
    )
    .await;
    match state {
        Ok(state) => Some(UpdateState::parse(
            &state.child_value(0).get::<String>().unwrap_or_default(),
        )),
        Err(e)
            if e.matches(gio::DBusError::ServiceUnknown)
                || e.matches(gio::DBusError::NameHasNoOwner) =>
        {
            None
        }
        Err(e) => {
            // GNOME keeps the proxy and treats the state as neither.
            eprintln!("tuna-shell-gtk: Failed to get update info from gnome-software: {e}");
            Some(UpdateState::None)
        }
    }
}

/// Tell gnome-software what follows the offline update
/// (`_setPostUpdateAction`); whether it took.
async fn set_post_update_action(bus: &gio::DBusConnection, action: &str) -> bool {
    let set = call(
        bus,
        SOFTWARE,
        OFFLINE_UPDATES_PATH,
        OFFLINE_UPDATES_IFACE,
        "SetAction",
        Some((action,).to_variant()),
        gio::DBusCallFlags::NO_AUTO_START,
    )
    .await;
    if let Err(e) = &set {
        eprintln!("tuna-shell-gtk: offline update SetAction({action}): {e}");
    }
    set.is_ok()
}

/// `_triggerOfflineUpdateCancel`.
async fn cancel_offline_update(bus: &gio::DBusConnection) {
    if let Err(e) = call(
        bus,
        SOFTWARE,
        OFFLINE_UPDATES_PATH,
        OFFLINE_UPDATES_IFACE,
        "Cancel",
        None,
        gio::DBusCallFlags::NO_AUTO_START,
    )
    .await
    {
        eprintln!("tuna-shell-gtk: offline update Cancel: {e}");
    }
}

/// A user as AccountsService knows them: the real name
/// (`act_user_get_real_name`) and the avatar file, or `None` while the
/// user is not loaded (AccountsService absent or the user unknown).
async fn account(
    system: &gio::DBusConnection,
    user_name: &str,
) -> Option<(String, Option<String>)> {
    let path = call(
        system,
        ACCOUNTS,
        "/org/freedesktop/Accounts",
        ACCOUNTS,
        "FindUserByName",
        Some((user_name,).to_variant()),
        gio::DBusCallFlags::NONE,
    )
    .await
    .ok()?
    .child_value(0);
    let path = path.str()?.to_owned();
    let user = properties(system, ACCOUNTS, &path, "org.freedesktop.Accounts.User").await?;
    let name = dict_string(&user, "UserName");
    let icon =
        Some(dict_string(&user, "IconFile")).filter(|icon| std::path::Path::new(icon).is_file());
    Some((
        real_name(
            &dict_string(&user, "RealName"),
            if name.is_empty() { user_name } else { &name },
        ),
        icon,
    ))
}

/// This process's logind session id (loginManager.js
/// `getCurrentSessionProxy`): `XDG_SESSION_ID`, else the session of
/// this PID.
async fn own_session_id(system: &gio::DBusConnection) -> Option<String> {
    if let Some(id) = std::env::var("XDG_SESSION_ID")
        .ok()
        .filter(|id| !id.is_empty())
    {
        return Some(id);
    }
    let path = call(
        system,
        LOGIND,
        MANAGER_PATH,
        MANAGER_IFACE,
        "GetSessionByPID",
        Some((std::process::id(),).to_variant()),
        gio::DBusCallFlags::NONE,
    )
    .await
    .ok()?
    .child_value(0);
    let session = properties(
        system,
        LOGIND,
        path.str()?,
        "org.freedesktop.login1.Session",
    )
    .await?;
    Some(dict_string(&session, "Id"))
}

/// The other users' sessions, titled and with their avatars
/// (endSessionDialog.js `_loadSessions`), at most
/// [`MAX_USERS_IN_SESSION_DIALOG`].
async fn other_sessions(system: &gio::DBusConnection) -> Vec<(String, Option<String>)> {
    let Some(own) = own_session_id(system).await else {
        return Vec::new();
    };
    let Ok(list) = call(
        system,
        LOGIND,
        MANAGER_PATH,
        MANAGER_IFACE,
        "ListSessions",
        None,
        gio::DBusCallFlags::NONE,
    )
    .await
    else {
        return Vec::new();
    };
    let mut listed = Vec::new();
    for entry in list.child_value(0).iter() {
        let user_name = entry.child_value(2).get::<String>().unwrap_or_default();
        let Some(path) = entry.child_value(4).str().map(str::to_owned) else {
            continue;
        };
        let Some(session) =
            properties(system, LOGIND, &path, "org.freedesktop.login1.Session").await
        else {
            continue;
        };
        if !lists_session(
            &dict_string(&session, "Class"),
            &dict_string(&session, "State"),
            &dict_string(&session, "Id"),
            &own,
        ) {
            continue;
        }
        let remote = session
            .lookup::<bool>("Remote")
            .ok()
            .flatten()
            .unwrap_or(false);
        let (real, icon) = account(system, &user_name).await.unzip();
        let label = session_label(
            &user_name,
            real.as_deref(),
            remote,
            &dict_string(&session, "Type"),
        );
        listed.push((label, icon.flatten()));
        if listed.len() == MAX_USERS_IN_SESSION_DIALOG {
            break;
        }
    }
    listed
}

/// loginManager.js `canRebootToBootLoaderMenu`.
async fn can_reboot_to_boot_loader_menu(system: &gio::DBusConnection) -> bool {
    call(
        system,
        LOGIND,
        MANAGER_PATH,
        MANAGER_IFACE,
        "CanRebootToBootLoaderMenu",
        None,
        gio::DBusCallFlags::NONE,
    )
    .await
    .ok()
    .and_then(|answer| answer.child_value(0).get::<String>())
    .is_some_and(|answer| boot_loader_menu_available(&answer))
}

/// The power surfaces: the menu rows for quick settings, the dialog.
pub struct PowerUi {
    nested: bool,
    dialog: gtk::ApplicationWindow,
    /// The modal fade in and out (`ModalDialog`, 100 ms).
    fade: crate::transient::ModalFade,
    title: gtk::Label,
    body: gtk::Label,
    confirm: gtk::Button,
    /// Restart's Alt alternative, "Boot Options" (`_rebootButtonAlt`).
    boot_options: gtk::Button,
    /// "Install pending software updates" (`_checkBox`).
    updates_check: gtk::CheckButton,
    /// `_batteryWarning`.
    battery_warning: gtk::Label,
    /// "Some applications are busy or have unsaved work" and its list.
    applications: gtk::Box,
    application_list: gtk::Box,
    /// "Other users are logged in" and its list (`_sessionSection`).
    sessions: gtk::Box,
    session_list: gtk::Box,
    /// The open dialog's content.
    content: Cell<Content>,
    /// The user's AccountsService real name, once loaded.
    real_name: RefCell<Option<String>>,
    /// logind can reboot into the boot loader menu (`_canRebootToBootLoaderMenu`).
    can_boot_menu: Cell<bool>,
    /// Alt swaps Restart for Boot Options (`_altCapture.enabled`).
    alt_capture: Cell<bool>,
    /// Inhibitors still listed or loading (`_applications.length`).
    inhibitors: Cell<usize>,
    pending: Cell<Option<Action>>,
    /// A confirmed dialog whose service call has not yet completed.
    /// Close still owes its waiting manager a Closed signal.
    confirming: Cell<bool>,
    /// The open dialog is the nested preview's own (no manager waits on
    /// its answer; it acts itself).
    local: Cell<bool>,
    /// Invalidates asynchronous loads and confirmations when a request
    /// is replaced, canceled or closed.
    serial: Cell<u64>,
    total: Cell<u32>,
    left: Cell<u32>,
    timer: RefCell<Option<glib::SourceId>>,
    /// The session bus, where the dialog's D-Bus signals go.
    bus: RefCell<Option<gio::DBusConnection>>,
    /// The system bus: logind, AccountsService.
    system: RefCell<Option<gio::DBusConnection>>,
    /// UPower's DisplayDevice (`_powerProxy`).
    upower: RefCell<Option<gio::DBusProxy>>,
}

impl PowerUi {
    /// Build the dialog; `lock` locks the session (before sleep).
    pub fn new(app: &gtk::Application, lock: Rc<dyn Fn()>) -> Rc<Self> {
        let nested = is_nested();
        let dialog = gtk::ApplicationWindow::new(app);
        dialog.add_css_class("tuna-end-session");
        dialog.init_layer_shell();
        dialog.set_layer(Layer::Overlay);
        dialog.set_namespace(Some(tuna_shell_control::END_SESSION_NAMESPACE));
        for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
            dialog.set_anchor(edge, true);
        }
        // Cover the top bar too: a modal dims the whole screen.
        dialog.set_exclusive_zone(-1);
        dialog.set_keyboard_mode(KeyboardMode::Exclusive);
        dialog.set_title(Some("End Session"));
        // GNOME's ModalDialog (.modal-dialog.end-session-dialog): a 24em
        // card, the title and countdown centered, two 43px buttons.
        let card = gtk::Box::new(gtk::Orientation::Vertical, 18);
        card.add_css_class("modal-dialog");
        card.set_halign(gtk::Align::Center);
        card.set_valign(gtk::Align::Center);
        card.set_size_request(400, -1);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 18);
        content.add_css_class("modal-dialog-content-box");
        let title = gtk::Label::new(None);
        title.add_css_class("message-dialog-title");
        let body = gtk::Label::new(None);
        body.add_css_class("message-dialog-description");
        body.set_wrap(true);
        body.set_max_width_chars(1);
        body.set_hexpand(true);
        body.set_justify(gtk::Justification::Center);
        content.append(&title);
        content.append(&body);
        // MessageDialogContent's check box and battery warning.
        let updates_check = gtk::CheckButton::with_label(INSTALL_UPDATES);
        updates_check.set_halign(gtk::Align::Center);
        updates_check.set_visible(false);
        let battery_warning = gtk::Label::new(Some(LOW_BATTERY_WARNING));
        battery_warning.add_css_class("end-session-dialog-battery-warning");
        battery_warning.set_wrap(true);
        battery_warning.set_max_width_chars(1);
        battery_warning.set_hexpand(true);
        battery_warning.set_justify(gtk::Justification::Center);
        battery_warning.set_visible(false);
        content.append(&updates_check);
        content.append(&battery_warning);
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        buttons.add_css_class("modal-dialog-button-box");
        buttons.set_homogeneous(true);
        let cancel = gtk::Button::with_label("Cancel");
        cancel.add_css_class("modal-dialog-button");
        let confirm = gtk::Button::with_label("");
        confirm.add_css_class("modal-dialog-button");
        let boot_options = gtk::Button::with_label(BOOT_OPTIONS);
        boot_options.add_css_class("modal-dialog-button");
        boot_options.set_visible(false);
        buttons.append(&cancel);
        buttons.append(&confirm);
        buttons.append(&boot_options);
        // endSessionDialog.js `_applicationSection` and `_sessionSection`:
        // Dialog.ListSections under the message, shown while listing.
        let (applications, application_list) = list_section(BUSY_APPLICATIONS);
        let (sessions, session_list) = list_section(OTHER_USERS);
        card.append(&content);
        card.append(&applications);
        card.append(&sessions);
        card.append(&buttons);
        dialog.set_child(Some(&card));

        let ui = Rc::new(Self {
            fade: crate::transient::ModalFade::new(&dialog),
            nested,
            dialog,
            title,
            body,
            confirm,
            boot_options,
            updates_check,
            battery_warning,
            applications,
            application_list,
            sessions,
            session_list,
            content: Cell::new(Content {
                action: Action::LogOut,
                update_restart: false,
                updates: None,
            }),
            real_name: RefCell::new(None),
            can_boot_menu: Cell::new(false),
            alt_capture: Cell::new(false),
            inhibitors: Cell::new(0),
            pending: Cell::new(None),
            confirming: Cell::new(false),
            local: Cell::new(false),
            serial: Cell::new(0),
            total: Cell::new(0),
            left: Cell::new(0),
            timer: RefCell::new(None),
            bus: RefCell::new(None),
            system: RefCell::new(None),
            upower: RefCell::new(None),
        });
        {
            let me = ui.clone();
            cancel.connect_clicked(move |_| me.cancel());
        }
        {
            let me = ui.clone();
            ui.confirm.connect_clicked(move |_| me.run_pending(false));
        }
        {
            let me = ui.clone();
            ui.boot_options
                .connect_clicked(move |_| me.run_pending(true));
        }
        {
            // The check box re-syncs the title and warning (`clicked`).
            let me = ui.clone();
            ui.updates_check.connect_toggled(move |_| me.sync());
        }
        {
            // Escape cancels, as in GNOME; Alt swaps Restart for Boot
            // Options while held (`_onKeyCaptured`).
            let keys = gtk::EventControllerKey::new();
            keys.set_propagation_phase(gtk::PropagationPhase::Capture);
            let me = ui.clone();
            keys.connect_key_pressed(move |_, key, _, _| {
                if key == gtk::gdk::Key::Escape {
                    me.cancel();
                    return glib::Propagation::Stop;
                }
                me.alt_held(key, true);
                glib::Propagation::Proceed
            });
            let me = ui.clone();
            keys.connect_key_released(move |_, key, _, _| me.alt_held(key, false));
            ui.dialog.add_controller(keys);
        }
        // System bus: the before-sleep lock, the boot loader menu check
        // (`_getCanRebootToBootLoaderMenu`) and UPower's display device.
        let weak = Rc::downgrade(&ui);
        gio::bus_get(
            gio::BusType::System,
            None::<&gio::Cancellable>,
            move |conn| {
                let Ok(conn) = conn else {
                    return;
                };
                if let Some(me) = weak.upgrade() {
                    *me.system.borrow_mut() = Some(conn.clone());
                    me.watch_battery(&conn);
                    let (me, conn) = (me.clone(), conn.clone());
                    glib::spawn_future_local(async move {
                        me.can_boot_menu
                            .set(can_reboot_to_boot_loader_menu(&conn).await);
                    });
                }
                let sub = conn.subscribe_to_signal(
                    Some(LOGIND),
                    Some(MANAGER_IFACE),
                    Some("PrepareForSleep"),
                    Some(MANAGER_PATH),
                    None,
                    gio::DBusSignalFlags::NONE,
                    move |signal| {
                        let going = signal
                            .parameters
                            .try_child_value(0)
                            .and_then(|v| v.get::<bool>())
                            .unwrap_or(false);
                        if going {
                            lock();
                        }
                    },
                );
                // Held for the session, with the subscription.
                std::mem::forget(sub);
                std::mem::forget(conn);
            },
        );
        // Administrative lockdown (#357 follow-up): a pending Log Out,
        // Restart or Power Off dialog dies when `disable-log-out`
        // engages, so an already-open countdown can never end the
        // session after the flip.
        if let Some(lockdown) = crate::lockdown::settings() {
            let weak = Rc::downgrade(&ui);
            lockdown.connect_changed(None, move |_, _| {
                if let Some(me) = weak.upgrade() {
                    me.apply_lockdown(crate::lockdown::read().entry_visibility().log_out);
                }
            });
            // Keep the settings object (and its signal) alive.
            std::mem::forget(lockdown);
        }
        serve_end_session_dialog(&ui);
        ui
    }

    /// Menu rows for this session, wired to their actions. Each row
    /// calls `close_menu` first (the quick-settings popover goes away).
    /// The actions the power menu offers here.
    pub fn actions(&self) -> &'static [Action] {
        menu_actions(self.nested)
    }

    /// Run a menu choice through the session manager, as GNOME Shell
    /// 51's `systemActions.js`: Suspend at once, the rest through the
    /// manager's end-session dialog. A Log Out, Restart or Power Off
    /// while `disable-log-out` denies is refused (the action is not
    /// available), live per activation.
    pub fn activate(self: &Rc<Self>, action: Action) {
        if !action_allowed(action, crate::lockdown::read().entry_visibility().log_out) {
            eprintln!("tuna-shell-gtk: {action:?} refused by administrator policy");
            return;
        }
        let Some(bus) = self.bus.borrow().clone() else {
            eprintln!("tuna-shell-gtk: no session bus for {action:?}");
            return;
        };
        if !self.nested {
            call_manager(&bus, action);
            return;
        }
        let me = self.clone();
        glib::spawn_future_local(async move {
            if manager_is_compositors(&bus).await {
                call_manager(&bus, action);
            } else {
                eprintln!(
                    "tuna-shell-gtk: power menu: the session manager is not this compositor's; {action:?} stays local"
                );
                let content = Content {
                    action,
                    update_restart: false,
                    updates: None,
                };
                me.ask_for(content, COUNTDOWN_S, Vec::new(), true);
            }
        });
    }

    /// Open the dialog for `content` with a fresh countdown of `seconds`
    /// (`Open`'s `seconds_to_stay_open`), listing the applications behind
    /// `inhibitors` and, for Restart and Power Off, the other users'
    /// sessions; `local` when the nested preview opens it itself.
    /// A Log Out, Restart or Power Off request while `disable-log-out`
    /// denies never opens (live read per open, so no stale path can
    /// bypass it). Whether it opened.
    fn ask_for(
        self: &Rc<Self>,
        content: Content,
        seconds: u32,
        inhibitors: Vec<String>,
        local: bool,
    ) -> bool {
        let action = content.action;
        if !action_allowed(action, crate::lockdown::read().entry_visibility().log_out) {
            return false;
        }
        self.stop_timer();
        let serial = self.serial.get() + 1;
        self.serial.set(serial);
        self.confirming.set(false);
        self.pending.set(Some(action));
        self.content.set(content);
        self.local.set(local);
        self.total.set(seconds);
        self.left.set(seconds);
        // `OpenAsync`: the check box shows with an update pending and
        // starts checked unless the battery is low.
        self.updates_check.set_visible(content.checkbox_visible());
        self.updates_check
            .set_active(content.checkbox_visible() && !self.battery_is_low());
        // `_updateButtons`: Boot Options backs ConfirmedReboot where
        // logind can reboot into the boot loader menu.
        self.alt_capture.set(
            !local
                && self.can_boot_menu.get()
                && confirmed_signal(action) == Some("ConfirmedReboot"),
        );
        self.confirm.set_visible(true);
        self.boot_options.set_visible(false);
        self.list_inhibitors(inhibitors, serial);
        self.list_sessions(content, serial);
        self.load_real_name(content, serial);
        self.sync();
        self.fade.present();
        // Cancel holds the focus when the dialog opens, as in GNOME.
        if let Some(cancel) = self.confirm.prev_sibling().and_downcast::<gtk::Button>() {
            cancel.grab_focus();
        }
        let me = self.clone();
        let id = glib::timeout_add_local(Duration::from_secs(1), move || {
            let left = me.left.get().saturating_sub(1);
            me.left.set(left);
            me.sync();
            if left == 0 {
                me.timer.borrow_mut().take();
                me.run_pending(false);
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
        *self.timer.borrow_mut() = Some(id);
        true
    }

    /// endSessionDialog.js `_sync`: title, countdown, confirm label and
    /// battery warning from the open dialog's state.
    fn sync(&self) {
        if self.pending.get().is_none() {
            return;
        }
        let content = self.content.get();
        let checked = self.updates_check.is_visible() && self.updates_check.is_active();
        let seconds = display_seconds(self.total.get(), self.left.get());
        let (title, body, confirm) =
            content.text(checked, self.real_name.borrow().as_deref(), seconds);
        self.title.set_text(&title);
        self.body.set_text(&body);
        self.confirm.set_label(confirm);
        self.battery_warning
            .set_visible(content.battery_warning(self.battery_is_low(), checked));
    }

    /// `_isBatteryLow`, from UPower's cached DisplayDevice properties.
    fn battery_is_low(&self) -> bool {
        let Some(proxy) = self.upower.borrow().clone() else {
            return false;
        };
        let get = |name: &str| proxy.cached_property(name);
        battery_low(
            get("IsPresent")
                .and_then(|v| v.get::<bool>())
                .unwrap_or(false),
            get("State").and_then(|v| v.get::<u32>()).unwrap_or(0),
            get("Percentage")
                .and_then(|v| v.get::<f64>())
                .unwrap_or(100.0),
        )
    }

    /// `_powerProxy`: UPower's DisplayDevice, re-syncing on changes.
    fn watch_battery(self: &Rc<Self>, system: &gio::DBusConnection) {
        let weak = Rc::downgrade(self);
        gio::DBusProxy::new(
            system,
            gio::DBusProxyFlags::DO_NOT_AUTO_START,
            None,
            Some(UPOWER),
            DISPLAY_DEVICE,
            "org.freedesktop.UPower.Device",
            None::<&gio::Cancellable>,
            move |proxy| {
                let (Ok(proxy), Some(me)) = (proxy, weak.upgrade()) else {
                    return;
                };
                let weak = Rc::downgrade(&me);
                proxy.connect_local("g-properties-changed", false, move |_| {
                    if let Some(me) = weak.upgrade() {
                        me.sync();
                    }
                    None
                });
                *me.upower.borrow_mut() = Some(proxy);
                me.sync();
            },
        );
    }

    /// The user's real name for Log Out (`this._user.get_real_name()`),
    /// fetched from AccountsService for each Log Out dialog.
    fn load_real_name(self: &Rc<Self>, content: Content, serial: u64) {
        if content.action != Action::LogOut {
            return;
        }
        let Some(system) = self.system.borrow().clone() else {
            return;
        };
        let me = self.clone();
        glib::spawn_future_local(async move {
            let name = account(&system, &glib::user_name().to_string_lossy())
                .await
                .map(|(name, _)| name);
            if me.serial.get() == serial {
                *me.real_name.borrow_mut() = name;
                me.sync();
            }
        });
    }

    /// Fill "Other users are logged in" (`_loadSessions`), for Restart
    /// and Power Off.
    fn list_sessions(self: &Rc<Self>, content: Content, serial: u64) {
        while let Some(row) = self.session_list.first_child() {
            self.session_list.remove(&row);
        }
        self.sessions.set_visible(false);
        if !content.shows_other_sessions() {
            return;
        }
        let Some(system) = self.system.borrow().clone() else {
            return;
        };
        let me = self.clone();
        glib::spawn_future_local(async move {
            let sessions = other_sessions(&system).await;
            if me.serial.get() != serial {
                return;
            }
            for (label, icon) in &sessions {
                me.add_session(label, icon.as_deref());
            }
            me.sessions.set_visible(!sessions.is_empty());
        });
    }

    /// One session's Dialog.ListSectionItem: the user's 64 px avatar
    /// (UserWidget.Avatar) and title.
    fn add_session(&self, label: &str, icon: Option<&str>) {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.add_css_class("dialog-list-item");
        let avatar = icon.map_or_else(
            || gtk::Image::from_icon_name("avatar-default-symbolic"),
            gtk::Image::from_file,
        );
        avatar.set_pixel_size(64);
        let title = gtk::Label::new(Some(label));
        title.add_css_class("dialog-list-item-title");
        title.set_xalign(0.0);
        title.set_valign(gtk::Align::Center);
        row.append(&avatar);
        row.append(&title);
        self.session_list.append(&row);
    }

    /// `_onKeyCaptured`: while Alt is held, Boot Options stands in for
    /// Restart.
    fn alt_held(&self, key: gtk::gdk::Key, pressed: bool) {
        if !self.alt_capture.get() || !matches!(key, gtk::gdk::Key::Alt_L | gtk::gdk::Key::Alt_R) {
            return;
        }
        self.confirm.set_visible(!pressed);
        self.boot_options.set_visible(pressed);
    }

    /// Fill the application section from the manager's inhibitors
    /// (endSessionDialog.js `OpenAsync`, `_onInhibitorLoaded`): shown
    /// while any is still loading or names an application that inhibits
    /// logging out.
    fn list_inhibitors(self: &Rc<Self>, inhibitors: Vec<String>, serial: u64) {
        while let Some(row) = self.application_list.first_child() {
            self.application_list.remove(&row);
        }
        self.inhibitors.set(inhibitors.len());
        self.applications.set_visible(!inhibitors.is_empty());
        let Some(bus) = self.bus.borrow().clone() else {
            return;
        };
        for path in inhibitors {
            let (me, bus) = (self.clone(), bus.clone());
            glib::spawn_future_local(async move {
                let app = inhibiting_app(&bus, &path).await;
                if me.serial.get() != serial {
                    return;
                }
                match app {
                    Some((app, reason)) => me.add_application(&app, &reason),
                    None => {
                        let left = me.inhibitors.get().saturating_sub(1);
                        me.inhibitors.set(left);
                        me.applications.set_visible(left > 0);
                    }
                }
            });
        }
    }

    /// One Dialog.ListSectionItem: the app's 64 px icon, name and reason.
    fn add_application(&self, app: &tuna_shell_host::apps::AppEntry, reason: &str) {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.add_css_class("dialog-list-item");
        let icon = app
            .icon
            .as_deref()
            .and_then(crate::notify::source_icon)
            .map(|icon| gtk::Image::from_gicon(&icon))
            .unwrap_or_else(|| gtk::Image::from_icon_name("application-x-executable"));
        icon.set_pixel_size(64);
        let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
        text.set_valign(gtk::Align::Center);
        let name = gtk::Label::new(Some(&app.name));
        name.add_css_class("dialog-list-item-title");
        name.set_xalign(0.0);
        let description = gtk::Label::new(Some(reason));
        description.add_css_class("dialog-list-item-description");
        description.set_xalign(0.0);
        description.set_wrap(true);
        text.append(&name);
        text.append(&description);
        row.append(&icon);
        row.append(&text);
        self.application_list.append(&row);
    }

    fn stop_timer(&self) {
        if let Some(id) = self.timer.borrow_mut().take() {
            id.remove();
        }
    }

    /// A signal for the session manager awaiting this dialog's answer;
    /// the nested preview's own dialog has none.
    fn emit(&self, signal: &str) {
        if self.local.get() {
            return;
        }
        if let Some(bus) = self.bus.borrow().as_ref() {
            let _ = bus.emit_signal(
                None,
                END_SESSION_DIALOG_PATH,
                END_SESSION_DIALOG_IFACE,
                signal,
                None,
            );
        }
    }

    fn cancel(&self) {
        self.serial.set(self.serial.get() + 1);
        self.stop_timer();
        let confirming = self.confirming.replace(false);
        let open = self.pending.take().is_some() || confirming;
        self.fade.hide();
        if open {
            self.emit("Canceled");
            self.emit("Closed");
        }
    }

    /// `Close`: dismiss without an answer.
    fn close(&self) {
        self.serial.set(self.serial.get() + 1);
        self.stop_timer();
        let confirming = self.confirming.replace(false);
        let open = self.pending.take().is_some() || confirming;
        self.fade.hide();
        if open {
            self.emit("Closed");
        }
    }

    /// Cancel a pending session-ending dialog while `disable-log-out`
    /// denies. The live watcher calls this on every policy change;
    /// harmless when nothing is pending.
    pub fn apply_lockdown(&self, logout_allowed: bool) {
        if should_cancel_pending(self.pending.get(), logout_allowed) {
            self.cancel();
        }
    }

    /// Confirm the open dialog (endSessionDialog.js `_confirm`), through
    /// Boot Options when `boot_menu` (`_confirmRebootToBootLoaderMenu`):
    /// logind is told to show the boot loader menu, gnome-software what
    /// follows a pending update, and then the confirmation goes out.
    fn run_pending(self: &Rc<Self>, boot_menu: bool) {
        self.stop_timer();
        self.fade.hide();
        self.alt_capture.set(false);
        let Some(action) = self.pending.take() else {
            return;
        };
        // Re-check at fire time: an action denied since the dialog
        // opened must not end the session (gnome-session refuses
        // `end_phase` from Running while locked down).
        if !action_allowed(action, crate::lockdown::read().entry_visibility().log_out) {
            eprintln!("tuna-shell-gtk: {action:?} refused by administrator policy");
            self.emit("Canceled");
            self.emit("Closed");
            return;
        }
        // The manager ends the session on the confirmation; only the
        // nested preview's own dialog acts here.
        if self.local.get() {
            perform_local(action);
            return;
        }
        let Some(mut signal) = confirmed_signal(action) else {
            return;
        };
        let (Some(bus), system) = (self.bus.borrow().clone(), self.system.borrow().clone()) else {
            return;
        };
        let checked = self.updates_check.is_visible() && self.updates_check.is_active();
        let plan = post_update(self.content.get(), checked, signal);
        let serial = self.serial.get();
        self.confirming.set(true);
        let me = self.clone();
        glib::spawn_future_local(async move {
            if let (true, Some(system)) = (boot_menu, system.as_ref()) {
                // Awaited, so logind holds the flag before the manager
                // asks it to reboot.
                if let Err(e) = call(
                    system,
                    LOGIND,
                    MANAGER_PATH,
                    MANAGER_IFACE,
                    "SetRebootToBootLoaderMenu",
                    Some((BOOT_LOADER_MENU_TIMEOUT_US,).to_variant()),
                    gio::DBusCallFlags::NONE,
                )
                .await
                {
                    eprintln!("tuna-shell-gtk: SetRebootToBootLoaderMenu: {e}");
                }
            }
            if me.serial.get() != serial {
                return;
            }
            match plan {
                PostUpdate::Nothing => {}
                PostUpdate::SetAction(action) => {
                    set_post_update_action(&bus, action).await;
                }
                PostUpdate::ShutdownAfterInstall => {
                    // Installing reboots, and gnome-software powers off
                    // once the update is in.
                    if set_post_update_action(&bus, "shutdown").await {
                        signal = "ConfirmedReboot";
                    }
                }
                PostUpdate::Cancel => cancel_offline_update(&bus).await,
            }
            // A newer Open or Close invalidates a confirmation whose
            // logind/software call was still in flight. Signals carry
            // no request id: a stale one would answer the new request.
            if me.serial.get() != serial {
                return;
            }
            me.confirming.set(false);
            if !action_allowed(action, crate::lockdown::read().entry_visibility().log_out) {
                me.emit("Canceled");
                me.emit("Closed");
                return;
            }
            let _ = bus.emit_signal(
                None,
                END_SESSION_DIALOG_PATH,
                END_SESSION_DIALOG_IFACE,
                signal,
                None,
            );
        });
    }
}

/// A Dialog.ListSection: its title over a scrolled list, hidden until
/// something is listed.
fn list_section(title: &str) -> (gtk::Box, gtk::Box) {
    let section = gtk::Box::new(gtk::Orientation::Vertical, 18);
    section.add_css_class("dialog-list");
    let heading = gtk::Label::new(Some(title));
    heading.add_css_class("dialog-list-title");
    heading.set_wrap(true);
    heading.set_max_width_chars(1);
    heading.set_hexpand(true);
    heading.set_justify(gtk::Justification::Center);
    let list = gtk::Box::new(gtk::Orientation::Vertical, 12);
    list.add_css_class("dialog-list-box");
    let scroll = gtk::ScrolledWindow::new();
    scroll.add_css_class("dialog-list-scrollview");
    scroll.set_hscrollbar_policy(gtk::PolicyType::Never);
    scroll.set_propagate_natural_height(true);
    scroll.set_max_content_height(200);
    scroll.set_child(Some(&list));
    section.append(&heading);
    section.append(&scroll);
    section.set_visible(false);
    (section, list)
}

/// Export the dialog as GNOME Shell does, on the shared session
/// connection that also carries `org.gnome.Shell`.
fn serve_end_session_dialog(ui: &Rc<PowerUi>) {
    let info = gio::DBusNodeInfo::for_xml(END_SESSION_DIALOG_XML)
        .ok()
        .and_then(|node| node.lookup_interface(END_SESSION_DIALOG_IFACE));
    let Some(info) = info else {
        eprintln!("tuna-shell-gtk: end-session dialog interface did not parse");
        return;
    };
    let weak = Rc::downgrade(ui);
    gio::bus_get(
        gio::BusType::Session,
        None::<&gio::Cancellable>,
        move |conn| {
            let Ok(conn) = conn else {
                return;
            };
            let Some(ui) = weak.upgrade() else {
                return;
            };
            *ui.bus.borrow_mut() = Some(conn.clone());
            let weak = Rc::downgrade(&ui);
            let registered = conn
                .register_object(END_SESSION_DIALOG_PATH, &info)
                .method_call(move |_, _, _, _, method, params, invocation| {
                    let Some(ui) = weak.upgrade() else {
                        invocation.return_dbus_error(
                            "org.freedesktop.DBus.Error.Failed",
                            "the shell is shutting down",
                        );
                        return;
                    };
                    match method {
                        "Open" => {
                            let Some((kind, seconds, inhibitors)) = open_args(&params) else {
                                invocation.return_dbus_error(
                                    "org.freedesktop.DBus.Error.InvalidArgs",
                                    "Open takes (uuuao)",
                                );
                                return;
                            };
                            let Some((action, update_restart)) = open_type(kind) else {
                                // endSessionDialog.js ModalDialogError.UNKNOWN_TYPE.
                                invocation.return_dbus_error(
                                    "org.gnome.Shell.ModalDialog.Error.UnknownType",
                                    "Unknown dialog type requested",
                                );
                                return;
                            };
                            // Reserve the request before any await.
                            // Close and a newer Open invalidate this
                            // load as well as any older confirmation.
                            let serial = ui.serial.get() + 1;
                            ui.serial.set(serial);
                            ui.stop_timer();
                            ui.pending.set(None);
                            ui.confirming.set(false);
                            ui.alt_capture.set(false);
                            ui.fade.hide();
                            // `OpenAsync` asks gnome-software first; a
                            // scheduled update turns Restart into
                            // `UPDATE_RESTART`.
                            let bus = ui.bus.borrow().clone();
                            glib::spawn_future_local(async move {
                                let updates = match &bus {
                                    Some(bus) => offline_update_state(bus).await,
                                    None => None,
                                };
                                if ui.serial.get() != serial {
                                    invocation.return_dbus_error(
                                        "org.freedesktop.DBus.Error.Cancelled",
                                        "The end-session dialog request was superseded",
                                    );
                                    return;
                                }
                                let content = Content {
                                    action,
                                    update_restart: update_restart
                                        || (action == Action::Restart
                                            && updates == Some(UpdateState::Scheduled)),
                                    updates,
                                };
                                if ui.ask_for(content, seconds, inhibitors, false) {
                                    eprintln!(
                                        "tuna-shell-gtk: end-session dialog: {} opened over D-Bus (updates {updates:?})",
                                        action.title()
                                    );
                                    invocation.return_value(None);
                                } else {
                                    invocation.return_dbus_error(
                                        "org.freedesktop.DBus.Error.AccessDenied",
                                        "Ending the session is disabled by the administrator",
                                    );
                                }
                            });
                        }
                        "Close" => {
                            ui.close();
                            invocation.return_value(None);
                        }
                        _ => invocation
                            .return_dbus_error("org.freedesktop.DBus.Error.UnknownMethod", method),
                    }
                })
                .build();
            if let Err(e) = registered {
                eprintln!("tuna-shell-gtk: end-session dialog object: {e}");
            }
        },
    );
}

/// `Open`'s dialog type, seconds to stay open and inhibitor object
/// paths, when the call is `(uuuao)`.
fn open_args(params: &glib::Variant) -> Option<(u32, u32, Vec<String>)> {
    if params.type_().as_str() != "(uuuao)" {
        return None;
    }
    let inhibitors = params
        .child_value(3)
        .iter()
        .filter_map(|path| path.str().map(str::to_owned))
        .collect();
    Some((
        params.child_value(0).get()?,
        params.child_value(2).get()?,
        inhibitors,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_process_running_the_compositor_binary_is_recognised_by_its_comm() {
        let dir = std::env::temp_dir().join(format!("tuna-comm-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("tuna-compositor");
        // A symlink, not a copy: the kernel names the process after the
        // path it executes, and a freshly written executable can fail with
        // ETXTBSY when another test thread forks while it is still open.
        std::os::unix::fs::symlink("/bin/sleep", &exe).unwrap();
        let mut child = std::process::Command::new(&exe).arg("5").spawn().unwrap();
        let comm = std::fs::read_to_string(format!("/proc/{}/comm", child.id())).unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(is_compositor_comm(&comm), "comm was {comm:?}");
        assert!(!is_compositor_comm("tuna-shell-gtk\n"));
    }

    #[test]
    fn nested_sessions_only_offer_log_out() {
        assert_eq!(menu_actions(true), [Action::LogOut]);
        assert_eq!(
            menu_actions(false),
            [
                Action::Suspend,
                Action::Restart,
                Action::PowerOff,
                Action::LogOut
            ]
        );
    }

    #[test]
    fn lockdown_hides_nested_menu_but_keeps_hardware_menu() {
        // Negative: nested sessions lose their only row and show no menu.
        assert!(!power_menu_visible_for(menu_actions(true), false));
        // Positive: allowed policies show both menus.
        assert!(power_menu_visible_for(menu_actions(true), true));
        assert!(power_menu_visible_for(menu_actions(false), true));
        // Hardware keeps its power rows while Log Out hides.
        assert!(power_menu_visible_for(menu_actions(false), false));
    }

    #[test]
    fn disable_log_out_denies_every_session_ending_action_but_suspend() {
        // Open/fire gate (gnome-session 51 CanShutdown/CanReboot and
        // Logout): Log Out, Restart and Power Off need the policy;
        // Suspend has no lockdown gate and always runs.
        for action in [Action::LogOut, Action::Restart, Action::PowerOff] {
            assert!(!action_allowed(action, false), "{action:?} denied");
            assert!(action_allowed(action, true), "{action:?} allowed");
        }
        assert!(action_allowed(Action::Suspend, false));
        assert!(action_allowed(Action::Suspend, true));
        // Pending-dialog denial: an open session-ending countdown dies
        // when the policy engages; Suspend and an idle dialog never do.
        for action in [Action::LogOut, Action::Restart, Action::PowerOff] {
            assert!(should_cancel_pending(Some(action), false), "{action:?}");
            assert!(!should_cancel_pending(Some(action), true), "{action:?}");
        }
        assert!(!should_cancel_pending(None, false));
        assert!(!should_cancel_pending(Some(Action::Suspend), false));
    }

    #[test]
    fn end_session_dialog_types_and_signals_match_gnome_51() {
        // gnome-session 51 GsmShellEndSessionDialogType.
        assert_eq!(dialog_action(0), Some(Action::LogOut));
        assert_eq!(dialog_action(1), Some(Action::PowerOff));
        assert_eq!(dialog_action(2), Some(Action::Restart));
        assert_eq!(dialog_action(3), None);
        assert_eq!(dialog_action(u32::MAX), None);
        // endSessionDialog.js confirm buttons' signals.
        assert_eq!(
            confirmed_signal(Action::PowerOff),
            Some("ConfirmedShutdown")
        );
        assert_eq!(confirmed_signal(Action::Restart), Some("ConfirmedReboot"));
        assert_eq!(confirmed_signal(Action::LogOut), Some("ConfirmedLogout"));
        assert_eq!(confirmed_signal(Action::Suspend), None);
        // The interface parses and declares exactly GNOME's members.
        let node = gio::DBusNodeInfo::for_xml(END_SESSION_DIALOG_XML).unwrap();
        let iface = node.lookup_interface(END_SESSION_DIALOG_IFACE).unwrap();
        assert!(iface.lookup_method("Open").is_some());
        assert!(iface.lookup_method("Close").is_some());
        // gsm-shell.c sends `(uuu@ao)`: type, timestamp, seconds, inhibitors.
        let inhibitors = glib::Variant::array_from_iter_with_type(
            glib::VariantTy::OBJECT_PATH,
            [
                glib::variant::ObjectPath::try_from("/org/gnome/SessionManager/Inhibitor1")
                    .unwrap()
                    .to_variant(),
            ],
        );
        let open = glib::Variant::tuple_from_iter([
            1u32.to_variant(),
            0u32.to_variant(),
            60u32.to_variant(),
            inhibitors,
        ]);
        assert_eq!(
            open_args(&open),
            Some((1, 60, vec!["/org/gnome/SessionManager/Inhibitor1".into()]))
        );
        assert_eq!(open_args(&(1u32, 0u32, 60u32).to_variant()), None);
        for signal in [
            "ConfirmedLogout",
            "ConfirmedReboot",
            "ConfirmedShutdown",
            "Canceled",
            "Closed",
        ] {
            assert!(iface.lookup_signal(signal).is_some(), "{signal}");
        }
    }

    #[test]
    fn countdown_text_matches_gnome() {
        assert_eq!(
            Action::PowerOff.countdown_text(60),
            "The system will power off automatically in 60 seconds"
        );
        assert_eq!(
            Action::Restart.countdown_text(1),
            "The system will restart automatically in 1 second"
        );
        assert_eq!(
            Action::LogOut.countdown_text(5),
            "You will be logged out automatically in 5 seconds"
        );
    }

    #[test]
    fn countdown_steps_by_ten_until_the_last_ten_seconds_like_gnome() {
        // endSessionDialog.js _roundSecondsToInterval(60, left, 10).
        let shown: Vec<u32> = (1..=60)
            .rev()
            .map(|left| display_seconds(60, left))
            .collect();
        let mut expected = Vec::new();
        for step in [60, 50, 40, 30, 20] {
            expected.extend(std::iter::repeat_n(step, 10));
        }
        expected.extend((1..=10).rev());
        assert_eq!(shown, expected);
        // A shorter Open rounds up no higher than its own total.
        assert_eq!(display_seconds(15, 15), 15);
        assert_eq!(display_seconds(15, 12), 15);
        assert_eq!(display_seconds(15, 10), 10);
        assert_eq!(display_seconds(60, 0), 0);
    }

    #[test]
    fn menu_rows_ask_the_session_manager_like_system_actions() {
        // systemActions.js: SuspendAsync(), RebootAsync(), ShutdownAsync()
        // and LogoutAsync(0) on org.gnome.SessionManager.
        assert_eq!(manager_call(Action::Suspend), ("Suspend", None));
        assert_eq!(manager_call(Action::Restart), ("Reboot", None));
        assert_eq!(manager_call(Action::PowerOff), ("Shutdown", None));
        assert_eq!(
            manager_call(Action::LogOut),
            ("Logout", Some((0u32,).to_variant()))
        );
    }

    #[test]
    fn other_sessions_are_filtered_and_titled_like_gnome_51() {
        // _loadSessions: user sessions only, none closing, not our own.
        assert!(lists_session("user", "active", "c2", "c1"));
        assert!(lists_session("user", "online", "c3", "c1"));
        assert!(!lists_session("greeter", "active", "c5", "c1"));
        assert!(!lists_session("user", "closing", "c6", "c1"));
        assert!(!lists_session("user", "active", "c1", "c1"));
        // Remote and console sessions carry the real name; a graphical
        // one shows the user name (GNOME 51's `userLabelText = userName`).
        assert_eq!(session_label("carol", None, true, "tty"), "carol (remote)");
        assert_eq!(
            session_label("bob", Some("Bob Builder"), false, "tty"),
            "Bob Builder (console)"
        );
        assert_eq!(
            session_label("alice", Some("Alice Liddell"), false, "wayland"),
            "alice"
        );
    }

    #[test]
    fn log_out_names_the_user_by_real_name_like_gnome_51() {
        let logout = Content {
            action: Action::LogOut,
            update_restart: false,
            updates: None,
        };
        assert_eq!(
            logout.text(false, Some("Proof Person"), 60),
            (
                "Log Out Proof Person".to_owned(),
                "Proof Person will be logged out automatically in 60 seconds".to_owned(),
                "Log Out"
            )
        );
        // Not loaded: the plain strings.
        assert_eq!(
            logout.text(false, None, 1),
            (
                "Log Out".to_owned(),
                "You will be logged out automatically in 1 second".to_owned(),
                "Log Out"
            )
        );
        // act_user_get_real_name: an empty RealName falls back to the user name.
        assert_eq!(real_name("", "proof"), "proof");
        assert_eq!(real_name("Proof Person", "proof"), "Proof Person");
        // Restart and Power Off never name the user.
        let restart = Content {
            action: Action::Restart,
            ..logout
        };
        assert_eq!(restart.text(false, Some("Proof Person"), 60).0, "Restart");
    }

    #[test]
    fn low_battery_warning_follows_gnome_51() {
        // _isBatteryLow: present, discharging, below 30 %.
        assert!(battery_low(true, 2, 29.9));
        assert!(!battery_low(true, 2, 30.0));
        assert!(!battery_low(true, 1, 10.0), "charging");
        assert!(!battery_low(true, 4, 10.0), "fully charged");
        assert!(!battery_low(false, 2, 10.0), "no battery");
        let scheduled = Content {
            action: Action::PowerOff,
            update_restart: false,
            updates: Some(UpdateState::Scheduled),
        };
        // Shown when updates are to install: checked or already scheduled.
        assert!(scheduled.battery_warning(true, false));
        assert!(!scheduled.battery_warning(false, true));
        let prepared = Content {
            updates: Some(UpdateState::Prepared),
            ..scheduled
        };
        assert!(prepared.battery_warning(true, true));
        assert!(!prepared.battery_warning(true, false));
        // No updates, no warning; Log Out never warns.
        assert!(!Content {
            updates: None,
            ..scheduled
        }
        .battery_warning(true, false));
        assert!(!Content {
            action: Action::LogOut,
            ..scheduled
        }
        .battery_warning(true, true));
    }

    #[test]
    fn offline_updates_shape_the_dialog_like_gnome_51() {
        assert_eq!(UpdateState::parse("prepared"), UpdateState::Prepared);
        assert_eq!(UpdateState::parse("scheduled"), UpdateState::Scheduled);
        assert_eq!(UpdateState::parse("none"), UpdateState::None);
        assert_eq!(open_type(3), Some((Action::Restart, true)));
        assert_eq!(open_type(2), Some((Action::Restart, false)));
        assert_eq!(open_type(4), None);
        let restart = Content {
            action: Action::Restart,
            update_restart: false,
            updates: Some(UpdateState::Prepared),
        };
        assert!(restart.checkbox_visible());
        assert!(!Content {
            updates: Some(UpdateState::None),
            ..restart
        }
        .checkbox_visible());
        assert!(!Content {
            updates: None,
            ..restart
        }
        .checkbox_visible());
        assert_eq!(restart.text(true, None, 60).0, "Install Updates & Restart");
        let poweroff = Content {
            action: Action::PowerOff,
            ..restart
        };
        assert_eq!(
            poweroff.text(true, None, 60).0,
            "Install Updates & Power Off"
        );
        let update_restart = Content {
            update_restart: true,
            updates: Some(UpdateState::Scheduled),
            ..restart
        };
        assert!(!update_restart.checkbox_visible());
        assert_eq!(
            update_restart.text(false, None, 60),
            (
                "Restart & Install Updates".to_owned(),
                "The system will automatically restart and install updates in 60 seconds"
                    .to_owned(),
                "Restart & Install"
            )
        );
        // _confirm.
        assert_eq!(
            post_update(restart, true, "ConfirmedReboot"),
            PostUpdate::SetAction("reboot")
        );
        assert_eq!(
            post_update(poweroff, true, "ConfirmedShutdown"),
            PostUpdate::ShutdownAfterInstall
        );
        assert_eq!(
            post_update(restart, false, "ConfirmedReboot"),
            PostUpdate::Cancel
        );
        assert_eq!(
            post_update(update_restart, false, "ConfirmedReboot"),
            PostUpdate::SetAction("reboot")
        );
        assert_eq!(
            post_update(
                Content {
                    updates: None,
                    ..restart
                },
                false,
                "ConfirmedReboot"
            ),
            PostUpdate::Nothing
        );
    }

    #[test]
    fn boot_options_need_logind_yes_or_challenge() {
        assert!(boot_loader_menu_available("yes"));
        assert!(boot_loader_menu_available("challenge"));
        assert!(!boot_loader_menu_available("no"));
        assert!(!boot_loader_menu_available("na"));
    }
}
