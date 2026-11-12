//! The X requests the frames client makes, over GDK's own connection.
//!
//! Mutter's frames client calls Xlib on `gdk_x11_display_get_xdisplay`
//! and brackets every request that names another client's window with
//! `gdk_x11_display_error_trap_push`/`pop`: clients die at any time, and
//! an untrapped `BadWindow` aborts a GDK program. This module does the
//! same. It must be GDK's connection, not a second one:
//!
//! - the save set that keeps a client alive when the frames client dies
//!   belongs to the connection that created the frame window;
//! - the events arrive through GDK's `xevent` signal;
//! - GDK's error traps only cover requests made on its display.
//!
//! Xlib comes from `x11-dl` (the bindings gdk4-x11's `xlib` feature is
//! typed in), which loads `libX11` and `libXi` at run time; GTK's X11
//! backend links both, so they are present wherever this program can
//! open a display.
//!
//! One difference from Mutter in how errors are noticed: a request with
//! a reply (`XGetWindowProperty`, `XGetWMName`) reports failure through
//! its return value by the time it returns, so those are trapped without
//! the extra `XSync` round trip that `gdk_x11_display_error_trap_pop`
//! makes. Requests without a reply whose failure matters are still
//! synced, as in Mutter.

use crate::protocol::{self, Event, Property, Xid};
use gdk4_x11::x11::{xinput2, xlib};
use gdk4_x11::X11Display;
use std::ffi::CStr;
use std::os::raw::{c_char, c_int, c_long, c_uchar, c_uint, c_ulong};
use std::ptr;

/// The atoms the frames client names, interned once through GDK.
pub struct Atoms {
    pub mutter_needs_frame: Xid,
    pub mutter_frame_for: Xid,
    pub mutter_frame_extents: Xid,
    pub net_wm_visible_name: Xid,
    pub net_wm_name: Xid,
    pub motif_wm_hints: Xid,
    pub net_wm_state: Xid,
    pub net_wm_state_fullscreen: Xid,
    pub wm_protocols: Xid,
    pub wm_delete_window: Xid,
    pub utf8_string: Xid,
    pub xdnd_aware: Xid,
}

pub struct Server {
    display: X11Display,
    lib: xlib::Xlib,
    dpy: *mut xlib::Display,
    /// XInput 2 and its major opcode, when the server has it.
    xi: Option<(xinput2::XInput2, c_int)>,
    pub root: Xid,
    pub atoms: Atoms,
}

impl Server {
    pub fn open(display: &X11Display) -> Result<Self, String> {
        let lib = xlib::Xlib::open().map_err(|error| format!("cannot load Xlib: {error}"))?;
        // SAFETY: the pointer is GDK's open connection; `display`, kept
        // in the returned value, keeps it open.
        let dpy = unsafe { display.xdisplay() };
        let atom = |name: &str| gdk4_x11::x11_get_xatom_by_name_for_display(display, name);
        let atoms = Atoms {
            mutter_needs_frame: atom("_MUTTER_NEEDS_FRAME"),
            mutter_frame_for: atom("_MUTTER_FRAME_FOR"),
            mutter_frame_extents: atom("_MUTTER_FRAME_EXTENTS"),
            net_wm_visible_name: atom("_NET_WM_VISIBLE_NAME"),
            net_wm_name: atom("_NET_WM_NAME"),
            motif_wm_hints: atom("_MOTIF_WM_HINTS"),
            net_wm_state: atom("_NET_WM_STATE"),
            net_wm_state_fullscreen: atom("_NET_WM_STATE_FULLSCREEN"),
            wm_protocols: atom("WM_PROTOCOLS"),
            wm_delete_window: atom("WM_DELETE_WINDOW"),
            utf8_string: atom("UTF8_STRING"),
            xdnd_aware: atom("XdndAware"),
        };
        let xi = xinput2::XInput2::open().ok().and_then(|xi| {
            let (mut opcode, mut event, mut error) = (0, 0, 0);
            // SAFETY: `dpy` is open and the out-pointers are live locals.
            let present = unsafe {
                (lib.XQueryExtension)(
                    dpy,
                    c"XInputExtension".as_ptr(),
                    &mut opcode,
                    &mut event,
                    &mut error,
                )
            };
            (present != 0).then_some((xi, opcode))
        });
        Ok(Self {
            display: display.clone(),
            lib,
            dpy,
            xi,
            root: display.xrootwindow(),
            atoms,
        })
    }

    /// Makes `requests` with X errors trapped, then waits for the server
    /// to have processed them: `None` when it rejected one.
    fn checked<T>(&self, requests: impl FnOnce() -> T) -> Option<T> {
        self.display.error_trap_push();
        let value = requests();
        (self.display.error_trap_pop() == 0).then_some(value)
    }

