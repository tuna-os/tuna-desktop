//! Screen magnifier settings shared by the GTK shell and the compositor (#349).
//!
//! GNOME implements the magnifier in gnome-shell, which reads GSettings in
//! the compositor process. Tuna Desktop splits the shell (`tuna-shell-gtk`)
//! from the compositor (`tuna-compositor`); the two meet through a small
//! session-scoped JSON file instead of the versioned control channel,
//! mirroring how the shell publishes `tuna-wallpaper` for the compositor
//! to consume. No wire-schema change is needed to add or extend keys.
//!
//! Reference: `org.gnome.desktop.a11y.magnifier` at
//! gsettings-desktop-schemas 51.0 (`schemas/org.gnome.desktop.a11y.magnifier.gschema.xml.in`)
//! and gnome-shell 51.0 `js/ui/magnifier.js`. Defaults below match 51.0.
//!
//! Stability: internal. This crate is not published and makes no SemVer
//! promise; its session JSON file is a cross-process contract between
//! components built from one commit. See `docs/api-stability.md`.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Tracking modes for mouse, focus and caret (`...TrackingMode` enums).
///
/// Discriminants match `headers/gdesktop-enums.h` at 51.0
/// (`NONE=0, CENTERED=1, PROPORTIONAL=2, PUSH=3`) so the shell can map
/// `Settings.get_enum` straight through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum TrackingMode {
    /// No tracking: the magnified view stays where it is.
    None,
    /// The tracked point sits at the center of the zoom region.
    Centered,
    /// The tracked point's position in the zoom region mirrors its
    /// position on screen (GNOME's default for mouse and focus).
    #[default]
    Proportional,
    /// The view scrolls only when the tracked point hits its boundary.
    Push,
}

impl TrackingMode {
    /// Map a GSettings enum value; unknown values fail closed to `None`
    /// (no view movement), never to a guessed mode.
    pub fn from_gnome(value: i32) -> Self {
        match value {
            1 => Self::Centered,
            2 => Self::Proportional,
            3 => Self::Push,
            _ => Self::None,
        }
    }
}

/// Where the magnified view sits (`GDesktopMagnifierScreenPosition`).
///
/// Discriminants match `headers/gdesktop-enums.h` at 51.0
/// (`NONE=0, FULL_SCREEN=1, TOP_HALF=2, BOTTOM_HALF=3, LEFT_HALF=4,
/// RIGHT_HALF=5`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ScreenPosition {
    /// The magnified view fills the output (GNOME's default).
    #[default]
    FullScreen,
    /// The magnified view fills the top half; the rest stays unmagnified.
    TopHalf,
    /// The magnified view fills the bottom half.
    BottomHalf,
    /// The magnified view fills the left half.
    LeftHalf,
    /// The magnified view fills the right half.
    RightHalf,
}

impl ScreenPosition {
    /// Map a GSettings enum value. GNOME's Settings UI only offers the
    /// full/half positions; `NONE` and unknown values fall back to
    /// full-screen, matching a zoom region that was never positioned.
    pub fn from_gnome(value: i32) -> Self {
        match value {
            2 => Self::TopHalf,
            3 => Self::BottomHalf,
            4 => Self::LeftHalf,
            5 => Self::RightHalf,
            _ => Self::FullScreen,
        }
    }
}

/// GNOME's default magnification factor (`mag-factor` at 51.0).
pub const DEFAULT_MAG_FACTOR: f64 = 2.0;
/// Schema range for `mag-factor` at 51.0.
pub const MIN_MAG_FACTOR: f64 = 0.1;
/// Schema range for `mag-factor` at 51.0.
pub const MAX_MAG_FACTOR: f64 = 32.0;

/// Schema defaults for the cross-hairs keys at 51.0.
pub const DEFAULT_CROSS_HAIRS_THICKNESS: i32 = 8;
pub const DEFAULT_CROSS_HAIRS_COLOR: &str = "#ff0000";
pub const DEFAULT_CROSS_HAIRS_OPACITY: f64 = 0.66;
pub const DEFAULT_CROSS_HAIRS_LENGTH: i32 = 4096;
/// Schema range for `cross-hairs-length` at 51.0.
pub const MIN_CROSS_HAIRS_LENGTH: i32 = 20;
pub const MAX_CROSS_HAIRS_LENGTH: i32 = 4096;

