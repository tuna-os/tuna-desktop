//! Nested session runtime (001 T1, ADR 0001).
//!
//! Owns the Smithay winit/calloop session: backend creation, client
//! acceptance over a private socket, output setup, per-tick dispatch,
//! frame production, window management, input routing, shell supervision
//! (T5), and the control channel. The loop keeps the host environment
//! untouched.
//!
//! The loop shape follows upstream `examples/minimal.rs` at the pinned
//! revision (manual repaint each round, toplevels stacked at the origin);
//! damage-tracked repaints use the same [`native_repaint`](crate::native_repaint)
//! machinery as the native path, so an unchanged signature submits nothing.
//! Layer-shell is served from [`State`](crate::State) (see [`crate::layer`])
//! so the supervised panel attaches over this same display.

use std::ffi::OsString;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use calloop::{EventLoop, LoopHandle};
use smithay::{
    backend::{
        renderer::{
            element::{
                surface::{render_elements_from_surface_tree, WaylandSurfaceRenderElement},
                Kind,
            },
            gles::GlesRenderer,
            Bind, Color32F, Frame, Renderer,
        },
        winit::{self, WinitEvent},
    },
    desktop::utils::send_frames_surface_tree,
    output::{Mode, Output, PhysicalProperties, Scale, Subpixel},
    reexports::wayland_server::Display,
    utils::{Logical, Point, Rectangle, Transform},
    wayland::{seat::WaylandFocus, socket::ListeningSocketSource},
};

use crate::control::{ControlHub, PeerGate};
use crate::lock::{content_visible, SessionLock, DEFAULT_IDLE_TIMEOUT_MS};
use crate::occlusion::FrameCost;

/// Idle timeout for the session lock: `TUNA_IDLE_TIMEOUT_MS` overrides
/// the five-minute default. Testability seam for scripted lock capture
/// (and lock journey tests); production runs leave it unset.
fn idle_timeout_ms() -> u64 {
    std::env::var("TUNA_IDLE_TIMEOUT_MS")
        .ok()
        .and_then(|raw| raw.parse().ok())
        .filter(|ms| *ms > 0)
        .unwrap_or(DEFAULT_IDLE_TIMEOUT_MS)
}
use crate::overlay::{overlay_key_for_keycode, Overlay, OverlayWindow};
use crate::state::TokenStore;
use crate::supervise::{RecoveryAction, RestartPolicy, ShellDriver, ShellStatus};
use crate::wallpaper::Wallpaper;
use crate::windows::{
    translate_input, ManagerInput, TriggerAction, TriggerState, WindowManager, ESCAPE_KEYCODE,
};
use crate::xwayland::XWaylandSupervisor;

use crate::{ClientState, State};

mod aids;
mod auth;
mod color;
mod display;
mod motion;
mod osk;

/// Display-rate loop pacing: while anything moves, dispatch blocks at
/// most this long, so animations step near 60 Hz and the loop never
/// spins. The nested (winit) backend always runs at this pace.
const FRAME_BUDGET: Duration = Duration::from_millis(16);

/// Longest the native loop sleeps with nothing due (#503). Producers
/// without an event source of their own (the XWayland supervisor's
/// backoff, the X11 icon reader, a stalled client flush) are still
/// served at least this often.
const IDLE_WAKE: Duration = Duration::from_millis(500);

/// Display-rate ticks continue this long after the last input event or
/// power-on (#503): an animation whose first steps draw nothing new still
/// advances. A queued frame alone does not extend it.
const ACTIVITY_LINGER: Duration = Duration::from_millis(500);

/// Administrative lockdown (#357): how often the tick polls the live
/// `disable-lock-screen` key. Each poll spawns one background reader at
/// most and drains completed reads without waiting (see
/// `ControlHub::refresh_lockdown`), skipped without a session bus; the
/// shell enforces its own surface immediately, so this backstop's short
/// lag fails safe (an extra lock in the window still needs auth to
/// unlock, and unlocks never come from policy).
const LOCKDOWN_REFRESH_INTERVAL: Duration = Duration::from_secs(2);

/// Wake the native loop for requests on already accepted Wayland clients.
/// The listening socket only signals new connections. The display owns the
/// client epoll set; a duplicate keeps calloop's source lifetime independent.
fn display_readiness_source(
    display: &Display<State>,
) -> std::io::Result<calloop::generic::Generic<std::os::fd::OwnedFd>> {
    use std::os::fd::AsFd;
    Ok(calloop::generic::Generic::new(
        display.as_fd().try_clone_to_owned()?,
        calloop::Interest::READ,
        calloop::Mode::Level,
    ))
}

#[cfg(test)]
mod display_readiness_tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;

    #[test]
    fn modal_parent_fades_run_until_settled() {
        let mut dim = crate::modal_dim::ModalDim::default();
        let alive = std::collections::HashSet::from([1]);
        let policy = tuna_shell_control::motion::MotionPolicy::default();
        dim.step(&alive, &alive, Duration::ZERO, policy, true);
        // A running fade keeps next_wake at the frame pace; a settled one
        // lets the loop sleep.
        assert!(dim.running());
        dim.step(&alive, &alive, Duration::from_secs(1), policy, true);
        assert!(!dim.running());
    }

    #[test]
    fn accepted_wayland_requests_wake_the_loop_and_drained_clients_do_not_spin() {
        let mut compositor = crate::TestCompositor::new();
        let (server, mut client) = UnixStream::pair().unwrap();
        client.set_nonblocking(true).unwrap();
        compositor.add_client(server);
        let mut event_loop = EventLoop::<usize>::try_new().unwrap();
        event_loop
            .handle()
            .insert_source(
                display_readiness_source(&compositor.display).unwrap(),
                |_, _, wakes| {
                    *wakes += 1;
                    Ok(calloop::PostAction::Continue)
                },
            )
            .unwrap();
        let mut wakes = 0;
        event_loop.dispatch(Duration::ZERO, &mut wakes).unwrap();
        assert_eq!(wakes, 0, "idle accepted client must not wake the loop");
        for expected in 1..=2 {
            // Actual wl_display.sync on object 1, callback object 2. The
            // server deletes the callback after done, allowing ID reuse.
            let request: Vec<u8> = [1_u32, 12_u32 << 16, 2_u32]
                .into_iter()
                .flat_map(u32::to_ne_bytes)
                .collect();
            client.write_all(&request).unwrap();
            event_loop.dispatch(Duration::ZERO, &mut wakes).unwrap();
            assert_eq!(wakes, expected, "client request must wake without a timer");
            compositor
                .display
                .dispatch_clients(&mut compositor.state)
                .unwrap();
            event_loop.dispatch(Duration::ZERO, &mut wakes).unwrap();
            assert_eq!(wakes, expected, "draining requests must clear readiness");
            compositor.display.flush_clients().unwrap();
            let mut reply = [0_u8; 24];
            client.read_exact(&mut reply).unwrap();
            assert_eq!(u32::from_ne_bytes(reply[0..4].try_into().unwrap()), 2);
            assert_eq!(
                u32::from_ne_bytes(reply[4..8].try_into().unwrap()),
                12 << 16
            );
            assert_eq!(u32::from_ne_bytes(reply[12..16].try_into().unwrap()), 1);
            assert_eq!(
                u32::from_ne_bytes(reply[16..20].try_into().unwrap()),
                (12 << 16) | 1
            );
            assert_eq!(u32::from_ne_bytes(reply[20..24].try_into().unwrap()), 2);
        }
    }
}

/// Default nested output size (physical pixels).
const DEFAULT_WIDTH: i32 = 1280;
const DEFAULT_HEIGHT: i32 = 800;

/// Nested session configuration: socket identity plus output geometry.
#[derive(Debug, Clone)]
pub struct NestedSession {
    /// Private Wayland socket name, e.g. `tuna-nested-<pid>`.
    pub socket_name: String,
    /// Output width in physical pixels.
    pub width: i32,
    /// Output height in physical pixels.
    pub height: i32,
    /// Output scale (#59): 1, or fractional like GNOME's 125, 150, 175
    /// and 200 percent. Layout works in logical pixels (physical size
    /// divided by this); clients render at it.
    pub scale: f64,
    /// Shell binary to supervise. `None` selects
    /// [`resolve_shell_bin`]: `TUNA_SHELL_BIN`, then the
    /// `tuna-shell-host` sibling of this binary, then `PATH`.
    pub shell_bin: Option<std::path::PathBuf>,
    /// Opt in to X11 compatibility: records the first X11 need at
    /// launch so the supervisor may spawn the server on demand.
    /// `false` (default) keeps the session native-only; the flag is
    /// the v1 trigger until per-app launch requests exist.
    pub xwayland: bool,
    /// Display backend (#52): nested window or hardware session.
    pub backend: BackendChoice,
    /// Open the overview when the session starts, as GNOME Shell does at
    /// login. `tuna-session` (real logins) sets it; developer and proof
    /// runs start on the desktop.
    pub startup_overview: bool,
    /// A nested session owns the screen reader as a hardware session
    /// does: it serves `org.freedesktop.a11y.Manager` and runs Orca while
    /// `screen-reader-enabled` is set. Off by default because a nested
    /// session normally runs inside a desktop that already has its own
    /// reader and accessibility manager (`--screen-reader`).
    pub screen_reader: bool,
}

/// Which display backend a session runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendChoice {
    /// Hardware when no host display exists (started by the display
    /// manager or from a TTY), nested otherwise.
    Auto,
    /// Nested window inside a host Wayland/X11 session.
    Winit,
    /// DRM/KMS hardware session (needs the `drm` feature).
    Drm,
}

impl BackendChoice {
    /// Parse a `--backend` value.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "auto" => Some(Self::Auto),
            "winit" | "nested" => Some(Self::Winit),
            "drm" | "kms" => Some(Self::Drm),
            _ => None,
        }
    }

    /// Resolve `Auto` against the environment: a host display means
    /// nested. Pure so the rule is testable without a display.
    pub fn resolve(self, has_host_display: bool) -> Self {
        match self {
            Self::Auto if has_host_display || !cfg!(feature = "drm") => Self::Winit,
            Self::Auto => Self::Drm,
            other => other,
        }
    }
}

/// Whether a host Wayland or X11 display is reachable from our env.
pub fn has_host_display() -> bool {
    ["WAYLAND_DISPLAY", "DISPLAY"]
        .iter()
        .any(|key| std::env::var_os(key).is_some_and(|v| !v.is_empty()))
}

/// The live display backend.
enum Backend {
    Winit(Box<winit::WinitGraphicsBackend<GlesRenderer>>),
    #[cfg(feature = "drm")]
    Drm(Box<crate::drm::DrmBackend>),
}

impl NestedSession {
    /// Session with an explicit socket name and size.
    pub fn new(socket_name: String, width: i32, height: i32) -> Self {
        Self {
            socket_name,
            width,
            height,
            scale: 1.0,
            shell_bin: None,
            xwayland: false,
            backend: BackendChoice::Auto,
            startup_overview: false,
            screen_reader: false,
        }
    }

    /// Default session: unique socket name from our pid, default size.
    pub fn default_for_pid() -> Self {
        Self::new(
            format!("tuna-nested-{}", std::process::id()),
            DEFAULT_WIDTH,
            DEFAULT_HEIGHT,
        )
    }
}

/// Sibling binary next to `dir` when it exists as a file.
/// Shared by the session launcher and the shell resolver so both agree
/// on what "installed side by side" means.
pub fn sibling_binary(dir: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
    let candidate = dir.join(name);
    candidate.is_file().then_some(candidate)
}

/// Directory holding this process's binary, for sibling resolution.
pub fn current_exe_dir() -> Option<std::path::PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.to_owned()))
}

/// Whether a session runs X11 apps (#59): `TUNA_XWAYLAND=1` forces it,
/// `TUNA_XWAYLAND=0` turns it off, and otherwise it is on whenever an
/// `Xwayland` binary is on `PATH`, as GNOME 51 runs X11 apps
/// transparently. Builds without the `xwayland` feature never ask.
pub fn xwayland_wanted(
    env: Option<std::ffi::OsString>,
    path_var: Option<std::ffi::OsString>,
) -> bool {
    if !cfg!(feature = "xwayland") {
        return false;
    }
    match env.as_ref().and_then(|v| v.to_str()) {
        Some("1") => return true,
        Some("0") => return false,
        _ => {}
    }
    path_var
        .map(|p| std::env::split_paths(&p).any(|dir| sibling_binary(&dir, "Xwayland").is_some()))
        .unwrap_or(false)
}

#[cfg(test)]
mod xwayland_wanted_tests {
    use super::*;

    #[test]
    fn on_by_default_when_xwayland_is_installed() {
        let dir = tempfile::tempdir().unwrap();
        let path = Some(dir.path().as_os_str().to_owned());
        let enabled = cfg!(feature = "xwayland");
        assert!(!xwayland_wanted(None, path.clone()), "no Xwayland binary");
        assert_eq!(xwayland_wanted(Some("1".into()), path.clone()), enabled);
        std::fs::write(dir.path().join("Xwayland"), b"").unwrap();
        assert_eq!(xwayland_wanted(None, path.clone()), enabled);
        assert!(
            !xwayland_wanted(Some("0".into()), path.clone()),
            "opt-out wins"
        );
        assert_eq!(xwayland_wanted(Some("".into()), path), enabled);
    }
}

/// Scales GNOME offers, as a range: anything from 1 to 4.
pub fn clamp_scale(scale: f64) -> f64 {
    if scale.is_finite() {
        scale.clamp(1.0, 4.0)
    } else {
        1.0
    }
}

/// Logical size of a physical area at `scale` (rounded, never zero).
pub fn logical_size(width: i32, height: i32, scale: f64) -> (i32, i32) {
    (
        ((f64::from(width) / scale).round() as i32).max(1),
        ((f64::from(height) / scale).round() as i32).max(1),
    )
}

/// Where one output's pixels sit in the global logical space, and how
/// many physical pixels a logical pixel covers there.
#[derive(Debug, Clone, Copy)]
pub struct View {
    /// The output's top-left in the global logical space.
    pub offset: (i32, i32),
    /// Physical pixels per logical pixel.
    pub scale: f64,
}

impl View {
    /// A global logical point in this output's physical pixels.
    pub fn physical(&self, x: f64, y: f64) -> Point<i32, smithay::utils::Physical> {
        (
            ((x - f64::from(self.offset.0)) * self.scale).round() as i32,
            ((y - f64::from(self.offset.1)) * self.scale).round() as i32,
        )
            .into()
    }
}

/// Tells the shell whether it runs nested (`nested`) or as the hardware
/// session (`hardware`).
pub const SESSION_KIND_ENV: &str = "TUNA_SESSION_KIND";

/// Shell binaries in preference order: the GTK4/libadwaita shell
/// (ADR 0006, the default), then the legacy software-drawn shell.
pub const SHELL_BINARIES: [&str; 2] = ["tuna-shell-gtk", "tuna-shell-host"];

/// Shell binary for a session: explicit config, then `TUNA_SHELL_BIN`,
/// then the first of [`SHELL_BINARIES`] beside this binary, then on
/// `PATH`, else the legacy name for a spawn-time lookup.
pub fn resolve_shell_bin(configured: Option<&std::path::Path>) -> std::path::PathBuf {
    resolve_shell_bin_in(
        configured,
        std::env::var_os("TUNA_SHELL_BIN"),
        current_exe_dir(),
        std::env::var_os("PATH"),
    )
}

/// [`resolve_shell_bin`] with its inputs explicit, for tests.
pub fn resolve_shell_bin_in(
    configured: Option<&std::path::Path>,
    env_bin: Option<std::ffi::OsString>,
    exe_dir: Option<std::path::PathBuf>,
    path_var: Option<std::ffi::OsString>,
) -> std::path::PathBuf {
    if let Some(path) = configured {
        return path.to_owned();
    }
    if let Some(path) = env_bin.filter(|p| !p.is_empty()) {
        return std::path::PathBuf::from(path);
    }
    for name in SHELL_BINARIES {
        if let Some(found) = exe_dir.as_deref().and_then(|dir| sibling_binary(dir, name)) {
            return found;
        }
    }
    let dirs: Vec<std::path::PathBuf> = path_var
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    for name in SHELL_BINARIES {
        if let Some(found) = dirs.iter().find_map(|dir| sibling_binary(dir, name)) {
            return found;
        }
    }
    std::path::PathBuf::from(SHELL_BINARIES[1])
}

#[cfg(test)]
mod scene_slot_tests {
    use super::{scene_slots, SceneSlot::*};

    #[test]
    fn frame_bell_stays_under_higher_windows_and_only_in_its_slice() {
        let mut over = scene_slots(2, 0, false, &[0]);
        super::insert_bell_slot(&mut over, 1, 0);
        assert_eq!(over, [Snapshot(0), Element(0), Bell(0), Element(1)]);
        let mut under = scene_slots(2, 2, true, &[]);
        super::insert_bell_slot(&mut under, 1, 0);
        assert_eq!(under, [Element(2), Element(3)]);
    }

    #[test]
    fn snapshots_draw_in_front_of_the_elements_at_and_after_their_index() {
        // Five elements split at 2: snapshot 0 sits at index 1 (over half),
        // snapshot 1 at the boundary 2 (under half's end only), snapshots
        // 2 and 3 both at 4 (the later in front), snapshot 4 past the end.
        let at = [1, 2, 4, 4, 5];
        assert_eq!(
            scene_slots(2, 0, false, &at),
            vec![Element(0), Snapshot(0), Element(1)]
        );
        assert_eq!(
            scene_slots(3, 2, true, &at),
            vec![
                Snapshot(1),
                Element(2),
                Element(3),
                Snapshot(3),
                Snapshot(2),
                Element(4),
                Snapshot(4),
            ]
        );
        assert_eq!(scene_slots(0, 0, false, &[0]), vec![]);
        assert_eq!(scene_slots(0, 0, true, &[0]), vec![Snapshot(0)]);
    }
}

#[cfg(test)]
mod shell_bin_tests {
    use super::*;

    fn touch(dir: &std::path::Path, name: &str) {
        std::fs::write(dir.join(name), b"").unwrap();
    }

    #[test]
    fn gtk_shell_is_preferred_and_legacy_is_the_fallback() {
        let exe = tempfile::tempdir().unwrap();
        let path = tempfile::tempdir().unwrap();
        let exe_dir = Some(exe.path().to_owned());
        let path_var = Some(path.path().as_os_str().to_owned());
        let resolve = || resolve_shell_bin_in(None, None, exe_dir.clone(), path_var.clone());

        // Nothing installed: the legacy name, looked up at spawn.
        assert_eq!(resolve(), std::path::PathBuf::from("tuna-shell-host"));
        // Only the legacy shell on PATH.
        touch(path.path(), "tuna-shell-host");
        assert_eq!(resolve(), path.path().join("tuna-shell-host"));
        // The GTK shell on PATH wins over the legacy one there.
        touch(path.path(), "tuna-shell-gtk");
        assert_eq!(resolve(), path.path().join("tuna-shell-gtk"));
        // Siblings of the compositor win over PATH, GTK first.
        touch(exe.path(), "tuna-shell-host");
        assert_eq!(resolve(), exe.path().join("tuna-shell-host"));
        touch(exe.path(), "tuna-shell-gtk");
        assert_eq!(resolve(), exe.path().join("tuna-shell-gtk"));
    }

    #[test]
    fn explicit_choices_win() {
        let exe = tempfile::tempdir().unwrap();
        touch(exe.path(), "tuna-shell-gtk");
        let exe_dir = Some(exe.path().to_owned());
        assert_eq!(
            resolve_shell_bin_in(None, Some("tuna-shell-host".into()), exe_dir.clone(), None),
            std::path::PathBuf::from("tuna-shell-host")
        );
        assert_eq!(
            resolve_shell_bin_in(
                Some(std::path::Path::new("/x/shell")),
                None,
                exe_dir.clone(),
                None
            ),
            std::path::PathBuf::from("/x/shell")
        );
        // An empty TUNA_SHELL_BIN means unset.
        assert_eq!(
            resolve_shell_bin_in(None, Some("".into()), exe_dir, None),
            exe.path().join("tuna-shell-gtk")
        );
    }
}

/// Summary of one nested run, for diagnostics (no sensitive content).
#[derive(Debug, Clone, Copy, Default)]
pub struct RunStats {
    /// Frames produced.
    pub frames: u64,
    /// Wayland clients accepted.
    pub clients: u64,
    /// Shell restarts consumed (initial spawn excluded).
    pub shell_restarts: u32,
}

/// Ways the nested runtime can fail to start or run.
#[derive(Debug)]
pub enum RuntimeError {
    /// GLES/window backend creation failed (no host display or EGL).
    Backend(String),
    /// Control socket binding failed.
    Socket(String),
    /// calloop event-loop setup or dispatch failed.
    Loop(String),
    /// Client dispatch or frame production failed.
    Dispatch(String),
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Backend(e) => write!(f, "nested backend unavailable: {e}"),
            Self::Socket(e) => write!(f, "nested socket failed: {e}"),
            Self::Loop(e) => write!(f, "event loop failed: {e}"),
            Self::Dispatch(e) => write!(f, "client dispatch failed: {e}"),
        }
    }
}

impl std::error::Error for RuntimeError {}

/// Apply nested-session environment hygiene (ADR 0003): point
/// `WAYLAND_DISPLAY` at our private socket for this process and its
/// children. The parent host environment is never touched — this runs in
/// our own process — and the previous value is returned so shutdown can
/// restore it.
pub fn apply_nested_env(socket_name: &str) -> Option<OsString> {
    let prev = std::env::var_os("WAYLAND_DISPLAY");
    std::env::set_var("WAYLAND_DISPLAY", socket_name);
    prev
}

/// Undo [`apply_nested_env`].
pub fn restore_env(prev: Option<OsString>) {
    match prev {
        Some(value) => std::env::set_var("WAYLAND_DISPLAY", value),
        None => std::env::remove_var("WAYLAND_DISPLAY"),
    }
}

/// AT-SPI focus and caret for the magnifier (#349): the arrival number
/// last taken and where it landed, global logical. Extents convert
/// once, on arrival, as GNOME's handlers do.
#[derive(Debug, Default)]
struct AtspiPoints {
    focus: (u64, Option<(f64, f64)>),
    caret: (u64, Option<(f64, f64)>),
}

/// Live nested session: display, protocol state, backend, and outputs.
/// Output handles live in the state's inventory (entry zero is the
/// primary); the runtime reaches them through [`State`] accessors.
struct RemoteHeld {
    grant: std::sync::Arc<std::sync::atomic::AtomicBool>,
    keys: std::collections::HashSet<u32>,
    buttons: std::collections::HashSet<u32>,
}

struct PendingScreenshotReply {
    deadline: Duration,
    reply: std::sync::mpsc::Sender<Option<std::path::PathBuf>>,
    saved: Option<std::path::PathBuf>,
}

enum UnlockEvent {
    Pending,
    Finished(bool),
}
struct UnlockWorker {
    attempt: crate::auth_owner::Attempt,
    worker: std::thread::JoinHandle<crate::pam_backend::Verification>,
    cancel: crate::pam_cancel::Cancellation,
    phase: Arc<std::sync::atomic::AtomicU8>,
    denied: bool,
}

pub struct Runtime {
    display: Display<State>,
    state: State,
    backend: Backend,
    manager: WindowManager,
    control: ControlHub,
    shell: ShellDriver,
    orca: crate::orca::Reader,
    a11y_keyboard: Option<crate::a11y_keyboard::Monitor>,
    /// The IBus bridge, when IBus is installed.
    ime: Option<crate::ime::ImeBridge>,
    overlay: Overlay,
    wallpaper: Wallpaper,
    /// The compositor-drawn pointer and locate-pointer (#342).
    cursor: crate::cursor::Cursor,
    triggers: TriggerState,
    /// Compositor-owned session lock: flag plus idle accumulator fed
    /// from input timestamps. The hub mirror carries the flag to shell
    /// snapshots; the shell never owns it.
    lock: SessionLock,
    blank: crate::lock::IdleBlank,
    /// GNOME's desktop-level transitions (lock curtain, hot-corner
    /// ripples, startup, monitor change), sampled once per frame.
    transitions: crate::transitions::Transitions,
    screenshot_flashes: crate::screenshot_flash::Flashes,
    /// Closing GTK panel menus (`crate::menu_close`), drawn over the bar.
    menu_close: crate::menu_close::MenuClose,
    screenshot_replies: Vec<PendingScreenshotReply>,
    /// Worker threads (idle watches, Orca, wallpaper, Night Light, IME,
    /// X11 icons, PAM, logind) ping the loop through this (#503).
    wake: crate::wake::Wake,
    /// Last live lockdown refresh (see [`LOCKDOWN_REFRESH_INTERVAL`]).
    lockdown_refresh_at: Instant,
    /// GNOME's remembered Num Lock (#346): restore once, save changes.
    numlock: crate::numlock::NumlockMemory,
    /// On-demand XWayland server supervisor. Idle until the first X11
    /// need is recorded via [`Runtime::request_x11`]; the window-model
    /// join installs the real spawner, until then ticks are no-ops.
    xwayland: XWaylandSupervisor,
    /// Loop handle for event-source installation (XWayland spawn).
    /// Cloned from the owned event loop at launch; `'static`
    /// because the loop outlives the runtime in `run`.
    loop_handle: LoopHandle<'static, Runtime>,
    /// A hardware button release that would end a direct strip move or
    /// resize, held until the current dispatch finishes (see the input
    /// source in `launch`).
    #[cfg(feature = "drm")]
    strip_release_held: Option<ManagerInput>,
    /// XWayland server client awaiting its `Ready` event, after
    /// which the X11 window manager starts against it (xwayland
    /// feature only).
    #[cfg(feature = "xwayland")]
    pending_x11_client: Option<smithay::reexports::wayland_server::Client>,
    #[cfg(feature = "xwayland")]
    x11_sockets: Option<smithay::xwayland::XWaylandSockets>,
    #[cfg(feature = "xwayland")]
    x11_watchers: Vec<calloop::RegistrationToken>,
    #[cfg(feature = "xwayland")]
    x11_source: Option<calloop::RegistrationToken>,
    #[cfg(feature = "xwayland")]
    active_x11_client: Option<smithay::reexports::wayland_server::Client>,
    #[cfg(feature = "xwayland")]
    x11_failed: bool,

    /// Output scale (#59), see [`NestedSession::scale`].
    scale: f64,
    /// GNOME's input settings as last applied (#60).
    input_settings: tuna_shell_control::InputSettings,
    wellbeing_effects: crate::wellbeing_effects::Effects,
    /// GNOME's keyboard accessibility aids, ahead of everything else (#350).
    input_aids: aids::RuntimeAids,
    /// Pointer motion waiting for its output frame (Mutter's stage
    /// event queue); see [`crate::motion_coalesce`].
    motion_queue: crate::motion_coalesce::MotionCoalescer,
    /// Keys GNOME's on-screen keyboard holds down (#349).
    osk: osk::RuntimeOsk,
    /// An Escape press cancelled a client drag; its repeats and release
    /// belong to that cancel, not to the overview or a client.
    dnd_escape_held: bool,
    /// PipeWire, connected on the first screen cast (#61).
    pipewire: Option<crate::screencast::PipeWire>,
    /// Running screen casts.
    casts: Vec<crate::screencast::Cast>,
    /// Monitor list the Mutter D-Bus side serves.
    cast_outputs: crate::mutter::Outputs,
    /// Orientation lock plus the latest accelerometer sample (#345).
    orientation: crate::orientation::OrientationPolicy,
    /// Flags shared with the Mutter D-Bus thread (managed property,
    /// sensor inhibition).
    orientation_share: std::sync::Arc<crate::orientation::OrientationShare>,
    /// DisplayConfig `PowerSaveMode` applied to the outputs (#355), and
    /// its copy for the Mutter D-Bus thread.
    power_save: crate::power_save::PowerSave,
    power_save_share: std::sync::Arc<crate::power_save::PowerSaveShare>,
    /// The last dummy page flip while powered off
    /// ([`FRAME_INTERVAL`](crate::power_save::FRAME_INTERVAL)).
    power_save_beat: Option<Instant>,
    /// Night Light evaluation: settings, schedule, smear (#339).
    /// The controller also owns the live state shared with the D-Bus
    /// thread (active, temperature, "disabled until tomorrow").
    night_light: crate::night_light::NightLightController,
    /// colord devices, calibration sessions and the CRTC white point
    /// (P-DS-03), shared with the colord and D-Bus threads.
    color: std::sync::Arc<crate::color_manager::ColorShare>,
    /// The colord client thread (hardware sessions only).
    colord: Option<crate::color_manager::ColordClient>,
    /// `org.gnome.desktop.wm.preferences` focus keys (owned GIO reader).
    wm_preferences: crate::wm_preferences::Reader,
    /// Last reported reader status, so a change logs once.
    wm_preferences_status: Option<crate::night_light_settings::CacheStatus>,
    /// GNOME's per-device tablet and stylus settings (owned GIO reader).
    tablet_settings: crate::tablet_settings::Reader,
    tablet_settings_status: Option<crate::night_light_settings::CacheStatus>,
    /// `org.gnome.desktop.privacy` `privacy-screen` (owned GIO worker;
    /// hardware session only, #356).
    #[cfg(feature = "drm")]
    privacy_screen_settings: Option<crate::privacy_screen_settings::PrivacyScreenSettings>,
    /// Configured transform per connector (Settings/monitors.xml):
    /// the sensor overrides these live but never rewrites them.
    manual_transforms: std::collections::HashMap<String, u32>,
    /// What org.gnome.Mutter.DisplayConfig reports (#340), shared with
    /// the D-Bus thread, and the key it was last published at.
    display_share: std::sync::Arc<crate::display_config::DisplayShare>,
    display_published: Option<(i32, i32, u64)>,
    /// Mutter's current configuration, its history and the pending
    /// confirmation of a persistent change.
    display_configs: crate::display_config::ConfigManager,
    capture_authority: crate::capture_security::Authority,
    cast_grants: std::collections::HashMap<u64, std::sync::Arc<std::sync::atomic::AtomicBool>>,
    remote_held: std::collections::HashMap<u64, RemoteHeld>,
    /// Keys the tablet action mappers' virtual keyboard holds (evdev).
    tablet_virtual_keys: std::collections::BTreeSet<u32>,
    /// The pad OSD labels the shell last received.
    pad_osd_labels_sent: Option<tuna_shell_control::PadOsdLabels>,
    /// Window list org.gnome.Shell.Introspect serves (window sharing).
    introspect: crate::introspect::Handle,
    /// A three-finger swipe in progress: its travel so far (#60).
    shell_swipe: Option<Point<f64, Logical>>,
    /// The overview's animations (#499): the open/close transition,
    /// the swipe driving it, and the settled scene's own motion.
    overview_motion: crate::overview::OverviewMotion,
    overview_progress_at: Duration,
    tile_animation: Option<(u64, crate::animation::EaseRect, Duration)>,
    /// Last strip-view spring step, for the per-frame time delta.
    strip_view_at: Duration,
    /// Time base for the animations above: real, or manual for proofs.
    animation_clock: crate::animation_clock::AnimationClock,
    /// Overview search is showing results: the workspace card and
    /// previews hide (GNOME). Reset whenever the overview closes.
    overview_search: bool,
    /// The overview shows the app grid: workspaces become thumbnails
    /// along the top (GNOME's app grid state). Reset on close.
    overview_app_grid: bool,
    /// A press on an overview preview: the window and where it was
    /// grabbed, and whether it has become a drag (GNOME DnD).
    overview_drag: Option<(u64, Point<f64, Logical>, bool)>,
    /// The Alt+Tab switcher's window thumbnails (frames from the shell).
    switcher_thumbnails: Vec<tuna_shell_control::SwitcherThumbnail>,
    /// What each output's last drawn frame cost, by output name (#503).
    frame_costs: std::collections::BTreeMap<String, FrameCost>,
    /// Nested output's damage history across submitted frames. None
    /// means unknown: the next frame paints complete (first frame,
    /// resize, or any global drawing change), like the native path.
    winit_tracker: Option<smithay::backend::renderer::damage::OutputDamageTracker>,
    /// Signature of the last presented nested frame; unchanged means
    /// no client, cursor, or effect moved, so the tick submits nothing.
    winit_signature: Option<crate::native_repaint::FrameSignature>,
    /// When hidden windows last got their throttled frame callbacks
    /// ([`HIDDEN_FRAME_INTERVAL`](crate::occlusion::HIDDEN_FRAME_INTERVAL)).
    hidden_beat: Option<Instant>,
    /// Hidden windows hold frame callbacks for the next beat (#503).
    hidden_waiting: bool,
    /// The last input event or power-on; see [`ACTIVITY_LINGER`].
    active_at: Instant,
    /// Whether an idle inhibitor was live at the last tick (#503).
    idle_inhibited: bool,
    /// `tuna-idle-blank` changed since it was last read.
    blank_stale: bool,
    /// The runtime dir is watched: `blank_stale` reports every change.
    /// Without the watch the file is read on every tick.
    blank_watched: bool,
    /// Backoff left before the shell supervisor restarts the shell.
    shell_retry_ms: Option<u64>,
    /// A proof harness drives this session through files polled every
    /// pass (animation clock, synthetic swipes): keep display-rate ticks.
    proof_polling: bool,
    /// Reserved X11 display, advertised before XWayland starts (#219).
    /// Actual window-manager readiness is independently state.xwm.is_some().
    x11_display: Option<u32>,
    #[cfg(feature = "xwayland")]
    x11_icon_reader: crate::x11_icons::Reader,
    /// Real-time anchor of the last input event. Input stamps live on
    /// the backend event clock while idle is measured here, so each
    /// tick evaluates the lock in the input base as
    /// `last_stamp + anchor.elapsed()`.
    idle_since: Instant,
    /// Lock-screen password checks report back here.
    unlock_results: calloop::channel::Sender<(crate::auth_owner::Attempt, UnlockEvent)>,
    /// A password check is running.
    unlock_slot: crate::auth_owner::Slot,
    unlock_worker: Option<UnlockWorker>,
    unlock_quarantined: Option<crate::auth_owner::Attempt>,
    unlock_generation: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// The lock screen's password and fingerprint conversations (#366).
    auth: auth::Conversations,
    /// org.gnome.Mutter.IdleMonitor.
    idle_monitor: crate::idle_monitor::IdleMonitor,
    /// org.gnome.SessionManager's inhibitors and session activity.
    session_manager: crate::session_manager::Share,
    /// Screen magnifier (#349): Settings file cache plus each
    /// output's zoom region, drawn live, in captures and casts, and
    /// on the lock screen (whose frame holds nothing but the lock UI).
    magnifier: crate::magnifier::MagnifierState,
    /// The magnified frame's offscreen scene and zoom textures.
    zoom_targets: crate::magnifier::ZoomTargets,
    /// AT-SPI focus and caret events for magnifier tracking.
    focus_caret: crate::focus_caret::FocusCaretTracker,
    /// The last AT-SPI events taken (arrival numbers) and where they
    /// mapped at arrival, global logical.
    atspi_points: AtspiPoints,
    exit: bool,
    stats: RunStats,
    /// `TUNA_COMPOSITOR_STATE` snapshot path (journeys only).
    state_path: Option<std::path::PathBuf>,
    state_last: String,
    /// Opt-in synthetic touchpad phases for the nested proof harness.
    proof_swipe_last: String,
    /// The `sequence` of the last phase consumed, published in the state
    /// file so the harness never replaces a phase the loop has not read.
    proof_swipe_sequence: Option<u64>,
    #[cfg(feature = "drm")]
    performance_trace: crate::performance_trace::Trace,
}

/// Control socket path for a session: alongside the Wayland socket in the
/// runtime dir, so one session owns both (`tuna-<name>.control`).
/// `None` when `XDG_RUNTIME_DIR` is unset: the privileged socket never
/// falls back to a shared temp dir (#30). [`ControlHub::bind`] further
/// refuses a runtime dir that is not private to this user.
pub fn control_socket_path(socket_name: &str) -> Option<std::path::PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(format!("tuna-{socket_name}.control")))
}

impl Drop for Runtime {
    fn drop(&mut self) {
        // Before anything else tears down: the session-owned screen reader
        // must go while its session and AT-SPI buses still exist (#836).
        // Every exit path drops the runtime, including SIGTERM from logind's
        // session stop or the supervisor.
        self.orca.shutdown();
        // Window sessions (P-SY-08): record every window still in one and
        // write the store before the clients lose their windows, as
        // Mutter does when its display closes at logout.
        self.manager.save_window_sessions(&mut self.state);
        if let Some(worker) = &self.unlock_worker {
            worker.cancel.cancel();
        }
        // Shutdown requests cancellation; it never certifies retirement.
        crate::pam_backend::Backend::shutdown_live();
        #[cfg(feature = "xwayland")]
        {
            // Event sources can retain loop handles after dispatch stops.
            // Release our owned sources explicitly so XWayland's display
            // lock/socket owner is dropped before the compositor exits.
            for token in self.x11_watchers.drain(..) {
                self.loop_handle.remove(token);
            }
            if let Some(token) = self.x11_source.take() {
                self.loop_handle.remove(token);
            }
            self.x11_sockets = None;
            self.state.xwm = None;
            self.pending_x11_client = None;
            self.active_x11_client = None;
        }
    }
}

