//! Where a client's committed surface is on screen for input, as Mutter
//! picks it: its buffer, clipped to its input region.
//!
//! GTK draws its resize handles in the invisible border outside the
//! xdg window geometry and keeps them in its input region: a press there
//! is the window's (mutter 51 meta-wayland-actor-surface.c:223-234,
//! meta-surface-actor.c:190-225), the rest of the shadow passes through.
//! A layer surface covers its committed buffer, not the last configure
//! it acked: gtk4-layer-shell asks 200x200 before GTK has measured the
//! window, and a 114x58 popup must not be taken for 200x200.
//!
//! Conventions follow `buffer_release.rs`: socketpair clients, a bounded
//! pump budget, plain asserts.

use std::os::unix::io::AsFd;
use std::os::unix::net::UnixStream;

use smithay::utils::{Point, Size};
use tuna_compositor::{layer, windows::WindowManager, TestCompositor};
use wayland_client::{
    protocol::{
        wl_buffer::WlBuffer,
        wl_callback::WlCallback,
        wl_compositor::WlCompositor,
        wl_display::WlDisplay,
        wl_region::WlRegion,
        wl_registry::WlRegistry,
        wl_shm::{Format, WlShm},
        wl_shm_pool::WlShmPool,
        wl_surface::WlSurface,
    },
    Connection, Dispatch, EventQueue, Proxy, QueueHandle,
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{Layer, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{Anchor, Event as LayerEvent, ZwlrLayerSurfaceV1},
};

#[derive(Default)]
struct Client {
    compositor: Option<WlCompositor>,
    layer_shell: Option<ZwlrLayerShellV1>,
    xdg_base: Option<XdgWmBase>,
    shm: Option<WlShm>,
    /// The last configure's serial, acked only when a test says so.
    layer_serial: Option<u32>,
    configured: bool,
    synced: bool,
}

impl Dispatch<WlRegistry, ()> for Client {
    fn event(
        state: &mut Self,
        registry: &WlRegistry,
        event: <WlRegistry as Proxy>::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wayland_client::protocol::wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_compositor" => {
                    state.compositor = Some(registry.bind(name, version.min(6), qh, ()))
                }
                "zwlr_layer_shell_v1" => {
                    state.layer_shell = Some(registry.bind(name, version.min(4), qh, ()))
                }
                "xdg_wm_base" => state.xdg_base = Some(registry.bind(name, version.min(5), qh, ())),
                "wl_shm" => state.shm = Some(registry.bind(name, version.min(2), qh, ())),
                _ => {}
            }
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, ()> for Client {
    fn event(
        state: &mut Self,
        _: &ZwlrLayerSurfaceV1,
        event: <ZwlrLayerSurfaceV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let LayerEvent::Configure { serial, .. } = event {
            state.layer_serial = Some(serial);
        }
    }
}

impl Dispatch<XdgSurface, ()> for Client {
    fn event(
        state: &mut Self,
        surface: &XdgSurface,
        event: <XdgSurface as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            surface.ack_configure(serial);
            state.configured = true;
        }
    }
}

impl Dispatch<XdgWmBase, ()> for Client {
    fn event(
        _: &mut Self,
        base: &XdgWmBase,
        event: <XdgWmBase as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            base.pong(serial);
        }
    }
}

impl Dispatch<WlCallback, ()> for Client {
    fn event(
        state: &mut Self,
        _: &WlCallback,
        _: <WlCallback as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        state.synced = true;
    }
}

macro_rules! empty {
    ($t:ty) => {
        impl Dispatch<$t, ()> for Client {
            fn event(
                _: &mut Self,
                _: &$t,
                _: <$t as Proxy>::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
            }
        }
    };
}
empty!(WlDisplay);
empty!(WlCompositor);
empty!(WlSurface);
empty!(WlRegion);
empty!(WlBuffer);
empty!(ZwlrLayerShellV1);
empty!(XdgToplevel);
empty!(WlShm);
empty!(WlShmPool);

struct Session {
    comp: TestCompositor,
    manager: WindowManager,
    conn: Connection,
    queue: EventQueue<Client>,
    qh: QueueHandle<Client>,
    client: Client,
    /// Buffers and their pools stay alive while the surfaces show them.
    buffers: Vec<(std::fs::File, WlShmPool, WlBuffer)>,
}

impl Session {
    fn new(shell: bool) -> Self {
        let mut comp = TestCompositor::new();
        let manager = comp.window_manager();
        let (server, stream) = UnixStream::pair().unwrap();
        if shell {
            comp.add_shell_client(server);
        } else {
            comp.add_client(server);
        }
        comp.state.set_output_size(1280, 800);
        let conn = Connection::from_socket(stream).unwrap();
        let queue = conn.new_event_queue();
        let qh = queue.handle();
        conn.display().get_registry(&qh, ());
        let mut session = Self {
            comp,
            manager,
            conn,
            queue,
            qh,
            client: Client::default(),
            buffers: Vec::new(),
        };
        session.pump(|c| {
            c.compositor.is_some()
                && c.xdg_base.is_some()
                && c.shm.is_some()
                && (c.layer_shell.is_some() || !shell)
        });
        session
    }

