//! The app grid's pages side by side (appDisplay.js AppDisplay's scroll
//! view): a page switch eases out-cubic over 300 ms as GNOME's
//! adjustment does, rather than adw::Carousel's spring. A discrete
//! wheel step turns one page (at most every 150 ms), a touchpad or
//! touch swipe follows the fingers and settles as GNOME's SwipeTracker
//! releases it (swipeTracker.js).
//!
//! The pages are allocated once at the page they settle on; while the
//! position moves only the drawing slides, so a switch costs no
//! relayout per frame.
use crate::grid_animation::{self as anim, Slot};
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk4 as gtk;
use gtk4::glib;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// `SCROLL_TIMEOUT_TIME`.
const SCROLL_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(150);
/// Smooth vertical scrolling turns a page per this many pixels.
const SMOOTH_STEP_PX: f64 = 30.0;

/// Hears the current page change.
type PageChanged = Rc<dyn Fn(u32)>;
/// Hears where the pages are drawn move.
type PositionChanged = Rc<dyn Fn(f64)>;

/// A swipe in progress: where it began (in pages) and whether a
/// touchpad (rather than a touchscreen) drives it.
#[derive(Clone, Copy, Debug)]
pub struct Swipe {
    initial: f64,
    touchpad: bool,
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct GridPager {
        /// Where the drawing is, in pages.
        pub position: Cell<f64>,
        /// Where the pages are allocated (the current page).
        pub page: Cell<u32>,
        pub slot: Rc<Slot>,
        pub can_scroll: Cell<bool>,
        /// Whether the wheel, touchpad and touch turn pages (Carousel's
        /// `interactive`).
        pub interactive: Cell<bool>,
        pub smooth_dy: Cell<f64>,
        /// A swipe in progress.
        pub swipe: Cell<Option<Swipe>>,
        /// The swipe's recent moves, for its velocity.
        pub history: RefCell<anim::SwipeHistory>,
        pub page_changed: RefCell<Vec<PageChanged>>,
        pub position_changed: RefCell<Vec<PositionChanged>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for GridPager {
        const NAME: &'static str = "TunaGridPager";
        type Type = super::GridPager;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for GridPager {
        fn constructed(&self) {
            self.parent_constructed();
            self.can_scroll.set(true);
            self.interactive.set(true);
            let obj = self.obj();
            obj.set_overflow(gtk::Overflow::Hidden);
            obj.set_hexpand(true);
            obj.set_vexpand(true);
        }

        fn dispose(&self) {
            self.slot.stop();
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for GridPager {
        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let mut child = self.obj().first_child();
            let (mut min, mut nat) = (0, 0);
            while let Some(page) = child {
                let (m, n, _, _) = page.measure(orientation, for_size);
                min = min.max(m);
                nat = nat.max(n);
                child = page.next_sibling();
            }
            (min, nat, -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, _baseline: i32) {
            let page = i64::from(self.page.get());
            let mut child = self.obj().first_child();
            let mut index = 0i64;
            while let Some(widget) = child {
                let x = ((index - page) * i64::from(width)) as f32;
                let transform =
                    gtk::gsk::Transform::new().translate(&gtk::graphene::Point::new(x, 0.0));
                widget.allocate(width, height, -1, Some(transform));
                index += 1;
                child = widget.next_sibling();
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            let width = f64::from(obj.width());
            let offset = (f64::from(self.page.get()) - self.position.get()) * width;
            snapshot.save();
            snapshot.translate(&gtk::graphene::Point::new(offset as f32, 0.0));
            // Only the pages that can show this frame.
            let position = self.position.get();
            let mut child = obj.first_child();
            let mut index = 0.0;
            while let Some(widget) = child {
                if (index - position).abs() < 1.0 {
                    obj.snapshot_child(&widget, snapshot);
                }
                index += 1.0;
                child = widget.next_sibling();
            }
            snapshot.restore();
        }
    }
}

glib::wrapper! {
    pub struct GridPager(ObjectSubclass<imp::GridPager>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for GridPager {
    fn default() -> Self {
        Self::new()
    }
}

impl GridPager {
    pub fn new() -> Self {
        let pager = glib::Object::new::<Self>();
        pager.add_scrolling();
        pager
    }

    pub fn append(&self, page: &impl IsA<gtk::Widget>) {
        page.set_parent(self);
    }

    pub fn remove(&self, page: &impl IsA<gtk::Widget>) {
        if page.parent().as_ref() == Some(self.upcast_ref()) {
            page.unparent();
        }
        let last = self.n_pages().saturating_sub(1);
        if self.page() > last {
            self.scroll_to(last, false);
        }
    }

    pub fn n_pages(&self) -> u32 {
        let mut n = 0;
        let mut child = self.first_child();
        while let Some(widget) = child {
            n += 1;
            child = widget.next_sibling();
        }
        n
    }

    pub fn nth_page(&self, index: u32) -> Option<gtk::Widget> {
        let mut child = self.first_child();
        for _ in 0..index {
            child = child?.next_sibling();
        }
        child
    }

    /// The page shown, or being switched to.
    pub fn page(&self) -> u32 {
        self.imp().page.get()
    }

    /// Where the pages are drawn, in pages: between two while switching.
    pub fn position(&self) -> f64 {
        self.imp().position.get()
    }

    /// Hears the current page change (when a switch begins).
    pub fn connect_page_changed(&self, f: impl Fn(u32) + 'static) {
        self.imp().page_changed.borrow_mut().push(Rc::new(f));
    }

    /// Hears where the pages are drawn move, each frame of a switch or
    /// swipe (AppDisplay's adjustment `notify::value`).
    pub fn connect_position_changed(&self, f: impl Fn(f64) + 'static) {
        self.imp().position_changed.borrow_mut().push(Rc::new(f));
    }

    /// One page forward or back, clamped.
    pub fn step(&self, step: i32) {
        let last = self.n_pages().saturating_sub(1) as i64;
        let target = (i64::from(self.page()) + i64::from(step)).clamp(0, last);
        self.scroll_to(target as u32, true);
    }

    /// Let the wheel, touchpad and touch turn pages, or not (as
    /// adw::Carousel's `interactive`); turning it off interrupts a swipe,
    /// which jumps to the page nearest where it began (SwipeTracker's
    /// `_interrupt`).
    pub fn set_interactive(&self, interactive: bool) {
        let imp = self.imp();
        if imp.interactive.replace(interactive) && !interactive {
            imp.smooth_dy.set(0.0);
            if let Some(swipe) = imp.swipe.take() {
                imp.history.borrow_mut().reset();
                let last = f64::from(self.n_pages().saturating_sub(1));
                let page = anim::swipe_cancel_page(swipe.initial).clamp(0.0, last);
                self.ease_to(page as u32, 0.0);
            }
        }
    }

    /// Go to page `index`: GNOME's goToPage, eased over the whole
    /// `PAGE_SWITCH_TIME` from wherever the pages are drawn, unless
    /// unmapped or not `animate`.
    pub fn scroll_to(&self, index: u32, animate: bool) {
        let duration_ms = if animate {
            anim::PAGE_SWITCH.duration_ms
        } else {
            0.0
        };
        self.ease_to(index, duration_ms);
    }

    /// Ease the drawing out-cubic to page `index` over `duration_ms`
    /// (before the slow-down factor; 0: at once). The page changes at
    /// once.
    fn ease_to(&self, index: u32, duration_ms: f64) {
        let imp = self.imp();
        let index = index.min(self.n_pages().saturating_sub(1));
        let from = imp.position.get();
        let to = f64::from(index);
        let changed = imp.page.replace(index) != index;
        if changed {
            self.queue_allocate();
        }
        let motion = anim::motion();
        let timing = anim::PAGE_SWITCH.lasting(duration_ms);
        let total = if self.is_mapped() && from != to {
            timing.end_ms(motion)
        } else {
            0.0
        };
        let weak = self.downgrade();
        imp.slot.start(
            self,
            "page-switch",
            total,
            move |elapsed| {
                if let Some(pager) = weak.upgrade() {
                    let position = anim::lerp(from, to, timing.at(elapsed, motion));
                    // Each animated frame, sampled on the frame clock,
                    // for the proof to check against GNOME's curve.
                    if total > 0.0 {
                        anim::trace(
                            "page-switch",
                            format_args!(
                                "frame elapsed={elapsed:.3} position={position:.6} from={from:.6} to={to:.6} ms={:.3}",
                                timing.duration_ms
                            ),
                        );
                    }
                    pager.draw_at(position);
                }
            },
            || {},
        );
        if changed {
            let callbacks = imp.page_changed.borrow().clone();
            for f in callbacks {
                f(index);
            }
        }
    }

    /// Draw the pages at `position` and tell who listens.
    fn draw_at(&self, position: f64) {
        let imp = self.imp();
        if imp.position.replace(position) == position {
            return;
        }
        self.queue_draw();
        let callbacks = imp.position_changed.borrow().clone();
        for f in callbacks {
            f(position);
        }
    }

    /// A swipe begins at event `time` (ms), taking over from a switch
    /// where it stands (AppDisplay._swipeBegin).
    fn swipe_begin(&self, time: u32, touchpad: bool) {
        let imp = self.imp();
        if imp.swipe.get().is_some() {
            return;
        }
        imp.slot.stop();
        imp.swipe.set(Some(Swipe {
            initial: imp.position.get(),
            touchpad,
        }));
        let mut history = imp.history.borrow_mut();
        history.reset();
        history.append(time, 0.0);
    }

    /// Follow the fingers `delta` pixels toward later pages, at most a
    /// page past where the swipe began.
    fn swipe_update(&self, time: u32, delta: f64, touchpad: bool) {
        self.swipe_begin(time, touchpad);
        let imp = self.imp();
        let Some(swipe) = imp.swipe.get() else {
            return;
        };
        imp.history.borrow_mut().append(time, delta);
        let last = f64::from(self.n_pages().saturating_sub(1));
        let (lower, upper) = anim::swipe_bounds(swipe.initial, last);
        let width = f64::from(self.width().max(1));
        self.draw_at((imp.position.get() + delta / width).clamp(lower, upper));
    }

    /// Released at event `time`: settle where GNOME's SwipeTracker
    /// would, over its duration, out-cubic (AppDisplay._swipeEnd).
    fn swipe_end(&self, time: u32) {
        let imp = self.imp();
        let Some(swipe) = imp.swipe.take() else {
            return;
        };
        let velocity = {
            let mut history = imp.history.borrow_mut();
            history.trim(time);
            let velocity = history.velocity();
            history.reset();
            velocity
        };
        let last = f64::from(self.n_pages().saturating_sub(1));
        let (page, duration_ms) = anim::swipe_end(
            imp.position.get(),
            swipe.initial,
            velocity,
            f64::from(self.width().max(1)),
            swipe.touchpad,
            last,
        );
        anim::trace(
            "swipe",
            format_args!("end velocity={velocity:.4} page={page:.0} ms={duration_ms:.1}"),
        );
        self.ease_to(page as u32, duration_ms);
    }

    fn add_scrolling(&self) {
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
        let weak = self.downgrade();
        scroll.connect_scroll(move |controller, dx, dy| {
            let Some(pager) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            let imp = pager.imp();
            if !imp.interactive.get() {
                return glib::Propagation::Proceed;
            }
            let rtl = pager.direction() == gtk::TextDirection::Rtl;
            if controller.unit() == gtk::gdk::ScrollUnit::Surface {
                // A touchpad: sideways follows the fingers (and a swipe
                // under way keeps following them), up and down turns a
                // page per step as GNOME's discrete scrolling.
                if imp.swipe.get().is_some() || dx.abs() > dy.abs() {
                    let time = controller.current_event_time();
                    pager.swipe_update(time, if rtl { -dx } else { dx }, true);
                    return glib::Propagation::Stop;
                }
                let acc = imp.smooth_dy.get() + dy;
                if acc.abs() < SMOOTH_STEP_PX {
                    imp.smooth_dy.set(acc);
                    return glib::Propagation::Stop;
                }
                imp.smooth_dy.set(0.0);
                return pager.wheel(0.0, acc);
            }
            pager.wheel(if rtl { -dx } else { dx }, dy)
        });
        let weak = self.downgrade();
        scroll.connect_scroll_end(move |controller| {
            if let Some(pager) = weak.upgrade() {
                pager.imp().smooth_dy.set(0.0);
                pager.swipe_end(controller.current_event_time());
            }
        });
        self.add_controller(scroll);
        // A touch swipe: the finger's moves, as pixels toward later
        // pages.
        let drag = gtk::GestureDrag::new();
        drag.set_touch_only(true);
        let offset = Rc::new(Cell::new(0.0));
        {
            let (weak, offset) = (self.downgrade(), offset.clone());
            drag.connect_drag_begin(move |gesture, _, _| {
                if let Some(pager) = weak.upgrade().filter(|p| p.imp().interactive.get()) {
                    offset.set(0.0);
                    pager.swipe_begin(gesture.current_event_time(), false);
                }
            });
        }
        {
            let weak = self.downgrade();
            drag.connect_drag_update(move |gesture, dx, _| {
                if let Some(pager) = weak
                    .upgrade()
                    .filter(|p| p.imp().interactive.get() && p.imp().swipe.get().is_some())
                {
                    let delta = offset.replace(dx) - dx;
                    pager.swipe_update(gesture.current_event_time(), delta, false);
                }
            });
        }
        let weak = self.downgrade();
        drag.connect_drag_end(move |gesture, _, _| {
            if let Some(pager) = weak.upgrade() {
                pager.swipe_end(gesture.current_event_time());
            }
        });
        self.add_controller(drag);
    }

    /// GNOME's _onScroll: one page per discrete step, then 150 ms rest.
    fn wheel(&self, dx: f64, dy: f64) -> glib::Propagation {
        let imp = self.imp();
        let step = if dy != 0.0 {
            dy.signum() as i32
        } else if dx != 0.0 {
            dx.signum() as i32
        } else {
            return glib::Propagation::Proceed;
        };
        if !imp.can_scroll.get() {
            return glib::Propagation::Stop;
        }
        self.step(step);
        imp.can_scroll.set(false);
        let weak = self.downgrade();
        glib::timeout_add_local_once(SCROLL_TIMEOUT, move || {
            if let Some(pager) = weak.upgrade() {
                pager.imp().can_scroll.set(true);
            }
        });
        glib::Propagation::Stop
    }
}
