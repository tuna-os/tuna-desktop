//! Small regular-file signals consumed by the compositor loop.
//!
//! Reject special files before reading, without waiting for a FIFO peer. This
//! bounds signal size and does not claim regular-file storage I/O is async.
use std::io::{self, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

const MAX_BYTES: u64 = 4096;

pub(crate) fn read(path: &Path) -> io::Result<String> {
    read_bounded(path, MAX_BYTES)
}
pub(crate) fn read_bounded(path: &Path, max_bytes: u64) -> io::Result<String> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "runtime signal must be a small regular file",
        ));
    }
    // The file can grow after metadata inspection. Limit the descriptor read
    // too, with one extra byte to detect overflow rather than accept a prefix.
    let mut bytes = Vec::new();
    file.take(max_bytes + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "runtime signal exceeds its size limit",
        ));
    }
    String::from_utf8(bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_bounded_wallpaper_signal_preserves_sixteen_kib_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wallpaper");
        std::fs::write(&path, vec![b'x'; 16 * 1024]).unwrap();
        let mut signal = CachedSignal::new_bounded(path.clone(), 16 * 1024);
        assert_eq!(signal.text().len(), 16 * 1024);
        assert!(
            read(&path).is_err(),
            "small proof signal limit remains unchanged"
        );
        std::fs::write(&path, vec![b'x'; 16 * 1024 + 1]).unwrap();
        signal.refresh();
        assert_eq!(signal.text(), "");
    }
    #[test]
    fn regular_runtime_signals_follow_atomic_policy_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tuna-idle-blank");
        std::fs::write(&path, "300000\n10000\n").unwrap();
        let policy = crate::lock::IdleBlank::parse(&read(&path).unwrap());
        assert_eq!((policy.idle_ms, policy.fade_ms), (300000, 10000));
        let next = dir.path().join(".tuna-idle-blank.tmp");
        std::fs::write(&next, "120000\n0\n").unwrap();
        std::fs::rename(next, &path).unwrap();
        let policy = crate::lock::IdleBlank::parse(&read(&path).unwrap());
        assert_eq!((policy.idle_ms, policy.fade_ms), (120000, 0));
        std::fs::write(&path, r#"{"phase":"begin","x":0,"y":0}"#).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&read(&path).unwrap()).unwrap()["phase"],
            "begin"
        );
    }

    #[test]
    fn runtime_signal_rejects_a_fifo_without_a_writer_and_a_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("tuna-idle-blank");
        rustix::fs::mkfifoat(
            rustix::fs::CWD,
            &fifo,
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        )
        .unwrap();
        assert_eq!(read(&fifo).unwrap_err().kind(), io::ErrorKind::InvalidData);
        // Rejection keeps the existing missing/malformed policy fallback.
        let fallback = crate::lock::IdleBlank::parse(&read(&fifo).unwrap_or_default());
        assert_eq!((fallback.idle_ms, fallback.fade_ms), (0, 0));
        let target = dir.path().join("regular");
        std::fs::write(&target, "300000\n10000\n").unwrap();
        let link = dir.path().join("tuna-swipe-input");
        std::os::unix::fs::symlink(target, &link).unwrap();
        assert!(read(&link).is_err(), "signal must not follow a symlink");
    }

    #[test]
    fn runtime_signal_bounds_bytes_and_rejects_invalid_utf8() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tuna-swipe-input");
        std::fs::write(&path, vec![b'x'; MAX_BYTES as usize]).unwrap();
        assert_eq!(read(&path).unwrap().len(), MAX_BYTES as usize);
        std::fs::write(&path, vec![b'x'; MAX_BYTES as usize + 1]).unwrap();
        assert_eq!(read(&path).unwrap_err().kind(), io::ErrorKind::InvalidData);
        std::fs::write(&path, [0xff]).unwrap();
        assert_eq!(read(&path).unwrap_err().kind(), io::ErrorKind::InvalidData);
    }
}

