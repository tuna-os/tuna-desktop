// SPDX-License-Identifier: GPL-3.0-or-later
// Colour pipeline adapted from Mutter/Clutter:
// Copyright (C) 2022 Intel Corporation
// Copyright (C) 2023-2026 Red Hat Inc.
// Copyright (C) 2024 SUSE SOFTWARE SOLUTIONS GERMANY GMBH
// Representation reference: Copyright (C) 2018 Barco NV,
// authored by Niels De Graef; GPL-2.0-or-later, selecting GPL-3.0-or-later.
// Clutter LGPL-2.0-or-later portions are used under GPL-3.0-or-later
// pursuant to LGPL v2 section 3. See the repository LICENSE and protocol docs.
//! Surfaces drawn through their colour transform (P-DS-03, #341).
//!
//! Mutter attaches each surface's colour pipeline to its texture's Cogl
//! pipeline (`clutter_color_pipeline_shader_set_color_state` from
//! `meta-shaped-texture.c`): sample, colour representation (straight
//! alpha premultiplied, identity coefficients at limited range expanded),
//! then unpremultiply, decode, matrix, encode, premultiply into the
//! output's blending space. On an SDR output that space is the output
//! itself, so blending happens in gamma-2.2 sRGB as before.
//!
//! [`ColorFrame`] resolves, once per drawn frame, every surface whose
//! committed colour state or representation is not the output's
//! (nothing at all when no client uses the protocols). Its entries are
//! keyed by render-element id, so every pass that draws surfaces —
//! windows, layers, popups, overview previews, switcher thumbnails —
//! swaps in [`PROGRAM`] for exactly those elements. The program also
//! carries the dim, wellbeing and fallback night-light uniforms of the
//! tint shaders, so the effects compose as before. Single-pixel buffers
//! are transformed on the CPU ([`color_state::Transform::apply`]).
//!
//! ICC TRCs (256 samples) and 3D LUTs (33³) live in float textures on
//! texture unit 1, sampled without filtering and interpolated in the
//! shader exactly as `clutter-color-pipeline-shader.c` does.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Weak};

use smithay::backend::allocator::{
    dmabuf::{Dmabuf, DmabufFlags, WeakDmabuf},
    Buffer as AllocatorBuffer, Fourcc, Modifier,
};
use smithay::backend::renderer::{
    element::{Id, RenderElement},
    gles::{
        ffi, GlesError, GlesFrame, GlesRenderer, GlesTexProgram, GlesTexture, Uniform, UniformName,
        UniformType, UniformValue,
    },
    utils::with_renderer_surface_state,
    Color32F,
};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Physical, Rectangle};

use crate::color_management::{self, AlphaMode, Coefficients};
use crate::color_state::{self, Decode, Encode, Transform, LUT_3D_SIZE, TRC_SAMPLES};
use crate::wellbeing_effects::FrameColor;
use crate::State;

mod raw;
pub(crate) use raw::identity_ycbcr_supported;
use raw::{raw_planes, RawPlanes};

#[cfg(all(test, feature = "drm"))]
mod raw_tests;

/// How one surface is drawn.
#[derive(Debug, Clone)]
enum Entry {
    /// Through [`PROGRAM`] with these uniforms and, for ICC
    /// profiles, a LUT texture on unit 1.
    Texture {
        uniforms: Vec<Uniform<'static>>,
        lut: Option<u32>,
        raw: Option<Rc<RawPlanes>>,
    },
    /// A single-pixel buffer, colour already transformed.
    Solid(Color32F),
    /// A representation that cannot be sampled exactly must never use
    /// the ordinary driver's guessed YUV conversion.
    Refused,
}

/// What the surfaces of one frame need beyond the default texture
/// program.
#[derive(Debug, Default)]
pub(crate) struct ColorFrame {
    entries: HashMap<Id, Entry>,
    program: Option<GlesTexProgram>,
}

/// Uniforms the colour program shares with the tint shaders.
fn effect_uniforms(tint: Option<&FrameColor>, brightness: f32) -> Vec<Uniform<'static>> {
    match tint {
        Some(tint) => tint.uniforms(brightness),
        None => vec![
            Uniform::new("brightness", brightness),
            Uniform::new("nl_gains", [1.0f32, 1.0, 1.0]),
            Uniform::new("desaturation", 0.0f32),
            Uniform::new("wellbeing_brightness", 1.0f32),
        ],
    }
}

impl ColorFrame {
    /// Resolve every colour-managed surface against output `output`'s
    /// colour state (the primary's when `None`).
    pub(crate) fn new(
        renderer: &mut GlesRenderer,
        state: &mut State,
        output: Option<&str>,
    ) -> Self {
        let output = output
            .map(str::to_owned)
            .or_else(|| state.primary_output_name());
        let target = match output {
            Some(name) => state.protocols.color.output_state(&name),
            None => state.protocols.color.default_state().clone(),
        };
        Self::for_target(renderer, state, target)
    }

    /// Capture uses its own negotiated SDR BT.709/gamma-2.2 target, not
    /// the monitor's ICC profile or HDR scanout state.
    pub(crate) fn for_capture(renderer: &mut GlesRenderer, state: &mut State) -> Self {
        Self::for_target(renderer, state, capture_target())
    }

