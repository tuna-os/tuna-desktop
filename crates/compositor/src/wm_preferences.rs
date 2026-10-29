//! Owned GIO reader for GNOME's focus policy keys
//! (`org.gnome.desktop.wm.preferences`, `org.gnome.mutter`), read by the
//! compositor itself as Mutter's `prefs.c` does in-process.
//!
//! The mechanics are `night_light_settings.rs`'s, without its preview:
//! native `Settings` and their retained `changed` subscription live and
//! retire on one owned GLib thread; every read is an idle source on that
//! thread's own context, admitted under a deadline the frame consumer
//! enforces; a timed-out, panicked or poisoned worker is quarantined for
//! good, never replaced. The consumer polls a completed snapshot and never
//! calls GIO. A missing or mismatched schema leaves the reader unavailable
//! and the consumer on GNOME 51's defaults ([`FocusPolicy::default`]).
//!
//! [`FocusPolicy::default`]: crate::focus_policy::FocusPolicy
use crate::focus_policy::{RawFocusSettings, MUTTER_SCHEMA, WM_SCHEMA};
use crate::night_light::ReadError;
use crate::night_light_settings::CacheStatus;
use crate::wake::Wake;
use gio::{glib, prelude::*};
use std::cell::RefCell;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

const OPERATION_LIMIT: Duration = Duration::from_secs(2);
/// Bounded re-read, as night light's: a failed first read retries and a
/// lost `changed` signal heals.
pub(crate) const READ_INTERVAL: Duration = Duration::from_secs(5);
const KEY_FOCUS_MODE: &str = "focus-mode";
const KEY_AUTO_RAISE: &str = "auto-raise";
const KEY_AUTO_RAISE_DELAY: &str = "auto-raise-delay";
const KEY_RAISE_ON_CLICK: &str = "raise-on-click";
const KEY_MOUSE_BUTTON_MODIFIER: &str = "mouse-button-modifier";
const KEY_RESIZE_WITH_RIGHT_BUTTON: &str = "resize-with-right-button";
const KEY_POINTER_REST: &str = "focus-change-on-pointer-rest";
const WM_KEYS: [(&str, &str); 6] = [
    // An enum key's value type is its string nick.
    (KEY_FOCUS_MODE, "s"),
    (KEY_AUTO_RAISE, "b"),
    (KEY_AUTO_RAISE_DELAY, "i"),
    (KEY_RAISE_ON_CLICK, "b"),
    (KEY_MOUSE_BUTTON_MODIFIER, "s"),
    (KEY_RESIZE_WITH_RIGHT_BUTTON, "b"),
];

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
    completion: Option<Result<RawFocusSettings, ReadError>>,
}
struct Shared {
    state: Mutex<State>,
    context: glib::MainContext,
    wake: Wake,
    operation_limit: Duration,
}
trait NativeReader {
    fn read(&mut self) -> Result<RawFocusSettings, ReadError>;
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
            }),
            context: glib::MainContext::new(),
            wake: wake.clone(),
            operation_limit,
        });
        let worker = shared.clone();
        let owner = std::thread::Builder::new()
            .name("tuna-wm-preferences".into())
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
        } else {
            reader.shared.request(Instant::now());
        }
        reader
    }
    /// The frame consumer never invokes GIO, waits for a worker, or joins
    /// it. `Some` only for a completed, valid read.
    pub(crate) fn poll(&self, now: Instant) -> Option<RawFocusSettings> {
        self.shared.expire(now);
        let (completion, request) = {
            let Ok(mut state) = self.shared.state.lock() else {
                return None;
            };
            let request = !state.stopped
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
        if state.stopped || state.status == CacheStatus::Quarantined {
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
            Some("tuna-wm-preferences-read"),
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
                local.native.as_mut().unwrap().read()
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
            state.completion = Some(result);
            std::mem::take(&mut state.dirty)
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
            Some("tuna-wm-preferences-stop"),
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

/// Watch `settings` on the owned context; only the focus keys re-read.
fn watch(settings: &gio::Settings, shared: Weak<Shared>) -> glib::SignalHandlerId {
    settings.connect_changed(None, move |_, key| {
        if key == KEY_POINTER_REST || WM_KEYS.iter().any(|(known, _)| *known == key) {
            if let Some(shared) = shared.upgrade() {
                shared.request(Instant::now());
            }
        }
    })
}

struct GioReader {
    wm: gio::Settings,
    /// `org.gnome.mutter`, when installed with the pointer-rest key.
    mutter: Option<gio::Settings>,
    handlers: Vec<(gio::Settings, glib::SignalHandlerId)>,
}
impl GioReader {
    fn new(shared: Weak<Shared>) -> Result<Self, ReadError> {
        let source = gio::SettingsSchemaSource::default()
            .ok_or_else(|| ReadError("schema source unavailable".into()))?;
        let wm = source
            .lookup(WM_SCHEMA, true)
            .ok_or_else(|| ReadError("wm preferences schema unavailable".into()))?;
        let mutter = source.lookup(MUTTER_SCHEMA, true);
        Self::from_schemas(&wm, mutter.as_ref(), None, shared)
    }
    fn from_schemas(
        wm: &gio::SettingsSchema,
        mutter: Option<&gio::SettingsSchema>,
        backend: Option<&gio::SettingsBackend>,
        shared: Weak<Shared>,
    ) -> Result<Self, ReadError> {
        if !schema_matches(wm, &WM_KEYS) {
            return Err(ReadError("wm preferences key/type mismatch".into()));
        }
        let wm = gio::Settings::new_full(wm, backend, None);
        let mutter = mutter
            .filter(|schema| schema_matches(schema, &[(KEY_POINTER_REST, "b")]))
            .map(|schema| gio::Settings::new_full(schema, backend, None));
        let handlers = std::iter::once(&wm)
            .chain(mutter.as_ref())
            .map(|settings| (settings.clone(), watch(settings, shared.clone())))
            .collect();
        Ok(Self {
            wm,
            mutter,
            handlers,
        })
    }
}
impl NativeReader for GioReader {
    #[cfg(test)]
    fn change(&self) {
        self.wm.set_string(KEY_FOCUS_MODE, "sloppy").unwrap();
    }
    fn read(&mut self) -> Result<RawFocusSettings, ReadError> {
        // Watches connected BEFORE these reads; GSettings emits `changed`
        // only for keys read since. Fixed types bound all data.
        let error = || ReadError("invalid wm preferences value".into());
        let value = |key| self.wm.value(key);
        Ok(RawFocusSettings {
            focus_mode: value(KEY_FOCUS_MODE).get::<String>().ok_or_else(error)?,
            auto_raise: value(KEY_AUTO_RAISE).get::<bool>().ok_or_else(error)?,
            auto_raise_delay_ms: value(KEY_AUTO_RAISE_DELAY).get::<i32>().ok_or_else(error)?,
            raise_on_click: value(KEY_RAISE_ON_CLICK).get::<bool>().ok_or_else(error)?,
            mouse_button_modifier: value(KEY_MOUSE_BUTTON_MODIFIER)
                .get::<String>()
                .ok_or_else(error)?,
            resize_with_right_button: value(KEY_RESIZE_WITH_RIGHT_BUTTON)
                .get::<bool>()
                .ok_or_else(error)?,
            focus_change_on_pointer_rest: match &self.mutter {
                Some(mutter) => Some(
                    mutter
                        .value(KEY_POINTER_REST)
                        .get::<bool>()
                        .ok_or_else(error)?,
                ),
                None => None,
            },
        })
    }
}
impl Drop for GioReader {
    fn drop(&mut self) {
        for (settings, handler) in self.handlers.drain(..) {
            settings.disconnect(handler);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::focus_policy::{FocusMode, FocusPolicy};
    use std::sync::mpsc;

    const HARNESS_LIMIT: Duration = Duration::from_secs(10);

    fn wake_loop() -> (Wake, calloop::EventLoop<'static, usize>) {
        let (wake, source) = Wake::new().unwrap();
        let loop_ = calloop::EventLoop::try_new().unwrap();
        loop_
            .handle()
            .insert_source(source, |_, _, count| *count += 1)
            .unwrap();
        (wake, loop_)
    }

    fn completed(
        reader: &Reader,
        loop_: &mut calloop::EventLoop<usize>,
        poll_at: Instant,
    ) -> RawFocusSettings {
        let deadline = Instant::now() + HARNESS_LIMIT;
        let mut count = 0;
        loop {
            if let Some(raw) = reader.poll(poll_at) {
                return raw;
            }
            let now = Instant::now();
            assert!(now < deadline, "actual worker failed to publish completion");
            loop_.dispatch(Some(deadline - now), &mut count).unwrap();
        }
    }

    fn retire(reader: &Reader) {
        reader.shared.quarantine();
        let deadline = Instant::now() + HARNESS_LIMIT;
        while !reader.owner.as_ref().unwrap().is_finished() {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
    }

    /// Compile a schema list into `dir` with the real schema compiler.
    fn compile(dir: &std::path::Path, xml: &str) {
        std::fs::write(dir.join("proof.gschema.xml"), xml).unwrap();
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

    const WM_XML: &str = "<schema id='org.gnome.desktop.wm.preferences' path='/tuna/wm-proof/'>\
        <key name='focus-mode' enum='org.gnome.desktop.GDesktopFocusMode'><default>'click'</default></key>\
        <key name='auto-raise' type='b'><default>false</default></key>\
        <key name='auto-raise-delay' type='i'><default>500</default></key>\
        <key name='raise-on-click' type='b'><default>true</default></key>\
        <key name='mouse-button-modifier' type='s'><default>'&lt;Super&gt;'</default></key>\
        <key name='resize-with-right-button' type='b'><default>false</default></key>\
        </schema>";
    const ENUM_XML: &str = "<enum id='org.gnome.desktop.GDesktopFocusMode'>\
        <value nick='click' value='0'/><value nick='sloppy' value='1'/><value nick='mouse' value='2'/>\
        </enum>";

    fn gio_reader(wake: &Wake, dir: std::path::PathBuf, with_mutter: bool) -> Reader {
        Reader::with_factory(
            wake,
            Box::new(move |shared| {
                let schemas = gio::SettingsSchemaSource::from_directory(&dir, None, false).unwrap();
                let wm = schemas.lookup(WM_SCHEMA, false).unwrap();
                let mutter = with_mutter
                    .then(|| schemas.lookup(MUTTER_SCHEMA, false))
                    .flatten();
                let backend = gio::memory_settings_backend_new();
                Ok(Box::new(GioReader::from_schemas(
                    &wm,
                    mutter.as_ref(),
                    Some(&backend),
                    shared,
                )?))
            }),
            HARNESS_LIMIT,
        )
    }

    #[test]
    fn actual_gio_watch_delivers_live_focus_mode_changes_on_the_owned_context() {
        let dir = tempfile::tempdir().unwrap();
        compile(
            dir.path(),
            &format!(
                "<schemalist>{ENUM_XML}{WM_XML}\
                 <schema id='org.gnome.mutter' path='/tuna/mutter-proof/'>\
                 <key name='focus-change-on-pointer-rest' type='b'><default>false</default></key>\
                 </schema></schemalist>"
            ),
        );
        let (wake, mut loop_) = wake_loop();
        // A frozen poll clock: only the genuine changed callback may
        // schedule the second read, never the periodic re-read.
        let poll_at = Instant::now();
        let reader = gio_reader(&wake, dir.path().to_path_buf(), true);
        let first = completed(&reader, &mut loop_, poll_at);
        assert_eq!(first.focus_mode, "click");
        assert_eq!(first.mouse_button_modifier, "<Super>");
        assert_eq!(first.focus_change_on_pointer_rest, Some(false));
        assert_eq!(reader.status(), CacheStatus::Fresh);
        let source = glib::idle_source_new(
            Some("tuna-wm-preferences-watch-proof"),
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
        assert_eq!(changed.focus_mode, "sloppy");
        let mut policy = FocusPolicy::default();
        policy.apply(&changed);
        assert_eq!(policy.mode, FocusMode::Sloppy);
        assert!(!policy.focus_change_on_pointer_rest);
        retire(&reader);
        assert_eq!(reader.next_deadline(Instant::now()), None);
    }

    #[test]
    fn missing_mutter_schema_reads_wm_keys_and_keeps_the_shell_default() {
        let dir = tempfile::tempdir().unwrap();
        compile(
            dir.path(),
            &format!("<schemalist>{ENUM_XML}{WM_XML}</schemalist>"),
        );
        let (wake, mut loop_) = wake_loop();
        let reader = gio_reader(&wake, dir.path().to_path_buf(), false);
        let raw = completed(&reader, &mut loop_, Instant::now());
        assert_eq!(raw.focus_change_on_pointer_rest, None);
        let mut policy = FocusPolicy::default();
        policy.apply(&raw);
        assert_eq!(policy, FocusPolicy::default());
        retire(&reader);
    }

    #[test]
    fn mismatched_wm_schema_is_unavailable_and_keeps_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        // auto-raise-delay typed as a string: the schema is refused.
        compile(
            dir.path(),
            &format!(
                "<schemalist>{ENUM_XML}{}</schemalist>",
                WM_XML.replace(
                    "name='auto-raise-delay' type='i'><default>500",
                    "name='auto-raise-delay' type='s'><default>'500'"
                )
            ),
        );
        let (wake, mut loop_) = wake_loop();
        let reader = gio_reader(&wake, dir.path().to_path_buf(), false);
        let deadline = Instant::now() + HARNESS_LIMIT;
        let mut count = 0;
        while reader.status() != CacheStatus::Unavailable {
            assert!(Instant::now() < deadline);
            loop_
                .dispatch(Some(Duration::from_millis(20)), &mut count)
                .unwrap();
        }
        // The consumer keeps GNOME's defaults: nothing completes.
        assert!(reader.poll(Instant::now()).is_none());
        retire(&reader);
    }

    struct Blocked {
        events: mpsc::Sender<&'static str>,
        answers: mpsc::Receiver<Result<RawFocusSettings, ReadError>>,
        creator: std::thread::ThreadId,
    }
    impl NativeReader for Blocked {
        fn read(&mut self) -> Result<RawFocusSettings, ReadError> {
            assert_eq!(self.creator, std::thread::current().id());
            self.events.send("entered").unwrap();
            self.answers.recv_timeout(HARNESS_LIMIT).unwrap()
        }
    }
    impl Drop for Blocked {
        fn drop(&mut self) {
            assert_eq!(self.creator, std::thread::current().id());
            let _ = self.events.send("retired");
        }
    }

    #[test]
    fn blocked_worker_quarantines_permanently_without_replacement() {
        let (wake, _loop) = wake_loop();
        let (events, receive) = mpsc::channel();
        let (send, answers) = mpsc::channel();
        let reader = Reader::with_factory(
            &wake,
            Box::new(move |_| {
                Ok(Box::new(Blocked {
                    events,
                    answers,
                    creator: std::thread::current().id(),
                }))
            }),
            HARNESS_LIMIT,
        );
        assert_eq!(receive.recv_timeout(HARNESS_LIMIT).unwrap(), "entered");
        let expiry = reader.shared.state.lock().unwrap().active.unwrap().deadline;
        assert!(reader.poll(expiry).is_none());
        assert_eq!(reader.status(), CacheStatus::Quarantined);
        assert_eq!(reader.next_deadline(expiry), None);
        for _ in 0..10 {
            reader.shared.request(expiry);
            assert!(reader.poll(expiry).is_none());
        }
        send.send(Err(ReadError("late".into()))).unwrap();
        assert_eq!(receive.recv_timeout(HARNESS_LIMIT).unwrap(), "retired");
        assert_eq!(reader.status(), CacheStatus::Quarantined);
        assert!(receive.try_recv().is_err(), "no new read after quarantine");
    }
}
