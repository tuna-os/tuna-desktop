//! GNOME 51's Activities workspace dots (`panel.js` `WorkspaceDot` and
//! `WorkspaceIndicators`, panel.js:44-196): each dot's expansion is one
//! minus its distance from the workspaces adjustment's value, so the wide
//! pill slides between dots as the workspace changes. An expanded dot is
//! 8px times the width multiplier wide, fully opaque and full size; a
//! collapsed one is 8px, half opaque and drawn at 0.75. Dots for added
//! workspaces scale in and dots for removed ones scale out over 500 ms
//! ease-out-cubic (panel.js:107-131).
//!
//! The adjustment's value is the primary monitor's workspace animation
//! progress (workspaceAnimation.js:255-268): a key switch moves it over
//! 250 ms ease-out-cubic, and a three-finger swipe sets it as the fingers
//! move, then eases it to where the swipe lands over the release time.
//! The compositor reports those swipe phases
//! (`Message::WorkspaceProgress`); [`Dots::follow`] applies them.
//!
//! Times are animation-clock milliseconds ([`crate::anim_clock`]) and
//! durations are scaled by the motion policy when they start. The pill's
//! slide and the scaling are motion: fade-only snaps them, as does
//! animations off. Held fingers move the dots under every policy (direct
//! manipulation; GNOME sets the value without an ease).

use std::cell::{Cell, RefCell};
use std::io::Write;
use std::rc::Rc;

use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use tuna_shell_control::MotionPolicy;

/// The `.workspace-dot` size.
const DOT: f64 = 8.0;
/// `INACTIVE_WORKSPACE_DOT_SCALE`.
const INACTIVE_SCALE: f64 = 0.75;
/// The expansion follows the workspace switch (`WINDOW_ANIMATION_TIME`).
pub const MOVE_MS: f64 = 250.0;
/// `scaleIn` / `scaleOutAndDestroy`.
pub const SCALE_MS: f64 = 500.0;

pub fn ease_out_cubic(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0) - 1.0;
    t * t * t + 1.0
}

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

/// `_updateExpansion`'s width multiplier for `n` dots.
pub fn width_multiplier(n: usize) -> f64 {
    match n {
        0..=2 => 3.625,
        3..=5 => 3.25,
        _ => 2.75,
    }
}

/// One dot as drawn: its layout width and the visible pill inside it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DotLook {
    pub slot: f64,
    pub width: f64,
    pub height: f64,
    pub opacity: f64,
}

/// A dot at `expansion` (0 collapsed, 1 the active pill), scaled by
/// `scale` (its scale-in or scale-out).
pub fn look(expansion: f64, scale: f64, multiplier: f64) -> DotLook {
    let e = expansion.clamp(0.0, 1.0);
    let slot = DOT * lerp(1.0, multiplier, e);
    let s = lerp(INACTIVE_SCALE, 1.0, e) * scale;
    DotLook {
        slot,
        width: slot * s,
        height: DOT * s,
        opacity: lerp(0.5, 1.0, e),
    }
}

/// One ease-out-cubic value on animation time (ms).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Ease {
    from: f64,
    to: f64,
    start: f64,
    duration: f64,
}

impl Ease {
    fn at(value: f64) -> Self {
        Self {
            from: value,
            to: value,
            start: 0.0,
            duration: 0.0,
        }
    }

    /// From wherever it is drawn at `now` to `to`.
    fn retarget(&self, now: f64, to: f64, duration: f64) -> Self {
        if duration <= 0.0 {
            return Self::at(to);
        }
        Self {
            from: self.value(now),
            to,
            start: now,
            duration,
        }
    }

    fn value(&self, now: f64) -> f64 {
        if self.duration <= 0.0 {
            return self.to;
        }
        lerp(
            self.from,
            self.to,
            ease_out_cubic((now - self.start) / self.duration),
        )
    }

    fn done(&self, now: f64) -> bool {
        self.duration <= 0.0 || now - self.start >= self.duration
    }
}

