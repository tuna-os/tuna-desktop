//! Private-bus coverage of org.gnome.Mutter.InputCapture (#1040): who
//! may create sessions, and one session as the portal drives it: zones,
//! barriers, enable, the capture reaching an EIS receiver, release with
//! a warp, lock, and close.
use super::input_capture::{
    CaptureSession, CaptureShared, InputCapture, Router, SessionSignals, Zones,
};
use super::*;
use crate::windows::ManagerInput;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use zbus::Connection;

const IC: &str = "org.gnome.Mutter.InputCapture";
const SESSION: &str = "org.gnome.Mutter.InputCapture.Session";
const PATH: &str = "/org/gnome/Mutter/InputCapture/Session/u1";

struct Bus(Child, String);
impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn bus() -> Bus {
    let mut child = Command::new("dbus-daemon")
        .args(["--session", "--nofork", "--print-address=1"])
        .stdout(Stdio::piped())
        .spawn()
        .expect("private test bus starts");
    let mut address = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut address)
        .unwrap();
    Bus(child, address.trim().to_owned())
}
fn connect(bus: &Bus) -> Connection {
    zbus::block_on(
        zbus::connection::Builder::address(bus.1.as_str())
            .method_timeout(Duration::from_secs(5))
            .build(),
    )
    .unwrap()
}
fn session<B>(conn: &Connection, method: &str, body: &B) -> zbus::Result<zbus::Message>
where
    B: serde::Serialize + zbus::DynamicType,
{
    zbus::block_on(conn.call_method(Some(IC), PATH, Some(SESSION), method, body))
}
/// zbus 6 reports fdo-named error replies as `Error::FDO`, any other
/// name as `Error::MethodError`.
#[track_caller]
fn assert_error(result: zbus::Result<zbus::Message>, name: &str) {
    let expected = format!("org.freedesktop.DBus.Error.{name}");
    match result {
        Err(zbus::Error::MethodError(error, _, _)) => {
            assert_eq!(error.as_str(), expected);
        }
        Err(zbus::Error::FDO(error)) => {
            assert_eq!(zbus::DBusError::name(&*error).as_str(), expected);
        }
        other => panic!("expected {name}, got {other:?}"),
    }
}
/// Every session signal `conn` receives, as (member, message).
fn signals(conn: &Connection) -> std::sync::mpsc::Receiver<(String, zbus::Message)> {
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface(SESSION)
        .build()
        .unwrap();
    let mut stream = zbus::block_on(zbus::MessageStream::for_match_rule(
        rule,
        conn,
        std::num::NonZeroUsize::new(64),
    ))
    .unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use futures_lite::StreamExt as _;
        zbus::block_on(async {
            while let Some(message) = stream.next().await {
                let Ok(message) = message else {
                    continue;
                };
                let member = message
                    .header()
                    .member()
                    .map(|m| m.to_string())
                    .unwrap_or_default();
                if tx.send((member, message)).is_err() {
                    return;
                }
            }
        })
    });
    rx
}
fn monitor(connector: &str, x: i32) -> OutputSnapshot {
    OutputSnapshot {
        connector: connector.into(),
        make: String::new(),
        model: String::new(),
        width: 1920,
        height: 1080,
        refresh_mhz: 60_000,
        x,
        y: 0,
        scale: 1.0,
        primary: x == 0,
        transform: 0,
    }
}

