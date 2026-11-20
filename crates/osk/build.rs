//! Bundle GNOME Shell's on-screen keyboard layouts and emoji list
//! (osk-layouts/README.md) as GNOME's gnome-shell-osk-layouts.gresource does.

use std::fmt::Write as _;
use std::path::Path;

fn main() {
    let root = Path::new("osk-layouts");
    println!("cargo:rerun-if-changed={}", root.display());
    let mut names: Vec<_> = std::fs::read_dir(root)
        .expect("osk-layouts dir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".json"))
        .collect();
    names.sort();
    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<gresources>\n  <gresource prefix=\"/org/tuna/Shell/osk-layouts\">\n",
    );
    for name in &names {
        let _ = writeln!(xml, "    <file>{name}</file>");
    }
    xml.push_str("  </gresource>\n</gresources>\n");
    let out = std::env::var("OUT_DIR").expect("OUT_DIR");
    let manifest = Path::new(&out).join("osk-layouts.gresource.xml");
    write_if_changed(&manifest, &xml);
    glib_build_tools::compile_resources(
        &[root],
        manifest.to_str().expect("utf-8"),
        "osk-layouts.gresource",
    );
}

/// Cargo reruns this script and rebuilds the crate when a file it compiles
/// from is newer than the last build, so an unchanged manifest keeps its
/// mtime: no-op builds stay no-ops and don't replace a running binary.
fn write_if_changed(path: &Path, contents: &str) {
    if std::fs::read_to_string(path).is_ok_and(|old| old == contents) {
        return;
    }
    std::fs::write(path, contents).expect("write manifest");
}
