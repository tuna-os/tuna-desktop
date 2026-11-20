//! Proof client for the Mutter 51 protocols of #364/#588: each subcommand
//! drives one protocol as a real client of the running session
//! (`$WAYLAND_DISPLAY`) and exits nonzero with `FAIL <why>` on stderr
//! when the session does not behave as Mutter 51 does.
//!
//! - `xdg-wm-base-7 --state FILE`: a version 7 bind sees a maximized
//!   window tiled and constrained on every edge; a version 6 bind of the
//!   same request sees the tiled states only.
//! - `xdg-foreign-v1 --state FILE`: a zxdg_exporter_v1 handle (GTK 3)
//!   imports through zxdg_importer_v2 (the portal) and parents its dialog;
//!   revoking the export destroys the import and the parent; a v2 handle
//!   imports through v1; an unknown v1 handle is destroyed.
//! - `wl-fixes`: wl_fixes 1 destroys a second registry and the connection
//!   carries on.
//! - `background-blur --state FILE`: ext_background_effect_manager_v1
//!   offers blur; a maximized window of one-pixel stripes is mapped with a
//!   clear window above it whose whole surface is a blur region. Prints
//!   `ready` and keeps both open until killed; the proof samples pixels.
//! - `xwayland-grab INSTANCE CLASS --state FILE` (an X11 client on
//!   `$DISPLAY`, the session's Xwayland): maps a window with that
//!   WM_CLASS, waits until the compositor focuses it, takes
//!   `XGrabKeyboard` (rootless Xwayland turns that into
//!   zwp_xwayland_keyboard_grab_v1), prints `grabbed WINDOW` and then one
//!   `press KEYCODE` / `release KEYCODE` line per key it receives until
//!   killed; the proof presses keys and reads the compositor's verdict.
//!
//! `--state` names the compositor state file (`TUNA_COMPOSITOR_STATE`),
//! read for window parents and mapping.
use std::{
    collections::HashMap,
    io::Write,
    os::fd::{AsFd, AsRawFd},
    path::{Path, PathBuf},
    process::exit,
    time::{Duration, Instant},
};

use wayland_client::{
    protocol::{
        wl_buffer::WlBuffer,
        wl_callback::{self, WlCallback},
        wl_compositor::WlCompositor,
        wl_fixes::WlFixes,
        wl_region::WlRegion,
        wl_registry::{self, WlRegistry},
        wl_shm::{self, WlShm},
        wl_shm_pool::WlShmPool,
        wl_surface::WlSurface,
    },
    Connection, Dispatch, EventQueue, Proxy, QueueHandle,
};
use wayland_protocols::ext::background_effect::v1::client::{
    ext_background_effect_manager_v1::{self, ExtBackgroundEffectManagerV1},
    ext_background_effect_surface_v1::ExtBackgroundEffectSurfaceV1,
};
use wayland_protocols::xdg::foreign::{
    zv1::client::{
        zxdg_exported_v1::{self, ZxdgExportedV1},
        zxdg_exporter_v1::ZxdgExporterV1,
        zxdg_imported_v1::{self, ZxdgImportedV1},
        zxdg_importer_v1::ZxdgImporterV1,
    },
    zv2::client::{
        zxdg_exported_v2::{self, ZxdgExportedV2},
        zxdg_exporter_v2::ZxdgExporterV2,
        zxdg_imported_v2::{self, ZxdgImportedV2},
        zxdg_importer_v2::ZxdgImporterV2,
    },
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::{self, XdgToplevel},
    xdg_wm_base::{self, XdgWmBase},
};

const DEADLINE: Duration = Duration::from_secs(10);
const MAXIMIZED: u32 = 1;

fn fail(why: impl std::fmt::Display) -> ! {
    eprintln!("FAIL {why}");
    exit(1);
}

/// How a window's buffer is filled.
#[derive(Clone, Copy)]
enum Fill {
    Solid(u32),
    /// One-pixel black and white vertical stripes.
    Stripes,
    /// Fully transparent.
    Clear,
}

struct Window {
    surface: WlSurface,
    top: XdgToplevel,
    fill: Fill,
    default_size: (i32, i32),
    pending_size: (i32, i32),
    pending_states: Vec<u32>,
    /// Every configure as (states, size), in arrival order.
    configures: Vec<(Vec<u32>, (i32, i32))>,
}

