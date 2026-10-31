//! GNOME 51's app grid animations (appDisplay.js, iconGrid.js,
//! pageIndicators.js, swipeTracker.js): the folder dialog's zoom from its
//! icon, the icons' scale-and-fade while dragged, the launch zoom, the
//! page switch and the swipe that ends in one, the page dots following
//! the pages, the page arrows' fade and the staggered reflow while
//! dragging.
//!
//! GNOME 51 defines AppIcon.scaleIn() (appDisplay.js) but nothing calls
//! it: an app new to the grid appears in place, so it does here too.
//!
//! Each animation is a tick callback that runs only while it moves and
//! touches nothing but a transform and an opacity: no relayout per frame.
//! Fades and motion are separate parts, so Reduced Motion keeps the fades
//! and drops the zooms, slides and scales; with animations off all of it
//! is instant.
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk4 as gtk;
use gtk4::glib;
use std::cell::{Cell, RefCell};
use std::io::{Read, Seek};
use std::rc::Rc;

/// The shell's motion policy (#493): `allows_motion` for zooms, slides
/// and scales, `allows_fades` for opacity, durations through `adjust_ms`.
pub type Motion = tuna_shell_control::MotionPolicy;

/// What an animated property is: an opacity, or a movement/scale.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    Fade,
    Motion,
}

fn allows(motion: Motion, part: Part) -> bool {
    match part {
        Part::Fade => motion.allows_fades(),
        Part::Motion => motion.allows_motion(),
    }
}

/// The live policy.
pub fn motion() -> Motion {
    crate::motion::current()
}

/// Clutter's easing modes the grid uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Curve {
    InQuad,
    OutQuad,
    OutCubic,
    OutExpo,
}

impl Curve {
    pub fn at(self, p: f64) -> f64 {
        let p = p.clamp(0.0, 1.0);
        match self {
            Curve::InQuad => p * p,
            Curve::OutQuad => 1.0 - (1.0 - p).powi(2),
            Curve::OutCubic => 1.0 - (1.0 - p).powi(3),
            Curve::OutExpo => {
                if p >= 1.0 {
                    1.0
                } else {
                    1.0 - 2f64.powf(-10.0 * p)
                }
            }
        }
    }
}

/// One eased property: when it starts, how long it runs, its curve.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Timing {
    pub delay_ms: f64,
    pub duration_ms: f64,
    pub curve: Curve,
    pub part: Part,
}

impl Timing {
    const fn new(duration_ms: f64, curve: Curve, part: Part) -> Self {
        Self {
            delay_ms: 0.0,
            duration_ms,
            curve,
            part,
        }
    }

    pub const fn delayed(self, delay_ms: f64) -> Self {
        Self { delay_ms, ..self }
    }

    pub const fn lasting(self, duration_ms: f64) -> Self {
        Self {
            duration_ms,
            ..self
        }
    }

    /// Eased progress `elapsed_ms` in; 1 when its part is not animated.
    /// Durations scale by GNOME's slow-down factor (`adjust_ms`).
    pub fn at(&self, elapsed_ms: f64, motion: Motion) -> f64 {
        let duration = motion.adjust_ms(self.duration_ms);
        if !allows(motion, self.part) || duration <= 0.0 {
            return 1.0;
        }
        self.curve
            .at((elapsed_ms - motion.adjust_ms(self.delay_ms)) / duration)
    }

    /// When it settles, 0 when it does not animate.
    pub fn end_ms(&self, motion: Motion) -> f64 {
        if allows(motion, self.part) {
            motion.adjust_ms(self.delay_ms + self.duration_ms)
        } else {
            0.0
        }
    }
}

/// `FOLDER_DIALOG_ANIMATION_TIME`: the dialog's zoom (ease-out-expo)
/// from its icon and back.
pub const FOLDER_ZOOM: Timing = Timing::new(200.0, Curve::OutExpo, Part::Motion);
/// The dialog's own fade, with the zoom: ease-out-expo opening,
/// ease-out-quad closing.
pub const FOLDER_FADE_IN: Timing = Timing::new(200.0, Curve::OutExpo, Part::Fade);
pub const FOLDER_FADE_OUT: Timing = Timing::new(200.0, Curve::OutQuad, Part::Fade);
/// The shade behind the dialog darkening and clearing.
pub const FOLDER_SHADE: Timing = Timing::new(200.0, Curve::OutQuad, Part::Fade);
/// The folder icon in the grid hides over the first half of the
/// opening and returns over the last half of the closing.
pub const FOLDER_ICON_HIDE: Timing = Timing::new(100.0, Curve::OutQuad, Part::Fade);
pub const FOLDER_ICON_SHOW: Timing = Timing::new(100.0, Curve::InQuad, Part::Fade).delayed(100.0);
/// scaleAndFade/undoScaleAndFade: Clutter's implicit 250 ms
/// ease-out-cubic, to half size and transparent.
pub const ICON_SCALE: Timing = Timing::new(250.0, Curve::OutCubic, Part::Motion);
pub const ICON_FADE: Timing = Timing::new(250.0, Curve::OutCubic, Part::Fade);
/// `APPICON_ANIMATION_OUT_TIME`, to `APPICON_ANIMATION_OUT_SCALE`.
pub const LAUNCH_ZOOM: Timing = Timing::new(250.0, Curve::OutQuad, Part::Motion);
pub const LAUNCH_FADE: Timing = Timing::new(250.0, Curve::OutQuad, Part::Fade);
pub const LAUNCH_SCALE: f64 = 3.0;
/// `VIEWS_SWITCH_TIME` after `VIEWS_SWITCH_ANIMATION_DELAY`: the grid
/// fading in on the switch from the window picker, and out on the way back.
pub const GRID_FADE_IN: Timing = Timing::new(400.0, Curve::OutQuad, Part::Fade).delayed(100.0);
pub const GRID_FADE_OUT: Timing = Timing::new(400.0, Curve::OutQuad, Part::Fade);
/// `PAGE_SWITCH_TIME`: goToPage eases out-cubic over 300 ms from wherever
/// the pages are drawn, even part of the way there.
pub const PAGE_SWITCH: Timing = Timing::new(300.0, Curve::OutCubic, Part::Motion);
/// `PAGE_INDICATOR_FADE_TIME`, Clutter's implicit ease-out-cubic.
pub const PAGE_ARROW_FADE: Timing = Timing::new(200.0, Curve::OutCubic, Part::Fade);
/// An icon's reflow: Clutter's implicit 250 ms, ease-out-quad, each
/// moved icon `ICON_POSITION_DELAY` after the one before.
pub const REFLOW: Timing = Timing::new(250.0, Curve::OutQuad, Part::Motion);
pub const REFLOW_STAGGER_MS: f64 = 10.0;

pub fn lerp(from: f64, to: f64, t: f64) -> f64 {
    from + (to - from) * t
}

