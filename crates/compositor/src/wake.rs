//! Event sources that let the compositor loop sleep until something
//! happens (#503). The loop used to wake every 16 ms and poll; now each
//! producer that lives outside the Wayland, DRM, input and D-Bus channel
//! sources wakes it: POSIX signals (child exits, termination) through a
//! socket pair, and the shell's runtime files through inotify. Worker
//! threads ping it through a per-runtime, coalesced [`Wake`].

use std::ffi::OsStr;
use std::io::Read;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use calloop::generic::Generic;
use calloop::{Interest, Mode};
use rustix::fs::inotify;

/// The shell's idle-blank settings, in the runtime dir.
pub(crate) const IDLE_BLANK_FILE: &str = "tuna-idle-blank";

/// A source that turns readable on every delivery of `signals`. The
/// handlers stay installed for the life of the process, next to any
/// others (signal-hook chains them); [`drain_signals`] empties it.
pub(crate) fn signal_source(signals: &[i32]) -> std::io::Result<Generic<UnixStream>> {
    let (read, write) = UnixStream::pair()?;
    read.set_nonblocking(true)?;
    write.set_nonblocking(true)?;
    for &signal in signals {
        signal_hook::low_level::pipe::register(signal, write.try_clone()?)?;
    }
    Ok(Generic::new(read, Interest::READ, Mode::Level))
}

/// Read every pending signal byte so a level-triggered source settles.
pub(crate) fn drain_signals(mut stream: &UnixStream) {
    let mut buf = [0u8; 64];
    while matches!(stream.read(&mut buf), Ok(n) if n > 0) {}
}

/// Watch `dir` for files being written, renamed in, or removed. Readable
/// while events are queued; [`changed`] reports and drains them.
pub(crate) fn file_watch(dir: &Path) -> std::io::Result<Generic<OwnedFd>> {
    let fd = inotify::init(inotify::CreateFlags::CLOEXEC | inotify::CreateFlags::NONBLOCK)?;
    inotify::add_watch(
        &fd,
        dir,
        inotify::WatchFlags::CLOSE_WRITE
            | inotify::WatchFlags::MOVED_TO
            | inotify::WatchFlags::CREATE
            | inotify::WatchFlags::DELETE
            | inotify::WatchFlags::MOVED_FROM,
    )?;
    Ok(Generic::new(fd, Interest::READ, Mode::Level))
}

/// Drain the watch's queued events and report whether any named
/// `wanted` (or the kernel dropped events, so any may have changed).
pub(crate) fn changed(fd: impl AsFd, wanted: &[&OsStr]) -> bool {
    let mut buf = [std::mem::MaybeUninit::<u8>::uninit(); 4096];
    let mut reader = inotify::Reader::new(fd, &mut buf);
    let mut hit = false;
    while let Ok(event) = reader.next() {
        hit |= event.events().contains(inotify::ReadFlags::QUEUE_OVERFLOW)
            || event
                .file_name()
                .is_some_and(|name| wanted.contains(&OsStr::from_bytes(name.to_bytes())));
    }
    hit
}

