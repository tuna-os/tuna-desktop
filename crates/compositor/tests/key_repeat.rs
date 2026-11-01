//! wl_seat 10 as Mutter 51 advertises it (#364, #588): version 10
//! keyboards are told `repeat_info(0, 0)` and hear the compositor's own
//! repeats as `key(state = repeated)`; older keyboards keep repeating by
//! themselves from the GNOME settings. Real clients over a socket, the
//! repeat clock driven through `run_key_repeat`.

use std::collections::HashMap;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use tuna_compositor::windows::{ManagerInput, WindowManager};
use tuna_compositor::TestCompositor;
use wayland_client::{
    protocol::{
        wl_compositor::WlCompositor,
        wl_keyboard::{self, KeyState, WlKeyboard},
        wl_registry::{self, WlRegistry},
        wl_seat::{self, WlSeat},
        wl_surface::WlSurface,
    },
    Connection, Dispatch, EventQueue, QueueHandle, WEnum,
};
use wayland_protocols::wp::text_input::zv3::client::{
    zwp_text_input_manager_v3::ZwpTextInputManagerV3,
    zwp_text_input_v3::{self, ZwpTextInputV3},
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
};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};

const ROUNDS: usize = 10;
const KEY_A: u32 = 30;
const KEY_B: u32 = 48;
const KEY_LEFTSHIFT: u32 = 42;
const KEY_R: u32 = 19;
const PRESS_TIME: u32 = 5000;

/// One key event: serial, time, key, state.
type Key = (u32, u32, u32, WEnum<KeyState>);

#[derive(Default)]
struct Client {
    globals: HashMap<String, (u32, u32)>,
    /// By the version of the seat each keyboard came from.
    repeat_info: HashMap<u32, (i32, i32)>,
    keys: HashMap<u32, Vec<Key>>,
    entered: bool,
    ime_active: bool,
    grab_repeat_info: Option<(i32, i32)>,
    grab_keys: Vec<(u32, u32, WEnum<KeyState>)>,
    keymaps: HashMap<u32, usize>,
    grab_keymaps: usize,
    modifiers: Vec<(u32, u32, u32, u32, u32)>,
    wire_order: Vec<(u32, &'static str)>,
}

impl Client {
    fn keys(&self, version: u32) -> &[Key] {
        self.keys.get(&version).map_or(&[], Vec::as_slice)
    }

    fn repeated(&self, version: u32) -> Vec<Key> {
        self.keys(version)
            .iter()
            .copied()
            .filter(|key| key.3 == WEnum::Value(KeyState::Repeated))
            .collect()
    }
}

impl Dispatch<WlRegistry, ()> for Client {
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

impl Dispatch<WlSeat, u32> for Client {
    fn event(
        _: &mut Self,
        seat: &WlSeat,
        event: wl_seat::Event,
        version: &u32,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(caps),
        } = event
        {
            if caps.contains(wl_seat::Capability::Keyboard) {
                seat.get_keyboard(qh, *version);
            }
        }
    }
}

impl Dispatch<WlKeyboard, u32> for Client {
    fn event(
        state: &mut Self,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        version: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Keymap { .. } => {
                *state.keymaps.entry(*version).or_default() += 1;
            }
            wl_keyboard::Event::Modifiers { mods_depressed, mods_latched, mods_locked, group, .. } => {
                state.modifiers.push((*version, mods_depressed, mods_latched, mods_locked, group));
                state.wire_order.push((*version, "modifiers"));
            }
            wl_keyboard::Event::RepeatInfo { rate, delay } => {
                state.repeat_info.insert(*version, (rate, delay));
            }
            wl_keyboard::Event::Enter { .. } => state.entered = true,
            wl_keyboard::Event::Key {
                serial,
                time,
                key,
                state: key_state,
            } => {
                state.wire_order.push((*version,
                    if key_state == WEnum::Value(KeyState::Repeated) { "repeat" } else { "key" }));
                state.keys.entry(*version).or_default().push((serial, time, key, key_state));
            }
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
        _: &mut Self,
        text_input: &ZwpTextInputV3,
        event: zwp_text_input_v3::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_text_input_v3::Event::Enter { .. } = event {
            text_input.enable();
            text_input.commit();
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
        match event {
            zwp_input_method_keyboard_grab_v2::Event::Keymap { .. } => state.grab_keymaps += 1,
            zwp_input_method_keyboard_grab_v2::Event::RepeatInfo { rate, delay } => {
                state.grab_repeat_info = Some((rate, delay));
            }
            zwp_input_method_keyboard_grab_v2::Event::Key {
                time,
                key,
                state: key_state,
                ..
            } => state.grab_keys.push((time, key, key_state)),
            _ => {}
        }
    }
}

wayland_client::delegate_noop!(Client: ignore WlCompositor);
wayland_client::delegate_noop!(Client: ignore WlSurface);
wayland_client::delegate_noop!(Client: ignore XdgToplevel);
wayland_client::delegate_noop!(Client: ignore ZwpTextInputManagerV3);
wayland_client::delegate_noop!(Client: ignore ZwpInputMethodManagerV2);
wayland_client::delegate_noop!(Client: ignore ZwpVirtualKeyboardManagerV1);
wayland_client::delegate_noop!(Client: ignore ZwpVirtualKeyboardV1);

struct Peer {
    _conn: Connection,
    queue: EventQueue<Client>,
    registry: WlRegistry,
    client: Client,
}

impl Peer {
    fn qh(&self) -> QueueHandle<Client> {
        self.queue.handle()
    }