    fn pump(&mut self, done: impl Fn(&Client) -> bool) {
        for _ in 0..200 {
            self.queue.flush().unwrap();
            self.comp.pump();
            self.manager.reconcile(&mut self.comp.state);
            self.comp.pump();
            if let Some(guard) = self.queue.prepare_read() {
                guard.read().unwrap();
            }
            self.queue.dispatch_pending(&mut self.client).unwrap();
            if done(&self.client) {
                return;
            }
        }
        panic!("pump budget exhausted");
    }

    fn roundtrip(&mut self) {
        self.client.synced = false;
        self.conn.display().sync(&self.qh, ());
        self.pump(|c| c.synced);
        assert!(
            self.conn.protocol_error().is_none(),
            "{:?}",
            self.conn.protocol_error()
        );
    }

    fn surface(&self) -> WlSurface {
        self.client
            .compositor
            .as_ref()
            .unwrap()
            .create_surface(&self.qh, ())
    }

    fn attach(&mut self, surface: &WlSurface, width: i32, height: i32) {
        let stride = width * 4;
        let file = tempfile::tempfile().unwrap();
        file.set_len((stride * height) as u64).unwrap();
        let pool = self.client.shm.as_ref().unwrap().create_pool(
            file.as_fd(),
            stride * height,
            &self.qh,
            (),
        );
        let buffer = pool.create_buffer(0, width, height, stride, Format::Argb8888, &self.qh, ());
        surface.attach(Some(&buffer), 0, 0);
        surface.damage_buffer(0, 0, width, height);
        self.buffers.push((file, pool, buffer));
    }
}

/// A GTK window: 100x72 of geometry inside a 128x100 buffer of shadow,
/// its input region the geometry plus GTK's 12 px of resize border.
#[test]
fn a_press_on_gtks_invisible_border_is_the_windows() {
    let mut s = Session::new(false);
    let surface = s.surface();
    let xdg = s
        .client
        .xdg_base
        .as_ref()
        .unwrap()
        .get_xdg_surface(&surface, &s.qh, ());
    let _toplevel = xdg.get_toplevel(&s.qh, ());
    surface.commit();
    s.pump(|c| c.configured);
    xdg.set_window_geometry(14, 12, 100, 72);
    let region = s
        .client
        .compositor
        .as_ref()
        .unwrap()
        .create_region(&s.qh, ());
    region.add(2, 0, 124, 96);
    surface.set_input_region(Some(&region));
    s.attach(&surface, 128, 100);
    surface.commit();
    s.roundtrip();
    let id = s.manager.model().windows().next().expect("mapped").id;
    let frame = s.manager.geometry(id).expect("geometry");
    assert_eq!((frame.size.w, frame.size.h), (100, 72));
    let (left, right, middle) = (
        f64::from(frame.loc.x),
        f64::from(frame.loc.x + frame.size.w),
        f64::from(frame.loc.y + frame.size.h / 2),
    );
    let at = |x: f64, y: f64| s.manager.window_at(Point::from((x, y)));
    assert_eq!(at(left + 10.0, middle), Some(id), "the geometry");
    assert_eq!(at(right + 4.0, middle), Some(id), "the right resize border");
    assert_eq!(at(left - 4.0, middle), Some(id), "the left resize border");
    assert_eq!(
        at(right + 4.0, f64::from(frame.loc.y + frame.size.h) + 4.0),
        Some(id),
        "the bottom-right corner's border"
    );
    // Shadow outside the input region, and past the buffer, pass through.
    assert_eq!(at(right + 13.0, middle), None, "shadow outside the region");
    assert_eq!(at(left - 15.0, middle), None, "past the buffer");
}

/// The workspace popup as gtk4-layer-shell maps it: 200x200 asked before
/// GTK measured the window, then its 114x58 buffer with the matching
/// size request, the configure answering that not yet acked.
#[test]
fn a_layer_covers_its_buffer_while_its_resize_is_unacked() {
    let mut s = Session::new(true);
    let surface = s.surface();
    let popup = s.client.layer_shell.as_ref().unwrap().get_layer_surface(
        &surface,
        None,
        Layer::Overlay,
        tuna_shell_control::WORKSPACE_POPUP_NAMESPACE.into(),
        &s.qh,
        (),
    );
    popup.set_anchor(Anchor::Bottom);
    popup.set_size(200, 200);
    surface.commit();
    s.pump(|c| c.layer_serial.is_some());
    s.roundtrip();
    popup.ack_configure(s.client.layer_serial.take().expect("configure"));
    popup.set_size(114, 58);
    s.attach(&surface, 114, 58);
    surface.commit();
    s.roundtrip();
    let server = s.comp.state.layer_surfaces()[0].wl_surface().clone();
    let (_, origin, _) = layer::layer_layout(&s.comp.state)
        .into_iter()
        .find(|(placed, _, _)| *placed == server)
        .expect("placed");
    assert_eq!(origin, ((1280 - 114) / 2, 800 - 58));
    assert_eq!(
        layer::placed_size(&s.comp.state, &server),
        Some(Size::from((114, 58)))
    );
    let hit = |x: i32, y: i32| layer::topmost_layer_at(&s.comp.state, origin.0 + x, origin.1 + y);
    assert!(hit(50, 20).is_some(), "the drawn pill");
    assert!(hit(150, 20).is_none(), "the 200x200 never drawn");
}
