//! Wellbeing: screen-time history/limits and break reminders (P-ST-02, #354).
//!
//! A headless port of GNOME Shell 51's `timeLimitsManager.js` (`TimeLimitsManager`)
//! and `breakManager.js` (`BreakManager` + dispatchers), plus the history file
//! GNOME Settings 51's wellbeing panel reads. This module owns no widgets: it is
//! pure state machines, history-file IO and GSettings reads that the GTK shell
//! drives from a timer. Compositor state is read only, through
//! `org.gnome.Mutter.IdleMonitor`'s `GetIdletime` (the same call `notify.rs`
//! makes); nothing here writes compositor state or bumps any protocol version.
//!
//! GNOME 51 baselines (all at the `51.0` tag):
//!
//! - Schemas (defaults and key types):
//!   <https://github.com/GNOME/gsettings-desktop-schemas/tree/51.0/schemas>,
//!   tag object `1a97aa82fe617217ed1b251665f7f3c6b536e407`.
//! - `js/misc/timeLimitsManager.js` and `js/misc/breakManager.js`:
//!   <https://github.com/GNOME/gnome-shell/tree/51.0/js/misc>,
//!   tag object `f50fc2b43906873ecdc3585e765ee01a27ffaa5a`.
//! - `panels/wellbeing/cc-screen-time-statistics-row.c` (history format read by
//!   Settings) and `cc-wellbeing-panel.c` (history path
//!   `$XDG_DATA_HOME/gnome-shell/session-active-history.json`):
//!   <https://github.com/GNOME/gnome-control-center/tree/51.0/panels/wellbeing>,
//!   tag object `0bb754c5f1e793939e504c1d081d846c071c4b7e`.
//!
//! Deliberate deviations from GNOME, all recorded here rather than hidden:
//!
//! - Activity source. GNOME derives active/inactive from logind (`State`,
//!   `IdleHint`, `PreparingForSleep`). Tuna has no logind user tracking in the
//!   shell, so active means "compositor idle time below the session
//!   `idle-delay`", read from IdleMonitor. Lock is a subset of idle (a locked
//!   session accumulates idle time), matching GNOME's accounting. Suspend is
//!   detected as a wall-clock jump with no matching monotonic advance, and the
//!   gap is backdated to inactive from the last poll.
//! - Polling, not watches. GNOME uses idle watches and exact timers; the shell
//!   drives [`ScreenTimeTracker::poll`] (every ~30 s) and [`BreakService::poll`]
//!   (every ~2 s) from GLib timeouts, so edges fire within one poll period.
//! - `format_time_span` uses integer floors for both the number and the plural
//!   form. GNOME's float counts can render "1 minutes" for 90 s; that quirk is
//!   not reproduced.
//! - Parental-controls (malcontent) session limits are out of scope: Tuna has no
//!   malcontent integration, so only the wellbeing daily limit is implemented.
//! - Grayscale and break fade-screen expose *desired* state
//!   ([`ScreenTimeEvent::GrayscaleChanged`], [`BreakUiEvent::FadeScreen`]);
//!   tuna-compositor has no desaturate/brightness effect hook yet, so applying
//!   them is a union-time compositor item (no protocol bump is made here).

#[cfg(not(test))]
use std::cell::RefCell;
use std::collections::BTreeMap;
#[cfg(test)]
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use gio::prelude::*;

// ---------------------------------------------------------------------------
// Schemas, paths and GNOME constants
// ---------------------------------------------------------------------------

/// Wellbeing daily-limit settings.
pub const SCREEN_TIME_SCHEMA: &str = "org.gnome.desktop.screen-time-limits";
/// Break-reminder parent settings (`selected-breaks`).
pub const BREAK_SCHEMA: &str = "org.gnome.desktop.break-reminders";
/// Per-type child schemas, keyed by [`BreakType::id`].
pub const BREAK_EYESIGHT_SCHEMA: &str = "org.gnome.desktop.break-reminders.eyesight";
/// Per-type child schemas, keyed by [`BreakType::id`].
pub const BREAK_MOVEMENT_SCHEMA: &str = "org.gnome.desktop.break-reminders.movement";

/// History directory under `$XDG_DATA_HOME`, shared with gnome-shell so GNOME
/// Settings 51 shows Tuna's history without changes (`cc-wellbeing-panel.c`).
pub const HISTORY_DIR_NAME: &str = "gnome-shell";
/// History file name, exactly as gnome-shell writes it.
pub const HISTORY_FILE_NAME: &str = "session-active-history.json";

/// `HISTORY_THRESHOLD_SECONDS`: entries older than 14 weeks are dropped.
pub const HISTORY_THRESHOLD_SECONDS: i64 = 14 * 7 * 24 * 60 * 60;
/// `LIMIT_UPCOMING_NOTIFICATION_TIME_SECONDS`: warn 10 min before the limit.
pub const LIMIT_UPCOMING_NOTIFICATION_SECONDS: i64 = 10 * 60;
/// `GRAYSCALE_FADE_TIME_SECONDS` / `GRAYSCALE_SATURATION` for limit grayscale.
pub const GRAYSCALE_FADE_SECONDS: f64 = 3.0;
/// Full desaturation factor once the daily limit is reached.
pub const GRAYSCALE_SATURATION: f64 = 1.0;
/// `BREAK_OVERDUE_TIME_SECONDS`: overdue notice 60 s after a break was due.
pub const BREAK_OVERDUE_SECONDS: i64 = 60;
/// `BREAK_UPCOMING_NOTIFICATION_TIME_SECONDS`: warn 2 min before a break.
pub const BREAK_UPCOMING_NOTIFICATION_SECONDS: &[i64] = &[2 * 60];
/// `BREAK_COUNTDOWN_TIME_SECONDS`: prominent countdown for the last 60 s.
pub const BREAK_COUNTDOWN_SECONDS: i64 = 60;
/// `MIN_BREAK_LENGTH_SECONDS`: idle this long before it counts as idle.
pub const MIN_BREAK_LENGTH_SECONDS: i64 = 10;
/// `BRIGHTNESS_FADE_TIME_SECONDS` / `BRIGHTNESS_FACTOR` for break fade-screen.
pub const BREAK_FADE_SECONDS: f64 = 3.0;
/// Brightness multiplier while a fading break is in progress.
pub const BREAK_BRIGHTNESS_FACTOR: f64 = 0.6;
/// GNOME counts the day from 03:00 local to dodge DST transitions at 01:00-02:00.
pub const DAY_START_HOUR: i8 = 3;
/// Break types with pre-defined child schemas, as `SUPPORTED_BREAK_TYPES`.
pub const SUPPORTED_BREAK_TYPES: &[&str] = &["eyesight", "movement"];
/// Recommended screen-time poll period for the shell timer.
pub const SCREEN_TIME_POLL: Duration = Duration::from_secs(30);
/// Recommended break poll period for the shell timer.
pub const BREAK_POLL: Duration = Duration::from_secs(2);

// Schema defaults at the 51.0 tag, used when a schema is not installed.
pub const DEFAULT_HISTORY_ENABLED: bool = true;
pub const DEFAULT_DAILY_LIMIT_ENABLED: bool = false;
pub const DEFAULT_DAILY_LIMIT_SECONDS: i64 = 28800;
pub const DEFAULT_GRAYSCALE: bool = true;
pub const DEFAULT_SESSION_IDLE_DELAY_SECONDS: u32 = 300;

// ---------------------------------------------------------------------------
// History file: `UserState` transitions
// ---------------------------------------------------------------------------

/// Active/inactive user state. The numeric values are the on-disk format, so
/// they must not change (`UserState` in `timeLimitsManager.js`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(i64)]
pub enum UserState {
    Inactive = 0,
    Active = 1,
}

impl UserState {
    fn from_int(v: i64) -> Option<Self> {
        match v {
            0 => Some(Self::Inactive),
            1 => Some(Self::Active),
            _ => None,
        }
    }
}

/// One edge event: the user moved from `old_state` to `new_state` at
/// `wall_time_secs` (wall clock, seconds since the Unix epoch).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transition {
    pub old_state: UserState,
    pub new_state: UserState,
    pub wall_time_secs: i64,
}

/// Why a history file could not be loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryError {
    /// The file does not exist; the caller should start with empty history.
    // Reserved: loaders report a missing file as empty history, so nothing
    // constructs this yet; kept for API completeness and tests.
    #[allow(dead_code)]
    NotFound,
    /// The file exists but is unreadable or fails validation. The raw text is
    /// kept for logging; the file itself is left untouched.
    Corrupt(String),
}

impl std::fmt::Display for HistoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => write!(f, "history file not found"),
            Self::Corrupt(detail) => write!(f, "corrupt history file: {detail}"),
        }
    }
}

/// Parse history with GNOME's loader rules (`_loadTransitions`): a top-level
/// array of `{oldState, newState, wallTimeSecs}` with states in {0, 1},
/// `oldState != newState`, and non-decreasing integer wall times.
pub fn parse_history(text: &str) -> Result<Vec<Transition>, HistoryError> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| HistoryError::Corrupt(e.to_string()))?;
    let array = value
        .as_array()
        .ok_or_else(|| HistoryError::Corrupt("not an array".into()))?;
    let mut out = Vec::with_capacity(array.len());
    let mut previous = 0i64;
    for (i, entry) in array.iter().enumerate() {
        let detail = |why: &str| HistoryError::Corrupt(format!("entry {i}: {why}"));
        let obj = entry.as_object().ok_or_else(|| detail("not an object"))?;
        let int = |key: &str| {
            obj.get(key)
                .and_then(serde_json::Value::as_i64)
                .ok_or_else(|| detail(key))
        };
        let old = UserState::from_int(int("oldState")?).ok_or_else(|| detail("oldState"))?;
        let new = UserState::from_int(int("newState")?).ok_or_else(|| detail("newState"))?;
        let wall = int("wallTimeSecs")?;
        if old == new {
            return Err(detail("oldState == newState"));
        }
        if wall < previous {
            return Err(detail("wallTimeSecs decreased"));
        }
        previous = wall;
        out.push(Transition {
            old_state: old,
            new_state: new,
            wall_time_secs: wall,
        });
    }
    Ok(out)
}

/// Serialize history exactly as gnome-shell does: a JSON array of
/// `{oldState, newState, wallTimeSecs}` objects.
pub fn render_history(transitions: &[Transition]) -> String {
    let array: Vec<serde_json::Value> = transitions
        .iter()
        .map(|t| {
            serde_json::json!({
                "oldState": t.old_state as i64,
                "newState": t.new_state as i64,
                "wallTimeSecs": t.wall_time_secs,
            })
        })
        .collect();
    serde_json::Value::Array(array).to_string()
}

/// GNOME's `_filterOldTransition`: always drop future entries; drop entries
/// older than the retention threshold except the newest one, which seeds the
/// current open interval.
pub fn keep_transition(entry: &Transition, idx: usize, len: usize, now_secs: i64) -> bool {
    entry.wall_time_secs <= now_secs
        && (entry.wall_time_secs >= now_secs - HISTORY_THRESHOLD_SECONDS || idx + 1 == len)
}

/// Drop the first entry of consecutive equal-time pairs whose states cancel out
/// (an on/off toggle within the same second), exactly as `_storeTransitions`
/// does: the loop skips pushing `first` when it pairs with `second`, and
/// `second` itself survives unless it also pairs forward.
///
/// Micro-deviation: a leading `ACTIVE → INACTIVE` left behind by the collapse
/// is dropped too. It closes an interval that no longer exists, and keeping it
/// would count the whole day as active (e.g. a suspend backdated into the
/// startup second). GNOME Settings already skips such a leading edge when it
/// renders history, so dropping it keeps the limit accounting and the panel
/// in agreement.
pub fn collapse_equal_time_pairs(transitions: &mut Vec<Transition>) {
    let mut kept = Vec::with_capacity(transitions.len());
    for (i, first) in transitions.iter().enumerate() {
        let pairs = transitions.get(i + 1).is_some_and(|next| {
            next.wall_time_secs == first.wall_time_secs && first.old_state == next.new_state
        });
        if !pairs {
            kept.push(*first);
        }
    }
    if kept
        .first()
        .is_some_and(|t| t.old_state == UserState::Active && t.new_state == UserState::Inactive)
    {
        kept.remove(0);
    }
    *transitions = kept;
}

/// The history path GNOME Settings 51 reads:
/// `$XDG_DATA_HOME/gnome-shell/session-active-history.json`.
pub fn history_path() -> PathBuf {
    glib::user_data_dir()
        .join(HISTORY_DIR_NAME)
        .join(HISTORY_FILE_NAME)
}

/// In-memory history plus its backing file. Load failures other than
/// "missing" leave the file on disk untouched and report [`HistoryError`];
/// the tracker then starts from empty history without fabricating usage.
#[derive(Debug)]
pub struct HistoryStore {
    path: PathBuf,
    transitions: Vec<Transition>,
}

impl HistoryStore {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            transitions: Vec::new(),
        }
    }

    // Inspection API for tests and diagnostics; the runtime holds the path.
    #[allow(dead_code)]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn transitions(&self) -> &[Transition] {
        &self.transitions
    }

    /// Load, validate and apply retention, mirroring `_loadTransitions`.
    /// Returns the number of transitions kept.
    pub fn load(&mut self, now_secs: i64) -> Result<usize, HistoryError> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                self.transitions.clear();
                return Ok(0);
            }
            Err(e) => return Err(HistoryError::Corrupt(e.to_string())),
        };
        let parsed = parse_history(&text)?;
        let len = parsed.len();
        self.transitions = parsed
            .into_iter()
            .enumerate()
            .filter(|(i, t)| keep_transition(t, *i, len, now_secs))
            .map(|(_, t)| t)
            .collect();
        Ok(self.transitions.len())
    }

    pub fn push(&mut self, transition: Transition) {
        self.transitions.push(transition);
    }

    /// Trim retention, collapse equal-time pairs and write atomically
    /// (temp file plus rename) with owner-only permissions, as
    /// `_storeTransitions` does with `Gio.FileCreateFlags.PRIVATE`. An empty
    /// store deletes the file instead.
    pub fn store(&mut self, now_secs: i64) -> io::Result<()> {
        let len = self.transitions.len();
        let kept: Vec<Transition> = std::mem::take(&mut self.transitions)
            .into_iter()
            .enumerate()
            .filter(|(i, t)| keep_transition(t, *i, len, now_secs))
            .map(|(_, t)| t)
            .collect();
        self.transitions = kept;
        collapse_equal_time_pairs(&mut self.transitions);
        if self.transitions.is_empty() {
            return self.delete();
        }
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let body = render_history(&self.transitions);
        let tmp = self.path.with_extension("json.tmp");
        {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create(true).truncate(true).mode(0o600);
            let mut file = opts.open(&tmp)?;
            file.write_all(body.as_bytes())?;
            file.sync_all()?;
        }
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    /// Delete the backing file, ignoring "not found" (`_deleteTransitions`).
    /// Recording can only resume by writing fresh transitions afterwards.
    pub fn delete(&mut self) -> io::Result<()> {
        self.transitions.clear();
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
}

// ---------------------------------------------------------------------------
// Day boundaries and usage accounting
// ---------------------------------------------------------------------------

/// Start of today and start of tomorrow as Unix seconds, with GNOME's 03:00
/// local day boundary (`_getStartOfTodaySecs`). Times in [00:00, 03:00) count
/// towards the previous day. Civil-day arithmetic keeps DST days correct (a
/// 23/25-hour day still starts at 03:00 local).
pub fn start_of_today(now_secs: i64) -> (i64, i64) {
    start_of_today_in(&jiff::tz::TimeZone::system(), now_secs)
}

/// [`start_of_today`] in an explicit time zone (DST tests use fixed zones).
pub fn start_of_today_in(tz: &jiff::tz::TimeZone, now_secs: i64) -> (i64, i64) {
    let now = jiff::Timestamp::from_second(now_secs).unwrap_or(jiff::Timestamp::UNIX_EPOCH);
    let at3 = |day: jiff::civil::Date| {
        day.at(DAY_START_HOUR, 0, 0, 0)
            .to_zoned(tz.clone())
            .map(|z| z.timestamp())
            .unwrap_or(now)
    };
    let mut day = now.to_zoned(tz.clone()).date();
    if at3(day) > now {
        day = day.yesterday().unwrap_or(day);
    }
    let today_secs = at3(day).as_second();
    let tomorrow_secs = at3(day.tomorrow().unwrap_or(day)).as_second();
    debug_assert!(today_secs <= now_secs, "start of today in the future");
    debug_assert!(
        tomorrow_secs > now_secs,
        "start of tomorrow not in the future"
    );
    (today_secs, tomorrow_secs)
}

/// Active screen seconds since `start_of_today_secs`, ported from
/// `_calculateActiveTimeTodaySecs`. An open trailing ACTIVE interval counts up
/// to `now_secs`; a first transition that ends an interval started before
/// today counts from the day boundary. Never negative.
pub fn active_time_today(
    transitions: &[Transition],
    now_secs: i64,
    start_of_today_secs: i64,
) -> i64 {
    let first = transitions
        .iter()
        .position(|t| t.wall_time_secs >= start_of_today_secs);
    let mut total = 0i64;
    let mut active_start = start_of_today_secs;
    if let Some(first) = first {
        for t in &transitions[first..] {
            if t.new_state == UserState::Active {
                active_start = t.wall_time_secs;
            } else if t.old_state == UserState::Active {
                total += (t.wall_time_secs - active_start).max(0);
            }
        }
    }
    if transitions
        .last()
        .is_some_and(|t| t.new_state == UserState::Active)
    {
        total += (now_secs - active_start).max(0);
    }
    debug_assert!(total >= 0, "active time must be non-negative");
    total.max(0)
}

/// When the daily limit was (or will be) reached, ported from
/// `_calculateWellbeingDailyLimitReachedAtSecs`. Returns 0 while the limit is
/// not reached yet today.
// Reached only through `daily_limit_time` (pending D-Bus property surface).
#[allow(dead_code)]
pub fn limit_reached_at(
    transitions: &[Transition],
    now_secs: i64,
    limit_secs: i64,
    start_of_today_secs: i64,
) -> i64 {
    let first = transitions
        .iter()
        .position(|t| t.wall_time_secs >= start_of_today_secs);
    let mut total = 0i64;
    let mut active_start = start_of_today_secs;
    if let Some(first) = first {
        for t in &transitions[first..] {
            if t.new_state == UserState::Active {
                active_start = t.wall_time_secs;
            } else if t.old_state == UserState::Active {
                total += (t.wall_time_secs - active_start).max(0);
            }
            if total >= limit_secs {
                return t.wall_time_secs - (total - limit_secs);
            }
        }
    }
    if transitions
        .last()
        .is_some_and(|t| t.new_state == UserState::Active)
    {
        total += (now_secs - active_start).max(0);
    }
    if total >= limit_secs {
        now_secs - (total - limit_secs)
    } else {
        0
    }
}

// ---------------------------------------------------------------------------
// Screen-time tracker (TimeLimitsManager, wellbeing branch)
// ---------------------------------------------------------------------------

/// `TimeLimitsState`: history disabled, recording, or the daily limit hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum TimeLimitsState {
    Disabled = 0,
    Active = 1,
    LimitReached = 2,
}

