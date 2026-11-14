//! Real ext-session-lock clients; runtime PID/PAM policy is tested separately.
use smithay::output::{Mode, Output, PhysicalProperties, Scale, Subpixel};
use smithay::reexports::wayland_server::Resource;
use std::collections::HashMap;
use std::os::unix::net::UnixStream;
use tuna_compositor::TestCompositor;
use wayland_client::protocol::{
    wl_compositor::WlCompositor,
    wl_output::WlOutput,
    wl_registry::{self, WlRegistry},
    wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols::ext::session_lock::v1::client::{
    ext_session_lock_manager_v1::ExtSessionLockManagerV1,
    ext_session_lock_surface_v1::{self, ExtSessionLockSurfaceV1},
    ext_session_lock_v1::{self, ExtSessionLockV1},
};
use wayland_protocols::wp::presentation_time::client::{
    wp_presentation::WpPresentation,
    wp_presentation_feedback::{self, WpPresentationFeedback},
};

#[derive(Default)]
struct Client {
    globals: HashMap<String, (u32, u32)>,
    /// Every wl_output global name, in announcement order.
    outputs: Vec<u32>,
    events: Vec<(u32, bool)>,
    configured: Vec<(u32, u32)>,
    /// Presentation feedback by tag: Some(seq) presented, None discarded.
    presented: HashMap<u32, Option<u64>>,
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
            if interface == "wl_output" {
                state.outputs.push(name);
            }
            state.globals.insert(interface, (name, version));
        }
    }
}
impl Dispatch<ExtSessionLockV1, u32> for Client {
    fn event(
        state: &mut Self,
        _: &ExtSessionLockV1,
        event: ext_session_lock_v1::Event,
        tag: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_session_lock_v1::Event::Locked => state.events.push((*tag, true)),
            ext_session_lock_v1::Event::Finished => state.events.push((*tag, false)),
            _ => (),
        }
    }
}
impl Dispatch<ExtSessionLockSurfaceV1, ()> for Client {
    fn event(
        state: &mut Self,
        surface: &ExtSessionLockSurfaceV1,
        event: ext_session_lock_surface_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let ext_session_lock_surface_v1::Event::Configure {
            serial,
            width,
            height,
        } = event
        {
            surface.ack_configure(serial);
            state.configured.push((width, height));
        }
    }
}
impl Dispatch<WpPresentationFeedback, u32> for Client {
    fn event(
        state: &mut Self,
        _: &WpPresentationFeedback,
        event: wp_presentation_feedback::Event,
        tag: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wp_presentation_feedback::Event::Presented { seq_hi, seq_lo, .. } => {
                let seq = (u64::from(seq_hi) << 32) | u64::from(seq_lo);
                state.presented.insert(*tag, Some(seq));
            }
            wp_presentation_feedback::Event::Discarded => {
                state.presented.insert(*tag, None);
            }
            _ => {}
        }
    }
}
wayland_client::delegate_noop!(Client: ignore WlCompositor);
wayland_client::delegate_noop!(Client: ignore WlOutput);
wayland_client::delegate_noop!(Client: ignore WlSurface);
wayland_client::delegate_noop!(Client: ignore ExtSessionLockManagerV1);
wayland_client::delegate_noop!(Client: ignore WpPresentation);

