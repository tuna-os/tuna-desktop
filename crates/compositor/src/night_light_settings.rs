//! Owned GIO reader. Native Settings never cross the creator thread.
//! Admission deadlines bound the consumer, not a claim that a blocked native
//! call has exited. A timed-out worker is quarantined permanently, never replaced.
use crate::night_light::*;
use crate::wake::Wake;
use gio::{glib, prelude::*};
use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, Weak};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

const OPERATION_LIMIT: Duration = Duration::from_secs(2);
const KEYS: [(&str, &str); 6] = [
    (KEY_ENABLED, "b"),
    (KEY_TEMPERATURE, "u"),
    (KEY_SCHEDULE_AUTOMATIC, "b"),
    (KEY_SCHEDULE_FROM, "d"),
    (KEY_SCHEDULE_TO, "d"),
    (KEY_LAST_COORDINATES, "(dd)"),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CacheStatus {
    Unavailable,
    Fresh,
    Stale,
    Quarantined,
}
#[derive(Clone, Copy)]
struct Operation {
    id: u64,
    admitted: Instant,
    deadline: Instant,
}
struct PreviewRequest {
    id: u64,
    admitted: Instant,
    until: Instant,
    reply: Arc<Mutex<PreviewReply>>,
}
#[derive(Default)]
struct PreviewReply {
    result: Option<Result<(), PreviewRejected>>,
    waker: Option<Waker>,
}
struct State {
    next: u64,
    active: Option<Operation>,
    dirty: bool,
    stopped: bool,
    status: CacheStatus,
    last: Option<Instant>,
    completion: Option<(Instant, Result<NightLightConfig, ReadError>)>,
    preview: Option<PreviewRequest>,
}
struct Shared {
    state: Mutex<State>,
    context: glib::MainContext,
    wake: Wake,
    share: Weak<NightLightShare>,
}
trait NativeReader {
    fn read(&mut self) -> Result<NightLightConfig, ReadError>;
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
    pub(crate) fn new(share: &Arc<NightLightShare>, wake: &Wake) -> Self {
        Self::with_factory(
            share,
            wake,
            Box::new(|shared| {
                GioReader::new(shared).map(|reader| Box::new(reader) as Box<dyn NativeReader>)
            }),
        )
    }
    fn with_factory(share: &Arc<NightLightShare>, wake: &Wake, factory: Factory) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                next: 0,
                active: None,
                dirty: false,
                stopped: false,
                status: CacheStatus::Unavailable,
                last: None,
                completion: None,
                preview: None,
            }),
            context: glib::MainContext::new(),
            wake: wake.clone(),
            share: Arc::downgrade(share),
        });
        let worker = shared.clone();
        let owner = std::thread::Builder::new()
            .name("tuna-night-light".into())
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
    /// The frame consumer never invokes GIO, waits for a worker, or joins it.
    pub(crate) fn poll(&self, now: Instant) -> Option<NightLightConfig> {
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
        let (_, result) = completion?;
        result.ok()
    }
    pub(crate) fn next_deadline(&self, now: Instant) -> Option<Instant> {
        let state = self.shared.state.lock().ok()?;
        let preview = state.preview.as_ref().map(|request| request.until.max(now));
        let read = if state.stopped || state.status == CacheStatus::Quarantined {
            None
        } else {
            Some(
                state
                    .active
                    .map(|op| op.deadline)
                    .or_else(|| state.last.and_then(|last| last.checked_add(READ_INTERVAL)))
                    .unwrap_or(now)
                    .max(now),
            )
        };
        read.into_iter().chain(preview).min()
    }
    pub(crate) fn cancel_preview(&self) {
        if let Ok(mut state) = self.shared.state.lock() {
            if let Some(request) = state.preview.take() {
                finish(&request.reply, Err(PreviewRejected::Unavailable));
            }
        }
    }
    pub(crate) fn status(&self) -> CacheStatus {
        self.shared
            .state
            .lock()
            .map(|s| s.status)
            .unwrap_or(CacheStatus::Quarantined)
    }
    pub(crate) fn preview(
        &self,
        duration: u32,
        now: Instant,
    ) -> Result<PreviewFuture, PreviewRejected> {
        if duration == 0 || duration > MAX_PREVIEW_SECONDS {
            return Err(PreviewRejected::InvalidDuration(duration));
        }
        let until = now
            .checked_add(Duration::from_secs(u64::from(duration)))
            .ok_or(PreviewRejected::Unavailable)?;
        let reply = Arc::new(Mutex::new(PreviewReply::default()));
        let id = {
            let mut state = self
                .shared
                .state
                .lock()
                .map_err(|_| PreviewRejected::Unavailable)?;
            if state.stopped || state.status == CacheStatus::Quarantined {
                return Err(PreviewRejected::Unavailable);
            }
            state.next = state
                .next
                .checked_add(1)
                .ok_or(PreviewRejected::Unavailable)?;
            let id = state.next;
            if let Some(old) = state.preview.take() {
                finish(&old.reply, Err(PreviewRejected::Unavailable));
            }
            state.preview = Some(PreviewRequest {
                id,
                admitted: now,
                until,
                reply: reply.clone(),
            });
            id
        };
        self.shared.request(now);
        self.shared.wake.signal();
        Ok(PreviewFuture {
            shared: Arc::downgrade(&self.shared),
            id,
            reply,
        })
    }
}
impl Drop for Reader {
    fn drop(&mut self) {
        self.shared.quarantine();
        // Drop detaches the one owner. No frame-thread join, including native
        // thread-local destruction: is_finished is not a hard retirement bound.
        // Quarantine is permanent and never grants replacement-worker admission.
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
            let Some(deadline) = now.checked_add(OPERATION_LIMIT) else {
                return;
            };
            state.next = id;
            let op = Operation {
                id,
                admitted: now,
                deadline,
            };
            state.active = Some(op);
            state.last = Some(now);
            if state.status == CacheStatus::Fresh {
                state.status = CacheStatus::Stale;
            }
            op
        };
        let weak = Arc::downgrade(self);
        // invoke() may run inline on the caller when a context is unowned.
        // An attached source can run ONLY through this worker's owned context.
        let source = glib::idle_source_new(
            Some("tuna-night-light-read"),
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
        let observed = Instant::now();
        if !self.current(op, observed) {
            self.expire(observed);
            return;
        }
        // Keep any last share owner alive until after the bookkeeping lock
        // drops: Share destruction owns Reader shutdown and must not recurse
        // into that same mutex from this native worker.
        let live_share = self.share.upgrade();
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
            if let Some(request) = state.preview.as_ref() {
                if op.admitted >= request.admitted {
                    let request = state.preview.take().unwrap();
                    let outcome = if observed < request.until {
                        match (&result, live_share.as_ref()) {
                            (Ok(config), Some(share)) => {
                                share.record_preview_until(config.temperature_k(), request.until)
                            }
                            _ => Err(PreviewRejected::Unavailable),
                        }
                    } else {
                        Err(PreviewRejected::Unavailable)
                    };
                    finish(&request.reply, outcome);
                } else {
                    state.dirty = true;
                }
            }
            state.completion = Some((observed, result));
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
            return;
        }
        if let Ok(mut state) = self.state.lock() {
            if state
                .preview
                .as_ref()
                .is_some_and(|request| now >= request.until)
            {
                let request = state.preview.take().unwrap();
                finish(&request.reply, Err(PreviewRejected::Unavailable));
            }
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
            if let Some(request) = state.preview.take() {
                finish(&request.reply, Err(PreviewRejected::Unavailable));
            }
        }
        let source =
            glib::idle_source_new(Some("tuna-night-light-stop"), glib::Priority::HIGH, || {
                LOCAL.with(|local| {
                    if let Some(local) = local.borrow().as_ref() {
                        local.loop_.quit();
                    }
                });
                glib::ControlFlow::Break
            });
        source.attach(Some(&self.context));
        self.wake.signal();
    }
}
fn finish(reply: &Mutex<PreviewReply>, result: Result<(), PreviewRejected>) {
    if let Ok(mut reply) = reply.lock() {
        reply.result = Some(result);
        if let Some(waker) = reply.waker.take() {
            waker.wake();
        }
    }
}
pub(crate) struct PreviewFuture {
    shared: Weak<Shared>,
    id: u64,
    reply: Arc<Mutex<PreviewReply>>,
}
impl Future for PreviewFuture {
    type Output = Result<(), PreviewRejected>;
    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let Ok(mut reply) = self.reply.lock() else {
            return Poll::Ready(Err(PreviewRejected::Unavailable));
        };
        if let Some(result) = reply.result {
            Poll::Ready(result)
        } else {
            reply.waker = Some(context.waker().clone());
            Poll::Pending
        }
    }
}
impl Drop for PreviewFuture {
    fn drop(&mut self) {
        if let Some(shared) = self.shared.upgrade() {
            if let Ok(mut state) = shared.state.lock() {
                if state
                    .preview
                    .as_ref()
                    .is_some_and(|request| request.id == self.id)
                {
                    state.preview = None;
                }
            }
        }
    }
}
struct GioReader {
    settings: gio::Settings,
    handler: Option<glib::SignalHandlerId>,
}
impl GioReader {
    fn new(shared: Weak<Shared>) -> Result<Self, ReadError> {
        let source = gio::SettingsSchemaSource::default()
            .ok_or_else(|| ReadError("schema source unavailable".into()))?;
        let schema = source
            .lookup(COLOR_SCHEMA, true)
            .ok_or_else(|| ReadError("color schema unavailable".into()))?;
        Self::from_schema(&schema, None, shared)
    }
    fn from_schema(
        schema: &gio::SettingsSchema,
        backend: Option<&gio::SettingsBackend>,
        shared: Weak<Shared>,
    ) -> Result<Self, ReadError> {
        for (key, signature) in KEYS {
            if !schema.has_key(key) || schema.key(key).value_type().as_str() != signature {
                return Err(ReadError("color schema key/type mismatch".into()));
            }
        }
        let settings = gio::Settings::new_full(schema, backend, None);
        let handler = settings.connect_changed(None, move |_, key| {
            if KEYS.iter().any(|(known, _)| *known == key) {
                if let Some(shared) = shared.upgrade() {
                    shared.request(Instant::now());
                }
            }
        });
        Ok(Self {
            settings,
            handler: Some(handler),
        })
    }
}
impl NativeReader for GioReader {
    #[cfg(test)]
    fn change(&self) {
        self.settings.set_uint(KEY_TEMPERATURE, 3200).unwrap();
    }
    fn read(&mut self) -> Result<NightLightConfig, ReadError> {
        // Watch connected BEFORE these initial reads; GSettings requires reading
        // a key before its changed signal is emitted. Fixed types bound all data.
        let value = |key| self.settings.value(key);
        let error = || ReadError("invalid color settings value".into());
        let (latitude_deg, longitude_deg) = value(KEY_LAST_COORDINATES)
            .get::<(f64, f64)>()
            .ok_or_else(error)?;
        let raw = RawSettings {
            enabled: value(KEY_ENABLED).get::<bool>().ok_or_else(error)?,
            temperature_k: value(KEY_TEMPERATURE).get::<u32>().ok_or_else(error)?,
            automatic: value(KEY_SCHEDULE_AUTOMATIC)
                .get::<bool>()
                .ok_or_else(error)?,
            from_hour: value(KEY_SCHEDULE_FROM).get::<f64>().ok_or_else(error)?,
            to_hour: value(KEY_SCHEDULE_TO).get::<f64>().ok_or_else(error)?,
            latitude_deg,
            longitude_deg,
        };
        if !raw.from_hour.is_finite()
            || !raw.to_hour.is_finite()
            || !latitude_deg.is_finite()
            || !longitude_deg.is_finite()
        {
            return Err(error());
        }
        Ok(raw.into_config())
    }
}
impl Drop for GioReader {
    fn drop(&mut self) {
        if let Some(handler) = self.handler.take() {
            self.settings.disconnect(handler);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    struct Controlled {
        events: mpsc::Sender<&'static str>,
        answers: mpsc::Receiver<Result<NightLightConfig, ReadError>>,
        creator: std::thread::ThreadId,
    }
    impl NativeReader for Controlled {
        fn read(&mut self) -> Result<NightLightConfig, ReadError> {
            assert_eq!(self.creator, std::thread::current().id());
            self.events.send("entered").unwrap();
            self.answers.recv_timeout(Duration::from_secs(10)).unwrap()
        }
    }
    impl Drop for Controlled {
        fn drop(&mut self) {
            assert_eq!(self.creator, std::thread::current().id());
            let _ = self.events.send("retired");
        }
    }
    type Harness = (
        Arc<NightLightShare>,
        Reader,
        mpsc::Receiver<&'static str>,
        mpsc::Sender<Result<NightLightConfig, ReadError>>,
        calloop::EventLoop<'static, usize>,
    );
    fn controlled() -> Harness {
        let (wake, source) = Wake::new().unwrap();
        let mut loop_ = calloop::EventLoop::try_new().unwrap();
        loop_
            .handle()
            .insert_source(source, |_, _, count| *count += 1)
            .unwrap();
        let share = Arc::new(NightLightShare::default());
        share.set_wake(&wake);
        let (events, receive) = mpsc::channel();
        let (send, answers) = mpsc::channel();
        let factory: Factory = Box::new(move |_| {
            Ok(Box::new(Controlled {
                events,
                answers,
                creator: std::thread::current().id(),
            }))
        });
        let reader = Reader::with_factory(&share, &wake, factory);
        assert_eq!(
            receive.recv_timeout(Duration::from_secs(2)).unwrap(),
            "entered"
        );
        (share, reader, receive, send, loop_)
    }
    fn completed(reader: &Reader, loop_: &mut calloop::EventLoop<usize>) -> NightLightConfig {
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut count = 0;
        loop {
            if let Some(config) = reader.poll(Instant::now()) {
                return config;
            }
            let now = Instant::now();
            assert!(now < deadline, "actual worker failed to publish completion");
            loop_.dispatch(Some(deadline - now), &mut count).unwrap();
        }
    }
    fn retired(mut reader: Reader, events: &mpsc::Receiver<&'static str>) {
        reader.shared.quarantine();
        assert_eq!(
            events.recv_timeout(Duration::from_secs(2)).unwrap(),
            "retired"
        );
        reader.owner.take().unwrap().join().unwrap();
    }
    #[test]
    fn actual_blocked_worker_quarantines_once_denies_late_cache_and_preview_without_replacement() {
        let (share, reader, events, answers, _) = controlled();
        let future = reader.preview(5, Instant::now()).unwrap();
        let expiry = reader.shared.state.lock().unwrap().active.unwrap().deadline;
        assert!(reader.poll(expiry).is_none());
        assert_eq!(reader.status(), CacheStatus::Quarantined);
        assert_eq!(reader.next_deadline(expiry), None);
        assert_eq!(
            future.reply.lock().unwrap().result,
            Some(Err(PreviewRejected::Unavailable))
        );
        for _ in 0..100 {
            reader.shared.request(expiry);
            assert!(reader.poll(expiry).is_none());
        }
        assert!(matches!(
            reader.preview(5, expiry),
            Err(PreviewRejected::Unavailable)
        ));
        answers
            .send(Ok(RawSettings::schema_defaults().into_config()))
            .unwrap();
        retired(reader, &events);
        assert_eq!(share.preview_temperature(Instant::now()), None);
        assert!(
            events.try_recv().is_err(),
            "no new creator/read after quarantine"
        );
    }
    #[test]
    fn actual_worker_fresh_preview_uses_request_expiry_and_stale_future_cannot_cancel_newer() {
        let (share, reader, events, answers, mut loop_) = controlled();
        let first = reader.preview(5, Instant::now()).unwrap();
        let admitted = Instant::now();
        let newer = reader.preview(5, admitted).unwrap();
        assert_eq!(
            first.reply.lock().unwrap().result,
            Some(Err(PreviewRejected::Unavailable))
        );
        drop(first);
        assert_eq!(
            reader
                .shared
                .state
                .lock()
                .unwrap()
                .preview
                .as_ref()
                .unwrap()
                .id,
            newer.id
        );
        answers
            .send(Ok(RawSettings::schema_defaults().into_config()))
            .unwrap();
        assert_eq!(
            events.recv_timeout(Duration::from_secs(2)).unwrap(),
            "entered"
        );
        assert_eq!(
            newer.reply.lock().unwrap().result,
            None,
            "pre-request read cannot grant preview"
        );
        let mut raw = RawSettings::schema_defaults();
        raw.temperature_k = 3200;
        answers.send(Ok(raw.into_config())).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut count = 0;
        while newer.reply.lock().unwrap().result.is_none() {
            let now = Instant::now();
            assert!(now < deadline);
            loop_.dispatch(Some(deadline - now), &mut count).unwrap();
        }
        assert_eq!(newer.reply.lock().unwrap().result, Some(Ok(())));
        assert_eq!(
            share.preview_deadline(),
            Some(admitted + Duration::from_secs(5))
        );
        assert_eq!(share.preview_temperature(Instant::now()), Some(3200));
        assert_eq!(
            share.preview_temperature(admitted + Duration::from_secs(5)),
            None
        );
        retired(reader, &events);
    }
    #[test]
    fn actual_pending_preview_expiry_is_consumed_before_a_later_valid_read() {
        let (share, reader, events, answers, mut loop_) = controlled();
        let future = reader.preview(1, Instant::now()).unwrap();
        let until = reader
            .shared
            .state
            .lock()
            .unwrap()
            .preview
            .as_ref()
            .unwrap()
            .until;
        assert!(reader.poll(until).is_none());
        assert_eq!(
            future.reply.lock().unwrap().result,
            Some(Err(PreviewRejected::Unavailable))
        );
        assert!(reader.shared.state.lock().unwrap().preview.is_none());
        assert_ne!(reader.status(), CacheStatus::Quarantined);
        answers
            .send(Ok(RawSettings::schema_defaults().into_config()))
            .unwrap();
        assert_eq!(
            events.recv_timeout(Duration::from_secs(2)).unwrap(),
            "entered"
        );
        answers
            .send(Ok(RawSettings::schema_defaults().into_config()))
            .unwrap();
        completed(&reader, &mut loop_);
        assert_eq!(share.preview_temperature(Instant::now()), None);
        retired(reader, &events);
    }
    #[test]
    fn actual_worker_failed_read_and_dropped_pending_preview_preserve_prior_config() {
        let (share, reader, events, answers, mut loop_) = controlled();
        let future = reader.preview(5, Instant::now()).unwrap();
        drop(future);
        assert!(reader.shared.state.lock().unwrap().preview.is_none());
        answers
            .send(Err(ReadError("controlled failure".into())))
            .unwrap();
        assert_eq!(
            events.recv_timeout(Duration::from_secs(2)).unwrap(),
            "entered"
        );
        let mut raw = RawSettings::schema_defaults();
        raw.enabled = true;
        answers.send(Ok(raw.into_config())).unwrap();
        assert!(completed(&reader, &mut loop_).enabled);
        assert_eq!(share.preview_temperature(Instant::now()), None);
        retired(reader, &events);
    }
    #[test]
    fn actual_controller_failed_cache_keeps_config_and_clear_cancels_pending_without_late_restore()
    {
        let (share, reader, events, answers, mut loop_) = controlled();
        let shared = reader.shared.clone();
        share.swap_reader_for_test(Some(reader));
        let mut controller = NightLightController::new(share.clone());
        let mut prior = RawSettings::schema_defaults();
        prior.enabled = true;
        prior.temperature_k = 4000;
        controller.maybe_refresh(Instant::now(), || Ok(prior.into_config()));
        let preview = share.request_preview(5).unwrap();
        share.clear_preview();
        assert_eq!(
            preview.reply.lock().unwrap().result,
            Some(Err(PreviewRejected::Unavailable))
        );
        answers
            .send(Err(ReadError("controlled unavailable".into())))
            .unwrap();
        assert_eq!(
            events.recv_timeout(Duration::from_secs(2)).unwrap(),
            "entered"
        );
        answers
            .send(Err(ReadError("controlled unavailable".into())))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut count = 0;
        loop {
            controller.poll_settings(Instant::now());
            if shared.state.lock().unwrap().active.is_none() {
                break;
            }
            let now = Instant::now();
            assert!(now < deadline);
            loop_.dispatch(Some(deadline - now), &mut count).unwrap();
        }
        controller.poll_settings(Instant::now());
        assert_eq!(controller.config().temperature_k(), 4000);
        assert!(controller.config().enabled);
        assert_eq!(share.preview_temperature(Instant::now()), None);
        loop_.dispatch(Some(Duration::ZERO), &mut count).unwrap();
        let stable = count;
        for _ in 0..10 {
            loop_.dispatch(Some(Duration::ZERO), &mut count).unwrap();
        }
        assert_eq!(count, stable, "actual completion wake drained without spin");
        retired(share.swap_reader_for_test(None).unwrap(), &events);
    }
    #[test]
    fn actual_owned_state_poison_quarantines_and_refuses_late_result() {
        let (_, reader, events, answers, _) = controlled();
        let shared = reader.shared.clone();
        assert!(std::thread::spawn(move || {
            let _guard = shared.state.lock().unwrap();
            panic!("controlled state poison");
        })
        .join()
        .is_err());
        assert!(reader.poll(Instant::now()).is_none());
        assert_eq!(reader.status(), CacheStatus::Quarantined);
        assert_eq!(reader.next_deadline(Instant::now()), None);
        answers
            .send(Ok(RawSettings::schema_defaults().into_config()))
            .unwrap();
        retired(reader, &events);
    }
    #[test]
    fn invalid_preview_is_rejected_before_reader_or_native_io() {
        let share = NightLightShare::default();
        assert!(matches!(
            share.request_preview(0),
            Err(PreviewRejected::InvalidDuration(0))
        ));
        assert!(matches!(
            share.request_preview(121),
            Err(PreviewRejected::InvalidDuration(121))
        ));
        assert!(share.reader_for_test_is_absent());
    }
    #[test]
    fn actual_gio_memory_watch_updates_typed_cache_on_owned_context_without_main_native_calls() {
        let dir = tempfile::tempdir().unwrap();
        let mut xml =
            format!("<schemalist><schema id='{COLOR_SCHEMA}' path='/tuna/night-light-proof/'>");
        for (key, signature) in KEYS {
            let default = match key {
                KEY_ENABLED | KEY_SCHEDULE_AUTOMATIC => "false",
                KEY_TEMPERATURE => "2700",
                KEY_SCHEDULE_FROM => "20.0",
                KEY_SCHEDULE_TO => "6.0",
                _ => "(91.0,181.0)",
            };
            xml.push_str(&format!(
                "<key name='{key}' type='{signature}'><default>{default}</default></key>"
            ));
        }
        xml.push_str("</schema></schemalist>");
        std::fs::write(dir.path().join("proof.gschema.xml"), xml).unwrap();
        assert!(
            std::process::Command::new("timeout")
                .args(["--kill-after=2s", "10s", "glib-compile-schemas"])
                .arg(dir.path())
                .status()
                .unwrap()
                .success(),
            "required real schema compiler must execute"
        );
        let path = dir.path().to_path_buf();
        let (wake, source) = Wake::new().unwrap();
        let mut loop_ = calloop::EventLoop::<usize>::try_new().unwrap();
        loop_
            .handle()
            .insert_source(source, |_, _, count| *count += 1)
            .unwrap();
        let share = Arc::new(NightLightShare::default());
        let reader = Reader::with_factory(
            &share,
            &wake,
            Box::new(move |shared| {
                let schemas =
                    gio::SettingsSchemaSource::from_directory(&path, None, false).unwrap();
                let schema = schemas.lookup(COLOR_SCHEMA, false).unwrap();
                let backend = gio::memory_settings_backend_new();
                Ok(Box::new(GioReader::from_schema(
                    &schema,
                    Some(&backend),
                    shared,
                )?))
            }),
        );
        assert_eq!(completed(&reader, &mut loop_).temperature_k(), 2700);
        let source = glib::idle_source_new(
            Some("tuna-night-light-native-watch-proof"),
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
        assert_eq!(completed(&reader, &mut loop_).temperature_k(), 3200);
        reader.shared.quarantine();
        // Tests may join only after the quit callback has actually executed.
        let deadline = Instant::now() + Duration::from_secs(2);
        while !reader.owner.as_ref().unwrap().is_finished() {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert_eq!(reader.next_deadline(Instant::now()), None);
    }
}
