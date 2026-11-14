//! Owned CI driver: controlled epoch injection, never a logind/S3 proof.
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};
use tuna_compositor::runtime::{run_auth_owner_probe, NestedSession};

fn main() -> ExitCode {
    // The private PAM helper re-executes this held image, exactly like the
    // compositor binary: never initialize a display there.
    if std::env::args_os().nth(1).as_deref()
        == Some(std::ffi::OsStr::new(tuna_compositor::pam_helper::FLAG))
    {
        if std::env::args_os().count() != 2 {
            return ExitCode::FAILURE;
        }
        return tuna_compositor::pam_helper::entry();
    }
    let root = PathBuf::from(std::env::var_os("TUNA_AUTH_OWNER_ROOT").expect("owned fixture root"));
    let mut session = NestedSession::default_for_pid();
    session.shell_bin = Some(
        std::env::var_os("TUNA_AUTH_OWNER_TEST_BIN")
            .expect("admitted owned test binary")
            .into(),
    );
    session.xwayland = false;
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut injected = false;
    let mut synced = false;
    // Native getenv does not take Rust's environment lock. Keep a reader live
    // across launch, authentication and teardown; the runtime must configure
    // child environments without changing its own (startup SIGSEGV #919).
    let ambient_display = std::env::var_os("WAYLAND_DISPLAY");
    let expected_display = ambient_display.clone();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let reader_stop = stop.clone();
    let (ready, started) = std::sync::mpsc::sync_channel(0);
    let reader = std::thread::spawn(move || {
        let key = c"XDG_DATA_DIRS";
        // SAFETY: a static NUL-terminated key; the returned borrowed pointer is
        // not retained. No thread in this fixture writes the process environment.
        unsafe { libc::getenv(key.as_ptr()) };
        ready.send(()).unwrap();
        while !reader_stop.load(std::sync::atomic::Ordering::Acquire) {
            unsafe { libc::getenv(key.as_ptr()) };
            std::thread::sleep(Duration::from_millis(1));
        }
    });
    started.recv_timeout(Duration::from_secs(5)).unwrap();
    run_auth_owner_probe(&session, move |probe| {
        assert_eq!(
            std::env::var_os("WAYLAND_DISPLAY"),
            expected_display,
            "runtime must not mutate environ while native getenv readers run"
        );
        assert!(
            Instant::now() < deadline,
            "bounded auth runtime driver deadline"
        );
        if !injected && root.join("inject-epoch").exists() {
            let before = probe.status();
            assert!(before.locked && before.outer_pending);
            let user = tuna_compositor::unlock::session_user().expect("own session user");
            let busy = probe.try_unlock(&user, "controlled-only");
            assert!(
                !busy,
                "public synchronous path must reject active outer slot"
            );
            assert!(probe.invalidate_epoch());
            let after = probe.status();
            assert!(after.locked && after.outer_pending && after.epoch == before.epoch + 1);
            std::fs::write(
                root.join("epoch-injected.tmp"),
                serde_json::to_vec(&serde_json::json!({
                    "before":before.epoch,"after":after.epoch,"busy_sync_denied":!busy,
                    "scope":"controlled runtime epoch injection, not logind or S3"
                }))
                .unwrap(),
            )
            .unwrap();
            std::fs::rename(
                root.join("epoch-injected.tmp"),
                root.join("epoch-injected.json"),
            )
            .unwrap();
            injected = true;
        }
        if !synced && root.join("try-sync").exists() {
            let before = probe.status();
            assert!(before.locked && !before.outer_pending);
            let user = tuna_compositor::unlock::session_user().expect("own session user");
            let applied = probe.try_unlock(&user, "controlled-only");
            assert!(applied);
            let after = probe.status();
            assert!(!after.locked && !after.outer_pending && after.epoch == before.epoch + 1);
            std::fs::write(
                root.join("sync-result.tmp"),
                serde_json::to_vec(&serde_json::json!({
                    "applied":applied,"before":before.epoch,"after":after.epoch,
                    "scope":"real public Runtime::try_unlock on controlled PAM"
                }))
                .unwrap(),
            )
            .unwrap();
            std::fs::rename(root.join("sync-result.tmp"), root.join("sync-result.json")).unwrap();
            synced = true;
        }
        if root.join("stop-probe").exists() {
            probe.request_exit();
        }
        Some(Duration::from_millis(20))
    })
    .expect("actual runtime probe loop");
    stop.store(true, std::sync::atomic::Ordering::Release);
    reader.join().expect("native getenv reader retired");
    assert_eq!(std::env::var_os("WAYLAND_DISPLAY"), ambient_display);
    eprintln!("auth owner runtime environment preserved with concurrent libc getenv");
    ExitCode::SUCCESS
}
