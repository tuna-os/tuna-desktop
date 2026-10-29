//! Greetd unlock path (session-lock, unlock task).
//!
//! Credential entry on the lock surface is verified through the greeter
//! crate's login machinery — [`GreeterClient`] for the greetd
//! conversation and [`GreeterModel`] for the prompt state machine —
//! never through a parallel password check. There is no live daemon in
//! CI, so [`UnlockClient`] is the seam tests stub at; production passes
//! the real [`GreeterClient`], connected to `$GREETD_SOCK` by the
//! runtime. When no daemon is reachable the attempt fails closed: the
//! session stays locked and silent.
//!
//! Unlock authenticates only: on success the pending greetd
//! conversation is cancelled (never started — this session is already
//! running) and the compositor-owned flag clears, restoring the intact
//! session on the next hub poll. Failures — bad credentials, daemon
//! errors, transport drops, endless prompts — all collapse to
//! [`UnlockOutcome::Denied`], which carries no detail by construction.
//! Secrets are never logged and daemon text never surfaces.
//!
//! [`GreeterClient`]: tuna_greeter::client::GreeterClient
//! [`GreeterModel`]: tuna_greeter::model::GreeterModel

use greetd_ipc::{codec::Error as CodecError, Response};
use tuna_greeter::{
    client::GreeterClient,
    model::{GreeterModel, ModelEvent},
};

use crate::control::ControlHub;
use crate::lock::SessionLock;
use crate::overlay::Overlay;

/// Environment variable holding the live greetd socket path.
pub const GREETD_SOCKET_ENV: &str = "GREETD_SOCK";

/// Upper bound on daemon prompt rounds per unlock attempt. A daemon
/// that keeps prompting (misconfiguration or abuse) denies instead of
/// looping forever.
pub const MAX_AUTH_ROUNDS: usize = 8;

/// Outcome of one unlock attempt. `Denied` covers every failure mode
/// — wrong credentials, daemon errors, transport drops — and carries
/// no detail, so failures stay silent and leak nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnlockOutcome {
    /// The daemon authenticated the credentials.
    Unlocked,
    /// Anything else: stay locked, say nothing.
    Denied,
}

/// The client seam [`attempt_unlock`] drives. [`GreeterClient`] is the
/// production implementation; tests stub here (no live greetd in CI).
pub trait UnlockClient {
    /// Open a login attempt for `user`; the daemon's first response.
    fn create_session(&mut self, user: &str) -> Result<Response, CodecError>;
    /// Answer the last auth message (`None` acknowledges info/error
    /// messages that carry no question).
    fn answer(&mut self, text: Option<String>) -> Result<Response, CodecError>;
    /// Release a successful attempt without starting a session.
    fn cancel(&mut self) -> Result<(), CodecError>;
}

impl UnlockClient for GreeterClient {
    fn create_session(&mut self, user: &str) -> Result<Response, CodecError> {
        GreeterClient::create_session(self, user)
    }

    fn answer(&mut self, text: Option<String>) -> Result<Response, CodecError> {
        GreeterClient::answer(self, text)
    }

    fn cancel(&mut self) -> Result<(), CodecError> {
        GreeterClient::cancel(self)
    }
}

/// Live greetd socket path from the environment, if configured.
pub fn greetd_socket_path() -> Option<String> {
    std::env::var(GREETD_SOCKET_ENV)
        .ok()
        .filter(|path| !path.is_empty())
}

/// Username to authenticate as when unlocking: the session owner's
/// login name from the environment.
pub fn unlock_user() -> Option<String> {
    std::env::var("LOGNAME")
        .or_else(|_| std::env::var("USER"))
        .ok()
        .filter(|user| !user.is_empty())
}

