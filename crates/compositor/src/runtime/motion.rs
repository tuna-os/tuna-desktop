//! The runtime's side of per-frame motion coalescing
//! ([`crate::motion_coalesce`]): every input bound for the window
//! manager passes [`Runtime::on_manager_input`], where motion waits for
//! its frame and anything else first delivers the motion before it.

use std::time::{Duration, Instant};

use smithay::utils::{Logical, Point};

use crate::windows::ManagerInput;

use super::Runtime;

impl Runtime {
    /// One input bound for the window manager. Motion is folded until
    /// the next output frame (Mutter's stage queue); any other input
    /// delivers the pending motion first, so a click never lands at a
    /// stale position nor ahead of the motion that preceded it.
    pub(super) fn on_manager_input(&mut self, input: ManagerInput) {
        let Some(input) = self.motion_queue.absorb(input) else {
            return;
        };
        self.flush_motion();
        self.dispatch_manager_input(input);
    }

    /// Deliver pending motion now, ahead of input or state changes that
    /// must follow it.
    pub(super) fn flush_motion(&mut self) {
        if self.motion_queue.pending() {
            self.deliver_motion(Instant::now());
        }
    }

    /// Deliver pending motion when its frame is due: once per output
    /// frame interval, or at once after a quiet one.
    pub(super) fn flush_motion_if_due(&mut self) {
        let now = Instant::now();
        if self.motion_queue.due_in(now, self.motion_interval()) == Some(Duration::ZERO) {
            self.deliver_motion(now);
        }
    }

    /// Time until pending motion is due, for the loop's wake.
    pub(super) fn motion_wake(&self) -> Option<Duration> {
        self.motion_queue
            .due_in(Instant::now(), self.motion_interval())
    }

    /// Where the pointer is, counting motion not yet delivered.
    pub(super) fn pointer_pos_now(&self) -> Point<f64, Logical> {
        self.motion_queue
            .position()
            .unwrap_or_else(|| self.manager.pointer_pos())
    }

    fn deliver_motion(&mut self, now: Instant) {
        for input in self.motion_queue.take(now).into_iter().flatten() {
            self.dispatch_manager_input(input);
        }
    }

    /// One frame of the primary output, the pace motion is delivered at.
    fn motion_interval(&self) -> Duration {
        self.state
            .primary_output()
            .map_or(crate::frame_timing::DEFAULT_REFRESH, |output| {
                crate::frame_timing::refresh_of(&output)
            })
    }
}
