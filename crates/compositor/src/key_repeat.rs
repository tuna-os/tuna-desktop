//! Compositor key repeat for wl_keyboard version 10 (wl_seat 10), as
//! Mutter 51's native seat generates it (`meta_seat_impl_notify_key_in_impl`
//! and `keyboard_repeat` in meta-seat-impl.c): after `delay` the most
//! recently pressed repeating key repeats every `repeat-interval` until it
//! is released or another repeating key is pressed. A key the keymap does
//! not repeat (a modifier) leaves the running repeat alone, and nothing
//! repeats when `repeat` is off. The settings are
//! org.gnome.desktop.peripherals.keyboard `repeat`, `delay` and
//! `repeat-interval`.

use std::time::{Duration, Instant};

/// The repeat timer: one held key at most.
#[derive(Debug, Clone)]
pub(crate) struct KeyRepeat {
    enabled: bool,
    delay: Duration,
    interval: Duration,
    held: Option<Held>,
}

#[derive(Debug, Clone, Copy)]
struct Held {
    keycode: u32,
    /// The press's event time and when it was handled: repeats carry
    /// event times in the same base.
    time: u32,
    at: Instant,
    due: Instant,
}

impl KeyRepeat {
    pub(crate) fn new(enabled: bool, delay_ms: u32, interval_ms: u32) -> Self {
        let mut repeat = Self {
            enabled: false,
            delay: Duration::ZERO,
            interval: Duration::ZERO,
            held: None,
        };
        repeat.configure(enabled, delay_ms, interval_ms);
        repeat
    }

    /// New settings apply from the next repeat. A zero interval repeats
    /// nothing, as clients before version 10 are told rate 0 for it.
    pub(crate) fn configure(&mut self, enabled: bool, delay_ms: u32, interval_ms: u32) {
        self.enabled = enabled && interval_ms > 0;
        self.delay = Duration::from_millis(delay_ms.into());
        self.interval = Duration::from_millis(interval_ms.into());
        if !self.enabled {
            self.held = None;
        }
    }

    /// A physical press reached the seat. `repeats` is the keymap's
    /// `xkb_keymap_key_repeats` for the key.
    pub(crate) fn press(&mut self, keycode: u32, repeats: bool, time: u32, now: Instant) {
        if !self.enabled {
            self.held = None;
            return;
        }
        if !repeats {
            return;
        }
        self.held = Some(Held {
            keycode,
            time,
            at: now,
            due: now + self.delay,
        });
    }

    /// A physical release: only the repeating key's stops its repeat.
    pub(crate) fn release(&mut self, keycode: u32) {
        if self.held.is_some_and(|held| held.keycode == keycode) {
            self.held = None;
        }
    }

    pub(crate) fn cancel(&mut self) {
        self.held = None;
    }

    /// When the next repeat is due.
    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.held.map(|held| held.due)
    }

    /// The repeat due at `now`, as `(keycode, event time)`. One repeat per
    /// call: a late loop never bursts, it resumes at the interval.
    pub(crate) fn take_due(&mut self, now: Instant) -> Option<(u32, u32)> {
        let interval = self.interval;
        let held = self.held.as_mut().filter(|held| now >= held.due)?;
        let elapsed = held.due.saturating_duration_since(held.at).as_millis();
        let time = held.time.wrapping_add(elapsed as u32);
        let next = held.due + interval;
        held.due = if next > now { next } else { now + interval };
        Some((held.keycode, time))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: u32 = 30;
    const B: u32 = 48;
    const SHIFT: u32 = 42;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn a_held_key_repeats_after_the_delay_at_the_interval() {
        let start = Instant::now();
        let mut repeat = KeyRepeat::new(true, 500, 30);
        repeat.press(A, true, 1000, start);
        assert_eq!(repeat.deadline(), Some(start + ms(500)));
        assert_eq!(repeat.take_due(start + ms(499)), None);
        assert_eq!(repeat.take_due(start + ms(500)), Some((A, 1500)));
        assert_eq!(repeat.take_due(start + ms(500)), None);
        assert_eq!(repeat.deadline(), Some(start + ms(530)));
        // A slightly late tick keeps the cadence.
        assert_eq!(repeat.take_due(start + ms(540)), Some((A, 1530)));
        assert_eq!(repeat.deadline(), Some(start + ms(560)));
        // A loop late by many intervals repeats once, then resumes.
        assert_eq!(repeat.take_due(start + ms(700)), Some((A, 1560)));
        assert_eq!(repeat.deadline(), Some(start + ms(730)));
        repeat.release(A);
        assert_eq!(repeat.deadline(), None);
    }

    #[test]
    fn another_repeating_key_takes_over_and_a_modifier_does_not() {
        let start = Instant::now();
        let mut repeat = KeyRepeat::new(true, 500, 30);
        repeat.press(A, true, 0, start);
        repeat.press(SHIFT, false, 100, start + ms(100));
        assert_eq!(repeat.deadline(), Some(start + ms(500)));
        repeat.release(SHIFT);
        assert_eq!(repeat.deadline(), Some(start + ms(500)));
        repeat.press(B, true, 200, start + ms(200));
        assert_eq!(repeat.deadline(), Some(start + ms(700)));
        // A's release no longer stops B.
        repeat.release(A);
        assert_eq!(repeat.take_due(start + ms(700)), Some((B, 700)));
    }

    #[test]
    fn disabled_or_zero_interval_repeat_never_arms() {
        let start = Instant::now();
        let mut repeat = KeyRepeat::new(false, 500, 30);
        repeat.press(A, true, 0, start);
        assert_eq!(repeat.deadline(), None);
        let mut repeat = KeyRepeat::new(true, 500, 0);
        repeat.press(A, true, 0, start);
        assert_eq!(repeat.deadline(), None);
        let mut repeat = KeyRepeat::new(true, 500, 30);
        repeat.press(A, true, 0, start);
        repeat.configure(false, 500, 30);
        assert_eq!(repeat.deadline(), None);
    }
}
