//! Genuine distribution Orca, owned by the compositor (every hardware
//! session; a nested one only with `--screen-reader`) rather than a
//! global user-manager service. No --replace, global PID selection or bus-stop
//! request can affect another display's reader.
use std::io::Read;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tuna_shell_control::ScreenReaderState as State;

/// Orca 50.3 and 51 expose different genuine remote-controller names.
const NAMES: [&str; 2] = ["org.gnome.Orca1.Service", "org.gnome.Orca.Service"];
/// GNOME starts Orca from session autostart and never kills it for being slow
/// to claim its bus name; on a cold VM speech-dispatcher alone can take several
/// seconds. Past this grace we only report the slow start, and keep watching.
const READINESS_GRACE: Duration = Duration::from_secs(10);
/// A stopped reader's names drop as soon as the bus daemon processes its
/// disconnect. This bound only guards against a wedged bus.
const DEPARTURE_BOUND: Duration = Duration::from_secs(5);
/// Session end retires the owned reader itself before the session and AT-SPI
/// buses go away (#836). In passing VM runs Orca exits within ~0.3–0.6 s of
/// SIGTERM (session scope stopped 0.34 s and 0.57 s after it); Orca 51.1's
/// handler can instead raise on an already disconnected AT-SPI bus and keep
/// running. Over three times the slowest normal exit, far under systemd's
/// 90 s stop timeout, then the whole owned group is killed.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);
/// After SIGKILL only the kernel's exit work remains before the reap.
const REAP_BOUND: Duration = Duration::from_secs(1);

/// Observer for one owned child. Dropping it cancels the watcher thread.
struct Observation {
    receiver: Receiver<(u32, State, &'static str)>,
    cancel: Arc<AtomicBool>,
}
impl Drop for Observation {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

pub struct Reader {
    wake: crate::wake::Wake,
    display: String,
    enabled: bool,
    child: Option<Child>,
    observed: Option<Observation>,
    /// Pending until the last stopped child's bus names have vanished; a new
    /// Orca started earlier would lose the name race to its own predecessor.
    departing: Option<Receiver<()>>,
    /// Killed child still awaiting its non-blocking reap in poll().
    reaping: bool,
    /// Last child poll() reaped, until another starts: a reader that already
    /// exited (on systemd's own SIGTERM, say) still gets its session-end
    /// receipt, and nothing is signalled.
    reaped: Option<u32>,
    state: State,
    sent: State,
    attempts: u8,
    next_start: Instant,
    keyboard: Option<crate::a11y_keyboard::Monitor>,
    keyboard_deadline: Instant,
}
impl Reader {
    pub fn new(display: &str, keyboard: Option<crate::a11y_keyboard::Monitor>) -> Self {
        Self {
            display: display.into(),
            wake: Default::default(),
            enabled: false,
            child: None,
            observed: None,
            departing: None,
            reaping: false,
            reaped: None,
            state: State::Disabled,
            sent: State::Disabled,
            attempts: 0,
            next_start: Instant::now(),
            keyboard,
            keyboard_deadline: Instant::now() + Duration::from_secs(10),
        }
    }
    pub(crate) fn set_wake(&self, wake: &crate::wake::Wake) {
        self.wake.connect(wake);
    }
    pub fn pid(&self) -> Option<u32> {
        self.child.as_ref().map(Child::id)
    }
    pub fn status(&self) -> State {
        self.state
    }
    pub fn state(&self) -> &'static str {
        match self.state {
            State::Disabled => "disabled",
            State::Starting => "starting",
            State::Active => "active",
            State::Unavailable => "unavailable",
            State::Conflict => "conflict",
        }
    }
    pub fn set_enabled(&mut self, enabled: bool) {
        if self.enabled == enabled {
            return;
        }
        self.enabled = enabled;
        self.attempts = 0;
        self.next_start = Instant::now();
        self.keyboard_deadline = Instant::now() + Duration::from_secs(10);
        if !enabled {
            self.stop();
            self.state = State::Disabled;
        }
    }

