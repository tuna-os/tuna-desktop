//! GNOME 51's `wiggle` (js/misc/animationUtils.js,
//! <https://gitlab.gnome.org/GNOME/gnome-shell/-/blob/51.0/js/misc/animationUtils.js>)
//! for the lock prompt: a bin that shakes its child sideways without
//! moving anything around it, as Clutter's `translation_x` does.
//!
//! GNOME eases the actor to -6 px over 65 ms (ease-out-quad), runs a
//! linear -6 → +6 px ease repeated three times with auto-reverse (four
//! runs), and eases back to 0 (ease-in-quad), each step 65 ms scaled by
//! the slow-down factor. It does nothing when animations are off.

use std::cell::{Cell, RefCell};

use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use gtk4::subclass::prelude::*;

/// animationUtils.js `WIGGLE_OFFSET`.
pub const WIGGLE_OFFSET: f64 = 6.0;
/// animationUtils.js `WIGGLE_DURATION`.
pub const WIGGLE_DURATION_MS: f64 = 65.0;
/// animationUtils.js `N_WIGGLES` (Clutter's repeat count).
pub const N_WIGGLES: u32 = 3;

/// The linear runs: the first plus `N_WIGGLES` repeats.
const RUNS: u32 = N_WIGGLES + 1;

/// The whole wiggle for one step length: accelerate, the runs, return.
pub fn total_ms(step_ms: f64) -> f64 {
    step_ms * f64::from(RUNS + 2)
}

/// The child's offset `elapsed_ms` into a wiggle of `step_ms` steps, or
/// `None` once it is over.
pub fn offset_at(elapsed_ms: f64, step_ms: f64) -> Option<f64> {
    if step_ms <= 0.0 || elapsed_ms >= total_ms(step_ms) {
        return None;
    }
    let elapsed = elapsed_ms.max(0.0);
    if elapsed < step_ms {
        return Some(-WIGGLE_OFFSET * crate::transient::ease_out_quad(elapsed / step_ms));
    }
    let wiggling = elapsed - step_ms;
    let runs = f64::from(RUNS);
    if wiggling < step_ms * runs {
        let run = (wiggling / step_ms).floor();
        let t = wiggling / step_ms - run;
        // Auto-reverse: even runs go -6 → +6, odd ones back.
        let (from, to) = if (run as u32).is_multiple_of(2) {
            (-WIGGLE_OFFSET, WIGGLE_OFFSET)
        } else {
            (WIGGLE_OFFSET, -WIGGLE_OFFSET)
        };
        return Some(from + (to - from) * t);
    }
    let end = if RUNS.is_multiple_of(2) {
        -WIGGLE_OFFSET
    } else {
        WIGGLE_OFFSET
    };
    let t = (wiggling - step_ms * runs) / step_ms;
    Some(end * (1.0 - t * t))
}

/// One step's length under the motion policy, `None` when GNOME would
/// not wiggle (`!St.Settings.enable_animations`).
fn step_ms() -> Option<f64> {
    let policy = crate::motion::current();
    let step = policy.adjust_ms(WIGGLE_DURATION_MS);
    (policy.animations_enabled() && step > 0.0).then_some(step)
}

/// How long a wiggle started now runs, in whole milliseconds.
pub fn duration_ms() -> u64 {
    step_ms().map_or(0, |step| total_ms(step).round() as u64)
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Wiggle {
        pub offset: Cell<f64>,
        pub tick: RefCell<Option<gtk::TickCallbackId>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Wiggle {
        const NAME: &'static str = "TunaLockWiggle";
        type Type = super::Wiggle;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for Wiggle {
        fn dispose(&self) {
            if let Some(tick) = self.tick.take() {
                tick.remove();
            }
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for Wiggle {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            self.obj()
                .first_child()
                .map_or(gtk::SizeRequestMode::ConstantSize, |c| c.request_mode())
        }

        fn compute_expand(&self, hexpand: &mut bool, vexpand: &mut bool) {
            if let Some(child) = self.obj().first_child() {
                *hexpand = child.compute_expand(gtk::Orientation::Horizontal);
                *vexpand = child.compute_expand(gtk::Orientation::Vertical);
            }
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            self.obj()
                .first_child()
                .filter(|c| c.should_layout())
                .map_or((0, 0, -1, -1), |c| c.measure(orientation, for_size))
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            let Some(child) = self.obj().first_child().filter(|c| c.should_layout()) else {
                return;
            };
            let offset = self.offset.get() as f32;
            let transform = (offset != 0.0).then(|| {
                gtk::gsk::Transform::new().translate(&gtk::graphene::Point::new(offset, 0.0))
            });
            child.allocate(width, height, baseline, transform);
        }
    }
}

glib::wrapper! {
    /// A bin whose child can wiggle (GNOME's `wiggle`).
    pub struct Wiggle(ObjectSubclass<imp::Wiggle>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Wiggle {
    pub fn new(child: &impl IsA<gtk::Widget>) -> Self {
        let wiggle: Self = glib::Object::new();
        child.set_parent(&wiggle);
        wiggle
    }

    /// Shake the child once, from the start (a running wiggle restarts,
    /// as GNOME resets `translation_x` first).
    pub fn start(&self) {
        let imp = self.imp();
        if let Some(tick) = imp.tick.take() {
            tick.remove();
        }
        imp.offset.set(0.0);
        self.queue_allocate();
        let Some(step) = step_ms() else {
            return;
        };
        let start = Cell::new(None::<i64>);
        let id = self.add_tick_callback(move |wiggle, clock| {
            let now = clock.frame_time();
            let begun = start.get().unwrap_or(now);
            start.set(Some(begun));
            let elapsed = (now - begun) as f64 / 1000.0;
            let imp = wiggle.imp();
            let offset = offset_at(elapsed, step);
            imp.offset.set(offset.unwrap_or(0.0));
            wiggle.queue_allocate();
            if offset.is_some() {
                glib::ControlFlow::Continue
            } else {
                // Returning Break removes the callback itself.
                drop(imp.tick.take());
                glib::ControlFlow::Break
            }
        });
        *imp.tick.borrow_mut() = Some(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn wiggle_is_six_65_ms_steps_like_animation_utils() {
        assert!(near(total_ms(65.0), 390.0));
        assert!(near(offset_at(0.0, 65.0).unwrap(), 0.0));
        // Ease-out-quad to -6 over the first step.
        assert!(near(offset_at(32.5, 65.0).unwrap(), -6.0 * 0.75));
        assert!(near(offset_at(65.0, 65.0).unwrap(), -6.0));
        // Four linear runs between -6 and +6, alternating.
        assert!(near(offset_at(97.5, 65.0).unwrap(), 0.0));
        assert!(near(offset_at(129.999_999_999, 65.0).unwrap(), 6.0));
        assert!(near(offset_at(195.0, 65.0).unwrap(), -6.0));
        assert!(near(offset_at(260.0, 65.0).unwrap(), 6.0));
        assert!(near(offset_at(292.5, 65.0).unwrap(), 0.0));
        // Ends at -6, then ease-in-quad back to 0.
        assert!(near(offset_at(325.0, 65.0).unwrap(), -6.0));
        assert!(near(offset_at(357.5, 65.0).unwrap(), -6.0 * 0.75));
        assert_eq!(offset_at(390.0, 65.0), None);
        assert_eq!(offset_at(10.0, 0.0), None);
        // The slow-down factor stretches every step.
        assert!(near(offset_at(130.0, 130.0).unwrap(), -6.0));
    }
}
