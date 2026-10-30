//! GNOME 51's workspace switch animation (`workspaceAnimation.js`,
//! `swipeTracker.js`).
//!
//! Workspaces sit side by side, one output width plus 100px apart. A
//! switch moves the view along that strip: 250 ms `EASE_OUT_CUBIC` from
//! wherever it is drawn now, so a second key press mid-flight carries on
//! from the current offset instead of jumping back. A three-finger swipe
//! drags the view with the fingers; on release GNOME projects the
//! finger velocity onto the strip, picks the workspace it lands on and
//! finishes in 100-400 ms depending on how fast the fingers were moving.
//!
//! The model switches workspace at once (focus and input follow without
//! waiting); only the drawn position of each workspace's windows trails
//! it. Nothing is reconfigured: the offset is applied at render time.
//! Under a fade-only motion policy a key switch keeps the workspaces in
//! place and crossfades over the same timeline (#523); with motion off it
//! is instant. A swipe slides under every policy: GNOME's swipe paths
//! never consult Reduced Motion, and with animations off only the release
//! is instant (`animationUtils.js:23-33`). Swipe phases also publish the
//! strip position for the shell's Activities dots
//! ([`WorkspaceSlide::take_progress`]).
//! Times are animation-clock time ([`crate::animation_clock`]).

use std::collections::VecDeque;
use std::time::Duration;

use tuna_shell_control::MotionPolicy;

/// `WINDOW_ANIMATION_TIME`: the keyboard switch.
pub const SWITCH_MS: f64 = 250.0;
/// `WORKSPACE_SPACING`: the gap between neighbouring workspaces.
pub const WORKSPACE_SPACING: i32 = 100;
/// `TOUCHPAD_BASE_WIDTH`: finger travel that moves one whole workspace.
pub const TOUCHPAD_BASE_WIDTH: f64 = 400.0;
/// `EVENT_HISTORY_THRESHOLD_MS`: the window the release velocity is
/// measured over.
const HISTORY_MS: u32 = 150;
const MIN_SWIPE_MS: f64 = 100.0;
const MAX_SWIPE_MS: f64 = 400.0;
const VELOCITY_THRESHOLD: f64 = 0.6;
const DECELERATION: f64 = 0.997;
const VELOCITY_CURVE_THRESHOLD: f64 = 2.0;
const DECELERATION_PARABOLA_MULTIPLIER: f64 = 0.35;
/// The derivative of ease-out-cubic at 0: a release keeps its speed.
const DURATION_MULTIPLIER: f64 = 3.0;
const ANIMATION_BASE_VELOCITY: f64 = 0.002;
/// `EPSILON`: a swipe beginning this close to a workspace began on it.
const SNAP_EPSILON: f64 = 0.005;
/// Finished motions kept for the state snapshot.
const HISTORY_LEN: usize = 8;

/// Clutter's `EASE_OUT_CUBIC`.
pub fn ease_out_cubic(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0) - 1.0;
    t * t * t + 1.0
}

/// How the switch is shown under a motion policy.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Motion {
    Slide,
    /// Fade-only (reduced motion): crossfade in place.
    Fade,
    Off,
}

impl Motion {
    pub fn of(policy: MotionPolicy) -> Self {
        if policy.allows_motion() {
            Motion::Slide
        } else if policy.allows_fades() {
            Motion::Fade
        } else {
            Motion::Off
        }
    }

    fn name(self) -> &'static str {
        match self {
            Motion::Slide => "slide",
            Motion::Fade => "fade",
            Motion::Off => "off",
        }
    }
}

/// GNOME's `adjustAnimationTime` for `ms`.
fn scaled(policy: MotionPolicy, ms: f64) -> Duration {
    Duration::from_secs_f64(policy.adjust_ms(ms).max(0.0) / 1000.0)
}

/// Recent finger deltas (`EventHistory`): the release velocity is their
/// sum over the last 150 ms, in px/ms.
#[derive(Debug, Default, Clone)]
pub struct SwipeHistory {
    samples: VecDeque<(u32, f64)>,
}

impl SwipeHistory {
    fn trim(&mut self, time: u32) {
        let threshold = time.saturating_sub(HISTORY_MS);
        while self.samples.front().is_some_and(|(t, _)| *t < threshold) {
            self.samples.pop_front();
        }
    }

    pub fn append(&mut self, time: u32, delta: f64) {
        self.trim(time);
        self.samples.push_back((time, delta));
    }

    /// Velocity at `time` (px/ms); zero without two samples in range.
    pub fn velocity(&mut self, time: u32) -> f64 {
        self.trim(time);
        let (Some(first), Some(last)) = (self.samples.front(), self.samples.back()) else {
            return 0.0;
        };
        if self.samples.len() < 2 || first.0 == last.0 {
            return 0.0;
        }
        let period = f64::from(last.0 - first.0);
        self.samples.iter().skip(1).map(|(_, d)| d).sum::<f64>() / period
    }
}

/// Where a released swipe settles and how long it takes to get there
/// (`_getEndProgress` and `_endGesture` for a touchpad). Positions are
/// workspace slots; `velocity` is px/ms along the strip.
pub fn swipe_release(
    progress: f64,
    initial: f64,
    slots: usize,
    velocity: f64,
    cancelled: bool,
) -> (f64, f64) {
    swipe_release_over(
        progress,
        initial,
        slots,
        velocity,
        cancelled,
        TOUCHPAD_BASE_WIDTH,
    )
}