struct Peer {
    conn: Connection,
    queue: EventQueue<Client>,
    registry: WlRegistry,
    client: Client,
}
impl Peer {
    fn bind<I>(&self, version: u32) -> I
    where
        I: Proxy + 'static,
        Client: Dispatch<I, ()>,
    {
        let (name, offered) = self.client.globals[I::interface().name];
        self.registry
            .bind(name, version.min(offered), &self.queue.handle(), ())
    }
}
fn pump(comp: &mut TestCompositor, peer: &mut Peer) {
    for _ in 0..10 {
        peer.queue.flush().unwrap();
        comp.pump();
        if let Some(guard) = peer.queue.prepare_read() {
            let _ = guard.read();
        }
        peer.queue.dispatch_pending(&mut peer.client).unwrap();
    }
}
fn connect(comp: &mut TestCompositor) -> Peer {
    let (server, stream) = UnixStream::pair().unwrap();
    comp.add_client(server);
    let conn = Connection::from_socket(stream).unwrap();
    let queue = conn.new_event_queue();
    let registry = conn.display().get_registry(&queue.handle(), ());
    let mut peer = Peer {
        conn,
        queue,
        registry,
        client: Client::default(),
    };
    pump(comp, &mut peer);
    peer
}
fn compositor() -> TestCompositor {
    let mut comp = TestCompositor::new();
    let output = Output::new(
        "fixture".into(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "Tuna Desktop".into(),
            model: "Test".into(),
        },
    );
    output.change_current_state(
        Some(Mode {
            size: (1500, 900).into(),
            refresh: 60_000,
        }),
        None,
        Some(Scale::Fractional(1.5)),
        Some((0, 0).into()),
    );
    output.create_global::<tuna_compositor::State>(&comp.display.handle());
    comp.state.add_output("fixture", Some(output), 1500, 900);
    comp
}

#[test]
fn pending_lock_is_not_granted_and_denial_reports_finished() {
    let mut comp = compositor();
    let mut peer = connect(&mut comp);
    let manager: ExtSessionLockManagerV1 = peer.bind(1);
    let lock = manager.lock(&peer.queue.handle(), 1);
    pump(&mut comp, &mut peer);
    assert!(
        peer.client.events.is_empty(),
        "client cannot approve its own request"
    );
    let (pending, pid) = comp
        .state
        .take_lock_request()
        .expect("actual protocol request");
    assert_eq!(
        pid,
        Some(std::process::id() as i32),
        "SO_PEERCRED sender PID"
    );
    assert!(comp.state.take_lock_request().is_none());
    drop(pending);
    pump(&mut comp, &mut peer);
    assert_eq!(peer.client.events, vec![(1, false)]);
    assert!(!comp.state.has_lock_surfaces());
    assert!(!comp.state.take_client_unlock());
    lock.destroy();
}

#[test]
fn approved_lock_configures_scaled_output_and_unlock_only_requests_release() {
    let mut comp = compositor();
    let mut peer = connect(&mut comp);
    let manager: ExtSessionLockManagerV1 = peer.bind(1);
    let lock = manager.lock(&peer.queue.handle(), 2);
    pump(&mut comp, &mut peer);
    let pending = comp.state.take_lock_request().unwrap().0;
    comp.state.resolve_lock_request(pending, true);
    pump(&mut comp, &mut peer);
    assert_eq!(peer.client.events, vec![(2, true)]);
    let compositor: WlCompositor = peer.bind(6);
    let output: WlOutput = peer.bind(4);
    let surface = compositor.create_surface(&peer.queue.handle(), ());
    let role = lock.get_lock_surface(&surface, &output, &peer.queue.handle(), ());
    pump(&mut comp, &mut peer);
    assert_eq!(
        peer.client.configured,
        vec![(1000, 600)],
        "physical mode divided by scale"
    );
    assert!(comp.state.has_lock_surfaces());
    assert!(comp.state.lock_surface_for("fixture").is_some());
    assert!(comp.state.lock_surface_for("missing").is_none());
    assert_eq!(comp.state.lock_surfaces().len(), 1);
    assert!(!comp.state.take_client_unlock());
    lock.unlock_and_destroy();
    pump(&mut comp, &mut peer);
    assert!(
        comp.state.take_client_unlock(),
        "notification, not PAM authentication"
    );
    assert!(
        !comp.state.take_client_unlock(),
        "request consumed exactly once"
    );
    assert!(!comp.state.has_lock_surfaces());
    assert!(comp.state.lock_surfaces().is_empty());
    role.destroy();
    surface.destroy();
}

