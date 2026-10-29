//! Screen magnifier Settings sync (#349).
//!
//! Mirrors `screen-magnifier-enabled` plus every
//! `org.gnome.desktop.a11y.magnifier` key gnome-shell 51.0's
//! `magnifier.js` reads into the session snapshot the compositor polls
//! (see the `tuna-magnifier` crate), now and on every change — the same
//! publish pattern as `tuna-wallpaper`, and no control-channel change.
//! The magnifier key bindings (`gsd-media-keys`' `magnifier`,
//! `magnifier-zoom-in` and `magnifier-zoom-out`) write the same keys
//! through [`toggle`] and [`zoom`].

use crate::gtk;
use gtk::prelude::*;
use std::rc::Rc;

const APPLICATIONS_SCHEMA: &str = "org.gnome.desktop.a11y.applications";
const MAGNIFIER_SCHEMA: &str = "org.gnome.desktop.a11y.magnifier";

/// One reading of the magnifier keys: GSettings values before enum
/// mapping, so the mapping itself stays pure and testable.
#[derive(Debug, Clone, Default)]
pub struct MagSnapshot {
    pub enabled: bool,
    pub mag_factor: f64,
    pub screen_position: i32,
    pub mouse_tracking: i32,
    pub focus_tracking: i32,
    pub caret_tracking: i32,
    pub scroll_at_edges: bool,
    pub lens_mode: bool,
    pub show_cross_hairs: bool,
    pub cross_hairs_thickness: i32,
    pub cross_hairs_color: String,
    pub cross_hairs_opacity: f64,
    pub cross_hairs_length: i32,
    pub cross_hairs_clip: bool,
    pub invert_lightness: bool,
    pub color_saturation: f64,
    pub brightness: [f64; 3],
    pub contrast: [f64; 3],
}

/// Map a raw snapshot to the compositor's config. Enum discriminants
/// follow `headers/gdesktop-enums.h` at 51.0; out-of-range values fail
/// to no tracking / full-screen, and every number sanitizes to its
/// schema range.
pub fn config_from_snapshot(snap: &MagSnapshot) -> tuna_magnifier::MagnifierConfig {
    tuna_magnifier::MagnifierConfig {
        enabled: snap.enabled,
        mag_factor: snap.mag_factor,
        screen_position: tuna_magnifier::ScreenPosition::from_gnome(snap.screen_position),
        mouse_tracking: tuna_magnifier::TrackingMode::from_gnome(snap.mouse_tracking),
        focus_tracking: tuna_magnifier::TrackingMode::from_gnome(snap.focus_tracking),
        caret_tracking: tuna_magnifier::TrackingMode::from_gnome(snap.caret_tracking),
        scroll_at_edges: snap.scroll_at_edges,
        lens_mode: snap.lens_mode,
        show_cross_hairs: snap.show_cross_hairs,
        cross_hairs_thickness: snap.cross_hairs_thickness,
        cross_hairs_color: snap.cross_hairs_color.clone(),
        cross_hairs_opacity: snap.cross_hairs_opacity,
        cross_hairs_length: snap.cross_hairs_length,
        cross_hairs_clip: snap.cross_hairs_clip,
        invert_lightness: snap.invert_lightness,
        color_saturation: snap.color_saturation,
        brightness: snap.brightness,
        contrast: snap.contrast,
    }
    .sanitized()
}

fn has_key(settings: &gio::Settings, key: &str) -> bool {
    settings.settings_schema().is_some_and(|s| s.has_key(key))
}