/// Cache a small signal behind a directory watch so atomic replacement stays
/// observable. Watch failure retains the previous per-tick read behavior.
#[derive(Debug)]
pub(crate) struct CachedSignal {
    max_bytes: u64,
    path: std::path::PathBuf,
    watch: Option<std::os::fd::OwnedFd>,
    valid: bool,
    value: String,
}
impl CachedSignal {
    pub fn new(path: std::path::PathBuf) -> Self {
        Self::new_bounded(path, MAX_BYTES)
    }
    pub fn watched(&self) -> bool {
        self.valid
    }
    pub fn new_bounded(path: std::path::PathBuf, max_bytes: u64) -> Self {
        use rustix::fs::inotify::{self, CreateFlags, WatchFlags};
        // Install before initial read so changes racing startup remain queued.
        let watch = path.parent().and_then(|parent| {
            let fd = inotify::init(CreateFlags::CLOEXEC | CreateFlags::NONBLOCK).ok()?;
            inotify::add_watch(
                &fd,
                parent,
                WatchFlags::MODIFY
                    | WatchFlags::CLOSE_WRITE
                    | WatchFlags::MOVED_TO
                    | WatchFlags::MOVED_FROM
                    | WatchFlags::DELETE
                    | WatchFlags::CREATE
                    | WatchFlags::ATTRIB
                    | WatchFlags::DELETE_SELF
                    | WatchFlags::MOVE_SELF,
            )
            .ok()?;
            Some(fd)
        });
        let valid = watch.is_some();
        let value = read_bounded(&path, max_bytes).unwrap_or_default();
        Self {
            max_bytes,
            path,
            watch,
            valid,
            value,
        }
    }
    pub fn source(&self) -> io::Result<Option<calloop::generic::Generic<std::os::fd::OwnedFd>>> {
        use std::os::fd::AsFd;
        self.watch
            .as_ref()
            .map(|fd| {
                fd.as_fd().try_clone_to_owned().map(|fd| {
                    calloop::generic::Generic::new(
                        fd,
                        calloop::Interest::READ,
                        calloop::Mode::Level,
                    )
                })
            })
            .transpose()
    }
    pub fn disable(&mut self) {
        self.valid = false;
    }
    pub fn text(&mut self) -> &str {
        if !self.valid {
            self.value = read_bounded(&self.path, self.max_bytes).unwrap_or_default();
        }
        &self.value
    }
    /// Return whether the watch remains usable; false removes its loop source.
    pub fn refresh(&mut self) -> bool {
        use rustix::fs::inotify::Reader;
        let Some(fd) = self.watch.as_ref() else {
            return false;
        };
        let mut buffer = [std::mem::MaybeUninit::uninit(); 8192];
        let mut reader = Reader::new(fd, &mut buffer);
        let mut dirty = false;
        // Bound one callback even if another same-user writer floods events.
        // Reader may prefetch events past the cap. Retire the watch at that
        // boundary because a lost watch-invalidating event could otherwise
        // leave a permanently stale cache. The 16 ms fallback remains bounded.
        for index in 0..256 {
            match reader.next() {
                Ok(event) => {
                    let (changed, valid) = signal_event(
                        event.events(),
                        event.file_name().map(|n| n.to_bytes()),
                        self.path.file_name().map(|n| {
                            use std::os::unix::ffi::OsStrExt;
                            n.as_bytes()
                        }),
                    );
                    dirty |= changed;
                    self.valid &= valid;
                    if index == 255 {
                        dirty = true;
                        self.valid = false;
                    }
                }
                Err(rustix::io::Errno::WOULDBLOCK) => break,
                Err(rustix::io::Errno::INTR) => continue,
                Err(_) => {
                    self.valid = false;
                    dirty = true;
                    break;
                }
            }
        }
        if dirty {
            self.value = read_bounded(&self.path, self.max_bytes).unwrap_or_default();
        }
        self.valid
    }
}

fn signal_event(
    flags: rustix::fs::inotify::ReadFlags,
    name: Option<&[u8]>,
    target: Option<&[u8]>,
) -> (bool, bool) {
    use rustix::fs::inotify::ReadFlags;
    let invalid = flags.intersects(
        ReadFlags::IGNORED | ReadFlags::DELETE_SELF | ReadFlags::MOVE_SELF | ReadFlags::UNMOUNT,
    );
    (
        invalid || flags.contains(ReadFlags::QUEUE_OVERFLOW) || name.is_some() && name == target,
        !invalid,
    )
}

