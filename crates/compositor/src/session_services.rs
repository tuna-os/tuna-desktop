//! Publish the hardware session's display to D-Bus and systemd activation.
//! Nested sessions never change the host user's activation environment.

use std::process::{Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(5);
/// How often to look for the shell's bus name. There is no deadline: a late
/// shell delays only GDM's registration, never the graphical session.
const SHELL_POLL: Duration = Duration::from_millis(200);
/// When to say the shell is late (it is still waited for).
const SHELL_LATE: Duration = Duration::from_secs(15);

/// A session utility runner: `(program, args) -> succeeded`.
type Runner<'a> = dyn FnMut(&str, &[String]) -> bool + 'a;

fn argv(list: &[&str]) -> Vec<String> {
    list.iter().map(|a| (*a).to_owned()).collect()
}

/// Run a session utility with a deadline; an absent service cannot stall login.
fn run(program: &str, args: &[String]) -> bool {
    let Ok(mut child) = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if start.elapsed() < TIMEOUT => {
                std::thread::sleep(Duration::from_millis(25));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

/// The display environment activated services need. An empty `DISPLAY`
/// clears a previous session's X display; the portal uses Wayland.
fn activation_vars<'a>(socket: &'a str, desktop: &'a str) -> [(&'static str, &'a str); 4] {
    [
        ("WAYLAND_DISPLAY", socket),
        ("XDG_CURRENT_DESKTOP", desktop),
        ("XDG_SESSION_TYPE", "wayland"),
        ("DISPLAY", ""),
    ]
}

/// systemd's user manager starts the portals and every unit-backed D-Bus
/// service. `systemctl` ships with systemd; `dbus-update-activation-environment`
/// does not (Fedora and EL put it in dbus-tools, which images need not carry).
fn systemd_import_args(vars: &[(&str, &str)]) -> Vec<String> {
    let mut args = vec!["--user".to_owned(), "set-environment".to_owned()];
    args.extend(vars.iter().map(|(name, value)| format!("{name}={value}")));
    args
}

/// The bus's own activation environment, for services it starts without
/// systemd. `gdbus` ships with glib2, which the shell already needs.
fn bus_import_args(vars: &[(&str, &str)]) -> Vec<String> {
    let quote = |text: &str| format!("'{}'", text.replace('\\', "\\\\").replace('\'', "\\'"));
    let entries: Vec<String> = vars
        .iter()
        .map(|(name, value)| format!("{}: {}", quote(name), quote(value)))
        .collect();
    [
        "call",
        "--session",
        "--dest",
        "org.freedesktop.DBus",
        "--object-path",
        "/org/freedesktop/DBus",
        "--method",
        "org.freedesktop.DBus.UpdateActivationEnvironment",
    ]
    .into_iter()
    .map(str::to_owned)
    .chain(std::iter::once(format!("{{{}}}", entries.join(", "))))
    .collect()
}

/// Holds the systemd graphical session open for the DRM runtime's lifetime.
pub struct SessionServices {
    alive: Arc<AtomicBool>,
}
impl Drop for SessionServices {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Release);
        let _ = run("systemctl", &argv(STOP_TARGET));
    }
}

const STOP_TARGET: &[&str] = &["--user", "--no-block", "stop", "tuna-session.target"];
/// Enqueue the start job without waiting for it; the worker waits.
const QUEUE_TARGET: &[&str] = &["--user", "--no-block", "start", "tuna-session.target"];

/// The session worker. The graphical session starts as soon as activated
/// services can see the display; it never waits on the shell. On
/// dbus-daemon a client that activates the GNOME portal before
/// graphical-session.target exists blocks for the whole D-Bus timeout
/// instead of failing fast as on dbus-broker, so gating the target on the
/// shell's bus name could skip it, and with it the portals, for good.
/// GDM is told the session is ready only once the shell owns its name.
fn bring_up(alive: &AtomicBool, run: &mut Runner<'_>, sleep: &mut dyn FnMut(Duration)) {
    if !start_graphical_session(alive, run) {
        return;
    }
    if wait_for_shell(alive, run, sleep) {
        register_with_gdm(run);
    }
}

