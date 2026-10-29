//! GNOME 51's unlock-dialog curtain (`unlockDialog.js` with
//! `swipeTracker.js`, gnome-shell 51.0).
//!
//! The lock screen shows the clock curtain or the unlock prompt. GNOME
//! moves between them on one progress value (0 the clock, 1 the prompt;
//! `_setTransitionProgress`): the clock fades out, rises
//! [`FADE_OUT_TRANSLATION`] px and shrinks toward [`FADE_OUT_SCALE`]
//! about its centre while the prompt fades in, rising from
//! [`FADE_OUT_TRANSLATION`] px below and growing from the same scale. A
//! key, click or wheel step eases the value over [`CROSSFADE_MS`]
//! ease-out quad (`_showPrompt`, `_showClock`). A vertical pointer drag
//! (`Clutter.PanGesture`, recognized after [`DRAG_THRESHOLD`] px; the
//! session hands touch to the lock surface as the primary button) moves
//! the value with the pointer; on release `SwipeTracker` projects the
//! release velocity onto the two pages and finishes in 100-400 ms
//! ease-out cubic.
//!
//! Reduced Motion keeps the fades and drops the translation and scale
//! (as `_setTransitionProgress` does); animations off land at once; the
//! slow-down factor stretches every ease. Time is
//! [`crate::anim_clock`], so a nested proof can sample the curtain on
//! the compositor's frozen clock.
//!
//! Only the two pages move inside the lock window: the window, its
//! input and the authentication path are untouched. A page is hidden
//! (not just transparent) once it is fully off, so it takes no input,
//! keeps no keyboard focus and leaves the accessible tree.
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use gtk4::subclass::prelude::*;

use crate::transient::{ease_out_quad, lerp, Frame};
use crate::ws_dots::ease_out_cubic;

/// `CROSSFADE_TIME`: clock to prompt and back by key, click or wheel.
pub const CROSSFADE_MS: f64 = 300.0;
/// `FADE_OUT_TRANSLATION`: how far a page travels, in logical px (GNOME
/// multiplies by St's scale factor, which is 1 on Wayland's scaled
/// stage views, as GTK's logical pixels are).
pub const FADE_OUT_TRANSLATION: f64 = 200.0;
/// `FADE_OUT_SCALE`: the smallest a page shrinks to.
pub const FADE_OUT_SCALE: f64 = 0.3;
/// `ClutterPanGesture` `DEFAULT_BEGIN_THRESHOLD_PX` along the pan axis.
pub const DRAG_THRESHOLD: f64 = 16.0;
/// `EVENT_HISTORY_THRESHOLD_MS` / `EVENT_HISTORY_DURATION_MS`.
const HISTORY_MS: u32 = 150;
/// `EVENT_HISTORY_MAX_LENGTH` of `ClutterPanGesture`.
const PAN_HISTORY_LEN: usize = 150;
const MIN_ANIMATION_MS: f64 = 100.0;
const MAX_ANIMATION_MS: f64 = 400.0;
const VELOCITY_THRESHOLD_TOUCH: f64 = 0.3;
const DECELERATION_TOUCH: f64 = 0.998;
const VELOCITY_CURVE_THRESHOLD: f64 = 2.0;
const DECELERATION_PARABOLA_MULTIPLIER: f64 = 0.35;
/// The derivative of ease-out cubic at 0: a release keeps its speed.
const DURATION_MULTIPLIER: f64 = 3.0;
const ANIMATION_BASE_VELOCITY: f64 = 0.002;

/// The page the dialog shows or is heading to (`_activePage`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Page {
    #[default]
    Clock,
    Prompt,
}

impl Page {
    pub fn name(self) -> &'static str {
        match self {
            Page::Clock => "clock",
            Page::Prompt => "prompt",
        }
    }

    fn progress(self) -> f64 {
        match self {
            Page::Clock => 0.0,
            Page::Prompt => 1.0,
        }
    }
}

/// The clock and the prompt at `progress` (`_setTransitionProgress`);
/// without `motion` (Reduced Motion) only their opacity changes.
pub fn frames(progress: f64, motion: bool) -> (Frame, Frame) {
    let p = progress.clamp(0.0, 1.0);
    let clock = Frame {
        opacity: 1.0 - p,
        dy: -FADE_OUT_TRANSLATION * p,
        scale: lerp(FADE_OUT_SCALE, 1.0, 1.0 - p),
        extent: 1.0,
    };
    let prompt = Frame {
        opacity: p,
        dy: FADE_OUT_TRANSLATION * (1.0 - p),
        scale: lerp(FADE_OUT_SCALE, 1.0, p),
        extent: 1.0,
    };
    if motion {
        (clock, prompt)
    } else {
        (clock.without_motion(), prompt.without_motion())
    }
}

/// The Clutter easing modes the dialog uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ease {
    /// `_showPrompt` / `_showClock`.
    OutQuad,
    /// `_swipeEnd`.
    OutCubic,
}

