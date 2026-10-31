//! Portal lockdown: `org.gnome.desktop.privacy` on the portal bus.
//!
//! This is Tuna's half of the GNOME camera-denial chain. Upstream,
//! `xdg-desktop-portal-gnome` binds the privacy kill-switches to the
//! standard `org.freedesktop.impl.portal.Lockdown` properties and
//! `xdg-desktop-portal`'s Camera portal denies new sessions while
//! `disable-camera` reads true. Tuna cannot assume that backend is
//! installed, so the shell host publishes the same interface itself,
//! from the same keys, at the same path — a session whose portal
//! configuration maps `Lockdown` here gets identical denial without
//! GNOME's backend.
//!
//! The core ([`PrivacyPolicy`](crate::settings::PrivacyPolicy),
//! settings reads/writes) is wire-free. This D-Bus edge mirrors
//! [`NotificationBus`](crate::intake::NotificationBus): the panel's
//! slow tick calls [`LockdownBus::ensure`] plus
//! [`LockdownBus::poll`], and a taken name (or no bus) reads as no
//! publisher — never a hang. Property sets from the bus persist back
//! to GSettings on the next poll, so the interface stays read-write
//! like the standard XML without touching gio from zbus threads.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::settings::{
    refresh_privacy, write_disable_camera, write_disable_microphone, write_disable_sound_output,
    PrivacyPolicy, SettingsBackend,
};

/// Tuna desktop backend name for the portal `Lockdown` interface.
/// Portal configuration maps the interface here; other portal
/// interfaces stay with the configured backend (GNOME/GTK).
pub const LOCKDOWN_NAME: &str = "org.freedesktop.impl.portal.desktop.tuna";
/// Standard backend object path (`DESKTOP_DBUS_PATH` in
/// `xdg-desktop-portal`).
pub const LOCKDOWN_PATH: &str = "/org/freedesktop/portal/desktop";
/// Standard lockdown interface (all three device properties).
pub const LOCKDOWN_IFACE: &str = "org.freedesktop.impl.portal.Lockdown";

/// The `Lockdown` object: wire calls become policy mutations. The
/// live policy is shared with [`LockdownBus`], which re-syncs it
/// from GSettings and emits `PropertiesChanged`; bus-originated
/// sets land in `pending` for the next poll to persist, so zbus
/// worker threads never touch a settings backend.
#[derive(Debug)]
struct Lockdown {
    live: Arc<Mutex<PrivacyPolicy>>,
    pending: Arc<Mutex<Option<PrivacyPolicy>>>,
}

/// Standard access-dialog interface (`org.freedesktop.impl.portal.Access`).
pub const ACCESS_IFACE: &str = "org.freedesktop.impl.portal.Access";

/// The `Access` object, served beside [`Lockdown`] at the same
/// path: `xdg-desktop-portal` only exports its Camera (and Device)
/// portals when an Access backend is configured, so a backend that
/// publishes Lockdown alone would leave Camera unexported. This
/// implementation answers every permission question fail-closed
/// (cancelled): the portal records the denial and the app is
/// refused without ever prompting. A prompting UI belongs to the
/// desktop backend (GTK/GNOME) in production; this edge only keeps
/// minimal sessions safe.
#[derive(Debug)]
struct Access;

#[zbus::interface(name = "org.freedesktop.impl.portal.Access")]
impl Access {
    /// Answer a deny/grant permission question fail-closed: 1
    /// (cancelled) with no results, so the requester is refused.
    #[allow(clippy::too_many_arguments)]
    fn access_dialog(
        &self,
        _handle: zbus::zvariant::OwnedObjectPath,
        _app_id: String,
        _parent_window: String,
        _title: String,
        _subtitle: String,
        _body: String,
        _options: HashMap<String, zbus::zvariant::OwnedValue>,
    ) -> (u32, HashMap<String, zbus::zvariant::OwnedValue>) {
        (1, HashMap::new())
    }
}

#[zbus::interface(name = "org.freedesktop.impl.portal.Lockdown")]
impl Lockdown {
    /// Camera kill-switch. While true, the portal Camera interface
    /// denies new sessions (`NOT_ALLOWED`, "Camera access disabled").
    #[zbus(property, name = "disable-camera")]
    fn disable_camera(&self) -> bool {
        self.live
            .lock()
            .map(|policy| policy.camera_disabled)
            .unwrap_or(false)
    }

