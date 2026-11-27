//! Bell principal for the GTK proof's G-VISUAL-BELL / G-X11-BELL gates.
//!
//! `wayland <app-id>` maps a 320x200 RGB 240 xdg window and rings
//! xdg-system-bell for each stdin line: `window` names the window's surface,
//! `whole` names none. `x11` maps a 320x200 RGB 240 X11 window and rings the
//! XKB bell for each line: `window` names the window (`XkbBell` with a
//! window, as xkbbell -w does), `none` names none (as `XBell` does).
//! `xsettings <name>` prints the `_XSETTINGS_S0` owner and the named
//! integer setting. Each ring prints `RING <n> <target>` after the server
//! handled it; that is no proof of pixels or sound.
use std::io::{BufRead, Write};
use std::os::fd::AsFd;
use wayland_client::{
    protocol::{
        wl_buffer::WlBuffer,
        wl_compositor::WlCompositor,
        wl_registry::{self, WlRegistry},
        wl_shm::{self, WlShm},
        wl_shm_pool::WlShmPool,
        wl_surface::WlSurface,
    },
    Connection, Dispatch, QueueHandle,
};
use wayland_protocols::xdg::{
    shell::client::{
        xdg_surface::{self, XdgSurface},
        xdg_toplevel::XdgToplevel,
        xdg_wm_base::{self, XdgWmBase},
    },
    system_bell::v1::client::xdg_system_bell_v1::XdgSystemBellV1,
};

const WIDTH: i32 = 320;
const HEIGHT: i32 = 200;

#[derive(Default)]
struct Client {
    globals: Vec<(u32, String, u32)>,
}

impl Client {
    fn bind<I: wayland_client::Proxy + 'static>(
        &self,
        registry: &WlRegistry,
        qh: &QueueHandle<Self>,
        version: u32,
    ) -> I
    where
        Self: Dispatch<I, ()>,
    {
        let (name, _, offered) = self
            .globals
            .iter()
            .find(|(_, interface, _)| interface == I::interface().name)
            .unwrap_or_else(|| panic!("no {} global", I::interface().name));
        registry.bind(*name, version.min(*offered), qh, ())
    }
}

impl Dispatch<WlRegistry, ()> for Client {
    fn event(
        state: &mut Self,
        _: &WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            state.globals.push((name, interface, version));
        }
    }
}

impl Dispatch<XdgWmBase, ()> for Client {
    fn event(
        _: &mut Self,
        wm: &XdgWmBase,
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

impl Dispatch<XdgSurface, ()> for Client {
    fn event(
        _: &mut Self,
        surface: &XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            surface.ack_configure(serial);
        }
    }
}

wayland_client::delegate_noop!(Client: ignore WlCompositor);
wayland_client::delegate_noop!(Client: ignore WlSurface);
wayland_client::delegate_noop!(Client: ignore WlShm);
wayland_client::delegate_noop!(Client: ignore WlShmPool);
wayland_client::delegate_noop!(Client: ignore WlBuffer);
wayland_client::delegate_noop!(Client: ignore XdgToplevel);
wayland_client::delegate_noop!(Client: ignore XdgSystemBellV1);

/// Stdin lines on a channel, so the Wayland connection keeps answering
/// pings while it waits. EOF ends the client.
fn requests() -> std::sync::mpsc::Receiver<String> {
    let (send, receive) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if send.send(line.trim().to_owned()).is_err() {
                break;
            }
        }
    });
    receive
}

fn ring_printed(count: u32, target: &str) {
    println!("RING {count} {target}");
    std::io::stdout().flush().unwrap();
}

