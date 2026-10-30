//! Touchscreen delivery as Wayland clients see it (P-DS-04, #345).
//!
//! A real client binds `wl_touch` once the seat advertises touch, as GTK
//! does, and asserts what Mutter (`meta-wayland-touch.c`) sends: each
//! sequence goes to the surface under the point where it began and stays
//! there (an implicit grab per sequence), every hardware frame ends with
//! `wl_touch.frame`, a compositor claim sends `wl_touch.cancel` and
//! silences the claimed fingers, the lock surface alone hears touch while
//! locked, and any finger's down serial may start a drag-and-drop
//! (`meta_wayland_seat_get_grab_info` looks through every sequence).
//! Conventions follow `pointer_events.rs`.

use std::os::unix::net::UnixStream;

use smithay::utils::{Logical, Point};
use smithay::wayland::seat::WaylandFocus;
use tuna_compositor::windows::{ManagerInput, WindowManager};
use tuna_compositor::TestCompositor;
use wayland_client::{
    protocol::{
        wl_callback::WlCallback,
        wl_compositor::WlCompositor,
        wl_data_device::{Event as DeviceEvent, WlDataDevice},
        wl_data_device_manager::WlDataDeviceManager,
        wl_data_offer::WlDataOffer,
        wl_data_source::WlDataSource,
        wl_display::WlDisplay,
        wl_pointer::{Event as PointerEvent, WlPointer},
        wl_registry::WlRegistry,
        wl_seat::{Capability, Event as SeatEvent, WlSeat},
        wl_surface::WlSurface,
        wl_touch::{Event as TouchEvent, WlTouch},
    },
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum,
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::XdgSurface, xdg_toplevel::XdgToplevel, xdg_wm_base::XdgWmBase,
};

const PUMP_ROUNDS: usize = 200;

/// One `wl_touch` event as the client saw it: `(kind, surface, id)`;
/// motion and up carry the surface their sequence's down named.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Seen {
    Down { surface: u32, id: i32, serial: u32 },
    Motion { surface: u32, id: i32 },
    Up { surface: u32, id: i32 },
    Frame,
    Cancel,
}

#[derive(Default)]
struct Client {
    compositor: Option<WlCompositor>,
    seat: Option<WlSeat>,
    xdg_base: Option<XdgWmBase>,
    data_manager: Option<WlDataDeviceManager>,
    data_device: Option<WlDataDevice>,
    touch: Option<WlTouch>,
    synced: bool,
    /// Which surface each live touch id went down on, as a toolkit keeps.
    sequences: std::collections::HashMap<i32, u32>,
    events: Vec<Seen>,
    pointer_buttons: u32,
    drag_enters: Vec<u32>,
}

impl Client {
    fn ready(&self) -> bool {
        self.compositor.is_some()
            && self.xdg_base.is_some()
            && self.seat.is_some()
            && self.data_device.is_some()
    }

    fn downs(&self) -> Vec<(u32, i32, u32)> {
        self.events
            .iter()
            .filter_map(|event| match event {
                Seen::Down {
                    surface,
                    id,
                    serial,
                } => Some((*surface, *id, *serial)),
                _ => None,
            })
            .collect()
    }

    fn count(&self, seen: &Seen) -> usize {
        self.events.iter().filter(|event| *event == seen).count()
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
                    let seat: WlSeat = registry.bind(name, version.min(9), qh, ());
                    seat.get_pointer(qh, ());
                    state.seat = Some(seat);
                }
                "xdg_wm_base" => {
                    state.xdg_base = Some(registry.bind(name, version.min(7), qh, ()));
                }
                "wl_data_device_manager" => {
                    state.data_manager = Some(registry.bind(name, version.min(3), qh, ()));
                }
                _ => {}
            }
            if state.data_device.is_none() {
                if let (Some(manager), Some(seat)) = (&state.data_manager, &state.seat) {
                    state.data_device = Some(manager.get_data_device(seat, qh, ()));
                }
            }
        }
    }
}

/// GTK binds `wl_touch` when the seat gains the touch capability.
impl Dispatch<WlSeat, ()> for Client {
    fn event(
        state: &mut Self,
        seat: &WlSeat,
        event: SeatEvent,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let SeatEvent::Capabilities {
            capabilities: WEnum::Value(capabilities),
        } = event
        {
            let touch = capabilities.contains(Capability::Touch);
            if touch && state.touch.is_none() {
                state.touch = Some(seat.get_touch(qh, ()));
            } else if !touch {
                if let Some(old) = state.touch.take() {
                    old.release();
                }
            }
        }
    }
}

