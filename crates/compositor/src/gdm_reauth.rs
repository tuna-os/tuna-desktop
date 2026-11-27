//! Lock-screen verification through GDM's reauthentication channel, as
//! GNOME Shell 51's unlock dialog verifies (`js/gdm/userVerifier.js`:
//! `Gdm.Client.open_reauthentication_channel`, then the channel's
//! `UserVerifier`). When GDM started the session, the lock screen's PAM
//! conversations run in GDM's session worker on GDM's own services
//! (`gdm-password`, `gdm-fingerprint`), so the system's PAM policy applies
//! to an unlock exactly as it applies to a login. Without GDM the private
//! helper ([`crate::pam_backend`]) runs the `tuna-lock` services as before.
//!
//! Selection ([`select`]): an opened channel is used. When none opens,
//! a session GDM started (its logind `Service` is a `gdm-*` PAM service)
//! refuses the attempt, as GNOME's unlock dialog cannot verify without
//! its channel; any other session uses the private helper.
//!
//! One channel serves every conversation of the session at a time (GDM's
//! session object relays to one verifier connection, and GNOME runs the
//! password and fingerprint services side by side on it): the password
//! and fingerprint backends share it through [`Source`] and each reads
//! only its own service's signals. All D-Bus traffic runs on one private
//! GLib thread; a backend's creator thread waits for its events, bounded
//! by the conversation's deadlines and its cancellation.

use std::collections::HashMap;
use std::future::Future;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, LazyLock, Mutex, Weak};
use std::time::{Duration, Instant};

use gio::glib;
use gio::prelude::*;
use tuna_gdm::{Call, Signal};

use crate::pam_cancel::Cancellation;
use crate::pam_helper::{Message, Refusal, Style};

/// GDM's password service, which the lock's password conversation runs.
pub const PASSWORD_SERVICE: &str = "gdm-password";
/// GDM's fingerprint service, which the lock's fingerprint conversation runs.
pub const FINGERPRINT_SERVICE: &str = "gdm-fingerprint";
/// Bound on each D-Bus step: opening the channel, beginning, answering,
/// cancelling.
const CALL_TIMEOUT: Duration = Duration::from_secs(5);
/// Signals one conversation may have queued before it reads them.
const QUEUE: usize = 64;

/// Where the attempt runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Selection {
    /// GDM's session worker, over the opened channel.
    Gdm,
    /// The private helper on the `tuna-lock` services.
    Helper,
    /// Nothing: a GDM session whose channel did not open.
    Refuse,
}

/// The backend for one attempt: GDM whenever its channel opened, else
/// the private helper, except in a session GDM started.
pub fn select(started_by_gdm: bool, channel_opened: bool) -> Selection {
    match (channel_opened, started_by_gdm) {
        (true, _) => Selection::Gdm,
        (false, true) => Selection::Refuse,
        (false, false) => Selection::Helper,
    }
}

/// Whether logind's `Service` for the session (the PAM service that
/// opened it) is one of GDM's: `gdm-password`, `gdm-autologin`,
/// `gdm-fingerprint`, `gdm-smartcard`.
pub fn started_by_gdm(logind_service: Option<&str>) -> bool {
    logind_service.is_some_and(|service| service.starts_with("gdm-") && service.len() > 4)
}

/// One event of a conversation's service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Event {
    Signal(Signal),
    /// The channel closed (GDM went away or the connection failed).
    Closed,
}

/// What a conversation does with an event.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// Relay one message to the runtime (and answer a question with GDM's
    /// `AnswerQuery`).
    Relay(Message),
    /// The conversation ended.
    Finish(Finish),
}

/// How GDM ended a conversation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Finish {
    /// `VerificationComplete` for this service.
    Complete,
    /// `VerificationFailed` or `ServiceUnavailable`, classed as GDM's
    /// worker classed the PAM result.
    Refused(Refusal),
    /// The conversation stopped without a result (reset, stopped, closed).
    Ended,
}

