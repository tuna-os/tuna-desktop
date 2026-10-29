//! Window session save and restore (P-SY-08, #338): `xdg_session_manager_v1`
//! as Mutter 51 ships it.
//!
//! GNOME 51 restores windows for the clients that ask, and only them. An
//! app opens a session with `get_session`, keeps the id the `created`
//! event hands it and adds its toplevels by name; on its next run it
//! passes the id back (`restored`), and asks `restore_toplevel` for each
//! name before that window's first commit. The compositor then puts the
//! window back where it was: workspace, position and size, maximized,
//! tiled or fullscreen, minimized, and on its output. An app that never
//! opens a session is placed as usual. That client opt-in is the whole
//! of GNOME 51's policy: gnome-session 51 switched its own app relaunch
//! off (`RestoreSupported` is false: "For GNOME 50, we've disabled the
//! save/restore feature", `gsm-manager.c`), so GNOME Settings 51 hides
//! its "Reopen Windows After Login" switch (`cc-multitasking-panel.c`
//! shows it only when `RestoreSupported` is true) and Mutter reads no
//! setting for it ([`crate::session_manager`] answers the same).
//!
//! Re-implemented from Mutter 51.0 (GPL-2.0-or-later; no code copied).
//! niri and cosmic-comp have no session management to compare with.
//!
//! - `src/wayland/meta-wayland-xdg-session-manager.c`: the global at
//!   version 1 for every client; an unknown session id is treated as
//!   none; reopening a session the same client holds is `in_use`, while
//!   another client takes it over (`replaced` to the old one); a new id is
//!   a random UUID; state reaches the disk 3 s after a toplevel's state is
//!   recorded (`TIMEOUT_DELAY_SECONDS`).
//! - `src/wayland/meta-wayland-xdg-session.c`: `add_toplevel` refuses a
//!   name the session already knows (`name_in_use`); `restore_toplevel`
//!   refuses a name in use and a toplevel that has committed
//!   (`already_mapped`), and sends `restored` only when a state applied;
//!   a toplevel's state is recorded when its window goes away;
//!   `remove_toplevel` forgets the name and `remove` the session.
//! - `src/wayland/meta-wayland-xdg-session-state.c`: per toplevel the
//!   window state (none, floating, maximized, tiled left or right,
//!   fullscreen, Mutter's numbering), its rectangle, minimized and the
//!   workspace index; restoring picks the monitor from the rectangle,
//!   moves the window to the workspace, then applies the state.
//! - `src/core/meta-session-manager.c` and `meta-context-main.c`: one file
//!   holds every session (`$XDG_DATA_HOME/<nick>/session.gvdb`), session
//!   data newer than format version 1 restores nothing, and the store is
//!   written once more, synchronously, when the compositor exits.
//!
//! Where this differs, on purpose: the file is JSON with Mutter's keys at
//! `$XDG_DATA_HOME/tuna/window-sessions.json`; a toplevel also remembers
//! its output by name and, under a maximized, tiled or fullscreen state,
//! the floating rectangle to return to; its state is also recorded when
//! the client destroys the session or disconnects (Mutter records only
//! when the window is unmanaged); the store is bounded (least recently
//! used sessions go first); `remove_toplevel` forgets a stored name even
//! when no window holds it this run, as the protocol text asks; the
//! staging protocol's `already_added`, `invalid_reason` and `rename` are
//! honoured, which Mutter 51 does not check or implement; Mutter's parser
//! never reads `is-minimized` back (it tests the wrong variant), here a
//! minimized window comes back minimized; a floating rectangle no output
//! shows any more is moved onto one.

use std::collections::{BTreeMap, HashMap};
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};
use smithay::reexports::wayland_server::{
    backend::{ClientId, GlobalId},
    protocol::wl_surface::WlSurface,
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, WEnum,
};
use smithay::utils::{Logical, Rectangle};
use smithay::wayland::compositor::{add_post_commit_hook, with_states};

use crate::{OutputEntry, State};

#[allow(
    dead_code,
    unused_imports,
    non_camel_case_types,
    non_upper_case_globals,
    missing_docs,
    clippy::all
)]
pub mod server {
    use wayland_protocols::xdg::shell::server::*;
    use wayland_server;
    use wayland_server::protocol::*;
    pub mod __interfaces {
        use wayland_protocols::xdg::shell::server::__interfaces::*;
        use wayland_server::backend as wayland_backend;
        use wayland_server::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("protocols/xdg-session-management-v1.xml");
    }
    use self::__interfaces::*;
    wayland_scanner::generate_server_code!("protocols/xdg-session-management-v1.xml");
}

use server::{
    xdg_session_manager_v1::{self, XdgSessionManagerV1},
    xdg_session_v1::{self, XdgSessionV1},
    xdg_toplevel_session_v1::{self, XdgToplevelSessionV1},
};

/// The global's version, Mutter 51's `META_XDG_SESSION_MANAGER_V1_VERSION`.
pub const VERSION: u32 = 1;
/// Delay between a recorded toplevel state and the write (Mutter's
/// `TIMEOUT_DELAY_SECONDS`).
pub const SAVE_DELAY: Duration = Duration::from_secs(3);
/// Mutter's `STATE_FORMAT_VERSION`: newer session data restores nothing.
pub const FORMAT_VERSION: i64 = 1;
/// Sessions kept; beyond this the least recently used one is forgotten.
pub const MAX_SESSIONS: usize = 256;
/// Toplevels remembered per session.
pub const MAX_TOPLEVELS: usize = 256;
/// Largest store read back (Mutter's `MAX_SIZE`).
pub const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;
/// Largest coordinate or size a saved rectangle may carry.
const MAX_COORDINATE: i64 = 1 << 20;

/// A window's state, numbered as Mutter's `WindowState`
/// (`meta-wayland-xdg-session-state.c`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WindowState {
    /// Nothing worth restoring (never placed).
    #[default]
    None,
    /// At its own position and size.
    Floating,
    /// Filling the work area.
    Maximized,
    /// Filling the left half.
    TiledLeft,
    /// Filling the right half.
    TiledRight,
    /// Covering the output.
    Fullscreen,
}

