//! Owned GIO reader for GNOME's per-device tablet, stylus, pad-button and
//! touchscreen settings (`org.gnome.desktop.peripherals.tablet` at
//! `/org/gnome/desktop/peripherals/tablets/VVVV:PPPP/`,
//! `org.gnome.desktop.peripherals.tablet.stylus` per tool,
//! `org.gnome.desktop.peripherals.tablet.pad-button` per pad button, ring
//! and strip direction, and `org.gnome.desktop.peripherals.touchscreen`
//! at `/org/gnome/desktop/peripherals/touchscreens/VVVV:PPPP/`), read by
//! the compositor itself as Mutter's `meta-input-settings.c`,
//! `meta-input-mapper.c` and `meta-pad-action-mapper.c` read them
//! in-process.
//!
//! The mechanics are `wm_preferences.rs`'s: native `Settings` and their
//! `changed` subscriptions live and retire on one owned GLib thread; every
//! read is an idle source on that thread's context, admitted under a
//! deadline the frame consumer enforces; a timed-out, panicked or poisoned
//! worker is quarantined for good. Unlike the fixed focus keys these
//! schemas are relocatable: the consumer names the paths it needs (one per
//! connected tablet, per tool seen in proximity and per pad action) and
//! each read covers exactly those, watching each path once. A missing
//! schema leaves the reader unavailable and every device on GNOME 51's
//! defaults. The one write, the tablet's `output` (Mutter's
//! `cycle_tablet_output`), runs on the same thread.
use crate::night_light::ReadError;
use crate::night_light_settings::CacheStatus;
use crate::tablet::{
    ButtonAction, EraserButtonMode, Mapping, StylusSettings, TabletSettings, STYLUS_SCHEMA,
    TABLET_SCHEMA,
};
use crate::tablet_actions::{PadButtonAction, PadButtonSettings, PAD_BUTTON_SCHEMA};
use crate::wake::Wake;
use gio::{glib, prelude::*};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

const OPERATION_LIMIT: Duration = Duration::from_secs(2);
/// Bounded re-read, as the focus policy's: a failed read retries and a
/// lost `changed` signal heals.
pub(crate) const READ_INTERVAL: Duration = Duration::from_secs(5);

const TABLET_KEYS: [(&str, &str); 5] = [
    ("output", "as"),
    // An enum key's value type is its string nick.
    ("mapping", "s"),
    ("area", "ad"),
    ("keep-aspect", "b"),
    ("left-handed", "b"),
];
const STYLUS_KEYS: [(&str, &str); 13] = [
    ("pressure-curve", "ai"),
    ("pressure-range", "ai"),
    ("eraser-pressure-curve", "ai"),
    ("eraser-pressure-range", "ai"),
    ("eraser-button-mode", "s"),
    ("eraser-button-action", "s"),
    ("eraser-button-keybinding", "s"),
    ("button-action", "s"),
    ("secondary-button-action", "s"),
    ("tertiary-button-action", "s"),
    ("button-keybinding", "s"),
    ("secondary-button-keybinding", "s"),
    ("tertiary-button-keybinding", "s"),
];
const PAD_BUTTON_KEYS: [(&str, &str); 2] = [("action", "s"), ("keybinding", "s")];
/// `org.gnome.desktop.peripherals.touchscreen` (GNOME 51).
pub(crate) const TOUCHSCREEN_SCHEMA: &str = "org.gnome.desktop.peripherals.touchscreen";
const TOUCHSCREEN_KEYS: [(&str, &str); 1] = [("output", "as")];

/// Which relocatable schema a path holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Kind {
    Tablet,
    Stylus,
    PadButton,
    Touchscreen,
}

/// One completed read: every requested path's values.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Snapshot {
    pub tablets: BTreeMap<String, TabletSettings>,
    pub styluses: BTreeMap<String, StylusSettings>,
    pub pad_buttons: BTreeMap<String, PadButtonSettings>,
    /// Each touchscreen path's `output` user value (`None`: automatic).
    pub touchscreens: BTreeMap<String, Option<Vec<String>>>,
}