/// GDM's signals for one service, mapped onto the relay's PAM messages
/// (`gdm-session-worker.c`: `PAM_TEXT_INFO` is `Info`, `PAM_ERROR_MSG` is
/// `Problem`, `PAM_PROMPT_ECHO_ON` is `InfoQuery`, `PAM_PROMPT_ECHO_OFF` is
/// `SecretInfoQuery`). GDM reports a failed conversation as a problem
/// carrying its generic message, then `VerificationFailed`
/// (`report_and_stop_conversation`); the runtime words that failure
/// itself, so a generic message directly before the failure is held back
/// and only classifies it.
pub(crate) struct Mapper {
    service: String,
    held: Option<String>,
}

impl Mapper {
    pub(crate) fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
            held: None,
        }
    }

    fn generic(text: &str) -> bool {
        use crate::runtime::auth::{ACCOUNT_EXPIRED, FINGERPRINT_FAILED, PASSWORD_FAILED};
        [PASSWORD_FAILED, FINGERPRINT_FAILED, ACCOUNT_EXPIRED].contains(&text)
    }

    fn message(style: Style, text: String) -> Step {
        Step::Relay(Message {
            index: 0,
            style,
            text,
        })
    }

    pub(crate) fn map(&mut self, event: Event) -> Vec<Step> {
        let signal = match event {
            Event::Closed => return vec![Step::Finish(Finish::Ended)],
            Event::Signal(signal) => signal,
        };
        if let Some(service) = signal_service(&signal) {
            if service != self.service {
                return Vec::new();
            }
        }
        let mut steps = Vec::new();
        let flush = |held: &mut Option<String>, steps: &mut Vec<Step>| {
            if let Some(text) = held.take() {
                steps.push(Self::message(Style::Error, text));
            }
        };
        match signal {
            Signal::Info { text, .. } => {
                flush(&mut self.held, &mut steps);
                steps.push(Self::message(Style::Info, text));
            }
            Signal::Problem { text, .. } if Self::generic(&text) => {
                flush(&mut self.held, &mut steps);
                self.held = Some(text);
            }
            Signal::Problem { text, .. } => {
                flush(&mut self.held, &mut steps);
                steps.push(Self::message(Style::Error, text));
            }
            Signal::InfoQuery { query, .. } => {
                flush(&mut self.held, &mut steps);
                steps.push(Self::message(Style::Visible, query));
            }
            Signal::SecretInfoQuery { query, .. } => {
                flush(&mut self.held, &mut steps);
                steps.push(Self::message(Style::Secret, query));
            }
            Signal::VerificationComplete { .. } => {
                self.held = None;
                steps.push(Step::Finish(Finish::Complete));
            }
            Signal::VerificationFailed { .. } => {
                let expired =
                    self.held.take().as_deref() == Some(crate::runtime::auth::ACCOUNT_EXPIRED);
                steps.push(Step::Finish(Finish::Refused(if expired {
                    Refusal::Expired
                } else {
                    Refusal::Failed
                })));
            }
            Signal::ServiceUnavailable { message, .. } => {
                self.held = None;
                let max_tries = message == crate::runtime::auth::FINGERPRINT_MAX_TRIES;
                steps.push(Step::Finish(Finish::Refused(if max_tries {
                    Refusal::MaxTries
                } else {
                    Refusal::Unavailable
                })));
            }
            Signal::ConversationStopped { .. } | Signal::Reset => {
                self.held = None;
                steps.push(Step::Finish(Finish::Ended));
            }
            Signal::ConversationStarted { .. }
            | Signal::DefaultSessionNameChanged { .. }
            | Signal::SessionOpened { .. } => {}
        }
        steps
    }
}

/// The service a signal belongs to; `None` for session-wide signals.
fn signal_service(signal: &Signal) -> Option<&str> {
    match signal {
        Signal::ConversationStarted { service }
        | Signal::ConversationStopped { service }
        | Signal::Info { service, .. }
        | Signal::Problem { service, .. }
        | Signal::InfoQuery { service, .. }
        | Signal::SecretInfoQuery { service, .. }
        | Signal::ServiceUnavailable { service, .. }
        | Signal::VerificationFailed { service }
        | Signal::VerificationComplete { service }
        | Signal::SessionOpened { service, .. } => Some(service),
        Signal::Reset | Signal::DefaultSessionNameChanged { .. } => None,
    }
}

