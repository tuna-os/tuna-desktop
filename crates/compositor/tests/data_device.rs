//! `wl_data_device_manager` advertisement and dispatch: toolkit
//! clients (GTK, Chromium) refuse a display without this global, so a
//! socketpair client binds the manager, creates a data device and a
//! data source, and offers a mime type. A sync barrier afterwards
//! proves the connection survived (a protocol error would kill the
//! client and exhaust the pump budget).

use std::os::unix::net::UnixStream;

use tuna_compositor::TestCompositor;
use wayland_client::{
    protocol::{
        wl_callback::WlCallback, wl_data_device::WlDataDevice,
        wl_data_device_manager::WlDataDeviceManager, wl_data_source::WlDataSource,
        wl_registry::WlRegistry, wl_seat::WlSeat,
    },
    Connection, Dispatch, EventQueue, QueueHandle,
};

macro_rules! empty_dispatch {
    ($iface:ty) => {
        impl Dispatch<$iface, ()> for Client {
            fn event(
                _: &mut Self,
                _: &$iface,
                _: <$iface as wayland_client::Proxy>::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
            }
        }
    };
}

const PUMP_ROUNDS: usize = 200;

/// One protocol client: bound globals plus the observations the
/// tests assert on.
#[derive(Default)]
struct Client {
    seat: Option<WlSeat>,
    manager: Option<WlDataDeviceManager>,
    device: Option<WlDataDevice>,
    source: Option<WlDataSource>,
    synced: bool,
}

