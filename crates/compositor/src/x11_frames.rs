//! The X11 frames client (P-WM-10): the process that draws title bars for
//! X11 windows, and its supervision.
//!
//! Mutter 51 does not draw X11 window decorations itself. It runs
//! `mutter-x11-frames`, a GTK4 X11 client that creates one frame window
//! with a header bar per decorated window, and reparents the window into
//! it (`src/x11/meta-x11-frame.c` `meta_frame_launch_client`,
//! `src/x11/meta-x11-display.c` `meta_x11_display_init_frames_client` and
//! `on_frames_client_died`). Tuna Desktop runs `tuna-x11-frames`, which
//! speaks the same X11 protocol; the window-manager half of that protocol
//! is in the vendored Smithay (`third-party/smithay/ROOST-PATCH.md`) and
//! the decisions in [`crate::xwayland`].
//!
//! As in Mutter the client is an X11 client of the compatibility server:
//! it starts once the window manager is ready, with `DISPLAY` naming that
//! server, and goes with it. Like the XIM helper in [`crate::ime`] it is
//! killed and reaped on server loss before a fresh one may start.
//!
//! Divergences from Mutter:
//!
//! - Mutter launches from its libexec directory and relaunches only a
//!   client that died of a signal other than SIGTERM or SIGKILL. Here the
//!   binary is found like the other helpers (beside the compositor, then
//!   on `PATH`; [`OVERRIDE_ENV`] names another, such as the stock
//!   `mutter-x11-frames`, or `0` for none), and any exit restarts it, a
//!   bounded number of times with backoff.
//! - Without the binary, or once the restart budget is spent, the session
//!   goes on and X11 windows map undecorated, as they did before frames.
//! - [`X11Frames::offer_due`]: Smithay's window manager keeps an unframed
//!   window in a bare wrapper, not on the root window where Mutter's
//!   frames client looks for windows when it starts. While the client
//!   starts, and while a window waits for its frame, the window manager
//!   offers such windows again (`X11Wm::request_frames`).
//!
//! The GTK header bars take their button layout and title-bar click
//! actions from XSETTINGS, which gnome-settings-daemon publishes in a
//! GNOME session. [`X11Frames::take_xsettings`] hands the window manager
//! the same four settings ([`TitlebarSettings::xsettings`]) for its own
//! XSETTINGS selection.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};

use crate::focus_policy::TitlebarSettings;
use crate::supervise::RestartPolicy;

/// The frames client's executable name.
pub const BINARY: &str = "tuna-x11-frames";
/// Names another frames client (a path, or a name on `PATH`); empty or
/// `0` runs none.
pub const OVERRIDE_ENV: &str = "TUNA_X11_FRAMES";

/// Pause between offers of unframed windows to the frames client.
const OFFER_INTERVAL_MS: u64 = 100;
/// How long after a start the offers go on: until the client is surely
/// listening on the root window.
const OFFER_WINDOW_MS: u64 = 10_000;
/// A client that ran this long gets its restart budget back.
const HEALTHY_MS: u64 = 60_000;

/// Starts or reports the frames client for one X11 display; `None` when
/// there is no binary to run.
type Launch<'a> = &'a mut dyn FnMut(u32) -> Option<std::io::Result<Child>>;

/// The frames client to run: the override when set (nothing for an empty
/// or `0` value, a path as it is, a bare name from `PATH`), otherwise
/// [`BINARY`] beside the compositor's executable, otherwise on `PATH`.
pub fn resolve(
    override_value: Option<&OsStr>,
    exe_dir: Option<&Path>,
    path: Option<&OsStr>,
    is_file: &dyn Fn(&Path) -> bool,
) -> Option<PathBuf> {
    let on_path = |name: &OsStr| {
        std::env::split_paths(path?)
            .map(|dir| dir.join(name))
            .find(|candidate| is_file(candidate))
    };
    match override_value {
        Some(value) if value.is_empty() || value == "0" => None,
        Some(value) => {
            let named = Path::new(value);
            if named
                .parent()
                .is_some_and(|dir| !dir.as_os_str().is_empty())
            {
                is_file(named).then(|| named.to_path_buf())
            } else {
                on_path(value)
            }
        }
        None => exe_dir
            .map(|dir| dir.join(BINARY))
            .filter(|beside| is_file(beside))
            .or_else(|| on_path(OsStr::new(BINARY))),
    }
}

