//! Capture admission is an authenticated desktop-service boundary, not a bus name.
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::fs::MetadataExt;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, RwLock,
};
use zbus::{fdo, message::Header, names::UniqueName, Connection};

/// GNOME's typed display bootstrap roles. These never confer capture authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ServiceClient {
    Portal,
    FileChooser,
    GlobalShortcuts,
}
impl ServiceClient {
    pub(crate) fn from_wire(value: u32) -> fdo::Result<Self> {
        match value {
            1 => Ok(Self::Portal),
            2 => Ok(Self::FileChooser),
            3 => Ok(Self::GlobalShortcuts),
            _ => Err(fdo::Error::InvalidArgs(
                "unsupported service client type".into(),
            )),
        }
    }
    fn executables(self) -> &'static [&'static str] {
        match self {
            Self::Portal => &[
                "/usr/libexec/xdg-desktop-portal-gnome",
                "/usr/lib/xdg-desktop-portal-gnome",
            ],
            Self::FileChooser => &["/usr/bin/nautilus"],
            Self::GlobalShortcuts => &[
                "/usr/libexec/gnome-control-center-global-shortcuts-provider",
                "/usr/lib/gnome-control-center-global-shortcuts-provider",
            ],
        }
    }
}

/// Capture identity shared by the D-Bus services and the runtime.
///
/// The supervised shell is recognised on the session bus by the unique
/// names it declared over its pidfd-pinned control session
/// ([`crate::control::ControlHub::shell_bus_names`]), never by a PID. The
/// bus never reuses a unique name, so neither PID reuse nor the bus's
/// `ProcessFD` support affects it.
#[derive(Clone, Default)]
pub struct Authority {
    locked: Arc<AtomicBool>,
    shell_names: Arc<RwLock<Vec<String>>>,
}

pub(crate) struct ConnectionCaller {
    pub owner: String,
    pub credentials: Arc<fdo::ConnectionCredentials>,
}

/// The kernel's `SO_PEERPIDFD` of a connected Unix socket: a descriptor
/// pinned to the process that connected, taken at connect time. `None`
/// when the kernel lacks it (before 6.5) or the peer is gone.
pub(crate) fn socket_pidfd(socket: &std::os::unix::net::UnixStream) -> Option<OwnedFd> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let mut descriptor: libc::c_int = -1;
    let mut length = std::mem::size_of_val(&descriptor) as libc::socklen_t;
    // SAFETY: socket remains borrowed; both output pointers refer to writable
    // values of the specified size. On success the kernel returns an owned FD.
    let result = unsafe {
        libc::getsockopt(
            socket.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERPIDFD,
            (&mut descriptor as *mut libc::c_int).cast(),
            &mut length,
        )
    };
    if result != 0 || descriptor < 0 {
        return None;
    }
    // SAFETY: a successful SO_PEERPIDFD allocated this descriptor for us.
    let descriptor = unsafe { OwnedFd::from_raw_fd(descriptor) };
    (length as usize == std::mem::size_of::<libc::c_int>()).then_some(descriptor)
}

/// Capture the kernel's original peer identity while accepting a native
/// socket. SO_PEERPIDFD pins the socket peer; opening a pidfd from a numeric
/// SO_PEERCRED PID later would instead risk pinning a recycled process.
/// Without SO_PEERPIDFD there is no identity at all: callers fail closed.
pub(crate) fn socket_credentials(
    socket: &std::os::unix::net::UnixStream,
) -> Option<Arc<fdo::ConnectionCredentials>> {
    let peer = rustix::net::sockopt::socket_peercred(socket).ok()?;
    let pid = u32::try_from(peer.pid.as_raw_pid())
        .ok()
        .filter(|pid| *pid != 0)?;
    let descriptor = socket_pidfd(socket)?;
    Some(Arc::new(
        fdo::ConnectionCredentials::default()
            .set_unix_user_id(peer.uid.as_raw())
            .set_process_id(pid)
            .set_process_fd(descriptor.into()),
    ))
}

