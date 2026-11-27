//! The IBus bridge (`tuna-ibus-bridge`): GNOME Shell is IBus's client
//! for text input, and Tuna Desktop's bridge plays that part. It joins the
//! session as the input method (input-method-v2), sends the keys typed
//! into a focused text field to IBus, and hands IBus's preedit and
//! commits to the app; keys IBus does not take go back through a
//! virtual keyboard only this client may hold. The compositor starts it
//! on a private socket when IBus is installed and restarts it a few
//! times if it exits.

use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use rustix::process::Signal;
use smithay::reexports::wayland_server::DisplayHandle;

/// Restarts after the first start, as for the shell.
const MAX_RESTARTS: u32 = 3;
/// Wait before a restart.
const RESTART_DELAY_MS: u64 = 2_000;
/// Session end gives the bridge's groups this long to leave on SIGTERM
/// before they are killed, as for Orca (`orca::SHUTDOWN_GRACE`): over an
/// `ibus-daemon`'s own stop of its components.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);
/// After SIGKILL only the kernel's exit work remains before the reap.
const REAP_BOUND: Duration = Duration::from_secs(1);
/// The session unit that owns `ibus-daemon` on the hardware session, where
/// the compositor's session manager starts GNOME's session units
/// (`IBus Daemon for GNOME`, `WantedBy=gnome-session.target`). The bridge
/// leaves the daemon to it (see `tuna-ibus-bridge`).
const SESSION_IBUS_UNIT: &str = "org.freedesktop.IBus.session.GNOME.service";

/// The supervised bridge process.
pub struct ImeBridge {
    wake: crate::wake::Wake,
    bin: PathBuf,
    wayland_display: String,
    child: Option<Child>,
    starts: u32,
    next_ms: u64,
    x11_display: Option<u32>,
    xim: Option<Child>,
    retired_xim: Option<Child>,
    xim_address_probe: Option<mpsc::Receiver<Option<String>>>,
    xim_starts: u32,
    xim_next_ms: u64,
}

impl ImeBridge {
    pub(crate) fn set_wake(&self, wake: &crate::wake::Wake) {
        self.wake.connect(wake);
    }