impl Dispatch<WlTouch, ()> for Client {
    fn event(
        state: &mut Self,
        _: &WlTouch,
        event: TouchEvent,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let seen = match event {
            TouchEvent::Down {
                surface,
                id,
                serial,
                ..
            } => {
                let surface = surface.id().protocol_id();
                state.sequences.insert(id, surface);
                Seen::Down {
                    surface,
                    id,
                    serial,
                }
            }
            TouchEvent::Motion { id, .. } => Seen::Motion {
                surface: state.sequences.get(&id).copied().unwrap_or(0),
                id,
            },
            TouchEvent::Up { id, .. } => Seen::Up {
                surface: state.sequences.remove(&id).unwrap_or(0),
                id,
            },
            TouchEvent::Frame => Seen::Frame,
            TouchEvent::Cancel => {
                state.sequences.clear();
                Seen::Cancel
            }
            _ => return,
        };
        state.events.push(seen);
    }
}

impl Dispatch<WlPointer, ()> for Client {
    fn event(
        state: &mut Self,
        _: &WlPointer,
        event: PointerEvent,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let PointerEvent::Button { .. } = event {
            state.pointer_buttons += 1;
        }
    }
}

impl Dispatch<WlDataDevice, ()> for Client {
    fn event(
        state: &mut Self,
        _: &WlDataDevice,
        event: DeviceEvent,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let DeviceEvent::Enter { surface, .. } = event {
            state.drag_enters.push(surface.id().protocol_id());
        }
    }

    wayland_client::event_created_child!(Client, WlDataDevice, [
        wayland_client::protocol::wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, ()),
    ]);
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
empty_dispatch!(WlDataDeviceManager);
empty_dispatch!(WlDataSource);
empty_dispatch!(WlDataOffer);

struct Fixture {
    comp: TestCompositor,
    manager: WindowManager,
    conn: Connection,
    queue: EventQueue<Client>,
    client: Client,
    /// Client-side surfaces of alpha and beta, and their protocol ids.
    surfaces: [WlSurface; 2],
    _xdg: Vec<XdgSurface>,
    ids: [u64; 2],
    time: u32,
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

/// Alpha at (100, 100) and beta at (600, 100), both 400x300, apart.
fn fixture() -> Fixture {
    let mut comp = TestCompositor::new();
    let mut manager = comp.window_manager();
    manager.set_touch_capability(&mut comp.state, true);
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
        if client.ready() && client.touch.is_some() {
            break;
        }
    }
    assert!(client.ready());
    assert!(client.touch.is_some(), "touch capability binds wl_touch");
    let mut surfaces = Vec::new();
    let mut xdgs = Vec::new();
    for title in ["alpha", "beta"] {
        let surface = client.compositor.as_ref().unwrap().create_surface(&qh, ());
        let xdg = client
            .xdg_base
            .as_ref()
            .unwrap()
            .get_xdg_surface(&surface, &qh, surface.clone());
        let toplevel = xdg.get_toplevel(&qh, ());
        toplevel.set_title(title.into());
        surface.commit();
        surfaces.push(surface);
        xdgs.push(xdg);
    }
    let mut f = Fixture {
        comp,
        manager,
        conn,
        queue,
        client,
        surfaces: [surfaces[0].clone(), surfaces[1].clone()],
        _xdg: xdgs,
        ids: [0, 0],
        time: 1,
    };
    sync(&mut f);
    sync(&mut f);
    for (index, title) in ["alpha", "beta"].into_iter().enumerate() {
        let id = f
            .manager
            .model()
            .windows()
            .find(|w| w.title == title)
            .unwrap()
            .id;
        f.ids[index] = id;
        let target = (100 + 500 * index as i32, 100);
        assert!(f.manager.resize_window(id, 400, 300));
        let at = f.manager.geometry(id).unwrap().loc;
        assert!(f.manager.move_window(id, target.0 - at.x, target.1 - at.y));
    }
    sync(&mut f);
    f
}

fn alpha() -> Point<f64, Logical> {
    (200.0, 200.0).into()
}

fn beta() -> Point<f64, Logical> {
    (700.0, 200.0).into()
}

impl Fixture {
    fn surface_id(&self, index: usize) -> u32 {
        self.surfaces[index].id().protocol_id()
    }

    fn tick(&mut self) -> u32 {
        self.time += 1;
        self.time
    }

    fn input(&mut self, input: ManagerInput) {
        self.manager.on_input(&mut self.comp.state, input);
    }

    fn down(&mut self, slot: u32, pos: Point<f64, Logical>) {
        let time = self.tick();
        self.input(ManagerInput::TouchDown { slot, pos, time });
    }

    fn motion(&mut self, slot: u32, pos: Point<f64, Logical>) {
        let time = self.tick();
        self.input(ManagerInput::TouchMotion { slot, pos, time });
    }

    fn up(&mut self, slot: u32) {
        let time = self.tick();
        self.input(ManagerInput::TouchUp { slot, time });
    }