/// `org.gnome.desktop.a11y.magnifier` plus the `screen-magnifier-enabled`
/// switch: every key gnome-shell 51.0's `magnifier.js` consumes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MagnifierConfig {
    /// `screen-magnifier-enabled`: the master switch.
    pub enabled: bool,
    /// `mag-factor`: 0.1..=32.0, accurate to two decimals like GNOME.
    pub mag_factor: f64,
    /// `screen-position`: full-screen or one half.
    pub screen_position: ScreenPosition,
    /// `mouse-tracking` (default proportional).
    pub mouse_tracking: TrackingMode,
    /// `focus-tracking` (default proportional).
    pub focus_tracking: TrackingMode,
    /// `caret-tracking` (default centered).
    pub caret_tracking: TrackingMode,
    /// `scroll-at-edges`: when false (default) the region of interest
    /// stays clamped inside the output.
    pub scroll_at_edges: bool,
    /// `lens-mode`: a non-full-screen view follows the tracked point.
    pub lens_mode: bool,
    /// `show-cross-hairs`.
    pub show_cross_hairs: bool,
    /// `cross-hairs-thickness` in logical pixels.
    pub cross_hairs_thickness: i32,
    /// `cross-hairs-color`, parsed like `Cogl.Color.from_string`.
    pub cross_hairs_color: String,
    /// `cross-hairs-opacity`: 0.0..=1.0.
    pub cross_hairs_opacity: f64,
    /// `cross-hairs-length`: 20..=4096 device pixels.
    pub cross_hairs_length: i32,
    /// `cross-hairs-clip`: leave a 100x100 hole around the pointer.
    pub cross_hairs_clip: bool,
    /// `invert-lightness`.
    pub invert_lightness: bool,
    /// `color-saturation`: 0.0 (gray) ..= 1.0 (unchanged).
    pub color_saturation: f64,
    /// `brightness-red`, `-green`, `-blue`: -1.0..=1.0, 0 unchanged.
    pub brightness: [f64; 3],
    /// `contrast-red`, `-green`, `-blue`: -1.0..=1.0, 0 unchanged.
    pub contrast: [f64; 3],
}

impl Default for MagnifierConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            mag_factor: DEFAULT_MAG_FACTOR,
            screen_position: ScreenPosition::FullScreen,
            mouse_tracking: TrackingMode::Proportional,
            focus_tracking: TrackingMode::Proportional,
            caret_tracking: TrackingMode::Centered,
            scroll_at_edges: false,
            lens_mode: false,
            show_cross_hairs: false,
            cross_hairs_thickness: DEFAULT_CROSS_HAIRS_THICKNESS,
            cross_hairs_color: DEFAULT_CROSS_HAIRS_COLOR.to_owned(),
            cross_hairs_opacity: DEFAULT_CROSS_HAIRS_OPACITY,
            cross_hairs_length: DEFAULT_CROSS_HAIRS_LENGTH,
            cross_hairs_clip: false,
            invert_lightness: false,
            color_saturation: 1.0,
            brightness: [0.0; 3],
            contrast: [0.0; 3],
        }
    }
}

/// `value` when finite, else `fallback`, clamped to `lo..=hi`.
fn finite_in(value: f64, fallback: f64, lo: f64, hi: f64) -> f64 {
    if value.is_finite() {
        value.clamp(lo, hi)
    } else {
        fallback
    }
}

impl MagnifierConfig {
    /// Clamp to the schema ranges. A non-positive or non-finite factor
    /// cannot come from GSettings (range 0.1..=32.0); a hand-edited file
    /// still fails safe to GNOME's defaults rather than panicking or
    /// inverting the view.
    pub fn sanitized(&self) -> Self {
        let mut out = self.clone();
        out.mag_factor = if out.mag_factor.is_finite() && out.mag_factor > 0.0 {
            // GNOME rounds to two decimals (`_updateMagFactor`).
            let rounded = (out.mag_factor * 100.0).round() / 100.0;
            rounded.clamp(MIN_MAG_FACTOR, MAX_MAG_FACTOR)
        } else {
            DEFAULT_MAG_FACTOR
        };
        // The thickness key has no schema range; a negative actor size
        // draws nothing in Clutter, so negative means no line.
        out.cross_hairs_thickness = out.cross_hairs_thickness.max(0);
        out.cross_hairs_length = out
            .cross_hairs_length
            .clamp(MIN_CROSS_HAIRS_LENGTH, MAX_CROSS_HAIRS_LENGTH);
        out.cross_hairs_opacity = finite_in(
            out.cross_hairs_opacity,
            DEFAULT_CROSS_HAIRS_OPACITY,
            0.0,
            1.0,
        );
        out.color_saturation = finite_in(out.color_saturation, 1.0, 0.0, 1.0);
        for value in out.brightness.iter_mut().chain(out.contrast.iter_mut()) {
            *value = finite_in(*value, 0.0, -1.0, 1.0);
        }
        out
    }