    /// Makes `requests` with X errors ignored, without a round trip.
    fn unchecked<T>(&self, requests: impl FnOnce() -> T) -> T {
        self.display.error_trap_push();
        let value = requests();
        self.display.error_trap_pop_ignored();
        value
    }

    /// `XGetWindowProperty` from offset 0, never deleting: `None` when
    /// the request failed (the window is gone).
    pub fn property(
        &self,
        window: Xid,
        property: Xid,
        long_length: c_long,
        type_: Xid,
    ) -> Option<Property> {
        let mut actual_type: Xid = 0;
        let mut format: c_int = 0;
        let mut items: c_ulong = 0;
        let mut bytes_after: c_ulong = 0;
        let mut data: *mut c_uchar = ptr::null_mut();
        // SAFETY: `dpy` is open and the out-pointers are live locals.
        let status = self.unchecked(|| unsafe {
            (self.lib.XGetWindowProperty)(
                self.dpy,
                window,
                property,
                0,
                long_length,
                xlib::False,
                type_,
                &mut actual_type,
                &mut format,
                &mut items,
                &mut bytes_after,
                &mut data,
            )
        });
        if status != c_int::from(xlib::Success) {
            return None;
        }
        let mut reply = Property {
            type_: actual_type,
            format,
            ..Property::default()
        };
        if !data.is_null() {
            let items = items as usize;
            // SAFETY: Xlib allocated `data` to hold `items` items of the
            // reported format: C `long`s for 32, bytes for 8.
            unsafe {
                match format {
                    32 => {
                        reply.longs = std::slice::from_raw_parts(data.cast::<c_ulong>(), items)
                            .iter()
                            .map(|item| *item as u32)
                            .collect();
                    }
                    8 => reply.bytes = std::slice::from_raw_parts(data, items).to_vec(),
                    _ => {}
                }
                (self.lib.XFree)(data.cast());
            }
        }
        Some(reply)
    }

    /// `meta_window_tracker_constructed`: ask for the root's structure,
    /// substructure and property events. The mask is Mutter's.
    pub fn select_root_events(&self) {
        // SAFETY: `dpy` is open; the root window always exists.
        unsafe {
            (self.lib.XSelectInput)(
                self.dpy,
                self.root,
                xlib::KeyPressMask
                    | xlib::PropertyChangeMask
                    | xlib::StructureNotifyMask
                    | xlib::SubstructureNotifyMask,
            );
        }
    }

    /// `meta_window_tracker_constructed`: the root's current children
    /// that are not override-redirect.
    pub fn managed_children(&self) -> Option<Vec<Xid>> {
        let (mut root, mut parent): (Xid, Xid) = (0, 0);
        let mut children: *mut Xid = ptr::null_mut();
        let mut count: c_uint = 0;
        // SAFETY: `dpy` is open and the out-pointers are live locals.
        self.checked(|| unsafe {
            (self.lib.XQueryTree)(
                self.dpy,
                self.root,
                &mut root,
                &mut parent,
                &mut children,
                &mut count,
            )
        })?;
        if children.is_null() {
            return Some(Vec::new());
        }
        // SAFETY: Xlib allocated `children` to hold `count` windows.
        let windows = unsafe {
            let windows = std::slice::from_raw_parts(children, count as usize).to_vec();
            (self.lib.XFree)(children.cast());
            windows
        };
        Some(
            windows
                .into_iter()
                .filter(|window| self.override_redirect(*window) == Some(false))
                .collect(),
        )
    }

    /// `None` when the window is gone.
    fn override_redirect(&self, window: Xid) -> Option<bool> {
        // SAFETY: an all-zero XWindowAttributes is a valid value for
        // Xlib to overwrite.
        let mut attributes: xlib::XWindowAttributes = unsafe { std::mem::zeroed() };
        // SAFETY: `dpy` is open and `attributes` is a live local.
        self.checked(|| unsafe {
            (self.lib.XGetWindowAttributes)(self.dpy, window, &mut attributes)
        })?;
        Some(attributes.override_redirect != 0)
    }

    /// `listen_set_up_frame`: watch the window's properties and its
    /// destruction, and read whether it asks for a frame now. `false`
    /// also when the window is already gone.
    pub fn listen(&self, window: Xid) -> bool {
        self.checked(|| {
            // SAFETY: `dpy` is open; a dead `window` is a trapped error.
            unsafe {
                (self.lib.XSelectInput)(
                    self.dpy,
                    window,
                    xlib::PropertyChangeMask | xlib::StructureNotifyMask,
                );
            }
            self.property(window, self.atoms.mutter_needs_frame, 1, xlib::XA_CARDINAL)
        })
        .flatten()
        .is_some_and(|property| protocol::needs_frame(&property))
    }

