//! Screen magnifier (#349): gnome-shell 51.0's zoom region as a
//! compositor render transform.
//!
//! The GTK shell mirrors `org.gnome.desktop.a11y.magnifier` into
//! `$XDG_RUNTIME_DIR/tuna-magnifier.json` (see the `tuna-magnifier`
//! crate). Each output then draws in three passes while the magnifier
//! is on: the unmagnified scene into an offscreen texture (with the
//! locate-pointer ripples, which GNOME keeps in the cloned `uiGroup`),
//! the zoom region into a second texture (GNOME's `mainGroup`: the
//! system background, the region of interest scaled by the factor, the
//! cross-hairs and the magnified pointer sprite), and finally the real
//! framebuffer: the unmagnified scene where the region does not cover
//! the screen, the zoom region through the color-effect shader, its
//! accent border, and the system pointer when it is outside the region.
//! Screenshots and casts read back the same passes.
//!
//! Pointer input stays in unmagnified coordinates as in GNOME: the
//! sprite is drawn where the pointer's position maps through the zoom,
//! so a click lands on exactly what the magnified sprite points at.
//! Absolute touch points are mapped back through the zoom
//! ([`MagnifierState::unzoom`]), so a tap hits what is under the finger.
//!
//! Geometry is per-output physical pixels, so each monitor zooms its
//! own region from the same tracking points (GNOME 51 keeps one zoom
//! region over the whole stage; see `docs/accessibility-shell.md`).
//!
//! Behavior reference: gnome-shell 51.0 `js/ui/magnifier.js`
//! (`ZoomRegion`, `Crosshairs`, `MagShaderEffects`), with the effect
//! shaders of mutter 51.0 `clutter-desaturate-effect.c`,
//! `clutter-brightness-contrast-effect.c` and gnome-shell
//! `src/shell-invert-lightness-effect.c`.

use std::collections::HashMap;
use std::time::{Duration, SystemTime};

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::gles::{
    GlesError, GlesFrame, GlesRenderer, GlesTexProgram, GlesTexture, Uniform, UniformName,
    UniformType,
};
use smithay::backend::renderer::{Bind, Color32F, Frame, Offscreen, Renderer, Texture};
use smithay::utils::{Buffer as BufferCoords, Physical, Rectangle, Size, Transform};
use tuna_shell_control::MotionPolicy;

pub use tuna_magnifier::{MagnifierConfig, ScreenPosition, TrackingMode};

/// `.magnifier-zoom-region { border: 2px solid -st-accent-color }`
/// (gnome-shell 51.0 `_a11y.scss`), logical pixels; none full-screen.
pub const BORDER_WIDTH: f64 = 2.0;
/// `CROSSHAIRS_CLIP_SIZE` (logical) when `cross-hairs-clip` is on.
pub const CROSS_HAIRS_CLIP: f64 = 100.0;
/// `Background.SystemBackground`'s `DEFAULT_BACKGROUND_COLOR`, shown
/// where the region of interest runs past the screen.
pub const SYSTEM_BACKGROUND: [u8; 3] = [40, 40, 40];
/// `POINTER_REST_TIME`: caret and focus moves wait this long after the
/// pointer last moved.
pub const POINTER_REST: Duration = Duration::from_millis(1000);
/// `scrollContentsTo` and `setMagFactor` ease over 100 ms
/// (`EASE_OUT_QUAD`).
pub const SCROLL_EASE_MS: f64 = 100.0;

/// A rectangle in output-local physical pixels: x, y, width, height.
pub type Rect = (i32, i32, i32, i32);

/// JavaScript's `Math.round` (halves round up), as GNOME's geometry.
fn js_round(value: f64) -> f64 {
    (value + 0.5).floor()
}

/// The viewport GNOME asks for at a screen position (`setTopHalf` and
/// friends), before `_setViewPort` rounds and clamps it.
pub fn position_request(screen: (f64, f64), position: ScreenPosition) -> (f64, f64, f64, f64) {
    let (w, h) = screen;
    match position {
        ScreenPosition::FullScreen => (0.0, 0.0, w, h),
        ScreenPosition::TopHalf => (0.0, 0.0, w, h / 2.0),
        ScreenPosition::BottomHalf => (0.0, h / 2.0, w, h / 2.0),
        ScreenPosition::LeftHalf => (0.0, 0.0, w / 2.0, h),
        ScreenPosition::RightHalf => (w / 2.0, 0.0, w / 2.0, h),
    }
}

/// `_setViewPort`: the size rounded and capped at the screen, the
/// origin clamped inside it and rounded.
pub fn place_viewport(request: (f64, f64, f64, f64), screen: (f64, f64)) -> Rect {
    let w = js_round(request.2.min(screen.0)).max(1.0);
    let h = js_round(request.3.min(screen.1)).max(1.0);
    let x = js_round(request.0.max(0.0).min(screen.0 - w)).max(0.0);
    let y = js_round(request.1.max(0.0).min(screen.1 - h)).max(0.0);
    (x as i32, y as i32, w as i32, h as i32)
}

/// ROI size for a viewport at a magnification factor
/// (`getROI`: viewport / mag factor per axis).
pub fn roi_size(viewport_w: i32, viewport_h: i32, mag: f64) -> (f64, f64) {
    (f64::from(viewport_w) / mag, f64::from(viewport_h) / mag)
}

/// New ROI center for one tracking event, following
/// `_centerFromPoint{Centered,Proportional,Push}`. `point` and
/// `current` are output-local physical pixels; `screen` is the output
/// size; `view` is the viewport size; `cursor` the pointer sprite size
/// that push tracking keeps inside the region.
#[allow(clippy::too_many_arguments)]
pub fn track_center(
    mode: TrackingMode,
    point: (f64, f64),
    current: (f64, f64),
    roi: (f64, f64),
    screen: (f64, f64),
    view: (f64, f64),
    mag: f64,
    cursor: (f64, f64),
) -> (f64, f64) {
    match mode {
        TrackingMode::None => current,
        TrackingMode::Centered => point,
        TrackingMode::Proportional => {
            // Proportional: the point's place in the view mirrors its
            // place on screen, padded by a fifth of the viewport
            // (unscaled, so divided by the factor).
            let (half_w, half_h) = (screen.0 / 2.0, screen.1 / 2.0);
            let unscaled = view.0.min(view.1) / 5.0;
            let (pad_x, pad_y) = (unscaled / mag, unscaled / mag);
            let prop_x = if half_w > 0.0 {
                (point.0 - half_w) / half_w
            } else {
                0.0
            };
            let prop_y = if half_h > 0.0 {
                (point.1 - half_h) / half_h
            } else {
                0.0
            };
            (
                point.0 - prop_x * (roi.0 / 2.0 - pad_x),
                point.1 - prop_y * (roi.1 / 2.0 - pad_y),
            )
        }
        TrackingMode::Push => {
            // Push: keep the current center unless the point left the
            // ROI (less the sprite at the far edges), then shift just
            // enough to bring it back into view.
            let (rx, ry) = (current.0 - roi.0 / 2.0, current.1 - roi.1 / 2.0);
            let push = |p: f64, lo: f64, size: f64, sprite: f64, c: f64| {
                let hi = lo + size - sprite;
                if p < lo {
                    c - (lo - p)
                } else if p > hi {
                    c + (p - hi)
                } else {
                    c
                }
            };
            (
                push(point.0, rx, roi.0, cursor.0, current.0),
                push(point.1, ry, roi.1, cursor.1, current.1),
            )
        }
    }
}

/// `_changeROI`'s clamp: with `scroll-at-edges` off (GNOME's default)
/// the region stays on screen, a region larger than the screen hugging
/// its top-left; with scrolling allowed the center passes through and
/// the region shows the system background past the edge.
pub fn clamp_center(
    center: (f64, f64),
    roi: (f64, f64),
    screen: (f64, f64),
    scroll_at_edges: bool,
) -> (f64, f64) {
    if scroll_at_edges {
        return center;
    }
    let axis = |c: f64, size: f64, screen: f64| c.min(screen - size / 2.0).max(size / 2.0);
    (
        axis(center.0, roi.0, screen.0),
        axis(center.1, roi.1, screen.1),
    )
}

/// One output's zoom region as drawn this frame: the on-screen
/// viewport (border included), the region of interest's center in
/// unmagnified output pixels, and the factor.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ZoomView {
    pub viewport: Rect,
    /// Border width in physical pixels (0 when full-screen).
    pub border: i32,
    pub center: (f64, f64),
    pub mag: f64,
}

/// Where the region of interest lands: `src` in unmagnified output
/// pixels (inside the frame), `dst` relative to the content box.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    pub src: (f64, f64, f64, f64),
    pub dst: Rect,
}

impl ZoomView {
    /// The content box: the viewport inside its border (GNOME's
    /// clipping `mainGroup`).
    pub fn content(&self) -> Rect {
        let (x, y, w, h) = self.viewport;
        let b = self.border;
        (x + b, y + b, (w - 2 * b).max(0), (h - 2 * b).max(0))
    }

    /// `getROI`: the unmagnified rectangle the region shows.
    pub fn roi(&self) -> (f64, f64, f64, f64) {
        let (rw, rh) = roi_size(self.viewport.2, self.viewport.3, self.mag);
        (self.center.0 - rw / 2.0, self.center.1 - rh / 2.0, rw, rh)
    }

    /// Screen position of unmagnified (0, 0): the content origin plus
    /// the rounded `_screenToViewPort(0, 0)` GNOME moves its clone to.
    fn origin(&self) -> (f64, f64) {
        let (cx, cy, _, _) = self.content();
        let (vw, vh) = (f64::from(self.viewport.2), f64::from(self.viewport.3));
        (
            f64::from(cx) + js_round(vw / 2.0 - self.center.0 * self.mag),
            f64::from(cy) + js_round(vh / 2.0 - self.center.1 * self.mag),
        )
    }

