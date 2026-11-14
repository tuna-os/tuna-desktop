//! A closed window leaves nothing behind in the compositor (#1088).
//!
//! Every window used to keep its `wl_surface` (and through the surface's
//! data the textures its frames were uploaded to) after it closed: about
//! 1.65 MB per GTK window, so a day of opening and closing windows grew
//! the compositor without bound. A real protocol client here maps a
//! toplevel the way GTK does (an `xdg_dialog_v1` for it, a subsurface, a
//! grabbing popup, shm buffers uploaded to textures, keyboard and pointer
//! focus, a data device), then either destroys every object or just
//! disconnects, as an app that exits does. Each `wl_surface`'s
//! server-side data carries a marker that sets a flag when it is dropped;
//! the flags prove no holder is left anywhere (compositor maps, Smithay
//! protocol objects, seat focus), and the manager's own id-keyed state
//! must have forgotten the window too.
//!
//! Conventions follow `windows.rs` and `layer_teardown.rs`: socketpair
//! clients, a bounded pump budget, plain asserts. Textures come from
//! Smithay's test renderer, which imports shm buffers as the GL upload
//! does and stores them in the same per-surface renderer state.

use std::os::unix::io::AsFd;
use std::os::unix::net::UnixStream;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use smithay::backend::renderer::{
    test::{DummyRenderer, DummyTexture},
    utils::{import_surface_tree, with_renderer_surface_state},
    Renderer,
};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface as ServerSurface;
use tuna_compositor::windows::WindowManager;
use tuna_compositor::TestCompositor;
use wayland_client::{
    protocol::{
        wl_buffer::WlBuffer,
        wl_callback::WlCallback,
        wl_compositor::WlCompositor,
        wl_data_device::WlDataDevice,
        wl_data_device_manager::WlDataDeviceManager,
        wl_display::WlDisplay,
        wl_keyboard::WlKeyboard,
        wl_pointer::WlPointer,
        wl_registry::WlRegistry,
        wl_seat::WlSeat,
        wl_shm::{Format, WlShm},
        wl_shm_pool::WlShmPool,
        wl_subcompositor::WlSubcompositor,
        wl_subsurface::WlSubsurface,
        wl_surface::WlSurface,
    },
    Connection, Dispatch, EventQueue, Proxy, QueueHandle,
};
use wayland_protocols::xdg::dialog::v1::client::{
    xdg_dialog_v1::XdgDialogV1, xdg_wm_dialog_v1::XdgWmDialogV1,
};
use wayland_protocols::xdg::shell::client::{
    xdg_popup::{self, XdgPopup},
    xdg_positioner::{Anchor, Gravity, XdgPositioner},
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

const PUMP_ROUNDS: usize = 200;

#[derive(Default)]
struct Client {
    compositor: Option<WlCompositor>,
    subcompositor: Option<WlSubcompositor>,
    shm: Option<WlShm>,
    seat: Option<WlSeat>,
    keyboard: Option<WlKeyboard>,
    pointer: Option<WlPointer>,
    data_devices: Option<WlDataDeviceManager>,
    data_device: Option<WlDataDevice>,
    xdg_base: Option<XdgWmBase>,
    dialogs: Option<XdgWmDialogV1>,
    synced: bool,
    /// Configures acked so far, toplevel and popup alike.
    configures: u32,
    popup_configured: bool,
}

impl Client {
    fn ready(&self) -> bool {
        self.compositor.is_some()
            && self.subcompositor.is_some()
            && self.shm.is_some()
            && self.keyboard.is_some()
            && self.pointer.is_some()
            && self.data_device.is_some()
            && self.xdg_base.is_some()
            && self.dialogs.is_some()
    }
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
        let wayland_client::protocol::wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        else {
            return;
        };
        match interface.as_str() {
            "wl_compositor" => state.compositor = Some(registry.bind(name, version.min(6), qh, ())),
            "wl_subcompositor" => state.subcompositor = Some(registry.bind(name, 1, qh, ())),
            "wl_shm" => state.shm = Some(registry.bind(name, 1, qh, ())),
            "wl_seat" => {
                let seat: WlSeat = registry.bind(name, version.min(7), qh, ());
                state.keyboard = Some(seat.get_keyboard(qh, ()));
                state.pointer = Some(seat.get_pointer(qh, ()));
                state.seat = Some(seat);
            }
            "wl_data_device_manager" => {
                state.data_devices = Some(registry.bind(name, version.min(3), qh, ()))
            }
            "xdg_wm_base" => state.xdg_base = Some(registry.bind(name, version.min(6), qh, ())),
            "xdg_wm_dialog_v1" => state.dialogs = Some(registry.bind(name, 1, qh, ())),
            _ => {}
        }
        // GTK makes its data device as soon as it has a seat.
        if state.data_device.is_none() {
            if let (Some(manager), Some(seat)) = (&state.data_devices, &state.seat) {
                state.data_device = Some(manager.get_data_device(seat, qh, ()));
            }
        }
    }
}

