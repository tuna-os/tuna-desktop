//! The GNOME 51 protocols Tuna Desktop added to close P-SY-06's gaps (#89):
//! presentation-time, fifo, keyboard-shortcuts-inhibit, xdg-dialog,
//! xdg-foreign, xdg-system-bell, xdg-toplevel-tag, ext-background-effect and
//! linux-drm-syncobj, driven by a real client against the headless
//! compositor.

use std::collections::HashMap;
use std::os::unix::net::UnixStream;

use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
use tuna_compositor::windows::{
    ManagerInput, WindowManager, ESCAPE_KEYCODE, PAGE_DOWN_KEYCODE, SUPER_LEFT_KEYCODE,
};
use tuna_compositor::TestCompositor;
use wayland_client::{
    protocol::{
        wl_callback::{self, WlCallback},
        wl_compositor::WlCompositor,
        wl_registry::{self, WlRegistry},
        wl_seat::WlSeat,
        wl_surface::WlSurface,
    },
    Connection, Dispatch, EventQueue, QueueHandle,
};
use wayland_protocols::ext::background_effect::v1::client::{
    ext_background_effect_manager_v1::{self, ExtBackgroundEffectManagerV1},
    ext_background_effect_surface_v1::{self, ExtBackgroundEffectSurfaceV1},
};
use wayland_protocols::wp::fifo::v1::client::{
    wp_fifo_manager_v1::WpFifoManagerV1, wp_fifo_v1::WpFifoV1,
};
use wayland_protocols::wp::keyboard_shortcuts_inhibit::zv1::client::{
    zwp_keyboard_shortcuts_inhibit_manager_v1::ZwpKeyboardShortcutsInhibitManagerV1,
    zwp_keyboard_shortcuts_inhibitor_v1::{self, ZwpKeyboardShortcutsInhibitorV1},
};
use wayland_protocols::wp::presentation_time::client::{
    wp_presentation::{self, WpPresentation},
    wp_presentation_feedback::{self, WpPresentationFeedback},
};
use wayland_protocols::xdg::dialog::v1::client::{
    xdg_dialog_v1::XdgDialogV1, xdg_wm_dialog_v1::XdgWmDialogV1,
};
use wayland_protocols::xdg::foreign::zv1::client::{
    zxdg_exported_v1::{self, ZxdgExportedV1},
    zxdg_exporter_v1::ZxdgExporterV1,
    zxdg_imported_v1::{self, ZxdgImportedV1},
    zxdg_importer_v1::ZxdgImporterV1,
};
use wayland_protocols::xdg::foreign::zv2::client::{
    zxdg_exported_v2::{self, ZxdgExportedV2},
    zxdg_exporter_v2::ZxdgExporterV2,
    zxdg_imported_v2::{self, ZxdgImportedV2},
    zxdg_importer_v2::ZxdgImporterV2,
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};
use wayland_protocols::xdg::system_bell::v1::client::xdg_system_bell_v1::XdgSystemBellV1;
use wayland_protocols::xdg::toplevel_tag::v1::client::xdg_toplevel_tag_manager_v1::XdgToplevelTagManagerV1;

const ROUNDS: usize = 10;

/// What a feedback object (by its tag) was told.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fate {
    Presented { seq: u64, refresh: u32 },
    Discarded,
}

#[derive(Default)]
struct Client {
    frames: usize,
    globals: HashMap<String, (u32, u32)>,
    clock_id: Option<u32>,
    fates: HashMap<u32, Fate>,
    inhibitor_active: Option<bool>,
    handle: Option<String>,
    /// xdg_imported destroyed events received (either version).
    imports_destroyed: usize,
    /// ext_background_effect_manager_v1 capabilities, as sent.
    background_effects: Option<u32>,
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
            state.frames += 1;
        }
    }
}

