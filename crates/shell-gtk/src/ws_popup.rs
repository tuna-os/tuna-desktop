//! GNOME 51's workspace switcher popup (workspaceSwitcherPopup.js): a
//! row of workspace dots in an OSD pill near the bottom of the screen,
//! the active one larger and white, shown for 600 ms after a workspace
//! key outside the overview (windowManager.js:1869-1880), even when the
//! key cannot move past the first or last workspace. Only a hidden popup
//! fades in, over 100 ms (`ANIMATION_TIME`, ease-out-quad); a key while it
//! shows, or while it fades out, makes it opaque at once and restarts the
//! 600 ms (`display`, workspaceSwitcherPopup.js:93-111). It then fades out
//! over 100 ms (`_onTimeout`, :113-121).
//!
//! The fades run on the shell's animation clock ([`crate::anim_clock`]),
//! so proofs sample them at exact delays. `DISPLAY_TIMEOUT` is a plain
//! GLib timeout in GNOME: the slow-down factor stretches the fades, not
//! the hold, and the hold is measured on the same clock so a frozen proof
//! clock holds the popup too.

use std::cell::{Cell, RefCell};
use std::io::Write;
use std::rc::Rc;
use std::time::Duration;

use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use tuna_shell_control::MotionPolicy;

/// DISPLAY_TIMEOUT.
pub const SHOW_FOR: Duration = Duration::from_millis(600);
/// ANIMATION_TIME: the fade in and out.
pub const FADE_MS: f64 = 100.0;

/// One opacity fade of the pill on animation time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fade {
    pub from: f64,
    pub to: f64,
    pub start: Duration,
    /// Already scaled by the motion policy; 0 is instant.
    pub duration_ms: f64,
}

impl Fade {
    /// `ANIMATION_TIME` from `from` to `to` starting at `start`. The fade
    /// is opacity, so fade-only motion keeps it; animations off snap it.
    pub fn new(from: f64, to: f64, start: Duration, policy: MotionPolicy) -> Self {
        Self {
            from,
            to,
            start,
            duration_ms: if policy.allows_fades() {
                policy.adjust_ms(FADE_MS)
            } else {
                0.0
            },
        }
    }

    fn progress(&self, now: Duration) -> f64 {
        if self.duration_ms <= 0.0 {
            return 1.0;
        }
        let elapsed = now.saturating_sub(self.start).as_secs_f64() * 1000.0;
        (elapsed / self.duration_ms).clamp(0.0, 1.0)
    }

    /// The pill's opacity at `now` (`EASE_OUT_QUAD`).
    pub fn opacity(&self, now: Duration) -> f64 {
        let p = self.progress(now);
        self.from + (self.to - self.from) * (1.0 - (1.0 - p).powi(2))
    }

    pub fn done(&self, now: Duration) -> bool {
        self.progress(now) >= 1.0
    }
}

/// How long the popup still holds before its fade-out, `now` after the
/// last display at `shown_at`; `None` once the fade-out is due, which
/// starts exactly at `shown_at + SHOW_FOR` however late it is noticed.
pub fn hold_left(shown_at: Duration, now: Duration) -> Option<Duration> {
    let due = shown_at + SHOW_FOR;
    (now < due).then(|| due - now)
}

/// Each `.ws-switcher-indicator` slot: 32px (dot plus margins).
const SLOT: f64 = 32.0;
/// `.workspace-switcher { spacing: 12px; padding: 12px 18px }`.
const SPACING: f64 = 12.0;
/// Dot diameters: 2.667px padding a side (inactive), 5.333px (active).
const DOT: f64 = 16.0 / 3.0;
const ACTIVE_DOT: f64 = 32.0 / 3.0;

/// The pill's content width for `count` workspaces.
pub fn content_width(count: u32) -> f64 {
    let n = f64::from(count.max(1));
    n * SLOT + (n - 1.0) * SPACING
}

/// With `TUNA_TRANSIENT_TRACE` set, the popup logs each display, fade,
/// changed frame and hide (the GTK shell proof's G-WS-POPUP-* gates), as
/// one write so a line never interleaves with the compositor's.
fn trace(what: std::fmt::Arguments) {
    if std::env::var_os("TUNA_TRANSIENT_TRACE").is_some() {
        let line = format!("tuna-shell-gtk: ws-popup {what}\n");
        let _ = std::io::stderr().lock().write_all(line.as_bytes());
    }
}

fn clock_name() -> &'static str {
    if crate::anim_clock::manual() {
        "manual"
    } else {
        "real"
    }
}

