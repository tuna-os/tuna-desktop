//! A real xdg-activation peer for the window-attention proof (#1185).
//!
//! Usage: `tuna-activation-client COMMANDS [APP_ID]`, with `WAYLAND_DISPLAY` set.
//! It polls the file COMMANDS for lines appended by the proof:
//!
//! - `open TITLE`: map a toplevel titled TITLE;
//! - `refuse TITLE`: mint an activation token with the keyboard serial
//!   this client last entered with, then activate the window TITLE with it;
//! - `open-refused TITLE`: mint the token, then create the toplevel TITLE,
//!   activate it with the token before its first commit, and map it.
//!
//! Run while another client holds keyboard focus, the token is the
//! background client's, which the compositor will not honor. Progress
//! lines go to stdout: `entered SERIAL`, `opened TITLE`, `minted`,
//! `activated TITLE`.
use std::{
    io::{Seek, SeekFrom, Write},
    os::fd::AsFd,
    time::Duration,
};
use wayland_client::{
    protocol::{
        wl_buffer, wl_compositor, wl_keyboard, wl_registry, wl_seat, wl_shm, wl_shm_pool,
        wl_surface,
    },
    Connection, Dispatch, QueueHandle, WEnum,
};
use wayland_protocols::xdg::{
    activation::v1::client::{xdg_activation_token_v1, xdg_activation_v1},
    shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base},
};

const SIDE: i32 = 400;

struct Window {
    title: String,
    surface: wl_surface::WlSurface,
    _xdg: xdg_surface::XdgSurface,
    _top: xdg_toplevel::XdgToplevel,
    buffer: wl_buffer::WlBuffer,
    _file: std::fs::File,
}

/// What a minted token is for, once the compositor sends it.
enum Pending {
    Refuse(String),
    OpenRefused(String),
}

#[derive(Default)]
struct Peer {
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    base: Option<xdg_wm_base::XdgWmBase>,
    seat: Option<wl_seat::WlSeat>,
    activation: Option<xdg_activation_v1::XdgActivationV1>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    enter_serial: Option<u32>,
    windows: Vec<Window>,
    pending: Option<Pending>,
    token: Option<String>,
    app_id: String,
}