#[derive(Clone, Default)]
pub(crate) struct Wake(Arc<Mutex<Option<calloop::ping::Ping>>>);
impl std::fmt::Debug for Wake {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Wake")
    }
}
impl Wake {
    pub fn connect(&self, other: &Self) {
        let ping = other.0.lock().ok().and_then(|p| p.clone());
        if let Ok(mut slot) = self.0.lock() {
            *slot = ping;
        }
    }
    pub fn new() -> std::io::Result<(Self, calloop::ping::PingSource)> {
        let (ping, source) = calloop::ping::make_ping()?;
        Ok((Self(Arc::new(Mutex::new(Some(ping)))), source))
    }
    pub fn signal(&self) {
        if let Ok(slot) = self.0.lock() {
            if let Some(ping) = slot.as_ref() {
                ping.ping();
            }
        }
    }
    pub fn completion(&self) -> CompletionWake {
        CompletionWake(self.clone())
    }
    /// Publish the terminal worker phase before waking, including on unwind.
    pub fn phase_completion(&self, phase: Arc<AtomicU8>) -> PhaseCompletion {
        PhaseCompletion {
            phase,
            _wake: self.completion(),
        }
    }
}
pub(crate) struct CompletionWake(Wake);
impl Drop for CompletionWake {
    fn drop(&mut self) {
        self.0.signal();
    }
}
pub(crate) struct PhaseCompletion {
    phase: Arc<AtomicU8>,
    _wake: CompletionWake,
}
impl Drop for PhaseCompletion {
    fn drop(&mut self) {
        self.phase.store(2, Ordering::Release);
        // _wake is dropped after this body. A dispatched wake must observe
        // phase 2 even if the thread is still unwinding before is_finished().
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_renamed_in_file_is_reported_and_other_names_are_not() {
        let dir = tempfile::tempdir().unwrap();
        let watch = file_watch(dir.path()).unwrap();
        let fd = watch.get_ref();
        let wanted = [OsStr::new("tuna-idle-blank")];
        std::fs::write(dir.path().join("unrelated"), "x").unwrap();
        assert!(!changed(fd, &wanted), "other files never count");
        std::fs::write(dir.path().join(".tuna-idle-blank.tmp"), "300000\n0\n").unwrap();
        std::fs::rename(
            dir.path().join(".tuna-idle-blank.tmp"),
            dir.path().join("tuna-idle-blank"),
        )
        .unwrap();
        assert!(changed(fd, &wanted), "the shell's atomic replace is seen");
        assert!(!changed(fd, &wanted), "events are drained");
    }

    #[test]
    fn a_signal_makes_the_source_readable_until_drained() {
        let source = signal_source(&[signal_hook::consts::SIGUSR2]).unwrap();
        let mut stream = source.get_ref();
        let mut probe = [0u8; 1];
        assert!(stream.read(&mut probe).is_err(), "quiet before any signal");
        signal_hook::low_level::raise(signal_hook::consts::SIGUSR2).unwrap();
        assert!(
            matches!(stream.read(&mut probe), Ok(1)),
            "one byte per delivery"
        );
        signal_hook::low_level::raise(signal_hook::consts::SIGUSR2).unwrap();
        signal_hook::low_level::raise(signal_hook::consts::SIGUSR2).unwrap();
        drain_signals(stream);
        assert!(stream.read(&mut probe).is_err(), "drained");
    }
}

#[cfg(test)]
mod wake_tests {
    use super::*;
    use std::time::Duration;
    #[test]
    fn pending_panic_publishes_terminal_phase_before_wake_and_thread_retirement() {
        struct HoldUnwind(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>);
        impl Drop for HoldUnwind {
            fn drop(&mut self) {
                let _ = self.0.send(());
                let _ = self.1.recv_timeout(Duration::from_secs(5));
            }
        }
        let (wake, source) = Wake::new().unwrap();
        let phase = Arc::new(AtomicU8::new(0));
        let observed_phase = phase.clone();
        let mut loop_ = calloop::EventLoop::<Option<u8>>::try_new().unwrap();
        loop_
            .handle()
            .insert_source(source, move |_, _, observed| {
                *observed = Some(observed_phase.load(Ordering::Acquire));
            })
            .unwrap();
        let (held, hold_ready) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let mut slot = crate::auth_owner::Slot::default();
        let attempt = slot.begin(None, 1).unwrap();
        let worker_phase = phase.clone();
        let worker = std::thread::spawn(move || -> crate::pam_backend::Verification {
            let _hold = HoldUnwind(held, released);
            let _completion = wake.phase_completion(worker_phase.clone());
            worker_phase.store(1, Ordering::Release);
            panic!("controlled pending worker panic");
        });
        hold_ready.recv_timeout(Duration::from_secs(2)).unwrap();
        let mut observed = None;
        loop_
            .dispatch(Some(Duration::from_secs(2)), &mut observed)
            .unwrap();
        assert_eq!(
            observed,
            Some(2),
            "completion wake cannot leave Pending idle"
        );
        assert!(
            !worker.is_finished(),
            "wake intentionally precedes actual thread retirement"
        );
        assert!(slot.matches(attempt), "wake is not an owner release");
        release.send(()).unwrap();
        let result = worker
            .join()
            .unwrap_or(crate::pam_backend::Verification::Unretired);
        assert!(!result.authenticated());
        assert!(
            !result.may_retire(),
            "panicked wrapper cannot certify backend retirement"
        );
        assert!(slot.matches(attempt));
    }
    #[test]
    fn disconnect_is_published_before_completion_wake_even_on_unwind() {
        let (wake, source) = Wake::new().unwrap();
        let (send, receive) = std::sync::mpsc::channel::<()>();
        let completion = wake.completion();
        let mut loop_ = calloop::EventLoop::<bool>::try_new().unwrap();
        loop_
            .handle()
            .insert_source(source, move |_, _, disconnected| {
                *disconnected = matches!(
                    receive.try_recv(),
                    Err(std::sync::mpsc::TryRecvError::Disconnected)
                );
            })
            .unwrap();
        let worker = std::thread::spawn(move || {
            let _completion = completion;
            let _send = send;
            panic!("deliberate worker failure");
        });
        let mut disconnected = false;
        loop_
            .dispatch(Some(Duration::from_secs(2)), &mut disconnected)
            .unwrap();
        assert!(disconnected, "wake must follow sender retirement");
        assert!(worker.join().is_err());
    }
    #[test]
    fn worker_completion_and_disconnect_wake_and_drain_without_spinning() {
        let (wake, source) = Wake::new().unwrap();
        let mut loop_ = calloop::EventLoop::<usize>::try_new().unwrap();
        loop_
            .handle()
            .insert_source(source, |_, _, count| *count += 1)
            .unwrap();
        let mut count = 0;
        let worker_wake = Wake::default();
        worker_wake.connect(&wake);
        let guard = worker_wake.completion();
        std::thread::spawn(move || {
            drop(guard);
        })
        .join()
        .unwrap();
        loop_
            .dispatch(Some(Duration::from_secs(1)), &mut count)
            .unwrap();
        assert_eq!(count, 1);
        loop_.dispatch(Some(Duration::ZERO), &mut count).unwrap();
        assert_eq!(count, 1);
        for _ in 0..100 {
            wake.signal();
        }
        loop_
            .dispatch(Some(Duration::from_secs(1)), &mut count)
            .unwrap();
        assert_eq!(count, 2, "bounded coalesced wake");
    }
}