    /// Persist a bus-originated camera switch on the next poll.
    #[zbus(property, name = "disable-camera")]
    fn set_disable_camera(&mut self, disabled: bool) {
        self.stage(|policy| policy.camera_disabled = disabled);
    }

    /// Microphone kill-switch. No portal consumes it today; Tuna's
    /// PipeWire enforcer mutes sources while it reads true.
    #[zbus(property, name = "disable-microphone")]
    fn disable_microphone(&self) -> bool {
        self.live
            .lock()
            .map(|policy| policy.microphone_disabled)
            .unwrap_or(false)
    }

    /// Persist a bus-originated microphone switch on the next poll.
    #[zbus(property, name = "disable-microphone")]
    fn set_disable_microphone(&mut self, disabled: bool) {
        self.stage(|policy| policy.microphone_disabled = disabled);
    }

    /// Sound-output kill-switch. No portal consumes it today;
    /// Tuna's PipeWire enforcer mutes sinks while it reads true.
    #[zbus(property, name = "disable-sound-output")]
    fn disable_sound_output(&self) -> bool {
        self.live
            .lock()
            .map(|policy| policy.sound_output_disabled)
            .unwrap_or(false)
    }

    /// Persist a bus-originated sound-output switch on the next poll.
    #[zbus(property, name = "disable-sound-output")]
    fn set_disable_sound_output(&mut self, disabled: bool) {
        self.stage(|policy| policy.sound_output_disabled = disabled);
    }
}

impl Lockdown {
    /// Apply `change` to the live policy immediately (so a
    /// back-to-back read sees the set) and stage the result for
    /// GSettings persistence. A poisoned lock reads as no change.
    fn stage(&self, change: impl FnOnce(&mut PrivacyPolicy)) {
        let Ok(mut live) = self.live.lock() else {
            return;
        };
        change(&mut live);
        if let Ok(mut pending) = self.pending.lock() {
            *pending = Some(*live);
        }
    }
}

/// D-Bus edge behind the privacy policy: owns the Tuna backend name
/// when it is free. The served object and the panel share one
/// policy, so settings flips show up on the portal bus without a
/// copy, on the slow tick.
pub struct LockdownBus {
    conn: Option<zbus::blocking::Connection>,
    live: Arc<Mutex<PrivacyPolicy>>,
    pending: Arc<Mutex<Option<PrivacyPolicy>>>,
}

impl LockdownBus {
    /// Disconnected edge over a default (all-allowed) policy;
    /// [`LockdownBus::ensure`] connects.
    pub fn new() -> Self {
        Self {
            conn: None,
            live: Arc::new(Mutex::new(PrivacyPolicy::default())),
            pending: Arc::new(Mutex::new(None)),
        }
    }

    /// Current live policy (what the bus object answers). A poisoned
    /// lock reads as all-allowed, like a fresh boot.
    pub fn policy(&self) -> PrivacyPolicy {
        self.live.lock().map(|policy| *policy).unwrap_or_default()
    }

    /// Connect the session bus, serve the lockdown object, and take
    /// the Tuna backend name without queueing. Returns true while we
    /// own the role. A taken name (or no bus) reads as no publisher:
    /// portal denial stays with whatever backend the portal
    /// configuration points at, and nothing else changes.
    pub fn ensure(&mut self) -> bool {
        self.connect(None)
    }

    /// Serve over an explicit bus address (`None` means the session
    /// bus). Tests point this at a private daemon; production passes
    /// `None` through [`LockdownBus::ensure`].
    fn connect(&mut self, address: Option<&str>) -> bool {
        if self.conn.is_some() {
            return true;
        }
        let builder = match address {
            Some(address) => zbus::blocking::connection::Builder::address(address),
            None => zbus::blocking::connection::Builder::session(),
        };
        let builder = match builder {
            Ok(builder) => builder,
            Err(e) => {
                eprintln!("tuna-shell-host: lockdown bus unavailable: {e}");
                return false;
            }
        };
        let lockdown = Lockdown {
            live: self.live.clone(),
            pending: self.pending.clone(),
        };
        let conn = match builder
            .serve_at(LOCKDOWN_PATH, lockdown)
            .and_then(|builder| builder.serve_at(LOCKDOWN_PATH, Access))
        {
            Ok(conn) => conn,
            Err(e) => {
                eprintln!("tuna-shell-host: lockdown serve failed: {e}");
                return false;
            }
        };
        let conn = match conn.build() {
            Ok(conn) => conn,
            Err(e) => {
                eprintln!("tuna-shell-host: lockdown connect failed: {e}");
                return false;
            }
        };
        let reply = conn.request_name_with_flags(
            LOCKDOWN_NAME,
            zbus::fdo::RequestNameFlags::DoNotQueue.into(),
        );
        match reply {
            Ok(zbus::fdo::RequestNameReply::PrimaryOwner) => {
                self.conn = Some(conn);
                true
            }
            Ok(other) => {
                eprintln!("tuna-shell-host: lockdown name taken ({other:?}), publisher off");
                false
            }
            Err(e) => {
                eprintln!("tuna-shell-host: lockdown name request failed: {e}");
                false
            }
        }
    }

