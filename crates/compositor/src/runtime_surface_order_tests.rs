//! Surface trees in scene order: a parent between the subsurfaces its
//! client placed below and above it (`wl_subsurface.place_below` /
//! `place_above`), in every scene that draws one.
//!
//! Smithay's `render_elements_from_surface_tree` returns a tree front to
//! back. The window, popup and layer scenes append trees bottom to top and
//! reverse once ([`crate::layer::front_to_back`]), so each tree goes in
//! through [`surface_tree_bottom_to_top`]; a drag icon is a single tree
//! drawn in Smithay's order. Real clients over socketpairs and Smithay's
//! test renderer, so no GL is needed.

use super::*;
use smithay::backend::renderer::element::{Element, Id};
use smithay::backend::renderer::test::DummyRenderer;
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use wayland_client::{
    protocol::{
        wl_buffer::WlBuffer,
        wl_callback::WlCallback,
        wl_compositor::WlCompositor,
        wl_registry::{self, WlRegistry},
        wl_shm::{Format, WlShm},
        wl_shm_pool::WlShmPool,
        wl_subcompositor::WlSubcompositor,
        wl_subsurface::WlSubsurface,
        wl_surface::WlSurface,
    },
    Connection, Dispatch, EventQueue, Proxy, QueueHandle,
};
use wayland_protocols::xdg::shell::client::{
    xdg_popup::{self, XdgPopup},
    xdg_positioner::{Anchor, Gravity, XdgPositioner},
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{Layer, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{self, ZwlrLayerSurfaceV1},
};

const PUMP_ROUNDS: usize = 200;

#[derive(Default)]
struct Globals {
    compositor: Option<WlCompositor>,
    subcompositor: Option<WlSubcompositor>,
    shm: Option<WlShm>,
    xdg_base: Option<XdgWmBase>,
    layer_shell: Option<ZwlrLayerShellV1>,
    synced: bool,
    /// `xdg_surface` configures acked so far.
    configures: u32,
    popup_configured: bool,
    layer_configured: bool,
}

impl Dispatch<WlRegistry, ()> for Globals {
    fn event(
        state: &mut Self,
        registry: &WlRegistry,
        event: <WlRegistry as Proxy>::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let wl_registry::Event::Global {
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
            "xdg_wm_base" => state.xdg_base = Some(registry.bind(name, version.min(6), qh, ())),
            "zwlr_layer_shell_v1" => {
                state.layer_shell = Some(registry.bind(name, version.min(4), qh, ()))
            }
            _ => {}
        }
    }
}

impl Dispatch<WlCallback, ()> for Globals {
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

impl Dispatch<XdgWmBase, ()> for Globals {
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

impl Dispatch<XdgSurface, ()> for Globals {
    fn event(
        state: &mut Self,
        xdg: &XdgSurface,
        event: <XdgSurface as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg.ack_configure(serial);
            state.configures += 1;
        }
    }
}

impl Dispatch<XdgPopup, ()> for Globals {
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

impl Dispatch<ZwlrLayerSurfaceV1, ()> for Globals {
    fn event(
        state: &mut Self,
        layer: &ZwlrLayerSurfaceV1,
        event: <ZwlrLayerSurfaceV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwlr_layer_surface_v1::Event::Configure { serial, .. } = event {
            layer.ack_configure(serial);
            state.layer_configured = true;
        }
    }
}

macro_rules! empty {
    ($($t:ty),* $(,)?) => {$(
        impl Dispatch<$t, ()> for Globals {
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
    WlCompositor,
    WlSubcompositor,
    WlSubsurface,
    WlSurface,
    WlShm,
    WlShmPool,
    WlBuffer,
    XdgToplevel,
    XdgPositioner,
    ZwlrLayerShellV1,
);

/// One client connection.
struct Peer {
    conn: Connection,
    queue: EventQueue<Globals>,
    qh: QueueHandle<Globals>,
    globals: Globals,
    server: smithay::reexports::wayland_server::Client,
    /// Shm backing, open for as long as the test runs.
    files: Vec<std::fs::File>,
}

impl Peer {
    /// An application client, or with `shell` the supervised shell (the
    /// one client offered layer-shell).
    fn new(comp: &mut crate::TestCompositor, shell: bool) -> Self {
        let (server, stream) = UnixStream::pair().unwrap();
        let server = if shell {
            comp.add_shell_client(server)
        } else {
            comp.add_client(server)
        };
        let conn = Connection::from_socket(stream).unwrap();
        let queue = conn.new_event_queue();
        let qh = queue.handle();
        conn.display().get_registry(&qh, ());
        Self {
            conn,
            queue,
            qh,
            globals: Globals::default(),
            server,
            files: Vec::new(),
        }
    }

    fn surface(&self) -> WlSurface {
        self.globals
            .compositor
            .as_ref()
            .unwrap()
            .create_surface(&self.qh, ())
    }

    /// Attach and commit a `width`x`height` shm buffer of a pool of its own.
    fn draw(&mut self, surface: &WlSurface, width: i32, height: i32) {
        let stride = width * 4;
        let file = tempfile::tempfile().unwrap();
        file.set_len((stride * height) as u64).unwrap();
        let pool = self.globals.shm.as_ref().unwrap().create_pool(
            file.as_fd(),
            stride * height,
            &self.qh,
            (),
        );
        let buffer = pool.create_buffer(0, width, height, stride, Format::Argb8888, &self.qh, ());
        pool.destroy();
        self.files.push(file);
        surface.attach(Some(&buffer), 0, 0);
        surface.damage_buffer(0, 0, width, height);
        surface.commit();
    }

    /// The compositor's handle for a client surface.
    fn server_surface(
        &self,
        comp: &crate::TestCompositor,
        surface: &WlSurface,
    ) -> smithay::reexports::wayland_server::protocol::wl_surface::WlSurface {
        self.server
            .object_from_protocol_id(&comp.display.handle(), surface.id().protocol_id())
            .expect("server surface")
    }

    /// The element id the compositor draws `surface` under.
    fn id(&self, comp: &crate::TestCompositor, surface: &WlSurface) -> Id {
        Id::from_wayland_resource(&self.server_surface(comp, surface))
    }

    /// Give `parent` a subsurface placed below it and one placed above it
    /// (the one below made first, so the default stacking would put it
    /// above), each drawn and waiting for the parent's commit, which then
    /// draws the parent: `(below, above)`.
    fn subsurfaces(&mut self, parent: &WlSurface) -> (WlSurface, WlSurface) {
        let subcompositor = self.globals.subcompositor.clone().unwrap();
        let below = self.surface();
        let below_sub = subcompositor.get_subsurface(&below, parent, &self.qh, ());
        below_sub.set_position(4, 4);
        below_sub.place_below(parent);
        let above = self.surface();
        let above_sub = subcompositor.get_subsurface(&above, parent, &self.qh, ());
        above_sub.set_position(8, 8);
        above_sub.place_above(parent);
        self.draw(&below, 16, 16);
        self.draw(&above, 16, 16);
        self.draw(parent, 64, 48);
        (below, above)
    }
}

/// An application client and the shell, connected to one compositor.
struct Session {
    comp: crate::TestCompositor,
    manager: crate::windows::WindowManager,
    app: Peer,
    shell: Peer,
}

impl Session {
    fn new() -> Self {
        let mut comp = crate::TestCompositor::new();
        let manager = comp.window_manager();
        let app = Peer::new(&mut comp, false);
        let shell = Peer::new(&mut comp, true);
        let mut session = Self {
            comp,
            manager,
            app,
            shell,
        };
        session.pump(|s| {
            let g = &s.app.globals;
            g.compositor.is_some()
                && g.subcompositor.is_some()
                && g.shm.is_some()
                && g.xdg_base.is_some()
                && s.shell.globals.layer_shell.is_some()
                && s.shell.globals.subcompositor.is_some()
                && s.shell.globals.shm.is_some()
        });
        session
    }

    /// Dispatch both clients and the compositor (the manager maps
    /// toplevels and sends their first configure) until `done`.
    fn pump(&mut self, done: impl Fn(&Self) -> bool) {
        for _ in 0..PUMP_ROUNDS {
            for peer in [&mut self.app, &mut self.shell] {
                peer.queue.flush().unwrap();
            }
            self.comp.pump();
            self.manager.reconcile(&mut self.comp.state);
            self.comp.pump();
            for peer in [&mut self.app, &mut self.shell] {
                if let Some(guard) = peer.queue.prepare_read() {
                    guard.read().unwrap();
                }
                peer.queue.dispatch_pending(&mut peer.globals).unwrap();
            }
            if done(self) {
                return;
            }
        }
        panic!("pump budget exhausted");
    }

    fn roundtrip(&mut self) {
        for peer in [&mut self.app, &mut self.shell] {
            peer.globals.synced = false;
            peer.conn.display().sync(&peer.qh, ());
        }
        self.pump(|s| s.app.globals.synced && s.shell.globals.synced);
        for peer in [&self.app, &self.shell] {
            assert!(
                peer.conn.protocol_error().is_none(),
                "{:?}",
                peer.conn.protocol_error()
            );
        }
    }

    /// A mapped toplevel with its two subsurfaces: its tree as the window
    /// scene appends it, the ids of (below, parent, above), the parent and
    /// its xdg_surface (a popup's parent).
    fn window(&mut self) -> (Vec<Id>, [Id; 3], WlSurface, XdgSurface) {
        let surface = self.app.surface();
        let xdg =
            self.app
                .globals
                .xdg_base
                .as_ref()
                .unwrap()
                .get_xdg_surface(&surface, &self.app.qh, ());
        let toplevel = xdg.get_toplevel(&self.app.qh, ());
        toplevel.set_app_id("org.example.Subsurfaces".into());
        surface.commit();
        self.pump(|s| s.app.globals.configures >= 1);
        let (below, above) = self.app.subsurfaces(&surface);
        self.roundtrip();
        let tree = self.bottom_to_top(false, &surface);
        let ids = self.ids(false, [&below, &surface, &above]);
        (tree, ids, surface, xdg)
    }

    fn ids(&self, shell: bool, surfaces: [&WlSurface; 3]) -> [Id; 3] {
        let peer = if shell { &self.shell } else { &self.app };
        surfaces.map(|surface| peer.id(&self.comp, surface))
    }

    /// `surface`'s tree as a scene appends it, bottom to top.
    fn bottom_to_top(&self, shell: bool, surface: &WlSurface) -> Vec<Id> {
        let peer = if shell { &self.shell } else { &self.app };
        let server = peer.server_surface(&self.comp, surface);
        ids(&surface_tree_bottom_to_top(
            &mut DummyRenderer,
            &server,
            (0, 0).into(),
            1.0,
            1.0,
        ))
    }
}

fn ids<E: Element>(elements: &[E]) -> Vec<Id> {
    elements.iter().map(|e| e.id().clone()).collect()
}

#[test]
fn a_window_draws_its_subsurfaces_on_either_side_of_it() {
    let mut s = Session::new();
    let (tree, [below, parent, above], surface, _xdg) = s.window();
    // The premise: Smithay hands a tree over front to back.
    let server = s.app.server_surface(&s.comp, &surface);
    let smithay: Vec<WaylandSurfaceRenderElement<DummyRenderer>> =
        render_elements_from_surface_tree(
            &mut DummyRenderer,
            &server,
            (0, 0),
            1.0,
            1.0,
            Kind::Unspecified,
        );
    assert_eq!(
        ids(&smithay),
        [above.clone(), parent.clone(), below.clone()]
    );
    assert_eq!(tree, [below.clone(), parent.clone(), above.clone()]);
    // What the window scene hands the renderer, first topmost.
    assert_eq!(crate::layer::front_to_back(tree), [above, parent, below]);
}

#[test]
fn a_popup_draws_its_subsurfaces_on_either_side_of_it_above_its_window() {
    let mut s = Session::new();
    let (window, [w_below, window_surface, w_above], _, xdg) = s.window();
    let positioner = s
        .app
        .globals
        .xdg_base
        .as_ref()
        .unwrap()
        .create_positioner(&s.app.qh, ());
    positioner.set_size(64, 48);
    positioner.set_anchor_rect(10, 10, 1, 1);
    positioner.set_anchor(Anchor::TopLeft);
    positioner.set_gravity(Gravity::BottomRight);
    let popup_surface = s.app.surface();
    let popup_xdg =
        s.app
            .globals
            .xdg_base
            .as_ref()
            .unwrap()
            .get_xdg_surface(&popup_surface, &s.app.qh, ());
    let _popup = popup_xdg.get_popup(Some(&xdg), &positioner, &s.app.qh, ());
    popup_surface.commit();
    s.pump(|s| s.app.globals.popup_configured);
    let (below, above) = s.app.subsurfaces(&popup_surface);
    s.roundtrip();
    let [p_below, popup, p_above] = s.ids(false, [&below, &popup_surface, &above]);
    let tree = s.bottom_to_top(false, &popup_surface);
    assert_eq!(tree, [p_below.clone(), popup.clone(), p_above.clone()]);
    // The window scene appends the window's tree, then its popup's.
    let scene = crate::layer::front_to_back([window, tree].concat());
    assert_eq!(
        scene,
        [p_above, popup, p_below, w_above, window_surface, w_below]
    );
}

#[test]
fn a_layer_surface_draws_its_subsurfaces_on_either_side_of_it_above_windows() {
    let mut s = Session::new();
    let (window, [w_below, window_surface, w_above], _, _xdg) = s.window();
    let surface = s.shell.surface();
    let layer = s
        .shell
        .globals
        .layer_shell
        .as_ref()
        .unwrap()
        .get_layer_surface(
            &surface,
            None,
            Layer::Top,
            "tuna-subsurface-order".into(),
            &s.shell.qh,
            (),
        );
    layer.set_size(64, 48);
    surface.commit();
    s.pump(|s| s.shell.globals.layer_configured);
    let (below, above) = s.shell.subsurfaces(&surface);
    s.roundtrip();
    let [l_below, layer_surface, l_above] = s.ids(true, [&below, &surface, &above]);
    let tree = s.bottom_to_top(true, &surface);
    assert_eq!(
        tree,
        [l_below.clone(), layer_surface.clone(), l_above.clone()]
    );
    // Layers go on top of the windows, bottom to top.
    let scene = crate::layer::front_to_back([window, tree].concat());
    assert_eq!(
        scene,
        [
            l_above,
            layer_surface,
            l_below,
            w_above,
            window_surface,
            w_below
        ]
    );
}

#[test]
fn a_drag_icon_draws_its_subsurfaces_on_either_side_of_it() {
    let mut s = Session::new();
    let icon = s.app.surface();
    let (below, above) = s.app.subsurfaces(&icon);
    s.roundtrip();
    let [below, icon_id, above] = s.ids(false, [&below, &icon, &above]);
    s.comp.state.dnd_icon = Some(s.app.server_surface(&s.comp, &icon));
    // A single tree, drawn as Smithay returns it: first topmost.
    let elements = dnd_icon_elements(
        &mut DummyRenderer,
        &s.comp.state,
        View {
            offset: (0, 0),
            scale: 1.0,
        },
        (100.0, 100.0).into(),
    );
    assert_eq!(ids(&elements), [above, icon_id, below]);
}
