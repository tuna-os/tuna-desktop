//! The frames client's behaviour where there is no X server: what the
//! packaging checks and a session without Xwayland see.

use std::process::Command;

fn frames() -> Command {
    Command::new(env!("CARGO_BIN_EXE_tuna-x11-frames"))
}

#[test]
fn the_version_is_reported_without_a_display() {
    let output = frames()
        .arg("--version")
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let line = String::from_utf8(output.stdout).unwrap();
    let version = line
        .strip_prefix("tuna-x11-frames ")
        .and_then(|rest| rest.strip_suffix('\n'))
        .unwrap_or_else(|| panic!("unexpected version line {line:?}"));
    assert!(
        !version.is_empty() && !version.contains(char::is_whitespace),
        "{line:?}"
    );
}

/// Frames exist only on X11. A Wayland display, even with the session's
/// `GDK_BACKEND=wayland`, is not somewhere to run.
#[test]
fn it_fails_without_an_x11_display() {
    let output = frames()
        .env_remove("DISPLAY")
        .env("GDK_BACKEND", "wayland")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("tuna-x11-frames: cannot open an X11 display"),
        "{stderr}"
    );
}