/// [`swipe_release`] for a gesture whose whole step is `distance`
/// touchpad units (the overview's vertical `TOUCHPAD_BASE_HEIGHT`).
pub fn swipe_release_over(
    progress: f64,
    initial: f64,
    slots: usize,
    velocity: f64,
    cancelled: bool,
    distance: f64,
) -> (f64, f64) {
    let last = slots.saturating_sub(1) as f64;
    let bounds = ((initial - 1.0).max(0.0), (initial + 1.0).min(last));
    release_within(
        progress, initial, bounds, slots, velocity, cancelled, distance,
    )
}

/// `_getBounds` for a swipe that began at `progress` on a strip of
/// `slots`: one workspace either side of the closest one, or of the two
/// it lay between when the fingers caught a switch mid-flight
/// (swipeTracker.js:547-565).
pub fn swipe_bounds(progress: f64, slots: usize) -> (f64, f64) {
    let last = slots.saturating_sub(1) as f64;
    let closest = progress.round().clamp(0.0, last);
    let (prev, next) = if (closest - progress).abs() < SNAP_EPSILON {
        (closest, closest)
    } else {
        (progress.floor(), progress.ceil())
    };
    ((prev - 1.0).max(0.0), (next + 1.0).min(last))
}

/// [`swipe_release_over`] within explicit `bounds` (`_getEndProgress`
/// and `_endGesture`, swipeTracker.js:601-658).
#[allow(clippy::too_many_arguments)]
fn release_within(
    progress: f64,
    initial: f64,
    (low, high): (f64, f64),
    slots: usize,
    velocity: f64,
    cancelled: bool,
    distance: f64,
) -> (f64, f64) {
    let last = slots.saturating_sub(1) as f64;
    let closest = |p: f64| p.round().clamp(0.0, last);
    let end = if cancelled {
        initial
    } else if velocity.abs() < VELOCITY_THRESHOLD {
        closest(progress)
    } else {
        let slope = DECELERATION / (1.0 - DECELERATION) / 1000.0;
        let speed = velocity.abs();
        let travel = if speed > VELOCITY_CURVE_THRESHOLD {
            let c = slope / 2.0 / DECELERATION_PARABOLA_MULTIPLIER;
            let x = speed - VELOCITY_CURVE_THRESHOLD + c;
            slope * VELOCITY_CURVE_THRESHOLD + DECELERATION_PARABOLA_MULTIPLIER * x * x
                - DECELERATION_PARABOLA_MULTIPLIER * c * c
        } else {
            speed * slope
        };
        let pos = (progress + travel * velocity.signum()).clamp(low, high);
        // `_findPointForProjection`: a projection that stays on the
        // starting workspace still moves one in the swipe's direction.
        let (prev, next) = (pos.floor(), pos.ceil());
        if velocity > 0.0 && prev == initial {
            next
        } else if velocity < 0.0 && next == initial {
            prev
        } else {
            closest(pos)
        }
    };
    let mut speed = velocity / distance.max(1.0);
    if (end - progress) * speed <= 0.0 {
        speed = ANIMATION_BASE_VELOCITY;
    }
    let points = (progress - end).abs().ceil().max(1.0);
    let max = MAX_SWIPE_MS * (1.0 + points).log2();
    let mut duration = ((progress - end) / speed * DURATION_MULTIPLIER).abs();
    if duration > 0.0 {
        duration = duration.clamp(MIN_SWIPE_MS, max);
    }
    (end, duration)
}

/// One switch, for the state snapshot and the proofs.
#[derive(Debug, Clone, PartialEq)]
pub struct SlideRecord {
    pub from: u32,
    pub to: u32,
    pub motion: Motion,
    pub gesture: bool,
    /// The last planned duration (after any retarget).
    pub duration_ms: f64,
    /// Start to settle, retargets included.
    pub elapsed_ms: f64,
    pub frames: u32,
    pub retargets: u32,
    /// Whether any drawn frame moved against the direction of travel.
    pub reversed: bool,
    /// The release velocity of a swipe (px/ms).
    pub velocity: f64,
    started: Duration,
}

impl SlideRecord {
    fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "from": self.from,
            "to": self.to,
            "motion": self.motion.name(),
            "gesture": self.gesture,
            "duration_ms": self.duration_ms.round(),
            "elapsed_ms": self.elapsed_ms.round(),
            "frames": self.frames,
            "retargets": self.retargets,
            "reversed": self.reversed,
            "velocity": self.velocity,
        })
    }
}

#[derive(Debug, Clone)]
enum Phase {
    Idle,
    Easing {
        from: f64,
        to: f64,
        start: Duration,
        duration: Duration,
    },
    Following {
        progress: f64,
        /// The closest workspace slot when the fingers went down.
        initial: f64,
        /// `_getBounds(initialProgress)`: how far the fingers may drag.
        bounds: (f64, f64),
        history: SwipeHistory,
    },
}

