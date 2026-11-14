//! X11 property IO stays on a worker; only compositor-mapped identities are queried.
//!
//! The worker holds its own connection to the X server and selects
//! `PropertyChangeMask` on every window it is told to follow, as Mutter
//! reloads a window's icon on `PropertyNotify`: an icon is read when its
//! window is first named and again when `_NET_WM_ICON` or `WM_HINTS`
//! changes on it (#503). Nothing is probed on a timer; only a server that
//! did not answer is asked again, [`RETRY`] later.
use crate::window_icons::Raster;
use smithay::reexports::x11rb::{
    connection::Connection,
    protocol::{
        xproto::{
            AtomEnum, ChangeWindowAttributesAux, ConnectionExt, EventMask, ImageFormat, ImageOrder,
            VisualClass,
        },
        Event,
    },
    rust_connection::RustConnection,
};
use std::{
    collections::HashMap,
    io::{Read, Write},
    os::{fd::AsFd, unix::net::UnixStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
const MAX_WORDS: u32 = 1_048_576;
/// Most windows followed at once.
const MAX_WINDOWS: usize = 512;
/// How soon a server that did not answer is asked again, and the least
/// time between two reads of icons that keep changing: the period of the
/// probe this replaced, so a client rewriting its icon in a loop costs no
/// more than it did.
const RETRY: Duration = Duration::from_millis(500);
/// The windows to follow on a display: (X11 id, compositor id), sorted.
type Request = (u32, Vec<(u32, u64)>);
type Icon = (u32, u64, Option<Raster>);
#[derive(Default)]
struct Shared {
    /// The newest request the worker has not taken yet.
    request: Mutex<Option<Request>>,
    /// Icons read since the compositor last looked, the newest per window.
    icons: Mutex<HashMap<(u32, u64), Option<Raster>>>,
    /// The server did not answer for the windows last named.
    failed: AtomicBool,
}
pub struct Reader {
    wake: crate::wake::Wake,
    shared: Arc<Shared>,
    /// Wakes the worker for a request; closing it ends the worker.
    notify: UnixStream,
    /// What the worker was last asked to follow.
    sent: Option<Request>,
    /// When the windows a server did not answer for are named again.
    retry_at: Option<Instant>,
}
impl Reader {
    pub fn new() -> Self {
        let wake = crate::wake::Wake::default();
        let shared = Arc::new(Shared::default());
        let (notify, notified) = UnixStream::pair().expect("X11 icon worker wake");
        for end in [&notify, &notified] {
            end.set_nonblocking(true).expect("X11 icon worker wake");
        }
        let worker = Worker {
            wake: wake.clone(),
            shared: shared.clone(),
            notified,
        };
        std::thread::Builder::new()
            .name("tuna-x11-icons".into())
            .spawn(move || worker.run())
            .expect("X11 icon worker");
        Self {
            wake,
            shared,
            notify,
            sent: None,
            retry_at: None,
        }
    }
    pub(crate) fn set_wake(&self, wake: &crate::wake::Wake) {
        self.wake.connect(wake);
    }
    /// When windows a server did not answer for are due again; `None`
    /// while the server answers, so a healthy reader asks for no wake.
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.retry_at
    }
    /// The X server went away: its successor's windows are named afresh.
    pub(crate) fn reset(&mut self) {
        self.sent = None;
        self.retry_at = None;
        self.shared.failed.store(false, Ordering::Release);
    }
    /// Follow `windows` on `display` and return the icons read since the
    /// last call: each window's once it is first named, then again each
    /// time it changes. The worker hears of the windows only when they
    /// change, so an unchanged session costs it nothing.
    pub fn poll(&mut self, display: u32, mut windows: Vec<(u32, u64)>) -> Vec<Icon> {
        let now = Instant::now();
        if self.shared.failed.swap(false, Ordering::AcqRel) {
            self.sent = None;
            self.retry_at = Some(now + RETRY);
        }
        // The identities come out of a hash map: their order is no change.
        windows.sort_unstable();
        windows.truncate(MAX_WINDOWS);
        let current = self
            .sent
            .as_ref()
            .is_some_and(|(sent, followed)| *sent == display && *followed == windows);
        if !current && self.retry_at.is_none_or(|at| now >= at) {
            self.retry_at = None;
            let request = (display, windows);
            if let Ok(mut slot) = self.shared.request.lock() {
                *slot = Some(request.clone());
            }
            self.sent = Some(request);
            // A full wake buffer already promises the worker a wake.
            let _ = (&self.notify).write(&[0]);
        }
        self.shared
            .icons
            .lock()
            .map(|mut icons| {
                icons
                    .drain()
                    .map(|((xid, id), raster)| (xid, id, raster))
                    .collect()
            })
            .unwrap_or_default()
    }
}
struct Worker {
    wake: crate::wake::Wake,
    shared: Arc<Shared>,
    notified: UnixStream,
}
impl Worker {
    fn run(self) {
        let mut session: Option<Session> = None;
        // Followed windows whose icon changed and was not read again yet.
        let mut changed: Vec<(u32, u64)> = Vec::new();
        let mut read_at: Option<Instant> = None;
        loop {
            let request = self
                .shared
                .request
                .lock()
                .ok()
                .and_then(|mut slot| slot.take());
            if let Some((display, windows)) = request {
                match follow(&mut session, display, &windows) {
                    Some(icons) => self.publish(icons),
                    None => self.fail(),
                }
            }
            // The replies above may have arrived with events behind them:
            // take every queued one before sleeping on the socket.
            if session
                .as_ref()
                .is_some_and(|live| !live.changed(&mut changed))
            {
                self.lose(&mut session);
            }
            if session.is_none() {
                changed.clear();
            }
            let now = Instant::now();
            let held = read_at
                .map(|at| at + RETRY)
                .filter(|until| *until > now && !changed.is_empty());
            if !changed.is_empty() && held.is_none() {
                match session.as_ref().and_then(|live| live.read(&changed)) {
                    Some(icons) => self.publish(icons),
                    None => self.lose(&mut session),
                }
                changed.clear();
                read_at = Some(now);
                continue;
            }
            match self.sleep(session.as_ref(), held.map(|until| until - now)) {
                None => return,
                Some(true) => self.lose(&mut session),
                Some(false) => {}
            }
        }
    }
    fn publish(&self, icons: Vec<Icon>) {
        if icons.is_empty() {
            return;
        }
        if let Ok(mut held) = self.shared.icons.lock() {
            for (xid, id, raster) in icons {
                held.insert((xid, id), raster);
            }
        }
        self.wake.signal();
    }
    /// The windows last named got no answer: the compositor names them
    /// again [`RETRY`] later, while it still has a server for them.
    fn fail(&self) {
        self.shared.failed.store(true, Ordering::Release);
        self.wake.signal();
    }
    /// The connection is gone. The worker never reconnects on its own:
    /// the display may by now be a socket that starts a server on connect.
    fn lose(&self, session: &mut Option<Session>) {
        if session.take().is_some_and(|live| !live.followed.is_empty()) {
            self.fail();
        }
    }
    /// Sleep until a request, an event from the server or `timeout`
    /// (`None`: no limit). `None` once the reader is gone, else whether
    /// the server hung up.
    fn sleep(&self, session: Option<&Session>, timeout: Option<Duration>) -> Option<bool> {
        use rustix::event::{PollFd, PollFlags};
        let wake = self.notified.as_fd();
        // Without a server the wake end is simply listed twice.
        let server = session.map_or(wake, |live| live.conn.stream().as_fd());
        let mut fds = [
            PollFd::from_borrowed_fd(wake, PollFlags::IN),
            PollFd::from_borrowed_fd(server, PollFlags::IN),
        ];
        match crate::wake::poll_fds(&mut fds, timeout) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => return Some(false),
            Err(_) => return None,
        }
        let hung_up = session.is_some()
            && fds[1]
                .revents()
                .intersects(PollFlags::HUP | PollFlags::ERR | PollFlags::NVAL);
        let mut bytes = [0u8; 64];
        loop {
            match (&self.notified).read(&mut bytes) {
                Ok(0) => return None,
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Some(hung_up),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return None,
            }
        }
    }
}
/// Follow `windows` on `display`, connecting when there is something to
/// follow there. `None` when the server does not answer.
fn follow(
    session: &mut Option<Session>,
    display: u32,
    windows: &[(u32, u64)],
) -> Option<Vec<Icon>> {
    if session.as_ref().is_some_and(|live| live.display != display) {
        *session = None;
    }
    // At most twice: a connection that died unnoticed is replaced once.
    for _ in 0..2 {
        if session.is_none() {
            if windows.is_empty() {
                // Nothing to follow: no connection is opened for it.
                return Some(Vec::new());
            }
            *session = Session::connect(display);
        }
        if let Some(icons) = session.as_mut()?.follow(windows) {
            return Some(icons);
        }
        *session = None;
    }
    None
}
/// One connection to an X server and the windows followed on it.
struct Session {
    display: u32,
    conn: RustConnection,
    /// `_NET_WM_ICON`.
    icon: u32,
    /// X11 id to compositor id.
    followed: HashMap<u32, u64>,
}
impl Session {
    fn connect(display: u32) -> Option<Self> {
        let (conn, _) = smithay::reexports::x11rb::connect(Some(&format!(":{display}"))).ok()?;
        let icon = conn
            .intern_atom(false, b"_NET_WM_ICON")
            .ok()?
            .reply()
            .ok()?
            .atom;
        Some(Self {
            display,
            conn,
            icon,
            followed: HashMap::new(),
        })
    }
    /// Ask for `window`'s events in `mask`. A window that is gone already
    /// answers with an error nobody waits for.
    fn select(&self, window: u32, mask: EventMask) -> Option<()> {
        self.conn
            .change_window_attributes(window, &ChangeWindowAttributesAux::new().event_mask(mask))
            .ok()?
            .ignore_error();
        Some(())
    }
    /// Follow exactly `windows` and read the icons of those newly
    /// followed. `None` once the server is gone.
    fn follow(&mut self, windows: &[(u32, u64)]) -> Option<Vec<Icon>> {
        for xid in self.followed.keys() {
            if !windows.iter().any(|(wanted, _)| wanted == xid) {
                self.select(*xid, EventMask::NO_EVENT)?;
            }
        }
        let added: Vec<(u32, u64)> = windows
            .iter()
            .copied()
            .filter(|(xid, id)| self.followed.get(xid) != Some(id))
            .collect();
        // Selected before the read: a change in between arrives as an event.
        for (xid, _) in &added {
            self.select(*xid, EventMask::PROPERTY_CHANGE)?;
        }
        self.followed = windows.iter().copied().collect();
        self.read(&added)
    }
    /// The icons of `windows`. `None` once the server is gone.
    fn read(&self, windows: &[(u32, u64)]) -> Option<Vec<Icon>> {
        let icons = windows
            .iter()
            .map(|&(xid, id)| {
                let raster = self
                    .conn
                    .get_property(false, xid, self.icon, AtomEnum::CARDINAL, 0, MAX_WORDS)
                    .ok()
                    .and_then(|c| c.reply().ok())
                    .filter(|p| p.bytes_after == 0 && p.format == 32)
                    .and_then(|p| parse(&p.value32()?.collect::<Vec<_>>()))
                    .or_else(|| wm_hints(&self.conn, xid));
                (xid, id, raster)
            })
            .collect();
        // A round trip only a live server answers, which also sends what
        // was queued: icons a dying connection failed to read must not
        // pass for windows without one.
        self.conn.get_input_focus().ok()?.reply().ok()?;
        Some(icons)
    }
    /// Add the followed windows whose icon sources changed, from the
    /// events received so far, to `into`. `false` once the server is gone.
    fn changed(&self, into: &mut Vec<(u32, u64)>) -> bool {
        loop {
            match self.conn.poll_for_event() {
                Ok(Some(Event::PropertyNotify(event)))
                    if event.atom == self.icon || event.atom == u32::from(AtomEnum::WM_HINTS) =>
                {
                    if let Some(&id) = self.followed.get(&event.window) {
                        if !into.contains(&(event.window, id)) {
                            into.push((event.window, id));
                        }
                    }
                }
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(_) => return false,
            }
        }
        into.retain(|(xid, id)| self.followed.get(xid) == Some(id));
        true
    }
}
fn wm_hints(conn: &impl Connection, xid: u32) -> Option<Raster> {
    let reply = conn
        .get_property(false, xid, AtomEnum::WM_HINTS, AtomEnum::WM_HINTS, 0, 9)
        .ok()?
        .reply()
        .ok()?;
    if reply.format != 32 || reply.bytes_after != 0 {
        return None;
    }
    let hints = reply.value32()?.collect::<Vec<_>>();
    if hints.len() < 9 || hints[0] & (1 << 2) == 0 || hints[3] == 0 {
        return None;
    }
    let geometry = conn.get_geometry(hints[3]).ok()?.reply().ok()?;
    let (w, h) = (u32::from(geometry.width), u32::from(geometry.height));
    if w == 0 || w != h || w > 1024 || !matches!(geometry.depth, 24 | 32) {
        return None;
    }
    let setup = conn.setup();
    let screen = setup.roots.iter().find(|r| r.root == geometry.root)?;
    let visual = screen
        .allowed_depths
        .iter()
        .find(|d| d.depth == geometry.depth)?
        .visuals
        .iter()
        .find(|v| {
            v.class == VisualClass::TRUE_COLOR
                && (v.visual_id == screen.root_visual || geometry.depth == 32)
        })?;
    let format = setup
        .pixmap_formats
        .iter()
        .find(|f| f.depth == geometry.depth)?;
    if !matches!(format.bits_per_pixel, 24 | 32) || !matches!(format.scanline_pad, 8 | 16 | 32) {
        return None;
    }
    let masks = [visual.red_mask, visual.green_mask, visual.blue_mask];
    if masks.iter().any(|m| !mask_valid(*m))
        || masks[0] & masks[1] != 0
        || masks[0] & masks[2] != 0
        || masks[1] & masks[2] != 0
    {
        return None;
    }
    let image = conn
        .get_image(
            ImageFormat::Z_PIXMAP,
            hints[3],
            0,
            0,
            geometry.width,
            geometry.height,
            u32::MAX,
        )
        .ok()?
        .reply()
        .ok()?;
    if image.depth != geometry.depth {
        return None;
    }
    let stride = (w * u32::from(format.bits_per_pixel)).div_ceil(u32::from(format.scanline_pad))
        * u32::from(format.scanline_pad)
        / 8;
    if image.data.len() != stride as usize * h as usize {
        return None;
    }
    let mask = if hints[0] & (1 << 5) != 0 && hints[7] != 0 {
        let g = conn.get_geometry(hints[7]).ok()?.reply().ok()?;
        if g.width != geometry.width
            || g.height != geometry.height
            || g.depth != 1
            || g.root != geometry.root
        {
            return None;
        }
        let f = setup.pixmap_formats.iter().find(|f| f.depth == 1)?;
        if f.bits_per_pixel != 1 || !matches!(f.scanline_pad, 8 | 16 | 32) {
            return None;
        }
        let image = conn
            .get_image(ImageFormat::Z_PIXMAP, hints[7], 0, 0, g.width, g.height, 1)
            .ok()?
            .reply()
            .ok()?;
        let stride = w.div_ceil(u32::from(f.scanline_pad)) * u32::from(f.scanline_pad) / 8;
        if image.depth != 1 || image.data.len() != stride as usize * h as usize {
            return None;
        }
        Some((image.data, stride))
    } else {
        None
    };
    let width = w.min(256);
    let mut rgba = Vec::with_capacity(width as usize * width as usize * 4);
    for y in 0..width {
        for x in 0..width {
            let (sx, sy) = (x * w / width, y * h / width);
            let offset = (sy * stride + sx * u32::from(format.bits_per_pixel) / 8) as usize;
            let bytes = &image.data[offset..offset + usize::from(format.bits_per_pixel / 8)];
            let pixel = if setup.image_byte_order == ImageOrder::LSB_FIRST {
                bytes
                    .iter()
                    .enumerate()
                    .fold(0u32, |v, (i, b)| v | u32::from(*b) << (8 * i))
            } else {
                bytes.iter().fold(0u32, |v, b| (v << 8) | u32::from(*b))
            };
            for m in masks {
                rgba.push(channel(pixel, m));
            }
            let alpha = mask.as_ref().map_or(255, |(data, stride)| {
                let byte = data[(sy * stride + sx / 8) as usize];
                let bit = if setup.bitmap_format_bit_order == ImageOrder::LSB_FIRST {
                    sx % 8
                } else {
                    7 - sx % 8
                };
                if byte & (1 << bit) != 0 {
                    255
                } else {
                    0
                }
            });
            rgba.push(alpha);
        }
    }
    Some(Raster { width, rgba })
}
fn mask_valid(mask: u32) -> bool {
    if mask == 0 {
        return false;
    }
    let bits = mask >> mask.trailing_zeros();
    bits & bits.wrapping_add(1) == 0
}
fn channel(pixel: u32, mask: u32) -> u8 {
    let shift = mask.trailing_zeros();
    (((pixel & mask) >> shift) as u64 * 255 / u64::from(mask >> shift)) as u8
}