/// The dots' animation state.
#[derive(Debug, Clone)]
pub struct Dots {
    /// Each dot's scale-in or scale-out.
    dots: Vec<Ease>,
    /// The workspaces adjustment's value (fractional workspace index).
    position: Ease,
}

impl Default for Dots {
    fn default() -> Self {
        Self {
            dots: Vec::new(),
            position: Ease::at(0.0),
        }
    }
}

/// A motion `ms` long under `policy`: scaled, or 0 when it may not move.
fn motion_ms(policy: MotionPolicy, ms: f64) -> f64 {
    if policy.allows_motion() {
        policy.adjust_ms(ms)
    } else {
        0.0
    }
}

impl Dots {
    fn removing(dot: &Ease) -> bool {
        dot.to == 0.0
    }

    /// Show `count` workspaces with `active` current (`notify::upper` and
    /// the end of a key switch).
    pub fn set(&mut self, count: usize, active: usize, now: f64, policy: MotionPolicy) {
        // The first workspaces seen are simply there.
        let first = self.dots.is_empty();
        let scale_ms = motion_ms(policy, SCALE_MS);
        let live: Vec<usize> = (0..self.dots.len())
            .filter(|i| !Self::removing(&self.dots[*i]))
            .collect();
        if live.len() < count {
            for _ in live.len()..count {
                let dot = if first {
                    Ease::at(1.0)
                } else {
                    Ease::at(0.0).retarget(now, 1.0, scale_ms)
                };
                self.dots.push(dot);
            }
        } else {
            // GNOME drops the last dots.
            for i in live.into_iter().skip(count) {
                self.dots[i] = self.dots[i].retarget(now, 0.0, scale_ms);
            }
        }
        let target = active as f64;
        if first {
            self.position = Ease::at(target);
        } else if self.position.to != target {
            self.position = self
                .position
                .retarget(now, target, motion_ms(policy, MOVE_MS));
        }
        self.prune(now);
    }

    /// A swipe phase: held fingers (`duration_ms` 0) put the value at
    /// `position`; a release eases it there over the swipe's own settle
    /// time, already scaled by the compositor.
    pub fn follow(&mut self, position: f64, duration_ms: f64, now: f64, policy: MotionPolicy) {
        let duration = if policy.allows_motion() {
            duration_ms
        } else {
            0.0
        };
        self.position = self.position.retarget(now, position, duration);
    }

    /// Jump every motion to its end.
    pub fn finish(&mut self) {
        self.dots.retain(|dot| !Self::removing(dot));
        self.dots.fill(Ease::at(1.0));
        self.position = Ease::at(self.position.to);
    }

    /// Drop dots whose scale-out finished; returns whether anything
    /// still moves.
    pub fn prune(&mut self, now: f64) -> bool {
        self.dots
            .retain(|dot| !(Self::removing(dot) && dot.done(now)));
        !self.position.done(now) || self.dots.iter().any(|dot| !dot.done(now))
    }

    /// The adjustment's value at `now`.
    pub fn position(&self, now: f64) -> f64 {
        self.position.value(now)
    }

    /// Where the value is heading.
    pub fn target(&self) -> f64 {
        self.position.to
    }

    /// Every dot as drawn at `now` (`_updateExpansion`).
    pub fn looks(&self, now: f64) -> Vec<DotLook> {
        let live = self.dots.iter().filter(|dot| !Self::removing(dot)).count();
        let multiplier = width_multiplier(live);
        let position = self.position(now);
        self.dots
            .iter()
            .enumerate()
            .map(|(i, dot)| {
                let expansion = 1.0 - (i as f64 - position).abs();
                look(expansion, dot.value(now), multiplier)
            })
            .collect()
    }
}

/// Animation time in milliseconds.
fn now_ms() -> f64 {
    crate::anim_clock::now().as_secs_f64() * 1000.0
}

