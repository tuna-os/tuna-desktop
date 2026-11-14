//! ext-background-effect-v1 at Mutter 51's version 1
//! (`meta-wayland-background-effect.c`): a client asks for the
//! content behind part of its surface to be blurred.
//!
//! As in Mutter the global is created unconditionally
//! (`meta_wayland_init_background_effect`) and every bind is told the
//! `blur` capability straight away. One `ext_background_effect_surface_v1`
//! per surface (`background_effect_exists` on the manager otherwise);
//! `set_blur_region` copies the region into the surface's pending state,
//! applied on the next `wl_surface.commit` (synchronised subsurfaces
//! included), a NULL region removing the effect. On a destroyed surface it
//! raises `surface_destroyed`. Destroying the object unsets the pending
//! region, so the blur goes on the next commit, and the surface may take
//! a new object. The blur itself is drawn by `background_blur.rs`.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Mutex;

use smithay::backend::renderer::element::Id;
use smithay::backend::renderer::utils::{CommitCounter, RendererSurfaceStateUserData};
use smithay::reexports::wayland_protocols::ext::background_effect::v1::server::{
    ext_background_effect_manager_v1::{self, ExtBackgroundEffectManagerV1},
    ext_background_effect_surface_v1::{self, ExtBackgroundEffectSurfaceV1},
};
use smithay::reexports::wayland_server::{
    backend::{ClientId, GlobalId},
    protocol::wl_surface::WlSurface,
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, Weak,
};
use smithay::utils::{Logical, Rectangle, Size};
use smithay::wayland::compositor::{
    get_region_attributes, with_states, Cacheable, RectangleKind, RegionAttributes,
};

use crate::State;

/// `META_EXT_BACKGROUND_EFFECT_V1_VERSION` in meta-wayland-versions.h.
pub const VERSION: u32 = 1;

/// The global, and every surface that ever took an effect object.
pub(crate) struct BackgroundEffects {
    _global: GlobalId,
    surfaces: Vec<Weak<WlSurface>>,
}

/// A committed blur region, as the renderer reads it.
#[derive(Debug, Clone)]
pub(crate) struct Blurred {
    /// The surface's render element id (`Id::from_wayland_resource`).
    pub id: Id,
    /// Damage-tracking id of the area the blur samples.
    pub marker: Id,
    /// Changes whenever the committed region does.
    pub commit: CommitCounter,
    /// Surface-local, clipped to the surface's size.
    pub rects: Vec<Rectangle<i32, Logical>>,
    /// The surface's size (its viewport destination).
    pub size: Size<i32, Logical>,
}

impl BackgroundEffects {
    pub(crate) fn new(dh: &DisplayHandle) -> Self {
        Self {
            _global: dh.create_global::<State, ExtBackgroundEffectManagerV1, ()>(VERSION, ()),
            surfaces: Vec::new(),
        }
    }

    /// Live surfaces with a non-empty committed blur region; nothing to
    /// look at (and no work for the renderer) when no client asked.
    pub(crate) fn blurred(&self) -> Vec<Blurred> {
        self.surfaces
            .iter()
            .filter_map(|weak| weak.upgrade().ok())
            .filter_map(|surface| {
                let (rects, size, marker) = with_states(&surface, |states| {
                    let rects = states
                        .cached_state
                        .get::<BlurRegionState>()
                        .current()
                        .region
                        .clone()?;
                    let size = states
                        .data_map
                        .get::<RendererSurfaceStateUserData>()?
                        .lock()
                        .unwrap()
                        .view()?
                        .dst;
                    let marker = states
                        .data_map
                        .get_or_insert_threadsafe(|| BlurMarker(Id::new()))
                        .0
                        .clone();
                    Some((rects, size, marker))
                })?;
                let rects = clip(&rects, size);
                if rects.is_empty() {
                    return None;
                }
                let mut hasher = DefaultHasher::new();
                for rect in &rects {
                    (rect.loc.x, rect.loc.y, rect.size.w, rect.size.h).hash(&mut hasher);
                }
                Some(Blurred {
                    id: Id::from_wayland_resource(&surface),
                    marker,
                    commit: CommitCounter::from(hasher.finish() as usize),
                    rects,
                    size,
                })
            })
            .collect()
    }
}

