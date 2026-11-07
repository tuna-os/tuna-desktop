//! The RemoteDesktop session clipboard, as mutter 51 serves it to
//! gnome-remote-desktop: `meta-clipboard-session.c` (Enable, Disable,
//! SetSelection, SelectionWrite, SelectionWriteDone, SelectionRead and the
//! SelectionOwnerChanged/SelectionTransfer signals), re-emitted on the
//! session by `meta-remote-desktop-session.c` (`on_clipboard_owner_changed`
//! wraps `mime-types` in a one-field tuple, which `grd-session.c` reads as
//! `(as)`).
//!
//! The Wayland side is smithay's data device: a remote selection is a
//! compositor-provided selection whose user data is the session id, and a
//! local client's selection is read through its source. Data never crosses
//! the compositor loop: each transfer is a pipe pumped on its own thread,
//! bounded in size, idle time and count, and cut off when the grant is
//! revoked (stop, owner loss, lock) or the clipboard is disabled.

use std::collections::{HashMap, VecDeque};
use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use zbus::fdo;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedValue, StructureBuilder, Value};

/// Mime types one selection may offer.
pub(crate) const MAX_MIME_TYPES: usize = 64;
/// Bytes in one mime type name.
pub(crate) const MAX_MIME_LEN: usize = 255;
/// Unanswered SelectionTransfer requests kept per session; older ones
/// are cancelled (their reader sees end of file).
pub(crate) const MAX_PENDING: usize = 16;
/// Concurrent SelectionWrite pipes per session.
pub(crate) const MAX_WRITES: u32 = 8;
/// Bytes one transfer may carry in either direction.
pub(crate) const TRANSFER_LIMIT: u64 = 256 * 1024 * 1024;
/// mutter's `TRANSFER_REQUEST_CLEANUP_TIMEOUT_MS`: an unanswered transfer
/// request is cancelled after 15 seconds. The same bound ends a transfer
/// that makes no progress.
pub(crate) const TRANSFER_TIMEOUT: Duration = Duration::from_secs(15);

/// One session's clipboard state, shared by the D-Bus thread and the loop.
#[derive(Default)]
pub struct Shared {
    enabled: AtomicBool,
    /// Bumped on Enable/Disable so pumps of an earlier enablement stop.
    generation: AtomicU32,
    pending: Mutex<Pending>,
    reading: AtomicBool,
    writes: AtomicU32,
}

#[derive(Default)]
struct Pending {
    serial: u32,
    requests: VecDeque<(u32, OwnedFd, Instant)>,
}

impl Pending {
    fn expire(&mut self, now: Instant) {
        self.requests
            .retain(|(_, _, at)| now.duration_since(*at) < TRANSFER_TIMEOUT);
    }
}

impl Shared {
    pub(crate) fn enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }
    /// EnableClipboard: false when it already was (mutter's "Already enabled").
    pub(crate) fn enable(&self) -> bool {
        if self.enabled.swap(true, Ordering::SeqCst) {
            return false;
        }
        self.generation.fetch_add(1, Ordering::SeqCst);
        true
    }
    /// DisableClipboard: false when it was not enabled. Cancels every
    /// pending request and in-flight transfer.
    pub(crate) fn disable(&self) -> bool {
        if !self.enabled.swap(false, Ordering::SeqCst) {
            return false;
        }
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.cancel();
        true
    }
    /// Drop unanswered transfer requests (their local readers see EOF).
    pub(crate) fn cancel(&self) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.requests.clear();
        }
    }
    /// A local client asked for this session's selection: queue its pipe
    /// and return the serial SelectionTransfer announces.
    pub(crate) fn request(&self, fd: OwnedFd, now: Instant) -> Option<u32> {
        let mut pending = self.pending.lock().ok()?;
        pending.expire(now);
        pending.serial = pending.serial.wrapping_add(1);
        let serial = pending.serial;
        pending.requests.push_back((serial, fd, now));
        while pending.requests.len() > MAX_PENDING {
            pending.requests.pop_front();
        }
        Some(serial)
    }
    /// SelectionWrite: the local reader waiting on `serial`.
    pub(crate) fn take(&self, serial: u32, now: Instant) -> Option<OwnedFd> {
        let mut pending = self.pending.lock().ok()?;
        pending.expire(now);
        let index = pending.requests.iter().position(|(s, _, _)| *s == serial)?;
        pending.requests.remove(index).map(|(_, fd, _)| fd)
    }
    /// Whether a pump started under `generation` must stop.
    fn cancelled(&self, generation: u32) -> bool {
        !self.enabled() || self.generation.load(Ordering::SeqCst) != generation
    }
}

