//! xdg-session-management v1 (P-SY-08, #338) against real Wayland
//! clients: a session's windows come back where they were after the
//! compositor restarts, only for the clients that ask, with Mutter 51's
//! protocol errors. The compositor restart is a second `TestCompositor`
//! reading the store the first one wrote at exit; the nested proof
//! (`scripts/tuna-session-restore-proof`) restarts the real binary.

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use serde_json::Value;
use tuna_compositor::windows::{TileSide, WindowLayout, WindowManager};
use tuna_compositor::TestCompositor;
use wayland_client::{
    protocol::{
        wl_compositor::WlCompositor,
        wl_registry::{self, WlRegistry},
        wl_surface::WlSurface,
    },
    Connection, Dispatch, EventQueue, QueueHandle,
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::{self, XdgToplevel},
    xdg_wm_base::{self, XdgWmBase},
};

#[allow(
    dead_code,
    unused_imports,
    non_camel_case_types,
    non_upper_case_globals,
    missing_docs,
    clippy::all
)]
mod protocol {
    use wayland_client;
    use wayland_client::protocol::*;
    use wayland_protocols::xdg::shell::client::*;
    pub mod __interfaces {
        use wayland_client::backend as wayland_backend;
        use wayland_client::protocol::__interfaces::*;
        use wayland_protocols::xdg::shell::client::__interfaces::*;
        wayland_scanner::generate_interfaces!("protocols/xdg-session-management-v1.xml");
    }
    use self::__interfaces::*;
    wayland_scanner::generate_client_code!("protocols/xdg-session-management-v1.xml");
}
use protocol::{
    xdg_session_manager_v1::{self, XdgSessionManagerV1},
    xdg_session_v1::{self, XdgSessionV1},
    xdg_toplevel_session_v1::{self, XdgToplevelSessionV1},
};

const ROUNDS: usize = 10;
const WIDTH: i32 = 1280;
const HEIGHT: i32 = 800;
const PANEL: i32 = tuna_shell_control::PANEL_HEIGHT as i32;

#[derive(Default)]
struct App {
    globals: Vec<(String, u32)>,
    compositor: Option<WlCompositor>,
    wm: Option<XdgWmBase>,
    sessions: Option<XdgSessionManagerV1>,
    created: Option<String>,
    /// Every event that matters, in arrival order.
    events: Vec<String>,
}

impl Dispatch<WlRegistry, ()> for App {
    fn event(
        app: &mut Self,
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
                "wl_compositor" => app.compositor = Some(registry.bind(name, 4, qh, ())),
                "xdg_wm_base" => app.wm = Some(registry.bind(name, 1, qh, ())),
                "xdg_session_manager_v1" => app.sessions = Some(registry.bind(name, 1, qh, ())),
                _ => {}
            }
            app.globals.push((interface, version));
        }
    }
}

impl Dispatch<XdgWmBase, ()> for App {
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

impl Dispatch<XdgSurface, ()> for App {
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

impl Dispatch<XdgToplevel, String> for App {
    fn event(
        app: &mut Self,
        _: &XdgToplevel,
        event: xdg_toplevel::Event,
        name: &String,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_toplevel::Event::Configure { width, height, .. } = event {
            app.events
                .push(format!("configure {name} {width}x{height}"));
        }
    }
}

impl Dispatch<XdgSessionV1, ()> for App {
    fn event(
        app: &mut Self,
        _: &XdgSessionV1,
        event: xdg_session_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            xdg_session_v1::Event::Created { session_id } => {
                app.events.push("session created".into());
                app.created = Some(session_id);
            }
            xdg_session_v1::Event::Restored => app.events.push("session restored".into()),
            xdg_session_v1::Event::Replaced => app.events.push("session replaced".into()),
        }
    }
}

impl Dispatch<XdgToplevelSessionV1, String> for App {
    fn event(
        app: &mut Self,
        _: &XdgToplevelSessionV1,
        event: xdg_toplevel_session_v1::Event,
        name: &String,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let xdg_toplevel_session_v1::Event::Restored = event;
        app.events.push(format!("restored {name}"));
    }
}

wayland_client::delegate_noop!(App: ignore WlCompositor);
wayland_client::delegate_noop!(App: ignore WlSurface);
wayland_client::delegate_noop!(App: ignore XdgSessionManagerV1);

struct Peer {
    conn: Connection,
    queue: EventQueue<App>,
    app: App,
}

impl Peer {
    fn qh(&self) -> QueueHandle<App> {
        self.queue.handle()
    }

