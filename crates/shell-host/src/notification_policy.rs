//! GNOME 51 notification settings, kept outside the pure queue store.
use gio::prelude::*;
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Mutex;

pub const MASTER_SCHEMA: &str = "org.gnome.desktop.notifications";
pub const APP_SCHEMA: &str = "org.gnome.desktop.notifications.application";
// Serialize this daemon's metadata read/append/write across wire workers.
// GTK's read-only policy refresh never takes this lock.
static REGISTRATION: Mutex<()> = Mutex::new(());

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NotificationPolicy {
    pub enable: bool,
    pub enable_sound: bool,
    pub show_banners: bool,
    pub force_expanded: bool,
    pub show_in_lock_screen: bool,
    pub details_in_lock_screen: bool,
}
impl Default for NotificationPolicy {
    fn default() -> Self {
        Self {
            enable: true,
            enable_sound: true,
            show_banners: true,
            force_expanded: false,
            show_in_lock_screen: false,
            details_in_lock_screen: false,
        }
    }
}

/// GNOME's lowercase ASCII key transformation, including collapsed dashes.
pub fn canonical_id(id: &str) -> String {
    let mut out = String::new();
    for c in id.to_lowercase().chars() {
        let c = if c.is_ascii_alphanumeric() || c == '-' {
            c
        } else {
            '-'
        };
        if c != '-' || !out.ends_with('-') {
            out.push(c);
        }
    }
    out
}
fn schema_settings(schema: &str, path: Option<&str>) -> Option<gio::Settings> {
    let source = gio::SettingsSchemaSource::default()?;
    let schema = source.lookup(schema, true)?;
    Some(gio::Settings::new_full(
        &schema,
        None::<&gio::SettingsBackend>,
        path,
    ))
}
fn boolean(settings: Option<&gio::Settings>, key: &str, fallback: bool) -> bool {
    settings
        .filter(|s| {
            s.settings_schema()
                .is_some_and(|schema| schema.has_key(key))
        })
        .map(|s| s.boolean(key))
        .unwrap_or(fallback)
}

/// GTK retains this reader on its main context for live settings signals.
/// Wire intake uses a short-lived reader on its worker before admission.
pub struct PolicyReader {
    master: Option<gio::Settings>,
    apps: HashMap<String, Option<gio::Settings>>,
    changed: Option<Rc<dyn Fn()>>,
    revision: Rc<Cell<u64>>,
    app_monitor: gio::AppInfoMonitor,
    app_revision: Rc<Cell<u64>>,
    observed_app_revision: u64,
}
impl Default for PolicyReader {
    fn default() -> Self {
        Self::new()
    }
}
impl PolicyReader {
    pub fn new() -> Self {
        Self {
            master: schema_settings(MASTER_SCHEMA, None),
            apps: HashMap::new(),
            changed: None,
            revision: Rc::new(Cell::new(0)),
            app_monitor: gio::AppInfoMonitor::get(),
            app_revision: Rc::new(Cell::new(0)),
            observed_app_revision: 0,
        }
    }
    pub fn on_changed(&mut self, changed: Rc<dyn Fn()>) {
        let app_revision = self.app_revision.clone();
        let revision = self.revision.clone();
        let callback = changed.clone();
        self.app_monitor.connect_changed(move |_| {
            crate::application_identity::installed_changed();
            app_revision.set(app_revision.get().wrapping_add(1));
            revision.set(revision.get().wrapping_add(1));
            callback();
        });
        if let Some(master) = &self.master {
            Self::observe(master, changed.clone(), self.revision.clone());
        }
        for settings in self.apps.values().flatten() {
            Self::observe(settings, changed.clone(), self.revision.clone());
        }
        self.changed = Some(changed);
    }
    pub fn revision(&self) -> u64 {
        self.revision.get()
    }
    pub fn retain_sources(&mut self, sources: &HashSet<String>) {
        self.apps.retain(|id, _| sources.contains(id));
    }
    fn observe(settings: &gio::Settings, changed: Rc<dyn Fn()>, revision: Rc<Cell<u64>>) {
        settings.connect_changed(None, move |_, key| {
            if matches!(
                key,
                "enable"
                    | "enable-sound-alerts"
                    | "show-banners"
                    | "force-expanded"
                    | "show-in-lock-screen"
                    | "details-in-lock-screen"
            ) {
                revision.set(revision.get().wrapping_add(1));
                changed();
            }
        });
    }
    pub fn read(&mut self, desktop_entry: &str, register: bool) -> NotificationPolicy {
        if self.observed_app_revision != self.app_revision.get() {
            self.apps.clear();
            self.observed_app_revision = self.app_revision.get();
        }
        // The store carries the app id without its final .desktop suffix.
        let id = desktop_entry;
        if !self.apps.contains_key(id) {
            let settings = (!id.is_empty()
                && gio_unix::DesktopAppInfo::new(&format!("{id}.desktop")).is_some())
            .then(|| {
                schema_settings(
                    APP_SCHEMA,
                    Some(&format!(
                        "/org/gnome/desktop/notifications/application/{}/",
                        canonical_id(id)
                    )),
                )
            })
            .flatten();
            if let (Some(settings), Some(changed)) = (&settings, &self.changed) {
                Self::observe(settings, changed.clone(), self.revision.clone());
            }
            self.apps.insert(id.to_owned(), settings);
        }
        let app = self.apps.get(id).and_then(|s| s.as_ref());
        if register {
            if let (Some(app), Some(master)) = (app, self.master.as_ref()) {
                let _registration = REGISTRATION.lock().unwrap_or_else(|e| e.into_inner());
                let desktop_id = format!("{id}.desktop");
                if app.string("application-id") != desktop_id {
                    let _ = app.set_string("application-id", &desktop_id);
                }
                let key = canonical_id(id);
                let mut children: Vec<String> = master
                    .strv("application-children")
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
                if !children.contains(&key) {
                    children.push(key);
                    let _ = master.set_strv("application-children", children);
                }
            }
        }
        NotificationPolicy {
            enable: boolean(app, "enable", true),
            enable_sound: boolean(app, "enable-sound-alerts", true),
            show_banners: boolean(self.master.as_ref(), "show-banners", true)
                && boolean(app, "show-banners", true),
            force_expanded: boolean(app, "force-expanded", false),
            show_in_lock_screen: boolean(self.master.as_ref(), "show-in-lock-screen", false)
                && boolean(app, "show-in-lock-screen", true),
            details_in_lock_screen: boolean(app, "details-in-lock-screen", false),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonical_ids_match_gnome() {
        assert_eq!(
            canonical_id("Org.Example__App--NAME"),
            "org-example-app-name"
        );
        assert_eq!(canonical_id("-Ö.Test!"), "-test-");
        assert_eq!(canonical_id("org.tuna.NotifyAlpha"), "org-tuna-notifyalpha");
    }
}