/// Whether the process a pidfd pins has not exited. A pidfd polls readable
/// once its process exits; until then its PID cannot be recycled.
pub(crate) fn pidfd_live(descriptor: BorrowedFd<'_>) -> bool {
    let mut poll = [rustix::event::PollFd::new(
        &descriptor,
        rustix::event::PollFlags::IN,
    )];
    let timeout = rustix::event::Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    rustix::event::poll(&mut poll, Some(&timeout)).is_ok_and(|ready| ready == 0)
}

/// The PID the kernel says a pidfd pins (`Pid:` in its
/// `/proc/self/fdinfo` entry), so a descriptor handed over by a bus is
/// checked against the PID reported beside it. `None` for a descriptor
/// that is not a pidfd, or whose process exited (`Pid: -1`).
pub(crate) fn pidfd_pid(descriptor: BorrowedFd<'_>) -> Option<u32> {
    use std::os::fd::AsRawFd;
    let info =
        std::fs::read_to_string(format!("/proc/self/fdinfo/{}", descriptor.as_raw_fd())).ok()?;
    info.lines()
        .find_map(|line| line.strip_prefix("Pid:"))
        .and_then(|pid| pid.trim().parse::<u32>().ok())
        .filter(|pid| *pid != 0)
}

/// A numeric PID alone can be recycled. Only expose live process identity
/// when the bus supplied a descriptor pinning the original connection peer.
pub(crate) fn pinned_process_id(credentials: &fdo::ConnectionCredentials) -> Option<u32> {
    let pid = credentials.process_id().filter(|pid| *pid != 0)?;
    pidfd_live(credentials.process_fd()?.as_fd()).then_some(pid)
}
impl Authority {
    /// The portal opens its native display before acquiring its service name.
    /// This authenticates that bootstrap executable, without granting capture.
    pub async fn admit_portal_connection(
        &self,
        conn: &Connection,
        header: &Header<'_>,
    ) -> fdo::Result<String> {
        self.admit_service_connection(conn, header, ServiceClient::Portal)
            .await
            .map(|caller| caller.owner)
    }
    /// Resolve bootstrap identity from the bus credentials and installed binary,
    /// independently of provider names acquired after GTK display initialization.
    pub(crate) async fn admit_service_connection(
        &self,
        conn: &Connection,
        header: &Header<'_>,
        role: ServiceClient,
    ) -> fdo::Result<ConnectionCaller> {
        let caller = self.connection_caller(conn, header).await?;
        if !installed_service(&caller.credentials, role) {
            return Err(denied(
                "service connection requires the installed provider for its type",
            ));
        }
        Ok(caller)
    }
    /// An ordinary display connection grants no provider or capture role.
    pub(crate) async fn admit_connection(
        &self,
        conn: &Connection,
        header: &Header<'_>,
    ) -> fdo::Result<ConnectionCaller> {
        self.connection_caller(conn, header).await
    }
    async fn connection_caller(
        &self,
        conn: &Connection,
        header: &Header<'_>,
    ) -> fdo::Result<ConnectionCaller> {
        let sender = header.sender().ok_or_else(|| denied("missing sender"))?;
        let dbus = fdo::DBusProxy::new(conn).await?;
        let credentials = dbus
            .get_connection_credentials(sender.clone().into())
            .await?;
        if credentials.unix_user_id() != Some(rustix::process::geteuid().as_raw())
            || credentials.process_id().is_none_or(|pid| pid == 0)
        {
            return Err(denied("display connection requires a same-user process"));
        }
        Ok(ConnectionCaller {
            owner: sender.to_string(),
            credentials: Arc::new(credentials),
        })
    }
    /// Publish the lock flag and the supervised shell's declared session-bus
    /// unique names (empty while no live, pinned shell session declared one).
    pub fn publish<'a, I>(&self, locked: bool, shell_names: I)
    where
        I: IntoIterator<Item = &'a str>,
        I::IntoIter: Clone,
    {
        self.locked.store(locked, Ordering::SeqCst);
        let shell_names = shell_names.into_iter();
        let current = self
            .shell_names
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if current.iter().map(String::as_str).eq(shell_names.clone()) {
            return;
        }
        drop(current);
        *self
            .shell_names
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            shell_names.map(str::to_owned).collect();
    }
    /// Whether `sender` is a unique name the supervised shell declared.
    fn is_shell(&self, sender: &str) -> bool {
        self.shell_names
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .any(|name| name == sender)
    }
    pub fn unlocked(&self) -> fdo::Result<()> {
        if self.locked.load(Ordering::SeqCst) {
            return Err(denied("session locked"));
        }
        Ok(())
    }
    /// Whether a capture session's creator may keep it. The shell is
    /// tracked by unique name: it stays an owner while the bus still has
    /// that name and the shell's pinned control session still declares it.
    pub async fn owner_alive(&self, conn: &Connection, owner: &str) -> bool {
        if self.unlocked().is_err() {
            return false;
        }
        let Ok(name) = UniqueName::try_from(owner) else {
            return false;
        };
        let Ok(dbus) = fdo::DBusProxy::new(conn).await else {
            return false;
        };
        if self.is_shell(owner) {
            return dbus.name_has_owner(name.into()).await.unwrap_or(false);
        }
        let Ok(credentials) = dbus.get_connection_credentials(name.into()).await else {
            return false;
        };
        let portal =
            zbus::names::BusName::try_from("org.freedesktop.impl.portal.desktop.gnome").unwrap();
        if dbus
            .get_name_owner(portal)
            .await
            .is_ok_and(|current| current.as_str() == owner)
            && installed_portal(&credentials)
        {
            return true;
        }
        remote_desktop_daemon(&dbus, owner, &credentials).await
    }
    pub async fn admit(&self, conn: &Connection, header: &Header<'_>) -> fdo::Result<String> {
        self.unlocked()?;
        let sender = self.authenticate(conn, header).await?;
        self.unlocked()?;
        Ok(sender)
    }
    /// [`Self::admit`] for a call the supervised shell relays from its
    /// `org.gnome.Shell` connection on behalf of `caller` (GNOME Shell owns
    /// both names on one connection; Tuna's shell owns only that one). The
    /// relay must be the shell itself, and `caller`, a unique name the bus
    /// never reuses, passes the same rule as a direct sender, so the second
    /// name widens nobody's access.
    pub async fn admit_forwarded(
        &self,
        conn: &Connection,
        header: &Header<'_>,
        caller: &str,
    ) -> fdo::Result<String> {
        self.unlocked()?;
        let (_, relay) = self.authenticate_caller(conn, header).await?;
        if relay != CaptureCaller::Shell {
            return Err(denied("only the supervised shell forwards calls"));
        }
        let caller =
            UniqueName::try_from(caller).map_err(|_| denied("caller must be a unique bus name"))?;
        self.authenticate_sender(conn, &caller).await?;
        self.unlocked()?;
        Ok(caller.to_string())
    }
    /// RemoteDesktop session creation (and only that, plus the ScreenCast
    /// session linked to it): the capture callers above, or the installed
    /// gnome-remote-desktop daemon. GNOME's grd talks to Mutter directly
    /// once Settings > System > Remote Desktop enabled it; its configured
    /// credentials are the consent, as in GNOME (`grd-session.c`
    /// `grd_session_start` calls `CreateSession` with no portal).
    pub async fn admit_remote(
        &self,
        conn: &Connection,
        header: &Header<'_>,
    ) -> fdo::Result<String> {
        self.unlocked()?;
        let sender = match self.authenticate(conn, header).await {
            Ok(sender) => sender,
            Err(denial) => {
                let sender = header.sender().ok_or_else(|| denied("missing sender"))?;
                let dbus = fdo::DBusProxy::new(conn).await?;
                let credentials = dbus
                    .get_connection_credentials(sender.clone().into())
                    .await?;
                if credentials.unix_user_id() != Some(rustix::process::geteuid().as_raw())
                    || !remote_desktop_daemon(&dbus, sender.as_str(), &credentials).await
                {
                    return Err(denial);
                }
                sender.to_string()
            }
        };
        self.unlocked()?;
        Ok(sender)
    }
    /// InputCapture session creation: the installed portal backend only.
    /// Its InputCapture portal holds the user's consent; GNOME Shell
    /// never captures input this way and grd has its own interface.
    pub async fn admit_portal(
        &self,
        conn: &Connection,
        header: &Header<'_>,
    ) -> fdo::Result<String> {
        self.unlocked()?;
        let (sender, caller) = self.authenticate_caller(conn, header).await?;
        if caller != CaptureCaller::Portal {
            return Err(denied("input capture requires the installed portal"));
        }
        self.unlocked()?;
        Ok(sender)
    }
    /// Authenticate desktop-service identity without authorizing capture.
    /// Screenshot's documented false result can deny an authenticated locked
    /// backend while other capture APIs retain their AccessDenied result.
    pub async fn authenticate(
        &self,
        conn: &Connection,
        header: &Header<'_>,
    ) -> fdo::Result<String> {
        self.authenticate_caller(conn, header)
            .await
            .map(|(sender, _)| sender)
    }
    /// [`Self::authenticate`], also saying which desktop service passed:
    /// the supervised shell stands in for GNOME Shell's in-process
    /// callers, the installed portal for external D-Bus clients.
    pub async fn authenticate_caller(
        &self,
        conn: &Connection,
        header: &Header<'_>,
    ) -> fdo::Result<(String, CaptureCaller)> {
        let sender = header.sender().ok_or_else(|| denied("missing sender"))?;
        let caller = self.authenticate_sender(conn, sender).await?;
        Ok((sender.to_string(), caller))
    }
    /// The shell is a unique name it declared over its pinned control
    /// session; the portal is the current owner of its well-known name
    /// running the installed executable.
    async fn authenticate_sender(
        &self,
        conn: &Connection,
        sender: &UniqueName<'_>,
    ) -> fdo::Result<CaptureCaller> {
        if self.is_shell(sender.as_str()) {
            return Ok(CaptureCaller::Shell);
        }
        let dbus = fdo::DBusProxy::new(conn).await?;
        let name =
            zbus::names::BusName::try_from("org.freedesktop.impl.portal.desktop.gnome").unwrap();
        let owner = dbus
            .get_name_owner(name)
            .await
            .map_err(|_| denied("portal not running"))?;
        if owner.as_str() != sender.as_str() {
            return Err(denied(
                "capture requires the supervised shell or installed portal",
            ));
        }
        let credentials = dbus
            .get_connection_credentials(sender.clone().into())
            .await?;
        if !installed_portal(&credentials) {
            return Err(denied(
                "capture requires the supervised shell or installed portal",
            ));
        }
        Ok(CaptureCaller::Portal)
    }
}
/// Which desktop service passed capture authentication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureCaller {
    /// The supervised Tuna shell (GNOME Shell's in-process role).
    Shell,
    /// The installed xdg-desktop-portal-gnome backend.
    Portal,
}
pub fn owns_session(owner: &str, sender: Option<&str>, revoked: bool) -> bool {
    !revoked && sender.is_some_and(|sender| sender == owner)
}
pub fn denied(reason: &str) -> fdo::Error {
    fdo::Error::AccessDenied(reason.into())
}
fn installed_portal(credentials: &fdo::ConnectionCredentials) -> bool {
    installed_service(credentials, ServiceClient::Portal)
}
/// gnome-remote-desktop's session-bus daemons (`grd-private.h`): the
/// user-mode Desktop Sharing daemon, the remote-login handover daemon and
/// the headless daemon. All three run `gnome-remote-desktop-daemon`.
const REMOTE_DESKTOP_DAEMON_NAMES: [&str; 3] = [
    "org.gnome.RemoteDesktop.User",
    "org.gnome.RemoteDesktop.Handover",
    "org.gnome.RemoteDesktop.Headless",
];
const REMOTE_DESKTOP_DAEMON: [&str; 2] = [
    "/usr/libexec/gnome-remote-desktop-daemon",
    "/usr/lib/gnome-remote-desktop-daemon",
];
/// The caller is the root-owned installed grd executable and currently
/// owns one of grd's well-known names, so neither a copied binary nor a
/// process that merely claims a grd name passes.
async fn remote_desktop_daemon(
    dbus: &fdo::DBusProxy<'_>,
    owner: &str,
    credentials: &fdo::ConnectionCredentials,
) -> bool {
    if !installed_executable(credentials, &REMOTE_DESKTOP_DAEMON) {
        return false;
    }
    for name in REMOTE_DESKTOP_DAEMON_NAMES {
        let name = zbus::names::BusName::try_from(name).unwrap();
        if dbus
            .get_name_owner(name)
            .await
            .is_ok_and(|current| current.as_str() == owner)
        {
            return true;
        }
    }
    false
}
fn installed_service(credentials: &fdo::ConnectionCredentials, role: ServiceClient) -> bool {
    installed_executable(credentials, role.executables())
}
/// Whether the caller runs one of `executables`, a root-owned file no group
/// or other user may write. With a bus `ProcessFD` the PID is read through
/// the pin: the descriptor must name that PID, and its process must still
/// be alive after `/proc/<pid>/exe` was read, so the read cannot have
/// reached a process that took over a recycled PID. A bus without
/// `ProcessFD` (dbus-daemon before 1.15.8) leaves the numeric PID, as
/// before; the same-UID boundary of ADR 0010 already admits a caller that
/// starts the genuine binary itself.
fn installed_executable(credentials: &fdo::ConnectionCredentials, executables: &[&str]) -> bool {
    let Some(pid) = credentials.process_id().filter(|pid| *pid != 0) else {
        return false;
    };
    let Some(pin) = credentials.process_fd() else {
        return runs_executable(pid, executables);
    };
    let pin = pin.as_fd();
    pidfd_pid(pin) == Some(pid)
        && pidfd_live(pin)
        && runs_executable(pid, executables)
        && pidfd_live(pin)
}
fn runs_executable(pid: u32, executables: &[&str]) -> bool {
    let Ok(exe) = std::fs::read_link(format!("/proc/{pid}/exe")) else {
        return false;
    };
    executables.iter().any(|path| {
        let Ok(real) = std::fs::canonicalize(path) else {
            return false;
        };
        let Ok(meta) = real.metadata() else {
            return false;
        };
        real == exe && meta.uid() == 0 && meta.mode() & 0o022 == 0
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_socket_identity_pins_the_connecting_process_and_expires_on_exit() {
        use std::os::unix::net::UnixListener;
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("peer.sock");
        let listener = UnixListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut child = Command::new("/usr/bin/python3")
            .args(["-c", "import socket,sys; s=socket.socket(socket.AF_UNIX); s.connect(sys.argv[1]); sys.stdin.buffer.read(1)"])
            .arg(&path)
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        let end = Instant::now() + Duration::from_secs(5);
        let socket = loop {
            match listener.accept() {
                Ok((socket, _)) => break socket,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < end =>
                {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("native peer did not connect: {error}"),
            }
        };
        let credentials = socket_credentials(&socket).unwrap();
        let original = pinned_process_id(&credentials);
        let pid = child.id();
        child.kill().unwrap();
        child.wait().unwrap();
        assert_ne!(pid, std::process::id());
        assert_eq!(original, Some(pid));
        assert_eq!(
            credentials.unix_user_id(),
            Some(rustix::process::geteuid().as_raw())
        );
        assert_eq!(pinned_process_id(&credentials), None);
    }

    #[test]
    fn native_peer_metadata_does_not_make_the_client_an_ime_or_service() {
        let (socket, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
        let client = crate::ClientState::native_connection(&socket);
        assert!(!client.ime_bridge);
        assert!(client.connection_window_tag.is_none());
        assert!(client.service_alive.is_none());
        assert_eq!(
            pinned_process_id(client.original_credentials.as_ref().unwrap()),
            Some(std::process::id())
        );
    }
    /// The Wayland shell flag and the session-lock identity, as the
    /// runtime's accept path and `take_lock_request` read them. PID reuse:
    /// a connection made by a process that has since exited, whose PID
    /// number is now the held shell child's (this process), is not the
    /// shell. Without `SO_PEERPIDFD` there is no shell at all.
    #[test]
    fn only_a_live_pinned_shell_child_is_the_wayland_shell() {
        use crate::session_lock::{lock_request_verdict, LockVerdict};
        let me = std::process::id();
        let (socket, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
        let mut shell = crate::ClientState::native_connection(&socket);
        shell.admit_supervised_shell(Some(me));
        assert!(shell.supervised_shell);
        assert_eq!(shell.shell_pid(), Some(me));
        assert_eq!(
            lock_request_verdict(shell.shell_pid(), Some(me), true),
            LockVerdict::Approved
        );
        shell.admit_supervised_shell(Some(me + 1));
        assert!(!shell.supervised_shell, "another held child");

        let mut gone = std::process::Command::new("/bin/sleep")
            .arg("60")
            .spawn()
            .unwrap();
        let gone_pin = rustix::process::pidfd_open(
            rustix::process::Pid::from_raw(gone.id() as i32).unwrap(),
            rustix::process::PidfdFlags::empty(),
        )
        .unwrap();
        gone.kill().unwrap();
        gone.wait().unwrap();
        let mut recycled = crate::ClientState {
            original_credentials: Some(Arc::new(numeric(me).set_process_fd(gone_pin.into()))),
            ..Default::default()
        };
        recycled.admit_supervised_shell(Some(me));
        assert!(!recycled.supervised_shell, "recycled PID at accept");
        // Flagged while its process lived, the connection outlives it.
        recycled.supervised_shell = true;
        assert_eq!(recycled.shell_pid(), None);
        assert_eq!(
            lock_request_verdict(recycled.shell_pid(), Some(me), true),
            LockVerdict::RefusedForeign
        );

        // A kernel without SO_PEERPIDFD: only a numeric PID, or nothing.
        let mut numeric_only = crate::ClientState {
            original_credentials: Some(Arc::new(numeric(me))),
            ..Default::default()
        };
        numeric_only.admit_supervised_shell(Some(me));
        assert!(!numeric_only.supervised_shell);
        let mut unknown = crate::ClientState::default();
        unknown.admit_supervised_shell(Some(me));
        assert!(!unknown.supervised_shell);
    }
    #[test]
    fn process_identity_requires_a_pin_and_does_not_survive_peer_exit() {
        let numeric = fdo::ConnectionCredentials::default().set_process_id(std::process::id());
        assert_eq!(pinned_process_id(&numeric), None);
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("60")
            .spawn()
            .unwrap();
        let pid = child.id();
        let descriptor = rustix::process::pidfd_open(
            rustix::process::Pid::from_raw(pid as i32).unwrap(),
            rustix::process::PidfdFlags::empty(),
        )
        .unwrap();
        let pinned = fdo::ConnectionCredentials::default()
            .set_process_id(pid)
            .set_process_fd(descriptor.into());
        let live = pinned_process_id(&pinned);
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(live, Some(pid));
        assert_eq!(pinned_process_id(&pinned), None);
        // A recycled/replaced numeric value cannot revive the old descriptor.
        let replaced = pinned.set_process_id(std::process::id());
        assert_eq!(pinned_process_id(&replaced), None);
    }
    #[test]
    fn lock_admission_fails_closed_and_updates() {
        let authority = Authority::default();
        authority.publish(true, [":1.42"]);
        assert!(matches!(
            authority.unlocked(),
            Err(fdo::Error::AccessDenied(_))
        ));
        authority.publish(false, std::iter::empty());
        assert!(authority.unlocked().is_ok());
        assert!(
            !authority.is_shell(":1.42"),
            "a dropped declaration is gone"
        );
    }
    #[test]
    fn the_shell_is_only_the_unique_names_it_declared() {
        let authority = Authority::default();
        assert!(!authority.is_shell(":1.7"), "nothing declared, nobody");
        authority.publish(false, [":1.7", ":1.9"]);
        assert!(authority.is_shell(":1.7"));
        assert!(authority.is_shell(":1.9"));
        assert!(!authority.is_shell(":1.8"), "another connection");
        assert!(!authority.is_shell("org.gnome.Shell"), "a well-known name");
        assert!(!authority.is_shell(""));
    }
    #[test]
    fn sessions_are_owned_by_the_unique_creator_and_revoke_permanently() {
        assert!(owns_session(":1.1", Some(":1.1"), false));
        assert!(!owns_session(":1.1", Some(":1.2"), false));
        assert!(!owns_session(":1.1", None, false));
        assert!(!owns_session(":1.1", Some(":1.1"), true));
    }
    /// Credentials as a bus without `ProcessFD` reports them.
    fn numeric(pid: u32) -> fdo::ConnectionCredentials {
        fdo::ConnectionCredentials::default().set_process_id(pid)
    }
    /// Credentials as a bus with `ProcessFD` reports them: `pid` beside a
    /// pidfd of the process `pinned` (normally the same process).
    fn pinned(pid: u32, pinned: u32) -> fdo::ConnectionCredentials {
        let descriptor = rustix::process::pidfd_open(
            rustix::process::Pid::from_raw(pinned as i32).unwrap(),
            rustix::process::PidfdFlags::empty(),
        )
        .unwrap();
        numeric(pid).set_process_fd(descriptor.into())
    }
    /// Spawn `program` and return once the child runs it. `spawn` returns
    /// as soon as the child releases this process's memory (glibc
    /// `posix_spawn` is a vfork), which is before `execve` installs the
    /// new image: until then `/proc/<pid>/exe` still names this test
    /// binary, and under load (the child waits on this process's mmap
    /// lock) that window is long enough to read. A close-on-exec socket
    /// end the child inherits is closed only after the new image is in
    /// place, so its peer's EOF marks the exec as done.
    fn spawn_exec(program: impl AsRef<std::ffi::OsStr>, args: &[&str]) -> std::process::Child {
        use std::io::Read;
        let (mut exec_done, inherited) = std::os::unix::net::UnixStream::pair().unwrap();
        let child = std::process::Command::new(program)
            .args(args)
            .spawn()
            .unwrap();
        drop(inherited);
        assert_eq!(exec_done.read(&mut [0u8; 1]).unwrap(), 0, "exec closes it");
        child
    }
    #[test]
    fn remote_desktop_daemon_identity_is_the_root_owned_installed_binary() {
        // This test process, a missing pid and pid 0 are not grd.
        let me = std::process::id();
        assert!(!installed_executable(&numeric(me), &REMOTE_DESKTOP_DAEMON));
        assert!(!installed_executable(
            &pinned(me, me),
            &REMOTE_DESKTOP_DAEMON
        ));
        assert!(!installed_executable(
            &numeric(u32::MAX),
            &REMOTE_DESKTOP_DAEMON
        ));
        assert!(!installed_executable(&numeric(0), &["/proc/self/exe"]));
        assert!(!installed_executable(
            &fdo::ConnectionCredentials::default(),
            &["/proc/self/exe"]
        ));
        // The identity check accepts a root-owned, non-writable binary
        // the process really runs (here coreutils' sleep stands in)...
        let mut sleep = spawn_exec("/bin/sleep", &["60"]);
        assert!(installed_executable(&numeric(sleep.id()), &["/bin/sleep"]));
        assert!(installed_executable(
            &pinned(sleep.id(), sleep.id()),
            &["/bin/sleep"]
        ));
        assert!(!installed_executable(
            &pinned(sleep.id(), sleep.id()),
            &REMOTE_DESKTOP_DAEMON
        ));
        sleep.kill().unwrap();
        sleep.wait().unwrap();
        // ...but not a user-owned copy named like the daemon.
        let directory = tempfile::tempdir().unwrap();
        let copy = directory.path().join("gnome-remote-desktop-daemon");
        // Copy in a separate process: a concurrent test can fork while an
        // in-process copy holds this inode writable, retaining that descriptor
        // until exec and making our immediate exec fail with ETXTBSY.
        assert!(std::process::Command::new("/bin/cp")
            .args(["--", "/bin/sleep"])
            .arg(&copy)
            .status()
            .unwrap()
            .success());
        let mut spoof = spawn_exec(&copy, &["60"]);
        let copy_path = copy.to_str().unwrap();
        let spoofed = pinned(spoof.id(), spoof.id());
        assert!(!installed_executable(&spoofed, &[copy_path]));
        assert!(!installed_executable(&spoofed, &REMOTE_DESKTOP_DAEMON));
        spoof.kill().unwrap();
        spoof.wait().unwrap();
    }
    /// PID reuse: the bus reports a PID number that now belongs to a
    /// genuine root-owned binary, beside the pin of the process that really
    /// connected. The pin refuses it; the numeric PID alone (the check
    /// before this one) admitted it. An exited pin is no identity either.
    #[test]
    fn a_recycled_pid_does_not_inherit_the_installed_executable() {
        let mut caller = spawn_exec("/bin/sleep", &["60"]);
        let mut genuine = spawn_exec("/bin/sleep", &["60"]);
        let recycled = pinned(genuine.id(), caller.id());
        assert!(
            !installed_executable(&recycled, &["/bin/sleep"]),
            "the pin names another process than the reported PID"
        );
        let own = pinned(genuine.id(), genuine.id());
        assert!(installed_executable(&own, &["/bin/sleep"]));
        caller.kill().unwrap();
        caller.wait().unwrap();
        genuine.kill().unwrap();
        genuine.wait().unwrap();
        assert!(
            !installed_executable(&own, &["/bin/sleep"]),
            "an exited pin is no identity, whoever holds its PID now"
        );
    }
    #[test]
    fn arbitrary_process_cannot_be_the_installed_portal() {
        let me = std::process::id();
        assert!(!installed_portal(&numeric(me)));
        assert!(!installed_portal(&pinned(me, me)));
        assert!(!installed_portal(&numeric(u32::MAX)));
    }
    #[test]
    fn typed_service_roles_do_not_accept_an_arbitrary_process() {
        for value in 1..=3 {
            let role = ServiceClient::from_wire(value).unwrap();
            let me = std::process::id();
            assert!(!installed_service(&numeric(me), role));
            assert!(!installed_service(&pinned(me, me), role));
            assert!(!installed_service(&numeric(u32::MAX), role));
        }
        for value in [0, 4, u32::MAX] {
            assert!(matches!(
                ServiceClient::from_wire(value),
                Err(fdo::Error::InvalidArgs(_))
            ));
        }
        assert_eq!(
            ServiceClient::from_wire(2).unwrap(),
            ServiceClient::FileChooser
        );
        assert_eq!(
            ServiceClient::from_wire(3).unwrap(),
            ServiceClient::GlobalShortcuts
        );
        for left in [
            ServiceClient::Portal,
            ServiceClient::FileChooser,
            ServiceClient::GlobalShortcuts,
        ] {
            for right in [
                ServiceClient::Portal,
                ServiceClient::FileChooser,
                ServiceClient::GlobalShortcuts,
            ] {
                if left != right {
                    assert!(left
                        .executables()
                        .iter()
                        .all(|path| !right.executables().contains(path)));
                }
            }
        }
    }
}