    fn get_session(
        &self,
        reason: xdg_session_manager_v1::Reason,
        id: Option<&str>,
    ) -> XdgSessionV1 {
        self.app
            .sessions
            .as_ref()
            .expect("xdg_session_manager_v1 advertised")
            .get_session(reason, id.map(str::to_owned), &self.qh(), ())
    }

    /// A toplevel named (and titled) `name`, not yet committed.
    fn toplevel(&self, name: &str) -> (WlSurface, XdgToplevel) {
        let (surface, xdg, toplevel) = self.window(name);
        std::mem::forget(xdg);
        (surface, toplevel)
    }

    /// The same, keeping its xdg_surface so it can be closed in order.
    fn window(&self, name: &str) -> (WlSurface, XdgSurface, XdgToplevel) {
        let qh = self.qh();
        let surface = self
            .app
            .compositor
            .as_ref()
            .unwrap()
            .create_surface(&qh, ());
        let xdg = self
            .app
            .wm
            .as_ref()
            .unwrap()
            .get_xdg_surface(&surface, &qh, ());
        let toplevel = xdg.get_toplevel(&qh, name.to_owned());
        toplevel.set_title(name.to_owned());
        (surface, xdg, toplevel)
    }

    fn position(&self, event: &str) -> Option<usize> {
        self.app.events.iter().position(|seen| seen == event)
    }

    fn first_configure(&self, name: &str) -> Option<(usize, String)> {
        let prefix = format!("configure {name} ");
        self.app
            .events
            .iter()
            .enumerate()
            .find(|(_, seen)| seen.starts_with(&prefix))
            .map(|(at, seen)| (at, seen[prefix.len()..].to_owned()))
    }

    /// The protocol error the compositor posted, as `(interface, code)`.
    fn error(&self) -> Option<(String, u32)> {
        self.conn
            .protocol_error()
            .map(|e| (e.object_interface, e.code))
    }
}

struct Session {
    comp: TestCompositor,
    manager: WindowManager,
}

impl Session {
    /// A compositor with one 1280x800 output whose window sessions are
    /// stored at `store`.
    fn start(store: &Path) -> Self {
        let mut comp = TestCompositor::new();
        comp.state.set_output_size(WIDTH, HEIGHT);
        comp.state
            .window_sessions_mut()
            .attach_store(store.to_path_buf());
        let manager = comp.window_manager();
        Self { comp, manager }
    }

    fn connect(&mut self) -> Peer {
        let (server, stream) = UnixStream::pair().unwrap();
        self.comp.add_client(server);
        let conn = Connection::from_socket(stream).unwrap();
        let queue = conn.new_event_queue();
        conn.display().get_registry(&queue.handle(), ());
        let mut peer = Peer {
            conn,
            queue,
            app: App::default(),
        };
        self.pump(&mut [&mut peer]);
        peer
    }

    fn pump(&mut self, peers: &mut [&mut Peer]) {
        for _ in 0..ROUNDS {
            for peer in peers.iter_mut() {
                let _ = peer.queue.flush();
            }
            self.comp.pump();
            self.manager.reconcile(&mut self.comp.state);
            self.comp.pump();
            for peer in peers.iter_mut() {
                if let Some(guard) = peer.queue.prepare_read() {
                    let _ = guard.read();
                }
                let _ = peer.queue.dispatch_pending(&mut peer.app);
            }
        }
    }

    fn id(&self, title: &str) -> u64 {
        self.manager
            .model()
            .windows()
            .find(|w| w.title == title)
            .unwrap_or_else(|| panic!("window {title} mapped"))
            .id
    }

