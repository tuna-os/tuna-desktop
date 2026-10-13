//! Touchscreens (#345): the seat advertises touch, and contacts mapped
//! onto an output reach the `wl_touch` client under them with framed
//! down/motion/up sequences, while pointer emulation still clicks.
//!
//! A real client asserts delivery; output selection, contact placement
//! and the orientation lock state machine carry their own unit tests in
//! `touchscreen.rs`. Conventions follow `pointer_events.rs`.

use std::os::unix::net::UnixStream;

use smithay::utils::{Logical, Point};
use tuna_compositor::touchscreen::{map_onto, select_output, OrientationState, TouchscreenConfig};
use tuna_compositor::windows::{ManagerInput, TouchPhase, WindowManager};
use tuna_compositor::TestCompositor;
use wayland_client::{
    protocol::{
        wl_callback::WlCallback,
        wl_compositor::WlCompositor,
        wl_display::WlDisplay,
        wl_registry::WlRegistry,
        wl_seat::{self, WlSeat},
        wl_surface::WlSurface,
        wl_touch::{Event as TouchEvent, WlTouch},
    },
    Connection, Dispatch, EventQueue, Proxy, QueueHandle,
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::XdgSurface, xdg_toplevel::XdgToplevel, xdg_wm_base::XdgWmBase,
};

const PUMP_ROUNDS: usize = 200;

#[derive(Default)]
struct Client {
    compositor: Option<WlCompositor>,
    seat: Option<WlSeat>,
    xdg_base: Option<XdgWmBase>,
    touch: Option<WlTouch>,
    synced: bool,
    /// Whether the seat advertised the touch capability.
    touch_capability: bool,
    /// Touch points that went down (id, surface-local x, y).
    downs: Vec<(i32, f64, f64)>,
    /// Touch motion (id, surface-local x, y).
    motions: Vec<(i32, f64, f64)>,
    /// Touch points that lifted.
    ups: Vec<i32>,
    cancels: u32,
    /// wl_touch.frame events received.
    frames: u32,
}

