//! Request/reply controller, independent of GTK. One outstanding request owns
//! the conversation; cancellation waits for its reply before sending Cancel.
use crate::{
    model::{GreeterModel, ModelEvent, Screen, SessionRef},
    session::SessionEntry,
};
use greetd_ipc::Response;

pub enum Command {
    Create(String),
    Answer(Option<String>),
    Start(Vec<String>, Vec<String>),
    Cancel,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Auth,
    Start,
    Cancel,
}
pub enum Effect {
    Render,
    Send(Command),
    Exit,
}
#[derive(Default)]
pub struct Controller {
    pub model: GreeterModel,
    phase: Option<Phase>,
    cancel_requested: bool,
    failure_cancel: bool,
    rounds: usize,
    environment: Vec<String>,
}
impl Controller {
    pub fn busy(&self) -> bool {
        self.phase.is_some()
    }
    pub fn cancelling(&self) -> bool {
        self.cancel_requested
    }
    pub fn submit(&mut self, user: &str, answer: String, session: Option<&SessionEntry>) -> Effect {
        if self.busy() {
            return Effect::Render;
        }
        if self.model.wants_fingerprint() {
            // No device verification is started by this typed submission path.
            self.model.notice = Some(crate::model::FINGERPRINT_TYPED_NOTICE.into());
            return Effect::Render;
        }
        let command = if self.model.current_prompt().is_some() {
            Command::Answer(Some(answer))
        } else {
            let Some(session) = session else {
                self.fail();
                return Effect::Render;
            };
            if user.trim().is_empty() {
                self.model.notice = Some("Enter your username.".into());
                return Effect::Render;
            }
            self.model.begin_user(user.trim());
            self.model.session = Some(SessionRef {
                name: session.name.clone(),
                command: session.command.clone(),
            });
            let x11 = session
                .source
                .parent()
                .is_some_and(|p| p.ends_with("xsessions"));
            self.environment = vec![format!(
                "XDG_SESSION_TYPE={}",
                if x11 { "x11" } else { "wayland" }
            )];
            if session.is_default {
                self.environment
                    .push("XDG_CURRENT_DESKTOP=Tuna:GNOME".into());
            }
            self.rounds = 0;
            Command::Create(user.trim().into())
        };
        self.phase = Some(Phase::Auth);
        Effect::Send(command)
    }
    pub fn cancel(&mut self) -> Effect {
        if self.model.screen == Screen::Launching {
            return Effect::Render;
        }
        if self.busy() {
            self.cancel_requested = true;
            return Effect::Render;
        }
        if self.model.user.is_none() {
            self.model.cancel();
            return Effect::Render;
        }
        self.cancel_requested = true;
        self.phase = Some(Phase::Cancel);
        Effect::Send(Command::Cancel)
    }
    fn fail(&mut self) {
        self.model.pending.clear();
        self.model.screen = Screen::Failed;
        self.model.notice = Some("Sign-in failed. Try again.".into());
    }
    pub fn receive(&mut self, response: Result<Response, ()>) -> Effect {
        let Some(phase) = self.phase.take() else {
            return Effect::Render;
        };
        let Ok(response) = response else {
            self.cancel_requested = false;
            self.failure_cancel = false;
            self.model.cancel();
            self.fail();
            return Effect::Render;
        };
        if phase == Phase::Cancel {
            let failed = self.failure_cancel || !matches!(response, Response::Success);
            self.cancel_requested = false;
            self.failure_cancel = false;
            self.model.cancel();
            if failed {
                self.fail();
            }
            return Effect::Render;
        }
        if phase == Phase::Start && matches!(response, Response::Success) {
            return Effect::Exit;
        }
        if self.cancel_requested
            || phase == Phase::Start
            || matches!(response, Response::Error { .. })
        {
            self.failure_cancel = !self.cancel_requested;
            self.phase = Some(Phase::Cancel);
            self.model.pending.clear();
            return Effect::Send(Command::Cancel);
        }
        self.rounds += 1;
        if self.rounds > 64 {
            self.failure_cancel = true;
            self.phase = Some(Phase::Cancel);
            return Effect::Send(Command::Cancel);
        }
        match self.model.apply_response(&response) {
            ModelEvent::NeedAnswer { .. } => Effect::Render,
            ModelEvent::AutoAck => {
                self.phase = Some(Phase::Auth);
                Effect::Send(Command::Answer(None))
            }
            ModelEvent::Authenticated => {
                let Some(session) = &self.model.session else {
                    self.fail();
                    return Effect::Render;
                };
                let cmd = session.command.clone();
                self.model.mark_session_starting();
                self.phase = Some(Phase::Start);
                Effect::Send(Command::Start(cmd, self.environment.clone()))
            }
            ModelEvent::Failed => {
                self.fail();
                Effect::Render
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use greetd_ipc::{AuthMessageType, ErrorType};
    fn session() -> SessionEntry {
        SessionEntry {
            name: "Tuna Desktop".into(),
            command: vec!["tuna-session".into()],
            source: Default::default(),
            is_default: true,
        }
    }
    fn prompt() -> Response {
        Response::AuthMessage {
            auth_message_type: AuthMessageType::Secret,
            auth_message: "Password".into(),
        }
    }
    fn denied() -> Response {
        Response::Error {
            error_type: ErrorType::AuthError,
            description: "private service detail".into(),
        }
    }
    #[test]
    fn fingerprint_typed_submission_is_refused_and_cancel_still_retires_conversation() {
        for style in [AuthMessageType::Secret, AuthMessageType::Visible] {
            let mut c = Controller::default();
            c.submit("ada", String::new(), Some(&session()));
            assert!(matches!(
                c.receive(Ok(Response::AuthMessage {
                    auth_message_type: style,
                    auth_message: "Place your finger on the fingerprint reader".into(),
                })),
                Effect::Render
            ));
            assert!(!c.busy());
            assert!(c.model.wants_fingerprint());
            assert!(matches!(
                c.submit("ada", "must-not-be-sent".into(), Some(&session())),
                Effect::Render
            ));
            assert!(!c.busy());
            assert_eq!(
                c.model.notice.as_deref(),
                Some(crate::model::FINGERPRINT_TYPED_NOTICE)
            );
            assert!(matches!(c.cancel(), Effect::Send(Command::Cancel)));
            assert!(matches!(c.receive(Ok(Response::Success)), Effect::Render));
            assert!(c.model.pending.is_empty());
            assert!(!c.busy());
            // A fresh normal conversation can still accept a fresh typed value.
            assert!(matches!(
                c.submit("ada", String::new(), Some(&session())),
                Effect::Send(Command::Create(_))
            ));
            c.receive(Ok(prompt()));
            assert!(
                matches!(c.submit("ada", "fresh-password".into(), Some(&session())),
                Effect::Send(Command::Answer(Some(value))) if value == "fresh-password")
            );
        }
    }

    #[test]
    fn launch_requires_success_reply_and_rejected_launch_resets_before_retry() {
        let mut c = Controller::default();
        assert!(matches!(
            c.submit("ada", String::new(), Some(&session())),
            Effect::Send(Command::Create(_))
        ));
        assert!(matches!(
            c.receive(Ok(Response::Success)),
            Effect::Send(Command::Start(..))
        ));
        assert_eq!(c.model.screen, Screen::Launching);
        assert!(matches!(
            c.receive(Ok(denied())),
            Effect::Send(Command::Cancel)
        ));
        assert!(c.busy());
        assert!(matches!(c.receive(Ok(Response::Success)), Effect::Render));
        assert_eq!(c.model.screen, Screen::Failed);
        assert!(!c.model.notice.as_ref().unwrap().contains("private"));
        assert!(matches!(
            c.submit("ada", String::new(), Some(&session())),
            Effect::Send(Command::Create(_))
        ));
        assert!(matches!(
            c.receive(Ok(Response::Success)),
            Effect::Send(Command::Start(..))
        ));
        assert!(matches!(c.receive(Ok(Response::Success)), Effect::Exit));
    }
    #[test]
    fn cancel_waits_for_pending_reply_then_consumes_cancel_reply() {
        let mut c = Controller::default();
        c.submit("ada", String::new(), Some(&session()));
        assert!(matches!(c.cancel(), Effect::Render));
        assert!(matches!(
            c.receive(Ok(prompt())),
            Effect::Send(Command::Cancel)
        ));
        assert!(matches!(
            c.submit("wrong", "secret".into(), Some(&session())),
            Effect::Render
        ));
        c.receive(Ok(Response::Success));
        assert_eq!(c.model.screen, Screen::UserPick);
        assert!(!c.busy());
        assert!(c.model.pending.is_empty());
    }
    #[test]
    fn transport_failure_and_rejected_cancellation_fail_closed() {
        let mut c = Controller::default();
        c.submit("ada", String::new(), Some(&session()));
        c.receive(Err(()));
        assert_eq!(c.model.screen, Screen::Failed);
        c.submit("ada", String::new(), Some(&session()));
        c.receive(Ok(prompt()));
        c.cancel();
        c.receive(Ok(denied()));
        assert_eq!(c.model.screen, Screen::Failed);
        assert!(c.model.user.is_none());
    }
    #[test]
    fn informational_round_limit_cancels_instead_of_unbounded_autoack() {
        let mut c = Controller::default();
        c.submit("ada", String::new(), Some(&session()));
        for _ in 0..64 {
            assert!(matches!(
                c.receive(Ok(Response::AuthMessage {
                    auth_message_type: AuthMessageType::Info,
                    auth_message: "Notice".into()
                })),
                Effect::Send(Command::Answer(None))
            ));
        }
        assert!(matches!(
            c.receive(Ok(prompt())),
            Effect::Send(Command::Cancel)
        ));
    }
}

#[cfg(test)]
mod selection_tests {
    use super::*;
    #[test]
    fn empty_session_or_username_cannot_create_authentication() {
        let mut c = Controller::default();
        assert!(matches!(
            c.submit("ada", String::new(), None),
            Effect::Render
        ));
        assert!(!c.busy());
        let session = SessionEntry {
            name: "X11".into(),
            command: vec!["x-session".into()],
            source: "/usr/share/xsessions/test.desktop".into(),
            is_default: false,
        };
        assert!(matches!(
            c.submit(" ", String::new(), Some(&session)),
            Effect::Render
        ));
        assert!(!c.busy());
        c.submit("ada", String::new(), Some(&session));
        match c.receive(Ok(Response::Success)) {
            Effect::Send(Command::Start(cmd, env)) => {
                assert_eq!(cmd, vec!["x-session"]);
                assert_eq!(env, vec!["XDG_SESSION_TYPE=x11"]);
            }
            _ => panic!("expected selected session"),
        }
    }
}
