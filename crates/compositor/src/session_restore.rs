//! Opt-in session save and restore (P-SY-08, #338).
//!
//! What GNOME 51 does: the desktop session can bring back the user's
//! windows after logout and reboot when the user opts in. Tuna Desktop
//! implements the capability in three layers:
//!
//! 1. **Saved state.** [`SavedSession`] is the on-disk snapshot: one
//!    entry per restorable window with its application identity
//!    (desktop id / app id), workspace, geometry and layout flags,
//!    plus the active workspace. It lives at
//!    [`session_file_path`] (`$XDG_STATE_HOME/tuna/session.json`).
//!    Writes are atomic (temporary file plus rename) and the file is
//!    created mode `0600`: window titles can name private documents.
//! 2. **Restore plan.** [`plan_restore`] turns a snapshot into launch
//!    requests ([`LaunchRequest`]) for apps that are still installed
//!    and did not crash, skipping the rest with an explicit
//!    [`SkipReason`]. [`scan_autostart`] adds the XDG autostart
//!    entries GNOME would launch at login, merged without duplicates
//!    by [`merge_autostart`].
//! 3. **Session-manager contracts.** [`serve_session_manager`]
//!    answers `org.gnome.SessionManager`'s `RestoreSupported`
//!    property (what GNOME Settings' multitasking panel checks) plus
//!    the `RegisterRestore` / `DeletedInstanceIds` /
//!    `UnregisterRestore` methods, with the exact signatures and
//!    restore-reason codes from gnome-session's interface definition
//!    (verified against the `main` branch XML; see below). The facade
//!    stays off the bus when a real session manager owns the name.
//!    The `xdg_session_manager_v1` Wayland global is *not* advertised
//!    here: Smithay 0.7 has no such global (see `docs/protocols.md`),
//!    so the wire protocol waits on a Smithay upgrade; the stable
//!    per-window session token it would carry is [`session_id_for`].
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
//! ## Runtime hook points (union-time wiring, not yet called)
//!
//! - Login: `load_from(session_file_path())` → on
//!   `LoadOutcome::Loaded`, `plan_restore` with the installed-app set
//!   and the previous session's crash set, then hand each
//!   [`LaunchRequest`] to the launcher and place the window via
//!   `WindowManager::{set_maximized, set_fullscreen, minimize}` plus
//!   the model workspace move.
//! - Logout / shutdown: collect one [`WindowRow`] per mapped toplevel
//!   (`StateModel` for app id, title, workspace; `WindowManager` for
//!   geometry and layout flags; `protocols::toplevel_tag` for the
//!   tag), [`collect`] them, and [`save_if_enabled`]. The natural
//!   call site is beside `session_services`' shutdown path.
//! - Crash sets: the supervisor's per-app crash record feeds
//!   `plan_restore`'s `crashed` argument so a crashing app is not
//!   relaunched into a loop; the entry stays in the file for the next
//!   clean login.
//! - Corrupt or version-newer files are ignored ([`IgnoreReason`]) and
//!   can be quarantined aside with [`quarantine_at`] for diagnosis;
//!   they never block login.
//!
//! ## GNOME comparison (verified 2026-10-10)
//!
//! Interface shape verified against gnome-session `main`
//! (`gnome-session/org.gnome.SessionManager.xml` on GNOME GitLab):
//! `RestoreSupported` is a read-only boolean property ("Communicates
//!   whether the session supports the save/restore mechanism");
//! `RegisterRestore(app_id, dbus_name)` returns `(reason,
//!   instance_id, cleanup_ids)`; `DeletedInstanceIds(app_id, ids)`
//!   and `UnregisterRestore(app_id, instance_id)` manage instance
//!   lifetime. Upstream documents these as a private interface
//!   between gnome-session and xdg-desktop-portal — apps talk to the
//!   portal, which forwards here. Restore reasons: 0 = restore
//!   nothing, 1 = normal launch (restore reasonable state), 2 =
//!   post-crash restore (restore carefully), 3 = normal restore after
//!   logout/shutdown (restore everything).
//!
//! Open at union time: confirm the `main`-branch XML still matches
//! the GNOME 51.0 tag; full `org.gnome.SessionManager` parity
//! (logout, inhibit, client registration, signals) is out of scope
//! for P-SY-08. Tuna restores only when the user opts in; GNOME's own
//! default (no automatic window reopen) is likewise off.
//!
//! Autostart honours `Hidden=`, `OnlyShowIn=` / `NotShowIn=`,
//! `TryExec=` and `X-GNOME-Autostart-enabled=` per the XDG autostart
//! spec and GNOME's desktop-file handling.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{SystemTime, UNIX_EPOCH};

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
        if self.workspace > MAX_WORKSPACE
            || self.width <= 0
            || self.height <= 0
            || self.width > MAX_DIMENSION
            || self.height > MAX_DIMENSION
            || self.x.abs() > MAX_DIMENSION * 4
            || self.y.abs() > MAX_DIMENSION * 4
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