fn snapshot(apps: Option<&gio::Settings>, mag: Option<&gio::Settings>) -> MagSnapshot {
    let defaults = tuna_magnifier::MagnifierConfig::default();
    let mut snap = MagSnapshot {
        enabled: false,
        mag_factor: defaults.mag_factor,
        screen_position: 1,
        mouse_tracking: 2,
        focus_tracking: 2,
        caret_tracking: 1,
        scroll_at_edges: defaults.scroll_at_edges,
        lens_mode: defaults.lens_mode,
        show_cross_hairs: defaults.show_cross_hairs,
        cross_hairs_thickness: defaults.cross_hairs_thickness,
        cross_hairs_color: defaults.cross_hairs_color.clone(),
        cross_hairs_opacity: defaults.cross_hairs_opacity,
        cross_hairs_length: defaults.cross_hairs_length,
        cross_hairs_clip: defaults.cross_hairs_clip,
        invert_lightness: defaults.invert_lightness,
        color_saturation: defaults.color_saturation,
        brightness: defaults.brightness,
        contrast: defaults.contrast,
    };
    if let Some(apps) = apps.filter(|s| has_key(s, "screen-magnifier-enabled")) {
        snap.enabled = apps.boolean("screen-magnifier-enabled");
    }
    let Some(mag) = mag else {
        return snap;
    };
    let double = |key: &str, slot: &mut f64| {
        if has_key(mag, key) {
            *slot = mag.double(key);
        }
    };
    let boolean = |key: &str, slot: &mut bool| {
        if has_key(mag, key) {
            *slot = mag.boolean(key);
        }
    };
    let enumeration = |key: &str, slot: &mut i32| {
        if has_key(mag, key) {
            *slot = mag.enum_(key);
        }
    };
    let int = |key: &str, slot: &mut i32| {
        if has_key(mag, key) {
            *slot = mag.int(key);
        }
    };
    double("mag-factor", &mut snap.mag_factor);
    enumeration("screen-position", &mut snap.screen_position);
    enumeration("mouse-tracking", &mut snap.mouse_tracking);
    enumeration("focus-tracking", &mut snap.focus_tracking);
    enumeration("caret-tracking", &mut snap.caret_tracking);
    boolean("scroll-at-edges", &mut snap.scroll_at_edges);
    boolean("lens-mode", &mut snap.lens_mode);
    boolean("show-cross-hairs", &mut snap.show_cross_hairs);
    int("cross-hairs-thickness", &mut snap.cross_hairs_thickness);
    if has_key(mag, "cross-hairs-color") {
        snap.cross_hairs_color = mag.string("cross-hairs-color").to_string();
    }
    double("cross-hairs-opacity", &mut snap.cross_hairs_opacity);
    int("cross-hairs-length", &mut snap.cross_hairs_length);
    boolean("cross-hairs-clip", &mut snap.cross_hairs_clip);
    boolean("invert-lightness", &mut snap.invert_lightness);
    double("color-saturation", &mut snap.color_saturation);
    for (i, channel) in ["red", "green", "blue"].into_iter().enumerate() {
        double(&format!("brightness-{channel}"), &mut snap.brightness[i]);
        double(&format!("contrast-{channel}"), &mut snap.contrast[i]);
    }
    snap
}

/// Publish the magnifier snapshot now and on every key change. The
/// change handlers retain the sync closure (which retains the
/// settings), so the watch lives for the shell process lifetime.
pub fn start_sync() {
    let apps = crate::settings(APPLICATIONS_SCHEMA);
    let mag = crate::settings(MAGNIFIER_SCHEMA);
    if apps.is_none() && mag.is_none() {
        return;
    }
    let held = [apps.clone(), mag.clone()];
    let sync: Rc<dyn Fn()> = Rc::new(move || {
        let config = config_from_snapshot(&snapshot(apps.as_ref(), mag.as_ref()));
        if let Err(error) = tuna_magnifier::save(&config) {
            eprintln!("tuna-shell-gtk: magnifier snapshot: {error}");
        }
    });
    sync();
    for settings in held.into_iter().flatten() {
        let sync = sync.clone();
        settings.connect_changed(None, move |_, _| sync());
    }
}

/// `gsd-media-keys`' `magnifier` binding (`do_magnifier_action`):
/// flip `screen-magnifier-enabled`. The change handler above then
/// publishes the snapshot.
pub fn toggle() {
    let Some(apps) =
        crate::settings(APPLICATIONS_SCHEMA).filter(|s| has_key(s, "screen-magnifier-enabled"))
    else {
        return;
    };
    let enabled = apps.boolean("screen-magnifier-enabled");
    if let Err(error) = apps.set_boolean("screen-magnifier-enabled", !enabled) {
        eprintln!("tuna-shell-gtk: magnifier toggle: {error}");
    }
}

