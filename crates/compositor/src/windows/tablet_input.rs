//! Tablet tools and pads on the Wayland seat (P-IN-05, #344).
//!
//! Mutter 51 routes a tablet's events to its tablet seat only
//! (`meta-wayland-seat.c` `is_tablet_event`): clients that bind
//! `zwp_tablet_manager_v2` see tools enter the surface under them, and
//! nothing reaches `wl_pointer`. While the tip is down the tool keeps the
//! surface it pressed on (`meta_wayland_tablet_seat_get_implicit_grab_surface`),
//! and a tip press focuses and raises like a click
//! (`meta_window_handle_ungrabbed_event` takes every button press). Pads
//! follow keyboard focus (`meta_wayland_tablet_seat_set_pad_focus`, see
//! `State::sync_tablet_pads`).

use std::collections::HashMap;

use smithay::{
    backend::input::{ButtonState, TabletToolDescriptor},
    input::pointer::{CursorIcon, CursorImageStatus},
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Point, SERIAL_COUNTER},
    wayland::tablet_manager::{TabletSeatTrait, TabletToolHandle},
};

use super::WindowManager;
use crate::tablet::{TabletEvent, ToolAxes};
use crate::State;

use smithay::reexports::wayland_server::Resource as _;
use smithay::wayland::seat::WaylandFocus as _;
/// One tool in proximity.
#[derive(Debug)]
struct ToolFocus {
    handle: TabletToolHandle,
    /// The surface the tip went down on and its origin, until it lifts.
    grab: Option<(WlSurface, Point<f64, Logical>)>,
}

/// Tools in proximity, by descriptor.
#[derive(Debug, Default)]
pub(crate) struct TabletFocus {
    tools: HashMap<TabletToolDescriptor, ToolFocus>,
}

impl ToolFocus {
    fn queue_axes(&self, axes: &ToolAxes) {
        if let Some(pressure) = axes.pressure {
            self.handle.pressure(pressure);
        }
        if let Some(distance) = axes.distance {
            self.handle.distance(distance);
        }
        if let Some(tilt) = axes.tilt {
            self.handle.tilt(tilt);
        }
        if let Some(rotation) = axes.rotation {
            self.handle.rotation(rotation);
        }
        if let Some(slider) = axes.slider {
            self.handle.slider_position(slider);
        }
        if let Some((degrees, clicks)) = axes.wheel {
            self.handle.wheel(degrees, clicks);
        }
    }
}

impl WindowManager {
    /// The surface a tool at `pos` is over and its origin, in the order
    /// pointer motion picks: popups, layer surfaces, then windows (none in
    /// the overview, and never a parent blocked by its modal dialog).
    fn tablet_target(
        &self,
        state: &State,
        pos: Point<f64, Logical>,
        lock: Option<&WlSurface>,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        if let Some(lock) = lock {
            return Some((lock.clone(), (0.0, 0.0).into()));
        }
        if let Some(popup) = self.popup_at(state, pos) {
            return Some((popup.surface, popup.origin.to_f64()));
        }
        if let Some((surface, (x, y))) =
            crate::layer::topmost_layer_at(state, pos.x.floor() as i32, pos.y.floor() as i32)
        {
            return Some((surface, (f64::from(x), f64::from(y)).into()));
        }
        if self.overview_open {
            return None;
        }
        let id = self.window_at(pos)?;
        if self.modal_target(id) != id {
            return None;
        }
        let window = self.windows.get(&id)?;
        let surface = window.surface.wl_surface()?.into_owned();
        let origin = crate::popup::surface_origin(&surface, window.geometry.loc);
        Some((surface, origin.to_f64()))
    }