/// The `mime-types` option of EnableClipboard and SetSelection: absent,
/// or a non-empty bounded list of printable names.
pub(crate) fn mime_types(
    options: &HashMap<String, OwnedValue>,
) -> fdo::Result<Option<Vec<String>>> {
    let Some(value) = options.get("mime-types") else {
        return Ok(None);
    };
    let Value::Array(array) = &**value else {
        return Err(fdo::Error::InvalidArgs(
            "mime-types must be a string array".into(),
        ));
    };
    if array.is_empty() {
        return Err(fdo::Error::InvalidArgs(
            "No mime types in mime types list".into(),
        ));
    }
    if array.len() > MAX_MIME_TYPES {
        return Err(fdo::Error::LimitsExceeded("too many mime types".into()));
    }
    array
        .iter()
        .map(|value| match value {
            Value::Str(name) => valid_mime_type(name.as_str()).map(str::to_owned),
            _ => Err(fdo::Error::InvalidArgs(
                "mime-types must be a string array".into(),
            )),
        })
        .collect::<fdo::Result<Vec<_>>>()
        .map(Some)
}

pub(crate) fn valid_mime_type(name: &str) -> fdo::Result<&str> {
    if name.is_empty()
        || name.len() > MAX_MIME_LEN
        || name.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return Err(fdo::Error::InvalidArgs("invalid mime type".into()));
    }
    Ok(name)
}

/// SelectionOwnerChanged's options: `mime-types` as `(as)` and
/// `session-is-owner`, or nothing when the selection has no owner.
pub(crate) fn owner_changed_options(
    owner: Option<(&[String], bool)>,
) -> HashMap<String, OwnedValue> {
    let mut options = HashMap::new();
    let Some((mime_types, is_owner)) = owner else {
        return options;
    };
    if let Ok(tuple) = StructureBuilder::new()
        .add_field(mime_types.to_vec())
        .build()
    {
        if let Ok(value) = OwnedValue::try_from(Value::Structure(tuple)) {
            options.insert("mime-types".into(), value);
        }
    }
    options.insert("session-is-owner".into(), OwnedValue::from(is_owner));
    options
}

/// How a pumped transfer ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PumpEnd {
    Done(u64),
    Revoked,
    TooLarge,
    Idle,
    Closed,
    Failed,
}

fn nonblocking(fd: &OwnedFd) -> rustix::io::Result<()> {
    let flags = rustix::fs::fcntl_getfl(fd)?;
    rustix::fs::fcntl_setfl(fd, flags | rustix::fs::OFlags::NONBLOCK)
}

fn ready(
    fd: &OwnedFd,
    flags: rustix::event::PollFlags,
    stop: &dyn Fn() -> bool,
    deadline: Instant,
) -> Result<(), PumpEnd> {
    loop {
        if stop() {
            return Err(PumpEnd::Revoked);
        }
        if Instant::now() >= deadline {
            return Err(PumpEnd::Idle);
        }
        let mut poll = [rustix::event::PollFd::new(fd, flags)];
        let tick = rustix::event::Timespec {
            tv_sec: 0,
            tv_nsec: 100_000_000,
        };
        match rustix::event::poll(&mut poll, Some(&tick)) {
            Ok(0) | Err(rustix::io::Errno::INTR) => continue,
            Ok(_) => return Ok(()),
            Err(_) => return Err(PumpEnd::Failed),
        }
    }
}

