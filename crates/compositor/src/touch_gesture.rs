//! GNOME Shell 51's touchscreen gestures, recognized in the compositor
//! (P-DS-04, #345).
//!
//! GNOME Shell puts these on its stage, in the capture phase, so they see
//! every touch before any client does: the three-finger `Clutter.PanGesture`
//! of the overview (vertical, `overview.js`) and of workspace switching
//! (horizontal, `workspaceAnimation.js`), both from `swipeTracker.js`, and
//! the keyboard's bottom `Shell.EdgeDragGesture` (`keyboard.js`,
//! `shell-edge-drag-gesture.c`). Until one recognizes, the touches reach
//! the client under them as usual; recognizing claims every sequence, which
//! Mutter turns into `wl_touch.cancel` for the client
//! (`meta_wayland_touch_cancel`). This module is the pure recognizer: the
//! runtime feeds it each touch and acts on what it reports.

use std::collections::BTreeMap;

use smithay::utils::{Logical, Point, Rectangle};

/// `swipeTracker.js` `GESTURE_FINGER_COUNT`: the pan's min and max points.
pub const PAN_FINGERS: usize = 3;
/// `clutter-pan-gesture.c` `DEFAULT_BEGIN_THRESHOLD_PX`, along the axis.
pub const PAN_BEGIN_THRESHOLD: f64 = 16.0;
/// `shell-edge-drag-gesture.c`: how near the monitor edge a drag begins,
pub const EDGE_THRESHOLD: f64 = 20.0;
/// how far from that edge it completes,
pub const EDGE_DRAG_DISTANCE: f64 = 80.0;
/// the sideways travel that cancels it,
pub const EDGE_CANCEL_THRESHOLD: f64 = 100.0;
/// and how long it may rest in the edge zone.
pub const EDGE_CANCEL_TIMEOUT_MS: u32 = 200;

/// A recognized gesture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gesture {
    /// Three fingers vertically: the overview follows them.
    OverviewSwipe,
    /// Three fingers horizontally on the desktop: workspaces follow them.
    WorkspaceSwipe,
    /// One finger up from the bottom edge: the on-screen keyboard.
    OskEdgeDrag,
}

impl Gesture {
    pub fn name(self) -> &'static str {
        match self {
            Gesture::OverviewSwipe => "overview-swipe",
            Gesture::WorkspaceSwipe => "workspace-swipe",
            Gesture::OskEdgeDrag => "osk-edge-drag",
        }
    }
}

/// Which gestures the session's action mode allows now (each tracker's
/// `allowedModes` and the edge drag's `may-recognize`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Allowed {
    /// `Shell.ActionMode.NORMAL | OVERVIEW`.
    pub overview_swipe: bool,
    /// `Shell.ActionMode.NORMAL`.
    pub workspace_swipe: bool,
    /// `ALL & ~LOCK_SCREEN`, and the keyboard not already shown.
    pub osk_edge_drag: bool,
}

