// SPDX-License-Identifier: GPL-3.0-or-later
//! Real dma-buf YUV pixel producer for native scanout/capture qualification.
//!
//! Requires TUNA_COLOR_DMABUF_HEAP (for example /dev/dma_heap/system) and a
//! compositor advertising the chosen YUV format with the explicit LINEAR
//! modifier. No SHM/memfd fallback exists. The client writes its own fixture
//! allocation with DMA_BUF_IOCTL_SYNC; compositor sampling stays zero-copy.
//!
//! --format nv12|p010|ayuv (or TUNA_COLOR_YUV_FORMAT),
//! --coeff identity-full|identity-limited|bt601-full|bt601-limited|
//!         bt709-full|bt709-limited|bt2020-full|bt2020-limited|unset
//!         (or TUNA_COLOR_YUV_COEFFICIENTS), --code Y,U,V.
//! Default: NV12 identity-full, code 48,201,91. Prints the existing
//! READY code_y code_u code_v expected_r expected_g expected_b contract and
//! remains mapped. Expected RGB is independently derived from the declared
//! coefficients/range, not read back from the producer or compositor.
use smithay::backend::allocator::Fourcc;
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use wayland_client::protocol::{
    wl_buffer::WlBuffer,
    wl_callback::{self, WlCallback},
    wl_compositor::WlCompositor,
    wl_registry::{self, WlRegistry},
    wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::wp::color_representation::v1::client::{
    wp_color_representation_manager_v1::WpColorRepresentationManagerV1,
    wp_color_representation_surface_v1::{self as repr, WpColorRepresentationSurfaceV1},
};
use wayland_protocols::wp::linux_dmabuf::zv1::client::{
    zwp_linux_buffer_params_v1::{self as buffer_params, ZwpLinuxBufferParamsV1},
    zwp_linux_dmabuf_v1::{self as linux_dmabuf, ZwpLinuxDmabufV1},
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::{self, XdgToplevel},
    xdg_wm_base::{self, XdgWmBase},
};

#[derive(Clone, Copy, Debug)]
enum Coefficients {
    Identity,
    Bt601,
    Bt709,
    Bt2020,
    Unset,
}

struct Options {
    format: Fourcc,
    coefficients: Coefficients,
    limited: bool,
    code: [u8; 3],
}
impl Options {
    fn parse() -> Self {
        let mut format = std::env::var("TUNA_COLOR_YUV_FORMAT").unwrap_or_else(|_| "nv12".into());
        let mut coefficients =
            std::env::var("TUNA_COLOR_YUV_COEFFICIENTS").unwrap_or_else(|_| "identity-full".into());
        let mut code = [48, 201, 91];
        let mut args = std::env::args().skip(1);
        while let Some(option) = args.next() {
            let value = args.next().expect("each option requires a value");
            match option.as_str() {
                "--format" => format = value,
                "--coeff" => coefficients = value,
                "--code" => {
                    let channels: Vec<u8> = value
                        .split(',')
                        .map(|s| s.parse().expect("8-bit Y,U,V"))
                        .collect();
                    code = channels.try_into().expect("exactly three Y,U,V channels");
                }
                _ => panic!("unknown option {option}"),
            }
        }
        let format = match format.as_str() {
            "nv12" => Fourcc::Nv12,
            "p010" => Fourcc::P010,
            "ayuv" => Fourcc::Ayuv,
            _ => panic!("unsupported fixture format {format}; use nv12, p010 or ayuv"),
        };
        let (coefficients, limited) = match coefficients.as_str() {
            "identity-full" => (Coefficients::Identity, false),
            "identity-limited" => (Coefficients::Identity, true),
            "bt601-full" => (Coefficients::Bt601, false),
            "bt601-limited" => (Coefficients::Bt601, true),
            "bt709-full" => (Coefficients::Bt709, false),
            "bt709-limited" => (Coefficients::Bt709, true),
            "bt2020-full" => (Coefficients::Bt2020, false),
            "bt2020-limited" => (Coefficients::Bt2020, true),
            "unset" => (Coefficients::Unset, true),
            _ => panic!("unsupported coefficients/range {coefficients}"),
        };
        Self {
            format,
            coefficients,
            limited,
            code,
        }
    }

    fn expected(&self) -> [u8; 3] {
        let [y, u, v] = self.code.map(f64::from);
        let rgb = if matches!(self.coefficients, Coefficients::Identity) {
            // P010's stored top 10 bits are imported as normalized R16/GR1616
            // raw views, matching the production path and Mutter's layout.
            let scale = if self.format == Fourcc::P010 {
                256.0 / 65535.0
            } else {
                1.0 / 255.0
            };
            [y, u, v].map(|c| {
                if self.limited {
                    (c * scale - 16.0 / 255.0) * 255.0 / 219.0
                } else {
                    c * scale
                }
            })
        } else {
            let (kr, kb) = match self.coefficients {
                Coefficients::Bt601 => (0.299, 0.114),
                Coefficients::Bt709 | Coefficients::Unset => (0.2126, 0.0722),
                Coefficients::Bt2020 => (0.2627, 0.0593),
                Coefficients::Identity => unreachable!(),
            };
            let bits = if self.format == Fourcc::P010 {
                4.0
            } else {
                1.0
            };
            let (y, cb, cr) = if self.limited {
                ((y - 16.0) / 219.0, (u - 128.0) / 224.0, (v - 128.0) / 224.0)
            } else {
                let max = if self.format == Fourcc::P010 {
                    1023.0
                } else {
                    255.0
                };
                (
                    y * bits / max,
                    (u - 128.0) * bits / max,
                    (v - 128.0) * bits / max,
                )
            };
            let r = y + (2.0 - 2.0 * kr) * cr;
            let b = y + (2.0 - 2.0 * kb) * cb;
            let g = (y - kr * r - kb * b) / (1.0 - kr - kb);
            [r, g, b]
        };
        rgb.map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8)
    }
}

