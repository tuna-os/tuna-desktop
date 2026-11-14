//! Real Wayland sockets exercise the compositor's admitted virtual-input route.
//! This is a protocol consumer test, not an IBus engine or app-consumption proof.

use std::{
    io::Write,
    os::{fd::AsFd, unix::net::UnixStream},
    sync::Arc,
};

use smithay::{
    reexports::wayland_server::protocol::wl_surface::WlSurface as ServerSurface,
    utils::SERIAL_COUNTER,
};
use wayland_client::{
    protocol::{
        wl_callback::WlCallback,
        wl_compositor::WlCompositor,
        wl_keyboard::{self, WlKeyboard},
        wl_registry::{self, WlRegistry},
        wl_seat::WlSeat,
        wl_surface::WlSurface,
    },
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum,
};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};

use crate::{windows::ManagerInput, ClientState, TestCompositor};

#[derive(Default)]
struct Consumer {
    compositor: Option<WlCompositor>,
    surface: Option<WlSurface>,
    seat: Option<WlSeat>,
    keyboard: Option<WlKeyboard>,
    virtual_manager: Option<ZwpVirtualKeyboardManagerV1>,
    keys: Vec<(u32, bool)>,
    synced: bool,
}

impl Dispatch<WlRegistry, ()> for Consumer {
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
                    let compositor =
                        registry.bind::<WlCompositor, _, _>(name, version.min(6), qh, ());
                    state.surface = Some(compositor.create_surface(qh, ()));
                    state.compositor = Some(compositor);
                }
                "wl_seat" => {
                    let seat = registry.bind::<WlSeat, _, _>(name, version, qh, ());
                    state.keyboard = Some(seat.get_keyboard(qh, ()));
                    state.seat = Some(seat);
                }
                "zwp_virtual_keyboard_manager_v1" => {
                    state.virtual_manager =
                        Some(registry.bind::<ZwpVirtualKeyboardManagerV1, _, _>(name, 1, qh, ()));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<WlCallback, ()> for Consumer {
    fn event(
        state: &mut Self,
        _: &WlCallback,
        _: wayland_client::protocol::wl_callback::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        state.synced = true;
    }
}

impl Dispatch<WlKeyboard, ()> for Consumer {
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
            state
                .keys
                .push((key, pressed == WEnum::Value(wl_keyboard::KeyState::Pressed)));
        }
    }
}

wayland_client::delegate_noop!(Consumer: ignore WlCompositor);
wayland_client::delegate_noop!(Consumer: ignore WlSurface);
wayland_client::delegate_noop!(Consumer: ignore WlSeat);
wayland_client::delegate_noop!(Consumer: ignore ZwpVirtualKeyboardManagerV1);
wayland_client::delegate_noop!(Consumer: ignore ZwpVirtualKeyboardV1);

struct Peer {
    owner: smithay::reexports::wayland_server::Client,
    conn: Connection,
    queue: EventQueue<Consumer>,
    consumer: Consumer,
}

impl Peer {
    fn connect(comp: &mut TestCompositor, bridge: bool) -> Self {
        Self::connect_with_data(comp, |server| {
            if bridge {
                ClientState::ime_bridge()
            } else {
                ClientState::native_connection(server)
            }
        })
    }

    fn connect_with_data(
        comp: &mut TestCompositor,
        data: impl FnOnce(&UnixStream) -> ClientState,
    ) -> Self {
        let (server, socket) = UnixStream::pair().unwrap();
        let data = data(&server);
        let owner = comp
            .display
            .handle()
            .insert_client(server, Arc::new(data))
            .unwrap();
        let conn = Connection::from_socket(socket).unwrap();
        let queue = conn.new_event_queue();
        conn.display().get_registry(&queue.handle(), ());
        let mut peer = Self {
            owner,
            conn,
            queue,
            consumer: Consumer::default(),
        };
        peer.sync(comp);
        peer.sync(comp);
        peer
    }

    fn sync(&mut self, comp: &mut TestCompositor) {
        self.consumer.synced = false;
        self.conn.display().sync(&self.queue.handle(), ());
        for _ in 0..200 {
            self.queue.flush().unwrap();
            comp.pump();
            if let Some(read) = self.queue.prepare_read() {
                read.read().unwrap();
            }
            self.queue.dispatch_pending(&mut self.consumer).unwrap();
            if self.consumer.synced {
                return;
            }
        }
        panic!("virtual keyboard consumer sync exhausted");
    }

