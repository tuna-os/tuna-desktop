//! On-demand XWayland server supervision.
//!
//! The supervisor owns the compatibility-server lifecycle behind a small
//! readiness state machine (`Idle → Pending → Ready | Absent`). Spawning
//! is on demand: nothing is attempted until [`XWaylandSupervisor::request`]
//! records the first X11 need, and a missing server binary settles on
//! [`Readiness::Absent`] — a logged, non-fatal report with the native
//! session untouched.
//!
//! The actual spawn is injected as a closure so the state machine stays
//! testable without an X server: unit tests drive [`XWaylandSupervisor::tick`]
//! with fake spawners. The Smithay `XWayland::spawn` wiring lands with the
//! window-model join and installs its spawner there; until then the runtime
//! polls with no spawner and the supervisor rests in `Pending`.
//!
//! # Server-side frames (P-WM-10)
//!
//! X11 windows that want window-manager decorations get them the way
//! Mutter 51 gives them: a separate X11 client (the frames client, see
//! `crate::x11_frames`) draws a GTK header bar in a frame window, and
//! the window manager reparents the application's window into it, so the
//! title bar is part of the window's own Wayland surface. This module
//! holds the compositor's share as pure decisions:
//!
//! - `x11_wants_frame`: which windows are decorated, from Mutter's
//!   `src/core/window.c` `meta_window_recalc_features` and
//!   `meta_window_get_frame_type` with `src/x11/window-props.c`
//!   `reload_mwm_hints`.
//! - `X11Frame`: the frame, buffer and client rectangles, from
//!   `src/x11/window-x11.c` `meta_window_x11_move_resize_internal`,
//!   `meta_window_frame_rect_to_client_rect` and
//!   `meta_window_client_rect_to_frame_rect`.
//!
//! The X11 protocol itself (`_MUTTER_NEEDS_FRAME`, `_MUTTER_FRAME_FOR`,
//! `_MUTTER_FRAME_EXTENTS`, `_GTK_FRAME_EXTENTS`, `_NET_FRAME_EXTENTS`,
//! adoption and loss of a frame) is in the vendored Smithay window
//! manager (`third-party/smithay/ROOST-PATCH.md`). Requests the frame's
//! header bar makes (`_NET_WM_MOVERESIZE`, `_NET_WM_STATE`,
//! `WM_CHANGE_STATE`, `_GTK_SHOW_WINDOW_MENU`) arrive here as
//! `X11ManagerEvent`s for the client window and run the same manager
//! actions as a native window's.
//!
//! Divergences from Mutter, all because Smithay's window manager keeps an
//! unframed window in a bare wrapper instead of on the root window:
//!
//! - Mutter shows a decorated window only once its frame exists. Here the
//!   map waits for the frame for `X11_FRAME_WAIT` at most, and only
//!   while a frames client runs; otherwise the window maps bare, exactly
//!   as before this module knew of frames, and is framed if a frame
//!   arrives later.
//! - When the frames client dies Mutter unmanages and re-manages the
//!   windows the X server hands back. Here they keep their place and
//!   identity, bare, and are offered to the restarted frames client.

use crate::supervise::RestartPolicy;

/// Why no compatibility server is available. Non-fatal by contract: the
/// session keeps running native windows either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbsentReason {
    /// The `xwayland` cargo feature is off or no spawner is installed,
    /// so starting was never possible in this build.
    Unsupported,
    /// The server binary is missing or exited during startup.
    SpawnFailed,
}

/// What [`XWaylandSupervisor::readiness`] reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Readiness {
    /// Never requested: no X11 need has been recorded.
    Idle,
    /// Requested and awaiting a spawn attempt (or its backoff delay).
    Pending,
    /// The server is up on the given display number.
    Ready {
        /// X11 display number, for the log line and future `DISPLAY`.
        display: u32,
    },
    /// Settled without a server. The report is logged exactly once.
    Absent {
        /// Why the server is unavailable.
        reason: AbsentReason,
    },
}

/// On-demand supervisor for the XWayland server.
///
/// Matches the [`crate::supervise`] shape: bounded attempts
/// (`1 + max_attempts` spawns, then settle), capped exponential backoff
/// between retries, and exactly one log line per terminal transition.
#[derive(Debug)]
pub struct XWaylandSupervisor {
    policy: RestartPolicy,
    requested: bool,
    readiness: Readiness,
    /// Spawn attempts so far (initial spawn plus restarts).
    attempts: u32,
    /// Earliest `now_ms` for the next attempt (backoff gate).
    next_allowed_ms: u64,
    start_logged: bool,
    absent_logged: bool,
}

impl XWaylandSupervisor {
    /// Build an idle supervisor: three restarts after the initial spawn,
    /// 500 ms base backoff capped at 5 s.
    pub fn new() -> Self {
        Self {
            policy: RestartPolicy::new(3, 500, 5_000),
            requested: false,
            readiness: Readiness::Idle,
            attempts: 0,
            next_allowed_ms: 0,
            start_logged: false,
            absent_logged: false,
        }
    }

    /// Record the first X11 need. Idempotent: later calls change nothing.
    /// Moves `Idle` to `Pending`; every other state is left alone.
    pub fn request(&mut self) {
        self.requested = true;
        if self.readiness == Readiness::Idle {
            self.readiness = Readiness::Pending;
        }
    }

    /// Resume bounded supervision after a running server disconnects.
    pub fn disconnected(&mut self, now_ms: u64) {
        if self.attempts > self.policy.max_attempts {
            self.readiness = Readiness::Absent {
                reason: AbsentReason::SpawnFailed,
            };
            if !self.absent_logged {
                self.absent_logged = true;
                eprintln!(
                    "tuna-compositor: xwayland: restart budget exhausted; native session continues"
                );
            }
        } else {
            self.readiness = Readiness::Pending;
            self.next_allowed_ms =
                now_ms.saturating_add(self.policy.next_delay(self.attempts.saturating_sub(1)));
        }
    }

    /// Current readiness. Never blocks and never spawns.
    pub fn readiness(&self) -> Readiness {
        self.readiness
    }