/// Mutter's `meta_frame_launch_client`: the client with `DISPLAY` set.
fn spawn(bin: &Path, display: u32) -> std::io::Result<Child> {
    let mut command = Command::new(bin);
    command
        .env("DISPLAY", format!(":{display}"))
        // An X11 client by definition: frames/main.c allows GDK no other
        // backend, and no session variable may send a GTK client to the
        // compositor's Wayland socket instead.
        .env("GDK_BACKEND", "x11")
        .env_remove("WAYLAND_SOCKET");
    // The stock client loads libadwaita only in a session it knows as
    // GNOME (frames/main.c `should_load_libadwaita`); ask for it outright
    // so the header bar is GNOME's under this desktop's name too.
    if std::env::var_os("MUTTER_FRAMES_PLATFORM_LIBRARY").is_none() {
        command.env("MUTTER_FRAMES_PLATFORM_LIBRARY", "adwaita");
    }
    command.spawn()
}

/// The supervised frames client.
#[derive(Debug)]
pub struct X11Frames {
    /// Three restarts after the first start, 500 ms backoff doubling to
    /// 5 s at most, as for the compatibility server itself.
    policy: RestartPolicy,
    /// The display whose window manager is ready.
    display: Option<u32>,
    child: Option<Child>,
    /// A killed client not reaped yet: no new one starts before it is.
    retired: Option<Child>,
    /// Spawn attempts on this display, the first start included.
    starts: u32,
    next_ms: u64,
    started_ms: u64,
    offer_until_ms: u64,
    next_offer_ms: u64,
    /// No binary, or the restart budget is spent: nothing more starts on
    /// this display.
    gave_up: bool,
    titlebar: TitlebarSettings,
    /// The current title-bar settings reached this display's XSETTINGS.
    published: bool,
}

impl Default for X11Frames {
    fn default() -> Self {
        Self::new()
    }
}

impl X11Frames {
    /// An idle supervisor: nothing runs before a display is ready.
    pub fn new() -> Self {
        Self {
            policy: RestartPolicy::new(3, 500, 5_000),
            display: None,
            child: None,
            retired: None,
            starts: 0,
            next_ms: 0,
            started_ms: 0,
            offer_until_ms: 0,
            next_offer_ms: 0,
            gave_up: false,
            titlebar: TitlebarSettings::default(),
            published: false,
        }
    }

    fn reap_retired(&mut self) {
        if self
            .retired
            .as_mut()
            .is_some_and(|child| matches!(child.try_wait(), Ok(Some(_))))
        {
            self.retired = None;
        }
    }