    fn surface(&self, comp: &TestCompositor) -> ServerSurface {
        self.owner
            .object_from_protocol_id(
                &comp.display.handle(),
                self.consumer.surface.as_ref().unwrap().id().protocol_id(),
            )
            .unwrap()
    }
}

#[test]
fn keyboard_delivery_observer_counts_real_virtual_socket_sends_to_actual_focus_only() {
    let mut comp = TestCompositor::new();
    let mut manager = comp.window_manager();
    let keyboard = comp.test_seat().get_keyboard().unwrap();
    keyboard.enable_delivery_observer();
    let mut app = Peer::connect(&mut comp, false);
    let mut shield = Peer::connect(&mut comp, false);
    let mut bridge = Peer::connect(&mut comp, true);
    // Ordinary socket clients cannot even bind this privileged input global.
    assert!(app.consumer.virtual_manager.is_none());
    assert!(shield.consumer.virtual_manager.is_none());
    let virtual_keyboard = bridge
        .consumer
        .virtual_manager
        .as_ref()
        .unwrap()
        .create_virtual_keyboard(
            bridge.consumer.seat.as_ref().unwrap(),
            &bridge.queue.handle(),
            (),
        );
    let context = xkbcommon::xkb::Context::new(xkbcommon::xkb::CONTEXT_NO_FLAGS);
    let keymap = xkbcommon::xkb::Keymap::new_from_names(
        &context,
        "",
        "",
        "us",
        "",
        None,
        xkbcommon::xkb::KEYMAP_COMPILE_NO_FLAGS,
    )
    .unwrap();
    let mut keymap_file = tempfile::tempfile().unwrap();
    let mut keymap_bytes = keymap
        .get_as_string(xkbcommon::xkb::KEYMAP_FORMAT_TEXT_V1)
        .into_bytes();
    keymap_bytes.push(0);
    keymap_file.write_all(&keymap_bytes).unwrap();
    virtual_keyboard.keymap(
        1,
        keymap_file.as_fd(),
        keymap_bytes.len().try_into().unwrap(),
    );
    bridge.sync(&mut comp);
    let app_surface = app.surface(&comp);
    let shield_surface = shield.surface(&comp);
    keyboard.set_focus(
        &mut comp.state,
        Some(app_surface.clone()),
        SERIAL_COUNTER.next_serial(),
    );
    let baseline_app = keyboard.delivery_snapshot(&app.owner).unwrap();
    let baseline_shield = keyboard.delivery_snapshot(&shield.owner).unwrap();
    for pressed in [1, 0] {
        virtual_keyboard.key(100, 30, pressed);
    }
    bridge.sync(&mut comp);
    app.sync(&mut comp);
    shield.sync(&mut comp);
    assert_eq!(&app.consumer.keys[..], &[(30, true), (30, false)]);
    assert!(shield.consumer.keys.is_empty());
    let positive = keyboard.delivery_snapshot(&app.owner).unwrap();
    assert_eq!(
        positive.resources[0].emitted_key_events,
        baseline_app.resources[0].emitted_key_events + 2
    );
    assert_eq!(
        keyboard.delivery_snapshot(&shield.owner).unwrap(),
        baseline_shield
    );
    assert_eq!(keyboard.current_focus(), Some(app_surface));
    manager.lock_input(
        &mut comp.state,
        &shield_surface,
        ManagerInput::Motion {
            pos: (0.0, 0.0).into(),
            time: 200,
        },
    );
    for pressed in [1, 0] {
        virtual_keyboard.key(200, 30, pressed);
    }
    bridge.sync(&mut comp);
    app.sync(&mut comp);
    shield.sync(&mut comp);
    assert_eq!(keyboard.current_focus(), Some(shield_surface));
    assert_eq!(keyboard.delivery_snapshot(&app.owner).unwrap(), positive);
    assert_eq!(&app.consumer.keys[..], &[(30, true), (30, false)]);
    assert_eq!(&shield.consumer.keys[..], &[(30, true), (30, false)]);
    assert_eq!(
        keyboard.delivery_snapshot(&shield.owner).unwrap().resources[0].emitted_key_events,
        baseline_shield.resources[0].emitted_key_events + 2
    );
    // Also exercise the owner-only snapshot's actual seat-focus credentials.
    let mut doc = serde_json::json!({});
    super::publish(&manager, &comp.state, &mut doc);
    assert_eq!(
        doc["native_keyboard_focus"]["client_pid"],
        std::process::id()
    );
    assert_eq!(doc["native_keyboard_focus"]["client_uid"], unsafe {
        libc::getuid()
    });
    assert_eq!(doc["keyboard_delivery_observer"]["evaluable"], true);
}

