//! Compositor-side control channel (001 R4/R6, ADR 0002).
//!
//! Private local IPC between the compositor (authoritative) and the shell
//! host. Wire types and framing live in the `tuna-shell-control` crate
//! (version handshake, length-prefixed frames, 1 MiB cap); this module owns
//! the compositor side: accept, handshake, snapshots, change deltas, and
//! command dispatch over a [`StateModel`].
//!
//! Nonblocking contract (spec R6): every socket here is nonblocking. No
//! method in this module blocks: when a read or write cannot proceed without
//! blocking it returns [`ControlError::WouldBlock`] immediately and the
//! caller is expected to wait for readiness (e.g. via poll) and retry, or —
//! for a peer that stays unready, per ADR 0002 backpressure — drop the
//! connection. A slow shell is disconnected; compositor input/frame paths
//! never wait on it. Note a `WouldBlock` from [`ControlConn::write_frame`]
//! may leave a partial frame behind, so the connection must be dropped
//! rather than retried once backpressure has been observed.
//!
//! Session rules (ADR 0002): full [`Message::Snapshot`] on every (re)connect
//! and after any revision gap (resnapshot rule — the shell discards state
//! and renders from the fresh snapshot); ordered [`Message::Changes`] deltas
//! otherwise; typed [`Message::Error`] for malformed, oversized,
//! stale-version, and unknown-kind frames, with the offending frame dropped.
//!
//! Activation policy: [`Session`] validates `ActivateWindow` commands through
//! a caller-supplied `Fn(&ActivationToken, Option<&str>) -> bool` (token,
//! target window `app_id`) so policy stays injectable. The default
//! ([`deny_all_tokens`]) denies everything (fail-closed); the live policy
//! (30 s expiry, one-use, seat binding, `app_id` match) is the
//! [`TokenStore`](crate::state::TokenStore) per ADR 0002, not protocol
//! behavior.
//!
//! Reference patterns (framing/versioning discipline, snapshot-on-connect)
//! follow the niri survey in `.spektacular/work/wave2/stream1-notes.md`;
//! all code below is original.

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};

use tuna_shell_control::{
    decode_frame, encode_frame, ActivationToken, CommandKind, CommandStatus, DecodeError,
    ErrorKind, Message, OutputInfo, ProtocolVersion, StateOp, SwitcherAction, WorkspaceInfo,
    CURRENT_VERSION, MAX_FRAME_BYTES,
};

use crate::lockdown::{LockdownPolicy, RestrictedAction};
use crate::state::{StateChange, StateModel, TokenPolicy, TokenStore, WindowEntry};

/// How soon the hub wants another [`ControlHub::poll`] while a handshake
/// is due or a broadcast is owed after backpressure (one display frame).
pub const RETRY: std::time::Duration = std::time::Duration::from_millis(16);

/// How often the hub reissues every window's activation token: half the
/// token lifetime, so the shell always holds one that has not expired.
pub const TOKEN_REISSUE: std::time::Duration =
    std::time::Duration::from_millis(TokenPolicy::MAX_AGE_MS / 2);

/// Transport and protocol failures, shared with the shell-host endpoint.
///
/// Defined in [`tuna_shell_control`] so both endpoints name the same type;
/// re-exported here because `tuna_compositor::control::ControlError` is the
/// path the rest of this crate (and its tests) use.
pub use tuna_shell_control::ControlError;

/// Bound control socket. Wraps an already-bound [`UnixListener`] (tests pass
/// a bound socket or use a socketpair directly with [`ControlConn`]).
pub struct ControlServer {
    listener: UnixListener,
}

impl ControlServer {
    /// Take ownership of an already-bound listener and put it in
    /// nonblocking mode, so [`accept`](Self::accept) is a try-accept.
    pub fn new(listener: UnixListener) -> std::io::Result<Self> {
        listener.set_nonblocking(true)?;
        Ok(Self { listener })
    }

    /// Accept one pending connection. Returns [`ControlError::WouldBlock`]
    /// when no peer is waiting; never blocks.
    pub fn accept(&self) -> Result<ControlConn, ControlError> {
        match self.listener.accept() {
            Ok((stream, _)) => ControlConn::new(stream),
            Err(e) => Err(ControlError::from(e)),
        }
    }
}

/// One control connection: framed, nonblocking, schema-validated transport.
///
/// `read_frame` returns whole [`Message`]s (u32-LE length prefix plus the
/// schema crate's decode, including the 1 MiB cap and stale-major reject);
/// `write_frame` sends one. Both surface [`ControlError::WouldBlock`]
/// instead of blocking (spec R6).
pub struct ControlConn {
    stream: UnixStream,
    rbuf: Vec<u8>,
}

impl ControlConn {
    /// Wrap a connected stream and put it in nonblocking mode.
    pub fn new(stream: UnixStream) -> Result<Self, ControlError> {
        stream.set_nonblocking(true)?;
        Ok(Self {
            stream,
            rbuf: Vec::new(),
        })
    }

    /// Read one complete frame. Buffers short reads internally and returns
    /// [`ControlError::WouldBlock`] until a whole frame has arrived; the
    /// length cap is enforced from the prefix before any body is awaited.
    pub fn read_frame(&mut self) -> Result<Message, ControlError> {
        loop {
            if self.rbuf.len() >= 4 {
                let len =
                    u32::from_le_bytes([self.rbuf[0], self.rbuf[1], self.rbuf[2], self.rbuf[3]])
                        as usize;
                if len > MAX_FRAME_BYTES {
                    return Err(ControlError::Decode(DecodeError::Oversize {
                        len,
                        max: MAX_FRAME_BYTES,
                    }));
                }
                if self.rbuf.len() >= 4 + len {
                    let frame = self.rbuf[..4 + len].to_vec();
                    self.rbuf.drain(..4 + len);
                    return decode_frame(&frame).map_err(ControlError::Decode);
                }
            }
            let mut chunk = [0u8; 8192];
            match self.stream.read(&mut chunk) {
                Ok(0) => {
                    if self.rbuf.is_empty() {
                        return Err(ControlError::Unexpected("peer closed".into()));
                    }
                    return Err(ControlError::Decode(DecodeError::Truncated {
                        expected: self.rbuf.len(),
                        actual: self.rbuf.len(),
                    }));
                }
                Ok(n) => self.rbuf.extend_from_slice(&chunk[..n]),
                Err(e) => return Err(ControlError::from(e)),
            }
        }
    }

    /// Observe half/full closure without consuming queued command bytes.
    fn peer_live(&self) -> bool {
        use std::os::fd::AsRawFd;
        let fd = self.stream.as_raw_fd();
        let mut poll = libc::pollfd {
            fd,
            events: libc::POLLIN | libc::POLLRDHUP,
            revents: 0,
        };
        if unsafe { libc::poll(&mut poll, 1, 0) } < 0
            || poll.revents & (libc::POLLHUP | libc::POLLRDHUP | libc::POLLERR | libc::POLLNVAL)
                != 0
        {
            return false;
        }
        let mut byte = 0u8;
        let count = unsafe {
            libc::recv(
                fd,
                (&mut byte as *mut u8).cast(),
                1,
                libc::MSG_PEEK | libc::MSG_DONTWAIT,
            )
        };
        count > 0
            || (count < 0
                && std::io::Error::last_os_error().kind() == std::io::ErrorKind::WouldBlock)
    }

    /// Write one message as a single frame. Returns
    /// [`ControlError::WouldBlock`] on backpressure; per the module docs the
    /// connection must then be dropped (a partial frame may be on the wire).
    pub fn write_frame(&mut self, msg: &Message) -> Result<(), ControlError> {
        let frame = encode_frame(msg);
        let mut written = 0;
        while written < frame.len() {
            match self.stream.write(&frame[written..]) {
                Ok(0) => return Err(ControlError::Unexpected("peer closed on write".into())),
                Ok(n) => written += n,
                Err(e) => return Err(ControlError::from(e)),
            }
        }
        Ok(())
    }

    /// Whether the next [`read_frame`](Self::read_frame) can answer from
    /// the buffer alone: a whole frame (or an oversize prefix, which fails
    /// at once) already arrived. The socket is then not readable, so the
    /// loop must come back without waiting for it.
    pub fn has_buffered_frame(&self) -> bool {
        if self.rbuf.len() < 4 {
            return false;
        }
        let len =
            u32::from_le_bytes([self.rbuf[0], self.rbuf[1], self.rbuf[2], self.rbuf[3]]) as usize;
        len > MAX_FRAME_BYTES || self.rbuf.len() >= 4 + len
    }
}

impl std::os::fd::AsFd for ControlConn {
    fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        self.stream.as_fd()
    }
}

/// Fail-closed activation-token validator: denies every token.
///
/// This is the [`Session`] default so that forgetting to wire real policy
/// denies rather than allows (ADR 0002 policy: 30 s expiry, one-use, seat
/// binding, `app_id` match — compositor policy code, injected here). The
/// expected `app_id` is the target window's, looked up from the model by
/// [`apply_command`].
pub fn deny_all_tokens(_: &ActivationToken, _: Option<&str>) -> bool {
    false
}

/// Outcome of [`Session::handle_next`]: what one inbound frame produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handled {
    /// A shell command was answered with `CommandResult`.
    CommandResult {
        /// Echo of the shell's request id.
        id: u64,
        /// Whether the command was applied (false = denied).
        applied: bool,
    },
    /// A typed `Error` was sent for a bad frame or unexpected message kind.
    ErrorSent {
        /// Category that was sent.
        kind: ErrorKind,
    },
    /// A mid-session `Hello` was treated as a reconnect: `Hello` plus a
    /// fresh snapshot were sent.
    HelloResync {
        /// Revision of the snapshot that was sent.
        revision: u64,
    },
}

/// Outcome of [`Session::emit_deltas`]: how the shell was caught up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Emitted {
    /// Incremental `Changes` were sent.
    Changes {
        /// Shell revision the deltas apply from.
        from: u64,
        /// Revision the shell holds after applying them.
        to: u64,
        /// Number of ops sent.
        ops: usize,
    },
    /// A [`RevisionGap`](crate::state::RevisionGap) forced a fresh snapshot
    /// (resnapshot rule).
    Snapshot {
        /// Revision of the snapshot that was sent.
        revision: u64,
    },
    /// Shell was already current; cursor advanced, nothing sent.
    Idle {
        /// Current revision.
        revision: u64,
    },
}

/// Token-check hook for `ActivateWindow` commands.
pub type TokenValidator<'a> = Box<dyn Fn(&ActivationToken, Option<&str>) -> bool + 'a>;
/// Per-window activation-token minter for snapshots and deltas.
pub type TokenMinter = std::rc::Rc<dyn Fn(Option<&str>) -> String>;

/// Current supervised process generation, never supplied by the shell wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuthOwner {
    pub pid: u32,
    pub generation: u64,
}
/// A routing lease only: no authentication or physical-retirement authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuthLease {
    origin: crate::auth_owner::Origin,
    serial: u64,
    epoch: u64,
    method: crate::auth_owner::MethodKind,
    owner: AuthOwner,
}

/// Authenticated control session over a [`ControlConn`].
///
/// The session owns the connection, the token-check hook, the per-window
/// token minter, and the last revision the shell is known to hold. The
/// [`StateModel`] itself is passed per call (`&mut` where commands may
/// mutate, `&` otherwise) so callers can drive the model between session
/// calls. Build with [`handshake`](Self::handshake) (fail-closed tokens)
/// or [`handshake_with`](Self::handshake_with) (live policy); the
/// handshake sends our `Hello` plus a full snapshot, so every (re)connect
/// starts from complete state.
pub struct Session<'a> {
    connection_id: u64,
    auth_owner: Option<AuthOwner>,
    auth_lease: Option<AuthLease>,
    conn: ControlConn,
    validator: TokenValidator<'a>,
    minter: TokenMinter,
    last_revision: u64,
    /// Shared overview intent with the hub (002 R1): the shell-facing
    /// open state flipped by `ToggleOverview` commands and runtime
    /// triggers, broadcast back as [`Message::Overview`].
    overview: std::rc::Rc<std::cell::Cell<bool>>,
    /// Compositor-owned session-lock flag shared with the hub: the
    /// runtime (idle timeout) and `Lock` commands flip it, and every
    /// snapshot/delta path reads it. The flag lives here in the
    /// compositor, never in the restartable shell.
    locked: std::rc::Rc<std::cell::Cell<bool>>,
    /// Lock value the shell last received. A flip forces a fresh
    /// snapshot (resnapshot rule for lock transitions): locking strips
    /// window content, unlocking restores it.
    locked_sent: bool,
    peer_minor: u16,
    /// Window ids the shell asked to close this round (`CloseWindow`
    /// commands the mirror applied). Drained by [`ControlHub::poll`]
    /// so the runtime can send the polite client close.
    closed: Vec<u64>,
    /// Latest `SetIdleTimeout` from the shell, drained by the hub.
    idle_timeout: Option<u64>,
    /// The latest accelerator grabs the shell sent, until drained.
    accelerators: Option<Vec<tuna_shell_control::AcceleratorGrab>>,
    /// Window-menu actions the shell asked for, until drained.
    window_actions: Vec<(u64, tuna_shell_control::WindowAction)>,
    shortcut_consent: Vec<(u64, bool)>,
    /// Display-change dialog answers (`true` keeps), drained by the hub.
    display_change: Vec<bool>,
    /// Latest `SetOverviewSearch` from the shell, drained by the hub.
    overview_search: Option<bool>,
    /// Latest `SetOverviewAppGrid` from the shell, drained by the hub.
    overview_app_grid: Option<bool>,
    /// A lock-screen password awaiting verification, with its request
    /// id: the reply waits for the result (see `ControlHub::finish_unlock`).
    unlock_request: Option<(u64, tuna_shell_control::Secret)>,
    /// The request id whose `CommandResult` is still owed: a legacy
    /// `Unlock` or a password `BeginVerification` (never both at once).
    unlock_pending: Option<u64>,
    /// The fingerprint `BeginVerification` whose result is still owed.
    fingerprint_pending: Option<u64>,
    /// Conversations the shell started this round, drained by the hub.
    auth_begin: Vec<(u64, tuna_shell_control::AuthService)>,
    /// Query answers the shell sent this round, drained by the hub.
    auth_answers: Vec<(
        tuna_shell_control::AuthService,
        u32,
        tuna_shell_control::Secret,
    )>,
    /// The shell asked to stop its conversations.
    auth_cancel: bool,
    /// Latest `SetInputSettings` from the shell, drained by the hub.
    input_settings: Option<tuna_shell_control::InputSettings>,
    wellbeing_effects: Option<(bool, bool)>,
    icon_geometries: Option<Vec<tuna_shell_control::WindowIconGeometry>>,
    screen_reader: Option<bool>,
    screenshot_pointer: Option<(i32, i32)>,
    /// The hover-click chooser's latest pick, drained by the hub.
    dwell_click: Option<tuna_shell_control::DwellClick>,
    /// On-screen keyboard input the shell typed, drained by the hub (#349).
    osk_inputs: Vec<tuna_shell_control::OskInput>,
    /// Input-source switches the shell asked for (`true` backward).
    input_source_switches: Vec<bool>,
    /// The switcher's thumbnail frames, when the shell sent new ones.
    switcher_thumbnails: Option<Vec<tuna_shell_control::SwitcherThumbnail>>,
    /// The shell's switcher keys, until the runtime drains them.
    switcher_keys: Option<Vec<tuna_shell_control::SwitcherKey>>,
    /// Administrative lockdown (#357): the hub's policy at accept,
    /// refreshed onto every session by [`ControlHub::refresh_lockdown`].
    /// `Lock` commands consult it; a restrictive key denies them with
    /// the administrator's reason instead of engaging the lock.
    lockdown: LockdownPolicy,
    /// The hub's pads (id, device node) a `RequestPadOsd` may name.
    pad_targets: std::rc::Rc<Vec<(u32, String)>>,
    /// Admitted `RequestPadOsd`s (pad id, edition mode), drained by the hub.
    pad_osd_requests: Vec<(u32, bool)>,
    /// `PadOsdClosed`s, drained by the hub.
    pad_osd_closed: Vec<u32>,
}

