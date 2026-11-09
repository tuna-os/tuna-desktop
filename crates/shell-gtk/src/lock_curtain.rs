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
//! session hands touch to the lock surface as the primary button), a
//! three-finger touchpad swipe ([`TouchpadSwipe`]) or a two-finger
//! touchpad scroll ([`ScrollGesture`], ended by the axis stop) moves the
//! value; on release `SwipeTracker` projects the release velocity onto
//! the two pages and finishes in 100-400 ms ease-out cubic.
//!
//! The clock's hint ([`Hint`], `UnlockDialogClock._hint`) is hidden
//! until the session has been idle for [`HINT_TIMEOUT_MS`], then fades
//! in over [`CROSSFADE_MS`]; it says "Swipe up" in touch mode and "Click
//! or press a key" otherwise ([`hint_text`]).
//!
//! Reduced Motion keeps the fades and drops the translation and scale
//! (as `_setTransitionProgress` does); animations off land at once; the
//! slow-down factor stretches every ease. Time is
//! [`crate::anim_clock`], so a nested proof can sample the curtain and
//! the hint on the compositor's frozen clock.
//!
//! Only the two pages move inside the lock window: the window, its
//! input and the authentication path are untouched. A page is hidden
//! (not just transparent) once it is fully off, so it takes no input,
//! keeps no keyboard focus and leaves the accessible tree.
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::Duration;

use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use gtk4::subclass::prelude::*;

use crate::transient::{ease_out_quad, lerp, Frame};
use crate::ws_dots::ease_out_cubic;
use tuna_shell_control::MotionPolicy;

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
/// `TOUCHPAD_BASE_HEIGHT`: the page a touchpad swipe or scroll moves
/// over (swipeTracker.js passes it, not the stack's height, for
/// touchpad updates and releases).
pub const TOUCHPAD_DISTANCE: f64 = 300.0;
const VELOCITY_THRESHOLD_TOUCHPAD: f64 = 0.6;
const DECELERATION_TOUCHPAD: f64 = 0.997;
/// `GESTURE_FINGER_COUNT`: fewer fingers are not a swipe.
pub const SWIPE_FINGERS: u32 = 3;
/// `HINT_TIMEOUT` (unlockDialog.js:31): idle time before the hint shows.
pub const HINT_TIMEOUT_MS: u32 = 4000;

/// What moves the curtain: a pointer or touch drag, or a touchpad
/// swipe or scroll (`isTouchpad` in `_endGesture`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Source {
    #[default]
    Drag,
    Swipe,
    Scroll,
}

impl Source {
    pub fn name(self) -> &'static str {
        match self {
            Source::Drag => "drag",
            Source::Swipe => "swipe",
            Source::Scroll => "scroll",
        }
    }

    pub fn touchpad(self) -> bool {
        self != Source::Drag
    }
}

/// swipeTracker.js `EventHistory`: the touchpad deltas of the last
/// 150 ms. `trim` keeps everything when nothing is that recent, as
/// `splice(0, -1)` removes nothing.
#[derive(Debug, Default, Clone)]
pub struct EventHistory {
    samples: VecDeque<(u32, f64)>,
}

impl EventHistory {
    pub fn trim(&mut self, time: u32) {
        let threshold = i64::from(time) - i64::from(HISTORY_MS);
        if let Some(index) = self
            .samples
            .iter()
            .position(|&(at, _)| i64::from(at) >= threshold)
        {
            self.samples.drain(..index);
        }
    }

    pub fn append(&mut self, time: u32, delta: f64) {
        self.trim(time);
        self.samples.push_back((time, delta));
    }

    /// `calculateVelocity`: every delta but the first over the time the
    /// samples span, in delta units per ms.
    pub fn velocity(&self) -> f64 {
        let (Some(&(first, _)), Some(&(last, _))) = (self.samples.front(), self.samples.back())
        else {
            return 0.0;
        };
        if self.samples.len() < 2 || first == last {
            return 0.0;
        }
        let total: f64 = self.samples.iter().skip(1).map(|&(_, delta)| delta).sum();
        total / (f64::from(last) - f64::from(first))
    }

    pub fn reset(&mut self) {
        self.samples.clear();
    }
}

/// What a touchpad gesture hands the tracker: a begin (with the delta of
/// the event that began it), an update, or the end.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Step {
    Begin { time: u32, delta: f64 },
    Update { time: u32, delta: f64 },
    End { time: u32 },
}

/// A touchpad swipe event's phase; GNOME ends on a cancel as on an end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwipePhase {
    Begin,
    Update,
    End,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum PadState {
    #[default]
    None,
    Pending,
    Handling,
    Ignored,
}

/// swipeTracker.js `TouchpadSwipeGesture`, vertical: three or more
/// fingers; nothing moves until they have gone [`DRAG_THRESHOLD`] px,
/// then a vertical swipe is handled and a horizontal one ignored to its
/// end. The deltas are the fingers', turned round under natural
/// scrolling.
#[derive(Debug, Default, Clone)]
pub struct TouchpadSwipe {
    state: PadState,
    cumulative: (f64, f64),
}

