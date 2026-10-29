//! Administrative lockdown policy (`org.gnome.desktop.lockdown`, P-ST-05, #357).
//!
//! Pure policy engine: one snapshot of the eleven lockdown keys plus the
//! allow/deny decision for every desktop entry point each key constrains.
//! The compositor owns enforcement for session-scoped requests (lock,
//! external screenshots); the shell owns its menus and dialogs;
//! applications and system daemons own the rest. The shell reads all
//! eleven keys live through `gio` and enforces its surface immediately;
//! the compositor backstops the lock paths from its own live read of
//! `disable-lock-screen` (see [`read_lock_screen_key`]) and refuses the
//! portal's screenshots under `disable-save-to-disk` (see
//! [`read_save_to_disk_key`]), so no control-protocol change is needed.
//! Tests inject values through [`LockdownPolicy::from_reader`].
//!
//! Key inventory and owners, against the GNOME 51 sources
//! (`gsettings-desktop-schemas` 51.0 `org.gnome.desktop.lockdown`,
//! gnome-shell 51.0, gnome-session 51.0, gnome-settings-daemon 51.0):
//!
//! | Key | Owner | Entry points constrained (GNOME 51 consumer) |
//! |---|---|---|
//! | `disable-command-line` | shell | Run a Command dialog, Alt+F2 (`runDialog.js` `open()`) |
//! | `disable-log-out` | shell | power menu Log Out…, Restart… and Power Off… (`systemActions.js`; gnome-session `gsm-manager.c` `CanShutdown`/`CanReboot`/`Logout`); Suspend stays; nested sessions lose their only row and show no menu |
//! | `disable-user-switching` | shell | Switch User… row and the unlock dialog's other-user button (`systemActions.js`, `unlockDialog.js`); Tuna ships neither, so the restriction holds by absence and [`LockdownPolicy::user_switching_allowed`] gates them when they land |
//! | `disable-lock-screen` | compositor + shell | `Lock` control command, idle lock, shell lock button, screensaver/logind/sleep lock requests (`screenShield.js` `lock()`, `systemActions.js` `_updateLockScreen`); denied, never weakened (see below) |
//! | `disable-show-password` | shell | password peek icons: unlock prompt, polkit, network secrets (`st-password-entry.c`) |
//! | `disable-save-to-disk` | shell + compositor + apps | shell screenshots stay on the clipboard and screencasts refuse (`screenshot.js` `_storeScreenshot`, `screencastService.js`); the portal's `org.gnome.Shell.Screenshot` calls are refused (`_createScreenshot`); applications' Save As dialogs stay app-owned |
//! | `disable-application-handlers` | none | no GNOME 51 desktop consumer (gnome-shell, gnome-session and gnome-settings-daemon 51.0 never read it) |
//! | `disable-printing` | app-owned | applications' Print dialogs; the shell exposes no print path of its own |
//! | `disable-print-setup` | app-owned | applications' Print Setup dialogs |
//! | `user-administration-disabled` | daemon/app-owned | user account administration (Settings' Users panel, accountsservice/polkit); Tuna ships no Users panel |
//! | `mount-removable-storage-devices-as-read-only` | daemon-owned | removable mounts (udisks2); Tuna ships no mount agent |
//!
//! Secure-lock invariant: `disable-lock-screen` removes the lock entry
//! points; it never changes verification. Unlock still requires session
//! auth, and no policy state auto-unlocks: a session locked before the
//! policy arrived stays locked.
//!
//! Refresh rule (same as the shell snapshot): an unreadable key keeps
//! its current value, never resets. A first load with no bus reads
//! GNOME defaults (everything allowed), exactly like GNOME with an
//! unreachable dconf.

use std::time::Duration;

