//! `xdg_toplevel_drag_manager_v1` version 1: Mutter 51's window-drag
//! behavior, with the protocol XML's mandatory lifecycle errors:
//! a toplevel attached to a client drag-and-drop moves with the pointer
//! (detachable browser tabs).
//!
//! This side keeps the protocol objects and their association with the
//! drag in progress; the window manager moves the window
//! (`windows/toplevel_drag.rs`), as Mutter's toplevel drag starts a
//! window drag. Unlike Mutter 51.0 source (commit
//! `138a14fbeef09d49ebf5be8a0cb83b042dd5c841`), we enforce the XML's
//! `invalid_source` for selection/already-started sources and `ongoing_drag`
//! for destroying a drag before its source has ended.
//!
//! - `get_xdg_toplevel_drag` on a source already used for toplevel drag,
//!   selection, or a started DND is `invalid_source`.
//! - A toplevel drag whose drag ended, or whose source died, is inert:
//!   `attach` does nothing (`if (!toplevel_drag->data_source) return`).
//! - `attach` while a live toplevel is attached is `toplevel_attached`. A
//!   destroyed toplevel detaches itself (`on_dragged_window_unmanaging`).
//! - Releasing the drag reports `dnd_drop_performed` to the source before
//!   the drop is resolved, then ends the toplevel drag
//!   (`drag_grab_release`); Escape ends it without (`drag_grab_key`).
//!   Source destruction also stops it; the drag object can then be destroyed.

