//! `wp_color_manager_v1` (version 2) and `wp_color_representation_manager_v1`
//! (version 1), as Mutter 51 serves them (P-DS-03, #341).
//!
//! Colour management (`meta-wayland-color-management.c`):
//! - The manager advertises the perceptual intent; the ICC v2/v4,
//!   parametric, set-primaries, set-tf-power and set-luminances
//!   features; gamma 2.2, gamma 2.8, PQ, BT.1886 and linear transfer
//!   functions plus compound power 2.4 (sRGB on version 1); and the
//!   sRGB, BT.2020, NTSC, PAL and Display P3 primaries.
//! - Each output's image description is its colour state
//!   ([`crate::color_state::output_params`]: Mutter's default colour
//!   mode is the only one Tuna Desktop offers, see `display_config`),
//!   with information. A removed output's object fails with `no_output`.
//! - Surface feedback reports the colour state of the surface's main
//!   output (the primary output when it is on none), with information.
//!   Every output has the same parameters, so `preferred_changed` never
//!   fires, exactly as Mutter compares states before sending it.
//! - Parametric and ICC creators build image descriptions without
//!   information, with Mutter's validation and errors (mastering
//!   primaries and luminances are unsupported, max-CLL/FALL ignored,
//!   ICC files are read synchronously instead of on a thread).
//! - A surface's image description is double-buffered state applied on
//!   commit; destroying the surface object reverts it to the default.
//!   [`crate::color_render`] draws surfaces through the transform.
//!
//! Colour representation (`meta-wayland-color-representation.c`):
//! premultiplied-electrical and straight alpha; identity, BT.709, BT.601
//! and BT.2020 coefficients at full and limited range; chroma locations
//! 0–5. A commit with YCbCr coefficients on an RGB buffer, or a chroma
//! location on a buffer that is not 4:2:0, is a `pixel_format` error.
//!
//! Sources (mutter `51.0`): `src/wayland/meta-wayland-color-management.c`,
//! `src/wayland/meta-wayland-color-representation.c`,
//! `src/wayland/meta-wayland-versions.h`.

use std::collections::BTreeSet;
use std::os::fd::{AsFd, OwnedFd};
use std::sync::{Arc, Mutex, Weak as ArcWeak};

use smithay::{
    backend::allocator::Fourcc,
    output::Output,
    reexports::wayland_server::{
        backend::GlobalId, protocol::wl_surface::WlSurface, Client, DataInit, Dispatch,
        DisplayHandle, GlobalDispatch, New, Resource, WEnum, Weak,
    },
    wayland::compositor::{self, with_states, Cacheable},
};
use wayland_protocols::wp::{
    color_management::v1::server::{
        wp_color_management_output_v1::{self as cm_output, WpColorManagementOutputV1},
        wp_color_management_surface_feedback_v1::{
            self as cm_feedback, WpColorManagementSurfaceFeedbackV1,
        },
        wp_color_management_surface_v1::{self as cm_surface, WpColorManagementSurfaceV1},
        wp_color_manager_v1::{self as manager, WpColorManagerV1},
        wp_image_description_creator_icc_v1::{
            self as creator_icc, WpImageDescriptionCreatorIccV1,
        },
        wp_image_description_creator_params_v1::{
            self as creator_params, WpImageDescriptionCreatorParamsV1,
        },
        wp_image_description_info_v1::{self as info, WpImageDescriptionInfoV1},
        wp_image_description_v1::{self as image_desc, WpImageDescriptionV1},
    },
    color_representation::v1::server::{
        wp_color_representation_manager_v1::{
            self as repr_manager, WpColorRepresentationManagerV1,
        },
        wp_color_representation_surface_v1::{self as repr, WpColorRepresentationSurfaceV1},
    },
};

use crate::color_state::{
    self, ColorState, Colorimetry, Colorspace, Eotf, Icc, Kind, Luminance, Params, Primaries,
    TransferFunction,
};
use crate::State;

/// `META_WP_COLOR_MANAGEMENT_VERSION`.
pub const COLOR_MANAGEMENT_VERSION: u32 = 2;
/// `META_WP_COLOR_REPRESENTATION_VERSION`.
pub const COLOR_REPRESENTATION_VERSION: u32 = 1;

/// Largest ICC file a client may hand over (Mutter's 32 MB).
const MAX_ICC_SIZE: u32 = 32 * 1024 * 1024;

/// How a surface's buffer stores alpha (`MetaMultiTextureAlphaMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AlphaMode {
    /// Not set: premultiplied electrical.
    #[default]
    Unset,
    PremultipliedElectrical,
    Straight,
}

/// YCbCr-to-RGB coefficients and quantization range
/// (`MetaMultiTextureCoefficients`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Coefficients {
    /// Not set: identity full range for RGB, BT.709 limited for YCbCr.
    #[default]
    Unset,
    IdentityFull,
    IdentityLimited,
    Bt709Full,
    Bt709Limited,
    Bt601Full,
    Bt601Limited,
    Bt2020Full,
    Bt2020Limited,
}

/// Mutter's `supported_coeffs` table.
const SUPPORTED_COEFFICIENTS: [(repr::Coefficients, repr::Range, Coefficients); 8] = [
    (
        repr::Coefficients::Identity,
        repr::Range::Full,
        Coefficients::IdentityFull,
    ),
    (
        repr::Coefficients::Identity,
        repr::Range::Limited,
        Coefficients::IdentityLimited,
    ),
    (
        repr::Coefficients::Bt709,
        repr::Range::Full,
        Coefficients::Bt709Full,
    ),
    (
        repr::Coefficients::Bt709,
        repr::Range::Limited,
        Coefficients::Bt709Limited,
    ),
    (
        repr::Coefficients::Bt601,
        repr::Range::Full,
        Coefficients::Bt601Full,
    ),
    (
        repr::Coefficients::Bt601,
        repr::Range::Limited,
        Coefficients::Bt601Limited,
    ),
    (
        repr::Coefficients::Bt2020,
        repr::Range::Full,
        Coefficients::Bt2020Full,
    ),
    (
        repr::Coefficients::Bt2020,
        repr::Range::Limited,
        Coefficients::Bt2020Limited,
    ),
];