#[test]
fn only_the_installed_portal_may_create_input_capture_sessions() {
    let bus = bus();
    let (to_loop, _from_dbus) = calloop::channel::channel();
    let authority = crate::capture_security::Authority::default();
    let sessions: input_capture::CaptureSessions = Default::default();
    let _server = zbus::block_on(
        zbus::connection::Builder::address(bus.1.as_str())
            .serve_at(
                "/org/gnome/Mutter/InputCapture",
                InputCapture {
                    authority: authority.clone(),
                    sessions: sessions.clone(),
                    outputs: Default::default(),
                    to_loop,
                    signals: crate::dbus_signals::SignalQueue::new("test-input-capture-signals"),
                    next_id: Arc::new(AtomicU64::new(1)),
                    scan: Default::default(),
                },
            )
            .name(IC)
            .build(),
    )
    .unwrap();
    let caller = connect(&bus);
    let create = || {
        zbus::block_on(caller.call_method(
            Some(IC),
            "/org/gnome/Mutter/InputCapture",
            Some(IC),
            "CreateSession",
            &(3u32,),
        ))
    };
    // SupportedCapabilities: keyboard and pointer, as the XML numbers them.
    let supported: OwnedVariant = zbus::block_on(caller.call_method(
        Some(IC),
        "/org/gnome/Mutter/InputCapture",
        Some("org.freedesktop.DBus.Properties"),
        "Get",
        &(IC, "SupportedCapabilities"),
    ))
    .unwrap()
    .body()
    .deserialize()
    .unwrap();
    assert_eq!(u32::try_from(supported).unwrap(), 3);
    assert_error(create(), "AccessDenied");
    // Owning the portal backend's name is not being the installed portal.
    zbus::block_on(caller.request_name("org.freedesktop.impl.portal.desktop.gnome")).unwrap();
    assert_error(create(), "AccessDenied");
    authority.publish(true, None);
    assert_error(create(), "AccessDenied");
    assert!(sessions.lock().unwrap().is_empty());
}

/// Warps the loop was asked for.
type Warps = Arc<Mutex<Vec<(f64, f64)>>>;

/// The loop's requests: warps, and the keymap ConnectToEIS asks for.
fn loop_requests(from_dbus: calloop::channel::Channel<ToLoop>) -> (Warps, Arc<AtomicBool>) {
    let warps = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));
    std::thread::spawn({
        let (warps, stop) = (warps.clone(), stop.clone());
        move || {
            let mut event_loop: calloop::EventLoop<()> = calloop::EventLoop::try_new().unwrap();
            event_loop
                .handle()
                .insert_source(from_dbus, move |event, _, _| match event {
                    calloop::channel::Event::Msg(ToLoop::InputCaptureWarp { x, y }) => {
                        warps.lock().unwrap().push((x, y));
                    }
                    calloop::channel::Event::Msg(ToLoop::RemoteKeymap { reply }) => {
                        let _ = reply.send("xkb_keymap { };".to_owned());
                    }
                    _ => {}
                })
                .unwrap();
            while !stop.load(Ordering::SeqCst) {
                event_loop
                    .dispatch(Some(Duration::from_millis(20)), &mut ())
                    .unwrap();
            }
        }
    });
    (warps, stop)
}

/// A receiver EIS client on `fd` that binds everything and reports
/// what it receives.
fn eis_receiver(fd: zbus::OwnedFd) -> std::sync::mpsc::Receiver<String> {
    use reis::event::{DeviceCapability, EiEvent};
    let stream = std::os::unix::net::UnixStream::from(std::os::fd::OwnedFd::from(fd));
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let context = reis::ei::Context::new(stream).unwrap();
        let (_connection, events) = context
            .handshake_blocking(
                "input-capture-test",
                reis::ei::handshake::ContextType::Receiver,
            )
            .unwrap();
        for event in events {
            let Ok(event) = event else {
                return;
            };
            let line = match event {
                EiEvent::SeatAdded(added) => {
                    added.seat.bind_capabilities(
                        DeviceCapability::Pointer
                            | DeviceCapability::Button
                            | DeviceCapability::Scroll
                            | DeviceCapability::Keyboard,
                    );
                    let _ = context.flush();
                    continue;
                }
                EiEvent::DeviceAdded(added) => {
                    format!("added {}", added.device.name().unwrap_or_default())
                }
                EiEvent::DeviceStartEmulating(start) => format!("start {}", start.sequence),
                EiEvent::DeviceStopEmulating(_) => "stop".to_owned(),
                EiEvent::PointerMotion(motion) => format!("motion {} {}", motion.dx, motion.dy),
                EiEvent::KeyboardKey(key) => format!("key {} {:?}", key.key, key.state),
                _ => continue,
            };
            if tx.send(line).is_err() {
                return;
            }
        }
    });
    rx
}

