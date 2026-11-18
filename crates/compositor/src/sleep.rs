//! Trusted logind sleep notifications, consumed on the compositor thread,
//! and the sleep delay inhibitor GNOME's screen shield holds.
//!
//! The VT can remain active throughout S3: libseat activation alone does not
//! notify us that the kernel discarded scanout. A retained system-bus proxy
//! binds signals to logind's actual owner. Two flags bound work and storage.
//!
//! As `screenShield.js` `_syncInhibitor` does, the session holds a logind
//! `delay` inhibitor on `sleep` while it is active and the lock screen is
//! allowed. On `PrepareForSleep(true)` the compositor locks and keeps the
//! inhibitor until every lit output has scanned out a frame drawn locked
//! ([`Delay`], [`Scanout`]), so the frame left on screen through sleep and
//! at resume is the lock. logind waits at most `InhibitDelayMaxUSec`; when
//! it gives up first, the session is locked all the same.

use futures_lite::StreamExt;
use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

/// The inhibitor's `Who` and `Why`, as `loginManager.js` `inhibit` names
/// GNOME Shell's ("GNOME needs to lock the screen").
pub const INHIBITOR_WHO: &str = "Tuna Desktop";
const INHIBITOR_WHY: &str = "Tuna Desktop needs to lock the screen";

#[derive(Default)]
struct Pending {
    lock: AtomicBool,
    wake: AtomicBool,
    /// `PrepareForSleep(true)` signals so far.
    sleeps: AtomicU64,
    /// The latest `PrepareForSleep` said the system is going to sleep.
    sleeping: AtomicBool,
}

/// Whether the compositor wants the delay inhibitor, and its descriptor.
/// Closing the descriptor is what releases it.
#[derive(Default)]
struct Inhibitor {
    wanted: bool,
    fd: Option<OwnedFd>,
}

pub struct Monitor {
    wake: crate::wake::Wake,
    connection: zbus::Connection,
    pending: Arc<Pending>,
    inhibitor: Arc<Mutex<Inhibitor>>,
    /// Asks the inhibit worker to take a delay inhibitor.
    acquire: Option<mpsc::Sender<()>>,
    workers: Vec<std::thread::JoinHandle<()>>,
}

impl Monitor {
    /// Each notification wakes the compositor loop through the
    /// [`Wake`](crate::wake::Wake) connected by [`set_wake`](Self::set_wake).
    pub fn start(generation: Arc<AtomicU64>) -> zbus::Result<Self> {
        let connection = zbus::block_on(
            zbus::connection::Builder::system()
                .method_timeout(Duration::from_secs(5))
                .build(),
        )?;
        Self::from_connection(connection, generation)
    }

