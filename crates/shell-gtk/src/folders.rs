//! GNOME's app-grid folders (`org.gnome.desktop.app-folders`).
//!
//! GNOME keeps its app-grid folders in GSettings: `folder-children` lists
//! folder ids, and each folder (a relocatable schema at
//! `/org/gnome/desktop/app-folders/folders/<id>/`) names its apps
//! explicitly, by desktop category, or both, minus excluded apps. A name
//! with `translate` set is a `.directory` file whose Name is shown.
//! Reading the same keys makes Tuna Desktop's grid match the user's GNOME grid.

use std::path::PathBuf;

use gio::prelude::*;
use tuna_shell_host::apps::AppEntry;

const SCHEMA: &str = "org.gnome.desktop.app-folders";
const FOLDER_SCHEMA: &str = "org.gnome.desktop.app-folders.folder";

/// One folder's definition.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Folder {
    pub id: String,
    pub name: String,
    pub apps: Vec<String>,
    pub categories: Vec<String>,
    pub excluded: Vec<String>,
}

fn same_app(a: &str, b: &str) -> bool {
    a.trim_end_matches(".desktop") == b.trim_end_matches(".desktop")
}

impl Folder {
    /// Whether `app` sits in this folder (GNOME's rule).
    pub fn contains(&self, app: &AppEntry) -> bool {
        if self.excluded.iter().any(|e| same_app(e, &app.app_id)) {
            return false;
        }
        self.apps.iter().any(|a| same_app(a, &app.app_id))
            || app
                .categories
                .iter()
                .any(|c| self.categories.iter().any(|f| f == c))
    }
}

