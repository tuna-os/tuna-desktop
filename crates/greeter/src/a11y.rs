//! The login dialog's accessibility menu and what its toggles do at the
//! login screen (#1164).
//!
//! gnome-shell 51.1 `js/gdm/loginDialog.js` `A11yMenuButton`: a round
//! button in the bottom-right group with
//! `org.gnome.Settings-accessibility-symbolic`, named "Accessibility",
//! opening a two-column quick-settings menu of `status/accessibility.js`'s
//! toggles in this order: High Contrast, Zoom, Large Text, Screen Reader,
//! Screen Keyboard, Visual Alerts, Sticky Keys, Slow Keys, Bounce Keys,
//! Mouse Keys. Each is bound to its GSettings key (Large Text sets
//! `text-scaling-factor` to `DPI_FACTOR_LARGE` or resets it), so GDM's
//! greeter user keeps them across boots as in GNOME.
//!
//! GNOME's login screen runs inside Mutter, which follows those keys
//! itself; here each key reaches whoever draws it:
//! - Screen Reader: the compositor's trusted Orca (`SetScreenReader`).
//! - Zoom: the compositor's magnifier, through the session snapshot
//!   `tuna_magnifier::gsettings` publishes (as the shell does).
//! - Screen Keyboard: the prompt's own keyboard (`tuna_osk`'s
//!   `PromptKeyboard`), which edits only the prompt.
//! - Large Text: this process's font DPI (`gtk-xft-dpi`).
//! - High Contrast: the login screen's high-contrast style.
//! - Visual Alerts and the keyboard aids: the compositor, inside the
//!   greeter user's input settings (`input.rs`).

use std::rc::Rc;

use gio::prelude::*;
use gtk4 as gtk;
use gtk4::prelude::*;
use tuna_osk::prompt::PromptKeyboard;
use tuna_shell_control::gsettings::settings;

use crate::control::Control;
use crate::i18n::tr;

pub const APPLICATIONS_SCHEMA: &str = "org.gnome.desktop.a11y.applications";
const INTERFACE_SCHEMA: &str = "org.gnome.desktop.interface";
const A11Y_INTERFACE_SCHEMA: &str = "org.gnome.desktop.a11y.interface";
const KEYBOARD_SCHEMA: &str = "org.gnome.desktop.a11y.keyboard";
const WM_SCHEMA: &str = "org.gnome.desktop.wm.preferences";
const TEXT_SCALING_FACTOR: &str = "text-scaling-factor";
const SCREEN_READER: &str = "screen-reader-enabled";
/// accessibility.js `DPI_FACTOR_LARGE`.
pub const DPI_FACTOR_LARGE: f64 = 1.25;
/// loginDialog.js `N_A11Y_MENU_COLUMNS`.
const COLUMNS: i32 = 2;
/// GTK's DPI unit (`gtk-xft-dpi` is 1024 × dots per inch) at 96 dpi.
const XFT_DPI_96: f64 = 96.0 * 1024.0;

/// One toggle: its title (gnome-shell's msgid), icon, schema and key.
pub struct Toggle {
    pub title: &'static str,
    pub icon: &'static str,
    pub schema: &'static str,
    pub key: &'static str,
}

/// `A11yMenuButton`'s items, in its order.
pub const TOGGLES: [Toggle; 10] = [
    Toggle {
        title: "High Contrast",
        icon: "accessibility-high-contrast-symbolic",
        schema: A11Y_INTERFACE_SCHEMA,
        key: "high-contrast",
    },
    Toggle {
        title: "Zoom",
        icon: "accessibility-zoom-symbolic",
        schema: APPLICATIONS_SCHEMA,
        key: "screen-magnifier-enabled",
    },
    Toggle {
        title: "Large Text",
        icon: "accessibility-large-text-symbolic",
        schema: INTERFACE_SCHEMA,
        key: TEXT_SCALING_FACTOR,
    },
    Toggle {
        title: "Screen Reader",
        icon: "accessibility-screen-reader-symbolic",
        schema: APPLICATIONS_SCHEMA,
        key: SCREEN_READER,
    },
    Toggle {
        title: "Screen Keyboard",
        icon: "input-keyboard-symbolic",
        schema: APPLICATIONS_SCHEMA,
        key: "screen-keyboard-enabled",
    },
    Toggle {
        title: "Visual Alerts",
        icon: "accessibility-visual-alerts-symbolic",
        schema: WM_SCHEMA,
        key: "visual-bell",
    },
    Toggle {
        title: "Sticky Keys",
        icon: "accessibility-sticky-keys-symbolic",
        schema: KEYBOARD_SCHEMA,
        key: "stickykeys-enable",
    },
    Toggle {
        title: "Slow Keys",
        icon: "accessibility-slow-keys-symbolic",
        schema: KEYBOARD_SCHEMA,
        key: "slowkeys-enable",
    },
    Toggle {
        title: "Bounce Keys",
        icon: "accessibility-bounce-keys-symbolic",
        schema: KEYBOARD_SCHEMA,
        key: "bouncekeys-enable",
    },
    Toggle {
        title: "Mouse Keys",
        icon: "accessibility-mouse-keys-symbolic",
        schema: KEYBOARD_SCHEMA,
        key: "mousekeys-enable",
    },
];