    /// Whether the zoom region is shown. GNOME shows it whenever the
    /// switch is on, at any factor: 1.0 is a legitimate setting (the
    /// color effects and cross-hairs still apply) and below 1.0 the
    /// region of interest is larger than the view.
    pub fn active(&self) -> bool {
        self.enabled
    }

    /// The cross-hairs color as straight RGBA bytes, or transparent
    /// black when the string does not parse (GJS hands back the zeroed
    /// out-parameter of a failed `Cogl.Color.from_string`).
    pub fn cross_hairs_rgba(&self) -> [u8; 4] {
        parse_cogl_color(&self.cross_hairs_color).unwrap_or([0, 0, 0, 0])
    }
}

/// `gsd-media-keys`' `do_magnifier_zoom_action` at 51.0: add or subtract
/// one and round. GSettings refuses a value outside the schema range,
/// so such a step is `None` and leaves the factor unchanged.
pub fn zoom_step(current: f64, zoom_in: bool) -> Option<f64> {
    let next = (current + if zoom_in { 1.0 } else { -1.0 }).round();
    (next.is_finite() && (MIN_MAG_FACTOR..=MAX_MAG_FACTOR).contains(&next)).then_some(next)
}

/// Skip ASCII whitespace, as Cogl's `skip_whitespace`.
fn skip_ws(s: &str) -> &str {
    s.trim_start_matches(|c: char| c.is_ascii_whitespace())
}

/// `g_ascii_strtod`: the longest leading decimal number (0 when none)
/// and the rest of the string.
fn strtod(s: &str) -> (f64, &str) {
    let bytes = s.as_bytes();
    let mut end = 0;
    if end < bytes.len() && (bytes[end] == b'+' || bytes[end] == b'-') {
        end += 1;
    }
    let digits_start = end;
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
    }
    if end < bytes.len() && bytes[end] == b'.' {
        end += 1;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
        }
    }
    if end == digits_start || &s[digits_start..end] == "." {
        return (0.0, s);
    }
    (s[..end].parse().unwrap_or(0.0), &s[end..])
}

/// Cogl's `parse_rgb_value`: a number, or a percentage of 255.
fn rgb_value(s: &str) -> (u8, &str) {
    let (number, rest) = strtod(skip_ws(s));
    let after = skip_ws(rest);
    if let Some(rest) = after.strip_prefix('%') {
        (((number / 100.0).clamp(0.0, 1.0) * 255.0) as u8, rest)
    } else {
        (number.clamp(0.0, 255.0) as u8, rest)
    }
}

fn parse_rgba(s: &str, has_alpha: bool) -> Option<[u8; 4]> {
    let s = skip_ws(s).strip_prefix('(')?;
    let (r, s) = rgb_value(s);
    let s = skip_ws(s).strip_prefix(',')?;
    let (g, s) = rgb_value(s);
    let s = skip_ws(s).strip_prefix(',')?;
    let (b, s) = rgb_value(s);
    let mut s = skip_ws(s);
    let a = if has_alpha {
        let rest = s.strip_prefix(',')?;
        let (number, rest) = strtod(skip_ws(rest));
        s = rest;
        (number * 255.0).clamp(0.0, 255.0) as u8
    } else {
        255
    };
    skip_ws(s).starts_with(')').then_some([r, g, b, a])
}

/// `cogl_color_init_from_hsl`.
fn hsl_to_rgb(hue: f32, saturation: f32, luminance: f32) -> [u8; 3] {
    let to_byte = |v: f32| (v * 255.0) as i32 as u8;
    let hue = hue / 360.0;
    if saturation == 0.0 {
        let l = to_byte(luminance);
        return [l, l, l];
    }
    let tmp2 = if luminance <= 0.5 {
        luminance * (1.0 + saturation)
    } else {
        luminance + saturation - luminance * saturation
    };
    let tmp1 = 2.0 * luminance - tmp2;
    let mut out = [0u8; 3];
    for (i, offset) in [1.0 / 3.0, 0.0, -1.0 / 3.0].into_iter().enumerate() {
        let mut t = hue + offset;
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        let c = if 6.0 * t < 1.0 {
            tmp1 + (tmp2 - tmp1) * t * 6.0
        } else if 2.0 * t < 1.0 {
            tmp2
        } else if 3.0 * t < 2.0 {
            tmp1 + (tmp2 - tmp1) * ((2.0 / 3.0) - t) * 6.0
        } else {
            tmp1
        };
        out[i] = to_byte(c);
    }
    out
}