    /// The bridge to run for this session, if any: `TUNA_IBUS=0` turns
    /// it off; otherwise it runs when IBus is installed (`ibus-daemon`
    /// on `PATH`) and the bridge binary is found (`TUNA_IBUS_BRIDGE_BIN`,
    /// then next to this executable, then `PATH`).
    pub fn configured(wayland_display: &str) -> Option<Self> {
        if std::env::var_os("TUNA_IBUS").is_some_and(|v| v == "0") {
            return None;
        }
        let path = std::env::var_os("PATH")?;
        let on_path = |name: &str| {
            std::env::split_paths(&path)
                .map(|dir| dir.join(name))
                .find(|p| p.is_file())
        };
        on_path("ibus-daemon")?;
        let bin = std::env::var_os("TUNA_IBUS_BRIDGE_BIN")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::current_exe()
                    .ok()
                    .and_then(|exe| exe.parent().map(|d| d.join("tuna-ibus-bridge")))
                    .filter(|p| p.is_file())
            })
            .or_else(|| on_path("tuna-ibus-bridge"))?;
        Some(Self {
            bin,
            wayland_display: wayland_display.to_owned(),
            child: None,
            starts: 0,
            next_ms: 0,
            x11_display: None,
            xim: None,
            retired_xim: None,
            xim_address_probe: None,
            xim_starts: 0,
            xim_next_ms: 0,
            wake: Default::default(),
        })
    }

    /// Start, or restart after an exit (bounded), never blocking.
    pub fn poll(&mut self, now_ms: u64, dh: &mut DisplayHandle) {
        self.poll_xim(now_ms);
        if let Some(child) = &mut self.child {
            match child.try_wait() {
                Ok(None) => return,
                Ok(Some(status)) => {
                    eprintln!("tuna-compositor: ime: bridge exited ({status})");
                    // Whatever it left in its group must not outlive it. A
                    // daemon it owned leads a group of its own and goes on
                    // the bridge's death signal (`tuna-ibus-bridge`).
                    sweep_group(child.id() as libc::pid_t);
                    self.child = None;
                    self.next_ms = now_ms + RESTART_DELAY_MS;
                }
                Err(_) => return,
            }
        }
        if self.starts > MAX_RESTARTS || now_ms < self.next_ms {
            return;
        }
        self.starts += 1;
        match spawn(&self.bin, &self.wayland_display, dh) {
            Ok(child) => {
                eprintln!("tuna-compositor: ime: IBus bridge started");
                self.child = Some(child);
            }
            Err(e) => {
                eprintln!("tuna-compositor: ime: bridge failed to start: {e}");
                self.next_ms = now_ms + RESTART_DELAY_MS;
            }
        }
    }

    /// Milliseconds until [`poll`](Self::poll) next has work of its own:
    /// a bridge or XIM restart gate, or (16 ms) an XIM address probe in
    /// flight. Child exits wake the loop through SIGCHLD (#503).
    pub fn next_wake_ms(&self, now_ms: u64) -> Option<u64> {
        let bridge = (self.child.is_none() && self.starts <= MAX_RESTARTS)
            .then(|| self.next_ms.saturating_sub(now_ms));
        let xim = if self.xim_address_probe.is_some() {
            Some(16)
        } else {
            (self.x11_display.is_some()
                && self.xim.is_none()
                && self.retired_xim.is_none()
                && self.xim_starts <= MAX_RESTARTS)
                .then(|| self.xim_next_ms.saturating_sub(now_ms))
        };
        bridge.into_iter().chain(xim).min()
    }

    /// The XIM helper may connect only after the compositor's XWM is ready.
    /// Advertising reserved sockets alone must not activate XWayland.
    pub fn set_ready_x11_display(&mut self, display: Option<u32>) {
        // try_wait never waits for an external process. This runs even while
        // locked, when normal IBus polling is suspended.
        if self
            .retired_xim
            .as_mut()
            .is_some_and(|child| matches!(child.try_wait(), Ok(Some(_))))
        {
            self.retired_xim = None;
        }
        if self.x11_display == display {
            return;
        }
        if let Some(mut child) = self.xim.take() {
            let _ = child.kill();
            self.retired_xim = Some(child);
        }
        self.x11_display = display;
        self.xim_starts = 0;
        self.xim_next_ms = 0;
    }

    fn poll_xim(&mut self, now_ms: u64) {
        // Keep at most one live helper and one killed child awaiting reaping.
        if self.retired_xim.is_some() {
            return;
        }
        let Some(display) = self.x11_display else {
            return;
        };
        if let Some(child) = &mut self.xim {
            if matches!(child.try_wait(), Ok(None)) {
                return;
            }
            self.xim = None;
            self.xim_next_ms = now_ms + RESTART_DELAY_MS;
        }
        if self.xim_address_probe.is_none()
            && (now_ms < self.xim_next_ms || self.xim_starts > MAX_RESTARTS)
        {
            return;
        }
        let bin = std::env::var_os("PATH")
            .into_iter()
            .flat_map(|path| {
                std::env::split_paths(&path)
                    .map(|p| p.join("ibus-x11"))
                    .collect::<Vec<_>>()
            })
            .chain([
                PathBuf::from("/usr/libexec/ibus-x11"),
                PathBuf::from("/usr/lib/ibus/ibus-x11"),
                PathBuf::from("/usr/lib64/ibus/ibus-x11"),
            ])
            .find(|path| path.is_file());
        let Some(bin) = bin else {
            // No XIM helper installed: nothing to start this session, and
            // no restart gate to come back for.
            self.xim_starts = MAX_RESTARTS + 1;
            self.xim_address_probe = None;
            return;
        };
        // A single worker owns the private address lookup and its pipe. The
        // compositor tick only receives a bounded result without waiting.
        let Some(probe) = &self.xim_address_probe else {
            let mut command = Command::new("ibus");
            command
                .arg("address")
                .env("WAYLAND_DISPLAY", &self.wayland_display)
                .env_remove("DISPLAY")
                .env_remove("IBUS_ADDRESS")
                .env_remove("WAYLAND_SOCKET");
            let (send, receive) = mpsc::sync_channel(1);
            self.xim_starts += 1;
            self.xim_next_ms = now_ms + RESTART_DELAY_MS;
            let completion = self.wake.completion();
            if std::thread::Builder::new()
                .name("tuna-xim-address".into())
                .stack_size(256 * 1024)
                .spawn(move || {
                    let _completion = completion;
                    // Drop the result sender before the completion wake, including unwind.
                    let result_sender = send;
                    let _ = result_sender.send(bounded_address_probe(command));
                })
                .is_ok()
            {
                self.xim_address_probe = Some(receive);
            }
            return;
        };
        let address = match probe.try_recv() {
            Ok(address) => address,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => None,
        };
        self.xim_address_probe = None;
        let Some(address) = address else { return };
        match crate::nofile::restore_in_child(&mut Command::new(bin))
            .env("DISPLAY", format!(":{display}"))
            .env("WAYLAND_DISPLAY", &self.wayland_display)
            .env("IBUS_ADDRESS", address)
            .env_remove("WAYLAND_SOCKET")
            .spawn()
        {
            Ok(child) => {
                self.xim = Some(child);
                eprintln!("tuna-compositor: ime: XIM started on :{display}");
            }
            Err(error) => eprintln!("tuna-compositor: ime: XIM failed: {error}"),
        }
    }
}