    fn frame(&mut self) {
        let time = self.tick();
        self.input(ManagerInput::TouchFrame { time });
    }

    /// The compositor's surface for alpha (0) or beta (1).
    fn server_surface(
        &self,
        index: usize,
    ) -> smithay::reexports::wayland_server::protocol::wl_surface::WlSurface {
        let want = self.surface_id(index);
        self.manager
            .visible_windows()
            .into_iter()
            .find_map(|(window, _)| {
                let surface = window.wl_surface()?.into_owned();
                let id = smithay::reexports::wayland_server::Resource::id(&surface);
                (id.protocol_id() == want).then_some(surface)
            })
            .expect("the window's server surface")
    }
}

#[test]
fn each_sequence_stays_on_the_surface_it_began_on_and_is_framed() {
    let mut f = fixture();
    let (a, b) = (f.surface_id(0), f.surface_id(1));
    // Two fingers in one hardware frame, one on each window.
    f.down(0, alpha());
    f.down(1, beta());
    f.frame();
    sync(&mut f);
    let downs = f.client.downs();
    assert_eq!(downs.len(), 2, "one down per sequence");
    assert_eq!((downs[0].0, downs[0].1), (a, 0));
    assert_eq!((downs[1].0, downs[1].1), (b, 1));
    assert_ne!(downs[0].2, downs[1].2, "each down has its own serial");
    assert_eq!(f.client.events.last(), Some(&Seen::Frame));
    // The first finger slides over beta: it stays alpha's.
    f.motion(0, beta());
    f.motion(1, (720.0, 220.0).into());
    f.frame();
    f.up(0);
    f.frame();
    f.up(1);
    f.frame();
    sync(&mut f);
    assert_eq!(f.client.count(&Seen::Motion { surface: a, id: 0 }), 1);
    assert_eq!(f.client.count(&Seen::Motion { surface: b, id: 1 }), 1);
    assert_eq!(f.client.count(&Seen::Up { surface: a, id: 0 }), 1);
    assert_eq!(f.client.count(&Seen::Up { surface: b, id: 1 }), 1);
    assert_eq!(
        f.client.count(&Seen::Frame),
        4,
        "one frame per hardware frame"
    );
    assert_eq!(f.client.count(&Seen::Cancel), 0);
    assert_eq!(
        f.client.pointer_buttons, 0,
        "touch is not pointer emulation"
    );
    // A touch begin focuses like a primary press.
    assert_eq!(f.manager.model().focused(), Some(f.ids[1]));
}

#[test]
fn a_compositor_claim_cancels_and_silences_the_fingers() {
    let mut f = fixture();
    f.down(0, alpha());
    f.down(1, beta());
    f.frame();
    sync(&mut f);
    // A shell gesture claims the fingers (`meta_wayland_touch_cancel`).
    f.manager.cancel_client_touches(&mut f.comp.state);
    sync(&mut f);
    assert_eq!(f.client.count(&Seen::Cancel), 1);
    let before = f.client.events.len();
    f.motion(0, (210.0, 210.0).into());
    f.motion(1, (710.0, 210.0).into());
    f.up(0);
    f.up(1);
    sync(&mut f);
    assert!(
        !f.client.events[before..]
            .iter()
            .any(|event| matches!(event, Seen::Motion { .. } | Seen::Up { .. })),
        "claimed sequences reach no client: {:?}",
        &f.client.events[before..]
    );
    // A new finger after the claim is delivered again.
    f.down(2, alpha());
    f.frame();
    sync(&mut f);
    assert_eq!(f.client.downs().last().map(|d| d.1), Some(2));
}

#[test]
fn touch_while_locked_reaches_only_the_lock_surface() {
    let mut f = fixture();
    // Beta's surface stands in for the lock surface: a touch over alpha
    // still goes to it, as the lock covers the session.
    let lock = f.server_surface(1);
    let time = f.tick();
    f.manager.lock_input(
        &mut f.comp.state,
        &lock,
        ManagerInput::TouchDown {
            slot: 0,
            pos: alpha(),
            time,
        },
    );
    let time = f.tick();
    f.manager
        .lock_input(&mut f.comp.state, &lock, ManagerInput::TouchFrame { time });
    let time = f.tick();
    f.manager.lock_input(
        &mut f.comp.state,
        &lock,
        ManagerInput::TouchUp { slot: 0, time },
    );
    let time = f.tick();
    f.manager
        .lock_input(&mut f.comp.state, &lock, ManagerInput::TouchFrame { time });
    sync(&mut f);
    let b = f.surface_id(1);
    assert_eq!(f.client.downs().len(), 1);
    assert_eq!(f.client.downs()[0].0, b, "the lock surface, not alpha");
    assert_eq!(f.client.count(&Seen::Up { surface: b, id: 0 }), 1);
}