#[test]
fn replacing_pending_requests_finishes_old_owners_without_granting_them() {
    let mut comp = compositor();
    let mut peer = connect(&mut comp);
    let manager: ExtSessionLockManagerV1 = peer.bind(1);
    let locks: Vec<_> = (0..64)
        .map(|tag| manager.lock(&peer.queue.handle(), tag))
        .collect();
    pump(&mut comp, &mut peer);
    assert_eq!(peer.client.events.len(), 63);
    assert!(peer.client.events.iter().all(|(_, granted)| !granted));
    let (last, _) = comp.state.take_lock_request().unwrap();
    assert!(
        comp.state.take_lock_request().is_none(),
        "only one pending owner retained"
    );
    drop(last);
    pump(&mut comp, &mut peer);
    assert_eq!(peer.client.events.len(), 64);
    assert!(!comp.state.has_lock_surfaces());
    for lock in locks {
        lock.destroy();
    }
    peer.conn.flush().unwrap();
}

#[test]
fn disconnected_lock_client_leaves_no_live_surface_or_unlock_request() {
    let mut comp = compositor();
    let mut peer = connect(&mut comp);
    let manager: ExtSessionLockManagerV1 = peer.bind(1);
    let lock = manager.lock(&peer.queue.handle(), 3);
    pump(&mut comp, &mut peer);
    let pending = comp.state.take_lock_request().unwrap().0;
    comp.state.resolve_lock_request(pending, true);
    pump(&mut comp, &mut peer);
    let compositor: WlCompositor = peer.bind(6);
    let output: WlOutput = peer.bind(4);
    let surface = compositor.create_surface(&peer.queue.handle(), ());
    let role = lock.get_lock_surface(&surface, &output, &peer.queue.handle(), ());
    pump(&mut comp, &mut peer);
    assert!(comp.state.has_lock_surfaces());
    drop((role, surface, output, compositor, lock, manager, peer));
    for _ in 0..10 {
        comp.pump();
    }
    assert!(!comp.state.has_lock_surfaces());
    assert!(comp.state.lock_surfaces().is_empty());
    assert!(comp.state.lock_surface_for("fixture").is_none());
    assert!(
        !comp.state.take_client_unlock(),
        "client death is never authentication"
    );
}

fn pending_surface(
    comp: &mut TestCompositor,
    peer: &mut Peer,
    tag: u32,
) -> (ExtSessionLockV1, ExtSessionLockSurfaceV1, WlSurface) {
    let manager: ExtSessionLockManagerV1 = peer.bind(1);
    let compositor: WlCompositor = peer.bind(6);
    let output: WlOutput = peer.bind(4);
    let lock = manager.lock(&peer.queue.handle(), tag);
    let surface = compositor.create_surface(&peer.queue.handle(), ());
    let role = lock.get_lock_surface(&surface, &output, &peer.queue.handle(), ());
    pump(comp, peer);
    (lock, role, surface)
}

#[test]
fn early_surface_becomes_visible_only_after_approval() {
    let mut comp = compositor();
    let mut peer = connect(&mut comp);
    let (_lock, _role, _surface) = pending_surface(&mut comp, &mut peer, 20);
    assert_eq!(peer.client.configured, vec![(1000, 600)]);
    assert!(!comp.state.has_lock_surfaces());
    assert!(comp.state.lock_surface_for("fixture").is_none());
    assert!(comp.state.lock_surfaces().is_empty());
    let pending = comp.state.take_lock_request().unwrap().0;
    comp.state.resolve_lock_request(pending, true);
    pump(&mut comp, &mut peer);
    assert_eq!(peer.client.events, vec![(20, true)]);
    assert!(comp.state.has_lock_surfaces());
    assert_eq!(comp.state.lock_surfaces().len(), 1);
}

#[test]
fn denied_early_surface_never_becomes_a_focus_or_render_target() {
    let mut comp = compositor();
    let mut peer = connect(&mut comp);
    let (_lock, _role, _surface) = pending_surface(&mut comp, &mut peer, 21);
    let pending = comp.state.take_lock_request().unwrap().0;
    comp.state.resolve_lock_request(pending, false);
    pump(&mut comp, &mut peer);
    assert_eq!(peer.client.events, vec![(21, false)]);
    assert!(!comp.state.has_lock_surfaces());
    assert!(comp.state.lock_surface_for("fixture").is_none());
    assert!(comp.state.lock_surfaces().is_empty());
    assert!(!comp.state.take_client_unlock());
}

