//! The runtime's side of org.gnome.Mutter.InputCapture (#1040): backend
//! input passes the capture router before aids, the lock screen,
//! keybindings or clients see it (mutter's
//! `meta_display_process_captured_input`, first in `events.c`).

use std::time::Instant;

use smithay::utils::{Logical, Point};

use crate::windows::ManagerInput;

use super::{input_time, Runtime};

impl Runtime {
    /// One backend event's inputs, minus what an active capture took.
    /// `from` is the pointer before them. Captured input is still the
    /// user's activity: it keeps the session from idling into the lock
    /// while they work on the other machine.
    #[cfg_attr(not(feature = "drm"), allow(dead_code))]
    pub(super) fn route_input_capture(
        &mut self,
        inputs: Vec<ManagerInput>,
        from: Point<f64, Logical>,
    ) -> Vec<ManagerInput> {
        let locked = self.is_locked();
        let motion_free = !self.manager.pointer_constraint_owns_motion(&self.state);
        let time = inputs.iter().map(input_time).max().unwrap_or(0);
        let routed = self.input_capture.route(inputs, from, locked, motion_free);
        if routed.captured {
            self.lock.note_input(time);
            self.idle_since = Instant::now();
            self.idle_monitor.activity();
            self.active_at = Instant::now();
        }
        routed.deliver
    }

    /// `Release` with `cursor_position`: the pointer goes there, kept on
    /// the outputs. Never while locked.
    pub(super) fn input_capture_warp(&mut self, x: f64, y: f64) {
        if self.is_locked() {
            return;
        }
        let (x, y) = self.clamp_to_outputs(x, y);
        let time = self.lock_now_ms() as u32;
        self.on_manager_input(ManagerInput::Motion {
            pos: (x, y).into(),
            time,
        });
    }
}
