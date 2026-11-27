//! Control-socket peer authentication (#30, #70).
//!
//! The control socket is privileged: a session can focus and close
//! windows, drive the overview, and engage the lock. These cases pin
//! who may hold one: only our user, only the supervised shell once one
//! runs, nobody between restarts, a bounded pending queue, and never a
//! shared directory.

use std::io::{self, Read};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::rc::Rc;
use std::time::{Duration, Instant};

use tuna_compositor::control::{
    is_private_dir, our_uid, AuthOwner, ControlHub, PeerCred, PeerGate, MAX_PENDING_PEERS,
};
use tuna_compositor::state::{StateModel, TokenStore};
use tuna_shell_control::{
    decode_frame, encode_frame, CommandKind, CommandStatus, Message, ProtocolVersion,
};

const SEAT: &str = "seat0";

fn bind() -> (tempfile::TempDir, ControlHub, std::path::PathBuf) {
    let dir = private_tempdir();
    let path = dir.path().join("tuna-test.control");
    let hub = ControlHub::bind(path.clone(), Rc::new(TokenStore::new()), SEAT).unwrap();
    (dir, hub, path)
}

fn hello(stream: &mut UnixStream) {
    use std::io::Write;
    let frame = encode_frame(&Message::Hello {
        version: ProtocolVersion::CURRENT,
    });
    stream.write_all(&frame).unwrap();
}

/// Drive enough rounds for accept, then handshake.
fn rounds(hub: &mut ControlHub, model: &mut StateModel, n: usize) {
    for _ in 0..n {
        hub.poll(model);
    }
}

/// Observe retirement before reconnecting. Dropping one shell handle does not
/// close its socket while another reference is still alive, and nonblocking
/// polls cannot make that last reference close. A persistent leak must fail.
fn wait_for_session_retirement(
    hub: &mut ControlHub,
    model: &mut StateModel,
    timeout: Duration,
) -> io::Result<()> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        hub.poll(model);
        if hub.session_count() == 0 && Instant::now() < deadline {
            return Ok(());
        }
        std::thread::sleep(
            Duration::from_millis(1).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        format!("{} shell session(s) did not retire", hub.session_count()),
    ))
}

/// True when the server closed our end without sending anything.
fn refused(stream: &mut UnixStream) -> bool {
    stream
        .set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();
    let mut buf = [0u8; 16];
    // EOF, or a reset when our unread Hello was discarded with the
    // socket; either way no byte of compositor state arrived.
    match stream.read(&mut buf) {
        Ok(0) => true,
        Err(e) => e.kind() == std::io::ErrorKind::ConnectionReset,
        Ok(_) => false,
    }
}

/// A pidfd pinned to `pid`, as `SO_PEERPIDFD` reports a peer.
fn pin(pid: u32) -> Option<std::sync::Arc<std::os::fd::OwnedFd>> {
    let pid = rustix::process::Pid::from_raw(pid as i32).unwrap();
    let pidfd = rustix::process::pidfd_open(pid, rustix::process::PidfdFlags::empty()).unwrap();
    Some(std::sync::Arc::new(pidfd))
}

fn me() -> PeerCred {
    PeerCred {
        pid: std::process::id(),
        uid: our_uid(),
        pidfd: pin(std::process::id()),
    }
}

#[test]
fn gate_decisions_fail_closed() {
    let uid = our_uid();
    let other_user = PeerCred {
        pid: 42,
        uid: uid.wrapping_add(1),
        pidfd: None,
    };
    assert!(
        !PeerGate::SameUser.admits(Some(&other_user), uid),
        "other uid"
    );
    assert!(!PeerGate::SameUser.admits(None, uid), "unknown creds");
    assert!(PeerGate::SameUser.admits(Some(&me()), uid));
    assert!(PeerGate::Pid(me().pid).admits(Some(&me()), uid));
    assert!(
        !PeerGate::Pid(me().pid + 1).admits(Some(&me()), uid),
        "forged shell"
    );
    assert!(
        !PeerGate::Pid(42).admits(Some(&other_user), uid),
        "pid match, wrong uid"
    );
    assert!(!PeerGate::Closed.admits(Some(&me()), uid));
}

/// Without `SO_PEERPIDFD` (kernels before 6.5) the shell's gate admits
/// nobody: a bare `SO_PEERCRED` PID was recorded at connect time and may
/// since have been recycled onto the shell.
#[test]
fn the_shell_gate_requires_a_pin() {
    let uid = our_uid();
    let numeric = PeerCred {
        pidfd: None,
        ..me()
    };
    assert!(!PeerGate::Pid(numeric.pid).admits(Some(&numeric), uid));
    assert!(
        PeerGate::SameUser.admits(Some(&numeric), uid),
        "an unsupervised hub still checks only the user"
    );
}

