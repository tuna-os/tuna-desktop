//! Fingerprint unlock through fprintd (#366, P-LK-02 fingerprint subset).
//!
//! When a reader is present and the session user has enrolled prints,
//! the lock screen may offer a fingerprint attempt alongside the
//! password. This module is the only place that speaks to fprintd's
//! D-Bus API; everything else goes through [`Capability`] (truthful
//! availability after enrollment) and [`Fprintd::verify`] (one bounded
//! attempt). Smartcard/PIN, multi-step and web login stay queued for
//! Wave 3; enrollment UI lives in Settings, not here.
//!
//! D-Bus shapes follow the fprintd reference manual
//! (`net.reactivated.Fprint.Manager` / `.Device` on the system bus):
//! `GetDevices`/`GetDefaultDevice`, `Claim(username)` + `Release`,
//! `VerifyStart("any")` + `VerifyStop`, `ListEnrolledFingers`, and the
//! `VerifyStatus(result, done)` signal whose `result` is one of
//! `verify-match`, `verify-no-match`, the `verify-retry-*` reasons,
//! `verify-unknown-error` or `verify-disconnected`. Prompt wording
//! matches fprintd's `pam/fingerprint-strings.h` English source
//! strings (see [`tuna_greeter::model::is_fingerprint_prompt`]).
//!
//! Failure rules: no daemon, no device, no enrolled prints, a denied
//! PolicyKit check, a dropped connection or a timeout all fail closed
//! ([`VerifyOutcome::Error`] and friends — never unlock), carry no
//! detail, and log nothing. Every D-Bus wait is bounded: the absolute
//! attempt deadline starts before setup, the connect itself retries a
//! full listener backlog only within the remaining budget, setup races
//! the remainder, method calls carry the bound fixed from the setup
//! remainder (NOT recomputed per call — whole-attempt enforcement past
//! that is the parent shutdown), and the signal wait ends on shutdown
//! or peer loss with the deadline applied to the verdict — so a wedged
//! daemon can delay but never hang the caller. Each attempt runs on a
//! private connection that the deadline shuts down at the fd level
//! (bypassing the transport write lock a saturated send may hold), so
//! a silent or backpressured daemon cannot strand the signal wait past
//! it. Only socket waits are released that way: computation that never
//! touches the socket (notably NSS user lookup) can outlive the
//! deadline, and then the retirement budget expiry — not a prompt
//! exit — retires the claim, into quarantine (fail closed).
//! Fingerprints never leave the daemon; this client only learns
//! match/no-match.
//!
//! Single-flight ownership: one [`ClaimGuard`] per attempt, held first
//! by the parent, then by a retained reaper thread that also holds the
//! worker's join handle. The claim releases ONLY on the verified
//! condition (worker joined after its completion signal, private fd
//! shutdown observed, all attempt handles dropped); every failure,
//! timeout, or unwind quarantines instead (later attempts deny,
//! fail closed). A guard's `Drop` never releases — quarantine only.
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::{AsRawFd, FromRawFd};
use std::os::unix::net::UnixStream;
use std::sync::{
    atomic::{AtomicU8, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};

/// Test-only panic flag below lives at module scope.
#[cfg(test)]
use std::sync::atomic::AtomicBool;

/// fprintd's well-known name on the system bus.
pub const BUS_NAME: &str = "net.reactivated.Fprint";
/// Object path of the manager interface.
pub const MANAGER_PATH: &str = "/net/reactivated/Fprint/Manager";
/// `GetDevices` / `GetDefaultDevice`.
pub const MANAGER_IFACE: &str = "net.reactivated.Fprint.Manager";
/// Claim/verify/enrolled-fingers plus `VerifyStatus`.
pub const DEVICE_IFACE: &str = "net.reactivated.Fprint.Device";
/// Finger name selecting any enrolled print for `VerifyStart`.
pub const VERIFY_ANY: &str = "any";
/// Setting this variable (any value) disables fingerprint auth: every
/// probe reports unavailable and every attempt fails closed. Tests and
/// hardened images use it to pin the no-reader path.
pub const DISABLE_ENV: &str = "TUNA_FPRINTD_OFF";
/// Bound on one D-Bus method call (device lookup, enrollment query,
/// claim, verify start/stop, release). The bus default waits forever;
/// a wedged daemon must fail the attempt, not hang the caller.
pub const DBUS_CALL_TIMEOUT: Duration = Duration::from_secs(10);
/// Default bound on one verification (finger press included).
pub const VERIFY_TIMEOUT: Duration = Duration::from_secs(30);
/// Bound on intermediate retry signals per attempt. A daemon that
/// keeps asking for another swipe denies instead of looping forever.
pub const MAX_VERIFY_RETRIES: usize = 10;
/// Post-deadline wind-down budget for one worker. The reaper's
/// retirement deadline is absolute — the attempt deadline plus this
/// grace — and NEVER counts from spawn: a healthy attempt running
/// longer than the grace must never quarantine early. Past the
/// retirement deadline the attempt quarantines instead of holding the
/// claim forever. Post-deadline worker work is itself expected to be
/// short (the deadline shutdown fails its socket waits fast; each
/// cleanup call carries at most [`DBUS_CALL_TIMEOUT`]), but only
/// socket waits are released that way: computation that never touches
/// the socket (notably NSS user lookup) can outlive the deadline, and
/// then the budget expiry — not a prompt exit — is what retires the
/// claim, into quarantine.
pub const REAPER_GRACE: Duration = Duration::from_secs(20);
/// Cap on waiting for actual thread exit after the worker's completion
/// signal (or death): the reaper joins only an observed-finished
/// worker, and only within the remaining retirement budget. Completion
/// precedes thread return by program order, so this covers scheduling
/// stalls, not logic; an exit that stays unobserved quarantines and
/// detaches instead of parking the reaper.
pub const REAPER_JOIN_GRACE: Duration = Duration::from_secs(5);
/// Sleep between backlog-full (`EAGAIN`) connect retries. The absolute
/// attempt deadline bounds the loop; this only sets the retry cadence.
const CONNECT_RETRY_SLEEP: Duration = Duration::from_millis(2);

/// Outcome of one fingerprint verification. Only [`Self::Matched`]
/// unlocks; every other variant keeps the session locked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyOutcome {
    /// The pressed finger matched an enrolled print.
    Matched,
    /// A complete press matched none of the enrolled prints.
    NoMatch,
    /// fprintd is disabled, unreachable, or has no reader.
    NoDevice,
    /// The reader works but this user enrolled no prints.
    NotEnrolled,
    /// The attempt was cancelled.
    Cancelled,
    /// No terminal signal arrived within the deadline.
    TimedOut,
    /// Claim denied, daemon error, disconnect, or malformed reply.
    Error,
}

/// Truthful fingerprint capability for one user, after checking the
/// daemon and that user's enrollment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capability {
    /// A reader answered on the system bus.
    pub available: bool,
    /// Enrolled finger names (`ListEnrolledFingers`), empty when none.
    pub enrolled_fingers: Vec<String>,
}

impl Capability {
    /// The fail-closed capability: nothing offered.
    pub fn unavailable() -> Self {
        Self {
            available: false,
            enrolled_fingers: Vec::new(),
        }
    }

    /// Whether unlock may offer a fingerprint attempt: a reader is
    /// present *and* this user enrolled at least one print.
    pub fn offered(&self) -> bool {
        self.available && !self.enrolled_fingers.is_empty()
    }
}

/// One `VerifyStatus(result, done)` signal, classified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifySignal {
    /// Keep waiting (a retry reason with `done == false`).
    Continue,
    /// Terminal: the attempt ends with this outcome.
    Done(VerifyOutcome),
}

/// Classify one `VerifyStatus` signal body. Terminal results end the
/// attempt; retry reasons (`verify-retry-scan`, `verify-swipe-too-short`,
/// `verify-finger-not-centered`, `verify-remove-and-retry`) continue it.
/// Unknown strings fail closed when terminal, else continue bounded.
/// A match counts only when the verification finished (`done == true`):
/// per the Device contract `done` reports "whether the verification
/// finished and can be stopped", so a nonterminal match is not a
/// decision and the wait continues bounded, like a retry.
pub fn classify_verify_status(result: &str, done: bool) -> VerifySignal {
    match result {
        "verify-match" if done => VerifySignal::Done(VerifyOutcome::Matched),
        "verify-no-match" if done => VerifySignal::Done(VerifyOutcome::NoMatch),
        "verify-unknown-error" | "verify-disconnected" => VerifySignal::Done(VerifyOutcome::Error),
        "verify-retry-scan"
        | "verify-swipe-too-short"
        | "verify-finger-not-centered"
        | "verify-remove-and-retry"
        | "verify-no-match"
        | "verify-match" => VerifySignal::Continue,
        _ if done => VerifySignal::Done(VerifyOutcome::Error),
        _ => VerifySignal::Continue,
    }
}

/// The username argument fprintd methods should receive. fprintd
/// recommends the empty string for the client's own user (it skips the
/// `setusername` PolicyKit check, which only root passes for anyone
/// else); an explicit name is kept only when it differs from the
/// process user, and then the daemon decides (fail closed on denial).
pub fn effective_claim_name<'a>(user: &'a str, process_user: Option<&str>) -> &'a str {
    if process_user.is_some_and(|owner| owner == user) {
        ""
    } else {
        user
    }
}

/// Drain one attempt's `VerifyStatus` messages until a terminal signal
/// ends it, retries (or malformed messages) run out, the iterator ends,
/// or `deadline` passes. Runs on the worker thread itself, straight off
/// the signal iterator: no forwarder. The iterator itself carries NO
/// timeout — on total silence it blocks until it ends — and it ends
/// when the daemon goes away mid-attempt (fail closed with
/// [`VerifyOutcome::Error`]) or when our private connection is shut
/// down, which the parent does at the deadline (past the deadline that
/// end is [`VerifyOutcome::TimedOut`]). The shutdown — not a receive
/// timeout — is what bounds the silent wait.
fn drain_verify_signals(
    signals: impl Iterator<Item = zbus::message::Message>,
    deadline: Instant,
) -> VerifyOutcome {
    let mut retries = 0usize;
    let mut signals = signals;
    loop {
        if Instant::now() >= deadline {
            return VerifyOutcome::TimedOut;
        }
        let Some(message) = signals.next() else {
            if Instant::now() >= deadline {
                return VerifyOutcome::TimedOut;
            }
            return VerifyOutcome::Error;
        };
        let body = message.body().deserialize::<(String, bool)>();
        match body.map_err(|_| ()) {
            Ok((result, done)) => match classify_verify_status(&result, done) {
                VerifySignal::Done(outcome) => return outcome,
                VerifySignal::Continue => {
                    retries += 1;
                    if retries > MAX_VERIFY_RETRIES {
                        return VerifyOutcome::Error;
                    }
                }
            },
            // Malformed signal: tolerate bounded, like a retry.
            Err(()) => {
                retries += 1;
                if retries > MAX_VERIFY_RETRIES {
                    return VerifyOutcome::Error;
                }
            }
        }
    }
}

/// Process-wide lock serializing [`DISABLE_ENV`] mutation between
/// tests (env is per-process; parallel tests must not interleave a
/// set/assert/remove cycle).
#[cfg(test)]
pub(crate) fn test_serialize_env() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

/// Process-wide single-flight claim for verification attempts: one
/// attempt's ownership at a time. Only [`ClaimGuard`] moves it, through
/// exactly two transitions: `Held -> Free` on the verified retirement
/// condition, anything `-> Quarantined` on failure, timeout, or unwind.
/// Contention and quarantine both deny at once ([`VerifyOutcome::Error`],
/// like the daemon's already-claimed denial) instead of queueing behind
/// a finger press.
static CLAIM: AtomicU8 = AtomicU8::new(ClaimState::FREE);

/// Single-flight claim lifecycle. Discriminants match [`CLAIM`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum ClaimState {
    Free = 0,
    Held = 1,
    Quarantined = 2,
}

impl ClaimState {
    const FREE: u8 = Self::Free as u8;
    const HELD: u8 = Self::Held as u8;
    const QUARANTINED: u8 = Self::Quarantined as u8;

    #[cfg(test)]
    fn of(raw: u8) -> Self {
        match raw {
            Self::HELD => Self::Held,
            Self::QUARANTINED => Self::Quarantined,
            _ => Self::Free,
        }
    }
}

/// Proof that a claim may release: either the reaper joined the worker
/// after its completion signal with the private fd shutdown observed
/// (all attempt handles already dropped by program order), or no worker
/// ever existed and its connection was shut down and dropped. The only
/// constructors are the verified sites themselves; a guard's `Drop`
/// never releases (quarantine only), so a release always names its
/// verified path.
pub(crate) struct RetirementProof {
    _private: (),
}

impl RetirementProof {
    /// The reaper verified join + fd-shutdown + handles-dropped.
    fn reaped() -> Self {
        Self { _private: () }
    }

    /// No worker was ever spawned and nothing is outstanding: either no
    /// connection existed yet, or it was shut down and dropped first.
    fn no_worker() -> Self {
        Self { _private: () }
    }
}

/// Single-flight ownership guard for one verification attempt. The
/// parent holds it through setup and spawn, then transfers ownership to
/// the retained reaper (which also holds the worker's join handle);
/// paths that never spawn a worker release it directly after verified
/// shutdown+drop. `Drop` QUARANTINES an armed guard — it never releases
/// — so a panic or an early return on any failure path denies later
/// attempts (fail closed) instead of freeing a live claim.
pub(crate) struct ClaimGuard {
    armed: bool,
}

impl ClaimGuard {
    /// Claim the single-flight slot, or `None` when an attempt already
    /// holds it or the process quarantined (the caller fails closed).
    fn try_claim() -> Option<Self> {
        CLAIM
            .compare_exchange(
                ClaimState::FREE,
                ClaimState::HELD,
                Ordering::Acquire,
                Ordering::Relaxed,
            )
            .ok()
            .map(|_| Self { armed: true })
    }

    /// Adopt an already-held claim without touching the atomic: the
    /// reaper's guard over the claim the parent claimed and transferred.
    fn adopt() -> Self {
        Self { armed: true }
    }

    /// Ownership moves to the spawned reaper (which holds its own
    /// adopted guard): disarm without touching the atomic.
    fn transfer(mut self) {
        self.armed = false;
    }

    /// Release on the verified condition only. A compare-exchange (not a
    /// blind store) so a release can never free a newer claim — though
    /// holding is exclusive, so a verified release always precedes the
    /// next claim.
    fn release_verified(mut self, _proof: RetirementProof) {
        let _ = CLAIM.compare_exchange(
            ClaimState::HELD,
            ClaimState::FREE,
            Ordering::Release,
            Ordering::Relaxed,
        );
        self.armed = false;
    }

    /// Deny this and all later attempts: the wedged/unverified path.
    /// Quarantine is a bounded state (one atomic), never a parked thread.
    fn quarantine(mut self) {
        CLAIM.store(ClaimState::QUARANTINED, Ordering::Release);
        self.armed = false;
    }

    /// Current lifecycle state (tests observe; production moves forward).
    #[cfg(test)]
    pub(crate) fn test_state() -> ClaimState {
        ClaimState::of(CLAIM.load(Ordering::Acquire))
    }

    /// Force the claim back to free. TESTS ONLY, and only while holding
    /// [`test_serialize_claim`]: the mutex proves no live attempt, worker,
    /// or reaper can exist, so no outstanding claim is freed.
    #[cfg(test)]
    pub(crate) fn test_reset() {
        CLAIM.store(ClaimState::FREE, Ordering::Release);
    }
}

impl Drop for ClaimGuard {
    fn drop(&mut self) {
        if self.armed {
            // Unwind or an unhandled early return: quarantine, never
            // release. A live claim must not free on a path that never
            // verified join + fd-shutdown + handles-dropped.
            CLAIM.store(ClaimState::QUARANTINED, Ordering::Release);
        }
    }
}

/// Synthetic worker panic injection, TESTS ONLY: when set, the next
/// worker panics at start (before any bus work), exercising the
/// reaper's worker-death path for real (unwind, dropped senders,
/// quarantine). Never set in production; tests reset it under the
/// claim serialization mutex.
#[cfg(test)]
static TEST_PANIC_IN_WORKER: AtomicBool = AtomicBool::new(false);

/// Process-wide lock serializing tests that touch the single-flight
/// claim (claim state is per-process; parallel tests must not interleave
/// claim/hold/release cycles).
#[cfg(test)]
pub(crate) fn test_serialize_claim() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

/// Login name of the process owner, for [`effective_claim_name`].
fn process_user() -> Option<String> {
    // SAFETY: getpwuid returns a pointer into static storage, read at once.
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if pw.is_null() {
            return None;
        }
        std::ffi::CStr::from_ptr((*pw).pw_name)
            .to_str()
            .ok()
            .map(str::to_owned)
    }
}

/// A unix bus endpoint: a filesystem socket path or a Linux abstract
/// name (raw bytes — abstract names need not be UTF-8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UnixTarget {
    Path(std::path::PathBuf),
    Abstract(Vec<u8>),
}

/// Outcome of one bounded unix connect (see [`connect_unix_deadline`]).
/// No equality: tests match variants (a connected socket proves itself).
#[derive(Debug)]
pub(crate) enum ConnectOutcome {
    /// Connected; the socket is nonblocking, for the async builder.
    Connected(UnixStream),
    /// Fast failure (no listener, refused, unusable address): no reader.
    Refused,
    /// The absolute deadline passed inside the connect loop.
    TimedOut,
}

/// Outcome of one raced bus setup (see [`build_attempt_connection`]).
enum SetupOutcome {
    /// The async build finished; convert to blocking at the call site.
    Built(zbus::Connection),
    /// The build failed fast (no daemon, refused, unusable address).
    Failed,
    /// The budget ran out first; the raced build was dropped.
    TimedOut,
}