    /// A tip press: what a primary click at `pos` does to focus. Returns
    /// whether the press is consumed (an xdg popup grab dismissed, or a
    /// parent behind its modal dialog).
    fn tablet_press_focus(&mut self, state: &mut State, pos: Point<f64, Logical>) -> bool {
        let on_popup = self.popup_at(state, pos).is_some();
        if !on_popup && state.popup_grab_active() {
            let target = self
                .surface_for_click(state, pos)
                .and_then(|s| s.client())
                .map(|c| c.id());
            if target.is_none() || target != state.popup_grab_owner() {
                state.dismiss_popup_grab();
                return true;
            }
        }
        if on_popup {
            return false;
        }
        if let Some((surface, _)) =
            crate::layer::topmost_layer_at(state, pos.x.floor() as i32, pos.y.floor() as i32)
        {
            if !self.overview_open && crate::layer::surface_takes_keyboard_on_press(&surface) {
                if let Some(keyboard) = self.keyboard.clone() {
                    keyboard.set_focus(state, Some(surface.clone()), SERIAL_COUNTER.next_serial());
                }
                state.sync_selection_focus(Some(&surface));
            }
            return false;
        }
        if self.overview_open {
            return false;
        }
        if let Some(id) = self.window_at(pos) {
            if self.modal_target(id) != id {
                self.apply_focus_raise(state, Some(id), self.focus_policy.raise_on_click);
                return true;
            }
            let keyboard_matches = self
                .keyboard
                .as_ref()
                .is_none_or(|keyboard| keyboard.current_focus() == self.surface_of(id));
            if self.model.focused() != Some(id) || !keyboard_matches {
                self.apply_focus_raise(state, Some(id), self.focus_policy.raise_on_click);
            } else if self.focus_policy.raise_on_click && self.mode == super::SessionMode::Gnome {
                self.raise_family(id);
            }
        }
        false
    }

    /// Show the tool's cursor at `pos` (Mutter hides the pointer and draws
    /// the tool's own sprite); over no client surface, the default arrow.
    fn tablet_cursor(
        state: &mut State,
        tool: &TabletToolDescriptor,
        pos: Point<f64, Logical>,
        over: bool,
    ) {
        if !over {
            state
                .tablet_cursor_images
                .insert(tool.clone(), CursorImageStatus::Named(CursorIcon::Default));
        }
        state.tablet_cursor = Some((tool.clone(), pos));
    }