/// `(reported_ok, ok)` for a joined unlock worker. `reported_ok` is what the
/// PAM worker itself returned: a cancelled or failed verification never
/// reports OK (the tick path used to claim OK for every finished attempt).
/// A denial (stale epoch, lost origin, cancel) leaves a real success visible
/// for the trace but never lets it unlock.
fn unlock_outcome(authenticated: bool, denied: bool) -> (bool, bool) {
    (authenticated, authenticated && !denied)
}

/// A presented nested frame: its repaint region, cost, and cursor surface.
/// `None` means the tick submitted nothing (unchanged signature).
type PresentedFrame = Option<(
    Vec<smithay::utils::Rectangle<i32, smithay::utils::Physical>>,
    crate::occlusion::FrameCost,
    Option<smithay::reexports::wayland_server::protocol::wl_surface::WlSurface>,
)>;

impl Runtime {
    /// Build the session: display plus globals, output, backend window,
    /// and private socket. Does not run the loop (see [`run`]).
    pub fn launch(
        session: &NestedSession,
    ) -> Result<(Self, EventLoop<'static, Self>), RuntimeError> {
        let display: Display<State> =
            Display::new().map_err(|e| RuntimeError::Loop(e.to_string()))?;
        let dh = display.handle();
        let mut state = State::new(&dh);
        if let Some(path) = crate::xdg_session::default_store_path() {
            state.window_sessions_mut().attach_store(path);
        }
        let event_loop = EventLoop::try_new().map_err(|e| RuntimeError::Loop(e.to_string()))?;
        let choice = session.backend.resolve(has_host_display());
        let backend = match choice {
            #[cfg(feature = "drm")]
            BackendChoice::Drm => {
                let (drm, sources) = crate::drm::DrmBackend::new()
                    .map_err(|e| RuntimeError::Backend(e.to_string()))?;
                // One wl_output per logical monitor: mirror clones and
                // switched-off connectors have none (#340; Mutter also
                // advertises each mirror clone, a known difference).
                for out in drm.outputs.iter().filter(|o| o.is_leader()) {
                    let global = out.output.create_global::<State>(&dh);
                    let (lw, lh) = out.logical_size();
                    state.add_output(&out.name, Some(out.output.clone()), lw, lh);
                    state.set_output_location(&out.name, out.loc);
                    state.note_output_global(&out.name, global);
                }
                let handle = event_loop.handle();
                handle
                    .insert_source(sources.session, |event, _, rt: &mut Runtime| {
                        if matches!(event, smithay::backend::session::Event::PauseSession) {
                            rt.manager.cancel_strip_pointer(&mut rt.state);
                        }
                        if let Backend::Drm(drm) = &mut rt.backend {
                            drm.on_session_event(event);
                        }
                    })
                    .map_err(|e| RuntimeError::Loop(e.to_string()))?;
                handle
                    .insert_source(sources.drm, |event, metadata, rt: &mut Runtime| {
                        let flip = match &mut rt.backend {
                            Backend::Drm(drm) => drm.on_drm_event(event, metadata),
                            Backend::Winit(_) => None,
                        };
                        if let Some(flip) = flip {
                            rt.page_flipped(flip);
                        }
                    })
                    .map_err(|e| RuntimeError::Loop(e.to_string()))?;
                handle
                    .insert_source(sources.input, |event, _, rt: &mut Runtime| {
                        // A VT switch revokes the evdev fds before libseat's
                        // PauseSession reaches us; libinput answers by
                        // releasing every held button and then removing the
                        // device, in one dispatch. That release must roll a
                        // direct strip move/resize back, never commit it, so
                        // a strip-owning release waits for the dispatch to
                        // end and a device removal meanwhile cancels the grab
                        // (marlin-vm 38732705939 committed a column move).
                        if rt.strip_release_held.is_some()
                            && matches!(
                                event,
                                smithay::backend::input::InputEvent::DeviceRemoved { .. }
                            )
                        {
                            rt.manager.cancel_strip_pointer(&mut rt.state);
                        }
                        let corners_active = !rt.is_locked()
                            && !rt.manager.pointer_constraint_owns_motion(&rt.state)
                            && !rt.overlay.visible
                            && crate::layer::exclusive_popup_keyboard_layer(&rt.state).is_none();
                        let corner_layout = rt.state.hot_corner_outputs();
                        rt.triggers.set_pressure_only(true);
                        let touch = matches!(
                            event,
                            smithay::backend::input::InputEvent::TouchDown { .. }
                                | smithay::backend::input::InputEvent::TouchMotion { .. }
                        );
                        let inputs = match &mut rt.backend {
                            Backend::Drm(drm) => {
                                drm.set_hot_corner_active(corners_active);
                                drm.translate(event, &corner_layout)
                            }
                            Backend::Winit(_) => Vec::new(),
                        };
                        for input in inputs {
                            let input = if touch { rt.unzoom_touch(input) } else { input };
                            if let ManagerInput::Button {
                                button,
                                pressed: false,
                                ..
                            } = input
                            {
                                if rt.strip_release_held.is_none()
                                    && rt.manager.strip_grab_button() == Some(button)
                                {
                                    rt.strip_release_held = Some(input);
                                    rt.loop_handle.insert_idle(|rt| {
                                        if let Some(input) = rt.strip_release_held.take() {
                                            rt.on_backend_input(input);
                                        }
                                    });
                                    continue;
                                }
                            }
                            rt.on_backend_input(input);
                        }
                        // Tablets (P-IN-05) after the dispatch's other input,
                        // in their own order; their settings paths follow
                        // hotplug and tool changes.
                        let tablet_events = match &mut rt.backend {
                            Backend::Drm(drm) => {
                                rt.tablet_settings
                                    .want(drm.tablet_settings_wanted(), Instant::now());
                                drm.take_tablet_events()
                            }
                            Backend::Winit(_) => Vec::new(),
                        };
                        for event in tablet_events {
                            rt.on_tablet_input(event);
                        }
                        rt.pad_osd_tick();
                        // The window manager has the final say (a locked
                        // pointer stays put): the drawn cursor follows it.
                        let pos = rt.manager.pointer_pos();
                        if let Backend::Drm(drm) = &mut rt.backend {
                            drm.set_pointer(pos);
                        }
                    })
                    .map_err(|e| RuntimeError::Loop(e.to_string()))?;
                if let Some(udev) = sources.udev {
                    // Privacy screen hw-state changes (#356) arrive as
                    // DRM `change` uevents.
                    handle
                        .insert_source(udev, |event, _, rt: &mut Runtime| {
                            if let smithay::backend::udev::UdevEvent::Changed { device_id } = event
                            {
                                if let Backend::Drm(drm) = &mut rt.backend {
                                    drm.on_device_changed(device_id);
                                }
                            }
                        })
                        .map_err(|e| RuntimeError::Loop(e.to_string()))?;
                }
                Some(Backend::Drm(Box::new(drm)))
            }
            _ => None,
        };
        let output = Output::new(
            "tuna-0".to_owned(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "Tuna Desktop".to_owned(),
                model: "Nested".to_owned(),
            },
        );
        let mode = Mode {
            size: (session.width, session.height).into(),
            refresh: 60_000,
        };
        let scale = clamp_scale(session.scale);
        output.change_current_state(
            Some(mode),
            None,
            Some(if scale == 1.0 {
                Scale::Integer(1)
            } else {
                Scale::Fractional(scale)
            }),
            Some((0, 0).into()),
        );
        output.set_preferred(mode);
        state.set_preferred_scale(scale);
        // Hardware: fractional clients render at the primary output's
        // scale from monitors.xml (#59).
        #[cfg(feature = "drm")]
        if let Some(Backend::Drm(drm)) = &backend {
            if let Some(first) = drm.outputs.first() {
                state.set_preferred_scale(first.scale);
            }
        }
        if backend.is_none() {
            let global = output.create_global::<State>(&dh);
            let (lw, lh) = logical_size(session.width, session.height, scale);
            state.add_output("tuna-0", Some(output), lw, lh);
            state.note_output_global("tuna-0", global);
        }
        let manager = WindowManager::new(&mut state);
        let tokens = std::rc::Rc::new(TokenStore::new());
        let control_path = control_socket_path(&session.socket_name)
            .ok_or_else(|| RuntimeError::Socket("XDG_RUNTIME_DIR is unset".to_owned()))?;
        let control = ControlHub::bind(control_path.clone(), tokens, crate::SEAT_NAME)
            .map_err(|e| RuntimeError::Socket(e.to_string()))?;
        event_loop
            .handle()
            .insert_source(
                control
                    .readiness_source()
                    .map_err(|e| RuntimeError::Loop(e.to_string()))?,
                |_, fd, _: &mut Runtime| {
                    crate::control_readiness::drain(fd)?;
                    Ok(calloop::PostAction::Continue)
                },
            )
            .map_err(|e| RuntimeError::Loop(e.to_string()))?;
        let shell_policy = RestartPolicy::default();
        let mut shell = ShellDriver::new(
            shell_policy,
            resolve_shell_bin(session.shell_bin.as_deref()),
            session.socket_name.clone(),
            control_path,
        );
        let overlay = Overlay::new(shell_policy.max_attempts);

        let backend = match backend {
            Some(backend) => backend,
            None => {
                let (backend, winit_loop) = winit::init::<GlesRenderer>()
                    // Smithay's Display omits the underlying Winit error.
                    // Retain it so missing runtime libraries/display errors
                    // can be distinguished from candidate render failures.
                    .map_err(|e| RuntimeError::Backend(format!("{e:?}")))?;
                event_loop
                    .handle()
                    .insert_source(winit_loop, |event, _, runtime: &mut Runtime| {
                        runtime.on_winit_event(event);
                    })
                    .map_err(|e| RuntimeError::Loop(e.to_string()))?;
                Backend::Winit(Box::new(backend))
            }
        };
        {
            // Accepted clients also wake the nested backend, independently of
            // host window input. The scheduler separately accounts for active work.
            event_loop
                .handle()
                .insert_source(
                    display_readiness_source(&display)
                        .map_err(|e| RuntimeError::Loop(e.to_string()))?,
                    |_, _, _: &mut Runtime| Ok(calloop::PostAction::Continue),
                )
                .map_err(|e| RuntimeError::Loop(e.to_string()))?;
        }
        let socket = ListeningSocketSource::with_name(&session.socket_name)
            .map_err(|e| RuntimeError::Socket(e.to_string()))?;
        event_loop
            .handle()
            .insert_source(socket, |stream, _, runtime: &mut Runtime| {
                let client_data = ClientState::native_connection(&stream);
                if let Ok(client) = runtime
                    .display
                    .handle()
                    .insert_client(stream, Arc::new(client_data))
                {
                    let _ = client;
                    runtime.stats.clients += 1;
                }
            })
            .map_err(|e| RuntimeError::Loop(e.to_string()))?;

        // The shell offers power actions only on a hardware session: in
        // the nested preview, logind's session is the host's, so Log Out
        // must end only this compositor and Power Off must not exist.
        // An explicit TUNA_SESSION_KIND wins (proofs, debugging).
        if std::env::var_os(SESSION_KIND_ENV).is_none() {
            shell.set_env(
                SESSION_KIND_ENV,
                match backend {
                    Backend::Winit(_) => "nested",
                    #[cfg(feature = "drm")]
                    Backend::Drm(_) => "hardware",
                },
            );
        }

        // linux-dmabuf lists exactly what this renderer imports (#89).
        let mut backend = backend;
        let mut state = state;
        {
            use smithay::backend::renderer::ImportDma;
            let formats: Vec<_> = match &mut backend {
                Backend::Winit(winit) => winit.renderer().dmabuf_formats().into_iter().collect(),
                #[cfg(feature = "drm")]
                Backend::Drm(drm) => drm.renderer.dmabuf_formats().into_iter().collect(),
            };
            state.enable_dmabuf(formats);
        }

        // org.gnome.Mutter.ScreenCast and DisplayConfig (#61): screen
        // sharing through the stock GNOME portal.
        let cast_outputs: crate::mutter::Outputs = Default::default();
        let orientation_share: std::sync::Arc<crate::orientation::OrientationShare> =
            Default::default();
        let night_light_share: std::sync::Arc<crate::night_light::NightLightShare> =
            Default::default();
        let power_save_share: std::sync::Arc<crate::power_save::PowerSaveShare> =
            Default::default();
        let display_share: std::sync::Arc<crate::display_config::DisplayShare> = Default::default();
        let color_share: std::sync::Arc<crate::color_manager::ColorShare> = Default::default();
        // Mutter registers physical monitors with colord; nested outputs
        // are virtual monitors and get no colord device.
        let colord = match &backend {
            Backend::Winit(_) => None,
            #[cfg(feature = "drm")]
            Backend::Drm(drm) => {
                color_share.set_gamma_sizes(drm.gamma_sizes());
                Some(crate::color_manager::ColordClient::start(
                    color_share.clone(),
                    drm.colord_monitors(),
                    crate::color_manager::Bus::System,
                    crate::color_manager::icc_directory(),
                ))
            }
        };
        // org.gnome.Shell.Introspect: the portal's window picker.
        let capture_authority = crate::capture_security::Authority::default();
        let introspect = crate::introspect::start(state.desktop_size(), capture_authority.clone());
        event_loop
            .handle()
            .insert_source(
                crate::mutter::start(
                    cast_outputs.clone(),
                    introspect.windows.clone(),
                    capture_authority.clone(),
                    display.handle(),
                    orientation_share.clone(),
                    night_light_share.clone(),
                    power_save_share.clone(),
                    display_share.clone(),
                    color_share.clone(),
                ),
                |event, _, rt: &mut Runtime| {
                    if let calloop::channel::Event::Msg(request) = event {
                        rt.on_screencast_request(request);
                    }
                },
            )
            .map_err(|e| RuntimeError::Loop(e.to_string()))?;
        // org.gnome.Shell.Screenshot (#61): requests from the D-Bus
        // thread are answered between frames.
        event_loop
            .handle()
            .insert_source(
                crate::screenshot::start(capture_authority.clone()),
                |event, _, rt: &mut Runtime| {
                    use crate::screenshot::Request;
                    match event {
                        calloop::channel::Event::Msg(Request::Shot {
                            filename,
                            window,
                            flash,
                            reply,
                        }) => {
                            if flash
                                && rt.screenshot_replies.len()
                                    == crate::screenshot_flash::MAX_FLASHES
                            {
                                let _ = reply.send(None);
                                return;
                            }
                            let saved = if window {
                                rt.capture_window(&filename)
                            } else {
                                rt.capture(&filename)
                            };
                            let deadline = (flash && saved.is_some())
                                .then(|| rt.start_screenshot_flash(window))
                                .flatten();
                            if let Some(deadline) = deadline {
                                rt.screenshot_replies.push(PendingScreenshotReply {
                                    deadline,
                                    reply,
                                    saved,
                                });
                            } else {
                                let _ = reply.send(saved);
                            }
                        }
                        calloop::channel::Event::Msg(Request::Windows { directory, reply }) => {
                            let _ = reply.send(rt.capture_selector_windows(&directory));
                        }
                        calloop::channel::Event::Msg(Request::Flash { area, reply }) => {
                            let valid = !rt.is_locked()
                                && rt.state.outputs.iter().any(|out| {
                                    let [x, y, width, height] = area;
                                    width > 0
                                        && height > 0
                                        && x >= out.loc.0
                                        && y >= out.loc.1
                                        && i64::from(x) + i64::from(width)
                                            <= i64::from(out.loc.0) + i64::from(out.size.w)
                                        && i64::from(y) + i64::from(height)
                                            <= i64::from(out.loc.1) + i64::from(out.size.h)
                                });
                            if valid {
                                let _ = rt.start_flash_area(area);
                            }
                            let _ = reply.send(valid);
                        }
                        calloop::channel::Event::Closed => {}
                    }
                },
            )
            .map_err(|e| RuntimeError::Loop(e.to_string()))?;

        // Lock-screen password checks finish here, between frames.
        let (unlock_results, unlock_source) =
            calloop::channel::channel::<(crate::auth_owner::Attempt, UnlockEvent)>();
        event_loop
            .handle()
            .insert_source(unlock_source, |event, _, rt: &mut Runtime| {
                if let calloop::channel::Event::Msg((attempt, event)) = event {
                    match event {
                        UnlockEvent::Pending => rt.deny_pending_unlock(attempt),
                        // Grant only after tick consumes control EOF/gate changes.
                        UnlockEvent::Finished(false) => rt.deny_pending_unlock(attempt),
                        UnlockEvent::Finished(true) => {}
                    }
                }
            })
            .map_err(|e| RuntimeError::Loop(e.to_string()))?;
        // Worker threads ping the loop when they have something for the
        // tick (#503); the ping only wakes it.
        let (wake, wake_source) =
            crate::wake::Wake::new().map_err(|e| RuntimeError::Loop(e.to_string()))?;
        event_loop
            .handle()
            .insert_source(wake_source, |_, _, _: &mut Runtime| {})
            .map_err(|e| RuntimeError::Loop(e.to_string()))?;

        // The session that owns the screen reader serves GNOME's
        // KeyboardMonitor for it: every hardware session, and a nested one
        // only when asked (`--screen-reader`).
        #[cfg(feature = "drm")]
        let hardware = matches!(backend, Backend::Drm(_));
        #[cfg(not(feature = "drm"))]
        let hardware = false;
        let a11y_keyboard =
            (hardware || session.screen_reader).then(crate::a11y_keyboard::Monitor::start);
        // org.gnome.desktop.privacy privacy-screen, on its own GIO thread
        // as mutter's meta-settings.c reads it in-process (#356); only
        // the hardware session has privacy screens.
        #[cfg(feature = "drm")]
        let privacy_screen_settings =
            hardware.then(|| crate::privacy_screen_settings::PrivacyScreenSettings::start(&wake));
        let loop_handle = event_loop.handle();
        let mut runtime = Runtime {
            display,
            state,
            backend,
            manager,
            control,
            shell,
            ime: crate::ime::ImeBridge::configured(&session.socket_name),
            overlay,
            wallpaper: Wallpaper::new(),
            cursor: crate::cursor::Cursor::new(),
            triggers: TriggerState::default(),
            lock: SessionLock::new(idle_timeout_ms()),
            blank: crate::lock::IdleBlank::default(),
            transitions: crate::transitions::Transitions::new(Duration::ZERO),
            screenshot_flashes: Default::default(),
            menu_close: Default::default(),
            screenshot_replies: Vec::new(),
            // GNOME's focus policy keys, read on their own GIO thread as
            // Mutter reads them in-process (prefs.c); changes wake the loop.
            wm_preferences: crate::wm_preferences::Reader::new(&wake),
            wm_preferences_status: None,
            // Tablet and stylus settings, read on their own GIO thread as
            // Mutter's meta-input-settings.c reads them in-process.
            tablet_settings: crate::tablet_settings::Reader::new(&wake),
            tablet_settings_status: None,
            #[cfg(feature = "drm")]
            privacy_screen_settings,
            numlock: {
                let wake = wake.clone();
                crate::numlock::NumlockMemory::start(crate::numlock::CliStore, move || {
                    wake.signal()
                })
            },
            wake,
            lockdown_refresh_at: Instant::now(),
            xwayland: XWaylandSupervisor::new(),
            loop_handle,
            #[cfg(feature = "drm")]
            strip_release_held: None,
            #[cfg(feature = "xwayland")]
            pending_x11_client: None,
            #[cfg(feature = "xwayland")]
            x11_sockets: None,
            #[cfg(feature = "xwayland")]
            x11_watchers: Vec::new(),
            #[cfg(feature = "xwayland")]
            x11_source: None,
            #[cfg(feature = "xwayland")]
            active_x11_client: None,
            #[cfg(feature = "xwayland")]
            x11_failed: false,
            x11_display: None,
            #[cfg(feature = "xwayland")]
            x11_icon_reader: crate::x11_icons::Reader::new(),
            overview_search: false,
            overview_app_grid: false,
            overview_drag: None,
            switcher_thumbnails: Vec::new(),
            frame_costs: std::collections::BTreeMap::new(),
            winit_tracker: None,
            winit_signature: None,
            hidden_beat: None,
            hidden_waiting: false,
            active_at: Instant::now(),
            idle_inhibited: false,
            blank_stale: true,
            blank_watched: false,
            shell_retry_ms: None,
            proof_polling: false,
            shell_swipe: None,
            overview_motion: Default::default(),
            overview_progress_at: Duration::ZERO,
            tile_animation: None,
            strip_view_at: Duration::ZERO,
            animation_clock: crate::animation_clock::AnimationClock::real(),
            scale: clamp_scale(session.scale),
            input_settings: Default::default(),
            wellbeing_effects: Default::default(),
            input_aids: aids::RuntimeAids::default(),
            motion_queue: crate::motion_coalesce::MotionCoalescer::new(),
            osk: osk::RuntimeOsk::default(),
            dnd_escape_held: false,
            orca: crate::orca::Reader::new(&session.socket_name, a11y_keyboard.clone()),
            a11y_keyboard,
            pipewire: None,
            casts: Vec::new(),
            cast_outputs,
            orientation: Default::default(),
            orientation_share,
            power_save: Default::default(),
            power_save_share,
            power_save_beat: None,
            night_light: crate::night_light::NightLightController::new(night_light_share.clone()),
            color: color_share.clone(),
            colord,
            manual_transforms: Default::default(),
            display_share,
            display_published: None,
            display_configs: Default::default(),
            capture_authority,
            cast_grants: Default::default(),
            remote_held: Default::default(),
            tablet_virtual_keys: Default::default(),
            pad_osd_labels_sent: None,
            introspect,
            idle_since: Instant::now(),
            unlock_results,
            unlock_slot: crate::auth_owner::Slot::default(),
            unlock_worker: None,
            unlock_quarantined: None,
            unlock_generation: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            auth: auth::Conversations::default(),
            idle_monitor: crate::idle_monitor::start(),
            session_manager: crate::session_manager::start(),
            magnifier: crate::magnifier::MagnifierState::new(),
            zoom_targets: crate::magnifier::ZoomTargets::default(),
            focus_caret: crate::focus_caret::FocusCaretTracker::default(),
            atspi_points: AtspiPoints::default(),
            exit: false,
            stats: RunStats::default(),
            state_path: std::env::var_os("TUNA_COMPOSITOR_STATE")
                .map(std::path::PathBuf::from)
                .filter(|p| p.is_absolute()),
            state_last: String::new(),
            proof_swipe_last: String::new(),
            proof_swipe_sequence: None,
            #[cfg(feature = "drm")]
            performance_trace: crate::performance_trace::Trace::from_env(),
        };
        runtime.animation_clock =
            crate::animation_clock::AnimationClock::from_env(runtime.state_path.is_some());
        runtime.wallpaper.set_wake(&runtime.wake);
        // Night Light (#339): D-Bus preview/clear/temp-disable
        // records ping the already-registered wake above; its no-op
        // callback plus the unconditional tick/render below serve
        // them, so no second wake source or thread is needed.
        night_light_share.set_wake(&runtime.wake);
        night_light_share.start_reader(&runtime.wake);
        color_share.set_wake(&runtime.wake);
        runtime.orca.set_wake(&runtime.wake);
        runtime.idle_monitor.set_wake(&runtime.wake);
        if let Some(ime) = &runtime.ime {
            ime.set_wake(&runtime.wake);
        }
        #[cfg(feature = "xwayland")]
        runtime.x11_icon_reader.set_wake(&runtime.wake);
        let started = runtime.animation_clock.now();
        runtime.overview_progress_at = started;
        runtime.strip_view_at = started;
        runtime.proof_polling = runtime.state_path.is_some()
            && [crate::animation_clock::CLOCK_ENV, "TUNA_PROOF_SWIPE_INPUT"]
                .iter()
                .any(|key| std::env::var_os(key).is_some());
        // The shell's idle-blank settings and wallpaper drop live in the
        // runtime dir: any change there wakes the loop, and the blank
        // file is read again only when it changed. A state document the
        // tick itself rewrites there would wake the loop on its own
        // writes, so then the dir is not watched and the blank file is
        // read on every tick.
        let state_dir = runtime
            .state_path
            .as_deref()
            .and_then(std::path::Path::parent)
            .map(std::path::Path::to_path_buf);
        if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR")
            .filter(|dir| state_dir.as_deref() != Some(std::path::Path::new(dir)))
        {
            let watched = crate::wake::file_watch(std::path::Path::new(&dir))
                .map_err(|e| e.to_string())
                .and_then(|watch| {
                    event_loop
                        .handle()
                        .insert_source(watch, |_, fd, rt: &mut Runtime| {
                            let blank = std::ffi::OsStr::new(crate::wake::IDLE_BLANK_FILE);
                            if crate::wake::changed(&*fd, &[blank]) {
                                rt.blank_stale = true;
                            }
                            Ok(calloop::PostAction::Continue)
                        })
                        .map_err(|e| e.to_string())
                });
            match watched {
                Ok(_) => runtime.blank_watched = true,
                Err(error) => {
                    eprintln!("tuna-compositor: runtime dir watch unavailable: {error}")
                }
            }
        }
        // Reserve and advertise sockets, but spawn only when a real X11
        // client connects. Native clients do not start a compatibility process.
        #[cfg(feature = "xwayland")]
        if session.xwayland {
            if let Err(error) = runtime.prepare_x11(None) {
                eprintln!(
                    "tuna-compositor: xwayland: socket preparation failed: {error}; native session continues"
                );
            }
        }
        // GNOME Shell greets a login with the overview.
        if session.startup_overview {
            runtime.control.set_overview(true);
        }
        Ok((runtime, event_loop))
    }

    /// Handle one backend event. While the recovery overlay is visible
    /// it keeps focus and input (GNOME shield shape): every event goes
    /// to the overlay and windows hear nothing.
    fn on_winit_event(&mut self, event: WinitEvent) {
        match event {
            WinitEvent::Resized { size, .. } => {
                let mode = Mode {
                    size,
                    refresh: 60_000,
                };
                if let Some(output) = self.state.primary_output() {
                    output.change_current_state(Some(mode), None, None, None);
                }
                let (lw, lh) = logical_size(size.w, size.h, self.scale);
                self.state.set_output_size(lw, lh);
                // A new size invalidates buffer-age history: the next
                // frame paints complete.
                self.winit_tracker = None;
                self.winit_signature = None;
            }
            WinitEvent::CloseRequested => self.exit = true,
            WinitEvent::Input(event) => {
                // Pointer positions land in logical pixels.
                let area = match &self.backend {
                    Backend::Winit(backend) => {
                        let size = backend.window_size();
                        logical_size(size.w, size.h, self.scale).into()
                    }
                    #[cfg(feature = "drm")]
                    Backend::Drm(_) => return,
                };
                let touch = matches!(
                    event,
                    smithay::backend::input::InputEvent::TouchDown { .. }
                        | smithay::backend::input::InputEvent::TouchMotion { .. }
                );
                for input in translate_input(event, area) {
                    let input = if touch {
                        self.unzoom_touch(input)
                    } else {
                        input
                    };
                    self.on_backend_input(input);
                }
            }
            WinitEvent::Redraw | WinitEvent::Focus(_) => {}
        }
    }

    /// A touch point lands on what the screen magnifier shows under the
    /// finger (#349), and the zoom holds still under it rather than
    /// chasing the pointer the touch moves. Pointer devices stay in
    /// unmagnified coordinates, as in GNOME: their sprite is drawn
    /// where the position maps, so clicks already hit what it shows.
    fn unzoom_touch(&mut self, input: ManagerInput) -> ManagerInput {
        match input {
            ManagerInput::Motion { pos, time } if self.magnifier.config().active() => {
                let (x, y) = self.magnifier.unzoom((pos.x, pos.y));
                self.magnifier.hold_pointer((x, y));
                ManagerInput::Motion {
                    pos: (x, y).into(),
                    time,
                }
            }
            other => other,
        }
    }

