//! Native GNOME media bindings when the session has no gsd-media-keys.
//! Mutations are serialized and each external call has a five-second bound.
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::io::Write;
use std::rc::{Rc, Weak};

use gio::prelude::*;
use gtk4::{gio, glib};

const AMPLIFIED_KEY: &str = "allow-volume-above-100-percent";

/// Opt-in journey diagnostics, enabled only while the harness marker exists.
/// Callers suppress these observations during lock input.
pub(crate) fn trace(stage: &str, details: serde_json::Value, locked: bool) {
    if locked || !trace_enabled() {
        return;
    }
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) } != 0 {
        return;
    }
    let start_ticks = std::fs::read_to_string("/proc/self/stat")
        .ok()
        .and_then(|s| s.rsplit_once(')').map(|(_, fields)| fields.to_owned()))
        .and_then(|s| {
            s.split_whitespace()
                .nth(19)
                .and_then(|n| n.parse::<u64>().ok())
        });
    let record = serde_json::json!({
        "actor": "shell", "pid": std::process::id(), "stage": stage,
        "monotonic_ns": now.tv_sec as u64 * 1_000_000_000 + now.tv_nsec as u64, "start_ticks": start_ticks,
        "details": details,
    });
    #[cfg(test)]
    DIAGNOSTIC_ROWS.with_borrow_mut(|rows| rows.push(record.clone()));
    let line = format!("tuna-media-diagnostic: {record}\n");
    let _ = std::io::stderr().lock().write_all(line.as_bytes());
}

pub(crate) fn trace_enabled() -> bool {
    std::env::var_os("TUNA_MEDIA_DIAGNOSTICS")
        .is_some_and(|path| std::path::Path::new(&path).is_file())
}

pub(crate) fn media_subset(list: &[tuna_shell_control::Accelerator]) -> serde_json::Value {
    serde_json::json!(list
        .iter()
        .filter(|a| matches!(a.keysym, 0x1008ff11 | 0x1008ff12 | 0x1008ff13 | 0x1008ffb2))
        .map(|a| serde_json::json!([a.action, a.keysym, a.mods, a.modes]))
        .collect::<Vec<_>>())
}

pub fn sound_settings() -> Option<gio::Settings> {
    let schema = gio::SettingsSchemaSource::default()?.lookup("org.gnome.desktop.sound", true)?;
    schema
        .has_key(AMPLIFIED_KEY)
        .then(|| gio::Settings::new_full(&schema, None::<&gio::SettingsBackend>, None))
}