/// Start tuna-session.target, then clear and refresh portal backends that
/// were activated before it existed. False when shutdown overtook it.
fn start_graphical_session(alive: &AtomicBool, run: &mut Runner<'_>) -> bool {
    if !alive.load(Ordering::Acquire) {
        return false;
    }
    // systemd refuses a manual start of graphical-session.target.
    // Our packaged target pulls it in through BindsTo instead.
    if !run(
        "systemctl",
        &argv(&["--user", "start", "tuna-session.target"]),
    ) {
        eprintln!("tuna-compositor: session services: graphical target failed");
    } else if !alive.load(Ordering::Acquire) {
        // Drop may race the start. Stop again if shutdown occurred
        // before systemd acknowledged the target.
        let _ = run("systemctl", &argv(STOP_TARGET));
        return false;
    } else {
        eprintln!("tuna-compositor: session services: graphical target active");
    }
    // A portal backend activated before graphical-session.target (which
    // xdg-desktop-portal-gnome requires) failed or hit its start limit.
    // Clear that and refresh the frontend so it picks Tuna Desktop's
    // backends using the imported desktop.
    let _ = run(
        "systemctl",
        &argv(&[
            "--user",
            "reset-failed",
            "xdg-desktop-portal-gtk.service",
            "xdg-desktop-portal-gnome.service",
        ]),
    );
    if run(
        "systemctl",
        &argv(&[
            "--user",
            "--no-block",
            "restart",
            "xdg-desktop-portal.service",
        ]),
    ) {
        eprintln!("tuna-compositor: session services: portals refreshed");
    }
    true
}

/// Wait, for as long as the session lives, until the shell owns
/// `org.gnome.Shell`. False when the session ended first.
fn wait_for_shell(
    alive: &AtomicBool,
    run: &mut Runner<'_>,
    sleep: &mut dyn FnMut(Duration),
) -> bool {
    let owner = argv(&[
        "call",
        "--session",
        "--dest",
        "org.freedesktop.DBus",
        "--object-path",
        "/org/freedesktop/DBus",
        "--method",
        "org.freedesktop.DBus.GetNameOwner",
        "org.gnome.Shell",
    ]);
    let mut waited = Duration::ZERO;
    let mut reported = false;
    while alive.load(Ordering::Acquire) {
        if run("gdbus", &owner) {
            return true;
        }
        if !reported && waited >= SHELL_LATE {
            reported = true;
            eprintln!("tuna-compositor: session services: shell bus name not ready; still waiting");
        }
        sleep(SHELL_POLL);
        waited += SHELL_POLL;
    }
    false
}

/// Tell GDM the session and display are ready rather than letting its
/// timed non-GNOME fallback hand off plymouth.
fn register_with_gdm(run: &mut Runner<'_>) {
    let mut registration = argv(&[
        "call",
        "--system",
        "--dest",
        "org.gnome.DisplayManager",
        "--object-path",
        "/org/gnome/DisplayManager/Manager",
        "--method",
        "org.gnome.DisplayManager.Manager.RegisterSession",
    ]);
    if !run("gdbus", &registration) {
        return;
    }
    *registration.last_mut().unwrap() = "org.gnome.DisplayManager.Manager.RegisterDisplay".into();
    if run("gdbus", &registration) {
        eprintln!("tuna-compositor: session services: GDM session and display registered");
    } else {
        eprintln!("tuna-compositor: session services: GDM display registration failed");
    }
}

