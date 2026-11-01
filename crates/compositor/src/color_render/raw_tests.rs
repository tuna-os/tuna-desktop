// SPDX-License-Identifier: GPL-3.0-or-later
//! GPU shader boundaries plus opt-in real DMA-heap/EGL import qualification.
use super::tests::{draw_raw, headless_renderer, reference, SIDE};
use super::*;
use crate::color_state::{ColorState, Colorspace, Decode, Encode, Params, TransferFunction};
use smithay::backend::renderer::ImportMem;

fn assert_pixels(
    got: &[[u8; 4]],
    input: [u8; 4],
    transform: Option<&Transform>,
    straight: bool,
    limited: bool,
) {
    let want = reference(input, transform, straight, limited);
    for pixel in got {
        for channel in 0..4 {
            assert!(
                (f32::from(pixel[channel]) - want[channel]).abs() <= 2.0,
                "input {input:?}, straight={straight}, limited={limited}: {pixel:?} != {want:?}"
            );
        }
    }
}

#[test]
fn identity_raw_shader_preserves_distinct_channels_range_and_alpha() {
    let Some(mut renderer) = headless_renderer() else {
        eprintln!("raw colour GPU test skipped: no surfaceless EGL");
        return;
    };
    // A cross-channel transform also observes values outside nominal range
    // before final framebuffer clamping. No inverse driver conversion can
    // reconstruct these from clamped RGB.
    let transform = Transform {
        decode: Decode::None,
        matrix: [[0.5, 0.5, 0.0], [0.0, 0.5, 0.5], [0.5, 0.0, 0.5]],
        encode: Encode::None,
    };
    for layout in 1..=10 {
        for input in [
            [4, 245, 91, 255],
            [230, 18, 205, 255],
            [16, 235, 128, 255],
            [0, 255, 61, 255],
            [60, 25, 90, 128],
            [99, 45, 223, 0],
        ] {
            let [y, u, v, a] = input;
            let components = match layout {
                1 => vec![[y, 0, 0, 255], [u, v, 0, 255]],
                2 => vec![[y, 0, 0, 255], [v, u, 0, 255]],
                3 => vec![[y, 0, 0, 255], [u, 0, 0, 255], [v, 0, 0, 255]],
                4 => vec![[y, 0, 0, 255], [v, 0, 0, 255], [u, 0, 0, 255]],
                5 | 6 => vec![[y, u, v, a]],
                7 => vec![[y, 0, 0, 255], [0, u, 0, v]],
                8 => vec![[y, 0, 0, 255], [0, v, 0, u]],
                9 => vec![[v, 0, u, 255], [0, y, 0, 255]],
                10 => vec![[u, 0, v, 255], [0, y, 0, 255]],
                _ => unreachable!(),
            };
            let textures = components
                .iter()
                .map(|pixel| {
                    let data: Vec<u8> = (0..SIDE * SIDE).flat_map(|_| *pixel).collect();
                    renderer
                        .import_memory(&data, Fourcc::Abgr8888, (SIDE, SIDE).into(), false)
                        .unwrap()
                })
                .collect();
            let raw = RawPlanes {
                _views: Vec::new(),
                textures,
                layout: layout as f32,
            };
            let input = [y, u, v, if layout == 5 { a } else { 255 }];
            // Deliberately unrelated unit-0 pixels: sampling the converted
            // texture instead of the component views fails every case.
            let dummy = vec![[177, 3, 42, 255]; (SIDE * SIDE) as usize];
            for limited in [false, true] {
                for straight in [false, true] {
                    for transform in [None, Some(&transform)] {
                        let got = draw_raw(
                            &mut renderer,
                            &dummy,
                            transform,
                            straight,
                            limited,
                            Some(&raw),
                        );
                        assert_pixels(&got, input, transform, straight, limited);
                    }
                }
            }
        }
    }
}

