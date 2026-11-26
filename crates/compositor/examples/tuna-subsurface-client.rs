//! Opaque toplevel with two opaque subsurfaces for G-SUBSURFACE-STACKING:
//! a green one placed above the red parent, inside it at (50, 100), and a
//! blue one placed below it at (300, 100), half outside the parent's
//! right edge. The parent must cover the blue one where they overlap and
//! never the green one.
use std::{io::Write, os::fd::AsFd};
use wayland_client::{
    protocol::{
        wl_buffer, wl_compositor, wl_registry, wl_shm, wl_shm_pool, wl_subcompositor,
        wl_subsurface, wl_surface,
    },
    Connection, Dispatch, QueueHandle,
};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

const PARENT: (i32, i32, u32) = (400, 300, 0xffc82828);
const ABOVE: (i32, i32, u32) = (100, 100, 0xff28c828);
const BELOW: (i32, i32, u32) = (150, 100, 0xff2828c8);

#[derive(Default)]
struct Peer {
    compositor: Option<wl_compositor::WlCompositor>,
    subcompositor: Option<wl_subcompositor::WlSubcompositor>,
    shm: Option<wl_shm::WlShm>,
    base: Option<xdg_wm_base::XdgWmBase>,
    /// Parent, above, below: each surface with its buffer, committed
    /// children first so the parent's commit applies all three.
    surfaces: Vec<(wl_surface::WlSurface, wl_buffer::WlBuffer, (i32, i32))>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for Peer {
    fn event(
        s: &mut Self,
        p: &wl_registry::WlRegistry,
        e: wl_registry::Event,
        _: &(),
        _: &Connection,
        q: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = e
        {
            match interface.as_str() {
                "wl_compositor" => s.compositor = Some(p.bind(name, version.min(4), q, ())),
                "wl_subcompositor" => s.subcompositor = Some(p.bind(name, 1, q, ())),
                "wl_shm" => s.shm = Some(p.bind(name, 1, q, ())),
                "xdg_wm_base" => s.base = Some(p.bind(name, version.min(6), q, ())),
                _ => {}
            }
        }
    }
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for Peer {
    fn event(
        _: &mut Self,
        p: &xdg_wm_base::XdgWmBase,
        e: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = e {
            p.pong(serial);
        }
    }
}

impl Dispatch<xdg_surface::XdgSurface, ()> for Peer {
    fn event(
        s: &mut Self,
        p: &xdg_surface::XdgSurface,
        e: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = e {
            p.ack_configure(serial);
            // Synchronized subsurfaces: their state is cached until the
            // parent commits, so the parent goes last.
            for (surface, buffer, (w, h)) in s.surfaces.iter().rev() {
                surface.attach(Some(buffer), 0, 0);
                surface.damage_buffer(0, 0, *w, *h);
                surface.commit();
            }
        }
    }
}

wayland_client::delegate_noop!(Peer: ignore wl_compositor::WlCompositor);
wayland_client::delegate_noop!(Peer: ignore wl_subcompositor::WlSubcompositor);
wayland_client::delegate_noop!(Peer: ignore wl_subsurface::WlSubsurface);
wayland_client::delegate_noop!(Peer: ignore wl_shm::WlShm);
wayland_client::delegate_noop!(Peer: ignore wl_shm_pool::WlShmPool);
wayland_client::delegate_noop!(Peer: ignore wl_buffer::WlBuffer);
wayland_client::delegate_noop!(Peer: ignore wl_surface::WlSurface);
wayland_client::delegate_noop!(Peer: ignore xdg_toplevel::XdgToplevel);

/// A surface with one opaque buffer of `(width, height, argb)`.
fn solid(
    peer: &mut Peer,
    q: &QueueHandle<Peer>,
    (width, height, argb): (i32, i32, u32),
) -> wl_surface::WlSurface {
    let mut file = tempfile::tempfile().unwrap();
    let pixels = argb.to_ne_bytes().repeat((width * height) as usize);
    file.write_all(&pixels).unwrap();
    let pool =
        peer.shm
            .as_ref()
            .expect("wl_shm")
            .create_pool(file.as_fd(), pixels.len() as i32, q, ());
    let buffer = pool.create_buffer(0, width, height, width * 4, wl_shm::Format::Argb8888, q, ());
    pool.destroy();
    let surface = peer
        .compositor
        .as_ref()
        .expect("wl_compositor")
        .create_surface(q, ());
    peer.surfaces
        .push((surface.clone(), buffer, (width, height)));
    surface
}

fn main() {
    let conn = Connection::connect_to_env().unwrap();
    let mut queue = conn.new_event_queue();
    let q = queue.handle();
    let mut peer = Peer::default();
    conn.display().get_registry(&q, ());
    queue.roundtrip(&mut peer).unwrap();
    let parent = solid(&mut peer, &q, PARENT);
    let above = solid(&mut peer, &q, ABOVE);
    let below = solid(&mut peer, &q, BELOW);
    let subcompositor = peer.subcompositor.clone().expect("wl_subcompositor");
    let above_sub = subcompositor.get_subsurface(&above, &parent, &q, ());
    above_sub.set_position(50, 100);
    above_sub.place_above(&parent);
    let below_sub = subcompositor.get_subsurface(&below, &parent, &q, ());
    below_sub.set_position(300, 100);
    below_sub.place_below(&parent);
    let xdg = peer
        .base
        .as_ref()
        .expect("xdg_wm_base")
        .get_xdg_surface(&parent, &q, ());
    let top = xdg.get_toplevel(&q, ());
    top.set_app_id("tuna-subsurface-proof".into());
    top.set_title("Tuna Subsurface Proof".into());
    xdg.set_window_geometry(0, 0, PARENT.0, PARENT.1);
    parent.commit();
    loop {
        queue.blocking_dispatch(&mut peer).unwrap();
    }
}