/// The surface's committed blur region, surface-local and unclipped
/// (`None`: no effect).
pub fn blur_region(surface: &WlSurface) -> Option<Vec<Rectangle<i32, Logical>>> {
    with_states(surface, |states| {
        states
            .cached_state
            .get::<BlurRegionState>()
            .current()
            .region
            .clone()
    })
}

/// The double-buffered blur region (`background_blur_region` and
/// `background_blur_region_set` of MetaWaylandSurfaceState).
#[derive(Debug, Default, Clone)]
struct BlurRegionState {
    region: Option<Vec<Rectangle<i32, Logical>>>,
    set: bool,
}

impl Cacheable for BlurRegionState {
    fn commit(&mut self, _dh: &DisplayHandle) -> Self {
        // Like Mutter's state reset after a commit: nothing pending.
        Self {
            region: self.region.take(),
            set: std::mem::take(&mut self.set),
        }
    }

    fn merge_into(self, into: &mut Self, _dh: &DisplayHandle) {
        if self.set {
            into.region = self.region;
            into.set = true;
        }
    }
}

/// Whether the surface has its effect object (Mutter's
/// `BACKGROUND_EFFECT_SURFACE_DATA`).
#[derive(Default)]
struct EffectObject(Mutex<bool>);

/// The damage-tracking id of a surface's blur sample area.
struct BlurMarker(Id);

/// Regions are kept within this many pixels of the origin: a client's
/// "everything" region (`i32::MAX` wide) must not overflow.
const REGION_LIMIT: i32 = 1 << 24;

/// A wl_region as disjoint rectangles, in the order its adds and
/// subtracts were made.
fn flatten(attributes: &RegionAttributes) -> Vec<Rectangle<i32, Logical>> {
    let mut rects: Vec<Rectangle<i32, Logical>> = Vec::new();
    for (kind, rect) in &attributes.rects {
        let x0 = rect.loc.x.clamp(-REGION_LIMIT, REGION_LIMIT);
        let y0 = rect.loc.y.clamp(-REGION_LIMIT, REGION_LIMIT);
        let x1 = rect
            .loc
            .x
            .saturating_add(rect.size.w)
            .clamp(-REGION_LIMIT, REGION_LIMIT);
        let y1 = rect
            .loc
            .y
            .saturating_add(rect.size.h)
            .clamp(-REGION_LIMIT, REGION_LIMIT);
        if x1 <= x0 || y1 <= y0 {
            continue;
        }
        let rect = Rectangle::new((x0, y0).into(), (x1 - x0, y1 - y0).into());
        match kind {
            RectangleKind::Add => {
                let fresh = Rectangle::subtract_rects_many([rect], rects.iter().copied());
                rects.extend(fresh);
            }
            RectangleKind::Subtract => {
                rects = Rectangle::subtract_rects_many(rects, [rect]);
            }
        }
    }
    rects
}

/// The region clipped to the surface (Mutter's actor-surface sync
/// intersects it with the surface rectangle).
fn clip(
    rects: &[Rectangle<i32, Logical>],
    size: Size<i32, Logical>,
) -> Vec<Rectangle<i32, Logical>> {
    let bounds = Rectangle::from_size(size);
    rects
        .iter()
        .filter_map(|rect| rect.intersection(bounds))
        .filter(|rect| !rect.is_empty())
        .collect()
}

impl GlobalDispatch<ExtBackgroundEffectManagerV1, ()> for State {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<ExtBackgroundEffectManagerV1>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        let manager = data_init.init(resource, ());
        // background_effect_manager_bind: the capabilities, at once.
        manager.capabilities(ext_background_effect_manager_v1::Capability::Blur);
    }
}

