//! Fingerprint reader probe for the lock screen (#366, P-LK-02
//! fingerprint subset; #1162 F5).
//!
//! The compositor never verifies a finger itself: verification is
//! `pam_fprintd` inside the private PAM helper on the
//! `tuna-lock-fingerprint` service (see `crates/compositor/src/runtime/auth.rs`),
//! so the unlock decision always comes from PAM. Before starting that
//! conversation the lock screen asks this module three questions, all
//! read-only: is a reader present, has the session user enrolled
//! prints, and is the reader a press or a swipe sensor (its
//! `scan-type`, for GNOME's "(or place finger on reader)" / "(or swipe
//! finger across reader)" hint). It never claims the device.
//! Enrollment UI lives in Settings, not here.
//!
//! D-Bus shapes follow the fprintd reference manual
//! (`net.reactivated.Fprint.Manager` / `.Device` on the system bus):
//! `GetDefaultDevice`/`GetDevices`, `ListEnrolledFingers`, and the
//! device's `scan-type` property.
//!
//! Failure rules: [`DISABLE_ENV`], no daemon, no device, no enrolled
//! prints, an error reply or a timeout all mean "not offered" (the
//! lock screen stays password-only), carry no detail, and log nothing.
//! Every wait is bounded: the connect retries a full listener backlog
//! only until an absolute deadline, the bus handshake races the
//! remaining budget, and every method call carries
//! [`DBUS_CALL_TIMEOUT`], so a wedged daemon can delay the offer but
//! never hang the caller.
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::FromRawFd;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

/// fprintd's well-known name on the system bus.
pub const BUS_NAME: &str = "net.reactivated.Fprint";
/// Object path of the manager interface.
pub const MANAGER_PATH: &str = "/net/reactivated/Fprint/Manager";
/// `GetDevices` / `GetDefaultDevice`.
pub const MANAGER_IFACE: &str = "net.reactivated.Fprint.Manager";
/// `ListEnrolledFingers` plus the `scan-type` property.
pub const DEVICE_IFACE: &str = "net.reactivated.Fprint.Device";
/// Setting this variable (any value) disables fingerprint auth: no
/// probe connects, so the lock screen never offers it. Tests and
/// hardened images use it to pin the no-reader path.
pub const DISABLE_ENV: &str = "TUNA_FPRINTD_OFF";
/// Bound on the connect handshake and on each D-Bus method call
/// (device lookup, enrollment query, scan type). The bus default waits
/// forever; a wedged daemon must fail the probe, not hang the caller.
pub const DBUS_CALL_TIMEOUT: Duration = Duration::from_secs(10);
/// Sleep between backlog-full (`EAGAIN`) connect retries. The absolute
/// connect deadline bounds the loop; this only sets the retry cadence.
const CONNECT_RETRY_SLEEP: Duration = Duration::from_millis(2);

/// Truthful fingerprint capability for one user, after checking the
/// daemon and that user's enrollment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capability {
    /// A reader answered on the system bus.
    pub available: bool,
    /// Enrolled finger names (`ListEnrolledFingers`), empty when none.
    pub enrolled_fingers: Vec<String>,
    /// The reader's `scan-type` is `swipe` (GNOME's
    /// `FingerprintReaderType`); false for press sensors, an unreadable
    /// property, or no reader.
    pub swipe: bool,
}

impl Capability {
    /// The fail-closed capability: nothing offered.
    pub fn unavailable() -> Self {
        Self {
            available: false,
            enrolled_fingers: Vec::new(),
            swipe: false,
        }
    }