fn parse_hsla(s: &str, has_alpha: bool) -> Option<[u8; 4]> {
    let s = skip_ws(s).strip_prefix('(')?;
    let (h, s) = strtod(skip_ws(s));
    let s = skip_ws(s).strip_prefix(',')?;
    let (sat, s) = strtod(skip_ws(s));
    let s = skip_ws(s).strip_prefix('%')?;
    let s = skip_ws(s).strip_prefix(',')?;
    let (lum, s) = strtod(skip_ws(s));
    let s = skip_ws(s).strip_prefix('%')?;
    let mut s = skip_ws(s);
    let alpha = if has_alpha {
        let rest = s.strip_prefix(',')?;
        let (number, rest) = strtod(skip_ws(rest));
        s = rest;
        (number * 255.0).clamp(0.0, 255.0)
    } else {
        255.0
    };
    if !skip_ws(s).starts_with(')') {
        return None;
    }
    let [r, g, b] = hsl_to_rgb(
        h as f32,
        (sat / 100.0).clamp(0.0, 1.0) as f32,
        (lum / 100.0).clamp(0.0, 1.0) as f32,
    );
    Some([r, g, b, alpha as u8])
}

/// `cogl_color_from_string` at mutter 51.0 (`cogl/cogl/cogl-color.c`):
/// `rgb()`, `rgba()`, `hsl()`, `hsla()` and `#rgb`, `#rgba`,
/// `#rrggbb`, `#rrggbbaa`. Cogl knows no color names. Straight
/// (non-premultiplied) RGBA bytes.
pub fn parse_cogl_color(text: &str) -> Option<[u8; 4]> {
    if let Some(rest) = text.strip_prefix("rgb") {
        return match rest.strip_prefix('a') {
            Some(rest) => parse_rgba(rest, true),
            None => parse_rgba(rest, false),
        };
    }
    if let Some(rest) = text.strip_prefix("hsl") {
        return match rest.strip_prefix('a') {
            Some(rest) => parse_hsla(rest, true),
            None => parse_hsla(rest, false),
        };
    }
    let hex = text.strip_prefix('#').filter(|rest| !rest.is_empty())?;
    // `sscanf ("%x")` reads the leading hex digits; the notation comes
    // from the whole length, as in Cogl.
    let digits: String = hex.chars().take_while(char::is_ascii_hexdigit).collect();
    if digits.is_empty() || digits.len() > 8 {
        return None;
    }
    let value = u32::from_str_radix(&digits, 16).ok()?;
    let byte = |shift: u32| ((value >> shift) & 0xff) as u8;
    let nibble = |shift: u32| {
        let n = ((value >> shift) & 0xf) as u8;
        (n << 4) | n
    };
    match hex.len() {
        8 => Some([byte(24), byte(16), byte(8), byte(0)]),
        6 => Some([byte(16), byte(8), byte(0), 255]),
        4 => Some([nibble(12), nibble(8), nibble(4), nibble(0)]),
        3 => Some([nibble(8), nibble(4), nibble(0), 255]),
        _ => None,
    }
}

/// `$XDG_RUNTIME_DIR/tuna-magnifier.json`: a session-scoped snapshot
/// of the magnifier keys, next to the shell's `tuna-wallpaper` file.
/// GSettings stays the persistent store (restart persistence comes
/// from dconf plus the shell republishing at startup); a missing file
/// simply means "magnifier off", so a compositor without a shell never
/// inherits a stale zoom.
pub fn path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from) {
        return Some(dir.join("tuna-magnifier.json"));
    }
    // No runtime dir (bare test harness): a UID-suffixed temp file so
    // users never share one magnifier state.
    let uid = rustix::process::getuid();
    Some(std::env::temp_dir().join(format!("tuna-magnifier-{}.json", uid.as_raw())))
}

/// Load the file; a missing or unreadable file means "magnifier off with
/// GNOME defaults", never an error that breaks the session.
pub fn load() -> MagnifierConfig {
    path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|text| serde_json::from_str::<MagnifierConfig>(&text).ok())
        .unwrap_or_default()
        .sanitized()
}