    /// Current time in the input-timestamp base: the newest input stamp
    /// plus real time elapsed since it arrived. The backend event clock
    /// and the system clock share no base, so the stamp anchors the
    /// base and only the gap is measured here.
    fn lock_now_ms(&self) -> u64 {
        self.lock.last_input_ms().saturating_add(
            self.idle_since
                .elapsed()
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX),
        )
    }

    fn blank_alpha(&self) -> f32 {
        self.blank.alpha(self.lock.idle_ms(self.lock_now_ms()))
    }

    /// GNOME's ripples out of the hot corner of the monitor under `pos`
    /// (`layout.js` `HotCorner._toggleOverview`).
    fn ripple_hot_corner(&mut self, pos: Point<f64, Logical>) {
        let outputs = self.state.hot_corner_outputs();
        let rtl = self.input_settings.right_to_left;
        if let Some((corner, mirrored)) = crate::transitions::hot_corner(&outputs, pos, rtl) {
            self.transitions.ripple(corner, mirrored);
        }
    }

    /// Transition counters for the proofs, the wallpaper's included.
    fn transitions_diagnostics(&self) -> serde_json::Value {
        let mut doc = self.transitions.diagnostics();
        let fade = self.wallpaper.fade_stats();
        doc["wallpaper_crossfade"] = serde_json::json!({
            "started": fade.started,
            "animated_frames": fade.animated_frames,
            "active": fade.active,
            "value": fade.value,
        });
        doc
    }

    /// Whether the session is locked (hub flag: what the snapshots say).
    fn is_locked(&self) -> bool {
        self.control.is_locked()
    }

    /// Engage the lock: flag (idle machine plus hub mirror for
    /// snapshots), overview dismissed, and the exclusive lock surface
    /// up with an empty list — no window titles while locked.
    fn engage_lock(&mut self) {
        // Motion that came before the lock is the session's, not the shield's.
        self.flush_motion();
        // Track lifecycle separately: idle check_timeout has already locked its flag.
        self.unlock_slot.note_lock(&self.unlock_generation);
        #[cfg(feature = "drm")]
        self.performance_trace.cancel();
        self.reset_input_aid_ownership();
        // Nothing the session's keyboard held stays down under the lock.
        self.release_osk_keys();
        if let Some(monitor) = &self.a11y_keyboard {
            monitor.set_locked(true);
        }
        self.lock.lock();
        // Pre-lock presses never report repeats or releases across the
        // lock boundary.
        self.manager.invalidate_held_accelerators();
        self.control.cancel_pending_accelerators();
        self.cursor.on_lock();
        self.control.set_locked(true);
        let remote_ids: Vec<_> = self.remote_held.keys().copied().collect();
        for id in remote_ids {
            self.stop_remote_input(id);
        }
        self.capture_authority.publish(true, self.shell.child_pid());
        self.state.set_shortcut_inhibition_locked(true);
        // A three-finger swipe under way is the unlocked session's: end
        // it cancelled, so its overview or workspace slide never resumes
        // (GNOME's trackers interrupt outside their allowed action modes,
        // swipeTracker.js `_updateTouchpadGesture`/`_endTouchpadGesture`).
        if self.shell_swipe.is_some() {
            self.shell_swipe_input(&ManagerInput::SwipeEnd {
                cancelled: true,
                time: crate::state::system_millis() as u32,
            });
        }
        self.control.set_overview(false);
        // The shield covers GNOME's switcher popup and its window clones;
        // no window image may draw over the lock.
        self.switcher_thumbnails.clear();
        self.overlay.show(Vec::new());
    }

    /// Attempt unlock through PAM: verify `password` for `user` against
    /// the `tuna-lock` service and clear the lock on success (see
    /// [`crate::unlock`]). No PAM backend or an unreachable helper fails
    /// closed — the session stays locked and silent; auth is never
    /// invented. Returns whether the session is unlocked afterwards.
    pub fn try_unlock(&mut self, user: &str, password: &str) -> bool {
        use crate::pam_backend::{Initial, Verification};
        if !self.is_locked() {
            return true;
        }
        self.unlock_slot.note_lock(&self.unlock_generation);
        let epoch = self
            .unlock_generation
            .load(std::sync::atomic::Ordering::Acquire);
        let Some(attempt) = self.unlock_slot.begin(None, epoch) else {
            return false;
        };
        let cancel = crate::pam_cancel::Cancellation::default();
        let ticket = crate::pam_backend::Backend::live()
            .ok_or(Verification::RejectedBeforeSpawn)
            .and_then(|backend| {
                backend.begin(
                    crate::pam::service(),
                    user.to_owned(),
                    tuna_shell_control::Secret(password.to_owned()),
                    cancel.clone(),
                )
            });
        match ticket {
            Ok(mut ticket) => match ticket.initial() {
                Initial::Finished(result) => {
                    if !result.may_retire() {
                        self.unlock_quarantined = Some(attempt);
                        return false;
                    }
                    self.unlock_slot.retire(attempt)
                        && result.authenticated()
                        && self.apply_verified_unlock(attempt.epoch)
                }
                Initial::Pending => {
                    // The public caller gets denial now; no later result can grant it.
                    self.start_unlock_worker(attempt, cancel, true, move |_| {
                        ticket.wait_retired(|| {})
                    });
                    false
                }
            },
            Err(result) => {
                if result.may_retire() {
                    self.unlock_slot.retire(attempt);
                } else {
                    self.unlock_quarantined = Some(attempt);
                }
                false
            }
        }
    }

    fn start_unlock_worker(
        &mut self,
        attempt: crate::auth_owner::Attempt,
        cancel: crate::pam_cancel::Cancellation,
        denied: bool,
        verify: impl FnOnce(Box<dyn FnMut() + Send>) -> crate::pam_backend::Verification
            + Send
            + 'static,
    ) {
        let reply = self.unlock_results.clone();
        let wake = self.wake.clone();
        let phase = Arc::new(std::sync::atomic::AtomicU8::new(u8::from(denied)));
        let worker_phase = phase.clone();
        let pending_reply = reply.clone();
        let pending_phase = phase.clone();
        let worker = std::thread::Builder::new()
            .name("tuna-unlock".into())
            .spawn(move || {
                let _completion = wake.phase_completion(worker_phase.clone());
                let result = verify(Box::new(move || {
                    pending_phase.store(1, std::sync::atomic::Ordering::Release);
                    let _ = pending_reply.send((attempt, UnlockEvent::Pending));
                }));
                worker_phase.store(2, std::sync::atomic::Ordering::Release);
                let _ = reply.send((attempt, UnlockEvent::Finished(result.authenticated())));
                result
            });
        match worker {
            Ok(worker) => {
                self.unlock_worker = Some(UnlockWorker {
                    attempt,
                    worker,
                    cancel,
                    phase,
                    denied,
                })
            }
            Err(_) => {
                cancel.cancel();
                // A public pending ticket may already own a helper: no fabricated reap.
                if denied {
                    self.unlock_quarantined = Some(attempt);
                } else {
                    self.unlock_slot.retire(attempt);
                }
                if let Some(origin) = attempt.origin {
                    self.control.finish_unlock(origin, false);
                }
            }
        }
    }

    fn deny_pending_unlock(&mut self, attempt: crate::auth_owner::Attempt) {
        if !self.unlock_slot.matches(attempt) {
            return;
        }
        let Some(worker) = self
            .unlock_worker
            .as_mut()
            .filter(|worker| worker.attempt == attempt)
        else {
            return;
        };
        worker.cancel.cancel();
        if worker.denied {
            return;
        }
        worker.denied = true;
        if let Some(origin) = attempt.origin {
            self.control.finish_unlock(origin, false);
        }
    }

    fn cancel_invalid_unlock(&mut self) {
        let Some(worker) = self.unlock_worker.as_ref() else {
            return;
        };
        let attempt = worker.attempt;
        if !self.unlock_slot.matches(attempt) {
            return;
        }
        let epoch = self
            .unlock_generation
            .load(std::sync::atomic::Ordering::Acquire);
        let origin_lost = attempt
            .origin
            .is_some_and(|origin| !self.control.origin_pending_live(origin));
        if epoch != attempt.epoch || origin_lost || !self.is_locked() {
            // Retain this exact worker and slot until actual helper retirement.
            self.deny_pending_unlock(attempt);
        }
    }

    fn apply_verified_unlock(&mut self, epoch: u64) -> bool {
        if !self.is_locked() || !self.unlock_slot.grant(&self.unlock_generation, epoch) {
            return false;
        }
        self.release_lock();
        true
    }

    fn release_lock(&mut self) {
        self.flush_motion();
        self.reset_input_aid_ownership();
        if let Some(monitor) = &self.a11y_keyboard {
            monitor.set_locked(false);
        }
        self.lock.unlock(self.lock_now_ms());
        self.control.set_locked(false);
        self.state.set_shortcut_inhibition_locked(false);
        self.overlay.hide();
    }

    /// logind's `Lock` signal (`loginctl lock-session`): `screenShield.js`
    /// connects it to `lock()`, which the disable-lock-screen lockdown
    /// refuses.
    #[cfg(feature = "drm")]
    fn lock_for_logind(&mut self) {
        if self.is_locked() {
            return;
        }
        if self.control.lock_screen_allowed() {
            self.engage_lock();
        } else {
            eprintln!("tuna-compositor: logind lock refused by administrator policy");
        }
    }

    /// logind's `Unlock` signal (`loginctl unlock-session`): `screenShield.js`
    /// connects it to `deactivate()`, which ends the lock without a password.
    /// logind only emits it for the session's owner or a polkit-authorized
    /// caller. A password check or conversation in flight is denied, never
    /// applied.
    #[cfg(feature = "drm")]
    fn unlock_for_logind(&mut self) {
        if !self.is_locked() {
            return;
        }
        self.unlock_slot.release_by_session(&self.unlock_generation);
        self.release_lock();
        self.cancel_invalid_unlock();
        self.cancel_all_conversations();
    }

    /// Actual helper retirement is required in addition to the joined wrapper.
    /// A poisoned owner denies permanently; it cannot release this slot.
    fn finish_unlock(&mut self, attempt: crate::auth_owner::Attempt) {
        if !self.unlock_slot.matches(attempt) {
            return;
        }
        let Some(worker) = self.unlock_worker.as_ref() else {
            return;
        };
        if worker.attempt != attempt || !worker.worker.is_finished() {
            return;
        }
        self.cancel_invalid_unlock();
        let worker = self.unlock_worker.take().expect("matched owned worker");
        let denied = worker.denied;
        let cancel_requested = worker.cancel.is_canceled();
        let joined = worker.worker.join();
        let joined_ok = joined.is_ok();
        let result = joined.unwrap_or(crate::pam_backend::Verification::Unretired);
        let backend_accepted = result.authenticated();
        let (reported_ok, ok) = unlock_outcome(backend_accepted, denied);
        let live = attempt
            .origin
            .is_some_and(|origin| self.control.origin_pending_live(origin));
        if !result.may_retire() {
            self.unlock_quarantined = Some(attempt);
            if std::env::var_os("TUNA_LOCK_TRACE").is_some() {
                eprintln!(
                    "tuna-compositor: lock authentication unretired {}",
                    serde_json::json!({
                        "pid":std::process::id(), "joined_ok":joined_ok,
                        "attempt_serial":attempt.serial,"attempt_epoch":attempt.epoch,
                        "current_epoch":self.unlock_generation.load(std::sync::atomic::Ordering::Acquire),
                        "origin_live":live,"locked_after":self.is_locked()
                    })
                );
            }
            if !denied {
                if let Some(origin) = attempt.origin {
                    self.control.finish_unlock(origin, false);
                }
            }
            return;
        }
        let completion_live =
            self.unlock_slot
                .completion_live(attempt, &self.unlock_generation, live);
        if !self.unlock_slot.retire_pam_method(
            attempt,
            crate::auth_owner::MethodKind::Password,
            result,
        ) {
            return;
        }
        let applied = ok && completion_live && self.apply_verified_unlock(attempt.epoch);
        if std::env::var_os("TUNA_LOCK_TRACE").is_some() {
            eprintln!(
                "tuna-compositor: lock authentication retired {}",
                serde_json::json!({
                    "pid": std::process::id(), "joined_ok": joined_ok,
                    "backend_accepted": backend_accepted, "reported_ok": reported_ok,
                    "applied": applied, "attempt_serial": attempt.serial,
                    "attempt_epoch": attempt.epoch,
                    "current_epoch": self.unlock_generation.load(std::sync::atomic::Ordering::Acquire),
                    "origin": attempt.origin.map(|origin| serde_json::json!({
                        "connection": origin.connection, "request": origin.request })),
                    "origin_live": live, "locked_after": self.is_locked()
                })
            );
        }
        if std::env::var_os("TUNA_LOCK_TRACE").is_some() {
            eprintln!(
                "tuna-compositor: lock authentication owned retirement {}",
                serde_json::json!({
                    "pid":std::process::id(), "joined_ok":joined_ok,
                    "retired":matches!(result, crate::pam_backend::Verification::Retired { .. }),
                    "authenticated":result.authenticated(), "denied":denied,
                    "cancel_requested":cancel_requested,"reported_ok":reported_ok,"applied":applied,
                    "attempt_serial":attempt.serial,"attempt_epoch":attempt.epoch,
                    "current_epoch":self.unlock_generation.load(std::sync::atomic::Ordering::Acquire),
                    "origin":attempt.origin.map(|origin| serde_json::json!({
                        "connection":origin.connection,"request":origin.request })),
                    "locked_after":self.is_locked()
                })
            );
        }
        if !denied {
            if let Some(origin) = attempt.origin {
                self.control.finish_unlock(origin, applied);
            }
        }
    }

    /// Route one backend input event, after [`Self::on_manager_input`]
    /// coalesced motion: consumed by the lock surface while
    /// locked (credential entry submits through [`try_unlock`](Self::try_unlock);
    /// windows hear nothing), else to the recovery overlay while it is visible,
    /// else through the overview triggers to the window manager.
    /// Trigger events still reach clients (tap toggles without
    /// breaking Super-combos); only Escape-closes is consumed, so a
    /// closing keypress never double-acts on client UI.
    fn dispatch_manager_input(&mut self, input: ManagerInput) {
        if let Some(monitor) = &self.a11y_keyboard {
            monitor.set_locked(self.is_locked());
            if self
                .manager
                .accessibility_input(&mut self.state, monitor, &input)
            {
                // Activity still wakes/feeds idle; consumed keys never enter
                // real seat XKB, WM shortcuts, overview or focused clients.
                self.lock.note_input(input_time(&input));
                self.idle_since = Instant::now();
                self.idle_monitor.activity();
                self.active_at = Instant::now();
                return;
            }
        }
        // A Super+L release can arrive after the lock takes ownership, or
        // before its surface maps. Consumed input must still clear modifier
        // holds, otherwise an ordinary post-unlock click becomes Super+drag.
        self.manager.note_modifiers(&input);
        // CI-only count markers locate missing pointer delivery without
        // recording button codes, key values or credential input.
        if !self.is_locked()
            && matches!(input, ManagerInput::Button { .. })
            && std::env::var_os("TUNA_POINTER_TRACE").is_some()
        {
            eprintln!("tuna-compositor: pointer trace: backend button received");
        }
        // Every timestamped event feeds the idle accumulator first,
        // including events consumed below: activity is activity.
        let waking_blank = self.blank_alpha() > 0.0;
        self.lock.note_input(input_time(&input));
        self.idle_since = Instant::now();
        self.idle_monitor.activity();
        self.active_at = Instant::now();
        if self.is_locked() {
            self.cursor.locate.reset_keys();
            if matches!(
                input,
                ManagerInput::Key { .. } | ManagerInput::Button { .. }
            ) {
                self.auth.note_user_input();
            }
        } else {
            // GNOME's locate-pointer: a lone Ctrl tap; the key still
            // reaches the client (#342).
            let now = self.animation_clock.now();
            let matches_key = if let ManagerInput::Key { keycode, .. } = &input {
                self.state.seat.get_keyboard().is_some_and(|keyboard| {
                    keyboard.with_xkb_state(&mut self.state, |context| {
                        let Ok(xkb) = context.xkb().lock() else {
                            return false;
                        };
                        let code = keycode.saturating_add(8).into();
                        // SAFETY: the state is borrowed only while the XKB
                        // mutex is held; no state reference escapes this call.
                        let layout = smithay::input::keyboard::Layout(unsafe {
                            xkb.state().key_get_layout(code)
                        });
                        self.cursor.locate.matches_keysym(
                            xkb.raw_syms_for_key_in_layout(code, layout)
                                .iter()
                                .map(|symbol| symbol.raw()),
                        )
                    })
                })
            } else {
                false
            };
            self.cursor
                .locate
                .feed_matched(&input, matches_key, self.manager.pointer_pos(), now);
        }
        if self.is_locked() {
            // The lock takes the keyboard: a held Escape's release
            // never comes back to the drag cancel below.
            self.dnd_escape_held = false;
            // Only the lock screen hears input; with none up (shell
            // gone), nothing does.
            let surface = self
                .state
                .primary_output_name()
                .and_then(|name| self.state.lock_surface_for(&name));
            if let Some(surface) = surface {
                self.manager.lock_input(&mut self.state, &surface, input);
            } else {
                self.manager.discard_key_input(&mut self.state, &input);
            }
            // Media keys still work on the lock screen, as in GNOME.
            for (action, time, mode) in self.manager.take_accelerators_fired() {
                self.control.queue_accelerator(action, time, mode);
            }
            for (action, time, mode) in self.manager.take_accelerators_released() {
                self.control
                    .queue_accelerator_deactivated(action, time, mode);
            }
            return;
        }
        if waking_blank {
            self.manager.discard_key_input(&mut self.state, &input);
            // Activity cancels the idle shield; the wake event belongs to
            // that shield, not the previously focused application.
            return;
        }
        // Mutter's drag grab: Escape cancels a client drag-and-drop and
        // is consumed, so the overview stays open and the release can
        // no longer drop (a Wayland drag owns the pointer, not the keys).
        if let ManagerInput::Key {
            keycode: ESCAPE_KEYCODE,
            pressed,
            ..
        } = &input
        {
            if self.dnd_escape_held {
                // Repeats and the matching release belong to the cancelled drag.
                self.dnd_escape_held = *pressed;
                self.manager.discard_key_input(&mut self.state, &input);
                return;
            }
            if *pressed
                && self.state.client_dnd_active()
                && self.state.cancel_client_dnd(input_time(&input) as u32)
            {
                self.dnd_escape_held = true;
                self.manager.discard_key_input(&mut self.state, &input);
                return;
            }
        }
        // Three-finger swipes are the shell's (GNOME 51): consumed here,
        // acted on at the end. Other swipes reach the client.
        if self.shell_swipe_input(&input) {
            return;
        }
        if !self.overlay.visible {
            if let ManagerInput::Key {
                keycode: ESCAPE_KEYCODE,
                pressed: true,
                ..
            } = input
            {
                if self.control.overview_open() {
                    self.control.set_overview(false);
                    return;
                }
            }
            if self.control.overview_open() {
                if let ManagerInput::Button { pressed: true, .. } = input {
                    if self.overview_press() {
                        return;
                    }
                }
            } else {
                self.overview_drag = None;
            }
            if self.overview_drag.is_some() {
                match input {
                    ManagerInput::Button { pressed: false, .. } => {
                        // The press never reached a client: neither
                        // does its release.
                        self.overview_release();
                        return;
                    }
                    ManagerInput::Motion { pos, .. } => {
                        if let Some((_, start, dragging)) = &mut self.overview_drag {
                            let (dx, dy) = (pos.x - start.x, pos.y - start.y);
                            if !*dragging && dx.hypot(dy) > crate::overview::DRAG_THRESHOLD {
                                *dragging = true;
                                self.overview_motion.drag_begin(self.input_settings.motion);
                            }
                        }
                    }
                    _ => {}
                }
            }
            // Super's tap belongs to a window inhibiting shortcuts
            // (keyboard-shortcuts-inhibit, #89).
            let inhibited =
                matches!(input, ManagerInput::Key { .. }) && self.state.shortcuts_inhibited();
            let popup = crate::layer::exclusive_popup_keyboard_layer(&self.state).is_some();
            let action = if inhibited || popup {
                self.triggers.cancel();
                TriggerAction::None
            } else {
                self.state.overview_trigger_action(
                    &mut self.triggers,
                    &input,
                    self.control.overview_open(),
                    self.manager.pointer_pos(),
                )
            };
            match action {
                TriggerAction::None => {}
                TriggerAction::Toggle => {
                    if let ManagerInput::CornerPressure { pos, .. } = input {
                        if !self.control.overview_open() && self.manager.fullscreen_at(pos) {
                            return;
                        }
                    }
                    self.control.set_overview(!self.control.overview_open());
                    if let ManagerInput::CornerPressure { pos, .. } = input {
                        self.ripple_hot_corner(pos);
                    }
                    #[cfg(feature = "drm")]
                    if matches!(self.backend, Backend::Drm(_)) {
                        self.performance_trace.input(
                            Duration::from(self.state.presentation_now()),
                            self.control.overview_open(),
                        );
                    }
                }
                TriggerAction::Open => {
                    // GNOME's corner cannot open over a fullscreen monitor.
                    // Super and Activities remain deliberate toggles; only
                    // the motion-driven corner uses this guard.
                    let blocked = matches!(&input, ManagerInput::Motion { pos, .. }
                        if self.manager.fullscreen_at(*pos));
                    if !blocked {
                        self.control.set_overview(true);
                        if let ManagerInput::Motion { pos, .. } = input {
                            self.ripple_hot_corner(pos);
                        }
                    }
                }
            }
            self.manager.on_input(&mut self.state, input);
            for backward in self.manager.take_input_source_switches() {
                self.switch_input_source_with_aid_reset(backward);
            }
            // Alt-Tab drive: forward whatever the manager queued into
            // the hub broadcast; the next hub poll delivers it to the
            // shell, which owns MRU order and rendering.
            for action in self.manager.take_switcher_queue() {
                // A closing switcher's window images fade out with its
                // popup (switcherPopup.js fadeAndDestroy): the shell keeps
                // sending their frames at the fading opacity and clears
                // them once the fade ends; the control hub clears them if
                // that shell goes away first, and the lock clears them.
                self.control.queue_switcher(action);
            }
            for (action, time, mode) in self.manager.take_accelerators_fired() {
                self.control.queue_accelerator(action, time, mode);
            }
            for (action, time, mode) in self.manager.take_accelerators_released() {
                self.control
                    .queue_accelerator_deactivated(action, time, mode);
            }
            for (index, count) in self.manager.take_workspace_popups() {
                self.control
                    .queue_message(tuna_shell_control::Message::WorkspacePopup { index, count });
            }
            return;
        }
        self.manager.discard_key_input(&mut self.state, &input);
        let ManagerInput::Key {
            keycode,
            pressed: true,
            ..
        } = input
        else {
            return;
        };
        let Some(key) = overlay_key_for_keycode(keycode) else {
            return;
        };
        match self.overlay.apply_key(key) {
            None => {}
            Some(RecoveryAction::ListWindows) => self.refresh_overlay(),
            Some(RecoveryAction::RelaunchShell) => {
                self.overlay.record_restart();
                self.shell.reset_budget();
            }
            Some(RecoveryAction::ShowOverlay) => self.overlay.hide(),
        }
    }

    /// Windows currently mapped, as overlay list entries served from
    /// compositor state (no shell IPC needed).
    fn overlay_windows(&self) -> Vec<OverlayWindow> {
        self.manager
            .model()
            .snapshot()
            .windows
            .iter()
            .map(|window| OverlayWindow {
                id: window.id,
                title: window.title.clone(),
            })
            .collect()
    }

    /// Show the recovery overlay with a fresh window list, or refresh
    /// the visible list in place (selection survives when its id does).
    fn refresh_overlay(&mut self) {
        if self.overlay.visible {
            let titles: Vec<(u64, String)> = self
                .overlay_windows()
                .iter()
                .map(|window| (window.id, window.title.clone()))
                .collect();
            self.overlay.refresh_from_snapshot(&titles);
        } else {
            self.overlay.show(self.overlay_windows());
        }
    }

    /// Dispatch clients, reconcile windows, supervise the shell, produce
    /// one frame, and report whether to continue. The shell step never
    /// blocks the tick: absence shows the overlay, exhaustion stays
    /// calm, and every outcome is logged redacted (codes/counts only).
    /// Record the first X11 need: the supervisor leaves `Idle` and the
    /// next tick may spawn the compatibility server once its spawner
    /// is installed. Session opt-in calls this at launch; until then
    /// native sessions never request.
    pub fn request_x11(&mut self) {
        self.xwayland.request();
    }

    /// Advertise the reserved display to apps, independently of XWM readiness.
    pub fn set_x11_display(&mut self, display: u32) {
        self.x11_display = Some(display);
        self.control
            .set_environment(vec![("DISPLAY".to_owned(), format!(":{display}"))]);
    }

    #[cfg(feature = "xwayland")]
    fn prepare_x11(&mut self, display: Option<u32>) -> Result<(), Box<dyn std::error::Error>> {
        let sockets = smithay::xwayland::XWaylandSockets::prepare(display, true)?;
        let number = sockets.display_number();
        let handles = sockets
            .listen_fds()
            .map(|fd| fd.try_clone_to_owned())
            .collect::<Result<Vec<_>, _>>()?;
        let mut watchers = Vec::new();
        for owned in handles {
            match self.loop_handle.insert_source(
                calloop::generic::Generic::new(
                    owned,
                    calloop::Interest::READ,
                    calloop::Mode::Level,
                ),
                |_, _, runtime| {
                    runtime.request_x11();
                    Ok(calloop::PostAction::Disable)
                },
            ) {
                Ok(token) => watchers.push(token),
                Err(error) => {
                    for token in watchers {
                        self.loop_handle.remove(token);
                    }
                    return Err(Box::new(error));
                }
            }
        }
        self.x11_watchers = watchers;
        self.x11_sockets = Some(sockets);
        self.set_x11_display(number);
        Ok(())
    }

    /// Loop handle for event-source installation (XWayland spawn).
    pub fn loop_handle(&self) -> LoopHandle<'static, Runtime> {
        self.loop_handle.clone()
    }

    /// Mutable protocol state, for the X11 handler impls.
    pub fn state_mut(&mut self) -> &mut State {
        &mut self.state
    }

    /// Take the XWayland server client awaiting its `Ready` event.
    /// (xwayland feature only.)
    #[cfg(feature = "xwayland")]
    pub fn take_pending_x11_client(
        &mut self,
    ) -> Option<smithay::reexports::wayland_server::Client> {
        self.pending_x11_client.take()
    }

    #[cfg(feature = "xwayland")]
    pub(crate) fn x11_startup_failed(&mut self) {
        self.x11_failed = true;
    }

    /// Handle one XWayland server event (spawned source callback).
    /// (xwayland feature only.)
    #[cfg(feature = "xwayland")]
    pub fn on_xwayland_event(&mut self, event: smithay::xwayland::XWaylandEvent) {
        crate::xwayland::on_xwayland_event(self, event);
    }

    /// Live libinput device configuration (#343): empty off hardware,
    /// where there is no libinput context.
    fn input_device_states(&self) -> Vec<serde_json::Value> {
        #[cfg(feature = "drm")]
        if let Backend::Drm(drm) = &self.backend {
            return drm.input_device_states();
        }
        Vec::new()
    }

    /// Write the compositor-side state snapshot when it changed
    /// (`TUNA_COMPOSITOR_STATE`, journeys only; #65). App ids and ids,
    /// never titles.
    fn publish_state(&mut self) {
        if self.state_path.is_none() {
            return;
        }
        let mut keyboard = self.manager.keyboard_summary(&mut self.state);
        if let Some(keyboard) = keyboard.as_object_mut() {
            let saving = self.numlock.saving(self.manager.keyboard_num_lock());
            keyboard.insert("numlock_saving".into(), saving.into());
        }
        let Some(path) = self.state_path.as_ref() else {
            return;
        };
        let model = self.manager.model();
        let snapshot = model.snapshot();
        let app_of = |id: u64| {
            snapshot
                .windows
                .iter()
                .find(|w| w.id == id)
                .and_then(|w| w.app_id.clone())
        };
        let overview_open = self.control.overview_open();
        // Previews only once the transition settles: harnesses click
        // where they say.
        let progress = self.overview_motion.progress();
        let scene = (overview_open && progress >= 1.0).then(|| self.overview_layout());
        // Record the drawn transition separately from settled click targets.
        let transition_scene = (progress > 0.0).then(|| self.overview_layout());
        let workspace_cards: Vec<serde_json::Value> = transition_scene
            .iter()
            .flat_map(|l| l.cards.iter())
            .map(|c| {
                serde_json::json!({
                    "workspace": c.workspace, "active": c.active, "alpha": c.alpha,
                    "rect": [c.rect.loc.x, c.rect.loc.y, c.rect.size.w, c.rect.size.h],
                })
            })
            .collect();
        // GNOME's workspace thumbnails strip (three or more workspaces).
        let thumbnails: Vec<serde_json::Value> = transition_scene
            .iter()
            .flat_map(|l| l.thumbnails.iter())
            .map(|t| {
                serde_json::json!({
                    "workspace": t.workspace,
                    "active": t.active,
                    "alpha": t.alpha,
                    "rect": [t.rect.loc.x, t.rect.loc.y, t.rect.size.w, t.rect.size.h],
                })
            })
            .collect();
        let previews: Vec<serde_json::Value> = if let Some(scene) = &scene {
            scene
                .previews
                .iter()
                .filter(|p| p.active)
                .map(|p| {
                    serde_json::json!({
                        "id": p.id,
                        "app_id": app_of(p.id),
                        "rect": [p.rect.loc.x, p.rect.loc.y, p.rect.size.w, p.rect.size.h],
                    })
                })
                .collect()
        } else {
            Vec::new()
        };
        let focused = model.focused();
        #[cfg(feature = "xwayland")]
        let x11_ready = self.state.xwm.is_some();
        #[cfg(not(feature = "xwayland"))]
        let x11_ready = false;
        let mut doc = serde_json::json!({
            "rendered_frames": self.stats.frames,
            "x11_display": self.x11_display.map(|d| format!(":{d}")),
            "capture_streams": self.casts.len(),
            "remote_input_sessions": self.remote_held.len(),
            // Held-state proof needs only a count, including during lock.
            // Never emit key identities that could expose credential input.
            "seat_pressed_key_count": self.state.pressed_key_count(),
            "x11_ready": x11_ready,
            "idle_timeout_ms": self.lock.timeout_ms(),
            "idle_blank_alpha": self.blank_alpha(),
            "power_save_mode": self.power_save.mode(),
            "overview_search": self.overview_search,
            "overview_app_grid": self.overview_app_grid,
            "keyboard": keyboard,
            "background_paint": if self.is_locked() { None } else { self.wallpaper.paint_state() },
            "overview_open": overview_open,
            "animations_enabled": self.input_settings.motion.animations_enabled(),
            "mouse_left_handed": self.input_settings.mouse_left_handed,
            "hot_corners": self.input_settings.hot_corners,
            "touchpad_left_handed": self.input_settings.touchpad_left_handed,
            // GNOME's per-class pointer settings as the shell sent them
            // (#343): the GSettings-to-compositor boundary nested proofs
            // assert; hardware sessions also report applied libinput
            // state per device in `input_devices`.
            "pointer_device_settings": {
                "mouse_accel_profile": self.input_settings.mouse_accel_profile,
                "touchpad": self.input_settings.touchpad,
                "mouse": self.input_settings.mouse,
                "trackball": self.input_settings.trackball,
                "pointing_stick": self.input_settings.pointing_stick,
            },
            "locked": self.is_locked(),
            // Administrative lockdown backstop (#357): the live
            // `disable-lock-screen` decision `Lock` and idle obey.
            "lock_screen_allowed": self.control.lock_screen_allowed(),
            "active_workspace": model.active_workspace(),
            "focused": focused,
            "focused_app_id": focused.and_then(app_of),
            "focused_rect": focused
                .and_then(|id| self.manager.geometry(id))
                .map(|g| [g.loc.x, g.loc.y, g.size.w, g.size.h]),
            // Floating ("gnome") or niri's strip ("scroll"), and how far
            // the strip's view has scrolled.
            "session_mode": match self.manager.session_mode() {
                crate::windows::SessionMode::Scroll => "scroll",
                crate::windows::SessionMode::Gnome => "gnome",
            },
            "strip_offset": self.manager.strip_offset(),
            // Where the strip is drawn this frame: trails the target
            // on niri's view-movement spring, equal once settled.
            "strip_view": self.manager.strip_view(),
            // GNOME's size-change transitions (#496): in flight, and the
            // last few with every drawn frame, for the proofs.
            "size_changes": self.manager.size_changes().to_json(),
            "minimized": snapshot
                .windows
                .iter()
                .map(|w| w.id)
                .filter(|id| self.manager.is_minimized(*id))
                .collect::<Vec<_>>(),
            "above": snapshot
                .windows
                .iter()
                .map(|w| w.id)
                .filter(|id| self.manager.is_above(*id))
                .collect::<Vec<_>>(),
            "windows": self.manager.overview_windows().iter().map(|w| serde_json::json!({
                "id": w.id,
                "app_id": app_of(w.id),
                "introspect_eligible": self.manager.is_introspect_eligible(w.id),
                "x11_window_id": self.manager.x11_window_id(w.id),
                "parent_window_id": self.manager.transient_parent(w.id),
                "application_window_id": self.manager.application_window(w.id),
                "icon": model.window(w.id).and_then(|w| w.icon.clone()),
                "workspace": w.workspace,
                "rect": [w.geometry.loc.x, w.geometry.loc.y, w.geometry.size.w, w.geometry.size.h],
            })).collect::<Vec<_>>(),
            "previews": previews,
            "thumbnails": thumbnails,
            "workspace_placeholder": scene.as_ref().and_then(|s| s.placeholder).map(|(at, r)| serde_json::json!({
                "workspace": at, "rect": [r.loc.x, r.loc.y, r.size.w, r.size.h],
            })),
            "overview_progress": progress,
            "workspace_cards": workspace_cards,

            // The Alt+Tab switcher's window thumbnails being drawn.
            "switcher_thumbnails": self.switcher_thumbnails.len(),
            // ... and how opaque each window image is drawn (#501).
            "switcher_thumbnail_alphas": switcher_previews(&self.manager, &self.switcher_thumbnails)
                .iter()
                .map(|p| p.alpha)
                .collect::<Vec<_>>(),
            // GNOME's tile preview while a dragged window is over a
            // snap edge: the window and the area it would fill.
            "tile_preview": self.manager.tile_preview(&self.state).map(|(id, r)| {
                serde_json::json!({
                    "window": id,
                    "rect": [r.loc.x, r.loc.y, r.size.w, r.size.h],
                })
            }),
            "layers": crate::layer::layer_layout(&self.state)
                .iter()
                .filter_map(|(surface, (x, y), _)| {
                    let record = self
                        .state
                        .panel_surfaces
                        .iter()
                        .find(|r| r.surface == *surface)?;
                    let size = self
                        .state
                        .layer_shell_state
                        .layer_surfaces()
                        .find(|s| s.wl_surface() == surface)
                        .and_then(|s| s.current_state().size)?;
                    Some(serde_json::json!({
                        "namespace": record.namespace,
                        "rect": [x, y, size.w, size.h],
                    }))
                })
                .collect::<Vec<_>>(),
        });
        // Keep this nested object outside the large json! expansion so
        // additions cannot exceed serde_json's default macro recursion bound.
        // One clock/sample also makes the diagnostic tuple internally coherent.
        let wellbeing_now = self.animation_clock.now();
        let wellbeing_sample = self.wellbeing_effects.sample(wellbeing_now);
        doc["wellbeing_effects"] = serde_json::json!({
            "desaturation": wellbeing_sample.desaturation,
            "brightness": wellbeing_sample.brightness,
            "animating": self.wellbeing_effects.animating(wellbeing_now),
        });
        // Overdraw per output for the last drawn frame (#503).
        doc["workspaces"] = serde_json::json!(model.workspaces());
        // GNOME's Multitasking preferences as the compositor applies them
        // (P-WM-08), so nested proofs wait for delivery, not a sleep.
        let policy = &self.input_settings.workspace_policy;
        doc["multitasking"] = serde_json::json!({
            "dynamic_workspaces": policy.dynamic,
            "num_workspaces": policy.fixed_count,
            "workspaces_only_on_primary": policy.only_on_primary,
            "edge_tiling": policy.edge_tiling,
            "center_new_windows": self.input_settings.center_new_windows,
        });
        // Bottom-to-top stacking and GNOME's live focus policy (P-WM-09).
        doc["stacking"] = serde_json::json!(self.manager.stacking_order());
        let focus_policy = self.manager.focus_policy();
        doc["focus_policy"] = serde_json::json!({
            "focus_mode": focus_policy.mode.as_str(),
            "auto_raise": focus_policy.auto_raise,
            "auto_raise_delay_ms": focus_policy.auto_raise_delay.as_millis() as u64,
            "raise_on_click": focus_policy.raise_on_click,
            "modifier": focus_policy.modifier,
            "resize_with_right_button": focus_policy.resize_with_right_button,
            "focus_change_on_pointer_rest": focus_policy.focus_change_on_pointer_rest,
        });
        doc["window_lifecycle"] = self.manager.window_lifecycle().to_json();
        doc["menu_close"] = self.menu_close.to_json();
        doc["modal_dim"] = self.manager.modal_dim().to_json();
        doc["frame_cost"] = self
            .frame_costs
            .iter()
            .map(|(name, cost)| (name.clone(), cost.to_json()))
            .collect::<serde_json::Map<_, _>>()
            .into();
        // Windows whose frame callbacks run at the hidden rate.
        doc["throttled_windows"] = if self.overview_motion.progress() > 0.0
            || !self.switcher_thumbnails.is_empty()
            || !self.casts.is_empty()
        {
            serde_json::json!([])
        } else {
            let scene: Vec<u64> = self
                .manager
                .render_entries()
                .iter()
                .map(|(id, _, _)| *id)
                .collect();
            let shown = crate::occlusion::shown_windows(&scene, self.frame_costs.values());
            serde_json::json!(snapshot
                .windows
                .iter()
                .map(|w| w.id)
                .filter(|id| !shown.contains(id))
                .collect::<Vec<_>>())
        };
        doc["wallpaper_diagnostics"] = if self.is_locked() {
            serde_json::Value::Null
        } else {
            self.wallpaper.diagnostics()
        };
        // GNOME's workspace switch: the drawn strip while it moves, and
        // the last finished switches with their timing.
        doc["workspace_slide"] = self
            .manager
            .workspace_slide()
            .snapshot(self.animation_clock.now());
        doc["transitions"] = self.transitions_diagnostics();
        // GNOME Settings' display configuration (#340): modes, the
        // kernel's CRTC modes on hardware, pending confirmation.
        doc["display_config"] = self.display_config_diagnostics();
        doc["color"] = self.color_diagnostics();
        let motion = self.input_settings.motion;
        doc["motion_policy"] = serde_json::json!(motion.level.as_str());
        doc["animation_slowdown"] = serde_json::json!(motion.slowdown());
        // Only a frozen clock is reported: real time would rewrite the
        // state file every frame.
        let manual = self.animation_clock.is_manual();
        doc["animation_clock_manual"] = serde_json::json!(manual);
        doc["proof_swipe_sequence"] = serde_json::json!(self.proof_swipe_sequence);
        doc["animation_clock_ms"] =
            serde_json::json!(manual.then(|| self.animation_clock.now().as_millis() as u64));
        doc["overview_motion"] = self.overview_motion.diagnostics();
        doc["cursor"] = self
            .cursor
            .diagnostics(self.animation_clock.now(), self.is_locked());
        // The zoom region's geometry only (#349): no frame content.
        doc["magnifier"] = self.magnifier.diagnostics();
        doc["screen_reader_pid"] = serde_json::json!(self.orca.pid());
        doc["screen_reader_state"] = serde_json::json!(self.orca.state());
        doc["a11y_keyboard"] = self
            .a11y_keyboard
            .as_ref()
            .map_or(serde_json::Value::Null, |monitor| monitor.snapshot());
        // Keep backend-specific observations outside the large scene macro so
        // its expansion remains below the compiler's default recursion limit.
        // These values keep the same flat JSON fields and suppress lock input.
        doc["native_relative_motion_count"] = serde_json::json!((!self.is_locked())
            .then_some(match &self.backend {
                #[cfg(feature = "drm")]
                Backend::Drm(drm) => Some(drm.relative_motion_events()),
                Backend::Winit(_) => None::<u64>,
            })
            .flatten());
        doc["pointer_position"] = serde_json::json!((!self.is_locked()).then(|| {
            let pos = self.manager.pointer_pos();
            [pos.x, pos.y]
        }));
        doc["input_aids"] = self.input_aids_summary();
        // What the on-screen keyboard follows (#349): the focused field's
        // content type only, never text; nothing while locked.
        doc["text_input"] = if self.is_locked() {
            serde_json::Value::Null
        } else {
            let focus = self
                .state
                .seat
                .get_keyboard()
                .and_then(|k| k.current_focus());
            self.state
                .protocols
                .text_inputs
                .focus(focus.as_ref())
                .map_or(
                    serde_json::Value::Null,
                    |field| serde_json::json!({ "purpose": field.purpose, "hints": field.hints }),
                )
        };
        // Owned proof diagnostics only: no credentials, prompts or typed input.
        doc["auth_owner"] = if std::env::var_os("TUNA_LOCK_TRACE").is_some() {
            serde_json::json!({ "outer_pending": self.unlock_worker.is_some(),
                "conversations": self.auth.diagnostics(),
                "backend_unretired": self.unlock_quarantined.is_some(),
                "epoch": self.unlock_generation.load(std::sync::atomic::Ordering::Acquire),
                "attempt": self.unlock_worker.as_ref().map(|worker| worker.attempt).or(self.unlock_quarantined).or(self.auth.password_attempt()).map(|attempt| serde_json::json!({
                    "serial": attempt.serial, "epoch": attempt.epoch,
                    "origin": attempt.origin.map(|origin| serde_json::json!({
                        "connection": origin.connection, "request": origin.request }))
                })) })
        } else {
            serde_json::Value::Null
        };
        doc["pointer_fullscreen_blocked"] = serde_json::json!(
            (!self.is_locked()).then(|| self.manager.fullscreen_at(self.manager.pointer_pos()))
        );
        // Live libinput device configuration (#343): beside the macro,
        // which is already at its recursion limit.
        // Keep this unlocked drag observation outside the large scene macro.
        doc["client_dnd_active"] =
            serde_json::Value::Bool(!self.is_locked() && self.state.client_dnd_active());
        // Canonical view AND column spring state, outside the large scene macro.
        doc["strip_animating"] = serde_json::Value::Bool(self.manager.strip_animating());
        doc["input_devices"] = serde_json::Value::Array(self.input_device_states());
        #[cfg(feature = "drm")]
        if let Backend::Drm(drm) = &self.backend {
            doc["tablets"] = drm.tablets_introspect();
        }
        let doc = doc.to_string();
        if doc == self.state_last {
            return;
        }
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, &doc).is_ok() && std::fs::rename(&tmp, path).is_ok() {
            self.state_last = doc;
        }
    }

    /// The overview at rest on the primary output (#54): the window
    /// picker or the app grid, before any animation.
    fn overview_base(&self) -> crate::overview::OverviewLayout {
        let size = self.state.primary_size();
        let output = Rectangle::new((0, 0).into(), size);
        let model = self.manager.model();
        let layout = if self.overview_app_grid {
            crate::overview::app_grid_layout_with_policy
        } else {
            crate::overview::layout_with_policy
        };
        layout(
            output,
            crate::windows::WORK_AREA_TOP,
            model.workspaces(),
            model.active_workspace(),
            &self.manager.overview_windows(),
            model.fixed_workspace_count().is_none(),
        )
    }

    /// The preview being dragged and where it was grabbed.
    fn overview_dragged(&self) -> Option<(u64, Point<f64, Logical>)> {
        match self.overview_drag {
            Some((id, start, true)) => Some((id, start)),
            _ => None,
        }
    }

    /// Overview scene on the primary output as drawn this frame.
    fn overview_layout(&self) -> crate::overview::OverviewLayout {
        let output = Rectangle::new((0, 0).into(), self.state.primary_size());
        self.overview_motion.scene(
            &self.overview_base(),
            self.overview_dragged(),
            self.manager.pointer_pos(),
            output,
            &self.manager.overview_windows(),
        )
    }

    /// Advance the overview's animations (GNOME 51 timings, times the
    /// slow-down factor) unless a swipe holds the transition. Fade-only
    /// keeps fades and snaps motion. Returns whether the overview is
    /// drawn at all.
    fn step_overview_transition(&mut self) -> bool {
        let now = self.animation_clock.now();
        let dt = now.saturating_sub(self.overview_progress_at).as_secs_f64() * 1000.0;
        self.overview_progress_at = now;
        let cues = crate::overview::OverviewCues {
            open: self.control.overview_open(),
            app_grid: self.overview_app_grid,
            search: self.overview_search,
            motion: self.input_settings.motion,
        };
        self.overview_motion
            .step_progress(dt, cues.open, cues.motion);
        if self.overview_motion.progress() > 0.0 {
            let base = self.overview_base();
            self.overview_motion.step_scene(
                dt,
                &base,
                cues,
                self.overview_dragged(),
                self.manager.pointer_pos(),
            );
        } else {
            self.overview_motion.reset_scene();
        }
        self.overview_motion.progress() > 0.0
    }

    fn animated_tile_preview(
        &mut self,
        target: Option<(u64, Rectangle<i32, Logical>)>,
    ) -> Option<(u64, Rectangle<i32, Logical>)> {
        let Some((id, to)) = target else {
            self.tile_animation = None;
            return None;
        };
        let now = self.animation_clock.now();
        let motion = self.input_settings.motion;
        let unchanged = self
            .tile_animation
            .as_ref()
            .is_some_and(|(old, animation, _)| *old == id && animation.to == to);
        if !unchanged {
            let from = self
                .tile_animation
                .as_ref()
                .filter(|(old, _, _)| *old == id)
                .map(|(_, animation, start)| {
                    animation.value_at(
                        now.saturating_sub(*start).as_secs_f64() / motion.slowdown(),
                        motion.allows_motion(),
                    )
                })
                .or_else(|| self.manager.render_geometry(id))
                .unwrap_or(to);
            self.tile_animation = Some((id, crate::animation::EaseRect { from, to }, now));
        }
        let (_, animation, start) = self.tile_animation.as_ref()?;
        Some((
            id,
            animation.value_at(
                now.saturating_sub(*start).as_secs_f64() / motion.slowdown(),
                motion.allows_motion(),
            ),
        ))
    }

    /// Advance the scroll-mode strip view on its spring by the time
    /// since the last frame (niri's view-movement animation), slowed by
    /// GNOME's slow-down factor.
    fn step_strip_view(&mut self) {
        let now = self.animation_clock.now();
        let dt = now.saturating_sub(self.strip_view_at).as_secs_f64();
        self.strip_view_at = now;
        self.manager
            .step_strip_view_with_pointer_dt(dt / self.input_settings.motion.slowdown(), dt);
    }

    /// Start and advance GNOME's size-change transitions (#496) before
    /// the scene is built. Skipped while the overview is open or a
    /// three-finger swipe is held, as GNOME's `_shouldAnimate`.
    fn step_size_changes(&mut self) {
        let policy = crate::size_change::Policy::from_motion(self.input_settings.motion);
        let allowed = !self.control.overview_open() && self.shell_swipe.is_none();
        let now = self.animation_clock.now();
        let renderer = match &mut self.backend {
            Backend::Winit(backend) => backend.renderer(),
            #[cfg(feature = "drm")]
            Backend::Drm(drm) => &mut drm.renderer,
        };
        crate::size_change::step_frame(
            &mut self.manager,
            renderer,
            &self.state,
            now,
            policy,
            allowed,
        );
    }

    /// Follow the active workspace with GNOME's switch animation; the
    /// overview (and the lock) settle it at once.
    fn step_workspace_slide(&mut self) {
        let now = self.animation_clock.now();
        let policy = self.input_settings.motion;
        let suspended = self.overview_motion.progress() > 0.0
            || self.control.overview_open()
            || self.is_locked();
        let width = self.state.primary_size().w;
        let model = self.manager.model();
        let active = model.active_workspace();
        let workspaces = model.workspaces().to_vec();
        let slide = self.manager.workspace_slide_mut();
        slide.observe(active, &workspaces, width, policy, suspended, now);
        slide.step(now);
    }

    /// A press while the overview is open that no shell surface took:
    /// focus the preview's window, switch to a neighbor card's
    /// workspace, or close the overview (GNOME shape). Returns whether
    /// the press was consumed.
    fn overview_press(&mut self) -> bool {
        let pos = self.manager.pointer_pos();
        if crate::layer::topmost_layer_at(&self.state, pos.x as i32, pos.y as i32).is_some()
            || self.manager.popup_at(&self.state, pos).is_some()
        {
            return false;
        }
        // Search hides the cards: a press beside the results dismisses.
        if self.overview_search {
            self.control.set_overview(false);
            return true;
        }
        match crate::overview::hit(&self.overview_layout(), pos) {
            crate::overview::OverviewHit::Window(id) => {
                // A click (focus) or a drag (move to a workspace): the
                // release decides.
                self.overview_drag = Some((id, pos, false));
            }
            crate::overview::OverviewHit::Workspace(ws) => {
                // GNOME's trailing empty workspace is not in the model
                // yet: it sits right after the active (last) one, and
                // stepping onto it creates it.
                if !self.manager.switch_to_workspace(&mut self.state, ws) {
                    // From a thumbnail the active one need not be last.
                    if let Some(&last) = self.manager.model().workspaces().last() {
                        self.manager.switch_to_workspace(&mut self.state, last);
                    }
                    self.manager.switch_relative(&mut self.state, 1);
                }
            }
            crate::overview::OverviewHit::Dismiss => self.control.set_overview(false),
        }
        true
    }

    /// The button came up after a press on a preview: a click focuses
    /// the window and closes the overview; a drag dropped on a
    /// thumbnail or a neighboring card moves the window to that
    /// workspace (GNOME), anywhere else puts it back.
    fn overview_release(&mut self) {
        let Some((id, _, dragging)) = self.overview_drag.take() else {
            return;
        };
        if !dragging {
            self.manager.focus(&mut self.state, Some(id));
            self.control.set_overview(false);
            return;
        }
        let pos = self.manager.pointer_pos();
        let layout = self.overview_layout();
        let motion = self.input_settings.motion;
        if let Some(at) = crate::overview::insertion_target(&layout, pos) {
            self.overview_motion.drag_released();
            self.manager
                .insert_workspace_and_move(&mut self.state, id, at);
            return;
        }
        // GNOME's drag ends: nowhere glides home, an accepted drop that
        // changes nothing fades back in place.
        let Some(target) = crate::overview::drop_target(&layout, pos) else {
            self.overview_motion.drag_snap_back(motion);
            return;
        };
        let here = self.manager.model().window(id).map(|w| w.workspace);
        if here != Some(target) {
            self.overview_motion.drag_released();
            self.manager.move_to_workspace(&mut self.state, id, target);
        } else {
            self.overview_motion.drag_revert(id, motion);
        }
    }

    /// Feed explicitly opted-in nested proofs through the real gesture handler.
    /// Each atomically replaced JSON file is consumed once; ordinary sessions
    /// never read it because state instrumentation is also required.
    fn proof_swipe_input(&mut self) {
        if self.state_path.is_none() {
            return;
        }
        let Some(path) = std::env::var_os("TUNA_PROOF_SWIPE_INPUT")
            .map(std::path::PathBuf::from)
            .filter(|p| p.is_absolute())
        else {
            return;
        };
        let Ok(body) = crate::runtime_signal::read(&path) else {
            return;
        };
        if body == self.proof_swipe_last {
            return;
        }
        self.proof_swipe_last = body.clone();
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&body) else {
            return;
        };
        self.proof_swipe_sequence = value["sequence"].as_u64();
        // Proofs may stamp phases to shape the release velocity.
        let time = value["time"]
            .as_u64()
            .map_or(crate::state::system_millis() as u32, |t| t as u32);
        let input = match value["phase"].as_str() {
            Some("begin") => ManagerInput::SwipeBegin {
                fingers: value["fingers"].as_u64().map_or(3, |f| f as u32),
                time,
            },
            Some("update") => ManagerInput::SwipeUpdate {
                delta: (
                    value["dx"].as_f64().unwrap_or(0.0),
                    value["dy"].as_f64().unwrap_or(0.0),
                )
                    .into(),
                time,
            },
            Some("end") => ManagerInput::SwipeEnd {
                cancelled: value["cancelled"].as_bool().unwrap_or(false),
                time,
            },
            // A two-finger touchpad scroll step (pixels), or with
            // `stop` libinput's terminating zero for the vertical axis.
            Some("scroll") => ManagerInput::Axis {
                horizontal: value["dx"].as_f64().unwrap_or(0.0),
                vertical: value["dy"].as_f64().unwrap_or(0.0),
                stop: crate::windows::AxisStop {
                    horizontal: false,
                    vertical: value["stop"].as_bool().unwrap_or(false),
                },
                time,
            },
            _ => return,
        };
        self.on_manager_input(input);
    }

    /// Track a three-finger swipe; returns whether `input` was one of
    /// its phases (and so consumed).
    fn shell_swipe_input(&mut self, input: &ManagerInput) -> bool {
        match *input {
            ManagerInput::SwipeBegin { fingers, .. } => {
                if fingers == crate::windows::SHELL_SWIPE_FINGERS {
                    self.shell_swipe = Some(Point::from((0.0, 0.0)));
                    return true;
                }
                false
            }
            ManagerInput::SwipeUpdate { delta, time } => match self.shell_swipe.as_mut() {
                Some(travel) => {
                    *travel += delta;
                    let travel = *travel;
                    // On the desktop a horizontal swipe drags the
                    // workspace strip with the fingers (GNOME).
                    let desktop =
                        !self.control.overview_open() && self.overview_motion.progress() == 0.0;
                    if self.manager.workspace_slide().following() {
                        self.manager
                            .workspace_slide_mut()
                            .update_swipe(delta.x, time);
                    } else if desktop
                        && !self.overview_motion.swiping()
                        && travel.x.abs() > travel.y.abs()
                    {
                        let policy = self.input_settings.motion;
                        let now = self.animation_clock.now();
                        let width = self.state.primary_size().w;
                        let model = self.manager.model();
                        let active = model.active_workspace();
                        let workspaces = model.workspaces().to_vec();
                        let slide = self.manager.workspace_slide_mut();
                        slide.begin_swipe(active, &workspaces, width, policy, now);
                        slide.update_swipe(travel.x, time);
                    }
                    // GNOME's overview follows the fingers: a vertical
                    // swipe moves the transition (up opens) as it goes.
                    if travel.y.abs() > travel.x.abs()
                        && !self.manager.workspace_slide().following()
                    {
                        self.overview_motion.swipe_update(
                            time,
                            -travel.y,
                            OVERVIEW_SWIPE_DISTANCE,
                            self.input_settings.motion,
                        );
                    }
                    true
                }
                None => false,
            },
            ManagerInput::SwipeEnd { cancelled, time } => {
                let Some(travel) = self.shell_swipe.take() else {
                    return false;
                };
                if self.manager.workspace_slide().following() {
                    // Released: the slide finishes on the fingers'
                    // velocity; the model switches now so focus and
                    // input never wait for it.
                    let now = self.animation_clock.now();
                    let target = self
                        .manager
                        .workspace_slide_mut()
                        .end_swipe(cancelled, time, now);
                    if let Some(target) = target {
                        if target != self.manager.model().active_workspace()
                            && !self.manager.switch_to_workspace(&mut self.state, target)
                        {
                            // GNOME's trailing empty workspace: create it.
                            self.manager.switch_relative(&mut self.state, 1);
                        }
                    }
                    return true;
                }
                // Released: finish where GNOME's swipe tracker would,
                // keeping the fingers' momentum (back where it began
                // when cancelled).
                if let Some(open) = self.overview_motion.swipe_end(
                    time,
                    cancelled,
                    OVERVIEW_SWIPE_DISTANCE,
                    self.input_settings.motion,
                ) {
                    self.control.set_overview(open);
                    return true;
                }
                if !cancelled {
                    use crate::windows::SwipeAction;
                    match crate::windows::swipe_action(travel.x, travel.y) {
                        Some(SwipeAction::OpenOverview) => self.control.set_overview(true),
                        Some(SwipeAction::CloseOverview) => self.control.set_overview(false),
                        Some(SwipeAction::NextWorkspace) => {
                            self.manager.switch_relative(&mut self.state, 1);
                        }
                        Some(SwipeAction::PreviousWorkspace) => {
                            self.manager.switch_relative(&mut self.state, -1);
                        }
                        None => {}
                    }
                }
                true
            }
            _ => false,
        }
    }

    fn start_screenshot_flash(&mut self, window: bool) -> Option<Duration> {
        if self.is_locked() || !self.input_settings.motion.allows_fades() {
            return None;
        }
        let area = if window {
            self.manager
                .model()
                .focused()
                .and_then(|id| self.manager.geometry(id))
        } else {
            self.state.primary_output_name().and_then(|name| {
                self.state
                    .outputs
                    .iter()
                    .find(|output| output.name == name)
                    .map(|output| Rectangle::<i32, Logical>::new(output.loc.into(), output.size))
            })
        };
        area.and_then(|area| {
            self.start_flash_area([area.loc.x, area.loc.y, area.size.w, area.size.h])
        })
    }

    fn start_flash_area(&mut self, area: [i32; 4]) -> Option<Duration> {
        if self.is_locked() || !self.input_settings.motion.allows_fades() {
            return None;
        }
        let now = self.animation_clock.now();
        let duration =
            Duration::from_secs_f64(self.input_settings.motion.adjust_ms(500.0) / 1000.0);
        self.screenshot_flashes.trigger(area, now, duration);
        now.checked_add(duration)
    }

    /// Render the primary output's current scene offscreen and save it
    /// as a PNG (`requested` when absolute, else GNOME's default path).
    /// Never while locked: nothing behind the lock may leave the session.
    pub fn capture(&mut self, requested: &std::path::Path) -> Option<std::path::PathBuf> {
        if self.is_locked() {
            return None;
        }
        let path =
            crate::screenshot::target_path(requested, &crate::screenshot::jiff_like::Stamp::now())?;
        let (w, h, rgba) =
            self.render_pixels(None, smithay::backend::allocator::Fourcc::Abgr8888)?;
        crate::screenshot::save_png(&path, w as u32, h as u32, &rgba).ok()?;
        eprintln!("tuna-compositor: screenshot saved to {}", path.display());
        Some(path)
    }

    /// Save the focused window alone as a PNG (GNOME's
    /// `ScreenshotWindow`): its visible geometry, popups included,
    /// client shadows left out. Never while locked.
    pub fn capture_window(&mut self, requested: &std::path::Path) -> Option<std::path::PathBuf> {
        if self.is_locked() {
            return None;
        }
        let id = self.manager.model().focused()?;
        let path =
            crate::screenshot::target_path(requested, &crate::screenshot::jiff_like::Stamp::now())?;
        let (w, h, rgba) =
            self.render_window_pixels(id, None, smithay::backend::allocator::Fourcc::Abgr8888)?;
        crate::screenshot::save_png(&path, w as u32, h as u32, &rgba).ok()?;
        eprintln!(
            "tuna-compositor: window screenshot saved to {}",
            path.display()
        );
        Some(path)
    }

    /// GNOME's screenshot window selector (screenshot.js
    /// `UIWindowSelector.capture`): every window on the active workspace,
    /// minimized ones left out, each saved at full size into `directory`
    /// and given its slot in the selector, laid out with GNOME's window
    /// spread inside the selector's margins. Never while locked.
    pub fn capture_selector_windows(
        &mut self,
        directory: &std::path::Path,
    ) -> Vec<crate::screenshot::WindowShot> {
        if self.is_locked() || std::fs::create_dir_all(directory).is_err() {
            return Vec::new();
        }
        let size = self.state.primary_size();
        let model = self.manager.model();
        let active = model.active_workspace();
        let focused = model.focused();
        let hidden = self.manager.hidden_by_minimize();
        let mut windows: Vec<(u64, Rectangle<i32, Logical>)> = self
            .manager
            .overview_windows()
            .into_iter()
            .filter(|w| w.workspace == active && !hidden.contains(&w.id))
            .map(|w| (w.id, w.geometry))
            .collect();
        // GNOME sorts by stable sequence: creation order (Tuna Desktop ids rise).
        windows.sort_by_key(|(id, _)| *id);
        let top = crate::windows::WORK_AREA_TOP;
        let workarea = Rectangle::new((0, top).into(), (size.w, (size.h - top).max(1)).into());
        let (ax, ay, aw, ah) = crate::screenshot::selector_area(size.w, size.h);
        let area = Rectangle::new((ax, ay).into(), (aw, ah).into());
        let slots = crate::overview::window_slots_spaced(
            workarea,
            size.h,
            area,
            &windows,
            crate::overview::SELECTOR_SPACING,
        );
        let mut shots = Vec::new();
        for (id, rect, _) in slots {
            let title = self
                .manager
                .model()
                .window(id)
                .map(|w| w.title.clone())
                .unwrap_or_default();
            let Some((w, h, rgba)) =
                self.render_window_pixels(id, None, smithay::backend::allocator::Fourcc::Abgr8888)
            else {
                continue;
            };
            let path = directory.join(format!("window-{id}.png"));
            if crate::screenshot::save_png(&path, w as u32, h as u32, &rgba).is_err() {
                continue;
            }
            shots.push(crate::screenshot::WindowShot {
                id,
                title,
                focused: focused == Some(id),
                x: rect.loc.x,
                y: rect.loc.y,
                width: rect.size.w,
                height: rect.size.h,
                path,
            });
        }
        shots
    }

    /// Physical size of window `id` at its output's scale, and the scale.
    fn window_pixel_size(&self, id: u64) -> Option<((i32, i32), f64)> {
        let geometry = self.manager.geometry(id)?;
        let scale = match &self.backend {
            Backend::Winit(_) => self.scale,
            #[cfg(feature = "drm")]
            Backend::Drm(_) => self.state.scale_for(geometry).fractional_scale(),
        };
        let px = |v: i32| ((f64::from(v) * scale).round() as i32).max(1);
        Some(((px(geometry.size.w), px(geometry.size.h)), scale))
    }

    /// Render one window offscreen with its visible geometry at (0, 0),
    /// into `size` physical pixels (default: the window's own), and read
    /// it back in `fourcc` byte order. Never while locked.
    fn render_window_pixels(
        &mut self,
        id: u64,
        size: Option<(i32, i32)>,
        fourcc: smithay::backend::allocator::Fourcc,
    ) -> Option<(i32, i32, Vec<u8>)> {
        use smithay::backend::renderer::{ExportMem, Offscreen};
        if self.is_locked() {
            return None;
        }
        let surface = self.manager.surface_of(id)?;
        let (natural, scale) = self.window_pixel_size(id)?;
        let (w, h) = size.unwrap_or(natural);
        let view = View {
            offset: (0, 0),
            scale,
        };
        let wellbeing = self.wellbeing_effects.sample(self.animation_clock.now());
        let renderer = match &mut self.backend {
            Backend::Winit(backend) => backend.renderer(),
            #[cfg(feature = "drm")]
            Backend::Drm(drm) => &mut drm.renderer,
        };
        let color_effect = crate::wellbeing_effects::FrameColor::new(renderer, wellbeing, None);
        // Surface-local origin such that the window geometry (inside any
        // client-side shadow) starts at the buffer's corner.
        let geo = crate::popup::window_geometry_loc(&surface);
        let origin: Point<i32, Logical> = (-geo.x, -geo.y).into();
        let mut elements: Vec<WaylandSurfaceRenderElement<GlesRenderer>> = Vec::new();
        let mut tree =
            |renderer: &mut GlesRenderer,
             surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
             at: Point<i32, Logical>| {
                elements.extend(render_elements_from_surface_tree(
                    renderer,
                    surface,
                    view.physical(f64::from(at.x), f64::from(at.y)),
                    view.scale,
                    1.0,
                    Kind::Unspecified,
                ));
            };
        tree(renderer, &surface, origin);
        for popup in crate::popup::placed_popups(&surface, origin, true) {
            tree(renderer, &popup.surface, popup.origin);
        }
        let elements = Scene::flat(crate::layer::front_to_back(elements));
        let buffer_size = (w, h).into();
        let mut texture: smithay::backend::renderer::gles::GlesTexture =
            renderer.create_buffer(fourcc, buffer_size).ok()?;
        let size: smithay::utils::Size<i32, smithay::utils::Physical> = (w, h).into();
        let mut target = renderer.bind(&mut texture).ok()?;
        {
            let mut frame = renderer.render(&mut target, size, Transform::Normal).ok()?;
            draw_scene(
                &mut frame,
                Color32F::new(0.0, 0.0, 0.0, 0.0),
                &[],
                &elements,
                Target {
                    output: Rectangle::from_size(size),
                    damage: &[Rectangle::from_size(size)],
                    scale,
                    blank_alpha: 0.0,
                    tint: color_effect.as_ref(),
                },
                &[],
                &[],
            )
            .ok()?;
            let _ = frame.finish().ok()?;
        }
        let mapping = renderer
            .copy_framebuffer(&target, Rectangle::from_size(buffer_size), fourcc)
            .ok()?;
        let bytes = renderer.map_texture(&mapping).ok()?.to_vec();
        Some((w, h, bytes))
    }

    /// Logical union of the actual output layout, including gaps and negative
    /// origins. Static spanning/tile phase must share it across all outputs.
    fn background_desktop(&self) -> [i32; 4] {
        match &self.backend {
            Backend::Winit(backend) => {
                let size = backend.window_size();
                [
                    0,
                    0,
                    (f64::from(size.w) / self.scale).round() as i32,
                    (f64::from(size.h) / self.scale).round() as i32,
                ]
            }
            #[cfg(feature = "drm")]
            Backend::Drm(drm) => {
                let mut left = i32::MAX;
                let mut top = i32::MAX;
                let mut right = i32::MIN;
                let mut bottom = i32::MIN;
                for out in drm.outputs.iter().filter(|o| o.enabled) {
                    let size = out.logical_size();
                    left = left.min(out.loc.0);
                    top = top.min(out.loc.1);
                    right = right.max(out.loc.0.saturating_add(size.0));
                    bottom = bottom.max(out.loc.1.saturating_add(size.1));
                }
                [
                    left,
                    top,
                    right.saturating_sub(left),
                    bottom.saturating_sub(top),
                ]
            }
        }
    }

    /// Resolve only currently rendered target windows. A destroyed/hidden
    /// explicit target does not redirect a frame flash to another window.
    fn bell_frame(&mut self) -> Option<BellFrame> {
        let now = Instant::now();
        self.state.protocols.bell.tick(now);
        if self.is_locked() {
            self.state.protocols.bell.clear_visual();
            return None;
        }
        let (alpha, surface) = self.state.protocols.bell.visual(now)?;
        let target = if let Some(surface) = surface {
            Some(
                self.manager
                    .render_entries()
                    .into_iter()
                    .find_map(|(id, _, geo)| {
                        (self.manager.surface_of(id).as_ref() == Some(surface)).then_some((id, geo))
                    })?,
            )
        } else {
            None
        };
        Some((alpha, target))
    }

    /// The desktop clear: GNOME's `primary-color` once the shell has
    /// published it, else a dark neutral.
    fn desktop_color(&self) -> Color32F {
        match self.wallpaper.color() {
            Some([r, g, b]) => Color32F::new(r, g, b, 1.0),
            None => Color32F::new(0.08, 0.09, 0.11, 1.0),
        }
    }

    /// Render one output's current scene (the primary, or `connector`)
    /// offscreen and read it back in `fourcc` byte order (Abgr8888 is
    /// RGBA in memory, Xrgb8888 is BGRx). Never while locked. When the
    /// screen magnifier is on (#349), the frame goes through the same
    /// zoom passes as the display, so screenshots and casts show the
    /// zoom region as seen (without the pointer sprite).
    fn render_pixels(
        &mut self,
        connector: Option<&str>,
        fourcc: smithay::backend::allocator::Fourcc,
    ) -> Option<(i32, i32, Vec<u8>)> {
        use smithay::backend::renderer::{ExportMem, Offscreen};
        #[cfg(not(feature = "drm"))]
        let _ = connector;
        if self.is_locked() {
            return None;
        }
        let blank_alpha = self.blank_alpha();
        let overview = (self.overview_motion.progress() > 0.0).then(|| self.overview_layout());
        let cards = overview
            .as_ref()
            .filter(|_| !self.overview_motion.search_covers());
        let tile = overview
            .is_none()
            .then(|| self.manager.tile_preview(&self.state))
            .flatten();
        let tile = self.animated_tile_preview(tile);
        let accent = self.wallpaper.accent();
        let decor_global = cards
            .map(|layout| overview_decor(layout, accent))
            .unwrap_or_default();
        let background = if overview.is_some() {
            OVERVIEW_BACKGROUND
        } else {
            self.desktop_color()
        };
        let desktop = self.background_desktop();
        let bell = self.bell_frame();
        let wellbeing = self.wellbeing_effects.sample(self.animation_clock.now());
        let zoom_inputs = self.magnifier_inputs();
        let zoom_now = self.animation_clock.now();
        let (renderer, size, view, output_name) = match &mut self.backend {
            Backend::Winit(backend) => {
                let size = backend.window_size();
                let view = View {
                    offset: (0, 0),
                    scale: self.scale,
                };
                (backend.renderer(), size, view, "tuna-0".to_owned())
            }
            #[cfg(feature = "drm")]
            Backend::Drm(drm) => {
                let crate::drm::DrmBackend {
                    renderer, outputs, ..
                } = &mut **drm;
                let out = match connector {
                    Some(name) => outputs.iter().find(|o| o.name == name)?,
                    None => outputs.first()?,
                };
                let view = View {
                    offset: out.loc,
                    scale: out.scale,
                };
                (renderer, out.size, view, out.name.clone())
            }
        };
        let color_effect = crate::wellbeing_effects::FrameColor::new(renderer, wellbeing, None);
        let buffer_size = (size.w, size.h).into();
        let mut texture: smithay::backend::renderer::gles::GlesTexture =
            renderer.create_buffer(fourcc, buffer_size).ok()?;
        let mut elements = scene_elements(
            renderer,
            &self.manager,
            &self.menu_close,
            &self.state,
            view,
            true,
            overview.as_ref(),
            None,
            tile.as_ref()
                .and_then(|(id, _)| self.manager.surface_of(*id))
                .as_ref(),
        );
        if let Some((_, rect)) = tile {
            elements.tile = tile_elements(rect, view, accent);
        }
        if overview.is_none() {
            elements
                .tile
                .extend(focus_ring_elements(&self.manager, view, None));
        }
        elements.top = previews_to_elements(
            renderer,
            &self.manager,
            view,
            &switcher_previews(&self.manager, &self.switcher_thumbnails),
        );
        elements.top.extend(dnd_icon_elements(
            renderer,
            &self.state,
            view,
            self.manager.pointer_pos(),
        ));
        install_bell(&mut elements, view, bell, desktop);
        let decor = decor_for_output(&decor_global, view);
        let previews = preview_elements(renderer, &self.manager, view, cards);
        let paper = backdrop(
            &mut self.wallpaper,
            renderer,
            background_geometry(size, view, desktop),
            view,
            overview.is_none(),
            cards,
            None,
        );
        let full = Rectangle::from_size(size);
        let zoom_plan = self.magnifier.plan_for(
            crate::magnifier::OutputFrame {
                name: &output_name,
                size: (size.w, size.h),
                scale: view.scale,
                offset: view.offset,
            },
            zoom_inputs,
            accent,
        );
        let zoom_program = zoom_plan
            .as_ref()
            .filter(|plan| !plan.effects.identity)
            .and_then(|_| crate::magnifier::effects_program(renderer));
        // The magnifier's passes (#349): captures and casts carry the
        // zoom region as shown, without the pointer sprite.
        let zoomed = match zoom_plan.as_ref() {
            Some(plan) => {
                let (scene, zoom, _) = zoom_passes(
                    renderer,
                    &mut self.zoom_targets,
                    &mut self.cursor,
                    &output_name,
                    plan,
                    &crate::cursor::Pointer::Hidden,
                    view.scale,
                    zoom_now,
                    &crate::cursor::CursorElements::default(),
                    |frame, damage| {
                        draw_scene(
                            frame,
                            background,
                            &paper,
                            &elements,
                            Target {
                                output: full,
                                damage,
                                scale: view.scale,
                                blank_alpha,
                                tint: color_effect.as_ref(),
                            },
                            &decor,
                            &previews,
                        )
                    },
                )
                .ok()?;
                Some((plan, scene, zoom))
            }
            None => None,
        };
        let mut target = renderer.bind(&mut texture).ok()?;
        {
            let mut frame = renderer.render(&mut target, size, Transform::Normal).ok()?;
            if let Some((plan, scene, zoom)) = zoomed {
                crate::magnifier::present(
                    &mut frame,
                    &scene,
                    &zoom,
                    plan,
                    zoom_program.as_ref(),
                    None,
                )
                .ok()?;
            } else {
                draw_scene(
                    &mut frame,
                    background,
                    &paper,
                    &elements,
                    Target {
                        output: full,
                        damage: &[full],
                        scale: view.scale,
                        blank_alpha,
                        // Captures stay untinted, like GNOME's (gamma
                        // applies past the framebuffer).
                        tint: color_effect.as_ref(),
                    },
                    &decor,
                    &previews,
                )
                .ok()?;
            }
            let _ = frame.finish().ok()?;
        }
        let mapping = renderer
            .copy_framebuffer(&target, Rectangle::from_size(buffer_size), fourcc)
            .ok()?;
        let bytes = renderer.map_texture(&mapping).ok()?.to_vec();
        Some((size.w, size.h, bytes))
    }

    /// The screen magnifier's tracking points this frame (#349), in
    /// global logical pixels: the pointer; the caret from AT-SPI and
    /// from the text-input rectangle (its origin, as GNOME takes the
    /// caret extents' x and y); the focus from AT-SPI (the widget's
    /// center) and the focused window's center. While locked only the
    /// lock surface's own AT-SPI events count, so nothing behind the
    /// lock steers the view.
    fn magnifier_inputs(&mut self) -> crate::magnifier::TrackingInputs {
        let locked = self.is_locked();
        self.focus_caret.sync(
            self.magnifier.wants_focus_events(),
            self.magnifier.wants_caret_events(),
            &self.wake,
        );
        let origin = self.keyboard_focus_origin(locked);
        let take = |seen: &mut (u64, Option<(f64, f64)>),
                    event: Option<crate::focus_caret::Extents>,
                    center: bool| {
            let Some(event) = event else {
                *seen = (0, None);
                return;
            };
            if event.seq == seen.0 {
                return;
            }
            let point = origin.and_then(|((ox, oy), pid, lock)| {
                let ours = u32::try_from(pid).ok() == event.pid;
                (!lock || ours).then(|| {
                    let (x, y, w, h) = event.rect;
                    let (dx, dy) = if center {
                        (f64::from(w) / 2.0, f64::from(h) / 2.0)
                    } else {
                        (0.0, 0.0)
                    };
                    (ox + f64::from(x) + dx, oy + f64::from(y) + dy)
                })
            });
            *seen = (event.seq, point);
        };
        take(&mut self.atspi_points.focus, self.focus_caret.focus(), true);
        take(
            &mut self.atspi_points.caret,
            self.focus_caret.caret(),
            false,
        );
        let text_caret = if locked {
            None
        } else {
            self.state
                .ime_cursor_rect()
                .map(|rect| (f64::from(rect.loc.x), f64::from(rect.loc.y)))
        };
        let window = if locked {
            None
        } else {
            self.manager
                .model()
                .focused()
                .and_then(|id| self.manager.geometry(id))
                .map(|rect| {
                    (
                        f64::from(rect.loc.x) + f64::from(rect.size.w) / 2.0,
                        f64::from(rect.loc.y) + f64::from(rect.size.h) / 2.0,
                    )
                })
        };
        let pointer = self.manager.pointer_pos();
        crate::magnifier::TrackingInputs {
            pointer: (pointer.x, pointer.y),
            caret: [self.atspi_points.caret.1, text_caret],
            focus: [self.atspi_points.focus.1, window],
            cursor: self.cursor.sprite_logical().unwrap_or((0.0, 0.0)),
            now: self.animation_clock.now(),
            motion: self.input_settings.motion,
        }
    }

    /// The keyboard-focused surface's frame origin (what AT-SPI window
    /// coordinates are relative to: Mutter's client content rect is the
    /// frame rect), global logical, with its client's pid and whether
    /// it is the lock surface. Unlocked that is a toplevel window;
    /// locked, only the lock surface. Shell layers and popups map
    /// nothing.
    fn keyboard_focus_origin(&self, locked: bool) -> Option<((f64, f64), i32, bool)> {
        let surface = self.state.seat.get_keyboard()?.current_focus()?;
        let pid = smithay::reexports::wayland_server::Resource::client(&surface)
            .and_then(|client| client.get_credentials(&self.display.handle()).ok())
            .map(|credentials| credentials.pid)?;
        if locked {
            let loc = self
                .state
                .output_entries()
                .into_iter()
                .find_map(|(name, _, loc, _)| {
                    (self.state.lock_surface_for(&name).as_ref() == Some(&surface)).then_some(loc)
                })?;
            return Some(((f64::from(loc.0), f64::from(loc.1)), pid, true));
        }
        let frame = self
            .manager
            .render_entries()
            .into_iter()
            .find_map(|(id, _, geo)| {
                (self.manager.surface_of(id).as_ref() == Some(&surface)).then_some(geo)
            })?;
        Some(((f64::from(frame.loc.x), f64::from(frame.loc.y)), pid, false))
    }

    /// Screen-cast work for this tick (#61): start and stop casts the
    /// D-Bus side asked for, and feed every streaming cast a frame.
    fn screencast_tick(&mut self) {
        use crate::mutter::CastTarget;
        if self.is_locked() {
            for grant in self.cast_grants.values() {
                grant.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            for cast in &self.casts {
                cast.close();
            }
            self.casts.clear();
            self.cast_grants.clear();
            return;
        }
        // A cast window that closed ends its session, as in Mutter.
        let manager = &self.manager;
        self.casts.retain(|cast| {
            let gone =
                matches!(cast.target, CastTarget::Window(id) if manager.geometry(id).is_none());
            if gone || cast.failed() {
                if let Some(grant) = self.cast_grants.get(&cast.session_id) {
                    grant.store(true, std::sync::atomic::Ordering::SeqCst);
                }
                cast.close();
            }
            !cast.failed()
                && !gone
                && !self
                    .cast_grants
                    .get(&cast.session_id)
                    .is_none_or(|grant| grant.load(std::sync::atomic::Ordering::SeqCst))
        });
        // Window casts follow their window's size.
        for index in 0..self.casts.len() {
            if let CastTarget::Window(id) = self.casts[index].target {
                if let Some(((w, h), _)) = self.window_pixel_size(id) {
                    self.casts[index].resize(w, h);
                }
            }
        }
        let wanted: Vec<(usize, CastTarget, (i32, i32))> = self
            .casts
            .iter()
            .enumerate()
            .filter_map(|(i, cast)| {
                cast.wants_frame()
                    .map(|size| (i, cast.target.clone(), size))
            })
            .collect();
        let xrgb = smithay::backend::allocator::Fourcc::Xrgb8888;
        for (index, target, size) in wanted {
            let frame = match &target {
                CastTarget::Window(id) => self.render_window_pixels(*id, Some(size), xrgb),
                CastTarget::Monitor(connector) => {
                    #[cfg(not(feature = "drm"))]
                    let _ = connector;
                    let output = match &self.backend {
                        Backend::Winit(_) => None,
                        #[cfg(feature = "drm")]
                        Backend::Drm(_) => Some(connector.as_str()),
                    };
                    self.render_pixels(output, xrgb)
                }
                CastTarget::Area(connector, area) => {
                    let output = match &self.backend {
                        Backend::Winit(_) => None,
                        #[cfg(feature = "drm")]
                        Backend::Drm(_) => Some(connector.as_str()),
                    };
                    #[cfg(not(feature = "drm"))]
                    let _ = connector;
                    self.render_pixels(output, xrgb).and_then(|(w, h, pixels)| {
                        let cropped = crate::screencast::crop(&pixels, (w, h), *area);
                        if cropped.is_none() {
                            eprintln!(
                                "tuna-compositor: screen cast area {area:?} is not inside the {w}x{h} frame ({} bytes)",
                                pixels.len()
                            );
                        }
                        cropped.map(|cropped| (area.2, area.3, cropped))
                    })
                }
            };
            let Some((w, h, bgrx)) = frame else {
                continue;
            };
            if let Some(cast) = self.casts.get_mut(index) {
                cast.send_frame(w, h, &bgrx);
            }
        }
    }

    /// One D-Bus screen-cast request.
    fn stop_remote_input(&mut self, session_id: u64) {
        let Some(held) = self.remote_held.remove(&session_id) else {
            return;
        };
        self.flush_motion();
        held.grant.store(true, std::sync::atomic::Ordering::SeqCst);
        let time = self.lock_now_ms() as u32;
        // Cleanup must reach the seat even while the lock shield has no
        // surface or idle blank would consume a wake event. These are only
        // releases for presses delivered by this grant, never new input.
        for keycode in held.keys {
            self.manager.on_input(
                &mut self.state,
                ManagerInput::Key {
                    keycode,
                    pressed: false,
                    time,
                },
            );
        }
        for button in held.buttons {
            self.manager.on_input(
                &mut self.state,
                ManagerInput::Button {
                    button,
                    pressed: false,
                    time,
                },
            );
        }
    }

    fn remote_input(
        &mut self,
        session_id: u64,
        grant: std::sync::Arc<std::sync::atomic::AtomicBool>,
        input: crate::mutter::RemoteInput,
    ) {
        if self.is_locked() || grant.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        let time = self.lock_now_ms() as u32;
        let held = self
            .remote_held
            .entry(session_id)
            .or_insert_with(|| RemoteHeld {
                grant,
                keys: Default::default(),
                buttons: Default::default(),
            });
        use crate::mutter::RemoteInput;
        let input = match input {
            RemoteInput::Key { evdev, pressed } => {
                // ManagerInput uses evdev; WindowManager adds the XKB offset
                // when delivering to the seat, exactly as for local input.
                let keycode = evdev;
                if (pressed && !held.keys.insert(keycode))
                    || (!pressed && !held.keys.remove(&keycode))
                {
                    return;
                }
                ManagerInput::Key {
                    keycode,
                    pressed,
                    time,
                }
            }
            RemoteInput::Button { button, pressed } => {
                if (pressed && !held.buttons.insert(button))
                    || (!pressed && !held.buttons.remove(&button))
                {
                    return;
                }
                ManagerInput::Button {
                    button,
                    pressed,
                    time,
                }
            }
            RemoteInput::Relative { dx, dy } => {
                let pos = self.manager.pointer_pos();
                let target = (pos.x + dx, pos.y + dy);
                let clamped = self
                    .cast_outputs
                    .lock()
                    .ok()
                    .and_then(|outputs| {
                        outputs
                            .iter()
                            .map(|out| {
                                let x = target.0.clamp(
                                    f64::from(out.x),
                                    f64::from(out.x) + f64::from(out.width) / out.scale - 1.0,
                                );
                                let y = target.1.clamp(
                                    f64::from(out.y),
                                    f64::from(out.y) + f64::from(out.height) / out.scale - 1.0,
                                );
                                (x, y, (target.0 - x).powi(2) + (target.1 - y).powi(2))
                            })
                            .min_by(|a, b| a.2.total_cmp(&b.2))
                            .map(|(x, y, _)| (x, y))
                    })
                    .unwrap_or((pos.x, pos.y));
                ManagerInput::Motion {
                    pos: clamped.into(),
                    time,
                }
            }
            RemoteInput::Absolute { x, y } => ManagerInput::Motion {
                pos: (x, y).into(),
                time,
            },
            RemoteInput::Axis { dx, dy } => ManagerInput::Axis {
                horizontal: dx,
                vertical: dy,
                stop: crate::windows::AxisStop::NONE,
                time,
            },
        };
        self.on_manager_input(input);
    }

    fn on_screencast_request(&mut self, request: crate::mutter::ToLoop) {
        match request {
            crate::mutter::ToLoop::RemoteInput {
                session_id,
                grant,
                pending,
                input,
            } => {
                pending.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                self.remote_input(session_id, grant, input);
            }
            crate::mutter::ToLoop::RemoteStop { session_id } => self.stop_remote_input(session_id),
            crate::mutter::ToLoop::RemoteKeymap { reply } => {
                let keymap = self
                    .state
                    .seat
                    .get_keyboard()
                    .map(|keyboard| {
                        keyboard.with_xkb_state(&mut self.state, |context| {
                            let xkb = context.xkb().lock().unwrap();
                            // SAFETY: stringify under the Xkb mutex; no keymap reference or clone escapes.
                            unsafe { xkb.keymap() }.get_as_string(1)
                        })
                    })
                    .unwrap_or_default();
                let _ = reply.send(keymap);
            }
            crate::mutter::ToLoop::StartCast {
                session_id,
                grant,
                target,
                signal,
            } => {
                if self.is_locked() || grant.load(std::sync::atomic::Ordering::SeqCst) {
                    grant.store(true, std::sync::atomic::Ordering::SeqCst);
                    signal.session_closed(session_id);
                    return;
                }
                self.cast_grants.insert(session_id, grant);
                if self.pipewire.is_none() {
                    self.pipewire = crate::screencast::PipeWire::new(&self.loop_handle);
                    if self.pipewire.is_none() {
                        eprintln!("tuna-compositor: screen cast: PipeWire is not running");
                        signal.session_closed(session_id);
                        return;
                    }
                }
                let size = match &target {
                    crate::mutter::CastTarget::Monitor(connector) => self
                        .cast_outputs
                        .lock()
                        .ok()
                        .and_then(|o| o.iter().find(|o| &o.connector == connector).cloned())
                        .map(|o| (o.width, o.height)),
                    crate::mutter::CastTarget::Window(id) => {
                        self.window_pixel_size(*id).map(|(size, _)| size)
                    }
                    crate::mutter::CastTarget::Area(_, (_, _, w, h)) => Some((*w, *h)),
                };
                let Some((width, height)) = size else {
                    signal.session_closed(session_id);
                    return;
                };
                let Some(pw) = self.pipewire.as_ref() else {
                    return;
                };
                match pw.start_cast(session_id, target, width, height, signal) {
                    Some(cast) => self.casts.push(cast),
                    None => eprintln!("tuna-compositor: screen cast: stream failed to start"),
                }
            }
            crate::mutter::ToLoop::StopCast { session_id } => {
                self.casts.retain(|cast| cast.session_id != session_id);
                self.cast_grants.remove(&session_id);
            }
            crate::mutter::ToLoop::SensorSample {
                present,
                orientation,
            } => {
                self.orientation.sensor_present = present;
                self.orientation.sensor = orientation;
                self.apply_orientation_policy();
            }
            crate::mutter::ToLoop::ApplyMonitors {
                config,
                method,
                reply,
            } => self.on_apply_monitors(config, method, reply),
            crate::mutter::ToLoop::PowerSave { mode, reply } => {
                self.set_power_save_mode(mode);
                let _ = reply.send(());
            }
        }
    }

    /// Apply a DisplayConfig `PowerSaveMode` write (#355), as Mutter's
    /// monitor manager does: off stops scanout (and on hardware turns
    /// every CRTC off); on repaints every output in full, which modesets
    /// them back.
    fn set_power_save_mode(&mut self, mode: i32) {
        let transition = self.power_save.request(mode);
        self.power_save_share.publish(self.power_save.mode());
        match transition {
            crate::power_save::Transition::Unchanged => return,
            crate::power_save::Transition::PowerOff { disable_outputs } => {
                eprintln!("tuna-compositor: display power save mode {mode}");
                self.power_save_beat = None;
                #[cfg(feature = "drm")]
                if let Backend::Drm(drm) = &mut self.backend {
                    drm.power_off(disable_outputs);
                }
                #[cfg(not(feature = "drm"))]
                let _ = disable_outputs;
            }
            crate::power_save::Transition::PowerOn => {
                eprintln!("tuna-compositor: display power save mode {mode}");
                #[cfg(feature = "drm")]
                if let Backend::Drm(drm) = &mut self.backend {
                    drm.power_on();
                }
                self.winit_tracker = None;
                self.winit_signature = None;
            }
        }
        self.active_at = Instant::now();
    }

    /// Private shell feedback remains inside the currently mapped screenshot
    /// overlay. Recheck the lock and surface after draining the command queue.
    fn move_screenshot_pointer(&mut self, x: i32, y: i32) {
        if self.is_locked() || x < 0 || y < 0 {
            return;
        }
        self.flush_motion();
        let Some(surface) = crate::layer::exclusive_keyboard_layer(&self.state) else {
            return;
        };
        if !self.state.panel_surfaces().into_iter().any(|record| {
            record.surface == surface
                && record.namespace == tuna_shell_control::SCREENSHOT_NAMESPACE
        }) {
            return;
        }
        let Some((_, origin, _)) = crate::layer::layer_layout(&self.state)
            .into_iter()
            .find(|(s, _, _)| *s == surface)
        else {
            return;
        };
        let Some(size) = self
            .state
            .layer_surfaces()
            .into_iter()
            .find(|s| *s.wl_surface() == surface)
            .and_then(|s| s.current_state().size)
        else {
            return;
        };
        if x >= size.w || y >= size.h {
            return;
        }
        let pos = (
            f64::from(origin.0) + f64::from(x),
            f64::from(origin.1) + f64::from(y),
        );
        let time = (crate::state::system_millis() & u64::from(u32::MAX)) as u32;
        self.manager
            .pointer_motion(&mut self.state, pos.into(), time);
        let applied = self.manager.pointer_pos();
        // Winit displays the host cursor; seat state alone leaves it behind.
        match &self.backend {
            Backend::Winit(backend) => {
                let size = backend.window_size();
                let logical = self.state.desktop_size();
                let physical = smithay::reexports::winit::dpi::PhysicalPosition::new(
                    applied.x * f64::from(size.w) / f64::from(logical.0.max(1)),
                    applied.y * f64::from(size.h) / f64::from(logical.1.max(1)),
                );
                if let Err(error) = backend.window().set_cursor_position(physical) {
                    eprintln!("tuna-compositor: screenshot cursor relocation: {error}");
                }
            }
            #[cfg(feature = "drm")]
            Backend::Drm(_) => {}
        }
    }

    /// Apply GNOME's input settings (#60): the seat's keymap and key
    /// repeat, libinput pointer devices (hardware), the hot corner.
    fn apply_input_settings(&mut self, settings: tuna_shell_control::InputSettings) {
        self.wellbeing_effects.motion(settings.motion);
        self.state.protocols.bell.set_policy(settings.bell.clone());
        self.manager
            .apply_workspace_policy(&mut self.state, settings.workspace_policy);
        self.manager
            .set_window_placement(settings.center_new_windows, settings.right_to_left);
        // Release delayed/synthetic input under the OLD map. An unrelated
        // preference change must not reinstall the map or lose held state.
        if aids::keymap_changed(&self.input_settings, &settings) {
            self.reset_input_aid_ownership();
            self.manager
                .apply_keyboard_settings(&mut self.state, &settings);
        } else {
            self.manager.apply_keyboard_repeat(&settings);
        }
        self.triggers.set_hot_corner(settings.hot_corners);
        self.triggers.set_right_to_left(settings.right_to_left);
        self.configure_input_aids(&settings.keyboard_aids, &settings.pointer_aids);
        self.cursor
            .set_theme(&settings.cursor_theme, settings.cursor_size);
        self.cursor.set_locate_pointer(settings.locate_pointer);
        self.cursor.locate.set_key(&settings.locate_pointer_key);
        // Introspect's AnimationsEnabled is GNOME's enable-animations
        // (St): reduced motion keeps it true, as in GNOME 51.
        self.introspect
            .publish_animations_enabled(settings.motion.animations_enabled());
        self.manager
            .set_motion_allowed(settings.motion.allows_motion());
        self.transitions.set_policy(settings.motion);
        self.wallpaper.set_policy(settings.motion);
        if !settings.motion.allows_motion() {
            self.overview_motion.snap(self.control.overview_open());
        }
        #[cfg(feature = "drm")]
        if let Backend::Drm(drm) = &mut self.backend {
            drm.apply_input_settings(&settings);
        }
        // Orientation lock (#345): stop sensor rotation live (and tell
        // the sensor thread to release its claim, as mutter does).
        if self.orientation.lock != settings.orientation_lock {
            self.orientation.lock = settings.orientation_lock;
            self.orientation_share
                .set_inhibited(settings.orientation_lock);
            self.apply_orientation_policy();
        }
        self.input_settings = settings;
    }

    /// Recompute sensor management and rotate unlocked built-in panels
    /// to the accelerometer reading (live only: manual transforms in
    /// monitors.xml are never rewritten). Called on every sensor
    /// sample and every orientation-lock change.
    fn apply_orientation_policy(&mut self) {
        let entries = self.state.output_entries();
        let has_builtin = entries
            .iter()
            .any(|(name, _, _, _)| crate::orientation::is_builtin_panel(name));
        self.orientation_share
            .set_managed(self.orientation.managed(has_builtin));
        #[cfg(feature = "drm")]
        {
            let wants: Vec<(String, u32)> = entries
                .iter()
                .filter(|(name, _, _, _)| crate::orientation::is_builtin_panel(name))
                .map(|(name, _, _, _)| {
                    let manual = self.manual_transforms.get(name).copied().unwrap_or(0);
                    (name.clone(), self.orientation.effective(name, manual))
                })
                .collect();
            if let Backend::Drm(drm) = &mut self.backend {
                for (name, want) in wants {
                    let Some(out) = drm.outputs.iter_mut().find(|o| o.name == name) else {
                        continue;
                    };
                    if out.transform == want {
                        continue;
                    }
                    out.transform = want;
                    out.output.change_current_state(
                        None,
                        Some(crate::orientation::smithay_transform(want)),
                        None,
                        None,
                    );
                    let (lw, lh) = out.logical_size();
                    self.state
                        .add_output(&name, Some(out.output.clone()), lw, lh);
                    eprintln!("tuna-compositor: orientation: {name} rotated to transform {want}");
                }
            }
        }
    }

    /// Each output's current picture (`None`: the nested window), for
    /// the monitor-change fade. Empty while locked.
    fn screen_snapshots(&mut self) -> Vec<(Option<String>, i32, i32, Vec<u8>)> {
        use smithay::backend::allocator::Fourcc;
        let names: Vec<Option<String>> = match &self.backend {
            Backend::Winit(_) => vec![None],
            #[cfg(feature = "drm")]
            Backend::Drm(drm) => drm
                .outputs
                .iter()
                .filter(|o| o.enabled)
                .map(|o| Some(o.name.clone()))
                .collect(),
        };
        names
            .into_iter()
            .filter_map(|name| {
                let (w, h, pixels) = self.render_pixels(name.as_deref(), Fourcc::Xrgb8888)?;
                Some((name, w, h, pixels))
            })
            .collect()
    }

    /// Keep the D-Bus side's monitor list current (cheap when unchanged).
    /// Preview geometry for the shell's preview chrome: GNOME's app
    /// icon, caption and close button (window picker only, not while
    /// search or the app grid covers it).
    fn publish_overview_previews(&mut self) {
        let (previews, hovered) = if self.control.overview_open()
            && self.overview_motion.progress() >= 1.0
            && !self.overview_search
            && !self.overview_app_grid
        {
            // The resting geometry, so the chrome moves once per change
            // rather than every frame of a hover or layout tween.
            let mut scene = self.overview_base();
            match self.overview_dragged() {
                Some((id, _)) => scene.previews.retain(|p| p.id != id),
                None => crate::overview::grow_hovered(&mut scene, self.manager.pointer_pos()),
            }
            let list: Vec<tuna_shell_control::PreviewInfo> = scene
                .previews
                .iter()
                .filter(|p| p.active)
                .map(|p| tuna_shell_control::PreviewInfo {
                    window: p.id,
                    x: p.rect.loc.x,
                    y: p.rect.loc.y,
                    width: p.rect.size.w,
                    height: p.rect.size.h,
                })
                .collect();
            (list, scene.hovered)
        } else {
            (Vec::new(), None)
        };
        self.control.set_overview_previews(previews, hovered);
    }

    fn publish_cast_outputs(&self) {
        self.introspect
            .publish_screen_size(self.state.desktop_size());
        let snapshot: Vec<crate::mutter::OutputSnapshot> = self
            .state
            .output_entries()
            .into_iter()
            .filter_map(|(name, output, loc, primary)| {
                let mode = output.current_mode()?;
                Some(crate::mutter::OutputSnapshot {
                    connector: name,
                    make: output.physical_properties().make,
                    model: output.physical_properties().model,
                    width: mode.size.w,
                    height: mode.size.h,
                    refresh_mhz: mode.refresh,
                    x: loc.0,
                    y: loc.1,
                    scale: output.current_scale().fractional_scale(),
                    primary,
                    transform: crate::orientation::transform_from_smithay(
                        output.current_transform(),
                    ),
                })
            })
            .collect();
        if let Ok(mut current) = self.cast_outputs.lock() {
            if *current != snapshot {
                *current = snapshot;
            }
        }
        // Windows for Introspect (signals only on change).
        let model = self.manager.model();
        let active = model.active_workspace();
        let windows = model
            .windows()
            .filter_map(|entry| {
                let geometry = self.manager.geometry(entry.id)?;
                let owner_id = self.manager.application_window(entry.id)?;
                let owner = model.window(owner_id)?;
                Some(crate::mutter::WindowSnapshot {
                    id: entry.id,
                    title: entry.title.clone(),
                    app_id: entry.app_id.clone(),
                    application_id: Some(crate::introspect::desktop_app_id(
                        owner_id,
                        owner.app_id.as_deref(),
                    )),
                    sandboxed_app_id: if self.manager.is_x11(entry.id) {
                        // The Wayland peer here is Xwayland, not the X11 application.
                        None
                    } else {
                        self.manager
                            .surface_of(entry.id)
                            .and_then(|surface| self.state.authenticated_sandboxed_app_id(&surface))
                    },
                    width: geometry.size.w,
                    height: geometry.size.h,
                    focused: entry.focused,
                    hidden: entry.workspace != active,
                    x11: self.manager.is_x11(entry.id),
                    standalone: self.manager.is_standalone(entry.id),
                    introspect_eligible: self.manager.is_introspect_eligible(entry.id),
                })
            })
            .collect();
        self.introspect.publish(windows);
    }

    /// How long the loop may sleep before the next [`tick`](Self::tick)
    /// (#503). Wayland clients, input, page flips, the control socket,
    /// signals, runtime files and worker threads wake it earlier through
    /// their event sources; this covers what only time moves, and
    /// coalesced motion falling due.
    fn next_wake(&self) -> Duration {
        let wake = self.producer_wake();
        self.motion_wake().map_or(wake, |due| wake.min(due))
    }

    /// The timed producers' earliest wake.
    fn producer_wake(&self) -> Duration {
        #[cfg(feature = "drm")]
        let native = matches!(self.backend, Backend::Drm(_));
        #[cfg(not(feature = "drm"))]
        let native = false;
        // The nested backend has no Wayland readiness source.
        if !native || self.proof_polling {
            return FRAME_BUDGET;
        }
        let now = Instant::now();
        let clock = self.animation_clock.now();
        if now.saturating_duration_since(self.active_at) < ACTIVITY_LINGER
            || self.overview_motion.animating()
            || self.manager.workspace_slide().running()
            || self.manager.strip_animating()
            || self.manager.size_changes().in_flight()
            || self.manager.window_lifecycle().running()
            || self.manager.modal_dim().running()
            || self.cursor.animating(clock)
            || self.magnifier.animating(clock)
            || self.wellbeing_effects.animating(clock)
            || self.screenshot_flashes.running()
            || self.menu_close.running()
            || self.transitions.running()
            || self.wallpaper.fade_stats().active
            || !self.screenshot_replies.is_empty()
            || self.tile_animation.as_ref().is_some_and(|(_, _, start)| {
                clock.saturating_sub(*start).as_secs_f64()
                    < 0.2 * self.input_settings.motion.slowdown()
            })
            || self.state.protocols.bell.visual(now).is_some()
            || self.shell_swipe.is_some()
            || self.overlay.visible
            || !self.casts.is_empty()
            // A running PAM worker publishes its terminal phase before
            // its wake; anything else is collected on the frame pace.
            || self
                .unlock_worker
                .as_ref()
                .is_some_and(|worker| worker.phase.load(std::sync::atomic::Ordering::Acquire) != 1)
        {
            return FRAME_BUDGET;
        }
        // Timed producers are served no more often than the display
        // rate: a gate that is due but cannot act yet (the IME bridge
        // while locked, say) costs what the fixed loop did, never a spin.
        let mut wake = IDLE_WAKE;
        let mut due = |after: Duration| wake = wake.min(after.max(FRAME_BUDGET));
        // An inhibited session neither blanks nor locks; its end is
        // handled by the tick that sees it.
        if !self.idle_inhibited {
            let idle = self.lock.idle_ms(self.lock_now_ms());
            if !self.is_locked() && self.lock.timeout_ms() != u64::MAX {
                due(Duration::from_millis(
                    self.lock.timeout_ms().saturating_sub(idle),
                ));
            }
            if self.blank.idle_ms > 0 {
                if idle < self.blank.idle_ms {
                    due(Duration::from_millis(self.blank.idle_ms - idle));
                } else if idle < self.blank.idle_ms.saturating_add(self.blank.fade_ms) {
                    due(FRAME_BUDGET);
                }
            }
        }
        for deadline in [
            self.idle_monitor.next_deadline(),
            self.orca.next_deadline(),
            self.manager.workspace_lease_deadline(),
            self.wallpaper.maintenance_deadline(),
            // Night Light (#339): exact preview expiry (or its past-due
            // restoring obligation) and the bounded settings re-read.
            self.night_light.next_deadline(),
            self.state.protocols.bell.player_deadline(),
            // GNOME focus policy: the bounded settings re-read, the
            // pointer-rest poll and auto-raise.
            self.wm_preferences.next_deadline(now),
            // Tablet and stylus settings: the bounded re-read.
            self.tablet_settings.next_deadline(now),
            self.manager.focus_deadline(),
            // Window sessions are written 3 s after a change (Mutter).
            self.state.window_sessions().save_deadline(),
            // The lock conversation's shared idle/total deadline.
            self.auth.next_deadline(),
        ]
        .into_iter()
        .flatten()
        {
            due(deadline.saturating_duration_since(now));
        }
        #[cfg(feature = "xwayland")]
        {
            let system_now = crate::state::system_millis();
            if !self.x11_failed {
                if let Some(ms) = self.xwayland.next_delay_ms(system_now) {
                    due(Duration::from_millis(ms));
                }
            }
            if self.state.xwm.is_some() {
                due(self
                    .x11_icon_reader
                    .next_deadline()
                    .saturating_duration_since(now));
            }
        }
        if let Some(ms) = self
            .ime
            .as_ref()
            .filter(|_| !self.is_locked())
            .and_then(|ime| ime.next_wake_ms(crate::state::system_millis()))
        {
            due(Duration::from_millis(ms));
        }
        if self.hidden_waiting {
            due(self.hidden_beat.map_or(Duration::ZERO, |last| {
                (last + crate::occlusion::HIDDEN_FRAME_INTERVAL).saturating_duration_since(now)
            }));
        }
        if self.power_save_waiting() {
            due(self.power_save_beat.map_or(Duration::ZERO, |last| {
                (last + crate::power_save::FRAME_INTERVAL).saturating_duration_since(now)
            }));
        }
        if let Some(ms) = self.shell_retry_ms {
            due(Duration::from_millis(ms));
        }
        // A command frame already buffered is applied on the next pass.
        match self.control.next_poll(self.manager.model()) {
            Some(after) => wake.min(after),
            None => wake,
        }
    }

    fn tick(&mut self) -> Result<bool, RuntimeError> {
        if self.blank_stale || !self.blank_watched {
            self.blank_stale = false;
            if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
                let path = std::path::PathBuf::from(dir).join(crate::wake::IDLE_BLANK_FILE);
                let text = crate::runtime_signal::read(&path).unwrap_or_default();
                self.blank = crate::lock::IdleBlank::parse(&text);
            }
        }
        // Motion folded since the last frame reaches the window manager
        // now, before client requests and this tick's frame.
        self.flush_motion_if_due();
        self.display
            .dispatch_clients(&mut self.state)
            .map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
        crate::grid_trace::event(&self.state, "flush_begin", format_args!("site=pre_render"));
        let flushed = self.display.flush_clients();
        crate::grid_trace::event(
            &self.state,
            "flush_end",
            format_args!("site=pre_render ok={}", flushed.is_ok()),
        );
        flushed.map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
        self.manager.reconcile(&mut self.state);
        crate::grid_trace::stage(&self.state, "reconciled");
        self.state.window_sessions_mut().save_if_due(Instant::now());
        self.focus_policy_tick();
        self.tablet_settings_tick();
        self.poll_input_aids();
        crate::grid_trace::stage(&self.state, "input_aids");
        #[cfg(feature = "xwayland")]
        // DISPLAY may be a reserved idle listener. An icon helper must not
        // connect and accidentally activate XWayland before a real client.
        if let Some(display) = self.x11_display.filter(|_| self.state.xwm.is_some()) {
            let windows = self.manager.x11_icon_identities();
            for (xid, id, raster) in self.x11_icon_reader.poll(display, windows.clone()) {
                if windows.contains(&(xid, id)) {
                    let icon =
                        raster.and_then(|r| self.state.window_icons.cache(format!("x11:{id}"), r));
                    self.manager.model_mut().set_icon(id, icon);
                }
            }
        }
        crate::grid_trace::stage(&self.state, "x11_icons");
        // A client's pointer warp moved the manager's pointer: the
        // drawn cursor follows (#89).
        // Not while coalesced motion is pending: the backend's pointer
        // is ahead of the manager's until the frame delivers it.
        #[cfg(feature = "drm")]
        if let Backend::Drm(drm) = &mut self.backend {
            if !self.motion_queue.pending() {
                drm.set_pointer(self.manager.pointer_pos());
            }
        }
        // Publish the output inventory every tick (multi-monitor): the
        // hub broadcasts on change only, so the steady state costs one
        // short comparison. The inventory is the compositor's tracking
        // handed over as-is — never a parallel database.
        self.control.set_outputs(self.state.output_infos());
        #[allow(unused_mut)]
        let mut native_outputs = Vec::new();
        #[cfg(feature = "drm")]
        if let Backend::Drm(drm) = &self.backend {
            native_outputs = drm.native_outputs();
        }
        self.control.set_native_outputs(native_outputs);
        let pointer = self.manager.pointer_pos();
        let output = self
            .state
            .outputs
            .iter()
            .find(|output| {
                pointer.x >= f64::from(output.loc.0)
                    && pointer.y >= f64::from(output.loc.1)
                    && pointer.x < f64::from(output.loc.0 + output.size.w)
                    && pointer.y < f64::from(output.loc.1 + output.size.h)
            })
            .map(|output| output.name.clone());
        self.control.set_pointer_output(output);
        self.control.set_auth_owner(
            self.shell
                .child_pid()
                .zip(self.shell.child_generation())
                .map(|(pid, generation)| crate::control::AuthOwner { pid, generation }),
        );
        self.publish_on_screen_keyboard();
        // Mutter's seat touch-mode picks the unlock dialog's hint
        // (unlockDialog.js:388-440, #502); nested sessions have none.
        #[cfg(feature = "drm")]
        if let Backend::Drm(drm) = &self.backend {
            self.control
                .set_seat_touch_mode(drm.touch_mode().touch_mode());
        }
        let outcome = self.control.poll(self.manager.model_mut());
        crate::grid_trace::stage(&self.state, "control_poll");
        for workspace in outcome.departed_workspaces.iter().copied() {
            self.manager.keep_departed_workspace_alive(workspace);
        }
        for id in outcome.activated {
            self.manager.focus(&mut self.state, Some(id));
        }
        for (request, allow) in outcome.shortcut_consent {
            if !self.is_locked() {
                self.state.answer_shortcut_consent(request, allow);
            }
        }
        // GNOME Shell's "Keep these display settings?" answer (#340).
        for keep in outcome.display_change {
            self.complete_display_change(keep);
        }
        // The shell's pad OSD (P-IN-05): shown on request, closed by it.
        for (pad, edition_mode) in outcome.pad_osd_requests {
            self.request_pad_osd(pad, edition_mode);
        }
        for pad in outcome.pad_osd_closed {
            self.pad_osd_closed(pad);
        }
        // PanelMenu.Button.vfunc_hide: the hidden button's menu closes
        // without BoxPointer's close.
        if outcome.close_menu_unanimated {
            self.menu_close.close_unanimated(std::time::Instant::now());
        }
        if let Some(request) = self.state.take_shortcut_consent_update() {
            self.control
                .queue_message(tuna_shell_control::Message::ShortcutConsent { request });
        }
        for id in outcome.closed {
            self.manager.close_window(id);
        }
        if let Some(active) = outcome.overview_search {
            self.overview_search = active;
        }
        if let Some(active) = outcome.overview_app_grid {
            self.overview_app_grid = active;
        }
        if let Some(geometries) = outcome.icon_geometries {
            self.manager.set_icon_geometries(
                geometries
                    .into_iter()
                    .map(|g| {
                        (
                            g.window,
                            Rectangle::new((g.x, g.y).into(), (g.width, g.height).into()),
                        )
                    })
                    .collect(),
            );
        }
        if let Some(settings) = outcome.input_settings {
            self.apply_input_settings(settings);
        }
        if let Some(click) = outcome.dwell_click {
            self.set_dwell_click_type(click);
        }
        if let Some((grayscale, fade)) = outcome.wellbeing_effects {
            self.wellbeing_effects.set(
                grayscale,
                fade,
                self.animation_clock.now(),
                self.input_settings.motion,
            );
        }
        if let Some((x, y)) = outcome.screenshot_pointer {
            self.move_screenshot_pointer(x, y);
        }
        if let Some(enabled) = outcome.screen_reader {
            // Only the session serving the KeyboardMonitor runs a reader.
            self.orca
                .set_enabled(enabled && self.a11y_keyboard.is_some());
        }
        let diagnostics_visible = !self.is_locked();
        let reader_changed = self.orca.poll(diagnostics_visible).is_some();
        crate::grid_trace::stage(&self.state, "orca_poll");
        if reader_changed || outcome.screen_reader.is_some() {
            self.control
                .queue_message(tuna_shell_control::Message::ScreenReader {
                    state: self.orca.status(),
                });
        }
        if let Some(list) = outcome.accelerators {
            self.manager.set_accelerator_grabs(list);
            self.manager.trace_media_accelerators(self.is_locked());
        }
        for (window, action) in outcome.window_actions {
            self.manager.window_action(&mut self.state, window, action);
        }
        if let Some(thumbnails) = outcome.switcher_thumbnails {
            // A fade frame landing after the lock engaged draws nothing.
            self.switcher_thumbnails = if self.is_locked() {
                Vec::new()
            } else {
                thumbnails
            };
        }
        if let Some(keys) = outcome.switcher_keys {
            self.manager.set_switcher_keys(keys);
        }
        for backward in outcome.input_source_switches {
            self.switch_input_source_with_aid_reset(backward);
        }
        for input in outcome.osk_inputs {
            self.osk_input(input);
        }
        // Header-bar right clicks: GNOME's window menu, drawn by the shell.
        for (window, x, y) in self.manager.take_menu_requests() {
            self.control
                .queue_message(tuna_shell_control::Message::WindowMenu {
                    window,
                    x,
                    y,
                    maximized: self.manager.is_maximized(window),
                    above: self.manager.is_above(window),
                    sticky: self.manager.is_sticky(window),
                    workspace_left: self.manager.workspace_left_of(window),
                    workspace_right: self.manager.workspace_right_of(window),
                });
        }
        if !self.control.overview_open() {
            self.overview_search = false;
            self.overview_app_grid = false;
        }
        // Administrative lockdown (#357 follow-up): poll the live key
        // on a slow tick without blocking (background read plus drain);
        // the shell maps it onto its idle sender too.
        if self.lockdown_refresh_at.elapsed() >= LOCKDOWN_REFRESH_INTERVAL {
            self.control.refresh_lockdown();
            self.lockdown_refresh_at = Instant::now();
        }
        // GNOME's remembered Num Lock (#346): apply the restore once it
        // lands and save every change, as mutter does.
        if let Some(on) = self.numlock.tick(self.manager.keyboard_num_lock()) {
            self.manager.set_num_lock(&mut self.state, on);
        }
        // GNOME's idle and lock settings, from the shell (#63).
        if let Some(ms) = outcome.idle_timeout {
            self.lock.set_timeout(if ms == 0 { u64::MAX } else { ms });
        }
        // Adopt a control-command lock (manual lock set path): the hub
        // flag flipped without the idle machine, so mirror it locally,
        // dismiss the overview, and raise the exclusive surface.
        if self.control.is_locked() && !self.lock.is_locked() {
            self.engage_lock();
        }
        // ext-session-lock-v1: only the supervised shell may lock, and
        // locking through the protocol engages the compositor's own
        // flag. Anyone else's locker is dropped, which tells it the
        // lock failed.
        if let Some((locker, pid)) = self.state.take_lock_request() {
            let ours = matches!(
                (pid, self.shell.child_pid()),
                (Some(pid), Some(child)) if u32::try_from(pid).ok() == Some(child)
            );
            // Administrative lockdown (#357): a disabled lock screen
            // refuses even our own shell's locker (no Wayland bypass).
            let approved = ours && self.control.lock_screen_allowed();
            if approved {
                if !self.is_locked() {
                    self.engage_lock();
                }
            } else if ours {
                eprintln!("tuna-compositor: session lock refused by administrator policy");
            } else {
                eprintln!("tuna-compositor: session lock refused for pid {pid:?}");
            }
            self.state.resolve_lock_request(locker, approved);
        }
        // A lock client's unlock_and_destroy never unlocks by itself:
        // only a verified password clears the flag.
        if self.state.take_client_unlock() && self.is_locked() {
            eprintln!("tuna-compositor: lock client left while locked; staying locked");
        }
        // A lock-screen password: verified off the loop (PAM may stall
        // for seconds on a failure), one attempt at a time.
        if let Some((origin, password)) = outcome.unlock {
            let password = crate::pam_backend::Credential::from_schema(password);
            let epoch = self
                .unlock_generation
                .load(std::sync::atomic::Ordering::Acquire);
            let attempt = self
                .is_locked()
                .then(|| self.unlock_slot.begin(Some(origin), epoch))
                .flatten();
            if let Some(attempt) = attempt {
                let cancel = crate::pam_cancel::Cancellation::default();
                let backend_cancel = cancel.clone();
                self.start_unlock_worker(attempt, cancel, false, move |pending| {
                    match crate::unlock::session_user() {
                        Some(user) => crate::unlock::verify_owned_credential(
                            &user,
                            password,
                            backend_cancel,
                            pending,
                        ),
                        None => crate::pam_backend::Verification::RejectedBeforeSpawn,
                    }
                });
            } else {
                self.control.finish_unlock(origin, false);
            }
        }
        // The lock screen's conversations (GDM's UserVerifier role): begun,
        // answered and cancelled here; advanced after the gates below.
        for (origin, service) in outcome.auth_begin {
            self.begin_verification(origin, service);
        }
        for (connection, service, sequence, answer) in outcome.auth_answers {
            self.answer_query(connection, service, sequence, answer);
        }
        for connection in outcome.auth_cancel {
            self.cancel_verification(connection);
        }
        crate::grid_trace::stage(&self.state, "control_outcome");
        self.idle_monitor.tick();
        // Idle timeout from input timestamps: lock the untouched session.
        // A live idle inhibitor (video, presentation) counts as activity,
        // and so does its end: the loop may have slept through the
        // inhibited stretch, so the idle clock restarts when it ends
        // instead of the session locking at once (#503). An idle
        // inhibitor held through org.gnome.SessionManager (a portal or
        // org.freedesktop.ScreenSaver caller) counts the same way.
        let inhibited = self.state.idle_inhibited() || self.session_manager.idle_inhibited();
        if self
            .lock
            .inhibitor_activity(self.idle_inhibited, inhibited, self.lock_now_ms())
        {
            self.idle_since = Instant::now();
        }
        self.idle_inhibited = inhibited;
        if !self.is_locked() && self.lock.check_timeout(self.lock_now_ms()) {
            // Administrative lockdown (#357): a disabled lock screen
            // never engages on idle; restart the stretch instead so no
            // surprise lock fires when the policy lifts.
            if self.control.lock_screen_allowed() {
                self.engage_lock();
            } else {
                self.lock.unlock(self.lock_now_ms());
            }
        }
        // XIM is an X11 client itself: keep it off merely advertised sockets,
        // and retire the helper on server loss even while the session is locked.
        #[cfg(feature = "xwayland")]
        let ready_x11_display = self.x11_display.filter(|_| self.state.xwm.is_some());
        #[cfg(not(feature = "xwayland"))]
        let ready_x11_display = None;
        if let Some(ime) = &mut self.ime {
            ime.set_ready_x11_display(ready_x11_display);
        }
        // Overview focus follows the hub flag (shell commands and
        // runtime triggers converge here); the next reconcile parks
        // or restores keyboard focus.
        self.manager.set_overview_open(self.control.overview_open());
        // Reap and restart the supervised lock UI even while locked.
        // Supervision is nonblocking and cannot clear the lock flag.
        let shell_status = self.shell.poll(crate::state::system_millis());
        self.shell_retry_ms = match shell_status {
            ShellStatus::Waiting { delay_ms } => Some(delay_ms),
            _ => None,
        };
        crate::grid_trace::stage(&self.state, "shell_poll");
        // While locked the overlay stays up with its empty list no
        // matter what the shell does: a shell restart while locked
        // keeps the lock screen up.
        if self.is_locked() {
            if !self.overlay.visible {
                self.overlay.show(Vec::new());
            }
        } else {
            if let Some(ime) = &mut self.ime {
                ime.poll(crate::state::system_millis(), &mut self.display.handle());
            }
            match shell_status {
                ShellStatus::Running => {
                    if self.overlay.visible {
                        self.overlay.hide();
                    }
                }
                ShellStatus::Waiting { .. } | ShellStatus::Fault(_) | ShellStatus::Exhausted => {
                    self.refresh_overlay()
                }
            }
        }
        crate::grid_trace::stage(&self.state, "ime_overlay");
        // Only the supervised shell may hold a control session (#30):
        // between restarts nobody may.
        self.capture_authority
            .publish(self.is_locked(), self.shell.child_pid());
        self.control.set_auth_owner(
            self.shell
                .child_pid()
                .zip(self.shell.child_generation())
                .map(|(pid, generation)| crate::control::AuthOwner { pid, generation }),
        );
        self.control.set_peer_gate(match self.shell.child_pid() {
            Some(pid) => PeerGate::Pid(pid),
            None => PeerGate::Closed,
        });
        // Control EOF, manual/protocol/idle gates and shell PID gating are now current.
        // Never grant from a channel callback before those changes are observed.
        self.cancel_invalid_unlock();
        let completed = self
            .unlock_worker
            .as_ref()
            .and_then(|worker| worker.worker.is_finished().then_some(worker.attempt));
        if let Some(attempt) = completed {
            self.finish_unlock(attempt);
        }
        self.poll_conversations();
        crate::grid_trace::stage(&self.state, "conversations");

        for event in self.shell.drain_events() {
            eprintln!("tuna-compositor: shell supervision: {event:?}");
        }
        // XWayland supervision rides the same tick. Without the
        // feature there is no spawner and this stays a no-op;
        // native sessions never leave `Idle`.
        #[cfg(not(feature = "xwayland"))]
        self.xwayland.tick(crate::state::system_millis(), None);
        #[cfg(feature = "xwayland")]
        {
            let display = self.display.handle();
            let loop_handle = self.loop_handle.clone();
            let now = crate::state::system_millis();
            if self.x11_failed
                || self
                    .active_x11_client
                    .as_ref()
                    .is_some_and(|client| client.get_credentials(&display).is_err())
            {
                if let Some(token) = self.x11_source.take() {
                    loop_handle.remove(token);
                }
                self.active_x11_client = None;
                self.x11_failed = false;
                self.pending_x11_client = None;
                if self.state.xwm.take().is_some() {
                    crate::session_services::x11_display_closed();
                }
                if let Some(ime) = &mut self.ime {
                    ime.set_ready_x11_display(None);
                }
                self.manager.clear_x11_windows(&mut self.state);
                self.xwayland.disconnected(now);
            }
            let pending = &mut self.pending_x11_client;
            let reserved = &mut self.x11_sockets;
            let watchers = &mut self.x11_watchers;
            let source = &mut self.x11_source;
            let client = &mut self.active_x11_client;
            let advertised = self.x11_display;
            let mut spawner = || {
                for token in watchers.drain(..) {
                    loop_handle.remove(token);
                }
                let sockets = match reserved.take() {
                    Some(sockets) => sockets,
                    None => smithay::xwayland::XWaylandSockets::prepare(advertised, true)
                        .map_err(|_| crate::xwayland::AbsentReason::SpawnFailed)?,
                };
                crate::xwayland::spawn_xwayland(
                    &display,
                    &loop_handle,
                    pending,
                    sockets,
                    source,
                    client,
                )
            };
            self.xwayland.tick(now, Some(&mut spawner));
        }
        crate::grid_trace::stage(&self.state, "xwayland");
        self.stats.shell_restarts = self.shell.restarts_used();
        self.proof_swipe_input();
        self.animation_clock.poll();
        crate::grid_trace::stage(&self.state, "animation_clock");
        // GNOME completes a flashing screenshot reply when its feedback ends.
        // Locking or disabling animations also ends pending feedback promptly.
        let now = self.animation_clock.now();
        let ended = self.is_locked() || !self.input_settings.motion.allows_fades();
        let mut index = 0;
        while index < self.screenshot_replies.len() {
            if ended || now >= self.screenshot_replies[index].deadline {
                let pending = self.screenshot_replies.swap_remove(index);
                let _ = pending.reply.send(pending.saved);
            } else {
                index += 1;
            }
        }
        self.manager.reconcile_dynamic_workspaces(
            self.overview_drag.is_some(),
            now,
            self.state.primary_size().w,
            self.input_settings.motion,
        );
        crate::grid_trace::stage(&self.state, "dynamic_workspaces");
        self.publish_state();
        crate::grid_trace::stage(&self.state, "publish_state");
        self.publish_cast_outputs();
        crate::grid_trace::stage(&self.state, "publish_cast_outputs");
        #[cfg(feature = "drm")]
        self.privacy_screen_tick();
        self.publish_display_state();
        crate::grid_trace::stage(&self.state, "publish_display_state");
        self.publish_overview_previews();
        crate::grid_trace::stage(&self.state, "publish_overview_previews");
        let strip_was_animating = self.manager.strip_animating();
        self.render()?;
        // Rendering advances the strip's springs. Publish the terminal frame
        // before the motion scheduler goes idle; the pre-render snapshot can
        // otherwise retain the previous frame's residual indefinitely. With
        // motion disabled, publish the instant frame's actual render count too.
        let strip_lands_immediately = self.manager.session_mode()
            == crate::windows::SessionMode::Scroll
            && !self.input_settings.motion.allows_motion();
        if (strip_was_animating || strip_lands_immediately) && !self.manager.strip_animating() {
            self.publish_state();
        }
        self.screencast_tick();
        // Send what this tick queued for the shell now, not at the next
        // wake, which may be far off (#503).
        self.control.flush(self.manager.model());
        #[cfg(feature = "drm")]
        if matches!(self.backend, Backend::Drm(_)) {
            // Rendering queues callbacks only for a submitted scanout. Send
            // those events now so a redraw need not wait for the next tick.
            crate::grid_trace::event(
                &self.state,
                "flush_begin",
                format_args!("site=post_render_drm"),
            );
            let flushed = self.display.flush_clients();
            crate::grid_trace::event(
                &self.state,
                "flush_end",
                format_args!("site=post_render_drm ok={}", flushed.is_ok()),
            );
            flushed.map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
        }
        if matches!(self.backend, Backend::Winit(_)) {
            crate::grid_trace::event(
                &self.state,
                "tick_end",
                format_args!("backend=nested post_render_flush=false"),
            );
        }
        Ok(!self.exit)
    }

    /// A page flip completed (hardware): mark what the frame drew
    /// presented with the kernel's vblank time and sequence, and on the
    /// primary output advance fifo barriers and commit timers (#89).
    #[cfg(feature = "drm")]
    fn page_flipped(&mut self, flip: crate::drm::PageFlip) {
        use smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback::Kind;
        use smithay::utils::{Monotonic, Time};
        // Mutter's KMS flags: vsync'd, kernel-timestamped, completion
        // reported by the hardware.
        let (time, flags): (Time<Monotonic>, Kind) = match flip.time {
            Some(time) => (
                Time::from(time),
                Kind::Vsync | Kind::HwClock | Kind::HwCompletion,
            ),
            None => (
                self.state.presentation_now(),
                Kind::Vsync | Kind::HwCompletion,
            ),
        };
        if let Some(mut feedback) = flip.feedback {
            feedback.presented::<_, Monotonic>(
                time,
                smithay::wayland::presentation::Refresh::fixed(flip.refresh),
                flip.sequence,
                flags,
            );
        }
        if flip.primary {
            self.performance_trace.presented(flip.time, flip.sequence);
            let roots = crate::frame_timing::frame_roots(&self.state, &self.manager);
            self.state.refresh_cycle(&roots, time, flip.refresh);
        }
    }

    /// GNOME's tablet and stylus settings (P-IN-05): hand a completed
    /// read to the hardware tablets, which apply what changed.
    fn tablet_settings_tick(&mut self) {
        let now = Instant::now();
        let snapshot = self.tablet_settings.poll(now);
        let status = self.tablet_settings.status();
        if self.tablet_settings_status != Some(status) {
            if status == crate::night_light_settings::CacheStatus::Quarantined {
                eprintln!(
                    "tuna-compositor: tablet settings cache {status:?}; retaining the applied settings"
                );
            }
            self.tablet_settings_status = Some(status);
        }
        #[cfg(feature = "drm")]
        if let (Some(snapshot), Backend::Drm(drm)) = (snapshot, &mut self.backend) {
            drm.set_tablet_settings(snapshot);
            self.pad_osd_tick();
        }
    }

    /// One tablet event (P-IN-05): activity like any input; the action
    /// mappers' effects are the compositor's own; while locked only the
    /// lock surface hears tools; a waking blank takes the event.
    fn on_tablet_input(&mut self, event: crate::tablet::TabletEvent) {
        use crate::tablet::TabletEvent;
        let mut waking_blank = false;
        self.flush_motion();
        if let Some(time) = event.time() {
            waking_blank = self.blank_alpha() > 0.0;
            self.lock.note_input(u64::from(time));
            self.idle_since = Instant::now();
            self.idle_monitor.activity();
            self.active_at = Instant::now();
        }
        let event = match event {
            TabletEvent::EmulateKeybinding {
                accel,
                pressed,
                time,
            } => {
                self.emulate_keybinding(&accel, pressed, time);
                return;
            }
            TabletEvent::SetOutput { path, output } => {
                self.tablet_settings.set_output(path, output);
                return;
            }
            TabletEvent::ShowOsd {
                connector,
                icon,
                label,
            } => {
                self.control
                    .queue_message(tuna_shell_control::Message::ShowOsd {
                        connector,
                        icon: icon.into(),
                        label,
                    });
                return;
            }
            TabletEvent::RequestPadOsd { pad, .. } => {
                self.request_pad_osd(pad, false);
                return;
            }
            TabletEvent::PadOsdInput { pad, event, .. } => {
                self.control
                    .queue_message(tuna_shell_control::Message::PadOsdInput { pad, event });
                return;
            }
            TabletEvent::ClosePadOsd { pad } => {
                self.pad_osd_labels_sent = None;
                self.control
                    .queue_message(tuna_shell_control::Message::ClosePadOsd { pad });
                return;
            }
            event => event,
        };
        if waking_blank && !matches!(event, TabletEvent::ProximityOut { .. }) {
            return;
        }
        if self.is_locked() {
            let surface = self
                .state
                .primary_output_name()
                .and_then(|name| self.state.lock_surface_for(&name));
            let hotplug = event.time().is_none();
            match surface {
                Some(surface) => self
                    .manager
                    .tablet_input(&mut self.state, event, Some(&surface)),
                // No lock surface: devices still come and go, nothing else lands.
                None if hotplug => self.manager.tablet_input(&mut self.state, event, None),
                None => {}
            }
            return;
        }
        self.manager.tablet_input(&mut self.state, event, None);
    }

    /// `meta_tablet_action_mapper_emulate_keybinding`: the accelerator's
    /// modifiers and key on the tablet's virtual keyboard, through the
    /// same path as a physical key (shortcuts first, then the focus).
    fn emulate_keybinding(&mut self, accel: &str, pressed: bool, time: u32) {
        let Some(steps) = crate::tablet_actions::keybinding_steps(accel, pressed) else {
            eprintln!("tuna-compositor: tablet: \"{accel}\" is not a valid accelerator");
            return;
        };
        eprintln!(
            "tuna-compositor: tablet: keybinding {accel} {}",
            if pressed { "press" } else { "release" }
        );
        for step in steps {
            self.notify_keyval(step.keysym, step.pressed, time);
        }
    }

    /// `notify_keyval_in_impl`: the key typing `keysym` in the current
    /// layout, with Shift or Level 3 held around it for a higher level;
    /// a duplicate press or unmatched release is ignored.
    fn notify_keyval(&mut self, keysym: u32, pressed: bool, time: u32) {
        let Some((keycode, level)) = self.manager.keycode_for_keysym(&mut self.state, keysym)
        else {
            eprintln!(
                "tuna-compositor: tablet: no keycode found for keyval {keysym:x} in current group"
            );
            return;
        };
        let admitted = if pressed {
            self.tablet_virtual_keys.insert(keycode)
        } else {
            self.tablet_virtual_keys.remove(&keycode)
        };
        if !admitted {
            eprintln!("tuna-compositor: tablet: ignoring repeated virtual key {keycode:#x}");
            return;
        }
        let level_key = crate::tablet_actions::level_modifier(level)
            .and_then(|modifier| self.manager.keycode_for_keysym(&mut self.state, modifier))
            .map(|(keycode, _)| keycode);
        let key = |rt: &mut Self, keycode: u32, pressed: bool| {
            rt.on_manager_input(ManagerInput::Key {
                keycode,
                pressed,
                time,
            });
        };
        if let (true, Some(modifier)) = (pressed, level_key) {
            key(self, modifier, true);
        }
        key(self, keycode, pressed);
        if let (false, Some(modifier)) = (pressed, level_key) {
            key(self, modifier, false);
        }
    }

    /// `meta_display_request_pad_osd`: hand the shell the pad's OSD. Like
    /// Mutter, nothing happens while one is shown or without a layout.
    fn request_pad_osd(&mut self, pad: u32, edition_mode: bool) {
        #[cfg(feature = "drm")]
        {
            if !self.control.pad_osd_capable() {
                return;
            }
            let Backend::Drm(drm) = &mut self.backend else {
                return;
            };
            let Some((desc, mut osd)) = drm.tablets().pad_osd(pad, edition_mode) else {
                return;
            };
            drm.tablets().set_pad_osd_shown(Some(pad));
            self.fill_pad_feedback(&desc, &mut osd.labels);
            self.pad_osd_labels_sent = Some(osd.labels.clone());
            eprintln!(
                "tuna-compositor: tablet: pad OSD for {} ({}) edition={edition_mode}",
                osd.name, osd.layout_path
            );
            self.control
                .queue_message(tuna_shell_control::Message::ShowPadOsd(Box::new(osd)));
        }
        #[cfg(not(feature = "drm"))]
        let _ = (pad, edition_mode);
    }

    /// The shell closed the pad OSD: pad events go back to clients.
    fn pad_osd_closed(&mut self, pad: u32) {
        #[cfg(feature = "drm")]
        if let Backend::Drm(drm) = &mut self.backend {
            if drm.tablets().pad_osd_shown() == Some(pad) {
                drm.tablets().set_pad_osd_shown(None);
                self.pad_osd_labels_sent = None;
                eprintln!("tuna-compositor: tablet: pad OSD closed");
            }
        }
        #[cfg(not(feature = "drm"))]
        let _ = pad;
    }

    /// Labels no action gives come from the focused client's
    /// `set_feedback` (`meta_display_get_pad_button_label`'s second half).
    fn fill_pad_feedback(
        &self,
        desc: &smithay::wayland::tablet_manager::TabletPadDescriptor,
        labels: &mut tuna_shell_control::PadOsdLabels,
    ) {
        use smithay::wayland::tablet_manager::TabletSeatTrait;
        let Some(handle) = self.state.seat.tablet_seat().get_pad(desc) else {
            return;
        };
        for (button, slot) in labels.buttons.iter_mut().enumerate() {
            if slot.label.is_none() {
                slot.label = handle.button_label(button as u32);
            }
        }
        for (ring, pair) in labels.rings.iter_mut().enumerate() {
            for label in pair.iter_mut().filter(|l| l.is_none()) {
                *label = handle.ring_label(ring as u32);
            }
        }
        for (strip, pair) in labels.strips.iter_mut().enumerate() {
            for label in pair.iter_mut().filter(|l| l.is_none()) {
                *label = handle.strip_label(strip as u32);
            }
        }
    }

    /// After each tablet dispatch and settings read: the shell's pad
    /// targets, and the shown OSD's labels when a mode or setting changed
    /// them. A shell that went away takes its OSD with it.
    fn pad_osd_tick(&mut self) {
        #[cfg(feature = "drm")]
        {
            let Backend::Drm(drm) = &mut self.backend else {
                return;
            };
            self.control.set_pad_targets(drm.tablets().pad_targets());
            let Some(pad) = drm.tablets().pad_osd_shown() else {
                return;
            };
            if !self.control.pad_osd_capable() {
                drm.tablets().set_pad_osd_shown(None);
                self.pad_osd_labels_sent = None;
                return;
            }
            let Some((desc, mut labels)) = drm.tablets().pad_osd_labels(pad) else {
                return;
            };
            self.fill_pad_feedback(&desc, &mut labels);
            if self.pad_osd_labels_sent.as_ref() != Some(&labels) {
                self.pad_osd_labels_sent = Some(labels.clone());
                self.control
                    .queue_message(tuna_shell_control::Message::PadOsdLabels { pad, labels });
            }
        }
    }

    /// GNOME's focus policy (P-WM-09): install a completed settings read,
    /// then repick the pointer window (a crossing without motion) and run
    /// the pointer-rest and auto-raise timers. Native reads stay on the
    /// owned GIO worker; a missing schema keeps GNOME 51's defaults.
    fn focus_policy_tick(&mut self) {
        let now = Instant::now();
        let raw = self.wm_preferences.poll(now);
        let status = self.wm_preferences.status();
        if self.wm_preferences_status != Some(status) {
            if matches!(
                status,
                crate::night_light_settings::CacheStatus::Unavailable
                    | crate::night_light_settings::CacheStatus::Quarantined
            ) {
                eprintln!(
                    "tuna-compositor: wm preferences cache {status:?}; retaining the prior focus policy"
                );
            }
            self.wm_preferences_status = Some(status);
        }
        if let Some(raw) = raw {
            let mut policy = self.manager.focus_policy();
            policy.apply(&raw);
            self.manager.set_focus_policy(policy);
        }
        if !self.is_locked() {
            self.manager.repick_pointer_window(&mut self.state);
            self.manager.run_focus_timers(&mut self.state, now);
        }
    }

    /// Consume the bounded settings cache and evaluate the schedule for
    /// this frame (#339). Native reads stay on the owned GIO worker.
    /// Every wall-clock outcome still consumes monotonic preview expiry.
    fn night_light_tick(&mut self) -> Option<crate::night_light::ColorGains> {
        if self.night_light.poll_settings(Instant::now()) {
            self.manager
                .model_mut()
                .set_night_light(self.night_light.config().clone());
        }
        self.night_light
            .tick_at_system_time(SystemTime::now(), Instant::now())
    }

    /// Render all mapped toplevels stacked at the origin, then send frame
    /// callbacks. While the recovery overlay is visible the background
    /// shifts to a deep red (provisional overlay visual; full overlay
    /// text rendering is deferred) so the shell-absent state is
    /// unmistakable. While locked nothing beneath the lock surface may
    /// show — no windows, no layer-shell chrome (panel, notifications),
    /// no wallpaper — over a dark lock background.
    fn render(&mut self) -> Result<(), RuntimeError> {
        crate::grid_trace::event(&self.state, "render_begin", format_args!(""));
        let locked = self.is_locked();
        #[cfg(feature = "drm")]
        if locked {
            self.performance_trace.cancel();
        }
        let blank_alpha = self.blank_alpha();
        let night_gains = self.night_light_tick();
        let show_content = content_visible(locked);
        let lock_ready = locked
            && self
                .state
                .primary_output_name()
                .and_then(|name| self.state.lock_surface_for(&name))
                .is_some();
        let shell_ready = !crate::layer::layer_layout(&self.state).is_empty();
        // Animation time, so proofs can sample transitions at exact delays.
        let now = self.animation_clock.now();
        if self.animation_clock.is_manual() {
            self.transitions.skip_startup();
        }
        self.wallpaper.set_time(now);
        self.transitions
            .advance(now, locked, lock_ready, shell_ready);
        // While locked the curtain carries the lock surface and its
        // background; it never uncovers content (`show_content`).
        let lock_curtain = self.transitions.curtain().filter(|_| locked);
        let overlay_visible = self.overlay.visible;
        let background = if locked {
            // GNOME's lock screen dims the desktop to 65%; without a
            // picture that is the dimmed primary-color.
            let c = self.desktop_color();
            let k = tuna_wallpaper::LOCK_BRIGHTNESS;
            Color32F::new(c.r() * k, c.g() * k, c.b() * k, 1.0)
        } else if overlay_visible {
            Color32F::new(0.20, 0.08, 0.10, 1.0)
        } else {
            self.desktop_color()
        };
        let drawn = self.step_overview_transition();
        self.step_strip_view();
        // The slide first: size changes read this frame's slide offsets.
        self.step_workspace_slide();
        self.step_size_changes();
        let now = self.animation_clock.now();
        if locked || !self.input_settings.motion.allows_fades() {
            self.screenshot_flashes.clear();
        }
        self.screenshot_flashes.expire(now);
        let flashes = self.screenshot_flashes.samples(now);
        let allowed = crate::window_lifecycle::effects_allowed(
            self.control.overview_open(),
            self.shell_swipe.is_some(),
            self.manager.workspace_slide().running(),
            locked,
        );
        let wellbeing = self.wellbeing_effects.sample(self.animation_clock.now());
        // Hardware outputs take Night Light and the assigned profile's
        // calibration as the CRTC white point (P-DS-03); the renderer
        // tints only outputs without one.
        let tint_needed = self.update_white_points(self.night_light.applied_kelvin());
        let renderer = match &mut self.backend {
            Backend::Winit(backend) => backend.renderer(),
            #[cfg(feature = "drm")]
            Backend::Drm(drm) => &mut drm.renderer,
        };
        self.manager.step_modal_dim(
            now,
            self.input_settings.motion,
            allowed,
            self.input_settings.attach_modal_dialogs,
        );
        crate::window_lifecycle::step_frame(
            &mut self.manager,
            renderer,
            &self.state,
            now,
            self.input_settings.motion,
            allowed,
            self.input_settings.right_to_left,
        );
        self.menu_close.step(
            renderer,
            &self.state,
            now,
            self.input_settings.motion,
            allowed,
        );
        // The tint program is compiled once per renderer and cached;
        // a compile failure draws this frame untinted.
        let night_tint = crate::wellbeing_effects::FrameColor::new(
            renderer,
            wellbeing,
            night_gains.filter(|_| tint_needed),
        );
        // Outputs whose CRTC carries the white point draw untinted.
        #[cfg(feature = "drm")]
        let plain_tint = match night_gains {
            Some(_) => crate::wellbeing_effects::FrameColor::new(renderer, wellbeing, None),
            None => night_tint.clone(),
        };
        let overview = (show_content && !overlay_visible && drawn).then(|| self.overview_layout());
        // While search shows results, the workspace view steps aside.
        let cards = overview
            .as_ref()
            .filter(|_| !self.overview_motion.search_covers());
        // GNOME's tile preview while a dragged window is over a snap edge.
        let tile = (show_content && overview.is_none())
            .then(|| self.manager.tile_preview(&self.state))
            .flatten();
        let tile = self.animated_tile_preview(tile);
        let accent = self.wallpaper.accent();
        let decor_global = cards
            .map(|layout| overview_decor(layout, accent))
            .unwrap_or_default();
        let background = if overview.is_some() {
            OVERVIEW_BACKGROUND
        } else {
            background
        };
        let show_paper = show_content && !overlay_visible && overview.is_none();
        #[cfg(feature = "drm")]
        let overview_requested = self.control.overview_open();
        #[cfg(feature = "drm")]
        let timing_roots = {
            let mut roots = crate::frame_timing::frame_roots(&self.state, &self.manager);
            // Cursor refresh obligations are output-local: only visible cursor
            // geometry below joins that output's drawn/timing roots.
            if let smithay::input::pointer::CursorImageStatus::Surface(surface) =
                &self.state.cursor_image
            {
                roots.retain(|root| root != surface);
            }
            roots
        };
        let desktop = self.background_desktop();
        let bell = self.bell_frame();
        // Window clones (overview, switcher) and casts show windows the
        // scene hides: their frame callbacks keep full rate then.
        let clones_shown =
            overview.is_some() || !self.switcher_thumbnails.is_empty() || !self.casts.is_empty();
        // The pointer is the compositor's to draw (#342): hidden while
        // locked or blanked, as the software pointer always was.
        let cursor_shown = !locked && blank_alpha < 1.0;
        // A tablet tool in proximity draws its own cursor where it is, and
        // the pointer's is hidden meanwhile (Mutter's tool sprite).
        let tablet_cursor = self
            .state
            .tablet_cursor
            .as_ref()
            .filter(|_| cursor_shown)
            .map(|(tool, pos)| {
                let image = self
                    .state
                    .tablet_cursor_images
                    .get(tool)
                    .cloned()
                    .unwrap_or_else(smithay::input::pointer::CursorImageStatus::default_named);
                (crate::cursor::Pointer::resolve(&image, None, true), *pos)
            });
        let cursor_pos_override = tablet_cursor.as_ref().map(|(_, pos)| *pos);
        let cursor_what = if let Some((what, _)) = tablet_cursor {
            what
        } else if cursor_shown {
            crate::cursor::Pointer::resolve(
                &self.state.cursor_image,
                self.manager.grab_cursor(),
                self.state.pointer_focused(),
            )
        } else {
            crate::cursor::Pointer::Hidden
        };
        let cursor_now = self.animation_clock.now();
        // Hidden windows' frame callbacks wait for the hidden beat; by
        // themselves they never make an unchanged output repaint.
        #[cfg(feature = "drm")]
        let throttled = throttled_surfaces(
            &self.state,
            &hidden_surfaces(&self.manager, &self.frame_costs, clones_shown),
        );
        // DisplayConfig PowerSaveMode (#355): the scene advanced above as
        // Mutter's stage does, but nothing is scanned out; frames complete
        // on the slow dummy page flip instead.
        if !self.power_save.is_on() {
            #[cfg(feature = "drm")]
            self.performance_trace.cancel();
            self.power_save_frame_beat();
            crate::grid_trace::event(&self.state, "render_end", format_args!("reason=power_save"));
            return Ok(());
        }
        // Screen magnifier (#349): this frame's tracking points; the
        // offscreen textures go away while it is off.
        let zoom_inputs = self.magnifier_inputs();
        if !self.magnifier.config().active() {
            self.zoom_targets.clear();
        }
        let queued = match &mut self.backend {
            Backend::Winit(backend) => {
                crate::grid_trace::event(&self.state, "nested_repaint_begin", format_args!(""));
                backend.window().set_cursor_visible(false);
                let size = backend.window_size();
                let age = backend.buffer_age().unwrap_or(0);
                let mut presented: PresentedFrame = None;
                {
                    crate::grid_trace::event(&self.state, "nested_bind_begin", format_args!(""));
                    let bound = backend.bind();
                    crate::grid_trace::event(
                        &self.state,
                        "nested_bind_end",
                        format_args!("ok={}", bound.is_ok()),
                    );
                    let (renderer, mut framebuffer) =
                        bound.map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
                    let view = View {
                        offset: (0, 0),
                        scale: self.scale,
                    };
                    let lock_surface = locked
                        .then(|| self.state.primary_output_name())
                        .flatten()
                        .and_then(|name| self.state.lock_surface_for(&name));
                    let mut elements = scene_elements(
                        renderer,
                        &self.manager,
                        &self.menu_close,
                        &self.state,
                        view,
                        show_content,
                        overview.as_ref(),
                        lock_surface
                            .as_ref()
                            .zip(lock_curtain)
                            .map(|(surface, curtain)| (surface, curtain, size.h)),
                        tile.as_ref()
                            .and_then(|(id, _)| self.manager.surface_of(*id))
                            .as_ref(),
                    );
                    if let Some((_, rect)) = tile {
                        elements.tile = tile_elements(rect, view, accent);
                    }
                    if overview.is_none() && show_content {
                        elements
                            .tile
                            .extend(focus_ring_elements(&self.manager, view, None));
                    }
                    elements.top = previews_to_elements(
                        renderer,
                        &self.manager,
                        view,
                        &switcher_previews(&self.manager, &self.switcher_thumbnails),
                    );
                    elements.top.extend(dnd_icon_elements(
                        renderer,
                        &self.state,
                        view,
                        self.manager.pointer_pos(),
                    ));
                    elements.flashes = screenshot_flash_elements(&flashes, view);
                    install_bell(&mut elements, view, bell, desktop);
                    let decor = decor_for_output(&decor_global, view);
                    let previews = preview_elements(renderer, &self.manager, view, cards);
                    let paper = backdrop(
                        &mut self.wallpaper,
                        renderer,
                        background_geometry(size, view, desktop),
                        view,
                        show_paper,
                        cards,
                        lock_curtain,
                    );
                    elements.effects = self.transitions.overlay(
                        renderer,
                        &mut self.wallpaper,
                        view,
                        size,
                        background_geometry(size, view, desktop),
                        None,
                        locked,
                    );
                    let cursor = self.cursor.elements(
                        renderer,
                        &cursor_what,
                        cursor_pos_override.unwrap_or_else(|| self.manager.pointer_pos()),
                        Rectangle::from_size(size.to_f64().to_logical(view.scale).to_i32_round()),
                        view.scale,
                        cursor_now,
                        accent,
                    );
                    // Screen magnifier (#349): the zoom region drawn
                    // over this output, locked or not.
                    let zoom_plan = self.magnifier.plan_for(
                        crate::magnifier::OutputFrame {
                            name: "tuna-0",
                            size: (size.w, size.h),
                            scale: view.scale,
                            offset: (0, 0),
                        },
                        zoom_inputs,
                        accent,
                    );
                    let presented_cursor = crate::cursor::presented_surface(
                        &cursor,
                        &cursor_what,
                        view.scale,
                        Rectangle::from_size(size),
                    );
                    use crate::native_repaint::{Cover, FrameSignature, Tracked};
                    // Tracked damage, like the native path: one damage
                    // computation over front-to-back passes, then draw
                    // and submit only that region. An unchanged
                    // signature submits nothing, so idle clients keep
                    // their cadence instead of repainting every tick.
                    let [cursor_images, cursor_surface] =
                        crate::cursor::tracked(&cursor, view.scale, Rectangle::from_size(size));
                    let cover = Cover::new(&elements.elements, view.scale);
                    let passes = [
                        Tracked::pass(&paper, 1.0),
                        Tracked::pass(&previews, view.scale),
                        Tracked::covered_pass(&elements.elements, view.scale, &cover),
                        Tracked::pass(&elements.tile, 1.0),
                        Tracked::pass(&elements.top, view.scale),
                        elements
                            .bell
                            .iter()
                            .rev()
                            .map(|(_, e)| Tracked::new(e, 1.0))
                            .collect(),
                        elements
                            .snapshots
                            .iter()
                            .rev()
                            .map(|(_, e, _)| Tracked::new(e, 1.0))
                            .collect(),
                        Tracked::pass(&elements.flashes, 1.0),
                        Tracked::pass(&elements.effects.textures, 1.0),
                        Tracked::pass(&elements.effects.solids, 1.0),
                        cursor_images,
                        cursor_surface,
                    ];
                    let signature = FrameSignature {
                        size: (size.w, size.h),
                        location: (0, 0),
                        scale: view.scale,
                        transform: 0,
                        locked,
                        background,
                        blank_alpha,
                        accent,
                        decor: decor.clone(),
                        above: elements.above,
                        effects: elements
                            .brightness
                            .iter()
                            .copied()
                            .chain(elements.snapshots.iter().map(|s| s.2))
                            .map(f32::to_bits)
                            .chain(self.night_light.effect_bits())
                            .chain(wellbeing.bits())
                            .chain(zoom_plan.iter().flat_map(|plan| plan.signature_bits()))
                            .collect(),
                        groups: FrameSignature::groups(&passes),
                    };
                    let mut roots: Vec<_> =
                        crate::frame_timing::drawn_roots(&self.state, &self.manager, locked)
                            .into_iter()
                            .map(|(surface, _)| surface)
                            .collect();
                    roots.extend(lock_surface.clone());
                    roots.extend(presented_cursor.clone());
                    let timing = crate::frame_timing::pending_callbacks(&roots);
                    // Proof harnesses drive time manually and count
                    // presented frames for liveness; a manually-stepped
                    // clock presents every tick, like before.
                    let present = self.animation_clock.is_manual()
                        || crate::native_repaint::needs_repaint(
                            self.winit_signature.as_ref(),
                            &signature,
                            timing,
                        );
                    let reset = self
                        .winit_signature
                        .as_ref()
                        .is_none_or(|old| !signature.same_global_drawing(old));
                    if reset {
                        self.winit_tracker = None;
                    }
                    crate::grid_trace::event(
                        &self.state,
                        "nested_repaint_decision",
                        format_args!("present={present} pending_callbacks={timing} reset={reset}"),
                    );
                    if present {
                        let tracker = self.winit_tracker.get_or_insert_with(|| {
                            smithay::backend::renderer::damage::OutputDamageTracker::new(
                                size,
                                1.0,
                                Transform::Normal,
                            )
                        });
                        let damage = crate::native_repaint::damage_region(
                            tracker,
                            if reset { 0 } else { age },
                            &passes,
                        )
                        .map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
                        // A magnified frame repaints whole: any change
                        // moves under the zoom.
                        let damage = if zoom_plan.is_some() {
                            vec![Rectangle::from_size(size)]
                        } else {
                            damage
                        };
                        self.winit_signature = Some(signature);
                        let zoom_program = zoom_plan
                            .as_ref()
                            .filter(|plan| !plan.effects.identity)
                            .and_then(|_| crate::magnifier::effects_program(renderer));
                        let zoomed = match zoom_plan.as_ref() {
                            Some(plan) => Some(zoom_passes(
                                renderer,
                                &mut self.zoom_targets,
                                &mut self.cursor,
                                "tuna-0",
                                plan,
                                &cursor_what,
                                view.scale,
                                cursor_now,
                                &cursor,
                                |frame, damage| {
                                    draw_scene(
                                        frame,
                                        background,
                                        &paper,
                                        &elements,
                                        Target {
                                            output: Rectangle::from_size(size),
                                            damage,
                                            scale: view.scale,
                                            blank_alpha,
                                            tint: night_tint.as_ref(),
                                        },
                                        &decor,
                                        &previews,
                                    )
                                },
                            )?),
                            None => None,
                        };
                        // The winit EGL surface presents bottom-up (see the
                        // Y-flip in the backend's own damage path), so the
                        // output transform mirrors vertically; placements
                        // stay top-down.
                        let mut frame = renderer
                            .render(&mut framebuffer, size, Transform::Flipped180)
                            .map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
                        let drew = match (zoom_plan.as_ref(), zoomed) {
                            (Some(plan), Some((scene, zoom, drew))) => {
                                crate::magnifier::present(
                                    &mut frame,
                                    &scene,
                                    &zoom,
                                    plan,
                                    zoom_program.as_ref(),
                                    Some((&cursor, view.scale)),
                                )
                                .map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
                                drew
                            }
                            _ => {
                                let drew = draw_scene(
                                    &mut frame,
                                    background,
                                    &paper,
                                    &elements,
                                    Target {
                                        output: Rectangle::from_size(size),
                                        damage: &damage,
                                        scale: view.scale,
                                        blank_alpha,
                                        tint: night_tint.as_ref(),
                                    },
                                    &decor,
                                    &previews,
                                )?;
                                crate::cursor::draw(&mut frame, &cursor, view.scale, &damage)
                                    .map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
                                drew
                            }
                        };
                        crate::grid_trace::event(
                            &self.state,
                            "nested_finish_begin",
                            format_args!(""),
                        );
                        let finished = frame.finish();
                        crate::grid_trace::event(
                            &self.state,
                            "nested_finish_end",
                            format_args!("ok={}", finished.is_ok()),
                        );
                        let _ = finished.map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
                        let cost = FrameCost::new(&drew, &elements.owners, &damage, size);
                        presented = Some((damage, cost, presented_cursor));
                    }
                }
                if let Some((damage, drawn, presented_cursor)) = presented {
                    self.frame_costs.insert("winit".to_owned(), drawn);
                    send_surface_scales(&self.state, &self.manager);
                    crate::grid_trace::event(&self.state, "nested_submit_begin", format_args!(""));
                    let submitted = backend.submit(Some(&damage));
                    crate::grid_trace::event(
                        &self.state,
                        "nested_submit_end",
                        format_args!("ok={}", submitted.is_ok()),
                    );
                    submitted.map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
                    if let Some(surface) = &presented_cursor {
                        crate::cursor::Cursor::frame_done(
                            &crate::cursor::Pointer::Surface(surface.clone()),
                            cursor_now,
                        );
                    }
                    // The host took the frame: presentation feedback,
                    // fifo barriers and commit timers (#89).
                    crate::frame_timing::present_nested_frame_with_cursor(
                        &mut self.state,
                        &self.manager,
                        locked,
                        self.stats.frames,
                        presented_cursor,
                    );
                    self.stats.frames += 1;
                    true
                } else {
                    crate::grid_trace::event(
                        &self.state,
                        "nested_repaint_skipped",
                        format_args!("reason=no_repaint_required"),
                    );
                    false
                }
            }
            #[cfg(feature = "drm")]
            Backend::Drm(drm) => {
                if !drm.scanout_ready() {
                    self.performance_trace.cancel();
                    return Ok(());
                }
                let pointer = drm.pointer();
                let crate::drm::DrmBackend {
                    renderer, outputs, ..
                } = &mut **drm;
                // What each output's frame draws, for presentation
                // feedback at its page flip (#89): a surface belongs to
                // the output under its center, else the primary.
                let drawn = crate::frame_timing::drawn_roots(&self.state, &self.manager, locked);
                // Clones and switched-off outputs own no surface: what a
                // mirrored logical monitor draws presents on its leader.
                let rects: Vec<Rectangle<i32, Logical>> = outputs
                    .iter()
                    .map(|o| {
                        if o.is_leader() {
                            Rectangle::new(o.loc.into(), o.logical_size().into())
                        } else {
                            Rectangle::default()
                        }
                    })
                    .collect();
                let owner = |at: &smithay::utils::Point<i32, Logical>| {
                    rects.iter().position(|r| r.contains(*at)).unwrap_or(0)
                };
                let mut queued = false;
                let mut cursor_presented = false;
                for (index, out) in outputs.iter_mut().enumerate() {
                    // Display-paced: one frame in flight per output; a
                    // switched-off CRTC gets none (#340).
                    if out.pending || !out.enabled {
                        continue;
                    }
                    let size = out.size;
                    let output_tint = if out.tint_fallback {
                        night_tint.as_ref()
                    } else {
                        plain_tint.as_ref()
                    };
                    // Per-output scale from GNOME's monitors.xml (#59).
                    let view = View {
                        offset: out.loc,
                        scale: out.scale,
                    };
                    let lock_surface = locked
                        .then(|| self.state.lock_surface_for(out.logical_name()))
                        .flatten();
                    // Rotation (#345): the tracker holds the output
                    // transform while Smithay renders inverted (see
                    // `orientation::render_transform`); layout, damage
                    // and the wallpaper all work in the pre-transform
                    // frame, whose physical size swaps at 90°/270°.
                    let output_transform = crate::orientation::smithay_transform(out.transform);
                    let logical = output_transform.transform_size(size);
                    let mut elements = scene_elements(
                        renderer,
                        &self.manager,
                        &self.menu_close,
                        &self.state,
                        view,
                        show_content,
                        overview.as_ref(),
                        lock_surface
                            .as_ref()
                            .zip(lock_curtain)
                            .map(|(surface, curtain)| (surface, curtain, logical.h)),
                        tile.as_ref()
                            .and_then(|(id, _)| self.manager.surface_of(*id))
                            .as_ref(),
                    );
                    if let Some((_, rect)) = tile {
                        elements.tile = tile_elements(rect, view, accent);
                    }
                    if overview.is_none() && show_content {
                        elements
                            .tile
                            .extend(focus_ring_elements(&self.manager, view, None));
                    }
                    elements.top = previews_to_elements(
                        renderer,
                        &self.manager,
                        view,
                        &switcher_previews(&self.manager, &self.switcher_thumbnails),
                    );
                    elements.top.extend(dnd_icon_elements(
                        renderer,
                        &self.state,
                        view,
                        self.manager.pointer_pos(),
                    ));
                    elements.flashes = screenshot_flash_elements(&flashes, view);
                    install_bell(&mut elements, view, bell, desktop);
                    let decor = decor_for_output(&decor_global, view);
                    let previews = preview_elements(renderer, &self.manager, view, cards);
                    let paper = backdrop(
                        &mut self.wallpaper,
                        renderer,
                        background_geometry(logical, view, desktop),
                        view,
                        show_paper,
                        cards,
                        lock_curtain,
                    );
                    elements.effects = self.transitions.overlay(
                        renderer,
                        &mut self.wallpaper,
                        view,
                        logical,
                        background_geometry(logical, view, desktop),
                        Some(out.name.as_str()),
                        locked,
                    );
                    let cursor = self.cursor.elements(
                        renderer,
                        &cursor_what,
                        cursor_pos_override.unwrap_or(pointer),
                        Rectangle::new(out.loc.into(), out.logical_size().into()),
                        out.scale,
                        cursor_now,
                        accent,
                    );
                    // Screen magnifier (#349): this output's zoom
                    // region, in the pre-transform frame.
                    let zoom_plan = self.magnifier.plan_for(
                        crate::magnifier::OutputFrame {
                            name: &out.name,
                            size: (logical.w, logical.h),
                            scale: out.scale,
                            offset: out.loc,
                        },
                        zoom_inputs,
                        accent,
                    );
                    use crate::native_repaint::{Cover, FrameSignature, Tracked};
                    let [cursor_images, cursor_surface] =
                        crate::cursor::tracked(&cursor, view.scale, Rectangle::from_size(logical));
                    // Windows and layers damage only what shows (#503).
                    let cover = Cover::new(&elements.elements, view.scale);
                    // Bottom to top, each pass at the scale it draws with.
                    let passes = [
                        Tracked::pass(&paper, 1.0),
                        Tracked::pass(&previews, view.scale),
                        Tracked::covered_pass(&elements.elements, view.scale, &cover),
                        Tracked::pass(&elements.tile, 1.0),
                        Tracked::pass(&elements.top, view.scale),
                        elements
                            .bell
                            .iter()
                            .rev()
                            .map(|(_, e)| Tracked::new(e, 1.0))
                            .collect(),
                        elements
                            .snapshots
                            .iter()
                            .rev()
                            .map(|(_, e, _)| Tracked::new(e, 1.0))
                            .collect(),
                        Tracked::pass(&elements.flashes, 1.0),
                        Tracked::pass(&elements.effects.textures, 1.0),
                        Tracked::pass(&elements.effects.solids, 1.0),
                        cursor_images,
                        cursor_surface,
                    ];
                    let signature = FrameSignature {
                        size: (size.w, size.h),
                        location: out.loc,
                        scale: out.scale,
                        transform: out.transform,
                        locked,
                        background,
                        blank_alpha,
                        accent,
                        decor: decor.clone(),
                        above: elements.above,
                        effects: elements
                            .brightness
                            .iter()
                            .copied()
                            .chain(elements.snapshots.iter().map(|s| s.2))
                            .map(f32::to_bits)
                            // Night Light gains: a temperature step
                            // repaints, an unchanged filter sleeps.
                            .chain(self.night_light.effect_bits())
                            .chain(wellbeing.bits())
                            .chain(zoom_plan.iter().flat_map(|plan| plan.signature_bits()))
                            .collect(),
                        groups: FrameSignature::groups(&passes),
                    };
                    let presented_cursor = crate::cursor::presented_surface(
                        &cursor,
                        &cursor_what,
                        view.scale,
                        Rectangle::from_size(logical),
                    );
                    let mine: Vec<_> = drawn
                        .iter()
                        .filter(|(_, at)| owner(at) == index)
                        .map(|(surface, _)| surface.clone())
                        .chain(lock_surface.clone())
                        .chain(presented_cursor.clone())
                        .collect();
                    let timing_work =
                        crate::frame_timing::pending_output_work(&timing_roots, &mine, &throttled);
                    if !crate::native_repaint::needs_repaint(
                        out.last_frame.as_ref(),
                        &signature,
                        timing_work,
                    ) {
                        continue;
                    }
                    // Account for changes since this particular swapchain buffer
                    // was submitted, including intervening frames.
                    let (mut dmabuf, age) = match out.surface.next_buffer() {
                        Ok(buffer) => buffer,
                        Err(e) => {
                            eprintln!("tuna-compositor: drm: next_buffer {}: {e}", out.name);
                            continue;
                        }
                    };
                    let reset = out
                        .last_frame
                        .as_ref()
                        .is_none_or(|old| !signature.same_global_drawing(old));
                    if reset {
                        out.damage_tracker = None;
                    }
                    // A transform change resets the tracker through the
                    // signature above; the tracker holds the output
                    // transform while the frame renders inverted.
                    let tracker = out.damage_tracker.get_or_insert_with(|| {
                        smithay::backend::renderer::damage::OutputDamageTracker::new(
                            size,
                            1.0,
                            output_transform,
                        )
                    });
                    let damage = crate::native_repaint::damage_region(
                        tracker,
                        if reset { 0 } else { usize::from(age) },
                        &passes,
                    )
                    .map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
                    // A magnified frame repaints whole.
                    let damage = if zoom_plan.is_some() {
                        vec![Rectangle::from_size(logical)]
                    } else {
                        damage
                    };
                    let zoom_program = zoom_plan
                        .as_ref()
                        .filter(|plan| !plan.effects.identity)
                        .and_then(|_| crate::magnifier::effects_program(renderer));
                    let zoomed = match zoom_plan.as_ref() {
                        Some(plan) => Some(zoom_passes(
                            renderer,
                            &mut self.zoom_targets,
                            &mut self.cursor,
                            &out.name,
                            plan,
                            &cursor_what,
                            view.scale,
                            cursor_now,
                            &cursor,
                            |frame, damage| {
                                draw_scene(
                                    frame,
                                    background,
                                    &paper,
                                    &elements,
                                    Target {
                                        output: Rectangle::from_size(logical),
                                        damage,
                                        scale: view.scale,
                                        blank_alpha,
                                        tint: output_tint,
                                    },
                                    &decor,
                                    &previews,
                                )
                            },
                        )?),
                        None => None,
                    };
                    let cost;
                    let sync = {
                        let mut target = renderer
                            .bind(&mut dmabuf)
                            .map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
                        let mut frame = renderer
                            .render(
                                &mut target,
                                size,
                                crate::orientation::render_transform(out.transform),
                            )
                            .map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
                        let drew = match (zoom_plan.as_ref(), zoomed) {
                            (Some(plan), Some((scene, zoom, drew))) => {
                                crate::magnifier::present(
                                    &mut frame,
                                    &scene,
                                    &zoom,
                                    plan,
                                    zoom_program.as_ref(),
                                    Some((&cursor, view.scale)),
                                )
                                .map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
                                drew
                            }
                            _ => {
                                let drew = draw_scene(
                                    &mut frame,
                                    background,
                                    &paper,
                                    &elements,
                                    Target {
                                        output: Rectangle::from_size(logical),
                                        damage: &damage,
                                        scale: view.scale,
                                        blank_alpha,
                                        tint: output_tint,
                                    },
                                    &decor,
                                    &previews,
                                )?;
                                // The pointer on top (no host cursor on bare
                                // hardware); hidden while locked (#342).
                                crate::cursor::draw(&mut frame, &cursor, view.scale, &damage)
                                    .map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
                                drew
                            }
                        };
                        cost = FrameCost::new(&drew, &elements.owners, &damage, size);
                        frame
                            .finish()
                            .map_err(|e| RuntimeError::Dispatch(e.to_string()))?
                    };
                    let feedback = crate::frame_timing::take_feedback(&mine, &out.output);
                    if let Err(e) = out.surface.queue_buffer(
                        Some(sync),
                        Some(crate::native_repaint::scanout_damage_rotated(
                            &damage,
                            output_transform,
                            (logical.w, logical.h),
                        )),
                        feedback,
                    ) {
                        eprintln!("tuna-compositor: drm: queue_buffer {}: {e}", out.name);
                        // Never reuse history after an unsubmitted partial update.
                        out.last_frame = None;
                        out.damage_tracker = None;
                        // A new mode the kernel tested but would not
                        // commit: back to the previous one (#340).
                        out.revert_failed_mode();
                        continue;
                    }
                    out.mode_committed();
                    out.trace_wake_submission();
                    out.pending = true;
                    if index == 0 {
                        self.performance_trace.queued(
                            Duration::from(self.state.presentation_now()),
                            crate::performance_trace::CardReadiness {
                                expected: cards.map(|layout| layout.cards.len()).or_else(|| {
                                    (!overview_requested && overview.is_none()).then_some(0)
                                }),
                                rendered: if cards.is_some() { paper.len() } else { 0 },
                                cached: self.wallpaper.card_counts().0,
                                pending: self.wallpaper.card_counts().1,
                            },
                        );
                    }
                    cursor_presented |= presented_cursor.is_some();
                    out.last_frame = Some(signature);
                    self.frame_costs.insert(out.name.clone(), cost);
                    queued = true;
                }
                send_surface_scales(&self.state, &self.manager);
                if queued {
                    // The cursor surface is not a window: its callback
                    // follows the scanout that showed it.
                    if cursor_presented {
                        crate::cursor::Cursor::frame_done(&cursor_what, cursor_now);
                    }
                    self.stats.frames += 1;
                    // A queued frame is no activity of its own (#503): its
                    // page flip, the clients' next commits and the running
                    // animations wake the loop, as Mutter's frame clock
                    // idles between a client's frames.
                }
                queued
            }
        };
        let hidden = hidden_surfaces(&self.manager, &self.frame_costs, clones_shown);
        let throttled = throttled_surfaces(&self.state, &hidden);
        if queued {
            // A pending page flip is not another rendered frame.
            // Granting callbacks on every client dispatch here would
            // let redraws outrun the display and keep the loop busy.
            send_frame_callbacks(&self.state, &self.manager, &throttled);
        }
        crate::grid_trace::event(&self.state, "render_end", format_args!("queued={queued}"));
        // Hidden windows get theirs on a slow beat of their own, whether
        // or not anything was drawn.
        self.hidden_frame_beat(&throttled);
        Ok(())
    }

    /// Send hidden windows their frame callbacks at most once per
    /// [`HIDDEN_FRAME_INTERVAL`](crate::occlusion::HIDDEN_FRAME_INTERVAL),
    /// without a scanout (#503). Mutter sends an obscured surface none;
    /// the slow beat keeps clients live that wait on a callback before
    /// acting on anything.
    fn hidden_frame_beat(
        &mut self,
        throttled: &[smithay::reexports::wayland_server::protocol::wl_surface::WlSurface],
    ) {
        let now = Instant::now();
        self.hidden_waiting = crate::frame_timing::pending_callbacks(throttled);
        if !self.hidden_waiting
            || self.hidden_beat.is_some_and(|last| {
                now.saturating_duration_since(last) < crate::occlusion::HIDDEN_FRAME_INTERVAL
            })
        {
            return;
        }
        let Some(output) = self.state.primary_output() else {
            return;
        };
        let time = Duration::from(self.state.presentation_now());
        for surface in throttled {
            send_frames_surface_tree(surface, &output, time, Some(Duration::ZERO), |_, _| {
                Some(output.clone())
            });
        }
        self.hidden_beat = Some(now);
        self.hidden_waiting = false;
    }

    /// While the display power-save mode is not on (#355), complete frames
    /// on Mutter's dummy page flip: every surface's frame callbacks, fifo
    /// barriers and commit timers at most once per
    /// [`FRAME_INTERVAL`](crate::power_save::FRAME_INTERVAL). Hidden windows
    /// are served by this beat too.
    fn power_save_frame_beat(&mut self) {
        self.hidden_waiting = false;
        let now = Instant::now();
        if self.power_save_beat.is_some_and(|last| {
            now.saturating_duration_since(last) < crate::power_save::FRAME_INTERVAL
        }) {
            return;
        }
        let Some(output) = self.state.primary_output() else {
            return;
        };
        send_frame_callbacks(&self.state, &self.manager, &[]);
        let roots = crate::frame_timing::frame_roots(&self.state, &self.manager);
        let time = self.state.presentation_now();
        self.state
            .refresh_cycle(&roots, time, crate::frame_timing::refresh_of(&output));
        self.power_save_beat = Some(now);
    }

    /// Whether a surface waits on the next power-save beat.
    fn power_save_waiting(&self) -> bool {
        !self.power_save.is_on()
            && crate::frame_timing::pending_callbacks(&crate::frame_timing::frame_roots(
                &self.state,
                &self.manager,
            ))
    }
}