/// The launch clone's box at `t`: (center x, center y, scale) of a clone
/// centred at `center`, `size` (width, height), grown `LAUNCH_SCALE`
/// times while staying inside `bounds` (x, y, width, height), as
/// zoomOutActorAtPos keeps it on the monitor.
pub fn launch_box(
    center: (f64, f64),
    size: (f64, f64),
    bounds: (f64, f64, f64, f64),
    t: f64,
) -> (f64, f64, f64) {
    let target = launch_center(center, size, bounds);
    (
        lerp(center.0, target.0, t),
        lerp(center.1, target.1, t),
        lerp(1.0, LAUNCH_SCALE, t),
    )
}

/// Where the fully grown launch clone is centred: zoomOutActorAtPos's
/// `Math.clamp(scaledX, monitor.x, monitor.x + monitor.width -
/// scaledWidth)`, and the same down.
pub fn launch_center(
    center: (f64, f64),
    size: (f64, f64),
    bounds: (f64, f64, f64, f64),
) -> (f64, f64) {
    let contain = |c: f64, extent: f64, lo: f64, span: f64| {
        let grown = extent * LAUNCH_SCALE;
        let start = c - grown / 2.0;
        start.clamp(lo, (lo + span - grown).max(lo)) + grown / 2.0
    };
    (
        contain(center.0, size.0, bounds.0, bounds.2),
        contain(center.1, size.1, bounds.1, bounds.3),
    )
}

/// The folder dialog's zoom at `t` (0 at the icon, 1 in place): its
/// top-left offset from its resting place and its scale, as GNOME's
/// translation and scale with the pivot at the top-left corner.
pub fn folder_zoom(
    source: (f64, f64, f64, f64),
    dialog: (f64, f64, f64, f64),
    t: f64,
) -> ((f64, f64), (f64, f64)) {
    let (sx, sy, sw, sh) = source;
    let (dx, dy, dw, dh) = dialog;
    (
        (lerp(sx - dx, 0.0, t), lerp(sy - dy, 0.0, t)),
        (lerp(sw / dw, 1.0, t), lerp(sh / dh, 1.0, t)),
    )
}

/// pageIndicators.js `INDICATOR_INACTIVE_SCALE`, `_SCALE_PRESSED`,
/// `INDICATOR_INACTIVE_OPACITY` and `_OPACITY_HOVER` (of 255).
pub const DOT_INACTIVE_SCALE: f64 = 2.0 / 3.0;
pub const DOT_INACTIVE_SCALE_PRESSED: f64 = 0.5;
pub const DOT_INACTIVE_OPACITY: f64 = 128.0 / 255.0;
pub const DOT_INACTIVE_OPACITY_HOVER: f64 = 1.0;

/// pageIndicators.js _updateIndicator: page dot `index`'s (scale,
/// opacity) with the pages drawn at `position`. A dot is whole where its
/// page is drawn and inactive a page away, in between in between, so
/// the dots follow the page switch's own curve and a swipe's fingers.
pub fn page_dot_look(position: f64, index: usize, hover: bool, pressed: bool) -> (f64, f64) {
    let progress = (1.0 - (position - index as f64).abs()).max(0.0);
    let inactive_scale = if pressed {
        DOT_INACTIVE_SCALE_PRESSED
    } else {
        DOT_INACTIVE_SCALE
    };
    let inactive_opacity = if hover {
        DOT_INACTIVE_OPACITY_HOVER
    } else {
        DOT_INACTIVE_OPACITY
    };
    (
        lerp(inactive_scale, 1.0, progress),
        lerp(inactive_opacity, 1.0, progress),
    )
}

/// swipeTracker.js `EVENT_HISTORY_THRESHOLD_MS`.
const SWIPE_HISTORY_MS: u32 = 150;
/// `MIN_ANIMATION_DURATION`, `MAX_ANIMATION_DURATION`.
const SWIPE_MIN_MS: f64 = 100.0;
const SWIPE_MAX_MS: f64 = 400.0;
/// `VELOCITY_THRESHOLD_TOUCH`, `VELOCITY_THRESHOLD_TOUCHPAD` (px/ms).
const SWIPE_VELOCITY_THRESHOLD_TOUCH: f64 = 0.3;
const SWIPE_VELOCITY_THRESHOLD_TOUCHPAD: f64 = 0.6;
/// `DECELERATION_TOUCH`, `DECELERATION_TOUCHPAD`.
const SWIPE_DECELERATION_TOUCH: f64 = 0.998;
const SWIPE_DECELERATION_TOUCHPAD: f64 = 0.997;
/// `VELOCITY_CURVE_THRESHOLD`, `DECELERATION_PARABOLA_MULTIPLIER`.
const SWIPE_VELOCITY_CURVE_THRESHOLD: f64 = 2.0;
const SWIPE_PARABOLA_MULTIPLIER: f64 = 0.35;
/// `DURATION_MULTIPLIER`, `ANIMATION_BASE_VELOCITY`, `EPSILON`.
const SWIPE_DURATION_MULTIPLIER: f64 = 3.0;
const SWIPE_BASE_VELOCITY: f64 = 0.002;
const SWIPE_EPSILON: f64 = 0.005;

/// swipeTracker.js EventHistory: the swipe's moves of the last 150 ms,
/// (event time in ms, pixels toward later pages), for its velocity.
#[derive(Debug, Default)]
pub struct SwipeHistory {
    data: Vec<(u32, f64)>,
}

impl SwipeHistory {
    pub fn reset(&mut self) {
        self.data.clear();
    }

    /// Forget moves older than 150 ms before `time`.
    pub fn trim(&mut self, time: u32) {
        let threshold = time.saturating_sub(SWIPE_HISTORY_MS);
        if let Some(index) = self.data.iter().position(|&(t, _)| t >= threshold) {
            self.data.drain(..index);
        }
    }

    pub fn append(&mut self, time: u32, delta: f64) {
        self.trim(time);
        self.data.push((time, delta));
    }

    /// Pixels per millisecond over the kept moves.
    pub fn velocity(&self) -> f64 {
        let (Some(&(first, _)), Some(&(last, _))) = (self.data.first(), self.data.last()) else {
            return 0.0;
        };
        if self.data.len() < 2 || first == last {
            return 0.0;
        }
        let total: f64 = self.data[1..].iter().map(|&(_, delta)| delta).sum();
        total / f64::from(last.wrapping_sub(first))
    }
}

/// _findClosestPoint over the pages 0..=`last`: the nearer page, the
/// earlier one on a tie (indexOf of the minimum).
fn closest_page(position: f64, last: f64) -> f64 {
    let below = position.floor().clamp(0.0, last);
    let above = (below + 1.0).min(last);
    if (position - below).abs() <= (above - position).abs() {
        below
    } else {
        above
    }
}

/// _getBounds without long swipes: how far a swipe that began at
/// `initial` may go, a page past the pages around it.
pub fn swipe_bounds(initial: f64, last: f64) -> (f64, f64) {
    let closest = closest_page(initial, last);
    let (previous, next) = if (closest - initial).abs() < SWIPE_EPSILON {
        (closest, closest)
    } else {
        (initial.floor().max(0.0), initial.ceil().min(last))
    };
    ((previous - 1.0).max(0.0), (next + 1.0).min(last))
}