/// The workspace strip's drawn position and its motion.
#[derive(Debug, Clone)]
pub struct WorkspaceSlide {
    /// Workspace ids left to right while a motion runs.
    strip: Vec<u32>,
    /// Pixels from one workspace to the next.
    distance: f64,
    policy: MotionPolicy,
    phase: Phase,
    /// The active workspace when last observed.
    seen: Option<u32>,
    last_value: Option<f64>,
    /// The time the current frame draws at.
    frame: Option<Duration>,
    record: Option<SlideRecord>,
    finished: VecDeque<SlideRecord>,
    /// The motion began under the fingers: it slides under every motion
    /// policy (GNOME's swipe paths never consult Reduced Motion, and with
    /// animations off only the release is instant).
    gesture: bool,
    /// The strip position (fractional workspace id) and settle time (ms)
    /// to tell the shell, from the latest swipe phase.
    progress_out: Option<(f64, f64)>,
}

impl Default for WorkspaceSlide {
    fn default() -> Self {
        Self {
            strip: Vec::new(),
            distance: 0.0,
            policy: MotionPolicy::default(),
            phase: Phase::Idle,
            seen: None,
            last_value: None,
            frame: None,
            record: None,
            finished: VecDeque::new(),
            gesture: false,
            progress_out: None,
        }
    }
}

/// How one workspace's windows are drawn this frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub dx: i32,
    pub alpha: f32,
}

impl WorkspaceSlide {
    /// How this motion draws: a finger-driven one always slides.
    fn drawn(&self) -> Motion {
        if self.gesture {
            Motion::Slide
        } else {
            Motion::of(self.policy)
        }
    }

    /// Whether a motion is drawn (motion off draws nothing).
    pub fn running(&self) -> bool {
        self.drawn() != Motion::Off && !matches!(self.phase, Phase::Idle)
    }

    /// The latest swipe position for the shell's Activities dots, once:
    /// `(fractional workspace id, settle ms)`. GNOME 51 binds the primary
    /// monitor's swipe progress to the workspaces adjustment the dots read
    /// (workspaceAnimation.js:255-268, panel.js:144-145,178-195).
    pub fn take_progress(&mut self) -> Option<(f64, f64)> {
        self.progress_out.take()
    }

    /// Queue strip slot `slot` for the shell, as a workspace id
    /// interpolated between its neighbours (workspaceAnimation.js:257-266).
    fn publish(&mut self, slot: f64, duration_ms: f64) {
        let last = self.strip.len().saturating_sub(1);
        let (lo, hi) = (
            (slot.floor().max(0.0) as usize).min(last),
            (slot.ceil().max(0.0) as usize).min(last),
        );
        let (Some(a), Some(b)) = (self.strip.get(lo), self.strip.get(hi)) else {
            return;
        };
        let t = slot - slot.floor();
        let id = f64::from(*a) + (f64::from(*b) - f64::from(*a)) * t;
        self.progress_out = Some((id, duration_ms));
    }

    /// Whether fingers hold the strip.
    pub fn following(&self) -> bool {
        matches!(self.phase, Phase::Following { .. })
    }

    /// The drawn strip position in slots, while anything moves.
    pub fn value(&self, now: Duration) -> Option<f64> {
        match &self.phase {
            Phase::Idle => None,
            Phase::Following { progress, .. } => Some(*progress),
            Phase::Easing {
                from,
                to,
                start,
                duration,
            } => {
                let t = if duration.is_zero() {
                    1.0
                } else {
                    now.saturating_sub(*start).as_secs_f64() / duration.as_secs_f64()
                };
                Some(from + (to - from) * ease_out_cubic(t))
            }
        }
    }

    fn slot(&self, workspace: u32) -> Option<f64> {
        self.strip
            .iter()
            .position(|w| *w == workspace)
            .map(|i| i as f64)
    }

    /// Where `workspace`'s windows draw this frame, or `None` when they
    /// are out of view. Only meaningful while [`running`](Self::running).
    pub fn placement(&self, workspace: u32, now: Duration) -> Option<Placement> {
        let d = self.slot(workspace)? - self.value(now)?;
        if d.abs() >= 1.0 {
            return None;
        }
        Some(match self.drawn() {
            Motion::Fade => Placement {
                dx: 0,
                alpha: (1.0 - d.abs()) as f32,
            },
            _ => Placement {
                dx: (d * self.distance).round() as i32,
                alpha: 1.0,
            },
        })
    }

    /// [`placement`](Self::placement) at the last stepped frame.
    pub fn frame_placement(&self, workspace: u32) -> Option<Placement> {
        self.placement(workspace, self.frame?)
    }

    /// The strip for a motion: the known workspaces, `extra` and what is
    /// already laid out, in id order (ids are GNOME's order). Positions
    /// already in flight move with their workspaces.
    fn merge_strip(&mut self, workspaces: &[u32], extra: &[u32]) {
        let old = std::mem::take(&mut self.strip);
        let mut strip: Vec<u32> = old.iter().chain(workspaces).chain(extra).copied().collect();
        strip.sort_unstable();
        strip.dedup();
        let remap = |v: &mut f64| {
            let base = v.floor().max(0.0) as usize;
            if let Some(i) = old
                .get(base)
                .and_then(|id| strip.iter().position(|w| w == id))
            {
                *v += i as f64 - base as f64;
            }
        };
        match &mut self.phase {
            Phase::Easing { from, to, .. } => {
                remap(from);
                remap(to);
            }
            Phase::Following {
                progress,
                initial,
                bounds,
                ..
            } => {
                remap(progress);
                remap(initial);
                remap(&mut bounds.0);
                remap(&mut bounds.1);
            }
            Phase::Idle => {}
        }
        if let Some(v) = self.last_value.as_mut() {
            remap(v);
        }
        self.strip = strip;
    }

