//! Actual runtimes, supervised control peer and a controlled delayed PAM
//! module (libpam_wrapper): outer ownership, lock epochs and private PAM
//! helper retirement. Not GTK UI, installed DRM, logind or S3.
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, PermissionsExt};
use std::os::unix::net::UnixStream;
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
    let display = std::env::var_os("WAYLAND_DISPLAY").expect("supervised private display");
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").unwrap();
    assert!(
        std::fs::metadata(Path::new(&runtime).join(display))
            .unwrap()
            .file_type()
            .is_socket(),
        "supervised client must receive the private Wayland socket"
    );
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
    send(
        &mut a,
        1,
        CommandKind::Unlock {
            password: Secret("controlled-only".into()),
        },
    );
    wait(|| root.join("started-1").exists());
    wait(|| root.join("compositor-pid").exists());
    // Keep the real source inode held, replace ONLY our own copied path.
    assert_pam_helper_image(&root, 1);
    std::fs::remove_file(root.join("compositor-copy")).unwrap();
    std::fs::write(
        root.join("compositor-copy"),
        b"replaced non-executable negative control",
    )
    .unwrap();
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
    assert_helper_retired_unreleased(&root, 1);
    assert!(
        canceled.elapsed() < Duration::from_secs(2),
        "origin loss must wake the actual PAM read"
    );
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
    send(
        &mut replacement,
        3,
        CommandKind::Unlock {
            password: Secret("controlled-only".into()),
        },
    );
    wait(|| root.join("started-2").exists());
    assert_pam_helper_image(&root, 2);
    std::fs::write(root.join("release-2"), b"1").unwrap();
    result(&mut replacement, 3, true);
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
    wait(|| state(&root)["locked"] == false && state(&root)["auth_owner"]["epoch"] == 2);
    send(&mut replacement, 12, CommandKind::SetIdleTimeout { ms: 50 });
    result(&mut replacement, 12, true);
    wait(|| state(&root)["locked"] == true && state(&root)["auth_owner"]["epoch"] == 3);
    publish_json(
        &root.join("success.json"),
        &serde_json::json!({
            "supervised_client_pid":std::process::id(), "manual_epoch":1,"unlock_epoch":2,"idle_epoch":3,
            "concurrent_other_same_id_denied":true,"same_session_excess_denied":true,
            "replacement_same_id_busy_denied":true,
            "closed_origin_canceled_retired":true,
            "fresh_origin_success":true,
            "held_self_path_replaced":true,
            "origin_loss_wakes_private_pam_without_fixture_release":true,
            "scope":"actual main Runtime PAM: stable held image, old origin denied, actual retirement; not hardware/MFA/S3"
        }),
    );
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
    wait(|| root.join("compositor-pid").exists());
    assert_pam_helper_image(&root, 1);
    // The held module keeps the outer slot busy for every other origin.
    send(
        &mut other,
        1,
        CommandKind::Unlock {
            password: Secret("controlled-only".into()),
        },
    );
    result(&mut other, 1, false);
    std::fs::write(root.join("inject-epoch"), b"1").unwrap();
    wait(|| root.join("epoch-injected.json").exists());
    let injected: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("epoch-injected.json")).unwrap()).unwrap();
    assert_eq!(injected["busy_sync_denied"], true);
    // The stale epoch denies the in-flight attempt and retires its helper
    // without the module ever being released to succeed.
    result(&mut original, 1, false);
    assert_helper_retired_unreleased(&root, 1);
    // The control reply can overtake the state file, which the runtime
    // republishes on its own tick (a stale file still shows epoch 1 and no
    // pending attempt): wait for the post-injection publication.
    assert_eq!(injected["after"], 2);
    wait(|| {
        let state = state(&root);
        state["locked"] == true
            && state["auth_owner"]["epoch"] == 2
            && state["auth_owner"]["outer_pending"] == false
    });
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
    publish_json(
        &root.join("success.json"),
        &serde_json::json!({
            "supervised_client_pid":std::process::id(),"stale_epoch_attempt_denied_retired":true,
            "busy_other_origin_denied":true,
            "busy_public_sync_denied":true,"fresh_origin_success":true,"public_sync_success":true,
            "manual_epoch":1,"injected_epoch":2,"fresh_epoch":3,"relock_epoch":4,"sync_epoch":5,
            "scope":"actual runtime PAM: controlled epoch injection and public try_unlock; not logind/S3/backend duplicate"
        }),
    );
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
    run_parent(&bin, "2", "probe");
}

