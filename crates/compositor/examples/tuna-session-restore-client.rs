//! Window-session peer for `scripts/tuna-session-restore-proof` (P-SY-08):
//! a plain Wayland client with two toplevels, `main` and `aux`, in an
//! xdg-session-management session, as an app that opts in does.
//!
//! Usage: `tuna-session-restore-client --report FILE [--session ID | --no-session]`
//!
//! Without `--session` it opens a new session and adds both toplevels;
//! with it, it reopens that session and asks to restore both before their
//! first commit; `--no-session` maps the same windows without any session
//! (the opt-out case). Pressing the left button moves a window (the
//! header-bar drag of a client-side decorated app), within 24 px of its
//! bottom-right corner resizes it. FILE gets every protocol event in
//! arrival order plus the session id, rewritten atomically after each
//! dispatch. The client exits when the compositor goes away.

use std::{
    io::{Seek, SeekFrom, Write},
    os::fd::AsFd,
    path::PathBuf,
};

use wayland_client::{
    protocol::{
        wl_buffer, wl_compositor, wl_pointer, wl_registry, wl_seat, wl_shm, wl_shm_pool, wl_surface,
    },
    Connection, Dispatch, QueueHandle, WEnum,
};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

#[allow(
    dead_code,
    unused_imports,
    non_camel_case_types,
    non_upper_case_globals,
    missing_docs,
    clippy::all
)]
mod protocol {
    use wayland_client;
    use wayland_client::protocol::*;
    use wayland_protocols::xdg::shell::client::*;
    pub mod __interfaces {
        use wayland_client::backend as wayland_backend;
        use wayland_client::protocol::__interfaces::*;
        use wayland_protocols::xdg::shell::client::__interfaces::*;
        wayland_scanner::generate_interfaces!("protocols/xdg-session-management-v1.xml");
    }
    use self::__interfaces::*;
    wayland_scanner::generate_client_code!("protocols/xdg-session-management-v1.xml");
}
use protocol::{
    xdg_session_manager_v1::{self, XdgSessionManagerV1},
    xdg_session_v1::{self, XdgSessionV1},
    xdg_toplevel_session_v1::{self, XdgToplevelSessionV1},
};

const NAMES: [&str; 2] = ["main", "aux"];
/// Size a window starts at when the compositor leaves it the choice.
const START: (i32, i32) = (420, 300);
/// Corner that resizes instead of moving.
const CORNER: f64 = 24.0;

struct Window {
    surface: wl_surface::WlSurface,
    toplevel: xdg_toplevel::XdgToplevel,
    /// Size from the latest configure (0 = the client's choice).
    pending: (i32, i32),
    size: (i32, i32),
    buffer: Option<(wl_buffer::WlBuffer, std::fs::File)>,
}

#[derive(Default)]
struct Peer {
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    wm: Option<xdg_wm_base::XdgWmBase>,
    seat: Option<wl_seat::WlSeat>,
    sessions: Option<XdgSessionManagerV1>,
    windows: Vec<Window>,
    /// Window under the pointer and the position on it.
    pointer: Option<(usize, f64, f64)>,
    session_id: Option<String>,
    events: Vec<String>,
    changed: bool,
}

impl Peer {
    fn log(&mut self, event: String) {
        self.events.push(event);
        self.changed = true;
    }

    fn index(&self, surface: &wl_surface::WlSurface) -> Option<usize> {
        self.windows.iter().position(|w| &w.surface == surface)
    }

