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
    leader: Option<Leader>,
}
impl Drop for SessionServices {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Release);
        HARDWARE_SESSION.store(false, Ordering::Release);
        let _ = run("systemctl", &argv(STOP_TARGET));
        if let Some(leader) = self.leader.take() {
            // gnome-session 51's leader on session end: start the shutdown
            // target, then hang up the FIFO the monitor watches (which
            // starts it again on a crash, when this never runs). Stopping
            // tuna-session.target alone leaves gnome-session.target and
            // with it graphical-session.target and the plugins running.
            let _ = run("systemctl", &argv(SHUTDOWN_GNOME_SESSION));
            drop(leader);
        }
    }
}

const STOP_TARGET: &[&str] = &["--user", "--no-block", "stop", "tuna-session.target"];
/// Enqueue the start job without waiting for it; the worker waits.
const QUEUE_TARGET: &[&str] = &["--user", "--no-block", "start", "tuna-session.target"];
/// `leader_term_or_int_signal_cb` in gnome-session 51 `leader-systemd.c`.
const SHUTDOWN_GNOME_SESSION: &[&str] = &[
    "--user",
    "--no-block",
    "--job-mode=replace-irreversibly",
    "start",
    "gnome-session-shutdown.target",
];

/// gnome-session 51 names the leader FIFO in `leader-systemd.c` and
/// `gnome-session-ctl.c`.
const LEADER_FIFO: &str = "gnome-session-leader-fifo";

/// The session leader's end of GNOME's session monitor. tuna-session.target
/// pulls in gnome-session.target (for the settings daemon plugins, whose
/// units require gnome-session-initialized.target); that target binds to
/// gnome-session-monitor.service, which runs `gnome-session-ctl --monitor`
/// on the read end of the FIFO and starts gnome-session-shutdown.target
/// when the writer hangs up. gnome-session 51's leader holds the write end
/// for the session's life; the compositor is the Tuna session's leader, so
/// its exit, clean or not, stops those targets and the plugins with them.
struct Leader {
    slot: Arc<std::sync::Mutex<LeaderSlot>>,
}

#[derive(Default)]
struct LeaderSlot {
    fifo: Option<std::fs::File>,
    released: bool,
}

impl Leader {
    /// Create the FIFO like the GNOME leader (mode 0666, an existing one
    /// reused) and open its write end on a thread: the open waits for the
    /// monitor's read end.
    fn hold(path: std::path::PathBuf) -> Self {
        let slot: Arc<std::sync::Mutex<LeaderSlot>> = Default::default();
        let held = slot.clone();
        let _ = std::thread::Builder::new()
            .name("tuna-session-leader".into())
            .spawn(move || {
                use std::os::unix::ffi::OsStrExt;
                let Ok(name) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
                    return;
                };
                // SAFETY: `name` is a valid NUL-terminated path.
                if unsafe { libc::mkfifo(name.as_ptr(), 0o666) } != 0 {
                    let error = std::io::Error::last_os_error();
                    if error.raw_os_error() != Some(libc::EEXIST) {
                        eprintln!("tuna-compositor: session services: leader FIFO: {error}");
                        return;
                    }
                }
                let fifo = match std::fs::OpenOptions::new().write(true).open(&path) {
                    Ok(fifo) => fifo,
                    Err(error) => {
                        eprintln!("tuna-compositor: session services: leader FIFO: {error}");
                        return;
                    }
                };
                use std::os::unix::fs::FileTypeExt;
                if !fifo.metadata().is_ok_and(|m| m.file_type().is_fifo()) {
                    eprintln!("tuna-compositor: session services: leader FIFO is not a FIFO");
                    return;
                }
                if let Ok(mut slot) = held.lock() {
                    if !slot.released {
                        slot.fifo = Some(fifo);
                        eprintln!(
                            "tuna-compositor: session services: holding the session leader FIFO"
                        );
                    }
                }
            });
        Self { slot }
    }
}

impl Drop for Leader {
    fn drop(&mut self) {
        if let Ok(mut slot) = self.slot.lock() {
            slot.released = true;
            slot.fifo = None;
        }
    }
}

/// Whether this process published a hardware session (X11 service
/// targets follow Xwayland only there).
static HARDWARE_SESSION: AtomicBool = AtomicBool::new(false);

/// Whether this compositor runs the hardware session (its logind
/// session is its own, so session-ending calls may act on it).
pub fn hardware_session() -> bool {
    HARDWARE_SESSION.load(Ordering::Acquire)
}

/// GNOME Shell 51 starts gnome-session-x11-services-ready.target once
/// Xwayland's display is set up, which brings gsd-xsettings up
/// (`windowManager.js` `_startX11Services`, mode "fail"). Mutter has
/// already published `DISPLAY`; Tuna publishes it when the display
/// becomes ready.
fn x11_ready_calls(display: u32) -> Vec<(&'static str, Vec<String>)> {
    let value = format!(":{display}");
    let vars = [("DISPLAY", value.as_str())];
    vec![
        ("systemctl", systemd_import_args(&vars)),
        ("gdbus", bus_import_args(&vars)),
        (
            "systemctl",
            argv(&[
                "--user",
                "--no-block",
                "--job-mode=fail",
                "start",
                "gnome-session-x11-services-ready.target",
            ]),
        ),
    ]
}

/// `_stopX11Services`: the display closed.
fn x11_closed_calls() -> Vec<(&'static str, Vec<String>)> {
    vec![(
        "systemctl",
        argv(&[
            "--user",
            "--no-block",
            "--job-mode=fail",
            "stop",
            "gnome-session-x11-services.target",
        ]),
    )]
}

