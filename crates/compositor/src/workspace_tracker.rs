//! Real-time workspace leases, separate from the animation clock. GNOME 51
//! keeps a workspace for one second after its last dialog/splash removal,
//! during a startup sequence, or for an explicit keep-alive lease.
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

#[derive(Debug, Default)]
pub struct WorkspaceTracker {
    removed_dialogs: BTreeMap<u32, Instant>,
    keep_alive: BTreeMap<u32, Instant>,
    startup: BTreeMap<String, u32>,
}

impl WorkspaceTracker {
    pub fn window_removed(&mut self, workspace: u32, dialog_or_splash: bool, now: Instant) {
        // A later ordinary window removal replaces the last removed dialog.
        self.removed_dialogs.remove(&workspace);
        if dialog_or_splash {
            self.removed_dialogs
                .insert(workspace, now + Duration::from_secs(1));
        }
    }

    pub fn keep_alive(&mut self, workspace: u32, duration: Duration, now: Instant) {
        if let Some(expiry) = now.checked_add(duration) {
            self.keep_alive.insert(workspace, expiry);
        }
    }

    pub fn startup_started(&mut self, identity: String, workspace: u32) {
        self.startup.insert(identity, workspace);
    }

    pub fn startup_finished(&mut self, identity: &str) {
        self.startup.remove(identity);
    }

    pub fn protected_workspaces(&mut self, now: Instant) -> Vec<u32> {
        self.removed_dialogs.retain(|_, expiry| *expiry > now);
        self.keep_alive.retain(|_, expiry| *expiry > now);
        self.removed_dialogs
            .keys()
            .chain(self.keep_alive.keys())
            .chain(self.startup.values())
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.removed_dialogs
            .values()
            .chain(self.keep_alive.values())
            .copied()
            .min()
    }

    pub fn remap(&mut self, indices: &[(u32, u32)]) {
        let indices: BTreeMap<_, _> = indices.iter().copied().collect();
        fn move_leases(leases: &mut BTreeMap<u32, Instant>, indices: &BTreeMap<u32, u32>) {
            *leases = std::mem::take(leases)
                .into_iter()
                .filter_map(|(old, expiry)| indices.get(&old).map(|new| (*new, expiry)))
                .collect();
        }
        move_leases(&mut self.removed_dialogs, &indices);
        move_leases(&mut self.keep_alive, &indices);
        self.startup.retain(|_, workspace| {
            if let Some(new) = indices.get(workspace) {
                *workspace = *new;
                true
            } else {
                false
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dialog_grace_expires_at_one_second_and_has_no_past_due_deadline() {
        let now = Instant::now();
        let mut tracker = WorkspaceTracker::default();
        tracker.window_removed(2, true, now);
        assert_eq!(tracker.protected_workspaces(now), [2]);
        assert_eq!(tracker.next_deadline(), Some(now + Duration::from_secs(1)));
        assert_eq!(
            tracker.protected_workspaces(now + Duration::from_millis(999)),
            [2]
        );
        assert!(
            tracker
                .protected_workspaces(now + Duration::from_secs(1))
                .is_empty()
        );
        assert_eq!(tracker.next_deadline(), None);
    }
    #[test]
    fn ordinary_removal_replaces_dialog_grace_but_does_not_cancel_explicit_lease() {
        let now = Instant::now();
        let mut tracker = WorkspaceTracker::default();
        tracker.window_removed(2, true, now);
        tracker.keep_alive(2, Duration::from_secs(5), now);
        tracker.window_removed(2, false, now);
        assert_eq!(tracker.next_deadline(), Some(now + Duration::from_secs(5)));
        assert_eq!(tracker.protected_workspaces(now), [2]);
    }
    #[test]
    fn later_dialog_restarts_grace() {
        let now = Instant::now();
        let mut tracker = WorkspaceTracker::default();
        tracker.window_removed(2, true, now);
        tracker.window_removed(2, true, now + Duration::from_millis(500));
        assert_eq!(
            tracker.protected_workspaces(now + Duration::from_secs(1)),
            [2]
        );
        assert!(
            tracker
                .protected_workspaces(now + Duration::from_millis(1500))
                .is_empty()
        );
    }
    #[test]
    fn startup_sequences_keep_workspace_until_each_identity_finishes() {
        let now = Instant::now();
        let mut tracker = WorkspaceTracker::default();
        tracker.startup_started("first".into(), 2);
        tracker.startup_started("second".into(), 2);
        tracker.startup_finished("first");
        assert_eq!(
            tracker.protected_workspaces(now + Duration::from_secs(100)),
            [2]
        );
        assert_eq!(tracker.next_deadline(), None);
        tracker.startup_finished("second");
        assert!(tracker.protected_workspaces(now).is_empty());
    }
    #[test]
    fn compaction_moves_all_lease_types_without_extending_deadlines() {
        let now = Instant::now();
        let mut tracker = WorkspaceTracker::default();
        tracker.window_removed(5, true, now);
        tracker.keep_alive(7, Duration::from_secs(2), now);
        tracker.startup_started("app".into(), 9);
        tracker.remap(&[(5, 1), (7, 2), (9, 3)]);
        assert_eq!(tracker.protected_workspaces(now), [1, 2, 3]);
        assert_eq!(tracker.next_deadline(), Some(now + Duration::from_secs(1)));
        assert_eq!(
            tracker.protected_workspaces(now + Duration::from_secs(1)),
            [2, 3]
        );
    }
}
