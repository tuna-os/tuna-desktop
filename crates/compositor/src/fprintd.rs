//! Fingerprint unlock through fprintd (#366, P-LK-02 fingerprint subset).
//!
//! When a reader is present and the session user has enrolled prints,
//! the lock screen may offer a fingerprint attempt alongside the
//! password. This module is the only place that speaks to fprintd's
//! D-Bus API; everything else goes through [`Capability`] (truthful
//! availability after enrollment) and [`Fprintd::verify`] (one bounded
//! attempt). Smartcard/PIN, multi-step and web login stay queued for
//! Wave 3; enrollment UI lives in Settings, not here.
//!
//! D-Bus shapes follow the fprintd reference manual
//! (`net.reactivated.Fprint.Manager` / `.Device` on the system bus):
//! `GetDevices`/`GetDefaultDevice`, `Claim(username)` + `Release`,
//! `VerifyStart("any")` + `VerifyStop`, `ListEnrolledFingers`, and the
//! `VerifyStatus(result, done)` signal whose `result` is one of
//! `verify-match`, `verify-no-match`, the `verify-retry-*` reasons,
//! `verify-unknown-error` or `verify-disconnected`. Prompt wording
//! matches fprintd's `pam/fingerprint-strings.h` English source
//! strings (see [`tuna_greeter::model::is_fingerprint_prompt`]).
//!
//! Failure rules: no daemon, no device, no enrolled prints, a denied
//! PolicyKit check, a dropped connection or a timeout all fail closed
//! ([`VerifyOutcome::Error`] and friends — never unlock), carry no
//! detail, and log nothing. Fingerprints never leave the daemon; this
//! client only learns match/no-match.
use std::sync::mpsc;
use std::time::Duration;

/// fprintd's well-known name on the system bus.
pub const BUS_NAME: &str = "net.reactivated.Fprint";
/// Object path of the manager interface.
pub const MANAGER_PATH: &str = "/net/reactivated/Fprint/Manager";
/// `GetDevices` / `GetDefaultDevice`.
pub const MANAGER_IFACE: &str = "net.reactivated.Fprint.Manager";
/// Claim/verify/enrolled-fingers plus `VerifyStatus`.
pub const DEVICE_IFACE: &str = "net.reactivated.Fprint.Device";
/// Finger name selecting any enrolled print for `VerifyStart`.
pub const VERIFY_ANY: &str = "any";
/// Setting this variable (any value) disables fingerprint auth: every
/// probe reports unavailable and every attempt fails closed. Tests and
/// hardened images use it to pin the no-reader path.
pub const DISABLE_ENV: &str = "TUNA_FPRINTD_OFF";
/// Default bound on one verification (finger press included).
pub const VERIFY_TIMEOUT: Duration = Duration::from_secs(30);
/// Bound on intermediate retry signals per attempt. A daemon that
/// keeps asking for another swipe denies instead of looping forever.
pub const MAX_VERIFY_RETRIES: usize = 10;

/// Outcome of one fingerprint verification. Only [`Self::Matched`]
/// unlocks; every other variant keeps the session locked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyOutcome {
    /// The pressed finger matched an enrolled print.
    Matched,
    /// A complete press matched none of the enrolled prints.
    NoMatch,
    /// fprintd is disabled, unreachable, or has no reader.
    NoDevice,
    /// The reader works but this user enrolled no prints.
    NotEnrolled,
    /// The attempt was cancelled.
    Cancelled,
    /// No terminal signal arrived within the deadline.
    TimedOut,
    /// Claim denied, daemon error, disconnect, or malformed reply.
    Error,
}

/// Truthful fingerprint capability for one user, after checking the
/// daemon and that user's enrollment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capability {
    /// A reader answered on the system bus.
    pub available: bool,
    /// Enrolled finger names (`ListEnrolledFingers`), empty when none.
    pub enrolled_fingers: Vec<String>,
}

impl Capability {
    /// The fail-closed capability: nothing offered.
    pub fn unavailable() -> Self {
        Self {
            available: false,
            enrolled_fingers: Vec::new(),
        }
    }

