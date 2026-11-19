//! The gestures that open gnome-shell 51's background menu
//! (`js/ui/backgroundMenu.js` `addBackgroundMenu`), made on the bare
//! wallpaper.
//!
//! GNOME adds two Clutter gestures to the wallpaper actor: a
//! `ClickGesture` on the secondary button that recognizes on press, and a
//! `LongPressGesture` on the primary button. `ClutterPressGesture` arms
//! its long press for the display's `long-press-duration` (500 ms, the
//! `ClutterSettings` default) and cancels it when the point moves past
//! its `cancel-threshold` (`DEFAULT_CANCEL_THRESHOLD`, 36 px) or the
//! button goes up. A second pointer button pressed meanwhile cancels the
//! gesture too (`clutter_press_gesture_point_began`: more than one active
//! point).
//!
//! This module is the gesture state only; the window manager feeds it
//! presses that landed on nothing and sends the shell a
//! [`tuna_shell_control::Message::BackgroundMenu`] for each recognized
//! gesture, which opens the menu (`crates/shell-gtk/src/background_menu.rs`).

use std::time::{Duration, Instant};

use smithay::utils::{Logical, Point};

/// `BTN_LEFT`, GNOME's `Clutter.BUTTON_PRIMARY`.
pub const BTN_LEFT: u32 = 0x110;
/// `BTN_RIGHT`, GNOME's `Clutter.BUTTON_SECONDARY`.
pub const BTN_RIGHT: u32 = 0x111;
/// `ClutterSettings:long-press-duration`.
pub const LONG_PRESS: Duration = Duration::from_millis(500);
/// `DEFAULT_CANCEL_THRESHOLD` of `ClutterPressGesture`, in pixels.
pub const CANCEL_THRESHOLD: f64 = 36.0;

/// A held primary press that becomes a long press at `due`.
#[derive(Debug, Clone, Copy)]
struct Pending {
    origin: Point<f64, Logical>,
    due: Instant,
}

/// The recognizer for one pointer.
#[derive(Debug, Default)]
pub struct Gestures {
    pending: Option<Pending>,
    /// Recognized gestures: where the menu opens, global logical pixels.
    recognized: Vec<Point<f64, Logical>>,
}

impl Gestures {
    /// A button went down on the bare wallpaper at `pos`. The secondary
    /// button recognizes at once; the primary one arms the long press;
    /// anything else cancels a long press in progress.
    pub fn press(&mut self, button: u32, pos: Point<f64, Logical>, now: Instant) {
        match button {
            BTN_RIGHT => {
                self.pending = None;
                self.recognized.push(pos);
            }
            BTN_LEFT => {
                self.pending = Some(Pending {
                    origin: pos,
                    due: now + LONG_PRESS,
                });
            }
            _ => self.pending = None,
        }
    }

    /// The pointer moved: a press dragged past the threshold is no long
    /// press.
    pub fn motion(&mut self, pos: Point<f64, Logical>) {
        if let Some(pending) = self.pending {
            let distance = (pos.x - pending.origin.x).hypot(pos.y - pending.origin.y);
            if distance > CANCEL_THRESHOLD {
                self.pending = None;
            }
        }
    }

    /// A button went up: releasing the primary one ends the long press.
    pub fn release(&mut self, button: u32) {
        if button == BTN_LEFT {
            self.pending = None;
        }
    }

    /// Something else took the press: a window, a layer surface, the
    /// overview, the lock.
    pub fn cancel(&mut self) {
        self.pending = None;
    }

    /// When the armed long press is due.
    pub fn deadline(&self) -> Option<Instant> {
        self.pending.map(|pending| pending.due)
    }

    /// Recognize the long press once it is due.
    pub fn tick(&mut self, now: Instant) {
        if let Some(pending) = self.pending {
            if now >= pending.due {
                self.pending = None;
                self.recognized.push(pending.origin);
            }
        }
    }

    /// Where the menu opens for each gesture recognized since the last
    /// call, in order.
    pub fn take(&mut self) -> Vec<Point<f64, Logical>> {
        std::mem::take(&mut self.recognized)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(x: f64, y: f64) -> Point<f64, Logical> {
        (x, y).into()
    }

    #[test]
    fn the_secondary_button_recognizes_on_press() {
        let mut gestures = Gestures::default();
        let now = Instant::now();
        gestures.press(BTN_RIGHT, at(10.0, 20.0), now);
        assert_eq!(gestures.take(), vec![at(10.0, 20.0)]);
        assert!(gestures.take().is_empty(), "taken once");
        assert_eq!(gestures.deadline(), None);
    }

    #[test]
    fn a_primary_press_held_500_ms_is_a_long_press_where_it_began() {
        let mut gestures = Gestures::default();
        let now = Instant::now();
        gestures.press(BTN_LEFT, at(100.0, 100.0), now);
        assert_eq!(gestures.deadline(), Some(now + LONG_PRESS));
        gestures.tick(now + LONG_PRESS - Duration::from_millis(1));
        assert!(gestures.take().is_empty(), "not yet");
        // A small wobble inside the threshold keeps it.
        gestures.motion(at(100.0 + CANCEL_THRESHOLD, 100.0));
        gestures.tick(now + LONG_PRESS);
        assert_eq!(gestures.take(), vec![at(100.0, 100.0)]);
        assert_eq!(gestures.deadline(), None);
    }

    #[test]
    fn releasing_dragging_or_another_button_cancels_the_long_press() {
        let now = Instant::now();
        let mut released = Gestures::default();
        released.press(BTN_LEFT, at(5.0, 5.0), now);
        released.release(BTN_LEFT);
        released.tick(now + LONG_PRESS);
        assert!(released.take().is_empty());

        let mut dragged = Gestures::default();
        dragged.press(BTN_LEFT, at(0.0, 0.0), now);
        dragged.motion(at(37.0, 0.0));
        dragged.tick(now + LONG_PRESS);
        assert!(dragged.take().is_empty());

        let mut other = Gestures::default();
        other.press(BTN_LEFT, at(0.0, 0.0), now);
        other.press(0x112, at(0.0, 0.0), now);
        other.tick(now + LONG_PRESS);
        assert!(other.take().is_empty());

        let mut taken = Gestures::default();
        taken.press(BTN_LEFT, at(0.0, 0.0), now);
        taken.cancel();
        taken.tick(now + LONG_PRESS);
        assert!(taken.take().is_empty());
    }

    #[test]
    fn the_secondary_press_during_a_long_press_opens_one_menu() {
        let mut gestures = Gestures::default();
        let now = Instant::now();
        gestures.press(BTN_LEFT, at(1.0, 1.0), now);
        gestures.press(BTN_RIGHT, at(2.0, 2.0), now);
        gestures.tick(now + LONG_PRESS);
        assert_eq!(gestures.take(), vec![at(2.0, 2.0)]);
    }
}