pub struct WorkspacePopup {
    window: gtk::Window,
    pill: gtk::Box,
    dots: gtk::DrawingArea,
    /// Bumped by every fade so a superseded one stops.
    fade: Cell<u64>,
    state: Rc<Cell<(u32, u32)>>,
    hold: RefCell<Option<glib::SourceId>>,
    /// The last display, on animation time.
    shown_at: Cell<Duration>,
    /// The last opacity traced.
    traced: Cell<Option<f64>>,
}

impl WorkspacePopup {
    pub fn new(app: &gtk::Application) -> Rc<Self> {
        let window = gtk::Window::new();
        window.set_application(Some(app));
        window.add_css_class("tuna-workspace-popup");
        window.set_title(Some("Workspace Switcher"));
        window.init_layer_shell();
        window.set_layer(Layer::Overlay);
        window.set_namespace(Some(tuna_shell_control::WORKSPACE_POPUP_NAMESPACE));
        window.set_anchor(Edge::Bottom, true);
        // `margin-bottom: 4em` at the shell's 14.666px.
        window.set_margin(Edge::Bottom, 59);
        window.set_exclusive_zone(-1);
        window.set_keyboard_mode(KeyboardMode::None);
        window.connect_realize(|w| {
            if let Some(surface) = w.surface() {
                surface.set_input_region(Some(&gtk::cairo::Region::create()));
            }
        });
        let pill = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        pill.add_css_class("workspace-switcher");
        let dots = gtk::DrawingArea::new();
        dots.set_content_height(SLOT as i32);
        let state = Rc::new(Cell::new((0u32, 1u32)));
        {
            let state = state.clone();
            dots.set_draw_func(move |_, cr, _, h| {
                let (active, count) = state.get();
                for i in 0..count.max(1) {
                    let cx = f64::from(i) * (SLOT + SPACING) + SLOT / 2.0;
                    let cy = f64::from(h) / 2.0;
                    let (d, alpha) = if i == active {
                        (ACTIVE_DOT, 1.0)
                    } else {
                        (DOT, 0.5)
                    };
                    cr.set_source_rgba(1.0, 1.0, 1.0, alpha);
                    cr.arc(cx, cy, d / 2.0, 0.0, std::f64::consts::TAU);
                    let _ = cr.fill();
                }
            });
        }
        pill.append(&dots);
        window.set_child(Some(&pill));
        Rc::new(Self {
            window,
            pill,
            dots,
            fade: Cell::new(0),
            state,
            hold: RefCell::new(None),
            shown_at: Cell::new(Duration::ZERO),
            traced: Cell::new(None),
        })
    }

    /// Show workspace `index` of `count`.
    pub fn display(self: &Rc<Self>, index: u32, count: u32) {
        let count = count.max(index + 1);
        let now = crate::anim_clock::now();
        self.shown_at.set(now);
        // Every display traces its first frame, even an unchanged one.
        self.traced.set(None);
        self.state.set((index, count));
        self.dots
            .set_content_width(content_width(count).round() as i32);
        self.dots.queue_draw();
        self.window.set_default_size(1, 1);
        let visible = self.window.is_visible();
        trace(format_args!(
            "display index={index} count={count} visible={visible} clock={}",
            clock_name()
        ));
        // GNOME fades in only when hidden; a repeat key (even during the
        // fade-out) is opaque at once.
        if visible {
            self.fade.set(self.fade.get().wrapping_add(1));
            self.set_opacity(1.0);
        } else {
            self.set_opacity(0.0);
            self.window.present();
            self.fade_to(Fade::new(0.0, 1.0, now, crate::motion::current()));
        }
        if let Some(id) = self.hold.borrow_mut().take() {
            id.remove();
        }
        self.arm_hold(SHOW_FOR);
    }