fn run_detached(calls: Vec<(&'static str, Vec<String>)>) {
    if !HARDWARE_SESSION.load(Ordering::Acquire) {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("tuna-x11-services".into())
        .spawn(move || {
            for (program, args) in calls {
                if !run(program, &args) {
                    eprintln!("tuna-compositor: session services: X11 services: {program} failed");
                }
            }
        });
}

/// Xwayland's window manager is up on `:display`.
pub fn x11_display_ready(display: u32) {
    run_detached(x11_ready_calls(display));
}

/// Xwayland went away.
pub fn x11_display_closed() {
    run_detached(x11_closed_calls());
}

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
    let mut guard = SessionServices {
        alive: alive.clone(),
        leader: None,
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
    if systemd {
        HARDWARE_SESSION.store(true, Ordering::Release);
        match std::env::var_os("XDG_RUNTIME_DIR").filter(|dir| !dir.is_empty()) {
            Some(dir) => {
                guard.leader = Some(Leader::hold(std::path::Path::new(&dir).join(LEADER_FIFO)))
            }
            None => eprintln!(
                "tuna-compositor: session services: no XDG_RUNTIME_DIR for the leader FIFO"
            ),
        }
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

    #[test]
    fn the_leader_fifo_hangs_up_for_the_monitor_when_the_session_ends() {
        use std::io::Read;
        use std::os::unix::fs::FileTypeExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LEADER_FIFO);
        let leader = Leader::hold(path.clone());
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !std::fs::metadata(&path).is_ok_and(|m| m.file_type().is_fifo()) {
            assert!(std::time::Instant::now() < deadline, "no leader FIFO");
            std::thread::sleep(Duration::from_millis(10));
        }
        // gnome-session-ctl --monitor's side: a blocking read-only open,
        // then a read that returns at the writer's hang-up.
        let (done, hung_up) = std::sync::mpsc::channel();
        let monitor = std::thread::spawn({
            let path = path.clone();
            move || {
                let mut fifo = std::fs::File::open(&path).unwrap();
                let mut byte = [0u8; 1];
                let read = fifo.read(&mut byte).unwrap();
                done.send(read).unwrap();
            }
        });
        let held = std::time::Instant::now() + Duration::from_secs(5);
        while leader.slot.lock().unwrap().fifo.is_none() {
            assert!(
                std::time::Instant::now() < held,
                "leader never held the FIFO"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(hung_up.recv_timeout(Duration::from_millis(200)).is_err());
        drop(leader);
        assert_eq!(hung_up.recv_timeout(Duration::from_secs(5)), Ok(0));
        monitor.join().unwrap();
    }

    #[test]
    fn xwayland_drives_gnome_shells_x11_service_targets() {
        let ready: Vec<String> = x11_ready_calls(2)
            .into_iter()
            .map(|(program, args)| line(program, &args))
            .collect();
        assert_eq!(
            ready,
            [
                "systemctl --user set-environment DISPLAY=:2",
                "gdbus call --session --dest org.freedesktop.DBus --object-path \
                 /org/freedesktop/DBus --method \
                 org.freedesktop.DBus.UpdateActivationEnvironment {'DISPLAY': ':2'}",
                "systemctl --user --no-block --job-mode=fail start \
                 gnome-session-x11-services-ready.target",
            ]
        );
        let closed: Vec<String> = x11_closed_calls()
            .into_iter()
            .map(|(program, args)| line(program, &args))
            .collect();
        assert_eq!(
            closed,
            ["systemctl --user --no-block --job-mode=fail stop gnome-session-x11-services.target"]
        );
    }

    /// `Wants=` units of the shipped session target.
    fn wanted() -> Vec<String> {
        include_str!("../../../share/systemd/user/tuna-session.target")
            .lines()
            .filter_map(|line| line.strip_prefix("Wants="))
            .flat_map(|units| units.split_whitespace().map(str::to_owned))
            .collect()
    }

    #[test]
    fn the_session_target_starts_gnome_51s_plugins_except_those_tuna_owns() {
        let wanted = wanted();
        // gnome-session 51 gnome-session-basic-services.target and
        // gnome-session-services.target, minus Tuna's own services.
        for plugin in [
            "Datetime",
            "Housekeeping",
            "Keyboard",
            "MediaKeys",
            "Power",
            "PrintNotifications",
            "Rfkill",
            "ScreensaverProxy",
            "Sharing",
            "Smartcard",
            "Sound",
            "UsbProtection",
            "Wwan",
        ] {
            let unit = format!("org.gnome.SettingsDaemon.{plugin}.target");
            assert!(wanted.contains(&unit), "{unit} is not started: {wanted:?}");
        }
        // The compositor owns org.gnome.SettingsDaemon.Color (Night
        // Light) and runs Orca, which gsd-a11y-settings would start again.
        for owned in ["Color", "A11ySettings"] {
            let unit = format!("org.gnome.SettingsDaemon.{owned}.target");
            assert!(!wanted.contains(&unit), "{unit} conflicts with Tuna");
        }
        // XSettings follows Xwayland (gnome-session-x11-services.target),
        // and the whole service sets would start the conflicting plugins.
        for indirect in [
            "org.gnome.SettingsDaemon.XSettings.target",
            "gnome-session-basic-services.target",
            "gnome-session-services.target",
        ] {
            assert!(!wanted.iter().any(|u| u == indirect), "{indirect} wanted");
        }
        // Every plugin target is Requisite= gnome-session-initialized.target,
        // itself Requisite= gnome-session.target: same transaction.
        assert!(wanted
            .iter()
            .any(|u| u == "gnome-session-initialized.target"));
        assert!(wanted.iter().any(|u| u == "gnome-session.target"));
    }
}
