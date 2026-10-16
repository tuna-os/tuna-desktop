//! The shell never goes through xdg-desktop-portal. It is part of the
//! desktop the portal's GNOME backend serves (that backend calls
//! `org.gnome.Shell`), and it starts before graphical-session.target,
//! which xdg-desktop-portal-gnome requires. GTK 4.17+ and libadwaita
//! read settings through the portal synchronously while the application
//! starts (GTK 4.17+ also checks the portal versions first). On
//! dbus-daemon that activation hangs for the whole D-Bus timeout (25 s
//! per call) instead of failing fast as on dbus-broker, and the main loop
//! stays blocked, so the shell cannot own its bus name. Without the
//! portal both read the same GSettings directly.
//!
//! The opt-out uses environment variables only, because
//! `gtk_disable_portals` needs GTK 4.18. The variables are set before GTK
//! starts. Once GTK and libadwaita have read them, the caller's values are
//! put back, so apps the shell launches inherit the user's environment.

use std::ffi::{OsStr, OsString};

const GDK_DEBUG: &str = "GDK_DEBUG";
const ADW_DISABLE_PORTAL: &str = "ADW_DISABLE_PORTAL";

/// The caller's values, restored by [`Saved::restore`].
pub struct Saved {
    gdk_debug: Option<OsString>,
    adw_disable_portal: Option<OsString>,
}

/// Call before `gtk::init`, while the process is single-threaded.
pub fn disable() -> Saved {
    let saved = Saved {
        gdk_debug: std::env::var_os(GDK_DEBUG),
        adw_disable_portal: std::env::var_os(ADW_DISABLE_PORTAL),
    };
    std::env::set_var(GDK_DEBUG, with_no_portals(saved.gdk_debug.as_deref()));
    std::env::set_var(ADW_DISABLE_PORTAL, "1");
    saved
}

impl Saved {
    /// Call from `startup`, after GTK (display open) and libadwaita
    /// (style manager) initialised.
    pub fn restore(self) {
        for (key, value) in [
            (GDK_DEBUG, self.gdk_debug),
            (ADW_DISABLE_PORTAL, self.adw_disable_portal),
        ] {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

/// `GDK_DEBUG` with `no-portals` added to any flags the user set.
fn with_no_portals(existing: Option<&OsStr>) -> OsString {
    match existing.map(OsStr::to_string_lossy) {
        Some(flags) if !flags.trim().is_empty() => format!("{flags},no-portals").into(),
        _ => "no-portals".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_portals_joins_the_users_gdk_debug_flags() {
        assert_eq!(with_no_portals(None), "no-portals");
        assert_eq!(with_no_portals(Some(OsStr::new(""))), "no-portals");
        assert_eq!(
            with_no_portals(Some(OsStr::new("events:dnd"))),
            "events:dnd,no-portals"
        );
    }
}
