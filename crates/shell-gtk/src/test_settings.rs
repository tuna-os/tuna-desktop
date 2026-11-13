//! Test-only GSettings fixtures: schemas compiled into a private
//! directory and read through a private memory backend, so settings
//! bindings run without dconf, a session bus, or the host's schemas.

/// Compiled fixture schemas and the memory backend they write to.
pub struct SettingsFixture {
    source: gio::SettingsSchemaSource,
    backend: gio::SettingsBackend,
    _dir: tempfile::TempDir,
}

impl SettingsFixture {
    /// Compile `xml` (a whole `<schemalist>`) with
    /// `glib-compile-schemas --strict`.
    pub fn new(xml: &str) -> Self {
        let dir = tempfile::tempdir().expect("schema dir");
        std::fs::write(dir.path().join("fixture.gschema.xml"), xml).expect("write schema");
        let status = std::process::Command::new("glib-compile-schemas")
            .arg("--strict")
            .arg(dir.path())
            .status()
            .expect("glib-compile-schemas must be installed");
        assert!(status.success(), "fixture schemas compile");
        let source = gio::SettingsSchemaSource::from_directory(dir.path(), None, false)
            .expect("compiled schemas load");
        Self {
            source,
            backend: gio::memory_settings_backend_new(),
            _dir: dir,
        }
    }

    /// Settings for fixture schema `id` on this fixture's backend. Signals
    /// are delivered on the thread-default main context at creation.
    pub fn settings(&self, id: &str) -> gio::Settings {
        let schema = self.source.lookup(id, false).expect("fixture schema");
        gio::Settings::new_full(&schema, Some(&self.backend), None)
    }
}

/// Run `context` until `ready`, failing after three seconds.
pub fn dispatch_until(context: &glib::MainContext, mut ready: impl FnMut() -> bool) {
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
            "settings change never arrived"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}