/// With `TUNA_TRANSIENT_TRACE` set, the dots log each change of what they
/// draw (G-WS-DOTS-* gates), as one write so the line stays whole.
fn trace(what: std::fmt::Arguments) {
    if std::env::var_os("TUNA_TRANSIENT_TRACE").is_some() {
        let line = format!("tuna-shell-gtk: ws-dots {what}\n");
        let _ = std::io::stderr().lock().write_all(line.as_bytes());
    }
}

fn traced() -> bool {
    std::env::var_os("TUNA_TRANSIENT_TRACE").is_some()
}

fn clock_name() -> &'static str {
    if crate::anim_clock::manual() {
        "manual"
    } else {
        "real"
    }
}

/// The dots in the Activities button.
pub struct Indicator {
    row: gtk::Box,
    dots: RefCell<Dots>,
    ticking: Cell<bool>,
    /// The last frame traced.
    last_trace: RefCell<String>,
}

impl Indicator {
    pub fn new(row: gtk::Box) -> Rc<Self> {
        Rc::new(Self {
            row,
            dots: RefCell::default(),
            ticking: Cell::new(false),
            last_trace: RefCell::default(),
        })
    }

    /// Show the workspaces `pills` describes (`true` the active one).
    pub fn show(self: &Rc<Self>, pills: &[bool]) {
        let active = pills.iter().position(|a| *a).unwrap_or(0);
        let now = now_ms();
        let policy = crate::motion::current();
        let from = self.dots.borrow().position(now);
        // Every set traces its first frame, even an unchanged one.
        self.last_trace.borrow_mut().clear();
        self.dots.borrow_mut().set(pills.len(), active, now, policy);
        trace(format_args!(
            "set count={} active={active} from={from:.4} motion={} clock={}",
            pills.len(),
            policy.allows_motion(),
            clock_name()
        ));
        self.run(now);
    }

    /// A workspace swipe phase from the compositor.
    pub fn follow(self: &Rc<Self>, position: f64, duration_ms: f64) {
        let now = now_ms();
        let policy = crate::motion::current();
        self.dots
            .borrow_mut()
            .follow(position, duration_ms, now, policy);
        self.last_trace.borrow_mut().clear();
        trace(format_args!(
            "follow position={position:.4} duration={duration_ms:.3} motion={} clock={}",
            policy.allows_motion(),
            clock_name()
        ));
        self.run(now);
    }

    /// Draw `now` and keep drawing while anything moves.
    fn run(self: &Rc<Self>, now: f64) {
        if !self.apply(now) || self.ticking.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        self.row.add_tick_callback(move |_, _| {
            let Some(ui) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if !crate::motion::current().allows_motion() {
                ui.dots.borrow_mut().finish();
            }
            if ui.apply(now_ms()) {
                glib::ControlFlow::Continue
            } else {
                ui.ticking.set(false);
                glib::ControlFlow::Break
            }
        });
    }

