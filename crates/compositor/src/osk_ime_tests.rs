//! Actual Wayland consumers for private admission, fallback order and epochs.
use crate::{ClientState, TestCompositor};
use std::{os::unix::net::UnixStream, sync::Arc};
use wayland_client::{
    protocol::{
        wl_compositor::WlCompositor,
        wl_keyboard::{self, WlKeyboard},
        wl_registry::{self, WlRegistry},
        wl_seat::WlSeat,
        wl_surface::WlSurface,
    },
    Connection, Dispatch, EventQueue, QueueHandle,
};
use wayland_protocols::{
    wp::text_input::zv3::client::{
        zwp_text_input_manager_v3::ZwpTextInputManagerV3,
        zwp_text_input_v3::{self, ZwpTextInputV3},
    },
    xdg::shell::client::{
        xdg_surface::{self, XdgSurface},
        xdg_toplevel::XdgToplevel,
        xdg_wm_base::{self, XdgWmBase},
    },
};
use wayland_protocols_misc::zwp_input_method_v2::client::{
    zwp_input_method_manager_v2::ZwpInputMethodManagerV2,
    zwp_input_method_v2::{self, ZwpInputMethodV2},
};

#[allow(dead_code, unused_imports, non_camel_case_types, clippy::all)]
mod protocol {
    use wayland_client;
    pub mod __interfaces {
        use wayland_client::backend as wayland_backend;
        wayland_scanner::generate_interfaces!("protocols/tuna-osk-input-v1.xml");
    }
    use self::__interfaces::*;
    wayland_scanner::generate_client_code!("protocols/tuna-osk-input-v1.xml");
}
use protocol::tuna_osk_input_v1::TunaOskInputV1;
#[derive(Debug, PartialEq)]
enum Input {
    Key(u32, bool),
    Commit(String),
}
#[derive(Default)]
struct Client {
    global: Option<u32>,
    compositor: Option<WlCompositor>,
    seat: Option<WlSeat>,
    wm: Option<XdgWmBase>,
    text_manager: Option<ZwpTextInputManagerV3>,
    im_manager: Option<ZwpInputMethodManagerV2>,
    serial: u32,
    input: Vec<Input>,
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
                "tuna_osk_input_v1" => state.global = Some(name),
                "wl_compositor" => state.compositor = Some(registry.bind(name, 4, qh, ())),
                "wl_seat" => {
                    let seat: WlSeat = registry.bind(name, 7, qh, ());
                    seat.get_keyboard(qh, ());
                    state.seat = Some(seat);
                }
                "xdg_wm_base" => state.wm = Some(registry.bind(name, 1, qh, ())),
                "zwp_text_input_manager_v3" => {
                    state.text_manager = Some(registry.bind(name, 1, qh, ()))
                }
                "zwp_input_method_manager_v2" => {
                    state.im_manager = Some(registry.bind(name, 1, qh, ()))
                }
                _ => {}
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
        if let wl_keyboard::Event::Key {
            key,
            state: pressed,
            ..
        } = event
        {
            state.input.push(Input::Key(
                key,
                matches!(
                    pressed,
                    wayland_client::WEnum::Value(wl_keyboard::KeyState::Pressed)
                ),
            ));
        }
    }
}
impl Dispatch<ZwpTextInputV3, ()> for Client {
    fn event(
        state: &mut Self,
        text: &ZwpTextInputV3,
        event: zwp_text_input_v3::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_text_input_v3::Event::Enter { .. } => {
                text.enable();
                text.commit();
            }
            zwp_text_input_v3::Event::CommitString { text: Some(text) } => {
                state.input.push(Input::Commit(text))
            }
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
        if let zwp_input_method_v2::Event::Done = event {
            state.serial = state.serial.wrapping_add(1);
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
wayland_client::delegate_noop!(Client: ignore TunaOskInputV1);
wayland_client::delegate_noop!(Client: ignore WlCompositor);
wayland_client::delegate_noop!(Client: ignore WlSeat);
wayland_client::delegate_noop!(Client: ignore WlSurface);
wayland_client::delegate_noop!(Client: ignore XdgToplevel);
wayland_client::delegate_noop!(Client: ignore ZwpTextInputManagerV3);
wayland_client::delegate_noop!(Client: ignore ZwpInputMethodManagerV2);
struct Peer {
    queue: EventQueue<Client>,
    state: Client,
    registry: WlRegistry,
}
fn connect(comp: &mut TestCompositor, trusted: bool) -> Peer {
    let (server, stream) = UnixStream::pair().unwrap();
    comp.display
        .handle()
        .insert_client(
            server,
            Arc::new(if trusted {
                ClientState::ime_bridge()
            } else {
                ClientState::default()
            }),
        )
        .unwrap();
    let conn = Connection::from_socket(stream).unwrap();
    let queue = conn.new_event_queue();
    let registry = conn.display().get_registry(&queue.handle(), ());
    Peer {
        queue,
        state: Client::default(),
        registry,
    }
}
fn pump(comp: &mut TestCompositor, peer: &mut Peer) -> Result<(), String> {
    peer.queue.flush().map_err(|e| e.to_string())?;
    comp.pump();
    if let Some(read) = peer.queue.prepare_read() {
        read.read().map_err(|e| e.to_string())?;
    }
    peer.queue
        .dispatch_pending(&mut peer.state)
        .map(|_| ())
        .map_err(|e| e.to_string())
}
fn settle(
    comp: &mut TestCompositor,
    manager: &mut crate::windows::WindowManager,
    app: &mut Peer,
    bridge: &mut Peer,
) {
    for _ in 0..20 {
        pump(comp, app).unwrap();
        manager.reconcile(&mut comp.state);
        pump(comp, bridge).unwrap();
    }
}
#[test]
fn virtual_text_is_private_even_when_an_ordinary_client_guesses_the_global() {
    let mut comp = TestCompositor::new();
    let mut bridge = connect(&mut comp, true);
    let mut app = connect(&mut comp, false);
    pump(&mut comp, &mut bridge).unwrap();
    pump(&mut comp, &mut app).unwrap();
    let global = bridge
        .state
        .global
        .expect("supervised bridge sees virtual input");
    assert!(
        app.state.global.is_none(),
        "ordinary apps must not discover virtual input"
    );
    let _forged: TunaOskInputV1 = app.registry.bind(global, 1, &app.queue.handle(), ());
    assert!(
        pump(&mut comp, &mut app).is_err(),
        "guessing the name must not grant admission"
    );
}
#[test]
fn virtual_fallback_requests_stay_ordered_once_and_cannot_cross_focus_or_lock() {
    let mut comp = TestCompositor::new();
    comp.state.set_output_size(1280, 800);
    let mut manager = comp.window_manager();
    let mut bridge = connect(&mut comp, true);
    let mut app = connect(&mut comp, false);
    settle(&mut comp, &mut manager, &mut app, &mut bridge);
    let qh = bridge.queue.handle();
    let osk: TunaOskInputV1 = bridge
        .registry
        .bind(bridge.state.global.unwrap(), 1, &qh, ());
    let im = bridge.state.im_manager.as_ref().unwrap().get_input_method(
        bridge.state.seat.as_ref().unwrap(),
        &qh,
        (),
    );
    let qh = app.queue.handle();
    let _text = app.state.text_manager.as_ref().unwrap().get_text_input(
        app.state.seat.as_ref().unwrap(),
        &qh,
        (),
    );
    let surface = app
        .state
        .compositor
        .as_ref()
        .unwrap()
        .create_surface(&qh, ());
    let xdg = app
        .state
        .wm
        .as_ref()
        .unwrap()
        .get_xdg_surface(&surface, &qh, ());
    let _top = xdg.get_toplevel(&qh, ());
    surface.commit();
    settle(&mut comp, &mut manager, &mut app, &mut bridge);
    let epoch = bridge.state.serial;
    assert!(epoch > 0);
    app.state.input.clear();
    // These are the production boundary requests for a declined/error key,
    // a consumed key's commit, then another fallback. No echo/test engine.
    osk.unhandled(epoch, u32::from('x'));
    im.commit_string("你".into());
    im.commit(epoch);
    osk.unhandled(epoch, u32::from('y'));
    settle(&mut comp, &mut manager, &mut app, &mut bridge);
    assert_eq!(
        app.state.input,
        vec![
            Input::Key(45, true),
            Input::Key(45, false),
            Input::Commit("你".into()),
            Input::Key(21, true),
            Input::Key(21, false)
        ]
    );
    app.state.input.clear();
    comp.state.set_shortcut_inhibition_locked(true);
    osk.unhandled(epoch, u32::from('z'));
    settle(&mut comp, &mut manager, &mut app, &mut bridge);
    assert!(
        app.state.input.is_empty(),
        "locked fallback must not type into the session"
    );
    comp.state.set_shortcut_inhibition_locked(false);
    let keyboard = comp.state.seat.get_keyboard().unwrap();
    keyboard.set_focus(
        &mut comp.state,
        None,
        smithay::utils::SERIAL_COUNTER.next_serial(),
    );
    osk.unhandled(epoch, u32::from('z'));
    im.commit_string("stale".into());
    im.commit(epoch);
    settle(&mut comp, &mut manager, &mut app, &mut bridge);
    assert!(
        app.state.input.is_empty(),
        "retired focus must reject raw and committed text"
    );
}