/// Snapshot of `org.gnome.desktop.screen-time-limits`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenTimeSettings {
    pub history_enabled: bool,
    pub daily_limit_enabled: bool,
    pub daily_limit_secs: i64,
    pub grayscale: bool,
}

impl Default for ScreenTimeSettings {
    fn default() -> Self {
        Self {
            history_enabled: DEFAULT_HISTORY_ENABLED,
            daily_limit_enabled: DEFAULT_DAILY_LIMIT_ENABLED,
            daily_limit_secs: DEFAULT_DAILY_LIMIT_SECONDS,
            grayscale: DEFAULT_GRAYSCALE,
        }
    }
}

/// What the shell must do after a [`ScreenTimeTracker::poll`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScreenTimeEvent {
    StateChanged(TimeLimitsState),
    /// The limit is exactly 10 min away (GNOME's
    /// `Math.ceil(remainingSecs) === timeoutLimit`): its source schedules an
    /// update for that instant and posts nothing at any other remaining
    /// time, so a limit set at or below 10 min gets no warning.
    LimitUpcoming {
        remaining_secs: i64,
    },
    LimitReached,
    /// Remove the resident limit notification (GNOME destroys it when the
    /// limit is off, history is disabled, or over 10 min remain again, as
    /// after the day rolls over or the limit is raised).
    LimitNoticeCleared,
    /// Desired grayscale state; applying it needs the compositor effect hook.
    GrayscaleChanged(bool),
}

/// Which text the resident limit notification carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LimitNotice {
    Upcoming,
    Reached,
}

/// Outcome of one poll: shell events plus the history load/store results.
/// A corrupt history file reports [`HistoryError`] here while the tracker
/// carries on with empty history; the file on disk is left untouched.
#[derive(Debug)]
pub struct ScreenTimePoll {
    pub events: Vec<ScreenTimeEvent>,
    pub loaded: Option<Result<usize, HistoryError>>,
    pub stored: io::Result<()>,
}

/// Wall-clock jump (with no matching monotonic advance) above which a gap is
/// treated as suspend/inactive rather than an NTP-style clock step.
pub const SUSPEND_GAP_SECONDS: i64 = 120;

/// Screen-time recording and daily limits. Poll with wall time, monotonic time
/// (seconds on any steady base) and whether the user is currently active.
/// Parental-controls session limits are out of scope (no malcontent
/// integration); only the wellbeing daily limit is evaluated.
#[derive(Debug)]
pub struct ScreenTimeTracker {
    settings: ScreenTimeSettings,
    store: HistoryStore,
    user_state: UserState,
    state: TimeLimitsState,
    last_state_change: i64,
    clock_offset: i64,
    last_poll: Option<(i64, i64)>,
    /// The resident limit notification, while it lives
    /// (`TimeLimitsNotificationSource._notification`).
    limit_notice: Option<LimitNotice>,
    grayscale_on: bool,
    started: bool,
}

impl ScreenTimeTracker {
    pub fn new(settings: ScreenTimeSettings, store: HistoryStore) -> Self {
        Self {
            settings,
            store,
            user_state: UserState::Active,
            state: TimeLimitsState::Disabled,
            last_state_change: 0,
            clock_offset: 0,
            last_poll: None,
            limit_notice: None,
            grayscale_on: false,
            started: false,
        }
    }

    // Inspection API for tests and pending status surfaces.
    #[allow(dead_code)]
    pub fn state(&self) -> TimeLimitsState {
        self.state
    }

    // Inspection API for tests and pending status surfaces.
    #[allow(dead_code)]
    pub fn user_state(&self) -> UserState {
        self.user_state
    }

    // Inspection API for tests and pending status surfaces.
    #[allow(dead_code)]
    pub fn settings(&self) -> ScreenTimeSettings {
        self.settings
    }

    pub fn set_settings(&mut self, settings: ScreenTimeSettings) {
        self.settings = settings;
    }

    // Inspection API for tests and pending status surfaces.
    #[allow(dead_code)]
    pub fn store(&self) -> &HistoryStore {
        &self.store
    }

    /// Mirror of GNOME's `dailyLimitTime` (wellbeing branch): when the limit
    /// will be / was reached, or 0 while inactive, disabled, or unlimited.
    // No D-Bus property surface reads this yet; exercised by tests.
    #[allow(dead_code)]
    pub fn daily_limit_time(&self, now_secs: i64) -> i64 {
        match self.state {
            TimeLimitsState::Disabled => 0,
            TimeLimitsState::Active => {
                if !self.settings.daily_limit_enabled || self.user_state == UserState::Inactive {
                    return 0;
                }
                let (start_today, _) = start_of_today(now_secs);
                let active = active_time_today(self.store.transitions(), now_secs, start_today);
                now_secs + (self.settings.daily_limit_secs - active).max(0)
            }
            TimeLimitsState::LimitReached => {
                let (start_today, _) = start_of_today(now_secs);
                limit_reached_at(
                    self.store.transitions(),
                    now_secs,
                    self.settings.daily_limit_secs,
                    start_today,
                )
            }
        }
    }

    /// One poll step. `active` is the compositor-idle verdict (idle time below
    /// the session `idle-delay`).
    pub fn poll(&mut self, now_wall: i64, now_mono: i64, active: bool) -> ScreenTimePoll {
        let mut events = Vec::new();
        let mut loaded = None;

        if !self.settings.history_enabled {
            if self.state != TimeLimitsState::Disabled {
                self.state = TimeLimitsState::Disabled;
                events.push(ScreenTimeEvent::StateChanged(TimeLimitsState::Disabled));
            }
            if self.grayscale_on {
                self.grayscale_on = false;
                events.push(ScreenTimeEvent::GrayscaleChanged(false));
            }
            // `TimeLimitsState.DISABLED` destroys the notification source.
            self.clear_limit_notice(&mut events);
            let stored = self.store.delete();
            self.started = false;
            self.last_poll = None;
            return ScreenTimePoll {
                events,
                loaded,
                stored,
            };
        }

        if !self.started {
            self.started = true;
            self.user_state = UserState::Active;
            self.state = TimeLimitsState::Active;
            loaded = Some(self.store.load(now_wall));
            let needs_startup_edge = self
                .store
                .transitions()
                .last()
                .is_none_or(|t| t.new_state != UserState::Active);
            if needs_startup_edge {
                self.push_clamped(UserState::Inactive, UserState::Active, now_wall);
            }
            self.clock_offset = now_wall - now_mono;
            self.last_poll = Some((now_wall, now_mono));
            events.push(ScreenTimeEvent::StateChanged(TimeLimitsState::Active));
            let stored = self.store.store(now_wall);
            self.update_limit_state(now_wall, &mut events);
            self.update_grayscale(&mut events);
            return ScreenTimePoll {
                events,
                loaded,
                stored,
            };
        }

        // Suspend gap vs. clock step, from the wall/monotonic comparison.
        let mut dirty = false;
        if let Some((last_wall, last_mono)) = self.last_poll {
            let wall_elapsed = now_wall - last_wall;
            let mono_elapsed = now_mono - last_mono;
            if wall_elapsed > 0 && wall_elapsed - mono_elapsed > SUSPEND_GAP_SECONDS {
                // Suspend (or hibernate): the gap counts as inactive,
                // backdated to the last poll, as PreparingForSleep does.
                if self.user_state == UserState::Active {
                    self.push_clamped(UserState::Active, UserState::Inactive, last_wall);
                    self.user_state = UserState::Inactive;
                    dirty = true;
                }
                self.clock_offset = now_wall - now_mono;
            } else {
                let offset = now_wall - now_mono;
                if offset != self.clock_offset {
                    self.adjust_all_times(offset - self.clock_offset);
                    self.clock_offset = offset;
                    dirty = true;
                }
            }
        }
        self.last_poll = Some((now_wall, now_mono));

        let new_state = if active {
            UserState::Active
        } else {
            UserState::Inactive
        };
        if new_state != self.user_state {
            self.push_clamped(self.user_state, new_state, now_wall);
            self.user_state = new_state;
            dirty = true;
        }

        let stored = if dirty {
            self.store.store(now_wall)
        } else {
            Ok(())
        };
        self.update_limit_state(now_wall, &mut events);
        self.update_grayscale(&mut events);
        ScreenTimePoll {
            events,
            loaded,
            stored,
        }
    }

    /// Background samples stay at most 30 seconds apart, while limit,
    /// upcoming-warning and local-day boundaries get their own earlier wake.
    /// GNOME 51 schedules the remaining active-time limit explicitly in
    /// TimeLimitsManager._updateState; a fixed history poll misses short limits.
    fn next_check_delay(&self, now_wall: i64) -> Duration {
        let mut seconds = SCREEN_TIME_POLL.as_secs();
        if !self.settings.history_enabled {
            return SCREEN_TIME_POLL;
        }
        let (today, tomorrow) = start_of_today(now_wall);
        seconds = seconds.min(tomorrow.saturating_sub(now_wall).max(1) as u64);
        if self.settings.daily_limit_enabled
            && self.state == TimeLimitsState::Active
            && self.user_state == UserState::Active
        {
            let used = active_time_today(self.store.transitions(), now_wall, today);
            let remaining = self.settings.daily_limit_secs.saturating_sub(used);
            seconds = seconds.min(remaining.max(1) as u64);
            let warning_in = remaining.saturating_sub(LIMIT_UPCOMING_NOTIFICATION_SECONDS);
            if warning_in > 0 {
                seconds = seconds.min(warning_in as u64);
            }
        }
        Duration::from_secs(seconds.max(1))
    }

    /// Record the shutdown edge and flush, as `_stopStateMachine` does. The
    /// shell calls this on session exit while history is still enabled.
    pub fn shutdown(&mut self, now_wall: i64) -> io::Result<()> {
        if self.started && self.settings.history_enabled {
            if self.user_state != UserState::Inactive {
                self.push_clamped(UserState::Active, UserState::Inactive, now_wall);
                self.user_state = UserState::Inactive;
            }
            self.store.store(now_wall)?;
        }
        self.started = false;
        Ok(())
    }

    /// Push a transition, clamped to keep wall times non-decreasing so the
    /// file stays valid when the clock stepped backwards while offline.
    fn push_clamped(&mut self, old: UserState, new: UserState, wall: i64) {
        let wall = self
            .store
            .transitions()
            .last()
            .map(|t| wall.max(t.wall_time_secs))
            .unwrap_or(wall);
        self.store.push(Transition {
            old_state: old,
            new_state: new,
            wall_time_secs: wall,
        });
    }

    /// Shift every stored wall time after a clock step (`_adjustAllTimes`).
    fn adjust_all_times(&mut self, delta: i64) {
        for t in self.store.transitions.iter_mut() {
            t.wall_time_secs += delta;
        }
        if self.last_state_change != 0 {
            self.last_state_change += delta;
        }
    }

    /// GNOME `_updateState`, wellbeing branch (no parental controls).
    fn update_limit_state(&mut self, now_wall: i64, events: &mut Vec<ScreenTimeEvent>) {
        let (start_today, _) = start_of_today(now_wall);
        let mut new_state = self.state;
        if start_today > self.last_state_change {
            new_state = TimeLimitsState::Active;
        }
        let active_today = active_time_today(self.store.transitions(), now_wall, start_today);
        let limit_on = self.settings.daily_limit_enabled;
        let limit = self.settings.daily_limit_secs;
        if limit_on && active_today >= limit {
            new_state = TimeLimitsState::LimitReached;
        } else if self.user_state == UserState::Active {
            new_state = TimeLimitsState::Active;
        }
        if new_state != self.state {
            self.state = new_state;
            self.last_state_change = now_wall;
            events.push(ScreenTimeEvent::StateChanged(new_state));
            if new_state == TimeLimitsState::LimitReached {
                self.limit_notice = Some(LimitNotice::Reached);
                events.push(ScreenTimeEvent::LimitReached);
            }
        }
        self.update_limit_notice(limit - active_today, events);
    }

    /// `TimeLimitsNotificationSource._updateState`, `ACTIVE` branch (the
    /// `LIMIT_REACHED` branch is the transition handled above). GNOME's
    /// `dailyLimitTime` is 0 for an inactive user, so nothing changes then.
    fn update_limit_notice(&mut self, remaining: i64, events: &mut Vec<ScreenTimeEvent>) {
        if self.state != TimeLimitsState::Active {
            return;
        }
        if !self.settings.daily_limit_enabled {
            self.clear_limit_notice(events);
            return;
        }
        if self.user_state != UserState::Active {
            return;
        }
        if remaining > LIMIT_UPCOMING_NOTIFICATION_SECONDS {
            self.clear_limit_notice(events);
        } else if remaining == LIMIT_UPCOMING_NOTIFICATION_SECONDS
            && self.limit_notice != Some(LimitNotice::Upcoming)
        {
            self.limit_notice = Some(LimitNotice::Upcoming);
            events.push(ScreenTimeEvent::LimitUpcoming {
                remaining_secs: remaining,
            });
        }
    }

    fn clear_limit_notice(&mut self, events: &mut Vec<ScreenTimeEvent>) {
        if self.limit_notice.take().is_some() {
            events.push(ScreenTimeEvent::LimitNoticeCleared);
        }
    }

    fn update_grayscale(&mut self, events: &mut Vec<ScreenTimeEvent>) {
        let want = self.state == TimeLimitsState::LimitReached && self.settings.grayscale;
        if want != self.grayscale_on {
            self.grayscale_on = want;
            events.push(ScreenTimeEvent::GrayscaleChanged(want));
        }
    }
}

// ---------------------------------------------------------------------------
// Break reminders (BreakManager)
// ---------------------------------------------------------------------------

/// A break type with its own child schema (`SUPPORTED_BREAK_TYPES`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BreakType {
    Eyesight,
    Movement,
}

impl BreakType {
    // Inspection API for tests; identifies breaks in selection round-trips.
    #[allow(dead_code)]
    pub fn id(self) -> &'static str {
        match self {
            Self::Eyesight => "eyesight",
            Self::Movement => "movement",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "eyesight" => Some(Self::Eyesight),
            "movement" => Some(Self::Movement),
            _ => None,
        }
    }

    pub fn schema_id(self) -> &'static str {
        match self {
            Self::Eyesight => BREAK_EYESIGHT_SCHEMA,
            Self::Movement => BREAK_MOVEMENT_SCHEMA,
        }
    }

    /// Schema defaults at the 51.0 tag.
    pub fn defaults(self) -> BreakTypeSettings {
        match self {
            Self::Eyesight => BreakTypeSettings {
                interval_secs: 1200,
                duration_secs: 20,
                delay_secs: 180,
                fade_screen: true,
                lock_screen: false,
                play_sound: true,
                notify: true,
                notify_upcoming: false,
                notify_overdue: true,
                countdown: false,
            },
            Self::Movement => BreakTypeSettings {
                interval_secs: 1800,
                duration_secs: 300,
                delay_secs: 180,
                fade_screen: true,
                lock_screen: false,
                play_sound: true,
                notify: true,
                notify_upcoming: true,
                notify_overdue: true,
                countdown: false,
            },
        }
    }
}

/// Snapshot of one `org.gnome.desktop.break-reminders.*` child schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BreakTypeSettings {
    pub interval_secs: i64,
    pub duration_secs: i64,
    pub delay_secs: i64,
    pub fade_screen: bool,
    pub lock_screen: bool,
    pub play_sound: bool,
    pub notify: bool,
    pub notify_upcoming: bool,
    pub notify_overdue: bool,
    pub countdown: bool,
}

