//! The actual nested runtime (#729): ext-session-lock authorization as
//! `Runtime::tick` applies it, and input exclusivity while locked, with
//! real host input (XTEST through Xvfb into the winit window).
//!
//! The test binary is both sides. Without `TUNA_SESSION_LOCK_ROLE` it
//! launches `tuna-compositor` with itself as the supervised shell and is
//! an ordinary (foreign) Wayland client; as the shell (`lock` role) it
//! is the ext-session-lock client. They coordinate through files in a
//! private directory. Needs an X display (CI: `xvfb-run`) and `xdotool`;
//! `TUNA_REQUIRE_SESSION_LOCK_RUNTIME=1` makes a missing one a failure.

use std::os::fd::AsFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use wayland_client::protocol::{
    wl_buffer::WlBuffer,
    wl_callback::WlCallback,
    wl_compositor::WlCompositor,
    wl_keyboard::{self, WlKeyboard},
    wl_output::WlOutput,
    wl_pointer::{self, WlPointer},
    wl_registry::{self, WlRegistry},
    wl_seat::WlSeat,
    wl_shm::{self, WlShm},
    wl_shm_pool::WlShmPool,
    wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum};
use wayland_protocols::ext::session_lock::v1::client::{
    ext_session_lock_manager_v1::ExtSessionLockManagerV1,
    ext_session_lock_surface_v1::{self, ExtSessionLockSurfaceV1},
    ext_session_lock_v1::{self, ExtSessionLockV1},
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::{self, XdgToplevel},
    xdg_wm_base::{self, XdgWmBase},
};

const ROLE: &str = "TUNA_SESSION_LOCK_ROLE";
const ROOT: &str = "TUNA_SESSION_LOCK_ROOT";
const SOCKET: &str = "tuna-session-lock-runtime";
const LIMIT: Duration = Duration::from_secs(20);
/// How long a client listens for input that must not arrive.
const QUIET: Duration = Duration::from_millis(600);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LockFate {
    Locked,
    Finished,
}

#[derive(Default)]
struct Client {
    compositor: Option<WlCompositor>,
    shm: Option<WlShm>,
    seat: Option<WlSeat>,
    output: Option<WlOutput>,
    wm_base: Option<XdgWmBase>,
    lock_manager: Option<ExtSessionLockManagerV1>,
    keyboard_focus: bool,
    keys: Vec<u32>,
    buttons: Vec<u32>,
    toplevel_configured: Option<(i32, i32)>,
    xdg_serial: Option<u32>,
    lock_fate: Option<LockFate>,
    lock_configure: Option<(u32, u32, u32)>,
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
        let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        else {
            return;
        };
        match interface.as_str() {
            "wl_compositor" => state.compositor = Some(registry.bind(name, version.min(6), qh, ())),
            "wl_shm" => state.shm = Some(registry.bind(name, 1, qh, ())),
            "wl_seat" if state.seat.is_none() => {
                state.seat = Some(registry.bind(name, version.min(7), qh, ()))
            }
            "wl_output" if state.output.is_none() => {
                state.output = Some(registry.bind(name, version.min(4), qh, ()))
            }
            "xdg_wm_base" => state.wm_base = Some(registry.bind(name, 1, qh, ())),
            "ext_session_lock_manager_v1" => {
                state.lock_manager = Some(registry.bind(name, 1, qh, ()))
            }
            _ => {}
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
        match event {
            wl_keyboard::Event::Enter { .. } => state.keyboard_focus = true,
            wl_keyboard::Event::Leave { .. } => state.keyboard_focus = false,
            wl_keyboard::Event::Key {
                key,
                state: WEnum::Value(wl_keyboard::KeyState::Pressed),
                ..
            } => state.keys.push(key),
            _ => {}
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
        if let wl_pointer::Event::Button {
            button,
            state: WEnum::Value(wl_pointer::ButtonState::Pressed),
            ..
        } = event
        {
            state.buttons.push(button);
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
        state: &mut Self,
        _: &XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            state.xdg_serial = Some(serial);
        }
    }
}

impl Dispatch<XdgToplevel, ()> for Client {
    fn event(
        state: &mut Self,
        _: &XdgToplevel,
        event: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_toplevel::Event::Configure { width, height, .. } = event {
            state.toplevel_configured = Some((width, height));
        }
    }
}

impl Dispatch<ExtSessionLockV1, ()> for Client {
    fn event(
        state: &mut Self,
        _: &ExtSessionLockV1,
        event: ext_session_lock_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_session_lock_v1::Event::Locked => state.lock_fate = Some(LockFate::Locked),
            ext_session_lock_v1::Event::Finished => state.lock_fate = Some(LockFate::Finished),
            _ => {}
        }
    }
}