/// PID reuse: the peer connected as a process that has since exited, and
/// its PID number now belongs to the shell. A numeric match admitted it
/// before the gate read the pin.
#[test]
fn a_recycled_shell_pid_is_refused() {
    let mut gone = std::process::Command::new("/bin/sleep")
        .arg("60")
        .spawn()
        .unwrap();
    let pidfd = pin(gone.id());
    gone.kill().unwrap();
    gone.wait().unwrap();
    // The recorded PID is the shell's (this process), the pin another's.
    let recycled = PeerCred { pidfd, ..me() };
    assert_eq!(recycled.pinned_pid(), None);
    assert!(!PeerGate::Pid(std::process::id()).admits(Some(&recycled), our_uid()));
}

fn read_message(stream: &mut UnixStream) -> Message {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut prefix = [0u8; 4];
    stream.read_exact(&mut prefix).unwrap();
    let mut frame = prefix.to_vec();
    frame.resize(4 + u32::from_le_bytes(prefix) as usize, 0);
    stream.read_exact(&mut frame[4..]).unwrap();
    decode_frame(&frame).unwrap()
}

/// Send `DeclareBusName` and return the compositor's answer to it.
fn declare(
    hub: &mut ControlHub,
    model: &mut StateModel,
    stream: &mut UnixStream,
    name: &str,
) -> CommandStatus {
    use std::io::Write;
    let frame = encode_frame(&Message::Command {
        id: 77,
        kind: CommandKind::DeclareBusName {
            unique_name: name.into(),
        },
    });
    stream.write_all(&frame).unwrap();
    rounds(hub, model, 3);
    loop {
        if let Message::CommandResult { id: 77, status } = read_message(stream) {
            return status;
        }
    }
}

/// The supervised shell names its session-bus connection over its pinned
/// control session; the hub reports that name only while the session
/// lives, and refuses a declaration from any session that is not the
/// supervised shell's.
#[test]
fn only_the_supervised_shell_declares_its_bus_name() {
    let (_dir, mut hub, path) = bind();
    let mut model = StateModel::new();
    // Unsupervised hub: a same-user peer holds a session but is not the
    // shell, so it cannot name the shell on D-Bus.
    let mut other = UnixStream::connect(&path).unwrap();
    hello(&mut other);
    rounds(&mut hub, &mut model, 3);
    assert_eq!(hub.session_count(), 1);
    assert!(matches!(
        declare(&mut hub, &mut model, &mut other, ":1.5"),
        CommandStatus::Denied { .. }
    ));
    assert_eq!(hub.shell_bus_names().count(), 0);
    drop(other);

    // Supervised: this process is the held shell child.
    hub.set_auth_owner(Some(AuthOwner {
        pid: std::process::id(),
        generation: 1,
    }));
    hub.set_peer_gate(PeerGate::Pid(std::process::id()));
    let mut shell = UnixStream::connect(&path).unwrap();
    hello(&mut shell);
    rounds(&mut hub, &mut model, 3);
    assert_eq!(hub.session_count(), 1);
    assert!(matches!(
        declare(&mut hub, &mut model, &mut shell, "org.gnome.Shell"),
        CommandStatus::Denied { .. }
    ));
    assert_eq!(
        declare(&mut hub, &mut model, &mut shell, ":1.9"),
        CommandStatus::Applied
    );
    assert_eq!(hub.shell_bus_names().collect::<Vec<_>>(), [":1.9"]);
    assert_eq!(
        declare(&mut hub, &mut model, &mut shell, ":1.10"),
        CommandStatus::Applied
    );
    assert_eq!(
        hub.shell_bus_names().collect::<Vec<_>>(),
        [":1.10"],
        "a later declaration replaces the earlier one"
    );
    // A narrowed gate (the shell restarted elsewhere) drops the name at once.
    hub.set_peer_gate(PeerGate::Pid(std::process::id() + 1));
    assert_eq!(hub.shell_bus_names().count(), 0);
    hub.set_peer_gate(PeerGate::Pid(std::process::id()));
    assert_eq!(hub.shell_bus_names().collect::<Vec<_>>(), [":1.10"]);
    // The session ending drops it.
    drop(shell);
    wait_for_session_retirement(&mut hub, &mut model, Duration::from_secs(1)).unwrap();
    assert_eq!(hub.shell_bus_names().count(), 0);
}

#[test]
fn unauthorized_same_user_client_is_refused_before_handshake() {
    let (_dir, mut hub, path) = bind();
    let mut model = StateModel::new();
    // The supervised shell is some other process.
    hub.set_peer_gate(PeerGate::Pid(std::process::id() + 1));
    let mut intruder = UnixStream::connect(&path).unwrap();
    hello(&mut intruder);
    rounds(&mut hub, &mut model, 3);
    assert_eq!(hub.session_count(), 0);
    assert!(hub.refused_count() >= 1);
    assert!(
        refused(&mut intruder),
        "intruder must see EOF, not a snapshot"
    );
}

#[test]
fn closed_gate_admits_nobody() {
    let (_dir, mut hub, path) = bind();
    let mut model = StateModel::new();
    hub.set_peer_gate(PeerGate::Closed);
    let mut client = UnixStream::connect(&path).unwrap();
    hello(&mut client);
    rounds(&mut hub, &mut model, 3);
    assert_eq!(hub.session_count(), 0);
    assert!(refused(&mut client));
}