/// The grid's layout: folders that hold at least one installed app (with
/// those apps), and the apps no folder took, each sorted by name. Apps in
/// the dash (`favorites`) are left out of both, as GNOME 40 and later do.
pub fn arrange<'a>(
    folders: &[Folder],
    apps: &'a [AppEntry],
    favorites: &[String],
) -> (Vec<(Folder, Vec<&'a AppEntry>)>, Vec<&'a AppEntry>) {
    let shown: Vec<&AppEntry> = apps
        .iter()
        .filter(|a| !favorites.iter().any(|f| same_app(f, &a.app_id)))
        .collect();
    let mut filled: Vec<(Folder, Vec<&AppEntry>)> = folders
        .iter()
        .map(|f| {
            let mut members: Vec<&AppEntry> =
                shown.iter().copied().filter(|a| f.contains(a)).collect();
            members.sort_by_key(|a| a.name.to_lowercase());
            (f.clone(), members)
        })
        .filter(|(_, members)| !members.is_empty())
        .collect();
    filled.sort_by_key(|(f, _)| f.name.to_lowercase());
    let mut loose: Vec<&AppEntry> = shown
        .iter()
        .copied()
        .filter(|a| {
            !filled
                .iter()
                .any(|(_, members)| members.iter().any(|m| m.app_id == a.app_id))
        })
        .collect();
    loose.sort_by_key(|a| a.name.to_lowercase());
    (filled, loose)
}

/// A translated folder name: the Name of `<data dir>/desktop-directories/
/// <file>`, else the file stem.
fn directory_name(file: &str) -> String {
    directory_name_found(file).unwrap_or_else(|| file.trim_end_matches(".directory").to_owned())
}

/// A `.directory` file's translated Name, when one is installed
/// (`Shell.util_get_translated_folder_name`).
fn directory_name_found(file: &str) -> Option<String> {
    let dirs = std::iter::once(glib::user_data_dir()).chain(glib::system_data_dirs());
    for dir in dirs {
        let path: PathBuf = dir.join("desktop-directories").join(file);
        let key_file = glib::KeyFile::new();
        if key_file
            .load_from_file(&path, glib::KeyFileFlags::NONE)
            .is_ok()
        {
            if let Ok(name) = key_file.locale_string("Desktop Entry", "Name", None) {
                return Some(name.to_string());
            }
        }
    }
    None
}

/// GNOME's `_findBestFolderName`: the first category every app shares
/// whose `<category>.directory` is installed, by its translated name.
pub fn best_folder_name(apps: &[&AppEntry]) -> Option<String> {
    let first = apps.first()?;
    first
        .categories
        .iter()
        .filter(|c| !c.is_empty() && apps.iter().all(|a| a.categories.contains(c)))
        .find_map(|c| directory_name_found(&format!("{c}.directory")))
}

/// GNOME's `FolderView.addApp` on a folder's lists: the app joins
/// `apps` and leaves `excluded-apps`. Returns (apps, excluded).
pub fn with_app(apps: &[String], excluded: &[String], app: &str) -> (Vec<String>, Vec<String>) {
    let mut apps = apps.to_vec();
    if !apps.iter().any(|a| same_app(a, app)) {
        apps.push(app.to_owned());
    }
    let excluded = excluded
        .iter()
        .filter(|e| !same_app(e, app))
        .cloned()
        .collect();
    (apps, excluded)
}

/// GNOME's `FolderView.removeApp` on a folder's lists: the app leaves
/// `apps`; a category folder also excludes it. `None` when `apps` ends
/// up empty: GNOME deletes the folder then.
pub fn without_app(
    apps: &[String],
    categories: &[String],
    excluded: &[String],
    app: &str,
) -> Option<(Vec<String>, Vec<String>)> {
    let apps: Vec<String> = apps.iter().filter(|a| !same_app(a, app)).cloned().collect();
    if apps.is_empty() {
        return None;
    }
    let mut excluded = excluded.to_vec();
    if !categories.is_empty() && !excluded.iter().any(|e| same_app(e, app)) {
        excluded.push(app.to_owned());
    }
    Some((apps, excluded))
}

/// A desktop id as GNOME stores it in folder lists (`<id>.desktop`).
fn desktop_id(app: &str) -> String {
    if app.ends_with(".desktop") {
        app.to_owned()
    } else {
        format!("{app}.desktop")
    }
}

/// GNOME Shell's settings, when its schema has the grid layout key.
fn shell_settings() -> Option<gio::Settings> {
    let source = gio::SettingsSchemaSource::default()?;
    let schema = source.lookup("org.gnome.shell", true)?;
    schema
        .has_key("app-picker-layout")
        .then(|| gio::Settings::new("org.gnome.shell"))
}

/// GNOME's saved app-grid order (`app-picker-layout`): per page, each
/// item's id (desktop id or folder id) and position. Empty when unset.
pub fn picker_layout() -> Vec<Vec<(String, i32)>> {
    let Some(settings) = shell_settings() else {
        return Vec::new();
    };
    let value = settings.value("app-picker-layout");
    value
        .iter()
        .map(|page| {
            let mut entries: Vec<(String, i32)> = page
                .iter()
                .filter_map(|entry| {
                    let id = entry.child_value(0).str()?.to_owned();
                    let props = entry.child_value(1).as_variant()?;
                    let position = glib::VariantDict::new(Some(&props))
                        .lookup_value("position", None)?
                        .get::<i32>()?;
                    Some((id, position))
                })
                .collect();
            entries.sort_by_key(|(_, pos)| *pos);
            entries
        })
        .collect()
}

/// Save the app-grid order as GNOME does (`PageManager.pages`).
pub fn save_picker_layout(pages: &[Vec<(String, i32)>]) {
    let Some(settings) = shell_settings() else {
        return;
    };
    let packed: Vec<std::collections::HashMap<String, glib::Variant>> = pages
        .iter()
        .map(|page| {
            page.iter()
                .map(|(id, pos)| {
                    let props: std::collections::HashMap<String, glib::Variant> =
                        [("position".to_owned(), pos.to_variant())].into();
                    (id.clone(), props.to_variant())
                })
                .collect()
        })
        .collect();
    let _ = settings.set_value("app-picker-layout", &packed.to_variant());
}

fn schemas_present() -> bool {
    gio::SettingsSchemaSource::default().is_some_and(|source| {
        source.lookup(SCHEMA, true).is_some() && source.lookup(FOLDER_SCHEMA, true).is_some()
    })
}

fn folder_settings(id: &str) -> gio::Settings {
    let path = format!("/org/gnome/desktop/app-folders/folders/{id}/");
    gio::Settings::with_path(FOLDER_SCHEMA, &path)
}

/// GNOME's `createFolder`: a new folder (random id) holding `apps`,
/// named after their common category, else "Unnamed Folder". Returns its
/// id.
pub fn create(apps: &[&AppEntry]) -> Option<String> {
    if !schemas_present() || apps.is_empty() {
        return None;
    }
    let id = glib::uuid_string_random().to_string();
    let root = gio::Settings::new(SCHEMA);
    let mut observation = FolderCreateObservation::from_proof_env(&id);
    let created = create_with_settings(
        apps,
        &id,
        &root,
        || folder_settings(&id),
        observation.as_mut(),
    );
    if let Some(observation) = observation {
        observation.emit();
    }
    created
}

// Prepare the unadvertised child before publishing its id. Setter acceptance and
// cached readback are required; neither proves disk durability or a transaction
// against another process changing the same settings concurrently.
fn create_with_settings(
    apps: &[&AppEntry],
    id: &str,
    root: &gio::Settings,
    make_child: impl FnOnce() -> gio::Settings,
    mut observation: Option<&mut FolderCreateObservation>,
) -> Option<String> {
    if apps.is_empty()
        || root
            .strv("folder-children")
            .iter()
            .any(|value| value.as_str() == id)
    {
        return None;
    }
    let child = make_child();
    let name = best_folder_name(apps).unwrap_or_else(|| "Unnamed Folder".to_owned());
    if let Some(trace) = observation.as_deref_mut() {
        trace.expected_name = name.clone();
    }
    let writable = observation.as_ref().map(|_| child.is_writable("name"));
    let result = child.set_string("name", &name).is_ok();
    if let Some(trace) = observation.as_deref_mut() {
        trace.observe(
            "name-write",
            root,
            Some(&child),
            Some(writable.unwrap()),
            Some(result),
        );
    }
    if !result || child.string("name").as_str() != name {
        return None;
    }
    let ids: Vec<String> = apps.iter().map(|a| desktop_id(&a.app_id)).collect();
    if let Some(trace) = observation.as_deref_mut() {
        trace.expected_apps = ids.clone();
    }
    let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    let writable = observation.as_ref().map(|_| child.is_writable("apps"));
    let result = child.set_strv("apps", refs.as_slice()).is_ok();
    if let Some(trace) = observation.as_deref_mut() {
        trace.observe(
            "apps-write",
            root,
            Some(&child),
            Some(writable.unwrap()),
            Some(result),
        );
    }
    if !result {
        return None;
    }
    // Either setter can dispatch a same-process consumer that changes another
    // child key. Require the complete child together before advertising it.
    if child.string("name").as_str() != name
        || child
            .strv("apps")
            .iter()
            .map(|value| value.as_str())
            .collect::<Vec<_>>()
            != refs
    {
        return None;
    }
    // Re-read just before publication; initialization can dispatch a same-process
    // change to the root. This is not cross-process compare-and-swap.
    let mut children: Vec<String> = root
        .strv("folder-children")
        .iter()
        .map(|v| v.to_string())
        .collect();
    if children.iter().any(|value| value == id) {
        return None;
    }
    children.push(id.to_owned());
    let refs: Vec<&str> = children.iter().map(String::as_str).collect();
    let writable = observation
        .as_ref()
        .map(|_| root.is_writable("folder-children"));
    let result = root.set_strv("folder-children", refs.as_slice()).is_ok();
    if let Some(trace) = observation.as_deref_mut() {
        trace.observe(
            "root-children-write",
            root,
            Some(&child),
            Some(writable.unwrap()),
            Some(result),
        );
    }
    if !result
        || !root
            .strv("folder-children")
            .iter()
            .any(|value| value.as_str() == id)
    {
        return None;
    }
    gio::Settings::sync();
    if let Some(trace) = observation {
        trace.observe("after-sync", root, Some(&child), None, None);
    }
    Some(id.to_owned())
}

struct FolderCreateObservation {
    id: String,
    declared_source: String,
    proof_config: Option<PathBuf>,
    expected_name: String,
    expected_apps: Vec<String>,
    events: Vec<serde_json::Value>,
}

impl FolderCreateObservation {
    fn from_proof_env(id: &str) -> Option<Self> {
        if std::env::var("TUNA_FOLDER_CREATE_TRACE").ok().as_deref() != Some("1") {
            return None;
        }
        let source = std::env::var("TUNA_FOLDER_CREATE_SOURCE_COMMIT").ok()?;
        if source.len() != 40 || !source.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        Some(Self {
            id: id.to_owned(),
            declared_source: source,
            proof_config: std::env::var_os("TUNA_FOLDER_CREATE_PROOF_CONFIG").map(PathBuf::from),
            expected_name: String::new(),
            expected_apps: Vec::new(),
            events: Vec::new(),
        })
    }

    fn observe(
        &mut self,
        operation: &str,
        root: &gio::Settings,
        child: Option<&gio::Settings>,
        writable: Option<bool>,
        result: Option<bool>,
    ) {
        self.events.push(serde_json::json!({
            "sequence": self.events.len(), "operation": operation,
            "writable": writable, "setter_result": result,
            "advertised": root.strv("folder-children").iter().any(|value| value.as_str() == self.id),
            "cached_name": child.map(|s| s.string("name").to_string()),
            "cached_apps": child.map(|s| s.strv("apps").iter().map(|v| v.to_string()).collect::<Vec<_>>()),
            "file_sample": self.file_sample(),
        }));
    }

    fn file_sample(&self) -> serde_json::Value {
        use std::io::Read;
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        const LIMIT: u64 = 128 * 1024;
        let read = || -> Result<serde_json::Value, &'static str> {
            let config = self.proof_config.as_ref().ok_or("no-fixture-config")?;
            if !config.is_absolute() {
                return Err("relative-fixture-config");
            }
            let owner = unsafe { libc::getuid() };
            let metadata =
                std::fs::symlink_metadata(config).map_err(|_| "fixture-config-missing")?;
            if !metadata.is_dir() || metadata.uid() != owner || metadata.mode() & 0o077 != 0 {
                return Err("fixture-config-not-private-owned");
            }
            let path = config.join("glib-2.0/settings/keyfile");
            for ancestor in path.ancestors() {
                if std::fs::symlink_metadata(ancestor)
                    .map_err(|_| "fixture-path-missing")?
                    .file_type()
                    .is_symlink()
                {
                    return Err("fixture-path-symlink");
                }
            }
            let file = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(path)
                .map_err(|_| "fixture-file-unreadable")?;
            let metadata = file.metadata().map_err(|_| "fixture-file-metadata")?;
            if !metadata.is_file() || metadata.uid() != owner || metadata.len() > LIMIT {
                return Err("fixture-file-unowned-or-over-budget");
            }
            let mut bytes = Vec::new();
            file.take(LIMIT + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| "fixture-file-read")?;
            if bytes.len() as u64 > LIMIT {
                return Err("fixture-file-over-budget");
            }
            let data = std::str::from_utf8(&bytes).map_err(|_| "fixture-file-not-utf8")?;
            let keyfile = glib::KeyFile::new();
            keyfile
                .load_from_data(data, glib::KeyFileFlags::NONE)
                .map_err(|_| "fixture-file-invalid")?;
            let group = format!("org/gnome/desktop/app-folders/folders/{}", self.id);
            Ok(
                serde_json::json!({"kind":"physical-file-sample", "durability_proven":false,
                "name_variant":keyfile.value(&group,"name").ok().map(|v|v.to_string()),
                "apps_variant":keyfile.value(&group,"apps").ok().map(|v|v.to_string())}),
            )
        };
        read().unwrap_or_else(|error|serde_json::json!({"kind":"unavailable", "reason":error, "durability_proven":false}))
    }

    fn emit(self) {
        use std::io::Write;
        let record = serde_json::json!({"kind":"folder-create-observation", "pid":std::process::id(),
            "folder":self.id, "declared_source":self.declared_source,
            "source_binding":"external proof admission required; not authenticated by this environment",
            "expected_name":self.expected_name, "expected_apps":self.expected_apps,
            "events":self.events, "scope":"cached readback and bounded physical samples; no durability or cross-process atomicity claim"});
        let bytes = format!("FOLDER-CREATE {record}\n");
        let _ = std::io::stderr().lock().write_all(bytes.as_bytes());
    }
}

