//! Publish the hardware session's display to D-Bus and systemd activation.
//! Nested sessions never change the host user's activation environment.

use std::process::{Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(5);

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
        let args = ["--user", "--no-block", "stop", "tuna-session.target"]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let _ = run("systemctl", &args);
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
    // GTK can activate portals before the display is ready. After the shell
    // owns its bus name, clear any start-limit failure and refresh the portal
    // frontend so it chooses Tuna Desktop's backends using the imported desktop.
    std::thread::spawn(move || {
        let args: Vec<String> = [
            "call",
            "--session",
            "--dest",
            "org.freedesktop.DBus",
            "--object-path",
            "/org/freedesktop/DBus",
            "--method",
            "org.freedesktop.DBus.GetNameOwner",
            "org.gnome.Shell",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        let start = Instant::now();
        while alive.load(Ordering::Acquire) && start.elapsed() < Duration::from_secs(15) {
            if run("gdbus", &args) {
                // Tell GDM the session and display are ready rather than
                // letting its timed non-GNOME fallback hand off plymouth.
                let mut registration = [
                    "call",
                    "--system",
                    "--dest",
                    "org.gnome.DisplayManager",
                    "--object-path",
                    "/org/gnome/DisplayManager/Manager",
                    "--method",
                    "org.gnome.DisplayManager.Manager.RegisterSession",
                ]
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>();
                if run("gdbus", &registration) {
                    *registration.last_mut().unwrap() =
                        "org.gnome.DisplayManager.Manager.RegisterDisplay".into();
                    if run("gdbus", &registration) {
                        eprintln!(
                            "tuna-compositor: session services: GDM session and display registered"
                        );
                    } else {
                        eprintln!(
                            "tuna-compositor: session services: GDM display registration failed"
                        );
                    }
                }

                if !alive.load(Ordering::Acquire) {
                    return;
                }
                // systemd refuses a manual start of graphical-session.target.
                // Our packaged target pulls it in through BindsTo instead.
                let start_graphical = ["--user", "start", "tuna-session.target"]
                    .into_iter()
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
                if !run("systemctl", &start_graphical) {
                    eprintln!("tuna-compositor: session services: graphical target failed");
                    return;
                }
                // Drop may race the worker's start. Stop again if shutdown
                // occurred before systemd acknowledged the target.
                if !alive.load(Ordering::Acquire) {
                    let stop = ["--user", "--no-block", "stop", "tuna-session.target"]
                        .into_iter()
                        .map(str::to_owned)
                        .collect::<Vec<_>>();
                    let _ = run("systemctl", &stop);
                    return;
                }
                eprintln!("tuna-compositor: session services: graphical target active");

                let reset = [
                    "--user",
                    "reset-failed",
                    "xdg-desktop-portal-gtk.service",
                    "xdg-desktop-portal-gnome.service",
                ]
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>();
                let _ = run("systemctl", &reset);
                let restart = [
                    "--user",
                    "--no-block",
                    "restart",
                    "xdg-desktop-portal.service",
                ]
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>();
                if run("systemctl", &restart) {
                    eprintln!("tuna-compositor: session services: portals refreshed");
                }
                return;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        eprintln!("tuna-compositor: session services: shell bus name not ready");
    });
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
}
