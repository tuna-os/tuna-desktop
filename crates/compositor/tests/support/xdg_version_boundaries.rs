//! Socket consumers for xdg-shell's negotiated v7 constrained-edge states.
//! Version 7 adds state entries, not requests: the request/lifecycle checks use
//! the existing XML requests, including its required invalid-parent error.
use super::*;
use std::os::fd::AsFd;
use tuna_compositor::windows::{TileSide, WindowLayout};
use wayland_client::{backend::ObjectId, Proxy};
use wayland_protocols::xdg::shell::client::xdg_toplevel::{self, State as Edge};

#[derive(Debug)]
struct Configure {
    width: i32,
    height: i32,
    states: Vec<u32>,
}

#[derive(Default)]
pub(super) struct ConfigureLog {
    toplevels: HashMap<ObjectId, Vec<Configure>>,
    surfaces: HashMap<ObjectId, Vec<u32>>,
}

impl Dispatch<XdgToplevel, ()> for Client {
    fn event(
        state: &mut Self,
        toplevel: &XdgToplevel,
        event: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_toplevel::Event::Configure {
            width,
            height,
            states,
        } = event
        {
            assert_eq!(
                states.len() % 4,
                0,
                "configure states are native-endian uint32s"
            );
            state
                .xdg_configures
                .toplevels
                .entry(toplevel.id())
                .or_default()
                .push(Configure {
                    width,
                    height,
                    states: states
                        .chunks_exact(4)
                        .map(|bytes| u32::from_ne_bytes(bytes.try_into().unwrap()))
                        .collect(),
                });
        }
    }
}

pub(super) fn record_surface_configure(state: &mut Client, surface: &XdgSurface, serial: u32) {
    state
        .xdg_configures
        .surfaces
        .entry(surface.id())
        .or_default()
        .push(serial);
}

struct VersionWindow {
    wm: XdgWmBase,
    surface: WlSurface,
    xdg: XdgSurface,
    top: XdgToplevel,
    buffer: Option<WlBuffer>,
    id: u64,
}

impl VersionWindow {
    fn counts(&self, peer: &Peer) -> (usize, usize) {
        (
            peer.client
                .xdg_configures
                .toplevels
                .get(&self.top.id())
                .map_or(0, Vec::len),
            peer.client
                .xdg_configures
                .surfaces
                .get(&self.xdg.id())
                .map_or(0, Vec::len),
        )
    }

    fn latest<'a>(&self, peer: &'a Peer) -> &'a Configure {
        peer.client.xdg_configures.toplevels[&self.top.id()]
            .last()
            .unwrap()
    }

    fn commit_size(&mut self, peer: &Peer, width: i32, height: i32) {
        assert!(width > 0 && height > 0);
        let shm: WlShm = peer.bind(1);
        let qh = peer.qh();
        let stride = width * 4;
        let file = tempfile::tempfile().unwrap();
        file.set_len((stride * height) as u64).unwrap();
        let pool = shm.create_pool(file.as_fd(), stride * height, &qh, ());
        let buffer =
            pool.create_buffer(0, width, height, stride, wl_shm::Format::Argb8888, &qh, ());
        self.xdg.set_window_geometry(0, 0, width, height);
        self.surface.attach(Some(&buffer), 0, 0);
        self.surface.damage_buffer(0, 0, width, height);
        self.surface.commit();
        pool.destroy();
        if let Some(previous) = self.buffer.replace(buffer) {
            previous.destroy();
        }
    }

    fn destroy(self) {
        self.top.destroy();
        self.xdg.destroy();
        self.surface.destroy();
        self.wm.destroy();
        if let Some(buffer) = self.buffer {
            buffer.destroy();
        }
    }
}

fn mapped(
    comp: &mut TestCompositor,
    manager: &mut WindowManager,
    peer: &mut Peer,
    version: u32,
    title: &str,
) -> VersionWindow {
    assert!(peer.client.globals["xdg_wm_base"].1 >= 7);
    let compositor: WlCompositor = peer.bind(6);
    let wm: XdgWmBase = peer.bind(version);
    assert_eq!(
        wm.version(),
        version,
        "the old client must really negotiate v6"
    );
    let qh = peer.qh();
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm.get_xdg_surface(&surface, &qh, ());
    let top = xdg.get_toplevel(&qh, ());
    assert_eq!(xdg.version(), version);
    assert_eq!(top.version(), version);
    top.set_title(title.into());
    top.set_app_id("org.tuna.XdgVersionBoundary".into());
    surface.commit();
    pump(comp, manager, &mut [&mut *peer]);
    let mut window = VersionWindow {
        wm,
        surface,
        xdg,
        top,
        buffer: None,
        id: id_of(manager, title),
    };
    // The initial configure has been ACKed by the existing Client dispatch.
    // A real shm commit places the window before any layout assertions.
    window.commit_size(peer, 400, 300);
    pump(comp, manager, &mut [&mut *peer]);
    assert_eq!(
        manager.window_layout(window.id),
        Some(WindowLayout::Floating)
    );
    assert_eq!(window.counts(peer).0, window.counts(peer).1);
    window
}