impl Dispatch<WlCallback, ()> for Client {
    fn event(
        state: &mut Self,
        _: &WlCallback,
        event: <WlCallback as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wayland_client::protocol::wl_callback::Event::Done { .. } = event {
            state.synced = true;
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

/// Configures are acked at once; the test attaches and commits after.
impl Dispatch<XdgSurface, ()> for Client {
    fn event(
        state: &mut Self,
        xdg_surface: &XdgSurface,
        event: <XdgSurface as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg_surface.ack_configure(serial);
            state.configures += 1;
        }
    }
}

impl Dispatch<XdgPopup, ()> for Client {
    fn event(
        state: &mut Self,
        _: &XdgPopup,
        event: <XdgPopup as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_popup::Event::Configure { .. } = event {
            state.popup_configured = true;
        }
    }
}

macro_rules! empty {
    ($($t:ty),* $(,)?) => {$(
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
    )*};
}
empty!(
    WlDisplay,
    WlCompositor,
    WlSubcompositor,
    WlSubsurface,
    WlSurface,
    WlShm,
    WlShmPool,
    WlBuffer,
    WlSeat,
    WlKeyboard,
    WlPointer,
    WlDataDeviceManager,
    WlDataDevice,
    XdgToplevel,
    XdgPositioner,
    XdgWmDialogV1,
    XdgDialogV1,
);

/// Set when the server-side surface data it is stored in is dropped.
struct Freed(Arc<AtomicBool>);

impl Drop for Freed {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// Mark `surface`'s server-side data: the returned flag sets once that
/// data (and the textures stored in it) is gone.
fn watch(surface: &ServerSurface) -> Arc<AtomicBool> {
    let freed = Arc::new(AtomicBool::new(false));
    smithay::wayland::compositor::with_states(surface, |states| {
        assert!(states
            .data_map
            .insert_if_missing_threadsafe(|| Freed(freed.clone())));
    });
    freed
}

/// Whether the server draws `surface` from an imported texture.
fn has_texture(surface: &ServerSurface) -> bool {
    with_renderer_surface_state(surface, |state| {
        state.buffer().is_some()
            && state
                .texture::<DummyTexture>(DummyRenderer.context_id())
                .is_some()
    })
    .unwrap_or(false)
}

/// One loop tick as the runtime runs it: dispatch, reconcile, repick the
/// pointer, alive checks, flush.
fn tick(comp: &mut TestCompositor, manager: &mut WindowManager) {
    comp.pump();
    manager.reconcile(&mut comp.state);
    manager.repick_pointer_window(&mut comp.state);
    manager.run_alive_checks(&mut comp.state, std::time::Instant::now(), false);
    comp.pump();
}

/// One connected client and the compositor it talks to.
struct Session {
    comp: TestCompositor,
    manager: WindowManager,
    conn: Connection,
    queue: EventQueue<Client>,
    qh: QueueHandle<Client>,
    client: Client,
    server_client: smithay::reexports::wayland_server::Client,
}

impl Session {
    fn new() -> Self {
        let mut comp = TestCompositor::new();
        let manager = comp.window_manager();
        let (server, stream) = UnixStream::pair().unwrap();
        let server_client = comp.add_client(server);
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
            server_client,
        };
        session.pump(Client::ready);
        session
    }

    fn tick(&mut self) {
        tick(&mut self.comp, &mut self.manager);
    }

    fn pump(&mut self, done: impl Fn(&Client) -> bool) {
        for _ in 0..PUMP_ROUNDS {
            self.queue.flush().unwrap();
            self.tick();
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

    /// A filled `width`x`height` ARGB buffer in a pool of its own, as
    /// GDK's cairo context makes them.
    fn buffer(&mut self, width: i32, height: i32) -> (WlShmPool, WlBuffer) {
        let stride = width * 4;
        let file = tempfile::tempfile().unwrap();
        std::io::Write::write_all(&mut &file, &vec![0x7f; (stride * height) as usize]).unwrap();
        let pool = self.client.shm.as_ref().unwrap().create_pool(
            file.as_fd(),
            stride * height,
            &self.qh,
            (),
        );
        let buffer = pool.create_buffer(0, width, height, stride, Format::Argb8888, &self.qh, ());
        (pool, buffer)
    }

    fn surface(&self) -> WlSurface {
        self.client
            .compositor
            .as_ref()
            .unwrap()
            .create_surface(&self.qh, ())
    }

    /// The compositor's handle for a client surface.
    fn server(&self, surface: &WlSurface) -> ServerSurface {
        self.server_client
            .object_from_protocol_id(&self.comp.display.handle(), surface.id().protocol_id())
            .expect("server surface")
    }
}

/// Every client object of one GTK-like window.
struct Window {
    surface: WlSurface,
    xdg: XdgSurface,
    toplevel: XdgToplevel,
    dialog: Option<XdgDialogV1>,
    child: WlSurface,
    subsurface: WlSubsurface,
    popup_surface: WlSurface,
    popup_xdg: XdgSurface,
    popup: XdgPopup,
    positioner: XdgPositioner,
    buffers: Vec<(WlShmPool, WlBuffer)>,
}

impl Window {
    /// Destroy every object in protocol order, children first.
    fn destroy(self) {
        self.popup.destroy();
        self.popup_xdg.destroy();
        self.popup_surface.destroy();
        self.positioner.destroy();
        if let Some(dialog) = self.dialog {
            dialog.destroy();
        }
        self.subsurface.destroy();
        self.child.destroy();
        self.toplevel.destroy();
        self.xdg.destroy();
        self.surface.destroy();
        for (pool, buffer) in self.buffers {
            buffer.destroy();
            pool.destroy();
        }
    }
}

/// Server-side flags for one window's surfaces.
struct Watched {
    toplevel: Arc<AtomicBool>,
    subsurface: Arc<AtomicBool>,
    popup: Arc<AtomicBool>,
    id: u64,
}

/// Map a toplevel (optionally with an `xdg_dialog_v1`, modal), a
/// subsurface and a grabbing popup, each with an uploaded shm buffer,
/// with keyboard and pointer focus on it.
fn open_window(s: &mut Session, dialog: bool) -> (Window, Watched) {
    let qh = s.qh.clone();
    let surface = s.surface();
    let xdg = s
        .client
        .xdg_base
        .as_ref()
        .unwrap()
        .get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title("Closing".into());
    toplevel.set_app_id("org.example.Closing".into());
    let dialog = dialog.then(|| {
        let dialog = s
            .client
            .dialogs
            .as_ref()
            .unwrap()
            .get_xdg_dialog(&toplevel, &qh, ());
        dialog.set_modal();
        dialog
    });
    let child = s.surface();
    let subsurface =
        s.client
            .subcompositor
            .as_ref()
            .unwrap()
            .get_subsurface(&child, &surface, &qh, ());
    subsurface.set_position(8, 8);
    surface.commit();
    s.pump(|c| c.configures >= 1);

    let mut buffers = Vec::new();
    let frame = s.buffer(160, 120);
    child.attach(Some(&frame.1), 0, 0);
    child.commit();
    buffers.push(frame);
    let frame = s.buffer(320, 240);
    surface.attach(Some(&frame.1), 0, 0);
    surface.commit();
    buffers.push(frame);
    s.roundtrip();

    let positioner = s
        .client
        .xdg_base
        .as_ref()
        .unwrap()
        .create_positioner(&qh, ());
    positioner.set_size(100, 50);
    positioner.set_anchor_rect(10, 10, 1, 1);
    positioner.set_anchor(Anchor::TopLeft);
    positioner.set_gravity(Gravity::BottomRight);
    let popup_surface = s.surface();
    let popup_xdg = s
        .client
        .xdg_base
        .as_ref()
        .unwrap()
        .get_xdg_surface(&popup_surface, &qh, ());
    let popup = popup_xdg.get_popup(Some(&xdg), &positioner, &qh, ());
    popup.grab(s.client.seat.as_ref().unwrap(), 1);
    popup_surface.commit();
    s.pump(|c| c.popup_configured);
    let frame = s.buffer(100, 50);
    popup_surface.attach(Some(&frame.1), 0, 0);
    popup_surface.commit();
    buffers.push(frame);
    s.roundtrip();

    let server = s.server(&surface);
    let server_child = s.server(&child);
    let server_popup = s.server(&popup_surface);
    // The frame is drawn: every surface of the window has its texture.
    import_surface_tree(&mut DummyRenderer, &server).unwrap();
    import_surface_tree(&mut DummyRenderer, &server_popup).unwrap();
    assert!(has_texture(&server) && has_texture(&server_child) && has_texture(&server_popup));

    // The pointer rests on the window, as it does on a window closed by
    // its own close button.
    let (_, geometry) = s.manager.visible_windows()[0].clone();
    let over = (
        f64::from(geometry.loc.x + 200),
        f64::from(geometry.loc.y + 200),
    );
    s.manager.pointer_motion(&mut s.comp.state, over.into(), 1);
    s.roundtrip();

    let id = s.manager.model().windows().next().expect("mapped").id;
    let watched = Watched {
        toplevel: watch(&server),
        subsurface: watch(&server_child),
        popup: watch(&server_popup),
        id,
    };
    (
        Window {
            surface,
            xdg,
            toplevel,
            dialog,
            child,
            subsurface,
            popup_surface,
            popup_xdg,
            popup,
            positioner,
            buffers,
        },
        watched,
    )
}

/// The window is gone from the manager and every surface's data dropped.
fn assert_released(manager: &WindowManager, comp: &TestCompositor, watched: &Watched) {
    assert!(
        comp.state.toplevels().is_empty(),
        "xdg-shell kept the toplevel"
    );
    assert!(
        manager.visible_windows().is_empty(),
        "manager kept the window"
    );
    assert_eq!(
        manager.window_references(watched.id),
        Vec::<&str>::new(),
        "manager state still names window {}",
        watched.id
    );
    for (what, freed) in [
        ("toplevel", &watched.toplevel),
        ("subsurface", &watched.subsurface),
        ("popup", &watched.popup),
    ] {
        assert!(
            freed.load(Ordering::SeqCst),
            "the closed window's {what} wl_surface (and its texture) was kept"
        );
    }
}

/// The app closes its window and destroys every object, staying
/// connected (an app with more windows, or a dialog closing).
fn close_by_destroying(dialog: bool) {
    let mut s = Session::new();
    let (window, watched) = open_window(&mut s, dialog);
    window.destroy();
    s.roundtrip();
    s.roundtrip();
    assert_released(&s.manager, &s.comp, &watched);
}

/// The app exits: its connection closes, nothing is destroyed first.
fn close_by_disconnecting(dialog: bool) {
    let mut s = Session::new();
    let (window, watched) = open_window(&mut s, dialog);
    let Session {
        mut comp,
        mut manager,
        conn,
        queue,
        qh,
        client,
        server_client,
    } = s;
    drop((window, conn, queue, qh, client, server_client));
    for _ in 0..4 {
        tick(&mut comp, &mut manager);
    }
    assert_released(&manager, &comp, &watched);
}

#[test]
fn a_destroyed_window_frees_its_surfaces() {
    close_by_destroying(false);
}

#[test]
fn a_destroyed_dialog_window_frees_its_surfaces() {
    close_by_destroying(true);
}

#[test]
fn a_window_whose_app_exits_frees_its_surfaces() {
    close_by_disconnecting(false);
}

#[test]
fn a_dialog_window_whose_app_exits_frees_its_surfaces() {
    close_by_disconnecting(true);
}