/// A surface's double-buffered colour state: image description and
/// colour representation, each applied on commit only when set since
/// the last one (Mutter's `has_new_*` flags).
#[derive(Debug, Clone, Default)]
pub struct SurfaceColor {
    /// The image description's colour state; `None` is the default.
    pub image: Option<Arc<ColorState>>,
    pub alpha_mode: AlphaMode,
    pub coefficients: Coefficients,
    /// Whether a chroma location is set.
    pub chroma_location: bool,
    new_image: bool,
    new_alpha_mode: bool,
    new_coefficients: bool,
    new_chroma_location: bool,
    /// Set when a commit changed how the buffer reads; taken by
    /// [`damage_changed`].
    changed: bool,
}

impl Cacheable for SurfaceColor {
    fn commit(&mut self, _dh: &DisplayHandle) -> Self {
        let committed = self.clone();
        self.new_image = false;
        self.new_alpha_mode = false;
        self.new_coefficients = false;
        self.new_chroma_location = false;
        committed
    }

    fn merge_into(self, into: &mut Self, _dh: &DisplayHandle) {
        let same_image = match (&self.image, &into.image) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        if self.new_image && !same_image {
            into.image = self.image;
            into.changed = true;
        }
        if self.new_alpha_mode && into.alpha_mode != self.alpha_mode {
            into.alpha_mode = self.alpha_mode;
            into.changed = true;
        }
        if self.new_coefficients && into.coefficients != self.coefficients {
            into.coefficients = self.coefficients;
            into.changed = true;
        }
        if self.new_chroma_location {
            into.chroma_location = self.chroma_location;
        }
    }
}

/// After a commit: surfaces of the committed tree whose colour state or
/// representation changed repaint whole even without new damage, as
/// Mutter redraws an actor whose colour state changed.
pub(crate) fn damage_changed(surface: &WlSurface) {
    if compositor::is_sync_subsurface(surface) {
        return;
    }
    compositor::with_surface_tree_upward(
        surface,
        (),
        |_, _, _| compositor::TraversalAction::DoChildren(()),
        |_, states, _| {
            let (changed, coefficients) = {
                let mut guard = states.cached_state.get::<SurfaceColor>();
                let current = guard.current();
                (std::mem::take(&mut current.changed), current.coefficients)
            };
            states
                .data_map
                .insert_if_missing_threadsafe(smithay::backend::egl::YuvConversionHint::default);
            if let Some(hint) = states
                .data_map
                .get::<smithay::backend::egl::YuvConversionHint>()
            {
                *hint.0.lock().unwrap() = yuv_conversion(coefficients);
            }
            if changed {
                if let Some(renderer) =
                    states
                        .data_map
                        .get::<smithay::backend::renderer::utils::RendererSurfaceStateUserData>()
                {
                    renderer.lock().unwrap().invalidate_buffer();
                }
            }
        },
        |_, _, _| true,
    );
}

/// The EGL import conversion for a surface's YCbCr buffers. Unset
/// coefficients are BT.709 limited, Mutter's default. Identity has no
/// EGL hint, so it falls back to the driver default (a divergence).
fn yuv_conversion(coefficients: Coefficients) -> Option<smithay::backend::egl::YuvConversion> {
    use smithay::backend::egl::{YuvColorSpace as S, YuvConversion, YuvRange as R};
    let (color_space, range) = match coefficients {
        Coefficients::Unset | Coefficients::Bt709Limited => (S::Rec709, R::Narrow),
        Coefficients::Bt709Full => (S::Rec709, R::Full),
        Coefficients::Bt601Full => (S::Rec601, R::Full),
        Coefficients::Bt601Limited => (S::Rec601, R::Narrow),
        Coefficients::Bt2020Full => (S::Rec2020, R::Full),
        Coefficients::Bt2020Limited => (S::Rec2020, R::Narrow),
        Coefficients::IdentityFull | Coefficients::IdentityLimited => return None,
    };
    Some(YuvConversion { color_space, range })
}

fn with_pending(surface: &WlSurface, f: impl FnOnce(&mut SurfaceColor)) {
    with_states(surface, |states| {
        f(states.cached_state.get::<SurfaceColor>().pending())
    });
}

/// The colour state and representation a surface was last committed
/// with.
pub fn current(surface: &WlSurface) -> SurfaceColor {
    with_states(surface, |states| {
        states.cached_state.get::<SurfaceColor>().current().clone()
    })
}

/// Per-surface protocol objects (`MetaWaylandColorManagementSurface`
/// and the representation surface).
#[derive(Default)]
struct SurfaceObjects {
    surface: Option<WpColorManagementSurfaceV1>,
    feedback: Vec<WpColorManagementSurfaceFeedbackV1>,
    preferred: Option<Arc<ColorState>>,
    representation: Option<WpColorRepresentationSurfaceV1>,
}

type SurfaceObjectsData = Mutex<SurfaceObjects>;

fn with_objects<R>(surface: &WlSurface, f: impl FnOnce(&mut SurfaceObjects) -> R) -> R {
    with_states(surface, |states| {
        let objects = states
            .data_map
            .get_or_insert_threadsafe(SurfaceObjectsData::default);
        let mut objects = objects.lock().unwrap();
        f(&mut objects)
    })
}

/// Compositor-wide colour-management state.
pub(crate) struct ColorManagement {
    _manager: GlobalId,
    _representation: GlobalId,
    /// `clutter_context_get_default_color_state`.
    default: Arc<ColorState>,
    /// Each output's colour state, by output name.
    outputs: Vec<(String, Arc<ColorState>)>,
    /// Surfaces that ever had a colour object: what the renderer looks
    /// at each frame.
    surfaces: Vec<Weak<WlSurface>>,
    /// Version 1 `ready`/`preferred_changed` identities
    /// (`get_image_description_id`): per state, freed ids reused lowest
    /// first.
    v1_ids: Vec<(ArcWeak<ColorState>, u32)>,
    v1_free: BTreeSet<u32>,
    v1_next: u32,
    /// `get_information` replies waiting for the end of dispatch.
    information: Vec<(WpImageDescriptionInfoV1, Arc<ColorState>)>,
}

