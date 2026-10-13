//! Graphics tablets (#344): the tablet manager is advertised, a
//! tablet and tool reach clients with their identity, and tool samples
//! drive the cursor plus tool protocol events (mapped position, curved
//! pressure, tip and barrel buttons).

use std::os::unix::net::UnixStream;
use std::sync::Arc;

use smithay::{
    backend::input::{TabletToolCapabilities, TabletToolType},
    utils::{Logical, Point},
    wayland::tablet_manager::TabletDescriptor,
};
use tuna_compositor::{
    tablet::{TabletDeviceChange, TabletToolId, TabletToolPhase},
    windows::{ManagerInput, WindowManager},
    TestCompositor,
};
use wayland_client::{
    backend::ObjectData,
    protocol::{
        wl_compositor::WlCompositor,
        wl_registry::{self, WlRegistry},
        wl_seat::WlSeat,
        wl_surface::WlSurface,
    },
    Connection, Dispatch, EventQueue, QueueHandle, WEnum,
};
use wayland_protocols::wp::tablet::zv2::client::{
    zwp_tablet_manager_v2::ZwpTabletManagerV2,
    zwp_tablet_pad_v2::ZwpTabletPadV2,
    zwp_tablet_seat_v2::{self, ZwpTabletSeatV2},
    zwp_tablet_tool_v2::{self, ZwpTabletToolV2},
    zwp_tablet_v2::{self, ZwpTabletV2},
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

const ROUNDS: usize = 20;

#[derive(Default)]
struct TabletClient {
    compositor: Option<WlCompositor>,
    seat: Option<WlSeat>,
    wm: Option<XdgWmBase>,
    tablet_manager: Option<ZwpTabletManagerV2>,
    /// Kept alive for the test (never read back).
    #[allow(dead_code)]
    tablet_seat: Option<ZwpTabletSeatV2>,
    tablet_names: Vec<String>,
    tools: u32,
    tools_done: u32,
    proximity_in: bool,
    proximity_out: bool,
    motions: Vec<(f64, f64)>,
    pressures: Vec<u32>,
    downs: u32,
    ups: u32,
    buttons: Vec<(u32, u32)>,
}

impl Dispatch<WlRegistry, ()> for TabletClient {
    fn event(
        state: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name, interface, ..
        } = event
        {
            match interface.as_str() {
                "wl_compositor" => state.compositor = Some(registry.bind(name, 4, qh, ())),
                "wl_seat" => state.seat = Some(registry.bind(name, 7, qh, ())),
                "xdg_wm_base" => state.wm = Some(registry.bind(name, 1, qh, ())),
                "zwp_tablet_manager_v2" => {
                    state.tablet_manager = Some(registry.bind(name, 1, qh, ()))
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<XdgWmBase, ()> for TabletClient {
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

impl Dispatch<XdgSurface, ()> for TabletClient {
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

impl Dispatch<ZwpTabletSeatV2, ()> for TabletClient {
    fn event_created_child(opcode: u16, qh: &QueueHandle<Self>) -> Arc<dyn ObjectData> {
        match opcode {
            0 => qh.make_data::<ZwpTabletV2, _>(()),
            1 => qh.make_data::<ZwpTabletToolV2, _>(()),
            2 => qh.make_data::<ZwpTabletPadV2, _>(()),
            _ => panic!("unknown tablet seat child opcode {opcode}"),
        }
    }

    fn event(
        state: &mut Self,
        _: &ZwpTabletSeatV2,
        event: zwp_tablet_seat_v2::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_tablet_seat_v2::Event::TabletAdded { .. } => {}
            zwp_tablet_seat_v2::Event::ToolAdded { .. } => state.tools += 1,
            _ => {}
        }
    }
}

impl Dispatch<ZwpTabletV2, ()> for TabletClient {
    fn event(
        state: &mut Self,
        _: &ZwpTabletV2,
        event: zwp_tablet_v2::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_tablet_v2::Event::Name { name } => state.tablet_names.push(name),
            // Smithay 0.7 never sends `removed` for tablets (unlike
            // tools): unplugging only drops the seat entry.
            _ => {}
        }
    }
}

impl Dispatch<ZwpTabletToolV2, ()> for TabletClient {
    fn event(
        state: &mut Self,
        _: &ZwpTabletToolV2,
        event: zwp_tablet_tool_v2::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_tablet_tool_v2::Event::Done => state.tools_done += 1,
            zwp_tablet_tool_v2::Event::ProximityIn { .. } => state.proximity_in = true,
            zwp_tablet_tool_v2::Event::ProximityOut => state.proximity_out = true,
            zwp_tablet_tool_v2::Event::Motion { x, y } => state.motions.push((x, y)),
            zwp_tablet_tool_v2::Event::Pressure { pressure } => state.pressures.push(pressure),
            zwp_tablet_tool_v2::Event::Down { .. } => state.downs += 1,
            zwp_tablet_tool_v2::Event::Up => state.ups += 1,
            zwp_tablet_tool_v2::Event::Button {
                button,
                state: pressed,
                ..
            } => {
                let pressed = matches!(
                    pressed,
                    WEnum::Value(zwp_tablet_tool_v2::ButtonState::Pressed)
                );
                state.buttons.push((button, u32::from(pressed)));
            }
            _ => {}
        }
    }
}

wayland_client::delegate_noop!(TabletClient: ignore WlCompositor);
wayland_client::delegate_noop!(TabletClient: ignore WlSurface);
wayland_client::delegate_noop!(TabletClient: ignore WlSeat);
wayland_client::delegate_noop!(TabletClient: ignore XdgToplevel);
wayland_client::delegate_noop!(TabletClient: ignore ZwpTabletManagerV2);
wayland_client::delegate_noop!(TabletClient: ignore ZwpTabletPadV2);

struct Peer {
    _conn: Connection,
    queue: EventQueue<TabletClient>,
    client: TabletClient,
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
        client: TabletClient::default(),
    }
}

fn pump(comp: &mut TestCompositor, manager: &mut WindowManager, peers: &mut [&mut Peer]) {
    for _ in 0..ROUNDS {
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

fn window(peer: &mut Peer) -> WlSurface {
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
    toplevel.set_title("canvas".into());
    surface.commit();
    std::mem::forget((xdg, toplevel));
    surface
}

fn tool() -> TabletToolId {
    TabletToolId {
        tool_type: TabletToolType::Pen,
        hardware_serial: 1,
        hardware_id_wacom: 0x802,
        capabilities: (TabletToolCapabilities::PRESSURE | TabletToolCapabilities::TILT).bits(),
    }
}

fn sample(
    phase: TabletToolPhase,
    pos: Point<f64, Logical>,
    pressure: f64,
    time: u32,
) -> ManagerInput {
    ManagerInput::TabletTool {
        tablet: 7,
        tool: tool(),
        phase,
        pos,
        area: (1280, 800).into(),
        delta: (0.0, 0.0).into(),
        pressure,
        tilt: (0.0, 0.0),
        time,
    }
}

#[test]
fn tablet_manager_advertises_and_tools_reach_clients() {
    let mut comp = TestCompositor::new();
    comp.state.set_output_size(1280, 800);
    let mut manager = comp.window_manager();
    let mut peer = connect(&mut comp);
    pump(&mut comp, &mut manager, &mut [&mut peer]);

    // Bind the tablet seat, as a tablet-aware client does at startup.
    let qh = peer.queue.handle();
    peer.client.tablet_seat = Some(
        peer.client
            .tablet_manager
            .as_ref()
            .expect("zwp_tablet_manager_v2 advertised")
            .get_tablet_seat(peer.client.seat.as_ref().unwrap(), &qh, ()),
    );
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    let _surface = window(&mut peer);
    pump(&mut comp, &mut manager, &mut [&mut peer]);

    // A tablet arrives (the hardware backend's registration, headless).
    manager.apply_tablet_device(
        &mut comp.state,
        TabletDeviceChange::Added {
            id: 7,
            tablet: TabletDescriptor {
                name: "Test Tablet".into(),
                usb_id: Some((0x0374, 0x056a)),
                syspath: None,
            },
            size_mm: (152.0, 95.0),
        },
    );
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert_eq!(peer.client.tablet_names, ["Test Tablet"]);

    // The stylus hovers over the window: proximity plus cursor motion.
    let (_, geometry) = manager.visible_windows()[0].clone();
    let inside: Point<f64, Logical> = (geometry.loc + Point::from((50, 50))).to_f64();
    manager.on_input(
        &mut comp.state,
        sample(TabletToolPhase::ProximityIn, inside, 0.0, 1),
    );
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert_eq!(peer.client.tools, 1, "the tool was advertised");
    assert_eq!(peer.client.tools_done, 1, "the tool was fully described");
    assert!(peer.client.proximity_in, "proximity entered the window");
    let cursor = manager.pointer_pos();
    assert!(
        (cursor.x - inside.x).abs() < 1e-6 && (cursor.y - inside.y).abs() < 1e-6,
        "the cursor follows the stylus: {cursor:?} vs {inside:?}"
    );

    // Motion carries the mapped position and the curved pressure
    // (linear default: half pressure is half of u16::MAX, rounded).
    manager.on_input(
        &mut comp.state,
        sample(TabletToolPhase::Axis, inside, 0.5, 2),
    );
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert!(!peer.client.motions.is_empty(), "motion reached the client");
    assert!(
        peer.client.pressures.contains(&32768),
        "curved pressure reached the client: {:?}",
        peer.client.pressures
    );

    // The tip clicks; a barrel button reports and context-clicks.
    manager.on_input(
        &mut comp.state,
        sample(TabletToolPhase::TipDown, inside, 0.9, 3),
    );
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert_eq!(peer.client.downs, 1);
    manager.on_input(
        &mut comp.state,
        sample(TabletToolPhase::TipUp, inside, 0.0, 4),
    );
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert_eq!(peer.client.ups, 1);
    manager.on_input(
        &mut comp.state,
        sample(
            TabletToolPhase::Button {
                button: 0x14b,
                pressed: true,
            },
            inside,
            0.0,
            5,
        ),
    );
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert!(
        peer.client.buttons.contains(&(0x14b, 1)),
        "barrel press reached the client: {:?}",
        peer.client.buttons
    );
    manager.on_input(
        &mut comp.state,
        sample(
            TabletToolPhase::Button {
                button: 0x14b,
                pressed: false,
            },
            inside,
            0.0,
            6,
        ),
    );
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert!(
        peer.client.buttons.contains(&(0x14b, 0)),
        "barrel release reached the client: {:?}",
        peer.client.buttons
    );

    // Leaving proximity and unplugging close the session cleanly.
    manager.on_input(
        &mut comp.state,
        sample(TabletToolPhase::ProximityOut, inside, 0.0, 7),
    );
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert!(peer.client.proximity_out);
    manager.apply_tablet_device(&mut comp.state, TabletDeviceChange::Removed { id: 7 });
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    // After unplugging, late samples degrade to cursor-only motion:
    // no tool events, but the cursor still follows.
    let motions_before = peer.client.motions.len();
    manager.on_input(
        &mut comp.state,
        sample(TabletToolPhase::Axis, inside, 0.0, 8),
    );
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert_eq!(
        peer.client.motions.len(),
        motions_before,
        "no tool events after removal"
    );
    let cursor = manager.pointer_pos();
    assert!(
        (cursor.x - inside.x).abs() < 1e-6 && (cursor.y - inside.y).abs() < 1e-6,
        "the cursor still follows after removal: {cursor:?} vs {inside:?}"
    );
}

#[test]
fn relative_mapping_steps_from_the_cursor() {
    use tuna_compositor::tablet::UNKNOWN_TABLET;
    use tuna_shell_control::InputSettings;

    let mut comp = TestCompositor::new();
    comp.state.set_output_size(1280, 800);
    let mut manager = comp.window_manager();
    manager.apply_tablet_settings(&InputSettings {
        tablet_mapping_relative: true,
        ..InputSettings::default()
    });

    // Unattributed samples still move the cursor; only tool protocol
    // events wait for a registered tablet.
    manager.on_input(
        &mut comp.state,
        ManagerInput::TabletTool {
            tablet: UNKNOWN_TABLET,
            tool: tool(),
            phase: TabletToolPhase::Axis,
            pos: (0.0, 0.0).into(),
            area: (1280, 800).into(),
            delta: (10.0, 5.0).into(),
            pressure: 0.0,
            tilt: (0.0, 0.0),
            time: 1,
        },
    );
    assert_eq!(manager.pointer_pos(), Point::from((10.0, 5.0)));
}
