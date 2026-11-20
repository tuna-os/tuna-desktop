//! Screen magnifier Settings sync (#349).
//!
//! Mirrors `screen-magnifier-enabled` plus every
//! `org.gnome.desktop.a11y.magnifier` key gnome-shell 51.0's
//! `magnifier.js` reads into the session snapshot the compositor polls
//! (`tuna_magnifier::gsettings`, shared with the login screen), now and
//! on every change — the same publish pattern as `tuna-wallpaper`, and no
//! control-channel change. The magnifier key bindings (`gsd-media-keys`'
//! `magnifier`, `magnifier-zoom-in` and `magnifier-zoom-out`) write the
//! same keys through [`toggle`] and [`zoom`].

use gio::prelude::*;
use tuna_magnifier::gsettings::{has_key, APPLICATIONS_SCHEMA, MAGNIFIER_SCHEMA};

/// Publish the magnifier snapshot now and on every key change, for the
/// shell process lifetime.
pub fn start_sync() {
    tuna_magnifier::gsettings::start_sync("tuna-shell-gtk");
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
