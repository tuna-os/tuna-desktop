//! Input protocols (#60, #89): an IME types into a text field, a
//! locked pointer stays put while relative motion flows, and GNOME's
//! three-finger swipes classify as the shell's.

use std::os::unix::net::UnixStream;

use smithay::utils::{Logical, Point};
use tuna_compositor::motion_coalesce::MotionCoalescer;
use tuna_compositor::windows::{swipe_action, ManagerInput, SwipeAction, WindowManager};
use tuna_compositor::TestCompositor;
use wayland_client::{
    protocol::{
        wl_compositor::WlCompositor,
        wl_keyboard::WlKeyboard,
        wl_pointer::{self, WlPointer},
        wl_region::WlRegion,
        wl_registry::{self, WlRegistry},
        wl_seat::{self, WlSeat},
        wl_surface::WlSurface,
    },
    Connection, Dispatch, EventQueue, QueueHandle,
};
use wayland_protocols::wp::{
    pointer_constraints::zv1::client::{
        zwp_confined_pointer_v1::{self, ZwpConfinedPointerV1},
        zwp_locked_pointer_v1::{self, ZwpLockedPointerV1},
        zwp_pointer_constraints_v1::{Lifetime, ZwpPointerConstraintsV1},
    },
    relative_pointer::zv1::client::{
        zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1,
        zwp_relative_pointer_v1::{self, ZwpRelativePointerV1},
    },
    text_input::zv3::client::{
        zwp_text_input_manager_v3::ZwpTextInputManagerV3,
        zwp_text_input_v3::{self, ZwpTextInputV3},
    },
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};
use wayland_protocols_misc::zwp_input_method_v2::client::{
    zwp_input_method_keyboard_grab_v2::{self, ZwpInputMethodKeyboardGrabV2},
    zwp_input_method_manager_v2::ZwpInputMethodManagerV2,
    zwp_input_method_v2::{self, ZwpInputMethodV2},
    zwp_input_popup_surface_v2::{self, ZwpInputPopupSurfaceV2},
};

use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{Layer, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{self, Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1},
};

const ROUNDS: usize = 20;

#[derive(Default)]
struct Client {
    compositor: Option<WlCompositor>,
    seat: Option<WlSeat>,
    wm: Option<XdgWmBase>,
    text_input_manager: Option<ZwpTextInputManagerV3>,
    input_method_manager: Option<ZwpInputMethodManagerV2>,
    input_method_name: Option<u32>,
    relative_manager: Option<ZwpRelativePointerManagerV1>,
    constraints: Option<ZwpPointerConstraintsV1>,
    pointer: Option<WlPointer>,
    /// text-input: entered surface, committed strings.
    text_entered: bool,
    committed: Vec<String>,
    /// input-method: activations and `done` count (the commit serial).
    ime_active: bool,
    ime_done: u32,
    ime_unavailable: bool,
    preedits: Vec<String>,
    text_leaves: usize,
    text_enters: usize,
    ime_grab_events: usize,
    ime_keys: usize,
    ime_popup_events: usize,
    /// Relative motion received (dx, dy).
    relative: Vec<(f64, f64)>,
    pointer_entered: bool,
    pointer_local: Option<(f64, f64)>,
    layer_shell: Option<ZwlrLayerShellV1>,
    layer_surface: Option<WlSurface>,
    locked_events: usize,
    unlocked_events: usize,
    confined_events: usize,
    unconfined_events: usize,
    relative_raw: Vec<(f64, f64, f64, f64, u64)>,
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
            name, interface, ..
        } = event
        {
            match interface.as_str() {
                "wl_compositor" => state.compositor = Some(registry.bind(name, 4, qh, ())),
                "zwlr_layer_shell_v1" => state.layer_shell = Some(registry.bind(name, 4, qh, ())),
                "wl_seat" => state.seat = Some(registry.bind(name, 7, qh, ())),
                "xdg_wm_base" => state.wm = Some(registry.bind(name, 1, qh, ())),
                "zwp_text_input_manager_v3" => {
                    state.text_input_manager = Some(registry.bind(name, 1, qh, ()))
                }
                "zwp_input_method_manager_v2" => {
                    state.input_method_name = Some(name);
                    state.input_method_manager = Some(registry.bind(name, 1, qh, ()))
                }
                "zwp_relative_pointer_manager_v1" => {
                    state.relative_manager = Some(registry.bind(name, 1, qh, ()))
                }
                "zwp_pointer_constraints_v1" => {
                    state.constraints = Some(registry.bind(name, 1, qh, ()))
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
            if caps.contains(wl_seat::Capability::Pointer) && state.pointer.is_none() {
                state.pointer = Some(seat.get_pointer(qh, ()));
            }
        }
    }
}

