//! Screen magnifier (#349): GNOME 51 zoom driven by Settings.
//!
//! The GTK shell mirrors `org.gnome.desktop.a11y.magnifier` into
//! `$XDG_RUNTIME_DIR/tuna-magnifier.json` (see the `tuna-magnifier`
//! crate); this module turns that file plus live pointer, focus and
//! caret positions into a magnified frame. All geometry here is
//! per-output physical pixels, so each monitor zooms independently
//! from the same tracking point.
//!
//! Behavior reference: gnome-shell 51.0 `js/ui/magnifier.js`
//! (`ZoomRegion`: ROI = viewport / mag factor; centered, proportional
//! and push tracking; edge clamping unless `scroll-at-edges`). The
//! magnifier never runs while locked: `render_pixels` refuses locked
//! captures, and the live path leaves the lock screen unmagnified, so
//! nothing behind the lock is ever enlarged or leaked.
//!
//! Proof path: `Runtime::render_pixels` (screenshots, monitor/area
//! screen casts) applies [`magnify_frame`] to the read-back frame, so
//! captures carry actual magnified pixels.

use std::collections::HashMap;
use std::time::SystemTime;

pub use tuna_magnifier::{MagnifierConfig, ScreenPosition, TrackingMode};

/// A magnified view: the on-screen viewport plus the source region of
/// interest (ROI), both in output-local physical pixels. The ROI may
/// extend past the frame when `scroll-at-edges` is on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MagnifiedView {
    /// Destination rect: full output, or one half.
    pub viewport: (i32, i32, i32, i32),
    /// Source rect in unmagnified output pixels (fractional).
    pub roi: (f64, f64, f64, f64),
}

/// Viewport for one output: full-screen fills it, halves split it, and
/// lens mode slides a non-full-screen viewport under the pointer.
/// `pointer` is output-local physical pixels.
pub fn viewport(
    output_w: i32,
    output_h: i32,
    position: ScreenPosition,
    lens_mode: bool,
    pointer: (f64, f64),
) -> (i32, i32, i32, i32) {
    let (x, y, w, h) = match position {
        ScreenPosition::FullScreen => (0, 0, output_w, output_h),
        ScreenPosition::TopHalf => (0, 0, output_w, output_h / 2),
        ScreenPosition::BottomHalf => (0, output_h - output_h / 2, output_w, output_h / 2),
        ScreenPosition::LeftHalf => (0, 0, output_w / 2, output_h),
        ScreenPosition::RightHalf => (output_w - output_w / 2, 0, output_w / 2, output_h),
    };
    // GNOME: lens mode does nothing full-screen ("a lens the size of
    // the screen is pointless"); otherwise the lens centers on the
    // pointer, clamped inside the output.
    if lens_mode && position != ScreenPosition::FullScreen {
        let x = (pointer.0 - f64::from(w) / 2.0).round() as i32;
        let y = (pointer.1 - f64::from(h) / 2.0).round() as i32;
        return (
            x.clamp(0, (output_w - w).max(0)),
            y.clamp(0, (output_h - h).max(0)),
            w,
            h,
        );
    }
    (x, y, w.max(1), h.max(1))
}

/// ROI size for a viewport at a magnification factor
/// (`getROI`: viewport / mag factor per axis).
pub fn roi_size(viewport_w: i32, viewport_h: i32, mag: f64) -> (f64, f64) {
    (f64::from(viewport_w) / mag, f64::from(viewport_h) / mag)
}

/// New ROI center for one tracking event, following
/// `_centerFromPoint{Centered,Proportional,Push}`. `point` and
/// `current` are output-local physical pixels; `screen` is the output
/// size; `view` is the viewport size.
pub fn track_center(
    mode: TrackingMode,
    point: (f64, f64),
    current: (f64, f64),
    roi: (f64, f64),
    screen: (f64, f64),
    view: (f64, f64),
    mag: f64,
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
            // ROI, then shift just enough to bring it back into view.
            let (rx, ry) = (current.0 - roi.0 / 2.0, current.1 - roi.1 / 2.0);
            let shift = |p: f64, lo: f64, size: f64| {
                if p < lo {
                    p - lo
                } else if p > lo + size {
                    p - (lo + size)
                } else {
                    0.0
                }
            };
            (
                current.0 + shift(point.0, rx, roi.0),
                current.1 + shift(point.1, ry, roi.1),
            )
        }
    }
}