fn parse(words: &[u32]) -> Option<Raster> {
    let mut remaining = words;
    let mut best = None;
    while remaining.len() >= 2 {
        let (w, h) = (remaining[0], remaining[1]);
        let count = (w as usize).checked_mul(h as usize)?;
        if w == 0 || h == 0 || count > remaining.len() - 2 {
            return None;
        }
        let pixels = &remaining[2..2 + count];
        if w == h && w <= 1024 && best.as_ref().is_none_or(|r: &Raster| w.min(256) >= r.width) {
            let width = w.min(256);
            let mut rgba = Vec::with_capacity(width as usize * width as usize * 4);
            for y in 0..width {
                for x in 0..width {
                    let p = pixels[(y * w / width * w + x * w / width) as usize];
                    rgba.extend_from_slice(&[
                        (p >> 16) as u8,
                        (p >> 8) as u8,
                        p as u8,
                        (p >> 24) as u8,
                    ]);
                }
            }
            best = Some(Raster { width, rgba });
        }
        remaining = &remaining[2 + count..];
    }
    if !remaining.is_empty() {
        return None;
    }
    best
}
#[cfg(test)]
mod tests {
    use super::*;
    use smithay::reexports::x11rb::protocol::xproto::{CreateWindowAux, PropMode, WindowClass};
    use smithay::reexports::x11rb::wrapper::ConnectionExt as _;
    use std::io::{BufRead, BufReader};
    use std::process::{Child, Command, Stdio};
    #[test]
    fn true_color_masks_are_contiguous_and_scaled() {
        assert!(mask_valid(0xff0000));
        assert!(mask_valid(0x3ff00000));
        assert!(!mask_valid(0));
        assert!(!mask_valid(0x101));
        assert_eq!(channel(0x3ff00000, 0x3ff00000), 255);
        assert_eq!(channel(0x00008000, 0x0000ff00), 128);
    }
    #[test]
    fn rejects_truncated_and_normalizes_argb() {
        assert!(parse(&[u32::MAX, u32::MAX]).is_none());
        assert!(parse(&[2, 2, 0]).is_none());
        let r = parse(&[1, 1, 0x80402010]).unwrap();
        assert_eq!(r.rgba, [64, 32, 16, 128]);
    }