/// Copy `src` to `dst` until end of file, at most `limit` bytes, giving
/// up after `idle` without progress or as soon as `stop` says so.
pub(crate) fn pump(
    src: OwnedFd,
    dst: OwnedFd,
    limit: u64,
    idle: Duration,
    stop: &dyn Fn() -> bool,
) -> PumpEnd {
    use rustix::event::PollFlags;
    use rustix::io::Errno;
    if nonblocking(&src).is_err() || nonblocking(&dst).is_err() {
        return PumpEnd::Failed;
    }
    let mut buffer = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    let mut deadline = Instant::now() + idle;
    loop {
        if let Err(end) = ready(&src, PollFlags::IN, stop, deadline) {
            return end;
        }
        let read = match rustix::io::read(&src, &mut buffer[..]) {
            Ok(0) => return PumpEnd::Done(total),
            Ok(read) => read,
            Err(Errno::AGAIN | Errno::INTR) => continue,
            Err(_) => return PumpEnd::Failed,
        };
        total += read as u64;
        if total > limit {
            return PumpEnd::TooLarge;
        }
        deadline = Instant::now() + idle;
        let mut written = 0;
        while written < read {
            if let Err(end) = ready(&dst, PollFlags::OUT, stop, deadline) {
                return end;
            }
            match rustix::io::write(&dst, &buffer[written..read]) {
                Ok(count) => {
                    written += count;
                    deadline = Instant::now() + idle;
                }
                Err(Errno::AGAIN | Errno::INTR) => {}
                Err(Errno::PIPE) => return PumpEnd::Closed,
                Err(_) => return PumpEnd::Failed,
            }
        }
    }
}

/// Pump on a thread of its own for one session's grant.
fn spawn(
    what: &'static str,
    src: OwnedFd,
    dst: OwnedFd,
    stopped: Arc<AtomicBool>,
    shared: Arc<Shared>,
    finished: impl FnOnce(&Shared) + Send + 'static,
) -> fdo::Result<()> {
    let generation = shared.generation.load(Ordering::SeqCst);
    std::thread::Builder::new()
        .name(format!("tuna-clip-{what}"))
        .spawn(move || {
            let stop = || stopped.load(Ordering::SeqCst) || shared.cancelled(generation);
            let end = pump(src, dst, TRANSFER_LIMIT, TRANSFER_TIMEOUT, &stop);
            if !matches!(end, PumpEnd::Done(_)) {
                eprintln!("tuna-compositor: remote clipboard {what} ended: {end:?}");
            }
            finished(&shared);
        })
        .map(|_| ())
        .map_err(|e| fdo::Error::Failed(e.to_string()))
}

fn pipe() -> fdo::Result<(OwnedFd, OwnedFd)> {
    let (reader, writer) = std::io::pipe().map_err(|e| fdo::Error::Failed(e.to_string()))?;
    Ok((reader.into(), writer.into()))
}

/// SelectionWrite: a pipe whose write end goes to the session, pumped
/// into the local reader that asked for `serial`.
pub(crate) fn selection_write(
    shared: &Arc<Shared>,
    stopped: &Arc<AtomicBool>,
    serial: u32,
) -> fdo::Result<OwnedFd> {
    let reader = shared.take(serial, Instant::now()).ok_or_else(|| {
        fdo::Error::Failed(format!(
            "Transfer serial {serial} doesn't match any transfer request"
        ))
    })?;
    let mut writes = shared.writes.load(Ordering::SeqCst);
    loop {
        if writes >= MAX_WRITES {
            return Err(fdo::Error::LimitsExceeded(
                "too many clipboard writes in flight".into(),
            ));
        }
        match shared.writes.compare_exchange_weak(
            writes,
            writes + 1,
            Ordering::SeqCst,
            Ordering::SeqCst,
        ) {
            Ok(_) => break,
            Err(current) => writes = current,
        }
    }
    let result = pipe().and_then(|(source, sink)| {
        spawn(
            "write",
            source,
            reader,
            stopped.clone(),
            shared.clone(),
            |shared| {
                shared.writes.fetch_sub(1, Ordering::SeqCst);
            },
        )
        .map(|()| sink)
    });
    if result.is_err() {
        shared.writes.fetch_sub(1, Ordering::SeqCst);
    }
    result
}

