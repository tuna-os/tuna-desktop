//! Actual nested runtime, supervised control peer and controlled delayed greetd.
//! This proves outer ownership, not PAM retirement, GTK UI or installed DRM.
use greetd_ipc::{codec::SyncCodec, AuthMessageType, Request, Response};
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tuna_compositor::control::{ControlConn, ControlError};
use tuna_shell_control::{CommandKind, CommandStatus, Message, Secret, CURRENT_VERSION};

const ROLE: &str = "TUNA_AUTH_OWNER_CLIENT";
const LIMIT: Duration = Duration::from_secs(5);
fn wait(mut ready: impl FnMut() -> bool) {
    let end = Instant::now() + LIMIT;
    while !ready() {
        assert!(
            Instant::now() < end,
            "bounded auth fixture observation expired"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn connect() -> ControlConn {
    let path = std::env::var_os("TUNA_CONTROL_SOCKET").unwrap();
    let mut conn = ControlConn::new(UnixStream::connect(path).unwrap()).unwrap();
    conn.write_frame(&Message::Hello {
        version: CURRENT_VERSION,
    })
    .unwrap();
    receive(&mut conn, |m| matches!(m, Message::Snapshot { .. }));
    conn
}
fn receive(conn: &mut ControlConn, wanted: impl FnMut(&Message) -> bool) -> Message {
    receive_until(conn, wanted, Instant::now() + LIMIT)
}
fn receive_until(
    conn: &mut ControlConn,
    mut wanted: impl FnMut(&Message) -> bool,
    end: Instant,
) -> Message {
    loop {
        assert!(Instant::now() < end, "control reply deadline");
        match conn.read_frame() {
            Ok(m) if wanted(&m) => return m,
            Ok(_) => {}
            Err(ControlError::WouldBlock) => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(_) => panic!("owned control peer unexpectedly closed"),
        }
    }
}
fn send(conn: &mut ControlConn, id: u64, kind: CommandKind) {
    conn.write_frame(&Message::Command { id, kind }).unwrap();
}
fn result(conn: &mut ControlConn, id: u64, success: bool) {
    let m = receive(
        conn,
        |m| matches!(m,Message::CommandResult{id:got,..} if *got==id),
    );
    if success {
        assert!(
            matches!(
                m,
                Message::CommandResult {
                    status: CommandStatus::Applied,
                    ..
                }
            ),
            "exact Applied required"
        );
    } else {
        assert!(
            matches!(
                m,
                Message::CommandResult {
                    status: CommandStatus::Denied { .. },
                    ..
                }
            ),
            "exact Denied required"
        );
    }
}
fn state(root: &Path) -> serde_json::Value {
    std::fs::read(root.join("state.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(serde_json::Value::Null)
}
fn client() {
    let root = PathBuf::from(std::env::var_os("TUNA_AUTH_OWNER_ROOT").unwrap());
    wait(|| state(&root)["auth_owner"].is_object());
    let mut a = connect();
    let mut b = connect();
    send(&mut a, 10, CommandKind::Lock);
    result(&mut a, 10, true);
    wait(|| state(&root)["auth_owner"]["epoch"] == 1 && state(&root)["locked"] == true);
    send(&mut a, 11, CommandKind::Lock);
    result(&mut a, 11, true);
    assert_eq!(
        state(&root)["auth_owner"]["epoch"],
        1,
        "redundant manual lock keeps epoch"
    );
    send(
        &mut a,
        1,
        CommandKind::Unlock {
            password: Secret("controlled-only".into()),
        },
    );
    wait(|| root.join("started-1").exists());
    if std::env::var(ROLE).as_deref() == Ok("3") {
        wait(|| root.join("compositor-pid").exists());
        // Keep the real source inode held, replace ONLY our own copied path.
        assert_pam_helper_image(&root, 1);
        std::fs::remove_file(root.join("compositor-copy")).unwrap();
        std::fs::write(
            root.join("compositor-copy"),
            b"replaced non-executable negative control",
        )
        .unwrap();
    }
    send(
        &mut b,
        1,
        CommandKind::Unlock {
            password: Secret("controlled-only".into()),
        },
    );
    result(&mut b, 1, false);
    send(
        &mut a,
        2,
        CommandKind::Unlock {
            password: Secret("controlled-only".into()),
        },
    );
    result(&mut a, 2, false);
    let mut replacement = connect();
    send(
        &mut replacement,
        1,
        CommandKind::Unlock {
            password: Secret("controlled-only".into()),
        },
    );
    result(&mut replacement, 1, false);
    let canceled = Instant::now();
    drop(a);
    if std::env::var(ROLE).as_deref() == Ok("3") {
        let helper: u32 = std::fs::read_to_string(root.join("started-1"))
            .unwrap()
            .parse()
            .unwrap();
        wait(|| {
            !Path::new(&format!("/proc/{helper}")).exists()
                && state(&root)["auth_owner"]["outer_pending"] == false
        });
        assert!(
            canceled.elapsed() < Duration::from_secs(2),
            "origin loss must wake the actual PAM read"
        );
        assert!(
            !root.join("release-1").exists() && !root.join("done-1").exists(),
            "retirement must not require releasing the hung module"
        );
    } else {
        std::fs::write(root.join("release-1"), b"1").unwrap();
        wait(|| {
            root.join("done-1").exists() && state(&root)["auth_owner"]["outer_pending"] == false
        });
    }
    assert_eq!(
        state(&root)["locked"],
        true,
        "closed-origin completion cannot unlock"
    );
    loop {
        match replacement.read_frame() {
            Ok(Message::CommandResult {
                status: CommandStatus::Applied,
                ..
            }) => panic!("old result transferred to replacement"),
            Ok(_) => {}
            Err(ControlError::WouldBlock) => break,
            Err(_) => panic!("replacement closed"),
        }
    }
    if std::env::var(ROLE).as_deref() != Ok("3") {
        std::fs::write(root.join("release-2"), b"1").unwrap();
    }
    send(
        &mut replacement,
        3,
        CommandKind::Unlock {
            password: Secret("controlled-only".into()),
        },
    );
    if std::env::var(ROLE).as_deref() == Ok("3") {
        wait(|| root.join("started-2").exists());
        assert_pam_helper_image(&root, 2);
        std::fs::write(root.join("release-2"), b"1").unwrap();
    }
    result(&mut replacement, 3, true);
    if std::env::var(ROLE).as_deref() == Ok("3") {
        let previous: u32 = std::fs::read_to_string(root.join("started-1"))
            .unwrap()
            .parse()
            .unwrap();
        assert!(
            !PathBuf::from(format!("/proc/{previous}")).exists(),
            "previous PAM leader must be reaped before fresh grant"
        );
        assert_eq!(
            std::fs::read(root.join("compositor-copy")).unwrap(),
            b"replaced non-executable negative control"
        );
    }
    wait(|| state(&root)["locked"] == false && state(&root)["auth_owner"]["epoch"] == 2);
    send(&mut replacement, 12, CommandKind::SetIdleTimeout { ms: 50 });
    result(&mut replacement, 12, true);
    wait(|| state(&root)["locked"] == true && state(&root)["auth_owner"]["epoch"] == 3);
    std::fs::write(root.join("success.json"),serde_json::to_vec(&serde_json::json!({
        "supervised_client_pid":std::process::id(), "manual_epoch":1,"unlock_epoch":2,"idle_epoch":3,
        "concurrent_other_same_id_denied":true,"same_session_excess_denied":true,
        "replacement_same_id_busy_denied":true,
        "late_closed_origin_success_denied":std::env::var(ROLE).as_deref()!=Ok("3"),
        "closed_origin_canceled_retired":std::env::var(ROLE).as_deref()==Ok("3"),
        "fresh_origin_success":true,
        "held_self_path_replaced":std::env::var(ROLE).as_deref()==Ok("3"),
        "origin_loss_wakes_private_pam_without_fixture_release":std::env::var(ROLE).as_deref()==Ok("3"),
        "scope":if std::env::var(ROLE).as_deref()==Ok("3") { "actual main Runtime PAM: stable held image, old origin denied, actual retirement; not hardware/MFA/S3" } else { "actual nested outer-wrapper ownership; no PAM retirement" }
    })).unwrap()).unwrap();
    // Keep the supervised process alive until parent sends graceful termination.
    std::thread::sleep(Duration::from_secs(2));
}
fn epoch_client() {
    let root = PathBuf::from(std::env::var_os("TUNA_AUTH_OWNER_ROOT").unwrap());
    wait(|| state(&root)["auth_owner"].is_object());
    let mut original = connect();
    let mut other = connect();
    send(&mut original, 10, CommandKind::Lock);
    result(&mut original, 10, true);
    wait(|| state(&root)["auth_owner"]["epoch"] == 1 && state(&root)["locked"] == true);
    send(
        &mut original,
        1,
        CommandKind::Unlock {
            password: Secret("controlled-only".into()),
        },
    );
    wait(|| root.join("started-1").exists());
    std::fs::write(root.join("inject-epoch"), b"1").unwrap();
    wait(|| root.join("epoch-injected.json").exists());
    let injected: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("epoch-injected.json")).unwrap()).unwrap();
    assert_eq!(injected["busy_sync_denied"], true);
    send(
        &mut other,
        1,
        CommandKind::Unlock {
            password: Secret("controlled-only".into()),
        },
    );
    result(&mut other, 1, false);
    std::fs::write(root.join("release-1"), b"1").unwrap();
    result(&mut original, 1, false);
    wait(|| state(&root)["auth_owner"]["outer_pending"] == false);
    assert_eq!(state(&root)["locked"], true);
    assert_eq!(state(&root)["auth_owner"]["epoch"], 2);
    drop(original);
    let mut replacement = connect();
    std::fs::write(root.join("release-2"), b"1").unwrap();
    send(
        &mut replacement,
        1,
        CommandKind::Unlock {
            password: Secret("controlled-only".into()),
        },
    );
    result(&mut replacement, 1, true);
    wait(|| state(&root)["locked"] == false && state(&root)["auth_owner"]["epoch"] == 3);
    send(&mut replacement, 11, CommandKind::Lock);
    result(&mut replacement, 11, true);
    wait(|| state(&root)["locked"] == true && state(&root)["auth_owner"]["epoch"] == 4);
    std::fs::write(root.join("release-3"), b"1").unwrap();
    std::fs::write(root.join("try-sync"), b"1").unwrap();
    wait(|| root.join("sync-result.json").exists());
    let synced: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("sync-result.json")).unwrap()).unwrap();
    assert_eq!(synced["applied"], true);
    wait(|| state(&root)["locked"] == false && state(&root)["auth_owner"]["epoch"] == 5);
    std::fs::write(root.join("success.json"),serde_json::to_vec(&serde_json::json!({
        "supervised_client_pid":std::process::id(),"late_epoch_success_denied":true,
        "busy_public_sync_denied":true,"fresh_origin_success":true,"public_sync_success":true,
        "manual_epoch":1,"injected_epoch":2,"fresh_epoch":3,"relock_epoch":4,"sync_epoch":5,
        "scope":"actual runtime controlled epoch injection and public try_unlock; not logind/S3/backend duplicate"
    })).unwrap()).unwrap();
    // Parent admits this live process, copies receipts, then terminates the driver.
    std::thread::sleep(Duration::from_secs(2));
}

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_some() {
            return;
        }
        unsafe {
            libc::kill(self.0.id() as i32, libc::SIGTERM);
        }
        let end = Instant::now() + Duration::from_secs(3);
        while Instant::now() < end {
            if self.0.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
struct Artifacts {
    root: PathBuf,
    out: Option<PathBuf>,
}
impl Drop for Artifacts {
    fn drop(&mut self) {
        if let Some(out) = &self.out {
            let _ = std::fs::create_dir_all(out);
            for name in [
                "success.json",
                "compositor.log",
                "state.json",
                "epoch-injected.json",
                "sync-result.json",
            ] {
                let _ = std::fs::copy(self.root.join(name), out.join(name));
            }
        }
    }
}

fn greetd(listener: UnixListener, root: PathBuf, attempts: usize) {
    listener.set_nonblocking(true).unwrap();
    for n in 1..=attempts {
        let mut connection = None;
        wait(|| match listener.accept() {
            Ok((s, _)) => {
                connection = Some(s);
                true
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => false,
            Err(_) => panic!("greetd fixture accept"),
        });
        let mut s = connection.unwrap();
        s.set_read_timeout(Some(LIMIT)).unwrap();
        s.set_write_timeout(Some(LIMIT)).unwrap();
        assert!(matches!(
            Request::read_from(&mut s).unwrap(),
            Request::CreateSession { username } if Some(username.as_str()) == tuna_compositor::unlock::session_user().as_deref()
        ));
        Response::AuthMessage {
            auth_message_type: AuthMessageType::Secret,
            auth_message: "controlled prompt".into(),
        }
        .write_to(&mut s)
        .unwrap();
        assert!(matches!(
            Request::read_from(&mut s).unwrap(),
            Request::PostAuthMessageResponse { response: Some(value) } if value == "controlled-only"
        ));
        std::fs::write(root.join(format!("started-{n}")), b"1").unwrap();
        wait(|| root.join(format!("release-{n}")).exists());
        Response::Success.write_to(&mut s).unwrap();
        assert!(matches!(
            Request::read_from(&mut s).unwrap(),
            Request::CancelSession
        ));
        Response::Success.write_to(&mut s).unwrap();
        std::fs::write(root.join(format!("done-{n}")), b"1").unwrap();
    }
}
#[test]
fn actual_runtime_owns_delayed_greetd_result_and_lock_epochs() {
    match std::env::var(ROLE).ok().as_deref() {
        Some("1") => return client(),
        Some(_) => return,
        None => {}
    }
    run_parent(
        Path::new(env!("CARGO_BIN_EXE_tuna-compositor")),
        "1",
        2,
        "compositor",
    );
}

#[test]
fn actual_runtime_epoch_invalidation_and_public_sync_unlock() {
    match std::env::var(ROLE).ok().as_deref() {
        Some("2") => return epoch_client(),
        Some(_) => return,
        None => {}
    }
    if std::env::var_os("DISPLAY").is_none() {
        assert!(
            std::env::var_os("TUNA_REQUIRE_AUTH_OWNER_RUNTIME").is_none(),
            "owned display required"
        );
        return;
    }
    let bin = PathBuf::from(
        std::env::var_os("TUNA_AUTH_OWNER_PROBE_BIN").expect("admitted probe example"),
    );
    run_parent(&bin, "2", 3, "probe");
}

fn assert_pam_helper_image(root: &Path, attempt: u32) {
    use std::os::unix::fs::MetadataExt;
    let helper: u32 = std::fs::read_to_string(root.join(format!("started-{attempt}")))
        .unwrap()
        .parse()
        .unwrap();
    let parent: u32 = std::fs::read_to_string(root.join("compositor-pid"))
        .unwrap()
        .parse()
        .unwrap();
    let expected = std::fs::metadata(format!("/proc/{parent}/exe")).unwrap();
    let actual = std::fs::metadata(format!("/proc/{helper}/exe")).unwrap();
    assert_eq!(
        (actual.dev(), actual.ino()),
        (expected.dev(), expected.ino())
    );
    let stat = std::fs::read_to_string(format!("/proc/{helper}/stat")).unwrap();
    let fields: Vec<_> = stat
        .rsplit(')')
        .next()
        .unwrap()
        .split_whitespace()
        .collect();
    assert_eq!(
        fields[1].parse::<u32>().unwrap(),
        parent,
        "owned actual main helper"
    );
}

#[test]
fn actual_main_runtime_private_pam_retirement_and_held_self() {
    match std::env::var(ROLE).ok().as_deref() {
        Some("3") => return client(),
        Some(_) => return,
        None => {}
    }
    run_parent(
        Path::new(env!("CARGO_BIN_EXE_tuna-compositor")),
        "3",
        2,
        "pam",
    );
}

fn run_parent(bin: &Path, role: &str, attempts: usize, kind: &str) {
    if std::env::var_os("DISPLAY").is_none() {
        assert!(
            std::env::var_os("TUNA_REQUIRE_AUTH_OWNER_RUNTIME").is_none(),
            "Xvfb runtime proof required"
        );
        eprintln!(
            "auth-owner runtime display unavailable; actual proof requires dedicated CI step"
        );
        return;
    }
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(tuna_compositor::control::is_private_dir(root.path()));
    let _artifacts = Artifacts {
        root: root.path().to_owned(),
        out: std::env::var_os("TUNA_AUTH_OWNER_ARTIFACTS").map(PathBuf::from),
    };
    let socket = root.path().join("greetd.sock");
    let server = if role == "3" {
        None
    } else {
        let listener = UnixListener::bind(&socket).unwrap();
        let server_root = root.path().to_owned();
        Some(std::thread::spawn(move || {
            greetd(listener, server_root, attempts)
        }))
    };
    let owned_bin = root.path().join("compositor-copy");
    if role == "3" {
        assert!(
            Path::new("/usr/lib/x86_64-linux-gnu/libpam_wrapper.so").exists(),
            "required actual PAM wrapper"
        );
        let module = root.path().join("controlled-pam.so");
        let source =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pam_conversation.c");
        assert!(Command::new("cc")
            .args(["-shared", "-fPIC", "-O2", "-Wall", "-Wextra", "-Werror"])
            .arg(source)
            .arg("-o")
            .arg(&module)
            .arg("-lpam")
            .status()
            .unwrap()
            .success());
        std::fs::create_dir(root.path().join("services")).unwrap();
        std::fs::write(
            root.path().join("services/runtime-delay"),
            format!(
                "auth required {} runtime-delay\naccount required {} runtime-delay\n",
                module.display(),
                module.display()
            ),
        )
        .unwrap();
        std::fs::copy(bin, &owned_bin).unwrap();
    }
    let executable = if role == "3" {
        owned_bin.as_path()
    } else {
        bin
    };
    let sha = Command::new("sha256sum").arg(bin).output().unwrap();
    assert!(sha.status.success());
    println!(
        "auth owner {kind} sha256: {}",
        String::from_utf8_lossy(&sha.stdout)
            .split_whitespace()
            .next()
            .unwrap()
    );
    let log = std::fs::File::create(root.path().join("compositor.log")).unwrap();
    let mut command = Command::new(executable);
    if role == "3" {
        command
            .env_remove("GREETD_SOCK")
            .env("TUNA_PAM_SERVICE", "runtime-delay")
            .env("LD_PRELOAD", "/usr/lib/x86_64-linux-gnu/libpam_wrapper.so")
            .env("PAM_WRAPPER", "1")
            .env("PAM_WRAPPER_SERVICE_DIR", root.path().join("services"));
    } else {
        command.env("GREETD_SOCK", &socket);
    }
    let mut child = OwnedChild(
        command
            .args([
                "--socket",
                "auth-owner-proof",
                "--width",
                "800",
                "--height",
                "600",
                "--shell-bin",
            ])
            .arg(std::env::current_exe().unwrap())
            .env(ROLE, role)
            .env("TUNA_AUTH_OWNER_TEST_BIN", std::env::current_exe().unwrap())
            .env("TUNA_AUTH_OWNER_ROOT", root.path())
            .env("XDG_RUNTIME_DIR", root.path())
            .env("TUNA_COMPOSITOR_STATE", root.path().join("state.json"))
            .env("TUNA_LOCK_TRACE", "1")
            .env("TUNA_XWAYLAND", "0")
            .env_remove("WAYLAND_DISPLAY")
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .unwrap(),
    );
    std::fs::write(root.path().join("compositor-pid"), child.0.id().to_string()).unwrap();
    let end = Instant::now() + Duration::from_secs(30);
    while !root.path().join("success.json").exists() {
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "actual compositor exited before proof"
        );
        assert!(Instant::now() < end, "actual runtime proof deadline");
        std::thread::sleep(Duration::from_millis(10));
    }
    let receipt_path = root.path().join("success.json");
    let mut receipt: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&receipt_path).unwrap()).unwrap();
    let driver_pid = receipt["supervised_client_pid"].as_u64().unwrap();
    let driver_stat = std::fs::read_to_string(format!("/proc/{driver_pid}/stat")).unwrap();
    let fields: Vec<_> = driver_stat
        .rsplit(')')
        .next()
        .unwrap()
        .split_whitespace()
        .collect();
    assert_eq!(
        fields[1].parse::<u32>().unwrap(),
        child.0.id(),
        "driver is actual supervised child"
    );
    let expected = std::fs::metadata(std::env::current_exe().unwrap()).unwrap();
    let actual = std::fs::metadata(format!("/proc/{driver_pid}/exe")).unwrap();
    use std::os::unix::fs::MetadataExt;
    assert_eq!(
        (expected.dev(), expected.ino()),
        (actual.dev(), actual.ino()),
        "admitted driver executable"
    );
    receipt["client_start_ticks"] = serde_json::json!(fields[19].parse::<u64>().unwrap());
    receipt["compositor_pid"] = serde_json::json!(child.0.id());
    let actual_sha = Command::new("sha256sum")
        .arg(format!("/proc/{}/exe", child.0.id()))
        .output()
        .unwrap();
    assert!(actual_sha.status.success());
    let actual_digest = String::from_utf8_lossy(&actual_sha.stdout)
        .split_whitespace()
        .next()
        .unwrap()
        .to_owned();
    let source_digest = String::from_utf8_lossy(&sha.stdout)
        .split_whitespace()
        .next()
        .unwrap()
        .to_owned();
    assert_eq!(actual_digest, source_digest, "actual live source image");
    receipt["compositor_sha256"] = serde_json::json!(actual_digest);
    std::fs::write(&receipt_path, serde_json::to_vec(&receipt).unwrap()).unwrap();
    println!(
        "auth owner {kind} runtime receipt: {}",
        std::fs::read_to_string(root.path().join("success.json")).unwrap()
    );
    if let Some(out) = std::env::var_os("TUNA_AUTH_OWNER_ARTIFACTS") {
        let out = PathBuf::from(out);
        std::fs::create_dir_all(&out).unwrap();
        for name in [
            "success.json",
            "compositor.log",
            "state.json",
            "epoch-injected.json",
            "sync-result.json",
        ] {
            if root.path().join(name).is_file() {
                std::fs::copy(root.path().join(name), out.join(name)).unwrap();
            }
        }
    }
    drop(child);
    if let Some(server) = server {
        server.join().unwrap();
    }
}

