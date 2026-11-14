//! Owned GIO worker for `org.gnome.desktop.lockdown` `disable-lock-screen`
//! (#357, #503): the compositor's live read of the key behind its lock
//! backstop, followed as gnome-shell follows `changed::disable-lock-screen`
//! in-process (`screenShield.js`, `systemActions.js`).
//!
//! The mechanics are `privacy_screen_settings.rs`'s without its write: the
//! native `Settings` and its `changed` subscription live and retire on one
//! owned GLib thread, and the initial value and every change arrive on a
//! channel the tick drains without waiting, each followed by a loop wake.
//! Nothing is polled and no `gsettings` process is spawned: a key that does
//! not change costs no wake-up, and a stalled settings backend delays the
//! policy, never a frame. A missing schema or key, or a session without a
//! bus, leaves the worker without settings: nothing is published and the
//! hub keeps the policy it has (the refresh rule in `lockdown.rs`).
use crate::lockdown::{KEY_DISABLE_LOCK_SCREEN as KEY, SCHEMA};
use crate::wake::Wake;
use gio::{glib, prelude::*};
use std::sync::mpsc;

/// Opens the settings on the worker thread.
type Factory = Box<dyn FnOnce() -> Option<gio::Settings> + Send>;

pub(crate) struct LockdownSettings {
    reads: mpsc::Receiver<bool>,
    context: glib::MainContext,
    loop_: glib::MainLoop,
}

/// The installed schema, when it has the key as a boolean and a session
/// bus can deliver its changes. Without a bus the policy stays at what
/// the hub read at startup, as before.
fn open_installed() -> Option<gio::Settings> {
    if !crate::lockdown::session_bus_present() {
        return None;
    }
    let source = gio::SettingsSchemaSource::default()?;
    let schema = source.lookup(SCHEMA, true)?;
    if !(schema.has_key(KEY) && schema.key(KEY).value_type().as_str() == "b") {
        eprintln!(
            "tuna-compositor: {SCHEMA} has no boolean {KEY}; the lock policy is not followed"
        );
        return None;
    }
    Some(gio::Settings::new_full(
        &schema,
        None::<&gio::SettingsBackend>,
        None,
    ))
}

impl LockdownSettings {
    /// Start the worker on the installed schema; `wake` runs after each read.
    pub(crate) fn start(wake: &Wake) -> Self {
        Self::with_factory(wake, Box::new(open_installed))
    }

    fn with_factory(wake: &Wake, factory: Factory) -> Self {
        let (tx, reads) = mpsc::channel();
        let context = glib::MainContext::new();
        let loop_ = glib::MainLoop::new(Some(&context), false);
        let (worker_context, worker_loop, wake) = (context.clone(), loop_.clone(), wake.clone());
        let spawned = std::thread::Builder::new()
            .name("tuna-lockdown".into())
            .spawn(move || {
                let _ = worker_context.with_thread_default(|| {
                    let Some(settings) = factory() else {
                        return;
                    };
                    let publish = move |settings: &gio::Settings| {
                        if tx.send(settings.boolean(KEY)).is_ok() {
                            wake.signal();
                        }
                    };
                    // Connected BEFORE the first read: GSettings emits
                    // `changed` only for keys read since.
                    let changed = publish.clone();
                    let handler = settings.connect_changed(Some(KEY), move |s, _| changed(s));
                    publish(&settings);
                    worker_loop.run();
                    settings.disconnect(handler);
                });
            });
        if let Err(error) = spawned {
            eprintln!("tuna-compositor: lockdown settings worker: {error}");
        }
        Self {
            reads,
            context,
            loop_,
        }
    }

    /// The newest `disable-lock-screen` read since the last call, without
    /// waiting: of several changes between two ticks the last one wins.
    pub(crate) fn poll(&self) -> Option<bool> {
        self.reads.try_iter().last()
    }
}