    /// An unmagnified point's place on screen.
    pub fn to_screen(&self, point: (f64, f64)) -> (f64, f64) {
        let (ox, oy) = self.origin();
        (ox + point.0 * self.mag, oy + point.1 * self.mag)
    }

    /// The unmagnified point shown at a screen position.
    pub fn to_source(&self, screen: (f64, f64)) -> (f64, f64) {
        let (ox, oy) = self.origin();
        ((screen.0 - ox) / self.mag, (screen.1 - oy) / self.mag)
    }

    /// `_getMousePosition`: the magnified sprite's hotspot on screen,
    /// rounded like GNOME's mouse actor.
    pub fn sprite(&self, pointer: (f64, f64)) -> (f64, f64) {
        let (cx, cy, _, _) = self.content();
        let (vw, vh) = (f64::from(self.viewport.2), f64::from(self.viewport.3));
        (
            f64::from(cx) + js_round(vw / 2.0 + (pointer.0 - self.center.0) * self.mag),
            f64::from(cy) + js_round(vh / 2.0 + (pointer.1 - self.center.1) * self.mag),
        )
    }

    /// `_isMouseOverRegion`: the pointer inside the viewport (the
    /// system pointer is hidden there).
    pub fn pointer_over(&self, pointer: (f64, f64)) -> bool {
        inside(self.viewport, pointer)
    }

    /// Whether a screen point shows magnified content.
    pub fn content_contains(&self, screen: (f64, f64)) -> bool {
        inside(self.content(), screen)
    }

    /// The part of an `output`-sized frame visible in the content box,
    /// and where it lands: pixel-exact destination edges, the source
    /// rectangle recomputed from them. Past-edge parts stay uncovered
    /// (the system background shows there). `None` when nothing of the
    /// frame is in view.
    pub fn sample(&self, output: (i32, i32)) -> Option<Sample> {
        let (cx, cy, cw, ch) = self.content();
        if cw <= 0 || ch <= 0 || self.mag.is_nan() || self.mag <= 0.0 {
            return None;
        }
        let (fw, fh) = (f64::from(output.0), f64::from(output.1));
        let (x0, y0) = self.to_source((f64::from(cx), f64::from(cy)));
        let (x1, y1) = self.to_source((f64::from(cx + cw), f64::from(cy + ch)));
        let (sx0, sy0) = (x0.max(0.0), y0.max(0.0));
        let (sx1, sy1) = (x1.min(fw), y1.min(fh));
        if sx1 <= sx0 || sy1 <= sy0 {
            return None;
        }
        let (dx0, dy0) = self.to_screen((sx0, sy0));
        let (dx1, dy1) = self.to_screen((sx1, sy1));
        let clamp_x = |v: f64| (v.round() as i32).clamp(cx, cx + cw);
        let clamp_y = |v: f64| (v.round() as i32).clamp(cy, cy + ch);
        let (dx0, dy0, dx1, dy1) = (clamp_x(dx0), clamp_y(dy0), clamp_x(dx1), clamp_y(dy1));
        if dx1 <= dx0 || dy1 <= dy0 {
            return None;
        }
        let (rx0, ry0) = self.to_source((f64::from(dx0), f64::from(dy0)));
        let (rx1, ry1) = self.to_source((f64::from(dx1), f64::from(dy1)));
        let (rx0, ry0) = (rx0.clamp(0.0, fw), ry0.clamp(0.0, fh));
        let (rx1, ry1) = (rx1.clamp(0.0, fw), ry1.clamp(0.0, fh));
        Some(Sample {
            src: (rx0, ry0, (rx1 - rx0).max(0.0), (ry1 - ry0).max(0.0)),
            dst: (dx0 - cx, dy0 - cy, dx1 - dx0, dy1 - dy0),
        })
    }

    /// The zoom region's border as four screen rectangles (none when
    /// full-screen).
    pub fn border_rects(&self) -> Vec<Rect> {
        let b = self.border;
        let (x, y, w, h) = self.viewport;
        if b <= 0 {
            return Vec::new();
        }
        [
            (x, y, w, b.min(h)),
            (x, y + h - b.min(h), w, b.min(h)),
            (x, y + b, b.min(w), (h - 2 * b).max(0)),
            (x + w - b.min(w), y + b, b.min(w), (h - 2 * b).max(0)),
        ]
        .into_iter()
        .filter(|r| r.2 > 0 && r.3 > 0)
        .collect()
    }
}

/// GNOME's `Crosshairs.reCenter`: four hairs around `center` (screen
/// pixels) of `thickness`, each `length` long, leaving a `clip`-sized
/// gap (zero when unclipped, which still leaves a thickness-sized
/// square open at the crossing).
pub fn cross_hairs(
    center: (f64, f64),
    thickness: f64,
    length: f64,
    clip: f64,
) -> [(f64, f64, f64, f64); 4] {
    let (cx, cy) = center;
    let t = thickness;
    [
        (cx - clip / 2.0 - length - t / 2.0, cy - t / 2.0, length, t),
        (cx + clip / 2.0 + t / 2.0, cy - t / 2.0, length, t),
        (cx - t / 2.0, cy - clip / 2.0 - length - t / 2.0, t, length),
        (cx - t / 2.0, cy + clip / 2.0 + t / 2.0, t, length),
    ]
}

/// Snap a fractional rectangle to pixels and intersect it with `bounds`.
fn snap_into(rect: (f64, f64, f64, f64), bounds: Rect) -> Option<Rect> {
    let x0 = (rect.0.round() as i64).max(i64::from(bounds.0));
    let y0 = (rect.1.round() as i64).max(i64::from(bounds.1));
    let x1 = ((rect.0 + rect.2).round() as i64).min(i64::from(bounds.0 + bounds.2));
    let y1 = ((rect.1 + rect.3).round() as i64).min(i64::from(bounds.1 + bounds.3));
    (x1 > x0 && y1 > y0).then_some((x0 as i32, y0 as i32, (x1 - x0) as i32, (y1 - y0) as i32))
}

/// Whether `point` lies in `rect`, half-open like GNOME's checks.
fn inside(rect: Rect, point: (f64, f64)) -> bool {
    let (x, y, w, h) = rect;
    (f64::from(x)..f64::from(x + w)).contains(&point.0)
        && (f64::from(y)..f64::from(y + h)).contains(&point.1)
}

/// `MagShaderEffects`: desaturation, per-channel brightness and
/// contrast, and lightness inversion, in the order Clutter applies the
/// chained offscreen effects (the last added runs first).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorEffects {
    /// `DesaturateEffect` factor: `1 - color-saturation` (enabled
    /// unless the saturation is 1).
    pub desaturation: f32,
    /// `BrightnessContrastEffect`, enabled when any channel changes.
    pub brightness_contrast: bool,
    pub brightness_multiplier: [f32; 3],
    pub brightness_offset: [f32; 3],
    /// `tan((contrast + 1) * π/4)` per channel, in double precision
    /// like Clutter; exactly 1 for an unchanged channel.
    pub contrast: [f32; 3],
    pub invert: bool,
    /// Every effect is disabled: the zoom region draws unfiltered.
    pub identity: bool,
}

impl ColorEffects {
    pub fn from_config(config: &MagnifierConfig) -> Self {
        // `get_brightness_values`.
        let brightness = |i: usize| {
            let b = config.brightness[i] as f32;
            if b < 0.0 {
                (1.0 + b, 0.0)
            } else {
                (1.0 - b, b)
            }
        };
        let contrast = |i: usize| {
            let c = config.contrast[i];
            if c == 0.0 {
                1.0
            } else {
                ((c + 1.0) * std::f64::consts::FRAC_PI_4).tan() as f32
            }
        };
        let brightness_contrast = config.brightness != [0.0; 3] || config.contrast != [0.0; 3];
        let desaturation = (1.0 - config.color_saturation) as f32;
        Self {
            desaturation,
            brightness_contrast,
            brightness_multiplier: std::array::from_fn(|i| brightness(i).0),
            brightness_offset: std::array::from_fn(|i| brightness(i).1),
            contrast: std::array::from_fn(contrast),
            invert: config.invert_lightness,
            identity: !config.invert_lightness && desaturation == 0.0 && !brightness_contrast,
        }
    }

    /// The effect chain on one opaque straight-RGB color, clamped
    /// between the effects as their 8-bit offscreen buffers do. The
    /// shader below is this function on the GPU.
    pub fn apply(&self, rgb: [f32; 3]) -> [f32; 3] {
        let clamp = |v: f32| v.clamp(0.0, 1.0);
        let mut c = rgb;
        if self.desaturation != 0.0 {
            let d = self.desaturation;
            let gray = 0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2];
            c = c.map(|v| clamp(v * (1.0 - d) + gray * d));
        }
        if self.brightness_contrast {
            c = std::array::from_fn(|i| {
                let v = clamp(c[i] * self.brightness_multiplier[i] + self.brightness_offset[i]);
                clamp((v - 0.5) * self.contrast[i] + 0.5)
            });
        }
        if self.invert {
            let max = c[0].max(c[1]).max(c[2]);
            let min = c[0].min(c[1]).min(c[2]);
            let lightness = (max + min) / 2.0;
            let delta = (1.0 - lightness) - lightness;
            c = c.map(|v| clamp(v + delta));
        }
        c
    }

    pub fn uniforms(&self) -> Vec<Uniform<'static>> {
        vec![
            Uniform::new("desaturation", self.desaturation),
            Uniform::new("brightness_multiplier", self.brightness_multiplier),
            Uniform::new("brightness_offset", self.brightness_offset),
            Uniform::new("contrast", self.contrast),
            Uniform::new("invert", if self.invert { 1.0f32 } else { 0.0 }),
        ]
    }
}

