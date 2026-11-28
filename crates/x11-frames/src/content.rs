//! The frame's child (Mutter 51 `src/frames/meta-frame-content.c`): an
//! empty widget standing where the client window goes.
//!
//! It draws nothing and asks for one pixel. The window manager sizes
//! the frame window around the client and reparents the client over
//! this widget's area; the frame reads this widget's position to report
//! how far the client sits from the frame's corner.
//!
//! Mutter keeps the client's window id in a construct-only GObject
//! property; nothing outside the frame reads it, so here it is a plain
//! field.

use crate::protocol::Xid;
use gtk::subclass::prelude::*;
use gtk4 as gtk;
use gtk4::glib;
use std::cell::Cell;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct FrameContent {
        pub window: Cell<Xid>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for FrameContent {
        const NAME: &'static str = "TunaX11FrameContent";
        type Type = super::FrameContent;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for FrameContent {}

    impl WidgetImpl for FrameContent {
        /// `meta_frame_content_measure`.
        fn measure(&self, _orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            (1, 1, -1, -1)
        }
    }
}

glib::wrapper! {
    pub struct FrameContent(ObjectSubclass<imp::FrameContent>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl FrameContent {
    /// `meta_frame_content_new`.
    pub fn new(window: Xid) -> Self {
        let content: Self = glib::Object::new();
        content.imp().window.set(window);
        content
    }

    /// `meta_frame_content_get_window`: the client window this frame is
    /// for.
    pub fn window(&self) -> Xid {
        self.imp().window.get()
    }
}
