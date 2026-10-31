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

    /// Finish the motion run before a raw non-motion input, even if an
    /// accessibility aid will consume that input instead of forwarding it.
    /// Keep this ahead of filtering: a consumed Mouse Keys release still
    /// separates the preceding motion from the next frame's events.
    pub(crate) fn before_backend_input(
        &mut self,
        input: &ManagerInput,
    ) -> [Option<ManagerInput>; 2] {
        if !self.pending()
            || matches!(
                input,
                ManagerInput::Motion { .. } | ManagerInput::RelativeMotion { .. }
            )
        {
            return [None, None];
        }
        self.take(Instant::now())
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

    #[test]
    fn backend_motion_does_not_end_the_coalescing_run() {
        let mut queue = MotionCoalescer::new();
        for i in 0..2000 {
            let absolute = motion(f64::from(i), 20.0, i);
            let relative = relative(0.25, -0.5, 0.125, -0.25, u64::from(i));
            assert_eq!(queue.before_backend_input(&absolute), [None, None]);
            assert_eq!(queue.absorb(absolute), None);
            assert_eq!(queue.before_backend_input(&relative), [None, None]);
            assert_eq!(queue.absorb(relative), None);
        }
        assert_eq!(queue.delivered_at, None, "motion never forces a delivery");
        assert_eq!(
            drain(&mut queue, Instant::now()),
            [
                motion(1999.0, 20.0, 1999),
                relative(500.0, -1000.0, 250.0, -500.0, 1999)
            ]
        );
    }

    #[test]
    fn empty_backend_barrier_does_not_advance_the_delivery_clock() {
        let mut queue = MotionCoalescer::new();
        let keyup = ManagerInput::Key {
            keycode: 77,
            pressed: false,
            time: 2,
        };
        assert_eq!(queue.before_backend_input(&keyup), [None, None]);
        assert_eq!(queue.delivered_at, None);
        let delivered = Instant::now();
        queue.absorb(motion(10.0, 20.0, 1));
        queue.take(delivered);
        for _ in 0..2000 {
            assert_eq!(queue.before_backend_input(&keyup), [None, None]);
        }
        assert_eq!(queue.delivered_at, Some(delivered));
    }

    #[test]
    fn consumed_mouse_keys_release_delivers_the_pending_timer_move_first() {
        use crate::input_aids::{AidEffect, InputAids};
        use tuna_shell_control::KeyboardAids;

        let mut aids = InputAids::new();
        aids.configure(
            &KeyboardAids {
                mouse: true,
                mouse_max_speed: 1000,
                mouse_accel_ms: 1,
                mouse_init_delay_ms: 0,
                ..KeyboardAids::default()
            },
            0,
        );
        let mut queue = MotionCoalescer::new();
        let down = ManagerInput::Key {
            keycode: 77,
            pressed: true,
            time: 0,
        };
        let effects = aids.process(down, 0, false);
        let [AidEffect::PointerMove { dx, dy, time }] = effects.as_slice() else {
            panic!("Mouse Keys must consume KP_Right and synthesize motion: {effects:?}");
        };
        assert_eq!((*dx, *dy), (1.0, 0.0));
        queue.absorb(motion(100.0 + dx, 200.0 + dy, *time));
        assert_eq!(drain(&mut queue, Instant::now()), [motion(101.0, 200.0, 0)]);

        // Runtime polls aids after flushing this frame. Its next timer
        // move therefore waits in the queue until a frame or raw barrier.
        let effects = aids.poll(100);
        let [AidEffect::PointerMove { dx, dy, time }] = effects.as_slice() else {
            panic!("the held keypad key must produce a timer move: {effects:?}");
        };
        assert_eq!((*dx, *dy), (100.0, 0.0));
        queue.absorb(motion(101.0 + dx, 200.0 + dy, *time));
        let up = ManagerInput::Key {
            pressed: false,
            time: 101,
            keycode: 77,
        };
        // Exactly the runtime ordering: deliver the run, then let the
        // aid consume keyup. Previously the aid swallowed this barrier,
        // leaving the delivered position at (101, 200) until a later frame.
        let delivered = queue.before_backend_input(&up);
        assert_eq!(delivered, [Some(motion(201.0, 200.0, 100)), None]);
        assert!(aids.process(up, 101, false).is_empty(), "keyup is consumed");
        assert!(
            !queue.pending(),
            "no movement is left to arrive after release"
        );
        assert!(
            aids.poll(1000).is_empty(),
            "the released aid cannot move again"
        );
        assert_eq!(queue.take(Instant::now()), [None, None]);
    }

    #[test]
    fn non_motion_backend_barriers_take_motion_before_filtering() {
        use crate::windows::AxisStop;

        for barrier in [
            ManagerInput::Key {
                keycode: 30,
                pressed: true,
                time: 2,
            },
            ManagerInput::Button {
                button: 0x110,
                pressed: true,
                time: 2,
            },
            ManagerInput::Axis {
                horizontal: 0.0,
                vertical: 120.0,
                stop: AxisStop::default(),
                time: 2,
            },
            ManagerInput::CornerPressure {
                pos: (0.0, 0.0).into(),
                time: 2,
            },
            ManagerInput::SwipeBegin {
                fingers: 3,
                time: 2,
            },
            ManagerInput::SwipeUpdate {
                delta: (1.0, 0.0).into(),
                time: 2,
            },
            ManagerInput::SwipeEnd {
                cancelled: false,
                time: 2,
            },
        ] {
            let mut queue = MotionCoalescer::new();
            queue.absorb(motion(10.0, 20.0, 1));
            queue.absorb(relative(2.0, 3.0, 1.0, 1.5, 1000));
            assert_eq!(
                queue.before_backend_input(&barrier),
                [
                    Some(motion(10.0, 20.0, 1)),
                    Some(relative(2.0, 3.0, 1.0, 1.5, 1000))
                ],
                "{barrier:?}"
            );
            assert!(!queue.pending());
        }
    }
}