/// `gsd-media-keys`' `magnifier-zoom-in` / `-out` bindings
/// (`do_magnifier_zoom_action`): one step, rounded; a step past the
/// schema range is refused, as GSettings refuses it in GNOME.
pub fn zoom(zoom_in: bool) {
    let Some(mag) = crate::settings(MAGNIFIER_SCHEMA).filter(|s| has_key(s, "mag-factor")) else {
        return;
    };
    let Some(next) = tuna_magnifier::zoom_step(mag.double("mag-factor"), zoom_in) else {
        return;
    };
    if let Err(error) = mag.set_double("mag-factor", next) {
        eprintln!("tuna-shell-gtk: magnifier zoom: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_mapping_matches_gnome_51_enums_and_ranges() {
        let snap = MagSnapshot {
            enabled: true,
            mag_factor: 4.0,
            screen_position: 4,
            mouse_tracking: 3,
            focus_tracking: 0,
            caret_tracking: 1,
            scroll_at_edges: true,
            lens_mode: true,
            ..MagSnapshot::default()
        };
        let config = config_from_snapshot(&snap);
        assert!(config.enabled);
        assert_eq!(config.mag_factor, 4.0);
        assert_eq!(
            config.screen_position,
            tuna_magnifier::ScreenPosition::LeftHalf
        );
        assert_eq!(config.mouse_tracking, tuna_magnifier::TrackingMode::Push);
        assert_eq!(config.focus_tracking, tuna_magnifier::TrackingMode::None);
        assert_eq!(
            config.caret_tracking,
            tuna_magnifier::TrackingMode::Centered
        );
        assert!(config.scroll_at_edges);
        assert!(config.lens_mode);
    }

    #[test]
    fn cross_hair_and_color_effect_keys_pass_through_sanitized() {
        let snap = MagSnapshot {
            enabled: true,
            mag_factor: 2.0,
            show_cross_hairs: true,
            cross_hairs_thickness: 12,
            cross_hairs_color: "#00ff00".into(),
            cross_hairs_opacity: 0.5,
            cross_hairs_length: 1,
            cross_hairs_clip: true,
            invert_lightness: true,
            color_saturation: 0.25,
            brightness: [0.1, -0.2, 7.0],
            contrast: [-0.3, 0.0, 0.4],
            ..MagSnapshot::default()
        };
        let config = config_from_snapshot(&snap);
        assert!(config.show_cross_hairs);
        assert_eq!(config.cross_hairs_thickness, 12);
        assert_eq!(config.cross_hairs_rgba(), [0, 255, 0, 255]);
        assert_eq!(config.cross_hairs_opacity, 0.5);
        assert_eq!(
            config.cross_hairs_length,
            tuna_magnifier::MIN_CROSS_HAIRS_LENGTH
        );
        assert!(config.cross_hairs_clip);
        assert!(config.invert_lightness);
        assert_eq!(config.color_saturation, 0.25);
        assert_eq!(config.brightness, [0.1, -0.2, 1.0]);
        assert_eq!(config.contrast, [-0.3, 0.0, 0.4]);
    }

    #[test]
    fn out_of_range_snapshot_fails_safe() {
        let snap = MagSnapshot {
            enabled: true,
            mag_factor: f64::NAN,
            screen_position: 99,
            mouse_tracking: -1,
            focus_tracking: -1,
            caret_tracking: -1,
            ..MagSnapshot::default()
        };
        let config = config_from_snapshot(&snap);
        assert_eq!(config.mag_factor, tuna_magnifier::DEFAULT_MAG_FACTOR);
        assert_eq!(
            config.screen_position,
            tuna_magnifier::ScreenPosition::FullScreen
        );
        assert_eq!(config.mouse_tracking, tuna_magnifier::TrackingMode::None);
    }

    #[test]
    fn disabled_snapshot_stays_disabled_through_mapping() {
        let config = config_from_snapshot(&MagSnapshot::default());
        assert!(!config.enabled);
        assert!(!config.active());
    }
}