impl Dispatch<wl_registry::WlRegistry, ()> for Peer {
    fn event(
        s: &mut Self,
        p: &wl_registry::WlRegistry,
        e: wl_registry::Event,
        _: &(),
        _: &Connection,
        q: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = e
        {
            match interface.as_str() {
                "wl_compositor" => s.compositor = Some(p.bind(name, version.min(4), q, ())),
                "wl_shm" => s.shm = Some(p.bind(name, 1, q, ())),
                "xdg_wm_base" => s.base = Some(p.bind(name, version.min(6), q, ())),
                "wl_seat" => s.seat = Some(p.bind(name, version.min(7), q, ())),
                "xdg_activation_v1" => s.activation = Some(p.bind(name, 1, q, ())),
                _ => {}
            }
        }
    }
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for Peer {
    fn event(
        _: &mut Self,
        p: &xdg_wm_base::XdgWmBase,
        e: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = e {
            p.pong(serial);
        }
    }
}

impl Dispatch<xdg_surface::XdgSurface, usize> for Peer {
    fn event(
        s: &mut Self,
        p: &xdg_surface::XdgSurface,
        e: xdg_surface::Event,
        index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = e {
            p.ack_configure(serial);
            if let Some(w) = s.windows.get(*index) {
                w.surface.attach(Some(&w.buffer), 0, 0);
                w.surface.damage_buffer(0, 0, SIDE, SIDE);
                w.surface.commit();
                println!("opened {}", w.title);
            }
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for Peer {
    fn event(
        s: &mut Self,
        p: &wl_seat::WlSeat,
        e: wl_seat::Event,
        _: &(),
        _: &Connection,
        q: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(caps),
        } = e
        {
            if caps.contains(wl_seat::Capability::Keyboard) && s.keyboard.is_none() {
                s.keyboard = Some(p.get_keyboard(q, ()));
            }
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for Peer {
    fn event(
        s: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        e: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_keyboard::Event::Enter { serial, .. } = e {
            s.enter_serial = Some(serial);
            println!("entered {serial}");
        }
    }
}

impl Dispatch<xdg_activation_token_v1::XdgActivationTokenV1, ()> for Peer {
    fn event(
        s: &mut Self,
        _: &xdg_activation_token_v1::XdgActivationTokenV1,
        e: xdg_activation_token_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_activation_token_v1::Event::Done { token } = e {
            s.token = Some(token);
        }
    }
}

wayland_client::delegate_noop!(Peer: ignore wl_compositor::WlCompositor);
wayland_client::delegate_noop!(Peer: ignore wl_shm::WlShm);
wayland_client::delegate_noop!(Peer: ignore wl_shm_pool::WlShmPool);
wayland_client::delegate_noop!(Peer: ignore wl_buffer::WlBuffer);
wayland_client::delegate_noop!(Peer: ignore wl_surface::WlSurface);
wayland_client::delegate_noop!(Peer: ignore xdg_toplevel::XdgToplevel);
wayland_client::delegate_noop!(Peer: ignore xdg_activation_v1::XdgActivationV1);

impl Peer {
    /// Create the toplevel `title` and, with `token`, activate it before
    /// its first commit, as GTK does for a window launched with a token.
    fn create(&mut self, q: &QueueHandle<Self>, title: &str, token: Option<&str>) {
        let mut file = tempfile::tempfile().unwrap();
        let color: u32 = 0xff3465a4 ^ ((self.windows.len() as u32) * 0x00332211);
        let mut bytes = Vec::with_capacity((SIDE * SIDE * 4) as usize);
        for _ in 0..SIDE * SIDE {
            bytes.extend_from_slice(&color.to_ne_bytes());
        }
        file.write_all(&bytes).unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        let pool = self
            .shm
            .as_ref()
            .unwrap()
            .create_pool(file.as_fd(), bytes.len() as i32, q, ());
        let buffer = pool.create_buffer(0, SIDE, SIDE, SIDE * 4, wl_shm::Format::Argb8888, q, ());
        pool.destroy();
        let surface = self.compositor.as_ref().unwrap().create_surface(q, ());
        let index = self.windows.len();
        let xdg = self
            .base
            .as_ref()
            .unwrap()
            .get_xdg_surface(&surface, q, index);
        let top = xdg.get_toplevel(q, ());
        top.set_app_id(self.app_id.clone());
        top.set_title(title.into());
        if let Some(token) = token {
            self.activation
                .as_ref()
                .unwrap()
                .activate(token.to_owned(), &surface);
            println!("activated {title}");
        }
        surface.commit();
        self.windows.push(Window {
            title: title.to_owned(),
            surface,
            _xdg: xdg,
            _top: top,
            buffer,
            _file: file,
        });
    }

    /// Ask for a token with the serial of the last keyboard enter.
    fn mint(&mut self, q: &QueueHandle<Self>, pending: Pending) {
        let token = self
            .activation
            .as_ref()
            .unwrap()
            .get_activation_token(q, ());
        if let (Some(serial), Some(seat)) = (self.enter_serial, self.seat.as_ref()) {
            token.set_serial(serial, seat);
        }
        token.commit();
        self.pending = Some(pending);
        println!("minted");
    }

    fn command(&mut self, q: &QueueHandle<Self>, line: &str) {
        match line.split_once(' ') {
            Some(("open", title)) => self.create(q, title, None),
            Some(("refuse", title)) => self.mint(q, Pending::Refuse(title.to_owned())),
            Some(("open-refused", title)) => self.mint(q, Pending::OpenRefused(title.to_owned())),
            _ => eprintln!("tuna-activation-client: unknown command {line:?}"),
        }
    }

    /// The compositor sent the token a command was waiting for.
    fn token_arrived(&mut self, q: &QueueHandle<Self>) {
        let (Some(token), Some(pending)) = (self.token.take(), self.pending.take()) else {
            return;
        };
        match pending {
            Pending::Refuse(title) => {
                if let Some(window) = self.windows.iter().find(|w| w.title == title) {
                    self.activation
                        .as_ref()
                        .unwrap()
                        .activate(token, &window.surface);
                    println!("activated {title}");
                }
            }
            Pending::OpenRefused(title) => self.create(q, &title, Some(&token)),
        }
    }
}

fn main() {
    let commands = std::env::args().nth(1).expect("COMMANDS file");
    let conn = Connection::connect_to_env().unwrap();
    let mut queue = conn.new_event_queue();
    let q = queue.handle();
    let mut peer = Peer {
        app_id: std::env::args()
            .nth(2)
            .unwrap_or_else(|| "tuna-activation-proof".into()),
        ..Default::default()
    };
    conn.display().get_registry(&q, ());
    queue.roundtrip(&mut peer).unwrap();
    queue.roundtrip(&mut peer).unwrap();
    println!("ready");
    let mut done = 0;
    loop {
        if let Ok(body) = std::fs::read_to_string(&commands) {
            let lines: Vec<&str> = body.lines().collect();
            while done < lines.len() && peer.pending.is_none() {
                let line = lines[done];
                done += 1;
                if line == "quit" {
                    return;
                }
                peer.command(&q, line);
                queue.roundtrip(&mut peer).unwrap();
            }
        }
        queue.roundtrip(&mut peer).unwrap();
        peer.token_arrived(&q);
        std::thread::sleep(Duration::from_millis(20));
    }
}