    fn archive(&mut self, now: Duration) {
        if let Some(mut record) = self.record.take() {
            record.elapsed_ms = now.saturating_sub(record.started).as_secs_f64() * 1000.0;
            if self.finished.len() == HISTORY_LEN {
                self.finished.pop_front();
            }
            self.finished.push_back(record);
        }
        // Fingers cut off (the overview opened, animations went off): the
        // dots return to the active workspace at once.
        if self.following() {
            if let Some(active) = self.seen {
                self.progress_out = Some((f64::from(active), 0.0));
            }
        }
        self.phase = Phase::Idle;
        self.gesture = false;
        self.last_value = None;
        self.strip.clear();
    }

    /// Track the active workspace once a frame: a change starts the
    /// switch animation or retargets one already running from where it
    /// is drawn. `suspended` (the overview is up) settles at once.
    #[allow(clippy::too_many_arguments)]
    pub fn observe(
        &mut self,
        active: u32,
        workspaces: &[u32],
        output_width: i32,
        policy: MotionPolicy,
        suspended: bool,
        now: Duration,
    ) {
        self.policy = policy;
        self.distance = f64::from(output_width + WORKSPACE_SPACING);
        let previous = self.seen.replace(active);
        if suspended {
            if !matches!(self.phase, Phase::Idle) {
                self.archive(now);
            }
            return;
        }
        // A changed active workspace retargets even while fingers hold
        // the strip (a key press mid-swipe); unchanged stays with them.
        let Some(previous) = previous.filter(|p| *p != active) else {
            return;
        };
        if Motion::of(policy) == Motion::Off {
            self.archive(now);
            self.record = Some(SlideRecord {
                from: previous,
                to: active,
                motion: Motion::Off,
                gesture: false,
                duration_ms: 0.0,
                elapsed_ms: 0.0,
                frames: 0,
                retargets: 0,
                reversed: false,
                velocity: 0.0,
                started: now,
            });
            self.archive(now);
            return;
        }
        // Retarget from where the strip is drawn now, never from where
        // the last motion began.
        self.last_value = self.value(now);
        self.merge_strip(workspaces, &[previous, active]);
        let Some(to) = self.slot(active) else {
            return;
        };
        let from = match (&self.phase, self.last_value) {
            (Phase::Easing { to: heading, .. }, _) if *heading == to => return,
            (Phase::Easing { .. } | Phase::Following { .. }, Some(shown)) => shown,
            _ => self.slot(previous).unwrap_or(to),
        };
        let duration = scaled(policy, SWITCH_MS);
        let drawn = self.drawn();
        match &mut self.record {
            Some(record) => {
                record.to = active;
                record.retargets += 1;
                record.duration_ms = duration.as_secs_f64() * 1000.0;
                record.motion = drawn;
            }
            None => {
                self.record = Some(SlideRecord {
                    from: previous,
                    to: active,
                    motion: Motion::of(policy),
                    gesture: false,
                    duration_ms: duration.as_secs_f64() * 1000.0,
                    elapsed_ms: 0.0,
                    frames: 0,
                    retargets: 0,
                    reversed: false,
                    velocity: 0.0,
                    started: now,
                });
            }
        }
        self.last_value = Some(from);
        self.phase = Phase::Easing {
            from,
            to,
            start: now,
            duration,
        };
    }

    /// Advance to `now`; returns whether a motion is still drawn.
    pub fn step(&mut self, now: Duration) -> bool {
        self.frame = Some(now);
        let Some(value) = self.value(now) else {
            return false;
        };
        if let Phase::Easing {
            to,
            start,
            duration,
            ..
        } = &self.phase
        {
            let to = *to;
            let done = now.saturating_sub(*start) >= *duration;
            if let (Some(record), Some(last)) = (self.record.as_mut(), self.last_value) {
                record.frames += 1;
                if (value - last) * (to - last).signum() < -1e-9 {
                    record.reversed = true;
                }
            }
            self.last_value = Some(value);
            if done || self.drawn() == Motion::Off {
                self.archive(now);
                return false;
            }
        } else if let Some(record) = self.record.as_mut() {
            record.frames += 1;
        }
        self.running()
    }