    fn workspace(&self, id: u64) -> u32 {
        self.manager.model().window(id).unwrap().workspace
    }

    fn rect(&self, id: u64) -> [i32; 4] {
        let g = self.manager.geometry(id).unwrap();
        [g.loc.x, g.loc.y, g.size.w, g.size.h]
    }

    /// Exit: record every session window and write the store.
    fn exit(mut self) {
        self.manager.save_window_sessions(&mut self.comp.state);
    }
}

fn store_in(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join("tuna/window-sessions.json")
}

fn stored(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// Place `main` at (200, 150) 640x480 on the second workspace, maximize
/// `aux`, tile `side` left and minimize it; exit. Returns the session id.
fn first_run(store: &Path) -> String {
    let mut run = Session::start(store);
    let mut peer = run.connect();
    let session = peer.get_session(xdg_session_manager_v1::Reason::Launch, None);
    let qh = peer.qh();
    let mut keep = Vec::new();
    for name in ["main", "aux", "side"] {
        let (surface, toplevel) = peer.toplevel(name);
        keep.push(session.add_toplevel(&toplevel, name.to_owned(), &qh, name.to_owned()));
        surface.commit();
        keep_alive(surface, toplevel);
    }
    run.pump(&mut [&mut peer]);
    assert_eq!(peer.position("session created"), Some(0));
    let id = peer.app.created.clone().expect("created carries the id");
    assert!(
        peer.position("restored main").is_none(),
        "added toplevels are never restored"
    );

    let main = run.id("main");
    let [x, y, _, _] = run.rect(main);
    assert!(run.manager.move_window(main, 200 - x, 150 - y));
    assert!(run.manager.resize_window(main, 640, 480));
    assert!(run.manager.move_to_workspace(&mut run.comp.state, main, 1));
    let aux = run.id("aux");
    assert!(run.manager.set_maximized(&mut run.comp.state, aux, true));
    let side = run.id("side");
    assert!(run
        .manager
        .set_tiled(&mut run.comp.state, side, TileSide::Left));
    assert!(run.manager.minimize(&mut run.comp.state, side));
    run.pump(&mut [&mut peer]);
    assert_eq!(run.rect(main), [200, 150, 640, 480]);
    run.exit();
    drop(keep);
    id
}

fn keep_alive(surface: WlSurface, toplevel: XdgToplevel) {
    std::mem::forget((surface, toplevel));
}

#[test]
fn a_restarted_compositor_puts_session_windows_back() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(&dir);
    let id = first_run(&store);

    // The file Mutter's keys and numbers describe.
    let saved = stored(&store);
    assert_eq!(saved["version"], 1);
    let toplevels = &saved["sessions"][&id]["toplevels"];
    assert_eq!(toplevels["main"]["state"], 1);
    assert_eq!(
        toplevels["main"]["floating-rect"],
        serde_json::json!([200, 150, 640, 480])
    );
    assert_eq!(toplevels["main"]["workspace"], 1);
    assert_eq!(toplevels["main"]["output"], "tuna-0");
    assert_eq!(toplevels["aux"]["state"], 2);
    assert_eq!(
        toplevels["aux"]["tiled-rect"],
        serde_json::json!([0, PANEL, WIDTH, HEIGHT - PANEL])
    );
    assert_eq!(toplevels["side"]["state"], 3);
    assert_eq!(toplevels["side"]["is-minimized"], true);

    // Restart: the client comes back with its id and asks for each
    // window before its first commit.
    let mut run = Session::start(&store);
    let mut peer = run.connect();
    let session = peer.get_session(xdg_session_manager_v1::Reason::SessionRestore, Some(&id));
    let qh = peer.qh();
    let mut keep = Vec::new();
    for name in ["main", "aux", "side"] {
        let (surface, toplevel) = peer.toplevel(name);
        keep.push(session.restore_toplevel(&toplevel, name.to_owned(), &qh, name.to_owned()));
        surface.commit();
        keep_alive(surface, toplevel);
    }
    // A window outside any session is placed as usual.
    let (plain, plain_top) = peer.toplevel("plain");
    plain.commit();
    keep_alive(plain, plain_top);
    run.pump(&mut [&mut peer]);

    assert_eq!(peer.position("session restored"), Some(0));
    assert!(peer.position("session created").is_none());
    assert!(peer.app.created.is_none());
    for name in ["main", "aux", "side"] {
        let restored = peer
            .position(&format!("restored {name}"))
            .expect("restored");
        let (configure, _) = peer.first_configure(name).expect("configured");
        assert!(
            restored < configure,
            "{name}: restored precedes its first configure"
        );
    }
    let (_, size) = peer.first_configure("main").unwrap();
    assert_eq!(
        size, "640x480",
        "the first configure carries the saved size"
    );

    let main = run.id("main");
    assert_eq!(run.rect(main), [200, 150, 640, 480]);
    assert_eq!(run.workspace(main), 1);
    assert_eq!(
        run.manager.window_layout(main),
        Some(WindowLayout::Floating)
    );
    assert_ne!(
        run.manager.model().focused(),
        Some(main),
        "not focused on a hidden workspace"
    );
    let aux = run.id("aux");
    assert_eq!(
        run.manager.window_layout(aux),
        Some(WindowLayout::Maximized)
    );
    assert_eq!(run.rect(aux), [0, PANEL, WIDTH, HEIGHT - PANEL]);
    assert_eq!(run.workspace(aux), 0);
    let side = run.id("side");
    assert_eq!(
        run.manager.window_layout(side),
        Some(WindowLayout::Tiled(TileSide::Left))
    );
    // Written, but as in Mutter 51 (whose parser never reads it back) a
    // restored window is not minimized.
    assert!(!run.manager.is_minimized(side));
    let plain = run.id("plain");
    assert!(peer.position("restored plain").is_none());
    assert_eq!(run.workspace(plain), 0);
    assert_eq!(run.manager.model().focused(), Some(plain));
    drop(keep);
}

#[test]
fn restoring_a_toplevel_already_mapped_but_not_committed_still_applies() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(&dir);
    let id = first_run(&store);
    let mut run = Session::start(&store);
    let mut peer = run.connect();
    let session = peer.get_session(xdg_session_manager_v1::Reason::Recover, Some(&id));
    let (surface, toplevel) = peer.toplevel("main");
    // The window manager maps the toplevel before the client asks.
    run.pump(&mut [&mut peer]);
    let main = run.id("main");
    assert_ne!(run.rect(main)[..2], [200, 150]);
    let handle = session.restore_toplevel(&toplevel, "main".into(), &peer.qh(), "main".into());
    surface.commit();
    run.pump(&mut [&mut peer]);
    assert!(peer.error().is_none());
    assert!(peer.position("restored main").is_some());
    assert_eq!(run.rect(main), [200, 150, 640, 480]);
    assert_eq!(run.workspace(main), 1);
    drop(handle);
    keep_alive(surface, toplevel);
}

