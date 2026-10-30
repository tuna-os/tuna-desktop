//! drm-smoke's colour pixel proof client (P-DS-03, #341): a fullscreen
//! SHM window filled with one BT.2020/PQ code value and tagged with that
//! parametric image description through wp_color_manager_v1.
//!
//! Prints one line once the tagged frame was presented to the client:
//! `READY <code r g b> <expected r g b>`, where the expected 8-bit
//! scanout colour is the Mutter 51 transform to the SDR output
//! ([`tuna_compositor::color_state`]). Then stays mapped until killed.
use std::collections::HashMap;
use std::io::Write;
use std::os::fd::AsFd;

use tuna_compositor::color_state::{self, ColorState, Colorspace, Params, TransferFunction};
use wayland_client::{
    protocol::{
        wl_buffer::WlBuffer,
        wl_callback::{self, WlCallback},
        wl_compositor::WlCompositor,
        wl_registry::{self, WlRegistry},
        wl_shm::{self, WlShm},
        wl_shm_pool::WlShmPool,
        wl_surface::WlSurface,
    },
    Connection, Dispatch, QueueHandle,
};
use wayland_protocols::wp::color_management::v1::client::{
    wp_color_management_surface_v1::WpColorManagementSurfaceV1,
    wp_color_manager_v1::{self as manager, WpColorManagerV1},
    wp_image_description_creator_params_v1::WpImageDescriptionCreatorParamsV1,
    wp_image_description_v1::{self as image_desc, WpImageDescriptionV1},
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::{self, XdgToplevel},
    xdg_wm_base::{self, XdgWmBase},
};

/// The BT.2020/PQ code value the window shows (8-bit, R G B).
const CODE: [u8; 3] = [140, 120, 100];

#[derive(Default)]
struct Client {
    globals: HashMap<String, (u32, u32)>,
    size: Option<(i32, i32)>,
    configured: bool,
    described: Option<Result<(), String>>,
    presented: bool,
}

impl Client {
    fn bind<I: wayland_client::Proxy + 'static>(
        &self,
        registry: &WlRegistry,
        qh: &QueueHandle<Self>,
        version: u32,
    ) -> I
    where
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
        state: &mut Self,
        surface: &XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            surface.ack_configure(serial);
            state.configured = true;
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
            if width > 0 && height > 0 {
                state.size = Some((width, height));
            }
        }
    }
}

impl Dispatch<WpImageDescriptionV1, ()> for Client {
    fn event(
        state: &mut Self,
        _: &WpImageDescriptionV1,
        event: image_desc::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            image_desc::Event::Ready { .. } | image_desc::Event::Ready2 { .. } => {
                state.described = Some(Ok(()))
            }
            image_desc::Event::Failed { msg, .. } => state.described = Some(Err(msg)),
            _ => {}
        }
    }
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
            state.presented = true;
        }
    }
}

wayland_client::delegate_noop!(Client: ignore WlCompositor);
wayland_client::delegate_noop!(Client: ignore WlSurface);
wayland_client::delegate_noop!(Client: ignore WlShm);
wayland_client::delegate_noop!(Client: ignore WlShmPool);
wayland_client::delegate_noop!(Client: ignore WlBuffer);
wayland_client::delegate_noop!(Client: ignore WpColorManagerV1);
wayland_client::delegate_noop!(Client: ignore WpColorManagementSurfaceV1);
wayland_client::delegate_noop!(Client: ignore WpImageDescriptionCreatorParamsV1);

/// The 8-bit colour Mutter 51 scans out for [`CODE`] on an SDR output.
fn expected() -> [u8; 3] {
    let source = ColorState::params(Params::named(Colorspace::Bt2020, TransferFunction::Pq));
    let output = ColorState::params(color_state::output_params());
    let transform = color_state::transform(&source, &output).expect("PQ needs a transform");
    let [r, g, b] = CODE.map(|v| f32::from(v) / 255.0);
    let out = transform.apply([r, g, b, 1.0]);
    [out[0], out[1], out[2]].map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)
}

fn main() {
    let conn = Connection::connect_to_env().expect("WAYLAND_DISPLAY");
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    let registry = conn.display().get_registry(&qh, ());
    let mut client = Client::default();
    queue.roundtrip(&mut client).unwrap();
    let compositor: WlCompositor = client.bind(&registry, &qh, 6);
    let wm: XdgWmBase = client.bind(&registry, &qh, 6);
    let shm: WlShm = client.bind(&registry, &qh, 1);
    let cm: WpColorManagerV1 = client.bind(&registry, &qh, 2);

    let creator = cm.create_parametric_creator(&qh, ());
    creator.set_primaries_named(manager::Primaries::Bt2020);
    creator.set_tf_named(manager::TransferFunction::St2084Pq);
    let description = creator.create(&qh, ());

    let surface = compositor.create_surface(&qh, ());
    let cm_surface = cm.get_surface(&surface, &qh, ());
    let xdg = wm.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_app_id("tuna-color-pixel".into());
    toplevel.set_title("Colour pixel proof".into());
    toplevel.set_fullscreen(None);
    surface.commit();
    while !client.configured || client.described.is_none() {
        queue.blocking_dispatch(&mut client).unwrap();
    }
    if let Some(Err(message)) = &client.described {
        panic!("image description failed: {message}");
    }
    let (width, height) = client.size.expect("a fullscreen size");

    let stride = width * 4;
    let len = (stride * height) as usize;
    let texel = [CODE[2], CODE[1], CODE[0], 0xff];
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(&texel.repeat(len / 4)).unwrap();
    let pool = shm.create_pool(file.as_fd(), len as i32, &qh, ());
    let buffer = pool.create_buffer(0, width, height, stride, wl_shm::Format::Xrgb8888, &qh, ());
    cm_surface.set_image_description(&description, manager::RenderIntent::Perceptual);
    surface.attach(Some(&buffer), 0, 0);
    surface.damage_buffer(0, 0, width, height);
    surface.frame(&qh, ());
    surface.commit();
    while !client.presented {
        queue.blocking_dispatch(&mut client).unwrap();
    }
    let [er, eg, eb] = expected();
    println!("READY {} {} {} {er} {eg} {eb}", CODE[0], CODE[1], CODE[2]);
    std::io::stdout().flush().unwrap();
    loop {
        queue.blocking_dispatch(&mut client).unwrap();
    }
}
