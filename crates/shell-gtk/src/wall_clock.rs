//! Wall-clock wakeups for the panel and lock-screen clocks, as GNOME's
//! GnomeWallClock does them: a `CLOCK_REALTIME` timerfd armed for the
//! next minute (or second) boundary with `TFD_TIMER_CANCEL_ON_SET`. It
//! fires once per boundary, and at once when the wall clock is set or
//! jumps over a suspend (the kernel cancels such timers then), so the
//! clock never waits on a periodic poll.
//!
//! The local time zone follows GnomeWallClock too (gnome-desktop 51.0,
//! `libgnome-desktop/gnome-wall-clock.c` lines 77-85 and 449-486): a
//! file monitor on `/etc/localtime` re-reads the zone and ticks at once
//! when timedated's `SetTimezone` (Settings' Date & Time panel) replaces
//! it, instead of waiting out jiff's five-minute system-zone cache.

use std::cell::{Cell, RefCell};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;
use std::rc::Rc;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gtk4::gio;
use gtk4::gio::prelude::*;
use gtk4::glib;
use jiff::tz::TimeZone;

/// The file GnomeWallClock watches for zone changes.
const LOCALTIME: &str = "/etc/localtime";

/// The resolved local zone; `None` until first use and after
/// `/etc/localtime` changes.
static ZONE: Mutex<Option<TimeZone>> = Mutex::new(None);

/// The local time zone, current with `/etc/localtime`.
pub fn local_zone() -> TimeZone {
    let mut zone = ZONE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    zone.get_or_insert_with(|| resolve_zone(Path::new(LOCALTIME)))
        .clone()
}

/// Now, in [`local_zone`].
pub fn now() -> jiff::Zoned {
    jiff::Zoned::now().with_time_zone(local_zone())
}

fn forget_zone() {
    *ZONE.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
}

/// The IANA name a `/etc/localtime` symlink target points at
/// (`../usr/share/zoneinfo/Pacific/Auckland` is `Pacific/Auckland`).
fn zone_name(target: &Path) -> Option<&str> {
    let target = target.to_str()?;
    let (_, name) = target.rsplit_once("zoneinfo/")?;
    (!name.is_empty()).then_some(name)
}

/// The zone `localtime` describes, as GLib's `g_time_zone_new_local`:
/// `TZ` wins when set; then the zone the symlink names; then the TZif
/// data itself; jiff's own detection last.
fn resolve_zone(localtime: &Path) -> TimeZone {
    if std::env::var_os("TZ").is_some() {
        return TimeZone::system();
    }
    let named = std::fs::read_link(localtime)
        .ok()
        .and_then(|target| zone_name(&target).and_then(|name| TimeZone::get(name).ok()));
    named
        .or_else(|| {
            std::fs::read(localtime)
                .ok()
                .and_then(|data| TimeZone::tzif("Local", &data).ok())
        })
        .unwrap_or_else(TimeZone::system)
}

/// Remove a pending fallback timeout before re-arming outside it.
fn drop_timeout(inner: &Inner) {
    if inner.timer.is_none() {
        if let Some(id) = inner.source.take() {
            id.remove();
        }
    }
}

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
    /// Watches `/etc/localtime`; `None` where GIO cannot monitor it.
    zone_monitor: Option<gio::FileMonitor>,
    tick: Box<dyn Fn()>,
}

impl Inner {
    /// Arm the wakeup for the next boundary after now.
    fn arm(self: &Rc<Self>) {
        let step = if self.seconds.get() { 1 } else { 60 };
        let since_epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let offset = i64::from(now().offset().seconds());
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
        Self::watching(seconds, Path::new(LOCALTIME), tick)
    }

