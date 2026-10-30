//! GNOME 51's dash motion (js/ui/dash.js): an app joining the dash zooms
//! and fades in and one leaving zooms and fades out
//! (`DashItemContainer.show` and `animateOutAndDestroy`, dash.js:154-185:
//! `DASH_ANIMATION_TIME` 200 ms ease-out-quad on scale and opacity, the
//! item's size following its scale, :82-98), and an item's label fades
//! in above it on hover (`showLabel`, :100-127, 150 ms) and out
//! (`hideLabel`, :134-141, 100 ms), both ease-out-quad, after
//! `DASH_ITEM_HOVER_TIMEOUT` (300 ms, at once while labels are showing;
//! `_syncLabel`, :539-571).
//!
//! Time comes from [`crate::anim_clock`], so a proof holding the
//! animation clock samples these at exact delays. Scale is motion and
//! opacity a fade under the shared motion policy: Reduced Motion keeps an
//! item at full size while it fades, and with animations off everything
//! lands at once. A new target eases from the value drawn now, so an item
//! removed while it is still zooming in shrinks back from where it stands
//! and a label shown again while fading out fades back in from there.
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk4 as gtk;
use gtk4::glib;

use crate::grid_animation::{lerp, Curve, Motion};

/// `DASH_ANIMATION_TIME`: an item zooming and fading in or out.
pub const ITEM_MS: f64 = 200.0;
/// `DASH_ITEM_LABEL_SHOW_TIME`, `DASH_ITEM_LABEL_HIDE_TIME`.
pub const LABEL_SHOW_MS: f64 = 150.0;
pub const LABEL_HIDE_MS: f64 = 100.0;
/// `DASH_ITEM_HOVER_TIMEOUT`: how long the pointer rests on an item
/// before its label shows, and how long labels keep showing at once
/// after it leaves.
pub const HOVER_TIMEOUT_MS: u64 = 300;
/// `.dash-label`'s `-y-offset`, `$base_margin * 2` (gnome-shell-sass
/// widgets/_dash.scss:105): the label's gap above the item.
pub const LABEL_Y_OFFSET: i32 = 8;

/// One ease-out-quad value on the animation clock.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tween {
    pub from: f64,
    pub to: f64,
    pub start: Duration,
    /// After the slow-down factor; 0 lands at once.
    pub duration_ms: f64,
}

impl Tween {
    /// From `from` to `to` over `nominal_ms`, stretched by the slow-down
    /// factor, or at once when `animated` is false.
    pub fn new(
        from: f64,
        to: f64,
        start: Duration,
        nominal_ms: f64,
        animated: bool,
        policy: Motion,
    ) -> Self {
        Self {
            from,
            to,
            start,
            duration_ms: if animated {
                policy.adjust_ms(nominal_ms)
            } else {
                0.0
            },
        }
    }

    /// The value at `now`, and whether it has landed.
    pub fn at(&self, now: Duration) -> (f64, bool) {
        if self.duration_ms <= 0.0 {
            return (self.to, true);
        }
        let elapsed = now.saturating_sub(self.start).as_secs_f64() * 1000.0;
        let t = elapsed / self.duration_ms;
        if t >= 1.0 {
            (self.to, true)
        } else {
            (lerp(self.from, self.to, Curve::OutQuad.at(t)), false)
        }
    }
}

/// An item's (scale, opacity) eases toward shown or gone from `drawn`.
/// The zoom is motion, the fade a fade; under Reduced Motion the item
/// keeps its whole size while it fades. `animate` false (GNOME's
/// `show(false)`, or a dash that is not on screen) lands at once.
pub fn item_tweens(
    drawn: (f64, f64),
    shown: bool,
    animate: bool,
    start: Duration,
    policy: Motion,
) -> (Tween, Tween) {
    let target = if shown { 1.0 } else { 0.0 };
    let scale = if animate && policy.allows_fades() && !policy.allows_motion() {
        Tween::new(1.0, 1.0, start, 0.0, false, policy)
    } else {
        Tween::new(
            drawn.0,
            target,
            start,
            ITEM_MS,
            animate && policy.allows_motion(),
            policy,
        )
    };
    let opacity = Tween::new(
        drawn.1,
        target,
        start,
        ITEM_MS,
        animate && policy.allows_fades(),
        policy,
    );
    (scale, opacity)
}