/// GSettings schema holding the lockdown keys.
pub const SCHEMA: &str = "org.gnome.desktop.lockdown";
/// Bound for one background `gsettings` read. The compositor tick never
/// waits on the settings backend (see
/// [`crate::control::ControlHub::refresh_lockdown`]); a read slower than
/// this yields `None` and the cached policy holds.
pub const LOCKDOWN_READ_TIMEOUT: Duration = Duration::from_millis(500);
/// Run-command dialog gate (Alt+F2).
pub const KEY_DISABLE_COMMAND_LINE: &str = "disable-command-line";
/// Shell screenshot/screencast files, portal screenshots, apps' Save As.
pub const KEY_DISABLE_SAVE_TO_DISK: &str = "disable-save-to-disk";
/// Applications' Print dialogs (app-owned).
pub const KEY_DISABLE_PRINTING: &str = "disable-printing";
/// Applications' Print Setup dialogs (app-owned).
pub const KEY_DISABLE_PRINT_SETUP: &str = "disable-print-setup";
/// User-switch entry points.
pub const KEY_DISABLE_USER_SWITCHING: &str = "disable-user-switching";
/// Lock entry points (button, `Lock` command, idle lock).
pub const KEY_DISABLE_LOCK_SCREEN: &str = "disable-lock-screen";
/// URL/MIME handlers (no GNOME 51 desktop consumer).
pub const KEY_DISABLE_APPLICATION_HANDLERS: &str = "disable-application-handlers";
/// Log Out, Restart and Power Off entry points.
pub const KEY_DISABLE_LOG_OUT: &str = "disable-log-out";
/// User account administration (daemon-owned).
pub const KEY_USER_ADMINISTRATION_DISABLED: &str = "user-administration-disabled";
/// Removable mounts writable (daemon-owned; `true` mounts read-only).
pub const KEY_MOUNT_REMOVABLE_READ_ONLY: &str = "mount-removable-storage-devices-as-read-only";
/// Password peek icons.
pub const KEY_DISABLE_SHOW_PASSWORD: &str = "disable-show-password";

/// Every lockdown key, in schema order.
pub const ALL_KEYS: [&str; 11] = [
    KEY_DISABLE_COMMAND_LINE,
    KEY_DISABLE_SAVE_TO_DISK,
    KEY_DISABLE_PRINTING,
    KEY_DISABLE_PRINT_SETUP,
    KEY_DISABLE_USER_SWITCHING,
    KEY_DISABLE_LOCK_SCREEN,
    KEY_DISABLE_APPLICATION_HANDLERS,
    KEY_DISABLE_LOG_OUT,
    KEY_USER_ADMINISTRATION_DISABLED,
    KEY_MOUNT_REMOVABLE_READ_ONLY,
    KEY_DISABLE_SHOW_PASSWORD,
];

/// One user-visible action a lockdown key can restrict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RestrictedAction {
    /// Open the run-command dialog.
    RunCommand,
    /// Offer Log Out.
    LogOut,
    /// Offer user switching.
    SwitchUser,
    /// Lock the session (button, command, idle).
    LockScreen,
    /// Reveal a typed password.
    ShowPassword,
    /// Open a URL or MIME handler (no GNOME 51 desktop consumer).
    OpenHandler,
    /// Print (app-owned).
    Print,
    /// Change print setup (app-owned).
    PrintSetup,
    /// Save to disk (shell screenshots/screencasts, portal screenshots, apps).
    SaveToDisk,
    /// Administer user accounts (daemon-owned).
    UserAdmin,
    /// Write to removable storage (daemon-owned).
    WriteRemovable,
}