/// GNOME's cancel progress for a swipe that began at `initial`
/// (AppDisplay._swipeBegin's `Math.round`).
pub fn swipe_cancel_page(initial: f64) -> f64 {
    (initial + 0.5).floor()
}

/// _getEndProgress: the page a swipe at `progress` (begun at `initial`)
/// settles on when released at `velocity` px/ms: the nearest page when
/// slow, else where its deceleration would carry it, never back past
/// the page it began on.
fn swipe_end_page(progress: f64, initial: f64, velocity: f64, touchpad: bool, last: f64) -> f64 {
    let threshold = if touchpad {
        SWIPE_VELOCITY_THRESHOLD_TOUCHPAD
    } else {
        SWIPE_VELOCITY_THRESHOLD_TOUCH
    };
    if velocity.abs() < threshold {
        return closest_page(progress, last);
    }
    let deceleration = if touchpad {
        SWIPE_DECELERATION_TOUCHPAD
    } else {
        SWIPE_DECELERATION_TOUCH
    };
    let slope = deceleration / (1.0 - deceleration) / 1000.0;
    let speed = velocity.abs();
    let travel = if speed > SWIPE_VELOCITY_CURVE_THRESHOLD {
        let c = slope / 2.0 / SWIPE_PARABOLA_MULTIPLIER;
        let x = speed - SWIPE_VELOCITY_CURVE_THRESHOLD + c;
        slope * SWIPE_VELOCITY_CURVE_THRESHOLD + SWIPE_PARABOLA_MULTIPLIER * x * x
            - SWIPE_PARABOLA_MULTIPLIER * c * c
    } else {
        speed * slope
    };
    let (lower, upper) = swipe_bounds(initial, last);
    let projected = (travel * velocity.signum() + progress).clamp(lower, upper);
    // _findPointForProjection.
    let start = closest_page(initial, last);
    let (previous, next) = (projected.floor(), projected.ceil());
    if velocity > 0.0 && previous == start {
        next
    } else if velocity < 0.0 && next == start {
        previous
    } else {
        closest_page(projected, last)
    }
}

/// _endGesture: the page a released swipe settles on and how long its
/// out-cubic ease takes (0: at once), from the swipe's `progress` and
/// `initial` position in pages, its `velocity` in px/ms, and the page
/// width `distance` in px.
pub fn swipe_end(
    progress: f64,
    initial: f64,
    velocity: f64,
    distance: f64,
    touchpad: bool,
    last: f64,
) -> (f64, f64) {
    let end = swipe_end_page(progress, initial, velocity, touchpad, last);
    let mut velocity = velocity / distance.max(1.0);
    if (end - progress) * velocity <= 0.0 {
        velocity = SWIPE_BASE_VELOCITY;
    }
    let pages = (progress - end).abs().ceil().max(1.0);
    let max_ms = SWIPE_MAX_MS * (1.0 + pages).log2();
    let duration = ((progress - end) / velocity * SWIPE_DURATION_MULTIPLIER).abs();
    let duration = if duration > 0.0 {
        duration.clamp(SWIPE_MIN_MS, max_ms)
    } else {
        0.0
    };
    (end, duration)
}

/// The slot a tile moves by in a reflow, in cells: (columns, rows).
pub fn reflow_shift(from: usize, to: usize, columns: usize) -> (f64, f64) {
    let columns = columns.max(1);
    (
        (from % columns) as f64 - (to % columns) as f64,
        (from / columns) as f64 - (to / columns) as f64,
    )
}

/// Whether the grid's animations are traced (`TUNA_GRID_TRACE`, read
/// once).
static TRACING: std::sync::LazyLock<bool> =
    std::sync::LazyLock::new(|| std::env::var_os("TUNA_GRID_TRACE").is_some());

pub fn tracing() -> bool {
    *TRACING
}