impl ColorManagement {
    pub(crate) fn new(dh: &DisplayHandle) -> Self {
        Self {
            _manager: dh.create_global::<State, WpColorManagerV1, ()>(COLOR_MANAGEMENT_VERSION, ()),
            _representation: dh.create_global::<State, WpColorRepresentationManagerV1, ()>(
                COLOR_REPRESENTATION_VERSION,
                (),
            ),
            default: ColorState::params(color_state::default_params()),
            outputs: Vec::new(),
            surfaces: Vec::new(),
            v1_ids: Vec::new(),
            v1_free: BTreeSet::new(),
            v1_next: 0,
            information: Vec::new(),
        }
    }

    /// The default colour state (surfaces without an image description).
    pub(crate) fn default_state(&self) -> &Arc<ColorState> {
        &self.default
    }

    /// The colour state of output `name`, created on first use.
    pub(crate) fn output_state(&mut self, name: &str) -> Arc<ColorState> {
        if let Some((_, state)) = self.outputs.iter().find(|(n, _)| n == name) {
            return state.clone();
        }
        let state = ColorState::params(color_state::output_params());
        self.outputs.push((name.to_owned(), state.clone()));
        state
    }

    /// Forget a removed output's colour state.
    pub(crate) fn output_removed(&mut self, name: &str) {
        self.outputs.retain(|(n, _)| n != name);
    }

    fn track(&mut self, surface: &WlSurface) {
        self.surfaces.retain(|weak| weak.upgrade().is_ok());
        if !self
            .surfaces
            .iter()
            .any(|weak| weak.upgrade().is_ok_and(|s| s == *surface))
        {
            self.surfaces.push(surface.downgrade());
        }
    }

    /// A destroyed surface leaves the renderer's list.
    pub(crate) fn surface_destroyed(&mut self, surface: &WlSurface) {
        self.surfaces
            .retain(|weak| weak.upgrade().is_ok_and(|s| s != *surface));
    }

    /// Live surfaces that ever had a colour object.
    pub(crate) fn tracked_surfaces(&self) -> impl Iterator<Item = WlSurface> + '_ {
        self.surfaces.iter().filter_map(|weak| weak.upgrade().ok())
    }

    fn v1_id(&mut self, state: &Arc<ColorState>) -> u32 {
        let mut freed = Vec::new();
        self.v1_ids.retain(|(weak, id)| {
            let alive = weak.strong_count() > 0;
            if !alive {
                freed.push(*id);
            }
            alive
        });
        self.v1_free.extend(freed);
        if let Some((_, id)) = self
            .v1_ids
            .iter()
            .find(|(weak, _)| weak.upgrade().is_some_and(|s| Arc::ptr_eq(&s, state)))
        {
            return *id;
        }
        let id = match self.v1_free.pop_first() {
            Some(id) => id,
            None => {
                self.v1_next += 1;
                self.v1_next
            }
        };
        self.v1_ids.push((Arc::downgrade(state), id));
        id
    }
}

/// An image description's state (`MetaWaylandImageDescription`).
#[derive(Debug, Default)]
pub struct ImageDescription(Mutex<Description>);

#[derive(Debug, Default)]
enum Description {
    #[default]
    Init,
    Ready {
        state: Arc<ColorState>,
        has_info: bool,
    },
    Failed,
}

impl ImageDescription {
    fn ready_state(&self) -> Option<Arc<ColorState>> {
        match &*self.0.lock().unwrap() {
            Description::Ready { state, .. } => Some(state.clone()),
            _ => None,
        }
    }
}

/// `meta_wayland_image_description_set_ready`.
fn set_ready(
    state: &mut State,
    resource: &WpImageDescriptionV1,
    color: Arc<ColorState>,
    has_info: bool,
) {
    if resource.version() >= 2 {
        let id = color.id();
        resource.ready2((id >> 32) as u32, id as u32);
    } else {
        let id = state.protocols.color.v1_id(&color);
        resource.ready(id);
    }
    *resource
        .data::<ImageDescription>()
        .unwrap()
        .0
        .lock()
        .unwrap() = Description::Ready {
        state: color,
        has_info,
    };
}

/// `meta_wayland_image_description_set_failed`.
fn set_failed(resource: &WpImageDescriptionV1, cause: image_desc::Cause, message: &str) {
    *resource
        .data::<ImageDescription>()
        .unwrap()
        .0
        .lock()
        .unwrap() = Description::Failed;
    resource.failed(cause, message.to_owned());
}

/// `float_to_scaled_uint32_chromaticity`: single precision, truncated.
fn chromaticity(v: f32) -> i32 {
    (v * 1_000_000.0) as u32 as i32
}

fn send_primaries(info: &WpImageDescriptionInfoV1, p: &Primaries) {
    let c = chromaticity;
    info.primaries(
        c(p.r[0]),
        c(p.r[1]),
        c(p.g[0]),
        c(p.g[1]),
        c(p.b[0]),
        c(p.b[1]),
        c(p.w[0]),
        c(p.w[1]),
    );
    info.target_primaries(
        c(p.r[0]),
        c(p.r[1]),
        c(p.g[0]),
        c(p.g[1]),
        c(p.b[0]),
        c(p.b[1]),
        c(p.w[0]),
        c(p.w[1]),
    );
}

fn primaries_to_wayland(space: Colorspace) -> manager::Primaries {
    match space {
        Colorspace::Srgb => manager::Primaries::Srgb,
        Colorspace::Bt2020 => manager::Primaries::Bt2020,
        Colorspace::Ntsc => manager::Primaries::Ntsc,
        Colorspace::Pal => manager::Primaries::Pal,
        Colorspace::P3 => manager::Primaries::DisplayP3,
    }
}

/// `clutter_tf_to_wayland`: piece-wise sRGB is `srgb` on version 1 and
/// `compound_power_2_4` from version 2.
fn tf_to_wayland(version: u32, tf: TransferFunction) -> manager::TransferFunction {
    match tf {
        TransferFunction::Gamma22 => manager::TransferFunction::Gamma22,
        TransferFunction::SrgbPiecewise if version >= 2 => {
            manager::TransferFunction::CompoundPower24
        }
        TransferFunction::SrgbPiecewise => manager::TransferFunction::Srgb,
        TransferFunction::Pq => manager::TransferFunction::St2084Pq,
        TransferFunction::Bt1886 => manager::TransferFunction::Bt1886,
        TransferFunction::Linear => manager::TransferFunction::ExtLinear,
    }
}