impl RestrictedAction {
    /// The lockdown key governing this action.
    pub fn key(self) -> &'static str {
        match self {
            RestrictedAction::RunCommand => KEY_DISABLE_COMMAND_LINE,
            RestrictedAction::LogOut => KEY_DISABLE_LOG_OUT,
            RestrictedAction::SwitchUser => KEY_DISABLE_USER_SWITCHING,
            RestrictedAction::LockScreen => KEY_DISABLE_LOCK_SCREEN,
            RestrictedAction::ShowPassword => KEY_DISABLE_SHOW_PASSWORD,
            RestrictedAction::OpenHandler => KEY_DISABLE_APPLICATION_HANDLERS,
            RestrictedAction::Print => KEY_DISABLE_PRINTING,
            RestrictedAction::PrintSetup => KEY_DISABLE_PRINT_SETUP,
            RestrictedAction::SaveToDisk => KEY_DISABLE_SAVE_TO_DISK,
            RestrictedAction::UserAdmin => KEY_USER_ADMINISTRATION_DISABLED,
            RestrictedAction::WriteRemovable => KEY_MOUNT_REMOVABLE_READ_ONLY,
        }
    }

    /// Short denial message for shell UX ("restricted by your administrator").
    pub fn denial(self) -> &'static str {
        match self {
            RestrictedAction::RunCommand => "Running commands is disabled by your administrator.",
            RestrictedAction::LogOut => "Logging out is disabled by your administrator.",
            RestrictedAction::SwitchUser => "Switching users is disabled by your administrator.",
            RestrictedAction::LockScreen => "Locking the screen is disabled by your administrator.",
            RestrictedAction::ShowPassword => {
                "Showing passwords is disabled by your administrator."
            }
            RestrictedAction::OpenHandler => "Opening links is disabled by your administrator.",
            RestrictedAction::Print => "Printing is disabled by your administrator.",
            RestrictedAction::PrintSetup => {
                "Changing print settings is disabled by your administrator."
            }
            RestrictedAction::SaveToDisk => "Saving files is disabled by your administrator.",
            RestrictedAction::UserAdmin => "Managing users is disabled by your administrator.",
            RestrictedAction::WriteRemovable => {
                "Removable storage is read-only by administrator policy."
            }
        }
    }
}

/// Snapshot of the eleven lockdown keys. GNOME defaults are all `false`
/// (nothing restricted); a `true` bit restricts its entry points.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LockdownPolicy {
    /// `disable-command-line`.
    pub disable_command_line: bool,
    /// `disable-save-to-disk`.
    pub disable_save_to_disk: bool,
    /// `disable-printing` (app-owned).
    pub disable_printing: bool,
    /// `disable-print-setup` (app-owned).
    pub disable_print_setup: bool,
    /// `disable-user-switching`.
    pub disable_user_switching: bool,
    /// `disable-lock-screen`.
    pub disable_lock_screen: bool,
    /// `disable-application-handlers` (no desktop consumer).
    pub disable_application_handlers: bool,
    /// `disable-log-out`.
    pub disable_log_out: bool,
    /// `user-administration-disabled` (daemon-owned).
    pub user_administration_disabled: bool,
    /// `mount-removable-storage-devices-as-read-only` (daemon-owned).
    pub mount_removable_read_only: bool,
    /// `disable-show-password`.
    pub disable_show_password: bool,
}

impl LockdownPolicy {
    /// Read one key: `None` (absent/unreadable) keeps the current value.
    fn refresh_key(current: &mut bool, read: &dyn Fn(&str) -> Option<bool>, key: &str) {
        if let Some(value) = read(key) {
            *current = value;
        }
    }

    /// Snapshot from a key reader (GSettings today, control channel at
    /// union). Starts at GNOME defaults, so unreadable keys read allowed.
    pub fn from_reader(read: &dyn Fn(&str) -> Option<bool>) -> Self {
        let mut policy = Self::default();
        policy.refresh(read);
        policy
    }

    /// Re-read every key. Absent or unreadable keys keep their current
    /// value, never reset; a returned `false` re-allows its action.
    pub fn refresh(&mut self, read: &dyn Fn(&str) -> Option<bool>) {
        Self::refresh_key(
            &mut self.disable_command_line,
            read,
            KEY_DISABLE_COMMAND_LINE,
        );
        Self::refresh_key(
            &mut self.disable_save_to_disk,
            read,
            KEY_DISABLE_SAVE_TO_DISK,
        );
        Self::refresh_key(&mut self.disable_printing, read, KEY_DISABLE_PRINTING);
        Self::refresh_key(&mut self.disable_print_setup, read, KEY_DISABLE_PRINT_SETUP);
        Self::refresh_key(
            &mut self.disable_user_switching,
            read,
            KEY_DISABLE_USER_SWITCHING,
        );
        Self::refresh_key(&mut self.disable_lock_screen, read, KEY_DISABLE_LOCK_SCREEN);
        Self::refresh_key(
            &mut self.disable_application_handlers,
            read,
            KEY_DISABLE_APPLICATION_HANDLERS,
        );
        Self::refresh_key(&mut self.disable_log_out, read, KEY_DISABLE_LOG_OUT);
        Self::refresh_key(
            &mut self.user_administration_disabled,
            read,
            KEY_USER_ADMINISTRATION_DISABLED,
        );
        Self::refresh_key(
            &mut self.mount_removable_read_only,
            read,
            KEY_MOUNT_REMOVABLE_READ_ONLY,
        );
        Self::refresh_key(
            &mut self.disable_show_password,
            read,
            KEY_DISABLE_SHOW_PASSWORD,
        );
    }

