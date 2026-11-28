//! Lock-screen unlock path (session-lock, unlock task).
//!
//! Credential entry on the lock surface is verified by PAM's `tuna-lock`
//! service (see [`crate::pam`]), never by a parallel password check.
//! [`UnlockClient`] is the conversation seam: [`crate::pam::PamClient`]
//! is the PAM implementation and tests stub here. When the authenticator
//! is unreachable the attempt fails closed: the session stays locked and
//! silent.
//!
//! Unlock authenticates only: on success the pending conversation is
//! cancelled (nothing starts — this session is already running). Nothing
//! here touches lock state: the runtime alone clears the lock, through
//! `Runtime::apply_verified_unlock` and its epoch-checked grant. Failures
//! — bad credentials, authenticator errors, transport drops, endless
//! prompts — all collapse to [`UnlockOutcome::Denied`], which carries no
//! detail by construction. Secrets are never logged and authenticator
//! text never surfaces.

/// Upper bound on authenticator prompt rounds per unlock attempt. An
/// authenticator that keeps prompting (misconfiguration or abuse) denies
/// instead of looping forever.
pub const MAX_AUTH_ROUNDS: usize = 8;

/// Outcome of one unlock attempt. `Denied` covers every failure mode
/// — wrong credentials, authenticator errors, transport drops — and
/// carries no detail, so failures stay silent and leak nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnlockOutcome {
    /// The authenticator accepted the credentials.
    Unlocked,
    /// Anything else: stay locked, say nothing.
    Denied,
}

/// One authenticator reply in an unlock conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthReply {
    /// A question; `secret` masks the answer (PAM's echo-off prompt).
    Prompt { text: String, secret: bool },
    /// An informational or error notice that asks nothing: acknowledged
    /// with an empty answer.
    Notice,
    /// The credentials were accepted.
    Accepted,
    /// The credentials were refused. Never carries the authenticator's
    /// detail, which could distinguish bad user from bad secret.
    Refused,
}

/// The authenticator could not be reached or the conversation broke.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unreachable;

/// The conversation seam [`attempt_unlock`] drives.
/// [`crate::pam::PamClient`] is the production implementation; tests
/// stub here.
pub trait UnlockClient {
    /// Open an authentication attempt for `user`; the first reply.
    fn begin(&mut self, user: &str) -> Result<AuthReply, Unreachable>;
    /// Answer the last reply (`None` acknowledges a notice that carries
    /// no question).
    fn answer(&mut self, text: Option<String>) -> Result<AuthReply, Unreachable>;
    /// Release the attempt without starting anything.
    fn cancel(&mut self);
}

/// Username to authenticate as when unlocking: the session owner's
/// login name from the environment.
pub fn unlock_user() -> Option<String> {
    std::env::var("LOGNAME")
        .or_else(|_| std::env::var("USER"))
        .ok()
        .filter(|user| !user.is_empty())
}

/// Verify one entered password: open an attempt for `user`, answer at
/// most one ordinary secret prompt with the password (notices
/// auto-acknowledge), and report the outcome. Success cancels the
/// pending conversation — this session is already running, so nothing
/// starts.
///
/// Every failure collapses to [`UnlockOutcome::Denied`] with no
/// detail: wrong credentials, authenticator errors, transport failures,
/// and prompt loops beyond [`MAX_AUTH_ROUNDS`]. A visible prompt, a
/// fingerprint prompt, or a second secret prompt cancels the attempt:
/// the password is never replayed into a distinct challenge.
pub fn attempt_unlock(client: &mut impl UnlockClient, user: &str, password: &str) -> UnlockOutcome {
    let Ok(mut reply) = client.begin(user) else {
        return UnlockOutcome::Denied;
    };
    let mut password_used = false;
    for _ in 0..MAX_AUTH_ROUNDS {
        let answer = match reply {
            AuthReply::Prompt { text, secret } => {
                if !secret || password_used || crate::pam::is_fingerprint_prompt(&text) {
                    // No typed answer exists for a device or distinct
                    // challenge. Never replay the password into it.
                    client.cancel();
                    return UnlockOutcome::Denied;
                }
                password_used = true;
                Some(password.to_string())
            }
            AuthReply::Notice => None,
            AuthReply::Accepted => {
                // Authenticated only: release the pending attempt
                // without starting anything.
                client.cancel();
                return UnlockOutcome::Unlocked;
            }
            AuthReply::Refused => return UnlockOutcome::Denied,
        };
        reply = match client.answer(answer) {
            Ok(reply) => reply,
            Err(Unreachable) => return UnlockOutcome::Denied,
        };
    }
    UnlockOutcome::Denied
}

/// Verify `password` for `user` against PAM's `tuna-lock` service without
/// touching any lock state. Blocking (PAM may delay a failure for
/// seconds), so the runtime calls it on a worker thread.
pub fn verify(user: &str, password: &str) -> bool {
    crate::pam::authenticate_with_deadline(&crate::pam::service(), user, password)
}