impl std::fmt::Debug for Session<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("last_revision", &self.last_revision)
            .finish_non_exhaustive()
    }
}

impl<'a> Session<'a> {
    /// Whether a whole command frame waits in this session's buffer
    /// (see [`ControlConn::has_buffered_frame`]).
    pub fn has_buffered_frame(&self) -> bool {
        self.conn.has_buffered_frame()
    }

    /// Whether [`emit_deltas`](Self::emit_deltas) has something to send:
    /// the lock flag flipped or the model moved since the last emit.
    pub fn owes_deltas(&self, model: &StateModel) -> bool {
        self.locked.get() != self.locked_sent || model.revision() != self.last_revision
    }

    /// Server-side handshake: expect `Hello` first, reply `Hello` with
    /// [`CURRENT_VERSION`], then send a full snapshot. A stale major gets a
    /// typed `IncompatibleVersion` error and the connection is refused
    /// (caller drops this result's absence — there is no session to use).
    /// Uses [`deny_all_tokens`] (fail-closed).
    pub fn handshake(conn: ControlConn, model: &StateModel) -> Result<Self, ControlError> {
        Self::handshake_with(
            conn,
            model,
            deny_all_tokens,
            std::rc::Rc::new(|_| String::new()),
            std::rc::Rc::new(std::cell::Cell::new(false)),
            std::rc::Rc::new(std::cell::Cell::new(false)),
        )
    }

    /// [`handshake`](Self::handshake) with an injectable activation-token
    /// policy for `ActivateWindow` commands plus the per-window token
    /// minter used for snapshots and deltas. In live sessions both come
    /// from one [`TokenStore`](crate::state::TokenStore)
    /// ([`minter`](crate::state::TokenStore::minter) /
    /// [`validator`](crate::state::TokenStore::validator)); the default
    /// minter issues empty tokens that can never validate. The shared
    /// `overview` cell joins the hub's overview intent so shell
    /// `ToggleOverview` commands flip state the hub broadcasts; the
    /// shared `locked` cell joins the hub's session-lock flag so every
    /// (re)connect snapshot carries the current lock state (a shell
    /// restart while locked stays locked).
    pub fn handshake_with(
        conn: ControlConn,
        model: &StateModel,
        validator: impl Fn(&ActivationToken, Option<&str>) -> bool + 'a,
        minter: TokenMinter,
        overview: std::rc::Rc<std::cell::Cell<bool>>,
        locked: std::rc::Rc<std::cell::Cell<bool>>,
    ) -> Result<Self, ControlError> {
        let locked_sent = locked.get();
        let mut session = Self {
            connection_id: 0,
            auth_owner: None,
            auth_lease: None,
            conn,
            validator: Box::new(validator),
            minter,
            last_revision: 0,
            overview,
            locked,
            locked_sent,
            peer_minor: 0,
            closed: Vec::new(),
            idle_timeout: None,
            accelerators: None,
            window_actions: Vec::new(),
            shortcut_consent: Vec::new(),
            display_change: Vec::new(),
            overview_search: None,
            overview_app_grid: None,
            unlock_request: None,
            unlock_pending: None,
            fingerprint_pending: None,
            auth_begin: Vec::new(),
            auth_answers: Vec::new(),
            auth_cancel: false,
            input_settings: None,
            wellbeing_effects: None,
            icon_geometries: None,
            screen_reader: None,
            screenshot_pointer: None,
            dwell_click: None,
            osk_inputs: Vec::new(),
            input_source_switches: Vec::new(),
            switcher_thumbnails: None,
            switcher_keys: None,
            lockdown: LockdownPolicy::default(),
            pad_targets: std::rc::Rc::default(),
            pad_osd_requests: Vec::new(),
            pad_osd_closed: Vec::new(),
        };
        let msg = match session.conn.read_frame() {
            Ok(msg) => msg,
            Err(ControlError::Decode(e)) => {
                let (kind, text) = decode_error_to_wire(&e);
                let _ = session.conn.write_frame(&Message::Error {
                    kind,
                    message: text,
                });
                return Err(ControlError::Decode(e));
            }
            Err(e) => return Err(e),
        };
        let Message::Hello { version } = msg else {
            let _ = session.conn.write_frame(&Message::Error {
                kind: ErrorKind::UnknownCommand,
                message: "expected Hello as first message".to_owned(),
            });
            return Err(ControlError::Unexpected(
                "first message is not Hello".to_owned(),
            ));
        };
        if version.major != CURRENT_VERSION.major {
            let _ = session.conn.write_frame(&Message::Error {
                kind: ErrorKind::IncompatibleVersion,
                message: format!(
                    "incompatible peer major {}.{} (ours {}.{})",
                    version.major, version.minor, CURRENT_VERSION.major, CURRENT_VERSION.minor
                ),
            });
            return Err(ControlError::Decode(DecodeError::IncompatibleVersion {
                got: version,
                current: CURRENT_VERSION,
            }));
        }
        session.peer_minor = version.minor;
        // Same major line: accept any minor (lenient reader on our side).
        session.conn.write_frame(&Message::Hello {
            version: CURRENT_VERSION,
        })?;
        session.send_snapshot(model)?;
        Ok(session)
    }

    /// Last revision the shell is known to hold (sent snapshot or deltas).
    pub fn last_revision(&self) -> u64 {
        self.last_revision
    }

    /// Send a full snapshot of `model` (on-request path; the handshake and
    /// gap paths call this internally). Every window carries a freshly
    /// minted activation token from this session's minter. Carries the
    /// session-lock flag (stripping window content while locked) and
    /// records it as sent for the lock-transition resnapshot rule.
    pub fn send_snapshot(&mut self, model: &StateModel) -> Result<u64, ControlError> {
        let revision = model.revision();
        let locked = self.locked.get();
        self.conn
            .write_frame(&snapshot_message(model, &*self.minter, locked))?;
        self.last_revision = revision;
        self.locked_sent = locked;
        Ok(revision)
    }

    /// Send the current overview intent (002 R1). Best-effort like the
    /// rest of the nonblocking plane: a `WouldBlock` caller keeps the
    /// hub dirty flag set and retries next round.
    pub fn send_overview(&mut self, open: bool) -> Result<(), ControlError> {
        self.conn.write_frame(&Message::Overview { open })
    }

    /// Send the current output inventory (multi-monitor). Best-effort
    /// like overview: the hub rebroadcasts until every live session
    /// holds it, and newcomers get it right after the handshake.
    pub fn send_outputs(&mut self, outputs: &[OutputInfo]) -> Result<(), ControlError> {
        self.conn.write_frame(&Message::Outputs {
            outputs: outputs.to_vec(),
        })
    }

    /// Send the native DRM connector authority inventory. Peers older
    /// than 0.37 cannot decode it and receive nothing; an empty list
    /// explicitly revokes authority sent earlier.
    pub fn send_native_outputs(
        &mut self,
        outputs: &[tuna_shell_control::NativeOutputInfo],
    ) -> Result<(), ControlError> {
        if self.peer_minor < 37 {
            return Ok(());
        }
        self.conn.write_frame(&Message::NativeOutputInventory {
            outputs: outputs.to_vec(),
        })
    }

    /// The latest `SetIdleTimeout` the shell sent, once (#63).
    pub fn take_idle_timeout(&mut self) -> Option<u64> {
        self.idle_timeout.take()
    }

    /// Replace this session's lockdown policy (#357). The hub pushes
    /// its live policy here on accept and refresh; tests inject a
    /// restrictive policy to prove `Lock` denies under it.
    pub fn set_lockdown_policy(&mut self, policy: LockdownPolicy) {
        self.lockdown = policy;
    }

    /// This session's lockdown policy.
    pub fn lockdown_policy(&self) -> LockdownPolicy {
        self.lockdown
    }

    /// Whether this connection is still owed the result of `request`.
    fn owes_auth(&self, request: u64) -> bool {
        self.unlock_pending == Some(request) || self.fingerprint_pending == Some(request)
    }

    /// Window-menu actions the shell sent since the last call.
    pub fn take_window_actions(&mut self) -> Vec<(u64, tuna_shell_control::WindowAction)> {
        std::mem::take(&mut self.window_actions)
    }

    /// Ask the shell to draw GNOME's window menu.
    /// Peers older than 0.42 cannot decode `ConfirmDisplayChange`, nor
    /// older than 0.45 the tablet OSD messages, nor older than 0.46
    /// `WorkspaceProgress`, and receive nothing.
    pub fn send_window_menu(&mut self, menu: &Message) -> Result<(), ControlError> {
        if matches!(menu, Message::ConfirmDisplayChange { .. }) && self.peer_minor < 42 {
            return Ok(());
        }
        if matches!(menu, Message::WorkspaceProgress { .. }) && self.peer_minor < 46 {
            return Ok(());
        }
        let tablet = matches!(
            menu,
            Message::ShowOsd { .. }
                | Message::ShowPadOsd(_)
                | Message::PadOsdLabels { .. }
                | Message::PadOsdInput { .. }
                | Message::ClosePadOsd { .. }
        );
        if tablet && self.peer_minor < 45 {
            return Ok(());
        }
        self.conn.write_frame(menu)
    }

    /// The latest accelerator grabs the shell sent (`SetAcceleratorGrabs`,
    /// or legacy `SetAccelerators` without flags), once.
    pub fn take_accelerators(&mut self) -> Option<Vec<tuna_shell_control::AcceleratorGrab>> {
        self.accelerators.take()
    }

    /// Report a grabbed accelerator's press.
    pub fn send_accelerator(
        &mut self,
        action: u32,
        time: u32,
        mode: u32,
    ) -> Result<(), ControlError> {
        self.conn
            .write_frame(&Message::AcceleratorActivated { action, time, mode })
    }

    /// Report a `GRAB_TRIGGER_RELEASE` grab's release. Peers older than
    /// 0.38 cannot decode it and receive nothing.
    pub fn send_accelerator_deactivated(
        &mut self,
        action: u32,
        time: u32,
        mode: u32,
    ) -> Result<(), ControlError> {
        if self.peer_minor < 38 {
            return Ok(());
        }
        self.conn
            .write_frame(&Message::AcceleratorDeactivated { action, time, mode })
    }

    /// Send what GNOME's on-screen keyboard follows (#349). Peers older
    /// than 0.43 cannot decode it and receive nothing.
    pub fn send_on_screen_keyboard(&mut self, osk: OskBroadcast) -> Result<(), ControlError> {
        if self.peer_minor < 43 {
            return Ok(());
        }
        self.conn.write_frame(&Message::OnScreenKeyboard {
            focus: osk.focus,
            show: osk.show,
            touch_auto: osk.touch_auto,
        })
    }

    /// Send where the overview's window previews sit.
    pub fn send_overview_previews(
        &mut self,
        previews: &[tuna_shell_control::PreviewInfo],
        hovered: Option<u64>,
    ) -> Result<(), ControlError> {
        self.conn.write_frame(&Message::OverviewPreviews {
            previews: previews.to_vec(),
            hovered,
        })
    }

    /// Send the session environment (#59).
    pub fn send_environment(&mut self, vars: &[(String, String)]) -> Result<(), ControlError> {
        self.conn.write_frame(&Message::Environment {
            vars: vars.to_vec(),
        })
    }

    /// Send one Alt-Tab switcher drive event (002 workspaces).
    pub fn send_switcher(&mut self, action: SwitcherAction) -> Result<(), ControlError> {
        self.conn.write_frame(&Message::Switcher { action })
    }

    /// Catch the shell up from [`last_revision`](Self::last_revision):
    /// incremental `Changes` when the change log covers the gap, else a
    /// fresh snapshot (resnapshot rule). New token-bearing entries mint
    /// through this session's minter. A lock transition since the last
    /// send forces a fresh snapshot (locking strips window content,
    /// unlocking restores it); while locked with no transition, deltas
    /// are suppressed (never leak titles) and only the cursor advances.
    pub fn emit_deltas(&mut self, model: &StateModel) -> Result<Emitted, ControlError> {
        if self.locked.get() != self.locked_sent {
            let revision = self.send_snapshot(model)?;
            return Ok(Emitted::Snapshot { revision });
        }
        if self.locked.get() {
            let to = model.revision();
            self.last_revision = to;
            return Ok(Emitted::Idle { revision: to });
        }
        match model.changes_since(self.last_revision) {
            Ok(entries) => {
                if entries
                    .iter()
                    .any(|(_, change)| matches!(change, StateChange::WorkspacesChanged))
                {
                    let revision = self.send_snapshot(model)?;
                    return Ok(Emitted::Snapshot { revision });
                }
                let ops: Vec<StateOp> = entries
                    .iter()
                    .filter_map(|(_, c)| state_op(c, &*self.minter))
                    .collect();
                let to = model.revision();
                let from = self.last_revision;
                if ops.is_empty() {
                    self.last_revision = to;
                    return Ok(Emitted::Idle { revision: to });
                }
                let count = ops.len();
                self.conn.write_frame(&Message::Changes {
                    from_revision: from,
                    to_revision: to,
                    ops,
                })?;
                self.last_revision = to;
                Ok(Emitted::Changes {
                    from,
                    to,
                    ops: count,
                })
            }
            Err(_) => {
                let revision = self.send_snapshot(model)?;
                Ok(Emitted::Snapshot { revision })
            }
        }
    }