    const LIMIT: Duration = Duration::from_secs(10);
    /// Three periods of the 500 ms probe the events replaced.
    const QUIET: Duration = Duration::from_millis(1600);

    /// A private X server for the worker to follow windows on.
    struct Server {
        child: Child,
        display: u32,
    }
    impl Server {
        fn start() -> Self {
            let mut child = Command::new("Xvfb")
                .args([
                    "-displayfd",
                    "1",
                    "-screen",
                    "0",
                    "64x64x24",
                    "-nolisten",
                    "tcp",
                ])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .expect("the required real X server (Xvfb) must start");
            let mut line = String::new();
            BufReader::new(child.stdout.take().unwrap())
                .read_line(&mut line)
                .unwrap();
            let display = line.trim().parse().expect("Xvfb names its display");
            Self { child, display }
        }
    }
    impl Drop for Server {
        fn drop(&mut self) {
            // SIGTERM lets the server remove its lock and socket.
            if let Some(pid) = rustix::process::Pid::from_raw(self.child.id() as i32) {
                let _ = rustix::process::kill_process(pid, rustix::process::Signal::TERM);
            }
            let _ = self.child.wait();
        }
    }

    /// A reader whose wakes a loop counts.
    fn reader() -> (Reader, calloop::EventLoop<'static, usize>) {
        let (wake, source) = crate::wake::Wake::new().unwrap();
        let loop_ = calloop::EventLoop::try_new().unwrap();
        loop_
            .handle()
            .insert_source(source, |_, _, wakes| *wakes += 1)
            .unwrap();
        let reader = Reader::new();
        reader.set_wake(&wake);
        (reader, loop_)
    }