/// Off-loop ownership path. PAM timeout emits denial while the SAME outer
/// wrapper waits for actual helper retirement; poison is never a retirement.
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
    let Some(backend) = crate::pam_backend::Backend::live() else {
        return crate::pam_backend::Verification::RejectedBeforeSpawn;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    fn secret_prompt(text: &str) -> AuthReply {
        AuthReply::Prompt {
            text: text.to_string(),
            secret: true,
        }
    }

    fn visible_prompt(text: &str) -> AuthReply {
        AuthReply::Prompt {
            text: text.to_string(),
            secret: false,
        }
    }

    /// Scripted seam stub: canned replies for `begin` then each
    /// `answer`, recording every answer verbatim.
    struct ScriptStub {
        replies: VecDeque<Result<AuthReply, Unreachable>>,
        answers: Vec<Option<String>>,
        cancelled: bool,
    }

    impl ScriptStub {
        fn new(replies: Vec<Result<AuthReply, Unreachable>>) -> Self {
            Self {
                replies: replies.into(),
                answers: Vec::new(),
                cancelled: false,
            }
        }

        fn next_reply(&mut self) -> Result<AuthReply, Unreachable> {
            self.replies.pop_front().unwrap_or(Err(Unreachable))
        }
    }

    impl UnlockClient for ScriptStub {
        fn begin(&mut self, _user: &str) -> Result<AuthReply, Unreachable> {
            self.next_reply()
        }

        fn answer(&mut self, text: Option<String>) -> Result<AuthReply, Unreachable> {
            self.answers.push(text);
            self.next_reply()
        }

        fn cancel(&mut self) {
            self.cancelled = true;
        }
    }

    #[test]
    fn fingerprint_questions_never_receive_password_and_cancel_the_attempt() {
        let text = "Place your finger on the fingerprint reader";
        for prompt in [secret_prompt(text), visible_prompt(text)] {
            let mut stub = ScriptStub::new(vec![Ok(prompt), Ok(AuthReply::Accepted)]);
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
            Ok(AuthReply::Accepted),
        ]);
        assert_eq!(
            attempt_unlock(&mut stub, "ada", "owned-fixture-secret"),
            UnlockOutcome::Denied
        );
        assert_eq!(stub.answers, [Some("owned-fixture-secret".into())]);
        assert!(stub.cancelled);
    }
    #[test]
    fn fingerprint_notice_remains_empty_ack_then_normal_password_succeeds() {
        let mut stub = ScriptStub::new(vec![
            Ok(AuthReply::Notice),
            Ok(secret_prompt("Password:")),
            Ok(AuthReply::Accepted),
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
        let mut stub = ScriptStub::new(vec![
            Ok(secret_prompt("Password:")),
            Ok(AuthReply::Accepted),
        ]);
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
        let mut stub =
            ScriptStub::new(vec![Ok(secret_prompt("Password:")), Ok(AuthReply::Refused)]);
        assert_eq!(
            attempt_unlock(&mut stub, "mallory", "wrong"),
            UnlockOutcome::Denied
        );
        assert!(!stub.cancelled);
    }

    #[test]
    fn authenticator_unreachable_denies_silently() {
        let mut stub = ScriptStub::new(vec![Err(Unreachable)]);
        assert_eq!(
            attempt_unlock(&mut stub, "ada", "s3cret"),
            UnlockOutcome::Denied
        );
        assert!(stub.answers.is_empty());
    }

    #[test]
    fn mid_conversation_drop_denies() {
        let mut stub = ScriptStub::new(vec![Ok(secret_prompt("Password:")), Err(Unreachable)]);
        assert_eq!(
            attempt_unlock(&mut stub, "ada", "s3cret"),
            UnlockOutcome::Denied
        );
    }

    #[test]
    fn distinct_pin_or_visible_prompt_never_receives_reused_password() {
        for second in [
            secret_prompt("Separate PIN:"),
            visible_prompt("Visible second challenge"),
        ] {
            let mut stub = ScriptStub::new(vec![
                Ok(secret_prompt("Password:")),
                Ok(second),
                Ok(AuthReply::Accepted),
            ]);
            assert_eq!(
                attempt_unlock(&mut stub, "ada", "owned-first-answer"),
                UnlockOutcome::Denied
            );
            assert_eq!(stub.answers, [Some("owned-first-answer".into())]);
            assert!(stub.cancelled);
        }
        let mut stub = ScriptStub::new(vec![Ok(visible_prompt("PIN:"))]);
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
    fn endless_notices_deny_bounded() {
        let mut stub = ScriptStub::new(
            (0..MAX_AUTH_ROUNDS + 4)
                .map(|_| Ok(AuthReply::Notice))
                .collect(),
        );
        assert_eq!(
            attempt_unlock(&mut stub, "ada", "s3cret"),
            UnlockOutcome::Denied
        );
        assert_eq!(stub.answers.len(), MAX_AUTH_ROUNDS);
        assert!(stub.answers.iter().all(Option::is_none));
    }

    #[test]
    fn notice_auto_acknowledges_empty() {
        let mut stub = ScriptStub::new(vec![Ok(AuthReply::Notice), Ok(AuthReply::Accepted)]);
        assert_eq!(
            attempt_unlock(&mut stub, "ada", "s3cret"),
            UnlockOutcome::Unlocked
        );
        // A notice carries no question: acknowledged empty, unprompted.
        assert_eq!(stub.answers, vec![None]);
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