/// Called only for the actual DRM backend, before its shell is started.
pub fn publish(socket: &str) -> SessionServices {
    let alive = Arc::new(AtomicBool::new(true));
    let guard = SessionServices {
        alive: alive.clone(),
    };
    let desktop = std::env::var("XDG_CURRENT_DESKTOP")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "Tuna:GNOME".into());
    let vars = activation_vars(socket, &desktop);
    let systemd = run("systemctl", &systemd_import_args(&vars));
    let bus = run("gdbus", &bus_import_args(&vars));
    if !systemd && !bus {
        eprintln!("tuna-compositor: session services: display environment import failed");
        return guard;
    }
    eprintln!(
        "tuna-compositor: session services: display environment imported (systemd={systemd} bus={bus})"
    );
    // Queue the graphical session before returning: the shell is spawned on
    // the first loop tick, after this. A portal its toolkit activates then
    // waits behind graphical-session.target (xdg-desktop-portal-gnome is
    // After= and Requisite= it) instead of failing the requisite, which on
    // dbus-daemon leaves the caller hanging for the whole D-Bus timeout.
    if systemd && !run("systemctl", &argv(QUEUE_TARGET)) {
        eprintln!("tuna-compositor: session services: graphical target could not be queued");
    }
    std::thread::spawn(move || bring_up(&alive, &mut run, &mut std::thread::sleep));
    guard
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activation_import_names_only_the_hardware_display() {
        let vars = activation_vars("wayland-tuna", "Tuna:GNOME");
        let args = systemd_import_args(&vars);
        assert_eq!(
            args,
            [
                "--user",
                "set-environment",
                "WAYLAND_DISPLAY=wayland-tuna",
                "XDG_CURRENT_DESKTOP=Tuna:GNOME",
                "XDG_SESSION_TYPE=wayland",
                "DISPLAY="
            ]
        );
        assert!(!args.iter().any(|a| a == "import-environment"));
        let bus = bus_import_args(&vars);
        assert_eq!(
            bus.last().map(String::as_str),
            Some(
                "{'WAYLAND_DISPLAY': 'wayland-tuna', 'XDG_CURRENT_DESKTOP': 'Tuna:GNOME', \
                 'XDG_SESSION_TYPE': 'wayland', 'DISPLAY': ''}"
            )
        );
        assert!(bus
            .iter()
            .any(|a| a == "org.freedesktop.DBus.UpdateActivationEnvironment"));
    }

    #[test]
    fn the_bus_import_quotes_values_as_gvariant_strings() {
        let bus = bus_import_args(&[("XDG_CURRENT_DESKTOP", r"it's\here")]);
        assert_eq!(
            bus.last().map(String::as_str),
            Some(r"{'XDG_CURRENT_DESKTOP': 'it\'s\\here'}")
        );
    }

    #[test]
    fn session_utility_failures_are_reported() {
        assert!(!run("/definitely/missing/tuna-session-tool", &[]));
        assert!(!run("/bin/false", &[]));
        assert!(run("/bin/true", &[]));
    }

    /// A recorded session-utility call, as `program arg…`.
    fn line(program: &str, args: &[String]) -> String {
        std::iter::once(program.to_owned())
            .chain(args.iter().cloned())
            .collect::<Vec<_>>()
            .join(" ")
    }

    const START: &str = "systemctl --user start tuna-session.target";
    const PORTALS: &str = "systemctl --user --no-block restart xdg-desktop-portal.service";

    fn position(calls: &[String], needle: &str) -> usize {
        calls
            .iter()
            .position(|c| c.contains(needle))
            .unwrap_or_else(|| panic!("{needle} never ran: {calls:#?}"))
    }

    /// The shell claims `org.gnome.Shell` only after `owned_after` of
    /// polling (simulated time); `None` means never.
    fn session(owned_after: Option<Duration>, alive_for: Duration) -> Vec<String> {
        let alive = AtomicBool::new(true);
        let clock = std::cell::Cell::new(Duration::ZERO);
        let calls = std::cell::RefCell::new(Vec::new());
        let mut run = |program: &str, args: &[String]| {
            calls.borrow_mut().push(line(program, args));
            if args.iter().any(|a| a == "org.gnome.Shell") {
                owned_after.is_some_and(|t| clock.get() >= t)
            } else {
                true
            }
        };
        let mut sleep = |d: Duration| {
            clock.set(clock.get() + d);
            if clock.get() >= alive_for {
                alive.store(false, Ordering::Release);
            }
        };
        bring_up(&alive, &mut run, &mut sleep);
        calls.into_inner()
    }

    #[test]
    fn the_graphical_session_starts_before_a_late_shell_owns_its_name() {
        // dbus-daemon: the shell's portal activation blocked ~50 s.
        let calls = session(Some(Duration::from_secs(60)), Duration::from_secs(600));
        let start = position(&calls, START);
        let portals = position(&calls, PORTALS);
        let first_poll = position(&calls, "org.gnome.Shell");
        let register = position(&calls, "RegisterSession");
        assert!(start < portals && portals < first_poll, "{calls:#?}");
        // Waited well past the old 15 s deadline, then told GDM.
        let polls = calls
            .iter()
            .filter(|c| c.contains("org.gnome.Shell"))
            .count();
        assert_eq!(polls, 301, "60 s at 200 ms, then the owned poll");
        assert!(first_poll < register);
        assert!(calls[register + 1].contains("RegisterDisplay"));
        assert_eq!(calls.iter().filter(|c| c.contains(START)).count(), 1);
    }

    #[test]
    fn a_shell_that_never_appears_still_gets_the_graphical_session_and_portals() {
        let calls = session(None, Duration::from_secs(120));
        assert!(position(&calls, START) < position(&calls, PORTALS));
        assert!(!calls.iter().any(|c| c.contains("DisplayManager")));
        assert!(!calls.iter().any(|c| c.contains("stop tuna-session.target")));
    }

    #[test]
    fn shutdown_during_the_start_stops_the_target_and_skips_the_shell() {
        let alive = AtomicBool::new(true);
        let mut calls = Vec::new();
        let mut run = |program: &str, args: &[String]| {
            let call = line(program, args);
            if call == START {
                alive.store(false, Ordering::Release);
            }
            calls.push(call);
            true
        };
        bring_up(&alive, &mut run, &mut |_| {});
        assert_eq!(
            calls,
            [
                START,
                "systemctl --user --no-block stop tuna-session.target"
            ]
        );
    }
}
