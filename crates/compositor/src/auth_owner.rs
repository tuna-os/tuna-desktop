//! Common runtime method ownership. A method tag is not a verification receipt.
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Origin {
    pub connection: u64,
    pub request: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Attempt {
    pub origin: Option<Origin>,
    pub serial: u64,
    pub epoch: u64,
}
/// Runtime-selected authority. Selecting a kind is not a verification result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MethodKind {
    Password,
    PamConversation,
    Fingerprint,
}
#[derive(Default)]
pub(crate) struct Slot {
    active: Option<Attempt>,
    method: Option<MethodKind>,
    serial: u64,
    locked: bool,
}
/// Saturate instead of reusing an epoch. MAX permanently refuses new grants.
pub(crate) fn advance_epoch(epoch: &AtomicU64) -> bool {
    let mut current = epoch.load(Ordering::Acquire);
    loop {
        let Some(next) = current.checked_add(1) else {
            return false;
        };
        match epoch.compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return true,
            Err(observed) => current = observed,
        }
    }
}
impl Slot {
    pub fn note_lock(&mut self, epoch: &AtomicU64) {
        if !self.locked {
            advance_epoch(epoch);
            self.locked = true;
        }
    }
    pub fn begin(&mut self, origin: Option<Origin>, epoch: u64) -> Option<Attempt> {
        self.begin_method(origin, epoch, MethodKind::Password)
    }
    pub fn begin_method(
        &mut self,
        origin: Option<Origin>,
        epoch: u64,
        method: MethodKind,
    ) -> Option<Attempt> {
        if self.active.is_some() || epoch == u64::MAX {
            return None;
        }
        self.serial = self.serial.checked_add(1)?;
        let attempt = Attempt {
            origin,
            serial: self.serial,
            epoch,
        };
        self.active = Some(attempt);
        self.method = Some(method);
        Some(attempt)
    }
    /// A callback can retire only its exact outer wrapper, never a newer one.
    pub fn retire(&mut self, attempt: Attempt) -> bool {
        if self.method != Some(MethodKind::Password) {
            return false;
        }
        self.retire_exact(attempt)
    }
    /// Only a PAM retirement result can release PAM/password ownership. This
    /// cannot accept a fingerprint Match as a retirement receipt.
    pub fn retire_pam_method(
        &mut self,
        attempt: Attempt,
        method: MethodKind,
        result: crate::pam_backend::Verification,
    ) -> bool {
        if method == MethodKind::Fingerprint || self.method != Some(method) || !result.may_retire()
        {
            return false;
        }
        self.retire_exact(attempt)
    }
    fn retire_exact(&mut self, attempt: Attempt) -> bool {
        if self.active != Some(attempt) {
            return false;
        }
        self.active = None;
        self.method = None;
        true
    }
    pub fn matches(&self, attempt: Attempt) -> bool {
        self.active == Some(attempt)
    }
    /// Check a completed result before releasing its exact wrapper slot.
    /// The caller must still use grant's CAS against a later epoch change.
    pub fn completion_live(&self, attempt: Attempt, epoch: &AtomicU64, origin_live: bool) -> bool {
        self.matches(attempt)
            && attempt.origin.is_some()
            && origin_live
            && epoch.load(Ordering::Acquire) == attempt.epoch
    }
    /// Linearize success against immediate suspend invalidation.
    pub fn grant(&mut self, epoch: &AtomicU64, expected: u64) -> bool {
        if self.active.is_some() {
            return false;
        }
        let Some(next) = expected.checked_add(1) else {
            return false;
        };
        if epoch
            .compare_exchange(expected, next, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return false;
        }
        self.locked = false;
        true
    }
    /// logind's `Unlock` ends the lock without a verification, as
    /// `screenShield.js` `deactivate()` does. The epoch still advances, so
    /// an attempt in flight belongs to the ended lock and can never grant.
    pub fn release_by_session(&mut self, epoch: &AtomicU64) {
        advance_epoch(epoch);
        self.locked = false;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lock_cycles_success_and_immediate_suspend_never_reuse_epochs() {
        let epoch = AtomicU64::new(0);
        let mut slot = Slot::default();
        slot.note_lock(&epoch);
        assert_eq!(epoch.load(Ordering::Acquire), 1);
        slot.note_lock(&epoch);
        assert_eq!(epoch.load(Ordering::Acquire), 1);
        let old = slot.begin(None, 1).unwrap();
        advance_epoch(&epoch);
        assert!(!slot.grant(&epoch, old.epoch));
        assert!(slot.retire(old));
        let fresh = slot.begin(None, 2).unwrap();
        assert!(!slot.grant(&epoch, fresh.epoch));
        assert!(slot.retire(fresh));
        assert!(slot.grant(&epoch, fresh.epoch));
        slot.note_lock(&epoch);
        assert_eq!(epoch.load(Ordering::Acquire), 4);
    }
    #[test]
    fn logind_unlock_ends_the_lock_and_strands_the_attempt_in_flight() {
        let epoch = AtomicU64::new(0);
        let mut slot = Slot::default();
        slot.note_lock(&epoch);
        let pending = slot.begin(None, 1).unwrap();
        slot.release_by_session(&epoch);
        assert_eq!(epoch.load(Ordering::Acquire), 2);
        assert!(slot.retire(pending));
        assert!(
            !slot.grant(&epoch, pending.epoch),
            "a password checked under the ended lock grants nothing"
        );
        slot.note_lock(&epoch);
        assert_eq!(epoch.load(Ordering::Acquire), 3, "the next lock is new");
    }
    #[test]
    fn stale_mismatched_duplicate_and_concurrent_results_preserve_ownership() {
        let mut slot = Slot::default();
        let origin = Origin {
            connection: 1,
            request: 1,
        };
        let old = slot.begin(Some(origin), 7).unwrap();
        assert!(slot.begin(Some(origin), 7).is_none());
        assert!(!slot.retire(Attempt { epoch: 8, ..old }));
        assert!(slot.matches(old));
        assert!(slot.retire(old));
        let fresh = slot
            .begin(
                Some(Origin {
                    connection: 2,
                    ..origin
                }),
                8,
            )
            .unwrap();
        assert!(!slot.retire(old));
        assert!(slot.matches(fresh));
        assert!(slot.retire(fresh));
        assert!(!slot.retire(fresh));
    }
    #[test]
    fn already_queued_retired_success_cannot_grant_after_origin_or_epoch_loss() {
        // This pins the common Runtime predicate, not physical PAM retirement:
        // the typed result is already joined/queued before invalidation.
        let (send, receive) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            send.send(crate::pam_backend::Verification::Retired {
                authenticated: true,
            })
            .unwrap();
        });
        worker.join().unwrap();
        let completed = receive.try_recv().unwrap();
        assert!(completed.authenticated());
        let epoch = AtomicU64::new(7);
        let mut slot = Slot::default();
        let origin = Origin {
            connection: 1,
            request: 1,
        };
        let old = slot.begin(Some(origin), 7).unwrap();
        assert!(!slot.completion_live(old, &epoch, false));
        assert!(slot.matches(old));
        assert!(advance_epoch(&epoch));
        assert!(!slot.completion_live(old, &epoch, true));
        assert!(slot.retire(old));
        let fresh = slot
            .begin(
                Some(Origin {
                    connection: 2,
                    request: 1,
                }),
                8,
            )
            .unwrap();
        assert!(!slot.completion_live(old, &epoch, true));
        assert!(slot.matches(fresh));
        assert!(slot.completion_live(fresh, &epoch, true));
        assert!(slot.retire(fresh));
        assert!(slot.grant(&epoch, fresh.epoch));
    }
    #[test]
    fn method_owners_share_one_slot_and_only_exact_pam_retirement_releases_it() {
        use crate::pam_backend::Verification;
        let epoch = AtomicU64::new(7);
        let mut slot = Slot::default();
        let origin = Some(Origin {
            connection: 1,
            request: 9,
        });
        let pam = slot
            .begin_method(origin, 7, MethodKind::PamConversation)
            .unwrap();
        for method in [
            MethodKind::Password,
            MethodKind::Fingerprint,
            MethodKind::PamConversation,
        ] {
            assert!(slot.begin_method(origin, 7, method).is_none());
        }
        assert!(!slot.retire(pam));
        assert!(!slot.grant(&epoch, 7));
        assert!(!slot.retire_pam_method(pam, MethodKind::PamConversation, Verification::Unretired));
        assert!(!slot.retire_pam_method(
            pam,
            MethodKind::Password,
            Verification::Retired {
                authenticated: true
            }
        ));
        assert!(!slot.retire_pam_method(
            Attempt { epoch: 8, ..pam },
            MethodKind::PamConversation,
            Verification::Retired {
                authenticated: false
            }
        ));
        assert!(slot.matches(pam));
        assert!(slot.retire_pam_method(
            pam,
            MethodKind::PamConversation,
            Verification::Retired {
                authenticated: false
            }
        ));
        let finger = slot
            .begin_method(origin, 7, MethodKind::Fingerprint)
            .unwrap();
        assert!(!slot.retire_pam_method(
            finger,
            MethodKind::Fingerprint,
            Verification::Retired {
                authenticated: true
            }
        ));
        assert!(!slot.retire_pam_method(
            pam,
            MethodKind::PamConversation,
            Verification::RejectedBeforeSpawn
        ));
        assert!(!slot.grant(&epoch, 7));
        assert!(
            slot.matches(finger),
            "Matched or PAM receipt cannot retire a fingerprint child"
        );
        let mut separate = Slot::default();
        let refused = separate
            .begin_method(origin, 7, MethodKind::PamConversation)
            .unwrap();
        assert!(separate.retire_pam_method(
            refused,
            MethodKind::PamConversation,
            Verification::RejectedBeforeSpawn
        ));
        assert!(!separate.retire_pam_method(
            refused,
            MethodKind::PamConversation,
            Verification::Retired {
                authenticated: true
            }
        ));
    }
    #[test]
    fn overflow_is_permanently_fail_closed() {
        let epoch = AtomicU64::new(u64::MAX);
        assert!(!advance_epoch(&epoch));
        assert_eq!(epoch.load(Ordering::Acquire), u64::MAX);
        let mut slot = Slot::default();
        assert!(slot.begin(None, u64::MAX).is_none());
        slot.serial = u64::MAX;
        assert!(slot.begin(None, 1).is_none());
        assert!(slot.begin(None, 1).is_none());
        assert!(!slot.grant(&epoch, u64::MAX));
    }
}