impl WindowState {
    fn code(self) -> u64 {
        match self {
            WindowState::None => 0,
            WindowState::Floating => 1,
            WindowState::Maximized => 2,
            WindowState::TiledLeft => 3,
            WindowState::TiledRight => 4,
            WindowState::Fullscreen => 5,
        }
    }

    fn from_code(code: u64) -> Option<Self> {
        Some(match code {
            0 => WindowState::None,
            1 => WindowState::Floating,
            2 => WindowState::Maximized,
            3 => WindowState::TiledLeft,
            4 => WindowState::TiledRight,
            5 => WindowState::Fullscreen,
            _ => return None,
        })
    }
}

/// What a session remembers of one toplevel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToplevelState {
    /// Its window state.
    pub state: WindowState,
    /// Floating position and size: the window's own while floating, the
    /// one to return to under another state.
    pub floating_rect: Option<Rectangle<i32, Logical>>,
    /// The maximized, tiled or fullscreen rectangle.
    pub tiled_rect: Option<Rectangle<i32, Logical>>,
    /// Minimized (hidden).
    pub minimized: bool,
    /// Workspace index; negative when on every workspace or unknown.
    pub workspace: i32,
    /// Name of the output it was on.
    pub output: Option<String>,
}

impl Default for ToplevelState {
    fn default() -> Self {
        Self {
            state: WindowState::None,
            floating_rect: None,
            tiled_rect: None,
            minimized: false,
            workspace: -1,
            output: None,
        }
    }
}

impl ToplevelState {
    /// Whether restoring applies anything. Mutter restores nothing for
    /// `none`; a floating state also needs its rectangle.
    pub fn restorable(&self) -> bool {
        match self.state {
            WindowState::None => false,
            WindowState::Floating => self.floating_rect.is_some(),
            _ => true,
        }
    }
}

fn rect_value(rect: Rectangle<i32, Logical>) -> Value {
    json!([rect.loc.x, rect.loc.y, rect.size.w, rect.size.h])
}

fn rect_from(value: &Value) -> Option<Rectangle<i32, Logical>> {
    let cells = value.as_array()?;
    if cells.len() != 4 {
        return None;
    }
    let mut out = [0i32; 4];
    for (slot, cell) in out.iter_mut().zip(cells) {
        let n = cell.as_i64()?;
        if !(-MAX_COORDINATE..=MAX_COORDINATE).contains(&n) {
            return None;
        }
        *slot = n as i32;
    }
    (out[2] > 0 && out[3] > 0)
        .then(|| Rectangle::new((out[0], out[1]).into(), (out[2], out[3]).into()))
}

fn toplevel_from(value: &Value) -> ToplevelState {
    // Like Mutter's parser, every field is optional and a field of the
    // wrong type is skipped, never the whole toplevel.
    let state = value
        .get("state")
        .and_then(Value::as_u64)
        .and_then(WindowState::from_code)
        .unwrap_or_default();
    ToplevelState {
        state,
        floating_rect: value.get("floating-rect").and_then(rect_from),
        tiled_rect: value.get("tiled-rect").and_then(rect_from),
        minimized: value
            .get("is-minimized")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        workspace: value
            .get("workspace")
            .and_then(Value::as_i64)
            .map(|w| w.clamp(-1, i64::from(u16::MAX)) as i32)
            .unwrap_or(-1),
        output: value
            .get("output")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty() && name.len() <= 256)
            .map(str::to_owned),
    }
}

fn toplevel_value(state: &ToplevelState) -> Value {
    let mut out = Map::new();
    out.insert("state".into(), json!(state.state.code()));
    if let Some(rect) = state.floating_rect {
        out.insert("floating-rect".into(), rect_value(rect));
    }
    if let Some(rect) = state.tiled_rect {
        out.insert("tiled-rect".into(), rect_value(rect));
    }
    out.insert("is-minimized".into(), json!(state.minimized));
    out.insert("workspace".into(), json!(state.workspace));
    if let Some(output) = &state.output {
        out.insert("output".into(), json!(output));
    }
    Value::Object(out)
}

/// One stored session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionRecord {
    /// When it was last opened or updated (microseconds since the epoch,
    /// Mutter's `last-used`).
    pub last_used: i64,
    /// Toplevel states by the names the client gave them.
    pub toplevels: BTreeMap<String, ToplevelState>,
}

/// Every session's saved state: the file's contents.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionStore {
    sessions: HashMap<String, SessionRecord>,
}

impl SessionStore {
    /// Read a store. A file that is not a version-1 store is refused
    /// whole; inside one, a session whose data is newer than
    /// [`FORMAT_VERSION`] stays known but restores nothing, as in
    /// Mutter, and fields of the wrong type are skipped.
    pub fn parse(text: &str) -> Result<Self, String> {
        let root: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        match root.get("version").and_then(Value::as_i64) {
            Some(version) if version <= FORMAT_VERSION => {}
            Some(version) => return Err(format!("store version {version} is newer")),
            None => return Err("no store version".to_owned()),
        }
        let sessions = root
            .get("sessions")
            .and_then(Value::as_object)
            .ok_or("no sessions")?;
        let mut store = SessionStore::default();
        for (id, data) in sessions {
            let mut record = SessionRecord::default();
            let current = data
                .get("version")
                .and_then(Value::as_i64)
                .is_some_and(|version| version <= FORMAT_VERSION);
            if current {
                record.last_used = data.get("last-used").and_then(Value::as_i64).unwrap_or(0);
                if let Some(toplevels) = data.get("toplevels").and_then(Value::as_object) {
                    for (name, toplevel) in toplevels.iter().take(MAX_TOPLEVELS) {
                        record
                            .toplevels
                            .insert(name.clone(), toplevel_from(toplevel));
                    }
                }
            }
            store.sessions.insert(id.clone(), record);
        }
        store.evict(&|_| false);
        Ok(store)
    }