impl Ease {
    fn apply(self, t: f64) -> f64 {
        match self {
            Ease::OutQuad => ease_out_quad(t),
            Ease::OutCubic => ease_out_cubic(t),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Ease::OutQuad => "ease-out-quad",
            Ease::OutCubic => "ease-out-cubic",
        }
    }
}

/// One ease of the progress value, timed on [`crate::anim_clock`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tween {
    pub from: f64,
    pub to: f64,
    pub start_ms: f64,
    pub duration_ms: f64,
    pub ease: Ease,
}

impl Tween {
    /// The value at `now_ms`, and whether the ease is over.
    pub fn at(&self, now_ms: f64) -> (f64, bool) {
        let t = if self.duration_ms <= 0.0 {
            1.0
        } else {
            ((now_ms - self.start_ms) / self.duration_ms).clamp(0.0, 1.0)
        };
        (lerp(self.from, self.to, self.ease.apply(t)), t >= 1.0)
    }
}

/// `ClutterPanGesture`'s event history: per-event deltas, at most one per
/// millisecond; the velocity sums every delta of the last 150 ms over
/// the time they span, in px/ms.
#[derive(Debug, Default, Clone)]
pub struct PanHistory {
    samples: VecDeque<(u32, f64)>,
}

impl PanHistory {
    /// The press: a zero delta at its time (`point_began`).
    pub fn begin(&mut self, time: u32) {
        self.samples.clear();
        self.samples.push_back((time, 0.0));
    }

    /// One motion (`add_delta_to_event_history`): dropped when the last
    /// stored event is less than a millisecond older.
    pub fn push(&mut self, time: u32, delta: f64) {
        if self
            .samples
            .back()
            .is_some_and(|&(last, _)| last > time.wrapping_sub(1))
        {
            return;
        }
        if self.samples.len() == PAN_HISTORY_LEN {
            self.samples.pop_front();
        }
        self.samples.push_back((time, delta));
    }

    /// The velocity at `time`, the release (`calculate_velocity`).
    pub fn velocity(&self, time: u32) -> f64 {
        let from = time.saturating_sub(HISTORY_MS);
        let mut span: Option<(u32, u32)> = None;
        let mut sum = 0.0;
        for &(at, delta) in self.samples.iter().filter(|(at, _)| *at >= from) {
            span = Some(span.map_or((at, at), |(first, _)| (first, at)));
            sum += delta;
        }
        match span {
            Some((first, last)) if last > first => sum / f64::from(last - first),
            _ => 0.0,
        }
    }
}

/// One vertical pointer or touch drag (`Clutter.PanGesture` with
/// `pan-axis` Y): nothing moves until the drag has gone
/// [`DRAG_THRESHOLD`] px vertically; from then on each event's delta
/// moves the curtain, the threshold travel itself staying unused.
#[derive(Debug, Default, Clone)]
pub struct Pan {
    last_offset: f64,
    total: f64,
    recognized: bool,
    history: PanHistory,
}

impl Pan {
    pub fn new(time: u32) -> Self {
        let mut history = PanHistory::default();
        history.begin(time);
        Self {
            history,
            ..Self::default()
        }
    }

    pub fn recognized(&self) -> bool {
        self.recognized
    }

    /// The drag is `offset_y` px from where it started at `time`: the
    /// vertical delta to apply, once recognized.
    pub fn motion(&mut self, offset_y: f64, time: u32) -> Option<f64> {
        let delta = offset_y - self.last_offset;
        self.last_offset = offset_y;
        self.history.push(time, delta);
        self.total += delta;
        if !self.recognized && self.total.abs() < DRAG_THRESHOLD {
            return None;
        }
        self.recognized = true;
        Some(delta)
    }

    /// The vertical velocity (px/ms, downward positive) at the release.
    pub fn velocity(&self, time: u32) -> f64 {
        self.history.velocity(time)
    }
}

/// A swipe between the two pages (`SwipeTracker` confirmed with snap
/// points `[0, 1]` and the current progress, `_swipeBegin`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Swipe {
    pub initial: f64,
    pub progress: f64,
    /// Where a cancelled swipe returns: the page it started nearest.
    pub cancel: f64,
}

impl Swipe {
    pub fn begin(progress: f64) -> Self {
        Self {
            initial: progress,
            progress,
            cancel: progress.round(),
        }
    }

    /// `_updateGesture`: `delta` swipe units over a page of `distance`.
    pub fn update(&mut self, delta: f64, distance: f64) -> f64 {
        self.progress = (self.progress + delta / distance.max(1.0)).clamp(0.0, 1.0);
        self.progress
    }

    /// Where the released swipe settles and how long it takes
    /// (`_getEndProgress` and `_endGesture`); `velocity` is in swipe
    /// units per ms, toward the prompt positive.
    pub fn end(&self, velocity: f64, distance: f64, cancelled: bool) -> (f64, f64) {
        release(
            self.progress,
            self.initial,
            self.cancel,
            velocity,
            distance,
            cancelled,
        )
    }
}

/// `_findClosestPoint` over `[0, 1]`: a tie picks the first.
fn closest(pos: f64) -> f64 {
    if (1.0 - pos).abs() < pos.abs() {
        1.0
    } else {
        0.0
    }
}

