//! Clipboard retention, as GNOME 51's mutter keeps the clipboard
//! pasteable after the copying application closes:
//! `src/core/meta-clipboard-manager.c` (`owner_changed_cb`,
//! `mimetype_match`, `transfer_cb`).
//!
//! When a client takes the clipboard, any copy in flight is cancelled
//! and the saved copy dropped; if the selection offers a supported mime
//! type, the most preferred one (mutter's `supported_mimetypes`, least
//! to most preferred, matched exactly) is read into memory. As mutter's
//! `meta_selection_transfer_async` with a size does, the read stops at
//! the type's limit (4 MiB for text, 200 MiB for images) and keeps what
//! it has: a larger copy is saved truncated. When the owner goes away
//! (it sets a NULL selection, destroys its source or disconnects) and a
//! finished copy exists, the compositor takes the clipboard over,
//! offering exactly the saved type, and serves the saved bytes; without
//! one the clipboard is left empty. The compositor's own retained
//! selection never restarts the cycle: smithay does not report
//! compositor-set selections to `SelectionHandler::new_selection`, as
//! mutter skips `display->selection_source`. Only the clipboard is
//! retained, never the primary selection, as in mutter.
//!
//! Smithay reports a new selection before it stores it
//! (`data_device/device.rs`, `SetSelection`) and drops a dead source
//! only lazily without telling the handler. The copy request, the dead
//! owner check and the take-over therefore run from
//! [`State::retain_clipboard`](crate::State::retain_clipboard) after each
//! client dispatch; the owner's liveness comes from the additive
//! `IsAlive for SelectionSource` patch (`third-party/smithay`, #1033).
//!
//! Recorded divergences:
//! - A RemoteDesktop session's selection is not copied. Mutter's owner
//!   change also fires for `MetaSelectionSourceRemote`, so it saves a
//!   copy of it too; here a remote session taking the clipboard only
//!   drops the watched owner and any saved copy.
//! - The copy and every serve run on their own threads (`tuna-clip-retain`,
//!   `tuna-clip-serve`), never on the compositor loop. Mutter cancels a
//!   copy 15 s after it starts (`TransferRequest`'s timeout); here 15 s
//!   without progress ends a copy or a serve (the RemoteDesktop
//!   clipboard's `TRANSFER_TIMEOUT`), and at most `MAX_WRITES` serves run
//!   at once; further readers see end of file.

use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use smithay::utils::IsAlive;
use smithay::wayland::selection::SelectionSource;

use crate::mutter::remote_clipboard::{nonblocking, ready, MAX_WRITES, TRANSFER_TIMEOUT};

/// Mutter's `MAX_TEXT_SIZE`.
pub(crate) const MAX_TEXT_SIZE: usize = 4 * 1024 * 1024;
/// Mutter's `MAX_IMAGE_SIZE`.
pub(crate) const MAX_IMAGE_SIZE: usize = 200 * 1024 * 1024;

/// Mutter's `supported_mimetypes`, from least to most preferred, with
/// each type's size limit. Mutter matches them with
/// `g_pattern_match_simple`; none holds a wildcard, so a match is exact.
pub(crate) const SUPPORTED_MIME_TYPES: [(&str, usize); 9] = [
    ("image/tiff", MAX_IMAGE_SIZE),
    ("image/bmp", MAX_IMAGE_SIZE),
    ("image/gif", MAX_IMAGE_SIZE),
    ("image/jpeg", MAX_IMAGE_SIZE),
    ("image/webp", MAX_IMAGE_SIZE),
    ("image/png", MAX_IMAGE_SIZE),
    ("image/svg+xml", MAX_IMAGE_SIZE),
    ("text/plain", MAX_TEXT_SIZE),
    ("text/plain;charset=utf-8", MAX_TEXT_SIZE),
];

/// The most preferred supported type among `offered`, with its size
/// limit: mutter's `owner_changed_cb` loop over `mimetype_match`. On a
/// tie (a type offered twice) the first offer wins, as `best_idx < idx`.
pub(crate) fn best_mime_type(offered: &[String]) -> Option<(String, usize)> {
    let mut best: Option<(usize, &String, usize)> = None;
    for mime_type in offered {
        let Some(index) = SUPPORTED_MIME_TYPES
            .iter()
            .position(|(supported, _)| supported == mime_type)
        else {
            continue;
        };
        if best.is_none_or(|(best_index, _, _)| best_index < index) {
            best = Some((index, mime_type, SUPPORTED_MIME_TYPES[index].1));
        }
    }
    best.map(|(_, mime_type, limit)| (mime_type.clone(), limit))
}