    /// Advance the state machine. Attempts a spawn when requested, the
    /// state is still pending, the backoff gate has passed, and a spawner
    /// is installed. With no spawner the call is a no-op and the
    /// supervisor stays pending — the runtime polls this way until the
    /// window-model join installs the real Smithay spawn.
    #[cfg(feature = "xwayland")]
    pub(crate) fn next_delay_ms(&self, now_ms: u64) -> Option<u64> {
        (self.requested && matches!(self.readiness, Readiness::Idle | Readiness::Pending))
            .then_some(self.next_allowed_ms.saturating_sub(now_ms))
    }
    pub fn tick(
        &mut self,
        now_ms: u64,
        spawn: Option<&mut dyn FnMut() -> Result<u32, AbsentReason>>,
    ) {
        if !self.requested {
            return;
        }
        match self.readiness {
            Readiness::Ready { .. } | Readiness::Absent { .. } => return,
            Readiness::Idle => {
                self.readiness = Readiness::Pending;
                return;
            }
            Readiness::Pending => {}
        }
        if now_ms < self.next_allowed_ms {
            return;
        }
        let spawn = match spawn {
            Some(spawn) => spawn,
            None => return,
        };
        self.attempts += 1;
        match spawn() {
            Ok(display) => {
                self.readiness = Readiness::Ready { display };
                if !self.start_logged {
                    self.start_logged = true;
                    eprintln!("tuna-compositor: xwayland: server started on :{display}");
                }
            }
            Err(reason) => {
                if self.attempts > self.policy.max_attempts {
                    self.readiness = Readiness::Absent { reason };
                    if !self.absent_logged {
                        self.absent_logged = true;
                        eprintln!(
                            "tuna-compositor: xwayland: unavailable ({reason:?}); native session continues"
                        );
                    }
                } else {
                    self.next_allowed_ms =
                        now_ms.saturating_add(self.policy.next_delay(self.attempts - 1));
                }
            }
        }
    }
}

impl Default for XWaylandSupervisor {
    fn default() -> Self {
        Self::new()
    }
}

/// Fallback title when an X11 window carries no title.
pub const UNTITLED_X11_TITLE: &str = "Untitled window";

/// Translate X11 window properties into the snapshot fields the shell
/// already consumes: title from the X11 title, app id from WM_CLASS
/// (class preferred, instance as tiebreak). Pure function with no
/// compositor access, so it reads like the existing snapshot-mapping
/// helpers. Empty properties resolve to the fixed fallbacks, never to
/// blanks. (xwayland feature only.)
#[cfg(feature = "xwayland")]
pub fn map_x11_identity(title: &str, class: &str, instance: &str) -> (String, Option<String>) {
    let title = if title.is_empty() {
        UNTITLED_X11_TITLE.to_owned()
    } else {
        title.to_owned()
    };
    let app_id = if !class.is_empty() {
        Some(class.to_owned())
    } else if !instance.is_empty() {
        Some(instance.to_owned())
    } else {
        None
    };
    (title, app_id)
}

/// How long a window that wants a frame waits for it before mapping bare.
///
/// Mutter waits without limit (`meta_window_x11_initialize_state` runs
/// only once the frame is set). The frames client answers within a few
/// frames once it runs; the bound keeps a window from never appearing
/// when it does not answer.
#[cfg(feature = "xwayland")]
pub const X11_FRAME_WAIT: std::time::Duration = std::time::Duration::from_millis(1500);

/// Whether an X11 window is decorated by the window manager, as Mutter
/// decides it in `meta_window_recalc_features`:
///
/// - "Use MWM hints initially": `_MOTIF_WM_HINTS` with the decorations
///   flag and no decoration at all (`reload_mwm_hints`) turns it off.
///   That is how GTK's client-side decorated windows opt out; Mutter has
///   no separate `_GTK_FRAME_EXTENTS` test.
/// - An override-redirect window, and a type whose
///   `meta_window_get_frame_type` is `META_FRAME_TYPE_LAST` (desktop,
///   dock, toolbar, splash screen, the menu and tooltip kinds,
///   notification, combo, DND), has no frame. Normal, dialog, menu and
///   utility windows have one; a window without a type is normal (or a
///   dialog when transient), both framed.
///
/// Fullscreen does not enter into it: "We leave fullscreen windows
/// decorated". The frame window carries the client's `_NET_WM_STATE`,
/// GTK hides the header bar of a fullscreen frame, and the frame's
/// extents drop to zero.
#[cfg(feature = "xwayland")]
pub fn x11_wants_frame(
    window_type: Option<smithay::xwayland::xwm::WmWindowType>,
    override_redirect: bool,
    motif_undecorated: bool,
) -> bool {
    use smithay::xwayland::xwm::WmWindowType;
    if override_redirect || motif_undecorated {
        return false;
    }
    matches!(
        window_type,
        None | Some(
            WmWindowType::Normal
                | WmWindowType::Dialog
                | WmWindowType::Menu
                | WmWindowType::Utility
        )
    )
}

/// The borders of an X11 window and whether a frame window holds them.
///
/// The window model keeps the *frame rectangle* (what the user sees as
/// the window, title bar included), like a native window's xdg geometry.
/// Smithay's `X11Surface::configure` takes the *buffer rectangle*: the X11
/// window carrying the Wayland surface, larger by the invisible borders
/// (shadows and resize handles). The application sizes its own *client*
/// window: inside the visible borders of a frame, or the whole buffer of
/// a window without one (a client-decorated window brings the invisible
/// borders itself, as `_GTK_FRAME_EXTENTS`).
#[cfg(feature = "xwayland")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct X11Frame {
    /// Visible and invisible borders around the frame rectangle.
    pub extents: smithay::xwayland::xwm::FrameExtents,
    /// Whether a frames-client frame holds the client.
    pub framed: bool,
}

#[cfg(feature = "xwayland")]
impl X11Frame {
    /// The current frame of `surface`.
    pub fn of(surface: &smithay::xwayland::X11Surface) -> Self {
        Self {
            extents: surface.frame_extents(),
            framed: surface.is_framed(),
        }
    }

    /// The buffer rectangle to configure for a frame rectangle.
    pub fn buffer_rect(
        &self,
        frame: smithay::utils::Rectangle<i32, smithay::utils::Logical>,
    ) -> smithay::utils::Rectangle<i32, smithay::utils::Logical> {
        self.extents.frame_rect_to_buffer_rect(frame)
    }

    /// The frame size that gives the application the client size it asked
    /// for (at map or in a `ConfigureRequest`):
    /// `meta_window_client_rect_to_frame_rect`.
    pub fn frame_size_for_client(
        &self,
        client: smithay::utils::Size<i32, smithay::utils::Logical>,
    ) -> smithay::utils::Size<i32, smithay::utils::Logical> {
        let rect = smithay::utils::Rectangle::from_size(client);
        if self.framed {
            self.extents.client_rect_to_frame_rect(rect).size
        } else {
            self.extents.buffer_rect_to_frame_rect(rect).size
        }
    }

    /// The client size inside a frame size:
    /// `meta_window_frame_rect_to_client_rect`.
    pub fn client_size(
        &self,
        frame: smithay::utils::Size<i32, smithay::utils::Logical>,
    ) -> smithay::utils::Size<i32, smithay::utils::Logical> {
        let rect = smithay::utils::Rectangle::from_size(frame);
        if self.framed {
            self.extents.frame_rect_to_client_rect(rect).size
        } else {
            self.extents.frame_rect_to_buffer_rect(rect).size
        }
    }

