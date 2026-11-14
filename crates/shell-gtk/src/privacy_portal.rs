//! Keep the portal lockdown publisher alive in the actual GTK session.
//! Bus connection and property round trips stay off the rendering thread.
//!
//! The worker sleeps in its own GLib main loop until something can change
//! what it publishes (#503), as gnome-shell follows these keys in-process:
//!
//! - `changed` on one `org.gnome.desktop.privacy` GSettings it holds;
//! - a property set from the bus, staged for persistence
//!   ([`LockdownBus::on_staged`]);
//! - the backend name vanishing while another connection held it
//!   (`g_bus_watch_name`), so a restarted shell takes the name over as soon
//!   as the old one lets go.
//!
//! Two failure modes keep the former 100 ms retry, and only while they
//! last: a staged set the settings backend refused, and a publisher that
//! could not take the name while nobody else holds it (no bus yet).

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gtk4::prelude::*;
use tuna_shell_host::lockdown::{LockdownBus, LOCKDOWN_NAME};
use tuna_shell_host::settings::{
    SettingsBackend, DISABLE_CAMERA_KEY, DISABLE_MICROPHONE_KEY, DISABLE_SOUND_OUTPUT_KEY,
    PRIVACY_SCHEMA,
};

/// The former poll period, kept for the two failure modes above.
const RETRY: Duration = Duration::from_millis(100);
const KEYS: [&str; 3] = [
    DISABLE_CAMERA_KEY,
    DISABLE_MICROPHONE_KEY,
    DISABLE_SOUND_OUTPUT_KEY,
];

pub fn start(app: &libadwaita::Application) {
    let context = glib::MainContext::new();
    let main_loop = glib::MainLoop::new(Some(&context), false);
    let (quit_context, quit_loop) = (context.clone(), main_loop.clone());
    app.connect_shutdown(move |_| {
        let quit_loop = quit_loop.clone();
        quit_context.invoke(move || quit_loop.quit());
    });
    std::thread::Builder::new()
        .name("tuna-privacy-portal".into())
        .spawn(move || {
            context
                .with_thread_default(|| run(&main_loop, None, installed()))
                .expect("privacy portal worker could not acquire its GLib context");
        })
        .unwrap_or_else(|error| panic!("privacy portal worker failed: {error}"));
}

/// The installed privacy schema on the default backend, or `None` when it
/// is not installed (then nothing follows the keys, as before).
fn installed() -> Option<gio::Settings> {
    let schema = gio::SettingsSchemaSource::default()?.lookup(PRIVACY_SCHEMA, true)?;
    Some(gio::Settings::new_full(
        &schema,
        None::<&gio::SettingsBackend>,
        None,
    ))
}

/// The privacy keys through the one held `Settings`; other schemas and
/// unknown keys read as absent and refuse writes, like a missing schema.
struct Held(Option<gio::Settings>);

impl Held {
    fn settings(&self, schema: &str, key: &str) -> Option<&gio::Settings> {
        let settings = self.0.as_ref().filter(|_| schema == PRIVACY_SCHEMA)?;
        settings
            .settings_schema()
            .is_some_and(|schema| schema.has_key(key))
            .then_some(settings)
    }
}

impl SettingsBackend for Held {
    fn string(&self, schema: &str, key: &str) -> Option<String> {
        self.settings(schema, key)?
            .value(key)
            .str()
            .map(str::to_owned)
    }

    fn set_string(&self, schema: &str, key: &str, value: &str) -> bool {
        self.settings(schema, key)
            .is_some_and(|settings| settings.set_string(key, value).is_ok())
    }

    fn boolean(&self, schema: &str, key: &str) -> Option<bool> {
        self.settings(schema, key)?.value(key).get::<bool>()
    }

    fn set_boolean(&self, schema: &str, key: &str, value: bool) -> bool {
        self.settings(schema, key)
            .is_some_and(|settings| settings.set_boolean(key, value).is_ok())
    }
}