impl Dispatch<WlPointer, ()> for Client {
    fn event(
        state: &mut Self,
        _: &WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Enter {
                surface_x,
                surface_y,
                ..
            } => {
                state.pointer_entered = true;
                state.pointer_local = Some((surface_x, surface_y));
            }
            wl_pointer::Event::Leave { .. } => state.pointer_entered = false,
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => state.pointer_local = Some((surface_x, surface_y)),
            _ => {}
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

impl Dispatch<ZwpTextInputV3, ()> for Client {
    fn event(
        state: &mut Self,
        text_input: &ZwpTextInputV3,
        event: zwp_text_input_v3::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_text_input_v3::Event::Enter { .. } => {
                // A focused text field: ask for input.
                state.text_entered = true;
                state.text_enters += 1;
                text_input.enable();
                text_input.commit();
            }
            zwp_text_input_v3::Event::CommitString { text: Some(text) } => {
                state.committed.push(text)
            }
            zwp_text_input_v3::Event::PreeditString {
                text: Some(text), ..
            } => state.preedits.push(text),
            zwp_text_input_v3::Event::Leave { .. } => state.text_leaves += 1,
            _ => {}
        }
    }
}

impl Dispatch<ZwpInputMethodV2, ()> for Client {
    fn event(
        state: &mut Self,
        _: &ZwpInputMethodV2,
        event: zwp_input_method_v2::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_input_method_v2::Event::Activate => state.ime_active = true,
            zwp_input_method_v2::Event::Deactivate => state.ime_active = false,
            zwp_input_method_v2::Event::Done => state.ime_done += 1,
            zwp_input_method_v2::Event::Unavailable => state.ime_unavailable = true,
            _ => {}
        }
    }
}

impl Dispatch<ZwpInputMethodKeyboardGrabV2, ()> for Client {
    fn event(
        state: &mut Self,
        _: &ZwpInputMethodKeyboardGrabV2,
        event: zwp_input_method_keyboard_grab_v2::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        state.ime_grab_events += 1;
        if matches!(event, zwp_input_method_keyboard_grab_v2::Event::Key { .. }) {
            state.ime_keys += 1;
        }
    }
}

impl Dispatch<ZwpInputPopupSurfaceV2, ()> for Client {
    fn event(
        state: &mut Self,
        _: &ZwpInputPopupSurfaceV2,
        _: zwp_input_popup_surface_v2::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        state.ime_popup_events += 1;
    }
}
impl Dispatch<ZwpRelativePointerV1, ()> for Client {
    fn event(
        state: &mut Self,
        _: &ZwpRelativePointerV1,
        event: zwp_relative_pointer_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_relative_pointer_v1::Event::RelativeMotion {
            dx,
            dy,
            dx_unaccel,
            dy_unaccel,
            utime_hi,
            utime_lo,
        } = event
        {
            state.relative.push((dx, dy));
            state.relative_raw.push((
                dx,
                dy,
                dx_unaccel,
                dy_unaccel,
                (u64::from(utime_hi) << 32) | u64::from(utime_lo),
            ));
        }
    }
}