    fn from_connection(
        connection: zbus::Connection,
        generation: Arc<AtomicU64>,
    ) -> zbus::Result<Self> {
        let (proxy, mut signals) = zbus::block_on(async {
            let proxy = zbus::Proxy::new(
                &connection,
                "org.freedesktop.login1",
                "/org/freedesktop/login1",
                "org.freedesktop.login1.Manager",
            )
            .await?;
            let signals = proxy.receive_signal("PrepareForSleep").await?;
            zbus::Result::Ok((proxy, signals))
        })?;
        let wake = crate::wake::Wake::default();
        let worker_wake = wake.clone();
        let pending = Arc::new(Pending::default());
        let worker_pending = pending.clone();
        let mut workers = Vec::with_capacity(2);
        workers.push(
            std::thread::Builder::new()
                .name("tuna-logind-sleep".into())
                .spawn(move || {
                    let _completion = worker_wake.completion();
                    let mut prepared = false;
                    zbus::block_on(async {
                        while let Some(signal) = signals.next().await {
                            let Ok((sleeping,)) = signal.body().deserialize::<(bool,)>() else {
                                continue;
                            };
                            if sleeping {
                                // Invalidate old PAM results on the signal thread,
                                // before the compositor can dequeue their callbacks.
                                crate::auth_owner::advance_epoch(&generation);
                                prepared = true;
                                worker_pending.sleeping.store(true, Ordering::Release);
                                worker_pending.sleeps.fetch_add(1, Ordering::AcqRel);
                                worker_pending.lock.store(true, Ordering::Release);
                                worker_wake.signal();
                            } else if prepared {
                                prepared = false;
                                worker_pending.sleeping.store(false, Ordering::Release);
                                worker_pending.wake.store(true, Ordering::Release);
                                worker_wake.signal();
                            }
                        }
                    });
                })?,
        );
        // Inhibit is a bus round trip: it stays off the compositor thread,
        // as `loginManager.js` takes it asynchronously. Releasing is a close.
        let inhibitor = Arc::new(Mutex::new(Inhibitor::default()));
        let (acquire, requests) = mpsc::channel::<()>();
        let worker_inhibitor = inhibitor.clone();
        let worker_wake = wake.clone();
        workers.push(
            std::thread::Builder::new()
                .name("tuna-logind-inhibit".into())
                .spawn(move || {
                    let _completion = worker_wake.completion();
                    let slot = || {
                        worker_inhibitor
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                    };
                    for () in requests {
                        {
                            let slot = slot();
                            if !slot.wanted || slot.fd.is_some() {
                                continue;
                            }
                        }
                        let taken = zbus::block_on(proxy.call::<_, _, zbus::OwnedFd>(
                            "Inhibit",
                            &("sleep", INHIBITOR_WHO, INHIBITOR_WHY, "delay"),
                        ));
                        match taken {
                            Ok(fd) => {
                                let mut slot = slot();
                                // Unwanted meanwhile: dropping it releases it.
                                if slot.wanted && slot.fd.is_none() {
                                    slot.fd = Some(fd.into());
                                    drop(slot);
                                    worker_wake.signal();
                                }
                            }
                            Err(error) => eprintln!(
                                "tuna-compositor: logind sleep delay inhibitor unavailable: {error}"
                            ),
                        }
                    }
                })?,
        );
        Ok(Self {
            wake,
            connection,
            pending,
            inhibitor,
            acquire: Some(acquire),
            workers,
        })
    }

    /// Coalesce notifications; the caller locks before processing a wake.
    pub(crate) fn set_wake(&self, wake: &crate::wake::Wake) {
        self.wake.connect(wake);
    }
    pub fn take(&self) -> (bool, bool) {
        (
            self.pending.lock.swap(false, Ordering::AcqRel),
            self.pending.wake.swap(false, Ordering::AcqRel),
        )
    }

    /// Sleeps logind has announced so far, and whether one is under way.
    fn sleep_state(&self) -> (u64, bool) {
        let sleeps = self.pending.sleeps.load(Ordering::Acquire);
        (sleeps, self.pending.sleeping.load(Ordering::Acquire))
    }

    fn slot(&self) -> MutexGuard<'_, Inhibitor> {
        self.inhibitor
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether this session holds its delay inhibitor now.
    pub fn held(&self) -> bool {
        self.slot().fd.is_some()
    }