    /// Read one inbound frame and act on it:
    /// - `Command` → apply against `model` (activation commands go through
    ///   the token hook) and reply `CommandResult` with the same id. Command
    ///   kinds outside the schema enum cannot decode; they arrive here as
    ///   `MalformedFrame` errors, and message kinds the shell must never
    ///   send (`Snapshot`, `Changes`, `CommandResult`, `Error`) get a typed
    ///   `UnknownCommand` error.
    /// - `Hello` → reconnect on a live connection: resend our `Hello` plus
    ///   a fresh snapshot.
    /// - Decode failures → typed `Error` for the offending frame; oversize
    ///   and stale-version failures additionally close the session (an
    ///   `Err` is returned after the `Error` is sent).
    pub fn handle_next(&mut self, model: &mut StateModel) -> Result<Handled, ControlError> {
        let msg = match self.conn.read_frame() {
            Ok(msg) => msg,
            Err(ControlError::Decode(e)) => {
                let (kind, text) = decode_error_to_wire(&e);
                let drop_conn = matches!(
                    e,
                    DecodeError::Oversize { .. } | DecodeError::IncompatibleVersion { .. }
                );
                let _ = self.conn.write_frame(&Message::Error {
                    kind,
                    message: text,
                });
                if drop_conn {
                    return Err(ControlError::Decode(e));
                }
                return Ok(Handled::ErrorSent { kind });
            }
            Err(e) => return Err(e),
        };
        match msg {
            Message::Hello { version } => {
                if version.major != CURRENT_VERSION.major {
                    let _ = self.conn.write_frame(&Message::Error {
                        kind: ErrorKind::IncompatibleVersion,
                        message: format!(
                            "incompatible peer major {}.{} (ours {}.{})",
                            version.major,
                            version.minor,
                            CURRENT_VERSION.major,
                            CURRENT_VERSION.minor
                        ),
                    });
                    return Err(ControlError::Decode(DecodeError::IncompatibleVersion {
                        got: version,
                        current: CURRENT_VERSION,
                    }));
                }
                self.conn.write_frame(&Message::Hello {
                    version: CURRENT_VERSION,
                })?;
                let revision = self.send_snapshot(model)?;
                Ok(Handled::HelloResync { revision })
            }
            Message::Command {
                id,
                kind: CommandKind::MoveScreenshotPointer { x, y },
            } => {
                let valid = x >= 0 && y >= 0;
                if valid {
                    self.screenshot_pointer = Some((x, y));
                }
                self.conn.write_frame(&Message::CommandResult {
                    id,
                    status: if valid {
                        CommandStatus::Applied
                    } else {
                        CommandStatus::Denied {
                            reason: "invalid screenshot pointer coordinates".into(),
                        }
                    },
                })?;
                Ok(Handled::CommandResult { id, applied: valid })
            }
            Message::Command {
                id,
                kind: CommandKind::SetDwellClickType { click },
            } => {
                self.dwell_click = Some(click);
                self.conn.write_frame(&Message::CommandResult {
                    id,
                    status: CommandStatus::Applied,
                })?;
                Ok(Handled::CommandResult { id, applied: true })
            }
            Message::Command {
                id,
                kind: CommandKind::OskInput { input },
            } => {
                if self.peer_minor < 43 {
                    self.conn.write_frame(&Message::Error {
                        kind: ErrorKind::UnknownCommand,
                        message: "on-screen keyboard input requires negotiated minor 43".into(),
                    })?;
                    return Ok(Handled::ErrorSent {
                        kind: ErrorKind::UnknownCommand,
                    });
                }
                // The lock screen's keyboard edits only its own prompt:
                // nothing typed through the compositor while locked.
                let applied = !self.locked.get();
                let status = if applied {
                    self.osk_inputs.push(input);
                    CommandStatus::Applied
                } else {
                    CommandStatus::Denied {
                        reason: "session locked".into(),
                    }
                };
                self.conn
                    .write_frame(&Message::CommandResult { id, status })?;
                Ok(Handled::CommandResult { id, applied })
            }
            Message::Command {
                id,
                kind: CommandKind::SetScreenReader { enabled },
            } => {
                self.screen_reader = Some(enabled);
                self.conn.write_frame(&Message::CommandResult {
                    id,
                    status: CommandStatus::Applied,
                })?;
                Ok(Handled::CommandResult { id, applied: true })
            }
            Message::Command {
                id,
                kind: CommandKind::SetWellbeingEffects { grayscale, fade },
            } => {
                self.wellbeing_effects = Some((grayscale, fade));
                self.conn.write_frame(&Message::CommandResult {
                    id,
                    status: CommandStatus::Applied,
                })?;
                Ok(Handled::CommandResult { id, applied: true })
            }
            Message::Command {
                id,
                kind: CommandKind::SetInputSettings(settings),
            } => {
                // Session-level settings for the runtime (seat, libinput).
                self.input_settings = Some(*settings);
                self.conn.write_frame(&Message::CommandResult {
                    id,
                    status: CommandStatus::Applied,
                })?;
                Ok(Handled::CommandResult { id, applied: true })
            }
            Message::Command {
                id,
                kind: CommandKind::SetWindowIconGeometries { geometries },
            } => {
                self.icon_geometries = Some(geometries);
                self.conn.write_frame(&Message::CommandResult {
                    id,
                    status: CommandStatus::Applied,
                })?;
                Ok(Handled::CommandResult { id, applied: true })
            }
            Message::Command {
                id,
                kind: CommandKind::SetSwitcherThumbnails { thumbnails },
            } => {
                self.switcher_thumbnails = Some(thumbnails);
                self.conn.write_frame(&Message::CommandResult {
                    id,
                    status: CommandStatus::Applied,
                })?;
                Ok(Handled::CommandResult { id, applied: true })
            }
            Message::Command {
                id,
                kind: CommandKind::SetSwitcherKeys { keys },
            } => {
                self.switcher_keys = Some(keys);
                self.conn.write_frame(&Message::CommandResult {
                    id,
                    status: CommandStatus::Applied,
                })?;
                Ok(Handled::CommandResult { id, applied: true })
            }
            Message::Command {
                id,
                kind: CommandKind::SwitchInputSource { backward },
            } => {
                self.input_source_switches.push(backward);
                self.conn.write_frame(&Message::CommandResult {
                    id,
                    status: CommandStatus::Applied,
                })?;
                Ok(Handled::CommandResult { id, applied: true })
            }
            Message::Command {
                id,
                kind: CommandKind::Unlock { password },
            } => {
                // Verified off the event loop by the runtime; the result
                // goes back as this id's CommandResult.
                if self.unlock_pending.is_some() {
                    self.conn.write_frame(&Message::CommandResult {
                        id,
                        status: CommandStatus::Denied {
                            reason: "authentication failed".into(),
                        },
                    })?;
                    return Ok(Handled::CommandResult { id, applied: false });
                }
                self.unlock_request = Some((id, password));
                self.unlock_pending = Some(id);
                Ok(Handled::CommandResult { id, applied: true })
            }
            Message::Command {
                id,
                kind: CommandKind::BeginVerification { service },
            } => {
                // Like `Unlock`, the runtime answers when the conversation
                // ends. One owed result per service; the password service
                // shares the legacy unlock slot, so the two exclude each other.
                let pending = match service {
                    tuna_shell_control::AuthService::Password => &mut self.unlock_pending,
                    tuna_shell_control::AuthService::Fingerprint => &mut self.fingerprint_pending,
                };
                if pending.is_some() || self.peer_minor < 44 {
                    self.conn.write_frame(&Message::CommandResult {
                        id,
                        status: CommandStatus::Denied {
                            reason: "authentication failed".into(),
                        },
                    })?;
                    return Ok(Handled::CommandResult { id, applied: false });
                }
                *pending = Some(id);
                self.auth_begin.push((id, service));
                Ok(Handled::CommandResult { id, applied: true })
            }
            Message::Command {
                id,
                kind:
                    CommandKind::AnswerQuery {
                        service,
                        sequence,
                        answer,
                    },
            } => {
                // Queued for the runtime, which applies it only to the exact
                // outstanding query; a stale or unmatched answer is dropped.
                // Bounded: one answer per outstanding query is all it needs.
                let applied = self.auth_answers.len() < 4;
                if applied {
                    self.auth_answers.push((service, sequence, answer));
                }
                self.conn.write_frame(&Message::CommandResult {
                    id,
                    status: if applied {
                        CommandStatus::Applied
                    } else {
                        CommandStatus::Denied {
                            reason: "authentication failed".into(),
                        }
                    },
                })?;
                Ok(Handled::CommandResult { id, applied })
            }
            Message::Command {
                id,
                kind: CommandKind::CancelVerification,
            } => {
                self.auth_cancel = true;
                self.conn.write_frame(&Message::CommandResult {
                    id,
                    status: CommandStatus::Applied,
                })?;
                Ok(Handled::CommandResult { id, applied: true })
            }
            Message::Command {
                id,
                kind: CommandKind::SetOverviewAppGrid { active },
            } => {
                // UI state for the runtime's overview drawing.
                self.overview_app_grid = Some(active);
                self.conn.write_frame(&Message::CommandResult {
                    id,
                    status: CommandStatus::Applied,
                })?;
                Ok(Handled::CommandResult { id, applied: true })
            }
            Message::Command {
                id,
                kind: CommandKind::SetOverviewSearch { active },
            } => {
                // UI state for the runtime's overview drawing.
                self.overview_search = Some(active);
                self.conn.write_frame(&Message::CommandResult {
                    id,
                    status: CommandStatus::Applied,
                })?;
                Ok(Handled::CommandResult { id, applied: true })
            }
            Message::Command {
                id,
                kind: CommandKind::ShortcutConsent { request, allow },
            } => {
                if !self.locked.get() {
                    self.shortcut_consent.push((request, allow));
                }
                self.conn.write_frame(&Message::CommandResult {
                    id,
                    status: CommandStatus::Applied,
                })?;
                Ok(Handled::CommandResult { id, applied: true })
            }
            Message::Command {
                id,
                kind: CommandKind::CompleteDisplayChange { keep },
            } => {
                if !self.locked.get() {
                    self.display_change.push(keep);
                }
                self.conn.write_frame(&Message::CommandResult {
                    id,
                    status: CommandStatus::Applied,
                })?;
                Ok(Handled::CommandResult { id, applied: true })
            }
            Message::Command {
                id,
                kind:
                    CommandKind::RequestPadOsd {
                        target,
                        edition_mode,
                    },
            } => {
                // padOsd.js `ShowAsync`: an unknown pad is "Invalid params";
                // never over the lock screen.
                let pad = self.pad_targets.iter().find(|(pad, node)| match &target {
                    tuna_shell_control::PadOsdTarget::Pad(id) => id == pad,
                    tuna_shell_control::PadOsdTarget::DeviceNode(wanted) => wanted == node,
                });
                let status = match pad {
                    Some((pad, _)) if !self.locked.get() => {
                        self.pad_osd_requests.push((*pad, edition_mode));
                        CommandStatus::Applied
                    }
                    Some(_) => CommandStatus::Denied {
                        reason: "locked".into(),
                    },
                    None => CommandStatus::Denied {
                        reason: "Invalid params".into(),
                    },
                };
                let applied = status == CommandStatus::Applied;
                self.conn
                    .write_frame(&Message::CommandResult { id, status })?;
                Ok(Handled::CommandResult { id, applied })
            }
            Message::Command {
                id,
                kind: CommandKind::PadOsdClosed { pad },
            } => {
                self.pad_osd_closed.push(pad);
                self.conn.write_frame(&Message::CommandResult {
                    id,
                    status: CommandStatus::Applied,
                })?;
                Ok(Handled::CommandResult { id, applied: true })
            }
            Message::Command {
                id,
                kind: CommandKind::WindowAction { window, action },
            } => {
                // The window manager carries it out (drained by the hub);
                // unknown windows are denied here, like CloseWindow.
                let known = model.windows().any(|w| w.id == window);
                let status = if known {
                    self.window_actions.push((window, action));
                    CommandStatus::Applied
                } else {
                    CommandStatus::Denied {
                        reason: format!("unknown window {window}"),
                    }
                };
                let applied = matches!(status, CommandStatus::Applied);
                self.conn
                    .write_frame(&Message::CommandResult { id, status })?;
                Ok(Handled::CommandResult { id, applied })
            }
            Message::Command {
                id,
                kind: CommandKind::SetAccelerators { accelerators },
            } => {
                // Session-level grabs, applied by the window manager's
                // key filter (drained by the hub).
                // Legacy grabs carry no GNOME flags.
                self.accelerators = Some(
                    accelerators
                        .into_iter()
                        .map(|accelerator| tuna_shell_control::AcceleratorGrab {
                            accelerator,
                            flags: 0,
                        })
                        .collect(),
                );
                self.conn.write_frame(&Message::CommandResult {
                    id,
                    status: CommandStatus::Applied,
                })?;
                Ok(Handled::CommandResult { id, applied: true })
            }
            Message::Command {
                id,
                kind: CommandKind::SetAcceleratorGrabs { grabs },
            } => {
                if self.peer_minor < 38 {
                    self.conn.write_frame(&Message::Error {
                        kind: ErrorKind::UnknownCommand,
                        message: "accelerator grab flags require negotiated minor 38".into(),
                    })?;
                    return Ok(Handled::ErrorSent {
                        kind: ErrorKind::UnknownCommand,
                    });
                }
                // Decoding already bounded the list and its flags.
                self.accelerators = Some(grabs);
                self.conn.write_frame(&Message::CommandResult {
                    id,
                    status: CommandStatus::Applied,
                })?;
                Ok(Handled::CommandResult { id, applied: true })
            }
            Message::Command {
                id,
                kind: CommandKind::SetIdleTimeout { ms },
            } => {
                // Session-level setting, not model state: the runtime
                // applies it to the idle lock (drained by the hub).
                self.idle_timeout = Some(ms);
                self.conn.write_frame(&Message::CommandResult {
                    id,
                    status: CommandStatus::Applied,
                })?;
                Ok(Handled::CommandResult { id, applied: true })
            }
            Message::Command {
                id,
                kind: CommandKind::Lock,
            } => {
                // Administrative lockdown (#357): a disabled lock
                // screen denies the command instead of engaging the
                // lock. Denying never weakens verification and never
                // unlocks; it only refuses to lock.
                if !self.lockdown.lock_screen_allowed() {
                    let status = CommandStatus::Denied {
                        reason: RestrictedAction::LockScreen.denial().to_owned(),
                    };
                    self.conn
                        .write_frame(&Message::CommandResult { id, status })?;
                    return Ok(Handled::CommandResult { id, applied: false });
                }
                let (status, closed) = apply_command(
                    model,
                    &self.validator,
                    &self.overview,
                    &self.locked,
                    &CommandKind::Lock,
                );
                if let Some(window) = closed {
                    self.closed.push(window);
                }
                let applied = matches!(status, CommandStatus::Applied);
                self.conn
                    .write_frame(&Message::CommandResult { id, status })?;
                Ok(Handled::CommandResult { id, applied })
            }
            Message::Command { id, kind } => {
                let (status, closed) =
                    apply_command(model, &self.validator, &self.overview, &self.locked, &kind);
                if let Some(window) = closed {
                    self.closed.push(window);
                }
                let applied = matches!(status, CommandStatus::Applied);
                self.conn
                    .write_frame(&Message::CommandResult { id, status })?;
                Ok(Handled::CommandResult { id, applied })
            }
            Message::Snapshot { .. }
            | Message::Changes { .. }
            | Message::CommandResult { .. }
            | Message::Overview { .. }
            | Message::Switcher { .. }
            | Message::NativeOutputInventory { .. }
            | Message::Outputs { .. }
            | Message::Environment { .. }
            | Message::OverviewPreviews { .. }
            | Message::AcceleratorActivated { .. }
            | Message::AcceleratorDeactivated { .. }
            | Message::WindowMenu { .. }
            | Message::WorkspacePopup { .. }
            | Message::WorkspaceProgress { .. }
            | Message::PointerOutput { .. }
            | Message::ScreenReader { .. }
            | Message::KeyboardAidToggled { .. }
            | Message::DwellClickType { .. }
            | Message::OnScreenKeyboard { .. }
            | Message::PointerTimeout { .. }
            | Message::ShortcutConsent { .. }
            | Message::ConfirmDisplayChange { .. }
            | Message::AuthEvent { .. }
            | Message::ShowOsd { .. }
            | Message::ShowPadOsd(_)
            | Message::PadOsdLabels { .. }
            | Message::PadOsdInput { .. }
            | Message::ClosePadOsd { .. }
            | Message::Error { .. } => {
                let _ = self.conn.write_frame(&Message::Error {
                    kind: ErrorKind::UnknownCommand,
                    message: format!("unexpected {} from shell", message_kind(&msg)),
                });
                Ok(Handled::ErrorSent {
                    kind: ErrorKind::UnknownCommand,
                })
            }
        }
    }
}

