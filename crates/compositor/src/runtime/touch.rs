//! The runtime's side of touchscreens (P-DS-04, #345): GNOME Shell 51's
//! stage gestures (`crate::touch_gesture`) see every touch first, and a
//! recognized one claims the fingers from clients (`wl_touch.cancel`);
//! the compositor-drawn overview previews take touch as Shell actors do.

use smithay::utils::{Logical, Point};
use tuna_shell_control::OskGesturePhase;

use crate::touch_gesture::{Allowed, Gesture, Output, TouchGestures};
use crate::windows::ManagerInput;

use super::Runtime;

/// `BTN_LEFT`: the overview previews' press.
const OVERVIEW_BUTTON: u32 = 0x110;
/// GNOME's `TOUCHPAD_BASE_WIDTH`: the touchpad units the workspace swipe
/// follows (`workspace_slide`).
const TOUCHPAD_BASE_WIDTH: f64 = crate::workspace_slide::TOUCHPAD_BASE_WIDTH;

#[derive(Debug, Default)]
pub(super) struct RuntimeTouch {
    gestures: TouchGestures,
    /// The touch slot working the overview previews as a primary press.
    overview: Option<u32>,
    overview_swipes: u64,
    workspace_swipes: u64,
    osk_edge_drags: u64,
    last: Option<Gesture>,
}

impl Runtime {
    /// Which gestures the action mode allows now.
    fn touch_gestures_allowed(&self) -> Allowed {
        let normal = !self.is_locked() && !self.overlay.visible;
        let osk_shown = self
            .state
            .panel_surfaces()
            .iter()
            .any(|record| record.namespace == tuna_shell_control::OSK_NAMESPACE);
        Allowed {
            overview_swipe: normal,
            workspace_swipe: normal && !self.control.overview_open(),
            osk_edge_drag: normal && !osk_shown,
        }
    }

    /// Feed one touch event to the gestures; returns whether a gesture
    /// owns it (nothing else hears it).
    pub(super) fn touch_gesture_input(&mut self, input: &ManagerInput) -> bool {
        let allowed = self.touch_gestures_allowed();
        let outputs = match *input {
            ManagerInput::TouchDown { slot, pos, time } => {
                let monitors: Vec<_> = self
                    .state
                    .hot_corner_outputs()
                    .into_iter()
                    .map(|(rect, _)| rect)
                    .collect();
                self.touch
                    .gestures
                    .down(slot, pos, time, &monitors, allowed)
            }
            ManagerInput::TouchMotion { slot, pos, time } => {
                self.touch.gestures.motion(slot, pos, time, allowed)
            }
            ManagerInput::TouchUp { slot, time } => self.touch.gestures.up(slot, time),
            ManagerInput::TouchCancel { time } => self.touch.gestures.cancel(time),
            ManagerInput::TouchFrame { .. } => Vec::new(),
            _ => return false,
        };
        self.apply_touch_gestures(outputs);
        self.touch.gestures.claimed().is_some()
            && !matches!(input, ManagerInput::TouchCancel { .. })
    }

    fn apply_touch_gestures(&mut self, outputs: Vec<Output>) {
        let size = self.state.primary_size();
        for output in outputs {
            match output {
                Output::Claim(gesture) => {
                    self.manager.cancel_client_touches(&mut self.state);
                    self.cancel_touch_overview();
                    self.touch.last = Some(gesture);
                    match gesture {
                        Gesture::OverviewSwipe => self.touch.overview_swipes += 1,
                        Gesture::WorkspaceSwipe => self.touch.workspace_swipes += 1,
                        Gesture::OskEdgeDrag => self.touch.osk_edge_drags += 1,
                    }
                    if gesture != Gesture::OskEdgeDrag {
                        let time = crate::state::system_millis() as u32;
                        self.shell_swipe_input(&ManagerInput::SwipeBegin {
                            fingers: crate::windows::SHELL_SWIPE_FINGERS,
                            time,
                        });
                    }
                }
                Output::PanUpdate { delta, time } => {
                    // Touchpad units, so the fingers' travel over the
                    // tracker's distance (the screen) moves it 1:1.
                    let width = f64::from(size.w + crate::workspace_slide::WORKSPACE_SPACING);
                    let height = f64::from(size.h.max(1));
                    let delta: Point<f64, Logical> = (
                        delta.x * TOUCHPAD_BASE_WIDTH / width.max(1.0),
                        delta.y * super::OVERVIEW_SWIPE_DISTANCE / height,
                    )
                        .into();
                    self.shell_swipe_input(&ManagerInput::SwipeUpdate { delta, time });
                }
                Output::PanEnd { cancelled, time } => {
                    self.shell_swipe_input(&ManagerInput::SwipeEnd { cancelled, time });
                }
                Output::EdgeProgress { distance } => self
                    .control
                    .queue_osk_gesture(OskGesturePhase::Progress { distance }),
                Output::EdgeActivate => self.control.queue_osk_gesture(OskGesturePhase::Activate),
                Output::EdgeCancel => self.control.queue_osk_gesture(OskGesturePhase::Cancel),
            }
        }
    }

