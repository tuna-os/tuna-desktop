//! Owned CI driver: controlled epoch injection, never a logind/S3 proof.
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tuna_compositor::runtime::{run_auth_owner_probe, NestedSession};

fn main() {
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
                root.join("epoch-injected.tmp"),
                serde_json::to_vec(&serde_json::json!({
                    "before":before.epoch,"after":after.epoch,"busy_sync_denied":!busy,
                    "scope":"controlled runtime epoch injection, not logind or S3"
                }))
                .unwrap(),
            )
            .unwrap();
            std::fs::rename(root.join("epoch-injected.tmp"), root.join("epoch-injected.json"))
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
                    "scope":"real public Runtime::try_unlock on controlled greetd"
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
}
