//! Opt-in session save and restore (P-SY-08, #338).
//!
//! What GNOME 51 does — and pointedly does not do: upstream's
//! `gsm-manager.c` hardcodes `RestoreSupported` to `FALSE` (`// TODO:
//! For GNOME 50, we've disabled the save/restore feature`), so GNOME
//! 50/51 advertises no restore support even though the
//! `RegisterRestore` plumbing still exists behind the flag. Tuna
//! Desktop goes further: when the user opts in, the session genuinely
//! brings back their windows after logout and reboot, using the same
//! D-Bus contract shapes GNOME defined (verified against the 51.0
//! tag; see below). Tuna implements the capability in three layers:
//!
//! 1. **Saved state.** [`SavedSession`] is the on-disk snapshot: one
//!    entry per restorable window with its application identity
//!    (desktop id / app id), workspace, geometry and layout flags,
//!    plus the active workspace. It lives at
//!    [`session_file_path`] (`$XDG_STATE_HOME/tuna/session.json`).
//!    Writes are atomic (unique temporary file plus rename, with a
//!    directory fsync) and the file is created mode `0600`: window
//!    titles can name private documents.
//! 2. **Restore plan.** [`plan_restore`] turns a snapshot into launch
//!    requests ([`LaunchRequest`]) for apps that are still installed
//!    and did not crash, skipping the rest with an explicit
//!    [`SkipReason`]. [`scan_autostart`] adds the XDG autostart
//!    entries GNOME would launch at login, merged without duplicates
//!    by [`merge_autostart`]. [`launch_desktop_id`] resolves each
//!    request to an installed `.desktop` file and spawns it detached,
//!    exactly as a user's own launch would.
//! 3. **Session-manager contracts.** [`serve_session_manager`]
//!    answers `org.gnome.SessionManager`'s `RestoreSupported`
//!    property truthfully from the user's opt-in preference (what
//!    GNOME Settings' multitasking panel checks) plus the
//!    `RegisterRestore` / `DeletedInstanceIds` / `UnregisterRestore`
//!    methods, with the exact signatures, restore-reason codes and
//!    instance lifecycle from gnome-session 51.0's interface and
//!    implementation (see below). The facade stays off the bus when a
//!    real session manager owns the name. The `xdg_session_manager_v1`
//!    Wayland global is *not* advertised here: Smithay 0.7 has no such
//!    global (see `docs/protocols.md`), so the wire protocol waits on
//!    a Smithay upgrade; the stable per-window session token it would
//!    carry is [`session_id_for`].
//!
//! ## Secrets policy
//!
//! Saved state names *what* to reopen, never *how it ran*: desktop
//! id, app id, display title, workspace, geometry and layout flags.
//! It never records command lines, `Exec=` expansions, environment,
//! working directories, URIs, tokens or window contents. The launcher
//! resolves the desktop id to a installed `.desktop` file at restore
//! time, exactly as a user's own launch would. [`SAVED_JSON_KEYS`]
//! pins the allowed serialized keys; [`saved_state_has_no_secrets`]
//! audits a snapshot for the policy and the tests fail on drift.
//!
//! ## Opt-in and disable
//!
//! Restore is off by default ([`RestorePrefs::default`]); the shell
//! owns the preference UI (as with `idle-delay`, the compositor does
//! not read GSettings) and pushes the bool down. Disabling calls
//! [`clear_at`], which removes only the session file — no other user
//! data is touched. Saving while disabled is a no-op
//! ([`save_if_enabled`]).
//!
//! ## Production entry points
//!
//! - Startup: [`startup_session_manager`] serves the facade on the
//!   session bus and hands back the live [`RestoreSupported`] flag
//!   the shell's preference UI drives.
//! - Login: [`login_restore_plan`] loads the snapshot, plans the
//!   restore against [`installed_apps`] and the previous session's
//!   crash set, merges [`scan_autostart`], and queues
//!   [`PendingPlacements`]; [`launch_plan`] runs each
//!   [`LaunchRequest`] through the launcher ([`launch_desktop_id`]).
//! - Map: as each relaunched window maps, the compositor pops its
//!   placement with `PendingPlacements::take_for` (matched by app id
//!   and toplevel tag) and applies it with
//!   [`apply_restored_placement`].
//! - Logout / shutdown: [`logout_save`] collects one [`WindowRow`]
//!   per mapped toplevel (`StateModel` for app id, title, workspace;
//!   `WindowManager` for geometry and layout flags;
//!   `protocols::toplevel_tag` for the tag) and saves it when the user
//!   opted in.
//! - Crash sets: the supervisor's per-app crash record feeds
//!   `plan_restore`'s `crashed` argument so a crashing app is not
//!   relaunched into a loop; the entry stays in the file for the next
//!   clean login. Within a session, the facade's `NameOwnerChanged`
//!   watch marks vanished bus names crashed the same way.
//! - Corrupt or version-newer files are ignored ([`IgnoreReason`]) and
//!   can be quarantined aside with [`quarantine_at`] for diagnosis;
//!   they never block login.
//!
//! ## GNOME comparison (verified against the 51.0 tag)
//!
//! Contract shapes verified against gnome-session 51.0
//! (`org.gnome.SessionManager.xml`, `gsm-manager.c` and
//! `gsm-session-save.c`): `RestoreSupported` is a read-only boolean
//! property ("Communicates whether the session supports the
//! save/restore mechanism"); `RegisterRestore(app_id, dbus_name)`
//! returns `(reason, instance_id, cleanup_ids)`; `DeletedInstanceIds`
//! `(app_id, ids)` and `UnregisterRestore(app_id, instance_id)`
//! manage instance lifetime. Upstream documents these as a private
//! interface between gnome-session and xdg-desktop-portal — apps talk
//! to the portal, which forwards here. Restore reasons (the XML's
//! documented meanings, matching [`reason`]): 0 = restore nothing, 1
//! = normal launch (restore reasonable state), 2 = post-crash
//! restore (restore carefully), 3 = normal restore after
//! logout/shutdown (restore everything).
//!
//! Two deliberate divergences from upstream. First, gnome-session
//! hardcodes `RestoreSupported` to `FALSE` ("For GNOME 50, we've
//! disabled the save/restore feature"), so GNOME 50/51 never
//! restores; Tuna answers the property truthfully from the user's
//! opt-in preference and genuinely restores — real restoration is
//! required here, not descoped. Second, upstream persists instance
//! state as a GVariant vardict (`version`, `dirty`, `instances`) at
//! `$XDG_STATE_HOME/gnome-session@<id>.state`; Tuna's snapshot is
//! JSON at [`session_file_path`] and carries no instance tokens (see
//! the secrets policy), so instance continuity lives in the
//! [`RestoreRegistry`] for the session. The instance lifecycle
//! otherwise mirrors `gsm-session-save.c`: register reuses a prior
//! instance id, cleanup ids are discarded ids only (never live
//! ones), an empty `dbus_name` falls back to the caller's bus name,
//! an unknown unregister is `NotRegistered`, and a
//! `NameOwnerChanged` watch marks vanished names crashed.
//!
//! Full `org.gnome.SessionManager` parity (logout, inhibit, client
//! registration, signals) is out of scope for P-SY-08.
//!
//! Autostart honours `Hidden=`, `OnlyShowIn=` / `NotShowIn=`,
//! `TryExec=` and `X-GNOME-Autostart-enabled=` per the XDG autostart
//! spec and GNOME's desktop-file handling.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, OnceLock,
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// On-disk snapshot schema version. Readers reject anything newer
/// rather than guessing at fields they do not understand.
pub const SCHEMA_VERSION: u32 = 1;

/// Upper bound on restored windows: a corrupt file claiming millions
/// of entries must not balloon memory or spawn a fork bomb.
pub const MAX_SAVED_WINDOWS: usize = 256;

/// Highest workspace id kept from a snapshot; dynamic workspaces are
/// small numbers and anything past this is stale or corrupt.
pub const MAX_WORKSPACE: u32 = 63;

/// Largest window dimension (either axis) accepted from a snapshot.
pub const MAX_DIMENSION: i32 = 16384;

/// Largest snapshot file accepted from disk. Reads are bounded to
/// this size *before* parsing so a hostile or ever-growing file
/// cannot balloon memory.
pub const MAX_SNAPSHOT_BYTES: u64 = 1024 * 1024;

/// D-Bus name whose `RestoreSupported` property Settings checks.
pub const SESSION_MANAGER_NAME: &str = "org.gnome.SessionManager";
/// Object path for the session-manager facade.
pub const SESSION_MANAGER_PATH: &str = "/org/gnome/SessionManager";

/// Exact JSON keys a snapshot may serialize. Anything else — command
/// lines, environment, tokens — is a secrets-policy violation.
pub const SAVED_JSON_KEYS: &[&str] = &[
    "version",
    "saved_at_unix",
    "active_workspace",
    "windows",
    "desktop_id",
    "app_id",
    "title",
    "tag",
    "workspace",
    "x",
    "y",
    "width",
    "height",
    "maximized",
    "fullscreen",
    "minimized",
    "sticky",
];

/// Opt-in preference. The shell owns the UI and pushes this down;
/// the compositor never reads GSettings itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RestorePrefs {
    /// Whether the user opted into save/restore. Off by default.
    pub enabled: bool,
}

impl RestorePrefs {
    /// Enabled preferences.
    pub fn enabled() -> Self {
        Self { enabled: true }
    }
}

/// Where one window sat: geometry plus workspace and layout flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowPlacement {
    /// Workspace the window belonged to.
    pub workspace: u32,
    /// Floating geometry in logical pixels (global origin).
    pub x: i32,
    /// Floating geometry in logical pixels (global origin).
    pub y: i32,
    /// Floating width in logical pixels; always positive.
    pub width: i32,
    /// Floating height in logical pixels; always positive.
    pub height: i32,
    /// Window was maximized (stashed floating geometry is `x..height`).
    pub maximized: bool,
    /// Window covered its output.
    pub fullscreen: bool,
    /// Window was hidden (GNOME's minimize).
    pub minimized: bool,
    /// Window was on every workspace.
    pub sticky: bool,
}

impl WindowPlacement {
    /// Validate a snapshot-supplied placement; `None` means the entry
    /// is stale or corrupt and must be skipped, not guessed at.
    pub fn validated(self) -> Option<Self> {
        // Saturating absolute value: `i32::MIN.abs()` panics in debug
        // builds, and a hostile snapshot may carry exactly that.
        // `saturating_abs` maps it to `i32::MAX`, which the bound
        // below rejects.
        if self.workspace > MAX_WORKSPACE
            || self.width <= 0
            || self.height <= 0
            || self.width > MAX_DIMENSION
            || self.height > MAX_DIMENSION
            || self.x.saturating_abs() > MAX_DIMENSION * 4
            || self.y.saturating_abs() > MAX_DIMENSION * 4
        {
            return None;
        }
        Some(self)
    }
}

/// One restorable window: application identity plus placement.
/// Display title only; see the module-level secrets policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedWindow {
    /// Desktop file id without the `.desktop` suffix
    /// (e.g. `org.gnome.TextEditor`): what the launcher resolves.
    pub desktop_id: String,
    /// Wayland app id / X11 class as the client set it, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_id: Option<String>,
    /// Window title for display in restore UI; untrusted, capped.
    #[serde(default)]
    pub title: String,
    /// xdg-toplevel-tag the client gave the window, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    /// Workspace the window belonged to.
    pub workspace: u32,
    /// Floating geometry in logical pixels (global origin).
    pub x: i32,
    /// Floating geometry in logical pixels (global origin).
    pub y: i32,
    /// Floating width in logical pixels.
    pub width: i32,
    /// Floating height in logical pixels.
    pub height: i32,
    /// Window was maximized.
    #[serde(default)]
    pub maximized: bool,
    /// Window covered its output.
    #[serde(default)]
    pub fullscreen: bool,
    /// Window was hidden (minimized).
    #[serde(default)]
    pub minimized: bool,
    /// Window was on every workspace.
    #[serde(default)]
    pub sticky: bool,
}

impl SavedWindow {
    /// Placement view of this entry, validated.
    pub fn placement(&self) -> Option<WindowPlacement> {
        WindowPlacement {
            workspace: self.workspace,
            x: self.x,
            y: self.y,
            width: self.width,
            height: self.height,
            maximized: self.maximized,
            fullscreen: self.fullscreen,
            minimized: self.minimized,
            sticky: self.sticky,
        }
        .validated()
    }
}

/// Whole-session snapshot as stored on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedSession {
    /// [`SCHEMA_VERSION`] at write time.
    pub version: u32,
    /// Save time as unix seconds; 0 when the clock was unavailable.
    pub saved_at_unix: u64,
    /// Active workspace at save time.
    pub active_workspace: u32,
    /// Restorable windows, most-recently-focused last.
    pub windows: Vec<SavedWindow>,
}

/// One live window row for [`collect`]: the runtime fills this from
/// the model (`StateModel`), geometry/layout (`WindowManager`) and
/// tag (`protocols::toplevel_tag`) without this module depending on
/// any of those types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowRow {
    /// Desktop file id (`org.gnome.TextEditor`); rows without one are
    /// kept only when `app_id` can stand in (see [`collect`]).
    pub desktop_id: Option<String>,
    /// Client-supplied app id, if known.
    pub app_id: Option<String>,
    /// Client-supplied title, capped on collect.
    pub title: String,
    /// Client-supplied toplevel tag, if known.
    pub tag: Option<String>,
    /// Placement at save time.
    pub placement: WindowPlacement,
}