impl TouchpadSwipe {
    /// One touchpad swipe event (`_handleEvent`). Returns what the
    /// tracker hears and whether the event is the curtain's.
    pub fn event(
        &mut self,
        phase: SwipePhase,
        fingers: u32,
        (dx, dy): (f64, f64),
        time: u32,
        natural_scroll: bool,
    ) -> (Option<Step>, bool) {
        if phase == SwipePhase::Begin {
            self.state = PadState::None;
        }
        if fingers < SWIPE_FINGERS || self.state == PadState::Ignored {
            return (None, false);
        }
        if self.state == PadState::None {
            if dx == 0.0 && dy == 0.0 {
                return (None, false);
            }
            self.cumulative = (0.0, 0.0);
            self.state = PadState::Pending;
        }
        let mut began = false;
        if self.state == PadState::Pending {
            self.cumulative.0 += dx;
            self.cumulative.1 += dy;
            let (cx, cy) = self.cumulative;
            if cx.hypot(cy) < DRAG_THRESHOLD {
                return (None, false);
            }
            self.cumulative = (0.0, 0.0);
            if cx.abs() > cy.abs() {
                self.state = PadState::Ignored;
                return (None, false);
            }
            self.state = PadState::Handling;
            began = true;
        }
        let step = match phase {
            SwipePhase::Begin | SwipePhase::Update => {
                let delta = if natural_scroll { -dy } else { dy };
                if began {
                    Step::Begin { time, delta }
                } else {
                    Step::Update { time, delta }
                }
            }
            SwipePhase::End => {
                self.state = PadState::None;
                Step::End { time }
            }
        };
        (Some(step), self.state == PadState::Handling)
    }
}

/// swipeTracker.js `ScrollGesture`, vertical: a two-finger touchpad
/// scroll follows from its first moving event and ends on the axis stop.
/// GTK's surface-unit deltas are libinput's pixels, Clutter's deltas
/// times `SCROLL_MULTIPLIER`, so they are the tracker's deltas as they
/// come.
#[derive(Debug, Default, Clone)]
pub struct ScrollGesture {
    began: bool,
}

impl ScrollGesture {
    /// One smooth touchpad scroll; both deltas zero is the stop.
    pub fn scroll(&mut self, dx: f64, dy: f64, time: u32) -> Step {
        if dx == 0.0 && dy == 0.0 {
            return self.stop(time);
        }
        if self.began {
            Step::Update { time, delta: dy }
        } else {
            self.began = true;
            Step::Begin { time, delta: dy }
        }
    }

    /// The axis stop (`get_scroll_delta` 0, 0): the end, even of a
    /// scroll that never began (the tracker ignores that one).
    pub fn stop(&mut self, time: u32) -> Step {
        self.began = false;
        Step::End { time }
    }
}

/// `Clock._updateHint` without an authentication mechanism's hint.
pub fn hint_text(touch_mode: bool) -> &'static str {
    if touch_mode {
        "Swipe up"
    } else {
        "Click or press a key"
    }
}

/// The hint's opacity (`_hint.opacity`, unlockDialog.js:378-409): 0
/// until the idle watch fires, then eased to 1 over [`CROSSFADE_MS`] from
/// where it stands, on Clutter's default mode, ease-out cubic (mutter
/// 51.0 clutter-actor.c:17306, `ease` names no mode here); a power-save
/// change sets 0 at once, dropping any ease.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct HintFade {
    opacity: f64,
    tween: Option<Tween>,
}

impl HintFade {
    pub fn opacity(&self) -> f64 {
        self.opacity
    }

    pub fn easing(&self) -> bool {
        self.tween.is_some()
    }

    /// The idle watch fired at `now`. Returns the ease's length under
    /// `policy`; 0 lands at once (animations off). Reduced Motion keeps
    /// the fade, as it keeps every opacity change.
    pub fn show(&mut self, now: Duration, policy: MotionPolicy) -> f64 {
        let duration = policy.adjust_ms(CROSSFADE_MS);
        if duration <= 0.0 {
            self.opacity = 1.0;
            self.tween = None;
        } else {
            self.tween = Some(Tween {
                from: self.opacity,
                to: 1.0,
                start: now,
                duration_ms: duration,
                ease: Ease::OutCubic,
            });
        }
        duration
    }

    /// `power-save-mode-changed`: transparent at once.
    pub fn hide(&mut self) {
        self.opacity = 0.0;
        self.tween = None;
    }

    /// Move the ease to `now`; true while it still runs.
    pub fn tick(&mut self, now: Duration) -> bool {
        let Some(tween) = self.tween else {
            return false;
        };
        let (value, done) = tween.at(now);
        self.opacity = value;
        if done {
            self.tween = None;
        }
        !done
    }
}

/// The clock's hint label on [`HintFade`], stepped on the label's frame
/// clock and timed on [`crate::anim_clock`].
pub struct Hint {
    label: gtk::Label,
    fade: Cell<HintFade>,
    ticking: Cell<bool>,
}