/// `_getEndProgress` and `_endGesture` for a pan (touch thresholds,
/// `isTouchpad` false) over snap points `[0, 1]`.
fn release(
    progress: f64,
    initial: f64,
    cancel: f64,
    velocity: f64,
    distance: f64,
    cancelled: bool,
) -> (f64, f64) {
    let end = if cancelled {
        cancel
    } else if velocity.abs() < VELOCITY_THRESHOLD_TOUCH {
        closest(progress)
    } else {
        let slope = DECELERATION_TOUCH / (1.0 - DECELERATION_TOUCH) / 1000.0;
        let speed = velocity.abs();
        let travel = if speed > VELOCITY_CURVE_THRESHOLD {
            let c = slope / 2.0 / DECELERATION_PARABOLA_MULTIPLIER;
            let x = speed - VELOCITY_CURVE_THRESHOLD + c;
            slope * VELOCITY_CURVE_THRESHOLD + DECELERATION_PARABOLA_MULTIPLIER * x * x
                - DECELERATION_PARABOLA_MULTIPLIER * c * c
        } else {
            speed * slope
        };
        let pos = (travel * velocity.signum() + progress).clamp(0.0, 1.0);
        // `_findPointForProjection`: a projection that stays on the
        // starting page still moves one page in the swipe's direction.
        let prev = if pos >= 1.0 { 1.0 } else { 0.0 };
        let next = if pos <= 0.0 { 0.0 } else { 1.0 };
        let start = closest(initial);
        if velocity > 0.0 && prev == start {
            next
        } else if velocity <= 0.0 && next == start {
            prev
        } else {
            closest(pos)
        }
    };
    let mut speed = velocity / distance.max(1.0);
    if (end - progress) * speed <= 0.0 {
        speed = ANIMATION_BASE_VELOCITY;
    }
    let points = (progress - end).abs().ceil().max(1.0);
    let longest = MAX_ANIMATION_MS * (1.0 + points).log2();
    let mut duration = ((progress - end) / speed * DURATION_MULTIPLIER).abs();
    if duration > 0.0 {
        duration = duration.clamp(MIN_ANIMATION_MS, longest);
    }
    (end, duration)
}

/// With `TUNA_TRANSIENT_TRACE` set, the curtain logs each ease, frame,
/// swipe and settle (the GTK shell proof's G-LOCK-CURTAIN gates). No
/// input, text or credential is ever logged.
fn trace(what: std::fmt::Arguments) {
    if std::env::var_os("TUNA_TRANSIENT_TRACE").is_some() {
        eprintln!("tuna-shell-gtk: unlock-dialog {what}");
    }
}

/// `-0.0` prints as `0.0`.
fn unsigned_zero(value: f64) -> f64 {
    if value == 0.0 {
        0.0
    } else {
        value
    }
}