/// Why a snapshot on disk was not used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IgnoreReason {
    /// Not valid JSON, or valid JSON of the wrong shape.
    Corrupt(String),
    /// Written by a newer snapshot writer.
    UnsupportedVersion(u32),
}

/// Result of reading a snapshot file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadOutcome {
    /// No snapshot stored (fresh install, or restore disabled).
    Absent,
    /// Snapshot parsed and validated.
    Loaded(SavedSession),
    /// A file exists but must not be restored from.
    Ignored(IgnoreReason),
}

/// Result of an opt-in-guarded save.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveOutcome {
    /// Snapshot written.
    Saved,
    /// Restore is disabled; nothing was written.
    SkippedDisabled,
}

/// Result of clearing saved state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClearOutcome {
    /// The session file was removed.
    Removed,
    /// There was no session file; nothing else was touched.
    Absent,
}

/// Why one saved window will not be relaunched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// No such `.desktop` file is installed anymore.
    MissingApp,
    /// The app crashed last session; not relaunched into a loop.
    Crashed,
    /// Placement failed validation (stale or corrupt entry).
    InvalidPlacement,
}

/// One window to reopen: identity plus where to put it. The launcher
/// resolves `desktop_id` itself; no command line is carried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchRequest {
    /// Desktop file id to launch.
    pub desktop_id: String,
    /// App id hint for matching the new window, when known.
    pub app_id: Option<String>,
    /// Tag hint for matching the new window, when known.
    pub tag: Option<String>,
    /// Placement to apply once the window maps.
    pub placement: WindowPlacement,
    /// Whether this launch comes from XDG autostart rather than the
    /// saved snapshot (for logging and restore UI).
    pub from_autostart: bool,
}

/// One saved window's restore fate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreEntry {
    /// Relaunch the app and apply the placement.
    Launch(LaunchRequest),
    /// Leave it closed, and say why.
    Skipped {
        /// Desktop file id that was skipped.
        desktop_id: String,
        /// Why it was skipped.
        reason: SkipReason,
    },
}

/// Snapshot path: `$XDG_STATE_HOME/tuna/session.json`, falling back
/// to `~/.local/state/tuna/session.json`.
pub fn session_file_path() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|h| h.join(".local/state"))
        })
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    base.join("tuna").join("session.json")
}

/// Current unix time in seconds; 0 when the clock is unavailable.
fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Cap an untrusted display title, keeping a valid UTF-8 prefix.
fn cap_title(title: &str) -> String {
    const MAX_TITLE: usize = 256;
    if title.len() <= MAX_TITLE {
        return title.to_owned();
    }
    let mut end = MAX_TITLE;
    while end > 0 && !title.is_char_boundary(end) {
        end -= 1;
    }
    title[..end].to_owned()
}

/// Cap an untrusted identifier (desktop id, app id, tag).
fn cap_id(value: &str, max: usize) -> String {
    if value.len() <= max {
        return value.to_owned();
    }
    let mut end = max;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

/// Normalize a desktop id for storage and lookup: trimmed, without a
/// trailing `.desktop` suffix, empty rejected.
fn normalize_desktop_id(raw: &str) -> Option<String> {
    let trimmed = raw.trim().strip_suffix(".desktop").unwrap_or(raw.trim());
    let trimmed = trimmed.trim();
    (!trimmed.is_empty()).then(|| cap_id(trimmed, 256))
}

/// Build a snapshot from live window rows. Rows without any
/// application identity are dropped (nothing could relaunch them);
/// rows whose app id can stand in for a missing desktop id keep the
/// app id in both fields so lookup still works. At most
/// [`MAX_SAVED_WINDOWS`] rows are kept.
pub fn collect(rows: impl IntoIterator<Item = WindowRow>, active_workspace: u32) -> SavedSession {
    let mut windows = Vec::new();
    for row in rows {
        if windows.len() >= MAX_SAVED_WINDOWS {
            break;
        }
        let Some(placement) = row.placement.validated() else {
            continue;
        };
        let desktop_id = row
            .desktop_id
            .as_deref()
            .and_then(normalize_desktop_id)
            .or_else(|| row.app_id.as_deref().and_then(normalize_desktop_id));
        let Some(desktop_id) = desktop_id else {
            continue;
        };
        windows.push(SavedWindow {
            desktop_id,
            app_id: row.app_id.as_deref().map(|a| cap_id(a.trim(), 256)),
            title: cap_title(&row.title),
            tag: row.tag.as_deref().map(|t| cap_id(t.trim(), 256)),
            workspace: placement.workspace,
            x: placement.x,
            y: placement.y,
            width: placement.width,
            height: placement.height,
            maximized: placement.maximized,
            fullscreen: placement.fullscreen,
            minimized: placement.minimized,
            sticky: placement.sticky,
        });
    }
    SavedSession {
        version: SCHEMA_VERSION,
        saved_at_unix: now_unix(),
        active_workspace: active_workspace.min(MAX_WORKSPACE),
        windows,
    }
}

/// Serialize a snapshot. The key set is pinned by [`SAVED_JSON_KEYS`];
/// adding a field here without updating that list fails the audit test.
pub fn serialize(session: &SavedSession) -> serde_json::Result<String> {
    serde_json::to_string_pretty(session)
}

/// Audit serialized snapshot JSON against the secrets policy: every
/// object key must be in [`SAVED_JSON_KEYS`]. Returns the offending
/// keys, empty when clean.
pub fn audit_json_keys(json: &str) -> Vec<String> {
    fn walk(value: &serde_json::Value, bad: &mut Vec<String>) {
        match value {
            serde_json::Value::Object(map) => {
                for (k, v) in map {
                    if !SAVED_JSON_KEYS.contains(&k.as_str()) && !bad.contains(k) {
                        bad.push(k.clone());
                    }
                    walk(v, bad);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    walk(item, bad);
                }
            }
            _ => {}
        }
    }
    let mut bad = Vec::new();
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(json) {
        walk(&value, &mut bad);
    }
    bad.sort();
    bad
}

/// Whether a snapshot's serialized form carries only the pinned keys.
/// Convenience wrapper over [`audit_json_keys`] for the save path.
pub fn saved_state_has_no_secrets(session: &SavedSession) -> bool {
    serialize(session)
        .map(|json| audit_json_keys(&json).is_empty())
        .unwrap_or(false)
}

/// Process-wide counter so concurrent saves never share a
/// temporary file name, even within the same nanosecond.
static SAVE_TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A temporary sibling of `path` unique to this process and call:
/// pid plus clock plus counter, created with `O_EXCL`, so two
/// sessions saving at once cannot truncate each other's file.
fn unique_tmp_path(path: &Path) -> PathBuf {
    let n = SAVE_TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let name = format!(".session.{}.{nanos:x}.{n}.tmp", std::process::id());
    path.parent().unwrap_or_else(|| Path::new(".")).join(name)
}

/// Write `session` to `path` atomically (unique temporary file in
/// the same directory plus rename, with a directory fsync so the
/// rename survives a crash) with mode `0600`. Parent directories are
/// created as needed. Parallel-session safe: each save uses its own
/// temporary file, so a concurrent save can only win or lose the
/// final rename, never interleave bytes.
pub fn save_to(path: &Path, session: &SavedSession) -> io::Result<()> {
    let json = serialize(session).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = unique_tmp_path(path);
    let saved = (|| -> io::Result<()> {
        use std::os::unix::fs::OpenOptionsExt;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true).mode(0o600);
        let mut file = opts.open(&tmp)?;
        use std::io::Write;
        file.write_all(json.as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, path)?;
        // Persist the rename itself: without a directory fsync a
        // power cut can lose it even though the file was synced.
        if let Some(parent) = path.parent() {
            std::fs::File::open(parent)?.sync_all()?;
        }
        Ok(())
    })();
    if saved.is_err() {
        std::fs::remove_file(&tmp).ok();
    }
    saved
}

/// Write `session` to `path` when `prefs` opts in; otherwise a no-op.
/// Disabling never deletes here — that is [`clear_at`]'s explicit job.
pub fn save_if_enabled(
    path: &Path,
    prefs: RestorePrefs,
    session: &SavedSession,
) -> io::Result<SaveOutcome> {
    if !prefs.enabled {
        return Ok(SaveOutcome::SkippedDisabled);
    }
    save_to(path, session)?;
    Ok(SaveOutcome::Saved)
}

/// Read and validate the snapshot at `path`. Missing files are
/// [`LoadOutcome::Absent`]; corrupt, wrong-shape and newer-version
/// files are [`LoadOutcome::Ignored`] — login never fails on them.
/// Oversized window lists are truncated to [`MAX_SAVED_WINDOWS`].
/// The read itself is bounded to [`MAX_SNAPSHOT_BYTES`]: the size is
/// checked up front *and* the stream is capped, so a file that grows
/// mid-read (or a device like `/dev/zero`) cannot balloon memory —
/// the old code read the whole file first and measured after.
pub fn load_from(path: &Path) -> LoadOutcome {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return LoadOutcome::Absent,
        Err(e) => return LoadOutcome::Ignored(IgnoreReason::Corrupt(e.to_string())),
    };
    // Fast path: reject an obviously oversized file without reading
    // it. This alone is not enough — the size can change, and some
    // files (fifos, devices) report no useful size — so the read
    // below is capped as well.
    if file
        .metadata()
        .is_ok_and(|meta| meta.len() > MAX_SNAPSHOT_BYTES)
    {
        return LoadOutcome::Ignored(IgnoreReason::Corrupt("snapshot over 1 MiB".into()));
    }
    let mut bytes = Vec::new();
    if let Err(e) = file.take(MAX_SNAPSHOT_BYTES + 1).read_to_end(&mut bytes) {
        return LoadOutcome::Ignored(IgnoreReason::Corrupt(e.to_string()));
    }
    if bytes.len() as u64 > MAX_SNAPSHOT_BYTES {
        return LoadOutcome::Ignored(IgnoreReason::Corrupt("snapshot over 1 MiB".into()));
    }
    let mut session: SavedSession = match serde_json::from_slice(&bytes) {
        Ok(session) => session,
        Err(e) => return LoadOutcome::Ignored(IgnoreReason::Corrupt(e.to_string())),
    };
    if session.version != SCHEMA_VERSION {
        return LoadOutcome::Ignored(IgnoreReason::UnsupportedVersion(session.version));
    }
    session.windows.truncate(MAX_SAVED_WINDOWS);
    session.active_workspace = session.active_workspace.min(MAX_WORKSPACE);
    LoadOutcome::Loaded(session)
}

/// Remove the snapshot at `path` and nothing else. Disabling restore
/// calls this: unrelated user data is never modified.
pub fn clear_at(path: &Path) -> io::Result<ClearOutcome> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(ClearOutcome::Removed),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(ClearOutcome::Absent),
        Err(e) => Err(e),
    }
}

/// Move an unreadable snapshot aside to `<name>.corrupt` for
/// diagnosis, so one bad write does not ignore-restore forever.
/// Missing files are a no-op returning `None`.
pub fn quarantine_at(path: &Path) -> io::Result<Option<PathBuf>> {
    if !path.exists() {
        return Ok(None);
    }
    let aside = path.with_extension("json.corrupt");
    std::fs::remove_file(&aside).ok();
    std::fs::rename(path, &aside)?;
    Ok(Some(aside))
}

/// Turn a snapshot into per-window restore fates. `installed` answers
/// whether a desktop id resolves to an installed `.desktop` file;
/// `crashed` holds desktop ids that crashed last session and must not
/// be relaunched into a loop (their entries stay in the file for the
/// next clean login). Invalid placements are skipped, never guessed.
pub fn plan_restore(
    session: &SavedSession,
    installed: &dyn Fn(&str) -> bool,
    crashed: &HashSet<String>,
) -> Vec<RestoreEntry> {
    session
        .windows
        .iter()
        .map(|window| {
            if crashed.contains(&window.desktop_id) {
                return RestoreEntry::Skipped {
                    desktop_id: window.desktop_id.clone(),
                    reason: SkipReason::Crashed,
                };
            }
            let Some(placement) = window.placement() else {
                return RestoreEntry::Skipped {
                    desktop_id: window.desktop_id.clone(),
                    reason: SkipReason::InvalidPlacement,
                };
            };
            if !installed(&window.desktop_id) {
                return RestoreEntry::Skipped {
                    desktop_id: window.desktop_id.clone(),
                    reason: SkipReason::MissingApp,
                };
            }
            RestoreEntry::Launch(LaunchRequest {
                desktop_id: window.desktop_id.clone(),
                app_id: window.app_id.clone(),
                tag: window.tag.clone(),
                placement,
                from_autostart: false,
            })
        })
        .collect()
}

/// Stable per-window session token: what the future
/// `xdg_session_manager_v1` global would hand clients to correlate a
/// relaunched window with its saved placement. Deterministic,
/// filesystem- and bus-safe, capped well under protocol limits.
pub fn session_id_for(desktop_id: &str, tag: Option<&str>) -> String {
    fn clean(part: &str, max: usize) -> String {
        let mut out = String::new();
        for ch in part.chars().take(max) {
            if ch.is_ascii_alphanumeric() || ch == '.' || ch == '-' || ch == '_' {
                out.push(ch);
            } else if ch.is_whitespace() || ch == '/' {
                out.push('_');
            }
        }
        out
    }
    let app = clean(desktop_id.trim(), 128);
    let app = if app.is_empty() { "app".into() } else { app };
    match tag.map(str::trim).filter(|t| !t.is_empty()) {
        Some(tag) => format!("tuna-{app}-{}", clean(tag, 64)),
        None => format!("tuna-{app}"),
    }
}

