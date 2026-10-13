//! Stable creator ownership for live private PAM helpers; deadlines never retire.
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

type Factory =
    Arc<dyn Fn(Identity, String, String, Duration) -> io::Result<Supervisor> + Send + Sync>;

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
struct Job {
    serial: u64,
    service: String,
    user: String,
    password: Secret,
    cancel: Arc<AtomicBool>,
    reply: mpsc::Sender<Reply>,
}
enum Command {
    Attempt(Job),
    Shutdown,
}
#[derive(Clone)]
pub struct Backend {
    sender: mpsc::SyncSender<Command>,
    state: Arc<AtomicU8>,
    serial: Arc<AtomicU64>,
    shutdown: Arc<AtomicBool>,
    owner: Arc<Mutex<Option<JoinHandle<()>>>>,
}
pub struct Ticket {
    pub serial: u64,
    reply: mpsc::Receiver<Reply>,
    cancel: Arc<AtomicBool>,
    deadline: Instant,
    denied: bool,
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
            self.cancel.store(true, Ordering::Release);
            return Initial::Pending;
        };
        match self.reply.recv_timeout(remaining) {
            Ok(Reply::Retired(authenticated)) => Initial::Finished(Verification::Retired {
                authenticated: authenticated && !self.denied,
            }),
            Ok(Reply::Pending) | Err(mpsc::RecvTimeoutError::Timeout) => {
                self.denied = true;
                self.cancel.store(true, Ordering::Release);
                Initial::Pending
            }
            Ok(Reply::Poisoned) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                Initial::Finished(Verification::Unretired)
            }
        }
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
        self.cancel.store(true, Ordering::Release);
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
            Self::from_factory(Arc::new(move |identity, service, user, budget| {
                image.spawn(identity, service, user, budget)
            }))
            .ok()
        })
        .clone()
    }
    /// Trusted Rust fixture seam, not a live executable configuration surface.
    /// The factory runs on this stable creator; callers must admit its binary.
    #[doc(hidden)]
    pub fn with_trusted_factory(
        factory: impl Fn(Identity, String, String, Duration) -> io::Result<Supervisor>
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
                            if !run_job(job, &factory, &owner_shutdown, &owner_state) {
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
            serial: Arc::new(AtomicU64::new(0)),
            owner: Arc::new(Mutex::new(Some(owner))),
        })
    }
    pub fn begin(
        &self,
        service: String,
        user: String,
        password: Secret,
        cancel: Arc<AtomicBool>,
    ) -> Result<Ticket, Verification> {
        if service.is_empty()
            || service.len() > 128
            || !service
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
            || user.is_empty()
            || user.len() > TEXT_LIMIT
            || user.contains('\0')
            || password.0.len() > TEXT_LIMIT
            || password.0.contains('\0')
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
        let (reply, receive) = mpsc::channel();
        let job = Job {
            serial,
            service,
            user,
            password,
            cancel: cancel.clone(),
            reply,
        };
        if self.sender.try_send(Command::Attempt(job)).is_err() {
            self.state.store(POISONED, Ordering::Release);
            return Err(Verification::RejectedBeforeSpawn);
        }
        Ok(Ticket {
            serial,
            reply: receive,
            cancel,
            deadline: Instant::now() + crate::pam::DEADLINE,
            denied: false,
        })
    }
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
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
fn run_job(job: Job, factory: &Factory, shutdown: &AtomicBool, state: &AtomicU8) -> bool {
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
        crate::pam::DEADLINE,
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
        if job.cancel.load(Ordering::Acquire) || shutdown.load(Ordering::Acquire) {
            break;
        }
        match supervisor.next_event() {
            Ok(Event::Finished { authenticated }) => {
                if supervisor.retirement() != Retirement::Retired {
                    break;
                }
                let accepted = authenticated
                    && !job.cancel.load(Ordering::Acquire)
                    && !shutdown.load(Ordering::Acquire);
                drop(secret);
                // Release only after actual Retired, before telling the caller.
                state.store(FREE, Ordering::Release);
                let _ = job.reply.send(Reply::Retired(accepted));
                return true;
            }
            Ok(Event::Challenge { sequence, messages }) => {
                rounds += 1;
                if rounds > crate::unlock::MAX_AUTH_ROUNDS {
                    break;
                }
                if !supported_styles(
                    messages.iter().map(|message| message.style),
                    secret_used,
                    visible_used,
                ) {
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
                            Some(AnswerText::new(std::mem::take(&mut password.0)))
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
            cancel: Arc::new(AtomicBool::new(false)),
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
            Backend::with_trusted_factory(|_, _, _, _| Err(io::ErrorKind::Other.into())).unwrap();
        let attempt = backend
            .begin(
                "tuna-lock".into(),
                "own-user".into(),
                Secret("first".into()),
                Arc::new(AtomicBool::new(false)),
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
                Arc::new(AtomicBool::new(false))
            ),
            Err(Verification::RejectedBeforeSpawn)
        ));
        backend.shutdown();
    }
    #[test]
    fn exhausted_serial_poison_never_runs_a_factory() {
        let backend = Backend::with_trusted_factory(|_, _, _, _| {
            panic!("overflow must not dispatch a credential")
        })
        .unwrap();
        backend.serial.store(u64::MAX, Ordering::Release);
        assert!(matches!(
            backend.begin(
                "tuna-lock".into(),
                "own-user".into(),
                Secret("first".into()),
                Arc::new(AtomicBool::new(false))
            ),
            Err(Verification::RejectedBeforeSpawn)
        ));
        assert_eq!(backend.state.load(Ordering::Acquire), POISONED);
        backend.shutdown();
    }
}