    /// Fingers went down: hold the strip where it is drawn (catching a
    /// switch still in flight) on the active workspace's strip. The snap
    /// points are the workspaces there are, GNOME's trailing empty one
    /// included: nothing past the last one exists to drag onto
    /// (workspaceAnimation.js:524-549 `_switchWorkspaceBegin`).
    /// `time` is the gesture's begin event time: GNOME seeds the velocity
    /// history with a zero sample then (swipeTracker.js:509-513).
    #[allow(clippy::too_many_arguments)]
    pub fn begin_swipe(
        &mut self,
        active: u32,
        workspaces: &[u32],
        output_width: i32,
        policy: MotionPolicy,
        time: u32,
        now: Duration,
    ) {
        self.policy = policy;
        self.distance = f64::from(output_width + WORKSPACE_SPACING);
        self.last_value = self.value(now);
        self.merge_strip(workspaces, &[active]);
        let progress = match self.last_value {
            Some(shown) => shown,
            None => self.slot(active).unwrap_or(0.0),
        };
        let last = self.strip.len().saturating_sub(1) as f64;
        let initial = progress.round().clamp(0.0, last);
        let bounds = swipe_bounds(progress, self.strip.len());
        let from = self.strip.get(initial as usize).copied().unwrap_or(active);
        self.gesture = true;
        if self.record.is_none() {
            self.record = Some(SlideRecord {
                from,
                to: from,
                motion: Motion::Slide,
                gesture: true,
                duration_ms: 0.0,
                elapsed_ms: 0.0,
                frames: 0,
                retargets: 0,
                reversed: false,
                velocity: 0.0,
                started: now,
            });
        } else if let Some(record) = self.record.as_mut() {
            record.gesture = true;
            record.motion = Motion::Slide;
            record.retargets += 1;
        }
        self.last_value = Some(progress);
        let mut history = SwipeHistory::default();
        history.append(time, 0.0);
        self.phase = Phase::Following {
            progress,
            initial,
            bounds,
            history,
        };
        self.publish(progress, 0.0);
    }

    /// Fingers moved `dx` px (left is towards the next workspace).
    pub fn update_swipe(&mut self, dx: f64, time: u32) {
        if let Phase::Following {
            progress,
            bounds: (low, high),
            history,
            ..
        } = &mut self.phase
        {
            let delta = -dx;
            history.append(time, delta);
            *progress = (*progress + delta / TOUCHPAD_BASE_WIDTH).clamp(*low, *high);
            let progress = *progress;
            self.last_value = Some(progress);
            self.publish(progress, 0.0);
        }
    }

    /// Fingers lifted: animate to where the swipe lands and return that
    /// workspace for the caller to activate.
    pub fn end_swipe(&mut self, cancelled: bool, time: u32, now: Duration) -> Option<u32> {
        let Phase::Following {
            progress,
            initial,
            bounds,
            history,
        } = &mut self.phase
        else {
            return None;
        };
        let velocity = history.velocity(time);
        let (progress, initial, bounds) = (*progress, *initial, *bounds);
        let (end, ms) = release_within(
            progress,
            initial,
            bounds,
            self.strip.len(),
            velocity,
            cancelled,
            TOUCHPAD_BASE_WIDTH,
        );
        let target = self.strip.get(end as usize).copied();
        let duration = scaled(self.policy, ms);
        let duration_ms = duration.as_secs_f64() * 1000.0;
        if let Some(record) = self.record.as_mut() {
            record.to = target.unwrap_or(record.to);
            record.duration_ms = duration_ms;
            record.velocity = velocity;
            record.motion = Motion::Slide;
        }
        self.phase = Phase::Easing {
            from: progress,
            to: end,
            start: now,
            duration,
        };
        self.publish(end, duration_ms);
        // Animations off: GNOME eases the release over 0 ms.
        if duration.is_zero() {
            self.archive(now);
        }
        target
    }