/// The private GLib thread every channel's D-Bus traffic runs on.
struct Dispatcher {
    context: glib::MainContext,
}

impl Dispatcher {
    fn get() -> Option<&'static Self> {
        static DISPATCHER: LazyLock<Option<Dispatcher>> = LazyLock::new(|| {
            let (ready, receive) = mpsc::sync_channel(1);
            std::thread::Builder::new()
                .name("tuna-gdm-reauth".into())
                .spawn(move || {
                    let context = glib::MainContext::new();
                    let _ = context.with_thread_default(|| {
                        // Owned (acquired) from here: invokes from other
                        // threads queue for this loop, never run inline.
                        let _ = ready.send(context.clone());
                        loop {
                            context.iteration(true);
                        }
                    });
                })
                .ok()?;
            receive
                .recv_timeout(CALL_TIMEOUT)
                .ok()
                .map(|context| Dispatcher { context })
        });
        DISPATCHER.as_ref()
    }

    /// Run a future on the GLib thread and wait for its result, both
    /// bounded by `timeout`; `None` when it did not finish in time.
    fn run<T, F>(&self, timeout: Duration, make: impl FnOnce() -> F + Send + 'static) -> Option<T>
    where
        T: Send + 'static,
        F: Future<Output = T> + 'static,
    {
        let (send, receive) = mpsc::sync_channel(1);
        let context = self.context.clone();
        self.context.invoke(move || {
            context.spawn_local(async move {
                if let Ok(value) = glib::future_with_timeout(timeout, make()).await {
                    let _ = send.send(value);
                }
            });
        });
        receive
            .recv_timeout(timeout + Duration::from_millis(250))
            .ok()
    }
}

/// A conversation's end of its service's signals: a queue plus a wake.
#[derive(Clone)]
struct Route {
    events: mpsc::SyncSender<Event>,
    wake: Arc<UnixStream>,
}

impl Route {
    fn deliver(&self, event: Event) {
        // A full queue (a peer flooding signals) drops the event: nothing
        // completes, and the conversation ends at its deadline.
        let _ = self.events.try_send(event);
        let _ = (&*self.wake).write(&[1]);
    }
}

type Routes = Arc<Mutex<HashMap<String, Route>>>;

fn deliver(routes: &Routes, event: Event) {
    let Ok(routes) = routes.lock() else {
        return;
    };
    let service = match &event {
        Event::Signal(signal) => signal_service(signal).map(str::to_owned),
        Event::Closed => None,
    };
    match service {
        Some(service) => {
            if let Some(route) = routes.get(&service) {
                route.deliver(event);
            }
        }
        None => routes
            .values()
            .for_each(|route| route.deliver(event.clone())),
    }
}

/// One open reauthentication channel, shared by the session's
/// conversations; closed when the last one lets it go.
struct Channel {
    dispatcher: &'static Dispatcher,
    connection: gio::DBusConnection,
    subscription: Mutex<Option<tuna_gdm::Subscription>>,
    routes: Routes,
    closed: Arc<AtomicBool>,
}

impl Channel {
    fn call(&self, call: Call) -> Result<(), String> {
        let connection = self.connection.clone();
        let (interface, method, args) = call.wire();
        self.dispatcher
            .run(CALL_TIMEOUT, move || async move {
                connection
                    .call_future(
                        None,
                        tuna_gdm::SESSION_PATH,
                        interface,
                        method,
                        Some(&args),
                        None,
                        gio::DBusCallFlags::NONE,
                        CALL_TIMEOUT.as_millis() as i32,
                    )
                    .await
                    .map(drop)
                    .map_err(|error| error.message().to_owned())
            })
            .unwrap_or_else(|| Err(format!("{method} timed out")))
    }
}

