//! The login screen's banner and logo, followed live (#1164), as
//! gnome-shell 51.1's `loginDialog.js` `_updateBanner` and `_updateLogo`
//! follow `org.gnome.login-screen`:
//!
//! - the banner shows while `banner-message-enable` is set: the text of
//!   `banner-message-text`, or with `banner-message-source` `file` the
//!   contents of `banner-message-path` (re-read when the file changes);
//!   nothing when that is empty or unreadable;
//! - the logo is the image at `logo`, at its natural size, at the bottom
//!   centre; none when the key is empty.

use std::cell::RefCell;
use std::rc::Rc;

use gio::prelude::*;
use gtk4 as gtk;
use gtk4::prelude::*;

const SCHEMA: &str = "org.gnome.login-screen";

/// The banner's text for the keys' values; `file` is the file's
/// contents when the source is a readable file.
pub fn banner_text(enabled: bool, source: &str, text: &str, file: Option<&str>) -> Option<String> {
    if !enabled {
        return None;
    }
    let text = if source == "file" { file? } else { text };
    Some(text.to_owned()).filter(|t| !t.is_empty())
}

/// The logo for the `logo` key: none when the key is empty.
pub fn logo_path(key: &str) -> Option<String> {
    Some(key.to_owned()).filter(|path| !path.is_empty())
}

pub struct Branding {
    pub banner: gtk::Label,
    pub logo: gtk::Picture,
    settings: Option<gio::Settings>,
    monitor: RefCell<Option<gio::FileMonitor>>,
}

impl Branding {
    pub fn new() -> Rc<Self> {
        let banner = gtk::Label::new(None);
        banner.add_css_class("login-dialog-banner");
        banner.set_wrap(true);
        banner.set_justify(gtk::Justification::Center);
        banner.set_visible(false);
        let logo = gtk::Picture::new();
        logo.add_css_class("login-dialog-logo-bin");
        logo.set_can_shrink(false);
        logo.set_halign(gtk::Align::Center);
        logo.set_valign(gtk::Align::End);
        logo.set_visible(false);
        let branding = Rc::new(Self {
            banner,
            logo,
            settings: tuna_shell_control::gsettings::settings(SCHEMA),
            monitor: RefCell::default(),
        });
        if let Some(settings) = &branding.settings {
            let weak = Rc::downgrade(&branding);
            settings.connect_changed(None, move |_, key| {
                let Some(branding) = weak.upgrade() else {
                    return;
                };
                match key {
                    "logo" => branding.update_logo(),
                    "banner-message-source" | "banner-message-path" => {
                        branding.update_banner_file();
                        branding.update_banner();
                    }
                    "banner-message-enable" | "banner-message-text" => branding.update_banner(),
                    _ => {}
                }
            });
        }
        branding.update_banner_file();
        branding.update_banner();
        branding.update_logo();
        branding
    }

    /// The banner file to read, when the source is `file`.
    fn banner_path(&self) -> Option<String> {
        let settings = self.settings.as_ref()?;
        (settings.string("banner-message-source") == "file")
            .then(|| settings.string("banner-message-path").to_string())
            .filter(|p| !p.is_empty())
    }

    /// `_updateBannerMessageFile`: watch the banner file.
    fn update_banner_file(self: &Rc<Self>) {
        self.monitor.borrow_mut().take();
        let Some(path) = self.banner_path() else {
            return;
        };
        let file = gio::File::for_path(&path);
        let Ok(monitor) = file.monitor_file(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE)
        else {
            return;
        };
        let weak = Rc::downgrade(self);
        monitor.connect_changed(move |_, _, _, _| {
            if let Some(branding) = weak.upgrade() {
                branding.update_banner();
            }
        });
        *self.monitor.borrow_mut() = Some(monitor);
    }

    fn update_banner(&self) {
        let Some(settings) = &self.settings else {
            return;
        };
        let file = self
            .banner_path()
            .and_then(|path| match std::fs::read_to_string(&path) {
                Ok(text) => Some(text),
                Err(error) => {
                    eprintln!("tuna-greeter: Failed to read banner from {path}: {error}");
                    None
                }
            });
        let text = banner_text(
            settings.boolean("banner-message-enable"),
            &settings.string("banner-message-source"),
            &settings.string("banner-message-text"),
            file.as_deref(),
        );
        crate::trace(&format!("greeter banner shown={}", text.is_some()));
        self.banner.set_label(text.as_deref().unwrap_or(""));
        self.banner.set_visible(text.is_some());
    }

    fn update_logo(&self) {
        let path = self
            .settings
            .as_ref()
            .and_then(|s| logo_path(&s.string("logo")));
        crate::trace(&format!(
            "greeter logo path={}",
            path.as_deref().unwrap_or("")
        ));
        match path {
            Some(path) => {
                self.logo.set_filename(Some(&path));
                self.logo.set_visible(true);
            }
            None => {
                self.logo.set_paintable(None::<&gtk::gdk::Paintable>);
                self.logo.set_visible(false);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_banner_shows_gnomes_text_or_file() {
        assert_eq!(banner_text(false, "settings", "Hi", None), None);
        assert_eq!(
            banner_text(true, "settings", "Hi", Some("file")),
            Some("Hi".to_owned())
        );
        assert_eq!(banner_text(true, "settings", "", None), None);
        assert_eq!(
            banner_text(true, "file", "Hi", Some("From file")),
            Some("From file".to_owned())
        );
        assert_eq!(banner_text(true, "file", "Hi", None), None);
    }

    #[test]
    fn the_logo_shows_only_when_named() {
        assert_eq!(logo_path(""), None);
        assert_eq!(
            logo_path("/usr/share/pixmaps/logo.png").as_deref(),
            Some("/usr/share/pixmaps/logo.png")
        );
    }
}
