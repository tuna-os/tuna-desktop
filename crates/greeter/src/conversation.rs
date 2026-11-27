//! GDM's UserVerifier as the [`Host`] of the shared AuthPrompt machine.
//!
//! The lock screen's machine (`tuna-auth-prompt`) speaks in requests,
//! sequenced queries and one result per request; GDM speaks in service
//! names. This adapter is the translation gnome-shell 51's
//! `authServices.js` does between `authPrompt.js` and libgdm's
//! `GdmUserVerifier`:
//!
//! - a begin is `BeginVerificationForUser(gdm-password, user)`; its result
//!   is the first of `VerificationComplete` (applied), `VerificationFailed`
//!   or `ConversationStopped` (denied) for that service;
//! - `SecretInfoQuery`/`InfoQuery` become sequenced hidden/visible queries
//!   and only the outstanding one may be answered (`AnswerQuery`);
//! - `Info` and `Problem` become messages; `ServiceUnavailable` reports its
//!   message as a problem, as `_onServiceUnavailable` queues it as an error;
//! - a cancel is `Cancel()`, after which GDM resets and stops every
//!   conversation: a cancelled begin's result is its `ConversationStopped`;
//! - `_canRetry`: the login screen retries `allowed-failures` times
//!   (org.gnome.login-screen, default 3), then gives up.
//!
//! Pure: GDM calls are queued as [`Call`]s for the D-Bus layer, and GDM
//! signals come back as machine [`Input`]s.

use std::cell::RefCell;

use tuna_auth_prompt::Host;
use tuna_shell_control::{AuthEvent, AuthService};

use crate::gdm::{Call, Signal};

/// GDM's password service (gnome-shell `authServicesLegacy.js`).
pub const PASSWORD_SERVICE: &str = "gdm-password";

/// What the machine must hear for a GDM signal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Input {
    Event {
        request: u64,
        service: AuthService,
        event: AuthEvent,
    },
    Result {
        request: u64,
        applied: bool,
    },
}

#[derive(Debug)]
struct Begun {
    request: u64,
    service: &'static str,
    /// The unanswered query's sequence.
    outstanding: Option<u32>,
}

#[derive(Default)]
struct State {
    user: Option<String>,
    next_request: u64,
    next_sequence: u32,
    current: Option<Begun>,
    /// Cancelled begins whose `ConversationStopped` is still owed.
    retiring: Vec<Begun>,
    calls: Vec<Call>,
    failures: u32,
}

/// The GDM side of one login prompt.
pub struct Conversations {
    state: RefCell<State>,
    allowed_failures: u32,
}

impl Conversations {
    pub fn new(allowed_failures: u32) -> Self {
        Self {
            state: RefCell::default(),
            allowed_failures,
        }
    }

    /// The user the next begins verify (the selected list item, or what
    /// was typed after "Not listed?"); a new user starts a new fail count,
    /// as a new `AuthPrompt.begin` does.
    pub fn set_user(&self, user: Option<String>) {
        let mut state = self.state.borrow_mut();
        if state.user != user {
            state.failures = 0;
        }
        state.user = user;
    }

    /// The begin owed a result, if any (the one the last
    /// `BeginVerificationForUser` call started).
    pub fn current_request(&self) -> Option<u64> {
        self.state.borrow().current.as_ref().map(|b| b.request)
    }

    /// GDM calls to make, oldest first.
    pub fn take_calls(&self) -> Vec<Call> {
        std::mem::take(&mut self.state.borrow_mut().calls)
    }

    /// The begin for `request` failed on the wire (GDM refused the service,
    /// or the connection is gone): userVerifier.js `_reportInitError`,
    /// counted as a failure.
    pub fn begin_failed(&self, request: u64) -> Vec<Input> {
        let mut state = self.state.borrow_mut();
        if state.current.as_ref().map(|b| b.request) != Some(request) {
            return Vec::new();
        }
        state.current = None;
        state.failures += 1;
        vec![
            Input::Event {
                request,
                service: AuthService::Password,
                event: AuthEvent::Problem {
                    text: tuna_auth_prompt::INIT_ERROR.to_owned(),
                },
            },
            Input::Result {
                request,
                applied: false,
            },
        ]
    }