    /// The icons of the next read, from a loop only the reader's wake ends.
    fn next_icons(
        reader: &mut Reader,
        loop_: &mut calloop::EventLoop<usize>,
        wakes: &mut usize,
        display: u32,
        windows: &[(u32, u64)],
    ) -> Vec<Icon> {
        let deadline = Instant::now() + LIMIT;
        loop {
            let icons = reader.poll(display, windows.to_vec());
            if !icons.is_empty() {
                return icons;
            }
            let now = Instant::now();
            assert!(now < deadline, "the worker read no icon");
            loop_.dispatch(Some(deadline - now), wakes).unwrap();
        }
    }

    /// The one icon of the next read, which must be `window`'s.
    fn next_icon(
        reader: &mut Reader,
        loop_: &mut calloop::EventLoop<usize>,
        wakes: &mut usize,
        display: u32,
        windows: &[(u32, u64)],
        window: (u32, u64),
    ) -> Option<Raster> {
        let mut icons = next_icons(reader, loop_, wakes, display, windows);
        assert_eq!(icons.len(), 1, "one window was read");
        let (xid, id, raster) = icons.remove(0);
        assert_eq!((xid, id), window);
        raster
    }

    /// `during` passes without the reader waking the loop or reading
    /// anything, however often the same windows are named.
    fn assert_quiet(
        reader: &mut Reader,
        loop_: &mut calloop::EventLoop<usize>,
        wakes: &mut usize,
        display: u32,
        windows: &[(u32, u64)],
        during: Duration,
    ) {
        // The last read's own wake, if it is still on its way.
        loop_
            .dispatch(Some(Duration::from_millis(200)), wakes)
            .unwrap();
        let before = *wakes;
        let until = Instant::now() + during;
        while let Some(left) = until.checked_duration_since(Instant::now()) {
            loop_
                .dispatch(Some(left.min(Duration::from_millis(100))), wakes)
                .unwrap();
            assert!(
                reader.poll(display, windows.to_vec()).is_empty(),
                "unchanged windows are not read again"
            );
        }
        assert_eq!(*wakes, before, "unchanged windows must not wake the loop");
        assert_eq!(reader.next_deadline(), None, "and ask for no timed wake");
    }