/// `wayland_tf_to_clutter`.
fn tf_from_wayland(version: u32, tf: manager::TransferFunction) -> Option<Eotf> {
    use manager::TransferFunction as T;
    Some(match tf {
        T::Gamma22 => Eotf::Named(TransferFunction::Gamma22),
        T::Gamma28 => Eotf::Gamma(2.8),
        // Deprecated in version 2; Mutter's version 1 reads it as gamma 2.2.
        T::Srgb if version < 2 => Eotf::Named(TransferFunction::Gamma22),
        T::St2084Pq => Eotf::Named(TransferFunction::Pq),
        T::Bt1886 => Eotf::Named(TransferFunction::Bt1886),
        T::ExtLinear => Eotf::Named(TransferFunction::Linear),
        T::CompoundPower24 if version >= 2 => Eotf::Named(TransferFunction::SrgbPiecewise),
        _ => return None,
    })
}

/// `send_information_from_params`. Only parametric states carry
/// information: outputs and surface feedback.
fn send_information(info: &WpImageDescriptionInfoV1, color: &ColorState) {
    let Kind::Params(params) = color.kind() else {
        return;
    };
    match params.colorimetry() {
        Colorimetry::Colorspace(space) => {
            info.primaries_named(primaries_to_wayland(space));
            send_primaries(info, &space.primaries());
        }
        Colorimetry::Primaries(p) => send_primaries(info, &p),
    }
    match params.eotf() {
        Eotf::Named(tf) => info.tf_named(tf_to_wayland(info.version(), tf)),
        Eotf::Gamma(exp) if (exp - 2.8).abs() < 0.0001 => {
            info.tf_named(manager::TransferFunction::Gamma28)
        }
        Eotf::Gamma(exp) => info.tf_power((exp * 10000.0) as u32),
    }
    let lum = params.luminance();
    info.luminances(
        (lum.min * 10000.0) as u32,
        lum.max as u32,
        lum.reference as u32,
    );
    info.target_luminance((lum.min * 10000.0) as u32, lum.max as u32);
}

/// `color_manager_send_supported_events`.
fn send_supported(resource: &WpColorManagerV1) {
    resource.supported_intent(manager::RenderIntent::Perceptual);
    for feature in [
        manager::Feature::IccV2V4,
        manager::Feature::Parametric,
        manager::Feature::SetPrimaries,
        manager::Feature::SetTfPower,
        manager::Feature::SetLuminances,
    ] {
        resource.supported_feature(feature);
    }
    for tf in [
        manager::TransferFunction::Gamma22,
        manager::TransferFunction::Gamma28,
        manager::TransferFunction::St2084Pq,
        manager::TransferFunction::Bt1886,
        manager::TransferFunction::ExtLinear,
    ] {
        resource.supported_tf_named(tf);
    }
    for primaries in [
        manager::Primaries::Srgb,
        manager::Primaries::Bt2020,
        manager::Primaries::Ntsc,
        manager::Primaries::Pal,
        manager::Primaries::DisplayP3,
    ] {
        resource.supported_primaries_named(primaries);
    }
    resource.supported_tf_named(if resource.version() < 2 {
        manager::TransferFunction::Srgb
    } else {
        manager::TransferFunction::CompoundPower24
    });
    resource.done();
}

impl State {
    /// Answer the `get_information` requests of the last dispatch; call
    /// after `Display::dispatch_clients`, before flushing.
    pub fn send_color_information(&mut self) {
        for (info, color) in std::mem::take(&mut self.protocols.color.information) {
            if info.is_alive() {
                send_information(&info, &color);
                info.done();
            }
        }
    }

    /// The output whose colour state a surface prefers (Mutter's main
    /// monitor): its layer's output, its window's, its parent's, the one
    /// a new window would open on, or the primary output.
    fn main_output_name(&self, surface: &WlSurface) -> Option<String> {
        let mut surface = surface.clone();
        for _ in 0..64 {
            if let Some(parent) = compositor::get_parent(&surface) {
                surface = parent;
                continue;
            }
            if let Some(layer) = self.panel_surfaces.iter().find(|l| l.surface == surface) {
                return layer
                    .output_name
                    .clone()
                    .or_else(|| self.primary_output_name());
            }
            if let Some(loc) = self.window_origins.get(&surface) {
                let rect = smithay::utils::Rectangle::new(*loc, (1, 1).into());
                return self
                    .outputs
                    .iter()
                    .find(|e| smithay::utils::Rectangle::new(e.loc.into(), e.size).overlaps(rect))
                    .map(|e| e.name.clone())
                    .or_else(|| self.primary_output_name());
            }
            let parent = self
                .xdg_shell_state
                .popup_surfaces()
                .iter()
                .find(|p| *p.wl_surface() == surface)
                .and_then(|p| p.get_parent_surface());
            match parent {
                Some(parent) => surface = parent,
                None => break,
            }
        }
        self.new_window_output()
            .map(|e| e.name.clone())
            .or_else(|| self.primary_output_name())
    }

    /// `update_preferred_color_state`: the main output's colour state;
    /// subscribers hear only when it is not equal to the last one.
    fn update_preferred(&mut self, surface: &WlSurface) {
        let preferred = match self.main_output_name(surface) {
            Some(name) => self.protocols.color.output_state(&name),
            None => self.protocols.color.default_state().clone(),
        };
        let (initial, changed, feedback) = with_objects(surface, |objects| {
            objects.feedback.retain(Resource::is_alive);
            let initial = objects.preferred.is_none();
            if objects
                .preferred
                .as_ref()
                .is_some_and(|old| old.equals(&preferred))
            {
                return (initial, false, Vec::new());
            }
            objects.preferred = Some(preferred.clone());
            (initial, true, objects.feedback.clone())
        });
        if initial || !changed {
            return;
        }
        for resource in feedback {
            if resource.version() >= 2 {
                let id = preferred.id();
                resource.preferred_changed2((id >> 32) as u32, id as u32);
            } else {
                let id = self.protocols.color.v1_id(&preferred);
                resource.preferred_changed(id);
            }
        }
    }