/// SelectionRead's pipes: the loop hands `local` to the selection
/// owner; the session reads the returned descriptor. One read at a time
/// (mutter's "Tried to read in parallel").
pub(crate) struct ReadPipes {
    pub local: OwnedFd,
    source: OwnedFd,
}

pub(crate) fn begin_read(shared: &Shared) -> fdo::Result<ReadPipes> {
    if shared.reading.swap(true, Ordering::SeqCst) {
        return Err(fdo::Error::Failed("Tried to read in parallel".into()));
    }
    match pipe() {
        Ok((source, local)) => Ok(ReadPipes { local, source }),
        Err(error) => {
            shared.reading.store(false, Ordering::SeqCst);
            Err(error)
        }
    }
}

/// The owner accepted the request: pump its data to the session.
pub(crate) fn finish_read(
    shared: &Arc<Shared>,
    stopped: &Arc<AtomicBool>,
    source: OwnedFd,
) -> fdo::Result<OwnedFd> {
    let result = pipe().and_then(|(session, sink)| {
        spawn(
            "read",
            source,
            sink,
            stopped.clone(),
            shared.clone(),
            |shared| shared.reading.store(false, Ordering::SeqCst),
        )
        .map(|()| session)
    });
    if result.is_err() {
        shared.reading.store(false, Ordering::SeqCst);
    }
    result
}

impl ReadPipes {
    /// Split into the owner's descriptor and the pump's source.
    pub(crate) fn split(self) -> (OwnedFd, OwnedFd) {
        (self.local, self.source)
    }
}

/// The owner refused or the request never reached it.
pub(crate) fn abort_read(shared: &Shared) {
    shared.reading.store(false, Ordering::SeqCst);
}

/// Requests from the session's D-Bus methods to the compositor loop.
pub enum Request {
    Enable {
        session_id: u64,
        stopped: Arc<AtomicBool>,
        shared: Arc<Shared>,
        signal: SignalEmitter<'static>,
        mime_types: Option<Vec<String>>,
    },
    Disable {
        session_id: u64,
    },
    SetSelection {
        session_id: u64,
        mime_types: Option<Vec<String>>,
    },
    Read {
        session_id: u64,
        mime_type: String,
        fd: OwnedFd,
        reply: std::sync::mpsc::Sender<fdo::Result<()>>,
    },
}

/// Who owns the clipboard, as the remote sessions see it.
#[derive(Debug, Clone, PartialEq)]
enum Owner {
    Client(Vec<String>),
    Remote(u64, Vec<String>),
}

struct Grant {
    stopped: Arc<AtomicBool>,
    shared: Arc<Shared>,
    signal: SignalEmitter<'static>,
}

/// The loop's view: which sessions have the clipboard enabled and who
/// owns the selection.
#[derive(Default)]
pub struct Registry {
    grants: HashMap<u64, Grant>,
    owner: Option<Owner>,
}

impl Registry {
    fn live(&self, session_id: u64) -> Option<&Grant> {
        self.grants
            .get(&session_id)
            .filter(|g| g.shared.enabled() && !g.stopped.load(Ordering::SeqCst))
    }

    fn options_for(&self, session_id: u64) -> HashMap<String, OwnedValue> {
        owner_changed_options(match &self.owner {
            None => None,
            Some(Owner::Client(types)) => Some((types, false)),
            Some(Owner::Remote(owner, types)) => Some((types, *owner == session_id)),
        })
    }

    fn notify(&self, session_id: u64) {
        if let Some(grant) = self.live(session_id) {
            let options = self.options_for(session_id);
            let _ = zbus::block_on(
                super::remote_desktop::RemoteSession::selection_owner_changed(
                    &grant.signal,
                    options,
                ),
            );
        }
    }