#[test]
fn capture_target_is_independent_and_missing_description_uses_default_source() {
    let capture = capture_target();
    let default = ColorState::params(color_state::output_params());
    let hdr_monitor = ColorState::params(Params::named(Colorspace::Bt2020, TransferFunction::Pq));
    let icc_monitor = ColorState::new(color_state::Kind::Icc(
        color_state::Icc::parse(&crate::icc::DisplayProfile::srgb().encode().unwrap()).unwrap(),
    ));
    assert!(source_transform(None, &default, &capture).is_none());
    assert!(source_transform(None, &default, &hdr_monitor).is_some());
    for monitor in [&hdr_monitor, &icc_monitor] {
        // ICC descriptions are supported sources, not output encodings.
        let capture_transform = source_transform(Some(monitor), &default, &capture).unwrap();
        let expected = color_state::transform(monitor, &default).unwrap();
        assert_eq!(
            capture_transform.apply([0.7, 0.3, 0.1, 1.0]),
            expected.apply([0.7, 0.3, 0.1, 1.0])
        );
    }
}

// Linux uapi linux/dma-heap.h and linux/dma-buf.h. This opt-in path requires
// actual dma-buf memory, never a memfd passed off as a dma-buf.
#[repr(C)]
struct HeapAllocation {
    len: u64,
    fd: u32,
    fd_flags: u32,
    heap_flags: u64,
}

fn dma_fixture(heap: &std::fs::File, format: Fourcc, input: [u8; 4]) -> Dmabuf {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    let mut alloc = HeapAllocation {
        len: 4096,
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
    let mut sync: u64 = 2; // DMA_BUF_SYNC_START | DMA_BUF_SYNC_WRITE
    assert_eq!(
        unsafe { libc::ioctl(fd.as_raw_fd(), 0x40086200 as libc::c_ulong, &mut sync) },
        0
    );
    let mapping = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            4096,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            fd.as_raw_fd(),
            0,
        )
    };
    assert_ne!(mapping, libc::MAP_FAILED);
    let bytes = unsafe { std::slice::from_raw_parts_mut(mapping.cast::<u8>(), 4096) };
    let [y, u, v, a] = input;
    let mut planes = Vec::new();
    let mut cursor = 0usize;
    let mut plane = |samples: &[u8], width: i32, height: i32| {
        let stride = width as usize * samples.len();
        planes.push((cursor as u32, stride as u32));
        for _ in 0..height {
            for _ in 0..width {
                bytes[cursor..cursor + samples.len()].copy_from_slice(samples);
                cursor += samples.len();
            }
        }
    };
    match format {
        Fourcc::Nv12 | Fourcc::Nv21 => {
            plane(&[y], SIDE, SIDE);
            plane(
                &if format == Fourcc::Nv12 {
                    [u, v]
                } else {
                    [v, u]
                },
                SIDE / 2,
                SIDE / 2,
            );
        }
        Fourcc::Yuv420 | Fourcc::Yvu420 => {
            plane(&[y], SIDE, SIDE);
            let uv = if format == Fourcc::Yuv420 {
                [u, v]
            } else {
                [v, u]
            };
            for sample in uv {
                plane(&[sample], SIDE / 2, SIDE / 2);
            }
        }
        Fourcc::P010 | Fourcc::P012 | Fourcc::P016 | Fourcc::P210 => {
            plane(&[0, y], SIDE, SIDE);
            plane(
                &[0, u, 0, v],
                SIDE / 2,
                if format == Fourcc::P210 {
                    SIDE
                } else {
                    SIDE / 2
                },
            );
        }
        Fourcc::Ayuv => plane(&[v, u, y, a], SIDE, SIDE),
        Fourcc::Yuyv => plane(&[y, u, y, v], SIDE / 2, SIDE),
        Fourcc::Uyvy => plane(&[u, y, v, y], SIDE / 2, SIDE),
        _ => unreachable!(),
    }
    assert_eq!(unsafe { libc::munmap(mapping, 4096) }, 0);
    sync = 2 | 4; // DMA_BUF_SYNC_END | DMA_BUF_SYNC_WRITE
    assert_eq!(
        unsafe { libc::ioctl(fd.as_raw_fd(), 0x40086200 as libc::c_ulong, &mut sync) },
        0
    );
    let mut buffer = Dmabuf::builder((SIDE, SIDE), format, Modifier::Linear, DmabufFlags::empty());
    for (index, (offset, stride)) in planes.into_iter().enumerate() {
        assert!(buffer.add_plane(fd.try_clone().unwrap(), index as u32, offset, stride));
    }
    buffer.build().unwrap()
}

