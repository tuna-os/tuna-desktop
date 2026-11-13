//! Mutter's window suspend state (#503, xdg-shell v6 `suspended`).
//!
//! Mutter (`src/core/window.c`, `update_suspend_state`) keeps every
//! window `ACTIVE` while its actor is effectively visible
//! (`meta_window_actor` `update_is_effectively_visible`: mapped and not
//! wholly obscured, or shown by a mapped clone such as the overview's).
//! Once it is not, the window turns `HIDDEN` and, after
//! `SUSPEND_HIDDEN_TIMEOUT_S` (3 s), `SUSPENDED`; showing it again makes
//! it `ACTIVE` at once. Only `SUSPENDED` reaches the client, as the
//! `xdg_toplevel` `suspended` state (`meta-window-wayland.c`
//! `suspend_state_changed`).
//!
//! This is that state machine with the visibility supplied by the
//! caller; the window manager sends the configures.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// How long a window stays hidden before it is suspended: Mutter's
/// `SUSPEND_HIDDEN_TIMEOUT_S`.
pub(crate) const SUSPEND_HIDDEN_TIMEOUT: Duration = Duration::from_secs(3);

/// A window not shown: hidden since a time, or suspended. A window
/// without one is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Hidden { since: Instant },
    Suspended,
}

#[derive(Debug, Clone, Copy)]
struct Entry {
    phase: Phase,
    /// The update that last listed the window.
    seen: u64,
}

#[derive(Debug, Default)]
pub(crate) struct Suspension {
    entries: HashMap<u64, Entry>,
    pass: u64,
}

impl Suspension {
    /// Advance every window to `now`. `windows` lists each tracked window
    /// with whether something on screen shows it; windows not listed are
    /// forgotten. Returns the windows whose suspended state changed, with
    /// the new state.
    pub(crate) fn update(
        &mut self,
        windows: impl IntoIterator<Item = (u64, bool)>,
        now: Instant,
    ) -> Vec<(u64, bool)> {
        self.pass = self.pass.wrapping_add(1);
        let pass = self.pass;
        let mut changed = Vec::new();
        for (id, shown) in windows {
            if shown {
                if self
                    .entries
                    .remove(&id)
                    .is_some_and(|entry| entry.phase == Phase::Suspended)
                {
                    changed.push((id, false));
                }
                continue;
            }
            let entry = self.entries.entry(id).or_insert(Entry {
                phase: Phase::Hidden { since: now },
                seen: pass,
            });
            entry.seen = pass;
            if let Phase::Hidden { since } = entry.phase {
                if now.saturating_duration_since(since) >= SUSPEND_HIDDEN_TIMEOUT {
                    entry.phase = Phase::Suspended;
                    changed.push((id, true));
                }
            }
        }
        self.entries.retain(|_, entry| entry.seen == pass);
        changed.sort_unstable();
        changed
    }

    /// When the next hidden window is due to be suspended.
    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.entries
            .values()
            .filter_map(|entry| match entry.phase {
                Phase::Hidden { since } => Some(since + SUSPEND_HIDDEN_TIMEOUT),
                Phase::Suspended => None,
            })
            .min()
    }

    pub(crate) fn is_suspended(&self, id: u64) -> bool {
        self.entries
            .get(&id)
            .is_some_and(|entry| entry.phase == Phase::Suspended)
    }

    /// Every suspended window, in id order.
    pub(crate) fn suspended(&self) -> Vec<u64> {
        let mut ids: Vec<u64> = self
            .entries
            .iter()
            .filter_map(|(id, entry)| (entry.phase == Phase::Suspended).then_some(*id))
            .collect();
        ids.sort_unstable();
        ids
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_windows_suspend_after_mutters_timeout_and_resume_when_shown() {
        let mut s = Suspension::default();
        let t0 = Instant::now();
        assert!(s.update([(1, true), (2, false)], t0).is_empty());
        assert_eq!(s.deadline(), Some(t0 + SUSPEND_HIDDEN_TIMEOUT));
        let early = t0 + SUSPEND_HIDDEN_TIMEOUT - Duration::from_millis(1);
        assert!(s.update([(1, true), (2, false)], early).is_empty());
        assert!(!s.is_suspended(2));
        let due = t0 + SUSPEND_HIDDEN_TIMEOUT;
        assert_eq!(s.update([(1, true), (2, false)], due), vec![(2, true)]);
        assert!(s.is_suspended(2) && !s.is_suspended(1));
        assert_eq!(s.suspended(), vec![2]);
        assert_eq!(s.deadline(), None);
        // Staying hidden changes nothing; showing it resumes it at once.
        assert!(s.update([(1, true), (2, false)], due).is_empty());
        assert_eq!(s.update([(1, true), (2, true)], due), vec![(2, false)]);
        assert!(s.suspended().is_empty());
    }

    #[test]
    fn a_window_shown_before_the_timeout_restarts_it_and_is_never_told() {
        let mut s = Suspension::default();
        let t0 = Instant::now();
        s.update([(1, false)], t0);
        let t1 = t0 + Duration::from_secs(2);
        assert!(s.update([(1, true)], t1).is_empty());
        assert!(s.update([(1, false)], t1).is_empty());
        assert!(s
            .update([(1, false)], t0 + SUSPEND_HIDDEN_TIMEOUT)
            .is_empty());
        assert_eq!(s.deadline(), Some(t1 + SUSPEND_HIDDEN_TIMEOUT));
    }

    #[test]
    fn unmapped_windows_are_forgotten() {
        let mut s = Suspension::default();
        let t0 = Instant::now();
        s.update([(1, false)], t0);
        s.update([(1, false)], t0 + SUSPEND_HIDDEN_TIMEOUT);
        assert!(s.is_suspended(1));
        assert!(s.update([], t0 + SUSPEND_HIDDEN_TIMEOUT).is_empty());
        assert!(!s.is_suspended(1));
        assert_eq!(s.deadline(), None);
    }
}
