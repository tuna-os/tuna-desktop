//! GNOME 51's background menu (`js/ui/backgroundMenu.js`, wired by
//! `layout.js` `_addBackgroundMenu`): a right click, or a held primary
//! press, on the bare wallpaper opens "Change Background…", a separator,
//! "Display Settings" and "Settings". Each row is
//! `PopupMenu.addSettingsAction`: it opens that Settings panel
//! (`gnome-background-panel.desktop`, `gnome-display-panel.desktop`,
//! `org.gnome.Settings.desktop`), which the overview and quick settings
//! close first in GNOME; here the menu closes and Settings opens.
//!
//! The compositor recognizes the gestures
//! (`crates/compositor/src/background_menu.rs`, wire 0.54
//! `BackgroundMenu`) because the wallpaper is its own, not a shell
//! surface; the shell draws the menu with the window menu's popup
//! ([`crate::window_menu::WindowMenu::show`]), opened with BoxPointer's
//! full fade and slide, 7px under the pointer.

use std::rc::Rc;

use tuna_shell_host::control::BackgroundMenuRequest;

use crate::window_menu::Entry;

pub const CHANGE_BACKGROUND: &str = "Change Background…";
pub const DISPLAY_SETTINGS: &str = "Display Settings";
pub const SETTINGS: &str = "Settings";

/// The Settings panel a row opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    /// `gnome-background-panel.desktop`.
    Background,
    /// `gnome-display-panel.desktop`.
    Display,
    /// `org.gnome.Settings.desktop`: the Settings app itself.
    Settings,
}

impl Panel {
    /// The arguments of `gnome-control-center` that show this panel.
    pub fn arguments(self) -> &'static [&'static str] {
        match self {
            Self::Background => &["background"],
            Self::Display => &["display"],
            Self::Settings => &[],
        }
    }
}