    /// When [`poll`](Self::poll) next starts a reader by itself: the
    /// restart gate of an enabled reader without a child (a departure
    /// still in progress is checked again at the gate). Child exits wake
    /// the loop through SIGCHLD and watcher reports through the [`Wake`](crate::wake::Wake).
    pub fn next_deadline(&self) -> Option<Instant> {
        (self.enabled && self.child.is_none() && self.attempts < 3).then_some(self.next_start)
    }
    fn stop(&mut self) {
        if let Some(keyboard) = &self.keyboard {
            keyboard.revoke_reader();
        }
        self.observed = None;
        if let Some(child) = self.child.as_ref() {
            // Keep this unreaped Child as process authority (never a recycled
            // PID or another display's reader) until poll() reaps it on its
            // SIGCHLD wake and starts the bus departure watch. Never
            // sleep/wait on the active compositor frame path.
            signal_group(child, libc::SIGKILL);
            self.reaping = true;
        }
    }
    /// Session end: retire the owned reader before its buses go away (#836).
    /// The runtime calls this first in its teardown, which every exit path
    /// reaches (SIGTERM/SIGINT from systemd or the supervisor, logind's
    /// session stop, a failed loop); Drop repeats it as a no-op. Only this
    /// Reader's own unreaped child's group is signalled, never another
    /// session's reader.
    pub fn shutdown(&mut self) {
        if let Some(receipt) = self.retire() {
            use std::io::Write;
            // The VM lane requires this receipt before the compositor exits.
            // Teardown never panics on a closed stderr.
            let _ = writeln!(std::io::stderr(), "{receipt}");
        }
    }
    fn retire(&mut self) -> Option<Receipt> {
        self.enabled = false;
        self.observed = None;
        if let Some(keyboard) = &self.keyboard {
            keyboard.revoke_reader();
        }
        let Some(mut child) = self.child.take() else {
            return self.reaped.take().map(|pid| Receipt {
                pid,
                retirement: Retirement {
                    terminated: false,
                    exited_in_grace: true,
                },
                reaped: true,
                elapsed_ms: 0,
            });
        };
        self.reaping = false;
        self.reaped = None;
        let pid = child.id();
        let started = Instant::now();
        let retirement = retire_group(
            || exited_unreaped(pid),
            |signal| signal_group(&child, signal),
            std::thread::sleep,
            Instant::now,
            SHUTDOWN_GRACE,
        );
        let until = Instant::now() + REAP_BOUND;
        let reaped = loop {
            match child.try_wait() {
                Ok(Some(_)) => break true,
                Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(10)),
                _ => break false,
            }
        };
        Some(Receipt {
            pid,
            retirement,
            reaped,
            elapsed_ms: started.elapsed().as_millis(),
        })
    }
    fn await_departure(&mut self, pid: u32) {
        let (sender, receiver) = mpsc::channel();
        self.departing = Some(receiver);
        let wake = self.wake.clone();
        std::thread::spawn(move || {
            let until = Instant::now() + DEPARTURE_BOUND;
            while departing_owner(pid, &bus_call) && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(50));
            }
            let _ = sender.send(());
            wake.signal();
        });
    }
    /// True while a stopped child may still hold an Orca name.
    fn departure_pending(&mut self) -> bool {
        match self.departing.as_ref().map(Receiver::try_recv) {
            Some(Err(TryRecvError::Empty)) => true,
            Some(_) => {
                self.departing = None;
                false
            }
            None => false,
        }
    }
    pub fn poll(&mut self, diagnostics_visible: bool) -> Option<State> {
        let mut observed_running = false;
        if !self.reaping
            && self.child.is_some()
            && self.keyboard.as_ref().is_some_and(|keyboard| {
                keyboard.availability() != crate::a11y_keyboard::Availability::Ready
            })
        {
            self.stop();
            self.state = State::Unavailable;
            self.attempts = 3;
        }
        if let Some(child) = self.child.as_mut() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    if diagnostics_visible {
                        eprintln!("TUNA_ORCA_DIAGNOSTIC stage=child-exit pid={} attempt={} code={} signal={}", child.id(), self.attempts, status.code().unwrap_or(-1), status.signal().unwrap_or(0));
                    }
                    let pid = child.id();
                    self.reaped = Some(pid);
                    if let Some(keyboard) = &self.keyboard {
                        keyboard.revoke_reader();
                    }
                    self.child = None;
                    self.observed = None;
                    self.await_departure(pid);
                    // A reap after our own stop() keeps the state stop() chose.
                    if !std::mem::take(&mut self.reaping) {
                        self.state = State::Unavailable;
                        // Startup exit 1 includes the genuine singleton conflict.
                        // Do not repeatedly race another session for its reader.
                        if status.code() == Some(1) {
                            self.attempts = 3;
                        }
                        self.next_start = Instant::now() + Duration::from_secs(2);
                    }
                }
                Err(error) => {
                    if diagnostics_visible {
                        eprintln!(
                            "TUNA_ORCA_DIAGNOSTIC stage=child-wait-error pid={} attempt={} kind={}",
                            child.id(),
                            self.attempts,
                            error_kind(error.kind())
                        );
                    }
                    self.stop();
                    self.state = State::Unavailable;
                    self.attempts = 3;
                }
                Ok(None) => observed_running = true,
            }
        }
        let observation = self.observed.as_ref().map(|o| o.receiver.try_recv());
        if matches!(observation, Some(Err(mpsc::TryRecvError::Disconnected))) {
            self.stop();
            self.state = State::Unavailable;
            self.attempts = 3;
        }
        if let Some(Ok((pid, state, reason))) = observation {
            if self.pid() == Some(pid) {
                if state == State::Active {
                    self.state = state;
                } else if state == State::Starting {
                    // Slow or re-registering, but alive: GNOME keeps it.
                    if diagnostics_visible {
                        eprintln!(
                            "TUNA_ORCA_DIAGNOSTIC stage={} pid={} attempt={} observed_running={}",
                            reason, pid, self.attempts, observed_running
                        );
                    }
                    self.state = state;
                } else {
                    if diagnostics_visible {
                        eprintln!(
                            "TUNA_ORCA_DIAGNOSTIC stage={} pid={} attempt={} observed_running={}",
                            reason, pid, self.attempts, observed_running
                        );
                    }
                    self.stop();
                    self.state = state;
                    self.attempts = 3;
                }
            }
        }
        if self.enabled
            && self.child.is_none()
            && self.attempts < 3
            && Instant::now() >= self.next_start
            && !self.departure_pending()
        {
            if let Some(keyboard) = &self.keyboard {
                match keyboard.availability() {
                    crate::a11y_keyboard::Availability::Ready => {}
                    crate::a11y_keyboard::Availability::Pending
                        if Instant::now() < self.keyboard_deadline =>
                    {
                        return None
                    }
                    state => {
                        self.state = if state == crate::a11y_keyboard::Availability::Conflict {
                            State::Conflict
                        } else {
                            State::Unavailable
                        };
                        self.attempts = 3;
                        if self.sent != self.state {
                            self.sent = self.state;
                            return Some(self.state);
                        }
                        return None;
                    }
                }
            }
            self.attempts += 1;
            let mut command = Command::new("/usr/bin/orca");
            crate::nofile::restore_in_child(&mut command);
            command
                .env("WAYLAND_DISPLAY", &self.display)
                .env("XDG_SESSION_TYPE", "wayland")
                // Orca's setproctitle must preserve /proc environ so the native
                // lifecycle probe can verify this owned child's actual display.
                .env("SPT_NOENV", "1")
                .env_remove("DISPLAY")
                .env_remove("NOTIFY_SOCKET")
                .env_remove("WATCHDOG_USEC")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit());
            parent_lifetime(&mut command);
            let spawned = match &self.keyboard {
                Some(keyboard) => keyboard.spawn(&mut command),
                None => command.spawn(),
            };
            match spawned {
                Ok(child) => {
                    let pid = child.id();
                    let (sender, receiver) = mpsc::channel();
                    let cancel = Arc::new(AtomicBool::new(false));
                    self.child = Some(child);
                    self.reaped = None;
                    self.state = State::Starting;
                    self.observed = Some(Observation {
                        receiver,
                        cancel: cancel.clone(),
                    });
                    let wake = self.wake.clone();
                    std::thread::spawn(move || {
                        let _completion = wake.completion();
                        let report = |state, reason| {
                            let sent = sender.send((pid, state, reason)).is_ok();
                            wake.signal();
                            sent
                        };
                        watch_orca_name(pid, &bus_call, &cancel, READINESS_GRACE, report)
                    });
                }
                Err(error) => {
                    if diagnostics_visible {
                        eprintln!(
                            "TUNA_ORCA_DIAGNOSTIC stage=child-spawn pid=0 attempt={} kind={}",
                            self.attempts,
                            error_kind(error.kind())
                        );
                    }
                    self.state = State::Unavailable;
                    self.next_start = Instant::now() + Duration::from_secs(2);
                }
            }
        }
        if self.sent != self.state {
            self.sent = self.state;
            Some(self.state)
        } else {
            None
        }
    }
}
impl Drop for Reader {
    fn drop(&mut self) {
        // Only final teardown waits (boundedly) for an owned child. Normal
        // preference changes and conflicts retain/reap it through poll().
        self.shutdown();
        if let Some(keyboard) = &self.keyboard {
            keyboard.shutdown();
        }
    }
}

