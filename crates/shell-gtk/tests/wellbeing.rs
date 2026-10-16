//! Wellbeing integration tests (P-ST-02, #354).
//!
//! The module is included by path so these tests run without `main.rs`
//! registration (that wiring lands at union time); after union they keep
//! exercising the same public API.

#[path = "../src/wellbeing.rs"]
mod wellbeing;

use std::path::{Path, PathBuf};

use gio::prelude::*;
use wellbeing::{
    BreakService, BreakState, BreakType, HistoryError, HistoryStore, ScreenTimeEvent,
    ScreenTimeSettings, ScreenTimeTracker, TimeLimitsState, Transition, UserState,
};

const T0: i64 = 1_760_000_000;

fn tr(old: UserState, new: UserState, wall: i64) -> Transition {
    Transition {
        old_state: old,
        new_state: new,
        wall_time_secs: wall,
    }
}

fn history_file(dir: &Path) -> PathBuf {
    dir.join("session-active-history.json")
}

#[test]
fn history_file_round_trip_restart_and_permissions() {
    let dir = tempfile::tempdir().unwrap();
    let path = history_file(dir.path());
    let mut store = HistoryStore::new(path.clone());
    assert_eq!(store.load(T0), Ok(0));
    store.push(tr(UserState::Inactive, UserState::Active, T0));
    store.push(tr(UserState::Active, UserState::Inactive, T0 + 60));
    store.store(T0 + 60).unwrap();

    // Owner-only, as Gio.FileCreateFlags.PRIVATE.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "history must be owner-only");
    }
    assert!(
        !path.with_extension("json.tmp").exists(),
        "no temp file left behind"
    );

    // A fresh store (session restart) loads the same transitions.
    let mut again = HistoryStore::new(path.clone());
    assert_eq!(again.load(T0 + 60), Ok(2));
    assert_eq!(again.transitions(), store.transitions());
}