/// Map a frame-decoding failure to the typed error sent for it.
fn decode_error_to_wire(e: &DecodeError) -> (ErrorKind, String) {
    match e {
        DecodeError::Oversize { .. } => (ErrorKind::OversizeFrame, e.to_string()),
        DecodeError::IncompatibleVersion { .. } => (ErrorKind::IncompatibleVersion, e.to_string()),
        DecodeError::Truncated { .. }
        | DecodeError::Malformed(_)
        | DecodeError::CollectionTooLong { .. }
        | DecodeError::TitleTooLong { .. } => (ErrorKind::MalformedFrame, e.to_string()),
    }
}

/// Short kind name for diagnostics (never sensitive content).
fn message_kind(msg: &Message) -> &'static str {
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
        Message::Outputs { .. } => "Outputs",
        Message::Environment { .. } => "Environment",
        Message::OverviewPreviews { .. } => "OverviewPreviews",
        Message::AcceleratorActivated { .. } => "AcceleratorActivated",
        Message::AcceleratorDeactivated { .. } => "AcceleratorDeactivated",
        Message::WindowMenu { .. } => "WindowMenu",
        Message::WorkspacePopup { .. } => "WorkspacePopup",
        Message::WorkspaceProgress { .. } => "WorkspaceProgress",
        Message::PointerOutput { .. } => "PointerOutput",
        Message::ShortcutConsent { .. } => "ShortcutConsent",
        Message::ConfirmDisplayChange { .. } => "ConfirmDisplayChange",
        Message::AuthEvent { .. } => "AuthEvent",
        Message::ScreenReader { .. } => "ScreenReader",
        Message::KeyboardAidToggled { .. } => "KeyboardAidToggled",
        Message::DwellClickType { .. } => "DwellClickType",
        Message::PointerTimeout { .. } => "PointerTimeout",
        Message::OnScreenKeyboard { .. } => "OnScreenKeyboard",
        Message::ShowOsd { .. } => "ShowOsd",
        Message::ShowPadOsd(_) => "ShowPadOsd",
        Message::PadOsdLabels { .. } => "PadOsdLabels",
        Message::PadOsdInput { .. } => "PadOsdInput",
        Message::ClosePadOsd { .. } => "ClosePadOsd",
    }
}

/// Build the full-snapshot message for `model`, minting one activation
/// token per window through `mint`. While `locked` the snapshot carries
/// the flag with no window content (empty list): the lock screen shows
/// no titles, and the restartable shell must not retain any.
/// Workspaces are structural ids only and pass through unchanged.
fn snapshot_message(
    model: &StateModel,
    mint: &dyn Fn(Option<&str>) -> String,
    locked: bool,
) -> Message {
    let snap = model.snapshot();
    Message::Snapshot {
        revision: snap.revision,
        windows: if locked {
            Vec::new()
        } else {
            snap.windows
                .iter()
                .map(|w| window_to_wire(w, mint))
                .collect()
        },
        workspaces: snap
            .workspaces
            .iter()
            .map(|id| WorkspaceInfo {
                id: *id as u64,
                name: None,
                active: *id == snap.active,
            })
            .collect(),
        locked,
    }
}

/// Map one model window to its wire mirror, minting its activation token
/// through `mint` (bound to the window's `app_id`).
fn window_to_wire(
    w: &WindowEntry,
    mint: &dyn Fn(Option<&str>) -> String,
) -> tuna_shell_control::WindowInfo {
    tuna_shell_control::WindowInfo {
        on_all_workspaces: w.on_all_workspaces,
        icon: w.icon.clone(),
        id: w.id,
        title: w.title.clone(),
        app_id: w.app_id.clone(),
        workspace: w.workspace as u64,
        focused: w.focused,
        activation_token: mint(w.app_id.as_deref()),
    }
}

/// Map one model change to its wire op, minting tokens for restated
/// windows through `mint`. Clearing focus has no wire counterpart (focus
/// is carried on the window entries and the `WindowFocused` op), so it
/// maps to `None` and only advances the cursor.
fn state_op(change: &StateChange, mint: &dyn Fn(Option<&str>) -> String) -> Option<StateOp> {
    match change {
        StateChange::WindowInserted { window } => {
            Some(StateOp::WindowOpened(window_to_wire(window, mint)))
        }
        StateChange::WindowRemoved { id } => Some(StateOp::WindowClosed { id: *id }),
        StateChange::WindowUpdated { window } => {
            Some(StateOp::WindowOpened(window_to_wire(window, mint)))
        }
        StateChange::FocusChanged { focused: Some(id) } => Some(StateOp::WindowFocused { id: *id }),
        StateChange::FocusChanged { focused: None } | StateChange::WorkspacesChanged => None,
        // Active is exclusive shell-side (applying active:true clears
        // the rest), so one op carries the switch.
        StateChange::ActiveWorkspaceChanged { active, .. } => {
            Some(StateOp::WorkspaceChanged(WorkspaceInfo {
                id: *active as u64,
                name: None,
                active: true,
            }))
        }
    }
}

/// Apply one shell command against `model`, gating `ActivateWindow` on the
/// injected token hook. Unknown ids are per-request denials
/// (`CommandResult::Denied`), not protocol errors. The validator sees the
/// target window's `app_id` from the model, so tokens bind to the window
/// they were minted for.
///
/// Returns the status plus the closed window id when a `CloseWindow`
/// applied: the hub drains those so the runtime can send the polite
/// client close (the mirror keeps the window until the client unmaps).
fn apply_command(
    model: &mut StateModel,
    validator: &dyn Fn(&ActivationToken, Option<&str>) -> bool,
    overview: &std::cell::Cell<bool>,
    locked: &std::cell::Cell<bool>,
    kind: &CommandKind,
) -> (CommandStatus, Option<u64>) {
    match kind {
        CommandKind::ActivateWindow { window, token } => {
            let Some(entry) = model.window(*window) else {
                return (
                    CommandStatus::Denied {
                        reason: "unknown window".to_owned(),
                    },
                    None,
                );
            };
            if !validator(token, entry.app_id.as_deref()) {
                return (
                    CommandStatus::Denied {
                        reason: "activation token rejected".to_owned(),
                    },
                    None,
                );
            }
            let _ = model.set_focused(Some(*window));
            // The token is spent: the next delta carries a fresh one, so
            // the shell can activate this window again (window cycling).
            model.reissue(Some(*window));
            (CommandStatus::Applied, None)
        }
        CommandKind::FocusWorkspace { workspace } => {
            // Dynamic workspaces: any representable id switches
            // (registering if new); focus lands on the tick's reconcile.
            let Ok(workspace) = u32::try_from(*workspace) else {
                return (
                    CommandStatus::Denied {
                        reason: "workspace id out of range".to_owned(),
                    },
                    None,
                );
            };
            if model.set_active_workspace(workspace) {
                (CommandStatus::Applied, None)
            } else {
                (
                    CommandStatus::Denied {
                        reason: "workspace outside fixed range".to_owned(),
                    },
                    None,
                )
            }
        }
        CommandKind::ToggleOverview => {
            // Flip the hub-shared intent; the hub broadcasts the new
            // state as `Message::Overview` (002 R1).
            overview.set(!overview.get());
            (CommandStatus::Applied, None)
        }
        CommandKind::CloseWindow { window } => {
            if model.window(*window).is_none() {
                return (
                    CommandStatus::Denied {
                        reason: "unknown window".to_owned(),
                    },
                    None,
                );
            }
            (CommandStatus::Applied, Some(*window))
        }
        // Intercepted by the session before it gets here.
        CommandKind::SetIdleTimeout { .. }
        | CommandKind::SetOverviewSearch { .. }
        | CommandKind::SetOverviewAppGrid { .. }
        | CommandKind::Unlock { .. }
        | CommandKind::SetAccelerators { .. }
        | CommandKind::SetAcceleratorGrabs { .. }
        | CommandKind::WindowAction { .. }
        | CommandKind::ShortcutConsent { .. }
        | CommandKind::CompleteDisplayChange { .. }
        | CommandKind::BeginVerification { .. }
        | CommandKind::AnswerQuery { .. }
        | CommandKind::CancelVerification
        | CommandKind::RequestPadOsd { .. }
        | CommandKind::PadOsdClosed { .. }
        | CommandKind::SwitchInputSource { .. }
        | CommandKind::SetWindowIconGeometries { .. }
        | CommandKind::SetWellbeingEffects { .. }
        | CommandKind::SetSwitcherThumbnails { .. }
        | CommandKind::SetSwitcherKeys { .. }
        | CommandKind::MoveScreenshotPointer { .. }
        | CommandKind::SetScreenReader { .. }
        | CommandKind::OskInput { .. }
        | CommandKind::SetDwellClickType { .. }
        | CommandKind::SetInputSettings(_) => (CommandStatus::Applied, None),
        CommandKind::Lock => {
            // Manual lock from the shell (session-lock set path):
            // engage the compositor-owned flag; idempotent, always
            // applied. The next poll snapshots the stripped state.
            locked.set(true);
            (CommandStatus::Applied, None)
        }
    }
}

/// Our protocol version, for handshake replies.
pub fn our_version() -> ProtocolVersion {
    CURRENT_VERSION
}

/// Outcome of one [`ControlHub::poll`] round: model ids the shell
/// activated (runtime applies Wayland-side focus) and asked to close
/// (runtime sends the polite client close).
#[derive(Debug, Default)]
pub struct PollOutcome {
    /// Windows to focus and raise.
    pub activated: Vec<u64>,
    /// Windows to ask to close.
    pub closed: Vec<u64>,
    /// Workspaces left by shell focus commands; the window manager keeps
    /// these alive through the next dynamic-workspace reconciliation.
    pub departed_workspaces: Vec<u32>,
    /// Idle-lock timeout the shell asked for (`0` never), if any.
    pub idle_timeout: Option<u64>,
    /// Whether overview search is showing results, if the shell said.
    pub overview_search: Option<bool>,
    /// Whether the overview shows the app grid, if the shell said.
    pub overview_app_grid: Option<bool>,
    /// A lock-screen password to verify, with its request id.
    pub unlock: Option<(crate::auth_owner::Origin, tuna_shell_control::Secret)>,
    /// Authentication conversations the shell started: the origin is the
    /// `BeginVerification` request whose result is owed.
    pub auth_begin: Vec<(crate::auth_owner::Origin, tuna_shell_control::AuthService)>,
    /// Query answers, by the connection that sent them.
    pub auth_answers: Vec<(
        u64,
        tuna_shell_control::AuthService,
        u32,
        tuna_shell_control::Secret,
    )>,
    /// Connections that asked to stop their conversations.
    pub auth_cancel: Vec<u64>,
    /// GNOME input settings the shell sent, if any.
    pub input_settings: Option<tuna_shell_control::InputSettings>,
    pub wellbeing_effects: Option<(bool, bool)>,
    pub icon_geometries: Option<Vec<tuna_shell_control::WindowIconGeometry>>,
    pub screen_reader: Option<bool>,
    pub screenshot_pointer: Option<(i32, i32)>,
    /// The hover-click chooser's pick, if the shell sent one.
    pub dwell_click: Option<tuna_shell_control::DwellClick>,
    /// Accelerator grabs the shell sent, if they changed.
    pub accelerators: Option<Vec<tuna_shell_control::AcceleratorGrab>>,
    /// Window-menu actions to carry out.
    pub window_actions: Vec<(u64, tuna_shell_control::WindowAction)>,
    /// Trusted shell decisions for pending inhibitors.
    pub shortcut_consent: Vec<(u64, bool)>,
    /// Display-change dialog answers (`true` = Keep Changes), in order.
    pub display_change: Vec<bool>,
    /// Admitted pad OSD requests (pad id, edition mode), in order.
    pub pad_osd_requests: Vec<(u32, bool)>,
    /// Pad OSDs the shell closed, in order.
    pub pad_osd_closed: Vec<u32>,
    /// Input-source switches to make (`true` backward), in order.
    pub input_source_switches: Vec<bool>,
    /// New switcher thumbnail frames, if the shell sent any.
    pub switcher_thumbnails: Option<Vec<tuna_shell_control::SwitcherThumbnail>>,
    /// The shell's switcher keys, if it sent new ones.
    pub switcher_keys: Option<Vec<tuna_shell_control::SwitcherKey>>,
    /// On-screen keyboard input to type, in order (#349).
    pub osk_inputs: Vec<tuna_shell_control::OskInput>,
}

/// What GNOME's on-screen keyboard follows, as broadcast to the shell
/// (`Message::OnScreenKeyboard`, #349).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct OskBroadcast {
    /// The field holding input-method focus (none while locked).
    pub focus: Option<tuna_shell_control::TextFieldFocus>,
    /// An input-panel ON request not yet delivered.
    pub show: bool,
    /// Touch-mode auto-show.
    pub touch_auto: bool,
}