impl Dispatch<WlRegistry, ()> for Client {
    fn event(
        state: &mut Self,
        registry: &WlRegistry,
        event: <WlRegistry as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wayland_client::protocol::wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_seat" => {
                    state.seat = Some(registry.bind::<WlSeat, _, _>(name, version, qh, ()));
                }
                "wl_data_device_manager" => {
                    state.manager = Some(registry.bind::<WlDataDeviceManager, _, _>(
                        name,
                        version.min(3),
                        qh,
                        (),
                    ));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<WlCallback, ()> for Client {
    fn event(
        state: &mut Self,
        _: &WlCallback,
        event: <WlCallback as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wayland_client::protocol::wl_callback::Event::Done { .. } = event {
            state.synced = true;
        }
    }
}

empty_dispatch!(WlSeat);
empty_dispatch!(WlDataDeviceManager);
empty_dispatch!(WlDataSource);

impl Dispatch<WlDataDevice, ()> for Client {
    fn event(
        _: &mut Self,
        _: &WlDataDevice,
        _event: <WlDataDevice as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // Devices only emit offers/selection on real input focus;
        // startup binds quietly.
    }
}

/// Pump the server and drain one client queue until `done` or the
/// round budget runs out.
fn pump(
    comp: &mut TestCompositor,
    queue: &mut EventQueue<Client>,
    client: &mut Client,
    done: impl Fn(&Client) -> bool,
) {
    for _ in 0..PUMP_ROUNDS {
        queue.flush().unwrap();
        comp.pump();
        if let Some(guard) = queue.prepare_read() {
            guard.read().unwrap();
        }
        queue.dispatch_pending(client).unwrap();
        if done(client) {
            return;
        }
    }
    panic!("pump budget exhausted");
}

/// The manager is advertised, a device and source dispatch cleanly,
/// and the connection survives a barrier afterwards.
#[test]
fn data_device_manager_binds_and_source_offers() {
    let mut comp = TestCompositor::new();
    let (server_stream, client_stream) = UnixStream::pair().unwrap();
    comp.add_client(server_stream);
    let conn = Connection::from_socket(client_stream).unwrap();
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    let mut client = Client::default();
    conn.display().get_registry(&qh, ());
    pump(&mut comp, &mut queue, &mut client, |c| {
        c.seat.is_some() && c.manager.is_some()
    });

    let device =
        client
            .manager
            .as_ref()
            .unwrap()
            .get_data_device(client.seat.as_ref().unwrap(), &qh, ());
    let source = client.manager.as_ref().unwrap().create_data_source(&qh, ());
    source.offer("text/plain".to_owned());
    client.device = Some(device);
    client.source = Some(source);

    client.synced = false;
    conn.display().sync(&qh, ());
    pump(&mut comp, &mut queue, &mut client, |c| c.synced);
    assert!(client.device.is_some());
    assert!(client.source.is_some());
}

// --- Copy/paste round-trip between two focused clients ---

use tuna_compositor::windows::{ManagerInput, WindowManager};
use wayland_client::protocol::{
    wl_compositor::WlCompositor,
    wl_data_offer::WlDataOffer,
    wl_display::WlDisplay,
    wl_keyboard::{Event as KeyEvent, WlKeyboard},
    wl_surface::WlSurface,
};
use wayland_protocols::wp::primary_selection::zv1::client::{
    zwp_primary_selection_device_manager_v1::ZwpPrimarySelectionDeviceManagerV1,
    zwp_primary_selection_device_v1::ZwpPrimarySelectionDeviceV1,
    zwp_primary_selection_offer_v1::ZwpPrimarySelectionOfferV1,
    zwp_primary_selection_source_v1::ZwpPrimarySelectionSourceV1,
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::XdgSurface, xdg_toplevel::XdgToplevel, xdg_wm_base::XdgWmBase,
};

/// Bytes the copy side serves; small enough to fit any pipe buffer so
/// the source can write without a concurrent reader.
const PASTE_BYTES: &[u8] = b"tuna-clipboard-round-trip";
/// Same for the primary selection side (distinct so a crossed wire
/// between the two selections fails loudly).
const PRIMARY_BYTES: &[u8] = b"tuna-primary-round-trip";
/// Evdev `a`: a bare key whose press hands the copy side an honest
/// serial for `set_selection`.
const KEY_A: u32 = 30;

/// One clipboard client: a mapped toplevel plus the data-device ends
/// and every observation the round-trip asserts on.
#[derive(Default)]
struct ClipClient {
    compositor: Option<WlCompositor>,
    seat: Option<WlSeat>,
    keyboard: Option<WlKeyboard>,
    pointer: Option<wayland_client::protocol::wl_pointer::WlPointer>,
    buttons: Vec<u32>,
    pointer_enters: usize,
    touch: Option<wayland_client::protocol::wl_touch::WlTouch>,
    touch_downs: Vec<u32>,
    cancelled: usize,
    drops: usize,
    drop_performed: usize,
    finished: usize,
    leaves: usize,
    drag_enter_serial: Option<u32>,
    drag_enters: Vec<(WlSurface, f64, f64, Option<WlDataOffer>)>,
    drag_motions: Vec<(f64, f64)>,
    drag_events: Vec<&'static str>,
    source_actions: Vec<wayland_client::protocol::wl_data_device_manager::DndAction>,
    offer_actions: Vec<wayland_client::protocol::wl_data_device_manager::DndAction>,
    chosen_actions: Vec<wayland_client::protocol::wl_data_device_manager::DndAction>,
    xdg_base: Option<XdgWmBase>,
    manager: Option<WlDataDeviceManager>,
    device: Option<WlDataDevice>,
    source: Option<WlDataSource>,
    primary_manager: Option<ZwpPrimarySelectionDeviceManagerV1>,
    primary_device: Option<ZwpPrimarySelectionDeviceV1>,
    primary_source: Option<ZwpPrimarySelectionSourceV1>,
    surface: Option<WlSurface>,
    synced: bool,
    configured: bool,
    /// (keycode, serial) in arrival order.
    keys: Vec<(u32, u32)>,
    /// Offers announced to this client, in arrival order.
    offers: Vec<WlDataOffer>,
    /// Mime types offered across all offers.
    offered_mimes: Vec<String>,
    /// Current selection offer, if the server sent one.
    selection: Option<WlDataOffer>,
    /// Mime types the peer requested from our source.
    sent_mimes: Vec<String>,
    /// Bytes our clipboard source serves; `PASTE_BYTES` when unset.
    payload: Option<Vec<u8>>,
    /// Primary offers announced to this client, in arrival order.
    primary_offers: Vec<ZwpPrimarySelectionOfferV1>,
    /// Mime types offered across all primary offers.
    primary_offered_mimes: Vec<String>,
    /// Current primary offer, if the server sent one.
    primary_selection: Option<ZwpPrimarySelectionOfferV1>,
    /// Mime types the peer requested from our primary source.
    primary_sent_mimes: Vec<String>,
}

impl ClipClient {
    fn ready(&self) -> bool {
        self.compositor.is_some()
            && self.seat.is_some()
            && self.keyboard.is_some()
            && self.pointer.is_some()
            && self.touch.is_some()
            && self.xdg_base.is_some()
            && self.manager.is_some()
            && self.primary_manager.is_some()
    }
}

macro_rules! clip_empty {
    ($iface:ty) => {
        impl Dispatch<$iface, ()> for ClipClient {
            fn event(
                _: &mut Self,
                _: &$iface,
                _: <$iface as wayland_client::Proxy>::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
            }
        }
    };
}

clip_empty!(WlDisplay);
clip_empty!(WlCompositor);
clip_empty!(WlSurface);
clip_empty!(WlSeat);
clip_empty!(WlDataDeviceManager);
clip_empty!(XdgToplevel);

impl Dispatch<WlRegistry, ()> for ClipClient {
    fn event(
        state: &mut Self,
        registry: &WlRegistry,
        event: <WlRegistry as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wayland_client::protocol::wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_compositor" => {
                    state.compositor =
                        Some(registry.bind::<WlCompositor, _, _>(name, version.min(6), qh, ()));
                }
                "wl_seat" => {
                    let seat = registry.bind::<WlSeat, _, _>(name, version, qh, ());
                    state.keyboard = Some(seat.get_keyboard(qh, ()));
                    state.pointer = Some(seat.get_pointer(qh, ()));
                    state.touch = Some(seat.get_touch(qh, ()));
                    state.seat = Some(seat);
                }
                "xdg_wm_base" => {
                    state.xdg_base =
                        Some(registry.bind::<XdgWmBase, _, _>(name, version.min(7), qh, ()));
                }
                "wl_data_device_manager" => {
                    state.manager = Some(registry.bind::<WlDataDeviceManager, _, _>(
                        name,
                        version.min(3),
                        qh,
                        (),
                    ));
                }
                "zwp_primary_selection_device_manager_v1" => {
                    state.primary_manager =
                        Some(registry.bind::<ZwpPrimarySelectionDeviceManagerV1, _, _>(
                            name,
                            version,
                            qh,
                            (),
                        ));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<WlCallback, ()> for ClipClient {
    fn event(
        state: &mut Self,
        _: &WlCallback,
        event: <WlCallback as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wayland_client::protocol::wl_callback::Event::Done { .. } = event {
            state.synced = true;
        }
    }
}

impl Dispatch<XdgWmBase, ()> for ClipClient {
    fn event(
        _: &mut Self,
        base: &XdgWmBase,
        event: <XdgWmBase as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wayland_protocols::xdg::shell::client::xdg_wm_base::Event::Ping { serial } = event {
            base.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, ()> for ClipClient {
    fn event(
        state: &mut Self,
        xdg_surface: &XdgSurface,
        event: <XdgSurface as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wayland_protocols::xdg::shell::client::xdg_surface::Event::Configure { serial } =
            event
        {
            xdg_surface.ack_configure(serial);
            state.configured = true;
        }
    }
}

impl Dispatch<WlKeyboard, ()> for ClipClient {
    fn event(
        state: &mut Self,
        _: &WlKeyboard,
        event: <WlKeyboard as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let KeyEvent::Key { key, serial, .. } = event {
            state.keys.push((key, serial));
        }
    }
}

impl Dispatch<WlDataDevice, ()> for ClipClient {
    fn event(
        state: &mut Self,
        _: &WlDataDevice,
        event: <WlDataDevice as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use wayland_client::protocol::wl_data_device::Event as DeviceEvent;
        match event {
            DeviceEvent::DataOffer { id } => {
                state.offers.push(id);
                state.drag_events.push("offer");
            }
            DeviceEvent::Selection { id } => state.selection = id,
            DeviceEvent::Drop => state.drops += 1,
            DeviceEvent::Leave => state.leaves += 1,
            DeviceEvent::Enter {
                serial,
                surface,
                x,
                y,
                id,
            } => {
                state.drag_enter_serial = Some(serial);
                state.drag_enters.push((surface, x, y, id));
                state.drag_events.push("enter");
            }
            DeviceEvent::Motion { x, y, .. } => {
                state.drag_motions.push((x, y));
                state.drag_events.push("motion");
            }
            _ => {}
        }
    }

    wayland_client::event_created_child!(ClipClient, WlDataDevice, [
        wayland_client::protocol::wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, ()),
    ]);
}

impl Dispatch<WlDataOffer, ()> for ClipClient {
    fn event(
        state: &mut Self,
        _: &WlDataOffer,
        event: <WlDataOffer as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use wayland_client::protocol::wl_data_offer::Event as OfferEvent;
        match event {
            OfferEvent::Offer { mime_type } => state.offered_mimes.push(mime_type),
            OfferEvent::SourceActions { source_actions } => {
                state
                    .source_actions
                    .push(source_actions.into_result().unwrap());
            }
            OfferEvent::Action { dnd_action } => {
                state.offer_actions.push(dnd_action.into_result().unwrap());
            }
            _ => {}
        }
    }
}

impl Dispatch<WlDataSource, ()> for ClipClient {
    fn event(
        state: &mut Self,
        _: &WlDataSource,
        event: <WlDataSource as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use std::io::Write;
        use wayland_client::protocol::wl_data_source::Event as SourceEvent;
        match &event {
            SourceEvent::Cancelled => state.cancelled += 1,
            SourceEvent::DndDropPerformed => state.drop_performed += 1,
            SourceEvent::DndFinished => state.finished += 1,
            SourceEvent::Action { dnd_action } => {
                state
                    .chosen_actions
                    .push((*dnd_action).into_result().unwrap());
            }
            _ => {}
        }
        if let SourceEvent::Send { mime_type, fd } = event {
            state.sent_mimes.push(mime_type);
            // Small payload: one write, then drop the fd so the
            // reader sees EOF.
            let mut file = std::fs::File::from(fd);
            file.write_all(state.payload.as_deref().unwrap_or(PASTE_BYTES))
                .unwrap();
        }
    }
}

impl Dispatch<ZwpPrimarySelectionDeviceManagerV1, ()> for ClipClient {
    fn event(
        _: &mut Self,
        _: &ZwpPrimarySelectionDeviceManagerV1,
        _: <ZwpPrimarySelectionDeviceManagerV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpPrimarySelectionDeviceV1, ()> for ClipClient {
    fn event(
        state: &mut Self,
        _: &ZwpPrimarySelectionDeviceV1,
        event: <ZwpPrimarySelectionDeviceV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use wayland_protocols::wp::primary_selection::zv1::client::zwp_primary_selection_device_v1::Event as PrimaryDeviceEvent;
        match event {
            PrimaryDeviceEvent::DataOffer { offer } => state.primary_offers.push(offer),
            PrimaryDeviceEvent::Selection { id } => state.primary_selection = id,
            _ => {}
        }
    }

    wayland_client::event_created_child!(ClipClient, ZwpPrimarySelectionDeviceV1, [
        wayland_protocols::wp::primary_selection::zv1::client::zwp_primary_selection_device_v1::EVT_DATA_OFFER_OPCODE => (ZwpPrimarySelectionOfferV1, ()),
    ]);
}

impl Dispatch<ZwpPrimarySelectionOfferV1, ()> for ClipClient {
    fn event(
        state: &mut Self,
        _: &ZwpPrimarySelectionOfferV1,
        event: <ZwpPrimarySelectionOfferV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use wayland_protocols::wp::primary_selection::zv1::client::zwp_primary_selection_offer_v1::Event as PrimaryOfferEvent;
        if let PrimaryOfferEvent::Offer { mime_type } = event {
            state.primary_offered_mimes.push(mime_type);
        }
    }
}

impl Dispatch<ZwpPrimarySelectionSourceV1, ()> for ClipClient {
    fn event(
        state: &mut Self,
        _: &ZwpPrimarySelectionSourceV1,
        event: <ZwpPrimarySelectionSourceV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use std::io::Write;
        use wayland_protocols::wp::primary_selection::zv1::client::zwp_primary_selection_source_v1::Event as PrimarySourceEvent;
        if let PrimarySourceEvent::Send { mime_type, fd } = event {
            state.primary_sent_mimes.push(mime_type);
            let mut file = std::fs::File::from(fd);
            file.write_all(PRIMARY_BYTES).unwrap();
        }
    }
}

/// Pump the server and drain one clipboard client's queue until
/// `done` or the round budget runs out.
fn pump_one(
    comp: &mut TestCompositor,
    queue: &mut EventQueue<ClipClient>,
    client: &mut ClipClient,
    done: impl Fn(&ClipClient) -> bool,
) {
    for _ in 0..PUMP_ROUNDS {
        queue.flush().unwrap();
        comp.pump();
        if let Some(guard) = queue.prepare_read() {
            guard.read().unwrap();
        }
        queue.dispatch_pending(client).unwrap();
        if done(client) {
            return;
        }
    }
    panic!("pump budget exhausted");
}

/// Pump the server and drain both client queues until `done` or the
/// round budget runs out.
fn pump_both(
    comp: &mut TestCompositor,
    queue_a: &mut EventQueue<ClipClient>,
    client_a: &mut ClipClient,
    queue_b: &mut EventQueue<ClipClient>,
    client_b: &mut ClipClient,
    done: impl Fn(&ClipClient, &ClipClient) -> bool,
) {
    for _ in 0..PUMP_ROUNDS {
        queue_a.flush().unwrap();
        queue_b.flush().unwrap();
        comp.pump();
        if let Some(guard) = queue_a.prepare_read() {
            guard.read().unwrap();
        }
        if let Some(guard) = queue_b.prepare_read() {
            guard.read().unwrap();
        }
        queue_a.dispatch_pending(client_a).unwrap();
        queue_b.dispatch_pending(client_b).unwrap();
        if done(client_a, client_b) {
            return;
        }
    }
    panic!("pump budget exhausted");
}

/// Connect one clipboard client and bind its globals.
fn clip_connect(comp: &mut TestCompositor) -> (Connection, EventQueue<ClipClient>, ClipClient) {
    let (server_stream, client_stream) = UnixStream::pair().unwrap();
    comp.add_client(server_stream);
    let conn = Connection::from_socket(client_stream).unwrap();
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    let mut client = ClipClient::default();
    conn.display().get_registry(&qh, ());
    pump_one(comp, &mut queue, &mut client, |c| c.ready());
    (conn, queue, client)
}

/// Map one titled toplevel with both selection devices attached,
/// then drain behind a sync barrier.
fn map_selection_client(
    comp: &mut TestCompositor,
    conn: &Connection,
    queue: &mut EventQueue<ClipClient>,
    client: &mut ClipClient,
    title: &str,
) {
    let qh = queue.handle();
    let surface = client.compositor.as_ref().unwrap().create_surface(&qh, ());
    let xdg_surface = client
        .xdg_base
        .as_ref()
        .unwrap()
        .get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg_surface.get_toplevel(&qh, ());
    toplevel.set_title(title.to_owned());
    surface.commit();
    client.surface = Some(surface);
    let seat = client.seat.as_ref().unwrap();
    client.device = Some(
        client
            .manager
            .as_ref()
            .unwrap()
            .get_data_device(seat, &qh, ()),
    );
    client.primary_device = Some(client.primary_manager.as_ref().unwrap().get_device(
        seat,
        &qh,
        (),
    ));
    client.synced = false;
    conn.display().sync(&qh, ());
    pump_one(comp, queue, client, |c| c.synced);
}

/// Drain the configure round-trip behind a barrier per client;
/// mapping is only real once both sides acked.
#[allow(clippy::too_many_arguments)]
fn sync_mapped(
    comp: &mut TestCompositor,
    conn_a: &Connection,
    queue_a: &mut EventQueue<ClipClient>,
    client_a: &mut ClipClient,
    conn_b: &Connection,
    queue_b: &mut EventQueue<ClipClient>,
    client_b: &mut ClipClient,
) {
    for (conn, queue, client) in [(conn_a, queue_a, client_a), (conn_b, queue_b, client_b)] {
        let qh = queue.handle();
        client.synced = false;
        conn.display().sync(&qh, ());
        pump_one(comp, queue, client, |c| c.synced);
        assert!(client.configured, "toplevel never configured");
        assert!(client.surface.is_some(), "surface dropped");
    }
}

/// Copy in the focused client pastes in the next-focused one: the
/// offer, mime, and bytes all cross through the compositor.
#[test]
fn clipboard_copy_paste_round_trips_between_focused_clients() {
    use std::io::Read;

    let mut comp = TestCompositor::new();
    // Manager first so seat capabilities exist before the clients'
    // `get_keyboard` requests arrive (mirrors `Runtime::launch`).
    let mut manager: WindowManager = comp.window_manager();

    let (conn_a, mut queue_a, mut client_a) = clip_connect(&mut comp);
    let (conn_b, mut queue_b, mut client_b) = clip_connect(&mut comp);

    map_selection_client(&mut comp, &conn_a, &mut queue_a, &mut client_a, "copy");
    map_selection_client(&mut comp, &conn_b, &mut queue_b, &mut client_b, "paste");
    manager.reconcile(&mut comp.state);
    sync_mapped(
        &mut comp,
        &conn_a,
        &mut queue_a,
        &mut client_a,
        &conn_b,
        &mut queue_b,
        &mut client_b,
    );
    let id_a = manager
        .model()
        .windows()
        .find(|w| w.title == "copy")
        .unwrap()
        .id;
    let id_b = manager
        .model()
        .windows()
        .find(|w| w.title == "paste")
        .unwrap()
        .id;

    // Focus the copy side and press a key so `set_selection` carries
    // an honest serial from a real key event.
    assert!(manager.focus(&mut comp.state, Some(id_a)));
    manager.on_input(
        &mut comp.state,
        ManagerInput::Key {
            keycode: KEY_A,
            pressed: true,
            time: 5000,
        },
    );
    pump_both(
        &mut comp,
        &mut queue_a,
        &mut client_a,
        &mut queue_b,
        &mut client_b,
        |a, _| !a.keys.is_empty(),
    );
    let serial = client_a.keys[0].1;

    // Copy: offer text and take the selection while focused.
    let qh_a = queue_a.handle();
    let source = client_a
        .manager
        .as_ref()
        .unwrap()
        .create_data_source(&qh_a, ());
    source.offer("text/plain".to_owned());
    client_a
        .device
        .as_ref()
        .unwrap()
        .set_selection(Some(&source), serial);
    client_a.source = Some(source);

    // Paste side gets nothing before it holds focus.
    manager.on_input(
        &mut comp.state,
        ManagerInput::Key {
            keycode: KEY_A,
            pressed: false,
            time: 5001,
        },
    );
    pump_both(
        &mut comp,
        &mut queue_a,
        &mut client_a,
        &mut queue_b,
        &mut client_b,
        |_, _| true,
    );
    assert!(
        client_b.selection.is_none(),
        "unfocused client must see no selection"
    );

    // Focusing the paste side delivers the offer plus selection.
    assert!(manager.focus(&mut comp.state, Some(id_b)));
    pump_both(
        &mut comp,
        &mut queue_a,
        &mut client_a,
        &mut queue_b,
        &mut client_b,
        |_, b| b.selection.is_some(),
    );
    assert_eq!(
        client_b.offers.len(),
        1,
        "one offer per selection, got {}",
        client_b.offers.len()
    );
    assert!(
        client_b.offered_mimes.contains(&"text/plain".to_owned()),
        "offered mimes: {:?}",
        client_b.offered_mimes
    );

    // Paste: receive into a socketpair; the copy side's `send` writes
    // the bytes, dropping its fd marks EOF for the reader below.
    use std::os::fd::AsFd;
    let (read_end, write_end) = UnixStream::pair().unwrap();
    client_b
        .selection
        .as_ref()
        .unwrap()
        .receive("text/plain".to_owned(), write_end.as_fd());
    pump_both(
        &mut comp,
        &mut queue_a,
        &mut client_a,
        &mut queue_b,
        &mut client_b,
        |a, _| a.sent_mimes.len() == 2,
    );
    // The first send was the clipboard manager's copy, requested when
    // the selection was set (mutter's MetaClipboardManager); the second
    // is the paste.
    assert_eq!(
        client_a.sent_mimes,
        vec!["text/plain".to_owned(), "text/plain".to_owned()]
    );
    // The request fd went over SCM at flush time; dropping our copy
    // leaves the source's write as the last writer, so EOF follows it.
    drop(write_end);
    assert!(
        client_a.source.is_some(),
        "source must stay alive until the peer receives"
    );
    let mut pasted = Vec::new();
    std::io::BufReader::new(read_end)
        .read_to_end(&mut pasted)
        .unwrap();
    assert_eq!(pasted, PASTE_BYTES);
}

/// Middle-click select in the focused client pastes in the
/// next-focused one, on the primary selection beside the clipboard.
#[test]
fn primary_selection_round_trips_between_focused_clients() {
    use std::io::Read;
    use std::os::fd::AsFd;

    let mut comp = TestCompositor::new();
    let mut manager: WindowManager = comp.window_manager();

    let (conn_a, mut queue_a, mut client_a) = clip_connect(&mut comp);
    let (conn_b, mut queue_b, mut client_b) = clip_connect(&mut comp);

    map_selection_client(&mut comp, &conn_a, &mut queue_a, &mut client_a, "select");
    map_selection_client(
        &mut comp,
        &conn_b,
        &mut queue_b,
        &mut client_b,
        "middle-paste",
    );
    manager.reconcile(&mut comp.state);
    sync_mapped(
        &mut comp,
        &conn_a,
        &mut queue_a,
        &mut client_a,
        &conn_b,
        &mut queue_b,
        &mut client_b,
    );
    let id_a = manager
        .model()
        .windows()
        .find(|w| w.title == "select")
        .unwrap()
        .id;
    let id_b = manager
        .model()
        .windows()
        .find(|w| w.title == "middle-paste")
        .unwrap()
        .id;

    assert!(manager.focus(&mut comp.state, Some(id_a)));
    manager.on_input(
        &mut comp.state,
        ManagerInput::Key {
            keycode: KEY_A,
            pressed: true,
            time: 5000,
        },
    );
    pump_both(
        &mut comp,
        &mut queue_a,
        &mut client_a,
        &mut queue_b,
        &mut client_b,
        |a, _| !a.keys.is_empty(),
    );
    let serial = client_a.keys[0].1;

    // Select: offer text and take the primary selection while focused.
    let qh_a = queue_a.handle();
    let source = client_a
        .primary_manager
        .as_ref()
        .unwrap()
        .create_source(&qh_a, ());
    source.offer("text/plain".to_owned());
    client_a
        .primary_device
        .as_ref()
        .unwrap()
        .set_selection(Some(&source), serial);
    client_a.primary_source = Some(source);

    manager.on_input(
        &mut comp.state,
        ManagerInput::Key {
            keycode: KEY_A,
            pressed: false,
            time: 5001,
        },
    );
    pump_both(
        &mut comp,
        &mut queue_a,
        &mut client_a,
        &mut queue_b,
        &mut client_b,
        |_, _| true,
    );
    assert!(
        client_b.primary_selection.is_none(),
        "unfocused client must see no primary selection"
    );

    assert!(manager.focus(&mut comp.state, Some(id_b)));
    pump_both(
        &mut comp,
        &mut queue_a,
        &mut client_a,
        &mut queue_b,
        &mut client_b,
        |_, b| b.primary_selection.is_some(),
    );
    assert_eq!(
        client_b.primary_offers.len(),
        1,
        "one primary offer per selection, got {}",
        client_b.primary_offers.len()
    );
    assert!(
        client_b
            .primary_offered_mimes
            .contains(&"text/plain".to_owned()),
        "offered mimes: {:?}",
        client_b.primary_offered_mimes
    );

    let (read_end, write_end) = UnixStream::pair().unwrap();
    client_b
        .primary_selection
        .as_ref()
        .unwrap()
        .receive("text/plain".to_owned(), write_end.as_fd());
    pump_both(
        &mut comp,
        &mut queue_a,
        &mut client_a,
        &mut queue_b,
        &mut client_b,
        |a, _| !a.primary_sent_mimes.is_empty(),
    );
    assert_eq!(client_a.primary_sent_mimes, vec!["text/plain".to_owned()]);
    drop(write_end);
    assert!(
        client_a.primary_source.is_some(),
        "primary source must stay alive until the peer receives"
    );
    let mut pasted = Vec::new();
    std::io::BufReader::new(read_end)
        .read_to_end(&mut pasted)
        .unwrap();
    assert_eq!(pasted, PRIMARY_BYTES);
}

impl Dispatch<wayland_client::protocol::wl_pointer::WlPointer, ()> for ClipClient {
    fn event(
        state: &mut Self,
        _: &wayland_client::protocol::wl_pointer::WlPointer,
        event: wayland_client::protocol::wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wayland_client::protocol::wl_pointer::Event::Enter { .. } => {
                state.pointer_enters += 1;
            }
            wayland_client::protocol::wl_pointer::Event::Button {
                serial,
                state:
                    wayland_client::WEnum::Value(
                        wayland_client::protocol::wl_pointer::ButtonState::Pressed,
                    ),
                ..
            } => state.buttons.push(serial),
            _ => {}
        }
    }
}

fn native_drag_case(
    cancel: bool,
    icon_present: bool,
    destroy_icon: bool,
    destroy_source: bool,
    touch_drag: bool,
    source_less: bool,
) {
    use wayland_client::protocol::wl_data_device_manager::DndAction;
    let mut comp = TestCompositor::new();
    let mut manager = comp.window_manager();
    let touch = touch_drag.then(|| comp.test_seat().add_touch());
    let (conn, mut queue, mut client) = clip_connect(&mut comp);
    map_selection_client(&mut comp, &conn, &mut queue, &mut client, "drag");
    manager.reconcile(&mut comp.state);
    client.synced = false;
    conn.display().sync(&queue.handle(), ());
    pump_one(&mut comp, &mut queue, &mut client, |c| c.synced);
    manager.reconcile(&mut comp.state);
    let rect = manager.visible_windows()[0].1;
    let serial = if let Some(touch) = &touch {
        let target = comp.state.first_toplevel_surface();
        touch.down(
            &mut comp.state,
            Some((target, rect.loc.to_f64())),
            &smithay::input::touch::DownEvent {
                slot: Some(0).into(),
                location: (f64::from(rect.loc.x + 20), f64::from(rect.loc.y + 20)).into(),
                serial: smithay::utils::SERIAL_COUNTER.next_serial(),
                time: 11,
            },
        );
        touch.frame(&mut comp.state);
        client.synced = false;
        conn.display().sync(&queue.handle(), ());
        pump_one(&mut comp, &mut queue, &mut client, |c| c.synced);
        assert!(!client.touch_downs.is_empty(), "real touch serial required");
        client.touch_downs[0]
    } else {
        manager.on_input(
            &mut comp.state,
            ManagerInput::Motion {
                pos: (f64::from(rect.loc.x + 20), f64::from(rect.loc.y + 20)).into(),
                time: 10,
            },
        );
        manager.on_input(
            &mut comp.state,
            ManagerInput::Button {
                button: 0x110,
                pressed: true,
                time: 11,
            },
        );
        client.synced = false;
        conn.display().sync(&queue.handle(), ());
        pump_one(&mut comp, &mut queue, &mut client, |c| c.synced);
        assert!(!client.buttons.is_empty(), "real pointer serial required");
        client.buttons[0]
    };
    assert!(
        !comp.client_dnd_active(),
        "implicit pointer/touch grab is not client DnD"
    );
    assert!(
        !comp.cancel_client_dnd(11),
        "cancellation preserves a non-DnD implicit grab"
    );
    let qh = queue.handle();
    let source = client.manager.as_ref().unwrap().create_data_source(&qh, ());
    source.offer("text/plain".into());
    source.set_actions(DndAction::Move);
    let icon = icon_present.then(|| client.compositor.as_ref().unwrap().create_surface(&qh, ()));
    client.device.as_ref().unwrap().start_drag(
        if source_less { None } else { Some(&source) },
        client.surface.as_ref().unwrap(),
        icon.as_ref(),
        serial,
    );
    client.source = Some(source.clone());
    client.synced = false;
    conn.display().sync(&qh, ());
    pump_one(&mut comp, &mut queue, &mut client, |c| c.synced);
    assert!(
        comp.client_dnd_active(),
        "native StartDrag must be accepted"
    );
    if destroy_icon {
        icon.as_ref().unwrap().destroy();
        client.synced = false;
        conn.display().sync(&qh, ());
        pump_one(&mut comp, &mut queue, &mut client, |c| c.synced);
        assert!(
            comp.client_dnd_active(),
            "icon lifetime does not own the drag"
        );
    }
    if let Some(touch) = &touch {
        let target = comp.state.first_toplevel_surface();
        touch.motion(
            &mut comp.state,
            Some((target, rect.loc.to_f64())),
            &smithay::input::touch::MotionEvent {
                slot: Some(0).into(),
                location: (f64::from(rect.loc.x + 40), f64::from(rect.loc.y + 40)).into(),
                time: 12,
            },
        );
        touch.frame(&mut comp.state);
    } else {
        manager.on_input(
            &mut comp.state,
            ManagerInput::Motion {
                pos: (f64::from(rect.loc.x + 40), f64::from(rect.loc.y + 40)).into(),
                time: 12,
            },
        );
    }
    // A source-less same-client drag has no offer but still owns the grab.
    let offer = if source_less {
        client.synced = false;
        conn.display().sync(&qh, ());
        pump_one(&mut comp, &mut queue, &mut client, |c| c.synced);
        assert!(client.offers.is_empty());
        None
    } else {
        client.synced = false;
        conn.display().sync(&qh, ());
        pump_one(&mut comp, &mut queue, &mut client, |c| c.synced);
        assert!(
            !client.offers.is_empty(),
            "real drag offer required after motion"
        );
        let offer = client.offers.last().unwrap().clone();
        offer.accept(
            client.drag_enter_serial.expect("real target Enter serial"),
            Some("text/plain".into()),
        );
        offer.set_actions(DndAction::Move, DndAction::Move);
        client.synced = false;
        conn.display().sync(&qh, ());
        pump_one(&mut comp, &mut queue, &mut client, |c| c.synced);
        Some(offer)
    };
    if destroy_source {
        source.destroy();
        client.synced = false;
        conn.display().sync(&qh, ());
        pump_one(&mut comp, &mut queue, &mut client, |c| c.synced);
    }
    if cancel {
        assert!(comp.cancel_client_dnd(13));
        assert!(!comp.client_dnd_active());
        assert!(!comp.cancel_client_dnd(14));
        if let Some(offer) = &offer {
            // Requests already queued by the target cannot revive a cancelled offer.
            offer.accept(
                client.drag_enter_serial.expect("real target Enter serial"),
                Some("text/plain".into()),
            );
            offer.set_actions(DndAction::Move, DndAction::Move);
            let (_reader, writer) = UnixStream::pair().unwrap();
            use std::os::fd::AsFd;
            offer.receive("text/plain".into(), writer.as_fd());
            client.synced = false;
            conn.display().sync(&qh, ());
            pump_one(&mut comp, &mut queue, &mut client, |c| c.synced);
            assert!(
                client.sent_mimes.is_empty(),
                "cancelled offer must refuse Receive before release"
            );
        }
    }
    if let Some(touch) = &touch {
        touch.up(
            &mut comp.state,
            &smithay::input::touch::UpEvent {
                slot: Some(0).into(),
                serial: smithay::utils::SERIAL_COUNTER.next_serial(),
                time: 15,
            },
        );
        touch.frame(&mut comp.state);
    } else {
        manager.on_input(
            &mut comp.state,
            ManagerInput::Button {
                button: 0x110,
                pressed: false,
                time: 15,
            },
        );
    }
    client.synced = false;
    conn.display().sync(&qh, ());
    pump_one(&mut comp, &mut queue, &mut client, |c| c.synced);
    assert!(!comp.client_dnd_active());
    assert_eq!(
        client.drops,
        usize::from(!cancel && !destroy_source && !source_less)
    );
    assert_eq!(
        client.drop_performed,
        usize::from(!cancel && !destroy_source && !source_less)
    );
    assert_eq!(
        client.cancelled,
        usize::from(cancel && !destroy_source && !source_less)
    );
    assert!(client.leaves > 0);
    if !cancel && !destroy_source && !source_less {
        // Actual owned bytes must cross the successful dropped offer BEFORE Finish.
        use std::io::Read;
        use std::os::fd::AsFd;
        let (reader, writer) = UnixStream::pair().unwrap();
        reader
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        offer
            .as_ref()
            .unwrap()
            .receive("text/plain".into(), writer.as_fd());
        client.synced = false;
        conn.display().sync(&qh, ());
        pump_one(&mut comp, &mut queue, &mut client, |c| c.synced);
        assert_eq!(
            client.sent_mimes,
            vec!["text/plain".to_owned()],
            "real Send event required before reading"
        );
        drop(writer);
        let mut received = Vec::new();
        std::io::BufReader::new(reader)
            .read_to_end(&mut received)
            .unwrap();
        assert_eq!(
            received, PASTE_BYTES,
            "normal dropped offer delivers the owned payload"
        );
        assert_eq!(client.sent_mimes, vec!["text/plain".to_owned()]);
        offer.as_ref().unwrap().finish();
        client.synced = false;
        conn.display().sync(&qh, ());
        pump_one(&mut comp, &mut queue, &mut client, |c| c.synced);
        assert_eq!(
            client.finished, 1,
            "successful offer remains valid for Finish"
        );
        assert!(!comp.cancel_client_dnd(16));
        assert_eq!(client.cancelled, 0);
    }
    if let Some(offer) = offer {
        offer.destroy();
    }
    client.synced = false;
    conn.display().sync(&qh, ());
    pump_one(&mut comp, &mut queue, &mut client, |c| c.synced);
    assert_eq!(
        client.cancelled,
        usize::from(cancel && !destroy_source && !source_less),
        "destroying the retired offer cannot double-cancel"
    );
}

#[test]
fn native_drag_cancel_without_icon_is_terminal_and_idempotent() {
    native_drag_case(true, false, false, false, false, false);
}
#[test]
fn native_drag_cancel_with_destroyed_icon_keeps_real_ownership() {
    native_drag_case(true, true, true, false, false, false);
}
#[test]
fn native_drag_dead_source_cannot_commit() {
    native_drag_case(false, false, false, true, false, false);
}
#[test]
fn native_drag_normal_drop_offer_can_finish() {
    native_drag_case(false, true, false, false, false, false);
}

impl Dispatch<wayland_client::protocol::wl_touch::WlTouch, ()> for ClipClient {
    fn event(
        state: &mut Self,
        _: &wayland_client::protocol::wl_touch::WlTouch,
        event: wayland_client::protocol::wl_touch::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wayland_client::protocol::wl_touch::Event::Down { serial, .. } = event {
            state.touch_downs.push(serial);
        }
    }
}

#[test]
fn native_touch_drag_cancel_without_icon_is_terminal() {
    native_drag_case(true, false, false, false, true, false);
}
#[test]
fn native_touch_drag_normal_drop_offer_can_finish() {
    native_drag_case(false, false, false, false, true, false);
}

#[test]
fn native_touch_drag_cancel_with_destroyed_icon_keeps_ownership() {
    native_drag_case(true, true, true, false, true, false);
}
#[test]
fn native_drag_cancel_after_source_destroy_never_double_notifies() {
    native_drag_case(true, false, false, true, false, false);
}

#[test]
fn native_source_less_same_client_drag_can_cancel_without_icon() {
    native_drag_case(true, false, false, false, false, true);
}
#[test]
fn native_touch_source_less_same_client_drag_can_cancel_without_icon() {
    native_drag_case(true, false, false, false, true, true);
}

/// StartDrag arrives after the press (and possibly after motion under its
/// implicit grab). Only protocol barriers follow: no input can manufacture the
/// target's initial Enter/Motion, offer, or successful drop.
fn stationary_native_drag_case(same_surface: bool, source_less: bool) {
    use wayland_client::protocol::wl_data_device_manager::DndAction;

    let mut comp = TestCompositor::new();
    let mut manager = comp.window_manager();
    let (conn_a, mut queue_a, mut client_a) = clip_connect(&mut comp);
    let (conn_b, mut queue_b, mut client_b) = clip_connect(&mut comp);
    map_selection_client(
        &mut comp,
        &conn_a,
        &mut queue_a,
        &mut client_a,
        "drag-origin",
    );
    map_selection_client(
        &mut comp,
        &conn_b,
        &mut queue_b,
        &mut client_b,
        "drag-target",
    );
    manager.reconcile(&mut comp.state);
    sync_mapped(
        &mut comp,
        &conn_a,
        &mut queue_a,
        &mut client_a,
        &conn_b,
        &mut queue_b,
        &mut client_b,
    );
    manager.reconcile(&mut comp.state);
    let origin_id = manager
        .model()
        .windows()
        .find(|w| w.title == "drag-origin")
        .unwrap()
        .id;
    let target_id = manager
        .model()
        .windows()
        .find(|w| w.title == "drag-target")
        .unwrap()
        .id;
    // Keep both hit-test regions exposed, independent of default placement.
    for (id, x) in [(origin_id, 50), (target_id, 400)] {
        assert!(manager.resize_window(id, 240, 180));
        let rect = manager.geometry(id).unwrap();
        assert!(manager.move_window(id, x - rect.loc.x, 80 - rect.loc.y));
    }
    let origin_rect = manager.geometry(origin_id).unwrap();
    let target_rect = if same_surface {
        origin_rect
    } else {
        manager.geometry(target_id).unwrap()
    };
    manager.on_input(
        &mut comp.state,
        ManagerInput::Motion {
            pos: (
                f64::from(origin_rect.loc.x + 20),
                f64::from(origin_rect.loc.y + 20),
            )
                .into(),
            time: 20,
        },
    );
    manager.on_input(
        &mut comp.state,
        ManagerInput::Button {
            button: 0x110,
            pressed: true,
            time: 21,
        },
    );
    sync_mapped(
        &mut comp,
        &conn_a,
        &mut queue_a,
        &mut client_a,
        &conn_b,
        &mut queue_b,
        &mut client_b,
    );
    let serial = *client_a
        .buttons
        .last()
        .expect("origin received the real press");
    assert_eq!(client_a.pointer_enters, 1);
    assert_eq!(client_b.pointer_enters, 0);

    // The implicit grab must retain pointer focus on A, while the compositor
    // hit-tests B. Even the same-surface case uses a position unlike the press.
    manager.on_input(
        &mut comp.state,
        ManagerInput::Motion {
            pos: (
                f64::from(target_rect.loc.x + 40),
                f64::from(target_rect.loc.y + 50),
            )
                .into(),
            time: 22,
        },
    );
    sync_mapped(
        &mut comp,
        &conn_a,
        &mut queue_a,
        &mut client_a,
        &conn_b,
        &mut queue_b,
        &mut client_b,
    );
    assert_eq!(
        client_a.pointer_enters, 1,
        "implicit focus stayed on origin"
    );
    assert_eq!(
        client_b.pointer_enters, 0,
        "implicit grab withheld target pointer focus"
    );

    let qh = queue_a.handle();
    let source = (!source_less).then(|| {
        let source = client_a
            .manager
            .as_ref()
            .unwrap()
            .create_data_source(&qh, ());
        source.offer("text/plain".into());
        source.set_actions(DndAction::Move);
        source
    });
    let device = client_a.device.as_ref().unwrap().clone();
    let origin = client_a.surface.as_ref().unwrap().clone();
    device.start_drag(source.as_ref(), &origin, None, serial.wrapping_add(1));
    sync_mapped(
        &mut comp,
        &conn_a,
        &mut queue_a,
        &mut client_a,
        &conn_b,
        &mut queue_b,
        &mut client_b,
    );
    assert!(!comp.client_dnd_active(), "invalid serial cannot start DnD");
    assert!(client_a.drag_enters.is_empty() && client_b.drag_enters.is_empty());
    assert!(client_a.offers.is_empty() && client_b.offers.is_empty());

    device.start_drag(source.as_ref(), &origin, None, serial);
    client_a.source = source;
    sync_mapped(
        &mut comp,
        &conn_a,
        &mut queue_a,
        &mut client_a,
        &conn_b,
        &mut queue_b,
        &mut client_b,
    );
    assert!(
        comp.client_dnd_active(),
        "valid implicit-grab serial starts DnD"
    );
    let offer = {
        let (target, other) = if same_surface {
            (&client_a, &client_b)
        } else {
            (&client_b, &client_a)
        };
        assert!(
            other.drag_enters.is_empty(),
            "no stale-origin or wrong-client Enter"
        );
        assert!(other.drag_motions.is_empty(), "no wrong-client Motion");
        assert!(other.offers.is_empty(), "no wrong-client offer");
        if source_less && !same_surface {
            assert!(
                target.drag_enters.is_empty(),
                "source-less drag is client-local"
            );
            assert!(target.drag_motions.is_empty());
            assert!(
                target.offers.is_empty(),
                "source-less drag cannot leak an offer"
            );
            None
        } else {
            assert_eq!(
                target.drag_enters.len(),
                1,
                "Enter before any subsequent input"
            );
            let (surface, x, y, offer) = &target.drag_enters[0];
            assert_eq!(surface, target.surface.as_ref().unwrap());
            assert_eq!(
                (*x, *y),
                (40.0, 50.0),
                "current surface-local position, not press"
            );
            assert_eq!(target.drag_motions, vec![(40.0, 50.0)]);
            if source_less {
                assert!(offer.is_none());
                assert!(target.offers.is_empty());
                assert_eq!(target.drag_events, vec!["enter", "motion"]);
                None
            } else {
                assert_eq!(target.offers.len(), 1);
                assert_eq!(offer.as_ref(), target.offers.first());
                assert_eq!(target.offered_mimes, vec!["text/plain"]);
                assert_eq!(target.source_actions, vec![DndAction::Move]);
                assert_eq!(target.drag_events, vec!["offer", "enter", "motion"]);
                let offer = offer.as_ref().unwrap().clone();
                offer.accept(target.drag_enter_serial.unwrap(), Some("text/plain".into()));
                offer.set_actions(DndAction::Move, DndAction::Move);
                Some(offer)
            }
        }
    };
    if offer.is_some() {
        // Flush the recipient's negotiation before the source's barrier:
        // cross-client Action is queued only when B's requests are handled.
        sync_mapped(
            &mut comp,
            &conn_b,
            &mut queue_b,
            &mut client_b,
            &conn_a,
            &mut queue_a,
            &mut client_a,
        );
        let target = if same_surface { &client_a } else { &client_b };
        assert_eq!(target.offer_actions, vec![DndAction::Move]);
        assert_eq!(client_a.chosen_actions, vec![DndAction::Move]);
    } else {
        assert!(comp.cancel_client_dnd(23));
        assert!(!comp.cancel_client_dnd(24), "cancellation stays idempotent");
    }
    manager.on_input(
        &mut comp.state,
        ManagerInput::Button {
            button: 0x110,
            pressed: false,
            time: 25,
        },
    );
    sync_mapped(
        &mut comp,
        &conn_a,
        &mut queue_a,
        &mut client_a,
        &conn_b,
        &mut queue_b,
        &mut client_b,
    );
    assert!(!comp.client_dnd_active());
    let target = if same_surface { &client_a } else { &client_b };
    assert_eq!(target.drops, usize::from(offer.is_some()));
    assert_eq!(client_a.drop_performed, usize::from(offer.is_some()));
    if source_less && !same_surface {
        assert_eq!(
            target.leaves, 0,
            "foreign client never entered, so receives no Leave"
        );
    } else {
        assert_eq!(
            target.leaves, 1,
            "initial target leaves once on termination"
        );
    }
    if let Some(offer) = offer {
        offer.finish();
        // Finish is a recipient request; drain B before observing A's reply.
        sync_mapped(
            &mut comp,
            &conn_b,
            &mut queue_b,
            &mut client_b,
            &conn_a,
            &mut queue_a,
            &mut client_a,
        );
        assert_eq!(
            client_a.finished, 1,
            "initial offer remains valid through Finish"
        );
        assert_eq!(client_a.cancelled, 0);
    }
}

#[test]
fn native_drag_enters_current_target_at_start_without_further_motion() {
    stationary_native_drag_case(false, false);
}

#[test]
fn native_drag_enters_same_surface_at_start_without_further_motion() {
    stationary_native_drag_case(true, false);
}

#[test]
fn native_source_less_drag_cannot_enter_other_client_at_start() {
    stationary_native_drag_case(false, true);
}

#[test]
fn native_source_less_drag_enters_same_surface_at_start_without_further_motion() {
    stationary_native_drag_case(true, true);
}

// --- Clipboard retention after the owner goes (#1033) ---

use std::time::Duration;

/// What the first copying client serves (distinct from `PASTE_BYTES`).
const COPY_A: &[u8] = b"copied by the first client";
/// What the paste side serves once it copies too.
const COPY_B: &[u8] = b"copied by the second client";

/// One connected clipboard client.
type Side = (Connection, EventQueue<ClipClient>, ClipClient);

/// Dispatch whatever reached one client; an empty socket is fine.
fn drain(queue: &mut EventQueue<ClipClient>, client: &mut ClipClient) {
    if let Some(guard) = queue.prepare_read() {
        match guard.read() {
            Ok(_) => {}
            Err(wayland_client::backend::WaylandError::Io(error))
                if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => panic!("client read failed: {error:?}"),
        }
    }
    queue.dispatch_pending(client).unwrap();
}

/// Two mapped clipboard clients: `copy` (A), which may disconnect, and
/// `paste` (B).
struct Retention {
    comp: TestCompositor,
    manager: WindowManager,
    a: Option<Side>,
    b: Side,
    id_a: u64,
    id_b: u64,
    time: u32,
}

impl Retention {
    fn new() -> Self {
        let mut comp = TestCompositor::new();
        let mut manager: WindowManager = comp.window_manager();
        let (conn_a, mut queue_a, mut a) = clip_connect(&mut comp);
        let (conn_b, mut queue_b, mut b) = clip_connect(&mut comp);
        map_selection_client(&mut comp, &conn_a, &mut queue_a, &mut a, "copy");
        map_selection_client(&mut comp, &conn_b, &mut queue_b, &mut b, "paste");
        manager.reconcile(&mut comp.state);
        sync_mapped(
            &mut comp,
            &conn_a,
            &mut queue_a,
            &mut a,
            &conn_b,
            &mut queue_b,
            &mut b,
        );
        let id = |title: &str| {
            manager
                .model()
                .windows()
                .find(|w| w.title == title)
                .unwrap()
                .id
        };
        let (id_a, id_b) = (id("copy"), id("paste"));
        Self {
            comp,
            manager,
            a: Some((conn_a, queue_a, a)),
            b: (conn_b, queue_b, b),
            id_a,
            id_b,
            time: 5000,
        }
    }

    fn side(&mut self, on_a: bool) -> &mut Side {
        if on_a {
            self.a.as_mut().expect("the copying client disconnected")
        } else {
            &mut self.b
        }
    }

    /// Pump the compositor and every connected client until `done`.
    fn pump(&mut self, done: impl Fn(Option<&ClipClient>, &ClipClient) -> bool) {
        for _ in 0..PUMP_ROUNDS {
            if let Some((_, queue, _)) = self.a.as_mut() {
                queue.flush().unwrap();
            }
            self.b.1.flush().unwrap();
            self.comp.pump();
            if let Some((_, queue, client)) = self.a.as_mut() {
                drain(queue, client);
            }
            drain(&mut self.b.1, &mut self.b.2);
            if done(self.a.as_ref().map(|side| &side.2), &self.b.2) {
                return;
            }
            // The clipboard manager copies and serves on its own threads.
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("pump budget exhausted");
    }

    fn focus(&mut self, on_a: bool) {
        let id = if on_a { self.id_a } else { self.id_b };
        assert!(self.manager.focus(&mut self.comp.state, Some(id)));
    }

    fn key(&mut self, pressed: bool) {
        self.time += 1;
        self.manager.on_input(
            &mut self.comp.state,
            ManagerInput::Key {
                keycode: KEY_A,
                pressed,
                time: self.time,
            },
        );
    }

    /// Focus one side and press a key there: an honest serial for
    /// `set_selection`.
    fn key_serial(&mut self, on_a: bool) -> u32 {
        self.focus(on_a);
        let keys = move |a: Option<&ClipClient>, b: &ClipClient| {
            if on_a {
                a.map_or(0, |a| a.keys.len())
            } else {
                b.keys.len()
            }
        };
        let before = keys(self.a.as_ref().map(|side| &side.2), &self.b.2);
        self.key(true);
        self.pump(|a, b| keys(a, b) > before);
        let serial = self.side(on_a).2.keys.last().unwrap().1;
        self.key(false);
        self.pump(|_, _| true);
        serial
    }

    /// One side takes the clipboard, offering `mime_types` and serving
    /// `payload` for each.
    fn copy(&mut self, on_a: bool, mime_types: &[&str], payload: &[u8]) {
        let serial = self.key_serial(on_a);
        let (_, queue, client) = self.side(on_a);
        let source = client
            .manager
            .as_ref()
            .unwrap()
            .create_data_source(&queue.handle(), ());
        for mime_type in mime_types {
            source.offer((*mime_type).to_owned());
        }
        client
            .device
            .as_ref()
            .unwrap()
            .set_selection(Some(&source), serial);
        client.source = Some(source);
        client.payload = Some(payload.to_vec());
        self.pump(|_, _| true);
    }

    /// Wait for the clipboard manager's copy of the current selection.
    fn wait_copy_saved(&mut self) {
        for _ in 0..PUMP_ROUNDS {
            self.pump(|_, _| true);
            if self.comp.clipboard_copy_saved() {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("the clipboard copy never finished");
    }

    /// B's current selection offer, read as `mime_type`.
    fn paste(&mut self, mime_type: &str) -> Vec<u8> {
        use std::io::Read;
        use std::os::fd::AsFd;
        let (read_end, write_end) = UnixStream::pair().unwrap();
        self.b
            .2
            .selection
            .as_ref()
            .expect("no selection offer")
            .receive(mime_type.to_owned(), write_end.as_fd());
        // One round carries the request to whoever serves it.
        self.pump(|_, _| true);
        // Our copy of the write end must go for end of file to follow
        // the server's.
        drop(write_end);
        read_end
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut pasted = Vec::new();
        std::io::BufReader::new(read_end)
            .read_to_end(&mut pasted)
            .unwrap();
        pasted
    }

    /// Wait until B's newest offer, the `count`th, is its selection.
    fn wait_offer(&mut self, count: usize) {
        self.pump(|_, b| b.offers.len() == count && b.selection.as_ref() == b.offers.last());
    }
}

/// The copying client quits: the focused client is offered the saved
/// copy, and pasting it returns the quit client's exact bytes.
#[test]
fn clipboard_survives_owner_disconnect() {
    let mut t = Retention::new();
    t.copy(true, &["text/plain;charset=utf-8"], COPY_A);
    t.wait_copy_saved();
    assert_eq!(
        t.side(true).2.sent_mimes,
        vec!["text/plain;charset=utf-8".to_owned()],
        "the compositor copies the selection when it is set"
    );
    t.focus(false);
    t.wait_offer(1);
    t.b.2.offered_mimes.clear();

    t.a = None;
    t.wait_offer(2);
    assert_eq!(
        t.b.2.offered_mimes,
        vec!["text/plain;charset=utf-8".to_owned()]
    );
    assert_eq!(t.paste("text/plain;charset=utf-8"), COPY_A);
    // The saved copy serves every paste, not just the first.
    assert_eq!(t.paste("text/plain;charset=utf-8"), COPY_A);
}

/// The copying client clears the clipboard: the saved copy takes over.
#[test]
fn clipboard_survives_owner_null_selection() {
    let mut t = Retention::new();
    t.copy(true, &["text/plain;charset=utf-8"], COPY_A);
    t.wait_copy_saved();
    let serial = t.key_serial(true);
    t.side(true)
        .2
        .device
        .as_ref()
        .unwrap()
        .set_selection(None, serial);
    t.pump(|_, _| true);

    t.focus(false);
    t.wait_offer(1);
    assert_eq!(
        t.b.2.offered_mimes,
        vec!["text/plain;charset=utf-8".to_owned()]
    );
    assert_eq!(t.paste("text/plain;charset=utf-8"), COPY_A);
}

/// Nothing supported is offered: nothing is copied, and the clipboard
/// empties when its owner quits.
#[test]
fn unsupported_clipboard_type_is_not_retained() {
    let mut t = Retention::new();
    t.copy(true, &["application/x-foo"], COPY_A);
    for _ in 0..10 {
        t.pump(|_, _| true);
    }
    assert!(t.side(true).2.sent_mimes.is_empty());
    assert!(!t.comp.clipboard_copy_saved());
    t.focus(false);
    t.wait_offer(1);

    t.a = None;
    t.pump(|_, b| b.selection.is_none());
    assert_eq!(t.b.2.offers.len(), 1, "no retained offer may follow");
}

/// Mutter's preference order picks the copied type; only that type is
/// offered afterwards.
#[test]
fn retained_clipboard_keeps_the_most_preferred_type() {
    let mut t = Retention::new();
    t.copy(true, &["image/jpeg", "image/png", "image/bmp"], COPY_A);
    t.wait_copy_saved();
    assert_eq!(t.side(true).2.sent_mimes, vec!["image/png".to_owned()]);
    t.focus(false);
    t.wait_offer(1);
    t.b.2.offered_mimes.clear();

    t.a = None;
    t.wait_offer(2);
    assert_eq!(t.b.2.offered_mimes, vec!["image/png".to_owned()]);
    assert_eq!(t.paste("image/png"), COPY_A);
    assert!(
        t.paste("image/jpeg").is_empty(),
        "only the saved type is served"
    );
}

/// A client selection after the take-over replaces the retained copy;
/// when that owner's source goes, its own copy is the one retained.
#[test]
fn new_client_selection_replaces_retained_copy() {
    let mut t = Retention::new();
    t.copy(true, &["text/plain;charset=utf-8"], COPY_A);
    t.wait_copy_saved();
    t.focus(false);
    t.wait_offer(1);
    t.a = None;
    t.wait_offer(2);
    // The runtime drops the quit client's window after its dispatch.
    t.manager.reconcile(&mut t.comp.state);
    t.pump(|_, _| true);
    let offers = t.b.2.offers.len();

    t.copy(false, &["text/plain"], COPY_B);
    t.wait_copy_saved();
    assert_eq!(t.b.2.sent_mimes, vec!["text/plain".to_owned()]);
    // B holds the focus, so it is offered its own selection.
    t.wait_offer(offers + 1);
    t.b.2.offered_mimes.clear();

    t.b.2.source.take().unwrap().destroy();
    t.wait_offer(offers + 2);
    assert_eq!(t.b.2.offered_mimes, vec!["text/plain".to_owned()]);
    assert_eq!(t.paste("text/plain"), COPY_B);
    assert!(
        t.paste("text/plain;charset=utf-8").is_empty(),
        "the first client's copy is gone"
    );
}
