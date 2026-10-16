//! Lockdown admin UX integration tests (#357).
//!
//! The binary's `lockdown` module cannot be imported from an
//! integration target, so this target includes it by path and proves
//! the GSettings round trip, the spawn/reveal guards, and real widget
//! application. The module's own `#[cfg(test)]` suite compiles into
//! this binary as well.

// The live wrappers (`handlers_allowed`, `open_settings_panel`) run in the
// binary, not here: this target pins their testable core instead of
// spawning real processes, so the unused copies stay quiet.
#[allow(dead_code)]
#[path = "../src/lockdown.rs"]
mod lockdown;

use glib::object::Cast;
use gtk4 as gtk;
use gtk4::prelude::*;
use lockdown::{apply_entry_visibility, read, read_from, LockdownPolicy, SCHEMA};

/// Every key round-trips through a private memory backend: set all,
/// read all, clear all, read defaults. Skips the value assertions when
/// the lockdown schema is not installed (fail-safe path instead),
/// unless `TUNA_REQUIRE_LOCKDOWN_SCHEMA` is set, as CI does so the
/// round trip can never silently skip.
#[test]
fn policy_round_trips_through_memory_backend() {
    let source = gio::SettingsSchemaSource::default();
    let installed = source
        .as_ref()
        .is_some_and(|source| source.lookup(SCHEMA, true).is_some());
    if !installed {
        assert!(
            std::env::var_os("TUNA_REQUIRE_LOCKDOWN_SCHEMA").is_none(),
            "TUNA_REQUIRE_LOCKDOWN_SCHEMA is set but {SCHEMA} is not installed"
        );
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
    // The guards follow the same snapshot: denied handlers block the
    // spawn and denied reveal yields no toggle.
    let calls = std::cell::Cell::new(0);
    assert!(!lockdown::spawn_settings_for_policy(
        Some("wifi"),
        &locked,
        &|_: Option<&str>| {
            calls.set(calls.get() + 1);
            true
        }
    ));
    assert_eq!(calls.get(), 0, "denied snapshot must not spawn");
    assert_eq!(
        lockdown::password_reveal_icon(true, &locked.entry_visibility()),
        None,
        "denied snapshot yields no reveal toggle"
    );
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
    let restored = read_from(&settings);
    assert_eq!(restored, LockdownPolicy::default());
    // Lifting the policy restores the spawn and the toggle.
    assert!(lockdown::spawn_settings_for_policy(
        Some("wifi"),
        &restored,
        &|_: Option<&str>| true
    ));
    assert_eq!(
        lockdown::password_reveal_icon(true, &restored.entry_visibility()),
        Some("view-reveal-symbolic")
    );
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
    // The top Settings button hides while handlers deny.
    let settings_btn = gtk::Button::with_label("Settings");
    let handlers_locked = LockdownPolicy {
        disable_application_handlers: true,
        ..LockdownPolicy::default()
    };
    lockdown::apply_settings_visibility(&handlers_locked.entry_visibility(), &settings_btn);
    assert!(!settings_btn.is_visible());
    lockdown::apply_settings_visibility(
        &LockdownPolicy::default().entry_visibility(),
        &settings_btn,
    );
    assert!(settings_btn.is_visible());
}
