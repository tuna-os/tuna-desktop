//! GNOME 51's lock-screen authentication flow as a pure state machine.
//!
//! The compositor runs the conversations (GDM's role: a PAM conversation
//! for the password service, fprintd for the fingerprint service); this
//! machine is GNOME Shell's side of them, combining:
//!
//! - `AuthPrompt` (<https://gitlab.gnome.org/GNOME/gnome-shell/-/blob/51.0/js/gdm/authPrompt.js>):
//!   `_onAskQuestion`, `_activateNext`, `updateEntry`, `setQuestion`,
//!   `updateSensitivity`, `_onVerificationFailed`, `reset`, `cancel`, the
//!   prompt step and the preemptive answer;
//! - `ShellUserVerifier` (<https://gitlab.gnome.org/GNOME/gnome-shell/-/blob/51.0/js/gdm/userVerifier.js>):
//!   the message queue, its read time and priority messages;
//! - `AuthServices` / `AuthServicesLegacy`
//!   (<https://gitlab.gnome.org/GNOME/gnome-shell/-/blob/51.0/js/gdm/authServices.js>,
//!   <https://gitlab.gnome.org/GNOME/gnome-shell/-/blob/51.0/js/gdm/authServicesLegacy.js>):
//!   which service's info, problem and query reach the prompt, the
//!   fingerprint hint, conversation-stopped and the reset that follows;
//! - `UnlockDialog` (<https://gitlab.gnome.org/GNOME/gnome-shell/-/blob/51.0/js/ui/unlockDialog.js>):
//!   `_ensureAuthPrompt`, `_maybeDestroyAuthPrompt` and `_onReset`.
//!
//! It never touches GTK: inputs are method calls, the compositor is a
//! [`Host`], and what the widgets must show is a [`View`] plus one-shot
//! [`Effect`]s.

use std::collections::VecDeque;

use tuna_shell_control::{AuthEvent, AuthService};

/// userVerifier.js `USER_READ_TIME`: 48 ms to read each character.
pub const USER_READ_TIME_MS: u64 = 48;
/// userVerifier.js `USER_READ_TIME_MIN`: a message stays at least 2 s.
pub const USER_READ_TIME_MIN_MS: u64 = 2000;
/// authServices.js `_waitPendingMessages`: a failed conversation waits at
/// most 10 s for queued messages before the prompt resets.
pub const WAIT_PENDING_MESSAGES_MS: u64 = 10_000;
/// The pause before restarting a prompt left with no conversation by
/// Problem-less Denied results (a helper may still be retiring).
pub const RESTART_BACKOFF_MS: u64 = 500;
/// The entry's hint before any question, and GNOME's own label for PAM's
/// `Password:` (authPrompt.js `_onAskQuestion`).
pub const DEFAULT_QUESTION: &str = "Password";
/// GNOME's wrong-password message, shown by the single-password fallback.
pub const LEGACY_FAILED: &str = "Sorry, that didn't work. Please try again.";
/// userVerifier.js `_reportInitError`: a conversation that could not start.
pub const INIT_ERROR: &str = "Authentication error";

/// userVerifier.js `MessageType`, in priority order (`NONE` is a hidden
/// message: `None` text).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum MessageKind {
    Hint = 1,
    Info = 2,
    Error = 3,
}

impl MessageKind {
    /// The `type=` of the trace line.
    pub fn trace_name(self) -> &'static str {
        match self {
            Self::Hint => "hint",
            Self::Info => "info",
            Self::Error => "problem",
        }
    }
}

/// The entry hint for a PAM question (authPrompt.js `_onAskQuestion`):
/// `Password:` becomes GNOME's own "Password"; anything else loses one
/// trailing colon (ASCII or full-width) with the spaces after it
/// (`/[:：] *$/`), then surrounding white space.
pub fn question_label(question: &str) -> String {
    if question == "Password:" || question == "Password: " {
        return DEFAULT_QUESTION.to_owned();
    }
    let unspaced = question.trim_end_matches(' ');
    let stripped = unspaced
        .strip_suffix(':')
        .or_else(|| unspaced.strip_suffix('：'))
        .unwrap_or(question);
    stripped.trim().to_owned()
}

/// authServicesLegacy.js `_handleOnInfo`: what the prompt says while the
/// fingerprint reader waits, by reader type.
pub fn fingerprint_hint(swipe: bool) -> &'static str {
    if swipe {
        "(or swipe finger across reader)"
    } else {
        "(or place finger on reader)"
    }
}

/// userVerifier.js `GDM_MESSAGE_TIME_MULTIPLIER`: a positive number, else 1.
pub fn parse_message_multiplier(value: Option<&str>) -> f64 {
    value
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v > 0.0)
        .unwrap_or(1.0)
}

/// userVerifier.js `_getIntervalForMessage`: how long a message holds the
/// queue, `max(48 ms × length, 2000 ms) × multiplier`; nothing for a
/// hidden message. The length is JavaScript's (UTF-16 code units).
pub fn message_interval_ms(text: Option<&str>, multiplier: f64) -> u64 {
    let Some(text) = text.filter(|t| !t.is_empty()) else {
        return 0;
    };
    let length = text.encode_utf16().count() as u64;
    let base = (length * USER_READ_TIME_MS).max(USER_READ_TIME_MIN_MS);
    (base as f64 * multiplier).round() as u64
}

/// The compositor's verification commands and the shell settings the
/// flow reads. Each command returns its request id, `None` when it could
/// not be sent.
pub trait Host {
    fn begin(&self, service: AuthService) -> Option<u64>;
    fn answer(&self, service: AuthService, sequence: u32, answer: String) -> Option<u64>;
    fn cancel(&self) -> Option<u64>;
    /// The single-password `Unlock` command (no conversation backend).
    fn unlock(&self, password: String) -> Option<u64>;
    /// `org.gnome.login-screen enable-fingerprint-authentication`.
    fn fingerprint_enabled(&self) -> bool;
    /// How long a wiggle runs now (0 with animations off).
    fn wiggle_ms(&self) -> u64;
}

/// What the prompt widgets show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct View {
    /// A hidden (password) entry, else a visible one.
    pub secret: bool,
    /// The entry's hint and accessible label.
    pub hint: String,
    /// Whether the entry and Submit take input.
    pub sensitive: bool,
    /// Back shown and targetable (`promptStep > 1`).
    pub back: bool,
    pub message: Option<(String, MessageKind)>,
}

/// One-shot work for the widgets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Call [`AuthMachine::timeout`] with `token` after `ms`.
    Timer { token: u64, ms: u64 },
    /// Empty the entry.
    ClearEntry,
    /// Give the entry the keyboard focus.
    Focus,
    /// Wiggle the entry area (failed answer).
    WiggleEntry,
    /// Wiggle the message (fingerprint problem).
    WiggleMessage,
    /// Tell assistive technologies (GNOME's assertive notification).
    Announce(String),
    /// Bring the prompt page back.
    ShowPrompt,
    /// A `TUNA_LOCK_TRACE` line, without the program prefix.
    Trace(String),
}

/// authPrompt.js `AuthPromptStatus`, minus the states the unlock prompt
/// never keeps (`VERIFICATION_CANCELLED` ends in [`AuthMachine::stop`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Status {
    NotVerifying,
    Verifying,
    InProgress,
    Failed,
    Succeeded,
}

#[derive(Clone, Debug, PartialEq)]
struct Message {
    service: AuthService,
    text: Option<String>,
    kind: MessageKind,
    wiggle: bool,
    interval: u64,
}

fn service_name(service: AuthService) -> &'static str {
    match service {
        AuthService::Password => "password",
        AuthService::Fingerprint => "fingerprint",
    }
}