#[derive(Default)]
struct Client {
    globals: HashMap<String, (u32, u32)>,
    formats: HashSet<(u32, u64)>,
    size: Option<(i32, i32)>,
    configured: bool,
    presented: bool,
}
impl Client {
    fn bind<I: Proxy + 'static>(
        &self,
        registry: &WlRegistry,
        qh: &QueueHandle<Self>,
        version: u32,
    ) -> I
    where
        Self: Dispatch<I, ()>,
    {
        let (name, offered) = self
            .globals
            .get(I::interface().name)
            .copied()
            .unwrap_or_else(|| panic!("required protocol {} unavailable", I::interface().name));
        assert!(
            offered >= version,
            "{} version {version} required",
            I::interface().name
        );
        registry.bind(name, version, qh, ())
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
impl Dispatch<ZwpLinuxDmabufV1, ()> for Client {
    fn event(
        state: &mut Self,
        _: &ZwpLinuxDmabufV1,
        event: linux_dmabuf::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let linux_dmabuf::Event::Modifier {
            format,
            modifier_hi,
            modifier_lo,
        } = event
        {
            state.formats.insert((
                format,
                (u64::from(modifier_hi) << 32) | u64::from(modifier_lo),
            ));
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
macro_rules! no_events {
    ($($proxy:ty),* $(,)?) => {$(impl Dispatch<$proxy, ()> for Client {
        fn event(_: &mut Self, _: &$proxy, _: <$proxy as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
    })*};
}
no_events!(
    WlCompositor,
    WlSurface,
    WlBuffer,
    ZwpLinuxBufferParamsV1,
    WpColorRepresentationManagerV1,
    WpColorRepresentationSurfaceV1
);

#[repr(C)]
struct HeapAllocation {
    len: u64,
    fd: u32,
    fd_flags: u32,
    heap_flags: u64,
}

/// The producer initializes its own real allocation. This is deliberately
/// not a compositor CPU conversion or a fake dma-buf backed by a memfd.
fn allocation(options: &Options, width: i32, height: i32) -> (OwnedFd, Vec<(u32, u32)>) {
    assert!(
        width > 0 && height > 0 && width % 2 == 0 && height % 2 == 0,
        "native proof requires a positive even fullscreen size"
    );
    let heap = std::env::var_os("TUNA_COLOR_DMABUF_HEAP")
        .expect("TUNA_COLOR_DMABUF_HEAP must name a real accessible DMA heap");
    let heap = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(heap)
        .expect("open DMA heap");
    let bpc = if options.format == Fourcc::P010 {
        2usize
    } else {
        1
    };
    let stride = width as usize * bpc;
    let len = if options.format == Fourcc::Ayuv {
        width as usize * height as usize * 4
    } else {
        stride * height as usize * 3 / 2
    };
    let len = len.checked_add(4095).expect("buffer size overflow") & !4095;
    let mut alloc = HeapAllocation {
        len: len as u64,
        fd: 0,
        fd_flags: (libc::O_RDWR | libc::O_CLOEXEC) as u32,
        heap_flags: 0,
    };
    assert_eq!(
        unsafe { libc::ioctl(heap.as_raw_fd(), 0xc0184800 as libc::c_ulong, &mut alloc) },
        0,
        "DMA_HEAP_IOCTL_ALLOC: {}",
        std::io::Error::last_os_error()
    );
    let fd = unsafe { OwnedFd::from_raw_fd(alloc.fd as i32) };
    let mut sync: u64 = 2;
    assert_eq!(
        unsafe { libc::ioctl(fd.as_raw_fd(), 0x40086200 as libc::c_ulong, &mut sync) },
        0,
        "DMA_BUF_IOCTL_SYNC start: {}",
        std::io::Error::last_os_error()
    );
    let mapping = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            fd.as_raw_fd(),
            0,
        )
    };
    assert_ne!(
        mapping,
        libc::MAP_FAILED,
        "mmap DMA buffer: {}",
        std::io::Error::last_os_error()
    );
    let bytes = unsafe { std::slice::from_raw_parts_mut(mapping.cast::<u8>(), len) };
    let [y, u, v] = options.code;
    let planes = if options.format == Fourcc::Ayuv {
        for pixel in bytes[..width as usize * height as usize * 4]
            .as_chunks_mut::<4>()
            .0
        {
            pixel.copy_from_slice(&[v, u, y, 255]);
        }
        vec![(0, width as u32 * 4)]
    } else {
        let offset = stride * height as usize;
        if options.format == Fourcc::P010 {
            for sample in bytes[..offset].as_chunks_mut::<2>().0 {
                sample.copy_from_slice(&[0, y]);
            }
            for pair in bytes[offset..offset + stride * height as usize / 2]
                .as_chunks_mut::<4>()
                .0
            {
                pair.copy_from_slice(&[0, u, 0, v]);
            }
        } else {
            bytes[..offset].fill(y);
            for pair in bytes[offset..offset + stride * height as usize / 2]
                .as_chunks_mut::<2>()
                .0
            {
                pair.copy_from_slice(&[u, v]);
            }
        }
        vec![(0, stride as u32), (offset as u32, stride as u32)]
    };
    assert_eq!(unsafe { libc::munmap(mapping, len) }, 0);
    sync = 2 | 4;
    assert_eq!(
        unsafe { libc::ioctl(fd.as_raw_fd(), 0x40086200 as libc::c_ulong, &mut sync) },
        0,
        "DMA_BUF_IOCTL_SYNC end: {}",
        std::io::Error::last_os_error()
    );
    (fd, planes)
}

fn main() {
    let options = Options::parse();
    let connection = Connection::connect_to_env().expect("WAYLAND_DISPLAY");
    let mut queue = connection.new_event_queue();
    let qh = queue.handle();
    let registry = connection.display().get_registry(&qh, ());
    let mut client = Client::default();
    queue.roundtrip(&mut client).unwrap();
    let compositor: WlCompositor = client.bind(&registry, &qh, 6);
    let wm: XdgWmBase = client.bind(&registry, &qh, 6);
    let dma: ZwpLinuxDmabufV1 = client.bind(&registry, &qh, 3);
    let representation: WpColorRepresentationManagerV1 = client.bind(&registry, &qh, 1);
    queue.roundtrip(&mut client).unwrap();
    assert!(
        client.formats.contains(&(options.format as u32, 0)),
        "compositor does not advertise {:?} with explicit LINEAR modifier; no fallback",
        options.format
    );
    let surface = compositor.create_surface(&qh, ());
    let representation_surface = representation.get_surface(&surface, &qh, ());
    if !matches!(options.coefficients, Coefficients::Unset) {
        let coefficients = match options.coefficients {
            Coefficients::Identity => repr::Coefficients::Identity,
            Coefficients::Bt601 => repr::Coefficients::Bt601,
            Coefficients::Bt709 => repr::Coefficients::Bt709,
            Coefficients::Bt2020 => repr::Coefficients::Bt2020,
            Coefficients::Unset => unreachable!(),
        };
        representation_surface.set_coefficients_and_range(
            coefficients,
            if options.limited {
                repr::Range::Limited
            } else {
                repr::Range::Full
            },
        );
    }
    let xdg = wm.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_app_id("tuna-color-yuv".into());
    toplevel.set_title("Native YUV colour pixel proof".into());
    toplevel.set_fullscreen(None);
    surface.commit();
    while !client.configured {
        queue.blocking_dispatch(&mut client).unwrap();
    }
    let (width, height) = client.size.expect("fullscreen size");
    let (_allocation, planes) = allocation(&options, width, height);
    let params = dma.create_params(&qh, ());
    for (index, (offset, stride)) in planes.into_iter().enumerate() {
        params.add(_allocation.as_fd(), index as u32, offset, stride, 0, 0);
    }
    let buffer = params.create_immed(
        width,
        height,
        options.format as u32,
        buffer_params::Flags::empty(),
        &qh,
        (),
    );
    params.destroy();
    surface.attach(Some(&buffer), 0, 0);
    surface.damage_buffer(0, 0, width, height);
    surface.frame(&qh, ());
    surface.commit();
    while !client.presented {
        queue.blocking_dispatch(&mut client).unwrap();
    }
    let [r, g, b] = options.expected();
    let [y, u, v] = options.code;
    println!("READY {y} {u} {v} {r} {g} {b}");
    std::io::stdout().flush().unwrap();
    loop {
        queue.blocking_dispatch(&mut client).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn expectations_distinguish_identity_matrices_and_default() {
        let options = |coefficients, limited| Options {
            format: Fourcc::Nv12,
            coefficients,
            limited,
            code: [48, 201, 91],
        };
        assert_eq!(
            options(Coefficients::Identity, false).expected(),
            [48, 201, 91]
        );
        assert_ne!(
            options(Coefficients::Identity, true).expected(),
            [48, 201, 91]
        );
        assert_eq!(
            options(Coefficients::Unset, true).expected(),
            options(Coefficients::Bt709, true).expected()
        );
        for coefficient in [
            Coefficients::Bt601,
            Coefficients::Bt709,
            Coefficients::Bt2020,
        ] {
            for limited in [false, true] {
                assert_ne!(options(coefficient, limited).expected(), [48, 201, 91]);
            }
        }
    }
}
