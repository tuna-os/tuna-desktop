//! The lock screen's authentication conversations (#366, P-LK-02): the
//! role GDM's UserVerifier plays for GNOME Shell 51's unlock prompt
//! (`js/gdm/authPrompt.js`, `authServicesLegacy.js`). The shell starts a
//! password and a fingerprint conversation side by side; this module
//! relays each PAM prompt and message of the password conversation
//! (private helper, `pam_backend::begin_conversation`) one question at a
//! time, runs `pam_fprintd`'s fingerprint loop over fprintd
//! (`unlock::run_fingerprint`), and unlocks only through the shared
//! [`apply_verified_unlock`](Runtime::apply_verified_unlock) grant.
//!
//! Ownership rules (docs/security/lock-conversations.md): each
//! conversation holds its own Slot attempt — the password one the common
//! unlock Slot, the fingerprint one its own — and answers its
//! `BeginVerification` only when that attempt has retired (helper reaped,
//! or fingerprint worker joined). Every prompt and every grant rechecks
//! the original connection, request and lock epoch; a lock-epoch change,
//! origin loss or unlock cancels. The winner grants only once the common
//! Slot is free, so a fingerprint match waits for the cancelled password
//! helper to retire.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use tuna_shell_control::{AuthEvent, AuthService, Secret};

use super::Runtime;
use crate::auth_owner::{Attempt, MethodKind, Origin, Slot};
use crate::pam_backend::{ConversationTicket, Verification};
use crate::pam_conversation::{Binding, Policy, ResponseHandle};
use crate::pam_helper::{Answer, AnswerText, Message, Style};
use crate::unlock::{FingerprintEnd, FingerprintStep};

/// GDM 51's `get_generic_error_message` for `gdm-password`
/// (`daemon/gdm-session-worker.c`): what GNOME's lock screen shows after
/// a failed password conversation.
pub(crate) const PASSWORD_FAILED: &str =
    "Sorry, password authentication didn’t work. Please try again.";

/// The admitted interactive policy for the lock service. A backend phase
/// (one PAM module step between two answers) gets 30 s: pam_unix's fail
/// delay is 2 s and network modules time out well inside it. The whole
/// conversation is capped at 15 minutes; the relay's own 120 s inactivity
/// watch (GNOME's unlock-dialog idle timeout) normally ends it first.
const POLICY: Policy = Policy {
    backend_phase: Duration::from_secs(30),
    total: Duration::from_secs(900),
};

/// The PAM batch being answered, one question at a time.
struct Batch {
    binding: Binding,
    sequence: u32,
    messages: Vec<Message>,
    response: Option<ResponseHandle>,
    answers: Vec<Answer>,
    /// The wire sequence of the question waiting for the shell's answer.
    asked: Option<u32>,
}

struct PasswordConversation {
    attempt: Attempt,
    origin: Origin,
    binding: Binding,
    ticket: ConversationTicket,
    source: Option<calloop::RegistrationToken>,
    batch: Option<Batch>,
    /// Questions asked so far: the wire `Query` sequence.
    queries: u32,
    canceled: bool,
}

impl PasswordConversation {
    fn cancel(&mut self) {
        self.canceled = true;
        // Dropping an unsubmitted response handle cancels the relay too.
        self.batch = None;
        self.ticket.cancel();
    }
}

struct FingerprintConversation {
    attempt: Attempt,
    origin: Origin,
    cancel: Arc<AtomicBool>,
    steps: mpsc::Receiver<FingerprintStep>,
    worker: Option<std::thread::JoinHandle<FingerprintEnd>>,
    canceled: bool,
}

impl FingerprintConversation {
    fn cancel(&mut self) {
        self.canceled = true;
        self.cancel.store(true, Ordering::Release);
    }
}

impl Drop for FingerprintConversation {
    fn drop(&mut self) {
        // Never join here: the reader worker winds down on its own.
        self.cancel.store(true, Ordering::Release);
    }
}

/// A verified success waiting for the common Slot to be free.
#[derive(Clone, Copy)]
struct Winner {
    origin: Origin,
    service: AuthService,
    epoch: u64,
}

/// The runtime's conversation state.
#[derive(Default)]
pub(super) struct Conversations {
    password: Option<PasswordConversation>,
    fingerprint: Option<FingerprintConversation>,
    fingerprint_slot: Slot,
    winner: Option<Winner>,
}