/// The next conversation step for `request`; its result may not come first.
fn auth_event(conn: &mut ControlConn, request: u64) -> tuna_shell_control::AuthEvent {
    match receive(conn, |m| {
        matches!(m, Message::AuthEvent { request: got, .. } if *got == request)
            || matches!(m, Message::CommandResult { id, .. } if *id == request)
    }) {
        Message::AuthEvent { event, .. } => event,
        other => panic!("conversation {request} ended early: {other:?}"),
    }
}

fn query(conn: &mut ControlConn, request: u64, sequence: u32, question: &str, secret: bool) {
    assert_eq!(
        auth_event(conn, request),
        tuna_shell_control::AuthEvent::Query {
            sequence,
            question: question.into(),
            secret
        }
    );
}

fn answer(conn: &mut ControlConn, id: u64, sequence: u32, text: &str) {
    send(
        conn,
        id,
        CommandKind::AnswerQuery {
            service: tuna_shell_control::AuthService::Password,
            sequence,
            answer: Secret(text.into()),
        },
    );
    result(conn, id, true);
}

fn begin(conn: &mut ControlConn, id: u64, service: tuna_shell_control::AuthService) {
    send(conn, id, CommandKind::BeginVerification { service });
}

/// The interactive relay against the controlled multi-prompt PAM module:
/// a visible question, a distinct secret, an info and an error message in
/// one PAM batch, then a separate PIN; wrong answers, a stale sequence and
/// a cancel keep the session locked; only fresh correct answers unlock.
fn conversation_client() {
    use tuna_shell_control::{AuthEvent, AuthService};
    let root = PathBuf::from(std::env::var_os("TUNA_AUTH_OWNER_ROOT").unwrap());
    wait(|| state(&root)["auth_owner"].is_object());
    let mut a = connect();
    send(&mut a, 10, CommandKind::Lock);
    result(&mut a, 10, true);
    wait(|| state(&root)["locked"] == true);
    // No reader: GDM's service-unavailable, then the denied result.
    begin(&mut a, 11, AuthService::Fingerprint);
    assert_eq!(auth_event(&mut a, 11), AuthEvent::Unavailable);
    result(&mut a, 11, false);
    // First conversation: a wrong PIN after correct first answers.
    begin(&mut a, 20, AuthService::Password);
    query(&mut a, 20, 1, "Visible account challenge", false);
    // A second password conversation on the same connection is refused.
    begin(&mut a, 21, AuthService::Password);
    result(&mut a, 21, false);
    // A stale sequence is dropped, never applied to the open question.
    answer(&mut a, 22, 2, "visible-answer");
    answer(&mut a, 23, 1, "visible-answer");
    query(&mut a, 20, 2, "First secret challenge", true);
    answer(&mut a, 24, 2, "first-secret");
    assert_eq!(
        auth_event(&mut a, 20),
        AuthEvent::Info {
            text: "Information from controlled stack".into()
        }
    );
    assert_eq!(
        auth_event(&mut a, 20),
        AuthEvent::Problem {
            text: "Error notice from controlled stack".into()
        }
    );
    query(&mut a, 20, 3, "Separate PIN challenge", true);
    // The PIN is its own answer: the password is never reused for it.
    answer(&mut a, 25, 3, "first-secret");
    assert_eq!(
        auth_event(&mut a, 20),
        AuthEvent::Problem {
            text: "Sorry, password authentication didn’t work. Please try again.".into()
        }
    );
    result(&mut a, 20, false);
    assert_eq!(state(&root)["locked"], true, "a wrong PIN cannot unlock");
    wait(|| state(&root)["auth_owner"]["conversations"]["password"] == false);
    // Second conversation: cancelled mid-way, silently denied.
    begin(&mut a, 30, AuthService::Password);
    query(&mut a, 30, 1, "Visible account challenge", false);
    answer(&mut a, 31, 1, "visible-answer");
    query(&mut a, 30, 2, "First secret challenge", true);
    send(&mut a, 32, CommandKind::CancelVerification);
    result(&mut a, 32, true);
    match receive(&mut a, |m| {
        matches!(m, Message::AuthEvent { request: 30, .. })
            || matches!(m, Message::CommandResult { id: 30, .. })
    }) {
        Message::CommandResult {
            status: CommandStatus::Denied { .. },
            ..
        } => {}
        other => panic!("a cancelled conversation reports nothing else: {other:?}"),
    }
    assert_eq!(state(&root)["locked"], true, "cancel cannot unlock");
    // A late answer to the cancelled conversation goes nowhere.
    answer(&mut a, 33, 2, "first-secret");
    // Third conversation: every fresh correct answer unlocks.
    begin(&mut a, 40, AuthService::Password);
    query(&mut a, 40, 1, "Visible account challenge", false);
    answer(&mut a, 41, 1, "visible-answer");
    query(&mut a, 40, 2, "First secret challenge", true);
    answer(&mut a, 42, 2, "first-secret");
    assert!(matches!(auth_event(&mut a, 40), AuthEvent::Info { .. }));
    assert!(matches!(auth_event(&mut a, 40), AuthEvent::Problem { .. }));
    query(&mut a, 40, 3, "Separate PIN challenge", true);
    answer(&mut a, 43, 3, "pin-secret");
    result(&mut a, 40, true);
    wait(|| {
        state(&root)["locked"] == false
            && state(&root)["auth_owner"]["conversations"]["password"] == false
    });
    // Unlocked: nothing to verify, so a new conversation is denied.
    begin(&mut a, 50, AuthService::Password);
    result(&mut a, 50, false);
    publish_json(
        &root.join("success.json"),
        &serde_json::json!({
            "supervised_client_pid": std::process::id(),
            "fingerprint_unavailable_denied": true,
            "second_password_conversation_denied": true,
            "stale_sequence_dropped": true,
            "distinct_answers_per_question": true,
            "info_and_error_relayed": true,
            "wrong_pin_denied_with_problem": true,
            "cancel_denied_silently": true,
            "fresh_answers_unlock": true,
            "scope": "actual nested Runtime + private PAM helper + controlled multi-prompt module; no GTK, reader or installed stack"
        }),
    );
    std::thread::sleep(Duration::from_secs(2));
}

