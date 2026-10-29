//! Administrative lockdown admin UX (`org.gnome.desktop.lockdown`, P-ST-05, #357).
//!
//! Shell side of the lockdown policy: read the eleven keys through
//! `gio::Settings` (quietly degrading to GNOME defaults when the schema,
//! a key, or the bus is missing), resolve which shell entry points stay
//! available, and apply that to the widgets. The decision engine — key
//! inventory, owners, denial copy — lives in the compositor's
//! `lockdown` module; this file mirrors only the key names needed to
//! read GSettings directly, and stays free of `crate::` uses so the
//! integration test can include it by path (binary modules are not
//! importable from integration targets).
//!
//! Every shell consumer below mirrors a GNOME 51 consumer (gnome-shell
//! 51.0 and gnome-session 51.0 sources); keys GNOME's desktop never
//! reads stay unenforced here too.
//!
//! | Entry point | Call site | GNOME 51 source | Application |
//! |---|---|---|---|
//! | Lock button and every lock path | `main.rs` quick-settings Lock, sleep, screensaver/D-Bus, keybinding | `systemActions.js` `_updateLockScreen`, `screenShield.js` `lock()` | hidden when denied, plus a live guard on every `control.lock()` path; idle never locks |
//! | Log Out…, Restart…, Power Off… rows | `main.rs` power menu, `power.rs` | `systemActions.js` `_updateLogout`/`_updateHaveShutdown`/`_updateHaveReboot`, gnome-session `gsm-manager.c` | hidden and refused when `disable-log-out` denies; Suspend stays |
//! | Run a Command dialog (Alt+F2) | `run_dialog.rs` | `runDialog.js` `open()` | never opens when `disable-command-line` denies |
//! | Password peek icons | `lock.rs` unlock prompt, `polkit.rs`, `network_agent.rs` | `st-password-entry.c` `disable-show-password` | [`bind_password_entry`] drops the peek icon and re-hides the text live |
//! | Screenshots and screencasts | `screenshot_ui.rs`, `main.rs` screenshot keys, `screencast.rs` | `screenshot.js` `_storeScreenshot`, `screencastService.js` | `disable-save-to-disk` keeps captures on the clipboard only and refuses screencasts |
//! | User switch row | none (Tuna ships no Switch User) | `systemActions.js` `_updateSwitchUser` | `visible.user_switch` gates the row when it lands |
//!
//! `disable-printing`, `disable-print-setup` (GTK print dialogs in each
//! application), `disable-application-handlers` (no GNOME 51 desktop
//! consumer: gnome-shell, gnome-session and gnome-settings-daemon 51.0
//! never read it), `user-administration-disabled` (Settings' Users
//! panel) and `mount-removable-storage-devices-as-read-only` (udisks2
//! mount options) are read into the snapshot but constrain no shell
//! widget.

use gio::prelude::SettingsExt;
use gtk4 as gtk;
use gtk4::prelude::*;

/// GSettings schema holding the lockdown keys.
pub const SCHEMA: &str = "org.gnome.desktop.lockdown";
/// Run-command dialog gate (Alt+F2).
pub const KEY_DISABLE_COMMAND_LINE: &str = "disable-command-line";
/// Shell screenshot/screencast files and applications' Save As dialogs.
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

/// Which shell entry points stay available under a policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryVisibility {
    /// Lock button and every lock path live.
    pub lock_screen: bool,
    /// Session-ending rows (Log Out…, Restart…, Power Off…) shown.
    pub log_out: bool,
    /// User-switch row shown (gates the row when one exists).
    pub user_switch: bool,
    /// Run a Command dialog opens.
    pub run_command: bool,
    /// Password peek icons shown.
    pub password_reveal: bool,
    /// Screenshots save files and screencasts record.
    pub save_to_disk: bool,
}

/// Shell-constrained action denied by policy, for UX copy.
///
/// Test-only until a shell surface renders denials inline (today's
/// guards hide widgets or log to stderr): the unit suite pins the copy.
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
        }
    }
}