impl Dispatch<ExtSessionLockSurfaceV1, ()> for Client {
    fn event(
        state: &mut Self,
        _: &ExtSessionLockSurfaceV1,
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
            state.lock_configure = Some((serial, width, height));
        }
    }
}

wayland_client::delegate_noop!(Client: ignore WlCompositor);
wayland_client::delegate_noop!(Client: ignore WlShm);
wayland_client::delegate_noop!(Client: ignore WlShmPool);
wayland_client::delegate_noop!(Client: ignore WlBuffer);
wayland_client::delegate_noop!(Client: ignore WlSurface);
wayland_client::delegate_noop!(Client: ignore WlOutput);
wayland_client::delegate_noop!(Client: ignore WlCallback);
wayland_client::delegate_noop!(Client: ignore ExtSessionLockManagerV1);

impl Dispatch<WlSeat, ()> for Client {
    fn event(
        _: &mut Self,
        _: &WlSeat,
        _: wayland_client::protocol::wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

struct Peer {
    _conn: Connection,
    queue: EventQueue<Client>,
    client: Client,
}

impl Peer {
    fn new(conn: Connection) -> Self {
        let queue = conn.new_event_queue();
        conn.display().get_registry(&queue.handle(), ());
        let mut peer = Self {
            _conn: conn,
            queue,
            client: Client::default(),
        };
        peer.roundtrip();
        peer.roundtrip();
        let qh = peer.queue.handle();
        let seat = peer.client.seat.clone().expect("wl_seat");
        seat.get_keyboard(&qh, ());
        seat.get_pointer(&qh, ());
        peer.roundtrip();
        peer
    }

    fn roundtrip(&mut self) {
        self.queue
            .roundtrip(&mut self.client)
            .expect("compositor connection");
    }

    /// Dispatch until `done` holds; a deadline is a failure.
    fn until(&mut self, what: &str, mut done: impl FnMut(&Client) -> bool) {
        let end = Instant::now() + LIMIT;
        while !done(&self.client) {
            assert!(Instant::now() < end, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(10));
            self.roundtrip();
        }
    }

    /// Keep dispatching for `span`, collecting whatever arrives.
    fn listen(&mut self, span: Duration) {
        let end = Instant::now() + span;
        while Instant::now() < end {
            std::thread::sleep(Duration::from_millis(10));
            self.roundtrip();
        }
    }

    /// A shared-memory buffer of `width` x `height` opaque pixels.
    fn buffer(&self, width: i32, height: i32) -> WlBuffer {
        let qh = self.queue.handle();
        let stride = width * 4;
        let file = tempfile::tempfile().unwrap();
        file.set_len((stride * height) as u64).unwrap();
        let pool = self.client.shm.as_ref().expect("wl_shm").create_pool(
            file.as_fd(),
            stride * height,
            &qh,
            (),
        );
        let buffer =
            pool.create_buffer(0, width, height, stride, wl_shm::Format::Xrgb8888, &qh, ());
        pool.destroy();
        buffer
    }
}

fn root() -> PathBuf {
    PathBuf::from(std::env::var_os(ROOT).expect("fixture root"))
}

fn wait_file(path: &Path) {
    let end = Instant::now() + LIMIT;
    while !path.exists() {
        assert!(Instant::now() < end, "timed out waiting for {path:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn publish(path: &Path, contents: &str) {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, contents).unwrap();
    std::fs::rename(tmp, path).unwrap();
}

/// The ext-session-lock client, run as the compositor's supervised shell.
fn lock_client() {
    let root = root();
    // A restarted shell must not replay the script: the first one's exit
    // is itself a failure the parent reports.
    if std::fs::File::create_new(root.join("shell-started")).is_err() {
        return;
    }
    let display = std::env::var("WAYLAND_DISPLAY").expect("supervised WAYLAND_DISPLAY");
    let runtime = std::env::var("XDG_RUNTIME_DIR").expect("XDG_RUNTIME_DIR");
    let stream = UnixStream::connect(Path::new(&runtime).join(display)).unwrap();
    let mut peer = Peer::new(Connection::from_socket(stream).unwrap());
    publish(&root.join("shell-ready"), "");

    wait_file(&root.join("go-lock"));
    let qh = peer.queue.handle();
    let manager = peer.client.lock_manager.clone().expect("lock manager");
    let lock = manager.lock(&qh, ());
    peer.until("the shell's lock decision", |c| c.lock_fate.is_some());
    let fate = peer.client.lock_fate.unwrap();
    publish(&root.join("shell-lock-fate"), &format!("{fate:?}"));
    if fate != LockFate::Locked {
        // Stay connected until the compositor goes (roundtrip panics).
        loop {
            peer.listen(Duration::from_secs(1));
        }
    }

    let compositor = peer.client.compositor.clone().unwrap();
    let output = peer.client.output.clone().expect("wl_output");
    let surface = compositor.create_surface(&qh, ());
    let role = lock.get_lock_surface(&surface, &output, &qh, ());
    peer.until("the lock surface configure", |c| c.lock_configure.is_some());
    let (serial, width, height) = peer.client.lock_configure.unwrap();
    role.ack_configure(serial);
    let buffer = peer.buffer(width as i32, height as i32);
    surface.attach(Some(&buffer), 0, 0);
    surface.damage_buffer(0, 0, width as i32, height as i32);
    surface.commit();
    peer.roundtrip();
    publish(&root.join("shell-surface-up"), "");

    let mut reported = (usize::MAX, usize::MAX, false);
    let mut dropped = false;
    loop {
        peer.listen(Duration::from_millis(20));
        let now = (
            peer.client.keys.len(),
            peer.client.buttons.len(),
            peer.client.keyboard_focus,
        );
        if now != reported {
            reported = now;
            publish(
                &root.join("shell-input.json"),
                &serde_json::json!({
                    "keys": peer.client.keys,
                    "buttons": peer.client.buttons,
                    "keyboard_focus": peer.client.keyboard_focus,
                })
                .to_string(),
            );
        }
        if !dropped && root.join("drop-surface").exists() {
            dropped = true;
            // The lock client misbehaves: its curtain goes away while the
            // session stays locked.
            role.destroy();
            surface.destroy();
            peer.roundtrip();
            publish(&root.join("shell-surface-dropped"), "");
        }
    }
}

struct Compositor {
    child: Child,
    log: PathBuf,
}

impl Compositor {
    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
}

impl Drop for Compositor {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if std::thread::panicking() {
            eprintln!("compositor log:\n{}", self.log());
        }
    }
}

fn locked(root: &Path) -> Option<bool> {
    let bytes = std::fs::read(root.join("state.json")).ok()?;
    let state: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    state["locked"].as_bool()
}

fn wait_locked(root: &Path, want: bool, what: &str) {
    let end = Instant::now() + LIMIT;
    while locked(root) != Some(want) {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn shell_input(root: &Path) -> serde_json::Value {
    std::fs::read(root.join("shell-input.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(serde_json::Value::Null)
}

fn xdotool(args: &[&str]) -> String {
    let out = Command::new("xdotool")
        .args(args)
        .output()
        .expect("xdotool");
    assert!(out.status.success(), "xdotool {args:?}: {out:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// The compositor's host window on the X display.
fn host_window() -> String {
    let end = Instant::now() + LIMIT;
    loop {
        let out = Command::new("xdotool")
            .args(["search", "--name", "^Smithay$"])
            .output()
            .expect("xdotool");
        if let Some(id) = String::from_utf8_lossy(&out.stdout).lines().next() {
            return id.to_owned();
        }
        assert!(Instant::now() < end, "no compositor host window");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// One key press and one click at the output's centre, as real host
/// input to the nested compositor.
fn inject(window: &str) {
    xdotool(&["windowfocus", "--sync", window]);
    xdotool(&["mousemove", "--window", window, "400", "300"]);
    xdotool(&["key", "--clearmodifiers", "a"]);
    xdotool(&["click", "1"]);
}

/// evdev keycode of `a` (X keycode 38 minus 8).
const KEY_A: u32 = 30;
/// evdev BTN_LEFT.
const BTN_LEFT: u32 = 0x110;

#[test]
fn actual_runtime_grants_only_the_shell_lock_and_input_reaches_only_its_curtain() {
    match std::env::var(ROLE).ok().as_deref() {
        Some("lock") => return lock_client(),
        Some(_) => return,
        None => {}
    }
    let required = std::env::var_os("TUNA_REQUIRE_SESSION_LOCK_RUNTIME").is_some();
    let display = std::env::var_os("DISPLAY").is_some();
    let xdotool_ok = Command::new("xdotool")
        .arg("version")
        .stdout(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if !display || !xdotool_ok {
        assert!(!required, "Xvfb and xdotool are required for this proof");
        eprintln!("session-lock runtime proof skipped: needs DISPLAY and xdotool (CI runs it)");
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_owned();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let home = root.join("home");
    std::fs::create_dir(&home).unwrap();
    let log = root.join("compositor.log");
    let child = Command::new(env!("CARGO_BIN_EXE_tuna-compositor"))
        .args(["--socket", SOCKET, "--width", "800", "--height", "600"])
        .arg("--shell-bin")
        .arg(std::env::current_exe().unwrap())
        .env(ROLE, "lock")
        .env(ROOT, &root)
        .env("RUST_TEST_NOCAPTURE", "1")
        .env("HOME", &home)
        .env("XDG_RUNTIME_DIR", &root)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .env("TUNA_COMPOSITOR_STATE", root.join("state.json"))
        .env("TUNA_XWAYLAND", "0")
        .env("TUNA_FPRINTD_OFF", "1")
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("WAYLAND_SOCKET")
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(&log).unwrap())
        .spawn()
        .expect("spawn tuna-compositor");
    let mut compositor = Compositor { child, log };
    let socket = root.join(SOCKET);
    let end = Instant::now() + LIMIT;
    while !socket.exists() || !root.join("shell-ready").exists() {
        assert!(
            compositor.child.try_wait().unwrap().is_none(),
            "compositor exited:\n{}",
            compositor.log()
        );
        assert!(Instant::now() < end, "compositor or shell never came up");
        std::thread::sleep(Duration::from_millis(20));
    }
    wait_locked(&root, false, "the initial unlocked state");
    let window = host_window();

    // An ordinary application: a mapped, focused window.
    let mut app =
        Peer::new(Connection::from_socket(UnixStream::connect(&socket).unwrap()).unwrap());
    let qh = app.queue.handle();
    let surface = app
        .client
        .compositor
        .clone()
        .unwrap()
        .create_surface(&qh, ());
    let xdg = app
        .client
        .wm_base
        .clone()
        .expect("xdg_wm_base")
        .get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title("ordinary app".into());
    surface.commit();
    app.until("the window's configure", |c| c.xdg_serial.is_some());
    xdg.ack_configure(app.client.xdg_serial.unwrap());
    let (w, h) = match app.client.toplevel_configured {
        Some((w, h)) if w > 0 && h > 0 => (w, h),
        _ => (800, 600),
    };
    let buffer = app.buffer(w, h);
    surface.attach(Some(&buffer), 0, 0);
    surface.damage_buffer(0, 0, w, h);
    surface.commit();
    app.until("keyboard focus on the new window", |c| c.keyboard_focus);

    // Positive control: unlocked, host input reaches the application.
    inject(&window);
    app.until("unlocked key and click", |c| {
        c.keys.contains(&KEY_A) && c.buttons.contains(&BTN_LEFT)
    });

    // Authorization: the application is not the supervised shell, so
    // the runtime refuses its lock and the session stays unlocked.
    let foreign = app
        .client
        .lock_manager
        .clone()
        .expect("ext_session_lock_manager_v1")
        .lock(&qh, ());
    app.until("the foreign lock decision", |c| c.lock_fate.is_some());
    assert_eq!(app.client.lock_fate, Some(LockFate::Finished));
    foreign.destroy();
    app.listen(Duration::from_millis(200));
    assert_eq!(locked(&root), Some(false), "a foreign lock must not lock");
    assert!(
        app.client.keyboard_focus,
        "a refused lock leaves the application focused"
    );

    // The supervised shell's lock is granted and engages the
    // compositor's own lock.
    publish(&root.join("go-lock"), "");
    wait_file(&root.join("shell-lock-fate"));
    assert_eq!(
        std::fs::read_to_string(root.join("shell-lock-fate")).unwrap(),
        "Locked"
    );
    wait_locked(&root, true, "the shell's lock to engage");
    wait_file(&root.join("shell-surface-up"));

    // While locked, host input reaches only the lock surface.
    app.listen(Duration::from_millis(200));
    assert!(
        !app.client.keyboard_focus,
        "the application loses keyboard focus under the lock"
    );
    app.client.keys.clear();
    app.client.buttons.clear();
    inject(&window);
    let end = Instant::now() + LIMIT;
    loop {
        let input = shell_input(&root);
        let keys = input["keys"].as_array().map(Vec::len).unwrap_or(0);
        let buttons = input["buttons"].as_array().map(Vec::len).unwrap_or(0);
        if keys > 0 && buttons > 0 {
            assert!(input["keys"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!(KEY_A)));
            break;
        }
        assert!(
            Instant::now() < end,
            "the lock surface never received the key and click: {input}"
        );
        app.listen(Duration::from_millis(20));
    }
    app.listen(QUIET);
    assert!(
        app.client.keys.is_empty() && app.client.buttons.is_empty(),
        "input leaked to the application under the lock: keys {:?} buttons {:?}",
        app.client.keys,
        app.client.buttons
    );

    // With no lock surface (the lock client lost it), the session stays
    // locked and input goes nowhere, not back to the application.
    let before = shell_input(&root);
    publish(&root.join("drop-surface"), "");
    wait_file(&root.join("shell-surface-dropped"));
    app.listen(Duration::from_millis(200));
    assert_eq!(
        locked(&root),
        Some(true),
        "losing the curtain never unlocks"
    );
    inject(&window);
    app.listen(QUIET);
    assert!(
        app.client.keys.is_empty() && app.client.buttons.is_empty(),
        "input leaked to the application with no lock surface: keys {:?} buttons {:?}",
        app.client.keys,
        app.client.buttons
    );
    assert!(!app.client.keyboard_focus);
    let after = shell_input(&root);
    assert_eq!(
        before["keys"], after["keys"],
        "a destroyed lock surface hears nothing"
    );
    assert_eq!(locked(&root), Some(true));
    assert!(
        compositor.child.try_wait().unwrap().is_none(),
        "compositor survived the whole proof"
    );
    drop(toplevel);
}