    /// Whether unlock may offer a fingerprint attempt: a reader is
    /// present *and* this user enrolled at least one print.
    pub fn offered(&self) -> bool {
        self.available && !self.enrolled_fingers.is_empty()
    }
}

/// One `VerifyStatus(result, done)` signal, classified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifySignal {
    /// Keep waiting (a retry reason with `done == false`).
    Continue,
    /// Terminal: the attempt ends with this outcome.
    Done(VerifyOutcome),
}

/// Classify one `VerifyStatus` signal body. Terminal results end the
/// attempt; retry reasons (`verify-retry-scan`, `verify-swipe-too-short`,
/// `verify-finger-not-centered`, `verify-remove-and-retry`) continue it.
/// Unknown strings fail closed when terminal, else continue bounded.
pub fn classify_verify_status(result: &str, done: bool) -> VerifySignal {
    match result {
        "verify-match" => VerifySignal::Done(VerifyOutcome::Matched),
        "verify-no-match" if done => VerifySignal::Done(VerifyOutcome::NoMatch),
        "verify-unknown-error" | "verify-disconnected" => VerifySignal::Done(VerifyOutcome::Error),
        "verify-retry-scan"
        | "verify-swipe-too-short"
        | "verify-finger-not-centered"
        | "verify-remove-and-retry"
        | "verify-no-match" => VerifySignal::Continue,
        _ if done => VerifySignal::Done(VerifyOutcome::Error),
        _ => VerifySignal::Continue,
    }
}

/// The username argument fprintd methods should receive. fprintd
/// recommends the empty string for the client's own user (it skips the
/// `setusername` PolicyKit check, which only root passes for anyone
/// else); an explicit name is kept only when it differs from the
/// process user, and then the daemon decides (fail closed on denial).
pub fn effective_claim_name<'a>(user: &'a str, process_user: Option<&str>) -> &'a str {
    if process_user.is_some_and(|owner| owner == user) {
        ""
    } else {
        user
    }
}

/// Process-wide lock serializing [`DISABLE_ENV`] mutation between
/// tests (env is per-process; parallel tests must not interleave a
/// set/assert/remove cycle).
#[cfg(test)]
pub(crate) fn test_serialize_env() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