    pub(crate) fn for_target(
        renderer: &mut GlesRenderer,
        state: &mut State,
        target: Arc<color_state::ColorState>,
    ) -> Self {
        raw::prune(renderer);
        let source_default = state.protocols.color.default_state().clone();
        let mut wanted = Vec::new();
        for surface in state.protocols.color.tracked_surfaces() {
            let color = color_management::current(&surface);
            let transform = source_transform(color.image.as_ref(), &source_default, &target);
            let straight = color.alpha_mode == AlphaMode::Straight;
            let limited = color.coefficients == Coefficients::IdentityLimited;
            let raw = matches!(
                color.coefficients,
                Coefficients::IdentityFull | Coefficients::IdentityLimited
            )
            .then(|| surface_ycbcr(&surface))
            .flatten();
            if transform.is_none() && !straight && !limited && raw.is_none() {
                continue;
            }
            wanted.push((surface, transform, straight, limited, raw));
        }
        if wanted.is_empty() {
            return Self::default();
        }
        let program = program(renderer);
        let cache = LutCache::of(renderer);
        let mut luts = cache.borrow_mut();
        let mut entries = HashMap::new();
        for (surface, transform, straight, limited, dmabuf) in wanted {
            let id = Id::from_wayland_resource(&surface);
            if program.is_none() {
                if dmabuf.is_some() {
                    color_management::reject_identity_ycbcr(
                        &surface,
                        "Raw YCbCr shader unavailable",
                    );
                } else {
                    eprintln!(
                        "tuna-compositor: refusing colour-managed surface: shader unavailable"
                    );
                }
                entries.insert(id, Entry::Refused);
                continue;
            }
            let raw = match dmabuf.as_ref() {
                Some(dmabuf) => match raw_planes(renderer, dmabuf) {
                    Ok(raw) => Some(raw),
                    Err(error) => {
                        color_management::reject_identity_ycbcr(
                            &surface,
                            &format!("Cannot sample identity YCbCr planes: {error}"),
                        );
                        entries.insert(id, Entry::Refused);
                        continue;
                    }
                },
                None => None,
            };
            if let Some(color) = single_pixel(&surface) {
                let mut c = color;
                if limited {
                    for v in &mut c[..3] {
                        *v = (*v - 16.0 / 255.0) * (255.0 / 219.0);
                    }
                }
                if straight {
                    c = [c[0] * c[3], c[1] * c[3], c[2] * c[3], c[3]];
                }
                if let Some(t) = &transform {
                    c = t.apply(c);
                }
                entries.insert(id, Entry::Solid(Color32F::new(c[0], c[1], c[2], c[3])));
                continue;
            }
            let lut = match transform.as_ref().map(|t| &t.decode) {
                Some(Decode::Curve(curves)) => luts.curve(renderer, curves),
                Some(Decode::Lut3d(lut)) => luts.lut3d(renderer, lut),
                _ => None,
            };
            if matches!(
                transform.as_ref().map(|t| &t.decode),
                Some(Decode::Curve(_) | Decode::Lut3d(_))
            ) && lut.is_none()
            {
                eprintln!("tuna-compositor: refusing colour-managed surface: ICC LUT unavailable");
                entries.insert(id, Entry::Refused);
                continue;
            }
            entries.insert(
                id,
                Entry::Texture {
                    uniforms: uniforms(
                        transform.as_ref(),
                        straight,
                        limited,
                        raw.as_ref().map_or(0.0, |r| r.layout),
                    ),
                    lut,
                    raw,
                },
            );
        }
        luts.prune(renderer);
        Self { entries, program }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

fn capture_target() -> Arc<color_state::ColorState> {
    color_state::ColorState::params(color_state::output_params())
}

fn source_transform(
    image: Option<&Arc<color_state::ColorState>>,
    default: &Arc<color_state::ColorState>,
    target: &Arc<color_state::ColorState>,
) -> Option<Transform> {
    color_state::transform(image.unwrap_or(default), target)
}

/// Reuse the surface pipeline for the final full-scene output transform.
pub(crate) fn final_output_program(renderer: &mut GlesRenderer) -> Option<GlesTexProgram> {
    program(renderer)
}

pub(crate) fn final_output_uniforms(transform: &Transform) -> Vec<Uniform<'static>> {
    let mut values = uniforms(Some(transform), false, false, 0.0);
    values.extend(effect_uniforms(None, 1.0));
    values
}

/// The original dmabuf, never a driver-converted RGB texture.
fn surface_ycbcr(surface: &WlSurface) -> Option<Dmabuf> {
    with_renderer_surface_state(surface, |s| {
        let buffer = s.buffer()?;
        let dmabuf = smithay::wayland::dmabuf::get_dmabuf(buffer).ok()?;
        color_management::is_ycbcr(dmabuf.format().code).then(|| dmabuf.clone())
    })
    .flatten()
}

/// A surface's single-pixel buffer colour, premultiplied.
fn single_pixel(surface: &WlSurface) -> Option<[f32; 4]> {
    with_renderer_surface_state(surface, |s| {
        s.buffer().and_then(|b| {
            smithay::wayland::single_pixel_buffer::get_single_pixel_buffer(b)
                .ok()
                .map(|spb| spb.rgba32f())
        })
    })
    .flatten()
}

fn uniforms(
    transform: Option<&Transform>,
    straight: bool,
    limited: bool,
    raw_layout: f32,
) -> Vec<Uniform<'static>> {
    let flag = |b: bool| if b { 1.0f32 } else { 0.0 };
    let (decode, decode_power) = match transform.map(|t| &t.decode) {
        None | Some(Decode::None) => (0.0, 1.0),
        Some(Decode::Power(p)) => (1.0, *p),
        Some(Decode::Srgb) => (2.0, 1.0),
        Some(Decode::Pq) => (3.0, 1.0),
        Some(Decode::Curve(_)) => (4.0, 1.0),
        Some(Decode::Lut3d(_)) => (5.0, 1.0),
    };
    let lut_size = match transform.map(|t| &t.decode) {
        Some(Decode::Curve(_)) => TRC_SAMPLES as f32,
        Some(Decode::Lut3d(_)) => LUT_3D_SIZE as f32,
        _ => 1.0,
    };
    let (encode, encode_power) = match transform.map(|t| t.encode) {
        None | Some(Encode::None) => (0.0, 1.0),
        Some(Encode::Power(p)) => (1.0, p),
        Some(Encode::Srgb) => (2.0, 1.0),
        Some(Encode::Pq) => (3.0, 1.0),
    };
    let m = transform.map_or([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]], |t| {
        t.matrix
    });
    // GLSL matrices are column major.
    let columns = [
        m[0][0], m[1][0], m[2][0], m[0][1], m[1][1], m[2][1], m[0][2], m[1][2], m[2][2],
    ];
    vec![
        Uniform::new("raw_layout", raw_layout),
        Uniform::new("plane0", 2i32),
        Uniform::new("plane1", 3i32),
        Uniform::new("plane2", 4i32),
        Uniform::new("straight", flag(straight)),
        Uniform::new("limited", flag(limited)),
        Uniform::new("transformed", flag(transform.is_some())),
        Uniform::new("decode", decode),
        Uniform::new("decode_power", decode_power),
        Uniform::new(
            "color_matrix",
            UniformValue::Matrix3x3 {
                matrices: vec![columns],
                transpose: false,
            },
        ),
        Uniform::new("encode", encode),
        Uniform::new("encode_power", encode_power),
        Uniform::new("lut", 1i32),
        Uniform::new("lut_size", lut_size),
    ]
}