    /// Route one tablet event. `lock` is the session-lock surface while
    /// locked: then it alone hears tools, as it alone hears the pointer.
    pub fn tablet_input(
        &mut self,
        state: &mut State,
        event: TabletEvent,
        lock: Option<&WlSurface>,
    ) {
        let dh = state.dh.clone();
        let seat = state.seat.tablet_seat();
        match event {
            TabletEvent::TabletAdded(desc) => {
                seat.add_tablet::<State>(&dh, &desc);
                state.sync_tablet_pads();
            }
            TabletEvent::TabletRemoved(desc) => {
                seat.remove_tablet(&desc);
                state.sync_tablet_pads();
            }
            TabletEvent::PadAdded { pad, tablet } => {
                seat.add_pad::<State>(&dh, &pad);
                state.tablet_pads.retain(|(known, _)| *known != pad);
                state.tablet_pads.push((pad, tablet));
                state.sync_tablet_pads();
            }
            TabletEvent::PadRemoved(pad) => {
                seat.remove_pad(&pad);
                state.tablet_pads.retain(|(known, _)| *known != pad);
            }
            TabletEvent::ProximityIn {
                tablet,
                tool,
                pos,
                axes,
                time,
            } => {
                let Some(tablet) = seat.get_tablet(&tablet) else {
                    return;
                };
                let handle = seat.add_tool::<State>(state, &dh, &tool);
                let focus = ToolFocus { handle, grab: None };
                let target = self.tablet_target(state, pos, lock);
                Self::tablet_cursor(state, &tool, pos, target.is_some());
                if let Some(target) = target {
                    focus.handle.proximity_in(
                        pos,
                        target.clone(),
                        &tablet,
                        SERIAL_COUNTER.next_serial(),
                        time,
                    );
                    // The first frame's axes follow in a motion frame.
                    focus.queue_axes(&axes);
                    focus.handle.motion(
                        pos,
                        Some(target),
                        &tablet,
                        SERIAL_COUNTER.next_serial(),
                        time,
                    );
                }
                self.tablet.tools.insert(tool, focus);
            }
            TabletEvent::Motion {
                tablet,
                tool,
                pos,
                axes,
                time,
            } => {
                let Some(tablet) = seat.get_tablet(&tablet) else {
                    return;
                };
                let target = match self.tablet.tools.get(&tool).and_then(|t| t.grab.clone()) {
                    Some(grab) => Some(grab),
                    None => self.tablet_target(state, pos, lock),
                };
                Self::tablet_cursor(state, &tool, pos, target.is_some());
                let Some(focus) = self.tablet.tools.get(&tool) else {
                    return;
                };
                focus.queue_axes(&axes);
                focus
                    .handle
                    .motion(pos, target, &tablet, SERIAL_COUNTER.next_serial(), time);
            }
            TabletEvent::Tip {
                tool,
                down,
                pos,
                time,
                ..
            } => {
                if down {
                    let consumed = lock.is_none() && self.tablet_press_focus(state, pos);
                    if consumed {
                        return;
                    }
                    let target = self.tablet_target(state, pos, lock);
                    let Some(focus) = self.tablet.tools.get_mut(&tool) else {
                        return;
                    };
                    focus.grab = target;
                    focus.handle.tip_down(SERIAL_COUNTER.next_serial(), time);
                } else if let Some(focus) = self.tablet.tools.get_mut(&tool) {
                    focus.grab = None;
                    focus.handle.tip_up(time);
                }
            }
            TabletEvent::Button {
                tool,
                button,
                pressed,
                time,
                ..
            } => {
                if let Some(focus) = self.tablet.tools.get(&tool) {
                    focus.handle.button(
                        button,
                        if pressed {
                            ButtonState::Pressed
                        } else {
                            ButtonState::Released
                        },
                        SERIAL_COUNTER.next_serial(),
                        time,
                    );
                }
            }
            TabletEvent::ProximityOut { tool, time, .. } => {
                if let Some(focus) = self.tablet.tools.remove(&tool) {
                    focus.handle.proximity_out(time);
                }
                if state
                    .tablet_cursor
                    .as_ref()
                    .is_some_and(|(shown, _)| *shown == tool)
                {
                    state.tablet_cursor = None;
                }
            }
            TabletEvent::PadButton {
                pad,
                button,
                pressed,
                mode,
                time,
            } => {
                let Some(handle) = seat.get_pad(&pad) else {
                    return;
                };
                // A mode-toggle button switches its group and is not
                // delivered as a button (meta_wayland_tablet_pad_group_handle_event).
                if let Some((group, mode)) = mode {
                    if pressed {
                        handle.mode_switch(group, mode, SERIAL_COUNTER.next_serial(), time);
                    }
                    return;
                }
                handle.button(button, pressed, time);
            }
            TabletEvent::PadRing {
                pad,
                ring,
                angle,
                finger,
                time,
            } => {
                if let Some(handle) = seat.get_pad(&pad) {
                    handle.ring(ring, angle, finger, time);
                }
            }
            TabletEvent::PadStrip {
                pad,
                strip,
                position,
                finger,
                time,
            } => {
                if let Some(handle) = seat.get_pad(&pad) {
                    handle.strip(strip, position, finger, time);
                }
            }
            // The runtime's own effects; never routed to clients.
            TabletEvent::EmulateKeybinding { .. }
            | TabletEvent::SetOutput { .. }
            | TabletEvent::ShowOsd { .. }
            | TabletEvent::RequestPadOsd { .. }
            | TabletEvent::PadOsdInput { .. }
            | TabletEvent::ClosePadOsd { .. } => {}
        }
    }
}
