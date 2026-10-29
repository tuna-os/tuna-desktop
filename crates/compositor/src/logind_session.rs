//! The compositor's own logind session, as GNOME Shell's screen shield
//! uses it (`js/ui/screenShield.js`, `js/misc/loginManager.js`):
//! `SetLockedHint` follows the lock, and the session's `Lock`/`Unlock`
//! signals (`loginctl lock-session` / `unlock-session`) lock and unlock.
//! The lock lives in the compositor (see [`crate::lock`]), so the hint is
//! reported from here rather than from the shell.

use zbus::zvariant::OwnedObjectPath;

/// This process's logind session object, found as `gsm-systemd.c` and
/// `loginManager.js` find it: `XDG_SESSION_ID`, else the session of this PID.
pub(crate) fn current_session_path(
    system: &zbus::blocking::Connection,
) -> zbus::Result<OwnedObjectPath> {
    let manager = zbus::blocking::Proxy::new(
        system,
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
    )?;
    match std::env::var("XDG_SESSION_ID") {
        Ok(id) if !id.is_empty() => manager.call("GetSession", &(id.as_str(),)),
        _ => manager.call("GetSessionByPID", &(std::process::id(),)),
    }
}

#[cfg(feature = "drm")]
pub use monitor::{Request, Session};

#[cfg(feature = "drm")]
mod monitor {
    use std::sync::atomic::{AtomicU8, Ordering};
    use std::sync::{mpsc, Arc};
    use std::time::Duration;

    const NONE: u8 = 0;
    const LOCK: u8 = 1;
    const UNLOCK: u8 = 2;