    /// `meta_wayland_color_representation_commit_check`, after the
    /// commit applied: YCbCr coefficients need a YCbCr buffer, a chroma
    /// location a 4:2:0 one.
    pub(crate) fn check_color_representation(&mut self, surface: &WlSurface) {
        let Some(resource) = with_objects(surface, |o| o.representation.clone()) else {
            return;
        };
        let Some(format) = buffer_format(surface) else {
            return;
        };
        let committed = current(surface);
        if committed.chroma_location && !is_420(format) {
            resource.post_error(
                repr::Error::PixelFormat,
                "Commit contains a color representation with chroma location set and \
                 a buffer with a pixel format which is not 4:2:0 subsampled",
            );
            return;
        }
        let ycbcr = !matches!(
            committed.coefficients,
            Coefficients::Unset | Coefficients::IdentityFull | Coefficients::IdentityLimited
        );
        if ycbcr && !is_ycbcr(format) {
            resource.post_error(
                repr::Error::PixelFormat,
                "Commit contains a color representation with coefficients for a \
                 YCbCr pixel format and a buffer with an RGB pixel format",
            );
        }
    }
}

/// The pixel format of a surface's current buffer (single-pixel
/// buffers are RGB).
fn buffer_format(surface: &WlSurface) -> Option<Fourcc> {
    let buffer = smithay::backend::renderer::utils::with_renderer_surface_state(surface, |s| {
        s.buffer().map(|b| (**b).clone())
    })??;
    if let Ok(dmabuf) = smithay::wayland::dmabuf::get_dmabuf(&buffer) {
        use smithay::backend::allocator::Buffer;
        return Some(dmabuf.format().code);
    }
    if let Ok(format) =
        smithay::wayland::shm::with_buffer_contents(&buffer, |_, _, data| data.format)
    {
        return smithay::wayland::shm::shm_format_to_fourcc(format);
    }
    Some(Fourcc::Abgr8888)
}

/// YCbCr formats (`!meta_multi_texture_is_simple` for imported YCbCr).
pub(crate) fn is_ycbcr(format: Fourcc) -> bool {
    use Fourcc::*;
    is_420(format)
        || matches!(
            format,
            Yuyv | Yvyu
                | Uyvy
                | Vyuy
                | Ayuv
                | Xyuv8888
                | Vuy888
                | Vuy101010
                | Y210
                | Y212
                | Y216
                | Y410
                | Y412
                | Y416
                | Xvyu2101010
                | Xvyu12_16161616
                | Xvyu16161616
                | Nv16
                | Nv61
                | Nv24
                | Nv42
                | P210
                | Yuv410
                | Yvu410
                | Yuv411
                | Yvu411
                | Yuv422
                | Yvu422
                | Yuv444
                | Yvu444
                | Q410
                | Q401
        )
}

/// 4:2:0 subsampled formats.
fn is_420(format: Fourcc) -> bool {
    use Fourcc::*;
    matches!(
        format,
        Nv12 | Nv21
            | Nv15
            | P010
            | P012
            | P016
            | Yuv420
            | Yvu420
            | Yuv420_8bit
            | Yuv420_10bit
            | Y0l0
            | X0l0
    )
}

impl GlobalDispatch<WpColorManagerV1, ()> for State {
    fn bind(
        _: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        resource: New<WpColorManagerV1>,
        _: &(),
        init: &mut DataInit<'_, Self>,
    ) {
        let resource = init.init(resource, ());
        send_supported(&resource);
    }
}

/// The output a `wp_color_management_output_v1` describes; `None` once
/// the `wl_output` lost its output.
pub struct OutputData(Option<Output>);

/// The surface a surface or feedback object belongs to.
pub struct SurfaceData(Weak<WlSurface>);

impl SurfaceData {
    fn surface(&self) -> Option<WlSurface> {
        self.0.upgrade().ok()
    }
}

/// A parametric creator's settings (`MetaWaylandCreatorParams`).
#[derive(Default)]
pub struct ParamsCreator(Mutex<ParamsSettings>);

#[derive(Default)]
struct ParamsSettings {
    colorimetry: Option<Colorimetry>,
    eotf: Option<Eotf>,
    luminance: Option<Luminance>,
}

/// An ICC creator's file (`MetaWaylandCreatorIcc`).
#[derive(Default)]
pub struct IccCreator(Mutex<Option<(OwnedFd, u32, u32)>>);

impl Dispatch<WpColorManagerV1, ()> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        resource: &WpColorManagerV1,
        request: manager::Request,
        _: &(),
        _: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        match request {
            manager::Request::GetOutput { id, output } => {
                init.init(id, OutputData(Output::from_resource(&output)));
            }
            manager::Request::GetSurface { id, surface } => {
                let object = init.init(id, SurfaceData(surface.downgrade()));
                let exists = with_objects(&surface, |objects| {
                    if objects.surface.as_ref().is_some_and(Resource::is_alive) {
                        return true;
                    }
                    objects.surface = Some(object);
                    false
                });
                if exists {
                    resource.post_error(manager::Error::SurfaceExists, "surface already requested");
                    return;
                }
                state.protocols.color.track(&surface);
            }
            manager::Request::GetSurfaceFeedback { id, surface } => {
                let object = init.init(id, SurfaceData(surface.downgrade()));
                with_objects(&surface, |objects| objects.feedback.push(object));
                state.protocols.color.track(&surface);
                state.update_preferred(&surface);
            }
            manager::Request::CreateIccCreator { obj } => {
                init.init(obj, IccCreator::default());
            }
            manager::Request::CreateParametricCreator { obj } => {
                init.init(obj, ParamsCreator::default());
            }
            manager::Request::CreateWindowsScrgb { image_description } => {
                init.init(image_description, ImageDescription::default());
                resource.post_error(
                    manager::Error::UnsupportedFeature,
                    "Windows scRGB is not supported",
                );
            }
            manager::Request::GetImageDescription {
                image_description, ..
            } => {
                // No protocol Tuna Desktop (or Mutter) serves creates
                // wp_image_description_reference_v1 objects.
                init.init(image_description, ImageDescription::default());
                resource.post_error(
                    manager::Error::UnsupportedFeature,
                    "image description references are not supported",
                );
            }
            manager::Request::CreateWindowsBt2100 { image_description } => {
                // Version 3 only; the global is version 2.
                init.init(image_description, ImageDescription::default());
                resource.post_error(
                    manager::Error::UnsupportedFeature,
                    "Windows BT.2100 is not supported",
                );
            }
            manager::Request::Destroy => {}
            _ => {}
        }
    }
}