#[derive(Default)]
struct Probe {
    globals: HashMap<String, (u32, u32)>,
    compositor: Option<WlCompositor>,
    shm: Option<WlShm>,
    windows: Vec<Window>,
    /// Export handles by export tag.
    handles: HashMap<u32, String>,
    /// Import tags the compositor destroyed.
    destroyed_imports: Vec<u32>,
    blur_capabilities: Option<u32>,
    synced: bool,
}

impl Probe {
    fn global(&self, interface: &str, version: u32) -> u32 {
        match self.globals.get(interface) {
            Some(&(name, v)) if v >= version => name,
            Some(&(_, v)) => fail(format!("{interface} is version {v}, need {version}")),
            None => fail(format!("{interface} is not advertised")),
        }
    }
}

struct Client {
    conn: Connection,
    queue: EventQueue<Probe>,
    probe: Probe,
    registry: WlRegistry,
}

impl Client {
    fn connect() -> Self {
        let conn = Connection::connect_to_env().unwrap_or_else(|e| fail(e));
        let queue = conn.new_event_queue();
        let registry = conn.display().get_registry(&queue.handle(), ());
        let mut client = Client {
            conn,
            queue,
            probe: Probe::default(),
            registry,
        };
        client.roundtrip();
        let compositor = client.probe.global("wl_compositor", 4);
        let shm = client.probe.global("wl_shm", 1);
        let qh = client.queue.handle();
        client.probe.compositor = Some(client.registry.bind(compositor, 4, &qh, ()));
        client.probe.shm = Some(client.registry.bind(shm, 1, &qh, ()));
        client
    }

    fn qh(&self) -> QueueHandle<Probe> {
        self.queue.handle()
    }

    fn bind<I: Proxy + 'static>(&self, interface: &str, version: u32) -> I
    where
        Probe: Dispatch<I, ()>,
    {
        let name = self.probe.global(interface, version);
        self.registry.bind(name, version, &self.qh(), ())
    }

    fn roundtrip(&mut self) {
        self.queue
            .roundtrip(&mut self.probe)
            .unwrap_or_else(|e| fail(format!("protocol error: {e}")));
    }

    /// Dispatch until `done` holds or the deadline passes.
    fn until(&mut self, what: &str, mut done: impl FnMut(&Probe) -> bool) {
        let end = Instant::now() + DEADLINE;
        loop {
            self.queue
                .dispatch_pending(&mut self.probe)
                .unwrap_or_else(|e| fail(format!("protocol error: {e}")));
            if done(&self.probe) {
                return;
            }
            if Instant::now() >= end {
                fail(format!("timed out waiting for {what}"));
            }
            let _ = self.queue.flush();
            if let Some(guard) = self.queue.prepare_read() {
                let mut fd = libc::pollfd {
                    fd: guard.connection_fd().as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                // SAFETY: one valid pollfd for the duration of the call.
                if unsafe { libc::poll(&mut fd, 1, 100) } > 0 {
                    let _ = guard.read();
                }
            }
        }
    }

    /// A toplevel bound through `wm`, drawn at each configure.
    fn window(&mut self, wm: &XdgWmBase, app_id: &str, fill: Fill, size: (i32, i32)) -> usize {
        let qh = self.qh();
        let index = self.probe.windows.len();
        let surface = self
            .probe
            .compositor
            .as_ref()
            .unwrap()
            .create_surface(&qh, ());
        let xdg = wm.get_xdg_surface(&surface, &qh, index);
        let top = xdg.get_toplevel(&qh, index);
        top.set_app_id(app_id.into());
        top.set_title(app_id.into());
        self.probe.windows.push(Window {
            surface: surface.clone(),
            top,
            fill,
            default_size: size,
            pending_size: (0, 0),
            pending_states: Vec::new(),
            configures: Vec::new(),
        });
        surface.commit();
        self.until("the first configure", |p| {
            !p.windows[index].configures.is_empty()
        });
        index
    }
}

fn sync(client: &mut Client) {
    client.probe.synced = false;
    client.conn.display().sync(&client.qh(), ());
    client.until("a sync", |p| p.synced);
}

fn buffer(probe: &Probe, qh: &QueueHandle<Probe>, fill: Fill, (w, h): (i32, i32)) -> WlBuffer {
    let mut file = tempfile::tempfile().unwrap_or_else(|e| fail(e));
    let mut row = Vec::with_capacity(w as usize * 4);
    for x in 0..w {
        let pixel: u32 = match fill {
            Fill::Solid(color) => color,
            Fill::Stripes if x % 2 == 0 => 0xff00_0000,
            Fill::Stripes => 0xffff_ffff,
            Fill::Clear => 0,
        };
        row.extend_from_slice(&pixel.to_ne_bytes());
    }
    for _ in 0..h {
        file.write_all(&row).unwrap_or_else(|e| fail(e));
    }
    let pool = probe
        .shm
        .as_ref()
        .unwrap()
        .create_pool(file.as_fd(), w * h * 4, qh, ());
    let buffer = pool.create_buffer(0, w, h, w * 4, wl_shm::Format::Argb8888, qh, ());
    pool.destroy();
    buffer
}

/// The compositor's window record for `app_id`, once it is mapped.
fn state_window(state: &Path, app_id: &str) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(state).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    json["windows"]
        .as_array()?
        .iter()
        .find(|w| w["app_id"] == app_id)
        .cloned()
}

