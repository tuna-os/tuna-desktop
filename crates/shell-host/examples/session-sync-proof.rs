//! Real nested Runtime + supervised ControlClient observer. The trusted
//! probe injects the production handlers' notifications, not physical S3
//! or libseat/KMS/evdev transitions. No synthetic control command exists.
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};
use tuna_compositor::runtime::{run_auth_owner_probe, NestedSession};
use tuna_shell_host::control::{ControlClient, ControlError};

struct Window(Child);
impl Drop for Window {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn wait(end: Instant, mut ready: impl FnMut() -> bool) {
    while !ready() {
        assert!(Instant::now() < end, "session sync proof deadline");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn poll_until(
    client: &mut ControlClient,
    end: Instant,
    mut ready: impl FnMut(&ControlClient) -> bool,
) {
    wait(end, || {
        match client.poll() {
            Ok(_) => {}
            Err(ControlError::Io(e)) if e.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => panic!("observer connection failed: {error:?}"),
        }
        ready(client)
    });
}

fn connect(path: &Path, end: Instant) -> ControlClient {
    let mut client = ControlClient::connect(path).expect("control socket");
    client.send_hello().unwrap();
    wait(end, || match client.await_hello() {
        Ok(()) => true,
        Err(ControlError::Io(e)) if e.kind() == io::ErrorKind::WouldBlock => false,
        Err(error) => panic!("observer hello failed: {error:?}"),
    });
    poll_until(&mut client, end, |c| !c.needs_snapshot());
    client
}

fn stage(root: &Path, name: &str, end: Instant) {
    std::fs::write(root.join("request.tmp"), name).unwrap();
    std::fs::rename(root.join("request.tmp"), root.join("request")).unwrap();
    wait(end, || {
        std::fs::read_to_string(root.join("ack")).is_ok_and(|ack| ack == name)
    });
}

fn child(root: &Path, window_bin: &Path) {
    let end = Instant::now() + Duration::from_secs(45);
    let socket = PathBuf::from(std::env::var_os("TUNA_CONTROL_SOCKET").unwrap());
    let mut client = connect(&socket, end);
    assert!(!client.locked(), "session starts unlocked");
    let _window = Window(
        Command::new(window_bin)
            .env("TUNA_LIFECYCLE_COMMAND", root.join("window-command"))
            .spawn()
            .expect("real xdg window client"),
    );
    poll_until(&mut client, end, |c| !c.model().windows().is_empty());
    let window = client.model().windows()[0].id;
    let workspaces = client.model().workspaces().to_vec();
    let active = client.model().active_workspace();
    client.toggle_overview().unwrap();
    poll_until(&mut client, end, |c| c.model().is_overview_open());

    // libseat notification handlers must not reset the shell's window,
    // workspace or overview model while temporarily off-seat.
    for notification in ["seat-pause", "seat-resume"] {
        stage(root, notification, end);
        // Reconnect across each boundary: observe the compositor's
        // current snapshot, not just unchanged stale client memory.
        client = connect(&socket, end);
        poll_until(&mut client, end, |c| c.model().is_overview_open());
        assert!(
            !client.locked(),
            "{notification} must not invent an unlock/lock"
        );
        assert_eq!(client.model().windows()[0].id, window, "{notification}");
        assert_eq!(client.model().workspaces(), workspaces, "{notification}");
        assert_eq!(client.model().active_workspace(), active, "{notification}");
    }

    stage(root, "sleep", end);
    poll_until(&mut client, end, ControlClient::locked);
    poll_until(&mut client, end, |c| !c.model().is_overview_open());
    assert!(
        client.model().windows().is_empty(),
        "locked snapshots redact window titles"
    );

    // The shell may be disconnected throughout wake. The replacement
    // must receive the current locked state, never replay its pre-sleep
    // overview or leak the previous window list.
    drop(client);
    stage(root, "wake-off-seat", end);
    let mut replacement = connect(&socket, end);
    assert!(
        replacement.locked(),
        "wake stays locked even without a shell"
    );
    assert!(replacement.model().windows().is_empty());
    poll_until(&mut replacement, end, |c| !c.model().is_overview_open());
    stage(root, "wake-seat-resume", end);
    replacement = connect(&socket, end);
    assert!(
        replacement.locked(),
        "seat reactivation cannot unlock after sleep"
    );
    assert!(replacement.model().windows().is_empty());
    assert_eq!(replacement.model().workspaces(), workspaces);
    assert_eq!(replacement.model().active_workspace(), active);
    std::fs::write(root.join("done"), "ok").unwrap();
}

fn main() {
    let root = PathBuf::from(std::env::var_os("TUNA_SESSION_SYNC_ROOT").expect("owned proof root"));
    let window_bin = PathBuf::from(
        std::env::var_os("TUNA_SESSION_SYNC_WINDOW").expect("admitted window client"),
    );
    if std::env::var_os("TUNA_SESSION_SYNC_CHILD").is_some() {
        child(&root, &window_bin);
        return;
    }
    let mut session = NestedSession::default_for_pid();
    session.shell_bin = Some(std::env::current_exe().unwrap());
    session.xwayland = false;
    std::env::set_var("TUNA_SESSION_SYNC_CHILD", "1");
    let end = Instant::now() + Duration::from_secs(70);
    let mut last = String::new();
    let mut epoch = None;
    run_auth_owner_probe(&session, |probe| {
        assert!(Instant::now() < end, "bounded runtime session sync driver");
        if root.join("done").exists() {
            assert!(probe.status().locked);
            std::fs::write(root.join("result.json"), serde_json::to_vec_pretty(&serde_json::json!({
                "passed": true,
                "scope": "simulated runtime sleep/libseat notifications; not physical VT/S3/KMS/evdev",
                "assertions": ["seat-preserves-window-workspace-overview", "sleep-locks-and-redacts",
                    "wake-without-shell-stays-locked", "reconnect-observes-current-state", "seat-resume-cannot-unlock"]
            })).unwrap()).unwrap();
            probe.request_exit();
        }
        if let Ok(request) = std::fs::read_to_string(root.join("request")) {
            if request != last {
                match request.as_str() {
                    "seat-pause" => probe.set_session_active(false),
                    "seat-resume" => probe.set_session_active(true),
                    "sleep" => {
                        let before = probe.status();
                        assert!(!before.locked);
                        epoch = Some(before.epoch);
                        probe.prepare_for_sleep(true);
                        assert!(probe.status().locked);
                        assert!(probe.status().epoch > before.epoch);
                    }
                    "wake-off-seat" => {
                        probe.set_session_active(false);
                        probe.prepare_for_sleep(false);
                        assert!(probe.status().locked);
                        assert!(probe.status().epoch > epoch.unwrap());
                    }
                    "wake-seat-resume" => probe.set_session_active(true),
                    _ => panic!("unexpected probe phase {request:?}"),
                }
                std::fs::write(root.join("ack"), &request).unwrap();
                last = request;
            }
        }
        Some(Duration::from_millis(5))
    }).expect("real nested runtime");
    assert!(
        root.join("result.json").is_file(),
        "observer must finish every phase"
    );
    println!(
        "G-SESSION-SYNC pass: real ControlClient snapshots across simulated seat/sleep transitions"
    );
}