/// Overview backdrop (GNOME 51's `#overviewGroup`, #222226).
/// Vertical finger travel for a whole overview transition (logical
/// touchpad units, as libinput reports swipe deltas).
const OVERVIEW_SWIPE_DISTANCE: f64 = 300.0;

const OVERVIEW_BACKGROUND: Color32F = Color32F::new(34.0 / 255.0, 34.0 / 255.0, 38.0 / 255.0, 1.0);

/// The overview's solid fills (the workspace thumbnails strip) in
/// global logical space.
fn overview_decor(
    layout: &crate::overview::OverviewLayout,
    accent: [f32; 3],
) -> Vec<(Color32F, Vec<Rectangle<i32, Logical>>)> {
    crate::overview::thumbnail_decor(layout, accent)
        .into_iter()
        .map(|([r, g, b, a], rects)| (Color32F::new(r, g, b, a), rects))
        .collect()
}

/// Global decor in one output's physical pixels.
fn decor_for_output(
    decor: &[(Color32F, Vec<Rectangle<i32, Logical>>)],
    view: View,
) -> Vec<(Color32F, Vec<Rectangle<i32, smithay::utils::Physical>>)> {
    decor
        .iter()
        .map(|(color, rects)| {
            (
                *color,
                rects
                    .iter()
                    .map(|r| {
                        let top_left = view.physical(f64::from(r.loc.x), f64::from(r.loc.y));
                        let bottom_right = view
                            .physical(f64::from(r.loc.x + r.size.w), f64::from(r.loc.y + r.size.h));
                        Rectangle::from_extremities(top_left, bottom_right)
                    })
                    .collect(),
            )
        })
        .collect()
}

