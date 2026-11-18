//! GNOME Shell's welcome dialog ("Welcome to <OS>", Skip / Take Tour):
//! gnome-shell `js/ui/welcomeDialog.js`, opened at startup by
//! `main.js` `_handleShowWelcomeScreen` (#1196).
//!
//! The session shows it once: when `org.gnome.shell
//! welcome-dialog-last-shown-version` predates `40.beta`, the key is set to
//! the shell's version and the dialog opens, if `org.gnome.Tour.desktop`
//! (gnome-tour) is installed. Without the tour GNOME's `open()` does
//! nothing, but the key is still written, so installing the tour later does
//! not bring the dialog back.

use std::cmp::Ordering;
use std::path::PathBuf;
use std::rc::Rc;

use gio::prelude::*;
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use tuna_shell_host::apps::AppEntry;

/// `org.gnome.shell`'s key for the last version the dialog was shown for.
pub const KEY: &str = "welcome-dialog-last-shown-version";
/// `WELCOME_DIALOG_LAST_TOUR_CHANGE`: shown again for any version of the
/// tour older than this.
pub const LAST_TOUR_CHANGE: &str = "40.beta";
/// gnome-tour's desktop entry, as `WelcomeDialog._init` looks it up.
pub const TOUR_DESKTOP_ID: &str = "org.gnome.Tour.desktop";
/// Layer-shell namespace of the dialog's surface.
pub const NAMESPACE: &str = "tuna-welcome";
/// Dialog description (`_buildLayout`).
pub const DESCRIPTION: &str = "If you want to learn your way around, check out the tour";
/// The dialog's image is 300 px square (`.welcome-dialog-image`).
const IMAGE_SIZE: i32 = 300;

/// `_GNOMEversionToNumber`: numbers as `Number()` reads them, the
/// pre-release words ordered before every release, anything else NaN.
fn version_number(part: &str) -> Option<f64> {
    let part = if part.is_empty() { "0" } else { part };
    match part {
        "alpha" => return Some(-3.0),
        "beta" => return Some(-2.0),
        "rc" => return Some(-1.0),
        _ => {}
    }
    let part = part.trim();
    if part.is_empty() {
        return Some(0.0);
    }
    // `Number()` takes decimal and exponent forms, not Rust's `inf`/`nan`.
    if !part
        .chars()
        .all(|c| c.is_ascii_digit() || matches!(c, '+' | '-' | '.' | 'e' | 'E'))
    {
        return None;
    }
    part.parse().ok()
}

/// `Util.GNOMEversionCompare`: older, equal or newer, element by element
/// on dotted parts; a missing part reads 0 and an unreadable one (NaN)
/// compares equal to everything.
pub fn version_compare(version1: &str, version2: &str) -> Ordering {
    let (v1, v2): (Vec<&str>, Vec<&str>) =
        (version1.split('.').collect(), version2.split('.').collect());
    for i in 0..v1.len().max(v2.len()) {
        let a = version_number(v1.get(i).copied().unwrap_or("0"));
        let b = version_number(v2.get(i).copied().unwrap_or("0"));
        if let (Some(a), Some(b)) = (a, b) {
            if a < b {
                return Ordering::Less;
            }
            if a > b {
                return Ordering::Greater;
            }
        }
    }
    Ordering::Equal
}

/// `_handleShowWelcomeScreen`'s test: the dialog is due when the last
/// version it was shown for predates [`LAST_TOUR_CHANGE`].
pub fn due(last_shown: &str) -> bool {
    version_compare(LAST_TOUR_CHANGE, last_shown) == Ordering::Greater
}

/// One `KEY=value` of an os-release file, unquoted as the spec's shell
/// syntax allows.
fn os_release_value(contents: &str, key: &str) -> Option<String> {
    contents.lines().find_map(|line| {
        let value = line.trim().strip_prefix(key)?.strip_prefix('=')?;
        let value = value.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
            .unwrap_or(value);
        Some(value.replace("\\\"", "\"").replace("\\\\", "\\"))
    })
}

