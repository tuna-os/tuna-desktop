//! Shell-side control client for the versioned shell control protocol.
//!
//! 001 spec R3 + ADR 0002: the shell host talks to the compositor over a
//! dedicated Unix socket whose frames are `u32-LE length + postcard body`
//! (see `tuna-shell-control`). This client owns the shell side of that
//! conversation: the `Hello` handshake, full-`Snapshot` application into
//! [`ShellModel`], ordered `Changes` application with revision-gap
//! resnapshot, and activation commands carrying compositor-minted tokens.
//!
//! Transport behavior mirrors the server side: the stream is put in
//! nonblocking mode at construction and `WouldBlock` is always surfaced
//! to the caller (as [`ControlError::Io`]), never hidden behind a retry
//! loop, so the panel event loop stays responsive. The socket is
//! exposed through [`AsFd`] so the caller can watch it for readability
//! and drain [`ControlClient::poll`] only when bytes arrive; frames
//! already read into the buffer show in
//! [`ControlClient::has_buffered_frame`], and a dead connection in
//! [`ControlClient::is_closed`].
//!
//! Resnapshot rule (ADR 0002): the protocol has no dedicated
//! snapshot-request opcode, so re-sending `Hello` via
//! [`ControlClient::request_snapshot`] is the resync signal. Any revision
//! gap — a `Changes.from_revision` that does not match the held revision,
//! or a typed [`ErrorKind::RevisionGap`] frame — sets
//! [`ControlClient::needs_snapshot`]; the caller re-hellos and the next
//! `Snapshot` clears the flag. A fresh client (or a reconnected one)
//! starts with no revision and the flag set: it must handshake, then wait
//! for its first full snapshot before trusting any delta.
//!
//! Activation (GNOME overview parallel: activating a window from the
//! overview dismisses it) stops at the wire here on purpose: this client
//! sends the token-bearing `Command` and hands the caller the request id
//! for `CommandResult` correlation. Mapping the result back into
//! [`ShellModel::select_window`] and overview dismissal is later UI
//! wiring, not transport.

use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use tuna_shell_control::{
    ActivationToken, CommandKind, CommandStatus, DecodeError, ErrorKind, Message, OutputInfo,
    SwitcherAction, WindowInfo, WorkspaceInfo, CURRENT_VERSION, MAX_FRAME_BYTES,
};

/// Transport and protocol failures, shared with the compositor endpoint.
///
/// Defined in [`tuna_shell_control`] so both endpoints name the same type;
/// re-exported here because `crate::control::ControlError` is the path the
/// rest of this crate (and its tests) use.
pub use tuna_shell_control::ControlError;

use crate::model::{ShellModel, SnapshotView, WindowEntry};

/// First shell-chosen command request id.
pub const INITIAL_REQUEST_ID: u64 = 1;

/// The least time between two redials [`ControlClient::poll`] makes
/// while the compositor is unavailable.
pub const REDIAL_INTERVAL: Duration = Duration::from_millis(250);

/// What one handled inbound message meant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Handled {
    ScreenReader(tuna_shell_control::ScreenReaderState),
    /// Compositor greeted back (e.g. after a resnapshot re-hello); the
    /// awaited full snapshot still follows.
    Hello,
    /// Full state replaced the model; [`ControlClient::needs_snapshot`]
    /// is now clear.
    Snapshot {
        /// Revision now held.
        revision: u64,
    },
    /// Ordered deltas applied on top of the held revision.
    Changes {
        /// Revision now held.
        to_revision: u64,
    },
    /// A workspace key switched workspaces: show GNOME's popup.
    WorkspacePopup {
        /// The active workspace's position.
        index: u32,
        /// How many workspaces there are.
        count: u32,
    },
    /// A client asked for GNOME's window menu.
    WindowMenu(WindowMenuRequest),
    /// Connector under the compositor pointer changed.
    PointerOutput,
    /// Show or dismiss the shortcut consent dialog.
    ShortcutConsent(Option<(u64, String)>),
    /// The keyboard switched Sticky or Slow Keys: save and confirm.
    KeyboardAidToggled {
        aid: tuna_shell_control::KeyboardAid,
        enabled: bool,
    },
    /// The click the next hover click makes.
    DwellClickType(tuna_shell_control::DwellClick),
    /// A pointer accessibility timeout started or stopped (pie timer).
    PointerTimeout {
        kind: tuna_shell_control::PointerTimeoutKind,
        duration_ms: Option<u32>,
        clicked: bool,
        x: i32,
        y: i32,
    },
    /// A grabbed accelerator was pressed (org.gnome.Shell).
    Accelerator {
        /// The grab's action id.
        action: u32,
        /// The key event's time in milliseconds.
        time: u32,
        /// The action mode it fired in.
        mode: u32,
    },
    /// Compositor answered one command; match `id` against the value
    /// [`ControlClient::send_activation`] returned.
    CommandResult {
        /// Echo of the command's request id.
        id: u64,
        /// Outcome.
        status: CommandStatus,
    },
    /// Typed compositor error that is not a revision gap (logged/counted
    /// by the caller; the connection stays usable).
    ServerError {
        /// Category.
        kind: ErrorKind,
        /// Diagnostics.
        message: String,
    },
    /// Held state is stale: discard nothing yet, but re-request a full
    /// snapshot before trusting further deltas. The offending delta was
    /// not applied.
    Gap {
        /// Revision held when the gap was detected (`None` = no snapshot
        /// received yet on this connection).
        have: Option<u64>,
    },
    /// Compositor overview intent (002 R1): the shell-facing open state.
    /// Carries no revision — UI state, not model truth.
    Overview {
        /// Whether the overview should be open.
        open: bool,
    },
    /// Compositor Alt-Tab drive applied to the shell switcher (002
    /// workspaces). `selection` is the switcher selection after
    /// applying (the commit target, or `None` when closed/empty).
    /// Carries no revision — UI state, not model truth.
    Switcher {
        /// Drive event that was applied.
        action: SwitcherAction,
        /// Current switcher selection, if any.
        selection: Option<u64>,
    },
    /// Compositor output inventory (multi-monitor): the full list the
    /// shell reconciles its per-output surfaces against. Carries no
    /// revision — structural state, not model truth.
    Outputs {
        /// Output count now held.
        count: usize,
    },
    /// Session environment for launched apps (#59), already applied to
    /// [`crate::apps::launch`].
    Environment {
        /// Variable count now held.
        count: usize,
    },
    /// Where the overview's previews sit (UI state, not model truth).
    OverviewPreviews {
        /// Preview count now held.
        count: usize,
    },
}

/// Write errors that mean the peer is gone for good (evicted,
/// crashed, restarted): the session must redial rather than retry on
/// the dead stream. `WouldBlock` is deliberately absent — transient
/// backpressure keeps surfacing to the caller per the nonblocking
/// contract, and a refused redial reports its own error normally.
fn is_dead_peer(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::NotConnected
    )
}

/// Admission for input-method UI actions on this connection only. A compatible
/// greeting and a subsequent snapshot are both required; this is protocol
/// compatibility, not authentication of a Unix peer's process identity.
#[derive(Clone, Copy, PartialEq, Eq)]
enum InputMethodAdmission {
    AwaitHello,
    AwaitSnapshot,
    Ready,
}

/// Shell-side control client over a connected Unix socket.
pub struct ControlClient {
    stream: UnixStream,
    /// Socket path for redial, kept by [`ControlClient::connect`]
    /// (`None` for a caller-supplied stream, which has no redial path).
    socket_path: Option<std::path::PathBuf>,
    /// Bound idle redial attempts while the compositor is unavailable.
    next_poll_reconnect: Option<Instant>,
    read_buf: Vec<u8>,
    model: ShellModel,
    revision: Option<u64>,
    needs_snapshot: bool,
    next_request_id: u64,
    shadow_windows: Vec<WindowInfo>,
    shadow_workspaces: Vec<WorkspaceInfo>,
    /// Compositor-owned session-lock display value from the latest
    /// snapshot. Read-only here: the flag lives in the compositor,
    /// never the shell.
    locked: bool,
    /// Output inventory from the latest `Outputs` message (empty
    /// before the first one): name, size, and dock anchor per output.
    /// Read-only here; the shell reconciles its surfaces against it
    /// and never edits it.
    outputs: Vec<OutputInfo>,
    native_outputs: Vec<tuna_shell_control::NativeOutputInfo>,
    monitor_identities: Vec<tuna_shell_control::MonitorIdentityInfo>,
    pointer_output: Option<String>,
    /// Session environment from the compositor (#59).
    environment: Vec<(String, String)>,
    /// Overview previews and the hovered one, from the compositor.
    overview_previews: (Vec<tuna_shell_control::PreviewInfo>, Option<u64>),
    connection_generation: u64,
    auth_needs_snapshot: bool,
    input_method_admission: InputMethodAdmission,
    wellbeing_applied: Option<(bool, bool)>,
    wellbeing_pending: Option<(u64, (bool, bool), Instant)>,
    /// The read half reached end-of-file or failed: nothing more will
    /// arrive on this connection.
    closed: bool,
}

impl ControlClient {
    /// Connect to the compositor's control socket and wrap the stream.
    /// Sends nothing: call [`hello`](Self::hello) first, then await the
    /// full snapshot before trusting any delta.
    ///
    /// The path is kept for redial: if the hub later evicts and closes
    /// this session, the next send reconnects and resends instead of
    /// failing forever (spec R6 keeps the hub's side drop-happy).
    pub fn connect(path: &std::path::Path) -> io::Result<Self> {
        let mut client = Self::new(UnixStream::connect(path)?)?;
        client.socket_path = Some(path.to_path_buf());
        Ok(client)
    }

    /// Wrap a connected stream, switching it to nonblocking mode.
    ///
    /// Starts with no revision and [`ControlClient::needs_snapshot`] set:
    /// handshake first, then await the initial full snapshot. A stream
    /// wrapped here has no redial path ([`ControlClient::connect`] keeps
    /// the socket path for that).
    pub fn new(stream: UnixStream) -> io::Result<Self> {
        stream.set_nonblocking(true)?;
        Ok(Self {
            stream,
            socket_path: None,
            next_poll_reconnect: None,
            read_buf: Vec::new(),
            model: ShellModel::new(),
            revision: None,
            needs_snapshot: true,
            next_request_id: INITIAL_REQUEST_ID,
            shadow_windows: Vec::new(),
            shadow_workspaces: Vec::new(),
            locked: false,
            outputs: Vec::new(),
            native_outputs: Vec::new(),
            monitor_identities: Vec::new(),
            pointer_output: None,
            environment: Vec::new(),
            overview_previews: (Vec::new(), None),
            connection_generation: 0,
            auth_needs_snapshot: true,
            input_method_admission: InputMethodAdmission::AwaitHello,
            wellbeing_applied: None,
            wellbeing_pending: None,
            closed: false,
        })
    }

    /// Shell-side view state (window list, overview flag).
    pub fn model(&self) -> &ShellModel {
        &self.model
    }

    /// Which desktop entry a window's app id belongs to (see
    /// [`ShellModel::set_app_resolver`]).
    pub fn set_app_resolver(&mut self, resolve: impl Fn(&str) -> Option<String> + 'static) {
        self.model.set_app_resolver(resolve);
    }

    pub fn set_switcher_workspace_policy(&mut self, policy: crate::model::SwitcherWorkspacePolicy) {
        self.model.set_switcher_workspace_policy(policy);
    }

    /// Revision of the last applied snapshot or delta (`None` before the
    /// first snapshot).
    pub fn revision(&self) -> Option<u64> {
        self.revision
    }

    /// Whether a full snapshot must be (re)requested before deltas can be
    /// trusted. Set at construction, on every gap signal, and never
    /// cleared except by applying a `Snapshot`.
    pub fn needs_snapshot(&self) -> bool {
        self.needs_snapshot
    }

