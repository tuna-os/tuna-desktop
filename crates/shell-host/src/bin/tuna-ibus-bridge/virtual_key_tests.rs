//! Fault boundaries, not a composition substitute: real GDBus calls fail at
//! different stages. Real libpinyin/GTK composition is qualified separately.
use super::*;
use std::{
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
    sync::{mpsc, Arc, Mutex},
};

const XML: &str = r#"<node><interface name="org.freedesktop.IBus.InputContext">
<method name="GetEngine"><arg type="v" direction="out"/></method>
<method name="ProcessKeyEvent"><arg type="u" direction="in"/><arg type="u" direction="in"/>
<arg type="u" direction="in"/><arg type="b" direction="out"/></method>
</interface></node>"#;
const UNICODE: u32 = 0x0100_4f60;

struct FaultContext {
    daemon: Child,
    main_loop: glib::MainLoop,
    thread: Option<std::thread::JoinHandle<()>>,
    context: IbusContext,
    calls: Arc<Mutex<Vec<(u32, u32, u32)>>>,
}
impl FaultContext {
    fn start() -> Self {
        let mut daemon = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("private D-Bus daemon");
        let mut address = String::new();
        BufReader::new(daemon.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        let address = address.trim().to_owned();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let recorded = calls.clone();
        let server_address = address.clone();
        let (ready, started) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            let main = glib::MainContext::new();
            main.with_thread_default(|| {
                let bus = connect(&server_address);
                let interface = gio::DBusNodeInfo::for_xml(XML)
                    .unwrap()
                    .lookup_interface(CONTEXT)
                    .unwrap();
                let object = bus
                    .register_object("/context", &interface)
                    .method_call(move |_, _, _, _, method, parameters, invocation| {
                        if method == "GetEngine" {
                            invocation.return_dbus_error(
                                "org.freedesktop.IBus.Error.Failed",
                                "lookup failed",
                            );
                            return;
                        }
                        let (keysym, code, state) = parameters.get::<(u32, u32, u32)>().unwrap();
                        recorded.lock().unwrap().push((keysym, code, state));
                        if keysym == u32::from('z') {
                            // A real delayed D-Bus reply exposes generation
                            // races; no replacement composition engine here.
                            glib::MainContext::ref_thread_default().spawn_local(async move {
                                glib::timeout_future(Duration::from_millis(250)).await;
                                invocation.return_value(Some(&(true,).to_variant()));
                            });
                            return;
                        }
                        if keysym == u32::from('b') || (keysym == UNICODE && state == RELEASE) {
                            invocation.return_dbus_error(
                                "org.freedesktop.IBus.Error.Failed",
                                "key stage failed",
                            );
                        } else {
                            let handled = keysym != u32::from('a')
                                && !(keysym == u32::from('d') && state == RELEASE);
                            invocation.return_value(Some(&(handled,).to_variant()));
                        }
                    })
                    .build()
                    .unwrap();
                let reply = bus
                    .call_sync(
                        Some("org.freedesktop.DBus"),
                        "/org/freedesktop/DBus",
                        "org.freedesktop.DBus",
                        "RequestName",
                        Some(&(IBUS, 4u32).to_variant()),
                        glib::VariantTy::new("(u)").ok(),
                        gio::DBusCallFlags::NONE,
                        CALL_TIMEOUT_MS,
                        None::<&gio::Cancellable>,
                    )
                    .unwrap();
                assert_eq!(reply.get::<(u32,)>(), Some((1,)));
                let main_loop = glib::MainLoop::new(Some(&main), false);
                ready.send(main_loop.clone()).unwrap();
                main_loop.run();
                bus.unregister_object(object).unwrap();
            })
            .unwrap();
        });
        let main_loop = started.recv_timeout(Duration::from_secs(10)).unwrap();
        let bus = connect(&address);
        let subscription = bus.subscribe_to_signal(
            None,
            Some(CONTEXT),
            None,
            Some("/context"),
            None,
            gio::DBusSignalFlags::NONE,
            |_| {},
        );
        let context = IbusContext {
            bus,
            path: "/context".into(),
            _subscription: subscription,
            incoming: Rc::default(),
        };
        Self {
            daemon,
            main_loop,
            thread: Some(thread),
            context,
            calls,
        }
    }
}
fn connect(address: &str) -> gio::DBusConnection {
    gio::DBusConnection::for_address_sync(
        address,
        gio::DBusConnectionFlags::AUTHENTICATION_CLIENT
            | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
        None,
        None::<&gio::Cancellable>,
    )
    .unwrap()
}
impl Drop for FaultContext {
    fn drop(&mut self) {
        self.main_loop.quit();
        self.thread.take().unwrap().join().unwrap();
        self.daemon.kill().unwrap();
        self.daemon.wait().unwrap();
    }
}
#[test]
fn dbus_press_errors_forward_once_but_consumed_release_errors_never_replay() {
    let fixture = FaultContext::start();
    assert_eq!(
        fixture.context.engine_requires_ibus(),
        None,
        "lookup failure is not a confirmed XKB source"
    );
    assert_eq!(
        fixture.context.virtual_key(u32::from('a')),
        VirtualKeyOutcome::Declined
    );
    assert_eq!(
        fixture.context.virtual_key(u32::from('b')),
        VirtualKeyOutcome::Unavailable
    );
    assert_eq!(
        fixture.context.virtual_key(UNICODE),
        VirtualKeyOutcome::ReleaseFailed
    );
    assert_eq!(
        fixture.context.virtual_key(u32::from('d')),
        VirtualKeyOutcome::Consumed
    );
    assert_eq!(
        fixture.context.virtual_key(u32::from('e')),
        VirtualKeyOutcome::Consumed
    );
    assert_eq!(*fixture.calls.lock().unwrap(), vec![
        (u32::from('a'), 0, 0), (u32::from('b'), 0, 0),
        (UNICODE, 0, 0), (UNICODE, 0, RELEASE),
        (u32::from('d'), 0, 0), (u32::from('d'), 0, RELEASE),
        (u32::from('e'), 0, 0), (u32::from('e'), 0, RELEASE),
    ], "declined/error presses have no release; consumed Unicode is exact and has no physical keycode");
}