    /// The frame size of a window whose X11 window carrying the surface
    /// (what `X11Surface::geometry` reports) has `buffer` size. This is
    /// the size a window is placed with when it maps: its frame, adopted
    /// before the map is granted, already grew the buffer around the
    /// client size the application asked for.
    pub fn frame_size_in_buffer(
        &self,
        buffer: smithay::utils::Size<i32, smithay::utils::Logical>,
    ) -> smithay::utils::Size<i32, smithay::utils::Logical> {
        self.extents
            .buffer_rect_to_frame_rect(smithay::utils::Rectangle::from_size(buffer))
            .size
    }

    /// The client size inside a buffer of `buffer` size.
    pub fn client_size_in_buffer(
        &self,
        buffer: smithay::utils::Size<i32, smithay::utils::Logical>,
    ) -> smithay::utils::Size<i32, smithay::utils::Logical> {
        self.client_size(self.frame_size_in_buffer(buffer))
    }

    /// The buffer size that gives the application the client size it
    /// asked for.
    pub fn buffer_size_for_client(
        &self,
        client: smithay::utils::Size<i32, smithay::utils::Logical>,
    ) -> smithay::utils::Size<i32, smithay::utils::Logical> {
        self.buffer_rect(smithay::utils::Rectangle::from_size(
            self.frame_size_for_client(client),
        ))
        .size
    }

    /// The frame rectangle after the window went from `old` to this
    /// frame: the same origin, sized so the application keeps its client
    /// size (a floating window grows by the title bar when it is framed).
    pub fn reframed(
        &self,
        old: &X11Frame,
        frame: smithay::utils::Rectangle<i32, smithay::utils::Logical>,
    ) -> smithay::utils::Rectangle<i32, smithay::utils::Logical> {
        smithay::utils::Rectangle::new(
            frame.loc,
            self.frame_size_for_client(old.client_size(frame.size)),
        )
    }

    /// The frame rectangle inside a committed buffer of `buffer` size,
    /// relative to the surface: the X11 counterpart of xdg window
    /// geometry, by which every consumer of a window's surface places and
    /// sizes it (`meta_window_x11_surface_rect_to_frame_rect`). `None`
    /// when nothing is invisible and the surface is the window.
    pub fn window_geometry(
        &self,
        buffer: smithay::utils::Size<i32, smithay::utils::Logical>,
    ) -> Option<smithay::utils::Rectangle<i32, smithay::utils::Logical>> {
        (!self.extents.invisible.is_zero()).then(|| {
            self.extents
                .buffer_rect_to_frame_rect(smithay::utils::Rectangle::from_size(buffer))
        })
    }
}

/// X11 window-manager happenings the [`crate::State`] handlers queue
/// for the window manager's `reconcile` drain, so all scene mutation
/// stays on the one manager call path. (xwayland feature only.)
#[cfg(feature = "xwayland")]
#[derive(Debug, Clone)]
pub enum X11ManagerEvent {
    /// An X11 window asked to be mapped; the manager grants it with
    /// `set_mapped(true)`, at once or when its frame arrives. Boxed: the
    /// surface is ~270 bytes and dwarfs the other variants.
    MapRequest(Box<smithay::xwayland::X11Surface>),
    /// An X11 window was unmapped (X11 window id).
    Unmapped(u32),
    /// An X11 window was destroyed (X11 window id).
    Destroyed(u32),
    /// An X11 window asked for a new size; the manager applies what
    /// fits its geometry and advertises it back.
    ConfigureRequest {
        /// X11 window id.
        id: u32,
        /// The real surface is retained so pre-map requests can be acknowledged.
        surface: Box<smithay::xwayland::X11Surface>,
        /// Each requested dimension is independent in X11's value mask.
        width: Option<u32>,
        height: Option<u32>,
    },
    /// An X11 window property changed; the manager re-reads identity and
    /// the decoration decision.
    Property(u32),
    /// An X11 maximize/unmaximize request (X11 window id, maximize?).
    Maximize(u32, bool),
    /// An X11 fullscreen/unfullscreen request (X11 window id, on?).
    Fullscreen(u32, bool),
    /// `_NET_WM_MOVERESIZE` move: the title bar of the window's frame is
    /// being dragged (X11 window id).
    Move(u32),
    /// `_NET_WM_MOVERESIZE` resize from an edge of the frame.
    Resize(u32, smithay::xwayland::xwm::ResizeEdge),
    /// `WM_CHANGE_STATE` to iconic: the frame's minimize button.
    Minimize(u32),
    /// `_NET_CLOSE_WINDOW` (a pager; the frame's own close button sends
    /// `WM_DELETE_WINDOW` to the client itself).
    Close(u32),
    /// `_GTK_SHOW_WINDOW_MENU` at a global position: a secondary click on
    /// the frame's title bar.
    Menu(u32, i32, i32),
    /// The window's frame was adopted or lost, or its borders changed.
    FrameChanged(u32),
    /// An override-redirect window (a menu, a tooltip, a dropdown) was
    /// mapped by its client. The window manager has no say in it; the
    /// compositor draws it where it is (`crate::windows`, unmanaged).
    UnmanagedMapped(Box<smithay::xwayland::X11Surface>),
    /// A mapped override-redirect window moved, resized or restacked
    /// itself (X11 window id, the sibling it now sits directly above).
    UnmanagedConfigured(u32, Option<u32>),
}

/// Spawn the compatibility server and hook its event source into the
/// loop: on `Ready` the X11 window manager starts against the
/// privileged socket; on `Error` the failure is logged (the windows
/// simply never arrive). Returns the display number on a successful
/// spawn so the supervisor can report its single start line; a
/// missing binary (or any spawn failure) resolves to
/// [`AbsentReason::SpawnFailed`]. (xwayland feature only.)
#[cfg(feature = "xwayland")]
pub fn spawn_xwayland(
    dh: &smithay::reexports::wayland_server::DisplayHandle,
    loop_handle: &calloop::LoopHandle<'static, crate::runtime::Runtime>,
    pending_client: &mut Option<smithay::reexports::wayland_server::Client>,
    sockets: smithay::xwayland::XWaylandSockets,
    source_token: &mut Option<calloop::RegistrationToken>,
    active_client: &mut Option<smithay::reexports::wayland_server::Client>,
) -> Result<u32, AbsentReason> {
    use smithay::xwayland::XWayland;

    // `TUNA_XWAYLAND_DEBUG=1` inherits the server's stdio for
    // troubleshooting (focus routing, startup failures); default
    // stays silent.
    let debug = std::env::var("TUNA_XWAYLAND_DEBUG").is_ok_and(|value| value == "1");
    let stdio = || {
        if debug {
            std::process::Stdio::inherit()
        } else {
            std::process::Stdio::null()
        }
    };
    let envs: [(String, String); 0] = [];
    let (xwayland, client) = XWayland::spawn_on_sockets(
        dh,
        sockets,
        envs,
        stdio(),
        stdio(),
        // The server client carries Smithay's `XWaylandClientData`
        // (not our `ClientState`); `client_compositor_state` serves
        // both, so nothing is installed here.
        |_| {},
    )
    .map_err(|_| AbsentReason::SpawnFailed)?;
    let display = xwayland.display_number();
    *active_client = Some(client.clone());
    *pending_client = Some(client);
    let token = loop_handle
        .insert_source(
            xwayland,
            |event, _, runtime: &mut crate::runtime::Runtime| {
                runtime.on_xwayland_event(event);
            },
        )
        .map_err(|_| {
            *pending_client = None;
            *active_client = None;
            AbsentReason::SpawnFailed
        })?;
    *source_token = Some(token);
    Ok(display)
}