/// Peers accepted but not yet handshaken are capped so a same-user
/// connection flood cannot grow memory or starve the real shell (#30).
pub const MAX_PENDING_PEERS: usize = 8;

/// Kernel-reported credentials of a connected control peer
/// (`SO_PEERCRED`): what the peer *is*, not what it claims.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PeerCred {
    /// Peer process id at connect time.
    pub pid: u32,
    /// Peer effective user id.
    pub uid: u32,
}

/// Who may hold a control session (#30).
///
/// The control socket is a privileged channel: a session can focus and
/// close windows, drive the overview, and engage the lock. Filesystem
/// permissions keep other users out; this gate also keeps other
/// same-user processes out once the compositor supervises its shell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeerGate {
    /// Any peer running as our user (tests, unsupervised development).
    SameUser,
    /// Only the supervised shell process with this pid, as our user.
    Pid(u32),
    /// Nobody: no shell is running (between restarts, or none spawned).
    Closed,
}

impl PeerGate {
    /// Whether a peer with `peer` credentials may hold a session.
    /// Unknown credentials are refused: fail closed.
    pub fn admits(self, peer: Option<PeerCred>, our_uid: u32) -> bool {
        let Some(peer) = peer else {
            return false;
        };
        if peer.uid != our_uid {
            return false;
        }
        match self {
            Self::SameUser => true,
            Self::Pid(pid) => peer.pid == pid,
            Self::Closed => false,
        }
    }
}

/// Read the kernel credentials of a connected Unix stream peer.
pub fn peer_cred(stream: &UnixStream) -> Option<PeerCred> {
    let cred = rustix::net::sockopt::socket_peercred(stream).ok()?;
    Some(PeerCred {
        pid: u32::try_from(cred.pid.as_raw_nonzero().get()).ok()?,
        uid: cred.uid.as_raw(),
    })
}

/// Our effective uid, for [`PeerGate::admits`].
pub fn our_uid() -> u32 {
    rustix::process::geteuid().as_raw()
}

/// Whether `dir` is a directory owned by us with no group or other
/// permission bits: the only place a privileged socket may live.
pub fn is_private_dir(dir: &std::path::Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(dir)
        .map(|meta| meta.is_dir() && meta.uid() == our_uid() && meta.mode() & 0o077 == 0)
        .unwrap_or(false)
}

/// Bound on concurrent background lockdown readers (#357 fd3): one slow
/// backend may overlap one fresh poll, but a hung backend never stacks a
/// thread per tick. Production ticks 2 s apart with a 500 ms reader bound,
/// so steady state holds one; the headroom keeps a slow poll from
/// suppressing its successor.
const LOCKDOWN_MAX_READERS: usize = 2;

/// Live control-plane driver: accepts shell connections, handshakes them
/// into [`Session`]s against one [`TokenStore`], emits deltas, and applies
/// commands — one nonblocking round per [`poll`](Self::poll), so slow or
/// dead shells never stall compositor input/frame paths (spec R6).
///
/// Sessions hold `'static` policy closures over an `Rc<TokenStore>`, so the
/// hub owns the store outright with no borrow of the runtime. Windows the
/// shell activated this round are returned for Wayland-side focus; the hub
/// only mutates the model.
pub struct ControlHub {
    listener: UnixListener,
    readiness: crate::control_readiness::ControlReadiness,
    socket_path: std::path::PathBuf,
    sessions: Vec<Session<'static>>,
    next_connection: Option<u64>,
    /// Peers accepted in an earlier round: their `Hello` is due now.
    pending: Vec<UnixStream>,
    /// Peers accepted this round: handshake next round, once their
    /// `Hello` has had a full tick to arrive.
    fresh: Vec<UnixStream>,
    store: std::rc::Rc<TokenStore>,
    seat: String,
    /// Shell-facing overview intent (002 R1), shared with every live
    /// session: shell `ToggleOverview` commands and runtime triggers
    /// flip it, and [`poll`](Self::poll) broadcasts the result.
    overview: std::rc::Rc<std::cell::Cell<bool>>,
    /// Intent value last broadcast to all sessions; a mismatch means a
    /// flip is still owed (or a newcomer joined mid-state).
    overview_sent: bool,
    /// Compositor-owned session-lock flag (session-lock): flipped by
    /// the runtime (idle timeout) or `Lock` commands, shared with every
    /// live session so snapshots carry it; the shell never owns it.
    locked: std::rc::Rc<std::cell::Cell<bool>>,
    /// Administrative lockdown (#357): read live at bind, refreshed by
    /// [`ControlHub::refresh_lockdown`], pushed onto every session so
    /// `Lock` commands deny under a restrictive key. Only the lock key
    /// is live here; the shell owns the rest of the surface.
    lockdown: LockdownPolicy,
    /// Pads (id, device node) the shell's `RequestPadOsd` may name.
    pad_targets: std::rc::Rc<Vec<(u32, String)>>,
    /// Background policy reads (#357 follow-up): each refresh spawns
    /// one reader thread reporting here (up to
    /// [`LOCKDOWN_MAX_READERS`] concurrent); the tick drains
    /// completed results without waiting, so a hung backend never
    /// stalls it.
    lockdown_tx: std::sync::mpsc::Sender<Option<bool>>,
    lockdown_rx: std::sync::mpsc::Receiver<Option<bool>>,
    /// Readers currently running (#357 fd3): refreshes while this many
    /// run skip spawning (single-flight bound), so a hung backend
    /// cannot stack a thread per tick; the drain still applies every
    /// landed result with last-valid-wins.
    lockdown_inflight: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// Alt-Tab drive events awaiting broadcast. Unlike the overview
    /// intent (level), steps are discrete events: each one is sent to
    /// every live session exactly once, retained until all sends land.
    switcher_queue: Vec<SwitcherAction>,
    /// Accelerator presses waiting for the next poll.
    accelerator_queue: Vec<(u32, u32, u32, bool)>,
    /// Window-menu requests waiting for the next poll.
    menu_queue: Vec<Message>,
    /// What the on-screen keyboard follows, as last set by the runtime,
    /// and the value every live session holds (#349).
    osk: OskBroadcast,
    osk_sent: Option<OskBroadcast>,
    /// Output inventory last handed to [`set_outputs`](Self::set_outputs)
    /// (multi-monitor): the runtime refreshes this every tick from the
    /// compositor's tracking, and [`poll`](Self::poll) broadcasts it
    /// whenever it differs from `outputs_sent`.
    outputs: Vec<OutputInfo>,
    native_outputs: Vec<tuna_shell_control::NativeOutputInfo>,
    pointer_output: Option<String>,
    /// Inventory value every live session holds; a mismatch means a
    /// broadcast is still owed (or a newcomer joined mid-state).
    outputs_sent: Vec<OutputInfo>,
    native_outputs_sent: Vec<tuna_shell_control::NativeOutputInfo>,
    /// Session environment for launched apps (#59), and the last value
    /// every live session holds.
    environment: Vec<(String, String)>,
    environment_sent: Vec<(String, String)>,
    /// Overview previews for the shell's chrome, and the last value
    /// every session holds.
    previews: (Vec<tuna_shell_control::PreviewInfo>, Option<u64>),
    previews_sent: (Vec<tuna_shell_control::PreviewInfo>, Option<u64>),
    /// Peer admission rule (#30); the runtime narrows it to the
    /// supervised shell's pid every tick.
    gate: PeerGate,
    auth_owner: Option<AuthOwner>,
    auth_owner_configured: bool,
    /// Peer pid per live session, index-aligned with `sessions`, so a
    /// gate change evicts sessions the new gate no longer admits.
    session_peers: Vec<Option<PeerCred>>,
    /// Peer credentials of `pending` / `fresh`, index-aligned.
    pending_peers: Vec<Option<PeerCred>>,
    fresh_peers: Vec<Option<PeerCred>>,
    /// Connections refused by the gate or the pending cap, for
    /// diagnostics (a count only; never peer details).
    refused: u64,
    /// When every window's activation token was last reissued; see
    /// [`TOKEN_REISSUE`].
    tokens_reissued: std::time::Instant,
}

