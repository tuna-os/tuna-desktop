//! The window manager's half of GNOME's window-manager focus, mouse and
//! shortcut preferences (#347): focus that follows the pointer,
//! raise-on-click, new-window focus, modifier-button drags, the extra
//! window keys and Show Desktop. Kept beside `windows.rs` so the core
//! input and layout paths only gain a call each; the decisions
//! themselves are [`crate::wm_prefs`]'s.

use smithay::utils::{Logical, Point, Rectangle};
use tuna_shell_control::{FocusMode, FocusNewWindows, WmPreferences};

use super::{SessionMode, WindowLayout, WindowManager};
use crate::wm_prefs::{self, ButtonRole, HoverEvent};
use crate::State;

impl WindowManager {
    /// GNOME's window-manager focus and mouse preferences in force.
    pub fn wm_preferences(&self) -> &WmPreferences {
        &self.prefs
    }

    /// Apply GNOME's window-manager focus and mouse preferences live,
    /// from the next pointer event on.
    pub fn apply_wm_preferences(&mut self, prefs: WmPreferences) {
        if self.prefs == prefs {
            return;
        }
        self.prefs = prefs;
        self.hover.reset();
        eprintln!(
            "tuna-compositor: wm preferences focus={:?} rest={} new={:?} auto_raise={}/{}ms raise_on_click={} modifier={} right_resize={}",
            prefs.focus_mode,
            prefs.focus_change_on_pointer_rest,
            prefs.focus_new_windows,
            prefs.auto_raise,
            prefs.auto_raise_delay_ms,
            prefs.raise_on_click,
            prefs.mouse_button_modifier,
            prefs.resize_with_right_button,
        );
    }

    /// Model ids bottom-to-top, for the state proof.
    pub fn stacking_order(&self) -> Vec<u64> {
        self.stacking.clone()
    }

    /// Focus `id` without changing the stacking (hover focus, and a
    /// click with `raise-on-click` off).
    fn focus_without_raise(&mut self, state: &mut State, id: Option<u64>) {
        self.hold_stacking = true;
        self.apply_focus(state, id);
        self.hold_stacking = false;
    }

    /// A press on window `id` without the modifier: focus it, raising it
    /// when `raise-on-click` is on. Under the pointer focus modes a
    /// hover-focused window is still below others, so a click on it
    /// raises it too.
    pub(super) fn click_focus(&mut self, state: &mut State, id: u64, keyboard_matches: bool) {
        let refocus = self.model.focused() != Some(id) || !keyboard_matches;
        if self.prefs.raise_on_click {
            if refocus {
                self.apply_focus(state, Some(id));
            } else if self.prefs.focus_mode != FocusMode::Click {
                self.raise(id);
            }
        } else if refocus {
            self.focus_without_raise(state, Some(id));
        }
    }

    /// The pointer moved to `pos` over windows: GNOME's sloppy and mouse
    /// focus modes.
    pub(super) fn hover_motion(&mut self, state: &mut State, pos: Point<f64, Logical>) {
        if self.prefs.focus_mode == FocusMode::Click {
            return;
        }
        let target = self.hover_target(pos);
        let now = crate::state::system_millis();
        if let Some(event) = self.hover.motion(&self.prefs, target, pos, now) {
            self.hover_event(state, event);
        }
    }

    /// Advance focus-follows-mouse timers (pointer rest, auto-raise);
    /// the runtime calls this every tick.
    pub fn tick_hover(&mut self, state: &mut State) {
        let now = crate::state::system_millis();
        if self.hover_wake_ms(now).is_none() {
            return;
        }
        let target = self.hover_target(self.pointer_pos);
        for event in self.hover.tick(&self.prefs, target, now) {
            self.hover_event(state, event);
        }
    }

    /// Milliseconds from `now_ms` until a hover timer is due.
    pub fn hover_wake_ms(&self, now_ms: u64) -> Option<u64> {
        if self.prefs.focus_mode == FocusMode::Click {
            return None;
        }
        self.hover.next_wake_ms(now_ms)
    }

    /// The window hover focus would pick at `pos`: what the pointer is
    /// over, through a modal dialog to the dialog.
    pub(super) fn hover_target(&self, pos: Point<f64, Logical>) -> Option<u64> {
        self.window_at(pos)
            .filter(|id| self.windows.get(id).is_some_and(|w| !w.minimized))
            .map(|id| self.modal_target(id))
    }