#[test]
fn supervised_shell_pid_is_admitted_and_reconnects() {
    let (_dir, mut hub, path) = bind();
    let mut model = StateModel::new();
    hub.set_peer_gate(PeerGate::Pid(std::process::id()));
    for _ in 0..2 {
        let mut shell = UnixStream::connect(&path).unwrap();
        hello(&mut shell);
        rounds(&mut hub, &mut model, 3);
        assert_eq!(hub.session_count(), 1, "valid shell (re)connect admitted");
        drop(shell);
        wait_for_session_retirement(&mut hub, &mut model, Duration::from_secs(1))
            .expect("previous shell session must retire before reconnect");
    }
}

#[test]
fn retirement_wait_observes_a_delayed_last_shell_reference() {
    let (_dir, mut hub, path) = bind();
    let mut model = StateModel::new();
    hub.set_peer_gate(PeerGate::Pid(std::process::id()));
    let mut shell = UnixStream::connect(&path).unwrap();
    hello(&mut shell);
    rounds(&mut hub, &mut model, 3);
    assert_eq!(hub.session_count(), 1);

    let duplicate = shell.try_clone().unwrap();
    drop(shell);
    rounds(&mut hub, &mut model, 3);
    assert_eq!(hub.session_count(), 1, "held reference prevents retirement");
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(10));
        drop(duplicate);
    });
    let retired = wait_for_session_retirement(&mut hub, &mut model, Duration::from_secs(1));
    release.join().unwrap();
    retired.expect("last reference closes within the retirement budget");
    assert_eq!(hub.session_count(), 0);

    let mut shell = UnixStream::connect(&path).unwrap();
    hello(&mut shell);
    rounds(&mut hub, &mut model, 3);
    assert_eq!(hub.session_count(), 1, "only the reconnected shell remains");
    drop(shell);
    wait_for_session_retirement(&mut hub, &mut model, Duration::from_secs(1)).unwrap();
}

#[test]
fn retirement_wait_rejects_a_persistent_shell_reference() {
    let (_dir, mut hub, path) = bind();
    let mut model = StateModel::new();
    hub.set_peer_gate(PeerGate::Pid(std::process::id()));
    let mut shell = UnixStream::connect(&path).unwrap();
    hello(&mut shell);
    rounds(&mut hub, &mut model, 3);
    assert_eq!(hub.session_count(), 1);

    let duplicate = shell.try_clone().unwrap();
    drop(shell);
    assert_eq!(
        wait_for_session_retirement(&mut hub, &mut model, Duration::from_millis(20))
            .unwrap_err()
            .kind(),
        io::ErrorKind::TimedOut
    );
    assert_eq!(hub.session_count(), 1, "a held socket is not retired");
    drop(duplicate);
    wait_for_session_retirement(&mut hub, &mut model, Duration::from_secs(1)).unwrap();
    assert_eq!(hub.session_count(), 0);
}

#[test]
fn narrowing_the_gate_evicts_a_live_session() {
    let (_dir, mut hub, path) = bind();
    let mut model = StateModel::new();
    let mut client = UnixStream::connect(&path).unwrap();
    hello(&mut client);
    rounds(&mut hub, &mut model, 3);
    assert_eq!(hub.session_count(), 1);
    // The shell restarted under a new pid: the old peer loses access.
    hub.set_peer_gate(PeerGate::Pid(std::process::id() + 1));
    rounds(&mut hub, &mut model, 1);
    assert_eq!(hub.session_count(), 0);
}

#[test]
fn connection_flood_is_bounded_and_the_shell_still_gets_in() {
    let (_dir, mut hub, path) = bind();
    let mut model = StateModel::new();
    let silent: Vec<UnixStream> = (0..MAX_PENDING_PEERS * 4)
        .map(|_| UnixStream::connect(&path).unwrap())
        .collect();
    hub.poll(&mut model);
    assert!(hub.pending_count() <= MAX_PENDING_PEERS);
    assert!(hub.refused_count() >= (MAX_PENDING_PEERS * 3) as u64);
    // Silent peers age out after their one handshake attempt.
    rounds(&mut hub, &mut model, 3);
    drop(silent);
    let mut shell = UnixStream::connect(&path).unwrap();
    hello(&mut shell);
    rounds(&mut hub, &mut model, 3);
    assert_eq!(hub.session_count(), 1);
}

#[test]
fn bind_refuses_a_shared_directory() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o1777)).unwrap();
    assert!(!is_private_dir(dir.path()));
    let Err(err) = ControlHub::bind(
        dir.path().join("tuna-test.control"),
        Rc::new(TokenStore::new()),
        SEAT,
    ) else {
        panic!("bind in a shared dir must fail");
    };
    assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
}

#[test]
fn socket_file_is_owner_only() {
    let (_dir, _hub, path) = bind();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

/// Temp dir with owner-only permissions: the control socket refuses to
/// bind anywhere less private (#30).
fn private_tempdir() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    dir
}
