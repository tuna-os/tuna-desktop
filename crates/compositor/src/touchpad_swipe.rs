//! GNOME 51's touchpad swipe recognition for the shell's gestures
//! (`TouchpadSwipeGesture`, js/ui/swipeTracker.js:89-209 at 51.0).
//!
//! A touchpad swipe of three or more fingers belongs to the shell; fewer
//! propagate (swipeTracker.js:35, 127). Nothing moves until the fingers'
//! unaccelerated travel (:142) reaches `DRAG_THRESHOLD_DISTANCE`; its
//! dominant axis then picks the one tracker that takes the gesture
//! (:153-179): vertical drives the overview (overview.js:226-233),
//! horizontal the workspaces (workspaceAnimation.js:374-380 on the
//! desktop, workspacesView.js:868-876 in the overview). The travel below
//! the threshold is dropped (:166-167); the event that crosses it is the
//! first update. Deltas come out with GNOME's sign, positive moving the
//! tracker forward (overview open, next workspace): the fingers' motion
//! negated with natural scrolling on, as it is (:188-189), so turning
//! natural scrolling off reverses every gesture. A libinput cancel ends
//! the gesture exactly as lifting the fingers does (:194-198).
//!
//! Tuna Desktop consumes the pending phase too: GNOME lets those first
//! events propagate, but with a tracker for each axis every such swipe
//! ends up the shell's in the session's normal and overview modes.

/// `GESTURE_FINGER_COUNT`: the fewest fingers a shell swipe takes.
pub const GESTURE_FINGER_COUNT: u32 = 3;
/// `DRAG_THRESHOLD_DISTANCE`: unaccelerated travel that commits a swipe
/// to an axis.
pub const DRAG_THRESHOLD_DISTANCE: f64 = 16.0;
/// `TOUCHPAD_BASE_HEIGHT` (swipeTracker.js:13): vertical travel for one
/// whole step of a tracker (hidden to open for the overview).
pub const TOUCHPAD_BASE_HEIGHT: f64 = 300.0;

/// The tracker axis a swipe committed to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Orientation {
    Horizontal,
    Vertical,
}

impl Orientation {
    pub fn name(self) -> &'static str {
        match self {
            Orientation::Horizontal => "horizontal",
            Orientation::Vertical => "vertical",
        }
    }
}

/// What one update of a shell swipe does.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Step {
    /// Under the drag threshold: held by the shell, nothing moves yet.
    Pending,
    /// The `orientation` tracker moves by `delta` (GNOME's sign);
    /// `begin` on the event that committed the swipe to that axis.
    Update {
        orientation: Orientation,
        delta: f64,
        begin: bool,
    },
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
enum State {
    /// No swipe, or one with too few fingers: not the shell's.
    #[default]
    Idle,
    Pending {
        x: f64,
        y: f64,
    },
    Handling(Orientation),
}

/// One touchpad swipe as the shell's trackers see it.
#[derive(Debug, Default, Clone)]
pub struct TouchpadSwipe {
    state: State,
    fingers: u32,
}

impl TouchpadSwipe {
    /// A swipe began with `fingers`; whether it is the shell's.
    pub fn begin(&mut self, fingers: u32) -> bool {
        self.fingers = fingers;
        self.state = if fingers >= GESTURE_FINGER_COUNT {
            State::Pending { x: 0.0, y: 0.0 }
        } else {
            State::Idle
        };
        self.active()
    }

    /// Whether the shell holds a swipe.
    pub fn active(&self) -> bool {
        self.state != State::Idle
    }

    /// The axis the held swipe committed to.
    pub fn orientation(&self) -> Option<Orientation> {
        match self.state {
            State::Handling(orientation) => Some(orientation),
            _ => None,
        }
    }

    /// The fingers moved `(dx, dy)`, unaccelerated (right and down
    /// positive). `None` when the swipe is not the shell's.
    pub fn update(&mut self, dx: f64, dy: f64, natural_scroll: bool) -> Option<Step> {
        let (orientation, begin) = match &mut self.state {
            State::Idle => return None,
            State::Handling(orientation) => (*orientation, false),
            State::Pending { x, y } => {
                *x += dx;
                *y += dy;
                if x.hypot(*y) < DRAG_THRESHOLD_DISTANCE {
                    return Some(Step::Pending);
                }
                let orientation = if x.abs() > y.abs() {
                    Orientation::Horizontal
                } else {
                    Orientation::Vertical
                };
                self.state = State::Handling(orientation);
                (orientation, true)
            }
        };
        let motion = match orientation {
            Orientation::Horizontal => dx,
            Orientation::Vertical => dy,
        };
        Some(Step::Update {
            orientation,
            delta: if natural_scroll { -motion } else { motion },
            begin,
        })
    }