    /// Whether the read half ended (the compositor closed the socket)
    /// or failed with something other than `WouldBlock`. A closed client
    /// keeps its last state; [`ControlClient::poll`] only errors from
    /// here on, unless it can redial ([`ControlClient::connect`]): a
    /// successful redial opens a fresh stream (a new
    /// [`connection_generation`](Self::connection_generation) and file
    /// descriptor) that is not closed.
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// Whether a complete frame (or an oversize prefix, which
    /// [`ControlClient::poll`] rejects) already sits in the read buffer.
    /// The socket does not report these as readable, so a caller that
    /// waits for readability drains them first.
    pub fn has_buffered_frame(&self) -> bool {
        let Some(prefix) = self.read_buf.first_chunk::<4>() else {
            return false;
        };
        let len = u32::from_le_bytes(*prefix) as usize;
        len > MAX_FRAME_BYTES || self.read_buf.len() >= 4 + len
    }

    /// Session-lock display value from the latest snapshot (false before
    /// the first one). Mirrors the compositor-owned flag; the shell
    /// renders from it and never flips it.
    pub fn locked(&self) -> bool {
        self.locked
    }

    /// Whether an input-method menu may dispatch an action. The current peer
    /// must have supplied a compatible Hello followed by a snapshot, that
    /// snapshot must still be current, and its session must be unlocked.
    pub fn input_method_actions_allowed(&self) -> bool {
        self.input_method_admission == InputMethodAdmission::Ready
            && !self.needs_snapshot
            && !self.locked
    }

    /// Session environment from the latest `Environment` message
    /// (empty before the first one), already applied to app launches.
    pub fn environment(&self) -> &[(String, String)] {
        &self.environment
    }

    /// The overview's window previews and the hovered one.
    pub fn overview_previews(&self) -> (&[tuna_shell_control::PreviewInfo], Option<u64>) {
        (&self.overview_previews.0, self.overview_previews.1)
    }

    /// Output inventory from the latest `Outputs` message (empty
    /// before the first one). The shell reconciles its per-output
    /// surfaces against this and never edits it.
    pub fn pointer_output(&self) -> Option<&str> {
        self.pointer_output.as_deref()
    }

    pub fn monitor_identities(&self) -> &[tuna_shell_control::MonitorIdentityInfo] {
        &self.monitor_identities
    }

    pub fn native_outputs(&self) -> &[tuna_shell_control::NativeOutputInfo] {
        &self.native_outputs
    }

    pub fn outputs(&self) -> &[OutputInfo] {
        &self.outputs
    }

    /// Send our `Hello`. Pair with [`ControlClient::await_hello`], or use
    /// [`ControlClient::hello`] for both halves at once.
    pub fn send_hello(&mut self) -> Result<(), ControlError> {
        self.write_message(&Message::Hello {
            version: CURRENT_VERSION,
        })
    }

    /// Read one frame and require it to be a compatible `Hello` reply (or
    /// fail on the compositor's typed `Error`).
    ///
    /// A stale-major `Hello` never arrives as a message: schema decoding
    /// rejects it first, surfacing [`ControlError::Decode`].
    pub fn await_hello(&mut self) -> Result<(), ControlError> {
        self.input_method_admission = InputMethodAdmission::AwaitHello;
        self.native_outputs.clear();
        match self.read_frame()? {
            Message::Hello { version } => {
                if version.is_compatible_with(&CURRENT_VERSION) {
                    self.input_method_admission = InputMethodAdmission::AwaitSnapshot;
                    Ok(())
                } else {
                    Err(ControlError::Remote {
                        kind: ErrorKind::IncompatibleVersion,
                        message: format!(
                            "peer {}.{} is newer than ours {}.{}",
                            version.major,
                            version.minor,
                            CURRENT_VERSION.major,
                            CURRENT_VERSION.minor
                        ),
                    })
                }
            }
            Message::Error { kind, message } => Err(ControlError::Remote { kind, message }),
            other => Err(ControlError::Unexpected(format!(
                "expected Hello reply, got {}",
                message_label(&other)
            ))),
        }
    }

    /// Handshake: send `Hello`, expect a compatible `Hello` reply.
    ///
    /// On a blocking stream (or a peer that already replied) this
    /// completes in one call; on a nonblocking stream with no reply
    /// waiting it surfaces `WouldBlock` from the read half — retry
    /// [`ControlClient::await_hello`] once the socket is readable.
    pub fn hello(&mut self) -> Result<(), ControlError> {
        self.send_hello()?;
        self.await_hello()
    }

    /// Ask for a full snapshot: re-send `Hello`, the resync signal (the
    /// protocol has no dedicated snapshot-request opcode). The flag stays
    /// set until the answering `Snapshot` is applied.
    pub fn request_snapshot(&mut self) -> Result<(), ControlError> {
        self.send_hello()
    }

    /// Activate one window by id with its latest token (002 T2).
    ///
    /// Selects in the shadow model first, so the activation target is
    /// explicit — callers holding their own view state (the host model)
    /// must not assume the shadow shares their selection. Unknown ids
    /// fail before anything is sent. Returns the request id for
    /// `CommandResult` correlation.
    pub fn activate_window(&mut self, window: u64) -> Result<u64, ControlError> {
        if !self.model.select_window(window) {
            return Err(ControlError::Unexpected(format!(
                "window {window} not in snapshot"
            )));
        }
        self.activate_selected()
    }

    /// Activate the currently selected window with its latest token.
    ///
    /// Overview flow: [`select_window`](crate::model::ShellModel::select_window)
    /// first, then this. The token comes from the shadow `WindowInfo` of
    /// the last snapshot or delta — never cached across resyncs, since
    /// every snapshot mints fresh one-use tokens. Returns the request id
    /// for `CommandResult` correlation.
    pub fn activate_selected(&mut self) -> Result<u64, ControlError> {
        let window = self
            .model
            .selected()
            .ok_or_else(|| ControlError::AppConstraint("no selected window to activate".into()))?;
        let token = self
            .shadow_windows
            .iter()
            .find(|w| w.id == window)
            .map(|w| ActivationToken::new(w.activation_token.clone()))
            .ok_or_else(|| {
                ControlError::Unexpected(format!("no activation token for window {window}"))
            })?;
        self.send_activation(token)
    }

    /// Ask the compositor to flip the overview intent (002 T2).
    ///
    /// The overview answers Enter-on-empty and post-activation dismissal
    /// through this: the shell never flips its flag locally and waits
    /// for the compositor's `Overview` broadcast instead. Returns the
    /// request id for `CommandResult` correlation.
    pub fn toggle_overview(&mut self) -> Result<u64, ControlError> {
        let id = self.alloc_request_id();
        self.write_message(&Message::Command {
            id,
            kind: CommandKind::ToggleOverview,
        })?;
        Ok(id)
    }

    /// The hover-click chooser's pick for the next dwell.
    pub fn set_dwell_click_type(
        &mut self,
        click: tuna_shell_control::DwellClick,
    ) -> Result<u64, ControlError> {
        let id = self.alloc_request_id();
        self.write_message(&Message::Command {
            id,
            kind: CommandKind::SetDwellClickType { click },
        })?;
        Ok(id)
    }

    /// Publish the Screen Reader preference to the owning compositor.
    pub fn set_screen_reader(&mut self, enabled: bool) -> Result<u64, ControlError> {
        let id = self.alloc_request_id();
        self.write_message(&Message::Command {
            id,
            kind: CommandKind::SetScreenReader { enabled },
        })?;
        Ok(id)
    }

    /// Move the cursor within the screenshot overlay after keyboard selection.
    pub fn move_screenshot_pointer(&mut self, x: f64, y: f64) -> Result<u64, ControlError> {
        let id = self.alloc_request_id();
        self.write_message(&Message::Command {
            id,
            kind: CommandKind::MoveScreenshotPointer {
                x: x.floor() as i32,
                y: y.floor() as i32,
            },
        })?;
        Ok(id)
    }

    /// Send GNOME's input settings (#60). Returns the request id.
    pub fn set_input_settings(
        &mut self,
        settings: tuna_shell_control::InputSettings,
    ) -> Result<u64, ControlError> {
        let id = self.alloc_request_id();
        let fixed_count =
            (!settings.workspace_policy.dynamic).then_some(settings.workspace_policy.fixed_count);
        self.write_message(&Message::Command {
            id,
            kind: CommandKind::SetInputSettings(Box::new(settings)),
        })?;
        self.model.set_fixed_workspace_count(fixed_count);
        Ok(id)
    }

    /// Send the wellbeing grayscale/fade pair in one protocol command.
    pub fn set_wellbeing_effects(
        &mut self,
        grayscale: bool,
        fade: bool,
    ) -> Result<u64, ControlError> {
        let id = self.alloc_request_id();
        self.write_message(&Message::Command {
            id,
            kind: CommandKind::SetWellbeingEffects { grayscale, fade },
        })?;
        Ok(id)
    }

    /// Reconcile desired effects with a correlated Applied result in this
    /// connection generation. One pending request bounds wire traffic; a lost
    /// reply expires after five seconds. Superseded results cannot acknowledge
    /// the current desired pair. Reconnect invalidates both caches.
    pub fn reconcile_wellbeing_effects(
        &mut self,
        grayscale: bool,
        fade: bool,
    ) -> Result<bool, ControlError> {
        let pair = (grayscale, fade);
        if let Some((_, pending_pair, deadline)) = self.wellbeing_pending {
            if pending_pair == pair && Instant::now() < deadline {
                return Ok(false);
            }
            self.wellbeing_pending = None;
        }
        if self.wellbeing_applied == Some(pair) {
            return Ok(true);
        }
        let id = self.set_wellbeing_effects(grayscale, fade)?;
        self.wellbeing_applied = None;
        self.wellbeing_pending = Some((id, pair, Instant::now() + Duration::from_secs(5)));
        Ok(false)
    }

    /// Local connection identity, advanced when a fresh socket replaces a peer.
    pub fn connection_generation(&self) -> u64 {
        self.connection_generation
    }

    /// Tell the compositor whether the overview shows the app grid, so
    /// it draws the workspaces as thumbnails. Returns the request id.
    pub fn set_overview_app_grid(&mut self, active: bool) -> Result<u64, ControlError> {
        let id = self.alloc_request_id();
        self.write_message(&Message::Command {
            id,
            kind: CommandKind::SetOverviewAppGrid { active },
        })?;
        Ok(id)
    }

    /// Tell the compositor whether overview search shows results, so
    /// it hides the workspace view meanwhile. Returns the request id.
    pub fn set_overview_search(&mut self, active: bool) -> Result<u64, ControlError> {
        let id = self.alloc_request_id();
        self.write_message(&Message::Command {
            id,
            kind: CommandKind::SetOverviewSearch { active },
        })?;
        Ok(id)
    }

    /// Lock after `ms` of idle time, `0` for never: GNOME's
    /// `idle-delay`, `lock-enabled` and `lock-delay` as the shell
    /// derives them (#63). Returns the request id.
    pub fn set_idle_timeout(&mut self, ms: u64) -> Result<u64, ControlError> {
        let id = self.alloc_request_id();
        self.write_message(&Message::Command {
            id,
            kind: CommandKind::SetIdleTimeout { ms },
        })?;
        Ok(id)
    }

    /// Ask the compositor to lock the session at once (manual lock
    /// from the shell).
    ///
    /// The shell trigger surface calls this: like `toggle_overview`
    /// it carries no activation token (the shell is a trusted local
    /// peer and locking hides rather than reveals), it is
    /// idempotent, and the compositor answers with a `CommandResult`
    /// plus a locked, content-free snapshot the [`locked`](Self::locked)
    /// display value mirrors. Returns the request id for
    /// `CommandResult` correlation.
    pub fn lock(&mut self) -> Result<u64, ControlError> {
        let id = self.alloc_request_id();
        self.write_message(&Message::Command {
            id,
            kind: CommandKind::Lock,
        })?;
        Ok(id)
    }

    /// Replace the compositor's accelerator grabs (org.gnome.Shell's
    /// GrabAccelerators), the full set each time. Returns the request id.
    pub fn set_accelerators(
        &mut self,
        accelerators: Vec<tuna_shell_control::Accelerator>,
    ) -> Result<u64, ControlError> {
        let id = self.alloc_request_id();
        self.write_message(&Message::Command {
            id,
            kind: CommandKind::SetAccelerators { accelerators },
        })?;
        Ok(id)
    }

    /// Make `workspace` active (GNOME's workspace keys). Returns the
    /// request id.
    pub fn focus_workspace(&mut self, workspace: u32) -> Result<u64, ControlError> {
        let id = self.alloc_request_id();
        self.write_message(&Message::Command {
            id,
            kind: CommandKind::FocusWorkspace {
                workspace: u64::from(workspace),
            },
        })?;
        Ok(id)
    }