/// Login name of the process owner, for [`effective_claim_name`].
fn process_user() -> Option<String> {
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

/// Blocking fprintd client on the system bus. Clone shares the
/// connection; all methods fail closed with no logging.
#[derive(Debug, Clone)]
pub struct Fprintd {
    conn: zbus::blocking::Connection,
}

impl Fprintd {
    /// Whether fingerprint auth is pinned off via [`DISABLE_ENV`].
    pub fn disabled_by_env() -> bool {
        std::env::var_os(DISABLE_ENV).is_some()
    }

    /// Connect to the system bus. `None` when disabled or unreachable:
    /// callers treat that as no reader and stay password-only.
    pub fn connect() -> Option<Self> {
        if Self::disabled_by_env() {
            return None;
        }
        zbus::blocking::Connection::system()
            .ok()
            .map(|conn| Self { conn })
    }

    fn manager(&self) -> Option<zbus::blocking::Proxy<'_>> {
        zbus::blocking::Proxy::new(&self.conn, BUS_NAME, MANAGER_PATH, MANAGER_IFACE).ok()
    }

    /// Default reader, else the first enumerated one, else none.
    fn default_device(&self) -> Option<zbus::zvariant::OwnedObjectPath> {
        let manager = self.manager()?;
        if let Ok(path) =
            manager.call::<_, _, zbus::zvariant::OwnedObjectPath>("GetDefaultDevice", &())
        {
            return Some(path);
        }
        let devices: Vec<zbus::zvariant::OwnedObjectPath> = manager.call("GetDevices", &()).ok()?;
        devices.into_iter().next()
    }

    fn device_proxy<'a>(
        &'a self,
        path: &'a zbus::zvariant::OwnedObjectPath,
    ) -> Option<zbus::blocking::Proxy<'a>> {
        zbus::blocking::Proxy::new(&self.conn, BUS_NAME, path.as_str(), DEVICE_IFACE).ok()
    }

    /// Enrolled finger names for `user`; empty on any failure.
    pub fn enrolled_fingers(&self, user: &str) -> Vec<String> {
        let Some(path) = self.default_device() else {
            return Vec::new();
        };
        let Some(device) = self.device_proxy(&path) else {
            return Vec::new();
        };
        let name = effective_claim_name(user, process_user().as_deref());
        device
            .call::<_, _, Vec<String>>("ListEnrolledFingers", &(name,))
            .unwrap_or_default()
    }

    /// Truthful capability for `user`: reader presence plus enrollment.
    pub fn capability(&self, user: &str) -> Capability {
        let Some(path) = self.default_device() else {
            return Capability::unavailable();
        };
        let Some(device) = self.device_proxy(&path) else {
            return Capability::unavailable();
        };
        let name = effective_claim_name(user, process_user().as_deref());
        let enrolled = device
            .call::<_, _, Vec<String>>("ListEnrolledFingers", &(name,))
            .unwrap_or_default();
        Capability {
            available: true,
            enrolled_fingers: enrolled,
        }
    }

    /// One verification for `user`, bounded by `timeout`: claim the
    /// reader, start any-finger verification, and wait for the terminal
    /// `VerifyStatus`. Always stops and releases the claim, best
    /// effort. Runs the blocking signal wait on a worker thread (the
    /// same shape as [`crate::pam::authenticate_with_deadline`]); on
    /// timeout the worker still releases once the daemon answers.
    pub fn verify(&self, user: &str, timeout: Duration) -> VerifyOutcome {
        let (tx, rx) = mpsc::channel();
        let worker = self.clone();
        let user = user.to_owned();
        let spawned = std::thread::Builder::new()
            .name("tuna-fprintd".into())
            .spawn(move || {
                let _ = tx.send(worker.verify_inner(&user));
            });
        if spawned.is_err() {
            return VerifyOutcome::Error;
        }
        rx.recv_timeout(timeout).unwrap_or(VerifyOutcome::TimedOut)
    }

    /// One verification with [`VERIFY_TIMEOUT`].
    pub fn verify_user(&self, user: &str) -> VerifyOutcome {
        self.verify(user, VERIFY_TIMEOUT)
    }

    /// Claim, verify, stop, release. Every error fails closed; the
    /// release runs on all paths so a user never wedges the reader
    /// for the next attempt ("already claimed" is a denial, not a
    /// wedge, and clears when this call's connection drops).
    fn verify_inner(&self, user: &str) -> VerifyOutcome {
        let Some(path) = self.default_device() else {
            return VerifyOutcome::NoDevice;
        };
        let Some(device) = self.device_proxy(&path) else {
            return VerifyOutcome::NoDevice;
        };
        let name = effective_claim_name(user, process_user().as_deref()).to_owned();
        // No enrolled prints: do not claim someone else's reader time.
        let enrolled: Vec<String> = device
            .call("ListEnrolledFingers", &(&name,))
            .unwrap_or_default();
        if enrolled.is_empty() {
            return VerifyOutcome::NotEnrolled;
        }
        if device.call::<_, _, ()>("Claim", &(&name,)).is_err() {
            return VerifyOutcome::Error;
        }
        let outcome = self.verify_claimed(&device);
        // Always stop and release; errors are best-effort cleanup.
        let _: Result<(), _> = device.call("VerifyStop", &());
        let _: Result<(), _> = device.call("Release", &());
        outcome
    }

    /// The claimed-device signal wait: subscribe before starting so no
    /// terminal signal is missed, then classify each `VerifyStatus`
    /// until one ends the attempt or retries run out.
    fn verify_claimed(&self, device: &zbus::blocking::Proxy<'_>) -> VerifyOutcome {
        let mut signals = match device.receive_signal("VerifyStatus") {
            Ok(signals) => signals,
            Err(_) => return VerifyOutcome::Error,
        };
        if device
            .call::<_, _, ()>("VerifyStart", &(VERIFY_ANY,))
            .is_err()
        {
            return VerifyOutcome::Error;
        }
        let mut retries = 0usize;
        for message in &mut signals {
            let Ok((result, done)): Result<(String, bool), _> = message.body().deserialize() else {
                // Malformed signal: tolerate bounded, like a retry.
                retries += 1;
                if retries > MAX_VERIFY_RETRIES {
                    return VerifyOutcome::Error;
                }
                continue;
            };
            match classify_verify_status(&result, done) {
                VerifySignal::Done(outcome) => return outcome,
                VerifySignal::Continue => {
                    retries += 1;
                    if retries > MAX_VERIFY_RETRIES {
                        return VerifyOutcome::Error;
                    }
                }
            }
        }
        // The daemon went away mid-attempt.
        VerifyOutcome::Error
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_signals_end_the_attempt() {
        assert_eq!(
            classify_verify_status("verify-match", true),
            VerifySignal::Done(VerifyOutcome::Matched)
        );
        assert_eq!(
            classify_verify_status("verify-no-match", true),
            VerifySignal::Done(VerifyOutcome::NoMatch)
        );
        assert_eq!(
            classify_verify_status("verify-unknown-error", true),
            VerifySignal::Done(VerifyOutcome::Error)
        );
        assert_eq!(
            classify_verify_status("verify-disconnected", true),
            VerifySignal::Done(VerifyOutcome::Error)
        );
    }

    #[test]
    fn retry_reasons_continue_bounded() {
        for result in [
            "verify-retry-scan",
            "verify-swipe-too-short",
            "verify-finger-not-centered",
            "verify-remove-and-retry",
            "verify-no-match",
        ] {
            assert_eq!(
                classify_verify_status(result, false),
                VerifySignal::Continue,
                "should continue: {result}"
            );
        }
    }

    #[test]
    fn unknown_results_fail_closed_when_terminal() {
        assert_eq!(
            classify_verify_status("verify-something-new", true),
            VerifySignal::Done(VerifyOutcome::Error)
        );
        assert_eq!(
            classify_verify_status("verify-something-new", false),
            VerifySignal::Continue
        );
    }

    #[test]
    fn own_user_claims_anonymously_other_users_explicitly() {
        assert_eq!(effective_claim_name("ada", Some("ada")), "");
        assert_eq!(effective_claim_name("ada", Some("grace")), "ada");
        assert_eq!(effective_claim_name("ada", None), "ada");
    }

    #[test]
    fn capability_is_offered_only_when_enrolled() {
        assert!(!Capability::unavailable().offered());
        assert!(!Capability {
            available: true,
            enrolled_fingers: Vec::new(),
        }
        .offered());
        assert!(Capability {
            available: true,
            enrolled_fingers: vec!["right-index-finger".to_string()],
        }
        .offered());
    }

    #[test]
    fn disabled_env_pins_everything_off() {
        let _guard = test_serialize_env().lock().unwrap();
        let prior = std::env::var_os(DISABLE_ENV);
        unsafe { std::env::set_var(DISABLE_ENV, "1") };
        assert!(Fprintd::disabled_by_env());
        assert!(Fprintd::connect().is_none());
        unsafe {
            match prior {
                Some(value) => std::env::set_var(DISABLE_ENV, value),
                None => std::env::remove_var(DISABLE_ENV),
            }
        }
    }

    #[test]
    fn no_daemon_fails_closed_without_panic() {
        if Fprintd::disabled_by_env() {
            return;
        }
        // Without fprintd on the system bus this is None (the expected
        // CI/container path); with a daemon but no reader, capability
        // reports unavailable and verify refuses promptly.
        if let Some(client) = Fprintd::connect() {
            let capability = client.capability("nobody-fprintd-probe");
            assert!(!capability.offered());
            assert!(matches!(
                client.verify("nobody-fprintd-probe", Duration::from_secs(5)),
                VerifyOutcome::NoDevice
                    | VerifyOutcome::NotEnrolled
                    | VerifyOutcome::Error
                    | VerifyOutcome::TimedOut
            ));
        }
    }
}