struct CachedProgram(Option<GlesTexProgram>);

/// The colour program, compiled once per GL context.
fn program(renderer: &mut GlesRenderer) -> Option<GlesTexProgram> {
    if let Some(cached) = renderer.egl_context().user_data().get::<CachedProgram>() {
        return cached.0.clone();
    }
    let names = [
        ("raw_layout", UniformType::_1f),
        ("plane0", UniformType::_1i),
        ("plane1", UniformType::_1i),
        ("plane2", UniformType::_1i),
        ("straight", UniformType::_1f),
        ("limited", UniformType::_1f),
        ("transformed", UniformType::_1f),
        ("decode", UniformType::_1f),
        ("decode_power", UniformType::_1f),
        ("color_matrix", UniformType::Matrix3x3),
        ("encode", UniformType::_1f),
        ("encode_power", UniformType::_1f),
        ("lut", UniformType::_1i),
        ("lut_size", UniformType::_1f),
        ("brightness", UniformType::_1f),
        ("nl_gains", UniformType::_3f),
        ("desaturation", UniformType::_1f),
        ("wellbeing_brightness", UniformType::_1f),
    ]
    .map(|(name, kind)| UniformName::new(name, kind));
    let result = renderer
        .compile_custom_texture_shader(PROGRAM, &names)
        .map_err(|error| eprintln!("tuna-compositor: colour transform shader unavailable: {error}"))
        .ok();
    renderer
        .egl_context()
        .user_data()
        .insert_if_missing(|| CachedProgram(result.clone()));
    result
}

/// Float LUT textures, one per live ICC colour state's data.
#[derive(Default)]
struct LutCache {
    curves: Vec<(Weak<[Vec<f32>; 3]>, u32)>,
    cubes: Vec<(Weak<Vec<[f32; 3]>>, u32)>,
    /// `None` until probed; whether float textures can be created.
    float: Option<bool>,
}

struct LutCacheCell(Rc<RefCell<LutCache>>);

impl LutCache {
    /// The context's cache (shared, so the renderer stays usable for
    /// uploads while it is borrowed).
    fn of(renderer: &mut GlesRenderer) -> Rc<RefCell<LutCache>> {
        let user_data = renderer.egl_context().user_data();
        user_data.insert_if_missing(|| LutCacheCell(Rc::default()));
        user_data
            .get::<LutCacheCell>()
            .expect("inserted above")
            .0
            .clone()
    }

    fn curve(&mut self, renderer: &mut GlesRenderer, curves: &Arc<[Vec<f32>; 3]>) -> Option<u32> {
        if let Some((_, tex)) = self
            .curves
            .iter()
            .find(|(weak, _)| weak.upgrade().is_some_and(|c| Arc::ptr_eq(&c, curves)))
        {
            return Some(*tex);
        }
        let n = curves[0].len();
        let data: Vec<f32> = (0..n)
            .flat_map(|i| [curves[0][i], curves[1][i], curves[2][i], 1.0])
            .collect();
        let tex = self.upload(renderer, n as i32, 1, &data)?;
        self.curves.push((Arc::downgrade(curves), tex));
        Some(tex)
    }