    /// Answer the compositor's pending shortcut consent request.
    pub fn shortcut_consent(&mut self, request: u64, allow: bool) -> Result<u64, ControlError> {
        let id = self.alloc_request_id();
        self.write_message(&Message::Command {
            id,
            kind: CommandKind::ShortcutConsent { request, allow },
        })?;
        Ok(id)
    }

    /// Carry out one of GNOME's window-menu actions. Returns the request id.
    pub fn window_action(
        &mut self,
        window: u64,
        action: tuna_shell_control::WindowAction,
    ) -> Result<u64, ControlError> {
        let id = self.alloc_request_id();
        self.write_message(&Message::Command {
            id,
            kind: CommandKind::WindowAction { window, action },
        })?;
        Ok(id)
    }

    /// Replace global logical minimize targets published by the dash.
    pub fn set_window_icon_geometries(
        &mut self,
        geometries: Vec<tuna_shell_control::WindowIconGeometry>,
    ) -> Result<u64, ControlError> {
        let id = self.alloc_request_id();
        self.write_message(&Message::Command {
            id,
            kind: CommandKind::SetWindowIconGeometries { geometries },
        })?;
        Ok(id)
    }

    /// Where the switcher's window thumbnails sit (empty: none).
    pub fn set_switcher_thumbnails(
        &mut self,
        thumbnails: Vec<tuna_shell_control::SwitcherThumbnail>,
    ) -> Result<u64, ControlError> {
        let id = self.alloc_request_id();
        self.write_message(&Message::Command {
            id,
            kind: CommandKind::SetSwitcherThumbnails { thumbnails },
        })?;
        Ok(id)
    }

    /// The switcher's chords (GNOME's rebindable switcher keys).
    pub fn set_switcher_keys(
        &mut self,
        keys: Vec<tuna_shell_control::SwitcherKey>,
    ) -> Result<u64, ControlError> {
        let id = self.alloc_request_id();
        self.write_message(&Message::Command {
            id,
            kind: CommandKind::SetSwitcherKeys { keys },
        })?;
        Ok(id)
    }

    /// Switch to the next (or previous) keyboard input source. Returns
    /// the request id.
    pub fn switch_input_source(&mut self, backward: bool) -> Result<u64, ControlError> {
        let id = self.alloc_request_id();
        self.write_message(&Message::Command {
            id,
            kind: CommandKind::SwitchInputSource { backward },
        })?;
        Ok(id)
    }

    /// Offer the lock screen's password (ext-session-lock prompt). The
    /// compositor verifies it off its loop (PAM or greetd) and answers
    /// `Applied` once the session is unlocked or `Denied` when the
    /// password was wrong; the unlocked snapshot follows an `Applied`.
    /// Credentials are never replayed after an uncertain write or reconnect.
    /// An uncertain stream is shut down; after resnapshot only a new explicit
    /// call can send another password. Owned input/frame buffers are wiped.
    /// The password is never logged (`Secret` redacts its `Debug`).
    /// Returns the request id for `CommandResult` correlation.
    pub fn unlock(&mut self, password: String) -> Result<u64, ControlError> {
        let id = self.alloc_request_id();
        let frame =
            tuna_shell_control::encode_unlock_frame(id, tuna_shell_control::Secret(password))?;
        if self.auth_needs_snapshot {
            return Err(ControlError::Unexpected(
                "authentication requires a fresh snapshot".into(),
            ));
        }
        // No cross-generation resend, including a partial write/WouldBlock.
        if let Err(error) = self.stream.write_all(frame.as_bytes()) {
            let _ = self.stream.shutdown(std::net::Shutdown::Both);
            self.auth_needs_snapshot = true;
            self.read_buf.clear();
            self.revision = None;
            self.needs_snapshot = true;
            return Err(error.into());
        }
        Ok(id)
    }

    /// Ask the compositor to close a window (dock quit). Like
    /// `toggle_overview` this carries no activation token: closing is
    /// not focus, and unknown ids come back `Denied`. Callers must
    /// already hold the id from their own view state. Returns the
    /// request id for `CommandResult` correlation.
    pub fn close_window(&mut self, window: u64) -> Result<u64, ControlError> {
        let id = self.alloc_request_id();
        self.write_message(&Message::Command {
            id,
            kind: CommandKind::CloseWindow { window },
        })?;
        Ok(id)
    }

    /// Request activation of the currently selected window.
    ///
    /// Builds the schema `Command` carrying the compositor-minted
    /// `token` (a raw Wayland serial is never sufficient) and returns the
    /// request id so the caller can correlate the later `CommandResult`.
    /// The target is the model's selected window — the overview-list
    /// behavior this mirrors always activates out of an explicit
    /// selection. Prefer [`activate_selected`](Self::activate_selected),
    /// which sources the token from the latest shadow state.
    pub fn send_activation(&mut self, token: ActivationToken) -> Result<u64, ControlError> {
        let window = self
            .model
            .selected()
            .ok_or_else(|| ControlError::AppConstraint("no selected window".into()))?;
        let id = self.alloc_request_id();
        self.write_message(&Message::Command {
            id,
            kind: CommandKind::ActivateWindow { window, token },
        })?;
        Ok(id)
    }

    /// Read one frame and apply it, surfacing `WouldBlock` when no
    /// complete frame is available yet. A closed connected peer triggers
    /// a fresh Hello even when the user sends no command. Failed idle
    /// redials are spaced by 250 ms; ordinary reads and sends never wait.
    pub fn poll(&mut self) -> Result<Handled, ControlError> {
        match self.read_frame() {
            Ok(msg) => self.handle_message(msg),
            Err(ControlError::Io(e))
                if self.socket_path.is_some()
                    && (is_dead_peer(&e) || e.kind() == io::ErrorKind::UnexpectedEof) =>
            {
                self.input_method_admission = InputMethodAdmission::AwaitHello;
                self.native_outputs.clear();
                self.wellbeing_applied = None;
                self.wellbeing_pending = None;
                self.read_buf.clear();
                self.revision = None;
                self.needs_snapshot = true;
                let now = Instant::now();
                if self
                    .next_poll_reconnect
                    .is_some_and(|deadline| now < deadline)
                {
                    return Err(ControlError::WouldBlock);
                }
                self.next_poll_reconnect = Some(now + REDIAL_INTERVAL);
                self.reconnect()?;
                // The hub must have a turn to accept Hello and produce
                // the snapshot. Do not spin or wait inside the UI loop.
                Err(ControlError::WouldBlock)
            }
            Err(e) => {
                // A nonblocking read commonly exposes the underlying I/O
                // variant. Ordinary EAGAIN or EINTR is transient readiness,
                // not peer loss.
                let transient = matches!(&e, ControlError::WouldBlock)
                    || matches!(&e, ControlError::Io(error)
                        if matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted));
                if !transient {
                    self.input_method_admission = InputMethodAdmission::AwaitHello;
                    self.native_outputs.clear();
                }
                Err(e)
            }
        }
    }

    /// Apply one already-decoded message to client state.
    fn handle_message(&mut self, msg: Message) -> Result<Handled, ControlError> {
        match msg {
            Message::Hello { version } => {
                if version.is_compatible_with(&CURRENT_VERSION) {
                    self.input_method_admission = InputMethodAdmission::AwaitSnapshot;
                    Ok(Handled::Hello)
                } else {
                    self.input_method_admission = InputMethodAdmission::AwaitHello;
                    self.native_outputs.clear();
                    // Same-major decode already passed, so this is a newer
                    // minor we cannot speak: report it as typed, keep the
                    // model state untouched; input-method admission fails closed.
                    Ok(Handled::ServerError {
                        kind: ErrorKind::IncompatibleVersion,
                        message: format!(
                            "peer {}.{} is newer than ours {}.{}",
                            version.major,
                            version.minor,
                            CURRENT_VERSION.major,
                            CURRENT_VERSION.minor
                        ),
                    })
                }
            }
            Message::Snapshot {
                revision,
                windows,
                workspaces,
                locked,
            } => {
                self.shadow_windows = windows;
                self.shadow_workspaces = workspaces;
                self.locked = locked;
                self.model.apply_snapshot_view(snapshot_view(
                    &self.shadow_windows,
                    &self.shadow_workspaces,
                ));
                self.revision = Some(revision);
                self.needs_snapshot = false;
                self.auth_needs_snapshot = false;
                if self.input_method_admission != InputMethodAdmission::AwaitHello {
                    self.input_method_admission = InputMethodAdmission::Ready;
                }
                Ok(Handled::Snapshot { revision })
            }
            Message::Changes {
                from_revision,
                to_revision,
                ops,
            } => {
                if self.revision != Some(from_revision) {
                    // Resnapshot rule: never apply a delta onto the wrong
                    // base. Flag a fresh snapshot; the delta is dropped.
                    self.needs_snapshot = true;
                    return Ok(Handled::Gap {
                        have: self.revision,
                    });
                }
                apply_ops(&mut self.shadow_windows, &mut self.shadow_workspaces, &ops);
                self.model.apply_snapshot_view(snapshot_view(
                    &self.shadow_windows,
                    &self.shadow_workspaces,
                ));
                self.revision = Some(to_revision);
                Ok(Handled::Changes { to_revision })
            }
            Message::CommandResult { id, status } => {
                if let Some((pending_id, pair, _)) = self.wellbeing_pending {
                    if pending_id == id {
                        self.wellbeing_pending = None;
                        if matches!(status, tuna_shell_control::CommandStatus::Applied) {
                            self.wellbeing_applied = Some(pair);
                        }
                    }
                }
                Ok(Handled::CommandResult { id, status })
            }
            Message::MonitorIdentityInventory { outputs } => {
                self.monitor_identities = outputs;
                Ok(Handled::Outputs {
                    count: self.monitor_identities.len(),
                })
            }
            Message::NativeOutputInventory { outputs } => {
                self.native_outputs = outputs;
                Ok(Handled::Outputs {
                    count: self.native_outputs.len(),
                })
            }
            Message::Outputs { outputs } => {
                // Structural state, not model truth — never touches
                // revision, `needs_snapshot`, or the window list. The
                // panel reconciles its per-output surfaces against the
                // stored list on its own tick.
                self.outputs = outputs;
                Ok(Handled::Outputs {
                    count: self.outputs.len(),
                })
            }
            Message::OverviewPreviews { previews, hovered } => {
                // UI state for the shell's preview chrome, like Overview.
                self.overview_previews = (previews, hovered);
                Ok(Handled::OverviewPreviews {
                    count: self.overview_previews.0.len(),
                })
            }
            Message::Environment { vars } => {
                // Apps the shell launches inherit these (DISPLAY once
                // XWayland is up). Not model truth, like Outputs.
                crate::apps::set_launch_environment(vars.clone());
                self.environment = vars;
                Ok(Handled::Environment {
                    count: self.environment.len(),
                })
            }
            Message::WindowMenu {
                window,
                x,
                y,
                maximized,
                above,
                sticky,
                workspace_left,
                workspace_right,
            } => Ok(Handled::WindowMenu(WindowMenuRequest {
                window,
                x,
                y,
                maximized,
                above,
                sticky,
                workspace_left,
                workspace_right,
            })),
            Message::PointerOutput { name } => {
                self.pointer_output = name;
                Ok(Handled::PointerOutput)
            }
            Message::ShortcutConsent { request } => Ok(Handled::ShortcutConsent(request)),
            Message::KeyboardAidToggled { aid, enabled } => {
                Ok(Handled::KeyboardAidToggled { aid, enabled })
            }
            Message::DwellClickType { click } => Ok(Handled::DwellClickType(click)),
            Message::PointerTimeout {
                kind,
                duration_ms,
                clicked,
                x,
                y,
            } => Ok(Handled::PointerTimeout {
                kind,
                duration_ms,
                clicked,
                x,
                y,
            }),
            Message::ScreenReader { state } => Ok(Handled::ScreenReader(state)),
            Message::WorkspacePopup { index, count } => {
                Ok(Handled::WorkspacePopup { index, count })
            }
            Message::AcceleratorActivated { action, time, mode } => {
                // A grabbed key combination: the shell signals its D-Bus
                // owner. UI state; never touches the model.
                Ok(Handled::Accelerator { action, time, mode })
            }
            Message::Overview { open } => {
                // 002 R1: overview intent is UI state, not model truth —
                // never touches revision, `needs_snapshot`, or the window
                // list.
                self.model.set_overview_open(open);
                Ok(Handled::Overview { open })
            }
            Message::Switcher { action } => {
                // 002 workspaces: switcher drive is UI state too — never
                // touches revision or `needs_snapshot`. A commit selects
                // and activates through the token path, like the
                // overview's Enter path.
                let selection = match action {
                    SwitcherAction::Step { forward } => self.model.switcher_step(forward),
                    SwitcherAction::StepAllWindows { forward } => {
                        self.model.switcher_step_all_windows(forward)
                    }
                    SwitcherAction::Cycle { forward, group } => {
                        self.model.cycle_window(forward, group)
                    }
                    SwitcherAction::StepWindow { forward } => {
                        self.model.switcher_step_window(forward)
                    }
                    SwitcherAction::Key { keysym } => {
                        match self.model.switcher_key(keysym) {
                            crate::model::SwitcherEffect::None => {}
                            crate::model::SwitcherEffect::CloseWindow(id) => {
                                self.close_window(id)?;
                            }
                            crate::model::SwitcherEffect::QuitApp(ids) => {
                                for id in ids {
                                    self.close_window(id)?;
                                }
                            }
                        }
                        self.model.switcher_selection()
                    }
                    SwitcherAction::Cancel => {
                        self.model.switcher_cancel();
                        None
                    }
                    SwitcherAction::Commit => self.model.switcher_commit(),
                };
                if matches!(
                    action,
                    SwitcherAction::Commit | SwitcherAction::Cycle { .. }
                ) {
                    if let Some(id) = selection {
                        self.activate_window(id)?;
                    }
                }
                Ok(Handled::Switcher { action, selection })
            }
            Message::Error { kind, message } => {
                if kind == ErrorKind::IncompatibleVersion {
                    self.input_method_admission = InputMethodAdmission::AwaitHello;
                    self.native_outputs.clear();
                }
                if kind == ErrorKind::RevisionGap {
                    // Compositor-side gap signal: same handling as a local
                    // `from_revision` mismatch.
                    self.needs_snapshot = true;
                    Ok(Handled::Gap {
                        have: self.revision,
                    })
                } else {
                    Ok(Handled::ServerError { kind, message })
                }
            }
            Message::Command { id, .. } => Err(ControlError::Unexpected(format!(
                "compositor sent shell-side Command id {id}"
            ))),
        }
    }

    /// Encode and write one frame. `WouldBlock` propagates to the caller.
    ///
    /// Existing NONCREDENTIAL recovery resends after one fresh Hello. Its
    /// partial-write semantics need a separate audit. Credentials never use
    /// this path and are explicitly refused before generic serialization.
    fn write_message(&mut self, msg: &Message) -> Result<(), ControlError> {
        if matches!(
            msg,
            Message::Command {
                kind: CommandKind::Unlock { .. },
                ..
            }
        ) {
            return Err(ControlError::Unexpected(
                "credentials require the dedicated writer".into(),
            ));
        }
        let frame = tuna_shell_control::encode_frame(msg);
        match self.stream.write_all(&frame) {
            Ok(()) => Ok(()),
            Err(e) if is_dead_peer(&e) && self.socket_path.is_some() => {
                self.reconnect()?;
                self.stream.write_all(&frame)?;
                Ok(())
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Redial the hub after an eviction: fresh stream, cleared buffers
    /// and revision, then `Hello` — the resync signal, which the hub
    /// answers with its own `Hello` plus a full snapshot through the
    /// normal poll path (which already accepts both mid-session). Never
    /// blocks awaiting that answer; the request-id counter keeps
    /// incrementing so no id is ever reused across the boundary.
    ///
    /// Uses a raw write for the greeting, never [`write_message`](Self::write_message):
    /// a redial that is itself dead must surface its error, not recurse.
    fn reconnect(&mut self) -> Result<(), ControlError> {
        self.input_method_admission = InputMethodAdmission::AwaitHello;
        self.native_outputs.clear();
        let path = self.socket_path.clone().ok_or_else(|| {
            ControlError::Unexpected("reconnect without a socket path".to_owned())
        })?;
        let stream = UnixStream::connect(&path)?;
        stream.set_nonblocking(true)?;
        self.stream = stream;
        self.connection_generation = self.connection_generation.wrapping_add(1);
        self.closed = false;
        self.auth_needs_snapshot = true;
        self.wellbeing_applied = None;
        self.wellbeing_pending = None;
        self.read_buf.clear();
        self.revision = None;
        self.needs_snapshot = true;
        let frame = tuna_shell_control::encode_frame(&Message::Hello {
            version: CURRENT_VERSION,
        });
        self.stream.write_all(&frame)?;
        Ok(())
    }

    /// Read one complete frame, buffering partial reads. Returns
    /// `WouldBlock` (as [`ControlError::Io`]) when no complete frame is
    /// available yet; oversize/validation failures surface immediately
    /// without waiting for more bytes.
    fn read_frame(&mut self) -> Result<Message, ControlError> {
        loop {
            if self.read_buf.len() >= 4 {
                let mut prefix = [0u8; 4];
                prefix.copy_from_slice(&self.read_buf[..4]);
                let len = u32::from_le_bytes(prefix) as usize;
                if len > MAX_FRAME_BYTES {
                    return Err(ControlError::Decode(DecodeError::Oversize {
                        len,
                        max: MAX_FRAME_BYTES,
                    }));
                }
                if self.read_buf.len() >= 4 + len {
                    let msg = tuna_shell_control::decode_frame(&self.read_buf[..4 + len])?;
                    self.read_buf.drain(..4 + len);
                    return Ok(msg);
                }
            }
            let mut chunk = [0u8; 8192];
            match self.stream.read(&mut chunk) {
                Ok(0) => {
                    self.closed = true;
                    self.native_outputs.clear();
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "control connection closed",
                    )
                    .into());
                }
                Ok(n) => self.read_buf.extend_from_slice(&chunk[..n]),
                Err(e) => {
                    if !matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) {
                        self.closed = true;
                        self.native_outputs.clear();
                    }
                    return Err(ControlError::Io(e));
                }
            }
        }
    }

    /// Hand out the next shell-chosen request id (never zero).
    fn alloc_request_id(&mut self) -> u64 {
        let id = self.next_request_id;
        self.next_request_id = self.next_request_id.wrapping_add(1);
        if self.next_request_id == 0 {
            self.next_request_id = INITIAL_REQUEST_ID;
        }
        id
    }
}