    fn bind<I, U>(&self, version: u32, data: U) -> I
    where
        I: wayland_client::Proxy + 'static,
        U: Send + Sync + 'static,
        Client: Dispatch<I, U>,
    {
        let (name, offered) = self.client.globals[I::interface().name];
        assert!(offered >= version, "{} {offered}", I::interface().name);
        self.registry.bind(name, version, &self.qh(), data)
    }

    /// A wl_seat bound at `version`; its keyboard records under it.
    fn seat(&self, version: u32) -> WlSeat {
        self.bind(version, version)
    }
}

/// GNOME's keyboard settings as the shell sends them.
fn settings(repeat: bool, delay: u32, interval: u32) -> tuna_shell_control::InputSettings {
    tuna_shell_control::InputSettings {
        repeat,
        repeat_delay_ms: delay,
        repeat_interval_ms: interval,
        ..Default::default()
    }
}

fn compositor() -> (TestCompositor, WindowManager) {
    let mut comp = TestCompositor::new();
    comp.state.set_output_size(1280, 800);
    let mut manager = comp.window_manager();
    manager.apply_keyboard_repeat(&settings(true, 500, 30));
    (comp, manager)
}

fn connect(comp: &mut TestCompositor, manager: &mut WindowManager, bridge: bool) -> Peer {
    let (server, stream) = UnixStream::pair().unwrap();
    if bridge {
        comp.add_ime_bridge_client(server);
    } else {
        comp.add_client(server);
    }
    let conn = Connection::from_socket(stream).unwrap();
    let queue = conn.new_event_queue();
    let registry = conn.display().get_registry(&queue.handle(), ());
    let mut peer = Peer {
        _conn: conn,
        queue,
        registry,
        client: Client::default(),
    };
    pump(comp, manager, &mut [&mut peer]);
    peer
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

struct MappedWindow {
    surface: WlSurface,
    xdg: XdgSurface,
    toplevel: XdgToplevel,
}

impl MappedWindow {
    fn destroy(self) {
        self.toplevel.destroy();
        self.xdg.destroy();
        self.surface.destroy();
    }
}

/// Map a toplevel, which takes keyboard focus.
fn window(comp: &mut TestCompositor, manager: &mut WindowManager, peer: &mut Peer) -> MappedWindow {
    let compositor: WlCompositor = peer.bind(6, ());
    let wm: XdgWmBase = peer.bind(6, ());
    let qh = peer.qh();
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title("typing".into());
    surface.commit();
    std::mem::forget((compositor, wm));
    pump(comp, manager, &mut [peer]);
    assert!(peer.client.entered, "the mapped window has keyboard focus");
    MappedWindow { surface, xdg, toplevel }
}

fn key(manager: &mut WindowManager, comp: &mut TestCompositor, keycode: u32, pressed: bool) {
    manager.on_input(
        &mut comp.state,
        ManagerInput::Key {
            keycode,
            pressed,
            time: PRESS_TIME,
        },
    );
}

/// Press `keycode` and return the repeat deadline it armed, checked to
/// lie `delay` after the press.
fn press_armed(
    manager: &mut WindowManager,
    comp: &mut TestCompositor,
    keycode: u32,
    delay: Duration,
) -> Instant {
    let before = Instant::now();
    key(manager, comp, keycode, true);
    let after = Instant::now();
    let due = manager.key_repeat_deadline().expect("a repeat is armed");
    assert!(
        due >= before + delay && due <= after + delay,
        "the first repeat comes `delay` after the press"
    );
    due
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

#[test]
fn version_10_keyboards_hear_compositor_repeats_and_version_9_repeat_themselves() {
    let (mut comp, mut manager) = compositor();
    let mut app = connect(&mut comp, &mut manager, false);
    assert_eq!(app.client.globals["wl_seat"].1, 10, "Mutter 51's wl_seat");
    let _v10 = app.seat(10);
    let _v9 = app.seat(9);
    window(&mut comp, &mut manager, &mut app);
    // META_WL_SEAT_VERSION 10: notify_key_repeat_for_resource.
    assert_eq!(app.client.repeat_info[&10], (0, 0));
    assert_eq!(app.client.repeat_info[&9], (1000 / 30, 500));

    let due = press_armed(&mut manager, &mut comp, KEY_A, ms(500));
    manager.run_key_repeat(&mut comp.state, due - ms(1), false);
    pump(&mut comp, &mut manager, &mut [&mut app]);
    assert!(
        app.client.repeated(10).is_empty(),
        "nothing before the delay"
    );
    manager.run_key_repeat(&mut comp.state, due, false);
    assert_eq!(manager.key_repeat_deadline(), Some(due + ms(30)));
    manager.run_key_repeat(&mut comp.state, due + ms(30), false);
    assert_eq!(manager.key_repeat_deadline(), Some(due + ms(60)));
    pump(&mut comp, &mut manager, &mut [&mut app]);

    let press = app.client.keys(10)[0];
    assert_eq!(
        (press.1, press.2, press.3),
        (PRESS_TIME, KEY_A, WEnum::Value(KeyState::Pressed))
    );
    let repeated = app.client.repeated(10);
    assert_eq!(
        repeated
            .iter()
            .map(|key| (key.1, key.2))
            .collect::<Vec<_>>(),
        [(PRESS_TIME + 500, KEY_A), (PRESS_TIME + 530, KEY_A)],
        "repeats at the configured delay and interval, in the press's time base"
    );
    assert!(
        press.0 < repeated[0].0 && repeated[0].0 < repeated[1].0,
        "every repeat takes a fresh serial"
    );
    assert_eq!(
        app.client.keys(9),
        [(press.0, PRESS_TIME, KEY_A, WEnum::Value(KeyState::Pressed))],
        "a version 9 keyboard repeats by itself and hears no repeats"
    );

    // Release stops the repeat.
    key(&mut manager, &mut comp, KEY_A, false);
    assert_eq!(manager.key_repeat_deadline(), None);
    manager.run_key_repeat(&mut comp.state, due + ms(1000), false);
    pump(&mut comp, &mut manager, &mut [&mut app]);
    assert_eq!(app.client.repeated(10).len(), 2);
    assert_eq!(
        app.client.keys(10).last().unwrap().3,
        WEnum::Value(KeyState::Released)
    );
}

#[test]
fn repeats_leave_xkb_holds_and_modifier_state_unchanged() {
    let (mut comp, mut manager) = compositor();
    let mut app = connect(&mut comp, &mut manager, false);
    let _seat = app.seat(10);
    window(&mut comp, &mut manager, &mut app);
    let keyboard = comp.test_seat().get_keyboard().unwrap();
    let due = press_armed(&mut manager, &mut comp, KEY_A, ms(500));
    manager.discard_key_input(&mut comp.state,
        &ManagerInput::Key { keycode: KEY_LEFTSHIFT, pressed: true, time: PRESS_TIME + 1 });
    let holds = keyboard.pressed_keys();
    let mods = keyboard.modifier_state().serialized;
    for n in 0..5 { manager.run_key_repeat(&mut comp.state, due + ms(n * 30), false); }
    assert_eq!(keyboard.pressed_keys(), holds);
    let after = keyboard.modifier_state().serialized;
    assert_eq!((mods.depressed, mods.latched, mods.locked, mods.layout_effective),
        (after.depressed, after.latched, after.locked, after.layout_effective));
    pump(&mut comp, &mut manager, &mut [&mut app]);
    assert_eq!(app.client.repeated(10).len(), 5);
    let first_repeat = app.client.wire_order.iter().position(|event| *event == (10, "repeat")).unwrap();
    assert_eq!(app.client.wire_order[first_repeat - 1], (10, "modifiers"),
        "a consumed modifier change precedes the repeated key");
    assert!(!app.client.keys(10).iter().any(|event| event.2 == KEY_LEFTSHIFT));
    assert!(app.client.modifiers.iter().any(|(v, depressed, _, _, _)| *v == 10 && *depressed != 0));
    key(&mut manager, &mut comp, KEY_A, false);
    manager.discard_key_input(&mut comp.state,
        &ManagerInput::Key { keycode: KEY_LEFTSHIFT, pressed: false, time: PRESS_TIME + 2 });
    assert!(keyboard.pressed_keys().is_empty());
}

#[test]
fn focus_leave_and_return_cannot_resurrect_a_held_repeat() {
    let (mut comp, mut manager) = compositor();
    let mut app = connect(&mut comp, &mut manager, false);
    let _seat = app.seat(10);
    window(&mut comp, &mut manager, &mut app);
    let keyboard = comp.test_seat().get_keyboard().unwrap();
    let focus = keyboard.current_focus();
    let due = press_armed(&mut manager, &mut comp, KEY_A, ms(500));
    keyboard.set_focus(&mut comp.state, None, smithay::utils::SERIAL_COUNTER.next_serial());
    keyboard.set_focus(&mut comp.state, focus, smithay::utils::SERIAL_COUNTER.next_serial());
    manager.run_key_repeat(&mut comp.state, due, false);
    pump(&mut comp, &mut manager, &mut [&mut app]);
    assert_eq!(manager.key_repeat_deadline(), None);
    assert!(app.client.repeated(10).is_empty());
    key(&mut manager, &mut comp, KEY_A, false);
}

#[test]
fn keymap_and_live_settings_retire_the_old_hold_and_update_both_versions() {
    let (mut comp, mut manager) = compositor();
    let mut app = connect(&mut comp, &mut manager, false);
    let _v10 = app.seat(10);
    let _v9 = app.seat(9);
    window(&mut comp, &mut manager, &mut app);
    let due = press_armed(&mut manager, &mut comp, KEY_A, ms(500));
    let mut changed = settings(true, 600, 40);
    changed.xkb_layout = \"de\".into();
    manager.apply_keyboard_settings(&mut comp.state, &changed);
    manager.run_key_repeat(&mut comp.state, due + ms(1000), false);
    pump(&mut comp, &mut manager, &mut [&mut app]);
    assert!(app.client.repeated(10).is_empty());
    assert_eq!(app.client.repeat_info[&10], (0, 0));
    assert_eq!(app.client.repeat_info[&9], (25, 600));
    assert!(app.client.keymaps[&10] >= 2 && app.client.keymaps[&9] >= 2);
    key(&mut manager, &mut comp, KEY_A, false);
    let due = press_armed(&mut manager, &mut comp, KEY_B, ms(600));
    // Changing cadence alone also retires the old hold.
    manager.apply_keyboard_repeat(&settings(true, 600, 45));
    manager.run_key_repeat(&mut comp.state, due, false);
    assert_eq!(manager.key_repeat_deadline(), None);
    key(&mut manager, &mut comp, KEY_B, false);
    manager.apply_keyboard_repeat(&settings(false, 600, 45));
    manager.apply_keyboard_repeat(&settings(true, 600, 45));
    manager.run_key_repeat(&mut comp.state, due + ms(1000), false);
    pump(&mut comp, &mut manager, &mut [&mut app]);
    assert!(app.client.repeated(10).is_empty());
}

#[test]
fn destroyed_focus_and_a_lock_boundary_cancel_pending_repeats() {
    let (mut comp, mut manager) = compositor();
    let mut app = connect(&mut comp, &mut manager, false);
    let _seat = app.seat(10);
    let surface = window(&mut comp, &mut manager, &mut app);
    let due = press_armed(&mut manager, &mut comp, KEY_A, ms(500));
    manager.cancel_key_repeat(); // Runtime lock transition's public hook.
    manager.run_key_repeat(&mut comp.state, due, true);
    assert_eq!(manager.key_repeat_deadline(), None);
    key(&mut manager, &mut comp, KEY_A, false);
    let due = press_armed(&mut manager, &mut comp, KEY_B, ms(500));
    surface.destroy();
    pump(&mut comp, &mut manager, &mut [&mut app]);
    manager.run_key_repeat(&mut comp.state, due, false);
    pump(&mut comp, &mut manager, &mut [&mut app]);
    assert!(app.client.repeated(10).is_empty());
    assert_eq!(manager.key_repeat_deadline(), None);
}

#[test]
fn another_key_takes_the_repeat_and_a_modifier_or_disabled_repeat_does_not() {
    let (mut comp, mut manager) = compositor();
    let mut app = connect(&mut comp, &mut manager, false);
    let _v10 = app.seat(10);
    window(&mut comp, &mut manager, &mut app);

    let due_a = press_armed(&mut manager, &mut comp, KEY_A, ms(500));
    // Shift does not repeat (xkb_keymap_key_repeats): A keeps going.
    key(&mut manager, &mut comp, KEY_LEFTSHIFT, true);
    assert_eq!(manager.key_repeat_deadline(), Some(due_a));
    key(&mut manager, &mut comp, KEY_LEFTSHIFT, false);
    // B takes over: A is never repeated again, B after its own delay.
    let due_b = press_armed(&mut manager, &mut comp, KEY_B, ms(500));
    manager.run_key_repeat(&mut comp.state, due_b, false);
    // A's release no longer ends B's repeat.
    key(&mut manager, &mut comp, KEY_A, false);
    manager.run_key_repeat(&mut comp.state, due_b + ms(30), false);
    key(&mut manager, &mut comp, KEY_B, false);
    pump(&mut comp, &mut manager, &mut [&mut app]);
    assert_eq!(
        app.client
            .repeated(10)
            .iter()
            .map(|key| key.2)
            .collect::<Vec<_>>(),
        [KEY_B, KEY_B]
    );

    // A modifier alone never repeats.
    key(&mut manager, &mut comp, KEY_LEFTSHIFT, true);
    assert_eq!(manager.key_repeat_deadline(), None);
    key(&mut manager, &mut comp, KEY_LEFTSHIFT, false);

    // org.gnome.desktop.peripherals.keyboard repeat=false: none at all.
    manager.apply_keyboard_repeat(&settings(false, 500, 30));
    key(&mut manager, &mut comp, KEY_A, true);
    assert_eq!(manager.key_repeat_deadline(), None);
    manager.run_key_repeat(&mut comp.state, Instant::now() + ms(5000), false);
    key(&mut manager, &mut comp, KEY_A, false);
    pump(&mut comp, &mut manager, &mut [&mut app]);
    assert_eq!(app.client.repeated(10).len(), 2);
    assert_eq!(app.client.repeat_info[&10], (0, 0));
}

#[test]
fn a_held_accelerator_repeats_unless_it_ignores_autorepeat() {
    use tuna_shell_control::{Accelerator, AcceleratorGrab, GRAB_IGNORE_AUTOREPEAT, MODE_NORMAL};
    let grab = |action, flags| AcceleratorGrab {
        accelerator: Accelerator {
            action,
            keysym: u32::from(b'r'),
            mods: 0,
            modes: MODE_NORMAL,
        },
        flags,
    };
    let (mut comp, mut manager) = compositor();
    let mut app = connect(&mut comp, &mut manager, false);
    let _v10 = app.seat(10);
    window(&mut comp, &mut manager, &mut app);

    manager.set_accelerator_grabs(vec![grab(7, 0)]);
    let due = press_armed(&mut manager, &mut comp, KEY_R, ms(500));
    assert_eq!(
        manager.take_accelerators_fired(),
        [(7, PRESS_TIME, MODE_NORMAL)]
    );
    manager.run_key_repeat(&mut comp.state, due, false);
    assert_eq!(
        manager.take_accelerators_fired(),
        [(7, PRESS_TIME + 500, MODE_NORMAL)],
        "Mutter's keybindings hear repeated events"
    );
    key(&mut manager, &mut comp, KEY_R, false);

    manager.set_accelerator_grabs(vec![grab(8, GRAB_IGNORE_AUTOREPEAT)]);
    let due = press_armed(&mut manager, &mut comp, KEY_R, ms(500));
    assert_eq!(
        manager.take_accelerators_fired(),
        [(8, PRESS_TIME, MODE_NORMAL)]
    );
    manager.run_key_repeat(&mut comp.state, due, false);
    assert!(manager.take_accelerators_fired().is_empty());
    key(&mut manager, &mut comp, KEY_R, false);
    pump(&mut comp, &mut manager, &mut [&mut app]);
    assert!(
        app.client.keys(10).is_empty(),
        "the grabbed key and its repeats never reach the window"
    );
}

/// An IM-consumed press is never repeated to the application. A declined
/// original edge admits only that physical hold to compositor repetition;
/// IM-grab wire itself remains press/release at every seat version.
#[test]
fn the_ibus_bridge_returns_only_original_edges_and_compositor_repeats_the_admitted_hold() {
    let (mut comp, mut manager) = compositor();
    let mut app = connect(&mut comp, &mut manager, false);
    let mut bridge = connect(&mut comp, &mut manager, true);
    let bridge_seat = bridge.seat(10);
    let im = bridge
        .bind::<ZwpInputMethodManagerV2, _>(1, ())
        .get_input_method(&bridge_seat, &bridge.qh(), ());
    let vk = bridge
        .bind::<ZwpVirtualKeyboardManagerV1, _>(1, ())
        .create_virtual_keyboard(&bridge_seat, &bridge.qh(), ());
    let _v10 = app.seat(10);
    let v9 = app.seat(9);
    let _text_input =
        app.bind::<ZwpTextInputManagerV3, _>(1, ())
            .get_text_input(&v9, &app.qh(), ());
    window(&mut comp, &mut manager, &mut app);
    pump(&mut comp, &mut manager, &mut [&mut app, &mut bridge]);
    assert!(bridge.client.ime_active, "the text field activated the IM");
    let _grab = im.grab_keyboard(&bridge.qh(), ());
    pump(&mut comp, &mut manager, &mut [&mut app, &mut bridge]);

    let due = press_armed(&mut manager, &mut comp, KEY_A, ms(500));
    manager.run_key_repeat(&mut comp.state, due, false);
    pump(&mut comp, &mut manager, &mut [&mut app, &mut bridge]);
    assert_eq!(bridge.client.grab_keys,
        [(PRESS_TIME, KEY_A, WEnum::Value(KeyState::Pressed))]);
    assert!(
        app.client.keys(10).is_empty(),
        "the grab holds the keys until IBus passes them on"
    );

    // Only the original decline is returned; synthetic declines never VK-forward.
    vk.key(PRESS_TIME, KEY_A, 1);
    vk.key(PRESS_TIME + 1, KEY_A, 2); // No private Repeated VK extension.
    pump(&mut comp, &mut manager, &mut [&mut app, &mut bridge]);
    manager.run_key_repeat(&mut comp.state, due + ms(30), false);
    pump(&mut comp, &mut manager, &mut [&mut app, &mut bridge]);
    let states = |version| {
        app.client
            .keys(version)
            .iter()
            .map(|key| (key.1, key.3))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        states(10),
        [
            (PRESS_TIME, WEnum::Value(KeyState::Pressed)),
            (PRESS_TIME + 530, WEnum::Value(KeyState::Repeated)),
        ]
    );
    assert_eq!(
        states(9),
        [(PRESS_TIME, WEnum::Value(KeyState::Pressed))],
        "a version 9 keyboard repeats by itself"
    );
    assert_eq!(bridge.client.grab_keys.len(), 1, "never Repeated on the IM wire");
    key(&mut manager, &mut comp, KEY_A, false);
    vk.key(PRESS_TIME + 600, KEY_A, 0);
    pump(&mut comp, &mut manager, &mut [&mut app, &mut bridge]);
    manager.run_key_repeat(&mut comp.state, due + ms(3000), false);
    assert_eq!(manager.key_repeat_deadline(), None);
    assert_eq!(app.client.repeated(10).len(), 1);
    // An OSK-generated key without a physical hold cannot acquire this clock.
    vk.key(PRESS_TIME + 700, KEY_B, 1);
    pump(&mut comp, &mut manager, &mut [&mut app, &mut bridge]);
    let keyboard = comp.test_seat().get_keyboard().unwrap();
    keyboard.input_repeat(&mut comp.state, (KEY_B + 8).into(),
        smithay::utils::SERIAL_COUNTER.next_serial(), PRESS_TIME + 800);
    pump(&mut comp, &mut manager, &mut [&mut app, &mut bridge]);
    assert_eq!(app.client.repeated(10).len(), 1);
    vk.key(PRESS_TIME + 900, KEY_B, 0);
}

/// Any other input method repeats by itself from its grab's repeat_info
/// (the settings, as for a keyboard before version 10): the compositor
/// sends its grab no repeats, and nothing leaks past it to the window.
#[test]
fn another_input_method_repeats_by_itself() {
    let (mut comp, mut manager) = compositor();
    let mut app = connect(&mut comp, &mut manager, false);
    let mut ime = connect(&mut comp, &mut manager, false);
    let ime_seat = ime.seat(10);
    let im = ime
        .bind::<ZwpInputMethodManagerV2, _>(1, ())
        .get_input_method(&ime_seat, &ime.qh(), ());
    let v10 = app.seat(10);
    let _text_input =
        app.bind::<ZwpTextInputManagerV3, _>(1, ())
            .get_text_input(&v10, &app.qh(), ());
    window(&mut comp, &mut manager, &mut app);
    pump(&mut comp, &mut manager, &mut [&mut app, &mut ime]);
    assert!(ime.client.ime_active);
    let _grab = im.grab_keyboard(&ime.qh(), ());
    pump(&mut comp, &mut manager, &mut [&mut app, &mut ime]);
    assert_eq!(ime.client.grab_repeat_info, Some((1000 / 30, 500)));

    let due = press_armed(&mut manager, &mut comp, KEY_A, ms(500));
    manager.run_key_repeat(&mut comp.state, due, false);
    pump(&mut comp, &mut manager, &mut [&mut app, &mut ime]);
    assert_eq!(
        ime.client.grab_keys,
        [(PRESS_TIME, KEY_A, WEnum::Value(KeyState::Pressed))]
    );
    assert!(app.client.keys(10).is_empty());
    // The grab always gets real configured values, not the v10 zeroes.
    manager.apply_keyboard_repeat(&settings(true, 700, 40));
    pump(&mut comp, &mut manager, &mut [&mut app, &mut ime]);
    assert_eq!(ime.client.grab_repeat_info, Some((25, 700)));
    assert_eq!(app.client.repeat_info[&10], (0, 0));
    manager.run_key_repeat(&mut comp.state, due + ms(30), false);
    assert_eq!(manager.key_repeat_deadline(), None);
    let keymaps = ime.client.grab_keymaps;
    let mut changed = settings(true, 700, 40);
    changed.xkb_layout = "de".into();
    manager.apply_keyboard_settings(&mut comp.state, &changed);
    pump(&mut comp, &mut manager, &mut [&mut app, &mut ime]);
    assert!(ime.client.grab_keymaps > keymaps, "live keymap retires the bridge's held repeat");
    key(&mut manager, &mut comp, KEY_A, false);
    let replacement = im.grab_keyboard(&ime.qh(), ());
    pump(&mut comp, &mut manager, &mut [&mut app, &mut ime]);
    _grab.release();
    pump(&mut comp, &mut manager, &mut [&mut app, &mut ime]);
    let before = ime.client.grab_keys.len();
    let due = press_armed(&mut manager, &mut comp, KEY_A, ms(700));
    manager.run_key_repeat(&mut comp.state, due, false);
    pump(&mut comp, &mut manager, &mut [&mut app, &mut ime]);
    assert_eq!(ime.client.grab_keys.len(), before + 1, "old resource teardown preserves its replacement");
    replacement.release();
    pump(&mut comp, &mut manager, &mut [&mut app, &mut ime]);
    manager.run_key_repeat(&mut comp.state, due + ms(40), false);
    assert_eq!(manager.key_repeat_deadline(), None);
    assert!(app.client.keys(10).is_empty(), "a destroyed grab cannot leak its consumed hold");
}