    /// `set_up_frame`: keep the client alive should this process die
    /// while the client is inside the frame, and tell the window manager
    /// which client the frame window is for. `false` when the client is
    /// gone.
    pub fn adopt(&self, client: Xid, frame_window: Xid) -> bool {
        let data: [c_ulong; 1] = [client];
        // SAFETY: `dpy` is open; format 32 data is an array of C longs.
        self.checked(|| unsafe {
            (self.lib.XAddToSaveSet)(self.dpy, client);
            (self.lib.XChangeProperty)(
                self.dpy,
                frame_window,
                self.atoms.mutter_frame_for,
                xlib::XA_WINDOW,
                32,
                xlib::PropModeReplace,
                data.as_ptr().cast(),
                1,
            );
        })
        .is_some()
    }

    /// `remove_frame`: the client no longer lives in one of our frames.
    pub fn release(&self, client: Xid) {
        // SAFETY: `dpy` is open; a dead `client` is a trapped error.
        self.unchecked(|| unsafe {
            (self.lib.XRemoveFromSaveSet)(self.dpy, client);
        });
    }

    /// `update_extents`: publish `_MUTTER_FRAME_EXTENTS` on the frame.
    pub fn set_frame_extents(&self, frame_window: Xid, extents: protocol::Extents) {
        let data = extents.cardinals();
        // SAFETY: `dpy` is open; format 32 data is an array of C longs.
        self.unchecked(|| unsafe {
            (self.lib.XChangeProperty)(
                self.dpy,
                frame_window,
                self.atoms.mutter_frame_extents,
                xlib::XA_CARDINAL,
                32,
                xlib::PropModeReplace,
                data.as_ptr().cast(),
                data.len() as c_int,
            );
        });
    }

    /// `meta_frame_new`: GDK announces every toplevel as a drop target;
    /// on a frame that leads clients to drop on the frame rather than
    /// on the window inside it.
    pub fn disable_xdnd(&self, frame_window: Xid) {
        // SAFETY: `dpy` is open and the frame window is ours.
        unsafe {
            (self.lib.XDeleteProperty)(self.dpy, frame_window, self.atoms.xdnd_aware);
        }
    }

    /// `frame_sync_wm_name`: the client's `WM_NAME`. The outer `None`
    /// means there is none to read and the frame keeps what it had; the
    /// inner one that the text does not convert to UTF-8.
    pub fn wm_name(&self, client: Xid) -> Option<Option<String>> {
        let mut text = xlib::XTextProperty {
            value: ptr::null_mut(),
            encoding: 0,
            format: 0,
            nitems: 0,
        };
        // SAFETY: `dpy` is open and `text` is a live local.
        let status =
            self.unchecked(|| unsafe { (self.lib.XGetWMName)(self.dpy, client, &mut text) });
        if status == 0 {
            return None;
        }
        let name = self.text_property_to_utf8(&text);
        // SAFETY: a successful XGetWMName allocated `value`.
        unsafe {
            (self.lib.XFree)(text.value.cast());
        }
        Some(name)
    }

    /// `text_property_to_utf8`: the first string of a text property,
    /// converted by Xlib for the current locale, if that is UTF-8.
    fn text_property_to_utf8(&self, text: &xlib::XTextProperty) -> Option<String> {
        // Xutil.h's conversion failures; x11-dl does not carry them.
        const X_NO_MEMORY: c_int = -1;
        const X_LOCALE_NOT_SUPPORTED: c_int = -2;
        const X_CONVERTER_NOT_FOUND: c_int = -3;

        let mut list: *mut *mut c_char = ptr::null_mut();
        let mut count: c_int = 0;
        // SAFETY: `dpy` is open, `text` came from Xlib, and the
        // out-pointers are live locals.
        let result =
            unsafe { (self.lib.XmbTextPropertyToTextList)(self.dpy, text, &mut list, &mut count) };
        let failed = matches!(
            result,
            X_NO_MEMORY | X_LOCALE_NOT_SUPPORTED | X_CONVERTER_NOT_FOUND
        );
        let name = if failed || count == 0 || list.is_null() {
            None
        } else {
            // SAFETY: `list` holds `count` (at least one) C strings.
            unsafe { CStr::from_ptr(*list) }
                .to_str()
                .ok()
                .map(str::to_owned)
        };
        if !list.is_null() {
            // SAFETY: Xlib allocated `list`.
            unsafe {
                (self.lib.XFreeStringList)(list);
            }
        }
        name
    }