impl Client {
    fn ready(&self) -> bool {
        self.compositor.is_some() && self.xdg_base.is_some() && self.touch.is_some()
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
        if let wayland_client::protocol::wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_compositor" => {
                    state.compositor = Some(registry.bind(name, version.min(6), qh, ()));
                }
                "wl_seat" => {
                    state.seat = Some(registry.bind(name, version, qh, ()));
                }
                "xdg_wm_base" => {
                    state.xdg_base = Some(registry.bind(name, version.min(7), qh, ()));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<WlSeat, ()> for Client {
    fn event(
        state: &mut Self,
        seat: &WlSeat,
        event: <WlSeat as Proxy>::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: wayland_client::WEnum::Value(caps),
        } = event
        {
            if caps.contains(wl_seat::Capability::Touch) {
                state.touch_capability = true;
                if state.touch.is_none() {
                    state.touch = Some(seat.get_touch(qh, ()));
                }
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
        if let wayland_protocols::xdg::shell::client::xdg_wm_base::Event::Ping { serial } = event {
            base.pong(serial);
        }
    }
}

/// Each xdg_surface carries its wl_surface so the configure ack can
/// commit the right one.
impl Dispatch<XdgSurface, WlSurface> for Client {
    fn event(
        _: &mut Self,
        xdg_surface: &XdgSurface,
        event: <XdgSurface as Proxy>::Event,
        surface: &WlSurface,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wayland_protocols::xdg::shell::client::xdg_surface::Event::Configure { serial } =
            event
        {
            xdg_surface.ack_configure(serial);
            surface.commit();
        }
    }
}

impl Dispatch<WlTouch, ()> for Client {
    fn event(
        state: &mut Self,
        _: &WlTouch,
        event: <WlTouch as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            TouchEvent::Down { id, x, y, .. } => state.downs.push((id, x, y)),
            TouchEvent::Motion { id, x, y, .. } => state.motions.push((id, x, y)),
            TouchEvent::Up { id, .. } => state.ups.push(id),
            TouchEvent::Frame => state.frames += 1,
            TouchEvent::Cancel => state.cancels += 1,
            _ => {}
        }
    }
}

macro_rules! empty_dispatch {
    ($iface:ty) => {
        impl Dispatch<$iface, ()> for Client {
            fn event(
                _: &mut Self,
                _: &$iface,
                _: <$iface as Proxy>::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
            }
        }
    };
}
empty_dispatch!(WlDisplay);
empty_dispatch!(WlCompositor);
empty_dispatch!(WlSurface);
empty_dispatch!(XdgToplevel);

struct Fixture {
    comp: TestCompositor,
    manager: WindowManager,
    conn: Connection,
    queue: EventQueue<Client>,
    client: Client,
    _toplevel_surface: WlSurface,
    _toplevel_xdg: XdgSurface,
    window_origin: Point<i32, Logical>,
}

fn pump(f: &mut Fixture, done: impl Fn(&Client) -> bool) {
    for _ in 0..PUMP_ROUNDS {
        f.queue.flush().unwrap();
        f.comp.pump();
        f.manager.reconcile(&mut f.comp.state);
        f.comp.pump();
        if let Some(guard) = f.queue.prepare_read() {
            guard.read().unwrap();
        }
        f.queue.dispatch_pending(&mut f.client).unwrap();
        if done(&f.client) {
            return;
        }
    }
    panic!("pump budget exhausted");
}

fn sync(f: &mut Fixture) {
    let qh = f.queue.handle();
    f.client.synced = false;
    f.conn.display().sync(&qh, ());
    pump(f, |c| c.synced);
}

fn fixture() -> Fixture {
    let mut comp = TestCompositor::new();
    // Manager first so seat capabilities exist before any client binds.
    let manager = comp.window_manager();
    let (server, client_stream) = UnixStream::pair().unwrap();
    comp.add_client(server);
    let conn = Connection::from_socket(client_stream).unwrap();
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    let mut client = Client::default();
    conn.display().get_registry(&qh, ());
    for _ in 0..PUMP_ROUNDS {
        queue.flush().unwrap();
        comp.pump();
        if let Some(guard) = queue.prepare_read() {
            guard.read().unwrap();
        }
        queue.dispatch_pending(&mut client).unwrap();
        if client.ready() {
            break;
        }
    }
    assert!(client.ready(), "seat offers wl_touch");
    let surface = client.compositor.as_ref().unwrap().create_surface(&qh, ());
    let xdg = client
        .xdg_base
        .as_ref()
        .unwrap()
        .get_xdg_surface(&surface, &qh, surface.clone());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title("touch-host".into());
    surface.commit();
    let mut f = Fixture {
        comp,
        manager,
        conn,
        queue,
        client,
        _toplevel_surface: surface,
        _toplevel_xdg: xdg,
        window_origin: (0, 0).into(),
    };
    sync(&mut f);
    sync(&mut f);
    let (_, geometry) = f.manager.visible_windows()[0].clone();
    f.window_origin = geometry.loc;
    f
}

fn inside(f: &Fixture) -> Point<f64, Logical> {
    (f.window_origin + Point::from((20, 20))).to_f64()
}

fn touch(slot: i32, phase: TouchPhase, pos: Point<f64, Logical>, time: u32) -> ManagerInput {
    ManagerInput::Touch {
        slot,
        phase,
        pos,
        time,
    }
}

#[test]
fn seat_advertises_touch_and_contacts_reach_the_window() {
    let mut f = fixture();
    assert!(f.client.touch_capability);
    let at = inside(&f);
    f.manager
        .on_input(&mut f.comp.state, touch(0, TouchPhase::Down, at, 1));
    f.manager.on_input(
        &mut f.comp.state,
        touch(0, TouchPhase::Motion, at + Point::from((5.0, 0.0)), 2),
    );
    f.manager
        .on_input(&mut f.comp.state, touch(0, TouchPhase::Up, at, 3));
    sync(&mut f);
    assert_eq!(f.client.downs.len(), 1, "one contact went down");
    assert_eq!(f.client.downs[0].0, 0, "slot survives the round trip");
    // Surface-local: 20px in, minus the client-side shadow offset the
    // focus origin carries; near the contact either way.
    assert!(
        (f.client.downs[0].1 - 20.0).abs() < 30.0,
        "down x near the contact: {:?}",
        f.client.downs[0]
    );
    assert_eq!(f.client.motions.len(), 1, "motion follows down");
    assert_eq!(f.client.ups, vec![0], "the contact lifted");
    assert!(f.client.frames >= 2, "down and motion each framed");
    assert_eq!(f.client.cancels, 0);
}

#[test]
fn cancelled_contacts_lift_so_no_touch_point_sticks() {
    let mut f = fixture();
    let at = inside(&f);
    f.manager
        .on_input(&mut f.comp.state, touch(1, TouchPhase::Down, at, 1));
    f.manager
        .on_input(&mut f.comp.state, touch(1, TouchPhase::Cancel, at, 2));
    sync(&mut f);
    assert_eq!(f.client.downs.len(), 1);
    // Smithay 0.7 cannot emit wl_touch.cancel after a frame, so the
    // voided contact lifts: balanced either way, never stranded.
    assert_eq!(f.client.ups, vec![1]);
}

#[test]
fn mapped_contacts_follow_the_configured_output() {
    use smithay::utils::Rectangle;
    let outputs = vec![
        (
            "eDP-1".to_owned(),
            Rectangle::new((0, 0).into(), (1512, 944).into()),
            true,
        ),
        (
            "HDMI-A-1".to_owned(),
            Rectangle::new((1512, 0).into(), (1920, 1080).into()),
            false,
        ),
    ];
    // Automatic mapping: the primary.
    let target = select_output(&outputs, &TouchscreenConfig::default()).unwrap();
    assert_eq!(target.loc.x, 0);
    let at: Point<f64, Logical> = map_onto(target, 756.0, 472.0);
    assert_eq!((at.x, at.y), (756.0, 472.0));
    // Explicit mapping: the named output's rectangle.
    let config = TouchscreenConfig {
        output: ["HDMI-A-1".to_owned(), String::new(), String::new()],
        orientation_lock: false,
    };
    let target = select_output(&outputs, &config).unwrap();
    let at: Point<f64, Logical> = map_onto(target, 960.0, 540.0);
    assert_eq!((at.x, at.y), (2472.0, 540.0));
}

#[test]
fn orientation_lock_freezes_sensor_updates_until_unlock() {
    use tuna_compositor::touchscreen::Orientation;
    let mut state = OrientationState::default();
    state.on_sensor(Orientation::LeftUp);
    assert_eq!(state.applied(), Orientation::LeftUp);
    state.set_locked(true);
    state.on_sensor(Orientation::BottomUp);
    assert_eq!(state.applied(), Orientation::LeftUp, "locked: frozen");
    state.set_locked(false);
    assert_eq!(state.applied(), Orientation::BottomUp, "unlocked: applied");
}