/// Clamp the ROI center so the region stays fully on screen (GNOME's
/// default: `scroll-at-edges` off means clamp). With scrolling allowed
/// the center passes through and sampling fills past-edge texels black.
pub fn clamp_center(
    center: (f64, f64),
    roi: (f64, f64),
    screen: (f64, f64),
    scroll_at_edges: bool,
) -> (f64, f64) {
    if scroll_at_edges {
        return center;
    }
    let clamp_axis = |c: f64, size: f64, screen: f64| {
        if size >= screen {
            screen / 2.0
        } else {
            c.clamp(size / 2.0, screen - size / 2.0)
        }
    };
    (
        clamp_axis(center.0, roi.0, screen.0),
        clamp_axis(center.1, roi.1, screen.1),
    )
}

/// Resample `src` (w x h 4-byte texels, any channel order) through the
/// magnified view: the ROI scales to the viewport with bilinear
/// filtering, output outside the viewport copies through unchanged,
/// and ROI texels past the frame edge sample black. Returns the new
/// frame, or `None` when the input is not a w x h x 4 buffer.
pub fn magnify_frame(src: &[u8], w: i32, h: i32, view: MagnifiedView) -> Option<Vec<u8>> {
    if w <= 0 || h <= 0 {
        return None;
    }
    let (w_us, h_us) = (w as usize, h as usize);
    if src.len() != w_us * h_us * 4 {
        return None;
    }
    let (vx, vy, vw, vh) = view.viewport;
    if vw <= 0 || vh <= 0 {
        return None;
    }
    let (rx, ry, rw, rh) = view.roi;
    if rw.is_nan() || rw <= 0.0 || rh.is_nan() || rh <= 0.0 {
        return None;
    }
    let mut dst = src.to_vec();
    let sample = |x: f64, y: f64, channel: usize| -> u8 {
        // Bilinear over the four surrounding texels; outside samples
        // black with an opaque pad byte (RGBA 0,0,0,255 and BGRx
        // 0,0,0,255 agree on this encoding).
        let x0 = x.floor() as i32;
        let y0 = y.floor() as i32;
        let (fx, fy) = (x - f64::from(x0), y - f64::from(y0));
        let texel = |x: i32, y: i32| -> f64 {
            if x < 0 || y < 0 || x >= w || y >= h {
                return if channel == 3 { 255.0 } else { 0.0 };
            }
            f64::from(src[(y as usize * w_us + x as usize) * 4 + channel])
        };
        let top = texel(x0, y0) * (1.0 - fx) + texel(x0 + 1, y0) * fx;
        let bottom = texel(x0, y0 + 1) * (1.0 - fx) + texel(x0 + 1, y0 + 1) * fx;
        (top * (1.0 - fy) + bottom * fy).round().clamp(0.0, 255.0) as u8
    };
    for j in 0..vh {
        let sy = ry + f64::from(j) * rh / f64::from(vh);
        for i in 0..vw {
            let sx = rx + f64::from(i) * rw / f64::from(vw);
            let (dx, dy) = (vx + i, vy + j);
            if dx < 0 || dy < 0 || dx >= w || dy >= h {
                continue;
            }
            let base = (dy as usize * w_us + dx as usize) * 4;
            for c in 0..4 {
                dst[base + c] = sample(sx, sy, c);
            }
        }
    }
    Some(dst)
}

/// Live tracking inputs in global logical pixels: pointer always
/// known, caret from the text-input rectangle, focus from the focused
/// window's center.
#[derive(Debug, Clone, Copy, Default)]
pub struct TrackingInputs {
    pub pointer: (f64, f64),
    pub caret: Option<(f64, f64)>,
    pub focus: Option<(f64, f64)>,
}