#[test]
fn an_input_capture_session_follows_mutter_from_barrier_to_release() {
    let bus = bus();
    let owner = connect(&bus);
    let foreign = connect(&bus);
    let owner_name = owner.unique_name().unwrap().to_string();
    let authority = crate::capture_security::Authority::default();
    let outputs: Outputs = Arc::new(Mutex::new(vec![monitor("DP-1", 0), monitor("DP-2", 1920)]));
    let (to_loop, from_dbus) = calloop::channel::channel();
    let (warps, stop) = loop_requests(from_dbus);
    let server = zbus::block_on(
        zbus::connection::Builder::address(bus.1.as_str())
            .name(IC)
            .build(),
    )
    .unwrap();
    let emitter = SignalEmitter::new(&server, PATH)
        .unwrap()
        .to_owned()
        .set_destination(zbus::names::BusName::try_from(owner_name.clone()).unwrap());
    let shared = Arc::new(CaptureShared::new(
        1,
        owner_name,
        Zones::from_outputs(&outputs.lock().unwrap()),
        Some(SessionSignals {
            emitter,
            queue: crate::dbus_signals::SignalQueue::new("test-input-capture-signals"),
        }),
    ));
    let mut router = Router::default();
    router.sessions().lock().unwrap().insert(1, shared.clone());
    zbus::block_on(server.object_server().at(
        PATH,
        CaptureSession {
            shared: shared.clone(),
            sessions: router.sessions(),
            authority: authority.clone(),
            outputs: outputs.clone(),
            to_loop,
        },
    ))
    .unwrap();
    let mine = signals(&owner);
    let theirs = signals(&foreign);
    let next = |member: &str| loop {
        let (name, message) = mine.recv_timeout(Duration::from_secs(5)).unwrap();
        if name == member {
            return message;
        }
    };

    // Only the owner, and zones as mutter lists them: (width, height, x, y).
    assert_error(session(&foreign, "GetZones", &()), "AccessDenied");
    let (serial, zones): (u32, Vec<(u32, u32, i32, i32)>) = session(&owner, "GetZones", &())
        .unwrap()
        .body()
        .deserialize()
        .unwrap();
    assert_eq!(serial, 0);
    assert_eq!(zones, vec![(1920, 1080, 0, 0), (1920, 1080, 1920, 0)]);

    // AddBarrier: stale serial, then geometry, then a left-edge barrier.
    assert_error(
        session(&owner, "AddBarrier", &(1u32, (0i32, 0i32, 0i32, 1079i32))),
        "BadAddress",
    );
    assert_error(
        session(
            &owner,
            "AddBarrier",
            &(0u32, (1920i32, 0i32, 1920i32, 1079i32)),
        ),
        "AccessDenied",
    );
    let id: u32 = session(&owner, "AddBarrier", &(0u32, (0i32, 0i32, 0i32, 1079i32)))
        .unwrap()
        .body()
        .deserialize()
        .unwrap();
    assert_eq!(id, 1);
    assert_error(
        session(&owner, "Release", &(HashMap::<&str, Variant>::new(),)),
        "AccessDenied",
    );
    assert_error(session(&owner, "Disable", &()), "Failed");
    assert_error(session(&foreign, "Enable", &()), "AccessDenied");
    // Locked: nothing, not even for the owner.
    authority.publish(true, None);
    assert_error(session(&owner, "Enable", &()), "AccessDenied");
    authority.publish(false, None);
    session(&owner, "Enable", &()).unwrap();
    assert_error(session(&owner, "Enable", &()), "Failed");

    // A receiver EIS client gets a pointer and a keyboard.
    let fd: zbus::OwnedFd = session(&owner, "ConnectToEIS", &())
        .unwrap()
        .body()
        .deserialize()
        .unwrap();
    let eis = eis_receiver(fd);
    let mut added: Vec<String> = (0..2)
        .map(|_| eis.recv_timeout(Duration::from_secs(5)).unwrap())
        .collect();
    added.sort();
    assert_eq!(
        added,
        ["added captured keyboard", "added captured relative pointer"]
    );

    // The pointer crosses the left edge: Activated to the owner alone,
    // and the devices start emulating for activation 1.
    let motion = |dx: f64| {
        vec![
            ManagerInput::Motion {
                pos: (0.0, 10.0).into(),
                time: 1,
            },
            ManagerInput::RelativeMotion {
                delta: (dx, 0.0).into(),
                delta_unaccel: (dx, 0.0).into(),
                utime: 1000,
            },
        ]
    };
    let routed = router.route(motion(-6.0), (1.0, 10.0).into(), false, true);
    assert_eq!(routed.deliver.len(), 1);
    let activated: (u32, u32, (f64, f64)) = next("Activated").body().deserialize().unwrap();
    assert_eq!(activated, (1, 1, (0.0, 10.0)));
    assert_eq!(eis.recv_timeout(Duration::from_secs(5)).unwrap(), "start 1");
    assert_eq!(eis.recv_timeout(Duration::from_secs(5)).unwrap(), "start 1");
    // Captured input reaches the receiver, not the seat.
    assert!(router
        .route(motion(4.0), (0.0, 10.0).into(), false, true)
        .deliver
        .is_empty());
    let key = vec![ManagerInput::Key {
        keycode: 30,
        pressed: true,
        time: 2,
    }];
    assert!(router
        .route(key, (0.0, 10.0).into(), false, true)
        .deliver
        .is_empty());
    assert_eq!(
        eis.recv_timeout(Duration::from_secs(5)).unwrap(),
        "motion 4 0"
    );
    assert_eq!(
        eis.recv_timeout(Duration::from_secs(5)).unwrap(),
        "key 30 Press"
    );

    // Release: a malformed position is refused; a good one deactivates,
    // stops emulating and warps the pointer.
    let position = |value: Variant<'static>| HashMap::from([("cursor_position", value)]);
    assert_error(
        session(&owner, "Release", &(position(Variant::from(7u32)),)),
        "InvalidArgs",
    );
    session(
        &owner,
        "Release",
        &(position(Variant::from((100.0, 200.0))),),
    )
    .unwrap();
    let deactivated: u32 = next("Deactivated").body().deserialize().unwrap();
    assert_eq!(deactivated, 1);
    assert_eq!(eis.recv_timeout(Duration::from_secs(5)).unwrap(), "stop");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while warps.lock().unwrap().is_empty() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(*warps.lock().unwrap(), vec![(100.0, 200.0)]);
    assert_error(
        session(&owner, "Release", &(HashMap::<&str, Variant>::new(),)),
        "AccessDenied",
    );
    // The foreign connection heard none of it.
    assert!(theirs.try_recv().is_err());

    // A second client is refused while the first is connected.
    assert_error(session(&owner, "ConnectToEIS", &()), "LimitsExceeded");

    // Disable, re-arm, and close: Closed goes out and the object goes.
    session(&owner, "Disable", &()).unwrap();
    session(&owner, "ClearBarriers", &()).unwrap();
    assert_error(session(&foreign, "Close", &()), "AccessDenied");
    session(&owner, "Close", &()).unwrap();
    next("Closed");
    assert!(session(&owner, "GetZones", &()).is_err());
    assert!(router.sessions().lock().unwrap().is_empty());
    stop.store(true, Ordering::SeqCst);
}