/// Connect to `target` with the absolute `deadline`: a nonblocking
/// `connect` loop that distinguishes the three unix outcomes. A
/// backlog-full listener answers `EAGAIN` — the attempt was never
/// queued, so the `connect` itself retries with [`CONNECT_RETRY_SLEEP`]
/// cadence until the deadline (an `EAGAIN` retry that succeeds returns
/// 0 or `EINPROGRESS`/`EISCONN`). `EINPROGRESS`/`EALREADY` polls
/// writability within the remaining budget and checks `SO_ERROR`.
/// Connectedness is confirmed ONLY by `getpeername` success: `SO_ERROR`
/// reads 0 on an unconnected socket too, so poll+`SO_ERROR` alone
/// proves nothing. Fast failures (`ENOENT`, `ECONNREFUSED`, ...) return
/// [`ConnectOutcome::Refused`] at once. Never panics, never blocks past
/// the deadline, and closes the fd on every failure path.
pub(crate) fn connect_unix_deadline(target: &UnixTarget, deadline: Instant) -> ConnectOutcome {
    // sockaddr_un for the target. Filesystem paths need a trailing NUL;
    // abstract names sit after a leading NUL.
    let mut raw: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    raw.sun_family = libc::AF_UNIX as libc::sa_family_t;
    let field_len = raw.sun_path.len();
    let addr_len: libc::socklen_t = match target {
        UnixTarget::Path(path) => {
            let bytes = path.as_os_str().as_bytes();
            if bytes.len() + 1 > field_len {
                return ConnectOutcome::Refused;
            }
            for (slot, byte) in raw.sun_path.iter_mut().zip(bytes.iter()) {
                *slot = *byte as libc::c_char;
            }
            (std::mem::size_of::<libc::sa_family_t>() + bytes.len() + 1) as libc::socklen_t
        }
        UnixTarget::Abstract(name) => {
            if name.len() + 1 > field_len {
                return ConnectOutcome::Refused;
            }
            for (slot, byte) in raw.sun_path.iter_mut().skip(1).zip(name.iter()) {
                *slot = *byte as libc::c_char;
            }
            (std::mem::size_of::<libc::sa_family_t>() + 1 + name.len()) as libc::socklen_t
        }
    };
    let fd = unsafe {
        libc::socket(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            0,
        )
    };
    if fd < 0 {
        return ConnectOutcome::Refused;
    }
    let close = |fd: libc::c_int| unsafe {
        libc::close(fd);
    };
    let addr_ptr = &raw as *const libc::sockaddr_un as *const libc::sockaddr;
    loop {
        if Instant::now() >= deadline {
            close(fd);
            return ConnectOutcome::TimedOut;
        }
        // SAFETY: fd is an open nonblocking unix socket; addr points at
        // a valid sockaddr_un of addr_len bytes; both outlive the call.
        let ret = unsafe { libc::connect(fd, addr_ptr, addr_len) };
        if ret == 0 {
            return confirm_connected(fd);
        }
        match std::io::Error::last_os_error().raw_os_error() {
            Some(code) if code == libc::EINPROGRESS || code == libc::EALREADY => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    close(fd);
                    return ConnectOutcome::TimedOut;
                }
                let mut pfd = libc::pollfd {
                    fd,
                    events: libc::POLLOUT,
                    revents: 0,
                };
                let ms = remaining.as_millis().min(i32::MAX as u128) as libc::c_int;
                // SAFETY: pfd points at one valid pollfd; the call only
                // reads it and fills revents.
                let polled = unsafe { libc::poll(&mut pfd as *mut libc::pollfd, 1, ms) };
                if polled > 0 {
                    let mut so_error: libc::c_int = 0;
                    let mut optlen = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
                    // SAFETY: out-params point at live locals of the
                    // right size; SO_ERROR always fits a c_int.
                    let opt = unsafe {
                        libc::getsockopt(
                            fd,
                            libc::SOL_SOCKET,
                            libc::SO_ERROR,
                            &mut so_error as *mut libc::c_int as *mut libc::c_void,
                            &mut optlen,
                        )
                    };
                    if opt == 0 && so_error == 0 {
                        // SO_ERROR 0 is necessary but not sufficient:
                        // only getpeername proves connected.
                        return confirm_connected(fd);
                    }
                    close(fd);
                    return ConnectOutcome::Refused;
                } else if polled == 0 {
                    close(fd);
                    return ConnectOutcome::TimedOut;
                } else if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                    continue;
                } else {
                    close(fd);
                    return ConnectOutcome::Refused;
                }
            }
            // Backlog full: nothing was queued, so retry the connect
            // itself (never poll — there is no pending attempt yet).
            Some(code) if code == libc::EAGAIN => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                std::thread::sleep(CONNECT_RETRY_SLEEP.min(remaining));
            }
            // Already connected (a completed attempt retried): confirm.
            Some(code) if code == libc::EISCONN => {
                return confirm_connected(fd);
            }
            Some(code) if code == libc::EINTR => {}
            // ENOENT, ECONNREFUSED, EACCES, ...: no listener to wait for.
            _ => {
                close(fd);
                return ConnectOutcome::Refused;
            }
        }
    }
}

/// The only connected proof: `getpeername` succeeds. Ownership moves to
/// the returned nonblocking [`UnixStream`] exactly once; failures close.
fn confirm_connected(fd: libc::c_int) -> ConnectOutcome {
    let mut peer: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t;
    // SAFETY: out-params point at a live sockaddr_un of the right size.
    let named = unsafe {
        libc::getpeername(
            fd,
            &mut peer as *mut libc::sockaddr_un as *mut libc::sockaddr,
            &mut len,
        )
    };
    if named == 0 {
        // SAFETY: fd is an open socket owned by this function; every
        // other path closes it instead, so ownership moves exactly once.
        ConnectOutcome::Connected(unsafe { UnixStream::from_raw_fd(fd) })
    } else {
        unsafe {
            libc::close(fd);
        }
        ConnectOutcome::Refused
    }
}

/// Build one bus connection over the connected `sock` with setup bounded
/// by `budget`: the async builder runs raced against a timer, so a
/// blackholed handshake fails instead of hanging the caller. Dropping
/// the raced build on timeout is safe cancellation: the builder starts
/// its executor only after the build completes, and an unconfigured
/// client holds no interfaces or names, so nothing lingers past the
/// drop. Method calls on the built connection carry `call_bound` (never
/// the full call timeout past the attempt deadline).
fn build_attempt_connection(
    sock: UnixStream,
    call_bound: Duration,
    budget: Duration,
) -> SetupOutcome {
    let builder = zbus::connection::Builder::async_io_unix_stream(sock).method_timeout(call_bound);
    async_io::block_on(async {
        futures_lite::future::race(
            async move {
                match builder.build().await {
                    Ok(conn) => SetupOutcome::Built(conn),
                    Err(_) => SetupOutcome::Failed,
                }
            },
            async {
                async_io::Timer::after(budget).await;
                SetupOutcome::TimedOut
            },
        )
        .await
    })
}

/// Cap on already-queued incoming bytes drained while observing
/// shutdown. A healthy peer's chatter SURVIVES our shutdown (probed:
/// 64KiB queued still reads back after `shutdown(Both)`), so EOF
/// requires draining first — bounded, never an unbounded drain. 1MiB
/// is ~1000x any legitimate daemon chatter (Hello, replies, signals
/// are KBs); past the cap the transport quarantines (safe direction).
const SHUTDOWN_DRAIN_CAP: usize = 1 << 20;

/// Observe our own private fd shutdown on a retained clone: after the
/// mandatory `shutdown(Both)` (issued by the worker on retirement, or by
/// the parent at the deadline), reads run dry to EOF. Drained-to-EOF
/// releases; a live unshut peer (`EAGAIN`), errors, or chatter past
/// [`SHUTDOWN_DRAIN_CAP`] quarantines. `MSG_DONTWAIT` keeps the check
/// nonblocking whatever mode the clone carries, so it never waits —
/// and the byte cap bounds the loop (every iteration consumes ≥1 byte
/// or exits).
fn shutdown_observed(observe: &UnixStream) -> bool {
    shutdown_observed_capped(observe, SHUTDOWN_DRAIN_CAP)
}

/// [`shutdown_observed`] with an explicit drain cap (the tiny-cap shape
/// is what the cap-exceeded regression pins; production always passes
/// [`SHUTDOWN_DRAIN_CAP`]).
pub(crate) fn shutdown_observed_capped(observe: &UnixStream, cap_bytes: usize) -> bool {
    let mut buf = [0u8; 4096];
    let mut drained = 0usize;
    loop {
        // `recv` (not `read`): no `Read` impl confusion on the raw socket.
        // SAFETY: buf points at 4096 live bytes; the fd is open.
        let got = unsafe {
            libc::recv(
                observe.as_raw_fd(),
                buf.as_mut_ptr() as *mut libc::c_void,
                buf.len(),
                libc::MSG_DONTWAIT,
            )
        };
        if got == 0 {
            return true;
        }
        if got > 0 {
            drained += got as usize;
            if drained > cap_bytes {
                return false;
            }
        } else {
            return false;
        }
    }
}