#[derive(Clone, Copy)]
struct Operation {
    id: u64,
    deadline: Instant,
}
struct State {
    next: u64,
    active: Option<Operation>,
    dirty: bool,
    stopped: bool,
    status: CacheStatus,
    last: Option<Instant>,
    completion: Option<Result<Snapshot, ReadError>>,
    /// The paths the consumer needs, read by the worker at each read.
    wanted: BTreeSet<(Kind, String)>,
}
struct Shared {
    state: Mutex<State>,
    context: glib::MainContext,
    wake: Wake,
    operation_limit: Duration,
}
trait NativeReader {
    fn read(&mut self, wanted: &BTreeSet<(Kind, String)>) -> Result<Snapshot, ReadError>;
    /// `g_settings_set_strv (info->settings, "output", edid)`.
    fn write_output(&mut self, path: &str, output: &[String]) -> Result<(), ReadError>;
    /// What GNOME Settings does to one tablet and its stylus.
    #[cfg(test)]
    fn change(&self) {
        panic!("not a native watch fixture");
    }
}
type Factory = Box<dyn FnOnce(Weak<Shared>) -> Result<Box<dyn NativeReader>, ReadError> + Send>;
struct Local {
    creator: std::thread::ThreadId,
    factory: Option<Factory>,
    native: Option<Box<dyn NativeReader>>,
    loop_: glib::MainLoop,
}
thread_local! { static LOCAL: RefCell<Option<Local>> = const { RefCell::new(None) }; }

pub(crate) struct Reader {
    shared: Arc<Shared>,
    owner: Option<std::thread::JoinHandle<()>>,
}
impl Reader {
    pub(crate) fn new(wake: &Wake) -> Self {
        Self::with_factory(
            wake,
            Box::new(|shared| {
                GioReader::new(shared).map(|reader| Box::new(reader) as Box<dyn NativeReader>)
            }),
            OPERATION_LIMIT,
        )
    }
    fn with_factory(wake: &Wake, factory: Factory, operation_limit: Duration) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                next: 0,
                active: None,
                dirty: false,
                stopped: false,
                status: CacheStatus::Unavailable,
                last: None,
                completion: None,
                wanted: BTreeSet::new(),
            }),
            context: glib::MainContext::new(),
            wake: wake.clone(),
            operation_limit,
        });
        let worker = shared.clone();
        let owner = std::thread::Builder::new()
            .name("tuna-tablet-settings".into())
            .spawn(move || {
                let context = worker.context.clone();
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    context.with_thread_default(|| {
                        let loop_ = glib::MainLoop::new(Some(&context), false);
                        LOCAL.with(|local| {
                            *local.borrow_mut() = Some(Local {
                                creator: std::thread::current().id(),
                                factory: Some(factory),
                                native: None,
                                loop_: loop_.clone(),
                            })
                        });
                        loop_.run();
                        // Native signal handlers and objects retire on their creator.
                        LOCAL.with(|local| *local.borrow_mut() = None);
                    })
                }));
                if !matches!(result, Ok(Ok(()))) {
                    worker.quarantine();
                }
                worker.wake.signal();
            })
            .ok();
        let reader = Self { shared, owner };
        if reader.owner.is_none() {
            reader.shared.quarantine();
        }
        reader
    }
    /// Name the paths to read; a change reads at once.
    pub(crate) fn want(&self, wanted: BTreeSet<(Kind, String)>, now: Instant) {
        let changed = {
            let Ok(mut state) = self.shared.state.lock() else {
                return;
            };
            if state.wanted == wanted {
                false
            } else {
                state.wanted = wanted;
                true
            }
        };
        if changed {
            self.shared.request(now);
        }
    }
    /// Write a tablet's `output` (switch-monitor). Runs on the worker; the
    /// change notification then re-reads, so the mapping follows like any
    /// other settings change. Best effort: a failed write is logged.
    pub(crate) fn set_output(&self, path: String, output: [String; 3]) {
        let source = glib::idle_source_new(
            Some("tuna-tablet-settings-write"),
            glib::Priority::DEFAULT,
            move || {
                let result = LOCAL.with(|local| {
                    let mut local = local.borrow_mut();
                    let local = local
                        .as_mut()
                        .ok_or_else(|| ReadError("worker unavailable".into()))?;
                    local
                        .native
                        .as_mut()
                        .ok_or_else(|| ReadError("settings never read".into()))?
                        .write_output(&path, &output)
                });
                if let Err(error) = result {
                    eprintln!(
                        "tuna-compositor: tablet: could not write {path} output: {}",
                        error.0
                    );
                }
                glib::ControlFlow::Break
            },
        );
        source.attach(Some(&self.shared.context));
        self.shared.wake.signal();
    }
    /// The frame consumer never invokes GIO, waits for a worker, or joins
    /// it. `Some` only for a completed, valid read.
    pub(crate) fn poll(&self, now: Instant) -> Option<Snapshot> {
        self.shared.expire(now);
        let (completion, request) = {
            let Ok(mut state) = self.shared.state.lock() else {
                return None;
            };
            let request = !state.stopped
                && !state.wanted.is_empty()
                && state.status != CacheStatus::Quarantined
                && state.active.is_none()
                && state
                    .last
                    .is_none_or(|last| now.saturating_duration_since(last) >= READ_INTERVAL);
            (state.completion.take(), request)
        };
        if request {
            self.shared.request(now);
        }
        completion?.ok()
    }
    pub(crate) fn next_deadline(&self, now: Instant) -> Option<Instant> {
        let state = self.shared.state.lock().ok()?;
        if state.stopped || state.status == CacheStatus::Quarantined || state.wanted.is_empty() {
            return None;
        }
        Some(
            state
                .active
                .map(|op| op.deadline)
                .or_else(|| state.last.and_then(|last| last.checked_add(READ_INTERVAL)))
                .unwrap_or(now)
                .max(now),
        )
    }
    pub(crate) fn status(&self) -> CacheStatus {
        self.shared
            .state
            .lock()
            .map(|s| s.status)
            .unwrap_or(CacheStatus::Quarantined)
    }
}
impl Drop for Reader {
    fn drop(&mut self) {
        // Drop detaches the one owner; no frame-thread join.
        self.shared.quarantine();
    }
}