/// GNOME's `addApp`: put `app` in folder `id`.
pub fn add_app(id: &str, app: &str) {
    if !schemas_present() {
        return;
    }
    let s = folder_settings(id);
    let list = |key: &str| -> Vec<String> { s.strv(key).iter().map(|v| v.to_string()).collect() };
    let (apps, excluded) = with_app(&list("apps"), &list("excluded-apps"), &desktop_id(app));
    let a: Vec<&str> = apps.iter().map(String::as_str).collect();
    let e: Vec<&str> = excluded.iter().map(String::as_str).collect();
    let _ = s.set_strv("apps", a.as_slice());
    let _ = s.set_strv("excluded-apps", e.as_slice());
    gio::Settings::sync();
}

/// GNOME's `removeApp`: take `app` out of folder `id`, deleting the
/// folder when its app list empties.
pub fn remove_app(id: &str, app: &str) {
    if !schemas_present() {
        return;
    }
    let s = folder_settings(id);
    let list = |key: &str| -> Vec<String> { s.strv(key).iter().map(|v| v.to_string()).collect() };
    match without_app(
        &list("apps"),
        &list("categories"),
        &list("excluded-apps"),
        &desktop_id(app),
    ) {
        Some((apps, excluded)) => {
            let a: Vec<&str> = apps.iter().map(String::as_str).collect();
            let e: Vec<&str> = excluded.iter().map(String::as_str).collect();
            let _ = s.set_strv("apps", a.as_slice());
            let _ = s.set_strv("excluded-apps", e.as_slice());
        }
        None => {
            // Resetting every key deletes the relocatable schema.
            if let Some(schema) = s.settings_schema() {
                for key in schema.list_keys() {
                    s.reset(&key);
                }
            }
            let root = gio::Settings::new(SCHEMA);
            let children: Vec<String> = root
                .strv("folder-children")
                .iter()
                .map(|v| v.to_string())
                .filter(|c| c != id)
                .collect();
            let refs: Vec<&str> = children.iter().map(String::as_str).collect();
            let _ = root.set_strv("folder-children", refs.as_slice());
        }
    }
    gio::Settings::sync();
}

/// GNOME 51's default folders (appDisplay.js DEFAULT_FOLDERS, with the
/// app lists from its build configuration): id, `.directory` name,
/// categories, apps.
const DEFAULT_FOLDERS: &[(&str, &str, &[&str], &[&str])] = &[
    (
        "System",
        "X-GNOME-Shell-System.directory",
        &[],
        &[
            "nm-connection-editor.desktop",
            "org.gnome.DejaDup.desktop",
            "org.gnome.baobab.desktop",
            "org.gnome.DiskUtility.desktop",
            "org.gnome.Logs.desktop",
            "org.freedesktop.MalcontentControl.desktop",
            "org.freedesktop.GnomeAbrt.desktop",
            "org.gnome.Sysprof.desktop",
            "org.gnome.SystemMonitor.desktop",
            "org.gnome.tweaks.desktop",
        ],
    ),
    (
        "Utilities",
        "X-GNOME-Shell-Utilities.directory",
        &[],
        &[
            "org.gnome.Decibels.desktop",
            "org.gnome.Connections.desktop",
            "org.gnome.Papers.desktop",
            "org.gnome.FileRoller.desktop",
            "org.gnome.font-viewer.desktop",
            "org.gnome.Loupe.desktop",
            "org.gnome.seahorse.Application.desktop",
            "org.gnome.Seahorse.desktop",
            "org.gnome.Showtime.desktop",
        ],
    ),
    ("YaST", "suse-yast.directory", &["X-SuSE-YaST"], &[]),
    ("Pardus", "X-Pardus-Apps.directory", &["X-Pardus-Apps"], &[]),
];