    /// Want the delay inhibitor or not: wanting asks the worker to take
    /// one; not wanting closes the one held. Returns whether this call
    /// released a held inhibitor.
    fn hold(&self, wanted: bool) -> bool {
        let mut slot = self.slot();
        if slot.wanted == wanted {
            return false;
        }
        slot.wanted = wanted;
        if wanted {
            drop(slot);
            if let Some(acquire) = &self.acquire {
                let _ = acquire.send(());
            }
            false
        } else {
            slot.fd.take().is_some()
        }
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        // Closing also ends the retained signal stream/worker and fails an
        // Inhibit in flight; no orphaned subscription survives a runtime
        // returning in this process. The inhibitor closes with the slot.
        let _ = zbus::block_on(self.connection.clone().close());
        self.acquire = None;
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

/// What one output scans out, for the sleep delay: whether the frame on
/// screen, and the one in flight, were drawn locked.
#[derive(Clone, Copy, Debug, Default)]
pub struct Scanout {
    in_flight: Option<bool>,
    shown: bool,
}

impl Scanout {
    /// A frame drawn `locked` was queued for scanout.
    pub fn queued(&mut self, locked: bool) {
        self.in_flight = Some(locked);
    }
    /// Its page flip completed: that frame is on screen.
    pub fn flipped(&mut self) {
        if let Some(locked) = self.in_flight.take() {
            self.shown = locked;
        }
    }
    /// Nothing this compositor drew is known to be on screen (another VT
    /// owns it, scanout was reset or the CRTC turned off).
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    /// The screen shows a locked frame, and no unlocked one is about to
    /// replace it.
    pub fn shows_locked(&self) -> bool {
        self.shown && self.in_flight != Some(false)
    }
}

/// The compositor's side of the sleep delay: when to hold the inhibitor,
/// and a record of each release for the VM lane (`V-SUSPEND-UNLOCKED`).
#[derive(Debug, Default)]
pub struct Delay {
    /// Sleeps seen through [`Monitor::sleep_state`].
    seen: u64,
    /// A sleep is under way.
    preparing: bool,
    /// The inhibitor is given up for this sleep.
    released: bool,
    record: Record,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct Record {
    /// The inhibitor is held now.
    pub held: bool,
    /// `PrepareForSleep(true)` signals seen.
    pub prepared: u64,
    /// Released during a sleep once every lit output showed a locked frame.
    pub released_locked: u64,
    /// Released during a sleep without that: locking is disabled or the
    /// session went inactive.
    pub released_unlocked: u64,
    /// Still held when the sleep ended: logind's `InhibitDelayMaxUSec`
    /// ran out before a locked frame showed.
    pub overrun: u64,
}

impl Delay {
    /// Bring the inhibitor in line with the session: held while it is
    /// `active` and `lock_allowed` (GNOME's `_syncInhibitor`), given up
    /// during a sleep once `locked_shown`, and taken again after it.
    pub fn sync(
        &mut self,
        monitor: &Monitor,
        active: bool,
        lock_allowed: bool,
        locked_shown: bool,
    ) {
        let (sleeps, sleeping) = monitor.sleep_state();
        if self.preparing && !self.released && monitor.held() && (sleeps != self.seen || !sleeping)
        {
            self.record.overrun += 1;
        }
        if sleeps != self.seen {
            self.record.prepared += sleeps.wrapping_sub(self.seen);
            self.seen = sleeps;
            self.released = false;
        }
        self.preparing = sleeping;
        if !sleeping {
            self.released = false;
        }
        let wanted = active && lock_allowed && !(self.preparing && (self.released || locked_shown));
        if monitor.hold(wanted) && self.preparing {
            if active && lock_allowed && locked_shown {
                self.record.released_locked += 1;
            } else {
                self.record.released_unlocked += 1;
            }
        }
        if self.preparing && !wanted {
            self.released = true;
        }
        self.record.held = monitor.held();
    }

    pub fn record(&self) -> Record {
        self.record
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::os::fd::AsRawFd;
    use std::process::{Child, Command, Stdio};
    use std::time::Instant;

    struct Bus(Child);
    impl Drop for Bus {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn private_bus() -> (Bus, String) {
        let mut daemon = Bus(Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("private test bus starts"));
        let mut address = String::new();
        BufReader::new(daemon.0.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        (daemon, address.trim().to_owned())
    }

    fn connect(address: &str) -> zbus::Connection {
        zbus::block_on(
            zbus::connection::Builder::address(address)
                .method_timeout(Duration::from_secs(10))
                .build(),
        )
        .unwrap()
    }

    fn emit(connection: &zbus::Connection, sleeping: bool) {
        zbus::block_on(connection.emit_signal(
            None::<()>,
            "/org/freedesktop/login1",
            "org.freedesktop.login1.Manager",
            "PrepareForSleep",
            &(sleeping,),
        ))
        .unwrap();
    }

    fn wait_for(mut done: impl FnMut() -> bool) -> bool {
        // Generous for a loaded CI runner; a pass returns at once.
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if done() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        done()
    }

    #[test]
    fn only_the_bus_name_owner_can_prepare_and_wake_and_monitor_releases_worker() {
        let (daemon, address) = private_bus();
        let owner = connect(&address);
        zbus::block_on(owner.request_name("org.freedesktop.login1")).unwrap();
        let attacker = connect(&address);
        let generation = Arc::new(AtomicU64::new(0));
        let monitor = Monitor::from_connection(connect(&address), generation.clone()).unwrap();
        emit(&attacker, true);
        emit(&attacker, false);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(
            monitor.take(),
            (false, false),
            "unowned signal must not change compositor state"
        );
        assert_eq!(monitor.sleep_state(), (0, false));
        assert_eq!(generation.load(Ordering::Acquire), 0);
        emit(&owner, true);
        emit(&owner, false);
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut received = (false, false);
        while Instant::now() < deadline && received != (true, true) {
            let update = monitor.take();
            received.0 |= update.0;
            received.1 |= update.1;
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            received,
            (true, true),
            "trusted logind pair must reach compositor"
        );
        assert_eq!(
            generation.load(Ordering::Acquire),
            1,
            "trusted sleep invalidates old PAM generation"
        );
        let start = Instant::now();
        drop(monitor);
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "subscription worker must stop on runtime exit"
        );
        drop(daemon);
    }

    type Call = (String, String, String, String);

    /// logind's `Inhibit`: the caller gets the write end of a pipe and
    /// holds the inhibitor until every copy of it closes.
    struct FakeLogind {
        calls: Arc<Mutex<Vec<Call>>>,
        pipes: Arc<Mutex<Vec<std::io::PipeReader>>>,
    }
    #[zbus::interface(name = "org.freedesktop.login1.Manager")]
    impl FakeLogind {
        fn inhibit(
            &self,
            what: String,
            who: String,
            why: String,
            mode: String,
        ) -> zbus::fdo::Result<zbus::OwnedFd> {
            let (reader, writer) =
                std::io::pipe().map_err(|error| zbus::fdo::Error::Failed(error.to_string()))?;
            self.calls.lock().unwrap().push((what, who, why, mode));
            self.pipes.lock().unwrap().push(reader);
            Ok(OwnedFd::from(writer).into())
        }
    }

    /// Whether logind would still count inhibitor `index` as held: some
    /// copy of its write end is open.
    fn open(pipes: &Mutex<Vec<std::io::PipeReader>>, index: usize) -> bool {
        let pipes = pipes.lock().unwrap();
        let mut poll = libc::pollfd {
            fd: pipes[index].as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd for a descriptor `pipes` keeps open.
        let ready = unsafe { libc::poll(&mut poll, 1, 0) };
        assert!(ready >= 0, "poll the inhibitor pipe");
        poll.revents & libc::POLLHUP == 0
    }

    #[test]
    fn the_delay_inhibitor_is_held_awake_and_released_only_once_every_output_shows_the_lock() {
        let (daemon, address) = private_bus();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let pipes = Arc::new(Mutex::new(Vec::new()));
        let owner = connect(&address);
        zbus::block_on(owner.object_server().at(
            "/org/freedesktop/login1",
            FakeLogind {
                calls: calls.clone(),
                pipes: pipes.clone(),
            },
        ))
        .unwrap();
        zbus::block_on(owner.request_name("org.freedesktop.login1")).unwrap();
        let monitor =
            Monitor::from_connection(connect(&address), Arc::new(AtomicU64::new(0))).unwrap();
        let mut delay = Delay::default();
        let mut outputs = [Scanout::default(); 2];
        let shown = |outputs: &[Scanout; 2]| outputs.iter().all(Scanout::shows_locked);

        // An active, unlocked session takes a delay inhibitor on sleep.
        delay.sync(&monitor, true, true, shown(&outputs));
        assert!(wait_for(|| monitor.held()), "the inhibitor is taken");
        delay.sync(&monitor, true, true, shown(&outputs));
        assert_eq!(
            *calls.lock().unwrap(),
            [(
                "sleep".to_owned(),
                INHIBITOR_WHO.to_owned(),
                INHIBITOR_WHY.to_owned(),
                "delay".to_owned()
            )]
        );
        assert!(delay.record().held);
        assert!(open(&pipes, 0));
        // The desktop is on screen: output 0 has another unlocked frame
        // in flight, output 1 shows one.
        outputs[0].queued(false);
        outputs[1].queued(false);
        outputs[1].flipped();

        // PrepareForSleep(true): the compositor locks, but nothing locked
        // is on screen yet, so logind keeps waiting.
        emit(&owner, true);
        assert!(wait_for(|| monitor.sleep_state() == (1, true)));
        delay.sync(&monitor, true, true, shown(&outputs));
        // The unlocked frame in flight before the lock lands.
        outputs[0].flipped();
        delay.sync(&monitor, true, true, shown(&outputs));
        // Locked frames are queued on both, then flip one at a time.
        outputs[0].queued(true);
        outputs[1].queued(true);
        delay.sync(&monitor, true, true, shown(&outputs));
        outputs[0].flipped();
        delay.sync(&monitor, true, true, shown(&outputs));
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            open(&pipes, 0),
            "released before every output showed the lock"
        );
        assert!(delay.record().held);
        outputs[1].flipped();
        delay.sync(&monitor, true, true, shown(&outputs));
        assert!(
            wait_for(|| !open(&pipes, 0)),
            "every output shows the lock: logind may sleep"
        );
        assert_eq!(
            delay.record(),
            Record {
                held: false,
                prepared: 1,
                released_locked: 1,
                released_unlocked: 0,
                overrun: 0,
            }
        );
        // Nothing is taken again before the system wakes.
        delay.sync(&monitor, true, true, shown(&outputs));
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(calls.lock().unwrap().len(), 1);

        // PrepareForSleep(false): taken again for the next sleep.
        emit(&owner, false);
        assert!(wait_for(|| monitor.sleep_state() == (1, false)));
        delay.sync(&monitor, true, true, shown(&outputs));
        assert!(wait_for(|| monitor.held()), "taken again on wake");
        assert_eq!(calls.lock().unwrap().len(), 2);

        // logind's InhibitDelayMaxUSec runs out before a locked frame
        // shows (a scanout reset): the sleep ends with the inhibitor held.
        outputs[0].reset();
        emit(&owner, true);
        assert!(wait_for(|| monitor.sleep_state() == (2, true)));
        delay.sync(&monitor, true, true, shown(&outputs));
        emit(&owner, false);
        assert!(wait_for(|| monitor.sleep_state() == (2, false)));
        delay.sync(&monitor, true, true, shown(&outputs));
        assert!(
            open(&pipes, 1),
            "the same inhibitor stays for the next sleep"
        );
        assert_eq!(delay.record().overrun, 1);
        assert_eq!(calls.lock().unwrap().len(), 2);

        // The lock screen locked down: given up at once, as GNOME's
        // `_syncInhibitor` drops it, and never taken during a sleep.
        delay.sync(&monitor, true, false, shown(&outputs));
        assert!(wait_for(|| !open(&pipes, 1)), "lockdown releases it");
        emit(&owner, true);
        assert!(wait_for(|| monitor.sleep_state() == (3, true)));
        delay.sync(&monitor, true, false, shown(&outputs));
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(calls.lock().unwrap().len(), 2);
        assert!(!delay.record().held);

        // A session that leaves its VT gives it up too; it comes back
        // after the sleep once the session is active again.
        emit(&owner, false);
        assert!(wait_for(|| monitor.sleep_state() == (3, false)));
        delay.sync(&monitor, false, true, shown(&outputs));
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(calls.lock().unwrap().len(), 2, "inactive: none taken");
        delay.sync(&monitor, true, true, shown(&outputs));
        assert!(wait_for(|| monitor.held()));
        assert_eq!(calls.lock().unwrap().len(), 3);

        let start = Instant::now();
        drop(monitor);
        assert!(start.elapsed() < Duration::from_secs(2));
        assert!(
            wait_for(|| !open(&pipes, 2)),
            "the inhibitor closes with the runtime"
        );
        drop(daemon);
    }
}