    /// Wait (real time) `after`, then fade out once the hold has run on
    /// animation time. Real time reaches it on the first wake; a frozen
    /// proof clock keeps it waiting, re-checked at most every 50 ms.
    fn arm_hold(self: &Rc<Self>, after: Duration) {
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(after.max(Duration::from_millis(50)), move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            ui.hold.borrow_mut().take();
            let shown_at = ui.shown_at.get();
            match hold_left(shown_at, crate::anim_clock::now()) {
                Some(left) => ui.arm_hold(left),
                None => ui.fade_to(Fade::new(
                    ui.pill.opacity(),
                    0.0,
                    shown_at + SHOW_FOR,
                    crate::motion::current(),
                )),
            }
        });
        *self.hold.borrow_mut() = Some(id);
    }

    fn set_opacity(&self, opacity: f64) {
        self.pill.set_opacity(opacity);
        if self.traced.replace(Some(opacity)) != Some(opacity) {
            trace(format_args!("frame opacity={opacity:.4}"));
        }
    }

    /// Run `fade` on the pill, hiding the window when it ends at 0.
    fn fade_to(self: &Rc<Self>, fade: Fade) {
        let generation = self.fade.get().wrapping_add(1);
        self.fade.set(generation);
        trace(format_args!(
            "fade to={} from={:.4} duration={:.0} clock={}",
            fade.to,
            fade.from,
            fade.duration_ms,
            clock_name()
        ));
        let finish = move |ui: &Self| {
            ui.set_opacity(fade.to);
            if fade.to == 0.0 {
                ui.window.set_visible(false);
                trace(format_args!("hidden"));
            }
        };
        let now = crate::anim_clock::now();
        if fade.done(now) {
            finish(self);
            return;
        }
        self.set_opacity(fade.opacity(now));
        let weak = Rc::downgrade(self);
        self.pill.add_tick_callback(move |_, _| {
            let Some(ui) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if ui.fade.get() != generation {
                return glib::ControlFlow::Break;
            }
            let now = crate::anim_clock::now();
            if fade.done(now) || !crate::motion::current().allows_fades() {
                finish(&ui);
                return glib::ControlFlow::Break;
            }
            ui.set_opacity(fade.opacity(now));
            glib::ControlFlow::Continue
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuna_shell_control::MotionLevel;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn the_pill_grows_32px_and_12px_a_workspace() {
        assert_eq!(content_width(1), 32.0);
        assert_eq!(content_width(2), 76.0);
        assert_eq!(content_width(3), 120.0);
    }

    #[test]
    fn the_pill_fades_over_100_ms_ease_out_quad() {
        let full = MotionPolicy::default();
        let t0 = ms(5_000);
        let fade_in = Fade::new(0.0, 1.0, t0, full);
        assert_eq!(fade_in.opacity(t0), 0.0);
        assert_eq!(fade_in.opacity(t0 + ms(50)), 0.75);
        assert!(!fade_in.done(t0 + ms(99)));
        assert_eq!(fade_in.opacity(t0 + ms(100)), 1.0);
        assert!(fade_in.done(t0 + ms(100)));
        let fade_out = Fade::new(1.0, 0.0, t0, full);
        assert_eq!(fade_out.opacity(t0 + ms(50)), 0.25);
    }

    #[test]
    fn reduced_motion_keeps_the_fade_and_animations_off_snap_it() {
        let t0 = ms(5_000);
        let reduced = MotionPolicy::new(MotionLevel::FadeOnly, 1.0);
        assert_eq!(Fade::new(0.0, 1.0, t0, reduced).opacity(t0 + ms(50)), 0.75);
        let off = MotionPolicy::new(MotionLevel::Off, 1.0);
        let snap = Fade::new(0.0, 1.0, t0, off);
        assert_eq!(snap.opacity(t0), 1.0);
        assert!(snap.done(t0));
    }

    #[test]
    fn the_slowdown_factor_stretches_the_fades_but_not_the_hold() {
        let t0 = ms(5_000);
        let slow = MotionPolicy::new(MotionLevel::Full, 2.0);
        let fade = Fade::new(0.0, 1.0, t0, slow);
        assert_eq!(fade.opacity(t0 + ms(100)), 0.75);
        assert!(fade.done(t0 + ms(200)));
        // DISPLAY_TIMEOUT is a GLib timeout: 600 ms whatever the factor.
        assert_eq!(hold_left(t0, t0 + ms(599)), Some(ms(1)));
        assert_eq!(hold_left(t0, t0 + ms(600)), None);
    }

    #[test]
    fn the_fade_out_starts_600_ms_after_the_last_display() {
        let t0 = ms(5_000);
        assert_eq!(hold_left(t0, t0), Some(SHOW_FOR));
        // Noticed late (a frozen clock moved past it): the fade still
        // starts at the due time, so a sample 50 ms after it is half way.
        let full = MotionPolicy::default();
        let out = Fade::new(1.0, 0.0, t0 + SHOW_FOR, full);
        assert_eq!(out.opacity(t0 + ms(650)), 0.25);
        assert!(out.done(t0 + ms(700)));
    }
}
