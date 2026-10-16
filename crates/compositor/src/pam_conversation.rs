//! Internal, fresh-response PAM relay. This API never grants a lock transition.
//! Its trusted runtime caller must retain the common auth Slot and validate the
//! live origin/epoch before displaying a challenge or applying a retired result.
pub use crate::auth_owner::{MethodKind, Origin};
use crate::pam_cancel::Cancellation;
use crate::pam_helper::{Answer, Message, Style, Supervisor};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

pub const INACTIVITY: Duration = Duration::from_secs(120);
pub const MAX_TOTAL: Duration = Duration::from_secs(3600);
/// A caller-admitted method policy, not a silent global MFA timeout. The UI idle
/// clock is separate from backend phases and the explicit total ceiling.
#[derive(Clone, Copy, Debug)]
pub struct Policy {
    pub backend_phase: Duration,
    pub total: Duration,
}
impl Policy {
    pub fn validate(self) -> io::Result<Self> {
        if self.backend_phase.is_zero()
            || self.backend_phase > Duration::from_secs(120)
            || self.total < INACTIVITY
            || self.total > MAX_TOTAL
            || self.backend_phase > self.total
        {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        Ok(self)
    }
}
/// Original common Slot identity. It is supplied by the trusted runtime, never
/// taken from a shell command or a PAM prompt. These fields are not an auth grant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Binding {
    pub origin: Origin,
    pub attempt: u64,
    pub epoch: u64,
}
pub struct Challenge {
    pub binding: Binding,
    pub sequence: u32,
    pub messages: Vec<Message>,
    pub response: ResponseHandle,
}
impl std::fmt::Debug for Challenge {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.debug_struct("Challenge")
            .field("binding", &self.binding)
            .field("sequence", &self.sequence)
            .field("message_count", &self.messages.len())
            .finish()
    }
}
enum Input {
    Answers(Binding, u32, Vec<Answer>, Instant),
    Activity(Binding, u32, Instant),
}
pub struct ResponseHandle {
    binding: Binding,
    sequence: u32,
    sender: mpsc::SyncSender<Input>,
    wake: UnixStream,
    cancel: Cancellation,
    submitted: bool,
    activity_pending: Arc<AtomicBool>,
}
impl ResponseHandle {
    /// Called only after an actual user input event, not on ticks, focus, a
    /// backend message, or a redraw. Invalid bindings cannot extend inactivity.
    pub fn note_user_input(&mut self, binding: Binding, sequence: u32) -> io::Result<()> {
        if binding != self.binding || sequence != self.sequence || self.cancel.is_canceled() {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        // Reserve one activity slot and one answer slot. Coalesced keystrokes
        // cannot fill the answer queue or extend idle without a real input event.
        if self.activity_pending.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let result = self.send(
            Input::Activity(binding, sequence, Instant::now()),
            binding,
            sequence,
        );
        if result.is_err() {
            self.activity_pending.store(false, Ordering::Release);
        }
        result
    }
    /// Consumes the one response capability. Each Secret or Visible answer must
    /// come from fresh input for this exact batch; Info/Error must have no text.
    /// AnswerText wipes owned buffers even on rejection or queue/receiver loss.
    pub fn submit(
        mut self,
        binding: Binding,
        sequence: u32,
        answers: Vec<Answer>,
    ) -> io::Result<()> {
        self.send(
            Input::Answers(binding, sequence, answers, Instant::now()),
            binding,
            sequence,
        )?;
        self.submitted = true;
        Ok(())
    }
    fn send(&mut self, input: Input, binding: Binding, sequence: u32) -> io::Result<()> {
        if binding != self.binding || sequence != self.sequence || self.cancel.is_canceled() {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        self.sender
            .try_send(input)
            .map_err(|_| io::ErrorKind::WouldBlock)?;
        if self.wake.write_all(&[1]).is_err() {
            self.cancel.cancel();
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        Ok(())
    }
}
impl Drop for ResponseHandle {
    fn drop(&mut self) {
        if !self.submitted {
            self.cancel.cancel();
        }
    }
}
/// Shared receipt clock: worker and receiver observe the SAME total Instant and
/// latest admitted input. A queued successful result cannot outlive either.
pub(crate) struct Timing {
    total_deadline: Instant,
    last_input: Mutex<Instant>,
}
impl Timing {
    fn deadline_at(&self, now: Instant) -> Option<Instant> {
        let last_input = *self.last_input.lock().ok()?;
        let deadline = self.total_deadline.min(last_input + INACTIVITY);
        (now < deadline).then_some(deadline)
    }
    fn input_at(&self, input: Instant) -> io::Result<()> {
        let now = Instant::now();
        let mut last = self
            .last_input
            .lock()
            .map_err(|_| io::ErrorKind::PermissionDenied)?;
        if now >= self.total_deadline || now >= *last + INACTIVITY {
            return Err(io::ErrorKind::TimedOut.into());
        }
        *last = (*last).max(input.min(now));
        Ok(())
    }
}
pub(crate) struct Relay {
    binding: Binding,
    policy: Policy,
    timing: Arc<Timing>,
    challenges: mpsc::SyncSender<Challenge>,
    cancel: Cancellation,
    notice: crate::wake::Wake,
}
pub struct Receiver {
    pub(crate) challenges: mpsc::Receiver<Challenge>,
    timing: Arc<Timing>,
    notice_source: Option<calloop::ping::PingSource>,
}
impl Receiver {
    pub(crate) fn take_notice_source(&mut self) -> Option<calloop::ping::PingSource> {
        self.notice_source.take()
    }
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        Some(
            self.timing
                .last_input
                .lock()
                .ok()
                .map_or_else(Instant::now, |last| {
                    self.timing.total_deadline.min(*last + INACTIVITY)
                }),
        )
    }
    pub(crate) fn timely_at(&self, now: Instant) -> bool {
        self.timing.deadline_at(now).is_some()
    }
    #[cfg(test)]
    pub(crate) fn expire_idle_for_test(&self) {
        *self.timing.last_input.lock().unwrap() = Instant::now() - INACTIVITY;
    }
    #[cfg(test)]
    pub(crate) fn total_deadline_for_test(&self) -> Instant {
        self.timing.total_deadline
    }
    /// The actual runtime must recheck its live Slot/origin/epoch here. Merely
    /// receiving a backend challenge is not permission to show an old prompt.
    pub fn try_challenge(&self) -> Result<Challenge, mpsc::TryRecvError> {
        self.challenges.try_recv()
    }
}
pub(crate) fn channel(
    binding: Binding,
    policy: Policy,
    cancel: Cancellation,
) -> io::Result<(Relay, Receiver)> {
    let policy = policy.validate()?;
    if binding.origin.connection == 0 || binding.attempt == 0 || binding.epoch == u64::MAX {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let now = Instant::now();
    let (notice, notice_source) = crate::wake::Wake::new()?;
    let (send, receive) = mpsc::sync_channel(1);
    let timing = Arc::new(Timing {
        total_deadline: now + policy.total,
        last_input: Mutex::new(now),
    });
    Ok((
        Relay {
            binding,
            policy,
            timing: timing.clone(),
            challenges: send,
            cancel,
            notice,
        },
        Receiver {
            challenges: receive,
            timing,
            notice_source: Some(notice_source),
        },
    ))
}
impl Relay {
    pub(crate) fn notice(&self) -> crate::wake::Wake {
        self.notice.clone()
    }
    pub(crate) fn total(&self) -> Duration {
        self.policy.total
    }
    pub(crate) fn total_deadline(&self) -> Instant {
        self.timing.total_deadline
    }
    pub(crate) fn phase(&self) -> io::Result<Instant> {
        let now = Instant::now();
        let deadline = self
            .timing
            .deadline_at(now)
            .ok_or(io::ErrorKind::TimedOut)?;
        Ok((now + self.policy.backend_phase).min(deadline))
    }
    pub(crate) fn timely(&self) -> bool {
        !self.cancel.is_canceled() && self.timing.deadline_at(Instant::now()).is_some()
    }
    /// Runs only on the existing stable creator thread. No Supervisor is moved
    /// to a UI worker; cancellation/quarantine remains with that creator.
    pub(crate) fn answer(
        &mut self,
        supervisor: &mut Supervisor,
        sequence: u32,
        messages: Vec<Message>,
    ) -> io::Result<()> {
        let styles: Vec<_> = messages.iter().map(|message| message.style).collect();
        let (read, write) = UnixStream::pair()?;
        read.set_nonblocking(true)?;
        write.set_nonblocking(true)?;
        let (sender, receive) = mpsc::sync_channel(2);
        let activity_pending = Arc::new(AtomicBool::new(false));
        self.challenges
            .try_send(Challenge {
                binding: self.binding,
                sequence,
                messages,
                response: ResponseHandle {
                    binding: self.binding,
                    sequence,
                    sender,
                    wake: write,
                    cancel: self.cancel.clone(),
                    submitted: false,
                    activity_pending: activity_pending.clone(),
                },
            })
            .map_err(|_| io::ErrorKind::BrokenPipe)?;
        self.notice.signal();
        let answers = self.wait_answers(&read, &receive, sequence, &styles, &activity_pending)?;
        // Fresh input authorizes only this response; it cannot extend the total
        // ceiling or bypass the separate bounded backend IPC phase.
        supervisor.set_phase_deadline(self.phase()?)?;
        supervisor.answer(sequence, answers)
    }
    fn wait_answers(
        &mut self,
        read: &UnixStream,
        receive: &mpsc::Receiver<Input>,
        sequence: u32,
        styles: &[Style],
        activity_pending: &AtomicBool,
    ) -> io::Result<Vec<Answer>> {
        use std::io::Read;
        let mut wake = read;
        loop {
            let now = Instant::now();
            let deadline = self
                .timing
                .deadline_at(now)
                .ok_or(io::ErrorKind::TimedOut)?;
            if self.cancel.is_canceled() {
                return Err(io::ErrorKind::TimedOut.into());
            }
            let mut byte = [0; 64];
            while wake.read(&mut byte).is_ok_and(|count| count > 0) {}
            self.cancel.drain()?;
            match receive.try_recv() {
                Ok(Input::Activity(binding, incoming, input_at)) => {
                    if binding != self.binding || incoming != sequence {
                        return Err(io::ErrorKind::PermissionDenied.into());
                    }
                    activity_pending.store(false, Ordering::Release);
                    self.timing.input_at(input_at)?;
                    self.notice.signal();
                    continue;
                }
                Ok(Input::Answers(binding, incoming, answers, input_at)) => {
                    if binding != self.binding
                        || incoming != sequence
                        || !crate::pam_helper::valid_answers(styles, &answers)
                        || !self.timely()
                    {
                        return Err(io::ErrorKind::InvalidInput.into());
                    }
                    if styles
                        .iter()
                        .any(|style| matches!(style, Style::Visible | Style::Secret))
                    {
                        self.timing.input_at(input_at)?;
                        self.notice.signal();
                    }
                    return Ok(answers);
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(io::ErrorKind::BrokenPipe.into());
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
            let mut fds = [
                libc::pollfd {
                    fd: read.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: self.cancel.fd().ok_or(io::ErrorKind::PermissionDenied)?,
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            let wait = deadline.saturating_duration_since(Instant::now());
            let millis = wait.as_millis().min(i32::MAX as u128) as i32;
            // SAFETY: initialized owned/borrowed fds live across this bounded poll.
            let status = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, millis.max(1)) };
            if status < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                return Err(io::Error::last_os_error());
            }
            // Do not drain the cancellation fd: the exact latch owns it and its
            // canceled flag is authoritative on the next iteration.
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pam_cancel::WakeFd;
    use crate::pam_helper::AnswerText;

    #[test]
    fn owned_notice_source_and_current_shared_deadline_are_not_retirement() {
        let cancel = Cancellation::default();
        let (relay, mut receiver) = channel(
            binding(),
            Policy {
                backend_phase: Duration::from_secs(30),
                total: Duration::from_secs(900),
            },
            cancel.clone(),
        )
        .unwrap();
        let source = receiver.take_notice_source().unwrap();
        assert!(receiver.take_notice_source().is_none());
        let mut event_loop = calloop::EventLoop::<usize>::try_new().unwrap();
        event_loop
            .handle()
            .insert_source(source, |_, _, seen| *seen += 1)
            .unwrap();
        let start = Instant::now();
        *relay.timing.last_input.lock().unwrap() = start - Duration::from_secs(60);
        let previous = receiver.next_deadline().unwrap();
        relay.timing.input_at(start).unwrap();
        relay.notice.signal();
        assert!(receiver.next_deadline().unwrap() > previous);
        assert_eq!(receiver.next_deadline(), Some(start + INACTIVITY));
        let mut seen = 0;
        event_loop
            .dispatch(Some(Duration::from_secs(2)), &mut seen)
            .unwrap();
        assert_eq!(seen, 1);
        event_loop
            .dispatch(Some(Duration::ZERO), &mut seen)
            .unwrap();
        assert_eq!(seen, 1);
        receiver.expire_idle_for_test();
        assert!(
            receiver.next_deadline().unwrap() <= Instant::now(),
            "expired obligation must schedule denial, not vanish"
        );
        assert!(!receiver.timely_at(Instant::now()));
        assert!(
            relay.timing.input_at(Instant::now()).is_err(),
            "late input cannot revive expired ownership"
        );
        cancel.cancel();
        assert!(!relay.timely());
    }
    fn binding() -> Binding {
        Binding {
            origin: Origin {
                connection: 7,
                request: 9,
            },
            attempt: 3,
            epoch: 5,
        }
    }
    fn fixture() -> (Relay, UnixStream, mpsc::Receiver<Input>, ResponseHandle) {
        let cancel = Cancellation::default();
        cancel
            .bind(WakeFd::new(Arc::new(AtomicBool::new(false))).unwrap())
            .unwrap();
        let (relay, _) = channel(
            binding(),
            Policy {
                backend_phase: Duration::from_secs(30),
                total: Duration::from_secs(600),
            },
            cancel.clone(),
        )
        .unwrap();
        let (read, write) = UnixStream::pair().unwrap();
        read.set_nonblocking(true).unwrap();
        write.set_nonblocking(true).unwrap();
        let (sender, receive) = mpsc::sync_channel(2);
        let handle = ResponseHandle {
            binding: binding(),
            sequence: 1,
            sender,
            wake: write,
            cancel,
            submitted: false,
            activity_pending: Arc::new(AtomicBool::new(false)),
        };
        (relay, read, receive, handle)
    }
    fn answers() -> Vec<Answer> {
        vec![
            Answer {
                index: 0,
                text: Some(AnswerText::new("fresh-password".into())),
            },
            Answer {
                index: 1,
                text: Some(AnswerText::new("fresh-pin".into())),
            },
            Answer {
                index: 2,
                text: None,
            },
            Answer {
                index: 3,
                text: None,
            },
        ]
    }
    #[test]
    fn explicit_policy_keeps_idle_phase_total_separate() {
        let policy = Policy {
            backend_phase: Duration::from_secs(30),
            total: Duration::from_secs(600),
        };
        assert!(policy.validate().is_ok());
        for invalid in [
            Policy {
                backend_phase: Duration::ZERO,
                ..policy
            },
            Policy {
                backend_phase: INACTIVITY + Duration::from_secs(1),
                ..policy
            },
            Policy {
                total: Duration::from_secs(10),
                ..policy
            },
            Policy {
                total: MAX_TOTAL + Duration::from_secs(1),
                ..policy
            },
        ] {
            assert!(invalid.validate().is_err());
        }
        let (relay, _, _, handle) = fixture();
        let remaining = relay.phase().unwrap().duration_since(Instant::now());
        assert!(remaining <= policy.backend_phase);
        assert!(remaining > Duration::from_secs(10));
        assert!(relay.timing.total_deadline.duration_since(Instant::now()) > INACTIVITY);
        drop(handle);
    }
    #[test]
    fn distinct_fresh_answers_and_activity_use_real_owned_socket_queue() {
        let (mut relay, read, receive, mut handle) = fixture();
        let activity = handle.activity_pending.clone();
        handle.note_user_input(binding(), 1).unwrap();
        handle.note_user_input(binding(), 1).unwrap(); // coalesced, reserves answer space
        handle.submit(binding(), 1, answers()).unwrap();
        let result = relay
            .wait_answers(
                &read,
                &receive,
                1,
                &[Style::Secret, Style::Visible, Style::Info, Style::Error],
                &activity,
            )
            .unwrap();
        let encoded = serde_json::to_string(&result).unwrap();
        assert!(encoded.contains("fresh-password") && encoded.contains("fresh-pin"));
        assert!(!relay.cancel.is_canceled());
        assert!(!activity.load(Ordering::Acquire));
    }
    #[test]
    fn wrong_origin_epoch_sequence_and_dropped_capability_cannot_send() {
        for (incoming, sequence) in [
            (
                Binding {
                    epoch: 6,
                    ..binding()
                },
                1,
            ),
            (
                Binding {
                    origin: Origin {
                        connection: 8,
                        request: 9,
                    },
                    ..binding()
                },
                1,
            ),
            (binding(), 2),
        ] {
            let (relay, _, receive, handle) = fixture();
            assert!(handle.submit(incoming, sequence, answers()).is_err());
            assert!(receive.try_recv().is_err());
            assert!(relay.cancel.is_canceled());
        }
        let (relay, _, _, handle) = fixture();
        drop(handle);
        assert!(relay.cancel.is_canceled());
    }
    #[test]
    fn wrong_index_and_text_for_info_cannot_reach_supervisor() {
        for result in [
            vec![Answer {
                index: 1,
                text: Some(AnswerText::new("fresh".into())),
            }],
            vec![Answer {
                index: 0,
                text: Some(AnswerText::new("not-an-ack".into())),
            }],
        ] {
            let (mut relay, read, receive, handle) = fixture();
            let activity = handle.activity_pending.clone();
            handle.submit(binding(), 1, result).unwrap();
            assert!(relay
                .wait_answers(&read, &receive, 1, &[Style::Info], &activity)
                .is_err());
        }
    }
    #[test]
    fn expired_idle_or_total_cannot_be_revived_by_queued_fresh_answer() {
        for total in [false, true] {
            let (mut relay, read, receive, handle) = fixture();
            let activity = handle.activity_pending.clone();
            handle.submit(binding(), 1, answers()).unwrap();
            if total {
                Arc::get_mut(&mut relay.timing).unwrap().total_deadline = Instant::now();
            } else {
                *relay.timing.last_input.lock().unwrap() = Instant::now() - INACTIVITY;
            }
            assert!(relay
                .wait_answers(
                    &read,
                    &receive,
                    1,
                    &[Style::Secret, Style::Visible, Style::Info, Style::Error],
                    &activity
                )
                .is_err());
        }
    }
    #[test]
    fn queued_typed_answer_resets_idle_at_input_not_later_worker_receipt() {
        let (mut relay, read, receive, mut handle) = fixture();
        let activity = handle.activity_pending.clone();
        let input_at = Instant::now() - Duration::from_secs(60);
        *relay.timing.last_input.lock().unwrap() = input_at - Duration::from_secs(10);
        assert!(handle
            .sender
            .try_send(Input::Answers(binding(), 1, answers(), input_at))
            .is_ok());
        handle.submitted = true;
        relay
            .wait_answers(
                &read,
                &receive,
                1,
                &[Style::Secret, Style::Visible, Style::Info, Style::Error],
                &activity,
            )
            .unwrap();
        assert_eq!(*relay.timing.last_input.lock().unwrap(), input_at);
    }

    #[test]
    fn info_ack_never_extends_input_idle_and_phase_never_extends_total() {
        let (mut relay, read, receive, handle) = fixture();
        let activity = handle.activity_pending.clone();
        let before = *relay.timing.last_input.lock().unwrap();
        handle
            .submit(
                binding(),
                1,
                vec![Answer {
                    index: 0,
                    text: None,
                }],
            )
            .unwrap();
        relay
            .wait_answers(&read, &receive, 1, &[Style::Info], &activity)
            .unwrap();
        assert_eq!(*relay.timing.last_input.lock().unwrap(), before);
        Arc::get_mut(&mut relay.timing).unwrap().total_deadline =
            Instant::now() + Duration::from_secs(2);
        assert!(relay.phase().unwrap() <= relay.timing.total_deadline);
    }
    // Read only this owned thread's syscall. Its two-fd poll must already be
    // blocked with no queued answer or ready wake before the negative stimulus.
    fn wait_for_user_poll(tid: libc::c_long) {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let state = std::fs::read_to_string(format!("/proc/self/task/{tid}/syscall")).unwrap();
            let fields: Vec<_> = state.split_whitespace().collect();
            let syscall = fields
                .first()
                .and_then(|field| field.parse::<libc::c_long>().ok());
            let polling = syscall == Some(libc::SYS_ppoll);
            #[cfg(target_arch = "x86_64")]
            let polling = polling || syscall == Some(libc::SYS_poll);
            if polling && fields.get(2).copied() == Some("0x2") {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "actual owned user wait never entered blocked poll"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    #[test]
    fn actual_bound_cancel_and_shutdown_fds_wake_blocked_user_wait_without_answer() {
        for shutdown in [false, true] {
            let (mut relay, read, receive, handle) = fixture();
            let activity = handle.activity_pending.clone();
            let cancel = relay.cancel.clone();
            // A fresh dedicated bound wake exposes the SAME shutdown flag that
            // production Backend::shutdown sets, without a pre-ready fd.
            let shutdown_flag = Arc::new(AtomicBool::new(false));
            let wake = WakeFd::new(shutdown_flag.clone()).unwrap();
            let cancellation = Cancellation::default();
            cancellation.bind(wake.clone()).unwrap();
            relay.cancel = cancellation.clone();
            let (send, entered) = mpsc::sync_channel(1);
            let worker = std::thread::spawn(move || {
                send.send(unsafe { libc::syscall(libc::SYS_gettid) })
                    .unwrap();
                relay
                    .wait_answers(&read, &receive, 1, &[Style::Secret], &activity)
                    .is_err()
            });
            let tid = entered.recv_timeout(Duration::from_secs(2)).unwrap();
            wait_for_user_poll(tid);
            let started = Instant::now();
            if shutdown {
                shutdown_flag.store(true, Ordering::Release);
                wake.signal().unwrap();
            } else {
                cancellation.cancel();
            }
            assert!(worker.join().unwrap());
            assert!(started.elapsed() < Duration::from_secs(1));
            assert!(cancellation.is_canceled());
            // The unrelated original handle's latch did not produce this wake.
            assert!(!cancel.is_canceled());
            drop(handle);
        }
    }
}