    /// Nothing restricted.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Whether `action` is currently allowed.
    pub fn allows(&self, action: RestrictedAction) -> bool {
        match action {
            RestrictedAction::RunCommand => self.command_line_allowed(),
            RestrictedAction::LogOut => self.logout_allowed(),
            RestrictedAction::SwitchUser => self.user_switching_allowed(),
            RestrictedAction::LockScreen => self.lock_screen_allowed(),
            RestrictedAction::ShowPassword => self.password_reveal_allowed(),
            RestrictedAction::OpenHandler => self.application_handlers_allowed(),
            RestrictedAction::Print => self.printing_allowed(),
            RestrictedAction::PrintSetup => self.print_setup_allowed(),
            RestrictedAction::SaveToDisk => self.save_to_disk_allowed(),
            RestrictedAction::UserAdmin => self.user_administration_allowed(),
            RestrictedAction::WriteRemovable => self.removable_storage_writable(),
        }
    }

    /// Denial message when `action` is restricted, `None` when allowed.
    pub fn denial_reason(&self, action: RestrictedAction) -> Option<&'static str> {
        (!self.allows(action)).then(|| action.denial())
    }

    /// Every currently restricted action, in [`ALL_KEYS`] order.
    pub fn restricted_actions(&self) -> Vec<RestrictedAction> {
        ALL_KEYS
            .iter()
            .filter_map(|key| {
                let action = match *key {
                    KEY_DISABLE_COMMAND_LINE => RestrictedAction::RunCommand,
                    KEY_DISABLE_SAVE_TO_DISK => RestrictedAction::SaveToDisk,
                    KEY_DISABLE_PRINTING => RestrictedAction::Print,
                    KEY_DISABLE_PRINT_SETUP => RestrictedAction::PrintSetup,
                    KEY_DISABLE_USER_SWITCHING => RestrictedAction::SwitchUser,
                    KEY_DISABLE_LOCK_SCREEN => RestrictedAction::LockScreen,
                    KEY_DISABLE_APPLICATION_HANDLERS => RestrictedAction::OpenHandler,
                    KEY_DISABLE_LOG_OUT => RestrictedAction::LogOut,
                    KEY_USER_ADMINISTRATION_DISABLED => RestrictedAction::UserAdmin,
                    KEY_MOUNT_REMOVABLE_READ_ONLY => RestrictedAction::WriteRemovable,
                    KEY_DISABLE_SHOW_PASSWORD => RestrictedAction::ShowPassword,
                    _ => return None,
                };
                (!self.allows(action)).then_some(action)
            })
            .collect()
    }

    /// Run-command dialog (Alt+F2) allowed.
    pub fn command_line_allowed(&self) -> bool {
        !self.disable_command_line
    }
    /// Log Out, Restart and Power Off offered.
    pub fn logout_allowed(&self) -> bool {
        !self.disable_log_out
    }
    /// User switching offered.
    pub fn user_switching_allowed(&self) -> bool {
        !self.disable_user_switching
    }
    /// Lock entry points live. Denying lock never weakens verification
    /// and never unlocks; it only removes the button, the `Lock`
    /// command, and idle lock.
    pub fn lock_screen_allowed(&self) -> bool {
        !self.disable_lock_screen
    }
    /// Password reveal toggles shown.
    pub fn password_reveal_allowed(&self) -> bool {
        !self.disable_show_password
    }
    /// URL/MIME handlers run (no GNOME 51 desktop consumer).
    pub fn application_handlers_allowed(&self) -> bool {
        !self.disable_application_handlers
    }
    /// Printing allowed (app-owned; exposed for admin UX text).
    pub fn printing_allowed(&self) -> bool {
        !self.disable_printing
    }
    /// Print setup allowed (app-owned).
    pub fn print_setup_allowed(&self) -> bool {
        !self.disable_print_setup
    }
    /// Saving to disk allowed.
    pub fn save_to_disk_allowed(&self) -> bool {
        !self.disable_save_to_disk
    }
    /// User administration allowed (daemon-owned).
    pub fn user_administration_allowed(&self) -> bool {
        !self.user_administration_disabled
    }
    /// Removable storage writable (daemon-owned).
    pub fn removable_storage_writable(&self) -> bool {
        !self.mount_removable_read_only
    }
}

