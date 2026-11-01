//! drm-smoke's colour pixel proof client (P-DS-03, #341): a fullscreen
//! SHM window filled with one BT.2020/PQ code value and tagged with that
//! parametric image description through wp_color_manager_v1.
//!
//! Prints one line once the tagged frame was presented to the client:
//! `READY <code r g b> <expected r g b>`, where the expected 8-bit
//! scanout colour is the Mutter 51 transform to the SDR output
//! ([`tuna_compositor::color_state`]). Then stays mapped until killed.
//!
//! After obtaining a real screenshot through the authorized shell/portal,
//! `--check-screenshot PATH` checks its centre pixel against that same SDR
//! transform and checks its actual PNG colour chunks. This only reads the
//! supplied file; it does not bypass screenshot authorization or locking.
//! `--sdr` tags the same code as BT.709/gamma 2.2; `--unmanaged`
//! creates no colour-management surface object. Both are live capture
//! controls whose expected pixels stay equal to the original code.
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    Pq,
    Sdr,
    Unmanaged,
}

/// The 8-bit capture colour for `code` in the fixed SDR target.
fn expected(source: Source, code: [u8; 3]) -> [u8; 3] {
    if source != Source::Pq {
        return code;
    }
    let source = ColorState::params(Params::named(Colorspace::Bt2020, TransferFunction::Pq));
    let output = ColorState::params(color_state::output_params());
    let transform = color_state::transform(&source, &output).expect("PQ needs a transform");
    let [r, g, b] = code.map(|v| f32::from(v) / 255.0);
    let out = transform.apply([r, g, b, 1.0]);
    [out[0], out[1], out[2]].map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)
}

fn expected_hdr(source: Source, code: [u8; 3]) -> [u32; 3] {
    let input = ColorState::params(if source == Source::Pq {
        color_state::hdr_output_params()
    } else {
        color_state::default_params()
    });
    let target = ColorState::params(color_state::hdr_output_params());
    let [r, g, b] = code.map(|v| f32::from(v) / 255.0);
    let pixel = [r, g, b, 1.0];
    let encoded =
        color_state::transform(&input, &target).map_or(pixel, |transform| transform.apply(pixel));
    [encoded[0], encoded[1], encoded[2]].map(|v| (v.clamp(0.0, 1.0) * 1023.0).round() as u32)
}

fn check_screenshot(
    path: &std::path::Path,
    source: Source,
    code: [u8; 3],
) -> Result<[u8; 3], String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let decoder = png::Decoder::new(std::io::BufReader::new(file));
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let metadata = reader.info();
    let chromaticities = png::SourceChromaticities::new(
        (0.3127, 0.3290),
        (0.6400, 0.3300),
        (0.3000, 0.6000),
        (0.1500, 0.0600),
    );
    if metadata.gama_chunk != Some(png::ScaledFloat::from_scaled(45455))
        || metadata.chrm_chunk != Some(chromaticities)
        || metadata.srgb.is_some()
        || metadata.icc_profile.is_some()
        || metadata.coding_independent_code_points.is_some()
    {
        return Err(
            "screenshot must declare SDR BT.709 / gamma 2.2, not output ICC/HDR or piecewise sRGB"
                .into(),
        );
    }
    let size = reader.output_buffer_size().ok_or("PNG is too large")?;
    let mut pixels = vec![0; size];
    let info = reader.next_frame(&mut pixels).map_err(|e| e.to_string())?;
    if info.bit_depth != png::BitDepth::Eight || info.color_type != png::ColorType::Rgba {
        return Err("screenshot must contain RGBA8 pixels".into());
    }
    let centre = (info.height as usize / 2) * info.line_size + (info.width as usize / 2) * 4;
    let got = [pixels[centre], pixels[centre + 1], pixels[centre + 2]];
    let want = expected(source, code);
    if got
        .iter()
        .zip(want)
        .any(|(&got, want)| got.abs_diff(want) > 2)
    {
        return Err(format!(
            "capture centre {got:?}, expected SDR {want:?}, source code {code:?}"
        ));
    }
    Ok(got)
}