#[test]
fn chatty_control_peer_cannot_extend_absolute_receive_deadline() {
    if std::env::var_os(ROLE).is_some() {
        return;
    }
    use std::cell::Cell;
    use std::io::Write;
    let (receiver, mut sender) = UnixStream::pair().unwrap();
    let frame = tuna_shell_control::encode_frame(&Message::Hello {
        version: CURRENT_VERSION,
    });
    sender.write_all(&frame.repeat(32)).unwrap();
    let mut conn = ControlConn::new(receiver).unwrap();
    let observed = Cell::new(0u8);
    let denied = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        receive_until(
            &mut conn,
            |_| {
                observed.set(observed.get() + 1);
                assert_eq!(
                    observed.get(),
                    1,
                    "chatty overrun: second unread frame reached after deadline"
                );
                // Keep many complete frames ready throughout the elapsed deadline.
                std::thread::sleep(Duration::from_millis(20));
                false
            },
            Instant::now() + Duration::from_millis(5),
        )
    }))
    .expect_err("chatty frames cannot satisfy or extend the deadline");
    let message = denied
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| denied.downcast_ref::<String>().map(String::as_str));
    assert_eq!(message, Some("control reply deadline"));
    assert!(
        observed.get() <= 1,
        "deadline applies to successful unrelated frame reads too"
    );
}