/// Signal the owned reader's process group. Its leader `child` is still
/// unreaped, so the PID, and with it the group ID, cannot name anyone else.
fn signal_group(child: &Child, signal: libc::c_int) {
    let pid = child.id() as libc::pid_t;
    // SAFETY: getpgid(2)/kill(2) only, on our own unreaped child.
    unsafe {
        // Readers spawned through parent_lifetime() lead their own group; a
        // child that merely joined another group is signalled alone.
        let target = if libc::getpgid(pid) == pid { -pid } else { pid };
        libc::kill(target, signal);
    }
}

/// Whether our child `pid` has exited, without reaping it (WNOWAIT): until the
/// group is killed its ID must stay ours.
fn exited_unreaped(pid: u32) -> bool {
    // SAFETY: waitid(2) writes only into the zeroed siginfo we own.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            pid as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result != 0 {
        // ECHILD: no longer waitable. EINTR: look again on the next check.
        return std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR);
    }
    // SAFETY: waitid filled (or left zeroed) the siginfo for SIGCHLD.
    unsafe { info.si_pid() == pid as libc::pid_t }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Retirement {
    /// SIGTERM went to a still running group.
    terminated: bool,
    /// The leader had exited, or exited within the grace.
    exited_in_grace: bool,
}

/// The session-end receipt the VM lane reads (`orca_logout_retirement`).
#[derive(Debug, PartialEq, Eq)]
struct Receipt {
    pid: u32,
    retirement: Retirement,
    reaped: bool,
    elapsed_ms: u128,
}
impl std::fmt::Display for Receipt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "tuna-compositor: screen reader retired pid={} terminated={} exited_in_grace={} reaped={} elapsed_ms={}",
            self.pid, self.retirement.terminated, self.retirement.exited_in_grace, self.reaped, self.elapsed_ms
        )
    }
}