/// Handle one XWayland server event: start the X11 window manager on
/// `Ready`, log a non-fatal report on `Error`. Called from the
/// event-source callback with the runtime in scope.
/// (xwayland feature only.)
#[cfg(feature = "xwayland")]
pub fn on_xwayland_event(
    runtime: &mut crate::runtime::Runtime,
    event: smithay::xwayland::XWaylandEvent,
) {
    use smithay::xwayland::{X11Wm, XWaylandEvent};

    match event {
        XWaylandEvent::Ready {
            x11_socket,
            display_number,
        } => {
            let Some(client) = runtime.take_pending_x11_client() else {
                eprintln!("tuna-compositor: xwayland: ready with no pending client; ignoring");
                return;
            };
            match X11Wm::start_wm(runtime.loop_handle(), x11_socket, client) {
                Ok(xwm) => {
                    runtime.state_mut().xwm = Some(xwm);
                    runtime.set_x11_display(display_number);
                    crate::session_services::x11_display_ready(display_number);
                    eprintln!(
                        "tuna-compositor: xwayland: window manager started on :{display_number}"
                    );
                }
                Err(error) => {
                    runtime.x11_startup_failed();
                    eprintln!("tuna-compositor: xwayland: window manager failed: {error}");
                }
            }
        }
        XWaylandEvent::Error => {
            runtime.take_pending_x11_client();
            runtime.x11_startup_failed();
            eprintln!(
                "tuna-compositor: xwayland: server exited during startup; X11 windows unavailable"
            );
        }
    }
}

#[cfg(feature = "xwayland")]
mod handlers {
    use smithay::wayland::xwayland_shell::XWaylandShellHandler;
    use smithay::xwayland::xwm::{Reorder, X11Surface, X11Window, XwmHandler, XwmId};
    use smithay::xwayland::X11Wm;

    use super::X11ManagerEvent;
    use crate::runtime::Runtime;
    use crate::State;

    /// Queue one manager event on the protocol state.
    fn push(state: &mut State, event: X11ManagerEvent) {
        state.x11_events.push(event);
    }

    /// Shared map-request handling: queue it. The grant
    /// (`set_mapped(true)`), the decoration decision, placement and scene
    /// work stay in the manager's reconcile drain, which may hold the map
    /// until the window's frame arrives. A refusal is logged (id) so a
    /// silent window has a trail; the manager logs the grant.
    fn on_map_request(state: &mut State, window: X11Surface) {
        let id = window.window_id();
        if window.is_override_redirect() {
            eprintln!("tuna-compositor: xwayland: map refused (override-redirect) id={id}");
            return;
        }
        push(state, X11ManagerEvent::MapRequest(Box::new(window)));
    }

    /// Shared configure-request handling: placement stays
    /// manager-owned (x/y ignored); the requested size flows to the
    /// manager, which advertises its geometry back via configure.
    fn on_configure_request(state: &mut State, window: X11Surface, w: Option<u32>, h: Option<u32>) {
        push(
            state,
            X11ManagerEvent::ConfigureRequest {
                id: window.window_id(),
                surface: Box::new(window),
                width: w,
                height: h,
            },
        );
    }

    /// Association takes effect on commit; the manager reads the
    /// live `wl_surface()` every reconcile and re-asserts seat focus
    /// then, so no event is needed here.
    fn on_surface_associated() {}

    /// The running X11 window manager, or a panic naming the broken
    /// invariant (callbacks only fire while it runs).
    fn xwm_slot(state: &mut State) -> &mut X11Wm {
        state
            .xwm
            .as_mut()
            .expect("xwm callback without a running X11Wm")
    }

    fn state_of(state: &mut State) -> &mut State {
        state
    }

    fn state_of_runtime(runtime: &mut Runtime) -> &mut State {
        runtime.state_mut()
    }

    macro_rules! impl_shell_handler {
        ($target:ty, $state:expr) => {
            impl XWaylandShellHandler for $target {
                fn xwayland_shell_state(
                    &mut self,
                ) -> &mut smithay::wayland::xwayland_shell::XWaylandShellState
                {
                    &mut $state(self).xwayland_shell_state
                }

                fn surface_associated(
                    &mut self,
                    _xwm: XwmId,
                    _wl_surface: smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
                    _surface: X11Surface,
                ) {
                    on_surface_associated();
                }
            }
        };
    }