/// Everything one output's magnified frame draws, computed purely
/// from the view, the pointer and the settings.
#[derive(Debug, Clone, PartialEq)]
pub struct ZoomPlan {
    /// The output frame (pre-transform physical pixels).
    pub output: (i32, i32),
    pub view: ZoomView,
    pub full_screen: bool,
    pub content: Rect,
    pub sample: Option<Sample>,
    /// Cross-hairs relative to the content box.
    pub cross_hairs: Vec<Rect>,
    /// Premultiplied RGBA.
    pub cross_hairs_color: [f32; 4],
    pub border: Vec<Rect>,
    pub border_color: [f32; 3],
    /// The magnified sprite's hotspot, screen pixels.
    pub sprite: (f64, f64),
    /// The pointer is inside the viewport: the system pointer hides.
    pub pointer_over: bool,
    pub effects: ColorEffects,
    pub background: [f32; 3],
    /// Settings generation, so a settings change repaints.
    pub generation: u64,
}

impl ZoomPlan {
    /// `output` frame size, `scale` the output scale, `pointer` the
    /// pointer in output-local physical pixels, `accent` GNOME's
    /// accent color for the border.
    pub fn new(
        view: ZoomView,
        output: (i32, i32),
        scale: f64,
        pointer: (f64, f64),
        config: &MagnifierConfig,
        accent: [f32; 3],
        generation: u64,
    ) -> Self {
        let content = view.content();
        let sprite = view.sprite(pointer);
        let effects = ColorEffects::from_config(config);
        let cross_hairs = if config.show_cross_hairs {
            // Thickness and the clip are logical; GNOME divides the
            // length by the UI scale factor, so it stays device pixels
            // at integer scales.
            let thickness = f64::from(config.cross_hairs_thickness) * scale;
            let ui_scale = scale.ceil().max(1.0);
            let length = f64::from(config.cross_hairs_length) / ui_scale * scale;
            let clip = if config.cross_hairs_clip {
                CROSS_HAIRS_CLIP * scale
            } else {
                0.0
            };
            cross_hairs(sprite, thickness, length, clip)
                .into_iter()
                .filter_map(|hair| snap_into(hair, content))
                .map(|(x, y, w, h)| (x - content.0, y - content.1, w, h))
                .collect()
        } else {
            Vec::new()
        };
        let [r, g, b, a] = config.cross_hairs_rgba();
        let alpha = f32::from(a) / 255.0 * config.cross_hairs_opacity as f32;
        let channel = |v: u8| f32::from(v) / 255.0 * alpha;
        let bg = SYSTEM_BACKGROUND.map(|v| f32::from(v) / 255.0);
        Self {
            output,
            view,
            full_screen: view.viewport == (0, 0, output.0, output.1),
            content,
            sample: view.sample(output),
            cross_hairs,
            cross_hairs_color: [channel(r), channel(g), channel(b), alpha],
            border: view.border_rects(),
            border_color: accent,
            sprite,
            pointer_over: view.pointer_over(pointer),
            effects,
            background: bg,
            generation,
        }
    }

    /// Repaint signature bits: any change of what the zoom shows.
    pub fn signature_bits(&self) -> Vec<u32> {
        let (x, y, w, h) = self.view.viewport;
        let bits = |v: f64| {
            let b = v.to_bits();
            [(b >> 32) as u32, b as u32]
        };
        let mut out = vec![1, x as u32, y as u32, w as u32, h as u32];
        out.extend(bits(self.view.center.0));
        out.extend(bits(self.view.center.1));
        out.extend(bits(self.view.mag));
        out.extend(bits(self.sprite.0));
        out.extend(bits(self.sprite.1));
        out.push(u32::from(self.pointer_over));
        out.push(self.generation as u32);
        out.push((self.generation >> 32) as u32);
        out
    }
}

/// An eased change of the view: from the shown clone offset and factor
/// toward the region's target, `EASE_OUT_QUAD`.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Ease {
    /// `_screenToViewPort(0, 0)` before rounding, and the factor.
    from: (f64, f64, f64),
    start: Duration,
    duration: Duration,
}

/// One output's zoom region: GNOME's `ZoomRegion` state.
#[derive(Debug, Clone)]
pub struct Region {
    center: (f64, f64),
    mag: f64,
    viewport: Rect,
    active: bool,
    generation: u64,
    last_pointer: Option<(f64, f64)>,
    last_caret: Option<(f64, f64)>,
    last_focus: Option<(f64, f64)>,
    /// `_followingCursor` until this time (the rest timer).
    following_until: Option<Duration>,
    /// `_xDelayed`/`_yDelayed`.
    delayed: Option<(f64, f64)>,
    ease: Option<Ease>,
    /// Global logical offset and scale of the output, for input mapping.
    offset: (i32, i32),
    scale: f64,
    shown: Option<ZoomView>,
}

/// One frame's tracking points for one output, in its local physical
/// pixels, plus time and the motion policy.
#[derive(Debug, Clone, Copy)]
pub struct RegionInputs {
    pub screen: (f64, f64),
    pub pointer: (f64, f64),
    pub caret: Option<(f64, f64)>,
    pub focus: Option<(f64, f64)>,
    pub cursor: (f64, f64),
    pub now: Duration,
    pub motion: MotionPolicy,
    /// Physical border width for this output.
    pub border: i32,
}

impl Region {
    pub fn new(screen: (f64, f64), mag: f64) -> Self {
        Self {
            center: (screen.0 / 2.0, screen.1 / 2.0),
            mag,
            viewport: (0, 0, screen.0 as i32, screen.1 as i32),
            active: false,
            generation: 0,
            last_pointer: None,
            last_caret: None,
            last_focus: None,
            following_until: None,
            delayed: None,
            ease: None,
            offset: (0, 0),
            scale: 1.0,
            shown: None,
        }
    }

    fn following(&self, now: Duration) -> bool {
        self.following_until.is_some_and(|until| now < until)
    }

    /// The clone offset (`_screenToViewPort(0, 0)`, unrounded).
    fn clone_offset(&self, center: (f64, f64), mag: f64) -> (f64, f64) {
        let (vw, vh) = (f64::from(self.viewport.2), f64::from(self.viewport.3));
        (vw / 2.0 - center.0 * mag, vh / 2.0 - center.1 * mag)
    }

    /// What is on screen now: the target, or the eased value toward it.
    fn displayed(&self, now: Duration) -> ((f64, f64), f64) {
        let Some(ease) = self.ease else {
            return (self.center, self.mag);
        };
        let elapsed = now.saturating_sub(ease.start);
        if ease.duration.is_zero() || elapsed >= ease.duration {
            return (self.center, self.mag);
        }
        let t = elapsed.as_secs_f64() / ease.duration.as_secs_f64();
        let k = 1.0 - (1.0 - t) * (1.0 - t);
        let to = self.clone_offset(self.center, self.mag);
        let lerp = |a: f64, b: f64| a + (b - a) * k;
        let mag = lerp(ease.from.2, self.mag);
        let (ox, oy) = (lerp(ease.from.0, to.0), lerp(ease.from.1, to.1));
        let (vw, vh) = (f64::from(self.viewport.2), f64::from(self.viewport.3));
        (((vw / 2.0 - ox) / mag, (vh / 2.0 - oy) / mag), mag)
    }

    /// Start an animated change from what is shown now (`animate: true`).
    fn begin_ease(&mut self, inputs: &RegionInputs) {
        let ms = if inputs.motion.allows_motion() {
            inputs.motion.adjust_ms(SCROLL_EASE_MS)
        } else {
            0.0
        };
        if ms <= 0.0 {
            self.ease = None;
            return;
        }
        let (center, mag) = self.displayed(inputs.now);
        let (ox, oy) = self.clone_offset(center, mag);
        self.ease = Some(Ease {
            from: (ox, oy, mag),
            start: inputs.now,
            duration: Duration::from_secs_f64(ms / 1000.0),
        });
    }

    fn roi(&self) -> (f64, f64) {
        roi_size(self.viewport.2, self.viewport.3, self.mag)
    }

    fn view_size(&self) -> (f64, f64) {
        (f64::from(self.viewport.2), f64::from(self.viewport.3))
    }

    /// `scrollContentsTo`: an animated move, ignored off screen; it
    /// ends any wait for the pointer to rest.
    fn scroll_to(&mut self, point: (f64, f64), inputs: &RegionInputs) {
        let (w, h) = inputs.screen;
        if !(0.0..=w).contains(&point.0) || !(0.0..=h).contains(&point.1) {
            return;
        }
        self.following_until = None;
        self.delayed = None;
        self.begin_ease(inputs);
        self.center = point;
    }

    /// `_scrollContentsToDelayed`.
    fn scroll_delayed(&mut self, point: (f64, f64), inputs: &RegionInputs) {
        if self.following(inputs.now) {
            self.delayed = Some(point);
        } else {
            self.scroll_to(point, inputs);
        }
    }

    fn track(&self, mode: TrackingMode, point: (f64, f64), inputs: &RegionInputs) -> (f64, f64) {
        track_center(
            mode,
            point,
            self.center,
            self.roi(),
            inputs.screen,
            self.view_size(),
            self.mag,
            inputs.cursor,
        )
    }