#[test]
fn rejected_second_client_preserves_the_approved_output_surface() {
    let mut comp = compositor();
    let mut owner = connect(&mut comp);
    let (_owner_lock, _owner_role, _owner_surface) = pending_surface(&mut comp, &mut owner, 22);
    let pending = comp.state.take_lock_request().unwrap().0;
    comp.state.resolve_lock_request(pending, true);
    pump(&mut comp, &mut owner);
    let original = comp.state.lock_surface_for("fixture").unwrap();
    let mut other = connect(&mut comp);
    let (_other_lock, _other_role, _other_surface) = pending_surface(&mut comp, &mut other, 23);
    assert_eq!(comp.state.lock_surface_for("fixture").unwrap(), original);
    assert_eq!(comp.state.lock_surfaces(), vec![original.clone()]);
    let pending = comp.state.take_lock_request().unwrap().0;
    comp.state.resolve_lock_request(pending, false);
    pump(&mut comp, &mut other);
    assert_eq!(other.client.events, vec![(23, false)]);
    assert_eq!(comp.state.lock_surface_for("fixture").unwrap(), original);
    assert_eq!(comp.state.lock_surfaces(), vec![original]);
    assert!(!comp.state.take_client_unlock());
}

#[test]
fn unapproved_release_is_a_protocol_error_without_clearing_the_owner() {
    let mut comp = compositor();
    let mut owner = connect(&mut comp);
    let (_owner_lock, _owner_role, _owner_surface) = pending_surface(&mut comp, &mut owner, 24);
    let pending = comp.state.take_lock_request().unwrap().0;
    comp.state.resolve_lock_request(pending, true);
    pump(&mut comp, &mut owner);
    let original = comp.state.lock_surface_for("fixture").unwrap();
    let mut other = connect(&mut comp);
    let manager: ExtSessionLockManagerV1 = other.bind(1);
    let unapproved = manager.lock(&other.queue.handle(), 25);
    pump(&mut comp, &mut other);
    let pending = comp.state.take_lock_request().unwrap().0;
    comp.state.resolve_lock_request(pending, false);
    pump(&mut comp, &mut other);
    unapproved.unlock_and_destroy();
    other.queue.flush().unwrap();
    pump(&mut comp, &mut owner);
    if let Some(guard) = other.queue.prepare_read() {
        let _ = guard.read();
    }
    let _ = other.queue.dispatch_pending(&mut other.client);
    assert!(other.conn.protocol_error().is_some());
    assert_eq!(comp.state.lock_surface_for("fixture").unwrap(), original);
    assert_eq!(comp.state.lock_surfaces(), vec![original]);
    assert!(!comp.state.take_client_unlock());
}

fn read_errors(peer: &mut Peer) -> Option<wayland_client::backend::protocol::ProtocolError> {
    let _ = peer.queue.flush();
    if let Some(guard) = peer.queue.prepare_read() {
        let _ = guard.read();
    }
    let _ = peer.queue.dispatch_pending(&mut peer.client);
    peer.conn.protocol_error()
}