fn assert_states(configure: &Configure, expected: &[Edge]) {
    let mut actual = configure.states.clone();
    let mut expected: Vec<_> = expected.iter().map(|state| *state as u32).collect();
    actual.sort_unstable();
    expected.sort_unstable();
    // Compare the entire wire array, not just presence of the new entries:
    // stale constrained edges, unknown states, and duplicates must all fail.
    assert_eq!(actual, expected);
}

fn transition(
    comp: &mut TestCompositor,
    manager: &mut WindowManager,
    peer: &mut Peer,
    window: &mut VersionWindow,
    layout: WindowLayout,
    expected: &[Edge],
    action: impl FnOnce(&VersionWindow, &mut TestCompositor, &mut WindowManager),
) {
    let before = window.counts(peer);
    action(window, comp, manager);
    pump(comp, manager, &mut [&mut *peer]);
    let after = window.counts(peer);
    assert!(
        after.0 > before.0,
        "the action must deliver a generated configure event"
    );
    assert_eq!(
        after.0 - before.0,
        after.1 - before.1,
        "each configure sequence ends with xdg_surface.configure"
    );
    for configure in &peer.client.xdg_configures.toplevels[&window.top.id()][before.0..] {
        assert_states(configure, expected);
    }
    let geometry = manager.geometry(window.id).unwrap();
    let latest = window.latest(peer);
    assert_eq!(
        (latest.width, latest.height),
        (geometry.size.w, geometry.size.h)
    );
    assert_eq!(manager.window_layout(window.id), Some(layout));
    // Exercise the generated ACK followed by a correctly sized commit; the
    // next transition must remain usable, not merely emit one initial event.
    window.commit_size(peer, latest.width, latest.height);
    pump(comp, manager, &mut [&mut *peer]);
    assert_states(window.latest(peer), expected);
    assert!(peer._conn.protocol_error().is_none());
}