/// The worker on the calling thread, whose default context `main_loop`
/// runs, until the loop quits. `address` names a private bus (tests);
/// `None` is the session bus.
fn run(main_loop: &glib::MainLoop, address: Option<&str>, settings: Option<gio::Settings>) {
    let (wake, woken) = async_channel::unbounded::<()>();
    let mut publisher = LockdownBus::new();
    let staged = wake.clone();
    publisher.on_staged(move || {
        let _ = staged.try_send(());
    });
    let handler = settings.as_ref().map(|settings| {
        let changed = wake.clone();
        let handler = settings.connect_changed(None, move |_, _| {
            let _ = changed.try_send(());
        });
        // GSettings reports a key only after it was read with a handler
        // connected.
        for key in KEYS {
            if settings
                .settings_schema()
                .is_some_and(|schema| schema.has_key(key))
            {
                let _ = settings.value(key);
            }
        }
        handler
    });
    // Whether another connection holds the name (or we do): then a failed
    // claim waits for it to vanish instead of retrying.
    let held_elsewhere = Rc::new(Cell::new(false));
    let appeared = {
        let held = held_elsewhere.clone();
        move |_: gio::DBusConnection, _: &str, _: &str| held.set(true)
    };
    let vanished = {
        let (held, wake) = (held_elsewhere.clone(), wake.clone());
        move |_: gio::DBusConnection, _: &str| {
            held.set(false);
            let _ = wake.try_send(());
        }
    };
    let flags = gio::BusNameWatcherFlags::NONE;
    let watch = match address {
        None => Some(gio::bus_watch_name(
            gio::BusType::Session,
            LOCKDOWN_NAME,
            flags,
            appeared,
            vanished,
        )),
        Some(address) => gio::DBusConnection::for_address_sync(
            address,
            gio::DBusConnectionFlags::AUTHENTICATION_CLIENT
                | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
            None,
            None::<&gio::Cancellable>,
        )
        .ok()
        .map(|connection| {
            gio::bus_watch_name_on_connection(&connection, LOCKDOWN_NAME, flags, appeared, vanished)
        }),
    };
    let backend = Held(settings.clone());
    let address = address.map(str::to_owned);
    let context = glib::MainContext::ref_thread_default();
    let worker = context.clone();
    context.spawn_local(async move {
        loop {
            // Seed the real policy before claiming the name, including a
            // camera disabled before login. Never publish an initial
            // all-allowed snapshot before reading persisted settings.
            publisher.poll(&backend);
            let owned = match address.as_deref() {
                None => publisher.ensure(),
                Some(address) => publisher.ensure_at(address),
            };
            if publisher.has_pending() || (!owned && !held_elsewhere.get()) {
                let retry = wake.clone();
                worker.spawn_local(async move {
                    glib::timeout_future(RETRY).await;
                    let _ = retry.try_send(());
                });
            }
            if woken.recv().await.is_err() {
                return;
            }
            while woken.try_recv().is_ok() {}
        }
    });
    main_loop.run();
    if let Some(watch) = watch {
        gio::bus_unwatch_name(watch);
    }
    if let (Some(settings), Some(handler)) = (settings, handler) {
        settings.disconnect(handler);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::sync::mpsc;
    use std::time::Instant;
    use tuna_shell_host::lockdown::{LOCKDOWN_IFACE, LOCKDOWN_PATH};

    const LIMIT: Duration = Duration::from_secs(10);
    const SCHEMA_XML: &str = "<schemalist><schema id='org.gnome.desktop.privacy' \
        path='/tuna/privacy-portal-proof/'>\
        <key name='disable-camera' type='b'><default>false</default></key>\
        <key name='disable-microphone' type='b'><default>false</default></key>\
        <key name='disable-sound-output' type='b'><default>false</default></key>\
        </schema></schemalist>";

    thread_local! {
        /// The administrator's settings on the worker's backend and thread.
        static ADMIN: RefCell<Option<gio::Settings>> = const { RefCell::new(None) };
    }

    /// A private bus daemon, killed on drop.
    struct Bus(std::process::Child, String);

    impl Drop for Bus {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn private_bus() -> Bus {
        use std::io::BufRead;
        let mut child = std::process::Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("the test needs dbus-daemon");
        let mut line = String::new();
        std::io::BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        Bus(child, line.trim().to_owned())
    }

    /// Voluntary context switches of thread `tid` so far.
    fn switches(tid: i32) -> u64 {
        std::fs::read_to_string(format!("/proc/self/task/{tid}/status"))
            .unwrap()
            .lines()
            .find_map(|line| line.strip_prefix("voluntary_ctxt_switches:"))
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }

    /// How often thread `tid` blocked and woke over one second.
    fn wakes_in_a_second(tid: i32) -> u64 {
        std::thread::sleep(Duration::from_millis(200));
        let before = switches(tid);
        std::thread::sleep(Duration::from_secs(1));
        switches(tid) - before
    }

    fn call(
        client: &gio::DBusConnection,
        name: &str,
        path: &str,
        interface: &str,
        method: &str,
        args: glib::Variant,
    ) -> Result<glib::Variant, glib::Error> {
        client.call_sync(
            Some(name),
            path,
            interface,
            method,
            Some(&args),
            None,
            gio::DBusCallFlags::NONE,
            5000,
            None::<&gio::Cancellable>,
        )
    }

    fn published(client: &gio::DBusConnection, property: &str) -> bool {
        call(
            client,
            LOCKDOWN_NAME,
            LOCKDOWN_PATH,
            "org.freedesktop.DBus.Properties",
            "Get",
            (LOCKDOWN_IFACE, property).to_variant(),
        )
        .ok()
        .and_then(|reply| reply.child_value(0).as_variant())
        .and_then(|value| value.get::<bool>())
        .unwrap_or(false)
    }

    /// Run `f` on the administrator's settings, on the worker's thread.
    fn administer<T: Send + 'static>(
        context: &glib::MainContext,
        f: impl FnOnce(&gio::Settings) -> T + Send + 'static,
    ) -> T {
        let (reply, answer) = mpsc::channel();
        context.invoke(move || {
            ADMIN.with(|admin| {
                let _ = reply.send(f(admin.borrow().as_ref().expect("the worker is up")));
            })
        });
        answer.recv_timeout(LIMIT).unwrap()
    }

    fn until(what: &str, check: impl Fn() -> bool) -> Duration {
        let started = Instant::now();
        while !check() {
            assert!(started.elapsed() < LIMIT, "{what} never happened");
            std::thread::sleep(Duration::from_millis(2));
        }
        started.elapsed()
    }

    /// The worker blocks while nothing changes (the former poll woke it
    /// ten times a second), follows a settings change through `changed`
    /// and persists a bus-originated set through the staged wake, each
    /// without waiting for a timer.
    #[test]
    fn the_worker_sleeps_until_a_setting_or_a_bus_set_changes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("proof.gschema.xml"), SCHEMA_XML).unwrap();
        assert!(std::process::Command::new("glib-compile-schemas")
            .arg(dir.path())
            .status()
            .unwrap()
            .success());
        let bus = private_bus();
        let context = glib::MainContext::new();
        let main_loop = glib::MainLoop::new(Some(&context), false);
        let (tid_tx, tid_rx) = mpsc::channel();
        let worker = {
            let (context, main_loop) = (context.clone(), main_loop.clone());
            let (address, schemas) = (bus.1.clone(), dir.path().to_owned());
            std::thread::spawn(move || {
                let _ = tid_tx.send(unsafe { libc::gettid() });
                context
                    .with_thread_default(|| {
                        let source =
                            gio::SettingsSchemaSource::from_directory(&schemas, None, false)
                                .unwrap();
                        let schema = source.lookup(PRIVACY_SCHEMA, false).unwrap();
                        let backend = gio::memory_settings_backend_new();
                        let settings = gio::Settings::new_full(&schema, Some(&backend), None);
                        let admin = gio::Settings::new_full(&schema, Some(&backend), None);
                        ADMIN.with(|cell| *cell.borrow_mut() = Some(admin));
                        run(&main_loop, Some(&address), Some(settings));
                    })
                    .unwrap();
            })
        };
        let tid = tid_rx.recv().unwrap();
        let client = gio::DBusConnection::for_address_sync(
            &bus.1,
            gio::DBusConnectionFlags::AUTHENTICATION_CLIENT
                | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
            None,
            None::<&gio::Cancellable>,
        )
        .unwrap();
        until("the name claim", || {
            call(
                &client,
                "org.freedesktop.DBus",
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
                "NameHasOwner",
                (LOCKDOWN_NAME,).to_variant(),
            )
            .ok()
            .and_then(|reply| reply.child_value(0).get::<bool>())
            .unwrap_or(false)
        });
        assert!(!published(&client, "disable-camera"));

        let quiet = wakes_in_a_second(tid);
        assert!(quiet <= 1, "the idle worker woke {quiet} times in 1 s");

        // A settings change reaches the bus through `changed`.
        administer(&context, |admin| {
            admin.set_boolean(DISABLE_CAMERA_KEY, true).unwrap()
        });
        let took = until("the published camera switch", || {
            published(&client, "disable-camera")
        });
        assert!(took < RETRY * 5, "changed took {took:?}");

        // A bus-originated set is persisted through the staged wake.
        call(
            &client,
            LOCKDOWN_NAME,
            LOCKDOWN_PATH,
            "org.freedesktop.DBus.Properties",
            "Set",
            (LOCKDOWN_IFACE, "disable-microphone", true.to_variant()).to_variant(),
        )
        .unwrap();
        let took = until("the persisted microphone switch", || {
            administer(&context, |admin| admin.boolean(DISABLE_MICROPHONE_KEY))
        });
        assert!(took < RETRY * 5, "the staged set took {took:?}");

        let quiet = wakes_in_a_second(tid);
        assert!(quiet <= 1, "the idle worker woke {quiet} times in 1 s");

        context.invoke(move || main_loop.quit());
        worker.join().unwrap();
    }
}