    fn broadcast(&self) {
        for id in self.grants.keys() {
            self.notify(*id);
        }
    }

    /// A local client set (or cleared) the clipboard.
    pub(crate) fn client_selection(&mut self, mime_types: Option<Vec<String>>) {
        self.owner = mime_types.map(Owner::Client);
        self.broadcast();
    }

    /// The session owning the compositor selection, if a remote one does.
    pub(crate) fn remote_owner(&self) -> Option<u64> {
        match self.owner {
            Some(Owner::Remote(id, _)) => Some(id),
            _ => None,
        }
    }

    /// A local client asked for `session_id`'s selection: hand its pipe
    /// to the session through SelectionTransfer. Without a live grant
    /// the pipe closes and the reader sees end of file.
    pub(crate) fn transfer(&self, session_id: u64, mime_type: &str, fd: OwnedFd) {
        let Some(grant) = self.live(session_id) else {
            return;
        };
        if let Some(serial) = grant.shared.request(fd, Instant::now()) {
            let _ = zbus::block_on(super::remote_desktop::RemoteSession::selection_transfer(
                &grant.signal,
                mime_type,
                serial,
            ));
        }
    }

    /// What a request asks of the seat's selection.
    pub(crate) fn handle(&mut self, request: Request) -> Effect {
        match request {
            Request::Enable {
                session_id,
                stopped,
                shared,
                signal,
                mime_types,
            } => {
                // Stop raced ahead of this request: nothing to register.
                if stopped.load(Ordering::SeqCst) {
                    shared.disable();
                    return Effect::None;
                }
                self.grants.insert(
                    session_id,
                    Grant {
                        stopped,
                        shared,
                        signal,
                    },
                );
                match mime_types {
                    Some(types) => self.set_remote(session_id, types),
                    None => {
                        if self.owner.is_some() {
                            self.notify(session_id);
                        }
                        Effect::None
                    }
                }
            }
            Request::Disable { session_id } => self.remove(session_id),
            Request::SetSelection {
                session_id,
                mime_types,
            } => {
                let Some(grant) = self.live(session_id) else {
                    return Effect::None;
                };
                grant.shared.cancel();
                match mime_types {
                    Some(types) => self.set_remote(session_id, types),
                    None => self.unset_remote(session_id),
                }
            }
            Request::Read {
                session_id,
                mime_type,
                fd,
                reply,
            } => {
                if self.live(session_id).is_none() {
                    let _ = reply.send(Err(fdo::Error::Failed("Clipboard not enabled".into())));
                    return Effect::None;
                }
                match &self.owner {
                    None => {
                        let _ = reply.send(Err(fdo::Error::Failed(
                            "No selection owner available".into(),
                        )));
                        Effect::None
                    }
                    Some(Owner::Remote(owner, _)) if *owner == session_id => {
                        let _ = reply.send(Err(fdo::Error::Failed(
                            "Tried to read own selection".into(),
                        )));
                        Effect::None
                    }
                    _ => Effect::Read {
                        mime_type,
                        fd,
                        reply,
                    },
                }
            }
        }
    }

    fn set_remote(&mut self, session_id: u64, mime_types: Vec<String>) -> Effect {
        self.owner = Some(Owner::Remote(session_id, mime_types.clone()));
        self.broadcast();
        Effect::Own {
            session_id,
            mime_types,
        }
    }

    fn unset_remote(&mut self, session_id: u64) -> Effect {
        if self.remote_owner() != Some(session_id) {
            return Effect::None;
        }
        self.owner = None;
        self.broadcast();
        Effect::Clear
    }

    /// Disable, stop, owner loss or lock: forget the session, cancel its
    /// transfers and give up a selection it owns.
    pub(crate) fn remove(&mut self, session_id: u64) -> Effect {
        let Some(grant) = self.grants.remove(&session_id) else {
            return Effect::None;
        };
        grant.shared.disable();
        grant.shared.cancel();
        self.unset_remote(session_id)
    }