#[test]
fn a_finished_lock_does_not_reserve_its_output_for_the_next_lock() {
    // The shell creates lock surfaces before Locked (GTK does). If that
    // first request is refused (lockdown policy) and the shell later
    // locks again with its same wl_output, the retry must work: a stale
    // reservation would be a duplicate_output error, disconnecting the
    // whole shell.
    let mut comp = compositor();
    let mut peer = connect(&mut comp);
    let manager: ExtSessionLockManagerV1 = peer.bind(1);
    let compositor: WlCompositor = peer.bind(6);
    let output: WlOutput = peer.bind(4);
    let first = manager.lock(&peer.queue.handle(), 30);
    let first_surface = compositor.create_surface(&peer.queue.handle(), ());
    let first_role = first.get_lock_surface(&first_surface, &output, &peer.queue.handle(), ());
    pump(&mut comp, &mut peer);
    let pending = comp.state.take_lock_request().unwrap().0;
    comp.state.resolve_lock_request(pending, false);
    pump(&mut comp, &mut peer);
    assert_eq!(peer.client.events, vec![(30, false)]);
    first_role.destroy();
    first_surface.destroy();
    first.destroy();

    let second = manager.lock(&peer.queue.handle(), 31);
    let surface = compositor.create_surface(&peer.queue.handle(), ());
    let _role = second.get_lock_surface(&surface, &output, &peer.queue.handle(), ());
    pump(&mut comp, &mut peer);
    assert!(
        read_errors(&mut peer).is_none(),
        "retry must not be a protocol error"
    );
    let pending = comp.state.take_lock_request().unwrap().0;
    comp.state.resolve_lock_request(pending, true);
    pump(&mut comp, &mut peer);
    assert_eq!(peer.client.events, vec![(30, false), (31, true)]);
    assert_eq!(
        comp.state
            .lock_surface_for("fixture")
            .as_ref()
            .map(|s| s.id().protocol_id()),
        Some(surface.id().protocol_id())
    );
}

#[test]
fn a_superseded_pending_lock_does_not_reserve_its_output_for_its_replacement() {
    // Two lock triggers in quick succession (idle, then Super+L): the
    // second request finishes the first, and the replacement's lock
    // surface for the same output must be accepted.
    let mut comp = compositor();
    let mut peer = connect(&mut comp);
    let manager: ExtSessionLockManagerV1 = peer.bind(1);
    let compositor: WlCompositor = peer.bind(6);
    let output: WlOutput = peer.bind(4);
    let first = manager.lock(&peer.queue.handle(), 40);
    let first_surface = compositor.create_surface(&peer.queue.handle(), ());
    let _first_role = first.get_lock_surface(&first_surface, &output, &peer.queue.handle(), ());
    pump(&mut comp, &mut peer);
    let second = manager.lock(&peer.queue.handle(), 41);
    pump(&mut comp, &mut peer);
    assert_eq!(peer.client.events, vec![(40, false)]);
    let surface = compositor.create_surface(&peer.queue.handle(), ());
    let _role = second.get_lock_surface(&surface, &output, &peer.queue.handle(), ());
    pump(&mut comp, &mut peer);
    assert!(read_errors(&mut peer).is_none());
    let pending = comp.state.take_lock_request().unwrap().0;
    comp.state.resolve_lock_request(pending, true);
    pump(&mut comp, &mut peer);
    assert_eq!(peer.client.events, vec![(40, false), (41, true)]);
    assert_eq!(comp.state.lock_surfaces().len(), 1);
    assert_eq!(
        comp.state
            .lock_surface_for("fixture")
            .as_ref()
            .map(|s| s.id().protocol_id()),
        Some(surface.id().protocol_id())
    );
}

#[test]
fn one_lock_object_cannot_cover_one_output_twice() {
    // The per-lock duplicate_output rule still holds: two curtains for
    // one output would leave the compositor guessing which to show.
    let mut comp = compositor();
    let mut peer = connect(&mut comp);
    let manager: ExtSessionLockManagerV1 = peer.bind(1);
    let compositor: WlCompositor = peer.bind(6);
    let output: WlOutput = peer.bind(4);
    let lock = manager.lock(&peer.queue.handle(), 50);
    let first = compositor.create_surface(&peer.queue.handle(), ());
    let _first_role = lock.get_lock_surface(&first, &output, &peer.queue.handle(), ());
    let second = compositor.create_surface(&peer.queue.handle(), ());
    let _second_role = lock.get_lock_surface(&second, &output, &peer.queue.handle(), ());
    peer.queue.flush().unwrap();
    for _ in 0..10 {
        comp.pump();
    }
    let error = read_errors(&mut peer).expect("duplicate_output");
    assert_eq!(error.code, 3, "ext_session_lock_v1.error.duplicate_output");
    assert!(!comp.state.has_lock_surfaces());
}