/// The lock prompt's authentication state, shared by every monitor.
pub struct AuthMachine {
    multiplier: f64,
    status: Status,
    /// The current begin of each service, by request id.
    password: Option<u64>,
    fingerprint: Option<u64>,
    /// A Problem arrived for the current request (its Denied is a
    /// failure, not a cancellation).
    password_problem: bool,
    fingerprint_problem: bool,
    /// Fingerprint said Unavailable: no hint, no restart until a reset.
    fingerprint_unavailable: bool,
    /// Password said Unavailable this lock cycle: single-password unlock.
    legacy: bool,
    legacy_pending: Option<u64>,
    /// Cancelled begins whose results are still owed; new begins wait
    /// for them (the compositor's retirement barrier).
    retiring: Vec<u64>,
    begin_wanted: bool,
    /// Begin again once the connection can carry it (after a reconnect
    /// or a begin that could not be sent).
    resume_wanted: bool,
    /// The unanswered question's sequence.
    query: Option<u32>,
    /// authPrompt.js `_queryingService === 'gdm-password'`.
    queried: bool,
    step: u32,
    preemptive_answer: Option<String>,
    preemptive_input: bool,
    /// An answer held until queued messages are read
    /// (`_completePendingCallback` awaits `handlePendingMessages`).
    deferred: Option<(u64, u32, String)>,
    /// A held answer the queue released (`no-more-messages`), sent once
    /// the input that drained the queue is handled.
    ready_answer: Option<(u64, u32, String)>,
    /// The soft reset waiting on queued messages, by timer token.
    reset_wait: Option<u64>,
    /// The soft reset is due once the current input is handled.
    ready_reset: bool,
    /// The backoff before restarting a prompt whose conversations all
    /// ended without a Problem, by timer token.
    restart_wait: Option<u64>,
    queue: VecDeque<Message>,
    queue_timer: Option<u64>,
    shown: Option<(AuthService, MessageKind)>,
    next_token: u64,
    entry_has_text: bool,
    secret: bool,
    hint: String,
    sensitive: bool,
    message: Option<(String, MessageKind)>,
    effects: Vec<Effect>,
}

impl AuthMachine {
    pub fn new(multiplier: f64) -> Self {
        Self {
            multiplier,
            status: Status::NotVerifying,
            password: None,
            fingerprint: None,
            password_problem: false,
            fingerprint_problem: false,
            fingerprint_unavailable: false,
            legacy: false,
            legacy_pending: None,
            retiring: Vec::new(),
            begin_wanted: false,
            resume_wanted: false,
            query: None,
            queried: false,
            step: 0,
            preemptive_answer: None,
            preemptive_input: false,
            deferred: None,
            ready_answer: None,
            reset_wait: None,
            ready_reset: false,
            restart_wait: None,
            queue: VecDeque::new(),
            queue_timer: None,
            shown: None,
            next_token: 0,
            entry_has_text: false,
            secret: true,
            hint: DEFAULT_QUESTION.to_owned(),
            sensitive: true,
            message: None,
            effects: Vec::new(),
        }
    }

    pub fn view(&self) -> View {
        View {
            secret: self.secret,
            hint: self.hint.clone(),
            sensitive: self.sensitive,
            back: self.step > 1,
            message: self.message.clone(),
        }
    }

    pub fn take_effects(&mut self) -> Vec<Effect> {
        std::mem::take(&mut self.effects)
    }

    /// authPrompt.js `promptStep`.
    #[cfg(test)]
    pub fn prompt_step(&self) -> u32 {
        self.step
    }

    /// The single-password request in flight.
    #[cfg(test)]
    pub fn legacy_pending(&self) -> Option<u64> {
        self.legacy_pending
    }

    /// Whether password verification fell back to single-password unlock.
    #[cfg(test)]
    pub fn legacy(&self) -> bool {
        self.legacy
    }

    /// The prompt page is coming up (unlockDialog.js `_showPrompt` →
    /// `_ensureAuthPrompt`): a prompt not verifying resets and begins;
    /// a printable key that lifted the curtain starts preemptive input
    /// (`startPreemptiveInput`).
    pub fn show_prompt(&mut self, host: &dyn Host, preemptive: bool) {
        if matches!(self.status, Status::NotVerifying | Status::Failed) {
            self.reset(host);
            self.begin(host);
            // `updateSensitivity({sensitive: verificationStatus ===
            // NOT_VERIFYING})` with the status from before the reset; a
            // failed begin keeps the entry for the retry.
            self.set_sensitive(true);
        }
        if preemptive {
            self.preemptive_input = true;
            self.set_sensitive(true);
        }
        self.flush(host);
    }

    /// Enter or Submit (authPrompt.js `_activateNext`).
    pub fn submit(&mut self, host: &dyn Host, text: String) {
        if !self.sensitive || self.status == Status::Succeeded {
            return;
        }
        if self.legacy {
            self.legacy_submit(host, text);
        } else {
            self.status = Status::InProgress;
            self.set_sensitive(false);
            match self.query.take() {
                Some(sequence) => self.complete_pending(host, sequence, text),
                None => self.preemptive_answer = Some(text),
            }
            self.preemptive_input = false;
        }
        self.flush(host);
    }

    /// The entry's text changed (authPrompt.js: the message fades unless
    /// messages are still queued).
    pub fn text_changed(&mut self, has_text: bool) {
        self.entry_has_text = has_text;
        self.fade_unless_pending();
    }

    /// Back or Escape (authPrompt.js `cancel`): from a later step, back to
    /// the first; otherwise stop. Returns whether the clock comes back.
    pub fn cancel(&mut self, host: &dyn Host) -> bool {
        let clock = match self.status {
            Status::Succeeded => false,
            _ if self.step > 1 => {
                self.soft_restart(host);
                false
            }
            _ => {
                self.stop(host);
                true
            }
        };
        self.flush(host);
        clock
    }

    /// The clock is back (unlockDialog.js `_maybeDestroyAuthPrompt`): the
    /// conversation and everything typed go.
    pub fn stop(&mut self, host: &dyn Host) {
        self.cancel_conversations(host);
        self.status = Status::NotVerifying;
        self.step = 0;
        self.query = None;
        self.queried = false;
        self.preemptive_answer = None;
        self.preemptive_input = false;
        self.resume_wanted = false;
        self.fingerprint_unavailable = false;
        self.clear_message_queue();
        self.drop_waiters();
        self.clear_entry();
        self.secret = true;
        self.hint = DEFAULT_QUESTION.to_owned();
        self.sensitive = true;
        self.flush(host);
    }

    /// One step of a conversation. Steps of requests no longer current
    /// are ignored.
    pub fn event(
        &mut self,
        host: &dyn Host,
        request: u64,
        service: AuthService,
        event: &AuthEvent,
    ) {
        let current = match service {
            AuthService::Password => self.password,
            AuthService::Fingerprint => self.fingerprint,
        };
        if current != Some(request) || self.status == Status::Succeeded {
            return;
        }
        match (service, event) {
            // authServicesLegacy.js `_handleOnInfo` / `_handleOnProblem`.
            (AuthService::Password, AuthEvent::Info { text }) => {
                self.queue_message(host, service, Some(text.clone()), MessageKind::Info, false);
            }
            (AuthService::Password, AuthEvent::Problem { text }) => {
                self.password_problem = true;
                self.queue_priority_message(
                    host,
                    service,
                    Some(text.clone()),
                    MessageKind::Error,
                    false,
                );
            }
            (
                AuthService::Password,
                AuthEvent::Query {
                    sequence,
                    question,
                    secret,
                },
            ) => self.ask_question(host, *sequence, question, *secret),
            (AuthService::Password, AuthEvent::Unavailable) => self.fall_back(host),
            (AuthService::Fingerprint, AuthEvent::Problem { text }) => {
                self.fingerprint_problem = true;
                self.queue_priority_message(
                    host,
                    service,
                    Some(text.clone()),
                    MessageKind::Error,
                    true,
                );
            }
            // The reader is waiting: GNOME's own hint, not fprintd's text.
            (AuthService::Fingerprint, AuthEvent::FingerprintScan { swipe }) => {
                self.queue_message(
                    host,
                    service,
                    Some(fingerprint_hint(*swipe).to_owned()),
                    MessageKind::Hint,
                    false,
                );
            }
            (AuthService::Fingerprint, AuthEvent::Unavailable) => {
                self.fingerprint_unavailable = true;
                self.filter_service_messages(host, service, MessageKind::Error);
                if self.shown == Some((AuthService::Fingerprint, MessageKind::Hint)) {
                    self.message = None;
                    self.shown = None;
                }
            }
            // Fingerprint info is only a cue (the scan event is ours);
            // the other combinations do not occur.
            _ => {}
        }
        self.flush(host);
    }

