//! Lock-screen PAM path (#62) against a real PAM stack.
//!
//! Needs pam_wrapper (libpam-wrapper), which runs PAM against a private
//! service directory and password file without root. The test re-runs
//! itself with LD_PRELOAD set; without pam_wrapper it is skipped with a
//! note, so a developer machine without the package stays green. CI
//! installs it (gtk-shell job) and sets TUNA_REQUIRE_PAM_WRAPPER=1,
//! which turns the skip into a failure.

use std::path::Path;
use std::process::Command;

use tuna_compositor::pam::{authenticate, PamClient};
use tuna_compositor::unlock::{attempt_unlock, UnlockOutcome};

const WRAPPER: &str = "/usr/lib/x86_64-linux-gnu/libpam_wrapper.so";
const MATRIX: &str = "/usr/lib/x86_64-linux-gnu/pam_wrapper/pam_matrix.so";
const CHILD: &str = "TUNA_PAM_TEST_CHILD";

#[test]
fn pam_unlock_accepts_the_right_password_and_nothing_else() {
    if std::env::var_os(CHILD).is_some() {
        return child();
    }
    if !Path::new(WRAPPER).exists() || !Path::new(MATRIX).exists() {
        assert!(
            std::env::var_os("TUNA_REQUIRE_PAM_WRAPPER").is_none(),
            "pam_wrapper is required here but missing"
        );
        eprintln!("pam_unlock: pam_wrapper not installed, skipped");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let passdb = dir.path().join("passdb");
    let own_user = tuna_compositor::unlock::session_user().expect("kernel session account");
    std::fs::write(
        &passdb,
        format!(
            "tunauser:right-password:tuna-lock-test\n{own_user}:right-password:tuna-lock-test\n"
        ),
    )
    .unwrap();
    let services = dir.path().join("services");
    std::fs::create_dir(&services).unwrap();
    std::fs::write(
        services.join("tuna-lock-test"),
        format!(
            "auth required {MATRIX} passdb={p}\naccount required {MATRIX} passdb={p}\n",
            p = passdb.display()
        ),
    )
    .unwrap();
    let exe = std::env::current_exe().unwrap();
    let status = Command::new(exe)
        .args([
            "--exact",
            "pam_unlock_accepts_the_right_password_and_nothing_else",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .env("LD_PRELOAD", WRAPPER)
        .env("PAM_WRAPPER", "1")
        .env("PAM_WRAPPER_SERVICE_DIR", &services)
        .status()
        .unwrap();
    assert!(status.success(), "child PAM checks failed");
}

fn child() {
    let service = "tuna-lock-test";
    assert!(authenticate(service, "tunauser", "right-password"));
    assert!(!authenticate(service, "tunauser", "wrong-password"));
    assert!(!authenticate(service, "nobody-here", "right-password"));
    assert!(!authenticate(service, "tunauser", ""));
    assert!(!authenticate(
        "no-such-service",
        "tunauser",
        "right-password"
    ));
    // The private helper permits ONLY the kernel session owner. Keep the raw
    // controlled matrix checks above, and use that owner for the live facade.
    let own_user = tuna_compositor::unlock::session_user().expect("kernel session account");
    let executable = std::path::PathBuf::from(env!("CARGO_BIN_EXE_tuna-compositor"));
    let backend = tuna_compositor::pam_backend::Backend::with_trusted_factory(
        move |identity, service, user, budget, cancel| {
            tuna_compositor::pam_helper::Supervisor::spawn_cancellable(
                &executable,
                identity,
                service,
                user,
                budget,
                cancel,
            )
        },
    )
    .unwrap();
    let mut client = PamClient::with_backend(service, backend.clone());
    assert_eq!(
        attempt_unlock(&mut client, &own_user, "right-password"),
        UnlockOutcome::Unlocked
    );
    assert_eq!(
        attempt_unlock(&mut client, &own_user, "nope"),
        UnlockOutcome::Denied
    );
    assert_ne!(
        own_user, "tunauser",
        "foreign-user negative fixture must be distinct"
    );
    assert_eq!(
        attempt_unlock(&mut client, "tunauser", "right-password"),
        UnlockOutcome::Denied
    );
    backend.shutdown();
}