impl Default for BreakTypeSettings {
    fn default() -> Self {
        BreakType::Eyesight.defaults()
    }
}

/// `BreakState`, exposed over D-Bus by gnome-shell, so values are fixed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(i32)]
pub enum BreakState {
    #[default]
    Disabled = 0,
    Active = 1,
    Idle = 2,
    InBreak = 3,
    BreakDue = 4,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum IdlePhase {
    #[default]
    Active,
    Idle,
}

/// Manager signals (`break-due`, `break-finished`); `take-break` is raised by
/// [`BreakService::take_break`] instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakSignal {
    Due,
    Finished,
}

/// Break scheduling state machine. Poll with wall time and the compositor
/// idle time in seconds; idle edges at [`MIN_BREAK_LENGTH_SECONDS`] emulate
/// GNOME's idle watch. Settings changes arrive via [`BreakManager::set_selected`]
/// and [`BreakManager::set_break_settings`], then take effect on the next poll.
#[derive(Debug, Default)]
pub struct BreakManager {
    configs: BTreeMap<BreakType, BreakTypeSettings>,
    last_end: BTreeMap<BreakType, i64>,
    state: BreakState,
    current_break: Option<BreakType>,
    current_start: i64,
    idle_phase: IdlePhase,
    idle_start: i64,
}

impl BreakManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn state(&self) -> BreakState {
        self.state
    }

    pub fn current_break_type(&self) -> Option<BreakType> {
        self.current_break
    }

    /// Start of the in-progress break, or 0 unless [`BreakState::InBreak`].
    pub fn current_break_start_time(&self) -> i64 {
        if self.state == BreakState::InBreak {
            self.current_start
        } else {
            0
        }
    }

    /// End of the most recent break, or 0 while a break is in progress.
    // No D-Bus property surface reads this yet; exercised by tests.
    #[allow(dead_code)]
    pub fn last_break_end_time(&self) -> i64 {
        if self.state == BreakState::InBreak {
            return 0;
        }
        self.last_end.values().copied().max().unwrap_or(0).max(0)
    }

    pub fn next_break_due_time(&self, now: i64) -> i64 {
        self.get_next_break_due(now).1
    }

    // Inspection API for tests.
    #[allow(dead_code)]
    pub fn config(&self, break_type: BreakType) -> Option<BreakTypeSettings> {
        self.configs.get(&break_type).copied()
    }

    pub fn should_notify(&self, t: BreakType) -> bool {
        self.configs.get(&t).is_some_and(|c| c.notify)
    }

    pub fn should_notify_upcoming(&self, t: BreakType) -> bool {
        self.configs.get(&t).is_some_and(|c| c.notify_upcoming)
    }

    pub fn should_notify_overdue(&self, t: BreakType) -> bool {
        self.configs.get(&t).is_some_and(|c| c.notify_overdue)
    }

    pub fn should_countdown(&self, t: BreakType) -> bool {
        self.configs.get(&t).is_some_and(|c| c.countdown)
    }

    pub fn should_play_sound(&self, t: BreakType) -> bool {
        self.configs.get(&t).is_some_and(|c| c.play_sound)
    }

    pub fn should_fade_screen(&self, t: BreakType) -> bool {
        self.configs.get(&t).is_some_and(|c| c.fade_screen)
    }

    pub fn should_lock_screen(&self, t: BreakType) -> bool {
        self.configs.get(&t).is_some_and(|c| c.lock_screen)
    }

    pub fn duration_for(&self, t: BreakType) -> i64 {
        self.configs.get(&t).map(|c| c.duration_secs).unwrap_or(0)
    }

    /// Apply the `selected-breaks` list (`_updateSettings`). Newly enabled
    /// types start their interval at `now`; an empty list stops the machine.
    pub fn set_selected(&mut self, selected: &[(BreakType, BreakTypeSettings)], now: i64) {
        if selected.is_empty() {
            *self = Self::new();
            return;
        }
        let mut next_configs = BTreeMap::new();
        let mut next_last_end = BTreeMap::new();
        for (break_type, settings) in selected {
            next_configs.insert(*break_type, *settings);
            let last = self.last_end.get(break_type).copied().unwrap_or(now);
            next_last_end.insert(*break_type, last);
        }
        self.configs = next_configs;
        self.last_end = next_last_end;
        if self.state == BreakState::Disabled {
            self.state = BreakState::Active;
            self.current_break = None;
            self.current_start = 0;
            self.idle_phase = IdlePhase::Active;
            self.idle_start = 0;
        }
    }

    /// Live-update one break type's settings (the `changed` handler path).
    pub fn set_break_settings(&mut self, break_type: BreakType, settings: BreakTypeSettings) {
        if self.configs.contains_key(&break_type) {
            self.configs.insert(break_type, settings);
        }
    }

    /// One poll step: `idle_secs` is the compositor idle time. Returns manager
    /// signals; UI effects are derived by [`BreakDispatcher::update`].
    pub fn poll(&mut self, now: i64, idle_secs: i64) -> Vec<BreakSignal> {
        if self.state == BreakState::Disabled {
            return Vec::new();
        }
        if self.idle_phase == IdlePhase::Active && idle_secs >= MIN_BREAK_LENGTH_SECONDS {
            self.idle_phase = IdlePhase::Idle;
            self.idle_start = now - MIN_BREAK_LENGTH_SECONDS;
        } else if self.idle_phase == IdlePhase::Idle && idle_secs < MIN_BREAK_LENGTH_SECONDS {
            self.idle_phase = IdlePhase::Active;
        }
        self.update(now)
    }

    /// Port of `_updateState` (timers replaced by polling).
    fn update(&mut self, now: i64) -> Vec<BreakSignal> {
        let mut signals = Vec::new();
        let idle_secs = now - self.idle_start;
        if self.idle_phase == IdlePhase::Idle {
            let (due, next_due) = self.get_next_break_due(now);
            let mut in_break = false;
            let mut start = 0;
            if due.is_some() && next_due <= now {
                let duration = due.map(|t| self.duration_for(t)).unwrap_or(0);
                if next_due + duration <= now {
                    if self.state == BreakState::InBreak {
                        signals.push(BreakSignal::Finished);
                    }
                } else {
                    in_break = true;
                    start = next_due;
                }
            }
            let new_state = if in_break {
                BreakState::InBreak
            } else {
                BreakState::Idle
            };
            if self.state != new_state {
                self.state = new_state;
                self.current_break = due;
                self.current_start = start;
            }
        } else {
            if self.idle_start > 0 {
                // Idle long enough to have taken a break counts as taking it.
                for (break_type, cfg) in &self.configs {
                    if idle_secs >= cfg.duration_secs {
                        self.last_end.insert(*break_type, now);
                    }
                }
            }
            self.idle_start = 0;
            let (due, next_due) = self.get_next_break_due(now);
            let is_due = due.is_some() && next_due <= now;
            if is_due {
                if self.state == BreakState::Active {
                    self.state = BreakState::BreakDue;
                    self.current_break = due;
                    self.current_start = next_due;
                    signals.push(BreakSignal::Due);
                }
            } else if self.state != BreakState::Active {
                self.state = BreakState::Active;
                self.current_break = None;
                self.current_start = 0;
            }
        }
        signals
    }

    /// Port of `getNextBreakDue`: the due (or next-due) break type and its due
    /// time. Among simultaneously due types the longest break wins; otherwise
    /// the earliest due time wins. Zero with `None` when nothing is enabled.
    pub fn get_next_break_due(&self, from: i64) -> (Option<BreakType>, i64) {
        let mut max_duration = 0i64;
        let mut winner: Option<BreakType> = None;
        let mut next_due = 0i64;
        for (break_type, cfg) in &self.configs {
            let last = self.last_end.get(break_type).copied().unwrap_or(from);
            let due_at = last + cfg.interval_secs;
            if due_at <= from {
                if max_duration == 0 || cfg.duration_secs > max_duration {
                    max_duration = cfg.duration_secs;
                    winner = Some(*break_type);
                }
                next_due = due_at;
            } else if next_due == 0 || next_due > due_at {
                next_due = due_at;
                winner = Some(*break_type);
            }
        }
        (winner, next_due)
    }

    /// Delay every currently due break by its `delay-seconds`.
    // Pending notification actions (notify action support); used by tests.
    #[allow(dead_code)]
    pub fn delay_break(&mut self, now: i64) {
        if self.state == BreakState::Disabled {
            return;
        }
        for (break_type, cfg) in &self.configs {
            let last = self.last_end.get(break_type).copied().unwrap_or(now);
            if last + cfg.interval_secs <= now {
                self.last_end.insert(*break_type, last + cfg.delay_secs);
            }
        }
        let _ = self.update(now);
    }

    /// Skip every currently due break (their intervals restart at `now`).
    // Pending notification actions (notify action support); used by tests.
    #[allow(dead_code)]
    pub fn skip_break(&mut self, now: i64) {
        if self.state == BreakState::Disabled {
            return;
        }
        for (break_type, cfg) in &self.configs {
            let last = self.last_end.get(break_type).copied().unwrap_or(now);
            if last + cfg.interval_secs <= now {
                self.last_end.insert(*break_type, now);
            }
        }
        let _ = self.update(now);
    }
}

// ---------------------------------------------------------------------------
// Break dispatcher (BreakDispatcher + BreakNotificationSource)
// ---------------------------------------------------------------------------

/// A resident wellbeing notification. Titles and bodies are GNOME 51's exact
/// strings; `urgent` is HIGH when the break type's `notify` toggle is on and
/// LOW (tray only) otherwise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub title: String,
    pub body: String,
    pub urgent: bool,
    pub allow_delay: bool,
    pub allow_skip: bool,
    pub allow_take: bool,
}

/// What the shell must do after a [`BreakService::poll`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BreakUiEvent {
    StateChanged(BreakState),
    /// Show the single resident break notification, updating it in place.
    Notify(Notice),
    /// Remove it (GNOME's `destroy(SOURCE_CLOSED)`: break complete, break
    /// over, or reminders disabled).
    DismissNotice,
    /// Show the prominent "Break in N seconds" countdown until due.
    StartCountdown {
        break_type: BreakType,
        due_in_secs: i64,
    },
    StopCountdown,
    /// Desired fade-screen state; applying it needs the compositor hook.
    FadeScreen(bool),
    LockScreen,
    /// The break-complete sound (`complete.oga` in GNOME).
    PlayCompleteSound,
}

/// Format a span as "(text, plural value)", ported from `_formatTimeSpan`
/// with integer floors (see the module docs for the deliberate plural
/// deviation from GNOME's float counts).
pub fn format_time_span(secs: i64) -> (String, i64) {
    let secs = secs.max(0);
    if secs < 60 {
        (plural(secs, "second"), secs)
    } else if secs < 3600 {
        let m = secs / 60;
        (plural(m, "minute"), m)
    } else if secs < 86400 {
        let h = secs / 3600;
        (plural(h, "hour"), h)
    } else {
        let d = secs / 86400;
        (plural(d, "day"), d)
    }
}

fn plural(n: i64, unit: &str) -> String {
    if n == 1 {
        format!("{n} {unit}")
    } else {
        format!("{n} {unit}s")
    }
}

/// "Screen Time Limit in 10 Minutes" / "Your screen time limit is approaching".
pub fn limit_upcoming_notice() -> Notice {
    let minutes = LIMIT_UPCOMING_NOTIFICATION_SECONDS / 60;
    Notice {
        title: format!("Screen Time Limit in {}", plural(minutes, "Minute")),
        body: "Your screen time limit is approaching".into(),
        urgent: true,
        allow_delay: false,
        allow_skip: false,
        allow_take: false,
    }
}

/// "Screen Time Limit Reached" / "It's time to stop using the device".
pub fn limit_reached_notice() -> Notice {
    Notice {
        title: "Screen Time Limit Reached".into(),
        body: "It's time to stop using the device".into(),
        urgent: true,
        allow_delay: false,
        allow_skip: false,
        allow_take: false,
    }
}

fn break_notice(
    title: &str,
    body: String,
    urgent: bool,
    allow_delay: bool,
    allow_skip: bool,
    allow_take: bool,
) -> Notice {
    Notice {
        title: title.into(),
        body,
        urgent,
        allow_delay,
        allow_skip,
        allow_take,
    }
}

/// Upcoming-break notice ("Movement/Eyesight Break Soon").
pub fn upcoming_notice(break_type: BreakType, remaining_secs: i64, urgent: bool) -> Notice {
    let (span, _) = format_time_span(remaining_secs);
    let title = match break_type {
        BreakType::Movement => "Movement Break Soon",
        BreakType::Eyesight => "Eyesight Break Soon",
    };
    break_notice(
        title,
        format!("It will be time for a break in {span}"),
        urgent,
        true,
        true,
        false,
    )
}

/// Due-break notice ("Time for a Movement/an Eyesight Break").
pub fn due_notice(break_type: BreakType, duration_secs: i64, urgent: bool) -> Notice {
    let (span, _) = format_time_span(duration_secs);
    let (title, body) = match break_type {
        BreakType::Movement => (
            "Time for a Movement Break",
            format!("Take a break from the device and move around for {span}"),
        ),
        BreakType::Eyesight => (
            "Time for an Eyesight Break",
            format!(
                "Take a break from the screen. Look at least 6 meters away for at least {span}"
            ),
        ),
    };
    break_notice(title, body, urgent, true, false, true)
}

/// Overdue-break notice ("Break Overdue").
pub fn overdue_notice(due_ago_secs: i64, urgent: bool) -> Notice {
    let (span, _) = format_time_span(due_ago_secs);
    break_notice(
        "Break Overdue",
        format!("You were due to take a break {span} ago"),
        urgent,
        true,
        false,
        true,
    )
}

/// In-progress notice ("Movement/Eyesight Break in Progress").
pub fn in_progress_notice(break_type: BreakType, remaining_secs: i64, urgent: bool) -> Notice {
    let (span, _) = format_time_span(remaining_secs);
    let (title, body) = match break_type {
        BreakType::Movement => (
            "Movement Break in Progress",
            format!("Continue moving around for {span}"),
        ),
        BreakType::Eyesight => (
            "Eyesight Break in Progress",
            format!("Continue looking away for {span}"),
        ),
    };
    break_notice(title, body, urgent, false, false, false)
}

/// Interrupted-break notice ("Break Interrupted").
pub fn interrupted_notice(remaining_secs: i64, urgent: bool) -> Notice {
    let (span, value) = format_time_span(remaining_secs);
    let body = if value == 1 {
        format!("There is {span} remaining in your break")
    } else {
        format!("There are {span} remaining in your break")
    };
    break_notice("Break Interrupted", body, urgent, false, false, false)
}

/// Countdown label ("Break in N second(s)").
pub fn countdown_text(due_in_secs: i64) -> String {
    format!("Break in {}", plural(due_in_secs.max(1), "second"))
}

/// Poll-driven port of GNOME's `BreakDispatcher` plus `BreakNotificationSource`.
/// GNOME arms exact timers; the same branches run here on every poll, with
/// fired-markers so each notice fires once per cycle and refreshes only when
/// its text changes.
#[derive(Debug, Default)]
pub struct BreakDispatcher {
    prev_state: BreakState,
    prev_type: Option<BreakType>,
    upcoming_fired: Option<(BreakType, i64)>,
    due_fired: Option<(BreakType, i64)>,
    overdue_fired: bool,
    interrupted_fired: bool,
    countdown_for: Option<(BreakType, i64)>,
    notice_shown: bool,
    last_body: Option<String>,
    faded: bool,
}

impl BreakDispatcher {
    pub fn new() -> Self {
        Self::default()
    }