#[test]
fn windows_record_their_state_when_they_close_not_when_their_session_goes() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(&dir);
    let mut run = Session::start(&store);
    let mut peer = run.connect();
    let session = peer.get_session(xdg_session_manager_v1::Reason::Launch, None);
    let qh = peer.qh();
    let (one, one_xdg, one_top) = peer.window("one");
    let one_handle = session.add_toplevel(&one_top, "one".into(), &qh, "one".into());
    one.commit();
    let (two, two_top) = peer.toplevel("two");
    let two_handle = session.add_toplevel(&two_top, "two".into(), &qh, "two".into());
    two.commit();
    run.pump(&mut [&mut peer]);
    let id = peer.app.created.clone().unwrap();
    let first = run.id("one");
    let [x, y, _, _] = run.rect(first);
    assert!(run.manager.move_window(first, 300 - x, 200 - y));
    assert!(run.manager.resize_window(first, 500, 400));
    assert!(run.comp.state.window_sessions().save_deadline().is_none());

    // Closing a window records it (Mutter's unmanage) and schedules the
    // write three seconds out.
    let before = std::time::Instant::now();
    one_handle.destroy();
    one_top.destroy();
    one_xdg.destroy();
    one.destroy();
    run.pump(&mut [&mut peer]);
    let one = run
        .comp
        .state
        .window_sessions()
        .store()
        .toplevel(&id, "one")
        .cloned();
    assert_eq!(
        one.and_then(|t| t.floating_rect)
            .map(|r| (r.loc.x, r.loc.y, r.size.w, r.size.h)),
        Some((300, 200, 500, 400))
    );
    let due = run
        .comp
        .state
        .window_sessions()
        .save_deadline()
        .expect("a save is due");
    assert!(due >= before + tuna_compositor::xdg_session::SAVE_DELAY);
    run.comp
        .state
        .window_sessions_mut()
        .save_if_due(std::time::Instant::now());
    assert!(!store.exists(), "not before the delay");

    // The client goes away with its window still open: as in Mutter 51,
    // the session object detaches its toplevels before they go, so the
    // window's last state is not recorded.
    let second = run.id("two");
    assert!(run.manager.set_maximized(&mut run.comp.state, second, true));
    drop(two_handle);
    drop(peer);
    run.pump(&mut []);
    assert!(run.manager.model().windows().next().is_none());
    assert!(run
        .comp
        .state
        .window_sessions()
        .store()
        .toplevel(&id, "two")
        .is_none());
    run.comp
        .state
        .window_sessions_mut()
        .save_if_due(due + std::time::Duration::from_millis(1));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !store.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let saved = stored(&store);
    assert_eq!(
        saved["sessions"][&id]["toplevels"]["one"]["floating-rect"],
        serde_json::json!([300, 200, 500, 400])
    );
    assert!(saved["sessions"][&id]["toplevels"].get("two").is_none());
}