impl Dispatch<WpColorManagementOutputV1, OutputData> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &WpColorManagementOutputV1,
        request: cm_output::Request,
        data: &OutputData,
        _: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        if let cm_output::Request::GetImageDescription { image_description } = request {
            let resource = init.init(image_description, ImageDescription::default());
            let name = data
                .0
                .as_ref()
                .map(Output::name)
                .filter(|name| state.outputs.iter().any(|e| &e.name == name));
            match name {
                Some(name) => {
                    let color = state.protocols.color.output_state(&name);
                    set_ready(state, &resource, color, true);
                }
                None => set_failed(
                    &resource,
                    image_desc::Cause::NoOutput,
                    "Underlying output object has been destroyed",
                ),
            }
        }
    }
}

impl Dispatch<WpColorManagementSurfaceV1, SurfaceData> for State {
    fn request(
        _: &mut Self,
        _: &Client,
        resource: &WpColorManagementSurfaceV1,
        request: cm_surface::Request,
        data: &SurfaceData,
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        let image = match request {
            cm_surface::Request::SetImageDescription {
                image_description,
                render_intent,
            } => {
                let Some(surface) = data.surface() else {
                    resource.post_error(
                        cm_surface::Error::Inert,
                        "Underlying surface object has been destroyed",
                    );
                    return;
                };
                let Some(color) = image_description
                    .data::<ImageDescription>()
                    .and_then(ImageDescription::ready_state)
                else {
                    resource.post_error(
                        cm_surface::Error::ImageDescription,
                        "Trying to set an image description which is not ready",
                    );
                    return;
                };
                if render_intent != WEnum::Value(manager::RenderIntent::Perceptual) {
                    resource.post_error(
                        cm_surface::Error::RenderIntent,
                        "Trying to use an unsupported rendering intent",
                    );
                    return;
                }
                (surface, Some(color))
            }
            cm_surface::Request::UnsetImageDescription => {
                let Some(surface) = data.surface() else {
                    resource.post_error(
                        cm_surface::Error::Inert,
                        "Underlying surface object has been destroyed",
                    );
                    return;
                };
                (surface, None)
            }
            _ => return,
        };
        set_image(&image.0, image.1);
    }

    fn destroyed(
        _: &mut Self,
        _: smithay::reexports::wayland_server::backend::ClientId,
        resource: &WpColorManagementSurfaceV1,
        data: &SurfaceData,
    ) {
        // `color_management_surface_destructor`: back to the default.
        if let Some(surface) = data.surface() {
            set_image(&surface, None);
            with_objects(&surface, |objects| {
                if objects.surface.as_ref() == Some(resource) {
                    objects.surface = None;
                }
            });
        }
    }
}

/// `set_image_description`: `None` is the default colour state.
fn set_image(surface: &WlSurface, color: Option<Arc<ColorState>>) {
    with_pending(surface, |pending| {
        pending.image = color;
        pending.new_image = true;
    });
}

impl Dispatch<WpColorManagementSurfaceFeedbackV1, SurfaceData> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        resource: &WpColorManagementSurfaceFeedbackV1,
        request: cm_feedback::Request,
        data: &SurfaceData,
        _: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        // Mutter has only parametric preferences: both are the same.
        let (cm_feedback::Request::GetPreferred { image_description }
        | cm_feedback::Request::GetPreferredParametric { image_description }) = request
        else {
            return;
        };
        let object = init.init(image_description, ImageDescription::default());
        let Some(surface) = data.surface() else {
            resource.post_error(
                cm_feedback::Error::Inert,
                "Underlying surface object has been destroyed",
            );
            return;
        };
        let preferred = with_objects(&surface, |objects| objects.preferred.clone())
            .unwrap_or_else(|| state.protocols.color.default_state().clone());
        set_ready(state, &object, preferred, true);
    }

    fn destroyed(
        _: &mut Self,
        _: smithay::reexports::wayland_server::backend::ClientId,
        resource: &WpColorManagementSurfaceFeedbackV1,
        data: &SurfaceData,
    ) {
        if let Some(surface) = data.surface() {
            with_objects(&surface, |objects| {
                objects.feedback.retain(|f| f != resource)
            });
        }
    }
}

impl Dispatch<WpImageDescriptionCreatorIccV1, IccCreator> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        resource: &WpImageDescriptionCreatorIccV1,
        request: creator_icc::Request,
        data: &IccCreator,
        _: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        match request {
            creator_icc::Request::SetIccFile {
                icc_profile,
                offset,
                length,
            } => {
                let mut file = data.0.lock().unwrap();
                if file.is_some() {
                    resource.post_error(
                        creator_icc::Error::AlreadySet,
                        "The ICC file was already set",
                    );
                    return;
                }
                if let Err((error, message)) = check_icc_file(&icc_profile, offset, length) {
                    resource.post_error(error, message);
                    return;
                }
                *file = Some((icc_profile, offset, length));
            }
            creator_icc::Request::Create { image_description } => {
                let file = data.0.lock().unwrap().take();
                let object = init.init(image_description, ImageDescription::default());
                let Some((fd, offset, length)) = file else {
                    resource.post_error(
                        creator_icc::Error::IncompleteSet,
                        "The ICC file has not been set",
                    );
                    return;
                };
                match read_icc(&fd, offset, length).and_then(|bytes| Icc::parse(&bytes)) {
                    Ok(icc) => set_ready(state, &object, ColorState::new(Kind::Icc(icc)), false),
                    Err(message) => {
                        set_failed(&object, image_desc::Cause::OperatingSystem, &message)
                    }
                }
            }
            _ => {}
        }
    }
}

