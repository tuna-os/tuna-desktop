//! One frame window (Mutter 51 `src/frames/meta-frame.c`): a
//! `GtkWindow` with the CSS class `ssd-frame`, a [`FrameHeader`] as its
//! title bar and a [`FrameContent`] placeholder as its child.
//!
//! What the frame mirrors from the client window:
//!
//! | Client property | Frame | Mutter function |
//! |---|---|---|
//! | `_NET_WM_VISIBLE_NAME`, `_NET_WM_NAME`, `WM_NAME` | title | `frame_sync_net_wm_visible_name`, `frame_sync_net_wm_name`, `frame_sync_wm_name` |
//! | `_MOTIF_WM_HINTS` functions | deletable (close button) | `frame_sync_motif_wm_hints` |
//! | `WM_NORMAL_HINTS` minimum = maximum size | resizable (maximize button, resize edges) | `frame_sync_wm_normal_hints` |
//! | `_NET_WM_STATE` fullscreen, read once at creation | fullscreen, no title bar | `frame_sync_wm_state` |
//!
//! What the frame publishes on its own window:
//!
//! - `_MUTTER_FRAME_EXTENTS`: the visible borders around the client
//!   (`update_extents`), first from the header's height, then from the
//!   content widget's allocated position (`meta_frame_size_allocate`).
//! - `_GTK_FRAME_EXTENTS`: the invisible border (shadow and resize
//!   area). GDK writes this itself for any window with client-side
//!   decorations; the frame does nothing for it, as in Mutter.
//!
//! What the frame does not do, as in Mutter:
//!
//! - It never sizes or moves itself after the client. The window manager
//!   resizes the frame window and GTK lays the header out in whatever
//!   size arrives.
//! - It does not read `_NET_WM_ALLOWED_ACTIONS` or the Motif resize
//!   function.
//! - Focus, maximized and tiled looks come from the `_NET_WM_STATE` the
//!   window manager writes on the frame window, which GDK reads like any
//!   toplevel's.
//!
//! The close button is the one action that does not go through the
//! window manager (`on_frame_close_request`): the frame sends
//! `WM_DELETE_WINDOW` to the client itself, or kills a client that does
//! not take part in that protocol.

use crate::content::FrameContent;
use crate::header::FrameHeader;
use crate::protocol::{self, Extents, Title, Xid};
use crate::server::Server;
use gdk4_x11::x11::xlib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk4 as gtk;
use gtk4::glib;
use std::cell::{Cell, OnceCell, RefCell};
use std::os::raw::c_long;
use std::rc::Rc;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Frame {
        pub server: OnceCell<Rc<Server>>,
        /// The frame's X window, once realized.
        pub window: Cell<Xid>,
        /// The extents last published.
        pub extents: Cell<Extents>,
        pub title: RefCell<Title>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Frame {
        const NAME: &'static str = "TunaX11Frame";
        type Type = super::Frame;
        type ParentType = gtk::Window;
    }

    impl ObjectImpl for Frame {
        /// `meta_frame_init`.
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().add_css_class("ssd-frame");
        }
    }

    impl WidgetImpl for Frame {
        /// `meta_frame_size_allocate`.
        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.parent_size_allocate(width, height, baseline);

            let obj = self.obj();
            let Some(content) = obj.child() else {
                return;
            };
            let Some(origin) = content.compute_point(&*obj, &gtk::graphene::Point::zero()) else {
                return;
            };
            let Some(surface) = obj.surface() else {
                return;
            };
            let extents =
                Extents::for_content_origin(origin.x(), origin.y(), surface.scale_factor());
            if extents != self.extents.get() {
                obj.update_extents(extents);
            }
        }
    }

    impl WindowImpl for Frame {
        /// `on_frame_close_request`: the close button closes the client,
        /// not the frame. The frame goes when the client's window does.
        fn close_request(&self) -> glib::Propagation {
            let (Some(server), Some(client)) = (self.server.get(), self.obj().client()) else {
                return self.parent_close_request();
            };
            server.close(client);
            glib::Propagation::Stop
        }
    }
}

glib::wrapper! {
    pub struct Frame(ObjectSubclass<imp::Frame>)
        @extends gtk::Window, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Native,
            gtk::Root, gtk::ShortcutManager;
}

impl Frame {
    /// `meta_frame_new`: a realized, not yet shown frame for `client`,
    /// with its extents published and the client's properties mirrored.
    pub fn new(server: &Rc<Server>, client: Xid) -> Option<Self> {
        let frame: Self = glib::Object::new();
        // A fresh cell: this cannot already be set.
        let _ = frame.imp().server.set(server.clone());

        let header = FrameHeader::default();
        frame.set_titlebar(Some(&header));
        frame.set_child(Some(&FrameContent::new(client)));

        WidgetExt::realize(&frame);
        let surface = frame
            .surface()
            .and_then(|surface| surface.downcast::<gdk4_x11::X11Surface>().ok());
        let Some(surface) = surface else {
            frame.destroy();
            return None;
        };
        surface.set_frame_sync_enabled(true);
        frame.imp().window.set(surface.xid());

        frame.sync_wm_state(server, client);

        let header_height = if frame.is_fullscreen() {
            0
        } else {
            header.measure(gtk::Orientation::Vertical, 1).0
        };
        frame.update_extents(Extents::for_header(header_height, surface.scale_factor()));

        frame.sync_net_wm_visible_name(server, client);
        frame.sync_net_wm_name(server, client);
        frame.sync_wm_name(server, client);
        frame.sync_motif_wm_hints(server, client);
        frame.sync_wm_normal_hints(server, client);

        server.disable_xdnd(surface.xid());

        Some(frame)
    }