    /// Derive UI events from the manager after [`BreakManager::poll`].
    pub fn update(&mut self, manager: &BreakManager, now: i64) -> Vec<BreakUiEvent> {
        let mut events = Vec::new();
        let state = manager.state();
        if state != self.prev_state {
            events.push(BreakUiEvent::StateChanged(state));
        }
        match state {
            BreakState::Disabled => {
                self.reset(&mut events);
            }
            BreakState::Active => {
                if self.prev_state == BreakState::InBreak {
                    self.break_complete(manager, &mut events);
                }
                self.unfade(&mut events);
                let (next_type, next_due) = manager.get_next_break_due(now);
                if let Some(t) = next_type {
                    let remaining = next_due - now;
                    if manager.should_notify_upcoming(t)
                        && remaining > 0
                        && self.upcoming_due(remaining)
                        && self.upcoming_fired != Some((t, next_due))
                    {
                        self.upcoming_fired = Some((t, next_due));
                        let urgent = manager.should_notify(t);
                        self.show(upcoming_notice(t, remaining, urgent), &mut events);
                    }
                    self.update_countdown(manager, t, next_due, now, &mut events);
                } else {
                    self.stop_countdown(&mut events);
                }
            }
            BreakState::Idle => {
                if self.prev_state == BreakState::InBreak {
                    self.break_complete(manager, &mut events);
                }
                self.unfade(&mut events);
                self.stop_countdown(&mut events);
            }
            BreakState::InBreak => {
                if let Some(t) = manager.current_break_type() {
                    if self.prev_state != BreakState::InBreak {
                        if manager.should_lock_screen(t) {
                            events.push(BreakUiEvent::LockScreen);
                        } else if manager.should_fade_screen(t) && !self.faded {
                            self.faded = true;
                            events.push(BreakUiEvent::FadeScreen(true));
                        }
                    }
                    let elapsed = now - manager.current_break_start_time();
                    let remaining = manager.duration_for(t) - elapsed;
                    if remaining >= 1 {
                        let urgent = manager.should_notify(t);
                        let notice = in_progress_notice(t, remaining, urgent);
                        if self.last_body.as_deref() != Some(notice.body.as_str()) {
                            self.show(notice, &mut events);
                        }
                    } else if self.notice_shown {
                        self.dismiss(&mut events);
                    }
                }
                self.stop_countdown(&mut events);
            }
            BreakState::BreakDue => {
                self.stop_countdown(&mut events);
                let (next_type, next_due) = manager.get_next_break_due(now);
                if let Some(t) = next_type {
                    let urgent = manager.should_notify(t);
                    let due_ago = (now - next_due).max(0);
                    let fresh = self.prev_state == BreakState::Active
                        || self.prev_state == BreakState::Disabled;
                    if fresh && due_ago < BREAK_OVERDUE_SECONDS {
                        if self.due_fired != Some((t, next_due)) {
                            self.due_fired = Some((t, next_due));
                            self.show(due_notice(t, manager.duration_for(t), urgent), &mut events);
                        }
                    } else if due_ago >= BREAK_OVERDUE_SECONDS && manager.should_notify_overdue(t) {
                        let notice = overdue_notice(due_ago, urgent);
                        if !self.overdue_fired
                            || self.last_body.as_deref() != Some(notice.body.as_str())
                        {
                            self.overdue_fired = true;
                            self.show(notice, &mut events);
                        }
                    } else if self.prev_state == BreakState::InBreak && !self.interrupted_fired {
                        self.interrupted_fired = true;
                        let remaining = manager.duration_for(t) - due_ago;
                        self.show(interrupted_notice(remaining, urgent), &mut events);
                    }
                }
            }
        }
        // Leaving due/dismissing cycles resets the one-shot markers.
        if state == BreakState::Active || state == BreakState::Idle {
            self.due_fired = None;
            self.overdue_fired = false;
            self.interrupted_fired = false;
        }
        if state != BreakState::Active {
            self.upcoming_fired = None;
        }
        self.prev_state = state;
        self.prev_type = manager.current_break_type();
        events
    }

    /// The user pressed Take (or the notification action): lock or fade now.
    // Pending notification actions (notify action support); used by tests.
    #[allow(dead_code)]
    pub fn take_break(&mut self, manager: &BreakManager) -> Vec<BreakUiEvent> {
        let mut events = Vec::new();
        if let Some(t) = manager.current_break_type() {
            if manager.should_lock_screen(t) {
                events.push(BreakUiEvent::LockScreen);
            } else if manager.should_fade_screen(t) && !self.faded {
                self.faded = true;
                events.push(BreakUiEvent::FadeScreen(true));
            }
        }
        events
    }

    fn upcoming_due(&self, remaining: i64) -> bool {
        BREAK_UPCOMING_NOTIFICATION_SECONDS
            .iter()
            .any(|t| remaining <= *t)
    }

    fn update_countdown(
        &mut self,
        manager: &BreakManager,
        break_type: BreakType,
        next_due: i64,
        now: i64,
        events: &mut Vec<BreakUiEvent>,
    ) {
        let due_in = next_due - now;
        let want =
            manager.should_countdown(break_type) && due_in > 0 && due_in <= BREAK_COUNTDOWN_SECONDS;
        match self.countdown_for {
            Some((t, due)) if t == break_type && due == next_due => {
                if !want {
                    self.stop_countdown(events);
                }
            }
            _ => {
                self.stop_countdown(events);
                if want {
                    self.countdown_for = Some((break_type, next_due));
                    events.push(BreakUiEvent::StartCountdown {
                        break_type,
                        due_in_secs: due_in,
                    });
                }
            }
        }
    }

    fn stop_countdown(&mut self, events: &mut Vec<BreakUiEvent>) {
        if self.countdown_for.take().is_some() {
            events.push(BreakUiEvent::StopCountdown);
        }
    }

    fn break_complete(&mut self, manager: &BreakManager, events: &mut Vec<BreakUiEvent>) {
        if self.notice_shown {
            self.dismiss(events);
        }
        if let Some(t) = self.prev_type {
            if manager.should_play_sound(t) {
                events.push(BreakUiEvent::PlayCompleteSound);
            }
        }
    }

    fn unfade(&mut self, events: &mut Vec<BreakUiEvent>) {
        if self.faded {
            self.faded = false;
            events.push(BreakUiEvent::FadeScreen(false));
        }
    }

    fn show(&mut self, notice: Notice, events: &mut Vec<BreakUiEvent>) {
        self.notice_shown = true;
        self.last_body = Some(notice.body.clone());
        events.push(BreakUiEvent::Notify(notice));
    }

    fn dismiss(&mut self, events: &mut Vec<BreakUiEvent>) {
        self.notice_shown = false;
        self.last_body = None;
        events.push(BreakUiEvent::DismissNotice);
    }

    fn reset(&mut self, events: &mut Vec<BreakUiEvent>) {
        if self.notice_shown {
            self.dismiss(events);
        }
        self.unfade(events);
        self.stop_countdown(events);
        self.upcoming_fired = None;
        self.due_fired = None;
        self.overdue_fired = false;
        self.interrupted_fired = false;
        self.prev_type = None;
    }
}

/// Manager plus dispatcher as the shell drives them: one [`BreakService::poll`]
/// per timer tick, with delay/skip/take wired to notification actions.
#[derive(Debug, Default)]
pub struct BreakService {
    pub manager: BreakManager,
    dispatcher: BreakDispatcher,
}

impl BreakService {
    pub fn new() -> Self {
        Self {
            manager: BreakManager::new(),
            dispatcher: BreakDispatcher::new(),
        }
    }

    pub fn poll(&mut self, now: i64, idle_secs: i64) -> Vec<BreakUiEvent> {
        let _ = self.manager.poll(now, idle_secs);
        self.dispatcher.update(&self.manager, now)
    }

    // Pending notification actions (notify action support); used by tests.
    #[allow(dead_code)]
    pub fn delay_break(&mut self, now: i64) -> Vec<BreakUiEvent> {
        self.manager.delay_break(now);
        self.dispatcher.update(&self.manager, now)
    }

    // Pending notification actions (notify action support); used by tests.
    #[allow(dead_code)]
    pub fn skip_break(&mut self, now: i64) -> Vec<BreakUiEvent> {
        self.manager.skip_break(now);
        self.dispatcher.update(&self.manager, now)
    }

    /// The user chose Take: ask them to go idle (lock or fade, per toggles).
    // Pending notification actions (notify action support); used by tests.
    #[allow(dead_code)]
    pub fn take_break(&mut self) -> Vec<BreakUiEvent> {
        self.dispatcher.take_break(&self.manager)
    }
}

// ---------------------------------------------------------------------------
// GSettings, idle state and clocks
// ---------------------------------------------------------------------------

/// Whether a settings schema is installed. The shell keeps working with
/// schema defaults when one is missing (older `gsettings-desktop-schemas`).
pub fn settings_schema_available(id: &str) -> bool {
    gio::SettingsSchemaSource::default()
        .and_then(|source| source.lookup(id, true))
        .is_some()
}

/// Open a schema, or `None` when it is not installed.
pub fn open_settings(id: &str) -> Option<gio::Settings> {
    if settings_schema_available(id) {
        Some(gio::Settings::new(id))
    } else {
        None
    }
}

fn clamped_uint(settings: &gio::Settings, key: &str) -> i64 {
    // GSettings already enforces each key's range (`min="10"`); the floor only
    // guards against a zero interval/duration reaching the schedulers.
    i64::from(settings.uint(key)).max(1)
}

/// Read screen-time settings from an open `org.gnome.desktop.screen-time-limits`.
pub fn read_screen_time_settings_from(settings: &gio::Settings) -> ScreenTimeSettings {
    ScreenTimeSettings {
        history_enabled: settings.boolean("history-enabled"),
        daily_limit_enabled: settings.boolean("daily-limit-enabled"),
        daily_limit_secs: i64::from(settings.uint("daily-limit-seconds")).max(1),
        grayscale: settings.boolean("grayscale"),
    }
}

/// Read one break type's settings from its open child schema.
pub fn read_break_settings_from(settings: &gio::Settings) -> BreakTypeSettings {
    BreakTypeSettings {
        interval_secs: clamped_uint(settings, "interval-seconds"),
        duration_secs: clamped_uint(settings, "duration-seconds"),
        delay_secs: clamped_uint(settings, "delay-seconds"),
        fade_screen: settings.boolean("fade-screen"),
        lock_screen: settings.boolean("lock-screen"),
        play_sound: settings.boolean("play-sound"),
        notify: settings.boolean("notify"),
        notify_upcoming: settings.boolean("notify-upcoming"),
        notify_overdue: settings.boolean("notify-overdue"),
        countdown: settings.boolean("countdown"),
    }
}

/// Read `org.gnome.desktop.screen-time-limits`, or schema defaults when the
/// schema is not installed.
pub fn read_screen_time_settings() -> ScreenTimeSettings {
    open_settings(SCREEN_TIME_SCHEMA)
        .map(|s| read_screen_time_settings_from(&s))
        .unwrap_or_default()
}

/// Read the `selected-breaks` list with each type's settings. Unknown ids are
/// skipped (they have no child schema to read).
pub fn read_selected_breaks() -> Vec<(BreakType, BreakTypeSettings)> {
    let Some(parent) = open_settings(BREAK_SCHEMA) else {
        return Vec::new();
    };
    read_selected_breaks_from(&parent, &|t| {
        open_settings(t.schema_id()).map(|s| read_break_settings_from(&s))
    })
}

fn read_selected_breaks_from(
    parent: &gio::Settings,
    child: &dyn Fn(BreakType) -> Option<BreakTypeSettings>,
) -> Vec<(BreakType, BreakTypeSettings)> {
    parent
        .strv("selected-breaks")
        .iter()
        .filter_map(|id| BreakType::from_id(id))
        .filter_map(|t| child(t).map(|c| (t, c)))
        .collect()
}

/// The session's idle time from `org.gnome.Mutter.IdleMonitor`, or `None`
/// without one. Read-only: `NO_AUTO_START` and a 200 ms timeout, as in
/// `notify.rs`.
pub fn idle_time() -> Option<Duration> {
    let conn = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).ok()?;
    let reply = conn
        .call_sync(
            Some("org.gnome.Mutter.IdleMonitor"),
            "/org/gnome/Mutter/IdleMonitor/Core",
            "org.gnome.Mutter.IdleMonitor",
            "GetIdletime",
            None,
            Some(glib::VariantTy::new("(t)").ok()?),
            gio::DBusCallFlags::NO_AUTO_START,
            200,
            gio::Cancellable::NONE,
        )
        .ok()?;
    reply.get::<(u64,)>().map(|(ms,)| Duration::from_millis(ms))
}

/// The session `idle-delay`: no input for this long means the user is away.
/// Zero disables idle detection (always active), as in GNOME.
pub fn session_idle_delay() -> Duration {
    let secs = open_settings("org.gnome.desktop.session")
        .map(|s| s.uint("idle-delay"))
        .unwrap_or(DEFAULT_SESSION_IDLE_DELAY_SECONDS);
    Duration::from_secs(u64::from(secs))
}

/// Whether the user counts as active: idle below the session delay, or no
/// idle monitor at all (GNOME's banner logic treats that as active too).
pub fn is_user_active(idle: Option<Duration>, delay: Duration) -> bool {
    delay.is_zero() || idle.is_none_or(|i| i < delay)
}

/// Wall clock in Unix seconds.
pub fn wall_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Monotonic clock in seconds on a per-process base, for suspend detection.
#[derive(Debug, Clone)]
pub struct MonoClock {
    base: std::time::Instant,
}

impl MonoClock {
    pub fn start() -> Self {
        Self {
            base: std::time::Instant::now(),
        }
    }

    pub fn now(&self) -> i64 {
        self.base.elapsed().as_secs() as i64
    }
}

// ---------------------------------------------------------------------------
// Live runtime: timers, settings watches and shell dispatch
// ---------------------------------------------------------------------------

/// Post the screen-time notice (title, body, urgent) to the shell's message
/// list, updating the resident one in place while it lives (`Some(id)`,
/// GNOME's `_ensureNotification` + `addNotification`). Returns its id.
/// `urgent` is GNOME's `Urgency.HIGH`, otherwise `Urgency.LOW`.
pub type NoticeHook = Rc<dyn Fn(String, String, bool, Option<u64>) -> Option<u64>>;
/// Remove a resident notice from the list and its banner
/// (GNOME's `notification.destroy()`).
pub type RemoveHook = Rc<dyn Fn(u64)>;
/// Show the break countdown OSD (label, 0..1 progress as due approaches).
pub type CountdownHook = Rc<dyn Fn(String, f64)>;
/// Hide only the OSD display still owned by this countdown.
pub type CountdownHideHook = Rc<dyn Fn()>;
/// Lock the session now.
pub type LockHook = Rc<dyn Fn()>;
/// Post a resident notice WITH invokable actions, updating `Some(id)` in
/// place (text, urgency and actions) while it lives; returns the center id.
/// Actions are (key, label) pairs; only allow-listed keys are posted.
pub type ActionNoticeHook =
    Rc<dyn Fn(String, String, Vec<(String, String)>, bool, Option<u64>) -> Option<u64>>;
/// Register a local callback for one (notice id, action key).
pub type ActionRegisterHook = Rc<dyn Fn(u64, String, Rc<dyn Fn()>)>;
/// Forget one notice's local callbacks (replacement/removal).
pub type ActionDropHook = Rc<dyn Fn(u64)>;
/// One user-chosen break action, queued for the next break tick (the hook
/// must not call back into the runtime; see [`ShellHooks`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakActionKind {
    Take,
    Delay,
    Skip,
}
/// Enqueue one user-chosen break action for the next tick.
pub type BreakActionHook = Rc<dyn Fn(BreakActionKind)>;
/// Send both effect bools atomically to the compositor (grayscale, fade)
/// via `CommandKind::SetWellbeingEffects`. True only after a correlated
/// Applied response in the current connection; false keeps the desired
/// pair pending. The hook bounds retries and outstanding wire commands.
pub type EffectHook = Rc<dyn Fn(bool, bool) -> bool>;

/// Action keys and button labels for break notices. Keys are center action
/// ids; labels are plain Header Case (GNOME 51 ships no verbatim strings
/// for these buttons to match).
pub const ACTION_TAKE: (&str, &str) = ("take", "Take");
pub const ACTION_DELAY: (&str, &str) = ("delay", "Delay");
pub const ACTION_SKIP: (&str, &str) = ("skip", "Skip");

/// Buttons for one break notice, from its allow flags (take, delay, skip
/// order). Only allow-listed keys are ever posted or registered, so a
/// denied key has no callback to reach.
pub fn notice_action_buttons(
    notice: &Notice,
) -> Vec<(&'static str, &'static str, BreakActionKind)> {
    let mut out = Vec::new();
    if notice.allow_take {
        out.push((ACTION_TAKE.0, ACTION_TAKE.1, BreakActionKind::Take));
    }
    if notice.allow_delay {
        out.push((ACTION_DELAY.0, ACTION_DELAY.1, BreakActionKind::Delay));
    }
    if notice.allow_skip {
        out.push((ACTION_SKIP.0, ACTION_SKIP.1, BreakActionKind::Skip));
    }
    out
}

/// Shell surfaces the wellbeing runtime drives. All hooks run on the GTK
/// thread. A hook must not call back into the runtime: the runtime lends
/// surfaces, it never borrows them back.
#[derive(Clone)]
pub struct ShellHooks {
    pub post_notice: NoticeHook,
    pub remove_notice: RemoveHook,
    pub show_countdown: CountdownHook,
    pub hide_countdown: CountdownHideHook,
    pub lock: LockHook,
    pub post_action_notice: ActionNoticeHook,
    pub on_notice_action: ActionRegisterHook,
    pub drop_notice_actions: ActionDropHook,
    pub break_action: BreakActionHook,
    pub effect_hook: EffectHook,
}

/// Reconcile effects on each tick, including an unchanged desired pair after
/// reconnect. The hook owns bounded wire sends and correlated Applied state;
/// a cached pair here never bypasses checking that connection state.
pub fn maybe_send_effect(hooks: &ShellHooks, sent: &mut Option<EffectState>, effect: &EffectState) {
    *sent = (hooks.effect_hook)(effect.grayscale, effect.fade).then_some(*effect);
}

/// Desired effect state. The runtime sends the pair atomically through
/// the effect hook on every change (protocol 0.35
/// `SetWellbeingEffects`), and keeps logging transitions as diagnostics
/// that proof gates read back until pixel proofs land.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EffectState {
    pub grayscale: bool,
    pub fade: bool,
}

/// OSD countdown progress: 0 when the countdown starts, 1 at due.
pub fn countdown_progress(due_in_secs: i64) -> f64 {
    if due_in_secs >= BREAK_COUNTDOWN_SECONDS {
        0.0
    } else if due_in_secs <= 0 {
        1.0
    } else {
        1.0 - due_in_secs as f64 / BREAK_COUNTDOWN_SECONDS as f64
    }
}