/// Modification time of the file, for the compositor's per-frame poll.
pub fn mtime() -> Option<std::time::SystemTime> {
    path()
        .and_then(|p| std::fs::metadata(p).ok())
        .and_then(|m| m.modified().ok())
}

/// Save atomically (write temp + rename), like `monitors::save`.
pub fn save(config: &MagnifierConfig) -> std::io::Result<PathBuf> {
    let file = path().ok_or_else(|| std::io::Error::other("no config dir"))?;
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = file.with_extension("json.tmp");
    std::fs::write(
        &tmp,
        serde_json::to_string_pretty(config).unwrap_or_default(),
    )?;
    std::fs::rename(&tmp, &file)?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_gnome_51() {
        let config = MagnifierConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.mag_factor, 2.0);
        assert_eq!(config.screen_position, ScreenPosition::FullScreen);
        assert_eq!(config.mouse_tracking, TrackingMode::Proportional);
        assert_eq!(config.focus_tracking, TrackingMode::Proportional);
        assert_eq!(config.caret_tracking, TrackingMode::Centered);
        assert!(!config.scroll_at_edges);
        assert!(!config.lens_mode);
        assert!(!config.show_cross_hairs);
        assert_eq!(config.cross_hairs_thickness, 8);
        assert_eq!(config.cross_hairs_color, "#ff0000");
        assert_eq!(config.cross_hairs_opacity, 0.66);
        assert_eq!(config.cross_hairs_length, 4096);
        assert!(!config.cross_hairs_clip);
        assert!(!config.invert_lightness);
        assert_eq!(config.color_saturation, 1.0);
        assert_eq!(config.brightness, [0.0; 3]);
        assert_eq!(config.contrast, [0.0; 3]);
    }

    #[test]
    fn sanitize_clamps_effect_and_cross_hair_keys_to_the_schema() {
        let config = MagnifierConfig {
            cross_hairs_thickness: -4,
            cross_hairs_length: 5,
            cross_hairs_opacity: 3.0,
            color_saturation: f64::NAN,
            brightness: [-2.0, 0.25, f64::INFINITY],
            contrast: [2.0, -0.5, f64::NAN],
            ..MagnifierConfig::default()
        }
        .sanitized();
        assert_eq!(config.cross_hairs_thickness, 0);
        assert_eq!(config.cross_hairs_length, MIN_CROSS_HAIRS_LENGTH);
        assert_eq!(config.cross_hairs_opacity, 1.0);
        assert_eq!(config.color_saturation, 1.0);
        assert_eq!(config.brightness, [-1.0, 0.25, 0.0]);
        assert_eq!(config.contrast, [1.0, -0.5, 0.0]);
    }

    #[test]
    fn cogl_color_strings_parse_like_mutter_51() {
        assert_eq!(parse_cogl_color("#ff0000"), Some([255, 0, 0, 255]));
        assert_eq!(parse_cogl_color("#00ff0080"), Some([0, 255, 0, 128]));
        assert_eq!(parse_cogl_color("#f0a"), Some([255, 0, 170, 255]));
        assert_eq!(parse_cogl_color("#f0a8"), Some([255, 0, 170, 136]));
        assert_eq!(parse_cogl_color("rgb(10, 20, 30)"), Some([10, 20, 30, 255]));
        assert_eq!(
            parse_cogl_color("rgba(100%, 0, 50%, 0.5)"),
            Some([255, 0, 127, 127])
        );
        assert_eq!(parse_cogl_color("rgb(300, -5, 7)"), Some([255, 0, 7, 255]));
        assert_eq!(
            parse_cogl_color("hsl(0, 100%, 50%)"),
            Some([255, 0, 0, 255])
        );
        assert_eq!(
            parse_cogl_color("hsl(120, 0%, 50%)"),
            Some([127, 127, 127, 255])
        );
        assert_eq!(
            parse_cogl_color("hsla(240, 100%, 50%, 1.0)"),
            Some([0, 0, 255, 255])
        );
        // No names, malformed notations, and wrong lengths fail.
        for bad in [
            "red",
            "",
            "#",
            "#12345",
            "#1234567",
            "rgb(1,2)",
            "rgb(1,2,3",
            "hsl(1,2,3)",
        ] {
            assert_eq!(parse_cogl_color(bad), None, "{bad:?}");
        }
        let config = MagnifierConfig {
            cross_hairs_color: "nonsense".into(),
            ..MagnifierConfig::default()
        };
        assert_eq!(config.cross_hairs_rgba(), [0, 0, 0, 0]);
    }

    #[test]
    fn zoom_keys_step_by_one_and_round_like_gsd_media_keys() {
        assert_eq!(zoom_step(2.0, true), Some(3.0));
        assert_eq!(zoom_step(2.0, false), Some(1.0));
        assert_eq!(zoom_step(1.25, true), Some(2.0));
        assert_eq!(zoom_step(2.5, false), Some(2.0));
        assert_eq!(zoom_step(31.4, true), Some(32.0));
        assert_eq!(zoom_step(31.6, true), None);
        // GSettings rejects the out-of-range result: no change.
        assert_eq!(zoom_step(1.0, false), None);
        assert_eq!(zoom_step(32.0, true), None);
    }

    #[test]
    fn gnome_enum_mapping_matches_gdesktop_enums_h() {
        assert_eq!(TrackingMode::from_gnome(0), TrackingMode::None);
        assert_eq!(TrackingMode::from_gnome(1), TrackingMode::Centered);
        assert_eq!(TrackingMode::from_gnome(2), TrackingMode::Proportional);
        assert_eq!(TrackingMode::from_gnome(3), TrackingMode::Push);
        assert_eq!(TrackingMode::from_gnome(99), TrackingMode::None);
        assert_eq!(TrackingMode::from_gnome(-1), TrackingMode::None);
        assert_eq!(ScreenPosition::from_gnome(0), ScreenPosition::FullScreen);
        assert_eq!(ScreenPosition::from_gnome(1), ScreenPosition::FullScreen);
        assert_eq!(ScreenPosition::from_gnome(2), ScreenPosition::TopHalf);
        assert_eq!(ScreenPosition::from_gnome(3), ScreenPosition::BottomHalf);
        assert_eq!(ScreenPosition::from_gnome(4), ScreenPosition::LeftHalf);
        assert_eq!(ScreenPosition::from_gnome(5), ScreenPosition::RightHalf);
        assert_eq!(ScreenPosition::from_gnome(99), ScreenPosition::FullScreen);
    }

    #[test]
    fn sanitize_clamps_to_schema_range_and_rounds() {
        let case = |factor: f64| {
            MagnifierConfig {
                mag_factor: factor,
                ..MagnifierConfig::default()
            }
            .sanitized()
            .mag_factor
        };
        assert_eq!(case(2.0), 2.0);
        assert_eq!(case(2.005), 2.01);
        assert_eq!(case(2.004), 2.0);
        assert_eq!(case(0.05), 0.1);
        assert_eq!(case(99.0), 32.0);
        assert_eq!(case(0.0), 2.0);
        assert_eq!(case(-3.0), 2.0);
        assert_eq!(case(f64::NAN), 2.0);
        assert_eq!(case(f64::INFINITY), 2.0);
    }

    #[test]
    fn active_follows_the_switch_at_every_factor_like_gnome() {
        let mut config = MagnifierConfig::default();
        assert!(!config.active());
        config.enabled = true;
        assert!(config.active());
        // GNOME keeps the zoom region (and its color effects) at 1.0
        // and zooms out below it.
        config.mag_factor = 1.0;
        assert!(config.active());
        config.mag_factor = 0.5;
        assert!(config.active());
        config.enabled = false;
        assert!(!config.active());
    }

    #[test]
    fn file_roundtrip_survives_missing_and_corrupt_files() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_RUNTIME_DIR", dir.path());
        // Missing file: defaults, disabled.
        assert!(!load().enabled);
        let config = MagnifierConfig {
            enabled: true,
            mag_factor: 4.0,
            screen_position: ScreenPosition::LeftHalf,
            mouse_tracking: TrackingMode::Push,
            ..MagnifierConfig::default()
        };
        let saved = save(&config).unwrap();
        assert!(saved.ends_with("tuna-magnifier.json"));
        assert_eq!(load().sanitized(), config.sanitized());
        // Corrupt file: defaults again, no panic.
        std::fs::write(&saved, "{ not json").unwrap();
        let back = load();
        assert!(!back.enabled);
        assert_eq!(back.mag_factor, DEFAULT_MAG_FACTOR);
        std::env::remove_var("XDG_RUNTIME_DIR");
    }

    #[test]
    fn partial_json_keeps_gnome_defaults_for_missing_keys() {
        let config: MagnifierConfig = serde_json::from_str(r#"{"enabled":true}"#).unwrap();
        assert!(config.enabled);
        assert_eq!(config.mag_factor, DEFAULT_MAG_FACTOR);
        assert_eq!(config.caret_tracking, TrackingMode::Centered);
    }
}