#[test]
fn xdg_v7_and_v6_clients_consume_layout_changes_and_repeated_configures() {
    for version in [7, 6] {
        let (mut comp, mut manager) = compositor();
        let mut peer = connect(&mut comp, &mut manager);
        let mut window = mapped(
            &mut comp,
            &mut manager,
            &mut peer,
            version,
            "negotiated layout client",
        );
        assert_states(window.latest(&peer), &[Edge::Activated]);
        let left = if version == 7 {
            vec![
                Edge::Activated,
                Edge::TiledTop,
                Edge::TiledBottom,
                Edge::TiledLeft,
                Edge::ConstrainedTop,
                Edge::ConstrainedBottom,
                Edge::ConstrainedLeft,
            ]
        } else {
            vec![
                Edge::Activated,
                Edge::TiledTop,
                Edge::TiledBottom,
                Edge::TiledLeft,
            ]
        };
        let right = if version == 7 {
            vec![
                Edge::Activated,
                Edge::TiledTop,
                Edge::TiledRight,
                Edge::TiledBottom,
                Edge::ConstrainedTop,
                Edge::ConstrainedRight,
                Edge::ConstrainedBottom,
            ]
        } else {
            vec![
                Edge::Activated,
                Edge::TiledTop,
                Edge::TiledRight,
                Edge::TiledBottom,
            ]
        };
        let maximized = if version == 7 {
            vec![
                Edge::Activated,
                Edge::Maximized,
                Edge::TiledTop,
                Edge::TiledRight,
                Edge::TiledBottom,
                Edge::TiledLeft,
                Edge::ConstrainedTop,
                Edge::ConstrainedRight,
                Edge::ConstrainedBottom,
                Edge::ConstrainedLeft,
            ]
        } else {
            vec![
                Edge::Activated,
                Edge::Maximized,
                Edge::TiledTop,
                Edge::TiledRight,
                Edge::TiledBottom,
                Edge::TiledLeft,
            ]
        };
        for _ in 0..2 {
            transition(
                &mut comp,
                &mut manager,
                &mut peer,
                &mut window,
                WindowLayout::Tiled(TileSide::Left),
                &left,
                |w, c, m| {
                    assert!(m.set_tiled(&mut c.state, w.id, TileSide::Left));
                },
            );
            transition(
                &mut comp,
                &mut manager,
                &mut peer,
                &mut window,
                WindowLayout::Tiled(TileSide::Right),
                &right,
                |w, c, m| {
                    assert!(m.set_tiled(&mut c.state, w.id, TileSide::Right));
                },
            );
            transition(
                &mut comp,
                &mut manager,
                &mut peer,
                &mut window,
                WindowLayout::Maximized,
                &maximized,
                |w, _, _| w.top.set_maximized(),
            );
            // An unchanged request still has a valid configure/ACK lifecycle.
            transition(
                &mut comp,
                &mut manager,
                &mut peer,
                &mut window,
                WindowLayout::Maximized,
                &maximized,
                |w, _, _| w.top.set_maximized(),
            );
            transition(
                &mut comp,
                &mut manager,
                &mut peer,
                &mut window,
                WindowLayout::Floating,
                &[Edge::Activated],
                |w, _, _| w.top.unset_maximized(),
            );
            transition(
                &mut comp,
                &mut manager,
                &mut peer,
                &mut window,
                WindowLayout::Fullscreen,
                &[Edge::Activated, Edge::Fullscreen],
                |w, _, _| w.top.set_fullscreen(None),
            );
            transition(
                &mut comp,
                &mut manager,
                &mut peer,
                &mut window,
                WindowLayout::Floating,
                &[Edge::Activated],
                |w, _, _| w.top.unset_fullscreen(),
            );
            let strip = [
                Edge::Activated,
                Edge::TiledTop,
                Edge::TiledRight,
                Edge::TiledBottom,
                Edge::TiledLeft,
            ];
            transition(
                &mut comp,
                &mut manager,
                &mut peer,
                &mut window,
                WindowLayout::Strip,
                &strip,
                |_, c, m| {
                    assert!(m.set_scroll(&mut c.state, true));
                },
            );
            // In scroll mode a native maximize request must stay an unconstrained
            // strip column, rather than leaking its previous monitor constraints.
            transition(
                &mut comp,
                &mut manager,
                &mut peer,
                &mut window,
                WindowLayout::Strip,
                &strip,
                |w, _, _| w.top.set_maximized(),
            );
            transition(
                &mut comp,
                &mut manager,
                &mut peer,
                &mut window,
                WindowLayout::Floating,
                &[Edge::Activated],
                |_, c, m| {
                    assert!(m.set_scroll(&mut c.state, false));
                },
            );
        }
        if version == 6 {
            for configure in &peer.client.xdg_configures.toplevels[&window.top.id()] {
                assert!(
                    configure
                        .states
                        .iter()
                        .all(|state| *state < Edge::ConstrainedLeft as u32),
                    "no v7-only enum may reach any old-client configure"
                );
            }
        }
        let id = window.id;
        window.destroy();
        pump(&mut comp, &mut manager, &mut [&mut peer]);
        assert!(manager.model().windows().all(|w| w.id != id));
        assert!(comp.state.toplevels().is_empty());
        assert!(peer._conn.protocol_error().is_none());
        // Destruction retires the old role; the same connection can map again.
        let replacement = mapped(&mut comp, &mut manager, &mut peer, version, "replacement");
        assert_states(replacement.latest(&peer), &[Edge::Activated]);
        replacement.destroy();
        pump(&mut comp, &mut manager, &mut [&mut peer]);
        assert_eq!(manager.model().windows().count(), 0);
        assert!(comp.state.toplevels().is_empty());
        assert!(peer._conn.protocol_error().is_none());
    }
}

