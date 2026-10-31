//! Per-frame pointer motion coalescing, as Mutter's stage does it.
//!
//! Clutter queues backend events and processes the queue once per frame
//! clock dispatch (`clutter-stage-view.c`, `handle_frame_clock_frame` →
//! `_clutter_stage_process_queued_events` in `clutter-stage.c`). There,
//! consecutive `CLUTTER_MOTION` events are folded into the last one:
//! its absolute position wins and `clutter_stage_compress_motion` sums
//! the relative deltas (`dx`/`dy`, `dx_unaccel`/`dy_unaccel`) so
//! `zwp_relative_pointer_v1` clients lose nothing. Any other event ends
//! the run, so it is handled after the motion that preceded it and at
//! that motion's position.
//!
//! [`MotionCoalescer`] is that queue for this compositor: motion and
//! relative motion are absorbed, anything else first takes them out in
//! their original order, and the runtime takes the rest once per output
//! frame interval.

use std::time::{Duration, Instant};

use smithay::utils::{Logical, Point};

use crate::windows::ManagerInput;

/// Accumulated `zwp_relative_pointer_v1` payload.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Relative {
    delta: Point<f64, Logical>,
    delta_unaccel: Point<f64, Logical>,
    utime: u64,
}

/// Pending pointer motion awaiting its frame.
#[derive(Debug, Default)]
pub struct MotionCoalescer {
    /// The newest absolute position and its timestamp.
    motion: Option<(Point<f64, Logical>, u32)>,
    /// Relative deltas summed since the last delivery.
    relative: Option<Relative>,
    /// When motion last reached the window manager.
    delivered_at: Option<Instant>,
}

impl MotionCoalescer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold `input` into the pending motion and return `None`, or hand
    /// it back when it is not motion: the caller must then
    /// [`take`](Self::take) and deliver the pending motion before it.
    pub fn absorb(&mut self, input: ManagerInput) -> Option<ManagerInput> {
        match input {
            ManagerInput::Motion { pos, time } => {
                self.motion = Some((pos, time));
                None
            }
            ManagerInput::RelativeMotion {
                delta,
                delta_unaccel,
                utime,
            } => {
                let sum = self.relative.map_or(
                    Relative {
                        delta,
                        delta_unaccel,
                        utime,
                    },
                    |pending| Relative {
                        delta: pending.delta + delta,
                        delta_unaccel: pending.delta_unaccel + delta_unaccel,
                        utime,
                    },
                );
                self.relative = Some(sum);
                None
            }
            other => Some(other),
        }
    }

    /// Whether motion awaits delivery.
    pub fn pending(&self) -> bool {
        self.motion.is_some() || self.relative.is_some()
    }

    /// The newest absolute position not yet delivered.
    pub fn position(&self) -> Option<Point<f64, Logical>> {
        self.motion.map(|(pos, _)| pos)
    }

    /// How long until pending motion is due: `None` with nothing
    /// pending, zero when due now. Motion is delivered at most once per
    /// `interval` (one output frame); the first after a quiet interval
    /// goes out at once, as an idle frame clock dispatches immediately.
    pub fn due_in(&self, now: Instant, interval: Duration) -> Option<Duration> {
        if !self.pending() {
            return None;
        }
        Some(self.delivered_at.map_or(Duration::ZERO, |at| {
            (at + interval).saturating_duration_since(now)
        }))
    }

    /// Take the pending motion in delivery order: the absolute position
    /// first, then the summed relative motion (the order each backend
    /// event produced them in).
    pub fn take(&mut self, now: Instant) -> [Option<ManagerInput>; 2] {
        if self.pending() {
            self.delivered_at = Some(now);
        }
        [
            self.motion
                .take()
                .map(|(pos, time)| ManagerInput::Motion { pos, time }),
            self.relative
                .take()
                .map(|relative| ManagerInput::RelativeMotion {
                    delta: relative.delta,
                    delta_unaccel: relative.delta_unaccel,
                    utime: relative.utime,
                }),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: Duration = Duration::from_nanos(16_666_667);

    fn motion(x: f64, y: f64, time: u32) -> ManagerInput {
        ManagerInput::Motion {
            pos: (x, y).into(),
            time,
        }
    }

    fn relative(dx: f64, dy: f64, ux: f64, uy: f64, utime: u64) -> ManagerInput {
        ManagerInput::RelativeMotion {
            delta: (dx, dy).into(),
            delta_unaccel: (ux, uy).into(),
            utime,
        }
    }

    fn drain(queue: &mut MotionCoalescer, now: Instant) -> Vec<ManagerInput> {
        queue.take(now).into_iter().flatten().collect()
    }

    #[test]
    fn motions_within_a_frame_fold_to_the_last_position() {
        let mut queue = MotionCoalescer::new();
        for i in 0..2000u32 {
            assert_eq!(
                queue.absorb(motion(f64::from(i), 2.0 * f64::from(i), i)),
                None
            );
        }
        assert_eq!(queue.position(), Some((1999.0, 3998.0).into()));
        let now = Instant::now();
        assert_eq!(drain(&mut queue, now), [motion(1999.0, 3998.0, 1999)]);
        assert!(!queue.pending());
        assert_eq!(drain(&mut queue, now), []);
    }

    #[test]
    fn other_input_is_handed_back_and_pending_motion_goes_first() {
        let mut queue = MotionCoalescer::new();
        let button = ManagerInput::Button {
            button: 0x110,
            pressed: true,
            time: 5,
        };
        assert_eq!(queue.absorb(motion(10.0, 20.0, 4)), None);
        assert_eq!(queue.absorb(button), Some(button));
        // The caller drains before handling the button.
        assert_eq!(drain(&mut queue, Instant::now()), [motion(10.0, 20.0, 4)]);
        assert_eq!(queue.absorb(motion(30.0, 40.0, 6)), None);
        assert_eq!(queue.position(), Some((30.0, 40.0).into()));
    }

    #[test]
    fn relative_deltas_sum_exactly_with_the_newest_timestamp() {
        let mut queue = MotionCoalescer::new();
        let deltas = [
            (1.5, -2.25, 1.0, -2.0),
            (0.25, 0.5, 0.25, 0.5),
            (-3.0, 4.0, -2.5, 3.5),
        ];
        for (i, (dx, dy, ux, uy)) in deltas.into_iter().enumerate() {
            queue.absorb(motion(100.0 + i as f64, 50.0, i as u32));
            queue.absorb(relative(dx, dy, ux, uy, 1_000 * (i as u64 + 1)));
        }
        assert_eq!(
            drain(&mut queue, Instant::now()),
            [
                motion(102.0, 50.0, 2),
                relative(-1.25, 2.25, -1.25, 2.0, 3_000)
            ]
        );
    }

    #[test]
    fn delivery_is_paced_to_one_per_frame_interval() {
        let mut queue = MotionCoalescer::new();
        let start = Instant::now();
        assert_eq!(queue.due_in(start, FRAME), None);
        queue.absorb(motion(1.0, 1.0, 1));
        // The first motion after a quiet spell is due at once.
        assert_eq!(queue.due_in(start, FRAME), Some(Duration::ZERO));
        drain(&mut queue, start);
        queue.absorb(motion(2.0, 2.0, 2));
        let later = start + Duration::from_millis(5);
        assert_eq!(
            queue.due_in(later, FRAME),
            Some(FRAME - Duration::from_millis(5))
        );
        assert_eq!(queue.due_in(start + FRAME, FRAME), Some(Duration::ZERO));
    }
}