    /// The channel to GDM closed: every begin owed a result is denied.
    pub fn disconnected(&self) -> Vec<Input> {
        let mut state = self.state.borrow_mut();
        let current = state.current.take();
        let retiring = std::mem::take(&mut state.retiring);
        current
            .into_iter()
            .chain(retiring)
            .map(|begun| Input::Result {
                request: begun.request,
                applied: false,
            })
            .collect()
    }

    /// Translate one GDM signal.
    pub fn on_signal(&self, signal: &Signal) -> Vec<Input> {
        let mut state = self.state.borrow_mut();
        match signal {
            Signal::ConversationStopped { service } => {
                if let Some(index) = state.retiring.iter().position(|b| b.service == service) {
                    let begun = state.retiring.remove(index);
                    return vec![Input::Result {
                        request: begun.request,
                        applied: false,
                    }];
                }
                match state.current.take_if(|b| b.service == service) {
                    Some(begun) => vec![Input::Result {
                        request: begun.request,
                        applied: false,
                    }],
                    None => Vec::new(),
                }
            }
            Signal::VerificationComplete { service } => {
                match state.current.take_if(|b| b.service == service) {
                    Some(begun) => vec![Input::Result {
                        request: begun.request,
                        applied: true,
                    }],
                    None => Vec::new(),
                }
            }
            Signal::VerificationFailed { service } => {
                match state.current.take_if(|b| b.service == service) {
                    Some(begun) => {
                        state.failures += 1;
                        vec![Input::Result {
                            request: begun.request,
                            applied: false,
                        }]
                    }
                    None => Vec::new(),
                }
            }
            Signal::Info { service, text } => {
                Self::event(&state, service, AuthEvent::Info { text: text.clone() })
            }
            Signal::Problem { service, text } => {
                Self::event(&state, service, AuthEvent::Problem { text: text.clone() })
            }
            Signal::ServiceUnavailable { service, message } if !message.is_empty() => Self::event(
                &state,
                service,
                AuthEvent::Problem {
                    text: message.clone(),
                },
            ),
            Signal::InfoQuery { service, query } | Signal::SecretInfoQuery { service, query } => {
                let secret = matches!(signal, Signal::SecretInfoQuery { .. });
                let sequence = state.next_sequence + 1;
                let Some(begun) = state.current.as_mut().filter(|b| b.service == service) else {
                    return Vec::new();
                };
                begun.outstanding = Some(sequence);
                let request = begun.request;
                state.next_sequence = sequence;
                vec![Input::Event {
                    request,
                    service: AuthService::Password,
                    event: AuthEvent::Query {
                        sequence,
                        question: query.clone(),
                        secret,
                    },
                }]
            }
            _ => Vec::new(),
        }
    }

    fn event(state: &State, service: &str, event: AuthEvent) -> Vec<Input> {
        match state.current.as_ref().filter(|b| b.service == service) {
            Some(begun) => vec![Input::Event {
                request: begun.request,
                service: AuthService::Password,
                event,
            }],
            None => Vec::new(),
        }
    }
}

impl Host for Conversations {
    fn begin(&self, service: AuthService) -> Option<u64> {
        // GDM 51 logs in with the password service only: fingerprint works
        // for re-authentication alone (GDM 51.0 NEWS).
        if service != AuthService::Password {
            return None;
        }
        let mut state = self.state.borrow_mut();
        let user = state.user.clone()?;
        if let Some(previous) = state.current.take() {
            state.retiring.push(previous);
        }
        state.next_request += 1;
        let request = state.next_request;
        state.current = Some(Begun {
            request,
            service: PASSWORD_SERVICE,
            outstanding: None,
        });
        state.calls.push(Call::BeginVerificationForUser {
            service: PASSWORD_SERVICE.to_owned(),
            user,
        });
        Some(request)
    }