impl ControlHub {
    /// Bind `socket_path` (removing a stale file first) and start
    /// nonblocking. Fails fast when the path cannot be bound, or when
    /// its directory is not private to us (#30: never a shared dir such
    /// as `/tmp`). The socket file itself is made owner-only.
    pub fn bind(
        socket_path: std::path::PathBuf,
        store: std::rc::Rc<TokenStore>,
        seat: &str,
    ) -> std::io::Result<Self> {
        use std::os::unix::fs::PermissionsExt;
        let parent = socket_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .ok_or_else(|| std::io::Error::other("control socket path has no directory"))?;
        if !is_private_dir(parent) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "control socket directory is not private to this user",
            ));
        }
        let _ = std::fs::remove_file(&socket_path);
        let listener = UnixListener::bind(&socket_path)?;
        std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        let readiness = crate::control_readiness::ControlReadiness::new(&listener)?;
        // Administrative lockdown (#357): constructed from the live
        // key at startup (bounded read); unreadable reads GNOME defaults
        // (allowed).
        let lockdown = LockdownPolicy::from_reader(&|key| {
            if key == crate::lockdown::KEY_DISABLE_LOCK_SCREEN {
                crate::lockdown::read_lock_screen_key()
            } else {
                None
            }
        });
        let (lockdown_tx, lockdown_rx) = std::sync::mpsc::channel();
        let lockdown_inflight = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        Ok(Self {
            listener,
            readiness,
            socket_path,
            sessions: Vec::new(),
            next_connection: Some(1),
            pending: Vec::new(),
            fresh: Vec::new(),
            overview: std::rc::Rc::new(std::cell::Cell::new(false)),
            overview_sent: false,
            locked: std::rc::Rc::new(std::cell::Cell::new(false)),
            lockdown,
            pad_targets: std::rc::Rc::default(),
            lockdown_tx,
            lockdown_rx,
            lockdown_inflight,
            switcher_queue: Vec::new(),
            accelerator_queue: Vec::new(),
            menu_queue: Vec::new(),
            osk: OskBroadcast::default(),
            // No focus and no auto-show is what a shell assumes before
            // any message: only a change is owed.
            osk_sent: Some(OskBroadcast::default()),
            outputs: Vec::new(),
            native_outputs: Vec::new(),
            pointer_output: None,
            outputs_sent: Vec::new(),
            native_outputs_sent: Vec::new(),
            environment: Vec::new(),
            environment_sent: Vec::new(),
            previews: (Vec::new(), None),
            previews_sent: (Vec::new(), None),
            store,
            seat: seat.to_owned(),
            gate: PeerGate::SameUser,
            auth_owner: None,
            auth_owner_configured: false,
            session_peers: Vec::new(),
            pending_peers: Vec::new(),
            fresh_peers: Vec::new(),
            refused: 0,
            tokens_reissued: std::time::Instant::now(),
        })
    }

    /// Narrow (or widen) who may hold a session. Live sessions the new
    /// gate refuses are dropped on the next [`poll`](Self::poll).
    pub fn set_peer_gate(&mut self, gate: PeerGate) {
        self.gate = gate;
    }

    /// Auth authority follows the actual nonreused child generation. A change
    /// closes old streams, including pending handshakes; inherited/reused PIDs
    /// cannot continue an old auth lease. Standalone nonruntime hubs retain their
    /// existing peer-gate behavior until this trusted owner API is configured.
    pub fn set_auth_owner(&mut self, owner: Option<AuthOwner>) {
        let owner = owner.filter(|owner| owner.pid != 0 && owner.generation != 0);
        if self.auth_owner_configured && self.auth_owner == owner {
            return;
        }
        self.auth_owner_configured = true;
        self.auth_owner = owner;
        self.sessions.clear();
        self.session_peers.clear();
        self.pending.clear();
        self.pending_peers.clear();
        self.fresh.clear();
        self.fresh_peers.clear();
    }
    /// The trusted runtime must derive these values from its exact matched Slot.
    /// This lease routes a conversation; it cannot grant unlock or free that Slot.
    pub fn bind_auth_lease(
        &mut self,
        origin: crate::auth_owner::Origin,
        serial: u64,
        epoch: u64,
        method: crate::auth_owner::MethodKind,
    ) -> Option<AuthLease> {
        let owner = self.auth_owner?;
        if serial == 0 || epoch == u64::MAX || !self.origin_pending_live(origin) {
            return None;
        }
        let session = self
            .sessions
            .iter_mut()
            .find(|session| session.connection_id == origin.connection)?;
        if session.auth_owner != Some(owner) || session.auth_lease.is_some() {
            return None;
        }
        let lease = AuthLease {
            origin,
            serial,
            epoch,
            method,
            owner,
        };
        session.auth_lease = Some(lease);
        Some(lease)
    }
    /// Reply correlation may end before physical retirement. The routing lease
    /// survives an early denial until explicit verified release or owner loss.
    pub fn auth_lease_live(&self, lease: AuthLease) -> bool {
        self.auth_owner == Some(lease.owner)
            && self.sessions.iter().enumerate().any(|(index, session)| {
                session.connection_id == lease.origin.connection
                    && session.auth_lease == Some(lease)
                    && session.auth_owner == Some(lease.owner)
                    && PeerGate::Pid(lease.owner.pid).admits(self.session_peers[index], our_uid())
                    && self.gate.admits(self.session_peers[index], our_uid())
                    && session.conn.peer_live()
            })
    }
    pub fn auth_lease_accepts(
        &self,
        lease: AuthLease,
        origin: crate::auth_owner::Origin,
        serial: u64,
        epoch: u64,
        method: crate::auth_owner::MethodKind,
    ) -> bool {
        lease.origin == origin
            && lease.serial == serial
            && lease.epoch == epoch
            && lease.method == method
            && self.auth_lease_live(lease)
    }
    /// A PAM receipt cannot release a fingerprint lease. Missing/old leases
    /// produce no mutation; callers still own their exact method/Slot retirement.
    pub fn retire_pam_auth_lease(
        &mut self,
        lease: AuthLease,
        result: crate::pam_backend::Verification,
    ) -> bool {
        if lease.method == crate::auth_owner::MethodKind::Fingerprint || !result.may_retire() {
            return false;
        }
        let Some(session) = self.sessions.iter_mut().find(|session| {
            session.connection_id == lease.origin.connection && session.auth_lease == Some(lease)
        }) else {
            return false;
        };
        session.auth_lease = None;
        true
    }

    /// Current admission rule.
    pub fn peer_gate(&self) -> PeerGate {
        self.gate
    }

    /// Connections refused so far (gate or pending cap).
    pub fn refused_count(&self) -> u64 {
        self.refused
    }

    /// When [`poll`](Self::poll) must run again although no socket is
    /// readable: at once for a command frame already buffered (one is
    /// applied per poll), and after [`RETRY`] while a handshake is due
    /// or a broadcast is still owed after backpressure. `None` when
    /// only new socket input can change anything.
    pub fn next_poll(&self, model: &StateModel) -> Option<std::time::Duration> {
        if self.sessions.iter().any(Session::has_buffered_frame) {
            return Some(std::time::Duration::ZERO);
        }
        let owed = self.overview.get() != self.overview_sent
            || self.environment != self.environment_sent
            || self.previews != self.previews_sent
            || self.outputs != self.outputs_sent
            || !self.switcher_queue.is_empty()
            || self.osk_sent != Some(self.osk)
            || !self.accelerator_queue.is_empty()
            || !self.menu_queue.is_empty()
            || self.sessions.iter().any(|s| s.owes_deltas(model));
        (owed || self.pending_count() > 0).then_some(RETRY)
    }

    /// Accepted peers still waiting for their handshake round.
    pub fn pending_count(&self) -> usize {
        self.pending.len() + self.fresh.len()
    }

    /// Bound control socket path (hand this to the shell child).
    pub fn socket_path(&self) -> &std::path::Path {
        &self.socket_path
    }

    /// Live session count, for diagnostics (never sensitive content).
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    /// Shell-facing overview intent (002 R1).
    pub fn overview_open(&self) -> bool {
        self.overview.get()
    }

    /// Set the overview intent; the next [`poll`](Self::poll) broadcasts
    /// the flip to every live session as [`Message::Overview`].
    /// Idempotent: setting the current value sends nothing.
    pub fn set_overview(&self, open: bool) {
        self.overview.set(open);
    }

    /// Refresh the output inventory; the next [`poll`](Self::poll)
    /// broadcasts it to every live session as [`Message::Outputs`]
    /// when it differs from the last broadcast. Idempotent: setting
    /// the current value sends nothing. The runtime calls this every
    /// tick from the compositor's tracking — the single inventory the
    /// spec requires, never a parallel database.
    pub fn set_outputs(&mut self, outputs: Vec<OutputInfo>) {
        self.outputs = outputs;
    }

    /// Refresh the native DRM connector authority; the next
    /// [`poll`](Self::poll) broadcasts a change as
    /// [`Message::NativeOutputInventory`] (empty revokes it). Nested
    /// sessions always set an empty inventory.
    pub fn set_native_outputs(&mut self, outputs: Vec<tuna_shell_control::NativeOutputInfo>) {
        self.native_outputs = outputs;
    }

    /// Refresh what the on-screen keyboard follows (#349); the next
    /// [`poll`](Self::poll) broadcasts a change. `show` queues one
    /// input-panel ON request, dropped if focus is gone by then.
    pub fn set_on_screen_keyboard(
        &mut self,
        focus: Option<tuna_shell_control::TextFieldFocus>,
        show: bool,
        touch_auto: bool,
    ) {
        self.osk.focus = focus;
        self.osk.touch_auto = touch_auto;
        self.osk.show = (self.osk.show || show) && focus.is_some();
    }

    /// Set the session environment (#59); the next [`poll`](Self::poll)
    /// broadcasts it when it differs from the last broadcast, and every
    /// newcomer gets it right after the handshake.
    /// Where the overview's previews sit and which one is hovered; sent
    /// to the shell when it changes (empty while the overview is closed).
    pub fn set_overview_previews(
        &mut self,
        previews: Vec<tuna_shell_control::PreviewInfo>,
        hovered: Option<u64>,
    ) {
        self.previews = (previews, hovered);
    }

    /// Whether `origin`'s authentication result is still owed to a live,
    /// admitted, authenticated shell connection.
    pub fn origin_pending_live(&self, origin: crate::auth_owner::Origin) -> bool {
        self.sessions.iter().enumerate().any(|(i, session)| {
            session.connection_id == origin.connection
                && session.owes_auth(origin.request)
                && self.gate.admits(self.session_peers[i], our_uid())
                && (!self.auth_owner_configured
                    || self.auth_owner.is_some_and(|owner| {
                        session.auth_owner == Some(owner)
                            && PeerGate::Pid(owner.pid).admits(self.session_peers[i], our_uid())
                    }))
                && session.conn.peer_live()
        })
    }
    /// Answer a lock-screen authentication request (`Unlock` or
    /// `BeginVerification`): `Applied` when it unlocked the session,
    /// `Denied` when it did not.
    pub fn finish_unlock(&mut self, origin: crate::auth_owner::Origin, unlocked: bool) {
        let Some(index) = self
            .sessions
            .iter()
            .position(|s| s.connection_id == origin.connection && s.owes_auth(origin.request))
        else {
            return;
        };
        let session = &mut self.sessions[index];
        if session.unlock_pending == Some(origin.request) {
            session.unlock_pending = None;
        } else {
            session.fingerprint_pending = None;
        }
        let sent = session.conn.write_frame(&Message::CommandResult {
            id: origin.request,
            status: if unlocked {
                CommandStatus::Applied
            } else {
                CommandStatus::Denied {
                    reason: "authentication failed".to_owned(),
                }
            },
        });
        if sent.is_err() {
            self.sessions.swap_remove(index);
            self.session_peers.swap_remove(index);
        }
    }

    /// Relay one conversation step to the connection that owns `origin`,
    /// only while that request is still owed to a live admitted shell.
    /// A write failure drops that connection, which ends its conversation.
    pub fn send_auth_event(
        &mut self,
        origin: crate::auth_owner::Origin,
        service: tuna_shell_control::AuthService,
        event: tuna_shell_control::AuthEvent,
    ) -> bool {
        if !self.origin_pending_live(origin) {
            return false;
        }
        let Some(index) = self
            .sessions
            .iter()
            .position(|s| s.connection_id == origin.connection)
        else {
            return false;
        };
        let sent = self.sessions[index].conn.write_frame(&Message::AuthEvent {
            request: origin.request,
            service,
            event,
        });
        if sent.is_err() {
            self.sessions.swap_remove(index);
            self.session_peers.swap_remove(index);
            return false;
        }
        true
    }

    pub fn set_environment(&mut self, mut vars: Vec<(String, String)>) {
        vars.sort();
        self.environment = vars;
    }

    /// Whether the session is locked (compositor-owned flag).
    pub fn is_locked(&self) -> bool {
        self.locked.get()
    }

    /// Whether the lock screen is currently allowed by policy (#357).
    pub fn lock_screen_allowed(&self) -> bool {
        self.lockdown.lock_screen_allowed()
    }

    /// Replace the hub's lockdown policy and push it onto every live
    /// session; newcomers clone it at accept. Tests inject a
    /// restrictive policy to prove the lock paths deny under it.
    pub fn set_lockdown_policy(&mut self, policy: LockdownPolicy) {
        self.lockdown = policy;
        for session in &mut self.sessions {
            session.set_lockdown_policy(policy);
        }
    }

    /// Poll the live `disable-lock-screen` key without blocking: drain
    /// completed background reads (last valid wins; unreadable keeps the
    /// cached value, never resets) and spawn the next reader unless
    /// enough already run. The runtime calls this on a slow tick; a hung
    /// backend never stalls it, and the shell enforces its own surface
    /// immediately, so this backstop's lag fails safe.
    pub fn refresh_lockdown(&mut self) {
        self.refresh_lockdown_with(crate::lockdown::read_lock_screen_key);
    }

    /// [`refresh_lockdown`](Self::refresh_lockdown) with an injectable
    /// reader. Tests pass a hung double to prove the tick stays bounded;
    /// production reads the live key on the background thread.
    pub fn refresh_lockdown_with(
        &mut self,
        reader: impl FnOnce() -> Option<bool> + Send + 'static,
    ) {
        // Last-valid-wins: overlapping readers land delayed results as one
        // batch, and a trailing unreadable result must not wipe the live
        // restriction (or restoration) the batch's valid result carries.
        if let Some(value) = self.lockdown_rx.try_iter().flatten().last() {
            self.lockdown
                .refresh(&|key| (key == crate::lockdown::KEY_DISABLE_LOCK_SCREEN).then_some(value));
            for session in &mut self.sessions {
                session.set_lockdown_policy(self.lockdown);
            }
        }
        // Single-flight bound: skip spawning while enough readers run, so
        // a hung backend cannot stack a thread per tick. The drain above
        // still runs, so landed results always apply.
        if self
            .lockdown_inflight
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            >= LOCKDOWN_MAX_READERS
        {
            self.lockdown_inflight
                .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            return;
        }
        let tx = self.lockdown_tx.clone();
        let inflight = self.lockdown_inflight.clone();
        std::thread::spawn(move || {
            let result = reader();
            let _ = tx.send(result);
            inflight.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        });
    }

    /// The pads a `RequestPadOsd` may name: id and device node.
    pub fn set_pad_targets(&mut self, pads: Vec<(u32, String)>) {
        if *self.pad_targets != pads {
            self.pad_targets = std::rc::Rc::new(pads);
        }
    }

    /// Whether a connected shell renders the 0.45 tablet OSDs.
    pub fn pad_osd_capable(&self) -> bool {
        self.sessions.iter().any(|s| s.peer_minor >= 45)
    }

    /// Set the session-lock flag; the next [`poll`](Self::poll) carries
    /// it to every live session as a (possibly content-stripped)
    /// [`Message::Snapshot`]. Idempotent. Unlock clears through session
    /// auth (see [`crate::unlock`]), never through this path from the
    /// shell.
    pub fn set_locked(&self, locked: bool) {
        self.locked.set(locked);
    }

    /// Send a compositor-to-shell message (window menu, workspace popup)
    /// on the next poll.
    pub fn set_pointer_output(&mut self, name: Option<String>) {
        if self.pointer_output != name {
            self.pointer_output = name.clone();
            self.queue_message(Message::PointerOutput { name });
        }
    }

    pub fn queue_message(&mut self, message: Message) {
        self.menu_queue.push(message);
    }

    /// Report a grabbed accelerator's press to the shell on the next poll.
    pub fn queue_accelerator(&mut self, action: u32, time: u32, mode: u32) {
        self.accelerator_queue.push((action, time, mode, false));
    }

    /// Report a `GRAB_TRIGGER_RELEASE` grab's release on the next poll.
    pub fn queue_accelerator_deactivated(&mut self, action: u32, time: u32, mode: u32) {
        self.accelerator_queue.push((action, time, mode, true));
    }

    /// Drop queued presses and releases that the shell has not been
    /// sent yet (a lock boundary invalidates them).
    pub fn cancel_pending_accelerators(&mut self) {
        self.accelerator_queue.clear();
    }

    /// Queue one Alt-Tab drive event; the next [`poll`](Self::poll)
    /// broadcasts it to every live session as [`Message::Switcher`].
    pub fn queue_switcher(&mut self, action: SwitcherAction) {
        self.switcher_queue.push(action);
    }

    /// Drain one session's applied close requests (called by
    /// [`ControlHub::poll`]).
    fn take_closed(session: &mut Session<'_>) -> Vec<u64> {
        std::mem::take(&mut session.closed)
    }

    pub(crate) fn readiness_source(
        &self,
    ) -> std::io::Result<calloop::generic::Generic<std::os::fd::OwnedFd>> {
        self.readiness.source()
    }

    /// One nonblocking round: accept waiting peers, advance pending
    /// handshakes, catch live sessions up, and apply one command frame
    /// per session. Returns the model ids of windows the shell activated
    /// this round (for Wayland-side focus) and asked to close (for the
    /// polite client close), so the runtime can apply both.
    pub fn poll(&mut self, model: &mut StateModel) -> PollOutcome {
        let locked_before = self.locked.get();
        let uid = our_uid();
        while let Ok((stream, _)) = self.listener.accept() {
            let cred = peer_cred(&stream);
            if !self.gate.admits(cred, uid) || self.pending_count() >= MAX_PENDING_PEERS {
                // Dropping the stream closes it: the peer sees EOF
                // before any state crosses the socket.
                self.refused += 1;
                continue;
            }
            if stream.set_nonblocking(true).is_err() || self.readiness.add(&stream).is_err() {
                self.refused += 1;
                continue;
            }
            self.fresh.push(stream);
            self.fresh_peers.push(cred);
        }
        // Tokens expire: reissue every window's before the shell's go
        // stale. Windows that did not change send no delta in between.
        if self.tokens_reissued.elapsed() >= TOKEN_REISSUE {
            model.reissue(None);
            self.tokens_reissued = std::time::Instant::now();
        }
        // A narrowed gate evicts sessions it no longer admits.
        let mut i = 0;
        while i < self.sessions.len() {
            if self.gate.admits(self.session_peers[i], uid) {
                i += 1;
            } else {
                self.sessions.swap_remove(i);
                self.session_peers.swap_remove(i);
            }
        }
        self.advance_pending(model);
        self.pending = std::mem::take(&mut self.fresh);
        self.pending_peers = std::mem::take(&mut self.fresh_peers);
        let mut outcome = PollOutcome::default();
        let mut i = 0;
        while i < self.sessions.len() {
            let alive = {
                let session = &mut self.sessions[i];
                let before = model.focused();
                let workspace_before = model.active_workspace();
                let deltas_ok = session.emit_deltas(model).is_ok();
                session.pad_targets = self.pad_targets.clone();
                let cmd_ok = match session.handle_next(model) {
                    Ok(_) => true,
                    Err(ControlError::WouldBlock) => true,
                    Err(_) => false,
                };
                if deltas_ok && cmd_ok {
                    let mut reply_ok = true;
                    if model.active_workspace() != workspace_before {
                        // FocusWorkspace changes the model directly; preserve
                        // every departed slot so rapid commands cannot be
                        // compacted into a snap-back.
                        outcome.departed_workspaces.push(workspace_before);
                    }
                    if model.focused() != before {
                        outcome.activated.extend(model.focused());
                    }
                    outcome.closed.extend(Self::take_closed(session));
                    if let Some(ms) = session.take_idle_timeout() {
                        outcome.idle_timeout = Some(ms);
                    }
                    if let Some(list) = session.take_accelerators() {
                        outcome.accelerators = Some(list);
                    }
                    outcome.window_actions.extend(session.take_window_actions());
                    outcome
                        .shortcut_consent
                        .extend(std::mem::take(&mut session.shortcut_consent));
                    outcome
                        .display_change
                        .extend(std::mem::take(&mut session.display_change));
                    outcome
                        .pad_osd_requests
                        .extend(std::mem::take(&mut session.pad_osd_requests));
                    outcome
                        .pad_osd_closed
                        .extend(std::mem::take(&mut session.pad_osd_closed));
                    outcome
                        .input_source_switches
                        .extend(std::mem::take(&mut session.input_source_switches));
                    if let Some(thumbnails) = session.switcher_thumbnails.take() {
                        outcome.switcher_thumbnails = Some(thumbnails);
                    }
                    if let Some(keys) = session.switcher_keys.take() {
                        outcome.switcher_keys = Some(keys);
                    }
                    if let Some(active) = session.overview_search.take() {
                        outcome.overview_search = Some(active);
                    }
                    if let Some(active) = session.overview_app_grid.take() {
                        outcome.overview_app_grid = Some(active);
                    }
                    if let Some(request) = session.unlock_request.take() {
                        let (request, password) = request;
                        let origin = crate::auth_owner::Origin {
                            connection: session.connection_id,
                            request,
                        };
                        if outcome.unlock.is_none() {
                            outcome.unlock = Some((origin, password));
                        } else {
                            session.unlock_pending = None;
                            reply_ok = session
                                .conn
                                .write_frame(&Message::CommandResult {
                                    id: request,
                                    status: CommandStatus::Denied {
                                        reason: "authentication failed".into(),
                                    },
                                })
                                .is_ok();
                        }
                    }
                    for (request, service) in std::mem::take(&mut session.auth_begin) {
                        outcome.auth_begin.push((
                            crate::auth_owner::Origin {
                                connection: session.connection_id,
                                request,
                            },
                            service,
                        ));
                    }
                    for (service, sequence, answer) in std::mem::take(&mut session.auth_answers) {
                        outcome.auth_answers.push((
                            session.connection_id,
                            service,
                            sequence,
                            answer,
                        ));
                    }
                    if std::mem::take(&mut session.auth_cancel) {
                        outcome.auth_cancel.push(session.connection_id);
                    }
                    if let Some(geometries) = session.icon_geometries.take() {
                        outcome.icon_geometries = Some(geometries);
                    }
                    if let Some(effects) = session.wellbeing_effects.take() {
                        outcome.wellbeing_effects = Some(effects);
                    }
                    if let Some(settings) = session.input_settings.take() {
                        outcome.input_settings = Some(settings);
                    }
                    if let Some(pos) = session.screenshot_pointer.take() {
                        outcome.screenshot_pointer = Some(pos);
                    }
                    if let Some(enabled) = session.screen_reader.take() {
                        outcome.screen_reader = Some(enabled);
                    }
                    if let Some(click) = session.dwell_click.take() {
                        outcome.dwell_click = Some(click);
                    }
                    outcome
                        .osk_inputs
                        .extend(std::mem::take(&mut session.osk_inputs));
                    reply_ok
                } else {
                    false
                }
            };
            if alive {
                i += 1;
            } else {
                self.sessions.swap_remove(i);
                self.session_peers.swap_remove(i);
            }
        }
        // A Lock command changes the logical lock before Runtime::engage_lock
        // runs. Do not broadcast pre-lock presses or releases in that gap.
        if !locked_before && self.locked.get() {
            self.cancel_pending_accelerators();
        }
        self.broadcast();
        outcome
    }

    /// Send what the runtime queued or changed since the last
    /// [`poll`](Self::poll) without reading commands: model deltas, the
    /// lock flag, and every pending broadcast. The runtime calls this at
    /// the end of each loop iteration so nothing waits for the next wake.
    pub fn flush(&mut self, model: &StateModel) {
        let mut i = 0;
        while i < self.sessions.len() {
            if self.sessions[i].emit_deltas(model).is_ok() {
                i += 1;
            } else {
                self.sessions.swap_remove(i);
                self.session_peers.swap_remove(i);
            }
        }
        self.broadcast();
    }

    fn broadcast(&mut self) {
        // Broadcast overview flips (002 R1): best-effort per session,
        // staying dirty until every live session holds the intent.
        if self.overview.get() != self.overview_sent {
            let open = self.overview.get();
            let mut all_sent = true;
            for session in &mut self.sessions {
                if session.send_overview(open).is_err() {
                    all_sent = false;
                }
            }
            if all_sent {
                self.overview_sent = open;
            }
        }
        // Broadcast the output inventory (multi-monitor): best-effort
        // per session like overview, staying dirty until every live
        // session holds it.
        if self.environment != self.environment_sent {
            let mut all_sent = true;
            for session in &mut self.sessions {
                if session.send_environment(&self.environment).is_err() {
                    all_sent = false;
                }
            }
            if all_sent {
                self.environment_sent = self.environment.clone();
            }
        }
        if self.previews != self.previews_sent {
            let mut all_sent = true;
            for session in &mut self.sessions {
                if session
                    .send_overview_previews(&self.previews.0, self.previews.1)
                    .is_err()
                {
                    all_sent = false;
                }
            }
            if all_sent {
                self.previews_sent = self.previews.clone();
            }
        }
        if self.native_outputs != self.native_outputs_sent {
            let mut all_sent = true;
            for session in &mut self.sessions {
                if session.send_native_outputs(&self.native_outputs).is_err() {
                    all_sent = false;
                }
            }
            if all_sent {
                self.native_outputs_sent = self.native_outputs.clone();
            }
        }
        if self.osk_sent != Some(self.osk) {
            let mut all_sent = true;
            for session in &mut self.sessions {
                if session.send_on_screen_keyboard(self.osk).is_err() {
                    all_sent = false;
                }
            }
            if all_sent {
                // The input-panel request is an edge: sent once.
                self.osk.show = false;
                self.osk_sent = Some(self.osk);
            }
        }
        if self.outputs != self.outputs_sent {
            let mut all_sent = true;
            for session in &mut self.sessions {
                if session.send_outputs(&self.outputs).is_err() {
                    all_sent = false;
                }
            }
            if all_sent {
                self.outputs_sent = self.outputs.clone();
            }
        }
        // Broadcast switcher drive events (002 workspaces): each queued
        // action goes to every live session exactly once; actions that
        // miss a session stay queued for the next poll.
        if !self.switcher_queue.is_empty() {
            let pending = std::mem::take(&mut self.switcher_queue);
            let mut unsent = Vec::with_capacity(pending.len());
            for action in pending {
                let mut all_sent = true;
                for session in &mut self.sessions {
                    if session.send_switcher(action).is_err() {
                        all_sent = false;
                    }
                }
                if !all_sent {
                    unsent.push(action);
                }
            }
            self.switcher_queue = unsent;
        }
        // Accelerator presses go to every live session once (the shell
        // signals the grabbing D-Bus caller).
        for (action, time, mode, released) in std::mem::take(&mut self.accelerator_queue) {
            for session in &mut self.sessions {
                let _ = if released {
                    session.send_accelerator_deactivated(action, time, mode)
                } else {
                    session.send_accelerator(action, time, mode)
                };
            }
        }
        for menu in std::mem::take(&mut self.menu_queue) {
            for session in &mut self.sessions {
                let _ = session.send_window_menu(&menu);
            }
        }
    }

    /// Try each aged peer's handshake once. Peers were accepted a full
    /// round ago, so a well-behaved peer's `Hello` is already waiting; a
    /// still-silent peer is dropped and must reconnect. One attempt per
    /// connection (`ControlConn` owns its stream, so a half-open
    /// handshake cannot be resumed) keeps silent peers from pinning slots.
    fn advance_pending(&mut self, model: &StateModel) {
        let pending = std::mem::take(&mut self.pending);
        let peers = std::mem::take(&mut self.pending_peers);
        let uid = our_uid();
        for (stream, peer) in pending.into_iter().zip(peers) {
            // The gate may have narrowed since accept (shell restart).
            if !self.gate.admits(peer, uid) {
                self.refused += 1;
                continue;
            }
            let conn = match ControlConn::new(stream) {
                Ok(conn) => conn,
                Err(_) => continue,
            };
            let validate = self.store.clone().validator(self.seat.clone());
            let validator =
                move |token: &ActivationToken, app_id: Option<&str>| validate(&token.0, app_id);
            let minter = std::rc::Rc::new(self.store.clone().minter(self.seat.clone()));
            let overview = self.overview.clone();
            let locked = self.locked.clone();
            let open = overview.get();
            let Some(connection_id) = self.next_connection else {
                self.refused += 1;
                continue;
            };
            // Reserve before handshake, so failed admissions cannot reuse an ID.
            self.next_connection = connection_id.checked_add(1);
            if let Ok(mut session) =
                Session::handshake_with(conn, model, validator, minter, overview, locked)
            {
                // Newcomers join the hub's live lockdown policy (#357).
                session.set_lockdown_policy(self.lockdown);
                // Newcomers join mid-state: tell them the intent and the
                // inventory now (best-effort; the poll broadcast covers
                // the rest). An empty inventory means unknown — never
                // send it; the shell keeps its legacy surfaces until a
                // real one arrives.
                let _ = session.send_overview(open);
                // A newcomer holds no native authority yet, so there is
                // nothing for an empty inventory to revoke.
                if !self.native_outputs.is_empty() {
                    let _ = session.send_native_outputs(&self.native_outputs);
                }
                if !self.outputs.is_empty() {
                    let _ = session.send_outputs(&self.outputs);
                }
                if !self.environment.is_empty() {
                    let _ = session.send_environment(&self.environment);
                    let _ = session.send_overview_previews(&self.previews.0, self.previews.1);
                }
                let osk = OskBroadcast {
                    show: false,
                    ..self.osk
                };
                if osk != OskBroadcast::default() {
                    let _ = session.send_on_screen_keyboard(osk);
                }
                if self.pointer_output.is_some() {
                    let _ = session.send_window_menu(&Message::PointerOutput {
                        name: self.pointer_output.clone(),
                    });
                }
                session.connection_id = connection_id;
                session.auth_owner = self.auth_owner;
                self.sessions.push(session);
                self.session_peers.push(peer);
            }
        }
    }
}