    macro_rules! impl_xwm_handler {
        ($target:ty, $state:expr) => {
            impl XwmHandler for $target {
                fn xwm_state(&mut self, _xwm: XwmId) -> &mut X11Wm {
                    xwm_slot($state(self))
                }

                fn new_window(&mut self, _xwm: XwmId, _window: X11Surface) {
                    // Unmapped yet: nothing to manage until map_window_request.
                }

                fn new_override_redirect_window(&mut self, _xwm: XwmId, _window: X11Surface) {
                    // Nothing to draw until it maps
                    // (mapped_override_redirect_window).
                }

                fn map_window_request(&mut self, _xwm: XwmId, window: X11Surface) {
                    on_map_request($state(self), window);
                }

                fn mapped_override_redirect_window(&mut self, _xwm: XwmId, window: X11Surface) {
                    push(
                        $state(self),
                        X11ManagerEvent::UnmanagedMapped(Box::new(window)),
                    );
                }

                fn unmapped_window(&mut self, _xwm: XwmId, window: X11Surface) {
                    push($state(self), X11ManagerEvent::Unmapped(window.window_id()));
                }

                fn destroyed_window(&mut self, _xwm: XwmId, window: X11Surface) {
                    push($state(self), X11ManagerEvent::Destroyed(window.window_id()));
                }

                fn configure_request(
                    &mut self,
                    _xwm: XwmId,
                    window: X11Surface,
                    _x: Option<i32>,
                    _y: Option<i32>,
                    w: Option<u32>,
                    h: Option<u32>,
                    _reorder: Option<Reorder>,
                ) {
                    on_configure_request($state(self), window, w, h);
                }

                fn configure_notify(
                    &mut self,
                    _xwm: XwmId,
                    window: X11Surface,
                    _geometry: smithay::utils::Rectangle<i32, smithay::utils::Logical>,
                    above: Option<X11Window>,
                ) {
                    // A managed window is where the manager put it. An
                    // override-redirect window moved, resized or restacked
                    // itself: its handle already has the new rectangle.
                    if window.is_override_redirect() {
                        push(
                            $state(self),
                            X11ManagerEvent::UnmanagedConfigured(window.window_id(), above),
                        );
                    }
                }

                fn resize_request(
                    &mut self,
                    _xwm: XwmId,
                    window: X11Surface,
                    _button: u32,
                    resize_edge: smithay::xwayland::xwm::ResizeEdge,
                ) {
                    push(
                        $state(self),
                        X11ManagerEvent::Resize(window.window_id(), resize_edge),
                    );
                }

                fn move_request(&mut self, _xwm: XwmId, window: X11Surface, _button: u32) {
                    push($state(self), X11ManagerEvent::Move(window.window_id()));
                }

                fn minimize_request(&mut self, _xwm: XwmId, window: X11Surface) {
                    push($state(self), X11ManagerEvent::Minimize(window.window_id()));
                }

                fn close_request(&mut self, _xwm: XwmId, window: X11Surface) {
                    push($state(self), X11ManagerEvent::Close(window.window_id()));
                }

                fn show_window_menu_request(
                    &mut self,
                    _xwm: XwmId,
                    window: X11Surface,
                    location: smithay::utils::Point<i32, smithay::utils::Logical>,
                ) {
                    push(
                        $state(self),
                        X11ManagerEvent::Menu(window.window_id(), location.x, location.y),
                    );
                }

                fn frame_changed(&mut self, _xwm: XwmId, window: X11Surface) {
                    push(
                        $state(self),
                        X11ManagerEvent::FrameChanged(window.window_id()),
                    );
                }

                fn property_notify(
                    &mut self,
                    _xwm: XwmId,
                    window: X11Surface,
                    _property: smithay::xwayland::xwm::WmWindowProperty,
                ) {
                    // Title/class may have changed; the manager
                    // re-reads identity on the next reconcile.
                    push($state(self), X11ManagerEvent::Property(window.window_id()));
                }

                fn maximize_request(&mut self, _xwm: XwmId, window: X11Surface) {
                    push(
                        $state(self),
                        X11ManagerEvent::Maximize(window.window_id(), true),
                    );
                }

                fn unmaximize_request(&mut self, _xwm: XwmId, window: X11Surface) {
                    push(
                        $state(self),
                        X11ManagerEvent::Maximize(window.window_id(), false),
                    );
                }

                fn fullscreen_request(&mut self, _xwm: XwmId, window: X11Surface) {
                    push(
                        $state(self),
                        X11ManagerEvent::Fullscreen(window.window_id(), true),
                    );
                }

                fn unfullscreen_request(&mut self, _xwm: XwmId, window: X11Surface) {
                    push(
                        $state(self),
                        X11ManagerEvent::Fullscreen(window.window_id(), false),
                    );
                }
            }
        };
    }

    // Protocol dispatch runs against `State` (the display data), while
    // `X11Wm::start_wm` binds the loop-data `Runtime`: both implement
    // the two handler traits over the same queue.
    impl_shell_handler!(State, state_of);
    impl_shell_handler!(Runtime, state_of_runtime);
    impl_xwm_handler!(State, state_of);
    impl_xwm_handler!(Runtime, state_of_runtime);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn fresh_supervisor_is_idle_and_tick_without_request_never_spawns() {
        let mut sup = XWaylandSupervisor::new();
        assert_eq!(sup.readiness(), Readiness::Idle);
        let calls = Cell::new(0);
        let mut spawn = || {
            calls.set(calls.get() + 1);
            Ok(7)
        };
        sup.tick(0, Some(&mut spawn));
        sup.tick(1_000_000, Some(&mut spawn));
        assert_eq!(calls.get(), 0);
        assert_eq!(sup.readiness(), Readiness::Idle);
    }

    #[test]
    fn request_moves_idle_to_pending_and_is_idempotent() {
        let mut sup = XWaylandSupervisor::new();
        sup.request();
        assert_eq!(sup.readiness(), Readiness::Pending);
        sup.request();
        assert_eq!(sup.readiness(), Readiness::Pending);
    }

    #[test]
    fn successful_spawn_on_first_attempt_yields_ready() {
        let mut sup = XWaylandSupervisor::new();
        sup.request();
        let calls = Cell::new(0);
        let mut spawn = || {
            calls.set(calls.get() + 1);
            Ok(7)
        };
        sup.tick(0, Some(&mut spawn));
        assert_eq!(calls.get(), 1);
        assert_eq!(sup.readiness(), Readiness::Ready { display: 7 });
    }

    #[test]
    fn spawn_failure_retries_with_backoff_then_settles_absent_after_budget() {
        let mut sup = XWaylandSupervisor::new();
        sup.request();
        let calls = Cell::new(0);
        let mut failing = || -> Result<u32, AbsentReason> {
            calls.set(calls.get() + 1);
            Err(AbsentReason::SpawnFailed)
        };
        // Attempt 1 at t=0; backoff gate is 500 ms (base delay).
        sup.tick(0, Some(&mut failing));
        assert_eq!(calls.get(), 1);
        assert_eq!(sup.readiness(), Readiness::Pending);
        // Attempt 2 at t=500; next gate is 1_000 ms.
        sup.tick(500, Some(&mut failing));
        assert_eq!(calls.get(), 2);
        assert_eq!(sup.readiness(), Readiness::Pending);
        // Attempt 3 at t=1500; next gate is 2_000 ms.
        sup.tick(1500, Some(&mut failing));
        assert_eq!(calls.get(), 3);
        assert_eq!(sup.readiness(), Readiness::Pending);
        // Attempt 4 at t=3500 exhausts the 3-restart budget: Absent.
        // Missing binary is non-fatal: no panic, session runs on native.
        sup.tick(3500, Some(&mut failing));
        assert_eq!(calls.get(), 4);
        assert_eq!(
            sup.readiness(),
            Readiness::Absent {
                reason: AbsentReason::SpawnFailed
            }
        );
    }

    #[test]
    fn running_disconnect_keeps_backoff_and_total_restart_budget() {
        let mut sup = XWaylandSupervisor::new();
        sup.request();
        let calls = Cell::new(0);
        let mut spawn = || {
            calls.set(calls.get() + 1);
            Ok(7)
        };
        sup.tick(0, Some(&mut spawn));
        sup.disconnected(10);
        sup.tick(509, Some(&mut spawn));
        assert_eq!(calls.get(), 1);
        sup.tick(510, Some(&mut spawn));
        assert_eq!(calls.get(), 2);
        sup.disconnected(520);
        sup.tick(1520, Some(&mut spawn));
        sup.disconnected(1530);
        sup.tick(3530, Some(&mut spawn));
        assert_eq!(calls.get(), 4);
        sup.disconnected(3540);
        sup.tick(99999, Some(&mut spawn));
        assert_eq!(calls.get(), 4);
        assert!(matches!(sup.readiness(), Readiness::Absent { .. }));
    }

