//! The installed-app list, kept current while the shell runs.
//!
//! GNOME lists an app the moment it is installed. Every applications
//! directory is watched; a change marks the list stale, and the next
//! read rescans (so a burst of package-manager writes costs one scan).
//! Views that cache what they drew from the list hear about changes
//! through [`LiveApps::connect_changed`].

use std::cell::{Cell, Ref, RefCell};
use std::rc::Rc;

use gio::prelude::*;
use tuna_shell_host::apps::{default_app_dirs, AppProvider};

type Listeners = Rc<RefCell<Vec<Rc<dyn Fn()>>>>;

pub struct LiveApps {
    inner: RefCell<AppProvider>,
    stale: Rc<Cell<bool>>,
    listeners: Listeners,
    _monitors: Vec<gio::FileMonitor>,
}

impl LiveApps {
    pub fn new() -> Rc<Self> {
        let stale = Rc::new(Cell::new(false));
        let listeners: Listeners = Rc::default();
        let monitors = default_app_dirs()
            .into_iter()
            .filter_map(|dir| {
                let monitor = gio::File::for_path(&dir)
                    .monitor_directory(gio::FileMonitorFlags::NONE, None::<&gio::Cancellable>)
                    .ok()?;
                let (stale, listeners) = (stale.clone(), listeners.clone());
                monitor.connect_changed(move |_, _, _, _| {
                    stale.set(true);
                    let listeners = listeners.borrow().clone();
                    for listener in listeners {
                        listener();
                    }
                });
                Some(monitor)
            })
            .collect();
        Rc::new(Self {
            inner: RefCell::new(AppProvider::system()),
            stale,
            listeners,
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

    /// Call `f` whenever an applications directory changes (the list
    /// is stale until the next [`LiveApps::get`]).
    pub fn connect_changed(&self, f: impl Fn() + 'static) {
        self.listeners.borrow_mut().push(Rc::new(f));
    }
}