/// GNOME's first-run folders for these installed apps: each default
/// folder, with only the listed apps that are installed.
pub fn default_folders(installed: &[AppEntry]) -> Vec<Folder> {
    DEFAULT_FOLDERS
        .iter()
        .map(|(id, name, categories, apps)| Folder {
            id: (*id).to_owned(),
            name: (*name).to_owned(),
            apps: apps
                .iter()
                .filter(|a| installed.iter().any(|i| same_app(a, &i.app_id)))
                .map(|a| (*a).to_owned())
                .collect(),
            categories: categories.iter().map(|c| (*c).to_owned()).collect(),
            excluded: Vec::new(),
        })
        .collect()
}

/// GNOME's `_ensureDefaultFolders`: an account that never set
/// `folder-children` gets the default folders written, as GNOME Shell
/// writes them on its first run.
pub fn ensure_defaults(installed: &[AppEntry]) {
    let Some(source) = gio::SettingsSchemaSource::default() else {
        return;
    };
    if source.lookup(SCHEMA, true).is_none() || source.lookup(FOLDER_SCHEMA, true).is_none() {
        return;
    }
    let root = gio::Settings::new(SCHEMA);
    if root.user_value("folder-children").is_some() || !root.strv("folder-children").is_empty() {
        return;
    }
    let folders = default_folders(installed);
    let ids: Vec<&str> = folders.iter().map(|f| f.id.as_str()).collect();
    let _ = root.set_strv("folder-children", ids.as_slice());
    for folder in &folders {
        let path = format!("/org/gnome/desktop/app-folders/folders/{}/", folder.id);
        let s = gio::Settings::with_path(FOLDER_SCHEMA, &path);
        let _ = s.set_string("name", &folder.name);
        let _ = s.set_boolean("translate", true);
        if !folder.categories.is_empty() {
            let c: Vec<&str> = folder.categories.iter().map(String::as_str).collect();
            let _ = s.set_strv("categories", c.as_slice());
        }
        if !folder.apps.is_empty() {
            let a: Vec<&str> = folder.apps.iter().map(String::as_str).collect();
            let _ = s.set_strv("apps", a.as_slice());
        }
    }
    gio::Settings::sync();
}

/// GNOME's rename: the folder's own name, untranslated from now on.
pub fn rename(id: &str, name: &str) {
    let Some(source) = gio::SettingsSchemaSource::default() else {
        return;
    };
    if source.lookup(FOLDER_SCHEMA, true).is_none() {
        return;
    }
    let path = format!("/org/gnome/desktop/app-folders/folders/{id}/");
    let s = gio::Settings::with_path(FOLDER_SCHEMA, &path);
    let _ = s.set_string("name", name);
    let _ = s.set_boolean("translate", false);
    gio::Settings::sync();
}

/// Retained settings instances subscribe before reading each key. A short-lived
/// reader can otherwise observe the backend cache before its change dispatch.
/// Callbacks mark a cell and schedule the [`on_change`](FolderWatch::on_change)
/// hook from an idle; the overview's sync owns graph reconciliation.
pub struct FolderWatch {
    root: Option<gio::Settings>,
    backend: Option<gio::SettingsBackend>,
    source: Option<gio::SettingsSchemaSource>,
    children: std::cell::RefCell<std::collections::BTreeMap<String, gio::Settings>>,
    dirty: std::rc::Rc<Dirty>,
}

/// A settings change not yet applied to the grid, and who to wake for it.
#[derive(Default)]
struct Dirty {
    set: std::cell::Cell<bool>,
    notify: std::cell::RefCell<Option<std::rc::Rc<dyn Fn()>>>,
}

impl Dirty {
    /// Mark the change and wake the hook once, outside the signal handler.
    fn mark(&self) {
        if self.set.replace(true) {
            return;
        }
        if let Some(notify) = self.notify.borrow().clone() {
            glib::MainContext::ref_thread_default().spawn_local(async move { notify() });
        }
    }
}

impl Default for FolderWatch {
    fn default() -> Self {
        Self::new()
    }
}

impl FolderWatch {
    pub fn new() -> Self {
        Self::with_backend(None)
    }

    fn with_backend(backend: Option<gio::SettingsBackend>) -> Self {
        Self::with_source(gio::SettingsSchemaSource::default(), backend)
    }

    fn with_source(
        source: Option<gio::SettingsSchemaSource>,
        backend: Option<gio::SettingsBackend>,
    ) -> Self {
        let dirty = std::rc::Rc::new(Dirty::default());
        let root = source.as_ref().and_then(|source| {
            source.lookup(FOLDER_SCHEMA, true)?;
            let schema = source.lookup(SCHEMA, true)?;
            Some(gio::Settings::new_full(&schema, backend.as_ref(), None))
        });
        if let Some(root) = &root {
            let weak = std::rc::Rc::downgrade(&dirty);
            root.connect_changed(Some("folder-children"), move |_, _| {
                if let Some(dirty) = weak.upgrade() {
                    dirty.mark();
                }
            });
        }
        let watch = Self {
            root,
            backend,
            source,
            children: Default::default(),
            dirty,
        };
        watch.reconcile();
        watch
    }

    /// Run `notify` (from an idle) when a folder setting changes. The
    /// shell is otherwise only woken by the compositor, which sends
    /// nothing while its state is unchanged.
    pub fn on_change(&self, notify: impl Fn() + 'static) {
        *self.dirty.notify.borrow_mut() = Some(std::rc::Rc::new(notify));
    }

    /// Refresh subscriptions even while the overview is closed or dragging.
    pub fn reconcile(&self) {
        let Some(root) = &self.root else { return };
        let ids = root.strv("folder-children");
        let mut children = self.children.borrow_mut();
        children.retain(|id, _| ids.iter().any(|v| v.as_str() == id));
        for id in ids.iter() {
            // A relocatable path component must not escape the folder tree.
            if id.is_empty() || id.contains('/') || children.contains_key(id.as_str()) {
                continue;
            }
            let schema = self
                .source
                .as_ref()
                .unwrap()
                .lookup(FOLDER_SCHEMA, true)
                .unwrap();
            let path = format!("/org/gnome/desktop/app-folders/folders/{id}/");
            let settings = gio::Settings::new_full(&schema, self.backend.as_ref(), Some(&path));
            let weak = std::rc::Rc::downgrade(&self.dirty);
            settings.connect_changed(None, move |_, _| {
                if let Some(dirty) = weak.upgrade() {
                    dirty.mark();
                }
            });
            // GSettings emits changed only for keys read after connecting.
            let _ = read_folder(id, &settings);
            children.insert(id.to_string(), settings);
        }
    }