/// What runs when an ease ends undisturbed.
type Settled = Rc<dyn Fn(Page)>;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Curtain {
        pub progress: Cell<f64>,
        pub active: Cell<Page>,
        pub motion: Cell<bool>,
        pub swipe: Cell<Option<Swipe>>,
        pub tween: Cell<Option<Tween>>,
        pub ticking: Cell<bool>,
        pub settled: RefCell<Option<Settled>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Curtain {
        const NAME: &'static str = "TunaLockCurtain";
        type Type = super::Curtain;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for Curtain {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for Curtain {
        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            // As big as the bigger page shown (GTK sizes hidden ones 0).
            let mut size = (0, 0);
            let mut child = self.obj().first_child();
            while let Some(page) = child {
                if page.should_layout() {
                    let (min, natural, _, _) = page.measure(orientation, for_size);
                    size = (size.0.max(min), size.1.max(natural));
                }
                child = page.next_sibling();
            }
            (size.0, size.1, -1, -1)
        }

        fn size_allocate(&self, width: i32, _height: i32, _baseline: i32) {
            let Some((clock, prompt)) = self.obj().pages() else {
                return;
            };
            let (clock_frame, prompt_frame) = frames(self.progress.get(), self.motion.get());
            for (page, frame) in [(clock, clock_frame), (prompt, prompt_frame)] {
                if page.should_layout() {
                    place(&page, width, frame);
                }
            }
        }
    }
}

/// Allocate `page` at its natural size, centred, its top margin placing
/// it, then moved and scaled about its own centre (GNOME's pivot point
/// 0.5, 0.5) as `frame` says.
fn place(page: &gtk::Widget, width: i32, frame: Frame) {
    let (min_width, natural_width, _, _) = page.measure(gtk::Orientation::Horizontal, -1);
    let page_width = natural_width.min(width).max(min_width);
    let (min_height, natural_height, _, _) = page.measure(gtk::Orientation::Vertical, page_width);
    let page_height = natural_height.max(min_height);
    let x = ((width - page_width) / 2) as f32;
    let top = page.margin_top();
    let content = (page_height - top - page.margin_bottom()).max(0);
    let cx = x + page_width as f32 / 2.0;
    let cy = top as f32 + content as f32 / 2.0;
    let scale = frame.scale.max(0.0) as f32;
    let transform = gtk::gsk::Transform::new()
        .translate(&gtk::graphene::Point::new(cx, cy + frame.dy as f32))
        .scale(scale, scale)
        .translate(&gtk::graphene::Point::new(-cx, -cy))
        .translate(&gtk::graphene::Point::new(x, 0.0));
    page.allocate(page_width, page_height, -1, Some(transform));
}

/// Take the keyboard focus out of `page`. GNOME gives it back to the
/// dialog when the prompt goes (`_maybeDestroyAuthPrompt`); here it goes
/// as soon as the clock is the page heading in, so nothing typed while
/// the prompt fades out can reach it, Return included.
fn drop_focus(page: &gtk::Widget) {
    if let Some(root) = page.root() {
        if root
            .focus()
            .is_some_and(|focus| focus == *page || focus.is_ancestor(page))
        {
            root.set_focus(gtk::Widget::NONE);
        }
    }
}

/// Show or hide a page; a hidden page never keeps the keyboard focus.
fn show(page: &gtk::Widget, visible: bool) {
    if page.is_visible() == visible {
        return;
    }
    if !visible {
        drop_focus(page);
    }
    page.set_visible(visible);
}

glib::wrapper! {
    /// The lock window's clock and prompt pages on GNOME's transition.
    pub struct Curtain(ObjectSubclass<imp::Curtain>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Curtain {
    /// The curtain over `clock`, the prompt hidden.
    pub fn new(clock: &impl IsA<gtk::Widget>, prompt: &impl IsA<gtk::Widget>) -> Self {
        let curtain: Self = glib::Object::new();
        clock.set_parent(&curtain);
        prompt.set_parent(&curtain);
        curtain
            .imp()
            .motion
            .set(crate::motion::current().allows_motion());
        curtain.apply();
        curtain
    }

    fn pages(&self) -> Option<(gtk::Widget, gtk::Widget)> {
        Some((self.first_child()?, self.last_child()?))
    }

    /// The page shown or being eased to.
    pub fn active(&self) -> Page {
        self.imp().active.get()
    }

    /// A drag is moving the curtain.
    pub fn swiping(&self) -> bool {
        self.imp().swipe.get().is_some()
    }

    /// Whether the prompt is on screen at all (`_promptBox.visible`).
    pub fn prompt_shown(&self) -> bool {
        self.pages().is_some_and(|(_, prompt)| prompt.is_visible())
    }

    /// Called whenever an ease ends undisturbed, with the page reached
    /// (the `onComplete` of `_showClock` and `_swipeEnd`).
    pub fn connect_settled(&self, settled: impl Fn(Page) + 'static) {
        *self.imp().settled.borrow_mut() = Some(Rc::new(settled));
    }

    /// `_showPrompt` / `_showClock`: ease to `page` over
    /// [`CROSSFADE_MS`] from wherever the curtain is.
    pub fn show_page(&self, page: Page) {
        let imp = self.imp();
        if imp.active.get() == page {
            return;
        }
        imp.active.set(page);
        if page == Page::Clock {
            self.leave_prompt();
        }
        self.ease_to(page.progress(), CROSSFADE_MS, Ease::OutQuad);
    }

    fn leave_prompt(&self) {
        if let Some((_, prompt)) = self.pages() {
            drop_focus(&prompt);
        }
    }

    /// `_swipeBegin`: stop any ease and follow from the current progress.
    /// Returns the page distance for drags (`_stack.height`, the taller
    /// page).
    pub fn swipe_begin(&self, source: &str) -> f64 {
        let imp = self.imp();
        imp.tween.set(None);
        imp.motion.set(crate::motion::current().allows_motion());
        imp.swipe.set(Some(Swipe::begin(imp.progress.get())));
        // Both pages take part while the pointer leads.
        self.apply();
        let distance = self.stack_height();
        trace(format_args!(
            "swipe begin source={source} progress={:.4} distance={distance:.0}",
            imp.progress.get()
        ));
        distance
    }

    /// `_swipeUpdate`: `delta` swipe units over a page of `distance`.
    pub fn swipe_update(&self, delta: f64, distance: f64) {
        let imp = self.imp();
        let Some(mut swipe) = imp.swipe.get() else {
            return;
        };
        imp.tween.set(None);
        let progress = swipe.update(delta, distance);
        imp.swipe.set(Some(swipe));
        self.set_progress(progress);
    }

    /// `_swipeEnd`: settle on the page the release velocity (swipe units
    /// per ms, toward the prompt positive) projects to. Returns that page.
    pub fn swipe_end(
        &self,
        source: &str,
        velocity: f64,
        distance: f64,
        cancelled: bool,
    ) -> Option<Page> {
        let imp = self.imp();
        let swipe = imp.swipe.take()?;
        let (end, duration) = swipe.end(velocity, distance, cancelled);
        let page = if end > 0.5 { Page::Prompt } else { Page::Clock };
        imp.active.set(page);
        if page == Page::Clock {
            self.leave_prompt();
        }
        trace(format_args!(
            "swipe end source={source} progress={:.6} initial={:.6} velocity={:.6} distance={distance:.0} cancelled={cancelled} end={end:.0} duration={duration:.3}",
            swipe.progress, swipe.initial, velocity
        ));
        self.ease_to(end, duration, Ease::OutCubic);
        Some(page)
    }

    /// `_stack.height`: the taller page's content, without the margin
    /// that places it.
    fn stack_height(&self) -> f64 {
        let width = self.width();
        let mut height = 0;
        let mut child = self.first_child();
        while let Some(page) = child {
            let (_, natural_width, _, _) = page.measure(gtk::Orientation::Horizontal, -1);
            let page_width = if width > 0 {
                natural_width.min(width)
            } else {
                natural_width
            };
            let (_, natural, _, _) = page.measure(gtk::Orientation::Vertical, page_width);
            height = height.max(natural - page.margin_top() - page.margin_bottom());
            child = page.next_sibling();
        }
        f64::from(height.max(1))
    }

    /// Ease the progress to `target` over `duration_ms` (before the
    /// motion policy's adjustment), superseding any running ease.
    fn ease_to(&self, target: f64, duration_ms: f64, ease: Ease) {
        let imp = self.imp();
        let policy = crate::motion::current();
        imp.motion.set(policy.allows_motion());
        let duration = policy.adjust_ms(duration_ms);
        let from = imp.progress.get();
        let instant = duration <= 0.0 || !self.is_mapped();
        let start_ms = if instant {
            0.0
        } else {
            crate::anim_clock::now_ms()
        };
        trace(format_args!(
            "ease to={} from={from:.4} duration={duration:.0} nominal={duration_ms:.0} mode={} motion={} clock={}",
            if target > 0.5 { "prompt" } else { "clock" },
            ease.name(),
            policy.allows_motion(),
            if instant {
                "instant"
            } else if crate::anim_clock::manual() {
                "manual"
            } else {
                "real"
            },
        ));
        if instant {
            imp.tween.set(None);
            self.set_progress(target);
            self.apply();
            self.settle();
            return;
        }
        imp.tween.set(Some(Tween {
            from,
            to: target,
            start_ms,
            duration_ms: duration,
            ease,
        }));
        self.apply();
        if !imp.ticking.replace(true) {
            // The callback gets the curtain itself: it holds no reference.
            self.add_tick_callback(|curtain, _| curtain.tick());
        }
    }

    fn tick(&self) -> glib::ControlFlow {
        let imp = self.imp();
        let Some(tween) = imp.tween.get() else {
            imp.ticking.set(false);
            return glib::ControlFlow::Break;
        };
        let (value, done) = tween.at(crate::anim_clock::now_ms());
        self.set_progress(value);
        if !done {
            return glib::ControlFlow::Continue;
        }
        imp.tween.set(None);
        imp.ticking.set(false);
        self.apply();
        self.settle();
        glib::ControlFlow::Break
    }

    fn settle(&self) {
        let imp = self.imp();
        let page = imp.active.get();
        trace(format_args!(
            "settled page={} progress={:.4}",
            page.name(),
            imp.progress.get()
        ));
        let settled = imp.settled.borrow().clone();
        if let Some(settled) = settled {
            settled(page);
        }
    }

    fn set_progress(&self, progress: f64) {
        let imp = self.imp();
        let progress = progress.clamp(0.0, 1.0);
        if imp.progress.replace(progress) == progress {
            return;
        }
        self.apply();
        let motion = imp.motion.get();
        let (clock, prompt) = frames(progress, motion);
        trace(format_args!(
            "frame progress={progress:.4} motion={motion} clock={:.3},{:.1},{:.3} prompt={:.3},{:.1},{:.3}",
            clock.opacity,
            unsigned_zero(clock.dy),
            clock.scale,
            prompt.opacity,
            unsigned_zero(prompt.dy),
            prompt.scale,
        ));
    }

    /// Both pages at the current progress: shown while on screen (and,
    /// for the page being eased to or a swipe, from the start), at their
    /// opacity, placed on the next allocation.
    fn apply(&self) {
        let imp = self.imp();
        let Some((clock, prompt)) = self.pages() else {
            return;
        };
        let progress = imp.progress.get();
        let active = imp.active.get();
        let swiping = imp.swipe.get().is_some();
        show(&clock, progress < 1.0 || active == Page::Clock || swiping);
        show(&prompt, progress > 0.0 || active == Page::Prompt || swiping);
        let (clock_frame, prompt_frame) = frames(progress, imp.motion.get());
        clock.set_opacity(clock_frame.opacity.clamp(0.0, 1.0));
        prompt.set_opacity(prompt_frame.opacity.clamp(0.0, 1.0));
        self.queue_allocate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn pages_trade_places_like_unlock_dialog_51() {
        let (clock, prompt) = frames(0.0, true);
        assert!(close(clock.opacity, 1.0) && close(clock.dy, 0.0) && close(clock.scale, 1.0));
        assert!(close(prompt.opacity, 0.0) && close(prompt.dy, 200.0));
        assert!(close(prompt.scale, 0.3));
        // Half way: both at half opacity, 100px off and at 65%.
        let (clock, prompt) = frames(0.5, true);
        assert!(close(clock.opacity, 0.5) && close(clock.dy, -100.0) && close(clock.scale, 0.65));
        assert!(close(prompt.opacity, 0.5) && close(prompt.dy, 100.0));
        assert!(close(prompt.scale, 0.65));
        // The proof's frozen sample: 150 ms into the 300 ms ease-out quad.
        let (clock, prompt) = frames(0.75, true);
        assert!(close(clock.opacity, 0.25) && close(clock.dy, -150.0));
        assert!(close(clock.scale, 0.475));
        assert!(close(prompt.opacity, 0.75) && close(prompt.dy, 50.0));
        assert!(close(prompt.scale, 0.825));
        let (clock, prompt) = frames(1.0, true);
        assert!(close(clock.opacity, 0.0) && close(clock.dy, -200.0) && close(clock.scale, 0.3));
        assert!(close(prompt.opacity, 1.0) && close(prompt.dy, 0.0) && close(prompt.scale, 1.0));
    }

    #[test]
    fn reduced_motion_keeps_only_the_fade() {
        for p in [0.0, 0.25, 0.75, 1.0] {
            let (clock, prompt) = frames(p, false);
            assert!(close(clock.opacity, 1.0 - p) && close(prompt.opacity, p));
            for frame in [clock, prompt] {
                assert_eq!((frame.dy, frame.scale), (0.0, 1.0));
            }
        }
    }

    #[test]
    fn crossfade_is_300_ms_ease_out_quad_from_the_current_value() {
        let show = Tween {
            from: 0.0,
            to: 1.0,
            start_ms: 1000.0,
            duration_ms: CROSSFADE_MS,
            ease: Ease::OutQuad,
        };
        assert_eq!(show.at(1000.0), (0.0, false));
        assert_eq!(show.at(1150.0), (0.75, false));
        assert_eq!(show.at(1300.0), (1.0, true));
        assert_eq!(show.at(5000.0), (1.0, true));
        // Escape half way back: from the drawn value, not from 1.
        let hide = Tween {
            from: 0.75,
            to: 0.0,
            ..show
        };
        assert!(close(hide.at(1150.0).0, 0.1875));
        let swipe = Tween {
            from: 0.4,
            to: 1.0,
            start_ms: 0.0,
            duration_ms: 200.0,
            ease: Ease::OutCubic,
        };
        assert!(close(swipe.at(100.0).0, 0.4 + 0.6 * 0.875));
        let instant = Tween {
            duration_ms: 0.0,
            ..swipe
        };
        let (value, done) = instant.at(0.0);
        assert!(close(value, 1.0) && done);
    }

    #[test]
    fn a_drag_moves_nothing_until_16_px_then_follows_each_event() {
        let mut pan = Pan::new(1000);
        assert_eq!(pan.motion(-10.0, 1010), None);
        assert_eq!(pan.motion(-15.9, 1020), None);
        // The event that crosses the threshold moves by its own delta.
        let delta = pan.motion(-26.0, 1030).unwrap();
        assert!(close(delta, -10.1));
        assert!(pan.recognized());
        assert!(close(pan.motion(-36.0, 1040).unwrap(), -10.0));
        // Moving back down counts as well.
        assert!(close(pan.motion(-30.0, 1050).unwrap(), 6.0));
    }

    #[test]
    fn pan_velocity_is_clutters_last_150_ms() {
        let mut pan = Pan::new(0);
        for (i, y) in [-20.0, -40.0, -60.0, -80.0].into_iter().enumerate() {
            pan.motion(y, 40 * (i as u32 + 1));
        }
        // Press (0) and four 20 px moves over 160 ms; at 165 ms the press
        // has left the window: 80 px over 120 ms, every delta counted.
        assert!(close(pan.velocity(165), -80.0 / 120.0));
        // A pause before release: no recent motion, no velocity.
        assert_eq!(pan.velocity(500), 0.0);
        // Events within a millisecond of the last stored one are dropped.
        let mut history = PanHistory::default();
        history.begin(10);
        history.push(10, -5.0);
        history.push(20, -10.0);
        history.push(20, -10.0);
        history.push(30, -10.0);
        assert!(close(history.velocity(30), -20.0 / 20.0));
    }

    #[test]
    fn slow_releases_settle_on_the_nearer_page() {
        let mut swipe = Swipe::begin(0.0);
        assert!(close(swipe.update(90.0, 300.0), 0.3));
        let (end, duration) = swipe.end(0.0, 300.0, false);
        assert_eq!(end, 0.0);
        // Back at ANIMATION_BASE_VELOCITY: 0.3 / 0.002 * 3 = 450, capped.
        assert!(close(duration, 400.0));
        swipe.update(120.0, 300.0);
        let (end, duration) = swipe.end(0.1, 300.0, false);
        assert_eq!(end, 1.0);
        // Too slow to project: the nearer page, the prompt; capped at
        // 400 ms (0.3 / (0.1 / 300) * 3 = 2700 ms).
        assert!(close(duration, 400.0));
        // Exactly half way is a tie: the first snap point, the clock.
        let half = Swipe {
            initial: 0.0,
            progress: 0.5,
            cancel: 0.0,
        };
        assert_eq!(half.end(0.0, 300.0, false).0, 0.0);
        // Never past either page.
        let mut far = Swipe::begin(1.0);
        assert_eq!(far.update(500.0, 300.0), 1.0);
        assert_eq!(far.update(-900.0, 300.0), 0.0);
    }

    #[test]
    fn a_flick_reaches_the_other_page_from_a_short_drag() {
        let mut swipe = Swipe::begin(0.0);
        swipe.update(30.0, 300.0);
        // 0.6 px/ms toward the prompt beats the 0.3 touch threshold.
        let (end, duration) = swipe.end(0.6, 300.0, false);
        assert_eq!(end, 1.0);
        // (1 - 0.1) / (0.6 / 300) * 3 = 1350 ms, capped at 400.
        assert!(close(duration, 400.0));
        // A fast flick finishes sooner: 0.9 / (6 / 300) * 3 = 135 ms.
        let (_, fast) = swipe.end(6.0, 300.0, false);
        assert!(close(fast, 135.0));
        // Toward the clock from the prompt.
        let mut down = Swipe::begin(1.0);
        down.update(-30.0, 300.0);
        assert_eq!(down.end(-0.6, 300.0, false).0, 0.0);
        // Below the touch threshold the nearer page wins.
        assert_eq!(swipe.end(0.29, 300.0, false).0, 0.0);
        // Velocity against the end slows to the base rate, min 100 ms.
        let near = Swipe {
            initial: 0.0,
            progress: 0.99,
            cancel: 0.0,
        };
        let (end, duration) = near.end(0.0, 300.0, false);
        assert_eq!(end, 1.0);
        assert!(close(duration, 100.0));
    }

    #[test]
    fn a_cancelled_swipe_returns_to_where_it_began() {
        let mut swipe = Swipe::begin(0.0);
        swipe.update(250.0, 300.0);
        assert_eq!(swipe.end(5.0, 300.0, true).0, 0.0);
        let mut back = Swipe::begin(1.0);
        back.update(-250.0, 300.0);
        assert_eq!(back.end(-5.0, 300.0, true).0, 1.0);
    }
}

#[cfg(test)]
mod gtk_curtain_tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn pump_until(limit: Duration, mut done: impl FnMut() -> bool) -> bool {
        let context = glib::MainContext::default();
        let start = Instant::now();
        while start.elapsed() < limit {
            while context.pending() {
                context.iteration(false);
            }
            if done() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        false
    }

    fn bounds(page: &gtk::Widget, curtain: &Curtain) -> gtk::graphene::Rect {
        page.compute_bounds(curtain).expect("page bounds")
    }

    fn near(a: f32, b: f32, what: &str) {
        assert!((a - b).abs() <= 1.5, "{what}: {a} vs {b}");
    }

    /// GTK keeps a widget's opacity as an 8-bit alpha (`gtk_widget_set_opacity`).
    fn alpha(opacity: f64, want: f64) -> bool {
        (opacity - want).abs() <= 1.0 / 255.0
    }

    /// Real GTK widgets: the pages are allocated at GNOME's frames about
    /// their own centres, the hidden page leaves focus and the tree, and
    /// Reduced Motion and animations off keep to GNOME's policy.
    #[test]
    #[ignore = "requires owned Xvfb/private D-Bus and explicit curtain admission"]
    fn actual_gtk_curtain_pages_follow_progress_and_policy() {
        assert_eq!(
            std::env::var("TUNA_REQUIRE_LOCK_CURTAIN").as_deref(),
            Ok("1")
        );
        assert!(std::env::var_os("WAYLAND_DISPLAY").is_none());
        gtk::init().expect("actual GTK display required");
        crate::motion::set(tuna_shell_control::MotionPolicy::default());
        let clock = gtk::Box::new(gtk::Orientation::Vertical, 0);
        clock.append(&gtk::Label::new(Some("12:00")));
        clock.append(&gtk::Label::new(Some("Click or press a key")));
        clock.set_margin_top(120);
        let entry = gtk::PasswordEntry::new();
        let prompt = gtk::Box::new(gtk::Orientation::Vertical, 0);
        prompt.append(&gtk::Label::new(Some("User")));
        prompt.append(&entry);
        prompt.set_margin_top(80);
        let curtain = Curtain::new(&clock, &prompt);
        let settled = Rc::new(RefCell::new(Vec::new()));
        {
            let settled = settled.clone();
            curtain.connect_settled(move |page| settled.borrow_mut().push(page));
        }
        let window = gtk::Window::new();
        window.set_default_size(800, 600);
        window.set_child(Some(&curtain));
        window.present();
        assert!(pump_until(Duration::from_secs(5), || curtain.is_mapped()
            && curtain.width() > 0));
        let clock_widget: gtk::Widget = clock.clone().upcast();
        let prompt_widget: gtk::Widget = prompt.clone().upcast();
        assert!(clock.is_visible() && !prompt.is_visible());
        let clock_rest = bounds(&clock_widget, &curtain);
        near(clock_rest.y(), 120.0, "the clock sits at its margin");

        // A swipe: both pages shown; at 0.5 each is half faded, 65% about
        // its centre and 100 px off.
        let distance = curtain.swipe_begin("test");
        assert!(distance > 1.0);
        assert!(clock.is_visible() && prompt.is_visible());
        curtain.swipe_update(0.5 * distance, distance);
        assert!(pump_until(Duration::from_secs(2), || {
            let b = bounds(&clock_widget, &curtain);
            (b.height() - clock_rest.height() * 0.65).abs() <= 1.5
        }));
        let half = bounds(&clock_widget, &curtain);
        near(half.height(), clock_rest.height() * 0.65, "clock scale");
        near(half.width(), clock_rest.width() * 0.65, "clock scale x");
        let rest_centre = clock_rest.y() + clock_rest.height() / 2.0;
        near(
            half.y() + half.height() / 2.0,
            rest_centre - 100.0,
            "clock rise",
        );
        near(
            half.x() + half.width() / 2.0,
            clock_rest.x() + clock_rest.width() / 2.0,
            "pivot x",
        );
        assert!(alpha(clock.opacity(), 0.5), "clock {}", clock.opacity());
        assert!(alpha(prompt.opacity(), 0.5), "prompt {}", prompt.opacity());
        let prompt_half = bounds(&prompt_widget, &curtain);

        // Released slowly past half: the prompt, ease-out cubic.
        curtain.swipe_update(0.1 * distance, distance);
        assert_eq!(
            curtain.swipe_end("test", 0.0, distance, false),
            Some(Page::Prompt)
        );
        assert!(pump_until(Duration::from_secs(3), || settled
            .borrow()
            .last()
            .copied()
            == Some(Page::Prompt)));
        assert!(!clock.is_visible() && prompt.is_visible());
        let prompt_rest = bounds(&prompt_widget, &curtain);
        near(prompt_rest.y(), 80.0, "the prompt sits at its margin");
        near(
            prompt_half.y() + prompt_half.height() / 2.0,
            prompt_rest.y() + prompt_rest.height() / 2.0 + 100.0,
            "prompt rose from below",
        );
        near(
            prompt_half.height(),
            prompt_rest.height() * 0.65,
            "prompt scale",
        );

        // The prompt leaves: it gives up the focus before it hides.
        entry.grab_focus();
        let focused = || GtkWindowExt::focus(&window);
        assert!(focused().is_some_and(|f| f.is_ancestor(&prompt)));
        curtain.show_page(Page::Clock);
        assert!(pump_until(Duration::from_secs(3), || settled
            .borrow()
            .last()
            .copied()
            == Some(Page::Clock)));
        assert!(!prompt.is_visible() && clock.is_visible());
        assert!(!focused().is_some_and(|f| f.is_ancestor(&prompt)));
        let back = bounds(&clock_widget, &curtain);
        near(back.y(), clock_rest.y(), "clock back in place");
        near(back.height(), clock_rest.height(), "clock back to size");

        // Reduced Motion: the same swipe only fades.
        crate::motion::set(tuna_shell_control::MotionPolicy::from_gnome(
            true, true, 1.0,
        ));
        let distance = curtain.swipe_begin("test");
        curtain.swipe_update(0.5 * distance, distance);
        assert!(pump_until(Duration::from_secs(2), || {
            alpha(clock.opacity(), 0.5)
        }));
        // Let an allocation pass, then the clock is where it rests.
        let _ = pump_until(Duration::from_millis(200), || false);
        let faded = bounds(&clock_widget, &curtain);
        near(faded.y(), clock_rest.y(), "no rise under Reduced Motion");
        near(
            faded.height(),
            clock_rest.height(),
            "no shrink under Reduced Motion",
        );
        assert_eq!(
            curtain.swipe_end("test", 0.0, distance, true),
            Some(Page::Clock)
        );
        assert!(pump_until(Duration::from_secs(3), || !prompt.is_visible()));

        // Animations off: the prompt is up at once.
        crate::motion::set(tuna_shell_control::MotionPolicy::from_gnome(
            false, false, 1.0,
        ));
        let before = settled.borrow().len();
        curtain.show_page(Page::Prompt);
        assert_eq!(curtain.imp().progress.get(), 1.0);
        assert_eq!(settled.borrow().len(), before + 1);
        assert!(prompt.is_visible() && !clock.is_visible());
        crate::motion::set(tuna_shell_control::MotionPolicy::default());
        println!("LOCK-CURTAIN pass pages follow GNOME 51 frames, focus leaves hidden pages, Reduced Motion fades only, animations off land at once");
    }
}