    pub(crate) fn sessions(&self) -> Vec<u64> {
        self.grants.keys().copied().collect()
    }
}

/// What the loop must do to the seat's selection after a request.
pub(crate) enum Effect {
    None,
    /// Offer the session's mime types as the compositor selection.
    Own {
        session_id: u64,
        mime_types: Vec<String>,
    },
    /// Clear the compositor selection the session owned.
    Clear,
    /// Ask the current client owner to write `mime_type` into `fd`.
    Read {
        mime_type: String,
        fd: OwnedFd,
        reply: std::sync::mpsc::Sender<fdo::Result<()>>,
    },
}

impl crate::State {
    /// One clipboard request on the loop. While locked nothing reaches
    /// the seat's selection (the lock also revokes the grant).
    pub(crate) fn remote_clipboard_request(&mut self, request: Request, locked: bool) {
        let effect = match request {
            Request::Disable { session_id } => self.remote_clipboard.remove(session_id),
            Request::Read { reply, .. } if locked => {
                let _ = reply.send(Err(crate::capture_security::denied("session locked")));
                Effect::None
            }
            _ if locked => Effect::None,
            request => self.remote_clipboard.handle(request),
        };
        self.apply_remote_clipboard(effect);
    }

    /// The session's grant ended (Stop, owner loss, lock): its selection
    /// and every transfer go with it.
    pub(crate) fn revoke_remote_clipboard(&mut self, session_id: u64) {
        let effect = self.remote_clipboard.remove(session_id);
        self.apply_remote_clipboard(effect);
    }

