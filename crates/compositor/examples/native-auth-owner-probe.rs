//! Owned CI driver: controlled epoch injection, never a logind/S3 proof.
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tuna_compositor::runtime::{run_auth_owner_probe, NestedSession};

fn main() {
    if std::env::var_os("TUNA_STAGEB_PROBE").is_some() {
        return interactive();
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
    run_auth_owner_probe(&session, move |probe| {
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
                root.join("epoch-injected.json"),
                serde_json::to_vec(&serde_json::json!({
                    "before":before.epoch,"after":after.epoch,"busy_sync_denied":!busy,
                    "scope":"controlled runtime epoch injection, not logind or S3"
                }))
                .unwrap(),
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
                root.join("sync-result.json"),
                serde_json::to_vec(&serde_json::json!({
                    "applied":applied,"before":before.epoch,"after":after.epoch,
                    "scope":"real public Runtime::try_unlock on controlled greetd"
                }))
                .unwrap(),
            )
            .unwrap();
            synced = true;
        }
        if root.join("stop-probe").exists() {
            probe.request_exit();
        }
        Some(Duration::from_millis(20))
    })
    .expect("actual runtime probe loop");
}

// Controlled CI mode in this admitted example ONLY; the normal compositor has
// no environment-driven interactive policy or helper-executable selection.
fn interactive() {
    use tuna_compositor::{pam_backend::Backend, pam_conversation::Policy, pam_helper::Supervisor};
    let root = PathBuf::from(std::env::var_os("TUNA_AUTH_OWNER_ROOT").expect("owned root"));
    let executable =
        PathBuf::from(std::env::var_os("TUNA_STAGEB_HELPER").expect("admitted compositor"));
    let expected = std::fs::metadata(&executable).unwrap();
    let admission_user = tuna_compositor::unlock::SessionUser::resolve_uid()
        .expect("uid-derived user before Runtime startup");
    let user = admission_user.username().to_owned();
    let backend =
        Backend::with_trusted_factory(move |identity, service, actual_user, budget, cancel| {
            assert_eq!(service, tuna_compositor::pam::DEFAULT_SERVICE);
            assert_eq!(actual_user, user);
            let helper = Supervisor::spawn_conversation(
                &executable,
                identity,
                service,
                actual_user,
                budget,
                cancel,
            )?;
            use std::os::unix::fs::MetadataExt;
            let actual = std::fs::metadata(format!("/proc/{}/exe", helper.pid()))?;
            assert_eq!(
                (actual.dev(), actual.ino()),
                (expected.dev(), expected.ino())
            );
            Ok(helper)
        })
        .unwrap();
    let mut session = NestedSession::default_for_pid();
    session.shell_bin = Some(
        std::env::var_os("TUNA_AUTH_OWNER_TEST_BIN")
            .expect("owned actor")
            .into(),
    );
    session.xwayland = false;
    let end = Instant::now() + Duration::from_secs(40);
    let mut admitted = false;
    let mut inputs = std::collections::HashSet::new();
    let mut invalidated = false;
    let mut timeout_policy = false;
    run_auth_owner_probe(&session, move |probe| {
        assert!(
            Instant::now() < end,
            "absolute controlled Runtime proof deadline"
        );
        if !admitted {
            assert!(probe.admit_interactive_pam(backend.clone(), admission_user.clone()));
            std::fs::write(root.join("profile-admitted"), b"controlled profile").unwrap();
            admitted = true;
        }
        for input in 1..=2 {
            if root.join(format!("input-{input}")).exists() && !inputs.contains(&input) {
                assert!(!probe.interactive_input_for_test(false));
                assert!(probe.interactive_input_for_test(true));
                std::fs::write(
                    root.join(format!("input-admitted-{input}")),
                    b"controlled Rust input seam; not native keyboard",
                )
                .unwrap();
                inputs.insert(input);
            }
        }
        if !invalidated && root.join("stageb-epoch").exists() {
            assert!(probe.interactive_pending_for_test());
            assert!(probe.invalidate_epoch());
            std::fs::write(root.join("stageb-epoch-done"), b"controlled epoch").unwrap();
            invalidated = true;
        }
        if !timeout_policy
            && root.join("short-phase").exists()
            && !probe.interactive_pending_for_test()
        {
            assert!(probe.admit_interactive_pam_policy(
                backend.clone(),
                admission_user.clone(),
                Policy {
                    backend_phase: Duration::from_millis(250),
                    total: Duration::from_secs(120),
                }
            ));
            std::fs::write(
                root.join("short-phase-admitted"),
                b"controlled 250ms backend phase",
            )
            .unwrap();
            timeout_policy = true;
        }
        std::fs::write(
            root.join("interactive-pending"),
            if probe.interactive_pending_for_test() {
                b"1"
            } else {
                b"0"
            },
        )
        .unwrap();
        if root.join("stop-probe").exists() {
            probe.request_exit();
        }
        Some(Duration::from_millis(10))
    })
    .expect("actual interactive Runtime loop");
}