    /// The fingers lifted or libinput cancelled. `None` when the swipe
    /// was not the shell's, else the axis it committed to (`Some(None)`
    /// when it never left the threshold).
    pub fn end(&mut self) -> Option<Option<Orientation>> {
        let ended = match std::mem::take(&mut self.state) {
            State::Idle => None,
            State::Pending { .. } => Some(None),
            State::Handling(orientation) => Some(Some(orientation)),
        };
        self.fingers = 0;
        ended
    }

    /// The held swipe for the state snapshot.
    pub fn snapshot(&self) -> serde_json::Value {
        serde_json::json!({
            "active": self.active(),
            "fingers": self.active().then_some(self.fingers),
            "orientation": self.orientation().map(Orientation::name),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn update(delta: f64, begin: bool, orientation: Orientation) -> Option<Step> {
        Some(Step::Update {
            orientation,
            delta,
            begin,
        })
    }

    #[test]
    fn three_or_more_fingers_are_the_shells_and_fewer_propagate() {
        let mut swipe = TouchpadSwipe::default();
        assert!(!swipe.begin(2));
        assert_eq!(swipe.update(0.0, -40.0, true), None);
        assert_eq!(swipe.end(), None);
        for fingers in [3, 4, 5] {
            assert!(swipe.begin(fingers), "{fingers} fingers");
            assert!(swipe.active());
            assert_eq!(
                swipe.update(0.0, -40.0, true),
                update(40.0, true, Orientation::Vertical),
                "{fingers} fingers"
            );
            assert_eq!(swipe.end(), Some(Some(Orientation::Vertical)));
            assert!(!swipe.active());
        }
    }

    #[test]
    fn travel_under_the_drag_threshold_moves_nothing_and_is_dropped() {
        let mut swipe = TouchpadSwipe::default();
        swipe.begin(3);
        assert_eq!(swipe.update(-6.0, -4.0, true), Some(Step::Pending));
        assert_eq!(swipe.update(-6.0, 2.0, true), Some(Step::Pending));
        assert_eq!(swipe.orientation(), None);
        // 18.2 units of travel, mostly leftward: horizontal from here on,
        // the crossing event's own motion first, vertical motion ignored.
        assert_eq!(
            swipe.update(-6.0, -1.0, true),
            update(6.0, true, Orientation::Horizontal)
        );
        assert_eq!(
            swipe.update(-3.0, -50.0, true),
            update(3.0, false, Orientation::Horizontal)
        );
        // Lifted before the threshold: the shell's, but nothing to end.
        swipe.begin(3);
        swipe.update(0.0, 10.0, true);
        assert_eq!(swipe.end(), Some(None));
    }

    #[test]
    fn an_exact_diagonal_commits_to_the_vertical_tracker() {
        // `Math.abs(cdx) > Math.abs(cdy)` is strict (swipeTracker.js:162).
        let mut swipe = TouchpadSwipe::default();
        swipe.begin(3);
        assert_eq!(
            swipe.update(12.0, 12.0, true),
            update(-12.0, true, Orientation::Vertical)
        );
    }

    #[test]
    fn natural_scrolling_off_reverses_the_gestures() {
        let mut natural = TouchpadSwipe::default();
        let mut traditional = TouchpadSwipe::default();
        natural.begin(3);
        traditional.begin(3);
        // Fingers up: forward (open) with natural scrolling, back without.
        assert_eq!(
            natural.update(0.0, -20.0, true),
            update(20.0, true, Orientation::Vertical)
        );
        assert_eq!(
            traditional.update(0.0, -20.0, false),
            update(-20.0, true, Orientation::Vertical)
        );
        natural.begin(4);
        traditional.begin(4);
        // Fingers right: the previous workspace, or the next one.
        assert_eq!(
            natural.update(20.0, 0.0, true),
            update(-20.0, true, Orientation::Horizontal)
        );
        assert_eq!(
            traditional.update(20.0, 0.0, false),
            update(20.0, true, Orientation::Horizontal)
        );
    }
}