/// The menu's rows in GNOME's order (`None`: the separator), each with
/// the panel it opens.
pub fn rows() -> [Option<(&'static str, Panel)>; 4] {
    [
        Some((CHANGE_BACKGROUND, Panel::Background)),
        None,
        Some((DISPLAY_SETTINGS, Panel::Display)),
        Some((SETTINGS, Panel::Settings)),
    ]
}

/// The popup's entries; a chosen row calls `open` with its panel.
pub fn entries(open: Rc<dyn Fn(Panel)>) -> Vec<Entry> {
    rows()
        .into_iter()
        .map(|row| match row {
            None => Entry::Separator,
            Some((label, panel)) => {
                let open = open.clone();
                Entry::Item {
                    label,
                    checked: false,
                    action: Some(Rc::new(move || open(panel))),
                }
            }
        })
        .collect()
}

/// Open `panel` in GNOME Settings, as the shell's other Settings links
/// do (`gnome-control-center <panel>` in the session's launch
/// environment).
pub fn open_panel(panel: Panel) {
    if let Err(error) = tuna_shell_host::apps::application_command("gnome-control-center")
        .args(panel.arguments())
        .spawn()
    {
        eprintln!("tuna-shell-gtk: background menu: {panel:?}: {error}");
    }
}

/// Show the menu where the compositor says, on its output.
pub fn show(
    menu: &Rc<crate::window_menu::WindowMenu>,
    request: &BackgroundMenuRequest,
    open: Rc<dyn Fn(Panel)>,
) {
    let monitor = request
        .connector
        .as_deref()
        .and_then(crate::pad_osd::monitor_for_connector);
    menu.show(entries(open), monitor, request.x, request.y, false);
}

#[cfg(test)]
mod tests {
    use super::*;
    use gtk4 as gtk;
    use gtk4::prelude::*;
    use std::cell::RefCell;

    #[test]
    fn rows_follow_gnome_51() {
        let labels: Vec<&str> = rows()
            .iter()
            .map(|row| row.map_or("", |(label, _)| label))
            .collect();
        assert_eq!(
            labels,
            ["Change Background…", "", "Display Settings", "Settings"]
        );
    }

    #[test]
    fn rows_open_the_background_display_and_settings_panels() {
        let opened = Rc::new(RefCell::new(Vec::new()));
        let log = opened.clone();
        let entries = entries(Rc::new(move |panel| log.borrow_mut().push(panel)));
        assert_eq!(entries.len(), 4);
        assert!(matches!(entries[1], Entry::Separator));
        for entry in &entries {
            if let Entry::Item {
                action, checked, ..
            } = entry
            {
                assert!(!checked);
                action.as_ref().expect("every row is sensitive")();
            }
        }
        assert_eq!(
            *opened.borrow(),
            [Panel::Background, Panel::Display, Panel::Settings]
        );
        assert_eq!(Panel::Background.arguments(), ["background"]);
        assert_eq!(Panel::Display.arguments(), ["display"]);
        assert!(Panel::Settings.arguments().is_empty());
    }

    fn descendants(root: &gtk::Widget, out: &mut Vec<gtk::Widget>) {
        let mut child = root.first_child();
        while let Some(widget) = child {
            out.push(widget.clone());
            descendants(&widget, out);
            child = widget.next_sibling();
        }
    }

    /// The labels of the menu's rows and how many separators it draws.
    fn drawn(menu: &crate::window_menu::WindowMenu) -> (Vec<(String, gtk::Button)>, usize) {
        let mut all = Vec::new();
        descendants(menu.window().upcast_ref(), &mut all);
        let buttons = all
            .iter()
            .filter_map(|w| w.clone().downcast::<gtk::Button>().ok())
            .map(|button| {
                let mut inside = Vec::new();
                descendants(button.upcast_ref(), &mut inside);
                let label = inside
                    .iter()
                    .find_map(|w| w.clone().downcast::<gtk::Label>().ok())
                    .map(|l| l.text().to_string())
                    .unwrap_or_default();
                (label, button)
            })
            .collect();
        let separators = all
            .iter()
            .filter(|w| w.has_css_class("popup-separator-menu-item"))
            .count();
        (buttons, separators)
    }

    fn pump_until(limit: std::time::Duration, mut done: impl FnMut() -> bool) -> bool {
        let start = std::time::Instant::now();
        let context = gtk::glib::MainContext::default();
        while start.elapsed() < limit {
            while context.iteration(false) {}
            if done() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        false
    }

    /// The shell draws the compositor's request as GNOME's menu: its
    /// three rows and one separator, each row opening its panel and
    /// closing the menu.
    #[test]
    #[ignore = "requires owned Xvfb/private D-Bus and explicit background menu admission"]
    fn actual_gtk_background_menu_lists_gnomes_rows_and_opens_their_panels() {
        assert_eq!(
            std::env::var("TUNA_REQUIRE_BACKGROUND_MENU").as_deref(),
            Ok("1")
        );
        gtk::init().expect("actual GTK display required");
        let app = gtk::Application::builder()
            .application_id("org.tuna.BackgroundMenuProof")
            .build();
        app.register(gtk::gio::Cancellable::NONE).unwrap();
        let menu = crate::window_menu::WindowMenu::new(&app, Rc::new(|_, _| {}));
        let opened = Rc::new(RefCell::new(Vec::new()));
        let request = BackgroundMenuRequest {
            connector: None,
            x: 100,
            y: 120,
        };
        for (row, panel) in [
            (CHANGE_BACKGROUND, Panel::Background),
            (DISPLAY_SETTINGS, Panel::Display),
            (SETTINGS, Panel::Settings),
        ] {
            let log = opened.clone();
            show(
                &menu,
                &request,
                Rc::new(move |panel| log.borrow_mut().push(panel)),
            );
            assert!(menu.window().is_visible(), "the menu shows");
            let (buttons, separators) = drawn(&menu);
            let labels: Vec<&str> = buttons.iter().map(|(l, _)| l.as_str()).collect();
            assert_eq!(labels, [CHANGE_BACKGROUND, DISPLAY_SETTINGS, SETTINGS]);
            assert_eq!(separators, 1);
            buttons
                .iter()
                .find(|(label, _)| label == row)
                .unwrap()
                .1
                .emit_clicked();
            assert_eq!(opened.borrow().last(), Some(&panel));
            assert!(
                pump_until(std::time::Duration::from_secs(2), || !menu
                    .window()
                    .is_visible()),
                "choosing {row} closes the menu"
            );
        }
        assert_eq!(opened.borrow().len(), 3);
        println!("BACKGROUND-MENU pass three rows, one separator, each opens its panel and closes");
    }
}