/// The user data of a compositor-provided clipboard selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompositorSelection {
    /// A RemoteDesktop session's selection (SetSelection); its id.
    Remote(u64),
    /// The saved copy of a gone client's selection; the generation of
    /// the client selection it was copied from.
    Retained(u64),
}

/// A finished copy of one client selection.
#[derive(Clone)]
pub(crate) struct Saved {
    generation: u64,
    mime_type: String,
    data: Arc<[u8]>,
}

/// Mutter's `display->saved_clipboard*` and `display->selection_source`.
#[derive(Default)]
pub(crate) struct ClipboardManager {
    /// The client selection being copied, watched for its end.
    owner: Option<SelectionSource>,
    /// The copy to request once smithay has stored `owner`: type and limit.
    pending: Option<(String, usize)>,
    /// The owner went away; take over on the next tick.
    owner_gone: bool,
    /// Bumped whenever a client or remote session takes the clipboard;
    /// a copy started under an earlier generation stops and is discarded.
    generation: Arc<AtomicU64>,
    /// Filled by the copy thread when its read finishes.
    saved: Arc<Mutex<Option<Saved>>>,
    /// The copy the compositor's selection currently serves.
    served: Option<Saved>,
    /// Serves in flight.
    writes: Arc<AtomicU32>,
}

impl ClipboardManager {
    /// Cancel a copy in flight and drop the saved and served copies.
    fn reset(&mut self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut saved) = self.saved.lock() {
            *saved = None;
        }
        self.served = None;
        self.owner = None;
        self.pending = None;
        self.owner_gone = false;
    }

    /// A client set the clipboard (`Some`) or cleared it (`None`):
    /// mutter's `owner_changed_cb` for a new owner or for no owner.
    pub(crate) fn client_selection(&mut self, source: Option<SelectionSource>) {
        match source {
            Some(source) => {
                self.reset();
                self.pending = best_mime_type(&source.mime_types());
                self.owner = Some(source);
            }
            None => {
                self.owner = None;
                self.pending = None;
                self.owner_gone = true;
            }
        }
    }

    /// A RemoteDesktop session took the clipboard: nothing is copied.
    pub(crate) fn forget(&mut self) {
        self.reset();
    }

    /// The saved copy of the current client selection, once finished.
    pub(crate) fn finished(&self) -> Option<Saved> {
        let generation = self.generation.load(Ordering::SeqCst);
        self.saved
            .lock()
            .ok()?
            .as_ref()
            .filter(|saved| saved.generation == generation)
            .cloned()
    }

    /// Read `reader` into the saved slot on a thread of its own, for the
    /// current generation and at most `limit` bytes.
    fn spawn_copy(&self, reader: OwnedFd, mime_type: String, limit: usize) -> std::io::Result<()> {
        let generation = self.generation.load(Ordering::SeqCst);
        let current = self.generation.clone();
        let saved = self.saved.clone();
        std::thread::Builder::new()
            .name("tuna-clip-retain".into())
            .spawn(move || {
                let stop = || current.load(Ordering::SeqCst) != generation;
                let Some(data) = read_copy(&reader, limit, TRANSFER_TIMEOUT, &stop) else {
                    return;
                };
                let Ok(mut slot) = saved.lock() else {
                    return;
                };
                if current.load(Ordering::SeqCst) == generation {
                    *slot = Some(Saved {
                        generation,
                        mime_type,
                        data: data.into(),
                    });
                }
            })
            .map(|_| ())
    }

    /// A client reads the retained selection of `generation`: write the
    /// saved bytes into `fd` on a thread of its own. False (and `fd`
    /// closed, so the reader sees end of file) when that copy is no
    /// longer served, the type differs or `MAX_WRITES` serves run.
    pub(crate) fn serve(&self, generation: u64, mime_type: &str, fd: OwnedFd) -> bool {
        let Some(served) = self
            .served
            .as_ref()
            .filter(|served| served.generation == generation && served.mime_type == mime_type)
        else {
            return false;
        };
        let mut writes = self.writes.load(Ordering::SeqCst);
        loop {
            if writes >= MAX_WRITES {
                return false;
            }
            match self.writes.compare_exchange_weak(
                writes,
                writes + 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => break,
                Err(current) => writes = current,
            }
        }
        let data = served.data.clone();
        let counter = self.writes.clone();
        let spawned = std::thread::Builder::new()
            .name("tuna-clip-serve".into())
            .spawn(move || {
                write_all(&fd, &data, TRANSFER_TIMEOUT);
                counter.fetch_sub(1, Ordering::SeqCst);
            });
        if spawned.is_err() {
            self.writes.fetch_sub(1, Ordering::SeqCst);
            return false;
        }
        true
    }
}

