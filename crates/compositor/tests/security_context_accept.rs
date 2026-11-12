//! A security-context listener with no descriptor left to accept on (#1071).
//!
//! The runtime (`Runtime::admit_security_contexts`) and `TestCompositor`
//! put the same `security_listener::RestingListener` on their loops; here
//! the real Smithay listener, created over the protocol, meets a real
//! EMFILE.
//!
//! This file holds one test on purpose: the test lowers RLIMIT_NOFILE and
//! uses up every descriptor of the process, which a test running on
//! another thread would not survive.

use std::collections::BTreeMap;
use std::fs::File;
use std::io;
use std::os::fd::AsFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::time::{Duration, Instant};

use rustix::process::{getrlimit, setrlimit, Resource, Rlimit};
use tuna_compositor::security_listener::REST;
use tuna_compositor::TestCompositor;
use wayland_client::protocol::{
    wl_callback::{self, WlCallback},
    wl_registry::{self, WlRegistry},
};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle};
use wayland_protocols::wp::security_context::v1::client::{
    wp_security_context_manager_v1::WpSecurityContextManagerV1,
    wp_security_context_v1::WpSecurityContextV1,
};

wayland_client::delegate_noop!(Client: ignore WpSecurityContextManagerV1);
wayland_client::delegate_noop!(Client: ignore WpSecurityContextV1);

/// How long the compositor gets to do what a test waits for.
const PATIENCE: Duration = Duration::from_secs(10);

#[derive(Default)]
struct Client {
    /// Advertised globals: interface to registry name.
    globals: BTreeMap<String, u32>,
    /// wl_display.sync callbacks answered.
    synced: u32,
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
            name, interface, ..
        } = event
        {
            state.globals.insert(interface, name);
        }
    }
}

impl Dispatch<WlCallback, ()> for Client {
    fn event(
        state: &mut Self,
        _: &WlCallback,
        event: wl_callback::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { .. } = event {
            state.synced += 1;
        }
    }
}

struct Peer {
    conn: Connection,
    registry: WlRegistry,
    queue: EventQueue<Client>,
    client: Client,
}

/// A client on `stream`, its registry requested.
fn peer(stream: UnixStream) -> Peer {
    let conn = Connection::from_socket(stream).unwrap();
    let queue = conn.new_event_queue();
    let registry = conn.display().get_registry(&queue.handle(), ());
    Peer {
        conn,
        registry,
        queue,
        client: Client::default(),
    }
}

/// Pump the compositor until it has answered a wl_display.sync from `peer`,
/// and so everything `peer` sent before it. Opens no descriptor.
fn roundtrip(comp: &mut TestCompositor, peer: &mut Peer) {
    let answered = peer.client.synced;
    peer.conn.display().sync(&peer.queue.handle(), ());
    let started = Instant::now();
    while peer.client.synced == answered {
        assert!(
            started.elapsed() < PATIENCE,
            "the compositor stopped answering its client"
        );
        peer.queue.flush().unwrap();
        comp.pump();
        if let Some(guard) = peer.queue.prepare_read() {
            let _ = guard.read();
        }
        peer.queue.dispatch_pending(&mut peer.client).unwrap();
    }
}

/// Every descriptor the process may open, held until dropped.
struct Exhausted {
    held: Vec<File>,
    limit: Rlimit,
}

impl Exhausted {
    /// The soft limit while exhausted: low, so that filling it is quick.
    const LIMIT: u64 = 256;

    fn new() -> Self {
        let null = File::open("/dev/null").unwrap();
        let limit = getrlimit(Resource::Nofile);
        let lowered = Rlimit {
            current: Some(
                limit
                    .current
                    .map_or(Self::LIMIT, |soft| soft.min(Self::LIMIT)),
            ),
            maximum: limit.maximum,
        };
        setrlimit(Resource::Nofile, lowered).unwrap();
        let mut exhausted = Self {
            held: Vec::new(),
            limit,
        };
        loop {
            match null.try_clone() {
                Ok(file) => exhausted.held.push(file),
                Err(error) => {
                    assert_eq!(error.raw_os_error(), Some(libc::EMFILE), "{error}");
                    break;
                }
            }
        }
        exhausted.held.push(null);
        exhausted
    }

    /// Connect to `path` on the one descriptor given up for it: none is
    /// left for the listener to accept the connection on.
    fn connect(&mut self, path: &Path) -> UnixStream {
        drop(self.held.pop());
        UnixStream::connect(path).unwrap()
    }
}

impl Drop for Exhausted {
    fn drop(&mut self) {
        self.held.clear();
        if let Err(error) = setrlimit(Resource::Nofile, self.limit) {
            eprintln!("RLIMIT_NOFILE not restored: {error}");
        }
    }
}

