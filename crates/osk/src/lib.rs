//! GNOME 51's on-screen keyboard (gnome-shell 51.0 `js/ui/keyboard.js`)
//! drawn with GTK (#349), shared by `tuna-shell-gtk` and `tuna-greeter`.
//!
//! The keys come from GNOME's own layouts ([`layout`]) and behave as
//! GNOME's `Keyboard` ([`model`]): levels, shift and its long-press latch,
//! long-press extended keys, the number levels, modifiers, the emoji pager
//! and the language menu. [`view::KeyboardView`] draws one keyboard;
//! [`prompt::PromptKeyboard`] is the keyboard of an authentication prompt
//! (the lock screen's and the login screen's), which edits only that
//! prompt. The shell's session keyboard, which types through the
//! compositor, stays in the shell.

pub mod layout;
pub mod model;
pub mod prompt;
pub mod view;
pub mod xkb;

use std::sync::Once;

/// The keyboard's style (gnome-shell 51.0 `_keyboard.scss`), for the
/// display's style provider.
pub const STYLE: &str = include_str!("style.css");

/// `include_bytes!` a build-script bundle from `OUT_DIR`, aligned so GLib
/// maps it in place instead of copying it.
macro_rules! bundled_resource {
    ($name:literal) => {{
        #[repr(C, align(16))]
        struct Aligned<T: ?Sized>(T);
        static BUNDLE: &Aligned<[u8]> =
            &Aligned(*include_bytes!(concat!(env!("OUT_DIR"), "/", $name)));
        &BUNDLE.0
    }};
}
#[cfg(test)]
pub(crate) use bundled_resource;

/// Register GNOME Shell's keyboard layouts and emoji list with GLib's
/// resources (once per process); the layouts load from there.
pub fn register_layouts() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let bundle = bundled_resource!("osk-layouts.gresource");
        match gio::Resource::from_data(&glib::Bytes::from_static(bundle)) {
            Ok(resource) => gio::resources_register(&resource),
            Err(error) => eprintln!("tuna-osk: keyboard layouts unavailable: {error}"),
        }
    });
}