/// A window preview element: the window's surface tree, shrunk about
/// its top-left corner.
type PreviewElement = smithay::backend::renderer::element::utils::RescaleRenderElement<
    WaylandSurfaceRenderElement<GlesRenderer>,
>;

/// Overview previews for one output, front to back (#54).
fn preview_elements(
    renderer: &mut GlesRenderer,
    manager: &WindowManager,
    view: View,
    overview: Option<&crate::overview::OverviewLayout>,
) -> Vec<PreviewElement> {
    let Some(layout) = overview else {
        return Vec::new();
    };
    previews_to_elements(renderer, manager, view, &layout.previews)
}

/// Window surfaces drawn scaled into preview rects, front to back.
fn previews_to_elements(
    renderer: &mut GlesRenderer,
    manager: &WindowManager,
    view: View,
    previews: &[crate::overview::Preview],
) -> Vec<PreviewElement> {
    let mut out = Vec::new();
    for preview in previews {
        let Some(surface) = manager.surface_of(preview.id) else {
            continue;
        };
        // Shrink about the surface origin, placed so the visible window
        // (inside client shadows, also shrunk) fills the preview rect.
        let geo = crate::popup::window_geometry_loc(&surface);
        let origin = view.physical(
            f64::from(preview.rect.loc.x) - f64::from(geo.x) * preview.scale,
            f64::from(preview.rect.loc.y) - f64::from(geo.y) * preview.scale,
        );
        for element in render_elements_from_surface_tree::<_, WaylandSurfaceRenderElement<_>>(
            renderer,
            &surface,
            origin,
            view.scale,
            preview.alpha,
            Kind::Unspecified,
        ) {
            out.push(
                smithay::backend::renderer::element::utils::RescaleRenderElement::from_element(
                    element,
                    origin,
                    preview.scale,
                ),
            );
        }
    }
    crate::layer::front_to_back(out)
}