fn log_effect(effect: EffectState) {
    eprintln!(
        "tuna-shell-gtk: wellbeing: effect grayscale={} fade={} (gray fades {}s to {}; break fades {}s to {}; compositor Applied correlation active)",
        effect.grayscale,
        effect.fade,
        GRAYSCALE_FADE_SECONDS,
        GRAYSCALE_SATURATION,
        BREAK_FADE_SECONDS,
        BREAK_BRIGHTNESS_FACTOR
    );
}

/// Deliver screen-time events to the shell: limit notices go to the message
/// list, grayscale updates the desired effect state. Like GNOME's
/// `TimeLimitsNotificationSource`, the upcoming and reached notices are one
/// resident notification (`resident`, its center id) updated in place, so
/// "Screen Time Limit Reached" takes over the warning's banner, even one
/// still waiting on screen for an away user, instead of queuing behind it;
/// it is removed outright once the source would destroy it.
pub fn dispatch_screen_time(
    hooks: &ShellHooks,
    effect: &mut EffectState,
    resident: &mut Option<u64>,
    events: &[ScreenTimeEvent],
) {
    for event in events {
        let notice = match event {
            ScreenTimeEvent::StateChanged(state) => {
                eprintln!("tuna-shell-gtk: wellbeing: screen-time state {state:?}");
                continue;
            }
            ScreenTimeEvent::LimitUpcoming { .. } => limit_upcoming_notice(),
            ScreenTimeEvent::LimitReached => limit_reached_notice(),
            ScreenTimeEvent::LimitNoticeCleared => {
                if let Some(id) = resident.take() {
                    eprintln!("tuna-shell-gtk: wellbeing: limit notice {id} removed");
                    (hooks.remove_notice)(id);
                }
                continue;
            }
            ScreenTimeEvent::GrayscaleChanged(on) => {
                effect.grayscale = *on;
                log_effect(*effect);
                continue;
            }
        };
        let replaces = *resident;
        *resident = (hooks.post_notice)(
            notice.title.clone(),
            notice.body.clone(),
            notice.urgent,
            replaces,
        );
        log_notice("limit", &notice, replaces, *resident);
    }
}

/// Proof-gate diagnostics: which resident a notice landed on, and whether
/// it updated the live one in place.
fn log_notice(kind: &str, notice: &Notice, replaces: Option<u64>, posted: Option<u64>) {
    let urgency = if notice.urgent { "high" } else { "low" };
    let how = if replaces.is_some() && replaces == posted {
        "updated"
    } else {
        "posted"
    };
    if let Some(id) = posted {
        eprintln!(
            "tuna-shell-gtk: wellbeing: {kind} notice {id} {how} urgency={urgency}: {}",
            notice.title
        );
    }
}

/// Deliver break events to the shell: notices to the message list, the
/// countdown to the OSD, lock to the session. `countdown` marks the live
/// OSD countdown so the tick can refresh its label from the manager's due
/// time; the OSD hides itself 1.5 s after the last show, so stopping needs
/// no call. `resident` tracks the single resident break notice's center
/// id: like GNOME's `BreakNotificationSource`, a new notice updates it in
/// place (text, urgency and actions; old action callbacks are dropped),
/// and dismissal removes it from the list, as GNOME destroys it.
pub fn dispatch_break_ui(
    hooks: &ShellHooks,
    effect: &mut EffectState,
    countdown: &mut Option<BreakType>,
    resident: &mut Option<u64>,
    events: &[BreakUiEvent],
) {
    for event in events {
        match event {
            BreakUiEvent::StateChanged(state) => {
                eprintln!("tuna-shell-gtk: wellbeing: break state {state:?}");
            }
            BreakUiEvent::Notify(notice) => {
                let replaces = resident.take();
                if let Some(old) = replaces {
                    (hooks.drop_notice_actions)(old);
                }
                let buttons = notice_action_buttons(notice);
                let posted = (hooks.post_action_notice)(
                    notice.title.clone(),
                    notice.body.clone(),
                    buttons
                        .iter()
                        .map(|(key, label, _)| (key.to_string(), label.to_string()))
                        .collect(),
                    notice.urgent,
                    replaces,
                );
                log_notice("break", notice, replaces, posted);
                if let Some(id) = posted {
                    for (key, _, kind) in buttons {
                        let push = hooks.break_action.clone();
                        (hooks.on_notice_action)(id, key.to_string(), Rc::new(move || push(kind)));
                    }
                    *resident = Some(id);
                }
            }
            BreakUiEvent::DismissNotice => {
                if let Some(id) = resident.take() {
                    (hooks.drop_notice_actions)(id);
                    eprintln!("tuna-shell-gtk: wellbeing: break notice {id} removed");
                    (hooks.remove_notice)(id);
                }
            }
            BreakUiEvent::StartCountdown {
                break_type,
                due_in_secs,
            } => {
                *countdown = Some(*break_type);
                (hooks.show_countdown)(
                    countdown_text(*due_in_secs),
                    countdown_progress(*due_in_secs),
                );
            }
            BreakUiEvent::StopCountdown => {
                *countdown = None;
                (hooks.hide_countdown)();
            }
            BreakUiEvent::FadeScreen(on) => {
                effect.fade = *on;
                log_effect(*effect);
            }
            BreakUiEvent::LockScreen => (hooks.lock)(),
            BreakUiEvent::PlayCompleteSound => {
                eprintln!("tuna-shell-gtk: wellbeing: break complete sound");
                play_complete_sound();
            }
        }
    }
}

/// Best-effort break-complete sound (`complete` in the GNOME sound theme),
/// gated on `org.gnome.desktop.sound event-sounds` and spawned with the same
/// guards as volume feedback. Silence on failure: a break ending is not an
/// error worth surfacing.
pub fn play_complete_sound() {
    let Some(sound) = open_settings("org.gnome.desktop.sound") else {
        return;
    };
    if !sound.boolean("event-sounds") {
        return;
    }
    let theme = format!("canberra.xdg-theme.name={}", sound.string("theme-name"));
    let launcher = gio::SubprocessLauncher::new(
        gio::SubprocessFlags::STDOUT_SILENCE | gio::SubprocessFlags::STDERR_SILENCE,
    );
    let parent = std::process::id() as libc::pid_t;
    launcher.set_child_setup(move || unsafe {
        if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0
            || libc::getppid() != parent
        {
            libc::_exit(127);
        }
        // The deadline survives exec and is independent of GTK's loop.
        libc::signal(libc::SIGALRM, libc::SIG_DFL);
        libc::alarm(5);
    });
    let _ = launcher.spawn(&[
        "canberra-gtk-play".as_ref(),
        "--id".as_ref(),
        "complete".as_ref(),
        "--description".as_ref(),
        "Break complete".as_ref(),
        "--property".as_ref(),
        theme.as_ref(),
    ]);
}

/// Live shell runtime. The GLib timeouts and settings watches own the `Rc`,
/// so this lives for the shell process lifetime.
#[cfg(not(test))]
struct Runtime {
    hooks: ShellHooks,
    tracker: ScreenTimeTracker,
    breaks: BreakService,
    mono: MonoClock,
    screen_settings: Option<gio::Settings>,
    break_parent: Option<gio::Settings>,
    break_children: Vec<(BreakType, gio::Settings)>,
    effect: EffectState,
    countdown: Option<BreakType>,
    resident: Option<u64>,
    /// The screen-time notice's center id (see [`dispatch_screen_time`]).
    limit_notice: Option<u64>,
    actions: Rc<RefCell<Vec<BreakActionKind>>>,
    sent_effect: Option<EffectState>,
    screen_timer: Option<glib::SourceId>,
    // The existing proof clock opts into deadline-only diagnostics. These
    // contain no account/history/window data and never change the scheduler.
    proof_diagnostics: bool,
    stopped: bool,
}

/// One screen-time poll: idle verdict from the compositor, history edges and
/// limit state, then dispatch. Hooks run outside the runtime borrow.
#[cfg(not(test))]
fn screen_tick(rt: &Rc<RefCell<Runtime>>) {
    if rt.borrow().stopped {
        return;
    }
    let (hooks, mut effect, mut limit_notice, events) = {
        let mut r = rt.borrow_mut();
        let now_wall = wall_now();
        let mono_now = r.mono.now();
        let active = is_user_active(idle_time(), session_idle_delay());
        let poll = r.tracker.poll(now_wall, mono_now, active);
        if let Err(e) = &poll.stored {
            eprintln!("tuna-shell-gtk: wellbeing: history store failed: {e}");
        }
        if let Some(Err(e)) = &poll.loaded {
            eprintln!("tuna-shell-gtk: wellbeing: {e}");
        }
        (r.hooks.clone(), r.effect, r.limit_notice, poll.events)
    };
    dispatch_screen_time(&hooks, &mut effect, &mut limit_notice, &events);
    let mut r = rt.borrow_mut();
    maybe_send_effect(&hooks, &mut r.sent_effect, &effect);
    r.effect = effect;
    r.limit_notice = limit_notice;
    drop(r);
    schedule_screen_tick(rt);
}