    /// Carry out a hover decision unless a grab, the overview, the lock
    /// screen, the switcher or a popup owns the pointer.
    fn hover_event(&mut self, state: &mut State, event: HoverEvent) {
        if self.grab.is_some()
            || self.overview_open
            || self.lock_input_active
            || self.switcher_open
            || state.popup_grab_active()
        {
            return;
        }
        match event {
            HoverEvent::Focus(id) => {
                if self.model.focused() != Some(id) {
                    eprintln!("tuna-compositor: hover focus {id}");
                    self.focus_without_raise(state, Some(id));
                }
            }
            HoverEvent::Unfocus => {
                if self.model.focused().is_some() {
                    eprintln!("tuna-compositor: hover focus none");
                    self.apply_focus(state, None);
                }
            }
            HoverEvent::Raise(id) => {
                eprintln!("tuna-compositor: auto-raise {id}");
                self.raise(id);
            }
        }
    }

    /// A press with `mouse-button-modifier` held over a window: move it,
    /// resize it from the nearest edges, or open its menu. Returns
    /// whether the press was taken (its release is swallowed too).
    pub(super) fn modifier_press(&mut self, state: &mut State, button: u32) -> bool {
        let Some(role) = wm_prefs::button_role(&self.prefs, self.held_mods(), button) else {
            return false;
        };
        let pos = self.pointer_pos;
        let Some(id) = self.window_at(pos) else {
            return false;
        };
        match role {
            ButtonRole::Move => {
                self.apply_focus(state, Some(id));
                // The trigger machine already disarmed the Super tap on
                // this press, so releasing Super will not open the
                // overview.
                self.begin_move_owned(state, id, pos, Some(button));
            }
            ButtonRole::Resize => {
                let Some(geometry) = self.geometry(id) else {
                    return false;
                };
                let edges = wm_prefs::resize_edges(geometry, pos);
                if edges == 0 {
                    return false;
                }
                self.apply_focus(state, Some(id));
                self.begin_resize(state, id, edges);
            }
            ButtonRole::Menu => {
                if self.prefs.raise_on_click {
                    self.raise(id);
                }
                self.menu_requests
                    .push((id, pos.x.round() as i32, pos.y.round() as i32));
            }
        }
        true
    }

    /// Whether a newly mapped `id` takes focus: always under GNOME's
    /// `smart` policy; under `strict`, only when nothing is focused, it
    /// is a dialog of the focused window, or the overview launched it.
    pub(super) fn new_window_takes_focus(&self, id: u64) -> bool {
        if self.prefs.focus_new_windows == FocusNewWindows::Smart || self.overview_held.is_some() {
            return true;
        }
        match self.model.focused() {
            None => true,
            Some(focused) => focused == id || self.transient_parent(id) == Some(focused),
        }
    }

    /// Stack a window that did not take focus just below the focused
    /// one, as Mutter does for denied focus.
    pub(super) fn stack_below_focus(&mut self, id: u64) {
        let Some(focused) = self.model.focused() else {
            return;
        };
        self.stacking.retain(|other| *other != id);
        let pos = self
            .stacking
            .iter()
            .position(|other| *other == focused)
            .unwrap_or(self.stacking.len());
        self.stacking.insert(pos, id);
        self.keep_above_on_top();
    }

    /// Raise `id` (and its transient family) to the top; Always on Top
    /// windows stay above.
    fn raise(&mut self, id: u64) {
        if self.mode != SessionMode::Gnome || !self.windows.contains_key(&id) {
            return;
        }
        self.raise_family(id);
    }

    /// Lower `id` to the bottom.
    fn lower(&mut self, id: u64) {
        if self.mode != SessionMode::Gnome || !self.windows.contains_key(&id) {
            return;
        }
        self.stacking.retain(|other| *other != id);
        self.stacking.insert(0, id);
        self.keep_above_on_top();
    }

    /// Mutter's `raise-or-lower`: lower when nothing shown overlaps it
    /// from above, else raise.
    fn raise_or_lower(&mut self, id: u64) {
        let Some(geometry) = self.geometry(id) else {
            return;
        };
        let shown: Vec<u64> = self
            .visible_entries()
            .iter()
            .map(|(id, _, _)| *id)
            .collect();
        let covered = self
            .stacking
            .iter()
            .skip_while(|other| **other != id)
            .skip(1)
            .filter(|other| shown.contains(other))
            .filter_map(|other| self.geometry(*other))
            .any(|above| above.overlaps(geometry));
        if covered {
            self.raise(id);
        } else {
            self.lower(id);
        }
    }