/// A drag's icon at the pointer, centred on it (front to back).
fn dnd_icon_elements(
    renderer: &mut GlesRenderer,
    state: &State,
    view: View,
    pointer: Point<f64, Logical>,
) -> Vec<PreviewElement> {
    let Some(icon) = state.dnd_icon() else {
        return Vec::new();
    };
    let size = smithay::wayland::compositor::with_states(&icon, |states| {
        states
            .data_map
            .get::<smithay::backend::renderer::utils::RendererSurfaceStateUserData>()
            .and_then(|d| d.lock().ok().and_then(|d| d.surface_size()))
    })
    .unwrap_or_default();
    let origin = view.physical(
        pointer.x - f64::from(size.w) / 2.0,
        pointer.y - f64::from(size.h) / 2.0,
    );
    let elements = render_elements_from_surface_tree::<_, WaylandSurfaceRenderElement<_>>(
        renderer,
        &icon,
        origin,
        view.scale,
        1.0,
        Kind::Unspecified,
    );
    crate::layer::front_to_back(
        elements
            .into_iter()
            .map(|e| {
                smithay::backend::renderer::element::utils::RescaleRenderElement::from_element(
                    e, origin, 1.0,
                )
            })
            .collect(),
    )
}

/// GNOME's switcher thumbnails (altTab.js `_createWindowClone`): each
/// window scaled down (never up) to fit its frame, centred in it.
fn switcher_previews(
    manager: &WindowManager,
    thumbnails: &[tuna_shell_control::SwitcherThumbnail],
) -> Vec<crate::overview::Preview> {
    thumbnails
        .iter()
        .filter_map(|t| Some(switcher_preview(t, manager.geometry(t.window)?.size)))
        .collect()
}

