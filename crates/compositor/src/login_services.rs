//! What the compositor serves at the login screen (`--greeter`, #1164,
//! docs/greeter.md "Services at the login screen").
//!
//! GNOME 51's login screen is `gnome-shell --mode=gdm` on Mutter. Mutter
//! owns its session-bus services in every mode (mutter 51.1
//! `meta-backend.c`, `meta-context.c`), but gnome-shell inhibits remote
//! access in a mode without `allowScreencast` (`js/ui/main.js`
//! `_sessionUpdated`, the `gdm` mode has none), so ScreenCast,
//! RemoteDesktop and InputCapture refuse every session there. gnome-shell
//! keeps its sender-checked Introspect and Screenshot services.
//! gnome-session (`--session gnome-login`) owns `org.gnome.SessionManager`,
//! and GDM's login session starts no colour plugin, so nothing owns
//! `org.gnome.SettingsDaemon.Color`. The login screen's input sources are
//! localed's XKB layouts (`keyboard.js` `InputSourceSystemSettings`), so
//! no IBus engine is ever active there.
//!
//! [`Service::runs`] is that table: a user session runs everything; the
//! login screen runs only what GNOME's has, and refuses the rest by not
//! serving it at all (no bus name and no object), which is stricter than
//! Mutter's inhibited sessions.

/// One service (or helper process) of the compositor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
    DisplayConfig,
    IdleMonitor,
    /// `org.freedesktop.a11y.Manager` (Mutter's `MetaA11yManager`), the
    /// screen reader's key events.
    KeyboardMonitor,
    ServiceChannel,
    ColorManager,
    Introspect,
    Screenshot,
    ScreenCast,
    RemoteDesktop,
    InputCapture,
    /// gnome-settings-daemon's colour plugin name (Night Light).
    SettingsDaemonColor,
    SessionManager,
    /// `tuna-ibus-bridge` and the IBus daemon it starts.
    IbusBridge,
}

impl Service {
    pub const ALL: [Service; 13] = [
        Service::DisplayConfig,
        Service::IdleMonitor,
        Service::KeyboardMonitor,
        Service::ServiceChannel,
        Service::ColorManager,
        Service::Introspect,
        Service::Screenshot,
        Service::ScreenCast,
        Service::RemoteDesktop,
        Service::InputCapture,
        Service::SettingsDaemonColor,
        Service::SessionManager,
        Service::IbusBridge,
    ];

    /// The bus name it owns, or the helper's name.
    pub fn name(self) -> &'static str {
        match self {
            Service::DisplayConfig => "org.gnome.Mutter.DisplayConfig",
            Service::IdleMonitor => "org.gnome.Mutter.IdleMonitor",
            Service::KeyboardMonitor => "org.freedesktop.a11y.Manager",
            Service::ServiceChannel => "org.gnome.Mutter.ServiceChannel",
            Service::ColorManager => "org.gnome.Mutter.ColorManager",
            Service::Introspect => "org.gnome.Shell.Introspect",
            Service::Screenshot => "org.gnome.Shell.Screenshot",
            Service::ScreenCast => "org.gnome.Mutter.ScreenCast",
            Service::RemoteDesktop => "org.gnome.Mutter.RemoteDesktop",
            Service::InputCapture => "org.gnome.Mutter.InputCapture",
            Service::SettingsDaemonColor => "org.gnome.SettingsDaemon.Color",
            Service::SessionManager => "org.gnome.SessionManager",
            Service::IbusBridge => "tuna-ibus-bridge",
        }
    }

    /// Whether a user session (`greeter == false`) or the login screen
    /// runs it.
    pub fn runs(self, greeter: bool) -> bool {
        !greeter || self.refusal().is_none()
    }

    /// Why the login screen does not run it.
    fn refusal(self) -> Option<&'static str> {
        match self {
            Service::DisplayConfig
            | Service::IdleMonitor
            | Service::KeyboardMonitor
            | Service::ServiceChannel
            | Service::ColorManager
            | Service::Introspect
            | Service::Screenshot => None,
            Service::ScreenCast | Service::RemoteDesktop | Service::InputCapture => {
                Some("remote access is inhibited in the gdm mode")
            }
            Service::SettingsDaemonColor => Some("GDM's login session runs no colour plugin"),
            Service::SessionManager => Some("gnome-session owns it"),
            Service::IbusBridge => Some("the login screen's input sources are XKB layouts"),
        }
    }
}

/// Say, once at startup, what the login screen does not serve.
pub fn report(greeter: bool) {
    if !greeter {
        return;
    }
    for service in Service::ALL {
        if let Some(reason) = service.refusal() {
            eprintln!(
                "tuna-compositor: greeter: not serving {} ({reason})",
                service.name()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_user_session_runs_every_service() {
        assert!(Service::ALL.iter().all(|s| s.runs(false)));
    }

    #[test]
    fn the_login_screen_runs_what_gnomes_gdm_mode_has() {
        let running: Vec<&str> = Service::ALL
            .iter()
            .filter(|s| s.runs(true))
            .map(|s| s.name())
            .collect();
        assert_eq!(
            running,
            [
                "org.gnome.Mutter.DisplayConfig",
                "org.gnome.Mutter.IdleMonitor",
                "org.freedesktop.a11y.Manager",
                "org.gnome.Mutter.ServiceChannel",
                "org.gnome.Mutter.ColorManager",
                "org.gnome.Shell.Introspect",
                "org.gnome.Shell.Screenshot",
            ]
        );
        for refused in [
            Service::ScreenCast,
            Service::RemoteDesktop,
            Service::InputCapture,
            Service::SettingsDaemonColor,
            Service::SessionManager,
            Service::IbusBridge,
        ] {
            assert!(!refused.runs(true), "{refused:?}");
        }
    }
}