    fn lut3d(&mut self, renderer: &mut GlesRenderer, lut: &Arc<Vec<[f32; 3]>>) -> Option<u32> {
        if let Some((_, tex)) = self
            .cubes
            .iter()
            .find(|(weak, _)| weak.upgrade().is_some_and(|c| Arc::ptr_eq(&c, lut)))
        {
            return Some(*tex);
        }
        let data: Vec<f32> = lut.iter().flat_map(|c| [c[0], c[1], c[2], 1.0]).collect();
        let n = LUT_3D_SIZE as i32;
        let tex = self.upload(renderer, n, n * n, &data)?;
        self.cubes.push((Arc::downgrade(lut), tex));
        Some(tex)
    }

    /// An RGBA float texture without filtering (`RGBA_FP_32323232`).
    fn upload(&mut self, renderer: &mut GlesRenderer, w: i32, h: i32, data: &[f32]) -> Option<u32> {
        if self.float == Some(false) {
            return None;
        }
        let result = renderer.with_context(|gl| unsafe {
            let version = std::ffi::CStr::from_ptr(gl.GetString(ffi::VERSION) as *const _)
                .to_string_lossy()
                .into_owned();
            let gles3 = version.starts_with("OpenGL ES 3") || version.starts_with("OpenGL ES 4");
            let extensions = std::ffi::CStr::from_ptr(gl.GetString(ffi::EXTENSIONS) as *const _)
                .to_string_lossy()
                .into_owned();
            let internal = if gles3 {
                ffi::RGBA32F as i32
            } else if extensions.split(' ').any(|e| e == "GL_OES_texture_float") {
                ffi::RGBA as i32
            } else {
                return None;
            };
            let mut tex = 0;
            gl.GenTextures(1, &mut tex);
            gl.BindTexture(ffi::TEXTURE_2D, tex);
            gl.TexParameteri(
                ffi::TEXTURE_2D,
                ffi::TEXTURE_MIN_FILTER,
                ffi::NEAREST as i32,
            );
            gl.TexParameteri(
                ffi::TEXTURE_2D,
                ffi::TEXTURE_MAG_FILTER,
                ffi::NEAREST as i32,
            );
            gl.TexParameteri(
                ffi::TEXTURE_2D,
                ffi::TEXTURE_WRAP_S,
                ffi::CLAMP_TO_EDGE as i32,
            );
            gl.TexParameteri(
                ffi::TEXTURE_2D,
                ffi::TEXTURE_WRAP_T,
                ffi::CLAMP_TO_EDGE as i32,
            );
            while gl.GetError() != ffi::NO_ERROR {}
            gl.TexImage2D(
                ffi::TEXTURE_2D,
                0,
                internal,
                w,
                h,
                0,
                ffi::RGBA,
                ffi::FLOAT,
                data.as_ptr().cast(),
            );
            let ok = gl.GetError() == ffi::NO_ERROR;
            gl.BindTexture(ffi::TEXTURE_2D, 0);
            if !ok {
                gl.DeleteTextures(1, &tex);
                return None;
            }
            Some(tex)
        });
        let tex = result.ok().flatten();
        if tex.is_none() && self.float.is_none() {
            eprintln!("tuna-compositor: no float textures; ICC colour transforms unavailable");
            self.float = Some(false);
        } else if tex.is_some() {
            self.float = Some(true);
        }
        tex
    }

    /// Delete the textures of colour states that are gone.
    fn prune(&mut self, renderer: &mut GlesRenderer) {
        let mut dead: Vec<u32> = Vec::new();
        self.curves.retain(|(weak, tex)| {
            let alive = weak.strong_count() > 0;
            if !alive {
                dead.push(*tex);
            }
            alive
        });
        self.cubes.retain(|(weak, tex)| {
            let alive = weak.strong_count() > 0;
            if !alive {
                dead.push(*tex);
            }
            alive
        });
        if !dead.is_empty() {
            let _ = renderer.with_context(|gl| unsafe {
                gl.DeleteTextures(dead.len() as i32, dead.as_ptr());
            });
        }
    }
}

