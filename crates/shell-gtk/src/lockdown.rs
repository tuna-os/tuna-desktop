//! Administrative lockdown admin UX (`org.gnome.desktop.lockdown`, P-ST-05, #357).
//!
//! Shell side of the lockdown policy: read the eleven keys through
//! `gio::Settings` (quietly degrading to GNOME defaults when the schema,
//! a key, or the bus is missing), resolve which shell entry points stay
//! visible, and apply that to the widgets. The decision engine — key
//! inventory, owners, denial copy — lives in the compositor's
//! `lockdown` module; this file mirrors only the key names needed to
//! read GSettings directly, and stays free of `crate::` uses so the
//! integration test can include it by path (binary modules are not
//! importable from integration targets).
//!
//! Wiring (live; entry points verified at `a445af2e`, wired on w2-357):
//!
//! | Entry point | Call site | Application |
//! |---|---|---|
//! | Lock button | `main.rs` `qs_round("system-lock-screen-symbolic", "Lock")` | hidden when denied, plus a live guard on every `control.lock()` path (button, sleep, screensaver/D-Bus, keybinding) |
//! | Log Out row | `main.rs` power menu `power::Action::LogOut` item | hidden when denied; nested sessions with no rows show no menu |
//! | User switch row | none yet | `visible.user_switch` gates the row when it lands |
//! | Run-command dialog | none yet | `visible.run_command` gates the dialog when it lands |
//! | Password reveal | `network_agent.rs` secret-entry reveal toggle | [`password_reveal_icon`] omits the toggle when denied |
//! | Settings buttons | `main.rs`/`wifi.rs`/`accessibility.rs` `gnome-control-center` spawns | [`open_settings_panel`]/[`handlers_allowed`] block the spawn when denied; the top Settings button hides too |
//!
//! App-owned (`disable-printing`, `disable-print-setup`,
//! `disable-save-to-disk`) and daemon-owned
//! (`user-administration-disabled`,
//! `mount-removable-storage-devices-as-read-only`) keys are read into
//! the snapshot for admin UX text but constrain no shell widget: the
//! compositor module documents those owners.

use gio::prelude::SettingsExt;
use gtk4 as gtk;
use gtk4::prelude::*;

/// GSettings schema holding the lockdown keys.
pub const SCHEMA: &str = "org.gnome.desktop.lockdown";
/// Run-command dialog gate (Alt+F2).
pub const KEY_DISABLE_COMMAND_LINE: &str = "disable-command-line";
/// Applications' Save As dialogs (app-owned).
pub const KEY_DISABLE_SAVE_TO_DISK: &str = "disable-save-to-disk";
/// Applications' Print dialogs (app-owned).
pub const KEY_DISABLE_PRINTING: &str = "disable-printing";
/// Applications' Print Setup dialogs (app-owned).
pub const KEY_DISABLE_PRINT_SETUP: &str = "disable-print-setup";
/// User-switch entry points.
pub const KEY_DISABLE_USER_SWITCHING: &str = "disable-user-switching";
/// Lock entry points (button, `Lock` command, idle lock).
pub const KEY_DISABLE_LOCK_SCREEN: &str = "disable-lock-screen";
/// Shell-opened URL/MIME handlers.
pub const KEY_DISABLE_APPLICATION_HANDLERS: &str = "disable-application-handlers";
/// Log Out entry points.
pub const KEY_DISABLE_LOG_OUT: &str = "disable-log-out";
/// User account administration (daemon-owned).
pub const KEY_USER_ADMINISTRATION_DISABLED: &str = "user-administration-disabled";
/// Removable mounts writable (daemon-owned; `true` mounts read-only).
pub const KEY_MOUNT_REMOVABLE_READ_ONLY: &str = "mount-removable-storage-devices-as-read-only";
/// Password reveal toggles.
pub const KEY_DISABLE_SHOW_PASSWORD: &str = "disable-show-password";

/// Snapshot of the eleven lockdown keys. GNOME defaults are all `false`
/// (nothing restricted); a `true` bit restricts its entry points.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LockdownPolicy {
    /// `disable-command-line`.
    pub disable_command_line: bool,
    /// `disable-save-to-disk` (app-owned).
    pub disable_save_to_disk: bool,
    /// `disable-printing` (app-owned).
    pub disable_printing: bool,
    /// `disable-print-setup` (app-owned).
    pub disable_print_setup: bool,
    /// `disable-user-switching`.
    pub disable_user_switching: bool,
    /// `disable-lock-screen`.
    pub disable_lock_screen: bool,
    /// `disable-application-handlers`.
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

