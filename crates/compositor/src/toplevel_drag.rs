//! `xdg_toplevel_drag_manager_v1` version 1, as Mutter 51 implements it
//! (`meta-wayland-toplevel-drag.c`, `META_XDG_TOPLEVEL_DRAG_VERSION` 1):
//! a toplevel attached to a client drag-and-drop moves with the pointer
//! (detachable browser tabs).
//!
//! This side keeps the protocol objects and their association with the
//! drag in progress; the window manager moves the window
//! (`windows/toplevel_drag.rs`), as Mutter's toplevel drag starts a
//! window drag. Mutter's rules:
//!
//! - `get_xdg_toplevel_drag` on a data source that already has a toplevel
//!   drag is `invalid_source`. Nothing else is checked (Mutter raises no
//!   error for a source used as a selection, nor `ongoing_drag` on destroy).
//! - A toplevel drag whose drag ended, or whose source died, is inert:
//!   `attach` does nothing (`if (!toplevel_drag->data_source) return`).
//! - `attach` while a live toplevel is attached is `toplevel_attached`. A
//!   destroyed toplevel detaches itself (`on_dragged_window_unmanaging`).
//! - Releasing the drag reports `dnd_drop_performed` to the source before
//!   the drop is resolved, then ends the toplevel drag
//!   (`drag_grab_release`); Escape ends it without (`drag_grab_key`).
//!   Destroying the toplevel drag or its source ends it too.

use smithay::{
    reexports::{
        wayland_protocols::xdg::toplevel_drag::v1::server::{
            xdg_toplevel_drag_manager_v1::{self, XdgToplevelDragManagerV1},
            xdg_toplevel_drag_v1::{self, XdgToplevelDragV1},
        },
        wayland_server::{
            backend::{ClientId, GlobalId},
            protocol::{wl_data_source::WlDataSource, wl_surface::WlSurface},
            Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
        },
    },
    utils::{Logical, Point},
    wayland::shell::xdg::ToplevelSurface,
};

use crate::State;

/// Mutter 51's `META_XDG_TOPLEVEL_DRAG_VERSION`.
pub const VERSION: u32 = 1;

/// How a toplevel drag's drag-and-drop ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DragEnd {
    /// The button was released: the window stays where it was dropped.
    Dropped,
    /// Escape cancelled the drag: the window drag returns the window to
    /// where it started (`process_mouse_move_resize_grab`).
    Cancelled,
}

/// One `xdg_toplevel_drag_v1` (Mutter's `MetaWaylandToplevelDrag`).
struct Record {
    resource: XdgToplevelDragV1,
    /// Cleared when the drag ends; dead once the client destroys it.
    source: Option<WlDataSource>,
    /// Cleared when the drag ends; dead once the toplevel is destroyed.
    dragged: Option<ToplevelSurface>,
    /// Pointer hotspot relative to the toplevel's window geometry.
    offset: Point<i32, Logical>,
}

impl Record {
    fn live_source(&self) -> Option<&WlDataSource> {
        self.source.as_ref().filter(|source| source.is_alive())
    }

    fn live_dragged(&self) -> Option<&ToplevelSurface> {
        self.dragged.as_ref().filter(|toplevel| toplevel.alive())
    }
}

/// The global and every live toplevel drag.
pub(crate) struct ToplevelDrags {
    _global: GlobalId,
    records: Vec<Record>,
    /// Source of the client drag in progress (Mutter's drag grab's
    /// `drag_data_source`), from `ClientDndGrabHandler::started`.
    dnd_source: Option<WlDataSource>,
    /// Set while the compositor cancels the drag (Escape).
    cancelling: bool,
    /// Toplevels attached since the window manager last looked.
    attached: Vec<WlSurface>,
    /// The attached toplevel of the last drag that ended, and how.
    ended: Option<(WlSurface, DragEnd)>,
}

impl ToplevelDrags {
    pub(crate) fn new(dh: &DisplayHandle) -> Self {
        Self {
            _global: dh.create_global::<State, XdgToplevelDragManagerV1, ()>(VERSION, ()),
            records: Vec::new(),
            dnd_source: None,
            cancelling: false,
            attached: Vec::new(),
            ended: None,
        }
    }

    fn record_index(&self, resource: &XdgToplevelDragV1) -> Option<usize> {
        self.records.iter().position(|r| &r.resource == resource)
    }

    /// The toplevel drag of `source`, if it still has one.
    fn record_of(&self, source: &WlDataSource) -> Option<&Record> {
        self.records
            .iter()
            .find(|r| r.live_source() == Some(source))
    }