/// A single replaceable source preserves background sampling while waking at
/// exact remaining-limit boundaries. Settings changes invoke screen_tick and
/// replace this source; stale deadlines cannot fire after a policy update.
#[cfg(not(test))]
fn schedule_screen_tick(rt: &Rc<RefCell<Runtime>>) {
    let (delay, old) = {
        let mut r = rt.borrow_mut();
        if r.stopped {
            return;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let whole = r.tracker.next_check_delay(now.as_secs() as i64);
        // Land 50 ms into the target wall second: the tracker counts whole
        // seconds, and GNOME's warning fires only at exactly 600 s left, so
        // a wake that slipped past the boundary must not read 599.
        let into_second = Duration::from_nanos(u64::from(now.subsec_nanos()));
        (
            (whole + Duration::from_millis(50)).saturating_sub(into_second),
            r.screen_timer.take(),
        )
    };
    if let Some(source) = old {
        source.remove();
    }
    let weak = Rc::downgrade(rt);
    let source = glib::timeout_add_local(delay, move || {
        if let Some(rt) = weak.upgrade() {
            // The current source expires on Break; clear its identity before
            // screen_tick installs the next source, without removing it twice.
            let _ = rt.borrow_mut().screen_timer.take();
            screen_tick(&rt);
        }
        glib::ControlFlow::Break
    });
    rt.borrow_mut().screen_timer = Some(source);
}

/// One break poll, plus a countdown label refresh from the manager's due
/// time (the dispatcher only announces the countdown's start and stop).
#[cfg(not(test))]
fn break_tick(rt: &Rc<RefCell<Runtime>>) {
    let now = wall_now();
    let (hooks, mut effect, mut countdown, mut resident, events) = {
        let mut r = rt.borrow_mut();
        let idle_secs = idle_time().map(|d| d.as_secs() as i64).unwrap_or(0);
        let mut events = r.breaks.poll(now, idle_secs);
        // Record the logical deadline when the model emits a due/overdue
        // notice, before queued actions can alter it. Banner presentation and
        // an AT-SPI walk happen later and are not scheduling timestamps.
        if r.proof_diagnostics
            && events
                .iter()
                .any(|event| matches!(event, BreakUiEvent::Notify(notice) if notice.allow_take))
        {
            eprintln!(
                "tuna-shell-gtk: wellbeing: due schedule pid={} now={} idle={} due={}",
                std::process::id(),
                now,
                idle_secs,
                r.breaks.manager.next_break_due_time(now)
            );
        }
        // Queued notice actions apply here, on the tick — never inside a
        // hook — so the model mutates under exactly one borrow and a
        // banner press lands within one 2 s tick. Drained into a local
        // first: holding the queue borrow across model calls would
        // alias it with the service borrow.
        let queued: Vec<BreakActionKind> = r.actions.borrow_mut().drain(..).collect();
        for kind in queued {
            events.extend(match kind {
                BreakActionKind::Take => r.breaks.take_break(),
                BreakActionKind::Delay => r.breaks.delay_break(now),
                BreakActionKind::Skip => {
                    let before = r
                        .proof_diagnostics
                        .then(|| r.breaks.manager.next_break_due_time(now));
                    let events = r.breaks.skip_break(now);
                    if let Some(before) = before {
                        // Both deadlines come from the same manager borrow
                        // and wall-time sample around the actual consumer.
                        eprintln!(
                            "tuna-shell-gtk: wellbeing: skip schedule pid={} now={} idle={} before={} after={}",
                            std::process::id(),
                            now,
                            idle_secs,
                            before,
                            r.breaks.manager.next_break_due_time(now)
                        );
                    }
                    events
                }
            });
        }
        (r.hooks.clone(), r.effect, r.countdown, r.resident, events)
    };
    dispatch_break_ui(&hooks, &mut effect, &mut countdown, &mut resident, &events);
    if countdown.is_some() {
        let due_in = rt.borrow().breaks.manager.next_break_due_time(now) - now;
        if due_in > 0 {
            (hooks.show_countdown)(countdown_text(due_in), countdown_progress(due_in));
        }
    }
    let mut r = rt.borrow_mut();
    maybe_send_effect(&hooks, &mut r.sent_effect, &effect);
    r.effect = effect;
    r.countdown = countdown;
    r.resident = resident;
}

/// Start the live runtime: an immediate first poll, then screen-time boundary
/// timers with a 30 s background cap and the 2 s break timer, live settings watches that re-poll at once (as
/// GNOME's changed handlers re-evaluate instead of waiting for the next
/// tick), and a shutdown flush of the closing history edge.
#[cfg(not(test))]
pub fn start(
    app: &gio::Application,
    hooks: ShellHooks,
    actions: Rc<RefCell<Vec<BreakActionKind>>>,
) {
    let tracker = ScreenTimeTracker::new(
        read_screen_time_settings(),
        HistoryStore::new(history_path()),
    );
    let breaks = BreakService::new();
    let rt = Rc::new(RefCell::new(Runtime {
        hooks,
        tracker,
        breaks,
        mono: MonoClock::start(),
        screen_settings: open_settings(SCREEN_TIME_SCHEMA),
        break_parent: open_settings(BREAK_SCHEMA),
        break_children: SUPPORTED_BREAK_TYPES
            .iter()
            .copied()
            .filter_map(BreakType::from_id)
            .filter_map(|t| open_settings(t.schema_id()).map(|s| (t, s)))
            .collect(),
        effect: EffectState::default(),
        countdown: None,
        resident: None,
        limit_notice: None,
        actions,
        sent_effect: None,
        screen_timer: None,
        proof_diagnostics: std::env::var_os("TUNA_ANIMATION_CLOCK").is_some(),
        stopped: false,
    }));
    // Baseline first: the compositor starts from neutral even if a limit
    // is already reached (the first polls then send the actual pair).
    (rt.borrow().hooks.effect_hook)(false, false);
    {
        let rt = rt.clone();
        glib::timeout_add_local(BREAK_POLL, move || {
            break_tick(&rt);
            glib::ControlFlow::Continue
        });
    }
    if let Some(settings) = rt.borrow().screen_settings.clone() {
        let rt = rt.clone();
        settings.connect_changed(None, move |settings, _| {
            rt.borrow_mut()
                .tracker
                .set_settings(read_screen_time_settings_from(settings));
            screen_tick(&rt);
        });
    }
    if let Some(parent) = rt.borrow().break_parent.clone() {
        let rt = rt.clone();
        parent.connect_changed(Some("selected-breaks"), move |_, _| {
            let now = wall_now();
            rt.borrow_mut()
                .breaks
                .manager
                .set_selected(&read_selected_breaks(), now);
            break_tick(&rt);
        });
    }
    for (break_type, child) in rt.borrow().break_children.clone() {
        let rt = rt.clone();
        child.connect_changed(None, move |settings, _| {
            rt.borrow_mut()
                .breaks
                .manager
                .set_break_settings(break_type, read_break_settings_from(settings));
            break_tick(&rt);
        });
    }
    // Read the live values only after every retained subscription is
    // connected: a write landing before the connect emits nothing we could
    // observe, so an earlier read would keep a stale selection for good.
    {
        let mut r = rt.borrow_mut();
        if let Some(settings) = r.screen_settings.clone() {
            r.tracker
                .set_settings(read_screen_time_settings_from(&settings));
        }
        r.breaks
            .manager
            .set_selected(&read_selected_breaks(), wall_now());
    }
    // The first polls run now so the startup edge, the limit state and any
    // due break apply immediately instead of a tick late.
    screen_tick(&rt);
    break_tick(&rt);
    {
        let rt = rt.clone();
        app.connect_shutdown(move |_| {
            let mut r = rt.borrow_mut();
            r.stopped = true;
            if let Some(source) = r.screen_timer.take() {
                source.remove();
            }
            if let Err(e) = r.tracker.shutdown(wall_now()) {
                eprintln!("tuna-shell-gtk: wellbeing: shutdown flush failed: {e}");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    const T0: i64 = 1_760_000_000; // fixed wall clock for scripted days

    fn tr(old: UserState, new: UserState, wall: i64) -> Transition {
        Transition {
            old_state: old,
            new_state: new,
            wall_time_secs: wall,
        }
    }

    #[test]
    fn history_round_trip_matches_gnome_field_names() {
        let ts = vec![
            tr(UserState::Inactive, UserState::Active, T0),
            tr(UserState::Active, UserState::Inactive, T0 + 60),
        ];
        let text = render_history(&ts);
        assert!(text.contains("\"oldState\""), "{text}");
        assert!(text.contains("\"newState\""), "{text}");
        assert!(text.contains("\"wallTimeSecs\""), "{text}");
        assert_eq!(parse_history(&text).unwrap(), ts);
    }

    #[test]
    fn history_rejects_malformed_entries() {
        for (name, body) in [
            ("not json", "nope"),
            ("not array", "{}"),
            ("not object", "[1]"),
            (
                "bad state",
                r#"[{"oldState":2,"newState":1,"wallTimeSecs":5}]"#,
            ),
            (
                "equal states",
                r#"[{"oldState":1,"newState":1,"wallTimeSecs":5}]"#,
            ),
            ("missing key", r#"[{"oldState":0,"newState":1}]"#),
            (
                "float time",
                r#"[{"oldState":0,"newState":1,"wallTimeSecs":1.5}]"#,
            ),
            (
                "decreasing time",
                r#"[{"oldState":0,"newState":1,"wallTimeSecs":9},{"oldState":1,"newState":0,"wallTimeSecs":8}]"#,
            ),
        ] {
            assert!(
                matches!(parse_history(body), Err(HistoryError::Corrupt(_))),
                "{name}"
            );
        }
    }

    #[test]
    fn retention_drops_future_and_ancient_entries_but_keeps_newest() {
        let now = T0;
        let ancient = tr(
            UserState::Inactive,
            UserState::Active,
            now - HISTORY_THRESHOLD_SECONDS - 1,
        );
        let recent = tr(UserState::Active, UserState::Inactive, now - 10);
        let future = tr(UserState::Inactive, UserState::Active, now + 60);
        assert!(!keep_transition(&ancient, 0, 3, now));
        assert!(keep_transition(&recent, 1, 3, now));
        assert!(!keep_transition(&future, 2, 3, now));
        // The newest entry survives even when ancient: it seeds the open interval.
        assert!(keep_transition(&ancient, 0, 1, now));
    }

    #[test]
    fn equal_time_cancelling_pairs_collapse_like_gnome() {
        // A mid-history on/off toggle within one second drops the first edge.
        let mut ts = vec![
            tr(UserState::Inactive, UserState::Active, T0 - 100),
            tr(UserState::Active, UserState::Inactive, T0),
            tr(UserState::Inactive, UserState::Active, T0),
            tr(UserState::Active, UserState::Inactive, T0 + 5),
        ];
        collapse_equal_time_pairs(&mut ts);
        assert_eq!(
            ts,
            vec![
                tr(UserState::Inactive, UserState::Active, T0 - 100),
                tr(UserState::Inactive, UserState::Active, T0),
                tr(UserState::Active, UserState::Inactive, T0 + 5),
            ]
        );
        // A leading close left behind by the collapse is dropped: it would
        // otherwise count the whole day as active.
        let mut ts = vec![
            tr(UserState::Inactive, UserState::Active, T0),
            tr(UserState::Active, UserState::Inactive, T0),
            tr(UserState::Inactive, UserState::Active, T0 + 600),
        ];
        collapse_equal_time_pairs(&mut ts);
        assert_eq!(
            ts,
            vec![tr(UserState::Inactive, UserState::Active, T0 + 600)]
        );
    }

    #[test]
    fn day_boundary_is_3am_local() {
        let tz = jiff::tz::TimeZone::UTC;
        // 2026-10-10 02:00 UTC belongs to the day starting 2026-10-09 03:00.
        let early = jiff::civil::date(2026, 10, 10)
            .at(2, 0, 0, 0)
            .to_zoned(tz.clone())
            .unwrap()
            .timestamp()
            .as_second();
        let (today, tomorrow) = start_of_today_in(&tz, early);
        let expect_today = jiff::civil::date(2026, 10, 9)
            .at(3, 0, 0, 0)
            .to_zoned(tz.clone())
            .unwrap()
            .timestamp()
            .as_second();
        let expect_tomorrow = jiff::civil::date(2026, 10, 10)
            .at(3, 0, 0, 0)
            .to_zoned(tz.clone())
            .unwrap()
            .timestamp()
            .as_second();
        assert_eq!((today, tomorrow), (expect_today, expect_tomorrow));
        // Noon belongs to the same civil day's 03:00 start.
        let noon = jiff::civil::date(2026, 10, 10)
            .at(12, 0, 0, 0)
            .to_zoned(tz.clone())
            .unwrap()
            .timestamp()
            .as_second();
        assert_eq!(start_of_today_in(&tz, noon).0, expect_tomorrow);
    }

    #[test]
    fn active_time_counts_open_and_partial_intervals() {
        // Active [T0-100, T0-40], then open from T0-10; day starts at T0-50.
        let ts = vec![
            tr(UserState::Inactive, UserState::Active, T0 - 100),
            tr(UserState::Active, UserState::Inactive, T0 - 40),
            tr(UserState::Inactive, UserState::Active, T0 - 10),
        ];
        // Closed slice today: T0-50..T0-40 = 10; open slice: T0-10..T0 = 10.
        assert_eq!(active_time_today(&ts, T0, T0 - 50), 20);
        // A first transition ending a pre-dawn interval counts from the boundary.
        let ts = vec![tr(UserState::Active, UserState::Inactive, T0 - 30)];
        assert_eq!(active_time_today(&ts, T0, T0 - 50), 20);
        assert_eq!(active_time_today(&[], T0, T0 - 50), 0);
    }

    #[test]
    fn limit_reached_at_pins_the_crossing_second() {
        let ts = vec![
            tr(UserState::Inactive, UserState::Active, T0 - 100),
            tr(UserState::Active, UserState::Inactive, T0 - 40),
            tr(UserState::Inactive, UserState::Active, T0 - 10),
        ];
        // 10 s closed today + open from T0-10: a 15 s limit hits at T0-5.
        assert_eq!(limit_reached_at(&ts, T0, 15, T0 - 50), T0 - 5);
        assert_eq!(limit_reached_at(&ts, T0, 10_000, T0 - 50), 0);
    }

    fn tracker_in(dir: &std::path::Path) -> ScreenTimeTracker {
        ScreenTimeTracker::new(
            ScreenTimeSettings::default(),
            HistoryStore::new(dir.join("session-active-history.json")),
        )
    }

    #[test]
    fn tracker_records_startup_idle_and_shutdown_edges() {
        let dir = tempfile::tempdir().unwrap();
        let mut tracker = tracker_in(dir.path());
        let poll = tracker.poll(T0, 1000, true);
        assert!(poll.stored.is_ok());
        assert_eq!(tracker.state(), TimeLimitsState::Active);
        // Idle edge then back to active.
        tracker.poll(T0 + 30, 1030, false);
        tracker.poll(T0 + 60, 1060, true);
        let ts = tracker.store().transitions().to_vec();
        assert_eq!(
            ts,
            vec![
                tr(UserState::Inactive, UserState::Active, T0),
                tr(UserState::Active, UserState::Inactive, T0 + 30),
                tr(UserState::Inactive, UserState::Active, T0 + 60),
            ]
        );
        tracker.shutdown(T0 + 90).unwrap();
        let ts = tracker.store().transitions().to_vec();
        assert_eq!(ts.last().unwrap().new_state, UserState::Inactive);
        // The flushed file parses back identically (restart persistence).
        let text = std::fs::read_to_string(dir.path().join("session-active-history.json")).unwrap();
        assert_eq!(parse_history(&text).unwrap(), ts);
    }

    #[test]
    fn disabling_history_deletes_the_file_and_stops_recording() {
        let dir = tempfile::tempdir().unwrap();
        let mut tracker = tracker_in(dir.path());
        tracker.poll(T0, 1000, true);
        assert!(dir.path().join("session-active-history.json").exists());
        tracker.set_settings(ScreenTimeSettings {
            history_enabled: false,
            ..Default::default()
        });
        let poll = tracker.poll(T0 + 30, 1030, true);
        assert!(poll.stored.is_ok());
        assert_eq!(tracker.state(), TimeLimitsState::Disabled);
        assert!(!dir.path().join("session-active-history.json").exists());
        assert!(tracker.store().transitions().is_empty());
        assert!(poll
            .events
            .contains(&ScreenTimeEvent::StateChanged(TimeLimitsState::Disabled)));
    }

    #[test]
    fn suspend_gap_is_backdated_to_inactive() {
        let dir = tempfile::tempdir().unwrap();
        let mut tracker = tracker_in(dir.path());
        tracker.poll(T0, 1000, true);
        // Wall jumps 10 min with no monotonic advance: suspend.
        tracker.poll(T0 + 600, 1030, true);
        assert_eq!(tracker.user_state(), UserState::Active);
        // The suspend lands in the startup second, so the startup edge and
        // the backdated close cancel out; only the resume edge survives.
        assert_eq!(
            tracker.store().transitions(),
            &[tr(UserState::Inactive, UserState::Active, T0 + 600)]
        );
    }

    #[test]
    fn small_clock_step_adjusts_stored_times() {
        let dir = tempfile::tempdir().unwrap();
        let mut tracker = tracker_in(dir.path());
        tracker.poll(T0, 1000, true);
        tracker.poll(T0 + 30, 1030, false);
        // Wall jumps 60 s with no monotonic advance: below the suspend
        // threshold, so every stored time shifts forward 60 s.
        tracker.poll(T0 + 90, 1030, false);
        let ts = tracker.store().transitions().to_vec();
        assert_eq!(ts[0].wall_time_secs, T0 + 60);
        assert_eq!(ts[1].wall_time_secs, T0 + 90);
    }

    fn limit_tracker(dir: &std::path::Path, today: i64, limit: i64) -> ScreenTimeTracker {
        // Seed history on disk: active since 03:00 today.
        std::fs::write(
            dir.join("session-active-history.json"),
            render_history(&[tr(UserState::Inactive, UserState::Active, today)]),
        )
        .unwrap();
        ScreenTimeTracker::new(
            ScreenTimeSettings {
                history_enabled: true,
                daily_limit_enabled: true,
                daily_limit_secs: limit,
                grayscale: true,
            },
            HistoryStore::new(dir.join("session-active-history.json")),
        )
    }

    fn upcoming(events: &[ScreenTimeEvent]) -> bool {
        events
            .iter()
            .any(|e| matches!(e, ScreenTimeEvent::LimitUpcoming { .. }))
    }

    #[test]
    fn limit_warns_at_exactly_ten_minutes_then_reaches_then_clears_next_day() {
        let dir = tempfile::tempdir().unwrap();
        let (today, tomorrow) = start_of_today(T0);
        let mut tracker = limit_tracker(dir.path(), today, T0 - today + 900);
        // 15 min left: GNOME schedules the warning, it posts nothing yet.
        assert!(!upcoming(&tracker.poll(T0, 1000, true).events));
        assert!(!upcoming(&tracker.poll(T0 + 299, 1299, true).events));
        // Exactly 600 s left (`Math.ceil(remainingSecs) === timeoutLimit`).
        let poll = tracker.poll(T0 + 300, 1300, true);
        assert!(
            poll.events.contains(&ScreenTimeEvent::LimitUpcoming {
                remaining_secs: LIMIT_UPCOMING_NOTIFICATION_SECONDS
            }),
            "{:?}",
            poll.events
        );
        // Any other remaining time posts nothing, re-polls included.
        assert!(!upcoming(&tracker.poll(T0 + 300, 1300, true).events));
        assert!(!upcoming(&tracker.poll(T0 + 301, 1301, true).events));
        assert_eq!(tracker.state(), TimeLimitsState::Active);
        let poll = tracker.poll(T0 + 900, 1900, true);
        assert!(poll.events.contains(&ScreenTimeEvent::LimitReached));
        assert!(poll
            .events
            .contains(&ScreenTimeEvent::GrayscaleChanged(true)));
        assert!(!poll.events.contains(&ScreenTimeEvent::LimitNoticeCleared));
        assert_eq!(tracker.state(), TimeLimitsState::LimitReached);
        // Reaching the limit twice must not re-fire the one-shot notices.
        let poll = tracker.poll(T0 + 950, 1950, true);
        assert!(!poll.events.contains(&ScreenTimeEvent::LimitReached));
        // After 03:00 the next day the limit resets to Active and, with
        // over 10 min left again, GNOME destroys the notification.
        let poll = tracker.poll(tomorrow + 10, 1950 + (tomorrow + 10 - (T0 + 950)), true);
        assert_eq!(tracker.state(), TimeLimitsState::Active);
        assert!(poll
            .events
            .contains(&ScreenTimeEvent::GrayscaleChanged(false)));
        assert!(poll.events.contains(&ScreenTimeEvent::LimitNoticeCleared));
    }

    #[test]
    fn limit_at_or_under_ten_minutes_gets_no_warning() {
        // GNOME only warns at exactly 600 s left, so a fresh 2 min (or
        // 10 s) limit goes straight to "Reached".
        let dir = tempfile::tempdir().unwrap();
        let (today, _) = start_of_today(T0);
        let mut tracker = limit_tracker(dir.path(), today, T0 - today + 120);
        for (wall, mono) in [(T0, 1000), (T0 + 60, 1060), (T0 + 119, 1119)] {
            assert!(!upcoming(&tracker.poll(wall, mono, true).events));
        }
        let poll = tracker.poll(T0 + 120, 1120, true);
        assert!(poll.events.contains(&ScreenTimeEvent::LimitReached));
        assert!(!upcoming(&poll.events));
    }

    #[test]
    fn turning_the_limit_off_removes_the_notice() {
        let dir = tempfile::tempdir().unwrap();
        let (today, _) = start_of_today(T0);
        let mut tracker = limit_tracker(dir.path(), today, T0 - today + 10);
        tracker.poll(T0, 1000, true);
        let poll = tracker.poll(T0 + 10, 1010, true);
        assert!(poll.events.contains(&ScreenTimeEvent::LimitReached));
        let mut settings = tracker.settings();
        settings.daily_limit_enabled = false;
        tracker.set_settings(settings);
        let poll = tracker.poll(T0 + 11, 1011, true);
        assert_eq!(tracker.state(), TimeLimitsState::Active);
        assert!(poll.events.contains(&ScreenTimeEvent::LimitNoticeCleared));
        // Removed once; nothing more to remove.
        let poll = tracker.poll(T0 + 12, 1012, true);
        assert!(!poll.events.contains(&ScreenTimeEvent::LimitNoticeCleared));
        // Disabling history destroys the source too.
        settings.daily_limit_enabled = true;
        tracker.set_settings(settings);
        assert!(tracker
            .poll(T0 + 13, 1013, true)
            .events
            .contains(&ScreenTimeEvent::LimitReached));
        settings.history_enabled = false;
        tracker.set_settings(settings);
        assert!(tracker
            .poll(T0 + 14, 1014, true)
            .events
            .contains(&ScreenTimeEvent::LimitNoticeCleared));
    }

    #[test]
    fn corrupt_history_is_reported_and_not_fabricated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session-active-history.json");
        std::fs::write(&path, r#"[{"oldState":0}]"#).unwrap();
        let mut tracker = tracker_in(dir.path());
        let poll = tracker.poll(T0, 1000, true);
        assert!(matches!(poll.loaded, Some(Err(HistoryError::Corrupt(_)))));
        // The tracker starts clean (startup edge only); the store then
        // replaces the corrupt bytes atomically with valid history.
        assert_eq!(tracker.store().transitions().len(), 1);
        assert_eq!(tracker.state(), TimeLimitsState::Active);
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(parse_history(&text).unwrap(), tracker.store().transitions());
    }

    fn eyesight() -> (BreakType, BreakTypeSettings) {
        (BreakType::Eyesight, BreakType::Eyesight.defaults())
    }

    fn break_service(now: i64) -> BreakService {
        let mut service = BreakService::new();
        service.manager.set_selected(&[eyesight()], now);
        service
    }

    fn has_title(events: &[BreakUiEvent], title: &str) -> bool {
        events
            .iter()
            .any(|e| matches!(e, BreakUiEvent::Notify(n) if n.title == title))
    }

    #[test]
    fn break_flows_from_due_through_overdue() {
        let mut service = break_service(T0);
        // Eyesight defaults: interval 1200 s. Still active beforehand.
        let events = service.poll(T0 + 100, 0);
        assert_eq!(service.manager.state(), BreakState::Active);
        assert!(!events.iter().any(|e| matches!(e, BreakUiEvent::Notify(_))));
        // Due at T0+1200 with the exact GNOME strings and actions.
        let events = service.poll(T0 + 1200, 0);
        assert_eq!(service.manager.state(), BreakState::BreakDue);
        let notice = events
            .iter()
            .find_map(|e| match e {
                BreakUiEvent::Notify(n) => Some(n),
                _ => None,
            })
            .expect("due notice");
        assert_eq!(notice.title, "Time for an Eyesight Break");
        assert_eq!(
            notice.body,
            "Take a break from the screen. Look at least 6 meters away for at least 20 seconds"
        );
        assert!(notice.urgent && notice.allow_delay && !notice.allow_skip && notice.allow_take);
        // 60 s later the overdue notice replaces it (overdue toggle is on).
        let events = service.poll(T0 + 1261, 0);
        assert!(has_title(&events, "Break Overdue"), "{events:?}");
    }

    #[test]
    fn break_upcoming_countdown_and_take_cycle() {
        let (t, mut cfg) = eyesight();
        cfg.notify_upcoming = true;
        cfg.countdown = true;
        let mut service = BreakService::new();
        service.manager.set_selected(&[(t, cfg)], T0);
        // 2 min before due: upcoming notice with Delay + Skip.
        let events = service.poll(T0 + 1080, 0);
        let notice = events
            .iter()
            .find_map(|e| match e {
                BreakUiEvent::Notify(n) => Some(n),
                _ => None,
            })
            .expect("upcoming notice");
        assert_eq!(notice.title, "Eyesight Break Soon");
        assert!(notice.allow_delay && notice.allow_skip && !notice.allow_take);
        // 60 s before due: the prominent countdown starts.
        let events = service.poll(T0 + 1141, 0);
        assert!(
            events.iter().any(|e| matches!(
                e,
                BreakUiEvent::StartCountdown { due_in_secs, .. } if *due_in_secs == 59
            )),
            "{events:?}"
        );
        assert_eq!(countdown_text(59), "Break in 59 seconds");
        assert_eq!(countdown_text(1), "Break in 1 second");
    }

    #[test]
    fn idle_break_fades_completes_and_sounds() {
        let mut service = break_service(T0);
        // Idle past the due time: the break starts, fading the screen.
        let events = service.poll(T0 + 1210, 60);
        assert_eq!(service.manager.state(), BreakState::InBreak);
        assert!(
            events.contains(&BreakUiEvent::FadeScreen(true)),
            "{events:?}"
        );
        assert!(
            has_title(&events, "Eyesight Break in Progress"),
            "{events:?}"
        );
        // 20 s idle counts as taking the eyesight break: back active, the
        // fade lifts, the notice dismisses and the complete sound plays.
        let events = service.poll(T0 + 1240, 0);
        assert_eq!(service.manager.state(), BreakState::Active);
        assert!(
            events.contains(&BreakUiEvent::FadeScreen(false)),
            "{events:?}"
        );
        assert!(events.contains(&BreakUiEvent::DismissNotice), "{events:?}");
        assert!(
            events.contains(&BreakUiEvent::PlayCompleteSound),
            "{events:?}"
        );
    }

    #[test]
    fn delay_and_skip_push_the_due_time() {
        let mut service = break_service(T0);
        service.poll(T0 + 1200, 0);
        assert_eq!(service.manager.state(), BreakState::BreakDue);
        // Delay pushes this break out by delay-seconds (180 s).
        service.delay_break(T0 + 1200);
        assert_eq!(service.manager.state(), BreakState::Active);
        assert_eq!(service.manager.next_break_due_time(T0 + 1200), T0 + 1380);
        // Skip restarts the interval at now.
        service.poll(T0 + 1380, 0);
        assert_eq!(service.manager.state(), BreakState::BreakDue);
        service.skip_break(T0 + 1380);
        assert_eq!(service.manager.state(), BreakState::Active);
        assert_eq!(service.manager.next_break_due_time(T0 + 1380), T0 + 2580);
    }

    #[test]
    fn skip_upcoming_preserves_the_actual_deadline_through_due() {
        let (t, mut cfg) = eyesight();
        cfg.interval_secs = 130;
        cfg.notify_upcoming = true;
        let mut service = BreakService::new();
        service.manager.set_selected(&[(t, cfg)], T0);
        assert!(has_title(&service.poll(T0 + 10, 0), "Eyesight Break Soon"));
        let due = service.manager.next_break_due_time(T0 + 11);
        assert_eq!(due, T0 + 130);
        // GNOME's skipBreak only restarts already-due breaks. An actual
        // upcoming Skip is a timer no-op, not a restart at the press.
        service.skip_break(T0 + 11);
        assert_eq!(service.manager.next_break_due_time(T0 + 11), due);
        assert!(!has_title(
            &service.poll(due - 1, 0),
            "Time for an Eyesight Break"
        ));
        assert!(has_title(
            &service.poll(due, 0),
            "Time for an Eyesight Break"
        ));
        assert_eq!(service.manager.next_break_due_time(due), due);
    }

    #[test]
    fn longest_due_break_wins_and_unknown_ids_are_ignored() {
        assert_eq!(BreakType::from_id("movement"), Some(BreakType::Movement));
        assert_eq!(BreakType::from_id("nap"), None);
        let mut manager = BreakManager::new();
        manager.set_selected(
            &[
                (BreakType::Eyesight, BreakType::Eyesight.defaults()),
                (BreakType::Movement, BreakType::Movement.defaults()),
            ],
            T0,
        );
        // Eyesight (1200 s) is due first and alone at T0+1200.
        assert_eq!(
            manager.get_next_break_due(T0 + 1200).0,
            Some(BreakType::Eyesight)
        );
        // Both due at T0+1800: movement wins on longer duration.
        assert_eq!(
            manager.get_next_break_due(T0 + 1800).0,
            Some(BreakType::Movement)
        );
        // Deselecting everything disables the machine.
        manager.set_selected(&[], T0 + 1800);
        assert_eq!(manager.state(), BreakState::Disabled);
    }

    #[test]
    fn lock_screen_toggle_beats_fade_and_quiet_breaks_stay_in_tray() {
        let (t, mut cfg) = eyesight();
        cfg.lock_screen = true;
        cfg.notify = false;
        let mut service = BreakService::new();
        service.manager.set_selected(&[(t, cfg)], T0);
        let events = service.poll(T0 + 1210, 60);
        assert!(events.contains(&BreakUiEvent::LockScreen), "{events:?}");
        assert!(!events.contains(&BreakUiEvent::FadeScreen(true)));
        let notice = events
            .iter()
            .find_map(|e| match e {
                BreakUiEvent::Notify(n) => Some(n),
                _ => None,
            })
            .expect("in-progress notice");
        assert!(!notice.urgent, "notify=false keeps LOW urgency");
    }

    #[test]
    fn time_spans_use_gnome_units() {
        assert_eq!(format_time_span(1), ("1 second".into(), 1));
        assert_eq!(format_time_span(90), ("1 minute".into(), 1));
        assert_eq!(format_time_span(120), ("2 minutes".into(), 2));
        assert_eq!(format_time_span(3600), ("1 hour".into(), 1));
        assert_eq!(format_time_span(90000), ("1 day".into(), 1));
        let upcoming = limit_upcoming_notice();
        assert_eq!(upcoming.title, "Screen Time Limit in 10 Minutes");
        let reached = limit_reached_notice();
        assert_eq!(reached.title, "Screen Time Limit Reached");
    }

    #[test]
    fn user_active_follows_idle_delay() {
        let delay = Duration::from_secs(300);
        assert!(is_user_active(Some(Duration::from_secs(10)), delay));
        assert!(!is_user_active(Some(Duration::from_secs(300)), delay));
        assert!(is_user_active(None, delay));
        assert!(is_user_active(
            Some(Duration::from_secs(10_000)),
            Duration::ZERO
        ));
    }

    #[test]
    fn take_break_fades_and_limit_time_tracks_state() {
        let dir = tempfile::tempdir().unwrap();
        let (today, _) = start_of_today(T0);
        std::fs::write(
            dir.path().join("session-active-history.json"),
            render_history(&[tr(UserState::Inactive, UserState::Active, today)]),
        )
        .unwrap();
        let mut tracker = ScreenTimeTracker::new(
            ScreenTimeSettings {
                history_enabled: true,
                daily_limit_enabled: true,
                daily_limit_secs: T0 - today + 60,
                grayscale: false,
            },
            HistoryStore::new(dir.path().join("session-active-history.json")),
        );
        tracker.poll(T0, 1000, true);
        assert_eq!(tracker.daily_limit_time(T0), T0 + 60);
        assert_eq!(tracker.settings().daily_limit_secs, T0 - today + 60);

        let mut service = break_service(T0);
        assert_eq!(
            service
                .manager
                .config(BreakType::Eyesight)
                .unwrap()
                .interval_secs,
            1200
        );
        // Take with nothing due does nothing.
        assert!(service.take_break().is_empty());
        service.poll(T0 + 1200, 0);
        assert_eq!(service.manager.state(), BreakState::BreakDue);
        // Take asks the user to go idle: fade (lock toggle is off).
        assert_eq!(service.take_break(), vec![BreakUiEvent::FadeScreen(true)]);
        assert_eq!(service.manager.last_break_end_time(), T0);
    }

    #[test]
    fn helpers_tolerate_a_schema_less_session_bus_less_host() {
        assert!(history_path().ends_with("gnome-shell/session-active-history.json"));
        assert!(wall_now() > 1_700_000_000);
        let mono = MonoClock::start();
        assert!(mono.now() >= 0);
        // Bogus schemas never crash; wellbeing falls back to compiled defaults.
        assert!(!settings_schema_available("org.tuna.does.not.exist"));
        assert!(open_settings("org.tuna.does.not.exist").is_none());
        if settings_schema_available(SCREEN_TIME_SCHEMA) {
            assert!(read_screen_time_settings().daily_limit_secs >= 1);
        } else {
            assert_eq!(read_screen_time_settings(), ScreenTimeSettings::default());
        }
        for (t, _) in read_selected_breaks() {
            assert!(SUPPORTED_BREAK_TYPES.contains(&t.id()));
        }
        // Without a session bus there is no idle monitor; that must be a
        // quiet None, never a block or a crash.
        let _ = idle_time();
        let _ = session_idle_delay();
    }

    /// A break notice post: title, body, actions, urgent, replaced id, id.
    type ActionPost = (
        String,
        String,
        Vec<(String, String)>,
        bool,
        Option<u64>,
        u64,
    );
    type ActionCallbacks = HashMap<(u64, String), Rc<dyn Fn()>>;
    /// A screen-time notice post: title, body, urgent and the id it replaced.
    type NoticePost = (String, String, bool, Option<u64>);

    struct Recorded {
        posts: Rc<RefCell<Vec<NoticePost>>>,
        countdowns: Rc<RefCell<Vec<(String, f64)>>>,
        countdown_hides: Rc<Cell<u32>>,
        locks: Rc<Cell<u32>>,
        action_posts: Rc<RefCell<Vec<ActionPost>>>,
        next_id: Rc<Cell<u64>>,
        registered: Rc<RefCell<Vec<(u64, String)>>>,
        callbacks: Rc<RefCell<ActionCallbacks>>,
        dropped: Rc<RefCell<Vec<u64>>>,
        queued: Rc<RefCell<Vec<BreakActionKind>>>,
        effect_sends: Rc<RefCell<Vec<(bool, bool)>>>,
        removals: Rc<RefCell<Vec<u64>>>,
    }

    fn recording_hooks(recorded: &Recorded) -> ShellHooks {
        let posts = recorded.posts.clone();
        let countdowns = recorded.countdowns.clone();
        let countdown_hides = recorded.countdown_hides.clone();
        let locks = recorded.locks.clone();
        let action_posts = recorded.action_posts.clone();
        let next_id = recorded.next_id.clone();
        let registered = recorded.registered.clone();
        let callbacks = recorded.callbacks.clone();
        let dropped = recorded.dropped.clone();
        let queued = recorded.queued.clone();
        let effect_sends = recorded.effect_sends.clone();
        let removals = recorded.removals.clone();
        ShellHooks {
            post_notice: Rc::new({
                let next_id = next_id.clone();
                move |title, body, urgent, replaces| {
                    posts.borrow_mut().push((title, body, urgent, replaces));
                    Some(replaces.unwrap_or_else(|| {
                        next_id.set(next_id.get() + 1);
                        next_id.get()
                    }))
                }
            }),
            remove_notice: Rc::new(move |id| removals.borrow_mut().push(id)),
            show_countdown: Rc::new(move |label, progress| {
                countdowns.borrow_mut().push((label, progress));
            }),
            hide_countdown: Rc::new(move || {
                countdown_hides.set(countdown_hides.get() + 1);
            }),
            lock: Rc::new(move || {
                locks.set(locks.get() + 1);
            }),
            post_action_notice: Rc::new(move |title, body, actions, urgent, replaces| {
                let id = replaces.unwrap_or_else(|| {
                    next_id.set(next_id.get() + 1);
                    next_id.get()
                });
                action_posts
                    .borrow_mut()
                    .push((title, body, actions, urgent, replaces, id));
                Some(id)
            }),
            on_notice_action: Rc::new({
                let callbacks = callbacks.clone();
                move |id, key, handler| {
                    registered.borrow_mut().push((id, key.clone()));
                    callbacks.borrow_mut().insert((id, key), handler);
                }
            }),
            drop_notice_actions: {
                let callbacks = callbacks.clone();
                Rc::new(move |id| {
                    dropped.borrow_mut().push(id);
                    callbacks.borrow_mut().retain(|(known, _), _| *known != id);
                })
            },
            break_action: Rc::new(move |kind| {
                queued.borrow_mut().push(kind);
            }),
            effect_hook: {
                let effect_sends = effect_sends.clone();
                Rc::new(move |grayscale, fade| {
                    effect_sends.borrow_mut().push((grayscale, fade));
                    true
                })
            },
        }
    }

    fn recorded() -> (ShellHooks, Recorded) {
        let hooks = Recorded {
            posts: Rc::new(RefCell::new(Vec::new())),
            countdowns: Rc::new(RefCell::new(Vec::new())),
            countdown_hides: Rc::new(Cell::new(0)),
            locks: Rc::new(Cell::new(0)),
            action_posts: Rc::new(RefCell::new(Vec::new())),
            next_id: Rc::new(Cell::new(40)),
            registered: Rc::new(RefCell::new(Vec::new())),
            callbacks: Rc::new(RefCell::new(HashMap::new())),
            dropped: Rc::new(RefCell::new(Vec::new())),
            queued: Rc::new(RefCell::new(Vec::new())),
            effect_sends: Rc::new(RefCell::new(Vec::new())),
            removals: Rc::new(RefCell::new(Vec::new())),
        };
        let shell = recording_hooks(&hooks);
        (shell, hooks)
    }

    #[test]
    fn dispatch_posts_limit_notices_with_gnome_strings_and_tracks_grayscale() {
        let (hooks, seen) = recorded();
        let mut effect = EffectState::default();
        let mut resident = None;
        dispatch_screen_time(
            &hooks,
            &mut effect,
            &mut resident,
            &[
                ScreenTimeEvent::StateChanged(TimeLimitsState::Active),
                ScreenTimeEvent::LimitUpcoming {
                    remaining_secs: 600,
                },
            ],
        );
        assert_eq!(
            seen.posts.borrow().as_slice(),
            &[(
                "Screen Time Limit in 10 Minutes".to_string(),
                "Your screen time limit is approaching".to_string(),
                true,
                None,
            )]
        );
        let warning = resident.expect("the warning is the resident notice");
        assert!(!effect.grayscale);
        dispatch_screen_time(
            &hooks,
            &mut effect,
            &mut resident,
            &[
                ScreenTimeEvent::LimitReached,
                ScreenTimeEvent::GrayscaleChanged(true),
            ],
        );
        // GNOME's TimeLimitsNotificationSource keeps one notification and
        // updates it in place: "Reached" replaces the warning, banner and
        // list entry both, rather than queuing a second one behind it.
        assert_eq!(
            seen.posts.borrow().as_slice()[1],
            (
                "Screen Time Limit Reached".to_string(),
                "It's time to stop using the device".to_string(),
                true,
                Some(warning),
            )
        );
        assert_eq!(resident, Some(warning));
        assert!(effect.grayscale);
        dispatch_screen_time(
            &hooks,
            &mut effect,
            &mut resident,
            &[ScreenTimeEvent::GrayscaleChanged(false)],
        );
        assert!(!effect.grayscale);
        // State changes and grayscale post no notices of their own.
        assert_eq!(seen.posts.borrow().len(), 2);
        // Limit off / next day: the resident is removed outright, once.
        dispatch_screen_time(
            &hooks,
            &mut effect,
            &mut resident,
            &[
                ScreenTimeEvent::LimitNoticeCleared,
                ScreenTimeEvent::LimitNoticeCleared,
            ],
        );
        assert_eq!(seen.removals.borrow().as_slice(), &[warning]);
        assert_eq!(resident, None);
    }

    #[test]
    fn dispatch_drives_break_surfaces_and_countdown_state() {
        let (hooks, seen) = recorded();
        let mut effect = EffectState::default();
        let mut countdown = None;
        let mut resident = None;
        dispatch_break_ui(
            &hooks,
            &mut effect,
            &mut countdown,
            &mut resident,
            &[
                BreakUiEvent::StateChanged(BreakState::BreakDue),
                BreakUiEvent::Notify(due_notice(BreakType::Eyesight, 20, true)),
                BreakUiEvent::StartCountdown {
                    break_type: BreakType::Eyesight,
                    due_in_secs: 30,
                },
            ],
        );
        // Break notices post WITH actions (plain posts stay empty). A due
        // notice allows take + delay, never skip.
        assert!(seen.posts.borrow().is_empty());
        let posted = seen.action_posts.borrow();
        assert_eq!(posted.len(), 1);
        let (title, body, actions, urgent, replaces, id) = &posted[0];
        assert!(*urgent, "GNOME posts it as Urgency.HIGH");
        assert_eq!(*replaces, None);
        assert_eq!(title, "Time for an Eyesight Break");
        assert_eq!(
            body,
            "Take a break from the screen. Look at least 6 meters away for at least 20 seconds"
        );
        assert_eq!(
            actions.as_slice(),
            &[
                ("take".to_string(), "Take".to_string()),
                ("delay".to_string(), "Delay".to_string()),
            ]
        );
        assert_eq!(resident, Some(*id));
        assert_eq!(
            seen.registered.borrow().as_slice(),
            &[(41, "take".to_string()), (41, "delay".to_string())]
        );
        drop(posted);
        assert_eq!(countdown, Some(BreakType::Eyesight));
        assert_eq!(
            seen.countdowns.borrow().as_slice(),
            &[("Break in 30 seconds".to_string(), 0.5)]
        );
        dispatch_break_ui(
            &hooks,
            &mut effect,
            &mut countdown,
            &mut resident,
            &[
                BreakUiEvent::FadeScreen(true),
                BreakUiEvent::LockScreen,
                BreakUiEvent::PlayCompleteSound,
                BreakUiEvent::StopCountdown,
                BreakUiEvent::DismissNotice,
                BreakUiEvent::FadeScreen(false),
            ],
        );
        assert!(!effect.grayscale && !effect.fade);
        assert_eq!(countdown, None);
        assert_eq!(seen.locks.get(), 1);
        assert_eq!(seen.countdown_hides.get(), 1);
        // Dismissal removes the resident (GNOME destroys it), drops its
        // callbacks and clears the resident; posts stay empty.
        assert_eq!(seen.removals.borrow().as_slice(), &[41]);
        assert_eq!(seen.dropped.borrow().as_slice(), &[41]);
        assert_eq!(resident, None);
        assert!(seen.callbacks.borrow().is_empty());
        assert!(seen.posts.borrow().is_empty());
        assert_eq!(seen.action_posts.borrow().len(), 1);
    }

    #[test]
    fn stop_countdown_dispatches_hide_even_when_state_already_empty() {
        let (hooks, seen) = recorded();
        let mut effect = EffectState::default();
        let mut countdown = None;
        let mut resident = None;
        dispatch_break_ui(
            &hooks,
            &mut effect,
            &mut countdown,
            &mut resident,
            &[BreakUiEvent::StopCountdown, BreakUiEvent::StopCountdown],
        );
        assert_eq!(countdown, None);
        assert_eq!(seen.countdown_hides.get(), 2);
        assert!(seen.countdowns.borrow().is_empty());
    }

    #[test]
    fn break_action_callbacks_enqueue_take_delay_skip() {
        let (hooks, seen) = recorded();
        let mut effect = EffectState::default();
        let mut countdown = None;
        let mut resident = None;
        dispatch_break_ui(
            &hooks,
            &mut effect,
            &mut countdown,
            &mut resident,
            &[BreakUiEvent::Notify(due_notice(
                BreakType::Movement,
                20,
                true,
            ))],
        );
        assert_eq!(resident, Some(41));
        // Fire every registered callback the way the banner path does
        // after a successful bus invoke; each enqueues exactly its kind.
        for key in ["take", "delay"] {
            seen.callbacks.borrow()[&(41, key.to_string())]();
        }
        assert_eq!(
            seen.queued.borrow().as_slice(),
            &[BreakActionKind::Take, BreakActionKind::Delay]
        );
        // Denied: a due notice never allows skip, so no callback exists.
        assert!(!seen
            .callbacks
            .borrow()
            .contains_key(&(41, "skip".to_string())));
        // Denied: upcoming notices never allow take.
        dispatch_break_ui(
            &hooks,
            &mut effect,
            &mut countdown,
            &mut resident,
            &[BreakUiEvent::Notify(upcoming_notice(
                BreakType::Movement,
                120,
                true,
            ))],
        );
        // Updated in place: same id, and only allowed keys re-registered.
        assert_eq!(resident, Some(41));
        assert_eq!(seen.dropped.borrow().as_slice(), &[41]);
        assert!(!seen
            .callbacks
            .borrow()
            .contains_key(&(41, "take".to_string())));
        assert!(seen
            .callbacks
            .borrow()
            .contains_key(&(41, "skip".to_string())));
    }

    #[test]
    fn break_notices_update_the_resident_in_place() {
        let (hooks, seen) = recorded();
        let mut effect = EffectState::default();
        let mut countdown = None;
        let mut resident = None;
        dispatch_break_ui(
            &hooks,
            &mut effect,
            &mut countdown,
            &mut resident,
            &[BreakUiEvent::Notify(due_notice(
                BreakType::Movement,
                20,
                true,
            ))],
        );
        assert_eq!(resident, Some(41));
        dispatch_break_ui(
            &hooks,
            &mut effect,
            &mut countdown,
            &mut resident,
            &[BreakUiEvent::Notify(overdue_notice(90, false))],
        );
        // GNOME's BreakNotificationSource keeps one notification and
        // `set()`s it: no removal, same id, new text and urgency; the
        // old callbacks are forgotten and the new ones registered.
        assert!(seen.removals.borrow().is_empty());
        assert_eq!(seen.dropped.borrow().as_slice(), &[41]);
        assert_eq!(resident, Some(41));
        let posts = seen.action_posts.borrow();
        let (title, _, _, urgent, replaces, id) = &posts[1];
        assert_eq!(title, "Break Overdue");
        assert!(!*urgent, "notify off is Urgency.LOW");
        assert_eq!((*replaces, *id), (Some(41), 41));
        drop(posts);
        seen.callbacks.borrow()[&(41, "delay".to_string())]();
        assert_eq!(seen.queued.borrow().as_slice(), &[BreakActionKind::Delay]);
    }

    #[test]
    fn dismiss_without_a_resident_is_a_noop() {
        let (hooks, seen) = recorded();
        let mut effect = EffectState::default();
        let mut countdown = None;
        let mut resident = None;
        dispatch_break_ui(
            &hooks,
            &mut effect,
            &mut countdown,
            &mut resident,
            &[BreakUiEvent::DismissNotice],
        );
        assert!(seen.removals.borrow().is_empty());
        assert!(seen.dropped.borrow().is_empty());
        assert_eq!(resident, None);
    }

    #[test]
    fn disabled_manager_ignores_take_delay_and_skip() {
        let mut service = BreakService::new();
        assert_eq!(service.manager.state(), BreakState::Disabled);
        let due_before = service.manager.next_break_due_time(T0);
        assert!(service.take_break().is_empty());
        assert!(service.delay_break(T0).is_empty());
        assert!(service.skip_break(T0).is_empty());
        assert_eq!(service.manager.state(), BreakState::Disabled);
        assert_eq!(service.manager.next_break_due_time(T0), due_before);
    }

    #[test]
    fn effect_pair_consults_connection_state_even_when_unchanged() {
        let (hooks, seen) = recorded();
        let mut sent = None;
        // Initial neutral pair is also reconciled on each tick.
        let initial = EffectState {
            grayscale: false,
            fade: false,
        };
        maybe_send_effect(&hooks, &mut sent, &initial);
        maybe_send_effect(&hooks, &mut sent, &initial);
        assert_eq!(
            seen.effect_sends.borrow().as_slice(),
            &[(false, false), (false, false)]
        );
        // Each hook consult carries BOTH bools together.
        maybe_send_effect(
            &hooks,
            &mut sent,
            &EffectState {
                grayscale: true,
                fade: false,
            },
        );
        maybe_send_effect(
            &hooks,
            &mut sent,
            &EffectState {
                grayscale: true,
                fade: true,
            },
        );
        maybe_send_effect(
            &hooks,
            &mut sent,
            &EffectState {
                grayscale: true,
                fade: true,
            },
        );
        assert_eq!(
            seen.effect_sends.borrow().as_slice(),
            &[
                (false, false),
                (false, false),
                (true, false),
                (true, true),
                (true, true)
            ]
        );
        assert_eq!(
            sent,
            Some(EffectState {
                grayscale: true,
                fade: true
            })
        );
    }

    #[test]
    fn refused_pair_retries_and_accepted_pair_rechecks_connection() {
        let (mut hooks, seen) = recorded();
        let attempts = Rc::new(Cell::new(0u32));
        let gate = attempts.clone();
        let effect_sends = seen.effect_sends.clone();
        hooks.effect_hook = Rc::new(move |grayscale, fade| {
            gate.set(gate.get() + 1);
            effect_sends.borrow_mut().push((grayscale, fade));
            // Control absent for the first two ticks, present after.
            gate.get() > 2
        });
        let mut sent = None;
        let want = EffectState {
            grayscale: true,
            fade: false,
        };
        // Refused: nothing commits, every tick retries the same pair.
        maybe_send_effect(&hooks, &mut sent, &want);
        maybe_send_effect(&hooks, &mut sent, &want);
        assert_eq!(sent, None);
        assert_eq!(attempts.get(), 2);
        // Applied: retain the pair, but recheck the connection every tick.
        maybe_send_effect(&hooks, &mut sent, &want);
        maybe_send_effect(&hooks, &mut sent, &want);
        assert_eq!(sent, Some(want));
        assert_eq!(attempts.get(), 4);
        assert_eq!(
            seen.effect_sends.borrow().as_slice(),
            &[(true, false), (true, false), (true, false), (true, false)]
        );
    }

    #[test]
    fn unchanged_pair_loses_cached_application_when_connection_is_lost() {
        let (mut hooks, _) = recorded();
        let connected = Rc::new(Cell::new(true));
        let check = connected.clone();
        hooks.effect_hook = Rc::new(move |_, _| check.get());
        let want = EffectState {
            grayscale: true,
            fade: true,
        };
        let mut sent = None;
        maybe_send_effect(&hooks, &mut sent, &want);
        assert_eq!(sent, Some(want));
        connected.set(false);
        maybe_send_effect(&hooks, &mut sent, &want);
        assert_eq!(sent, None);
        connected.set(true);
        maybe_send_effect(&hooks, &mut sent, &want);
        assert_eq!(sent, Some(want));
    }

    #[test]
    fn fresh_short_limit_gets_a_deadline_before_the_background_sample() {
        let dir = tempfile::tempdir().unwrap();
        let mut tracker = tracker_in(dir.path());
        tracker.poll(T0, 1000, true);
        tracker.set_settings(ScreenTimeSettings {
            daily_limit_enabled: true,
            daily_limit_secs: 10,
            grayscale: true,
            ..ScreenTimeSettings::default()
        });
        tracker.poll(T0, 1000, true);
        assert_eq!(tracker.next_check_delay(T0), Duration::from_secs(10));
        let before = tracker.poll(T0 + 9, 1009, true);
        assert!(!before.events.contains(&ScreenTimeEvent::LimitReached));
        assert_eq!(tracker.next_check_delay(T0 + 9), Duration::from_secs(1));
        let at = tracker.poll(T0 + 10, 1010, true);
        assert!(at.events.contains(&ScreenTimeEvent::LimitReached));
        assert!(at.events.contains(&ScreenTimeEvent::GrayscaleChanged(true)));
        assert!(!tracker
            .poll(T0 + 11, 1011, true)
            .events
            .contains(&ScreenTimeEvent::LimitReached));
    }

    #[test]
    fn limit_changes_disable_and_inactive_state_replace_the_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let mut tracker = tracker_in(dir.path());
        let mut settings = ScreenTimeSettings {
            daily_limit_enabled: true,
            daily_limit_secs: 10,
            ..ScreenTimeSettings::default()
        };
        tracker.set_settings(settings);
        tracker.poll(T0, 1000, true);
        assert_eq!(tracker.next_check_delay(T0), Duration::from_secs(10));
        settings.daily_limit_secs = 300;
        tracker.set_settings(settings);
        tracker.poll(T0, 1000, true);
        assert_eq!(tracker.next_check_delay(T0), SCREEN_TIME_POLL);
        settings.daily_limit_secs = 3;
        tracker.set_settings(settings);
        tracker.poll(T0, 1000, true);
        assert_eq!(tracker.next_check_delay(T0), Duration::from_secs(3));
        tracker.poll(T0 + 1, 1001, false);
        assert_eq!(tracker.next_check_delay(T0 + 1), SCREEN_TIME_POLL);
        tracker.poll(T0 + 20, 1020, true);
        assert_eq!(tracker.next_check_delay(T0 + 20), Duration::from_secs(2));
        settings.daily_limit_enabled = false;
        tracker.set_settings(settings);
        tracker.poll(T0 + 20, 1020, true);
        assert_eq!(tracker.next_check_delay(T0 + 20), SCREEN_TIME_POLL);
        settings.history_enabled = false;
        tracker.set_settings(settings);
        tracker.poll(T0 + 20, 1020, true);
        assert_eq!(tracker.next_check_delay(T0 + 20), SCREEN_TIME_POLL);
    }

    #[test]
    fn warning_and_local_day_reset_get_exact_earlier_wakes() {
        let dir = tempfile::tempdir().unwrap();
        let mut tracker = tracker_in(dir.path());
        tracker.set_settings(ScreenTimeSettings {
            daily_limit_enabled: true,
            daily_limit_secs: LIMIT_UPCOMING_NOTIFICATION_SECONDS + 5,
            ..ScreenTimeSettings::default()
        });
        tracker.poll(T0, 1000, true);
        assert_eq!(tracker.next_check_delay(T0), Duration::from_secs(5));
        let warning = tracker.poll(T0 + 5, 1005, true);
        assert!(warning.events.iter().any(|event| matches!(event,
            ScreenTimeEvent::LimitUpcoming { remaining_secs } if *remaining_secs == LIMIT_UPCOMING_NOTIFICATION_SECONDS)));
        assert_eq!(tracker.next_check_delay(T0 + 5), SCREEN_TIME_POLL);
        let (_, tomorrow) = start_of_today(T0);
        tracker.poll(tomorrow - 2, 1000 + (tomorrow - 2 - T0), true);
        assert_eq!(tracker.state(), TimeLimitsState::LimitReached);
        assert_eq!(
            tracker.next_check_delay(tomorrow - 2),
            Duration::from_secs(2)
        );
        tracker.poll(tomorrow, 1000 + (tomorrow - T0), true);
        assert_eq!(tracker.state(), TimeLimitsState::Active);
        assert_eq!(tracker.next_check_delay(tomorrow), Duration::from_secs(5));
    }

    #[test]
    fn background_sampling_and_break_poll_caps_are_preserved() {
        assert_eq!(SCREEN_TIME_POLL, Duration::from_secs(30));
        assert_eq!(BREAK_POLL, Duration::from_secs(2));
    }

    #[test]
    fn break_settings_update_applies_live() {
        let mut manager = BreakManager::new();
        manager.set_selected(&[(BreakType::Eyesight, BreakType::Eyesight.defaults())], T0);
        assert_eq!(manager.next_break_due_time(T0), T0 + 1200);
        let mut cfg = BreakType::Eyesight.defaults();
        cfg.interval_secs = 60;
        manager.set_break_settings(BreakType::Eyesight, cfg);
        assert_eq!(
            manager.config(BreakType::Eyesight).unwrap().interval_secs,
            60
        );
        assert_eq!(manager.next_break_due_time(T0), T0 + 60);
        // Unselected types stay ignored.
        manager.set_break_settings(BreakType::Movement, BreakType::Movement.defaults());
        assert!(manager.config(BreakType::Movement).is_none());
    }

    #[test]
    fn countdown_progress_clamps_to_the_countdown_window() {
        assert_eq!(countdown_progress(60), 0.0);
        assert_eq!(countdown_progress(600), 0.0);
        assert_eq!(countdown_progress(30), 0.5);
        assert_eq!(countdown_progress(0), 1.0);
        assert_eq!(countdown_progress(-5), 1.0);
    }

    #[test]
    fn empty_break_selection_stays_disabled_silently() {
        let (hooks, seen) = recorded();
        let mut service = BreakService::new();
        let mut effect = EffectState::default();
        let mut countdown = None;
        let mut resident = None;
        // No selection, no config: polls stay Disabled and silent.
        for now in [T0, T0 + 3600] {
            let events = service.poll(now, 0);
            assert!(events.is_empty());
            dispatch_break_ui(&hooks, &mut effect, &mut countdown, &mut resident, &events);
        }
        assert_eq!(service.manager.state(), BreakState::Disabled);
        assert!(seen.posts.borrow().is_empty());
        assert!(seen.countdowns.borrow().is_empty());
        assert_eq!(seen.locks.get(), 0);
        assert_eq!(countdown, None);
    }

    #[test]
    fn disabled_history_dispatches_no_limit_notices() {
        let (hooks, seen) = recorded();
        let dir = tempfile::tempdir().unwrap();
        let mut tracker = ScreenTimeTracker::new(
            ScreenTimeSettings {
                history_enabled: false,
                daily_limit_enabled: true,
                daily_limit_secs: 10,
                grayscale: true,
            },
            HistoryStore::new(dir.path().join("session-active-history.json")),
        );
        let mut effect = EffectState::default();
        let poll = tracker.poll(T0, 1000, true);
        assert!(poll.stored.is_ok());
        assert_eq!(tracker.state(), TimeLimitsState::Disabled);
        dispatch_screen_time(&hooks, &mut effect, &mut None, &poll.events);
        assert!(seen.posts.borrow().is_empty());
        assert!(!effect.grayscale);
        assert!(!dir.path().join("session-active-history.json").exists());
    }
}