/// Verify one entered password through the greeter login path: open a
/// greetd attempt for `user`, answer at most one ordinary Secret prompt
/// (info/error notices auto-acknowledge exactly like login), and
/// report the outcome. Success cancels the pending conversation —
/// this session is already running, so nothing starts.
///
/// Every failure collapses to [`UnlockOutcome::Denied`] with no
/// detail: wrong credentials, daemon errors, transport failures, and
/// prompt loops beyond [`MAX_AUTH_ROUNDS`].
pub fn attempt_unlock(client: &mut impl UnlockClient, user: &str, password: &str) -> UnlockOutcome {
    let mut model = GreeterModel::new();
    model.begin_user(user);
    let mut response = match client.create_session(user) {
        Ok(response) => response,
        Err(_) => return UnlockOutcome::Denied,
    };
    let mut password_used = false;
    for _ in 0..MAX_AUTH_ROUNDS {
        match model.apply_response(&response) {
            ModelEvent::NeedAnswer { prompt } => {
                if prompt.fingerprint || !prompt.secret || password_used {
                    // This adapter has no typed answer for a device challenge.
                    // Never replay a password (or acknowledge it as an answer).
                    let _ = client.cancel();
                    return UnlockOutcome::Denied;
                }
                password_used = true;
                response = match client.answer(Some(password.to_string())) {
                    Ok(response) => response,
                    Err(_) => return UnlockOutcome::Denied,
                };
            }
            ModelEvent::AutoAck => {
                response = match client.answer(None) {
                    Ok(response) => response,
                    Err(_) => return UnlockOutcome::Denied,
                };
            }
            ModelEvent::Authenticated => {
                // Authenticated only: release the pending attempt
                // without starting anything; best-effort, the daemon
                // drops it on disconnect either way.
                let _ = client.cancel();
                return UnlockOutcome::Unlocked;
            }
            ModelEvent::Failed => return UnlockOutcome::Denied,
        }
    }
    UnlockOutcome::Denied
}

/// Scans per fingerprint conversation: `pam_fprintd`'s `DEFAULT_MAX_TRIES`
/// (fprintd `pam/pam_fprintd.c`), which GDM's `gdm-fingerprint` service
/// runs for GNOME Shell 51's lock screen.
pub const MAX_FINGERPRINT_TRIES: usize = 3;
/// `pam_fprintd`'s message after a complete non-matching scan.
pub const FINGERPRINT_NO_MATCH: &str = "Failed to match fingerprint";
/// `pam_fprintd`'s message when a scan times out (it then returns
/// `PAM_AUTHINFO_UNAVAIL`, which GDM reports as an unavailable service).
pub const FINGERPRINT_TIMED_OUT: &str = "Verification timed out";
/// GDM 51 `gdm-session-worker.c` `get_max_retries_error_message` for
/// `gdm-fingerprint` after `PAM_MAXTRIES`.
pub const FINGERPRINT_MAX_TRIES: &str =
    "You reached the maximum fingerprint authentication attempts, please try another method";

/// One step of a fingerprint conversation the lock screen shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FingerprintStep {
    /// The reader waits for a finger (`swipe`: a swipe sensor).
    Scan { swipe: bool },
    /// `pam_fprintd`'s informational message.
    Info(&'static str),
    /// `pam_fprintd`'s (or GDM's) error message.
    Problem(&'static str),
}

/// How a fingerprint conversation ended. Only [`Self::Matched`] may unlock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FingerprintEnd {
    Matched,
    /// Every scan failed to match (`PAM_MAXTRIES`).
    Failed,
    /// No daemon, reader or enrolled print, a timed-out scan or a daemon
    /// error (`PAM_AUTHINFO_UNAVAIL`): GDM's service-unavailable.
    Unavailable,
    Cancelled,
}

/// The fingerprint seam [`run_fingerprint`] drives. [`crate::fprintd::Fprintd`]
/// is the production implementation; tests stub here (no reader on CI).
pub trait FprintVerifier {
    /// A reader is present and `user` enrolled a print (pam_fprintd's
    /// `user_has_prints`; GNOME offers fingerprint only then).
    fn offered(&mut self, user: &str) -> bool;
    /// One bounded, cancellable scan.
    fn verify(
        &mut self,
        user: &str,
        cancel: &std::sync::atomic::AtomicBool,
        progress: crate::fprintd::ProgressSink,
    ) -> crate::fprintd::VerifyOutcome;
}

impl FprintVerifier for crate::fprintd::Fprintd {
    fn offered(&mut self, user: &str) -> bool {
        self.capability(user).offered()
    }