/// One XDG autostart entry that survived filtering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutostartEntry {
    /// Desktop file id without the `.desktop` suffix.
    pub desktop_id: String,
}

/// Parse the `[Desktop Entry]` group of a `.desktop` file into
/// key/value pairs. Only the keys autostart filtering needs are read;
/// `Exec=` lines are never expanded or returned.
fn desktop_entry_keys(text: &str) -> HashMap<String, String> {
    let mut keys = HashMap::new();
    let mut in_entry = false;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            keys.insert(k.trim().to_owned(), v.trim().to_owned());
        }
    }
    keys
}

/// Whether `candidate` names an executable: absolute path that
/// executes, or a `PATH` lookup hit.
fn try_exec_ok(candidate: &str, path_dirs: &[PathBuf]) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let is_exec = |p: &Path| {
        std::fs::metadata(p)
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    };
    let first = candidate.split_whitespace().next().unwrap_or("");
    if first.is_empty() {
        return false;
    }
    if first.contains('/') {
        return is_exec(Path::new(first));
    }
    path_dirs.iter().any(|dir| is_exec(&dir.join(first)))
}

/// Whether an autostart entry shows in this session per
/// `OnlyShowIn=` / `NotShowIn=`. Tuna sessions count as both `Tuna`
/// and `GNOME` (the session names itself `Tuna:GNOME`).
/// Split a `;`-separated desktop list (`OnlyShowIn=` / `NotShowIn=`).
fn split_desktops(value: &str) -> impl Iterator<Item = &str> {
    value.split(';').map(str::trim).filter(|s| !s.is_empty())
}

fn shown_in_session(keys: &HashMap<String, String>) -> bool {
    let ours = ["tuna", "gnome"];
    if let Some(only) = keys.get("OnlyShowIn") {
        return split_desktops(only).any(|d| ours.contains(&d.to_ascii_lowercase().as_str()));
    }
    let excluded = keys.get("NotShowIn").is_some_and(|not| {
        split_desktops(not).any(|d| ours.contains(&d.to_ascii_lowercase().as_str()))
    });
    !excluded
}

/// Whether an autostart `.desktop` file is launchable at all: it
/// names an application (`Type=Application`, required) with
/// something to run (`Exec=`, required and non-empty). Files that
/// fail this are config-only — a bare `[Desktop Entry]` stub, a
/// `Type=Link` bookmark — and must neither launch nor shadow a
/// same-stem system entry (see [`scan_autostart`]).
fn autostart_is_launchable(keys: &HashMap<String, String>) -> bool {
    keys.get("Type").is_some_and(|t| t == "Application")
        && keys.get("Exec").is_some_and(|exec| !exec.trim().is_empty())
}

/// Scan `dirs` (user dir first, so it shadows system entries by
/// desktop id) for autostart entries, honouring `Hidden=true`,
/// `OnlyShowIn=` / `NotShowIn=`, `TryExec=` and
/// `X-GNOME-Autostart-enabled=false`, like GNOME's startup handling.
/// Only launchable entries are returned: a file without
/// `Type=Application` or without `Exec=` is config-only and is
/// skipped *without* retaining its stem, so a launchable system
/// entry with the same name still loads. (An explicit disable —
/// `Hidden=true` or `X-GNOME-Autostart-enabled=false` — still
/// shadows, even when the stub carries no `Exec=`.) Unparseable
/// files are skipped; nothing is executed here.
pub fn scan_autostart(dirs: &[PathBuf]) -> Vec<AutostartEntry> {
    let path_dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    let mut seen = HashSet::new();
    let mut entries = Vec::new();
    for dir in dirs {
        let Ok(read) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut files: Vec<PathBuf> = read
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|ext| ext == "desktop"))
            .collect();
        files.sort();
        for file in files {
            let Some(stem) = file.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let Some(desktop_id) = normalize_desktop_id(stem) else {
                continue;
            };
            if seen.contains(&desktop_id) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&file) else {
                // An unreadable file still shadows by name (XDG
                // filename shadowing): only config-only *content*
                // releases the stem.
                seen.insert(desktop_id);
                continue;
            };
            let keys = desktop_entry_keys(&text);
            // Explicit disables shadow first, before the
            // launchability check: a `Hidden=true` stub is exactly
            // how a user turns a system autostart entry off.
            if keys
                .get("Hidden")
                .is_some_and(|v| v.eq_ignore_ascii_case("true"))
                || keys
                    .get("X-GNOME-Autostart-enabled")
                    .is_some_and(|v| v.eq_ignore_ascii_case("false"))
            {
                seen.insert(desktop_id);
                continue;
            }
            if !autostart_is_launchable(&keys) {
                continue;
            }
            seen.insert(desktop_id.clone());
            if !shown_in_session(&keys) {
                continue;
            }
            if let Some(try_exec) = keys.get("TryExec") {
                if !try_exec_ok(try_exec, &path_dirs) {
                    continue;
                }
            }
            entries.push(AutostartEntry { desktop_id });
        }
    }
    entries
}

/// Default autostart directories: user config first, then system dirs.
/// The user dir honours `$XDG_CONFIG_HOME` (absolute paths only;
/// anything else falls back to `~/.config`, per the base-directory
/// spec), then `$XDG_CONFIG_DIRS` or `/etc/xdg`.
pub fn default_autostart_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let user_config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|h| h.join(".config"))
        });
    if let Some(config) = user_config {
        dirs.push(config.join("autostart"));
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_DIRS") {
        for dir in std::env::split_paths(&xdg) {
            dirs.push(dir.join("autostart"));
        }
    } else {
        dirs.push(PathBuf::from("/etc/xdg/autostart"));
    }
    dirs
}

/// Merge autostart entries into a restore plan: autostart launches go
/// first on the active workspace with default (unmaximized, visible)
/// placement, and entries whose app is already restored are not
/// launched twice — the saved placement wins.
pub fn merge_autostart(
    plan: Vec<RestoreEntry>,
    autostart: &[AutostartEntry],
    active_workspace: u32,
) -> Vec<RestoreEntry> {
    let restored: HashSet<&str> = plan
        .iter()
        .filter_map(|entry| match entry {
            RestoreEntry::Launch(req) => Some(req.desktop_id.as_str()),
            RestoreEntry::Skipped { .. } => None,
        })
        .collect();
    let mut merged: Vec<RestoreEntry> = autostart
        .iter()
        .filter(|entry| !restored.contains(entry.desktop_id.as_str()))
        .map(|entry| {
            RestoreEntry::Launch(LaunchRequest {
                desktop_id: entry.desktop_id.clone(),
                app_id: None,
                tag: None,
                placement: WindowPlacement {
                    workspace: active_workspace.min(MAX_WORKSPACE),
                    x: 0,
                    y: 0,
                    width: 800,
                    height: 600,
                    maximized: false,
                    fullscreen: false,
                    minimized: false,
                    sticky: false,
                },
                from_autostart: true,
            })
        })
        .collect();
    merged.extend(plan);
    merged
}

/// Live `RestoreSupported` flag: the shell's opt-in preference,
/// mirrored here so the D-Bus facade answers truthfully.
pub type RestoreSupported = Arc<AtomicBool>;

/// Restore reason codes from gnome-session's `RegisterRestore`
/// contract (verified against upstream's interface XML).
pub mod reason {
    /// The app should not restore any state (restore disabled).
    pub const NO_RESTORE: u32 = 0;
    /// Normal launch: restore reasonable state (mode, not history).
    pub const NORMAL_LAUNCH: u32 = 1;
    /// Post-crash restore: restore as much as possible, carefully.
    pub const CRASH_RESTORE: u32 = 2;
    /// Normal restore after logout/shutdown: restore everything.
    pub const NORMAL_RESTORE: u32 = 3;
}

/// One registered app instance: the id persists in the registry
/// for the session, the bus name is kept for crash monitoring (like
/// `gsm-session-save.c`'s `SavedAppInstance`, minus the watch id —
/// the single `NameOwnerChanged` subscription in
/// [`serve_session_manager`] feeds every record through
/// [`RestoreRegistry::note_name_owner_changed`]).
#[derive(Debug, Clone)]
struct InstanceRecord {
    /// Instance id handed to the app; reused across re-registration.
    id: String,
    /// The app's bus name while it runs; `None` once it exits or
    /// crashes, which is what makes the id reusable.
    dbus_name: Option<String>,
    /// The bus name vanished without an unregister: a crash.
    crashed: bool,
}

/// One app's instance bookkeeping, mirroring `SavedApp` in
/// `gsm-session-save.c`: live instances, ids the app should delete,
/// and the id the next registration reuses.
#[derive(Debug, Default)]
struct AppRecord {
    instances: Vec<InstanceRecord>,
    discarded: Vec<String>,
    next_id: Option<String>,
}

/// Live instance registry behind the `RegisterRestore` family.
/// Tracks which apps asked for restore, what instance ids they hold,
/// and which ids are stale and should be deleted — the bookkeeping
/// gnome-session does between the portal and the apps. Instance ids
/// persist in the registry for the session and are reused across
/// re-registration; cleanup ids are discarded ids only, never
/// currently-live instance ids.
#[derive(Debug)]
pub struct RestoreRegistry {
    supported: RestoreSupported,
    /// Desktop ids present in the on-disk snapshot at login.
    saved: HashSet<String>,
    /// Desktop ids that crashed last session.
    crashed: HashSet<String>,
    /// Instance bookkeeping per desktop id.
    apps: HashMap<String, AppRecord>,
    /// Per-session token baked into issued instance ids so they can
    /// never collide with another session manager's ids.
    token: String,
    next_instance: u64,
}

impl RestoreRegistry {
    /// Registry over an explicit saved/crashed set (tests and tools).
    pub fn new(
        supported: RestoreSupported,
        saved: HashSet<String>,
        crashed: HashSet<String>,
    ) -> Self {
        Self {
            supported,
            saved,
            crashed,
            apps: HashMap::new(),
            token: Self::session_token(),
            next_instance: 0,
        }
    }

    /// Registry seeded from the on-disk snapshot: the login-time
    /// state. Missing or unreadable snapshots seed an empty set —
    /// every app then gets a normal launch, never a failure.
    pub fn from_disk(supported: RestoreSupported) -> Self {
        let saved = match load_from(&session_file_path()) {
            LoadOutcome::Loaded(session) => session
                .windows
                .iter()
                .map(|w| w.desktop_id.clone())
                .collect(),
            _ => HashSet::new(),
        };
        Self::new(supported, saved, HashSet::new())
    }