impl Hint {
    /// The hint over `label`, transparent, saying what `touch_mode` asks.
    pub fn new(label: &gtk::Label, touch_mode: bool) -> Rc<Self> {
        label.set_opacity(0.0);
        let hint = Rc::new(Self {
            label: label.clone(),
            fade: Cell::new(HintFade::default()),
            ticking: Cell::new(false),
        });
        hint.set_touch_mode(touch_mode);
        hint
    }

    /// `_updateHint` on `notify::touch-mode`.
    pub fn set_touch_mode(&self, touch_mode: bool) {
        let text = hint_text(touch_mode);
        if self.label.label() != text {
            self.label.set_label(text);
            trace(format_args!("hint text={text}"));
        }
    }

    /// The idle watch fired: ease in. An unmapped hint lands at once.
    pub fn show(self: &Rc<Self>) {
        let mapped = self.label.is_mapped();
        let policy = if mapped {
            crate::motion::current()
        } else {
            MotionPolicy::from_gnome(false, false, 1.0)
        };
        let mut fade = self.fade.get();
        let from = fade.opacity();
        let now = if mapped {
            crate::anim_clock::now()
        } else {
            Duration::ZERO
        };
        let duration = fade.show(now, policy);
        self.fade.set(fade);
        trace(format_args!(
            "hint ease to=1 from={from:.3} duration={duration:.0} nominal={CROSSFADE_MS:.0} mode=ease-out-cubic clock={}",
            if !fade.easing() {
                "instant"
            } else if crate::anim_clock::manual() {
                "manual"
            } else {
                "real"
            }
        ));
        self.apply();
        if !fade.easing() {
            trace(format_args!("hint settled opacity={:.3}", fade.opacity()));
        } else if !self.ticking.replace(true) {
            let weak = Rc::downgrade(self);
            self.label.add_tick_callback(move |_, _| {
                weak.upgrade()
                    .map_or(glib::ControlFlow::Break, |hint| hint.tick())
            });
        }
    }

    /// A power-save change: transparent at once.
    pub fn hide(&self) {
        let mut fade = self.fade.get();
        fade.hide();
        self.fade.set(fade);
        self.apply();
        trace(format_args!("hint hidden"));
    }

    fn tick(&self) -> glib::ControlFlow {
        let mut fade = self.fade.get();
        let before = fade.opacity();
        let running = fade.tick(crate::anim_clock::now());
        self.fade.set(fade);
        if fade.opacity() != before {
            self.apply();
            trace(format_args!("hint frame opacity={:.3}", fade.opacity()));
        }
        if running {
            return glib::ControlFlow::Continue;
        }
        self.ticking.set(false);
        if fade.opacity() >= 1.0 {
            trace(format_args!("hint settled opacity={:.3}", fade.opacity()));
        }
        glib::ControlFlow::Break
    }

    fn apply(&self) {
        self.label
            .set_opacity(self.fade.get().opacity().clamp(0.0, 1.0));
    }
}

const IDLE_MONITOR: &str = "org.gnome.Mutter.IdleMonitor";
const IDLE_MONITOR_CORE: &str = "/org/gnome/Mutter/IdleMonitor/Core";
const DISPLAY_CONFIG: &str = "org.gnome.Mutter.DisplayConfig";
const DISPLAY_CONFIG_PATH: &str = "/org/gnome/Mutter/DisplayConfig";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum IdleWatch {
    #[default]
    Off,
    /// AddIdleWatch sent, no reply yet.
    Pending,
    Armed(u32),
}

/// What the hints follow while the lock screens are up: the core idle
/// monitor's [`HINT_TIMEOUT_MS`] watch (`add_idle_watch`, over
/// org.gnome.Mutter.IdleMonitor), mutter's power-save changes (the
/// DisplayConfig `PowerSaveMode` property) and the seat's touch-mode.
#[derive(Default)]
pub struct HintWatch {
    hints: RefCell<Vec<std::rc::Weak<Hint>>>,
    touch_mode: Cell<bool>,
    watch: Cell<IdleWatch>,
    /// Watch ids fired before AddIdleWatch answered: a session already
    /// idle that long fires at once, as Mutter's does.
    early: RefCell<Vec<u32>>,
    conn: RefCell<Option<gio::DBusConnection>>,
    subscriptions: RefCell<Vec<gio::SignalSubscription>>,
}

impl HintWatch {
    pub fn new() -> Rc<Self> {
        Rc::default()
    }

    /// A lock window's hint joins: it says what touch-mode asks and
    /// shows when the watch fires.
    pub fn add(&self, hint: &Rc<Hint>) {
        hint.set_touch_mode(self.touch_mode.get());
        self.hints.borrow_mut().push(Rc::downgrade(hint));
    }

    /// The seat's touch-mode changed (`notify::touch-mode`).
    pub fn set_touch_mode(&self, touch_mode: bool) {
        self.touch_mode.set(touch_mode);
        for hint in self.live() {
            hint.set_touch_mode(touch_mode);
        }
    }

