//! Dedicated interactive transport. No credentials/control authority are replayed.
use super::{ControlClient, ControlError, Handled, InputMethodAdmission};
use std::io::Write;
use tuna_shell_control::{
    auth::{Answer, Availability, Key, Method, Phase, Prompt, Style},
    CommandKind, CommandStatus, Message,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Context {
    pub generation: u64,
    pub start_request: u64,
    pub key: Option<Key>,
    pub sequence: u32,
    pub epoch: u64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Availability(Availability),
    State {
        context: Context,
        phase: Phase,
    },
    Challenge {
        context: Context,
        messages: Vec<Prompt>,
    },
    AnswerResult {
        context: Context,
        applied: bool,
    },
    RootResult {
        context: Context,
        applied: bool,
    },
    CancelResult {
        context: Context,
        applied: bool,
    },
    Ignored,
}
pub(super) struct Active {
    context: Context,
    phase: Phase,
    styles: Vec<Style>,
    answer: Option<u64>,
    cancel: Option<u64>,
    // Intent survives transport acknowledgements until physical denial/EOF.
    cancel_requested: bool,
    input_stopped: bool,
}
impl ControlClient {
    pub(super) fn clear_interactive_auth(&mut self) {
        self.interactive_auth = None;
        self.auth_availability = None;
        self.auth_peer_minor = None;
    }
    fn interactive_base_ready(&self) -> bool {
        self.auth_peer_minor.is_some_and(|minor| minor >= 36)
            && self.input_method_admission == InputMethodAdmission::Ready
            && !self.auth_needs_snapshot
            && !self.needs_snapshot
            && self.revision.is_some()
            && self.locked
    }
    /// This is reported policy support, not proof of backend authentication.
    /// UI clearing token: an uncertain stream is unavailable immediately,
    /// before a later redial can increment its numeric generation.
    pub fn locked_auth_generation(&self) -> Option<u64> {
        (self.auth_peer_minor.is_some()
            && self.input_method_admission == InputMethodAdmission::Ready
            && !self.auth_needs_snapshot
            && !self.needs_snapshot
            && self.locked)
            .then_some(self.connection_generation)
    }
    pub fn interactive_auth_available(&self) -> bool {
        self.interactive_base_ready()
            && self
                .auth_availability
                .is_some_and(|availability| availability.policy_available)
    }
    pub fn interactive_auth_start_ready(&self) -> bool {
        self.interactive_auth_available()
            && self
                .auth_availability
                .is_some_and(|availability| availability.slot_idle)
            && self.interactive_auth.is_none()
            && !self.auth_request_exhausted
    }
    pub fn interactive_auth_context(&self) -> Option<Context> {
        self.interactive_auth.as_ref().map(|auth| auth.context)
    }
    fn checked_auth_id(&mut self) -> Result<u64, ControlError> {
        let id = self.next_request_id;
        let Some(next) = id
            .checked_add(1)
            .filter(|_| id != 0 && !self.auth_request_exhausted)
        else {
            self.poison_interactive_auth();
            return Err(ControlError::Unexpected(
                "authentication request ids exhausted".into(),
            ));
        };
        self.next_request_id = next;
        Ok(id)
    }
    fn write_auth_once(&mut self, bytes: &[u8]) -> Result<(), ControlError> {
        if let Err(error) = self.stream.write_all(bytes) {
            self.poison_interactive_auth();
            return Err(error.into());
        }
        Ok(())
    }
    pub(super) fn poison_interactive_auth(&mut self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
        self.clear_interactive_auth();
        self.auth_needs_snapshot = true;
        self.needs_snapshot = true;
        self.revision = None;
        self.read_buf.clear();
    }
    pub fn start_interactive_auth(&mut self) -> Result<Context, ControlError> {
        if !self.interactive_auth_available()
            || !self
                .auth_availability
                .is_some_and(|availability| availability.slot_idle)
            || self.interactive_auth.is_some()
        {
            return Err(ControlError::Unexpected(
                "interactive authentication unavailable".into(),
            ));
        }
        let id = self.checked_auth_id()?;
        let message = Message::Command {
            id,
            kind: CommandKind::AuthStart {
                method: Method::SessionStack,
            },
        };
        self.write_auth_once(&tuna_shell_control::encode_frame(&message))?;
        let context = Context {
            generation: self.connection_generation,
            start_request: id,
            key: None,
            sequence: 0,
            epoch: self.auth_availability.expect("admitted capability").epoch,
        };
        self.interactive_auth = Some(Active {
            context,
            phase: Phase::Starting,
            styles: Vec::new(),
            answer: None,
            cancel: None,
            cancel_requested: false,
            input_stopped: false,
        });
        Ok(context)
    }
    pub fn answer_interactive_auth(
        &mut self,
        context: Context,
        answers: Vec<Answer>,
    ) -> Result<u64, ControlError> {
        let eligible = self.interactive_base_ready()
            && self.interactive_auth.as_ref().is_some_and(|auth| {
                auth.context == context
                    && auth.phase == Phase::Prompt
                    && auth.answer.is_none()
                    && !auth.cancel_requested
                    && !auth.input_stopped
                    && tuna_shell_control::auth::valid_answers(&answers)
                    && answers.len() == auth.styles.len()
                    && auth
                        .styles
                        .iter()
                        .zip(&answers)
                        .all(|(style, answer)| match style {
                            Style::Visible | Style::Secret => answer.text.is_some(),
                            Style::Info | Style::Error => answer.text.is_none(),
                        })
            });
        if !eligible {
            return Err(ControlError::Unexpected(
                "stale authentication response".into(),
            ));
        }
        let key = context
            .key
            .ok_or_else(|| ControlError::Unexpected("missing authentication key".into()))?;
        let id = self.checked_auth_id()?;
        let frame =
            tuna_shell_control::auth::encode_answer_frame(id, key, context.sequence, answers)?;
        self.write_auth_once(frame.as_bytes())?;
        let auth = self
            .interactive_auth
            .as_mut()
            .expect("successful owned write");
        auth.answer = Some(id);
        auth.phase = Phase::Verifying;
        Ok(id)
    }
    pub fn cancel_interactive_auth(&mut self, context: Context) -> Result<u64, ControlError> {
        if !self.interactive_base_ready()
            || !self
                .interactive_auth
                .as_ref()
                .is_some_and(|auth| auth.context == context && !auth.cancel_requested)
        {
            return Err(ControlError::Unexpected(
                "stale authentication cancellation".into(),
            ));
        }
        let id = self.checked_auth_id()?;
        let message = Message::Command {
            id,
            kind: CommandKind::AuthCancel {
                start_request: context.start_request,
                key: context.key,
            },
        };
        self.write_auth_once(&tuna_shell_control::encode_frame(&message))?;
        let auth = self
            .interactive_auth
            .as_mut()
            .expect("successful owned write");
        auth.cancel = Some(id);
        auth.cancel_requested = true;
        auth.phase = Phase::Cancelling;
        auth.styles.clear();
        Ok(id)
    }
    pub(super) fn handle_auth_availability(
        &mut self,
        availability: Availability,
    ) -> Result<Handled, ControlError> {
        if !self.interactive_base_ready() {
            self.poison_interactive_auth();
            return Err(ControlError::Unexpected(
                "authentication capability before admission".into(),
            ));
        }
        if self
            .interactive_auth
            .as_ref()
            .is_some_and(|auth| auth.context.epoch != availability.epoch)
        {
            self.poison_interactive_auth();
            return Err(ControlError::Unexpected(
                "authentication epoch changed".into(),
            ));
        }
        self.auth_availability = Some(availability);
        Ok(Handled::InteractiveAuth(Event::Availability(availability)))
    }
    pub(super) fn handle_auth_state(
        &mut self,
        start_request: u64,
        key: Option<Key>,
        phase: Phase,
    ) -> Result<Handled, ControlError> {
        if !self.interactive_base_ready() {
            self.poison_interactive_auth();
            return Err(ControlError::Unexpected(
                "authentication state before admission".into(),
            ));
        }
        let Some(auth) = &mut self.interactive_auth else {
            return Ok(Handled::InteractiveAuth(Event::Ignored));
        };
        let matching = auth.context.start_request == start_request
            && auth.context.generation == self.connection_generation
            && key.is_none_or(|key| {
                self.auth_availability
                    .is_some_and(|a| key.epoch == a.epoch && key.epoch == auth.context.epoch)
                    && auth.context.key.is_none_or(|current| current == key)
            });
        if !matching {
            return Ok(Handled::InteractiveAuth(Event::Ignored));
        }
        if key.is_none() && !matches!(phase, Phase::Unavailable | Phase::Blocked | Phase::Denied) {
            self.poison_interactive_auth();
            return Err(ControlError::Unexpected(
                "unbound authentication state".into(),
            ));
        }
        if (auth.cancel_requested || auth.input_stopped)
            && matches!(phase, Phase::Prompt | Phase::Verifying | Phase::Starting)
        {
            return Ok(Handled::InteractiveAuth(Event::Ignored));
        }
        if let Some(key) = key {
            auth.context.key = Some(key);
        }
        auth.phase = phase;
        auth.input_stopped |= matches!(
            phase,
            Phase::Cancelling | Phase::Blocked | Phase::Unavailable
        );
        let context = auth.context;
        // A Denied state follows the server's physical receipt; a bare
        // cancellation/root acknowledgement must never imply retirement.
        if phase == Phase::Denied
            || (key.is_none() && matches!(phase, Phase::Unavailable | Phase::Blocked))
        {
            self.interactive_auth = None;
        }
        Ok(Handled::InteractiveAuth(Event::State { context, phase }))
    }
    pub(super) fn handle_auth_challenge(
        &mut self,
        key: Key,
        sequence: u32,
        messages: Vec<Prompt>,
    ) -> Result<Handled, ControlError> {
        if !self.interactive_base_ready() {
            self.poison_interactive_auth();
            return Err(ControlError::Unexpected(
                "authentication challenge before admission".into(),
            ));
        }
        let Some(auth) = &mut self.interactive_auth else {
            return Ok(Handled::InteractiveAuth(Event::Ignored));
        };
        let matching = auth.context.generation == self.connection_generation
            && auth.context.start_request == key.start_request
            && auth.context.key.is_none_or(|current| current == key)
            && self.auth_availability.is_some_and(|availability| {
                key.epoch == availability.epoch && key.epoch == auth.context.epoch
            })
            && auth.context.sequence.checked_add(1) == Some(sequence)
            && !auth.cancel_requested
            && !auth.input_stopped
            && auth.answer.is_none()
            && tuna_shell_control::auth::valid_prompts(&messages);
        if !matching {
            return Ok(Handled::InteractiveAuth(Event::Ignored));
        }
        auth.context.key = Some(key);
        auth.context.sequence = sequence;
        auth.styles = messages.iter().map(|message| message.style).collect();
        auth.phase = Phase::Prompt;
        Ok(Handled::InteractiveAuth(Event::Challenge {
            context: auth.context,
            messages,
        }))
    }
    pub(super) fn interactive_reply(&mut self, id: u64, status: &CommandStatus) -> Option<Event> {
        let auth = self.interactive_auth.as_mut()?;
        let applied = matches!(status, CommandStatus::Applied);
        let context = auth.context;
        if auth.answer == Some(id) {
            auth.answer = None;
            if !applied
                && !auth.cancel_requested
                && !auth.input_stopped
                && auth.phase == Phase::Verifying
            {
                auth.phase = Phase::Prompt;
            }
            Some(Event::AnswerResult { context, applied })
        } else if auth.cancel == Some(id) {
            auth.cancel = None;
            Some(Event::CancelResult { context, applied })
        } else if context.start_request == id {
            Some(Event::RootResult { context, applied })
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::unix::net::UnixStream;
    use std::time::Duration;
    use tuna_shell_control::{auth::AnswerText, CURRENT_VERSION};
    fn pair() -> (ControlClient, UnixStream) {
        let (local, peer) = UnixStream::pair().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        (ControlClient::new(local).unwrap(), peer)
    }
    fn send(peer: &mut UnixStream, message: Message) {
        peer.write_all(&tuna_shell_control::encode_frame(&message))
            .unwrap();
    }
    fn read(peer: &mut UnixStream) -> Message {
        let mut prefix = [0; 4];
        peer.read_exact(&mut prefix).unwrap();
        let mut frame = prefix.to_vec();
        let length = u32::from_le_bytes(prefix) as usize;
        frame.resize(length + 4, 0);
        peer.read_exact(&mut frame[4..]).unwrap();
        tuna_shell_control::decode_frame(&frame).unwrap()
    }
    fn ready() -> (ControlClient, UnixStream) {
        let (mut client, mut peer) = pair();
        send(
            &mut peer,
            Message::Hello {
                version: CURRENT_VERSION,
            },
        );
        client.poll().unwrap();
        send(
            &mut peer,
            Message::Snapshot {
                revision: 0,
                windows: vec![],
                workspaces: vec![],
                locked: true,
            },
        );
        client.poll().unwrap();
        send(
            &mut peer,
            Message::AuthAvailability(Availability {
                epoch: 7,
                policy_available: true,
                slot_idle: true,
            }),
        );
        assert!(matches!(
            client.poll().unwrap(),
            Handled::InteractiveAuth(Event::Availability(_))
        ));
        (client, peer)
    }
    fn challenge(client: &mut ControlClient, peer: &mut UnixStream, context: Context) -> Context {
        let key = Key {
            start_request: context.start_request,
            attempt_serial: 42,
            epoch: context.epoch,
        };
        send(
            peer,
            Message::AuthState {
                start_request: context.start_request,
                key: Some(key),
                phase: Phase::Starting,
            },
        );
        client.poll().unwrap();
        send(
            peer,
            Message::AuthChallenge {
                key,
                sequence: 1,
                messages: vec![Prompt {
                    index: 0,
                    style: Style::Secret,
                    text: "Separate credential".into(),
                }],
            },
        );
        let Handled::InteractiveAuth(Event::Challenge { context, .. }) = client.poll().unwrap()
        else {
            panic!()
        };
        context
    }
    fn answers() -> Vec<Answer> {
        vec![Answer {
            index: 0,
            text: Some(AnswerText("owned-fixture-only".into())),
        }]
    }
    #[test]
    fn capabilities_before_hello_snapshot_or_locked_state_never_admit_start() {
        for stage in 0..3 {
            let (mut client, mut peer) = pair();
            if stage >= 1 {
                send(
                    &mut peer,
                    Message::Hello {
                        version: CURRENT_VERSION,
                    },
                );
                client.poll().unwrap();
            }
            if stage >= 2 {
                send(
                    &mut peer,
                    Message::Snapshot {
                        revision: 0,
                        windows: vec![],
                        workspaces: vec![],
                        locked: false,
                    },
                );
                client.poll().unwrap();
            }
            send(
                &mut peer,
                Message::AuthAvailability(Availability {
                    epoch: 7,
                    policy_available: true,
                    slot_idle: true,
                }),
            );
            assert!(client.poll().is_err());
            assert!(!client.interactive_auth_available());
            assert!(client.start_interactive_auth().is_err());
            assert_eq!(
                peer.read(&mut [0u8; 1]).unwrap(),
                0,
                "invalid capability poisons old stream"
            );
        }
    }
    #[test]
    fn policy_and_common_slot_busy_are_distinct_from_owned_prompt_authority() {
        let (mut client, mut peer) = ready();
        send(
            &mut peer,
            Message::AuthAvailability(Availability {
                epoch: 7,
                policy_available: false,
                slot_idle: true,
            }),
        );
        client.poll().unwrap();
        assert!(client.start_interactive_auth().is_err());
        send(
            &mut peer,
            Message::AuthAvailability(Availability {
                epoch: 7,
                policy_available: true,
                slot_idle: false,
            }),
        );
        client.poll().unwrap();
        assert!(client.interactive_auth_available());
        assert!(client.start_interactive_auth().is_err());
        send(
            &mut peer,
            Message::AuthAvailability(Availability {
                epoch: 7,
                policy_available: true,
                slot_idle: true,
            }),
        );
        client.poll().unwrap();
        let root = client.start_interactive_auth().unwrap();
        assert!(matches!(
            read(&mut peer),
            Message::Command {
                kind: CommandKind::AuthStart { .. },
                ..
            }
        ));
        send(
            &mut peer,
            Message::AuthAvailability(Availability {
                epoch: 7,
                policy_available: true,
                slot_idle: false,
            }),
        );
        client.poll().unwrap();
        let context = challenge(&mut client, &mut peer, root);
        let answer_id = client.answer_interactive_auth(context, answers()).unwrap();
        assert!(
            matches!(read(&mut peer), Message::Command { id, kind:CommandKind::AuthAnswer { .. } } if id == answer_id)
        );
        assert!(
            client.answer_interactive_auth(context, answers()).is_err(),
            "one bounded outstanding answer"
        );
    }
    #[test]
    fn stale_epoch_generation_root_and_sequence_cannot_send_answer_or_cancel() {
        let (mut client, mut peer) = ready();
        let root = client.start_interactive_auth().unwrap();
        read(&mut peer);
        let context = challenge(&mut client, &mut peer, root);
        for stale in [
            Context {
                generation: context.generation + 1,
                ..context
            },
            Context {
                start_request: context.start_request + 1,
                ..context
            },
            Context {
                sequence: context.sequence + 1,
                ..context
            },
            Context {
                epoch: context.epoch + 1,
                ..context
            },
        ] {
            assert!(client.answer_interactive_auth(stale, answers()).is_err());
            assert!(client.cancel_interactive_auth(stale).is_err());
        }
        peer.set_nonblocking(true).unwrap();
        assert_eq!(
            peer.read(&mut [0; 1]).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        peer.set_nonblocking(false).unwrap();
        send(
            &mut peer,
            Message::AuthAvailability(Availability {
                epoch: 8,
                policy_available: true,
                slot_idle: true,
            }),
        );
        assert!(client.poll().is_err());
        assert!(client.interactive_auth_context().is_none());
        assert!(client.answer_interactive_auth(context, answers()).is_err());
    }
    #[test]
    fn prekey_start_epoch_change_cancels_old_authority_and_foreign_challenge_is_ignored() {
        let (mut client, mut peer) = ready();
        let root = client.start_interactive_auth().unwrap();
        read(&mut peer);
        send(
            &mut peer,
            Message::AuthChallenge {
                key: Key {
                    start_request: root.start_request + 1,
                    attempt_serial: 42,
                    epoch: 7,
                },
                sequence: 1,
                messages: vec![Prompt {
                    index: 0,
                    style: Style::Secret,
                    text: "foreign-not-displayed".into(),
                }],
            },
        );
        assert_eq!(
            client.poll().unwrap(),
            Handled::InteractiveAuth(Event::Ignored)
        );
        assert_eq!(client.interactive_auth_context(), Some(root));
        send(
            &mut peer,
            Message::AuthAvailability(Availability {
                epoch: 8,
                policy_available: true,
                slot_idle: true,
            }),
        );
        assert!(client.poll().is_err());
        assert!(client.interactive_auth_context().is_none());
    }
    #[test]
    fn cancellation_reply_does_not_retire_but_denied_state_does_and_applied_never_unlocks() {
        let (mut client, mut peer) = ready();
        let root = client.start_interactive_auth().unwrap();
        read(&mut peer);
        let context = challenge(&mut client, &mut peer, root);
        let id = client.cancel_interactive_auth(context).unwrap();
        assert!(matches!(
            read(&mut peer),
            Message::Command {
                kind: CommandKind::AuthCancel { .. },
                ..
            }
        ));
        assert!(client.cancel_interactive_auth(context).is_err());
        send(
            &mut peer,
            Message::CommandResult {
                id,
                status: CommandStatus::Applied,
            },
        );
        client.poll().unwrap();
        assert_eq!(client.interactive_auth_context(), Some(context));
        assert!(client.locked());
        send(
            &mut peer,
            Message::CommandResult {
                id: root.start_request,
                status: CommandStatus::Applied,
            },
        );
        client.poll().unwrap();
        assert_eq!(client.interactive_auth_context(), Some(context));
        assert!(client.locked());
        send(
            &mut peer,
            Message::AuthState {
                start_request: root.start_request,
                key: context.key,
                phase: Phase::Denied,
            },
        );
        client.poll().unwrap();
        assert!(client.interactive_auth_context().is_none());
        assert!(client.locked());
        send(
            &mut peer,
            Message::Snapshot {
                revision: 1,
                windows: vec![],
                workspaces: vec![],
                locked: false,
            },
        );
        client.poll().unwrap();
        assert!(!client.locked());
        assert!(!client.interactive_auth_available());
    }
    #[test]
    fn cancel_ack_cannot_reopen_delayed_prompt_or_challenge_authority() {
        let (mut client, mut peer) = ready();
        let root = client.start_interactive_auth().unwrap();
        read(&mut peer);
        let context = challenge(&mut client, &mut peer, root);
        let cancel_id = client.cancel_interactive_auth(context).unwrap();
        read(&mut peer);
        send(
            &mut peer,
            Message::CommandResult {
                id: cancel_id,
                status: CommandStatus::Applied,
            },
        );
        assert!(matches!(
            client.poll().unwrap(),
            Handled::InteractiveAuth(Event::CancelResult { applied: true, .. })
        ));
        // A queued valid next batch may outlive the cancellation ack.
        send(
            &mut peer,
            Message::AuthChallenge {
                key: context.key.unwrap(),
                sequence: context.sequence + 1,
                messages: vec![Prompt {
                    index: 0,
                    style: Style::Secret,
                    text: "late-owned-prompt".into(),
                }],
            },
        );
        assert_eq!(
            client.poll().unwrap(),
            Handled::InteractiveAuth(Event::Ignored)
        );
        for phase in [Phase::Starting, Phase::Prompt, Phase::Verifying] {
            send(
                &mut peer,
                Message::AuthState {
                    start_request: context.start_request,
                    key: context.key,
                    phase,
                },
            );
            assert_eq!(
                client.poll().unwrap(),
                Handled::InteractiveAuth(Event::Ignored)
            );
        }
        assert_eq!(client.interactive_auth_context(), Some(context));
        assert_eq!(
            client.interactive_auth.as_ref().unwrap().phase,
            Phase::Cancelling
        );
        assert!(client.answer_interactive_auth(context, answers()).is_err());
        assert!(client.cancel_interactive_auth(context).is_err());
        assert!(client.start_interactive_auth().is_err());
        peer.set_nonblocking(true).unwrap();
        assert_eq!(
            peer.read(&mut [0; 1]).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        peer.set_nonblocking(false).unwrap();
        send(
            &mut peer,
            Message::AuthState {
                start_request: context.start_request,
                key: context.key,
                phase: Phase::Denied,
            },
        );
        client.poll().unwrap();
        assert!(client.interactive_auth_context().is_none());
        assert!(client.locked());
    }
    #[test]
    fn actual_socket_gap_or_decoded_error_cannot_recover_old_prompt_on_snapshot() {
        for failure in [
            Message::Changes {
                from_revision: 99,
                to_revision: 100,
                ops: vec![],
            },
            Message::Error {
                kind: tuna_shell_control::ErrorKind::RevisionGap,
                message: "owned gap".into(),
            },
            Message::Error {
                kind: tuna_shell_control::ErrorKind::IncompatibleVersion,
                message: "owned incompatibility".into(),
            },
            Message::Command {
                id: 90,
                kind: CommandKind::ToggleOverview,
            },
        ] {
            let (mut client, mut peer) = ready();
            let root = client.start_interactive_auth().unwrap();
            read(&mut peer);
            let context = challenge(&mut client, &mut peer, root);
            // Queue a locked snapshot before dispatching the failure, using
            // the actual socket. It must not resurrect the cached authority.
            send(&mut peer, failure);
            send(
                &mut peer,
                Message::Snapshot {
                    revision: 100,
                    windows: vec![],
                    workspaces: vec![],
                    locked: true,
                },
            );
            let _ = client.poll();
            assert!(client.interactive_auth_context().is_none());
            assert!(client.answer_interactive_auth(context, answers()).is_err());
            assert!(client.cancel_interactive_auth(context).is_err());
            assert!(client.start_interactive_auth().is_err());
            assert!(
                client.poll().is_err(),
                "poisoned stream cannot deliver queued snapshot"
            );
            assert!(client.answer_interactive_auth(context, answers()).is_err());
        }
    }
    #[test]
    fn negative_answer_retry_requires_still_verifying_owned_phase() {
        let (mut client, mut peer) = ready();
        let root = client.start_interactive_auth().unwrap();
        read(&mut peer);
        let context = challenge(&mut client, &mut peer, root);
        let answer = client.answer_interactive_auth(context, answers()).unwrap();
        read(&mut peer);
        send(
            &mut peer,
            Message::CommandResult {
                id: answer,
                status: CommandStatus::Denied {
                    reason: "owned response refusal".into(),
                },
            },
        );
        client.poll().unwrap();
        assert_eq!(
            client.interactive_auth.as_ref().unwrap().phase,
            Phase::Prompt
        );
        let fresh_answer = client.answer_interactive_auth(context, answers()).unwrap();
        assert!(fresh_answer > answer);
        assert!(
            matches!(read(&mut peer), Message::Command { id, kind: CommandKind::AuthAnswer { .. } } if id == fresh_answer)
        );
        assert!(client.locked());
    }
    #[test]
    fn negative_answer_ack_never_reopens_server_cancelling_or_blocked_phase() {
        for stopped in [Phase::Cancelling, Phase::Blocked] {
            let (mut client, mut peer) = ready();
            let root = client.start_interactive_auth().unwrap();
            read(&mut peer);
            let context = challenge(&mut client, &mut peer, root);
            let answer = client.answer_interactive_auth(context, answers()).unwrap();
            read(&mut peer);
            send(
                &mut peer,
                Message::AuthState {
                    start_request: context.start_request,
                    key: context.key,
                    phase: stopped,
                },
            );
            client.poll().unwrap();
            for id in [context.start_request, answer] {
                send(
                    &mut peer,
                    Message::CommandResult {
                        id,
                        status: CommandStatus::Denied {
                            reason: "owned stopped phase".into(),
                        },
                    },
                );
                client.poll().unwrap();
            }
            assert_eq!(client.interactive_auth.as_ref().unwrap().phase, stopped);
            send(
                &mut peer,
                Message::AuthState {
                    start_request: context.start_request,
                    key: context.key,
                    phase: Phase::Prompt,
                },
            );
            assert_eq!(
                client.poll().unwrap(),
                Handled::InteractiveAuth(Event::Ignored)
            );
            send(
                &mut peer,
                Message::AuthChallenge {
                    key: context.key.unwrap(),
                    sequence: context.sequence + 1,
                    messages: vec![Prompt {
                        index: 0,
                        style: Style::Secret,
                        text: "late blocked batch".into(),
                    }],
                },
            );
            assert_eq!(
                client.poll().unwrap(),
                Handled::InteractiveAuth(Event::Ignored)
            );
            assert!(client.answer_interactive_auth(context, answers()).is_err());
            assert!(client.interactive_auth_context().is_some());
            assert!(client.locked());
            peer.set_nonblocking(true).unwrap();
            assert_eq!(
                peer.read(&mut [0; 1]).unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock
            );
            peer.set_nonblocking(false).unwrap();
            send(
                &mut peer,
                Message::AuthState {
                    start_request: context.start_request,
                    key: context.key,
                    phase: Phase::Denied,
                },
            );
            client.poll().unwrap();
            assert!(client.interactive_auth_context().is_none());
            assert!(client.locked());
        }
    }
    #[test]
    fn partial_or_wouldblock_answer_write_poisoning_has_no_resend_or_cached_response() {
        let (mut client, mut peer) = ready();
        let root = client.start_interactive_auth().unwrap();
        read(&mut peer);
        let context = challenge(&mut client, &mut peer, root);
        rustix::net::sockopt::set_socket_send_buffer_size(&client.stream, 4096).unwrap();
        let mut filled = 0;
        loop {
            match client.stream.write(&[0u8; 8192]) {
                Ok(n) => {
                    assert!(n > 0);
                    filled += n;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => panic!("owned saturation failed: {error}"),
            }
        }
        assert!(filled > 0);
        assert!(client.answer_interactive_auth(context, answers()).is_err());
        assert!(client.interactive_auth_context().is_none());
        assert!(client.answer_interactive_auth(context, answers()).is_err());
        assert!(client.start_interactive_auth().is_err());
        assert!(client.auth_needs_snapshot);
    }
    #[test]
    fn real_reconnect_receives_only_hello_and_requires_fresh_explicit_start() {
        use std::os::unix::net::UnixListener;
        use std::sync::mpsc;
        use std::time::Instant;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("control.sock");
        let listener = UnixListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        let (actions, commands) = mpsc::channel();
        let (observed, receipts) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let accept = || {
                let end = Instant::now() + Duration::from_secs(3);
                loop {
                    let attempt = listener.accept();
                    assert!(Instant::now() < end, "owned socket acceptance deadline");
                    match attempt {
                        Ok((stream, _)) => {
                            stream
                                .set_read_timeout(Some(Duration::from_secs(1)))
                                .unwrap();
                            return stream;
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(1))
                        }
                        Err(error) => panic!("owned listener failed: {error}"),
                    }
                }
            };
            let greet = |peer: &mut UnixStream| {
                send(
                    peer,
                    Message::Hello {
                        version: CURRENT_VERSION,
                    },
                );
                send(
                    peer,
                    Message::Snapshot {
                        revision: 0,
                        windows: vec![],
                        workspaces: vec![],
                        locked: true,
                    },
                );
                send(
                    peer,
                    Message::AuthAvailability(Availability {
                        epoch: 7,
                        policy_available: true,
                        slot_idle: true,
                    }),
                );
            };
            let mut first = accept();
            assert!(matches!(read(&mut first), Message::Hello { .. }));
            greet(&mut first);
            let Message::Command {
                id: old_root,
                kind: CommandKind::AuthStart { .. },
            } = read(&mut first)
            else {
                panic!("fresh original Start required")
            };
            commands.recv_timeout(Duration::from_secs(3)).unwrap();
            first.shutdown(std::net::Shutdown::Both).unwrap();
            drop(first);
            observed.send("closed").unwrap();
            let mut second = accept();
            assert!(
                matches!(read(&mut second), Message::Hello { .. }),
                "redial sends Hello only"
            );
            greet(&mut second);
            commands.recv_timeout(Duration::from_secs(3)).unwrap();
            second
                .set_read_timeout(Some(Duration::from_millis(30)))
                .unwrap();
            assert!(
                matches!(second.read(&mut [0u8;1]), Err(error) if matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut)),
                "no automatic Start or credentials after reconnect"
            );
            second
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            observed.send("no-replay").unwrap();
            let Message::Command {
                id: new_root,
                kind: CommandKind::AuthStart { .. },
            } = read(&mut second)
            else {
                panic!("fresh explicit action required")
            };
            assert!(new_root > old_root);
            observed.send("new-start").unwrap();
        });
        let mut client = ControlClient::connect(&path).unwrap();
        client.send_hello().unwrap();
        let wait_ready = |client: &mut ControlClient| {
            let end = Instant::now() + Duration::from_secs(3);
            while !client.interactive_auth_start_ready() {
                let result = client.poll();
                assert!(Instant::now() < end, "fresh connection admission deadline");
                match result {
                    Ok(_) => {}
                    Err(error)
                        if matches!(&error, ControlError::WouldBlock)
                            || matches!(&error, ControlError::Io(e) if e.kind()==std::io::ErrorKind::WouldBlock) =>
                    {
                        std::thread::sleep(Duration::from_millis(1))
                    }
                    Err(error) => panic!("fresh peer failed: {error:?}"),
                }
            }
        };
        wait_ready(&mut client);
        let old = client.start_interactive_auth().unwrap();
        actions.send(()).unwrap();
        assert_eq!(
            receipts.recv_timeout(Duration::from_secs(3)).unwrap(),
            "closed"
        );
        assert!(client.poll().is_err());
        assert_ne!(client.connection_generation(), old.generation);
        assert!(client.interactive_auth_context().is_none());
        assert!(client.cancel_interactive_auth(old).is_err());
        wait_ready(&mut client);
        assert!(client.cancel_interactive_auth(old).is_err());
        actions.send(()).unwrap();
        assert_eq!(
            receipts.recv_timeout(Duration::from_secs(3)).unwrap(),
            "no-replay"
        );
        let fresh = client.start_interactive_auth().unwrap();
        assert_ne!(fresh.generation, old.generation);
        assert_eq!(
            receipts.recv_timeout(Duration::from_secs(3)).unwrap(),
            "new-start"
        );
        server.join().unwrap();
    }
    #[test]
    fn legacy035_compatible_snapshot_has_no_interactive_availability() {
        let (mut client, mut peer) = pair();
        send(
            &mut peer,
            Message::Hello {
                version: tuna_shell_control::ProtocolVersion::new(0, 35),
            },
        );
        client.poll().unwrap();
        send(
            &mut peer,
            Message::Snapshot {
                revision: 0,
                windows: vec![],
                workspaces: vec![],
                locked: true,
            },
        );
        client.poll().unwrap();
        assert!(client.start_interactive_auth().is_err());
        assert!(!client.interactive_auth_available());
    }
}