    /// Per-process session token: pid plus start-time nanos, so ids
    /// never collide across sessions or managers.
    fn session_token() -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!("{:x}-{:x}", std::process::id(), nanos & 0xffff_ffff_ffff)
    }

    /// Refresh the saved/crashed sets (the runtime calls this after a
    /// save or when the supervisor reports a crash).
    pub fn refresh(&mut self, saved: HashSet<String>, crashed: HashSet<String>) {
        self.saved = saved;
        self.crashed = crashed;
    }

    /// Record a crash without a full refresh.
    pub fn note_crashed(&mut self, desktop_id: &str) {
        if let Some(id) = normalize_desktop_id(desktop_id) {
            self.crashed.insert(id);
        }
    }

    /// Crash-monitor hook for the facade's `NameOwnerChanged`
    /// subscription: when `name` loses its owner (`new_owner` empty),
    /// every live instance registered under it is marked crashed and
    /// its bus name cleared, exactly like `gsm-session-save.c`'s
    /// `on_instance_vanished`. Returns how many instances were marked.
    /// A re-registering app then reuses its prior instance id with a
    /// crash reason instead of minting a fresh one.
    pub fn note_name_owner_changed(&mut self, name: &str, new_owner: &str) -> usize {
        if name.is_empty() || !new_owner.is_empty() {
            return 0;
        }
        let mut marked = 0;
        let mut crashed_apps = Vec::new();
        for (desktop_id, app) in self.apps.iter_mut() {
            for instance in app.instances.iter_mut() {
                if instance.dbus_name.as_deref() == Some(name) {
                    instance.dbus_name = None;
                    instance.crashed = true;
                    crashed_apps.push(desktop_id.clone());
                    marked += 1;
                }
            }
        }
        for desktop_id in crashed_apps {
            self.crashed.insert(desktop_id);
        }
        marked
    }

    /// The restore reason one app gets: crashed beats saved (a
    /// crashing app must restore carefully even when a snapshot names
    /// it), saved beats unknown, disabled restores nothing.
    fn reason_for(&self, desktop_id: &str) -> u32 {
        if !self.supported.load(Ordering::SeqCst) {
            return reason::NO_RESTORE;
        }
        if self.crashed.contains(desktop_id) {
            return reason::CRASH_RESTORE;
        }
        if self.saved.contains(desktop_id) {
            return reason::NORMAL_RESTORE;
        }
        reason::NORMAL_LAUNCH
    }

    /// A fresh instance id for `desktop_id`: the session token plus
    /// a per-registry counter, so ids never collide across sessions
    /// or managers (the XML's no-collision promise). Preferring a
    /// cleanly-exited instance's leftover id (GNOME's `next_id`) is
    /// the caller's job — see [`RestoreRegistry::register_with_sender`].
    fn fresh_instance_id(token: &str, n: u64, desktop_id: &str) -> String {
        let app_part = session_id_for(desktop_id, None);
        let app_part = app_part.trim_start_matches("tuna-");
        format!("tuna-{token}-{app_part}-{n}")
    }

    /// Register one app for restore: reuse a prior instance id when
    /// one is not currently running, otherwise mint one, and report
    /// the app's discarded ids for cleanup. Cleanup ids are only
    /// ever discarded ids — a currently-live instance id is never
    /// sent for cleanup. An empty `dbus_name` falls back to `sender`
    /// (the D-Bus caller, like `gsm-manager.c`); empty app ids, and
    /// an empty bus name with no sender, are rejected. The bus name
    /// is kept for crash monitoring but not trusted for anything else.
    pub fn register_with_sender(
        &mut self,
        app_id: &str,
        dbus_name: &str,
        sender: Option<&str>,
    ) -> Result<(u32, String, Vec<String>), String> {
        let desktop_id = normalize_desktop_id(app_id)
            .ok_or_else(|| "RegisterRestore needs a non-empty app id".to_owned())?;
        let bus_name = (!dbus_name.is_empty())
            .then(|| dbus_name.to_owned())
            .or_else(|| sender.filter(|s| !s.is_empty()).map(str::to_owned))
            .ok_or_else(|| "RegisterRestore needs the app's bus name".to_owned())?;
        let reason = self.reason_for(&desktop_id);
        // Reuse a prior instance id when one is not running (GNOME's
        // "check if the instance is running" scan): a cleanly-exited
        // or crashed instance keeps its id, so the app's saved state
        // stays addressable under the same id.
        if let Some(app) = self.apps.get_mut(&desktop_id) {
            if let Some(instance) = app.instances.iter_mut().find(|i| i.dbus_name.is_none()) {
                instance.dbus_name = Some(bus_name);
                instance.crashed = false;
                let id = instance.id.clone();
                let cleanup_ids = app.discarded.clone();
                return Ok((reason, id, cleanup_ids));
            }
        }
        // No reusable instance: prefer the id a cleanly-exited
        // instance left behind (GNOME's `next_id`), else mint fresh.
        let leftover = self
            .apps
            .get_mut(&desktop_id)
            .and_then(|app| app.next_id.take());
        let instance_id = match leftover {
            Some(reuse) => reuse,
            None => {
                self.next_instance += 1;
                let n = self.next_instance;
                Self::fresh_instance_id(&self.token, n, &desktop_id)
            }
        };
        let app = self.apps.entry(desktop_id).or_default();
        let cleanup_ids = app.discarded.clone();
        app.instances.push(InstanceRecord {
            id: instance_id.clone(),
            dbus_name: Some(bus_name),
            crashed: false,
        });
        Ok((reason, instance_id, cleanup_ids))
    }

    /// Register one app for restore without a D-Bus caller to fall
    /// back to: an empty `dbus_name` is rejected. See
    /// [`RestoreRegistry::register_with_sender`].
    pub fn register(
        &mut self,
        app_id: &str,
        dbus_name: &str,
    ) -> Result<(u32, String, Vec<String>), String> {
        self.register_with_sender(app_id, dbus_name, None)
    }

    /// Forget discarded ids the app confirms it deleted. Live ids
    /// are never forgotten here (they were never sent for cleanup);
    /// unknown ids are ignored. The return is how many discarded ids
    /// were actually held.
    pub fn deleted(&mut self, app_id: &str, ids: &[String]) -> usize {
        let Some(desktop_id) = normalize_desktop_id(app_id) else {
            return 0;
        };
        let Some(app) = self.apps.get_mut(&desktop_id) else {
            return 0;
        };
        let before = app.discarded.len();
        app.discarded.retain(|id| !ids.contains(id));
        before - app.discarded.len()
    }

    /// Drop one registration when the app exits cleanly. The id moves
    /// to the discard path (GNOME's `saved_app_discard_instance`):
    /// the next registration reuses it, and any id it displaces is
    /// reported for cleanup. Unknown apps and ids return false; the
    /// D-Bus layer reports those as `NotRegistered`.
    pub fn unregister(&mut self, app_id: &str, instance_id: &str) -> bool {
        let Some(desktop_id) = normalize_desktop_id(app_id) else {
            return false;
        };
        let Some(app) = self.apps.get_mut(&desktop_id) else {
            return false;
        };
        let Some(pos) = app.instances.iter().position(|i| i.id == instance_id) else {
            return false;
        };
        app.instances.remove(pos);
        if let Some(previous) = app.next_id.replace(instance_id.to_owned()) {
            app.discarded.push(previous);
        }
        true
    }
}

/// D-Bus errors for the restore facade, named like upstream's
/// `GSM_MANAGER_ERROR` domain.
#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.gnome.SessionManager.Error")]
enum RestoreDbusError {
    /// `gsm_manager_unregister_restore` answers an unknown instance
    /// with `GSM_MANAGER_ERROR_NOT_REGISTERED` ("Provided instance
    /// not found"); so do we.
    NotRegistered(String),
    /// The registry lock was poisoned: an internal failure, never a
    /// client error.
    Internal(String),
}

struct SessionManagerNode {
    registry: Arc<std::sync::Mutex<RestoreRegistry>>,
}

#[zbus::interface(name = "org.gnome.SessionManager")]
impl SessionManagerNode {
    /// Whether this session supports opt-in window restoration.
    /// This is the property GNOME Settings' multitasking panel
    /// checks; full gnome-session method parity is out of scope.
    #[zbus(property)]
    fn restore_supported(&self) -> bool {
        self.registry
            .lock()
            .map(|r| r.supported.load(Ordering::SeqCst))
            .unwrap_or(false)
    }

    /// Register the caller for save/restore: returns the restore
    /// reason, an instance id (reused across the app's restarts when
    /// possible), and discarded instance ids the app should delete.
    /// An empty `dbus_name` falls back to the caller's bus name,
    /// like `gsm-manager.c`. Signature and semantics match
    /// gnome-session.
    fn register_restore(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        app_id: &str,
        dbus_name: &str,
    ) -> zbus::fdo::Result<(u32, String, Vec<String>)> {
        let sender = hdr.sender().map(|s| s.to_string());
        self.registry
            .lock()
            .map_err(|_| zbus::fdo::Error::Failed("restore registry poisoned".into()))?
            .register_with_sender(app_id, dbus_name, sender.as_deref())
            .map_err(zbus::fdo::Error::InvalidArgs)
    }

    /// The app deleted these discarded instance ids; forget them for
    /// good.
    fn deleted_instance_ids(&self, app_id: &str, ids: Vec<String>) {
        if let Ok(mut registry) = self.registry.lock() {
            registry.deleted(app_id, &ids);
        }
    }

    /// The app exited cleanly; drop its registration. An unknown app
    /// or instance id is `NotRegistered`, like upstream.
    fn unregister_restore(&self, app_id: &str, instance_id: &str) -> Result<(), RestoreDbusError> {
        let mut registry = self
            .registry
            .lock()
            .map_err(|_| RestoreDbusError::Internal("restore registry poisoned".into()))?;
        if registry.unregister(app_id, instance_id) {
            Ok(())
        } else {
            Err(RestoreDbusError::NotRegistered(
                "Provided instance not found".into(),
            ))
        }
    }
}

/// Subscribe to `NameOwnerChanged` and feed owner losses into the
/// registry's crash monitor for the life of the connection (like
/// `gsm-session-save.c`'s per-instance `g_bus_watch_name`, but with
/// one subscription for every record). A failed subscription degrades
/// to no crash monitoring — the facade keeps serving — and is logged.
fn watch_names_for_crash(
    conn: &zbus::blocking::Connection,
    registry: &Arc<std::sync::Mutex<RestoreRegistry>>,
) {
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender("org.freedesktop.DBus")
        .and_then(|b| b.interface("org.freedesktop.DBus"))
        .and_then(|b| b.member("NameOwnerChanged"))
        .and_then(|b| b.path("/org/freedesktop/DBus"))
        .map(|b| b.build());
    let rule = match rule {
        Ok(rule) => rule,
        Err(e) => {
            eprintln!("tuna-compositor: session manager: no crash watch ({e}); serving anyway");
            loop {
                std::thread::park();
            }
        }
    };
    let iter = match zbus::blocking::MessageIterator::for_match_rule(rule, conn, Some(32)) {
        Ok(iter) => iter,
        Err(e) => {
            eprintln!("tuna-compositor: session manager: no crash watch ({e}); serving anyway");
            loop {
                std::thread::park();
            }
        }
    };
    for msg in iter {
        let Ok(msg) = msg else {
            continue;
        };
        let Ok((name, _old_owner, new_owner)) =
            msg.body().deserialize::<(String, String, String)>()
        else {
            continue;
        };
        if let Ok(mut registry) = registry.lock() {
            registry.note_name_owner_changed(&name, &new_owner);
        }
    }
    eprintln!("tuna-compositor: session manager: bus gone, facade down");
}

/// Serve the session-manager restore facade on the session bus from a
/// thread, with a `NameOwnerChanged` crash watch over the registered
/// bus names. When a real session manager owns the name (a nested run
/// inside GNOME), the service stays off, like the Mutter facade.
/// Hook: the runtime calls this once at startup with its live flag
/// (see [`startup_session_manager`).
/// Hardening note (union time): upstream treats `RegisterRestore` as
/// a private portal interface; a caller-identity check against
/// `org.freedesktop.portal.Desktop` belongs here before trusting
/// `dbus_name` for crash monitoring.
pub fn serve_session_manager(supported: RestoreSupported) {
    let registry = Arc::new(std::sync::Mutex::new(RestoreRegistry::from_disk(supported)));
    let node = SessionManagerNode {
        registry: registry.clone(),
    };
    let _ = std::thread::Builder::new()
        .name("tuna-session-manager".into())
        .spawn(move || {
            let conn = match zbus::blocking::connection::Builder::session()
                .and_then(|b| b.serve_at(SESSION_MANAGER_PATH, node))
                .and_then(|b| b.build())
            {
                Ok(conn) => conn,
                Err(e) => {
                    eprintln!("tuna-compositor: session manager: no session bus: {e}");
                    return;
                }
            };
            let flags = zbus::fdo::RequestNameFlags::DoNotQueue.into();
            match conn.request_name_with_flags(SESSION_MANAGER_NAME, flags) {
                Ok(zbus::fdo::RequestNameReply::PrimaryOwner) => {
                    eprintln!("tuna-compositor: serving {SESSION_MANAGER_NAME}");
                }
                _ => {
                    eprintln!("tuna-compositor: {SESSION_MANAGER_NAME} is taken");
                    return;
                }
            }
            watch_names_for_crash(&conn, &registry);
        });
}

/// Startup entry point: create the live opt-in flag (off until the
/// shell's preference UI turns it on), serve the restore facade, and
/// hand the flag back. The runtime calls this once at startup; the
/// shell pushes its persisted preference into the flag.
pub fn startup_session_manager() -> RestoreSupported {
    let supported: RestoreSupported = Arc::new(AtomicBool::new(false));
    serve_session_manager(supported.clone());
    supported
}

/// Directories searched for installed applications:
/// `$XDG_DATA_HOME/applications` (absolute only, else
/// `~/.local/share/applications`), then each
/// `$XDG_DATA_DIRS/applications` (default
/// `/usr/local/share:/usr/share`), per the base-directory spec.
pub fn application_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let user_data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|h| h.join(".local/share"))
        });
    if let Some(data) = user_data {
        dirs.push(data.join("applications"));
    }
    match std::env::var_os("XDG_DATA_DIRS") {
        Some(list) => {
            for dir in std::env::split_paths(&list) {
                dirs.push(dir.join("applications"));
            }
        }
        None => {
            dirs.push(PathBuf::from("/usr/local/share/applications"));
            dirs.push(PathBuf::from("/usr/share/applications"));
        }
    }
    dirs
}

/// Resolve a desktop id to its installed `.desktop` file, or `None`
/// when no application dir carries it. Ids containing path
/// separators are rejected outright (desktop ids are bare names —
/// never trust a snapshot into a path traversal).
pub fn resolve_desktop_file(desktop_id: &str) -> Option<PathBuf> {
    let id = normalize_desktop_id(desktop_id)?;
    if id.contains('/') || id.contains('\0') {
        return None;
    }
    let file_name = format!("{id}.desktop");
    application_dirs()
        .iter()
        .map(|dir| dir.join(&file_name))
        .find(|path| path.is_file())
}

/// Desktop ids installed anywhere in [`application_dirs`]: the
/// `installed` set [`login_restore_plan`] plans against. The scan is
/// capped so a hostile applications directory cannot balloon memory.
pub fn installed_apps() -> HashSet<String> {
    const MAX_APPS: usize = 4096;
    let mut apps = HashSet::new();
    for dir in application_dirs() {
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in read.filter_map(|e| e.ok()) {
            if apps.len() >= MAX_APPS {
                return apps;
            }
            let path = entry.path();
            if !path.extension().is_some_and(|ext| ext == "desktop") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if let Some(id) = normalize_desktop_id(stem) {
                if !id.contains('/') {
                    apps.insert(id);
                }
            }
        }
    }
    apps
}

