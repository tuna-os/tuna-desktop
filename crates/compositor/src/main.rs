//! Nested compositor binary (001 T1).
//!
//! Starts one isolated nested Wayland session on a private socket and
//! drives it until the window closes. The host display session is never
//! touched: child services receive `WAYLAND_DISPLAY` explicitly, and
//! the compositor process environment remains unmutated.

use std::process::ExitCode;

use tuna_compositor::eprintln;
use tuna_compositor::runtime::{run, BackendChoice, NestedSession, RunStats};

/// `tuna-compositor [--socket NAME] [--width W] [--height H] [--shell-bin PATH] [--xwayland] [--greeter]`.
/// X11 compatibility is on when `Xwayland` is installed (`TUNA_XWAYLAND=0`
/// turns it off, `--xwayland` or `TUNA_XWAYLAND=1` forces it).
///
/// Release version stamped at build time: `TUNA_VERSION` (a `vX.Y.Z` tag or
/// plain `X.Y.Z`) wins, otherwise the crate version. Duplicated per binary on
/// purpose — no shared dependency for a few lines.
fn normalize_version<'a>(raw: Option<&'a str>, fallback: &'a str) -> &'a str {
    match raw {
        Some(v) if !v.is_empty() => v.strip_prefix('v').unwrap_or(v),
        _ => fallback,
    }
}

fn release_version() -> &'static str {
    normalize_version(option_env!("TUNA_VERSION"), env!("CARGO_PKG_VERSION"))
}

/// `--help`.
const HELP: &str = "\
tuna-compositor: nested Tuna Desktop session (001 developer preview)

Usage: tuna-compositor [--backend auto|winit|drm] [--socket NAME] [--width W] [--height H] [--shell-bin PATH] [--xwayland] [--startup-overview] [--screen-reader] [--greeter]

Starts one isolated nested Wayland session on a private
socket (default tuna-nested-<pid>) and drives it until the
window closes. --backend auto (default) runs nested when a
host Wayland/X display exists and as a DRM/KMS hardware
session otherwise (from GDM or a TTY; needs a seat
from logind or seatd). --width/--height apply to nested runs.
The shell binary (default: TUNA_SHELL_BIN, else the
tuna-shell-host sibling) is spawned supervised with a
finite restart budget; WAYLAND_DISPLAY is configured for
supervised children without mutating process environment.
--startup-overview opens the overview at start, as GNOME
does at login (tuna-session passes it).
--screen-reader lets a nested session own the screen
reader as a hardware session always does: it serves
org.freedesktop.a11y.Manager and runs Orca while
screen-reader-enabled is set.
--greeter runs GDM's login screen instead of a user
session: the supervised client is tuna-greeter
(TUNA_GREETER_BIN, else the sibling, else PATH), with
no lock screen, overview or Tuna session target.
";

/// Apply the command line to `session`. `Some` ends the process at once
/// with that code (help, version, a bad argument); `None` runs it.
fn parse_args(
    mut args: impl Iterator<Item = String>,
    session: &mut NestedSession,
) -> Option<ExitCode> {
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--socket" => {
                if let Some(name) = args.next() {
                    session.socket_name = name;
                }
            }
            "--width" => {
                if let Some(value) = args.next().and_then(|v| v.parse().ok()) {
                    session.width = value;
                }
            }
            "--scale" => {
                if let Some(value) = args.next().and_then(|v| v.parse().ok()) {
                    session.scale = value;
                }
            }
            "--height" => {
                if let Some(value) = args.next().and_then(|v| v.parse().ok()) {
                    session.height = value;
                }
            }
            "--shell-bin" => {
                if let Some(path) = args.next() {
                    session.shell_bin = Some(path.into());
                }
            }
            "--xwayland" => {
                session.xwayland = true;
            }
            "--startup-overview" => {
                session.startup_overview = true;
            }
            "--screen-reader" => {
                session.screen_reader = true;
            }
            "--greeter" => {
                session.greeter = true;
            }
            "--backend" => match args.next().as_deref().and_then(BackendChoice::parse) {
                Some(choice) => session.backend = choice,
                None => {
                    eprintln!("tuna-compositor: --backend takes auto, winit, or drm");
                    return Some(ExitCode::FAILURE);
                }
            },
            "--version" | "-V" => {
                println!("tuna-compositor {}", release_version());
                return Some(ExitCode::SUCCESS);
            }
            "--help" | "-h" => {
                print!("{HELP}");
                return Some(ExitCode::SUCCESS);
            }
            other => {
                eprintln!("tuna-compositor: unknown argument {other}");
                return Some(ExitCode::FAILURE);
            }
        }
    }
    None
}

/// The user-settings upgrade belongs to a user's session; the login
/// screen runs as GDM's dynamic greeter user.
fn upgrades_user_settings(session: &NestedSession) -> bool {
    !session.greeter
}

