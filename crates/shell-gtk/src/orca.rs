//! Publish the GNOME preference; the compositor that serves the
//! KeyboardMonitor (hardware, or nested with `--screen-reader`) owns Orca.
use gio::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

fn applications() -> Option<gio::Settings> {
    crate::settings("org.gnome.desktop.a11y.applications").filter(|settings| {
        settings
            .settings_schema()
            .is_some_and(|schema| schema.has_key("screen-reader-enabled"))
    })
}

/// `gsd-media-keys`' `screenreader` binding (Super+Alt+S): flip
/// `screen-reader-enabled` (`do_toggle_accessibility_key`, gsd 51.0).
pub fn toggle() {
    let Some(settings) = applications() else {
        return;
    };
    let enabled = settings.boolean("screen-reader-enabled");
    if let Err(error) = settings.set_boolean("screen-reader-enabled", !enabled) {
        eprintln!("tuna-shell-gtk: screen reader toggle: {error}");
    }
}

pub fn start(shell: Rc<RefCell<crate::Shell>>) {
    let Some(settings) = applications() else {
        return;
    };
    let publish = {
        let settings = settings.clone();
        Rc::new(move || {
            let enabled = settings.boolean("screen-reader-enabled");
            if enabled {
                if let Some(interface) = crate::settings("org.gnome.desktop.interface") {
                    if let Err(error) = interface.set_boolean("toolkit-accessibility", true) {
                        eprintln!("tuna-shell-gtk: toolkit accessibility unavailable: {error}");
                    }
                }
            }
            if let Some(control) = shell.borrow_mut().control.as_mut() {
                let _ = control.set_screen_reader(enabled);
            }
        })
    };
    publish();
    settings.connect_changed(Some("screen-reader-enabled"), move |_, _| publish());
    std::mem::forget(settings);
}