    /// A command result. Returns whether it was one of this machine's
    /// requests (answers' own "queued" results are not).
    pub fn result(&mut self, host: &dyn Host, id: u64, applied: bool) -> bool {
        if self.legacy_pending == Some(id) {
            self.legacy_result(host, id, applied);
            self.flush(host);
            return true;
        }
        if let Some(index) = self.retiring.iter().position(|r| *r == id) {
            self.retiring.swap_remove(index);
            if self.retiring.is_empty() && self.begin_wanted {
                self.start_services(host);
            }
            self.flush(host);
            return true;
        }
        let service = if self.password == Some(id) {
            AuthService::Password
        } else if self.fingerprint == Some(id) {
            AuthService::Fingerprint
        } else {
            return false;
        };
        self.effects.push(Effect::Trace(format!(
            "lock auth result service={} request={id} applied={applied}",
            service_name(service)
        )));
        if applied {
            // `_onVerificationComplete`: wait for the unlocked snapshot.
            self.status = Status::Succeeded;
            let other = match service {
                AuthService::Password => self.fingerprint.take(),
                AuthService::Fingerprint => self.password.take(),
            };
            self.retiring.extend(other);
            self.password = None;
            self.fingerprint = None;
            self.query = None;
            self.drop_waiters();
            self.sensitive = false;
            self.flush(host);
            return true;
        }
        // A Denied after a Problem is a failure; without one it was a
        // cancellation (ours, the lock's, the other service winning, or a
        // busy refusal), and after Unavailable the service is simply gone.
        let (problem, unavailable) = match service {
            AuthService::Password => {
                self.password = None;
                self.query = None;
                self.deferred = None;
                self.ready_answer = None;
                (std::mem::take(&mut self.password_problem), self.legacy)
            }
            AuthService::Fingerprint => {
                self.fingerprint = None;
                (
                    std::mem::take(&mut self.fingerprint_problem),
                    self.fingerprint_unavailable,
                )
            }
        };
        // `_onConversationStopped` filters the service's messages.
        self.filter_service_messages(host, service, MessageKind::Error);
        if problem && !unavailable {
            self.verification_failed(host, service);
        } else if !problem
            && !unavailable
            && self.password.is_none()
            && self.fingerprint.is_none()
            && self.reset_wait.is_none()
            && !self.ready_reset
            && self.restart_wait.is_none()
        {
            // Nothing of this cycle runs any more and nothing will restart
            // it (the compositor cancelled on a lock-epoch change such as
            // suspend or origin loss, or refused while a helper retires):
            // begin again after a backoff rather than leave a dead prompt.
            // A service that lost to the other is still current here, so a
            // win never restarts. An answer held for this cycle is dropped:
            // the next conversation never replays what was typed for a
            // cancelled one (the stale-attempt rule of the single-password
            // `Unlock`, which is denied and never resubmitted).
            self.preemptive_answer = None;
            let token = self.token();
            self.restart_wait = Some(token);
            self.effects.push(Effect::Timer {
                token,
                ms: RESTART_BACKOFF_MS,
            });
        }
        self.flush(host);
        true
    }

    /// A timer from [`Effect::Timer`] fired.
    pub fn timeout(&mut self, host: &dyn Host, token: u64) {
        if self.queue_timer == Some(token) {
            self.queue_timer = None;
            if self.queue.len() > 1 {
                self.queue.pop_front();
                self.queue_message_timeout(host);
            } else {
                self.finish_message_queue();
            }
        } else if self.reset_wait == Some(token) {
            self.reset_wait = None;
            self.ready_reset = true;
        } else if self.restart_wait == Some(token) {
            self.restart_wait = None;
            if self.status != Status::Succeeded
                && self.password.is_none()
                && self.fingerprint.is_none()
                && !self.begin_wanted
            {
                self.soft_restart(host);
            }
        }
        self.flush(host);
    }

    /// A new control connection replaced the old one: its conversations
    /// and owed results died with it. Local entry is aborted (the prompt's
    /// failure message, nothing sent) and verification begins again once
    /// the connection can carry it ([`AuthMachine::resume`]).
    pub fn reconnected(&mut self, host: &dyn Host) {
        let multiplier = self.multiplier;
        let next_token = self.next_token;
        let entry_has_text = self.entry_has_text;
        let effects = std::mem::take(&mut self.effects);
        *self = Self::new(multiplier);
        self.next_token = next_token;
        self.entry_has_text = entry_has_text;
        self.effects = effects;
        self.clear_entry();
        self.set_sensitive(true);
        self.queue_priority_message(
            host,
            AuthService::Password,
            Some(LEGACY_FAILED.to_owned()),
            MessageKind::Error,
            false,
        );
        self.resume_wanted = true;
        self.flush(host);
    }

    /// Whether [`AuthMachine::resume`] has anything to begin.
    pub fn wants_resume(&self) -> bool {
        self.resume_wanted && self.status != Status::Succeeded
    }

    /// The connection can carry authentication again: begin what a
    /// reconnect or an unsendable begin left waiting.
    pub fn resume(&mut self, host: &dyn Host) {
        if self.wants_resume() {
            self.begin(host);
            self.set_sensitive(true);
        }
        self.flush(host);
    }

    /// The session unlocked: nothing carries over to the next lock.
    pub fn forget(&mut self) {
        let next_token = self.next_token;
        *self = Self::new(self.multiplier);
        self.next_token = next_token;
    }

    fn token(&mut self) -> u64 {
        self.next_token += 1;
        self.next_token
    }

    /// authPrompt.js `updateSensitivity`: a held preemptive answer keeps
    /// the entry insensitive; a sensitive entry takes the focus.
    fn set_sensitive(&mut self, sensitive: bool) {
        if sensitive && self.preemptive_answer.is_some() {
            return;
        }
        self.sensitive = sensitive;
        if sensitive {
            self.effects.push(Effect::Focus);
        }
    }

    fn clear_entry(&mut self) {
        if std::mem::take(&mut self.entry_has_text) {
            self.fade_unless_pending();
        }
        self.effects.push(Effect::ClearEntry);
    }

    fn fade_unless_pending(&mut self) {
        if self.queue.is_empty() {
            self.message = None;
            self.shown = None;
        }
    }

    /// authPrompt.js `reset` (the unlock prompt's form): cancel, clear,
    /// and keep typed text only while preemptive input holds it.
    fn reset(&mut self, host: &dyn Host) {
        let old = self.status;
        self.status = Status::NotVerifying;
        if old != Status::InProgress {
            self.preemptive_answer = None;
        }
        self.step = 0;
        self.cancel_conversations(host);
        self.clear_message_queue();
        let reuse = self.preemptive_answer.is_some() || self.preemptive_input;
        self.query = None;
        self.queried = false;
        self.drop_waiters();
        self.fingerprint_unavailable = false;
        if !reuse {
            self.clear_entry();
        }
        self.secret = true;
        self.hint = DEFAULT_QUESTION.to_owned();
    }

    fn soft_restart(&mut self, host: &dyn Host) {
        self.reset(host);
        self.begin(host);
    }

    fn cancel_conversations(&mut self, host: &dyn Host) {
        let ids: Vec<u64> = [self.password.take(), self.fingerprint.take()]
            .into_iter()
            .flatten()
            .collect();
        if !ids.is_empty() {
            let _ = host.cancel();
            self.retiring.extend(ids);
        }
        self.password_problem = false;
        self.fingerprint_problem = false;
        self.begin_wanted = false;
    }

    /// authPrompt.js `begin`; the services start once every cancelled
    /// conversation has retired.
    fn begin(&mut self, host: &dyn Host) {
        if !self.preemptive_input {
            self.set_sensitive(false);
        }
        self.status = Status::Verifying;
        self.resume_wanted = false;
        if self.retiring.is_empty() {
            self.start_services(host);
        } else {
            self.begin_wanted = true;
        }
    }

    fn start_services(&mut self, host: &dyn Host) {
        self.begin_wanted = false;
        if !self.legacy {
            let Some(id) = host.begin(AuthService::Password) else {
                self.init_error(host);
                return;
            };
            self.password = Some(id);
            self.trace_begin(AuthService::Password, id);
        } else if self.legacy_pending.is_none() {
            self.set_sensitive(true);
        }
        if host.fingerprint_enabled() && !self.fingerprint_unavailable {
            if let Some(id) = host.begin(AuthService::Fingerprint) {
                self.fingerprint = Some(id);
                self.trace_begin(AuthService::Fingerprint, id);
            }
        }
    }