/// One thumbnail's window (of `size`) fitted into its frame, drawn at the
/// frame's opacity: the clone sits inside the thumbnail list, so it fades
/// with it (altTab.js THUMBNAIL_FADE_TIME) and with the whole popup
/// (switcherPopup.js POPUP_FADE_OUT_TIME).
fn switcher_preview(
    t: &tuna_shell_control::SwitcherThumbnail,
    size: smithay::utils::Size<i32, Logical>,
) -> crate::overview::Preview {
    let (w, h) = (f64::from(size.w.max(1)), f64::from(size.h.max(1)));
    let scale = (f64::from(t.width) / w)
        .min(f64::from(t.height) / h)
        .min(1.0);
    let (sw, sh) = ((w * scale).round() as i32, (h * scale).round() as i32);
    crate::overview::Preview {
        id: t.window,
        rect: Rectangle::new(
            (t.x + (t.width - sw) / 2, t.y + (t.height - sh) / 2).into(),
            (sw, sh).into(),
        ),
        scale,
        active: false,
        alpha: f32::from(t.opacity) / 255.0,
    }
}

use crate::wellbeing_effects::FrameColor;

/// One output's surfaces, front to back, with GNOME's tile preview
/// slotted in: `elements[..above]` draw over the preview (the dragged
/// window, its popups and the shell's layers), the rest beneath it.
struct Scene {
    elements: Vec<PreviewElement>,
    above: usize,
    tile: Vec<smithay::backend::renderer::element::solid::SolidColorRenderElement>,
    /// Window thumbnails over everything (the Alt+Tab switcher's).
    top: Vec<PreviewElement>,
    /// Capture feedback is painted only on outputs, never into saved pixels.
    flashes: Vec<smithay::backend::renderer::element::solid::SolidColorRenderElement>,
    /// Desktop transitions over the scene, under the screenshot flash
    /// (`crate::transitions`); output feedback only, never captured.
    effects: crate::transitions::Overlay,
    bell: Vec<(
        Option<usize>,
        smithay::backend::renderer::element::solid::SolidColorRenderElement,
    )>,
    /// Old-frame snapshots of size-change transitions (#496), each
    /// drawn in front of `elements[index..]`, in physical pixels.
    snapshots: Vec<(usize, SnapshotElement, f32)>,
    brightness: Vec<f32>,
    dim_program: Option<smithay::backend::renderer::gles::GlesTexProgram>,
    /// The window each of `elements` belongs to (popups included), for
    /// reporting which windows occlusion hid; `None` for layers.
    owners: Vec<Option<u64>>,
}

/// A size-change snapshot, drawn at scale 1.
type SnapshotElement = smithay::backend::renderer::element::texture::TextureRenderElement<
    smithay::backend::renderer::gles::GlesTexture,
>;

impl Scene {
    /// Surfaces with nothing between them.
    fn flat(elements: Vec<WaylandSurfaceRenderElement<GlesRenderer>>) -> Self {
        let above = elements.len();
        Self {
            elements: elements
                .into_iter()
                .map(|element| {
                    use smithay::backend::renderer::element::Element;
                    let origin = element.geometry(1.0.into()).loc;
                    smithay::backend::renderer::element::utils::RescaleRenderElement::from_element(
                        element, origin, 1.0,
                    )
                })
                .collect(),
            above,
            tile: Vec::new(),
            top: Vec::new(),
            flashes: Vec::new(),
            effects: Default::default(),
            bell: Vec::new(),
            snapshots: Vec::new(),
            brightness: vec![1.0; above],
            dim_program: None,
            owners: vec![None; above],
        }
    }
}

/// The flash uses premultiplied white, with changing commit metadata so native
/// damage includes every fade frame and the frame that removes the feedback.
fn screenshot_flash_elements(
    flashes: &[([i32; 4], f32)],
    view: View,
) -> Vec<smithay::backend::renderer::element::solid::SolidColorRenderElement> {
    use smithay::backend::renderer::element::{solid::SolidColorRenderElement, Id, Kind};
    thread_local! {
        static IDS: [Id; crate::screenshot_flash::MAX_FLASHES] = std::array::from_fn(|_| Id::new());
    }
    IDS.with(|ids| {
        flashes
            .iter()
            .zip(ids.iter())
            .map(|(sample, id)| {
                let ([x, y, width, height], alpha) = *sample;
                let start = view.physical(f64::from(x), f64::from(y));
                let end = view.physical(
                    f64::from(x) + f64::from(width),
                    f64::from(y) + f64::from(height),
                );
                SolidColorRenderElement::new(
                    id.clone(),
                    Rectangle::new(
                        start,
                        ((end.x - start.x).max(1), (end.y - start.y).max(1)).into(),
                    ),
                    alpha.to_bits() as usize,
                    Color32F::new(alpha, alpha, alpha, alpha),
                    Kind::Unspecified,
                )
            })
            .collect()
    })
}

/// A frame bell sits above its own window and below every higher layer/window.
fn insert_bell_slots(slots: &mut Vec<SceneSlot>, scene: &Scene) {
    for (k, (at, _)) in scene.bell.iter().enumerate() {
        let Some(at) = at else { continue };
        insert_bell_slot(slots, *at, k);
    }
}

fn insert_bell_slot(slots: &mut Vec<SceneSlot>, at: usize, k: usize) {
    if let Some(index) = slots
        .iter()
        .position(|slot| *slot == SceneSlot::Element(at))
    {
        slots.insert(index, SceneSlot::Bell(k));
    }
}

type BellFrame = (f32, Option<(u64, Rectangle<i32, Logical>)>);

fn install_bell(scene: &mut Scene, view: View, bell: Option<BellFrame>, desktop: [i32; 4]) {
    use smithay::backend::renderer::element::{solid::SolidColorRenderElement, Id};
    let Some((alpha, target)) = bell.filter(|(alpha, _)| *alpha > 0.0) else {
        return;
    };
    let (at, rect) = match target {
        Some((id, geo)) => {
            let Some(at) = scene.owners.iter().position(|owner| *owner == Some(id)) else {
                return;
            };
            (Some(at), geo)
        }
        None => (
            None,
            Rectangle::new(
                (desktop[0], desktop[1]).into(),
                (desktop[2], desktop[3]).into(),
            ),
        ),
    };
    let origin = view.physical(f64::from(rect.loc.x), f64::from(rect.loc.y));
    let end = view.physical(
        f64::from(rect.loc.x + rect.size.w),
        f64::from(rect.loc.y + rect.size.h),
    );
    thread_local! { static BELL_ID: Id = Id::new(); }
    scene.bell.push((
        at,
        SolidColorRenderElement::new(
            BELL_ID.with(Clone::clone),
            Rectangle::new(origin, (end.x - origin.x, end.y - origin.y).into()),
            0usize,
            Color32F::new(0.0, 0.0, 0.0, alpha),
            Kind::Unspecified,
        ),
    ));
}

/// GNOME's tile preview (`.tile-preview`: the accent at half opacity
/// with a 1px accent border) over `rect`, in one output's pixels.
fn tile_elements(
    rect: Rectangle<i32, Logical>,
    view: View,
    accent: [f32; 3],
) -> Vec<smithay::backend::renderer::element::solid::SolidColorRenderElement> {
    use smithay::backend::renderer::element::solid::SolidColorRenderElement;
    use smithay::backend::renderer::element::Id;
    let top_left = view.physical(f64::from(rect.loc.x), f64::from(rect.loc.y));
    let bottom_right = view.physical(
        f64::from(rect.loc.x + rect.size.w),
        f64::from(rect.loc.y + rect.size.h),
    );
    let r = Rectangle::from_extremities(top_left, bottom_right);
    let b = (view.scale.round() as i32).max(1);
    let (w, h) = (r.size.w, r.size.h);
    if w <= 2 * b || h <= 2 * b {
        return Vec::new();
    }
    let [cr, cg, cb] = accent;
    let border = Color32F::new(cr, cg, cb, 1.0);
    // Premultiplied: half opacity halves every channel.
    let fill = Color32F::new(cr * 0.5, cg * 0.5, cb * 0.5, 0.5);
    let at = |x: i32, y: i32, w: i32, h: i32| -> Rectangle<i32, smithay::utils::Physical> {
        Rectangle::new((r.loc.x + x, r.loc.y + y).into(), (w, h).into())
    };
    // Stable ids keep damage tracking quiet while the preview holds.
    thread_local! {
        static IDS: [Id; 5] = std::array::from_fn(|_| Id::new());
    }
    IDS.with(|ids| {
        [
            (at(0, 0, w, b), border),
            (at(0, h - b, w, b), border),
            (at(0, b, b, h - 2 * b), border),
            (at(w - b, b, b, h - 2 * b), border),
            (at(b, b, w - 2 * b, h - 2 * b), fill),
        ]
        .into_iter()
        .zip(ids.iter())
        .map(|((geo, color), id)| {
            SolidColorRenderElement::new(id.clone(), geo, 0usize, color, Kind::Unspecified)
        })
        .collect()
    })
}

/// niri's focus ring in scroll mode (its default `focus-ring`): 4px of
/// #7fc8ff around the focused column, outside its edges, where the
/// strip's 16px gaps leave room.
fn focus_ring_elements(
    manager: &WindowManager,
    view: View,
    tint: Option<crate::night_light::ColorGains>,
) -> Vec<smithay::backend::renderer::element::solid::SolidColorRenderElement> {
    use smithay::backend::renderer::element::solid::SolidColorRenderElement;
    use smithay::backend::renderer::element::Id;
    const WIDTH: i32 = 4;
    if manager.session_mode() != crate::windows::SessionMode::Scroll {
        return Vec::new();
    }
    let Some(rect) = manager
        .model()
        .focused()
        .and_then(|id| manager.render_geometry(id))
    else {
        return Vec::new();
    };
    let outer = view.physical(f64::from(rect.loc.x - WIDTH), f64::from(rect.loc.y - WIDTH));
    let outer_end = view.physical(
        f64::from(rect.loc.x + rect.size.w + WIDTH),
        f64::from(rect.loc.y + rect.size.h + WIDTH),
    );
    let r = Rectangle::from_extremities(outer, outer_end);
    let b = (f64::from(WIDTH) * view.scale).round().max(1.0) as i32;
    let (w, h) = (r.size.w, r.size.h);
    if w <= 2 * b || h <= 2 * b {
        return Vec::new();
    }
    let [fr, fg, fb] = tint
        .map(|gains| {
            crate::night_light::apply_gains([0x7f as f32 / 255.0, 0xc8 as f32 / 255.0, 1.0], gains)
        })
        .unwrap_or([0x7f as f32 / 255.0, 0xc8 as f32 / 255.0, 1.0]);
    let color = Color32F::new(fr, fg, fb, 1.0);
    let at = |x: i32, y: i32, w: i32, h: i32| -> Rectangle<i32, smithay::utils::Physical> {
        Rectangle::new((r.loc.x + x, r.loc.y + y).into(), (w, h).into())
    };
    thread_local! {
        static IDS: [Id; 4] = std::array::from_fn(|_| Id::new());
    }
    IDS.with(|ids| {
        [
            at(0, 0, w, b),
            at(0, h - b, w, b),
            at(0, b, b, h - 2 * b),
            at(w - b, b, b, h - 2 * b),
        ]
        .into_iter()
        .zip(ids.iter())
        .map(|(geo, id)| {
            SolidColorRenderElement::new(id.clone(), geo, 0usize, color, Kind::Unspecified)
        })
        .collect()
    })
}

/// Window and layer-shell elements for one output whose top-left sits
/// at `offset` in the global space. Empty while content is hidden
/// (locked): nothing beneath the lock surface may show. `split` is the
/// window being dragged over a snap edge: it and everything above it
/// draw over the tile preview.
#[allow(clippy::too_many_arguments)]
fn scene_elements(
    renderer: &mut GlesRenderer,
    manager: &WindowManager,
    menu_close: &crate::menu_close::MenuClose,
    state: &State,
    view: View,
    show_content: bool,
    overview: Option<&crate::overview::OverviewLayout>,
    lock: Option<(
        &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
        crate::transitions::CurtainFrame,
        i32,
    )>,
    split: Option<&smithay::reexports::wayland_server::protocol::wl_surface::WlSurface>,
) -> Scene {
    if !show_content {
        // Locked: only the shell's lock surface for this output, at its
        // origin (ext-session-lock) less the curtain's lift; nothing
        // else may show.
        return Scene::flat(
            lock.map(|(surface, curtain, height)| {
                render_elements_from_surface_tree(
                    renderer,
                    surface,
                    (0, -curtain.lift_px(height)),
                    view.scale,
                    curtain.alpha,
                    Kind::Unspecified,
                )
            })
            .unwrap_or_default(),
        );
    }
    if overview.is_some() {
        // Overview (#54): windows are drawn as rescaled previews in
        // their own pass (`preview_elements`); here only the shell's
        // layers (and their popovers) go on top.
        let mut elements = Vec::new();
        for (surface, (x, y), _) in crate::layer::layer_layout(state) {
            elements.extend(render_elements_from_surface_tree(
                renderer,
                &surface,
                view.physical(f64::from(x), f64::from(y)),
                view.scale,
                1.0,
                Kind::Unspecified,
            ));
            for popup in crate::popup::placed_popups(&surface, (x, y).into(), false) {
                elements.extend(render_elements_from_surface_tree(
                    renderer,
                    &popup.surface,
                    view.physical(f64::from(popup.origin.x), f64::from(popup.origin.y)),
                    view.scale,
                    1.0,
                    Kind::Unspecified,
                ));
            }
        }
        return Scene::flat(crate::layer::front_to_back(elements));
    }
    let mut elements: Vec<PreviewElement> = Vec::new();
    let mut owners: Vec<Option<u64>> = Vec::new();
    // Bottom-to-top index where the dragged window starts.
    let mut split_from = None;
    let tree = |renderer: &mut GlesRenderer,
                elements: &mut Vec<PreviewElement>,
                surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
                at: Point<i32, Logical>,
                sx: f64,
                alpha: f32| {
        let origin = view.physical(f64::from(at.x), f64::from(at.y));
        elements.extend(
            render_elements_from_surface_tree::<_, WaylandSurfaceRenderElement<_>>(
                renderer,
                surface,
                origin,
                view.scale,
                alpha,
                Kind::Unspecified,
            )
            .into_iter()
            .map(|element| {
                smithay::backend::renderer::element::utils::RescaleRenderElement::from_element(
                    element,
                    origin,
                    (sx, 1.0),
                )
            }),
        );
    };
    // Size-change snapshots with the bottom-to-top index they sit at.
    let mut snapshots = Vec::new();
    for (id, window, geometry) in manager.render_entries() {
        for ghost in manager
            .window_lifecycle()
            .ghosts()
            .filter(|g| g.before == Some(id))
        {
            snapshots.extend(
                manager
                    .window_lifecycle()
                    .element(ghost, renderer, view)
                    .map(|e| (elements.len(), e, 1.0)),
            );
        }
        // Unassociated X11 windows contribute no surface yet and
        // render nothing this frame.
        if let Some(surface) = window.wl_surface() {
            if split_from.is_none() && split.is_some_and(|s| *s == *surface) {
                split_from = Some(elements.len());
            }
            let buffer_size =
                crate::windows::committed_size(surface.as_ref()).unwrap_or(geometry.size);
            let buffer_geometry = Rectangle::new(
                crate::popup::surface_origin(&surface, geometry.loc),
                buffer_size,
            );
            if let Some((r, fade)) = manager
                .window_lifecycle()
                .minimizing_frame(id, buffer_geometry.to_f64())
            {
                let scale = (
                    r.size.w / f64::from(buffer_size.w),
                    r.size.h / f64::from(buffer_size.h),
                );
                let origin = view.physical(r.loc.x, r.loc.y);
                elements.extend(render_elements_from_surface_tree::<_, WaylandSurfaceRenderElement<_>>(renderer, &surface, origin, view.scale, fade, Kind::Unspecified).into_iter().map(|e| smithay::backend::renderer::element::utils::RescaleRenderElement::from_element(e, origin, scale)));
                owners.resize(elements.len(), Some(id));
                continue;
            }
            if let Some((mut frame, snapshot)) = manager.size_changes().frame(id) {
                // A workspace switch carries the transition along with its
                // window: the live frame and the snapshot above it.
                let dx = f64::from(manager.slide_dx(id));
                frame.live.loc.x += dx;
                frame.snapshot.loc.x += dx;
                let alpha = manager.render_alpha(&window);
                frame.snapshot_alpha *= alpha;
                // GNOME's size-change transition (#496): the live window
                // eased (never scaled past its size), the old frame
                // fading over it. Popups wait for the window to land.
                if frame.live_visible {
                    let geo = crate::popup::window_geometry_loc(&surface);
                    let (sx, sy) = frame.live_scale;
                    let origin = view.physical(
                        frame.live.loc.x - f64::from(geo.x) * sx,
                        frame.live.loc.y - f64::from(geo.y) * sy,
                    );
                    elements.extend(
                        render_elements_from_surface_tree::<_, WaylandSurfaceRenderElement<_>>(
                            renderer,
                            &surface,
                            origin,
                            view.scale,
                            alpha,
                            Kind::Unspecified,
                        )
                        .into_iter()
                        .map(|element| {
                            smithay::backend::renderer::element::utils::RescaleRenderElement::from_element(
                                element,
                                origin,
                                (sx, sy),
                            )
                        }),
                    );
                }
                snapshots.extend(
                    crate::size_change::snapshot_element(renderer, snapshot, &frame, view).map(
                        |element| (elements.len(), element, manager.modal_dim().multiplier(id)),
                    ),
                );
                owners.resize(elements.len(), Some(id));
                continue;
            }
            let buffer_size =
                crate::windows::committed_size(surface.as_ref()).unwrap_or(geometry.size);
            let buffer_geometry = Rectangle::new(
                crate::popup::surface_origin(&surface, geometry.loc),
                buffer_size,
            );
            if let Some((r, fade)) = manager
                .window_lifecycle()
                .restoring_frame(id, buffer_geometry.to_f64())
            {
                let scale = (
                    r.size.w / f64::from(buffer_size.w),
                    r.size.h / f64::from(buffer_size.h),
                );
                let origin = view.physical(r.loc.x, r.loc.y);
                elements.extend(render_elements_from_surface_tree::<_,WaylandSurfaceRenderElement<_>>(renderer,&surface,origin,view.scale,fade*manager.render_alpha(&window),Kind::Unspecified).into_iter().map(|e|smithay::backend::renderer::element::utils::RescaleRenderElement::from_element(e,origin,scale)));
                owners.resize(elements.len(), Some(id));
                continue;
            }
            if let Some(frame) = manager.window_lifecycle().opening_frame(id) {
                let r = crate::window_lifecycle::rect(geometry, frame);
                let geo = crate::popup::window_geometry_loc(&surface);
                let origin = view.physical(
                    r.loc.x - f64::from(geo.x) * frame.scale.0,
                    r.loc.y - f64::from(geo.y) * frame.scale.1,
                );
                let alpha = frame.alpha * manager.render_alpha(&window);
                elements.extend(render_elements_from_surface_tree::<_,WaylandSurfaceRenderElement<_>>(renderer,&surface,origin,view.scale,alpha,Kind::Unspecified).into_iter().map(|e|smithay::backend::renderer::element::utils::RescaleRenderElement::from_element(e,origin,frame.scale)));
                owners.resize(elements.len(), Some(id));
                continue;
            }
            let origin = crate::popup::surface_origin(&surface, geometry.loc);
            let committed_width = window.geometry().size.w;
            let sx = if manager.session_mode() == crate::windows::SessionMode::Scroll
                && committed_width > 0
            {
                f64::from(geometry.size.w) / f64::from(committed_width)
            } else {
                // An unmapped tree has no committed bounds to scale yet.
                1.0
            };
            // Below 1 only while a fade-only workspace switch crossfades.
            let alpha = manager.render_alpha(&window);
            tree(renderer, &mut elements, &surface, origin, sx, alpha);
            // Popups (#88) right above their window.
            for popup in crate::popup::placed_popups(&surface, origin, true) {
                tree(
                    renderer,
                    &mut elements,
                    &popup.surface,
                    popup.origin,
                    1.0,
                    alpha,
                );
            }
            owners.resize(elements.len(), Some(id));
        }
    }
    let rendered_ids: Vec<_> = manager.render_entries().iter().map(|e| e.0).collect();
    for ghost in manager
        .window_lifecycle()
        .ghosts()
        .filter(|g| g.before.is_none_or(|id| !rendered_ids.contains(&id)))
    {
        snapshots.extend(
            manager
                .window_lifecycle()
                .element(ghost, renderer, view)
                .map(|e| (elements.len(), e, 1.0)),
        );
    }
    // Layer shell above windows: panel strip, then overview, each with
    // its popups (toolkit popovers hang off layer surfaces).
    for (surface, (x, y), _) in crate::layer::layer_layout(state) {
        tree(renderer, &mut elements, &surface, (x, y).into(), 1.0, 1.0);
        for popup in crate::popup::placed_popups(&surface, (x, y).into(), false) {
            tree(
                renderer,
                &mut elements,
                &popup.surface,
                popup.origin,
                1.0,
                1.0,
            );
        }
        // Panel menus closing over the bar they hang off.
        for ghost in menu_close.ghosts_of(&surface) {
            snapshots.extend(
                menu_close
                    .element(ghost, renderer, view)
                    .map(|e| (elements.len(), e, 1.0)),
            );
        }
    }
    // `elements` accumulates bottom-to-top (windows, then
    // background-to-overlay layers); Smithay 0.7 draws the first
    // element topmost.
    let above = elements.len() - split_from.unwrap_or(0);
    let total = elements.len();
    owners.resize(total, None);
    let snapshots: Vec<(usize, SnapshotElement, f32)> = snapshots
        .into_iter()
        .map(|(index, element, brightness)| (total - index, element, brightness))
        .collect();
    let brightness: Vec<f32> = owners
        .iter()
        .map(|id| id.map_or(1.0, |id| manager.modal_dim().multiplier(id)))
        .collect();
    let dim_program = if brightness.iter().any(|v| *v < 1.0) || snapshots.iter().any(|s| s.2 < 1.0)
    {
        crate::modal_dim::program(renderer)
    } else {
        None
    };
    Scene {
        elements: crate::layer::front_to_back(elements),
        brightness: crate::layer::front_to_back(brightness),
        dim_program,
        above,
        tile: Vec::new(),
        top: Vec::new(),
        flashes: Vec::new(),
        effects: Default::default(),
        bell: Vec::new(),
        snapshots,
        owners: crate::layer::front_to_back(owners),
    }
}

