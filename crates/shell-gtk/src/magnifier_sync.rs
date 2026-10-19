//! Screen magnifier Settings sync (#349).
//!
//! Mirrors `screen-magnifier-enabled` plus the honored
//! `org.gnome.desktop.a11y.magnifier` keys into the session snapshot
//! the compositor polls (see the `tuna-magnifier` crate), now and on
//! every change — the same publish pattern as `tuna-wallpaper`, and no
//! control-channel change. Cross-hairs and color-filter keys stay
//! unconsumed in this slice.

use crate::gtk;
use gtk::prelude::*;
use std::rc::Rc;

const APPLICATIONS_SCHEMA: &str = "org.gnome.desktop.a11y.applications";
const MAGNIFIER_SCHEMA: &str = "org.gnome.desktop.a11y.magnifier";

/// One reading of the honored magnifier keys: GSettings values before
/// enum mapping, so the mapping itself stays pure and testable.
#[derive(Debug, Clone, Copy, Default)]
pub struct MagSnapshot {
    pub enabled: bool,
    pub mag_factor: f64,
    pub screen_position: i32,
    pub mouse_tracking: i32,
    pub focus_tracking: i32,
    pub caret_tracking: i32,
    pub scroll_at_edges: bool,
    pub lens_mode: bool,
}

/// Map a raw snapshot to the compositor's config. Enum discriminants
/// follow `headers/gdesktop-enums.h` at 51.0; out-of-range values fail
/// to no tracking / full-screen, and the factor sanitizes to the
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
        scroll_at_edges: false,
        lens_mode: false,
    };
    if let Some(apps) = apps.filter(|s| has_key(s, "screen-magnifier-enabled")) {
        snap.enabled = apps.boolean("screen-magnifier-enabled");
    }
    if let Some(mag) = mag {
        if has_key(mag, "mag-factor") {
            snap.mag_factor = mag.double("mag-factor");
        }
        if has_key(mag, "screen-position") {
            snap.screen_position = mag.enum_("screen-position");
        }
        if has_key(mag, "mouse-tracking") {
            snap.mouse_tracking = mag.enum_("mouse-tracking");
        }
        if has_key(mag, "focus-tracking") {
            snap.focus_tracking = mag.enum_("focus-tracking");
        }
        if has_key(mag, "caret-tracking") {
            snap.caret_tracking = mag.enum_("caret-tracking");
        }
        if has_key(mag, "scroll-at-edges") {
            snap.scroll_at_edges = mag.boolean("scroll-at-edges");
        }
        if has_key(mag, "lens-mode") {
            snap.lens_mode = mag.boolean("lens-mode");
        }
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