    /// Advance the region one frame, in GNOME's event order: settings,
    /// pointer motion (`scrollToMousePos`), the rest timer, then caret
    /// and focus events. Returns the view to draw.
    pub fn step(
        &mut self,
        config: &MagnifierConfig,
        generation: u64,
        inputs: &RegionInputs,
    ) -> ZoomView {
        let screen = inputs.screen;
        let full = (0, 0, screen.0 as i32, screen.1 as i32);
        let base = place_viewport(position_request(screen, config.screen_position), screen);
        let lens = config.lens_mode && base != full;
        if !lens || (self.viewport.2, self.viewport.3) != (base.2, base.3) {
            self.viewport = base;
        }
        let activating = !self.active;
        self.active = true;
        let mut redo_mouse = false;
        if activating {
            // Settings applied while inactive take effect unanimated.
            self.mag = config.mag_factor;
            self.ease = None;
            self.generation = generation;
        } else if self.generation != generation {
            self.generation = generation;
            if config.mag_factor != self.mag {
                // `setMagFactor`: animated.
                self.begin_ease(inputs);
                self.mag = config.mag_factor;
            }
            // `_changeROI({redoCursorTracking: this._followingCursor})`.
            redo_mouse = self.following(inputs.now);
        }
        if self.last_pointer != Some(inputs.pointer) {
            self.last_pointer = Some(inputs.pointer);
            // `scrollToMousePos`: following, and the rest timer restarts.
            self.following_until = Some(inputs.now + POINTER_REST);
            redo_mouse = true;
        }
        if redo_mouse && config.mouse_tracking != TrackingMode::None {
            self.center = self.track(config.mouse_tracking, inputs.pointer, inputs);
            // Unanimated: an ease in flight jumps to the new value.
            self.ease = None;
        }
        if self
            .following_until
            .is_some_and(|until| inputs.now >= until)
        {
            self.following_until = None;
            if let Some(point) = self.delayed.take() {
                self.scroll_to(point, inputs);
            }
        }
        if self.last_caret != inputs.caret {
            self.last_caret = inputs.caret;
            if let Some(caret) = inputs
                .caret
                .filter(|_| config.caret_tracking != TrackingMode::None)
            {
                let target = self.track(config.caret_tracking, caret, inputs);
                self.scroll_delayed(target, inputs);
            }
        }
        if self.last_focus != inputs.focus {
            self.last_focus = inputs.focus;
            if let Some(focus) = inputs
                .focus
                .filter(|_| config.focus_tracking != TrackingMode::None)
            {
                let target = self.track(config.focus_tracking, focus, inputs);
                self.scroll_delayed(target, inputs);
            }
        }
        self.center = clamp_center(self.center, self.roi(), screen, config.scroll_at_edges);
        if lens {
            // The lens centers on the region of interest.
            let (vw, vh) = self.view_size();
            self.viewport = place_viewport(
                (self.center.0 - vw / 2.0, self.center.1 - vh / 2.0, vw, vh),
                screen,
            );
        }
        let (center, mag) = self.displayed(inputs.now);
        let border = if self.viewport == full {
            0
        } else {
            inputs.border
        };
        ZoomView {
            viewport: self.viewport,
            border,
            center,
            mag,
        }
    }

    /// Whether time alone changes this region: an ease in flight or a
    /// delayed move waiting for the pointer to rest.
    pub fn animating(&self, now: Duration) -> bool {
        self.ease
            .is_some_and(|ease| now.saturating_sub(ease.start) < ease.duration)
            || (self.delayed.is_some() && self.following_until.is_some())
    }
}

/// The newest of several sources for one tracking point: whichever
/// changed last wins, the first listed on ties.
#[derive(Debug, Clone, Default)]
pub struct LatestPoint {
    sources: Vec<Option<(f64, f64)>>,
    value: Option<(f64, f64)>,
}

impl LatestPoint {
    pub fn update(&mut self, sources: &[Option<(f64, f64)>]) -> Option<(f64, f64)> {
        self.sources.resize(sources.len(), None);
        let mut changed = None;
        for (old, new) in self.sources.iter().zip(sources) {
            if old != new && new.is_some() && changed.is_none() {
                changed = *new;
            }
        }
        if changed.is_some() {
            self.value = changed;
        }
        self.sources.copy_from_slice(sources);
        self.value
    }
}

/// One output's place in the global layout for [`MagnifierState::view_for`].
#[derive(Debug, Clone, Copy)]
pub struct OutputFrame<'a> {
    pub name: &'a str,
    /// Physical frame size (pre-transform).
    pub size: (i32, i32),
    pub scale: f64,
    /// Global logical top-left.
    pub offset: (i32, i32),
}

/// Live tracking inputs in global logical pixels: the pointer, the
/// caret from AT-SPI and from the text-input rectangle, the focus from
/// AT-SPI and the focused window's center, the pointer sprite's
/// logical size, the animation clock and the motion policy.
#[derive(Debug, Clone, Copy)]
pub struct TrackingInputs {
    pub pointer: (f64, f64),
    pub caret: [Option<(f64, f64)>; 2],
    pub focus: [Option<(f64, f64)>; 2],
    pub cursor: (f64, f64),
    pub now: Duration,
    pub motion: MotionPolicy,
}

/// The settings file cache plus each output's zoom region.
#[derive(Default)]
pub struct MagnifierState {
    config: MagnifierConfig,
    loaded: Option<SystemTime>,
    generation: u64,
    regions: HashMap<String, Region>,
    caret: LatestPoint,
    focus: LatestPoint,
}