fn add_output(comp: &mut TestCompositor, name: &str, size: (i32, i32), scale: f64) {
    let output = Output::new(
        name.into(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "Tuna Desktop".into(),
            model: "Test".into(),
        },
    );
    output.change_current_state(
        Some(Mode {
            size: size.into(),
            refresh: 60_000,
        }),
        None,
        Some(Scale::Fractional(scale)),
        None,
    );
    output.create_global::<tuna_compositor::State>(&comp.display.handle());
    let logical = (
        (f64::from(size.0) / scale).round() as i32,
        (f64::from(size.1) / scale).round() as i32,
    );
    comp.state
        .add_output(name, Some(output), logical.0, logical.1);
}

#[test]
fn every_output_gets_its_own_curtain_sized_to_that_output() {
    // A lock surface smaller than its output would leave session
    // content visible around it, and one keyed to the wrong output
    // would leave another monitor uncovered.
    let mut comp = compositor();
    add_output(&mut comp, "second", (3840, 2160), 2.0);
    let mut peer = connect(&mut comp);
    assert_eq!(peer.client.outputs.len(), 2);
    let manager: ExtSessionLockManagerV1 = peer.bind(1);
    let compositor: WlCompositor = peer.bind(6);
    let outputs: Vec<WlOutput> = peer
        .client
        .outputs
        .clone()
        .into_iter()
        .map(|name| peer.registry.bind(name, 4, &peer.queue.handle(), ()))
        .collect();
    pump(&mut comp, &mut peer);
    let lock = manager.lock(&peer.queue.handle(), 60);
    let surfaces: Vec<WlSurface> = outputs
        .iter()
        .map(|output| {
            let surface = compositor.create_surface(&peer.queue.handle(), ());
            let role = lock.get_lock_surface(&surface, output, &peer.queue.handle(), ());
            std::mem::forget(role);
            surface
        })
        .collect();
    pump(&mut comp, &mut peer);
    let pending = comp.state.take_lock_request().unwrap().0;
    comp.state.resolve_lock_request(pending, true);
    pump(&mut comp, &mut peer);
    assert!(read_errors(&mut peer).is_none());
    // Logical size: physical mode over the output's own scale.
    let mut configured = peer.client.configured.clone();
    configured.sort();
    assert_eq!(configured, vec![(1000, 600), (1920, 1080)]);
    let fixture = comp.state.lock_surface_for("fixture").unwrap();
    let second = comp.state.lock_surface_for("second").unwrap();
    assert_ne!(fixture, second);
    let ids: Vec<u32> = surfaces.iter().map(|s| s.id().protocol_id()).collect();
    assert!(ids.contains(&fixture.id().protocol_id()));
    assert!(ids.contains(&second.id().protocol_id()));
    assert_eq!(comp.state.lock_surfaces().len(), 2);
}

#[test]
fn destroying_a_granted_lock_without_unlock_is_an_error_and_never_unlocks() {
    // ext-session-lock: a client that crashes or misbehaves while the
    // session is locked must leave it locked (the compositor keeps its
    // own curtain up), never reveal the session.
    let mut comp = compositor();
    let mut peer = connect(&mut comp);
    let (lock, _role, _surface) = pending_surface(&mut comp, &mut peer, 70);
    let pending = comp.state.take_lock_request().unwrap().0;
    comp.state.resolve_lock_request(pending, true);
    pump(&mut comp, &mut peer);
    assert_eq!(peer.client.events, vec![(70, true)]);
    lock.destroy();
    peer.queue.flush().unwrap();
    for _ in 0..10 {
        comp.pump();
    }
    let error = read_errors(&mut peer).expect("invalid_destroy");
    assert_eq!(error.code, 0, "ext_session_lock_v1.error.invalid_destroy");
    for _ in 0..10 {
        comp.pump();
    }
    assert!(
        !comp.state.take_client_unlock(),
        "a destroyed lock is not an unlock request"
    );
    assert!(comp.state.take_lock_request().is_none());
}