    /// Whether unlock may offer a fingerprint attempt: a reader is
    /// present *and* this user enrolled at least one print.
    pub fn offered(&self) -> bool {
        self.available && !self.enrolled_fingers.is_empty()
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
    /// The async build finished.
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
/// the raced build on timeout is safe cancellation: the builder spawns
/// the connection's tasks only as the build completes, and an
/// unconfigured client holds no interfaces or names, so nothing lingers
/// past the drop. Method calls on the built connection carry
/// `call_bound`. The async-io timer runs on its own reactor, so it
/// fires inside `zbus::block_on` like any other future.
fn build_attempt_connection(
    sock: UnixStream,
    call_bound: Duration,
    budget: Duration,
) -> SetupOutcome {
    let builder = zbus::connection::Builder::unix_stream(sock).method_timeout(call_bound);
    zbus::block_on(async {
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

/// Blocking fprintd probe client. Clone shares the connection; all
/// methods fail closed with no logging.
#[derive(Debug, Clone)]
pub struct Fprintd {
    conn: zbus::Connection,
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
            SetupOutcome::Built(conn) => Some(Self { conn }),
            SetupOutcome::Failed | SetupOutcome::TimedOut => None,
        }
    }

    /// The system bus as a connectable unix endpoint: the system bus
    /// selection (which reads `DBUS_SYSTEM_BUS_ADDRESS` and falls back
    /// to the well-known path — mirrored with zero mutation). Non-unix
    /// transports fail closed (no reader): the system bus is unix in
    /// production, and only unix gets the bounded manual connect.
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

    fn manager(&self) -> Option<zbus::Proxy<'_>> {
        zbus::block_on(zbus::Proxy::new(
            &self.conn,
            BUS_NAME,
            MANAGER_PATH,
            MANAGER_IFACE,
        ))
        .ok()
    }

    /// Default reader, else the first enumerated one, else none.
    fn default_device(&self) -> Option<zbus::OwnedObjectPath> {
        let manager = self.manager()?;
        if let Ok(path) =
            zbus::block_on(manager.call::<_, _, zbus::OwnedObjectPath>("GetDefaultDevice", &()))
        {
            return Some(path);
        }
        let devices: Vec<zbus::OwnedObjectPath> =
            zbus::block_on(manager.call("GetDevices", &())).ok()?;
        devices.into_iter().next()
    }

    fn device_proxy<'a>(&'a self, path: &'a zbus::OwnedObjectPath) -> Option<zbus::Proxy<'a>> {
        zbus::block_on(zbus::Proxy::new(
            &self.conn,
            BUS_NAME,
            path.as_str(),
            DEVICE_IFACE,
        ))
        .ok()
    }

    /// Truthful capability for `user`: reader presence, enrollment, and
    /// the reader's scan type.
    pub fn capability(&self, user: &str) -> Capability {
        let Some(path) = self.default_device() else {
            return Capability::unavailable();
        };
        let Some(device) = self.device_proxy(&path) else {
            return Capability::unavailable();
        };
        let name = effective_claim_name(user, process_user().as_deref());
        let enrolled =
            zbus::block_on(device.call::<_, _, Vec<String>>("ListEnrolledFingers", &(name,)))
                .unwrap_or_default();
        Capability {
            available: true,
            enrolled_fingers: enrolled,
            swipe: self.scan_type_is_swipe(&path),
        }
    }

    /// The device's `scan-type` property is `swipe` (fprintd Device
    /// API). An unreadable property means a press sensor, as in
    /// `pam_fprintd`.
    fn scan_type_is_swipe(&self, path: &zbus::OwnedObjectPath) -> bool {
        zbus::block_on(async {
            let properties = zbus::Proxy::new(
                &self.conn,
                BUS_NAME,
                path.as_str(),
                "org.freedesktop.DBus.Properties",
            )
            .await?;
            properties
                .call::<_, _, zbus::OwnedVariant>("Get", &(DEVICE_IFACE, "scan-type"))
                .await
        })
        .ok()
        .and_then(|value| String::try_from(value).ok())
        .is_some_and(|kind| kind == "swipe")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::os::unix::io::AsRawFd;
    use std::process::{Child, Command, Stdio};
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    };

    /// Serializes [`DISABLE_ENV`] access between tests (env is
    /// per-process; parallel tests must not interleave a
    /// set/assert/remove cycle with a read).
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn own_user_claims_anonymously_other_users_explicitly() {
        assert_eq!(effective_claim_name("ada", Some("ada")), "");
        assert_eq!(effective_claim_name("ada", Some("grace")), "ada");
        assert_eq!(effective_claim_name("ada", None), "ada");
    }

    #[test]
    fn capability_is_offered_only_when_enrolled() {
        assert!(!Capability::unavailable().offered());
        assert!(!Capability::unavailable().swipe);
        assert!(!Capability {
            available: true,
            enrolled_fingers: Vec::new(),
            swipe: true,
        }
        .offered());
        assert!(Capability {
            available: true,
            enrolled_fingers: vec!["right-index-finger".to_string()],
            swipe: false,
        }
        .offered());
    }

    #[test]
    fn disabled_env_pins_everything_off() {
        let _guard = ENV_LOCK.lock().unwrap();
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
        let _guard = ENV_LOCK.lock().unwrap();
        let _watchdog = TestWatchdog::arm(120);
        if Fprintd::disabled_by_env() {
            return;
        }
        // Without fprintd on the system bus this is None (the expected
        // CI/container path); with a daemon, an unknown user is never
        // offered a fingerprint attempt.
        if let Some(client) = Fprintd::connect() {
            assert!(!client.capability("nobody-fprintd-probe").offered());
        }
    }

    #[test]
    fn method_calls_are_bounded() {
        // The bus default waits forever; our connection must not.
        let _guard = ENV_LOCK.lock().unwrap();
        if let Some(client) = Fprintd::connect() {
            assert_eq!(client.conn.method_timeout(), Some(DBUS_CALL_TIMEOUT));
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
                    while !worker_done.load(Ordering::Acquire) {
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
            self.done.store(true, Ordering::Release);
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
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

        /// One more peer on this bus (stub service, client handle).
        fn connect(&self) -> zbus::Connection {
            self.connect_with_timeout(Duration::from_secs(2))
        }

        fn connect_with_timeout(&self, timeout: Duration) -> zbus::Connection {
            zbus::block_on(
                zbus::connection::Builder::address(self.address.as_str())
                    .method_timeout(timeout)
                    .build(),
            )
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

        fn target(&self) -> UnixTarget {
            UnixTarget::Path(self.dir.path().join("blackhole.sock"))
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

        fn target(&self) -> UnixTarget {
            UnixTarget::Path(self.dir.path().join("stall.sock"))
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

    /// Connect to `target` (which accepts), then race the bus setup
    /// against a short budget: it must resolve as timed out at the
    /// budget — not hang, and not fail fast as no-reader.
    fn assert_setup_times_out_at_the_budget(target: &UnixTarget) {
        let budget = Duration::from_millis(500);
        let start = Instant::now();
        let ConnectOutcome::Connected(sock) =
            connect_unix_deadline(target, start + Duration::from_secs(10))
        else {
            panic!("the stalling peer must accept the connect");
        };
        let outcome = build_attempt_connection(sock, DBUS_CALL_TIMEOUT, budget);
        let elapsed = start.elapsed();
        assert!(
            matches!(outcome, SetupOutcome::TimedOut),
            "a stalled setup must time out, not build or fail fast"
        );
        assert!(
            elapsed >= budget.saturating_sub(Duration::from_millis(100)),
            "setup race must wait out the budget, took {elapsed:?}"
        );
        assert!(
            elapsed < budget + Duration::from_secs(5),
            "setup race must not linger past the budget, took {elapsed:?}"
        );
    }

    /// (a) Setup-auth blackhole: the peer accepts and goes silent, so the
    /// handshake never completes.
    #[test]
    fn setup_auth_blackhole_resolves_at_the_deadline() {
        let _watchdog = TestWatchdog::arm(120);
        let blackhole = Blackhole::spawn();
        assert_setup_times_out_at_the_budget(&blackhole.target());
    }

    /// (b) Authenticated Hello stall: the peer completes the handshake
    /// and the fd negotiation, then never answers Hello.
    #[test]
    fn authenticated_hello_stall_resolves_at_the_deadline() {
        let _watchdog = TestWatchdog::arm(120);
        let stall = HelloStall::spawn();
        assert_setup_times_out_at_the_budget(&stall.target());
    }

    struct StubManager;

    // Member names below are the fprintd D-Bus spelling (the macro maps
    // snake_case to PascalCase); the tests prove the wiring end to end.
    #[zbus::interface(name = "net.reactivated.Fprint.Manager")]
    impl StubManager {
        fn get_default_device(&self) -> zbus::OwnedObjectPath {
            zbus::OwnedObjectPath::try_from(STUB_DEVICE_PATH).expect("stub device path valid")
        }

        fn get_devices(&self) -> Vec<zbus::OwnedObjectPath> {
            vec![self.get_default_device()]
        }
    }

    struct StubDevice {
        enrolled: Vec<String>,
        scan_type: String,
    }

    #[zbus::interface(name = "net.reactivated.Fprint.Device")]
    impl StubDevice {
        fn list_enrolled_fingers(&self, _username: String) -> Vec<String> {
            self.enrolled.clone()
        }

        /// Hyphenated in fprintd's interface XML.
        #[zbus(property, name = "scan-type")]
        fn scan_type(&self) -> String {
            self.scan_type.clone()
        }
    }

    /// One scripted fprintd on an owned private bus: daemon plus a stub
    /// service owning [`BUS_NAME`].
    struct FprintStub {
        bus: TestBus,
        /// Serves the stub objects; held for the fixture's lifetime.
        _service: zbus::Connection,
    }

    impl FprintStub {
        fn spawn(enrolled: Vec<String>, scan_type: &str) -> Self {
            let bus = TestBus::spawn();
            // Name acquisition is a control-plane setup call on a freshly
            // started private bus; allow CI process contention without
            // changing the client calls' two-second failure bound.
            let service = bus.connect_with_timeout(Duration::from_secs(10));
            zbus::block_on(service.request_name(BUS_NAME)).expect("stub owns the fprintd name");
            assert!(
                zbus::block_on(service.object_server().at(MANAGER_PATH, StubManager))
                    .expect("manager serves")
                    .is_none(),
                "manager serves"
            );
            assert!(
                zbus::block_on(service.object_server().at(
                    STUB_DEVICE_PATH,
                    StubDevice {
                        enrolled,
                        scan_type: scan_type.to_owned(),
                    }
                ))
                .expect("device serves")
                .is_none(),
                "device serves"
            );
            Self {
                bus,
                _service: service,
            }
        }

        /// Probe client on the private bus. No environment override.
        fn client(&self) -> Fprintd {
            Fprintd {
                conn: self.bus.connect(),
            }
        }
    }

    /// The probe against a scripted fprintd: reader present, enrolled
    /// fingers passed through, `swipe` mirroring the device's
    /// `scan-type`; no enrolled prints is not offered.
    #[test]
    fn capability_reports_enrollment_and_scan_type() {
        let _watchdog = TestWatchdog::arm(120);
        let finger = || vec!["right-index-finger".to_string()];
        for (scan_type, swipe) in [("swipe", true), ("press", false)] {
            let stub = FprintStub::spawn(finger(), scan_type);
            let capability = stub.client().capability("ada");
            assert_eq!(
                capability,
                Capability {
                    available: true,
                    enrolled_fingers: finger(),
                    swipe,
                },
                "scan-type {scan_type}"
            );
            assert!(capability.offered(), "scan-type {scan_type}");
        }
        let stub = FprintStub::spawn(Vec::new(), "swipe");
        let capability = stub.client().capability("ada");
        assert!(capability.enrolled_fingers.is_empty());
        assert!(!capability.offered(), "no enrolled prints is not offered");
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
    /// `EAGAIN` loop never spins past the absolute budget).
    #[test]
    fn connect_backlog_full_times_out_bounded() {
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
}