pub fn trace(name: &str, what: std::fmt::Arguments) {
    if tracing() {
        // Formatting directly into stderr can split a record around another
        // process's writes to the same proof pipe. Buffer each short record,
        // as media diagnostics do, so its text and newline travel together.
        let line = format!("tuna-shell-gtk: grid-anim {name} {what}\n");
        let _ = std::io::Write::write_all(&mut std::io::stderr().lock(), line.as_bytes());
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SchedTimes {
    run_ns: u64,
    wait_ns: u64,
}

fn parse_schedstat(bytes: &[u8]) -> Option<SchedTimes> {
    let mut fields = std::str::from_utf8(bytes).ok()?.split_ascii_whitespace();
    let run_ns = fields.next()?.parse().ok()?;
    let wait_ns = fields.next()?.parse().ok()?;
    fields.next()?.parse::<u64>().ok()?;
    if fields.next().is_some() {
        return None;
    }
    Some(SchedTimes { run_ns, wait_ns })
}

/// Open only on a traced page tick, once per GTK thread. Reads reuse the
/// proc handle and a stack buffer; no per-frame path or String allocation.
struct ThreadSched {
    file: Option<std::fs::File>,
    wait_enabled: &'static str,
}

impl ThreadSched {
    fn open() -> Self {
        let mut enabled = [0_u8; 4];
        let wait_enabled = match std::fs::File::open("/proc/sys/kernel/sched_schedstats")
            .and_then(|mut file| file.read(&mut enabled))
        {
            Ok(2) if &enabled[..2] == b"1\n" => "enabled",
            Ok(2) if &enabled[..2] == b"0\n" => "disabled",
            _ => "unknown",
        };
        Self {
            file: std::fs::File::open("/proc/thread-self/schedstat").ok(),
            wait_enabled,
        }
    }

    fn read(&mut self) -> Result<SchedTimes, &'static str> {
        let file = self.file.as_mut().ok_or("open")?;
        file.rewind().map_err(|_| "seek")?;
        let mut bytes = [0_u8; 128];
        let count = file.read(&mut bytes).map_err(|_| "read")?;
        if count == bytes.len() {
            return Err("truncated");
        }
        parse_schedstat(&bytes[..count]).ok_or("parse")
    }
}

thread_local! {
    static THREAD_SCHED: RefCell<ThreadSched> = RefCell::new(ThreadSched::open());
}

#[derive(Clone, Copy)]
struct TickSample {
    monotonic_us: i64,
    sched: SchedTimes,
}

impl TickSample {
    fn since(self, previous: Self) -> Option<(i64, u64, u64)> {
        let wall_us = self.monotonic_us.checked_sub(previous.monotonic_us)?;
        if wall_us < 0 {
            return None;
        }
        Some((
            wall_us,
            self.sched.run_ns.checked_sub(previous.sched.run_ns)?,
            self.sched.wait_ns.checked_sub(previous.sched.wait_ns)?,
        ))
    }
}

fn trace_page_tick(clock_us: i64, previous: &Cell<Option<TickSample>>) {
    let monotonic_us = glib::monotonic_time();
    THREAD_SCHED.with(|reader| {
        let mut reader = reader.borrow_mut();
        match reader.read() {
            Ok(sched) => {
                let current = TickSample {
                    monotonic_us,
                    sched,
                };
                let delta = previous.get().and_then(|before| current.since(before));
                previous.set(Some(current));
                let observation_us = glib::monotonic_time() - monotonic_us;
                if let Some((wall_us, run_ns, wait_ns)) = delta {
                    trace(
                        "page-switch",
                        format_args!(
                            "tick clock_us={clock_us} monotonic_us={monotonic_us} run_ns={} wait_ns={} schedstats={} wall_delta_us={wall_us} run_delta_ns={run_ns} wait_delta_ns={wait_ns} observation_us={observation_us}",
                            sched.run_ns, sched.wait_ns, reader.wait_enabled
                        ),
                    );
                } else {
                    trace(
                        "page-switch",
                        format_args!(
                            "tick clock_us={clock_us} monotonic_us={monotonic_us} run_ns={} wait_ns={} schedstats={} delta=baseline-or-reset observation_us={observation_us}",
                            sched.run_ns, sched.wait_ns, reader.wait_enabled
                        ),
                    );
                }
            }
            Err(reason) => {
                previous.set(None);
                trace(
                    "page-switch",
                    format_args!(
                        "tick clock_us={clock_us} monotonic_us={monotonic_us} schedstat=unavailable reason={reason} observation_us={}",
                        glib::monotonic_time() - monotonic_us
                    ),
                );
            }
        }
    });
}

/// One running animation per property set: a new start replaces it.
#[derive(Default)]
pub struct Slot {
    tick: RefCell<Option<gtk::TickCallbackId>>,
}

impl Slot {
    pub fn stop(&self) {
        if let Some(tick) = self.tick.borrow_mut().take() {
            tick.remove();
        }
    }

    pub fn running(&self) -> bool {
        self.tick.borrow().is_some()
    }

    /// Run `step` with the elapsed milliseconds on `widget`'s frames for
    /// `total_ms` (from `parts` under `motion`), then `done`. With
    /// nothing to animate, one final step and `done` happen at once.
    pub fn start(
        self: &Rc<Self>,
        widget: &impl IsA<gtk::Widget>,
        name: &'static str,
        total_ms: f64,
        step: impl Fn(f64) + 'static,
        done: impl FnOnce() + 'static,
    ) {
        self.stop();
        if total_ms <= 0.0 {
            step(f64::INFINITY);
            trace(
                name,
                format_args!("instant motion={}", motion().level.as_str()),
            );
            done();
            return;
        }
        step(0.0);
        trace(
            name,
            format_args!("start ms={total_ms:.0} motion={}", motion().level.as_str()),
        );
        let started = Cell::new(None::<i64>);
        let observe_ticks = name == "page-switch" && tracing();
        let previous_tick = Cell::new(None);
        let done = RefCell::new(Some(done));
        let weak = Rc::downgrade(self);
        let id = widget.add_tick_callback(move |_, clock| {
            let now = clock.frame_time();
            let start = *started.get().get_or_insert(now);
            started.set(Some(start));
            let elapsed = (now - start) as f64 / 1000.0;
            if observe_ticks {
                trace_page_tick(now, &previous_tick);
            }
            step(elapsed);
            if elapsed < total_ms {
                return glib::ControlFlow::Continue;
            }
            trace(name, format_args!("settle ms={elapsed:.0}"));
            if let Some(slot) = weak.upgrade() {
                // Returning Break removes the callback; forget its id.
                if let Some(id) = slot.tick.borrow_mut().take() {
                    std::mem::forget(id);
                }
            }
            if let Some(done) = done.borrow_mut().take() {
                done();
            }
            glib::ControlFlow::Break
        });
        *self.tick.borrow_mut() = Some(id);
    }
}

/// Fade `widget` in or out over `PAGE_ARROW_FADE`, hiding it once out.
/// A `targetable` one takes clicks only while shown, not while fading
/// out; others never do. Unmapped, it jumps.
pub fn fade_visible(
    widget: &gtk::Widget,
    slot: &Rc<Slot>,
    name: &'static str,
    visible: bool,
    targetable: bool,
) {
    let shown = widget.parent().is_some_and(|parent| parent.is_mapped());
    let animate = shown && widget.is_visible() != visible;
    let from = if widget.is_visible() {
        widget.opacity()
    } else {
        0.0
    };
    let to = if visible { 1.0 } else { 0.0 };
    if visible {
        widget.set_visible(true);
    }
    widget.set_can_target(visible && targetable);
    let total = if animate || slot.running() {
        PAGE_ARROW_FADE.end_ms(motion())
    } else {
        0.0
    };
    let policy = motion();
    let w = widget.downgrade();
    let step = move |elapsed: f64| {
        if let Some(w) = w.upgrade() {
            w.set_opacity(lerp(from, to, PAGE_ARROW_FADE.at(elapsed, policy)));
        }
    };
    let w = widget.downgrade();
    slot.start(widget, name, total, step, move || {
        if let Some(w) = w.upgrade() {
            if !visible {
                w.set_visible(false);
            }
        }
    });
}

/// Where a layer window's (0, 0) is on the output.
fn window_origin(window: &gtk::Window) -> (f64, f64) {
    use gtk4_layer_shell::{Edge, LayerShell};
    if !window.is_layer_window() {
        return (0.0, 0.0);
    }
    let along = |edge: Edge| {
        if window.is_anchor(edge) {
            f64::from(window.margin(edge))
        } else {
            0.0
        }
    };
    (along(Edge::Left), along(Edge::Top))
}

/// `widget`'s box on the output: (x, y, width, height).
pub fn screen_bounds(widget: &impl IsA<gtk::Widget>) -> Option<(f64, f64, f64, f64)> {
    let window = widget.root()?.downcast::<gtk::Window>().ok()?;
    let bounds = widget.compute_bounds(&window)?;
    let (ox, oy) = window_origin(&window);
    Some((
        ox + f64::from(bounds.x()),
        oy + f64::from(bounds.y()),
        f64::from(bounds.width()),
        f64::from(bounds.height()),
    ))
}

/// The first output's box.
pub fn monitor_bounds() -> (f64, f64, f64, f64) {
    gtk::gdk::Display::default()
        .and_then(|d| d.monitors().item(0))
        .and_downcast::<gtk::gdk::Monitor>()
        .map(|m| {
            let g = m.geometry();
            (
                f64::from(g.x()),
                f64::from(g.y()),
                f64::from(g.width()),
                f64::from(g.height()),
            )
        })
        .unwrap_or((0.0, 0.0, 1280.0, 800.0))
}

/// The first `T` in `widget`'s tree, `widget` included, depth first.
fn first_of<T: IsA<gtk::Widget>>(widget: &gtk::Widget) -> Option<T> {
    let mut pending = vec![widget.clone()];
    while let Some(widget) = pending.pop() {
        if let Ok(found) = widget.clone().downcast::<T>() {
            return Some(found);
        }
        let mut child = widget.last_child();
        while let Some(next) = child {
            child = next.prev_sibling();
            pending.push(next);
        }
    }
    None
}

/// What a launch clones (iconGrid.js animateZoomOut zooms BaseIcon's
/// child): a grid or folder tile's icon with its label, the box under
/// the tile's [`Zoom`]; a tile without one (the dash's, whose icons have
/// no label) its icon.
fn launch_source(tile: &gtk::Widget) -> Option<gtk::Widget> {
    if let Some(zoom) = tile.first_child().and_downcast::<Zoom>() {
        return zoom.first_child();
    }
    first_of::<gtk::Image>(tile).map(|image| image.upcast())
}

/// GNOME's launch animation (iconGrid.js zoomOutActor): a clone of the
/// launched tile's icon (and label, in the grid) grows to three times its
/// size about its centre and fades out over 250 ms, kept on the monitor,
/// while the overview closes. As Clutter.Clone paints its source, the
/// clone is the source's last drawn frame, scaled. It is drawn on its
/// own click-through overlay surface, only as big as the zoom covers,
/// and destroyed when done.
pub fn launch_zoom(tile: &impl IsA<gtk::Widget>) {
    use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
    let policy = motion();
    let total = LAUNCH_ZOOM.end_ms(policy).max(LAUNCH_FADE.end_ms(policy));
    let Some(source) = launch_source(tile.upcast_ref()) else {
        return;
    };
    let (Some((x, y, w, h)), Some(app)) = (
        screen_bounds(&source),
        source
            .root()
            .and_downcast::<gtk::Window>()
            .and_then(|window| window.application()),
    ) else {
        return;
    };
    if total <= 0.0 {
        trace(
            "launch-zoom",
            format_args!("instant motion={}", policy.level.as_str()),
        );
        return;
    }
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    // The source's render node as it was last drawn: it stays whole
    // after the overview hides the source.
    let frame = gtk::WidgetPaintable::new(Some(&source)).current_image();
    if tracing() {
        trace(
            "launch-zoom",
            format_args!(
                "clone size={w:.0}x{h:.0} drawn={}x{} label={}",
                frame.intrinsic_width(),
                frame.intrinsic_height(),
                u8::from(first_of::<gtk::Label>(&source).is_some())
            ),
        );
    }
    let size = (w, h);
    let center = (x + w / 2.0, y + h / 2.0);
    let monitor = monitor_bounds();
    let (grown_w, grown_h) = (w * LAUNCH_SCALE, h * LAUNCH_SCALE);
    let end = launch_center(center, size, monitor);
    // The surface covers where the clone starts and where it ends.
    let left = (center.0 - w / 2.0).min(end.0 - grown_w / 2.0).floor();
    let top = (center.1 - h / 2.0).min(end.1 - grown_h / 2.0).floor();
    let right = (center.0 + w / 2.0).max(end.0 + grown_w / 2.0).ceil();
    let bottom = (center.1 + h / 2.0).max(end.1 + grown_h / 2.0).ceil();
    let picture = gtk::Picture::for_paintable(&frame);
    picture.set_can_shrink(true);
    picture.set_content_fit(gtk::ContentFit::Fill);
    picture.set_size_request(w.round() as i32, h.round() as i32);
    picture.set_halign(gtk::Align::Start);
    picture.set_valign(gtk::Align::Start);
    let zoom = Zoom::new(&picture);
    zoom.set_pivot(0.0, 0.0);
    let window = gtk::Window::new();
    window.set_application(Some(&app));
    window.add_css_class("tuna-launch-zoom");
    window.set_title(Some("Launch"));
    window.init_layer_shell();
    window.set_layer(Layer::Overlay);
    window.set_namespace(Some("tuna-shell-launch-zoom"));
    window.set_anchor(Edge::Top, true);
    window.set_anchor(Edge::Left, true);
    window.set_margin(Edge::Left, (left - monitor.0) as i32);
    window.set_margin(Edge::Top, (top - monitor.1) as i32);
    window.set_exclusive_zone(-1);
    window.set_keyboard_mode(KeyboardMode::None);
    window.set_can_target(false);
    window.set_default_size((right - left) as i32, (bottom - top) as i32);
    window.set_size_request((right - left) as i32, (bottom - top) as i32);
    window.set_child(Some(&zoom));
    window.connect_realize(|window| {
        if let Some(surface) = window.surface() {
            surface.set_input_region(Some(&gtk::cairo::Region::create()));
        }
    });
    let place = {
        let zoom = zoom.downgrade();
        move |elapsed: f64| {
            let Some(zoom) = zoom.upgrade() else { return };
            let (cx, cy, scale) =
                launch_box(center, size, monitor, LAUNCH_ZOOM.at(elapsed, policy));
            // The clone's top-left, `scale` times its size about the
            // clone's centre.
            zoom.set(
                (cx - left - w * scale / 2.0, cy - top - h * scale / 2.0),
                (scale, scale),
                1.0 - LAUNCH_FADE.at(elapsed, policy),
            );
        }
    };
    let slot = Rc::new(Slot::default());
    let keep = slot.clone();
    let closing = window.downgrade();
    slot.start(&zoom, "launch-zoom", total, place, move || {
        drop(keep);
        if let Some(window) = closing.upgrade() {
            window.destroy();
        }
    });
    window.present();
}

mod zoom_imp {
    use super::*;

    pub struct Zoom {
        pub shift: Cell<(f64, f64)>,
        pub scale: Cell<(f64, f64)>,
        /// Fraction of the size the scale grows from: (0, 0) top-left.
        pub pivot: Cell<(f64, f64)>,
        pub alpha: Cell<f64>,
    }

    impl Default for Zoom {
        fn default() -> Self {
            Self {
                shift: Cell::new((0.0, 0.0)),
                scale: Cell::new((1.0, 1.0)),
                pivot: Cell::new((0.5, 0.5)),
                alpha: Cell::new(1.0),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Zoom {
        const NAME: &'static str = "TunaGridZoom";
        type Type = super::Zoom;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }
    }

    impl ObjectImpl for Zoom {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for Zoom {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let (dx, dy) = self.shift.get();
            let (sx, sy) = self.scale.get();
            let alpha = self.alpha.get();
            if alpha <= 0.0 || sx <= 0.0 || sy <= 0.0 {
                return;
            }
            if (dx, dy) == (0.0, 0.0) && (sx, sy) == (1.0, 1.0) && alpha >= 1.0 {
                self.parent_snapshot(snapshot);
                return;
            }
            let obj = self.obj();
            let (px, py) = self.pivot.get();
            let (px, py) = (px * f64::from(obj.width()), py * f64::from(obj.height()));
            if alpha < 1.0 {
                snapshot.push_opacity(alpha);
            }
            snapshot.save();
            snapshot.translate(&gtk::graphene::Point::new(
                (dx + px) as f32,
                (dy + py) as f32,
            ));
            snapshot.scale(sx as f32, sy as f32);
            snapshot.translate(&gtk::graphene::Point::new(-px as f32, -py as f32));
            self.parent_snapshot(snapshot);
            snapshot.restore();
            if alpha < 1.0 {
                snapshot.pop();
            }
        }
    }
}

glib::wrapper! {
    /// Draws its one child moved, scaled and faded, without relayout.
    pub struct Zoom(ObjectSubclass<zoom_imp::Zoom>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Zoom {
    pub fn new(child: &impl IsA<gtk::Widget>) -> Self {
        let zoom = glib::Object::new::<Self>();
        child.set_parent(&zoom);
        zoom
    }

    pub fn set_pivot(&self, x: f64, y: f64) {
        self.imp().pivot.set((x, y));
    }

    pub fn set(&self, shift: (f64, f64), scale: (f64, f64), alpha: f64) {
        let imp = self.imp();
        // Apply every property before testing for damage: short-circuiting
        // the replacements can leave the terminal frame at an old scale
        // or opacity when its translation changes too.
        let shifted = imp.shift.replace(shift) != shift;
        let scaled = imp.scale.replace(scale) != scale;
        let faded = imp.alpha.replace(alpha) != alpha;
        if shifted || scaled || faded {
            self.queue_draw();
        }
    }

    /// Only the offset (a reflow), keeping scale and opacity.
    pub fn set_shift(&self, shift: (f64, f64)) {
        if self.imp().shift.replace(shift) != shift {
            self.queue_draw();
        }
    }

    /// Only the scale (about the pivot) and opacity, keeping the offset.
    pub fn set_look(&self, scale: f64, alpha: f64) {
        let imp = self.imp();
        let scaled = imp.scale.replace((scale, scale)) != (scale, scale);
        let faded = imp.alpha.replace(alpha) != alpha;
        if scaled || faded {
            self.queue_draw();
        }
    }

    pub fn shift(&self) -> (f64, f64) {
        self.imp().shift.get()
    }

    pub fn scale(&self) -> (f64, f64) {
        self.imp().scale.get()
    }

    pub fn alpha(&self) -> f64 {
        self.imp().alpha.get()
    }
}

#[cfg(test)]
mod gtk_zoom_tests {
    use super::*;

    #[test]
    #[ignore = "requires owned Xvfb/private D-Bus and explicit grid zoom admission"]
    fn actual_gtk_zoom_applies_all_properties_including_terminal_frame() {
        assert_eq!(std::env::var("TUNA_REQUIRE_GRID_ZOOM").as_deref(), Ok("1"));
        assert!(std::env::var_os("WAYLAND_DISPLAY").is_none());
        gtk::init().expect("actual GTK display required");
        let zoom = Zoom::new(&gtk::Label::new(Some("Folder apps")));
        zoom.set_pivot(0.0, 0.0);
        let source = (158.0, 268.0, 113.0, 113.0);
        let dialog = (279.0, 56.0, 722.0, 720.0);
        let full = Motion::from_gnome(true, false, 1.0);
        let apply = |elapsed| {
            let (shift, scale) = folder_zoom(source, dialog, FOLDER_ZOOM.at(elapsed, full));
            let alpha = FOLDER_FADE_IN.at(elapsed, full);
            zoom.set(shift, scale, alpha);
            assert_eq!(zoom.shift(), shift, "translation at {elapsed} ms");
            assert_eq!(zoom.scale(), scale, "scale at {elapsed} ms");
            assert_eq!(zoom.alpha(), alpha, "opacity at {elapsed} ms");
        };
        apply(0.0);
        apply(50.0);
        // A delayed frame callback may jump straight past the end. No
        // subsequent tick repairs it, so all three properties must land.
        apply(2933.0);
        assert_eq!(zoom.shift(), (0.0, 0.0));
        assert_eq!(zoom.scale(), (1.0, 1.0));
        assert_eq!(zoom.alpha(), 1.0);
        // Identical values and single-property updates must work too.
        apply(2933.0);
        zoom.set((0.0, 0.0), (1.0, 1.0), 0.4);
        assert_eq!(zoom.alpha(), 0.4);
        zoom.set_shift((12.0, 8.0));
        zoom.set_look(0.5, 0.0);
        assert_eq!(zoom.shift(), (12.0, 8.0), "look preserves reflow");
        assert_eq!(zoom.scale(), (0.5, 0.5));
        assert_eq!(zoom.alpha(), 0.0, "drag shrinks and fades in one call");
        zoom.set_look(1.0, 1.0);
        assert_eq!(zoom.scale(), (1.0, 1.0));
        assert_eq!(zoom.alpha(), 1.0, "drag end restores both properties");
        zoom.set_look(1.0, 0.3);
        assert_eq!(zoom.alpha(), 0.3, "opacity-only look update");
        println!("GRID-ZOOM pass simultaneous translation/scale/opacity, delayed terminal frame and drag look updates");
    }

    #[test]
    #[ignore = "requires owned Xvfb/private D-Bus and explicit grid zoom admission"]
    fn actual_gtk_page_tick_trace_records_real_clock_and_thread_scheduler() {
        assert_eq!(std::env::var("TUNA_REQUIRE_GRID_ZOOM").as_deref(), Ok("1"));
        assert!(std::env::var_os("WAYLAND_DISPLAY").is_none());
        // This exact test runs alone in its admitted process, before GTK
        // or the trace's lazy environment reader starts.
        std::env::set_var("TUNA_GRID_TRACE", "1");
        assert!(tracing());
        gtk::init().expect("actual GTK display required");
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let window = gtk::Window::builder()
            .default_width(128)
            .default_height(128)
            .child(&widget)
            .build();
        window.present();
        let context = glib::MainContext::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !widget.is_mapped() {
            context.iteration(false);
            assert!(
                std::time::Instant::now() < deadline,
                "GTK widget never mapped"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let ticks = Rc::new(Cell::new(0));
        let done = Rc::new(Cell::new(false));
        let slot = Rc::new(Slot::default());
        let seen = ticks.clone();
        let finished = done.clone();
        slot.start(
            &widget,
            "page-switch",
            PAGE_SWITCH.duration_ms,
            move |_| seen.set(seen.get() + 1),
            move || finished.set(true),
        );
        while !done.get() {
            context.iteration(false);
            assert!(
                std::time::Instant::now() < deadline,
                "GTK ticks never finished"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(ticks.get() >= 2, "initial and terminal samples required");
        THREAD_SCHED.with(|reader| {
            let mut reader = reader.borrow_mut();
            let observed = reader
                .read()
                .expect("real Linux main-thread scheduler observation");
            assert!(observed.run_ns > 0);
            println!(
                "GRID-TICK-TRACE pass actual GTK ticks={} run_ns={} wait_ns={} schedstats={}",
                ticks.get(),
                observed.run_ns,
                observed.wait_ns,
                reader.wait_enabled
            );
        });
        window.close();
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

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn scheduler_samples_require_all_fields_and_never_invent_zero() {
        assert_eq!(
            parse_schedstat(b"123456 789 12\n"),
            Some(SchedTimes {
                run_ns: 123456,
                wait_ns: 789,
            })
        );
        for bytes in [
            &b""[..],
            &b"1 2"[..],
            &b"1 nope 3"[..],
            &b"1 2 3 4"[..],
            &b"-1 2 3"[..],
            &b"18446744073709551616 2 3"[..],
        ] {
            assert_eq!(parse_schedstat(bytes), None);
        }
        let sample = TickSample {
            monotonic_us: 400,
            sched: SchedTimes {
                run_ns: 200,
                wait_ns: 100,
            },
        };
        let next = TickSample {
            monotonic_us: 700,
            sched: SchedTimes {
                run_ns: 450,
                wait_ns: 120,
            },
        };
        assert_eq!(next.since(sample), Some((300, 250, 20)));
        assert_eq!(sample.since(next), None);
    }

    #[test]
    fn curves_match_clutter_easing_modes() {
        for curve in [
            Curve::InQuad,
            Curve::OutQuad,
            Curve::OutCubic,
            Curve::OutExpo,
        ] {
            assert_eq!(curve.at(0.0), 0.0, "{curve:?}");
            assert_eq!(curve.at(1.0), 1.0, "{curve:?}");
            assert_eq!(curve.at(2.0), 1.0, "{curve:?} clamps");
        }
        assert!(close(Curve::InQuad.at(0.5), 0.25));
        assert!(close(Curve::OutQuad.at(0.5), 0.75));
        assert!(close(Curve::OutCubic.at(0.5), 0.875));
        assert!(close(Curve::OutExpo.at(0.5), 1.0 - 1.0 / 32.0));
    }

    #[test]
    fn gnome_51_durations() {
        assert_eq!(FOLDER_ZOOM.duration_ms, 200.0);
        assert_eq!(FOLDER_ZOOM.curve, Curve::OutExpo);
        assert_eq!(FOLDER_SHADE.duration_ms, 200.0);
        assert_eq!(FOLDER_ICON_HIDE.end_ms(FULL), 100.0);
        assert_eq!(FOLDER_ICON_SHOW.end_ms(FULL), 200.0);
        assert_eq!(FOLDER_ICON_SHOW.curve, Curve::InQuad);
        assert_eq!(LAUNCH_ZOOM.duration_ms, 250.0);
        assert_eq!(LAUNCH_ZOOM.curve, Curve::OutQuad);
        assert_eq!(PAGE_SWITCH.duration_ms, 300.0);
        assert_eq!(PAGE_SWITCH.curve, Curve::OutCubic);
        assert_eq!(PAGE_ARROW_FADE.duration_ms, 200.0);
        assert_eq!(REFLOW.curve, Curve::OutQuad);
        assert_eq!(GRID_FADE_IN.end_ms(FULL), 500.0);
        assert_eq!(GRID_FADE_IN.at(100.0, FULL), 0.0);
        assert_eq!(GRID_FADE_OUT.end_ms(FULL), 400.0);
        assert_eq!(
            GRID_FADE_OUT.at(200.0, Motion::from_gnome(true, true, 1.0)),
            0.75
        );
    }

    #[test]
    fn reduced_motion_keeps_fades_and_off_keeps_nothing() {
        let fades = Motion::from_gnome(true, true, 1.0);
        let off = Motion::from_gnome(false, false, 1.0);
        // Halfway through, a fade is still moving under Reduced Motion
        // while the zoom has already landed; with animations off both
        // have.
        assert!(FOLDER_SHADE.at(100.0, fades) < 1.0);
        assert_eq!(FOLDER_ZOOM.at(100.0, fades), 1.0);
        assert_eq!(FOLDER_SHADE.at(0.0, off), 1.0);
        assert_eq!(FOLDER_ZOOM.end_ms(fades), 0.0);
        assert_eq!(FOLDER_SHADE.end_ms(fades), 200.0);
        assert_eq!(FOLDER_SHADE.end_ms(off), 0.0);
        assert_eq!(PAGE_SWITCH.at(0.0, fades), 1.0);
    }

    #[test]
    fn slow_down_factor_stretches_durations_and_delays() {
        let slow = Motion::from_gnome(true, false, 2.0);
        assert_eq!(PAGE_SWITCH.end_ms(slow), 600.0);
        assert!(close(PAGE_SWITCH.at(300.0, slow), 0.875));
        assert_eq!(GRID_FADE_IN.end_ms(slow), 1000.0);
        assert_eq!(GRID_FADE_IN.at(200.0, slow), 0.0);
        assert!(close(GRID_FADE_IN.at(600.0, slow), Curve::OutQuad.at(0.5)));
    }

    #[test]
    fn delays_hold_the_start_value() {
        assert_eq!(GRID_FADE_IN.at(0.0, FULL), 0.0);
        assert_eq!(GRID_FADE_IN.at(100.0, FULL), 0.0);
        assert!(close(GRID_FADE_IN.at(300.0, FULL), Curve::OutQuad.at(0.5)));
        assert_eq!(GRID_FADE_IN.at(500.0, FULL), 1.0);
        assert_eq!(FOLDER_ICON_SHOW.at(50.0, FULL), 0.0);
        assert!(close(FOLDER_ICON_SHOW.at(150.0, FULL), 0.25));
    }

    #[test]
    fn folder_dialog_zooms_from_its_icon() {
        let source = (271.0, 456.0, 113.0, 113.0);
        let dialog = (279.0, 56.0, 722.0, 720.0);
        let (shift, scale) = folder_zoom(source, dialog, 0.0);
        assert_eq!(shift, (-8.0, 400.0));
        assert!(close(scale.0, 113.0 / 722.0) && close(scale.1, 113.0 / 720.0));
        assert_eq!(folder_zoom(source, dialog, 1.0), ((0.0, 0.0), (1.0, 1.0)));
        let ((x, y), _) = folder_zoom(source, dialog, 0.5);
        assert_eq!((x, y), (-4.0, 200.0));
    }

    #[test]
    fn launch_zoom_triples_and_stays_on_the_monitor() {
        let monitor = (0.0, 0.0, 1280.0, 800.0);
        let icon = (64.0, 64.0);
        assert_eq!(
            launch_box((640.0, 400.0), icon, monitor, 0.0),
            (640.0, 400.0, 1.0)
        );
        assert_eq!(
            launch_box((640.0, 400.0), icon, monitor, 1.0),
            (640.0, 400.0, 3.0)
        );
        // Near the left edge the grown clone slides right to stay whole.
        assert_eq!(launch_center((40.0, 400.0), icon, monitor), (96.0, 400.0));
        assert_eq!(
            launch_center((1270.0, 790.0), icon, monitor),
            (1184.0, 704.0)
        );
        let (x, _, s) = launch_box((40.0, 400.0), icon, monitor, 0.5);
        assert_eq!((x, s), (68.0, 2.0));
        // A grid tile's clone (icon and label) is taller than wide: each
        // axis stays on the monitor by its own grown extent.
        assert_eq!(
            launch_center((60.0, 760.0), (100.0, 120.0), monitor),
            (150.0, 620.0)
        );
    }

    #[test]
    fn page_dots_follow_the_drawn_position() {
        let inactive = (DOT_INACTIVE_SCALE, DOT_INACTIVE_OPACITY);
        assert_eq!(page_dot_look(0.0, 0, false, false), (1.0, 1.0));
        assert_eq!(page_dot_look(0.0, 1, false, false), inactive);
        assert_eq!(page_dot_look(0.0, 2, false, false), inactive);
        assert!(close(DOT_INACTIVE_OPACITY, 128.0 / 255.0));
        // Halfway through a 300 ms switch from page 0 to page 1, the
        // pages are drawn at 0.875 and both dots are part way.
        let position = lerp(0.0, 1.0, PAGE_SWITCH.at(150.0, FULL));
        let (scale, opacity) = page_dot_look(position, 1, false, false);
        assert!(close(scale, 2.0 / 3.0 + 0.875 / 3.0));
        assert!(close(opacity, (128.0 + 127.0 * 0.875) / 255.0));
        let (scale, _) = page_dot_look(position, 0, false, false);
        assert!(close(scale, 2.0 / 3.0 + 0.125 / 3.0));
        // Hovered, an inactive dot is opaque; pressed, it shrinks to half.
        assert_eq!(page_dot_look(0.0, 1, true, false), (2.0 / 3.0, 1.0));
        assert_eq!(page_dot_look(0.0, 1, false, true), (0.5, 128.0 / 255.0));
        assert_eq!(page_dot_look(1.0, 1, true, true), (1.0, 1.0));
    }

    #[test]
    fn swipe_history_keeps_150_ms_of_moves() {
        let mut history = SwipeHistory::default();
        history.append(1000, 0.0);
        history.append(1010, 5.0);
        history.append(1020, 5.0);
        assert_eq!(history.velocity(), 0.5);
        history.trim(1165);
        assert_eq!(history.velocity(), 0.0);
        history.append(1170, 6.0);
        assert_eq!(history.velocity(), 6.0 / 150.0);
        history.reset();
        assert_eq!(history.velocity(), 0.0);
    }

    #[test]
    fn a_swipe_goes_at_most_a_page_past_where_it_began() {
        assert_eq!(swipe_bounds(0.0, 3.0), (0.0, 1.0));
        assert_eq!(swipe_bounds(2.0, 3.0), (1.0, 3.0));
        assert_eq!(swipe_bounds(3.0, 3.0), (2.0, 3.0));
        assert_eq!(swipe_bounds(2.003, 3.0), (1.0, 3.0));
        // Begun mid-switch, it may reach a page past either neighbour.
        assert_eq!(swipe_bounds(1.5, 3.0), (0.0, 3.0));
        assert_eq!(swipe_cancel_page(1.5), 2.0);
        assert_eq!(swipe_cancel_page(1.49), 1.0);
    }

    #[test]
    fn released_swipes_settle_as_gnome_swipe_tracker() {
        let end = |progress, initial, velocity, touchpad| {
            swipe_end(progress, initial, velocity, 1000.0, touchpad, 3.0)
        };
        // Slow: the nearest page (the earlier on a tie), at the slowest
        // base velocity, so the longest single-page ease.
        assert_eq!(end(0.4, 0.0, 0.1, false), (0.0, 400.0));
        assert_eq!(end(0.6, 0.0, 0.1, false), (1.0, 400.0));
        assert_eq!(end(0.5, 0.0, 0.0, false), (0.0, 400.0));
        // A gentle touch flick still turns the page.
        assert_eq!(end(0.1, 0.0, 0.5, false), (1.0, 400.0));
        // The same speed on a touchpad is under its threshold: back at
        // the base velocity, 0.1 page / 0.002 * 3 = 150 ms.
        let (page, ms) = end(0.1, 0.0, 0.5, true);
        assert_eq!(page, 0.0);
        assert!((ms - 150.0).abs() < 1e-6, "{ms}");
        // A fast flick: the rest of the way at three times its speed.
        let (page, ms) = end(0.5, 0.0, 4.0, false);
        assert_eq!(page, 1.0);
        assert!((ms - 375.0).abs() < 1e-6, "{ms}");
        let (page, ms) = end(1.4, 1.0, 10.0, false);
        assert_eq!(page, 2.0);
        assert!((ms - 180.0).abs() < 1e-6, "{ms}");
        // Flicked back past where it began: the page before.
        assert_eq!(end(0.7, 1.0, -1.0, false), (0.0, 400.0));
        // Already there: nothing to ease.
        assert_eq!(end(1.0, 1.0, 0.0, false), (1.0, 0.0));
        // Two pages away the longest ease grows with log2(1 + pages).
        let (page, ms) = end(0.9, 1.5, 2.0, false);
        assert_eq!(page, 2.0);
        assert!((ms - 400.0 * 3f64.log2()).abs() < 1e-6, "{ms}");
        let (_, ms) = end(0.9, 1.5, 0.1, false);
        assert!(ms >= 100.0);
    }

    fn page_position(from: f64, to: f64, elapsed_ms: f64, motion: Motion) -> f64 {
        lerp(from, to, PAGE_SWITCH.at(elapsed_ms, motion))
    }

    #[test]
    fn page_switch_eases_out_cubic_over_300_ms() {
        assert_eq!(page_position(0.0, 1.0, 0.0, FULL), 0.0);
        assert!(close(page_position(0.0, 1.0, 150.0, FULL), 0.875));
        assert_eq!(page_position(0.0, 1.0, 300.0, FULL), 1.0);
        assert_eq!(page_position(2.0, 1.0, 150.0, FULL), 1.125);
        assert_eq!(
            page_position(0.0, 1.0, 0.0, Motion::from_gnome(true, true, 1.0)),
            1.0
        );
        assert_eq!(
            page_position(0.0, 1.0, 0.0, Motion::from_gnome(false, false, 1.0)),
            1.0
        );
        // goToPage takes the whole 300 ms even from part of the way
        // there (iconGrid.js: `duration: animate ? PAGE_SWITCH_TIME : 0`).
        assert!(close(page_position(0.6, 0.0, 150.0, FULL), 0.6 * 0.125));
        assert_eq!(PAGE_SWITCH.end_ms(FULL), 300.0);
    }

    #[test]
    fn reflow_moves_by_whole_cells_and_staggers() {
        assert_eq!(reflow_shift(0, 1, 8), (-1.0, 0.0));
        assert_eq!(reflow_shift(7, 8, 8), (7.0, -1.0));
        assert_eq!(reflow_shift(9, 2, 8), (-1.0, 1.0));
        let third = REFLOW.delayed(2.0 * REFLOW_STAGGER_MS);
        assert_eq!(third.at(20.0, FULL), 0.0);
        assert!(close(third.at(145.0, FULL), 0.75));
        assert_eq!(third.end_ms(FULL), 270.0);
    }
}