    /// Lay the dots out for `now`; returns whether any still move.
    fn apply(&self, now: f64) -> bool {
        let moving = self.dots.borrow_mut().prune(now);
        let looks = self.dots.borrow().looks(now);
        let position = self.dots.borrow().position(now);
        let active = self.dots.borrow().target().round() as usize;
        let mut child = self.row.first_child();
        for (i, look) in looks.iter().enumerate() {
            let pill = match child.take() {
                Some(widget) => widget,
                None => {
                    let pill = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                    pill.add_css_class("ws-pill");
                    pill.set_valign(gtk::Align::Center);
                    pill.set_halign(gtk::Align::Center);
                    self.row.append(&pill);
                    pill.upcast()
                }
            };
            child = pill.next_sibling();
            if i == active {
                pill.add_css_class("active");
            } else {
                pill.remove_css_class("active");
            }
            let width = look.width.round() as i32;
            let height = look.height.round() as i32;
            let side = ((look.slot.round() as i32 - width) / 2).max(0);
            let edge = ((DOT.round() as i32 - height) / 2).max(0);
            pill.set_size_request(width, height);
            pill.set_margin_start(side);
            pill.set_margin_end(look.slot.round() as i32 - width - side);
            pill.set_margin_top(edge);
            pill.set_margin_bottom(DOT.round() as i32 - height - edge);
            pill.set_opacity(look.opacity);
            pill.set_visible(width > 0 && height > 0);
        }
        while let Some(extra) = child {
            child = extra.next_sibling();
            self.row.remove(&extra);
        }
        if traced() {
            let join =
                |f: &dyn Fn(&DotLook) -> String| looks.iter().map(f).collect::<Vec<_>>().join(",");
            let line = format!(
                "frame position={position:.4} slots={} widths={} heights={} opacities={}",
                join(&|l| format!("{:.3}", l.slot)),
                join(&|l| format!("{:.3}", l.width)),
                join(&|l| format!("{:.3}", l.height)),
                join(&|l| format!("{:.4}", l.opacity)),
            );
            if *self.last_trace.borrow() != line {
                trace(format_args!("{line}"));
                *self.last_trace.borrow_mut() = line;
            }
        }
        moving
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuna_shell_control::MotionLevel;

    fn full() -> MotionPolicy {
        MotionPolicy::default()
    }

    #[test]
    fn settled_dots_match_gnome_sizes() {
        // Two workspaces: the active pill 29px, the other a 6px dot in
        // an 8px slot at half opacity.
        let active = look(1.0, 1.0, width_multiplier(2));
        assert_eq!(
            (active.slot, active.height, active.opacity),
            (29.0, 8.0, 1.0)
        );
        let idle = look(0.0, 1.0, width_multiplier(2));
        assert_eq!(
            (idle.slot, idle.width, idle.height, idle.opacity),
            (8.0, 6.0, 6.0, 0.5)
        );
        assert_eq!(look(1.0, 1.0, width_multiplier(5)).slot, 26.0);
        assert_eq!(look(1.0, 1.0, width_multiplier(6)).slot, 22.0);
    }

    #[test]
    fn the_pill_moves_between_dots_over_250_ms() {
        let mut dots = Dots::default();
        dots.set(3, 0, 0.0, full());
        dots.set(3, 1, 1000.0, full());
        let start = dots.looks(1000.0);
        assert_eq!(start[0].opacity, 1.0);
        // Ease-out-cubic is 87.5% of the way half way through.
        assert_eq!(dots.position(1125.0), 0.875);
        let mid = dots.looks(1125.0);
        assert!(mid[0].slot > 8.0 && mid[0].slot < start[0].slot);
        assert!(mid[1].slot > 8.0);
        assert!(dots.prune(1125.0));
        let end = dots.looks(1250.0);
        assert_eq!((end[0].slot, end[1].slot), (8.0, 26.0));
        assert!(!dots.prune(1250.0));
    }

    #[test]
    fn a_second_switch_retargets_from_the_drawn_pill() {
        let mut dots = Dots::default();
        dots.set(4, 0, 0.0, full());
        dots.set(4, 1, 1000.0, full());
        let shown = dots.position(1060.0);
        dots.set(4, 2, 1060.0, full());
        assert_eq!(dots.position(1060.0), shown);
        let mut last = shown;
        for step in 1..=25 {
            let now = 1060.0 + f64::from(step) * 10.0;
            let value = dots.position(now);
            assert!(value >= last);
            last = value;
        }
        assert_eq!(dots.position(1310.0), 2.0);
    }

    #[test]
    fn added_dots_scale_in_and_removed_dots_scale_out_over_500_ms() {
        let mut dots = Dots::default();
        dots.set(2, 0, 0.0, full());
        dots.set(3, 0, 1000.0, full());
        assert_eq!(dots.looks(1000.0)[2].width, 0.0);
        assert!(dots.looks(1250.0)[2].width > 0.0);
        assert_eq!(dots.looks(1500.0)[2].width, 6.0);
        dots.set(2, 0, 2000.0, full());
        assert_eq!(dots.looks(2000.0).len(), 3);
        assert!(dots.prune(2400.0));
        assert!(!dots.prune(2500.0));
        assert_eq!(dots.looks(2500.0).len(), 2);
    }

    #[test]
    fn the_slowdown_factor_stretches_the_dots() {
        let slow = MotionPolicy::new(MotionLevel::Full, 2.0);
        let mut dots = Dots::default();
        dots.set(3, 0, 0.0, slow);
        dots.set(3, 1, 1000.0, slow);
        assert_eq!(dots.position(1250.0), 0.875);
        assert!(dots.prune(1499.0));
        assert!(!dots.prune(1500.0));
    }

    #[test]
    fn without_motion_everything_lands_at_once() {
        for level in [MotionLevel::Off, MotionLevel::FadeOnly] {
            let policy = MotionPolicy::new(level, 1.0);
            let mut dots = Dots::default();
            dots.set(2, 0, 0.0, policy);
            dots.set(3, 2, 10.0, policy);
            let looks = dots.looks(10.0);
            assert_eq!(looks.len(), 3);
            assert_eq!(looks[2].slot, 26.0);
            assert_eq!(looks[2].width, 26.0);
            assert!(!dots.prune(10.0));
            dots.set(2, 0, 20.0, policy);
            assert_eq!(dots.looks(20.0).len(), 2);
            assert_eq!(dots.looks(20.0)[0].slot, 29.0);
        }
    }

    #[test]
    fn held_fingers_move_the_pill_and_the_release_settles_on_its_time() {
        let mut dots = Dots::default();
        dots.set(3, 0, 0.0, full());
        // A quarter of the way to workspace 1: GNOME's expansion is
        // 1 - distance, so the first dot is 0.75 expanded, the second 0.25.
        dots.follow(0.25, 0.0, 1000.0, full());
        assert!(!dots.prune(1000.0));
        let held = dots.looks(1000.0);
        assert_eq!(held[0], look(0.75, 1.0, width_multiplier(3)));
        assert_eq!(held[1], look(0.25, 1.0, width_multiplier(3)));
        assert_eq!(held[2], look(0.0, 1.0, width_multiplier(3)));
        // Released: eased to workspace 1 over the swipe's 180 ms.
        dots.follow(1.0, 180.0, 1000.0, full());
        assert_eq!(dots.position(1090.0), 0.25 + 0.75 * 0.875);
        assert!(dots.prune(1179.0));
        assert_eq!(dots.position(1180.0), 1.0);
        // The model's switch arriving after the release keeps its timing.
        dots.set(3, 1, 1090.0, full());
        assert_eq!(dots.position(1180.0), 1.0);
        assert!(!dots.prune(1180.0));
    }

    #[test]
    fn a_switch_arriving_before_the_release_is_retimed_by_it() {
        let mut dots = Dots::default();
        dots.set(3, 0, 0.0, full());
        dots.follow(0.6, 0.0, 1000.0, full());
        // The model switched first: a 250 ms ease from the held 0.6...
        dots.set(3, 1, 1000.0, full());
        assert_eq!(dots.position(1000.0), 0.6);
        // ...then the release retimes it from where it is drawn.
        dots.follow(1.0, 120.0, 1000.0, full());
        assert_eq!(dots.position(1000.0), 0.6);
        assert_eq!(dots.position(1120.0), 1.0);
    }

    #[test]
    fn held_fingers_move_the_pill_under_every_policy_and_releases_snap_without_motion() {
        for level in [MotionLevel::FadeOnly, MotionLevel::Off] {
            let policy = MotionPolicy::new(level, 1.0);
            let mut dots = Dots::default();
            dots.set(2, 0, 0.0, policy);
            dots.follow(0.4, 0.0, 10.0, policy);
            assert_eq!(dots.position(10.0), 0.4);
            dots.follow(1.0, 200.0, 20.0, policy);
            assert_eq!(dots.position(20.0), 1.0);
            assert!(!dots.prune(20.0));
        }
    }
}