/// Live value of `disable-lock-screen`, if GSettings answers within
/// [`LOCKDOWN_READ_TIMEOUT`]. `None` keeps the caller's cached value
/// under the refresh rule: a missing binary, schema, key, or session
/// bus, a slow backend, or garbage output never restricts by accident
/// (same fail-safe as the shell's missing-schema path).
///
/// The compositor owns no `gio` dependency, so this shells out to the
/// `gsettings` CLI. The slow-tick refresh runs this on a background
/// thread (never the frame path); hub startup calls it directly,
/// bounded by the same timeout.
pub fn read_lock_screen_key() -> Option<bool> {
    read_key(KEY_DISABLE_LOCK_SCREEN)
}

/// Live value of `disable-save-to-disk` under the same bound and
/// fail-safe as [`read_lock_screen_key`]; the screenshot service reads
/// it per external request, off the frame path.
pub fn read_save_to_disk_key() -> Option<bool> {
    read_key(KEY_DISABLE_SAVE_TO_DISK)
}

fn read_key(key: &str) -> Option<bool> {
    if !session_bus_present() {
        return None;
    }
    read_bool_via_command("gsettings", &["get", SCHEMA, key], LOCKDOWN_READ_TIMEOUT)
}

/// Run `program args` with a timeout, parsing stdout as a `gsettings`
/// boolean (`true`/`false`). A hung backend yields `None` after
/// `timeout` (the child is killed); a bad exit, missing binary, or
/// unparseable output yields `None` too. Never blocks past the bound
/// plus one poll slice, and caps stdout so a rogue backend cannot grow
/// the buffer. The post-exit drain shares the same absolute deadline:
/// the child is gone but an inherited writer (a forked grandchild
/// holding the pipe) can keep EOF away forever, so the drain reads
/// nonblocking in poll slices and gives up at the deadline — no helper
/// thread.
pub fn read_bool_via_command(program: &str, args: &[&str], timeout: Duration) -> Option<bool> {
    let mut child = std::process::Command::new(program)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let start = std::time::Instant::now();
    let deadline = start + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                let mut output = String::new();
                if let Some(stdout) = child.stdout.take() {
                    use std::io::Read;
                    let mut stdout = stdout;
                    // `ChildStdout` has no `set_nonblocking`; flag the fd
                    // directly (same rustix pair as the IME address probe).
                    let flags = rustix::fs::fcntl_getfl(&stdout).ok()?;
                    rustix::fs::fcntl_setfl(&stdout, flags | rustix::fs::OFlags::NONBLOCK).ok()?;
                    let mut bytes = Vec::new();
                    let mut chunk = [0u8; 256];
                    loop {
                        if bytes.len() >= 1024 {
                            break;
                        }
                        match stdout.read(&mut chunk) {
                            Ok(0) => break,
                            Ok(n) => {
                                let room = 1024 - bytes.len();
                                bytes.extend_from_slice(&chunk[..n.min(room)]);
                            }
                            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                if std::time::Instant::now() >= deadline {
                                    return None;
                                }
                                std::thread::sleep(Duration::from_millis(5));
                            }
                            Err(_) => return None,
                        }
                    }
                    output = String::from_utf8_lossy(&bytes).into_owned();
                }
                return parse_gsettings_bool(&output);
            }
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(_) => return None,
        }
    }
}

/// Whether a session bus is plausibly reachable for a `gsettings` read.
pub(crate) fn session_bus_present() -> bool {
    if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some() {
        return true;
    }
    std::env::var_os("XDG_RUNTIME_DIR")
        .is_some_and(|dir| std::path::Path::new(&dir).join("bus").exists())
}