use smithay::{
    reexports::{
        wayland_protocols::xdg::toplevel_drag::v1::server::{
            xdg_toplevel_drag_manager_v1::{self, XdgToplevelDragManagerV1},
            xdg_toplevel_drag_v1::{self, XdgToplevelDragV1},
        },
        wayland_server::{
            backend::{ClientId, GlobalId},
            protocol::{
                wl_data_source::{self, WlDataSource},
                wl_surface::WlSurface,
            },
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
    /// Retired after its destructor; the source reservation outlives it.
    resource: Option<XdgToplevelDragV1>,
    manager: XdgToplevelDragManagerV1,
    source: WlDataSource,
    inert: bool,
    /// Cleared when the drag ends; dead once the toplevel is destroyed.
    dragged: Option<ToplevelSurface>,
    /// Pointer hotspot relative to the toplevel's window geometry.
    offset: Point<i32, Logical>,
    /// Initial unbuffered commits are not unmaps; only a mapped-to-unmapped
    /// transition detaches a live role.
    mapped: bool,
    attachment: u64,
}

impl Record {
    fn live_source(&self) -> Option<&WlDataSource> {
        (!self.inert && self.resource.is_some() && self.source.is_alive()).then_some(&self.source)
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
    /// The attached toplevel of the last drag that ended, and how.
    ended: Option<(WlSurface, DragEnd)>,
    next_attachment: u64,
}

impl ToplevelDrags {
    pub(crate) fn new(dh: &DisplayHandle) -> Self {
        Self {
            _global: dh.create_global::<State, XdgToplevelDragManagerV1, ()>(VERSION, ()),
            records: Vec::new(),
            dnd_source: None,
            cancelling: false,
            ended: None,
            next_attachment: 0,
        }
    }

    fn record_index(&self, resource: &XdgToplevelDragV1) -> Option<usize> {
        self.records
            .iter()
            .position(|r| r.resource.as_ref() == Some(resource))
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
    pub(crate) fn target(&self) -> Option<(WlSurface, Point<i32, Logical>, u64)> {
        let source = self.dnd_source.as_ref().filter(|s| s.is_alive())?;
        let record = self.record_of(source)?;
        let dragged = record.live_dragged()?;
        Some((
            dragged.wl_surface().clone(),
            record.offset,
            record.attachment,
        ))
    }

    /// Called after Smithay applies each surface commit, not once per tick:
    /// an unmap and remap in one dispatch batch must still detach.
    pub(crate) fn surface_committed(&mut self, surface: &WlSurface) {
        for record in &mut self.records {
            if record.dragged.as_ref().map(ToplevelSurface::wl_surface) != Some(surface) {
                continue;
            }
            let mapped = surface_mapped(surface);
            if record.mapped && !mapped {
                record.dragged = None;
            }
            record.mapped = mapped;
        }
    }

    /// Retire a destroyed source before cancelling Smithay's native grab.
    /// Source destruction stops the move in place, unlike Escape.
    pub(crate) fn retire_dead_source(&mut self) -> bool {
        // A destroyed drag still reserves its original live source for DND.
        // Once both resources are gone there is no association left to keep.
        self.records
            .retain(|record| record.resource.is_some() || record.source.is_alive());
        if !self
            .dnd_source
            .as_ref()
            .is_some_and(|source| !source.is_alive())
        {
            return false;
        }
        let source = self.dnd_source.take();
        for record in &mut self.records {
            if Some(&record.source) == source.as_ref() {
                record.inert = true;
                record.dragged = None;
            }
        }
        true
    }

    pub(crate) fn take_ended(&mut self) -> Option<(WlSurface, DragEnd)> {
        self.ended.take()
    }

    pub(crate) fn drag_started(&mut self, source: Option<WlDataSource>) {
        self.dnd_source = source;
    }

    /// A toplevel-drag source cannot be repurposed as a clipboard selection.
    /// Report the extension error on its live factory. If the factory has
    /// gone, the source still exists and rejects its incompatible use.
    pub(crate) fn selection_allowed(&self, source: &WlDataSource) -> bool {
        let Some(record) = self.records.iter().find(|record| &record.source == source) else {
            return true;
        };
        if record.manager.is_alive() {
            record.manager.post_error(
                xdg_toplevel_drag_manager_v1::Error::InvalidSource,
                "toplevel drag data source cannot be used as a selection",
            );
        } else {
            source.post_error(
                wl_data_source::Error::InvalidSource,
                "toplevel drag data source cannot be used as a selection",
            );
        }
        false
    }

    /// Whether `source`'s release reports `dnd_drop_performed` whatever
    /// the target does: it has a toplevel drag (`drag_grab_release`).
    pub(crate) fn performs_drop(&self, source: &WlDataSource) -> bool {
        self.dnd_source.as_ref() == Some(source) && self.record_of(source).is_some()
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
        let Some(record) = self.records.iter_mut().find(|r| r.source == source) else {
            return;
        };
        if let Some(dragged) = record.dragged.take().filter(|t| t.alive()) {
            self.ended = Some((dragged.wl_surface().clone(), end));
        }
        record.inert = true;
    }

    pub(crate) fn set_cancelling(&mut self, cancelling: bool) {
        self.cancelling = cancelling;
    }
}

fn surface_mapped(surface: &WlSurface) -> bool {
    smithay::backend::renderer::utils::with_renderer_surface_state(surface, |state| {
        state.surface_size().is_some()
    })
    .unwrap_or(false)
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
                let used = smithay::wayland::selection::data_device::with_source_metadata(
                    &data_source,
                    |metadata| metadata.used_for_selection || metadata.drag_started,
                )
                .unwrap_or(true);
                if used
                    || drags
                        .records
                        .iter()
                        .any(|record| record.source == data_source)
                {
                    manager.post_error(
                        xdg_toplevel_drag_manager_v1::Error::InvalidSource,
                        "data source is already used for selection, drag-and-drop, or toplevel drag",
                    );
                    return;
                }
                drags.records.push(Record {
                    resource: Some(resource),
                    manager: manager.clone(),
                    source: data_source,
                    inert: false,
                    dragged: None,
                    offset: Point::default(),
                    mapped: false,
                    attachment: 0,
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
                drags.next_attachment = drags.next_attachment.wrapping_add(1);
                let attachment = drags.next_attachment;
                let Some(index) = drags.record_index(resource) else {
                    return;
                };
                let record = &mut drags.records[index];
                // Inert once its drag ended or its source died.
                if record.live_source().is_none() {
                    record.inert = true;
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
                record.attachment = attachment;
                record.mapped = surface_mapped(surface.wl_surface());
                record.dragged = Some(surface);
            }
            xdg_toplevel_drag_v1::Request::Destroy
                if state
                    .protocols
                    .toplevel_drag
                    .records
                    .iter()
                    .find(|record| record.resource.as_ref() == Some(resource))
                    .is_some_and(|record| record.live_source().is_some()) =>
            {
                resource.post_error(
                    xdg_toplevel_drag_v1::Error::OngoingDrag,
                    "underlying data-source drag has not ended",
                );
            }
            _ => {}
        }
    }

    fn destroyed(state: &mut Self, _client: ClientId, resource: &XdgToplevelDragV1, _data: &()) {
        // Keep the source's DND-only reservation until the source dies.
        if let Some(record) = state
            .protocols
            .toplevel_drag
            .records
            .iter_mut()
            .find(|record| record.resource.as_ref() == Some(resource))
        {
            record.resource = None;
            record.inert = true;
            record.dragged = None;
        }
    }
}
