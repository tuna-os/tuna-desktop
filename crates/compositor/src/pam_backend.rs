//! Stable creator ownership for live private PAM helpers; deadlines never retire.
use crate::pam_cancel::{Cancellation, WakeFd};
use crate::pam_helper::{
    Answer, AnswerText, Event, Identity, Retirement, SelfImage, Style, Supervisor,
};
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tuna_shell_control::Secret;

const FREE: u8 = 0;
const BUSY: u8 = 1;
const POISONED: u8 = 2;
const TEXT_LIMIT: usize = 4096;
static LIVE: OnceLock<Option<Backend>> = OnceLock::new();

type Factory = Arc<
    dyn Fn(Identity, String, String, Duration, Cancellation) -> io::Result<Supervisor>
        + Send
        + Sync,
>;

/// Only Retired or a proven rejection before submitting this attempt can free it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verification {
    RejectedBeforeSpawn,
    Retired { authenticated: bool },
    Unretired,
}
impl Verification {
    pub fn authenticated(self) -> bool {
        matches!(
            self,
            Self::Retired {
                authenticated: true
            }
        )
    }
    pub fn may_retire(self) -> bool {
        !matches!(self, Self::Unretired)
    }
}
#[derive(Clone, Copy, Debug)]
enum Reply {
    Pending,
    Retired(bool),
    Poisoned,
}
/// Private owned credential buffer. Schema Secret only redacts Debug and has
/// no Drop; conversion moves its String, with no extra raw credential copy.
pub(crate) struct Credential {
    text: String,
    #[cfg(test)]
    dropped: Option<Arc<std::sync::atomic::AtomicUsize>>,
}
impl Credential {
    pub(crate) fn from_string(text: String) -> Self {
        Self {
            text,
            #[cfg(test)]
            dropped: None,
        }
    }
    pub(crate) fn from_schema(secret: Secret) -> Self {
        Self::from_string(secret.0)
    }
    pub(crate) fn as_str(&self) -> &str {
        &self.text
    }
    fn wipe(&mut self) {
        unsafe {
            crate::pam_helper::wipe(self.text.as_mut_vec());
        }
    }
    fn take_answer(&mut self) -> AnswerText {
        AnswerText::new(std::mem::take(&mut self.text))
    }
}
impl Drop for Credential {
    fn drop(&mut self) {
        self.wipe();
        #[cfg(test)]
        if let Some(dropped) = &self.dropped {
            dropped.fetch_add(1, Ordering::SeqCst);
        }
    }
}
impl std::fmt::Debug for Credential {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.write_str("Credential(<redacted>)")
    }
}
/// Publish queue state before the owned per-conversation calloop wake. On
/// unwind/receiver loss, sender closure precedes the disconnect notification.
struct ReplyPublisher {
    sender: Option<mpsc::Sender<Reply>>,
    notice: Option<crate::wake::Wake>,
}
impl ReplyPublisher {
    fn send(&self, reply: Reply) -> Result<(), mpsc::SendError<Reply>> {
        let result = self
            .sender
            .as_ref()
            .expect("owned reply publisher")
            .send(reply);
        if let Some(notice) = &self.notice {
            notice.signal();
        }
        result
    }
}
impl Drop for ReplyPublisher {
    fn drop(&mut self) {
        drop(self.sender.take());
        if let Some(notice) = &self.notice {
            notice.signal();
        }
    }
}
struct Job {
    serial: u64,
    service: String,
    user: String,
    password: Credential,
    relay: Option<crate::pam_conversation::Relay>,
    cancel: Cancellation,
    reply: ReplyPublisher,
}
enum Command {
    Attempt(Box<Job>),
    Shutdown,
}
#[derive(Clone)]
pub struct Backend {
    sender: mpsc::SyncSender<Command>,
    state: Arc<AtomicU8>,
    serial: Arc<AtomicU64>,
    shutdown: Arc<AtomicBool>,
    wake: Arc<WakeFd>,
    owner: Arc<Mutex<Option<JoinHandle<()>>>>,
}
pub struct Ticket {
    pub serial: u64,
    reply: mpsc::Receiver<Reply>,
    cancel: Cancellation,
    deadline: Instant,
    denied: bool,
}
/// Pollable verifier result plus one bounded challenge channel. A retired true
/// still needs the runtime's exact live-origin/common Slot epoch grant.
pub struct ConversationTicket {
    ticket: Ticket,
    receiver: crate::pam_conversation::Receiver,
    outcome: Option<Verification>,
}
impl ConversationTicket {
    /// Register this owned source once in the runtime loop. Queue publication
    /// precedes its wake; the source is not an authorization or retirement proof.
    pub fn take_notice_source(&mut self) -> Option<calloop::ping::PingSource> {
        self.receiver.take_notice_source()
    }
    /// Real shared idle/total deadline. After cancellation, wait for the owned
    /// terminal wake instead of scheduling the expired deadline forever.
    pub fn next_deadline(&self) -> Option<Instant> {
        if self.outcome.is_some() || self.ticket.denied || self.ticket.cancel.is_canceled() {
            None
        } else {
            self.receiver.next_deadline()
        }
    }
    pub fn serial(&self) -> u64 {
        self.ticket.serial
    }
    pub fn try_challenge(&self) -> Result<crate::pam_conversation::Challenge, mpsc::TryRecvError> {
        if self.ticket.cancel.is_canceled() || !self.receiver.timely_at(Instant::now()) {
            return Err(mpsc::TryRecvError::Disconnected);
        }
        self.receiver.try_challenge()
    }
    pub fn cancel(&mut self) {
        self.ticket.denied = true;
        self.ticket.cancel.cancel();
        if matches!(self.outcome, Some(Verification::Retired { .. })) {
            self.outcome = Some(Verification::Retired {
                authenticated: false,
            });
        }
    }
    /// Cancellation or deadline expiry is denial, never a retirement receipt.
    pub fn try_outcome(&mut self) -> Option<Verification> {
        if Instant::now() >= self.ticket.deadline
            || !self.receiver.timely_at(Instant::now())
            || self.ticket.cancel.is_canceled()
        {
            self.cancel();
        }
        if self.outcome.is_some() {
            return self.outcome;
        }
        // The trusted producer publishes at most one Pending followed by
        // one terminal reply. PingSource coalesces them, so drain both in a
        // bounded poll rather than sleeping with Retired already queued.
        for _ in 0..2 {
            let outcome = match self.ticket.reply.try_recv() {
                Ok(Reply::Retired(authenticated)) => {
                    let observed = Instant::now();
                    self.ticket.denied |=
                        self.ticket.cancel.is_canceled() || !self.receiver.timely_at(observed);
                    match self.ticket.retired_at(authenticated, observed) {
                        Initial::Finished(result) => Some(result),
                        Initial::Pending => None,
                    }
                }
                Ok(Reply::Pending) => {
                    self.cancel();
                    continue;
                }
                Ok(Reply::Poisoned) | Err(mpsc::TryRecvError::Disconnected) => {
                    Some(Verification::Unretired)
                }
                Err(mpsc::TryRecvError::Empty) => None,
            };
            self.outcome = outcome;
            return outcome;
        }
        None
    }
}
pub enum Initial {
    Finished(Verification),
    Pending,
}
impl Ticket {
    /// A facade may return denial now, while this ticket's backend stays owned.
    pub fn initial(&mut self) -> Initial {
        let Some(remaining) = self.deadline.checked_duration_since(Instant::now()) else {
            self.denied = true;
            self.cancel.cancel();
            return Initial::Pending;
        };
        match self.reply.recv_timeout(remaining) {
            Ok(Reply::Retired(authenticated)) => self.retired_at(authenticated, Instant::now()),
            Ok(Reply::Pending) | Err(mpsc::RecvTimeoutError::Timeout) => {
                self.denied = true;
                self.cancel.cancel();
                Initial::Pending
            }
            Ok(Reply::Poisoned) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                Initial::Finished(Verification::Unretired)
            }
        }
    }
    // Physical retirement and timely authentication are separate facts. The
    // receiver can be descheduled across its deadline AFTER a reply is queued.
    fn retired_at(&mut self, authenticated: bool, observed: Instant) -> Initial {
        self.denied |= observed >= self.deadline;
        if self.denied {
            self.cancel.cancel();
        }
        Initial::Finished(Verification::Retired {
            authenticated: authenticated && !self.denied,
        })
    }
    /// Off the event loop: after denial, wait for actual retirement or poison.
    /// Receiver loss and a returned facade never release backend admission.
    pub fn wait_retired(mut self, mut pending: impl FnMut()) -> Verification {
        if !self.denied {
            match self.initial() {
                Initial::Finished(outcome) => return outcome,
                Initial::Pending => pending(),
            }
        }
        loop {
            match self.reply.recv() {
                Ok(Reply::Retired(_)) => {
                    return Verification::Retired {
                        authenticated: false,
                    };
                }
                Ok(Reply::Pending) => {}
                Ok(Reply::Poisoned) | Err(_) => return Verification::Unretired,
            }
        }
    }
}
impl Drop for Ticket {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
struct OwnerGuard(Arc<AtomicU8>);
impl Drop for OwnerGuard {
    fn drop(&mut self) {
        // Includes panic/receiver failure: no replacement owner or invented reap.
        self.0.store(POISONED, Ordering::Release);
    }
}
impl Backend {
    pub fn shutdown_live() {
        if let Some(Some(backend)) = LIVE.get() {
            backend.shutdown();
        }
    }
    pub fn live() -> Option<Self> {
        LIVE.get_or_init(|| {
            let image = SelfImage::capture().ok()?;
            Self::from_factory(Arc::new(move |identity, service, user, budget, cancel| {
                if budget <= crate::pam::DEADLINE {
                    image.spawn_cancellable(identity, service, user, budget, cancel)
                } else {
                    image.spawn_conversation(identity, service, user, budget, cancel)
                }
            }))
            .ok()
        })
        .clone()
    }
    /// Trusted Rust fixture seam, not a live executable configuration surface.
    /// The factory runs on this stable creator; callers must admit its binary.
    #[doc(hidden)]
    pub fn with_trusted_factory(
        factory: impl Fn(Identity, String, String, Duration, Cancellation) -> io::Result<Supervisor>
            + Send
            + Sync
            + 'static,
    ) -> io::Result<Self> {
        Self::from_factory(Arc::new(factory))
    }
    fn from_factory(factory: Factory) -> io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel(1);
        let state = Arc::new(AtomicU8::new(FREE));
        let shutdown = Arc::new(AtomicBool::new(false));
        let wake = WakeFd::new(shutdown.clone())?;
        let owner_state = state.clone();
        let owner_shutdown = shutdown.clone();
        let owner = std::thread::Builder::new()
            .name("tuna-pam-owner".into())
            .spawn(move || {
                let _guard = OwnerGuard(owner_state.clone());
                while let Ok(command) = receiver.recv() {
                    match command {
                        Command::Shutdown => break,
                        Command::Attempt(job) => {
                            if !run_job(*job, &factory, &owner_shutdown, &owner_state) {
                                break;
                            }
                            if owner_shutdown.load(Ordering::Acquire) {
                                break;
                            }
                        }
                    }
                }
            })?;
        Ok(Self {
            sender,
            state,
            shutdown,
            wake,
            serial: Arc::new(AtomicU64::new(0)),
            owner: Arc::new(Mutex::new(Some(owner))),
        })
    }
    pub fn begin(
        &self,
        service: String,
        user: String,
        password: Secret,
        cancel: Cancellation,
    ) -> Result<Ticket, Verification> {
        self.begin_credential(service, user, Credential::from_schema(password), cancel)
    }
    pub(crate) fn begin_credential(
        &self,
        service: String,
        user: String,
        password: Credential,
        cancel: Cancellation,
    ) -> Result<Ticket, Verification> {
        self.begin_mode(service, user, password, cancel, None)
    }
    /// Internal challenge relay. The trusted runtime supplies its admitted common
    /// Slot identity; this starts verification only and never grants unlock.
    pub fn begin_conversation(
        &self,
        service: String,
        user: String,
        binding: crate::pam_conversation::Binding,
        policy: crate::pam_conversation::Policy,
        cancel: Cancellation,
    ) -> Result<ConversationTicket, Verification> {
        let (relay, receiver) = crate::pam_conversation::channel(binding, policy, cancel.clone())
            .map_err(|_| Verification::RejectedBeforeSpawn)?;
        let ticket = self.begin_mode(
            service,
            user,
            Credential::from_string(String::new()),
            cancel,
            Some(relay),
        )?;
        Ok(ConversationTicket {
            ticket,
            receiver,
            outcome: None,
        })
    }
    fn begin_mode(
        &self,
        service: String,
        user: String,
        password: Credential,
        cancel: Cancellation,
        relay: Option<crate::pam_conversation::Relay>,
    ) -> Result<Ticket, Verification> {
        if service.is_empty()
            || service.len() > 128
            || !service
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
            || user.is_empty()
            || user.len() > TEXT_LIMIT
            || user.contains('\0')
            || password.as_str().len() > TEXT_LIMIT
            || password.as_str().contains('\0')
            || self.shutdown.load(Ordering::Acquire)
            || self
                .state
                .compare_exchange(FREE, BUSY, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return Err(Verification::RejectedBeforeSpawn);
        }
        if !crate::auth_owner::advance_epoch(&self.serial) {
            self.state.store(POISONED, Ordering::Release);
            return Err(Verification::RejectedBeforeSpawn);
        }
        let serial = self.serial.load(Ordering::Acquire);
        if cancel.bind(self.wake.clone()).is_err() {
            // This exact latch was already bound: reject before submission/spawn.
            self.state.store(FREE, Ordering::Release);
            return Err(Verification::RejectedBeforeSpawn);
        }
        let (reply, receive) = mpsc::channel();
        let deadline = relay.as_ref().map_or_else(
            || Instant::now() + crate::pam::DEADLINE,
            |relay| relay.total_deadline(),
        );
        let notice = relay.as_ref().map(|relay| relay.notice());
        let job = Job {
            serial,
            service,
            user,
            password,
            relay,
            cancel: cancel.clone(),
            reply: ReplyPublisher {
                sender: Some(reply),
                notice,
            },
        };
        if self
            .sender
            .try_send(Command::Attempt(Box::new(job)))
            .is_err()
        {
            self.state.store(POISONED, Ordering::Release);
            return Err(Verification::RejectedBeforeSpawn);
        }
        Ok(Ticket {
            serial,
            reply: receive,
            cancel,
            deadline,
            denied: false,
        })
    }
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
        let _ = self.wake.signal();
        let _ = self.sender.try_send(Command::Shutdown);
    }
    /// An already-finished owner may be joined for cleanup. This does not promise
    /// an OS/TLS termination deadline or certify a helper retirement.
    pub fn join_if_finished(&self) {
        if let Ok(mut owner) = self.owner.lock() {
            if owner.as_ref().is_some_and(JoinHandle::is_finished) {
                if let Some(owner) = owner.take() {
                    let _ = owner.join();
                }
            }
        }
    }
}
fn run_job(mut job: Job, factory: &Factory, shutdown: &AtomicBool, state: &AtomicU8) -> bool {
    let identity = Identity {
        nonce: rand::random(),
        request: job.serial,
        generation: job.serial,
        uid: unsafe { libc::getuid() },
    };
    let mut supervisor = match factory(
        identity,
        job.service,
        job.user.clone(),
        job.relay
            .as_ref()
            .map_or(crate::pam::DEADLINE, |relay| relay.total()),
        job.cancel.clone(),
    ) {
        Ok(supervisor) => supervisor,
        Err(_) => {
            // Once spawn was attempted, lack of an owned result is not proof of
            // physical retirement. Keep admission poisoned, never retry a factory.
            let _ = job.reply.send(Reply::Poisoned);
            return false;
        }
    };
    let mut secret = Some(job.password);
    let mut secret_used = false;
    let mut visible_used = false;
    let mut rounds = 0;
    loop {
        if job.cancel.is_canceled() || shutdown.load(Ordering::Acquire) {
            break;
        }
        let phase_deadline = if let Some(relay) = &job.relay {
            let Ok(deadline) = relay.phase() else {
                break;
            };
            if supervisor.set_phase_deadline(deadline).is_err() {
                break;
            }
            Some(deadline)
        } else {
            None
        };
        match supervisor.next_event() {
            Ok(Event::Finished { authenticated }) => {
                if supervisor.retirement() != Retirement::Retired {
                    break;
                }
                let accepted = authenticated
                    && !job.cancel.is_canceled()
                    && !shutdown.load(Ordering::Acquire)
                    && job.relay.as_ref().is_none_or(|relay| relay.timely())
                    && phase_deadline.is_none_or(|deadline| Instant::now() < deadline);
                drop(secret);
                // Release only after actual Retired, before telling the caller.
                state.store(FREE, Ordering::Release);
                let _ = job.reply.send(Reply::Retired(accepted));
                return true;
            }
            Ok(Event::Challenge { sequence, messages }) => {
                rounds += 1;
                if rounds
                    > if job.relay.is_some() {
                        64
                    } else {
                        crate::unlock::MAX_AUTH_ROUNDS
                    }
                {
                    break;
                }
                if let Some(relay) = &mut job.relay {
                    if relay.answer(&mut supervisor, sequence, messages).is_err() {
                        break;
                    }
                    continue;
                }
                // Validate the complete batch before constructing any answer
                // or transferring the one owned password into AnswerText.
                if !supported_messages(&messages, secret_used, visible_used) {
                    break;
                }
                let mut answers = Vec::with_capacity(messages.len());
                for message in messages {
                    let text = match message.style {
                        Style::Secret => {
                            secret_used = true;
                            let Some(mut password) = secret.take() else {
                                unreachable!("validated single secret")
                            };
                            Some(password.take_answer())
                        }
                        Style::Visible => {
                            visible_used = true;
                            Some(AnswerText::new(job.user.clone()))
                        }
                        Style::Info | Style::Error => None,
                    };
                    answers.push(Answer {
                        index: message.index,
                        text,
                    });
                }
                if supervisor.answer(sequence, answers).is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    drop(secret);
    let _ = job.reply.send(Reply::Pending);
    let mut retirement = supervisor.cancel();
    // SAME creator thread and SAME Supervisor are retained through quarantine.
    // No receiver/UI lifetime is a release condition.
    while retirement != Retirement::Retired {
        std::thread::sleep(Duration::from_millis(50));
        retirement = supervisor.retry_retirement();
    }
    state.store(FREE, Ordering::Release);
    let _ = job.reply.send(Reply::Retired(false));
    true
}

fn supported_messages(
    messages: &[crate::pam_helper::Message],
    secret: bool,
    visible: bool,
) -> bool {
    !messages.iter().any(|message| {
        matches!(message.style, Style::Secret | Style::Visible)
            && tuna_greeter::model::is_fingerprint_prompt(&message.text)
    }) && supported_styles(
        messages.iter().map(|message| message.style),
        secret,
        visible,
    )
}

fn supported_styles(
    styles: impl Iterator<Item = Style>,
    mut secret: bool,
    mut visible: bool,
) -> bool {
    for style in styles {
        match style {
            Style::Secret if !secret => secret = true,
            Style::Visible if !secret && !visible => visible = true,
            Style::Info | Style::Error => {}
            _ => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn owned_reply_wake_observes_publication_disconnect_and_no_idle_spin() {
        let (notice, source) = crate::wake::Wake::new().unwrap();
        let (sender, receiver) = mpsc::channel();
        let publisher = ReplyPublisher {
            sender: Some(sender),
            notice: Some(notice),
        };
        let mut event_loop = calloop::EventLoop::<Vec<Option<Reply>>>::try_new().unwrap();
        event_loop
            .handle()
            .insert_source(source, move |_, _, seen| match receiver.try_recv() {
                Ok(reply) => seen.push(Some(reply)),
                Err(mpsc::TryRecvError::Disconnected) => seen.push(None),
                Err(mpsc::TryRecvError::Empty) => panic!("wake before publication"),
            })
            .unwrap();
        let mut seen = Vec::new();
        for reply in [Reply::Pending, Reply::Retired(false), Reply::Poisoned] {
            publisher.send(reply).unwrap();
            event_loop
                .dispatch(Some(Duration::from_secs(2)), &mut seen)
                .unwrap();
            assert!(seen.last().unwrap().is_some());
        }
        assert!(matches!(
            seen.as_slice(),
            [
                Some(Reply::Pending),
                Some(Reply::Retired(false)),
                Some(Reply::Poisoned)
            ]
        ));
        drop(publisher);
        event_loop
            .dispatch(Some(Duration::from_secs(2)), &mut seen)
            .unwrap();
        assert!(
            seen.last().unwrap().is_none(),
            "sender must close before disconnect wake"
        );
        let count = seen.len();
        event_loop
            .dispatch(Some(Duration::ZERO), &mut seen)
            .unwrap();
        assert_eq!(seen.len(), count);
    }
    #[test]
    fn coalesced_pending_retired_disconnect_is_drained_by_one_owned_callback() {
        for terminal_receipt in [true, false] {
            let cancel = Cancellation::default();
            let (relay, receiver) = crate::pam_conversation::channel(
                crate::pam_conversation::Binding {
                    origin: crate::auth_owner::Origin {
                        connection: 1,
                        request: 1,
                    },
                    attempt: 1,
                    epoch: 1,
                },
                crate::pam_conversation::Policy {
                    backend_phase: Duration::from_secs(2),
                    total: Duration::from_secs(120),
                },
                cancel.clone(),
            )
            .unwrap();
            let (sender, reply) = mpsc::channel();
            let publisher = ReplyPublisher {
                sender: Some(sender),
                notice: Some(relay.notice()),
            };
            let mut ticket = ConversationTicket {
                ticket: Ticket {
                    serial: 1,
                    reply,
                    cancel,
                    deadline: receiver.total_deadline_for_test(),
                    denied: false,
                },
                receiver,
                outcome: None,
            };
            let source = ticket.take_notice_source().unwrap();
            let mut event_loop =
                calloop::EventLoop::<Vec<Option<Verification>>>::try_new().unwrap();
            event_loop
                .handle()
                .insert_source(source, move |_, _, seen| {
                    seen.push(ticket.try_outcome());
                    assert!(ticket.next_deadline().is_none());
                })
                .unwrap();
            // Every publication happens before the sole dispatch: the actual
            // calloop source must coalesce, not provide one wake per queue entry.
            publisher.send(Reply::Pending).unwrap();
            if terminal_receipt {
                publisher.send(Reply::Retired(true)).unwrap();
            }
            drop(relay);
            drop(publisher);
            let mut seen = Vec::new();
            event_loop
                .dispatch(Some(Duration::from_secs(2)), &mut seen)
                .unwrap();
            assert_eq!(
                seen,
                vec![Some(if terminal_receipt {
                    Verification::Retired {
                        authenticated: false,
                    }
                } else {
                    Verification::Unretired
                })]
            );
            event_loop
                .dispatch(Some(Duration::ZERO), &mut seen)
                .unwrap();
            assert_eq!(
                seen.len(),
                1,
                "no idle polling or invented disconnect retirement"
            );
        }
    }

    fn observed_credential(counter: &Arc<std::sync::atomic::AtomicUsize>) -> Credential {
        Credential {
            text: "owned-control-value".into(),
            dropped: Some(counter.clone()),
        }
    }
    #[test]
    fn live_single_password_adapter_refuses_entire_fingerprint_batch_before_answers() {
        let message = |index, style, text: &str| crate::pam_helper::Message {
            index,
            style,
            text: text.into(),
        };
        for style in [Style::Secret, Style::Visible] {
            assert!(!supported_messages(
                &[message(
                    0,
                    style,
                    "Place your finger on the fingerprint reader"
                )],
                false,
                false
            ));
        }
        // This shape passes the style-only adapter: the whole batch must be
        // refused before even the ordinary visible answer is constructed.
        assert!(supported_styles(
            [Style::Visible, Style::Secret].into_iter(),
            false,
            false
        ));
        assert!(!supported_messages(
            &[
                message(0, Style::Visible, "Login:"),
                message(1, Style::Secret, "Place your finger on the reader"),
            ],
            false,
            false
        ));
        assert!(!supported_messages(
            &[
                message(0, Style::Secret, "Password:"),
                message(1, Style::Visible, "Swipe your finger again"),
            ],
            false,
            false
        ));
        assert!(!supported_messages(
            &[
                message(0, Style::Visible, "Fingerprint verification:"),
                message(1, Style::Secret, "Password:"),
            ],
            false,
            false
        ));
        assert!(!supported_messages(
            &[message(0, Style::Secret, "Swipe your finger again")],
            true,
            true
        ));
        assert!(supported_messages(
            &[
                message(
                    0,
                    Style::Info,
                    "Place your finger on the fingerprint reader"
                ),
                message(1, Style::Error, "Swipe was too short"),
                message(2, Style::Visible, "Login:"),
                message(3, Style::Secret, "Password:"),
            ],
            false,
            false
        ));
        assert!(!supported_messages(
            &[message(0, Style::Secret, "Password:")],
            true,
            false
        ));
    }

    #[test]
    fn conversation_result_poll_keeps_retirement_but_cancellation_denies_grant() {
        for cancel_before_result in [false, true] {
            let (reply, receive) = mpsc::channel();
            let (_relay, receiver) = crate::pam_conversation::channel(
                crate::pam_conversation::Binding {
                    origin: crate::auth_owner::Origin {
                        connection: 1,
                        request: 1,
                    },
                    attempt: 1,
                    epoch: 1,
                },
                crate::pam_conversation::Policy {
                    backend_phase: Duration::from_secs(2),
                    total: Duration::from_secs(120),
                },
                Cancellation::default(),
            )
            .unwrap();
            let cancellation = Cancellation::default();
            let mut ticket = ConversationTicket {
                ticket: Ticket {
                    serial: 1,
                    reply: receive,
                    cancel: cancellation.clone(),
                    deadline: Instant::now() + Duration::from_secs(2),
                    denied: false,
                },
                receiver,
                outcome: None,
            };
            reply.send(Reply::Retired(true)).unwrap();
            if cancel_before_result {
                cancellation.cancel();
            }
            assert_eq!(
                ticket.try_outcome(),
                Some(Verification::Retired {
                    authenticated: !cancel_before_result
                })
            );
            assert_eq!(
                ticket.try_outcome(),
                Some(Verification::Retired {
                    authenticated: !cancel_before_result
                })
            );
            ticket.cancel();
            assert_eq!(
                ticket.try_outcome(),
                Some(Verification::Retired {
                    authenticated: false
                })
            );
        }
    }
    #[test]
    fn conversation_total_instant_is_pinned_and_late_idle_result_cannot_grant() {
        let backend = Backend::with_trusted_factory(|_, _, _, _, _| {
            Err(io::ErrorKind::PermissionDenied.into())
        })
        .unwrap();
        let ticket = backend
            .begin_conversation(
                "tuna-lock".into(),
                "owned-user".into(),
                crate::pam_conversation::Binding {
                    origin: crate::auth_owner::Origin {
                        connection: 1,
                        request: 2,
                    },
                    attempt: 1,
                    epoch: 3,
                },
                crate::pam_conversation::Policy {
                    backend_phase: Duration::from_secs(2),
                    total: Duration::from_secs(120),
                },
                Cancellation::default(),
            )
            .unwrap();
        assert_eq!(
            ticket.ticket.deadline,
            ticket.receiver.total_deadline_for_test()
        );
        drop(ticket);
        backend.shutdown();
        for expire_before_first_poll in [true, false] {
            let (reply, receive) = mpsc::channel();
            let (_relay, receiver) = crate::pam_conversation::channel(
                crate::pam_conversation::Binding {
                    origin: crate::auth_owner::Origin {
                        connection: 1,
                        request: 2,
                    },
                    attempt: 1,
                    epoch: 3,
                },
                crate::pam_conversation::Policy {
                    backend_phase: Duration::from_secs(2),
                    total: Duration::from_secs(600),
                },
                Cancellation::default(),
            )
            .unwrap();
            let deadline = receiver.total_deadline_for_test();
            let mut ticket = ConversationTicket {
                ticket: Ticket {
                    serial: 1,
                    reply: receive,
                    cancel: Cancellation::default(),
                    deadline,
                    denied: false,
                },
                receiver,
                outcome: None,
            };
            reply.send(Reply::Retired(true)).unwrap();
            if !expire_before_first_poll {
                assert_eq!(
                    ticket.try_outcome(),
                    Some(Verification::Retired {
                        authenticated: true
                    })
                );
            }
            ticket.receiver.expire_idle_for_test();
            assert!(Instant::now() < ticket.ticket.deadline);
            assert_eq!(
                ticket.try_outcome(),
                Some(Verification::Retired {
                    authenticated: false
                })
            );
            assert_eq!(
                ticket.try_outcome(),
                Some(Verification::Retired {
                    authenticated: false
                })
            );
        }
    }

    #[test]
    fn conversation_denial_is_not_a_retirement_receipt() {
        let (reply, receive) = mpsc::channel();
        let (_relay, receiver) = crate::pam_conversation::channel(
            crate::pam_conversation::Binding {
                origin: crate::auth_owner::Origin {
                    connection: 1,
                    request: 1,
                },
                attempt: 1,
                epoch: 1,
            },
            crate::pam_conversation::Policy {
                backend_phase: Duration::from_secs(2),
                total: Duration::from_secs(120),
            },
            Cancellation::default(),
        )
        .unwrap();
        let mut ticket = ConversationTicket {
            ticket: Ticket {
                serial: 1,
                reply: receive,
                cancel: Cancellation::default(),
                deadline: Instant::now(),
                denied: false,
            },
            receiver,
            outcome: None,
        };
        assert_eq!(ticket.try_outcome(), None);
        reply.send(Reply::Pending).unwrap();
        assert_eq!(ticket.try_outcome(), None);
        reply.send(Reply::Retired(true)).unwrap();
        assert_eq!(
            ticket.try_outcome(),
            Some(Verification::Retired {
                authenticated: false
            })
        );
    }

    #[test]
    fn private_credential_wipes_owned_storage_and_moves_into_answer_without_clone() {
        let mut credential = Credential::from_string("owned-control-value".into());
        let answer = credential.take_answer();
        assert!(credential.as_str().is_empty());
        assert_eq!(
            serde_json::to_string(&answer).unwrap(),
            "\"owned-control-value\""
        );
        let mut credential = Credential::from_string("owned-control-value".into());
        credential.wipe();
        assert!(credential.as_str().as_bytes().iter().all(|b| *b == 0));
        assert!(!format!("{credential:?}").contains("owned-control-value"));
        // SAME method used by Drop, observed while storage is still owned.
    }
    #[test]
    fn denied_validation_busy_and_lost_receiver_drop_owned_credential() {
        use std::sync::atomic::AtomicUsize;
        let counter = Arc::new(AtomicUsize::new(0));
        let backend =
            Backend::with_trusted_factory(|_, _, _, _, _| panic!("denials must not dispatch"))
                .unwrap();
        assert!(backend
            .begin_credential(
                "bad/service".into(),
                "owner".into(),
                observed_credential(&counter),
                Cancellation::default()
            )
            .is_err());
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        backend.state.store(BUSY, Ordering::Release);
        assert!(backend
            .begin_credential(
                "tuna-lock".into(),
                "owner".into(),
                observed_credential(&counter),
                Cancellation::default()
            )
            .is_err());
        assert_eq!(counter.load(Ordering::SeqCst), 2);
        backend.shutdown();
        // A closed submission receiver rejects BEFORE any helper could start.
        let (sender, receive) = mpsc::sync_channel(1);
        drop(receive);
        let backend = Backend {
            sender,
            state: Arc::new(AtomicU8::new(FREE)),
            serial: Arc::new(AtomicU64::new(0)),
            shutdown: Arc::new(AtomicBool::new(false)),
            wake: WakeFd::new(Arc::new(AtomicBool::new(false))).unwrap(),
            owner: Arc::new(Mutex::new(None)),
        };
        assert!(backend
            .begin_credential(
                "tuna-lock".into(),
                "owner".into(),
                observed_credential(&counter),
                Cancellation::default()
            )
            .is_err());
        assert_eq!(counter.load(Ordering::SeqCst), 3);
    }
    #[test]
    fn discarded_queued_credential_and_pending_unanswered_credential_drop() {
        use std::sync::atomic::AtomicUsize;
        let counter = Arc::new(AtomicUsize::new(0));
        let (reply, _receive) = mpsc::channel();
        let job = Job {
            serial: 1,
            service: "tuna-lock".into(),
            user: "owner".into(),
            password: observed_credential(&counter),
            relay: None,
            cancel: Cancellation::default(),
            reply: ReplyPublisher {
                sender: Some(reply),
                notice: None,
            },
        };
        let (send, receive) = mpsc::sync_channel(1);
        assert!(send.send(Command::Attempt(Box::new(job))).is_ok());
        drop(receive);
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        let pending = Some(observed_credential(&counter));
        drop(pending);
        assert_eq!(counter.load(Ordering::SeqCst), 2);
        // These are ownership/drop controls, not physical helper retirement.
    }
    #[test]
    fn single_password_never_answers_a_distinct_second_secret() {
        assert!(supported_styles(
            [Style::Visible, Style::Secret, Style::Info, Style::Error].into_iter(),
            false,
            false
        ));
        for styles in [
            vec![Style::Secret, Style::Secret],
            vec![Style::Secret, Style::Visible],
            vec![Style::Visible, Style::Visible],
        ] {
            assert!(!supported_styles(styles.into_iter(), false, false));
        }
        assert!(!supported_styles([Style::Secret].into_iter(), true, false));
        assert!(supported_styles(
            [Style::Info, Style::Error].into_iter(),
            true,
            true
        ));
    }
    fn ticket(reply: mpsc::Receiver<Reply>) -> Ticket {
        Ticket {
            serial: 1,
            reply,
            cancel: Cancellation::default(),
            deadline: Instant::now() + Duration::from_secs(1),
            denied: false,
        }
    }
    #[test]
    fn pending_denial_cannot_grant_a_late_correct_retired_result() {
        let (send, receive) = mpsc::channel();
        send.send(Reply::Pending).unwrap();
        send.send(Reply::Retired(true)).unwrap();
        let mut pending = 0;
        assert_eq!(
            ticket(receive).wait_retired(|| pending += 1),
            Verification::Retired {
                authenticated: false
            }
        );
        assert_eq!(pending, 1);
    }
    #[test]
    fn queued_retired_true_after_deadline_denies_but_preserves_physical_receipt() {
        let (send, receive) = mpsc::channel();
        send.send(Reply::Retired(true)).unwrap();
        let mut ticket = ticket(receive);
        ticket.deadline = Instant::now() - Duration::from_millis(1);
        assert!(matches!(ticket.initial(), Initial::Pending));
        assert!(ticket.denied && ticket.cancel.is_canceled());
        assert_eq!(
            ticket.wait_retired(|| panic!("already latched denial")),
            Verification::Retired {
                authenticated: false
            }
        );
    }
    #[test]
    fn received_retired_result_crossing_absolute_deadline_cannot_grant() {
        let (_send, receive) = mpsc::channel();
        let mut ticket = ticket(receive);
        let deadline = ticket.deadline;
        // Exact production acceptance branch, with deterministic before/at/after
        // observation times. This models scheduler delay, not a clock override.
        assert!(matches!(
            ticket.retired_at(true, deadline - Duration::from_nanos(1)),
            Initial::Finished(Verification::Retired {
                authenticated: true
            })
        ));
        assert!(matches!(
            ticket.retired_at(true, deadline),
            Initial::Finished(Verification::Retired {
                authenticated: false
            })
        ));
        assert!(ticket.denied);
        assert!(matches!(
            ticket.retired_at(true, deadline + Duration::from_nanos(1)),
            Initial::Finished(Verification::Retired {
                authenticated: false
            })
        ));
        // Once denied, even an earlier-looking timestamp cannot undo the latch.
        assert!(matches!(
            ticket.retired_at(true, deadline - Duration::from_nanos(1)),
            Initial::Finished(Verification::Retired {
                authenticated: false
            })
        ));
    }
    #[test]
    fn disconnected_or_poisoned_owner_is_not_retired() {
        let (send, receive) = mpsc::channel();
        drop(send);
        assert_eq!(
            ticket(receive).wait_retired(|| panic!("no pending")),
            Verification::Unretired
        );
        let (send, receive) = mpsc::channel();
        send.send(Reply::Poisoned).unwrap();
        assert_eq!(
            ticket(receive).wait_retired(|| panic!("no pending")),
            Verification::Unretired
        );
        assert!(!Verification::Unretired.may_retire());
    }
    #[test]
    fn owner_factory_failure_poison_refuses_a_second_credential() {
        let backend =
            Backend::with_trusted_factory(|_, _, _, _, _| Err(io::ErrorKind::Other.into()))
                .unwrap();
        let attempt = backend
            .begin(
                "tuna-lock".into(),
                "own-user".into(),
                Secret("first".into()),
                Cancellation::default(),
            )
            .unwrap();
        assert_eq!(
            attempt.wait_retired(|| panic!("no pending")),
            Verification::Unretired
        );
        assert!(matches!(
            backend.begin(
                "tuna-lock".into(),
                "own-user".into(),
                Secret("second".into()),
                Cancellation::default()
            ),
            Err(Verification::RejectedBeforeSpawn)
        ));
        backend.shutdown();
    }
    #[test]
    fn exhausted_serial_poison_never_runs_a_factory() {
        let backend = Backend::with_trusted_factory(|_, _, _, _, _| {
            panic!("overflow must not dispatch a credential")
        })
        .unwrap();
        backend.serial.store(u64::MAX, Ordering::Release);
        assert!(matches!(
            backend.begin(
                "tuna-lock".into(),
                "own-user".into(),
                Secret("first".into()),
                Cancellation::default()
            ),
            Err(Verification::RejectedBeforeSpawn)
        ));
        assert_eq!(backend.state.load(Ordering::Acquire), POISONED);
        backend.shutdown();
    }
}
