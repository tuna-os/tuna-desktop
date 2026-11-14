//! Advertised protocols (#89) and the xdg-activation focus policy.
//!
//! The globals list is golden: adding or dropping a protocol is a
//! deliberate change that updates `EXPECTED` and `docs/protocols.md`
//! together. xdg-decoration stays absent on purpose (Mutter parity).
//! `EXPECTED` is what an ordinary client sees; `SHELL_ONLY` holds the
//! globals the supervised shell's connection sees on top of it (#1048),
//! and `SANDBOX_HIDDEN` the ones a client inside a
//! wp_security_context_v1 does not see (#1046).

use std::collections::BTreeMap;
use std::os::fd::AsFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::time::{Duration, Instant};

use smithay::backend::allocator::{Fourcc, Modifier};
use tuna_compositor::windows::WindowManager;
use tuna_compositor::TestCompositor;
use wayland_client::{
    protocol::{
        wl_compositor::WlCompositor,
        wl_keyboard::{self, WlKeyboard},
        wl_registry::{self, WlRegistry},
        wl_seat::{self, WlSeat},
        wl_surface::WlSurface,
    },
    Connection, Dispatch, EventQueue, QueueHandle,
};
use wayland_protocols::xdg::{
    activation::v1::client::{
        xdg_activation_token_v1::{self, XdgActivationTokenV1},
        xdg_activation_v1::XdgActivationV1,
    },
    shell::client::{
        xdg_surface::{self, XdgSurface},
        xdg_toplevel::XdgToplevel,
        xdg_wm_base::{self, XdgWmBase},
    },
};

use wayland_protocols::wp::linux_dmabuf::zv1::client::{
    zwp_linux_dmabuf_feedback_v1::{self, ZwpLinuxDmabufFeedbackV1},
    zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1,
};
use wayland_protocols::xdg::toplevel_icon::v1::client::{
    xdg_toplevel_icon_manager_v1::XdgToplevelIconManagerV1, xdg_toplevel_icon_v1::XdgToplevelIconV1,
};
wayland_client::delegate_noop!(Client: ignore XdgToplevelIconManagerV1);
wayland_client::delegate_noop!(Client: ignore XdgToplevelIconV1);
wayland_client::delegate_noop!(Client: ignore wayland_client::protocol::wl_shm::WlShm);
wayland_client::delegate_noop!(Client: ignore wayland_client::protocol::wl_shm_pool::WlShmPool);
wayland_client::delegate_noop!(Client: ignore wayland_client::protocol::wl_buffer::WlBuffer);
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::ZwlrLayerShellV1;
wayland_client::delegate_noop!(Client: ignore ZwlrLayerShellV1);
use wayland_protocols::ext::{
    foreign_toplevel_list::v1::client::{
        ext_foreign_toplevel_handle_v1::{self, ExtForeignToplevelHandleV1},
        ext_foreign_toplevel_list_v1::{self, ExtForeignToplevelListV1},
    },
    idle_notify::v1::client::{
        ext_idle_notification_v1::{self, ExtIdleNotificationV1},
        ext_idle_notifier_v1::ExtIdleNotifierV1,
    },
};
use wayland_protocols::wp::{
    content_type::v1::client::{
        wp_content_type_manager_v1::WpContentTypeManagerV1,
        wp_content_type_v1::{self, WpContentTypeV1},
    },
    idle_inhibit::zv1::client::{
        zwp_idle_inhibit_manager_v1::ZwpIdleInhibitManagerV1,
        zwp_idle_inhibitor_v1::ZwpIdleInhibitorV1,
    },
    security_context::v1::client::{
        wp_security_context_manager_v1::WpSecurityContextManagerV1,
        wp_security_context_v1::WpSecurityContextV1,
    },
};
wayland_client::delegate_noop!(Client: ignore ExtIdleNotifierV1);
wayland_client::delegate_noop!(Client: ignore WpContentTypeManagerV1);
wayland_client::delegate_noop!(Client: ignore WpContentTypeV1);
wayland_client::delegate_noop!(Client: ignore ZwpIdleInhibitManagerV1);
wayland_client::delegate_noop!(Client: ignore ZwpIdleInhibitorV1);
wayland_client::delegate_noop!(Client: ignore WpSecurityContextManagerV1);
wayland_client::delegate_noop!(Client: ignore WpSecurityContextV1);

const ROUNDS: usize = 200;

/// Every global an ordinary client sees on a headless compositor, by
/// version. linux-dmabuf joins once a renderer reports formats (see
/// below), and wl_output once an output is connected.
const EXPECTED: &[(&str, u32)] = &[
    ("ext_background_effect_manager_v1", 1),
    ("ext_idle_notifier_v1", 2),
    ("ext_session_lock_manager_v1", 1),
    ("wl_compositor", 6),
    ("wl_data_device_manager", 3),
    ("wl_fixes", 1),
    ("wl_seat", 9),
    ("wl_shm", 2),
    ("wl_subcompositor", 1),
    ("wp_alpha_modifier_v1", 1),
    ("wp_commit_timing_manager_v1", 1),
    ("wp_content_type_manager_v1", 1),
    ("wp_cursor_shape_manager_v1", 2),
    ("wp_fifo_manager_v1", 1),
    ("wp_fractional_scale_manager_v1", 1),
    ("wp_pointer_warp_v1", 1),
    ("wp_presentation", 2),
    ("wp_security_context_manager_v1", 1),
    ("wp_single_pixel_buffer_manager_v1", 1),
    ("wp_viewporter", 1),
    ("xdg_activation_v1", 1),
    ("xdg_session_manager_v1", 1),
    ("xdg_system_bell_v1", 1),
    ("xdg_toplevel_icon_manager_v1", 1),
    ("xdg_toplevel_tag_manager_v1", 1),
    ("xdg_wm_base", 7),
    ("xdg_wm_dialog_v1", 1),
    ("zwp_idle_inhibit_manager_v1", 1),
    ("zwp_keyboard_shortcuts_inhibit_manager_v1", 1),
    ("zwp_pointer_constraints_v1", 1),
    ("zwp_pointer_gestures_v1", 3),
    ("zwp_primary_selection_device_manager_v1", 1),
    ("zwp_relative_pointer_manager_v1", 1),
    ("zwp_tablet_manager_v2", 2),
    ("zwp_text_input_manager_v3", 1),
    ("zxdg_exporter_v1", 1),
    ("zxdg_exporter_v2", 1),
    ("zxdg_importer_v1", 1),
    ("zxdg_importer_v2", 1),
    ("zxdg_output_manager_v1", 3),
];