#[test]
fn xdg_v6_bind_does_not_change_the_v7_clients_edge_events() {
    let (mut comp, mut manager) = compositor();
    let mut new_peer = connect(&mut comp, &mut manager);
    let new = mapped(&mut comp, &mut manager, &mut new_peer, 7, "v7 simultaneous");
    let mut old_peer = connect(&mut comp, &mut manager);
    let old = mapped(&mut comp, &mut manager, &mut old_peer, 6, "v6 simultaneous");
    pump(&mut comp, &mut manager, &mut [&mut new_peer, &mut old_peer]);
    assert!(manager.set_tiled(&mut comp.state, new.id, TileSide::Left));
    assert!(manager.set_tiled(&mut comp.state, old.id, TileSide::Left));
    pump(&mut comp, &mut manager, &mut [&mut new_peer, &mut old_peer]);
    assert_states(
        new.latest(&new_peer),
        &[
            Edge::TiledTop,
            Edge::TiledBottom,
            Edge::TiledLeft,
            Edge::ConstrainedTop,
            Edge::ConstrainedBottom,
            Edge::ConstrainedLeft,
        ],
    );
    assert_states(
        old.latest(&old_peer),
        &[
            Edge::Activated,
            Edge::TiledTop,
            Edge::TiledBottom,
            Edge::TiledLeft,
        ],
    );
    old.destroy();
    pump(&mut comp, &mut manager, &mut [&mut new_peer, &mut old_peer]);
    assert_states(
        new.latest(&new_peer),
        &[
            Edge::Activated,
            Edge::TiledTop,
            Edge::TiledBottom,
            Edge::TiledLeft,
            Edge::ConstrainedTop,
            Edge::ConstrainedBottom,
            Edge::ConstrainedLeft,
        ],
    );
    new.destroy();
    pump(&mut comp, &mut manager, &mut [&mut new_peer, &mut old_peer]);
    assert_eq!(manager.model().windows().count(), 0);
    assert!(comp.state.toplevels().is_empty());
    assert!(new_peer._conn.protocol_error().is_none());
    assert!(old_peer._conn.protocol_error().is_none());
}

#[test]
fn xdg_v7_and_v6_reject_the_xmls_invalid_self_parent_without_harming_other_clients() {
    for version in [7, 6] {
        let (mut comp, mut manager) = compositor();
        let mut survivor = connect(&mut comp, &mut manager);
        let mut good = mapped(
            &mut comp,
            &mut manager,
            &mut survivor,
            7,
            "surviving client",
        );
        let mut offender = connect(&mut comp, &mut manager);
        let bad = mapped(
            &mut comp,
            &mut manager,
            &mut offender,
            version,
            "invalid parent client",
        );
        let target = protocol_error_target(&bad.top);
        bad.top.set_parent(Some(&bad.top));
        let error = pump_to_error(&mut comp, &mut manager, &mut offender);
        assert_eq!(error.object_id, target.0);
        assert_eq!(error.object_interface, target.1);
        assert_eq!(error.code, xdg_toplevel::Error::InvalidParent as u32);
        pump(&mut comp, &mut manager, &mut [&mut survivor]);
        assert!(manager.model().windows().all(|w| w.id != bad.id));
        transition(
            &mut comp,
            &mut manager,
            &mut survivor,
            &mut good,
            WindowLayout::Fullscreen,
            &[Edge::Activated, Edge::Fullscreen],
            |w, _, _| w.top.set_fullscreen(None),
        );
        good.destroy();
        pump(&mut comp, &mut manager, &mut [&mut survivor]);
        assert_eq!(manager.model().windows().count(), 0);
        assert!(comp.state.toplevels().is_empty());
        assert!(survivor._conn.protocol_error().is_none());
    }
}

fn required_error(
    version: u32,
    on_surface: bool,
    code: u32,
    request: impl FnOnce(&VersionWindow, &Peer),
) {
    let (mut comp, mut manager) = compositor();
    let mut peer = connect(&mut comp, &mut manager);
    let window = mapped(
        &mut comp,
        &mut manager,
        &mut peer,
        version,
        "required xdg error",
    );
    let target = if on_surface {
        protocol_error_target(&window.xdg)
    } else {
        protocol_error_target(&window.top)
    };
    request(&window, &peer);
    let error = pump_to_error(&mut comp, &mut manager, &mut peer);
    assert_eq!(error.object_id, target.0);
    assert_eq!(error.object_interface, target.1);
    assert_eq!(error.code, code);
    // Complete disconnect cleanup without trying to flush the dead peer.
    pump(&mut comp, &mut manager, &mut []);
    assert_eq!(manager.model().windows().count(), 0);
    assert!(comp.state.toplevels().is_empty());
}

#[test]
fn xdg_required_invalid_window_geometry_uses_the_surface_error() {
    // Locked stable/xdg-shell/xdg-shell.xml set_window_geometry requires
    // strictly positive dimensions; negative positions remain legal.
    for version in [7, 6] {
        for (width, height) in [(0, 300), (400, 0), (-1, 300), (400, -1)] {
            required_error(
                version,
                true,
                xdg_surface::Error::InvalidSize as u32,
                |w, _| {
                    w.xdg.set_window_geometry(-10, -20, width, height);
                },
            );
        }
    }
}