fn background_geometry(
    size: smithay::utils::Size<i32, smithay::utils::Physical>,
    view: View,
    desktop: [i32; 4],
) -> tuna_wallpaper::background::Geometry {
    tuna_wallpaper::background::Geometry {
        physical: [size.w.max(0) as u32, size.h.max(0) as u32],
        origin: [view.offset.0, view.offset.1],
        desktop,
        scale_bits: view.scale.to_bits(),
    }
}

/// What lies under the windows on one output, in physical pixels: the
/// wallpaper on the desktop, or in the overview GNOME's workspace cards
/// (the wallpaper below the bar, rounded, over a shadow).
fn backdrop(
    wallpaper: &mut Wallpaper,
    renderer: &mut GlesRenderer,
    geometry: tuna_wallpaper::background::Geometry,
    view: View,
    show_paper: bool,
    cards: Option<&crate::overview::OverviewLayout>,
    curtain: Option<crate::transitions::CurtainFrame>,
) -> Vec<smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement<GlesRenderer>>
{
    let size = smithay::utils::Size::<i32, smithay::utils::Physical>::from((
        geometry.physical[0] as i32,
        geometry.physical[1] as i32,
    ));
    // Locked: the lock background, on the curtain.
    if let Some(curtain) = curtain {
        return wallpaper
            .lock_element(
                renderer,
                size.w,
                size.h,
                geometry,
                curtain.lift_px(size.h),
                curtain.alpha,
            )
            .into_iter()
            .collect();
    }
    if show_paper {
        return wallpaper.element(renderer, size.w, size.h, geometry);
    }
    let Some(layout) = cards else {
        return Vec::new();
    };
    let output = (size.w, size.h).into();
    let work_top = (f64::from(crate::windows::WORK_AREA_TOP) * view.scale).round() as i32;
    layout
        .cards
        .iter()
        .filter_map(|card| {
            let r = card.rect;
            let loc = view.physical(f64::from(r.loc.x), f64::from(r.loc.y));
            let end = view.physical(f64::from(r.loc.x + r.size.w), f64::from(r.loc.y + r.size.h));
            let rect = Rectangle::new(loc, (end.x - loc.x, end.y - loc.y).into());
            // Transition frames scale the card rendered at its settled size.
            let settled = if r.size == card.settled {
                rect.size
            } else {
                (
                    (f64::from(card.settled.w) * view.scale).round() as i32,
                    (f64::from(card.settled.h) * view.scale).round() as i32,
                )
                    .into()
            };
            wallpaper.card_element(
                renderer, output, work_top, rect, settled, card.alpha, geometry,
            )
        })
        .collect()
}

/// Clear, wallpaper (own pass: mixing element types in one list needs
/// DMA import bounds this backend does not satisfy), then the scene.
/// What one frame redraws, and at which scale.
#[derive(Debug, Clone, Copy)]
struct Target<'a> {
    /// The whole frame in physical pixels (rotated outputs swap its size).
    output: Rectangle<i32, smithay::utils::Physical>,
    damage: &'a [Rectangle<i32, smithay::utils::Physical>],
    scale: f64,
    blank_alpha: f32,
    /// Scene wellbeing effect and output temperature; captures use unity temperature.
    tint: Option<&'a FrameColor>,
}

fn draw_scene(
    frame: &mut smithay::backend::renderer::gles::GlesFrame<'_, '_>,
    background: Color32F,
    paper: &[smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement<
        GlesRenderer,
    >],
    scene: &Scene,
    target: Target<'_>,
    decor: &[(Color32F, Vec<Rectangle<i32, smithay::utils::Physical>>)],
    previews: &[PreviewElement],
) -> Result<crate::occlusion::Drawn, RuntimeError> {
    use crate::occlusion::{draw_planned, Drawn, Occlusion};
    use smithay::backend::renderer::element::{solid::SolidColorRenderElement, Id, Kind};
    let Target {
        output,
        damage,
        scale,
        blank_alpha,
        tint,
    } = target;
    let recolor = |color| tint.map_or(color, |tint| tint.color(color));
    let background = recolor(background);
    let recolor_solid =
        |element: &smithay::backend::renderer::element::solid::SolidColorRenderElement| {
            use smithay::backend::renderer::element::Element;
            SolidColorRenderElement::new(
                element.id().clone(),
                element.geometry(1.0.into()),
                element.current_commit(),
                recolor(element.color()),
                element.kind(),
            )
        };
    // The usual neutral frame borrows its solids without an extra allocation.
    let colored_tile = tint.map(|_| scene.tile.iter().map(recolor_solid).collect::<Vec<_>>());
    let tile = colored_tile.as_deref().unwrap_or(&scene.tile);
    let colored_flashes = tint.map(|_| scene.flashes.iter().map(recolor_solid).collect::<Vec<_>>());
    let flashes = colored_flashes.as_deref().unwrap_or(&scene.flashes);
    // Install the tint program around one texture group; a no-op
    // without a tint, cleared right after like the modal-dim override.
    let set_tint = |frame: &mut smithay::backend::renderer::gles::GlesFrame<'_, '_>,
                    brightness: f32| {
        if let Some(tint) = tint {
            frame.override_default_tex_program(tint.program.clone(), tint.uniforms(brightness));
        }
    };
    let error =
        |e: smithay::backend::renderer::gles::GlesError| RuntimeError::Dispatch(e.to_string());
    // Plan top to bottom (#503): each element repaints only damage that
    // no opaque element above it covers, so stacked windows and the
    // wallpaper under an opaque window are skipped.
    let mut occlusion = Occlusion::new(output, damage);
    thread_local! { static SHIELD: Id = Id::new(); }
    // The blank covers the whole output; only its damage is repainted.
    let shield = (blank_alpha > 0.0).then(|| {
        SolidColorRenderElement::new(
            SHIELD.with(Clone::clone),
            output,
            0usize,
            Color32F::new(0.0, 0.0, 0.0, blank_alpha),
            Kind::Unspecified,
        )
    });
    let shield_plan = occlusion.plan(1.0, shield.as_slice());
    let flash_plan = occlusion.plan(1.0, &scene.flashes);
    let effect_solids_plan = occlusion.plan(1.0, &scene.effects.solids);
    let effect_textures_plan = occlusion.plan(1.0, &scene.effects.textures);
    let fullscreen_bell: Vec<_> = scene
        .bell
        .iter()
        .filter(|(at, _)| at.is_none())
        .map(|(_, e)| recolor_solid(e))
        .collect();
    let bell_plan = occlusion.plan(1.0, &fullscreen_bell);
    let top_plan = occlusion.plan(scale, &scene.top);
    // Beneath the tile preview, the preview (blended), then the
    // dragged window and the layers over it, each half with the
    // size-change snapshots among its elements (#496).
    let split = scene.above.min(scene.elements.len());
    let snapshot_at: Vec<usize> = scene.snapshots.iter().map(|(index, _, _)| *index).collect();
    let mut over = scene_slots(split, 0, false, &snapshot_at);
    let mut under = scene_slots(scene.elements.len() - split, split, true, &snapshot_at);
    insert_bell_slots(&mut over, scene);
    insert_bell_slots(&mut under, scene);
    let (over_plan, over_shown) = plan_slots(&mut occlusion, scale, scene, &over);
    let tile_plan = occlusion.plan(1.0, &scene.tile);
    let (under_plan, under_shown) = plan_slots(&mut occlusion, scale, scene, &under);
    let previews_plan = occlusion.plan(scale, previews);
    // Solid shapes under the surfaces (overview cards, hover ring).
    let decor_plan: Vec<_> = decor
        .iter()
        .rev()
        .map(|(color, rects)| occlusion.visit_fill(rects, color.is_opaque()))
        .collect();
    let paper_plan = occlusion.plan(1.0, paper);
    let clear = occlusion.uncovered();
    if !clear.is_empty() {
        frame.clear(background, &clear).map_err(error)?;
    }
    set_tint(frame, 1.0);
    draw_planned::<GlesRenderer, _>(frame, 1.0, paper, &paper_plan).map_err(error)?;
    frame.clear_tex_program_override();
    for ((color, _), rects) in decor.iter().zip(decor_plan.iter().rev()) {
        if !rects.is_empty() {
            frame.clear(recolor(*color), rects).map_err(error)?;
        }
    }
    // Surfaces carry logical sizes: draw them at the output scale. The
    // wallpaper above is already sized in physical pixels.
    set_tint(frame, 1.0);
    draw_planned::<GlesRenderer, _>(frame, scale, previews, &previews_plan).map_err(error)?;
    frame.clear_tex_program_override();
    draw_slots(frame, scale, scene, &under, &under_plan, tint).map_err(error)?;
    draw_planned::<GlesRenderer, _>(frame, 1.0, tile, &tile_plan).map_err(error)?;
    draw_slots(frame, scale, scene, &over, &over_plan, tint).map_err(error)?;
    set_tint(frame, 1.0);
    draw_planned::<GlesRenderer, _>(frame, scale, &scene.top, &top_plan).map_err(error)?;
    frame.clear_tex_program_override();
    draw_planned::<GlesRenderer, _>(frame, 1.0, &fullscreen_bell, &bell_plan).map_err(error)?;
    draw_planned::<GlesRenderer, _>(frame, 1.0, &scene.effects.textures, &effect_textures_plan)
        .map_err(error)?;
    draw_planned::<GlesRenderer, _>(frame, 1.0, &scene.effects.solids, &effect_solids_plan)
        .map_err(error)?;
    draw_planned::<GlesRenderer, _>(frame, 1.0, flashes, &flash_plan).map_err(error)?;
    draw_planned::<GlesRenderer, _>(frame, 1.0, shield.as_slice(), &shield_plan).map_err(error)?;
    let mut scene_visible = vec![false; scene.elements.len()];
    for (slot, shown) in over
        .iter()
        .zip(&over_shown)
        .chain(under.iter().zip(&under_shown))
    {
        if let SceneSlot::Element(index) = slot {
            scene_visible[*index] = *shown;
        }
    }
    Ok(Drawn {
        rendered: occlusion.rendered(),
        culled: occlusion.culled(),
        scene_visible,
    })
}

/// The screen magnifier's offscreen passes (#349): the unmagnified
/// scene (`draw` paints it at full damage) plus the locate-pointer
/// ripples into the scene texture, then the zoom region with the
/// pointer sprite magnified at `scale × factor`. Returns both textures
/// for [`crate::magnifier::present`] and the scene pass's report.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn zoom_passes(
    renderer: &mut GlesRenderer,
    targets: &mut crate::magnifier::ZoomTargets,
    cursor: &mut crate::cursor::Cursor,
    output: &str,
    plan: &crate::magnifier::ZoomPlan,
    pointer: &crate::cursor::Pointer,
    scale: f64,
    now: Duration,
    ripples: &crate::cursor::CursorElements,
    draw: impl FnOnce(
        &mut smithay::backend::renderer::gles::GlesFrame<'_, '_>,
        &[Rectangle<i32, smithay::utils::Physical>],
    ) -> Result<crate::occlusion::Drawn, RuntimeError>,
) -> Result<
    (
        smithay::backend::renderer::gles::GlesTexture,
        smithay::backend::renderer::gles::GlesTexture,
        crate::occlusion::Drawn,
    ),
    RuntimeError,
> {
    let error =
        |e: smithay::backend::renderer::gles::GlesError| RuntimeError::Dispatch(e.to_string());
    let (mut scene, mut zoom) = targets.textures(renderer, output, plan).map_err(error)?;
    let full: Rectangle<i32, smithay::utils::Physical> =
        Rectangle::from_size((plan.output.0, plan.output.1).into());
    let drawn = {
        let mut target = renderer.bind(&mut scene).map_err(error)?;
        let mut frame = renderer
            .render(&mut target, full.size, Transform::Normal)
            .map_err(error)?;
        let drawn = draw(&mut frame, &[full])?;
        crate::cursor::draw_ripples(&mut frame, ripples, &[full]).map_err(error)?;
        let _ = frame.finish().map_err(error)?;
        drawn
    };
    let magnified = scale * plan.view.mag;
    let sprite = (!matches!(pointer, crate::cursor::Pointer::Hidden)).then(|| {
        let hotspot = (
            plan.sprite.0 - f64::from(plan.content.0),
            plan.sprite.1 - f64::from(plan.content.1),
        );
        cursor.sprite_at(renderer, pointer, hotspot.into(), magnified, now)
    });
    crate::magnifier::compose_zoom(
        renderer,
        &scene,
        &mut zoom,
        plan,
        sprite.as_ref().map(|sprite| (sprite, magnified)),
    )
    .map_err(error)?;
    Ok((scene, zoom, drawn))
}

/// One drawn item of the scene's window/layer pass: a surface element or
/// a size-change snapshot (#496), by index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SceneSlot {
    Element(usize),
    Snapshot(usize),
    Bell(usize),
}

/// The scene's `[base..base + len]` elements front to back, with the
/// size-change snapshots that sit among them: a snapshot at index `i`
/// draws in front of `elements[i..]`. One at the slice's end belongs to
/// it only when `through_end` (the two halves around the tile preview
/// share that boundary). Of snapshots at one index, the later is in front.
fn scene_slots(
    len: usize,
    base: usize,
    through_end: bool,
    snapshot_at: &[usize],
) -> Vec<SceneSlot> {
    let mut slots = Vec::with_capacity(len + snapshot_at.len());
    for at in 0..=len {
        if at < len || through_end {
            slots.extend(
                snapshot_at
                    .iter()
                    .enumerate()
                    .rev()
                    .filter(|(_, index)| **index == base + at)
                    .map(|(k, _)| SceneSlot::Snapshot(k)),
            );
        }
        if at < len {
            slots.push(SceneSlot::Element(base + at));
        }
    }
    slots
}

/// Each slot's repaint plan, and whether it shows on the output.
fn plan_slots(
    occlusion: &mut crate::occlusion::Occlusion,
    scale: f64,
    scene: &Scene,
    slots: &[SceneSlot],
) -> (Vec<Vec<crate::occlusion::PhysicalRect>>, Vec<bool>) {
    slots
        .iter()
        .map(|slot| {
            let plan = match *slot {
                SceneSlot::Element(index) => occlusion
                    .plan(scale, std::slice::from_ref(&scene.elements[index]))
                    .remove(0),
                SceneSlot::Bell(k) => occlusion
                    .plan(1.0, std::slice::from_ref(&scene.bell[k].1))
                    .remove(0),
                SceneSlot::Snapshot(k) => occlusion
                    .plan(1.0, std::slice::from_ref(&scene.snapshots[k].1))
                    .remove(0),
            };
            (plan, occlusion.shown())
        })
        .unzip()
}

fn draw_slots(
    frame: &mut smithay::backend::renderer::gles::GlesFrame<'_, '_>,
    scale: f64,
    scene: &Scene,
    slots: &[SceneSlot],
    plan: &[Vec<Rectangle<i32, smithay::utils::Physical>>],
    tint: Option<&FrameColor>,
) -> Result<(), smithay::backend::renderer::gles::GlesError> {
    use crate::occlusion::draw_planned;
    for (slot, visible) in slots.iter().zip(plan).rev() {
        let visible = std::slice::from_ref(visible);
        let brightness = match *slot {
            SceneSlot::Element(index) => scene.brightness[index],
            SceneSlot::Snapshot(k) => scene.snapshots[k].2,
            SceneSlot::Bell(_) => 1.0,
        };
        // A tinted frame draws every texture through the tint
        // program (brightness folds into the same override);
        // otherwise only dimmed elements take an override.
        if let Some(tint) = tint {
            frame.override_default_tex_program(tint.program.clone(), tint.uniforms(brightness));
        } else if brightness < 1.0 {
            if let Some(program) = &scene.dim_program {
                frame.override_default_tex_program(
                    program.clone(),
                    vec![smithay::backend::renderer::gles::Uniform::new(
                        "brightness",
                        brightness,
                    )],
                );
            }
        }
        let result = match *slot {
            SceneSlot::Element(index) => draw_planned::<GlesRenderer, _>(
                frame,
                scale,
                std::slice::from_ref(&scene.elements[index]),
                visible,
            ),
            SceneSlot::Bell(k) => draw_planned::<GlesRenderer, _>(
                frame,
                1.0,
                std::slice::from_ref(&{
                    use smithay::backend::renderer::element::{
                        solid::SolidColorRenderElement, Element,
                    };
                    let e = &scene.bell[k].1;
                    SolidColorRenderElement::new(
                        e.id().clone(),
                        e.geometry(1.0.into()),
                        e.current_commit(),
                        tint.map_or(e.color(), |tint| tint.color(e.color())),
                        e.kind(),
                    )
                }),
                visible,
            ),
            SceneSlot::Snapshot(k) => draw_planned::<GlesRenderer, _>(
                frame,
                1.0,
                std::slice::from_ref(&scene.snapshots[k].1),
                visible,
            ),
        };
        frame.clear_tex_program_override();
        result?;
    }
    Ok(())
}

/// Frame callbacks for every client surface. Provisional pacing: always
/// send (zero throttle) with the primary output as scan-out; per-output
/// callbacks arrive with damage tracking.
/// Tell every window and layer surface the scale of the output it is on
/// (preferred buffer scale and fractional scale), so on mixed-DPI setups
/// each renders sharp for its own monitor. Smithay only sends changes.
/// niri's `send_scale_transform` shape (#59).
fn send_surface_scales(state: &State, manager: &WindowManager) {
    use smithay::wayland::compositor::{
        send_surface_state, with_surface_tree_downward, TraversalAction,
    };
    use smithay::wayland::fractional_scale::with_fractional_scale;
    let send = |surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
                scale: smithay::output::Scale| {
        with_surface_tree_downward(
            surface,
            (),
            |_, _, _| TraversalAction::DoChildren(()),
            |surface, data, _| {
                send_surface_state(surface, data, scale.integer_scale(), Transform::Normal);
                with_fractional_scale(data, |fractional| {
                    fractional.set_preferred_scale(scale.fractional_scale());
                });
            },
            |_, _, _| true,
        );
    };
    for (window, geometry) in manager.visible_windows() {
        if let Some(surface) = window.wl_surface() {
            send(&surface, state.scale_for(geometry));
        }
    }
    for (surface, (x, y), _) in crate::layer::layer_layout(state) {
        send(
            &surface,
            state.scale_for(Rectangle::new((x, y).into(), (1, 1).into())),
        );
    }
}

/// Root surfaces of the managed windows nothing on screen shows (#503),
/// whose frame callbacks drop to [`HIDDEN_FRAME_INTERVAL`](crate::occlusion::HIDDEN_FRAME_INTERVAL).
/// Empty while window clones or casts show every window. A toplevel the
/// window manager does not track (not yet mapped, or not a managed
/// window) is never throttled.
fn hidden_surfaces(
    manager: &WindowManager,
    costs: &std::collections::BTreeMap<String, FrameCost>,
    clones_shown: bool,
) -> Vec<smithay::reexports::wayland_server::protocol::wl_surface::WlSurface> {
    if clones_shown {
        return Vec::new();
    }
    let scene: Vec<u64> = manager
        .render_entries()
        .iter()
        .map(|(id, _, _)| *id)
        .collect();
    let shown = crate::occlusion::shown_windows(&scene, costs.values());
    manager
        .window_ids()
        .filter(|id| !shown.contains(id))
        .filter_map(|id| manager.surface_of(id))
        .collect()
}

/// Whether a toplevel has configures it has not yet acknowledged. Clients
/// may wait on a frame callback before drawing (and acking) the new
/// state; like Mutter's flush after a configure, they keep full rate.
fn awaiting_configure(toplevel: &smithay::wayland::shell::xdg::ToplevelSurface) -> bool {
    smithay::wayland::compositor::with_states(toplevel.wl_surface(), |states| {
        states
            .data_map
            .get::<smithay::wayland::shell::xdg::XdgToplevelSurfaceData>()
            .and_then(|data| data.lock().ok().map(|d| !d.pending_configures().is_empty()))
            .unwrap_or(false)
    })
}

/// Hidden windows' root surfaces whose frame callbacks wait for the
/// hidden beat (#503). A window still acknowledging a configure keeps
/// full rate: like Mutter's flush after a configure, it may wait on a
/// callback before drawing the new state.
fn throttled_surfaces(
    state: &State,
    hidden: &[smithay::reexports::wayland_server::protocol::wl_surface::WlSurface],
) -> Vec<smithay::reexports::wayland_server::protocol::wl_surface::WlSurface> {
    if hidden.is_empty() {
        return Vec::new();
    }
    state
        .toplevels()
        .iter()
        .filter(|toplevel| hidden.contains(toplevel.wl_surface()) && !awaiting_configure(toplevel))
        .map(|toplevel| toplevel.wl_surface().clone())
        .collect()
}

/// Frame callbacks for everything a scanout shows. `throttled` surfaces
/// are skipped: [`Runtime::hidden_frame_beat`] pays them.
fn send_frame_callbacks(
    state: &State,
    manager: &WindowManager,
    throttled: &[smithay::reexports::wayland_server::protocol::wl_surface::WlSurface],
) {
    // Frame callback time measures elapsed time, independently of refresh
    // rate, idle periods, or how many frames the backend has queued.
    let time = Duration::from(state.presentation_now());
    let Some(output) = state.primary_output() else {
        return;
    };
    for surface in state.toplevels() {
        if throttled.contains(surface.wl_surface()) {
            continue;
        }
        send_frames_surface_tree(
            surface.wl_surface(),
            &output,
            time,
            Some(Duration::ZERO),
            |_, _| Some(output.clone()),
        );
    }
    for (surface, _, _) in crate::layer::layer_layout(state) {
        crate::grid_trace::send_layer_frames(state, &surface, &output, time);
    }
    // A drag's icon animates on frame callbacks too.
    if let Some(icon) = state.dnd_icon() {
        send_frames_surface_tree(&icon, &output, time, Some(Duration::ZERO), |_, _| {
            Some(output.clone())
        });
    }
    // The lock screen (ext-session-lock) paints on frame callbacks too.
    for surface in state.lock_surfaces() {
        send_frames_surface_tree(&surface, &output, time, Some(Duration::ZERO), |_, _| {
            Some(output.clone())
        });
    }
    // Popups commit on frame callbacks like any surface (#88).
    let parents = state
        .toplevels()
        .iter()
        .map(|t| t.wl_surface().clone())
        .chain(
            crate::layer::layer_layout(state)
                .into_iter()
                .map(|(s, _, _)| s),
        )
        .collect::<Vec<_>>();
    for parent in parents {
        for (popup, _) in smithay::desktop::PopupManager::popups_for_surface(&parent) {
            send_frames_surface_tree(
                popup.wl_surface(),
                &output,
                time,
                Some(Duration::ZERO),
                |_, _| Some(output.clone()),
            );
        }
    }
    // X11 windows are not xdg toplevels, so the loop above never
    // reaches them; Xwayland waits on these callbacks before
    // committing content.
    #[cfg(feature = "xwayland")]
    for surface in manager.x11_surfaces() {
        send_frames_surface_tree(&surface, &output, time, Some(Duration::ZERO), |_, _| {
            Some(output.clone())
        });
    }
    #[cfg(not(feature = "xwayland"))]
    let _ = manager;
}

/// Run a nested session to shutdown: drive the loop and report run
/// statistics.
///
/// Environment hygiene ([`apply_nested_env`]) is applied only after the
/// backend exists: the winit backend prefers Wayland when
/// `WAYLAND_DISPLAY` is set, so setting it first would point winit at our
/// own not-yet-existing socket.
pub fn run(session: &NestedSession) -> Result<RunStats, RuntimeError> {
    run_loop(session, |_| None)
}

/// Narrow, trusted Rust-only CI driver. The normal binary supplies no probe.
/// No credential challenge, UI/configuration or IPC entry point is added.
pub struct AuthOwnerProbe<'a> {
    runtime: &'a mut Runtime,
}
#[derive(Clone, Copy, Debug)]
pub struct AuthOwnerStatus {
    pub locked: bool,
    pub epoch: u64,
    pub outer_pending: bool,
}
impl AuthOwnerProbe<'_> {
    pub fn status(&self) -> AuthOwnerStatus {
        AuthOwnerStatus {
            locked: self.runtime.is_locked(),
            epoch: self
                .runtime
                .unlock_generation
                .load(std::sync::atomic::Ordering::Acquire),
            outer_pending: self.runtime.unlock_worker.is_some(),
        }
    }
    /// Controlled epoch injection, not an observation of logind or S3.
    pub fn invalidate_epoch(&mut self) -> bool {
        crate::auth_owner::advance_epoch(&self.runtime.unlock_generation)
    }
    pub fn try_unlock(&mut self, user: &str, password: &str) -> bool {
        self.runtime.try_unlock(user, password)
    }
    pub fn request_exit(&mut self) {
        self.runtime.exit = true;
        self.runtime.wake.signal();
    }
}

/// Runs the identical runtime, input, IPC and owned-worker loop on the caller's
/// main thread. A deadline belongs solely to the trusted probe caller.
pub fn run_auth_owner_probe(
    session: &NestedSession,
    probe: impl FnMut(&mut AuthOwnerProbe<'_>) -> Option<Duration>,
) -> Result<RunStats, RuntimeError> {
    run_loop(session, probe)
}

fn run_loop(
    session: &NestedSession,
    mut probe: impl FnMut(&mut AuthOwnerProbe<'_>) -> Option<Duration>,
) -> Result<RunStats, RuntimeError> {
    let (mut runtime, mut event_loop) = Runtime::launch(session)?;
    let prev_env = apply_nested_env(&session.socket_name);
    #[cfg(feature = "drm")]
    let _session_services = if matches!(runtime.backend, Backend::Drm(_)) {
        Some(crate::session_services::publish(&session.socket_name))
    } else {
        None
    };
    #[cfg(feature = "drm")]
    let sleep_monitor = if matches!(runtime.backend, Backend::Drm(_)) {
        // The logind thread wakes the loop so lock-on-suspend and resume
        // are handled at once, not at the next timed wake.
        match crate::sleep::Monitor::start(runtime.unlock_generation.clone()) {
            Ok(monitor) => Some(monitor),
            Err(error) => {
                eprintln!("tuna-compositor: logind sleep monitor unavailable: {error}");
                None
            }
        }
    } else {
        None
    };
    #[cfg(feature = "drm")]
    if let Some(monitor) = &sleep_monitor {
        monitor.set_wake(&runtime.wake);
    }
    // The session's logind object, as GNOME's screen shield follows it:
    // Lock/Unlock signals in, LockedHint out (#846). Nested runs share
    // the host's session and leave it alone.
    #[cfg(feature = "drm")]
    let mut logind_session = if matches!(runtime.backend, Backend::Drm(_)) {
        match crate::logind_session::Session::start() {
            Ok(session) => Some(session),
            Err(error) => {
                eprintln!("tuna-compositor: logind session unavailable: {error}");
                None
            }
        }
    } else {
        None
    };
    #[cfg(feature = "drm")]
    if let Some(session) = &logind_session {
        session.set_wake(&runtime.wake);
    }
    // Graceful shutdown on SIGTERM/SIGINT/SIGHUP: ending the loop drops the
    // runtime, whose supervisor kills the shell child (ADR 0003 kill
    // on exit). Without this a signal would bypass `Drop` and orphan
    // the shell on a dead socket. logind creates session scopes with
    // SendSIGHUP=yes, so a session stop sends SIGHUP right after SIGTERM;
    // left at its default it killed the compositor before teardown, so the
    // owned Orca was never retired (#836). The flag write is the only work
    // done in the handler; the loop observes it below.
    let shutdown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    for signal in [
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGHUP,
    ] {
        signal_hook::flag::register(signal, std::sync::Arc::clone(&shutdown))
            .map_err(|e| RuntimeError::Loop(format!("signal handler failed: {e}")))?;
    }
    // The same signals, and child exits for the shell, IME and XWayland
    // supervisors, wake a loop that may be sleeping (#503).
    let signals = crate::wake::signal_source(&[
        signal_hook::consts::SIGCHLD,
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGHUP,
    ])
    .map_err(|e| RuntimeError::Loop(format!("signal source failed: {e}")))?;
    event_loop
        .handle()
        .insert_source(signals, |_, stream, _: &mut Runtime| {
            crate::wake::drain_signals(stream.as_ref());
            Ok(calloop::PostAction::Continue)
        })
        .map_err(|e| RuntimeError::Loop(e.to_string()))?;
    let mut wait = Duration::ZERO;
    loop {
        if shutdown.load(std::sync::atomic::Ordering::Relaxed) {
            runtime.shutdown_colord();
            restore_env(prev_env);
            return Ok(runtime.stats);
        }
        let dispatched = event_loop
            .dispatch(Some(wait), &mut runtime)
            .map_err(|e| RuntimeError::Loop(e.to_string()));
        #[cfg(feature = "drm")]
        if let Some(monitor) = &sleep_monitor {
            let (lock, wake) = monitor.take();
            if lock {
                runtime.engage_lock();
            }
            if wake {
                // Lock is compositor-owned even if the supervised shell died.
                runtime.engage_lock();
                if let Backend::Drm(drm) = &mut runtime.backend {
                    drm.resume_from_sleep();
                }
            }
        }
        #[cfg(feature = "drm")]
        match logind_session.as_ref().and_then(|session| session.take()) {
            Some(crate::logind_session::Request::Lock) => runtime.lock_for_logind(),
            Some(crate::logind_session::Request::Unlock) => runtime.unlock_for_logind(),
            None => {}
        }
        let ticked = dispatched.and_then(|_| runtime.tick());
        #[cfg(feature = "drm")]
        if let Some(session) = &mut logind_session {
            session.report_locked(runtime.is_locked());
        }
        let probe_delay = probe(&mut AuthOwnerProbe {
            runtime: &mut runtime,
        });
        match ticked {
            Ok(true) => {
                wait = runtime.next_wake();
                if let Some(delay) = probe_delay {
                    wait =
                        wait.min(delay.clamp(Duration::from_millis(1), Duration::from_millis(100)));
                }
                continue;
            }
            done => {
                runtime.shutdown_colord();
                restore_env(prev_env);
                return done.map(|_| runtime.stats);
            }
        }
    }
}

/// Input-event timestamp in the backend event base, for the session-lock
/// idle accumulator. Every [`ManagerInput`] variant carries one; axis
/// scroll counts as activity like any other event.
fn input_time(input: &ManagerInput) -> u64 {
    match *input {
        ManagerInput::Key { time, .. }
        | ManagerInput::Motion { time, .. }
        | ManagerInput::CornerPressure { time, .. }
        | ManagerInput::Button { time, .. }
        | ManagerInput::Axis { time, .. }
        | ManagerInput::SwipeBegin { time, .. }
        | ManagerInput::SwipeUpdate { time, .. }
        | ManagerInput::SwipeEnd { time, .. } => u64::from(time),
        ManagerInput::RelativeMotion { utime, .. } => utime / 1000,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn unlock_outcome_reports_only_what_the_worker_returned() {
        // S3 cancellation: the cancelled verification is denied and failed.
        assert_eq!(unlock_outcome(false, true), (false, false));
        assert_eq!(unlock_outcome(false, false), (false, false));
        // Superseded epoch: the worker succeeded, but a denied attempt never applies.
        assert_eq!(unlock_outcome(true, true), (true, false));
        assert_eq!(unlock_outcome(true, false), (true, true));
    }

    #[test]
    fn switcher_thumbnails_draw_their_window_at_the_frames_opacity() {
        let thumbnail = |opacity| tuna_shell_control::SwitcherThumbnail {
            window: 3,
            x: 100,
            y: 50,
            width: 256,
            height: 160,
            opacity,
        };
        let size = (1280, 800).into();
        // Half way through a fade (Clutter's 0-255 opacity): half alpha.
        let half = switcher_preview(&thumbnail(128), size);
        assert_eq!(half.alpha, 128.0 / 255.0);
        assert!((half.alpha - 0.5).abs() <= 1.0 / 255.0);
        assert_eq!(switcher_preview(&thumbnail(255), size).alpha, 1.0);
        assert_eq!(switcher_preview(&thumbnail(0), size).alpha, 0.0);
        // The fit is unchanged: scaled down to the frame, centred in it.
        assert_eq!(half.scale, 0.2);
        assert_eq!(
            half.rect,
            Rectangle::new((100, 50).into(), (256, 160).into())
        );
    }

    /// A preview record must ping an idle loop through the share's
    /// connected wake (#339): real calloop, no sleeps — dispatch
    /// returns on the ping itself.
    #[test]
    fn night_light_preview_record_wakes_idle_loop() {
        let (wake, source) = crate::wake::Wake::new().unwrap();
        let share = std::sync::Arc::new(crate::night_light::NightLightShare::default());
        share.set_wake(&wake);
        let mut loop_: EventLoop<usize> = EventLoop::try_new().unwrap();
        loop_
            .handle()
            .insert_source(source, |_, _, count| *count += 1)
            .unwrap();
        let mut count = 0;
        share.set_preview(2000, 5).unwrap();
        loop_
            .dispatch(Some(Duration::from_secs(2)), &mut count)
            .unwrap();
        assert_eq!(count, 1, "preview record must ping an idle loop");
        // No spin: a zero-wait dispatch with no new records observes
        // nothing further.
        loop_.dispatch(Some(Duration::ZERO), &mut count).unwrap();
        assert_eq!(count, 1);
    }

    /// Clear and temp-disable records carry the same wake
    /// obligation as preview records (#339).
    #[test]
    fn night_light_clear_and_temp_disable_wake_idle_loop() {
        let (wake, source) = crate::wake::Wake::new().unwrap();
        let share = std::sync::Arc::new(crate::night_light::NightLightShare::default());
        share.set_wake(&wake);
        let mut loop_: EventLoop<usize> = EventLoop::try_new().unwrap();
        loop_
            .handle()
            .insert_source(source, |_, _, count| *count += 1)
            .unwrap();
        let mut count = 0;
        share.set_preview(2000, 5).unwrap();
        loop_
            .dispatch(Some(Duration::from_secs(2)), &mut count)
            .unwrap();
        assert_eq!(count, 1);
        share.clear_preview();
        loop_
            .dispatch(Some(Duration::from_secs(2)), &mut count)
            .unwrap();
        assert_eq!(
            count, 2,
            "clear must provide its own wake after preview was drained"
        );
        loop_.dispatch(Some(Duration::ZERO), &mut count).unwrap();
        assert_eq!(count, 2, "no repeated clear wake");
        share.set_disabled_until_tmw(Some(1_700_000_000));
        loop_
            .dispatch(Some(Duration::from_secs(2)), &mut count)
            .unwrap();
        assert_eq!(count, 3, "temp-disable set must ping");
        share.set_disabled_until_tmw(None);
        loop_
            .dispatch(Some(Duration::from_secs(2)), &mut count)
            .unwrap();
        assert_eq!(count, 4, "temp-disable clear must ping");
    }

    /// Without `set_wake` the share still records (fallback path),
    /// but nothing is pinged: wiring the setup hunk is what makes
    /// an idle loop responsive (#339).
    #[test]
    fn night_light_unconnected_share_records_without_ping() {
        let (_wake, source) = crate::wake::Wake::new().unwrap();
        let share = crate::night_light::NightLightShare::default();
        let mut loop_: EventLoop<usize> = EventLoop::try_new().unwrap();
        loop_
            .handle()
            .insert_source(source, |_, _, count| *count += 1)
            .unwrap();
        let mut count = 0;
        share.set_preview(2000, 5).unwrap();
        loop_.dispatch(Some(Duration::ZERO), &mut count).unwrap();
        assert_eq!(count, 0, "unconnected share must not ping");
        assert!(share
            .preview_temperature(std::time::Instant::now())
            .is_some());
    }
}