/// `creator_icc_set_icc_file`'s checks.
fn check_icc_file(
    fd: &OwnedFd,
    offset: u32,
    length: u32,
) -> Result<(), (creator_icc::Error, &'static str)> {
    use rustix::fs::{fcntl_getfl, fstat, seek, OFlags, SeekFrom};
    let readable = fcntl_getfl(fd).is_ok_and(|flags| !flags.contains(OFlags::WRONLY));
    if !readable || seek(fd, SeekFrom::Current(0)).is_err() {
        return Err((
            creator_icc::Error::BadFd,
            "The ICC file is not readable and seekable",
        ));
    }
    if length == 0 || length > MAX_ICC_SIZE {
        return Err((
            creator_icc::Error::BadSize,
            "The size is 0 or bigger than 32 MB",
        ));
    }
    let Ok(stat) = fstat(fd) else {
        return Err((
            creator_icc::Error::BadFd,
            "Couldn't fstat the ICC profile fd",
        ));
    };
    if stat.st_size < i64::from(offset) + i64::from(length) {
        return Err((
            creator_icc::Error::OutOfFile,
            "ICC file shorter than expected",
        ));
    }
    Ok(())
}

/// `meta_read_bytes`: exactly `length` bytes at `offset`.
fn read_icc(fd: &OwnedFd, offset: u32, length: u32) -> Result<Vec<u8>, String> {
    let mut bytes = vec![0u8; length as usize];
    let mut done = 0;
    while done < bytes.len() {
        match rustix::io::pread(
            fd.as_fd(),
            &mut bytes[done..],
            u64::from(offset) + done as u64,
        ) {
            Ok(0) => return Err("Unexpected end of ICC file".to_owned()),
            Ok(n) => done += n,
            Err(rustix::io::Errno::INTR) => {}
            Err(e) => return Err(format!("Couldn't read ICC file: {e}")),
        }
    }
    Ok(bytes)
}

impl Dispatch<WpImageDescriptionCreatorParamsV1, ParamsCreator> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        resource: &WpImageDescriptionCreatorParamsV1,
        request: creator_params::Request,
        data: &ParamsCreator,
        _: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        use creator_params::Error as E;
        let mut settings = data.0.lock().unwrap();
        match request {
            creator_params::Request::Create { image_description } => {
                let object = init.init(image_description, ImageDescription::default());
                let (Some(colorimetry), Some(eotf)) = (settings.colorimetry, settings.eotf) else {
                    resource.post_error(E::IncompleteSet, "Not all required parameters were set");
                    return;
                };
                let params = Params::new(colorimetry, eotf, settings.luminance);
                drop(settings);
                set_ready(state, &object, ColorState::params(params), false);
            }
            creator_params::Request::SetTfNamed { tf } => {
                if settings.eotf.is_some() {
                    resource.post_error(
                        E::AlreadySet,
                        "The transfer characteristics were already set",
                    );
                    return;
                }
                let eotf = match tf {
                    WEnum::Value(tf) => tf_from_wayland(resource.version(), tf),
                    WEnum::Unknown(_) => None,
                };
                let Some(eotf) = eotf else {
                    resource
                        .post_error(E::InvalidTf, "The named transfer function is not supported");
                    return;
                };
                settings.eotf = Some(eotf);
            }
            creator_params::Request::SetTfPower { eexp } => {
                if settings.eotf.is_some() {
                    resource.post_error(
                        E::AlreadySet,
                        "The transfer characteristics were already set",
                    );
                    return;
                }
                if !(10000..=100000).contains(&eexp) {
                    resource.post_error(E::InvalidTf, "The exponent must be between 1.0 and 10.0");
                    return;
                }
                let exp = eexp as f32 * 0.0001;
                settings.eotf = Some(if (exp - 2.2).abs() < 0.0001 {
                    Eotf::Named(TransferFunction::Gamma22)
                } else {
                    Eotf::Gamma(exp)
                });
            }
            creator_params::Request::SetPrimariesNamed { primaries } => {
                if settings.colorimetry.is_some() {
                    resource.post_error(E::AlreadySet, "The primaries were already set");
                    return;
                }
                let space = match primaries {
                    WEnum::Value(manager::Primaries::Srgb) => Some(Colorspace::Srgb),
                    WEnum::Value(manager::Primaries::Bt2020) => Some(Colorspace::Bt2020),
                    WEnum::Value(manager::Primaries::Ntsc) => Some(Colorspace::Ntsc),
                    WEnum::Value(manager::Primaries::Pal) => Some(Colorspace::Pal),
                    WEnum::Value(manager::Primaries::DisplayP3) => Some(Colorspace::P3),
                    _ => None,
                };
                let Some(space) = space else {
                    resource.post_error(
                        E::InvalidPrimariesNamed,
                        "The named primaries are not supported",
                    );
                    return;
                };
                settings.colorimetry = Some(Colorimetry::Colorspace(space));
            }
            creator_params::Request::SetPrimaries {
                r_x,
                r_y,
                g_x,
                g_y,
                b_x,
                b_y,
                w_x,
                w_y,
            } => {
                if settings.colorimetry.is_some() {
                    resource.post_error(E::AlreadySet, "The primaries were already set");
                    return;
                }
                let c = |v: i32| v as f32 * 0.000001;
                let primaries = Primaries {
                    r: [c(r_x), c(r_y)],
                    g: [c(g_x), c(g_y)],
                    b: [c(b_x), c(b_y)],
                    w: [c(w_x), c(w_y)],
                };
                // Mutter warns and clamps out-of-range chromaticities.
                settings.colorimetry = Some(Colorimetry::Primaries(primaries.clamped()));
            }
            creator_params::Request::SetLuminances {
                min_lum,
                max_lum,
                reference_lum,
            } => {
                if settings.luminance.is_some() {
                    resource.post_error(E::AlreadySet, "The luminance was already set");
                    return;
                }
                let (min, max, reference) = (
                    min_lum as f32 * 0.0001,
                    max_lum as f32,
                    reference_lum as f32,
                );
                if max <= min {
                    resource.post_error(
                        E::InvalidLuminance,
                        "The maximum luminance is smaller than the minimum luminance",
                    );
                    return;
                }
                if reference <= min {
                    resource.post_error(
                        E::InvalidLuminance,
                        "The reference luminance is less or equal to the minimum luminance",
                    );
                    return;
                }
                if reference > max {
                    resource.post_error(
                        E::InvalidLuminance,
                        "The reference luminance is bigger than the maximum luminance, \
                         extended target volume unsupported",
                    );
                    return;
                }
                settings.luminance = Some(Luminance {
                    min,
                    max,
                    reference,
                    mastering_max: 0.0,
                });
            }
            creator_params::Request::SetMasteringDisplayPrimaries { .. } => {
                resource.post_error(
                    manager::Error::UnsupportedFeature as u32,
                    "Setting mastering display primaries is not supported",
                );
            }
            creator_params::Request::SetMasteringLuminance { .. } => {
                resource.post_error(
                    manager::Error::UnsupportedFeature as u32,
                    "Setting mastering display luminances is not supported",
                );
            }
            // Ignored, as in Mutter.
            creator_params::Request::SetMaxCll { .. }
            | creator_params::Request::SetMaxFall { .. } => {}
            _ => {}
        }
    }
}