/// SIGTERM the owned group and wait up to `grace` for its leader to exit
/// (unreaped), then SIGKILL the group in every case: a leader whose own
/// shutdown handler failed (#836) and any member that outlived it.
fn retire_group(
    mut exited: impl FnMut() -> bool,
    mut signal: impl FnMut(libc::c_int),
    mut sleep: impl FnMut(Duration),
    mut now: impl FnMut() -> Instant,
    grace: Duration,
) -> Retirement {
    let mut retirement = Retirement {
        terminated: false,
        exited_in_grace: exited(),
    };
    if !retirement.exited_in_grace {
        signal(libc::SIGTERM);
        retirement.terminated = true;
        let deadline = now() + grace;
        loop {
            if exited() {
                retirement.exited_in_grace = true;
                break;
            }
            let current = now();
            if current >= deadline {
                break;
            }
            sleep((deadline - current).min(Duration::from_millis(10)));
        }
    }
    signal(libc::SIGKILL);
    retirement
}

fn parent_lifetime(command: &mut Command) {
    // Its own process group, led by the unreaped child, is what stop() and
    // session-end shutdown signal (#836), never the compositor's group.
    command.process_group(0);
    let parent = unsafe { libc::getpid() };
    // Only async-signal-safe libc calls run after fork. Linux clears this setting
    // for a forked child, so speech-dispatcher is not accidentally bound to us.
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::getppid() != parent {
                libc::_exit(127);
            }
            Ok(())
        });
    }
}