/// What the recognizer reports for one touch event.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Output {
    /// The gesture recognized: every client sequence is cancelled and
    /// the touches belong to the compositor until all fingers lift.
    Claim(Gesture),
    /// Pan motion in logical pixels along the recognized axis (the other
    /// axis is zero), as `pan-update`'s `get_delta_abs`.
    PanUpdate {
        delta: Point<f64, Logical>,
        time: u32,
    },
    /// The pan ended: lifted (`cancelled` false) or cancelled.
    PanEnd { cancelled: bool, time: u32 },
    /// The edge drag's `progress`: distance from where it began.
    EdgeProgress { distance: f64 },
    /// The edge drag completed (`end`).
    EdgeActivate,
    /// The recognized edge drag was cancelled (`cancel`).
    EdgeCancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Axis {
    X,
    Y,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Pan {
    /// Fewer than three points so far.
    Waiting,
    /// Three points; `use_slot` (the latest) drives the threshold.
    Possible {
        use_slot: u32,
        total: Point<f64, Logical>,
    },
    Recognized {
        axis: Axis,
        use_slot: u32,
    },
    /// Cancelled or completed: idle until every point lifts.
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Edge {
    Possible {
        slot: u32,
        begin: Point<f64, Logical>,
        begin_time: u32,
        monitor: Rectangle<i32, Logical>,
    },
    Recognizing {
        slot: u32,
        begin: Point<f64, Logical>,
        monitor: Rectangle<i32, Logical>,
    },
    Done,
}

/// The touchscreen gestures of one seat.
#[derive(Debug, Clone)]
pub struct TouchGestures {
    points: BTreeMap<u32, Point<f64, Logical>>,
    pan: Pan,
    edge: Edge,
    claimed: Option<Gesture>,
}

impl Default for TouchGestures {
    fn default() -> Self {
        Self {
            points: BTreeMap::new(),
            pan: Pan::Waiting,
            edge: Edge::Done,
            claimed: None,
        }
    }
}

fn bottom(monitor: Rectangle<i32, Logical>) -> f64 {
    f64::from(monitor.loc.y) + f64::from(monitor.size.h)
}

/// `is_near_monitor_edge` for `ST_SIDE_BOTTOM`.
fn near_bottom(monitor: Rectangle<i32, Logical>, pos: Point<f64, Logical>) -> bool {
    pos.y > bottom(monitor) - EDGE_THRESHOLD
}

/// `get_monitor_for_coords`: the monitor holding the 1x1 rectangle up and
/// left of the point.
fn monitor_at(
    monitors: &[Rectangle<i32, Logical>],
    pos: Point<f64, Logical>,
) -> Option<Rectangle<i32, Logical>> {
    let probe = Rectangle::<i32, Logical>::new(
        ((pos.x - 1.0).floor() as i32, (pos.y - 1.0).floor() as i32).into(),
        (1, 1).into(),
    );
    monitors
        .iter()
        .copied()
        .find(|monitor| monitor.overlaps(probe))
        .or_else(|| {
            monitors
                .iter()
                .copied()
                .find(|monitor| monitor.to_f64().contains(pos))
        })
}

impl TouchGestures {
    /// The gesture owning the touches, if one recognized: their events
    /// are the compositor's until every finger lifts.
    pub fn claimed(&self) -> Option<Gesture> {
        self.claimed
    }

    /// Fingers down now.
    pub fn points(&self) -> usize {
        self.points.len()
    }

    fn claim(&mut self, gesture: Gesture, out: &mut Vec<Output>) {
        if self.claimed.is_none() {
            self.claimed = Some(gesture);
            out.push(Output::Claim(gesture));
        }
    }

    /// A finger touched down at `pos` (logical); `monitors` are the
    /// outputs' logical rectangles.
    pub fn down(
        &mut self,
        slot: u32,
        pos: Point<f64, Logical>,
        time: u32,
        monitors: &[Rectangle<i32, Logical>],
        allowed: Allowed,
    ) -> Vec<Output> {
        let mut out = Vec::new();
        if self.points.is_empty() {
            // Every gesture waits again once all points ended.
            self.pan = Pan::Waiting;
            self.claimed = None;
            self.edge = match monitor_at(monitors, pos) {
                Some(monitor) if allowed.osk_edge_drag && near_bottom(monitor, pos) => {
                    Edge::Possible {
                        slot,
                        begin: pos,
                        begin_time: time,
                        monitor,
                    }
                }
                _ => Edge::Done,
            };
        } else {
            // `point_began` with more than one point.
            if let Edge::Recognizing { .. } = self.edge {
                out.push(Output::EdgeCancel);
            }
            self.edge = Edge::Done;
        }
        self.points.insert(slot, pos);
        let n = self.points.len();
        match self.pan {
            Pan::Waiting | Pan::Possible { .. } if n > PAN_FINGERS => self.pan = Pan::Done,
            Pan::Waiting if n == PAN_FINGERS => {
                self.pan = Pan::Possible {
                    use_slot: slot,
                    total: Point::default(),
                };
            }
            Pan::Possible { total, .. } => {
                self.pan = Pan::Possible {
                    use_slot: slot,
                    total,
                };
            }
            _ => {}
        }
        out
    }

    /// A finger moved to `pos`.
    pub fn motion(
        &mut self,
        slot: u32,
        pos: Point<f64, Logical>,
        time: u32,
        allowed: Allowed,
    ) -> Vec<Output> {
        let mut out = Vec::new();
        let Some(previous) = self.points.insert(slot, pos) else {
            // Never seen down (it began before the recognizer did).
            self.points.remove(&slot);
            return out;
        };
        let delta = pos - previous;
        self.edge_motion(slot, pos, time, &mut out);
        match self.pan {
            Pan::Possible { use_slot, total } if use_slot == slot => {
                let total = total + delta;
                self.pan = Pan::Possible { use_slot, total };
                let vertical = allowed.overview_swipe && total.y.abs() >= PAN_BEGIN_THRESHOLD;
                let horizontal = allowed.workspace_swipe && total.x.abs() >= PAN_BEGIN_THRESHOLD;
                let axis = match (vertical, horizontal) {
                    (true, true) if total.x.abs() > total.y.abs() => Some(Axis::X),
                    (true, _) => Some(Axis::Y),
                    (false, true) => Some(Axis::X),
                    (false, false) => None,
                };
                if let Some(axis) = axis {
                    if self.claimed.is_none() && self.points.len() == PAN_FINGERS {
                        self.pan = Pan::Recognized { axis, use_slot };
                        self.claim(
                            match axis {
                                Axis::Y => Gesture::OverviewSwipe,
                                Axis::X => Gesture::WorkspaceSwipe,
                            },
                            &mut out,
                        );
                        out.push(Output::PanUpdate {
                            delta: along(axis, delta),
                            time,
                        });
                    }
                }
            }
            Pan::Recognized { axis, use_slot } if use_slot == slot => {
                out.push(Output::PanUpdate {
                    delta: along(axis, delta),
                    time,
                });
            }
            _ => {}
        }
        out
    }

    fn edge_motion(
        &mut self,
        slot: u32,
        pos: Point<f64, Logical>,
        time: u32,
        out: &mut Vec<Output>,
    ) {
        match self.edge {
            Edge::Possible {
                slot: edge_slot,
                begin,
                begin_time,
                monitor,
            } if edge_slot == slot => {
                // The cancel timeout fired while it rested in the zone
                // (still Possible means it never left).
                if time.wrapping_sub(begin_time) >= EDGE_CANCEL_TIMEOUT_MS
                    || (pos.x - begin.x).abs() > EDGE_CANCEL_THRESHOLD
                {
                    self.edge = Edge::Done;
                    return;
                }
                if !near_bottom(monitor, pos) && self.claimed.is_none() {
                    self.edge = Edge::Recognizing {
                        slot,
                        begin,
                        monitor,
                    };
                    self.claim(Gesture::OskEdgeDrag, out);
                    self.edge_progress(pos, out);
                }
            }
            Edge::Recognizing {
                slot: edge_slot,
                begin,
                ..
            } if edge_slot == slot => {
                if (pos.x - begin.x).abs() > EDGE_CANCEL_THRESHOLD {
                    self.edge = Edge::Done;
                    out.push(Output::EdgeCancel);
                    return;
                }
                self.edge_progress(pos, out);
            }
            _ => {}
        }
    }

    fn edge_progress(&mut self, pos: Point<f64, Logical>, out: &mut Vec<Output>) {
        let Edge::Recognizing { begin, monitor, .. } = self.edge else {
            return;
        };
        out.push(Output::EdgeProgress {
            distance: (pos.y - begin.y).abs(),
        });
        if pos.y < bottom(monitor) - EDGE_DRAG_DISTANCE {
            self.edge = Edge::Done;
            out.push(Output::EdgeActivate);
        }
    }

    /// A finger lifted.
    pub fn up(&mut self, slot: u32, time: u32) -> Vec<Output> {
        let mut out = Vec::new();
        if self.points.remove(&slot).is_none() {
            return out;
        }
        match self.edge {
            Edge::Possible { slot: edge, .. } if edge == slot => self.edge = Edge::Done,
            Edge::Recognizing { slot: edge, .. } if edge == slot => {
                self.edge = Edge::Done;
                out.push(Output::EdgeCancel);
            }
            _ => {}
        }
        let n = self.points.len();
        match self.pan {
            Pan::Possible { use_slot, total } => {
                if n >= PAN_FINGERS {
                    let next = self.points.keys().copied().find(|s| *s != use_slot);
                    self.pan = Pan::Possible {
                        use_slot: if use_slot == slot {
                            next.unwrap_or(use_slot)
                        } else {
                            use_slot
                        },
                        total,
                    };
                } else {
                    self.pan = Pan::Done;
                }
            }
            Pan::Recognized { axis, use_slot } => {
                if n >= PAN_FINGERS {
                    if use_slot == slot {
                        let next = self.points.keys().copied().next().unwrap_or(use_slot);
                        self.pan = Pan::Recognized {
                            axis,
                            use_slot: next,
                        };
                    }
                } else {
                    self.pan = Pan::Done;
                    out.push(Output::PanEnd {
                        cancelled: false,
                        time,
                    });
                }
            }
            _ => {}
        }
        out
    }

    /// The device cancelled every touch (libinput's touch cancel, the
    /// touchscreen gone): recognized gestures cancel too.
    pub fn cancel(&mut self, time: u32) -> Vec<Output> {
        let mut out = Vec::new();
        if let Pan::Recognized { .. } = self.pan {
            out.push(Output::PanEnd {
                cancelled: true,
                time,
            });
        }
        if let Edge::Recognizing { .. } = self.edge {
            out.push(Output::EdgeCancel);
        }
        *self = Self::default();
        out
    }
}

fn along(axis: Axis, delta: Point<f64, Logical>) -> Point<f64, Logical> {
    match axis {
        Axis::X => (delta.x, 0.0).into(),
        Axis::Y => (0.0, delta.y).into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: Allowed = Allowed {
        overview_swipe: true,
        workspace_swipe: true,
        osk_edge_drag: true,
    };

    fn monitors() -> Vec<Rectangle<i32, Logical>> {
        vec![Rectangle::new((0, 0).into(), (1280, 800).into())]
    }

    fn p(x: f64, y: f64) -> Point<f64, Logical> {
        (x, y).into()
    }

    fn three_down(g: &mut TouchGestures, allowed: Allowed) {
        for (slot, x) in [(0, 400.0), (1, 500.0), (2, 600.0)] {
            assert!(g
                .down(slot, p(x, 400.0), 10, &monitors(), allowed)
                .is_empty());
        }
    }

    #[test]
    fn three_fingers_claim_after_sixteen_pixels_on_the_latest_finger() {
        let mut g = TouchGestures::default();
        three_down(&mut g, ALL);
        // Older fingers do not drive the threshold (`use_point`).
        assert!(g.motion(0, p(300.0, 400.0), 20, ALL).is_empty());
        assert!(g.motion(2, p(590.0, 400.0), 20, ALL).is_empty());
        assert_eq!(g.claimed(), None);
        let out = g.motion(2, p(580.0, 401.0), 30, ALL);
        assert_eq!(
            out,
            vec![
                Output::Claim(Gesture::WorkspaceSwipe),
                Output::PanUpdate {
                    delta: p(-10.0, 0.0),
                    time: 30
                }
            ]
        );
        assert_eq!(
            g.motion(2, p(570.0, 420.0), 40, ALL),
            vec![Output::PanUpdate {
                delta: p(-10.0, 0.0),
                time: 40
            }]
        );
        // Lifting below three fingers ends the pan; the claim holds
        // until every finger is up.
        assert_eq!(
            g.up(0, 50),
            vec![Output::PanEnd {
                cancelled: false,
                time: 50
            }]
        );
        assert_eq!(g.claimed(), Some(Gesture::WorkspaceSwipe));
        g.up(1, 51);
        g.up(2, 52);
        assert_eq!(g.points(), 0);
        g.down(0, p(10.0, 10.0), 60, &monitors(), ALL);
        assert_eq!(g.claimed(), None);
    }

    #[test]
    fn vertical_pans_drive_the_overview_and_respect_action_modes() {
        let mut g = TouchGestures::default();
        three_down(&mut g, ALL);
        let out = g.motion(2, p(600.0, 380.0), 20, ALL);
        assert_eq!(out[0], Output::Claim(Gesture::OverviewSwipe));
        assert_eq!(
            out[1],
            Output::PanUpdate {
                delta: p(0.0, -20.0),
                time: 20
            }
        );
        // In the overview only the vertical tracker is allowed.
        let overview = Allowed {
            workspace_swipe: false,
            ..ALL
        };
        let mut g = TouchGestures::default();
        three_down(&mut g, overview);
        assert!(g.motion(2, p(560.0, 400.0), 20, overview).is_empty());
        // Locked: nothing at all.
        let mut g = TouchGestures::default();
        three_down(&mut g, Allowed::default());
        assert!(g
            .motion(2, p(600.0, 300.0), 20, Allowed::default())
            .is_empty());
        assert_eq!(g.claimed(), None);
    }

    #[test]
    fn two_or_four_fingers_never_pan() {
        let mut g = TouchGestures::default();
        g.down(0, p(400.0, 400.0), 0, &monitors(), ALL);
        g.down(1, p(500.0, 400.0), 0, &monitors(), ALL);
        assert!(g.motion(1, p(300.0, 400.0), 10, ALL).is_empty());
        let mut g = TouchGestures::default();
        three_down(&mut g, ALL);
        g.down(3, p(700.0, 400.0), 10, &monitors(), ALL);
        assert!(g.motion(3, p(600.0, 300.0), 20, ALL).is_empty());
        assert_eq!(g.claimed(), None);
        // A finger lifting before recognition cancels until all lift.
        let mut g = TouchGestures::default();
        three_down(&mut g, ALL);
        g.up(0, 15);
        g.down(0, p(400.0, 400.0), 16, &monitors(), ALL);
        assert!(g.motion(0, p(400.0, 300.0), 20, ALL).is_empty());
    }

    #[test]
    fn bottom_edge_drag_claims_on_leaving_the_zone_and_completes_at_eighty_pixels() {
        let mut g = TouchGestures::default();
        assert!(g.down(0, p(640.0, 795.0), 0, &monitors(), ALL).is_empty());
        assert!(g.motion(0, p(640.0, 785.0), 50, ALL).is_empty());
        let out = g.motion(0, p(642.0, 770.0), 80, ALL);
        assert_eq!(
            out,
            vec![
                Output::Claim(Gesture::OskEdgeDrag),
                Output::EdgeProgress { distance: 25.0 }
            ]
        );
        // Still short of 80 px above the bottom (720): progress only.
        assert_eq!(
            g.motion(0, p(642.0, 730.0), 90, ALL),
            vec![Output::EdgeProgress { distance: 65.0 }]
        );
        assert_eq!(
            g.motion(0, p(642.0, 715.0), 100, ALL),
            vec![
                Output::EdgeProgress { distance: 80.0 },
                Output::EdgeActivate
            ]
        );
        assert!(g.motion(0, p(642.0, 600.0), 120, ALL).is_empty());
        assert!(g.up(0, 130).is_empty());
    }

    #[test]
    fn edge_drag_cancels_like_shell_edge_drag_gesture() {
        // Resting in the zone past 200 ms.
        let mut g = TouchGestures::default();
        g.down(0, p(640.0, 795.0), 0, &monitors(), ALL);
        assert!(g.motion(0, p(640.0, 760.0), 250, ALL).is_empty());
        assert_eq!(g.claimed(), None);
        // Lifted after recognizing, before completing.
        let mut g = TouchGestures::default();
        g.down(0, p(640.0, 795.0), 0, &monitors(), ALL);
        g.motion(0, p(640.0, 770.0), 50, ALL);
        assert_eq!(g.up(0, 60), vec![Output::EdgeCancel]);
        // Too far sideways.
        let mut g = TouchGestures::default();
        g.down(0, p(640.0, 795.0), 0, &monitors(), ALL);
        g.motion(0, p(640.0, 770.0), 50, ALL);
        assert_eq!(
            g.motion(0, p(760.0, 760.0), 60, ALL),
            vec![Output::EdgeCancel]
        );
        // A second finger.
        let mut g = TouchGestures::default();
        g.down(0, p(640.0, 795.0), 0, &monitors(), ALL);
        g.motion(0, p(640.0, 770.0), 50, ALL);
        assert_eq!(
            g.down(1, p(100.0, 100.0), 60, &monitors(), ALL),
            vec![Output::EdgeCancel]
        );
        // Not beginning near the edge, or not allowed (locked, keyboard up).
        let mut g = TouchGestures::default();
        g.down(0, p(640.0, 700.0), 0, &monitors(), ALL);
        assert!(g.motion(0, p(640.0, 500.0), 50, ALL).is_empty());
        let none = Allowed::default();
        let mut g = TouchGestures::default();
        g.down(0, p(640.0, 795.0), 0, &monitors(), none);
        assert!(g.motion(0, p(640.0, 600.0), 50, none).is_empty());
    }

    #[test]
    fn edges_belong_to_the_touched_monitor() {
        let monitors = vec![
            Rectangle::new((0, 0).into(), (1280, 800).into()),
            Rectangle::new((1280, 0).into(), (1920, 1080).into()),
        ];
        let mut g = TouchGestures::default();
        // 795 is the first monitor's edge but deep inside the second.
        g.down(0, p(1500.0, 795.0), 0, &monitors, ALL);
        assert!(g.motion(0, p(1500.0, 600.0), 50, ALL).is_empty());
        let mut g = TouchGestures::default();
        g.down(0, p(1500.0, 1075.0), 0, &monitors, ALL);
        assert_eq!(
            g.motion(0, p(1500.0, 1050.0), 50, ALL)[0],
            Output::Claim(Gesture::OskEdgeDrag)
        );
    }

    #[test]
    fn device_cancel_ends_recognized_gestures() {
        let mut g = TouchGestures::default();
        three_down(&mut g, ALL);
        g.motion(2, p(600.0, 300.0), 20, ALL);
        assert_eq!(
            g.cancel(30),
            vec![Output::PanEnd {
                cancelled: true,
                time: 30
            }]
        );
        assert_eq!(g.points(), 0);
        assert_eq!(g.claimed(), None);
    }
}
