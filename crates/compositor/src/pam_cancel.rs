//! Per-attempt cancellation with a wake bound only to its admitted PAM backend.
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

#[derive(Default)]
struct State {
    canceled: AtomicBool,
    wake: OnceLock<Arc<WakeFd>>,
}

/// Clone this exact latch into the runtime worker, ticket and admitted job.
/// An unbound greetd latch cannot initialize a PAM backend or choose a helper.
#[derive(Clone, Default)]
pub struct Cancellation(Arc<State>);
impl Cancellation {
    pub fn cancel(&self) {
        self.0.canceled.store(true, Ordering::Release);
        if let Some(wake) = self.0.wake.get() {
            // The owned nonblocking fd stays alive with this latch. A write
            // failure still leaves denial authoritative; never releases admission.
            let _ = wake.signal();
        }
    }
    pub fn is_canceled(&self) -> bool {
        self.0.canceled.load(Ordering::Acquire)
            || self
                .0
                .wake
                .get()
                .is_some_and(|wake| wake.shutdown.load(Ordering::Acquire))
    }
    pub(crate) fn bind(&self, wake: Arc<WakeFd>) -> io::Result<()> {
        self.0
            .wake
            .set(wake.clone())
            .map_err(|_| io::ErrorKind::InvalidInput)?;
        // Cancel-before-bind and cancel racing registration both leave a wake.
        if self.is_canceled() {
            wake.signal()?;
        }
        Ok(())
    }
    pub(crate) fn fd(&self) -> Option<RawFd> {
        self.0.wake.get().map(|wake| wake.fd.as_raw_fd())
    }
    pub(crate) fn drain(&self) -> io::Result<()> {
        self.0
            .wake
            .get()
            .ok_or(io::ErrorKind::InvalidInput)?
            .drain()
    }
}

pub(crate) struct WakeFd {
    fd: OwnedFd,
    shutdown: Arc<AtomicBool>,
}
impl WakeFd {
    pub(crate) fn new(shutdown: Arc<AtomicBool>) -> io::Result<Arc<Self>> {
        // SAFETY: eventfd returns a newly owned descriptor or an error.
        let fd = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Arc::new(Self {
            fd: unsafe { OwnedFd::from_raw_fd(fd) },
            shutdown,
        }))
    }
    pub(crate) fn signal(&self) -> io::Result<()> {
        let value = 1_u64;
        loop {
            // SAFETY: borrowed fd and one initialized eventfd-sized value.
            let count =
                unsafe { libc::write(self.fd.as_raw_fd(), (&value as *const u64).cast(), 8) };
            if count == 8 {
                return Ok(());
            }
            if count >= 0 {
                return Err(io::ErrorKind::InvalidData.into());
            }
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            if error.kind() == io::ErrorKind::WouldBlock {
                return Ok(());
            }
            return Err(error);
        }
    }
    fn drain(&self) -> io::Result<()> {
        loop {
            let mut value = 0_u64;
            // SAFETY: borrowed fd and exclusively owned eventfd-sized storage.
            let count =
                unsafe { libc::read(self.fd.as_raw_fd(), (&mut value as *mut u64).cast(), 8) };
            if count == 8 {
                return Ok(());
            }
            if count >= 0 {
                return Err(io::ErrorKind::InvalidData.into());
            }
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            if error.kind() == io::ErrorKind::WouldBlock {
                return Ok(());
            }
            return Err(error);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancellation_before_registration_is_woken_and_cannot_rebind() {
        let latch = Cancellation::default();
        latch.cancel();
        let wake = WakeFd::new(Arc::new(AtomicBool::new(false))).unwrap();
        latch.bind(wake.clone()).unwrap();
        let mut fd = libc::pollfd {
            fd: latch.fd().unwrap(),
            events: libc::POLLIN,
            revents: 0,
        };
        assert_eq!(unsafe { libc::poll(&mut fd, 1, 0) }, 1);
        assert!(latch.is_canceled());
        assert!(latch.bind(wake).is_err());
    }
    #[test]
    fn stale_wake_count_does_not_cancel_a_fresh_admitted_latch() {
        let wake = WakeFd::new(Arc::new(AtomicBool::new(false))).unwrap();
        let old = Cancellation::default();
        old.bind(wake.clone()).unwrap();
        let fresh = Cancellation::default();
        fresh.bind(wake).unwrap();
        for _ in 0..100 {
            old.cancel();
        }
        fresh.drain().unwrap();
        assert!(old.is_canceled());
        assert!(!fresh.is_canceled());
        let mut fd = libc::pollfd {
            fd: fresh.fd().unwrap(),
            events: libc::POLLIN,
            revents: 0,
        };
        assert_eq!(unsafe { libc::poll(&mut fd, 1, 0) }, 0);
    }
}