    fn apply_remote_clipboard(&mut self, effect: Effect) {
        use smithay::wayland::selection::data_device::{
            clear_data_device_selection, request_data_device_client_selection,
            set_data_device_selection, SelectionRequestError,
        };
        match effect {
            Effect::None => {}
            Effect::Own {
                session_id,
                mime_types,
            } => set_data_device_selection(&self.dh, &self.seat, mime_types, session_id),
            Effect::Clear => clear_data_device_selection(&self.dh, &self.seat),
            Effect::Read {
                mime_type,
                fd,
                reply,
            } => {
                let result = request_data_device_client_selection(&self.seat, mime_type, fd)
                    .map_err(|error| {
                        fdo::Error::Failed(
                            match error {
                                SelectionRequestError::NoSelection => {
                                    "No selection owner available"
                                }
                                SelectionRequestError::InvalidMimetype => {
                                    "The selection does not offer this mime type"
                                }
                                SelectionRequestError::ServerSideSelection => {
                                    "The selection belongs to another remote session"
                                }
                            }
                            .into(),
                        )
                    });
                let _ = reply.send(result);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    fn options(value: Value<'_>) -> HashMap<String, OwnedValue> {
        HashMap::from([(
            "mime-types".to_owned(),
            OwnedValue::try_from(value).unwrap(),
        )])
    }

    #[test]
    fn mime_type_options_are_bounded_and_typed() {
        assert_eq!(mime_types(&HashMap::new()).unwrap(), None);
        let ok = options(Value::from(vec!["text/plain;charset=utf-8", "UTF8_STRING"]));
        assert_eq!(
            mime_types(&ok).unwrap().unwrap(),
            vec!["text/plain;charset=utf-8", "UTF8_STRING"]
        );
        // Garbage: wrong type, empty list, empty/oversized/control names,
        // too many entries.
        assert!(mime_types(&options(Value::from(7u32))).is_err());
        assert!(mime_types(&options(Value::from(Vec::<String>::new()))).is_err());
        assert!(mime_types(&options(Value::from(vec![""]))).is_err());
        assert!(mime_types(&options(Value::from(vec!["a\nb"]))).is_err());
        assert!(mime_types(&options(Value::from(vec!["x".repeat(MAX_MIME_LEN + 1)]))).is_err());
        assert!(mime_types(&options(Value::from(vec![1u32, 2]))).is_err());
        let many: Vec<String> = (0..=MAX_MIME_TYPES).map(|i| format!("a/{i}")).collect();
        assert!(matches!(
            mime_types(&options(Value::from(many))),
            Err(fdo::Error::LimitsExceeded(_))
        ));
    }

    #[test]
    fn owner_changed_wraps_mime_types_in_a_tuple_like_mutter() {
        let types = vec!["text/plain".to_owned()];
        let options = owner_changed_options(Some((&types, true)));
        let value = options.get("mime-types").unwrap();
        assert_eq!(value.value_signature().to_string(), "(as)");
        assert!(bool::try_from(options.get("session-is-owner").unwrap()).unwrap());
        assert!(owner_changed_options(None).is_empty());
    }

    #[test]
    fn transfer_requests_match_serials_expire_and_are_bounded() {
        let shared = Shared::default();
        let now = Instant::now();
        let (_r, w) = pipe().unwrap();
        let first = shared.request(w, now).unwrap();
        assert_eq!(first, 1);
        // A serial nobody asked for, or one already taken, is refused.
        assert!(shared.take(first + 1, now).is_none());
        assert!(shared.take(first, now).is_some());
        assert!(shared.take(first, now).is_none());
        // Unanswered requests expire after mutter's 15 s.
        let (_r, w) = pipe().unwrap();
        let old = shared.request(w, now).unwrap();
        assert!(shared.take(old, now + TRANSFER_TIMEOUT).is_none());
        // At most MAX_PENDING stay queued; the oldest are cancelled.
        let mut readers = Vec::new();
        let mut serials = Vec::new();
        for _ in 0..MAX_PENDING + 2 {
            let (r, w) = pipe().unwrap();
            readers.push(r);
            serials.push(shared.request(w, now).unwrap());
        }
        assert!(shared.take(serials[0], now).is_none());
        assert!(shared.take(serials[1], now).is_none());
        assert!(shared.take(serials[2], now).is_some());
        // The cancelled request's local reader sees end of file.
        let mut reader = std::fs::File::from(readers.remove(0));
        let mut data = Vec::new();
        assert_eq!(reader.read_to_end(&mut data).unwrap(), 0);
    }

    #[test]
    fn pump_copies_exactly_and_stops_on_limit_revocation_idle_and_close() {
        let never = || false;
        // Exact copy.
        let (src_r, src_w) = pipe().unwrap();
        let (dst_r, dst_w) = pipe().unwrap();
        let writer = std::thread::spawn(move || {
            let mut w = std::fs::File::from(src_w);
            w.write_all(&vec![7u8; 300_000]).unwrap();
        });
        let reader = std::thread::spawn(move || {
            let mut data = Vec::new();
            std::fs::File::from(dst_r).read_to_end(&mut data).unwrap();
            data
        });
        assert_eq!(
            pump(src_r, dst_w, 1 << 20, Duration::from_secs(5), &never),
            PumpEnd::Done(300_000)
        );
        writer.join().unwrap();
        assert_eq!(reader.join().unwrap(), vec![7u8; 300_000]);

        // Oversized: cut off without forwarding the overflow.
        let (src_r, src_w) = pipe().unwrap();
        let (dst_r, dst_w) = pipe().unwrap();
        let writer = std::thread::spawn(move || {
            let _ = std::fs::File::from(src_w).write_all(&vec![1u8; 200_000]);
        });
        let reader = std::thread::spawn(move || {
            let mut data = Vec::new();
            std::fs::File::from(dst_r).read_to_end(&mut data).unwrap();
            data.len()
        });
        assert_eq!(
            pump(src_r, dst_w, 100_000, Duration::from_secs(5), &never),
            PumpEnd::TooLarge
        );
        writer.join().unwrap();
        assert!(reader.join().unwrap() <= 100_000);

        // Revoked mid-transfer: the writer stalls, the grant stops.
        let (src_r, _src_w) = pipe().unwrap();
        let (_dst_r, dst_w) = pipe().unwrap();
        let revoked = AtomicBool::new(false);
        let started = Instant::now();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(150));
                revoked.store(true, Ordering::SeqCst);
            });
            let stop = || revoked.load(Ordering::SeqCst);
            assert_eq!(
                pump(src_r, dst_w, 1 << 20, Duration::from_secs(30), &stop),
                PumpEnd::Revoked
            );
        });
        assert!(started.elapsed() < Duration::from_secs(5));

        // A peer that sends nothing times out.
        let (src_r, _src_w) = pipe().unwrap();
        let (_dst_r, dst_w) = pipe().unwrap();
        assert_eq!(
            pump(src_r, dst_w, 1 << 20, Duration::from_millis(200), &never),
            PumpEnd::Idle
        );

        // The reader went away.
        let (src_r, src_w) = pipe().unwrap();
        let (dst_r, dst_w) = pipe().unwrap();
        drop(dst_r);
        std::fs::File::from(src_w).write_all(b"hello").unwrap();
        assert_eq!(
            pump(src_r, dst_w, 1 << 20, Duration::from_secs(5), &never),
            PumpEnd::Closed
        );
    }

    #[test]
    fn writes_need_a_matching_request_are_bounded_and_end_on_revocation() {
        let shared = Arc::new(Shared::default());
        assert!(shared.enable());
        assert!(!shared.enable());
        let stopped = Arc::new(AtomicBool::new(false));
        assert!(selection_write(&shared, &stopped, 42).is_err());
        // A real round trip: remote data reaches the local reader.
        let (local_r, local_w) = pipe().unwrap();
        let serial = shared.request(local_w, Instant::now()).unwrap();
        let sink = selection_write(&shared, &stopped, serial).unwrap();
        std::fs::File::from(sink).write_all(b"remote text").unwrap();
        let mut data = String::new();
        std::fs::File::from(local_r)
            .read_to_string(&mut data)
            .unwrap();
        assert_eq!(data, "remote text");
        // In-flight writes are bounded.
        let mut held = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        while shared.writes.load(Ordering::SeqCst) != 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        for _ in 0..MAX_WRITES {
            let (r, w) = pipe().unwrap();
            let serial = shared.request(w, Instant::now()).unwrap();
            held.push((r, selection_write(&shared, &stopped, serial).unwrap()));
        }
        let (_r, w) = pipe().unwrap();
        let serial = shared.request(w, Instant::now()).unwrap();
        assert!(matches!(
            selection_write(&shared, &stopped, serial),
            Err(fdo::Error::LimitsExceeded(_))
        ));
        // Revocation ends every pump; the local readers see end of file.
        stopped.store(true, Ordering::SeqCst);
        for (reader, _sink) in held {
            let mut data = Vec::new();
            std::fs::File::from(reader).read_to_end(&mut data).unwrap();
            assert!(data.is_empty());
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while shared.writes.load(Ordering::SeqCst) != 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(shared.writes.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn reads_are_serialized_and_disable_cancels_them() {
        let shared = Arc::new(Shared::default());
        assert!(shared.enable());
        let stopped = Arc::new(AtomicBool::new(false));
        let pipes = begin_read(&shared).unwrap();
        assert!(begin_read(&shared).is_err(), "parallel read refused");
        let (local, source) = pipes.split();
        let session = finish_read(&shared, &stopped, source).unwrap();
        // The owner writes; the session reads it.
        std::fs::File::from(local).write_all(b"local text").unwrap();
        let mut data = String::new();
        std::fs::File::from(session)
            .read_to_string(&mut data)
            .unwrap();
        assert_eq!(data, "local text");
        let deadline = Instant::now() + Duration::from_secs(5);
        while shared.reading.load(Ordering::SeqCst) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        // A stalled read ends when the clipboard is disabled.
        let (_local, source) = begin_read(&shared).unwrap().split();
        let session = finish_read(&shared, &stopped, source).unwrap();
        assert!(shared.disable());
        assert!(!shared.disable());
        let mut data = Vec::new();
        std::fs::File::from(session).read_to_end(&mut data).unwrap();
        assert!(data.is_empty());
    }
}