/// The regression (#1071): with no descriptor left, the listener's failed
/// accept went to the event loop as an error and ended it. The loop now
/// keeps serving, the listener retries once per rest instead of on every
/// dispatch, the waiting client joins inside the context once descriptors
/// are free, and a context closed during a rest is still removed.
#[test]
fn a_security_context_listener_survives_descriptor_exhaustion() {
    let mut comp = TestCompositor::new();
    let (server, stream) = UnixStream::pair().unwrap();
    comp.add_client(server);
    let mut engine = peer(stream);
    roundtrip(&mut comp, &mut engine);

    // A sandbox engine commits a context and keeps only its end of the
    // close fd: the compositor has the listening socket to itself.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wayland-sandbox");
    let listener = UnixListener::bind(&path).unwrap();
    let (close_fd, engine_end) = UnixStream::pair().unwrap();
    let qh = engine.queue.handle();
    let manager: WpSecurityContextManagerV1 = engine.registry.bind(
        engine.client.globals["wp_security_context_manager_v1"],
        1,
        &qh,
        (),
    );
    let context = manager.create_listener(listener.as_fd(), close_fd.as_fd(), &qh, ());
    context.set_sandbox_engine("org.flatpak".into());
    context.set_app_id("org.example.Sandboxed".into());
    context.set_instance_id("42".into());
    context.commit();
    roundtrip(&mut comp, &mut engine);
    drop((listener, close_fd));

    // A client connects while the compositor has no descriptor to accept
    // it on. Every round is a full dispatch of the compositor that answers
    // the engine; the listener is tried once per rest.
    let mut exhausted = Exhausted::new();
    let waiting = exhausted.connect(&path);
    let started = Instant::now();
    let mut rounds = 0;
    while comp.security_accept_failures() < 3 && started.elapsed() < PATIENCE {
        roundtrip(&mut comp, &mut engine);
        rounds += 1;
        std::thread::sleep(Duration::from_millis(1));
    }
    let spent = started.elapsed();
    let failures = comp.security_accept_failures();
    let admitted_without_descriptors = comp.security_context_clients().len();
    // Assertions wait until the process can open files again.
    drop(exhausted);
    assert_eq!(
        failures, 3,
        "the listener keeps retrying while no descriptor is free"
    );
    assert!(
        spent >= REST * 2,
        "three accept attempts are two rests apart, not {spent:?}"
    );
    assert!(
        rounds >= 10 * failures,
        "the compositor dispatched {rounds} times in {spent:?}"
    );
    assert_eq!(admitted_without_descriptors, 0);

    // Descriptors are free again: the connection that waited is accepted
    // at the next retry, inside the context.
    let started = Instant::now();
    while comp.security_context_clients().is_empty() && started.elapsed() < PATIENCE {
        roundtrip(&mut comp, &mut engine);
        std::thread::sleep(Duration::from_millis(1));
    }
    let admitted = comp.security_context_clients();
    assert_eq!(admitted.len(), 1, "the waiting client is admitted");
    assert_eq!(admitted[0].sandbox_engine.as_deref(), Some("org.flatpak"));
    assert_eq!(admitted[0].app_id.as_deref(), Some("org.example.Sandboxed"));
    assert_eq!(admitted[0].instance_id.as_deref(), Some("42"));
    assert_eq!(comp.security_accept_failures(), failures);
    let mut sandboxed = peer(waiting);
    roundtrip(&mut comp, &mut sandboxed);
    assert!(sandboxed.client.globals.contains_key("wl_compositor"));
    for hidden in [
        "ext_session_lock_manager_v1",
        "wp_security_context_manager_v1",
    ] {
        assert!(
            !sandboxed.client.globals.contains_key(hidden),
            "{hidden} must stay out of a sandboxed client's registry"
        );
    }

    // The engine closes the context while its listener rests after another
    // failed accept: the listener still goes, and with it the socket.
    let mut exhausted = Exhausted::new();
    let _unaccepted = exhausted.connect(&path);
    roundtrip(&mut comp, &mut engine);
    let resting = comp.security_accept_failures();
    drop(engine_end);
    roundtrip(&mut comp, &mut engine);
    drop(exhausted);
    assert_eq!(resting, failures + 1, "the listener is resting");
    let started = Instant::now();
    let removed = loop {
        roundtrip(&mut comp, &mut engine);
        match UnixStream::connect(&path) {
            Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => break true,
            _ if started.elapsed() >= PATIENCE => break false,
            // Few probes per rest: each one that connects waits in the
            // listener's queue.
            _ => std::thread::sleep(REST / 5),
        }
    };
    assert!(
        removed,
        "a context closed while its listener rested is removed"
    );
}
