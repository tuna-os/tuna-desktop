//! Advertised protocols (#89) and the xdg-activation focus policy.
//!
//! The globals list is golden: adding or dropping a protocol is a
//! deliberate change that updates `EXPECTED` and `docs/protocols.md`
//! together. xdg-decoration stays absent on purpose (Mutter parity).

use std::collections::BTreeMap;
use std::os::unix::net::UnixStream;

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

const ROUNDS: usize = 200;

/// Every global a client sees on a headless compositor, by version.
/// linux-dmabuf joins once a renderer reports formats (see below), and
/// wl_output once an output is connected.
const EXPECTED: &[(&str, u32)] = &[
    ("ext_background_effect_manager_v1", 1),
    ("ext_session_lock_manager_v1", 1),
    ("wl_compositor", 6),
    ("wl_data_device_manager", 3),
    ("wl_fixes", 1),
    ("wl_seat", 9),
    ("wl_shm", 2),
    ("wl_subcompositor", 1),
    ("wp_commit_timing_manager_v1", 1),
    ("wp_cursor_shape_manager_v1", 2),
    ("wp_fifo_manager_v1", 1),
    ("wp_fractional_scale_manager_v1", 1),
    ("wp_pointer_warp_v1", 1),
    ("wp_presentation", 2),
    ("wp_single_pixel_buffer_manager_v1", 1),
    ("wp_viewporter", 1),
    ("xdg_activation_v1", 1),
    ("xdg_session_manager_v1", 1),
    ("xdg_system_bell_v1", 1),
    ("xdg_toplevel_icon_manager_v1", 1),
    ("xdg_toplevel_tag_manager_v1", 1),
    ("xdg_wm_base", 7),
    ("xdg_wm_dialog_v1", 1),
    ("zwlr_layer_shell_v1", 4),
    ("zwp_idle_inhibit_manager_v1", 1),
    ("zwp_input_method_manager_v2", 1),
    ("zwp_keyboard_shortcuts_inhibit_manager_v1", 1),
    ("zwp_pointer_constraints_v1", 1),
    ("zwp_pointer_gestures_v1", 3),
    ("zwp_primary_selection_device_manager_v1", 1),
    ("zwp_relative_pointer_manager_v1", 1),
    ("zwp_tablet_manager_v2", 2),
    ("zwp_text_input_manager_v3", 2),
    ("zxdg_exporter_v1", 1),
    ("zxdg_exporter_v2", 1),
    ("zxdg_importer_v1", 1),
    ("zxdg_importer_v2", 1),
    ("zxdg_output_manager_v1", 3),
];

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

struct Peer {
    _conn: Connection,
    queue: EventQueue<Client>,
    client: Client,
}

fn connect(comp: &mut TestCompositor) -> Peer {
    let (server, stream) = UnixStream::pair().unwrap();
    comp.add_client(server);
    let conn = Connection::from_socket(stream).unwrap();
    let queue = conn.new_event_queue();
    conn.display().get_registry(&queue.handle(), ());
    Peer {
        _conn: conn,
        queue,
        client: Client::default(),
    }
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