    /// The store as the file holds it, sessions and names sorted.
    pub fn to_json(&self) -> String {
        let mut ids: Vec<&String> = self.sessions.keys().collect();
        ids.sort();
        let mut sessions = Map::new();
        for id in ids {
            let record = &self.sessions[id];
            let toplevels: Map<String, Value> = record
                .toplevels
                .iter()
                .map(|(name, state)| (name.clone(), toplevel_value(state)))
                .collect();
            sessions.insert(
                id.clone(),
                json!({
                    "version": FORMAT_VERSION,
                    "last-used": record.last_used,
                    "toplevels": toplevels,
                }),
            );
        }
        let mut text = serde_json::to_string_pretty(&json!({
            "version": FORMAT_VERSION,
            "sessions": sessions,
        }))
        .unwrap_or_default();
        text.push('\n');
        text
    }

    /// Read the store at `path`; a missing, oversized or unreadable file
    /// starts empty (and the next save replaces it).
    pub fn load(path: &Path) -> Self {
        let text = match std::fs::metadata(path) {
            Ok(meta) if meta.len() > MAX_FILE_BYTES => {
                eprintln!(
                    "tuna-compositor: window sessions in {} exceed {MAX_FILE_BYTES} bytes; ignored",
                    path.display()
                );
                return Self::default();
            }
            Ok(_) => match std::fs::read_to_string(path) {
                Ok(text) => text,
                Err(error) => {
                    eprintln!(
                        "tuna-compositor: window sessions in {} unreadable: {error}",
                        path.display()
                    );
                    return Self::default();
                }
            },
            Err(_) => return Self::default(),
        };
        Self::parse(&text).unwrap_or_else(|error| {
            eprintln!(
                "tuna-compositor: window sessions in {} ignored: {error}",
                path.display()
            );
            Self::default()
        })
    }

    /// Whether a session with this id is stored.
    pub fn contains(&self, id: &str) -> bool {
        self.sessions.contains_key(id)
    }

    /// A stored session.
    pub fn session(&self, id: &str) -> Option<&SessionRecord> {
        self.sessions.get(id)
    }

    /// How many sessions are stored.
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    /// Whether no session is stored.
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// Open (or create) a session, marking it used now; older sessions
    /// beyond [`MAX_SESSIONS`] that `keep` does not protect are forgotten.
    pub fn open(&mut self, id: &str, now: i64, keep: &dyn Fn(&str) -> bool) {
        self.sessions.entry(id.to_owned()).or_default().last_used = now;
        self.evict(&|other| other == id || keep(other));
    }

    fn evict(&mut self, keep: &dyn Fn(&str) -> bool) {
        while self.sessions.len() > MAX_SESSIONS {
            let Some(oldest) = self
                .sessions
                .iter()
                .filter(|(id, _)| !keep(id))
                .min_by(|(a_id, a), (b_id, b)| {
                    a.last_used.cmp(&b.last_used).then_with(|| a_id.cmp(b_id))
                })
                .map(|(id, _)| id.clone())
            else {
                return;
            };
            self.sessions.remove(&oldest);
        }
    }

    /// Record a toplevel's state. A session already holding
    /// [`MAX_TOPLEVELS`] other names records no more.
    pub fn record(&mut self, id: &str, name: &str, state: ToplevelState, now: i64) -> bool {
        let record = self.sessions.entry(id.to_owned()).or_default();
        if record.toplevels.len() >= MAX_TOPLEVELS && !record.toplevels.contains_key(name) {
            return false;
        }
        record.last_used = now;
        record.toplevels.insert(name.to_owned(), state);
        true
    }

    /// A stored toplevel state.
    pub fn toplevel(&self, id: &str, name: &str) -> Option<&ToplevelState> {
        self.sessions.get(id)?.toplevels.get(name)
    }

    /// Forget a toplevel; whether it was stored.
    pub fn forget(&mut self, id: &str, name: &str) -> bool {
        self.sessions
            .get_mut(id)
            .is_some_and(|record| record.toplevels.remove(name).is_some())
    }

    /// Carry a toplevel's state over to a new name; whether one moved.
    pub fn rename(&mut self, id: &str, from: &str, to: &str) -> bool {
        let Some(record) = self.sessions.get_mut(id) else {
            return false;
        };
        match record.toplevels.remove(from) {
            Some(state) => {
                record.toplevels.insert(to.to_owned(), state);
                true
            }
            None => false,
        }
    }

    /// Delete a session; whether it existed.
    pub fn delete(&mut self, id: &str) -> bool {
        self.sessions.remove(id).is_some()
    }
}

/// Where the store lives: `$XDG_DATA_HOME/tuna/window-sessions.json`
/// (Mutter keeps its sessions under the data directory too).
pub fn default_store_path() -> Option<PathBuf> {
    store_path_in(std::env::var_os("XDG_DATA_HOME"), std::env::var_os("HOME"))
}

fn store_path_in(
    data_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    data_home
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            home.map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .map(|home| home.join(".local/share"))
        })
        .map(|data| data.join("tuna").join("window-sessions.json"))
}

/// Replace `path` with `bytes`: a private file in a private directory,
/// synced, then renamed over the old one.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("store path has no directory"))?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("store path has no file name"))?;
    let tmp = dir.join(format!(".{}.tmp", name.to_string_lossy()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, path)
}

/// Writes in generation order: a write older than the last one made is
/// dropped, so a slow background save never lands over the final one.
#[derive(Debug, Default)]
struct Writer {
    written: Mutex<u64>,
}

impl Writer {
    fn write(&self, generation: u64, path: &Path, bytes: &[u8]) {
        let mut written = self.written.lock().unwrap_or_else(|e| e.into_inner());
        if generation <= *written {
            return;
        }
        match write_atomic(path, bytes) {
            Ok(()) => *written = generation,
            Err(error) => eprintln!(
                "tuna-compositor: could not save window sessions to {}: {error}",
                path.display()
            ),
        }
    }
}

