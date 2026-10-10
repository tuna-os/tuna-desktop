//! GNOME 51 tracked-window desktop matching. Notification hints stay exact.
//! Desktop discovery runs at intake, outside the notification-center lock.
use gio::prelude::*;
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex, OnceLock,
    },
};

#[derive(Clone, PartialEq, Eq)]
pub struct WindowIdentity {
    pub class: String,
    /// X11 instance; native windows have no separate instance.
    pub instance: Option<String>,
}

#[derive(Clone)]
struct Entry {
    id: String,
    startup: Option<String>,
    visible: bool,
}
struct Catalog {
    entries: Vec<Entry>,
    startup: HashMap<String, usize>,
}
impl Catalog {
    fn new(entries: Vec<Entry>) -> Self {
        let mut startup = HashMap::<String, usize>::new();
        for (index, entry) in entries.iter().enumerate() {
            let Some(class) = &entry.startup else {
                continue;
            };
            // GNOME's cache scan replaces an earlier duplicate for an exact
            // basename, or for a visible entry after a hidden one.
            let replace = startup.get(class).is_none_or(|old| {
                entry.id == *class
                    || entry.id == format!("{class}.desktop")
                    || (entry.visible && !entries[*old].visible)
            });
            if replace {
                startup.insert(class.clone(), index);
            }
        }
        Self { entries, startup }
    }
    fn discover() -> Self {
        let entries = gio::AppInfo::all()
            .into_iter()
            .filter_map(|app| {
                let desktop = app.downcast::<gio_unix::DesktopAppInfo>().ok()?;
                Some(Entry {
                    id: desktop.id()?.to_string(),
                    startup: desktop.startup_wm_class().map(|s| s.to_string()),
                    visible: desktop.should_show(),
                })
            })
            .collect();
        Self::new(entries)
    }
    fn basename(&self, class: &str) -> Option<&str> {
        const PREFIXES: [&str; 5] = ["", "gnome-", "fedora-", "mozilla-", "debian-"];
        let canonical = class.to_ascii_lowercase().replace(' ', "-");
        for name in [class, canonical.as_str()] {
            for prefix in PREFIXES {
                let id = format!("{prefix}{name}.desktop");
                if let Some(entry) = self.entries.iter().find(|entry| entry.id == id) {
                    return Some(&entry.id);
                }
            }
        }
        None
    }
    fn resolve(&self, identity: &WindowIdentity) -> Option<String> {
        let names = [identity.instance.as_deref(), Some(identity.class.as_str())];
        for name in names.into_iter().flatten().filter(|name| !name.is_empty()) {
            if let Some(index) = self.startup.get(name) {
                let id = &self.entries[*index].id;
                return Some(id.strip_suffix(".desktop").unwrap_or(id).to_owned());
            }
        }
        for name in names.into_iter().flatten().filter(|name| !name.is_empty()) {
            if let Some(id) = self.basename(name) {
                return Some(id.strip_suffix(".desktop").unwrap_or(id).to_owned());
            }
        }
        None
    }
}
static EPOCH: AtomicU64 = AtomicU64::new(1);
static CATALOG: OnceLock<Mutex<Option<(u64, Catalog)>>> = OnceLock::new();

/// Called by the UI's live AppInfoMonitor, never scans or locks the catalog.
pub(crate) fn installed_changed() {
    EPOCH.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn resolve_tracked(identity: &WindowIdentity) -> Option<String> {
    let epoch = EPOCH.load(Ordering::Relaxed);
    let mut cached = CATALOG.get_or_init(|| Mutex::new(None)).lock().ok()?;
    if cached.as_ref().is_none_or(|(seen, _)| *seen != epoch) {
        *cached = Some((epoch, Catalog::discover()));
    }
    cached.as_ref()?.1.resolve(identity)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn entry(id: &str, startup: Option<&str>, visible: bool) -> Entry {
        Entry {
            id: format!("{id}.desktop"),
            startup: startup.map(str::to_owned),
            visible,
        }
    }
    fn identity(class: &str, instance: Option<&str>) -> WindowIdentity {
        WindowIdentity {
            class: class.into(),
            instance: instance.map(str::to_owned),
        }
    }
    #[test]
    fn startup_instance_precedes_class_and_desktop_heuristics() {
        let catalog = Catalog::new(vec![
            entry("Instance", Some("shortcut"), true),
            entry("Class", Some("Browser"), true),
            entry("shortcut", None, true),
        ]);
        assert_eq!(
            catalog.resolve(&identity("Browser", Some("shortcut"))),
            Some("Instance".into())
        );
        assert_eq!(
            catalog.resolve(&identity("Browser", Some("Shortcut"))),
            Some("Class".into())
        );
    }
    #[test]
    fn duplicate_scan_prefers_exact_and_visible_as_gnome_does() {
        let catalog = Catalog::new(vec![
            entry("Hidden", Some("Match"), false),
            entry("Visible", Some("Match"), true),
            entry("Match", Some("Match"), true),
        ]);
        assert_eq!(
            catalog.resolve(&identity("Match", None)),
            Some("Match".into())
        );
        let catalog = Catalog::new(vec![
            entry("Match", Some("Match"), false),
            entry("Visible", Some("Match"), true),
        ]);
        assert_eq!(
            catalog.resolve(&identity("Match", None)),
            Some("Visible".into())
        );
    }
    #[test]
    fn basename_preserves_case_before_ascii_canonicalization_and_vendor_order() {
        let catalog = Catalog::new(vec![
            entry("gnome-Editor", None, true),
            entry("editor", None, true),
            entry("fedora-fedora-eclipse", None, true),
            entry("mozilla-fedora-eclipse", None, true),
            entry("Exact.Instance", None, true),
        ]);
        assert_eq!(
            catalog.resolve(&identity("Editor", None)),
            Some("gnome-Editor".into())
        );
        assert_eq!(
            catalog.resolve(&identity("Fedora Eclipse", None)),
            Some("fedora-fedora-eclipse".into())
        );
        assert_eq!(
            catalog.resolve(&identity("Unknown", Some("Exact.Instance"))),
            Some("Exact.Instance".into())
        );
        assert_eq!(catalog.resolve(&identity("Not Installed", None)), None);
    }
}