impl Dispatch<WpImageDescriptionV1, ImageDescription> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        resource: &WpImageDescriptionV1,
        request: image_desc::Request,
        data: &ImageDescription,
        _: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        if let image_desc::Request::GetInformation { information } = request {
            let described = match &*data.0.lock().unwrap() {
                Description::Ready { state, has_info } => Some((state.clone(), *has_info)),
                _ => None,
            };
            let info = init.init(information, ());
            match described {
                None => resource.post_error(
                    image_desc::Error::NotReady,
                    "The image description is not ready",
                ),
                Some((_, false)) => resource.post_error(
                    image_desc::Error::NoInformation,
                    "The image description has no information",
                ),
                // `done` destroys the info object, which wayland-backend
                // must still find when this request returns: send after
                // dispatch ([`State::send_color_information`]).
                Some((color, true)) => state.protocols.color.information.push((info, color)),
            }
        }
    }
}

impl Dispatch<WpImageDescriptionInfoV1, ()> for State {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &WpImageDescriptionInfoV1,
        _: info::Request,
        _: &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
    }
}

impl GlobalDispatch<WpColorRepresentationManagerV1, ()> for State {
    fn bind(
        _: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        resource: New<WpColorRepresentationManagerV1>,
        _: &(),
        init: &mut DataInit<'_, Self>,
    ) {
        let resource = init.init(resource, ());
        resource.supported_alpha_mode(repr::AlphaMode::PremultipliedElectrical);
        resource.supported_alpha_mode(repr::AlphaMode::Straight);
        for (coefficients, range, _) in SUPPORTED_COEFFICIENTS {
            resource.supported_coefficients_and_ranges(coefficients, range);
        }
        resource.done();
    }
}

impl Dispatch<WpColorRepresentationManagerV1, ()> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        resource: &WpColorRepresentationManagerV1,
        request: repr_manager::Request,
        _: &(),
        _: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        if let repr_manager::Request::GetSurface { id, surface } = request {
            let object = init.init(id, SurfaceData(surface.downgrade()));
            let exists = with_objects(&surface, |objects| {
                if objects
                    .representation
                    .as_ref()
                    .is_some_and(Resource::is_alive)
                {
                    return true;
                }
                objects.representation = Some(object);
                false
            });
            if exists {
                resource.post_error(
                    repr_manager::Error::SurfaceExists,
                    "a wp_color_representation_v1 object already exists for this surface",
                );
                return;
            }
            state.protocols.color.track(&surface);
        }
    }
}

impl Dispatch<WpColorRepresentationSurfaceV1, SurfaceData> for State {
    fn request(
        _: &mut Self,
        _: &Client,
        resource: &WpColorRepresentationSurfaceV1,
        request: repr::Request,
        data: &SurfaceData,
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        if matches!(request, repr::Request::Destroy) {
            return;
        }
        let Some(surface) = data.surface() else {
            resource.post_error(
                repr::Error::Inert,
                "Underlying surface object has been destroyed",
            );
            return;
        };
        match request {
            repr::Request::SetAlphaMode { alpha_mode } => {
                let mode = match alpha_mode {
                    WEnum::Value(repr::AlphaMode::PremultipliedElectrical) => {
                        AlphaMode::PremultipliedElectrical
                    }
                    WEnum::Value(repr::AlphaMode::Straight) => AlphaMode::Straight,
                    _ => {
                        resource.post_error(repr::Error::AlphaMode, "Unsupported alpha mode");
                        return;
                    }
                };
                with_pending(&surface, |pending| {
                    pending.alpha_mode = mode;
                    pending.new_alpha_mode = true;
                });
            }
            repr::Request::SetCoefficientsAndRange {
                coefficients,
                range,
            } => {
                let found = SUPPORTED_COEFFICIENTS.iter().find(|(c, r, _)| {
                    coefficients == WEnum::Value(*c) && range == WEnum::Value(*r)
                });
                let Some((_, _, value)) = found else {
                    resource.post_error(repr::Error::Coefficients, "Unsupported coefficients");
                    return;
                };
                with_pending(&surface, |pending| {
                    pending.coefficients = *value;
                    pending.new_coefficients = true;
                });
            }
            repr::Request::SetChromaLocation { chroma_location } => {
                if !matches!(chroma_location, WEnum::Value(_)) {
                    // Mutter posts WL_DISPLAY_ERROR_INVALID_OBJECT (0).
                    resource.post_error(0u32, "Invalid chroma location");
                    return;
                }
                with_pending(&surface, |pending| {
                    pending.chroma_location = true;
                    pending.new_chroma_location = true;
                });
            }
            _ => {}
        }
    }

    fn destroyed(
        _: &mut Self,
        _: smithay::reexports::wayland_server::backend::ClientId,
        resource: &WpColorRepresentationSurfaceV1,
        data: &SurfaceData,
    ) {
        // `unset_pending_color_representation`.
        if let Some(surface) = data.surface() {
            with_pending(&surface, |pending| {
                pending.alpha_mode = AlphaMode::Unset;
                pending.coefficients = Coefficients::Unset;
                pending.chroma_location = false;
                pending.new_alpha_mode = true;
                pending.new_coefficients = true;
                pending.new_chroma_location = true;
            });
            with_objects(&surface, |objects| {
                if objects.representation.as_ref() == Some(resource) {
                    objects.representation = None;
                }
            });
        }
    }
}
