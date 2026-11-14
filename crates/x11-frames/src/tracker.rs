//! Which windows have frames (Mutter 51
//! `src/frames/meta-window-tracker.c`).
//!
//! The tracker watches the root window's children. A window whose
//! `_MUTTER_NEEDS_FRAME` property is set gets a frame (`set_up_frame`):
//!
//! 1. a frame window is created and realized, not yet mapped;
//! 2. the client is added to this connection's save set, so that the X
//!    server hands it back to the root if this process dies with the
//!    client inside a frame;
//! 3. `_MUTTER_FRAME_FOR` on the frame window names the client;
//! 4. the frame is shown. The window manager sees the map request for a
//!    window carrying `_MUTTER_FRAME_FOR`, reparents the client into it
//!    and maps both.
//!
//! The frame goes (`remove_frame`) when the property is deleted, when
//! the client is destroyed, or when it turns override-redirect. The
//! window manager reparents a living client out of the frame before it
//! deletes the property: destroying the frame window destroys whatever
//! is still inside it.
//!
//! A window is noticed in one of three ways, as in Mutter: it is a child
//! of the root when the tracker starts; it is created as a child of the
//! root later; or a `ConfigureNotify` for it arrives on the root window.
//! The last is also how a window manager that keeps its clients inside
//! windows of its own (not children of the root) offers them to a
//! tracker that started late: it sends that event itself. Synthetic
//! events are therefore taken like real ones, and an offer for a window
//! that already has a frame does nothing.
//!
//! The decisions are [`protocol::classify`]; this module carries them
//! out.
//!
//! Differences from Mutter:
//!
//! - Mutter follows `org.gnome.desktop.interface color-scheme` here when
//!   libadwaita is not loaded. This program always initializes
//!   libadwaita (see `main.rs`), whose style manager follows it.
//! - Mutter calls `XIQueryVersion` to find XInput's opcode; GDK has
//!   already negotiated the version on this connection, so only the
//!   opcode is looked up (`Server::open`).

use crate::frame::Frame;
use crate::protocol::{self, Action, Registry, Xid};
use crate::server::{self, Server};
use gdk4_x11::x11::xlib;
use gdk4_x11::X11Display;
use gtk::prelude::*;
use gtk4 as gtk;
use gtk4::glib;
use std::cell::RefCell;
use std::rc::Rc;

pub struct Tracker {
    server: Rc<Server>,
    frames: RefCell<Registry<Frame>>,
}

impl Tracker {
    /// `meta_window_tracker_constructed`: select the root's events,
    /// take every X event GDK reads from now on, and frame the windows
    /// that already ask for it. The tracker lives as long as `display`.
    pub fn start(server: Rc<Server>, display: &X11Display) {
        let tracker = Rc::new(Self {
            server,
            frames: RefCell::default(),
        });
        tracker.server.select_root_events();

        let handler = tracker.clone();
        // SAFETY: GDK emits `xevent` with a pointer to the event it is
        // translating, valid for the emission.
        unsafe {
            display.connect_xevent(move |_, event| {
                handler.on_xevent(&*event);
                glib::Propagation::Proceed
            });
        }

        match tracker.server.managed_children() {
            Some(windows) => {
                for window in windows {
                    tracker.listen_set_up_frame(window);
                }
            }
            None => eprintln!("tuna-x11-frames: could not query existing windows"),
        }
    }

    /// `on_xevent`.
    fn on_xevent(&self, event: &xlib::XEvent) {
        // When the pointer crosses from the frame to the client window
        // inside it, the cursor GTK set on the frame (a resize arrow,
        // say) must not stay.
        if let Some((device, window)) = self.server.leave_to_inferior(event) {
            if self.frames.borrow().is_frame(window) {
                self.server.undefine_cursor(device, window);
            }
            return;
        }
        let Some(event) = server::decode(event) else {
            return;
        };
        let action = protocol::classify(
            event,
            self.server.root,
            self.server.atoms.mutter_needs_frame,
            &self.frames.borrow(),
        );
        match action {
            Action::Listen(window) => self.listen_set_up_frame(window),
            Action::SetUp(window) => self.set_up_frame(window),
            Action::Remove(window) => self.remove_frame(window),
            Action::Sync { window, atom } => {
                let frame = self.frames.borrow().frame_for(window).cloned();
                if let Some(frame) = frame {
                    frame.property_changed(window, atom);
                }
            }
            Action::Ignore => {}
        }
    }

    /// `listen_set_up_frame`.
    fn listen_set_up_frame(&self, window: Xid) {
        if self.server.listen(window) {
            self.set_up_frame(window);
        }
    }

    /// `set_up_frame`.
    fn set_up_frame(&self, client: Xid) {
        {
            let frames = self.frames.borrow();
            // The second test: never frame one of our own frames.
            if frames.has_frame(client) || frames.is_frame(client) {
                return;
            }
        }
        let Some(frame) = Frame::new(&self.server, client) else {
            return;
        };
        if !self.server.adopt(client, frame.window()) {
            // The client went away while its frame was being made.
            frame.destroy();
            return;
        }
        self.frames
            .borrow_mut()
            .insert(client, frame.window(), frame.clone());
        frame.set_visible(true);
    }

    /// `remove_frame`.
    fn remove_frame(&self, client: Xid) {
        if !self.frames.borrow().has_frame(client) {
            return;
        }
        self.server.release(client);
        let frame = self.frames.borrow_mut().remove(client);
        if let Some(frame) = frame {
            frame.destroy();
        }
    }
}