/// Runs on the one address worker, never the compositor thread. A deadline and
/// fixed output cap bound the probe; retaining its receiver until completion
/// also bounds workers if an external child cannot finish being reaped.
fn bounded_address_probe(mut command: Command) -> Option<String> {
    command.stdout(Stdio::piped()).stderr(Stdio::null());
    #[cfg(target_os = "linux")]
    {
        let owner = rustix::process::getpid();
        // SAFETY: these are only async-signal-safe prctl/getppid syscalls and
        // raw OS error construction between fork and exec.
        unsafe {
            command.pre_exec(move || {
                rustix::process::set_parent_process_death_signal(Some(
                    rustix::process::Signal::KILL,
                ))?;
                // The owner may have exited before prctl installed the signal.
                if rustix::process::getppid() != Some(owner) {
                    return Err(std::io::Error::from_raw_os_error(
                        rustix::io::Errno::CHILD.raw_os_error(),
                    ));
                }
                Ok(())
            });
        }
    }
    let mut child = command.spawn().ok()?;
    let result = (|| {
        let mut output = child.stdout.take()?;
        let flags = rustix::fs::fcntl_getfl(&output).ok()?;
        rustix::fs::fcntl_setfl(&output, flags | rustix::fs::OFlags::NONBLOCK).ok()?;
        let deadline = Instant::now() + Duration::from_millis(500);
        let mut bytes = Vec::with_capacity(4096);
        let mut buffer = [0u8; 1024];
        loop {
            if Instant::now() >= deadline {
                return None;
            }
            match output.read(&mut buffer) {
                Ok(0) => {
                    if let Some(status) = child.try_wait().ok()? {
                        if !status.success() {
                            return None;
                        }
                        return String::from_utf8(bytes)
                            .ok()
                            .map(|s| s.trim().to_owned())
                            .filter(|s| !s.is_empty() && s != "(null)");
                    }
                }
                Ok(n) => {
                    if bytes.len() + n > 4096 {
                        return None;
                    }
                    bytes.extend_from_slice(&buffer[..n]);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => return None,
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    })();
    if !matches!(child.try_wait(), Ok(Some(_))) {
        let _ = child.kill();
        // This wait is confined to the worker. The tick cannot create another
        // worker until its result arrives, even if reaping itself stalls.
        let _ = child.wait();
    }
    result
}

impl Drop for ImeBridge {
    fn drop(&mut self) {
        if let Some(child) = &mut self.retired_xim {
            let _ = child.kill();
            let _ = child.try_wait();
        }
        if let Some(child) = &mut self.xim {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.shutdown();
    }
}

impl ImeBridge {
    /// Session end: retire what the bridge runs before the session's buses
    /// go away, its own process group and the groups its children lead. A
    /// daemon the bridge owns is such a child: `ibus-daemon` makes itself
    /// a group leader (`setpgid (0, 0)` in its `main`) for its components
    /// (`ibus-dconf`, engines). Left running it stays in the logind session
    /// scope and keeps the session `closing` (#1014). A session whose
    /// manager owns the daemon has none of it here. Idempotent; `Drop`
    /// repeats it.
    pub fn shutdown(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        // No further restarts once the session is ending.
        self.starts = MAX_RESTARTS + 1;
        let pid = child.id();
        let started = Instant::now();
        let (groups, clean) = retire_group(&mut child, SHUTDOWN_GRACE);
        eprintln!(
            "tuna-compositor: ime: bridge group retired pid={pid} groups={groups} group_exited_in_grace={clean} elapsed_ms={}",
            started.elapsed().as_millis()
        );
    }
}

/// The process groups the bridge's live children lead, from `/proc`: a
/// process's parent and group are the two fields after its state.
fn child_groups(bridge: libc::pid_t) -> Vec<libc::pid_t> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let pid: libc::pid_t = entry.file_name().to_str()?.parse().ok()?;
            let stat = std::fs::read_to_string(entry.path().join("stat")).ok()?;
            let mut fields = stat.rsplit_once(')')?.1.split_whitespace().skip(1);
            let ppid: libc::pid_t = fields.next()?.parse().ok()?;
            let pgid: libc::pid_t = fields.next()?.parse().ok()?;
            (ppid == bridge && pgid == pid).then_some(pid)
        })
        .collect()
}

/// SIGTERM the bridge's process group and the groups its children lead,
/// wait up to `grace` for all of them to empty, then SIGKILL whatever is
/// left. The bridge is reaped here, so its group ID stays ours until the
/// group is empty; a child's group keeps its ID while any member lives,
/// which each signal probes first. Returns how many groups there were,
/// and true when nothing had to be killed.
fn retire_group(child: &mut Child, grace: Duration) -> (usize, bool) {
    let bridge = child.id() as libc::pid_t;
    // Listed before the signal: once the bridge is gone its children are
    // init's.
    let mut groups = vec![bridge];
    groups.extend(child_groups(bridge));
    // A group is empty once nothing answers signal 0 (a zombie leader
    // still answers, hence the reaping below).
    let occupied = |group: libc::pid_t| kill_group(group, None).is_ok();
    let signal = |signal: Signal| {
        for &group in &groups {
            // Our own child's group, or one a child of it leads; while
            // members remain, the ID cannot belong to anyone else.
            if occupied(group) {
                let _ = kill_group(group, Some(signal));
            }
        }
    };
    let empty = || groups.iter().all(|&group| !occupied(group));
    signal(Signal::TERM);
    let deadline = Instant::now() + grace;
    let mut reaped = false;
    loop {
        reaped = reaped || !matches!(child.try_wait(), Ok(None));
        if reaped && empty() {
            return (groups.len(), true);
        }
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    signal(Signal::KILL);
    let until = Instant::now() + REAP_BOUND;
    while !reaped && Instant::now() < until {
        reaped = !matches!(child.try_wait(), Ok(None));
        std::thread::sleep(Duration::from_millis(5));
    }
    (groups.len(), false)
}

/// SIGKILL what a reaped bridge left in its group. Only while members
/// remain: they keep the group ID from belonging to anyone else.
fn sweep_group(group: libc::pid_t) {
    if kill_group(group, None).is_ok() {
        let _ = kill_group(group, Some(Signal::KILL));
    }
}

/// kill(2) as `kill(-group, signal)`, where `None` is signal 0 (a probe):
/// process group `group`, the caller's own group for 0, and the single
/// process `-group` for a negative ID. `i32::MIN` has no negation, and
/// the kernel answers it with ESRCH.
fn kill_group(group: libc::pid_t, signal: Option<Signal>) -> rustix::io::Result<()> {
    use rustix::process::{self, Pid};
    if group == 0 {
        return match signal {
            Some(signal) => process::kill_current_process_group(signal),
            None => process::test_kill_current_process_group(),
        };
    }
    let (pid, whole_group) = if group > 0 {
        (Pid::from_raw(group), true)
    } else {
        (group.checked_neg().and_then(Pid::from_raw), false)
    };
    let pid = pid.ok_or(rustix::io::Errno::SRCH)?;
    match (whole_group, signal) {
        (true, Some(signal)) => process::kill_process_group(pid, signal),
        (true, None) => process::test_kill_process_group(pid),
        (false, Some(signal)) => process::kill_process(pid, signal),
        (false, None) => process::test_kill_process(pid),
    }
}

/// The bridge's command: its own process group (so session end and a bridge
/// exit retire what it started, never the compositor's group), with
/// its end of a fresh socket pair as `WAYLAND_SOCKET`. `unit` is the
/// session unit that owns `ibus-daemon` when the session manager runs
/// GNOME's session units; the bridge then starts no daemon of its own.
fn bridge_command(bin: &Path, wayland_display: &str, fd: i32, unit: Option<&str>) -> Command {
    let mut command = Command::new(bin);
    crate::nofile::restore_in_child(&mut command);
    command
        .process_group(0)
        .env("WAYLAND_SOCKET", fd.to_string())
        // IBus names its bus after the display.
        .env("WAYLAND_DISPLAY", wayland_display)
        .env_remove("DISPLAY")
        .env_remove("IBUS_ADDRESS");
    if let Some(unit) = unit {
        command.env("TUNA_IBUS_UNIT", unit);
    }
    command
}

/// Spawn the bridge; the compositor's end of the socket pair becomes the
/// trusted client.
fn spawn(bin: &Path, wayland_display: &str, dh: &mut DisplayHandle) -> std::io::Result<Child> {
    let (ours, theirs) = UnixStream::pair()?;
    let fd = theirs.as_raw_fd();
    let unit = crate::session_services::hardware_session().then_some(SESSION_IBUS_UNIT);
    let mut command = bridge_command(bin, wayland_display, fd, unit);
    // SAFETY: one async-signal-safe fcntl between fork and exec, so the
    // socket survives exec (clears FD_CLOEXEC).
    unsafe {
        command.pre_exec(move || {
            let fd = std::os::fd::BorrowedFd::borrow_raw(fd);
            rustix::io::fcntl_setfd(fd, rustix::io::FdFlags::empty()).map_err(std::io::Error::from)
        });
    }
    let child = command.spawn()?;
    drop(theirs);
    dh.insert_client(ours, Arc::new(crate::ClientState::ime_bridge()))
        .map_err(std::io::Error::other)?;
    Ok(child)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_probe_reads_a_bounded_result_and_rejects_oversized_output() {
        let mut command = Command::new("sh");
        command.args(["-c", "printf ' unix:path=/tmp/ibus\\n'"]);
        assert_eq!(
            bounded_address_probe(command).as_deref(),
            Some("unix:path=/tmp/ibus")
        );
        let mut command = Command::new("sh");
        command.args(["-c", "printf '%05000d' 0"]);
        assert!(bounded_address_probe(command).is_none());
    }

    #[test]
    fn address_probe_kills_a_process_that_does_not_finish() {
        let mut command = Command::new("sh");
        command.args(["-c", "exec sleep 30"]);
        let started = Instant::now();
        assert!(bounded_address_probe(command).is_none());
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn address_probe_owner_fixture() {
        let Some(path) = std::env::var_os("TUNA_XIM_PROBE_OWNER_FIXTURE") else {
            return;
        };
        let mut command = Command::new("sh");
        command
            .args([
                "-c",
                r#"printf '%s' "$$" > "$TUNA_XIM_PROBE_PID_FILE"; exec sleep 30"#,
            ])
            .env("TUNA_XIM_PROBE_PID_FILE", path);
        let _ = bounded_address_probe(command);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn owner_exit_terminates_an_in_flight_address_probe() {
        let path = std::env::temp_dir().join(format!(
            "tuna-xim-owner-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut owner = Command::new(std::env::current_exe().unwrap())
            .args(["address_probe_owner_fixture", "--nocapture"])
            .env("TUNA_XIM_PROBE_OWNER_FIXTURE", &path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let pid = loop {
            if let Ok(pid) = std::fs::read_to_string(&path) {
                if let Ok(pid) = pid.parse::<u32>() {
                    break Some(pid);
                }
            }
            if Instant::now() >= deadline {
                break None;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        let owner_was_running = owner.try_wait().unwrap().is_none();
        let _ = owner.kill();
        let _ = owner.wait();
        let _ = std::fs::remove_file(path);
        let pid = pid.expect("address probe never started in its isolated owner");
        assert!(
            owner_was_running,
            "owner exited before the abrupt-exit fixture"
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"));
            // A dead orphan may await init's reaping; it must never keep running.
            let running = stat.is_ok_and(|s| {
                !s.rsplit_once(") ")
                    .is_some_and(|(_, fields)| fields.starts_with('Z'))
            });
            if !running {
                break;
            }
            if Instant::now() >= deadline {
                if let Some(pid) = rustix::process::Pid::from_raw(pid as i32) {
                    let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
                }
                panic!("probe outlived its owner");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn losing_xwm_reaps_xim_and_resets_its_restart_budget() {
        let child = Command::new("sleep").arg("30").spawn().unwrap();
        let pid = child.id();
        let mut bridge = ImeBridge {
            bin: PathBuf::new(),
            wayland_display: "test".into(),
            child: None,
            starts: 1,
            next_ms: 123,
            x11_display: Some(5),
            xim: Some(child),
            retired_xim: None,
            xim_address_probe: None,
            xim_starts: MAX_RESTARTS + 1,
            xim_next_ms: 456,
            wake: Default::default(),
        };
        bridge.set_ready_x11_display(Some(5));
        assert_eq!(bridge.xim.as_ref().unwrap().id(), pid);
        bridge.set_ready_x11_display(None);
        assert!(bridge.xim.is_none());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while bridge.retired_xim.is_some() {
            assert!(
                std::time::Instant::now() < deadline,
                "killed XIM not reaped"
            );
            bridge.set_ready_x11_display(None);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(!PathBuf::from(format!("/proc/{pid}")).exists());
        assert_eq!((bridge.starts, bridge.next_ms), (1, 123));
        bridge.set_ready_x11_display(Some(5));
        assert_eq!((bridge.xim_starts, bridge.xim_next_ms), (0, 0));
    }

    /// A bridge-shaped group: `script` runs as the leader of its own group
    /// and prints the pid of one member first.
    fn group_child(script: &str) -> (Child, u32) {
        let mut command = bridge_command(Path::new("/bin/sh"), "isolated-test", 99, None);
        command
            .args(["-c", script])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = command.spawn().unwrap();
        let mut line = String::new();
        let mut stdout = child.stdout.take().unwrap();
        let mut byte = [0u8; 1];
        while stdout.read(&mut byte).unwrap() == 1 && byte[0] != b'\n' {
            line.push(byte[0] as char);
        }
        (child, line.trim().parse().unwrap())
    }

    fn gone(pid: u32) -> bool {
        // Reparented members may linger as zombies until init reaps them.
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(stat) => stat
                .rsplit_once(')')
                .is_some_and(|(_, rest)| rest.trim_start().starts_with('Z')),
            Err(_) => true,
        }
    }

    fn wait_gone(pid: u32) -> bool {
        let until = Instant::now() + Duration::from_secs(2);
        while !gone(pid) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10));
        }
        gone(pid)
    }

    /// getpgid(2) with libc's answer shape: -1 when it fails.
    fn pgid(pid: libc::pid_t) -> libc::pid_t {
        rustix::process::getpgid(rustix::process::Pid::from_raw(pid))
            .map_or(-1, |group| group.as_raw_nonzero().get())
    }

    /// `libc::kill(-group, signal)` as retire_group and sweep_group called
    /// it before kill_group, errno included; `-i32::MIN` wrapped in release.
    fn libc_kill(group: libc::pid_t, signal: libc::c_int) -> Result<(), i32> {
        // SAFETY: kill(2) only on IDs this test owns or probes with 0.
        match unsafe { libc::kill(group.wrapping_neg(), signal) } {
            0 => Ok(()),
            _ => Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(0)),
        }
    }

    #[test]
    fn kill_group_answers_as_kill_on_the_negated_id() {
        let (mut child, _) = group_child("echo 0; exec sleep 30");
        let live = child.id() as libc::pid_t;
        let (mut dead, _) = group_child("echo 0");
        let _ = dead.wait();
        let dead = dead.id() as libc::pid_t;
        let own = rustix::process::getpid().as_raw_nonzero().get();
        let ids = [0, live, dead, -own, -dead, i32::MAX, i32::MIN];
        for group in ids {
            let ours = kill_group(group, None).map_err(|errno| errno.raw_os_error());
            assert_eq!(ours, libc_kill(group, 0), "probe of {group}");
        }
        // A real signal takes the same route: CONT changes nothing here.
        let ours = kill_group(live, Some(Signal::CONT)).map_err(|errno| errno.raw_os_error());
        assert_eq!(ours, libc_kill(live, libc::SIGCONT));
        assert_eq!(ours, Ok(()));
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(
            kill_group(live, Some(Signal::CONT)).map_err(|errno| errno.raw_os_error()),
            libc_kill(live, libc::SIGCONT)
        );
    }

    #[test]
    fn bridge_runs_in_its_own_group_and_learns_the_session_unit() {
        let envs = |command: &Command| {
            command
                .get_envs()
                .find(|(name, _)| *name == "TUNA_IBUS_UNIT")
                .map(|(_, value)| value.map(|v| v.to_owned()))
        };
        let hardware = bridge_command(Path::new("/bin/true"), "d", 3, Some(SESSION_IBUS_UNIT));
        assert_eq!(
            envs(&hardware),
            Some(Some(SESSION_IBUS_UNIT.into())),
            "hardware session names the unit that owns the daemon"
        );
        // Nested: nothing is set, so the bridge owns its own daemon.
        assert_eq!(
            envs(&bridge_command(Path::new("/bin/true"), "d", 3, None)),
            None
        );

        let (mut child, _) = group_child("echo 0; exec sleep 30");
        let pid = child.id() as libc::pid_t;
        assert_eq!(pgid(pid), pid);
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn session_end_retires_the_group_without_killing_a_cooperative_member() {
        let (mut child, member) = group_child("(exec sleep 30) & echo $!; exec sleep 30");
        let pid = child.id();
        let started = Instant::now();
        assert_eq!(retire_group(&mut child, SHUTDOWN_GRACE), (1, true));
        assert!(started.elapsed() < SHUTDOWN_GRACE);
        // The leader is reaped, not left a zombie.
        assert!(!PathBuf::from(format!("/proc/{pid}")).exists());
        assert!(wait_gone(member));
    }

    #[test]
    fn session_end_kills_a_member_that_ignores_term_and_reaps_the_leader() {
        // A member whose handler fails to quit: the group sweep ends it.
        // The member names itself only once it ignores TERM, so the signal
        // cannot land before the trap.
        let (mut child, member) =
            group_child("sh -c 'trap \"\" TERM; echo $$; exec sleep 30' & exec sleep 30");
        let pid = child.id();
        let grace = Duration::from_millis(300);
        let started = Instant::now();
        assert_eq!(retire_group(&mut child, grace), (1, false));
        assert!(started.elapsed() >= grace);
        assert!(started.elapsed() < grace + REAP_BOUND + Duration::from_millis(500));
        assert!(!PathBuf::from(format!("/proc/{pid}")).exists());
        assert!(wait_gone(member));
    }

    #[test]
    fn a_bridge_that_dies_first_leaves_nothing_in_its_group() {
        // The leader exits at once; the member survives it (something the
        // bridge started in its group, outliving a crashed bridge).
        let (mut child, member) = group_child("(trap '' TERM; exec sleep 30) & echo $!");
        let _ = child.wait();
        assert!(!gone(member));
        sweep_group(child.id() as libc::pid_t);
        assert!(wait_gone(member));
    }

    #[test]
    fn session_end_retires_the_group_a_bridge_child_leads() {
        // ibus-daemon's `setpgid (0, 0)`: a child of the bridge that leads
        // a group of its own, which the bridge's group signal misses. It
        // names itself once it leads that group.
        let (mut child, daemon) =
            group_child("setsid sh -c 'echo $$; exec sleep 30' & exec sleep 30");
        assert_eq!(pgid(daemon as libc::pid_t), daemon as libc::pid_t);
        let started = Instant::now();
        assert_eq!(retire_group(&mut child, SHUTDOWN_GRACE), (2, true));
        assert!(started.elapsed() < SHUTDOWN_GRACE);
        assert!(wait_gone(daemon));
    }

    #[test]
    fn session_end_kills_a_bridge_childs_group_that_ignores_term() {
        let (mut child, daemon) =
            group_child("setsid sh -c 'trap \"\" TERM; echo $$; exec sleep 30' & exec sleep 30");
        let grace = Duration::from_millis(300);
        let started = Instant::now();
        assert_eq!(retire_group(&mut child, grace), (2, false));
        assert!(started.elapsed() >= grace);
        assert!(wait_gone(daemon));
    }
}