/// A finished run's exit code: failure when the greeter's restart budget
/// ran out, so the unit fails and GDM restarts the login screen.
fn exit_code(stats: &RunStats) -> ExitCode {
    if stats.greeter_exhausted {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn main() -> ExitCode {
    // Internal authentication process: never initialize a display or user directories.
    if std::env::args_os().nth(1).as_deref()
        == Some(std::ffi::OsStr::new(tuna_compositor::pam_helper::FLAG))
    {
        if std::env::args_os().count() != 2 {
            return ExitCode::FAILURE;
        }
        return tuna_compositor::pam_helper::entry();
    }
    // Before any thread starts: full-screen pixel buffers go back to the
    // kernel when dropped (see tuna_compositor::malloc).
    if !tuna_compositor::malloc::pin_mmap_threshold() {
        eprintln!("tuna-compositor: malloc mmap threshold left to the C library");
    }
    // Client connections are descriptors: start from the hard limit, as
    // Mutter does; spawned processes get the original back.
    match tuna_compositor::nofile::raise() {
        Some((old, new)) if old != new => {
            eprintln!("tuna-compositor: open-file limit raised from {old} to {new}");
        }
        Some(_) => {}
        None => eprintln!("tuna-compositor: open-file limit unchanged (could not raise it)"),
    }
    let mut session = NestedSession::default_for_pid();
    session.xwayland = tuna_compositor::runtime::xwayland_wanted(
        std::env::var_os("TUNA_XWAYLAND"),
        std::env::var_os("PATH"),
    );
    // Output scale (#59): TUNA_SCALE=1.5, or --scale.
    if let Some(scale) = std::env::var("TUNA_SCALE")
        .ok()
        .and_then(|v| v.parse().ok())
    {
        session.scale = scale;
    }
    if let Some(code) = parse_args(std::env::args().skip(1), &mut session) {
        return code;
    }
    if upgrades_user_settings(&session) {
        if let Err(error) = tuna_compositor::user_upgrade::prepare() {
            eprintln!("tuna-compositor: {error}");
            return ExitCode::FAILURE;
        }
    }
    println!(
        "tuna-compositor: nested session on {} ({}x{})",
        session.socket_name, session.width, session.height
    );
    match run(&session) {
        Ok(stats) => {
            println!(
                "tuna-compositor: shutdown after {} frames, {} clients, {} shell restarts",
                stats.frames, stats.clients, stats.shell_restarts
            );
            exit_code(&stats)
        }
        Err(err) => {
            eprintln!("tuna-compositor: {err}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod version_tests {
    use super::normalize_version;

    #[test]
    fn tag_prefix_is_stripped() {
        assert_eq!(normalize_version(Some("v1.2.3"), "0.1.0"), "1.2.3");
    }

    #[test]
    fn plain_version_is_kept() {
        assert_eq!(normalize_version(Some("1.2.3"), "0.1.0"), "1.2.3");
    }

    #[test]
    fn missing_or_empty_falls_back() {
        assert_eq!(normalize_version(None, "0.1.0"), "0.1.0");
        assert_eq!(normalize_version(Some(""), "0.1.0"), "0.1.0");
    }
}

#[cfg(test)]
mod cli_tests {
    use super::*;

    fn parse(list: &[&str]) -> (NestedSession, Option<ExitCode>) {
        let mut session = NestedSession::new("tuna-nested-test".into(), 1280, 800);
        let code = parse_args(list.iter().map(|a| (*a).to_owned()), &mut session);
        (session, code)
    }

    #[test]
    fn greeter_flag_makes_a_login_screen_session() {
        let (plain, code) = parse(&["--socket", "tuna-x"]);
        assert_eq!(code, None);
        assert!(!plain.greeter);
        assert!(upgrades_user_settings(&plain));
        let (greeter, code) = parse(&["--backend", "winit", "--greeter", "--startup-overview"]);
        assert_eq!(code, None, "--greeter runs the session");
        assert!(greeter.greeter);
        assert!(
            !upgrades_user_settings(&greeter),
            "the greeter user's settings are not upgraded"
        );
        assert_eq!(parse(&["--greeter=yes"]).1, Some(ExitCode::FAILURE));
    }

    #[test]
    fn help_documents_greeter_mode() {
        assert_eq!(parse(&["--help"]).1, Some(ExitCode::SUCCESS));
        let usage = HELP.lines().find(|l| l.starts_with("Usage:")).unwrap();
        assert!(usage.ends_with(" [--greeter]"), "{usage}");
        assert!(HELP.contains("\n--greeter runs GDM's login screen instead of a user\n"));
        assert!(HELP.contains("\nno lock screen, overview or Tuna session target.\n"));
    }

    #[test]
    fn a_spent_greeter_budget_fails_the_unit() {
        assert_eq!(exit_code(&RunStats::default()), ExitCode::SUCCESS);
        let spent = RunStats {
            greeter_exhausted: true,
            ..RunStats::default()
        };
        assert_eq!(exit_code(&spent), ExitCode::FAILURE);
    }
}