    fn trace_begin(&mut self, service: AuthService, id: u64) {
        self.effects.push(Effect::Trace(format!(
            "lock auth begin service={} request={id}",
            service_name(service)
        )));
    }

    /// userVerifier.js `_reportInitError`. The entry stays usable (what is
    /// submitted is held), and the begin is retried with the connection.
    fn init_error(&mut self, host: &dyn Host) {
        self.status = Status::Failed;
        self.resume_wanted = true;
        self.queue_message(
            host,
            AuthService::Password,
            Some(INIT_ERROR.to_owned()),
            MessageKind::Error,
            false,
        );
        self.set_sensitive(true);
    }

    /// authPrompt.js `_onAskQuestion`.
    fn ask_question(&mut self, host: &dyn Host, sequence: u32, question: &str, secret: bool) {
        if self.queried {
            // `clear()`: the previous answer goes before the next question.
            self.clear_entry();
        }
        self.queried = true;
        self.step += 1;
        self.query = Some(sequence);
        let label = question_label(question);
        self.effects.push(Effect::Trace(format!(
            "lock auth query service=password step={} secret={secret} question={label}",
            self.step
        )));
        self.preemptive_input = false;
        if !secret && self.secret {
            // Hidden input is never carried into visible input, nor sent
            // as the answer to a visible question.
            self.preemptive_answer = None;
            self.clear_entry();
        }
        if let Some(answer) = self.preemptive_answer.take() {
            self.query = None;
            self.complete_pending(host, sequence, answer);
            return;
        }
        self.secret = secret;
        self.hint = label;
        self.set_sensitive(true);
    }

    /// `_completePendingCallback`: answers wait for queued messages.
    fn complete_pending(&mut self, host: &dyn Host, sequence: u32, answer: String) {
        let Some(request) = self.password else {
            return;
        };
        if self.queue.is_empty() {
            self.send_answer(host, request, sequence, answer);
        } else {
            self.deferred = Some((request, sequence, answer));
        }
    }

    /// Answer `sequence` of `request`, if that conversation still runs.
    fn send_answer(&mut self, host: &dyn Host, request: u64, sequence: u32, answer: String) {
        if self.password != Some(request) {
            return;
        }
        if host
            .answer(AuthService::Password, sequence, answer)
            .is_some()
        {
            self.effects.push(Effect::Trace(format!(
                "lock auth answer service=password step={}",
                self.step
            )));
        }
    }

    /// Password said Unavailable: no conversation backend (greetd), so the
    /// prompt sends one password with `Unlock`, as before conversations.
    fn fall_back(&mut self, host: &dyn Host) {
        self.legacy = true;
        self.query = None;
        self.effects
            .push(Effect::Trace("lock auth fallback legacy-unlock".to_owned()));
        self.secret = true;
        self.hint = DEFAULT_QUESTION.to_owned();
        self.preemptive_input = false;
        match self.preemptive_answer.take() {
            Some(answer) => {
                self.status = Status::Verifying;
                self.legacy_submit(host, answer);
            }
            None => self.set_sensitive(true),
        }
    }

    fn legacy_submit(&mut self, host: &dyn Host, password: String) {
        if self.legacy_pending.is_some() {
            return;
        }
        self.effects
            .push(Effect::Trace("lock password submitted".to_owned()));
        self.clear_message_queue();
        self.set_sensitive(false);
        match host.unlock(password) {
            Some(id) => self.legacy_pending = Some(id),
            None => self.legacy_failed(host),
        }
    }

    fn legacy_failed(&mut self, host: &dyn Host) {
        self.clear_entry();
        self.set_sensitive(true);
        self.queue_priority_message(
            host,
            AuthService::Password,
            Some(LEGACY_FAILED.to_owned()),
            MessageKind::Error,
            false,
        );
    }

    fn legacy_result(&mut self, host: &dyn Host, id: u64, applied: bool) {
        self.effects.push(Effect::Trace(format!(
            "lock command result applied={applied} request_id={id}"
        )));
        self.legacy_pending = None;
        self.set_sensitive(true);
        if !applied {
            self.effects.push(Effect::ShowPrompt);
            if self.status == Status::NotVerifying {
                self.reset(host);
                self.begin(host);
            }
            self.legacy_failed(host);
            self.effects.push(Effect::WiggleEntry);
        }
    }

    /// authServices.js `_verificationFailed` with a retry (the unlock
    /// screen always retries), then authPrompt.js `_onVerificationFailed`:
    /// clear and reopen the entry, wiggle it if the password service was
    /// asking, and soft-reset once queued messages are read.
    fn verification_failed(&mut self, host: &dyn Host, service: AuthService) {
        self.filter_service_messages(host, service, MessageKind::Error);
        let was_querying = service == AuthService::Password && self.queried;
        if was_querying {
            self.queried = false;
        }
        self.status = Status::Verifying;
        self.clear_entry();
        self.preemptive_input = true;
        self.set_sensitive(true);
        if was_querying {
            self.effects.push(Effect::WiggleEntry);
        }
        // `_waitPendingMessages`: at once with nothing queued, else when
        // the queue runs out, at most 10 s.
        if self.reset_wait.is_none() && !self.ready_reset {
            if self.queue.is_empty() {
                self.ready_reset = true;
            } else {
                let token = self.token();
                self.reset_wait = Some(token);
                self.effects.push(Effect::Timer {
                    token,
                    ms: WAIT_PENDING_MESSAGES_MS,
                });
            }
        }
    }

    /// Nothing waits on the queue any more (the conversation it served
    /// ended).
    fn drop_waiters(&mut self) {
        self.deferred = None;
        self.ready_answer = None;
        self.reset_wait = None;
        self.ready_reset = false;
        self.restart_wait = None;
    }

    /// Run what the queue released (`no-more-messages`) or found nothing
    /// to wait for: GNOME resolves those promises after the handler that
    /// released them.
    fn flush(&mut self, host: &dyn Host) {
        loop {
            if let Some((request, sequence, answer)) = self.ready_answer.take() {
                self.send_answer(host, request, sequence, answer);
            } else if std::mem::take(&mut self.ready_reset) {
                if self.status != Status::Succeeded {
                    self.soft_restart(host);
                }
            } else {
                break;
            }
        }
    }

    /// userVerifier.js `_queueMessage`.
    fn queue_message(
        &mut self,
        host: &dyn Host,
        service: AuthService,
        text: Option<String>,
        kind: MessageKind,
        wiggle: bool,
    ) {
        let interval = message_interval_ms(text.as_deref(), self.multiplier);
        self.queue.push_back(Message {
            service,
            text,
            kind,
            wiggle,
            interval,
        });
        self.queue_message_timeout(host);
    }

    /// userVerifier.js `_queueMessageTimeout`: show the head, and once its
    /// wiggle and read time are over, the next.
    fn queue_message_timeout(&mut self, host: &dyn Host) {
        if self.queue_timer.is_some() {
            return;
        }
        let Some(message) = self.queue.front().cloned() else {
            return;
        };
        match message.text.as_deref().filter(|t| !t.is_empty()) {
            Some(text) => {
                self.message = Some((text.to_owned(), message.kind));
                self.shown = Some((message.service, message.kind));
                self.effects.push(Effect::Announce(text.to_owned()));
                self.effects.push(Effect::Trace(format!(
                    "lock auth message type={} text={text}",
                    message.kind.trace_name()
                )));
            }
            None => {
                self.message = None;
                self.shown = None;
            }
        }
        let wiggle = if message.wiggle && message.text.is_some() {
            self.effects.push(Effect::WiggleMessage);
            host.wiggle_ms()
        } else {
            0
        };
        let token = self.token();
        self.queue_timer = Some(token);
        self.effects.push(Effect::Timer {
            token,
            ms: wiggle + message.interval,
        });
    }

    /// userVerifier.js `_queuePriorityMessage`: drop the service's
    /// lesser messages and duplicates; if that drops the one on screen,
    /// show the new one at once.
    fn queue_priority_message(
        &mut self,
        host: &dyn Host,
        service: AuthService,
        text: Option<String>,
        kind: MessageKind,
        wiggle: bool,
    ) {
        let keep = |m: &Message| (m.service != service || m.kind >= kind) && m.text != text;
        let front_kept = self.queue.front().is_some_and(keep);
        let kept: VecDeque<Message> = self.queue.iter().filter(|m| keep(m)).cloned().collect();
        if !front_kept {
            self.clear_message_queue();
        }
        self.queue = kept;
        self.queue_message(host, service, text, kind, wiggle);
    }