    /// Every output rectangle and whether it is the primary one.
    fn output_rects(&self) -> impl Iterator<Item = (Rectangle<i32, Logical>, bool)> + '_ {
        self.workspace_outputs.iter().map(|o| {
            (
                Rectangle::new((o.x, o.y).into(), (o.width, o.height).into()),
                o.primary,
            )
        })
    }

    /// The output holding the center of `geometry`, else the primary.
    fn output_of(&self, geometry: Rectangle<i32, Logical>) -> Option<Rectangle<i32, Logical>> {
        let center = Point::<i32, Logical>::from((
            geometry.loc.x + geometry.size.w / 2,
            geometry.loc.y + geometry.size.h / 2,
        ));
        self.output_rects()
            .find(|(rect, _)| rect.contains(center))
            .or_else(|| self.output_rects().find(|(_, primary)| *primary))
            .map(|(rect, _)| rect)
    }

    /// The work area of the output holding `geometry`: the panel only
    /// insets the primary output.
    fn work_area_of(
        &self,
        state: &State,
        geometry: Rectangle<i32, Logical>,
    ) -> Rectangle<i32, Logical> {
        let primary = self
            .output_rects()
            .find(|(_, primary)| *primary)
            .map(|(rect, _)| rect);
        match self.output_of(geometry) {
            Some(output) if Some(output) != primary => output,
            _ => Self::work_area(state),
        }
    }

    /// Carry out the window keys added for GNOME 51's full list
    /// (#347). Returns whether anything changed.
    pub(super) fn extra_window_action(
        &mut self,
        state: &mut State,
        id: u64,
        action: tuna_shell_control::WindowAction,
    ) -> bool {
        use tuna_shell_control::WindowAction;
        let floating = self.window_layout(id) == Some(WindowLayout::Floating);
        match action {
            WindowAction::ToggleFullscreen => {
                let fullscreen = self.window_layout(id) == Some(WindowLayout::Fullscreen);
                self.set_fullscreen(state, id, !fullscreen)
            }
            WindowAction::Raise => {
                self.raise(id);
                true
            }
            WindowAction::Lower => {
                self.lower(id);
                true
            }
            WindowAction::RaiseOrLower => {
                self.raise_or_lower(id);
                true
            }
            WindowAction::MaximizeVertically | WindowAction::MaximizeHorizontally => {
                if !floating {
                    return false;
                }
                let Some(geometry) = self.geometry(id) else {
                    return false;
                };
                let work = self.work_area_of(state, geometry);
                let vertical = action == WindowAction::MaximizeVertically;
                let restore = self.axis_restore.get(&(id, vertical)).copied();
                let (next, keep) =
                    wm_prefs::toggle_axis_maximize(work, geometry, restore, vertical);
                match keep {
                    Some(before) => self.axis_restore.insert((id, vertical), before),
                    None => self.axis_restore.remove(&(id, vertical)),
                };
                self.place_floating(id, next)
            }
            WindowAction::MoveToMonitor { direction } => {
                // Maximized, tiled and fullscreen layouts fill the
                // primary output only; floating windows move.
                if !floating {
                    return false;
                }
                let Some(geometry) = self.geometry(id) else {
                    return false;
                };
                let outputs: Vec<Rectangle<i32, Logical>> =
                    self.output_rects().map(|(rect, _)| rect).collect();
                let Some(from) = self.output_of(geometry) else {
                    return false;
                };
                let Some(to) = wm_prefs::neighbor_output(&outputs, from, direction) else {
                    return false;
                };
                self.place_floating(id, wm_prefs::carry_to_output(geometry, from, to))
            }
            WindowAction::MoveTo { gravity } => {
                if !floating {
                    return false;
                }
                let Some(mut geometry) = self.geometry(id) else {
                    return false;
                };
                let work = self.work_area_of(state, geometry);
                geometry.loc = wm_prefs::gravity_position(work, geometry, gravity);
                self.place_floating(id, geometry)
            }
            _ => false,
        }
    }

    /// Put floating `id` at `geometry` and tell its client.
    fn place_floating(&mut self, id: u64, geometry: Rectangle<i32, Logical>) -> bool {
        self.settle(id);
        let Some(window) = self.windows.get_mut(&id) else {
            return false;
        };
        window.geometry = geometry;
        self.configure(id, self.model.focused() == Some(id));
        true
    }

    /// GNOME's `show-desktop` key: hide every shown window on the active
    /// workspace, or bring back the ones it hid.
    pub fn toggle_show_desktop(&mut self, state: &mut State) {
        if !self.showing_desktop() {
            let shown: Vec<u64> = self
                .visible_entries()
                .iter()
                .map(|(id, _, _)| *id)
                .collect();
            for id in &shown {
                if let Some(window) = self.windows.get_mut(id) {
                    window.minimized = true;
                }
            }
            self.showing_desktop = shown;
            if !self.showing_desktop.is_empty() {
                self.apply_focus(state, None);
            }
        } else {
            for id in std::mem::take(&mut self.showing_desktop) {
                if let Some(window) = self.windows.get_mut(&id) {
                    window.minimized = false;
                }
            }
            let active = self.model.active_workspace();
            self.focus_topmost(state, active);
        }
    }

    /// Whether Show Desktop is hiding windows (any it hid still hidden).
    pub fn showing_desktop(&self) -> bool {
        self.showing_desktop
            .iter()
            .any(|id| self.windows.get(id).is_some_and(|w| w.minimized))
    }
}