fn wayland(app: String) {
    let conn = Connection::connect_to_env().unwrap();
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    let registry = conn.display().get_registry(&qh, ());
    let mut client = Client::default();
    queue.roundtrip(&mut client).unwrap();
    let compositor: WlCompositor = client.bind(&registry, &qh, 4);
    let wm: XdgWmBase = client.bind(&registry, &qh, 1);
    let shm: WlShm = client.bind(&registry, &qh, 1);
    let bell: XdgSystemBellV1 = client.bind(&registry, &qh, 1);
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm.get_xdg_surface(&surface, &qh, ());
    let top = xdg.get_toplevel(&qh, ());
    top.set_app_id(app);
    top.set_title("Tuna Bell Probe".into());
    surface.commit();
    queue.roundtrip(&mut client).unwrap();
    let size = WIDTH * HEIGHT * 4;
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(&vec![0xf0; size as usize]).unwrap();
    let pool = shm.create_pool(file.as_fd(), size, &qh, ());
    let buffer = pool.create_buffer(
        0,
        WIDTH,
        HEIGHT,
        WIDTH * 4,
        wl_shm::Format::Xrgb8888,
        &qh,
        (),
    );
    surface.attach(Some(&buffer), 0, 0);
    surface.damage(0, 0, WIDTH, HEIGHT);
    surface.commit();
    queue.roundtrip(&mut client).unwrap();
    println!("READY {}", std::process::id());
    std::io::stdout().flush().unwrap();
    let lines = requests();
    let mut count = 0;
    loop {
        match lines.recv_timeout(std::time::Duration::from_millis(20)) {
            Ok(line) => {
                let target = match line.as_str() {
                    "window" => Some(&surface),
                    "whole" => None,
                    _ => continue,
                };
                bell.ring(target);
                queue.roundtrip(&mut client).unwrap();
                count += 1;
                ring_printed(count, &line);
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                queue.roundtrip(&mut client).unwrap();
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

#[cfg(feature = "xwayland")]
fn x11() {
    use smithay::reexports::x11rb::{
        connection::Connection,
        protocol::{
            xkb::{self, ConnectionExt as _},
            xproto::{AtomEnum, ConnectionExt as _, CreateWindowAux, PropMode, WindowClass},
        },
        wrapper::ConnectionExt as _,
        NONE,
    };
    let (conn, screen) = smithay::reexports::x11rb::connect(None).unwrap();
    assert!(
        conn.xkb_use_extension(1, 0)
            .unwrap()
            .reply()
            .unwrap()
            .supported
    );
    let root = &conn.setup().roots[screen];
    let window = conn.generate_id().unwrap();
    conn.create_window(
        root.root_depth,
        window,
        root.root,
        0,
        0,
        WIDTH as u16,
        HEIGHT as u16,
        0,
        WindowClass::INPUT_OUTPUT,
        root.root_visual,
        &CreateWindowAux::new().background_pixel(0xf0f0f0),
    )
    .unwrap();
    conn.change_property8(
        PropMode::REPLACE,
        window,
        AtomEnum::WM_CLASS,
        AtomEnum::STRING,
        b"tuna-x11-bell\0TunaX11Bell\0",
    )
    .unwrap();
    conn.change_property8(
        PropMode::REPLACE,
        window,
        AtomEnum::WM_NAME,
        AtomEnum::STRING,
        b"Tuna X11 Bell Probe",
    )
    .unwrap();
    conn.map_window(window).unwrap();
    conn.flush().unwrap();
    println!("READY {window}");
    std::io::stdout().flush().unwrap();
    let mut count = 0;
    for line in std::io::stdin().lock().lines() {
        let line = line.unwrap();
        let target = match line.trim() {
            "window" => window,
            "none" => NONE,
            _ => continue,
        };
        conn.xkb_bell(
            xkb::ID::USE_CORE_KBD.into(),
            xkb::BellClass::DFLT_XI_CLASS.into(),
            xkb::ID::DFLT_XI_ID.into(),
            0,
            false,
            false,
            0,
            0,
            NONE,
            target,
        )
        .unwrap();
        // A round trip: the server has handled the bell.
        conn.get_input_focus().unwrap().reply().unwrap();
        count += 1;
        ring_printed(count, line.trim());
    }
}

#[cfg(feature = "xwayland")]
fn xsettings(name: &str) {
    use smithay::reexports::x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _};
    let (conn, screen) = smithay::reexports::x11rb::connect(None).unwrap();
    let selection = format!("_XSETTINGS_S{screen}");
    let atom = |name: &str| {
        conn.intern_atom(false, name.as_bytes())
            .unwrap()
            .reply()
            .unwrap()
            .atom
    };
    let owner = conn
        .get_selection_owner(atom(&selection))
        .unwrap()
        .reply()
        .unwrap()
        .owner;
    if owner == 0 {
        println!("owner none");
        return;
    }
    let data = conn
        .get_property(
            false,
            owner,
            atom("_XSETTINGS_SETTINGS"),
            AtomEnum::ANY,
            0,
            1 << 16,
        )
        .unwrap()
        .reply()
        .unwrap()
        .value;
    print!("owner {owner:#x}");
    match xsettings_integer(&data, name) {
        Some(value) => println!(" {name} {value}"),
        None => println!(" {name} unset"),
    }
}

/// The integer setting `name` from an XSETTINGS property
/// (specifications.freedesktop.org/xsettings-spec 0.5).
#[cfg(feature = "xwayland")]
fn xsettings_integer(data: &[u8], name: &str) -> Option<i32> {
    let big = *data.first()? == 1;
    let u16_at = |at: usize| -> Option<u16> {
        let bytes = [*data.get(at)?, *data.get(at + 1)?];
        Some(if big {
            u16::from_be_bytes(bytes)
        } else {
            u16::from_le_bytes(bytes)
        })
    };
    let u32_at = |at: usize| -> Option<u32> {
        let bytes = [
            *data.get(at)?,
            *data.get(at + 1)?,
            *data.get(at + 2)?,
            *data.get(at + 3)?,
        ];
        Some(if big {
            u32::from_be_bytes(bytes)
        } else {
            u32::from_le_bytes(bytes)
        })
    };
    let pad = |n: usize| n.div_ceil(4) * 4;
    let count = u32_at(8)?;
    let mut at = 12;
    for _ in 0..count {
        let kind = *data.get(at)?;
        let length = usize::from(u16_at(at + 2)?);
        let key = data.get(at + 4..at + 4 + length)?;
        let value = at + 4 + pad(length) + 4;
        if key == name.as_bytes() {
            return (kind == 0).then(|| u32_at(value).map(|v| v as i32))?;
        }
        at = value
            + match kind {
                0 => 4,
                1 => 4 + pad(u32_at(value)? as usize),
                2 => 8,
                _ => return None,
            };
    }
    None
}

fn main() {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("wayland") => wayland(args.next().expect("app id")),
        #[cfg(feature = "xwayland")]
        Some("x11") => x11(),
        #[cfg(feature = "xwayland")]
        Some("xsettings") => xsettings(&args.next().expect("setting name")),
        _ => panic!("usage: tuna-bell-client wayland <app-id> | x11 | xsettings <name>"),
    }
}
