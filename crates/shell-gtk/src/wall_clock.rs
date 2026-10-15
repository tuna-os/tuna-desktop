//! Wall-clock wakeups for the panel and lock-screen clocks, as GNOME's
//! GnomeWallClock does them: a `CLOCK_REALTIME` timerfd armed for the
//! next minute (or second) boundary with `TFD_TIMER_CANCEL_ON_SET`. It
//! fires once per boundary, and at once when the wall clock is set or
//! jumps over a suspend (the kernel cancels such timers then), so the
//! clock never waits on a periodic poll.

use std::cell::{Cell, RefCell};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gtk4::glib;

/// The first `step`-second boundary of local time after `now` (Unix
/// seconds), for a zone `offset` seconds east of UTC: the next minute
/// with `step` 60 (on the local minute even in zones offset by a
/// fraction of an hour), the next second with 1.
pub fn next_boundary(now: i64, offset: i64, step: i64) -> i64 {
    let local = now + offset;
    local - local.rem_euclid(step) + step - offset
}

struct Inner {
    /// The timerfd; `None` where the kernel refused one, and the clock
    /// falls back to one-shot main-loop timeouts.
    timer: Option<OwnedFd>,
    seconds: Cell<bool>,
    /// The timerfd's watch, or the pending fallback timeout.
    source: RefCell<Option<glib::SourceId>>,
    tick: Box<dyn Fn()>,
}

impl Inner {
    /// Arm the wakeup for the next boundary after now.
    fn arm(self: &Rc<Self>) {
        let step = if self.seconds.get() { 1 } else { 60 };
        let since_epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let offset = i64::from(jiff::Zoned::now().offset().seconds());
        let at = next_boundary(since_epoch.as_secs() as i64, offset, step);
        if let Some(timer) = self.timer.as_ref() {
            let spec = libc::itimerspec {
                it_interval: libc::timespec {
                    tv_sec: 0,
                    tv_nsec: 0,
                },
                it_value: libc::timespec {
                    tv_sec: at as libc::time_t,
                    tv_nsec: 0,
                },
            };
            // SAFETY: a valid timerfd and itimerspec; no old value asked.
            let set = unsafe {
                libc::timerfd_settime(
                    timer.as_raw_fd(),
                    libc::TFD_TIMER_ABSTIME | libc::TFD_TIMER_CANCEL_ON_SET,
                    &spec,
                    std::ptr::null_mut(),
                )
            };
            if set != 0 {
                eprintln!(
                    "tuna-shell-gtk: clock timer: {}",
                    std::io::Error::last_os_error()
                );
            }
            return;
        }
        let delay = Duration::from_secs(at.max(0) as u64).saturating_sub(since_epoch);
        // Whole milliseconds, rounded up: never wake before the boundary.
        let delay = Duration::from_millis(delay.as_micros().div_ceil(1000) as u64);
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(delay, move || {
            if let Some(inner) = weak.upgrade() {
                inner.source.take();
                inner.fire();
            }
        });
        *self.source.borrow_mut() = Some(id);
    }

    fn fire(self: &Rc<Self>) {
        self.arm();
        (self.tick)();
    }
}

/// Calls its tick on every local minute (or second) boundary and after
/// any wall-clock change, until dropped.
pub struct WallClock(Rc<Inner>);

impl WallClock {
    /// Start ticking on minutes, or on seconds when `seconds` is set.
    /// The first tick comes at the next boundary: draw the current time
    /// yourself first.
    pub fn new(seconds: bool, tick: impl Fn() + 'static) -> Self {
        // SAFETY: plain syscall; the fd is owned from here on.
        let fd = unsafe {
            libc::timerfd_create(libc::CLOCK_REALTIME, libc::TFD_NONBLOCK | libc::TFD_CLOEXEC)
        };
        let timer = if fd >= 0 {
            // SAFETY: a fresh descriptor nothing else owns.
            Some(unsafe { OwnedFd::from_raw_fd(fd) })
        } else {
            eprintln!(
                "tuna-shell-gtk: no clock timerfd ({}), using timeouts",
                std::io::Error::last_os_error()
            );
            None
        };
        let inner = Rc::new(Inner {
            timer,
            seconds: Cell::new(seconds),
            source: RefCell::new(None),
            tick: Box::new(tick),
        });
        if let Some(timer) = inner.timer.as_ref() {
            let weak = Rc::downgrade(&inner);
            let id = crate::fd_watch::add_local(
                timer.as_raw_fd(),
                glib::IOCondition::IN,
                move |fd, _| {
                    let Some(inner) = weak.upgrade() else {
                        return glib::ControlFlow::Break;
                    };
                    // The expiry count, or ECANCELED after a clock change:
                    // either way the clock re-reads the time and re-arms.
                    let mut expirations = 0u64;
                    // SAFETY: reads 8 bytes into a u64 from our timerfd.
                    unsafe {
                        libc::read(
                            fd,
                            (&mut expirations as *mut u64).cast(),
                            std::mem::size_of::<u64>(),
                        );
                    }
                    inner.fire();
                    glib::ControlFlow::Continue
                },
            );
            *inner.source.borrow_mut() = Some(id);
        }
        inner.arm();
        Self(inner)
    }

    /// Tick on seconds (GNOME's `clock-show-seconds`) or minutes.
    pub fn set_seconds(&self, seconds: bool) {
        if self.0.seconds.replace(seconds) == seconds {
            return;
        }
        if self.0.timer.is_none() {
            if let Some(id) = self.0.source.take() {
                id.remove();
            }
        }
        self.0.arm();
    }
}

impl Drop for WallClock {
    fn drop(&mut self) {
        // Before the timerfd closes with `Inner`.
        if let Some(id) = self.0.source.take() {
            id.remove();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::next_boundary;

    #[test]
    fn the_next_boundary_is_the_next_local_minute_or_second() {
        // 56 seconds past a UTC minute.
        let now = 1_792_067_696;
        assert_eq!(now % 60, 56);
        assert_eq!(next_boundary(now, 0, 60), now + 4);
        assert_eq!(next_boundary(now, 0, 1), now + 1);
        // On the boundary itself: the next one, never the same instant.
        assert_eq!(next_boundary(now + 4, 0, 60), now + 64);
        assert_eq!(next_boundary(now + 4, 0, 1), now + 5);
        // Whole-hour and fractional zones keep minutes on the minute
        // (UTC+2, India's UTC+5:30, Nepal's UTC+5:45, Newfoundland's
        // UTC-3:30).
        for offset in [7200, 19_800, 20_700, -12_600] {
            let next = next_boundary(now, offset, 60);
            assert_eq!((next + offset) % 60, 0);
            assert!(next > now && next <= now + 60);
        }
        // A zone with seconds in its offset (historic local mean time).
        let next = next_boundary(now, 3_208, 60);
        assert_eq!((next + 3_208).rem_euclid(60), 0);
        assert!(next > now && next <= now + 60);
        // Before the epoch.
        assert_eq!(next_boundary(-1, 0, 60), 0);
        assert_eq!(next_boundary(-61, 0, 60), -60);
    }
}