impl Conversations {
    /// Proof diagnostics only: which conversations still own an attempt.
    pub(super) fn diagnostics(&self) -> serde_json::Value {
        serde_json::json!({
            "password": self.password.is_some(),
            "fingerprint": self.fingerprint.is_some(),
            "fingerprint_slot": self.fingerprint_slot.active(),
            "winner": self.winner.is_some(),
        })
    }

    /// The common Slot attempt the password conversation owns, if any.
    pub(super) fn password_attempt(&self) -> Option<Attempt> {
        self.password
            .as_ref()
            .map(|conversation| conversation.attempt)
    }

    /// The password conversation's shared idle/total deadline.
    pub(super) fn next_deadline(&self) -> Option<Instant> {
        self.password
            .as_ref()
            .and_then(|conversation| conversation.ticket.next_deadline())
    }

    /// Real seat input reached the lock surface: GNOME's unlock dialog
    /// idle watch restarts on any input, so the relay's inactivity watch
    /// does too while a question is outstanding.
    pub(super) fn note_user_input(&mut self) {
        let Some(batch) = self
            .password
            .as_mut()
            .filter(|conversation| !conversation.canceled)
            .and_then(|conversation| conversation.batch.as_mut())
            .filter(|batch| batch.asked.is_some())
        else {
            return;
        };
        let (binding, sequence) = (batch.binding, batch.sequence);
        if let Some(response) = batch.response.as_mut() {
            let _ = response.note_user_input(binding, sequence);
        }
    }
}

fn service_name(service: AuthService) -> &'static str {
    match service {
        AuthService::Password => "password",
        AuthService::Fingerprint => "fingerprint",
    }
}

impl Runtime {
    fn trace_conversation(
        &self,
        event: &str,
        service: AuthService,
        origin: Origin,
        extra: serde_json::Value,
    ) {
        if std::env::var_os("TUNA_LOCK_TRACE").is_none() {
            return;
        }
        let mut doc = serde_json::json!({
            "event": event,
            "service": service_name(service),
            "request": origin.request,
            "connection": origin.connection,
            "epoch": self.unlock_generation.load(Ordering::Acquire),
            "locked": self.is_locked(),
        });
        if let (Some(doc), serde_json::Value::Object(extra)) = (doc.as_object_mut(), extra) {
            doc.extend(extra);
        }
        eprintln!("tuna-compositor: lock conversation {doc}");
    }

    /// Relay one step; a lost origin cancels that conversation.
    fn send_auth(&mut self, origin: Origin, service: AuthService, event: AuthEvent) -> bool {
        let name = match &event {
            AuthEvent::Info { .. } => "info",
            AuthEvent::Problem { .. } => "problem",
            AuthEvent::Query { .. } => "query",
            AuthEvent::FingerprintScan { .. } => "scan",
            AuthEvent::Unavailable => "unavailable",
        };
        let detail = match &event {
            AuthEvent::Query {
                sequence, secret, ..
            } => serde_json::json!({ "sequence": sequence, "secret": secret }),
            AuthEvent::FingerprintScan { swipe } => serde_json::json!({ "swipe": swipe }),
            _ => serde_json::json!({}),
        };
        let sent = self.control.send_auth_event(origin, service, event);
        let mut detail = detail;
        detail["sent"] = serde_json::Value::Bool(sent);
        self.trace_conversation(name, service, origin, detail);
        sent
    }

    /// Refuse a conversation that cannot start: GDM's service-unavailable,
    /// then its denied result.
    fn refuse_verification(&mut self, origin: Origin, service: AuthService) {
        self.send_auth(origin, service, AuthEvent::Unavailable);
        self.control.finish_unlock(origin, false);
    }

    /// The shell's `BeginVerification` for `service`.
    pub(super) fn begin_verification(&mut self, origin: Origin, service: AuthService) {
        self.trace_conversation("begin", service, origin, serde_json::json!({}));
        if !self.is_locked() || self.auth.winner.is_some() {
            self.control.finish_unlock(origin, false);
            return;
        }
        match service {
            AuthService::Password => self.begin_password(origin),
            AuthService::Fingerprint => self.begin_fingerprint(origin),
        }
    }