impl Drop for LockdownSettings {
    fn drop(&mut self) {
        // The worker retires its settings on its own thread; no join. The
        // quit is queued on its context so it also ends a loop that has
        // not started running yet.
        let loop_ = self.loop_.clone();
        self.context.invoke(move || loop_.quit());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::time::{Duration, Instant};

    const LIMIT: Duration = Duration::from_secs(10);
    /// Longer than the 2 s `gsettings` poll this worker replaced.
    const QUIET: Duration = Duration::from_millis(2500);
    const SCHEMA_XML: &str =
        "<schemalist><schema id='org.gnome.desktop.lockdown' path='/tuna/lockdown-proof/'>\
        <key name='disable-lock-screen' type='b'><default>false</default></key>\
        </schema></schemalist>";

    thread_local! {
        /// The worker's settings, for the administrator's writes below.
        static ADMIN: RefCell<Option<gio::Settings>> = const { RefCell::new(None) };
    }

    fn compile(dir: &std::path::Path) {
        std::fs::write(dir.join("proof.gschema.xml"), SCHEMA_XML).unwrap();
        assert!(
            std::process::Command::new("timeout")
                .args(["--kill-after=2s", "10s", "glib-compile-schemas"])
                .arg(dir)
                .status()
                .unwrap()
                .success(),
            "required real schema compiler must execute"
        );
    }

    /// A worker on a real compiled schema, and a loop counting its wakes.
    fn worker(dir: std::path::PathBuf) -> (LockdownSettings, calloop::EventLoop<'static, usize>) {
        let (wake, source) = Wake::new().unwrap();
        let loop_ = calloop::EventLoop::try_new().unwrap();
        loop_
            .handle()
            .insert_source(source, |_, _, wakes| *wakes += 1)
            .unwrap();
        let settings = LockdownSettings::with_factory(
            &wake,
            Box::new(move || {
                let schemas = gio::SettingsSchemaSource::from_directory(&dir, None, false).unwrap();
                let schema = schemas.lookup(SCHEMA, false).unwrap();
                let backend = gio::memory_settings_backend_new();
                let settings = gio::Settings::new_full(&schema, Some(&backend), None);
                ADMIN.with(|cell| *cell.borrow_mut() = Some(settings.clone()));
                Some(settings)
            }),
        );
        (settings, loop_)
    }

    /// The administrator sets the key, on the thread that owns the settings.
    fn administer(settings: &LockdownSettings, value: bool) {
        settings.context.invoke(move || {
            ADMIN.with(|cell| {
                cell.borrow()
                    .as_ref()
                    .expect("the worker opened its settings")
                    .set_boolean(KEY, value)
                    .unwrap();
            })
        });
    }

    /// The next value, woken by the real wake source.
    fn next(
        settings: &LockdownSettings,
        loop_: &mut calloop::EventLoop<usize>,
        wakes: &mut usize,
    ) -> bool {
        let deadline = Instant::now() + LIMIT;
        loop {
            if let Some(value) = settings.poll() {
                return value;
            }
            let now = Instant::now();
            assert!(now < deadline, "the worker published no read");
            loop_.dispatch(Some(deadline - now), wakes).unwrap();
        }
    }

    /// Everything queued on the worker so far has run, its wakes included.
    fn settled(settings: &LockdownSettings) {
        let (done, landed) = mpsc::channel();
        settings.context.invoke(move || {
            let _ = done.send(());
        });
        landed.recv_timeout(LIMIT).unwrap();
    }

    #[test]
    fn a_change_of_the_key_arrives_as_an_event_and_an_unchanged_key_costs_no_wake() {
        let dir = tempfile::tempdir().unwrap();
        compile(dir.path());
        let (settings, mut loop_) = worker(dir.path().to_owned());
        let mut wakes = 0;
        assert!(
            !next(&settings, &mut loop_, &mut wakes),
            "GNOME's default allows the lock screen"
        );

        // Nothing changes: no read is published and the loop is not woken.
        settled(&settings);
        loop_.dispatch(Some(Duration::ZERO), &mut wakes).unwrap();
        let before = wakes;
        let quiet_until = Instant::now() + QUIET;
        while let Some(left) = quiet_until.checked_duration_since(Instant::now()) {
            loop_.dispatch(Some(left), &mut wakes).unwrap();
        }
        assert_eq!(wakes, before, "an unchanged key must not wake the loop");
        assert_eq!(settings.poll(), None, "an unchanged key publishes nothing");

        // The administrator restricts, then lifts: each change is read
        // from a loop that only its wake can end, with no poll interval.
        administer(&settings, true);
        assert!(next(&settings, &mut loop_, &mut wakes));
        administer(&settings, false);
        assert!(!next(&settings, &mut loop_, &mut wakes));
        settled(&settings);
        loop_.dispatch(Some(Duration::ZERO), &mut wakes).unwrap();
        assert!(wakes > before, "changes wake the loop");
        assert_eq!(settings.poll(), None);
    }

    #[test]
    fn of_several_changes_between_two_ticks_the_newest_wins() {
        let dir = tempfile::tempdir().unwrap();
        compile(dir.path());
        let (settings, mut loop_) = worker(dir.path().to_owned());
        let mut wakes = 0;
        assert!(!next(&settings, &mut loop_, &mut wakes));
        administer(&settings, true);
        administer(&settings, false);
        administer(&settings, true);
        // Let all three land before the one read that a tick would make.
        settled(&settings);
        assert_eq!(
            settings.poll(),
            Some(true),
            "the restriction set last holds"
        );
        assert_eq!(settings.poll(), None);
    }

    #[test]
    fn without_settings_nothing_is_published() {
        let (wake, _source) = Wake::new().unwrap();
        let settings = LockdownSettings::with_factory(&wake, Box::new(|| None));
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(
            settings.poll(),
            None,
            "an unreadable key keeps the cached policy"
        );
    }

    #[test]
    fn a_stalled_settings_backend_never_delays_the_caller() {
        let (wake, _source) = Wake::new().unwrap();
        let started = Instant::now();
        // The detached worker dies with the test process.
        let settings = LockdownSettings::with_factory(
            &wake,
            Box::new(|| {
                std::thread::sleep(Duration::from_secs(5));
                None
            }),
        );
        assert_eq!(settings.poll(), None, "the cached policy holds meanwhile");
        drop(settings);
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(1),
            "a hung backend must not stall the tick, took {elapsed:?}"
        );
    }
}