    pub fn take_dirty_if(&self, open: bool, dragging: bool) -> bool {
        open && !dragging && self.take_dirty()
    }

    pub fn take_dirty(&self) -> bool {
        self.dirty.set.replace(false)
    }

    pub fn load(&self) -> Vec<Folder> {
        self.reconcile();
        let Some(root) = &self.root else {
            return Vec::new();
        };
        let children = self.children.borrow();
        root.strv("folder-children")
            .iter()
            .filter_map(|id| children.get(id.as_str()).map(|s| read_folder(id, s)))
            .collect()
    }
}

fn read_folder(id: &str, s: &gio::Settings) -> Folder {
    let list = |key: &str| s.strv(key).iter().map(|v| v.to_string()).collect();
    let raw = s.string("name").to_string();
    let name = if s.boolean("translate") {
        directory_name(&raw)
    } else if raw.is_empty() {
        id.to_owned()
    } else {
        raw
    };
    Folder {
        id: id.to_owned(),
        name,
        apps: list("apps"),
        categories: list("categories"),
        excluded: list("excluded-apps"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestSchemas {
        source: gio::SettingsSchemaSource,
        dir: PathBuf,
    }

    impl TestSchemas {
        fn new() -> Self {
            Self::with_name_refusal(false)
        }

        fn with_name_refusal(refuse_name: bool) -> Self {
            Self::with_create_refusal(refuse_name, false, false)
        }

        fn with_create_refusal(refuse_name: bool, refuse_apps: bool, refuse_root: bool) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "tuna-folder-schemas-{}",
                glib::uuid_string_random()
            ));
            std::fs::create_dir(&dir).unwrap();
            std::fs::write(
                dir.join("folders.gschema.xml"),
                r#"<schemalist>
<schema id="org.gnome.desktop.app-folders" path="/org/gnome/desktop/app-folders/">
<key name="folder-children" type="as"><default>[]</default></key></schema>
<schema id="org.gnome.desktop.app-folders.folder">
<key name="name" type="s"><default>''</default></key>
<key name="translate" type="b"><default>false</default></key>
<key name="apps" type="as"><default>[]</default></key>
<key name="categories" type="as"><default>[]</default></key>
<key name="excluded-apps" type="as"><default>[]</default></key>
</schema></schemalist>"#.replace(
                    "<key name=\"name\" type=\"s\"><default>''</default></key>",
                    if refuse_name {
                        "<key name=\"name\" type=\"s\"><choices><choice value=\"\"/></choices><default>''</default></key>"
                    } else {
                        "<key name=\"name\" type=\"s\"><default>''</default></key>"
                    },
                ).replace(
                    "<key name=\"apps\" type=\"as\"><default>[]</default></key>",
                    if refuse_apps {
                        "<key name=\"apps\" type=\"as\"><choices><choice value=\"one.desktop\"/></choices><default>[]</default></key>"
                    } else {
                        "<key name=\"apps\" type=\"as\"><default>[]</default></key>"
                    },
                ).replace(
                    "<key name=\"folder-children\" type=\"as\"><default>[]</default></key>",
                    if refuse_root {
                        "<key name=\"folder-children\" type=\"as\"><choices><choice value=\"Existing\"/></choices><default>[]</default></key>"
                    } else {
                        "<key name=\"folder-children\" type=\"as\"><default>[]</default></key>"
                    },
                ),
            )
            .unwrap();
            assert!(std::process::Command::new("glib-compile-schemas")
                .arg("--strict")
                .arg(&dir)
                .status()
                .unwrap()
                .success());
            let source = gio::SettingsSchemaSource::from_directory(&dir, None, false).unwrap();
            Self { source, dir }
        }
    }

    impl Drop for TestSchemas {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.dir).unwrap();
        }
    }

    fn dispatch_until(context: &glib::MainContext, mut ready: impl FnMut() -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            while context.pending() {
                context.iteration(false);
            }
            if ready() {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "folder change not dispatched"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn closed_and_dragging_preserve_folder_refresh_until_ready() {
        let watch = FolderWatch {
            root: None,
            backend: None,
            source: None,
            children: Default::default(),
            dirty: std::rc::Rc::new(Dirty {
                set: std::cell::Cell::new(true),
                notify: Default::default(),
            }),
        };
        assert!(!watch.take_dirty_if(false, false));
        assert!(!watch.take_dirty_if(true, true));
        assert!(watch.take_dirty_if(true, false));
        assert!(!watch.take_dirty_if(true, false));
        assert!(watch.load().is_empty(), "missing schemas remain graceful");
    }

    #[test]
    fn retained_folder_watch_memory_changes_and_subscription_removal() {
        let context = glib::MainContext::new();
        context
            .with_thread_default(|| {
                let fixture = TestSchemas::new();
                let source = fixture.source.clone();
                let backend = gio::memory_settings_backend_new();
                let watch = FolderWatch::with_source(Some(source.clone()), Some(backend.clone()));
                let root = gio::Settings::new_full(
                    &source.lookup(SCHEMA, true).unwrap(),
                    Some(&backend),
                    None,
                );
                let child = gio::Settings::new_full(
                    &source.lookup(FOLDER_SCHEMA, true).unwrap(),
                    Some(&backend),
                    Some("/org/gnome/desktop/app-folders/folders/Live/"),
                );
                assert!(watch.load().is_empty());
                root.set_strv("folder-children", ["Live"]).unwrap();
                dispatch_until(&context, || watch.dirty.set.get());
                watch.reconcile();
                watch.take_dirty();
                child.set_string("name", "Changed").unwrap();
                child.set_strv("apps", ["one.desktop"]).unwrap();
                child.set_strv("categories", ["Utility"]).unwrap();
                child.set_strv("excluded-apps", ["two.desktop"]).unwrap();
                child.set_boolean("translate", false).unwrap();
                dispatch_until(&context, || watch.dirty.set.get());
                let folders = watch.load();
                assert_eq!(folders[0].name, "Changed");
                assert_eq!(folders[0].apps, ["one.desktop"]);
                assert_eq!(folders[0].categories, ["Utility"]);
                assert_eq!(folders[0].excluded, ["two.desktop"]);
                root.set_strv("folder-children", [] as [&str; 0]).unwrap();
                dispatch_until(&context, || watch.load().is_empty());
                watch.take_dirty();
                child.set_string("name", "Removed child").unwrap();
                while context.pending() {
                    context.iteration(false);
                }
                assert!(!watch.take_dirty(), "removed child must no longer notify");
            })
            .unwrap();
    }

    #[test]
    fn folder_changes_wake_the_overview_without_compositor_traffic() {
        let context = glib::MainContext::new();
        context
            .with_thread_default(|| {
                let fixture = TestSchemas::new();
                let source = fixture.source.clone();
                let backend = gio::memory_settings_backend_new();
                let watch = FolderWatch::with_source(Some(source.clone()), Some(backend.clone()));
                let woken = std::rc::Rc::new(std::cell::Cell::new(0));
                let counter = woken.clone();
                watch.on_change(move || counter.set(counter.get() + 1));
                let root = gio::Settings::new_full(
                    &source.lookup(SCHEMA, true).unwrap(),
                    Some(&backend),
                    None,
                );
                root.set_strv("folder-children", ["Live"]).unwrap();
                dispatch_until(&context, || woken.get() == 1);
                // Pending changes wake once until the grid takes them.
                root.set_strv("folder-children", ["Live", "More"]).unwrap();
                while context.pending() {
                    context.iteration(false);
                }
                assert_eq!(woken.get(), 1);
                assert!(watch.take_dirty());
                root.set_strv("folder-children", ["Live"]).unwrap();
                dispatch_until(&context, || woken.get() == 2);
            })
            .unwrap();
    }

    #[test]
    fn retained_folder_watch_preserves_root_priority_and_rejects_path_escape() {
        let fixture = TestSchemas::new();
        let backend = gio::memory_settings_backend_new();
        let watch = FolderWatch::with_source(Some(fixture.source.clone()), Some(backend));
        let root = watch.root.as_ref().unwrap();
        root.set_strv("folder-children", ["Zebra", "Alpha", "escape/child", ""])
            .unwrap();
        let ids: Vec<_> = watch.load().into_iter().map(|f| f.id).collect();
        assert_eq!(ids, ["Zebra", "Alpha"]);
        root.set_strv("folder-children", ["Alpha", "Zebra"])
            .unwrap();
        let ids: Vec<_> = watch.load().into_iter().map(|f| f.id).collect();
        assert_eq!(ids, ["Alpha", "Zebra"]);
    }

    #[test]
    fn retained_folder_watch_keyfile_external_atomic_write() {
        let context = glib::MainContext::new();
        let dir =
            std::env::temp_dir().join(format!("tuna-folder-watch-{}", glib::uuid_string_random()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("settings");
        context.with_thread_default(|| {
            let fixture = TestSchemas::new();
            let backend = gio::keyfile_settings_backend_new(path.to_str().unwrap(), "/", None);
            let watch = FolderWatch::with_source(Some(fixture.source.clone()), Some(backend));
            assert!(watch.load().is_empty());
            let replacement = dir.join("replacement");
            std::fs::write(&replacement, "[org/gnome/desktop/app-folders]\nfolder-children=['Live']\n[org/gnome/desktop/app-folders/folders/Live]\nname='External'\napps=['one.desktop']\n").unwrap();
            std::fs::rename(&replacement, &path).unwrap();
            dispatch_until(&context, || watch.dirty.set.get());
            assert_eq!(watch.load()[0].name, "External");
            watch.take_dirty();
            std::fs::write(&replacement, "[org/gnome/desktop/app-folders]\nfolder-children=['Live']\n[org/gnome/desktop/app-folders/folders/Live]\nname='Renamed'\napps=['two.desktop']\n").unwrap();
            std::fs::rename(&replacement, &path).unwrap();
            dispatch_until(&context, || watch.dirty.set.get());
            let folders = watch.load();
            assert_eq!(folders[0].name, "Renamed");
            assert_eq!(folders[0].apps, ["two.desktop"]);
        }).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn observed_keyfile_create(
        refuse_name: bool,
    ) -> (
        TestSchemas,
        FolderCreateObservation,
        gio::Settings,
        gio::Settings,
    ) {
        observed_keyfile_create_with_refusal(refuse_name, false, false)
    }

    fn observed_keyfile_create_with_refusal(
        refuse_name: bool,
        refuse_apps: bool,
        refuse_root: bool,
    ) -> (
        TestSchemas,
        FolderCreateObservation,
        gio::Settings,
        gio::Settings,
    ) {
        use std::os::unix::fs::PermissionsExt;
        let fixture = TestSchemas::with_create_refusal(refuse_name, refuse_apps, refuse_root);
        let config = fixture.dir.join("config");
        std::fs::create_dir_all(config.join("glib-2.0/settings")).unwrap();
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o700)).unwrap();
        let file = config.join("glib-2.0/settings/keyfile");
        let backend = gio::keyfile_settings_backend_new(file.to_str().unwrap(), "/", None);
        let root = gio::Settings::new_full(
            &fixture.source.lookup(SCHEMA, true).unwrap(),
            Some(&backend),
            None,
        );
        let child = gio::Settings::new_full(
            &fixture.source.lookup(FOLDER_SCHEMA, true).unwrap(),
            Some(&backend),
            Some("/org/gnome/desktop/app-folders/folders/Live/"),
        );
        root.set_strv("folder-children", ["Existing"]).unwrap();
        let mut trace = FolderCreateObservation {
            id: "Live".to_owned(),
            declared_source: "a".repeat(40),
            proof_config: Some(config),
            expected_name: String::new(),
            expected_apps: Vec::new(),
            events: Vec::new(),
        };
        let one = app("one", "One", &[]);
        let two = app("two", "Two", &[]);
        assert_eq!(
            create_with_settings(
                &[&one, &two],
                "Live",
                &root,
                || child.clone(),
                Some(&mut trace)
            ),
            if refuse_name || refuse_apps || refuse_root {
                None
            } else {
                Some("Live".to_owned())
            }
        );
        (fixture, trace, root, child)
    }

    #[test]
    fn create_observer_real_keyfile_records_each_actual_setter_and_file() {
        let (_fixture, trace, _root, _child) = observed_keyfile_create(false);
        assert_eq!(trace.expected_name, "Unnamed Folder");
        assert_eq!(trace.expected_apps, ["one.desktop", "two.desktop"]);
        assert_eq!(trace.events.len(), 4);
        for (index, operation) in [
            "name-write",
            "apps-write",
            "root-children-write",
            "after-sync",
        ]
        .iter()
        .enumerate()
        {
            assert_eq!(trace.events[index]["operation"], *operation);
            assert_eq!(trace.events[index]["sequence"], index);
            if index < 3 {
                assert_eq!(trace.events[index]["setter_result"], true);
            }
        }
        assert_eq!(trace.events[0]["advertised"], false);
        assert_eq!(trace.events[1]["advertised"], false);
        assert_eq!(trace.events[2]["advertised"], true);
        // The publication event already observes the complete child.
        assert_eq!(trace.events[2]["cached_name"], "Unnamed Folder");
        assert_eq!(
            trace.events[2]["cached_apps"],
            serde_json::json!(["one.desktop", "two.desktop"])
        );
        let final_event = &trace.events[3];
        assert_eq!(final_event["cached_name"], "Unnamed Folder");
        assert_eq!(
            final_event["file_sample"]["name_variant"],
            "'Unnamed Folder'"
        );
        assert_eq!(
            final_event["file_sample"]["apps_variant"],
            "['one.desktop', 'two.desktop']"
        );
        assert_eq!(final_event["file_sample"]["durability_proven"], false);
    }

    #[test]
    fn create_refused_name_does_not_publish_or_write_apps() {
        let (_fixture, trace, root, child) = observed_keyfile_create(true);
        // Deliberately restrictive real schema: not an attribution of the old CI failure.
        assert_eq!(trace.events.len(), 1);
        assert_eq!(trace.events[0]["operation"], "name-write");
        assert_eq!(trace.events[0]["writable"], true);
        assert_eq!(trace.events[0]["setter_result"], false);
        assert_eq!(trace.events[0]["advertised"], false);
        assert_eq!(
            root.strv("folder-children")
                .iter()
                .map(|value| value.as_str())
                .collect::<Vec<_>>(),
            ["Existing"]
        );
        assert_eq!(child.string("name"), "");
        assert!(child.strv("apps").is_empty());
    }

    #[test]
    fn create_refused_apps_does_not_publish_or_change_existing_children() {
        let (_fixture, trace, root, child) =
            observed_keyfile_create_with_refusal(false, true, false);
        assert_eq!(trace.events.len(), 2);
        assert_eq!(trace.events[1]["operation"], "apps-write");
        assert_eq!(trace.events[1]["setter_result"], false);
        assert!(trace.events.iter().all(|e| e["advertised"] == false));
        assert_eq!(
            root.strv("folder-children")
                .iter()
                .map(|value| value.as_str())
                .collect::<Vec<_>>(),
            ["Existing"]
        );
        assert_eq!(child.string("name"), "Unnamed Folder");
        assert!(child.strv("apps").is_empty());
    }

    #[test]
    fn create_refused_root_does_not_report_success_or_remove_existing_children() {
        let (_fixture, trace, root, child) =
            observed_keyfile_create_with_refusal(false, false, true);
        assert_eq!(trace.events.len(), 3);
        assert_eq!(trace.events[2]["operation"], "root-children-write");
        assert_eq!(trace.events[2]["setter_result"], false);
        assert!(trace.events.iter().all(|e| e["advertised"] == false));
        assert_eq!(
            root.strv("folder-children")
                .iter()
                .map(|value| value.as_str())
                .collect::<Vec<_>>(),
            ["Existing"]
        );
        assert_eq!(child.string("name"), "Unnamed Folder");
        assert_eq!(
            child
                .strv("apps")
                .iter()
                .map(|value| value.as_str())
                .collect::<Vec<_>>(),
            ["one.desktop", "two.desktop"]
        );
    }

    #[test]
    fn create_existing_id_or_empty_members_leaves_existing_folder_untouched() {
        let fixture = TestSchemas::new();
        let backend = gio::memory_settings_backend_new();
        let root = gio::Settings::new_full(
            &fixture.source.lookup(SCHEMA, true).unwrap(),
            Some(&backend),
            None,
        );
        root.set_strv("folder-children", ["Existing"]).unwrap();
        let one = app("one", "One", &[]);
        assert_eq!(
            create_with_settings(
                &[&one],
                "Existing",
                &root,
                || panic!("must not touch existing child"),
                None
            ),
            None
        );
        assert_eq!(
            create_with_settings(
                &[],
                "New",
                &root,
                || panic!("empty input must not touch child"),
                None
            ),
            None
        );
        assert_eq!(
            root.strv("folder-children")
                .iter()
                .map(|value| value.as_str())
                .collect::<Vec<_>>(),
            ["Existing"]
        );
    }

    #[test]
    fn create_preserves_root_change_dispatched_during_child_initialization() {
        let fixture = TestSchemas::new();
        let backend = gio::memory_settings_backend_new();
        let root = gio::Settings::new_full(
            &fixture.source.lookup(SCHEMA, true).unwrap(),
            Some(&backend),
            None,
        );
        let child = gio::Settings::new_full(
            &fixture.source.lookup(FOLDER_SCHEMA, true).unwrap(),
            Some(&backend),
            Some("/org/gnome/desktop/app-folders/folders/Live/"),
        );
        root.set_strv("folder-children", ["Existing"]).unwrap();
        let changing_root = root.clone();
        child.connect_changed(Some("name"), move |_, _| {
            changing_root
                .set_strv("folder-children", ["Existing", "Other"])
                .unwrap();
        });
        // Gio requires reading this key after connecting for changed delivery.
        let _ = child.string("name");
        let one = app("one", "One", &[]);
        assert_eq!(
            create_with_settings(&[&one], "Live", &root, || child.clone(), None),
            Some("Live".to_owned())
        );
        assert_eq!(
            root.strv("folder-children")
                .iter()
                .map(|value| value.as_str())
                .collect::<Vec<_>>(),
            ["Existing", "Other", "Live"]
        );
    }

    #[test]
    fn create_refuses_name_changed_by_actual_apps_setter_callback() {
        let fixture = TestSchemas::new();
        let backend = gio::memory_settings_backend_new();
        let root = gio::Settings::new_full(
            &fixture.source.lookup(SCHEMA, true).unwrap(),
            Some(&backend),
            None,
        );
        let child = gio::Settings::new_full(
            &fixture.source.lookup(FOLDER_SCHEMA, true).unwrap(),
            Some(&backend),
            Some("/org/gnome/desktop/app-folders/folders/Live/"),
        );
        root.set_strv("folder-children", ["Existing"]).unwrap();
        child.connect_changed(Some("apps"), move |child, _| {
            child
                .set_string("name", "Changed during app initialization")
                .unwrap();
        });
        let _ = child.strv("apps");
        let one = app("one", "One", &[]);
        assert_eq!(
            create_with_settings(&[&one], "Live", &root, || child.clone(), None),
            None
        );
        assert_eq!(
            root.strv("folder-children")
                .iter()
                .map(|value| value.as_str())
                .collect::<Vec<_>>(),
            ["Existing"]
        );
        assert_eq!(child.string("name"), "Changed during app initialization");
    }

    #[test]
    fn create_observer_actual_file_replacement_distinguishes_later_loss() {
        let (_fixture, mut trace, root, child) = observed_keyfile_create(false);
        assert_eq!(trace.events[0]["setter_result"], true);
        let path = trace
            .proof_config
            .as_ref()
            .unwrap()
            .join("glib-2.0/settings/keyfile");
        let replacement = path.with_extension("replacement");
        std::fs::write(&replacement, "[org/gnome/desktop/app-folders]\nfolder-children=['Live']\n[org/gnome/desktop/app-folders/folders/Live]\napps=['one.desktop', 'two.desktop']\n").unwrap();
        std::fs::rename(replacement, path).unwrap();
        trace.observe(
            "fixture-external-replacement",
            &root,
            Some(&child),
            None,
            None,
        );
        assert_eq!(
            trace.events[3]["file_sample"]["name_variant"],
            "'Unnamed Folder'"
        );
        assert_eq!(
            trace.events[4]["file_sample"]["name_variant"],
            serde_json::Value::Null
        );
        assert_eq!(
            trace.events[4]["file_sample"]["apps_variant"],
            "['one.desktop', 'two.desktop']"
        );
        // A cached backend value is deliberately NOT called a durable file read.
        assert_eq!(trace.events[4]["file_sample"]["durability_proven"], false);
    }

    #[test]
    fn create_file_observation_refuses_symlink_and_excess_file_before_read() {
        use std::os::unix::fs::symlink;
        let (fixture, trace, _root, _child) = observed_keyfile_create(false);
        let config = trace.proof_config.as_ref().unwrap();
        let original = config.join("glib-2.0");
        let moved = fixture.dir.join("moved");
        std::fs::rename(&original, &moved).unwrap();
        symlink(&moved, &original).unwrap();
        assert_eq!(trace.file_sample()["reason"], "fixture-path-symlink");
        std::fs::remove_file(&original).unwrap();
        std::fs::rename(&moved, &original).unwrap();
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(original.join("settings/keyfile"))
            .unwrap();
        file.set_len(128 * 1024 + 1).unwrap();
        assert_eq!(
            trace.file_sample()["reason"],
            "fixture-file-unowned-or-over-budget"
        );
    }

    #[test]
    fn folder_lists_change_like_gnome() {
        let v = |items: &[&str]| items.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        // Adding: joins apps, leaves the exclusions.
        let (apps, excluded) = with_app(&v(&["a.desktop"]), &v(&["b.desktop"]), "b.desktop");
        assert_eq!(apps, v(&["a.desktop", "b.desktop"]));
        assert!(excluded.is_empty());
        // Removing from an app-list folder.
        let (apps, excluded) =
            without_app(&v(&["a.desktop", "b.desktop"]), &[], &[], "b.desktop").unwrap();
        assert_eq!(apps, v(&["a.desktop"]));
        assert!(excluded.is_empty());
        // A category folder also excludes it.
        let (_, excluded) =
            without_app(&v(&["a.desktop", "b.desktop"]), &v(&["Game"]), &[], "b").unwrap();
        assert_eq!(excluded, v(&["b"]));
        // The last app out deletes the folder.
        assert!(without_app(&v(&["a.desktop"]), &[], &[], "a").is_none());
    }

    fn app(id: &str, name: &str, categories: &[&str]) -> AppEntry {
        AppEntry {
            app_id: id.into(),
            name: name.into(),
            generic_name: None,
            keywords: Vec::new(),
            argv: Vec::new(),
            icon: None,
            categories: categories.iter().map(|c| (*c).to_owned()).collect(),
            new_window: Default::default(),
        }
    }

    #[test]
    fn folders_take_apps_by_id_and_category_minus_exclusions() {
        let utilities = Folder {
            id: "Utilities".into(),
            name: "Utilities".into(),
            apps: vec!["org.gnome.Logs.desktop".into()],
            categories: vec!["X-GNOME-Utilities".into()],
            excluded: vec!["org.gnome.Console.desktop".into()],
        };
        let empty = Folder {
            id: "Empty".into(),
            name: "Empty".into(),
            apps: vec!["missing.desktop".into()],
            ..Default::default()
        };
        let apps = vec![
            app("org.gnome.Logs", "Logs", &[]),
            app("org.gnome.Calculator", "Calculator", &["X-GNOME-Utilities"]),
            app("org.gnome.Console", "Console", &["X-GNOME-Utilities"]),
            app("firefox", "Firefox", &["Network"]),
        ];
        let (folders, loose) = arrange(&[utilities, empty], &apps, &[]);
        assert_eq!(folders.len(), 1, "a folder with no installed app is hidden");
        let names: Vec<&str> = folders[0].1.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["Calculator", "Logs"]);
        let loose: Vec<&str> = loose.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(
            loose,
            ["Console", "Firefox"],
            "excluded and unfiled apps stay loose"
        );
    }

    #[test]
    fn first_run_folders_follow_gnome_51() {
        let installed = vec![
            app(
                "org.freedesktop.MalcontentControl",
                "Parental Controls",
                &[],
            ),
            app("org.gnome.Loupe", "Image Viewer", &[]),
        ];
        let folders = default_folders(&installed);
        let ids: Vec<&str> = folders.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(ids, ["System", "Utilities", "YaST", "Pardus"]);
        assert_eq!(
            folders[0].apps,
            ["org.freedesktop.MalcontentControl.desktop"]
        );
        assert_eq!(folders[1].apps, ["org.gnome.Loupe.desktop"]);
        assert_eq!(folders[2].categories, ["X-SuSE-YaST"]);
        assert!(folders[2].apps.is_empty());
        let (filled, loose) = arrange(&folders, &installed, &[]);
        assert_eq!(filled.len(), 2);
        assert!(loose.is_empty());
    }

    #[test]
    fn dash_favorites_leave_the_grid() {
        let utilities = Folder {
            id: "Utilities".into(),
            name: "Utilities".into(),
            categories: vec!["X-GNOME-Utilities".into()],
            ..Default::default()
        };
        let apps = vec![
            app("org.gnome.Calculator", "Calculator", &["X-GNOME-Utilities"]),
            app("org.gnome.Nautilus", "Files", &[]),
            app("org.gnome.Settings", "Settings", &[]),
        ];
        let favorites = vec![
            "org.gnome.Calculator.desktop".to_owned(),
            "org.gnome.Nautilus.desktop".to_owned(),
        ];
        let (folders, loose) = arrange(&[utilities], &apps, &favorites);
        assert!(folders.is_empty(), "a folder of favorites alone is hidden");
        let loose: Vec<&str> = loose.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(loose, ["Settings"]);
    }
}