/// Join only an observed-finished worker, within `budget`; drops
/// (detaches) the handle when exit stays unobserved. Returns the join
/// result, or `None` when no exit was observed in budget. A channel
/// message (or sender drop) can precede actual thread exit, so joining
/// on the message alone could park past every budget.
fn join_bounded(
    worker: std::thread::JoinHandle<()>,
    budget: Duration,
) -> Option<std::thread::Result<()>> {
    // An unrepresentable end is an unbounded wait: never join — fail
    // closed at once (the handle detaches here, the caller quarantines).
    let end = Instant::now().checked_add(budget)?;
    loop {
        if worker.is_finished() {
            return Some(worker.join());
        }
        if Instant::now() >= end {
            return None;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Retained external owner: holds the single-flight claim plus the
/// worker's join handle until the worker's exit is OBSERVED and joined
/// with the private fd shutdown observed — or quarantine. Always exits
/// bounded: the completion wait runs to the absolute `retire_by`
/// (attempt deadline plus grace — never counted from spawn, so a
/// healthy long attempt never quarantines early), the join observes
/// `is_finished` within the remaining budget (capped by
/// [`REAPER_JOIN_GRACE`]), and an unobserved worker is detached (its
/// handle dropped) after quarantine — never an endlessly parked
/// thread. A worker that died without completing (panic/unwind drops
/// the sender) takes the bounded join and then quarantines at once,
/// without waiting out the retirement budget. zbus driver exit is NOT
/// observed and stays OUTSIDE the claim: retirement is exactly
/// application-worker-joined + fd-shutdown + application/zbus attempt
/// handles dropped (the parent's and reaper's raw clones briefly
/// outlive that), never a hardware/daemon-backend claim.
fn reap_worker(
    completion: mpsc::Receiver<()>,
    worker: std::thread::JoinHandle<()>,
    observe: UnixStream,
    guard: ClaimGuard,
    retire_by: Instant,
) {
    let remaining = || retire_by.saturating_duration_since(Instant::now());
    match completion.recv_timeout(remaining()) {
        Ok(()) => {
            // Completion precedes thread return by program order, but
            // the exit itself is still observed before joining. A clean
            // join plus observed shutdown releases; an unobserved exit,
            // a panicked worker, or a live transport quarantines (safe
            // direction).
            let joined = join_bounded(worker, remaining().min(REAPER_JOIN_GRACE));
            match joined {
                Some(Ok(())) if shutdown_observed(&observe) => {
                    guard.release_verified(RetirementProof::reaped())
                }
                _ => guard.quarantine(),
            }
        }
        // The worker died without completing: collect bounded, then
        // quarantine (death is unverified retirement).
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            let _ = join_bounded(worker, remaining().min(REAPER_JOIN_GRACE));
            guard.quarantine();
        }
        // Wedged past the retirement budget: quarantine and detach
        // (the handle drops here).
        Err(mpsc::RecvTimeoutError::Timeout) => {
            guard.quarantine();
        }
    }
}

/// Shutdown-before-drop guard: the FIRST field of [`WorkerKit`], so
/// it drops before the captured connection on spawn failure (struct
/// fields drop in declaration order — language guarantee). Armed at
/// build, carrying the worker's shutdown socket; the running worker
/// disarms it at start and takes the socket for its own explicit
/// retirement. On spawn failure the unstarted closure drops the kit:
/// the guard shuts the shared socket down FIRST, then the built
/// connection drops — the approved shutdown-before-drop order,
/// enforced by construction, not by the parent handler running later
/// (which additionally shuts its own retained clone down).
struct ShutdownFirst {
    sock: Option<UnixStream>,
    armed: bool,
}

impl ShutdownFirst {
    fn arm(sock: UnixStream) -> Self {
        Self {
            sock: Some(sock),
            armed: true,
        }
    }

    /// Disarm and take the shutdown socket: the running worker owns
    /// explicit retirement from here (this consumes the guard, so its
    /// `Drop` below never fires for a started worker).
    fn disarm(mut self) -> UnixStream {
        self.armed = false;
        self.sock
            .take()
            .expect("guard holds its socket until disarm")
    }
}

impl Drop for ShutdownFirst {
    fn drop(&mut self) {
        if self.armed {
            if let Some(sock) = &self.sock {
                let _ = sock.shutdown(std::net::Shutdown::Both);
            }
        }
    }
}

/// Everything the worker thread owns, as one container. Moves whole
/// into the worker closure (`move || WorkerKit::run(kit)`); [`run`](WorkerKit::run)
/// keeps it INTACT across panic/verify — never destructured into
/// locals, whose reverse-order drops would release the captured
/// connection before the guard — so every unwind path drops whole-kit
/// field order: [`ShutdownFirst`] (field 0) shuts the socket down
/// BEFORE the captured connection drops. On spawn failure the
/// unstarted closure drops the kit the same way, and the parent's
/// factored handler then shuts its own retained clone down and
/// releases only on observed shutdown+EOF. Nothing is ever recovered
/// out of a failed spawn. (`WorkerKit` itself has no `Drop`; fields
/// move out only inside `run`, at explicit retirement after
/// `verify_inner` returns.)
struct WorkerKit {
    shutdown_guard: ShutdownFirst,
    attempt: Fprintd,
    user: String,
    deadline: Instant,
    outcome_tx: mpsc::Sender<VerifyOutcome>,
    completion_tx: mpsc::Sender<()>,
}

impl WorkerKit {
    /// The worker body. Runs with the kit INTACT — the guard stays
    /// armed inside the whole container through `verify_inner`, and
    /// disarms only at explicit retirement below. Any unwind before
    /// that (including the synthetic test panic) drops whole-kit field
    /// order: shutdown first, connection second. Post-disarm steps
    /// cannot unwind back into an unguarded connection drop: they are
    /// infallible syscalls, an ignored close result, plain drops, and
    /// ignored send results.
    fn run(kit: Self) {
        #[cfg(test)]
        if TEST_PANIC_IN_WORKER.load(Ordering::Acquire) {
            // Fires with the kit whole and the guard armed: the
            // unwinding kit shuts down before its connection drops.
            panic!("synthetic worker panic (test injection)");
        }
        let outcome = kit.attempt.verify_inner(&kit.user, kit.deadline);
        // Started and verified: disarm the spawn-failure guard and take
        // the shutdown socket — explicit retirement owns it from here.
        let worker_shutdown = kit.shutdown_guard.disarm();
        // Explicit retirement, in order — never scope-drop order:
        // cleanup was attempted inside `verify_inner`; now shut the
        // private fd down FIRST (this bypasses the transport write lock
        // and releases any parked send) ...
        let _ = worker_shutdown.shutdown(std::net::Shutdown::Both);
        // ... close the private transport (uncontended: this worker is
        // its only sender, and its sends completed) ...
        let _ = kit.attempt.conn.clone().close();
        // ... drop every application/zbus attempt handle (the peer
        // observes EOF), report (a late result finds the waiter gone
        // and applies nowhere), and signal full exit LAST — the reaper
        // observes the exit, joins, then verifies the shutdown. NOTE: a
        // positive outcome sends BEFORE reaper retirement — future live
        // adoption must enforce retirement/callback identity separately.
        drop(kit.attempt);
        drop(kit.user);
        let _ = kit.outcome_tx.send(outcome);
        drop(kit.outcome_tx);
        let _ = kit.completion_tx.send(());
    }
}

/// Everything the reaper thread owns, as one container. Moves into the
/// reaper closure; on spawn failure the container drops with the
/// unstarted closure (the worker runs detached, the completion
/// receiver is gone) and the parent's factored handler quarantines —
/// retirement without the join handle is unverifiable, so denial is
/// the only safe direction.
struct ReaperKit {
    completion_rx: mpsc::Receiver<()>,
    worker: std::thread::JoinHandle<()>,
    observe: UnixStream,
}

/// Blocking fprintd client. Clone shares the connection; all methods
/// fail closed with no logging. Each verification opens its own private
/// connection on this client's bus (the system bus in production).
#[derive(Debug, Clone)]
pub struct Fprintd {
    conn: zbus::blocking::Connection,
    /// Bus this client targets: `None` is the system bus, `Some` is an
    /// explicit test-bus address (unit tests only).
    bus_address: Option<String>,
}

impl Fprintd {
    /// Whether fingerprint auth is pinned off via [`DISABLE_ENV`].
    pub fn disabled_by_env() -> bool {
        std::env::var_os(DISABLE_ENV).is_some()
    }

    /// Connect to the system bus, with the handshake bounded by
    /// [`DBUS_CALL_TIMEOUT`] and method calls bounded the same. `None`
    /// when disabled or unreachable: callers treat that as no reader
    /// and stay password-only.
    pub fn connect() -> Option<Self> {
        if Self::disabled_by_env() {
            return None;
        }
        let target = Self::system_target()?;
        let deadline = Instant::now().checked_add(DBUS_CALL_TIMEOUT)?;
        let ConnectOutcome::Connected(sock) = connect_unix_deadline(&target, deadline) else {
            return None;
        };
        let budget = deadline.saturating_duration_since(Instant::now());
        if budget.is_zero() {
            return None;
        }
        match build_attempt_connection(sock, DBUS_CALL_TIMEOUT, budget) {
            SetupOutcome::Built(conn) => Some(Self {
                conn: zbus::blocking::Connection::from(conn),
                bus_address: None,
            }),
            SetupOutcome::Failed | SetupOutcome::TimedOut => None,
        }
    }

    /// This client's bus as a connectable unix endpoint: the system bus
    /// selection (which reads `DBUS_SYSTEM_BUS_ADDRESS` and falls back
    /// to the well-known path — mirrored with zero mutation) or the
    /// explicit test address. Non-unix transports fail closed (no
    /// reader): the system bus is unix in production, and only unix
    /// gets the bounded manual connect.
    fn bus_unix_target(&self) -> Option<UnixTarget> {
        let address = match &self.bus_address {
            None => zbus::Address::system().ok()?,
            Some(addr) => addr.parse::<zbus::Address>().ok()?,
        };
        Self::unix_target_of(&address)
    }

    /// The system bus endpoint, without a client handle.
    fn system_target() -> Option<UnixTarget> {
        let address = zbus::Address::system().ok()?;
        Self::unix_target_of(&address)
    }

    fn unix_target_of(address: &zbus::Address) -> Option<UnixTarget> {
        match address.transport() {
            zbus::address::transport::Transport::Unix(unix) => match unix.path() {
                zbus::address::transport::UnixSocket::File(path) => {
                    Some(UnixTarget::Path(path.clone()))
                }
                #[cfg(target_os = "linux")]
                zbus::address::transport::UnixSocket::Abstract(name) => {
                    Some(UnixTarget::Abstract(name.as_os_str().as_bytes().to_vec()))
                }
                // Server-only Dir/TmpDir addresses (and future variants):
                // not connectable by a client — fail closed.
                _ => None,
            },
            _ => None,
        }
    }

    /// Client for an explicit bus address (unit tests only): the outer
    /// handle is the caller's, and attempts open their private
    /// connections on `address`, so the owned private-bus stub exercises
    /// the public [`verify`](Self::verify) path with no environment
    /// override and no process-global mutation.
    #[cfg(test)]
    pub(crate) fn for_test_address(address: &str, conn: zbus::blocking::Connection) -> Self {
        Self {
            conn,
            bus_address: Some(address.to_owned()),
        }
    }

    fn manager(&self) -> Option<zbus::blocking::Proxy<'_>> {
        zbus::blocking::Proxy::new(&self.conn, BUS_NAME, MANAGER_PATH, MANAGER_IFACE).ok()
    }

    /// Default reader, else the first enumerated one, else none.
    fn default_device(&self) -> Option<zbus::zvariant::OwnedObjectPath> {
        let manager = self.manager()?;
        if let Ok(path) =
            manager.call::<_, _, zbus::zvariant::OwnedObjectPath>("GetDefaultDevice", &())
        {
            return Some(path);
        }
        let devices: Vec<zbus::zvariant::OwnedObjectPath> = manager.call("GetDevices", &()).ok()?;
        devices.into_iter().next()
    }

    fn device_proxy<'a>(
        &'a self,
        path: &'a zbus::zvariant::OwnedObjectPath,
    ) -> Option<zbus::blocking::Proxy<'a>> {
        zbus::blocking::Proxy::new(&self.conn, BUS_NAME, path.as_str(), DEVICE_IFACE).ok()
    }

    /// Enrolled finger names for `user`; empty on any failure.
    pub fn enrolled_fingers(&self, user: &str) -> Vec<String> {
        let Some(path) = self.default_device() else {
            return Vec::new();
        };
        let Some(device) = self.device_proxy(&path) else {
            return Vec::new();
        };
        let name = effective_claim_name(user, process_user().as_deref());
        device
            .call::<_, _, Vec<String>>("ListEnrolledFingers", &(name,))
            .unwrap_or_default()
    }

    /// Truthful capability for `user`: reader presence plus enrollment.
    pub fn capability(&self, user: &str) -> Capability {
        let Some(path) = self.default_device() else {
            return Capability::unavailable();
        };
        let Some(device) = self.device_proxy(&path) else {
            return Capability::unavailable();
        };
        let name = effective_claim_name(user, process_user().as_deref());
        let enrolled = device
            .call::<_, _, Vec<String>>("ListEnrolledFingers", &(name,))
            .unwrap_or_default();
        Capability {
            available: true,
            enrolled_fingers: enrolled,
        }
    }

    /// Retire attempt resources when NO worker was ever spawned (the
    /// post-setup budget is exhausted, or worker spawn failed): shut the
    /// private fd down FIRST
    /// — the shutdown bypasses the transport write lock on every
    /// retirement path, including this one — then drop the connection
    /// and release the claim on the verified no-worker condition.
    /// Returns the outcome the caller reports.
    fn retire_unspawned(
        shutdown: &UnixStream,
        conn: zbus::blocking::Connection,
        guard: ClaimGuard,
        outcome: VerifyOutcome,
    ) -> VerifyOutcome {
        let _ = shutdown.shutdown(std::net::Shutdown::Both);
        drop(conn);
        guard.release_verified(RetirementProof::no_worker());
        outcome
    }

    /// Worker-spawn failure: the [`WorkerKit`] dropped whole with the
    /// unstarted closure — [`ShutdownFirst`] shut the socket down
    /// BEFORE the captured connection dropped (whole-kit field order).
    /// Then shut the parent's own retained clone down and release ONLY
    /// on the verified condition: the shutdown syscall succeeded AND
    /// the retained observe clone reads EOF. A failed shutdown or a
    /// live transport quarantines (safe direction) — a shutdown error
    /// is never ignored into a release. Denies either way.
    fn on_worker_spawn_failure(
        parent_shutdown: &UnixStream,
        observe: &UnixStream,
        guard: ClaimGuard,
    ) -> VerifyOutcome {
        let shut = parent_shutdown.shutdown(std::net::Shutdown::Both);
        if shut.is_ok() && shutdown_observed(observe) {
            guard.release_verified(RetirementProof::no_worker());
        } else {
            guard.quarantine();
        }
        VerifyOutcome::Error
    }

    /// Reaper-spawn failure: the worker runs detached with no completion
    /// receiver and no join handle, so retirement is unverifiable —
    /// wait the recomputed remainder, shut down-only, QUARANTINE, and
    /// return. Never joins (the parent never blocks past its budget);
    /// the detached worker still winds down on the shutdown and exits
    /// on its own. Quarantine is the only safe direction here.
    fn on_reaper_spawn_failure(
        outcome_rx: mpsc::Receiver<VerifyOutcome>,
        parent_shutdown: &UnixStream,
        guard: ClaimGuard,
        deadline: Instant,
    ) -> VerifyOutcome {
        let outcome =
            match outcome_rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(outcome) => outcome,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    let _ = parent_shutdown.shutdown(std::net::Shutdown::Both);
                    VerifyOutcome::TimedOut
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    let _ = parent_shutdown.shutdown(std::net::Shutdown::Both);
                    VerifyOutcome::Error
                }
            };
        guard.quarantine();
        outcome
    }

    /// One verification for `user`, bounded by `timeout`: claim the
    /// reader, start any-finger verification, and wait for the terminal
    /// `VerifyStatus`. Always stops and releases the claim, best
    /// effort. The blocking D-Bus work runs on a worker thread; a
    /// second attempt while one is in flight — including cleanup —
    /// fails closed at once ([`VerifyOutcome::Error`]). The attempt
    /// uses a private connection whose connect AND setup race the
    /// attempt deadline, so even a backlog-full listener or a
    /// blackholed handshake resolves instead of hanging the caller; at
    /// the deadline the caller shuts the private fd down (bypassing
    /// the transport write lock a saturated send may hold), which
    /// releases the worker's socket waits instead of lingering on a
    /// silent daemon (non-socket computation such as NSS lookup is not
    /// released this way — see the module docs). A late worker result
    /// can never apply: the caller is gone, so its send fails. A
    /// retained reaper thread owns the claim plus the worker's join
    /// handle until the verified retirement condition, then releases —
    /// or quarantines on any failure.
    pub fn verify(&self, user: &str, timeout: Duration) -> VerifyOutcome {
        // One claim at a time: contention fails closed at once, like
        // the daemon's already-claimed denial — before any bus work, so
        // the denial needs no daemon. The guard moves to the reaper at
        // spawn; paths below that never spawn release it directly.
        let Some(guard) = ClaimGuard::try_claim() else {
            return VerifyOutcome::Error;
        };
        // The absolute deadline starts BEFORE setup: every later
        // budget derives from the remainder, so setup can never push the
        // attempt past it. Checked: an overflowing budget fails closed,
        // never panics.
        let start = Instant::now();
        let Some(deadline) = start.checked_add(timeout) else {
            guard.release_verified(RetirementProof::no_worker());
            return VerifyOutcome::Error;
        };
        // A private connection per attempt, on this client's bus: the
        // deadline shutdown below must not disturb any other handle.
        let Some(target) = self.bus_unix_target() else {
            guard.release_verified(RetirementProof::no_worker());
            return VerifyOutcome::NoDevice;
        };
        let setup_budget = deadline.saturating_duration_since(Instant::now());
        if setup_budget.is_zero() {
            guard.release_verified(RetirementProof::no_worker());
            return VerifyOutcome::TimedOut;
        }
        // Bounded connect first (backlog-full retries within the
        // budget), then the raced build on the connected socket.
        let sock = match connect_unix_deadline(&target, deadline) {
            ConnectOutcome::Connected(sock) => sock,
            ConnectOutcome::Refused => {
                guard.release_verified(RetirementProof::no_worker());
                return VerifyOutcome::NoDevice;
            }
            ConnectOutcome::TimedOut => {
                guard.release_verified(RetirementProof::no_worker());
                return VerifyOutcome::TimedOut;
            }
        };
        // Retain fd-level handles BEFORE handing the socket over: the
        // parent's deadline shutdown, the worker's retirement shutdown,
        // and the reaper's EOF observation. `shutdown` on any clone
        // acts on the shared socket, bypassing the transport write lock.
        let parent_shutdown = match sock.try_clone() {
            Ok(clone) => clone,
            Err(_) => {
                // No zbus connection exists yet (no write lock, no
                // senders); shut down before dropping anyway — every
                // retirement path leads with the fd shutdown.
                let _ = sock.shutdown(std::net::Shutdown::Both);
                guard.release_verified(RetirementProof::no_worker());
                return VerifyOutcome::Error;
            }
        };
        let worker_shutdown = match sock.try_clone() {
            Ok(clone) => clone,
            Err(_) => {
                let _ = sock.shutdown(std::net::Shutdown::Both);
                guard.release_verified(RetirementProof::no_worker());
                return VerifyOutcome::Error;
            }
        };
        let reaper_observe = match sock.try_clone() {
            Ok(clone) => clone,
            Err(_) => {
                let _ = sock.shutdown(std::net::Shutdown::Both);
                guard.release_verified(RetirementProof::no_worker());
                return VerifyOutcome::Error;
            }
        };
        let build_budget = deadline.saturating_duration_since(Instant::now());
        if build_budget.is_zero() {
            // Setup consumed the whole budget: shut the fresh socket
            // down explicitly (ownership — no leak, no doomed worker)
            // and deny on the verified no-worker condition.
            let _ = parent_shutdown.shutdown(std::net::Shutdown::Both);
            drop(sock);
            guard.release_verified(RetirementProof::no_worker());
            return VerifyOutcome::TimedOut;
        }
        let call_bound = DBUS_CALL_TIMEOUT.min(build_budget);
        let attempt_conn = match build_attempt_connection(sock, call_bound, build_budget) {
            SetupOutcome::Built(conn) => zbus::blocking::Connection::from(conn),
            // The raced build dropped its socket half; shut our retained
            // clones' socket down before they drop — every retirement
            // path leads with the fd shutdown.
            SetupOutcome::Failed => {
                let _ = parent_shutdown.shutdown(std::net::Shutdown::Both);
                guard.release_verified(RetirementProof::no_worker());
                return VerifyOutcome::NoDevice;
            }
            SetupOutcome::TimedOut => {
                let _ = parent_shutdown.shutdown(std::net::Shutdown::Both);
                guard.release_verified(RetirementProof::no_worker());
                return VerifyOutcome::TimedOut;
            }
        };
        let attempt = Self {
            conn: attempt_conn,
            bus_address: self.bus_address.clone(),
        };
        // Nothing left after setup retires the fresh connection
        // explicitly instead of spawning a doomed worker: shutdown
        // first, then drop, then release — the no-thread case releases
        // ONLY after the actual owned private fd shutdown + handles
        // drop below.
        if deadline.saturating_duration_since(Instant::now()).is_zero() {
            return Self::retire_unspawned(
                &parent_shutdown,
                attempt.conn,
                guard,
                VerifyOutcome::TimedOut,
            );
        }
        // Absolute retirement deadline, checked: the reaper's budget
        // runs to the attempt deadline plus grace, never from spawn.
        let Some(retire_by) = deadline.checked_add(REAPER_GRACE) else {
            return Self::retire_unspawned(
                &parent_shutdown,
                attempt.conn,
                guard,
                VerifyOutcome::Error,
            );
        };
        let (outcome_tx, outcome_rx) = mpsc::channel();
        let (completion_tx, completion_rx) = mpsc::channel();
        let kit = WorkerKit {
            shutdown_guard: ShutdownFirst::arm(worker_shutdown),
            attempt,
            user: user.to_owned(),
            deadline,
            outcome_tx,
            completion_tx,
        };
        // Whole-kit capture: the closure holds the kit intact (see
        // `WorkerKit::run`) — never destructured into locals — so a
        // spawn failure drops whole-kit field order (guard first).
        let worker = std::thread::Builder::new()
            .name("tuna-fprintd".into())
            .spawn(move || WorkerKit::run(kit));
        let worker = match worker {
            Ok(handle) => handle,
            // The kit dropped whole with the unstarted closure: nothing
            // is recovered out of a failed spawn (see WorkerKit). The
            // reaper never spawned either, so its observe clone is still
            // owned here for the handler's EOF check.
            Err(_) => {
                return Self::on_worker_spawn_failure(&parent_shutdown, &reaper_observe, guard)
            }
        };
        // The retained reaper owns the claim plus the worker handle from
        // here on; the parent never touches the claim again.
        let reaper_kit = ReaperKit {
            completion_rx,
            worker,
            observe: reaper_observe,
        };
        let reaper = std::thread::Builder::new()
            .name("tuna-fprintd-reap".into())
            .spawn(move || {
                let ReaperKit {
                    completion_rx,
                    worker,
                    observe,
                } = reaper_kit;
                reap_worker(
                    completion_rx,
                    worker,
                    observe,
                    ClaimGuard::adopt(),
                    retire_by,
                );
            });
        if reaper.is_err() {
            // No reaper: quarantine (verification without the join
            // handle is impossible) — never an inline join, which would
            // block the parent past its budget.
            return Self::on_reaper_spawn_failure(outcome_rx, &parent_shutdown, guard, deadline);
        }
        guard.transfer();
        // Recompute the remainder AFTER both spawns: the receive below
        // must never wait past the absolute deadline. OS thread spawn
        // cost (~us) sits outside this budget by construction — it can
        // skew parent-return past the deadline by the spawn cost, but
        // never extends a socket wait (every worker-side wait keys off
        // the absolute deadline). Nothing left: shutdown-only + return.
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            let _ = parent_shutdown.shutdown(std::net::Shutdown::Both);
            return VerifyOutcome::TimedOut;
        }
        match outcome_rx.recv_timeout(remaining) {
            Ok(outcome) => outcome,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Deadline: fd shutdown ONLY, then return — never a
                // synchronous close or join (either could block behind
                // a saturated send). The shutdown releases the worker's
                // waits; the reaper owns release-or-quarantine.
                let _ = parent_shutdown.shutdown(std::net::Shutdown::Both);
                VerifyOutcome::TimedOut
            }
            // The worker died without answering: fail closed after the
            // same shutdown-only path (its handles died with it; the
            // reaper observes the death and quarantines).
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = parent_shutdown.shutdown(std::net::Shutdown::Both);
                VerifyOutcome::Error
            }
        }
    }

    /// One verification with [`VERIFY_TIMEOUT`].
    pub fn verify_user(&self, user: &str) -> VerifyOutcome {
        self.verify(user, VERIFY_TIMEOUT)
    }

    /// Claim, verify, stop, release. Every error fails closed; the
    /// release runs on all paths so a user never wedges the reader
    /// for the next attempt ("already claimed" is a denial, not a
    /// wedge, and clears when this call's connection drops). Every
    /// method call carries the bound fixed from the setup remainder
    /// (not recomputed per call); the signal wait ends on shutdown or
    /// peer loss with the attempt `deadline` applied to the verdict.
    /// Cleanup is always attempted before the worker retires the
    /// transport.
    fn verify_inner(&self, user: &str, deadline: Instant) -> VerifyOutcome {
        let Some(path) = self.default_device() else {
            return VerifyOutcome::NoDevice;
        };
        let Some(device) = self.device_proxy(&path) else {
            return VerifyOutcome::NoDevice;
        };
        let name = effective_claim_name(user, process_user().as_deref()).to_owned();
        // No enrolled prints: do not claim someone else's reader time.
        let enrolled: Vec<String> = device
            .call("ListEnrolledFingers", &(&name,))
            .unwrap_or_default();
        if enrolled.is_empty() {
            return VerifyOutcome::NotEnrolled;
        }
        if device.call::<_, _, ()>("Claim", &(&name,)).is_err() {
            return VerifyOutcome::Error;
        }
        let outcome = self.verify_claimed(&device, deadline);
        // Always stop and release; errors are best-effort cleanup. Each
        // call carries the setup-remainder-derived method bound, and
        // past the deadline the parent's shutdown fails them fast. The
        // transport itself retires in the worker after this returns:
        // cleanup is attempted BEFORE shutdown+close, never after.
        let _: Result<(), _> = device.call("VerifyStop", &());
        let _: Result<(), _> = device.call("Release", &());
        outcome
    }

    /// The claimed-device signal wait: subscribe before starting so no
    /// terminal signal is missed, then classify each `VerifyStatus`
    /// until one ends the attempt, retries run out, the iterator ends,
    /// or `deadline` passes. The wait runs on the worker thread itself,
    /// straight off the iterator — no forwarder to strand. The iterator
    /// ends when the daemon goes away or when our private connection
    /// is shut down, which the deadline does, so a silent daemon delays
    /// the worker past the deadline only by the shutdown propagation,
    /// never forever.
    fn verify_claimed(
        &self,
        device: &zbus::blocking::Proxy<'_>,
        deadline: Instant,
    ) -> VerifyOutcome {
        let mut signals = match device.receive_signal("VerifyStatus") {
            Ok(signals) => signals,
            Err(_) => return VerifyOutcome::Error,
        };
        if device
            .call::<_, _, ()>("VerifyStart", &(VERIFY_ANY,))
            .is_err()
        {
            return VerifyOutcome::Error;
        }
        drain_verify_signals(&mut signals, deadline)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::os::unix::io::AsRawFd;
    use std::process::{Child, Command, Stdio};
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering as TestOrdering},
        Arc, Mutex,
    };

    #[test]
    fn terminal_signals_end_the_attempt() {
        assert_eq!(
            classify_verify_status("verify-match", true),
            VerifySignal::Done(VerifyOutcome::Matched)
        );
        assert_eq!(
            classify_verify_status("verify-no-match", true),
            VerifySignal::Done(VerifyOutcome::NoMatch)
        );
        assert_eq!(
            classify_verify_status("verify-unknown-error", true),
            VerifySignal::Done(VerifyOutcome::Error)
        );
        assert_eq!(
            classify_verify_status("verify-disconnected", true),
            VerifySignal::Done(VerifyOutcome::Error)
        );
    }

    #[test]
    fn retry_reasons_continue_bounded() {
        for result in [
            "verify-retry-scan",
            "verify-swipe-too-short",
            "verify-finger-not-centered",
            "verify-remove-and-retry",
            "verify-no-match",
            // A match that is not finished is not a decision either.
            "verify-match",
        ] {
            assert_eq!(
                classify_verify_status(result, false),
                VerifySignal::Continue,
                "should continue: {result}"
            );
        }
    }

    #[test]
    fn nonterminal_match_is_not_a_match() {
        // Regression: the classifier used to accept `verify-match` even
        // when `done == false`. Per the Device contract `done` reports
        // "whether the verification finished and can be stopped", so a
        // nonterminal match continues the wait bounded, like a retry.
        assert_eq!(
            classify_verify_status("verify-match", false),
            VerifySignal::Continue
        );
        // And through the drain: a lone nonterminal match, then the
        // daemon going away, must fail closed — crucially, not match.
        let scripted = vec![verify_status_message("verify-match", false)];
        let outcome = drain_verify_signals(
            scripted.into_iter(),
            Instant::now() + Duration::from_secs(5),
        );
        assert_eq!(outcome, VerifyOutcome::Error);
    }

    #[test]
    fn unknown_results_fail_closed_when_terminal() {
        assert_eq!(
            classify_verify_status("verify-something-new", true),
            VerifySignal::Done(VerifyOutcome::Error)
        );
        assert_eq!(
            classify_verify_status("verify-something-new", false),
            VerifySignal::Continue
        );
    }

    #[test]
    fn own_user_claims_anonymously_other_users_explicitly() {
        assert_eq!(effective_claim_name("ada", Some("ada")), "");
        assert_eq!(effective_claim_name("ada", Some("grace")), "ada");
        assert_eq!(effective_claim_name("ada", None), "ada");
    }

    #[test]
    fn capability_is_offered_only_when_enrolled() {
        assert!(!Capability::unavailable().offered());
        assert!(!Capability {
            available: true,
            enrolled_fingers: Vec::new(),
        }
        .offered());
        assert!(Capability {
            available: true,
            enrolled_fingers: vec!["right-index-finger".to_string()],
        }
        .offered());
    }

    #[test]
    fn disabled_env_pins_everything_off() {
        let _guard = test_serialize_env().lock().unwrap();
        let prior = std::env::var_os(DISABLE_ENV);
        unsafe { std::env::set_var(DISABLE_ENV, "1") };
        assert!(Fprintd::disabled_by_env());
        assert!(Fprintd::connect().is_none());
        unsafe {
            match prior {
                Some(value) => std::env::set_var(DISABLE_ENV, value),
                None => std::env::remove_var(DISABLE_ENV),
            }
        }
    }

    #[test]
    fn no_daemon_fails_closed_without_panic() {
        // Serialize on the single-flight claim: this test verifies
        // against the live bus while stub tests may run alongside, and
        // either side could otherwise steal the claim and flip outcomes.
        let _serial = test_serialize_claim().lock().unwrap();
        let _watchdog = TestWatchdog::arm(120);
        if Fprintd::disabled_by_env() {
            return;
        }
        // Without fprintd on the system bus this is None (the expected
        // CI/container path); with a daemon but no reader, capability
        // reports unavailable and verify refuses promptly.
        if let Some(client) = Fprintd::connect() {
            let capability = client.capability("nobody-fprintd-probe");
            assert!(!capability.offered());
            assert!(matches!(
                client.verify("nobody-fprintd-probe", Duration::from_secs(5)),
                VerifyOutcome::NoDevice
                    | VerifyOutcome::NotEnrolled
                    | VerifyOutcome::Error
                    | VerifyOutcome::TimedOut
            ));
            // The reaper owns the release: observe it before unlocking.
            await_claim_free(Duration::from_secs(30));
        }
    }

    #[test]
    fn method_calls_are_bounded() {
        // The bus default waits forever; our connection must not.
        if let Some(client) = Fprintd::connect() {
            assert_eq!(client.conn.method_timeout(), Some(DBUS_CALL_TIMEOUT));
        }
    }

    /// Budget for a worker to exit once its signal wait is released.
    /// The pass path takes milliseconds; the budget only bounds failure.
    const JOIN_BUDGET: Duration = Duration::from_secs(5);
    /// Zero stub delay (shorthand for the scripted fixtures below).
    const D0: Duration = Duration::ZERO;

    /// Absolute retirement deadline `d` from now (checked: overflow
    /// fails the test loudly instead of wrapping silently).
    fn retire_in(d: Duration) -> Instant {
        Instant::now()
            .checked_add(d)
            .expect("retirement deadline in range")
    }

    /// Poll until the single-flight claim reads free or the budget runs
    /// out. Every claim-touching test calls this BEFORE releasing
    /// [`test_serialize_claim`]: the mutex may only unlock once the
    /// ACTUAL claim release is observed (never on a peer-disconnect
    /// proxy — `ListNames` absence can precede the token drop), so no
    /// live worker or reaper can exist when the next test starts.
    fn await_claim_free(budget: Duration) {
        let deadline = Instant::now() + budget;
        while ClaimGuard::test_state() != ClaimState::Free {
            assert!(
                Instant::now() < deadline,
                "claim must release within {budget:?}, stuck at {:?}",
                ClaimGuard::test_state()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Hard ceiling per socket/thread test, enforced CI-only: on breach
    /// the watchdog aborts the test binary loudly instead of hanging
    /// the runner. Generous (green tests take seconds); disarmed by
    /// drop at test end. A breach is a failure, never a skip.
    struct TestWatchdog {
        done: Arc<AtomicBool>,
        worker: Option<std::thread::JoinHandle<()>>,
    }

    impl TestWatchdog {
        fn arm(secs: u64) -> Self {
            let done = Arc::new(AtomicBool::new(false));
            let worker_done = done.clone();
            let worker = std::thread::Builder::new()
                .name("tuna-fprintd-test-watchdog".into())
                .spawn(move || {
                    let deadline = Instant::now() + Duration::from_secs(secs);
                    while !worker_done.load(TestOrdering::Acquire) {
                        if Instant::now() >= deadline {
                            eprintln!("WATCHDOG: test hung past {secs}s — aborting");
                            std::process::abort();
                        }
                        std::thread::sleep(Duration::from_millis(100));
                    }
                })
                .expect("watchdog spawns");
            Self {
                done,
                worker: Some(worker),
            }
        }
    }

    impl Drop for TestWatchdog {
        fn drop(&mut self) {
            self.done.store(true, TestOrdering::Release);
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }

    /// Queued-unsent bytes on `sock` (`SIOCOUTQ`): the test's fill
    /// evidence on a real attempt transport (measured through a clone
    /// the test retained — same socket, no stream corruption). `-1`
    /// when unmeasurable; callers treat that as no-evidence, never as
    /// fill.
    fn test_outq(sock: &UnixStream) -> i64 {
        let mut outq: libc::c_int = 0;
        // SAFETY: out-param points at a live c_int; the fd is open.
        let read = unsafe { libc::ioctl(sock.as_raw_fd(), libc::TIOCOUTQ, &mut outq) };
        if read == 0 {
            outq as i64
        } else {
            -1
        }
    }

    /// Effective send-buffer capacity of `sock`: `SO_SNDBUF` (which the
    /// kernel reports doubled) halved.
    fn test_snd_capacity(sock: &UnixStream) -> usize {
        let mut sndbuf: libc::c_int = 0;
        let mut optlen = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
        // SAFETY: out-params point at live locals of the right size.
        let read = unsafe {
            libc::getsockopt(
                sock.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_SNDBUF,
                &mut sndbuf as *mut libc::c_int as *mut libc::c_void,
                &mut optlen,
            )
        };
        assert_eq!(read, 0, "SO_SNDBUF must be readable");
        (sndbuf.max(0) / 2) as usize
    }

    /// Shrink `sock`'s send buffer toward the minimum so a bounded test
    /// payload demonstrably fills it. Returns the effective capacity
    /// afterwards (the kernel clamps; callers read, never assume).
    fn shrink_sndbuf(sock: &UnixStream) -> usize {
        let small: libc::c_int = 0;
        // SAFETY: in-param points at a live c_int of the right size.
        let set = unsafe {
            libc::setsockopt(
                sock.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_SNDBUF,
                &small as *const libc::c_int as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            )
        };
        assert_eq!(set, 0, "SO_SNDBUF must be settable");
        test_snd_capacity(sock)
    }

    /// One scripted `VerifyStatus` body as a real message, for drain tests.
    fn verify_status_message(result: &str, done: bool) -> zbus::message::Message {
        zbus::message::Message::signal(
            "/net/reactivated/Fprint/Device/0",
            "net.reactivated.Fprint.Device",
            "VerifyStatus",
        )
        .unwrap()
        .build(&(result.to_string(), done))
        .unwrap()
    }

    /// A `VerifyStatus`-shaped message with a malformed body.
    fn malformed_verify_status_message() -> zbus::message::Message {
        zbus::message::Message::signal(
            "/net/reactivated/Fprint/Device/0",
            "net.reactivated.Fprint.Device",
            "VerifyStatus",
        )
        .unwrap()
        .build(&"not-a-status-pair".to_string())
        .unwrap()
    }

    #[test]
    fn claim_contention_denies_without_bus_work() {
        let _serial = test_serialize_claim().lock().unwrap();
        let _watchdog = TestWatchdog::arm(120);
        // A held claim denies a second attempt at the public API at
        // once, without touching the bus (the claim check runs before
        // any connection work, so no daemon is needed for the denial —
        // and the stub proves none was touched).
        let (stub, _events) = FprintStub::spawn(vec!["right-index-finger".to_string()], D0, D0);
        let client = stub.client();
        let held = ClaimGuard::try_claim().expect("claim free in test");
        assert_eq!(ClaimGuard::test_state(), ClaimState::Held);
        assert!(ClaimGuard::try_claim().is_none());
        let start = Instant::now();
        assert_eq!(
            client.verify("ada", Duration::from_secs(5)),
            VerifyOutcome::Error
        );
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "contention must deny at once, not after bus work"
        );
        assert_eq!(
            stub.calls().claims.len(),
            0,
            "denied attempt must never reach the bus"
        );
        // Explicit verified release (no worker ever existed): the test
        // names the same no-worker condition production uses.
        held.release_verified(RetirementProof::no_worker());
        assert_eq!(ClaimGuard::test_state(), ClaimState::Free);
    }

    #[test]
    fn guard_drop_quarantines_and_denies() {
        let _serial = test_serialize_claim().lock().unwrap();
        let _watchdog = TestWatchdog::arm(120);
        // The retirement guard NEVER releases on drop: an armed guard
        // that falls out of scope (the unwind shape) quarantines.
        let (stub, _events) = FprintStub::spawn(vec!["right-index-finger".to_string()], D0, D0);
        let client = stub.client();
        drop(ClaimGuard::try_claim().expect("claim free in test"));
        assert_eq!(ClaimGuard::test_state(), ClaimState::Quarantined);
        // Quarantine denies at the public API, without bus work.
        assert!(ClaimGuard::try_claim().is_none());
        assert_eq!(
            client.verify("ada", Duration::from_secs(5)),
            VerifyOutcome::Error
        );
        assert_eq!(stub.calls().claims.len(), 0);
        // Quarantine is sticky: nothing re-arms it implicitly.
        assert_eq!(ClaimGuard::test_state(), ClaimState::Quarantined);
        ClaimGuard::test_reset();
        assert_eq!(ClaimGuard::test_state(), ClaimState::Free);
    }

    #[test]
    fn verified_release_frees_for_next_attempt() {
        let _serial = test_serialize_claim().lock().unwrap();
        let _watchdog = TestWatchdog::arm(120);
        // The ONLY release shape: explicit, with the verified proof. A
        // release always precedes the next claim (holding is exclusive),
        // and still denies contention while held.
        let held = ClaimGuard::try_claim().expect("claim free in test");
        assert!(ClaimGuard::try_claim().is_none());
        held.release_verified(RetirementProof::no_worker());
        let fresh = ClaimGuard::try_claim().expect("released claim reclaims");
        assert!(ClaimGuard::try_claim().is_none());
        fresh.release_verified(RetirementProof::no_worker());
        assert_eq!(ClaimGuard::test_state(), ClaimState::Free);
    }

    #[test]
    fn silent_wait_releases_the_worker_within_a_bounded_budget() {
        // A silent daemon: the signal iterator blocks with nothing
        // queued. Model it with a channel-backed iterator — the same
        // shape as the production signal iterator (blocking `next`,
        // `None` when the peer goes away or our connection is shut
        // down) — and prove the drain exits within a bounded budget
        // once the wait is released, instead of lingering on silence.
        // Pure drain shape: the claim lifecycle is covered separately.
        let (signal_tx, signal_rx) = mpsc::channel::<zbus::message::Message>();
        let (outcome_tx, outcome_rx) = mpsc::channel::<VerifyOutcome>();
        let (waiting_tx, waiting_rx) = mpsc::channel::<()>();
        let deadline = Instant::now() + Duration::from_secs(30);
        let worker = std::thread::Builder::new()
            .name("tuna-fprintd-test-silent".into())
            .spawn(move || {
                waiting_tx.send(()).unwrap();
                let outcome = drain_verify_signals(signal_rx.into_iter(), deadline);
                let _ = outcome_tx.send(outcome);
            })
            .unwrap();
        waiting_rx
            .recv_timeout(JOIN_BUDGET)
            .expect("worker starts waiting");
        // The wait is released (daemon gone, or our connection shut
        // down): the drain must exit within the budget — polled here.
        drop(signal_tx);
        let outcome = outcome_rx
            .recv_timeout(JOIN_BUDGET)
            .expect("worker must exit within the join budget");
        // Ended before the deadline: the daemon-gone shape fails closed.
        assert_eq!(outcome, VerifyOutcome::Error);
        worker.join().expect("worker joins");
    }

    #[test]
    fn wait_ending_past_the_deadline_times_out() {
        // The timeout shape: silence past the deadline, then the
        // shutdown lands (the parent shuts our connection down at the
        // deadline) and the iterator ends just after. The drain must
        // report the timeout and exit within the budget.
        let (signal_tx, signal_rx) = mpsc::channel::<zbus::message::Message>();
        let (outcome_tx, outcome_rx) = mpsc::channel::<VerifyOutcome>();
        let (waiting_tx, waiting_rx) = mpsc::channel::<()>();
        let deadline = Instant::now() + Duration::from_millis(50);
        let worker = std::thread::Builder::new()
            .name("tuna-fprintd-test-timeout".into())
            .spawn(move || {
                waiting_tx.send(()).unwrap();
                let outcome = drain_verify_signals(signal_rx.into_iter(), deadline);
                let _ = outcome_tx.send(outcome);
            })
            .unwrap();
        waiting_rx
            .recv_timeout(JOIN_BUDGET)
            .expect("worker starts waiting");
        while Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        drop(signal_tx);
        let outcome = outcome_rx
            .recv_timeout(JOIN_BUDGET)
            .expect("worker must exit within the join budget");
        assert_eq!(outcome, VerifyOutcome::TimedOut);
        worker.join().expect("worker joins");
    }

    /// Ordering-only channel test (no asynchronous callback, no
    /// runtime generation/correlation): pins that a late send finds its
    /// waiter gone. Must NOT qualify live async correlation.
    #[test]
    fn late_worker_result_cannot_apply_to_a_newer_generation() {
        // Locks the per-attempt handoff shape `verify` relies on: the
        // parent drops its outcome receiver at the deadline, so a worker
        // result sent afterwards fails to send and can never apply —
        // neither to its own (already resolved) attempt nor to any newer
        // generation, which always uses a fresh channel.
        let (stale_tx, stale_rx) = mpsc::channel::<VerifyOutcome>();
        drop(stale_rx);
        assert!(
            stale_tx.send(VerifyOutcome::Matched).is_err(),
            "late match must be undeliverable"
        );
        let (fresh_tx, fresh_rx) = mpsc::channel::<VerifyOutcome>();
        fresh_tx.send(VerifyOutcome::NoMatch).unwrap();
        assert_eq!(
            fresh_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            VerifyOutcome::NoMatch,
            "a newer generation resolves solely on its own channel"
        );
    }

    #[test]
    fn shutdown_releases_a_blocked_signal_wait() {
        // Live demonstration of the cancellation `verify` relies on: an
        // fd-level shutdown releases a thread blocked in a signal
        // iterator with `None`. Needs any message bus (the bus itself is
        // the peer — never fprintd hardware); without one this proves
        // nothing and returns, like the other bus-dependent tests here.
        let conn = zbus::blocking::Connection::system()
            .or_else(|_| zbus::blocking::Connection::session())
            .ok();
        let Some(conn) = conn else { return };
        let proxy = match zbus::blocking::Proxy::new(
            &conn,
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
        ) {
            Ok(proxy) => proxy,
            Err(_) => return,
        };
        let mut signals = match proxy.receive_signal("TunaNoSuchSignalForShutdownTest") {
            Ok(signals) => signals,
            Err(_) => return,
        };
        // A member no one ever emits: the wait below blocks until the
        // shutdown lands. The shutdown runs aside on a clone so the
        // production shape is exact (one thread blocked in `next`,
        // another shutting the shared socket down).
        let shutdown = conn.clone();
        let grace = Duration::from_millis(100);
        let closer = std::thread::Builder::new()
            .name("tuna-fprintd-test-shutdown".into())
            .spawn(move || {
                std::thread::sleep(grace);
                let _ = shutdown.close();
            })
            .unwrap();
        let start = Instant::now();
        let item = signals.next();
        let elapsed = start.elapsed();
        closer.join().unwrap();
        assert!(item.is_none(), "shut connection ends the iterator");
        assert!(
            elapsed < JOIN_BUDGET,
            "released within the join budget after shutdown"
        );
        assert!(
            elapsed >= grace / 2,
            "control: the wait actually blocked before the shutdown"
        );
    }

    #[test]
    fn expired_deadline_times_out_without_consuming() {
        let mut scripted = vec![verify_status_message("verify-match", true)].into_iter();
        let outcome = drain_verify_signals(scripted.by_ref(), Instant::now());
        assert_eq!(outcome, VerifyOutcome::TimedOut);
        assert_eq!(
            scripted.len(),
            1,
            "an expired deadline must not consume signals"
        );
    }

    #[test]
    fn terminal_signal_through_the_drain_matches() {
        let scripted = vec![verify_status_message("verify-match", true)];
        let outcome = drain_verify_signals(
            scripted.into_iter(),
            Instant::now() + Duration::from_secs(5),
        );
        assert_eq!(outcome, VerifyOutcome::Matched);
    }

    #[test]
    fn retry_then_match_through_the_drain_matches() {
        let scripted = vec![
            verify_status_message("verify-retry-scan", false),
            verify_status_message("verify-match", true),
        ];
        let outcome = drain_verify_signals(
            scripted.into_iter(),
            Instant::now() + Duration::from_secs(5),
        );
        assert_eq!(outcome, VerifyOutcome::Matched);
    }

    #[test]
    fn endless_retries_through_the_drain_deny_bounded() {
        let scripted: Vec<_> = (0..=MAX_VERIFY_RETRIES)
            .map(|_| verify_status_message("verify-retry-scan", false))
            .collect();
        let outcome = drain_verify_signals(
            scripted.into_iter(),
            Instant::now() + Duration::from_secs(5),
        );
        assert_eq!(outcome, VerifyOutcome::Error);
    }

    #[test]
    fn malformed_signals_through_the_drain_deny_bounded() {
        let scripted: Vec<_> = (0..=MAX_VERIFY_RETRIES)
            .map(|_| malformed_verify_status_message())
            .collect();
        let outcome = drain_verify_signals(
            scripted.into_iter(),
            Instant::now() + Duration::from_secs(5),
        );
        assert_eq!(outcome, VerifyOutcome::Error);
    }

    #[test]
    fn daemon_gone_mid_attempt_is_an_error() {
        let scripted: Vec<zbus::message::Message> = Vec::new();
        let outcome = drain_verify_signals(
            scripted.into_iter(),
            Instant::now() + Duration::from_secs(5),
        );
        assert_eq!(outcome, VerifyOutcome::Error);
    }

    /// Device path the scripted stub serves on the private bus.
    const STUB_DEVICE_PATH: &str = "/net/reactivated/Fprint/Device/0";
    /// Server GUID the Hello-stall stub answers with. The client address
    /// carries no GUID, so any well-formed server GUID is accepted.
    const STALL_GUID: &str = "00112233445566778899aabbccddeeff";

    /// Owned private bus for one test (sleep.rs shape): a dbus-daemon
    /// child whose address the stub and the client use explicitly.
    /// Killed on drop.
    struct TestBus {
        child: Child,
        address: String,
    }

    impl TestBus {
        fn spawn() -> Self {
            let mut child = Command::new("dbus-daemon")
                .args(["--session", "--nofork", "--print-address=1"])
                .stdout(Stdio::piped())
                .spawn()
                .expect("private test bus starts");
            let mut address = String::new();
            BufReader::new(child.stdout.take().expect("bus prints its address"))
                .read_line(&mut address)
                .expect("bus address line readable");
            Self {
                child,
                address: address.trim().to_owned(),
            }
        }

        /// One more peer on this bus (stub service, client outer handle).
        fn connect(&self) -> zbus::blocking::Connection {
            zbus::blocking::connection::Builder::address(self.address.as_str())
                .expect("test address parses")
                .method_timeout(Duration::from_secs(2))
                .build()
                .expect("test peer connects")
        }
    }

    impl Drop for TestBus {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    /// Hold `sock` open until `done` flips: read with a short timeout
    /// and discard whatever arrives. The peer never sees EOF while held.
    fn hold_until_done(sock: &mut std::os::unix::net::UnixStream, done: &AtomicBool) {
        let mut byte = [0u8; 1];
        while !done.load(Ordering::Acquire) {
            match sock.read(&mut byte) {
                Ok(_) => {}
                Err(error)
                    if error.kind() == std::io::ErrorKind::TimedOut
                        || error.kind() == std::io::ErrorKind::WouldBlock =>
                {
                    std::thread::sleep(Duration::from_millis(10));
                }
                // The peer went away first: nothing left to hold.
                Err(_) => break,
            }
        }
    }

    /// Accepted-but-silent peer: holds one accepted socket open (reading
    /// nothing, writing nothing) until dropped. Models a setup-auth
    /// blackhole: the client's handshake never completes.
    struct Blackhole {
        dir: tempfile::TempDir,
        done: Arc<AtomicBool>,
        worker: Option<std::thread::JoinHandle<()>>,
    }

    impl Blackhole {
        fn spawn() -> Self {
            use std::os::unix::net::UnixListener;
            let dir = tempfile::tempdir().expect("blackhole dir");
            let path = dir.path().join("blackhole.sock");
            let listener = UnixListener::bind(&path).expect("blackhole binds");
            listener
                .set_nonblocking(true)
                .expect("blackhole nonblocking");
            let done = Arc::new(AtomicBool::new(false));
            let worker_done = done.clone();
            let worker = std::thread::Builder::new()
                .name("tuna-fprintd-test-blackhole".into())
                .spawn(move || {
                    while !worker_done.load(Ordering::Acquire) {
                        match listener.accept() {
                            Ok((mut sock, _)) => {
                                sock.set_read_timeout(Some(Duration::from_millis(100)))
                                    .expect("blackhole read timeout");
                                hold_until_done(&mut sock, &worker_done);
                                break;
                            }
                            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                                std::thread::sleep(Duration::from_millis(10));
                            }
                            Err(error) => panic!("blackhole accept failed: {error}"),
                        }
                    }
                })
                .expect("blackhole spawns");
            Self {
                dir,
                done,
                worker: Some(worker),
            }
        }

        /// Client address of the blackhole.
        fn address(&self) -> String {
            format!(
                "unix:path={}",
                self.dir.path().join("blackhole.sock").display()
            )
        }
    }

    impl Drop for Blackhole {
        fn drop(&mut self) {
            self.done.store(true, Ordering::Release);
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }

    /// Authenticated-but-stalled peer: completes the SASL handshake and
    /// the file-descriptor negotiation, then never answers the `Hello`
    /// method call. Models a post-auth setup stall (content unchecked —
    /// only the line sequence matters).
    struct HelloStall {
        dir: tempfile::TempDir,
        done: Arc<AtomicBool>,
        worker: Option<std::thread::JoinHandle<()>>,
    }

    impl HelloStall {
        fn spawn() -> Self {
            use std::os::unix::net::UnixListener;
            let dir = tempfile::tempdir().expect("stall dir");
            let path = dir.path().join("stall.sock");
            let listener = UnixListener::bind(&path).expect("stall binds");
            listener.set_nonblocking(true).expect("stall nonblocking");
            let done = Arc::new(AtomicBool::new(false));
            let worker_done = done.clone();
            let worker = std::thread::Builder::new()
                .name("tuna-fprintd-test-stall".into())
                .spawn(move || {
                    while !worker_done.load(Ordering::Acquire) {
                        match listener.accept() {
                            Ok((sock, _)) => {
                                sock.set_read_timeout(Some(Duration::from_secs(10)))
                                    .expect("stall read timeout");
                                Self::stall(sock, &worker_done);
                                break;
                            }
                            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                                std::thread::sleep(Duration::from_millis(10));
                            }
                            Err(error) => panic!("stall accept failed: {error}"),
                        }
                    }
                })
                .expect("stall spawns");
            Self {
                dir,
                done,
                worker: Some(worker),
            }
        }

        /// Client address of the stall.
        fn address(&self) -> String {
            format!("unix:path={}", self.dir.path().join("stall.sock").display())
        }

        /// Run the server side of the handshake on `sock`, then hold past
        /// the client's setup race without answering `Hello`.
        fn stall(mut sock: std::os::unix::net::UnixStream, done: &AtomicBool) {
            let mut reader = BufReader::new(sock.try_clone().expect("stall clones its socket"));
            let mut line = String::new();
            // The client's AUTH (leading NUL byte included): accept anything.
            reader.read_line(&mut line).expect("stall reads AUTH");
            sock.write_all(format!("OK {STALL_GUID}\r\n").as_bytes())
                .expect("stall replies OK");
            line.clear();
            reader
                .read_line(&mut line)
                .expect("stall reads fd negotiation");
            sock.write_all(b"AGREE_UNIX_FD\r\n").expect("stall agrees");
            line.clear();
            reader.read_line(&mut line).expect("stall reads BEGIN");
            // Authenticated; the pipelined Hello sits unread. Hold the
            // socket open (no Hello reply, no EOF) until released.
            hold_until_done(&mut sock, done);
        }
    }

    impl Drop for HelloStall {
        fn drop(&mut self) {
            self.done.store(true, Ordering::Release);
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }

    /// What the scripted stub saw. Method calls (attempted) and handler
    /// returns (reply sent) are recorded SEPARATELY: after a client-side
    /// close a delayed handler may still return, and that return must
    /// never read as successful cleanup.
    #[derive(Default)]
    struct StubCalls {
        /// Usernames seen by Claim, in order.
        claims: Vec<String>,
        /// Caller unique names seen by Claim, in order (peer identities).
        claim_callers: Vec<String>,
        /// Fingers seen by VerifyStart, in order.
        verify_starts: Vec<String>,
        /// VerifyStop handler entries (attempted).
        stop_calls: usize,
        /// VerifyStop handler returns (reply sent).
        stop_replies: usize,
        /// Release handler entries (attempted).
        release_calls: usize,
        /// Release handler returns (reply sent).
        release_replies: usize,
    }

    struct StubShared {
        calls: Mutex<StubCalls>,
        enrolled: Vec<String>,
        stop_delay: Duration,
        release_delay: Duration,
        start_tx: mpsc::Sender<()>,
        stop_tx: mpsc::Sender<()>,
        release_tx: mpsc::Sender<()>,
    }

    struct StubManager;

    // Member names below are the fprintd D-Bus spelling (the macro maps
    // snake_case to PascalCase); the tests prove the wiring end to end.
    #[zbus::interface(name = "net.reactivated.Fprint.Manager")]
    impl StubManager {
        fn get_default_device(&self) -> zbus::zvariant::OwnedObjectPath {
            zbus::zvariant::OwnedObjectPath::try_from(STUB_DEVICE_PATH)
                .expect("stub device path valid")
        }

        fn get_devices(&self) -> Vec<zbus::zvariant::OwnedObjectPath> {
            vec![self.get_default_device()]
        }
    }

    struct StubDevice {
        shared: Arc<StubShared>,
    }

    #[zbus::interface(name = "net.reactivated.Fprint.Device")]
    impl StubDevice {
        fn list_enrolled_fingers(&self, _username: String) -> Vec<String> {
            self.shared.enrolled.clone()
        }

        fn claim(&self, username: String, #[zbus(header)] header: zbus::message::Header<'_>) {
            let mut calls = self.shared.calls.lock().expect("stub calls lockable");
            calls.claims.push(username);
            calls.claim_callers.push(
                header
                    .sender()
                    .map(|name| name.as_str().to_owned())
                    .unwrap_or_default(),
            );
        }

        fn verify_start(&self, finger: String) {
            {
                let mut calls = self.shared.calls.lock().expect("stub calls lockable");
                calls.verify_starts.push(finger);
            }
            let _ = self.shared.start_tx.send(());
        }

        async fn verify_stop(&self) {
            {
                let mut calls = self.shared.calls.lock().expect("stub calls lockable");
                calls.stop_calls += 1;
            }
            let _ = self.shared.stop_tx.send(());
            async_io::Timer::after(self.shared.stop_delay).await;
            self.shared
                .calls
                .lock()
                .expect("stub calls lockable")
                .stop_replies += 1;
        }

        async fn release(&self) {
            {
                let mut calls = self.shared.calls.lock().expect("stub calls lockable");
                calls.release_calls += 1;
            }
            let _ = self.shared.release_tx.send(());
            async_io::Timer::after(self.shared.release_delay).await;
            self.shared
                .calls
                .lock()
                .expect("stub calls lockable")
                .release_replies += 1;
        }
    }

    /// One scripted fprintd on an owned private bus: daemon plus a stub
    /// service owning [`BUS_NAME`]. Receivers live in [`StubEvents`] so
    /// the emitter thread can take them (this fixture cannot partially
    /// move: it owns a child process).
    struct FprintStub {
        bus: TestBus,
        stub_conn: zbus::blocking::Connection,
        shared: Arc<StubShared>,
    }

    struct StubEvents {
        start_rx: mpsc::Receiver<()>,
        stop_rx: mpsc::Receiver<()>,
        release_rx: mpsc::Receiver<()>,
    }

    impl FprintStub {
        fn spawn(
            enrolled: Vec<String>,
            stop_delay: Duration,
            release_delay: Duration,
        ) -> (Self, StubEvents) {
            let bus = TestBus::spawn();
            let stub_conn = bus.connect();
            stub_conn
                .request_name(BUS_NAME)
                .expect("stub owns the fprintd name");
            let (start_tx, start_rx) = mpsc::channel();
            let (stop_tx, stop_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel();
            let shared = Arc::new(StubShared {
                calls: Mutex::new(StubCalls::default()),
                enrolled,
                stop_delay,
                release_delay,
                start_tx,
                stop_tx,
                release_tx,
            });
            assert!(
                stub_conn
                    .object_server()
                    .at(MANAGER_PATH, StubManager)
                    .expect("manager serves"),
                "manager serves"
            );
            assert!(
                stub_conn
                    .object_server()
                    .at(
                        STUB_DEVICE_PATH,
                        StubDevice {
                            shared: shared.clone()
                        }
                    )
                    .expect("device serves"),
                "device serves"
            );
            (
                Self {
                    bus,
                    stub_conn,
                    shared,
                },
                StubEvents {
                    start_rx,
                    stop_rx,
                    release_rx,
                },
            )
        }

        /// Client for the PUBLIC path: outer handle on the private bus,
        /// attempts on the same bus. No environment override anywhere.
        fn client(&self) -> Fprintd {
            Fprintd::for_test_address(&self.bus.address, self.bus.connect())
        }

        fn calls(&self) -> std::sync::MutexGuard<'_, StubCalls> {
            self.shared.calls.lock().expect("stub calls lockable")
        }

        /// Unique names currently on the private bus (the test's view of
        /// peer connect/disconnect — never a hardware claim).
        fn list_names(&self) -> Vec<String> {
            let proxy = zbus::blocking::Proxy::new(
                &self.stub_conn,
                "org.freedesktop.DBus",
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
            )
            .expect("dbus proxy builds");
            proxy
                .call::<_, _, Vec<String>>("ListNames", &())
                .expect("ListNames answers")
        }
    }

    /// Background emitter: per VerifyStart notification, emit one
    /// scripted VerifyStatus sequence from the stub (owner sender).
    /// Exits after `rounds` notifications (bounded wait each); the test
    /// joins it at the end.
    fn spawn_emitter(
        conn: zbus::blocking::Connection,
        starts: mpsc::Receiver<()>,
        script: Vec<(String, bool)>,
        rounds: usize,
    ) -> std::thread::JoinHandle<()> {
        std::thread::Builder::new()
            .name("tuna-fprintd-test-emit".into())
            .spawn(move || {
                for _ in 0..rounds {
                    if starts.recv_timeout(Duration::from_secs(10)).is_err() {
                        break;
                    }
                    for (result, done) in &script {
                        conn.emit_signal(
                            None::<()>,
                            STUB_DEVICE_PATH,
                            DEVICE_IFACE,
                            "VerifyStatus",
                            &(result.clone(), *done),
                        )
                        .expect("stub emits VerifyStatus");
                    }
                }
            })
            .expect("emitter spawns")
    }

    /// Poll `cond` until true or the budget runs out (10ms cadence).
    fn await_stub_condition(budget: Duration, what: &str, mut cond: impl FnMut() -> bool) {
        let deadline = Instant::now() + budget;
        while !cond() {
            assert!(
                Instant::now() < deadline,
                "stub condition unmet within {budget:?}: {what}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Poll until `name` vanishes from the bus (peer disconnect observed
    /// by the test) or the budget runs out.
    fn await_name_gone(stub: &FprintStub, name: &str, budget: Duration) {
        let deadline = Instant::now() + budget;
        while stub
            .list_names()
            .iter()
            .any(|listed| listed.as_str() == name)
        {
            assert!(
                Instant::now() < deadline,
                "peer {name} must disconnect within {budget:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// (a) Setup-auth blackhole: the peer accepts and goes silent, so the
    /// handshake never completes. The PUBLIC prelude must resolve at the
    /// attempt deadline (TimedOut) — not hang, and not fail fast as
    /// no-reader.
    #[test]
    fn setup_auth_blackhole_resolves_at_the_deadline() {
        let _serial = test_serialize_claim().lock().unwrap();
        let _watchdog = TestWatchdog::arm(120);
        let blackhole = Blackhole::spawn();
        let address = blackhole.address();
        let companion = TestBus::spawn();
        let client = Fprintd::for_test_address(&address, companion.connect());
        let timeout = Duration::from_millis(500);
        let start = Instant::now();
        let outcome = client.verify("ada", timeout);
        let elapsed = start.elapsed();
        assert_eq!(outcome, VerifyOutcome::TimedOut);
        assert!(
            elapsed >= timeout.saturating_sub(Duration::from_millis(100)),
            "setup race must wait out the budget, took {elapsed:?}"
        );
        assert!(
            elapsed < timeout + Duration::from_secs(5),
            "setup race must not linger past the deadline, took {elapsed:?}"
        );
        await_claim_free(Duration::from_secs(10));
    }

    /// (b) Authenticated Hello stall: the peer completes the handshake
    /// and the fd negotiation, then never answers Hello. Like (a), the
    /// PUBLIC prelude must resolve at the deadline.
    #[test]
    fn authenticated_hello_stall_resolves_at_the_deadline() {
        let _serial = test_serialize_claim().lock().unwrap();
        let _watchdog = TestWatchdog::arm(120);
        let stall = HelloStall::spawn();
        let address = stall.address();
        let companion = TestBus::spawn();
        let client = Fprintd::for_test_address(&address, companion.connect());
        let timeout = Duration::from_millis(500);
        let start = Instant::now();
        let outcome = client.verify("ada", timeout);
        let elapsed = start.elapsed();
        assert_eq!(outcome, VerifyOutcome::TimedOut);
        assert!(
            elapsed >= timeout.saturating_sub(Duration::from_millis(100)),
            "setup race must wait out the budget, took {elapsed:?}"
        );
        assert!(
            elapsed < timeout + Duration::from_secs(5),
            "setup race must not linger past the deadline, took {elapsed:?}"
        );
        await_claim_free(Duration::from_secs(10));
    }

    /// (c) Silence: the stub answers every method call but emits no
    /// VerifyStatus. The PUBLIC attempt must time out at the deadline —
    /// and the stub must have seen the attempt (Claim + VerifyStart),
    /// proving the private-bus path ran end to end rather than failing
    /// vacuously.
    #[test]
    fn silent_stub_times_out_at_the_deadline() {
        let _serial = test_serialize_claim().lock().unwrap();
        let _watchdog = TestWatchdog::arm(120);
        let (stub, _events) = FprintStub::spawn(
            vec!["right-index-finger".to_string()],
            Duration::ZERO,
            Duration::ZERO,
        );
        let client = stub.client();
        let timeout = Duration::from_millis(500);
        let start = Instant::now();
        let outcome = client.verify("ada", timeout);
        let elapsed = start.elapsed();
        assert_eq!(outcome, VerifyOutcome::TimedOut);
        assert!(
            elapsed >= timeout.saturating_sub(Duration::from_millis(100)),
            "silent wait must last the budget, took {elapsed:?}"
        );
        assert!(
            elapsed < timeout + Duration::from_secs(5),
            "silent wait must not linger past the deadline, took {elapsed:?}"
        );
        let calls = stub.calls();
        assert_eq!(calls.claims.len(), 1, "the stub must see the attempt");
        assert_eq!(calls.verify_starts, vec![VERIFY_ANY.to_string()]);
        drop(calls);
        await_claim_free(Duration::from_secs(30));
    }

    /// (d) Delayed cleanup, in budget: Stop/Release replies arrive late
    /// but before the deadline. Attempted and replied are recorded
    /// separately and must AGREE here (1/1), and the worker must have
    /// awaited both replies (elapsed covers both delays) — with no
    /// close-on-timeout anywhere (a closed socket cannot reply).
    #[test]
    fn delayed_cleanup_within_budget_attempts_and_replies() {
        let _serial = test_serialize_claim().lock().unwrap();
        let _watchdog = TestWatchdog::arm(120);
        let stop_delay = Duration::from_millis(400);
        let release_delay = Duration::from_millis(400);
        let (stub, events) = FprintStub::spawn(
            vec!["right-index-finger".to_string()],
            stop_delay,
            release_delay,
        );
        let emitter = spawn_emitter(
            stub.stub_conn.clone(),
            events.start_rx,
            vec![("verify-match".to_string(), true)],
            1,
        );
        let client = stub.client();
        let start = Instant::now();
        let outcome = client.verify("ada", Duration::from_secs(10));
        let elapsed = start.elapsed();
        emitter.join().expect("emitter joins");
        assert_eq!(outcome, VerifyOutcome::Matched);
        assert!(
            elapsed >= (stop_delay + release_delay).saturating_sub(Duration::from_millis(200)),
            "worker must await both delayed replies, took {elapsed:?}"
        );
        let calls = stub.calls();
        assert_eq!(calls.stop_calls, 1, "VerifyStop attempted");
        assert_eq!(calls.stop_replies, 1, "VerifyStop reply sent");
        assert_eq!(calls.release_calls, 1, "Release attempted");
        assert_eq!(calls.release_replies, 1, "Release reply sent");
        drop(calls);
        await_claim_free(Duration::from_secs(30));
    }

    /// (d2) Delayed cleanup, past the deadline: the Stop reply arrives
    /// long after the attempt resolved. The stub saw the Stop call while
    /// the socket was open (attempted), the peer went away at the
    /// deadline (close-on-timeout, observed via ListNames), and the
    /// handler's late return is recorded separately — it must NEVER read
    /// as successful cleanup (the reply postdates the attempt end, which
    /// the elapsed bound proves).
    #[test]
    fn cleanup_past_the_deadline_is_attempted_but_never_successful() {
        let _serial = test_serialize_claim().lock().unwrap();
        let _watchdog = TestWatchdog::arm(120);
        let stop_delay = Duration::from_millis(1500);
        let (stub, events) = FprintStub::spawn(
            vec!["right-index-finger".to_string()],
            stop_delay,
            Duration::ZERO,
        );
        let emitter = spawn_emitter(
            stub.stub_conn.clone(),
            events.start_rx,
            vec![("verify-match".to_string(), true)],
            1,
        );
        let stop_rx = events.stop_rx;
        let client = stub.client();
        let timeout = Duration::from_millis(400);
        let start = Instant::now();
        let outcome = client.verify("ada", timeout);
        let elapsed = start.elapsed();
        emitter.join().expect("emitter joins");
        assert_eq!(outcome, VerifyOutcome::TimedOut);
        assert!(
            elapsed >= timeout.saturating_sub(Duration::from_millis(100)),
            "attempt must last the budget, took {elapsed:?}"
        );
        assert!(
            elapsed < stop_delay,
            "attempt must resolve before the delayed reply can exist, took {elapsed:?}"
        );
        // Stop was attempted (the stub saw the call on the open socket).
        stop_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("Stop attempted");
        // ... and the peer went away at the deadline (close-on-timeout).
        let caller = stub
            .calls()
            .claim_callers
            .first()
            .expect("claim recorded")
            .clone();
        await_name_gone(&stub, &caller, Duration::from_secs(5));
        // The delayed handler still returns afterwards; recorded
        // separately, and deliberately NOT asserted as success.
        await_stub_condition(Duration::from_secs(5), "Stop handler returned", || {
            stub.calls().stop_replies == 1
        });
        let calls = stub.calls();
        assert_eq!(calls.stop_calls, 1);
        assert_eq!(calls.stop_replies, 1);
        drop(calls);
        await_claim_free(Duration::from_secs(30));
    }

    /// (e) Terminal match, done flag set: a nonterminal match is not a
    /// decision (the wait continues), and the terminal one matches.
    #[test]
    fn nonterminal_match_then_terminal_match_matches() {
        let _serial = test_serialize_claim().lock().unwrap();
        let _watchdog = TestWatchdog::arm(120);
        let (stub, events) = FprintStub::spawn(
            vec!["right-index-finger".to_string()],
            Duration::ZERO,
            Duration::ZERO,
        );
        let emitter = spawn_emitter(
            stub.stub_conn.clone(),
            events.start_rx,
            vec![
                ("verify-match".to_string(), false),
                ("verify-match".to_string(), true),
            ],
            1,
        );
        let client = stub.client();
        let outcome = client.verify("ada", Duration::from_secs(5));
        emitter.join().expect("emitter joins");
        assert_eq!(outcome, VerifyOutcome::Matched);
        assert_eq!(stub.calls().claims.len(), 1);
        await_claim_free(Duration::from_secs(30));
    }

    /// (e2) A lone nonterminal match never matches: with nothing after
    /// it, the wait runs out instead of deciding.
    #[test]
    fn lone_nonterminal_match_never_matches() {
        let _serial = test_serialize_claim().lock().unwrap();
        let _watchdog = TestWatchdog::arm(120);
        let (stub, events) = FprintStub::spawn(
            vec!["right-index-finger".to_string()],
            Duration::ZERO,
            Duration::ZERO,
        );
        let emitter = spawn_emitter(
            stub.stub_conn.clone(),
            events.start_rx,
            vec![("verify-match".to_string(), false)],
            1,
        );
        let client = stub.client();
        let timeout = Duration::from_millis(500);
        let start = Instant::now();
        let outcome = client.verify("ada", timeout);
        let elapsed = start.elapsed();
        emitter.join().expect("emitter joins");
        assert_eq!(outcome, VerifyOutcome::TimedOut);
        assert!(
            elapsed >= timeout.saturating_sub(Duration::from_millis(100)),
            "nonterminal wait must last the budget, took {elapsed:?}"
        );
        assert_eq!(stub.calls().claims.len(), 1);
        await_claim_free(Duration::from_secs(30));
    }

    /// (f) Claim ownership through cleanup until verified release: while
    /// the first attempt sits in delayed Release, a second attempt fails
    /// closed WITHOUT reaching the stub (no second Claim); after the
    /// first attempt fully exits, its peer disconnect is observed AND
    /// the actual claim release is observed, a newer attempt proceeds
    /// (on its own private connection). The release observation — never
    /// the `ListNames` proxy, which can precede the token drop — gates
    /// the second attempt.
    #[test]
    fn token_held_through_cleanup_until_peer_disconnect() {
        let _serial = test_serialize_claim().lock().unwrap();
        let _watchdog = TestWatchdog::arm(120);
        let release_delay = Duration::from_millis(800);
        let (stub, events) = FprintStub::spawn(
            vec!["right-index-finger".to_string()],
            Duration::ZERO,
            release_delay,
        );
        let emitter = spawn_emitter(
            stub.stub_conn.clone(),
            events.start_rx,
            vec![("verify-match".to_string(), true)],
            2,
        );
        let release_rx = events.release_rx;
        let client = stub.client();
        // Attempt 1 runs aside: prompt match, then delayed Release.
        let first = std::thread::Builder::new()
            .name("tuna-fprintd-test-first".into())
            .spawn({
                let client = client.clone();
                move || client.verify("ada", Duration::from_secs(10))
            })
            .expect("first attempt spawns");
        // Deterministic sync point: the worker reached cleanup.
        release_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("Release attempted");
        // During cleanup the token still holds: denied, stub untouched.
        assert_eq!(
            client.verify("ada", Duration::from_millis(300)),
            VerifyOutcome::Error
        );
        assert_eq!(
            stub.calls().claims.len(),
            1,
            "cleanup window must admit no second Claim"
        );
        // Attempt 1 completes once the delayed Release replies.
        assert_eq!(
            first.join().expect("first attempt joins"),
            VerifyOutcome::Matched
        );
        // Full exit retires the transport: the peer connection is gone
        // (observer evidence), and the reaper verified join +
        // fd-shutdown + handles-dropped (the actual release — the gate
        // below waits for THIS, in case the name vanished first).
        let caller = stub
            .calls()
            .claim_callers
            .first()
            .expect("claim recorded")
            .clone();
        await_name_gone(&stub, &caller, Duration::from_secs(5));
        await_claim_free(Duration::from_secs(30));
        // ... and only now does a newer attempt proceed to the stub.
        assert_eq!(
            client.verify("ada", Duration::from_secs(5)),
            VerifyOutcome::Matched
        );
        emitter.join().expect("emitter joins");
        let calls = stub.calls();
        assert_eq!(calls.claims.len(), 2, "token released after full exit");
        assert_ne!(
            calls.claim_callers.first(),
            calls.claim_callers.get(1),
            "each attempt uses its own private connection"
        );
        drop(calls);
        await_claim_free(Duration::from_secs(30));
    }

    /// (g) Parent shutdown ordering: on silence the worker's signal
    /// wait ends ONLY via the parent's deadline fd shutdown. The parent
    /// must return at the deadline without waiting for the worker, and
    /// the worker must exit on the shutdown promptly enough that a
    /// second attempt RUNS its full wait (not an instant denial) — with
    /// both private transports retired afterwards.
    #[test]
    fn parent_shutdown_releases_the_worker_for_the_next_attempt() {
        let _serial = test_serialize_claim().lock().unwrap();
        let _watchdog = TestWatchdog::arm(120);
        let (stub, _events) = FprintStub::spawn(
            vec!["right-index-finger".to_string()],
            Duration::ZERO,
            Duration::ZERO,
        );
        let client = stub.client();
        let timeout = Duration::from_millis(400);
        let start = Instant::now();
        let first = client.verify("ada", timeout);
        let first_elapsed = start.elapsed();
        assert_eq!(first, VerifyOutcome::TimedOut);
        assert!(
            first_elapsed >= timeout.saturating_sub(Duration::from_millis(100)),
            "parent must wait out the budget, took {first_elapsed:?}"
        );
        assert!(
            first_elapsed < timeout + Duration::from_secs(5),
            "parent must return at the deadline, not with the worker, took {first_elapsed:?}"
        );
        assert_eq!(stub.calls().verify_starts.len(), 1);
        // The worker exits on the parent's shutdown (its only release):
        // poll until a second attempt RUNS — contention denials (instant
        // Error, stub untouched) may precede it while the worker winds
        // down and the reaper verifies the release.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let start = Instant::now();
            let second = client.verify("ada", timeout);
            let second_elapsed = start.elapsed();
            if second == VerifyOutcome::TimedOut
                && second_elapsed >= timeout.saturating_sub(Duration::from_millis(100))
            {
                break;
            }
            assert_eq!(
                second,
                VerifyOutcome::Error,
                "only contention may precede the running attempt"
            );
            assert!(
                Instant::now() < deadline,
                "worker must release the token promptly after the shutdown"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(stub.calls().verify_starts.len(), 2);
        // Both private transports retired: neither peer lingers on the bus.
        let first_caller = stub
            .calls()
            .claim_callers
            .first()
            .expect("first claim recorded")
            .clone();
        let second_caller = stub
            .calls()
            .claim_callers
            .get(1)
            .expect("second claim recorded")
            .clone();
        assert_ne!(
            first_caller, second_caller,
            "each attempt uses its own private connection"
        );
        await_name_gone(&stub, &first_caller, Duration::from_secs(5));
        await_name_gone(&stub, &second_caller, Duration::from_secs(5));
        await_claim_free(Duration::from_secs(30));
    }

    /// Reaper happy path through the REAL production reaper: a worker
    /// that shuts its transport down, drops every handle, signals
    /// completion, and exits is joined with EOF observed — the claim
    /// releases. Retirement claimed here is exactly application-worker
    /// completion + join + fd-shutdown + handles-dropped; no driver,
    /// hardware, or daemon-backend claim is made or needed.
    #[test]
    fn reaper_releases_after_verified_join_and_shutdown() {
        let _serial = test_serialize_claim().lock().unwrap();
        let _watchdog = TestWatchdog::arm(120);
        let (ours, peer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        let observe = ours.try_clone().expect("observe clone");
        let held = ClaimGuard::try_claim().expect("claim free in test");
        held.transfer();
        let (completion_tx, completion_rx) = mpsc::channel::<()>();
        let worker = std::thread::Builder::new()
            .name("tuna-fprintd-test-reaped".into())
            .spawn(move || {
                // The production worker's retirement order, verbatim in
                // miniature: shutdown first, drop every handle, signal
                // completion last, then return at once.
                let _ = ours.shutdown(std::net::Shutdown::Both);
                drop(ours);
                drop(peer);
                let _ = completion_tx.send(());
            })
            .expect("test worker spawns");
        reap_worker(
            completion_rx,
            worker,
            observe,
            ClaimGuard::adopt(),
            retire_in(Duration::from_secs(5)),
        );
        assert_eq!(ClaimGuard::test_state(), ClaimState::Free);
    }

    /// Reaper safe direction: a worker that completes and exits WITHOUT
    /// shutting its transport down (the close-failure shape — EOF never
    /// observed on a live peer) quarantines instead of releasing, and
    /// the quarantine sticks even after the peer later goes away.
    #[test]
    fn reaper_quarantines_when_shutdown_unobserved() {
        let _serial = test_serialize_claim().lock().unwrap();
        let _watchdog = TestWatchdog::arm(120);
        let (ours, peer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        let observe = ours.try_clone().expect("observe clone");
        let held = ClaimGuard::try_claim().expect("claim free in test");
        held.transfer();
        let (completion_tx, completion_rx) = mpsc::channel::<()>();
        let (peer_tx, peer_rx) = mpsc::channel::<std::os::unix::net::UnixStream>();
        let worker = std::thread::Builder::new()
            .name("tuna-fprintd-test-unshut".into())
            .spawn(move || {
                // Completes and exits with the transport still live: no
                // shutdown issued (the close-failure shape under test).
                // The peer end moves back to the test so it provably
                // outlives the worker.
                drop(ours);
                peer_tx.send(peer).expect("peer moves out");
                let _ = completion_tx.send(());
            })
            .expect("test worker spawns");
        reap_worker(
            completion_rx,
            worker,
            observe,
            ClaimGuard::adopt(),
            retire_in(Duration::from_secs(5)),
        );
        assert_eq!(ClaimGuard::test_state(), ClaimState::Quarantined);
        // Sticky: dropping the peer afterwards changes nothing.
        drop(peer_rx.recv_timeout(JOIN_BUDGET).expect("peer held out"));
        assert_eq!(ClaimGuard::test_state(), ClaimState::Quarantined);
        assert!(ClaimGuard::try_claim().is_none());
        ClaimGuard::test_reset();
        assert_eq!(ClaimGuard::test_state(), ClaimState::Free);
    }

    /// Reaper wedge path: a worker parked past a short test
    /// retirement budget (completion sender held, never sent)
    /// quarantines WITHOUT waiting for it — the reaper returns at the
    /// budget and detaches the handle. The parked worker is released
    /// afterwards and exits on its own (signalled, so no leaked thread
    /// hides behind the detach): quarantine is a state, not a parked
    /// thread.
    #[test]
    fn reaper_quarantines_a_wedged_worker_bounded() {
        let _serial = test_serialize_claim().lock().unwrap();
        let _watchdog = TestWatchdog::arm(120);
        let (ours, peer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        let observe = ours.try_clone().expect("observe clone");
        let held = ClaimGuard::try_claim().expect("claim free in test");
        held.transfer();
        let (completion_tx, completion_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let (exiting_tx, exiting_rx) = mpsc::channel::<()>();
        let worker = std::thread::Builder::new()
            .name("tuna-fprintd-test-wedged".into())
            .spawn(move || {
                // Parked: holds its ends and the unsent completion
                // sender until the test releases it — AFTER the verdict.
                let _held = (completion_tx, ours, peer);
                release_rx.recv().expect("test releases the wedge");
                let _ = exiting_tx.send(());
            })
            .expect("wedged worker spawns");
        let start = Instant::now();
        reap_worker(
            completion_rx,
            worker,
            observe,
            ClaimGuard::adopt(),
            retire_in(Duration::from_millis(300)),
        );
        let elapsed = start.elapsed();
        assert_eq!(ClaimGuard::test_state(), ClaimState::Quarantined);
        assert!(
            elapsed < Duration::from_secs(5),
            "wedged reap must return at the retirement budget, took {elapsed:?}"
        );
        // The detached worker still exits on its own once released.
        release_tx.send(()).expect("wedge released");
        exiting_rx
            .recv_timeout(JOIN_BUDGET)
            .expect("released worker exits");
        ClaimGuard::test_reset();
        assert_eq!(ClaimGuard::test_state(), ClaimState::Free);
    }

    /// Reaper death path: a worker that dies WITHOUT completing (panic
    /// drops the completion sender) quarantines at once via the
    /// disconnected channel — without waiting out the retirement
    /// budget — and the bounded join collects the panic.
    #[test]
    fn reaper_quarantines_a_dead_worker_at_once() {
        let _serial = test_serialize_claim().lock().unwrap();
        let _watchdog = TestWatchdog::arm(120);
        let (ours, peer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        let observe = ours.try_clone().expect("observe clone");
        let held = ClaimGuard::try_claim().expect("claim free in test");
        held.transfer();
        let (completion_tx, completion_rx) = mpsc::channel::<()>();
        let worker = std::thread::Builder::new()
            .name("tuna-fprintd-test-panicker".into())
            .spawn(move || {
                let _held = (completion_tx, ours, peer);
                panic!("synthetic test-worker panic");
            })
            .expect("panicking worker spawns");
        let start = Instant::now();
        reap_worker(
            completion_rx,
            worker,
            observe,
            ClaimGuard::adopt(),
            retire_in(Duration::from_secs(30)),
        );
        let elapsed = start.elapsed();
        assert_eq!(ClaimGuard::test_state(), ClaimState::Quarantined);
        assert!(
            elapsed < Duration::from_secs(5),
            "death path must not wait out the retirement budget, took {elapsed:?}"
        );
        assert!(ClaimGuard::try_claim().is_none());
        ClaimGuard::test_reset();
        assert_eq!(ClaimGuard::test_state(), ClaimState::Free);
    }

    /// Retirement-budget discrimination: the grace runs from the attempt
    /// deadline, never from spawn. A healthy worker completing 3s after
    /// spawn — past a 2s from-spawn grace, before its 5s attempt
    /// deadline — must RELEASE (retire_by = deadline + 2s test grace).
    /// Against from-spawn semantics this test FAILS (quarantine at 2s),
    /// which is exactly the 58ff defect it pins.
    #[test]
    fn reaper_releases_a_healthy_attempt_past_from_spawn_grace() {
        let _serial = test_serialize_claim().lock().unwrap();
        let _watchdog = TestWatchdog::arm(120);
        let (ours, peer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        let observe = ours.try_clone().expect("observe clone");
        let held = ClaimGuard::try_claim().expect("claim free in test");
        held.transfer();
        let (completion_tx, completion_rx) = mpsc::channel::<()>();
        let worker = std::thread::Builder::new()
            .name("tuna-fprintd-test-slow-healthy".into())
            .spawn(move || {
                // Healthy but slow: completes 3s after spawn (the
                // attempt deadline is 5s out), with the textbook
                // retirement order.
                std::thread::sleep(Duration::from_secs(3));
                let _ = ours.shutdown(std::net::Shutdown::Both);
                drop(ours);
                drop(peer);
                let _ = completion_tx.send(());
            })
            .expect("slow worker spawns");
        // Attempt deadline +5s, test grace +2s: retire_by is +7s, so a
        // 3s completion releases. From-spawn grace semantics would have
        // quarantined at +2s.
        let retire_by = retire_in(Duration::from_secs(7));
        reap_worker(
            completion_rx,
            worker,
            observe,
            ClaimGuard::adopt(),
            retire_by,
        );
        assert_eq!(
            ClaimGuard::test_state(),
            ClaimState::Free,
            "healthy completion before the attempt deadline must release"
        );
    }

    /// Unobserved-exit path: a worker that signals completion but then
    /// parks past the retirement budget (never exiting) quarantines —
    /// the reaper never joins an unobserved thread — and returns
    /// bounded. The parked worker is released afterwards and exits on
    /// its own (signalled).
    #[test]
    fn reaper_quarantines_completion_without_observed_exit() {
        let _serial = test_serialize_claim().lock().unwrap();
        let _watchdog = TestWatchdog::arm(120);
        let (ours, peer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        let observe = ours.try_clone().expect("observe clone");
        let held = ClaimGuard::try_claim().expect("claim free in test");
        held.transfer();
        let (completion_tx, completion_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let (exiting_tx, exiting_rx) = mpsc::channel::<()>();
        let worker = std::thread::Builder::new()
            .name("tuna-fprintd-test-completed-parked".into())
            .spawn(move || {
                let _ = ours.shutdown(std::net::Shutdown::Both);
                drop(ours);
                drop(peer);
                let _ = completion_tx.send(());
                // Completed but never exits: parks until released AFTER
                // the verdict (the shape an unbounded join would hang
                // on).
                release_rx.recv().expect("test releases the parked worker");
                let _ = exiting_tx.send(());
            })
            .expect("parking worker spawns");
        let start = Instant::now();
        reap_worker(
            completion_rx,
            worker,
            observe,
            ClaimGuard::adopt(),
            retire_in(Duration::from_millis(500)),
        );
        let elapsed = start.elapsed();
        assert_eq!(ClaimGuard::test_state(), ClaimState::Quarantined);
        assert!(
            elapsed < Duration::from_secs(5),
            "unobserved exit must quarantine bounded, took {elapsed:?}"
        );
        release_tx.send(()).expect("parked worker released");
        exiting_rx
            .recv_timeout(JOIN_BUDGET)
            .expect("released worker exits");
        ClaimGuard::test_reset();
        assert_eq!(ClaimGuard::test_state(), ClaimState::Free);
    }

    /// Worker-spawn-failure handler, directly: shuts the retained clone
    /// down (the peer observes EOF), releases ONLY on the verified
    /// shutdown+EOF condition, and denies. Real pthread spawn failure is
    /// not inducible, so the handler — not the trigger — is what CI
    /// pins; the kit-drop test below drives the full destruction order.
    #[test]
    fn worker_spawn_failure_handler_shuts_down_and_releases() {
        let _serial = test_serialize_claim().lock().unwrap();
        let _watchdog = TestWatchdog::arm(120);
        let (shutdown_end, mut peer_end) =
            std::os::unix::net::UnixStream::pair().expect("socketpair");
        peer_end
            .set_read_timeout(Some(JOIN_BUDGET))
            .expect("peer read timeout");
        // Same-socket observe clone, as in production (the reaper's
        // clone is still owned on the spawn-failure path).
        let observe = shutdown_end.try_clone().expect("observe clone");
        let guard = ClaimGuard::try_claim().expect("claim free in test");
        let outcome = Fprintd::on_worker_spawn_failure(&shutdown_end, &observe, guard);
        assert_eq!(outcome, VerifyOutcome::Error);
        let mut byte = [0u8; 1];
        assert_eq!(
            peer_end
                .read(&mut byte)
                .expect("peer EOF read must succeed"),
            0,
            "peer must observe EOF after the handler shutdown"
        );
        assert_eq!(ClaimGuard::test_state(), ClaimState::Free);
    }

    /// Worker-spawn-failure handler quarantines when the shutdown is
    /// unobserved: the observe clone sits on a different, live,
    /// silent transport (no EOF), so the verified condition fails and
    /// the claim quarantines instead of releasing. Denies either way.
    #[test]
    fn worker_spawn_failure_handler_quarantines_when_shutdown_unobserved() {
        let _serial = test_serialize_claim().lock().unwrap();
        let _watchdog = TestWatchdog::arm(120);
        let (shutdown_end, _peer_held) =
            std::os::unix::net::UnixStream::pair().expect("socketpair");
        // A different live socket: held open and silent, so its reads
        // never run dry — EOF stays unobserved, deterministically.
        let (observed_end, _observed_peer) =
            std::os::unix::net::UnixStream::pair().expect("socketpair");
        let guard = ClaimGuard::try_claim().expect("claim free in test");
        let outcome = Fprintd::on_worker_spawn_failure(&shutdown_end, &observed_end, guard);
        assert_eq!(outcome, VerifyOutcome::Error);
        assert_eq!(
            ClaimGuard::test_state(),
            ClaimState::Quarantined,
            "unobserved shutdown must quarantine, never release"
        );
        ClaimGuard::test_reset();
        assert_eq!(ClaimGuard::test_state(), ClaimState::Free);
    }

    /// `join_bounded` with an unrepresentable end fails closed at once:
    /// the parked worker is NOT joined (no unbounded wait), the handle
    /// detaches, and the caller gets `None` immediately. The parked
    /// thread exits on its own when its release channel drops.
    #[test]
    fn join_bounded_overflow_fails_closed_without_joining() {
        let _watchdog = TestWatchdog::arm(120);
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let worker = std::thread::spawn(move || {
            let _ = release_rx.recv();
        });
        let start = Instant::now();
        // (`thread::Result` is not `Debug`, so no `assert_eq!` here.)
        assert!(
            join_bounded(worker, Duration::MAX).is_none(),
            "overflow budget must fail closed, never join"
        );
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "overflow must return at once, not after any wait"
        );
        drop(release_tx);
    }

    /// Reaper-spawn-failure handler, directly: waits the recomputed
    /// remainder, shuts down-only, QUARANTINES (verification without
    /// the handle is impossible), and returns the outcome — never an
    /// inline join. Both the timeout shape (outcome unknown, worker
    /// detached-alive) and the answered shape are pinned.
    #[test]
    fn reaper_spawn_failure_handler_quarantines_without_joining() {
        let _serial = test_serialize_claim().lock().unwrap();
        let _watchdog = TestWatchdog::arm(120);
        // Answered shape: the detached worker's outcome arrives, but
        // the claim still quarantines (unverifiable retirement).
        let (shutdown_end, peer_end) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        peer_end
            .set_read_timeout(Some(JOIN_BUDGET))
            .expect("peer read timeout");
        let (outcome_tx, outcome_rx) = mpsc::channel::<VerifyOutcome>();
        outcome_tx
            .send(VerifyOutcome::Matched)
            .expect("outcome sends");
        let guard = ClaimGuard::try_claim().expect("claim free in test");
        let outcome = Fprintd::on_reaper_spawn_failure(
            outcome_rx,
            &shutdown_end,
            guard,
            Instant::now() + Duration::from_secs(5),
        );
        assert_eq!(outcome, VerifyOutcome::Matched, "outcome passes through");
        assert_eq!(ClaimGuard::test_state(), ClaimState::Quarantined);
        // Timeout shape: nothing answers within the recomputed
        // remainder — shutdown-only, then quarantine, bounded.
        let (shutdown_end, mut peer_end) =
            std::os::unix::net::UnixStream::pair().expect("socketpair");
        peer_end
            .set_read_timeout(Some(JOIN_BUDGET))
            .expect("peer read timeout");
        let (_held_tx, outcome_rx) = mpsc::channel::<VerifyOutcome>();
        ClaimGuard::test_reset();
        let guard = ClaimGuard::try_claim().expect("claim free in test");
        let start = Instant::now();
        let outcome = Fprintd::on_reaper_spawn_failure(
            outcome_rx,
            &shutdown_end,
            guard,
            Instant::now() + Duration::from_millis(200),
        );
        assert_eq!(outcome, VerifyOutcome::TimedOut);
        assert_eq!(ClaimGuard::test_state(), ClaimState::Quarantined);
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "handler must return at the remainder, bounded"
        );
        let mut byte = [0u8; 1];
        assert_eq!(
            peer_end
                .read(&mut byte)
                .expect("peer EOF read must succeed"),
            0,
            "peer must observe EOF after the handler shutdown"
        );
        ClaimGuard::test_reset();
        assert_eq!(ClaimGuard::test_state(), ClaimState::Free);
    }

    /// Closure-destruction order, for real: the ACTUAL worker spawn
    /// closure (`move || WorkerKit::run(kit)` — whole-kit capture, never
    /// destructured) drops unstarted over a genuinely built connection
    /// on the RETAINED same underlying socket (production setup order:
    /// bounded unix connect, clones retained, THEN the attempt
    /// connection built over the original). The armed guard (field 0)
    /// shuts the shared socket down BEFORE the captured connection
    /// drops (whole-kit field order — a language guarantee, not handler
    /// sequencing). An INDEPENDENT observer (a distinct bus connection —
    /// never a proxy cloned off the dropped conn, which would pin the
    /// name alive) sees the name vanish, and a retained same-socket
    /// clone observes the shutdown — all BEFORE any parent handler
    /// runs, with the claim still held. The parent handler then
    /// releases on the verified shutdown+EOF condition.
    #[test]
    fn worker_kit_drop_shuts_down_before_parent_handler() {
        let _serial = test_serialize_claim().lock().unwrap();
        let _watchdog = TestWatchdog::arm(120);
        TEST_PANIC_IN_WORKER.store(false, Ordering::Release);
        let bus = TestBus::spawn();
        // Independent observer: its own connection, so the dropped
        // attempt's name CAN vanish from its `ListNames` poll.
        let observer = bus.connect();
        let proxy = zbus::blocking::Proxy::new(
            &observer,
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
        )
        .expect("dbus proxy builds");
        // Production setup order against the test bus: bounded unix
        // connect, retain same-socket clones (guard + parent + observe),
        // then build the attempt connection over the original socket.
        let probe = Fprintd {
            conn: observer.clone(),
            bus_address: Some(bus.address.clone()),
        };
        let target = probe.bus_unix_target().expect("test bus is a unix target");
        let ConnectOutcome::Connected(sock) =
            connect_unix_deadline(&target, retire_in(Duration::from_secs(10)))
        else {
            panic!("test bus connect must succeed");
        };
        let guard_sock = sock.try_clone().expect("guard clone");
        let parent_shutdown = sock.try_clone().expect("parent clone");
        let observe = sock.try_clone().expect("observe clone");
        let SetupOutcome::Built(built) =
            build_attempt_connection(sock, DBUS_CALL_TIMEOUT, Duration::from_secs(10))
        else {
            panic!("attempt connection must build");
        };
        let conn = zbus::blocking::Connection::from(built);
        let ours = conn
            .unique_name()
            .expect("connection has a unique name")
            .as_str()
            .to_owned();
        let names: Vec<String> = proxy
            .call::<_, _, Vec<String>>("ListNames", &())
            .expect("ListNames answers");
        assert!(
            names.iter().any(|listed| listed == &ours),
            "attempt connection genuinely on the bus before destruction"
        );
        let (outcome_tx, _outcome_rx) = mpsc::channel::<VerifyOutcome>();
        let (completion_tx, _completion_rx) = mpsc::channel::<()>();
        let guard = ClaimGuard::try_claim().expect("claim free in test");
        let kit = WorkerKit {
            shutdown_guard: ShutdownFirst::arm(guard_sock),
            attempt: Fprintd {
                conn,
                bus_address: Some(bus.address.clone()),
            },
            user: "ada".to_owned(),
            deadline: retire_in(Duration::from_secs(5)),
            outcome_tx,
            completion_tx,
        };
        // The unstarted-spawn shape: the ACTUAL spawn closure value
        // drops whole, never invoked — no parent handler has run.
        let closure = move || WorkerKit::run(kit);
        drop(closure);
        // Destruction effects, BEFORE the parent handler: our side sees
        // the shutdown on the retained same-socket clone (the production
        // predicate), and the INDEPENDENT observer sees the name leave
        // the bus (the daemon-side effect) — with the claim still held.
        assert!(
            shutdown_observed(&observe),
            "kit drop must shut the shared socket down first"
        );
        let deadline = Instant::now() + JOIN_BUDGET;
        loop {
            let names: Vec<String> = proxy
                .call::<_, _, Vec<String>>("ListNames", &())
                .expect("ListNames answers");
            if !names.iter().any(|listed| listed == &ours) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "dropped kit connection must leave the bus"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            ClaimGuard::test_state(),
            ClaimState::Held,
            "kit destruction never touches the claim"
        );
        // ... then the parent handler (retained clones, as in
        // production) releases on the verified shutdown+EOF condition.
        let outcome = Fprintd::on_worker_spawn_failure(&parent_shutdown, &observe, guard);
        assert_eq!(outcome, VerifyOutcome::Error);
        assert_eq!(ClaimGuard::test_state(), ClaimState::Free);
    }

    /// Shutdown observation drains healthy chatter: a peer that queued
    /// 64KiB and then went quiet still yields EOF after our shutdown
    /// (probed kernel behavior), so a talkative-but-retired transport
    /// releases instead of false-quarantining.
    #[test]
    fn shutdown_observed_drains_queued_chatter_to_eof() {
        let _watchdog = TestWatchdog::arm(120);
        let (ours, mut peer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        peer.set_nonblocking(true).expect("peer nonblocking");
        let chunk = vec![0xC4u8; 4096];
        for _ in 0..16 {
            let mut written = 0usize;
            while written < chunk.len() {
                match peer.write(&chunk[written..]) {
                    Ok(0) => panic!("peer write must progress"),
                    Ok(n) => written += n,
                    Err(error) => panic!("peer must queue 64KiB freely: {error}"),
                }
            }
        }
        let observe = ours.try_clone().expect("observe clone");
        ours.shutdown(std::net::Shutdown::Both)
            .expect("shutdown issues");
        assert!(
            shutdown_observed(&observe),
            "shutdown with queued chatter must drain to EOF and release"
        );
    }

    /// Shutdown observation is cap-bounded: chatter past a tiny test cap
    /// quarantines (returns false) instead of draining forever. The
    /// production cap is [`SHUTDOWN_DRAIN_CAP`]; this pins the cap
    /// MECHANISM, not the constant. Each half uses a fresh pair with
    /// its chatter queued BEFORE shutdown (nothing is ever sent
    /// post-shutdown — that behavior is not relied on).
    #[test]
    fn shutdown_observed_cap_exceeded_quarantines() {
        let _watchdog = TestWatchdog::arm(120);
        // Over the tiny cap: quarantine.
        let (ours, mut peer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        peer.set_nonblocking(true).expect("peer nonblocking");
        peer.write_all(&vec![0xC4u8; 4096])
            .expect("peer queues 4KiB");
        let observe = ours.try_clone().expect("observe clone");
        ours.shutdown(std::net::Shutdown::Both)
            .expect("shutdown issues");
        assert!(
            !shutdown_observed_capped(&observe, 16),
            "chatter past the cap must quarantine, not release"
        );
        // Same backlog shape under the production cap: drain to EOF.
        let (ours, mut peer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        peer.set_nonblocking(true).expect("peer nonblocking");
        peer.write_all(&vec![0xC4u8; 4096])
            .expect("peer queues 4KiB");
        let observe = ours.try_clone().expect("observe clone");
        ours.shutdown(std::net::Shutdown::Both)
            .expect("shutdown issues");
        assert!(
            shutdown_observed(&observe),
            "4KiB chatter is far under the production cap"
        );
    }

    /// Listener held full: backlog pinned to zero with one completed but
    /// never accepted connection, so every further connect answers
    /// `EAGAIN` until the test drains it. Models a saturated bus
    /// listener for the connect controls below.
    struct BacklogFull {
        dir: tempfile::TempDir,
        listener: std::os::unix::net::UnixListener,
        _held: std::os::unix::net::UnixStream,
    }

    impl BacklogFull {
        fn spawn() -> Self {
            use std::os::unix::net::{UnixListener, UnixStream};
            let dir = tempfile::tempdir().expect("backlog dir");
            let path = dir.path().join("backlog.sock");
            let listener = UnixListener::bind(&path).expect("backlog binds");
            // SAFETY: the fd is an open bound listener; re-listening only
            // adjusts the backlog.
            let pinned = unsafe { libc::listen(listener.as_raw_fd(), 0) };
            assert_eq!(pinned, 0, "backlog pins to zero");
            // One completed-but-unaccepted connection fills the queue.
            let held = UnixStream::connect(&path).expect("filler connects");
            Self {
                dir,
                listener,
                _held: held,
            }
        }

        fn target(&self) -> UnixTarget {
            UnixTarget::Path(self.dir.path().join("backlog.sock"))
        }

        /// Accept one queued connection after `delay` (drains the wedge).
        fn drain_after(&self, delay: Duration) -> std::thread::JoinHandle<()> {
            let listener = self.listener.try_clone().expect("listener clones");
            std::thread::Builder::new()
                .name("tuna-fprintd-test-drain".into())
                .spawn(move || {
                    std::thread::sleep(delay);
                    let _ = listener.accept();
                })
                .expect("drain spawns")
        }
    }

    /// A refused connect (no listener) fails FAST as refused — never a
    /// hang, never a timeout wait.
    #[test]
    fn connect_refused_fails_fast() {
        let dir = tempfile::tempdir().expect("refused dir");
        let target = UnixTarget::Path(dir.path().join("nobody.sock"));
        let start = Instant::now();
        assert!(matches!(
            connect_unix_deadline(&target, Instant::now() + Duration::from_secs(10)),
            ConnectOutcome::Refused
        ));
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "refusal must be fast"
        );
    }

    /// Backlog-full past the deadline fails bounded as timed-out (the
    /// `EAGAIN` loop never spins past the absolute budget), and the
    /// public path reports `TimedOut` with the claim released.
    #[test]
    fn connect_backlog_full_times_out_bounded() {
        let _serial = test_serialize_claim().lock().unwrap();
        let _watchdog = TestWatchdog::arm(120);
        let backlog = BacklogFull::spawn();
        let target = backlog.target();
        let budget = Duration::from_millis(300);
        let start = Instant::now();
        assert!(matches!(
            connect_unix_deadline(&target, Instant::now() + budget),
            ConnectOutcome::TimedOut
        ));
        let elapsed = start.elapsed();
        assert!(
            elapsed >= budget.saturating_sub(Duration::from_millis(100)),
            "EAGAIN retries must wait out the budget, took {elapsed:?}"
        );
        assert!(
            elapsed < budget + Duration::from_secs(5),
            "EAGAIN retries must not linger past the deadline, took {elapsed:?}"
        );
        // ... and through the public path: TimedOut, claim released.
        let companion = TestBus::spawn();
        let address = format!(
            "unix:path={}",
            backlog.dir.path().join("backlog.sock").display()
        );
        let client = Fprintd::for_test_address(&address, companion.connect());
        let outcome = client.verify("ada", budget);
        assert_eq!(outcome, VerifyOutcome::TimedOut);
        await_claim_free(Duration::from_secs(10));
    }

    /// Positive control: `EAGAIN` retries bridge a transiently full
    /// backlog — once the listener drains, the SAME looping connect
    /// returns connected (getpeername-confirmed by construction).
    #[test]
    fn eagain_retry_connects_when_backlog_drains() {
        let _watchdog = TestWatchdog::arm(120);
        let backlog = BacklogFull::spawn();
        let target = backlog.target();
        let drain = backlog.drain_after(Duration::from_millis(200));
        let start = Instant::now();
        let outcome = connect_unix_deadline(&target, Instant::now() + Duration::from_secs(10));
        drain.join().expect("drain joins");
        let ConnectOutcome::Connected(sock) = outcome else {
            panic!("drained backlog must connect, got {outcome:?}");
        };
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "retry must connect promptly after the drain"
        );
        // Corroboration (the variant already proves getpeername): the
        // socket is connected and nonblocking, ready for the builder.
        assert!(sock.peer_addr().is_ok(), "connected socket has a peer");
        drop(backlog);
    }

    /// A zero budget denies at once with the claim released and no
    /// worker spawned: setup never starts, the bus is untouched, and
    /// the no-worker release runs synchronously (asserted directly, not
    /// polled — program order, not timing).
    #[test]
    fn zero_timeout_releases_without_worker() {
        let _serial = test_serialize_claim().lock().unwrap();
        let _watchdog = TestWatchdog::arm(120);
        let (stub, _events) = FprintStub::spawn(vec!["right-index-finger".to_string()], D0, D0);
        let client = stub.client();
        let start = Instant::now();
        assert_eq!(
            client.verify("ada", Duration::ZERO),
            VerifyOutcome::TimedOut
        );
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "zero budget must deny at once"
        );
        assert_eq!(stub.calls().claims.len(), 0, "no worker, no bus work");
        assert_eq!(
            ClaimGuard::test_state(),
            ClaimState::Free,
            "no-worker release is synchronous"
        );
    }

    /// The no-worker retirement handler, directly: shuts the passed fd
    /// down (the peer observes EOF), drops the connection (its bus name
    /// vanishes), releases the claim on the verified condition, and
    /// reports the given outcome. Covers the spawn-failure and
    /// post-setup-zero call sites' shared handler; pthread spawn failure
    /// itself is not forcibly inducible, so the handler — not the
    /// trigger — is what CI pins here.
    #[test]
    fn retire_unspawned_shuts_down_drops_and_releases() {
        let _serial = test_serialize_claim().lock().unwrap();
        let _watchdog = TestWatchdog::arm(120);
        let bus = TestBus::spawn();
        let conn = bus.connect();
        // Independent observer: its own connection, so the retired
        // connection's name CAN vanish (a proxy cloned off `conn`
        // would pin the name on the bus past the drop).
        let observer = bus.connect();
        let proxy = zbus::blocking::Proxy::new(
            &observer,
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
        )
        .expect("dbus proxy builds");
        let names: Vec<String> = proxy
            .call::<_, _, Vec<String>>("ListNames", &())
            .expect("ListNames answers");
        let ours = conn
            .unique_name()
            .expect("connection has a unique name")
            .as_str()
            .to_owned();
        assert!(
            names.iter().any(|listed| listed == &ours),
            "test connection is on the bus"
        );
        let (shutdown_end, mut peer_end) =
            std::os::unix::net::UnixStream::pair().expect("socketpair");
        peer_end
            .set_read_timeout(Some(JOIN_BUDGET))
            .expect("peer read timeout");
        let guard = ClaimGuard::try_claim().expect("claim free in test");
        let outcome = Fprintd::retire_unspawned(&shutdown_end, conn, guard, VerifyOutcome::Error);
        assert_eq!(outcome, VerifyOutcome::Error, "outcome passes through");
        // The fd went down first: the peer reads EOF, not a hang.
        let mut byte = [0u8; 1];
        assert_eq!(
            peer_end
                .read(&mut byte)
                .expect("peer EOF read must succeed"),
            0,
            "peer must observe EOF after the shutdown"
        );
        // The connection dropped: its bus name vanishes.
        let deadline = Instant::now() + JOIN_BUDGET;
        loop {
            let names: Vec<String> = proxy
                .call::<_, _, Vec<String>>("ListNames", &())
                .expect("ListNames answers");
            if !names.iter().any(|listed| listed == &ours) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "dropped connection must leave the bus"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(ClaimGuard::test_state(), ClaimState::Free);
    }

    /// A worker that dies without completing quarantines the process for
    /// real through the PUBLIC path: the synthetic panic fires at worker
    /// start (before any bus work), the parent reports `Error` on the
    /// disconnected outcome channel, the stub sees nothing, and the
    /// reaper observes the death and quarantines — a newer attempt
    /// denies without bus work.
    #[test]
    fn worker_panic_quarantines_through_the_public_path() {
        let _serial = test_serialize_claim().lock().unwrap();
        let _watchdog = TestWatchdog::arm(120);
        let (stub, _events) = FprintStub::spawn(vec!["right-index-finger".to_string()], D0, D0);
        let client = stub.client();
        TEST_PANIC_IN_WORKER.store(true, Ordering::Release);
        let outcome = client.verify("ada", Duration::from_secs(10));
        TEST_PANIC_IN_WORKER.store(false, Ordering::Release);
        assert_eq!(outcome, VerifyOutcome::Error);
        assert_eq!(
            stub.calls().claims.len(),
            0,
            "panicked worker must never reach the bus"
        );
        // The reaper owns the verdict: poll for the quarantine (fast —
        // the death path never waits out the grace).
        let deadline = Instant::now() + Duration::from_secs(10);
        while ClaimGuard::test_state() != ClaimState::Quarantined {
            assert!(
                Instant::now() < deadline,
                "worker death must quarantine promptly"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        // ... and quarantine denies a newer attempt without bus work.
        assert_eq!(
            client.verify("ada", Duration::from_secs(5)),
            VerifyOutcome::Error
        );
        assert_eq!(stub.calls().claims.len(), 0);
        ClaimGuard::test_reset();
        assert_eq!(ClaimGuard::test_state(), ClaimState::Free);
    }

    /// Unread-but-received bytes on `sock` (`FIONREAD`): the peer-side
    /// view of a sender's pending output. `None` when unmeasurable —
    /// callers treat that as no-evidence, never as pending.
    fn test_inq(sock: &UnixStream) -> Option<usize> {
        let mut inq: libc::c_int = 0;
        // SAFETY: out-param points at a live c_int; the fd is open.
        let read = unsafe { libc::ioctl(sock.as_raw_fd(), libc::FIONREAD, &mut inq) };
        (read == 0 && inq >= 0).then_some(inq as usize)
    }

    /// Relay counters, shared with the test. `last_client_inq` is the
    /// unread backlog on the relay's client socket, sampled live every
    /// 100ms while frozen (monotonic — the frozen leg never reads) and
    /// once more at shutdown (`usize::MAX` when the worker never served
    /// a client — the tests reject that as vacuous, never as pending).
    struct RelayStats {
        c2d_forwarded: AtomicUsize,
        d2c_forwarded: AtomicUsize,
        last_client_inq: AtomicUsize,
    }

    /// Byte relay between one client and a real bus daemon: pumps both
    /// directions until `freeze_after` client→daemon bytes have passed,
    /// then STOPS reading the client (holds every socket open) while
    /// daemon→client flows to the end. The frozen leg is a
    /// never-reading peer with real bus semantics behind it — no
    /// hand-framed D-Bus needed. Killed on drop.
    struct Relay {
        dir: tempfile::TempDir,
        stats: Arc<RelayStats>,
        done: Arc<AtomicBool>,
        worker: Option<std::thread::JoinHandle<()>>,
    }

    impl Relay {
        fn spawn(daemon_addr: &str, freeze_after: usize) -> Self {
            use std::os::unix::net::UnixListener;
            let dir = tempfile::tempdir().expect("relay dir");
            let path = dir.path().join("relay.sock");
            let listener = UnixListener::bind(&path).expect("relay binds");
            listener.set_nonblocking(true).expect("relay nonblocking");
            let stats = Arc::new(RelayStats {
                c2d_forwarded: AtomicUsize::new(0),
                d2c_forwarded: AtomicUsize::new(0),
                last_client_inq: AtomicUsize::new(usize::MAX),
            });
            let done = Arc::new(AtomicBool::new(false));
            let daemon_addr = daemon_addr.to_owned();
            let worker_stats = stats.clone();
            let worker_done = done.clone();
            let worker = std::thread::Builder::new()
                .name("tuna-fprintd-test-relay".into())
                .spawn(move || {
                    Self::serve(
                        listener,
                        &daemon_addr,
                        freeze_after,
                        &worker_stats,
                        &worker_done,
                    );
                })
                .expect("relay spawns");
            Self {
                dir,
                stats,
                done,
                worker: Some(worker),
            }
        }

        fn address(&self) -> String {
            format!("unix:path={}", self.dir.path().join("relay.sock").display())
        }

        fn serve(
            listener: std::os::unix::net::UnixListener,
            daemon_addr: &str,
            freeze_after: usize,
            stats: &Arc<RelayStats>,
            done: &Arc<AtomicBool>,
        ) {
            // One client (polled accept so teardown never hangs).
            let client = loop {
                if done.load(TestOrdering::Acquire) {
                    return;
                }
                match listener.accept() {
                    Ok((sock, _)) => break sock,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => return,
                }
            };
            // The daemon leg, through the PRODUCTION connect (same
            // target parsing and bounded connect the attempts use).
            let address: zbus::Address = daemon_addr.parse().expect("daemon address parses");
            let target = Fprintd::unix_target_of(&address).expect("daemon target is unix");
            let ConnectOutcome::Connected(daemon) =
                connect_unix_deadline(&target, Instant::now() + Duration::from_secs(10))
            else {
                panic!("relay cannot reach its daemon");
            };
            // The production connect returns nonblocking (for the async
            // builder); the relay pumps blocking with timeouts instead.
            daemon.set_nonblocking(false).expect("daemon leg blocks");
            let client_reader = client.try_clone().expect("client reader clones");
            let client_inq = client.try_clone().expect("client sampler clones");
            let daemon_reader = daemon.try_clone().expect("daemon reader clones");
            let d2c_stats = stats.clone();
            let d2c_done = done.clone();
            let d2c = std::thread::Builder::new()
                .name("tuna-fprintd-test-relay-d2c".into())
                .spawn(move || {
                    Self::pump(daemon_reader, client, &d2c_done, &d2c_stats.d2c_forwarded);
                })
                .expect("d2c spawns");
            let c2d_stats = stats.clone();
            let c2d_done = done.clone();
            let c2d = std::thread::Builder::new()
                .name("tuna-fprintd-test-relay-c2d".into())
                .spawn(move || {
                    Self::pump_freeze(
                        client_reader,
                        daemon,
                        client_inq,
                        freeze_after,
                        &c2d_done,
                        &c2d_stats,
                    );
                })
                .expect("c2d spawns");
            let _ = d2c.join();
            let _ = c2d.join();
        }

        /// Pump `from` to `to` until done, EOF, or error, counting bytes.
        fn pump(
            mut from: UnixStream,
            mut to: UnixStream,
            done: &Arc<AtomicBool>,
            counter: &AtomicUsize,
        ) {
            from.set_read_timeout(Some(Duration::from_millis(100)))
                .expect("pump read timeout");
            let mut buf = [0u8; 4096];
            loop {
                if done.load(TestOrdering::Acquire) {
                    break;
                }
                match from.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if to.write_all(&buf[..n]).is_err() {
                            break;
                        }
                        counter.fetch_add(n, TestOrdering::Release);
                    }
                    Err(error)
                        if error.kind() == std::io::ErrorKind::TimedOut
                            || error.kind() == std::io::ErrorKind::WouldBlock =>
                    {
                        continue;
                    }
                    Err(_) => break,
                }
            }
        }

        /// Pump client→daemon until `freeze_after` bytes, then stop
        /// reading the client (never-reading peer from here on) and, at
        /// shutdown, sample the unread backlog the client left behind.
        fn pump_freeze(
            mut from: UnixStream,
            mut to: UnixStream,
            sampler: UnixStream,
            freeze_after: usize,
            done: &Arc<AtomicBool>,
            stats: &Arc<RelayStats>,
        ) {
            from.set_read_timeout(Some(Duration::from_millis(100)))
                .expect("freeze read timeout");
            let mut buf = [0u8; 4096];
            let mut forwarded = 0usize;
            while forwarded < freeze_after {
                if done.load(TestOrdering::Acquire) {
                    break;
                }
                match from.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if to.write_all(&buf[..n]).is_err() {
                            break;
                        }
                        forwarded += n;
                        stats.c2d_forwarded.fetch_add(n, TestOrdering::Release);
                    }
                    Err(error)
                        if error.kind() == std::io::ErrorKind::TimedOut
                            || error.kind() == std::io::ErrorKind::WouldBlock =>
                    {
                        continue;
                    }
                    Err(_) => break,
                }
            }
            // Frozen: hold every socket open, read nothing, until done —
            // sampling the unread backlog live (it only grows: nothing
            // here ever reads it), plus once more at shutdown.
            while !done.load(TestOrdering::Acquire) {
                stats.last_client_inq.store(
                    test_inq(&sampler).unwrap_or(usize::MAX),
                    TestOrdering::Release,
                );
                std::thread::sleep(Duration::from_millis(100));
            }
            stats.last_client_inq.store(
                test_inq(&sampler).unwrap_or(usize::MAX),
                TestOrdering::Release,
            );
        }
    }

    impl Drop for Relay {
        fn drop(&mut self) {
            self.done.store(true, TestOrdering::Release);
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }

    // Observation only: never selects a test branch or changes transport behavior.
    fn emit_phase(phase: &str, measurements: serde_json::Value) {
        let Ok(nonce) = std::env::var("TUNA_FPRINT_PHASE_NONCE") else {
            return;
        };
        assert!(nonce.len() == 32 && nonce.bytes().all(|b| b.is_ascii_hexdigit()));
        let record = serde_json::json!({
            "phase": phase, "nonce": nonce, "pid": std::process::id(),
            "measurements": measurements, "driver_exit": "unobserved"
        });
        let bytes = format!("FPRINT-PHASE {record}\n");
        // Diagnostic write errors must fail this specifically requested receipt.
        std::io::stderr()
            .lock()
            .write_all(bytes.as_bytes())
            .expect("phase receipt");
    }

    /// Test A — a saturated send parks `close` until fd shutdown, on the
    /// SAME transport a real attempt uses. Phase 1 proves the socket
    /// physics (fill to `EAGAIN`, a blocked sender, shutdown release);
    /// phase 2 drives a real zbus send into a frozen relay until
    /// `SIOCOUTQ` saturates, proves the send parked and a concurrent
    /// `close` stuck behind its write lock, then releases both with one
    /// fd shutdown on a retained clone — the production rescue.
    #[test]
    fn saturated_send_parks_close_until_fd_shutdown() {
        // No claim touched anywhere below: no serialization needed.
        let _watchdog = TestWatchdog::arm(180);
        // Phase 1: fill a socketpair to EAGAIN against a never-reading
        // peer, park a blocking sender on it, release it by shutdown.
        let (mut a, _peer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        a.set_nonblocking(true).expect("phase-1 nonblocking");
        let capacity = shrink_sndbuf(&a);
        assert!(capacity > 0, "send buffer capacity must be measurable");
        let chunk = vec![0xA5u8; 4096];
        let mut eagain = false;
        for _ in 0..100_000 {
            match a.write(&chunk) {
                Ok(_) => continue,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    eagain = true;
                    break;
                }
                Err(error) => panic!("fill must EAGAIN, not fail: {error}"),
            }
        }
        assert!(eagain, "finite buffer + never-reading peer must EAGAIN");
        assert!(
            test_outq(&a) > 0,
            "pending-bytes evidence: SIOCOUTQ queued unsent"
        );
        let blocking = a.try_clone().expect("blocking clone");
        blocking.set_nonblocking(false).expect("sender blocks");
        let (entering_tx, entering_rx) = mpsc::channel::<()>();
        let (done_tx, done_rx) = mpsc::channel::<std::io::Result<()>>();
        let sender = std::thread::Builder::new()
            .name("tuna-fprintd-test-blocked-send".into())
            .spawn(move || {
                entering_tx.send(()).expect("sender enters");
                let big = vec![0x5Au8; 1024 * 1024];
                let mut blocking = blocking;
                done_tx
                    .send(blocking.write_all(&big))
                    .expect("sender reports");
            })
            .expect("blocked sender spawns");
        entering_rx
            .recv_timeout(JOIN_BUDGET)
            .expect("sender enters its write");
        // Settle for thread scheduling (labeled as such — the proof is
        // physics: a full buffer plus a never-reading peer plus a
        // blocking fd means the send cannot return).
        std::thread::sleep(Duration::from_millis(200));
        assert!(
            done_rx.try_recv().is_err(),
            "send on a saturated transport must park"
        );
        let parked_outq = test_outq(&a);
        assert!(
            parked_outq > 0,
            "transport still saturated while the sender parks"
        );
        a.shutdown(std::net::Shutdown::Both)
            .expect("phase-1 shutdown");
        let sent = done_rx
            .recv_timeout(JOIN_BUDGET)
            .expect("shutdown must release the parked send");
        assert!(
            sent.is_err(),
            "released send must error, not silently succeed"
        );
        sender.join().expect("sender joins");
        emit_phase(
            "A1",
            serde_json::json!({
                "scope": "synthetic_socketpair", "capacity": capacity,
                "outq": parked_outq, "eagain": eagain, "send_parked": true,
                "shutdown_released_error": sent.is_err(), "sender_joined": true
            }),
        );

        // Phase 2: the same shape through a real zbus send. The relay
        // forwards the handshake, Hello, and liveness probe, then
        // freezes; an 8MB signal (far past every buffer) saturates the
        // SAME attempt transport type the production path builds.
        let daemon = TestBus::spawn();
        let relay = Relay::spawn(&daemon.address, 4096);
        let stats = relay.stats.clone();
        let target = UnixTarget::Path(relay.dir.path().join("relay.sock"));
        let ConnectOutcome::Connected(sock) =
            connect_unix_deadline(&target, Instant::now() + Duration::from_secs(10))
        else {
            panic!("test transport must connect through the live relay");
        };
        let capacity = shrink_sndbuf(&sock);
        assert!(capacity > 0, "send buffer capacity must be measurable");
        let observe = sock.try_clone().expect("observe clone");
        let rescue = sock.try_clone().expect("rescue clone");
        let SetupOutcome::Built(async_conn) =
            build_attempt_connection(sock, Duration::from_secs(5), Duration::from_secs(10))
        else {
            panic!("test connection must build pre-freeze");
        };
        let conn = zbus::blocking::Connection::from(async_conn);
        // Liveness probe: the path works and the relay still forwards.
        let probe = zbus::blocking::Proxy::new(
            &conn,
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
        )
        .expect("probe proxy builds");
        let _: Vec<String> = probe.call("ListNames", &()).expect("path is live");
        // The saturating send runs aside; its completion channel proves
        // parked (empty) versus released (error).
        let payload = "x".repeat(8 * 1024 * 1024);
        let big = zbus::message::Message::signal(
            "/net/reactivated/Fprint/Device/0",
            DEVICE_IFACE,
            "TunaBackpressureProbe",
        )
        .expect("probe signal builds")
        .build(&payload)
        .expect("probe payload serializes");
        drop(payload);
        let (send_tx, send_rx) = mpsc::channel::<zbus::Result<()>>();
        let send_conn = conn.clone();
        let sender = std::thread::Builder::new()
            .name("tuna-fprintd-test-big-send".into())
            .spawn(move || {
                send_tx.send(send_conn.send(&big)).expect("send reports");
            })
            .expect("big sender spawns");
        // The send MUST fill the transport: 8MB past a frozen drain.
        // Poll to saturation (must-happen event, bounded wait).
        let fill_deadline = Instant::now() + Duration::from_secs(30);
        let saturated_outq = loop {
            let outq = test_outq(&observe);
            assert!(outq >= 0, "SIOCOUTQ must stay measurable, got {outq}");
            if outq as usize >= capacity {
                break outq;
            }
            assert!(
                Instant::now() < fill_deadline,
                "saturating send must fill the transport (outq {outq}, capacity {capacity})"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        // Parked, not complete: completion needs the full write, the
        // write needs a drain, and the relay froze — so the send cannot
        // have completed. (A signal has no reply to wait for instead.)
        assert!(
            send_rx.try_recv().is_err(),
            "send on the saturated transport must park"
        );
        // ... and the relay proves the freeze (tripped, and actually
        // stopping the flow rather than merely slowing it).
        let forwarded = stats.c2d_forwarded.load(TestOrdering::Acquire);
        assert!(
            forwarded >= 4096,
            "relay must have frozen, forwarded only {forwarded}"
        );
        assert!(
            forwarded < 1024 * 1024,
            "frozen relay must stop the 8MB flow, forwarded {forwarded}"
        );
        assert!(
            stats.d2c_forwarded.load(TestOrdering::Acquire) > 0,
            "daemon-to-client must flow (Hello reply and probe answers)"
        );
        // Close-behind-parked-send: a concurrent close must STICK behind
        // the send's write lock (a 1s probe — a close that could
        // complete takes microseconds). If this completes, the hang
        // theory is FALSIFIED — report it, do not weaken the assert.
        let (close_tx, close_rx) = mpsc::channel::<()>();
        let close_conn = conn.clone();
        let closer = std::thread::Builder::new()
            .name("tuna-fprintd-test-stuck-close".into())
            .spawn(move || {
                let _ = close_conn.close();
                let _ = close_tx.send(());
            })
            .expect("closer spawns");
        assert!(
            close_rx.recv_timeout(Duration::from_secs(1)).is_err(),
            "HANG THEORY FALSIFIED: close completed while the send parked"
        );
        // The production rescue: one fd shutdown on the retained clone
        // (bypassing the write lock) releases the parked send with an
        // error, which frees the lock, which completes the close.
        rescue
            .shutdown(std::net::Shutdown::Both)
            .expect("rescue shutdown");
        let send_result = send_rx
            .recv_timeout(JOIN_BUDGET)
            .expect("shutdown must release the parked send");
        assert!(
            send_result.is_err(),
            "released send must error, not silently succeed"
        );
        close_rx
            .recv_timeout(JOIN_BUDGET)
            .expect("freed close must complete");
        sender.join().expect("sender joins");
        closer.join().expect("closer joins");
        drop(conn);
        drop(relay);
        // Corroboration: the frozen peer still holds the client's
        // unread output at shutdown.
        let inq = stats.last_client_inq.load(TestOrdering::Acquire);
        assert_ne!(inq, usize::MAX, "relay must have served the client");
        assert!(inq > 0, "frozen peer must retain unread client output");
        emit_phase(
            "A2",
            serde_json::json!({
                "scope": "synthetic_zbus_signal", "capacity": capacity,
                "outq": saturated_outq, "c2d": forwarded,
                "d2c": stats.d2c_forwarded.load(TestOrdering::Acquire), "inq": inq,
                "send_parked": true, "close_parked": true,
                "shutdown_released_error": send_result.is_err(),
                "sender_joined": true, "closer_joined": true
            }),
        );
    }

    /// Test B — public `verify` against a frozen peer: the attempt's
    /// output provably pends (peer-side unread backlog), the attempt
    /// resolves bounded and fail-closed, and the claim releases — with
    /// the frozen peer held alive THROUGH the release (teardown-first
    /// would rescue a production failure with EOF). The pending-bytes
    /// assertion runs ONLY on the mandatorily exercised measured phases
    /// (freeze tripped on the attempt's own early traffic + backlog
    /// sampled live pre-teardown); a zero backlog takes the EXPLICIT
    /// non-fill branch — never silently the same proof. Retirement
    /// claimed here is exactly owned application work (bounded outcome),
    /// socket shutdown/drop and handles released: NO zero-driver-linger
    /// claim is made (driver exit stays unobserved, outside the claim).
    #[test]
    fn frozen_peer_leaves_attempt_output_pending_and_bounded() {
        let _serial = test_serialize_claim().lock().unwrap();
        let _watchdog = TestWatchdog::arm(180);
        // Silence (no emitter): whatever the frozen leg admits, the
        // attempt cannot match — every resolution is fail-closed.
        let (stub, _events) = FprintStub::spawn(vec!["right-index-finger".to_string()], D0, D0);
        // Freeze past the build (handshake + Hello alone exceed 256
        // bytes, so the freeze ALWAYS trips), leaving the attempt's own
        // method calls pending on the frozen leg.
        let relay = Relay::spawn(&stub.bus.address, 256);
        let stats = relay.stats.clone();
        let client = Fprintd::for_test_address(&relay.address(), stub.bus.connect());
        let start = Instant::now();
        let outcome = client.verify("ada", Duration::from_secs(3));
        let elapsed = start.elapsed();
        // Bounded fail-closed resolutions only. WHICH one is timing
        // shaped (early calls pend on the frozen leg; the worker may
        // deny first or the parent may time out first) — all four are
        // bounded denials, and the pending evidence below is what pins
        // the frozen shape.
        assert!(
            matches!(
                outcome,
                VerifyOutcome::TimedOut
                    | VerifyOutcome::Error
                    | VerifyOutcome::NoDevice
                    | VerifyOutcome::NotEnrolled
            ),
            "frozen attempt must resolve bounded and fail-closed, got {outcome:?}"
        );
        assert!(
            elapsed < Duration::from_secs(15),
            "frozen attempt must not linger, took {elapsed:?}"
        );
        // The frozen peer stays alive until the claim releases: the
        // worker must retire on the PRODUCTION shutdown paths alone —
        // tearing the relay down first would rescue a production
        // failure with EOF and prove nothing.
        await_claim_free(Duration::from_secs(30));
        // One sampler cadence past retirement, so the live backlog
        // sample below postdates all client output (the relay samples
        // every 100ms; client output is final once the claim frees).
        std::thread::sleep(Duration::from_millis(150));
        // Mandatorily exercised measured phases: the freeze tripped
        // (the build's handshake + Hello flowed — the ONLY traffic the
        // test relies on pre-freeze), and the peer-side backlog was
        // sampled while the peer was still frozen alive.
        let forwarded = stats.c2d_forwarded.load(TestOrdering::Acquire);
        let inq = stats.last_client_inq.load(TestOrdering::Acquire);
        let relay_live = !relay.done.load(TestOrdering::Acquire)
            && relay
                .worker
                .as_ref()
                .is_some_and(|worker| !worker.is_finished());
        let daemon_forwarded = stats.d2c_forwarded.load(TestOrdering::Acquire);
        drop(relay);
        assert!(
            forwarded >= 256,
            "freeze must trip on the build traffic, forwarded {forwarded}"
        );
        assert_ne!(inq, usize::MAX, "peer backlog must be sampled");
        assert!(
            stats.d2c_forwarded.load(TestOrdering::Acquire) > 0,
            "daemon-to-client must flow (Hello reply and early answers)"
        );
        if inq > 0 {
            // Pending branch: the frozen peer provably holds attempt
            // output it never read (peer-side unread backlog).
            assert!(
                stub.calls().claims.len() <= 1,
                "at most one attempt ran, claims seen: {}",
                stub.calls().claims.len()
            );
        } else {
            // EXPLICIT NON-FILL branch — a different result, not the
            // same proof: the attempt ran (freeze tripped, outcome
            // bounded) but left no sampled backlog. Pinned here, never
            // asserted as pending.
            assert_eq!(
                inq, 0,
                "non-fill branch: outcome {outcome:?} with zero sampled backlog"
            );
        }
        emit_phase(
            "B",
            serde_json::json!({
                "scope": "public_verify", "branch": if inq > 0 { "pending" } else { "non_fill" },
                "inq": inq, "c2d": forwarded, "d2c": daemon_forwarded,
                "peer_live_at_retirement": relay_live, "claim_free_before_teardown": true,
                "outcome": format!("{outcome:?}"), "elapsed_ms": elapsed.as_millis(),
                "claims": stub.calls().claims.len()
            }),
        );
    }
}
