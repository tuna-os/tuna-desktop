//! Lockdown admin UX integration tests (#357).
//!
//! The `lockdown` module is not yet registered in `main.rs` (union-time
//! patch, see evidence), so this target includes it by path and proves
//! the GSettings round trip plus real widget application. The module's
//! own `#[cfg(test)]` suite compiles into this binary as well.

#[path = "../src/lockdown.rs"]
mod lockdown;

use glib::object::Cast;
use gtk4 as gtk;
use gtk4::prelude::*;
use lockdown::{apply_entry_visibility, read, read_from, LockdownPolicy, SCHEMA};

/// Every key round-trips through a private memory backend: set all,
/// read all, clear all, read defaults. Skips the value assertions when
/// the lockdown schema is not installed (fail-safe path instead).
#[test]
fn policy_round_trips_through_memory_backend() {
    let source = gio::SettingsSchemaSource::default();
    let installed = source
        .as_ref()
        .is_some_and(|source| source.lookup(SCHEMA, true).is_some());
    if !installed {
        assert_eq!(read(), LockdownPolicy::default());
        return;
    }
    let backend = gio::memory_settings_backend_new();
    let settings = gio::Settings::with_backend(SCHEMA, &backend);
    for key in [
        lockdown::KEY_DISABLE_COMMAND_LINE,
        lockdown::KEY_DISABLE_SAVE_TO_DISK,
        lockdown::KEY_DISABLE_PRINTING,
        lockdown::KEY_DISABLE_PRINT_SETUP,
        lockdown::KEY_DISABLE_USER_SWITCHING,
        lockdown::KEY_DISABLE_LOCK_SCREEN,
        lockdown::KEY_DISABLE_APPLICATION_HANDLERS,
        lockdown::KEY_DISABLE_LOG_OUT,
        lockdown::KEY_USER_ADMINISTRATION_DISABLED,
        lockdown::KEY_MOUNT_REMOVABLE_READ_ONLY,
        lockdown::KEY_DISABLE_SHOW_PASSWORD,
    ] {
        settings.set_boolean(key, true).unwrap();
    }
    let locked = read_from(&settings);
    assert!(!locked.is_default());
    let visibility = locked.entry_visibility();
    assert!(!visibility.lock_screen);
    assert!(!visibility.log_out);
    assert!(!visibility.user_switch);
    assert!(!visibility.run_command);
    assert!(!visibility.password_reveal);
    assert!(!visibility.url_handlers);
    // Policy unlock restores every entry point.
    for key in [
        lockdown::KEY_DISABLE_COMMAND_LINE,
        lockdown::KEY_DISABLE_LOCK_SCREEN,
        lockdown::KEY_DISABLE_LOG_OUT,
        lockdown::KEY_DISABLE_USER_SWITCHING,
        lockdown::KEY_DISABLE_SHOW_PASSWORD,
        lockdown::KEY_DISABLE_APPLICATION_HANDLERS,
    ] {
        settings.set_boolean(key, false).unwrap();
    }
    for key in [
        lockdown::KEY_DISABLE_SAVE_TO_DISK,
        lockdown::KEY_DISABLE_PRINTING,
        lockdown::KEY_DISABLE_PRINT_SETUP,
        lockdown::KEY_USER_ADMINISTRATION_DISABLED,
        lockdown::KEY_MOUNT_REMOVABLE_READ_ONLY,
    ] {
        settings.set_boolean(key, false).unwrap();
    }
    assert_eq!(read_from(&settings), LockdownPolicy::default());
}

/// Real widgets flip visibility under the policy. Needs a display;
/// run under `xvfb-run`.
#[test]
#[ignore = "requires a display (xvfb-run) for GTK widgets"]
fn widgets_apply_entry_visibility() {
    gtk::init().expect("GTK display");
    let lock = gtk::Button::with_label("Lock");
    let log_out = gtk::Button::with_label("Log Out…");
    let user_switch = gtk::Button::with_label("Switch User");
    let locked = LockdownPolicy {
        disable_lock_screen: true,
        disable_log_out: true,
        disable_user_switching: true,
        ..LockdownPolicy::default()
    };
    apply_entry_visibility(
        &locked.entry_visibility(),
        lock.upcast_ref(),
        log_out.upcast_ref(),
        user_switch.upcast_ref(),
    );
    assert!(!lock.is_visible());
    assert!(!log_out.is_visible());
    assert!(!user_switch.is_visible());
    apply_entry_visibility(
        &LockdownPolicy::default().entry_visibility(),
        lock.upcast_ref(),
        log_out.upcast_ref(),
        user_switch.upcast_ref(),
    );
    assert!(lock.is_visible());
    assert!(log_out.is_visible());
    assert!(user_switch.is_visible());
}