/// `_getOSName`: `PRETTY_NAME`, else `NAME` and `VERSION`, else GNOME
/// plus the major version.
pub fn os_name(os_release: &str, shell_version: &str) -> String {
    if let Some(pretty) = os_release_value(os_release, "PRETTY_NAME").filter(|v| !v.is_empty()) {
        return pretty;
    }
    if let Some(name) = os_release_value(os_release, "NAME").filter(|v| !v.is_empty()) {
        return match os_release_value(os_release, "VERSION").filter(|v| !v.is_empty()) {
            Some(version) => format!("{name} {version}"),
            None => name,
        };
    }
    let major = shell_version.split('.').next().unwrap_or(shell_version);
    format!("GNOME {major}")
}

/// The dialog's title.
pub fn title(os_name: &str) -> String {
    format!("Welcome to {os_name}")
}

fn os_release() -> String {
    ["/etc/os-release", "/usr/lib/os-release"]
        .iter()
        .find_map(|path| std::fs::read_to_string(path).ok())
        .unwrap_or_default()
}

/// `appSystem.lookup_app('org.gnome.Tour.desktop')`: the tour's entry
/// from any application directory, NoDisplay included.
pub fn tour_entry() -> Option<AppEntry> {
    tuna_shell_host::apps::default_app_dirs()
        .into_iter()
        .map(|dir| -> PathBuf { dir.join(TOUR_DESKTOP_ID) })
        .find(|path| path.is_file())
        .and_then(|path| tuna_shell_host::apps::handler_entry_from_file(&path))
}

/// `main.js` `_handleShowWelcomeScreen`: on a due session, record the
/// shell's version and open the dialog when the tour is installed.
/// `hide_overview` is the Take Tour button's `Main.overview.hide()`.
pub fn handle_startup(
    app: &gtk::Application,
    settings: Option<&gio::Settings>,
    shell_version: &str,
    hide_overview: Rc<dyn Fn()>,
) {
    let Some(settings) = settings else { return };
    // GNOME shows a privileged-user warning instead of the welcome.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    if settings
        .settings_schema()
        .is_none_or(|schema| !schema.has_key(KEY))
    {
        return;
    }
    if !due(settings.string(KEY).as_str()) {
        return;
    }
    // Open first, as GNOME does: the key is written whether or not the
    // dialog could open.
    if let Some(tour) = tour_entry() {
        WelcomeDialog::open(
            app,
            &tour,
            &title(&os_name(&os_release(), shell_version)),
            hide_overview,
        );
    }
    if let Err(error) = settings.set_string(KEY, shell_version) {
        eprintln!("tuna-shell-gtk: {KEY} not saved: {error}");
    }
}

/// The dialog: GNOME's ModalDialog with the OS logo, a title and
/// description, and Skip (Escape) and Take Tour buttons.
struct WelcomeDialog;