impl Drop for Channel {
    fn drop(&mut self) {
        let subscription = self.subscription.lock().ok().and_then(|mut s| s.take());
        let connection = self.connection.clone();
        self.dispatcher.context.invoke(move || {
            drop(subscription);
            connection.close(None::<&gio::Cancellable>, |_| {});
        });
    }
}

/// Where channels come from: GDM's manager on the system bus, or (tests)
/// a manager at a fixed peer-to-peer address. Both backends share one, so
/// they share the channel.
pub struct Source {
    manager: Option<String>,
    started_by_gdm: Mutex<Option<bool>>,
    shared: Mutex<Weak<Channel>>,
}

impl Source {
    /// GDM's manager on the system bus; whether GDM started the session
    /// comes from logind.
    pub fn system() -> Arc<Self> {
        static SYSTEM: LazyLock<Arc<Source>> = LazyLock::new(|| {
            Arc::new(Source {
                manager: None,
                started_by_gdm: Mutex::new(None),
                shared: Mutex::new(Weak::new()),
            })
        });
        SYSTEM.clone()
    }

    /// Trusted Rust fixture seam: a manager serving
    /// `OpenReauthenticationChannel` at a peer-to-peer `address`, and a
    /// fixed answer to whether GDM started the session.
    #[doc(hidden)]
    pub fn with_manager(address: impl Into<String>, started_by_gdm: bool) -> Arc<Self> {
        Arc::new(Self {
            manager: Some(address.into()),
            started_by_gdm: Mutex::new(Some(started_by_gdm)),
            shared: Mutex::new(Weak::new()),
        })
    }

    /// Whether GDM started this session, asked of logind once it answers.
    fn started_by_gdm(&self, dispatcher: &Dispatcher) -> bool {
        if let Some(known) = self.started_by_gdm.lock().ok().and_then(|known| *known) {
            return known;
        }
        let service = dispatcher
            .run(CALL_TIMEOUT, || async { logind_service().await.ok() })
            .flatten();
        let Some(service) = service else {
            return false;
        };
        let started = started_by_gdm(Some(&service));
        if let Ok(mut known) = self.started_by_gdm.lock() {
            *known = Some(started);
        }
        started
    }

    /// The open channel, or a newly opened one; `None` when GDM did not
    /// open one for `user` in time.
    fn channel(&self, dispatcher: &'static Dispatcher, user: &str) -> Option<Arc<Channel>> {
        let mut shared = self.shared.lock().ok()?;
        if let Some(channel) = shared
            .upgrade()
            .filter(|channel| !channel.closed.load(Ordering::Acquire))
        {
            return Some(channel);
        }
        let routes: Routes = Arc::default();
        let closed = Arc::new(AtomicBool::new(false));
        let (manager, user) = (self.manager.clone(), user.to_owned());
        let (signal_routes, closed_routes, closed_flag) =
            (routes.clone(), routes.clone(), closed.clone());
        let (connection, subscription) = dispatcher
            .run(3 * CALL_TIMEOUT, move || async move {
                let bus = match manager {
                    None => gio::bus_get_future(gio::BusType::System).await,
                    Some(address) => tuna_gdm::connect(&address).await,
                }
                .ok()?;
                let address = tuna_gdm::open_reauthentication_channel(&bus, &user, CALL_TIMEOUT)
                    .await
                    .ok()
                    .filter(|address| !address.is_empty())?;
                let connection = tuna_gdm::connect(&address).await.ok()?;
                // Subscribed on this thread, so signals dispatch here.
                let subscription = tuna_gdm::subscribe(&connection, move |signal| {
                    deliver(&signal_routes, Event::Signal(signal))
                });
                connection.connect_closed(move |_, _, _| {
                    closed_flag.store(true, Ordering::Release);
                    deliver(&closed_routes, Event::Closed);
                });
                Some((connection, subscription))
            })
            .flatten()?;
        let channel = Arc::new(Channel {
            dispatcher,
            connection,
            subscription: Mutex::new(Some(subscription)),
            routes,
            closed,
        });
        *shared = Arc::downgrade(&channel);
        Some(channel)
    }
}

