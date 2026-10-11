//! Earliest real obligation wins. Active work retains the previous 16 ms cap.
use std::time::{Duration, Instant};
pub(crate) const ACTIVE_INTERVAL: Duration = Duration::from_millis(16);
#[derive(Default)]
pub(crate) struct Schedule {
    wait: Option<Duration>,
}
impl Schedule {
    pub fn after(&mut self, delay: Duration) {
        self.wait = Some(self.wait.map_or(delay, |old| old.min(delay)));
    }
    pub fn at(&mut self, deadline: Instant) {
        self.after(deadline.saturating_duration_since(Instant::now()));
    }
    pub fn idle(
        &mut self,
        elapsed: u64,
        lock_timeout: Option<u64>,
        blank: (u64, u64),
        inhibited: bool,
    ) {
        if inhibited {
            return;
        }
        if let Some(timeout) = lock_timeout.filter(|timeout| *timeout != u64::MAX) {
            self.after(Duration::from_millis(timeout.saturating_sub(elapsed)));
        }
        let (start, fade) = blank;
        if start > 0 {
            if elapsed < start {
                self.after(Duration::from_millis(start - elapsed));
            } else if elapsed < start.saturating_add(fade) {
                self.active();
            }
        }
    }
    pub fn motion(&mut self, manual: bool, moving: bool) {
        if moving && !manual {
            self.active();
        }
    }
    pub fn active(&mut self) {
        self.after(ACTIVE_INTERVAL);
    }
    pub fn wait(self) -> Option<Duration> {
        self.wait
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lock_and_blank_deadlines_stop_after_completion_and_during_inhibition() {
        let mut s = Schedule::default();
        s.idle(900, Some(1000), (2000, 500), false);
        assert_eq!(s.wait(), Some(Duration::from_millis(100)));
        let mut s = Schedule::default();
        s.idle(2000, None, (2000, 500), false);
        assert_eq!(s.wait(), Some(ACTIVE_INTERVAL));
        let mut s = Schedule::default();
        s.idle(2500, None, (2000, 500), false);
        assert_eq!(s.wait(), None, "opaque blank has no fade timer");
        let mut s = Schedule::default();
        s.idle(99999, Some(1000), (2000, 500), true);
        assert_eq!(s.wait(), None, "held inhibitor needs its removal event");
        let mut s = Schedule::default();
        s.idle(0, Some(1000), (2000, 500), false);
        assert_eq!(
            s.wait(),
            Some(Duration::from_millis(1000)),
            "inhibition removal starts a fresh idle stretch"
        );
        let mut s = Schedule::default();
        s.idle(0, Some(u64::MAX), (0, 0), false);
        assert_eq!(s.wait(), None);
    }
    #[test]
    fn manual_motion_waits_for_file_wake_but_real_protocol_work_still_runs() {
        let mut s = Schedule::default();
        s.motion(true, true);
        assert_eq!(s.wait(), None);
        let mut s = Schedule::default();
        s.motion(false, true);
        assert_eq!(s.wait(), Some(ACTIVE_INTERVAL));
        let mut s = Schedule::default();
        s.motion(true, true);
        s.active();
        assert_eq!(s.wait(), Some(ACTIVE_INTERVAL));
    }
    #[test]
    fn an_expired_deadline_is_due_without_underflow() {
        let mut s = Schedule::default();
        s.at(Instant::now() - Duration::from_secs(1));
        assert_eq!(s.wait(), Some(Duration::ZERO));
    }
    #[test]
    fn idle_blocks_but_real_deadlines_and_active_work_do_not() {
        assert_eq!(Schedule::default().wait(), None);
        let mut s = Schedule::default();
        s.after(Duration::from_secs(10));
        s.after(Duration::from_secs(2));
        assert_eq!(s.wait(), Some(Duration::from_secs(2)));
        let mut s = Schedule::default();
        s.after(Duration::from_secs(2));
        s.active();
        assert_eq!(s.wait(), Some(ACTIVE_INTERVAL));
        let mut s = Schedule::default();
        s.active();
        s.after(Duration::ZERO);
        assert_eq!(s.wait(), Some(Duration::ZERO));
    }
}