#[test]
fn unknown_ids_start_new_sessions_and_removal_forgets() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(&dir);
    let id = first_run(&store);
    let mut run = Session::start(&store);
    let mut peer = run.connect();
    let unknown = peer.get_session(
        xdg_session_manager_v1::Reason::SessionRestore,
        Some("not-a-session"),
    );
    run.pump(&mut [&mut peer]);
    let fresh = peer
        .app
        .created
        .clone()
        .expect("an unknown id is a new session");
    assert_ne!(fresh, "not-a-session");
    assert_ne!(fresh, id);
    unknown.destroy();

    // remove_toplevel forgets one name, rename carries another's state.
    let session = peer.get_session(xdg_session_manager_v1::Reason::SessionRestore, Some(&id));
    let qh = peer.qh();
    let (aux, aux_top) = peer.toplevel("aux");
    let aux_handle = session.restore_toplevel(&aux_top, "aux".into(), &qh, "aux".into());
    aux.commit();
    run.pump(&mut [&mut peer]);
    aux_handle.rename("aux-renamed".into());
    session.remove_toplevel("side".into());
    run.pump(&mut [&mut peer]);
    let sessions = run.comp.state.window_sessions().store();
    assert!(sessions.toplevel(&id, "side").is_none());
    assert!(sessions.toplevel(&id, "aux").is_none());
    assert_eq!(
        sessions.toplevel(&id, "aux-renamed").map(|t| t.state),
        Some(tuna_compositor::xdg_session::WindowState::Maximized)
    );

    // remove deletes the session: its id starts afresh next time.
    session.remove();
    run.pump(&mut [&mut peer]);
    assert!(!run.comp.state.window_sessions().store().contains(&id));
    peer.app.created = None;
    let _again = peer.get_session(xdg_session_manager_v1::Reason::SessionRestore, Some(&id));
    run.pump(&mut [&mut peer]);
    assert!(peer.app.created.as_deref().is_some_and(|new| new != id));
    assert!(peer.error().is_none());
    keep_alive(aux, aux_top);
}