    /// One poll: persist bus-originated sets, re-sync the live policy
    /// from `backend`, and broadcast `PropertiesChanged` for whatever
    /// moved. Silent no-op without a connection; a refused write
    /// keeps the staged value for the next poll.
    pub fn poll(&mut self, backend: &dyn SettingsBackend) {
        self.flush_pending(backend);
        let before = self.policy();
        let mut synced = before;
        refresh_privacy(&mut synced, backend);
        if synced == before {
            return;
        }
        if let Ok(mut live) = self.live.lock() {
            *live = synced;
        }
        self.emit_changed(&before, &synced);
    }

    /// Persist a staged bus-originated policy to the backend. Each
    /// key writes independently; only a fully persisted stage
    /// clears, so a refused key is retried on the next poll.
    fn flush_pending(&mut self, backend: &dyn SettingsBackend) {
        let staged = self.pending.lock().map(|mut pending| pending.take());
        let Ok(Some(staged)) = staged else {
            return;
        };
        let ok = write_disable_camera(backend, staged.camera_disabled)
            & write_disable_microphone(backend, staged.microphone_disabled)
            & write_disable_sound_output(backend, staged.sound_output_disabled);
        if !ok {
            *self
                .pending
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(staged);
        }
    }

    /// Broadcast `PropertiesChanged` for the keys that moved between
    /// two policies. Emission never fails the mutation it reports: a
    /// dead bus logs and the live policy still stands.
    fn emit_changed(&self, before: &PrivacyPolicy, after: &PrivacyPolicy) {
        let Some(conn) = self.conn.as_ref() else {
            return;
        };
        let mut changed = HashMap::new();
        if before.camera_disabled != after.camera_disabled {
            changed.insert(
                "disable-camera".to_owned(),
                zbus::zvariant::OwnedValue::from(after.camera_disabled),
            );
        }
        if before.microphone_disabled != after.microphone_disabled {
            changed.insert(
                "disable-microphone".to_owned(),
                zbus::zvariant::OwnedValue::from(after.microphone_disabled),
            );
        }
        if before.sound_output_disabled != after.sound_output_disabled {
            changed.insert(
                "disable-sound-output".to_owned(),
                zbus::zvariant::OwnedValue::from(after.sound_output_disabled),
            );
        }
        if changed.is_empty() {
            return;
        }
        if let Err(e) = conn.emit_signal(
            None::<&str>,
            LOCKDOWN_PATH,
            "org.freedesktop.DBus.Properties",
            "PropertiesChanged",
            &(LOCKDOWN_IFACE, changed, Vec::<String>::new()),
        ) {
            eprintln!("tuna-shell-host: lockdown signal PropertiesChanged failed: {e}");
        }
    }
}