/// Bounded observation only. A competing owner can make us stop OUR Child,
/// never another process; process ownership does not come from these snapshots.
fn error_kind(kind: std::io::ErrorKind) -> &'static str {
    match kind {
        std::io::ErrorKind::NotFound => "not-found",
        std::io::ErrorKind::PermissionDenied => "permission-denied",
        std::io::ErrorKind::Interrupted => "interrupted",
        std::io::ErrorKind::InvalidData => "invalid-data",
        _ => "other",
    }
}
fn bus_call(method: &str, arg: &str) -> Result<String, &'static str> {
    let mut command = Command::new("gdbus");
    command
        .args([
            "call",
            "--session",
            "--dest",
            "org.freedesktop.DBus",
            "--object-path",
            "/org/freedesktop/DBus",
            "--method",
            method,
            arg,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    bus_call_command(command)
}
fn bus_call_command(mut command: Command) -> Result<String, &'static str> {
    parent_lifetime(&mut command);
    let mut child = command.spawn().map_err(|_| "bus-spawn")?;
    let until = Instant::now() + Duration::from_secs(2);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => {
                let mut data = Vec::new();
                child
                    .stdout
                    .take()
                    .ok_or("bus-read")?
                    .take(1025)
                    .read_to_end(&mut data)
                    .map_err(|_| "bus-read")?;
                if data.len() > 1024 {
                    return Err("bus-oversize");
                }
                return String::from_utf8(data).map_err(|_| "bus-malformed");
            }
            Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(20)),
            result => {
                let reason = match result {
                    Ok(Some(_)) => "bus-nonzero",
                    Ok(None) => "bus-timeout",
                    Err(_) => "bus-wait-error",
                };
                let _ = child.kill();
                let _ = child.wait();
                return Err(reason);
            }
        }
    }
}
/// One snapshot of the Orca names against the owned child's credentials.
#[derive(Debug, PartialEq)]
enum Probe {
    Active,
    /// No name yet, or an owner vanished between bus calls.
    Absent(&'static str),
    Failed(State, &'static str),
}
fn probe_orca_name<B>(pid: u32, euid: u32, bus: &B) -> Probe
where
    B: Fn(&str, &str) -> Result<String, &'static str>,
{
    let mut last_reason = "name-absent";
    let mut active = false;
    // A foreign owner of either contract is a conflict, never replacement.
    'names: for name in NAMES {
        let reply = match bus("org.freedesktop.DBus.GetNameOwner", name) {
            Ok(reply) => reply,
            Err(reason) => {
                if last_reason != "owner-vanished" {
                    last_reason = reason;
                }
                continue;
            }
        };
        let Some(owner) = parse_owner(&reply) else {
            return Probe::Failed(State::Unavailable, "owner-malformed");
        };
        for (method, expected) in [
            ("org.freedesktop.DBus.GetConnectionUnixProcessID", pid),
            ("org.freedesktop.DBus.GetConnectionUnixUser", euid),
        ] {
            let actual = match bus(method, owner) {
                Ok(reply) => parse_uint(&reply),
                // The owner disconnected after GetNameOwner answered.
                Err("bus-nonzero") => {
                    last_reason = "owner-vanished";
                    continue 'names;
                }
                Err(reason) => return Probe::Failed(State::Unavailable, reason),
            };
            match actual {
                Some(actual) if actual == expected => {}
                Some(_) => return Probe::Failed(State::Conflict, "credentials-conflict"),
                None => return Probe::Failed(State::Unavailable, "credentials-malformed"),
            }
        }
        active = true;
    }
    if active {
        Probe::Active
    } else {
        Probe::Absent(last_reason)
    }
}
fn parse_owner(reply: &str) -> Option<&str> {
    let owner = reply.split('\'').nth(1)?;
    (owner.starts_with(':')
        && owner
            .bytes()
            .all(|byte| byte.is_ascii_digit() || b":.".contains(&byte)))
    .then_some(owner)
}
fn parse_uint(reply: &str) -> Option<u32> {
    reply
        .trim()
        .strip_prefix("(uint32 ")
        .and_then(|s| s.strip_suffix(",)"))
        .and_then(|s| s.parse::<u32>().ok())
}
/// Follows the owned child's bus registration until cancelled. Reports
/// Active when the name owner appears, Starting once if registration outlasts
/// the grace or the name is later lost, and a terminal state on conflict.
fn watch_orca_name<B, R>(pid: u32, bus: &B, cancel: &AtomicBool, grace: Duration, mut report: R)
where
    B: Fn(&str, &str) -> Result<String, &'static str>,
    R: FnMut(State, &'static str) -> bool,
{
    let euid = rustix::process::geteuid().as_raw();
    let started = Instant::now();
    let mut reported: Option<State> = None;
    while !cancel.load(Ordering::Relaxed) {
        let (state, reason, interval) = match probe_orca_name(pid, euid, bus) {
            Probe::Active => (State::Active, "active", Duration::from_secs(1)),
            Probe::Failed(state, reason) => {
                report(state, reason);
                return;
            }
            Probe::Absent(reason) => {
                let lost = reported == Some(State::Active);
                if !lost && started.elapsed() < grace {
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                }
                let reason = if lost {
                    "name-lost"
                } else {
                    slow_reason(reason)
                };
                (State::Starting, reason, Duration::from_millis(100))
            }
        };
        if reported != Some(state) {
            if !report(state, reason) {
                return;
            }
            reported = Some(state);
        }
        std::thread::sleep(interval);
    }
}
fn slow_reason(reason: &str) -> &'static str {
    match reason {
        "bus-spawn" => "slow-start-bus-spawn",
        "bus-timeout" => "slow-start-bus-timeout",
        "bus-oversize" => "slow-start-bus-oversize",
        "bus-malformed" => "slow-start-bus-malformed",
        "bus-read" => "slow-start-bus-read",
        "bus-wait-error" => "slow-start-bus-wait-error",
        "owner-vanished" => "slow-start-owner-vanished",
        _ => "slow-start",
    }
}
/// True while either name is still held by the stopped child `pid`, or by a
/// connection that is mid-disconnect. A live foreign owner does not delay the
/// restart; conflict detection handles it once the new child starts.
fn departing_owner<B>(pid: u32, bus: &B) -> bool
where
    B: Fn(&str, &str) -> Result<String, &'static str>,
{
    NAMES.iter().any(|name| {
        let Ok(reply) = bus("org.freedesktop.DBus.GetNameOwner", name) else {
            return false;
        };
        let Some(owner) = parse_owner(&reply) else {
            return false;
        };
        match bus("org.freedesktop.DBus.GetConnectionUnixProcessID", owner) {
            Ok(reply) => parse_uint(&reply) == Some(pid),
            Err("bus-nonzero") => true,
            Err(_) => false,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn finite_bus_failures_use_actual_production_command_path() {
        for (script, expected) in [
            ("exit 9", "bus-nonzero"),
            ("printf '\\377'", "bus-malformed"),
            ("head -c 1025 /dev/zero", "bus-oversize"),
        ] {
            let mut command = Command::new("/bin/sh");
            command
                .args(["-c", script])
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            assert_eq!(bus_call_command(command).unwrap_err(), expected);
        }
        assert_eq!(
            bus_call_command(Command::new("/nonexistent/tuna-orca-test")).unwrap_err(),
            "bus-spawn"
        );
    }
    #[test]
    fn finite_error_kinds_never_emit_error_content() {
        assert_eq!(
            error_kind(std::io::ErrorKind::PermissionDenied),
            "permission-denied"
        );
        assert_eq!(error_kind(std::io::ErrorKind::Other), "other");
    }
    #[test]
    fn child_has_parent_death_signal_before_exec() {
        let mut command = Command::new("/usr/bin/python3");
        command.args(["-c", "import ctypes, os; p=ctypes.c_int(); assert ctypes.CDLL(None).prctl(2,ctypes.byref(p)) == 0; assert p.value == 15; assert os.getpgrp() == os.getpid()"]);
        parent_lifetime(&mut command);
        assert!(command.status().unwrap().success());
    }
    #[test]
    fn disable_reaps_only_owned_child() {
        let child = Command::new("/bin/sleep").arg("30").spawn().unwrap();
        let mut other = Command::new("/bin/sleep").arg("30").spawn().unwrap();
        let mut reader = Reader::new("test-display", None);
        reader.enabled = true;
        reader.child = Some(child);
        reader.set_enabled(false);
        // stop never drops the unreaped Child or waits for it on a frame.
        assert!(reader.pid().is_some());
        let deadline = Instant::now() + Duration::from_secs(1);
        while reader.pid().is_some() && Instant::now() < deadline {
            reader.poll(false);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(reader.pid().is_none());
        assert!(other.try_wait().unwrap().is_none());
        other.kill().unwrap();
        other.wait().unwrap();
    }

    /// Fake session bus: `owners` maps a well-known name to (unique name, pid).
    fn fake_bus(
        owners: Vec<(&'static str, &'static str, u32)>,
        vanished: &'static [&'static str],
    ) -> impl Fn(&str, &str) -> Result<String, &'static str> {
        let uid = rustix::process::geteuid().as_raw();
        move |method, arg| match method {
            "org.freedesktop.DBus.GetNameOwner" => owners
                .iter()
                .find(|(name, _, _)| *name == arg)
                .map(|(_, owner, _)| format!("('{owner}',)\n"))
                .ok_or("bus-nonzero"),
            _ if vanished.contains(&arg) => Err("bus-nonzero"),
            "org.freedesktop.DBus.GetConnectionUnixProcessID" => owners
                .iter()
                .find(|(_, owner, _)| *owner == arg)
                .map(|(_, _, pid)| format!("(uint32 {pid},)\n"))
                .ok_or("bus-nonzero"),
            "org.freedesktop.DBus.GetConnectionUnixUser" => Ok(format!("(uint32 {uid},)\n")),
            _ => Err("bus-nonzero"),
        }
    }
    #[test]
    fn owner_vanishing_mid_probe_is_not_a_failure() {
        let euid = rustix::process::geteuid().as_raw();
        let bus = fake_bus(vec![("org.gnome.Orca1.Service", ":1.9", 40)], &[":1.9"]);
        assert_eq!(
            probe_orca_name(41, euid, &bus),
            Probe::Absent("owner-vanished")
        );
        let bus = fake_bus(vec![("org.gnome.Orca1.Service", ":1.9", 41)], &[]);
        assert_eq!(probe_orca_name(41, euid, &bus), Probe::Active);
        let bus = fake_bus(vec![("org.gnome.Orca1.Service", ":1.9", 40)], &[]);
        assert_eq!(
            probe_orca_name(41, euid, &bus),
            Probe::Failed(State::Conflict, "credentials-conflict")
        );
        assert_eq!(
            probe_orca_name(41, euid, &fake_bus(vec![], &[])),
            Probe::Absent("bus-nonzero")
        );
    }
    #[test]
    fn slow_registration_is_reported_but_never_abandoned() {
        let appeared = Instant::now() + Duration::from_millis(400);
        let late = fake_bus(vec![("org.gnome.Orca1.Service", ":1.4", 41)], &[]);
        let bus = |method: &str, arg: &str| {
            if Instant::now() < appeared {
                Err("bus-nonzero")
            } else {
                late(method, arg)
            }
        };
        let cancel = AtomicBool::new(false);
        let mut seen = Vec::new();
        watch_orca_name(
            41,
            &bus,
            &cancel,
            Duration::from_millis(100),
            |state, reason| {
                seen.push((state, reason));
                state != State::Active
            },
        );
        assert_eq!(
            seen,
            [(State::Starting, "slow-start"), (State::Active, "active")]
        );
    }
    #[test]
    fn cancelled_watch_reports_nothing() {
        let cancel = AtomicBool::new(true);
        let bus = fake_bus(vec![], &[]);
        watch_orca_name(41, &bus, &cancel, Duration::ZERO, |_, _| panic!("reported"));
    }
    #[test]
    fn restart_waits_only_for_the_stopped_childs_names() {
        let old = fake_bus(vec![("org.gnome.Orca1.Service", ":1.7", 41)], &[]);
        assert!(departing_owner(41, &old));
        let closing = fake_bus(vec![("org.gnome.Orca.Service", ":1.7", 41)], &[":1.7"]);
        assert!(departing_owner(41, &closing));
        let foreign = fake_bus(vec![("org.gnome.Orca1.Service", ":1.8", 77)], &[]);
        assert!(!departing_owner(41, &foreign));
        assert!(!departing_owner(41, &fake_bus(vec![], &[])));
    }
    #[test]
    fn no_new_child_while_predecessor_departs() {
        let (sender, receiver) = mpsc::channel();
        let mut reader = Reader::new("test-display", None);
        reader.enabled = true;
        reader.departing = Some(receiver);
        reader.poll(false);
        assert!(reader.pid().is_none());
        assert_eq!(reader.attempts, 0);
        sender.send(()).unwrap();
        assert!(!reader.departure_pending());
        assert!(reader.departing.is_none());
    }
    /// Fake clock: `exits_after` checks before the leader is seen exited.
    fn retire_fake(exits_after: Option<usize>) -> (Retirement, Vec<libc::c_int>, Duration) {
        let start = Instant::now();
        let clock = std::cell::Cell::new(start);
        let checks = std::cell::Cell::new(0usize);
        let mut signals = Vec::new();
        let retirement = retire_group(
            || {
                checks.set(checks.get() + 1);
                exits_after.is_some_and(|after| checks.get() > after)
            },
            |signal| signals.push(signal),
            |pause| clock.set(clock.get() + pause),
            || clock.get(),
            SHUTDOWN_GRACE,
        );
        (retirement, signals, clock.get() - start)
    }
    #[test]
    fn cooperative_reader_gets_term_then_group_sweep_without_waiting_out_the_grace() {
        let (retirement, signals, waited) = retire_fake(Some(4));
        assert_eq!(
            retirement,
            Retirement {
                terminated: true,
                exited_in_grace: true
            }
        );
        assert_eq!(signals, [libc::SIGTERM, libc::SIGKILL]);
        assert_eq!(waited, Duration::from_millis(30));
    }
    #[test]
    fn reader_whose_shutdown_handler_failed_is_killed_exactly_at_the_grace() {
        let (retirement, signals, waited) = retire_fake(None);
        assert_eq!(
            retirement,
            Retirement {
                terminated: true,
                exited_in_grace: false
            }
        );
        assert_eq!(signals, [libc::SIGTERM, libc::SIGKILL]);
        assert_eq!(waited, SHUTDOWN_GRACE);
    }
    #[test]
    fn already_exited_reader_only_has_its_group_swept() {
        let (retirement, signals, waited) = retire_fake(Some(0));
        assert_eq!(
            retirement,
            Retirement {
                terminated: false,
                exited_in_grace: true
            }
        );
        assert_eq!(signals, [libc::SIGKILL]);
        assert_eq!(waited, Duration::ZERO);
    }
    fn group_child(script: &str) -> (Child, u32) {
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", script])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        parent_lifetime(&mut command);
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
    #[test]
    fn actual_exit_is_observed_without_reaping_the_group_leader() {
        let (mut child, member) = group_child("(exec sleep 30) & echo $!; exec sleep 30");
        let pid = child.id();
        assert!(!exited_unreaped(pid));
        let started = Instant::now();
        let retirement = retire_group(
            || exited_unreaped(pid),
            |signal| signal_group(&child, signal),
            std::thread::sleep,
            Instant::now,
            SHUTDOWN_GRACE,
        );
        assert_eq!(
            retirement,
            Retirement {
                terminated: true,
                exited_in_grace: true
            }
        );
        assert!(started.elapsed() < SHUTDOWN_GRACE);
        // Still unreaped: its PID, so its group ID, stayed ours throughout.
        assert!(exited_unreaped(pid));
        assert!(std::path::Path::new(&format!("/proc/{pid}")).exists());
        assert!(child.wait().unwrap().signal().is_some());
        let until = Instant::now() + Duration::from_secs(1);
        while !gone(member) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(gone(member));
    }
    #[test]
    fn session_end_kills_and_reaps_a_reader_group_that_ignores_term() {
        // Orca 51.1 can survive its SIGTERM when its handler raises (#836).
        let (child, member) = group_child("trap '' TERM; (exec sleep 30) & echo $!; exec sleep 30");
        let pid = child.id();
        let mut reader = Reader::new("isolated-test", None);
        reader.enabled = true;
        reader.child = Some(child);
        let started = Instant::now();
        let receipt = reader.retire().unwrap();
        assert!(started.elapsed() >= SHUTDOWN_GRACE);
        assert!(started.elapsed() < SHUTDOWN_GRACE + REAP_BOUND + Duration::from_millis(500));
        assert_eq!(
            (receipt.pid, receipt.retirement, receipt.reaped),
            (
                pid,
                Retirement {
                    terminated: true,
                    exited_in_grace: false
                },
                true
            )
        );
        assert!(receipt.elapsed_ms >= SHUTDOWN_GRACE.as_millis());
        assert_eq!(
            receipt.to_string(),
            format!(
                "tuna-compositor: screen reader retired pid={pid} terminated=true \
                 exited_in_grace=false reaped=true elapsed_ms={}",
                receipt.elapsed_ms
            )
        );
        assert!(reader.pid().is_none());
        assert!(!reader.enabled);
        // Reaped by retire(), not left as our zombie.
        assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
        let until = Instant::now() + Duration::from_secs(1);
        while !gone(member) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(gone(member));
        // Idempotent: Drop's repeat signals nothing and reports nothing.
        assert!(reader.retire().is_none());
    }
    #[test]
    fn reader_already_reaped_by_poll_still_gets_its_session_end_receipt() {
        // A reader that honoured systemd's own SIGTERM is reaped by poll()
        // before the loop observes shutdown; it is gone, nothing is signalled.
        let (child, _) = group_child("echo 0; exec sleep 30");
        let pid = child.id();
        let mut reader = Reader::new("isolated-test", None);
        reader.enabled = true;
        reader.child = Some(child);
        signal_group(reader.child.as_ref().unwrap(), libc::SIGTERM);
        let until = Instant::now() + Duration::from_secs(2);
        while reader.pid().is_some() && Instant::now() < until {
            reader.poll(false);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(reader.pid().is_none());
        let receipt = reader.retire().unwrap();
        assert_eq!(
            receipt,
            Receipt {
                pid,
                retirement: Retirement {
                    terminated: false,
                    exited_in_grace: true
                },
                reaped: true,
                elapsed_ms: 0
            }
        );
        assert!(reader.retire().is_none());
    }
    #[test]
    fn child_outside_its_own_group_is_signalled_alone() {
        // Never a group it merely joined (here: the test runner's own).
        let mut child = Command::new("/bin/sleep").arg("30").spawn().unwrap();
        signal_group(&child, libc::SIGKILL);
        assert_eq!(child.wait().unwrap().signal(), Some(libc::SIGKILL));
    }
}

#[cfg(test)]
mod wake_tests {
    use super::*;
    #[test]
    fn broken_observer_stops_bounded_retry_and_never_leaves_a_stale_deadline() {
        let mut reader = Reader::new("isolated-test", None);
        let (send, receive) = mpsc::channel();
        drop(send);
        reader.observed = Some(Observation {
            receiver: receive,
            cancel: Arc::new(AtomicBool::new(false)),
        });
        reader.enabled = true;
        reader.attempts = 1;
        assert_eq!(reader.poll(false), Some(State::Unavailable));
        assert_eq!(reader.attempts, 3);
        assert_eq!(reader.next_deadline(), None);
        assert!(reader.observed.is_none());
    }
}