/// Parse `gsettings get` boolean output (`true`/`false` plus newline).
fn parse_gsettings_bool(text: &str) -> Option<bool> {
    match text.trim() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Reader over an explicit key set; missing keys read absent.
    fn reader(values: &HashMap<String, bool>) -> impl Fn(&str) -> Option<bool> + '_ {
        move |key: &str| values.get(key).copied()
    }

    #[test]
    fn defaults_allow_everything() {
        let policy = LockdownPolicy::default();
        assert!(policy.is_default());
        assert!(policy.restricted_actions().is_empty());
        for key in ALL_KEYS {
            let action = match key {
                KEY_DISABLE_COMMAND_LINE => RestrictedAction::RunCommand,
                KEY_DISABLE_SAVE_TO_DISK => RestrictedAction::SaveToDisk,
                KEY_DISABLE_PRINTING => RestrictedAction::Print,
                KEY_DISABLE_PRINT_SETUP => RestrictedAction::PrintSetup,
                KEY_DISABLE_USER_SWITCHING => RestrictedAction::SwitchUser,
                KEY_DISABLE_LOCK_SCREEN => RestrictedAction::LockScreen,
                KEY_DISABLE_APPLICATION_HANDLERS => RestrictedAction::OpenHandler,
                KEY_DISABLE_LOG_OUT => RestrictedAction::LogOut,
                KEY_USER_ADMINISTRATION_DISABLED => RestrictedAction::UserAdmin,
                KEY_MOUNT_REMOVABLE_READ_ONLY => RestrictedAction::WriteRemovable,
                KEY_DISABLE_SHOW_PASSWORD => RestrictedAction::ShowPassword,
                _ => unreachable!("ALL_KEYS covers every action"),
            };
            assert!(policy.allows(action), "{key} must default to allowed");
            assert_eq!(policy.denial_reason(action), None);
        }
    }

    #[test]
    fn each_key_denies_exactly_its_action() {
        let actions = [
            (KEY_DISABLE_COMMAND_LINE, RestrictedAction::RunCommand),
            (KEY_DISABLE_LOG_OUT, RestrictedAction::LogOut),
            (KEY_DISABLE_USER_SWITCHING, RestrictedAction::SwitchUser),
            (KEY_DISABLE_LOCK_SCREEN, RestrictedAction::LockScreen),
            (KEY_DISABLE_SHOW_PASSWORD, RestrictedAction::ShowPassword),
            (
                KEY_DISABLE_APPLICATION_HANDLERS,
                RestrictedAction::OpenHandler,
            ),
            (KEY_DISABLE_PRINTING, RestrictedAction::Print),
            (KEY_DISABLE_PRINT_SETUP, RestrictedAction::PrintSetup),
            (KEY_DISABLE_SAVE_TO_DISK, RestrictedAction::SaveToDisk),
            (
                KEY_USER_ADMINISTRATION_DISABLED,
                RestrictedAction::UserAdmin,
            ),
            (
                KEY_MOUNT_REMOVABLE_READ_ONLY,
                RestrictedAction::WriteRemovable,
            ),
        ];
        assert_eq!(actions.len(), ALL_KEYS.len());
        for (key, action) in actions {
            let values = HashMap::from([(key.to_owned(), true)]);
            let policy = LockdownPolicy::from_reader(&reader(&values));
            assert!(!policy.is_default());
            assert_eq!(policy.restricted_actions(), vec![action], "{key}");
            assert_eq!(policy.denial_reason(action), Some(action.denial()));
            assert_eq!(action.key(), key);
        }
    }

    #[test]
    fn absent_keys_keep_current_values_never_reset() {
        let values = HashMap::from([
            (KEY_DISABLE_LOG_OUT.to_owned(), true),
            (KEY_DISABLE_LOCK_SCREEN.to_owned(), true),
        ]);
        let mut policy = LockdownPolicy::from_reader(&reader(&values));
        assert!(!policy.logout_allowed());
        assert!(!policy.lock_screen_allowed());
        // A later unreadable bus changes nothing.
        policy.refresh(&|_| None);
        assert!(!policy.logout_allowed());
        assert!(!policy.lock_screen_allowed());
        // Unknown keys are ignored.
        policy.refresh(&|key| (key == "bogus").then_some(true));
        assert!(!policy.logout_allowed());
    }

    #[test]
    fn policy_unlock_restores_allowed_actions() {
        let locked = HashMap::from([
            (KEY_DISABLE_LOG_OUT.to_owned(), true),
            (KEY_DISABLE_LOCK_SCREEN.to_owned(), true),
            (KEY_DISABLE_COMMAND_LINE.to_owned(), true),
        ]);
        let mut policy = LockdownPolicy::from_reader(&reader(&locked));
        assert_eq!(policy.restricted_actions().len(), 3);
        let unlocked = HashMap::from([
            (KEY_DISABLE_LOG_OUT.to_owned(), false),
            (KEY_DISABLE_LOCK_SCREEN.to_owned(), false),
            (KEY_DISABLE_COMMAND_LINE.to_owned(), false),
        ]);
        policy.refresh(&reader(&unlocked));
        assert!(policy.is_default());
        assert!(policy.logout_allowed());
        assert!(policy.lock_screen_allowed());
        assert!(policy.command_line_allowed());
    }

    #[test]
    fn gsettings_booleans_parse_and_garbage_yields_none() {
        assert_eq!(parse_gsettings_bool("true\n"), Some(true));
        assert_eq!(parse_gsettings_bool("false\n"), Some(false));
        assert_eq!(parse_gsettings_bool("true"), Some(true));
        assert_eq!(parse_gsettings_bool(""), None);
        assert_eq!(parse_gsettings_bool("no such key\n"), None);
        assert_eq!(parse_gsettings_bool("1"), None);
    }

    #[test]
    fn command_booleans_parse_and_hung_backend_times_out() {
        let bound = Duration::from_secs(2);
        // Fast backends parse.
        assert_eq!(read_bool_via_command("echo", &["true"], bound), Some(true));
        assert_eq!(
            read_bool_via_command("echo", &["false"], bound),
            Some(false)
        );
        // Bad exit, garbage, and a missing binary yield None (fail-safe).
        assert_eq!(read_bool_via_command("false", &[], bound), None);
        assert_eq!(read_bool_via_command("echo", &["bogus"], bound), None);
        assert_eq!(
            read_bool_via_command("tuna-no-such-backend", &[], bound),
            None
        );
        // Hung backend: bounded, never the full hang.
        let start = std::time::Instant::now();
        assert_eq!(
            read_bool_via_command("sleep", &["10"], Duration::from_millis(100)),
            None
        );
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_secs(2),
            "hung backend must time out, took {elapsed:?}"
        );
    }

    #[test]
    fn inherited_writer_cannot_hold_the_post_exit_read_past_the_deadline() {
        // The child exits 0 at once while a backgrounded grandchild
        // inherits the pipe and never writes: the post-exit drain must
        // give up at the remaining absolute deadline, not at EOF.
        let timeout = Duration::from_millis(200);
        let start = std::time::Instant::now();
        let result = read_bool_via_command("sh", &["-c", "sleep 5 & exec true"], timeout);
        let elapsed = start.elapsed();
        assert_eq!(result, None);
        assert!(
            elapsed < Duration::from_secs(2),
            "inherited writer must not hold the read past the deadline, took {elapsed:?}"
        );
    }

    #[test]
    fn live_lock_screen_read_is_deterministic_and_never_panics() {
        // Passes with or without a session bus or schema: two reads
        // agree, and an unreadable key reads absent (refresh rule).
        assert_eq!(read_lock_screen_key(), read_lock_screen_key());
    }

    #[test]
    fn denial_messages_name_the_restriction() {
        assert!(RestrictedAction::LogOut.denial().contains("Logging out"));
        assert!(RestrictedAction::LockScreen.denial().contains("Locking"));
        assert!(RestrictedAction::WriteRemovable
            .denial()
            .contains("read-only"));
        for key in ALL_KEYS {
            let policy = LockdownPolicy::from_reader(&|k| (k == key).then_some(true));
            assert_eq!(policy.restricted_actions().len(), 1, "{key}");
        }
    }
}