    #[test]
    fn backoff_gate_blocks_tick_before_delay_passes() {
        let mut sup = XWaylandSupervisor::new();
        sup.request();
        let calls = Cell::new(0);
        let mut failing = || -> Result<u32, AbsentReason> {
            calls.set(calls.get() + 1);
            Err(AbsentReason::SpawnFailed)
        };
        sup.tick(0, Some(&mut failing));
        assert_eq!(calls.get(), 1);
        // 499 ms is still inside the 500 ms first backoff: no attempt.
        sup.tick(499, Some(&mut failing));
        assert_eq!(calls.get(), 1);
        assert_eq!(sup.readiness(), Readiness::Pending);
    }

    #[test]
    fn terminal_states_are_sticky() {
        // Ready never changes on further ticks.
        let mut ready = XWaylandSupervisor::new();
        ready.request();
        let ok_calls = Cell::new(0);
        let mut ok = || -> Result<u32, AbsentReason> {
            ok_calls.set(ok_calls.get() + 1);
            Ok(7)
        };
        ready.tick(0, Some(&mut ok));
        assert_eq!(ready.readiness(), Readiness::Ready { display: 7 });
        let fail_calls = Cell::new(0);
        let mut fail = || -> Result<u32, AbsentReason> {
            fail_calls.set(fail_calls.get() + 1);
            Err(AbsentReason::SpawnFailed)
        };
        ready.tick(u64::MAX, Some(&mut fail));
        ready.tick(u64::MAX, None);
        assert_eq!(ok_calls.get(), 1);
        assert_eq!(fail_calls.get(), 0);
        assert_eq!(ready.readiness(), Readiness::Ready { display: 7 });

        // Absent never changes on further ticks.
        let mut absent = XWaylandSupervisor::new();
        absent.request();
        let calls = Cell::new(0);
        let mut failing = || -> Result<u32, AbsentReason> {
            calls.set(calls.get() + 1);
            Err(AbsentReason::SpawnFailed)
        };
        absent.tick(0, Some(&mut failing));
        absent.tick(500, Some(&mut failing));
        absent.tick(1500, Some(&mut failing));
        absent.tick(3500, Some(&mut failing));
        assert_eq!(
            absent.readiness(),
            Readiness::Absent {
                reason: AbsentReason::SpawnFailed
            }
        );
        absent.tick(u64::MAX, Some(&mut failing));
        absent.tick(u64::MAX, None);
        assert_eq!(calls.get(), 4);
        assert_eq!(
            absent.readiness(),
            Readiness::Absent {
                reason: AbsentReason::SpawnFailed
            }
        );
    }

    #[test]
    fn no_spawner_stays_pending_with_zero_attempts() {
        let mut sup = XWaylandSupervisor::new();
        sup.request();
        // Pre-join wiring: the runtime polls with no spawner installed.
        sup.tick(0, None);
        sup.tick(u64::MAX, None);
        assert_eq!(sup.readiness(), Readiness::Pending);
        // Nothing was attempted: the first real spawner still runs attempt 1.
        let calls = Cell::new(0);
        let mut failing = || -> Result<u32, AbsentReason> {
            calls.set(calls.get() + 1);
            Err(AbsentReason::SpawnFailed)
        };
        sup.tick(u64::MAX, Some(&mut failing));
        assert_eq!(calls.get(), 1);
        assert_eq!(sup.readiness(), Readiness::Pending);
    }

    #[test]
    fn default_equals_new() {
        let a = XWaylandSupervisor::default();
        let b = XWaylandSupervisor::new();
        assert_eq!(a.readiness(), b.readiness());
        assert_eq!(a.readiness(), Readiness::Idle);
    }

    #[test]
    fn untitled_fallback_const_is_stable_and_nonblank() {
        // Runs with and without the `xwayland` feature (unlike
        // `identity_tests`): pins the mapping fallback the snapshot
        // path depends on in every suite configuration.
        assert!(!UNTITLED_X11_TITLE.is_empty());
        assert_eq!(UNTITLED_X11_TITLE, "Untitled window");
    }

    #[test]
    fn unsupported_spawner_settles_absent_unsupported_after_budget() {
        // The feature-off report path: `Unsupported` must propagate to
        // the terminal state exactly like `SpawnFailed`, never succeed.
        let mut sup = XWaylandSupervisor::new();
        sup.request();
        let calls = Cell::new(0);
        let mut unsupported = || -> Result<u32, AbsentReason> {
            calls.set(calls.get() + 1);
            Err(AbsentReason::Unsupported)
        };
        sup.tick(0, Some(&mut unsupported));
        sup.tick(500, Some(&mut unsupported));
        sup.tick(1500, Some(&mut unsupported));
        assert_eq!(sup.readiness(), Readiness::Pending);
        sup.tick(3500, Some(&mut unsupported));
        assert_eq!(calls.get(), 4);
        assert_eq!(
            sup.readiness(),
            Readiness::Absent {
                reason: AbsentReason::Unsupported
            }
        );
    }

    #[test]
    fn request_on_terminal_absent_never_revives() {
        // Late X11 needs after the settle must not reopen the server
        // attempt: the report stays logged-once and terminal.
        let mut sup = XWaylandSupervisor::new();
        sup.request();
        let calls = Cell::new(0);
        let mut failing = || -> Result<u32, AbsentReason> {
            calls.set(calls.get() + 1);
            Err(AbsentReason::SpawnFailed)
        };
        sup.tick(0, Some(&mut failing));
        sup.tick(500, Some(&mut failing));
        sup.tick(1500, Some(&mut failing));
        sup.tick(3500, Some(&mut failing));
        assert_eq!(
            sup.readiness(),
            Readiness::Absent {
                reason: AbsentReason::SpawnFailed
            }
        );
        sup.request();
        sup.tick(u64::MAX, Some(&mut failing));
        sup.tick(u64::MAX, None);
        assert_eq!(calls.get(), 4);
        assert_eq!(
            sup.readiness(),
            Readiness::Absent {
                reason: AbsentReason::SpawnFailed
            }
        );
    }

    #[test]
    fn production_style_spawn_with_empty_path_fails_cleanly_to_spawn_failed() {
        // Absent-binary report path: the production-style spawn lookup
        // for the `Xwayland` binary with an empty PATH must fail
        // cleanly (no panic, no process) and resolve to `SpawnFailed`,
        // driving the supervisor to the non-fatal `Absent` settle with
        // the native session untouched. No real loop is involved.
        let mut sup = XWaylandSupervisor::new();
        sup.request();
        let calls = Cell::new(0);
        let mut production_style = || -> Result<u32, AbsentReason> {
            calls.set(calls.get() + 1);
            match std::process::Command::new("Xwayland")
                .env_clear()
                .env("PATH", "")
                .arg("-help")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                Ok(mut child) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("Xwayland resolved with an empty PATH; expected a clean lookup failure");
                }
                Err(_) => Err(AbsentReason::SpawnFailed),
            }
        };
        sup.tick(0, Some(&mut production_style));
        sup.tick(500, Some(&mut production_style));
        sup.tick(1500, Some(&mut production_style));
        assert_eq!(sup.readiness(), Readiness::Pending);
        sup.tick(3500, Some(&mut production_style));
        assert_eq!(calls.get(), 4);
        assert_eq!(
            sup.readiness(),
            Readiness::Absent {
                reason: AbsentReason::SpawnFailed
            }
        );
    }
}