// Stage B: actual Runtime+ControlHub+private PAM, with a separately labelled
// trusted Rust input seam. This is not GTK/native seat/installed policy proof.
#[test]
fn actual_interactive_runtime_private_pam_batches_and_retirement() {
    match std::env::var(ROLE).ok().as_deref() {
        Some("4") => return interactive_client(),
        Some(_) => return,
        None => {}
    }
    if std::env::var_os("DISPLAY").is_none() {
        assert!(
            std::env::var_os("TUNA_REQUIRE_AUTH_OWNER_RUNTIME").is_none(),
            "dedicated Xvfb proof required"
        );
        return;
    }
    let probe =
        PathBuf::from(std::env::var_os("TUNA_AUTH_OWNER_PROBE_BIN").expect("admitted example"));
    let helper = Path::new(env!("CARGO_BIN_EXE_tuna-compositor"));
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let _artifacts = Artifacts {
        root: root.path().to_owned(),
        out: std::env::var_os("TUNA_AUTH_OWNER_ARTIFACTS").map(PathBuf::from),
    };
    let module = root.path().join("controlled-pam.so");
    assert!(Command::new("cc")
        .args(["-shared", "-fPIC", "-O2", "-Wall", "-Wextra", "-Werror"])
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pam_conversation.c"))
        .arg("-o")
        .arg(&module)
        .arg("-lpam")
        .status()
        .unwrap()
        .success());
    std::fs::create_dir(root.path().join("services")).unwrap();
    std::fs::write(
        root.path().join("services/tuna-lock"),
        format!(
            "auth required {} multi\naccount required {} multi\n",
            module.display(),
            module.display()
        ),
    )
    .unwrap();
    std::fs::write(
        root.path().join("module-path"),
        module.as_os_str().as_encoded_bytes(),
    )
    .unwrap();
    let log = std::fs::File::create(root.path().join("runtime.log")).unwrap();
    let mut child = OwnedChild(
        Command::new(&probe)
            .env("TUNA_STAGEB_PROBE", "1")
            .env("TUNA_STAGEB_HELPER", helper)
            .env("TUNA_AUTH_OWNER_ROOT", root.path())
            .env("TUNA_AUTH_OWNER_TEST_BIN", std::env::current_exe().unwrap())
            .env(ROLE, "4")
            .env("XDG_RUNTIME_DIR", root.path())
            .env("TUNA_XWAYLAND", "0")
            .env_remove("GREETD_SOCK")
            .env_remove("WAYLAND_DISPLAY")
            .env("LD_PRELOAD", "/usr/lib/x86_64-linux-gnu/libpam_wrapper.so")
            .env("PAM_WRAPPER", "1")
            .env("PAM_WRAPPER_SERVICE_DIR", root.path().join("services"))
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .unwrap(),
    );
    use std::os::unix::fs::MetadataExt;
    let expected_probe = std::fs::metadata(&probe).unwrap();
    let actual_probe = std::fs::metadata(format!("/proc/{}/exe", child.0.id())).unwrap();
    assert_eq!(
        (expected_probe.dev(), expected_probe.ino()),
        (actual_probe.dev(), actual_probe.ino()),
        "admitted real Runtime example"
    );
    let end = Instant::now() + Duration::from_secs(40);
    loop {
        let ready = root.path().join("success.json").exists();
        assert!(
            Instant::now() < end,
            "absolute interactive fixture deadline"
        );
        if ready {
            break;
        }
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "actual Runtime exited early"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let receipt: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.path().join("success.json")).unwrap()).unwrap();
    let pid = receipt["supervised_client_pid"].as_u64().unwrap();
    let actual = std::fs::metadata(format!("/proc/{pid}/exe")).unwrap();
    let expected = std::fs::metadata(std::env::current_exe().unwrap()).unwrap();
    assert_eq!(
        (actual.dev(), actual.ino()),
        (expected.dev(), expected.ino())
    );
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let fields: Vec<_> = stat
        .rsplit(')')
        .next()
        .unwrap()
        .split_whitespace()
        .collect();
    assert_eq!(
        fields[1].parse::<u32>().unwrap(),
        child.0.id(),
        "actual supervised actor parent"
    );
    for name in [
        "mixed_styles",
        "separate_pin",
        "no_input_denied",
        "foreign_denied",
        "replay_denied",
        "cancel_retired",
        "epoch_denied",
        "backend_phase_timeout",
        "tagged_fingerprint_denied",
    ] {
        assert_eq!(receipt[name], true, "missing actual phase {name}");
    }
    std::fs::write(root.path().join("stop-probe"), b"1").unwrap();
    let end = Instant::now() + Duration::from_secs(3);
    loop {
        let status = child.0.try_wait().unwrap();
        assert!(Instant::now() < end, "owned Runtime retirement deadline");
        if let Some(status) = status {
            assert!(status.success());
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    println!("interactive actual Runtime receipt: {}", receipt);
}

fn interactive_client() {
    use tuna_shell_control::auth::{Answer, AnswerText, Key, Method, Phase, Style};
    let root = PathBuf::from(std::env::var_os("TUNA_AUTH_OWNER_ROOT").unwrap());
    let end = Instant::now() + Duration::from_secs(35);
    let observe = |predicate: &mut dyn FnMut() -> bool| loop {
        let ready = predicate();
        assert!(
            Instant::now() < end,
            "interactive absolute observation deadline"
        );
        if ready {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let receive = |conn: &mut ControlConn, predicate: &mut dyn FnMut(&Message) -> bool| loop {
        let message = conn.read_frame();
        assert!(
            Instant::now() < end,
            "interactive absolute message deadline"
        );
        match message {
            Ok(message) => {
                let accepted = predicate(&message);
                assert!(Instant::now() < end);
                if accepted {
                    return message;
                }
            }
            Err(ControlError::WouldBlock) => std::thread::sleep(Duration::from_millis(5)),
            Err(error) => panic!("actual owned socket failed: {error:?}"),
        }
    };
    let reply = |conn: &mut ControlConn, id: u64, success: bool| {
        let message = receive(
            conn,
            &mut |message| matches!(message, Message::CommandResult { id: got, .. } if *got == id),
        );
        assert!(
            matches!(
                message,
                Message::CommandResult {
                    status: CommandStatus::Applied,
                    ..
                }
            ) == success
        );
    };
    let pending_empty =
        || std::fs::read(root.join("interactive-pending")).is_ok_and(|bytes| bytes == b"0");
    observe(&mut || root.join("profile-admitted").exists());
    let mut a = connect();
    let mut b = connect();
    send(&mut a, 100, CommandKind::Lock);
    reply(&mut a, 100, true);
    send(
        &mut a,
        1,
        CommandKind::AuthStart {
            method: Method::SessionStack,
        },
    );
    let first = receive(&mut a, &mut |m| {
        matches!(m, Message::AuthChallenge { sequence: 1, .. })
    });
    let Message::AuthChallenge {
        key,
        sequence,
        messages,
    } = first
    else {
        panic!()
    };
    assert_eq!(
        messages.iter().map(|m| m.style).collect::<Vec<_>>(),
        [Style::Visible, Style::Secret, Style::Info, Style::Error]
    );
    let values = || {
        vec![
            Answer {
                index: 0,
                text: Some(AnswerText("visible-answer".into())),
            },
            Answer {
                index: 1,
                text: Some(AnswerText("first-secret".into())),
            },
            Answer {
                index: 2,
                text: None,
            },
            Answer {
                index: 3,
                text: None,
            },
        ]
    };
    let answer = |conn: &mut ControlConn, id, key: Key, sequence, values| {
        let frame =
            tuna_shell_control::auth::encode_answer_frame(id, key, sequence, values).unwrap();
        // The proof client uses the same dedicated encoding and owns one write;
        // transport/NoReplay UI implementation is Stage C, not qualified here.
        conn.write_frame(&tuna_shell_control::decode_frame(frame.as_bytes()).unwrap())
            .unwrap();
    };
    answer(&mut a, 2, key, sequence, values());
    reply(&mut a, 2, false);
    answer(&mut b, 3, key, sequence, values());
    reply(&mut b, 3, false);
    answer(
        &mut a,
        4,
        Key {
            epoch: key.epoch + 1,
            ..key
        },
        sequence,
        values(),
    );
    reply(&mut a, 4, false);
    answer(&mut a, 5, key, sequence + 1, values());
    reply(&mut a, 5, false);
    send(
        &mut b,
        6,
        CommandKind::AuthStart {
            method: Method::SessionStack,
        },
    );
    reply(&mut b, 6, false);
    std::fs::write(root.join("input-1"), b"1").unwrap();
    observe(&mut || root.join("input-admitted-1").exists());
    let mut invalid = values();
    invalid[2].text = Some(AnswerText("info must be None".into()));
    answer(&mut a, 7, key, sequence, invalid);
    reply(&mut a, 7, false);
    answer(&mut a, 8, key, sequence, values());
    reply(&mut a, 8, true);
    let second = receive(&mut a, &mut |m| {
        matches!(m, Message::AuthChallenge { sequence: 2, .. })
    });
    let Message::AuthChallenge {
        key: second_key,
        messages,
        ..
    } = second
    else {
        panic!()
    };
    assert_eq!(key, second_key);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].style, Style::Secret);
    answer(&mut a, 9, key, 1, values());
    reply(&mut a, 9, false);
    std::fs::write(root.join("input-2"), b"1").unwrap();
    observe(&mut || root.join("input-admitted-2").exists());
    answer(
        &mut a,
        10,
        key,
        2,
        vec![Answer {
            index: 0,
            text: Some(AnswerText("pin-secret".into())),
        }],
    );
    reply(&mut a, 10, true);
    reply(&mut a, 1, true);
    receive(&mut a, &mut |m| {
        matches!(m, Message::Snapshot { locked: false, .. })
    });
    observe(&mut || pending_empty());
    send(&mut a, 101, CommandKind::Lock);
    reply(&mut a, 101, true);
    send(
        &mut a,
        11,
        CommandKind::AuthStart {
            method: Method::SessionStack,
        },
    );
    let Message::AuthChallenge {
        key: cancel_key, ..
    } = receive(&mut a, &mut |m| matches!(m, Message::AuthChallenge { .. }))
    else {
        panic!()
    };
    send(
        &mut a,
        12,
        CommandKind::AuthCancel {
            start_request: 11,
            key: Some(cancel_key),
        },
    );
    reply(&mut a, 11, false);
    reply(&mut a, 12, true);
    observe(&mut || pending_empty());
    send(
        &mut a,
        13,
        CommandKind::AuthStart {
            method: Method::SessionStack,
        },
    );
    let Message::AuthChallenge { key: epoch_key, .. } =
        receive(&mut a, &mut |m| matches!(m, Message::AuthChallenge { .. }))
    else {
        panic!()
    };
    std::fs::write(root.join("stageb-epoch"), b"1").unwrap();
    observe(&mut || root.join("stageb-epoch-done").exists());
    reply(&mut a, 13, false);
    answer(&mut a, 14, epoch_key, 1, values());
    reply(&mut a, 14, false);
    observe(&mut || pending_empty());
    let module = std::fs::read_to_string(root.join("module-path")).unwrap();
    std::fs::write(
        root.join("services/tuna-lock"),
        format!("auth required {module} fingerprint\naccount required {module} fingerprint\n"),
    )
    .unwrap();
    send(
        &mut a,
        15,
        CommandKind::AuthStart {
            method: Method::SessionStack,
        },
    );
    let tagged = receive(&mut a, &mut |message| {
        assert!(
            !matches!(message, Message::AuthChallenge { key, .. } if key.start_request == 15),
            "tagged hardware prompt must not become a typed challenge"
        );
        matches!(message, Message::CommandResult { id: 15, .. })
    });
    assert!(matches!(
        tagged,
        Message::CommandResult {
            status: CommandStatus::Denied { .. },
            ..
        }
    ));
    observe(&mut || pending_empty());
    std::fs::write(
        root.join("services/tuna-lock"),
        format!("auth required {module} hang\naccount required {module} hang\n"),
    )
    .unwrap();
    std::fs::write(root.join("short-phase"), b"1").unwrap();
    observe(&mut || root.join("short-phase-admitted").exists());
    let started = Instant::now();
    send(
        &mut a,
        16,
        CommandKind::AuthStart {
            method: Method::SessionStack,
        },
    );
    receive(&mut a, &mut |m| {
        matches!(
            m,
            Message::AuthState {
                start_request: 16,
                phase: Phase::Denied,
                ..
            }
        )
    });
    reply(&mut a, 16, false);
    assert!(started.elapsed() < Duration::from_secs(3));
    observe(&mut || pending_empty());
    std::fs::write(root.join("success.json"), serde_json::to_vec(&serde_json::json!({
        "supervised_client_pid": std::process::id(), "mixed_styles": true, "separate_pin": true,
        "no_input_denied": true, "foreign_denied": true, "replay_denied": true,
        "cancel_retired": true, "epoch_denied": true, "backend_phase_timeout": true, "tagged_fingerprint_denied": true,
        "scope": "actual Runtime, sockets, private PAM; controlled input/epoch only; no GTK/native seat/installed policy"
    })).unwrap()).unwrap();
    observe(&mut || root.join("stop-probe").exists());
}