wayland_client::delegate_noop!(Client: ignore WlCompositor);
wayland_client::delegate_noop!(Client: ignore WlSurface);
wayland_client::delegate_noop!(Client: ignore WlKeyboard);
wayland_client::delegate_noop!(Client: ignore XdgToplevel);
wayland_client::delegate_noop!(Client: ignore ZwpTextInputManagerV3);
wayland_client::delegate_noop!(Client: ignore ZwpInputMethodManagerV2);
wayland_client::delegate_noop!(Client: ignore ZwpRelativePointerManagerV1);
wayland_client::delegate_noop!(Client: ignore ZwpPointerConstraintsV1);
wayland_client::delegate_noop!(Client: ignore WlRegion);
wayland_client::delegate_noop!(Client: ignore ZwlrLayerShellV1);
impl Dispatch<ZwpLockedPointerV1, ()> for Client {
    fn event(
        state: &mut Self,
        _: &ZwpLockedPointerV1,
        event: zwp_locked_pointer_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_locked_pointer_v1::Event::Locked => state.locked_events += 1,
            zwp_locked_pointer_v1::Event::Unlocked => state.unlocked_events += 1,
            _ => {}
        }
    }
}
impl Dispatch<ZwpConfinedPointerV1, ()> for Client {
    fn event(
        state: &mut Self,
        _: &ZwpConfinedPointerV1,
        event: zwp_confined_pointer_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_confined_pointer_v1::Event::Confined => state.confined_events += 1,
            zwp_confined_pointer_v1::Event::Unconfined => state.unconfined_events += 1,
            _ => {}
        }
    }
}
impl Dispatch<ZwlrLayerSurfaceV1, ()> for Client {
    fn event(
        state: &mut Self,
        layer: &ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwlr_layer_surface_v1::Event::Configure { serial, .. } = event {
            layer.ack_configure(serial);
            state.layer_surface.as_ref().unwrap().commit();
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
    connect_with_admission(comp, false)
}

fn connect_ime_bridge(comp: &mut TestCompositor) -> Peer {
    connect_with_admission(comp, true)
}

fn connect_with_admission(comp: &mut TestCompositor, ime_bridge: bool) -> Peer {
    let (server, stream) = UnixStream::pair().unwrap();
    if ime_bridge {
        comp.add_ime_bridge_client(server);
    } else {
        comp.add_client(server);
    }
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
    toplevel.set_title("text field".into());
    surface.commit();
    std::mem::forget((xdg, toplevel));
    surface
}

#[test]
fn an_input_method_types_into_the_focused_text_field() {
    let mut comp = TestCompositor::new();
    comp.state.set_output_size(1280, 800);
    let mut manager = comp.window_manager();
    let mut app = connect(&mut comp);
    let mut ime = connect_ime_bridge(&mut comp);
    pump(&mut comp, &mut manager, &mut [&mut app, &mut ime]);

    // The server-owned bridge binds first, before an application maps.
    let qh = ime.queue.handle();
    let im = ime
        .client
        .input_method_manager
        .as_ref()
        .expect("input-method-v2 advertised")
        .get_input_method(ime.client.seat.as_ref().unwrap(), &qh, ());

    // The app maps a window (focused on map) with a text input.
    let qh = app.queue.handle();
    let _ti = app
        .client
        .text_input_manager
        .as_ref()
        .expect("text-input-v3 advertised")
        .get_text_input(app.client.seat.as_ref().unwrap(), &qh, ());
    let _surface = window(&mut app);
    pump(&mut comp, &mut manager, &mut [&mut app, &mut ime]);
    assert!(
        app.client.text_entered,
        "text-input entered the focused window"
    );
    assert!(ime.client.ime_active, "the input method was activated");

    im.commit_string("é".into());
    im.commit(ime.client.ime_done);
    pump(&mut comp, &mut manager, &mut [&mut app, &mut ime]);
    assert_eq!(app.client.committed, ["é"]);
}

#[test]
fn a_locked_pointer_stays_put_while_relative_motion_flows() {
    let mut comp = TestCompositor::new();
    comp.state.set_output_size(1280, 800);
    let mut manager = comp.window_manager();
    let mut game = connect(&mut comp);
    pump(&mut comp, &mut manager, &mut [&mut game]);
    let surface = window(&mut game);
    pump(&mut comp, &mut manager, &mut [&mut game]);
    let (_, geometry) = manager.visible_windows()[0].clone();
    let inside: Point<f64, Logical> = (geometry.loc + Point::from((50, 50))).to_f64();
    manager.on_input(
        &mut comp.state,
        ManagerInput::Motion {
            pos: inside,
            time: 1,
        },
    );
    pump(&mut comp, &mut manager, &mut [&mut game]);
    assert!(game.client.pointer_entered);

    let qh = game.queue.handle();
    let pointer = game.client.pointer.clone().unwrap();
    let _relative = game
        .client
        .relative_manager
        .as_ref()
        .expect("relative-pointer advertised")
        .get_relative_pointer(&pointer, &qh, ());
    let _lock = game
        .client
        .constraints
        .as_ref()
        .expect("pointer-constraints advertised")
        .lock_pointer(&surface, &pointer, None, Lifetime::Persistent, &qh, ());
    surface.commit();
    pump(&mut comp, &mut manager, &mut [&mut game]);

    manager.on_input(
        &mut comp.state,
        ManagerInput::Motion {
            pos: inside + Point::from((300.0, 200.0)),
            time: 2,
        },
    );
    manager.on_input(
        &mut comp.state,
        ManagerInput::RelativeMotion {
            delta: (12.0, -5.0).into(),
            delta_unaccel: (10.0, -4.0).into(),
            utime: 3000,
        },
    );
    pump(&mut comp, &mut manager, &mut [&mut game]);
    assert_eq!(
        manager.pointer_pos(),
        inside,
        "the locked pointer did not move"
    );
    assert_eq!(game.client.relative, [(12.0, -5.0)]);
}

/// Mutter folds a frame's motions and sums their relative deltas
/// (clutter-stage.c, clutter_stage_compress_motion): a locked pointer's
/// client gets one relative event carrying every delta exactly, with
/// the newest timestamp, and the pointer still stays put.
#[test]
fn coalesced_relative_motion_sums_every_delta_for_a_locked_pointer() {
    let mut comp = TestCompositor::new();
    comp.state.set_output_size(1280, 800);
    let mut manager = comp.window_manager();
    let mut game = connect(&mut comp);
    pump(&mut comp, &mut manager, &mut [&mut game]);
    let surface = window(&mut game);
    pump(&mut comp, &mut manager, &mut [&mut game]);
    let (_, geometry) = manager.visible_windows()[0].clone();
    let inside: Point<f64, Logical> = (geometry.loc + Point::from((50, 50))).to_f64();
    manager.on_input(
        &mut comp.state,
        ManagerInput::Motion {
            pos: inside,
            time: 1,
        },
    );
    pump(&mut comp, &mut manager, &mut [&mut game]);
    let qh = game.queue.handle();
    let pointer = game.client.pointer.clone().unwrap();
    let _relative = game
        .client
        .relative_manager
        .as_ref()
        .expect("relative-pointer advertised")
        .get_relative_pointer(&pointer, &qh, ());
    let _lock = game
        .client
        .constraints
        .as_ref()
        .expect("pointer-constraints advertised")
        .lock_pointer(&surface, &pointer, None, Lifetime::Persistent, &qh, ());
    surface.commit();
    pump(&mut comp, &mut manager, &mut [&mut game]);

    // Each backend event is an absolute position plus its raw delta,
    // as the libinput backend reports them.
    let deltas = [
        (1.5, -2.25, 1.25, -2.0),
        (0.125, 0.5, 0.0, 0.375),
        (-3.0, 4.75, -2.5, 4.0),
        (7.0, 0.0, 6.5, -0.125),
    ];
    let mut queue = MotionCoalescer::new();
    let mut target = inside;
    for (i, (dx, dy, ux, uy)) in deltas.into_iter().enumerate() {
        target += Point::from((dx, dy));
        for input in [
            ManagerInput::Motion {
                pos: target,
                time: 2 + i as u32,
            },
            ManagerInput::RelativeMotion {
                delta: (dx, dy).into(),
                delta_unaccel: (ux, uy).into(),
                utime: 3_000 + 1_000 * i as u64,
            },
        ] {
            assert_eq!(queue.absorb(input), None, "motion waits for the frame");
        }
    }
    for input in queue.take(std::time::Instant::now()).into_iter().flatten() {
        manager.on_input(&mut comp.state, input);
    }
    pump(&mut comp, &mut manager, &mut [&mut game]);
    assert_eq!(
        manager.pointer_pos(),
        inside,
        "the locked pointer did not move"
    );
    assert_eq!(
        game.client.relative_raw,
        [(5.625, 3.0, 5.25, 2.25, 6_000)],
        "one relative event, every delta summed exactly"
    );
}

#[test]
fn three_finger_swipes_follow_gnome() {
    assert_eq!(swipe_action(0.0, -150.0), Some(SwipeAction::OpenOverview));
    assert_eq!(swipe_action(10.0, 140.0), Some(SwipeAction::CloseOverview));
    assert_eq!(swipe_action(-160.0, 20.0), Some(SwipeAction::NextWorkspace));
    assert_eq!(
        swipe_action(160.0, -20.0),
        Some(SwipeAction::PreviousWorkspace)
    );
    assert_eq!(swipe_action(40.0, -60.0), None, "short swipes do nothing");
}

#[test]
fn focused_real_constraints_own_motion_before_native_pressure_accumulates() {
    for locked in [true, false] {
        let mut comp = TestCompositor::new();
        comp.state.set_output_size(1280, 800);
        let mut manager = comp.window_manager();
        let mut app = connect(&mut comp);
        pump(&mut comp, &mut manager, &mut [&mut app]);
        let surface = window(&mut app);
        pump(&mut comp, &mut manager, &mut [&mut app]);
        let (_, geometry) = manager.visible_windows()[0].clone();
        let inside: Point<f64, Logical> = (geometry.loc + Point::from((50, 50))).to_f64();
        manager.on_input(
            &mut comp.state,
            ManagerInput::Motion {
                pos: inside,
                time: 1,
            },
        );
        pump(&mut comp, &mut manager, &mut [&mut app]);
        assert!(app.client.pointer_entered);
        assert!(!manager.pointer_constraint_owns_motion(&comp.state));
        let qh = app.queue.handle();
        let pointer = app.client.pointer.clone().unwrap();
        let _relative = app
            .client
            .relative_manager
            .as_ref()
            .unwrap()
            .get_relative_pointer(&pointer, &qh, ());
        let constraints = app.client.constraints.as_ref().unwrap();
        let lock = locked.then(|| {
            constraints.lock_pointer(&surface, &pointer, None, Lifetime::Persistent, &qh, ())
        });
        let confinement = (!locked).then(|| {
            constraints.confine_pointer(&surface, &pointer, None, Lifetime::Persistent, &qh, ())
        });
        surface.commit();
        pump(&mut comp, &mut manager, &mut [&mut app]);
        assert!(
            manager.pointer_constraint_owns_motion(&comp.state),
            "pending focused constraint must already gate backend barriers"
        );

        let mut pressure = tuna_compositor::corner_pressure::CornerPressure::default();
        let layout = comp.state.hot_corner_outputs();
        // Start with real partial pressure, then the protocol owner's gate
        // disables interception and discards it before a large native hit.
        for time in 0..6 {
            assert!(pressure
                .motion(
                    (0.0, 10.0).into(),
                    (-15.0, 0.0).into(),
                    time,
                    &layout,
                    false,
                    true
                )
                .1
                .is_none());
        }
        let (position, trigger) = pressure.motion(
            (0.0, 10.0).into(),
            (-1000.0, 0.0).into(),
            7,
            &layout,
            false,
            !manager.pointer_constraint_owns_motion(&comp.state),
        );
        assert_eq!(
            position,
            (-1000.0, 10.0).into(),
            "barrier must not trap the constraint owner's motion"
        );
        assert!(
            trigger.is_none(),
            "no pressure action is allowed behind a real constraint"
        );
        manager.on_input(
            &mut comp.state,
            ManagerInput::Motion {
                pos: inside + Point::from((-1000.0, -1000.0)),
                time: 8,
            },
        );
        manager.on_input(
            &mut comp.state,
            ManagerInput::RelativeMotion {
                delta: (-1000.0, -1000.0).into(),
                delta_unaccel: (-900.0, -900.0).into(),
                utime: 9000,
            },
        );
        pump(&mut comp, &mut manager, &mut [&mut app]);
        assert!(manager.pointer_constraint_owns_motion(&comp.state));
        if locked {
            assert_eq!(manager.pointer_pos(), inside);
        } else {
            assert!(geometry.to_f64().contains(manager.pointer_pos()));
        }
        assert_eq!(
            app.client.relative,
            [(-1000.0, -1000.0)],
            "original relative client payload must still arrive"
        );
        if let Some(lock) = lock {
            lock.destroy();
        }
        if let Some(confinement) = confinement {
            confinement.destroy();
        }
        pump(&mut comp, &mut manager, &mut [&mut app]);
        assert!(
            !manager.pointer_constraint_owns_motion(&comp.state),
            "destroying the owner restores native barrier eligibility"
        );
        assert!(
            pressure
                .motion(
                    (0.0, 10.0).into(),
                    (-15.0, 0.0).into(),
                    10,
                    &layout,
                    false,
                    !manager.pointer_constraint_owns_motion(&comp.state)
                )
                .1
                .is_none(),
            "old pressure must not survive constraint ownership"
        );
        assert!(pressure
            .motion(
                (0.0, 10.0).into(),
                (-100.0, 0.0).into(),
                11,
                &layout,
                false,
                !manager.pointer_constraint_owns_motion(&comp.state)
            )
            .1
            .is_some());
    }
}

/// Real layer protocol, nonzero global origin, committed constraint
/// regions, activation events and unchanged raw relative payload.
#[test]
fn layer_constraints_use_actual_origin_and_commit_regions_before_activation() {
    for locked in [true, false] {
        for lifetime in [Lifetime::Persistent, Lifetime::Oneshot] {
            let mut comp = TestCompositor::new();
            comp.state.set_output_size(1280, 800);
            comp.state.set_output_location("tuna-0", (-400, 100));
            let mut manager = comp.window_manager();
            let mut app = connect(&mut comp);
            pump(&mut comp, &mut manager, &mut [&mut app]);
            let qh = app.queue.handle();
            let surface = app
                .client
                .compositor
                .as_ref()
                .unwrap()
                .create_surface(&qh, ());
            app.client.layer_surface = Some(surface.clone());
            let layer = app.client.layer_shell.as_ref().unwrap().get_layer_surface(
                &surface,
                None,
                Layer::Overlay,
                "constraint-test".into(),
                &qh,
                (),
            );
            layer.set_size(128, 128);
            layer.set_anchor(Anchor::Top | Anchor::Left);
            layer.set_exclusive_zone(0);
            layer.set_keyboard_interactivity(KeyboardInteractivity::None);
            surface.commit();
            pump(&mut comp, &mut manager, &mut [&mut app]);
            let inside: Point<f64, Logical> = (-390.0, 110.0).into();
            manager.on_input(
                &mut comp.state,
                ManagerInput::Motion {
                    pos: inside,
                    time: 1,
                },
            );
            pump(&mut comp, &mut manager, &mut [&mut app]);
            assert_eq!(app.client.pointer_local, Some((10.0, 10.0)));
            let region = app
                .client
                .compositor
                .as_ref()
                .unwrap()
                .create_region(&qh, ());
            region.add(20, 20, 40, 40);
            let pointer = app.client.pointer.as_ref().unwrap();
            let _relative = app
                .client
                .relative_manager
                .as_ref()
                .unwrap()
                .get_relative_pointer(pointer, &qh, ());
            let constraints = app.client.constraints.as_ref().unwrap();
            let lock = locked.then(|| {
                constraints.lock_pointer(&surface, pointer, Some(&region), lifetime, &qh, ())
            });
            let confinement = (!locked).then(|| {
                constraints.confine_pointer(&surface, pointer, Some(&region), lifetime, &qh, ())
            });
            surface.commit();
            pump(&mut comp, &mut manager, &mut [&mut app]);
            assert!(
                !manager.pointer_constraint_owns_motion(&comp.state),
                "outside-region registration cannot own motion"
            );
            manager.on_input(
                &mut comp.state,
                ManagerInput::Motion {
                    pos: (-370.0, 130.0).into(),
                    time: 2,
                },
            );
            pump(&mut comp, &mut manager, &mut [&mut app]);
            assert_eq!(app.client.locked_events + app.client.confined_events, 0);
            assert!(
                manager.pointer_constraint_owns_motion(&comp.state),
                "eligible pending constraint gates the next native motion"
            );
            // The actual committed input region is narrower than the layer.
            surface.set_input_region(Some(&region));
            surface.commit();
            pump(&mut comp, &mut manager, &mut [&mut app]);
            manager.on_input(
                &mut comp.state,
                ManagerInput::Motion {
                    pos: (-1000.0, -1000.0).into(),
                    time: 3,
                },
            );
            manager.on_input(
                &mut comp.state,
                ManagerInput::RelativeMotion {
                    delta: (-630.0, -1130.0).into(),
                    delta_unaccel: (-600.0, -1100.0).into(),
                    utime: 0x1_0000_1234,
                },
            );
            pump(&mut comp, &mut manager, &mut [&mut app]);
            assert_eq!(app.client.locked_events + app.client.confined_events, 1);
            if locked {
                assert_eq!(manager.pointer_pos(), (-370.0, 130.0).into());
            } else {
                assert!(manager.pointer_pos().x >= -380.0 && manager.pointer_pos().x < -340.0);
                assert!(manager.pointer_pos().y >= 120.0 && manager.pointer_pos().y < 160.0);
            }
            assert_eq!(
                app.client.relative_raw,
                [(-630.0, -1130.0, -600.0, -1100.0, 0x1_0000_1234)]
            );
            if !locked {
                manager.on_input(
                    &mut comp.state,
                    ManagerInput::Motion {
                        pos: (1000.0, 135.0).into(),
                        time: 4,
                    },
                );
                pump(&mut comp, &mut manager, &mut [&mut app]);
                assert!(manager.pointer_pos().x < -340.0 && manager.pointer_pos().x > -341.0);
                assert!(
                    app.client.pointer_entered,
                    "a valid negative-origin subpixel edge must retain actual layer focus"
                );
                assert!(manager.pointer_constraint_owns_motion(&comp.state));
                manager.on_input(
                    &mut comp.state,
                    ManagerInput::RelativeMotion {
                        delta: (1370.0, 0.0).into(),
                        delta_unaccel: (1300.0, 0.0).into(),
                        utime: 0x1_0000_5678,
                    },
                );
                pump(&mut comp, &mut manager, &mut [&mut app]);
                assert_eq!(
                    app.client.relative_raw.last(),
                    Some(&(1370.0, 0.0, 1300.0, 0.0, 0x1_0000_5678))
                );
            }
            let empty = app
                .client
                .compositor
                .as_ref()
                .unwrap()
                .create_region(&qh, ());
            if let Some(lock) = &lock {
                lock.set_region(Some(&empty));
            }
            if let Some(confinement) = &confinement {
                confinement.set_region(Some(&empty));
            }
            pump(&mut comp, &mut manager, &mut [&mut app]);
            manager.on_input(
                &mut comp.state,
                ManagerInput::Motion {
                    pos: (-365.0, 135.0).into(),
                    time: 4,
                },
            );
            pump(&mut comp, &mut manager, &mut [&mut app]);
            assert_eq!(
                app.client.unlocked_events + app.client.unconfined_events,
                0,
                "pending region update must not apply before surface commit"
            );
            surface.commit();
            pump(&mut comp, &mut manager, &mut [&mut app]);
            manager.on_input(
                &mut comp.state,
                ManagerInput::Motion {
                    pos: (-365.0, 135.0).into(),
                    time: 5,
                },
            );
            pump(&mut comp, &mut manager, &mut [&mut app]);
            assert_eq!(app.client.unlocked_events + app.client.unconfined_events, 1);
            assert!(!manager.pointer_constraint_owns_motion(&comp.state));
            if lifetime == Lifetime::Persistent {
                if let Some(lock) = &lock {
                    lock.set_region(Some(&region));
                }
                if let Some(confinement) = &confinement {
                    confinement.set_region(Some(&region));
                }
                surface.commit();
                pump(&mut comp, &mut manager, &mut [&mut app]);
                manager.on_input(
                    &mut comp.state,
                    ManagerInput::Motion {
                        pos: (-365.0, 135.0).into(),
                        time: 6,
                    },
                );
                pump(&mut comp, &mut manager, &mut [&mut app]);
                assert_eq!(app.client.locked_events + app.client.confined_events, 2);
            } else {
                manager.on_input(
                    &mut comp.state,
                    ManagerInput::Motion {
                        pos: (-365.0, 135.0).into(),
                        time: 6,
                    },
                );
                pump(&mut comp, &mut manager, &mut [&mut app]);
                assert_eq!(
                    app.client.locked_events + app.client.confined_events,
                    1,
                    "oneshot object is defunct after deactivation"
                );
            }
            if let Some(lock) = lock {
                lock.destroy();
            }
            if let Some(confinement) = confinement {
                confinement.destroy();
            }
            layer.destroy();
            surface.destroy();
            pump(&mut comp, &mut manager, &mut [&mut app]);
        }
    }
}

#[test]
fn unavailable_input_methods_cannot_inject_or_retire_the_live_resource() {
    let mut comp = TestCompositor::new();
    comp.state.set_output_size(1280, 800);
    let mut manager = comp.window_manager();
    let mut owner = connect_ime_bridge(&mut comp);
    let mut app = connect(&mut comp);
    let mut foreign = connect_ime_bridge(&mut comp);
    pump(
        &mut comp,
        &mut manager,
        &mut [&mut app, &mut owner, &mut foreign],
    );
    let im = owner
        .client
        .input_method_manager
        .as_ref()
        .unwrap()
        .get_input_method(
            owner.client.seat.as_ref().unwrap(),
            &owner.queue.handle(),
            (),
        );
    let text = app
        .client
        .text_input_manager
        .as_ref()
        .unwrap()
        .get_text_input(app.client.seat.as_ref().unwrap(), &app.queue.handle(), ());
    let _surface = window(&mut app);
    text.enable();
    text.commit();
    pump(
        &mut comp,
        &mut manager,
        &mut [&mut app, &mut owner, &mut foreign],
    );
    let epoch = owner.client.ime_done;
    assert!(epoch > 0);
    let enters = app.client.text_enters;
    let rejected = foreign
        .client
        .input_method_manager
        .as_ref()
        .unwrap()
        .get_input_method(
            foreign.client.seat.as_ref().unwrap(),
            &foreign.queue.handle(),
            (),
        );
    pump(
        &mut comp,
        &mut manager,
        &mut [&mut app, &mut owner, &mut foreign],
    );
    assert!(
        foreign.client.ime_unavailable,
        "the newcomer, not the live owner, is unavailable"
    );
    assert_eq!(
        app.client.text_enters, enters,
        "unavailable creation must not re-enter the live field"
    );
    assert!(!owner.client.ime_unavailable);
    assert_eq!(
        owner.client.ime_done, epoch,
        "a newcomer cannot reset the live done epoch"
    );
    app.client.committed.clear();
    app.client.preedits.clear();
    let leaves = app.client.text_leaves;
    rejected.commit_string("foreign".into());
    rejected.set_preedit_string("foreign".into(), 0, 0);
    rejected.delete_surrounding_text(1, 1);
    rejected.commit(epoch);
    pump(
        &mut comp,
        &mut manager,
        &mut [&mut app, &mut owner, &mut foreign],
    );
    assert!(app.client.committed.is_empty());
    assert!(app.client.preedits.is_empty());
    rejected.destroy();
    pump(
        &mut comp,
        &mut manager,
        &mut [&mut app, &mut owner, &mut foreign],
    );
    assert_eq!(
        app.client.text_leaves, leaves,
        "foreign destruction cannot retire the live field"
    );
    im.commit_string("owner".into());
    im.commit(epoch);
    pump(
        &mut comp,
        &mut manager,
        &mut [&mut app, &mut owner, &mut foreign],
    );
    assert_eq!(app.client.committed, ["owner"]);

    // Resource identity, not merely connection identity, owns the seat.
    let duplicate = owner
        .client
        .input_method_manager
        .as_ref()
        .unwrap()
        .get_input_method(
            owner.client.seat.as_ref().unwrap(),
            &owner.queue.handle(),
            (),
        );
    pump(&mut comp, &mut manager, &mut [&mut app, &mut owner]);
    assert!(owner.client.ime_unavailable);
    assert_eq!(
        app.client.text_enters, enters,
        "same-client unavailable creation must not re-enter the field"
    );
    app.client.committed.clear();
    duplicate.commit_string("same-client impostor".into());
    duplicate.commit(epoch);
    duplicate.destroy();
    pump(&mut comp, &mut manager, &mut [&mut app, &mut owner]);
    assert!(app.client.committed.is_empty());
    assert_eq!(app.client.text_leaves, leaves);
    im.commit_string("still owner".into());
    im.commit(epoch);
    pump(&mut comp, &mut manager, &mut [&mut app, &mut owner]);
    assert_eq!(app.client.committed, ["still owner"]);

    // A real owner's destruction releases the seat for a fresh resource.
    im.destroy();
    pump(&mut comp, &mut manager, &mut [&mut app, &mut owner]);
    owner.client.ime_unavailable = false;
    owner.client.ime_done = 0;
    let replacement = owner
        .client
        .input_method_manager
        .as_ref()
        .unwrap()
        .get_input_method(
            owner.client.seat.as_ref().unwrap(),
            &owner.queue.handle(),
            (),
        );
    text.enable();
    text.commit();
    pump(&mut comp, &mut manager, &mut [&mut app, &mut owner]);
    assert!(!owner.client.ime_unavailable);
    app.client.committed.clear();
    replacement.commit_string("replacement".into());
    replacement.commit(owner.client.ime_done);
    pump(&mut comp, &mut manager, &mut [&mut app, &mut owner]);
    assert_eq!(app.client.committed, ["replacement"]);
}

#[test]
fn inert_input_method_new_ids_have_no_role_events_or_grab_side_effects() {
    for same_client in [false, true] {
        let mut comp = TestCompositor::new();
        comp.state.set_output_size(1280, 800);
        let mut manager = comp.window_manager();
        let mut owner = connect_ime_bridge(&mut comp);
        let mut app = connect(&mut comp);
        let mut foreign = connect_ime_bridge(&mut comp);
        pump(
            &mut comp,
            &mut manager,
            &mut [&mut app, &mut owner, &mut foreign],
        );
        let im = owner
            .client
            .input_method_manager
            .as_ref()
            .unwrap()
            .get_input_method(
                owner.client.seat.as_ref().unwrap(),
                &owner.queue.handle(),
                (),
            );
        let text = app
            .client
            .text_input_manager
            .as_ref()
            .unwrap()
            .get_text_input(app.client.seat.as_ref().unwrap(), &app.queue.handle(), ());
        let _surface = window(&mut app);
        text.enable();
        text.commit();
        pump(
            &mut comp,
            &mut manager,
            &mut [&mut app, &mut owner, &mut foreign],
        );
        let _owner_grab = im.grab_keyboard(&owner.queue.handle(), ());
        pump(
            &mut comp,
            &mut manager,
            &mut [&mut app, &mut owner, &mut foreign],
        );
        assert!(owner.client.ime_grab_events > 0);
        let grab_events = owner.client.ime_grab_events;
        let leaves = app.client.text_leaves;
        let target = if same_client {
            &mut owner
        } else {
            &mut foreign
        };
        let rejected = target
            .client
            .input_method_manager
            .as_ref()
            .unwrap()
            .get_input_method(
                target.client.seat.as_ref().unwrap(),
                &target.queue.handle(),
                (),
            );
        let surface = target
            .client
            .compositor
            .as_ref()
            .unwrap()
            .create_surface(&target.queue.handle(), ());
        let popup = rejected.get_input_popup_surface(&surface, &target.queue.handle(), ());
        let inert_grab = rejected.grab_keyboard(&target.queue.handle(), ());
        pump(
            &mut comp,
            &mut manager,
            &mut [&mut app, &mut owner, &mut foreign],
        );
        assert_eq!(
            owner.client.ime_grab_events, grab_events,
            "inert children must emit no keymap/repeat/modifiers"
        );
        assert_eq!(foreign.client.ime_grab_events, 0);
        assert_eq!(
            owner.client.ime_popup_events + foreign.client.ime_popup_events,
            0
        );
        assert_eq!(app.client.text_leaves, leaves);
        inert_grab.release();
        popup.destroy();
        rejected.destroy();
        pump(
            &mut comp,
            &mut manager,
            &mut [&mut app, &mut owner, &mut foreign],
        );
        let keys = owner.client.ime_keys;
        manager.keyboard_key(&mut comp.state, 30, true, 1);
        manager.keyboard_key(&mut comp.state, 30, false, 2);
        pump(
            &mut comp,
            &mut manager,
            &mut [&mut app, &mut owner, &mut foreign],
        );
        assert_eq!(
            owner.client.ime_keys,
            keys + 2,
            "inert child destruction must leave the real grab installed"
        );
        assert_eq!(foreign.client.ime_keys, 0);
        assert_eq!(app.client.text_leaves, leaves);
        im.commit_string("still owned".into());
        im.commit(owner.client.ime_done);
        pump(
            &mut comp,
            &mut manager,
            &mut [&mut app, &mut owner, &mut foreign],
        );
        assert_eq!(app.client.committed, ["still owned"]);
        // Ignoring the popup request did not reserve its surface's role.
        let target = if same_client {
            &mut owner
        } else {
            &mut foreign
        };
        let xdg = target.client.wm.as_ref().unwrap().get_xdg_surface(
            &surface,
            &target.queue.handle(),
            (),
        );
        let _top = xdg.get_toplevel(&target.queue.handle(), ());
        surface.commit();
        pump(
            &mut comp,
            &mut manager,
            &mut [&mut app, &mut owner, &mut foreign],
        );
    }
}

#[test]
fn ordinary_and_spoofed_clients_cannot_discover_or_force_bind_an_input_method() {
    for spoof_identity in [false, true] {
        let mut comp = TestCompositor::new();
        let mut manager = comp.window_manager();
        let mut app = connect(&mut comp);
        pump(&mut comp, &mut manager, &mut [&mut app]);
        assert!(app.client.input_method_manager.is_none());
        assert!(
            app.client.input_method_name.is_none(),
            "hidden before any bridge claims the seat"
        );
        if spoof_identity {
            let surface = app
                .client
                .compositor
                .as_ref()
                .unwrap()
                .create_surface(&app.queue.handle(), ());
            let xdg =
                app.client
                    .wm
                    .as_ref()
                    .unwrap()
                    .get_xdg_surface(&surface, &app.queue.handle(), ());
            let top = xdg.get_toplevel(&app.queue.handle(), ());
            top.set_app_id("tuna-ibus-bridge".into());
            top.set_title("tuna-ibus-bridge".into());
            surface.commit();
            pump(&mut comp, &mut manager, &mut [&mut app]);
            assert!(
                app.client.input_method_manager.is_none(),
                "application metadata cannot confer transport authority"
            );
        }
        // Both connections originate from this same executable and PID.
        // Only the server-owned private connection is authorized.
        let mut bridge = connect_ime_bridge(&mut comp);
        pump(&mut comp, &mut manager, &mut [&mut app, &mut bridge]);
        let name = bridge
            .client
            .input_method_name
            .expect("authorized bridge sees the manager");
        let _forced: ZwpInputMethodManagerV2 = app.registry.bind(name, 1, &app.queue.handle(), ());
        let mut denied = false;
        for _ in 0..ROUNDS {
            let _ = app.queue.flush();
            comp.pump();
            if let Some(guard) = app.queue.prepare_read() {
                let _ = guard.read();
            }
            let _ = app.queue.dispatch_pending(&mut app.client);
            if app._conn.backend().last_error().is_some() {
                denied = true;
                break;
            }
        }
        assert!(
            denied,
            "knowing the private global name must not bypass forced-bind admission"
        );
        // libwayland can expose wl_display.invalid_object (code zero) as a
        // terminal I/O error. Check connection death, not its error variant.
        assert!(app._conn.backend().last_error().is_some());
        assert!(
            bridge._conn.backend().last_error().is_none(),
            "denial does not retire the admitted connection"
        );
        let _owner = bridge
            .client
            .input_method_manager
            .as_ref()
            .unwrap()
            .get_input_method(
                bridge.client.seat.as_ref().unwrap(),
                &bridge.queue.handle(),
                (),
            );
        pump(&mut comp, &mut manager, &mut [&mut bridge]);
        assert!(
            !bridge.client.ime_unavailable,
            "denied public client never wins first ownership"
        );
    }
}

#[test]
fn a_copied_helper_on_a_public_connection_cannot_claim_input_method_authority() {
    if let Some(name) = std::env::var_os("TUNA_TEST_IM_DENIAL_GLOBAL") {
        let connection = Connection::connect_to_env().expect("inherited ordinary socket");
        let mut queue = connection.new_event_queue::<Client>();
        let registry = connection.display().get_registry(&queue.handle(), ());
        let mut client = Client::default();
        queue.roundtrip(&mut client).unwrap();
        assert!(
            client.input_method_manager.is_none(),
            "copy sees no privileged global"
        );
        let name = name.to_str().unwrap().parse::<u32>().unwrap();
        let _forced: ZwpInputMethodManagerV2 = registry.bind(name, 1, &queue.handle(), ());
        assert!(
            queue.roundtrip(&mut client).is_err(),
            "copy cannot force-bind a known private global"
        );
        assert!(connection.backend().last_error().is_some());
        return;
    }
    let mut comp = TestCompositor::new();
    let mut manager = comp.window_manager();
    let mut bridge = connect_ime_bridge(&mut comp);
    pump(&mut comp, &mut manager, &mut [&mut bridge]);
    let name = bridge.client.input_method_name.unwrap();
    let directory = tempfile::tempdir().unwrap();
    let copy = directory.path().join("tuna-ibus-bridge");
    std::fs::copy(std::env::current_exe().unwrap(), &copy).unwrap();
    let (server, stream) = UnixStream::pair().unwrap();
    comp.add_client(server);
    // Same executable bytes under a spoofed helper name, in a distinct real
    // process. Only this public connection is passed; no trusted marker is.
    let mut child = std::process::Command::new(copy)
        .args([
            "--exact",
            "a_copied_helper_on_a_public_connection_cannot_claim_input_method_authority",
            "--nocapture",
        ])
        .env("TUNA_TEST_IM_DENIAL_GLOBAL", name.to_string())
        .env("WAYLAND_SOCKET", "0")
        .stdin(std::process::Stdio::from(std::os::fd::OwnedFd::from(
            stream,
        )))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        comp.pump();
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if std::time::Instant::now() >= deadline {
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            panic!(
                "copied helper missed its deadline: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "copied helper failed its denial checks:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let _owner = bridge
        .client
        .input_method_manager
        .as_ref()
        .unwrap()
        .get_input_method(
            bridge.client.seat.as_ref().unwrap(),
            &bridge.queue.handle(),
            (),
        );
    pump(&mut comp, &mut manager, &mut [&mut bridge]);
    assert!(!bridge.client.ime_unavailable);
}