fn has_key(settings: &gio::Settings, key: &str) -> bool {
    settings
        .settings_schema()
        .is_some_and(|schema| schema.has_key(key))
}

/// `schema`'s settings when it has `key`.
fn settings_with(schema: &str, key: &str) -> Option<gio::Settings> {
    settings(schema).filter(|s| has_key(s, key))
}

/// The Accessibility button and its menu.
pub fn menu_button() -> gtk::MenuButton {
    let grid = gtk::Grid::new();
    grid.add_css_class("quick-settings-grid");
    grid.set_row_spacing(12);
    grid.set_column_spacing(12);
    grid.set_column_homogeneous(true);
    for (index, toggle) in TOGGLES.iter().enumerate() {
        let index = i32::try_from(index).unwrap_or(0);
        grid.attach(
            &toggle_button(toggle),
            index % COLUMNS,
            index / COLUMNS,
            1,
            1,
        );
    }
    let popover = gtk::Popover::new();
    popover.add_css_class("quick-settings");
    popover.set_child(Some(&grid));
    let button = gtk::MenuButton::new();
    button.set_icon_name("org.gnome.Settings-accessibility-symbolic");
    button.add_css_class("login-dialog-button");
    button.add_css_class("a11y-button");
    let name = tr("Accessibility");
    button.set_tooltip_text(Some(&name));
    button.update_property(&[gtk::accessible::Property::Label(&name)]);
    button.set_popover(Some(&popover));
    button
}

/// One `QuickToggle`: icon and title, bound to its key.
fn toggle_button(toggle: &Toggle) -> gtk::ToggleButton {
    let title = tr(toggle.title);
    let icon = gtk::Image::from_icon_name(toggle.icon);
    let label = gtk::Label::new(Some(&title));
    label.set_xalign(0.0);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    content.append(&icon);
    content.append(&label);
    let button = gtk::ToggleButton::new();
    button.add_css_class("quick-toggle");
    button.set_child(Some(&content));
    button.update_property(&[gtk::accessible::Property::Label(&title)]);
    let Some(settings) = settings_with(toggle.schema, toggle.key) else {
        button.set_sensitive(false);
        return button;
    };
    if toggle.key == TEXT_SCALING_FACTOR {
        // LargeTextToggle: on above 1.0; on sets DPI_FACTOR_LARGE, off
        // resets the key.
        button.set_active(settings.double(TEXT_SCALING_FACTOR) > 1.0);
        button.set_sensitive(settings.is_writable(TEXT_SCALING_FACTOR));
        let target = settings.clone();
        button.connect_toggled(move |button| {
            let large = target.double(TEXT_SCALING_FACTOR) > 1.0;
            if button.is_active() == large {
                return;
            }
            if button.is_active() {
                let _ = target.set_double(TEXT_SCALING_FACTOR, DPI_FACTOR_LARGE);
            } else {
                target.reset(TEXT_SCALING_FACTOR);
            }
        });
        let weak = button.downgrade();
        settings.connect_changed(Some(TEXT_SCALING_FACTOR), move |s, key| {
            if let Some(button) = weak.upgrade() {
                button.set_active(s.double(key) > 1.0);
            }
        });
        // `target`, held by the button's handler, keeps the settings (and
        // this handler) alive with the button.
    } else {
        settings.bind(toggle.key, &button, "active").build();
    }
    button
}