#[test]
fn retired_repeat_reply_cannot_complete_the_new_held_key() {
    let fixture = FaultContext::start();
    let mut repeat = KeyRepeat::default();
    let now = Instant::now();
    repeat.info(20, 0);
    repeat.key(14, true, true, true, now);
    repeat.request(&fixture.context, u32::from('a'), 0, now);
    let until = Instant::now() + Duration::from_secs(2);
    while !fixture
        .calls
        .lock()
        .unwrap()
        .iter()
        .any(|call| call.0 == u32::from('a'))
        && Instant::now() < until
    {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(fixture
        .calls
        .lock()
        .unwrap()
        .iter()
        .any(|call| call.0 == u32::from('a')));
    repeat.key(14, false, true, true, now);
    repeat.key(21, true, true, true, now);
    repeat.request(&fixture.context, u32::from('z'), 0, now);
    let pump = |duration| {
        let until = Instant::now() + duration;
        let main = glib::MainContext::default();
        while Instant::now() < until {
            while main.pending() {
                main.iteration(false);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    };
    pump(Duration::from_millis(100));
    assert_eq!(
        repeat.due(now + Duration::from_secs(1)),
        None,
        "the retired key's reply cannot release the new key's in-flight guard"
    );
    pump(Duration::from_millis(250));
    assert_eq!(repeat.due(now + Duration::from_secs(1)), Some(21));
    repeat.cancel();
    assert_eq!(repeat.due(now + Duration::from_secs(2)), None);
}

#[test]
fn physical_grab_uses_xkb_repeatability_not_keysym_guesses() {
    let fixture = FaultContext::start();
    let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
    let keymap = xkb::Keymap::new_from_names(
        &context,
        "evdev",
        "pc105",
        "us",
        "",
        None,
        xkb::KEYMAP_COMPILE_NO_FLAGS,
    )
    .unwrap();
    let state = xkb::State::new(&keymap);
    let bus = fixture.context.bus.clone();
    let subscription = bus.subscribe_to_signal(
        None,
        Some(CONTEXT),
        None,
        Some("/context"),
        None,
        gio::DBusSignalFlags::NONE,
        |_| {},
    );
    let input_context = IbusContext {
        bus,
        path: "/context".into(),
        _subscription: subscription,
        incoming: Rc::default(),
    };
    let mut bridge = Bridge {
        active: true,
        context: Some(input_context),
        xkb: Some((keymap, state)),
        ..Default::default()
    };
    bridge.repeat.info(20, 0);
    bridge.key(1, 42, true); // Shift is nonrepeating in the actual keymap.
    assert_eq!(
        bridge.repeat.due(Instant::now() + Duration::from_secs(1)),
        None
    );
    bridge.key(2, 42, false);
    bridge.key(3, 14, true);
    assert_eq!(
        bridge.repeat.due(Instant::now() + Duration::from_secs(1)),
        Some(14)
    );
    bridge.key(4, 14, false);
    assert_eq!(
        bridge.repeat.due(Instant::now() + Duration::from_secs(1)),
        None
    );
}