#[test]
fn real_dmabuf_identity_component_import_qualification() {
    let Some(heap) = std::env::var_os("TUNA_COLOR_DMABUF_HEAP") else {
        eprintln!("real dmabuf qualification skipped: set TUNA_COLOR_DMABUF_HEAP and TUNA_COLOR_DRM_RENDER_NODE");
        return;
    };
    let node =
        std::env::var_os("TUNA_COLOR_DRM_RENDER_NODE").expect("explicit DRM render node required");
    let node = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(node)
        .unwrap();
    let gbm = smithay::backend::allocator::gbm::GbmDevice::new(node).unwrap();
    let display = unsafe { smithay::backend::egl::EGLDisplay::new(gbm) }.unwrap();
    let context = smithay::backend::egl::EGLContext::new(&display).unwrap();
    let mut renderer = unsafe { GlesRenderer::new(context) }.unwrap();
    let heap = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(heap)
        .unwrap();
    for format in [
        Fourcc::Nv12,
        Fourcc::Nv21,
        Fourcc::Yuv420,
        Fourcc::Yvu420,
        Fourcc::P010,
        Fourcc::P012,
        Fourcc::P016,
        Fourcc::P210,
        Fourcc::Ayuv,
        Fourcc::Yuyv,
        Fourcc::Uyvy,
    ] {
        for input in [
            [4, 245, 91, 255],
            [230, 18, 205, 255],
            [60, 25, 90, 128],
            [99, 45, 223, 0],
        ] {
            let buffer = dma_fixture(&heap, format, input);
            let raw = raw_planes(&mut renderer, &buffer).unwrap_or_else(|error| {
                panic!("{format:?}: raw component import unavailable: {error}")
            });
            let expected = [
                input[0],
                input[1],
                input[2],
                if format == Fourcc::Ayuv {
                    input[3]
                } else {
                    255
                },
            ];
            for limited in [false, true] {
                let dummy = vec![[177, 3, 42, 255]; (SIDE * SIDE) as usize];
                let got = draw_raw(&mut renderer, &dummy, None, true, limited, Some(&raw));
                assert_pixels(&got, expected, None, true, limited);
            }
            let mut unsupported = Dmabuf::builder(
                (SIDE, SIDE),
                format,
                Modifier::Invalid,
                DmabufFlags::empty(),
            );
            use std::os::fd::AsFd;
            unsupported.add_plane(
                buffer
                    .handles()
                    .next()
                    .unwrap()
                    .as_fd()
                    .try_clone_to_owned()
                    .unwrap(),
                0,
                0,
                8,
            );
            assert!(!identity_ycbcr_supported(&unsupported.build().unwrap()));
        }
    }
}

#[test]
fn structural_admission_refuses_unknown_layouts_modifiers_and_interlacing() {
    // These descriptors are metadata-only: never imported or sampled.
    let descriptor = std::fs::File::open("/dev/null").unwrap();
    let make = |format, modifier, flags, count, width| {
        let mut buffer = Dmabuf::builder((width, SIDE), format, modifier, flags);
        for plane in 0..count {
            buffer.add_plane(descriptor.try_clone().unwrap().into(), plane, 0, 32);
        }
        buffer.build().unwrap()
    };
    assert!(identity_ycbcr_supported(&make(
        Fourcc::Nv12,
        Modifier::Linear,
        DmabufFlags::empty(),
        2,
        SIDE
    )));
    assert!(identity_ycbcr_supported(&make(
        Fourcc::P010,
        Modifier::Linear,
        DmabufFlags::empty(),
        2,
        SIDE
    )));
    assert!(!identity_ycbcr_supported(&make(
        Fourcc::Nv12,
        Modifier::Invalid,
        DmabufFlags::empty(),
        2,
        SIDE
    )));
    assert!(!identity_ycbcr_supported(&make(
        Fourcc::Nv15,
        Modifier::Linear,
        DmabufFlags::empty(),
        2,
        SIDE
    )));
    assert!(!identity_ycbcr_supported(&make(
        Fourcc::Nv12,
        Modifier::Linear,
        DmabufFlags::INTERLACED,
        2,
        SIDE
    )));
    assert!(!identity_ycbcr_supported(&make(
        Fourcc::Nv12,
        Modifier::Linear,
        DmabufFlags::empty(),
        3,
        SIDE
    )));
    assert!(!identity_ycbcr_supported(&make(
        Fourcc::Nv12,
        Modifier::Linear,
        DmabufFlags::empty(),
        2,
        SIDE - 1
    )));
}