    fn live(&self) -> Vec<Rc<Hint>> {
        let mut hints = self.hints.borrow_mut();
        hints.retain(|hint| hint.strong_count() > 0);
        hints.iter().filter_map(std::rc::Weak::upgrade).collect()
    }

    fn fire(&self) {
        for hint in self.live() {
            hint.show();
        }
    }

    /// The lock screens are up: watch for [`HINT_TIMEOUT_MS`] of idle
    /// time and for power-save changes. Without a session bus the hints
    /// stay hidden.
    pub fn arm(self: &Rc<Self>) {
        if self.watch.get() != IdleWatch::Off {
            return;
        }
        let Ok(conn) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) else {
            return;
        };
        let weak = Rc::downgrade(self);
        let fired = conn.subscribe_to_signal(
            Some(IDLE_MONITOR),
            Some(IDLE_MONITOR),
            Some("WatchFired"),
            Some(IDLE_MONITOR_CORE),
            None,
            gio::DBusSignalFlags::NONE,
            move |signal| {
                let (Some(watch), Some((id,))) =
                    (weak.upgrade(), signal.parameters.get::<(u32,)>())
                else {
                    return;
                };
                match watch.watch.get() {
                    IdleWatch::Armed(armed) if armed == id => watch.fire(),
                    IdleWatch::Pending => watch.early.borrow_mut().push(id),
                    _ => {}
                }
            },
        );
        let weak = Rc::downgrade(self);
        let power = conn.subscribe_to_signal(
            Some(DISPLAY_CONFIG),
            Some("org.freedesktop.DBus.Properties"),
            Some("PropertiesChanged"),
            Some(DISPLAY_CONFIG_PATH),
            Some(DISPLAY_CONFIG),
            gio::DBusSignalFlags::NONE,
            move |signal| {
                let Some(watch) = weak.upgrade() else {
                    return;
                };
                let changed = signal
                    .parameters
                    .get::<(String, glib::VariantDict, Vec<String>)>()
                    .is_some_and(|(_, changed, invalidated)| {
                        changed.contains("PowerSaveMode")
                            || invalidated.iter().any(|name| name == "PowerSaveMode")
                    });
                if changed {
                    for hint in watch.live() {
                        hint.hide();
                    }
                }
            },
        );
        *self.subscriptions.borrow_mut() = vec![fired, power];
        self.watch.set(IdleWatch::Pending);
        let weak = Rc::downgrade(self);
        conn.call(
            Some(IDLE_MONITOR),
            IDLE_MONITOR_CORE,
            IDLE_MONITOR,
            "AddIdleWatch",
            Some(&(u64::from(HINT_TIMEOUT_MS),).to_variant()),
            glib::VariantTy::new("(u)").ok(),
            gio::DBusCallFlags::NO_AUTO_START,
            1000,
            gio::Cancellable::NONE,
            move |reply| {
                let id = reply.ok().and_then(|r| r.get::<(u32,)>()).map(|(id,)| id);
                let Some(watch) = weak.upgrade() else {
                    return;
                };
                let early = std::mem::take(&mut *watch.early.borrow_mut());
                match (watch.watch.get(), id) {
                    (IdleWatch::Pending, Some(id)) => {
                        watch.watch.set(IdleWatch::Armed(id));
                        if early.contains(&id) {
                            watch.fire();
                        }
                    }
                    // Disarmed meanwhile: give the watch back.
                    (IdleWatch::Off, Some(id)) => watch.remove(id),
                    (IdleWatch::Pending, None) => watch.watch.set(IdleWatch::Off),
                    _ => {}
                }
            },
        );
        *self.conn.borrow_mut() = Some(conn);
    }

    fn remove(&self, id: u32) {
        if let Some(conn) = self.conn.borrow().as_ref() {
            conn.call(
                Some(IDLE_MONITOR),
                IDLE_MONITOR_CORE,
                IDLE_MONITOR,
                "RemoveWatch",
                Some(&(id,).to_variant()),
                None,
                gio::DBusCallFlags::NO_AUTO_START,
                1000,
                gio::Cancellable::NONE,
                |_| {},
            );
        }
    }

    /// The lock screens went: drop the watch (`_onDestroy`) and the
    /// hints.
    pub fn disarm(&self) {
        if let IdleWatch::Armed(id) = self.watch.replace(IdleWatch::Off) {
            self.remove(id);
        }
        self.subscriptions.borrow_mut().clear();
        self.early.borrow_mut().clear();
        self.hints.borrow_mut().clear();
    }
}

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
    /// When it began, on [`crate::anim_clock::now`].
    pub start: Duration,
    pub duration_ms: f64,
    pub ease: Ease,
}