impl Shared {
    fn request(self: &Arc<Self>, now: Instant) {
        let operation = {
            let Ok(mut state) = self.state.lock() else {
                return;
            };
            if state.stopped || state.status == CacheStatus::Quarantined {
                return;
            }
            if state.active.is_some() {
                state.dirty = true;
                return;
            }
            let Some(id) = state.next.checked_add(1) else {
                drop(state);
                self.quarantine();
                return;
            };
            let Some(deadline) = now.checked_add(self.operation_limit) else {
                return;
            };
            state.next = id;
            let op = Operation { id, deadline };
            state.active = Some(op);
            state.last = Some(now);
            if state.status == CacheStatus::Fresh {
                state.status = CacheStatus::Stale;
            }
            op
        };
        let weak = Arc::downgrade(self);
        // An attached source runs ONLY through this worker's owned context.
        let source = glib::idle_source_new(
            Some("tuna-tablet-settings-read"),
            glib::Priority::DEFAULT,
            move || {
                if let Some(shared) = weak.upgrade() {
                    shared.run(operation);
                }
                glib::ControlFlow::Break
            },
        );
        source.attach(Some(&self.context));
        self.wake.signal();
    }
    fn run(self: &Arc<Self>, op: Operation) {
        if !self.current(op, Instant::now()) {
            return;
        }
        let Ok(wanted) = self.state.lock().map(|state| state.wanted.clone()) else {
            self.quarantine();
            return;
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            LOCAL.with(|local| {
                let mut local = local.borrow_mut();
                let local = local
                    .as_mut()
                    .ok_or_else(|| ReadError("worker unavailable".into()))?;
                if local.creator != std::thread::current().id() {
                    return Err(ReadError("wrong native owner".into()));
                }
                if local.native.is_none() {
                    let factory = local
                        .factory
                        .take()
                        .ok_or_else(|| ReadError("schema unavailable".into()))?;
                    local.native = Some(factory(Arc::downgrade(self))?);
                }
                local.native.as_mut().unwrap().read(&wanted)
            })
        }));
        let Ok(result) = result else {
            self.quarantine();
            return;
        };
        let again = {
            let Ok(mut state) = self.state.lock() else {
                return;
            };
            if state.active.map(|active| active.id) != Some(op.id) || state.stopped {
                return;
            }
            let consumed = Instant::now();
            if consumed >= op.deadline {
                drop(state);
                self.expire(consumed);
                return;
            }
            state.active = None;
            state.status = if result.is_ok() {
                CacheStatus::Fresh
            } else {
                CacheStatus::Unavailable
            };
            // The wanted set changed during the read: read again.
            let stale = state.wanted != wanted;
            state.completion = Some(result);
            std::mem::take(&mut state.dirty) || stale
        };
        self.wake.signal();
        if again {
            self.request(Instant::now());
        }
    }
    fn current(&self, op: Operation, now: Instant) -> bool {
        self.state.lock().is_ok_and(|state| {
            !state.stopped
                && state.status != CacheStatus::Quarantined
                && now < op.deadline
                && state.active.map(|active| active.id) == Some(op.id)
        })
    }
    fn expire(&self, now: Instant) {
        if self.state.is_poisoned() {
            self.quarantine();
            return;
        }
        let expired = self
            .state
            .lock()
            .is_ok_and(|state| state.active.is_some_and(|op| now >= op.deadline));
        if expired {
            self.quarantine();
        }
    }
    fn quarantine(&self) {
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            if state.stopped {
                return;
            }
            state.stopped = true;
            state.status = CacheStatus::Quarantined;
            state.active = None;
            state.dirty = false;
            state.completion = None;
        }
        let source = glib::idle_source_new(
            Some("tuna-tablet-settings-stop"),
            glib::Priority::HIGH,
            || {
                LOCAL.with(|local| {
                    if let Some(local) = local.borrow().as_ref() {
                        local.loop_.quit();
                    }
                });
                glib::ControlFlow::Break
            },
        );
        source.attach(Some(&self.context));
        self.wake.signal();
    }
}