    fn begin_password(&mut self, origin: Origin) {
        // A quarantined helper is a permanent refusal: the shell falls back
        // to the single-password `Unlock` path.
        if self.unlock_quarantined.is_some() {
            self.refuse_verification(origin, AuthService::Password);
            return;
        }
        // A previous helper still retiring is transient: a plain denial the
        // shell retries, never `Unavailable` (which would end the service).
        if self.auth.password.is_some() || self.unlock_worker.is_some() {
            self.control.finish_unlock(origin, false);
            return;
        }
        let (Some(user), Some(backend)) = (
            crate::unlock::session_user(),
            crate::pam_backend::Backend::live(),
        ) else {
            self.refuse_verification(origin, AuthService::Password);
            return;
        };
        self.unlock_slot.note_lock(&self.unlock_generation);
        let epoch = self.unlock_generation.load(Ordering::Acquire);
        let Some(attempt) =
            self.unlock_slot
                .begin_method(Some(origin), epoch, MethodKind::PamConversation)
        else {
            self.refuse_verification(origin, AuthService::Password);
            return;
        };
        let binding = Binding {
            origin,
            attempt: attempt.serial,
            epoch,
        };
        let cancel = crate::pam_cancel::Cancellation::default();
        match backend.begin_conversation(crate::pam::service(), user, binding, POLICY, cancel) {
            Ok(mut ticket) => {
                // The owned notice only wakes the loop; the tick reads the queues.
                let source = ticket.take_notice_source().and_then(|ping| {
                    self.loop_handle
                        .insert_source(ping, |_, _, _: &mut Runtime| {})
                        .ok()
                });
                self.auth.password = Some(PasswordConversation {
                    attempt,
                    origin,
                    binding,
                    ticket,
                    source,
                    batch: None,
                    queries: 0,
                    canceled: false,
                });
            }
            Err(result) => {
                if result.may_retire() {
                    self.unlock_slot.retire_pam_method(
                        attempt,
                        MethodKind::PamConversation,
                        result,
                    );
                } else {
                    self.unlock_quarantined = Some(attempt);
                }
                self.refuse_verification(origin, AuthService::Password);
            }
        }
    }

