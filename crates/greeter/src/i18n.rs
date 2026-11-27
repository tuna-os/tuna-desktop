//! The login screen's strings through gettext (#1164).
//!
//! Its words are GNOME Shell 51's login dialog's, so its msgids are
//! gnome-shell's and the translations come from gnome-shell's own
//! catalogs (`gnome-shell.mo`), which the image installs with GNOME. GTK
//! sets the locale from GDM's environment before the window is built.
//! The catalogs are bound from the first `XDG_DATA_DIRS` entry that has
//! any (GDM puts `/usr/share/gdm/greeter` first, then the system's), so a
//! catalog directory in front of the system's wins as for every other
//! data file the login screen reads.

use std::path::{Path, PathBuf};

use gtk4::glib;

/// gnome-shell's gettext domain.
const DOMAIN: &str = "gnome-shell";

/// Bind gnome-shell's catalogs; call once, before the first string.
pub fn init() {
    let dirs = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|dirs| !dirs.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".to_owned());
    if let Some(dir) = catalog_dir(dirs.split(':').map(Path::new)) {
        let _ = gettextrs::bindtextdomain(DOMAIN, dir);
    }
    let _ = gettextrs::bind_textdomain_codeset(DOMAIN, "UTF-8");
}

/// The first `<data dir>/locale` holding a `gnome-shell.mo` for any
/// language.
fn catalog_dir<'a>(data_dirs: impl Iterator<Item = &'a Path>) -> Option<PathBuf> {
    data_dirs.map(|dir| dir.join("locale")).find(|locale| {
        std::fs::read_dir(locale).is_ok_and(|languages| {
            languages.filter_map(Result::ok).any(|language| {
                language
                    .path()
                    .join("LC_MESSAGES")
                    .join(format!("{DOMAIN}.mo"))
                    .is_file()
            })
        })
    })
}

/// `_()`.
pub fn tr(msgid: &str) -> String {
    glib::dgettext(Some(DOMAIN), msgid).into()
}

/// `C_()`.
pub fn ctr(context: &str, msgid: &str) -> String {
    glib::dpgettext2(Some(DOMAIN), context, msgid).into()
}

/// `ngettext()`, with `%d` replaced by `n`.
pub fn ntr(msgid: &str, plural: &str, n: u32) -> String {
    glib::dngettext(Some(DOMAIN), msgid, plural, n.into()).replace("%d", &n.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogs_come_from_the_first_data_dir_that_has_them() {
        let empty = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(other.path().join("locale/de/LC_MESSAGES")).unwrap();
        std::fs::write(other.path().join("locale/de/LC_MESSAGES/gtk40.mo"), b"").unwrap();
        let shell = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(shell.path().join("locale/de/LC_MESSAGES")).unwrap();
        std::fs::write(
            shell.path().join("locale/de/LC_MESSAGES/gnome-shell.mo"),
            b"",
        )
        .unwrap();
        let dirs = [empty.path(), other.path(), shell.path()];
        assert_eq!(
            catalog_dir(dirs.into_iter()),
            Some(shell.path().join("locale"))
        );
        assert_eq!(catalog_dir([empty.path()].into_iter()), None);
    }

    #[test]
    fn untranslated_strings_are_the_msgids() {
        assert_eq!(tr("Not listed?"), "Not listed?");
        assert_eq!(ctr("title", "Power Off"), "Power Off");
        assert_eq!(
            ntr(
                "The system will power off automatically in %d second",
                "The system will power off automatically in %d seconds",
                60
            ),
            "The system will power off automatically in 60 seconds"
        );
    }
}
