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
fn publish_json(path: &Path, receipt: &serde_json::Value) {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_vec(receipt).unwrap()).unwrap();
    std::fs::rename(tmp, path).unwrap();
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
    std::fs::write(root.join("admitted-1"), b"1").unwrap();
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
    std::fs::write(root.join("admitted-2"), b"1").unwrap();
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
    publish_json(&root.join("success.json"), &serde_json::json!({
        "supervised_client_pid":std::process::id(), "manual_epoch":1,"unlock_epoch":2,"idle_epoch":3,
        "concurrent_other_same_id_denied":true,"same_session_excess_denied":true,
        "replacement_same_id_busy_denied":true,
        "late_closed_origin_success_denied":std::env::var(ROLE).as_deref()!=Ok("3"),
        "closed_origin_canceled_retired":std::env::var(ROLE).as_deref()==Ok("3"),
        "fresh_origin_success":true,
        "held_self_path_replaced":std::env::var(ROLE).as_deref()==Ok("3"),
        "origin_loss_wakes_private_pam_without_fixture_release":std::env::var(ROLE).as_deref()==Ok("3"),
        "scope":if std::env::var(ROLE).as_deref()==Ok("3") { "actual main Runtime PAM: stable held image, old origin denied, actual retirement; not hardware/MFA/S3" } else { "actual nested outer-wrapper ownership; no PAM retirement" }
    }));
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
    std::fs::write(root.join("admitted-1"), b"1").unwrap();
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
    std::fs::write(root.join("admitted-2"), b"1").unwrap();
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
    std::fs::write(root.join("admitted-3"), b"1").unwrap();
    std::fs::write(root.join("try-sync"), b"1").unwrap();
    wait(|| root.join("sync-result.json").exists());
    let synced: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("sync-result.json")).unwrap()).unwrap();
    assert_eq!(synced["applied"], true);
    wait(|| state(&root)["locked"] == false && state(&root)["auth_owner"]["epoch"] == 5);
    publish_json(&root.join("success.json"), &serde_json::json!({
        "supervised_client_pid":std::process::id(),"late_epoch_success_denied":true,
        "busy_public_sync_denied":true,"fresh_origin_success":true,"public_sync_success":true,
        "manual_epoch":1,"injected_epoch":2,"fresh_epoch":3,"relock_epoch":4,"sync_epoch":5,
        "scope":"actual runtime controlled epoch injection and public try_unlock; not logind/S3/backend duplicate"
    }));
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

fn greetd(listener: UnixListener, root: PathBuf, attempts: usize, deadline: Instant) {
    listener.set_nonblocking(true).unwrap();
    for n in 1..=attempts {
        // Startup and time between attempts belong to the total parent deadline,
        // not the backend's per-attempt budget. Only the supervised client can
        // admit the next attempt in this private fixture directory.
        while !root.join(format!("admitted-{n}")).exists() {
            assert!(Instant::now() < deadline, "greetd admission deadline");
            std::thread::sleep(Duration::from_millis(5));
        }
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
fn greetd_accept_budget_waits_for_client_admission() {
    if std::env::var_os(ROLE).is_some() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let socket = root.path().join("greetd.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server_root = root.path().to_owned();
    let server = std::thread::spawn(move || {
        greetd(listener, server_root, 1, Instant::now() + Duration::from_secs(30))
    });
    // Model compositor startup longer than the unchanged backend accept budget.
    std::thread::sleep(LIMIT + Duration::from_millis(50));
    std::fs::write(root.path().join("admitted-1"), b"1").unwrap();
    let mut connection = UnixStream::connect(socket).unwrap();
    connection.set_read_timeout(Some(LIMIT)).unwrap();
    connection.set_write_timeout(Some(LIMIT)).unwrap();
    Request::CreateSession {
        username: tuna_compositor::unlock::session_user().unwrap(),
    }
    .write_to(&mut connection)
    .unwrap();
    assert!(matches!(
        Response::read_from(&mut connection).unwrap(),
        Response::AuthMessage {
            auth_message_type: AuthMessageType::Secret,
            ..
        }
    ));
    Request::PostAuthMessageResponse {
        response: Some("controlled-only".into()),
    }
    .write_to(&mut connection)
    .unwrap();
    wait(|| root.path().join("started-1").exists());
    std::fs::write(root.path().join("release-1"), b"1").unwrap();
    assert!(matches!(
        Response::read_from(&mut connection).unwrap(),
        Response::Success
    ));
    Request::CancelSession.write_to(&mut connection).unwrap();
    assert!(matches!(
        Response::read_from(&mut connection).unwrap(),
        Response::Success
    ));
    server.join().unwrap();
    assert!(root.path().join("done-1").exists());
}

#[test]
fn published_receipt_is_complete_on_creation_and_replacement() {
    if std::env::var_os(ROLE).is_some() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("success.json");
    let reader_path = path.clone();
    let reader = std::thread::spawn(move || {
        wait(|| reader_path.exists());
        let end = Instant::now() + LIMIT;
        loop {
            assert!(Instant::now() < end, "receipt publication deadline");
            let receipt: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&reader_path).unwrap()).unwrap();
            assert_eq!(receipt["payload"].as_str().unwrap().len(), 65536);
            if receipt["generation"] == 32 {
                break;
            }
        }
    });
    for generation in 0..=32 {
        publish_json(
            &path,
            &serde_json::json!({"generation":generation,"payload":"x".repeat(65536)}),
        );
    }
    reader.join().unwrap();
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
    let end = Instant::now() + Duration::from_secs(30);
    let socket = root.path().join("greetd.sock");
    let server = if role == "3" {
        None
    } else {
        let listener = UnixListener::bind(&socket).unwrap();
        let server_root = root.path().to_owned();
        Some(std::thread::spawn(move || {
            greetd(listener, server_root, attempts, end)
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
    publish_json(&receipt_path, &receipt);
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