/// Draw `elements` front to back over their occlusion `plan` (as
/// [`crate::occlusion::draw_planned`]), the colour-managed ones through
/// the colour program and the rest through the tint program (`tint`),
/// the dim program (`brightness` below 1 with `dim`) or the default one.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_planned<E>(
    frame: &mut GlesFrame<'_, '_>,
    scale: f64,
    elements: &[E],
    plan: &[Vec<Rectangle<i32, Physical>>],
    colors: &ColorFrame,
    tint: Option<&FrameColor>,
    brightness: f32,
    dim: Option<&GlesTexProgram>,
) -> Result<(), GlesError>
where
    E: RenderElement<GlesRenderer>,
{
    let base = |frame: &mut GlesFrame<'_, '_>| {
        if let Some(tint) = tint {
            frame.override_default_tex_program(tint.program.clone(), tint.uniforms(brightness));
        } else if let (true, Some(dim)) = (brightness < 1.0, dim) {
            frame.override_default_tex_program(
                dim.clone(),
                vec![Uniform::new("brightness", brightness)],
            );
        } else {
            frame.clear_tex_program_override();
        }
    };
    base(frame);
    if colors.is_empty() {
        let result =
            crate::occlusion::draw_planned::<GlesRenderer, _>(frame, scale, elements, plan);
        frame.clear_tex_program_override();
        return result;
    }
    let mut overridden = false;
    let mut result = Ok(());
    for (element, visible) in elements.iter().zip(plan).rev() {
        if visible.is_empty() {
            continue;
        }
        let geometry = element.geometry(scale.into());
        let damage: Vec<Rectangle<i32, Physical>> = visible
            .iter()
            .map(|r| {
                let mut r = *r;
                r.loc -= geometry.loc;
                r
            })
            .collect();
        let entry = colors.entries.get(element.id());
        let drawn = match (entry, colors.program.as_ref()) {
            (Some(Entry::Solid(color)), _) => {
                frame.draw_solid(geometry, &damage, *color * element.alpha())
            }
            (Some(Entry::Refused), _) => Ok(()),
            (Some(Entry::Texture { uniforms, lut, raw }), Some(program)) => {
                let mut all = uniforms.clone();
                all.extend(effect_uniforms(tint, brightness));
                frame.override_default_tex_program(program.clone(), all);
                overridden = true;
                if let Some(lut) = *lut {
                    frame.with_context(|gl| unsafe {
                        gl.ActiveTexture(ffi::TEXTURE1);
                        gl.BindTexture(ffi::TEXTURE_2D, lut);
                        gl.ActiveTexture(ffi::TEXTURE0);
                    })?;
                }
                let drawn = if let Some(raw) = raw {
                    raw.with_bindings(frame, |frame| {
                        element.draw(frame, element.src(), geometry, &damage, &[])
                    })
                } else {
                    element.draw(frame, element.src(), geometry, &damage, &[])
                };
                if lut.is_some() {
                    frame.with_context(|gl| unsafe {
                        gl.ActiveTexture(ffi::TEXTURE1);
                        gl.BindTexture(ffi::TEXTURE_2D, 0);
                        gl.ActiveTexture(ffi::TEXTURE0);
                    })?;
                }
                drawn
            }
            _ => {
                if overridden {
                    base(frame);
                    overridden = false;
                }
                element.draw(frame, element.src(), geometry, &damage, &[])
            }
        };
        if let Err(error) = drawn {
            result = Err(error);
            break;
        }
        if overridden {
            base(frame);
            overridden = false;
        }
    }
    frame.clear_tex_program_override();
    result
}

/// The colour program: smithay's texture shader inputs (`tex`,
/// `alpha`, `v_coords`, the `EXTERNAL`/`NO_ALPHA`/`DEBUG_FLAGS`
/// variants), Mutter's colour representation and pipeline snippets,
/// then the tint shaders' effects.
pub(crate) const PROGRAM: &str = r#"#version 100
//_DEFINES_
#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif
#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#define TUNA_SAMPLER_PRECISION highp
#else
precision mediump float;
#define TUNA_SAMPLER_PRECISION
#endif
#if defined(EXTERNAL)
uniform TUNA_SAMPLER_PRECISION samplerExternalOES tex;
#else
uniform TUNA_SAMPLER_PRECISION sampler2D tex;
#endif
uniform float alpha;
varying vec2 v_coords;
#if defined(DEBUG_FLAGS)
uniform float tint;
#endif
uniform float straight;
uniform float limited;
uniform float transformed;
uniform float decode;
uniform float decode_power;
uniform mat3 color_matrix;
uniform float encode;
uniform float encode_power;
uniform TUNA_SAMPLER_PRECISION sampler2D lut;
uniform float lut_size;
uniform float brightness;
uniform vec3 nl_gains;
uniform float desaturation;
uniform float wellbeing_brightness;
uniform float raw_layout;
uniform TUNA_SAMPLER_PRECISION sampler2D plane0;
uniform TUNA_SAMPLER_PRECISION sampler2D plane1;
uniform TUNA_SAMPLER_PRECISION sampler2D plane2;

vec3 gamma_power(vec3 color, float power) {
    vec3 is_negative = vec3(lessThan(color, vec3(0.0)));
    vec3 positive = pow(abs(color), vec3(power));
    return mix(positive, -positive, is_negative);
}

vec3 srgb_piecewise_eotf(vec3 color) {
    vec3 vsign = sign(color);
    color = abs(color);
    vec3 is_low = vec3(lessThanEqual(color, vec3(0.04045)));
    vec3 lo_part = color / 12.92;
    vec3 hi_part = pow((color + 0.055) / 1.055, vec3(12.0 / 5.0));
    return vsign * mix(hi_part, lo_part, is_low);
}

vec3 srgb_piecewise_inv_eotf(vec3 color) {
    vec3 vsign = sign(color);
    color = abs(color);
    vec3 is_lo = vec3(lessThanEqual(color, vec3(0.0031308)));
    vec3 lo_part = color * 12.92;
    vec3 hi_part = pow(color, vec3(5.0 / 12.0)) * 1.055 - 0.055;
    return vsign * mix(hi_part, lo_part, is_lo);
}

vec3 pq_eotf(vec3 color) {
    const float c1 = 0.8359375;
    const float c2 = 18.8515625;
    const float c3 = 18.6875;
    const float oo_m1 = 1.0 / 0.1593017;
    const float oo_m2 = 1.0 / 78.84375;
    color = clamp(color, vec3(0.0), vec3(1.0));
    vec3 color_pow = pow(color, vec3(oo_m2));
    vec3 num = max(color_pow - c1, vec3(0.0));
    vec3 den = c2 - c3 * color_pow;
    return pow(num / den, vec3(oo_m1));
}