#[test]
fn unlock_before_locked_is_rejected_and_requests_nothing() {
    let mut comp = compositor();
    let mut peer = connect(&mut comp);
    let manager: ExtSessionLockManagerV1 = peer.bind(1);
    let lock = manager.lock(&peer.queue.handle(), 80);
    pump(&mut comp, &mut peer);
    // Still pending: the runtime has not decided.
    lock.unlock_and_destroy();
    peer.queue.flush().unwrap();
    for _ in 0..10 {
        comp.pump();
    }
    let error = read_errors(&mut peer).expect("invalid_unlock");
    assert_eq!(error.code, 1, "ext_session_lock_v1.error.invalid_unlock");
    assert!(!comp.state.take_client_unlock());
    assert!(!comp.state.has_lock_surfaces());
}

#[test]
fn relocking_after_unlock_needs_a_fresh_approval() {
    // The owner of a previous lock holds no standing grant: its next
    // lock and lock surfaces stay hidden until the runtime approves
    // that request too.
    let mut comp = compositor();
    let mut peer = connect(&mut comp);
    let (lock, role, surface) = pending_surface(&mut comp, &mut peer, 90);
    let pending = comp.state.take_lock_request().unwrap().0;
    comp.state.resolve_lock_request(pending, true);
    pump(&mut comp, &mut peer);
    lock.unlock_and_destroy();
    role.destroy();
    surface.destroy();
    pump(&mut comp, &mut peer);
    assert!(comp.state.take_client_unlock());
    assert!(!comp.state.has_lock_surfaces());

    let (_lock, _role, surface) = pending_surface(&mut comp, &mut peer, 91);
    assert!(
        read_errors(&mut peer).is_none(),
        "same output, new lock object"
    );
    assert!(
        !comp.state.has_lock_surfaces(),
        "the previous approval does not carry over"
    );
    assert_eq!(peer.client.events, vec![(90, true)]);
    let pending = comp.state.take_lock_request().unwrap().0;
    comp.state.resolve_lock_request(pending, true);
    pump(&mut comp, &mut peer);
    assert_eq!(peer.client.events, vec![(90, true), (91, true)]);
    assert_eq!(
        comp.state
            .lock_surface_for("fixture")
            .map(|s| s.id().protocol_id()),
        Some(surface.id().protocol_id())
    );
    assert!(
        !comp.state.take_client_unlock(),
        "the earlier unlock request was consumed and the new grant reset it"
    );
}

#[test]
fn only_the_approved_lock_screen_is_presented_while_locked() {
    // Frames drawn while locked show the lock surface and nothing else:
    // a pending (unapproved) curtain is not drawn, so its presentation
    // feedback must not claim it reached the screen.
    let mut comp = compositor();
    let manager = comp.window_manager();
    let mut peer = connect(&mut comp);
    let presentation: WpPresentation = peer.bind(2);
    let (_lock, _role, surface) = pending_surface(&mut comp, &mut peer, 100);
    let _feedback = presentation.feedback(&surface, &peer.queue.handle(), 1);
    surface.commit();
    pump(&mut comp, &mut peer);
    assert!(read_errors(&mut peer).is_none());
    tuna_compositor::frame_timing::present_nested_frame(&mut comp.state, &manager, true, 1);
    pump(&mut comp, &mut peer);
    assert_eq!(peer.client.presented.get(&1), None, "not drawn yet");

    let pending = comp.state.take_lock_request().unwrap().0;
    comp.state.resolve_lock_request(pending, true);
    pump(&mut comp, &mut peer);
    tuna_compositor::frame_timing::present_nested_frame(&mut comp.state, &manager, true, 2);
    pump(&mut comp, &mut peer);
    assert_eq!(peer.client.presented.get(&1), Some(&Some(2)));
}