    #[test]
    fn an_icon_is_read_when_its_window_is_named_and_when_it_changes_never_on_a_timer() {
        let server = Server::start();
        let display = server.display;
        let (conn, screen) =
            smithay::reexports::x11rb::connect(Some(&format!(":{display}"))).unwrap();
        let root = conn.setup().roots[screen].root;
        let create = || {
            let window = conn.generate_id().unwrap();
            conn.create_window(
                0,
                window,
                root,
                0,
                0,
                16,
                16,
                0,
                WindowClass::INPUT_OUTPUT,
                0,
                &CreateWindowAux::new(),
            )
            .unwrap()
            .check()
            .unwrap();
            window
        };
        let window = create();
        let icon = conn
            .intern_atom(false, b"_NET_WM_ICON")
            .unwrap()
            .reply()
            .unwrap()
            .atom;
        let set_icon = |argb: u32| {
            conn.change_property32(
                PropMode::REPLACE,
                window,
                icon,
                AtomEnum::CARDINAL,
                &[1, 1, argb],
            )
            .unwrap()
            .check()
            .unwrap();
        };
        let (mut reader, mut loop_) = reader();
        let mut wakes = 0;
        let one = [(window, 7)];

        // Named for the first time: read at once; it has no icon yet.
        let first = next_icon(&mut reader, &mut loop_, &mut wakes, display, &one, one[0]);
        assert!(first.is_none());
        assert_quiet(&mut reader, &mut loop_, &mut wakes, display, &one, QUIET);

        // The client sets, replaces and removes its icon: each change
        // arrives from its PropertyNotify.
        set_icon(0x8040_2010);
        let set = next_icon(&mut reader, &mut loop_, &mut wakes, display, &one, one[0]);
        assert_eq!(set.expect("the icon that was set").rgba, [64, 32, 16, 128]);
        set_icon(0xff00_00ff);
        let replaced = next_icon(&mut reader, &mut loop_, &mut wakes, display, &one, one[0]);
        assert_eq!(
            replaced.expect("the icon that replaced it").rgba,
            [0, 0, 255, 255]
        );
        conn.delete_property(window, icon).unwrap().check().unwrap();
        let removed = next_icon(&mut reader, &mut loop_, &mut wakes, display, &one, one[0]);
        assert!(removed.is_none(), "a removed icon reads as none");

        // A second window is read alone, and naming the two in another
        // order is no change.
        let second = create();
        let both = [(window, 7), (second, 8)];
        let added = next_icon(&mut reader, &mut loop_, &mut wakes, display, &both, both[1]);
        assert!(added.is_none());
        let reordered = [both[1], both[0]];
        assert_quiet(
            &mut reader,
            &mut loop_,
            &mut wakes,
            display,
            &reordered,
            Duration::from_millis(300),
        );
    }

    #[test]
    fn a_server_that_does_not_answer_is_asked_again_on_a_deadline_and_never_without_windows() {
        // No X server listens on this display.
        const ABSENT: u32 = 4091;
        let (mut reader, mut loop_) = reader();
        let mut wakes = 0;

        // No windows: no connection is tried, so nothing fails or wakes.
        assert_quiet(
            &mut reader,
            &mut loop_,
            &mut wakes,
            ABSENT,
            &[],
            Duration::from_millis(300),
        );

        // A window there: the failure is reported, and its retry is a
        // deadline one old probe period away.
        let deadline = Instant::now() + LIMIT;
        while reader.next_deadline().is_none() {
            assert!(reader.poll(ABSENT, vec![(1, 1)]).is_empty());
            let now = Instant::now();
            assert!(now < deadline, "the failure was not reported");
            loop_.dispatch(Some(deadline - now), &mut wakes).unwrap();
            assert!(reader.poll(ABSENT, vec![(1, 1)]).is_empty());
        }
        assert!(reader
            .next_deadline()
            .is_some_and(|at| at <= Instant::now() + RETRY));

        // The compositor lost its server: nothing is asked of it again.
        reader.reset();
        assert_eq!(reader.next_deadline(), None);
    }
}