vec3 pq_inv_eotf(vec3 color) {
    const float c1 = 0.8359375;
    const float c2 = 18.8515625;
    const float c3 = 18.6875;
    const float m1 = 0.1593017;
    const float m2 = 78.84375;
    color = clamp(color, vec3(0.0), vec3(1.0));
    vec3 in_pow_m1 = pow(color, vec3(m1));
    vec3 num = c1 + c2 * in_pow_m1;
    vec3 den = vec3(1.0) + c3 * in_pow_m1;
    return pow(num / den, vec3(m2));
}

vec4 curve_texel(float i) {
    return texture2D(lut, vec2((i + 0.5) / lut_size, 0.5));
}

/* curve_1d: linear interpolation between the sampled points. */
float curve_channel(float v, int channel) {
    float x = clamp(v, 0.0, 1.0) * (lut_size - 1.0);
    float i = floor(x);
    float j = min(i + 1.0, lut_size - 1.0);
    vec4 lo = curve_texel(i);
    vec4 hi = curve_texel(j);
    float a = channel == 0 ? lo.r : (channel == 1 ? lo.g : lo.b);
    float b = channel == 0 ? hi.r : (channel == 1 ? hi.g : hi.b);
    return mix(a, b, x - i);
}

vec3 lut_texel(vec3 index) {
    float rows = lut_size * lut_size;
    return texture2D(lut, vec2((index.x + 0.5) / lut_size,
                               (index.y + index.z * lut_size + 0.5) / rows)).rgb;
}

/* sample_3d_lut: tetrahedral interpolation. */
vec3 sample_3d_lut(vec3 color) {
    vec3 scaled = clamp(color, 0.0, 1.0) * (lut_size - 1.0);
    vec3 lo = floor(scaled);
    vec3 hi = min(lo + 1.0, lut_size - 1.0);
    vec3 t = scaled - lo;
    vec3 v000 = lut_texel(lo);
    vec3 v111 = lut_texel(hi);
    if (t.x > t.y) {
        if (t.y > t.z) {
            vec3 v100 = lut_texel(vec3(hi.x, lo.y, lo.z));
            vec3 v110 = lut_texel(vec3(hi.x, hi.y, lo.z));
            return v000 + t.x * (v100 - v000) + t.y * (v110 - v100) + t.z * (v111 - v110);
        } else if (t.x > t.z) {
            vec3 v100 = lut_texel(vec3(hi.x, lo.y, lo.z));
            vec3 v101 = lut_texel(vec3(hi.x, lo.y, hi.z));
            return v000 + t.x * (v100 - v000) + t.y * (v111 - v101) + t.z * (v101 - v100);
        } else {
            vec3 v001 = lut_texel(vec3(lo.x, lo.y, hi.z));
            vec3 v101 = lut_texel(vec3(hi.x, lo.y, hi.z));
            return v000 + t.x * (v101 - v001) + t.y * (v111 - v101) + t.z * (v001 - v000);
        }
    } else {
        if (t.z > t.y) {
            vec3 v001 = lut_texel(vec3(lo.x, lo.y, hi.z));
            vec3 v011 = lut_texel(vec3(lo.x, hi.y, hi.z));
            return v000 + t.x * (v111 - v011) + t.y * (v011 - v001) + t.z * (v001 - v000);
        } else if (t.z > t.x) {
            vec3 v010 = lut_texel(vec3(lo.x, hi.y, lo.z));
            vec3 v011 = lut_texel(vec3(lo.x, hi.y, hi.z));
            return v000 + t.x * (v111 - v011) + t.y * (v010 - v000) + t.z * (v011 - v010);
        } else {
            vec3 v010 = lut_texel(vec3(lo.x, hi.y, lo.z));
            vec3 v110 = lut_texel(vec3(hi.x, hi.y, lo.z));
            return v000 + t.x * (v110 - v010) + t.y * (v010 - v000) + t.z * (v111 - v110);
        }
    }
}

void main() {
    vec4 color = texture2D(tex, v_coords);
#if defined(NO_ALPHA)
    color = vec4(color.rgb, 1.0);
#endif
    // Raw component views bypass all EGL YUV conversion and clamping.
    // Layout numbers are shared with raw_layout() below.
    if (raw_layout == 1.0 || raw_layout == 2.0) {
        vec2 uv = texture2D(plane1, v_coords).rg;
        color = vec4(texture2D(plane0, v_coords).r,
                     raw_layout == 1.0 ? uv : uv.yx, 1.0);
    } else if (raw_layout == 3.0 || raw_layout == 4.0) {
        float u = texture2D(plane1, v_coords).r;
        float v = texture2D(plane2, v_coords).r;
        color = vec4(texture2D(plane0, v_coords).r,
                     raw_layout == 3.0 ? vec2(u, v) : vec2(v, u), 1.0);
    } else if (raw_layout == 5.0 || raw_layout == 6.0) {
        color = texture2D(plane0, v_coords);
        if (raw_layout == 6.0) color.a = 1.0;
    } else if (raw_layout == 7.0 || raw_layout == 8.0) {
        vec2 uv = texture2D(plane1, v_coords).ga;
        color = vec4(texture2D(plane0, v_coords).r,
                     raw_layout == 7.0 ? uv : uv.yx, 1.0);
    } else if (raw_layout == 9.0 || raw_layout == 10.0) {
        vec2 uv = texture2D(plane0, v_coords).br;
        color = vec4(texture2D(plane1, v_coords).g,
                     raw_layout == 9.0 ? uv : uv.yx, 1.0);
    }
    if (limited > 0.5)
        color.rgb = 255.0 / 219.0 * (color.rgb - 16.0 / 255.0);
    if (straight > 0.5)
        color.rgb *= color.a;
    if (transformed > 0.5) {
        vec3 c = color.a > 0.0 ? color.rgb / color.a : color.rgb;
        if (decode == 1.0)
            c = gamma_power(c, decode_power);
        else if (decode == 2.0)
            c = srgb_piecewise_eotf(c);
        else if (decode == 3.0)
            c = pq_eotf(c);
        else if (decode == 4.0)
            c = vec3(curve_channel(c.r, 0), curve_channel(c.g, 1), curve_channel(c.b, 2));
        else if (decode == 5.0)
            c = clamp(sample_3d_lut(c), 0.0, 1.0);
        c = color_matrix * c;
        if (encode == 1.0)
            c = gamma_power(c, encode_power);
        else if (encode == 2.0)
            c = srgb_piecewise_inv_eotf(c);
        else if (encode == 3.0)
            c = pq_inv_eotf(c);
        color.rgb = c * color.a;
    }
    color = color * alpha;
    color.rgb *= brightness;
    float gray = dot(color.rgb, vec3(0.299, 0.587, 0.114));
    color.rgb = mix(color.rgb, vec3(gray), desaturation) * wellbeing_brightness;
    color.rgb *= nl_gains;
#if defined(DEBUG_FLAGS)
    if (tint == 1.0) color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif
    gl_FragColor = color;
}
"#;