/// Per-output magnifier state: the config file cache plus each
/// output's last ROI center and last seen inputs. Tracking is
/// event-driven like GNOME (mouse moves retarget at once; caret and
/// focus retarget when they change): a changed input wins over an
/// unchanged one, with mouse over caret over focus on ties.
#[derive(Default)]
pub struct MagnifierState {
    config: MagnifierConfig,
    loaded: Option<SystemTime>,
    centers: HashMap<String, (f64, f64)>,
    last_inputs: HashMap<String, TrackingInputs>,
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
            self.config = tuna_magnifier::load();
            self.loaded = now;
        }
    }

    /// Test hook: drive the state without touching the config file.
    #[cfg(test)]
    pub fn set_config(&mut self, config: MagnifierConfig) {
        self.config = config.sanitized();
        self.loaded = tuna_magnifier::mtime();
    }

    pub fn config(&self) -> &MagnifierConfig {
        &self.config
    }

    /// Active magnified view for one output, or `None` when the
    /// magnifier is off, unity (or below), or has no size to work
    /// with. `output_w/h` are physical pixels, `scale` the output
    /// scale, `offset` its global logical top-left, `inputs` the
    /// tracking points in global logical pixels.
    pub fn view_for(
        &mut self,
        output: &str,
        output_w: i32,
        output_h: i32,
        scale: f64,
        offset: (i32, i32),
        inputs: TrackingInputs,
    ) -> Option<MagnifiedView> {
        self.poll();
        if !self.config.active() || output_w <= 0 || output_h <= 0 || scale.is_nan() || scale <= 0.0
        {
            return None;
        }
        let mag = self.config.mag_factor;
        let to_local = |(gx, gy): (f64, f64)| {
            (
                (gx - f64::from(offset.0)) * scale,
                (gy - f64::from(offset.1)) * scale,
            )
        };
        let pointer = to_local(inputs.pointer);
        let screen = (f64::from(output_w), f64::from(output_h));
        let port = viewport(
            output_w,
            output_h,
            self.config.screen_position,
            self.config.lens_mode,
            pointer,
        );
        let (rw, rh) = roi_size(port.2, port.3, mag);
        let key = output.to_owned();
        let center = self
            .centers
            .get(&key)
            .copied()
            .unwrap_or((screen.0 / 2.0, screen.1 / 2.0));
        // Event edge detection: a changed input retargets; mouse wins
        // ties, then caret, then focus. Unchanged inputs hold the view
        // (notably push mode keeps its center while the point rests
        // inside the ROI).
        let last = self.last_inputs.get(&key).copied().unwrap_or(inputs);
        let pointer_moved = last.pointer != inputs.pointer;
        let caret_moved = last.caret != inputs.caret;
        let focus_moved = last.focus != inputs.focus;
        self.last_inputs.insert(key.clone(), inputs);
        let view_size = (f64::from(port.2), f64::from(port.3));
        let mut next = center;
        if pointer_moved && self.config.mouse_tracking != TrackingMode::None {
            next = track_center(
                self.config.mouse_tracking,
                pointer,
                center,
                (rw, rh),
                screen,
                view_size,
                mag,
            );
        } else if caret_moved {
            if let Some(caret) = inputs.caret {
                next = track_center(
                    self.config.caret_tracking,
                    to_local(caret),
                    center,
                    (rw, rh),
                    screen,
                    view_size,
                    mag,
                );
            }
        } else if focus_moved {
            if let Some(focus) = inputs.focus {
                next = track_center(
                    self.config.focus_tracking,
                    to_local(focus),
                    center,
                    (rw, rh),
                    screen,
                    view_size,
                    mag,
                );
            }
        } else if self.config.mouse_tracking != TrackingMode::None
            && !self.centers.contains_key(&key)
        {
            // First frame with no motion yet: settle the view on the
            // pointer rather than the screen middle.
            next = track_center(
                self.config.mouse_tracking,
                pointer,
                center,
                (rw, rh),
                screen,
                view_size,
                mag,
            );
        }
        let next = clamp_center(next, (rw, rh), screen, self.config.scroll_at_edges);
        self.centers.insert(key, next);
        Some(MagnifiedView {
            viewport: port,
            roi: (next.0 - rw / 2.0, next.1 - rh / 2.0, rw, rh),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled() -> MagnifierConfig {
        MagnifierConfig {
            enabled: true,
            ..MagnifierConfig::default()
        }
    }

    fn inputs(x: f64, y: f64) -> TrackingInputs {
        TrackingInputs {
            pointer: (x, y),
            caret: None,
            focus: None,
        }
    }

    #[test]
    fn viewport_halves_split_the_output() {
        assert_eq!(
            viewport(800, 600, ScreenPosition::FullScreen, false, (0.0, 0.0)),
            (0, 0, 800, 600)
        );
        assert_eq!(
            viewport(800, 600, ScreenPosition::TopHalf, false, (0.0, 0.0)),
            (0, 0, 800, 300)
        );
        assert_eq!(
            viewport(800, 600, ScreenPosition::BottomHalf, false, (0.0, 0.0)),
            (0, 300, 800, 300)
        );
        assert_eq!(
            viewport(800, 600, ScreenPosition::LeftHalf, false, (0.0, 0.0)),
            (0, 0, 400, 600)
        );
        assert_eq!(
            viewport(800, 600, ScreenPosition::RightHalf, false, (0.0, 0.0)),
            (400, 0, 400, 600)
        );
        // Odd sizes keep full coverage without gaps.
        assert_eq!(
            viewport(801, 601, ScreenPosition::BottomHalf, false, (0.0, 0.0)),
            (0, 301, 801, 300)
        );
        assert_eq!(
            viewport(801, 601, ScreenPosition::RightHalf, false, (0.0, 0.0)),
            (401, 0, 400, 601)
        );
    }

    #[test]
    fn lens_mode_follows_the_pointer_except_full_screen() {
        // Full-screen lens is pointless: the viewport stays put.
        assert_eq!(
            viewport(800, 600, ScreenPosition::FullScreen, true, (700.0, 500.0)),
            (0, 0, 800, 600)
        );
        // A half lens centers on the pointer (a full-height half has
        // nowhere to slide vertically).
        assert_eq!(
            viewport(800, 600, ScreenPosition::LeftHalf, true, (700.0, 500.0)),
            (400, 0, 400, 600)
        );
        assert_eq!(
            viewport(800, 600, ScreenPosition::TopHalf, true, (700.0, 500.0)),
            (0, 300, 800, 300)
        );
        // ...clamped inside the output.
        assert_eq!(
            viewport(800, 600, ScreenPosition::LeftHalf, true, (50.0, 50.0)),
            (0, 0, 400, 600)
        );
    }

    #[test]
    fn centered_tracking_puts_the_point_at_the_middle() {
        let center = track_center(
            TrackingMode::Centered,
            (100.0, 200.0),
            (400.0, 300.0),
            (400.0, 300.0),
            (800.0, 600.0),
            (800.0, 600.0),
            2.0,
        );
        assert_eq!(center, (100.0, 200.0));
        // None holds the view still.
        let held = track_center(
            TrackingMode::None,
            (100.0, 200.0),
            (400.0, 300.0),
            (400.0, 300.0),
            (800.0, 600.0),
            (800.0, 600.0),
            2.0,
        );
        assert_eq!(held, (400.0, 300.0));
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
        );
        assert_eq!(middle, (400.0, 300.0));
    }

    #[test]
    fn push_tracking_scrolls_only_past_the_boundary() {
        // ROI 400x300 centered on (400,300): spans x 200..600.
        let inside = track_center(
            TrackingMode::Push,
            (500.0, 300.0),
            (400.0, 300.0),
            (400.0, 300.0),
            (800.0, 600.0),
            (800.0, 600.0),
            2.0,
        );
        assert_eq!(inside, (400.0, 300.0));
        // Past the right edge by 50: shift right by 50.
        let pushed = track_center(
            TrackingMode::Push,
            (650.0, 100.0),
            (400.0, 300.0),
            (400.0, 300.0),
            (800.0, 600.0),
            (800.0, 600.0),
            2.0,
        );
        assert_eq!(pushed, (450.0, 250.0));
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
    }

    #[test]
    fn disabled_or_unity_magnifier_has_no_view() {
        let mut state = MagnifierState::new();
        state.set_config(MagnifierConfig::default());
        assert!(state
            .view_for("tuna-0", 800, 600, 1.0, (0, 0), inputs(10.0, 10.0))
            .is_none());
        let mut unity = enabled();
        unity.mag_factor = 1.0;
        state.set_config(unity);
        assert!(state
            .view_for("tuna-0", 800, 600, 1.0, (0, 0), inputs(10.0, 10.0))
            .is_none());
        assert!(state
            .view_for("tuna-0", 0, 600, 1.0, (0, 0), inputs(10.0, 10.0))
            .is_none());
    }

    #[test]
    fn first_frame_settles_on_the_pointer() {
        let mut state = MagnifierState::new();
        let mut config = enabled();
        config.mouse_tracking = TrackingMode::Centered;
        state.set_config(config);
        let view = state
            .view_for("tuna-0", 800, 600, 1.0, (0, 0), inputs(100.0, 100.0))
            .unwrap();
        assert_eq!(view.viewport, (0, 0, 800, 600));
        // ROI 400x300 centered on the pointer, clamped on screen.
        assert_eq!((view.roi.0, view.roi.1), (0.0, 0.0));
        assert_eq!((view.roi.2, view.roi.3), (400.0, 300.0));
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
            .view_for("tuna-0", 800, 600, 1.0, (0, 0), inputs(700.0, 500.0))
            .unwrap();
        assert_eq!((view.roi.0, view.roi.1), (200.0, 150.0));
        // A caret event retargets to the caret.
        let caret = TrackingInputs {
            pointer: (700.0, 500.0),
            caret: Some((100.0, 100.0)),
            focus: None,
        };
        let view = state
            .view_for("tuna-0", 800, 600, 1.0, (0, 0), caret)
            .unwrap();
        assert_eq!((view.roi.0, view.roi.1), (0.0, 0.0));
        assert_eq!((view.roi.2, view.roi.3), (400.0, 300.0));
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
            .view_for("tuna-0", 800, 600, 1.0, (0, 0), inputs(0.0, 0.0))
            .unwrap();
        assert_eq!((view.roi.0, view.roi.1), (200.0, 150.0));
        // A focus event retargets to the focused window's center.
        let focus = TrackingInputs {
            pointer: (0.0, 0.0),
            caret: None,
            focus: Some((600.0, 450.0)),
        };
        let view = state
            .view_for("tuna-0", 800, 600, 1.0, (0, 0), focus)
            .unwrap();
        assert_eq!((view.roi.0, view.roi.1), (400.0, 300.0));
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
            .view_for("eDP-1", 800, 600, 1.0, (0, 0), inputs(1000.0, 300.0))
            .unwrap();
        let right = state
            .view_for("HDMI-1", 800, 600, 1.0, (800, 0), inputs(1000.0, 300.0))
            .unwrap();
        // Right output: pointer is local (200,300), ROI clamped.
        assert_eq!((right.roi.0, right.roi.1), (0.0, 150.0));
        // Left output: pointer is off-output to the right; the ROI
        // clamps to the nearest edge instead of leaving the screen.
        assert_eq!((left.roi.0, left.roi.1), (400.0, 150.0));
    }

    #[test]
    fn fractional_scale_converts_logical_tracking_points() {
        let mut state = MagnifierState::new();
        let mut config = enabled();
        config.mouse_tracking = TrackingMode::Centered;
        state.set_config(config);
        // 150% output: logical (100,100) is physical (150,150).
        let view = state
            .view_for("tuna-0", 1200, 900, 1.5, (0, 0), inputs(100.0, 100.0))
            .unwrap();
        assert_eq!((view.roi.2, view.roi.3), (600.0, 450.0));
        assert_eq!((view.roi.0, view.roi.1), (0.0, 0.0));
    }

    /// Solid test frame: left half red, right half blue (RGBA).
    fn split_frame(w: i32, h: i32) -> Vec<u8> {
        let mut frame = vec![0u8; w as usize * h as usize * 4];
        for y in 0..h as usize {
            for x in 0..w as usize {
                let base = (y * w as usize + x) * 4;
                if x < w as usize / 2 {
                    frame[base..base + 4].copy_from_slice(&[255, 0, 0, 255]);
                } else {
                    frame[base..base + 4].copy_from_slice(&[0, 0, 255, 255]);
                }
            }
        }
        frame
    }

    #[test]
    fn doubled_frame_scales_source_blocks() {
        // 8x8 frame, left red / right blue; 2x zoom of the left
        // quarter must paint the whole viewport red.
        let frame = split_frame(8, 8);
        let view = MagnifiedView {
            viewport: (0, 0, 8, 8),
            roi: (0.0, 0.0, 4.0, 4.0),
        };
        let out = magnify_frame(&frame, 8, 8, view).unwrap();
        assert_eq!(out.len(), frame.len());
        for y in 0..8usize {
            for x in 0..7usize {
                let base = (y * 8 + x) * 4;
                assert_eq!(
                    &out[base..base + 4],
                    &[255, 0, 0, 255],
                    "viewport pixel ({x},{y}) samples the red quarter"
                );
            }
            // The last column straddles the red/blue boundary.
            let edge = (y * 8 + 7) * 4;
            assert_eq!(&out[edge..edge + 4], &[128, 0, 128, 255]);
        }
        // The same ROI shifted over the boundary blends red into blue
        // across the middle columns.
        let view = MagnifiedView {
            viewport: (0, 0, 8, 8),
            roi: (2.0, 0.0, 4.0, 4.0),
        };
        let out = magnify_frame(&frame, 8, 8, view).unwrap();
        let at = |x: usize| out[x * 4..x * 4 + 3].to_vec();
        assert_eq!(at(0), vec![255, 0, 0]);
        assert_eq!(at(7), vec![0, 0, 255]);
        let mid = at(3);
        assert!(
            mid[0] > 0 && mid[2] > 0,
            "boundary column blends, got {mid:?}"
        );
    }

    #[test]
    fn half_viewport_leaves_the_other_half_untouched() {
        let frame = split_frame(8, 8);
        let view = MagnifiedView {
            viewport: (0, 0, 8, 4),
            roi: (4.0, 0.0, 4.0, 4.0),
        };
        let out = magnify_frame(&frame, 8, 8, view).unwrap();
        // Top half: zoomed blue quarter.
        assert_eq!(&out[0..4], &[0, 0, 255, 255]);
        // Bottom half: original split frame.
        let row4_left = (4 * 8) * 4;
        assert_eq!(&out[row4_left..row4_left + 4], &[255, 0, 0, 255]);
        let row4_right = (4 * 8 + 7) * 4;
        assert_eq!(&out[row4_right..row4_right + 4], &[0, 0, 255, 255]);
    }

    #[test]
    fn past_edge_roi_samples_black() {
        let frame = split_frame(8, 8);
        let view = MagnifiedView {
            viewport: (0, 0, 8, 8),
            roi: (-4.0, 0.0, 4.0, 4.0),
        };
        let out = magnify_frame(&frame, 8, 8, view).unwrap();
        assert_eq!(&out[0..4], &[0, 0, 0, 255]);
    }

    #[test]
    fn malformed_frames_are_rejected_not_panicked() {
        let view = MagnifiedView {
            viewport: (0, 0, 8, 8),
            roi: (0.0, 0.0, 4.0, 4.0),
        };
        assert!(magnify_frame(&[0u8; 10], 8, 8, view).is_none());
        assert!(magnify_frame(&[0u8; 8 * 8 * 4], 0, 8, view).is_none());
        let empty_roi = MagnifiedView {
            viewport: (0, 0, 8, 8),
            roi: (0.0, 0.0, 0.0, 4.0),
        };
        assert!(magnify_frame(&[0u8; 8 * 8 * 4], 8, 8, empty_roi).is_none());
    }
}