    fn begin_fingerprint(&mut self, origin: Origin) {
        if crate::fprintd::Fprintd::disabled_by_env() {
            self.refuse_verification(origin, AuthService::Fingerprint);
            return;
        }
        if self.auth.fingerprint.is_some() {
            self.control.finish_unlock(origin, false);
            return;
        }
        let Some(user) = crate::unlock::session_user() else {
            self.refuse_verification(origin, AuthService::Fingerprint);
            return;
        };
        let epoch = self.unlock_generation.load(Ordering::Acquire);
        let Some(attempt) =
            self.auth
                .fingerprint_slot
                .begin_method(Some(origin), epoch, MethodKind::Fingerprint)
        else {
            self.control.finish_unlock(origin, false);
            return;
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let (send, steps) = mpsc::channel();
        let wake = self.wake.clone();
        let step: Arc<dyn Fn(FingerprintStep) + Send + Sync> = Arc::new(move |step| {
            if send.send(step).is_ok() {
                wake.signal();
            }
        });
        let worker_cancel = cancel.clone();
        let completion = self.wake.completion();
        let worker = std::thread::Builder::new()
            .name("tuna-fingerprint".into())
            .spawn(move || {
                let _completion = completion;
                match crate::fprintd::Fprintd::connect() {
                    Some(mut client) => {
                        crate::unlock::run_fingerprint(&mut client, &user, &worker_cancel, step)
                    }
                    None => FingerprintEnd::Unavailable,
                }
            });
        match worker {
            Ok(worker) => {
                self.auth.fingerprint = Some(FingerprintConversation {
                    attempt,
                    origin,
                    cancel,
                    steps,
                    worker: Some(worker),
                    canceled: false,
                });
            }
            Err(_) => {
                // Nothing ran: the attempt retires on the spot.
                self.auth
                    .fingerprint_slot
                    .retire_fingerprint(attempt, &FingerprintEnd::Unavailable);
                self.refuse_verification(origin, AuthService::Fingerprint);
            }
        }
    }

    /// The shell's answer to the outstanding question `sequence`. Any
    /// other answer is wiped and dropped.
    pub(super) fn answer_query(
        &mut self,
        connection: u64,
        service: AuthService,
        sequence: u32,
        answer: Secret,
    ) {
        let answer = AnswerText::new(answer.0);
        let Some(conversation) = self.auth.password.as_mut().filter(|conversation| {
            service == AuthService::Password
                && !conversation.canceled
                && conversation.origin.connection == connection
        }) else {
            return;
        };
        let Some(batch) = conversation
            .batch
            .as_mut()
            .filter(|batch| batch.asked == Some(sequence))
        else {
            return;
        };
        let index = batch.messages[batch.answers.len()].index;
        batch.answers.push(Answer {
            index,
            text: Some(answer),
        });
        batch.asked = None;
        if batch.answers.len() < batch.messages.len() {
            // An answered question inside a longer batch is user input: it
            // restarts the relay's inactivity watch for the next question.
            let (binding, sequence) = (batch.binding, batch.sequence);
            if let Some(response) = batch.response.as_mut() {
                let _ = response.note_user_input(binding, sequence);
            }
        }
        let origin = conversation.origin;
        self.trace_conversation(
            "answer",
            AuthService::Password,
            origin,
            serde_json::json!({ "sequence": sequence }),
        );
    }

    /// The shell's `CancelVerification`: stop every conversation it owns.
    /// Each result follows once that conversation has retired.
    pub(super) fn cancel_verification(&mut self, connection: u64) {
        if let Some(conversation) = self
            .auth
            .password
            .as_mut()
            .filter(|conversation| conversation.origin.connection == connection)
        {
            conversation.cancel();
        }
        if let Some(conversation) = self
            .auth
            .fingerprint
            .as_mut()
            .filter(|conversation| conversation.origin.connection == connection)
        {
            conversation.cancel();
        }
    }

    /// The lock ended without a verification (logind `Unlock`): every
    /// conversation is cancelled like the single-password attempt it
    /// strands; their helpers still retire before the Slot is free, and a
    /// pending winner of the ended lock fails the epoch check and grants
    /// nothing.
    #[cfg(feature = "drm")]
    pub(super) fn cancel_all_conversations(&mut self) {
        if let Some(conversation) = self.auth.password.as_mut() {
            conversation.cancel();
        }
        if let Some(conversation) = self.auth.fingerprint.as_mut() {
            conversation.cancel();
        }
    }

    /// Whether `attempt` may still show prompts and apply a result: the
    /// session is locked in the same epoch and its origin is live.
    fn conversation_live(&self, attempt: Attempt, origin: Origin) -> bool {
        self.is_locked()
            && self.unlock_generation.load(Ordering::Acquire) == attempt.epoch
            && self.control.origin_pending_live(origin)
    }

    /// Advance every conversation. Runs in the tick after control EOF,
    /// lock gates and shell identity are current, like the password
    /// worker's completion.
    pub(super) fn poll_conversations(&mut self) {
        self.poll_password();
        self.poll_fingerprint();
        self.grant_winner();
    }

    fn poll_password(&mut self) {
        let Some(conversation) = self.auth.password.as_ref() else {
            return;
        };
        let (attempt, origin) = (conversation.attempt, conversation.origin);
        if !conversation.canceled && !self.conversation_live(attempt, origin) {
            self.auth.password.as_mut().expect("checked").cancel();
        }
        // Questions and messages, one question at a time.
        let mut events = Vec::new();
        {
            let conversation = self.auth.password.as_mut().expect("checked");
            if !conversation.canceled {
                if conversation.batch.is_none() {
                    match conversation.ticket.try_challenge() {
                        Ok(challenge) if challenge.binding == conversation.binding => {
                            conversation.batch = Some(Batch {
                                binding: challenge.binding,
                                sequence: challenge.sequence,
                                messages: challenge.messages,
                                response: Some(challenge.response),
                                answers: Vec::new(),
                                asked: None,
                            });
                        }
                        // A challenge for another binding: never shown.
                        Ok(_) => conversation.cancel(),
                        Err(_) => {}
                    }
                }
                if let Some(batch) = conversation.batch.as_mut() {
                    while batch.asked.is_none() && batch.answers.len() < batch.messages.len() {
                        let message = &batch.messages[batch.answers.len()];
                        match message.style {
                            Style::Info | Style::Error => {
                                if !message.text.is_empty() {
                                    events.push(if message.style == Style::Info {
                                        AuthEvent::Info {
                                            text: message.text.clone(),
                                        }
                                    } else {
                                        AuthEvent::Problem {
                                            text: message.text.clone(),
                                        }
                                    });
                                }
                                batch.answers.push(Answer {
                                    index: message.index,
                                    text: None,
                                });
                            }
                            Style::Visible | Style::Secret => {
                                conversation.queries = conversation.queries.saturating_add(1);
                                batch.asked = Some(conversation.queries);
                                events.push(AuthEvent::Query {
                                    sequence: conversation.queries,
                                    question: message.text.clone(),
                                    secret: message.style == Style::Secret,
                                });
                            }
                        }
                    }
                    if batch.answers.len() == batch.messages.len() {
                        let batch = conversation.batch.take().expect("checked");
                        let submitted = batch.response.is_some_and(|response| {
                            response
                                .submit(batch.binding, batch.sequence, batch.answers)
                                .is_ok()
                        });
                        if !submitted {
                            conversation.cancel();
                        }
                    }
                }
            }
        }
        for event in events {
            // Recheck before every prompt: never show a stale question.
            if !self.conversation_live(attempt, origin)
                || !self.send_auth(origin, AuthService::Password, event)
            {
                if let Some(conversation) = self.auth.password.as_mut() {
                    conversation.cancel();
                }
                break;
            }
        }
        // The result: only an actual retirement frees the common Slot.
        let conversation = self.auth.password.as_mut().expect("checked");
        let Some(result) = conversation.ticket.try_outcome() else {
            return;
        };
        let conversation = self.auth.password.take().expect("checked");
        if let Some(source) = conversation.source {
            self.loop_handle.remove(source);
        }
        let live = self.control.origin_pending_live(origin);
        if !result.may_retire() {
            // A helper that did not retire keeps the Slot quarantined.
            self.unlock_quarantined = Some(attempt);
            self.trace_conversation(
                "retired",
                AuthService::Password,
                origin,
                serde_json::json!({ "retired": false }),
            );
            self.control.finish_unlock(origin, false);
            return;
        }
        let completion_live =
            self.unlock_slot
                .completion_live(attempt, &self.unlock_generation, live);
        let retired =
            self.unlock_slot
                .retire_pam_method(attempt, MethodKind::PamConversation, result);
        let authenticated = retired
            && result.authenticated()
            && completion_live
            && !conversation.canceled
            && self.auth.winner.is_none();
        self.trace_conversation(
            "retired",
            AuthService::Password,
            origin,
            serde_json::json!({
                "retired": retired,
                "authenticated": authenticated,
                "canceled": conversation.canceled,
            }),
        );
        if authenticated {
            self.auth.winner = Some(Winner {
                origin,
                service: AuthService::Password,
                epoch: attempt.epoch,
            });
            return;
        }
        // The same owned-retirement receipt the single-password worker
        // writes (`Runtime::finish_unlock`), so the VM lanes bind a
        // cancelled conversation helper exactly as they bind that worker.
        if std::env::var_os("TUNA_LOCK_TRACE").is_some() {
            eprintln!(
                "tuna-compositor: lock authentication owned retirement {}",
                serde_json::json!({
                    "pid": std::process::id(), "joined_ok": true,
                    "retired": matches!(result, Verification::Retired { .. }),
                    "authenticated": result.authenticated(), "denied": true,
                    "cancel_requested": conversation.canceled, "reported_ok": false,
                    "applied": false, "attempt_serial": attempt.serial,
                    "attempt_epoch": attempt.epoch,
                    "current_epoch": self.unlock_generation.load(Ordering::Acquire),
                    "origin": { "connection": origin.connection, "request": origin.request },
                    "locked_after": self.is_locked()
                })
            );
        }
        // GDM reports a failed (not cancelled) conversation as a problem.
        if !conversation.canceled && live && matches!(result, Verification::Retired { .. }) {
            self.send_auth(
                origin,
                AuthService::Password,
                AuthEvent::Problem {
                    text: PASSWORD_FAILED.to_owned(),
                },
            );
        }
        self.control.finish_unlock(origin, false);
    }

    fn poll_fingerprint(&mut self) {
        let Some((attempt, origin, canceled)) =
            self.auth.fingerprint.as_ref().map(|conversation| {
                (
                    conversation.attempt,
                    conversation.origin,
                    conversation.canceled,
                )
            })
        else {
            return;
        };
        if !canceled && !self.conversation_live(attempt, origin) {
            self.auth.fingerprint.as_mut().expect("checked").cancel();
        }
        let finished = self
            .auth
            .fingerprint
            .as_ref()
            .and_then(|conversation| conversation.worker.as_ref())
            .is_none_or(std::thread::JoinHandle::is_finished);
        let end = finished.then(|| {
            let worker = self
                .auth
                .fingerprint
                .as_mut()
                .and_then(|conversation| conversation.worker.take());
            // A panicked worker is joined too: nothing can report later.
            worker.map_or(FingerprintEnd::Unavailable, |worker| {
                worker.join().unwrap_or(FingerprintEnd::Unavailable)
            })
        });
        // Steps sent before the end are drained after the join as well.
        let conversation = self.auth.fingerprint.as_ref().expect("checked");
        let steps: Vec<_> = conversation.steps.try_iter().collect();
        let canceled = conversation.canceled;
        for step in steps {
            if canceled {
                break;
            }
            let event = match step {
                FingerprintStep::Scan { swipe } => AuthEvent::FingerprintScan { swipe },
                FingerprintStep::Info(text) => AuthEvent::Info {
                    text: text.to_owned(),
                },
                FingerprintStep::Problem(text) => AuthEvent::Problem {
                    text: text.to_owned(),
                },
            };
            if !self.conversation_live(attempt, origin)
                || !self.send_auth(origin, AuthService::Fingerprint, event)
            {
                self.auth.fingerprint.as_mut().expect("checked").cancel();
                break;
            }
        }
        let Some(end) = end else {
            return;
        };
        let conversation = self.auth.fingerprint.take().expect("checked");
        let live = self.control.origin_pending_live(origin);
        let completion_live =
            self.auth
                .fingerprint_slot
                .completion_live(attempt, &self.unlock_generation, live);
        let retired = self.auth.fingerprint_slot.retire_fingerprint(attempt, &end);
        let matched = retired
            && end == FingerprintEnd::Matched
            && completion_live
            && !conversation.canceled
            && self.auth.winner.is_none();
        self.trace_conversation(
            "retired",
            AuthService::Fingerprint,
            origin,
            serde_json::json!({
                "retired": retired,
                "authenticated": matched,
                "end": format!("{end:?}"),
                "canceled": conversation.canceled,
            }),
        );
        if matched {
            self.auth.winner = Some(Winner {
                origin,
                service: AuthService::Fingerprint,
                epoch: attempt.epoch,
            });
            return;
        }
        // GDM 51 reports PAM_MAXTRIES like an unavailable service, with
        // the max-tries message (gdm-session.c `report_and_stop_conversation`,
        // GDM_SESSION_WORKER_ERROR_TOO_MANY_RETRIES): GNOME keeps the
        // password prompt and leaves fingerprint off until the next reset.
        if matches!(end, FingerprintEnd::Unavailable | FingerprintEnd::Failed)
            && !conversation.canceled
            && live
        {
            self.send_auth(origin, AuthService::Fingerprint, AuthEvent::Unavailable);
        }
        self.control.finish_unlock(origin, false);
    }

    /// Apply a verified success: cancel the other conversation, then grant
    /// once the common Slot is free, through the same epoch CAS as every
    /// other unlock. A stale winner (epoch or origin changed) is denied.
    fn grant_winner(&mut self) {
        let Some(winner) = self.auth.winner else {
            return;
        };
        if winner.service != AuthService::Password {
            if let Some(conversation) = self.auth.password.as_mut() {
                conversation.cancel();
            }
        }
        if winner.service != AuthService::Fingerprint {
            if let Some(conversation) = self.auth.fingerprint.as_mut() {
                conversation.cancel();
            }
        }
        let live = self.control.origin_pending_live(winner.origin)
            && self.unlock_generation.load(Ordering::Acquire) == winner.epoch;
        if live && self.unlock_quarantined.is_none() && self.unlock_slot.active() {
            // The cancelled password helper (or a legacy worker) has not
            // retired yet: grant on a later tick.
            return;
        }
        self.auth.winner = None;
        let applied =
            live && self.unlock_quarantined.is_none() && self.apply_verified_unlock(winner.epoch);
        self.trace_conversation(
            "granted",
            winner.service,
            winner.origin,
            serde_json::json!({ "applied": applied }),
        );
        self.control.finish_unlock(winner.origin, applied);
    }
}