    /// The display whose X11 window manager is ready, or `None`. The
    /// frames client may connect only then; a change (server loss, a new
    /// server) kills the running client and starts over with a fresh
    /// budget.
    pub fn set_ready_x11_display(&mut self, display: Option<u32>) {
        // try_wait never waits for an external process.
        self.reap_retired();
        if self.display == display {
            return;
        }
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            self.retired = Some(child);
        }
        self.display = display;
        self.starts = 0;
        self.next_ms = 0;
        self.offer_until_ms = 0;
        self.gave_up = false;
        self.published = false;
    }

    /// Start the client, or restart it after an exit (bounded), never
    /// blocking.
    pub fn poll(&mut self, now_ms: u64) {
        self.poll_with(now_ms, &mut |display| {
            let bin = resolve(
                std::env::var_os(OVERRIDE_ENV).as_deref(),
                std::env::current_exe()
                    .ok()
                    .as_deref()
                    .and_then(Path::parent),
                std::env::var_os("PATH").as_deref(),
                &|path| path.is_file(),
            )?;
            Some(spawn(&bin, display))
        });
    }

    fn poll_with(&mut self, now_ms: u64, launch: Launch<'_>) {
        self.reap_retired();
        let Some(display) = self.display else {
            return;
        };
        if let Some(child) = &mut self.child {
            match child.try_wait() {
                Ok(None) => {
                    if self.starts > 1 && now_ms.saturating_sub(self.started_ms) >= HEALTHY_MS {
                        self.starts = 1;
                    }
                    return;
                }
                Ok(Some(status)) => {
                    eprintln!("tuna-compositor: x11 frames: client exited ({status})");
                    self.child = None;
                    self.next_ms = now_ms
                        .saturating_add(self.policy.next_delay(self.starts.saturating_sub(1)));
                }
                Err(_) => return,
            }
        }
        if self.retired.is_some() || self.gave_up || now_ms < self.next_ms {
            return;
        }
        if self.starts > self.policy.max_attempts {
            self.gave_up = true;
            eprintln!(
                "tuna-compositor: x11 frames: restart budget exhausted; X11 windows stay undecorated"
            );
            return;
        }
        match launch(display) {
            None => {
                // Nothing installed to start on this display, and no
                // restart gate to come back for.
                self.gave_up = true;
                eprintln!(
                    "tuna-compositor: x11 frames: no {BINARY} to run; X11 windows stay undecorated"
                );
            }
            Some(Ok(child)) => {
                self.starts += 1;
                self.child = Some(child);
                self.started_ms = now_ms;
                self.offer_until_ms = now_ms.saturating_add(OFFER_WINDOW_MS);
                self.next_offer_ms = now_ms.saturating_add(OFFER_INTERVAL_MS);
                eprintln!("tuna-compositor: x11 frames: client started on :{display}");
            }
            Some(Err(error)) => {
                self.starts += 1;
                self.next_ms = now_ms.saturating_add(self.policy.next_delay(self.starts - 1));
                eprintln!("tuna-compositor: x11 frames: client failed to start: {error}");
            }
        }
    }

    /// Whether a frames client runs: a window that wants a frame waits
    /// for it only then.
    pub fn running(&self) -> bool {
        self.child.is_some()
    }

    /// Whether the window manager should offer its unframed windows to
    /// the client now: every [`OFFER_INTERVAL_MS`] while the client
    /// starts, and for as long as a window waits for its frame.
    pub fn offer_due(&mut self, now_ms: u64, window_waits: bool) -> bool {
        let offering = window_waits || now_ms < self.offer_until_ms;
        if self.child.is_none() || !offering || now_ms < self.next_offer_ms {
            return false;
        }
        self.next_offer_ms = now_ms.saturating_add(OFFER_INTERVAL_MS);
        true
    }

    /// Milliseconds until [`poll`](Self::poll) or
    /// [`offer_due`](Self::offer_due) next has work: a restart gate or the
    /// next offer. Child exits wake the loop through SIGCHLD.
    pub fn next_wake_ms(&self, now_ms: u64, window_waits: bool) -> Option<u64> {
        self.display?;
        if self.child.is_some() {
            return (window_waits || now_ms < self.offer_until_ms)
                .then(|| self.next_offer_ms.saturating_sub(now_ms));
        }
        (self.retired.is_none() && !self.gave_up).then(|| self.next_ms.saturating_sub(now_ms))
    }

    /// Install freshly read title-bar settings.
    pub fn set_titlebar(&mut self, titlebar: TitlebarSettings) {
        if self.titlebar != titlebar {
            self.titlebar = titlebar;
            self.published = false;
        }
    }

    /// The XSETTINGS the ready display still lacks: the title-bar
    /// settings, once per X server and after each change.
    pub fn take_xsettings(&mut self) -> Option<[(&'static str, String); 4]> {
        (self.display.is_some() && !self.published).then(|| {
            self.published = true;
            self.titlebar.xsettings()
        })
    }
}