#[cfg(all(test, feature = "drm"))]
mod tests {
    use super::*;
    use crate::color_state::{
        ColorState, Colorimetry, Colorspace, Eotf, Kind, Luminance, Params, TransferFunction,
    };
    use smithay::backend::allocator::Fourcc;
    use smithay::backend::renderer::{Bind, ExportMem, Frame, ImportMem, Offscreen, Renderer};
    use smithay::utils::{Buffer, Size};

    pub(super) const SIDE: i32 = 8;

    /// Surfaceless EGL, or `None` on a machine without EGL (the test
    /// skips, as the night-light GL test does).
    pub(super) fn headless_renderer() -> Option<GlesRenderer> {
        use smithay::backend::egl::native::EGLSurfacelessDisplay;
        use smithay::backend::egl::{EGLContext, EGLDisplay};
        let display = unsafe { EGLDisplay::new(EGLSurfacelessDisplay) }.ok()?;
        let context = EGLContext::new(&display).ok()?;
        unsafe { GlesRenderer::new(context) }.ok()
    }

    /// 64 premultiplied RGBA8 texels covering the range, a quarter of
    /// them half transparent.
    fn texels() -> Vec<[u8; 4]> {
        (0..SIDE * SIDE)
            .map(|i| {
                let a: u8 = if i % 4 == 3 { 128 } else { 255 };
                let channel = |mul: i32, add: i32| ((i * mul + add) % 256).min(i32::from(a)) as u8;
                [channel(37, 0), channel(91, 13), channel(53, 200), a]
            })
            .collect()
    }

    /// Draw the texels through the colour program and read them back.
    fn draw(
        renderer: &mut GlesRenderer,
        texels: &[[u8; 4]],
        transform: Option<&Transform>,
        straight: bool,
        limited: bool,
    ) -> Vec<[u8; 4]> {
        draw_raw(renderer, texels, transform, straight, limited, None)
    }

    pub(super) fn draw_raw(
        renderer: &mut GlesRenderer,
        texels: &[[u8; 4]],
        transform: Option<&Transform>,
        straight: bool,
        limited: bool,
        raw: Option<&RawPlanes>,
    ) -> Vec<[u8; 4]> {
        let program = program(renderer).expect("colour shader compiles");
        let lut = {
            let cache = LutCache::of(renderer);
            let mut luts = cache.borrow_mut();
            match transform.map(|t| &t.decode) {
                Some(Decode::Curve(curves)) => {
                    Some(luts.curve(renderer, curves).expect("float textures"))
                }
                Some(Decode::Lut3d(lut)) => {
                    Some(luts.lut3d(renderer, lut).expect("float textures"))
                }
                _ => None,
            }
        };
        let size: Size<i32, Buffer> = (SIDE, SIDE).into();
        let bytes: Vec<u8> = texels.iter().flatten().copied().collect();
        let texture = renderer
            .import_memory(&bytes, Fourcc::Abgr8888, size, false)
            .unwrap();
        let mut target: smithay::backend::renderer::gles::GlesTexture =
            renderer.create_buffer(Fourcc::Abgr8888, size).unwrap();
        let mut bound = renderer.bind(&mut target).unwrap();
        let out = Rectangle::<i32, Physical>::from_size((SIDE, SIDE).into());
        {
            let mut frame = renderer
                .render(&mut bound, out.size, smithay::utils::Transform::Normal)
                .unwrap();
            frame.clear(Color32F::TRANSPARENT, &[out]).unwrap();
            let mut all = uniforms(transform, straight, limited, raw.map_or(0.0, |r| r.layout));
            all.extend(effect_uniforms(None, 1.0));
            frame.override_default_tex_program(program, all);
            if let Some(lut) = lut {
                frame
                    .with_context(|gl| unsafe {
                        gl.ActiveTexture(ffi::TEXTURE1);
                        gl.BindTexture(ffi::TEXTURE_2D, lut);
                        gl.ActiveTexture(ffi::TEXTURE0);
                    })
                    .unwrap();
            }
            let draw = |frame: &mut GlesFrame<'_, '_>| {
                frame.render_texture_from_to(
                    &texture,
                    Rectangle::<f64, Buffer>::from_size((f64::from(SIDE), f64::from(SIDE)).into()),
                    out,
                    &[out],
                    &[],
                    smithay::utils::Transform::Normal,
                    1.0,
                    None,
                    &[],
                )
            };
            if let Some(raw) = raw {
                raw.with_bindings(&mut frame, draw).unwrap();
            } else {
                draw(&mut frame).unwrap();
            }
            frame.clear_tex_program_override();
            let _ = frame.finish().unwrap();
        }
        let mapping = renderer
            .copy_framebuffer(&bound, Rectangle::from_size(size), Fourcc::Abgr8888)
            .unwrap();
        let read = renderer.map_texture(&mapping).unwrap();
        read.as_chunks::<4>().0.to_vec()
    }