impl Client {
    fn bind<I>(&self, registry: &WlRegistry, qh: &QueueHandle<Self>, version: u32) -> I
    where
        I: wayland_client::Proxy + 'static,
        Self: Dispatch<I, ()>,
    {
        let (name, offered) = self.globals[I::interface().name];
        registry.bind(name, version.min(offered), qh, ())
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

impl Dispatch<WpPresentation, ()> for Client {
    fn event(
        state: &mut Self,
        _: &WpPresentation,
        event: wp_presentation::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wp_presentation::Event::ClockId { clk_id } = event {
            state.clock_id = Some(clk_id);
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
            wp_presentation_feedback::Event::Presented {
                refresh,
                seq_hi,
                seq_lo,
                ..
            } => {
                let seq = (u64::from(seq_hi) << 32) | u64::from(seq_lo);
                state.fates.insert(*tag, Fate::Presented { seq, refresh });
            }
            wp_presentation_feedback::Event::Discarded => {
                state.fates.insert(*tag, Fate::Discarded);
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwpKeyboardShortcutsInhibitorV1, ()> for Client {
    fn event(
        state: &mut Self,
        _: &ZwpKeyboardShortcutsInhibitorV1,
        event: zwp_keyboard_shortcuts_inhibitor_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_keyboard_shortcuts_inhibitor_v1::Event::Active => {
                state.inhibitor_active = Some(true)
            }
            zwp_keyboard_shortcuts_inhibitor_v1::Event::Inactive => {
                state.inhibitor_active = Some(false)
            }
            _ => {}
        }
    }
}

impl Dispatch<ZxdgExportedV2, ()> for Client {
    fn event(
        state: &mut Self,
        _: &ZxdgExportedV2,
        event: zxdg_exported_v2::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zxdg_exported_v2::Event::Handle { handle } = event {
            state.handle = Some(handle);
        }
    }
}

impl Dispatch<ZxdgExportedV1, ()> for Client {
    fn event(
        state: &mut Self,
        _: &ZxdgExportedV1,
        event: zxdg_exported_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zxdg_exported_v1::Event::Handle { handle } = event {
            state.handle = Some(handle);
        }
    }
}

impl Dispatch<ZxdgImportedV2, ()> for Client {
    fn event(
        state: &mut Self,
        _: &ZxdgImportedV2,
        event: zxdg_imported_v2::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zxdg_imported_v2::Event::Destroyed = event {
            state.imports_destroyed += 1;
        }
    }
}

impl Dispatch<ZxdgImportedV1, ()> for Client {
    fn event(
        state: &mut Self,
        _: &ZxdgImportedV1,
        event: zxdg_imported_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zxdg_imported_v1::Event::Destroyed = event {
            state.imports_destroyed += 1;
        }
    }
}

impl Dispatch<ExtBackgroundEffectManagerV1, ()> for Client {
    fn event(
        state: &mut Self,
        _: &ExtBackgroundEffectManagerV1,
        event: ext_background_effect_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let ext_background_effect_manager_v1::Event::Capabilities {
            flags: wayland_client::WEnum::Value(flags),
        } = event
        {
            state.background_effects = Some(flags.bits());
        }
    }
}

wayland_client::delegate_noop!(Client: ignore WlCompositor);
wayland_client::delegate_noop!(Client: ignore WlSurface);
wayland_client::delegate_noop!(Client: ignore WlSeat);
wayland_client::delegate_noop!(Client: ignore XdgToplevel);
wayland_client::delegate_noop!(Client: ignore WpFifoManagerV1);
wayland_client::delegate_noop!(Client: ignore WpFifoV1);
wayland_client::delegate_noop!(Client: ignore ZwpKeyboardShortcutsInhibitManagerV1);
wayland_client::delegate_noop!(Client: ignore XdgSystemBellV1);
wayland_client::delegate_noop!(Client: ignore XdgWmDialogV1);
wayland_client::delegate_noop!(Client: ignore XdgDialogV1);
wayland_client::delegate_noop!(Client: ignore XdgToplevelTagManagerV1);
wayland_client::delegate_noop!(Client: ignore ZxdgExporterV2);
wayland_client::delegate_noop!(Client: ignore ZxdgImporterV2);
wayland_client::delegate_noop!(Client: ignore ZxdgExporterV1);
wayland_client::delegate_noop!(Client: ignore ZxdgImporterV1);
wayland_client::delegate_noop!(Client: ignore ExtBackgroundEffectSurfaceV1);
wayland_client::delegate_noop!(Client: ignore wayland_client::protocol::wl_region::WlRegion);

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

    fn bind<I>(&self, version: u32) -> I
    where
        I: wayland_client::Proxy + 'static,
        Client: Dispatch<I, ()>,
    {
        self.client.bind(&self.registry, &self.qh(), version)
    }
}

/// A compositor with one 60 Hz output, as the nested session has.
fn compositor() -> (TestCompositor, WindowManager) {
    let mut comp = TestCompositor::new();
    let output = Output::new(
        "tuna-0".to_owned(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "Tuna Desktop".to_owned(),
            model: "Test".to_owned(),
        },
    );
    let mode = Mode {
        size: (1280, 800).into(),
        refresh: 60_000,
    };
    output.change_current_state(Some(mode), None, None, Some((0, 0).into()));
    comp.state.add_output("tuna-0", Some(output), 1280, 800);
    let manager = comp.window_manager();
    (comp, manager)
}

fn connect(comp: &mut TestCompositor, manager: &mut WindowManager) -> Peer {
    let (server, stream) = UnixStream::pair().unwrap();
    comp.add_client(server);
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

/// Map a toplevel titled `title`; returns its surface and role.
fn window(peer: &mut Peer, title: &str) -> (WlSurface, XdgToplevel) {
    let qh = peer.qh();
    let compositor: WlCompositor = peer.bind(6);
    let wm: XdgWmBase = peer.bind(6);
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title(title.into());
    surface.commit();
    // The protocol objects live with the connection.
    std::mem::forget((compositor, wm, xdg));
    (surface, toplevel)
}

fn id_of(manager: &WindowManager, title: &str) -> u64 {
    manager
        .model()
        .windows()
        .find(|w| w.title == title)
        .map(|w| w.id)
        .expect("window mapped")
}

fn focused_title(manager: &WindowManager) -> Option<String> {
    let id = manager.model().focused()?;
    manager
        .model()
        .windows()
        .find(|w| w.id == id)
        .map(|w| w.title.clone())
}

fn key(manager: &mut WindowManager, comp: &mut TestCompositor, keycode: u32, pressed: bool) {
    manager.on_input(
        &mut comp.state,
        ManagerInput::Key {
            keycode,
            pressed,
            time: 5000,
        },
    );
}

fn chord(manager: &mut WindowManager, comp: &mut TestCompositor, keycode: u32) {
    key(manager, comp, SUPER_LEFT_KEYCODE, true);
    key(manager, comp, keycode, true);
    key(manager, comp, keycode, false);
    key(manager, comp, SUPER_LEFT_KEYCODE, false);
}

#[test]
fn a_frame_request_without_pixel_damage_keeps_native_refresh_work_pending() {
    let (mut comp, mut manager) = compositor();
    let mut peer = connect(&mut comp, &mut manager);
    let (surface, _toplevel) = window(&mut peer, "idle client");
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    let roots = tuna_compositor::frame_timing::frame_roots(&comp.state, &manager);
    assert!(!tuna_compositor::frame_timing::pending_frame_work(&roots));
    let _callback = surface.frame(&peer.qh(), ());
    // No buffer attachment or damage: this is a request for pacing alone.
    surface.commit();
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert!(tuna_compositor::frame_timing::pending_frame_work(&roots));
    assert!(
        tuna_compositor::frame_timing::pending_output_work(&roots, &[], &[]),
        "callback-only pacing remains owed even without visible presentation"
    );
    assert!(
        !tuna_compositor::frame_timing::pending_output_work(&roots, &[], &roots),
        "a hidden window's callback waits for its beat, not a scanout"
    );
    assert!(
        !tuna_compositor::frame_timing::pending_output_work(&roots, &roots, &roots),
        "an occluded window on the active workspace is drawn yet waits for its beat"
    );
    assert_eq!(peer.client.frames, 0);
    let output = comp.state.primary_output().unwrap();
    for root in &roots {
        smithay::desktop::utils::send_frames_surface_tree(
            root,
            &output,
            std::time::Duration::from_millis(20),
            Some(std::time::Duration::ZERO),
            |_, _| Some(output.clone()),
        );
    }
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert_eq!(peer.client.frames, 1);
    assert!(!tuna_compositor::frame_timing::pending_frame_work(&roots));
}

#[test]
fn presentation_feedback_reports_the_frame_that_drew_the_window() {
    let (mut comp, mut manager) = compositor();
    let mut peer = connect(&mut comp, &mut manager);
    let presentation: WpPresentation = peer.bind(2);
    let (surface, _toplevel) = window(&mut peer, "video");
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert_eq!(
        peer.client.clock_id,
        Some(libc::CLOCK_MONOTONIC as u32),
        "Mutter reports on CLOCK_MONOTONIC"
    );

    let _feedback = presentation.feedback(&surface, &peer.qh(), 1);
    surface.commit();
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert!(peer.client.fates.is_empty(), "nothing drawn yet");
    let roots = tuna_compositor::frame_timing::frame_roots(&comp.state, &manager);
    assert!(tuna_compositor::frame_timing::pending_frame_work(&roots));

    tuna_compositor::frame_timing::present_nested_frame(&mut comp.state, &manager, false, 42);
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert_eq!(
        peer.client.fates.get(&1),
        Some(&Fate::Presented {
            seq: 42,
            refresh: 16_666_666,
        })
    );
    assert!(!tuna_compositor::frame_timing::pending_frame_work(&roots));
}

#[test]
fn a_hidden_window_keeps_its_feedback_until_drawn() {
    let (mut comp, mut manager) = compositor();
    let mut peer = connect(&mut comp, &mut manager);
    let presentation: WpPresentation = peer.bind(2);
    let (surface, _toplevel) = window(&mut peer, "video");
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    let _feedback = presentation.feedback(&surface, &peer.qh(), 7);
    surface.commit();
    pump(&mut comp, &mut manager, &mut [&mut peer]);

    // Locked: only the lock screen is drawn.
    tuna_compositor::frame_timing::present_nested_frame(&mut comp.state, &manager, true, 1);
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert_eq!(peer.client.fates.get(&7), None);
    let roots = tuna_compositor::frame_timing::frame_roots(&comp.state, &manager);
    assert!(tuna_compositor::frame_timing::pending_frame_work(&roots));
    assert!(
        !tuna_compositor::frame_timing::pending_output_work(&roots, &[], &[]),
        "hidden presentation must not keep an unchanged output repainting"
    );
    let drawn: Vec<_> = tuna_compositor::frame_timing::drawn_roots(&comp.state, &manager, false)
        .into_iter()
        .map(|(surface, _)| surface)
        .collect();
    assert!(
        tuna_compositor::frame_timing::pending_output_work(&roots, &drawn, &[]),
        "revealing the actual window owes a real submission"
    );
    assert_eq!(
        peer.client.fates.get(&7),
        None,
        "visibility check does not resolve feedback"
    );

    tuna_compositor::frame_timing::present_nested_frame(&mut comp.state, &manager, false, 2);
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert!(matches!(
        peer.client.fates.get(&7),
        Some(Fate::Presented { seq: 2, .. })
    ));
}

#[test]
fn fifo_holds_a_waiting_commit_until_the_next_refresh() {
    let (mut comp, mut manager) = compositor();
    let mut peer = connect(&mut comp, &mut manager);
    let presentation: WpPresentation = peer.bind(2);
    let fifo_manager: WpFifoManagerV1 = peer.bind(1);
    let (surface, _toplevel) = window(&mut peer, "game");
    pump(&mut comp, &mut manager, &mut [&mut peer]);

    let fifo = fifo_manager.get_fifo(&surface, &peer.qh(), ());
    fifo.set_barrier();
    surface.commit();
    // The next update waits for a refresh after the barrier.
    fifo.wait_barrier();
    fifo.set_barrier();
    let _feedback = presentation.feedback(&surface, &peer.qh(), 2);
    surface.commit();
    pump(&mut comp, &mut manager, &mut [&mut peer]);

    // The first refresh draws what was applied (not the held update),
    // then releases the barrier.
    let roots = tuna_compositor::frame_timing::frame_roots(&comp.state, &manager);
    assert!(tuna_compositor::frame_timing::pending_frame_work(&roots));
    assert!(
        tuna_compositor::frame_timing::pending_output_work(&roots, &[], &roots),
        "a hidden window's FIFO barrier still advances on each refresh"
    );
    tuna_compositor::frame_timing::present_nested_frame(&mut comp.state, &manager, false, 1);
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert_eq!(peer.client.fates.get(&2), None, "held behind the barrier");

    tuna_compositor::frame_timing::present_nested_frame(&mut comp.state, &manager, false, 2);
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert!(matches!(
        peer.client.fates.get(&2),
        Some(Fate::Presented { seq: 2, .. })
    ));
}

#[test]
fn an_inhibiting_window_gets_the_shortcuts_until_super_escape() {
    let (mut comp, mut manager) = compositor();
    let mut peer = connect(&mut comp, &mut manager);
    let seat: WlSeat = peer.bind(7);
    let inhibit: ZwpKeyboardShortcutsInhibitManagerV1 = peer.bind(1);
    let (surface, _toplevel) = window(&mut peer, "vm");
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert_eq!(focused_title(&manager).as_deref(), Some("vm"));

    let _inhibitor = inhibit.inhibit_shortcuts(&surface, &seat, &peer.qh(), ());
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert_eq!(peer.client.inhibitor_active, Some(false));
    assert!(!comp.state.shortcuts_inhibited());
    let request = comp.state.shortcut_consent_request().unwrap().0;
    comp.state.answer_shortcut_consent(request, true);
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert_eq!(peer.client.inhibitor_active, Some(true));
    assert!(comp.state.shortcuts_inhibited());

    // Super+PageDown goes to the virtual machine, not the workspaces.
    chord(&mut manager, &mut comp, PAGE_DOWN_KEYCODE);
    assert_eq!(manager.model().active_workspace(), 0);

    // Registered shell grabs must obey the same consent. This catches a
    // global accelerator bypass even when built-in workspace keys work.
    manager.set_accelerators(vec![tuna_shell_control::Accelerator {
        action: 77,
        keysym: 0xff56, // Page_Down
        mods: tuna_shell_control::MOD_LOGO,
        modes: tuna_shell_control::MODE_NORMAL,
    }]);
    chord(&mut manager, &mut comp, PAGE_DOWN_KEYCODE);
    assert!(manager.take_accelerators_fired().is_empty());

    // Super+Escape (Mutter's restore-shortcuts) takes them back.
    chord(&mut manager, &mut comp, ESCAPE_KEYCODE);
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert!(!comp.state.shortcuts_inhibited());
    assert_eq!(peer.client.inhibitor_active, Some(false));
    chord(&mut manager, &mut comp, PAGE_DOWN_KEYCODE);
    assert_eq!(
        manager.take_accelerators_fired(),
        [(77, 5000, tuna_shell_control::MODE_NORMAL)]
    );
    manager.set_accelerators(vec![]);
    chord(&mut manager, &mut comp, PAGE_DOWN_KEYCODE);
    assert_eq!(manager.model().active_workspace(), 1);
}

#[test]
fn denied_shortcut_inhibition_stays_denied_after_refocus() {
    let (mut comp, mut manager) = compositor();
    let mut peer = connect(&mut comp, &mut manager);
    let seat: WlSeat = peer.bind(7);
    let inhibit: ZwpKeyboardShortcutsInhibitManagerV1 = peer.bind(1);
    let (surface, _) = window(&mut peer, "denied-vm");
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    let _inhibitor = inhibit.inhibit_shortcuts(&surface, &seat, &peer.qh(), ());
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    let request = comp.state.shortcut_consent_request().unwrap().0;
    comp.state.answer_shortcut_consent(request, false);
    let vm = id_of(&manager, "denied-vm");
    let _other = window(&mut peer, "other");
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    manager.focus(&mut comp.state, Some(vm));
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert_eq!(peer.client.inhibitor_active, Some(false));
    assert!(!comp.state.shortcuts_inhibited());
    chord(&mut manager, &mut comp, PAGE_DOWN_KEYCODE);
    assert_eq!(manager.model().active_workspace(), 1);
}

#[test]
fn shortcut_consent_cannot_survive_a_lock_or_focus_change() {
    let (mut comp, mut manager) = compositor();
    let mut peer = connect(&mut comp, &mut manager);
    let seat: WlSeat = peer.bind(7);
    let inhibit: ZwpKeyboardShortcutsInhibitManagerV1 = peer.bind(1);
    let (surface, _) = window(&mut peer, "vm");
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    let inhibitor = inhibit.inhibit_shortcuts(&surface, &seat, &peer.qh(), ());
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    let request = comp.state.shortcut_consent_request().unwrap().0;
    let _other = window(&mut peer, "other");
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    comp.state.answer_shortcut_consent(request, true);
    let vm = id_of(&manager, "vm");
    manager.focus(&mut comp.state, Some(vm));
    assert!(!comp.state.shortcuts_inhibited());
    assert!(comp.state.shortcut_consent_request().is_none());
    inhibitor.destroy();
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    let _inhibitor = inhibit.inhibit_shortcuts(&surface, &seat, &peer.qh(), ());
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    let request = comp.state.shortcut_consent_request().unwrap().0;
    comp.state.set_shortcut_inhibition_locked(true);
    comp.state.answer_shortcut_consent(request, true);
    comp.state.set_shortcut_inhibition_locked(false);
    assert!(!comp.state.shortcuts_inhibited());
    assert!(comp.state.shortcut_consent_request().is_none());
}

#[test]
fn modal_dialogs_keep_focus_from_their_parent() {
    let (mut comp, mut manager) = compositor();
    let mut peer = connect(&mut comp, &mut manager);
    let dialogs: XdgWmDialogV1 = peer.bind(1);
    let (_main, main_toplevel) = window(&mut peer, "main");
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    let (child, child_toplevel) = window(&mut peer, "save");
    child_toplevel.set_parent(Some(&main_toplevel));
    let dialog = dialogs.get_xdg_dialog(&child_toplevel, &peer.qh(), ());
    dialog.set_modal();
    child.commit();
    pump(&mut comp, &mut manager, &mut [&mut peer]);

    let main = id_of(&manager, "main");
    manager.focus(&mut comp.state, Some(main));
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert_eq!(focused_title(&manager).as_deref(), Some("save"));

    // No longer modal: the parent may take focus again.
    dialog.unset_modal();
    child.commit();
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    manager.focus(&mut comp.state, Some(main));
    assert_eq!(focused_title(&manager).as_deref(), Some("main"));
}

#[test]
fn exported_windows_parent_another_clients_dialog() {
    let (mut comp, mut manager) = compositor();
    let mut app = connect(&mut comp, &mut manager);
    let mut portal = connect(&mut comp, &mut manager);
    let exporter: ZxdgExporterV2 = app.bind(1);
    let importer: ZxdgImporterV2 = portal.bind(1);
    let (app_surface, _app_toplevel) = window(&mut app, "app");
    let (dialog_surface, _dialog_toplevel) = window(&mut portal, "file chooser");
    pump(&mut comp, &mut manager, &mut [&mut app, &mut portal]);

    let _exported = exporter.export_toplevel(&app_surface, &app.qh(), ());
    pump(&mut comp, &mut manager, &mut [&mut app, &mut portal]);
    let handle = app.client.handle.clone().expect("export handle");
    let imported = importer.import_toplevel(handle, &portal.qh(), ());
    imported.set_parent_of(&dialog_surface);
    pump(&mut comp, &mut manager, &mut [&mut app, &mut portal]);

    let toplevels = comp.state.toplevels();
    assert_eq!(toplevels.len(), 2);
    assert!(
        toplevels.iter().any(|child| child
            .parent()
            .is_some_and(|parent| toplevels.iter().any(|t| *t.wl_surface() == parent))),
        "the portal's dialog is parented to the app's window"
    );
}

/// The window titled `child` has the window titled `parent` as its
/// parent.
fn parented(comp: &TestCompositor, child: &str, parent: &str) -> bool {
    use smithay::wayland::{compositor::with_states, shell::xdg::XdgToplevelSurfaceData};
    let toplevels = comp.state.toplevels();
    let titled = |title: &str| {
        toplevels
            .iter()
            .find(|t| {
                with_states(t.wl_surface(), |states| {
                    states
                        .data_map
                        .get::<XdgToplevelSurfaceData>()
                        .is_some_and(|data| data.lock().unwrap().title.as_deref() == Some(title))
                })
            })
            .cloned()
    };
    let (Some(child), Some(parent)) = (titled(child), titled(parent)) else {
        return false;
    };
    child.parent().as_ref() == Some(parent.wl_surface())
}

#[test]
fn gtk3_v1_exports_import_through_either_version_and_revoke() {
    // Mutter keeps one handle table for zxdg_foreign v1 and v2
    // (meta-wayland-legacy-xdg-foreign.c calls the v2 export/import
    // functions): GTK 3 exports with v1 and the portal imports with v2.
    let (mut comp, mut manager) = compositor();
    let mut gtk3 = connect(&mut comp, &mut manager);
    let mut portal = connect(&mut comp, &mut manager);
    assert_eq!(gtk3.client.globals["zxdg_exporter_v1"].1, 1);
    assert_eq!(gtk3.client.globals["zxdg_importer_v1"].1, 1);
    let exporter_v1: ZxdgExporterV1 = gtk3.bind(1);
    let importer_v1: ZxdgImporterV1 = gtk3.bind(1);
    let exporter_v2: ZxdgExporterV2 = portal.bind(1);
    let importer_v2: ZxdgImporterV2 = portal.bind(1);
    let (app_surface, _app) = window(&mut gtk3, "gtk3 app");
    let (dialog_surface, _dialog) = window(&mut portal, "portal chooser");
    pump(&mut comp, &mut manager, &mut [&mut gtk3, &mut portal]);

    // v1 export, v2 import.
    let exported = exporter_v1.export(&app_surface, &gtk3.qh(), ());
    pump(&mut comp, &mut manager, &mut [&mut gtk3, &mut portal]);
    let handle = gtk3.client.handle.take().expect("v1 export handle");
    let imported = importer_v2.import_toplevel(handle.clone(), &portal.qh(), ());
    imported.set_parent_of(&dialog_surface);
    pump(&mut comp, &mut manager, &mut [&mut gtk3, &mut portal]);
    assert!(parented(&comp, "portal chooser", "gtk3 app"));
    assert_eq!(portal.client.imports_destroyed, 0);

    // Revoking the v1 export destroys the v2 import and frees the dialog
    // (Mutter's meta_wayland_xdg_exported_destroy).
    exported.destroy();
    pump(&mut comp, &mut manager, &mut [&mut gtk3, &mut portal]);
    assert_eq!(portal.client.imports_destroyed, 1);
    assert!(!parented(&comp, "portal chooser", "gtk3 app"));
    // The revoked handle no longer imports, through either version.
    let stale = importer_v2.import_toplevel(handle, &portal.qh(), ());
    pump(&mut comp, &mut manager, &mut [&mut gtk3, &mut portal]);
    assert_eq!(portal.client.imports_destroyed, 2);
    stale.set_parent_of(&dialog_surface);
    pump(&mut comp, &mut manager, &mut [&mut gtk3, &mut portal]);
    assert!(!parented(&comp, "portal chooser", "gtk3 app"));

    // v2 export, v1 import, and the import's own destruction unparents.
    let _exported = exporter_v2.export_toplevel(&dialog_surface, &portal.qh(), ());
    pump(&mut comp, &mut manager, &mut [&mut gtk3, &mut portal]);
    let handle = portal.client.handle.take().expect("v2 export handle");
    let imported = importer_v1.import(handle, &gtk3.qh(), ());
    imported.set_parent_of(&app_surface);
    pump(&mut comp, &mut manager, &mut [&mut gtk3, &mut portal]);
    assert!(parented(&comp, "gtk3 app", "portal chooser"));
    imported.destroy();
    pump(&mut comp, &mut manager, &mut [&mut gtk3, &mut portal]);
    assert!(!parented(&comp, "gtk3 app", "portal chooser"));
    assert_eq!(gtk3.client.imports_destroyed, 0);

    // An unknown v1 handle is destroyed at once.
    let _unknown = importer_v1.import("not-a-handle".into(), &gtk3.qh(), ());
    pump(&mut comp, &mut manager, &mut [&mut gtk3, &mut portal]);
    assert_eq!(gtk3.client.imports_destroyed, 1);
}

#[test]
fn running_app_eligibility_follows_real_transient_parent_changes() {
    let (mut comp, mut manager) = compositor();
    let mut app = connect(&mut comp, &mut manager);
    let mut portal = connect(&mut comp, &mut manager);
    let exporter: ZxdgExporterV2 = app.bind(1);
    let importer: ZxdgImporterV2 = portal.bind(1);
    let (parent_surface, parent) = window(&mut app, "app parent");
    let (_child_surface, child) = window(&mut app, "app dialog");
    let (picker_surface, picker) = window(&mut portal, "portal picker");
    parent.set_app_id("org.gnome.TextEditor".into());
    child.set_app_id("org.gnome.EditorDialog".into());
    picker.set_app_id("org.gnome.Nautilus".into());
    pump(&mut comp, &mut manager, &mut [&mut app, &mut portal]);
    let id = |manager: &WindowManager, title: &str| {
        manager
            .model()
            .windows()
            .find(|w| w.title == title)
            .unwrap()
            .id
    };
    let parent_id = id(&manager, "app parent");
    let child_id = id(&manager, "app dialog");
    let picker_id = id(&manager, "portal picker");
    assert!(manager.is_standalone(child_id));
    assert!(manager.is_standalone(picker_id));
    child.set_parent(Some(&parent));
    pump(&mut comp, &mut manager, &mut [&mut app, &mut portal]);
    assert!(!manager.is_standalone(child_id));
    assert!(
        manager.is_introspect_eligible(child_id),
        "native dialogs remain selectable"
    );
    assert_eq!(manager.application_window(child_id), Some(parent_id));
    child.set_parent(None);
    pump(&mut comp, &mut manager, &mut [&mut app, &mut portal]);
    assert!(manager.is_standalone(child_id));
    let exported = exporter.export_toplevel(&parent_surface, &app.qh(), ());
    pump(&mut comp, &mut manager, &mut [&mut app, &mut portal]);
    let imported = importer.import_toplevel(app.client.handle.clone().unwrap(), &portal.qh(), ());
    imported.set_parent_of(&picker_surface);
    pump(&mut comp, &mut manager, &mut [&mut app, &mut portal]);
    assert!(!manager.is_standalone(picker_id));
    assert!(
        manager.is_introspect_eligible(picker_id),
        "foreign-parented pickers remain selectable"
    );
    assert_eq!(manager.application_window(picker_id), Some(parent_id));
    assert_eq!(
        manager.model().window(picker_id).unwrap().app_id.as_deref(),
        Some("org.gnome.Nautilus")
    );
    exported.destroy();
    pump(&mut comp, &mut manager, &mut [&mut app, &mut portal]);
    assert!(
        manager.is_standalone(picker_id),
        "withdrawn foreign parent clears eligibility"
    );
    assert_eq!(manager.application_window(picker_id), Some(picker_id));
    picker.destroy();
    picker_surface.destroy();
    pump(&mut comp, &mut manager, &mut [&mut app, &mut portal]);
    assert!(
        !manager.is_standalone(picker_id),
        "unmapped window is not a running app"
    );
    assert_eq!(manager.application_window(picker_id), None);
    assert!(!manager.is_introspect_eligible(picker_id));
    assert!(!manager.is_introspect_eligible(u64::MAX));
}

#[test]
fn the_system_bell_rings() {
    // No sound from the test run.
    std::env::set_var("TUNA_BELL", "0");
    let (mut comp, mut manager) = compositor();
    let mut peer = connect(&mut comp, &mut manager);
    let bell: XdgSystemBellV1 = peer.bind(1);
    let (surface, _toplevel) = window(&mut peer, "terminal");
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert_eq!(comp.state.bell_rings(), 0);
    bell.ring(Some(&surface));
    bell.ring(None);
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert_eq!(comp.state.bell_rings(), 2);
}

#[test]
fn windows_keep_their_toplevel_tag() {
    let (mut comp, mut manager) = compositor();
    let mut peer = connect(&mut comp, &mut manager);
    let tags: XdgToplevelTagManagerV1 = peer.bind(1);
    let (_surface, toplevel) = window(&mut peer, "mail");
    tags.set_toplevel_tag(&toplevel, "composer".into());
    tags.set_toplevel_description(&toplevel, "New message".into());
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    let surface = comp.state.toplevels()[0].wl_surface().clone();
    let tag = tuna_compositor::protocols::toplevel_tag(&surface);
    assert_eq!(tag.tag.as_deref(), Some("composer"));
    assert_eq!(tag.description.as_deref(), Some("New message"));
}

#[test]
fn parented_dialogs_remain_centered_on_left_and_upper_outputs() {
    for parent_location in [(-1100, 50), (100, -750)] {
        for foreign in [false, true] {
            let (mut comp, mut manager) = compositor();
            for (name, location) in [("left", (-1280, 0)), ("above", (0, -800))] {
                let output = Output::new(
                    name.into(),
                    PhysicalProperties {
                        size: (0, 0).into(),
                        subpixel: Subpixel::Unknown,
                        make: "Tuna Desktop".into(),
                        model: "Dialog topology test".into(),
                    },
                );
                output.change_current_state(
                    Some(Mode {
                        size: (1280, 800).into(),
                        refresh: 60_000,
                    }),
                    None,
                    None,
                    Some(location.into()),
                );
                comp.state.add_output(name, Some(output), 1280, 800);
                comp.state.set_output_location(name, location);
            }
            let mut app = connect(&mut comp, &mut manager);
            let (parent_surface, parent_toplevel) = window(&mut app, "negative-origin parent");
            pump(&mut comp, &mut manager, &mut [&mut app]);
            let parent = id_of(&manager, "negative-origin parent");
            let initial = manager.geometry(parent).unwrap();
            assert!(manager.move_window(
                parent,
                parent_location.0 - initial.loc.x,
                parent_location.1 - initial.loc.y
            ));
            let parent_geometry = manager.geometry(parent).unwrap();
            let mut portal_peer = None;
            if foreign {
                let exporter: ZxdgExporterV2 = app.bind(1);
                let _exported = exporter.export_toplevel(&parent_surface, &app.qh(), ());
                pump(&mut comp, &mut manager, &mut [&mut app]);
                let mut portal = connect(&mut comp, &mut manager);
                let importer: ZxdgImporterV2 = portal.bind(1);
                let imported =
                    importer.import_toplevel(app.client.handle.clone().unwrap(), &portal.qh(), ());
                let (surface, _) = window(&mut portal, "negative-origin chooser");
                imported.set_parent_of(&surface);
                surface.commit();
                pump(&mut comp, &mut manager, &mut [&mut app, &mut portal]);
                portal_peer = Some(portal);
            } else {
                let (surface, toplevel) = window(&mut app, "negative-origin chooser");
                toplevel.set_parent(Some(&parent_toplevel));
                surface.commit();
                pump(&mut comp, &mut manager, &mut [&mut app]);
            }
            let child = id_of(&manager, "negative-origin chooser");
            let child_geometry = manager.geometry(child).unwrap();
            let expected = (
                parent_geometry.loc.x + (parent_geometry.size.w - child_geometry.size.w) / 2,
                parent_geometry.loc.y + (parent_geometry.size.h - child_geometry.size.h) / 2,
            );
            assert_eq!(
                (child_geometry.loc.x, child_geometry.loc.y),
                expected,
                "foreign={foreign}, parent={parent_location:?}"
            );
            assert_eq!(manager.geometry(parent).unwrap(), parent_geometry);
            drop(portal_peer);
        }
    }
}

#[test]
fn minimized_parent_hides_transients_and_child_activation_restores_ancestors() {
    let (mut comp, mut manager) = compositor();
    let mut peer = connect(&mut comp, &mut manager);
    let (_, parent) = window(&mut peer, "parent-minimize");
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    let (child_surface, child) = window(&mut peer, "child-minimize");
    child.set_parent(Some(&parent));
    child_surface.commit();
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    let parent_id = id_of(&manager, "parent-minimize");
    let child_id = id_of(&manager, "child-minimize");
    manager.focus(&mut comp.state, Some(child_id));
    assert!(manager.minimize(&mut comp.state, parent_id));
    assert!(manager.visible_windows().is_empty());
    assert!(!manager.is_minimized(child_id));
    assert_eq!(manager.model().focused(), None);
    manager.focus(&mut comp.state, Some(child_id));
    assert!(!manager.is_minimized(parent_id));
    assert_eq!(manager.visible_windows().len(), 2);
    assert_eq!(manager.model().focused(), Some(child_id));
}

#[test]
fn dash_icon_targets_ignore_unknown_windows_and_retire_destroyed_window_targets() {
    let (mut comp, mut manager) = compositor();
    let mut peer = connect(&mut comp, &mut manager);
    let (surface, top) = window(&mut peer, "dash-target-owner");
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    let id = id_of(&manager, "dash-target-owner");
    let target = smithay::utils::Rectangle::new((-500, 700).into(), (64, 64).into());
    manager.set_icon_geometries(vec![(id, target), (u64::MAX, target)]);
    assert_eq!(manager.icon_geometry(id), Some(target));
    assert_eq!(manager.icon_geometry(u64::MAX), None);
    manager.set_icon_geometries(Vec::new());
    assert_eq!(
        manager.icon_geometry(id),
        None,
        "full replacement selects fallback"
    );
    manager.set_icon_geometries(vec![(id, target)]);
    top.destroy();
    surface.destroy();
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert_eq!(
        manager.icon_geometry(id),
        None,
        "destroy must retire old target"
    );
    manager.set_icon_geometries(vec![(id, target)]);
    assert_eq!(
        manager.icon_geometry(id),
        None,
        "stale shell publication is ignored"
    );
}

#[test]
fn restoring_parent_preserves_independent_child_minimize() {
    let (mut comp, mut manager) = compositor();
    let mut peer = connect(&mut comp, &mut manager);
    let (_, parent) = window(&mut peer, "independent-parent");
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    let (surface, child) = window(&mut peer, "independent-child");
    child.set_parent(Some(&parent));
    surface.commit();
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    let parent_id = id_of(&manager, "independent-parent");
    let child_id = id_of(&manager, "independent-child");
    manager.minimize(&mut comp.state, child_id);
    manager.minimize(&mut comp.state, parent_id);
    manager.focus(&mut comp.state, Some(parent_id));
    assert!(!manager.is_minimized(parent_id));
    assert!(manager.is_minimized(child_id));
    assert_eq!(manager.visible_windows().len(), 1);
    assert_eq!(manager.model().focused(), Some(parent_id));
}

#[test]
fn child_mapping_and_closing_cannot_restore_hidden_parent() {
    let (mut comp, mut manager) = compositor();
    let mut peer = connect(&mut comp, &mut manager);
    let (_, parent) = window(&mut peer, "hidden-parent");
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    let parent_id = id_of(&manager, "hidden-parent");
    manager.minimize(&mut comp.state, parent_id);
    let (surface, child) = window(&mut peer, "hidden-new-child");
    child.set_parent(Some(&parent));
    surface.commit();
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert!(manager.is_minimized(parent_id));
    assert!(manager.visible_windows().is_empty());
    assert_eq!(manager.model().focused(), None);
    child.destroy();
    surface.destroy();
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert!(manager.is_minimized(parent_id));
    assert!(manager.visible_windows().is_empty());
    assert_eq!(manager.model().focused(), None);
}

/// The committed blur region's area, `None` without an effect.
fn blur_area(
    surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
) -> Option<i32> {
    tuna_compositor::background_effect::blur_region(surface)
        .map(|rects| rects.iter().map(|r| r.size.w * r.size.h).sum())
}

/// Pump until the compositor kills the connection, which then refuses
/// further I/O; returns the protocol error it sent.
fn pump_to_error(
    comp: &mut TestCompositor,
    manager: &mut WindowManager,
    peer: &mut Peer,
) -> wayland_client::backend::protocol::ProtocolError {
    for _ in 0..ROUNDS {
        let _ = peer.queue.flush();
        comp.pump();
        manager.reconcile(&mut comp.state);
        comp.pump();
        if let Some(guard) = peer.queue.prepare_read() {
            let _ = guard.read();
        }
        let _ = peer.queue.dispatch_pending(&mut peer.client);
        if let Some(error) = peer._conn.protocol_error() {
            return error;
        }
    }
    panic!("no protocol error");
}

#[test]
fn background_blur_regions_apply_on_commit_like_mutter() {
    let (mut comp, mut manager) = compositor();
    let mut peer = connect(&mut comp, &mut manager);
    // meta_wayland_init_background_effect: version 1, always offered.
    assert_eq!(
        peer.client
            .globals
            .get("ext_background_effect_manager_v1")
            .map(|g| g.1),
        Some(1)
    );
    let effects: ExtBackgroundEffectManagerV1 = peer.bind(1);
    let compositor: WlCompositor = peer.bind(6);
    let (surface, _toplevel) = window(&mut peer, "terminal");
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    // Told at bind: blur is supported.
    assert_eq!(
        peer.client.background_effects,
        Some(ext_background_effect_manager_v1::Capability::Blur.bits())
    );
    let server = comp.state.toplevels()[0].wl_surface().clone();
    assert_eq!(blur_area(&server), None, "no effect until asked");

    let qh = peer.qh();
    let effect = effects.get_background_effect(&surface, &qh, ());
    let region = compositor.create_region(&qh, ());
    region.add(10, 20, 100, 50);
    region.subtract(10, 20, 10, 10);
    effect.set_blur_region(Some(&region));
    // Copy semantics: the region may go before the commit.
    region.destroy();
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert_eq!(
        blur_area(&server),
        None,
        "double-buffered: pending until commit"
    );
    surface.commit();
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert_eq!(blur_area(&server), Some(100 * 50 - 10 * 10));

    // A NULL region removes the effect, on commit.
    effect.set_blur_region(None);
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert_eq!(blur_area(&server), Some(100 * 50 - 10 * 10));
    surface.commit();
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert_eq!(blur_area(&server), None);

    // Destroying the object unsets the region on the next commit, and
    // the surface may take a new object.
    let region = compositor.create_region(&qh, ());
    region.add(0, 0, 64, 64);
    effect.set_blur_region(Some(&region));
    surface.commit();
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert_eq!(blur_area(&server), Some(64 * 64));
    effect.destroy();
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert_eq!(blur_area(&server), Some(64 * 64));
    surface.commit();
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert_eq!(blur_area(&server), None);
    let again = effects.get_background_effect(&surface, &qh, ());
    again.set_blur_region(Some(&region));
    surface.commit();
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert!(peer._conn.protocol_error().is_none());
    assert_eq!(blur_area(&server), Some(64 * 64));

    // A second object on the surface: background_effect_exists.
    let _second = effects.get_background_effect(&surface, &qh, ());
    let error = pump_to_error(&mut comp, &mut manager, &mut peer);
    assert_eq!(error.object_interface, "ext_background_effect_manager_v1");
    assert_eq!(
        error.code,
        ext_background_effect_manager_v1::Error::BackgroundEffectExists as u32
    );
}

#[test]
fn a_blur_region_for_a_destroyed_surface_is_a_protocol_error() {
    let (mut comp, mut manager) = compositor();
    let mut peer = connect(&mut comp, &mut manager);
    let effects: ExtBackgroundEffectManagerV1 = peer.bind(1);
    let compositor: WlCompositor = peer.bind(6);
    let qh = peer.qh();
    let surface = compositor.create_surface(&qh, ());
    let effect = effects.get_background_effect(&surface, &qh, ());
    surface.destroy();
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert!(
        peer._conn.protocol_error().is_none(),
        "the object turns inert"
    );
    effect.set_blur_region(None);
    let error = pump_to_error(&mut comp, &mut manager, &mut peer);
    assert_eq!(error.object_interface, "ext_background_effect_surface_v1");
    assert_eq!(
        error.code,
        ext_background_effect_surface_v1::Error::SurfaceDestroyed as u32
    );
}

/// linux-drm-syncobj v1 (explicit sync) against a stand-in import device:
/// the errors Mutter's `meta-wayland-linux-drm-syncobj.c` raises that need
/// no timeline syncobj. The checks that do (acquire and release points on
/// a dmabuf, `no_buffer`, `conflicting_points`) are unit tests of the same
/// check in `explicit_sync.rs`; a timeline-capable GPU runs them for real.
#[cfg(feature = "drm")]
mod explicit_sync {
    use super::*;
    use std::os::fd::AsFd;
    use wayland_client::protocol::{wl_buffer::WlBuffer, wl_shm::WlShm, wl_shm_pool::WlShmPool};
    use wayland_protocols::wp::linux_dmabuf::zv1::client::{
        zwp_linux_buffer_params_v1::{self, ZwpLinuxBufferParamsV1},
        zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1,
    };
    use wayland_protocols::wp::linux_drm_syncobj::v1::client::{
        wp_linux_drm_syncobj_manager_v1::{self, WpLinuxDrmSyncobjManagerV1},
        wp_linux_drm_syncobj_surface_v1::{self, WpLinuxDrmSyncobjSurfaceV1},
        wp_linux_drm_syncobj_timeline_v1::WpLinuxDrmSyncobjTimelineV1,
    };

    wayland_client::delegate_noop!(Client: ignore WpLinuxDrmSyncobjManagerV1);
    wayland_client::delegate_noop!(Client: ignore WpLinuxDrmSyncobjSurfaceV1);
    wayland_client::delegate_noop!(Client: ignore WpLinuxDrmSyncobjTimelineV1);
    wayland_client::delegate_noop!(Client: ignore ZwpLinuxDmabufV1);
    wayland_client::delegate_noop!(Client: ignore ZwpLinuxBufferParamsV1);
    wayland_client::delegate_noop!(Client: ignore WlShm);
    wayland_client::delegate_noop!(Client: ignore WlShmPool);
    wayland_client::delegate_noop!(Client: ignore WlBuffer);

    const SIZE: i32 = 4;

    /// A compositor offering linux-dmabuf and explicit sync, timelines
    /// importing on /dev/null (so every import fails).
    fn compositor() -> (TestCompositor, WindowManager) {
        let (mut comp, manager) = super::compositor();
        comp.state.enable_dmabuf(
            vec![smithay::backend::allocator::Format {
                code: smithay::backend::allocator::Fourcc::Argb8888,
                modifier: smithay::backend::allocator::Modifier::Linear,
            }],
            None,
        );
        let null = std::fs::File::open("/dev/null").unwrap();
        let device = smithay::backend::drm::DrmDeviceFd::new(smithay::utils::DeviceFd::from(
            std::os::fd::OwnedFd::from(null),
        ));
        assert!(comp
            .state
            .enable_explicit_sync(tuna_compositor::explicit_sync::SyncobjSupport {
                native_fence_sync: true,
                device_path: Some("/dev/null".into()),
                device: Some(device),
                timeline: true,
                eventfd: true,
            }));
        (comp, manager)
    }

    /// Like [`super::pump`], but a client may be disconnected on the way.
    fn pump(comp: &mut TestCompositor, manager: &mut WindowManager, peer: &mut Peer) {
        for _ in 0..ROUNDS {
            let _ = peer.queue.flush();
            comp.pump();
            manager.reconcile(&mut comp.state);
            comp.pump();
            if let Some(guard) = peer.queue.prepare_read() {
                let _ = guard.read();
            }
            let _ = peer.queue.dispatch_pending(&mut peer.client);
        }
    }

    /// The protocol error the compositor posted, as `(interface, code)`.
    fn error(peer: &Peer) -> Option<(String, u32)> {
        peer._conn
            .protocol_error()
            .map(|e| (e.object_interface, e.code))
    }

    /// A fresh client's surface and the syncobj manager.
    fn syncobj_client(
        comp: &mut TestCompositor,
        manager: &mut WindowManager,
    ) -> (Peer, WpLinuxDrmSyncobjManagerV1, WlSurface) {
        let peer = connect(comp, manager);
        let compositor: WlCompositor = peer.bind(6);
        let surface = compositor.create_surface(&peer.qh(), ());
        let syncobj: WpLinuxDrmSyncobjManagerV1 = peer.bind(1);
        (peer, syncobj, surface)
    }

    fn shm_buffer(peer: &Peer) -> WlBuffer {
        let file = tempfile::tempfile().unwrap();
        file.set_len((SIZE * SIZE * 4) as u64).unwrap();
        let shm: WlShm = peer.bind(1);
        let pool = shm.create_pool(file.as_fd(), SIZE * SIZE * 4, &peer.qh(), ());
        pool.create_buffer(
            0,
            SIZE,
            SIZE,
            SIZE * 4,
            wayland_client::protocol::wl_shm::Format::Argb8888,
            &peer.qh(),
            (),
        )
    }

    /// A linux-dmabuf buffer (linear ARGB8888; a memfd stands in for the
    /// dmabuf, which only drawing would notice).
    fn dmabuf_buffer(peer: &Peer) -> WlBuffer {
        let file = tempfile::tempfile().unwrap();
        file.set_len((SIZE * SIZE * 4) as u64).unwrap();
        let dmabuf: ZwpLinuxDmabufV1 = peer.bind(3);
        let params = dmabuf.create_params(&peer.qh(), ());
        params.add(file.as_fd(), 0, 0, (SIZE * 4) as u32, 0, 0);
        params.create_immed(
            SIZE,
            SIZE,
            smithay::backend::allocator::Fourcc::Argb8888 as u32,
            zwp_linux_buffer_params_v1::Flags::empty(),
            &peer.qh(),
            (),
        )
    }

    #[test]
    fn a_second_syncobj_surface_is_surface_exists() {
        let (mut comp, mut manager) = compositor();
        let (mut peer, syncobj, surface) = syncobj_client(&mut comp, &mut manager);
        syncobj.get_surface(&surface, &peer.qh(), ());
        pump(&mut comp, &mut manager, &mut peer);
        assert_eq!(error(&peer), None);
        syncobj.get_surface(&surface, &peer.qh(), ());
        pump(&mut comp, &mut manager, &mut peer);
        assert_eq!(
            error(&peer),
            Some((
                "wp_linux_drm_syncobj_manager_v1".into(),
                wp_linux_drm_syncobj_manager_v1::Error::SurfaceExists as u32
            ))
        );
    }

    #[test]
    fn a_timeline_that_does_not_import_is_invalid_timeline() {
        let (mut comp, mut manager) = compositor();
        let (mut peer, syncobj, _surface) = syncobj_client(&mut comp, &mut manager);
        let not_a_syncobj = tempfile::tempfile().unwrap();
        syncobj.import_timeline(not_a_syncobj.as_fd(), &peer.qh(), ());
        pump(&mut comp, &mut manager, &mut peer);
        assert_eq!(
            error(&peer),
            Some((
                "wp_linux_drm_syncobj_manager_v1".into(),
                wp_linux_drm_syncobj_manager_v1::Error::InvalidTimeline as u32
            ))
        );
    }

    /// Mutter checks the buffer type before the points: shm under explicit
    /// sync is `unsupported_buffer` even with no points set.
    #[test]
    fn an_shm_buffer_under_explicit_sync_is_unsupported_buffer() {
        let (mut comp, mut manager) = compositor();
        let (mut peer, syncobj, surface) = syncobj_client(&mut comp, &mut manager);
        let _sync = syncobj.get_surface(&surface, &peer.qh(), ());
        surface.attach(Some(&shm_buffer(&peer)), 0, 0);
        surface.commit();
        pump(&mut comp, &mut manager, &mut peer);
        assert_eq!(
            error(&peer),
            Some((
                "wp_linux_drm_syncobj_surface_v1".into(),
                wp_linux_drm_syncobj_surface_v1::Error::UnsupportedBuffer as u32
            ))
        );
    }

    /// A dmabuf attached without an acquire point is `no_acquire_point`
    /// (the spec's "if and only if"; upstream Smithay let it through).
    #[test]
    fn a_dmabuf_without_points_is_no_acquire_point() {
        let (mut comp, mut manager) = compositor();
        let (mut peer, syncobj, surface) = syncobj_client(&mut comp, &mut manager);
        let _sync = syncobj.get_surface(&surface, &peer.qh(), ());
        surface.attach(Some(&dmabuf_buffer(&peer)), 0, 0);
        surface.commit();
        pump(&mut comp, &mut manager, &mut peer);
        assert_eq!(
            error(&peer),
            Some((
                "wp_linux_drm_syncobj_surface_v1".into(),
                wp_linux_drm_syncobj_surface_v1::Error::NoAcquirePoint as u32
            ))
        );
    }

    /// Commits without a buffer or points are fine, and so is shm once the
    /// syncobj surface is gone (Mutter forgets it with the object).
    #[test]
    fn bufferless_commits_and_shm_after_destroy_are_accepted() {
        let (mut comp, mut manager) = compositor();
        let (mut peer, syncobj, surface) = syncobj_client(&mut comp, &mut manager);
        let sync = syncobj.get_surface(&surface, &peer.qh(), ());
        surface.commit();
        pump(&mut comp, &mut manager, &mut peer);
        assert_eq!(error(&peer), None);
        sync.destroy();
        surface.attach(Some(&shm_buffer(&peer)), 0, 0);
        surface.commit();
        pump(&mut comp, &mut manager, &mut peer);
        assert_eq!(error(&peer), None);
        // A second syncobj surface is allowed once the first is destroyed.
        let _again = syncobj.get_surface(&surface, &peer.qh(), ());
        pump(&mut comp, &mut manager, &mut peer);
        assert_eq!(error(&peer), None);
    }
}