/// Write `session` to `path` atomically (temporary file in the same
/// directory plus rename) with mode `0600`. Parent directories are
/// created as needed.
pub fn save_to(path: &Path, session: &SavedSession) -> io::Result<()> {
    let json = serialize(session).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true).mode(0o600);
        std::fs::remove_file(&tmp).ok();
        let mut file = opts.open(&tmp)?;
        use std::io::Write;
        file.write_all(json.as_bytes())?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
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
pub fn load_from(path: &Path) -> LoadOutcome {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return LoadOutcome::Absent,
        Err(e) => return LoadOutcome::Ignored(IgnoreReason::Corrupt(e.to_string())),
    };
    if bytes.len() > 1024 * 1024 {
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

/// Scan `dirs` (user dir first, so it shadows system entries by
/// desktop id) for autostart entries, honouring `Hidden=true`,
/// `OnlyShowIn=` / `NotShowIn=`, `TryExec=` and
/// `X-GNOME-Autostart-enabled=false`, like GNOME's startup handling.
/// Unparseable files are skipped; nothing is executed here.
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
            if !seen.insert(desktop_id.clone()) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&file) else {
                continue;
            };
            let keys = desktop_entry_keys(&text);
            if keys
                .get("Hidden")
                .is_some_and(|v| v.eq_ignore_ascii_case("true"))
            {
                continue;
            }
            if keys
                .get("X-GNOME-Autostart-enabled")
                .is_some_and(|v| v.eq_ignore_ascii_case("false"))
            {
                continue;
            }
            if !shown_in_session(&keys) {
                continue;
            }
            if let Some(try_exec) = keys.get("TryExec") {
                if !try_exec_ok(try_exec, &path_dirs) {
                    continue;
                }
            }
            // Only launchable entries: GNOME requires Type=Application.
            if keys.get("Type").is_some_and(|t| t != "Application") {
                continue;
            }
            entries.push(AutostartEntry { desktop_id });
        }
    }
    entries
}

/// Default autostart directories: user config first, then system dirs.
pub fn default_autostart_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        dirs.push(home.join(".config/autostart"));
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

/// Live instance registry behind the `RegisterRestore` family.
/// Tracks which apps asked for restore, what instance ids they hold,
/// and which ids are stale and should be deleted — the bookkeeping
/// gnome-session does between the portal and the apps.
#[derive(Debug)]
pub struct RestoreRegistry {
    supported: RestoreSupported,
    /// Desktop ids present in the on-disk snapshot at login.
    saved: HashSet<String>,
    /// Desktop ids that crashed last session.
    crashed: HashSet<String>,
    /// Instance ids currently issued per desktop id.
    live: HashMap<String, Vec<String>>,
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
            live: HashMap::new(),
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

    /// Register one app for restore: mint a fresh instance id and
    /// report previously issued ids for the app as cleanup ids (they
    /// belong to older registrations the app should delete). Empty
    /// app ids and bus names are rejected; the D-Bus name is recorded
    /// for crash monitoring but not trusted for anything else.
    pub fn register(
        &mut self,
        app_id: &str,
        dbus_name: &str,
    ) -> Result<(u32, String, Vec<String>), String> {
        let desktop_id = normalize_desktop_id(app_id)
            .ok_or_else(|| "RegisterRestore needs a non-empty app id".to_owned())?;
        if dbus_name.is_empty() {
            return Err("RegisterRestore needs the app's bus name".to_owned());
        }
        self.next_instance += 1;
        let app_part = session_id_for(&desktop_id, None);
        let app_part = app_part.trim_start_matches("tuna-");
        let instance_id = format!("tuna-{}-{app_part}-{}", self.token, self.next_instance);
        let reason = self.reason_for(&desktop_id);
        let cleanup_ids = self.live.get(&desktop_id).cloned().unwrap_or_default();
        self.live
            .entry(desktop_id)
            .or_default()
            .push(instance_id.clone());
        Ok((reason, instance_id, cleanup_ids))
    }