/// The connected socket, for a readability watch in the caller's event
/// loop (the stream stays owned, and nonblocking, here).
impl AsFd for ControlClient {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.stream.as_fd()
    }
}

impl AsRawFd for ControlClient {
    fn as_raw_fd(&self) -> RawFd {
        self.stream.as_raw_fd()
    }
}

/// A window-menu request: where to draw GNOME's menu and the window's
/// state for its items.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowMenuRequest {
    pub window: u64,
    pub x: i32,
    pub y: i32,
    pub maximized: bool,
    pub above: bool,
    pub sticky: bool,
    pub workspace_left: bool,
    pub workspace_right: bool,
}

/// Short label for unexpected-message diagnostics.
fn message_label(msg: &Message) -> &'static str {
    match msg {
        Message::Hello { .. } => "Hello",
        Message::Snapshot { .. } => "Snapshot",
        Message::Changes { .. } => "Changes",
        Message::Command { .. } => "Command",
        Message::CommandResult { .. } => "CommandResult",
        Message::Error { .. } => "Error",
        Message::Overview { .. } => "Overview",
        Message::Switcher { .. } => "Switcher",
        Message::NativeOutputInventory { .. } => "NativeOutputInventory",
        Message::MonitorIdentityInventory { .. } => "MonitorIdentityInventory",
        Message::Outputs { .. } => "Outputs",
        Message::Environment { .. } => "Environment",
        Message::OverviewPreviews { .. } => "OverviewPreviews",
        Message::AcceleratorActivated { .. } => "AcceleratorActivated",
        Message::WindowMenu { .. } => "WindowMenu",
        Message::WorkspacePopup { .. } => "WorkspacePopup",
        Message::ScreenReader { .. } => "ScreenReader",
        Message::PointerOutput { .. } => "PointerOutput",
        Message::ShortcutConsent { .. } => "ShortcutConsent",
        Message::KeyboardAidToggled { .. } => "KeyboardAidToggled",
        Message::DwellClickType { .. } => "DwellClickType",
        Message::PointerTimeout { .. } => "PointerTimeout",
    }
}

/// Map authoritative schema state into the plain shell-host view.
///
/// `focused` becomes the list `active` flag (single-selection is
/// re-normalized by the model); workspace ids narrow `u64 -> u32` with
/// saturation to `u32::MAX` on overflow — ids are compositor-opaque, and
/// saturating keeps an absurd id visible-but-harmless instead of silently
/// colliding with a real workspace. Titles pass through byte-for-byte
/// (length already capped at decode); rendering must still treat them as
/// untrusted text.
fn snapshot_view(windows: &[WindowInfo], workspaces: &[WorkspaceInfo]) -> SnapshotView {
    SnapshotView {
        windows: windows
            .iter()
            .map(|w| {
                WindowEntry::new(w.id, w.title.clone(), w.focused)
                    .with_workspace(u32::try_from(w.workspace).unwrap_or(u32::MAX))
                    .with_app_id(w.app_id.clone())
                    .with_all_workspaces(w.on_all_workspaces)
                    .with_icon(w.icon.clone())
            })
            .collect(),
        workspaces: workspaces
            .iter()
            .map(|ws| u32::try_from(ws.id).unwrap_or(u32::MAX))
            .collect(),
        active_workspace: workspaces
            .iter()
            .find(|ws| ws.active)
            .map(|ws| u32::try_from(ws.id).unwrap_or(u32::MAX))
            .unwrap_or(0),
    }
}