/// Wait until the state file says `check` of `app_id`'s window record,
/// dispatching `clients` meanwhile.
fn await_state(
    clients: &mut [&mut Client],
    state: &Path,
    app_id: &str,
    what: &str,
    check: impl Fn(&serde_json::Value) -> bool,
) -> serde_json::Value {
    let end = Instant::now() + DEADLINE;
    loop {
        for client in clients.iter_mut() {
            let _ = client.queue.flush();
            if let Some(guard) = client.queue.prepare_read() {
                let mut fd = libc::pollfd {
                    fd: guard.connection_fd().as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                // SAFETY: one valid pollfd for the duration of the call.
                if unsafe { libc::poll(&mut fd, 1, 0) } > 0 {
                    let _ = guard.read();
                }
            }
            client
                .queue
                .dispatch_pending(&mut client.probe)
                .unwrap_or_else(|e| fail(format!("protocol error: {e}")));
        }
        if let Some(window) = state_window(state, app_id) {
            if check(&window) {
                return window;
            }
        }
        if Instant::now() >= end {
            fail(format!(
                "timed out waiting for {what}; {app_id} is {:?}",
                state_window(state, app_id)
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The edge states of a configure: tiled 5-8 and constrained 10-13.
fn edges(states: &[u32]) -> Vec<u32> {
    let mut edges: Vec<u32> = states
        .iter()
        .copied()
        .filter(|s| (5..=13).contains(s) && *s != 9)
        .collect();
    edges.sort_unstable();
    edges
}

fn maximized_edges(version: u32, app_id: &str) -> Vec<u32> {
    let mut client = Client::connect();
    let wm: XdgWmBase = client.bind("xdg_wm_base", version);
    let index = client.window(&wm, app_id, Fill::Solid(0xff25_3040), (320, 200));
    client.probe.windows[index].top.set_maximized();
    client.until("the maximized configure", |p| {
        p.windows[index]
            .configures
            .iter()
            .any(|(states, _)| states.contains(&MAXIMIZED))
    });
    let (states, _) = client.probe.windows[index]
        .configures
        .iter()
        .rev()
        .find(|(states, _)| states.contains(&MAXIMIZED))
        .unwrap()
        .clone();
    client.probe.windows[index].top.destroy();
    client.roundtrip();
    edges(&states)
}

fn xdg_wm_base_7() {
    // Mutter 51: META_XDG_WM_BASE_VERSION 7; fill_states sends the
    // constrained states only to version 7 resources.
    let seven = maximized_edges(7, "tuna-probe-wm-base-7");
    if seven != [5, 6, 7, 8, 10, 11, 12, 13] {
        fail(format!(
            "a version 7 maximized window's edge states are {seven:?}, want tiled and constrained on all four edges"
        ));
    }
    println!("ok version 7: maximized is tiled and constrained on every edge {seven:?}");
    let six = maximized_edges(6, "tuna-probe-wm-base-6");
    if six != [5, 6, 7, 8] {
        fail(format!(
            "a version 6 maximized window's edge states are {six:?}, want the tiled states alone"
        ));
    }
    println!("ok version 6: maximized is tiled on every edge, with no constrained state {six:?}");
}

fn parent_is(window: &serde_json::Value, parent: Option<&serde_json::Value>) -> bool {
    match parent {
        Some(parent) => window["parent_window_id"] == parent["id"],
        None => window["parent_window_id"].is_null(),
    }
}

fn xdg_foreign_v1(state: &Path) {
    // Mutter 51 (meta-wayland-legacy-xdg-foreign.c) shares v2's handle
    // table: GTK 3 exports with v1, the portal imports with v2.
    let mut gtk3 = Client::connect();
    let mut portal = Client::connect();
    let exporter_v1: ZxdgExporterV1 = gtk3.bind("zxdg_exporter_v1", 1);
    let importer_v1: ZxdgImporterV1 = gtk3.bind("zxdg_importer_v1", 1);
    let exporter_v2: ZxdgExporterV2 = portal.bind("zxdg_exporter_v2", 1);
    let importer_v2: ZxdgImporterV2 = portal.bind("zxdg_importer_v2", 1);
    let wm_app: XdgWmBase = gtk3.bind("xdg_wm_base", 1);
    let wm_dialog: XdgWmBase = portal.bind("xdg_wm_base", 1);
    let app_id = "tuna-probe-foreign-app";
    let dialog_id = "tuna-probe-foreign-dialog";
    let app = gtk3.window(&wm_app, app_id, Fill::Solid(0xff35_84e4), (360, 240));
    let dialog = portal.window(&wm_dialog, dialog_id, Fill::Solid(0xffe0_1b24), (240, 160));
    let app_record = await_state(
        &mut [&mut gtk3, &mut portal],
        state,
        app_id,
        "the app to map",
        |_| true,
    );
    let dialog_record = await_state(
        &mut [&mut gtk3, &mut portal],
        state,
        dialog_id,
        "the dialog to map",
        |_| true,
    );

    // v1 export, v2 import.
    let app_surface = gtk3.probe.windows[app].surface.clone();
    let exported = exporter_v1.export(&app_surface, &gtk3.qh(), 1);
    gtk3.until("the v1 export handle", |p| p.handles.contains_key(&1));
    let handle = gtk3.probe.handles[&1].clone();
    let dialog_surface = portal.probe.windows[dialog].surface.clone();
    let imported = importer_v2.import_toplevel(handle.clone(), &portal.qh(), 1);
    imported.set_parent_of(&dialog_surface);
    await_state(
        &mut [&mut gtk3, &mut portal],
        state,
        dialog_id,
        "the v2 import of a v1 handle to parent the dialog",
        |w| parent_is(w, Some(&app_record)),
    );
    if !portal.probe.destroyed_imports.is_empty() {
        fail("the v2 import of a live v1 handle was destroyed");
    }
    println!("ok a zxdg_exporter_v1 handle imported through zxdg_importer_v2 parents the dialog");

    // Revoking the export destroys the import and its parent
    // (meta_wayland_xdg_exported_destroy).
    exported.destroy();
    gtk3.roundtrip();
    portal.until("the revoked import's destroyed", |p| {
        p.destroyed_imports.contains(&1)
    });
    await_state(
        &mut [&mut gtk3, &mut portal],
        state,
        dialog_id,
        "the revoked import to unparent the dialog",
        |w| parent_is(w, None),
    );
    println!("ok revoking the v1 export destroys the v2 import and unparents the dialog");

    // v2 export, v1 import; destroying the import unparents.
    let _exported = exporter_v2.export_toplevel(&dialog_surface, &portal.qh(), 2);
    portal.until("the v2 export handle", |p| p.handles.contains_key(&2));
    let handle = portal.probe.handles[&2].clone();
    let imported = importer_v1.import(handle, &gtk3.qh(), 2);
    imported.set_parent_of(&app_surface);
    await_state(
        &mut [&mut gtk3, &mut portal],
        state,
        app_id,
        "the v1 import of a v2 handle to parent the app",
        |w| parent_is(w, Some(&dialog_record)),
    );
    imported.destroy();
    await_state(
        &mut [&mut gtk3, &mut portal],
        state,
        app_id,
        "the destroyed v1 import to unparent the app",
        |w| parent_is(w, None),
    );
    println!("ok a zxdg_exporter_v2 handle imported through zxdg_importer_v1 parents, and its destruction unparents");

    // An unknown handle is destroyed at once.
    let _unknown = importer_v1.import("not-a-handle".into(), &gtk3.qh(), 3);
    gtk3.until("the unknown v1 import's destroyed", |p| {
        p.destroyed_imports.contains(&3)
    });
    println!("ok an unknown v1 handle's import is destroyed");
}

fn wl_fixes() {
    // Mutter 51 (meta-wayland-fixes.c): destroy_registry destroys the
    // registry resource.
    let mut client = Client::connect();
    let fixes: WlFixes = client.bind("wl_fixes", 1);
    let doomed = client.conn.display().get_registry(&client.qh(), ());
    sync(&mut client);
    fixes.destroy_registry(&doomed);
    sync(&mut client);
    sync(&mut client);
    fixes.destroy();
    client.roundtrip();
    // The surviving registry still binds.
    let _wm: XdgWmBase = client.bind("xdg_wm_base", 1);
    client.roundtrip();
    println!("ok wl_fixes 1 destroyed a registry and the connection carries on");
}

fn background_blur(state: &Path) {
    let mut client = Client::connect();
    let manager: ExtBackgroundEffectManagerV1 = client.bind("ext_background_effect_manager_v1", 1);
    client.until("the background effect capabilities", |p| {
        p.blur_capabilities.is_some()
    });
    let caps = client.probe.blur_capabilities.unwrap();
    if caps & u32::from(ext_background_effect_manager_v1::Capability::Blur) == 0 {
        fail(format!("capabilities {caps:#x} do not offer blur"));
    }
    println!("ok ext_background_effect_manager_v1 offers blur");
    let wm: XdgWmBase = client.bind("xdg_wm_base", 1);
    let stripes = client.window(&wm, "tuna-probe-blur-stripes", Fill::Stripes, (640, 480));
    client.probe.windows[stripes].top.set_maximized();
    client.until("the stripes to maximize", |p| {
        p.windows[stripes]
            .configures
            .iter()
            .any(|(states, _)| states.contains(&MAXIMIZED))
    });
    await_state(
        &mut [&mut client],
        state,
        "tuna-probe-blur-stripes",
        "the stripes to map",
        |_| true,
    );
    let qh = client.qh();
    let surface = client
        .probe
        .compositor
        .as_ref()
        .unwrap()
        .create_surface(&qh, ());
    let effect = manager.get_background_effect(&surface, &qh, ());
    let region: WlRegion = client
        .probe
        .compositor
        .as_ref()
        .unwrap()
        .create_region(&qh, ());
    region.add(0, 0, i32::MAX, i32::MAX);
    effect.set_blur_region(Some(&region));
    region.destroy();
    let index = client.probe.windows.len();
    let xdg = wm.get_xdg_surface(&surface, &qh, index);
    let top = xdg.get_toplevel(&qh, index);
    top.set_app_id("tuna-probe-blur-frost".into());
    top.set_title("tuna-probe-blur-frost".into());
    client.probe.windows.push(Window {
        surface: surface.clone(),
        top,
        fill: Fill::Clear,
        default_size: (240, 160),
        pending_size: (0, 0),
        pending_states: Vec::new(),
        configures: Vec::new(),
    });
    surface.commit();
    await_state(
        &mut [&mut client],
        state,
        "tuna-probe-blur-frost",
        "the frost window to map",
        |_| true,
    );
    println!("ready");
    let _ = std::io::stdout().flush();
    loop {
        client.until("nothing (held until killed)", |_| false);
    }
}

/// The compositor's state file as JSON.
#[cfg(feature = "xwayland")]
fn state_json(state: &Path) -> Option<serde_json::Value> {
    serde_json::from_str(&std::fs::read_to_string(state).ok()?).ok()
}

/// An X11 client grabbing the keyboard as a remote viewer does:
/// `XGrabKeyboard` on its focused window. Whether the grab holds the
/// compositor's shortcuts is the compositor's decision; this reports the
/// keys the window receives.
#[cfg(feature = "xwayland")]
fn xwayland_grab(state: &Path, instance: &str, class: &str) {
    use smithay::reexports::x11rb::{
        connection::Connection,
        protocol::{
            xproto::{
                AtomEnum, ConnectionExt as _, CreateWindowAux, EventMask, GrabMode, GrabStatus,
                PropMode, WindowClass,
            },
            Event,
        },
        wrapper::ConnectionExt as _,
        CURRENT_TIME,
    };
    let (conn, screen) = smithay::reexports::x11rb::connect(None)
        .unwrap_or_else(|e| fail(format!("cannot reach the session's Xwayland: {e}")));
    fn x(what: &str, e: &dyn std::fmt::Display) -> ! {
        fail(format!("X11 {what}: {e}"))
    }
    let root = &conn.setup().roots[screen];
    let window = conn.generate_id().unwrap_or_else(|e| x("id", &e));
    conn.create_window(
        root.root_depth,
        window,
        root.root,
        0,
        0,
        320,
        200,
        0,
        WindowClass::INPUT_OUTPUT,
        root.root_visual,
        &CreateWindowAux::new()
            .background_pixel(0x253040)
            .event_mask(EventMask::KEY_PRESS | EventMask::KEY_RELEASE),
    )
    .unwrap_or_else(|e| x("create_window", &e));
    conn.change_property8(
        PropMode::REPLACE,
        window,
        AtomEnum::WM_CLASS,
        AtomEnum::STRING,
        format!("{instance}\0{class}\0").as_bytes(),
    )
    .unwrap_or_else(|e| x("WM_CLASS", &e));
    conn.change_property8(
        PropMode::REPLACE,
        window,
        AtomEnum::WM_NAME,
        AtomEnum::STRING,
        format!("Tuna Grab Probe {class}").as_bytes(),
    )
    .unwrap_or_else(|e| x("WM_NAME", &e));
    conn.map_window(window)
        .unwrap_or_else(|e| x("map_window", &e));
    conn.flush().unwrap_or_else(|e| x("flush", &e));
    // Xwayland asks for the grab only for the surface holding keyboard
    // focus, as a viewer grabs once the user clicked into it.
    let end = Instant::now() + DEADLINE;
    loop {
        let focused = state_json(state).is_some_and(|json| {
            json["windows"].as_array().is_some_and(|windows| {
                windows
                    .iter()
                    .any(|w| w["app_id"] == class && json["focused"] == w["id"])
            })
        });
        if focused {
            break;
        }
        if Instant::now() >= end {
            fail(format!("{class}'s X11 window never took focus"));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let grab = conn
        .grab_keyboard(
            false,
            window,
            CURRENT_TIME,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
        )
        .unwrap_or_else(|e| x("grab_keyboard", &e))
        .reply()
        .unwrap_or_else(|e| x("grab_keyboard reply", &e));
    if grab.status != GrabStatus::SUCCESS {
        fail(format!("XGrabKeyboard answered {:?}", grab.status));
    }
    println!("grabbed {window}");
    let _ = std::io::stdout().flush();
    loop {
        match conn.wait_for_event() {
            Ok(Event::KeyPress(key)) => println!("press {}", key.detail),
            Ok(Event::KeyRelease(key)) => println!("release {}", key.detail),
            Ok(_) => continue,
            Err(e) => x("connection", &e),
        }
        let _ = std::io::stdout().flush();
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let state = args
        .iter()
        .position(|a| a == "--state")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from);
    let need_state = || {
        state
            .clone()
            .unwrap_or_else(|| fail("--state FILE is required"))
    };
    match args.first().map(String::as_str) {
        Some("xdg-wm-base-7") => xdg_wm_base_7(),
        Some("xdg-foreign-v1") => xdg_foreign_v1(&need_state()),
        Some("wl-fixes") => wl_fixes(),
        Some("background-blur") => background_blur(&need_state()),
        #[cfg(feature = "xwayland")]
        Some("xwayland-grab") => match (args.get(1), args.get(2)) {
            (Some(instance), Some(class)) => xwayland_grab(&need_state(), instance, class),
            _ => fail("xwayland-grab needs INSTANCE CLASS"),
        },
        other => fail(format!(
            "unknown check {other:?}: xdg-wm-base-7, xdg-foreign-v1, wl-fixes, background-blur, xwayland-grab"
        )),
    }
}

impl Dispatch<WlRegistry, ()> for Probe {
    fn event(
        probe: &mut Self,
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
            probe.globals.insert(interface, (name, version));
        }
    }
}

impl Dispatch<WlCallback, ()> for Probe {
    fn event(
        probe: &mut Self,
        _: &WlCallback,
        event: wl_callback::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { .. } = event {
            probe.synced = true;
        }
    }
}

impl Dispatch<XdgWmBase, ()> for Probe {
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

impl Dispatch<XdgSurface, usize> for Probe {
    fn event(
        probe: &mut Self,
        xdg: &XdgSurface,
        event: xdg_surface::Event,
        &index: &usize,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg.ack_configure(serial);
            let window = &mut probe.windows[index];
            let (w, h) = window.pending_size;
            let size = if w > 0 && h > 0 {
                (w, h)
            } else {
                window.default_size
            };
            let states = std::mem::take(&mut window.pending_states);
            window.configures.push((states, size));
            let (surface, fill) = (window.surface.clone(), window.fill);
            let buffer = buffer(probe, qh, fill, size);
            surface.attach(Some(&buffer), 0, 0);
            surface.damage_buffer(0, 0, size.0, size.1);
            surface.commit();
        }
    }
}

impl Dispatch<XdgToplevel, usize> for Probe {
    fn event(
        probe: &mut Self,
        _: &XdgToplevel,
        event: xdg_toplevel::Event,
        &index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_toplevel::Event::Configure {
            width,
            height,
            states,
        } = event
        {
            let window = &mut probe.windows[index];
            window.pending_size = (width, height);
            window.pending_states = states
                .as_chunks::<4>()
                .0
                .iter()
                .map(|c| u32::from_ne_bytes(*c))
                .collect();
        }
    }
}

impl Dispatch<ZxdgExportedV1, u32> for Probe {
    fn event(
        probe: &mut Self,
        _: &ZxdgExportedV1,
        event: zxdg_exported_v1::Event,
        &tag: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zxdg_exported_v1::Event::Handle { handle } = event {
            probe.handles.insert(tag, handle);
        }
    }
}

impl Dispatch<ZxdgExportedV2, u32> for Probe {
    fn event(
        probe: &mut Self,
        _: &ZxdgExportedV2,
        event: zxdg_exported_v2::Event,
        &tag: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zxdg_exported_v2::Event::Handle { handle } = event {
            probe.handles.insert(tag, handle);
        }
    }
}

impl Dispatch<ZxdgImportedV1, u32> for Probe {
    fn event(
        probe: &mut Self,
        _: &ZxdgImportedV1,
        event: zxdg_imported_v1::Event,
        &tag: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zxdg_imported_v1::Event::Destroyed = event {
            probe.destroyed_imports.push(tag);
        }
    }
}

impl Dispatch<ZxdgImportedV2, u32> for Probe {
    fn event(
        probe: &mut Self,
        _: &ZxdgImportedV2,
        event: zxdg_imported_v2::Event,
        &tag: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zxdg_imported_v2::Event::Destroyed = event {
            probe.destroyed_imports.push(tag);
        }
    }
}

impl Dispatch<ExtBackgroundEffectManagerV1, ()> for Probe {
    fn event(
        probe: &mut Self,
        _: &ExtBackgroundEffectManagerV1,
        event: ext_background_effect_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let ext_background_effect_manager_v1::Event::Capabilities { flags } = event {
            probe.blur_capabilities = Some(flags.into());
        }
    }
}

wayland_client::delegate_noop!(Probe: ignore WlCompositor);
wayland_client::delegate_noop!(Probe: ignore WlShm);
wayland_client::delegate_noop!(Probe: ignore WlShmPool);
wayland_client::delegate_noop!(Probe: ignore WlBuffer);
wayland_client::delegate_noop!(Probe: ignore WlSurface);
wayland_client::delegate_noop!(Probe: ignore WlRegion);
wayland_client::delegate_noop!(Probe: WlFixes);
wayland_client::delegate_noop!(Probe: ZxdgExporterV1);
wayland_client::delegate_noop!(Probe: ZxdgImporterV1);
wayland_client::delegate_noop!(Probe: ZxdgExporterV2);
wayland_client::delegate_noop!(Probe: ZxdgImporterV2);
wayland_client::delegate_noop!(Probe: ignore ExtBackgroundEffectSurfaceV1);