    /// The lock engaged or released: every touch is taken from whoever
    /// had it, and recognized gestures cancel.
    pub(super) fn reset_touch_ownership(&mut self) {
        self.manager.cancel_client_touches(&mut self.state);
        self.cancel_touch_overview();
        let time = crate::state::system_millis() as u32;
        let outputs = self.touch.gestures.cancel(time);
        self.apply_touch_gestures(outputs);
    }

    /// A touch outside every shell surface in the overview works the
    /// compositor-drawn previews and workspace cards as a primary press
    /// would (GNOME's previews are Clutter actors taking touch through
    /// their gestures). Returns whether the event was consumed.
    pub(super) fn touch_overview_input(&mut self, input: &ManagerInput) -> bool {
        match *input {
            ManagerInput::TouchDown { slot, pos, time } => {
                if self.touch.overview.is_some()
                    || !self.control.overview_open()
                    || self.manager.touch_lands_on_client(&self.state, pos)
                {
                    return false;
                }
                self.touch.overview = Some(slot);
                self.overview_touch_motion(pos, time);
                self.on_manager_input(ManagerInput::Button {
                    button: OVERVIEW_BUTTON,
                    pressed: true,
                    time,
                });
                true
            }
            ManagerInput::TouchMotion { slot, pos, time } if self.touch.overview == Some(slot) => {
                self.overview_touch_motion(pos, time);
                true
            }
            ManagerInput::TouchUp { slot, time } if self.touch.overview == Some(slot) => {
                self.touch.overview = None;
                self.on_manager_input(ManagerInput::Button {
                    button: OVERVIEW_BUTTON,
                    pressed: false,
                    time,
                });
                true
            }
            ManagerInput::TouchCancel { .. } => {
                self.cancel_touch_overview();
                false
            }
            _ => false,
        }
    }

    fn overview_touch_motion(&mut self, pos: Point<f64, Logical>, time: u32) {
        if self.magnifier.config().active() {
            // Already mapped through the zoom: the view must not chase it.
            self.magnifier.hold_pointer((pos.x, pos.y));
        }
        self.on_manager_input(ManagerInput::Motion { pos, time });
    }

    /// The overview press a touch was working is cancelled: a preview
    /// drag goes back, and the press is released without a click.
    fn cancel_touch_overview(&mut self) {
        if self.touch.overview.take().is_none() {
            return;
        }
        if let Some((_, _, dragging)) = self.overview_drag.take() {
            if dragging {
                self.overview_motion
                    .drag_snap_back(self.input_settings.motion);
            }
        }
        let time = crate::state::system_millis() as u32;
        self.on_manager_input(ManagerInput::Button {
            button: OVERVIEW_BUTTON,
            pressed: false,
            time,
        });
    }

    /// The state file's touch record.
    pub(super) fn touch_diagnostics(&self) -> serde_json::Value {
        let mut doc = self.manager.touch_introspect(&self.state);
        doc["gestures"] = serde_json::json!({
            "overview_swipe": self.touch.overview_swipes,
            "workspace_swipe": self.touch.workspace_swipes,
            "osk_edge_drag": self.touch.osk_edge_drags,
        });
        doc["last_gesture"] = serde_json::json!(self.touch.last.map(Gesture::name));
        doc["overview_press"] = serde_json::json!(self.touch.overview.is_some());
        doc
    }
}