    /// The frame's X window.
    pub fn window(&self) -> Xid {
        self.imp().window.get()
    }

    /// The client window this frame is for.
    fn client(&self) -> Option<Xid> {
        self.child()
            .and_downcast::<FrameContent>()
            .map(|content| content.window())
    }

    /// `meta_frame_handle_xevent`: a property changed on `window`. Only
    /// the client's properties are mirrored; changes on the frame's own
    /// window are GDK's business.
    pub fn property_changed(&self, window: Xid, atom: Xid) {
        let Some(server) = self.imp().server.get() else {
            return;
        };
        if self.client() != Some(window) {
            return;
        }
        let atoms = &server.atoms;
        if atom == atoms.net_wm_visible_name {
            self.sync_net_wm_visible_name(server, window);
        } else if atom == atoms.net_wm_name {
            self.sync_net_wm_name(server, window);
        } else if atom == xlib::XA_WM_NAME {
            self.sync_wm_name(server, window);
        } else if atom == atoms.motif_wm_hints {
            self.sync_motif_wm_hints(server, window);
        } else if atom == xlib::XA_WM_NORMAL_HINTS {
            self.sync_wm_normal_hints(server, window);
        }
    }

    /// `update_extents`.
    fn update_extents(&self, extents: Extents) {
        let imp = self.imp();
        let (Some(server), window) = (imp.server.get(), imp.window.get()) else {
            return;
        };
        if window == 0 {
            return;
        }
        server.set_frame_extents(window, extents);
        imp.extents.set(extents);
    }

    /// `update_frame_title`, after `change` took a newly read name.
    fn update_title(&self, change: impl FnOnce(&mut Title)) {
        let text = {
            let mut title = self.imp().title.borrow_mut();
            change(&mut title);
            title.text().to_owned()
        };
        self.set_title(Some(&text));
    }

    /// `get_utf8_string_prop`.
    fn utf8_string(server: &Server, client: Xid, property: Xid) -> Option<String> {
        server
            .property(client, property, c_long::MAX, server.atoms.utf8_string)
            .and_then(|property| protocol::utf8_string(&property))
    }

    /// `frame_sync_net_wm_visible_name`.
    fn sync_net_wm_visible_name(&self, server: &Server, client: Xid) {
        let name = Self::utf8_string(server, client, server.atoms.net_wm_visible_name);
        self.update_title(|title| title.set_net_wm_visible_name(name));
    }

    /// `frame_sync_net_wm_name`.
    fn sync_net_wm_name(&self, server: &Server, client: Xid) {
        let name = Self::utf8_string(server, client, server.atoms.net_wm_name);
        self.update_title(|title| title.set_net_wm_name(name));
    }

    /// `frame_sync_wm_name`.
    fn sync_wm_name(&self, server: &Server, client: Xid) {
        if let Some(name) = server.wm_name(client) {
            self.update_title(|title| title.set_wm_name(name));
        }
    }

    /// `frame_sync_motif_wm_hints`.
    fn sync_motif_wm_hints(&self, server: &Server, client: Xid) {
        let hints = server.property(
            client,
            server.atoms.motif_wm_hints,
            protocol::MOTIF_WM_HINTS_LONGS,
            xlib::AnyPropertyType as Xid,
        );
        if let Some(hints) = hints {
            self.set_deletable(protocol::deletable(&hints));
        }
    }

    /// `frame_sync_wm_normal_hints`.
    fn sync_wm_normal_hints(&self, server: &Server, client: Xid) {
        let resizable = server
            .property(
                client,
                xlib::XA_WM_NORMAL_HINTS,
                protocol::SIZE_HINTS_LONGS,
                xlib::XA_WM_SIZE_HINTS,
            )
            .and_then(|hints| protocol::resizable(&hints, xlib::XA_WM_SIZE_HINTS));
        if let Some(resizable) = resizable {
            self.set_resizable(resizable);
        }
    }

    /// `frame_sync_wm_state`.
    fn sync_wm_state(&self, server: &Server, client: Xid) {
        let fullscreen = server
            .property(client, server.atoms.net_wm_state, 32, xlib::XA_ATOM)
            .is_some_and(|state| {
                protocol::lists_atom(&state, server.atoms.net_wm_state_fullscreen)
            });
        if fullscreen {
            self.fullscreen();
        }
    }
}
