//! Private-bus coverage of the Mutter RemoteDesktop surface
//! gnome-remote-desktop drives (#358): who may create sessions, and the
//! session clipboard and keysym input with foreign, stopped, locked and
//! malformed callers.
use super::remote_clipboard::{Effect, Registry};
use super::*;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use zbus::blocking::Connection;

const RD: &str = "org.gnome.Mutter.RemoteDesktop";
const SESSION: &str = "org.gnome.Mutter.RemoteDesktop.Session";
const PATH: &str = "/org/gnome/Mutter/RemoteDesktop/Session/u1";

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
    zbus::blocking::connection::Builder::address(bus.1.as_str())
        .unwrap()
        .method_timeout(Duration::from_secs(5))
        .build()
        .unwrap()
}
fn call<B>(
    conn: &Connection,
    path: &str,
    iface: &str,
    method: &str,
    body: &B,
) -> zbus::Result<zbus::Message>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
{
    conn.call_method(Some(RD), path, Some(iface), method, body)
}
fn session<B>(conn: &Connection, method: &str, body: &B) -> zbus::Result<zbus::Message>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
{
    call(conn, PATH, SESSION, method, body)
}
#[track_caller]
fn assert_error(result: zbus::Result<zbus::Message>, name: &str) -> String {
    match result {
        Err(zbus::Error::MethodError(error, message, _)) => {
            assert_eq!(error.as_str(), format!("org.freedesktop.DBus.Error.{name}"));
            message.unwrap_or_default()
        }
        other => panic!("expected {name}, got {other:?}"),
    }
}
fn no_options() -> HashMap<&'static str, Value<'static>> {
    HashMap::new()
}
fn mime_options(types: &[&str]) -> HashMap<&'static str, Value<'static>> {
    HashMap::from([(
        "mime-types",
        Value::from(types.iter().map(|t| t.to_string()).collect::<Vec<_>>()),
    )])
}
fn fd(message: zbus::Result<zbus::Message>) -> std::fs::File {
    let fd: zbus::zvariant::OwnedFd = message.unwrap().body().deserialize().unwrap();
    std::fs::File::from(std::os::fd::OwnedFd::from(fd))
}
/// Every session signal `conn` receives, as (member, message).
fn signals(conn: &Connection) -> std::sync::mpsc::Receiver<(String, zbus::Message)> {
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface(SESSION)
        .unwrap()
        .build();
    let iterator = zbus::blocking::MessageIterator::for_match_rule(rule, conn, Some(64)).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for message in iterator.flatten() {
            let member = message
                .header()
                .member()
                .map(|m| m.to_string())
                .unwrap_or_default();
            if tx.send((member, message)).is_err() {
                return;
            }
        }
    });
    rx
}
/// SelectionOwnerChanged's mime types (`(as)`) and session-is-owner.
fn owner_changed(message: &zbus::Message) -> (Vec<String>, bool) {
    let options: HashMap<String, OwnedValue> = message.body().deserialize().unwrap();
    let Value::Structure(tuple) = &*options["mime-types"] else {
        panic!("mime-types is not a tuple: {options:?}");
    };
    assert_eq!(tuple.signature().to_string(), "(as)");
    let Value::Array(array) = &tuple.fields()[0] else {
        panic!("not an array");
    };
    let types = array
        .iter()
        .map(|v| match v {
            Value::Str(s) => s.to_string(),
            _ => panic!("not a string"),
        })
        .collect();
    (types, bool::try_from(&options["session-is-owner"]).unwrap())
}

