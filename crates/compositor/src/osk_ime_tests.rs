//! Consumer-facing private-global admission, including a guessed bind.
use crate::{ClientState, TestCompositor};
use std::{os::unix::net::UnixStream, sync::Arc};
use wayland_client::{
    protocol::wl_registry::{self, WlRegistry},
    Connection, Dispatch, EventQueue, QueueHandle,
};

#[allow(dead_code, unused_imports, non_camel_case_types, clippy::all)]
mod protocol {
    use wayland_client;
    pub mod __interfaces {
        use wayland_client::backend as wayland_backend;
        wayland_scanner::generate_interfaces!("protocols/tuna-osk-input-v1.xml");
    }
    use self::__interfaces::*;
    wayland_scanner::generate_client_code!("protocols/tuna-osk-input-v1.xml");
}
use protocol::tuna_osk_input_v1::TunaOskInputV1;
#[derive(Default)]
struct Client {
    global: Option<u32>,
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
            if interface == "tuna_osk_input_v1" {
                state.global = Some(name);
            }
        }
    }
}
wayland_client::delegate_noop!(Client: ignore TunaOskInputV1);
struct Peer {
    queue: EventQueue<Client>,
    state: Client,
    registry: WlRegistry,
}
fn connect(comp: &mut TestCompositor, trusted: bool) -> Peer {
    let (server, stream) = UnixStream::pair().unwrap();
    comp.display
        .handle()
        .insert_client(
            server,
            Arc::new(if trusted {
                ClientState::ime_bridge()
            } else {
                ClientState::default()
            }),
        )
        .unwrap();
    let conn = Connection::from_socket(stream).unwrap();
    let queue = conn.new_event_queue();
    let registry = conn.display().get_registry(&queue.handle(), ());
    Peer {
        queue,
        state: Client::default(),
        registry,
    }
}
fn pump(comp: &mut TestCompositor, peer: &mut Peer) -> Result<(), String> {
    peer.queue.flush().map_err(|e| e.to_string())?;
    comp.pump();
    if let Some(read) = peer.queue.prepare_read() {
        read.read().map_err(|e| e.to_string())?;
    }
    peer.queue
        .dispatch_pending(&mut peer.state)
        .map(|_| ())
        .map_err(|e| e.to_string())
}
#[test]
fn virtual_text_is_private_even_when_an_ordinary_client_guesses_the_global() {
    let mut comp = TestCompositor::new();
    let mut bridge = connect(&mut comp, true);
    let mut app = connect(&mut comp, false);
    pump(&mut comp, &mut bridge).unwrap();
    pump(&mut comp, &mut app).unwrap();
    let global = bridge
        .state
        .global
        .expect("supervised bridge sees virtual input");
    assert!(
        app.state.global.is_none(),
        "ordinary apps must not discover virtual input"
    );
    let _forged: TunaOskInputV1 = app.registry.bind(global, 1, &app.queue.handle(), ());
    assert!(
        pump(&mut comp, &mut app).is_err(),
        "guessing the name must not grant admission"
    );
}