    /// `on_frame_close_request`: ask the client to close its window if
    /// it takes part in `WM_DELETE_WINDOW`, else disconnect it.
    pub fn close(&self, client: Xid) {
        let protocols = self.property(client, self.atoms.wm_protocols, c_long::MAX, xlib::XA_ATOM);
        let polite = protocol::has_wm_protocol(protocols.as_ref(), self.atoms.wm_delete_window);
        // SAFETY: `dpy` is open; a dead `client` is a trapped error.
        self.unchecked(|| unsafe {
            if polite {
                let mut data = xlib::ClientMessageData::new();
                data.set_long(0, self.atoms.wm_delete_window as c_long);
                // No timestamp, as in Mutter ("FIXME: missing timestamp").
                data.set_long(1, 0);
                let mut event = xlib::XEvent::from(xlib::XClientMessageEvent {
                    type_: xlib::ClientMessage,
                    serial: 0,
                    send_event: xlib::False,
                    display: self.dpy,
                    window: client,
                    message_type: self.atoms.wm_protocols,
                    format: 32,
                    data,
                });
                (self.lib.XSendEvent)(self.dpy, client, xlib::False, 0, &mut event);
            } else {
                (self.lib.XKillClient)(self.dpy, client);
            }
        });
    }

    /// `on_xevent`'s XInput branch: the device and window of a pointer
    /// leaving `event`'s window for a window inside it.
    pub fn leave_to_inferior(&self, event: &xlib::XEvent) -> Option<(c_int, Xid)> {
        let (_, opcode) = self.xi.as_ref()?;
        if event.get_type() != xlib::GenericEvent {
            return None;
        }
        // SAFETY: the type says the event is a generic event cookie. GDK
        // fetched its data (XGetEventData) before emitting `xevent`; an
        // XI_Leave's data is an XILeaveEvent.
        unsafe {
            let cookie = event.generic_event_cookie;
            if cookie.extension != *opcode
                || cookie.evtype != xinput2::XI_Leave
                || cookie.data.is_null()
            {
                return None;
            }
            let crossing = &*cookie.data.cast::<xinput2::XILeaveEvent>();
            (crossing.detail == xinput2::XINotifyInferior)
                .then_some((crossing.deviceid, crossing.event))
        }
    }

    /// `on_xevent`'s XInput branch: drop the cursor GTK set on the frame
    /// window for `device`, so the client's own cursor shows over it.
    pub fn undefine_cursor(&self, device: c_int, frame_window: Xid) {
        let Some((xi, _)) = &self.xi else {
            return;
        };
        // SAFETY: `dpy` is open; a removed device is a trapped error.
        self.unchecked(|| unsafe {
            (xi.XIUndefineCursor)(self.dpy, device, frame_window);
        });
    }

    /// `main`: `XFixesSetClientDisconnectMode (Terminate)`. An X server
    /// that exits when its last client leaves (`Xwayland -terminate`)
    /// must not count the frames client as a reason to stay.
    ///
    /// The call is XFixes 6; `x11-dl` predates it, so the symbol is
    /// looked up in the libXfixes that GTK already loaded. An older
    /// library has no such mode to set.
    pub fn terminate_with_server(&self) {
        const XFIXES_CLIENT_DISCONNECT_FLAG_TERMINATE: c_int = 1 << 0;
        type SetClientDisconnectMode = unsafe extern "C" fn(*mut xlib::Display, c_int);
        // SAFETY: the symbol, when present, has libXfixes' documented
        // signature; the library stays loaded for the process's life.
        unsafe {
            let library = libc::dlopen(c"libXfixes.so.3".as_ptr(), libc::RTLD_LAZY);
            if library.is_null() {
                return;
            }
            let symbol = libc::dlsym(library, c"XFixesSetClientDisconnectMode".as_ptr());
            if symbol.is_null() {
                return;
            }
            let set_mode =
                std::mem::transmute::<*mut libc::c_void, SetClientDisconnectMode>(symbol);
            set_mode(self.dpy, XFIXES_CLIENT_DISCONNECT_FLAG_TERMINATE);
        }
    }
}

/// The part of an X event the tracker dispatches on.
pub fn decode(event: &xlib::XEvent) -> Option<Event> {
    // SAFETY: the event's type says which member of the union Xlib
    // filled in.
    unsafe {
        match event.get_type() {
            xlib::CreateNotify => {
                let event = event.create_window;
                Some(Event::Create {
                    parent: event.parent,
                    window: event.window,
                    override_redirect: event.override_redirect != 0,
                })
            }
            xlib::ConfigureNotify => {
                let event = event.configure;
                Some(Event::Configure {
                    event: event.event,
                    window: event.window,
                    override_redirect: event.override_redirect != 0,
                })
            }
            xlib::DestroyNotify => Some(Event::Destroy {
                window: event.destroy_window.window,
            }),
            xlib::PropertyNotify => {
                let event = event.property;
                Some(Event::Property {
                    window: event.window,
                    atom: event.atom,
                    deleted: event.state == xlib::PropertyDelete,
                })
            }
            _ => None,
        }
    }
}