/// A session object a client holds.
#[derive(Debug)]
struct LiveSession {
    serial: u64,
    client: ClientId,
    resource: XdgSessionV1,
    /// Toplevels in it by name.
    toplevels: HashMap<String, WlSurface>,
}

/// A toplevel added to (or restored from) a live session.
#[derive(Debug, Clone)]
pub(crate) struct Binding {
    session: String,
    serial: u64,
    name: String,
}

/// A restore the window manager applies when it maps the toplevel (or
/// at once, when it is mapped already).
#[derive(Debug)]
pub(crate) struct PendingRestore {
    pub(crate) surface: WlSurface,
    pub(crate) state: ToplevelState,
    handle: XdgToplevelSessionV1,
}

impl PendingRestore {
    /// Tell the client this toplevel is being restored: before the
    /// configure that carries the restored state.
    pub(crate) fn announce(&self) {
        if self.handle.is_alive() {
            self.handle.restored();
        }
    }
}

/// Session object user data: the id and the serial of the live entry it
/// was created for. Serial 0 is an inert object (refused or replaced).
#[derive(Debug, Clone)]
pub struct SessionHandle {
    id: String,
    serial: u64,
}

impl SessionHandle {
    fn inert() -> Self {
        Self {
            id: String::new(),
            serial: 0,
        }
    }
}

/// Toplevel session object user data; `None` when inert.
#[derive(Debug, Clone)]
pub struct ToplevelHandle {
    surface: Option<WlSurface>,
}

/// Marker in a toplevel's surface data once it has committed.
struct ToplevelCommitted;

/// Remember when a new toplevel first commits: `restore_toplevel` is
/// refused after that (`already_mapped`).
pub(crate) fn track_initial_commit(surface: &WlSurface) {
    add_post_commit_hook::<State, _>(surface, |_, _, surface| {
        with_states(surface, |states| {
            states
                .data_map
                .insert_if_missing_threadsafe(|| ToplevelCommitted);
        });
    });
}

fn has_committed(surface: &WlSurface) -> bool {
    with_states(surface, |states| {
        states.data_map.get::<ToplevelCommitted>().is_some()
    })
}

fn now_micros() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0)
}