#[cfg(all(test, feature = "xwayland"))]
mod identity_tests {
    use super::{map_x11_identity, UNTITLED_X11_TITLE};

    #[test]
    fn full_title_and_class() {
        let (title, app_id) = map_x11_identity("Terminal", "xterm", "xterm-instance");
        assert_eq!(title, "Terminal");
        assert_eq!(app_id.as_deref(), Some("xterm"));
    }

    #[test]
    fn empty_title_falls_back_to_untitled_const() {
        let (title, app_id) = map_x11_identity("", "xterm", "xterm-instance");
        assert_eq!(title, UNTITLED_X11_TITLE);
        assert_eq!(app_id.as_deref(), Some("xterm"));
    }

    #[test]
    fn class_preferred_over_instance() {
        let (_, app_id) = map_x11_identity("Editor", "emacs", "emacs-instance");
        assert_eq!(app_id.as_deref(), Some("emacs"));
    }

    #[test]
    fn instance_used_when_class_empty() {
        let (_, app_id) = map_x11_identity("Editor", "", "emacs-instance");
        assert_eq!(app_id.as_deref(), Some("emacs-instance"));
    }

    #[test]
    fn both_class_and_instance_empty_yields_no_app_id() {
        let (title, app_id) = map_x11_identity("Editor", "", "");
        assert_eq!(title, "Editor");
        assert_eq!(app_id, None);
    }

    #[test]
    fn title_preserved_verbatim_without_trimming() {
        let (title, _) = map_x11_identity("  padded title  ", "xterm", "");
        assert_eq!(title, "  padded title  ");
    }

    #[test]
    fn all_fields_empty_yields_untitled_and_no_app_id() {
        // The fully-bare window: every missing-field fallback at once.
        // The snapshot entry must never carry a blank title, and with
        // no WM_CLASS at all there is no app id to report.
        let (title, app_id) = map_x11_identity("", "", "");
        assert_eq!(title, UNTITLED_X11_TITLE);
        assert!(!title.is_empty());
        assert_eq!(app_id, None);
    }
}

#[cfg(all(test, feature = "xwayland"))]
mod frame_tests {
    use super::{x11_wants_frame, X11Frame};
    use smithay::utils::{Logical, Rectangle, Size};
    use smithay::xwayland::xwm::{
        addresses_window, wm_state_property, FrameBorder, FrameExtents, WmWindowType,
    };

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Logical> {
        Rectangle::new((x, y).into(), (w, h).into())
    }

    fn size(w: i32, h: i32) -> Size<i32, Logical> {
        (w, h).into()
    }

    /// A GTK frames-client frame: a 47 px header bar, and the shadow GDK
    /// reports as `_GTK_FRAME_EXTENTS`, wider below than above.
    fn gtk_frame() -> X11Frame {
        X11Frame {
            extents: FrameExtents {
                visible: FrameBorder {
                    left: 0,
                    right: 0,
                    top: 47,
                    bottom: 0,
                },
                invisible: FrameBorder {
                    left: 25,
                    right: 25,
                    top: 22,
                    bottom: 28,
                },
            },
            framed: true,
        }
    }

    #[test]
    fn mutter_frames_normal_dialog_menu_and_utility_windows_only() {
        for kind in [
            None,
            Some(WmWindowType::Normal),
            Some(WmWindowType::Dialog),
            Some(WmWindowType::Menu),
            Some(WmWindowType::Utility),
        ] {
            assert!(x11_wants_frame(kind, false, false), "framed type {kind:?}");
        }
        // meta_window_get_frame_type: META_FRAME_TYPE_LAST.
        for kind in [
            WmWindowType::Desktop,
            WmWindowType::Dock,
            WmWindowType::Toolbar,
            WmWindowType::Splash,
            WmWindowType::DropdownMenu,
            WmWindowType::PopupMenu,
            WmWindowType::Tooltip,
            WmWindowType::Notification,
            WmWindowType::Combo,
            WmWindowType::Dnd,
        ] {
            assert!(
                !x11_wants_frame(Some(kind), false, false),
                "unframed type {kind:?}"
            );
        }
    }

    #[test]
    fn motif_undecorated_and_override_redirect_windows_get_no_frame() {
        // GTK client-side decorations and borderless players set
        // _MOTIF_WM_HINTS decorations to 0.
        assert!(!x11_wants_frame(Some(WmWindowType::Normal), false, true));
        assert!(!x11_wants_frame(None, false, true));
        // "We don't want to allow override-redirect windows to have
        // decorated-window types".
        assert!(!x11_wants_frame(Some(WmWindowType::Normal), true, false));
        assert!(!x11_wants_frame(Some(WmWindowType::Dialog), true, false));
    }

    #[test]
    fn frame_window_is_the_frame_rectangle_grown_by_the_invisible_borders() {
        let frame = gtk_frame();
        let visible = rect(100, 200, 640, 527);
        let buffer = frame.buffer_rect(visible);
        assert_eq!(buffer, rect(75, 178, 690, 577));
        assert_eq!(frame.extents.buffer_rect_to_frame_rect(buffer), visible);
    }

    #[test]
    fn client_sits_at_the_total_offset_and_loses_every_border() {
        let frame = gtk_frame();
        let buffer = frame.buffer_rect(rect(100, 200, 640, 527));
        // frame->child_x / child_y: invisible plus visible, left and top.
        let child = frame.extents.child_rect(buffer.size);
        assert_eq!(child, rect(25, 69, 640, 480));
        // The same client rectangle from the frame rectangle, in root
        // coordinates (the synthetic ConfigureNotify the client receives).
        assert_eq!(
            frame
                .extents
                .frame_rect_to_client_rect(rect(100, 200, 640, 527)),
            rect(100, 247, 640, 480)
        );
        assert_eq!(
            Rectangle::new(buffer.loc + child.loc, child.size),
            rect(100, 247, 640, 480)
        );
    }

    #[test]
    fn a_requested_client_size_grows_by_the_title_bar_and_back() {
        let frame = gtk_frame();
        assert_eq!(frame.frame_size_for_client(size(640, 480)), size(640, 527));
        assert_eq!(frame.client_size(size(640, 527)), size(640, 480));
        // No frame, no borders: the client is the window.
        let bare = X11Frame::default();
        assert_eq!(bare.frame_size_for_client(size(640, 480)), size(640, 480));
        assert_eq!(bare.client_size(size(640, 480)), size(640, 480));
        assert_eq!(
            bare.buffer_rect(rect(10, 20, 640, 480)),
            rect(10, 20, 640, 480)
        );
    }