    /// Forget instance ids the app confirms it deleted. Unknown ids
    /// are ignored; the return is how many were actually held.
    pub fn deleted(&mut self, app_id: &str, ids: &[String]) -> usize {
        let Some(desktop_id) = normalize_desktop_id(app_id) else {
            return 0;
        };
        let Some(live) = self.live.get_mut(&desktop_id) else {
            return 0;
        };
        let before = live.len();
        live.retain(|id| !ids.contains(id));
        before - live.len()
    }

    /// Drop one registration when the app exits cleanly.
    pub fn unregister(&mut self, app_id: &str, instance_id: &str) -> bool {
        let Some(desktop_id) = normalize_desktop_id(app_id) else {
            return false;
        };
        let Some(live) = self.live.get_mut(&desktop_id) else {
            return false;
        };
        let before = live.len();
        live.retain(|id| id != instance_id);
        before != live.len()
    }
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
    /// reason, a fresh instance id, and stale instance ids the app
    /// should delete. Signature and semantics match gnome-session.
    fn register_restore(
        &self,
        app_id: &str,
        dbus_name: &str,
    ) -> zbus::fdo::Result<(u32, String, Vec<String>)> {
        self.registry
            .lock()
            .map_err(|_| zbus::fdo::Error::Failed("restore registry poisoned".into()))?
            .register(app_id, dbus_name)
            .map_err(zbus::fdo::Error::InvalidArgs)
    }

    /// The app deleted these instance ids; forget them for good.
    fn deleted_instance_ids(&self, app_id: &str, ids: Vec<String>) {
        if let Ok(mut registry) = self.registry.lock() {
            registry.deleted(app_id, &ids);
        }
    }

    /// The app exited cleanly; drop its registration.
    fn unregister_restore(&self, app_id: &str, instance_id: &str) {
        if let Ok(mut registry) = self.registry.lock() {
            registry.unregister(app_id, instance_id);
        }
    }
}

/// Serve the session-manager restore facade on the session bus from a
/// thread. When a real session manager owns the name (a nested run
/// inside GNOME), the service stays off, like the Mutter facade.
/// Hook: the runtime calls this once at startup with its live flag.
/// Hardening note (union time): upstream treats `RegisterRestore` as
/// a private portal interface; a caller-identity check against
/// `org.freedesktop.portal.Desktop` belongs here before trusting
/// `dbus_name` for crash monitoring.
pub fn serve_session_manager(supported: RestoreSupported) {
    let node = SessionManagerNode {
        registry: Arc::new(std::sync::Mutex::new(RestoreRegistry::from_disk(supported))),
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
            // The facade is property-only; park the thread on the
            // connection until the process exits.
            loop {
                std::thread::park();
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let prior = std::env::var_os("XDG_STATE_HOME");
        std::env::set_var("XDG_STATE_HOME", home.path().join("state"));
        let path = session_file_path();
        match prior {
            Some(value) => std::env::set_var("XDG_STATE_HOME", value),
            None => std::env::remove_var("XDG_STATE_HOME"),
        }
        assert_eq!(
            path,
            home.path().join("state").join("tuna").join("session.json")
        );
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
    fn reregistration_reports_stale_ids_for_cleanup() {
        let mut reg = registry_with(&[], &[], true);
        let (_, first, _) = reg
            .register("org.gnome.TextEditor", ":1.7")
            .expect("register");
        let (_, second, cleanup) = reg
            .register("org.gnome.TextEditor", ":1.7")
            .expect("register");
        assert_ne!(first, second, "each registration mints a fresh id");
        assert_eq!(cleanup, [first.as_str()]);
        // The app deletes the stale id; unknown ids are ignored.
        assert_eq!(
            reg.deleted("org.gnome.TextEditor", &[first.clone(), "bogus".into()]),
            1
        );
        // Clean exit drops the live registration.
        assert!(reg.unregister("org.gnome.TextEditor", &second));
        assert!(!reg.unregister("org.gnome.TextEditor", &second));
        assert!(!reg.unregister("org.gnome.TextEditor", "bogus"));
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
}