#[test]
fn xdg_required_negative_size_hints_use_the_toplevel_error() {
    // Mutter 51's set_min/max_size and the locked XML agree: either
    // negative axis is an immediate xdg_toplevel.invalid_size error.
    for version in [7, 6] {
        for maximum in [false, true] {
            for (width, height) in [(-1, 300), (400, -1)] {
                required_error(
                    version,
                    false,
                    xdg_toplevel::Error::InvalidSize as u32,
                    |w, _| {
                        if maximum {
                            w.top.set_max_size(width, height);
                        } else {
                            w.top.set_min_size(width, height);
                        }
                    },
                );
            }
        }
    }
}

#[test]
fn xdg_required_conflicting_size_hints_fail_on_surface_commit() {
    for version in [7, 6] {
        for minimum_first in [false, true] {
            for (width, height) in [(500, 100), (100, 500)] {
                required_error(
                    version,
                    false,
                    xdg_toplevel::Error::InvalidSize as u32,
                    |w, _| {
                        if minimum_first {
                            w.top.set_min_size(width, height);
                            w.top.set_max_size(400, 400);
                        } else {
                            w.top.set_max_size(400, 400);
                            w.top.set_min_size(width, height);
                        }
                        w.surface.commit();
                    },
                );
            }
        }
    }
}

#[test]
fn xdg_required_unknown_duplicate_and_stale_acks_use_the_surface_error() {
    // ACK consumes the referenced serial AND all preceding serials. The
    // existing dispatch has already ACKed these actual socket events.
    for version in [7, 6] {
        for kind in ["unknown", "duplicate", "stale"] {
            required_error(
                version,
                true,
                xdg_surface::Error::InvalidSerial as u32,
                |w, peer| {
                    let serials = &peer.client.xdg_configures.surfaces[&w.xdg.id()];
                    assert!(serials.len() >= 2, "initial map and placement configure");
                    let serial = match kind {
                        "unknown" => (0..=u32::MAX)
                            .find(|serial| !serials.contains(serial))
                            .unwrap(),
                        "duplicate" => *serials.last().unwrap(),
                        "stale" => serials[0],
                        _ => unreachable!(),
                    };
                    w.xdg.ack_configure(serial);
                },
            );
        }
    }
}

#[test]
fn xdg_size_hints_are_double_buffered_and_zero_resets_limits() {
    use smithay::wayland::{compositor::with_states, shell::xdg::SurfaceCachedState};
    for version in [7, 6] {
        let (mut comp, mut manager) = compositor();
        let mut peer = connect(&mut comp, &mut manager);
        let window = mapped(
            &mut comp,
            &mut manager,
            &mut peer,
            version,
            "buffered size client",
        );
        let server = comp.state.toplevels()[0].wl_surface().clone();
        let hints = || {
            with_states(&server, |states| {
                let cached = *states.cached_state.get::<SurfaceCachedState>().current();
                (cached.min_size, cached.max_size)
            })
        };
        window.top.set_min_size(400, 300);
        window.top.set_max_size(500, 400);
        pump(&mut comp, &mut manager, &mut [&mut peer]);
        assert_eq!(hints(), ((0, 0).into(), (0, 0).into()));
        window.surface.commit();
        pump(&mut comp, &mut manager, &mut [&mut peer]);
        assert_eq!(hints(), ((400, 300).into(), (500, 400).into()));
        // An intermediate maximum below the old minimum is legal when
        // the same transaction also lowers the minimum before commit.
        window.top.set_max_size(300, 200);
        window.top.set_min_size(200, 100);
        pump(&mut comp, &mut manager, &mut [&mut peer]);
        assert_eq!(hints(), ((400, 300).into(), (500, 400).into()));
        window.surface.commit();
        pump(&mut comp, &mut manager, &mut [&mut peer]);
        assert_eq!(hints(), ((200, 100).into(), (300, 200).into()));
        window.top.set_max_size(0, 0);
        window.top.set_min_size(0, 0);
        // Negative offsets are legal: only dimensions must be positive.
        window.xdg.set_window_geometry(-10, -20, 400, 300);
        window.surface.commit();
        pump(&mut comp, &mut manager, &mut [&mut peer]);
        assert_eq!(hints(), ((0, 0).into(), (0, 0).into()));
        assert!(peer._conn.protocol_error().is_none());
        window.destroy();
        pump(&mut comp, &mut manager, &mut [&mut peer]);
        assert_eq!(manager.model().windows().count(), 0);
        assert!(comp.state.toplevels().is_empty());
    }
}