impl Tween {
    /// The value at `now`, and whether the ease is over. Elapsed time is
    /// the exact difference of two clock readings, in whole microseconds:
    /// subtracting two large millisecond floats could leave a 300 ms ease
    /// a hair short of its end forever on a frozen clock.
    pub fn at(&self, now: Duration) -> (f64, bool) {
        let elapsed_ms = now.saturating_sub(self.start).as_micros() as f64 / 1000.0;
        let t = if self.duration_ms <= 0.0 {
            1.0
        } else {
            (elapsed_ms / self.duration_ms).clamp(0.0, 1.0)
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
    /// units per ms, toward the prompt positive; `touchpad` picks the
    /// touchpad thresholds.
    pub fn end(&self, velocity: f64, distance: f64, cancelled: bool, touchpad: bool) -> (f64, f64) {
        release(
            self.progress,
            self.initial,
            self.cancel,
            velocity,
            distance,
            cancelled,
            touchpad,
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

/// `_getEndProgress` and `_endGesture` over snap points `[0, 1]`, with
/// the touch or (`touchpad`) the touchpad thresholds.
fn release(
    progress: f64,
    initial: f64,
    cancel: f64,
    velocity: f64,
    distance: f64,
    cancelled: bool,
    touchpad: bool,
) -> (f64, f64) {
    let (threshold, deceleration) = if touchpad {
        (VELOCITY_THRESHOLD_TOUCHPAD, DECELERATION_TOUCHPAD)
    } else {
        (VELOCITY_THRESHOLD_TOUCH, DECELERATION_TOUCH)
    };
    let end = if cancelled {
        cancel
    } else if velocity.abs() < threshold {
        closest(progress)
    } else {
        let slope = deceleration / (1.0 - deceleration) / 1000.0;
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
        /// What began the swipe under way.
        pub source: Cell<Source>,
        /// The touchpad gesture's deltas (`SwipeTracker._history`).
        pub history: RefCell<EventHistory>,
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
    /// Returns the page distance the source moves over (`_stack.height`
    /// for drags, [`TOUCHPAD_DISTANCE`] for the touchpad), or `None`
    /// while another swipe is under way (`_beginGesture`).
    pub fn swipe_begin(&self, source: Source) -> Option<f64> {
        let imp = self.imp();
        if imp.swipe.get().is_some() {
            return None;
        }
        imp.tween.set(None);
        imp.motion.set(crate::motion::current().allows_motion());
        imp.swipe.set(Some(Swipe::begin(imp.progress.get())));
        imp.source.set(source);
        // Both pages take part while the pointer leads.
        self.apply();
        let distance = if source.touchpad() {
            TOUCHPAD_DISTANCE
        } else {
            self.stack_height()
        };
        trace(format_args!(
            "swipe begin source={} progress={:.4} distance={distance:.0}",
            source.name(),
            imp.progress.get()
        ));
        Some(distance)
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
    pub fn swipe_end(&self, velocity: f64, distance: f64, cancelled: bool) -> Option<Page> {
        let imp = self.imp();
        let swipe = imp.swipe.take()?;
        let source = imp.source.get();
        imp.history.borrow_mut().reset();
        let (end, duration) = swipe.end(velocity, distance, cancelled, source.touchpad());
        let page = if end > 0.5 { Page::Prompt } else { Page::Clock };
        imp.active.set(page);
        if page == Page::Clock {
            self.leave_prompt();
        }
        trace(format_args!(
            "swipe end source={} progress={:.6} initial={:.6} velocity={:.6} distance={distance:.0} cancelled={cancelled} end={end:.0} duration={duration:.3}",
            source.name(),
            swipe.progress,
            swipe.initial,
            velocity
        ));
        self.ease_to(end, duration, Ease::OutCubic);
        Some(page)
    }

    /// One step of a touchpad swipe or scroll (`_beginTouchpadGesture`,
    /// `_updateTouchpadGesture`, `_endTouchpadGesture`). `begin` runs
    /// when this step began the swipe (`_ensureAuthPrompt`). Returns the
    /// page an end settles on.
    pub fn touchpad(&self, source: Source, step: Step, begin: impl FnOnce()) -> Option<Page> {
        let imp = self.imp();
        let ours = || imp.swipe.get().is_some() && imp.source.get() == source;
        match step {
            Step::Begin { time, delta } => {
                imp.history.borrow_mut().append(time, 0.0);
                if self.swipe_begin(source).is_some() {
                    begin();
                }
                if ours() {
                    imp.history.borrow_mut().append(time, delta);
                    self.swipe_update(delta, TOUCHPAD_DISTANCE);
                }
                None
            }
            Step::Update { time, delta } => {
                if ours() {
                    imp.history.borrow_mut().append(time, delta);
                    self.swipe_update(delta, TOUCHPAD_DISTANCE);
                }
                None
            }
            Step::End { time } => {
                if !ours() {
                    return None;
                }
                let velocity = {
                    let mut history = imp.history.borrow_mut();
                    history.trim(time);
                    history.velocity()
                };
                self.swipe_end(velocity, TOUCHPAD_DISTANCE, false)
            }
        }
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
        let start = if instant {
            Duration::ZERO
        } else {
            crate::anim_clock::now()
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
            start,
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
        let (value, done) = tween.at(crate::anim_clock::now());
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
        let ms = Duration::from_millis;
        let show = Tween {
            from: 0.0,
            to: 1.0,
            start: ms(1000),
            duration_ms: CROSSFADE_MS,
            ease: Ease::OutQuad,
        };
        assert_eq!(show.at(ms(1000)), (0.0, false));
        assert_eq!(show.at(ms(1150)), (0.75, false));
        assert_eq!(show.at(ms(1300)), (1.0, true));
        assert_eq!(show.at(ms(5000)), (1.0, true));
        // Escape half way back: from the drawn value, not from 1.
        let hide = Tween {
            from: 0.75,
            to: 0.0,
            ..show
        };
        assert!(close(hide.at(ms(1150)).0, 0.1875));
        let swipe = Tween {
            from: 0.4,
            to: 1.0,
            start: Duration::ZERO,
            duration_ms: 200.0,
            ease: Ease::OutCubic,
        };
        assert!(close(swipe.at(ms(100)).0, 0.4 + 0.6 * 0.875));
        let instant = Tween {
            duration_ms: 0.0,
            ..swipe
        };
        let (value, done) = instant.at(Duration::ZERO);
        assert!(close(value, 1.0) && done);
    }

    /// A proof moves the frozen clock by exactly 300 ms; after a long
    /// session the clock reads in the hundreds of thousands of seconds,
    /// where millisecond floats no longer subtract exactly. The ease must
    /// still end (and be at exactly half way at 150 ms).
    #[test]
    fn an_ease_ends_exactly_on_a_late_frozen_clock() {
        for start in [
            Duration::new(123_456, 789_123_457),
            Duration::new(987_654_321, 999_999_999),
            Duration::new(3_141, 592_653_589),
        ] {
            let show = Tween {
                from: 0.0,
                to: 1.0,
                start,
                duration_ms: CROSSFADE_MS,
                ease: Ease::OutQuad,
            };
            assert_eq!(show.at(start + Duration::from_millis(150)), (0.75, false));
            assert_eq!(show.at(start + Duration::from_millis(300)), (1.0, true));
        }
    }

    #[test]
    fn a_drag_moves_nothing_until_16_px_then_follows_each_event() {
        let mut pan = Pan::new(1000);
        assert_eq!(pan.motion(-10.0, 1010), None);
        assert_eq!(pan.motion(-15.9, 1020), None);
        // The event that crosses the threshold moves by its own delta.
        let delta = pan.motion(-26.0, 1030).unwrap();
        assert!(close(delta, -10.1));
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
        let (end, duration) = swipe.end(0.0, 300.0, false, false);
        assert_eq!(end, 0.0);
        // Back at ANIMATION_BASE_VELOCITY: 0.3 / 0.002 * 3 = 450, capped.
        assert!(close(duration, 400.0));
        swipe.update(120.0, 300.0);
        let (end, duration) = swipe.end(0.1, 300.0, false, false);
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
        assert_eq!(half.end(0.0, 300.0, false, false).0, 0.0);
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
        let (end, duration) = swipe.end(0.6, 300.0, false, false);
        assert_eq!(end, 1.0);
        // (1 - 0.1) / (0.6 / 300) * 3 = 1350 ms, capped at 400.
        assert!(close(duration, 400.0));
        // A fast flick finishes sooner: 0.9 / (6 / 300) * 3 = 135 ms.
        let (_, fast) = swipe.end(6.0, 300.0, false, false);
        assert!(close(fast, 135.0));
        // Toward the clock from the prompt.
        let mut down = Swipe::begin(1.0);
        down.update(-30.0, 300.0);
        assert_eq!(down.end(-0.6, 300.0, false, false).0, 0.0);
        // Below the touch threshold the nearer page wins.
        assert_eq!(swipe.end(0.29, 300.0, false, false).0, 0.0);
        // Velocity against the end slows to the base rate, min 100 ms.
        let near = Swipe {
            initial: 0.0,
            progress: 0.99,
            cancel: 0.0,
        };
        let (end, duration) = near.end(0.0, 300.0, false, false);
        assert_eq!(end, 1.0);
        assert!(close(duration, 100.0));
    }

    #[test]
    fn a_cancelled_swipe_returns_to_where_it_began() {
        let mut swipe = Swipe::begin(0.0);
        swipe.update(250.0, 300.0);
        assert_eq!(swipe.end(5.0, 300.0, true, false).0, 0.0);
        let mut back = Swipe::begin(1.0);
        back.update(-250.0, 300.0);
        assert_eq!(back.end(-5.0, 300.0, true, false).0, 1.0);
        assert_eq!(back.end(-5.0, 300.0, true, true).0, 1.0);
    }

    /// swipeTracker.js `_getEndProgress` with `isTouchpad`: the release
    /// must beat 0.6 (not 0.3) to project, and projects with 0.997.
    #[test]
    fn touchpad_releases_use_the_touchpad_thresholds() {
        let mut swipe = Swipe::begin(0.0);
        swipe.update(30.0, TOUCHPAD_DISTANCE);
        // 0.5 delta/ms flicks a touch drag up, not a touchpad.
        assert_eq!(swipe.end(0.5, TOUCHPAD_DISTANCE, false, false).0, 1.0);
        assert_eq!(swipe.end(0.5, TOUCHPAD_DISTANCE, false, true).0, 0.0);
        // Past 0.6 it projects: 0.997 / 0.003 / 1000 * 0.7 = 0.23 more,
        // short of half way, yet `_findPointForProjection` moves on.
        let (end, duration) = swipe.end(0.7, TOUCHPAD_DISTANCE, false, true);
        assert_eq!(end, 1.0);
        // 0.9 / (0.7 / 300) * 3 = 1157 ms, capped at 400.
        assert!(close(duration, 400.0));
        // A fast one finishes sooner: 0.9 / (6 / 300) * 3 = 135 ms.
        let (_, fast) = swipe.end(6.0, TOUCHPAD_DISTANCE, false, true);
        assert!(close(fast, 135.0));
    }

    /// `EventHistory`: deltas of the last 150 ms over the span they
    /// cover, the first left out; nothing recent keeps everything.
    #[test]
    fn touchpad_history_is_gnomes_event_history() {
        let mut history = EventHistory::default();
        assert_eq!(history.velocity(), 0.0);
        history.append(1000, 0.0);
        assert_eq!(history.velocity(), 0.0, "one sample has no span");
        for (i, delta) in [10.0, 10.0, 10.0].into_iter().enumerate() {
            history.append(1000 + 20 * (i as u32 + 1), delta);
        }
        // 30 over 60 ms; the first sample's delta never counts.
        assert!(close(history.velocity(), 30.0 / 60.0));
        // At 1180 everything before 1030 goes.
        history.trim(1180);
        assert!(close(history.velocity(), 10.0 / 20.0));
        // Held still past 150 ms: `splice(0, -1)` removes nothing.
        history.trim(5000);
        assert!(close(history.velocity(), 10.0 / 20.0));
        history.append(5010, 40.0);
        assert!(
            close(history.velocity(), 50.0 / (5010.0 - 1040.0)),
            "an append after a pause keeps the stale samples once"
        );
        history.append(5020, 30.0);
        assert!(close(history.velocity(), 30.0 / 10.0));
        history.reset();
        assert_eq!(history.velocity(), 0.0);
    }

    /// `TouchpadSwipeGesture`: three fingers, 16 px before anything
    /// moves, a vertical swipe handled from the event that crossed the
    /// threshold (by its own delta), a horizontal one ignored to its end,
    /// natural scrolling turning the deltas round.
    #[test]
    fn touchpad_swipe_follows_gnomes_three_finger_gesture() {
        let mut pad = TouchpadSwipe::default();
        let ev = |pad: &mut TouchpadSwipe, phase, fingers, d, time| {
            pad.event(phase, fingers, d, time, true)
        };
        // Two fingers are never a swipe.
        assert_eq!(
            ev(&mut pad, SwipePhase::Begin, 2, (0.0, 0.0), 1),
            (None, false)
        );
        assert_eq!(
            ev(&mut pad, SwipePhase::Update, 2, (0.0, -40.0), 2),
            (None, false)
        );
        // Three: the begin carries nothing, 10 px is under the threshold.
        assert_eq!(
            ev(&mut pad, SwipePhase::Begin, 3, (0.0, 0.0), 10),
            (None, false)
        );
        assert_eq!(
            ev(&mut pad, SwipePhase::Update, 3, (0.0, -10.0), 20),
            (None, false)
        );
        // 18 px up: begun, moving by this event's 8 px, natural: +8.
        assert_eq!(
            ev(&mut pad, SwipePhase::Update, 3, (2.0, -8.0), 30),
            (
                Some(Step::Begin {
                    time: 30,
                    delta: 8.0
                }),
                true
            )
        );
        assert_eq!(
            ev(&mut pad, SwipePhase::Update, 3, (0.0, -12.0), 40),
            (
                Some(Step::Update {
                    time: 40,
                    delta: 12.0
                }),
                true
            )
        );
        assert_eq!(
            ev(&mut pad, SwipePhase::End, 3, (0.0, 0.0), 50),
            (Some(Step::End { time: 50 }), false)
        );
        // Without natural scrolling the fingers' delta is the progress.
        pad.event(SwipePhase::Begin, 4, (0.0, 0.0), 60, false);
        assert_eq!(
            pad.event(SwipePhase::Update, 4, (0.0, 20.0), 70, false),
            (
                Some(Step::Begin {
                    time: 70,
                    delta: 20.0
                }),
                true
            )
        );
        pad.event(SwipePhase::End, 4, (0.0, 0.0), 80, false);
        // Sideways first: ignored, end included, until the next begin.
        assert_eq!(
            ev(&mut pad, SwipePhase::Begin, 3, (0.0, 0.0), 90),
            (None, false)
        );
        assert_eq!(
            ev(&mut pad, SwipePhase::Update, 3, (-20.0, -5.0), 100),
            (None, false)
        );
        assert_eq!(
            ev(&mut pad, SwipePhase::Update, 3, (0.0, -40.0), 110),
            (None, false)
        );
        assert_eq!(
            ev(&mut pad, SwipePhase::End, 3, (0.0, 0.0), 120),
            (None, false)
        );
        assert_eq!(
            ev(&mut pad, SwipePhase::Begin, 3, (0.0, 0.0), 130),
            (None, false)
        );
        assert_eq!(
            ev(&mut pad, SwipePhase::Update, 3, (0.0, -20.0), 140),
            (
                Some(Step::Begin {
                    time: 140,
                    delta: 20.0
                }),
                true
            )
        );
    }

    /// `ScrollGesture`: the first moving scroll begins, each one moves
    /// by its delta, and the stop ends it.
    #[test]
    fn touchpad_scroll_follows_and_the_stop_ends_it() {
        let mut scroll = ScrollGesture::default();
        assert_eq!(
            scroll.scroll(0.0, 6.0, 10),
            Step::Begin {
                time: 10,
                delta: 6.0
            }
        );
        assert_eq!(
            scroll.scroll(1.0, 4.0, 20),
            Step::Update {
                time: 20,
                delta: 4.0
            }
        );
        assert_eq!(scroll.scroll(0.0, 0.0, 30), Step::End { time: 30 });
        assert_eq!(
            scroll.scroll(0.0, -3.0, 40),
            Step::Begin {
                time: 40,
                delta: -3.0
            }
        );
        assert_eq!(scroll.stop(50), Step::End { time: 50 });
        assert_eq!(
            scroll.scroll(0.0, 2.0, 60),
            Step::Begin {
                time: 60,
                delta: 2.0
            }
        );
    }

    /// `Clock._updateHint`: touch mode says swipe.
    #[test]
    fn the_hint_follows_touch_mode() {
        assert_eq!(hint_text(false), "Click or press a key");
        assert_eq!(hint_text(true), "Swipe up");
    }

    /// `_hint.ease({opacity: 255, duration: CROSSFADE_TIME})`: Clutter's
    /// default ease-out cubic over 300 ms from where it stands; the
    /// power-save reset is immediate; Reduced Motion keeps the fade,
    /// animations off land at once and the slow-down factor stretches it.
    #[test]
    fn the_hint_fades_in_on_clutters_default_ease_and_hides_at_once() {
        let ms = Duration::from_millis;
        let mut fade = HintFade::default();
        assert_eq!(fade.opacity(), 0.0, "hidden until the idle watch fires");
        assert_eq!(fade.show(ms(1000), MotionPolicy::default()), 300.0);
        assert!(fade.tick(ms(1000)) && fade.opacity() == 0.0);
        assert!(fade.tick(ms(1150)) && close(fade.opacity(), 0.875));
        assert!(!fade.tick(ms(1300)) && fade.opacity() == 1.0 && !fade.easing());
        // A power-save change mid-fade: 0 at once, the ease gone.
        fade.hide();
        fade.show(ms(2000), MotionPolicy::default());
        fade.tick(ms(2150));
        fade.hide();
        assert!(fade.opacity() == 0.0 && !fade.easing());
        assert!(!fade.tick(ms(2300)) && fade.opacity() == 0.0);
        // Shown again part way: from the drawn value, the full 300 ms.
        fade.show(ms(3000), MotionPolicy::default());
        fade.tick(ms(3150));
        fade.show(ms(3150), MotionPolicy::default());
        fade.tick(ms(3300));
        assert!(close(fade.opacity(), 0.875 + 0.125 * 0.875));
        // Reduced Motion keeps the fade.
        let mut reduced = HintFade::default();
        assert_eq!(
            reduced.show(ms(0), MotionPolicy::from_gnome(true, true, 1.0)),
            300.0
        );
        reduced.tick(ms(150));
        assert!(close(reduced.opacity(), 0.875));
        // Animations off: opaque at once.
        let mut off = HintFade::default();
        assert_eq!(
            off.show(ms(0), MotionPolicy::from_gnome(false, false, 1.0)),
            0.0
        );
        assert!(off.opacity() == 1.0 && !off.easing());
        // The slow-down factor stretches it.
        let mut slow = HintFade::default();
        assert_eq!(
            slow.show(ms(0), MotionPolicy::from_gnome(true, false, 2.0)),
            600.0
        );
        slow.tick(ms(300));
        assert!(close(slow.opacity(), 0.875));
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
        let distance = curtain.swipe_begin(Source::Drag).unwrap();
        assert!(
            curtain.swipe_begin(Source::Scroll).is_none(),
            "one swipe at a time"
        );
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
        assert_eq!(curtain.swipe_end(0.0, distance, false), Some(Page::Prompt));
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
        let distance = curtain.swipe_begin(Source::Drag).unwrap();
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
        assert_eq!(curtain.swipe_end(0.0, distance, true), Some(Page::Clock));
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
