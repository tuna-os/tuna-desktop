//! Owned GIO worker for `org.gnome.desktop.privacy` `privacy-screen`
//! (#356), the compositor's read and write of the key as mutter's
//! `meta-settings.c` does in-process (`privacy_settings_changed`,
//! `meta_settings_set_privacy_screen_enabled`).
//!
//! As in `wm_preferences.rs`, the native `Settings` and its `changed`
//! subscription live and retire on one owned GLib thread and never cross
//! it. Unlike the polled readers, this one pushes: the initial value and
//! every change arrive on a channel the frame consumer drains without
//! waiting, and a write is an `invoke` on the worker's context, so a
//! stalled settings backend delays the privacy screen, never a frame.
//! A missing schema or key leaves the worker without settings: nothing
//! is read and nothing is written, so the privacy screens stay as the
//! hardware has them.
use crate::privacy_screen::{KEY, SCHEMA};
use crate::wake::Wake;
use gio::{glib, prelude::*};
use std::cell::RefCell;
use std::sync::mpsc;

thread_local! {
    static SETTINGS: RefCell<Option<gio::Settings>> = const { RefCell::new(None) };
}

/// Opens the settings on the worker thread.
type Factory = Box<dyn FnOnce() -> Option<gio::Settings> + Send>;

pub(crate) struct PrivacyScreenSettings {
    reads: mpsc::Receiver<bool>,
    context: glib::MainContext,
    loop_: glib::MainLoop,
}

/// The installed schema, when it has the key as a boolean.
fn open_installed() -> Option<gio::Settings> {
    let source = gio::SettingsSchemaSource::default()?;
    let schema = source.lookup(SCHEMA, true)?;
    if !(schema.has_key(KEY) && schema.key(KEY).value_type().as_str() == "b") {
        eprintln!(
            "tuna-compositor: {SCHEMA} has no boolean {KEY}; privacy screens follow the hardware"
        );
        return None;
    }
    Some(gio::Settings::new_full(
        &schema,
        None::<&gio::SettingsBackend>,
        None,
    ))
}

impl PrivacyScreenSettings {
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
            .name("tuna-privacy-screen".into())
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
                    SETTINGS.with(|cell| *cell.borrow_mut() = Some(settings.clone()));
                    worker_loop.run();
                    SETTINGS.with(|cell| *cell.borrow_mut() = None);
                    settings.disconnect(handler);
                });
            });
        if let Err(error) = spawned {
            eprintln!("tuna-compositor: privacy screen settings worker: {error}");
        }
        Self {
            reads,
            context,
            loop_,
        }
    }

    /// The newest value read since the last call, without waiting.
    pub(crate) fn poll(&self) -> Option<bool> {
        self.reads.try_iter().last()
    }

    /// Write the key (the hotkey's write-back), skipped when it already
    /// holds `value`. Queued on the worker; never waits.
    pub(crate) fn write(&self, value: bool) {
        self.context.invoke(move || {
            SETTINGS.with(|cell| {
                if let Some(settings) = cell.borrow().as_ref() {
                    if settings.boolean(KEY) != value {
                        if let Err(error) = settings.set_boolean(KEY, value) {
                            eprintln!("tuna-compositor: could not save {SCHEMA} {KEY}: {error}");
                        }
                    }
                }
            })
        });
    }
}

impl Drop for PrivacyScreenSettings {
    fn drop(&mut self) {
        // The worker retires its settings on its own thread; no join.
        self.loop_.quit();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    const LIMIT: Duration = Duration::from_secs(10);
    const SCHEMA_XML: &str =
        "<schemalist><schema id='org.gnome.desktop.privacy' path='/tuna/privacy-proof/'>\
        <key name='privacy-screen' type='b'><default>false</default></key>\
        </schema></schemalist>";

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

    fn worker(dir: std::path::PathBuf) -> (PrivacyScreenSettings, calloop::EventLoop<'static, ()>) {
        let (wake, source) = Wake::new().unwrap();
        let loop_ = calloop::EventLoop::try_new().unwrap();
        loop_.handle().insert_source(source, |_, _, _| {}).unwrap();
        let settings = PrivacyScreenSettings::with_factory(
            &wake,
            Box::new(move || {
                let schemas = gio::SettingsSchemaSource::from_directory(&dir, None, false).unwrap();
                let schema = schemas.lookup(SCHEMA, false).unwrap();
                let backend = gio::memory_settings_backend_new();
                Some(gio::Settings::new_full(&schema, Some(&backend), None))
            }),
        );
        (settings, loop_)
    }

    /// The next value, woken by the real wake source.
    fn next(settings: &PrivacyScreenSettings, loop_: &mut calloop::EventLoop<()>) -> bool {
        let deadline = Instant::now() + LIMIT;
        loop {
            if let Some(value) = settings.poll() {
                return value;
            }
            let now = Instant::now();
            assert!(now < deadline, "the worker published no read");
            loop_.dispatch(Some(deadline - now), &mut ()).unwrap();
        }
    }

    #[test]
    fn worker_reads_the_key_and_delivers_its_own_writes_as_changes() {
        let dir = tempfile::tempdir().unwrap();
        compile(dir.path());
        let (settings, mut loop_) = worker(dir.path().to_owned());
        assert!(!next(&settings, &mut loop_), "GNOME's default is off");
        settings.write(true);
        assert!(next(&settings, &mut loop_), "the write reached GSettings");
        settings.write(false);
        assert!(!next(&settings, &mut loop_));
        // Writing the value it holds emits nothing.
        settings.write(false);
        settings.write(true);
        assert!(next(&settings, &mut loop_));
        assert_eq!(settings.poll(), None);
    }

    #[test]
    fn without_settings_nothing_is_read_or_written() {
        let (wake, _source) = Wake::new().unwrap();
        let settings = PrivacyScreenSettings::with_factory(&wake, Box::new(|| None));
        settings.write(true);
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(settings.poll(), None);
    }
}