impl Dispatch<ExtBackgroundEffectManagerV1, ()> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &ExtBackgroundEffectManagerV1,
        request: ext_background_effect_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            ext_background_effect_manager_v1::Request::GetBackgroundEffect { id, surface } => {
                let exists = with_states(&surface, |states| {
                    let object = states
                        .data_map
                        .get_or_insert_threadsafe(EffectObject::default);
                    let mut exists = object.0.lock().unwrap();
                    std::mem::replace(&mut *exists, true)
                });
                if exists {
                    resource.post_error(
                        ext_background_effect_manager_v1::Error::BackgroundEffectExists,
                        "Background effect resource already exists on surface",
                    );
                    return;
                }
                data_init.init(id, surface.downgrade());
                let effects = &mut state.protocols.background_effects;
                effects.surfaces.retain(|weak| weak.upgrade().is_ok());
                if !effects
                    .surfaces
                    .iter()
                    .any(|weak| weak.id() == surface.id())
                {
                    effects.surfaces.push(surface.downgrade());
                }
            }
            ext_background_effect_manager_v1::Request::Destroy => {}
            _ => unreachable!(),
        }
    }
}

impl Dispatch<ExtBackgroundEffectSurfaceV1, Weak<WlSurface>> for State {
    fn request(
        _state: &mut Self,
        _client: &Client,
        resource: &ExtBackgroundEffectSurfaceV1,
        request: ext_background_effect_surface_v1::Request,
        data: &Weak<WlSurface>,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            ext_background_effect_surface_v1::Request::SetBlurRegion { region } => {
                let Ok(surface) = data.upgrade() else {
                    resource.post_error(
                        ext_background_effect_surface_v1::Error::SurfaceDestroyed,
                        "Surface destroyed",
                    );
                    return;
                };
                // Copy semantics: the wl_region may go right after.
                let region = region.map(|region| flatten(&get_region_attributes(&region)));
                with_states(&surface, |states| {
                    let mut cached = states.cached_state.get::<BlurRegionState>();
                    let pending = cached.pending();
                    pending.region = region;
                    pending.set = true;
                });
            }
            ext_background_effect_surface_v1::Request::Destroy => {}
            _ => unreachable!(),
        }
    }

    fn destroyed(
        _state: &mut Self,
        _client: ClientId,
        _resource: &ExtBackgroundEffectSurfaceV1,
        data: &Weak<WlSurface>,
    ) {
        // background_effect_surface_destructor: unset the pending region
        // (the blur goes on the next commit) and free the surface for a
        // new object.
        let Ok(surface) = data.upgrade() else {
            return;
        };
        with_states(&surface, |states| {
            {
                let mut cached = states.cached_state.get::<BlurRegionState>();
                let pending = cached.pending();
                pending.region = None;
                pending.set = true;
            }
            if let Some(object) = states.data_map.get::<EffectObject>() {
                *object.0.lock().unwrap() = false;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Logical> {
        Rectangle::new((x, y).into(), (w, h).into())
    }

    fn area(rects: &[Rectangle<i32, Logical>]) -> i64 {
        rects
            .iter()
            .map(|r| i64::from(r.size.w) * i64::from(r.size.h))
            .sum()
    }

    #[test]
    fn regions_flatten_in_order_and_clip_to_the_surface() {
        let region = RegionAttributes {
            rects: vec![
                (RectangleKind::Add, rect(0, 0, 100, 100)),
                (RectangleKind::Add, rect(50, 50, 100, 100)),
                (RectangleKind::Subtract, rect(0, 0, 10, 10)),
            ],
        };
        let rects = flatten(&region);
        assert_eq!(area(&rects), 100 * 100 + 100 * 100 - 50 * 50 - 10 * 10);
        for (i, a) in rects.iter().enumerate() {
            for b in &rects[i + 1..] {
                assert!(!a.overlaps(*b), "{a:?} overlaps {b:?}");
            }
        }
        let clipped = clip(&rects, (120, 80).into());
        assert_eq!(area(&clipped), 100 * 80 - 10 * 10 + 20 * 30);
    }

    #[test]
    fn an_everything_region_does_not_overflow() {
        let region = RegionAttributes {
            rects: vec![(RectangleKind::Add, rect(10, 10, i32::MAX, i32::MAX))],
        };
        let clipped = clip(&flatten(&region), (64, 48).into());
        assert_eq!(clipped, vec![rect(10, 10, 54, 38)]);
    }
}