/// A label's opacity eases in (150 ms) or out (100 ms) from `drawn`; a
/// fade, so Reduced Motion keeps it.
pub fn label_tween(drawn: f64, shown: bool, start: Duration, policy: Motion) -> Tween {
    let (to, ms) = if shown {
        (1.0, LABEL_SHOW_MS)
    } else {
        (0.0, LABEL_HIDE_MS)
    };
    Tween::new(drawn, to, start, ms, policy.allows_fades(), policy)
}

/// `_syncLabel`'s wait before showing a hovered item's label: none while
/// labels are already showing (the pointer moved on from another item).
pub fn label_delay_ms(labels_showing: bool) -> u64 {
    if labels_showing {
        0
    } else {
        HOVER_TIMEOUT_MS
    }
}

/// Whether the dash's motion is traced (`TUNA_DASH_TRACE`, read once).
static TRACING: std::sync::LazyLock<bool> =
    std::sync::LazyLock::new(|| std::env::var_os("TUNA_DASH_TRACE").is_some());

fn trace(what: std::fmt::Arguments) {
    if *TRACING {
        eprintln!("tuna-shell-gtk: dash {what}");
    }
}

fn clock_name(policy: Motion) -> &'static str {
    if crate::anim_clock::manual() {
        "manual"
    } else if policy.allows_fades() {
        "real"
    } else {
        "none"
    }
}

/// The hover label: GNOME's `dash-label` chrome, a click-through popup
/// above its item that screen readers skip (the item's own accessible
/// name carries the text, as `setLabelText` sets it).
struct HoverLabel {
    popover: gtk::Popover,
    text: gtk::Label,
    tween: Cell<Option<Tween>>,
    /// Its tick callback runs (GTK drops it when the label unrealizes).
    ticking: Rc<Cell<bool>>,
}

impl HoverLabel {
    fn new(anchor: &DashItem, text: &str) -> Self {
        let label = gtk::Label::new(Some(text));
        label.add_css_class("dash-label");
        label.set_opacity(0.0);
        label.update_state(&[gtk::accessible::State::Hidden(true)]);
        let ticking = Rc::new(Cell::new(false));
        {
            let ticking = ticking.clone();
            label.connect_unrealize(move |_| ticking.set(false));
        }
        let popover = gtk::Popover::new();
        popover.add_css_class("dash-label-popover");
        popover.set_has_arrow(false);
        popover.set_autohide(false);
        popover.set_can_focus(false);
        popover.set_can_target(false);
        popover.set_position(gtk::PositionType::Top);
        popover.set_offset(0, -LABEL_Y_OFFSET);
        popover.update_state(&[gtk::accessible::State::Hidden(true)]);
        popover.set_child(Some(&label));
        popover.connect_realize(|popover| {
            if let Some(surface) = popover.surface() {
                surface.set_input_region(Some(&gtk::cairo::Region::create()));
            }
        });
        popover.set_parent(anchor);
        Self {
            popover,
            text: label,
            tween: Cell::new(None),
            ticking,
        }
    }
}

/// What runs once an item's ease lands.
type Settled = Box<dyn FnOnce(&DashItem)>;

mod imp {
    use super::*;

    pub struct DashItem {
        /// The item's one child (its popover label is a child too).
        pub child: RefCell<Option<gtk::Widget>>,
        pub key: RefCell<String>,
        pub scale: Cell<f64>,
        pub scale_tween: Cell<Option<Tween>>,
        pub opacity_tween: Cell<Option<Tween>>,
        pub ticking: Cell<bool>,
        pub shown: Cell<bool>,
        /// Runs once the current ease lands; a newer ease drops it.
        pub settled: RefCell<Option<Settled>>,
        pub(super) label: RefCell<Option<HoverLabel>>,
        pub label_text: RefCell<String>,
    }