fn main() {
    let mut args = std::env::args_os().skip(1);
    let mut source = Source::Pq;
    let mut code = CODE;
    let mut screenshot = None;
    while let Some(option) = args.next() {
        if option == "--check-screenshot" {
            screenshot = Some(args.next().expect("--check-screenshot PATH"));
        } else if option == "--sdr" {
            source = Source::Sdr;
        } else if option == "--unmanaged" {
            source = Source::Unmanaged;
        } else if option == "--highlight" {
            code = [192, 192, 192];
        } else if option == "--white" {
            code = [255, 255, 255];
        } else {
            panic!("expected --sdr, --unmanaged, --highlight, --white or --check-screenshot PATH");
        }
    }
    if let Some(path) = screenshot {
        let rgb = check_screenshot(std::path::Path::new(&path), source, code)
            .unwrap_or_else(|e| panic!("screenshot colour proof failed: {e}"));
        println!("CAPTURE-SDR {} {} {} BT709 GAMMA22", rgb[0], rgb[1], rgb[2]);
        return;
    }
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

    let description = if source == Source::Unmanaged {
        client.described = Some(Ok(()));
        None
    } else {
        let creator = cm.create_parametric_creator(&qh, ());
        let (primaries, tf) = if source == Source::Pq {
            (
                manager::Primaries::Bt2020,
                manager::TransferFunction::St2084Pq,
            )
        } else {
            (manager::Primaries::Srgb, manager::TransferFunction::Gamma22)
        };
        creator.set_primaries_named(primaries);
        creator.set_tf_named(tf);
        Some(creator.create(&qh, ()))
    };

    let surface = compositor.create_surface(&qh, ());
    let cm_surface = description
        .as_ref()
        .map(|_| cm.get_surface(&surface, &qh, ()));
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
    let texel = [code[2], code[1], code[0], 0xff];
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(&texel.repeat(len / 4)).unwrap();
    let pool = shm.create_pool(file.as_fd(), len as i32, &qh, ());
    let buffer = pool.create_buffer(0, width, height, stride, wl_shm::Format::Xrgb8888, &qh, ());
    if let (Some(cm_surface), Some(description)) = (&cm_surface, &description) {
        cm_surface.set_image_description(description, manager::RenderIntent::Perceptual);
    }
    surface.attach(Some(&buffer), 0, 0);
    surface.damage_buffer(0, 0, width, height);
    surface.frame(&qh, ());
    surface.commit();
    while !client.presented {
        queue.blocking_dispatch(&mut client).unwrap();
    }
    let [er, eg, eb] = expected(source, code);
    println!("READY {} {} {} {er} {eg} {eb}", code[0], code[1], code[2]);
    let [hr, hg, hb] = expected_hdr(source, code);
    println!("HDR-EXPECTED10 {hr} {hg} {hb}");
    std::io::stdout().flush().unwrap();
    loop {
        queue.blocking_dispatch(&mut client).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screenshot_consumer_checks_pixels_not_just_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("capture.png");
        let [r, g, b] = expected(Source::Pq, CODE);
        tuna_compositor::screenshot::save_png(&path, 1, 1, &[r, g, b, 255]).unwrap();
        assert_eq!(
            check_screenshot(&path, Source::Pq, CODE).unwrap(),
            [r, g, b]
        );

        // Correct-looking colour chunks must not pass untransformed PQ.
        let [r, g, b] = CODE;
        tuna_compositor::screenshot::save_png(&path, 1, 1, &[r, g, b, 255]).unwrap();
        assert!(check_screenshot(&path, Source::Pq, CODE)
            .unwrap_err()
            .contains("expected SDR"));
        assert_eq!(check_screenshot(&path, Source::Sdr, CODE).unwrap(), CODE);
        assert_eq!(
            check_screenshot(&path, Source::Unmanaged, CODE).unwrap(),
            CODE
        );
    }

    #[test]
    fn screenshot_consumer_rejects_untagged_pixels() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("untagged.png");
        let file = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
        let mut encoder = png::Encoder::new(file, 1, 1);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        let [r, g, b] = expected(Source::Pq, CODE);
        writer.write_image_data(&[r, g, b, 255]).unwrap();
        writer.finish().unwrap();
        assert!(check_screenshot(&path, Source::Pq, CODE)
            .unwrap_err()
            .contains("must declare SDR"));
    }

    #[test]
    fn hdr_reference_distinguishes_highlights_from_sdr_white() {
        let highlight = expected_hdr(Source::Pq, [192; 3]);
        let white = expected_hdr(Source::Sdr, [255; 3]);
        assert!(highlight
            .iter()
            .zip(white)
            .all(|(highlight, white)| *highlight > white + 100));
        assert_eq!(
            expected_hdr(Source::Sdr, CODE),
            expected_hdr(Source::Unmanaged, CODE)
        );
    }
}