#[test]
fn retention_and_corrupt_files_behave() {
    let dir = tempfile::tempdir().unwrap();
    let path = history_file(dir.path());
    // Ancient entries are dropped on load except the newest seed.
    let ancient = T0 - wellbeing::HISTORY_THRESHOLD_SECONDS - 100;
    std::fs::write(
        &path,
        wellbeing::render_history(&[
            tr(UserState::Inactive, UserState::Active, ancient),
            tr(UserState::Active, UserState::Inactive, ancient + 10),
            tr(UserState::Inactive, UserState::Active, T0 - 5),
        ]),
    )
    .unwrap();
    let mut store = HistoryStore::new(path.clone());
    assert_eq!(store.load(T0), Ok(1));
    assert_eq!(store.transitions()[0].wall_time_secs, T0 - 5);

    // Corrupt bytes report Corrupt and are then replaced atomically.
    std::fs::write(&path, "{oops").unwrap();
    let mut store = HistoryStore::new(path.clone());
    assert!(matches!(store.load(T0), Err(HistoryError::Corrupt(_))));
    assert!(store.transitions().is_empty());
    store.push(tr(UserState::Inactive, UserState::Active, T0));
    store.store(T0).unwrap();
    assert_eq!(
        wellbeing::parse_history(&std::fs::read_to_string(&path).unwrap())
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn scripted_day_warns_reaches_and_resets() {
    let dir = tempfile::tempdir().unwrap();
    let (today, tomorrow) = wellbeing::start_of_today(T0);
    std::fs::write(
        history_file(dir.path()),
        wellbeing::render_history(&[tr(UserState::Inactive, UserState::Active, today)]),
    )
    .unwrap();
    let mut tracker = ScreenTimeTracker::new(
        ScreenTimeSettings {
            history_enabled: true,
            daily_limit_enabled: true,
            daily_limit_secs: T0 - today + 120,
            grayscale: true,
        },
        HistoryStore::new(history_file(dir.path())),
    );
    let poll = tracker.poll(T0, 1000, true);
    assert!(poll.stored.is_ok());
    assert!(
        poll.events.iter().any(|e| matches!(
            e,
            ScreenTimeEvent::LimitUpcoming {
                remaining_secs: 120
            }
        )),
        "{:?}",
        poll.events
    );

    let poll = tracker.poll(T0 + 120, 1120, true);
    assert_eq!(tracker.state(), TimeLimitsState::LimitReached);
    assert!(poll.events.contains(&ScreenTimeEvent::LimitReached));
    assert!(poll
        .events
        .contains(&ScreenTimeEvent::GrayscaleChanged(true)));

    // Disabling history mid-day deletes the file and stops the machine.
    tracker.set_settings(ScreenTimeSettings::default());
    let poll = tracker.poll(T0 + 150, 1150, true);
    assert_eq!(tracker.state(), TimeLimitsState::Active);
    assert!(poll
        .events
        .contains(&ScreenTimeEvent::GrayscaleChanged(false)));
    tracker.set_settings(ScreenTimeSettings {
        history_enabled: false,
        ..Default::default()
    });
    tracker.poll(T0 + 180, 1180, true);
    assert_eq!(tracker.state(), TimeLimitsState::Disabled);
    assert!(!history_file(dir.path()).exists());

    // Re-enabled the next day, the limit starts over (fresh tracker, same path).
    let mut tracker = ScreenTimeTracker::new(
        ScreenTimeSettings {
            history_enabled: true,
            daily_limit_enabled: true,
            daily_limit_secs: 3600,
            grayscale: false,
        },
        HistoryStore::new(history_file(dir.path())),
    );
    let mono = 1180 + (tomorrow + 10 - (T0 + 180));
    tracker.poll(tomorrow + 10, mono, true);
    assert_eq!(tracker.state(), TimeLimitsState::Active);
    assert_eq!(
        tracker.daily_limit_time(tomorrow + 10),
        tomorrow + 10 + 3600
    );
}

#[test]
fn break_service_public_flow() {
    let mut service = BreakService::new();
    service
        .manager
        .set_selected(&[(BreakType::Movement, BreakType::Movement.defaults())], T0);
    // Movement defaults: interval 1800 s, upcoming toggle on.
    let events = service.poll(T0 + 1680, 0);
    assert!(
        events.iter().any(|e| matches!(
            e,
            wellbeing::BreakUiEvent::Notify(n) if n.title == "Movement Break Soon"
        )),
        "{events:?}"
    );
    let events = service.poll(T0 + 1800, 0);
    assert_eq!(service.manager.state(), BreakState::BreakDue);
    assert!(
        events.iter().any(|e| matches!(
            e,
            wellbeing::BreakUiEvent::Notify(n) if n.title == "Time for a Movement Break"
        )),
        "{events:?}"
    );
    // Take fades (lock off); idling starts the break; a full 300 s idle
    // completes it with the end sound.
    assert_eq!(
        service.take_break(),
        vec![wellbeing::BreakUiEvent::FadeScreen(true)]
    );
    let events = service.poll(T0 + 1900, 100);
    assert_eq!(service.manager.state(), BreakState::InBreak);
    assert!(
        events.iter().any(|e| matches!(
            e,
            wellbeing::BreakUiEvent::Notify(n) if n.title == "Movement Break in Progress"
        )),
        "{events:?}"
    );
    let events = service.poll(T0 + 2300, 0);
    assert_eq!(service.manager.state(), BreakState::Active);
    assert!(events.contains(&wellbeing::BreakUiEvent::PlayCompleteSound));
    // Live settings updates take effect on the next poll.
    let mut cfg = BreakType::Movement.defaults();
    cfg.interval_secs = 60;
    service.manager.set_break_settings(BreakType::Movement, cfg);
    assert_eq!(
        service
            .manager
            .config(BreakType::Movement)
            .unwrap()
            .interval_secs,
        60
    );
    let events = service.poll(T0 + 2361, 0);
    assert_eq!(service.manager.state(), BreakState::BreakDue);
    assert!(
        events.iter().any(|e| matches!(
            e,
            wellbeing::BreakUiEvent::Notify(n) if n.title == "Time for a Movement Break"
        )),
        "{events:?}"
    );
}

#[test]
fn documented_constants_and_error_strings() {
    use std::time::Duration;
    assert_eq!(wellbeing::SCREEN_TIME_POLL, Duration::from_secs(30));
    assert_eq!(wellbeing::BREAK_POLL, Duration::from_secs(2));
    assert_eq!(wellbeing::GRAYSCALE_FADE_SECONDS, 3.0);
    assert_eq!(wellbeing::GRAYSCALE_SATURATION, 1.0);
    assert_eq!(wellbeing::BREAK_FADE_SECONDS, 3.0);
    assert_eq!(wellbeing::BREAK_BRIGHTNESS_FACTOR, 0.6);
    assert_eq!(HistoryError::NotFound.to_string(), "history file not found");
    assert!(HistoryError::Corrupt("x".into()).to_string().contains('x'));
    let dir = tempfile::tempdir().unwrap();
    let store = HistoryStore::new(history_file(dir.path()));
    assert!(store.path().ends_with("session-active-history.json"));
}

/// Compile the 51.0 fixture schemas and read them through a memory backend:
/// defaults match the tag, writes round-trip, and unknown break ids are
/// skipped without touching child schemas.
#[test]
fn gsettings_fixture_defaults_and_selection() {
    let schemas = tempfile::tempdir().unwrap();
    std::fs::copy(
        format!(
            "{}/../../tests/settings/gnome51-wellbeing.gschema.xml",
            env!("CARGO_MANIFEST_DIR")
        ),
        schemas.path().join("gnome51-wellbeing.gschema.xml"),
    )
    .unwrap();
    let status = std::process::Command::new("glib-compile-schemas")
        .arg(schemas.path())
        .status()
        .expect("glib-compile-schemas must be installed");
    assert!(status.success());

    // Memory backend keeps this hermetic (no session bus, no dconf writes).
    // This is the only GSettings user in this test binary, so the
    // process-wide backend switch cannot leak into other tests.
    std::env::set_var("GSETTINGS_BACKEND", "memory");
    let default_source = gio::SettingsSchemaSource::default();
    let source =
        gio::SettingsSchemaSource::from_directory(schemas.path(), default_source.as_ref(), false)
            .unwrap();
    let open = |id: &str| {
        let schema = source.lookup(id, true).unwrap();
        gio::Settings::new_full(&schema, None::<&gio::SettingsBackend>, None)
    };

    // Screen-time defaults are the 51.0 tag values.
    let limits = open(wellbeing::SCREEN_TIME_SCHEMA);
    let settings = wellbeing::read_screen_time_settings_from(&limits);
    assert_eq!(settings, ScreenTimeSettings::default());
    // A settings write round-trips through the reader.
    limits.set_uint("daily-limit-seconds", 3600).unwrap();
    limits.set_boolean("daily-limit-enabled", true).unwrap();
    let settings = wellbeing::read_screen_time_settings_from(&limits);
    assert!(settings.daily_limit_enabled && settings.daily_limit_secs == 3600);

    // Eyesight/movement defaults match the tag (incl. the split toggles).
    let eyesight = wellbeing::read_break_settings_from(&open(BreakType::Eyesight.schema_id()));
    assert_eq!(eyesight, BreakType::Eyesight.defaults());
    assert!(!eyesight.notify_upcoming);
    let movement = wellbeing::read_break_settings_from(&open(BreakType::Movement.schema_id()));
    assert_eq!(movement, BreakType::Movement.defaults());
    assert!(movement.notify_upcoming);

    // Selection keeps known ids, skips unknown ones, and reads children.
    let parent = open(wellbeing::BREAK_SCHEMA);
    parent
        .set_strv("selected-breaks", ["eyesight", "nap", "movement"])
        .unwrap();
    let selected: Vec<(BreakType, _)> = parent
        .strv("selected-breaks")
        .iter()
        .filter_map(|id| BreakType::from_id(id))
        .map(|t| (t, wellbeing::read_break_settings_from(&open(t.schema_id()))))
        .collect();
    assert_eq!(selected.len(), 2);
    assert_eq!(selected[0].0, BreakType::Eyesight);
    assert_eq!(selected[1].1.interval_secs, 1800);
}