/// Globals filtered to the supervised shell's connection (#1048): its
/// registry is `EXPECTED` plus these, an ordinary client's lacks them.
/// The foreign toplevel list publishes every window's title and app id,
/// which only the shell may read (docs/security-model.md, #1046).
const SHELL_ONLY: &[(&str, u32)] = &[
    ("ext_foreign_toplevel_list_v1", 1),
    ("zwlr_layer_shell_v1", 4),
];

/// Globals of `EXPECTED` a client inside a wp_security_context_v1 does
/// not see (#1046): it cannot lock the session or nest a context.
const SANDBOX_HIDDEN: &[&str] = &[
    "ext_session_lock_manager_v1",
    "wp_security_context_manager_v1",
];

/// One ext_foreign_toplevel_handle_v1 as its listener sees it.
#[derive(Debug, Default, Clone, PartialEq)]
struct Listed {
    title: String,
    app_id: String,
    closed: bool,
}

#[derive(Default)]
struct Client {
    globals: BTreeMap<String, (u32, u32)>,
    compositor: Option<WlCompositor>,
    seat: Option<WlSeat>,
    wm: Option<XdgWmBase>,
    activation: Option<XdgActivationV1>,
    icons: Option<XdgToplevelIconManagerV1>,
    shm: Option<wayland_client::protocol::wl_shm::WlShm>,
    keyboard_serial: Option<u32>,
    /// Interfaces each tagged registry announced.
    tagged: BTreeMap<u32, Vec<String>>,
    token: Option<String>,
    /// ext-idle-notify events by notification tag: `true` for idled.
    idle_events: Vec<(u32, bool)>,
    /// ext-foreign-toplevel-list handles in announcement order.
    listed: Vec<(ExtForeignToplevelHandleV1, Listed)>,
}

impl Dispatch<WlRegistry, ()> for Client {
    fn event(
        state: &mut Self,
        registry: &WlRegistry,
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
                    state.compositor = Some(registry.bind(name, version.min(6), qh, ()))
                }
                "wl_shm" => state.shm = Some(registry.bind(name, 1, qh, ())),
                "wl_seat" => state.seat = Some(registry.bind(name, version.min(7), qh, ())),
                "xdg_toplevel_icon_manager_v1" => {
                    state.icons = Some(registry.bind(name, 1, qh, ()))
                }
                "xdg_wm_base" => state.wm = Some(registry.bind(name, 1, qh, ())),
                "xdg_activation_v1" => state.activation = Some(registry.bind(name, 1, qh, ())),
                _ => {}
            }
            state.globals.insert(interface, (name, version));
        }
    }
}

impl Dispatch<WlSeat, ()> for Client {
    fn event(
        _: &mut Self,
        seat: &WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: wayland_client::WEnum::Value(caps),
        } = event
        {
            if caps.contains(wl_seat::Capability::Keyboard) {
                seat.get_keyboard(qh, ());
            }
        }
    }
}