    fn verify(
        &mut self,
        user: &str,
        cancel: &std::sync::atomic::AtomicBool,
        progress: crate::fprintd::ProgressSink,
    ) -> crate::fprintd::VerifyOutcome {
        // A cancelled previous attempt's reaper may still be retiring its
        // claim; wait for it (bounded) rather than fail on contention.
        let until = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !crate::fprintd::claim_idle() && std::time::Instant::now() < until {
            if cancel.load(std::sync::atomic::Ordering::Acquire) {
                return crate::fprintd::VerifyOutcome::Cancelled;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        self.verify_interactive(user, crate::fprintd::VERIFY_TIMEOUT, cancel, progress)
    }
}

/// One fingerprint conversation for `user`, as `pam_fprintd`'s `do_verify`
/// runs it: up to [`MAX_FINGERPRINT_TRIES`] scans, a no-match message after
/// each failed one, the max-tries message at the end; a timed-out scan or
/// any daemon failure ends it as unavailable. Only a daemon-reported match
/// ends it [`FingerprintEnd::Matched`]. Blocking: runs on a worker thread.
pub fn run_fingerprint(
    verifier: &mut impl FprintVerifier,
    user: &str,
    cancel: &std::sync::atomic::AtomicBool,
    step: std::sync::Arc<dyn Fn(FingerprintStep) + Send + Sync>,
) -> FingerprintEnd {
    use crate::fprintd::{VerifyOutcome, VerifyProgress};
    if !verifier.offered(user) {
        return FingerprintEnd::Unavailable;
    }
    for _ in 0..MAX_FINGERPRINT_TRIES {
        if cancel.load(std::sync::atomic::Ordering::Acquire) {
            return FingerprintEnd::Cancelled;
        }
        let sink = step.clone();
        let progress: crate::fprintd::ProgressSink = Box::new(move |progress| {
            sink(match progress {
                VerifyProgress::Armed { swipe } => FingerprintStep::Scan { swipe },
                VerifyProgress::Retry(message) => FingerprintStep::Problem(message),
            })
        });
        match verifier.verify(user, cancel, progress) {
            VerifyOutcome::Matched => return FingerprintEnd::Matched,
            VerifyOutcome::NoMatch => step(FingerprintStep::Problem(FINGERPRINT_NO_MATCH)),
            VerifyOutcome::TimedOut => {
                step(FingerprintStep::Info(FINGERPRINT_TIMED_OUT));
                return FingerprintEnd::Unavailable;
            }
            VerifyOutcome::Cancelled => return FingerprintEnd::Cancelled,
            VerifyOutcome::NoDevice | VerifyOutcome::NotEnrolled | VerifyOutcome::Error => {
                return FingerprintEnd::Unavailable
            }
        }
    }
    step(FingerprintStep::Problem(FINGERPRINT_MAX_TRIES));
    FingerprintEnd::Failed
}

/// Verify `password` for `user` the way [`unlock_session`] does, without
/// touching any lock state: greetd's daemon when one runs, else PAM's
/// `tuna-lock` service (GDM sessions). Blocking (PAM may delay a
/// failure for seconds), so the runtime calls it on a worker thread.
pub(crate) fn verify_greetd(path: &str, user: &str, password: &str) -> bool {
    let Ok(mut client) = GreeterClient::connect(path) else {
        return false;
    };
    matches!(
        attempt_unlock(&mut client, user, password),
        UnlockOutcome::Unlocked
    )
}

pub fn verify(user: &str, password: &str) -> bool {
    if let Some(path) = greetd_socket_path() {
        return verify_greetd(&path, user, password);
    }
    crate::pam::authenticate_with_deadline(&crate::pam::service(), user, password)
}

/// Off-loop ownership path. PAM timeout emits denial while the SAME outer
/// wrapper waits for actual helper retirement; poison is never a retirement.
/// Configured greetd remains authoritative and never falls back to PAM.
pub fn verify_owned(
    user: &str,
    password: tuna_shell_control::Secret,
    cancel: crate::pam_cancel::Cancellation,
    pending: impl FnMut(),
) -> crate::pam_backend::Verification {
    verify_owned_credential(
        user,
        crate::pam_backend::Credential::from_schema(password),
        cancel,
        pending,
    )
}

pub(crate) fn verify_owned_credential(
    user: &str,
    password: crate::pam_backend::Credential,
    cancel: crate::pam_cancel::Cancellation,
    pending: impl FnMut(),
) -> crate::pam_backend::Verification {
    use crate::pam_backend::Verification;
    if let Some(path) = greetd_socket_path() {
        return Verification::Retired {
            authenticated: verify_greetd(&path, user, password.as_str()),
        };
    }
    let Some(backend) = crate::pam_backend::Backend::live() else {
        return Verification::RejectedBeforeSpawn;
    };
    match backend.begin_credential(crate::pam::service(), user.to_owned(), password, cancel) {
        Ok(ticket) => ticket.wait_retired(pending),
        Err(result) => result,
    }
}

/// The session's user name, for unlocking.
pub fn session_user() -> Option<String> {
    // SAFETY: getpwuid returns a pointer into static storage, read at once.
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if pw.is_null() {
            return None;
        }
        std::ffi::CStr::from_ptr((*pw).pw_name)
            .to_str()
            .ok()
            .map(str::to_owned)
    }
}

/// Apply one unlock attempt to the compositor-owned lock state: verify
/// `password` for `user` through `client`, and on success clear the
/// idle flag plus the hub mirror (the next poll restores the intact
/// session) and drop the lock surface. Returns whether the session is
/// unlocked afterwards.
///
/// An already-unlocked session returns true without contacting the
/// daemon — nothing is protected, so no credential is needed.
/// Failures touch nothing: the flag, the hub mirror, and the surface
/// stay exactly as they were, silently.
pub fn unlock_session(
    lock: &mut SessionLock,
    hub: &ControlHub,
    overlay: &mut Overlay,
    now_ms: u64,
    user: &str,
    password: &str,
    client: &mut impl UnlockClient,
) -> bool {
    if !lock.is_locked() && !hub.is_locked() {
        return true;
    }
    if matches!(
        attempt_unlock(client, user, password),
        UnlockOutcome::Unlocked
    ) {
        // Restart the idle accumulator at now so the session does not
        // relock on the next tick; the hub poll carries the restore.
        lock.unlock(now_ms);
        hub.set_locked(false);
        overlay.hide();
        true
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use greetd_ipc::{AuthMessageType, ErrorType};
    use std::collections::VecDeque;

    fn secret_prompt(text: &str) -> Response {
        Response::AuthMessage {
            auth_message_type: AuthMessageType::Secret,
            auth_message: text.to_string(),
        }
    }

    fn info_notice(text: &str) -> Response {
        Response::AuthMessage {
            auth_message_type: AuthMessageType::Info,
            auth_message: text.to_string(),
        }
    }

    fn auth_error() -> Response {
        Response::Error {
            error_type: ErrorType::AuthError,
            description: "PAM authentication failed".to_string(),
        }
    }

    /// Scripted seam stub: canned replies for `create_session` then
    /// each `answer`, recording every answer verbatim.
    struct ScriptStub {
        replies: VecDeque<Result<Response, CodecError>>,
        answers: Vec<Option<String>>,
        cancelled: bool,
    }

    impl ScriptStub {
        fn new(replies: Vec<Result<Response, CodecError>>) -> Self {
            Self {
                replies: replies.into(),
                answers: Vec::new(),
                cancelled: false,
            }
        }

        fn next_reply(&mut self) -> Result<Response, CodecError> {
            self.replies.pop_front().unwrap_or(Err(CodecError::Eof))
        }
    }

    impl UnlockClient for ScriptStub {
        fn create_session(&mut self, _user: &str) -> Result<Response, CodecError> {
            self.next_reply()
        }

        fn answer(&mut self, text: Option<String>) -> Result<Response, CodecError> {
            self.answers.push(text);
            self.next_reply()
        }

        fn cancel(&mut self) -> Result<(), CodecError> {
            self.cancelled = true;
            Ok(())
        }
    }

    #[test]
    fn fingerprint_questions_never_receive_password_and_cancel_the_attempt() {
        for style in [AuthMessageType::Secret, AuthMessageType::Visible] {
            let mut stub = ScriptStub::new(vec![
                Ok(Response::AuthMessage {
                    auth_message_type: style,
                    auth_message: "Place your finger on the fingerprint reader".into(),
                }),
                Ok(Response::Success),
            ]);
            assert_eq!(
                attempt_unlock(&mut stub, "ada", "owned-fixture-secret"),
                UnlockOutcome::Denied
            );
            assert!(stub.answers.is_empty());
            assert!(stub.cancelled);
        }
    }
    #[test]
    fn password_then_fingerprint_does_not_replay_password_into_second_question() {
        let mut stub = ScriptStub::new(vec![
            Ok(secret_prompt("Password:")),
            Ok(secret_prompt("Swipe your finger again")),
            Ok(Response::Success),
        ]);
        assert_eq!(
            attempt_unlock(&mut stub, "ada", "owned-fixture-secret"),
            UnlockOutcome::Denied
        );
        assert_eq!(stub.answers, [Some("owned-fixture-secret".into())]);
        assert!(stub.cancelled);
    }
    #[test]
    fn fingerprint_info_remains_empty_ack_then_normal_password_succeeds() {
        let mut stub = ScriptStub::new(vec![
            Ok(info_notice("Place your finger on the fingerprint reader")),
            Ok(secret_prompt("Password:")),
            Ok(Response::Success),
        ]);
        assert_eq!(
            attempt_unlock(&mut stub, "ada", "owned-fixture-secret"),
            UnlockOutcome::Unlocked
        );
        assert_eq!(stub.answers, [None, Some("owned-fixture-secret".into())]);
        assert!(stub.cancelled);
    }

    #[test]
    fn correct_password_unlocks_and_releases_the_attempt() {
        let mut stub = ScriptStub::new(vec![Ok(secret_prompt("Password:")), Ok(Response::Success)]);
        assert_eq!(
            attempt_unlock(&mut stub, "ada", "s3cret"),
            UnlockOutcome::Unlocked
        );
        // The entered credential is what the conversation carried.
        assert_eq!(stub.answers, vec![Some("s3cret".to_string())]);
        // Success never starts a session: the pending attempt is out.
        assert!(stub.cancelled);
    }

    #[test]
    fn wrong_password_denies_without_release() {
        let mut stub = ScriptStub::new(vec![Ok(secret_prompt("Password:")), Ok(auth_error())]);
        assert_eq!(
            attempt_unlock(&mut stub, "mallory", "wrong"),
            UnlockOutcome::Denied
        );
        assert!(!stub.cancelled);
    }

    #[test]
    fn daemon_unreachable_denies_silently() {
        let mut stub = ScriptStub::new(vec![Err(CodecError::Eof)]);
        assert_eq!(
            attempt_unlock(&mut stub, "ada", "s3cret"),
            UnlockOutcome::Denied
        );
        assert!(stub.answers.is_empty());
    }

    #[test]
    fn mid_conversation_drop_denies() {
        let mut stub = ScriptStub::new(vec![Ok(secret_prompt("Password:")), Err(CodecError::Eof)]);
        assert_eq!(
            attempt_unlock(&mut stub, "ada", "s3cret"),
            UnlockOutcome::Denied
        );
    }

    #[test]
    fn distinct_pin_or_visible_prompt_never_receives_reused_password() {
        for second in [
            secret_prompt("Separate PIN:"),
            Response::AuthMessage {
                auth_message_type: AuthMessageType::Visible,
                auth_message: "Visible second challenge".into(),
            },
        ] {
            let mut stub = ScriptStub::new(vec![
                Ok(secret_prompt("Password:")),
                Ok(second),
                Ok(Response::Success),
            ]);
            assert_eq!(
                attempt_unlock(&mut stub, "ada", "owned-first-answer"),
                UnlockOutcome::Denied
            );
            assert_eq!(stub.answers, [Some("owned-first-answer".into())]);
            assert!(stub.cancelled);
        }
        let mut stub = ScriptStub::new(vec![Ok(Response::AuthMessage {
            auth_message_type: AuthMessageType::Visible,
            auth_message: "PIN:".into(),
        })]);
        assert_eq!(
            attempt_unlock(&mut stub, "ada", "owned-first-answer"),
            UnlockOutcome::Denied
        );
        assert!(stub.answers.is_empty());
        assert!(stub.cancelled);
    }

    #[test]
    fn endless_prompts_deny_bounded() {
        let mut stub = ScriptStub::new(
            (0..MAX_AUTH_ROUNDS + 4)
                .map(|_| Ok(secret_prompt("Password:")))
                .collect(),
        );
        assert_eq!(
            attempt_unlock(&mut stub, "ada", "s3cret"),
            UnlockOutcome::Denied
        );
        assert_eq!(stub.answers.len(), 1);
    }

    #[test]
    fn info_notice_auto_acknowledges_like_login() {
        let mut stub = ScriptStub::new(vec![Ok(info_notice("Hello.")), Ok(Response::Success)]);
        assert_eq!(
            attempt_unlock(&mut stub, "ada", "s3cret"),
            UnlockOutcome::Unlocked
        );
        // Info carries no question: acknowledged empty, unprompted.
        assert_eq!(stub.answers, vec![None]);
    }

    /// Scripted fingerprint stub: canned scans, each optionally reporting
    /// progress first, recording the user of every scan.
    struct FprintStub {
        offered: bool,
        scans: VecDeque<(
            Vec<crate::fprintd::VerifyProgress>,
            crate::fprintd::VerifyOutcome,
        )>,
        users: Vec<String>,
    }

    impl FprintStub {
        fn sequenced(outcomes: Vec<crate::fprintd::VerifyOutcome>) -> Self {
            Self {
                offered: true,
                scans: outcomes.into_iter().map(|o| (Vec::new(), o)).collect(),
                users: Vec::new(),
            }
        }
    }

    impl FprintVerifier for FprintStub {
        fn offered(&mut self, _user: &str) -> bool {
            self.offered
        }
        fn verify(
            &mut self,
            user: &str,
            _cancel: &std::sync::atomic::AtomicBool,
            progress: crate::fprintd::ProgressSink,
        ) -> crate::fprintd::VerifyOutcome {
            self.users.push(user.to_string());
            let (steps, outcome) = self
                .scans
                .pop_front()
                .unwrap_or((Vec::new(), crate::fprintd::VerifyOutcome::Error));
            for step in steps {
                progress(step);
            }
            outcome
        }
    }

    fn run(stub: &mut FprintStub, cancel: bool) -> (FingerprintEnd, Vec<FingerprintStep>) {
        let steps = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = steps.clone();
        let end = run_fingerprint(
            stub,
            "ada",
            &std::sync::atomic::AtomicBool::new(cancel),
            std::sync::Arc::new(move |step| sink.lock().unwrap().push(step)),
        );
        let steps = steps.lock().unwrap().clone();
        (end, steps)
    }

    #[test]
    fn fingerprint_match_ends_matched_for_that_user_with_the_scan_cue() {
        use crate::fprintd::{VerifyOutcome::Matched, VerifyProgress::Armed};
        let mut stub = FprintStub::sequenced(Vec::new());
        stub.scans.push_back((vec![Armed { swipe: true }], Matched));
        let (end, steps) = run(&mut stub, false);
        assert_eq!(end, FingerprintEnd::Matched);
        assert_eq!(steps, [FingerprintStep::Scan { swipe: true }]);
        assert_eq!(stub.users, ["ada"]);
    }

    #[test]
    fn three_failed_scans_end_failed_like_pam_fprintd_max_tries() {
        use crate::fprintd::VerifyOutcome::{Matched, NoMatch};
        let mut stub = FprintStub::sequenced(vec![NoMatch, NoMatch, NoMatch, Matched]);
        let (end, steps) = run(&mut stub, false);
        assert_eq!(end, FingerprintEnd::Failed);
        assert_eq!(
            steps,
            [
                FingerprintStep::Problem(FINGERPRINT_NO_MATCH),
                FingerprintStep::Problem(FINGERPRINT_NO_MATCH),
                FingerprintStep::Problem(FINGERPRINT_NO_MATCH),
                FingerprintStep::Problem(FINGERPRINT_MAX_TRIES),
            ]
        );
        // The fourth scripted match is never reached: three scans only.
        assert_eq!(stub.users.len(), MAX_FINGERPRINT_TRIES);
    }

    #[test]
    fn a_retry_then_match_reports_pam_fprintd_messages() {
        use crate::fprintd::{VerifyOutcome::*, VerifyProgress::*};
        let mut stub = FprintStub::sequenced(Vec::new());
        stub.scans.push_back((
            vec![
                Armed { swipe: false },
                Retry("Place your finger on the reader again"),
            ],
            NoMatch,
        ));
        stub.scans
            .push_back((vec![Armed { swipe: false }], Matched));
        let (end, steps) = run(&mut stub, false);
        assert_eq!(end, FingerprintEnd::Matched);
        assert_eq!(
            steps,
            [
                FingerprintStep::Scan { swipe: false },
                FingerprintStep::Problem("Place your finger on the reader again"),
                FingerprintStep::Problem(FINGERPRINT_NO_MATCH),
                FingerprintStep::Scan { swipe: false },
            ]
        );
    }

    #[test]
    fn timeouts_and_daemon_failures_end_unavailable_never_matched() {
        use crate::fprintd::VerifyOutcome::*;
        let mut stub = FprintStub::sequenced(vec![TimedOut, Matched]);
        let (end, steps) = run(&mut stub, false);
        assert_eq!(end, FingerprintEnd::Unavailable);
        assert_eq!(steps, [FingerprintStep::Info(FINGERPRINT_TIMED_OUT)]);
        assert_eq!(stub.users.len(), 1, "a timed-out conversation ends");
        for outcome in [NoDevice, NotEnrolled, Error] {
            let mut stub = FprintStub::sequenced(vec![outcome, Matched]);
            let (end, steps) = run(&mut stub, false);
            assert_eq!(end, FingerprintEnd::Unavailable, "{outcome:?}");
            assert!(steps.is_empty());
        }
        let mut stub = FprintStub::sequenced(vec![Cancelled, Matched]);
        assert_eq!(run(&mut stub, false).0, FingerprintEnd::Cancelled);
    }

    #[test]
    fn no_reader_or_prints_and_prior_cancel_never_scan() {
        let mut stub = FprintStub::sequenced(vec![crate::fprintd::VerifyOutcome::Matched]);
        stub.offered = false;
        assert_eq!(run(&mut stub, false).0, FingerprintEnd::Unavailable);
        assert!(stub.users.is_empty());
        let mut stub = FprintStub::sequenced(vec![crate::fprintd::VerifyOutcome::Matched]);
        assert_eq!(run(&mut stub, true).0, FingerprintEnd::Cancelled);
        assert!(stub.users.is_empty());
    }

    #[test]
    fn retry_statuses_use_pam_fprintd_wording_by_scan_type() {
        use crate::fprintd::retry_message;
        assert_eq!(
            retry_message("verify-retry-scan", false),
            Some("Place your finger on the reader again")
        );
        assert_eq!(
            retry_message("verify-retry-scan", true),
            Some("Swipe your finger again")
        );
        assert_eq!(
            retry_message("verify-swipe-too-short", true),
            Some("Swipe was too short, try again")
        );
        assert_eq!(retry_message("verify-match", false), None);
        assert_eq!(retry_message("verify-no-match", false), None);
    }

    #[test]
    fn unlock_user_reads_session_owner() {
        let outer_logname = std::env::var_os("LOGNAME");
        let outer_user = std::env::var_os("USER");
        unsafe {
            std::env::set_var("LOGNAME", "ada");
            std::env::remove_var("USER");
        }
        assert_eq!(unlock_user().as_deref(), Some("ada"));
        unsafe {
            std::env::remove_var("LOGNAME");
            std::env::set_var("USER", "grace");
        }
        assert_eq!(unlock_user().as_deref(), Some("grace"));
        unsafe {
            std::env::remove_var("LOGNAME");
            std::env::remove_var("USER");
        }
        assert_eq!(unlock_user(), None);
        unsafe {
            match outer_logname {
                Some(value) => std::env::set_var("LOGNAME", value),
                None => std::env::remove_var("LOGNAME"),
            }
            match outer_user {
                Some(value) => std::env::set_var("USER", value),
                None => std::env::remove_var("USER"),
            }
        }
    }
}