#[test]
fn another_client_takes_a_session_over_and_the_same_client_cannot_twice() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(&dir);
    let id = first_run(&store);
    let mut run = Session::start(&store);
    let mut old = run.connect();
    let _held = old.get_session(xdg_session_manager_v1::Reason::SessionRestore, Some(&id));
    let mut new = run.connect();
    let _taken = new.get_session(xdg_session_manager_v1::Reason::SessionRestore, Some(&id));
    run.pump(&mut [&mut old, &mut new]);
    assert!(old.position("session replaced").is_some());
    assert!(new.position("session restored").is_some());
    assert!(old.error().is_none() && new.error().is_none());

    let _twice = new.get_session(xdg_session_manager_v1::Reason::SessionRestore, Some(&id));
    run.pump(&mut [&mut new]);
    assert_eq!(
        new.error(),
        Some((
            "xdg_session_manager_v1".into(),
            xdg_session_manager_v1::Error::InUse as u32
        ))
    );
}

/// One client's protocol error: `act` gets a fresh client holding the
/// restored session.
fn session_error(
    act: impl FnOnce(&mut Session, &mut Peer, &XdgSessionV1),
) -> Option<(String, u32)> {
    let dir = tempfile::tempdir().unwrap();
    let store = store_in(&dir);
    let id = first_run(&store);
    let mut run = Session::start(&store);
    let mut peer = run.connect();
    let session = peer.get_session(xdg_session_manager_v1::Reason::SessionRestore, Some(&id));
    run.pump(&mut [&mut peer]);
    act(&mut run, &mut peer, &session);
    run.pump(&mut [&mut peer]);
    peer.error()
}

#[test]
fn session_requests_raise_mutters_protocol_errors() {
    let name_in_use = Some((
        "xdg_session_v1".to_owned(),
        xdg_session_v1::Error::NameInUse as u32,
    ));
    // add_toplevel may not take a name the session already knows.
    let known = session_error(|_, peer, session| {
        let (surface, toplevel) = peer.toplevel("main");
        session.add_toplevel(&toplevel, "main".into(), &peer.qh(), "main".into());
        keep_alive(surface, toplevel);
    });
    assert_eq!(known, name_in_use);
    // Nor may two toplevels hold one name.
    let twice = session_error(|_, peer, session| {
        let qh = peer.qh();
        let (a, a_top) = peer.toplevel("a");
        let (b, b_top) = peer.toplevel("b");
        session.add_toplevel(&a_top, "fresh".into(), &qh, "a".into());
        session.restore_toplevel(&b_top, "fresh".into(), &qh, "b".into());
        keep_alive(a, a_top);
        keep_alive(b, b_top);
    });
    assert_eq!(twice, name_in_use);
    // A toplevel joins a session once.
    let added = session_error(|_, peer, session| {
        let qh = peer.qh();
        let (a, a_top) = peer.toplevel("a");
        session.add_toplevel(&a_top, "first".into(), &qh, "a".into());
        session.add_toplevel(&a_top, "second".into(), &qh, "a".into());
        keep_alive(a, a_top);
    });
    assert_eq!(
        added,
        Some((
            "xdg_session_v1".into(),
            xdg_session_v1::Error::AlreadyAdded as u32
        ))
    );
    // Restoring comes before the first commit.
    let mapped = session_error(|run, peer, session| {
        let (surface, toplevel) = peer.toplevel("main");
        surface.commit();
        run.pump(&mut [&mut *peer]);
        session.restore_toplevel(&toplevel, "main".into(), &peer.qh(), "main".into());
        keep_alive(surface, toplevel);
    });
    assert_eq!(
        mapped,
        Some((
            "xdg_session_v1".into(),
            xdg_session_v1::Error::AlreadyMapped as u32
        ))
    );
}

#[test]
fn the_global_is_advertised_at_mutters_version() {
    let dir = tempfile::tempdir().unwrap();
    let mut run = Session::start(&store_in(&dir));
    let peer = run.connect();
    assert!(peer
        .app
        .globals
        .contains(&("xdg_session_manager_v1".to_owned(), 1)));
    // Nothing happened: nothing is written.
    run.exit();
    assert!(!store_in(&dir).exists());
}
