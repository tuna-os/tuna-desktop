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

/// The honored subset of `org.gnome.desktop.a11y.magnifier` plus the
/// `screen-magnifier-enabled` switch. Cross-hairs and color-filter keys
/// are out of scope for this slice (#349) and stay unconsumed.
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
    /// `lens-mode`: a non-full-screen view follows the pointer.
    pub lens_mode: bool,
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
        }
    }
}

impl MagnifierConfig {
    /// Clamp to the schema ranges. A non-positive or non-finite factor
    /// cannot come from GSettings (range 0.1..=32.0); a hand-edited file
    /// still fails safe to GNOME's default rather than panicking or
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
        out
    }

    /// Whether any frame work is needed: enabled with a factor that
    /// actually magnifies. A factor at or below 1.0 renders the source
    /// unchanged, so the compositor skips the resample.
    pub fn active(&self) -> bool {
        self.enabled && self.mag_factor > 1.0
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
    fn active_needs_enabled_and_real_magnification() {
        let mut config = MagnifierConfig::default();
        assert!(!config.active());
        config.enabled = true;
        assert!(config.active());
        config.mag_factor = 1.0;
        assert!(!config.active());
        config.mag_factor = 0.5;
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