impl Drop for X11Frames {
    fn drop(&mut self) {
        if let Some(child) = &mut self.retired {
            let _ = child.kill();
            let _ = child.try_wait();
        }
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    const HARNESS_LIMIT: Duration = Duration::from_secs(10);

    fn sleeper(_display: u32) -> Option<std::io::Result<Child>> {
        Some(Command::new("sleep").arg("30").spawn())
    }

    fn crasher(_display: u32) -> Option<std::io::Result<Child>> {
        Some(Command::new("false").spawn())
    }

    /// Wait for the running child to exit by itself.
    fn exited(frames: &mut X11Frames) {
        let deadline = Instant::now() + HARNESS_LIMIT;
        let child = frames.child.as_mut().expect("a client was started");
        while child.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "client did not exit");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Wait for a killed child to be reaped.
    fn reaped(frames: &mut X11Frames) {
        let deadline = Instant::now() + HARNESS_LIMIT;
        while frames.retired.is_some() {
            assert!(Instant::now() < deadline, "killed client was not reaped");
            std::thread::sleep(Duration::from_millis(2));
            frames.reap_retired();
        }
    }

    #[test]
    fn starts_once_the_window_manager_is_ready_and_goes_with_the_server() {
        let mut frames = X11Frames::new();
        let mut displays = Vec::new();
        let mut launch = |display| {
            displays.push(display);
            sleeper(display)
        };
        // DISPLAY is advertised long before a server runs: nothing starts.
        frames.poll_with(0, &mut launch);
        assert!(!frames.running());
        frames.set_ready_x11_display(Some(7));
        frames.poll_with(0, &mut launch);
        assert!(frames.running());
        frames.poll_with(50, &mut launch);
        // Server loss: the client is killed, and none starts without one.
        frames.set_ready_x11_display(None);
        assert!(!frames.running());
        frames.poll_with(60, &mut launch);
        assert!(!frames.running());
        reaped(&mut frames);
        // The restarted server gets a fresh client.
        frames.set_ready_x11_display(Some(7));
        frames.poll_with(70, &mut launch);
        assert!(frames.running());
        drop(launch);
        assert_eq!(displays, [7, 7]);
    }

    #[test]
    fn a_killed_client_is_reaped_before_its_successor_starts() {
        let mut frames = X11Frames::new();
        let mut launches = 0;
        let mut launch = |display| {
            launches += 1;
            sleeper(display)
        };
        frames.set_ready_x11_display(Some(1));
        frames.poll_with(0, &mut launch);
        // The server went and came back within one tick.
        frames.set_ready_x11_display(None);
        frames.set_ready_x11_display(Some(1));
        // Whenever the kill lands, a successor never runs beside a
        // predecessor that is not reaped yet.
        frames.poll_with(1, &mut launch);
        assert!(
            !frames.running() || frames.retired.is_none(),
            "a client started while the old one was unreaped"
        );
        reaped(&mut frames);
        frames.poll_with(2, &mut launch);
        assert!(frames.running());
        drop(launch);
        assert_eq!(launches, 2);
    }

    #[test]
    fn a_crashing_client_restarts_with_backoff_a_bounded_number_of_times() {
        let mut frames = X11Frames::new();
        let mut launches = 0;
        let mut launch = |display| {
            launches += 1;
            crasher(display)
        };
        frames.set_ready_x11_display(Some(1));
        frames.poll_with(0, &mut launch);
        // 500 ms, 1 s and 2 s after each observed exit.
        for (seen, wait) in [(1_000, 500), (2_000, 1_000), (4_000, 2_000)] {
            exited(&mut frames);
            frames.poll_with(seen, &mut launch);
            assert!(!frames.running());
            assert_eq!(frames.next_wake_ms(seen, false), Some(wait));
            frames.poll_with(seen + wait - 1, &mut launch);
            assert!(!frames.running(), "restarted before the backoff ended");
            frames.poll_with(seen + wait, &mut launch);
            assert!(frames.running());
        }
        // The fourth client dies too: the budget is spent.
        exited(&mut frames);
        frames.poll_with(10_000, &mut launch);
        frames.poll_with(60_000, &mut launch);
        assert!(!frames.running());
        assert_eq!(frames.next_wake_ms(60_000, false), None);
        // A new server starts over.
        frames.set_ready_x11_display(None);
        frames.set_ready_x11_display(Some(1));
        frames.poll_with(60_001, &mut launch);
        assert!(frames.running());
        drop(launch);
        assert_eq!(launches, 5);
    }

    #[test]
    fn a_client_that_ran_a_minute_earns_its_restart_budget_back() {
        let mut frames = X11Frames::new();
        let mut first = true;
        let mut launch = |display| {
            if std::mem::take(&mut first) {
                crasher(display)
            } else {
                sleeper(display)
            }
        };
        frames.set_ready_x11_display(Some(1));
        frames.poll_with(0, &mut launch);
        exited(&mut frames);
        frames.poll_with(100, &mut launch);
        frames.poll_with(600, &mut launch);
        assert!(frames.running());
        assert_eq!(frames.starts, 2);
        frames.poll_with(600 + HEALTHY_MS - 1, &mut launch);
        assert_eq!(frames.starts, 2);
        frames.poll_with(600 + HEALTHY_MS, &mut launch);
        assert_eq!(frames.starts, 1);
    }

    #[test]
    fn a_missing_binary_is_reported_once_and_never_retried() {
        let mut frames = X11Frames::new();
        let mut asked = 0;
        let mut launch = |_display| {
            asked += 1;
            None
        };
        frames.set_ready_x11_display(Some(1));
        frames.poll_with(0, &mut launch);
        frames.poll_with(10_000, &mut launch);
        assert!(!frames.running());
        assert_eq!(frames.next_wake_ms(10_000, true), None);
        drop(launch);
        assert_eq!(asked, 1);
    }

    #[test]
    fn unframed_windows_are_offered_while_the_client_starts_and_while_one_waits() {
        let mut frames = X11Frames::new();
        assert!(!frames.offer_due(0, true), "no client, nobody to offer to");
        frames.set_ready_x11_display(Some(1));
        frames.poll_with(0, &mut sleeper);
        assert!(!frames.offer_due(50, false));
        assert_eq!(frames.next_wake_ms(50, false), Some(50));
        assert!(frames.offer_due(100, false));
        assert!(!frames.offer_due(150, false));
        assert!(frames.offer_due(200, false));
        // Past the start-up window the client listens for new windows by
        // itself; only a window still waiting is offered again.
        assert!(!frames.offer_due(OFFER_WINDOW_MS, false));
        assert_eq!(frames.next_wake_ms(OFFER_WINDOW_MS, false), None);
        assert!(frames.offer_due(OFFER_WINDOW_MS, true));
        assert_eq!(frames.next_wake_ms(OFFER_WINDOW_MS + 40, true), Some(60));
    }

    #[test]
    fn titlebar_settings_publish_once_per_server_and_on_change() {
        let mut frames = X11Frames::new();
        assert_eq!(frames.take_xsettings(), None, "no XSETTINGS owner yet");
        frames.set_ready_x11_display(Some(1));
        let defaults = frames.take_xsettings().expect("a ready display");
        assert_eq!(defaults, TitlebarSettings::default().xsettings());
        assert_eq!(frames.take_xsettings(), None);
        frames.set_titlebar(TitlebarSettings::default());
        assert_eq!(frames.take_xsettings(), None, "unchanged settings");
        let custom = TitlebarSettings {
            button_layout: "close:appmenu".into(),
            ..TitlebarSettings::default()
        };
        frames.set_titlebar(custom.clone());
        assert_eq!(frames.take_xsettings(), Some(custom.xsettings()));
        // A restarted server has a new XSETTINGS owner with nothing set.
        frames.set_ready_x11_display(None);
        assert_eq!(frames.take_xsettings(), None);
        frames.set_ready_x11_display(Some(1));
        assert_eq!(frames.take_xsettings(), Some(custom.xsettings()));
    }

    #[test]
    fn the_binary_is_the_override_else_beside_the_compositor_else_on_path() {
        let os = OsStr::new;
        let path = os("/opt/bin:/usr/bin");
        let exe_dir = Path::new("/usr/libexec/tuna");
        let installed = |files: &'static [&'static str]| {
            move |candidate: &Path| files.iter().any(|file| Path::new(file) == candidate)
        };
        let both = installed(&[
            "/usr/libexec/tuna/tuna-x11-frames",
            "/usr/bin/tuna-x11-frames",
            "/usr/libexec/mutter-x11-frames",
            "/opt/bin/mutter-x11-frames",
        ]);
        assert_eq!(
            resolve(None, Some(exe_dir), Some(path), &both),
            Some("/usr/libexec/tuna/tuna-x11-frames".into())
        );
        let on_path_only = installed(&["/usr/bin/tuna-x11-frames"]);
        assert_eq!(
            resolve(None, Some(exe_dir), Some(path), &on_path_only),
            Some("/usr/bin/tuna-x11-frames".into())
        );
        assert_eq!(
            resolve(None, Some(exe_dir), Some(path), &installed(&[])),
            None
        );
        // The stock Mutter client as the oracle, by path or by name.
        assert_eq!(
            resolve(
                Some(os("/usr/libexec/mutter-x11-frames")),
                Some(exe_dir),
                Some(path),
                &both
            ),
            Some("/usr/libexec/mutter-x11-frames".into())
        );
        assert_eq!(
            resolve(
                Some(os("mutter-x11-frames")),
                Some(exe_dir),
                Some(path),
                &both
            ),
            Some("/opt/bin/mutter-x11-frames".into())
        );
        // A named client that is not there is not replaced by the default.
        assert_eq!(
            resolve(
                Some(os("/nowhere/frames")),
                Some(exe_dir),
                Some(path),
                &both
            ),
            None
        );
        // Turned off.
        assert_eq!(
            resolve(Some(os("0")), Some(exe_dir), Some(path), &both),
            None
        );
        assert_eq!(
            resolve(Some(os("")), Some(exe_dir), Some(path), &both),
            None
        );
    }
}