#[test]
fn actual_main_runtime_pam_conversation_relay() {
    match std::env::var(ROLE).ok().as_deref() {
        Some("4") => return conversation_client(),
        Some(_) => return,
        None => {}
    }
    run_parent(
        Path::new(env!("CARGO_BIN_EXE_tuna-compositor")),
        "4",
        "conversation",
    );
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

/// The attempt's private helper is gone and the outer slot is free, yet the
/// module was never released: retirement came from cancellation alone.
fn assert_helper_retired_unreleased(root: &Path, attempt: u32) {
    let helper: u32 = std::fs::read_to_string(root.join(format!("started-{attempt}")))
        .unwrap()
        .parse()
        .unwrap();
    wait(|| {
        !Path::new(&format!("/proc/{helper}")).exists()
            && state(root)["auth_owner"]["outer_pending"] == false
    });
    assert!(
        !root.join(format!("release-{attempt}")).exists()
            && !root.join(format!("done-{attempt}")).exists(),
        "retirement must not require releasing the hung module"
    );
}

#[test]
fn actual_main_runtime_private_pam_retirement_and_held_self() {
    match std::env::var(ROLE).ok().as_deref() {
        Some("3") => return client(),
        Some(_) => return,
        None => {}
    }
    run_parent(Path::new(env!("CARGO_BIN_EXE_tuna-compositor")), "3", "pam");
}

const NO_MATCH: &str = "Failed to match fingerprint";
const RETRY: &str = "Place your finger on the reader again";
const ACCOUNT_EXPIRED: &str = "Your account was given a time limit that’s now passed.";
const MAX_TRIES: &str =
    "You reached the maximum fingerprint authentication attempts, please try another method";

/// Script the next fingerprint conversation of the controlled module:
/// one letter per scan, `m` match, `n` no-match, `t` timed out, each
/// optionally after `r` retry statuses within that scan.
fn plan_scans(root: &Path, plan: &str) {
    std::fs::write(root.join("fprint-plan"), plan).unwrap();
}

fn fingerprint_events(conn: &mut ControlConn, request: u64, expected: &[&str]) {
    use tuna_shell_control::AuthEvent;
    for step in expected {
        let wanted = match *step {
            "scan" => AuthEvent::FingerprintScan { swipe: true },
            "unavailable" => AuthEvent::Unavailable,
            text => AuthEvent::Problem { text: text.into() },
        };
        assert_eq!(auth_event(conn, request), wanted, "request {request}");
    }
}

/// Fingerprint unlock through PAM (#1162 F5) against the actual runtime,
/// private helper and a pam_fprintd-like module behind `tuna-lock-fingerprint`:
/// the account stack refuses an expired account after a match; PAM's
/// max-tries and a timed-out scan end the service as GDM reports them;
/// a lock admits three failed conversations, then refuses before PAM
/// runs; the password still unlocks; on a new lock a retry status and a
/// no-match, then a match, unlock.
fn fingerprint_client() {
    use tuna_shell_control::AuthService;
    let root = PathBuf::from(std::env::var_os("TUNA_AUTH_OWNER_ROOT").unwrap());
    let failures = || state(&root)["auth_owner"]["conversations"]["fingerprint_failures"].clone();
    wait(|| state(&root)["auth_owner"].is_object());
    let mut a = connect();
    send(&mut a, 10, CommandKind::Lock);
    result(&mut a, 10, true);
    wait(|| state(&root)["locked"] == true);
    // A match on an expired account: pam_acct_mgmt refuses it.
    std::fs::write(root.join("fprint-expired"), "").unwrap();
    plan_scans(&root, "nm");
    begin(&mut a, 11, AuthService::Fingerprint);
    fingerprint_events(&mut a, 11, &["scan", NO_MATCH, "scan", ACCOUNT_EXPIRED]);
    result(&mut a, 11, false);
    assert_eq!(
        state(&root)["locked"],
        true,
        "an expired account never unlocks by fingerprint"
    );
    std::fs::remove_file(root.join("fprint-expired")).unwrap();
    // Three no-matches: PAM_MAXTRIES, GDM's message, the service stops.
    plan_scans(&root, "nnn");
    begin(&mut a, 12, AuthService::Fingerprint);
    fingerprint_events(
        &mut a,
        12,
        &[
            "scan",
            NO_MATCH,
            "scan",
            NO_MATCH,
            "scan",
            NO_MATCH,
            MAX_TRIES,
            "unavailable",
        ],
    );
    result(&mut a, 12, false);
    // A no-match, then a timed-out scan (PAM_AUTHINFO_UNAVAIL; its info
    // message is only GNOME's hint cue).
    plan_scans(&root, "nt");
    begin(&mut a, 13, AuthService::Fingerprint);
    fingerprint_events(
        &mut a,
        13,
        &["scan", NO_MATCH, "scan", "scan", "unavailable"],
    );
    result(&mut a, 13, false);
    wait(|| failures() == 3);
    // The lock's failed conversations are used up: a matching finger is
    // never even asked for.
    plan_scans(&root, "m");
    begin(&mut a, 14, AuthService::Fingerprint);
    fingerprint_events(&mut a, 14, &["unavailable"]);
    result(&mut a, 14, false);
    assert!(
        root.join("fprint-plan").exists(),
        "PAM ran after the lock's fingerprint budget"
    );
    assert_eq!(state(&root)["locked"], true);
    // The password conversation is unaffected.
    begin(&mut a, 20, AuthService::Password);
    query(&mut a, 20, 1, "Visible account challenge", false);
    answer(&mut a, 21, 1, "visible-answer");
    query(&mut a, 20, 2, "First secret challenge", true);
    answer(&mut a, 22, 2, "first-secret");
    let _info = auth_event(&mut a, 20);
    let _error = auth_event(&mut a, 20);
    query(&mut a, 20, 3, "Separate PIN challenge", true);
    answer(&mut a, 23, 3, "pin-secret");
    result(&mut a, 20, true);
    wait(|| state(&root)["locked"] == false);
    // A new lock starts afresh: a retry status and a no-match are shown,
    // then a match on a valid account unlocks.
    send(&mut a, 30, CommandKind::Lock);
    result(&mut a, 30, true);
    wait(|| state(&root)["locked"] == true);
    plan_scans(&root, "rnm");
    begin(&mut a, 31, AuthService::Fingerprint);
    fingerprint_events(&mut a, 31, &["scan", RETRY, NO_MATCH, "scan"]);
    result(&mut a, 31, true);
    wait(|| {
        state(&root)["locked"] == false
            && state(&root)["auth_owner"]["conversations"]["fingerprint"] == false
    });
    publish_json(
        &root.join("success.json"),
        &serde_json::json!({
            "supervised_client_pid": std::process::id(),
            "expired_account_match_denied": true,
            "max_tries_unavailable": true,
            "timed_out_unavailable": true,
            "lock_budget_refuses_before_pam": true,
            "password_unaffected": true,
            "retry_then_match_unlocks": true,
            "scope": "actual nested Runtime + private PAM helper on the fingerprint service + pam_fprintd-like controlled module + private-bus fprintd probe; no GTK, reader or installed stack"
        }),
    );
    std::thread::sleep(Duration::from_secs(2));
}

/// `enable-fingerprint-authentication` off: the compositor refuses the
/// service itself, whatever the shell asks; PAM never runs.
fn fingerprint_setting_client() {
    use tuna_shell_control::AuthService;
    let root = PathBuf::from(std::env::var_os("TUNA_AUTH_OWNER_ROOT").unwrap());
    wait(|| state(&root)["auth_owner"].is_object());
    let mut a = connect();
    send(&mut a, 10, CommandKind::Lock);
    result(&mut a, 10, true);
    wait(|| state(&root)["locked"] == true);
    plan_scans(&root, "m");
    begin(&mut a, 11, AuthService::Fingerprint);
    fingerprint_events(&mut a, 11, &["unavailable"]);
    result(&mut a, 11, false);
    assert!(
        root.join("fprint-plan").exists(),
        "PAM ran with fingerprint disabled"
    );
    assert_eq!(state(&root)["locked"], true);
    publish_json(
        &root.join("success.json"),
        &serde_json::json!({
            "supervised_client_pid": std::process::id(),
            "setting_off_refused_before_pam": true,
            "scope": "actual nested Runtime; org.gnome.login-screen from a private schema and keyfile backend"
        }),
    );
    std::thread::sleep(Duration::from_secs(2));
}

#[test]
fn actual_main_runtime_fingerprint_through_pam() {
    match std::env::var(ROLE).ok().as_deref() {
        Some("5") => return fingerprint_client(),
        Some(_) => return,
        None => {}
    }
    run_parent(
        Path::new(env!("CARGO_BIN_EXE_tuna-compositor")),
        "5",
        "fingerprint",
    );
}

#[test]
fn actual_main_runtime_fingerprint_setting_enforced() {
    match std::env::var(ROLE).ok().as_deref() {
        Some("6") => return fingerprint_setting_client(),
        Some(_) => return,
        None => {}
    }
    run_parent(
        Path::new(env!("CARGO_BIN_EXE_tuna-compositor")),
        "6",
        "fingerprint-setting",
    );
}

struct StubManager;

#[zbus::interface(name = "net.reactivated.Fprint.Manager")]
impl StubManager {
    fn get_default_device(&self) -> zbus::OwnedObjectPath {
        zbus::OwnedObjectPath::try_from(STUB_DEVICE).unwrap()
    }
}

struct StubDevice;

#[zbus::interface(name = "net.reactivated.Fprint.Device")]
impl StubDevice {
    fn list_enrolled_fingers(&self, _username: String) -> Vec<String> {
        vec!["right-index-finger".into()]
    }

    #[zbus(property, name = "scan-type")]
    fn scan_type(&self) -> String {
        "swipe".into()
    }
}

const STUB_DEVICE: &str = "/net/reactivated/Fprint/Device/0";

/// fprintd's probe surface (a swipe reader with one enrolled print) on a
/// private bus handed to the compositor as its system bus. Verification
/// itself is the PAM module's: the stub has no Claim or VerifyStart.
struct FprintdStub {
    daemon: Child,
    address: String,
    _service: zbus::Connection,
}

impl FprintdStub {
    fn spawn() -> Self {
        use std::io::BufRead;
        let mut daemon = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("private fprintd bus starts");
        let mut address = String::new();
        std::io::BufReader::new(daemon.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        let address = address.trim().to_owned();
        let service = zbus::block_on(
            zbus::connection::Builder::address(address.as_str())
                .method_timeout(Duration::from_secs(10))
                .build(),
        )
        .unwrap();
        zbus::block_on(service.request_name("net.reactivated.Fprint")).unwrap();
        zbus::block_on(
            service
                .object_server()
                .at("/net/reactivated/Fprint/Manager", StubManager),
        )
        .unwrap();
        zbus::block_on(service.object_server().at(STUB_DEVICE, StubDevice)).unwrap();
        Self {
            daemon,
            address,
            _service: service,
        }
    }
}

impl Drop for FprintdStub {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

/// GNOME's `org.gnome.login-screen` schema (shipped by GDM, absent on CI)
/// compiled under `root/schemas`, with `enable-fingerprint-authentication`
/// set in the keyfile backend's file under `root/config`.
fn login_screen_setting(root: &Path, enabled: bool) {
    let schemas = root.join("schemas");
    std::fs::create_dir(&schemas).unwrap();
    std::fs::write(
        schemas.join("org.gnome.login-screen.gschema.xml"),
        "<schemalist><schema id='org.gnome.login-screen' path='/org/gnome/login-screen/'>\
         <key name='enable-fingerprint-authentication' type='b'><default>true</default></key>\
         </schema></schemalist>",
    )
    .unwrap();
    assert!(Command::new("glib-compile-schemas")
        .arg(&schemas)
        .status()
        .unwrap()
        .success());
    let keyfile = root.join("config/glib-2.0/settings");
    std::fs::create_dir_all(&keyfile).unwrap();
    std::fs::write(
        keyfile.join("keyfile"),
        format!("[org/gnome/login-screen]\nenable-fingerprint-authentication={enabled}\n"),
    )
    .unwrap();
}

fn run_parent(bin: &Path, role: &str, kind: &str) {
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
    // Real startup must not inspect the invoking user's migration paths.
    for name in ["home", "config", "cache", "data", "state"] {
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(root.path().join(name))
            .unwrap();
    }
    let _artifacts = Artifacts {
        root: root.path().to_owned(),
        out: std::env::var_os("TUNA_AUTH_OWNER_ARTIFACTS").map(PathBuf::from),
    };
    let end = Instant::now() + Duration::from_secs(30);
    // Role 4 is the interactive relay: the module's multi-prompt mode;
    // roles 5 and 6 add the fingerprint service; every other role delays
    // a single controlled password.
    let fingerprint = matches!(role, "5" | "6");
    let (service, mode) = if role == "4" || fingerprint {
        ("runtime-conversation", "multi")
    } else {
        ("runtime-delay", "runtime-delay")
    };
    assert!(
        Path::new("/usr/lib/x86_64-linux-gnu/libpam_wrapper.so").exists(),
        "required actual PAM wrapper"
    );
    let module = root.path().join("controlled-pam.so");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pam_conversation.c");
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
        root.path().join("services").join(service),
        format!(
            "auth required {} {mode}\naccount required {} {mode}\n",
            module.display(),
            module.display()
        ),
    )
    .unwrap();
    // The fingerprint service: a pam_fprintd-like module scripted by the
    // client, then an account stack the client can expire.
    std::fs::write(
        root.path().join("services").join("runtime-fingerprint"),
        format!(
            "auth required {} fprint-plan\naccount required {} fprint-plan\n",
            module.display(),
            module.display()
        ),
    )
    .unwrap();
    // A reader with an enrolled print, and GNOME's login-screen setting:
    // on for role 5, off for role 6.
    let fprintd = fingerprint.then(FprintdStub::spawn);
    if fingerprint {
        login_screen_setting(root.path(), role == "5");
    }
    let owned_bin = root.path().join("compositor-copy");
    if role == "3" {
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
    command
        .env("TUNA_PAM_SERVICE", service)
        .env("TUNA_PAM_FINGERPRINT_SERVICE", "runtime-fingerprint")
        .env("LD_PRELOAD", "/usr/lib/x86_64-linux-gnu/libpam_wrapper.so")
        .env("PAM_WRAPPER", "1")
        .env("PAM_WRAPPER_SERVICE_DIR", root.path().join("services"))
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
        // The supervised peer is libtest too; retain its panic in compositor.log.
        .env("RUST_TEST_NOCAPTURE", "1")
        .env("TUNA_AUTH_OWNER_TEST_BIN", std::env::current_exe().unwrap())
        .env("TUNA_AUTH_OWNER_ROOT", root.path())
        .env("XDG_RUNTIME_DIR", root.path())
        .env("HOME", root.path().join("home"))
        .env("XDG_CONFIG_HOME", root.path().join("config"))
        .env("XDG_CACHE_HOME", root.path().join("cache"))
        .env("XDG_DATA_HOME", root.path().join("data"))
        .env("XDG_STATE_HOME", root.path().join("state"))
        .env("TUNA_COMPOSITOR_STATE", root.path().join("state.json"))
        .env("TUNA_LOCK_TRACE", "1")
        .env("TUNA_XWAYLAND", "0")
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("DBUS_SYSTEM_BUS_ADDRESS")
        .stdout(Stdio::null())
        .stderr(log);
    match &fprintd {
        Some(stub) => {
            command
                .env("DBUS_SYSTEM_BUS_ADDRESS", &stub.address)
                .env("GSETTINGS_BACKEND", "keyfile")
                .env("GSETTINGS_SCHEMA_DIR", root.path().join("schemas"));
        }
        // No reader here: the fingerprint service must say unavailable.
        None => {
            command.env("TUNA_FPRINTD_OFF", "1");
        }
    }
    let mut child = OwnedChild(command.spawn().unwrap());
    publish_json(
        &root.path().join("compositor-pid"),
        &serde_json::json!(child.0.id()),
    );
    while !root.path().join("success.json").exists() {
        let status = child.0.try_wait().unwrap();
        assert!(
            status.is_none(),
            "actual compositor exited before proof: {status:?}"
        );
        // The supervised driver is libtest: its panic is in compositor.log,
        // and the restarted driver cannot finish the proof it began.
        let log = std::fs::read_to_string(root.path().join("compositor.log")).unwrap_or_default();
        if let Some(at) = log.find("panicked at") {
            panic!(
                "supervised driver failed: {}",
                log[at..].lines().take(4).collect::<Vec<_>>().join(" / ")
            );
        }
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
