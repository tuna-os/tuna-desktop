//! IM-grab repeats mirror Mutter's native-seat clock, not OSK surrounding
//! deletion. Synthetic repeats are offered to IBus only: declined repeats must
//! not be forwarded as extra presses to a wl_keyboard client doing its own repeat.
use super::{IbusContext, CALL_TIMEOUT_MS, CONTEXT, IBUS};
use gio::{glib, prelude::*};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::{Duration, Instant},
};

struct Held {
    key: u32,
    at: Instant,
    pending: bool,
    cancel: gio::Cancellable,
}

#[derive(Default)]
pub(super) struct KeyRepeat {
    interval: Option<Duration>,
    delay: Duration,
    held: Option<Held>,
    epoch: Rc<Cell<u64>>,
    reply: Rc<RefCell<Option<Result<(), glib::Error>>>>,
}

impl KeyRepeat {
    pub fn info(&mut self, rate: i32, delay: i32) {
        // The protocol forbids negative rate/delay; zero disables repeating.
        assert!(rate >= 0 && delay >= 0, "invalid IM-grab repeat_info");
        self.interval = (rate > 0).then(|| Duration::from_secs_f64(1.0 / f64::from(rate)));
        self.delay = Duration::from_millis(delay as u64);
        if self.interval.is_none() {
            self.cancel();
        }
    }

    pub fn key(&mut self, key: u32, pressed: bool, repeatable: bool, active: bool, now: Instant) {
        if pressed && repeatable {
            self.cancel();
            if active && self.interval.is_some() {
                self.held = Some(Held {
                    key,
                    at: now + self.delay,
                    pending: false,
                    cancel: gio::Cancellable::new(),
                });
            }
        } else if !pressed && self.held.as_ref().is_some_and(|held| held.key == key) {
            self.cancel();
        }
    }

    pub fn cancel(&mut self) {
        self.epoch.set(self.epoch.get().wrapping_add(1));
        if let Some(held) = self.held.take() {
            held.cancel.cancel();
        }
        self.reply.borrow_mut().take();
    }

    pub fn deadline(&self, now: Instant) -> Option<Instant> {
        self.held.as_ref().and_then(|held| {
            // A slow engine must not cause a busy loop or concurrent requests.
            (!held.pending || self.reply.borrow().is_some() || held.at > now).then_some(held.at)
        })
    }

    pub fn due(&mut self, now: Instant) -> Option<u32> {
        let reply = self.reply.borrow_mut().take();
        if let Some(result) = reply {
            if let Some(held) = self.held.as_mut() {
                held.pending = false;
            }
            if let Err(error) = result {
                eprintln!("tuna-ibus-bridge: repeat ProcessKeyEvent: {error}");
                self.cancel();
            }
        }
        self.held
            .as_ref()
            .filter(|held| !held.pending && held.at <= now)
            .map(|held| held.key)
    }

    pub fn request(&mut self, context: &IbusContext, keyval: u32, modifiers: u32, now: Instant) {
        let Some(held) = self.held.as_mut() else {
            return;
        };
        let Some(interval) = self.interval else {
            return;
        };
        held.pending = true;
        held.at = now + interval;
        let (epoch, generation, reply) = (self.epoch.clone(), self.epoch.get(), self.reply.clone());
        context.bus.call(
            Some(IBUS),
            &context.path,
            CONTEXT,
            "ProcessKeyEvent",
            Some(&(keyval, held.key, modifiers).to_variant()),
            glib::VariantTy::new("(b)").ok(),
            gio::DBusCallFlags::NONE,
            CALL_TIMEOUT_MS,
            Some(&held.cancel),
            move |result| {
                if epoch.get() == generation {
                    // Consumed or declined, a synthetic repeat never forwards a
                    // second native press. Mutter drops these for wl_keyboard <10.
                    *reply.borrow_mut() = Some(result.map(|_| ()));
                }
            },
        );
    }
}

impl Drop for KeyRepeat {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advertised_clock_excludes_modifiers_and_stops_on_release_or_disable() {
        let now = Instant::now();
        let mut repeat = KeyRepeat::default();
        repeat.info(20, 200);
        repeat.key(42, true, false, true, now);
        assert_eq!(repeat.due(now + Duration::from_secs(1)), None);
        repeat.key(14, true, true, true, now);
        assert_eq!(repeat.due(now + Duration::from_millis(199)), None);
        assert_eq!(repeat.due(now + Duration::from_millis(200)), Some(14));
        repeat.key(42, false, false, true, now);
        assert_eq!(repeat.due(now + Duration::from_millis(200)), Some(14));
        repeat.key(14, false, true, true, now);
        assert_eq!(repeat.due(now + Duration::from_secs(1)), None);
        repeat.key(14, true, true, true, now);
        repeat.info(0, 200);
        assert_eq!(repeat.due(now + Duration::from_secs(1)), None);
        repeat.info(20, 200);
        assert_eq!(
            repeat.due(now + Duration::from_secs(1)),
            None,
            "enabling is not a new key press"
        );
        repeat.key(14, true, true, false, now);
        assert_eq!(
            repeat.due(now + Duration::from_secs(1)),
            None,
            "inactive contexts never repeat"
        );
    }
}
