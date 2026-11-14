//! A committed `wl_shm` buffer goes back to its client once uploaded (#1084).
//!
//! GTK's cairo renderer (before 4.22) forgets a buffer the compositor
//! still holds when its surface changes size, and never destroys it when
//! the release arrives late: held until replaced, as Smithay does by
//! itself, every overview search that showed results left one buffer in
//! the shell and its pool's descriptor in the compositor. Mutter releases
//! a `wl_shm` buffer as soon as the commit is applied; these tests drive
//! the same contract with real clients: one `wl_buffer.release` per
//! commit, sent by the upload and not by the next attach, and a pool whose
//! descriptor closes as soon as the client lets go of it.
//!
//! Conventions follow `layer_teardown.rs`: socketpair clients, a bounded
//! pump budget, plain asserts. The renderer is Smithay's test renderer,
//! which reads the buffer as an upload does, so no GL is needed.

use std::os::unix::io::{AsFd, AsRawFd};
use std::os::unix::net::UnixStream;

use smithay::backend::renderer::{
    test::{DummyRenderer, DummyTexture},
    utils::with_renderer_surface_state,
    Renderer,
};
use tuna_compositor::{buffer_upload::upload_committed, TestCompositor};
use wayland_client::{
    protocol::{
        wl_buffer::{self, WlBuffer},
        wl_callback::WlCallback,
        wl_compositor::WlCompositor,
        wl_display::WlDisplay,
        wl_registry::WlRegistry,
        wl_shm::{Format, WlShm},
        wl_shm_pool::WlShmPool,
        wl_subcompositor::WlSubcompositor,
        wl_subsurface::WlSubsurface,
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
    subcompositor: Option<WlSubcompositor>,
    layer_shell: Option<ZwlrLayerShellV1>,
    xdg_base: Option<XdgWmBase>,
    shm: Option<WlShm>,
    configured: bool,
    synced: bool,
    /// `wl_buffer.release` events per buffer, by the index it was created with.
    releases: Vec<u32>,
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
                "wl_subcompositor" => state.subcompositor = Some(registry.bind(name, 1, qh, ())),
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
        layer: &ZwlrLayerSurfaceV1,
        event: <ZwlrLayerSurfaceV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let LayerEvent::Configure { serial, .. } = event {
            layer.ack_configure(serial);
            state.configured = true;
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

impl Dispatch<WlBuffer, usize> for Client {
    fn event(
        state: &mut Self,
        _: &WlBuffer,
        event: <WlBuffer as Proxy>::Event,
        index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_buffer::Event::Release = event {
            state.releases[*index] += 1;
        }
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
empty!(WlSubcompositor);
empty!(WlSubsurface);
empty!(WlSurface);
empty!(ZwlrLayerShellV1);
empty!(XdgToplevel);
empty!(WlShm);
empty!(WlShmPool);

/// One connected client and the compositor it talks to.
struct Session {
    comp: TestCompositor,
    conn: Connection,
    queue: EventQueue<Client>,
    qh: QueueHandle<Client>,
    client: Client,
}

/// One buffer the way GDK's cairo context makes them: a pool of its own.
struct ShmBuffer {
    file: std::fs::File,
    pool: WlShmPool,
    buffer: WlBuffer,
    index: usize,
}

impl Session {
    fn new(shell: bool) -> Self {
        let mut comp = TestCompositor::new();
        let (server, stream) = UnixStream::pair().unwrap();
        if shell {
            // The supervised shell: the only client offered layer-shell (#1048).
            comp.add_shell_client(server);
        } else {
            comp.add_client(server);
        }
        let conn = Connection::from_socket(stream).unwrap();
        let queue = conn.new_event_queue();
        let qh = queue.handle();
        conn.display().get_registry(&qh, ());
        let mut session = Self {
            comp,
            conn,
            queue,
            qh,
            client: Client::default(),
        };
        session.pump(|c| {
            c.compositor.is_some()
                && c.subcompositor.is_some()
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

    /// What the runtime does on the tick of a commit: upload, then flush.
    fn upload(&mut self) {
        self.queue.flush().unwrap();
        self.comp.pump();
        upload_committed(&mut DummyRenderer, &mut self.comp.state);
        self.roundtrip();
    }

    fn buffer(&mut self, width: i32, height: i32) -> ShmBuffer {
        let stride = width * 4;
        let file = tempfile::tempfile().unwrap();
        file.set_len((stride * height) as u64).unwrap();
        let pool = self.client.shm.as_ref().unwrap().create_pool(
            file.as_fd(),
            stride * height,
            &self.qh,
            (),
        );
        let index = self.client.releases.len();
        self.client.releases.push(0);
        let buffer =
            pool.create_buffer(0, width, height, stride, Format::Argb8888, &self.qh, index);
        ShmBuffer {
            file,
            pool,
            buffer,
            index,
        }
    }

    fn releases(&self, buffer: &ShmBuffer) -> u32 {
        self.client.releases[buffer.index]
    }

    fn surface(&self) -> WlSurface {
        self.client
            .compositor
            .as_ref()
            .unwrap()
            .create_surface(&self.qh, ())
    }
}

/// How many open fds point at the same file as `file` (the client's own
/// descriptor plus any compositor-side dup received over the socket).
fn backing_count(file: &std::fs::File) -> usize {
    let target =
        std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd())).expect("pool fd");
    std::fs::read_dir("/proc/self/fd")
        .expect("fd dir")
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            std::fs::read_link(entry.path())
                .map(|link| link == target)
                .unwrap_or(false)
        })
        .count()
}

/// Poll `backing_count` until it reaches `want` or the deadline passes:
/// Smithay closes a freed pool on a background thread, a retained one
/// never converges.
fn wait_backing_count(file: &std::fs::File, want: usize, what: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let count = backing_count(file);
        if count == want {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{what}: backing fd count stuck at {count}, want {want}"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// Whether the server still draws `surface` from an uploaded texture.
fn has_texture(
    surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
) -> bool {
    with_renderer_surface_state(surface, |state| {
        state.buffer().is_some()
            && state
                .texture::<DummyTexture>(DummyRenderer.context_id())
                .is_some()
    })
    .unwrap_or(false)
}

/// The overview's search surface: entry-only, then grown for its results
/// the way GDK's cairo context does it.
#[test]
fn a_resized_layer_surface_gets_each_buffer_back_once_it_is_uploaded() {
    let mut s = Session::new(true);
    let surface = s.surface();
    let layer = s.client.layer_shell.as_ref().unwrap().get_layer_surface(
        &surface,
        None,
        Layer::Top,
        tuna_shell_control::OVERVIEW_NAMESPACE.into(),
        &s.qh,
        (),
    );
    layer.set_anchor(Anchor::Top);
    layer.set_size(370, 40);
    surface.commit();
    s.pump(|c| c.configured);

    // The entry-only frame.
    let a = s.buffer(370, 40);
    surface.attach(Some(&a.buffer), 0, 0);
    surface.damage_buffer(0, 0, 370, 40);
    surface.commit();
    s.roundtrip();
    wait_backing_count(&a.file, 2, "the pool's descriptor did not arrive");
    assert_eq!(s.releases(&a), 0, "released before its pixels were read");

    // Uploaded: the buffer comes back although nothing replaced it. Held
    // until replaced, GDK would have forgotten it in the resize below.
    s.upload();
    assert_eq!(
        s.releases(&a),
        1,
        "an uploaded wl_shm buffer was kept until another replaced it"
    );
    let server = s.comp.state.layer_surfaces()[0].wl_surface().clone();
    assert!(has_texture(&server));

    // Results appear. GDK drops every buffer of the old size, so this one,
    // being back, is destroyed with its pool; then the grown surface
    // commits a buffer of the new size.
    a.buffer.destroy();
    a.pool.destroy();
    s.roundtrip();
    assert!(
        has_texture(&server),
        "the surface lost its content with its released buffer"
    );
    let b = s.buffer(845, 203);
    layer.set_size(845, 203);
    surface.attach(Some(&b.buffer), 0, 0);
    surface.damage_buffer(0, 0, 845, 203);
    surface.commit();
    s.upload();
    assert_eq!(s.releases(&b), 1);
    assert_eq!(s.releases(&a), 1, "a buffer was released twice");
    // The destroyed buffer's handle stayed with the texture it was drawn
    // from until now; replaced, its pool closes.
    wait_backing_count(
        &a.file,
        1,
        "the compositor kept the pool of a released, destroyed and replaced buffer",
    );

    // The next frame is drawn into the buffer that came back: one release
    // for each commit.
    surface.attach(Some(&b.buffer), 0, 0);
    surface.damage_buffer(0, 0, 16, 16);
    surface.commit();
    s.upload();
    assert_eq!(
        s.releases(&b),
        2,
        "a buffer attached again was not released again"
    );

    // A buffer replaced before any upload still comes back exactly once.
    let c = s.buffer(845, 203);
    surface.attach(Some(&c.buffer), 0, 0);
    surface.commit();
    surface.attach(Some(&b.buffer), 0, 0);
    surface.commit();
    s.roundtrip();
    assert_eq!(s.releases(&c), 1, "a replaced buffer was not released");
    s.upload();
    assert_eq!(s.releases(&c), 1);
    assert_eq!(s.releases(&b), 3);

    // Hidden (GTK destroys the role and the surface) with a buffer no
    // upload has seen: it comes back with the surface.
    surface.attach(Some(&c.buffer), 0, 0);
    surface.commit();
    s.roundtrip();
    assert_eq!(s.releases(&c), 1);
    layer.destroy();
    surface.destroy();
    s.roundtrip();
    assert_eq!(s.releases(&c), 2, "a hidden surface kept its last buffer");
    s.upload();
    assert_eq!(s.releases(&c), 2, "a buffer was released twice");
    assert_eq!(s.releases(&b), 3, "a buffer was released twice");

    for buffer in [&b, &c] {
        buffer.buffer.destroy();
        buffer.pool.destroy();
    }
    s.roundtrip();
    wait_backing_count(&b.file, 1, "a released buffer's pool stayed open");
    wait_backing_count(&c.file, 1, "a hidden surface's pool stayed open");
}

/// Application windows and their subsurfaces take the same path: a
/// toplevel's commit uploads its whole tree.
#[test]
fn a_toplevel_and_its_subsurface_get_their_buffers_back_once_uploaded() {
    let mut s = Session::new(false);
    // The manager maps the toplevel and sends its first configure.
    let mut manager = s.comp.window_manager();
    let surface = s.surface();
    let xdg = s
        .client
        .xdg_base
        .as_ref()
        .unwrap()
        .get_xdg_surface(&surface, &s.qh, ());
    let _toplevel = xdg.get_toplevel(&s.qh, ());
    surface.commit();
    s.roundtrip();
    manager.reconcile(&mut s.comp.state);
    s.pump(|c| c.configured);

    let child = s.surface();
    let sub = s
        .client
        .subcompositor
        .as_ref()
        .unwrap()
        .get_subsurface(&child, &surface, &s.qh, ());
    sub.set_position(8, 8);

    // A synchronized subsurface's commit is applied by its parent's.
    let child_buffer = s.buffer(64, 32);
    child.attach(Some(&child_buffer.buffer), 0, 0);
    child.commit();
    s.upload();
    assert_eq!(
        s.releases(&child_buffer),
        0,
        "a cached subsurface commit was uploaded before its parent's"
    );

    let parent_buffer = s.buffer(320, 200);
    surface.attach(Some(&parent_buffer.buffer), 0, 0);
    surface.commit();
    s.roundtrip();
    assert_eq!(s.releases(&parent_buffer), 0);
    s.upload();
    assert_eq!(s.releases(&parent_buffer), 1);
    assert_eq!(s.releases(&child_buffer), 1);
    let server = s.comp.state.toplevels()[0].wl_surface().clone();
    assert!(has_texture(&server));

    // The window resizes: both old buffers are back, so the client frees
    // them while the old frame is still shown, then draws both surfaces
    // at the new size.
    for buffer in [&parent_buffer, &child_buffer] {
        buffer.buffer.destroy();
        buffer.pool.destroy();
    }
    s.roundtrip();
    assert!(has_texture(&server));
    let grown_child = s.buffer(128, 64);
    child.attach(Some(&grown_child.buffer), 0, 0);
    child.commit();
    let grown = s.buffer(640, 400);
    surface.attach(Some(&grown.buffer), 0, 0);
    surface.commit();
    s.upload();
    assert_eq!(s.releases(&grown), 1);
    assert_eq!(s.releases(&grown_child), 1);
    assert_eq!(s.releases(&parent_buffer), 1, "a buffer was released twice");
    assert_eq!(s.releases(&child_buffer), 1, "a buffer was released twice");
    wait_backing_count(
        &parent_buffer.file,
        1,
        "the toplevel's old pool stayed open",
    );
    wait_backing_count(
        &child_buffer.file,
        1,
        "the subsurface's old pool stayed open",
    );
}