impl MagnifierState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reload the config file when it changed. Cheap: one metadata
    /// stat when the file is untouched. A deleted file falls back to
    /// defaults (magnifier off).
    pub fn poll(&mut self) {
        let now = tuna_magnifier::mtime();
        if now != self.loaded {
            self.loaded = now;
            self.apply(tuna_magnifier::load());
        }
    }

    fn apply(&mut self, config: MagnifierConfig) {
        if config != self.config {
            self.config = config;
            self.generation += 1;
        }
    }

    /// Test hook: drive the state without touching the config file.
    #[cfg(test)]
    pub fn set_config(&mut self, config: MagnifierConfig) {
        self.apply(config.sanitized());
        self.loaded = tuna_magnifier::mtime();
    }

    pub fn config(&self) -> &MagnifierConfig {
        &self.config
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Whether AT-SPI focus and caret events are wanted (GNOME
    /// registers its listeners only while active with the mode on).
    pub fn wants_focus_events(&self) -> bool {
        self.config.active() && self.config.focus_tracking != TrackingMode::None
    }

    pub fn wants_caret_events(&self) -> bool {
        self.config.active() && self.config.caret_tracking != TrackingMode::None
    }

    /// The zoom view for one output this frame, or `None` when the
    /// magnifier is off or the output has no size.
    pub fn view_for(
        &mut self,
        output: OutputFrame<'_>,
        inputs: TrackingInputs,
    ) -> Option<ZoomView> {
        self.poll();
        let caret = self.caret.update(&inputs.caret);
        let focus = self.focus.update(&inputs.focus);
        let (w, h) = output.size;
        if !self.config.active() || w <= 0 || h <= 0 || output.scale.is_nan() || output.scale <= 0.0
        {
            if let Some(region) = self.regions.get_mut(output.name) {
                region.active = false;
                region.shown = None;
            }
            return None;
        }
        let scale = output.scale;
        let (ox, oy) = (f64::from(output.offset.0), f64::from(output.offset.1));
        let to_local = |(gx, gy): (f64, f64)| ((gx - ox) * scale, (gy - oy) * scale);
        let screen = (f64::from(w), f64::from(h));
        let region_inputs = RegionInputs {
            screen,
            pointer: to_local(inputs.pointer),
            caret: caret.map(to_local),
            focus: focus.map(to_local),
            cursor: (inputs.cursor.0 * scale, inputs.cursor.1 * scale),
            now: inputs.now,
            motion: inputs.motion,
            border: (BORDER_WIDTH * scale).round() as i32,
        };
        let mag = self.config.mag_factor;
        let region = self
            .regions
            .entry(output.name.to_owned())
            .or_insert_with(|| Region::new(screen, mag));
        region.offset = output.offset;
        region.scale = scale;
        let view = region.step(&self.config, self.generation, &region_inputs);
        region.shown = Some(view);
        Some(view)
    }

    /// [`view_for`](Self::view_for) plus the draw plan.
    pub fn plan_for(
        &mut self,
        output: OutputFrame<'_>,
        inputs: TrackingInputs,
        accent: [f32; 3],
    ) -> Option<ZoomPlan> {
        let view = self.view_for(output, inputs)?;
        let pointer = (
            (inputs.pointer.0 - f64::from(output.offset.0)) * output.scale,
            (inputs.pointer.1 - f64::from(output.offset.1)) * output.scale,
        );
        Some(ZoomPlan::new(
            view,
            output.size,
            output.scale,
            pointer,
            &self.config,
            accent,
            self.generation,
        ))
    }

    /// Whether time alone changes a zoom region.
    pub fn animating(&self, now: Duration) -> bool {
        self.config.active() && self.regions.values().any(|r| r.active && r.animating(now))
    }

    /// The unmagnified point under an absolute screen position (global
    /// logical), for touch: a point inside a zoom region's content maps
    /// back through it; elsewhere it is already unmagnified.
    pub fn unzoom(&self, global: (f64, f64)) -> (f64, f64) {
        for region in self.regions.values().filter(|r| r.active) {
            let Some(view) = region.shown else {
                continue;
            };
            let (ox, oy) = (f64::from(region.offset.0), f64::from(region.offset.1));
            let local = (
                (global.0 - ox) * region.scale,
                (global.1 - oy) * region.scale,
            );
            if view.content_contains(local) {
                let (sx, sy) = view.to_source(local);
                return (ox + sx / region.scale, oy + sy / region.scale);
            }
        }
        global
    }

    /// Record a pointer position the view must not chase: a touch
    /// point already mapped through the zoom moves the pointer under
    /// the finger, and tracking it would slide the content away.
    pub fn hold_pointer(&mut self, global: (f64, f64)) {
        for region in self.regions.values_mut() {
            let (ox, oy) = (f64::from(region.offset.0), f64::from(region.offset.1));
            region.last_pointer = Some((
                (global.0 - ox) * region.scale,
                (global.1 - oy) * region.scale,
            ));
        }
    }

    /// The state file's record for proofs.
    pub fn diagnostics(&self) -> serde_json::Value {
        let regions: serde_json::Map<String, serde_json::Value> = self
            .regions
            .iter()
            .filter(|(_, r)| r.active)
            .filter_map(|(name, r)| {
                let view = r.shown?;
                let roi = view.roi();
                let c = view.content();
                Some((
                    name.clone(),
                    serde_json::json!({
                        "viewport": [view.viewport.0, view.viewport.1, view.viewport.2, view.viewport.3],
                        "content": [c.0, c.1, c.2, c.3],
                        "center": [view.center.0, view.center.1],
                        "mag": view.mag,
                        "roi": [roi.0, roi.1, roi.2, roi.3],
                        "following": r.following_until.is_some(),
                        "delayed": r.delayed.is_some(),
                    }),
                ))
            })
            .collect();
        serde_json::json!({
            "active": self.config.active(),
            "mag_factor": self.config.mag_factor,
            "generation": self.generation,
            "regions": regions,
        })
    }
}

/// The two offscreen textures one magnified output draws through,
/// kept across frames and recreated on a size change.
#[derive(Default)]
pub struct ZoomTargets {
    outputs: HashMap<String, [Option<GlesTexture>; 2]>,
}

fn sized(
    slot: &mut Option<GlesTexture>,
    renderer: &mut GlesRenderer,
    size: (i32, i32),
) -> Result<GlesTexture, GlesError> {
    let want: Size<i32, BufferCoords> = (size.0.max(1), size.1.max(1)).into();
    if let Some(texture) = slot.as_ref().filter(|t| t.size() == want) {
        return Ok(texture.clone());
    }
    let texture: GlesTexture = renderer.create_buffer(Fourcc::Abgr8888, want)?;
    *slot = Some(texture.clone());
    Ok(texture)
}

impl ZoomTargets {
    /// The scene (output-sized) and zoom (content-sized) textures.
    pub fn textures(
        &mut self,
        renderer: &mut GlesRenderer,
        output: &str,
        plan: &ZoomPlan,
    ) -> Result<(GlesTexture, GlesTexture), GlesError> {
        let [scene, zoom] = self.outputs.entry(output.to_owned()).or_default();
        let scene = sized(scene, renderer, plan.output)?;
        let zoom = sized(zoom, renderer, (plan.content.2, plan.content.3))?;
        Ok((scene, zoom))
    }

    /// Drop every texture (the magnifier went off): no frame content
    /// outlives its use.
    pub fn clear(&mut self) {
        self.outputs.clear();
    }
}

fn rect(r: Rect) -> Rectangle<i32, Physical> {
    Rectangle::new((r.0, r.1).into(), (r.2, r.3).into())
}

fn whole(r: Rect) -> [Rectangle<i32, Physical>; 1] {
    [Rectangle::from_size((r.2, r.3).into())]
}

/// Pass 2: compose the zoom region into `zoom` from the unmagnified
/// `scene` (GNOME's `mainGroup`: background, the clone, cross-hairs
/// under the magnified sprite). `sprite` is the pointer drawn with its
/// hotspot at the plan's sprite position relative to the content box,
/// already at `scale × factor`.
pub fn compose_zoom(
    renderer: &mut GlesRenderer,
    scene: &GlesTexture,
    zoom: &mut GlesTexture,
    plan: &ZoomPlan,
    sprite: Option<(&crate::cursor::CursorElements, f64)>,
) -> Result<(), GlesError> {
    let size: Size<i32, Physical> = (plan.content.2.max(1), plan.content.3.max(1)).into();
    let all = Rectangle::from_size(size);
    let mut target = renderer.bind(zoom)?;
    let mut frame = renderer.render(&mut target, size, Transform::Normal)?;
    let [r, g, b] = plan.background;
    frame.clear(Color32F::new(r, g, b, 1.0), &[all])?;
    if let Some(sample) = plan.sample {
        let src = Rectangle::<f64, BufferCoords>::new(
            (sample.src.0, sample.src.1).into(),
            (sample.src.2, sample.src.3).into(),
        );
        frame.render_texture_from_to(
            scene,
            src,
            rect(sample.dst),
            &whole(sample.dst),
            &[],
            Transform::Normal,
            1.0,
            None,
            &[],
        )?;
    }
    let [cr, cg, cb, ca] = plan.cross_hairs_color;
    if ca > 0.0 {
        for hair in &plan.cross_hairs {
            frame.draw_solid(rect(*hair), &whole(*hair), Color32F::new(cr, cg, cb, ca))?;
        }
    }
    if let Some((elements, scale)) = sprite {
        crate::cursor::draw(&mut frame, elements, scale, &[all])?;
    }
    let _ = frame.finish()?;
    Ok(())
}

/// Pass 3 into the output's own frame: the unmagnified scene where
/// the region leaves the screen uncovered, the zoom region through the
/// color effects, its border, and the system pointer outside it.
pub fn present(
    frame: &mut GlesFrame<'_, '_>,
    scene: &GlesTexture,
    zoom: &GlesTexture,
    plan: &ZoomPlan,
    program: Option<&GlesTexProgram>,
    system_pointer: Option<(&crate::cursor::CursorElements, f64)>,
) -> Result<(), GlesError> {
    let output = (0, 0, plan.output.0, plan.output.1);
    if !plan.full_screen {
        let src = Rectangle::<f64, BufferCoords>::from_size(
            (f64::from(plan.output.0), f64::from(plan.output.1)).into(),
        );
        frame.render_texture_from_to(
            scene,
            src,
            rect(output),
            &whole(output),
            &[],
            Transform::Normal,
            1.0,
            None,
            &[],
        )?;
    }
    let (_, _, cw, ch) = plan.content;
    if cw > 0 && ch > 0 {
        let src = Rectangle::<f64, BufferCoords>::from_size((f64::from(cw), f64::from(ch)).into());
        let identity = plan.effects.identity;
        let uniforms = if identity {
            Vec::new()
        } else {
            plan.effects.uniforms()
        };
        let program = if identity { None } else { program };
        frame.render_texture_from_to(
            zoom,
            src,
            rect(plan.content),
            &whole(plan.content),
            &[rect((0, 0, cw, ch))],
            Transform::Normal,
            1.0,
            program,
            &uniforms,
        )?;
    }
    let [r, g, b] = plan.border_color;
    for edge in &plan.border {
        frame.draw_solid(rect(*edge), &whole(*edge), Color32F::new(r, g, b, 1.0))?;
    }
    if let Some((elements, scale)) = system_pointer.filter(|_| !plan.pointer_over) {
        crate::cursor::draw_sprite(frame, elements, scale, &[rect(output)])?;
    }
    Ok(())
}

/// The effect program, compiled once per renderer and cached; a
/// compile failure leaves the zoom region unfiltered.
pub fn effects_program(renderer: &mut GlesRenderer) -> Option<GlesTexProgram> {
    struct Cached(Option<GlesTexProgram>);
    if let Some(cached) = renderer.egl_context().user_data().get::<Cached>() {
        return cached.0.clone();
    }
    let result = renderer
        .compile_custom_texture_shader(
            SHADER,
            &[
                UniformName::new("desaturation", UniformType::_1f),
                UniformName::new("brightness_multiplier", UniformType::_3f),
                UniformName::new("brightness_offset", UniformType::_3f),
                UniformName::new("contrast", UniformType::_3f),
                UniformName::new("invert", UniformType::_1f),
            ],
        )
        .map_err(|error| eprintln!("tuna-compositor: magnifier color shader unavailable: {error}"))
        .ok();
    renderer
        .egl_context()
        .user_data()
        .insert_if_missing(|| Cached(result.clone()));
    result
}

/// [`ColorEffects::apply`] on the GPU, for the opaque zoom texture.
const SHADER: &str = r#"#version 100
//_DEFINES_
#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif
precision mediump float;
#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif
uniform float alpha;
uniform float desaturation;
uniform vec3 brightness_multiplier;
uniform vec3 brightness_offset;
uniform vec3 contrast;
uniform float invert;
varying vec2 v_coords;
#if defined(DEBUG_FLAGS)
uniform float tint;
#endif
void main() {
    vec4 color = texture2D(tex, v_coords);
    vec3 rgb = color.rgb;
    // Clutter.DesaturateEffect (added last, so applied first).
    float gray = dot(vec3(0.299, 0.587, 0.114), rgb);
    rgb = clamp(mix(rgb, vec3(gray), desaturation), 0.0, 1.0);
    // Clutter.BrightnessContrastEffect.
    rgb = clamp(rgb * brightness_multiplier + brightness_offset, 0.0, 1.0);
    rgb = clamp((rgb - 0.5) * contrast + 0.5, 0.0, 1.0);
    // Shell.InvertLightnessEffect (added first, so applied last).
    if (invert > 0.5) {
        float maxColor = max(rgb.r, max(rgb.g, rgb.b));
        float minColor = min(rgb.r, min(rgb.g, rgb.b));
        float lightness = (maxColor + minColor) / 2.0;
        float delta = (1.0 - lightness) - lightness;
        rgb = clamp(rgb + delta, 0.0, 1.0);
    }
    color = vec4(rgb, 1.0) * alpha;
#if defined(DEBUG_FLAGS)
    if (tint == 1.0) color = vec4(0.0,0.2,0.0,0.2) + color * 0.8;
#endif
    gl_FragColor = color;
}"#;

#[cfg(test)]
mod tests {
    use super::*;
    use tuna_shell_control::MotionLevel;

    fn enabled() -> MagnifierConfig {
        MagnifierConfig {
            enabled: true,
            ..MagnifierConfig::default()
        }
    }

    fn at(x: f64, y: f64, now_ms: u64) -> TrackingInputs {
        TrackingInputs {
            pointer: (x, y),
            caret: [None, None],
            focus: [None, None],
            cursor: (0.0, 0.0),
            now: Duration::from_millis(now_ms),
            motion: MotionPolicy::new(MotionLevel::Off, 1.0),
        }
    }

    fn output(name: &str, w: i32, h: i32) -> OutputFrame<'_> {
        OutputFrame {
            name,
            size: (w, h),
            scale: 1.0,
            offset: (0, 0),
        }
    }

    fn roi_origin(view: &ZoomView) -> (f64, f64) {
        let roi = view.roi();
        (roi.0, roi.1)
    }

    #[test]
    fn viewport_halves_split_the_output_with_gnome_rounding() {
        let place = |w: i32, h: i32, position| {
            let screen = (f64::from(w), f64::from(h));
            place_viewport(position_request(screen, position), screen)
        };
        assert_eq!(
            place(800, 600, ScreenPosition::FullScreen),
            (0, 0, 800, 600)
        );
        assert_eq!(place(800, 600, ScreenPosition::TopHalf), (0, 0, 800, 300));
        assert_eq!(
            place(800, 600, ScreenPosition::BottomHalf),
            (0, 300, 800, 300)
        );
        assert_eq!(place(800, 600, ScreenPosition::LeftHalf), (0, 0, 400, 600));
        assert_eq!(
            place(800, 600, ScreenPosition::RightHalf),
            (400, 0, 400, 600)
        );
        // Odd sizes: `Math.round` rounds the half up and `_setViewPort`
        // clamps the origin back on screen, so the halves still cover
        // the output without a gap.
        assert_eq!(place(801, 601, ScreenPosition::TopHalf), (0, 0, 801, 301));
        assert_eq!(
            place(801, 601, ScreenPosition::BottomHalf),
            (0, 300, 801, 301)
        );
        assert_eq!(
            place(801, 601, ScreenPosition::RightHalf),
            (400, 0, 401, 601)
        );
        // Oversized and negative requests clamp on screen.
        assert_eq!(
            place_viewport((-50.0, 700.0, 400.0, 300.0), (800.0, 600.0)),
            (0, 300, 400, 300)
        );
    }

    #[test]
    fn lens_mode_follows_the_view_center_except_full_screen() {
        let mut config = enabled();
        config.lens_mode = true;
        config.mouse_tracking = TrackingMode::Centered;
        // Full-screen lens is pointless: the viewport stays put.
        let mut state = MagnifierState::new();
        state.set_config(config.clone());
        let view = state
            .view_for(output("tuna-0", 800, 600), at(700.0, 500.0, 0))
            .unwrap();
        assert_eq!(view.viewport, (0, 0, 800, 600));
        assert_eq!(view.border, 0);
        // A left-half lens centers on the (clamped) region center: ROI
        // 200x300 around (700,500) clamps to (700,450), so the 400x600
        // lens sits at x 500 clamped to 400, full height.
        config.screen_position = ScreenPosition::LeftHalf;
        let mut state = MagnifierState::new();
        state.set_config(config.clone());
        let view = state
            .view_for(output("tuna-0", 800, 600), at(700.0, 500.0, 0))
            .unwrap();
        assert_eq!(view.viewport, (400, 0, 400, 600));
        assert_eq!(view.border, 2);
        // A top-half lens near the top-left corner hugs it.
        config.screen_position = ScreenPosition::TopHalf;
        let mut state = MagnifierState::new();
        state.set_config(config);
        let view = state
            .view_for(output("tuna-0", 800, 600), at(50.0, 50.0, 0))
            .unwrap();
        assert_eq!(view.viewport, (0, 0, 800, 300));
    }

    #[test]
    fn centered_tracking_puts_the_point_at_the_middle() {
        let track = |mode| {
            track_center(
                mode,
                (100.0, 200.0),
                (400.0, 300.0),
                (400.0, 300.0),
                (800.0, 600.0),
                (800.0, 600.0),
                2.0,
                (0.0, 0.0),
            )
        };
        assert_eq!(track(TrackingMode::Centered), (100.0, 200.0));
        // None holds the view still.
        assert_eq!(track(TrackingMode::None), (400.0, 300.0));
    }

    #[test]
    fn proportional_tracking_mirrors_gnome_math() {
        // Hand-computed from magnifier.js _centerFromPointProportional:
        // screen 800x600, view 800x600, mag 2: ROI 400x300, padding
        // min(800,600)/5/2 = 60. Point (600,150): proportions
        // (0.5,-0.5), so x = 600-0.5*(200-60) = 530,
        // y = 150+0.5*(150-60) = 195.
        let center = track_center(
            TrackingMode::Proportional,
            (600.0, 150.0),
            (400.0, 300.0),
            (400.0, 300.0),
            (800.0, 600.0),
            (800.0, 600.0),
            2.0,
            (0.0, 0.0),
        );
        assert_eq!(center, (530.0, 195.0));
        // Screen middle stays centered.
        let middle = track_center(
            TrackingMode::Proportional,
            (400.0, 300.0),
            (0.0, 0.0),
            (400.0, 300.0),
            (800.0, 600.0),
            (800.0, 600.0),
            2.0,
            (0.0, 0.0),
        );
        assert_eq!(middle, (400.0, 300.0));
    }

    #[test]
    fn push_tracking_scrolls_only_past_the_boundary() {
        // ROI 400x300 centered on (400,300): spans x 200..600.
        let push = |point, cursor| {
            track_center(
                TrackingMode::Push,
                point,
                (400.0, 300.0),
                (400.0, 300.0),
                (800.0, 600.0),
                (800.0, 600.0),
                2.0,
                cursor,
            )
        };
        assert_eq!(push((500.0, 300.0), (0.0, 0.0)), (400.0, 300.0));
        // Past the right edge by 50: shift right by 50; above the top
        // by 50: shift up by 50.
        assert_eq!(push((650.0, 100.0), (0.0, 0.0)), (450.0, 250.0));
        // The sprite must fit too: a 24x24 pointer at x 590 is 14 past
        // the 576 limit (`xRoiRight = xRoi + widthRoi - cursorWidth`).
        assert_eq!(push((590.0, 440.0), (24.0, 24.0)), (414.0, 314.0));
        // The near edges ignore the sprite size.
        assert_eq!(push((190.0, 300.0), (24.0, 24.0)), (390.0, 300.0));
    }

    #[test]
    fn edge_clamp_holds_the_roi_on_screen_unless_scrolling() {
        // ROI 400x300 on 800x600 clamps to x 200..600, y 150..450.
        assert_eq!(
            clamp_center((10.0, 590.0), (400.0, 300.0), (800.0, 600.0), false),
            (200.0, 450.0)
        );
        assert_eq!(
            clamp_center((400.0, 300.0), (400.0, 300.0), (800.0, 600.0), false),
            (400.0, 300.0)
        );
        // Scrolling at edges passes the center through untouched.
        assert_eq!(
            clamp_center((10.0, 590.0), (400.0, 300.0), (800.0, 600.0), true),
            (10.0, 590.0)
        );
        // A region larger than the screen (factor below 1) hugs the
        // top-left, as `Math.max(Math.min(c, screen - roi/2), roi/2)`.
        assert_eq!(
            clamp_center((400.0, 300.0), (1600.0, 1200.0), (800.0, 600.0), false),
            (800.0, 600.0)
        );
    }

    #[test]
    fn disabled_magnifier_or_empty_output_has_no_view_but_unity_zooms() {
        let mut state = MagnifierState::new();
        state.set_config(MagnifierConfig::default());
        assert!(state
            .view_for(output("tuna-0", 800, 600), at(10.0, 10.0, 0))
            .is_none());
        // GNOME keeps the zoom region at factor 1 (effects still apply).
        let mut unity = enabled();
        unity.mag_factor = 1.0;
        state.set_config(unity);
        let view = state
            .view_for(output("tuna-0", 800, 600), at(10.0, 10.0, 0))
            .unwrap();
        assert_eq!(view.roi(), (0.0, 0.0, 800.0, 600.0));
        assert!(state
            .view_for(output("tuna-0", 0, 600), at(10.0, 10.0, 0))
            .is_none());
    }

    #[test]
    fn first_frame_settles_on_the_pointer() {
        let mut state = MagnifierState::new();
        let mut config = enabled();
        config.mouse_tracking = TrackingMode::Centered;
        state.set_config(config);
        let view = state
            .view_for(output("tuna-0", 800, 600), at(100.0, 100.0, 0))
            .unwrap();
        assert_eq!(view.viewport, (0, 0, 800, 600));
        // ROI 400x300 centered on the pointer, clamped on screen.
        assert_eq!(view.roi(), (0.0, 0.0, 400.0, 300.0));
    }

    #[test]
    fn caret_wins_when_the_pointer_rests() {
        let mut state = MagnifierState::new();
        let mut config = enabled();
        config.mouse_tracking = TrackingMode::None;
        config.caret_tracking = TrackingMode::Centered;
        config.focus_tracking = TrackingMode::None;
        state.set_config(config);
        // Pointer motion with mouse tracking off: view holds the middle.
        let view = state
            .view_for(output("tuna-0", 800, 600), at(700.0, 500.0, 0))
            .unwrap();
        assert_eq!(roi_origin(&view), (200.0, 150.0));
        // A caret move while the pointer still counts as moving waits
        // (`_followingCursor`)...
        let caret = |now_ms| TrackingInputs {
            caret: [Some((100.0, 100.0)), None],
            ..at(700.0, 500.0, now_ms)
        };
        let view = state
            .view_for(output("tuna-0", 800, 600), caret(500))
            .unwrap();
        assert_eq!(roi_origin(&view), (200.0, 150.0));
        // ...and once the pointer has rested a second, the view goes
        // to the caret.
        let view = state
            .view_for(output("tuna-0", 800, 600), caret(1000))
            .unwrap();
        assert_eq!(roi_origin(&view), (0.0, 0.0));
        assert_eq!((view.roi().2, view.roi().3), (400.0, 300.0));
        // A caret event after a long rest applies at once.
        let view = state
            .view_for(
                output("tuna-0", 800, 600),
                TrackingInputs {
                    caret: [Some((700.0, 500.0)), None],
                    ..at(700.0, 500.0, 5000)
                },
            )
            .unwrap();
        assert_eq!(roi_origin(&view), (400.0, 300.0));
    }

    #[test]
    fn focus_tracking_follows_the_focused_window() {
        let mut state = MagnifierState::new();
        let mut config = enabled();
        config.mouse_tracking = TrackingMode::None;
        config.caret_tracking = TrackingMode::None;
        config.focus_tracking = TrackingMode::Centered;
        state.set_config(config);
        // No focus yet: the view holds the middle.
        let view = state
            .view_for(output("tuna-0", 800, 600), at(0.0, 0.0, 0))
            .unwrap();
        assert_eq!(roi_origin(&view), (200.0, 150.0));
        // A focus event after the pointer rests retargets to the
        // focused window's center.
        let view = state
            .view_for(
                output("tuna-0", 800, 600),
                TrackingInputs {
                    focus: [None, Some((600.0, 450.0))],
                    ..at(0.0, 0.0, 2000)
                },
            )
            .unwrap();
        assert_eq!(roi_origin(&view), (400.0, 300.0));
        // An AT-SPI widget focus that arrives later wins over the
        // unchanged window center: (300, 250) less half the 400x300 ROI.
        let view = state
            .view_for(
                output("tuna-0", 800, 600),
                TrackingInputs {
                    focus: [Some((300.0, 250.0)), Some((600.0, 450.0))],
                    ..at(0.0, 0.0, 2100)
                },
            )
            .unwrap();
        assert_eq!(roi_origin(&view), (100.0, 100.0));
    }

    #[test]
    fn caret_off_screen_or_untracked_is_ignored() {
        let mut state = MagnifierState::new();
        let mut config = enabled();
        config.mouse_tracking = TrackingMode::None;
        config.caret_tracking = TrackingMode::Centered;
        state.set_config(config);
        state
            .view_for(output("tuna-0", 800, 600), at(0.0, 0.0, 0))
            .unwrap();
        // `scrollContentsTo` ignores a point off this screen.
        let view = state
            .view_for(
                output("tuna-0", 800, 600),
                TrackingInputs {
                    caret: [Some((900.0, 100.0)), None],
                    ..at(0.0, 0.0, 2000)
                },
            )
            .unwrap();
        assert_eq!(roi_origin(&view), (200.0, 150.0));
    }

    #[test]
    fn outputs_track_independently_in_global_space() {
        let mut state = MagnifierState::new();
        let mut config = enabled();
        config.mouse_tracking = TrackingMode::Centered;
        state.set_config(config);
        // Two 800x600 outputs side by side at scale 1; the pointer
        // sits on the right one.
        let left = state
            .view_for(output("eDP-1", 800, 600), at(1000.0, 300.0, 0))
            .unwrap();
        let right = state
            .view_for(
                OutputFrame {
                    offset: (800, 0),
                    ..output("HDMI-1", 800, 600)
                },
                at(1000.0, 300.0, 0),
            )
            .unwrap();
        // Right output: pointer is local (200,300), ROI clamped.
        assert_eq!(roi_origin(&right), (0.0, 150.0));
        // Left output: pointer is off-output to the right; the ROI
        // clamps to the nearest edge instead of leaving the screen.
        assert_eq!(roi_origin(&left), (400.0, 150.0));
    }

    #[test]
    fn fractional_scale_converts_logical_tracking_points() {
        let mut state = MagnifierState::new();
        let mut config = enabled();
        config.mouse_tracking = TrackingMode::Centered;
        state.set_config(config);
        // 150% output: logical (100,100) is physical (150,150).
        let view = state
            .view_for(
                OutputFrame {
                    scale: 1.5,
                    ..output("tuna-0", 1200, 900)
                },
                at(100.0, 100.0, 0),
            )
            .unwrap();
        assert_eq!(view.roi(), (0.0, 0.0, 600.0, 450.0));
    }

    #[test]
    fn points_map_both_ways_through_the_zoom() {
        let view = ZoomView {
            viewport: (0, 0, 1280, 800),
            border: 0,
            center: (640.0, 200.0),
            mag: 2.0,
        };
        // `_screenToViewPort`: 640 + (x - 640) * 2, 400 + (y - 200) * 2.
        assert_eq!(view.to_screen((640.0, 200.0)), (640.0, 400.0));
        assert_eq!(view.to_screen((470.0, 24.0)), (300.0, 48.0));
        assert_eq!(view.to_source((300.0, 48.0)), (470.0, 24.0));
        for point in [(0.0, 0.0), (123.25, 77.5), (1279.0, 799.0)] {
            let back = view.to_source(view.to_screen(point));
            assert!((back.0 - point.0).abs() < 1e-9 && (back.1 - point.1).abs() < 1e-9);
        }
        // The sprite sits where the pointer maps (rounded).
        assert_eq!(view.sprite((700.3, 250.0)), (761.0, 500.0));
        // A clicked screen point and the pointer under the sprite agree:
        // the content shown under the sprite is the pointer's position.
        let pointer = (700.0, 250.0);
        assert_eq!(view.to_source(view.sprite(pointer)), pointer);
    }

    #[test]
    fn half_screen_content_is_inset_by_the_border_and_centered_in_the_viewport() {
        let view = ZoomView {
            viewport: (0, 0, 800, 300),
            border: 2,
            center: (400.0, 150.0),
            mag: 2.0,
        };
        assert_eq!(view.content(), (2, 2, 796, 296));
        // GNOME positions the clone against the whole viewport width,
        // from the content origin.
        assert_eq!(view.to_screen((400.0, 150.0)), (402.0, 152.0));
        assert_eq!(
            view.border_rects(),
            vec![
                (0, 0, 800, 2),
                (0, 298, 800, 2),
                (0, 2, 2, 296),
                (798, 2, 2, 296)
            ]
        );
        assert!(view.pointer_over((10.0, 10.0)));
        assert!(!view.pointer_over((10.0, 300.0)));
    }

    #[test]
    fn sampling_clips_past_edge_regions_to_the_frame() {
        // Scrolled past the left/top edge: ROI 400x300 centered on
        // (100, 50) spans -100..300 x -100..200; only 0..300 x 0..200
        // exists, landing at viewport 200..800 x 200..600.
        let view = ZoomView {
            viewport: (0, 0, 800, 600),
            border: 0,
            center: (100.0, 50.0),
            mag: 2.0,
        };
        let sample = view.sample((800, 600)).unwrap();
        assert_eq!(sample.dst, (200, 200, 600, 400));
        assert_eq!(sample.src, (0.0, 0.0, 300.0, 200.0));
        // Fully inside: the whole content box.
        let inside = ZoomView {
            center: (400.0, 300.0),
            ..view
        };
        let sample = inside.sample((800, 600)).unwrap();
        assert_eq!(sample.dst, (0, 0, 800, 600));
        assert_eq!(sample.src, (200.0, 150.0, 400.0, 300.0));
        // Nothing of the frame in view.
        let away = ZoomView {
            center: (-1000.0, 300.0),
            ..view
        };
        assert!(away.sample((800, 600)).is_none());
    }

    #[test]
    fn cross_hairs_frame_the_sprite_like_gnome_recenter() {
        // Thickness 8, length 100, unclipped: an 8x8 hole at the
        // crossing, each hair starting t/2 from the center.
        let hairs = cross_hairs((400.0, 300.0), 8.0, 100.0, 0.0);
        assert_eq!(hairs[0], (296.0, 296.0, 100.0, 8.0));
        assert_eq!(hairs[1], (404.0, 296.0, 100.0, 8.0));
        assert_eq!(hairs[2], (396.0, 196.0, 8.0, 100.0));
        assert_eq!(hairs[3], (396.0, 304.0, 8.0, 100.0));
        // Clipped: a 100x100 opening around the sprite.
        let clipped = cross_hairs((400.0, 300.0), 8.0, 100.0, 100.0);
        assert_eq!(clipped[1].0, 454.0);
        assert_eq!(clipped[3].1, 354.0);
        // The plan clips hairs to the content box and colors them with
        // the opacity premultiplied in.
        let config = MagnifierConfig {
            enabled: true,
            show_cross_hairs: true,
            cross_hairs_length: 4096,
            ..MagnifierConfig::default()
        };
        let view = ZoomView {
            viewport: (0, 0, 800, 600),
            border: 0,
            center: (400.0, 300.0),
            mag: 2.0,
        };
        let plan = ZoomPlan::new(view, (800, 600), 1.0, (400.0, 300.0), &config, [0.0; 3], 1);
        assert_eq!(
            plan.cross_hairs,
            vec![
                (0, 296, 396, 8),
                (404, 296, 396, 8),
                (396, 0, 8, 296),
                (396, 304, 8, 296)
            ]
        );
        let a = 0.66f32;
        assert_eq!(plan.cross_hairs_color, [a, 0.0, 0.0, a]);
        let hidden = ZoomPlan::new(
            view,
            (800, 600),
            1.0,
            (400.0, 300.0),
            &MagnifierConfig {
                show_cross_hairs: false,
                ..config
            },
            [0.0; 3],
            1,
        );
        assert!(hidden.cross_hairs.is_empty());
    }

    #[test]
    fn color_effects_match_the_clutter_and_shell_shaders() {
        let effects = |config: MagnifierConfig| ColorEffects::from_config(&config);
        let base = MagnifierConfig::default();
        assert!(effects(base.clone()).identity);
        assert_eq!(
            effects(base.clone()).apply([0.2, 0.4, 0.6]),
            [0.2, 0.4, 0.6]
        );
        // Invert lightness: L = (max+min)/2; every channel + (1 - 2L).
        let invert = effects(MagnifierConfig {
            invert_lightness: true,
            ..base.clone()
        });
        assert_eq!(invert.apply([1.0, 1.0, 1.0]), [0.0, 0.0, 0.0]);
        assert_eq!(invert.apply([0.0, 0.0, 0.0]), [1.0, 1.0, 1.0]);
        let red = invert.apply([1.0, 0.0, 0.0]);
        assert_eq!(red, [1.0, 0.0, 0.0], "pure hues keep lightness 0.5");
        let dark = invert.apply([0.2, 0.1, 0.0]);
        let expected = [1.0, 0.9, 0.8];
        assert!(
            dark.iter()
                .zip(expected)
                .all(|(got, want)| (got - want).abs() < 1e-6),
            "{dark:?}"
        );
        // Full desaturation: Rec. 601 luma.
        let gray = effects(MagnifierConfig {
            color_saturation: 0.0,
            ..base.clone()
        })
        .apply([1.0, 0.0, 0.0]);
        assert!(gray.iter().all(|v| (v - 0.299).abs() < 1e-6));
        // Brightness: -0.5 halves, +0.5 halves the distance to white.
        let bright = effects(MagnifierConfig {
            brightness: [-0.5, 0.5, 0.0],
            ..base.clone()
        });
        let out = bright.apply([0.8, 0.2, 0.3]);
        assert!((out[0] - 0.4).abs() < 1e-6);
        assert!((out[1] - 0.6).abs() < 1e-6);
        assert!((out[2] - 0.3).abs() < 1e-6);
        // Contrast: +1 is tan(π/2), a hard threshold at 0.5; -1 is
        // tan(0), flat mid-gray.
        let contrast = effects(MagnifierConfig {
            contrast: [1.0, -1.0, 0.5],
            ..base
        });
        let out = contrast.apply([0.6, 0.9, 0.5]);
        assert_eq!(out[0], 1.0);
        assert!((out[1] - 0.5).abs() < 1e-6);
        assert!((out[2] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn zoom_eases_to_caret_moves_and_factor_changes_when_animations_are_on() {
        let mut state = MagnifierState::new();
        let mut config = enabled();
        config.mouse_tracking = TrackingMode::None;
        config.caret_tracking = TrackingMode::Centered;
        state.set_config(config.clone());
        let full = MotionPolicy::new(MotionLevel::Full, 1.0);
        let inputs = |now_ms, caret| TrackingInputs {
            caret: [caret, None],
            motion: full,
            ..at(400.0, 300.0, now_ms)
        };
        state
            .view_for(output("tuna-0", 800, 600), inputs(0, None))
            .unwrap();
        // The caret move animates: halfway in time is 75% of the way.
        let start = state
            .view_for(
                output("tuna-0", 800, 600),
                inputs(2000, Some((600.0, 400.0))),
            )
            .unwrap();
        assert_eq!(start.center, (400.0, 300.0));
        assert!(state.animating(Duration::from_millis(2000)));
        let mid = state
            .view_for(
                output("tuna-0", 800, 600),
                inputs(2050, Some((600.0, 400.0))),
            )
            .unwrap();
        assert!((mid.center.0 - 550.0).abs() < 1e-9, "{mid:?}");
        let done = state
            .view_for(
                output("tuna-0", 800, 600),
                inputs(2100, Some((600.0, 400.0))),
            )
            .unwrap();
        assert_eq!(done.center, (600.0, 400.0));
        assert!(!state.animating(Duration::from_millis(2100)));
        // Zoom keys change the factor: eased too, around the same center.
        config.mag_factor = 3.0;
        state.set_config(config);
        let eased = state
            .view_for(
                output("tuna-0", 800, 600),
                inputs(3000, Some((600.0, 400.0))),
            )
            .unwrap();
        assert_eq!(eased.mag, 2.0);
        let done = state
            .view_for(
                output("tuna-0", 800, 600),
                inputs(3100, Some((600.0, 400.0))),
            )
            .unwrap();
        assert_eq!(done.mag, 3.0);
        // 800/3 x 600/3 around (600,400) stays on screen.
        assert_eq!(done.center, (600.0, 400.0));
    }

    #[test]
    fn touch_points_map_back_through_the_zoom_and_do_not_chase() {
        let mut state = MagnifierState::new();
        let mut config = enabled();
        config.mouse_tracking = TrackingMode::Proportional;
        config.screen_position = ScreenPosition::TopHalf;
        state.set_config(config);
        let view = state
            .view_for(output("tuna-0", 800, 600), at(400.0, 100.0, 0))
            .unwrap();
        // Inside the content box: the magnified point under the finger.
        let finger = (300.0, 100.0);
        let mapped = state.unzoom(finger);
        assert_eq!(view.to_screen(mapped), finger);
        // Outside the zoom region the screen is unmagnified.
        assert_eq!(state.unzoom((300.0, 500.0)), (300.0, 500.0));
        // The pointer moved under the finger does not retarget the view.
        state.hold_pointer(mapped);
        let after = state
            .view_for(output("tuna-0", 800, 600), at(mapped.0, mapped.1, 10))
            .unwrap();
        assert_eq!(after.center, view.center);
    }

    #[test]
    fn latest_point_takes_whichever_source_changed_last() {
        let mut latest = LatestPoint::default();
        assert_eq!(latest.update(&[None, Some((1.0, 1.0))]), Some((1.0, 1.0)));
        assert_eq!(
            latest.update(&[Some((2.0, 2.0)), Some((1.0, 1.0))]),
            Some((2.0, 2.0))
        );
        // Unchanged sources keep the last winner.
        assert_eq!(
            latest.update(&[Some((2.0, 2.0)), Some((1.0, 1.0))]),
            Some((2.0, 2.0))
        );
        assert_eq!(
            latest.update(&[Some((2.0, 2.0)), Some((3.0, 3.0))]),
            Some((3.0, 3.0))
        );
        // A source going away never clears the point.
        assert_eq!(latest.update(&[None, None]), Some((3.0, 3.0)));
    }

    /// Headless GL harness: surfaceless EGL, or `None` when this
    /// machine has no EGL (the test skips instead of failing).
    #[cfg(feature = "drm")]
    fn headless_renderer() -> Option<GlesRenderer> {
        use smithay::backend::egl::native::EGLSurfacelessDisplay;
        use smithay::backend::egl::{EGLContext, EGLDisplay};
        let display = unsafe { EGLDisplay::new(EGLSurfacelessDisplay) }.ok()?;
        let context = EGLContext::new(&display).ok()?;
        unsafe { GlesRenderer::new(context) }.ok()
    }

    /// The production passes end to end on the GPU: a 16x16 frame of
    /// four solid quadrants, zoomed 2x on its center through the color
    /// effects, read back and compared texel by texel with the pure
    /// mapping and [`ColorEffects::apply`].
    #[cfg(feature = "drm")]
    #[test]
    fn gpu_passes_render_the_zoomed_region_through_the_effects() {
        use smithay::backend::renderer::{ExportMem, ImportMem};
        let Some(mut renderer) = headless_renderer() else {
            eprintln!("magnifier GL test skipped: no surfaceless EGL");
            return;
        };
        let quadrant = |x: usize, y: usize| -> [u8; 4] {
            match (x < 8, y < 8) {
                (true, true) => [255, 0, 0, 255],
                (false, true) => [0, 255, 0, 255],
                (true, false) => [0, 0, 255, 255],
                (false, false) => [255, 255, 255, 255],
            }
        };
        let mut bytes = Vec::with_capacity(16 * 16 * 4);
        for y in 0..16 {
            for x in 0..16 {
                bytes.extend(quadrant(x, y));
            }
        }
        let scene = renderer
            .import_memory(&bytes, Fourcc::Abgr8888, (16, 16).into(), false)
            .expect("scene upload");
        let program = effects_program(&mut renderer).expect("magnifier shader compiles");
        for config in [
            MagnifierConfig {
                enabled: true,
                ..MagnifierConfig::default()
            },
            MagnifierConfig {
                enabled: true,
                invert_lightness: true,
                color_saturation: 0.5,
                brightness: [0.1, -0.2, 0.0],
                ..MagnifierConfig::default()
            },
        ] {
            let view = ZoomView {
                viewport: (0, 0, 16, 16),
                border: 0,
                center: (8.0, 8.0),
                mag: 2.0,
            };
            let plan = ZoomPlan::new(view, (16, 16), 1.0, (8.0, 8.0), &config, [0.0; 3], 1);
            let mut targets = ZoomTargets::default();
            let (_, mut zoom) = targets
                .textures(&mut renderer, "test", &plan)
                .expect("zoom texture");
            compose_zoom(&mut renderer, &scene, &mut zoom, &plan, None).expect("compose");
            let mut out: GlesTexture = renderer
                .create_buffer(Fourcc::Abgr8888, (16, 16).into())
                .expect("output texture");
            let mut target = renderer.bind(&mut out).expect("bind output");
            {
                let mut frame = renderer
                    .render(&mut target, (16, 16).into(), Transform::Normal)
                    .expect("frame");
                present(&mut frame, &scene, &zoom, &plan, Some(&program), None).expect("present");
                let _ = frame.finish().expect("finish");
            }
            let mapping = renderer
                .copy_framebuffer(
                    &target,
                    Rectangle::from_size((16, 16).into()),
                    Fourcc::Abgr8888,
                )
                .expect("read back");
            let pixels = renderer.map_texture(&mapping).expect("map").to_vec();
            let effects = ColorEffects::from_config(&config);
            // Sample texel centers away from the doubled quadrant
            // boundaries, where bilinear filtering blends.
            for (sx, sy) in [
                (1usize, 1usize),
                (14, 1),
                (1, 14),
                (14, 14),
                (5, 2),
                (11, 13),
            ] {
                let source = view.to_source((sx as f64 + 0.5, sy as f64 + 0.5));
                let texel = quadrant(source.0 as usize, source.1 as usize);
                let rgb = effects.apply([0, 1, 2].map(|i| f32::from(texel[i]) / 255.0));
                let at = (sy * 16 + sx) * 4;
                for c in 0..3 {
                    let got = f32::from(pixels[at + c]) / 255.0;
                    assert!(
                        (got - rgb[c]).abs() <= 2.0 / 255.0,
                        "pixel ({sx},{sy}) channel {c}: {got} vs {}",
                        rgb[c]
                    );
                }
            }
        }
    }
}