    /// The in-flight motion and the last finished ones, for the state
    /// snapshot.
    pub fn snapshot(&self, now: Duration) -> serde_json::Value {
        let flight = self.value(now).map(|position| {
            let (to, from) = match &self.phase {
                Phase::Easing { to, from, .. } => (*to, *from),
                Phase::Following { initial, .. } => (*initial, *initial),
                Phase::Idle => (position, position),
            };
            serde_json::json!({
                "position": position,
                // How far the destination still is from the screen.
                "target_dx": ((to - position) * self.distance).round(),
                "from_slot": from,
                "to_slot": to,
                "strip": self.strip,
                "following": self.following(),
                "record": self.record.as_ref().map(SlideRecord::json),
            })
        });
        serde_json::json!({
            "in_flight": flight,
            "history": self.finished.iter().map(SlideRecord::json).collect::<Vec<_>>(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuna_shell_control::MotionLevel;

    fn policy(level: MotionLevel, slowdown: f64) -> MotionPolicy {
        MotionPolicy::new(level, slowdown)
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn ease_out_cubic_matches_clutter() {
        assert_eq!(ease_out_cubic(0.0), 0.0);
        assert_eq!(ease_out_cubic(1.0), 1.0);
        assert!((ease_out_cubic(0.5) - 0.875).abs() < 1e-12);
        assert_eq!(ease_out_cubic(2.0), 1.0);
    }

    #[test]
    fn a_key_switch_slides_both_workspaces_for_250_ms() {
        let t0 = Duration::from_secs(10);
        let mut slide = WorkspaceSlide::default();
        let on = MotionPolicy::default();
        slide.observe(0, &[0], 1000, on, false, t0);
        assert!(!slide.running());
        slide.observe(1, &[0, 1], 1000, on, false, t0);
        assert!(slide.running());
        // At the start the old workspace fills the screen, the new one
        // waits just off it, one output plus 100px to the right.
        assert_eq!(slide.placement(0, t0).unwrap().dx, 0);
        assert!(slide.placement(1, t0).is_none());
        let half = t0 + ms(125);
        let d0 = slide.placement(0, half).unwrap().dx;
        assert_eq!(d0, -(0.875f64 * 1100.0).round() as i32);
        assert!((slide.placement(1, half).unwrap().dx - (1100 + d0)).abs() <= 1);
        assert!(slide.step(half));
        assert!(!slide.step(t0 + ms(250)));
        assert!(!slide.running());
        let history = slide.snapshot(t0 + ms(250))["history"].clone();
        assert_eq!(history[0]["duration_ms"], 250.0);
        assert_eq!(history[0]["elapsed_ms"], 250.0);
        assert_eq!(history[0]["reversed"], false);
    }

    #[test]
    fn a_second_press_retargets_from_the_drawn_offset() {
        let t0 = Duration::from_secs(10);
        let mut slide = WorkspaceSlide::default();
        let on = MotionPolicy::default();
        slide.observe(0, &[0], 1000, on, false, t0);
        slide.observe(1, &[0, 1], 1000, on, false, t0);
        let mid = t0 + ms(60);
        slide.step(mid);
        let shown = slide.value(mid).unwrap();
        assert!(shown > 0.0 && shown < 1.0);
        slide.observe(2, &[0, 1, 2], 1000, on, false, mid);
        assert!((slide.value(mid).unwrap() - shown).abs() < 1e-9);
        let mut last = shown;
        for step in 1..=25 {
            let now = mid + ms(step * 10);
            slide.step(now);
            if let Some(v) = slide.value(now) {
                assert!(v >= last);
                last = v;
            }
        }
        let record = &slide.snapshot(mid + ms(250))["history"][0];
        assert_eq!(record["from"], 0);
        assert_eq!(record["to"], 2);
        assert_eq!(record["retargets"], 1);
        assert_eq!(record["reversed"], false);
    }

    #[test]
    fn motion_off_switches_instantly_and_fade_crossfades_in_place() {
        let t0 = Duration::from_secs(10);
        let mut slide = WorkspaceSlide::default();
        slide.observe(0, &[0], 1000, policy(MotionLevel::Off, 1.0), false, t0);
        slide.observe(1, &[0, 1], 1000, policy(MotionLevel::Off, 1.0), false, t0);
        assert!(!slide.running());
        assert_eq!(slide.snapshot(t0)["history"][0]["motion"], "off");
        let fade = policy(MotionLevel::FadeOnly, 1.0);
        slide.observe(2, &[0, 1, 2], 1000, fade, false, t0);
        let mid = t0 + ms(125);
        let out = slide.placement(1, mid).unwrap();
        let incoming = slide.placement(2, mid).unwrap();
        assert_eq!((out.dx, incoming.dx), (0, 0));
        assert!((out.alpha + incoming.alpha - 1.0).abs() < 1e-6);
        assert!(incoming.alpha > 0.8);
    }

    #[test]
    fn the_slowdown_factor_stretches_the_switch() {
        let t0 = Duration::from_secs(10);
        let mut slide = WorkspaceSlide::default();
        let slow = policy(MotionLevel::Full, 2.0);
        slide.observe(0, &[0], 1000, slow, false, t0);
        slide.observe(1, &[0, 1], 1000, slow, false, t0);
        assert!(slide.step(t0 + ms(300)));
        assert!(!slide.step(t0 + ms(500)));
    }

    #[test]
    fn the_overview_settles_a_running_switch() {
        let t0 = Duration::from_secs(10);
        let mut slide = WorkspaceSlide::default();
        let on = MotionPolicy::default();
        slide.observe(0, &[0], 1000, on, false, t0);
        slide.observe(1, &[0, 1], 1000, on, false, t0);
        slide.observe(1, &[0, 1], 1000, on, true, t0 + ms(10));
        assert!(!slide.running());
    }

    #[test]
    fn velocity_is_the_last_150_ms_of_travel() {
        let mut history = SwipeHistory::default();
        history.append(1000, 10.0);
        history.append(1010, 20.0);
        history.append(1020, 20.0);
        assert_eq!(history.velocity(1020), 2.0);
        // A pause before lifting the fingers drops the old samples.
        assert_eq!(history.velocity(1400), 0.0);
    }

    #[test]
    fn slow_releases_snap_to_the_nearer_workspace() {
        // Under half way and slow: back where it began, 3x the base speed.
        let (end, duration) = swipe_release(0.3, 0.0, 3, 0.0, false);
        assert_eq!(end, 0.0);
        assert_eq!(duration, 400.0f64.min(0.3 / ANIMATION_BASE_VELOCITY * 3.0));
        let (end, duration) = swipe_release(0.6, 0.0, 3, 0.0, false);
        assert_eq!(end, 1.0);
        assert_eq!(duration, 400.0);
        let (end, _) = swipe_release(0.9, 0.0, 3, 0.0, true);
        assert_eq!(end, 0.0);
    }

    #[test]
    fn a_flick_completes_faster_than_a_drag() {
        // A short fast flick still lands one workspace over...
        let (end, fast) = swipe_release(0.1, 0.0, 3, 5.0, false);
        assert_eq!(end, 1.0);
        // ...and a faster one finishes sooner, never under 100 ms.
        let (_, faster) = swipe_release(0.1, 0.0, 3, 12.0, false);
        assert!(faster < fast);
        assert!(faster >= MIN_SWIPE_MS);
        assert!(fast <= MAX_SWIPE_MS);
        let (_, drag) = swipe_release(0.6, 0.0, 3, 0.0, false);
        assert!(fast < drag);
        // A flick back towards the start returns there.
        let (end, _) = swipe_release(1.4, 1.0, 3, -1.0, false);
        assert_eq!(end, 1.0);
        // Never past the neighbouring workspace.
        let (end, _) = swipe_release(1.0, 1.0, 5, 50.0, false);
        assert_eq!(end, 2.0);
    }

    #[test]
    fn a_swipe_follows_the_fingers_then_finishes_on_its_velocity() {
        let t0 = Duration::from_secs(10);
        let mut slide = WorkspaceSlide::default();
        let on = MotionPolicy::default();
        // Dynamic workspaces: the occupied one and GNOME's trailing empty one.
        slide.observe(0, &[0, 1], 1000, on, false, t0);
        slide.begin_swipe(0, &[0, 1], 1000, on, 990, t0);
        slide.update_swipe(-100.0, 1000);
        assert_eq!(slide.value(t0), Some(0.25));
        assert_eq!(slide.placement(1, t0).unwrap().dx, 825);
        slide.update_swipe(-40.0, 1010);
        slide.update_swipe(-40.0, 1020);
        let target = slide.end_swipe(false, 1020, t0);
        assert_eq!(target, Some(1));
        // The model follows; the observed switch keeps the swipe's timing.
        slide.observe(1, &[0, 1], 1000, on, false, t0);
        let record = slide.snapshot(t0)["in_flight"]["record"].clone();
        assert_eq!(record["gesture"], true);
        // 180px over 30 ms from the begin sample: 6 px/ms.
        assert_eq!(record["velocity"], 6.0);
        assert!(record["duration_ms"].as_f64().unwrap() < 250.0);
        assert!(!slide.step(t0 + ms(400)));
    }

    #[test]
    fn the_begin_sample_counts_the_first_movement_towards_the_velocity() {
        let t0 = Duration::from_secs(10);
        let on = MotionPolicy::default();
        let mut slide = WorkspaceSlide::default();
        slide.observe(0, &[0, 1], 1000, on, false, t0);
        // swipeTracker.js:509-513: (begin, 0), then 40px 8 ms later.
        slide.begin_swipe(0, &[0, 1], 1000, on, 1000, t0);
        slide.update_swipe(-40.0, 1008);
        assert_eq!(slide.end_swipe(false, 1008, t0), Some(1));
        assert_eq!(slide.snapshot(t0)["in_flight"]["record"]["velocity"], 5.0);
    }

    #[test]
    fn a_swipe_cannot_drag_past_the_last_workspace() {
        let t0 = Duration::from_secs(10);
        let on = MotionPolicy::default();
        // On GNOME's trailing empty workspace (dynamic) or the last fixed
        // one, the strip holds at the edge: there is nothing to drag onto.
        for workspaces in [&[0, 1][..], &[0, 1, 2][..]] {
            let last = *workspaces.last().unwrap();
            let mut slide = WorkspaceSlide::default();
            slide.observe(last, workspaces, 1000, on, false, t0);
            slide.begin_swipe(last, workspaces, 1000, on, 1000, t0);
            slide.update_swipe(-300.0, 1010);
            slide.update_swipe(-300.0, 1020);
            assert_eq!(slide.value(t0), Some(f64::from(last)));
            assert_eq!(slide.end_swipe(false, 1020, t0), Some(last));
        }
    }

    #[test]
    fn swipe_bounds_follow_gnomes_get_bounds() {
        // On a workspace: one either side.
        assert_eq!(swipe_bounds(1.0, 4), (0.0, 2.0));
        assert_eq!(swipe_bounds(1.004, 4), (0.0, 2.0));
        assert_eq!(swipe_bounds(0.0, 4), (0.0, 1.0));
        assert_eq!(swipe_bounds(3.0, 4), (2.0, 3.0));
        // Caught between two: one past either of them.
        assert_eq!(swipe_bounds(1.5, 4), (0.0, 3.0));
        assert_eq!(swipe_bounds(2.4, 5), (1.0, 4.0));
    }

    #[test]
    fn a_held_swipe_slides_under_every_motion_policy() {
        let t0 = Duration::from_secs(10);
        for level in [MotionLevel::Full, MotionLevel::FadeOnly, MotionLevel::Off] {
            let p = policy(level, 1.0);
            let mut slide = WorkspaceSlide::default();
            slide.observe(0, &[0, 1], 1000, p, false, t0);
            slide.begin_swipe(0, &[0, 1], 1000, p, 1000, t0);
            slide.update_swipe(-100.0, 1010);
            assert!(slide.running(), "{level:?}");
            assert_eq!(
                slide.placement(1, t0),
                Some(Placement {
                    dx: 825,
                    alpha: 1.0
                }),
                "{level:?}"
            );
            // A slow release under half way goes back: it slides back
            // unless animations are off, which settles it at once.
            assert_eq!(slide.end_swipe(false, 1500, t0), Some(0));
            assert_eq!(slide.running(), level != MotionLevel::Off, "{level:?}");
            if level == MotionLevel::FadeOnly {
                let mid = slide.placement(0, t0 + ms(10)).unwrap();
                assert!(mid.dx < 0 && mid.alpha == 1.0);
            }
        }
    }

    #[test]
    fn swipe_phases_tell_the_dots_where_the_strip_is() {
        let t0 = Duration::from_secs(10);
        let on = MotionPolicy::default();
        let mut slide = WorkspaceSlide::default();
        slide.observe(1, &[0, 1, 2], 1000, on, false, t0);
        assert_eq!(slide.take_progress(), None);
        slide.begin_swipe(1, &[0, 1, 2], 1000, on, 1000, t0);
        assert_eq!(slide.take_progress(), Some((1.0, 0.0)));
        slide.update_swipe(-100.0, 1010);
        assert_eq!(slide.take_progress(), Some((1.25, 0.0)));
        assert_eq!(slide.take_progress(), None);
        slide.update_swipe(-100.0, 1020);
        let target = slide.end_swipe(false, 1020, t0);
        assert_eq!(target, Some(2));
        let (position, duration) = slide.take_progress().unwrap();
        assert_eq!(position, 2.0);
        let record = slide.snapshot(t0)["in_flight"]["record"].clone();
        assert_eq!(duration.round(), record["duration_ms"].as_f64().unwrap());
        // Ordinary frames and key switches never publish.
        slide.observe(2, &[0, 1, 2], 1000, on, false, t0);
        slide.step(t0 + ms(50));
        assert_eq!(slide.take_progress(), None);
        // The overview cutting a held swipe off returns the dots home.
        slide.step(t0 + ms(1000));
        slide.begin_swipe(2, &[0, 1, 2], 1000, on, 3000, t0 + ms(1000));
        slide.update_swipe(150.0, 3010);
        assert_eq!(slide.take_progress(), Some((1.625, 0.0)));
        slide.observe(2, &[0, 1, 2], 1000, on, true, t0 + ms(1010));
        assert_eq!(slide.take_progress(), Some((2.0, 0.0)));
        assert!(!slide.running());
    }

    #[test]
    fn a_swipe_catches_a_switch_in_flight() {
        let t0 = Duration::from_secs(10);
        let mut slide = WorkspaceSlide::default();
        let on = MotionPolicy::default();
        slide.observe(0, &[0], 1000, on, false, t0);
        slide.observe(1, &[0, 1], 1000, on, false, t0);
        let mid = t0 + ms(50);
        slide.step(mid);
        let shown = slide.value(mid).unwrap();
        slide.begin_swipe(1, &[0, 1], 1000, on, 1000, mid);
        assert!((slide.value(mid).unwrap() - shown).abs() < 1e-9);
        assert!(slide.following());
    }

    #[test]
    fn a_press_mid_swipe_retargets_from_the_drawn_offset() {
        // Direct observe() coverage in this tree's test style (the
        // WindowManager fixed-count driver lives on the feature stack;
        // production reaches observe once per tick via
        // Runtime::step_workspace_slide).
        let t0 = Duration::from_secs(10);
        let on = MotionPolicy::default();
        let mut slide = WorkspaceSlide::default();

        // The first press starts an easing slide 0 -> 1.
        slide.observe(0, &[0], 1000, on, false, t0);
        slide.step(t0);
        slide.observe(1, &[0, 1], 1000, on, false, t0);
        slide.step(t0);
        assert!(slide.running());
        assert!(!slide.following());

        // Fingers catch the switch mid-flight and drag it partway. An
        // unchanged frame while following stays with the fingers.
        let mid = t0 + ms(60);
        slide.observe(1, &[0, 1], 1000, on, false, mid);
        slide.step(mid);
        slide.begin_swipe(1, &[0, 1], 1000, on, 990, mid);
        slide.update_swipe(-100.0, 1000);
        let dragged = slide.value(mid).unwrap();
        slide.observe(1, &[0, 1], 1000, on, false, mid);
        slide.step(mid);
        assert!(slide.following());
        assert_eq!(slide.value(mid), Some(dragged));

        // A rapid second press retargets from the drawn offset instead
        // of being swallowed by the following early return.
        slide.observe(2, &[0, 1, 2], 1000, on, false, mid);
        slide.step(mid);
        assert!(!slide.following());
        assert!((slide.value(mid).unwrap() - dragged).abs() < 1e-9);
        let record = slide.snapshot(mid)["in_flight"]["record"].clone();
        assert_eq!(record["to"], 2);
        assert_eq!(record["retargets"], 2);

        // A third rapid press retargets again from further along.
        let later = mid + ms(60);
        slide.observe(2, &[0, 1, 2], 1000, on, false, later);
        slide.step(later);
        slide.observe(3, &[0, 1, 2, 3], 1000, on, false, later);
        slide.step(later);
        let record = slide.snapshot(later)["in_flight"]["record"].clone();
        assert_eq!(record["to"], 3);
        assert_eq!(record["retargets"], 3);

        // The motion runs to completion without ever moving backwards.
        // (Unchanged observes are no-ops, so step() alone advances it.)
        for step in 1..=40 {
            slide.step(later + ms(step * 10));
        }
        assert!(!slide.running());
        let done = later + ms(400);
        let history = slide.snapshot(done)["history"].clone();
        assert_eq!(history[0]["from"], 0);
        assert_eq!(history[0]["to"], 3);
        assert_eq!(history[0]["retargets"], 3);
        assert_eq!(history[0]["gesture"], true);
        assert_eq!(history[0]["reversed"], false);
    }
}