/// Expand an `Exec=` line into argv, per the Desktop Entry Spec.
/// Quoting and backslash escapes are honoured (single quotes are
/// *not* special in `Exec=`, unlike a shell); field codes expand as:
/// `%f %F %u %U` dropped (a restore launch opens no files),
/// deprecated `%d %D %n %N %v %m` dropped, `%i` becomes
/// `--icon <Icon>`, `%c` the entry name, `%k` the desktop file path,
/// `%%` a literal `%`. Unknown codes are dropped.
fn expand_exec(exec: &str, name: &str, icon: Option<&str>, desktop_path: &Path) -> Vec<String> {
    // Tokenize on unquoted whitespace first.
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_token = false;
    let mut in_quotes = false;
    let mut chars = exec.chars();
    while let Some(ch) = chars.next() {
        if in_quotes {
            match ch {
                '"' => in_quotes = false,
                '\\' => {
                    if let Some(next) = chars.next() {
                        current.push(next);
                    }
                }
                _ => current.push(ch),
            }
        } else {
            match ch {
                '"' => {
                    in_quotes = true;
                    in_token = true;
                }
                '\\' => {
                    in_token = true;
                    if let Some(next) = chars.next() {
                        current.push(next);
                    }
                }
                _ if ch.is_whitespace() => {
                    if in_token {
                        tokens.push(std::mem::take(&mut current));
                        in_token = false;
                    }
                }
                _ => {
                    in_token = true;
                    current.push(ch);
                }
            }
        }
    }
    if in_token {
        tokens.push(current);
    }
    // Then expand field codes per token.
    let mut argv = Vec::new();
    for token in tokens {
        if token == "%i" {
            if let Some(icon) = icon.filter(|i| !i.is_empty()) {
                argv.push("--icon".to_owned());
                argv.push(icon.to_owned());
            }
            continue;
        }
        let mut expanded = String::new();
        let mut chars = token.chars();
        while let Some(ch) = chars.next() {
            if ch != '%' {
                expanded.push(ch);
                continue;
            }
            match chars.next() {
                Some('%') => expanded.push('%'),
                Some('c') => expanded.push_str(name),
                Some('k') => expanded.push_str(&desktop_path.to_string_lossy()),
                Some('i') => {
                    if let Some(icon) = icon {
                        expanded.push_str(icon);
                    }
                }
                // %f %F %u %U, deprecated codes, and unknown codes:
                // dropped (a restore launch carries no files).
                Some(_) | None => {}
            }
        }
        if !expanded.is_empty() {
            argv.push(expanded);
        }
    }
    argv
}

/// Queue of launched restore children awaiting reaping, drained by
/// one background thread so relaunched apps never linger as zombies.
/// (A `SIGCHLD`/`SA_NOCLDWAIT` shortcut is out of the question: the
/// supervisor owns `waitpid` for the shell child and must keep it.)
fn reaper() -> &'static std::sync::mpsc::Sender<std::process::Child> {
    static REAPER: OnceLock<std::sync::mpsc::Sender<std::process::Child>> = OnceLock::new();
    REAPER.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<std::process::Child>();
        let _ = std::thread::Builder::new()
            .name("tuna-restore-reaper".into())
            .spawn(move || {
                let mut kids = Vec::new();
                loop {
                    while let Ok(kid) = rx.try_recv() {
                        kids.push(kid);
                    }
                    kids.retain_mut(|kid| matches!(kid.try_wait(), Ok(None)));
                    match rx.recv_timeout(Duration::from_secs(1)) {
                        Ok(kid) => kids.push(kid),
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                    }
                }
            });
        tx
    })
}

/// Launch one installed application by desktop id: resolve the id to
/// its `.desktop` file at restore time (like `gsm-util`'s
/// `GDesktopAppInfo` launch, and like the user's own launch), honour
/// `TryExec=` and refuse `Hidden` / non-`Application` / terminal
/// entries, then spawn detached in its own session with no stdio
/// inheritance. The child is handed to the reaper so it never
/// zombifies.
pub fn launch_desktop_id(desktop_id: &str) -> io::Result<()> {
    let path = resolve_desktop_file(desktop_id).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("{desktop_id}: no such installed application"),
        )
    })?;
    let text = std::fs::read_to_string(&path)?;
    let keys = desktop_entry_keys(&text);
    if keys
        .get("Hidden")
        .is_some_and(|v| v.eq_ignore_ascii_case("true"))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{desktop_id}: entry is Hidden"),
        ));
    }
    if !keys.get("Type").is_some_and(|t| t == "Application") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{desktop_id}: not a Type=Application entry"),
        ));
    }
    let exec = keys
        .get("Exec")
        .filter(|e| !e.trim().is_empty())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{desktop_id}: entry has no Exec="),
            )
        })?;
    if let Some(try_exec) = keys.get("TryExec") {
        let path_dirs: Vec<PathBuf> = std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).collect())
            .unwrap_or_default();
        if !try_exec_ok(try_exec, &path_dirs) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("{desktop_id}: TryExec target is missing"),
            ));
        }
    }
    if keys
        .get("Terminal")
        .is_some_and(|v| v.eq_ignore_ascii_case("true"))
    {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("{desktop_id}: terminal apps need a terminal emulator"),
        ));
    }
    let name = keys.get("Name").map(String::as_str).unwrap_or(desktop_id);
    let argv = expand_exec(exec, name, keys.get("Icon").map(String::as_str), &path);
    if argv.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{desktop_id}: Exec= expands to nothing"),
        ));
    }
    let mut command = std::process::Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if let Some(dir) = keys.get("Path").filter(|d| Path::new(d).is_dir()) {
        command.current_dir(dir);
    }
    // Detach into a new session so the app outlives restore-time
    // bookkeeping and never takes our signals. A freshly forked
    // child cannot be a process-group leader, so `setsid` cannot
    // fail here — but report it honestly if it ever does.
    // SAFETY: `setsid` is async-signal-safe; nothing else runs
    // between fork and exec.
    unsafe {
        use std::os::unix::process::CommandExt as _;
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command.spawn()?;
    // The reaper lives for the process; a failed handoff leaks one
    // zombie at worst, never the launch itself.
    let _ = reaper().send(child);
    Ok(())
}

/// One restore launch and what came of it. A failed launch never
/// aborts the plan: the runtime logs it and carries on.
#[derive(Debug)]
pub struct LaunchAttempt {
    /// Desktop id that was launched.
    pub desktop_id: String,
    /// The launch came from XDG autostart rather than the snapshot.
    pub from_autostart: bool,
    /// The launcher's verdict.
    pub result: io::Result<()>,
}

/// Run every [`RestoreEntry::Launch`] in a plan through `launcher`
/// (production passes [`launch_desktop_id`); skipped entries produce
/// no attempt. Launches run in plan order: autostart first, then the
/// snapshot in saved order.
pub fn launch_plan(
    entries: &[RestoreEntry],
    launcher: &dyn Fn(&LaunchRequest) -> io::Result<()>,
) -> Vec<LaunchAttempt> {
    entries
        .iter()
        .filter_map(|entry| match entry {
            RestoreEntry::Launch(req) => Some(LaunchAttempt {
                desktop_id: req.desktop_id.clone(),
                from_autostart: req.from_autostart,
                result: launcher(req),
            }),
            RestoreEntry::Skipped { .. } => None,
        })
        .collect()
}

/// Logout/shutdown entry point: collect the live rows into a snapshot
/// and save it at [`session_file_path`] when `prefs` opts in. The
/// runtime calls this on its shutdown path with one [`WindowRow`] per
/// mapped toplevel.
pub fn logout_save(
    rows: impl IntoIterator<Item = WindowRow>,
    active_workspace: u32,
    prefs: RestorePrefs,
) -> io::Result<SaveOutcome> {
    save_if_enabled(
        &session_file_path(),
        prefs,
        &collect(rows, active_workspace),
    )
}

/// One login's restore work: the merged launch entries plus the
/// placement queue for windows as they map.
#[derive(Debug)]
pub struct LoginRestore {
    /// What to launch and what to skip, with reasons.
    pub entries: Vec<RestoreEntry>,
    /// Workspace that was active at save time (0 without a snapshot).
    pub active_workspace: u32,
    /// Queued placements for mapping windows (see
    /// [`PendingPlacements`]).
    pub placements: PendingPlacements,
}

/// Login entry point: load the snapshot at [`session_file_path`],
/// plan the restore against [`installed_apps`] and the previous
/// session's crash set (`crashed`), merge [`scan_autostart`], and
/// queue the launch entries' placements. The snapshot restores only
/// when `prefs` opts in; autostart launches at every login whatever
/// the preference, like GNOME.
pub fn login_restore_plan(prefs: RestorePrefs, crashed: &HashSet<String>) -> LoginRestore {
    let installed = installed_apps();
    let (snapshot, active_workspace) = match load_from(&session_file_path()) {
        LoadOutcome::Loaded(session) => {
            let active = session.active_workspace.min(MAX_WORKSPACE);
            (Some(session), active)
        }
        LoadOutcome::Absent | LoadOutcome::Ignored(_) => (None, 0),
    };
    let plan = match (prefs.enabled, snapshot) {
        (true, Some(session)) => plan_restore(&session, &|id| installed.contains(id), crashed),
        _ => Vec::new(),
    };
    let autostart = scan_autostart(&default_autostart_dirs());
    let entries = merge_autostart(plan, &autostart, active_workspace);
    let mut placements = PendingPlacements::new();
    placements.push_all(&entries);
    LoginRestore {
        entries,
        active_workspace,
        placements,
    }
}

/// Placements waiting for relaunched windows to map, queued by
/// [`login_restore_plan`]. The compositor pops each mapped window's
/// placement with [`PendingPlacements::take_for`] and applies it with
/// [`apply_restored_placement`].
#[derive(Debug, Clone)]
struct PendingPlacement {
    desktop_id: String,
    app_id: Option<String>,
    tag: Option<String>,
    placement: WindowPlacement,
}

/// FIFO queue matching mapped windows back to their saved placement.
#[derive(Debug, Default)]
pub struct PendingPlacements {
    queue: VecDeque<PendingPlacement>,
}

impl PendingPlacements {
    /// Empty queue.
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue every launch entry's placement, in plan order.
    pub fn push_all(&mut self, entries: &[RestoreEntry]) {
        for entry in entries {
            if let RestoreEntry::Launch(req) = entry {
                self.queue.push_back(PendingPlacement {
                    desktop_id: req.desktop_id.clone(),
                    app_id: req.app_id.clone(),
                    tag: req.tag.clone(),
                    placement: req.placement,
                });
            }
        }
    }

    /// Queued placements.
    pub fn len(&self) -> usize {
        self.queue.len()
    }

    /// Whether any placement is still queued.
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// Pop the placement for a mapping window: an exact toplevel-tag
    /// match wins (tags disambiguate an app's several windows), else
    /// the first app-id match (falling back to the desktop id, since
    /// clients often set the app id to it), FIFO. Returns `None` for
    /// windows no restore launched — they place normally.
    pub fn take_for(&mut self, app_id: Option<&str>, tag: Option<&str>) -> Option<WindowPlacement> {
        if let Some(tag) = tag.map(str::trim).filter(|t| !t.is_empty()) {
            if let Some(pos) = self
                .queue
                .iter()
                .position(|p| p.tag.as_deref().is_some_and(|t| t == tag))
            {
                return self.queue.remove(pos).map(|p| p.placement);
            }
        }
        if let Some(app) = app_id.map(str::trim).filter(|a| !a.is_empty()) {
            let normalized = normalize_desktop_id(app);
            if let Some(pos) = self.queue.iter().position(|p| {
                p.app_id.as_deref() == Some(app)
                    || p.desktop_id == app
                    || normalized.as_deref().is_some_and(|n| p.desktop_id == n)
            }) {
                return self.queue.remove(pos).map(|p| p.placement);
            }
        }
        None
    }
}

/// What [`apply_restored_placement`] managed to apply. Steps report
/// false for unknown ids (the window closed mid-restore) rather than
/// failing the whole restore.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppliedPlacement {
    /// The window moved to its saved workspace.
    pub workspace: bool,
    /// Floating size and position were applied.
    pub geometry: bool,
    /// Maximize / fullscreen / minimize were applied.
    pub layout: bool,
}

