//! Event sources that let the compositor loop sleep until something
//! happens (#503). The loop used to wake every 16 ms and poll; now each
//! producer that lives outside the Wayland, DRM, input and D-Bus channel
//! sources wakes it: POSIX signals (child exits, termination) through a
//! socket pair, and the shell's runtime files through inotify. Worker
//! threads use a `calloop::ping::Ping` directly.

use std::ffi::OsStr;
use std::io::Read;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::path::Path;

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