    /// Draw a window at its configured size (one colour per window).
    fn draw(&mut self, index: usize, qh: &QueueHandle<Self>) {
        let shm = self.shm.clone().expect("wl_shm");
        let window = &mut self.windows[index];
        let (w, h) = match window.pending {
            (0, _) | (_, 0) => START,
            size => size,
        };
        let color: u32 = if index == 0 { 0xff3465a4 } else { 0xff73d216 };
        let mut file = tempfile::tempfile().expect("buffer file");
        let row: Vec<u8> = (0..w).flat_map(|_| color.to_ne_bytes()).collect();
        for _ in 0..h {
            file.write_all(&row).expect("buffer bytes");
        }
        file.seek(SeekFrom::Start(0)).expect("buffer rewind");
        let pool = shm.create_pool(file.as_fd(), w * h * 4, qh, ());
        let buffer = pool.create_buffer(0, w, h, w * 4, wl_shm::Format::Argb8888, qh, ());
        pool.destroy();
        window.surface.attach(Some(&buffer), 0, 0);
        window.surface.damage_buffer(0, 0, w, h);
        window.surface.commit();
        if let Some((old, _)) = window.buffer.replace((buffer, file)) {
            old.destroy();
        }
        window.size = (w, h);
    }

    fn write(&mut self, report: &PathBuf) {
        if !self.changed {
            return;
        }
        self.changed = false;
        let body = serde_json::json!({
            "pid": std::process::id(),
            "session_id": self.session_id,
            "events": self.events,
            "sizes": self.windows.iter().map(|w| [w.size.0, w.size.1]).collect::<Vec<_>>(),
        });
        let tmp = report.with_extension("tmp");
        std::fs::write(&tmp, body.to_string()).expect("report");
        std::fs::rename(&tmp, report).expect("report rename");
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for Peer {
    fn event(
        peer: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_compositor" => {
                    peer.compositor = Some(registry.bind(name, version.min(4), qh, ()))
                }
                "wl_shm" => peer.shm = Some(registry.bind(name, 1, qh, ())),
                "wl_seat" => peer.seat = Some(registry.bind(name, version.min(5), qh, ())),
                "xdg_wm_base" => peer.wm = Some(registry.bind(name, 1, qh, ())),
                "xdg_session_manager_v1" => peer.sessions = Some(registry.bind(name, 1, qh, ())),
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for Peer {
    fn event(
        _: &mut Self,
        seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(caps),
        } = event
        {
            if caps.contains(wl_seat::Capability::Pointer) {
                seat.get_pointer(qh, ());
            }
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for Peer {
    fn event(
        peer: &mut Self,
        _: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Enter {
                surface,
                surface_x,
                surface_y,
                ..
            } => peer.pointer = peer.index(&surface).map(|i| (i, surface_x, surface_y)),
            wl_pointer::Event::Leave { .. } => peer.pointer = None,
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => {
                if let Some((i, _, _)) = peer.pointer {
                    peer.pointer = Some((i, surface_x, surface_y));
                }
            }
            wl_pointer::Event::Button {
                serial,
                button: 0x110,
                state: WEnum::Value(wl_pointer::ButtonState::Pressed),
                ..
            } => {
                let (Some((index, x, y)), Some(seat)) = (peer.pointer, peer.seat.clone()) else {
                    return;
                };
                let window = &peer.windows[index];
                let (w, h) = (f64::from(window.size.0), f64::from(window.size.1));
                if x >= w - CORNER && y >= h - CORNER {
                    window
                        .toplevel
                        .resize(&seat, serial, xdg_toplevel::ResizeEdge::BottomRight);
                    peer.log(format!("resize {}", NAMES[index]));
                } else {
                    window.toplevel._move(&seat, serial);
                    peer.log(format!("move {}", NAMES[index]));
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for Peer {
    fn event(
        _: &mut Self,
        wm: &xdg_wm_base::XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm.pong(serial);
        }
    }
}

impl Dispatch<xdg_toplevel::XdgToplevel, usize> for Peer {
    fn event(
        peer: &mut Self,
        _: &xdg_toplevel::XdgToplevel,
        event: xdg_toplevel::Event,
        index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_toplevel::Event::Configure { width, height, .. } = event {
            peer.windows[*index].pending = (width, height);
            peer.log(format!("configure {} {width}x{height}", NAMES[*index]));
        }
    }
}

impl Dispatch<xdg_surface::XdgSurface, usize> for Peer {
    fn event(
        peer: &mut Self,
        xdg: &xdg_surface::XdgSurface,
        event: xdg_surface::Event,
        index: &usize,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg.ack_configure(serial);
            peer.draw(*index, qh);
        }
    }
}

impl Dispatch<XdgSessionV1, ()> for Peer {
    fn event(
        peer: &mut Self,
        _: &XdgSessionV1,
        event: xdg_session_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            xdg_session_v1::Event::Created { session_id } => {
                peer.session_id = Some(session_id);
                peer.log("session created".into());
            }
            xdg_session_v1::Event::Restored => peer.log("session restored".into()),
            xdg_session_v1::Event::Replaced => peer.log("session replaced".into()),
        }
    }
}

impl Dispatch<XdgToplevelSessionV1, usize> for Peer {
    fn event(
        peer: &mut Self,
        _: &XdgToplevelSessionV1,
        event: xdg_toplevel_session_v1::Event,
        index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let xdg_toplevel_session_v1::Event::Restored = event;
        peer.log(format!("restored {}", NAMES[*index]));
    }
}

wayland_client::delegate_noop!(Peer: ignore wl_compositor::WlCompositor);
wayland_client::delegate_noop!(Peer: ignore wl_shm::WlShm);
wayland_client::delegate_noop!(Peer: ignore wl_shm_pool::WlShmPool);
wayland_client::delegate_noop!(Peer: ignore wl_buffer::WlBuffer);
wayland_client::delegate_noop!(Peer: ignore wl_surface::WlSurface);
wayland_client::delegate_noop!(Peer: ignore XdgSessionManagerV1);

enum Mode {
    New,
    Restore(String),
    Plain,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut report = None;
    let mut mode = Mode::New;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--report" => report = args.next().map(PathBuf::from),
            "--session" => mode = Mode::Restore(args.next().expect("--session ID")),
            "--no-session" => mode = Mode::Plain,
            other => panic!("unknown argument {other}"),
        }
    }
    let report = report.expect("--report FILE");
    let conn = Connection::connect_to_env().expect("Wayland display");
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    let mut peer = Peer::default();
    conn.display().get_registry(&qh, ());
    queue.roundtrip(&mut peer).expect("registry");

    let session = match &mode {
        Mode::Plain => None,
        Mode::New => Some(
            peer.sessions
                .as_ref()
                .expect("xdg_session_manager_v1")
                .get_session(xdg_session_manager_v1::Reason::Launch, None, &qh, ()),
        ),
        Mode::Restore(id) => Some(
            peer.sessions
                .as_ref()
                .expect("xdg_session_manager_v1")
                .get_session(
                    xdg_session_manager_v1::Reason::SessionRestore,
                    Some(id.clone()),
                    &qh,
                    (),
                ),
        ),
    };
    let mut handles = Vec::new();
    for (index, name) in NAMES.iter().enumerate() {
        let surface = peer
            .compositor
            .as_ref()
            .expect("wl_compositor")
            .create_surface(&qh, ());
        let xdg = peer
            .wm
            .as_ref()
            .expect("xdg_wm_base")
            .get_xdg_surface(&surface, &qh, index);
        let toplevel = xdg.get_toplevel(&qh, index);
        toplevel.set_app_id("org.tuna.SessionRestoreProof".into());
        toplevel.set_title(format!("Session {name}"));
        if let Some(session) = &session {
            handles.push(match mode {
                Mode::Restore(_) => session.restore_toplevel(&toplevel, (*name).into(), &qh, index),
                _ => session.add_toplevel(&toplevel, (*name).into(), &qh, index),
            });
        }
        // The initial commit: restore_toplevel came before it.
        surface.commit();
        std::mem::forget(xdg);
        peer.windows.push(Window {
            surface,
            toplevel,
            pending: (0, 0),
            size: (0, 0),
            buffer: None,
        });
    }
    peer.changed = true;
    loop {
        peer.write(&report);
        if queue.blocking_dispatch(&mut peer).is_err() {
            peer.write(&report);
            break;
        }
    }
    drop(handles);
}