/// Map-time entry point: apply a saved [`WindowPlacement`] to a just
/// mapped window. Workspace first, then floating geometry (resize
/// plus a delta move from the current position — there is no absolute
/// setter, and the delta from
/// [`WindowManager::geometry`](crate::windows::WindowManager::geometry)
/// is exact),
/// then layout flags last so maximize/fullscreen stash the floating
/// geometry they cover, exactly as a user's own toggles would.
/// (`sticky` has no `WindowManager` setter — only `is_sticky` — so it
/// cannot be restored yet; see the union patch notes.)
pub fn apply_restored_placement(
    manager: &mut crate::windows::WindowManager,
    state: &mut crate::State,
    id: u64,
    placement: &WindowPlacement,
) -> AppliedPlacement {
    let workspace = manager.move_to_workspace(state, id, placement.workspace.min(MAX_WORKSPACE));
    let geometry = match manager.geometry(id) {
        Some(geo) => {
            let resized = manager.resize_window(id, placement.width, placement.height);
            let moved = manager.move_window(
                id,
                placement.x.saturating_sub(geo.loc.x),
                placement.y.saturating_sub(geo.loc.y),
            );
            resized && moved
        }
        None => false,
    };
    let mut layout = true;
    if placement.fullscreen {
        layout &= manager.set_fullscreen(state, id, true);
    } else if placement.maximized {
        layout &= manager.set_maximized(state, id, true);
    }
    if placement.minimized {
        layout &= manager.minimize(state, id);
    }
    AppliedPlacement {
        workspace,
        geometry,
        layout,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    /// Serializes tests that mutate process-global XDG variables and
    /// restores every variable it touched on drop. The harness runs
    /// tests on threads in one process, so unsynchronized `set_var`
    /// in one test can leak into another test's reads.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct EnvGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        saved: Vec<(&'static str, Option<OsString>)>,
    }

    impl EnvGuard {
        fn set(vars: &[(&'static str, Option<&str>)]) -> Self {
            let lock = ENV_LOCK.lock().expect("env lock");
            let mut saved = Vec::new();
            for (key, value) in vars {
                saved.push((*key, std::env::var_os(key)));
                match value {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
            Self { _lock: lock, saved }
        }

        /// Isolated XDG world: state/config/data all under `root`,
        /// system dirs emptied so the host cannot leak in.
        fn isolated(root: &Path) -> Self {
            let root = root.to_string_lossy().into_owned();
            Self::set(&[
                ("XDG_STATE_HOME", Some(&format!("{root}/state"))),
                ("XDG_CONFIG_HOME", Some(&format!("{root}/config"))),
                ("XDG_CONFIG_DIRS", Some("")),
                ("XDG_DATA_HOME", Some(&format!("{root}/data"))),
                ("XDG_DATA_DIRS", Some("")),
            ])
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, value) in self.saved.drain(..) {
                match value {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    fn placement(workspace: u32) -> WindowPlacement {
        WindowPlacement {
            workspace,
            x: 10,
            y: 20,
            width: 800,
            height: 600,
            maximized: false,
            fullscreen: false,
            minimized: false,
            sticky: false,
        }
    }

    fn row(desktop_id: &str, workspace: u32) -> WindowRow {
        WindowRow {
            desktop_id: Some(desktop_id.into()),
            app_id: Some(desktop_id.into()),
            title: format!("{desktop_id} window"),
            tag: None,
            placement: placement(workspace),
        }
    }

    fn tmp_file(name: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join(name);
        (dir, path)
    }

    #[test]
    fn restore_is_opt_in_by_default() {
        assert!(!RestorePrefs::default().enabled);
        assert!(RestorePrefs::enabled().enabled);
    }

    #[test]
    fn disabled_save_writes_nothing() {
        let (_dir, path) = tmp_file("session.json");
        let session = collect([row("org.gnome.TextEditor", 1)], 1);
        let outcome = save_if_enabled(&path, RestorePrefs::default(), &session).unwrap();
        assert_eq!(outcome, SaveOutcome::SkippedDisabled);
        assert!(!path.exists());
    }

    #[test]
    fn roundtrip_preserves_workspace_geometry_and_layout() {
        let (_dir, path) = tmp_file("session.json");
        let mut max = row("org.gnome.TextEditor", 2);
        max.placement.maximized = true;
        max.tag = Some("main window".into());
        let mut mini = row("org.gnome.Calculator", 0);
        mini.placement.minimized = true;
        mini.placement.x = -5;
        let session = collect([max, mini], 2);
        let outcome = save_if_enabled(&path, RestorePrefs::enabled(), &session).unwrap();
        assert_eq!(outcome, SaveOutcome::Saved);
        // Atomic write leaves no temporary file behind.
        assert!(!path.with_extension("json.tmp").exists());
        match load_from(&path) {
            LoadOutcome::Loaded(back) => {
                assert_eq!(back.version, SCHEMA_VERSION);
                assert_eq!(back.active_workspace, 2);
                assert_eq!(back.windows.len(), 2);
                assert_eq!(back.windows[0].desktop_id, "org.gnome.TextEditor");
                assert_eq!(back.windows[0].workspace, 2);
                assert!(back.windows[0].maximized);
                assert_eq!(back.windows[0].tag.as_deref(), Some("main window"));
                assert_eq!((back.windows[0].x, back.windows[0].y), (10, 20));
                assert_eq!((back.windows[0].width, back.windows[0].height), (800, 600));
                assert!(back.windows[1].minimized);
                assert_eq!(back.windows[1].workspace, 0);
            }
            other => panic!("expected loaded snapshot, got {other:?}"),
        }
    }

    #[test]
    fn missing_file_loads_as_absent() {
        let (_dir, path) = tmp_file("session.json");
        assert_eq!(load_from(&path), LoadOutcome::Absent);
    }

    #[test]
    fn corrupt_file_is_ignored_and_quarantinable() {
        let (_dir, path) = tmp_file("session.json");
        std::fs::write(&path, b"{ not json").unwrap();
        assert!(matches!(
            load_from(&path),
            LoadOutcome::Ignored(IgnoreReason::Corrupt(_))
        ));
        std::fs::write(&path, b"[1, 2]").unwrap();
        assert!(matches!(
            load_from(&path),
            LoadOutcome::Ignored(IgnoreReason::Corrupt(_))
        ));
        let aside = quarantine_at(&path).unwrap().expect("quarantined");
        assert!(!path.exists());
        assert!(aside.exists());
        assert_eq!(load_from(&path), LoadOutcome::Absent);
    }

    #[test]
    fn newer_version_is_ignored_not_guessed() {
        let (_dir, path) = tmp_file("session.json");
        let mut session = collect([row("org.gnome.TextEditor", 0)], 0);
        session.version = SCHEMA_VERSION + 1;
        std::fs::write(&path, serde_json::to_string(&session).unwrap()).unwrap();
        assert_eq!(
            load_from(&path),
            LoadOutcome::Ignored(IgnoreReason::UnsupportedVersion(SCHEMA_VERSION + 1))
        );
    }

    #[test]
    fn disable_removes_only_the_session_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("session.json");
        let sibling = dir.path().join("other-user-data.json");
        std::fs::write(&sibling, b"{\"keep\": true}").unwrap();
        let session = collect([row("org.gnome.TextEditor", 0)], 0);
        save_to(&path, &session).unwrap();
        assert_eq!(clear_at(&path).unwrap(), ClearOutcome::Removed);
        assert!(!path.exists());
        assert_eq!(
            std::fs::read_to_string(&sibling).unwrap(),
            "{\"keep\": true}"
        );
        assert_eq!(clear_at(&path).unwrap(), ClearOutcome::Absent);
    }

    #[test]
    fn saved_state_carries_no_secrets() {
        let session = collect(
            [WindowRow {
                desktop_id: Some("org.gnome.TextEditor".into()),
                app_id: Some("org.gnome.TextEditor".into()),
                title: "notes.txt --password=hunter2 --token=abc".into(),
                tag: Some("main".into()),
                placement: placement(0),
            }],
            0,
        );
        let json = serialize(&session).unwrap();
        assert!(audit_json_keys(&json).is_empty());
        assert!(saved_state_has_no_secrets(&session));
        for forbidden in [
            "exec",
            "Exec",
            "env",
            "environment",
            "token",
            "password",
            "secret",
            "command",
            "commandline",
            "uri",
            "working",
            "cwd",
        ] {
            assert!(
                !json.contains(&format!("\"{forbidden}\"")),
                "snapshot must not carry {forbidden}"
            );
        }
        // The audit catches a smuggled key rather than passing silently.
        assert_eq!(audit_json_keys(r#"{"version":1,"exec":"x"}"#), ["exec"]);
    }

    #[test]
    fn missing_and_crashed_apps_are_skipped_with_reasons() {
        let session = collect(
            [
                row("org.gnome.TextEditor", 0),
                row("com.example.Gone", 1),
                row("org.gnome.Calculator", 0),
            ],
            0,
        );
        let installed = |id: &str| id != "com.example.Gone";
        let crashed: HashSet<String> = ["org.gnome.Calculator".to_owned()].into_iter().collect();
        let plan = plan_restore(&session, &installed, &crashed);
        assert_eq!(plan.len(), 3);
        assert!(matches!(
            &plan[0],
            RestoreEntry::Launch(req)
                if req.desktop_id == "org.gnome.TextEditor" && !req.from_autostart
        ));
        assert_eq!(
            plan[1],
            RestoreEntry::Skipped {
                desktop_id: "com.example.Gone".into(),
                reason: SkipReason::MissingApp,
            }
        );
        assert_eq!(
            plan[2],
            RestoreEntry::Skipped {
                desktop_id: "org.gnome.Calculator".into(),
                reason: SkipReason::Crashed,
            }
        );
        // Crashed entries stay in the file for the next clean login.
        assert!(session
            .windows
            .iter()
            .any(|w| w.desktop_id == "org.gnome.Calculator"));
    }

    #[test]
    fn invalid_placements_are_skipped_never_guessed() {
        let mut bad = row("org.gnome.TextEditor", 0);
        bad.placement.width = 0;
        let mut wild = row("org.gnome.Calculator", 0);
        wild.placement.workspace = MAX_WORKSPACE + 100;
        // Collect drops invalid rows at save time.
        let session = collect([bad, wild], 0);
        assert!(session.windows.is_empty());
        // And a hand-edited file with bad geometry skips at plan time.
        let mut session = collect([row("org.gnome.TextEditor", 0)], 0);
        session.windows[0].width = -50;
        let plan = plan_restore(&session, &|_| true, &HashSet::new());
        assert_eq!(
            plan[0],
            RestoreEntry::Skipped {
                desktop_id: "org.gnome.TextEditor".into(),
                reason: SkipReason::InvalidPlacement,
            }
        );
    }

    #[test]
    fn identityless_windows_have_nothing_to_relaunch() {
        let session = collect(
            [WindowRow {
                desktop_id: None,
                app_id: None,
                title: "mystery".into(),
                tag: None,
                placement: placement(0),
            }],
            0,
        );
        assert!(session.windows.is_empty());
        // An app id can stand in when no desktop id is known.
        let session = collect(
            [WindowRow {
                desktop_id: None,
                app_id: Some("org.gnome.TextEditor".into()),
                title: "t".into(),
                tag: None,
                placement: placement(0),
            }],
            0,
        );
        assert_eq!(session.windows.len(), 1);
        assert_eq!(session.windows[0].desktop_id, "org.gnome.TextEditor");
    }

    #[test]
    fn autostart_filtering_matches_gnome_rules() {
        let dir = tempfile::tempdir().expect("temp dir");
        let write = |name: &str, body: &str| {
            std::fs::write(dir.path().join(name), body).unwrap();
        };
        write(
            "keep.desktop",
            "[Desktop Entry]\nType=Application\nName=Keep\nExec=keep\n",
        );
        write(
            "hidden.desktop",
            "[Desktop Entry]\nType=Application\nName=H\nExec=h\nHidden=true\n",
        );
        write(
            "disabled.desktop",
            "[Desktop Entry]\nType=Application\nName=D\nExec=d\nX-GNOME-Autostart-enabled=false\n",
        );
        write(
            "kde-only.desktop",
            "[Desktop Entry]\nType=Application\nName=K\nExec=k\nOnlyShowIn=KDE;\n",
        );
        write(
            "not-gnome.desktop",
            "[Desktop Entry]\nType=Application\nName=N\nExec=n\nNotShowIn=GNOME;\n",
        );
        write(
            "gnome-only.desktop",
            "[Desktop Entry]\nType=Application\nName=G\nExec=g\nOnlyShowIn=GNOME;\n",
        );
        write(
            "tryexec-missing.desktop",
            "[Desktop Entry]\nType=Application\nName=T\nExec=t\nTryExec=/definitely/missing/tuna-autostart-probe\n",
        );
        write(
            "tryexec-ok.desktop",
            "[Desktop Entry]\nType=Application\nName=O\nExec=o\nTryExec=/bin/true\n",
        );
        write(
            "link.desktop",
            "[Desktop Entry]\nType=Link\nName=L\nURL=https://x\n",
        );
        // A non-entry group must not leak keys into the filter.
        write(
            "tricky.desktop",
            "[Desktop Action toggles]\nHidden=true\n[Desktop Entry]\nType=Application\nName=Tr\nExec=tr\n",
        );
        let ids: Vec<String> = scan_autostart(&[dir.path().to_path_buf()])
            .into_iter()
            .map(|e| e.desktop_id)
            .collect();
        assert_eq!(ids, ["gnome-only", "keep", "tricky", "tryexec-ok"]);
    }

    #[test]
    fn user_autostart_shadows_system_entry() {
        let user = tempfile::tempdir().expect("temp dir");
        let system = tempfile::tempdir().expect("temp dir");
        std::fs::write(
            system.path().join("app.desktop"),
            "[Desktop Entry]\nType=Application\nName=A\nExec=a\n",
        )
        .unwrap();
        // User shadows by id even when disabling: one sighting wins.
        std::fs::write(
            user.path().join("app.desktop"),
            "[Desktop Entry]\nType=Application\nName=A\nExec=a\nHidden=true\n",
        )
        .unwrap();
        let entries = scan_autostart(&[user.path().to_path_buf(), system.path().to_path_buf()]);
        assert!(entries.is_empty(), "user Hidden shadows system entry");
    }

    #[test]
    fn restore_merges_autostart_without_double_launch() {
        let session = collect([row("org.gnome.TextEditor", 2)], 2);
        let plan = plan_restore(&session, &|_| true, &HashSet::new());
        let merged = merge_autostart(
            plan,
            &[
                AutostartEntry {
                    desktop_id: "org.gnome.TextEditor".into(),
                },
                AutostartEntry {
                    desktop_id: "org.gnome.Calculator".into(),
                },
            ],
            2,
        );
        let launches: Vec<&LaunchRequest> = merged
            .iter()
            .filter_map(|e| match e {
                RestoreEntry::Launch(req) => Some(req),
                RestoreEntry::Skipped { .. } => None,
            })
            .collect();
        assert_eq!(launches.len(), 2);
        // Autostart-only app launches first, on the active workspace.
        assert_eq!(launches[0].desktop_id, "org.gnome.Calculator");
        assert!(launches[0].from_autostart);
        assert_eq!(launches[0].placement.workspace, 2);
        // The restored app keeps its saved placement, launched once.
        assert_eq!(launches[1].desktop_id, "org.gnome.TextEditor");
        assert!(!launches[1].from_autostart);
        assert_eq!(launches[1].placement.workspace, 2);
    }

    #[test]
    fn session_ids_are_stable_safe_and_bounded() {
        assert_eq!(
            session_id_for("org.gnome.TextEditor", Some("main window")),
            "tuna-org.gnome.TextEditor-main_window"
        );
        assert_eq!(
            session_id_for("org.gnome.TextEditor", None),
            "tuna-org.gnome.TextEditor"
        );
        assert_eq!(session_id_for("  ", None), "tuna-app");
        let id = session_id_for("a/b c", Some("x/y;z"));
        assert!(id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_'));
        let long = "w".repeat(500);
        assert!(session_id_for(&long, Some(&long)).len() < 220);
    }

    #[test]
    fn snapshot_file_defaults_under_state_home() {
        let home = tempfile::tempdir().expect("temp dir");
        let state = home.path().join("state");
        let state_str = state.to_string_lossy().into_owned();
        let _guard = EnvGuard::set(&[("XDG_STATE_HOME", Some(&state_str))]);
        assert_eq!(session_file_path(), state.join("tuna").join("session.json"));
    }

    #[test]
    fn oversized_snapshot_is_rejected() {
        let (_dir, path) = tmp_file("session.json");
        std::fs::write(&path, vec![b'x'; 2 * 1024 * 1024]).unwrap();
        assert!(matches!(
            load_from(&path),
            LoadOutcome::Ignored(IgnoreReason::Corrupt(_))
        ));
    }

    fn registry_with(saved: &[&str], crashed: &[&str], enabled: bool) -> RestoreRegistry {
        RestoreRegistry::new(
            Arc::new(AtomicBool::new(enabled)),
            saved.iter().map(|s| s.to_string()).collect(),
            crashed.iter().map(|s| s.to_string()).collect(),
        )
    }

    #[test]
    fn register_reasons_follow_gnome_codes() {
        let mut reg = registry_with(
            &["org.gnome.TextEditor", "org.gnome.Calculator"],
            &["org.gnome.Calculator"],
            true,
        );
        // Saved app restored after logout/shutdown: reason 3.
        let (reason, first, cleanup) = reg
            .register("org.gnome.TextEditor", ":1.7")
            .expect("register");
        assert_eq!(reason, reason::NORMAL_RESTORE);
        assert!(cleanup.is_empty(), "first registration has no stale ids");
        assert!(
            first.starts_with("tuna-"),
            "instance id carries session token"
        );
        // Crashed beats saved: restore carefully (reason 2).
        let (reason, _, _) = reg
            .register("org.gnome.Calculator", ":1.8")
            .expect("register");
        assert_eq!(reason, reason::CRASH_RESTORE);
        // Unknown app: normal launch (reason 1).
        let (reason, _, _) = reg.register("org.gnome.Maps", ":1.9").expect("register");
        assert_eq!(reason, reason::NORMAL_LAUNCH);
    }

    #[test]
    fn disabled_registry_restores_nothing() {
        let mut reg = registry_with(&["org.gnome.TextEditor"], &[], false);
        let (reason, _, _) = reg
            .register("org.gnome.TextEditor", ":1.7")
            .expect("register");
        assert_eq!(reason, reason::NO_RESTORE);
    }

    #[test]
    fn live_ids_are_never_sent_for_cleanup() {
        let mut reg = registry_with(&[], &[], true);
        let (_, first, cleanup) = reg
            .register("org.gnome.TextEditor", ":1.7")
            .expect("register");
        assert!(cleanup.is_empty(), "first registration has no cleanup");
        // A second live instance mints a fresh id and reports no
        // cleanup: the first instance is still running, so its id
        // must not be deleted.
        let (_, second, cleanup) = reg
            .register("org.gnome.TextEditor", ":1.9")
            .expect("register");
        assert_ne!(first, second, "concurrent instances hold distinct ids");
        assert!(
            cleanup.is_empty(),
            "live instance ids are never cleanup ids, got {cleanup:?}"
        );
        // `deleted` cannot forget a live id either: it only forgets
        // discarded ids the registry actually sent for cleanup.
        assert_eq!(
            reg.deleted("org.gnome.TextEditor", std::slice::from_ref(&first)),
            0
        );
        assert_eq!(
            reg.deleted("org.gnome.TextEditor", std::slice::from_ref(&second)),
            0
        );
        assert_eq!(
            reg.deleted(
                "org.gnome.TextEditor",
                std::slice::from_ref(&"bogus".to_owned())
            ),
            0
        );
    }

    #[test]
    fn clean_exit_recycles_ids_through_the_discard_path() {
        let mut reg = registry_with(&[], &[], true);
        let (_, first, _) = reg
            .register("org.gnome.TextEditor", ":1.7")
            .expect("register");
        let (_, second, cleanup) = reg
            .register("org.gnome.TextEditor", ":1.9")
            .expect("register");
        assert_ne!(first, second);
        assert!(cleanup.is_empty());
        // Clean exits drop the live registrations. The first id is
        // parked for reuse; the second displaces it to cleanup.
        assert!(reg.unregister("org.gnome.TextEditor", &first));
        assert!(!reg.unregister("org.gnome.TextEditor", &first));
        assert!(!reg.unregister("org.gnome.TextEditor", "bogus"));
        assert!(!reg.unregister("org.gnome.Missing", &first));
        assert!(reg.unregister("org.gnome.TextEditor", &second));
        // The next registration reuses the parked id and reports the
        // displaced one — and only that one — for cleanup.
        let (_, reused, cleanup) = reg
            .register("org.gnome.TextEditor", ":1.11")
            .expect("register");
        assert_eq!(reused, second, "prior instance id is reused");
        assert_eq!(cleanup, [first.as_str()]);
        // The app deletes the discarded id; it is forgotten for good,
        // while the live id still cannot be forgotten.
        assert_eq!(
            reg.deleted("org.gnome.TextEditor", std::slice::from_ref(&first)),
            1
        );
        assert_eq!(
            reg.deleted("org.gnome.TextEditor", std::slice::from_ref(&first)),
            0
        );
        assert_eq!(
            reg.deleted("org.gnome.TextEditor", std::slice::from_ref(&reused)),
            0
        );
        // A lone clean exit parks its id with no cleanup at all.
        assert!(reg.unregister("org.gnome.TextEditor", &reused));
        let (_, again, cleanup) = reg
            .register("org.gnome.TextEditor", ":1.12")
            .expect("register");
        assert_eq!(again, second);
        assert!(cleanup.is_empty());
    }

    #[test]
    fn vanished_bus_names_crash_their_instances() {
        let mut reg = registry_with(&["org.gnome.TextEditor"], &[], true);
        let (_, first, _) = reg
            .register("org.gnome.TextEditor", ":1.7")
            .expect("register");
        // Gaining an owner, or an unrelated name, marks nothing.
        assert_eq!(reg.note_name_owner_changed(":1.7", ":1.7"), 0);
        assert_eq!(reg.note_name_owner_changed(":1.99", ""), 0);
        assert_eq!(reg.note_name_owner_changed("", ""), 0);
        // Losing the owner marks the instance crashed and frees the
        // id for reuse with a crash reason.
        assert_eq!(reg.note_name_owner_changed(":1.7", ""), 1);
        assert_eq!(reg.note_name_owner_changed(":1.7", ""), 0);
        let (reason, reused, _) = reg
            .register("org.gnome.TextEditor", ":1.20")
            .expect("register");
        assert_eq!(reused, first, "crashed instance keeps its id");
        assert_eq!(reason, reason::CRASH_RESTORE);
    }

    #[test]
    fn empty_dbus_name_falls_back_to_the_sender() {
        let mut reg = registry_with(&[], &[], true);
        let (_, id, _) = reg
            .register_with_sender("org.gnome.TextEditor", "", Some(":1.42"))
            .expect("sender fallback");
        assert!(id.starts_with("tuna-"));
        // The sender's name is what crash monitoring watches.
        assert_eq!(reg.note_name_owner_changed(":1.42", ""), 1);
        // An explicit bus name still wins over the sender.
        let (_, _, _) = reg
            .register_with_sender("org.gnome.TextEditor", ":1.7", Some(":1.99"))
            .expect("register");
        assert_eq!(reg.note_name_owner_changed(":1.7", ""), 1);
        assert_eq!(reg.note_name_owner_changed(":1.99", ""), 0);
        // No name anywhere is still rejected.
        assert!(reg
            .register_with_sender("org.gnome.TextEditor", "", None)
            .is_err());
        assert!(reg
            .register_with_sender("org.gnome.TextEditor", "", Some(""))
            .is_err());
    }

    #[test]
    fn unknown_unregister_is_not_registered() {
        use zbus::DBusError as _;
        let mut reg = registry_with(&[], &[], true);
        assert!(!reg.unregister("org.gnome.TextEditor", "tuna-nope"));
        let (_, id, _) = reg
            .register("org.gnome.TextEditor", ":1.7")
            .expect("register");
        assert!(!reg.unregister("org.gnome.TextEditor", "tuna-nope"));
        assert!(reg.unregister("org.gnome.TextEditor", &id));
        assert!(!reg.unregister("org.gnome.TextEditor", &id));
        // The wire error matches upstream's
        // `GSM_MANAGER_ERROR_NOT_REGISTERED` domain and name.
        let err = RestoreDbusError::NotRegistered("Provided instance not found".into());
        assert_eq!(
            err.name().as_str(),
            "org.gnome.SessionManager.Error.NotRegistered"
        );
        assert!(err.description().is_some_and(|d| !d.is_empty()));
    }

    #[test]
    fn gnome_51_contract_shapes_are_pinned() {
        // Restore reasons from the 51.0 `org.gnome.SessionManager.xml`
        // itemized list (0 = restore nothing, 1 = normal launch, 2 =
        // post-crash restore, 3 = normal restore).
        assert_eq!(reason::NO_RESTORE, 0);
        assert_eq!(reason::NORMAL_LAUNCH, 1);
        assert_eq!(reason::CRASH_RESTORE, 2);
        assert_eq!(reason::NORMAL_RESTORE, 3);
    }

    #[test]
    fn register_rejects_empty_identity() {
        let mut reg = registry_with(&[], &[], true);
        assert!(reg.register("", ":1.7").is_err());
        assert!(reg.register("   ", ":1.7").is_err());
        assert!(reg.register("org.gnome.TextEditor", "").is_err());
        assert_eq!(reg.deleted("", &[]), 0);
        assert!(!reg.unregister("", "x"));
    }

    #[test]
    fn crash_notes_and_refresh_change_reasons() {
        let mut reg = registry_with(&["org.gnome.TextEditor"], &[], true);
        reg.note_crashed("org.gnome.TextEditor");
        let (reason, _, _) = reg
            .register("org.gnome.TextEditor", ":1.7")
            .expect("register");
        assert_eq!(reason, reason::CRASH_RESTORE);
        reg.note_crashed("   ");
        reg.refresh(HashSet::new(), HashSet::new());
        let (reason, _, _) = reg
            .register("org.gnome.TextEditor", ":1.7")
            .expect("register");
        assert_eq!(reason, reason::NORMAL_LAUNCH);
    }

    #[test]
    fn extreme_coordinates_reject_without_panicking() {
        // `i32::MIN.abs()` panics in debug builds; a hostile snapshot
        // may carry exactly that.
        for (x, y) in [
            (i32::MIN, 0),
            (0, i32::MIN),
            (i32::MIN, i32::MIN),
            (i32::MAX, i32::MAX),
        ] {
            let mut bad = placement(0);
            bad.x = x;
            bad.y = y;
            assert!(
                bad.validated().is_none(),
                "({x}, {y}) must be rejected, not panic"
            );
        }
        // Boundary survivors still validate.
        let mut edge = placement(0);
        edge.x = MAX_DIMENSION * 4;
        edge.y = -(MAX_DIMENSION * 4);
        assert!(edge.validated().is_some());
        // And collect drops hostile rows at save time.
        let mut hostile = row("org.gnome.TextEditor", 0);
        hostile.placement.x = i32::MIN;
        assert!(collect([hostile], 0).windows.is_empty());
    }

    #[test]
    fn reads_are_bounded_before_the_size_check() {
        // `/dev/zero` reports a zero size but streams forever: without
        // the capped read, `load_from` would never return. With it,
        // the read stops just past the limit and the file is corrupt.
        assert!(matches!(
            load_from(Path::new("/dev/zero")),
            LoadOutcome::Ignored(IgnoreReason::Corrupt(_))
        ));
        // One byte past the cap is rejected even when the metadata
        // fast path cannot see it coming (covered by the same take).
        let (_dir, path) = tmp_file("session.json");
        std::fs::write(&path, vec![b'x'; MAX_SNAPSHOT_BYTES as usize + 1]).unwrap();
        assert!(matches!(
            load_from(&path),
            LoadOutcome::Ignored(IgnoreReason::Corrupt(_))
        ));
    }

    #[test]
    fn autostart_dirs_honor_xdg_config_home() {
        let root = tempfile::tempdir().expect("temp dir");
        let custom = root.path().join("custom-config");
        let custom_str = custom.to_string_lossy().into_owned();
        let home_str = root.path().to_string_lossy().into_owned();
        let _guard = EnvGuard::set(&[
            ("XDG_CONFIG_HOME", Some(&custom_str)),
            ("HOME", Some(&home_str)),
            ("XDG_CONFIG_DIRS", None),
        ]);
        let dirs = default_autostart_dirs();
        assert_eq!(dirs[0], custom.join("autostart"));
        assert_eq!(dirs[1], PathBuf::from("/etc/xdg/autostart"));
        drop(_guard);
        // A relative XDG_CONFIG_HOME is ignored per the spec.
        let _guard = EnvGuard::set(&[
            ("XDG_CONFIG_HOME", Some("relative/path")),
            ("HOME", Some(&home_str)),
            ("XDG_CONFIG_DIRS", None),
        ]);
        let dirs = default_autostart_dirs();
        assert_eq!(dirs[0], root.path().join(".config/autostart"));
    }

    #[test]
    fn autostart_rejects_unlaunchable_entries_without_shadowing() {
        let user = tempfile::tempdir().expect("temp dir");
        let system = tempfile::tempdir().expect("temp dir");
        // Config-only user entries: no Type, no Exec.
        std::fs::write(
            user.path().join("notype.desktop"),
            "[Desktop Entry]\nName=NoType\nExec=/bin/true\n",
        )
        .unwrap();
        std::fs::write(
            user.path().join("noexec.desktop"),
            "[Desktop Entry]\nType=Application\nName=NoExec\n",
        )
        .unwrap();
        // Same stems, launchable, in the system dir.
        for stem in ["notype", "noexec"] {
            std::fs::write(
                system.path().join(format!("{stem}.desktop")),
                "[Desktop Entry]\nType=Application\nName=S\nExec=/bin/true\n",
            )
            .unwrap();
        }
        // A Hidden stub with no Exec is still a disable, not a
        // config-only entry: it shadows the system file.
        std::fs::write(
            user.path().join("disabled.desktop"),
            "[Desktop Entry]\nHidden=true\n",
        )
        .unwrap();
        std::fs::write(
            system.path().join("disabled.desktop"),
            "[Desktop Entry]\nType=Application\nName=S\nExec=/bin/true\n",
        )
        .unwrap();
        let ids: Vec<String> =
            scan_autostart(&[user.path().to_path_buf(), system.path().to_path_buf()])
                .into_iter()
                .map(|e| e.desktop_id)
                .collect();
        assert_eq!(ids, ["noexec", "notype"]);
    }

    #[test]
    fn parallel_saves_never_interleave_or_litter() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("session.json");
        let sessions: Vec<SavedSession> = (0..8)
            .map(|ws| collect([row("org.gnome.TextEditor", ws)], ws))
            .collect();
        std::thread::scope(|s| {
            for session in &sessions {
                s.spawn(|| {
                    for _ in 0..8 {
                        save_to(&path, session).expect("parallel save");
                    }
                });
            }
        });
        // The winner is one whole snapshot, never interleaved bytes.
        match load_from(&path) {
            LoadOutcome::Loaded(back) => {
                assert!(sessions.contains(&back));
            }
            other => panic!("parallel saves must leave a valid snapshot, got {other:?}"),
        }
        // No temporary file is left behind, fixed-name or unique.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.file_name()))
            .filter(|n| n != "session.json")
            .collect();
        assert!(leftovers.is_empty(), "tmp litter: {leftovers:?}");
        // And the survivor still carries the private mode.
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn exec_expansion_follows_the_desktop_spec() {
        let desktop = PathBuf::from("/apps/demo.desktop");
        // Quoting, escapes, and single quotes (not special in Exec=).
        assert_eq!(
            expand_exec(r#"app "two words" it\'s \\"#, "Demo", None, &desktop),
            ["app", "two words", "it's", "\\"]
        );
        // File codes drop; %% survives; %c/%k expand.
        assert_eq!(
            expand_exec("app %f %F %u %U 100%% %c %k", "Demo", None, &desktop),
            ["app", "100%", "Demo", "/apps/demo.desktop"]
        );
        // Standalone %i becomes --icon plus the value; without an
        // Icon key it vanishes.
        assert_eq!(
            expand_exec("app %i", "Demo", Some("demo-icon"), &desktop),
            ["app", "--icon", "demo-icon"]
        );
        assert_eq!(expand_exec("app %i", "Demo", None, &desktop), ["app"]);
        // Deprecated and unknown codes drop.
        assert_eq!(
            expand_exec("app %d %v %m %q", "Demo", None, &desktop),
            ["app"]
        );
    }

    /// Fake installed application under an isolated XDG world; the
    /// guard must outlive every resolve/launch call.
    fn fake_app_world(desktop_id: &str, body: &str) -> (tempfile::TempDir, EnvGuard) {
        let root = tempfile::tempdir().expect("temp dir");
        let apps = root.path().join("data/applications");
        std::fs::create_dir_all(&apps).unwrap();
        std::fs::write(apps.join(format!("{desktop_id}.desktop")), body).unwrap();
        let guard = EnvGuard::isolated(root.path());
        (root, guard)
    }

    #[test]
    fn launcher_resolves_spawns_and_refuses() {
        let (_root, _guard) = fake_app_world(
            "com.example.Marker",
            "[Desktop Entry]\nType=Application\nName=Marker\nExec=/bin/true\n",
        );
        assert!(installed_apps().contains("com.example.Marker"));
        assert!(resolve_desktop_file("com.example.Marker").is_some());
        launch_desktop_id("com.example.Marker").expect("launch installed app");
        // Unknown apps, traversal ids, and refusal cases fail cleanly.
        assert_eq!(
            launch_desktop_id("com.example.Gone").unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        assert!(resolve_desktop_file("../evil").is_none());
        assert!(resolve_desktop_file("a/b").is_none());
        drop(_guard);
        let (_root, _guard) = fake_app_world(
            "com.example.Terminal",
            "[Desktop Entry]\nType=Application\nName=T\nExec=/bin/true\nTerminal=true\n",
        );
        assert_eq!(
            launch_desktop_id("com.example.Terminal")
                .unwrap_err()
                .kind(),
            io::ErrorKind::Unsupported
        );
        drop(_guard);
        let (_root, _guard) = fake_app_world(
            "com.example.NoExec",
            "[Desktop Entry]\nType=Application\nName=N\n",
        );
        assert_eq!(
            launch_desktop_id("com.example.NoExec").unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        drop(_guard);
        let (_root, _guard) = fake_app_world(
            "com.example.TryExec",
            "[Desktop Entry]\nType=Application\nName=T\nExec=/bin/true\nTryExec=/definitely/missing/tuna-restore-probe\n",
        );
        assert_eq!(
            launch_desktop_id("com.example.TryExec").unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn launcher_runs_the_child_detached() {
        let dir = tempfile::tempdir().expect("temp dir");
        let marker = dir.path().join("launched");
        let marker_str = marker.to_string_lossy().into_owned();
        let (_root, _guard) = fake_app_world(
            "com.example.Spawn",
            &format!(
                "[Desktop Entry]\nType=Application\nName=S\nExec=/bin/sh -c \"echo ok > '{marker_str}'\"\n"
            ),
        );
        launch_desktop_id("com.example.Spawn").expect("spawn marker");
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::fs::read_to_string(&marker).unwrap_or_default() != "ok\n" {
            assert!(
                std::time::Instant::now() < deadline,
                "spawned child never wrote its marker"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn launch_plan_runs_launches_in_order_and_survives_failure() {
        let session = collect(
            [row("org.gnome.TextEditor", 0), row("com.example.Gone", 0)],
            0,
        );
        let plan = plan_restore(&session, &|id| id != "com.example.Gone", &HashSet::new());
        let merged = merge_autostart(
            plan,
            &[AutostartEntry {
                desktop_id: "org.gnome.Calculator".into(),
            }],
            0,
        );
        let seen = std::sync::Mutex::new(Vec::new());
        let attempts = launch_plan(&merged, &|req| {
            seen.lock().unwrap().push(req.desktop_id.clone());
            if req.desktop_id == "org.gnome.TextEditor" {
                Err(io::Error::other("boom"))
            } else {
                Ok(())
            }
        });
        // Autostart first, then the snapshot; the skipped app has no
        // attempt; the failure does not abort the plan.
        assert_eq!(
            *seen.lock().unwrap(),
            ["org.gnome.Calculator", "org.gnome.TextEditor"]
        );
        assert_eq!(attempts.len(), 2);
        assert!(attempts[0].result.is_ok());
        assert!(attempts[0].from_autostart);
        assert!(attempts[1].result.is_err());
        assert!(!attempts[1].from_autostart);
    }

    #[test]
    fn placement_queue_matches_tags_then_apps_fifo() {
        let mut tagged = row("org.gnome.TextEditor", 1);
        tagged.tag = Some("main".into());
        tagged.app_id = Some("org.gnome.TextEditor".into());
        let mut other = row("org.gnome.TextEditor", 2);
        other.tag = Some("scratch".into());
        other.app_id = Some("org.gnome.TextEditor".into());
        let session = collect([tagged, other], 1);
        let plan = plan_restore(&session, &|_| true, &HashSet::new());
        let mut queue = PendingPlacements::new();
        queue.push_all(&plan);
        assert_eq!(queue.len(), 2);
        // Exact tag match wins regardless of queue order.
        assert_eq!(
            queue
                .take_for(Some("org.gnome.TextEditor"), Some("scratch"))
                .expect("tag match")
                .workspace,
            2
        );
        // App id matches what remains; desktop-id fallback covers
        // clients that set only the bare name.
        assert_eq!(
            queue
                .take_for(Some("org.gnome.TextEditor"), None)
                .expect("app match")
                .workspace,
            1
        );
        assert!(queue.is_empty());
        assert!(queue.take_for(Some("org.gnome.TextEditor"), None).is_none());
        // Unknown windows place normally.
        let mut queue = PendingPlacements::new();
        queue.push_all(&plan);
        assert!(queue.take_for(Some("org.gnome.Maps"), None).is_none());
        assert_eq!(queue.len(), 2);
    }

    #[test]
    fn login_and_logout_entry_points_roundtrip() {
        let root = tempfile::tempdir().expect("temp dir");
        let apps = root.path().join("data/applications");
        std::fs::create_dir_all(&apps).unwrap();
        std::fs::write(
            apps.join("org.gnome.TextEditor.desktop"),
            "[Desktop Entry]\nType=Application\nName=E\nExec=/bin/true\n",
        )
        .unwrap();
        let autostart = root.path().join("config/autostart");
        std::fs::create_dir_all(&autostart).unwrap();
        std::fs::write(
            autostart.join("org.gnome.Calculator.desktop"),
            "[Desktop Entry]\nType=Application\nName=C\nExec=/bin/true\n",
        )
        .unwrap();
        // The installed-app world needs its own .desktop too, or the
        // login plan skips the snapshot entry as missing.
        std::fs::write(
            apps.join("org.gnome.Calculator.desktop"),
            "[Desktop Entry]\nType=Application\nName=C\nExec=/bin/true\n",
        )
        .unwrap();
        let _guard = EnvGuard::isolated(root.path());
        // Logout saves the rows when opted in.
        let outcome = logout_save([row("org.gnome.TextEditor", 1)], 1, RestorePrefs::enabled())
            .expect("logout save");
        assert_eq!(outcome, SaveOutcome::Saved);
        // Login restores the snapshot plus autostart, with queued
        // placements for both.
        let login = login_restore_plan(RestorePrefs::enabled(), &HashSet::new());
        assert_eq!(login.active_workspace, 1);
        let launches: Vec<&LaunchRequest> = login
            .entries
            .iter()
            .filter_map(|e| match e {
                RestoreEntry::Launch(req) => Some(req),
                RestoreEntry::Skipped { .. } => None,
            })
            .collect();
        assert_eq!(launches.len(), 2);
        assert_eq!(launches[0].desktop_id, "org.gnome.Calculator");
        assert!(launches[0].from_autostart);
        assert_eq!(launches[1].desktop_id, "org.gnome.TextEditor");
        assert_eq!(login.placements.len(), 2);
        // Disabled: the snapshot stays closed but autostart still
        // launches, like GNOME.
        let login = login_restore_plan(RestorePrefs::default(), &HashSet::new());
        let launches: Vec<&LaunchRequest> = login
            .entries
            .iter()
            .filter_map(|e| match e {
                RestoreEntry::Launch(req) => Some(req),
                RestoreEntry::Skipped { .. } => None,
            })
            .collect();
        assert_eq!(
            launches
                .iter()
                .map(|r| r.desktop_id.as_str())
                .collect::<Vec<_>>(),
            ["org.gnome.Calculator"]
        );
        // And disabling at logout writes nothing.
        std::fs::remove_file(session_file_path()).unwrap();
        let outcome = logout_save([row("org.gnome.TextEditor", 1)], 1, RestorePrefs::default())
            .expect("logout save");
        assert_eq!(outcome, SaveOutcome::SkippedDisabled);
        assert!(!session_file_path().exists());
    }

    #[test]
    fn restore_applier_degrades_gracefully_for_unknown_windows() {
        let display =
            smithay::reexports::wayland_server::Display::<crate::State>::new().expect("display");
        let mut state = crate::State::new(&display.handle());
        let mut manager = crate::windows::WindowManager::new(&mut state);
        let applied = apply_restored_placement(&mut manager, &mut state, 9999, &placement(1));
        assert_eq!(
            applied,
            AppliedPlacement {
                workspace: false,
                geometry: false,
                layout: true,
            }
        );
        // Layout flags on an unknown window fail the layout step
        // without panicking.
        let mut max = placement(1);
        max.maximized = true;
        max.minimized = true;
        let applied = apply_restored_placement(&mut manager, &mut state, 9999, &max);
        assert!(!applied.workspace);
        assert!(!applied.geometry);
        assert!(!applied.layout);
    }
}
