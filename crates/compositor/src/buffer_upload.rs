//! Committed buffers are uploaded at once, and `wl_shm` buffers handed
//! back to their client (#1084).
//!
//! Mutter copies a `wl_shm` buffer into a texture when it applies the
//! commit and does not hold the buffer afterwards
//! (`meta_wayland_surface_apply_state` in meta-wayland-surface.c:
//! `buffer_held` is false for SHM buffers), so `wl_buffer.release`
//! follows the commit at once and the client draws its next frame into
//! the same buffer. Smithay uploads a buffer only when its surface is
//! drawn and holds it until another replaces it.
//!
//! Clients are written against the first behavior. GTK's cairo renderer
//! before 4.22 forgets a buffer the compositor still holds when its
//! surface changes size, and never destroys it when the release arrives
//! (GTK commit 8a39f8e6, "wayland: Avoid a leak in corner cases",
//! gtk#7078, first in 4.21.5): held until replaced, every size change of
//! a cairo-drawn surface (the overview's search surface growing for its
//! results) left one buffer mapped in the client and its pool's
//! descriptor open here.
//!
//! So every committed surface is imported on the tick of its commit,
//! drawn or not (a hidden window resizes too), and a `wl_shm` buffer is
//! released as soon as its pixels are in the texture. The renderer is the
//! session's only one, which is what makes the texture the only reader.
//! The released buffer stays attached until another replaces it, so a
//! client that destroys it meanwhile frees its pool then.

use smithay::{
    backend::renderer::{
        utils::{import_surface, RendererSurfaceStateUserData},
        ImportAll, Renderer, Texture,
    },
    reexports::wayland_server::{backend::ObjectId, protocol::wl_surface::WlSurface, Resource},
    wayland::compositor::{with_surface_tree_downward, TraversalAction},
};

use crate::State;

/// Surfaces committed since the last [`upload_committed`], by id: a
/// `WlSurface` kept here would outlive its client's destroy.
#[derive(Debug, Default)]
pub(crate) struct Committed(Vec<ObjectId>);

impl Committed {
    /// `surface` was committed (its buffer state is already applied).
    pub(crate) fn push(&mut self, surface: &WlSurface) {
        let id = surface.id();
        if !self.0.contains(&id) {
            self.0.push(id);
        }
    }
}

/// Import what the surfaces committed since the last call attached, and
/// release their `wl_shm` buffers. `renderer` must be the one the session
/// draws every surface with.
pub fn upload_committed<R>(renderer: &mut R, state: &mut State)
where
    R: Renderer + ImportAll,
    R::TextureId: Texture + 'static,
{
    if state.committed.0.is_empty() {
        return;
    }
    let context = renderer.context_id();
    for id in state.committed.0.drain(..) {
        // A surface destroyed after its commit has nothing left to upload.
        let Ok(surface) = WlSurface::from_id(&state.dh, id) else {
            continue;
        };
        if !surface.is_alive() {
            continue;
        }
        with_surface_tree_downward(
            &surface,
            (),
            |_, _, _| TraversalAction::DoChildren(()),
            |_, states, _| {
                // A buffer that fails to import (logged by Smithay) stays
                // held until another replaces it, as before.
                let _ = import_surface(renderer, states);
                if let Some(data) = states.data_map.get::<RendererSurfaceStateUserData>() {
                    data.lock()
                        .unwrap()
                        .release_imported_shm_buffer(context.clone());
                }
            },
            |_, _, _| true,
        );
    }
}