/// logind's `Service` for this process's session, found as
/// [`crate::logind_session::current_session_path`] finds the session.
async fn logind_service() -> Result<String, glib::Error> {
    const LOGIND: &str = "org.freedesktop.login1";
    let system = gio::bus_get_future(gio::BusType::System).await?;
    let (method, args) = match std::env::var("XDG_SESSION_ID") {
        Ok(id) if !id.is_empty() => ("GetSession", (id,).to_variant()),
        _ => ("GetSessionByPID", (std::process::id(),).to_variant()),
    };
    let timeout = CALL_TIMEOUT.as_millis() as i32;
    let reply = system
        .call_future(
            Some(LOGIND),
            "/org/freedesktop/login1",
            "org.freedesktop.login1.Manager",
            method,
            Some(&args),
            glib::VariantTy::new("(o)").ok(),
            gio::DBusCallFlags::NONE,
            timeout,
        )
        .await?;
    let path = reply.child_value(0).str().unwrap_or_default().to_owned();
    let reply = system
        .call_future(
            Some(LOGIND),
            &path,
            "org.freedesktop.DBus.Properties",
            "Get",
            Some(&("org.freedesktop.login1.Session", "Service").to_variant()),
            glib::VariantTy::new("(v)").ok(),
            gio::DBusCallFlags::NONE,
            timeout,
        )
        .await?;
    Ok(reply
        .child_value(0)
        .as_variant()
        .and_then(|service| service.str().map(str::to_owned))
        .unwrap_or_default())
}

/// One backend's reauthentication: its GDM service on a shared [`Source`].
#[derive(Clone)]
pub struct Reauth {
    service: &'static str,
    source: Arc<Source>,
}

/// What [`Reauth::start`] decided.
pub(crate) enum Start {
    Gdm(Conversation),
    Helper,
    Refuse,
}

impl Reauth {
    /// The password conversation's reauthentication (`gdm-password`).
    pub fn password(source: Arc<Source>) -> Self {
        Self {
            service: PASSWORD_SERVICE,
            source,
        }
    }

    /// The fingerprint conversation's reauthentication (`gdm-fingerprint`).
    pub fn fingerprint(source: Arc<Source>) -> Self {
        Self {
            service: FINGERPRINT_SERVICE,
            source,
        }
    }

    /// Open (or share) the channel and begin this service's verification of
    /// `user`, or choose the private helper or a refusal ([`select`]).
    pub(crate) fn start(&self, user: &str) -> Start {
        let start = self.choose(user);
        if std::env::var_os("TUNA_LOCK_TRACE").is_some() {
            let backend = match &start {
                Start::Gdm(_) => "gdm",
                Start::Helper => "helper",
                Start::Refuse => "refused",
            };
            eprintln!(
                "tuna-compositor: lock verification backend {}",
                serde_json::json!({ "service": self.service, "backend": backend })
            );
        }
        start
    }

    fn choose(&self, user: &str) -> Start {
        let Some(dispatcher) = Dispatcher::get() else {
            return Start::Refuse;
        };
        let Some(channel) = self.source.channel(dispatcher, user) else {
            return match select(self.source.started_by_gdm(dispatcher), false) {
                Selection::Helper => Start::Helper,
                _ => Start::Refuse,
            };
        };
        match Conversation::begin(channel, self.service, user) {
            Some(conversation) => Start::Gdm(conversation),
            // GDM is reachable: never fall back to the private helper.
            None => Start::Refuse,
        }
    }
}

/// One service's verification on the channel. Dropping it stops reading
/// the service's signals; the channel closes with its last conversation.
pub(crate) struct Conversation {
    channel: Arc<Channel>,
    service: &'static str,
    events: mpsc::Receiver<Event>,
    wake: UnixStream,
    mapper: Mapper,
}