    fn answer(&self, service: AuthService, sequence: u32, answer: String) -> Option<u64> {
        let mut state = self.state.borrow_mut();
        let begun = state
            .current
            .as_mut()
            .filter(|b| service == AuthService::Password && b.outstanding == Some(sequence))?;
        begun.outstanding = None;
        let call = Call::AnswerQuery {
            service: begun.service.to_owned(),
            answer,
        };
        let request = begun.request;
        state.calls.push(call);
        Some(request)
    }

    fn cancel(&self) -> Option<u64> {
        let mut state = self.state.borrow_mut();
        let begun = state.current.take()?;
        let request = begun.request;
        state.retiring.push(begun);
        state.calls.push(Call::Cancel);
        Some(request)
    }

    /// The single-password `Unlock` is the lock screen's own fallback;
    /// GDM has none.
    fn unlock(&self, _password: String) -> Option<u64> {
        None
    }

    fn fingerprint_enabled(&self) -> bool {
        false
    }

    /// The greeter draws no wiggle yet.
    fn wiggle_ms(&self) -> u64 {
        0
    }

    fn can_retry(&self) -> bool {
        self.state.borrow().failures < self.allowed_failures
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const USER: &str = "alice";

    fn s(service: &str) -> String {
        service.to_owned()
    }

    fn begun(c: &Conversations) -> u64 {
        c.set_user(Some(USER.into()));
        let request = c.begin(AuthService::Password).unwrap();
        assert_eq!(
            c.take_calls(),
            vec![Call::BeginVerificationForUser {
                service: s(PASSWORD_SERVICE),
                user: s(USER)
            }]
        );
        request
    }

    #[test]
    fn begin_needs_a_user_and_logs_in_with_the_password_service_only() {
        let c = Conversations::new(3);
        assert_eq!(c.begin(AuthService::Password), None);
        c.set_user(Some(USER.into()));
        assert_eq!(c.begin(AuthService::Fingerprint), None);
        assert!(!c.fingerprint_enabled());
        assert_eq!(c.unlock("x".into()), None);
        assert!(c.begin(AuthService::Password).is_some());
    }

    #[test]
    fn secret_query_is_answered_once_with_its_sequence() {
        let c = Conversations::new(3);
        let request = begun(&c);
        let inputs = c.on_signal(&Signal::SecretInfoQuery {
            service: s(PASSWORD_SERVICE),
            query: s("Password:"),
        });
        let Input::Event {
            event: AuthEvent::Query {
                sequence, secret, ..
            },
            ..
        } = inputs[0].clone()
        else {
            panic!("{inputs:?}");
        };
        assert!(secret);
        // A stale sequence is not sent.
        assert_eq!(
            c.answer(AuthService::Password, sequence + 1, "x".into()),
            None
        );
        assert_eq!(
            c.answer(AuthService::Password, sequence, "pw".into()),
            Some(request)
        );
        assert_eq!(
            c.take_calls(),
            vec![Call::AnswerQuery {
                service: s(PASSWORD_SERVICE),
                answer: s("pw")
            }]
        );
        // The same query cannot be answered twice.
        assert_eq!(c.answer(AuthService::Password, sequence, "pw".into()), None);
        assert!(c.take_calls().is_empty());
    }

    #[test]
    fn visible_query_messages_and_unavailable_reach_the_machine() {
        let c = Conversations::new(3);
        let request = begun(&c);
        let ev = |signal| c.on_signal(&signal);
        assert!(matches!(
            ev(Signal::InfoQuery {
                service: s(PASSWORD_SERVICE),
                query: s("Code:")
            })[0],
            Input::Event {
                event: AuthEvent::Query { secret: false, .. },
                ..
            }
        ));
        assert_eq!(
            ev(Signal::Info {
                service: s(PASSWORD_SERVICE),
                text: s("Sent")
            }),
            vec![Input::Event {
                request,
                service: AuthService::Password,
                event: AuthEvent::Info { text: s("Sent") }
            }]
        );
        assert_eq!(
            ev(Signal::ServiceUnavailable {
                service: s(PASSWORD_SERVICE),
                message: s("Gone")
            }),
            vec![Input::Event {
                request,
                service: AuthService::Password,
                event: AuthEvent::Problem { text: s("Gone") }
            }]
        );
        // Other services' steps are not this prompt's.
        assert!(ev(Signal::Problem {
            service: s("gdm-smartcard"),
            text: s("x")
        })
        .is_empty());
        assert!(ev(Signal::SecretInfoQuery {
            service: s("gdm-fingerprint"),
            query: s("x")
        })
        .is_empty());
    }

    #[test]
    fn failure_then_stop_is_one_denied_result_and_counts() {
        let c = Conversations::new(2);
        let request = begun(&c);
        assert_eq!(
            c.on_signal(&Signal::Problem {
                service: s(PASSWORD_SERVICE),
                text: s("Sorry")
            }),
            vec![Input::Event {
                request,
                service: AuthService::Password,
                event: AuthEvent::Problem { text: s("Sorry") }
            }]
        );
        assert_eq!(
            c.on_signal(&Signal::VerificationFailed {
                service: s(PASSWORD_SERVICE)
            }),
            vec![Input::Result {
                request,
                applied: false
            }]
        );
        // GDM's report_and_stop_conversation stops it too: no second result.
        assert!(c
            .on_signal(&Signal::ConversationStopped {
                service: s(PASSWORD_SERVICE)
            })
            .is_empty());
        assert!(c.can_retry());
        let second = c.begin(AuthService::Password).unwrap();
        assert_ne!(second, request);
        c.take_calls();
        c.on_signal(&Signal::VerificationFailed {
            service: s(PASSWORD_SERVICE),
        });
        assert!(!c.can_retry(), "allowed-failures reached");
        // Another user starts a new count.
        c.set_user(Some("bob".into()));
        assert!(c.can_retry());
    }

    #[test]
    fn completion_is_an_applied_result() {
        let c = Conversations::new(3);
        let request = begun(&c);
        assert_eq!(
            c.on_signal(&Signal::VerificationComplete {
                service: s(PASSWORD_SERVICE)
            }),
            vec![Input::Result {
                request,
                applied: true
            }]
        );
        assert!(c
            .on_signal(&Signal::VerificationComplete {
                service: s(PASSWORD_SERVICE)
            })
            .is_empty());
    }

    #[test]
    fn a_cancelled_begin_retires_on_conversation_stopped() {
        let c = Conversations::new(3);
        let first = begun(&c);
        assert_eq!(c.cancel(), Some(first));
        assert_eq!(c.take_calls(), vec![Call::Cancel]);
        // Nothing current: nothing to cancel twice.
        assert_eq!(c.cancel(), None);
        // Steps of the cancelled conversation are dropped.
        assert!(c
            .on_signal(&Signal::SecretInfoQuery {
                service: s(PASSWORD_SERVICE),
                query: s("P")
            })
            .is_empty());
        let second = c.begin(AuthService::Password).unwrap();
        c.take_calls();
        // The first ConversationStopped retires the cancelled begin, not
        // the new one.
        assert_eq!(
            c.on_signal(&Signal::ConversationStopped {
                service: s(PASSWORD_SERVICE)
            }),
            vec![Input::Result {
                request: first,
                applied: false
            }]
        );
        assert_eq!(
            c.on_signal(&Signal::VerificationComplete {
                service: s(PASSWORD_SERVICE)
            }),
            vec![Input::Result {
                request: second,
                applied: true
            }]
        );
    }

    #[test]
    fn wire_failures_and_disconnects_deny_what_is_owed() {
        let c = Conversations::new(3);
        let request = begun(&c);
        let inputs = c.begin_failed(request);
        assert_eq!(inputs.len(), 2);
        assert_eq!(
            inputs[1],
            Input::Result {
                request,
                applied: false
            }
        );
        assert!(c.begin_failed(request).is_empty());
        let a = c.begin(AuthService::Password).unwrap();
        c.cancel();
        let b = c.begin(AuthService::Password).unwrap();
        let mut owed: Vec<u64> = c
            .disconnected()
            .into_iter()
            .map(|i| match i {
                Input::Result {
                    request,
                    applied: false,
                } => request,
                other => panic!("{other:?}"),
            })
            .collect();
        owed.sort_unstable();
        assert_eq!(owed, vec![a, b]);
        assert!(c.disconnected().is_empty());
    }
}