impl Default for LockdownBus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::MapBackend;

    #[test]
    fn poll_syncs_all_three_switches_and_keeps_absent() {
        let backend = MapBackend::with_values(&[
            (crate::settings::PRIVACY_SCHEMA, "disable-camera", "true"),
            (
                crate::settings::PRIVACY_SCHEMA,
                "disable-microphone",
                "true",
            ),
        ]);
        let mut bus = LockdownBus::new();
        assert_eq!(bus.policy(), PrivacyPolicy::default());
        bus.poll(&backend);
        assert_eq!(
            bus.policy(),
            PrivacyPolicy {
                camera_disabled: true,
                microphone_disabled: true,
                sound_output_disabled: false,
            }
        );
        // Absent keys keep their synced values, like a restart.
        bus.poll(&MapBackend::default());
        assert!(bus.policy().camera_disabled);
    }

    #[test]
    fn staged_bus_sets_persist_and_refused_keys_retry() {
        let backend = MapBackend::default();
        let mut bus = LockdownBus::new();
        *bus.pending.lock().expect("pending lock") = Some(PrivacyPolicy {
            camera_disabled: true,
            microphone_disabled: false,
            sound_output_disabled: true,
        });
        bus.poll(&backend);
        assert!(bus.pending.lock().expect("pending lock").is_none());
        assert_eq!(
            backend.boolean(crate::settings::PRIVACY_SCHEMA, "disable-camera"),
            Some(true)
        );
        assert_eq!(
            backend.boolean(crate::settings::PRIVACY_SCHEMA, "disable-sound-output"),
            Some(true)
        );

        // A refusing backend keeps the stage for the next poll.
        struct Refusing;
        impl SettingsBackend for Refusing {
            fn string(&self, _schema: &str, _key: &str) -> Option<String> {
                None
            }
            fn set_string(&self, _schema: &str, _key: &str, _value: &str) -> bool {
                false
            }
        }
        *bus.pending.lock().expect("pending lock") = Some(PrivacyPolicy {
            camera_disabled: false,
            microphone_disabled: true,
            sound_output_disabled: false,
        });
        bus.poll(&Refusing);
        assert!(bus.pending.lock().expect("pending lock").is_some());
    }

    /// Live stubs over a private session bus: a stub portal speaks
    /// the standard `Properties` shape to the real [`LockdownBus`]
    /// edge and listens for `PropertiesChanged`. Each test owns its
    /// daemon and address, so no session-bus name is touched and
    /// parallel tests never share a bus. Skips (loudly) when no bus
    /// daemon is available.
    mod live_bus {
        use std::time::Duration;

        use super::super::{
            LockdownBus, ACCESS_IFACE, LOCKDOWN_IFACE, LOCKDOWN_NAME, LOCKDOWN_PATH,
        };
        use crate::settings::{MapBackend, PrivacyPolicy, SettingsBackend, PRIVACY_SCHEMA};

        /// Private bus daemon plus its address, or `None` when no
        /// daemon is usable (the caller skips the test).
        fn private_bus(home: Option<&std::path::Path>) -> Option<(String, std::process::Child)> {
            let mut command = std::process::Command::new("dbus-daemon");
            if let Some(home) = home {
                use std::os::unix::fs::PermissionsExt;
                let run = home.join("run");
                std::fs::create_dir_all(&run).expect("private bus runtime dir");
                std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o700))
                    .expect("private bus runtime dir mode");
                // Activated services inherit the daemon's environment, not the
                // later portal child's. Keep permission-store writes private too.
                command
                    .env("HOME", home)
                    .env("XDG_DATA_HOME", home)
                    .env("XDG_CONFIG_HOME", home.join("config"))
                    .env("XDG_CACHE_HOME", home.join("cache"))
                    .env("XDG_STATE_HOME", home.join("state"))
                    .env("XDG_RUNTIME_DIR", home.join("run"));
            }
            let mut child = command
                .args(["--session", "--nofork", "--print-address=1"])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
                .ok()?;
            let stdout = child.stdout.take()?;
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let mut reader = std::io::BufReader::new(stdout);
                use std::io::BufRead;
                let mut line = String::new();
                let line = reader.read_line(&mut line).ok().map(|_| line);
                let _ = tx.send(line);
            });
            match rx.recv_timeout(Duration::from_secs(10)) {
                Ok(Some(line)) if !line.trim().is_empty() => Some((line.trim().to_owned(), child)),
                _ => {
                    let _ = child.kill();
                    None
                }
            }
        }

        /// Connect a client to the private bus, retrying while the
        /// daemon starts listening.
        fn client_conn(address: &str) -> Option<zbus::blocking::Connection> {
            for _ in 0..50 {
                match zbus::blocking::connection::Builder::address(address) {
                    Ok(builder) => match builder.build() {
                        Ok(conn) => return Some(conn),
                        Err(_) => std::thread::sleep(Duration::from_millis(100)),
                    },
                    Err(_) => std::thread::sleep(Duration::from_millis(100)),
                }
            }
            None
        }

        /// Take the Tuna backend role on the private bus, retrying
        /// while the daemon starts listening.
        fn owned_bus(address: &str) -> Option<LockdownBus> {
            let mut bus = LockdownBus::new();
            for _ in 0..50 {
                if bus.connect(Some(address)) {
                    return Some(bus);
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            None
        }

        fn properties_proxy(
            client: &zbus::blocking::Connection,
        ) -> Option<zbus::blocking::Proxy<'_>> {
            zbus::blocking::Proxy::new(
                client,
                LOCKDOWN_NAME,
                LOCKDOWN_PATH,
                "org.freedesktop.DBus.Properties",
            )
            .ok()
        }

        fn get_bool(proxy: &zbus::blocking::Proxy<'_>, property: &str) -> bool {
            proxy
                .call::<&str, _, zbus::zvariant::OwnedValue>("Get", &(LOCKDOWN_IFACE, property))
                .expect("stub Get reaches lockdown")
                .try_into()
                .expect("lockdown property is a boolean")
        }

        /// Subscribe to `PropertiesChanged` before triggering it, so
        /// the emission cannot race the subscription.
        fn subscribe_changed(
            client: &zbus::blocking::Connection,
        ) -> zbus::blocking::MessageIterator {
            let rule = zbus::MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .interface("org.freedesktop.DBus.Properties")
                .expect("signal rule")
                .member("PropertiesChanged")
                .expect("signal rule")
                .build();
            zbus::blocking::MessageIterator::for_match_rule(rule, client, Some(8))
                .expect("signal subscription")
        }

        #[test]
        fn stub_portal_sees_hyphenated_properties_and_change_signals() {
            let Some((address, mut daemon)) = private_bus(None) else {
                eprintln!("SKIP live lockdown test: no usable dbus-daemon on PATH");
                return;
            };
            let Some(mut bus) = owned_bus(&address) else {
                let _ = daemon.kill();
                eprintln!("SKIP live lockdown test: private bus refused connections");
                return;
            };
            let Some(client) = client_conn(&address) else {
                let _ = daemon.kill();
                eprintln!("SKIP live lockdown test: private bus refused connections");
                return;
            };
            let Some(proxy) = properties_proxy(&client) else {
                let _ = daemon.kill();
                panic!("lockdown proxy builds on a private bus");
            };

            // The wire names are the hyphenated standard ones — a
            // PascalCase publish would fail these Gets.
            assert!(!get_bool(&proxy, "disable-camera"));
            assert!(!get_bool(&proxy, "disable-microphone"));
            assert!(!get_bool(&proxy, "disable-sound-output"));

            let introspect: String = zbus::blocking::Proxy::new(
                &client,
                LOCKDOWN_NAME,
                LOCKDOWN_PATH,
                "org.freedesktop.DBus.Introspectable",
            )
            .expect("introspect proxy")
            .call::<&str, _, String>("Introspect", &())
            .expect("stub Introspect reaches lockdown");
            for property in [
                "disable-camera",
                "disable-microphone",
                "disable-sound-output",
            ] {
                assert!(
                    introspect.contains(&format!("name=\"{property}\"")),
                    "introspection publishes {property}"
                );
            }
            assert!(
                introspect.contains(&format!("name=\"{ACCESS_IFACE}\"")),
                "introspection publishes the Access interface"
            );
            assert!(
                introspect.contains("name=\"AccessDialog\""),
                "introspection publishes AccessDialog"
            );

            // The access dialog answers fail-closed, promptly.
            let access =
                zbus::blocking::Proxy::new(&client, LOCKDOWN_NAME, LOCKDOWN_PATH, ACCESS_IFACE)
                    .expect("access proxy");
            let options = std::collections::HashMap::<String, zbus::zvariant::OwnedValue>::new();
            let (response, results): (
                u32,
                std::collections::HashMap<String, zbus::zvariant::OwnedValue>,
            ) = access
                .call::<&str, _, (
                    u32,
                    std::collections::HashMap<String, zbus::zvariant::OwnedValue>,
                )>(
                    "AccessDialog",
                    &(
                        zbus::zvariant::ObjectPath::from_str_unchecked("/org/test/request"),
                        "com.example.app",
                        "",
                        "Title",
                        "Subtitle",
                        "Body",
                        options,
                    ),
                )
                .expect("stub AccessDialog reaches the backend");
            assert_eq!(response, 1, "access dialog denies fail-closed");
            assert!(results.is_empty());

            // Settings flip -> poll -> portal-visible change signal.
            let mut changed = subscribe_changed(&client);
            let backend = MapBackend::with_values(&[
                (PRIVACY_SCHEMA, "disable-camera", "true"),
                (PRIVACY_SCHEMA, "disable-sound-output", "true"),
            ]);
            bus.poll(&backend);
            assert_eq!(
                bus.policy(),
                PrivacyPolicy {
                    camera_disabled: true,
                    microphone_disabled: false,
                    sound_output_disabled: true,
                }
            );
            assert!(get_bool(&proxy, "disable-camera"));
            assert!(!get_bool(&proxy, "disable-microphone"));
            let msg = changed
                .next()
                .expect("change signal arrives")
                .expect("change signal decodes");
            let (iface, values, _invalid): (
                String,
                std::collections::HashMap<String, zbus::zvariant::OwnedValue>,
                Vec<String>,
            ) = msg.body().deserialize().expect("change signal body");
            assert_eq!(iface, LOCKDOWN_IFACE);
            assert!(
                bool::try_from(values["disable-camera"].clone()).expect("bool value"),
                "change signal carries the camera flip"
            );
            assert!(
                bool::try_from(values["disable-sound-output"].clone()).expect("bool value"),
                "change signal carries the sound-output flip"
            );
            assert!(
                !values.contains_key("disable-microphone"),
                "unchanged keys stay out of the signal"
            );

            // A steady poll emits nothing further.
            bus.poll(&backend);
            assert!(bus.policy().camera_disabled);

            let _ = daemon.kill();
            let _ = bus;
        }

        #[test]
        fn bus_originated_set_persists_to_settings_on_poll() {
            let Some((address, mut daemon)) = private_bus(None) else {
                eprintln!("SKIP live lockdown test: no usable dbus-daemon on PATH");
                return;
            };
            let Some(mut bus) = owned_bus(&address) else {
                let _ = daemon.kill();
                eprintln!("SKIP live lockdown test: private bus refused connections");
                return;
            };
            let Some(client) = client_conn(&address) else {
                let _ = daemon.kill();
                eprintln!("SKIP live lockdown test: private bus refused connections");
                return;
            };
            let Some(proxy) = properties_proxy(&client) else {
                let _ = daemon.kill();
                panic!("lockdown proxy builds on a private bus");
            };

            proxy
                .call::<&str, _, ()>(
                    "Set",
                    &(
                        LOCKDOWN_IFACE,
                        "disable-microphone",
                        zbus::zvariant::Value::Bool(true),
                    ),
                )
                .expect("stub Set reaches lockdown");
            // The set answers immediately, before any poll.
            assert!(get_bool(&proxy, "disable-microphone"));

            let backend = MapBackend::default();
            bus.poll(&backend);
            assert_eq!(
                backend.boolean(PRIVACY_SCHEMA, "disable-microphone"),
                Some(true),
                "bus Set persists to the settings backend"
            );
            assert!(bus.policy().microphone_disabled);

            let _ = daemon.kill();
            let _ = bus;
        }

        #[test]
        fn private_bus_activation_keeps_permission_store_writes_in_fixture() {
            if ![
                "/usr/libexec/xdg-permission-store",
                "/usr/lib/xdg-permission-store",
            ]
            .into_iter()
            .any(|path| std::path::Path::new(path).is_file())
            {
                eprintln!("SKIP live permission-store test: no daemon installed");
                return;
            }
            let home = tempfile::tempdir().expect("permission-store home");
            let Some((address, mut daemon)) = private_bus(Some(home.path())) else {
                eprintln!("SKIP live permission-store test: no usable dbus-daemon on PATH");
                return;
            };
            let client = client_conn(&address).expect("private permission-store bus");
            let store = zbus::blocking::Proxy::new(
                &client,
                "org.freedesktop.impl.portal.PermissionStore",
                "/org/freedesktop/impl/portal/PermissionStore",
                "org.freedesktop.impl.portal.PermissionStore",
            )
            .expect("permission-store proxy");
            let result = store.call::<&str, _, ()>(
                "SetPermission",
                &(
                    "tuna-fixture-isolation",
                    true,
                    "isolation",
                    "com.example.Fixture",
                    vec!["yes"],
                ),
            );
            let _ = daemon.kill();
            let _ = daemon.wait();
            result.expect("actual activated permission store persists");
            assert!(
                home.path()
                    .join("flatpak/db/tuna-fixture-isolation")
                    .is_file(),
                "the bus-activated service must write inside the fixture data home"
            );
        }

        /// Real `xdg-desktop-portal` against the live [`LockdownBus`]
        /// edge: with the camera switch disabled, the portal's Camera
        /// interface must deny new sessions with its lockdown error;
        /// with the switch allowed (before and after), the call must
        /// get past the lockdown gate. The gate fires before any
        /// hardware or permission lookup, so no camera is needed.
        /// Skips (loudly) without the portal daemon or a bus daemon.
        #[test]
        fn real_portal_denies_camera_while_disabled_and_readmits_on_restore() {
            let portal = [
                "/usr/libexec/xdg-desktop-portal",
                "/usr/lib/xdg-desktop-portal",
            ]
            .into_iter()
            .find(|path| std::path::Path::new(path).is_file());
            let Some(portal) = portal else {
                eprintln!("SKIP live portal test: no xdg-desktop-portal daemon installed");
                return;
            };
            let home = tempfile::tempdir().expect("portal config dir");
            let Some((address, mut daemon)) = private_bus(Some(home.path())) else {
                eprintln!("SKIP live portal test: no usable dbus-daemon on PATH");
                return;
            };
            let Some(mut bus) = owned_bus(&address) else {
                let _ = daemon.kill();
                eprintln!("SKIP live portal test: private bus refused connections");
                return;
            };
            let Some(client) = client_conn(&address) else {
                let _ = daemon.kill();
                eprintln!("SKIP live portal test: private bus refused connections");
                return;
            };

            // Portal configuration only this test's desktop matches:
            // `Lockdown` resolves to the Tuna backend name we own.
            // `.portal` files load from `XDG_DESKTOP_PORTAL_DIR` (the
            // daemon's documented test override); nothing else is
            // visible, so no other backend can answer instead.
            let portals_dir = home.path().join("portals");
            std::fs::create_dir_all(&portals_dir).expect("portal config dir");
            std::fs::write(
                portals_dir.join("tuna-privacylive.portal"),
                "[portal]\nDBusName=org.freedesktop.impl.portal.desktop.tuna\n\
                 Interfaces=org.freedesktop.impl.portal.Lockdown;org.freedesktop.impl.portal.Access;\n\
                 UseIn=tunaprivacylive\n",
            )
            .expect("portal config file");
            let run = home.path().join("run");
            let log_path = home.path().join("xdp.log");
            let log_file = std::fs::File::create(&log_path).expect("portal log file");

            // Start allowed: the portal must get past our gate.
            let backend = MapBackend::default();
            bus.poll(&backend);
            assert_eq!(bus.policy(), PrivacyPolicy::default());

            let mut portal_child = std::process::Command::new(portal)
                .env("DBUS_SESSION_BUS_ADDRESS", &address)
                .env("XDG_DESKTOP_PORTAL_DIR", &portals_dir)
                .env("XDG_DATA_HOME", home.path())
                .env("XDG_CONFIG_HOME", home.path().join("config"))
                .env("XDG_CACHE_HOME", home.path().join("cache"))
                .env("XDG_STATE_HOME", home.path().join("state"))
                .env("XDG_CURRENT_DESKTOP", "tunaprivacylive")
                .env("XDG_RUNTIME_DIR", &run)
                .env("HOME", home.path())
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(log_file)
                .spawn()
                .expect("portal daemon spawns");
            let portal_up = wait_for_name(&client, "org.freedesktop.portal.Desktop");
            if !portal_up {
                let _ = portal_child.kill();
                let _ = daemon.kill();
                panic!(
                    "portal daemon never owned its name; log tail: {}",
                    log_tail(&log_path)
                );
            }

            // Allowed: the call reaches past the lockdown gate. It
            // may fail later (no permission backend, no camera) or
            // wait there, but never with the lockdown denial.
            assert!(
                !call_denied_by_lockdown(&address, Duration::from_secs(20)),
                "allowed camera policy reaches past the lockdown gate; log tail: {}",
                log_tail(&log_path)
            );

            // Disabled: deny, promptly and with the exact upstream
            // lockdown error, even though nothing else changed.
            assert!(backend.set_boolean(PRIVACY_SCHEMA, "disable-camera", true));
            bus.poll(&backend);
            assert!(bus.policy().camera_disabled);
            assert!(
                wait_for_lockdown_denial(&address, true),
                "disabled camera policy denies new portal sessions; log tail: {}",
                log_tail(&log_path)
            );

            // Explicit restore: the gate opens again.
            assert!(backend.set_boolean(PRIVACY_SCHEMA, "disable-camera", false));
            bus.poll(&backend);
            assert!(!bus.policy().camera_disabled);
            assert!(
                wait_for_lockdown_denial(&address, false),
                "restored camera policy readmits portal sessions; log tail: {}",
                log_tail(&log_path)
            );

            let _ = portal_child.kill();
            let _ = portal_child.wait();
            let _ = daemon.kill();
            let _ = bus;
        }

        /// True once `name` has an owner on the private bus (20s).
        fn wait_for_name(client: &zbus::blocking::Connection, name: &str) -> bool {
            let proxy = zbus::blocking::Proxy::new(
                client,
                "org.freedesktop.DBus",
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
            )
            .expect("bus proxy");
            for _ in 0..200 {
                let owned: bool = proxy
                    .call::<&str, _, bool>("NameHasOwner", &(name,))
                    .unwrap_or(false);
                if owned {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            false
        }

        /// One `AccessCamera` call from a fresh connection: true
        /// when the portal denies it with the lockdown error. A
        /// timeout reads as past-the-gate (the gate itself never
        /// waits — only the permission/dialog stage behind it does).
        fn call_denied_by_lockdown(address: &str, timeout: Duration) -> bool {
            let (tx, rx) = std::sync::mpsc::channel();
            let address = address.to_owned();
            std::thread::spawn(move || {
                let denied = (|| {
                    let conn = client_conn(&address)?;
                    let proxy = zbus::blocking::Proxy::new(
                        &conn,
                        "org.freedesktop.portal.Desktop",
                        "/org/freedesktop/portal/desktop",
                        "org.freedesktop.portal.Camera",
                    )
                    .ok()?;
                    let options =
                        std::collections::HashMap::<String, zbus::zvariant::OwnedValue>::new();
                    match proxy.call::<&str, _, (u32, zbus::zvariant::OwnedObjectPath)>(
                        "AccessCamera",
                        &(options,),
                    ) {
                        Ok(_) => Some(false),
                        Err(e) => Some(format!("{e:?}").contains("Camera access disabled")),
                    }
                })();
                let _ = tx.send(denied);
            });
            // A dropped call (no connection, no proxy) is a harness
            // failure, not a denial: fail the test loudly.
            match rx.recv_timeout(timeout) {
                Ok(Some(denied)) => denied,
                Ok(None) => panic!("portal test call could not reach the Camera interface"),
                Err(_) => false,
            }
        }

        /// Poll `AccessCamera` until the lockdown verdict matches
        /// `denied` (the portal caches our properties until its
        /// `PropertiesChanged` lands). Rounds are paced so the
        /// portal's loop can dispatch the signal between calls;
        /// instant rounds would all sample the stale cache.
        fn wait_for_lockdown_denial(address: &str, denied: bool) -> bool {
            for round in 0..20 {
                if round > 0 {
                    std::thread::sleep(Duration::from_millis(500));
                }
                if call_denied_by_lockdown(address, Duration::from_secs(5)) == denied {
                    return true;
                }
            }
            false
        }

        /// Last bytes of the portal log for failure context.
        fn log_tail(path: &std::path::Path) -> String {
            let text = std::fs::read_to_string(path).unwrap_or_default();
            let mut start = text.len().saturating_sub(2048);
            while !text.is_char_boundary(start) {
                start += 1;
            }
            text[start..].to_owned()
        }

        #[test]
        fn second_publisher_loses_the_backend_name() {
            let Some((address, mut daemon)) = private_bus(None) else {
                eprintln!("SKIP live lockdown test: no usable dbus-daemon on PATH");
                return;
            };
            let Some(first) = owned_bus(&address) else {
                let _ = daemon.kill();
                eprintln!("SKIP live lockdown test: private bus refused connections");
                return;
            };
            let mut second = LockdownBus::new();
            assert!(
                !second.connect(Some(&address)),
                "a taken backend name reads as no publisher"
            );

            let _ = daemon.kill();
            let _ = first;
        }
    }
}