/// Apply ordered state ops onto the shadow copies the view is rebuilt
/// from. `WindowOpened` restates (same id replaces, unknown id appends);
/// focus is single; moves retarget the shadow workspace; workspace
/// changes restate by id. `ShellModel` carries no per-window workspace
/// field, so moves surface in the model only via the workspace set — the
/// shadow keeps the full per-window truth for the next rebuild.
fn apply_ops(
    shadow_windows: &mut Vec<WindowInfo>,
    shadow_workspaces: &mut Vec<WorkspaceInfo>,
    ops: &[tuna_shell_control::StateOp],
) {
    use tuna_shell_control::StateOp;
    for op in ops {
        match op {
            StateOp::WindowOpened(info) => {
                if let Some(slot) = shadow_windows.iter_mut().find(|w| w.id == info.id) {
                    *slot = info.clone();
                } else {
                    shadow_windows.push(info.clone());
                }
            }
            StateOp::WindowClosed { id } => {
                shadow_windows.retain(|w| w.id != *id);
            }
            StateOp::WindowFocused { id } => {
                for w in shadow_windows.iter_mut() {
                    w.focused = w.id == *id;
                }
            }
            StateOp::WindowMoved { id, workspace } => {
                if let Some(w) = shadow_windows.iter_mut().find(|w| w.id == *id) {
                    w.workspace = *workspace;
                }
            }
            StateOp::WorkspaceChanged(info) => {
                // Active is exclusive: a newly-active workspace clears
                // the flag everywhere else, so a single op per switch
                // converges the shadow without a resnapshot.
                if info.active {
                    for ws in shadow_workspaces.iter_mut() {
                        ws.active = ws.id == info.id;
                    }
                }
                if let Some(slot) = shadow_workspaces.iter_mut().find(|ws| ws.id == info.id) {
                    *slot = info.clone();
                } else {
                    shadow_workspaces.push(info.clone());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuna_shell_control::{decode_frame, encode_frame, ProtocolVersion, WindowId, WorkspaceId};

    fn pair() -> (ControlClient, UnixStream) {
        let (ours, peer) = UnixStream::pair().expect("socketpair");
        let client = ControlClient::new(ours).expect("nonblocking client");
        (client, peer)
    }

    /// Blocking one-frame read for the in-test fake server.
    fn server_read(peer: &mut UnixStream) -> Message {
        let mut prefix = [0u8; 4];
        peer.read_exact(&mut prefix).expect("server prefix");
        let len = u32::from_le_bytes(prefix) as usize;
        let mut body = vec![0u8; len];
        peer.read_exact(&mut body).expect("server body");
        let mut frame = prefix.to_vec();
        frame.extend_from_slice(&body);
        decode_frame(&frame).expect("server decode")
    }

    fn server_write(peer: &mut UnixStream, msg: &Message) {
        peer.write_all(&encode_frame(msg)).expect("server write");
    }

    fn window(id: WindowId, title: &str, workspace: WorkspaceId, focused: bool) -> WindowInfo {
        WindowInfo {
            on_all_workspaces: false,
            icon: None,
            id,
            title: title.to_owned(),
            app_id: Some("org.example.App".to_owned()),
            workspace,
            focused,
            activation_token: format!("token-{id}"),
        }
    }

    fn workspace(id: WorkspaceId, active: bool) -> WorkspaceInfo {
        WorkspaceInfo {
            id,
            name: Some(format!("ws{id}")),
            active,
        }
    }

    /// Handshake against the fake server; returns the client plus the
    /// server end for further scripted traffic.
    fn handshook() -> (ControlClient, UnixStream) {
        let (mut client, mut peer) = pair();
        assert!(client.needs_snapshot());
        assert_eq!(client.revision(), None);
        client.send_hello().expect("send hello");
        assert_eq!(
            server_read(&mut peer),
            Message::Hello {
                version: CURRENT_VERSION
            }
        );
        server_write(
            &mut peer,
            &Message::Hello {
                version: CURRENT_VERSION,
            },
        );
        client.await_hello().expect("hello reply");
        (client, peer)
    }

    fn snapshot_rev7() -> Message {
        Message::Snapshot {
            revision: 7,
            windows: vec![
                window(7, "Terminal", 1, true),
                window(8, "Browser", 1, false),
            ],
            workspaces: vec![workspace(1, true), workspace(2, false)],
            locked: false,
        }
    }

    #[test]
    fn input_method_admission_requires_current_hello_snapshot_and_unlocked_state() {
        let (mut client, mut peer) = pair();
        assert!(!client.input_method_actions_allowed());
        // A snapshot without a compatible greeting cannot qualify this peer.
        server_write(&mut peer, &snapshot_rev7());
        client.poll().unwrap();
        assert!(!client.input_method_actions_allowed());
        server_write(
            &mut peer,
            &Message::Hello {
                version: CURRENT_VERSION,
            },
        );
        client.poll().unwrap();
        assert!(!client.input_method_actions_allowed());
        server_write(&mut peer, &snapshot_rev7());
        client.poll().unwrap();
        assert!(client.input_method_actions_allowed());
        for _ in 0..3 {
            assert!(matches!(client.poll(), Err(ControlError::Io(error))
                if error.kind() == io::ErrorKind::WouldBlock));
            assert!(client.input_method_actions_allowed());
        }
        let mut locked = snapshot_rev7();
        if let Message::Snapshot { locked, .. } = &mut locked {
            *locked = true;
        }
        server_write(&mut peer, &locked);
        client.poll().unwrap();
        assert!(!client.input_method_actions_allowed());
        server_write(&mut peer, &snapshot_rev7());
        client.poll().unwrap();
        assert!(client.input_method_actions_allowed());
        server_write(
            &mut peer,
            &Message::Changes {
                from_revision: 999,
                to_revision: 1000,
                ops: vec![],
            },
        );
        client.poll().unwrap();
        assert!(client.needs_snapshot());
        assert!(!client.input_method_actions_allowed());
        server_write(&mut peer, &snapshot_rev7());
        client.poll().unwrap();
        assert!(client.input_method_actions_allowed());
        drop(peer);
        assert!(client.poll().is_err());
        assert!(!client.input_method_actions_allowed());
    }

    #[test]
    fn input_method_admission_refuses_incompatible_peer_even_with_snapshot() {
        let (mut client, mut peer) = handshook();
        server_write(&mut peer, &snapshot_rev7());
        client.poll().unwrap();
        assert!(client.input_method_actions_allowed());
        server_write(
            &mut peer,
            &Message::Hello {
                version: ProtocolVersion::new(CURRENT_VERSION.major, CURRENT_VERSION.minor + 1),
            },
        );
        assert!(matches!(
            client.poll().unwrap(),
            Handled::ServerError { .. }
        ));
        server_write(&mut peer, &snapshot_rev7());
        client.poll().unwrap();
        assert!(!client.input_method_actions_allowed());
        server_write(
            &mut peer,
            &Message::Hello {
                version: CURRENT_VERSION,
            },
        );
        client.poll().unwrap();
        assert!(!client.input_method_actions_allowed());
        server_write(&mut peer, &snapshot_rev7());
        client.poll().unwrap();
        assert!(client.input_method_actions_allowed());
        server_write(
            &mut peer,
            &Message::Error {
                kind: ErrorKind::IncompatibleVersion,
                message: "incompatible controlled peer".into(),
            },
        );
        client.poll().unwrap();
        server_write(&mut peer, &snapshot_rev7());
        client.poll().unwrap();
        assert!(!client.input_method_actions_allowed());
    }

    #[test]
    fn input_method_admission_reconnect_never_reuses_cached_unlocked_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let (path, listener) = listener_at(&dir);
        let mut client = ControlClient::connect(&path).unwrap();
        let (mut first_peer, _) = listener.accept().unwrap();
        server_write(
            &mut first_peer,
            &Message::Hello {
                version: CURRENT_VERSION,
            },
        );
        client.poll().unwrap();
        server_write(&mut first_peer, &snapshot_rev7());
        client.poll().unwrap();
        assert!(client.input_method_actions_allowed());
        let generation = client.connection_generation();
        drop(first_peer);
        assert!(client.poll().is_err());
        assert_eq!(client.connection_generation(), generation + 1);
        assert!(!client.input_method_actions_allowed());
        let (mut second_peer, _) = listener.accept().unwrap();
        assert!(matches!(
            server_read(&mut second_peer),
            Message::Hello { .. }
        ));
        server_write(&mut second_peer, &snapshot_rev7());
        client.poll().unwrap();
        assert!(!client.input_method_actions_allowed());
        server_write(
            &mut second_peer,
            &Message::Hello {
                version: CURRENT_VERSION,
            },
        );
        client.poll().unwrap();
        assert!(!client.input_method_actions_allowed());
        server_write(&mut second_peer, &snapshot_rev7());
        client.poll().unwrap();
        assert!(client.input_method_actions_allowed());
    }

    #[test]
    fn hello_roundtrip_against_fake_server() {
        let (client, _peer) = handshook();
        // Handshake alone brings no state: still awaiting first snapshot.
        assert!(client.needs_snapshot());
        assert_eq!(client.revision(), None);
        assert!(client.model().windows().is_empty());
    }

    #[test]
    fn hello_convenience_succeeds_when_reply_is_waiting() {
        let (ours, mut peer) = UnixStream::pair().expect("socketpair");
        // Fake server answers eagerly so one blocking-style call works.
        let mut client = ControlClient::new(ours).expect("client");
        client.send_hello().expect("send");
        assert_eq!(
            server_read(&mut peer),
            Message::Hello {
                version: CURRENT_VERSION
            }
        );
        server_write(
            &mut peer,
            &Message::Hello {
                version: CURRENT_VERSION,
            },
        );
        client.await_hello().expect("await");
    }

    #[test]
    fn hello_stale_major_reports_typed_error() {
        // Typed Error reply carrying the version category.
        let (mut client, mut peer) = pair();
        client.send_hello().expect("send");
        assert!(matches!(server_read(&mut peer), Message::Hello { .. }));
        server_write(
            &mut peer,
            &Message::Error {
                kind: ErrorKind::IncompatibleVersion,
                message: "need major 9".to_owned(),
            },
        );
        match client.await_hello() {
            Err(ControlError::Remote { kind, .. }) => {
                assert_eq!(kind, ErrorKind::IncompatibleVersion);
            }
            other => panic!("expected typed Remote error, got {other:?}"),
        }

        // A stale-major Hello frame itself never decodes: the schema
        // rejects it before any message is produced.
        let (mut client, mut peer) = pair();
        client.send_hello().expect("send");
        let _ = server_read(&mut peer);
        server_write(
            &mut peer,
            &Message::Hello {
                version: ProtocolVersion::new(9, 0),
            },
        );
        match client.await_hello() {
            Err(ControlError::Decode(DecodeError::IncompatibleVersion { got, .. })) => {
                assert_eq!(got, ProtocolVersion::new(9, 0));
            }
            other => panic!("expected decode IncompatibleVersion, got {other:?}"),
        }
    }

    #[test]
    fn snapshot_applies_into_model() {
        let (mut client, mut peer) = handshook();
        server_write(&mut peer, &snapshot_rev7());
        match client.poll().expect("poll snapshot") {
            Handled::Snapshot { revision } => assert_eq!(revision, 7),
            other => panic!("expected Snapshot, got {other:?}"),
        }
        assert_eq!(client.revision(), Some(7));
        assert!(!client.needs_snapshot());
        let model = client.model();
        assert_eq!(model.windows().len(), 2);
        assert_eq!(model.selected(), Some(7));
        assert_eq!(model.workspaces(), &[1, 2]);
        assert!(!model.is_overview_open(), "snapshot keeps UI flag");
    }

    #[test]
    fn overview_intent_sets_flag_without_touching_revision() {
        let (mut client, mut peer) = handshook();
        server_write(&mut peer, &snapshot_rev7());
        assert!(matches!(
            client.poll().expect("snapshot"),
            Handled::Snapshot { .. }
        ));
        server_write(&mut peer, &Message::Overview { open: true });
        match client.poll().expect("poll overview") {
            Handled::Overview { open } => assert!(open),
            other => panic!("expected Overview, got {other:?}"),
        }
        let model = client.model();
        assert!(model.is_overview_open());
        assert_eq!(model.selected(), Some(7), "intent keeps selection");
        assert_eq!(model.windows().len(), 2, "intent keeps the list");
        assert_eq!(client.revision(), Some(7), "intent is not model truth");
        assert!(!client.needs_snapshot());

        server_write(&mut peer, &Message::Overview { open: false });
        assert!(matches!(
            client.poll().expect("poll overview close"),
            Handled::Overview { open: false }
        ));
        assert!(!client.model().is_overview_open());
        assert_eq!(client.revision(), Some(7));
    }

    #[test]
    fn environment_frame_reaches_app_launches() {
        let (mut client, mut peer) = handshook();
        server_write(
            &mut peer,
            &Message::Environment {
                vars: vec![("DISPLAY".to_owned(), ":7".to_owned())],
            },
        );
        assert!(matches!(
            client.poll().expect("environment"),
            Handled::Environment { count: 1 }
        ));
        assert_eq!(
            client.environment(),
            [("DISPLAY".to_owned(), ":7".to_owned())]
        );
        assert!(
            crate::apps::launch_environment().contains(&("DISPLAY".to_owned(), ":7".to_owned()))
        );
    }

    #[test]
    fn optional_identity_none_and_revocation_preserve_owner_outputs_and_revision() {
        let (mut client, mut peer) = handshook();
        server_write(&mut peer, &snapshot_rev7());
        client.poll().unwrap();
        let owner = tuna_shell_control::NativeOutputInfo {
            name: "eDP-1".into(),
            drm_device: 226,
            connector_id: 39,
            connector_sysfs: "/sys/owned/connector".into(),
            connector_device: 1,
            connector_inode: 2,
        };
        server_write(
            &mut peer,
            &Message::NativeOutputInventory {
                outputs: vec![owner.clone()],
            },
        );
        client.poll().unwrap();
        let info = tuna_shell_control::MonitorIdentityInfo {
            owner: owner.clone(),
            edid: None,
        };
        server_write(
            &mut peer,
            &Message::MonitorIdentityInventory {
                outputs: vec![info.clone()],
            },
        );
        client.poll().unwrap();
        assert_eq!(client.monitor_identities(), &[info]);
        assert_eq!(client.native_outputs(), std::slice::from_ref(&owner));
        server_write(
            &mut peer,
            &Message::MonitorIdentityInventory { outputs: vec![] },
        );
        client.poll().unwrap();
        assert!(client.monitor_identities().is_empty());
        assert_eq!(client.native_outputs(), &[owner]);
        assert!(client.outputs().is_empty());
        assert_eq!(client.revision(), Some(7));
    }

    #[test]
    fn native_owner_inventory_revocation_never_rewrites_legacy_outputs_or_revision() {
        let (mut client, mut peer) = handshook();
        server_write(&mut peer, &snapshot_rev7());
        client.poll().unwrap();
        let owner = tuna_shell_control::NativeOutputInfo {
            name: "eDP-1".into(),
            drm_device: 226,
            connector_id: 39,
            connector_sysfs: "/sys/owned/connector".into(),
            connector_device: 1,
            connector_inode: 2,
        };
        server_write(
            &mut peer,
            &Message::NativeOutputInventory {
                outputs: vec![owner.clone()],
            },
        );
        assert!(matches!(
            client.poll().unwrap(),
            Handled::Outputs { count: 1 }
        ));
        assert_eq!(client.native_outputs(), &[owner]);
        assert!(client.outputs().is_empty());
        assert_eq!(client.revision(), Some(7));
        server_write(
            &mut peer,
            &Message::NativeOutputInventory { outputs: vec![] },
        );
        assert!(matches!(
            client.poll().unwrap(),
            Handled::Outputs { count: 0 }
        ));
        assert!(client.native_outputs().is_empty());
        assert_eq!(client.revision(), Some(7));
    }

    #[test]
    fn native_outputs_cleared_on_failed_redial_and_transport_loss() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (path, listener) = listener_at(&dir);
        let mut client = ControlClient::connect(&path).expect("connect");
        let (mut server1, _) = listener.accept().expect("accept one");
        let _ = client.send_hello();
        assert_eq!(
            server_read(&mut server1),
            Message::Hello {
                version: CURRENT_VERSION
            }
        );
        server_write(
            &mut server1,
            &Message::Hello {
                version: CURRENT_VERSION,
            },
        );
        client.await_hello().expect("await hello");
        let owner = tuna_shell_control::NativeOutputInfo {
            name: "eDP-1".into(),
            drm_device: 226,
            connector_id: 39,
            connector_sysfs: "/sys/owned/connector".into(),
            connector_device: 1,
            connector_inode: 2,
        };
        server_write(
            &mut server1,
            &Message::NativeOutputInventory {
                outputs: vec![owner.clone()],
            },
        );
        client.poll().unwrap();
        assert_eq!(client.native_outputs(), &[owner]);

        // Transport loss and socket gone: redial fails, native_outputs must be cleared.
        drop(server1);
        drop(listener);
        std::fs::remove_file(&path).expect("remove socket");
        assert!(client.poll().is_err());
        assert!(
            client.native_outputs().is_empty(),
            "failed redial must clear connection-scoped native outputs"
        );
    }

    #[test]
    fn native_outputs_cleared_on_reconnect_without_inventory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (path, listener) = listener_at(&dir);
        let mut client = ControlClient::connect(&path).expect("connect");
        let (mut server1, _) = listener.accept().expect("accept one");
        let _ = client.send_hello();
        assert_eq!(
            server_read(&mut server1),
            Message::Hello {
                version: CURRENT_VERSION
            }
        );
        server_write(
            &mut server1,
            &Message::Hello {
                version: CURRENT_VERSION,
            },
        );
        client.await_hello().expect("await hello");
        let owner = tuna_shell_control::NativeOutputInfo {
            name: "eDP-1".into(),
            drm_device: 226,
            connector_id: 39,
            connector_sysfs: "/sys/owned/connector".into(),
            connector_device: 1,
            connector_inode: 2,
        };
        server_write(
            &mut server1,
            &Message::NativeOutputInventory {
                outputs: vec![owner.clone()],
            },
        );
        client.poll().unwrap();
        assert_eq!(client.native_outputs(), &[owner]);

        // Server 1 drops connection. Server 2 accepts reconnect.
        drop(server1);
        // Redial attempt
        let (mut server2, _) = loop {
            let _ = client.poll();
            match listener.accept() {
                Ok(stream) => break stream,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(e) => panic!("accept: {e}"),
            }
        };
        assert_eq!(
            server_read(&mut server2),
            Message::Hello {
                version: CURRENT_VERSION
            }
        );
        // While absent, compositor revoked outputs: server 2 answers with Hello and snapshot only
        server_write(
            &mut server2,
            &Message::Hello {
                version: CURRENT_VERSION,
            },
        );
        server_write(&mut server2, &snapshot_rev7());
        // Handle hello
        client.poll().unwrap();
        // Handle snapshot
        client.poll().unwrap();
        assert!(
            client.native_outputs().is_empty(),
            "reconnect without new native outputs must leave inventory empty"
        );
    }

    #[test]
    fn native_outputs_cleared_immediately_on_eof() {
        let (mut client, mut peer) = handshook();
        let owner = tuna_shell_control::NativeOutputInfo {
            name: "eDP-1".into(),
            drm_device: 226,
            connector_id: 39,
            connector_sysfs: "/sys/owned/connector".into(),
            connector_device: 1,
            connector_inode: 2,
        };
        server_write(
            &mut peer,
            &Message::NativeOutputInventory {
                outputs: vec![owner.clone()],
            },
        );
        client.poll().unwrap();
        assert_eq!(client.native_outputs(), &[owner]);

        // Peer drops stream.
        drop(peer);
        assert!(client.poll().is_err());
        assert!(
            client.native_outputs().is_empty(),
            "EOF must immediately clear native_outputs"
        );
    }
    #[test]
    fn outputs_frame_stores_inventory_without_touching_revision() {
        let (mut client, mut peer) = handshook();
        assert!(
            client.outputs().is_empty(),
            "no inventory before the first frame"
        );
        server_write(&mut peer, &snapshot_rev7());
        assert!(matches!(
            client.poll().expect("snapshot"),
            Handled::Snapshot { .. }
        ));
        let inventory = vec![
            tuna_shell_control::OutputInfo {
                name: "left".to_owned(),
                width: 1280,
                height: 800,
                primary: true,
            },
            tuna_shell_control::OutputInfo {
                name: "right".to_owned(),
                width: 1920,
                height: 1080,
                primary: false,
            },
        ];
        server_write(
            &mut peer,
            &Message::Outputs {
                outputs: inventory.clone(),
            },
        );
        match client.poll().expect("poll outputs") {
            Handled::Outputs { count } => assert_eq!(count, 2),
            other => panic!("expected Outputs, got {other:?}"),
        }
        assert_eq!(client.outputs(), inventory.as_slice());
        assert_eq!(client.revision(), Some(7), "inventory is not model truth");
        assert!(
            !client.needs_snapshot(),
            "inventory never flags a resnapshot"
        );
        assert_eq!(
            client.model().windows().len(),
            2,
            "inventory keeps the window list"
        );
    }

    #[test]
    fn changes_apply_deltas_and_bump_revision() {
        let (mut client, mut peer) = handshook();
        server_write(&mut peer, &snapshot_rev7());
        assert!(matches!(
            client.poll().expect("snapshot"),
            Handled::Snapshot { .. }
        ));
        server_write(
            &mut peer,
            &Message::Changes {
                from_revision: 7,
                to_revision: 9,
                ops: vec![
                    tuna_shell_control::StateOp::WindowOpened(window(9, "Editor", 2, false)),
                    tuna_shell_control::StateOp::WindowFocused { id: 9 },
                    tuna_shell_control::StateOp::WindowClosed { id: 7 },
                ],
            },
        );
        match client.poll().expect("poll changes") {
            Handled::Changes { to_revision } => assert_eq!(to_revision, 9),
            other => panic!("expected Changes, got {other:?}"),
        }
        assert_eq!(client.revision(), Some(9));
        let model = client.model();
        assert_eq!(model.selected(), Some(9), "focus delta moves selection");
        assert_eq!(
            model.windows().iter().map(|w| w.id).collect::<Vec<_>>(),
            vec![8, 9],
            "closed window leaves the list, opened one joins"
        );
    }

    #[test]
    fn gap_triggers_resnapshot_request() {
        let (mut client, mut peer) = handshook();
        server_write(&mut peer, &snapshot_rev7());
        assert!(matches!(
            client.poll().expect("snapshot"),
            Handled::Snapshot { .. }
        ));

        // Stale base: the delta is dropped, state is untouched, and the
        // client flags itself for a fresh snapshot.
        server_write(
            &mut peer,
            &Message::Changes {
                from_revision: 3,
                to_revision: 4,
                ops: vec![tuna_shell_control::StateOp::WindowClosed { id: 8 }],
            },
        );
        match client.poll().expect("poll gap") {
            Handled::Gap { have } => assert_eq!(have, Some(7)),
            other => panic!("expected Gap, got {other:?}"),
        }
        assert!(client.needs_snapshot());
        assert_eq!(client.revision(), Some(7), "gap keeps old revision");
        assert_eq!(client.model().windows().len(), 2, "gap applies nothing");

        // Resnapshot: re-hello goes out on the wire; the answering full
        // snapshot clears the flag.
        client.request_snapshot().expect("re-hello");
        assert_eq!(
            server_read(&mut peer),
            Message::Hello {
                version: CURRENT_VERSION
            }
        );
        server_write(
            &mut peer,
            &Message::Snapshot {
                revision: 10,
                windows: vec![window(8, "Browser", 1, true)],
                workspaces: vec![workspace(1, true)],
                locked: false,
            },
        );
        assert!(matches!(
            client.poll().expect("resnapshot"),
            Handled::Snapshot { revision: 10 }
        ));
        assert!(!client.needs_snapshot());
        assert_eq!(client.model().selected(), Some(8));
    }

    #[test]
    fn compositor_gap_signal_triggers_resnapshot() {
        let (mut client, mut peer) = handshook();
        server_write(&mut peer, &snapshot_rev7());
        assert!(matches!(
            client.poll().expect("snapshot"),
            Handled::Snapshot { .. }
        ));
        server_write(
            &mut peer,
            &Message::Error {
                kind: ErrorKind::RevisionGap,
                message: "have 7, need 12; resnapshot".to_owned(),
            },
        );
        match client.poll().expect("poll gap signal") {
            Handled::Gap { have } => assert_eq!(have, Some(7)),
            other => panic!("expected Gap, got {other:?}"),
        }
        assert!(client.needs_snapshot());
    }

    #[test]
    fn activation_returns_id_correlated_with_result() {
        let (mut client, mut peer) = handshook();
        server_write(&mut peer, &snapshot_rev7());
        assert!(matches!(
            client.poll().expect("snapshot"),
            Handled::Snapshot { .. }
        ));

        let token = ActivationToken::new("opaque-token-123".to_owned());
        let id = client.send_activation(token.clone()).expect("activate");
        assert_eq!(id, INITIAL_REQUEST_ID);
        match server_read(&mut peer) {
            Message::Command {
                id: wire_id,
                kind:
                    CommandKind::ActivateWindow {
                        window,
                        token: wire_token,
                    },
            } => {
                assert_eq!(wire_id, id, "wire id matches returned id");
                assert_eq!(window, 7, "selected window is the target");
                assert_eq!(wire_token, token);
            }
            other => panic!("expected ActivateWindow command, got {other:?}"),
        }

        // A second activation takes the next id; results correlate back
        // even when they arrive out of order with other traffic.
        let id2 = client
            .send_activation(ActivationToken::new("second-token".to_owned()))
            .expect("second activate");
        assert_ne!(id, id2);
        let _ = server_read(&mut peer);
        server_write(
            &mut peer,
            &Message::CommandResult {
                id,
                status: CommandStatus::Applied,
            },
        );
        match client.poll().expect("poll result") {
            Handled::CommandResult { id: got, status } => {
                assert_eq!(got, id, "result echoes the request id");
                assert_eq!(status, CommandStatus::Applied);
            }
            other => panic!("expected CommandResult, got {other:?}"),
        }
    }

    #[test]
    fn close_window_sends_id_command_with_fresh_id() {
        let (mut client, mut peer) = handshook();
        let id = client.close_window(9).expect("close");
        assert_eq!(id, INITIAL_REQUEST_ID);
        match server_read(&mut peer) {
            Message::Command {
                id: wire_id,
                kind: CommandKind::CloseWindow { window },
            } => {
                assert_eq!(wire_id, id, "wire id matches returned id");
                assert_eq!(window, 9, "close names the dock's window");
            }
            other => panic!("expected CloseWindow command, got {other:?}"),
        }
        // Close needs no selection: it works on an empty model too.
        assert_eq!(client.model().selected(), None);
    }

    #[test]
    fn snapshot_carries_app_id_into_entries() {
        let (mut client, mut peer) = handshook();
        server_write(&mut peer, &snapshot_rev7());
        assert!(matches!(
            client.poll().expect("snapshot"),
            Handled::Snapshot { .. }
        ));
        let entry = client
            .model()
            .windows()
            .iter()
            .find(|w| w.id == 7)
            .expect("window 7 in snapshot");
        assert_eq!(entry.app_id.as_deref(), Some("org.example.App"));
    }

    #[test]
    fn snapshot_lock_flag_mirrors_into_display_value() {
        let (mut client, mut peer) = handshook();
        assert!(!client.locked(), "no snapshot yet: unlocked");
        server_write(&mut peer, &snapshot_rev7());
        assert!(matches!(
            client.poll().expect("snapshot"),
            Handled::Snapshot { .. }
        ));
        assert!(!client.locked());
        // A locked snapshot flips the display value and strips content.
        server_write(
            &mut peer,
            &Message::Snapshot {
                revision: 8,
                windows: vec![],
                workspaces: vec![workspace(1, true)],
                locked: true,
            },
        );
        assert!(matches!(
            client.poll().expect("locked snapshot"),
            Handled::Snapshot { revision: 8 }
        ));
        assert!(client.locked());
        assert!(client.model().windows().is_empty());
        // Unlock restores the display value with content.
        server_write(&mut peer, &snapshot_rev7());
        assert!(matches!(
            client.poll().expect("unlocked snapshot"),
            Handled::Snapshot { .. }
        ));
        assert!(!client.locked());
        assert_eq!(client.model().windows().len(), 2);
    }

    #[test]
    fn toggle_overview_sends_unit_command_with_fresh_id() {
        let (mut client, mut peer) = handshook();
        let id = client.toggle_overview().expect("toggle");
        assert_eq!(id, INITIAL_REQUEST_ID);
        match server_read(&mut peer) {
            Message::Command {
                id: wire_id,
                kind: CommandKind::ToggleOverview,
            } => assert_eq!(wire_id, id, "wire id matches returned id"),
            other => panic!("expected ToggleOverview command, got {other:?}"),
        }
        // Toggle needs no selection: it works on an empty model too.
        assert_eq!(client.model().selected(), None);
    }

    #[test]
    fn screenshot_pointer_command_uses_gnome_integer_cursor_positions() {
        let (mut client, mut peer) = handshook();
        let id = client
            .move_screenshot_pointer(475.0, 399.5)
            .expect("pointer");
        assert_eq!(
            server_read(&mut peer),
            Message::Command {
                id,
                kind: CommandKind::MoveScreenshotPointer { x: 475, y: 399 },
            }
        );
    }

    #[test]
    fn lock_sends_unit_command_with_fresh_id() {
        let (mut client, mut peer) = handshook();
        let id = client.lock().expect("lock");
        assert_eq!(id, INITIAL_REQUEST_ID);
        match server_read(&mut peer) {
            Message::Command {
                id: wire_id,
                kind: CommandKind::Lock,
            } => assert_eq!(wire_id, id, "wire id matches returned id"),
            other => panic!("expected Lock command, got {other:?}"),
        }
        // Lock needs no selection: it works on an empty model too.
        assert_eq!(client.model().selected(), None);
    }

    #[test]
    fn activate_window_targets_by_id_not_shadow_selection() {
        let (mut client, mut peer) = handshook();
        server_write(&mut peer, &snapshot_rev7());
        assert!(matches!(
            client.poll().expect("snapshot"),
            Handled::Snapshot { .. }
        ));
        // Shadow selection is window 7; activating 8 must name 8.
        let id = client.activate_window(8).expect("activate by id");
        match server_read(&mut peer) {
            Message::Command {
                id: wire_id,
                kind: CommandKind::ActivateWindow { window, .. },
            } => {
                assert_eq!(wire_id, id);
                assert_eq!(window, 8);
            }
            other => panic!("expected ActivateWindow for 8, got {other:?}"),
        }
        // Unknown ids fail before anything is sent.
        match client.activate_window(99) {
            Err(ControlError::Unexpected(_)) => {}
            other => panic!("expected Unknown-window refusal, got {other:?}"),
        }
    }

    #[test]
    fn activation_without_selection_is_refused_locally() {
        let (mut client, _peer) = handshook();
        assert_eq!(client.model().selected(), None);
        match client.send_activation(ActivationToken::new("token".to_owned())) {
            Err(ControlError::AppConstraint(_)) => {}
            other => panic!("expected AppConstraint, got {other:?}"),
        }
    }

    #[test]
    fn wellbeing_effects_send_one_atomic_pair_with_correlated_request_id() {
        let (mut client, mut peer) = handshook();
        let mut previous = None;
        for (grayscale, fade) in [(false, false), (true, false), (true, true), (false, true)] {
            let id = client
                .set_wellbeing_effects(grayscale, fade)
                .expect("effect send");
            assert_eq!(
                server_read(&mut peer),
                Message::Command {
                    id,
                    kind: CommandKind::SetWellbeingEffects { grayscale, fade },
                }
            );
            if let Some(old) = previous {
                assert_ne!(id, old);
            }
            previous = Some(id);
        }
    }

    fn effect_command_id(peer: &mut UnixStream, pair: (bool, bool)) -> u64 {
        match server_read(peer) {
            Message::Command {
                id,
                kind: CommandKind::SetWellbeingEffects { grayscale, fade },
            } => {
                assert_eq!((grayscale, fade), pair);
                id
            }
            other => panic!("unexpected effect frame {other:?}"),
        }
    }

    #[test]
    fn wellbeing_disable_supersedes_pending_enable_without_waiting_for_timeout() {
        let (mut client, mut peer) = handshook();
        assert!(!client.reconcile_wellbeing_effects(true, true).unwrap());
        let enable = effect_command_id(&mut peer, (true, true));
        assert!(!client.reconcile_wellbeing_effects(false, false).unwrap());
        let disable = effect_command_id(&mut peer, (false, false));
        assert_ne!(enable, disable);
        server_write(
            &mut peer,
            &Message::CommandResult {
                id: enable,
                status: CommandStatus::Applied,
            },
        );
        client.poll().unwrap();
        assert!(client.wellbeing_applied.is_none());
        assert_eq!(client.wellbeing_pending.unwrap().0, disable);
        server_write(
            &mut peer,
            &Message::CommandResult {
                id: disable,
                status: CommandStatus::Applied,
            },
        );
        client.poll().unwrap();
        assert!(client.reconcile_wellbeing_effects(false, false).unwrap());
    }

    #[test]
    fn wellbeing_requires_matching_applied_and_bounds_lost_reply_retry() {
        let (mut client, mut peer) = handshook();
        assert!(!client.reconcile_wellbeing_effects(true, false).unwrap());
        let first = effect_command_id(&mut peer, (true, false));
        let next = client.next_request_id;
        for _ in 0..20 {
            assert!(!client.reconcile_wellbeing_effects(true, false).unwrap());
        }
        assert_eq!(
            client.next_request_id, next,
            "pending request must not flood"
        );
        server_write(
            &mut peer,
            &Message::CommandResult {
                id: first + 100,
                status: CommandStatus::Applied,
            },
        );
        client.poll().unwrap();
        assert!(client.wellbeing_applied.is_none());
        client.wellbeing_pending.as_mut().unwrap().2 = Instant::now();
        assert!(!client.reconcile_wellbeing_effects(true, false).unwrap());
        let second = effect_command_id(&mut peer, (true, false));
        assert_ne!(first, second);
        server_write(
            &mut peer,
            &Message::CommandResult {
                id: first,
                status: CommandStatus::Applied,
            },
        );
        client.poll().unwrap();
        assert!(
            client.wellbeing_applied.is_none(),
            "late result cannot qualify replacement request"
        );
        server_write(
            &mut peer,
            &Message::CommandResult {
                id: second,
                status: CommandStatus::Denied {
                    reason: "denied".into(),
                },
            },
        );
        client.poll().unwrap();
        assert!(!client.reconcile_wellbeing_effects(true, false).unwrap());
        let third = effect_command_id(&mut peer, (true, false));
        server_write(
            &mut peer,
            &Message::CommandResult {
                id: third,
                status: CommandStatus::Applied,
            },
        );
        client.poll().unwrap();
        assert!(client.reconcile_wellbeing_effects(true, false).unwrap());
        assert!(client.wellbeing_pending.is_none());
        let next = client.next_request_id;
        assert!(client.reconcile_wellbeing_effects(true, false).unwrap());
        assert_eq!(
            client.next_request_id, next,
            "acknowledged unchanged pair sends no frames"
        );
    }

    #[test]
    fn wellbeing_replays_unchanged_pair_after_real_idle_reconnect() {
        let dir = tempfile::tempdir().unwrap();
        let (path, listener) = listener_at(&dir);
        let mut client = ControlClient::connect(&path).unwrap();
        let (mut first_peer, _) = listener.accept().unwrap();
        assert!(!client.reconcile_wellbeing_effects(true, true).unwrap());
        let first = effect_command_id(&mut first_peer, (true, true));
        server_write(
            &mut first_peer,
            &Message::CommandResult {
                id: first,
                status: CommandStatus::Applied,
            },
        );
        client.poll().unwrap();
        assert!(client.reconcile_wellbeing_effects(true, true).unwrap());
        drop(first_peer);
        assert!(client.poll().is_err());
        assert_eq!(client.connection_generation(), 1);
        let (mut second_peer, _) = listener.accept().unwrap();
        assert!(matches!(
            server_read(&mut second_peer),
            Message::Hello { .. }
        ));
        assert!(!client.reconcile_wellbeing_effects(true, true).unwrap());
        let second = effect_command_id(&mut second_peer, (true, true));
        assert_ne!(first, second);
        server_write(
            &mut second_peer,
            &Message::CommandResult {
                id: first,
                status: CommandStatus::Applied,
            },
        );
        client.poll().unwrap();
        assert!(client.wellbeing_applied.is_none());
        server_write(
            &mut second_peer,
            &Message::CommandResult {
                id: second,
                status: CommandStatus::Applied,
            },
        );
        client.poll().unwrap();
        assert!(client.reconcile_wellbeing_effects(true, true).unwrap());
    }

    #[test]
    fn wellbeing_failed_redial_invalidates_previous_application() {
        let dir = tempfile::tempdir().unwrap();
        let (path, listener) = listener_at(&dir);
        let mut client = ControlClient::connect(&path).unwrap();
        let (mut peer, _) = listener.accept().unwrap();
        assert!(!client.reconcile_wellbeing_effects(false, false).unwrap());
        let id = effect_command_id(&mut peer, (false, false));
        server_write(
            &mut peer,
            &Message::CommandResult {
                id,
                status: CommandStatus::Applied,
            },
        );
        client.poll().unwrap();
        drop(peer);
        drop(listener);
        std::fs::remove_file(&path).unwrap();
        assert!(client.poll().is_err());
        assert!(client.wellbeing_applied.is_none());
        assert!(client.reconcile_wellbeing_effects(false, false).is_err());
        assert!(client.wellbeing_pending.is_none());
    }

    #[test]
    fn idle_poll_surfaces_would_block() {
        let (mut client, _peer) = handshook();
        match client.poll() {
            Err(ControlError::Io(e)) => {
                assert_eq!(e.kind(), io::ErrorKind::WouldBlock);
            }
            other => panic!("expected WouldBlock, got {other:?}"),
        }
    }

    /// Bound listener for reconnect tests: the client dials the path,
    /// like against the real hub, so a dropped server end exercises
    /// the redial path instead of a dead socketpair.
    fn listener_at(
        dir: &tempfile::TempDir,
    ) -> (std::path::PathBuf, std::os::unix::net::UnixListener) {
        let path = dir.path().join("control.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind");
        (path, listener)
    }

    fn initial_auth_snapshot(client: &mut ControlClient, peer: &mut UnixStream) {
        client.send_hello().unwrap();
        assert!(matches!(server_read(peer), Message::Hello { .. }));
        server_write(
            peer,
            &Message::Snapshot {
                revision: 1,
                windows: vec![],
                workspaces: vec![],
                locked: true,
            },
        );
        assert!(matches!(client.poll().unwrap(), Handled::Snapshot { .. }));
    }
    #[test]
    fn initial_authentication_requires_authoritative_snapshot_before_credentials() {
        let (mut client, mut peer) = pair();
        peer.set_nonblocking(true).unwrap();
        assert!(client.unlock("before-initial-snapshot".into()).is_err());
        let mut byte = [0u8; 1];
        assert!(matches!(peer.read(&mut byte),Err(e) if e.kind()==io::ErrorKind::WouldBlock));
        peer.set_nonblocking(false).unwrap();
        initial_auth_snapshot(&mut client, &mut peer);
        let id = client
            .unlock("explicit-after-initial-snapshot".into())
            .unwrap();
        assert_eq!(
            server_read(&mut peer),
            Message::Command {
                id,
                kind: CommandKind::Unlock {
                    password: tuna_shell_control::Secret("explicit-after-initial-snapshot".into())
                }
            }
        );
    }
    fn auth_reconnect_only_hello(
        client: &mut ControlClient,
        listener: &std::os::unix::net::UnixListener,
    ) -> UnixStream {
        assert!(matches!(client.poll(), Err(ControlError::WouldBlock)));
        let (mut peer, _) = listener.accept().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        assert!(matches!(server_read(&mut peer), Message::Hello { .. }));
        peer.set_nonblocking(true).unwrap();
        let mut byte = [0u8; 1];
        assert!(
            matches!(peer.read(&mut byte),Err(e) if e.kind()==io::ErrorKind::WouldBlock),
            "no credential replay in new generation"
        );
        assert!(client.unlock("explicit-before-snapshot".into()).is_err());
        assert!(matches!(peer.read(&mut byte),Err(e) if e.kind()==io::ErrorKind::WouldBlock));
        peer.set_nonblocking(false).unwrap();
        server_write(
            &mut peer,
            &Message::Snapshot {
                revision: 1,
                windows: vec![],
                workspaces: vec![],
                locked: true,
            },
        );
        assert!(matches!(client.poll().unwrap(), Handled::Snapshot { .. }));
        peer
    }
    #[test]
    fn authentication_disconnect_never_replays_and_fresh_action_after_snapshot_can_send() {
        let dir = tempfile::tempdir().unwrap();
        let (path, listener) = listener_at(&dir);
        let mut client = ControlClient::connect(&path).unwrap();
        let (mut peer, _) = listener.accept().unwrap();
        initial_auth_snapshot(&mut client, &mut peer);
        drop(peer);
        assert!(client.unlock("old-controlled-secret".into()).is_err());
        assert_eq!(
            client.connection_generation(),
            0,
            "credential failure must not redial/resend"
        );
        let mut new_peer = auth_reconnect_only_hello(&mut client, &listener);
        let id = client
            .unlock("explicit-new-controlled-secret".into())
            .unwrap();
        assert_eq!(
            server_read(&mut new_peer),
            Message::Command {
                id,
                kind: CommandKind::Unlock {
                    password: tuna_shell_control::Secret("explicit-new-controlled-secret".into())
                }
            }
        );
    }
    #[test]
    fn partial_authentication_write_closes_uncertain_stream_and_never_contaminates_next() {
        let dir = tempfile::tempdir().unwrap();
        let (path, listener) = listener_at(&dir);
        let mut client = ControlClient::connect(&path).unwrap();
        let (mut old_peer, _) = listener.accept().unwrap();
        initial_auth_snapshot(&mut client, &mut old_peer);
        rustix::net::sockopt::set_socket_send_buffer_size(&client.stream, 1024).unwrap();
        let password = "controlled-p".repeat(8192);
        let expected = tuna_shell_control::encode_unlock_frame(
            1,
            tuna_shell_control::Secret(password.clone()),
        )
        .unwrap();
        assert!(matches!(
            client.unlock(password),
            Err(ControlError::WouldBlock)
        ));
        old_peer
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut partial = Vec::new();
        old_peer.read_to_end(&mut partial).unwrap();
        assert!(
            !partial.is_empty() && partial.len() < expected.as_bytes().len(),
            "real kernel partial write required"
        );
        assert_eq!(partial, expected.as_bytes()[..partial.len()]);
        drop(old_peer);
        let mut new_peer = auth_reconnect_only_hello(&mut client, &listener);
        let id = client.unlock("fresh-input".into()).unwrap();
        assert_eq!(
            server_read(&mut new_peer),
            Message::Command {
                id,
                kind: CommandKind::Unlock {
                    password: tuna_shell_control::Secret("fresh-input".into())
                }
            }
        );
    }
    #[test]
    fn uncertain_caller_stream_without_redial_path_stays_closed() {
        let (mut client, mut peer) = pair();
        initial_auth_snapshot(&mut client, &mut peer);
        drop(peer);
        assert!(client.unlock("controlled-input".into()).is_err());
        assert!(client
            .unlock("new-input-without-new-transport".into())
            .is_err());
        assert!(client.poll().is_err());
        assert_eq!(client.connection_generation(), 0);
    }

    #[test]
    fn evicted_session_reconnects_and_resends_over_hello() {
        use std::time::{Duration, Instant};

        let dir = tempfile::tempdir().expect("tempdir");
        let (path, listener) = listener_at(&dir);
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");

        let mut client = ControlClient::connect(&path).expect("connect");
        let (mut server1, _) = listener.accept().expect("accept one");
        client.send_hello().expect("hello");
        assert_eq!(
            server_read(&mut server1),
            Message::Hello {
                version: CURRENT_VERSION
            }
        );

        // The hub evicts and closes our session (spec R6): the shell
        // sees EOF on its next poll, then EPIPE on its next send.
        drop(server1);
        let id = client.toggle_overview().expect("resend after reconnect");

        // The reconnect redials: Hello (the resync signal), then the
        // original command with its original request id.
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut server2 = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "reconnect redial timed out");
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(e) => panic!("accept: {e}"),
            }
        };
        assert_eq!(
            server_read(&mut server2),
            Message::Hello {
                version: CURRENT_VERSION
            }
        );
        assert_eq!(
            server_read(&mut server2),
            Message::Command {
                id,
                kind: CommandKind::ToggleOverview
            }
        );
        // Fresh-session semantics: no revision until the new snapshot
        // lands through the normal poll path.
        assert!(client.needs_snapshot());
        assert_eq!(client.revision(), None);
    }

    #[test]
    fn reconnect_failure_returns_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (path, listener) = listener_at(&dir);
        let mut client = ControlClient::connect(&path).expect("connect");
        let (server1, _) = listener.accept().expect("accept one");
        // Hub and socket both gone: the redial must fail cleanly, not
        // hang or recurse.
        drop(server1);
        drop(listener);
        std::fs::remove_file(&path).expect("remove socket");
        assert!(client.poll().is_err());
        assert!(
            client.is_closed(),
            "a failed redial leaves the client closed"
        );
        assert!(client.toggle_overview().is_err());
    }

    #[test]
    fn idle_evicted_session_reconnects_and_receives_fresh_snapshot() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (path, listener) = listener_at(&dir);
        let mut client = ControlClient::connect(&path).expect("connect");
        let (mut server1, _) = listener.accept().expect("accept one");
        server_write(
            &mut server1,
            &Message::Snapshot {
                revision: 7,
                windows: vec![],
                workspaces: vec![],
                locked: false,
            },
        );
        assert!(matches!(
            client.poll(),
            Ok(Handled::Snapshot { revision: 7 })
        ));
        // An incomplete old frame must not contaminate the new stream.
        server1.write_all(&[10, 0, 0, 0, 1]).expect("partial frame");
        let generation = client.connection_generation();
        drop(server1);
        assert!(matches!(client.poll(), Err(ControlError::WouldBlock)));
        assert!(client.needs_snapshot());
        assert_eq!(client.revision(), None);
        // The redial is a fresh, open stream a readability watch re-arms on.
        assert!(!client.is_closed());
        assert_eq!(client.connection_generation(), generation.wrapping_add(1));

        let (mut server2, _) = listener.accept().expect("idle reconnect");
        assert_eq!(
            server_read(&mut server2),
            Message::Hello {
                version: CURRENT_VERSION
            }
        );
        server_write(
            &mut server2,
            &Message::Hello {
                version: CURRENT_VERSION,
            },
        );
        server_write(
            &mut server2,
            &Message::Snapshot {
                revision: 12,
                windows: vec![],
                workspaces: vec![],
                locked: true,
            },
        );
        assert!(matches!(client.poll(), Ok(Handled::Hello)));
        assert!(matches!(
            client.poll(),
            Ok(Handled::Snapshot { revision: 12 })
        ));
        assert!(!client.needs_snapshot());
        assert!(client.locked());
        assert_eq!(client.revision(), Some(12));
        assert_eq!(client.next_request_id, INITIAL_REQUEST_ID);
    }

    #[test]
    fn unavailable_hub_limits_idle_redials() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (path, listener) = listener_at(&dir);
        let mut client = ControlClient::connect(&path).expect("connect");
        let (server, _) = listener.accept().expect("accept");
        drop(server);
        drop(listener);
        std::fs::remove_file(&path).expect("remove socket");
        assert!(matches!(client.poll(), Err(ControlError::Io(_))));
        // Fix the deadline well into the future to make the cooldown
        // assertion independent of a loaded test host's timing.
        let deadline = Instant::now() + Duration::from_secs(60);
        client.next_poll_reconnect = Some(deadline);
        assert!(matches!(client.poll(), Err(ControlError::WouldBlock)));
        assert_eq!(client.next_poll_reconnect, Some(deadline));
        assert!(client.needs_snapshot());
    }

    #[test]
    fn supplied_stream_reports_eof_without_redial() {
        let (mut client, peer) = pair();
        drop(peer);
        assert!(matches!(client.poll(), Err(ControlError::Io(e))
            if e.kind() == io::ErrorKind::UnexpectedEof));
    }

    #[test]
    fn frames_read_ahead_show_as_buffered_until_polled() {
        let (mut client, mut peer) = handshook();
        // Two frames in one write: the first read takes both off the
        // socket, so only the buffer still holds the second.
        let mut both = encode_frame(&snapshot_rev7());
        both.extend_from_slice(&encode_frame(&Message::Overview { open: true }));
        peer.write_all(&both).expect("server write");
        assert!(!client.has_buffered_frame());
        assert_eq!(
            client.poll().expect("snapshot"),
            Handled::Snapshot { revision: 7 }
        );
        assert!(client.has_buffered_frame());
        assert_eq!(
            client.poll().expect("overview"),
            Handled::Overview { open: true }
        );
        assert!(!client.has_buffered_frame());
        // Half a frame is not a frame.
        let frame = encode_frame(&Message::Overview { open: false });
        peer.write_all(&frame[..frame.len() - 1])
            .expect("server write");
        assert!(client.poll().is_err());
        assert!(!client.has_buffered_frame());
        assert!(!client.is_closed());
    }

    #[test]
    fn a_hung_up_peer_marks_the_client_closed() {
        let (mut client, peer) = handshook();
        assert!(client.poll().is_err());
        assert!(!client.is_closed(), "WouldBlock is not a hang-up");
        // Shut the peer down instead of only dropping it: a test thread that
        // forks a child concurrently holds a copy of every fd until exec,
        // which keeps a merely dropped peer open.
        peer.shutdown(std::net::Shutdown::Both)
            .expect("peer shutdown");
        drop(peer);
        assert!(matches!(client.poll(), Err(ControlError::Io(e))
            if e.kind() == io::ErrorKind::UnexpectedEof));
        assert!(client.is_closed());
    }

    #[test]
    fn the_socket_is_exposed_for_a_readability_watch() {
        let (client, _peer) = pair();
        assert_eq!(client.as_fd().as_raw_fd(), client.as_raw_fd());
        assert!(client.as_raw_fd() >= 0);
    }
}
