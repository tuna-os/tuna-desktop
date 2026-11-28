//! tuna-x11-frames: title bars for X11 windows.
//!
//! X11 applications expect the window manager to draw their title bar.
//! GNOME does this with a helper, `mutter-x11-frames` (Mutter 51
//! `src/frames/`): an X11 GTK program that makes one frame window per
//! client, into which the window manager reparents the client. The
//! title bar is then part of the window's own surface, and it is a real
//! GTK header bar. This program is that helper for Tuna Desktop, written
//! after Mutter's and speaking the same protocol, so either helper works
//! with either window manager:
//!
//! | Property | On | Set by | Meaning |
//! |---|---|---|---|
//! | `_MUTTER_NEEDS_FRAME` (`CARDINAL`) | client | window manager | this window wants a frame |
//! | `_MUTTER_FRAME_FOR` (`WINDOW`) | frame | frames client, before mapping | the client this frame is for |
//! | `_MUTTER_FRAME_EXTENTS` (`CARDINAL` ×4: left, right, top, bottom) | frame | frames client | visible borders around the client |
//! | `_GTK_FRAME_EXTENTS` (`CARDINAL` ×4) | frame | GDK | invisible borders (shadow, resize area) |
//!
//! | Mutter file | Here |
//! |---|---|
//! | `main.c` | this file |
//! | `meta-window-tracker.c` | `tracker.rs` |
//! | `meta-frame.c` | `frame.rs` |
//! | `meta-frame-header.c` | `header.rs` |
//! | `meta-frame-content.c` | `content.rs` |
//!
//! `protocol.rs` holds the logic of those files that needs no X server;
//! `server.rs` makes their X requests.
//!
//! Differences from Mutter's `main.c`:
//!
//! - libadwaita is always initialized. Mutter loads it only when
//!   `XDG_CURRENT_DESKTOP` lists GNOME (a Tuna session's `Tuna:GNOME`
//!   does) or `MUTTER_FRAMES_PLATFORM_LIBRARY=adwaita`, and otherwise
//!   follows the colour scheme itself. That choice is not carried over.
//! - GTK's and libadwaita's settings portal is turned off, as in the
//!   shell (`crates/shell-gtk/src/portal_opt_out.rs`): both would call
//!   xdg-desktop-portal synchronously during start-up, which can hold
//!   the process for the D-Bus timeout while X11 windows wait for their
//!   frames. Without it GTK reads XSETTINGS and libadwaita reads
//!   GSettings, which is what the portal would have answered from.
//! - `SIGTERM` ends the process with status 0 straight from the signal
//!   handler, not from the main loop. The X server cleans up either way:
//!   closing the connection destroys the frames and the save set returns
//!   their clients to the root window.
//! - `--version` prints the release version, like every Tuna binary.
//!
//! Mutter's source is GPL-2.0-or-later, © Red Hat (author Carlos
//! Garnacho); this adaptation is distributed under the workspace's
//! GPL-3.0-or-later.

/// Writes a complete, newline-terminated stderr record with one
/// `write_all`, so this process cannot split another's record on the
/// session log.
fn stderr_record(record: &str) -> std::io::Result<()> {
    std::io::Write::write_all(&mut std::io::stderr().lock(), record.as_bytes())
}

/// Shadows `std::eprintln!` in every module: format one String including
/// the newline, then write it whole. Ignore closed-stderr errors.
macro_rules! eprintln {
    () => {
        eprintln!("")
    };
    ($($arg:tt)*) => {{
        let record = ::std::format!("{}\n", ::std::format_args!($($arg)*));
        let _ = $crate::stderr_record(&record);
    }};
}

mod content;
mod frame;
mod header;
mod protocol;
mod server;
mod tracker;

use gtk::prelude::*;
use gtk::{gdk, glib};
use gtk4 as gtk;
use libadwaita as adw;
use std::ffi::{OsStr, OsString};
use std::process::ExitCode;
use std::rc::Rc;

const NAME: &str = "tuna-x11-frames";

fn release_version() -> &'static str {
    match option_env!("TUNA_VERSION") {
        Some(v) if !v.is_empty() => v.strip_prefix('v').unwrap_or(v),
        _ => env!("CARGO_PKG_VERSION"),
    }
}

fn main() -> ExitCode {
    // Before anything that needs a display: packaging checks run this
    // where there is none.
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!("{NAME} {}", release_version());
        return ExitCode::SUCCESS;
    }
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{NAME}: {error}");
            ExitCode::FAILURE
        }
    }
}

/// `GDK_DEBUG` with `no-portals` added to any flags the user set.
fn with_no_portals(existing: Option<&OsStr>) -> OsString {
    match existing.map(OsStr::to_string_lossy) {
        Some(flags) if !flags.trim().is_empty() => format!("{flags},no-portals").into(),
        _ => "no-portals".into(),
    }
}

/// Mutter's `main`.
fn run() -> Result<(), String> {
    // Frames are small and mostly static, and Xwayland composites them
    // into the client's surface anyway.
    std::env::set_var("GSK_RENDERER", "cairo");
    // This program only makes sense on X11: the session's GDK_BACKEND
    // (wayland, for everything else) must not apply.
    std::env::remove_var("GDK_BACKEND");
    // No synchronous portal calls while windows wait for their frames.
    let gdk_debug = std::env::var_os("GDK_DEBUG");
    std::env::set_var("GDK_DEBUG", with_no_portals(gdk_debug.as_deref()));
    std::env::set_var("ADW_DISABLE_PORTAL", "1");
    gdk::set_allowed_backends("x11");
    glib::set_prgname(Some(NAME));

    gtk::init().map_err(|error| format!("cannot open an X11 display: {error}"))?;
    let display = gdk::Display::default()
        .and_then(|display| display.downcast::<gdk4_x11::X11Display>().ok())
        .ok_or("the display is not an X11 display")?;
    adw::init().map_err(|error| format!("cannot initialize libadwaita: {error}"))?;

    let server = Rc::new(server::Server::open(&display)?);
    server.terminate_with_server();
    tracker::Tracker::start(server, &display);

    exit_on_sigterm();
    glib::MainLoop::new(None, false).run();
    Ok(())
}

/// Mutter's `on_sigterm`: the session stopping this helper is not a
/// failure.
fn exit_on_sigterm() {
    extern "C" fn on_sigterm(_signal: libc::c_int) {
        // SAFETY: `_exit` is async-signal-safe.
        unsafe { libc::_exit(0) }
    }
    // SAFETY: the handler only calls `_exit`.
    unsafe {
        libc::signal(libc::SIGTERM, on_sigterm as *const () as libc::sighandler_t);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_portals_joins_the_users_gdk_debug_flags() {
        assert_eq!(with_no_portals(None), "no-portals");
        assert_eq!(with_no_portals(Some(OsStr::new(" "))), "no-portals");
        assert_eq!(
            with_no_portals(Some(OsStr::new("events,dnd"))),
            "events,dnd,no-portals"
        );
    }
}
