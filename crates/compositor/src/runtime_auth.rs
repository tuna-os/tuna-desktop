//! Interactive PAM on the actual Runtime/common Slot. No production policy is
//! admitted by default; only the trusted Rust probe can install a controlled one.
use super::Runtime;
use crate::auth_owner::{Attempt, MethodKind, Origin};
use crate::control::{AuthLease, AuthRequest};
use crate::pam_backend::{Backend, ConversationTicket, Verification};
use crate::pam_conversation::{Binding, Challenge, Policy};
use crate::pam_helper::{Answer, AnswerText, Style};
use crate::windows::ManagerInput;
use smithay::reexports::wayland_server::Resource;
use tuna_shell_control::{
    auth::{Key, Phase, Prompt, Style as WireStyle},
    Message,
};

pub(super) struct Admission {
    pub backend: Backend,
    pub policy: Policy,
}
pub(super) struct Pending {
    challenge: Challenge,
    styles: Vec<Style>,
    input_seen: bool,
}
pub(super) struct Active {
    attempt: Attempt,
    lease: AuthLease,
    ticket: ConversationTicket,
    source: Option<calloop::RegistrationToken>,
    pending: Option<Pending>,
    denied: bool,
    quarantined: bool,
}
impl Active {
    fn key(&self) -> Key {
        Key {
            start_request: self.attempt.origin.expect("admitted origin").request,
            attempt_serial: self.attempt.serial,
            epoch: self.attempt.epoch,
        }
    }
    fn binding(&self) -> Binding {
        Binding {
            origin: self.attempt.origin.expect("admitted origin"),
            attempt: self.attempt.serial,
            epoch: self.attempt.epoch,
        }
    }
}
impl Runtime {
    fn auth_live(&self, auth: &Active) -> bool {
        self.is_locked()
            && self.unlock_slot.matches(auth.attempt)
            && self
                .unlock_generation
                .load(std::sync::atomic::Ordering::Acquire)
                == auth.attempt.epoch
            && self.control.auth_lease_live(auth.lease)
    }
    fn auth_state(&mut self, auth: &Active, phase: Phase) {
        let _ = self.control.send_auth(
            auth.lease,
            Message::AuthState {
                start_request: auth.key().start_request,
                key: Some(auth.key()),
                phase,
            },
        );
    }
    fn deny_auth(&mut self, auth: &mut Active) {
        auth.ticket.cancel();
        auth.pending.take(); // Dropping the exact response capability cancels/wipes.
        if !auth.denied {
            self.auth_state(auth, Phase::Cancelling);
            self.control
                .finish_auth(auth.attempt.origin.expect("admitted origin"), false);
            auth.denied = true;
        }
    }
    pub(super) fn handle_auth_requests(&mut self, requests: Vec<AuthRequest>) {
        for request in requests {
            match request {
                AuthRequest::Start { origin } => self.start_auth(origin),
                AuthRequest::Answer {
                    origin,
                    key,
                    sequence,
                    answers,
                } => {
                    let Some(mut auth) = self.interactive_auth.take() else {
                        self.control.auth_reply(origin, false);
                        continue;
                    };
                    let eligible = self.auth_live(&auth)
                        && !auth.denied
                        && !auth.quarantined
                        && auth
                            .ticket
                            .next_deadline()
                            .is_some_and(|deadline| std::time::Instant::now() < deadline)
                        && origin.connection
                            == auth.attempt.origin.expect("admitted origin").connection
                        && key == auth.key()
                        && auth.pending.as_ref().is_some_and(|pending| {
                            pending.challenge.sequence == sequence
                                && answers.len() == pending.styles.len()
                                && pending.styles.iter().enumerate().all(|(index, style)| {
                                    answers[index].index as usize == index
                                        && match style {
                                            Style::Visible | Style::Secret => {
                                                answers[index].text.is_some() && pending.input_seen
                                            }
                                            Style::Info | Style::Error => {
                                                answers[index].text.is_none()
                                            }
                                        }
                                })
                        });
                    let accepted = if eligible {
                        let pending = auth.pending.take().expect("validated challenge");
                        let binding = auth.binding();
                        let values = answers
                            .into_iter()
                            .map(|answer| {
                                Some(Answer {
                                    index: u16::try_from(answer.index).ok()?,
                                    text: answer.text.map(|text| {
                                        let mut secret = text.into_secret();
                                        AnswerText::new(std::mem::take(&mut secret.0))
                                    }),
                                })
                            })
                            .collect::<Option<Vec<_>>>();
                        values.is_some_and(|values| {
                            pending
                                .challenge
                                .response
                                .submit(binding, sequence, values)
                                .is_ok()
                        })
                    } else {
                        false
                    };
                    if accepted {
                        self.auth_state(&auth, Phase::Verifying);
                    } else if eligible {
                        self.deny_auth(&mut auth);
                    }
                    self.control.auth_reply(origin, accepted);
                    self.interactive_auth = Some(auth);
                }
                AuthRequest::Cancel {
                    origin,
                    start_request,
                    key,
                } => {
                    let Some(mut auth) = self.interactive_auth.take() else {
                        self.control.auth_reply(origin, false);
                        continue;
                    };
                    let accepted = origin.connection
                        == auth.attempt.origin.expect("admitted origin").connection
                        && start_request == auth.key().start_request
                        && key.is_none_or(|key| key == auth.key())
                        && self.control.auth_lease_live(auth.lease);
                    if accepted {
                        self.deny_auth(&mut auth);
                    }
                    self.control.auth_reply(origin, accepted);
                    self.interactive_auth = Some(auth);
                }
            }
        }
    }
    fn start_auth(&mut self, origin: Origin) {
        // Existing greetd authority is never silently replaced with local PAM.
        let admitted = self.is_locked()
            && self.control.origin_pending_live(origin)
            && self.interactive_admission.is_some()
            && crate::unlock::greetd_socket_path().is_none();
        if !admitted {
            self.control.deny_auth_start(origin, Phase::Unavailable);
            return;
        }
        let Some(user) = crate::unlock::session_user() else {
            self.control.deny_auth_start(origin, Phase::Unavailable);
            return;
        };
        let epoch = self
            .unlock_generation
            .load(std::sync::atomic::Ordering::Acquire);
        let Some(attempt) =
            self.unlock_slot
                .begin_method(Some(origin), epoch, MethodKind::PamConversation)
        else {
            self.control.deny_auth_start(origin, Phase::Blocked);
            return;
        };
        let Some(lease) = self.control.bind_auth_lease(
            origin,
            attempt.serial,
            epoch,
            MethodKind::PamConversation,
        ) else {
            self.unlock_slot.retire_pam_method(
                attempt,
                MethodKind::PamConversation,
                Verification::RejectedBeforeSpawn,
            );
            self.control.deny_auth_start(origin, Phase::Denied);
            return;
        };
        let binding = Binding {
            origin,
            attempt: attempt.serial,
            epoch,
        };
        let admission = self
            .interactive_admission
            .as_ref()
            .expect("admitted profile");
        // No wire user/service/policy. The installed lock service is fixed.
        let result = admission.backend.begin_conversation(
            crate::pam::DEFAULT_SERVICE.into(),
            user,
            binding,
            admission.policy,
            crate::pam_cancel::Cancellation::default(),
        );
        match result {
            Err(result) => {
                if result.may_retire() {
                    self.control.retire_pam_auth_lease(lease, result);
                    self.unlock_slot.retire_pam_method(
                        attempt,
                        MethodKind::PamConversation,
                        result,
                    );
                } else {
                    self.unlock_quarantined = Some(attempt);
                }
                self.control.deny_auth_start(origin, Phase::Blocked);
            }
            Ok(mut ticket) => {
                let source = ticket
                    .take_notice_source()
                    .expect("fresh owned conversation source");
                let token = self
                    .loop_handle
                    .insert_source(source, |_, _, rt: &mut Runtime| {
                        // No grants here: tick must first observe control EOF/generation/epoch.
                        rt.auth_notice = true;
                    });
                let mut auth = Active {
                    attempt,
                    lease,
                    ticket,
                    source: token.ok(),
                    pending: None,
                    denied: false,
                    quarantined: false,
                };
                if auth.source.is_none() {
                    self.deny_auth(&mut auth);
                    auth.quarantined = true;
                    self.unlock_quarantined = Some(attempt);
                    self.auth_state(&auth, Phase::Blocked);
                } else {
                    self.auth_state(&auth, Phase::Starting);
                }
                self.interactive_auth = Some(auth);
            }
        }
    }
    /// Called only after the actual control/supervision/lock gates are current.
    pub(super) fn poll_auth(&mut self) {
        self.auth_notice = false;
        let Some(mut auth) = self.interactive_auth.take() else {
            return;
        };
        if auth.quarantined {
            self.interactive_auth = Some(auth);
            return;
        }
        if !self.auth_live(&auth) {
            self.deny_auth(&mut auth);
        }
        if let Some(result) = auth.ticket.try_outcome() {
            let live = self.auth_live(&auth);
            let grant = result.authenticated() && live && !auth.denied && auth.pending.is_none();
            auth.pending.take();
            if let Some(token) = auth.source.take() {
                self.loop_handle.remove(token);
            }
            if !result.may_retire() {
                self.deny_auth(&mut auth);
                auth.quarantined = true;
                self.unlock_quarantined = Some(auth.attempt);
                self.auth_state(&auth, Phase::Blocked);
                self.interactive_auth = Some(auth);
                return;
            }
            if !grant {
                self.auth_state(&auth, Phase::Denied);
            }
            self.control.retire_pam_auth_lease(auth.lease, result);
            let released = self.unlock_slot.retire_pam_method(
                auth.attempt,
                MethodKind::PamConversation,
                result,
            );
            let applied = released && grant && self.apply_verified_unlock(auth.attempt.epoch);
            if !auth.denied {
                self.control
                    .finish_auth(auth.attempt.origin.expect("admitted origin"), applied);
            }
            return;
        }
        if !auth.denied && auth.pending.is_none() {
            match auth.ticket.try_challenge() {
                Ok(mut challenge) => {
                    if challenge.binding != auth.binding()
                        || !self.auth_live(&auth)
                        || !auth
                            .ticket
                            .next_deadline()
                            .is_some_and(|deadline| std::time::Instant::now() < deadline)
                        || challenge.messages.iter().any(|message| {
                            matches!(message.style, Style::Visible | Style::Secret)
                                && tuna_greeter::model::is_fingerprint_prompt(&message.text)
                        })
                    {
                        drop(challenge);
                        self.deny_auth(&mut auth);
                    } else {
                        let styles = challenge
                            .messages
                            .iter()
                            .map(|message| message.style)
                            .collect();
                        let messages = std::mem::take(&mut challenge.messages)
                            .into_iter()
                            .map(|message| Prompt {
                                index: u32::from(message.index),
                                style: match message.style {
                                    Style::Visible => WireStyle::Visible,
                                    Style::Secret => WireStyle::Secret,
                                    Style::Info => WireStyle::Info,
                                    Style::Error => WireStyle::Error,
                                },
                                text: message.text,
                            })
                            .collect();
                        let message = Message::AuthChallenge {
                            key: auth.key(),
                            sequence: challenge.sequence,
                            messages,
                        };
                        if self.control.send_auth(auth.lease, message) {
                            auth.pending = Some(Pending {
                                challenge,
                                styles,
                                input_seen: false,
                            });
                            self.auth_state(&auth, Phase::Prompt);
                        } else {
                            drop(challenge);
                            self.deny_auth(&mut auth);
                        }
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.deny_auth(&mut auth),
            }
        }
        self.interactive_auth = Some(auth);
    }
    pub(super) fn auth_deadline(&self) -> Option<std::time::Instant> {
        self.interactive_auth
            .as_ref()
            .filter(|auth| !auth.quarantined)
            .and_then(|auth| auth.ticket.next_deadline())
    }
    /// Actual backend entry only: remote requests, aid timer effects and
    /// replayed/synthetic delivery never pass this provenance boundary.
    pub(super) fn record_auth_native_input(&mut self, input: &ManagerInput) {
        if !self.is_locked() {
            return;
        }
        let native_activity = !matches!(input, ManagerInput::CornerPressure { .. });
        let owned_surface = self
            .state
            .seat
            .get_keyboard()
            .and_then(|keyboard| keyboard.current_focus())
            .filter(|surface| self.state.lock_surfaces().contains(surface))
            .and_then(|surface| surface.client())
            .and_then(|client| client.get_credentials(&self.display.handle()).ok())
            .is_some_and(|credentials| {
                Some(credentials.pid as u32) == self.shell.child_pid()
                    && credentials.uid == crate::control::our_uid()
            });
        self.record_auth_input(native_activity && owned_surface);
    }
    /// Called by real seat input only (or separately admitted Rust CI probe).
    pub(super) fn record_auth_input(&mut self, owned_lock_input: bool) -> bool {
        let Some(mut auth) = self.interactive_auth.take() else {
            return false;
        };
        let mut accepted = false;
        if owned_lock_input && self.auth_live(&auth) && !auth.denied && !auth.quarantined {
            let binding = auth.binding();
            if let Some(pending) = &mut auth.pending {
                accepted = pending
                    .challenge
                    .response
                    .note_user_input(binding, pending.challenge.sequence)
                    .is_ok();
                pending.input_seen |= accepted;
            }
        }
        self.interactive_auth = Some(auth);
        accepted
    }
}