/// Which shell entry points stay visible under a policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryVisibility {
    /// Lock button and click path live.
    pub lock_screen: bool,
    /// Log Out row shown.
    pub log_out: bool,
    /// User-switch row shown (gates the row when one exists).
    pub user_switch: bool,
    /// Run-command dialog allowed (gates the dialog when one exists).
    pub run_command: bool,
    /// Password reveal toggles shown.
    pub password_reveal: bool,
    /// Shell-opened handlers (settings buttons) run.
    pub url_handlers: bool,
}

/// Shell-constrained action denied by policy, for UX copy.
///
/// Test-only until a shell surface renders denials inline (today's
/// guards hide widgets or log to stderr): the unit suite pins the
/// copy, and run-command/user-switch rows will construct their
/// variants when they land.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellAction {
    /// Open the run-command dialog.
    RunCommand,
    /// Offer Log Out.
    LogOut,
    /// Offer user switching.
    SwitchUser,
    /// Lock the session.
    LockScreen,
    /// Reveal a typed password.
    ShowPassword,
    /// Open a URL or MIME handler from the shell.
    OpenHandler,
}

#[cfg(test)]
impl ShellAction {
    /// Short denial message ("restricted by your administrator").
    pub fn denial(self) -> &'static str {
        match self {
            ShellAction::RunCommand => "Running commands is disabled by your administrator.",
            ShellAction::LogOut => "Logging out is disabled by your administrator.",
            ShellAction::SwitchUser => "Switching users is disabled by your administrator.",
            ShellAction::LockScreen => "Locking the screen is disabled by your administrator.",
            ShellAction::ShowPassword => "Showing passwords is disabled by your administrator.",
            ShellAction::OpenHandler => "Opening links is disabled by your administrator.",
        }
    }
}

impl LockdownPolicy {
    /// Resolve which shell entry points stay visible.
    pub fn entry_visibility(&self) -> EntryVisibility {
        EntryVisibility {
            lock_screen: !self.disable_lock_screen,
            log_out: !self.disable_log_out,
            user_switch: !self.disable_user_switching,
            run_command: !self.disable_command_line,
            password_reveal: !self.disable_show_password,
            url_handlers: !self.disable_application_handlers,
        }
    }

    /// Nothing restricted (test-only: asserted by the suites below).
    #[cfg(test)]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// GSettings for the lockdown schema when installed; `None` keeps the
/// shell running without it (same guard as the shell's `settings()`).
fn settings() -> Option<gio::Settings> {
    let source = gio::SettingsSchemaSource::default()?;
    source.lookup(SCHEMA, true)?;
    Some(gio::Settings::new(SCHEMA))
}

/// Read one boolean key; missing keys read `false` (GNOME default), so
/// a schema older than the key list can never restrict by accident.
fn read_key(settings: &gio::Settings, key: &str) -> bool {
    settings
        .settings_schema()
        .is_some_and(|schema| schema.has_key(key))
        && settings.boolean(key)
}

/// Snapshot from an open settings object (memory-backend fixtures in
/// tests, the live schema in the shell).
pub fn read_from(settings: &gio::Settings) -> LockdownPolicy {
    LockdownPolicy {
        disable_command_line: read_key(settings, KEY_DISABLE_COMMAND_LINE),
        disable_save_to_disk: read_key(settings, KEY_DISABLE_SAVE_TO_DISK),
        disable_printing: read_key(settings, KEY_DISABLE_PRINTING),
        disable_print_setup: read_key(settings, KEY_DISABLE_PRINT_SETUP),
        disable_user_switching: read_key(settings, KEY_DISABLE_USER_SWITCHING),
        disable_lock_screen: read_key(settings, KEY_DISABLE_LOCK_SCREEN),
        disable_application_handlers: read_key(settings, KEY_DISABLE_APPLICATION_HANDLERS),
        disable_log_out: read_key(settings, KEY_DISABLE_LOG_OUT),
        user_administration_disabled: read_key(settings, KEY_USER_ADMINISTRATION_DISABLED),
        mount_removable_read_only: read_key(settings, KEY_MOUNT_REMOVABLE_READ_ONLY),
        disable_show_password: read_key(settings, KEY_DISABLE_SHOW_PASSWORD),
    }
}

/// Live snapshot. Never panics: a missing schema, key, or bus reads
/// GNOME defaults (everything allowed).
pub fn read() -> LockdownPolicy {
    settings().map(|s| read_from(&s)).unwrap_or_default()
}

/// Whether shell-opened URL/MIME handlers may run (live read of
/// `disable-application-handlers`). Guards every Settings launch and
/// the enterprise Wi-Fi setup path, which all re-read per activation.
pub fn handlers_allowed() -> bool {
    read().entry_visibility().url_handlers
}

/// Open a Settings panel unless policy blocks handlers. Returns
/// whether it launched (`false` covers both denial and spawn
/// failure; only denial is logged here).
pub fn open_settings_panel(panel: Option<&str>) -> bool {
    let policy = read();
    let launched = spawn_settings_for_policy(panel, &policy, &|panel| {
        let mut command = std::process::Command::new("gnome-control-center");
        if let Some(panel) = panel {
            command.arg(panel);
        }
        command.spawn().is_ok()
    });
    if !launched && !policy.entry_visibility().url_handlers {
        eprintln!("tuna-shell-gtk: opening settings is disabled by your administrator.");
    }
    launched
}

/// Testable core of [`open_settings_panel`]: the spawn runs only when
/// the policy allows handlers. Callers pass the live [`read`] policy;
/// tests inject restrictive policies with a recording spawn.
pub fn spawn_settings_for_policy(
    panel: Option<&str>,
    policy: &LockdownPolicy,
    spawn: &dyn Fn(Option<&str>) -> bool,
) -> bool {
    if !policy.entry_visibility().url_handlers {
        return false;
    }
    spawn(panel)
}

/// Reveal-toggle icon for a secret entry, if the field is a password
/// field and the policy shows passwords. A denied policy (or a
/// non-password field) yields no icon, so no toggle path exists.
pub fn password_reveal_icon(
    password_field: bool,
    visibility: &EntryVisibility,
) -> Option<&'static str> {
    (password_field && visibility.password_reveal).then_some("view-reveal-symbolic")
}