#[test]
fn only_the_installed_remote_desktop_daemon_may_create_sessions() {
    let bus = bus();
    let (to_loop, _from_dbus) = calloop::channel::channel();
    let authority = crate::capture_security::Authority::default();
    let remote: remote_desktop::RemoteSessions = Default::default();
    let sessions: Sessions = Default::default();
    let next_id = Arc::new(AtomicU64::new(1));
    let remote_desktop = remote_desktop::RemoteDesktop {
        authority: authority.clone(),
        remote: remote.clone(),
        captures: sessions.clone(),
        outputs: Default::default(),
        to_loop: to_loop.clone(),
        next_id: next_id.clone(),
    };
    let screencast = ScreenCast {
        remote: remote.clone(),
        authority,
        sessions: sessions.clone(),
        outputs: Default::default(),
        windows: Default::default(),
        to_loop,
        signals: crate::dbus_signals::SignalQueue::new("test-mutter-signals"),
        next_id,
    };
    let _server = zbus::blocking::connection::Builder::address(bus.1.as_str())
        .unwrap()
        .serve_at("/org/gnome/Mutter/RemoteDesktop", remote_desktop)
        .unwrap()
        .serve_at("/org/gnome/Mutter/ScreenCast", screencast)
        .unwrap()
        .name(RD)
        .unwrap()
        .build()
        .unwrap();
    let caller = connect(&bus);
    let create = |conn: &Connection| {
        call(
            conn,
            "/org/gnome/Mutter/RemoteDesktop",
            RD,
            "CreateSession",
            &(),
        )
    };
    assert_error(create(&caller), "AccessDenied");
    // A same-user process that takes every grd bus name is still not the
    // installed root-owned gnome-remote-desktop-daemon.
    for name in [
        "org.gnome.RemoteDesktop.User",
        "org.gnome.RemoteDesktop.Handover",
        "org.gnome.RemoteDesktop.Headless",
    ] {
        caller.request_name(name).unwrap();
    }
    assert_error(create(&caller), "AccessDenied");
    // Nor may it open the ScreenCast session grd links to its grant.
    let linked = HashMap::from([("remote-desktop-session-id", Value::from("tuna-remote-1"))]);
    assert_error(
        call(
            &caller,
            "/org/gnome/Mutter/ScreenCast",
            "org.gnome.Mutter.ScreenCast",
            "CreateSession",
            &(linked,),
        ),
        "AccessDenied",
    );
    assert!(remote.lock().unwrap().is_empty());
    assert!(sessions.lock().unwrap().is_empty());
}