/// A random version 4 UUID, as `g_uuid_string_random` makes them.
fn random_uuid() -> String {
    let mut bytes: [u8; 16] = rand::random();
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// The output whose area holds the rectangle's centre.
pub(crate) fn output_at(
    outputs: &[OutputEntry],
    rect: Rectangle<i32, Logical>,
) -> Option<&OutputEntry> {
    let (cx, cy) = (rect.loc.x + rect.size.w / 2, rect.loc.y + rect.size.h / 2);
    outputs.iter().find(|entry| {
        cx >= entry.loc.0
            && cx < entry.loc.0 + entry.size.w
            && cy >= entry.loc.1
            && cy < entry.loc.1 + entry.size.h
    })
}

/// A restored floating rectangle, kept where it was when an output still
/// shows it. Otherwise it moves inside the work area (below the top bar,
/// `work_top` high) of its saved output, or the primary one, shrinking
/// to fit. With no outputs at all it is left as saved.
pub(crate) fn constrain_restored(
    outputs: &[OutputEntry],
    rect: Rectangle<i32, Logical>,
    saved_output: Option<&str>,
    work_top: i32,
) -> Rectangle<i32, Logical> {
    if outputs.is_empty() || output_at(outputs, rect).is_some() {
        return rect;
    }
    let Some(target) = saved_output
        .and_then(|name| outputs.iter().find(|entry| entry.name == name))
        .or_else(|| outputs.iter().find(|entry| entry.primary))
        .or_else(|| outputs.first())
    else {
        return rect;
    };
    let (x, y) = (target.loc.0, target.loc.1 + work_top);
    let (w, h) = (target.size.w, (target.size.h - work_top).max(1));
    let size = (rect.size.w.min(w).max(1), rect.size.h.min(h).max(1));
    let loc = (
        rect.loc.x.clamp(x, x + w - size.0),
        rect.loc.y.clamp(y, y + h - size.1),
    );
    Rectangle::new(loc.into(), size.into())
}

/// The protocol global and every session: what is stored, what clients
/// hold, and which toplevels belong to which session.
pub struct WindowSessions {
    _global: GlobalId,
    store: SessionStore,
    path: Option<PathBuf>,
    writer: Arc<Writer>,
    generation: u64,
    dirty: bool,
    save_at: Option<Instant>,
    live: HashMap<String, LiveSession>,
    bindings: HashMap<WlSurface, Binding>,
    /// Toplevels of sessions the client let go: their state is recorded
    /// once more on the window manager's next pass.
    finalizing: Vec<(WlSurface, Binding)>,
    restores: Vec<PendingRestore>,
    next_serial: u64,
}

impl WindowSessions {
    /// Advertise the global. Nothing is stored or read until
    /// [`attach_store`](Self::attach_store) names the file.
    pub(crate) fn new(dh: &DisplayHandle) -> Self {
        Self {
            _global: dh.create_global::<State, XdgSessionManagerV1, ()>(VERSION, ()),
            store: SessionStore::default(),
            path: None,
            writer: Arc::new(Writer::default()),
            generation: 0,
            dirty: false,
            save_at: None,
            live: HashMap::new(),
            bindings: HashMap::new(),
            finalizing: Vec::new(),
            restores: Vec::new(),
            next_serial: 0,
        }
    }

    /// Load the sessions saved at `path` and save there from now on.
    pub fn attach_store(&mut self, path: PathBuf) {
        self.store = SessionStore::load(&path);
        self.path = Some(path);
        self.dirty = false;
        self.save_at = None;
    }

    /// The sessions as stored now (including unsaved changes).
    pub fn store(&self) -> &SessionStore {
        &self.store
    }

    /// When the pending save is due.
    pub fn save_deadline(&self) -> Option<Instant> {
        self.save_at
    }

    /// Start the pending save once it is due; the file is written off
    /// the compositor thread, as Mutter writes from a worker.
    pub fn save_if_due(&mut self, now: Instant) {
        if self.save_at.is_some_and(|at| at <= now) {
            self.save_at = None;
            if let Some((generation, path, bytes)) = self.snapshot() {
                let writer = self.writer.clone();
                let spawned = std::thread::Builder::new()
                    .name("tuna-window-sessions".into())
                    .spawn(move || writer.write(generation, &path, &bytes));
                if let Err(error) = spawned {
                    eprintln!("tuna-compositor: window session save thread: {error}");
                }
            }
        }
    }

    /// Write every change now, on this thread (compositor exit).
    pub fn save_now(&mut self) {
        self.save_at = None;
        if let Some((generation, path, bytes)) = self.snapshot() {
            self.writer.write(generation, &path, &bytes);
        }
    }

    fn snapshot(&mut self) -> Option<(u64, PathBuf, Vec<u8>)> {
        if !self.dirty {
            return None;
        }
        let path = self.path.clone()?;
        self.dirty = false;
        self.generation += 1;
        Some((self.generation, path, self.store.to_json().into_bytes()))
    }

    fn changed(&mut self) {
        self.dirty = true;
        if self.save_at.is_none() {
            self.save_at = Some(Instant::now() + SAVE_DELAY);
        }
    }

    fn live_session(&self, handle: &SessionHandle) -> Option<&LiveSession> {
        self.live
            .get(&handle.id)
            .filter(|live| handle.serial != 0 && live.serial == handle.serial)
    }

    /// Drop a live session without recording its toplevels again (it was
    /// replaced or removed).
    fn forget_live(&mut self, id: &str) {
        if let Some(live) = self.live.remove(id) {
            for surface in live.toplevels.values() {
                self.bindings.remove(surface);
            }
        }
    }

    /// Whether `surface` belongs to a session.
    pub(crate) fn is_bound(&self, surface: &WlSurface) -> bool {
        self.bindings.contains_key(surface)
            || self.finalizing.iter().any(|(other, _)| other == surface)
    }

    /// Every toplevel in a session, with what the window manager last
    /// placed: recorded at compositor exit, as Mutter records each window
    /// it unmanages when its display closes.
    pub(crate) fn record_all(&mut self, snapshot: impl Fn(&WlSurface) -> Option<ToplevelState>) {
        let now = now_micros();
        let bound: Vec<(WlSurface, Binding)> = self
            .finalizing
            .drain(..)
            .chain(self.bindings.iter().map(|(s, b)| (s.clone(), b.clone())))
            .collect();
        for (surface, binding) in bound {
            if let Some(state) = snapshot(&surface) {
                if self
                    .store
                    .record(&binding.session, &binding.name, state, now)
                {
                    self.dirty = true;
                }
            }
        }
    }

    /// Record the toplevels of sessions the client let go, then forget
    /// them (they are inert now).
    pub(crate) fn record_finalizing(
        &mut self,
        snapshot: impl Fn(&WlSurface) -> Option<ToplevelState>,
    ) {
        if self.finalizing.is_empty() {
            return;
        }
        let now = now_micros();
        let mut recorded = false;
        for (surface, binding) in std::mem::take(&mut self.finalizing) {
            if let Some(state) = snapshot(&surface) {
                recorded |= self
                    .store
                    .record(&binding.session, &binding.name, state, now);
            }
        }
        if recorded {
            self.changed();
        }
    }

    /// A toplevel's window is going away: record its last state
    /// (Mutter's `on_window_unmanaging`) and let it go.
    pub(crate) fn record_unmapped(&mut self, surface: &WlSurface, state: Option<ToplevelState>) {
        let binding = match self.bindings.remove(surface) {
            Some(binding) => {
                if let Some(live) = self
                    .live
                    .get_mut(&binding.session)
                    .filter(|live| live.serial == binding.serial)
                {
                    live.toplevels.remove(&binding.name);
                }
                Some(binding)
            }
            None => self
                .finalizing
                .iter()
                .position(|(other, _)| other == surface)
                .map(|at| self.finalizing.swap_remove(at).1),
        };
        self.restores.retain(|pending| &pending.surface != surface);
        if let (Some(binding), Some(state)) = (binding, state) {
            if self
                .store
                .record(&binding.session, &binding.name, state, now_micros())
            {
                self.changed();
            }
        }
    }

    /// The restore waiting for this toplevel, if any.
    pub(crate) fn take_restore(&mut self, surface: &WlSurface) -> Option<PendingRestore> {
        let at = self
            .restores
            .iter()
            .position(|pending| &pending.surface == surface)?;
        Some(self.restores.swap_remove(at))
    }

    /// Restores for toplevels the window manager already maps; those of
    /// toplevels that are gone are dropped.
    pub(crate) fn take_mapped_restores(
        &mut self,
        mapped: impl Fn(&WlSurface) -> bool,
    ) -> Vec<PendingRestore> {
        if self.restores.is_empty() {
            return Vec::new();
        }
        self.restores.retain(|pending| pending.surface.is_alive());
        let (ready, waiting): (Vec<_>, Vec<_>) = std::mem::take(&mut self.restores)
            .into_iter()
            .partition(|pending| mapped(&pending.surface));
        self.restores = waiting;
        ready
    }

    fn get_session(
        &mut self,
        client: &Client,
        manager: &XdgSessionManagerV1,
        id: New<XdgSessionV1>,
        requested: Option<String>,
        data_init: &mut DataInit<'_, State>,
    ) {
        // An id that names no stored session is the same as none.
        let requested = requested.filter(|id| self.store.contains(id));
        let created = requested.is_none();
        if let Some(existing) = requested.as_deref() {
            if let Some(previous) = self.live.get(existing) {
                if previous.client == client.id() {
                    data_init.init(id, SessionHandle::inert());
                    manager.post_error(
                        xdg_session_manager_v1::Error::InUse,
                        format!("Session {existing} already in use"),
                    );
                    return;
                }
                // Another client takes it over.
                previous.resource.replaced();
                self.forget_live(existing);
            }
        }
        let name = requested.unwrap_or_else(|| loop {
            let fresh = random_uuid();
            if !self.store.contains(&fresh) && !self.live.contains_key(&fresh) {
                break fresh;
            }
        });
        let live = &self.live;
        self.store
            .open(&name, now_micros(), &|other| live.contains_key(other));
        self.dirty = true;
        self.next_serial += 1;
        let serial = self.next_serial;
        let resource = data_init.init(
            id,
            SessionHandle {
                id: name.clone(),
                serial,
            },
        );
        if created {
            resource.created(name.clone());
        } else {
            resource.restored();
        }
        self.live.insert(
            name,
            LiveSession {
                serial,
                client: client.id(),
                resource,
                toplevels: HashMap::new(),
            },
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn bind_toplevel(
        &mut self,
        xdg: &smithay::wayland::shell::xdg::XdgShellState,
        session: &XdgSessionV1,
        handle: &SessionHandle,
        id: New<XdgToplevelSessionV1>,
        toplevel: &wayland_protocols::xdg::shell::server::xdg_toplevel::XdgToplevel,
        name: String,
        restore: bool,
        data_init: &mut DataInit<'_, State>,
    ) {
        // The toplevel's surface, while its session is live; an inert
        // handle otherwise.
        let surface = xdg
            .get_toplevel(toplevel)
            .map(|top| top.wl_surface().clone())
            .filter(|_| self.live_session(handle).is_some());
        let refusal = surface.as_ref().and_then(|surface| {
            let in_use = self
                .live_session(handle)
                .is_some_and(|live| live.toplevels.contains_key(&name))
                || (!restore && self.store.toplevel(&handle.id, &name).is_some());
            if in_use {
                Some((
                    xdg_session_v1::Error::NameInUse,
                    "Name of toplevel is already in use",
                ))
            } else if self.is_bound(surface) {
                Some((
                    xdg_session_v1::Error::AlreadyAdded,
                    "Toplevel already added",
                ))
            } else if restore && has_committed(surface) {
                Some((
                    xdg_session_v1::Error::AlreadyMapped,
                    "Tried to restore an already mapped toplevel",
                ))
            } else {
                None
            }
        });
        let accepted = surface.filter(|_| refusal.is_none());
        let resource = data_init.init(
            id,
            ToplevelHandle {
                surface: accepted.clone(),
            },
        );
        if let Some((code, message)) = refusal {
            session.post_error(code, message);
        }
        let Some(surface) = accepted else {
            return;
        };
        if let Some(live) = self.live.get_mut(&handle.id) {
            live.toplevels.insert(name.clone(), surface.clone());
        }
        self.bindings.insert(
            surface.clone(),
            Binding {
                session: handle.id.clone(),
                serial: handle.serial,
                name: name.clone(),
            },
        );
        if !restore {
            return;
        }
        if let Some(state) = self
            .store
            .toplevel(&handle.id, &name)
            .filter(|state| state.restorable())
        {
            self.restores.push(PendingRestore {
                surface,
                state: state.clone(),
                handle: resource,
            });
        }
    }

    /// Forget a name: its toplevel leaves the session and its stored
    /// state goes, whether or not a window holds the name this run (the
    /// protocol's "remove any data related to the toplevel"; Mutter 51
    /// acts only on a name a window holds).
    fn remove_toplevel(&mut self, handle: &SessionHandle, name: &str) {
        let Some(live) = self
            .live
            .get_mut(&handle.id)
            .filter(|live| handle.serial != 0 && live.serial == handle.serial)
        else {
            return;
        };
        if let Some(surface) = live.toplevels.remove(name) {
            self.bindings.remove(&surface);
            self.restores.retain(|pending| pending.surface != surface);
        }
        if self.store.forget(&handle.id, name) {
            self.changed();
        }
    }

    fn remove_session(&mut self, handle: &SessionHandle) {
        if self.live_session(handle).is_none() {
            return;
        }
        if let Some(live) = self.live.get(&handle.id) {
            let surfaces: Vec<WlSurface> = live.toplevels.values().cloned().collect();
            self.restores
                .retain(|pending| !surfaces.contains(&pending.surface));
        }
        self.forget_live(&handle.id);
        if self.store.delete(&handle.id) {
            self.changed();
        }
    }

    /// The client destroyed its session object (or went away): the state
    /// stays, its toplevels are recorded once more and become inert.
    fn session_gone(&mut self, handle: &SessionHandle) {
        if self.live_session(handle).is_none() {
            return;
        }
        if let Some(live) = self.live.remove(&handle.id) {
            for surface in live.toplevels.into_values() {
                if let Some(binding) = self.bindings.remove(&surface) {
                    self.finalizing.push((surface, binding));
                }
            }
        }
    }

    fn rename(&mut self, resource: &XdgToplevelSessionV1, handle: &ToplevelHandle, name: String) {
        let Some(surface) = handle.surface.as_ref() else {
            return;
        };
        let Some(binding) = self.bindings.get(surface).cloned() else {
            return;
        };
        if binding.name == name {
            return;
        }
        let Some(live) = self
            .live
            .get_mut(&binding.session)
            .filter(|live| live.serial == binding.serial)
        else {
            return;
        };
        if live.toplevels.contains_key(&name)
            || self.store.toplevel(&binding.session, &name).is_some()
        {
            resource.post_error(
                xdg_session_v1::Error::NameInUse,
                "Name of toplevel is already in use",
            );
            return;
        }
        if let Some(surface) = live.toplevels.remove(&binding.name) {
            live.toplevels.insert(name.clone(), surface);
        }
        if self.store.rename(&binding.session, &binding.name, &name) {
            self.changed();
        }
        if let Some(bound) = self.bindings.get_mut(surface) {
            bound.name = name;
        }
    }
}

impl GlobalDispatch<XdgSessionManagerV1, ()> for State {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<XdgSessionManagerV1>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<XdgSessionManagerV1, ()> for State {
    fn request(
        state: &mut Self,
        client: &Client,
        resource: &XdgSessionManagerV1,
        request: xdg_session_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            xdg_session_manager_v1::Request::GetSession {
                id,
                reason,
                session_id,
            } => {
                if !matches!(reason, WEnum::Value(_)) {
                    data_init.init(id, SessionHandle::inert());
                    resource.post_error(
                        xdg_session_manager_v1::Error::InvalidReason,
                        "Invalid reason",
                    );
                    return;
                }
                state
                    .window_sessions
                    .get_session(client, resource, id, session_id, data_init);
            }
            xdg_session_manager_v1::Request::Destroy => {}
        }
    }
}

impl Dispatch<XdgSessionV1, SessionHandle> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &XdgSessionV1,
        request: xdg_session_v1::Request,
        handle: &SessionHandle,
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            xdg_session_v1::Request::AddToplevel { id, toplevel, name } => {
                state.window_sessions.bind_toplevel(
                    &state.xdg_shell_state,
                    resource,
                    handle,
                    id,
                    &toplevel,
                    name,
                    false,
                    data_init,
                );
            }
            xdg_session_v1::Request::RestoreToplevel { id, toplevel, name } => {
                state.window_sessions.bind_toplevel(
                    &state.xdg_shell_state,
                    resource,
                    handle,
                    id,
                    &toplevel,
                    name,
                    true,
                    data_init,
                );
            }
            xdg_session_v1::Request::RemoveToplevel { name } => {
                state.window_sessions.remove_toplevel(handle, &name);
            }
            xdg_session_v1::Request::Remove => {
                state.window_sessions.remove_session(handle);
            }
            xdg_session_v1::Request::Destroy => {}
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: ClientId,
        _resource: &XdgSessionV1,
        handle: &SessionHandle,
    ) {
        state.window_sessions.session_gone(handle);
    }
}

impl Dispatch<XdgToplevelSessionV1, ToplevelHandle> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &XdgToplevelSessionV1,
        request: xdg_toplevel_session_v1::Request,
        handle: &ToplevelHandle,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            xdg_toplevel_session_v1::Request::Rename { name } => {
                state.window_sessions.rename(resource, handle, name);
            }
            // Destroying the handle leaves the window in its session.
            xdg_toplevel_session_v1::Request::Destroy => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Logical> {
        Rectangle::new((x, y).into(), (w, h).into())
    }

    fn output(name: &str, loc: (i32, i32), w: i32, h: i32, primary: bool) -> OutputEntry {
        OutputEntry {
            output: None,
            global: None,
            name: name.to_owned(),
            size: (w, h).into(),
            loc,
            primary,
        }
    }

    fn floating() -> ToplevelState {
        ToplevelState {
            state: WindowState::Floating,
            floating_rect: Some(rect(120, 90, 640, 480)),
            tiled_rect: None,
            minimized: false,
            workspace: 1,
            output: Some("DP-1".into()),
        }
    }

    #[test]
    fn store_round_trips_mutter_keys_and_state_numbers() {
        let mut store = SessionStore::default();
        store.open("a", 5, &|_| false);
        assert!(store.record("a", "main", floating(), 7));
        let maximized = ToplevelState {
            state: WindowState::Maximized,
            floating_rect: Some(rect(10, 40, 500, 400)),
            tiled_rect: Some(rect(0, 32, 1280, 768)),
            minimized: true,
            workspace: 0,
            output: None,
        };
        assert!(store.record("a", "aux", maximized.clone(), 8));
        let text = store.to_json();
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["version"], 1);
        let session = &value["sessions"]["a"];
        assert_eq!(session["version"], 1);
        assert_eq!(session["last-used"], 8);
        let main = &session["toplevels"]["main"];
        assert_eq!(main["state"], 1);
        assert_eq!(main["floating-rect"], json!([120, 90, 640, 480]));
        assert!(main.get("tiled-rect").is_none());
        assert_eq!(main["is-minimized"], false);
        assert_eq!(main["workspace"], 1);
        assert_eq!(main["output"], "DP-1");
        assert_eq!(session["toplevels"]["aux"]["state"], 2);
        assert_eq!(
            session["toplevels"]["aux"]["tiled-rect"],
            json!([0, 32, 1280, 768])
        );
        let back = SessionStore::parse(&text).unwrap();
        assert_eq!(back, store);
        assert_eq!(back.toplevel("a", "aux"), Some(&maximized));
        for (code, state) in [
            (0, WindowState::None),
            (1, WindowState::Floating),
            (2, WindowState::Maximized),
            (3, WindowState::TiledLeft),
            (4, WindowState::TiledRight),
            (5, WindowState::Fullscreen),
        ] {
            assert_eq!(state.code(), code);
            assert_eq!(WindowState::from_code(code), Some(state));
        }
        assert_eq!(WindowState::from_code(6), None);
    }

    #[test]
    fn newer_session_data_restores_nothing_but_the_session_stays_known() {
        let text = r#"{"version":1,"sessions":{
            "new":{"version":2,"last-used":9,"toplevels":{"main":{"state":1,"floating-rect":[1,2,3,4]}}},
            "old":{"version":1,"last-used":3,"toplevels":{"main":{"state":1,"floating-rect":[1,2,3,4]}}}}}"#;
        let store = SessionStore::parse(text).unwrap();
        assert!(store.contains("new"));
        assert!(store.session("new").unwrap().toplevels.is_empty());
        assert!(store.toplevel("old", "main").unwrap().restorable());
        assert!(SessionStore::parse(r#"{"version":2,"sessions":{}}"#).is_err());
        assert!(SessionStore::parse(r#"{"sessions":{}}"#).is_err());
        assert!(SessionStore::parse("not json").is_err());
    }

    #[test]
    fn wrongly_typed_fields_are_skipped_not_the_toplevel() {
        let text = r#"{"version":1,"sessions":{"s":{"version":1,"toplevels":{
            "a":{"state":"floating","floating-rect":[1,2,3,4],"workspace":2},
            "b":{"state":1,"floating-rect":[1,2,0,4],"is-minimized":"yes","workspace":"x"},
            "c":{"state":9,"tiled-rect":[0,0,10,10,5],"output":7},
            "d":{"state":2,"floating-rect":[1,2,3,4999999999],"workspace":-7}}}}}"#;
        let store = SessionStore::parse(text).unwrap();
        let a = store.toplevel("s", "a").unwrap();
        assert_eq!(
            (a.state, a.floating_rect, a.workspace),
            (WindowState::None, Some(rect(1, 2, 3, 4)), 2)
        );
        assert!(!a.restorable(), "an unknown state restores nothing");
        let b = store.toplevel("s", "b").unwrap();
        assert_eq!(
            (b.state, b.floating_rect, b.minimized, b.workspace),
            (WindowState::Floating, None, false, -1)
        );
        assert!(!b.restorable(), "floating without a usable rectangle");
        let c = store.toplevel("s", "c").unwrap();
        assert_eq!(
            (c.state, c.tiled_rect, c.output.clone()),
            (WindowState::None, None, None)
        );
        let d = store.toplevel("s", "d").unwrap();
        assert_eq!(
            (d.state, d.floating_rect, d.workspace),
            (WindowState::Maximized, None, -1)
        );
        assert!(d.restorable(), "maximize needs no rectangle");
    }

    #[test]
    fn missing_corrupt_and_oversized_files_load_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("window-sessions.json");
        assert!(SessionStore::load(&path).is_empty());
        std::fs::write(&path, "{corrupt").unwrap();
        assert!(SessionStore::load(&path).is_empty());
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(MAX_FILE_BYTES + 1).unwrap();
        assert!(SessionStore::load(&path).is_empty());
    }

    #[test]
    fn atomic_writes_are_private_and_never_go_back_a_generation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data/tuna/window-sessions.json");
        let writer = Writer::default();
        writer.write(2, &path, b"second");
        writer.write(1, &path, b"first");
        assert_eq!(std::fs::read(&path).unwrap(), b"second");
        use std::os::unix::fs::PermissionsExt as _;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(path.parent().unwrap()), 0o700);
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
    }

    #[test]
    fn least_recently_used_sessions_go_first_but_live_ones_stay() {
        let mut store = SessionStore::default();
        for n in 0..MAX_SESSIONS {
            store.open(&format!("s{n}"), n as i64 + 10, &|_| false);
        }
        // s0 is oldest but protected; s1 goes instead.
        store.open("fresh", 1_000, &|id| id == "s0");
        assert_eq!(store.len(), MAX_SESSIONS);
        assert!(store.contains("s0") && store.contains("fresh"));
        assert!(!store.contains("s1"));
    }

    #[test]
    fn a_session_records_a_bounded_number_of_toplevels() {
        let mut store = SessionStore::default();
        for n in 0..MAX_TOPLEVELS {
            assert!(store.record("s", &format!("t{n}"), floating(), 1));
        }
        assert!(!store.record("s", "one-more", floating(), 2));
        assert!(
            store.record("s", "t0", floating(), 3),
            "updating a known name"
        );
        assert!(store.rename("s", "t0", "renamed"));
        assert!(store.toplevel("s", "t0").is_none());
        assert_eq!(store.toplevel("s", "renamed"), Some(&floating()));
        assert!(store.forget("s", "renamed") && !store.forget("s", "renamed"));
        assert!(store.delete("s") && !store.contains("s"));
    }

    #[test]
    fn restored_rectangles_stay_where_an_output_shows_them() {
        let outputs = [
            output("DP-1", (0, 0), 1280, 800, true),
            output("HDMI-1", (1280, 0), 1920, 1080, false),
        ];
        let on_second = rect(1500, 300, 800, 600);
        assert_eq!(
            constrain_restored(&outputs, on_second, Some("HDMI-1"), 32),
            on_second
        );
        // HDMI-1 unplugged: back onto the primary's work area, inside it.
        let primary_only = [output("DP-1", (0, 0), 1280, 800, true)];
        assert_eq!(
            constrain_restored(&primary_only, on_second, Some("HDMI-1"), 32),
            rect(480, 200, 800, 600)
        );
        // Too big for the output it lands on: shrunk to the work area.
        assert_eq!(
            constrain_restored(&primary_only, rect(4000, -900, 3000, 2000), None, 32),
            rect(0, 32, 1280, 768)
        );
        // The saved output still exists elsewhere: the window goes there.
        let moved = [
            output("DP-1", (0, 0), 1280, 800, true),
            output("HDMI-1", (0, 800), 1920, 1080, false),
        ];
        assert_eq!(
            constrain_restored(&moved, rect(1500, 300, 800, 600), Some("HDMI-1"), 32),
            rect(1120, 832, 800, 600)
        );
        assert_eq!(constrain_restored(&[], on_second, None, 32), on_second);
        assert_eq!(
            output_at(&outputs, on_second).map(|o| o.name.as_str()),
            Some("HDMI-1")
        );
    }

    #[test]
    fn store_lives_under_the_data_home() {
        assert_eq!(
            store_path_in(Some("/data".into()), Some("/home/u".into())),
            Some(PathBuf::from("/data/tuna/window-sessions.json"))
        );
        assert_eq!(
            store_path_in(Some("relative".into()), Some("/home/u".into())),
            Some(PathBuf::from(
                "/home/u/.local/share/tuna/window-sessions.json"
            ))
        );
        assert_eq!(store_path_in(None, Some("rel".into())), None);
    }

    #[test]
    fn session_ids_are_version_4_uuids() {
        let id = random_uuid();
        assert_eq!(id.len(), 36);
        let parts: Vec<&str> = id.split('-').collect();
        assert_eq!(
            parts.iter().map(|p| p.len()).collect::<Vec<_>>(),
            [8, 4, 4, 4, 12]
        );
        assert!(parts[2].starts_with('4'));
        assert!(matches!(&parts[3][..1], "8" | "9" | "a" | "b"));
        assert_ne!(id, random_uuid());
    }
}