    /// What logind asked of the session's screen shield.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Request {
        /// `Lock`: `screenShield.lock(false)`.
        Lock,
        /// `Unlock`: `screenShield.deactivate(false)`.
        Unlock,
    }

    pub struct Session {
        wake: crate::wake::Wake,
        connection: zbus::blocking::Connection,
        /// The latest signal wins, as GNOME handles them in order.
        pending: Arc<AtomicU8>,
        hint: Option<mpsc::Sender<bool>>,
        reported: Option<bool>,
        workers: Vec<std::thread::JoinHandle<()>>,
    }

    impl Session {
        pub fn start() -> zbus::Result<Self> {
            let connection = zbus::blocking::connection::Builder::system()?
                .method_timeout(Duration::from_secs(5))
                .build()?;
            let path = super::current_session_path(&connection)?;
            Self::from_connection(connection, path)
        }

        fn from_connection(
            connection: zbus::blocking::Connection,
            path: zbus::zvariant::OwnedObjectPath,
        ) -> zbus::Result<Self> {
            // A proxy on the well-known name accepts signals from its
            // owner only, never from another connection on the bus.
            let proxy = zbus::blocking::Proxy::new(
                &connection,
                "org.freedesktop.login1",
                path,
                "org.freedesktop.login1.Session",
            )?;
            let lock = proxy.receive_signal("Lock")?;
            let unlock = proxy.receive_signal("Unlock")?;
            let wake = crate::wake::Wake::default();
            let pending = Arc::new(AtomicU8::new(NONE));
            let mut workers = Vec::with_capacity(3);
            for (signals, request, name) in [
                (lock, LOCK, "tuna-logind-lock"),
                (unlock, UNLOCK, "tuna-logind-unlock"),
            ] {
                let wake = wake.clone();
                let pending = pending.clone();
                workers.push(
                    std::thread::Builder::new()
                        .name(name.into())
                        .spawn(move || {
                            let _completion = wake.completion();
                            for _ in signals {
                                pending.store(request, Ordering::Release);
                                wake.signal();
                            }
                        })?,
                );
            }
            // SetLockedHint leaves the loop: a slow bus never stalls a
            // frame. Every change is sent, in order, as GNOME sends it.
            let (hint, hints) = mpsc::channel::<bool>();
            workers.push(
                std::thread::Builder::new()
                    .name("tuna-logind-hint".into())
                    .spawn(move || {
                        for locked in hints {
                            if let Err(error) = proxy.call_method("SetLockedHint", &(locked,)) {
                                eprintln!(
                                    "tuna-compositor: logind SetLockedHint({locked}) failed: {error}"
                                );
                            }
                        }
                    })?,
            );
            Ok(Self {
                wake,
                connection,
                pending,
                hint: Some(hint),
                reported: None,
                workers,
            })
        }

        pub(crate) fn set_wake(&self, wake: &crate::wake::Wake) {
            self.wake.connect(wake);
        }

        /// The latest logind request since the last call.
        pub fn take(&self) -> Option<Request> {
            match self.pending.swap(NONE, Ordering::AcqRel) {
                LOCK => Some(Request::Lock),
                UNLOCK => Some(Request::Unlock),
                _ => None,
            }
        }

        /// `screenShield.js` `_setLocked`: tell logind on each change of
        /// the lock, starting with the state this session began in.
        pub fn report_locked(&mut self, locked: bool) {
            if self.reported == Some(locked) {
                return;
            }
            self.reported = Some(locked);
            if let Some(hint) = &self.hint {
                let _ = hint.send(locked);
            }
        }
    }

    impl Drop for Session {
        fn drop(&mut self) {
            // Closing ends the signal iterators and fails any call in
            // flight; dropping the sender ends the hint worker.
            let _ = self.connection.clone().close();
            self.hint = None;
            for worker in self.workers.drain(..) {
                let _ = worker.join();
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::io::{BufRead, BufReader};
        use std::process::{Child, Command, Stdio};
        use std::sync::Mutex;
        use std::time::Instant;

        struct Bus(Child);
        impl Drop for Bus {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }

        const PATH: &str = "/org/freedesktop/login1/session/_31";

        struct FakeSession(Arc<Mutex<Vec<bool>>>);
        #[zbus::interface(name = "org.freedesktop.login1.Session")]
        impl FakeSession {
            fn set_locked_hint(&self, locked: bool) {
                self.0.lock().unwrap().push(locked);
            }
        }

        fn wait_for(mut done: impl FnMut() -> bool) -> bool {
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline {
                if done() {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            done()
        }

        #[test]
        fn logind_lock_and_unlock_reach_the_compositor_and_the_hint_follows_the_lock() {
            let mut daemon = Bus(Command::new("dbus-daemon")
                .args(["--session", "--nofork", "--print-address=1"])
                .stdout(Stdio::piped())
                .spawn()
                .expect("private test bus starts"));
            let mut address = String::new();
            BufReader::new(daemon.0.stdout.take().unwrap())
                .read_line(&mut address)
                .unwrap();
            let connect = || {
                zbus::blocking::connection::Builder::address(address.trim())
                    .unwrap()
                    .method_timeout(Duration::from_secs(2))
                    .build()
                    .unwrap()
            };
            let hints = Arc::new(Mutex::new(Vec::new()));
            let owner = connect();
            owner
                .object_server()
                .at(PATH, FakeSession(hints.clone()))
                .unwrap();
            owner.request_name("org.freedesktop.login1").unwrap();
            let attacker = connect();
            let mut session =
                Session::from_connection(connect(), PATH.try_into().unwrap()).unwrap();
            let emit = |connection: &zbus::blocking::Connection, name: &str| {
                connection
                    .emit_signal(
                        None::<()>,
                        PATH,
                        "org.freedesktop.login1.Session",
                        name,
                        &(),
                    )
                    .unwrap();
            };

            emit(&attacker, "Lock");
            emit(&attacker, "Unlock");
            std::thread::sleep(Duration::from_millis(100));
            assert_eq!(session.take(), None, "only logind may lock or unlock");

            emit(&owner, "Lock");
            assert!(wait_for(|| session.pending.load(Ordering::Acquire) != NONE));
            assert_eq!(session.take(), Some(Request::Lock));
            assert_eq!(session.take(), None, "a request is taken once");
            emit(&owner, "Unlock");
            assert!(wait_for(|| session.pending.load(Ordering::Acquire) != NONE));
            assert_eq!(session.take(), Some(Request::Unlock));

            session.report_locked(false);
            session.report_locked(true);
            session.report_locked(true);
            session.report_locked(false);
            assert!(wait_for(|| hints.lock().unwrap().len() >= 3));
            std::thread::sleep(Duration::from_millis(100));
            assert_eq!(
                *hints.lock().unwrap(),
                [false, true, false],
                "the starting state, then each change once, in order"
            );

            let start = Instant::now();
            drop(session);
            assert!(
                start.elapsed() < Duration::from_secs(2),
                "workers stop with the runtime"
            );
            drop(daemon);
        }
    }
}