    impl Default for DashItem {
        fn default() -> Self {
            // GNOME's DashItemContainer starts at scale 0, opacity 0.
            Self {
                child: RefCell::new(None),
                key: RefCell::default(),
                scale: Cell::new(0.0),
                scale_tween: Cell::new(None),
                opacity_tween: Cell::new(None),
                ticking: Cell::new(false),
                shown: Cell::new(false),
                settled: RefCell::new(None),
                label: RefCell::new(None),
                label_text: RefCell::default(),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for DashItem {
        const NAME: &'static str = "TunaDashItem";
        type Type = super::DashItem;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for DashItem {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_opacity(0.0);
            // GTK drops tick callbacks with the widget's realization.
            obj.connect_unrealize(|item| item.imp().ticking.set(false));
        }

        fn dispose(&self) {
            if let Some(label) = self.label.borrow_mut().take() {
                label.popover.unparent();
            }
            if let Some(child) = self.child.borrow_mut().take() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for DashItem {
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            let Some(child) = self.child.borrow().clone() else {
                return (0, 0, -1, -1);
            };
            // vfunc_get_preferred_width/height: the child's size times
            // the item's scale.
            let (min, natural, _, _) = child.measure(orientation, -1);
            let scale = self.scale.get().max(0.0);
            let share = |v: i32| (f64::from(v) * scale).round() as i32;
            (share(min), share(natural), -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, _baseline: i32) {
            if let Some(label) = self.label.borrow().as_ref() {
                label.popover.present();
            }
            let Some(child) = self.child.borrow().clone() else {
                return;
            };
            let (_, child_width, _, _) = child.measure(gtk::Orientation::Horizontal, -1);
            let (_, child_height, _, _) = child.measure(gtk::Orientation::Vertical, child_width);
            // Scaled about the centre (pivot_point 0.5, 0.5); a zero scale
            // would leave GTK no inverse to pick through.
            let scale = self.scale.get().max(1e-3) as f32;
            let transform = gtk::gsk::Transform::new()
                .translate(&gtk::graphene::Point::new(
                    width as f32 / 2.0,
                    height as f32 / 2.0,
                ))
                .scale(scale, scale)
                .translate(&gtk::graphene::Point::new(
                    -child_width as f32 / 2.0,
                    -child_height as f32 / 2.0,
                ));
            child.allocate(child_width, child_height, -1, Some(transform));
        }
    }
}

glib::wrapper! {
    /// GNOME's DashItemContainer: one dash item, sized by its scale.
    pub struct DashItem(ObjectSubclass<imp::DashItem>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl DashItem {
    /// A hidden item (scale 0, transparent) around `child`, labelled
    /// `label` and traced as `key`.
    pub fn new(key: &str, child: &impl IsA<gtk::Widget>, label: &str) -> Self {
        let item: Self = glib::Object::new();
        child.set_parent(&item);
        item.imp().child.replace(Some(child.clone().upcast()));
        item.imp().key.replace(key.to_owned());
        item.set_label_text(label);
        item
    }

    pub fn key(&self) -> String {
        self.imp().key.borrow().clone()
    }

    /// (scale, opacity) as drawn now.
    pub fn look(&self) -> (f64, f64) {
        (self.imp().scale.get(), self.opacity())
    }

    /// Whether it is shown or on its way in (not leaving).
    pub fn shown(&self) -> bool {
        self.imp().shown.get()
    }

    /// `setLabelText`: what the hover label says.
    pub fn set_label_text(&self, text: &str) {
        let imp = self.imp();
        imp.label_text.replace(text.to_owned());
        if let Some(label) = imp.label.borrow().as_ref() {
            label.text.set_text(text);
        }
    }

    fn set_look(&self, scale: f64, opacity: f64) {
        let imp = self.imp();
        self.set_opacity(opacity.clamp(0.0, 1.0));
        if imp.scale.replace(scale) != scale {
            self.queue_resize();
        }
    }

    /// `show(animate)` (shown) or `animateOutAndDestroy` (not): ease the
    /// item's scale and opacity there from where they are drawn, then
    /// `settled`. A later ease replaces this one and drops its `settled`.
    pub fn ease(&self, shown: bool, animate: bool, settled: impl FnOnce(&DashItem) + 'static) {
        let imp = self.imp();
        let policy = crate::motion::current();
        // A dash off screen gets no frames: it lands at once.
        let animate = animate && self.parent().is_some_and(|parent| parent.is_mapped());
        let start = if animate {
            crate::anim_clock::now()
        } else {
            Duration::ZERO
        };
        let drawn = self.look();
        let (scale, opacity) = item_tweens(drawn, shown, animate, start, policy);
        imp.shown.set(shown);
        if !shown {
            self.hide_label_now();
        }
        imp.settled.replace(Some(Box::new(settled)));
        let way = if shown { "show" } else { "hide" };
        if scale.duration_ms <= 0.0 && opacity.duration_ms <= 0.0 {
            imp.scale_tween.set(None);
            imp.opacity_tween.set(None);
            trace(format_args!(
                "item {} {way} instant motion={}",
                self.key(),
                policy.level.as_str()
            ));
            self.set_look(scale.to, opacity.to);
            self.settle();
            return;
        }
        trace(format_args!(
            "item {} {way} start from={:.4},{:.4} ms={:.0} motion={} clock={}",
            self.key(),
            drawn.0,
            drawn.1,
            scale.duration_ms.max(opacity.duration_ms),
            policy.level.as_str(),
            clock_name(policy),
        ));
        imp.scale_tween.set(Some(scale));
        imp.opacity_tween.set(Some(opacity));
        self.step();
        if imp.scale_tween.get().is_some() && !imp.ticking.replace(true) {
            self.add_tick_callback(|item, _| item.tick());
        }
    }

    /// One frame: false once both values have landed.
    fn step(&self) -> bool {
        let imp = self.imp();
        let (Some(scale), Some(opacity)) = (imp.scale_tween.get(), imp.opacity_tween.get()) else {
            return false;
        };
        let now = crate::anim_clock::now();
        let (s, scale_done) = scale.at(now);
        let (o, opacity_done) = opacity.at(now);
        if (s, o) != self.look() {
            trace(format_args!(
                "item {} frame scale={s:.4} opacity={o:.4}",
                self.key()
            ));
        }
        self.set_look(s, o);
        if scale_done && opacity_done {
            imp.scale_tween.set(None);
            imp.opacity_tween.set(None);
            self.settle();
            return false;
        }
        true
    }

    fn tick(&self) -> glib::ControlFlow {
        if self.step() {
            return glib::ControlFlow::Continue;
        }
        self.imp().ticking.set(false);
        glib::ControlFlow::Break
    }

    fn settle(&self) {
        let (scale, opacity) = self.look();
        trace(format_args!(
            "item {} settle scale={scale:.4} opacity={opacity:.4}",
            self.key()
        ));
        let settled = self.imp().settled.borrow_mut().take();
        if let Some(settled) = settled {
            settled(self);
        }
    }

    fn with_label<R>(&self, f: impl FnOnce(&HoverLabel) -> R) -> R {
        let imp = self.imp();
        if imp.label.borrow().is_none() {
            let label = HoverLabel::new(self, &imp.label_text.borrow());
            imp.label.replace(Some(label));
        }
        f(imp.label.borrow().as_ref().expect("label just made"))
    }

    /// `showLabel` (shown) or `hideLabel`: the label fades in above the
    /// item or out, from the opacity drawn now.
    pub fn ease_label(&self, shown: bool) {
        let imp = self.imp();
        if shown && imp.label_text.borrow().is_empty() {
            return;
        }
        if !shown && imp.label.borrow().is_none() {
            return;
        }
        let policy = crate::motion::current();
        let animate = self.is_mapped();
        let key = self.key();
        let (text, ticking) = self.with_label(|label| {
            let drawn = if label.popover.is_visible() {
                label.text.opacity()
            } else {
                0.0
            };
            if shown {
                label.popover.popup();
            }
            let start = if animate {
                crate::anim_clock::now()
            } else {
                Duration::ZERO
            };
            let mut tween = label_tween(drawn, shown, start, policy);
            if !animate {
                tween.duration_ms = 0.0;
            }
            let way = if shown { "show" } else { "hide" };
            if tween.duration_ms <= 0.0 {
                trace(format_args!(
                    "label {key} {way} instant motion={}",
                    policy.level.as_str()
                ));
            } else {
                trace(format_args!(
                    "label {key} {way} start from={drawn:.4} ms={:.0} motion={} clock={}",
                    tween.duration_ms,
                    policy.level.as_str(),
                    clock_name(policy),
                ));
            }
            label.tween.set(Some(tween));
            (label.text.clone(), label.ticking.clone())
        });
        if shown {
            // The popover is placed in the item's size_allocate.
            self.queue_allocate();
        }
        // An instant ease lands (and a faded label pops down) here.
        if !self.label_step() || ticking.replace(true) {
            return;
        }
        let item = self.downgrade();
        text.add_tick_callback(move |_, _| {
            if item.upgrade().is_some_and(|item| item.label_step()) {
                return glib::ControlFlow::Continue;
            }
            ticking.set(false);
            glib::ControlFlow::Break
        });
    }

    /// One label frame: false once it has landed (and, faded out, hidden).
    fn label_step(&self) -> bool {
        let key = self.key();
        let imp = self.imp();
        let label = imp.label.borrow();
        let Some(label) = label.as_ref() else {
            return false;
        };
        let Some(tween) = label.tween.get() else {
            return false;
        };
        let (opacity, done) = tween.at(crate::anim_clock::now());
        if opacity != label.text.opacity() {
            trace(format_args!("label {key} frame opacity={opacity:.4}"));
        }
        label.text.set_opacity(opacity);
        if !done {
            return true;
        }
        label.tween.set(None);
        trace(format_args!("label {key} settle opacity={opacity:.4}"));
        if tween.to <= 0.0 {
            label.popover.popdown();
        }
        false
    }

    /// The label gone at once (`animateOutAndDestroy`'s `label.hide()`,
    /// or the overview hiding while it fades).
    pub fn hide_label_now(&self) {
        if let Some(label) = self.imp().label.borrow().as_ref() {
            label.tween.set(None);
            label.text.set_opacity(0.0);
            label.popover.popdown();
        }
    }
}

/// The dash's labels together (`_syncLabel`'s `_labelShowing` and its two
/// timeouts): the first hovered item's label waits 300 ms, the next shows
/// at once, until the pointer has been off the items for 300 ms.
#[derive(Default)]
pub struct Labels {
    showing: Cell<bool>,
    show_timeout: RefCell<Option<glib::SourceId>>,
    reset_timeout: RefCell<Option<glib::SourceId>>,
}

impl Labels {
    /// `_hookUpLabel`: hovering `button` shows `item`'s label, leaving it
    /// or clicking hides it.
    pub fn hook(self: &Rc<Self>, item: &DashItem, button: &impl IsA<gtk::Button>) {
        let motion = gtk::EventControllerMotion::new();
        {
            let (labels, item) = (Rc::downgrade(self), item.downgrade());
            motion.connect_enter(move |_, _, _| {
                if let (Some(labels), Some(item)) = (labels.upgrade(), item.upgrade()) {
                    labels.sync(&item, true);
                }
            });
        }
        {
            let (labels, item) = (Rc::downgrade(self), item.downgrade());
            motion.connect_leave(move |_| {
                if let (Some(labels), Some(item)) = (labels.upgrade(), item.upgrade()) {
                    labels.sync(&item, false);
                }
            });
        }
        button.upcast_ref::<gtk::Button>().add_controller(motion);
        let (labels, item) = (Rc::downgrade(self), item.downgrade());
        button.connect_clicked(move |_| {
            if let (Some(labels), Some(item)) = (labels.upgrade(), item.upgrade()) {
                labels.showing.set(false);
                labels.cancel_show();
                item.ease_label(false);
            }
        });
    }

    fn cancel_show(&self) {
        if let Some(id) = self.show_timeout.borrow_mut().take() {
            id.remove();
        }
    }

    fn cancel_reset(&self) {
        if let Some(id) = self.reset_timeout.borrow_mut().take() {
            id.remove();
        }
    }

    fn sync(self: &Rc<Self>, item: &DashItem, hovered: bool) {
        if hovered {
            if self.show_timeout.borrow().is_some() {
                return;
            }
            let delay = label_delay_ms(self.showing.get());
            let (labels, target) = (Rc::downgrade(self), item.downgrade());
            let id = glib::timeout_add_local_once(Duration::from_millis(delay), move || {
                let Some(labels) = labels.upgrade() else {
                    return;
                };
                labels.show_timeout.borrow_mut().take();
                labels.showing.set(true);
                if let Some(item) = target.upgrade() {
                    item.ease_label(true);
                }
            });
            self.show_timeout.replace(Some(id));
            self.cancel_reset();
        } else {
            self.cancel_show();
            item.ease_label(false);
            if self.showing.get() {
                let labels = Rc::downgrade(self);
                let id = glib::timeout_add_local_once(
                    Duration::from_millis(HOVER_TIMEOUT_MS),
                    move || {
                        if let Some(labels) = labels.upgrade() {
                            labels.reset_timeout.borrow_mut().take();
                            labels.showing.set(false);
                        }
                    },
                );
                self.cancel_reset();
                self.reset_timeout.replace(Some(id));
            }
        }
    }

    /// The overview hiding: no label waits or shows.
    pub fn reset<'a>(&self, items: impl IntoIterator<Item = &'a DashItem>) {
        self.cancel_show();
        self.cancel_reset();
        self.showing.set(false);
        for item in items {
            item.hide_label_now();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuna_shell_control::MotionLevel;

    const FULL: Motion = Motion {
        level: MotionLevel::Full,
        slowdown_milli: 1000,
    };

    fn ms(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn gnome_51_dash_durations() {
        assert_eq!(ITEM_MS, 200.0);
        assert_eq!(LABEL_SHOW_MS, 150.0);
        assert_eq!(LABEL_HIDE_MS, 100.0);
        assert_eq!(HOVER_TIMEOUT_MS, 300);
        assert_eq!(LABEL_Y_OFFSET, 8);
        assert_eq!(label_delay_ms(false), 300);
        assert_eq!(label_delay_ms(true), 0);
    }

    #[test]
    fn items_zoom_and_fade_in_and_out_over_200_ms_ease_out_quad() {
        let (scale, opacity) = item_tweens((0.0, 0.0), true, true, ms(1000), FULL);
        assert_eq!(scale.at(ms(1000)), (0.0, false));
        let (s, done) = scale.at(ms(1100));
        assert!(close(s, 0.75) && !done, "{s}");
        assert!(close(opacity.at(ms(1100)).0, 0.75));
        assert_eq!(scale.at(ms(1200)), (1.0, true));
        assert_eq!(opacity.at(ms(1200)), (1.0, true));
        let (scale, opacity) = item_tweens((1.0, 1.0), false, true, ms(0), FULL);
        assert!(close(scale.at(ms(100)).0, 0.25));
        assert!(close(opacity.at(ms(100)).0, 0.25));
        assert_eq!(opacity.at(ms(200)), (0.0, true));
    }

    #[test]
    fn an_item_removed_while_zooming_in_shrinks_back_from_where_it_stands() {
        let (scale, _) = item_tweens((0.0, 0.0), true, true, ms(0), FULL);
        let drawn = scale.at(ms(100)).0;
        let (back, opacity) = item_tweens((drawn, drawn), false, true, ms(100), FULL);
        // No jump: the reversal starts at the drawn value...
        assert_eq!(back.at(ms(100)), (drawn, false));
        assert_eq!(opacity.at(ms(100)), (drawn, false));
        // ...and takes GNOME's whole 200 ms back to nothing.
        assert!(close(back.at(ms(200)).0, drawn * 0.25));
        assert_eq!(back.at(ms(300)), (0.0, true));
    }

    #[test]
    fn reduced_motion_keeps_the_fade_at_full_size_and_off_lands_at_once() {
        let fades = Motion::from_gnome(true, true, 1.0);
        let (scale, opacity) = item_tweens((0.0, 0.0), true, true, ms(0), fades);
        assert_eq!(scale.at(ms(0)), (1.0, true));
        assert!(close(opacity.at(ms(100)).0, 0.75));
        let (scale, opacity) = item_tweens((1.0, 1.0), false, true, ms(0), fades);
        assert_eq!(
            scale.at(ms(100)),
            (1.0, true),
            "a leaving item keeps its size"
        );
        assert!(close(opacity.at(ms(100)).0, 0.25));
        let off = Motion::from_gnome(false, false, 1.0);
        let (scale, opacity) = item_tweens((0.0, 0.0), true, true, ms(0), off);
        assert_eq!(
            (scale.at(ms(0)), opacity.at(ms(0))),
            ((1.0, true), (1.0, true))
        );
        let (scale, opacity) = item_tweens((1.0, 1.0), false, true, ms(0), off);
        assert_eq!(
            (scale.at(ms(0)), opacity.at(ms(0))),
            ((0.0, true), (0.0, true))
        );
        // GNOME's show(false): the initial fill lands at once.
        let (scale, opacity) = item_tweens((0.0, 0.0), true, false, ms(0), FULL);
        assert_eq!(
            (scale.at(ms(0)), opacity.at(ms(0))),
            ((1.0, true), (1.0, true))
        );
        assert_eq!(label_tween(0.0, true, ms(0), off).at(ms(0)), (1.0, true));
        assert!(close(
            label_tween(0.0, true, ms(0), fades).at(ms(75)).0,
            0.75
        ));
    }

    #[test]
    fn labels_fade_in_over_150_ms_and_out_over_100_ms_from_where_they_stand() {
        let show = label_tween(0.0, true, ms(0), FULL);
        assert!(close(show.at(ms(75)).0, 0.75));
        assert_eq!(show.at(ms(150)), (1.0, true));
        let hide = label_tween(1.0, false, ms(0), FULL);
        assert!(close(hide.at(ms(50)).0, 0.25));
        assert_eq!(hide.at(ms(100)), (0.0, true));
        // Shown again half way out, it fades back in from there.
        let again = label_tween(0.25, true, ms(50), FULL);
        assert_eq!(again.at(ms(50)), (0.25, false));
        assert!(close(again.at(ms(125)).0, 0.25 + 0.75 * 0.75));
    }

    #[test]
    fn the_slow_down_factor_stretches_dash_motion() {
        let slow = Motion::from_gnome(true, false, 2.0);
        let (scale, _) = item_tweens((0.0, 0.0), true, true, ms(0), slow);
        assert_eq!(scale.duration_ms, 400.0);
        assert!(close(scale.at(ms(200)).0, 0.75));
        assert_eq!(label_tween(0.0, true, ms(0), slow).duration_ms, 300.0);
        assert_eq!(label_tween(1.0, false, ms(0), slow).duration_ms, 200.0);
    }
}
