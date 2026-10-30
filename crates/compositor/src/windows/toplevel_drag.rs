//! The window moved with a client drag (`xdg_toplevel_drag_v1`, see
//! [`crate::toplevel_drag`]): Mutter's toplevel drag starts a window drag
//! (`start_window_drag` in `meta-wayland-toplevel-drag.c`, an unconstrained
//! move under the drag grab's pointer).
//!
//! - A shown window attached during its source's drag starts moving at
//!   once; one attached before it is shown is placed with the pointer at
//!   its offset when it is (`calc_origin_for_dragged_window`) and moves
//!   from then on. Either way it then stays at pointer − offset, the
//!   offset being relative to its window geometry.
//! - The moving window takes no part in the hit test (its actor is made
//!   non-reactive), so the drag finds the targets beneath it.
//! - The window drag raises and focuses the window as it begins
//!   (`meta_window_drag_begin`); without raise-on-click it raises at the
//!   end only if it barely moved.
//! - A drop leaves the window where it is, tiled or maximized if dropped
//!   on a screen edge (`end_grab_op`); Escape returns it to where its drag
//!   began, in the layout it had (`process_mouse_move_resize_grab`).
//!   Destroying the toplevel drag, its source or the toplevel just stops.

use smithay::utils::{Logical, Point};

use super::{snap_target, Snap, WindowLayout, WindowManager};
use crate::toplevel_drag::DragEnd;
use crate::State;

/// A window following the drag-and-drop pointer.
#[derive(Debug, Clone, Copy)]
pub(super) struct ToplevelWindowDrag {
    pub(super) id: u64,
    /// Pointer hotspot relative to the window geometry.
    offset: Point<i32, Logical>,
    /// Where the window was when the drag began.
    start: Point<i32, Logical>,
    /// The layout the drag took the window out of.
    layout: WindowLayout,
    /// Pointer when the drag began, and the furthest it went on either
    /// axis since (Mutter's drag threshold for raising at the end).
    pointer_start: Point<f64, Logical>,
    distance: f64,
}

impl WindowManager {
    /// Whether `id` is the window moved with the drag.
    pub(super) fn toplevel_dragged(&self, id: u64) -> bool {
        self.toplevel_drag.is_some_and(|drag| drag.id == id)
    }

    /// Start moving toplevels attached since the last tick, if shown.
    /// Runs after new toplevels are mapped, before they are placed.
    pub(super) fn reconcile_toplevel_drag(&mut self, state: &mut State) {
        self.end_finished_toplevel_drag(state);
        let attached = state.protocols.toplevel_drag.take_attached();
        let Some((surface, offset)) = state.protocols.toplevel_drag.target() else {
            return;
        };
        if self.toplevel_drag.is_some() || !attached.contains(&surface) {
            return;
        }
        let Some(id) = self.surface_index.get(&surface).copied() else {
            return;
        };
        if self.windows.get(&id).is_some_and(|w| !w.unplaced) {
            self.start_toplevel_drag(state, id, offset);
        }
    }

    /// A window shown during its toplevel drag: where it goes, and the
    /// drag starts with it there.
    pub(super) fn place_toplevel_dragged(
        &self,
        state: &State,
        id: u64,
    ) -> Option<Point<i32, Logical>> {
        let (surface, offset) = state.protocols.toplevel_drag.target()?;
        if self.toplevel_drag.is_some() || self.surface_of(id).as_ref() != Some(&surface) {
            return None;
        }
        Some(self.pointer_hotspot(offset))
    }

    /// Begin the window drag of `id` once it is placed.
    pub(super) fn start_placed_toplevel_drag(&mut self, state: &mut State, id: u64) {
        if let Some((surface, offset)) = state.protocols.toplevel_drag.target() {
            if self.toplevel_drag.is_none() && self.surface_of(id).as_ref() == Some(&surface) {
                self.start_toplevel_drag(state, id, offset);
            }
        }
    }

    /// Pointer motion: the window follows at its offset.
    pub(super) fn toplevel_drag_motion(&mut self, state: &mut State, pos: Point<f64, Logical>) {
        self.end_finished_toplevel_drag(state);
        let Some(drag) = self.toplevel_drag.as_mut() else {
            return;
        };
        drag.distance = drag
            .distance
            .max((pos.x - drag.pointer_start.x).abs())
            .max((pos.y - drag.pointer_start.y).abs());
        let (id, offset) = (drag.id, drag.offset);
        let loc = self.pointer_hotspot(offset);
        if let Some(window) = self.windows.get_mut(&id) {
            window.geometry.loc = loc;
        }
    }

    /// Stop the window drag whose toplevel drag ended, or whose window
    /// left it.
    pub(super) fn end_finished_toplevel_drag(&mut self, state: &mut State) {
        let ended = state.protocols.toplevel_drag.take_ended();
        let Some(drag) = self.toplevel_drag else {
            return;
        };
        let surface = self.surface_of(drag.id);
        let target = state.protocols.toplevel_drag.target();
        if surface.is_some() && target.map(|(s, _)| s) == surface {
            return;
        }
        self.toplevel_drag = None;
        let how = ended
            .filter(|(ended, _)| Some(ended) == surface.as_ref())
            .map(|(_, how)| how);
        match how {
            Some(DragEnd::Cancelled) => {
                if let Some(window) = self.windows.get_mut(&drag.id) {
                    window.geometry.loc = drag.start;
                }
                if drag.layout != WindowLayout::Floating {
                    self.apply_layout(state, drag.id, drag.layout);
                }
            }
            Some(DragEnd::Dropped) if self.workspace_policy.edge_tiling => {
                match snap_target(self.pointer_pos, state.primary_size().w) {
                    Some(Snap::Maximize) => {
                        self.set_maximized(state, drag.id, true);
                    }
                    Some(Snap::Tile(side)) => {
                        self.set_tiled(state, drag.id, side);
                    }
                    None => {}
                }
            }
            _ => {}
        }
        if self.focus_policy.grab_raises_at_end(drag.distance)
            && self.windows.contains_key(&drag.id)
        {
            self.raise_family(drag.id);
        }
    }

    fn start_toplevel_drag(&mut self, state: &mut State, id: u64, offset: Point<i32, Logical>) {
        // Scroll-mode columns have no Mutter counterpart: they stay put.
        if self.mode == super::SessionMode::Scroll {
            return;
        }
        let Some(layout) = self.windows.get(&id).map(|w| w.layout) else {
            return;
        };
        self.settle(id);
        if layout != WindowLayout::Floating {
            self.restore_window(id);
        }
        let Some(start) = self.windows.get(&id).map(|w| w.geometry.loc) else {
            return;
        };
        self.toplevel_drag = Some(ToplevelWindowDrag {
            id,
            offset,
            start,
            layout,
            pointer_start: self.pointer_pos,
            distance: 0.0,
        });
        let raise = self.focus_policy.grab_raises_at_start();
        self.apply_focus_raise(state, Some(id), raise);
    }

    /// Window geometry origin with the pointer at `offset` inside it
    /// (Mutter truncates the pointer position).
    fn pointer_hotspot(&self, offset: Point<i32, Logical>) -> Point<i32, Logical> {
        (
            (self.pointer_pos.x as i32).saturating_sub(offset.x),
            (self.pointer_pos.y as i32).saturating_sub(offset.y),
        )
            .into()
    }
}