impl Dispatch<WlKeyboard, ()> for Client {
    fn event(
        state: &mut Self,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_keyboard::Event::Enter { serial, .. } = event {
            state.keyboard_serial = Some(serial);
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

impl Dispatch<XdgActivationTokenV1, ()> for Client {
    fn event(
        state: &mut Self,
        _: &XdgActivationTokenV1,
        event: xdg_activation_token_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_activation_token_v1::Event::Done { token } = event {
            state.token = Some(token);
        }
    }
}

wayland_client::delegate_noop!(Client: ignore WlCompositor);
wayland_client::delegate_noop!(Client: ignore WlSurface);
wayland_client::delegate_noop!(Client: ignore XdgToplevel);
wayland_client::delegate_noop!(Client: ignore XdgActivationV1);
wayland_client::delegate_noop!(Client: ignore wayland_client::protocol::wl_fixes::WlFixes);

/// A second registry, tagged: the interfaces each one announced.
impl Dispatch<WlRegistry, u32> for Client {
    fn event(
        state: &mut Self,
        _: &WlRegistry,
        event: wl_registry::Event,
        tag: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global { interface, .. } = event {
            state.tagged.entry(*tag).or_default().push(interface);
        }
    }
}

impl Dispatch<ExtIdleNotificationV1, u32> for Client {
    fn event(
        state: &mut Self,
        _: &ExtIdleNotificationV1,
        event: ext_idle_notification_v1::Event,
        tag: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_idle_notification_v1::Event::Idled => state.idle_events.push((*tag, true)),
            ext_idle_notification_v1::Event::Resumed => state.idle_events.push((*tag, false)),
            _ => {}
        }
    }
}

impl Dispatch<ExtForeignToplevelListV1, ()> for Client {
    fn event(
        state: &mut Self,
        _: &ExtForeignToplevelListV1,
        event: ext_foreign_toplevel_list_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let ext_foreign_toplevel_list_v1::Event::Toplevel { toplevel } = event {
            state.listed.push((toplevel, Listed::default()));
        }
    }

    wayland_client::event_created_child!(Client, ExtForeignToplevelListV1, [
        ext_foreign_toplevel_list_v1::EVT_TOPLEVEL_OPCODE => (ExtForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ExtForeignToplevelHandleV1, ()> for Client {
    fn event(
        state: &mut Self,
        handle: &ExtForeignToplevelHandleV1,
        event: ext_foreign_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some((_, listed)) = state.listed.iter_mut().find(|(h, _)| h == handle) else {
            return;
        };
        match event {
            ext_foreign_toplevel_handle_v1::Event::Title { title } => listed.title = title,
            ext_foreign_toplevel_handle_v1::Event::AppId { app_id } => listed.app_id = app_id,
            ext_foreign_toplevel_handle_v1::Event::Closed => listed.closed = true,
            _ => {}
        }
    }
}

struct Peer {
    _conn: Connection,
    registry: WlRegistry,
    queue: EventQueue<Client>,
    client: Client,
}

fn connect(comp: &mut TestCompositor) -> Peer {
    connect_with(comp, TestCompositor::add_client)
}

fn connect_with(
    comp: &mut TestCompositor,
    admit: fn(&mut TestCompositor, UnixStream) -> wayland_server::Client,
) -> Peer {
    let (server, stream) = UnixStream::pair().unwrap();
    admit(comp, server);
    peer(stream)
}

/// A client on `stream`, its registry requested.
fn peer(stream: UnixStream) -> Peer {
    let conn = Connection::from_socket(stream).unwrap();
    let queue = conn.new_event_queue();
    let registry = conn.display().get_registry(&queue.handle(), ());
    Peer {
        _conn: conn,
        registry,
        queue,
        client: Client::default(),
    }
}

/// Pump until `peer`'s connection dies of a protocol error; whether it
/// did within the round budget.
fn refused(comp: &mut TestCompositor, peer: &mut Peer) -> bool {
    for _ in 0..ROUNDS {
        let _ = peer.queue.flush();
        comp.pump();
        if let Some(guard) = peer.queue.prepare_read() {
            let _ = guard.read();
        }
        let _ = peer.queue.dispatch_pending(&mut peer.client);
        if peer._conn.backend().last_error().is_some() {
            return true;
        }
    }
    false
}

fn pump(comp: &mut TestCompositor, manager: &mut WindowManager, peers: &mut [&mut Peer]) {
    for _ in 0..ROUNDS / 20 {
        for p in peers.iter_mut() {
            p.queue.flush().unwrap();
        }
        comp.pump();
        manager.reconcile(&mut comp.state);
        comp.pump();
        for p in peers.iter_mut() {
            if let Some(guard) = p.queue.prepare_read() {
                let _ = guard.read();
            }
            p.queue.dispatch_pending(&mut p.client).unwrap();
        }
    }
}

/// Map one toplevel; returns its surface.
fn window(peer: &mut Peer, title: &str) -> WlSurface {
    let qh = peer.queue.handle();
    let surface = peer
        .client
        .compositor
        .as_ref()
        .unwrap()
        .create_surface(&qh, ());
    let xdg = peer
        .client
        .wm
        .as_ref()
        .unwrap()
        .get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title(title.into());
    surface.commit();
    // The protocol objects live with the connection.
    std::mem::forget((xdg, toplevel));
    surface
}

fn focused_title(manager: &WindowManager) -> Option<String> {
    let id = manager.model().focused()?;
    manager
        .model()
        .windows()
        .find(|w| w.id == id)
        .map(|w| w.title.clone())
}

#[test]
fn advertised_globals_match_the_golden_list() {
    let mut comp = TestCompositor::new();
    let mut manager = comp.window_manager();
    let mut peer = connect(&mut comp);
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    let seen: Vec<(String, u32)> = peer
        .client
        .globals
        .iter()
        .map(|(k, (_, v))| (k.clone(), *v))
        .collect();
    let expected: Vec<(String, u32)> = EXPECTED
        .iter()
        .map(|(k, v)| ((*k).to_owned(), *v))
        .collect();
    assert_eq!(
        seen, expected,
        "update EXPECTED and docs/protocols.md together"
    );
    assert!(
        !peer
            .client
            .globals
            .contains_key("zxdg_decoration_manager_v1"),
        "GNOME is client-side decorations only"
    );
}

fn seen_globals(peer: &Peer) -> Vec<(String, u32)> {
    peer.client
        .globals
        .iter()
        .map(|(k, (_, v))| (k.clone(), *v))
        .collect()
}

#[test]
fn layer_shell_is_visible_and_bindable_only_by_the_supervised_shell() {
    let mut comp = TestCompositor::new();
    let mut manager = comp.window_manager();
    let mut app = connect(&mut comp);
    let mut shell = connect_with(&mut comp, TestCompositor::add_shell_client);
    pump(&mut comp, &mut manager, &mut [&mut app, &mut shell]);

    // The shell's registry is the ordinary one plus the filtered globals.
    let mut expected: Vec<(String, u32)> = EXPECTED
        .iter()
        .chain(SHELL_ONLY)
        .map(|(k, v)| ((*k).to_owned(), *v))
        .collect();
    expected.sort();
    assert_eq!(seen_globals(&shell), expected);
    for (interface, _) in SHELL_ONLY {
        assert!(
            !app.client.globals.contains_key(*interface),
            "{interface} must stay out of an ordinary client's registry"
        );
    }

    // Knowing the global's name (from the shell's registry) must not let
    // an ordinary client bind it blind: the bind is a protocol error.
    let (name, _) = shell.client.globals["zwlr_layer_shell_v1"];
    let _forced: ZwlrLayerShellV1 = app.registry.bind(name, 4, &app.queue.handle(), ());
    assert!(
        refused(&mut comp, &mut app),
        "a blind bind of the shell-only global must fail"
    );
    // The shell's own connection is untouched by the denial.
    let _bound: ZwlrLayerShellV1 = shell.registry.bind(name, 4, &shell.queue.handle(), ());
    pump(&mut comp, &mut manager, &mut [&mut shell]);
    assert!(shell._conn.backend().last_error().is_none());
}

#[test]
fn dmabuf_is_advertised_only_with_renderer_formats() {
    let mut comp = TestCompositor::new();
    let mut manager = comp.window_manager();
    comp.state.enable_dmabuf(Vec::new(), Some(RENDER_NODE));
    let mut peer = connect(&mut comp);
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert!(!peer.client.globals.contains_key("zwp_linux_dmabuf_v1"));

    // No renderer device: version 3, as Mutter without
    // EGL_EXT_device_drm(_render_node).
    comp.state.enable_dmabuf(
        vec![smithay::backend::allocator::Format {
            code: Fourcc::Argb8888,
            modifier: Modifier::Linear,
        }],
        None,
    );
    let mut late = connect(&mut comp);
    pump(&mut comp, &mut manager, &mut [&mut late]);
    assert_eq!(
        late.client.globals.get("zwp_linux_dmabuf_v1").map(|g| g.1),
        Some(3)
    );
}

/// A stand-in import device: the gate only needs one to exist (timeline
/// imports on it fail).
#[cfg(feature = "drm")]
fn stand_in_drm_device() -> smithay::backend::drm::DrmDeviceFd {
    let file = std::fs::File::open("/dev/null").unwrap();
    smithay::backend::drm::DrmDeviceFd::new(smithay::utils::DeviceFd::from(
        std::os::fd::OwnedFd::from(file),
    ))
}

/// Mutter creates wp_linux_drm_syncobj_manager_v1 only when every check of
/// `meta_wayland_drm_syncobj_manager_new` passes; one missing capability
/// keeps it away, all of them give version 1.
#[cfg(feature = "drm")]
#[test]
fn explicit_sync_is_advertised_only_with_timeline_syncobj() {
    use tuna_compositor::explicit_sync::SyncobjSupport;
    let capable = || SyncobjSupport {
        native_fence_sync: true,
        device_path: Some("/dev/dri/renderD128".into()),
        device: Some(stand_in_drm_device()),
        timeline: true,
        eventfd: true,
    };
    type Without = fn(&mut SyncobjSupport);
    let lacking: [(&str, Without); 5] = [
        ("native fence sync", |s| s.native_fence_sync = false),
        ("EGL device node", |s| s.device_path = None),
        ("opened device", |s| s.device = None),
        ("timeline syncobj", |s| s.timeline = false),
        ("syncobj eventfd", |s| s.eventfd = false),
    ];
    for (missing, without) in lacking {
        let mut comp = TestCompositor::new();
        let mut manager = comp.window_manager();
        let mut support = capable();
        without(&mut support);
        assert!(!comp.state.enable_explicit_sync(support), "{missing}");
        let mut peer = connect(&mut comp);
        pump(&mut comp, &mut manager, &mut [&mut peer]);
        assert!(
            !peer
                .client
                .globals
                .contains_key("wp_linux_drm_syncobj_manager_v1"),
            "advertised without {missing}"
        );
    }

    let mut comp = TestCompositor::new();
    let mut manager = comp.window_manager();
    assert!(comp.state.enable_explicit_sync(capable()));
    let mut peer = connect(&mut comp);
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert_eq!(
        peer.client
            .globals
            .get("wp_linux_drm_syncobj_manager_v1")
            .map(|g| g.1),
        Some(1)
    );
}

/// renderD128's device number (major 226, minor 128).
const RENDER_NODE: u64 = (226 << 8) | 128;

#[derive(Default)]
struct Feedback {
    main_device: Option<Vec<u8>>,
    table: Option<(std::os::fd::OwnedFd, u32)>,
    target_device: Option<Vec<u8>>,
    indices: Vec<u16>,
    tranche_flags: Option<u32>,
    tranches: usize,
    done: bool,
}

#[derive(Default)]
struct DmabufClient {
    globals: BTreeMap<String, (u32, u32)>,
    feedback: Feedback,
}

impl Dispatch<WlRegistry, ()> for DmabufClient {
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
            state.globals.insert(interface, (name, version));
        }
    }
}

impl Dispatch<ZwpLinuxDmabufFeedbackV1, ()> for DmabufClient {
    fn event(
        state: &mut Self,
        _: &ZwpLinuxDmabufFeedbackV1,
        event: zwp_linux_dmabuf_feedback_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use zwp_linux_dmabuf_feedback_v1::Event;
        let feedback = &mut state.feedback;
        match event {
            Event::MainDevice { device } => feedback.main_device = Some(device),
            Event::FormatTable { fd, size } => feedback.table = Some((fd, size)),
            Event::TrancheTargetDevice { device } => feedback.target_device = Some(device),
            Event::TrancheFormats { indices } => feedback.indices.extend(
                indices
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|i| u16::from_ne_bytes(*i)),
            ),
            Event::TrancheFlags { flags } => {
                feedback.tranche_flags = Some(match flags {
                    wayland_client::WEnum::Value(flags) => flags.bits(),
                    wayland_client::WEnum::Unknown(bits) => bits,
                })
            }
            Event::TrancheDone => feedback.tranches += 1,
            Event::Done => feedback.done = true,
            _ => {}
        }
    }
}

wayland_client::delegate_noop!(DmabufClient: ignore ZwpLinuxDmabufV1);

#[test]
fn dmabuf_feedback_names_the_renderer_device_like_mutter() {
    // Mutter 51 (meta-wayland-dma-buf.c): with the EGL device's node the
    // global is version 5 and the default feedback names that device
    // with one tranche of the renderer's formats.
    let mut comp = TestCompositor::new();
    let formats = [
        (Fourcc::Argb8888, Modifier::Linear),
        (Fourcc::Xrgb8888, Modifier::Invalid),
    ];
    comp.state.enable_dmabuf(
        formats
            .iter()
            .map(|&(code, modifier)| smithay::backend::allocator::Format { code, modifier })
            .collect(),
        Some(RENDER_NODE),
    );
    let (server, stream) = UnixStream::pair().unwrap();
    comp.add_client(server);
    let conn = Connection::from_socket(stream).unwrap();
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    let registry = conn.display().get_registry(&qh, ());
    let mut client = DmabufClient::default();
    let mut pump = |client: &mut DmabufClient, comp: &mut TestCompositor| {
        for _ in 0..10 {
            queue.flush().unwrap();
            comp.pump();
            if let Some(guard) = queue.prepare_read() {
                let _ = guard.read();
            }
            queue.dispatch_pending(client).unwrap();
        }
    };
    pump(&mut client, &mut comp);
    let (name, version) = client.globals["zwp_linux_dmabuf_v1"];
    assert_eq!(version, 5);
    let dmabuf: ZwpLinuxDmabufV1 = registry.bind(name, 5, &qh, ());
    let _feedback = dmabuf.get_default_feedback(&qh, ());
    pump(&mut client, &mut comp);

    let feedback = &client.feedback;
    assert!(feedback.done);
    let device = RENDER_NODE.to_ne_bytes().to_vec();
    assert_eq!(feedback.main_device.as_ref(), Some(&device));
    assert_eq!(feedback.target_device.as_ref(), Some(&device));
    assert_eq!(feedback.tranches, 1);
    assert_eq!(feedback.tranche_flags, Some(0), "no scanout tranche");

    // The format table: 16-byte (format, padding, modifier) entries the
    // tranche indexes.
    let (fd, size) = feedback.table.as_ref().expect("format table");
    let mut table = vec![0u8; *size as usize];
    let mut file = std::fs::File::from(fd.try_clone().unwrap());
    std::io::Read::read_exact(&mut file, &mut table).unwrap();
    let mut offered: Vec<(u32, u64)> = feedback
        .indices
        .iter()
        .map(|&i| {
            let entry = &table[usize::from(i) * 16..usize::from(i) * 16 + 16];
            (
                u32::from_ne_bytes(entry[0..4].try_into().unwrap()),
                u64::from_ne_bytes(entry[8..16].try_into().unwrap()),
            )
        })
        .collect();
    offered.sort();
    let mut expected: Vec<(u32, u64)> = formats
        .iter()
        .map(|&(code, modifier)| (code as u32, u64::from(modifier)))
        .collect();
    expected.sort();
    assert_eq!(offered, expected);
}

#[test]
fn wl_fixes_destroys_a_registry_and_its_announcements_stop() {
    // Mutter 51 (meta-wayland-fixes.c): destroy_registry destroys the
    // registry resource; later globals are announced to live registries
    // only.
    let mut comp = TestCompositor::new();
    let mut manager = comp.window_manager();
    let mut peer = connect(&mut comp);
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    let qh = peer.queue.handle();
    let (name, version) = peer.client.globals["wl_fixes"];
    assert_eq!(version, 1);
    let kept = peer._conn.display().get_registry(&qh, 1u32);
    let dropped = peer._conn.display().get_registry(&qh, 2u32);
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    let fixes: wayland_client::protocol::wl_fixes::WlFixes = kept.bind(name, 1, &qh, ());
    assert!(peer.client.tagged[&2].iter().any(|i| i == "wl_compositor"));
    fixes.destroy_registry(&dropped);
    pump(&mut comp, &mut manager, &mut [&mut peer]);

    // A new global: the renderer reports dmabuf formats.
    let dmabuf = "zwp_linux_dmabuf_v1";
    assert!(!peer.client.tagged[&1].iter().any(|i| i == dmabuf));
    comp.state.enable_dmabuf(
        vec![smithay::backend::allocator::Format {
            code: Fourcc::Argb8888,
            modifier: Modifier::Linear,
        }],
        None,
    );
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert!(
        peer.client.tagged[&1].iter().any(|i| i == dmabuf),
        "a live registry hears the new global"
    );
    assert!(
        !peer.client.tagged[&2].iter().any(|i| i == dmabuf),
        "a destroyed registry hears nothing more"
    );
    fixes.destroy();
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert!(peer.queue.dispatch_pending(&mut peer.client).is_ok());
}

fn mint(peer: &mut Peer, with_serial: bool) -> XdgActivationTokenV1 {
    let qh = peer.queue.handle();
    let token = peer
        .client
        .activation
        .as_ref()
        .unwrap()
        .get_activation_token(&qh, ());
    if with_serial {
        let serial = peer.client.keyboard_serial.expect("keyboard enter");
        token.set_serial(serial, peer.client.seat.as_ref().unwrap());
    }
    token.commit();
    token
}

#[test]
fn focused_client_can_hand_focus_with_a_token() {
    let mut comp = TestCompositor::new();
    let mut manager = comp.window_manager();
    let mut peer = connect(&mut comp);
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    let first = window(&mut peer, "first");
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    let _second = window(&mut peer, "second");
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert_eq!(focused_title(&manager).as_deref(), Some("second"));

    let _token = mint(&mut peer, true);
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    let token = peer.client.token.take().expect("token issued");
    peer.client
        .activation
        .as_ref()
        .unwrap()
        .activate(token.clone(), &first);
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert_eq!(focused_title(&manager).as_deref(), Some("first"));

    // One use: replaying the token after focus moves changes nothing.
    manager.focus(
        &mut comp.state,
        manager.model().windows().map(|w| w.id).max(),
    );
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    peer.client
        .activation
        .as_ref()
        .unwrap()
        .activate(token, &first);
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert_eq!(focused_title(&manager).as_deref(), Some("second"));
}

#[test]
fn token_activation_raises_only_with_raise_on_click() {
    // Mutter activates xdg-activation requests as META_CLIENT_TYPE_APPLICATION:
    // with raise-on-click off they focus without raising (window.c).
    let mut comp = TestCompositor::new();
    let mut manager = comp.window_manager();
    manager.set_focus_policy(tuna_compositor::focus_policy::FocusPolicy {
        raise_on_click: false,
        ..Default::default()
    });
    let mut peer = connect(&mut comp);
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    let first = window(&mut peer, "first");
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    let _second = window(&mut peer, "second");
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    let second_id = manager.model().focused().unwrap();
    let _token = mint(&mut peer, true);
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    let token = peer.client.token.take().expect("token issued");
    peer.client
        .activation
        .as_ref()
        .unwrap()
        .activate(token, &first);
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert_eq!(focused_title(&manager).as_deref(), Some("first"));
    assert_eq!(
        manager.stacking_order().last(),
        Some(&second_id),
        "focused beneath the window that was on top"
    );
}

#[test]
fn background_clients_and_serial_less_tokens_cannot_steal_focus() {
    let mut comp = TestCompositor::new();
    let mut manager = comp.window_manager();
    let mut back = connect(&mut comp);
    let mut front = connect(&mut comp);
    pump(&mut comp, &mut manager, &mut [&mut back, &mut front]);
    let back_window = window(&mut back, "back");
    pump(&mut comp, &mut manager, &mut [&mut back, &mut front]);
    let _front_window = window(&mut front, "front");
    pump(&mut comp, &mut manager, &mut [&mut back, &mut front]);
    assert_eq!(focused_title(&manager).as_deref(), Some("front"));

    // The background client still has its old enter serial, but it no
    // longer holds keyboard focus.
    let _t = mint(&mut back, true);
    pump(&mut comp, &mut manager, &mut [&mut back, &mut front]);
    let token = back
        .client
        .token
        .take()
        .expect("a token string is always sent");
    back.client
        .activation
        .as_ref()
        .unwrap()
        .activate(token, &back_window);
    pump(&mut comp, &mut manager, &mut [&mut back, &mut front]);
    assert_eq!(focused_title(&manager).as_deref(), Some("front"));

    // The focused client without an input serial is refused too.
    let _t = mint(&mut front, false);
    pump(&mut comp, &mut manager, &mut [&mut back, &mut front]);
    let token = front.client.token.take().expect("token string");
    front
        .client
        .activation
        .as_ref()
        .unwrap()
        .activate(token, &back_window);
    pump(&mut comp, &mut manager, &mut [&mut back, &mut front]);
    assert_eq!(focused_title(&manager).as_deref(), Some("front"));
}

#[test]
fn toplevel_icon_assignment_is_double_buffered_and_survives_resource_destroy() {
    let mut comp = TestCompositor::new();
    let mut manager = comp.window_manager();
    let mut peer = connect(&mut comp);
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    let qh = peer.queue.handle();
    let surface = peer
        .client
        .compositor
        .as_ref()
        .unwrap()
        .create_surface(&qh, ());
    let xdg = peer
        .client
        .wm
        .as_ref()
        .unwrap()
        .get_xdg_surface(&surface, &qh, ());
    let top = xdg.get_toplevel(&qh, ());
    surface.commit();
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    let icons = peer.client.icons.as_ref().unwrap();
    let icon = icons.create_icon(&qh, ());
    icon.set_name("utilities-terminal".into());
    use std::io::Write;
    use std::os::fd::AsFd;
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(&0xff123456u32.to_ne_bytes()).unwrap();
    let pool = peer
        .client
        .shm
        .as_ref()
        .unwrap()
        .create_pool(file.as_fd(), 4, &qh, ());
    let buffer = pool.create_buffer(
        0,
        1,
        1,
        4,
        wayland_client::protocol::wl_shm::Format::Argb8888,
        &qh,
        (),
    );
    icon.add_buffer(&buffer, 1);
    icons.set_icon(&top, Some(&icon));
    icon.destroy();
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert!(manager.model().windows().next().unwrap().icon.is_none());
    surface.commit();
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    let path = manager
        .model()
        .windows()
        .next()
        .unwrap()
        .icon
        .clone()
        .unwrap();
    assert!(path.starts_with('/'));
    assert_eq!(
        image::open(path).unwrap().to_rgba8().as_raw(),
        &[0x12, 0x34, 0x56, 255]
    );
    buffer.destroy();
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    peer.client.icons.as_ref().unwrap().set_icon(&top, None);
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert!(manager.model().windows().next().unwrap().icon.is_some());
    surface.commit();
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert!(manager.model().windows().next().unwrap().icon.is_none());
}

#[test]
fn security_context_clients_see_no_privileged_global() {
    let mut comp = TestCompositor::new();
    let mut manager = comp.window_manager();
    let mut engine = connect(&mut comp);
    pump(&mut comp, &mut manager, &mut [&mut engine]);

    // A sandbox engine (Flatpak) commits a context on a listener of its
    // own; the context lives until the engine closes its end of close_fd.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wayland-sandbox");
    let listener = UnixListener::bind(&path).unwrap();
    let (close_fd, _engine_end) = UnixStream::pair().unwrap();
    let qh = engine.queue.handle();
    let (name, _) = engine.client.globals["wp_security_context_manager_v1"];
    let contexts: WpSecurityContextManagerV1 = engine.registry.bind(name, 1, &qh, ());
    let context = contexts.create_listener(listener.as_fd(), close_fd.as_fd(), &qh, ());
    context.set_sandbox_engine("org.flatpak".into());
    context.set_app_id("org.example.Sandboxed".into());
    context.set_instance_id("42".into());
    context.commit();
    pump(&mut comp, &mut manager, &mut [&mut engine]);

    let mut sandboxed = peer(UnixStream::connect(&path).unwrap());
    pump(&mut comp, &mut manager, &mut [&mut engine, &mut sandboxed]);
    let admitted = comp.security_context_clients();
    assert_eq!(admitted.len(), 1, "the listener admits its client");
    assert_eq!(admitted[0].sandbox_engine.as_deref(), Some("org.flatpak"));
    assert_eq!(admitted[0].app_id.as_deref(), Some("org.example.Sandboxed"));
    assert_eq!(admitted[0].instance_id.as_deref(), Some("42"));

    // Ordinary globals only: no session lock, no nested context, and
    // nothing the shell alone is offered.
    let expected: Vec<(String, u32)> = EXPECTED
        .iter()
        .filter(|(interface, _)| !SANDBOX_HIDDEN.contains(interface))
        .map(|(k, v)| ((*k).to_owned(), *v))
        .collect();
    assert_eq!(seen_globals(&sandboxed), expected);
    for (interface, _) in SHELL_ONLY {
        assert!(
            !sandboxed.client.globals.contains_key(*interface),
            "{interface} must stay out of a sandboxed client's registry"
        );
    }

    // Knowing the manager's name does not let it nest a context.
    let _nested: WpSecurityContextManagerV1 =
        sandboxed
            .registry
            .bind(name, 1, &sandboxed.queue.handle(), ());
    assert!(
        refused(&mut comp, &mut sandboxed),
        "a client inside a security context must not bind the manager"
    );
}

/// Tags of the two ext-idle-notify notifications the tests create.
const PLAIN: u32 = 1;
const INPUT_ONLY: u32 = 2;

/// Bind ext_idle_notifier_v1 at the version the compositor offers (v2).
fn idle_notifier(peer: &Peer) -> ExtIdleNotifierV1 {
    let (name, version) = peer.client.globals["ext_idle_notifier_v1"];
    assert_eq!(version, 2, "get_input_idle_notification needs v2");
    peer.registry.bind(name, 2, &peer.queue.handle(), ())
}

#[test]
fn idle_notifications_fire_after_their_timeout_and_resume_on_input() {
    let mut comp = TestCompositor::new();
    let mut manager = comp.window_manager();
    let mut peer = connect(&mut comp);
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    let qh = peer.queue.handle();
    let notifier = idle_notifier(&peer);
    let seat = peer.client.seat.clone().unwrap();
    let start = Instant::now();
    let at = |secs| start + Duration::from_secs(secs);
    let _plain = notifier.get_idle_notification(10_000, &seat, &qh, PLAIN);
    let _input = notifier.get_input_idle_notification(10_000, &seat, &qh, INPUT_ONLY);
    pump(&mut comp, &mut manager, &mut [&mut peer]);

    comp.idle_tick(at(5));
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert!(peer.client.idle_events.is_empty(), "not idle yet");

    comp.idle_tick(at(60));
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert_eq!(
        std::mem::take(&mut peer.client.idle_events),
        [(PLAIN, true), (INPUT_ONLY, true)]
    );
    comp.idle_tick(at(61));
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert!(peer.client.idle_events.is_empty(), "idled once per stretch");

    // Input resumes both and restarts their timeouts.
    comp.idle_activity(at(62));
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert_eq!(
        std::mem::take(&mut peer.client.idle_events),
        [(PLAIN, false), (INPUT_ONLY, false)]
    );
    comp.idle_tick(at(70));
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert!(peer.client.idle_events.is_empty(), "the timeout restarted");
    comp.idle_tick(at(72));
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert_eq!(
        std::mem::take(&mut peer.client.idle_events),
        [(PLAIN, true), (INPUT_ONLY, true)]
    );
}

#[test]
fn idle_inhibitors_hold_back_only_ordinary_idle_notifications() {
    let mut comp = TestCompositor::new();
    let mut manager = comp.window_manager();
    let mut peer = connect(&mut comp);
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    let qh = peer.queue.handle();
    let notifier = idle_notifier(&peer);
    let seat = peer.client.seat.clone().unwrap();
    let (name, _) = peer.client.globals["zwp_idle_inhibit_manager_v1"];
    let inhibit: ZwpIdleInhibitManagerV1 = peer.registry.bind(name, 1, &qh, ());
    let surface = peer
        .client
        .compositor
        .as_ref()
        .unwrap()
        .create_surface(&qh, ());
    let inhibitor = inhibit.create_inhibitor(&surface, &qh, ());
    let start = Instant::now();
    let at = |secs| start + Duration::from_secs(secs);
    let _plain = notifier.get_idle_notification(10_000, &seat, &qh, PLAIN);
    let _input = notifier.get_input_idle_notification(10_000, &seat, &qh, INPUT_ONLY);
    pump(&mut comp, &mut manager, &mut [&mut peer]);

    // get_input_idle_notification ignores the inhibitor; the other waits.
    comp.idle_tick(at(60));
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert_eq!(
        std::mem::take(&mut peer.client.idle_events),
        [(INPUT_ONLY, true)]
    );

    // Once the inhibitor goes, the held notification's timeout starts.
    inhibitor.destroy();
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    comp.idle_tick(at(65));
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert!(peer.client.idle_events.is_empty());
    comp.idle_tick(at(76));
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert_eq!(
        std::mem::take(&mut peer.client.idle_events),
        [(PLAIN, true)]
    );

    // A new inhibitor ends the ordinary notification's idleness only.
    let _again = inhibit.create_inhibitor(&surface, &qh, ());
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    comp.idle_tick(at(77));
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert_eq!(
        std::mem::take(&mut peer.client.idle_events),
        [(PLAIN, false)]
    );
}

fn listed(peer: &Peer) -> Vec<Listed> {
    peer.client
        .listed
        .iter()
        .map(|(_, listed)| listed.clone())
        .collect()
}

#[test]
fn the_shell_lists_toplevels_until_they_close_and_apps_cannot_bind_the_list() {
    let mut comp = TestCompositor::new();
    let mut manager = comp.window_manager();
    let mut app = connect(&mut comp);
    let mut shell = connect_with(&mut comp, TestCompositor::add_shell_client);
    pump(&mut comp, &mut manager, &mut [&mut app, &mut shell]);
    let (name, version) = shell.client.globals["ext_foreign_toplevel_list_v1"];
    assert_eq!(version, 1);
    let _list: ExtForeignToplevelListV1 = shell.registry.bind(name, 1, &shell.queue.handle(), ());
    pump(&mut comp, &mut manager, &mut [&mut app, &mut shell]);
    assert!(listed(&shell).is_empty());

    let qh = app.queue.handle();
    let surface = app
        .client
        .compositor
        .as_ref()
        .unwrap()
        .create_surface(&qh, ());
    let xdg = app
        .client
        .wm
        .as_ref()
        .unwrap()
        .get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title("Notes".into());
    toplevel.set_app_id("org.example.Notes".into());
    surface.commit();
    pump(&mut comp, &mut manager, &mut [&mut app, &mut shell]);
    let mut notes = Listed {
        title: "Notes".into(),
        app_id: "org.example.Notes".into(),
        closed: false,
    };
    assert_eq!(listed(&shell), [notes.clone()]);

    // A retitle reaches the same handle.
    toplevel.set_title("Notes - draft".into());
    surface.commit();
    pump(&mut comp, &mut manager, &mut [&mut app, &mut shell]);
    notes.title = "Notes - draft".into();
    assert_eq!(listed(&shell), [notes.clone()]);

    // Unmapping the window closes its handle.
    toplevel.destroy();
    pump(&mut comp, &mut manager, &mut [&mut app, &mut shell]);
    notes.closed = true;
    assert_eq!(listed(&shell), [notes]);

    // Titles and app ids are the shell's alone: an ordinary client that
    // learned the global's name still cannot bind it.
    let _forced: ExtForeignToplevelListV1 = app.registry.bind(name, 1, &app.queue.handle(), ());
    assert!(
        refused(&mut comp, &mut app),
        "a blind bind of the foreign toplevel list must fail"
    );
}

#[test]
fn content_type_hint_is_recorded_per_surface_on_commit() {
    use smithay::reexports::wayland_protocols::wp::content_type::v1::server::wp_content_type_v1::Type;
    use tuna_compositor::protocols::surface_content_type;

    let mut comp = TestCompositor::new();
    let mut manager = comp.window_manager();
    let mut peer = connect(&mut comp);
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    let surface = window(&mut peer, "film");
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    let id = manager.model().windows().next().unwrap().id;
    let mapped = manager.surface_of(id).unwrap();
    assert_eq!(surface_content_type(&mapped), Type::None);

    let qh = peer.queue.handle();
    let (name, _) = peer.client.globals["wp_content_type_manager_v1"];
    let types: WpContentTypeManagerV1 = peer.registry.bind(name, 1, &qh, ());
    let hint = types.get_surface_content_type(&surface, &qh, ());
    hint.set_content_type(wp_content_type_v1::Type::Video);
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert_eq!(surface_content_type(&mapped), Type::None, "double-buffered");
    surface.commit();
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert_eq!(surface_content_type(&mapped), Type::Video);
}