impl Conversation {
    fn begin(channel: Arc<Channel>, service: &'static str, user: &str) -> Option<Self> {
        let (wake, wake_write) = UnixStream::pair().ok()?;
        wake.set_nonblocking(true).ok()?;
        wake_write.set_nonblocking(true).ok()?;
        let (send, events) = mpsc::sync_channel(QUEUE);
        {
            let mut routes = channel.routes.lock().ok()?;
            if routes.contains_key(service) {
                return None;
            }
            routes.insert(
                service.to_owned(),
                Route {
                    events: send,
                    wake: Arc::new(wake_write),
                },
            );
        }
        let conversation = Self {
            channel,
            service,
            events,
            wake,
            mapper: Mapper::new(service),
        };
        conversation
            .channel
            .call(Call::BeginVerificationForUser {
                service: service.to_owned(),
                user: user.to_owned(),
            })
            .ok()?;
        Some(conversation)
    }

    /// The next event of this service, or `None` once `cancel` fired,
    /// `shutdown` is set or `deadline` passed.
    pub(crate) fn next(
        &mut self,
        cancel: &Cancellation,
        shutdown: &AtomicBool,
        deadline: Instant,
    ) -> Option<Event> {
        wait_event(&self.events, &self.wake, cancel, shutdown, deadline)
    }

    /// Map an event onto relay steps.
    pub(crate) fn map(&mut self, event: Event) -> Vec<Step> {
        self.mapper.map(event)
    }

    /// GDM's `AnswerQuery` for this service.
    pub(crate) fn answer(&self, answer: &str) -> Result<(), String> {
        self.channel.call(Call::AnswerQuery {
            service: self.service.to_owned(),
            answer: answer.to_owned(),
        })
    }

    /// GDM's `Cancel`: the session's verification stops (GNOME's unlock
    /// dialog cancels the whole verifier the same way).
    pub(crate) fn cancel(&self) {
        let _ = self.channel.call(Call::Cancel);
    }
}

impl Drop for Conversation {
    fn drop(&mut self) {
        if let Ok(mut routes) = self.channel.routes.lock() {
            routes.remove(self.service);
        }
    }
}