pub fn output_limit(settings: Option<&gio::Settings>) -> f64 {
    crate::logic::volume_limit(settings.is_some_and(|s| s.boolean(AMPLIFIED_KEY)))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VolumeKey {
    Up,
    Down,
    Mute,
}

pub(crate) fn parse_volume(output: &str) -> Option<(f64, bool)> {
    let mut words = output.split_whitespace();
    if words.next()? != "Volume:" {
        return None;
    }
    let percent = words.next()?.parse::<f64>().ok()? * 100.0;
    percent
        .is_finite()
        .then_some((percent, output.contains("[MUTED]")))
}

/// GNOME unmutes without raising an existing nonzero level; lowering to
/// zero mutes it. The ordinary range stops at 100 percent.
fn adjusted(percent: f64, muted: bool, key: VolumeKey, step: f64, limit: f64) -> (f64, bool) {
    match key {
        VolumeKey::Mute => (percent.clamp(0.0, limit), !muted),
        VolumeKey::Up => (
            (if muted && percent > 0.0 {
                percent
            } else {
                percent + step
            })
            .clamp(0.0, limit),
            false,
        ),
        VolumeKey::Down => {
            let percent = (percent - step).clamp(0.0, limit);
            (percent, muted || percent == 0.0)
        }
    }
}

type Osd = Rc<dyn Fn(f64, bool, bool)>;
type MediaIntent = (VolumeKey, f64, bool, bool, u64);
#[derive(Clone)]
pub struct MediaKeys(Rc<Inner>);
struct Inner {
    queue: RefCell<VecDeque<MediaIntent>>,
    diagnostic_token: Cell<u64>,
    diagnostic_instance: u64,
    diagnostic_active: Cell<Option<(u64, &'static str)>>,
    diagnostic_clamp: Cell<u64>,
    diagnostic_failed: Cell<u64>,
    diagnostic_policy: Cell<bool>,
    clamp_pending: Cell<bool>,
    busy: Cell<bool>,
    osd: Osd,
    sound: Option<gio::Settings>,
}
impl MediaKeys {
    pub fn new(osd: Osd) -> Self {
        Self::with_sound(osd, sound_settings())
    }
    fn with_sound(osd: Osd, sound: Option<gio::Settings>) -> Self {
        let policy = sound.as_ref().is_some_and(|s| s.boolean(AMPLIFIED_KEY));
        let this = Self(Rc::new(Inner {
            queue: RefCell::new(VecDeque::new()),
            busy: Cell::new(false),
            clamp_pending: Cell::new(true),
            osd,
            sound,
            diagnostic_token: Cell::new(0),
            diagnostic_instance: NEXT_INSTANCE.with(|next| {
                let id = next.get().saturating_add(1);
                next.set(id);
                id
            }),
            diagnostic_active: Cell::new(None),
            diagnostic_clamp: Cell::new(0),
            diagnostic_failed: Cell::new(0),
            diagnostic_policy: Cell::new(policy),
        }));
        if let Some(settings) = this.0.sound.as_ref() {
            let weak = Rc::downgrade(&this.0);
            settings.connect_changed(Some(AMPLIFIED_KEY), move |_, _| {
                if let Some(inner) = weak.upgrade() {
                    inner.diagnostic_policy.set(
                        inner
                            .sound
                            .as_ref()
                            .is_some_and(|s| s.boolean(AMPLIFIED_KEY)),
                    );
                    let media = Self(inner);
                    media.accept_clamp();
                    media.0.clamp_pending.set(true);
                    media.next();
                }
            });
        }
        OBSERVERS.with_borrow_mut(|observers| {
            observers.retain(|w| w.strong_count() != 0);
            observers.push(Rc::downgrade(&this.0));
        });
        this.accept_clamp();
        this.next();
        this
    }
    pub fn change(&self, key: VolumeKey, step: f64, microphone: bool, quiet: bool) {
        // Bound repeated input while a missing audio service is timing out.
        if self.0.queue.borrow().len() >= 32 {
            return;
        }
        let token = self.accept("key");
        self.0
            .queue
            .borrow_mut()
            .push_back((key, step, microphone, quiet, token));
        self.next();
    }
    // Diagnostic tokens never determine execution, ordering or policy.
    fn accept(&self, kind: &'static str) -> u64 {
        let token = self.0.diagnostic_token.get().saturating_add(1);
        self.0.diagnostic_token.set(token);
        self.observe(
            "media-accepted",
            serde_json::json!({"token":token,"kind":kind}),
        );
        token
    }
    fn accept_clamp(&self) {
        let token = self.accept("clamp");
        let previous = self.0.diagnostic_clamp.replace(token);
        if previous != 0 {
            self.observe(
                "media-complete",
                serde_json::json!({"token":previous,"kind":"clamp","outcome":"superseded"}),
            );
        }
    }
    fn observe(&self, stage: &str, details: serde_json::Value) {
        if !trace_enabled() {
            return;
        }
        let mut details = details;
        details["instance"] = serde_json::json!(self.0.diagnostic_instance);
        trace(stage, details, false);
    }
    fn finish(&self, success: bool) {
        if let Some((token, kind)) = self.0.diagnostic_active.take() {
            if !success {
                self.0
                    .diagnostic_failed
                    .set(self.0.diagnostic_failed.get().max(token));
            }
            self.observe("media-complete", serde_json::json!({"token":token,"kind":kind,"outcome":if success {"success"} else {"failure"}}));
        }
        self.0.busy.set(false);
        self.next();
    }
    fn observe_scheduler(&self) {
        if !trace_enabled() {
            return;
        }
        let mut pending: Vec<u64> = self.0.queue.borrow().iter().map(|item| item.4).collect();
        if let Some((token, _)) = self.0.diagnostic_active.get() {
            pending.push(token);
        }
        if self.0.diagnostic_clamp.get() != 0 {
            pending.push(self.0.diagnostic_clamp.get());
        }
        self.observe("media-scheduler", serde_json::json!({
            "accepted":self.0.diagnostic_token.get(), "pending":pending,
            "busy":self.0.busy.get(),"clamp_pending":self.0.clamp_pending.get(),
            "failed":self.0.diagnostic_failed.get(),"allow_amplified":self.0.diagnostic_policy.get(),
            "command_busy":COMMAND_BUSY.get(),"commands":COMMANDS.with_borrow(VecDeque::len)
        }));
    }
    fn next(&self) {
        if self.0.busy.get() {
            return;
        }
        if self.0.clamp_pending.replace(false) {
            self.0.busy.set(true);
            self.0
                .diagnostic_active
                .set(Some((self.0.diagnostic_clamp.replace(0), "clamp")));
            let this = self.clone();
            wpctl(&["get-volume", "@DEFAULT_AUDIO_SINK@"], move |out| {
                let Some((percent, _)) = out.as_deref().and_then(parse_volume) else {
                    this.finish(false);
                    return;
                };
                let limit = output_limit(this.0.sound.as_ref());
                if percent > limit {
                    let volume = format!("{:.4}", limit / 100.0);
                    wpctl(
                        &["set-volume", "@DEFAULT_AUDIO_SINK@", &volume],
                        move |result| this.finish(result.is_some()),
                    );
                } else {
                    this.finish(true);
                }
            });
            return;
        }
        let Some((key, step, microphone, quiet, token)) = self.0.queue.borrow_mut().pop_front()
        else {
            return;
        };
        self.0.busy.set(true);
        self.0.diagnostic_active.set(Some((token, "key")));
        let target = if microphone {
            "@DEFAULT_AUDIO_SOURCE@"
        } else {
            "@DEFAULT_AUDIO_SINK@"
        };
        let this = self.clone();
        wpctl(&["get-volume", target], move |out| {
            let Some((percent, muted)) = out.as_deref().and_then(parse_volume) else {
                this.finish(false);
                return;
            };
            let before = (percent, muted);
            let (percent, muted) = adjusted(
                percent,
                muted,
                key,
                step,
                if microphone {
                    100.0
                } else {
                    output_limit(this.0.sound.as_ref())
                },
            );
            let volume = format!("{:.4}", percent / 100.0);
            wpctl(&["set-volume", target, &volume], move |success| {
                if success.is_none() {
                    this.finish(false);
                    return;
                }
                wpctl(
                    &["set-mute", target, if muted { "1" } else { "0" }],
                    move |success| {
                        if success.is_none() {
                            this.finish(false);
                            return;
                        }
                        wpctl(&["get-volume", target], move |out| {
                            let observed = out.as_deref().and_then(parse_volume);
                            if let Some((percent, muted)) = observed {
                                (this.0.osd)(percent, muted, microphone);
                                if !microphone {
                                    crate::sound_feedback::volume(
                                        quiet,
                                        before != (percent, muted),
                                        muted,
                                    );
                                }
                            }
                            this.finish(observed.is_some());
                        });
                    },
                );
            });
        });
    }
}

type AudioCommand = (Vec<String>, Box<dyn FnOnce(Option<String>)>);
thread_local! {
    static COMMANDS: RefCell<VecDeque<AudioCommand>> = const { RefCell::new(VecDeque::new()) };
    static NEXT_INSTANCE: Cell<u64> = const { Cell::new(0) };
    static COMMAND_BUSY: Cell<bool> = const { Cell::new(false) };
    static OBSERVERS: RefCell<Vec<Weak<Inner>>> = const { RefCell::new(Vec::new()) };
    #[cfg(test)]
    static DIAGNOSTIC_ROWS: RefCell<Vec<serde_json::Value>> = const { RefCell::new(Vec::new()) };
}

/// Serialize slider writes with key mutations and the live policy clamp: an
/// older amplified slider write must finish before a policy clamp reads it.
pub(crate) fn wpctl(args: &[&str], done: impl FnOnce(Option<String>) + 'static) {
    if COMMANDS.with_borrow(|queue| queue.len() >= 128) {
        done(None);
        return;
    }
    COMMANDS.with_borrow_mut(|queue| {
        queue.push_back((args.iter().map(|s| s.to_string()).collect(), Box::new(done)))
    });
    next_command();
}

fn next_command() {
    if COMMAND_BUSY.get() {
        return;
    }
    let Some((args, done)) = COMMANDS.with_borrow_mut(VecDeque::pop_front) else {
        // This runs after communicate completion, done(), busy release and
        // next scheduling. A fixture child exit/set-volume write is earlier.
        OBSERVERS.with_borrow_mut(|observers| {
            observers.retain(|weak| {
                if let Some(inner) = weak.upgrade() {
                    MediaKeys(inner).observe_scheduler();
                    true
                } else {
                    false
                }
            });
        });
        return;
    };
    COMMAND_BUSY.set(true);
    let argv: Vec<_> = args.iter().map(String::as_str).collect();
    run_wpctl(&argv, move |result| {
        done(result);
        COMMAND_BUSY.set(false);
        next_command();
    });
}

fn run_wpctl(args: &[&str], done: impl FnOnce(Option<String>) + 'static) {
    let mut argv = vec![std::ffi::OsStr::new("wpctl")];
    argv.extend(args.iter().map(std::ffi::OsStr::new));
    let launcher = gio::SubprocessLauncher::new(
        gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_SILENCE,
    );
    let parent = std::process::id() as libc::pid_t;
    launcher.set_child_setup(move || unsafe {
        if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0
            || libc::getppid() != parent
        {
            libc::_exit(127);
        }
        libc::signal(libc::SIGALRM, libc::SIG_DFL);
        libc::alarm(5);
    });
    let Ok(process) = launcher.spawn(&argv) else {
        done(None);
        return;
    };
    let timer = Rc::new(RefCell::new(None));
    let timeout_process = process.clone();
    let timeout_timer = timer.clone();
    *timer.borrow_mut() = Some(glib::timeout_add_local_once(
        std::time::Duration::from_secs(5),
        move || {
            timeout_timer.borrow_mut().take();
            timeout_process.force_exit();
        },
    ));
    let read_process = process.clone();
    process.communicate_utf8_async(None, gio::Cancellable::NONE, move |result| {
        if let Some(timer) = timer.borrow_mut().take() {
            timer.remove();
        }
        let output = result
            .ok()
            .filter(|_| read_process.is_successful())
            .map(|(out, _)| out.map_or_else(String::new, |out| out.to_string()));
        done(output);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn amplified_output_obeys_live_limit_and_preserves_unmute_level() {
        let max = crate::logic::volume_limit(true);
        assert_eq!(
            adjusted(98.0, false, VolumeKey::Up, 6.0, max),
            (104.0, false)
        );
        assert_eq!(
            adjusted(150.0, false, VolumeKey::Up, 6.0, max),
            (max, false)
        );
        assert_eq!(
            adjusted(120.0, true, VolumeKey::Up, 6.0, max),
            (120.0, false)
        );
        assert_eq!(
            adjusted(120.0, false, VolumeKey::Up, 6.0, 100.0),
            (100.0, false)
        );
        assert_eq!(
            adjusted(98.0, false, VolumeKey::Up, 6.0, 100.0),
            (100.0, false)
        );
        assert_eq!(output_limit(None), 100.0);
    }

    #[test]
    fn unmute_cannot_bypass_disabled_amplification() {
        assert_eq!(
            adjusted(140.0, true, VolumeKey::Mute, 0.0, 100.0),
            (100.0, false)
        );
        assert_eq!(
            adjusted(
                140.0,
                true,
                VolumeKey::Mute,
                0.0,
                crate::logic::volume_limit(true)
            ),
            (140.0, false)
        );
    }

    #[test]
    fn gnome_volume_unmute_zero_and_bounds() {
        assert_eq!(
            adjusted(40.0, true, VolumeKey::Up, 6.0, 100.0),
            (40.0, false)
        );
        assert_eq!(adjusted(0.0, true, VolumeKey::Up, 6.0, 100.0), (6.0, false));
        assert_eq!(
            adjusted(4.0, false, VolumeKey::Down, 6.0, 100.0),
            (0.0, true)
        );
        assert_eq!(
            adjusted(98.0, false, VolumeKey::Up, 6.0, 100.0),
            (100.0, false)
        );
        assert_eq!(
            adjusted(40.0, false, VolumeKey::Mute, 0.0, 100.0),
            (40.0, true)
        );
        assert_eq!(parse_volume("Volume: 0.40 [MUTED]"), Some((40.0, true)));
        assert_eq!(parse_volume("Volume: NaN"), None);
        assert_eq!(parse_volume("no default sink"), None);
    }
}

#[cfg(test)]
mod completion_tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    struct Held {
        socket: UnixStream,
        row: serde_json::Value,
    }
    impl Held {
        fn send(&mut self, value: serde_json::Value) {
            writeln!(self.socket, "{value}").unwrap();
        }
        fn release(mut self) {
            self.send(serde_json::json!({"go":true}));
            if self.row["stage"] == "enter" {
                let mut line = String::new();
                BufReader::new(&mut self.socket)
                    .read_line(&mut line)
                    .unwrap();
                let exit: serde_json::Value = serde_json::from_str(&line).unwrap();
                assert_eq!(exit["stage"], "exit");
                assert_eq!(exit["pid"], self.row["pid"]);
                self.send(serde_json::json!({"go":true}));
            }
        }
    }
    struct Fixture {
        root: PathBuf,
        listener: UnixListener,
    }
    impl Fixture {
        fn new() -> Self {
            let root = PathBuf::from(
                std::env::var_os("TUNA_MEDIA_FIXTURE_ROOT").expect("CI owned fixture required"),
            );
            let listener = UnixListener::bind(root.join("control.sock")).unwrap();
            listener.set_nonblocking(true).unwrap();
            Self { root, listener }
        }
        fn volume(&self) -> String {
            std::fs::read_to_string(self.root.join("volume")).unwrap()
        }
        fn reset(&self, value: &str) {
            std::fs::write(self.root.join("volume"), value).unwrap();
            std::fs::write(self.root.join("muted"), "0").unwrap();
        }
        fn poll(&self, hold_stage: &str, hold_command: &str) -> Option<Held> {
            let (mut socket, _) = match self.listener.accept() {
                Ok(pair) => pair,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return None,
                Err(e) => panic!("owned fixture accept: {e}"),
            };
            socket
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            socket
                .set_write_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut line = String::new();
            BufReader::new(&mut socket).read_line(&mut line).unwrap();
            let row: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert_eq!(row["stage"], "enter");
            assert_eq!(
                row["parent_pid"].as_u64(),
                Some(u64::from(std::process::id()))
            );
            assert!(row["start_ticks"].as_u64().unwrap() > 0);
            let mut held = Held { socket, row };
            if hold_stage == "enter" && held.row["args"][0] == hold_command {
                return Some(held);
            }
            held.send(serde_json::json!({"go":true}));
            line.clear();
            BufReader::new(&mut held.socket)
                .read_line(&mut line)
                .unwrap();
            let exit: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert_eq!(exit["stage"], "exit");
            assert_eq!(exit["pid"], held.row["pid"]);
            held.row = exit;
            if hold_stage == "exit" && held.row["args"][0] == hold_command {
                return Some(held);
            }
            held.send(serde_json::json!({"go":true}));
            None
        }
        fn pump(&self) {
            let context = glib::MainContext::default();
            while context.pending() {
                context.iteration(false);
            }
        }
        fn hold(&self, stage: &str, command: &str) -> Held {
            let end = Instant::now() + Duration::from_secs(3);
            loop {
                self.pump();
                if let Some(held) = self.poll(stage, command) {
                    return held;
                }
                assert!(
                    Instant::now() < end,
                    "production command never reached owned phase"
                );
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        fn idle(&self, media: &MediaKeys) {
            let end = Instant::now() + Duration::from_secs(3);
            loop {
                self.pump();
                assert!(self.poll("none", "none").is_none());
                if !media.0.busy.get()
                    && media.0.queue.borrow().is_empty()
                    && !media.0.clamp_pending.get()
                    && !COMMAND_BUSY.get()
                    && COMMANDS.with_borrow(VecDeque::is_empty)
                {
                    break;
                }
                assert!(
                    Instant::now() < end,
                    "actual production callbacks failed to settle"
                );
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        fn barrier(&self, since: u64, kind: &str, policy: bool) -> std::process::ExitStatus {
            self.barrier_owner(since, kind, policy, true)
        }
        fn barrier_owner(
            &self,
            since: u64,
            kind: &str,
            policy: bool,
            valid_owner: bool,
        ) -> std::process::ExitStatus {
            let rows = DIAGNOSTIC_ROWS.with_borrow(Clone::clone);
            let log = rows
                .iter()
                .map(|row| format!("tuna-media-diagnostic: {row}\n"))
                .collect::<String>();
            std::fs::write(self.root.join("observer.log"), log).unwrap();
            let stat = std::fs::read_to_string("/proc/self/stat").unwrap();
            let start: u64 = stat
                .rsplit_once(')')
                .unwrap()
                .1
                .split_whitespace()
                .nth(19)
                .unwrap()
                .parse()
                .unwrap();
            std::fs::write(
                self.root.join("owner.json"),
                serde_json::json!({"pid":std::process::id(),"start_ticks":if valid_owner { start } else { start + 1 }}).to_string(),
            )
            .unwrap();
            std::process::Command::new("/usr/bin/python3")
                .arg(std::env::var_os("TUNA_MEDIA_BARRIER_HELPER").expect("actual helper required"))
                .args(["--check-once", "--log"])
                .arg(self.root.join("observer.log"))
                .args(["--pid", &std::process::id().to_string(), "--owner"])
                .arg(self.root.join("owner.json"))
                .args([
                    "--since",
                    &since.to_string(),
                    "--kind",
                    kind,
                    "--policy",
                    if policy { "true" } else { "false" },
                    "--receipt",
                ])
                .arg(self.root.join("barrier.json"))
                .status()
                .unwrap()
        }
    }
    fn stamp() -> u64 {
        let mut now = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        assert_eq!(
            unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) },
            0
        );
        now.tv_sec as u64 * 1_000_000_000 + now.tv_nsec as u64
    }

    #[test]
    #[ignore = "requires CI-owned executable fixture; run exact admitted filter"]
    fn actual_media_keys_clamp_key_completion_barrier() {
        assert_eq!(
            std::env::var("TUNA_REQUIRE_MEDIA_COMPLETION").as_deref(),
            Ok("1")
        );
        assert!(Path::new(
            &std::env::var_os("TUNA_MEDIA_DIAGNOSTICS").expect("diagnostic marker required")
        )
        .is_file());
        let fixture = Fixture::new();
        fixture.reset("0.40");
        let backend = gio::memory_settings_backend_new();
        let schema = gio::SettingsSchemaSource::default()
            .unwrap()
            .lookup("org.gnome.desktop.sound", true)
            .unwrap();
        let settings = gio::Settings::new_full(&schema, Some(&backend), None);
        settings.set_boolean(AMPLIFIED_KEY, true).unwrap();
        let media = MediaKeys::with_sound(Rc::new(|_, _, _| {}), Some(settings.clone()));
        fixture.idle(&media);

        // A changed policy requires its own fresh clamp completion, not a
        // scheduler-only idle snapshot or an unrelated earlier mutation.
        fixture.reset("1.50");
        let policy_since = stamp();
        settings.set_boolean(AMPLIFIED_KEY, false).unwrap();
        let policy_clamp = fixture.hold("exit", "set-volume");
        assert!(!fixture.barrier(policy_since, "clamp", false).success());
        policy_clamp.release();
        fixture.idle(&media);
        assert!(fixture.barrier(policy_since, "clamp", false).success());
        assert!(!fixture.barrier(stamp(), "clamp", false).success());
        settings.set_boolean(AMPLIFIED_KEY, true).unwrap();
        fixture.idle(&media);

        // Old predicate sees the clamp's1.0 while a real Raise is queued.
        fixture.reset("1.50");
        settings.set_boolean(AMPLIFIED_KEY, false).unwrap();
        let clamp = fixture.hold("exit", "set-volume");
        assert_eq!(fixture.volume(), "1.0000");
        let old = stamp();
        media.change(VolumeKey::Up, 6.0, false, true);
        assert_eq!(media.0.queue.borrow().len(), 1);
        assert!(!fixture.barrier(old, "key", false).success());
        clamp.release();
        let raise_read = fixture.hold("enter", "get-volume");
        assert!(!fixture.barrier(old, "key", false).success());
        fixture.reset("0.4200"); // precisely the old premature harness reset
        raise_read.release();
        fixture.idle(&media);
        assert_eq!(fixture.volume(), "0.4800");
        assert!(fixture.barrier(old, "key", false).success());

        settings.set_boolean(AMPLIFIED_KEY, true).unwrap();
        fixture.idle(&media);
        fixture.reset("1.50");
        settings.set_boolean(AMPLIFIED_KEY, false).unwrap();
        let clamp = fixture.hold("exit", "set-volume");
        let new = stamp();
        media.change(VolumeKey::Up, 6.0, false, true);
        assert!(!fixture.barrier(new, "key", false).success());
        clamp.release();
        let read = fixture.hold("enter", "get-volume");
        assert!(!fixture.barrier(new, "key", false).success());
        read.release();
        let volume = fixture.hold("exit", "set-volume");
        assert_eq!(fixture.volume(), "1.0000");
        assert!(!fixture.barrier(new, "key", false).success());
        volume.release();
        let mute = fixture.hold("exit", "set-mute");
        assert!(!fixture.barrier(new, "key", false).success());
        mute.release();
        let terminal = fixture.hold("exit", "get-volume");
        assert!(!fixture.barrier(new, "key", false).success());
        terminal.release();
        // Child release still precedes GLib communicate callback/scheduler.
        assert!(!fixture.barrier(new, "key", false).success());
        fixture.idle(&media);
        assert!(fixture.barrier(new, "key", false).success());
        assert!(!fixture.barrier_owner(new, "key", false, false).success());
        assert!(!fixture.barrier(stamp(), "key", false).success());
        fixture.reset("0.4200");
        // Resetting an alreadyfalse setting has no changed signal to wait for.
        settings.reset(AMPLIFIED_KEY);
        fixture.idle(&media);
        assert_eq!(fixture.volume(), "0.4200");

        let concurrent = stamp();
        media.change(VolumeKey::Up, 6.0, false, true);
        let first = fixture.hold("enter", "get-volume");
        media.change(VolumeKey::Down, 6.0, false, true);
        assert_eq!(media.0.queue.borrow().len(), 1);
        assert!(!fixture.barrier(concurrent, "key", false).success());
        first.release();
        fixture.idle(&media);
        assert_eq!(fixture.volume(), "0.4200");

        let failure = stamp();
        media.change(VolumeKey::Up, 6.0, false, true);
        let mut broken = fixture.hold("enter", "get-volume");
        broken.send(serde_json::json!({"fail":true}));
        drop(broken);
        fixture.idle(&media);
        assert!(!fixture.barrier(failure, "key", false).success());

        let timeout = stamp();
        media.change(VolumeKey::Up, 6.0, false, true);
        let held = fixture.hold("enter", "get-volume");
        let end = Instant::now() + Duration::from_secs(7);
        while media.0.busy.get() || COMMAND_BUSY.get() {
            fixture.pump();
            assert!(
                Instant::now() < end,
                "existing5sec subprocess bound did not retire command"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        drop(held);
        assert!(!fixture.barrier(timeout, "key", false).success());
        assert!(COMMANDS.with_borrow(VecDeque::is_empty));
        println!(
            "MEDIA-COMPLETION pass old=0.4800 guarded=0.4200 callback-scheduler failure timeout"
        );
    }
}