impl Drop for ControlHub {
    /// Best-effort socket cleanup; a stale file would block the next bind
    /// (which also removes it first).
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

#[cfg(test)]
mod readiness_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    #[test]
    fn buffered_complete_frames_require_continuation_but_partial_frames_wait_for_io() {
        let (stream, _peer) = UnixStream::pair().unwrap();
        let mut conn = ControlConn::new(stream).unwrap();
        conn.rbuf = vec![1, 0, 0];
        assert!(!conn.has_buffered_frame());
        conn.rbuf = vec![2, 0, 0, 0, 1];
        assert!(!conn.has_buffered_frame());
        conn.rbuf.push(2);
        assert!(conn.has_buffered_frame());
        conn.rbuf = (MAX_FRAME_BYTES as u32 + 1).to_le_bytes().to_vec();
        assert!(
            conn.has_buffered_frame(),
            "invalid buffered header must be rejected without another socket edge"
        );
    }
    #[test]
    fn partial_buffer_and_unread_kernel_bytes_rewake_without_a_new_write() {
        use std::os::fd::AsRawFd;

        for old_edge in [true, false] {
            let (receiver, mut sender) = UnixStream::pair().unwrap();
            let mut conn = ControlConn::new(receiver).unwrap();
            let readiness = if old_edge {
                crate::control_readiness::ControlReadiness::old_edge_fixture(&conn.stream)
            } else {
                crate::control_readiness::ControlReadiness::new(&conn.stream)
            }
            .unwrap();
            let mut event_loop = calloop::EventLoop::<usize>::try_new().unwrap();
            event_loop
                .handle()
                .insert_source(readiness.source().unwrap(), |_, fd, wakes| {
                    crate::control_readiness::drain(fd).unwrap();
                    *wakes += 1;
                    Ok(calloop::PostAction::Continue)
                })
                .unwrap();
            let first = Message::Command {
                id: 1,
                kind: CommandKind::SetIdleTimeout { ms: 10 },
            };
            // A valid maximum-count session command with multi-byte postcard
            // integers spans the actual 8192-byte ControlConn read boundary.
            let second = Message::Command {
                id: 2,
                kind: CommandKind::SetAccelerators {
                    accelerators: vec![
                        tuna_shell_control::Accelerator {
                            action: u32::MAX,
                            keysym: u32::MAX,
                            mods: u32::MAX,
                            modes: u32::MAX,
                        };
                        tuna_shell_control::MAX_ACCELERATORS
                    ],
                },
            };
            let first_frame = encode_frame(&first);
            let second_frame = encode_frame(&second);
            assert!(second_frame.len() > 8192 && second_frame.len() < MAX_FRAME_BYTES);
            assert_eq!(decode_frame(&second_frame).unwrap(), second);
            let bytes = [first_frame.as_slice(), second_frame.as_slice()].concat();
            assert_eq!(
                sender.write(&bytes).unwrap(),
                bytes.len(),
                "one bounded socket write"
            );
            let mut wakes = 0;
            event_loop
                .dispatch(Some(Duration::from_secs(1)), &mut wakes)
                .unwrap();
            assert_eq!(wakes, 1);
            assert_eq!(conn.read_frame().unwrap(), first);
            assert_eq!(conn.rbuf.len(), 8192 - first_frame.len());
            assert!(
                !conn.has_buffered_frame(),
                "partial userspace frame does not request continuation"
            );
            let mut unread: libc::c_int = 0;
            assert_eq!(
                unsafe { libc::ioctl(conn.stream.as_raw_fd(), libc::FIONREAD, &mut unread) },
                0
            );
            assert!(
                unread > 0,
                "remaining bytes are already in the kernel, no future write needed"
            );

            // No sender write, external input, animation or runtime timer here.
            event_loop
                .dispatch(Some(Duration::ZERO), &mut wakes)
                .unwrap();
            assert_eq!(
                wakes,
                if old_edge { 1 } else { 2 },
                "old inner edge loses the wake; fixed inner level preserves it"
            );
            let before_close = wakes;
            assert_eq!(
                conn.read_frame().unwrap(),
                second,
                "frame bytes and decode unchanged"
            );
            assert!(matches!(conn.read_frame(), Err(ControlError::WouldBlock)));
            event_loop
                .dispatch(Some(Duration::ZERO), &mut wakes)
                .unwrap();
            assert_eq!(
                wakes, before_close,
                "fully drained idle socket must not spin"
            );

            drop(sender);
            event_loop
                .dispatch(Some(Duration::from_secs(1)), &mut wakes)
                .unwrap();
            assert_eq!(
                wakes,
                before_close + 1,
                "peer closure must wake the registered receiver"
            );
            assert!(matches!(
                conn.read_frame(),
                Err(ControlError::Unexpected(_))
            ));
            drop(conn);
            event_loop
                .dispatch(Some(Duration::ZERO), &mut wakes)
                .unwrap();
            assert_eq!(
                wakes,
                before_close + 1,
                "closed original socket must retire interest without a duplicate"
            );
        }
    }