    /// userVerifier.js `_filterServiceMessages`.
    fn filter_service_messages(
        &mut self,
        host: &dyn Host,
        service: AuthService,
        kind: MessageKind,
    ) {
        if self.queue.iter().any(|m| m.service == service) {
            self.queue_priority_message(host, service, None, kind, false);
        }
    }

    fn finish_message_queue(&mut self) {
        if self.queue.is_empty() {
            return;
        }
        self.queue.clear();
        // Only waiters registered by now are released.
        if let Some(answer) = self.deferred.take() {
            self.ready_answer = Some(answer);
        }
        if self.reset_wait.take().is_some() {
            self.ready_reset = true;
        }
    }

    /// userVerifier.js `_clearMessageQueue`.
    fn clear_message_queue(&mut self) {
        self.finish_message_queue();
        self.queue_timer = None;
        self.message = None;
        self.shown = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Call {
        Begin(AuthService, u64),
        Answer(u32, String),
        Cancel,
        Unlock(String),
    }

    struct Fake {
        calls: RefCell<Vec<Call>>,
        next: Cell<u64>,
        fingerprint: Cell<bool>,
        sendable: Cell<bool>,
    }

    impl Fake {
        fn new() -> Self {
            Self {
                calls: RefCell::default(),
                next: Cell::new(100),
                fingerprint: Cell::new(true),
                sendable: Cell::new(true),
            }
        }
        fn id(&self) -> Option<u64> {
            if !self.sendable.get() {
                return None;
            }
            self.next.set(self.next.get() + 1);
            Some(self.next.get())
        }
        fn take(&self) -> Vec<Call> {
            std::mem::take(&mut self.calls.borrow_mut())
        }
        fn begins(&self) -> Vec<(AuthService, u64)> {
            self.calls
                .borrow()
                .iter()
                .filter_map(|c| match c {
                    Call::Begin(s, id) => Some((*s, *id)),
                    _ => None,
                })
                .collect()
        }
    }

    impl Host for Fake {
        fn begin(&self, service: AuthService) -> Option<u64> {
            let id = self.id()?;
            self.calls.borrow_mut().push(Call::Begin(service, id));
            Some(id)
        }
        fn answer(&self, _: AuthService, sequence: u32, answer: String) -> Option<u64> {
            let id = self.id()?;
            self.calls.borrow_mut().push(Call::Answer(sequence, answer));
            Some(id)
        }
        fn cancel(&self) -> Option<u64> {
            let id = self.id()?;
            self.calls.borrow_mut().push(Call::Cancel);
            Some(id)
        }
        fn unlock(&self, password: String) -> Option<u64> {
            let id = self.id()?;
            self.calls.borrow_mut().push(Call::Unlock(password));
            Some(id)
        }
        fn fingerprint_enabled(&self) -> bool {
            self.fingerprint.get()
        }
        fn wiggle_ms(&self) -> u64 {
            390
        }
    }

    const PW: AuthService = AuthService::Password;
    const FP: AuthService = AuthService::Fingerprint;

    fn query(sequence: u32, question: &str, secret: bool) -> AuthEvent {
        AuthEvent::Query {
            sequence,
            question: question.into(),
            secret,
        }
    }
    fn problem(text: &str) -> AuthEvent {
        AuthEvent::Problem { text: text.into() }
    }
    fn info(text: &str) -> AuthEvent {
        AuthEvent::Info { text: text.into() }
    }

    /// A shown prompt; returns (password id, fingerprint id).
    fn started(m: &mut AuthMachine, host: &Fake) -> (u64, u64) {
        m.show_prompt(host, false);
        let begins = host.begins();
        host.take();
        assert_eq!(begins.len(), 2);
        (begins[0].1, begins[1].1)
    }

    fn timers(effects: &[Effect]) -> Vec<(u64, u64)> {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::Timer { token, ms } => Some((*token, *ms)),
                _ => None,
            })
            .collect()
    }

    fn traces(effects: &[Effect]) -> Vec<String> {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::Trace(t) => Some(t.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn questions_are_normalised_like_auth_prompt() {
        assert_eq!(question_label("Password:"), "Password");
        assert_eq!(question_label("Password: "), "Password");
        assert_eq!(question_label("Verification code: "), "Verification code");
        assert_eq!(question_label("  PIN：  "), "PIN");
        assert_eq!(question_label("Token::"), "Token:");
        assert_eq!(question_label(" Say the word "), "Say the word");
        assert_eq!(question_label("Mot de passe :"), "Mot de passe");
    }

    #[test]
    fn fingerprint_hint_follows_reader_type() {
        assert_eq!(fingerprint_hint(true), "(or swipe finger across reader)");
        assert_eq!(fingerprint_hint(false), "(or place finger on reader)");
    }

    #[test]
    fn message_read_time_is_48_ms_a_char_at_least_2_s() {
        assert_eq!(message_interval_ms(None, 1.0), 0);
        assert_eq!(message_interval_ms(Some(""), 1.0), 0);
        assert_eq!(message_interval_ms(Some("short"), 1.0), 2000);
        let long = "x".repeat(50);
        assert_eq!(message_interval_ms(Some(&long), 1.0), 2400);
        assert_eq!(message_interval_ms(Some(&long), 2.0), 4800);
        // UTF-16 code units, as JavaScript counts them.
        assert_eq!(message_interval_ms(Some(&"😀".repeat(25)), 1.0), 2400);
        assert_eq!(parse_message_multiplier(Some("0.5")), 0.5);
        assert_eq!(parse_message_multiplier(Some("-1")), 1.0);
        assert_eq!(parse_message_multiplier(None), 1.0);
    }

    #[test]
    fn showing_the_prompt_begins_both_services_once() {
        let host = Fake::new();
        let mut m = AuthMachine::new(1.0);
        m.show_prompt(&host, false);
        assert_eq!(host.begins(), vec![(PW, 101), (FP, 102)]);
        let t = traces(&m.take_effects());
        assert!(t.contains(&"lock auth begin service=password request=101".to_owned()));
        assert!(t.contains(&"lock auth begin service=fingerprint request=102".to_owned()));
        m.show_prompt(&host, false);
        assert_eq!(host.begins().len(), 2, "an existing conversation is kept");
        assert!(m.view().sensitive);
    }

    #[test]
    fn fingerprint_setting_off_begins_password_only() {
        let host = Fake::new();
        host.fingerprint.set(false);
        let mut m = AuthMachine::new(1.0);
        m.show_prompt(&host, false);
        assert_eq!(host.begins(), vec![(PW, 101)]);
    }

    #[test]
    fn steps_count_queries_and_back_shows_from_the_second() {
        let host = Fake::new();
        let mut m = AuthMachine::new(1.0);
        let (pw, _) = started(&mut m, &host);
        m.event(&host, pw, PW, &query(1, "Password:", true));
        let v = m.view();
        assert_eq!(
            (v.secret, v.hint.as_str(), v.back, v.sensitive),
            (true, "Password", false, true)
        );
        assert_eq!(m.prompt_step(), 1);
        m.submit(&host, "hunter2".into());
        assert_eq!(host.take(), vec![Call::Answer(1, "hunter2".into())]);
        assert!(!m.view().sensitive);
        let e = m.take_effects();
        assert!(traces(&e).contains(&"lock auth answer service=password step=1".to_owned()));
        assert!(traces(&e).iter().all(|t| !t.contains("hunter2")));
        m.event(&host, pw, PW, &query(2, "Verification code: ", false));
        let v = m.view();
        assert_eq!(
            (v.secret, v.hint.as_str(), v.back, v.sensitive),
            (false, "Verification code", true, true)
        );
        let e = m.take_effects();
        assert!(
            e.contains(&Effect::ClearEntry),
            "the next question clears the entry"
        );
        assert!(traces(&e).contains(
            &"lock auth query service=password step=2 secret=false question=Verification code"
                .to_owned()
        ));
        m.submit(&host, "123456".into());
        assert_eq!(host.take(), vec![Call::Answer(2, "123456".into())]);
        assert!(m.result(&host, pw, true));
        assert!(!m.view().sensitive);
        assert!(!m.cancel(&host), "verified: nothing to cancel");
        assert!(host.take().is_empty());
    }

    #[test]
    fn back_at_a_later_step_soft_resets_after_retirement() {
        let host = Fake::new();
        let mut m = AuthMachine::new(1.0);
        let (pw, fp) = started(&mut m, &host);
        m.event(&host, pw, PW, &query(1, "Password:", true));
        m.submit(&host, "pw".into());
        m.event(&host, pw, PW, &query(2, "PIN:", true));
        host.take();
        assert!(!m.cancel(&host), "step 2 stays on the prompt");
        assert_eq!(
            host.take(),
            vec![Call::Cancel],
            "no begin before retirement"
        );
        assert_eq!(m.prompt_step(), 0);
        assert!(!m.view().back);
        assert_eq!(m.view().hint, "Password");
        // Stale steps of the cancelled conversation are ignored.
        m.event(&host, pw, PW, &query(3, "Other:", false));
        assert_eq!(m.prompt_step(), 0);
        assert!(m.result(&host, pw, false));
        assert!(host.take().is_empty(), "fingerprint still owed");
        assert!(m.result(&host, fp, false));
        assert_eq!(host.begins().len(), 2, "both begin again once retired");
    }

    #[test]
    fn back_or_escape_at_the_first_step_stops_and_returns_to_the_clock() {
        let host = Fake::new();
        let mut m = AuthMachine::new(1.0);
        let (pw, _) = started(&mut m, &host);
        m.event(&host, pw, PW, &query(1, "Password:", true));
        assert!(m.cancel(&host));
        assert_eq!(host.take(), vec![Call::Cancel]);
        assert!(m.take_effects().contains(&Effect::ClearEntry));
        // The clock settling again sends nothing more.
        m.stop(&host);
        assert!(host.take().is_empty());
    }

    #[test]
    fn a_wake_key_then_submit_before_the_query_answers_it_once() {
        let host = Fake::new();
        let mut m = AuthMachine::new(1.0);
        // `_onKeyPress`: `_showPrompt()` resets and begins, then
        // `startPreemptiveInput(unichar)`; the reset's clearing comes
        // before the wake character is inserted.
        m.show_prompt(&host, true);
        let e = m.take_effects();
        assert!(e.contains(&Effect::ClearEntry));
        assert!(m.view().sensitive, "the wake key's entry takes input");
        let pw = host.begins()[0].1;
        host.take();
        m.text_changed(true); // the wake character
        m.submit(&host, "wake-and-rest".into());
        assert!(host.take().is_empty());
        m.event(&host, pw, PW, &query(1, "Password:", true));
        assert_eq!(host.take(), vec![Call::Answer(1, "wake-and-rest".into())]);
        assert!(
            !m.take_effects().contains(&Effect::ClearEntry),
            "nothing typed is cleared"
        );
        assert!(m.result(&host, pw, true));
    }

    #[test]
    fn a_wake_key_survives_the_first_query() {
        let host = Fake::new();
        let mut m = AuthMachine::new(1.0);
        m.show_prompt(&host, true);
        let pw = host.begins()[0].1;
        host.take();
        m.take_effects();
        m.text_changed(true);
        m.event(&host, pw, PW, &query(1, "Password:", true));
        let e = m.take_effects();
        assert!(!e.contains(&Effect::ClearEntry), "the wake character stays");
        assert!(m.view().sensitive);
        m.submit(&host, "wake-and-rest".into());
        assert_eq!(host.take(), vec![Call::Answer(1, "wake-and-rest".into())]);
    }

    #[test]
    fn preemptive_answer_goes_to_the_first_query_only() {
        let host = Fake::new();
        let mut m = AuthMachine::new(1.0);
        m.show_prompt(&host, true);
        let pw = host.begins()[0].1;
        host.take();
        m.submit(&host, "typed-early".into());
        assert!(host.take().is_empty(), "held until a question");
        assert!(!m.view().sensitive);
        m.event(&host, pw, PW, &query(1, "Password:", true));
        assert_eq!(host.take(), vec![Call::Answer(1, "typed-early".into())]);
        assert!(!m.view().sensitive, "auto-answered: still verifying");
        m.event(&host, pw, PW, &query(2, "Verification code:", false));
        assert!(host.take().is_empty(), "never reused for a later question");
        assert!(m.view().sensitive);
        assert_eq!(m.prompt_step(), 2);
    }

    #[test]
    fn answers_wait_for_queued_messages() {
        let host = Fake::new();
        let mut m = AuthMachine::new(1.0);
        let (pw, _) = started(&mut m, &host);
        m.take_effects();
        m.event(&host, pw, PW, &info("Your password expires in 3 days"));
        let e = m.take_effects();
        let (token, ms) = timers(&e)[0];
        assert_eq!(ms, 2000);
        assert_eq!(
            m.view().message,
            Some(("Your password expires in 3 days".into(), MessageKind::Info))
        );
        assert!(e.contains(&Effect::Announce("Your password expires in 3 days".into())));
        m.event(&host, pw, PW, &query(1, "Password:", true));
        m.submit(&host, "pw".into());
        assert!(host.take().is_empty());
        m.timeout(&host, token);
        assert_eq!(host.take(), vec![Call::Answer(1, "pw".into())]);
        // The read message stays until the text changes.
        assert!(m.view().message.is_some());
        m.text_changed(true);
        assert_eq!(m.view().message, None);
    }

    #[test]
    fn queued_messages_each_hold_their_read_time_and_problems_jump_in() {
        let host = Fake::new();
        let mut m = AuthMachine::new(1.0);
        let (pw, fp) = started(&mut m, &host);
        m.take_effects();
        m.event(&host, fp, FP, &AuthEvent::FingerprintScan { swipe: false });
        let (first, _) = timers(&m.take_effects())[0];
        assert_eq!(
            m.view().message,
            Some(("(or place finger on reader)".into(), MessageKind::Hint))
        );
        m.event(&host, pw, PW, &info("Insert token"));
        assert!(
            timers(&m.take_effects()).is_empty(),
            "waits behind the hint"
        );
        assert_eq!(m.view().message.unwrap().1, MessageKind::Hint);
        m.timeout(&host, first);
        let e = m.take_effects();
        assert_eq!(
            m.view().message,
            Some(("Insert token".into(), MessageKind::Info))
        );
        let (second, _) = timers(&e)[0];
        // A password problem drops the password info on screen at once.
        m.event(&host, pw, PW, &problem("Bad"));
        assert_eq!(m.view().message, Some(("Bad".into(), MessageKind::Error)));
        let (bad, ms) = timers(&m.take_effects())[0];
        assert_eq!(ms, 2000, "a problem holds the queue like any message");
        m.timeout(&host, second);
        assert_eq!(
            m.view().message.unwrap().0,
            "Bad",
            "the stale timer is ignored"
        );
        // Another service's problem queues behind one on screen.
        m.event(&host, fp, FP, &problem("Failed to match fingerprint"));
        assert_eq!(m.view().message.unwrap().0, "Bad");
        assert!(!m.take_effects().contains(&Effect::WiggleMessage));
        // Then shows, wiggling, its read time after the wiggle.
        m.timeout(&host, bad);
        let e = m.take_effects();
        assert!(e.contains(&Effect::WiggleMessage));
        assert_eq!(timers(&e)[0].1, 390 + 2000);
        assert_eq!(
            m.view().message,
            Some(("Failed to match fingerprint".into(), MessageKind::Error))
        );
        // With nothing queued, a fingerprint problem shows at once.
        m.timeout(&host, timers(&e)[0].0);
        m.event(
            &host,
            fp,
            FP,
            &problem("Place your finger on the reader again"),
        );
        let e = m.take_effects();
        assert!(e.contains(&Effect::WiggleMessage));
        assert_eq!(
            m.view().message.unwrap().0,
            "Place your finger on the reader again"
        );
    }

    #[test]
    fn password_failure_wiggles_keeps_the_problem_and_restarts_after_it() {
        let host = Fake::new();
        let mut m = AuthMachine::new(1.0);
        let (pw, fp) = started(&mut m, &host);
        m.event(&host, pw, PW, &query(1, "Password:", true));
        m.submit(&host, "wrong".into());
        host.take();
        m.take_effects();
        let sorry = "Sorry, password authentication didn’t work. Please try again.";
        m.event(&host, pw, PW, &problem(sorry));
        let (problem_timer, _) = timers(&m.take_effects())[0];
        assert!(m.result(&host, pw, false));
        let e = m.take_effects();
        assert!(e.contains(&Effect::WiggleEntry));
        assert!(e.contains(&Effect::ClearEntry));
        assert!(e.contains(&Effect::Trace(format!(
            "lock auth result service=password request={pw} applied=false"
        ))));
        assert_eq!(m.view().message, Some((sorry.into(), MessageKind::Error)));
        assert!(m.view().sensitive, "typing may start again at once");
        assert!(
            host.take().is_empty(),
            "no restart while the problem is read"
        );
        // The problem's read time ends: cancel fingerprint, then both
        // begin again after it retired.
        m.timeout(&host, problem_timer);
        assert_eq!(m.view().message, None);
        assert!(
            host.take().is_empty(),
            "the filter's hidden message is next"
        );
        for (token, ms) in timers(&m.take_effects()) {
            assert_eq!(ms, 0);
            m.timeout(&host, token);
        }
        assert_eq!(host.take(), vec![Call::Cancel]);
        assert_eq!(m.view().message, None);
        m.result(&host, fp, false);
        assert_eq!(host.begins().len(), 2);
    }

    #[test]
    fn a_failure_restart_is_bounded_by_ten_seconds() {
        let host = Fake::new();
        let mut m = AuthMachine::new(1000.0);
        let (pw, _) = started(&mut m, &host);
        m.event(&host, pw, PW, &problem("Sorry"));
        m.result(&host, pw, false);
        let e = m.take_effects();
        let wait = timers(&e)
            .into_iter()
            .find(|(_, ms)| *ms == WAIT_PENDING_MESSAGES_MS)
            .unwrap();
        host.take();
        m.timeout(&host, wait.0);
        assert_eq!(host.take(), vec![Call::Cancel]);
    }

    #[test]
    fn text_submitted_during_the_failure_wait_answers_the_restart() {
        let host = Fake::new();
        let mut m = AuthMachine::new(1.0);
        let (pw, fp) = started(&mut m, &host);
        m.event(&host, pw, PW, &query(1, "Password:", true));
        m.submit(&host, "wrong".into());
        m.event(&host, pw, PW, &problem("Sorry"));
        m.take_effects();
        m.result(&host, pw, false);
        let wait = timers(&m.take_effects())[0].0;
        m.submit(&host, "right".into());
        host.take();
        m.timeout(&host, wait);
        m.result(&host, fp, false);
        let pw2 = host.begins()[0].1;
        host.take();
        m.event(&host, pw2, PW, &query(1, "Password:", true));
        assert_eq!(host.take(), vec![Call::Answer(1, "right".into())]);
    }

    #[test]
    fn denied_without_problem_while_the_other_runs_is_a_cancellation() {
        let host = Fake::new();
        let mut m = AuthMachine::new(1.0);
        let (pw, fp) = started(&mut m, &host);
        m.event(&host, pw, PW, &query(1, "Password:", true));
        // Fingerprint wins: password is Denied first, no Problem, while
        // fingerprint is still current.
        assert!(m.result(&host, pw, false));
        assert!(host.take().is_empty());
        let e = m.take_effects();
        assert!(!e.contains(&Effect::WiggleEntry));
        assert!(
            timers(&e).is_empty(),
            "no restart while a conversation runs"
        );
        assert!(m.result(&host, fp, true));
        assert!(host.take().is_empty());
        // Likewise a fingerprint Denied without a Problem while password
        // still runs.
        let host = Fake::new();
        let mut m = AuthMachine::new(1.0);
        let (_, fp) = started(&mut m, &host);
        m.result(&host, fp, false);
        assert!(host.take().is_empty());
        assert!(timers(&m.take_effects()).is_empty());
    }

    #[test]
    fn our_own_cancellation_never_restarts() {
        let host = Fake::new();
        let mut m = AuthMachine::new(1.0);
        let (pw, fp) = started(&mut m, &host);
        m.event(&host, pw, PW, &query(1, "Password:", true));
        assert!(m.cancel(&host), "step 1: back to the clock");
        assert_eq!(host.take(), vec![Call::Cancel]);
        m.take_effects();
        assert!(m.result(&host, pw, false));
        assert!(m.result(&host, fp, false));
        assert!(host.take().is_empty());
        assert!(timers(&m.take_effects()).is_empty());
    }

    #[test]
    fn a_prompt_left_with_no_conversation_restarts_after_a_backoff() {
        let host = Fake::new();
        let mut m = AuthMachine::new(1.0);
        let (pw, fp) = started(&mut m, &host);
        m.event(&host, pw, PW, &query(1, "Password:", true));
        m.take_effects();
        // A lock-epoch change cancels both: no Problem for either.
        assert!(m.result(&host, fp, false));
        assert!(timers(&m.take_effects()).is_empty(), "password still runs");
        assert!(m.result(&host, pw, false));
        let e = m.take_effects();
        assert_eq!(timers(&e).len(), 1);
        let (backoff, ms) = timers(&e)[0];
        assert_eq!(ms, RESTART_BACKOFF_MS);
        assert!(host.take().is_empty(), "nothing begins before the backoff");
        m.timeout(&host, backoff);
        let services: Vec<AuthService> = host.begins().iter().map(|(s, _)| *s).collect();
        assert_eq!(services, vec![PW, FP]);
        assert_eq!(m.prompt_step(), 0);
        // A stale backoff after a reset does nothing.
        host.take();
        m.timeout(&host, backoff);
        assert!(host.take().is_empty());
    }

    #[test]
    fn a_cancelled_conversation_never_replays_a_held_answer() {
        let host = Fake::new();
        host.fingerprint.set(false);
        let mut m = AuthMachine::new(1.0);
        m.show_prompt(&host, true);
        let pw = host.begins()[0].1;
        host.take();
        // Typed before PAM asked (e.g. a delay module), then a suspend
        // cancels the conversation: Denied, no Problem, nothing running.
        m.submit(&host, "typed-before-suspend".into());
        m.take_effects();
        m.result(&host, pw, false);
        let (backoff, _) = timers(&m.take_effects())[0];
        m.timeout(&host, backoff);
        assert!(m.take_effects().contains(&Effect::ClearEntry));
        let pw2 = host.begins()[0].1;
        host.take();
        m.event(&host, pw2, PW, &query(1, "Password:", true));
        assert!(host.take().is_empty(), "the held answer is never replayed");
        assert!(m.view().sensitive, "the new question takes fresh input");
        m.submit(&host, "fresh".into());
        assert_eq!(host.take(), vec![Call::Answer(1, "fresh".into())]);
    }

    #[test]
    fn hidden_input_never_reaches_a_visible_question() {
        let host = Fake::new();
        let mut m = AuthMachine::new(1.0);
        m.show_prompt(&host, true);
        let pw = host.begins()[0].1;
        host.take();
        m.take_effects();
        m.submit(&host, "hidden-early".into());
        m.event(&host, pw, PW, &query(1, "Username:", false));
        assert!(host.take().is_empty(), "not sent as a visible answer");
        assert!(m.take_effects().contains(&Effect::ClearEntry));
        let v = m.view();
        assert_eq!(
            (v.secret, v.hint.as_str(), v.sensitive),
            (false, "Username", true)
        );
        m.submit(&host, "visible".into());
        assert_eq!(host.take(), vec![Call::Answer(1, "visible".into())]);
    }

    /// GDM 51 reports `PAM_MAXTRIES` as service-unavailable with the
    /// max-tries message (gdm-session.c `report_and_stop_conversation`);
    /// on the unlock screen authServices.js `_canRetry` always retries and
    /// authServicesLegacy.js `_handleOnConversationStopped` returns early
    /// for an unavailable service, so the prompt stays and fingerprint
    /// stays off until the next reset.
    #[test]
    fn fingerprint_max_tries_stops_fingerprint_and_keeps_the_prompt() {
        let max = "You reached the maximum fingerprint authentication attempts, please try another method";
        let host = Fake::new();
        let mut m = AuthMachine::new(1.0);
        let (pw, fp) = started(&mut m, &host);
        m.event(&host, pw, PW, &query(1, "Password:", true));
        m.text_changed(true);
        m.take_effects();
        m.event(&host, fp, FP, &problem(max));
        m.event(&host, fp, FP, &AuthEvent::Unavailable);
        m.result(&host, fp, false);
        let e = m.take_effects();
        assert!(e.contains(&Effect::WiggleMessage), "the message wiggles");
        assert!(e.contains(&Effect::Announce(max.into())));
        assert!(
            !e.iter()
                .any(|e| matches!(e, Effect::ClearEntry | Effect::WiggleEntry)),
            "the typed password stays: {e:?}"
        );
        assert_eq!(m.view().message, Some((max.into(), MessageKind::Error)));
        // Its read time passes; nothing cancels or begins again.
        let mut pending = timers(&e);
        while let Some((token, _)) = pending.pop() {
            m.timeout(&host, token);
            pending.extend(timers(&m.take_effects()));
        }
        assert_eq!(m.view().message, None);
        assert!(host.take().is_empty(), "no cancel, no restart");
        let v = m.view();
        assert_eq!(
            (v.secret, v.hint.as_str(), v.sensitive),
            (true, "Password", true)
        );
        assert_eq!(m.prompt_step(), 1);
        m.submit(&host, "secret".into());
        assert_eq!(host.take(), vec![Call::Answer(1, "secret".into())]);
        // A password failure's reset tries the reader again.
        m.event(&host, pw, PW, &problem("Sorry"));
        m.result(&host, pw, false);
        let wait = timers(&m.take_effects())
            .into_iter()
            .find(|(_, ms)| *ms == WAIT_PENDING_MESSAGES_MS)
            .unwrap()
            .0;
        host.take();
        m.timeout(&host, wait);
        let services: Vec<AuthService> = host.begins().iter().map(|(s, _)| *s).collect();
        assert_eq!(
            services,
            vec![AuthService::Password, AuthService::Fingerprint]
        );
    }

    #[test]
    fn fingerprint_max_tries_then_the_clock_offers_fingerprint_again() {
        let host = Fake::new();
        let mut m = AuthMachine::new(1.0);
        let (pw, fp) = started(&mut m, &host);
        m.event(&host, fp, FP, &problem("max"));
        m.event(&host, fp, FP, &AuthEvent::Unavailable);
        m.result(&host, fp, false);
        host.take();
        assert!(m.cancel(&host), "step 0: Escape returns to the clock");
        assert_eq!(host.take(), vec![Call::Cancel]);
        m.result(&host, pw, false);
        m.show_prompt(&host, false);
        let services: Vec<AuthService> = host.begins().iter().map(|(s, _)| *s).collect();
        assert_eq!(
            services,
            vec![AuthService::Password, AuthService::Fingerprint]
        );
    }

    #[test]
    fn fingerprint_unavailable_drops_the_hint_without_restart() {
        let host = Fake::new();
        let mut m = AuthMachine::new(1.0);
        let (pw, fp) = started(&mut m, &host);
        m.event(&host, fp, FP, &AuthEvent::FingerprintScan { swipe: true });
        assert_eq!(
            m.view().message,
            Some(("(or swipe finger across reader)".into(), MessageKind::Hint))
        );
        m.event(&host, fp, FP, &info("Verification timed out"));
        assert_eq!(
            m.view().message.as_ref().unwrap().1,
            MessageKind::Hint,
            "info is only a cue"
        );
        m.event(&host, fp, FP, &AuthEvent::Unavailable);
        assert_eq!(m.view().message, None);
        m.result(&host, fp, false);
        assert!(host.take().is_empty(), "no fingerprint restart");
        // A later reset for another reason tries the reader again.
        m.event(&host, pw, PW, &problem("Sorry"));
        m.result(&host, pw, false);
        let wait = timers(&m.take_effects())
            .into_iter()
            .find(|(_, ms)| *ms == WAIT_PENDING_MESSAGES_MS)
            .unwrap()
            .0;
        host.take();
        m.timeout(&host, wait);
        let services: Vec<AuthService> = host.begins().iter().map(|(s, _)| *s).collect();
        assert_eq!(
            services,
            vec![PW, FP],
            "both retired already: begin at once"
        );
    }

    #[test]
    fn password_unavailable_falls_back_to_single_password_unlock() {
        let host = Fake::new();
        let mut m = AuthMachine::new(1.0);
        m.show_prompt(&host, false);
        let (pw, fp) = (host.begins()[0].1, host.begins()[1].1);
        host.take();
        m.submit(&host, "early".into());
        m.event(&host, pw, PW, &AuthEvent::Unavailable);
        assert!(m.legacy());
        assert_eq!(host.take(), vec![Call::Unlock("early".into())]);
        let e = m.take_effects();
        assert!(traces(&e).contains(&"lock auth fallback legacy-unlock".to_owned()));
        let pending = m.legacy_pending().unwrap();
        assert!(m.result(&host, pw, false), "the unavailable begin's Denied");
        assert!(host.take().is_empty(), "never restarted");
        assert!(m.result(&host, pending, false));
        assert_eq!(
            m.view().message,
            Some((LEGACY_FAILED.into(), MessageKind::Error))
        );
        assert!(m.view().sensitive);
        assert_eq!(m.view().hint, "Password");
        let e = m.take_effects();
        assert!(e.contains(&Effect::ClearEntry));
        assert!(e.contains(&Effect::WiggleEntry));
        m.submit(&host, "again".into());
        assert_eq!(host.take(), vec![Call::Unlock("again".into())]);
        m.submit(&host, "while-pending".into());
        assert!(host.take().is_empty());
        // The fallback outlives a reset within this lock.
        let fp = Some(fp);
        assert!(m.cancel(&host));
        m.show_prompt(&host, false);
        if let Some(fp) = fp {
            m.result(&host, fp, false);
        }
        let services: Vec<AuthService> = host.begins().iter().map(|(s, _)| *s).collect();
        assert_eq!(services, vec![FP]);
    }

    #[test]
    fn stale_request_ids_are_ignored() {
        let host = Fake::new();
        let mut m = AuthMachine::new(1.0);
        let (pw, fp) = started(&mut m, &host);
        m.event(&host, 7, PW, &query(1, "Password:", true));
        m.event(&host, fp, PW, &query(1, "Password:", true));
        m.event(&host, pw, FP, &problem("x"));
        assert_eq!(m.prompt_step(), 0);
        assert_eq!(m.view().message, None);
        assert!(!m.result(&host, 7, false));
        assert!(!m.result(&host, 7, true));
    }

    #[test]
    fn unsendable_begin_reports_and_resumes_with_the_connection() {
        let host = Fake::new();
        host.sendable.set(false);
        let mut m = AuthMachine::new(1.0);
        m.show_prompt(&host, false);
        assert_eq!(
            m.view().message,
            Some((INIT_ERROR.into(), MessageKind::Error))
        );
        assert!(m.view().sensitive);
        m.submit(&host, "held".into());
        host.sendable.set(true);
        m.resume(&host);
        let pw = host.begins()[0].1;
        host.take();
        m.event(&host, pw, PW, &query(1, "Password:", true));
        assert!(host.take().is_empty(), "the error message is read first");
        let error = timers(&m.take_effects())
            .into_iter()
            .find(|(_, ms)| *ms == 2000)
            .unwrap();
        m.timeout(&host, error.0);
        assert_eq!(host.take(), vec![Call::Answer(1, "held".into())]);
    }

    #[test]
    fn reconnect_drops_everything_without_sending() {
        let host = Fake::new();
        let mut m = AuthMachine::new(1.0);
        let (pw, _) = started(&mut m, &host);
        m.event(&host, pw, PW, &query(1, "Password:", true));
        m.submit(&host, "in-flight".into());
        m.event(&host, pw, PW, &query(2, "PIN:", true));
        host.take();
        m.reconnected(&host);
        assert!(host.take().is_empty(), "no cancel, no begin, no answer");
        assert_eq!(m.prompt_step(), 0);
        assert!(m.view().sensitive);
        assert_eq!(m.view().message.unwrap().0, LEGACY_FAILED);
        assert!(
            !m.result(&host, pw, false),
            "the old connection's ids are gone"
        );
        m.resume(&host);
        assert_eq!(host.begins().len(), 2);
        m.resume(&host);
        assert_eq!(host.begins().len(), 2, "resumes once");
    }
}
