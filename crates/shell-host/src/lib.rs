//! Supervised shell host: Activities trigger and window list as a
//! separate Wayland client (001 spec R3/R5, ADR 0003).
//!
//! The binary entry point is `src/main.rs`. This library exposes the two
//! halves a later control-protocol adapter will join up:
//!
//! - [`model`] — plain shell-side view state (no dependency on any other
//!   workspace crate).
//! - [`panel`] — Wayland client setup for the top-anchored Activities
//!   panel surface.
//! - [`control`] — shell-side control-protocol client (Hello, snapshot,
//!   ordered changes with gap resnapshot, token activation commands).
//!
//! Stability: internal. This crate is not published and makes no SemVer
//! promise; see `docs/api-stability.md` in the repository.

/// Writes a complete, newline-terminated stderr record with one `write_all`
/// (see `tuna_compositor::stderr_record`).
#[doc(hidden)]
pub fn stderr_record(record: &str) -> std::io::Result<()> {
    std::io::Write::write_all(&mut std::io::stderr().lock(), record.as_bytes())
}

/// Shadows `std::eprintln!` here and in importing binaries: format one String
/// including the newline, then write it whole. Ignore closed-stderr errors.
#[macro_export]
macro_rules! eprintln {
    () => {
        $crate::eprintln!("")
    };
    ($($arg:tt)*) => {{
        let record = ::std::format!("{}\n", ::std::format_args!($($arg)*));
        let _ = $crate::stderr_record(&record);
    }};
}

pub mod apps;
pub mod control;
pub mod dock;
pub mod error;
pub mod extensions;

pub use error::ShellHostError;
pub mod favorites;
pub mod ibus;
pub mod ibus_properties;
pub mod icons;
pub mod intake;
pub mod introspect;
pub mod keyboard;
pub mod lockdown;
pub mod model;
pub mod notification_policy;
pub mod notifications;
pub mod overview;
pub mod panel;
pub mod popup;
pub mod prefs;
pub mod search;
pub mod settings;
pub mod tiles;
pub mod watcher;
pub mod xdg;