    #[test]
    fn hub_listener_hello_commands_and_peer_eviction_use_real_readiness() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = dir.path().join("control.sock");
        let mut hub = ControlHub::bind(
            path.clone(),
            std::rc::Rc::new(TokenStore::new()),
            crate::SEAT_NAME,
        )
        .unwrap();
        let mut model = StateModel::new();
        let mut event_loop = calloop::EventLoop::<usize>::try_new().unwrap();
        event_loop
            .handle()
            .insert_source(hub.readiness_source().unwrap(), |_, fd, wakes| {
                crate::control_readiness::drain(fd).unwrap();
                *wakes += 1;
                Ok(calloop::PostAction::Continue)
            })
            .unwrap();
        let mut wakes = 0;
        let mut client = UnixStream::connect(&path).unwrap();
        event_loop
            .dispatch(Some(Duration::from_secs(1)), &mut wakes)
            .unwrap();
        assert_eq!(wakes, 1, "listener must wake before a periodic tick");
        hub.poll(&mut model);
        assert!(
            hub.next_poll(&model).is_some(),
            "accepted Hello phase needs a bounded continuation"
        );
        client
            .write_all(&encode_frame(&Message::Hello {
                version: CURRENT_VERSION,
            }))
            .unwrap();
        event_loop
            .dispatch(Some(Duration::from_secs(1)), &mut wakes)
            .unwrap();
        assert_eq!(wakes, 2, "accepted peer must wake for its Hello");
        hub.poll(&mut model);
        assert_eq!(hub.sessions.len(), 1);
        event_loop
            .dispatch(Some(Duration::ZERO), &mut wakes)
            .unwrap();
        assert_eq!(wakes, 2, "drained control must not spin");
        client
            .write_all(&encode_frame(&Message::Command {
                id: 1,
                kind: CommandKind::ToggleOverview,
            }))
            .unwrap();
        event_loop
            .dispatch(Some(Duration::from_secs(1)), &mut wakes)
            .unwrap();
        assert_eq!(wakes, 3);
        hub.poll(&mut model);
        assert!(hub.overview_open());
        client
            .write_all(&encode_frame(&Message::Command {
                id: 2,
                kind: CommandKind::FocusWorkspace { workspace: 1 },
            }))
            .unwrap();
        event_loop
            .dispatch(Some(Duration::from_secs(1)), &mut wakes)
            .unwrap();
        assert_eq!(wakes, 4);
        let outcome = hub.poll(&mut model);
        assert_eq!(model.active_workspace(), 1);
        assert_eq!(outcome.departed_workspaces, [0]);
        hub.set_peer_gate(PeerGate::Closed);
        hub.poll(&mut model);
        assert!(hub.sessions.is_empty());
        event_loop
            .dispatch(Some(Duration::ZERO), &mut wakes)
            .unwrap();
        assert_eq!(wakes, 4, "evicted socket interest must retire");
        // Concurrent forked test children may briefly inherit the peer until
        // exec closes it. Still reject a persistent duplicate: WouldBlock is
        // never closure, and queued replies are drained before checking EOF.
        crate::control_readiness::test_peer::wait_for_peer_close(
            &mut client,
            Duration::from_secs(1),
        )
        .expect("readiness source must not retain a duplicate peer socket");
    }
}

#[cfg(test)]
mod auth_origin_tests {
    use super::*;
    use crate::auth_owner::Origin;
    use std::net::Shutdown;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    fn setup() -> (tempfile::TempDir, ControlHub, StateModel) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(is_private_dir(dir.path()));
        let hub = ControlHub::bind(
            dir.path().join("control"),
            std::rc::Rc::new(TokenStore::new()),
            crate::SEAT_NAME,
        )
        .unwrap();
        (dir, hub, StateModel::new())
    }
    fn connect(hub: &mut ControlHub, model: &mut StateModel) -> ControlConn {
        let mut client = ControlConn::new(UnixStream::connect(hub.socket_path()).unwrap()).unwrap();
        client
            .write_frame(&Message::Hello {
                version: CURRENT_VERSION,
            })
            .unwrap();
        for _ in 0..3 {
            hub.poll(model);
        }
        drain(&mut client);
        client
    }
    fn drain(client: &mut ControlConn) -> Vec<(u64, CommandStatus)> {
        let mut out = Vec::new();
        loop {
            match client.read_frame() {
                Ok(Message::CommandResult { id, status }) => out.push((id, status)),
                Ok(_) => {}
                Err(ControlError::WouldBlock) => return out,
                Err(error) => panic!("unexpected control error {error:?}"),
            }
        }
    }
    fn submit(client: &mut ControlConn, id: u64) {
        client
            .write_frame(&Message::Command {
                id,
                kind: CommandKind::Unlock {
                    password: tuna_shell_control::Secret("fixture-only".into()),
                },
            })
            .unwrap();
    }
    #[test]
    fn same_wire_id_routes_only_to_origin_and_all_excess_requests_are_denied() {
        let (_dir, mut hub, mut model) = setup();
        let mut a = connect(&mut hub, &mut model);
        let mut b = connect(&mut hub, &mut model);
        submit(&mut a, 1);
        submit(&mut b, 1);
        let (origin, _) = hub.poll(&mut model).unlock.unwrap();
        assert!(hub.origin_pending_live(origin));
        assert!(drain(&mut a).is_empty());
        assert!(matches!(
            drain(&mut b).as_slice(),
            [(1, CommandStatus::Denied { .. })]
        ));
        hub.finish_unlock(origin, true);
        assert_eq!(drain(&mut a), [(1, CommandStatus::Applied)]);
        assert!(drain(&mut b).is_empty());
        submit(&mut a, 4);
        let (first, _) = hub.poll(&mut model).unlock.unwrap();
        submit(&mut a, 5);
        assert!(hub.poll(&mut model).unlock.is_none());
        assert!(matches!(
            drain(&mut a).as_slice(),
            [(5, CommandStatus::Denied { .. })]
        ));
        assert!(hub.origin_pending_live(first));
        hub.finish_unlock(first, false);
        assert!(matches!(
            drain(&mut a).as_slice(),
            [(4, CommandStatus::Denied { .. })]
        ));
    }
    #[test]
    fn eof_with_unread_bytes_compaction_reconnect_and_reused_ids_do_not_transfer_owner() {
        let (_dir, mut hub, mut model) = setup();
        let mut a = connect(&mut hub, &mut model);
        let mut b = connect(&mut hub, &mut model);
        submit(&mut a, 1);
        let (old, _) = hub.poll(&mut model).unlock.unwrap();
        // Peer half-close must be observed even with unread commands ahead of EOF.
        a.write_frame(&Message::Command {
            id: 9,
            kind: CommandKind::ToggleOverview,
        })
        .unwrap();
        a.stream.shutdown(Shutdown::Write).unwrap();
        assert!(!hub.origin_pending_live(old));
        drop(a);
        hub.poll(&mut model);
        hub.poll(&mut model);
        assert_eq!(hub.sessions.len(), 1);
        submit(&mut b, 1);
        let (other, _) = hub.poll(&mut model).unlock.unwrap();
        assert_ne!(other.connection, old.connection);
        assert!(hub.origin_pending_live(other));
        let mut replacement = connect(&mut hub, &mut model);
        submit(&mut replacement, 1);
        let (fresh, _) = hub.poll(&mut model).unlock.unwrap();
        assert_ne!(fresh.connection, old.connection);
        assert_ne!(fresh.connection, other.connection);
        hub.finish_unlock(old, true);
        assert!(drain(&mut b).is_empty());
        assert!(drain(&mut replacement).is_empty());
        hub.finish_unlock(other, false);
        hub.finish_unlock(fresh, false);
        assert!(matches!(
            drain(&mut b).as_slice(),
            [(1, CommandStatus::Denied { .. })]
        ));
        assert!(matches!(
            drain(&mut replacement).as_slice(),
            [(1, CommandStatus::Denied { .. })]
        ));
    }
    #[test]
    fn changed_gate_and_unknown_connection_cannot_authorize_before_poll() {
        let (_dir, mut hub, mut model) = setup();
        let mut client = connect(&mut hub, &mut model);
        submit(&mut client, 1);
        let (origin, _) = hub.poll(&mut model).unlock.unwrap();
        assert!(hub.origin_pending_live(origin));
        assert!(!hub.origin_pending_live(Origin {
            connection: origin.connection + 1,
            ..origin
        }));
        hub.set_peer_gate(PeerGate::Closed);
        assert!(!hub.origin_pending_live(origin));
    }
    #[test]
    fn auth_lease_binds_socket_child_generation_and_retirement_not_reply() {
        use crate::auth_owner::{MethodKind, Slot};
        use crate::pam_backend::Verification;
        let (_dir, mut hub, mut model) = setup();
        let owner = AuthOwner {
            pid: std::process::id(),
            generation: 1,
        };
        hub.set_peer_gate(PeerGate::Pid(owner.pid));
        hub.set_auth_owner(Some(owner));
        let mut a = connect(&mut hub, &mut model);
        submit(&mut a, 1);
        let (origin, _) = hub.poll(&mut model).unlock.unwrap();
        let mut slot = Slot::default();
        let attempt = slot
            .begin_method(Some(origin), 7, MethodKind::PamConversation)
            .unwrap();
        let lease = hub
            .bind_auth_lease(
                origin,
                attempt.serial,
                attempt.epoch,
                MethodKind::PamConversation,
            )
            .unwrap();
        assert!(hub.auth_lease_accepts(
            lease,
            origin,
            attempt.serial,
            7,
            MethodKind::PamConversation
        ));
        for (wrong_origin, serial, epoch, method) in [
            (
                Origin {
                    connection: origin.connection + 1,
                    ..origin
                },
                attempt.serial,
                7,
                MethodKind::PamConversation,
            ),
            (
                Origin {
                    request: 2,
                    ..origin
                },
                attempt.serial,
                7,
                MethodKind::PamConversation,
            ),
            (origin, attempt.serial + 1, 7, MethodKind::PamConversation),
            (origin, attempt.serial, 8, MethodKind::PamConversation),
            (origin, attempt.serial, 7, MethodKind::Fingerprint),
        ] {
            assert!(!hub.auth_lease_accepts(lease, wrong_origin, serial, epoch, method));
        }
        hub.finish_unlock(origin, false);
        assert!(matches!(
            drain(&mut a).as_slice(),
            [(1, CommandStatus::Denied { .. })]
        ));
        assert!(
            hub.auth_lease_live(lease),
            "early command reply cannot release method ownership"
        );
        assert!(!hub.retire_pam_auth_lease(lease, Verification::Unretired));
        assert!(slot.matches(attempt));
        assert!(hub.retire_pam_auth_lease(
            lease,
            Verification::Retired {
                authenticated: false
            }
        ));
        assert!(slot.retire_pam_method(
            attempt,
            MethodKind::PamConversation,
            Verification::Retired {
                authenticated: false
            }
        ));
        submit(&mut a, 1);
        let (next_origin, _) = hub.poll(&mut model).unlock.unwrap();
        let next = slot
            .begin_method(Some(next_origin), 7, MethodKind::PamConversation)
            .unwrap();
        let next_lease = hub
            .bind_auth_lease(
                next_origin,
                next.serial,
                next.epoch,
                MethodKind::PamConversation,
            )
            .unwrap();
        assert!(!hub.retire_pam_auth_lease(
            lease,
            Verification::Retired {
                authenticated: true
            }
        ));
        assert!(hub.auth_lease_live(next_lease));
        hub.set_auth_owner(Some(AuthOwner {
            generation: 2,
            ..owner
        }));
        assert!(!hub.origin_pending_live(next_origin));
        assert!(!hub.auth_lease_live(next_lease));
        crate::control_readiness::test_peer::wait_for_peer_close(
            &mut a.stream,
            Duration::from_secs(1),
        )
        .unwrap();
        let mut b = connect(&mut hub, &mut model);
        submit(&mut b, 1);
        let (fresh_origin, _) = hub.poll(&mut model).unlock.unwrap();
        assert_ne!(fresh_origin.connection, next_origin.connection);
        // Origin loss is cancellation, not retirement: no newer slot yet.
        assert!(slot
            .begin_method(Some(fresh_origin), 7, MethodKind::Password)
            .is_none());
        assert!(slot.retire_pam_method(
            next,
            MethodKind::PamConversation,
            Verification::Retired {
                authenticated: false
            }
        ));
        let fresh = slot
            .begin_method(Some(fresh_origin), 7, MethodKind::PamConversation)
            .unwrap();
        let fresh_lease = hub
            .bind_auth_lease(fresh_origin, fresh.serial, 7, MethodKind::PamConversation)
            .unwrap();
        assert!(!hub.retire_pam_auth_lease(
            next_lease,
            Verification::Retired {
                authenticated: true
            }
        ));
        assert!(hub.auth_lease_live(fresh_lease));
        b.stream.shutdown(Shutdown::Write).unwrap();
        assert!(!hub.auth_lease_live(fresh_lease));
        hub.set_auth_owner(None);
        assert!(!hub.origin_pending_live(fresh_origin));
        assert!(slot.matches(fresh));
    }
    #[test]
    fn missing_child_identity_cannot_bind_an_auth_lease() {
        let (_dir, mut hub, mut model) = setup();
        let mut client = connect(&mut hub, &mut model);
        submit(&mut client, 1);
        let (origin, _) = hub.poll(&mut model).unlock.unwrap();
        assert!(hub
            .bind_auth_lease(origin, 1, 7, crate::auth_owner::MethodKind::Password)
            .is_none());
        hub.set_auth_owner(Some(AuthOwner {
            pid: std::process::id() + 1,
            generation: 1,
        }));
        let mut wrong_peer = connect(&mut hub, &mut model);
        submit(&mut wrong_peer, 1);
        let (wrong_origin, _) = hub.poll(&mut model).unlock.unwrap();
        assert!(
            !hub.origin_pending_live(wrong_origin),
            "SameUser gate cannot substitute for the current child credentials"
        );
        assert!(hub
            .bind_auth_lease(wrong_origin, 1, 7, crate::auth_owner::MethodKind::Password)
            .is_none());
        hub.set_auth_owner(Some(AuthOwner {
            pid: std::process::id(),
            generation: 0,
        }));
        assert!(!hub.origin_pending_live(origin));
    }
    #[test]
    fn connection_id_overflow_refuses_future_peers_without_reuse() {
        let (_dir, mut hub, mut model) = setup();
        hub.next_connection = Some(u64::MAX);
        let mut a = connect(&mut hub, &mut model);
        submit(&mut a, 1);
        let (origin, _) = hub.poll(&mut model).unlock.unwrap();
        assert_eq!(origin.connection, u64::MAX);
        let mut b = UnixStream::connect(hub.socket_path()).unwrap();
        b.write_all(&encode_frame(&Message::Hello {
            version: CURRENT_VERSION,
        }))
        .unwrap();
        for _ in 0..3 {
            hub.poll(&mut model);
        }
        crate::control_readiness::test_peer::wait_for_peer_close(
            &mut b,
            std::time::Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(hub.sessions.len(), 1);
        assert!(hub.next_connection.is_none());
        assert!(hub.refused_count() > 0);
    }
}
