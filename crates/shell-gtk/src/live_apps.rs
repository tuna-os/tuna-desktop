//! The installed-app list, kept current while the shell runs.
//!
//! GNOME lists an app the moment it is installed. Every applications
//! directory is watched; a change marks the list stale, and the next
//! read rescans (so a burst of package-manager writes costs one scan).

use std::cell::{Cell, Ref, RefCell};
use std::rc::Rc;

use gio::prelude::*;
use tuna_shell_host::apps::{default_app_dirs, AppProvider};

pub struct LiveApps {
    inner: RefCell<AppProvider>,
    stale: Rc<Cell<bool>>,
    _monitors: Vec<gio::FileMonitor>,
}

impl LiveApps {
    pub fn new(wake: Rc<crate::ui_wake::UiWake>) -> Rc<Self> {
        Self::with_dirs(default_app_dirs(), wake)
    }

    fn with_dirs(dirs: Vec<std::path::PathBuf>, wake: Rc<crate::ui_wake::UiWake>) -> Rc<Self> {
        let stale = Rc::new(Cell::new(false));
        let monitors = dirs
            .into_iter()
            .filter_map(|dir| {
                let monitor = gio::File::for_path(&dir)
                    .monitor_directory(gio::FileMonitorFlags::NONE, None::<&gio::Cancellable>)
                    .ok()?;
                let stale = stale.clone();
                let wake = Rc::downgrade(&wake);
                monitor.connect_changed(move |_, _, _, _| {
                    stale.set(true);
                    if let Some(wake) = wake.upgrade() {
                        wake.request();
                    }
                });
                Some(monitor)
            })
            .collect();
        Rc::new(Self {
            inner: RefCell::new(AppProvider::system()),
            stale,
            _monitors: monitors,
        })
    }

    /// The current list, rescanned first if an app directory changed.
    pub fn get(&self) -> Ref<'_, AppProvider> {
        if self.stale.get() {
            if let Ok(mut inner) = self.inner.try_borrow_mut() {
                *inner = AppProvider::system();
                self.stale.set(false);
            }
        }
        self.inner.borrow()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn native_directory_monitor_invalidates_cache_and_wakes_without_a_poll_timer() {
        let context = glib::MainContext::default();
        let _owner = context.acquire().unwrap();
        let wake = Rc::new(crate::ui_wake::UiWake::default());
        let changes = Rc::new(Cell::new(0));
        let changed = changes.clone();
        wake.install(move || changed.set(changed.get() + 1));
        while context.pending() {
            context.iteration(false);
        }
        assert_eq!(changes.get(), 1);
        let directory = tempfile::tempdir().unwrap();
        let apps = LiveApps::with_dirs(vec![directory.path().to_owned()], wake.clone());
        assert_eq!(apps._monitors.len(), 1);
        assert!(!apps.stale.get());
        std::fs::write(
            directory.path().join("new-app.desktop"),
            "[Desktop Entry]\nName=Test\n",
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while changes.get() == 1 && Instant::now() < deadline {
            while context.pending() {
                context.iteration(false);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(
            changes.get() > 1,
            "real file notification must schedule UI work"
        );
        assert!(apps.stale.get());
        drop(apps.get());
        assert!(!apps.stale.get(), "next cache read refreshes it");
        // The production dispatcher owns the cache/UI. Signal callbacks must
        // not retain the dispatcher in the opposite direction.
        let weak = Rc::downgrade(&wake);
        let retained_cache = apps.clone();
        wake.install(move || drop(retained_cache.get()));
        drop(wake);
        assert!(
            weak.upgrade().is_none(),
            "file monitors must not form a dispatcher/cache cycle"
        );
        while context.pending() {
            context.iteration(false);
        }
    }
}