impl LockdownPolicy {
    /// Resolve which shell entry points stay available.
    pub fn entry_visibility(&self) -> EntryVisibility {
        EntryVisibility {
            lock_screen: !self.disable_lock_screen,
            log_out: !self.disable_log_out,
            user_switch: !self.disable_user_switching,
            run_command: !self.disable_command_line,
            password_reveal: !self.disable_show_password,
            save_to_disk: !self.disable_save_to_disk,
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
pub fn settings() -> Option<gio::Settings> {
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

/// Reveal-toggle icon for a secret entry, if the field is a password
/// field and the policy shows passwords. A denied policy (or a
/// non-password field) yields no icon, so no toggle path exists.
pub fn password_reveal_icon(
    password_field: bool,
    visibility: &EntryVisibility,
) -> Option<&'static str> {
    (password_field && visibility.password_reveal).then_some("view-reveal-symbolic")
}

/// Apply `disable-show-password` to a password entry as GNOME 51's
/// `StPasswordEntry` does (`st-password-entry.c`
/// `on_disable_show_password_changed`, with `shellEntry.js` binding the
/// context menu's "Show Password" item to the peek icon): denied, the
/// typed text goes back to hidden, the peek icon disappears and GTK's
/// context-menu "Show Text" (`misc.toggle-visibility` on the inner
/// `GtkText`, which GTK offers even without the icon) is disabled;
/// allowed, both return with the text still hidden, so only a fresh
/// peek reveals it.
pub fn apply_password_peek(entry: &gtk::PasswordEntry, allowed: bool) {
    if let Some(text) = entry.delegate().and_downcast::<gtk::Text>() {
        if !allowed {
            text.set_visibility(false);
        }
        text.action_set_enabled("misc.toggle-visibility", allowed);
    }
    entry.set_show_peek_icon(allowed);
}

/// Bind a password entry's peek icon to the live lockdown schema: the
/// icon follows `disable-show-password` now and on every change, for as
/// long as the entry lives. Without the schema the icon shows, as GNOME
/// shows it by default.
pub fn bind_password_entry(entry: &gtk::PasswordEntry) {
    match settings() {
        Some(settings) => bind_password_entry_to(entry, settings),
        None => entry.set_show_peek_icon(true),
    }
}

/// [`bind_password_entry`] against an explicit settings object (memory
/// backends in tests). The entry holds the subscription and drops it
/// with itself. A reveal from any path while the policy denies (a
/// stale menu, a programmatic toggle) is undone at once from the same
/// live read.
pub fn bind_password_entry_to(entry: &gtk::PasswordEntry, settings: gio::Settings) {
    apply_password_peek(
        entry,
        read_from(&settings).entry_visibility().password_reveal,
    );
    let weak = entry.downgrade();
    let handler = settings.connect_changed(Some(KEY_DISABLE_SHOW_PASSWORD), move |s, _| {
        if let Some(entry) = weak.upgrade() {
            apply_password_peek(&entry, read_from(s).entry_visibility().password_reveal);
        }
    });
    if let Some(text) = entry.delegate().and_downcast::<gtk::Text>() {
        let guard = settings.clone();
        text.connect_notify_local(Some("visibility"), move |text, _| {
            if text.property::<bool>("visibility")
                && !read_from(&guard).entry_visibility().password_reveal
            {
                text.set_visibility(false);
            }
        });
    }
    let held = std::cell::RefCell::new(Some((settings, handler)));
    entry.connect_destroy(move |_| {
        if let Some((settings, handler)) = held.borrow_mut().take() {
            settings.disconnect(handler);
        }
    });
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
                save_to_disk: true,
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
                    save_to_disk: true,
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
                    save_to_disk: true,
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
                    save_to_disk: true,
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
                    save_to_disk: true,
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
                    save_to_disk: true,
                },
            ),
            (
                LockdownPolicy {
                    disable_save_to_disk: true,
                    ..LockdownPolicy::default()
                },
                EntryVisibility {
                    lock_screen: true,
                    log_out: true,
                    user_switch: true,
                    run_command: true,
                    password_reveal: true,
                    save_to_disk: false,
                },
            ),
        ];
        for (policy, expected) in cases {
            assert_eq!(policy.entry_visibility(), expected);
        }
    }

    #[test]
    fn keys_without_a_gnome_shell_consumer_touch_no_shell_widget() {
        let policy = LockdownPolicy {
            disable_printing: true,
            disable_print_setup: true,
            disable_application_handlers: true,
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
                save_to_disk: false,
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
        ];
        for (action, fragment) in cases {
            assert!(action.denial().contains(fragment), "{action:?}");
            assert!(action.denial().contains("administrator"), "{action:?}");
        }
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