fn schema_matches(schema: &gio::SettingsSchema, keys: &[(&str, &str)]) -> bool {
    keys.iter().all(|(key, signature)| {
        schema.has_key(key) && schema.key(key).value_type().as_str() == *signature
    })
}

struct GioReader {
    tablet: gio::SettingsSchema,
    stylus: gio::SettingsSchema,
    pad_button: gio::SettingsSchema,
    touchscreen: gio::SettingsSchema,
    backend: Option<gio::SettingsBackend>,
    shared: Weak<Shared>,
    /// One live settings object and its watch per path read so far.
    settings: HashMap<(Kind, String), (gio::Settings, glib::SignalHandlerId)>,
}
impl GioReader {
    fn new(shared: Weak<Shared>) -> Result<Self, ReadError> {
        let source = gio::SettingsSchemaSource::default()
            .ok_or_else(|| ReadError("schema source unavailable".into()))?;
        let lookup = |id: &str| {
            source
                .lookup(id, true)
                .ok_or_else(|| ReadError(format!("{id} schema unavailable")))
        };
        Self::from_schemas(
            lookup(TABLET_SCHEMA)?,
            lookup(STYLUS_SCHEMA)?,
            lookup(PAD_BUTTON_SCHEMA)?,
            lookup(TOUCHSCREEN_SCHEMA)?,
            None,
            shared,
        )
    }
    fn from_schemas(
        tablet: gio::SettingsSchema,
        stylus: gio::SettingsSchema,
        pad_button: gio::SettingsSchema,
        touchscreen: gio::SettingsSchema,
        backend: Option<gio::SettingsBackend>,
        shared: Weak<Shared>,
    ) -> Result<Self, ReadError> {
        if !schema_matches(&tablet, &TABLET_KEYS) {
            return Err(ReadError("tablet schema key/type mismatch".into()));
        }
        if !schema_matches(&stylus, &STYLUS_KEYS) {
            return Err(ReadError("stylus schema key/type mismatch".into()));
        }
        if !schema_matches(&pad_button, &PAD_BUTTON_KEYS) {
            return Err(ReadError("pad-button schema key/type mismatch".into()));
        }
        if !schema_matches(&touchscreen, &TOUCHSCREEN_KEYS) {
            return Err(ReadError("touchscreen schema key/type mismatch".into()));
        }
        Ok(Self {
            tablet,
            stylus,
            pad_button,
            touchscreen,
            backend,
            shared,
            settings: HashMap::new(),
        })
    }
    /// The settings for one path, created and watched on first use.
    fn settings(&mut self, kind: Kind, path: &str) -> gio::Settings {
        let key = (kind, path.to_owned());
        if let Some((settings, _)) = self.settings.get(&key) {
            return settings.clone();
        }
        let schema = match kind {
            Kind::Tablet => &self.tablet,
            Kind::Stylus => &self.stylus,
            Kind::PadButton => &self.pad_button,
            Kind::Touchscreen => &self.touchscreen,
        };
        let settings = gio::Settings::new_full(schema, self.backend.as_ref(), Some(path));
        let shared = self.shared.clone();
        let handler = settings.connect_changed(None, move |_, _| {
            if let Some(shared) = shared.upgrade() {
                shared.request(Instant::now());
            }
        });
        self.settings.insert(key, (settings.clone(), handler));
        settings
    }
}