impl WelcomeDialog {
    fn open(app: &gtk::Application, tour: &AppEntry, title: &str, hide_overview: Rc<dyn Fn()>) {
        let window = gtk::Window::new();
        window.set_application(Some(app));
        window.set_title(Some("Welcome"));
        window.add_css_class("tuna-end-session");
        window.init_layer_shell();
        window.set_layer(Layer::Overlay);
        window.set_namespace(Some(NAMESPACE));
        for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
            window.set_anchor(edge, true);
        }
        window.set_exclusive_zone(-1);
        window.set_keyboard_mode(KeyboardMode::Exclusive);
        let card = gtk::Box::new(gtk::Orientation::Vertical, 18);
        card.add_css_class("modal-dialog");
        card.set_halign(gtk::Align::Center);
        card.set_valign(gtk::Align::Center);
        card.set_size_request(400, -1);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 18);
        content.add_css_class("modal-dialog-content-box");
        // `.welcome-dialog-image` leads the content.
        if let Some(icon) = logo_icon() {
            let image = gtk::Image::from_icon_name(&icon);
            image.add_css_class("welcome-dialog-image");
            image.set_pixel_size(IMAGE_SIZE);
            image.set_accessible_role(gtk::AccessibleRole::Presentation);
            content.append(&image);
        }
        let heading = gtk::Label::new(Some(title));
        heading.add_css_class("message-dialog-title");
        heading.set_wrap(true);
        let body = gtk::Label::new(Some(DESCRIPTION));
        body.add_css_class("message-dialog-description");
        body.set_wrap(true);
        body.set_max_width_chars(1);
        body.set_hexpand(true);
        body.set_justify(gtk::Justification::Center);
        content.append(&heading);
        content.append(&body);
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        buttons.add_css_class("modal-dialog-button-box");
        buttons.set_homogeneous(true);
        let skip = gtk::Button::with_label("Skip");
        let take_tour = gtk::Button::with_label("Take Tour");
        for button in [&skip, &take_tour] {
            button.add_css_class("modal-dialog-button");
            buttons.append(button);
        }
        card.append(&content);
        card.append(&buttons);
        window.set_child(Some(&card));
        {
            let window = window.clone();
            skip.connect_clicked(move |_| window.destroy());
        }
        {
            let (window, tour) = (window.clone(), tour.clone());
            take_tour.connect_clicked(move |_| {
                if let Err(error) = tuna_shell_host::apps::launch(&tour) {
                    eprintln!("tuna-shell-gtk: could not launch the tour: {error}");
                }
                hide_overview();
                window.destroy();
            });
        }
        let keys = gtk::EventControllerKey::new();
        {
            let window = window.clone();
            keys.connect_key_pressed(move |_, key, _, _| {
                if key != gtk::gdk::Key::Escape {
                    return glib::Propagation::Proceed;
                }
                window.destroy();
                glib::Propagation::Stop
            });
        }
        window.add_controller(keys);
        window.present();
        // The last button added is the default one: it holds the focus.
        take_tour.grab_focus();
    }
}

/// The OS logo `os-release` names, when the icon theme has it.
fn logo_icon() -> Option<String> {
    let icon = os_release_value(&os_release(), "LOGO").filter(|logo| !logo.is_empty())?;
    let display = gtk::gdk::Display::default()?;
    gtk::IconTheme::for_display(&display)
        .has_icon(&icon)
        .then_some(icon)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_like_gnome_shell() {
        // Util.GNOMEversionCompare: pre-release words sort before releases.
        assert_eq!(version_compare("40.beta", "40.0"), Ordering::Less);
        assert_eq!(version_compare("40.beta", "40.beta"), Ordering::Equal);
        assert_eq!(version_compare("40.rc", "40.beta"), Ordering::Greater);
        assert_eq!(version_compare("40.alpha", "40.beta"), Ordering::Less);
        assert_eq!(version_compare("51.1", "51"), Ordering::Greater);
        assert_eq!(version_compare("51.0", "51"), Ordering::Equal);
        assert_eq!(version_compare("9", "40"), Ordering::Less);
        // The empty string is the oldest possible version.
        assert_eq!(version_compare("40.beta", ""), Ordering::Greater);
        // NaN parts never order.
        assert_eq!(version_compare("40.beta", "40.x"), Ordering::Equal);
    }

    #[test]
    fn dialog_is_due_until_the_shell_has_shown_it() {
        // The key's schema default (oldest version) and GNOME 3 era values.
        assert!(due(""));
        assert!(due("3.38"));
        assert!(due("39.0"));
        // Written after showing: the shell's version, never due again.
        assert!(!due("51.0"));
        assert!(!due("40.beta"));
        assert!(!due("40.0"));
        // "A huge number disables the dialog" (the key's description).
        assert!(!due("99999"));
    }

    #[test]
    fn os_name_follows_os_info() {
        let pretty = "NAME=\"Tuna\"\nVERSION=\"1\"\nPRETTY_NAME=\"Tuna OS Marlin\"\n";
        assert_eq!(os_name(pretty, "51.0"), "Tuna OS Marlin");
        assert_eq!(
            os_name("NAME=Fedora\nVERSION=\"43 (Workstation)\"\n", "51.0"),
            "Fedora 43 (Workstation)"
        );
        assert_eq!(os_name("NAME='Arch Linux'\n", "51.0"), "Arch Linux");
        assert_eq!(os_name("", "51.1"), "GNOME 51");
        assert_eq!(title("Tuna OS Marlin"), "Welcome to Tuna OS Marlin");
    }
}