/// Follow the keys GNOME's login screen follows in-process: now, and on
/// every change, for the life of the greeter.
pub fn follow(window: &gtk::Window, control: &Rc<Control>, keyboard: &Rc<PromptKeyboard>) {
    if let Some(apps) = settings(APPLICATIONS_SCHEMA) {
        let reader = |apps: &gio::Settings, control: &Control| {
            if has_key(apps, SCREEN_READER) {
                screen_reader(apps.boolean(SCREEN_READER), control);
            }
        };
        let osk = |apps: &gio::Settings, keyboard: &Rc<PromptKeyboard>| {
            if has_key(apps, "screen-keyboard-enabled") {
                let on = apps.boolean("screen-keyboard-enabled");
                crate::trace(&format!("greeter a11y screen-keyboard={on}"));
                keyboard.set_enabled(on);
            }
        };
        reader(&apps, control);
        osk(&apps, keyboard);
        let control = control.clone();
        let keyboard = keyboard.clone();
        apps.connect_changed(None, move |apps, key| match key {
            SCREEN_READER => reader(apps, &control),
            "screen-keyboard-enabled" => osk(apps, &keyboard),
            _ => {}
        });
        keep(apps);
    }
    if let Some(interface) = settings_with(INTERFACE_SCHEMA, TEXT_SCALING_FACTOR) {
        let apply = |interface: &gio::Settings| {
            let factor = interface.double(TEXT_SCALING_FACTOR);
            let dpi = xft_dpi(factor);
            crate::trace(&format!(
                "greeter a11y text-scaling-factor={factor} dpi={dpi}"
            ));
            if let Some(gtk_settings) = gtk::Settings::default() {
                gtk_settings.set_gtk_xft_dpi(dpi);
            }
        };
        apply(&interface);
        interface.connect_changed(Some(TEXT_SCALING_FACTOR), move |s, _| apply(s));
        keep(interface);
    }
    if let Some(a11y) = settings_with(A11Y_INTERFACE_SCHEMA, "high-contrast") {
        let apply = |a11y: &gio::Settings, window: &gtk::Window| {
            let on = a11y.boolean("high-contrast");
            crate::trace(&format!("greeter a11y high-contrast={on}"));
            crate::set_class(window, "high-contrast", on);
        };
        apply(&a11y, window);
        let weak = window.downgrade();
        a11y.connect_changed(Some("high-contrast"), move |s, _| {
            if let Some(window) = weak.upgrade() {
                apply(s, &window);
            }
        });
        keep(a11y);
    }
    tuna_magnifier::gsettings::start_sync("tuna-greeter");
}

/// `screen-reader-enabled` changed: the compositor's trusted Orca runs
/// for the login screen while it is on (GNOME's login screen starts Orca
/// from the same key).
fn screen_reader(on: bool, control: &Control) {
    crate::trace(&format!("greeter a11y screen-reader={on}"));
    control.set_screen_reader(on);
}

/// GTK's `gtk-xft-dpi` for a text scaling factor (Mutter scales the
/// login screen's fonts from 96 dpi by the same factor).
pub fn xft_dpi(factor: f64) -> i32 {
    let factor = if factor.is_finite() && factor > 0.0 {
        factor
    } else {
        1.0
    };
    // At most a few × 98304: far inside i32.
    (XFT_DPI_96 * factor).round() as i32
}

/// Keep `settings` and its change handlers for the process: the login
/// screen has one window for its whole life (as the shell keeps its
/// input settings).
fn keep(settings: gio::Settings) {
    std::mem::forget(settings);
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuna_shell_control::{CommandKind, Message};

    #[test]
    fn the_menu_has_gnomes_toggles_in_gnomes_order() {
        let titles: Vec<&str> = TOGGLES.iter().map(|t| t.title).collect();
        assert_eq!(
            titles,
            [
                "High Contrast",
                "Zoom",
                "Large Text",
                "Screen Reader",
                "Screen Keyboard",
                "Visual Alerts",
                "Sticky Keys",
                "Slow Keys",
                "Bounce Keys",
                "Mouse Keys",
            ]
        );
    }

    #[test]
    fn large_text_scales_the_font_dpi_from_96() {
        assert_eq!(xft_dpi(1.0), 98_304);
        assert_eq!(xft_dpi(DPI_FACTOR_LARGE), 122_880);
        assert_eq!(xft_dpi(f64::NAN), 98_304);
        assert_eq!(xft_dpi(0.0), 98_304);
    }

    #[test]
    fn the_screen_reader_toggle_starts_and_stops_the_compositors_orca() {
        let toggle = TOGGLES.iter().find(|t| t.title == "Screen Reader").unwrap();
        assert_eq!(
            (toggle.schema, toggle.key),
            (APPLICATIONS_SCHEMA, SCREEN_READER)
        );
        let (control, mut compositor) = crate::control::testing::pair();
        assert!(matches!(
            crate::control::testing::read_message(&mut compositor),
            Message::Hello { .. }
        ));
        for (id, on) in [(1, true), (2, false)] {
            screen_reader(on, &control);
            assert_eq!(
                crate::control::testing::read_message(&mut compositor),
                Message::Command {
                    id,
                    kind: CommandKind::SetScreenReader { enabled: on },
                }
            );
        }
    }
}