#[test]
fn any_fingers_serial_starts_a_drag_like_mutter() {
    let mut f = fixture();
    f.down(0, alpha());
    f.frame();
    f.down(1, (250.0, 250.0).into());
    f.frame();
    sync(&mut f);
    let downs = f.client.downs();
    assert_eq!(downs.len(), 2);
    let second = downs[1].2;
    let qh = f.queue.handle();
    let source = f
        .client
        .data_manager
        .as_ref()
        .unwrap()
        .create_data_source(&qh, ());
    source.offer("text/plain".into());
    source.set_actions(wayland_client::protocol::wl_data_device_manager::DndAction::Copy);
    f.client
        .data_device
        .as_ref()
        .unwrap()
        .start_drag(Some(&source), &f.surfaces[0], None, second);
    sync(&mut f);
    assert!(
        f.comp.client_dnd_active(),
        "the second finger's down serial is a grab serial"
    );
    // The drag follows its finger onto beta.
    f.motion(1, beta());
    f.frame();
    sync(&mut f);
    assert_eq!(f.client.drag_enters.last(), Some(&f.surface_id(1)));
    f.up(1);
    f.frame();
    sync(&mut f);
    assert!(!f.comp.client_dnd_active(), "lifting its finger ends it");
    f.up(0);
    f.frame();
    sync(&mut f);
}

#[test]
fn a_stale_touch_serial_starts_no_drag() {
    let mut f = fixture();
    f.down(0, alpha());
    f.frame();
    sync(&mut f);
    let serial = f.client.downs()[0].2;
    f.up(0);
    f.frame();
    sync(&mut f);
    let qh = f.queue.handle();
    let source = f
        .client
        .data_manager
        .as_ref()
        .unwrap()
        .create_data_source(&qh, ());
    source.offer("text/plain".into());
    f.client
        .data_device
        .as_ref()
        .unwrap()
        .start_drag(Some(&source), &f.surfaces[0], None, serial);
    sync(&mut f);
    assert!(!f.comp.client_dnd_active());
}

/// gnome-shell 51's keyboard window slide: a field whose cursor would sit
/// under the keyboard moves its window up by the keyboard's height over
/// `KEYBOARD_ANIMATION_TIME` (EASE_OUT_QUAD), and back once it hides.
#[test]
fn the_keyboard_slides_a_covered_fields_window_up_and_back() {
    use std::time::Duration;
    use tuna_compositor::osk::KeyboardBox;
    let mut f = fixture();
    let alpha = f.ids[0];
    let surface = f.server_surface(0);
    // Alpha spans y 100..400; its field's cursor at surface y 250.
    let cursor = f.manager.field_cursor(&surface, (10, 250, 2, 20));
    assert_eq!(cursor.map(|c| (c.window, c.y)), Some((alpha, 350)));
    let keyboard = KeyboardBox {
        top: 340,
        height: 260,
    };
    let ms = Duration::from_millis;
    f.manager
        .osk_slide(Some(keyboard), cursor, ms(1000), ms(150));
    assert_eq!(f.manager.osk_slide_window(), Some(alpha));
    f.manager.step_osk_slide(ms(1075));
    // Half the time, three quarters of the way (ease-out-quad).
    assert_eq!(f.manager.geometry(alpha).unwrap().loc.y, 100 - 195);
    assert!(f.manager.osk_slide_running());
    f.manager.step_osk_slide(ms(1150));
    assert_eq!(f.manager.geometry(alpha).unwrap().loc.y, -160);
    assert!(!f.manager.osk_slide_running());
    assert_eq!(
        f.manager.geometry(f.ids[1]).unwrap().loc.y,
        100,
        "beta stays"
    );
    // The shifted field stays near the keyboard: nothing more moves.
    let shifted = f.manager.field_cursor(&surface, (10, 250, 2, 20));
    f.manager
        .osk_slide(Some(keyboard), shifted, ms(1200), ms(150));
    assert!(!f.manager.osk_slide_running());
    // The keyboard hides: back where it started.
    f.manager.osk_slide(None, shifted, ms(1300), ms(150));
    f.manager.step_osk_slide(ms(1450));
    assert_eq!(f.manager.geometry(alpha).unwrap().loc.y, 100);
    assert_eq!(f.manager.osk_slide_window(), None);
    // Reduced motion: at once, both ways.
    f.manager
        .osk_slide(Some(keyboard), cursor, ms(2000), Duration::ZERO);
    assert_eq!(f.manager.geometry(alpha).unwrap().loc.y, -160);
    f.manager.osk_slide(None, None, ms(2100), Duration::ZERO);
    assert_eq!(f.manager.geometry(alpha).unwrap().loc.y, 100);
}