/// Read `src` to end of file, stopping at `limit` bytes with what was
/// read (mutter trims the transfer at its size). `None` when `stop`
/// says so, `idle` passes without data or the read fails.
pub(crate) fn read_copy(
    src: &OwnedFd,
    limit: usize,
    idle: Duration,
    stop: &dyn Fn() -> bool,
) -> Option<Vec<u8>> {
    use rustix::event::PollFlags;
    use rustix::io::Errno;
    nonblocking(src).ok()?;
    let mut data = Vec::new();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut deadline = Instant::now() + idle;
    while data.len() < limit {
        ready(src, PollFlags::IN, stop, deadline).ok()?;
        let want = buffer.len().min(limit - data.len());
        match rustix::io::read(src, &mut buffer[..want]) {
            Ok(0) => return Some(data),
            Ok(read) => {
                data.extend_from_slice(&buffer[..read]);
                deadline = Instant::now() + idle;
            }
            Err(Errno::AGAIN | Errno::INTR) => {}
            Err(_) => return None,
        }
    }
    Some(data)
}

/// Write all of `data` into `dst`, giving up after `idle` without
/// progress or when the reader closes. Whether everything was written.
pub(crate) fn write_all(dst: &OwnedFd, data: &[u8], idle: Duration) -> bool {
    use rustix::event::PollFlags;
    use rustix::io::Errno;
    if nonblocking(dst).is_err() {
        return false;
    }
    let mut written = 0;
    let mut deadline = Instant::now() + idle;
    while written < data.len() {
        if ready(dst, PollFlags::OUT, &|| false, deadline).is_err() {
            return false;
        }
        match rustix::io::write(dst, &data[written..]) {
            Ok(count) => {
                written += count;
                deadline = Instant::now() + idle;
            }
            Err(Errno::AGAIN | Errno::INTR) => {}
            Err(_) => return false,
        }
    }
    true
}

