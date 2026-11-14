//! The frame's title bar (Mutter 51 `src/frames/meta-frame-header.c`):
//! a stock `GtkHeaderBar` inside a wrapper that lets the frame become
//! narrower than the header bar's minimum width.
//!
//! Everything a user sees or presses is GTK's own:
//!
//! - the title label follows the frame window's title;
//! - the buttons follow `gtk-decoration-layout`, which on X11 is the
//!   XSETTING `Gtk/DecorationLayout` (GNOME's `button-layout`), and the
//!   window's deletable and resizable flags;
//! - dragging, double-click (`gtk-titlebar-double-click`, the XSETTING
//!   `Gtk/TitlebarDoubleClick`), middle and right click are handled by
//!   the header bar's `GtkWindowHandle`, which asks the window manager
//!   through GDK: `_NET_WM_MOVERESIZE`, `_NET_WM_STATE`,
//!   `WM_CHANGE_STATE` and `_GTK_SHOW_WINDOW_MENU`, all naming the frame
//!   window.
//!
//! The wrapper asks for one pixel of width. Given less than the header
//! bar needs, it allocates the header bar its minimum anyway, shifted
//! left so its end (where GNOME's buttons are) stays in view.

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk4 as gtk;
use gtk4::glib;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct FrameHeader;

    #[glib::object_subclass]
    impl ObjectSubclass for FrameHeader {
        const NAME: &'static str = "TunaX11FrameHeader";
        type Type = super::FrameHeader;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for FrameHeader {
        /// `meta_frame_header_init`.
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            let header_bar = gtk::HeaderBar::new();
            header_bar.add_css_class("titlebar");
            header_bar.add_css_class("default-decoration");
            header_bar.insert_before(&*obj, gtk::Widget::NONE);
            obj.add_css_class("default-decoration");
        }

        /// `meta_frame_header_dispose`.
        fn dispose(&self) {
            if let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for FrameHeader {
        /// `meta_frame_header_measure`.
        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            match self.obj().first_child() {
                Some(child) if orientation == gtk::Orientation::Vertical => {
                    child.measure(orientation, for_size)
                }
                _ => (1, 1, -1, -1),
            }
        }

        /// `meta_frame_header_size_allocate`.
        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            let Some(child) = self.obj().first_child() else {
                return;
            };
            let (minimum, ..) = child.measure(gtk::Orientation::Horizontal, height);
            let shrunk = width < minimum;
            let allocation = gtk::Allocation::new(
                if shrunk { width - minimum } else { 0 },
                0,
                if shrunk { minimum } else { width },
                height,
            );
            child.size_allocate(&allocation, baseline);
        }
    }
}

glib::wrapper! {
    pub struct FrameHeader(ObjectSubclass<imp::FrameHeader>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for FrameHeader {
    /// `meta_frame_header_new`.
    fn default() -> Self {
        glib::Object::new()
    }
}