#[cfg(test)]
mod cache_tests {
    use super::*;
    #[test]
    fn cached_signal_updates_on_atomic_replacement_deletion_and_recreation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tuna-idle-blank");
        std::fs::write(&path, "old").unwrap();
        let mut signal = CachedSignal::new(path.clone());
        assert!(signal.valid);
        assert_eq!(signal.text(), "old");
        let next = dir.path().join(".next");
        std::fs::write(&next, "new").unwrap();
        std::fs::rename(next, &path).unwrap();
        assert_eq!(signal.text(), "old", "watched cache waits for its event");
        assert!(signal.refresh());
        assert_eq!(signal.text(), "new");
        std::fs::remove_file(&path).unwrap();
        signal.refresh();
        assert_eq!(signal.text(), "");
        std::fs::write(&path, "again").unwrap();
        signal.refresh();
        assert_eq!(signal.text(), "again");
    }
    #[test]
    fn watched_signal_wakes_loop_drains_and_removes_invalid_directory_source() {
        use std::time::Duration;
        let dir = tempfile::tempdir().unwrap();
        let watched = dir.path().join("watched");
        std::fs::create_dir(&watched).unwrap();
        let path = watched.join("policy");
        let signal = CachedSignal::new(path.clone());
        let mut event_loop = calloop::EventLoop::<(CachedSignal, usize)>::try_new().unwrap();
        event_loop
            .handle()
            .insert_source(signal.source().unwrap().unwrap(), |_, _, state| {
                state.1 += 1;
                Ok(if state.0.refresh() {
                    calloop::PostAction::Continue
                } else {
                    calloop::PostAction::Remove
                })
            })
            .unwrap();
        let mut state = (signal, 0);
        std::fs::write(&path, "live").unwrap();
        event_loop
            .dispatch(Some(Duration::from_secs(1)), &mut state)
            .unwrap();
        assert_eq!(state.0.text(), "live");
        assert_eq!(state.1, 1);
        event_loop
            .dispatch(Some(Duration::ZERO), &mut state)
            .unwrap();
        assert_eq!(state.1, 1, "drained directory must not spin");
        std::fs::rename(&watched, dir.path().join("moved")).unwrap();
        event_loop
            .dispatch(Some(Duration::from_secs(1)), &mut state)
            .unwrap();
        assert_eq!(state.1, 2);
        assert!(!state.0.valid);
        std::fs::create_dir(&watched).unwrap();
        std::fs::write(&path, "fallback").unwrap();
        event_loop
            .dispatch(Some(Duration::ZERO), &mut state)
            .unwrap();
        assert_eq!(state.1, 2, "invalid watch source must be removed");
        assert_eq!(state.0.text(), "fallback");
    }
    #[test]
    fn overflow_requires_rescan_and_invalid_watch_requires_polling() {
        use rustix::fs::inotify::ReadFlags;
        assert_eq!(
            signal_event(ReadFlags::QUEUE_OVERFLOW, None, Some(b"policy")),
            (true, true)
        );
        assert_eq!(
            signal_event(ReadFlags::IGNORED, None, Some(b"policy")),
            (true, false)
        );
        assert_eq!(
            signal_event(ReadFlags::CLOSE_WRITE, Some(b"other"), Some(b"policy")),
            (false, true)
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("policy");
        let mut signal = CachedSignal::new(path.clone());
        signal.disable();
        std::fs::write(&path, "fallback").unwrap();
        assert_eq!(signal.text(), "fallback");
    }
    #[test]
    fn cache_preserves_regular_file_rejection_after_symlink_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("policy");
        std::fs::write(&path, "old").unwrap();
        let mut signal = CachedSignal::new(path.clone());
        std::fs::remove_file(&path).unwrap();
        let target = dir.path().join("target");
        std::fs::write(&target, "bad").unwrap();
        std::os::unix::fs::symlink(target, path).unwrap();
        signal.refresh();
        assert_eq!(signal.text(), "");
    }
    #[test]
    fn event_flood_and_late_directory_move_force_bounded_polling_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let watched = dir.path().join("watched");
        std::fs::create_dir(&watched).unwrap();
        let path = watched.join("policy");
        std::fs::write(&path, "initial").unwrap();
        let mut signal = CachedSignal::new(path.clone());
        // Long distinct names yield >256 events and multiple read batches.
        // The move event can be prefetched beyond the callback's event cap.
        for index in 0..100 {
            std::fs::write(
                watched.join(format!("unrelated-long-name-{}-{index}", "x".repeat(64))),
                "flood",
            )
            .unwrap();
        }
        std::fs::rename(&watched, dir.path().join("moved")).unwrap();
        assert!(
            !signal.refresh(),
            "cap must retire possibly invalid prefetched watch"
        );
        std::fs::create_dir(&watched).unwrap();
        std::fs::write(&path, "new directory").unwrap();
        assert_eq!(
            signal.text(),
            "new directory",
            "lost invalidation must not strand cache"
        );
    }
    #[test]
    fn watch_creation_failure_keeps_live_polling_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("not-created");
        let path = parent.join("policy");
        let mut signal = CachedSignal::new(path.clone());
        assert!(!signal.valid);
        assert!(signal.source().unwrap().is_none());
        std::fs::create_dir(&parent).unwrap();
        std::fs::write(&path, "created later").unwrap();
        assert_eq!(signal.text(), "created later");
        std::fs::remove_file(&path).unwrap();
        assert_eq!(signal.text(), "");
    }
    #[test]
    fn directory_move_invalidates_watch_and_retains_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("watched");
        std::fs::create_dir(&original).unwrap();
        let path = original.join("policy");
        let mut signal = CachedSignal::new(path.clone());
        std::fs::rename(&original, dir.path().join("moved")).unwrap();
        assert!(!signal.refresh());
        std::fs::create_dir(&original).unwrap();
        std::fs::write(&path, "new directory").unwrap();
        assert_eq!(signal.text(), "new directory");
    }
}