    /// The toplevel moved with the drag in progress and its offset
    /// (`meta_wayland_data_device_get_toplevel_drag` and its
    /// `dragged_surface`).
    pub(crate) fn target(&self) -> Option<(WlSurface, Point<i32, Logical>)> {
        let source = self.dnd_source.as_ref().filter(|s| s.is_alive())?;
        let record = self.record_of(source)?;
        let dragged = record.live_dragged()?;
        Some((dragged.wl_surface().clone(), record.offset))
    }

    pub(crate) fn take_attached(&mut self) -> Vec<WlSurface> {
        std::mem::take(&mut self.attached)
    }

    pub(crate) fn take_ended(&mut self) -> Option<(WlSurface, DragEnd)> {
        self.ended.take()
    }

    pub(crate) fn drag_started(&mut self, source: Option<WlDataSource>) {
        self.dnd_source = source;
    }

    /// Whether `source`'s release reports `dnd_drop_performed` whatever
    /// the target does: it has a toplevel drag (`drag_grab_release`).
    pub(crate) fn performs_drop(&self, source: &WlDataSource) -> bool {
        self.record_of(source).is_some()
    }

    /// The drag-and-drop ended: end its toplevel drag
    /// (`meta_wayland_toplevel_drag_end`), leaving it inert.
    pub(crate) fn drag_ended(&mut self) {
        let end = if self.cancelling {
            DragEnd::Cancelled
        } else {
            DragEnd::Dropped
        };
        let Some(source) = self.dnd_source.take() else {
            return;
        };
        let Some(record) = self
            .records
            .iter_mut()
            .find(|r| r.source.as_ref() == Some(&source))
        else {
            return;
        };
        if let Some(dragged) = record.dragged.take().filter(|t| t.alive()) {
            self.ended = Some((dragged.wl_surface().clone(), end));
        }
        record.source = None;
    }

    pub(crate) fn set_cancelling(&mut self, cancelling: bool) {
        self.cancelling = cancelling;
    }
}

impl GlobalDispatch<XdgToplevelDragManagerV1, ()> for State {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<XdgToplevelDragManagerV1>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<XdgToplevelDragManagerV1, ()> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        manager: &XdgToplevelDragManagerV1,
        request: xdg_toplevel_drag_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            xdg_toplevel_drag_manager_v1::Request::GetXdgToplevelDrag { id, data_source } => {
                let drags = &mut state.protocols.toplevel_drag;
                let resource = data_init.init(id, ());
                if drags.record_of(&data_source).is_some() {
                    manager.post_error(
                        xdg_toplevel_drag_manager_v1::Error::InvalidSource,
                        "toplevel drag resource already exists on data source",
                    );
                    return;
                }
                drags.records.push(Record {
                    resource,
                    source: Some(data_source),
                    dragged: None,
                    offset: Point::default(),
                });
            }
            xdg_toplevel_drag_manager_v1::Request::Destroy => {}
            _ => {}
        }
    }
}

impl Dispatch<XdgToplevelDragV1, ()> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &XdgToplevelDragV1,
        request: xdg_toplevel_drag_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            xdg_toplevel_drag_v1::Request::Attach {
                toplevel,
                x_offset,
                y_offset,
            } => {
                let surface = state.xdg_shell_state.get_toplevel(&toplevel);
                let drags = &mut state.protocols.toplevel_drag;
                let Some(index) = drags.record_index(resource) else {
                    return;
                };
                let record = &mut drags.records[index];
                // Inert once its drag ended or its source died.
                if record.live_source().is_none() {
                    record.source = None;
                    return;
                }
                let Some(surface) = surface else {
                    return;
                };
                if record.live_dragged().is_some() {
                    resource.post_error(
                        xdg_toplevel_drag_v1::Error::ToplevelAttached,
                        "toplevel drag already has a surface attached",
                    );
                    return;
                }
                record.offset = (x_offset, y_offset).into();
                let wl_surface = surface.wl_surface().clone();
                record.dragged = Some(surface);
                drags.attached.push(wl_surface);
            }
            xdg_toplevel_drag_v1::Request::Destroy => {}
            _ => {}
        }
    }

    fn destroyed(state: &mut Self, _client: ClientId, resource: &XdgToplevelDragV1, _data: &()) {
        // The destructor ends the toplevel drag; the window stays.
        state
            .protocols
            .toplevel_drag
            .records
            .retain(|r| &r.resource != resource);
    }
}