    /// [`WallClock::new`], with the zone file at `localtime`.
    fn watching(seconds: bool, localtime: &Path, tick: impl Fn() + 'static) -> Self {
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
        let zone_monitor = gio::File::for_path(localtime)
            .monitor_file(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE)
            .map_err(|e| eprintln!("tuna-shell-gtk: no time zone monitor: {e}"))
            .ok();
        let inner = Rc::new(Inner {
            timer,
            seconds: Cell::new(seconds),
            source: RefCell::new(None),
            zone_monitor,
            tick: Box::new(tick),
        });
        if let Some(monitor) = inner.zone_monitor.as_ref() {
            let weak = Rc::downgrade(&inner);
            monitor.connect_changed(move |_, _, _, _| {
                let Some(inner) = weak.upgrade() else { return };
                // GnomeWallClock's on_tz_changed: a new local zone, then
                // the clock redrawn in it.
                forget_zone();
                drop_timeout(&inner);
                inner.fire();
            });
        }
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
        drop_timeout(&self.0);
        self.0.arm();
    }
}

impl Drop for WallClock {
    fn drop(&mut self) {
        if let Some(monitor) = self.0.zone_monitor.as_ref() {
            monitor.cancel();
        }
        // Before the timerfd closes with `Inner`.
        if let Some(id) = self.0.source.take() {
            id.remove();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::path::Path;
    use std::rc::Rc;
    use std::time::{Duration, Instant};

    use gtk4::glib;

    use super::{next_boundary, resolve_zone, zone_name, WallClock};

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

    #[test]
    fn the_zone_name_is_the_path_below_zoneinfo() {
        fn name(target: &str) -> Option<&str> {
            zone_name(Path::new(target))
        }
        assert_eq!(
            name("../usr/share/zoneinfo/Pacific/Auckland"),
            Some("Pacific/Auckland")
        );
        assert_eq!(name("/usr/share/zoneinfo/UTC"), Some("UTC"));
        assert_eq!(
            name("/usr/lib/zoneinfo/America/New_York"),
            Some("America/New_York")
        );
        assert_eq!(name("/usr/share/zoneinfo/"), None);
        assert_eq!(name("/etc/timezone-file"), None);
    }

    /// Point `link` at a zone in the host's zoneinfo, as timedated's
    /// `SetTimezone` does: a new symlink renamed over the old one.
    fn point(link: &Path, zone: &str) {
        let next = link.with_extension("next");
        std::os::unix::fs::symlink(format!("/usr/share/zoneinfo/{zone}"), &next).unwrap();
        std::fs::rename(&next, link).unwrap();
    }

    fn zone_dir(test: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("tuna-wall-clock-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_zone_is_the_one_localtime_names_now() {
        let dir = zone_dir("resolve");
        let link = dir.join("localtime");
        point(&link, "Pacific/Auckland");
        assert_eq!(resolve_zone(&link).iana_name(), Some("Pacific/Auckland"));
        point(&link, "America/New_York");
        assert_eq!(resolve_zone(&link).iana_name(), Some("America/New_York"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_time_zone_change_redraws_the_clock_at_once() {
        let dir = zone_dir("monitor");
        let link = dir.join("localtime");
        point(&link, "Pacific/Auckland");
        // The timerfd watch lives on the default context, which this
        // thread must own (fd_watch::add_local); it is never iterated here.
        let main = glib::MainContext::default();
        let _owner = main.acquire().expect("default main context free");
        let ticks = Rc::new(Cell::new(0));
        let counted = ticks.clone();
        // A private context: only the zone monitor attaches here (the
        // timerfd watch is on the global default context), so a minute
        // boundary cannot stand in for the zone change.
        let context = glib::MainContext::new();
        context
            .with_thread_default(|| {
                let _clock =
                    WallClock::watching(false, &link, move || counted.set(counted.get() + 1));
                let pump = |limit: Duration, done: &dyn Fn() -> bool| {
                    let deadline = Instant::now() + limit;
                    while Instant::now() < deadline && !done() {
                        context.iteration(false);
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    done()
                };
                pump(Duration::from_millis(300), &|| false);
                assert_eq!(ticks.get(), 0, "no tick before the zone changes");
                point(&link, "America/New_York");
                assert!(
                    pump(Duration::from_secs(5), &|| ticks.get() > 0),
                    "the clock ticks when /etc/localtime changes"
                );
            })
            .unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