impl crate::State {
    /// After each client dispatch: request the copy of a selection smithay
    /// has now stored, notice an owner that went away, and take the
    /// clipboard over with the finished copy (mutter's `owner_changed_cb`
    /// with no new owner, `meta_selection_source_memory_new`).
    pub(crate) fn retain_clipboard(&mut self) {
        use smithay::wayland::selection::data_device::{
            clear_data_device_selection, request_data_device_client_selection,
            set_data_device_selection,
        };
        let manager = &mut self.clipboard_manager;
        // A destroyed source or a disconnected client: smithay still
        // holds the dead selection until its next broadcast.
        let died = manager.owner.as_ref().is_some_and(|owner| !owner.alive());
        if died {
            manager.owner = None;
            manager.pending = None;
            manager.owner_gone = true;
        }
        if let Some((mime_type, limit)) = manager.pending.take() {
            match std::io::pipe() {
                Ok((reader, writer)) => {
                    let requested = request_data_device_client_selection(
                        &self.seat,
                        mime_type.clone(),
                        writer.into(),
                    );
                    if requested.is_ok() {
                        if let Err(error) = manager.spawn_copy(reader.into(), mime_type, limit) {
                            eprintln!("tuna-compositor: clipboard copy not started: {error}");
                        }
                    }
                }
                Err(error) => eprintln!("tuna-compositor: clipboard copy not started: {error}"),
            }
        }
        if !std::mem::take(&mut manager.owner_gone) {
            return;
        }
        match manager.finished() {
            Some(saved) => {
                let mime_types = vec![saved.mime_type.clone()];
                let generation = saved.generation;
                manager.served = Some(saved);
                set_data_device_selection(
                    &self.dh,
                    &self.seat,
                    mime_types.clone(),
                    CompositorSelection::Retained(generation),
                );
                self.remote_clipboard.client_selection(Some(mime_types));
            }
            None => {
                if died {
                    // Mutter unsets a destroyed owner; tell clients now.
                    clear_data_device_selection(&self.dh, &self.seat);
                }
                self.remote_clipboard.client_selection(None);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offered(types: &[&str]) -> Vec<String> {
        types.iter().map(|t| (*t).to_owned()).collect()
    }

    fn pipe() -> (OwnedFd, OwnedFd) {
        let (reader, writer) = std::io::pipe().unwrap();
        (reader.into(), writer.into())
    }

    #[test]
    fn best_type_is_the_most_preferred_supported_offer() {
        assert_eq!(
            best_mime_type(&offered(&["image/png", "text/plain", "image/jpeg"])),
            Some(("text/plain".to_owned(), MAX_TEXT_SIZE))
        );
        assert_eq!(
            best_mime_type(&offered(&[
                "text/plain;charset=utf-8",
                "text/plain",
                "image/svg+xml"
            ])),
            Some(("text/plain;charset=utf-8".to_owned(), MAX_TEXT_SIZE))
        );
        assert_eq!(
            best_mime_type(&offered(&["image/tiff", "image/png", "image/bmp"])),
            Some(("image/png".to_owned(), MAX_IMAGE_SIZE))
        );
    }

    #[test]
    fn unsupported_or_inexact_types_are_not_copied() {
        assert_eq!(best_mime_type(&offered(&[])), None);
        assert_eq!(
            best_mime_type(&offered(&[
                "application/x-foo",
                "TEXT/PLAIN",
                "text/plain;charset=UTF-8",
                "text/html",
                "image/*"
            ])),
            None
        );
        assert_eq!(
            best_mime_type(&offered(&["application/x-foo", "image/gif"])),
            Some(("image/gif".to_owned(), MAX_IMAGE_SIZE))
        );
    }

    #[test]
    fn copy_reads_to_end_of_file() {
        let (reader, writer) = pipe();
        rustix::io::write(&writer, b"retained bytes").unwrap();
        drop(writer);
        let data = read_copy(&reader, MAX_TEXT_SIZE, Duration::from_secs(5), &|| false);
        assert_eq!(data.as_deref(), Some(&b"retained bytes"[..]));
    }

    #[test]
    fn copy_stops_at_the_limit_keeping_what_it_read() {
        let (reader, writer) = pipe();
        rustix::io::write(&writer, b"0123456789").unwrap();
        // The writer stays open: the read must end at the limit, not EOF.
        let data = read_copy(&reader, 4, Duration::from_secs(5), &|| false);
        assert_eq!(data.as_deref(), Some(&b"0123"[..]));
        drop(writer);
    }

    #[test]
    fn copy_gives_up_when_idle_or_stopped() {
        let (reader, writer) = pipe();
        rustix::io::write(&writer, b"partial").unwrap();
        let started = Instant::now();
        assert_eq!(
            read_copy(&reader, MAX_TEXT_SIZE, Duration::from_millis(200), &|| {
                false
            }),
            None
        );
        assert!(started.elapsed() >= Duration::from_millis(200));
        assert_eq!(
            read_copy(&reader, MAX_TEXT_SIZE, Duration::from_secs(5), &|| true),
            None
        );
        drop(writer);
    }

    #[test]
    fn serve_writes_every_byte_and_reports_a_closed_reader() {
        let (reader, writer) = pipe();
        let data = vec![7u8; 200 * 1024];
        let expected = data.clone();
        let consumer = std::thread::spawn(move || {
            let mut read = Vec::new();
            std::io::Read::read_to_end(&mut std::fs::File::from(reader), &mut read).unwrap();
            read
        });
        assert!(write_all(&writer, &data, Duration::from_secs(5)));
        drop(writer);
        assert_eq!(consumer.join().unwrap(), expected);

        let (reader, writer) = pipe();
        drop(reader);
        assert!(!write_all(&writer, b"nobody reads", Duration::from_secs(5)));
    }
}
