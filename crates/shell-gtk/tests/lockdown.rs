//! Lockdown admin UX integration tests (#357).
//!
//! The binary's `lockdown` module cannot be imported from an
//! integration target, so this target includes it by path and proves
//! the GSettings round trip, the reveal guard, and real widget
//! application (visibility and the live password-peek binding). The
//! module's own `#[cfg(test)]` suite compiles into this binary as well.

// The live readers (`read`, `bind_password_entry`) run in the binary;
// this target drives their memory-backend cores, so the unused copies
// stay quiet.
#[allow(dead_code)]
#[path = "../src/lockdown.rs"]
mod lockdown;

use std::sync::{mpsc, LazyLock, Mutex};

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
    assert!(!visibility.save_to_disk);
    // The reveal guard follows the same snapshot.
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
    // Lifting the policy restores the toggle.
    assert_eq!(
        lockdown::password_reveal_icon(true, &restored.entry_visibility()),
        Some("view-reveal-symbolic")
    );
}

type GtkJob = (fn(), mpsc::Sender<Result<(), String>>);

/// GTK refuses initialization from a second thread, while libtest runs
/// every test on its own thread (even with `--test-threads=1`). Every
/// display test therefore runs its body on one dedicated GTK thread,
/// one body at a time, and re-raises the body's panic on the test's
/// own thread.
static GTK_THREAD: LazyLock<Mutex<mpsc::Sender<GtkJob>>> = LazyLock::new(|| {
    let (jobs, inbox) = mpsc::channel::<GtkJob>();
    std::thread::Builder::new()
        .name("gtk".into())
        .spawn(move || {
            let display = gtk::init().map_err(|error| format!("GTK display: {error}"));
            for (body, reply) in inbox {
                let outcome = display.clone().and_then(|()| {
                    std::panic::catch_unwind(body).map_err(|payload| {
                        payload
                            .downcast_ref::<&str>()
                            .map(|message| (*message).to_owned())
                            .or_else(|| payload.downcast_ref::<String>().cloned())
                            .unwrap_or_else(|| "non-string panic".to_owned())
                    })
                });
                let _ = reply.send(outcome);
            }
        })
        .expect("spawn the GTK thread");
    Mutex::new(jobs)
});

fn on_gtk_thread(body: fn()) {
    let (reply, outcome) = mpsc::channel();
    GTK_THREAD
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .send((body, reply))
        .expect("GTK thread accepts work");
    if let Err(message) = outcome.recv().expect("GTK thread replies") {
        panic!("on the GTK thread: {message}");
    }
}

/// Real widgets flip visibility under the policy. Needs a display;
/// run under `xvfb-run`.
#[test]
#[ignore = "requires a display (xvfb-run) for GTK widgets"]
fn widgets_apply_entry_visibility() {
    on_gtk_thread(entry_visibility_on_display);
}

fn entry_visibility_on_display() {
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

/// A memory-backend lockdown schema, or `None` (with the CI guard) when
/// the schema is not installed.
fn memory_lockdown() -> Option<gio::Settings> {
    let installed = gio::SettingsSchemaSource::default()
        .is_some_and(|source| source.lookup(SCHEMA, true).is_some());
    if !installed {
        assert!(
            std::env::var_os("TUNA_REQUIRE_LOCKDOWN_SCHEMA").is_none(),
            "TUNA_REQUIRE_LOCKDOWN_SCHEMA is set but {SCHEMA} is not installed"
        );
        return None;
    }
    let backend = gio::memory_settings_backend_new();
    Some(gio::Settings::with_backend(SCHEMA, &backend))
}

/// GtkText's `visibility` (its getter shares `is_visible`'s name).
fn text_shown(entry: &gtk::PasswordEntry) -> bool {
    text_of(entry).property::<bool>("visibility")
}

fn text_of(entry: &gtk::PasswordEntry) -> gtk::Text {
    entry
        .delegate()
        .and_downcast::<gtk::Text>()
        .expect("GtkPasswordEntry delegates to a GtkText")
}

/// GNOME 51's StPasswordEntry contract on the shell's real password
/// widgets (unlock prompt, polkit): the peek icon shows by default;
/// `disable-show-password` flipping on removes it, hides an already
/// revealed password and leaves GTK's "Show Text" toggle inert;
/// flipping off restores the icon with the text still hidden.
#[test]
#[ignore = "requires a display (xvfb-run) for GTK widgets"]
fn password_peek_follows_disable_show_password_live() {
    on_gtk_thread(password_peek_on_display);
}

fn password_peek_on_display() {
    let Some(settings) = memory_lockdown() else {
        return;
    };
    let context = glib::MainContext::default();
    let entry = gtk::PasswordEntry::new();
    entry.set_text("hunter2");
    lockdown::bind_password_entry_to(&entry, settings.clone());
    assert!(
        entry.shows_peek_icon(),
        "GNOME shows the peek icon by default"
    );
    // Reveal, as a peek-icon click does.
    text_of(&entry).set_visibility(true);
    assert!(text_shown(&entry));

    settings
        .set_boolean(lockdown::KEY_DISABLE_SHOW_PASSWORD, true)
        .unwrap();
    while context.iteration(false) {}
    assert!(!entry.shows_peek_icon(), "denied: no peek icon");
    assert!(
        !text_shown(&entry),
        "denied: a revealed password goes back to hidden"
    );
    // GTK's context-menu "Show Text" (offered even without the icon)
    // and any direct reveal are both inert while denied.
    let _ = text_of(&entry).activate_action("misc.toggle-visibility", None);
    while context.iteration(false) {}
    assert!(!text_shown(&entry), "denied: Show Text cannot reveal");
    text_of(&entry).set_visibility(true);
    while context.iteration(false) {}
    assert!(!text_shown(&entry), "denied: no reveal path sticks");

    settings
        .set_boolean(lockdown::KEY_DISABLE_SHOW_PASSWORD, false)
        .unwrap();
    while context.iteration(false) {}
    assert!(entry.shows_peek_icon(), "lifted: the peek icon returns");
    assert!(
        !text_shown(&entry),
        "lifted: the text stays hidden until a fresh peek"
    );
    text_of(&entry).set_visibility(true);
    while context.iteration(false) {}
    assert!(text_shown(&entry), "lifted: a fresh peek reveals again");
    // Unrelated keys never touch the icon.
    settings
        .set_boolean(lockdown::KEY_DISABLE_LOG_OUT, true)
        .unwrap();
    while context.iteration(false) {}
    assert!(entry.shows_peek_icon());
}
