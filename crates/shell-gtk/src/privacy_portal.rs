//! Keep the portal lockdown publisher alive in the actual GTK session.
//! Bus connection and property round trips stay off the rendering thread.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use gtk4::prelude::*;
use tuna_shell_host::lockdown::LockdownBus;
use tuna_shell_host::settings::GioBackend;

pub fn start(app: &libadwaita::Application) {
    let stopping = Arc::new(AtomicBool::new(false));
    let shutdown = stopping.clone();
    app.connect_shutdown(move |_| shutdown.store(true, Ordering::Release));
    std::thread::Builder::new()
        .name("tuna-privacy-portal".into())
        .spawn(move || {
            let context = glib::MainContext::new();
            context
                .with_thread_default(|| {
                    let mut publisher = LockdownBus::new();
                    while !stopping.load(Ordering::Acquire) {
                        while context.pending() {
                            context.iteration(false);
                        }
                        // Seed the real policy before claiming the name, including
                        // a camera disabled before login. Never publish an initial
                        // all-allowed snapshot before reading persisted settings.
                        publisher.poll(&GioBackend);
                        publisher.ensure();
                        std::thread::sleep(Duration::from_millis(100));
                    }
                })
                .expect("privacy portal worker could not acquire its GLib context");
        })
        .unwrap_or_else(|error| panic!("privacy portal worker failed: {error}"));
}