    /// The CPU reference: the same steps on each texel, in f32.
    pub(super) fn reference(
        texel: [u8; 4],
        transform: Option<&Transform>,
        straight: bool,
        limited: bool,
    ) -> [f32; 4] {
        let mut c = texel.map(|v| f32::from(v) / 255.0);
        if limited {
            for v in &mut c[..3] {
                *v = 255.0 / 219.0 * (*v - 16.0 / 255.0);
            }
        }
        if straight {
            c = [c[0] * c[3], c[1] * c[3], c[2] * c[3], c[3]];
        }
        if let Some(t) = transform {
            c = t.apply(c);
        }
        c.map(|v| v.clamp(0.0, 1.0) * 255.0)
    }

    fn check(
        renderer: &mut GlesRenderer,
        name: &str,
        transform: Option<&Transform>,
        straight: bool,
        limited: bool,
    ) {
        let texels = texels();
        let drawn = draw(renderer, &texels, transform, straight, limited);
        for (texel, got) in texels.iter().zip(&drawn) {
            let want = reference(*texel, transform, straight, limited);
            for k in 0..4 {
                assert!(
                    (f32::from(got[k]) - want[k]).abs() <= 2.0,
                    "{name}: texel {texel:?} drew {got:?}, CPU reference {want:?}"
                );
            }
        }
    }

    fn to_output(source: Params) -> Transform {
        let output = ColorState::params(color_state::output_params());
        color_state::transform(&ColorState::params(source), &output).expect("a transform")
    }

    /// Every decode and encode stage of the shader, against
    /// [`Transform::apply`] (which single-pixel buffers use).
    #[test]
    fn shader_matches_the_cpu_reference() {
        let Some(mut renderer) = headless_renderer() else {
            eprintln!("colour GL test skipped: no surfaceless EGL");
            return;
        };
        let pq = to_output(Params::named(Colorspace::Bt2020, TransferFunction::Pq));
        assert_eq!(pq.decode, Decode::Pq);
        check(&mut renderer, "BT.2020 PQ", Some(&pq), false, false);
        check(
            &mut renderer,
            "BT.2020 PQ, straight, limited",
            Some(&pq),
            true,
            true,
        );
        check(
            &mut renderer,
            "no transform, straight, limited",
            None,
            true,
            true,
        );

        let srgb = to_output(Params::named(
            Colorspace::P3,
            TransferFunction::SrgbPiecewise,
        ));
        assert_eq!(srgb.decode, Decode::Srgb);
        check(&mut renderer, "Display P3 sRGB", Some(&srgb), false, false);

        let power = to_output(Params::new(
            Colorimetry::Colorspace(Colorspace::Ntsc),
            Eotf::Gamma(2.8),
            Some(Luminance {
                min: 0.1,
                max: 200.0,
                reference: 100.0,
                mastering_max: 200.0,
            }),
        ));
        assert!(matches!(power.decode, Decode::Power(_)));
        check(&mut renderer, "NTSC gamma 2.8", Some(&power), false, false);

        let icc =
            color_state::Icc::parse(&crate::icc::DisplayProfile::srgb().encode().unwrap()).unwrap();
        let output = ColorState::params(color_state::output_params());
        let curve = color_state::transform(&ColorState::new(Kind::Icc(icc)), &output).unwrap();
        assert!(
            matches!(curve.decode, Decode::Curve(_)),
            "{:?}",
            curve.decode
        );
        check(&mut renderer, "sRGB ICC TRC", Some(&curve), false, false);

        // A cross-talking 3D LUT, as a LUT-based ICC profile samples.
        let n = color_state::LUT_3D_SIZE;
        let step = 1.0 / (n - 1) as f32;
        let mut cube = Vec::with_capacity(n * n * n);
        for k in 0..n {
            for j in 0..n {
                for i in 0..n {
                    let (r, g, b) = (i as f32 * step, j as f32 * step, k as f32 * step);
                    cube.push([
                        (0.8 * r * r + 0.2 * g).min(1.0),
                        (0.7 * g.powf(2.2) + 0.3 * b).min(1.0),
                        (0.9 * b.powf(1.8) + 0.1 * r).min(1.0),
                    ]);
                }
            }
        }
        let lut = Transform {
            decode: Decode::Lut3d(Arc::new(cube)),
            matrix: [[0.9, 0.1, 0.0], [0.05, 0.9, 0.05], [0.0, 0.1, 0.9]],
            encode: Encode::Power(1.0 / 2.2),
        };
        check(&mut renderer, "3D LUT", Some(&lut), false, false);
        drop(renderer);
    }
}