    #[test]
    fn a_client_decorated_window_is_its_buffer_inside_its_own_shadow() {
        // GTK draws the shadow itself and reports it as _GTK_FRAME_EXTENTS:
        // the visible window is the client minus that margin.
        let csd = X11Frame {
            extents: FrameExtents {
                visible: FrameBorder::default(),
                invisible: FrameBorder {
                    left: 26,
                    right: 26,
                    top: 23,
                    bottom: 29,
                },
            },
            framed: false,
        };
        assert_eq!(csd.frame_size_for_client(size(852, 652)), size(800, 600));
        assert_eq!(csd.client_size(size(800, 600)), size(852, 652));
        assert_eq!(
            csd.buffer_rect(rect(100, 100, 800, 600)),
            rect(74, 77, 852, 652)
        );
        assert_eq!(
            csd.window_geometry(size(852, 652)),
            Some(rect(26, 23, 800, 600))
        );
    }

    #[test]
    fn framing_a_bare_window_keeps_its_client_size_and_origin() {
        let frame = gtk_frame();
        let bare = X11Frame::default();
        let framed = frame.reframed(&bare, rect(300, 150, 640, 480));
        assert_eq!(framed, rect(300, 150, 640, 527));
        // Losing the frame (the frames client died) gives the height back.
        assert_eq!(bare.reframed(&frame, framed), rect(300, 150, 640, 480));
    }

    #[test]
    fn adopting_a_frame_grows_the_x11_window_around_the_unchanged_client() {
        // The X11 window carrying the surface when the frame arrives is
        // the bare wrapper the client fills. The frame must not shrink
        // the client into it: the buffer grows by every border, and the
        // frame rectangle starts where the bare window did.
        let frame = gtk_frame();
        let bare = rect(300, 150, 640, 480);
        let buffer = frame.extents.bare_rect_to_buffer_rect(bare);
        assert_eq!(buffer, rect(275, 128, 690, 577));
        assert_eq!(frame.extents.child_rect(buffer.size).size, bare.size);
        // The same rectangle the manager lays the window out with.
        let framed = frame.reframed(&X11Frame::default(), bare);
        assert_eq!(frame.buffer_rect(framed), buffer);
        // Losing the frame gives the client's rectangle back, at the
        // frame rectangle's origin.
        assert_eq!(frame.extents.buffer_rect_to_bare_rect(buffer), bare);
        // Without borders nothing moves.
        let none = FrameExtents::default();
        assert_eq!(none.bare_rect_to_buffer_rect(bare), bare);
        assert_eq!(none.buffer_rect_to_bare_rect(bare), bare);
    }

    #[test]
    fn a_window_maps_at_the_frame_size_inside_its_x11_window() {
        // A frame adopted before the map was granted: the X11 window is
        // the asked client size plus every border.
        let frame = gtk_frame();
        assert_eq!(frame.frame_size_in_buffer(size(690, 577)), size(640, 527));
        assert_eq!(frame.client_size_in_buffer(size(690, 577)), size(640, 480));
        assert_eq!(frame.buffer_size_for_client(size(640, 480)), size(690, 577));
        // No frame: the client is the X11 window.
        let bare = X11Frame::default();
        assert_eq!(bare.frame_size_in_buffer(size(640, 480)), size(640, 480));
        assert_eq!(bare.client_size_in_buffer(size(640, 480)), size(640, 480));
        assert_eq!(bare.buffer_size_for_client(size(640, 480)), size(640, 480));
        // A client-decorated window is its X11 window, shadow included.
        let csd = X11Frame {
            extents: FrameExtents {
                visible: FrameBorder::default(),
                invisible: gtk_frame().extents.invisible,
            },
            framed: false,
        };
        assert_eq!(csd.frame_size_in_buffer(size(690, 530)), size(640, 480));
        assert_eq!(csd.client_size_in_buffer(size(690, 530)), size(690, 530));
        assert_eq!(csd.buffer_size_for_client(size(690, 530)), size(690, 530));
    }

    #[test]
    fn window_geometry_is_the_surface_minus_the_shadow() {
        let frame = gtk_frame();
        assert_eq!(
            frame.window_geometry(size(690, 577)),
            Some(rect(25, 22, 640, 527))
        );
        // A maximized or fullscreen frame has no shadow: the surface is
        // the window and carries no geometry of its own.
        let flat = X11Frame {
            extents: FrameExtents {
                visible: gtk_frame().extents.visible,
                invisible: FrameBorder::default(),
            },
            framed: true,
        };
        assert_eq!(flat.window_geometry(size(1920, 1048)), None);
        assert_eq!(X11Frame::default().window_geometry(size(640, 480)), None);
    }

    #[test]
    fn net_frame_extents_are_the_visible_borders_left_right_top_bottom() {
        assert_eq!(gtk_frame().extents.net_frame_extents(), [0, 0, 47, 0]);
        assert_eq!(FrameExtents::default().net_frame_extents(), [0, 0, 0, 0]);
    }

    #[test]
    fn frame_extents_properties_need_exactly_four_cardinals() {
        assert_eq!(
            FrameBorder::from_cardinals(&[1, 2, 3, 4]),
            Some(FrameBorder {
                left: 1,
                right: 2,
                top: 3,
                bottom: 4,
            })
        );
        // meta_frame_query_borders: `nitems == 4`, else no borders.
        assert_eq!(FrameBorder::from_cardinals(&[]), None);
        assert_eq!(FrameBorder::from_cardinals(&[1, 2, 3]), None);
        assert_eq!(FrameBorder::from_cardinals(&[1, 2, 3, 4, 5]), None);
    }

    #[test]
    fn borders_scale_from_x11_pixels_to_logical_pixels() {
        let scaled = gtk_frame().extents.to_logical(2.0);
        assert_eq!(scaled.visible.top, 24);
        assert_eq!(scaled.invisible.left, 13);
        assert_eq!(scaled.invisible.bottom, 14);
        assert_eq!(gtk_frame().extents.to_logical(1.0), gtk_frame().extents);
    }

    #[test]
    fn requests_naming_the_frame_reach_its_client_window() {
        let (client, frame, other) = (0x40_0001, 0x60_0007, 0x40_0002);
        // GTK sends _NET_WM_STATE, WM_CHANGE_STATE, _NET_WM_MOVERESIZE and
        // _GTK_SHOW_WINDOW_MENU for the header bar's own window.
        assert!(addresses_window(frame, client, Some(frame)));
        assert!(addresses_window(client, client, Some(frame)));
        assert!(addresses_window(client, client, None));
        assert!(!addresses_window(other, client, Some(frame)));
        // A window without a frame answers to its own id alone.
        assert!(!addresses_window(frame, client, None));
    }

    #[test]
    fn a_minimized_window_s_wm_state_is_iconic_without_an_icon_window() {
        // ICCCM 4.1.3.1: NormalState 1, IconicState 3, then the icon
        // window (None), as Mutter's set_wm_state_on_xwindow writes them.
        assert_eq!(wm_state_property(true), [3, 0]);
        assert_eq!(wm_state_property(false), [1, 0]);
    }
}