/// Apply handler visibility to the top Settings button: hidden while
/// `disable-application-handlers` denies, restored when it lifts.
pub fn apply_settings_visibility(visibility: &EntryVisibility, settings_button: &gtk::Button) {
    settings_button.set_visible(visibility.url_handlers);
}

/// Apply visibility to the lock, log-out, and user-switch widgets.
/// Callers pass the already-built rows; when `disable-log-out` hides
/// the only nested row, the caller hides the menu itself.
///
/// Exercised by the integration target (the binary wires its own
/// watcher around per-widget visibility); the allow keeps the
/// binary's build quiet until the user-switch row lands.
#[allow(dead_code)]
pub fn apply_entry_visibility(
    visibility: &EntryVisibility,
    lock: &gtk::Widget,
    log_out: &gtk::Widget,
    user_switch: &gtk::Widget,
) {
    lock.set_visible(visibility.lock_screen);
    log_out.set_visible(visibility.log_out);
    user_switch.set_visible(visibility.user_switch);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_show_every_entry_point() {
        let visibility = LockdownPolicy::default().entry_visibility();
        assert_eq!(
            visibility,
            EntryVisibility {
                lock_screen: true,
                log_out: true,
                user_switch: true,
                run_command: true,
                password_reveal: true,
                url_handlers: true,
            }
        );
    }

    #[test]
    fn each_shell_key_hides_exactly_its_entry_point() {
        let cases = [
            (
                LockdownPolicy {
                    disable_lock_screen: true,
                    ..LockdownPolicy::default()
                },
                EntryVisibility {
                    lock_screen: false,
                    log_out: true,
                    user_switch: true,
                    run_command: true,
                    password_reveal: true,
                    url_handlers: true,
                },
            ),
            (
                LockdownPolicy {
                    disable_log_out: true,
                    ..LockdownPolicy::default()
                },
                EntryVisibility {
                    lock_screen: true,
                    log_out: false,
                    user_switch: true,
                    run_command: true,
                    password_reveal: true,
                    url_handlers: true,
                },
            ),
            (
                LockdownPolicy {
                    disable_user_switching: true,
                    ..LockdownPolicy::default()
                },
                EntryVisibility {
                    lock_screen: true,
                    log_out: true,
                    user_switch: false,
                    run_command: true,
                    password_reveal: true,
                    url_handlers: true,
                },
            ),
            (
                LockdownPolicy {
                    disable_command_line: true,
                    ..LockdownPolicy::default()
                },
                EntryVisibility {
                    lock_screen: true,
                    log_out: true,
                    user_switch: true,
                    run_command: false,
                    password_reveal: true,
                    url_handlers: true,
                },
            ),
            (
                LockdownPolicy {
                    disable_show_password: true,
                    ..LockdownPolicy::default()
                },
                EntryVisibility {
                    lock_screen: true,
                    log_out: true,
                    user_switch: true,
                    run_command: true,
                    password_reveal: false,
                    url_handlers: true,
                },
            ),
            (
                LockdownPolicy {
                    disable_application_handlers: true,
                    ..LockdownPolicy::default()
                },
                EntryVisibility {
                    lock_screen: true,
                    log_out: true,
                    user_switch: true,
                    run_command: true,
                    password_reveal: true,
                    url_handlers: false,
                },
            ),
        ];
        for (policy, expected) in cases {
            assert_eq!(policy.entry_visibility(), expected);
        }
    }

    #[test]
    fn app_and_daemon_keys_touch_no_shell_widget() {
        let policy = LockdownPolicy {
            disable_printing: true,
            disable_print_setup: true,
            disable_save_to_disk: true,
            user_administration_disabled: true,
            mount_removable_read_only: true,
            ..LockdownPolicy::default()
        };
        assert!(!policy.is_default());
        assert_eq!(
            policy.entry_visibility(),
            LockdownPolicy::default().entry_visibility()
        );
    }

    #[test]
    fn full_lockdown_hides_every_shell_entry_point() {
        let policy = LockdownPolicy {
            disable_command_line: true,
            disable_save_to_disk: true,
            disable_printing: true,
            disable_print_setup: true,
            disable_user_switching: true,
            disable_lock_screen: true,
            disable_application_handlers: true,
            disable_log_out: true,
            user_administration_disabled: true,
            mount_removable_read_only: true,
            disable_show_password: true,
        };
        assert_eq!(
            policy.entry_visibility(),
            EntryVisibility {
                lock_screen: false,
                log_out: false,
                user_switch: false,
                run_command: false,
                password_reveal: false,
                url_handlers: false,
            }
        );
    }

    #[test]
    fn denial_copy_names_the_restriction() {
        let cases = [
            (ShellAction::RunCommand, "commands"),
            (ShellAction::LogOut, "Logging out"),
            (ShellAction::SwitchUser, "Switching users"),
            (ShellAction::LockScreen, "Locking"),
            (ShellAction::ShowPassword, "passwords"),
            (ShellAction::OpenHandler, "links"),
        ];
        for (action, fragment) in cases {
            assert!(action.denial().contains(fragment), "{action:?}");
            assert!(action.denial().contains("administrator"), "{action:?}");
        }
    }

    #[test]
    fn settings_spawn_blocked_under_deny_and_runs_when_allowed() {
        use std::cell::Cell;
        let calls = Cell::new(0);
        let spawn = |_: Option<&str>| {
            calls.set(calls.get() + 1);
            true
        };
        let denied = LockdownPolicy {
            disable_application_handlers: true,
            ..LockdownPolicy::default()
        };
        assert!(!spawn_settings_for_policy(Some("wifi"), &denied, &spawn));
        assert_eq!(calls.get(), 0, "denied policy must not spawn");
        assert!(spawn_settings_for_policy(
            Some("wifi"),
            &LockdownPolicy::default(),
            &spawn
        ));
        assert_eq!(calls.get(), 1, "allowed policy spawns");
        // Unrelated keys never block the spawn.
        let unrelated = LockdownPolicy {
            disable_lock_screen: true,
            disable_log_out: true,
            ..LockdownPolicy::default()
        };
        assert!(spawn_settings_for_policy(None, &unrelated, &spawn));
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn reveal_icon_hidden_under_deny_and_shown_when_allowed() {
        let denied = LockdownPolicy {
            disable_show_password: true,
            ..LockdownPolicy::default()
        };
        assert_eq!(
            password_reveal_icon(true, &denied.entry_visibility()),
            None,
            "denied policy yields no toggle"
        );
        assert_eq!(
            password_reveal_icon(true, &LockdownPolicy::default().entry_visibility()),
            Some("view-reveal-symbolic"),
            "allowed policy keeps the toggle"
        );
        assert_eq!(
            password_reveal_icon(false, &LockdownPolicy::default().entry_visibility()),
            None,
            "non-password fields never reveal"
        );
    }

    #[test]
    fn live_read_is_deterministic_and_never_panics() {
        // Passes on any backend: two reads agree, and a missing schema
        // or a fresh memory backend reads GNOME defaults. A dconf user
        // database with admin-set keys is the only non-default source,
        // and no test in this binary writes the shared backend.
        let first = read();
        assert_eq!(first, read());
        let memory = std::env::var("GSETTINGS_BACKEND").as_deref() == Ok("memory");
        let missing = gio::SettingsSchemaSource::default()
            .and_then(|source| source.lookup(SCHEMA, true))
            .is_none();
        if memory || missing {
            assert_eq!(first, LockdownPolicy::default());
        }
    }
}
