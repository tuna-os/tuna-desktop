//! Additional real socket boundaries using protocol_gaps' existing harness.
use super::*;
use std::io::Write;
use wayland_client::protocol::{wl_subcompositor::WlSubcompositor, wl_subsurface::WlSubsurface};
use wayland_client::Proxy;

wayland_client::delegate_noop!(Client: ignore WlSubcompositor);
wayland_client::delegate_noop!(Client: ignore WlSubsurface);

fn connect_with_wire(comp: &mut TestCompositor, manager: &mut WindowManager) -> (Peer, UnixStream) {
    let (server, stream) = UnixStream::pair().unwrap();
    let wire = stream.try_clone().unwrap();
    comp.add_client(server);
    let conn = Connection::from_socket(stream).unwrap();
    let queue = conn.new_event_queue();
    let registry = conn.display().get_registry(&queue.handle(), ());
    let mut peer = Peer { _conn: conn, queue, registry, client: Client::default() };
    pump(comp, manager, &mut [&mut peer]);
    (peer, wire)
}

fn raw_region(wire: &mut UnixStream, effect: &ExtBackgroundEffectSurfaceV1, region: u32) {
    // ext_background_effect_surface_v1.set_blur_region: opcode 1, 12 bytes.
    // Raw wire is essential: the typed API rejects foreign/stale proxies itself.
    let mut bytes = [0_u8; 12];
    for (index, word) in [effect.id().protocol_id(), (12_u32 << 16) | 1, region].into_iter().enumerate() {
        bytes[index * 4..index * 4 + 4].copy_from_slice(&word.to_ne_bytes());
    }
    wire.write_all(&bytes).unwrap();
}

#[test]
fn background_effect_foreign_region_id_is_rejected_without_touching_owner() {
    let (mut comp, mut manager) = compositor();
    let mut owner = connect(&mut comp, &mut manager);
    let compositor: WlCompositor = owner.bind(6);
    let owner_qh = owner.qh();
    // Make the foreign id unambiguously absent from the attacker's namespace.
    let regions: Vec<_> = (0..64).map(|_| compositor.create_region(&owner_qh, ())).collect();
    let foreign = regions.last().unwrap();
    foreign.add(0, 0, 40, 40);
    pump(&mut comp, &mut manager, &mut [&mut owner]);

    let (mut attacker, mut wire) = connect_with_wire(&mut comp, &mut manager);
    let (surface, _top) = window(&mut attacker, "foreign region attacker");
    let effects: ExtBackgroundEffectManagerV1 = attacker.bind(1);
    let effect = effects.get_background_effect(&surface, &attacker.qh(), ());
    pump(&mut comp, &mut manager, &mut [&mut attacker]);
    raw_region(&mut wire, &effect, foreign.id().protocol_id());
    let error = pump_to_error(&mut comp, &mut manager, &mut attacker);
    assert_eq!(error.object_interface, "wl_display");
    assert_eq!(error.code, wayland_client::protocol::wl_display::Error::InvalidObject as u32);
    // The foreign client and its region still work after the other disconnect.
    foreign.add(-10, -10, 10, 10);
    pump(&mut comp, &mut manager, &mut [&mut owner]);
    assert!(owner._conn.protocol_error().is_none());
}

#[test]
fn background_effect_stale_region_id_is_rejected_at_socket_boundary() {
    let (mut comp, mut manager) = compositor();
    let (mut peer, mut wire) = connect_with_wire(&mut comp, &mut manager);
    let (surface, _top) = window(&mut peer, "stale region attacker");
    let effects: ExtBackgroundEffectManagerV1 = peer.bind(1);
    let compositor: WlCompositor = peer.bind(6);
    let qh = peer.qh();
    let effect = effects.get_background_effect(&surface, &qh, ());
    let region = compositor.create_region(&qh, ());
    let stale = region.id().protocol_id();
    region.destroy();
    // Do not allocate any new object after delete_id, so this remains stale.
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    raw_region(&mut wire, &effect, stale);
    let error = pump_to_error(&mut comp, &mut manager, &mut peer);
    assert_eq!(error.object_interface, "wl_display");
    assert_eq!(error.code, wayland_client::protocol::wl_display::Error::InvalidObject as u32);
}

#[test]
fn background_effect_negative_region_is_copied_and_subsurface_state_waits_for_parent() {
    use smithay::reexports::wayland_server::Resource;
    use smithay::wayland::compositor::{with_surface_tree_downward, TraversalAction};
    let (mut comp, mut manager) = compositor();
    let mut peer = connect(&mut comp, &mut manager);
    let (parent, _top) = window(&mut peer, "effect subsurface parent");
    let compositor: WlCompositor = peer.bind(6);
    let subcompositor: WlSubcompositor = peer.bind(1);
    let effects: ExtBackgroundEffectManagerV1 = peer.bind(1);
    let qh = peer.qh();
    let child = compositor.create_surface(&qh, ());
    let _subsurface = subcompositor.get_subsurface(&child, &parent, &qh, ());
    let effect = effects.get_background_effect(&child, &qh, ());
    let region = compositor.create_region(&qh, ());
    region.add(-20, -10, 100, 80);
    region.subtract(0, 0, 10, 10);
    effect.set_blur_region(Some(&region));
    region.subtract(-20, -10, 100, 80);
    region.destroy();
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    let server_parent = comp.state.toplevels()[0].wl_surface().clone();
    let mut server_child = None;
    with_surface_tree_downward(&server_parent, (), |_, _, _| TraversalAction::DoChildren(()),
        |surface, _, _| {
            if surface.id().protocol_id() == child.id().protocol_id() {
                server_child = Some(surface.clone());
            }
        }, |_, _, _| true);
    let server_child = server_child.expect("real child in server surface tree");
    assert_eq!(blur_area(&server_child), None);
    child.commit();
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert_eq!(blur_area(&server_child), None, "synchronized state waits for parent");
    parent.commit();
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert_eq!(blur_area(&server_child), Some(100 * 80 - 10 * 10));
    // Introspection is intentionally unclipped; the rendered consumer proof
    // separately checks clipping to the actual 384x256 committed shm buffer.
    let committed = tuna_compositor::background_effect::blur_region(&server_child).unwrap();
    assert!(committed.iter().any(|rect| rect.loc.x < 0 || rect.loc.y < 0));
    effect.destroy();
    child.commit();
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert!(blur_area(&server_child).is_some());
    parent.commit();
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert_eq!(blur_area(&server_child), None);
}