#[test]
fn session_clipboard_and_keysyms_are_owned_bounded_and_revocable() {
    let bus = bus();
    let owner = connect(&bus);
    let foreign = connect(&bus);
    let authority = crate::capture_security::Authority::default();
    let grant = remote_desktop::RemoteGrant::new(1, owner.unique_name().unwrap().to_string());
    let (to_loop, from_dbus) = calloop::channel::channel();
    let _server = zbus::blocking::connection::Builder::address(bus.1.as_str())
        .unwrap()
        .serve_at(
            PATH,
            remote_desktop::RemoteSession {
                grant: grant.clone(),
                authority: authority.clone(),
                captures: Default::default(),
                outputs: Default::default(),
                to_loop,
            },
        )
        .unwrap()
        .name(RD)
        .unwrap()
        .build()
        .unwrap();

    // The loop: the session registry, a selection owner that answers
    // reads with "local text", and the input queue.
    let registry = Arc::new(Mutex::new(Registry::default()));
    let owned = Arc::new(Mutex::new(Vec::<Option<Vec<String>>>::new()));
    let inputs = Arc::new(Mutex::new(Vec::<String>::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let event_loop_thread = std::thread::spawn({
        let (registry, owned, inputs, stop) = (
            registry.clone(),
            owned.clone(),
            inputs.clone(),
            stop.clone(),
        );
        move || {
            let mut event_loop: calloop::EventLoop<()> = calloop::EventLoop::try_new().unwrap();
            event_loop
                .handle()
                .insert_source(from_dbus, move |event, _, _| {
                    let calloop::channel::Event::Msg(message) = event else {
                        return;
                    };
                    match message {
                        ToLoop::RemoteClipboard(request) => {
                            let effect = registry.lock().unwrap().handle(request);
                            match effect {
                                Effect::Own { mime_types, .. } => {
                                    owned.lock().unwrap().push(Some(mime_types))
                                }
                                Effect::Clear => owned.lock().unwrap().push(None),
                                Effect::Read { fd, reply, .. } => {
                                    std::fs::File::from(fd).write_all(b"local text").unwrap();
                                    reply.send(Ok(())).unwrap();
                                }
                                Effect::None => {}
                            }
                        }
                        ToLoop::RemoteInput { input, pending, .. } => {
                            pending.fetch_sub(1, Ordering::SeqCst);
                            inputs.lock().unwrap().push(format!("{input:?}"));
                        }
                        _ => {}
                    }
                })
                .unwrap();
            while !stop.load(Ordering::SeqCst) {
                event_loop
                    .dispatch(Some(Duration::from_millis(20)), &mut ())
                    .unwrap();
            }
        }
    });
    let mine = signals(&owner);
    let theirs = signals(&foreign);
    let next = |member: &str| loop {
        let (name, message) = mine.recv_timeout(Duration::from_secs(5)).unwrap();
        if name == member {
            return message;
        }
    };

    // A foreign caller gets nothing; the owner needs an enabled clipboard.
    assert_error(
        session(&foreign, "EnableClipboard", &(no_options(),)),
        "AccessDenied",
    );
    assert_error(session(&owner, "SelectionWrite", &(1u32,)), "Failed");
    assert_error(session(&owner, "SelectionRead", &("text/plain",)), "Failed");
    assert_error(session(&owner, "SetSelection", &(no_options(),)), "Failed");
    // Garbage options never enable it.
    let garbage = HashMap::from([("mime-types", Value::from(7u32))]);
    assert_error(
        session(&owner, "EnableClipboard", &(garbage,)),
        "InvalidArgs",
    );
    assert_error(
        session(&owner, "EnableClipboard", &(mime_options(&[]),)),
        "InvalidArgs",
    );
    session(&owner, "EnableClipboard", &(no_options(),)).unwrap();
    assert_error(
        session(&owner, "EnableClipboard", &(no_options(),)),
        "Failed",
    );

    // A local client copies: only the owner hears, with mutter's (as).
    registry
        .lock()
        .unwrap()
        .client_selection(Some(vec!["text/plain;charset=utf-8".into()]));
    let changed = next("SelectionOwnerChanged");
    assert_eq!(
        owner_changed(&changed),
        (vec!["text/plain;charset=utf-8".to_owned()], false)
    );
    assert_eq!(
        changed.header().destination().map(|d| d.to_string()),
        Some(owner.unique_name().unwrap().to_string())
    );
    // The session pastes it through a pipe.
    let mut text = String::new();
    fd(session(
        &owner,
        "SelectionRead",
        &("text/plain;charset=utf-8",),
    ))
    .read_to_string(&mut text)
    .unwrap();
    assert_eq!(text, "local text");
    assert_error(
        session(&owner, "SelectionRead", &("not a mime",)),
        "InvalidArgs",
    );
    assert_error(
        session(&foreign, "SelectionRead", &("text/plain",)),
        "AccessDenied",
    );

    // The remote side copies: the session owns the selection.
    session(&owner, "SetSelection", &(mime_options(&["text/plain"]),)).unwrap();
    assert_eq!(
        owner_changed(&next("SelectionOwnerChanged")),
        (vec!["text/plain".to_owned()], true)
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while owned.lock().unwrap().is_empty() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        *owned.lock().unwrap(),
        vec![Some(vec!["text/plain".to_owned()])]
    );
    // Reading one's own selection is refused, as in mutter (once the
    // earlier read's pump has finished, a parallel read is refused too).
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let refusal = loop {
        let message = assert_error(session(&owner, "SelectionRead", &("text/plain",)), "Failed");
        if !message.contains("parallel") || std::time::Instant::now() > deadline {
            break message;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(refusal, "Tried to read own selection");

    // A local client pastes: SelectionTransfer, then a write pipe.
    let (mut local, local_w) = std::io::pipe().unwrap();
    registry
        .lock()
        .unwrap()
        .transfer(1, "text/plain", local_w.into());
    let transfer = next("SelectionTransfer");
    let (mime, serial): (String, u32) = transfer.body().deserialize().unwrap();
    assert_eq!(mime, "text/plain");
    assert_error(
        session(&foreign, "SelectionWrite", &(serial,)),
        "AccessDenied",
    );
    assert_error(
        session(&owner, "SelectionWrite", &(serial + 100,)),
        "Failed",
    );
    let mut sink = fd(session(&owner, "SelectionWrite", &(serial,)));
    sink.write_all(b"remote text").unwrap();
    drop(sink);
    let mut text = String::new();
    local.read_to_string(&mut text).unwrap();
    assert_eq!(text, "remote text");
    session(&owner, "SelectionWriteDone", &(serial, true)).unwrap();
    assert_error(session(&owner, "SelectionWrite", &(serial,)), "Failed");

    // Nothing above reached the foreign connection.
    assert!(theirs.recv_timeout(Duration::from_millis(300)).is_err());

    // Disable gives up the selection; a second disable fails.
    session(&owner, "DisableClipboard", &()).unwrap();
    assert_error(session(&owner, "DisableClipboard", &()), "Failed");
    assert_error(session(&owner, "SelectionWrite", &(serial,)), "Failed");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while owned.lock().unwrap().len() < 2 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(owned.lock().unwrap().last(), Some(&None));

    // EIS may be requested before Start (as grd does), never by another
    // caller.
    assert_error(
        session(&foreign, "ConnectToEIS", &(no_options(),)),
        "AccessDenied",
    );
    // Keysyms need a started session and its owner.
    assert_error(
        session(&owner, "NotifyKeyboardKeysym", &(0x61u32, true)),
        "AccessDenied",
    );
    grant.started.store(true, Ordering::SeqCst);
    session(&owner, "NotifyKeyboardKeysym", &(0x61u32, true)).unwrap();
    session(&owner, "NotifyKeyboardKeysym", &(0x61u32, false)).unwrap();
    assert_error(
        session(&foreign, "NotifyKeyboardKeysym", &(0x61u32, true)),
        "AccessDenied",
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while inputs.lock().unwrap().len() < 2 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        *inputs.lock().unwrap(),
        vec![
            "Keysym { keysym: 97, pressed: true }".to_owned(),
            "Keysym { keysym: 97, pressed: false }".to_owned()
        ]
    );

    // Locked: every clipboard and input call is refused.
    session(&owner, "EnableClipboard", &(no_options(),)).unwrap();
    authority.publish(true, None);
    for result in [
        session(&owner, "SetSelection", &(mime_options(&["text/plain"]),)),
        session(&owner, "SelectionRead", &("text/plain",)),
        session(&owner, "SelectionWrite", &(1u32,)),
        session(&owner, "DisableClipboard", &()),
        session(&owner, "NotifyKeyboardKeysym", &(0x61u32, true)),
    ] {
        assert_error(result, "AccessDenied");
    }
    authority.publish(false, None);

    // A stopped grant cuts an in-flight transfer: the local reader sees
    // end of file, and the session can do nothing more.
    let (mut local, local_w) = std::io::pipe().unwrap();
    registry
        .lock()
        .unwrap()
        .transfer(1, "text/plain", local_w.into());
    let (_, serial): (String, u32) = next("SelectionTransfer").body().deserialize().unwrap();
    let _stalled = fd(session(&owner, "SelectionWrite", &(serial,)));
    grant.stopped.store(true, Ordering::SeqCst);
    let mut data = Vec::new();
    local.read_to_end(&mut data).unwrap();
    assert!(data.is_empty());
    for result in [
        session(&owner, "SelectionRead", &("text/plain",)),
        session(&owner, "SetSelection", &(no_options(),)),
        session(&owner, "EnableClipboard", &(no_options(),)),
        session(&owner, "NotifyKeyboardKeysym", &(0x61u32, true)),
    ] {
        assert_error(result, "AccessDenied");
    }
    assert!(matches!(registry.lock().unwrap().remove(1), Effect::None));
    // grd's Stop after its grant ended (its EIS client disconnects first)
    // succeeds for the owner and changes nothing; another caller is
    // still refused, and the ended grant never restarts.
    assert_error(session(&foreign, "Stop", &()), "AccessDenied");
    session(&owner, "Stop", &()).unwrap();
    assert_error(session(&owner, "Start", &()), "AccessDenied");

    stop.store(true, Ordering::SeqCst);
    event_loop_thread.join().unwrap();
}

#[test]
fn an_empty_connector_records_the_primary_monitor_like_mutter() {
    let output = |connector: &str, primary| OutputSnapshot {
        connector: connector.into(),
        make: String::new(),
        model: String::new(),
        width: 1280,
        height: 800,
        refresh_mhz: 60000,
        x: 0,
        y: 0,
        scale: 1.0,
        primary,
        transform: 0,
    };
    let outputs = [output("Virtual-1", false), output("Virtual-2", true)];
    let pick = |c| record_monitor_target(&outputs, c).map(|o| o.connector);
    assert_eq!(pick("").as_deref(), Some("Virtual-2"));
    assert_eq!(pick("Virtual-1").as_deref(), Some("Virtual-1"));
    assert_eq!(pick("HDMI-A-1"), None);
    assert_eq!(record_monitor_target(&[], ""), None);
    // No primary published yet: the first lit output stands in.
    let unflagged = [output("Virtual-1", false)];
    assert_eq!(
        record_monitor_target(&unflagged, "")
            .map(|o| o.connector)
            .as_deref(),
        Some("Virtual-1")
    );
}

#[test]
fn linked_monitor_streams_carry_the_eis_mapping_id_grd_requires() {
    let output = OutputSnapshot {
        connector: "Virtual-1".into(),
        make: String::new(),
        model: String::new(),
        width: 2560,
        height: 1600,
        refresh_mhz: 60000,
        x: 10,
        y: 20,
        scale: 2.0,
        primary: true,
        transform: 0,
    };
    let linked = StreamSource::Monitor(output.clone(), Some(output_mapping_id("Virtual-1")));
    let parameters = linked.parameters();
    assert_eq!(
        <&str>::try_from(&parameters["mapping-id"]).unwrap(),
        "tuna-monitor-Virtual-1"
    );
    let pair = |key: &str| match &*parameters[key] {
        Value::Structure(s) => match s.fields() {
            [Value::I32(a), Value::I32(b)] => (*a, *b),
            other => panic!("{key}: {other:?}"),
        },
        other => panic!("{key}: {other:?}"),
    };
    assert_eq!(pair("size"), (1280, 800));
    assert_eq!(pair("position"), (10, 20));
    // A portal (unlinked) stream has no mapping id, as in mutter.
    assert!(!StreamSource::Monitor(output, None)
        .parameters()
        .contains_key("mapping-id"));
}

fn monitor(connector: &str) -> OutputSnapshot {
    OutputSnapshot {
        connector: connector.into(),
        make: String::new(),
        model: String::new(),
        width: 1280,
        height: 800,
        refresh_mhz: 60000,
        x: 0,
        y: 0,
        scale: 1.0,
        primary: true,
        transform: 0,
    }
}

/// A ScreenCast session for `owner` at `/org/gnome/Mutter/ScreenCast/Session/u<id>`,
/// linked to `grant` when given (what CreateSession builds once the
/// caller is admitted).
fn capture_session(
    id: u64,
    owner: &str,
    authority: &crate::capture_security::Authority,
    grant: Option<&remote_desktop::RemoteGrant>,
    to_loop: &calloop::channel::Sender<ToLoop>,
) -> Session {
    Session {
        owner: owner.into(),
        authority: authority.clone(),
        id,
        outputs: Arc::new(Mutex::new(vec![monitor("Virtual-1")])),
        windows: Default::default(),
        to_loop: to_loop.clone(),
        signals: crate::dbus_signals::SignalQueue::new("test-mutter-signals"),
        next_id: Arc::new(AtomicU64::new(100)),
        streams: Default::default(),
        stopped: grant.map_or_else(|| Arc::new(AtomicBool::new(false)), |g| g.stopped.clone()),
        started: Arc::new(AtomicBool::new(false)),
        linked: grant.is_some(),
    }
}

/// The casts the event loop is asked to start and stop, as
/// `start <stream> <signal destination>` / `stop <stream>` / `stop-all`.
fn cast_requests(
    from_dbus: calloop::channel::Channel<ToLoop>,
) -> (
    Arc<Mutex<Vec<String>>>,
    Arc<AtomicBool>,
    std::thread::JoinHandle<()>,
) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let thread = std::thread::spawn({
        let (seen, stop) = (seen.clone(), stop.clone());
        move || {
            let mut event_loop: calloop::EventLoop<()> = calloop::EventLoop::try_new().unwrap();
            event_loop
                .handle()
                .insert_source(from_dbus, move |event, _, _| {
                    let calloop::channel::Event::Msg(message) = event else {
                        return;
                    };
                    let line = match message {
                        ToLoop::StartCast {
                            stream_id, signal, ..
                        } => format!(
                            "start {stream_id} {}",
                            signal
                                .stream
                                .destination()
                                .map(|d| d.to_string())
                                .unwrap_or_default()
                        ),
                        ToLoop::StopStream { stream_id, .. } => format!("stop {stream_id}"),
                        ToLoop::StopCast { .. } => "stop-all".to_owned(),
                        _ => return,
                    };
                    seen.lock().unwrap().push(line);
                })
                .unwrap();
            while !stop.load(Ordering::SeqCst) {
                event_loop
                    .dispatch(Some(Duration::from_millis(20)), &mut ())
                    .unwrap();
            }
        }
    });
    (seen, stop, thread)
}

#[track_caller]
fn expect_requests(seen: &Mutex<Vec<String>>, expected: &[String]) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while seen.lock().unwrap().len() < expected.len() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    // Nothing beyond what was expected trickles in.
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(*seen.lock().unwrap(), expected);
}

fn object_path(message: zbus::Result<zbus::Message>) -> String {
    let path: OwnedObjectPath = message.unwrap().body().deserialize().unwrap();
    path.to_string()
}

fn stream_id(path: &str) -> u64 {
    path.rsplit_once("/u").unwrap().1.parse().unwrap()
}

#[test]
fn linked_screencast_records_after_remote_start_and_serves_stream_start_stop() {
    const SC_PATH: &str = "/org/gnome/Mutter/ScreenCast/Session/u7";
    const SC_SESSION: &str = "org.gnome.Mutter.ScreenCast.Session";
    const SC_STREAM: &str = "org.gnome.Mutter.ScreenCast.Stream";
    let bus = bus();
    let owner = connect(&bus);
    let foreign = connect(&bus);
    let owner_name = owner.unique_name().unwrap().to_string();
    let authority = crate::capture_security::Authority::default();
    let grant = remote_desktop::RemoteGrant::new(1, owner_name.clone());
    let (to_loop, from_dbus) = calloop::channel::channel();
    let captures: Sessions = Default::default();
    let capture = capture_session(7, &owner_name, &authority, Some(&grant), &to_loop);
    captures
        .lock()
        .unwrap()
        .push((capture.clone(), OwnedObjectPath::try_from(SC_PATH).unwrap()));
    let _server = zbus::blocking::connection::Builder::address(bus.1.as_str())
        .unwrap()
        .serve_at(
            PATH,
            remote_desktop::RemoteSession {
                grant: grant.clone(),
                authority: authority.clone(),
                captures: captures.clone(),
                outputs: Default::default(),
                to_loop: to_loop.clone(),
            },
        )
        .unwrap()
        .serve_at(SC_PATH, capture)
        .unwrap()
        .name(RD)
        .unwrap()
        .build()
        .unwrap();
    let (seen, stop, event_loop_thread) = cast_requests(from_dbus);
    let sc = |conn: &Connection, method: &str, body: &(&str, HashMap<&str, Value>)| {
        call(conn, SC_PATH, SC_SESSION, method, body)
    };
    let stream =
        |conn: &Connection, path: &str, method: &str| call(conn, path, SC_STREAM, method, &());

    // Before the remote session starts, a stream may be recorded but
    // not started.
    let early = object_path(sc(&owner, "RecordMonitor", &("", no_options())));
    assert_error(
        sc(&foreign, "RecordMonitor", &("", no_options())),
        "AccessDenied",
    );
    let refusal = assert_error(stream(&owner, &early, "Start"), "Failed");
    assert_eq!(refusal, "Failed to start stream: session not started");
    // A linked session follows its RemoteDesktop session (mutter's
    // handle_start / handle_stop).
    assert_eq!(
        assert_error(call(&owner, SC_PATH, SC_SESSION, "Start", &()), "Failed"),
        "Must be started from remote desktop session"
    );
    assert_eq!(
        assert_error(call(&owner, SC_PATH, SC_SESSION, "Stop", &()), "Failed"),
        "Must be stopped from remote desktop session"
    );
    assert!(!grant.stopped.load(Ordering::SeqCst));

    // RemoteDesktop.Start starts what was recorded so far; its node
    // signal is addressed to the owner alone.
    assert_error(session(&foreign, "Start", &()), "AccessDenied");
    session(&owner, "Start", &()).unwrap();
    expect_requests(
        &seen,
        &[format!("start {} {owner_name}", stream_id(&early))],
    );
    assert_eq!(
        assert_error(stream(&owner, &early, "Start"), "Failed"),
        "Failed to start stream: Stream already started"
    );

    // gnome-remote-desktop's order: RecordMonitor after Start, then
    // Stream.Start.
    let late = object_path(sc(&owner, "RecordMonitor", &("", no_options())));
    assert_error(stream(&foreign, &late, "Start"), "AccessDenied");
    assert_error(stream(&foreign, &late, "Stop"), "AccessDenied");
    let not_started = assert_error(stream(&owner, &late, "Stop"), "AccessDenied");
    assert_eq!(not_started, "Stream not started");
    stream(&owner, &late, "Start").unwrap();
    let mut expected = vec![
        format!("start {} {owner_name}", stream_id(&early)),
        format!("start {} {owner_name}", stream_id(&late)),
    ];
    expect_requests(&seen, &expected);
    // Its parameters carry the mapping id grd needs.
    let parameters: OwnedValue = owner
        .call_method(
            Some(RD),
            late.as_str(),
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &(SC_STREAM, "Parameters"),
        )
        .unwrap()
        .body()
        .deserialize()
        .unwrap();
    let parameters: HashMap<String, OwnedValue> = parameters.try_into().unwrap();
    assert_eq!(
        <&str>::try_from(&parameters["mapping-id"]).unwrap(),
        "tuna-monitor-Virtual-1"
    );

    // Locked: nothing new is recorded or started.
    let pending = object_path(sc(&owner, "RecordMonitor", &("", no_options())));
    authority.publish(true, None);
    assert_error(
        sc(&owner, "RecordMonitor", &("", no_options())),
        "AccessDenied",
    );
    assert_error(stream(&owner, &pending, "Start"), "AccessDenied");
    authority.publish(false, None);

    // Stream.Stop ends that stream alone; the linked session and its
    // grant live on, and the stopped stream's object is gone.
    stream(&owner, &late, "Stop").unwrap();
    expected.push(format!("stop {}", stream_id(&late)));
    expect_requests(&seen, &expected);
    assert_error(stream(&owner, &late, "Stop"), "UnknownObject");
    assert!(!grant.stopped.load(Ordering::SeqCst));
    assert_eq!(
        captures.lock().unwrap()[0].0.streams.lock().unwrap().len(),
        2
    );

    // Revoked: the owner can neither record, start nor stop.
    grant.stopped.store(true, Ordering::SeqCst);
    assert_error(
        sc(&owner, "RecordMonitor", &("", no_options())),
        "AccessDenied",
    );
    assert_error(stream(&owner, &pending, "Start"), "AccessDenied");
    assert_error(stream(&owner, &early, "Stop"), "AccessDenied");
    expect_requests(&seen, &expected);

    stop.store(true, Ordering::SeqCst);
    event_loop_thread.join().unwrap();
}

#[test]
fn a_plain_session_streams_after_start_and_ends_with_its_stream() {
    const SC_PATH: &str = "/org/gnome/Mutter/ScreenCast/Session/u3";
    const SC_SESSION: &str = "org.gnome.Mutter.ScreenCast.Session";
    const SC_STREAM: &str = "org.gnome.Mutter.ScreenCast.Stream";
    let bus = bus();
    let owner = connect(&bus);
    let foreign = connect(&bus);
    let owner_name = owner.unique_name().unwrap().to_string();
    let authority = crate::capture_security::Authority::default();
    let (to_loop, from_dbus) = calloop::channel::channel();
    let capture = capture_session(3, &owner_name, &authority, None, &to_loop);
    let stopped = capture.stopped.clone();
    let _server = zbus::blocking::connection::Builder::address(bus.1.as_str())
        .unwrap()
        .serve_at(SC_PATH, capture)
        .unwrap()
        .name(RD)
        .unwrap()
        .build()
        .unwrap();
    let (seen, stop, event_loop_thread) = cast_requests(from_dbus);
    let record = |conn: &Connection| {
        call(
            conn,
            SC_PATH,
            SC_SESSION,
            "RecordMonitor",
            &("Virtual-1", no_options()),
        )
    };

    assert_error(
        call(&foreign, SC_PATH, SC_SESSION, "Start", &()),
        "AccessDenied",
    );
    call(&owner, SC_PATH, SC_SESSION, "Start", &()).unwrap();
    // Recorded on an active session: waits for its own Start.
    let path = object_path(record(&owner));
    expect_requests(&seen, &[]);
    call(&owner, &path, SC_STREAM, "Start", &()).unwrap();
    let mut expected = vec![format!("start {} {owner_name}", stream_id(&path))];
    expect_requests(&seen, &expected);
    // Stopping the stream closes the plain session (mutter's
    // on_stream_closed); nothing more may be recorded on it.
    assert_error(
        call(&foreign, &path, SC_STREAM, "Stop", &()),
        "AccessDenied",
    );
    call(&owner, &path, SC_STREAM, "Stop", &()).unwrap();
    expected.push(format!("stop {}", stream_id(&path)));
    expect_requests(&seen, &expected);
    assert!(stopped.load(Ordering::SeqCst));
    assert_error(record(&owner), "AccessDenied");

    stop.store(true, Ordering::SeqCst);
    event_loop_thread.join().unwrap();
}

#[test]
fn a_remote_grant_links_one_screencast_session_of_its_own_owner() {
    let (to_loop, _from_dbus) = calloop::channel::channel();
    let remote: remote_desktop::RemoteSessions = Default::default();
    let sessions: Sessions = Default::default();
    let screencast = ScreenCast {
        remote: remote.clone(),
        authority: Default::default(),
        sessions: sessions.clone(),
        outputs: Default::default(),
        windows: Default::default(),
        to_loop,
        signals: crate::dbus_signals::SignalQueue::new("test-mutter-signals"),
        next_id: Arc::new(AtomicU64::new(1)),
    };
    let grant = remote_desktop::RemoteGrant::new(1, ":1.5".into());
    remote
        .lock()
        .unwrap()
        .insert("tuna-remote-1".into(), grant.clone());
    let error_name = |result: fdo::Result<Arc<AtomicBool>>| match result {
        Err(fdo::Error::AccessDenied(_)) => "AccessDenied",
        Err(fdo::Error::Failed(_)) => "Failed",
        Err(other) => panic!("unexpected {other:?}"),
        Ok(_) => "Ok",
    };
    assert_eq!(
        error_name(screencast.linked_grant(":1.5", "tuna-remote-9")),
        "AccessDenied"
    );
    assert_eq!(
        error_name(screencast.linked_grant(":1.6", "tuna-remote-1")),
        "AccessDenied"
    );
    let flag = screencast.linked_grant(":1.5", "tuna-remote-1").unwrap();
    assert!(Arc::ptr_eq(&flag, &grant.stopped));
    // Registered: a second linked session is refused, as in mutter.
    let (sender, _receiver) = calloop::channel::channel();
    let linked = capture_session(2, ":1.5", &Default::default(), Some(&grant), &sender);
    sessions.lock().unwrap().push((
        linked,
        OwnedObjectPath::try_from("/org/gnome/Mutter/ScreenCast/Session/u2").unwrap(),
    ));
    assert_eq!(
        error_name(screencast.linked_grant(":1.5", "tuna-remote-1")),
        "Failed"
    );
    // A started or stopped grant takes none.
    sessions.lock().unwrap().clear();
    grant.started.store(true, Ordering::SeqCst);
    assert_eq!(
        error_name(screencast.linked_grant(":1.5", "tuna-remote-1")),
        "AccessDenied"
    );
    grant.started.store(false, Ordering::SeqCst);
    grant.stopped.store(true, Ordering::SeqCst);
    assert_eq!(
        error_name(screencast.linked_grant(":1.5", "tuna-remote-1")),
        "AccessDenied"
    );
}