/// The next queued event, waking on its route's wake or the attempt's
/// cancellation; `None` once `cancel` fired, `shutdown` is set or
/// `deadline` passed. A route dropped by the channel reads as closed.
fn wait_event(
    events: &mpsc::Receiver<Event>,
    wake: &UnixStream,
    cancel: &Cancellation,
    shutdown: &AtomicBool,
    deadline: Instant,
) -> Option<Event> {
    let cancel_fd = cancel.wake_fd()?;
    loop {
        let mut buffer = [0; 64];
        while (&*wake).read(&mut buffer).is_ok_and(|count| count > 0) {}
        let _ = cancel.drain();
        if cancel.is_canceled() || shutdown.load(Ordering::Acquire) {
            return None;
        }
        match events.try_recv() {
            Ok(event) => return Some(event),
            Err(mpsc::TryRecvError::Disconnected) => return Some(Event::Closed),
            Err(mpsc::TryRecvError::Empty) => {}
        }
        let wait = deadline.checked_duration_since(Instant::now())?;
        let mut fds = [
            rustix::event::PollFd::new(wake, rustix::event::PollFlags::IN),
            rustix::event::PollFd::new(&cancel_fd, rustix::event::PollFlags::IN),
        ];
        let _ = crate::wake::poll_fds(&mut fds, Some(wait.max(Duration::from_millis(1))));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::auth::{
        ACCOUNT_EXPIRED, FINGERPRINT_FAILED, FINGERPRINT_MAX_TRIES, PASSWORD_FAILED,
    };

    fn signal(signal: Signal) -> Event {
        Event::Signal(signal)
    }

    fn relayed(style: Style, text: &str) -> Step {
        Step::Relay(Message {
            index: 0,
            style,
            text: text.into(),
        })
    }

    #[test]
    fn gdm_signals_map_onto_relay_messages_for_their_own_service() {
        let mut mapper = Mapper::new(PASSWORD_SERVICE);
        let s = |text: &str| (PASSWORD_SERVICE.to_owned(), text.to_owned());
        let (service, text) = s("Welcome");
        assert_eq!(
            mapper.map(signal(Signal::Info { service, text })),
            vec![relayed(Style::Info, "Welcome")]
        );
        let (service, query) = s("Password: ");
        assert_eq!(
            mapper.map(signal(Signal::SecretInfoQuery { service, query })),
            vec![relayed(Style::Secret, "Password: ")]
        );
        let (service, query) = s("Verification code: ");
        assert_eq!(
            mapper.map(signal(Signal::InfoQuery { service, query })),
            vec![relayed(Style::Visible, "Verification code: ")]
        );
        let (service, text) = s("The account is locked due to 3 failed logins.");
        assert_eq!(
            mapper.map(signal(Signal::Problem { service, text })),
            vec![relayed(
                Style::Error,
                "The account is locked due to 3 failed logins."
            )]
        );
        // Another service's signals belong to its own conversation.
        assert!(mapper
            .map(signal(Signal::SecretInfoQuery {
                service: FINGERPRINT_SERVICE.into(),
                query: "Password: ".into()
            }))
            .is_empty());
        assert!(mapper
            .map(signal(Signal::VerificationComplete {
                service: FINGERPRINT_SERVICE.into()
            }))
            .is_empty());
        assert!(mapper
            .map(signal(Signal::ConversationStarted {
                service: PASSWORD_SERVICE.into()
            }))
            .is_empty());
    }

    #[test]
    fn only_verification_complete_for_the_service_completes() {
        let mut mapper = Mapper::new(PASSWORD_SERVICE);
        assert_eq!(
            mapper.map(signal(Signal::VerificationComplete {
                service: PASSWORD_SERVICE.into()
            })),
            vec![Step::Finish(Finish::Complete)]
        );
        for ended in [
            signal(Signal::Reset),
            signal(Signal::ConversationStopped {
                service: PASSWORD_SERVICE.into(),
            }),
            Event::Closed,
        ] {
            assert_eq!(
                Mapper::new(PASSWORD_SERVICE).map(ended),
                vec![Step::Finish(Finish::Ended)]
            );
        }
    }

    #[test]
    fn failures_keep_gdms_class_and_the_runtime_words_them() {
        // The generic message directly before the failure is not relayed:
        // the runtime reports the failed conversation itself.
        let mut mapper = Mapper::new(PASSWORD_SERVICE);
        assert!(mapper
            .map(signal(Signal::Problem {
                service: PASSWORD_SERVICE.into(),
                text: PASSWORD_FAILED.into()
            }))
            .is_empty());
        assert_eq!(
            mapper.map(signal(Signal::VerificationFailed {
                service: PASSWORD_SERVICE.into()
            })),
            vec![Step::Finish(Finish::Refused(Refusal::Failed))]
        );
        // An expired account is classed as one.
        let mut mapper = Mapper::new(FINGERPRINT_SERVICE);
        mapper.map(signal(Signal::Problem {
            service: FINGERPRINT_SERVICE.into(),
            text: ACCOUNT_EXPIRED.into(),
        }));
        assert_eq!(
            mapper.map(signal(Signal::VerificationFailed {
                service: FINGERPRINT_SERVICE.into()
            })),
            vec![Step::Finish(Finish::Refused(Refusal::Expired))]
        );
        // A held generic message followed by anything else is relayed.
        let mut mapper = Mapper::new(FINGERPRINT_SERVICE);
        mapper.map(signal(Signal::Problem {
            service: FINGERPRINT_SERVICE.into(),
            text: FINGERPRINT_FAILED.into(),
        }));
        assert_eq!(
            mapper.map(signal(Signal::Info {
                service: FINGERPRINT_SERVICE.into(),
                text: "Place your finger on the reader".into()
            })),
            vec![
                relayed(Style::Error, FINGERPRINT_FAILED),
                relayed(Style::Info, "Place your finger on the reader")
            ]
        );
        // GDM's too-many-retries is a service-unavailable with its message.
        assert_eq!(
            Mapper::new(FINGERPRINT_SERVICE).map(signal(Signal::ServiceUnavailable {
                service: FINGERPRINT_SERVICE.into(),
                message: FINGERPRINT_MAX_TRIES.into()
            })),
            vec![Step::Finish(Finish::Refused(Refusal::MaxTries))]
        );
        assert_eq!(
            Mapper::new(FINGERPRINT_SERVICE).map(signal(Signal::ServiceUnavailable {
                service: FINGERPRINT_SERVICE.into(),
                message: "No reader".into()
            })),
            vec![Step::Finish(Finish::Refused(Refusal::Unavailable))]
        );
    }

    #[test]
    fn gdm_is_used_when_reachable_and_required_when_it_started_the_session() {
        assert_eq!(select(false, true), Selection::Gdm);
        assert_eq!(select(true, true), Selection::Gdm);
        // No GDM: the private helper, unchanged.
        assert_eq!(select(false, false), Selection::Helper);
        // A GDM session never falls back to the private helper.
        assert_eq!(select(true, false), Selection::Refuse);
        for service in [
            "gdm-password",
            "gdm-autologin",
            "gdm-fingerprint",
            "gdm-smartcard",
        ] {
            assert!(started_by_gdm(Some(service)), "{service}");
        }
        for service in [None, Some(""), Some("gdm-"), Some("login"), Some("greetd")] {
            assert!(!started_by_gdm(service), "{service:?}");
        }
    }

    #[test]
    fn session_wide_signals_reach_every_conversation_and_service_signals_one() {
        let routes: Routes = Arc::default();
        let mut receivers = Vec::new();
        for service in [PASSWORD_SERVICE, FINGERPRINT_SERVICE] {
            let (send, receive) = mpsc::sync_channel(QUEUE);
            let (read, write) = UnixStream::pair().unwrap();
            write.set_nonblocking(true).unwrap();
            routes.lock().unwrap().insert(
                service.into(),
                Route {
                    events: send,
                    wake: Arc::new(write),
                },
            );
            receivers.push((receive, read));
        }
        deliver(
            &routes,
            Event::Signal(Signal::SecretInfoQuery {
                service: PASSWORD_SERVICE.into(),
                query: "Password: ".into(),
            }),
        );
        deliver(&routes, Event::Signal(Signal::Reset));
        let password: Vec<_> = receivers[0].0.try_iter().collect();
        let fingerprint: Vec<_> = receivers[1].0.try_iter().collect();
        assert_eq!(password.len(), 2);
        assert_eq!(fingerprint, vec![Event::Signal(Signal::Reset)]);
    }

    #[test]
    fn a_cancelled_conversation_stops_waiting_at_once() {
        // The wait observes the attempt's own cancellation latch, bound to
        // its backend's wake as every admitted attempt is.
        let cancel = Cancellation::default();
        cancel
            .bind(crate::pam_cancel::WakeFd::new(Arc::new(AtomicBool::new(false))).unwrap())
            .unwrap();
        let (send, events) = mpsc::sync_channel(QUEUE);
        let (wake, write) = UnixStream::pair().unwrap();
        wake.set_nonblocking(true).unwrap();
        let shutdown = AtomicBool::new(false);
        let deadline = Instant::now() + Duration::from_secs(30);
        // A queued event is read first.
        Route {
            events: send.clone(),
            wake: Arc::new(write),
        }
        .deliver(Event::Signal(Signal::Reset));
        assert_eq!(
            wait_event(&events, &wake, &cancel, &shutdown, deadline),
            Some(Event::Signal(Signal::Reset))
        );
        let canceller = cancel.clone();
        let started = Instant::now();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            canceller.cancel();
        });
        assert_eq!(
            wait_event(&events, &wake, &cancel, &shutdown, deadline),
            None
        );
        assert!(started.elapsed() < Duration::from_secs(5));
        // An expired deadline ends the wait without an event.
        let fresh = Cancellation::default();
        fresh
            .bind(crate::pam_cancel::WakeFd::new(Arc::new(AtomicBool::new(false))).unwrap())
            .unwrap();
        assert_eq!(
            wait_event(&events, &wake, &fresh, &shutdown, Instant::now()),
            None
        );
        drop(send);
    }
}
