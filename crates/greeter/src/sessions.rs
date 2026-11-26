//! The sessions the login screen offers, as libgdm's `gdm_get_session_ids`
//! finds them (GDM 51.1.1 `libgdm/gdm-sessions.c`): `wayland-sessions`
//! (and `xsessions` when X11 is supported) under every system data
//! directory, later directories first so earlier ones win for the same id;
//! files without a `Name`, hidden, `NoDisplay`, or whose `TryExec` is not
//! on `PATH` are left out, as are later files repeating a name. The
//! session type list is GDM's `GDM_SUPPORTED_SESSION_TYPES`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    /// The desktop file's name without `.desktop` (`SelectSession`'s id).
    pub id: String,
    /// `Name`, as the menu shows it.
    pub name: String,
}

/// Where to look: `xsessions` then `wayland-sessions` under each data dir,
/// per supported type (no list means both, as in libgdm).
pub fn search_dirs(data_dirs: &[PathBuf], supported_types: Option<&str>) -> Vec<PathBuf> {
    let supports =
        |kind: &str| supported_types.is_none_or(|types| types.split(':').any(|t| t == kind));
    let mut dirs = Vec::new();
    if supports("x11") {
        dirs.extend(data_dirs.iter().map(|d| d.join("xsessions")));
    }
    if supports("wayland") {
        dirs.extend(data_dirs.iter().map(|d| d.join("wayland-sessions")));
    }
    dirs
}

/// The offered sessions, sorted by name as the login dialog sorts them.
pub fn list(dirs: &[PathBuf], path_var: Option<&std::ffi::OsStr>) -> Vec<Session> {
    let mut by_id: BTreeMap<String, Session> = BTreeMap::new();
    // libgdm walks the directories last to first, so the first wins.
    for dir in dirs.iter().rev() {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut files: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        files.sort();
        for file in files {
            let Some(id) = file
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_suffix(".desktop"))
            else {
                continue;
            };
            match load(&file, path_var) {
                Some(name) => {
                    by_id.insert(
                        id.to_owned(),
                        Session {
                            id: id.to_owned(),
                            name,
                        },
                    );
                }
                None => {
                    by_id.remove(id);
                }
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    let mut sessions: Vec<Session> = by_id
        .into_values()
        .filter(|s| seen.insert(s.name.clone()))
        .collect();
    sessions.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
    sessions
}

/// The `Name` of a relevant session file.
fn load(path: &Path, path_var: Option<&std::ffi::OsStr>) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut in_entry = false;
    let mut fields: BTreeMap<&str, &str> = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            fields.entry(key.trim()).or_insert(value.trim());
        }
    }
    let truthy = |key| fields.get(key).is_some_and(|v| *v == "true");
    if truthy("NoDisplay") || truthy("Hidden") {
        return None;
    }
    if let Some(tryexec) = fields.get("TryExec") {
        if !on_path(tryexec, path_var) {
            return None;
        }
    }
    fields.get("Name").map(|n| (*n).to_owned())
}

fn on_path(program: &str, path_var: Option<&std::ffi::OsStr>) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let executable = |p: &Path| {
        std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    };
    if program.contains('/') {
        return executable(Path::new(program));
    }
    path_var
        .map(std::env::split_paths)
        .into_iter()
        .flatten()
        .any(|dir| executable(&dir.join(program)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn write(dir: &Path, name: &str, body: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(name), body).unwrap();
    }

    #[test]
    fn types_choose_the_directories() {
        let dirs = [PathBuf::from("/a"), PathBuf::from("/b")];
        assert_eq!(
            search_dirs(&dirs, Some("wayland")),
            vec![
                PathBuf::from("/a/wayland-sessions"),
                PathBuf::from("/b/wayland-sessions")
            ]
        );
        assert_eq!(search_dirs(&dirs, Some("wayland:x11")).len(), 4);
        assert_eq!(search_dirs(&dirs, None)[0], PathBuf::from("/a/xsessions"));
    }

    #[test]
    fn hidden_untryable_nameless_and_duplicate_sessions_are_left_out() {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let tool = bin.join("tuna-session");
        std::fs::write(&tool, "").unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        let first = root.path().join("first/wayland-sessions");
        let second = root.path().join("second/wayland-sessions");
        write(
            &first,
            "tuna.desktop",
            "[Desktop Entry]\nName=Tuna Desktop\nExec=tuna-session\nTryExec=tuna-session\n",
        );
        write(
            &first,
            "gnome.desktop",
            "[Desktop Entry]\nName=GNOME\nExec=gnome-session\n",
        );
        write(
            &first,
            "greeter.desktop",
            "[Desktop Entry]\nName=Greeter\nNoDisplay=true\n",
        );
        write(
            &first,
            "absent.desktop",
            "[Desktop Entry]\nName=Absent\nTryExec=nope-xyz\n",
        );
        write(&first, "nameless.desktop", "[Desktop Entry]\nExec=x\n");
        write(&first, "notes.txt", "Name=Not a session\n");
        // The same id later on the path loses; a different id with a
        // name already seen is dropped.
        write(&second, "tuna.desktop", "[Desktop Entry]\nName=Old Tuna\n");
        write(&second, "zz-gnome.desktop", "[Desktop Entry]\nName=GNOME\n");
        let sessions = list(&[first, second], Some(bin.as_os_str()));
        let ids: Vec<&str> = sessions.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["gnome", "tuna"]);
        assert_eq!(sessions[1].name, "Tuna Desktop");
        // Without tuna-session on PATH the Tuna session is not offered.
        let bare = list(
            &[root.path().join("first/wayland-sessions")],
            Some(std::ffi::OsStr::new("/nonexistent")),
        );
        assert!(bare.iter().all(|s| s.id != "tuna"));
    }
}