#[test]
fn observer_service_focus_uses_live_original_caller_not_socketpair_creator() {
    let mut child = std::process::Command::new("/bin/sleep")
        .arg("60")
        .spawn()
        .unwrap();
    let pid = child.id();
    let descriptor = rustix::process::pidfd_open(
        rustix::process::Pid::from_raw(pid as i32).unwrap(),
        rustix::process::PidfdFlags::empty(),
    )
    .unwrap();
    let uid = rustix::process::getuid().as_raw();
    let original = zbus::fdo::ConnectionCredentials::default()
        .set_process_id(pid)
        .set_unix_user_id(uid)
        .set_process_fd(descriptor.into());
    let mut comp = TestCompositor::new();
    let manager = comp.window_manager();
    let keyboard = comp.test_seat().get_keyboard().unwrap();
    keyboard.enable_delivery_observer();
    let app = Peer::connect_with_data(&mut comp, |_| {
        let mut data = ClientState::service_connection(
            Arc::new(std::sync::atomic::AtomicBool::new(true)),
            None,
        );
        data.original_credentials = Some(Arc::new(original));
        data
    });
    let surface = app.surface(&comp);
    keyboard.set_focus(&mut comp.state, Some(surface), SERIAL_COUNTER.next_serial());
    let mut doc = serde_json::json!({});
    super::publish(&manager, &comp.state, &mut doc);
    let socket_creator = app
        .owner
        .get_credentials(&comp.display.handle())
        .unwrap()
        .pid;
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(socket_creator, std::process::id() as i32);
    assert_ne!(pid, std::process::id());
    assert_eq!(doc["native_keyboard_focus"]["client_pid"], pid);
    assert_eq!(doc["native_keyboard_focus"]["client_uid"], uid);
    assert_eq!(doc["native_keyboard_focus"]["evaluable"], true);

    // Retaining the connection or reusing a numeric PID cannot revive its pin.
    super::publish(&manager, &comp.state, &mut doc);
    assert_eq!(doc["native_keyboard_focus"]["evaluable"], false);
    assert!(doc["native_keyboard_focus"]["client_pid"].is_null());
    assert!(doc["native_keyboard_focus"]["client_uid"].is_null());
    assert_eq!(
        doc["native_keyboard_focus"]["error"],
        "original-process-pin-unavailable"
    );
}

#[test]
fn observer_missing_original_credentials_or_pin_never_uses_socketpair_creator() {
    let descriptor = rustix::process::pidfd_open(
        rustix::process::Pid::from_raw(std::process::id() as i32).unwrap(),
        rustix::process::PidfdFlags::empty(),
    )
    .unwrap();
    for original in [
        None,
        Some(
            zbus::fdo::ConnectionCredentials::default()
                .set_process_id(std::process::id())
                .set_unix_user_id(rustix::process::getuid().as_raw()),
        ),
        Some(
            zbus::fdo::ConnectionCredentials::default()
                .set_process_id(std::process::id())
                .set_process_fd(descriptor.into()),
        ),
    ] {
        let mut comp = TestCompositor::new();
        let manager = comp.window_manager();
        let keyboard = comp.test_seat().get_keyboard().unwrap();
        keyboard.enable_delivery_observer();
        let app = Peer::connect_with_data(&mut comp, |_| {
            let mut data = ClientState::service_connection(
                Arc::new(std::sync::atomic::AtomicBool::new(true)),
                None,
            );
            data.original_credentials = original.map(Arc::new);
            data
        });
        let surface = app.surface(&comp);
        keyboard.set_focus(&mut comp.state, Some(surface), SERIAL_COUNTER.next_serial());
        let mut doc = serde_json::json!({});
        super::publish(&manager, &comp.state, &mut doc);
        assert_eq!(doc["native_keyboard_focus"]["evaluable"], false);
        assert!(doc["native_keyboard_focus"]["client_pid"].is_null());
        assert!(doc["native_keyboard_focus"]["client_uid"].is_null());
        assert_ne!(
            doc["native_keyboard_focus"]["error"],
            serde_json::Value::Null
        );
    }
}
