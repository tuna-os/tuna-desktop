//! Bundle the login screen's own icons as a GResource, as GNOME Shell
//! bundles its icons, so the greeter shows GNOME's icons whatever the
//! system theme:
//! - the accessibility menu's toggles, GNOME Shell 51's own icons shared
//!   with the shell (`../shell-gtk/icons`, see its README);
//! - `icons/scalable/apps/org.gnome.Settings-accessibility-symbolic.svg`,
//!   the menu button's icon (`loginDialog.js` `A11yMenuButton`), copied
//!   unchanged from gnome-control-center 51.1
//!   (`panels/universal-access/icons/scalable/`,
//!   https://gitlab.gnome.org/GNOME/gnome-control-center,
//!   GPL-2.0-or-later). GNOME's login screen takes it from the installed
//!   Settings app; the greeter must not depend on Settings being there.

use std::fmt::Write as _;
use std::path::Path;

/// The icons the greeter draws that the system icon theme may lack.
const ICONS: [&str; 11] = [
    "scalable/apps/org.gnome.Settings-accessibility-symbolic.svg",
    "scalable/actions/accessibility-high-contrast-symbolic.svg",
    "scalable/actions/accessibility-zoom-symbolic.svg",
    "scalable/actions/accessibility-large-text-symbolic.svg",
    "scalable/actions/accessibility-screen-reader-symbolic.svg",
    "scalable/actions/accessibility-visual-alerts-symbolic.svg",
    "scalable/actions/accessibility-sticky-keys-symbolic.svg",
    "scalable/actions/accessibility-slow-keys-symbolic.svg",
    "scalable/actions/accessibility-bounce-keys-symbolic.svg",
    "scalable/actions/accessibility-mouse-keys-symbolic.svg",
    "scalable/actions/accessibility-menu-symbolic.svg",
];

fn main() {
    let own = Path::new("icons");
    let shell = Path::new("../shell-gtk/icons");
    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<gresources>\n  <gresource prefix=\"/org/tuna/Greeter/icons\">\n",
    );
    for icon in ICONS {
        let found = [own, shell]
            .into_iter()
            .map(|dir| dir.join(icon))
            .find(|path| path.is_file())
            .unwrap_or_else(|| panic!("icon {icon} is in neither icons dir"));
        println!("cargo:rerun-if-changed={}", found.display());
        let _ = writeln!(
            xml,
            "    <file preprocess=\"xml-stripblanks\">{icon}</file>"
        );
    }
    xml.push_str("  </gresource>\n</gresources>\n");
    let out = std::env::var("OUT_DIR").expect("OUT_DIR");
    let manifest = Path::new(&out).join("icons.gresource.xml");
    write_if_changed(&manifest, &xml);
    glib_build_tools::compile_resources(
        &[own, shell],
        manifest.to_str().expect("utf-8"),
        "icons.gresource",
    );
}

/// An unchanged manifest keeps its mtime, so no-op builds stay no-ops.
fn write_if_changed(path: &Path, contents: &str) {
    if std::fs::read_to_string(path).is_ok_and(|old| old == contents) {
        return;
    }
    std::fs::write(path, contents).expect("write manifest");
}