fn nick<T>(
    settings: &gio::Settings,
    key: &str,
    parse: fn(&str) -> Option<T>,
) -> Result<T, ReadError> {
    settings
        .value(key)
        .get::<String>()
        .as_deref()
        .and_then(parse)
        .ok_or_else(|| ReadError(format!("invalid {key}")))
}

fn ints(settings: &gio::Settings, key: &str) -> Result<Vec<i32>, ReadError> {
    settings
        .value(key)
        .get::<Vec<i32>>()
        .ok_or_else(|| ReadError(format!("invalid {key}")))
}

impl NativeReader for GioReader {
    #[cfg(test)]
    fn change(&self) {
        for ((kind, _), (settings, _)) in &self.settings {
            match kind {
                Kind::Tablet => {
                    settings
                        .set_strv("output", ["GSM", "LG ULTRAFINE", "DP-1", "DP-1"])
                        .unwrap();
                    settings.set_string("mapping", "relative").unwrap();
                    settings.set_boolean("keep-aspect", true).unwrap();
                }
                Kind::Stylus => {
                    settings
                        .set_value("pressure-curve", &vec![0i32, 75, 25, 100].to_variant())
                        .unwrap();
                    settings.set_string("button-action", "back").unwrap();
                    settings
                        .set_string("tertiary-button-action", "keybinding")
                        .unwrap();
                    settings
                        .set_string("tertiary-button-keybinding", "<Control>z")
                        .unwrap();
                }
                Kind::PadButton => {
                    settings.set_string("action", "keybinding").unwrap();
                    settings.set_string("keybinding", "<Shift>b").unwrap();
                }
                Kind::Touchscreen => {
                    settings
                        .set_strv("output", ["BOE", "0x0a1c", "eDP-1"])
                        .unwrap();
                }
            }
        }
    }
    fn write_output(&mut self, path: &str, output: &[String]) -> Result<(), ReadError> {
        self.settings(Kind::Tablet, path)
            .set_strv("output", output)
            .map_err(|error| ReadError(error.to_string()))
    }
    fn read(&mut self, wanted: &BTreeSet<(Kind, String)>) -> Result<Snapshot, ReadError> {
        // Paths no longer wanted retire with their watches.
        let gone: Vec<_> = self
            .settings
            .keys()
            .filter(|key| !wanted.contains(*key))
            .cloned()
            .collect();
        for key in gone {
            if let Some((settings, handler)) = self.settings.remove(&key) {
                settings.disconnect(handler);
            }
        }
        let mut snapshot = Snapshot::default();
        for (kind, path) in wanted {
            // Watches connect BEFORE these reads; GSettings emits
            // `changed` only for keys read since.
            let settings = self.settings(*kind, path);
            match kind {
                Kind::Tablet => {
                    let bad = |key: &str| ReadError(format!("invalid {key}"));
                    snapshot.tablets.insert(
                        path.clone(),
                        TabletSettings {
                            // Mutter's automatic mapping: no user value.
                            output: settings
                                .user_value("output")
                                .map(|value| {
                                    value.get::<Vec<String>>().ok_or_else(|| bad("output"))
                                })
                                .transpose()?,
                            mapping: nick(&settings, "mapping", Mapping::from_nick)?,
                            area: settings
                                .value("area")
                                .get::<Vec<f64>>()
                                .ok_or_else(|| bad("area"))?,
                            keep_aspect: settings.boolean("keep-aspect"),
                            left_handed: settings.boolean("left-handed"),
                        },
                    );
                }
                Kind::Stylus => {
                    snapshot.styluses.insert(
                        path.clone(),
                        StylusSettings {
                            pressure_curve: ints(&settings, "pressure-curve")?,
                            pressure_range: ints(&settings, "pressure-range")?,
                            eraser_pressure_curve: ints(&settings, "eraser-pressure-curve")?,
                            eraser_pressure_range: ints(&settings, "eraser-pressure-range")?,
                            eraser_button_mode: nick(
                                &settings,
                                "eraser-button-mode",
                                EraserButtonMode::from_nick,
                            )?,
                            eraser_button_action: nick(
                                &settings,
                                "eraser-button-action",
                                ButtonAction::from_nick,
                            )?,
                            button_action: nick(
                                &settings,
                                "button-action",
                                ButtonAction::from_nick,
                            )?,
                            secondary_button_action: nick(
                                &settings,
                                "secondary-button-action",
                                ButtonAction::from_nick,
                            )?,
                            tertiary_button_action: nick(
                                &settings,
                                "tertiary-button-action",
                                ButtonAction::from_nick,
                            )?,
                            eraser_button_keybinding: settings
                                .string("eraser-button-keybinding")
                                .into(),
                            button_keybinding: settings.string("button-keybinding").into(),
                            secondary_button_keybinding: settings
                                .string("secondary-button-keybinding")
                                .into(),
                            tertiary_button_keybinding: settings
                                .string("tertiary-button-keybinding")
                                .into(),
                        },
                    );
                }
                Kind::PadButton => {
                    snapshot.pad_buttons.insert(
                        path.clone(),
                        PadButtonSettings {
                            action: nick(&settings, "action", PadButtonAction::from_nick)?,
                            keybinding: settings.string("keybinding").into(),
                        },
                    );
                }
                Kind::Touchscreen => {
                    // meta-input-mapper.c `guess_candidates`: automatic
                    // mapping while `output` has no user value.
                    let output = settings
                        .user_value("output")
                        .map(|value| {
                            value
                                .get::<Vec<String>>()
                                .ok_or_else(|| ReadError("invalid output".into()))
                        })
                        .transpose()?;
                    snapshot.touchscreens.insert(path.clone(), output);
                }
            }
        }
        Ok(snapshot)
    }
}
impl Drop for GioReader {
    fn drop(&mut self) {
        for (_, (settings, handler)) in self.settings.drain() {
            settings.disconnect(handler);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HARNESS_LIMIT: Duration = Duration::from_secs(10);
    const TABLET_PATH: &str = "/org/gnome/desktop/peripherals/tablets/056a:0357/";
    const OTHER_PATH: &str = "/org/gnome/desktop/peripherals/tablets/256c:006d/";
    const STYLUS_PATH: &str = "/org/gnome/desktop/peripherals/stylus/default-056a:0357/";
    const PAD_PATH: &str = "/org/gnome/desktop/peripherals/tablets/056a:0357/buttonA/";

    /// gsettings-desktop-schemas 51's tablet, stylus and pad-button
    /// schemas, as installed (relocatable, the enums from gdesktop-enums.h).
    const SCHEMAS: &str =
        include_str!("../../../tests/settings/gnome51-peripherals-tablet.gschema.xml");
    /// gsettings-desktop-schemas 51's touchscreen schema and
    /// gnome-settings-daemon 51's touchscreen orientation lock.
    const TOUCHSCREEN_SCHEMAS: &str =
        include_str!("../../../tests/settings/gnome51-peripherals-touchscreen.gschema.xml");
    const TOUCHSCREEN_PATH: &str = "/org/gnome/desktop/peripherals/touchscreens/04f3:2c82/";

    fn wake_loop() -> (Wake, calloop::EventLoop<'static, usize>) {
        let (wake, source) = Wake::new().unwrap();
        let loop_ = calloop::EventLoop::try_new().unwrap();
        loop_
            .handle()
            .insert_source(source, |_, _, count| *count += 1)
            .unwrap();
        (wake, loop_)
    }

    fn compile(dir: &std::path::Path, xml: &str) {
        std::fs::write(dir.join("proof.gschema.xml"), xml).unwrap();
        std::fs::write(dir.join("touchscreen.gschema.xml"), TOUCHSCREEN_SCHEMAS).unwrap();
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

    fn gio_reader(wake: &Wake, dir: std::path::PathBuf) -> Reader {
        Reader::with_factory(
            wake,
            Box::new(move |shared| {
                let schemas = gio::SettingsSchemaSource::from_directory(&dir, None, false).unwrap();
                let tablet = schemas.lookup(TABLET_SCHEMA, false).unwrap();
                let stylus = schemas.lookup(STYLUS_SCHEMA, false).unwrap();
                let pad_button = schemas.lookup(PAD_BUTTON_SCHEMA, false).unwrap();
                let touchscreen = schemas.lookup(TOUCHSCREEN_SCHEMA, false).unwrap();
                Ok(Box::new(GioReader::from_schemas(
                    tablet,
                    stylus,
                    pad_button,
                    touchscreen,
                    Some(gio::memory_settings_backend_new()),
                    shared,
                )?))
            }),
            HARNESS_LIMIT,
        )
    }

    fn completed(
        reader: &Reader,
        loop_: &mut calloop::EventLoop<usize>,
        poll_at: Instant,
    ) -> Snapshot {
        let deadline = Instant::now() + HARNESS_LIMIT;
        let mut count = 0;
        loop {
            if let Some(snapshot) = reader.poll(poll_at) {
                return snapshot;
            }
            let now = Instant::now();
            assert!(now < deadline, "actual worker failed to publish completion");
            loop_.dispatch(Some(deadline - now), &mut count).unwrap();
        }
    }

    fn wanted(paths: &[(Kind, &str)]) -> BTreeSet<(Kind, String)> {
        paths.iter().map(|(k, p)| (*k, (*p).to_owned())).collect()
    }

    #[test]
    fn per_device_paths_read_gnome_defaults_then_follow_live_changes() {
        let dir = tempfile::tempdir().unwrap();
        compile(dir.path(), SCHEMAS);
        let (wake, mut loop_) = wake_loop();
        let reader = gio_reader(&wake, dir.path().to_path_buf());
        // Nothing wanted, nothing read.
        assert_eq!(reader.next_deadline(Instant::now()), None);
        // A frozen poll clock: only `want` and the genuine changed
        // callback may schedule reads, never the periodic re-read.
        let poll_at = Instant::now();
        reader.want(
            wanted(&[
                (Kind::Tablet, TABLET_PATH),
                (Kind::Stylus, STYLUS_PATH),
                (Kind::PadButton, PAD_PATH),
                (Kind::Touchscreen, TOUCHSCREEN_PATH),
            ]),
            poll_at,
        );
        let first = completed(&reader, &mut loop_, poll_at);
        assert_eq!(first.tablets[TABLET_PATH], TabletSettings::default());
        assert_eq!(first.styluses[STYLUS_PATH], StylusSettings::default());
        assert_eq!(first.pad_buttons[PAD_PATH], PadButtonSettings::default());
        // GNOME's default `output` is no user value: automatic mapping.
        assert_eq!(first.touchscreens[TOUCHSCREEN_PATH], None);
        assert_eq!(reader.status(), CacheStatus::Fresh);
        let source = glib::idle_source_new(
            Some("tuna-tablet-settings-watch-proof"),
            glib::Priority::DEFAULT,
            || {
                LOCAL.with(|local| {
                    local
                        .borrow()
                        .as_ref()
                        .unwrap()
                        .native
                        .as_ref()
                        .unwrap()
                        .change();
                });
                glib::ControlFlow::Break
            },
        );
        source.attach(Some(&reader.shared.context));
        let changed = completed(&reader, &mut loop_, poll_at);
        let tablet = &changed.tablets[TABLET_PATH];
        assert_eq!(
            tablet.output.as_deref(),
            Some(&["GSM", "LG ULTRAFINE", "DP-1", "DP-1"].map(String::from)[..])
        );
        assert_eq!(tablet.mapping, Mapping::Relative);
        assert!(tablet.keep_aspect);
        assert_eq!(
            changed.touchscreens[TOUCHSCREEN_PATH].as_deref(),
            Some(&["BOE", "0x0a1c", "eDP-1"].map(String::from)[..])
        );
        let stylus = &changed.styluses[STYLUS_PATH];
        assert_eq!(stylus.pressure_curve, vec![0, 75, 25, 100]);
        assert_eq!(stylus.button_action, ButtonAction::Back);
        assert_eq!(stylus.tertiary_button_action, ButtonAction::Keybinding);
        assert_eq!(stylus.tertiary_button_keybinding, "<Control>z");
        assert_eq!(
            changed.pad_buttons[PAD_PATH],
            PadButtonSettings {
                action: PadButtonAction::Keybinding,
                keybinding: "<Shift>b".into(),
            }
        );
        // switch-monitor's write lands like any change: the next read has it.
        reader.set_output(
            TABLET_PATH.into(),
            [String::new(), String::new(), String::new()],
        );
        let written = loop {
            let snapshot = completed(&reader, &mut loop_, poll_at);
            if snapshot.tablets[TABLET_PATH]
                .output
                .as_ref()
                .is_some_and(|o| o.len() == 3)
            {
                break snapshot;
            }
        };
        assert_eq!(
            written.tablets[TABLET_PATH].output.as_deref(),
            Some(&[String::new(), String::new(), String::new()][..])
        );
        // A second tablet's path is its own: still GNOME's defaults.
        reader.want(
            wanted(&[(Kind::Tablet, TABLET_PATH), (Kind::Tablet, OTHER_PATH)]),
            poll_at,
        );
        // A read already queued by the change may complete first with the
        // old set; the new set's read follows.
        let both = loop {
            let snapshot = completed(&reader, &mut loop_, poll_at);
            if snapshot.tablets.contains_key(OTHER_PATH) {
                break snapshot;
            }
        };
        assert_eq!(both.tablets[OTHER_PATH], TabletSettings::default());
        assert_eq!(both.tablets[TABLET_PATH].mapping, Mapping::Relative);
        assert!(both.styluses.is_empty(), "an unplugged tool's path retires");
        reader.shared.quarantine();
        assert_eq!(reader.next_deadline(Instant::now()), None);
    }

    #[test]
    fn mismatched_schema_is_unavailable_and_keeps_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        compile(
            dir.path(),
            &SCHEMAS.replace(
                "<key name=\"keep-aspect\" type=\"b\">\n      <default>false</default>",
                "<key name=\"keep-aspect\" type=\"s\">\n      <default>'no'</default>",
            ),
        );
        assert!(SCHEMAS
            .contains("<key name=\"keep-aspect\" type=\"b\">\n      <default>false</default>"));
        let (wake, mut loop_) = wake_loop();
        let reader = gio_reader(&wake, dir.path().to_path_buf());
        reader.want(wanted(&[(Kind::Tablet, TABLET_PATH)]), Instant::now());
        let deadline = Instant::now() + HARNESS_LIMIT;
        let mut count = 0;
        while reader.status() != CacheStatus::Unavailable {
            assert!(Instant::now() < deadline);
            loop_
                .dispatch(Some(Duration::from_millis(20)), &mut count)
                .unwrap();
        }
        assert!(reader.poll(Instant::now()).is_none());
        reader.shared.quarantine();
    }
}
