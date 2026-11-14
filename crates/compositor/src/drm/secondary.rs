//! Secondary GPU copy (Mutter's `MetaRendererNative` secondary GPUs).
//!
//! Frames render on the primary GPU only. An output whose connector
//! belongs to another KMS device (QEMU's bochs std VGA beside a virgl
//! virtio-gpu, a hybrid laptop's second GPU) renders into a texture on the
//! primary GPU, which is read back and written into that device's own
//! linear scanout buffers before its page flip: Mutter's CPU copy path
//! (`copy_shared_framebuffer_cpu`), the one that works with a KMS-only
//! device that cannot import the primary GPU's buffers.

use std::path::PathBuf;

use smithay::backend::allocator::dmabuf::{
    Dmabuf, DmabufMapping, DmabufMappingMode, DmabufSyncFlags, WeakDmabuf,
};
use smithay::backend::allocator::gbm::GbmDevice;
use smithay::backend::allocator::{Format, Fourcc, Modifier};
use smithay::backend::drm::{DrmDevice, DrmDeviceFd};
use smithay::backend::renderer::gles::{GlesError, GlesRenderer, GlesTarget, GlesTexture};
use smithay::backend::renderer::{Bind, ExportMem, Offscreen, Texture};
use smithay::reexports::drm::control::Device as ControlDevice;
use smithay::utils::{Physical, Rectangle, Size};

/// A KMS device other than the primary GPU, driving outputs whose frames
/// are copied from the primary GPU.
pub struct SecondaryGpu {
    pub path: PathBuf,
    pub drm: DrmDevice,
    pub(super) gbm: GbmDevice<DrmDeviceFd>,
    pub(super) native_device: Option<u64>,
}

/// Scanout formats of a copied output: XRGB8888 in a linear layout the
/// CPU writes row by row.
pub(super) fn copy_formats() -> smithay::backend::allocator::format::FormatSet {
    [Modifier::Linear, Modifier::Invalid]
        .into_iter()
        .map(|modifier| Format {
            code: Fourcc::Xrgb8888,
            modifier,
        })
        .collect()
}

/// Where an output's frame renders.
pub enum RenderTarget {
    /// The output's own scanout buffer on the primary GPU.
    Scanout(Dmabuf),
    /// A primary-GPU texture copied to the output's device when queued.
    Copy(GlesTexture),
}

impl RenderTarget {
    pub fn bind<'a>(
        &'a mut self,
        renderer: &mut GlesRenderer,
    ) -> Result<GlesTarget<'a>, GlesError> {
        match self {
            Self::Scanout(dmabuf) => renderer.bind(dmabuf),
            Self::Copy(texture) => renderer.bind(texture),
        }
    }
}

/// A CPU mapping of one scanout buffer of the secondary device.
enum Mapping {
    /// The buffer's dmabuf, mapped with `DMA_BUF_IOCTL_SYNC` around writes.
    Dmabuf(DmabufMapping),
    /// The GEM object mapped through `DRM_IOCTL_MODE_MAP_DUMB` (the way
    /// Mutter maps its dumb copy buffers), for drivers without dmabuf mmap.
    Dumb { ptr: *mut u8, len: usize },
}

impl Mapping {
    fn bytes(&mut self) -> &mut [u8] {
        let (ptr, len) = match self {
            Self::Dmabuf(mapping) => (mapping.ptr().cast::<u8>(), mapping.length()),
            Self::Dumb { ptr, len } => (*ptr, *len),
        };
        // SAFETY: a live shared writable mapping of `len` bytes, owned by
        // self and unmapped only on drop.
        unsafe { std::slice::from_raw_parts_mut(ptr, len) }
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        if let Self::Dumb { ptr, len } = *self {
            // SAFETY: mapped by `map_dumb` with this exact length.
            unsafe { libc::munmap(ptr.cast(), len) };
        }
    }
}

/// Map `dmabuf` (a buffer of the device `drm` exported) for writing.
fn map_buffer(drm: &impl ControlDevice, dmabuf: &Dmabuf, height: usize) -> Result<Mapping, String> {
    match dmabuf.map_plane(0, DmabufMappingMode::WRITE) {
        Ok(mapping) => return Ok(Mapping::Dmabuf(mapping)),
        Err(error) => eprintln!("tuna-compositor: drm: secondary GPU copy: dmabuf mmap: {error}; mapping the GEM object"),
    }
    let fd = dmabuf
        .handles()
        .next()
        .ok_or("scanout buffer has no plane")?;
    // The exporting device's own handle comes back: GBM keeps owning it.
    let handle = drm
        .prime_fd_to_buffer(fd)
        .map_err(|e| format!("prime import: {e}"))?;
    let offset = drm_ffi::mode::dumbbuffer::map(drm.as_fd(), u32::from(handle), 0, 0)
        .map_err(|e| format!("map dumb: {e}"))?
        .offset;
    let stride = dmabuf.strides().next().unwrap_or(0) as usize;
    let len = stride * height + dmabuf.offsets().next().unwrap_or(0) as usize;
    let offset = libc::off_t::try_from(offset).map_err(|e| e.to_string())?;
    // SAFETY: a fresh shared mapping of the GEM object's fake offset.
    let ptr = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            std::os::fd::AsRawFd::as_raw_fd(&drm.as_fd()),
            offset,
        )
    };
    if ptr == libc::MAP_FAILED {
        return Err(format!("mmap: {}", std::io::Error::last_os_error()));
    }
    Ok(Mapping::Dumb {
        ptr: ptr.cast(),
        len,
    })
}

/// Copy `height` rows of `width` tightly packed 32-bit pixels from `src`
/// into `dst` rows `dst_stride` bytes apart, swapping the red and blue
/// bytes when `swap_rb` (RGBA readback into XRGB8888 scanout). Returns
/// false, copying nothing, when either buffer is too small.
pub(crate) fn copy_rows(
    src: &[u8],
    width: usize,
    height: usize,
    swap_rb: bool,
    dst: &mut [u8],
    dst_stride: usize,
) -> bool {
    let row = width * 4;
    if height == 0 {
        return true;
    }
    if dst_stride < row || src.len() < row * height || dst.len() < dst_stride * (height - 1) + row {
        return false;
    }
    for (from, to) in src
        .chunks_exact(row)
        .take(height)
        .zip(dst.chunks_mut(dst_stride))
    {
        let to = &mut to[..row];
        if swap_rb {
            let (pixels, _) = from.as_chunks::<4>();
            let (out, _) = to.as_chunks_mut::<4>();
            for (s, d) in pixels.iter().zip(out) {
                *d = [s[2], s[1], s[0], s[3]];
            }
        } else {
            to.copy_from_slice(from);
        }
    }
    true
}

/// One output's secondary GPU copy state.
pub struct ScanoutCopy {
    /// Index of the output's device in the backend's secondary GPUs.
    pub gpu: usize,
    /// The primary-GPU texture frames render into; it keeps the previous
    /// frame, so its age is 1 after the first.
    texture: Option<GlesTexture>,
    /// GL_BGRA_EXT readback (EXT_read_format_bgra) failed: read RGBA and
    /// swap red and blue on the CPU.
    rgba_readback: bool,
    /// Mappings of the device's scanout buffers, while they live.
    mappings: Vec<(WeakDmabuf, Mapping)>,
}

impl ScanoutCopy {
    pub fn new(gpu: usize) -> Self {
        Self {
            gpu,
            texture: None,
            rgba_readback: false,
            mappings: Vec::new(),
        }
    }

    /// The texture this frame renders into, and its age.
    pub fn target(
        &mut self,
        renderer: &mut GlesRenderer,
        size: Size<i32, Physical>,
    ) -> Result<(RenderTarget, u8), String> {
        let wanted = (size.w, size.h).into();
        let reuse = self.texture.as_ref().filter(|t| t.size() == wanted);
        let (texture, age) = match reuse {
            Some(texture) => (texture.clone(), 1),
            None => {
                let texture: GlesTexture = renderer
                    .create_buffer(Fourcc::Abgr8888, wanted)
                    .map_err(|e| format!("copy texture: {e}"))?;
                self.texture = Some(texture.clone());
                (texture, 0)
            }
        };
        Ok((RenderTarget::Copy(texture), age))
    }

    /// Read `texture` back from the primary GPU into `dmabuf`, a scanout
    /// buffer of the device `drm`.
    pub fn write(
        &mut self,
        renderer: &mut GlesRenderer,
        texture: &GlesTexture,
        drm: &impl ControlDevice,
        dmabuf: &Dmabuf,
    ) -> Result<(), String> {
        let size = texture.size();
        let (width, height) = (size.w as usize, size.h as usize);
        let region = Rectangle::from_size(size);
        let mapping = match renderer.copy_texture(
            texture,
            region,
            if self.rgba_readback {
                Fourcc::Abgr8888
            } else {
                Fourcc::Argb8888
            },
        ) {
            Err(error) if !self.rgba_readback => {
                eprintln!("tuna-compositor: drm: secondary GPU copy: BGRA readback: {error}; reading RGBA");
                self.rgba_readback = true;
                renderer.copy_texture(texture, region, Fourcc::Abgr8888)
            }
            result => result,
        }
        .map_err(|e| format!("readback: {e}"))?;
        let pixels = renderer
            .map_texture(&mapping)
            .map_err(|e| format!("readback map: {e}"))?;
        self.mappings.retain(|(buffer, _)| !buffer.is_gone());
        let weak = dmabuf.weak();
        let index = match self.mappings.iter().position(|(buffer, _)| *buffer == weak) {
            Some(index) => index,
            None => {
                self.mappings.push((weak, map_buffer(drm, dmabuf, height)?));
                self.mappings.len() - 1
            }
        };
        let mapping = &mut self.mappings[index].1;
        let stride = dmabuf.strides().next().unwrap_or(0) as usize;
        let offset = dmabuf.offsets().next().unwrap_or(0) as usize;
        let dmabuf_sync = matches!(mapping, Mapping::Dmabuf(_));
        if dmabuf_sync {
            dmabuf
                .sync_plane(0, DmabufSyncFlags::START | DmabufSyncFlags::WRITE)
                .map_err(|e| format!("dmabuf sync: {e}"))?;
        }
        // A dmabuf mapping starts at the plane offset; a dumb one at 0.
        let bytes = mapping.bytes();
        let dst = if dmabuf_sync {
            &mut bytes[..]
        } else {
            bytes.get_mut(offset..).unwrap_or_default()
        };
        let copied = copy_rows(pixels, width, height, self.rgba_readback, dst, stride);
        if dmabuf_sync {
            dmabuf
                .sync_plane(0, DmabufSyncFlags::END | DmabufSyncFlags::WRITE)
                .map_err(|e| format!("dmabuf sync: {e}"))?;
        }
        if copied {
            Ok(())
        } else {
            Err(format!(
                "scanout buffer too small for {width}x{height} (stride {stride})"
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_land_at_the_scanout_stride_with_padding_untouched() {
        // 2x2 BGRA readback into a buffer with 4 bytes of row padding.
        let src = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];
        let mut dst = [0xAA; 24];
        assert!(copy_rows(&src, 2, 2, false, &mut dst, 12));
        assert_eq!(
            dst,
            [
                1, 2, 3, 4, 5, 6, 7, 8, 0xAA, 0xAA, 0xAA, 0xAA, 9, 10, 11, 12, 13, 14, 15, 16,
                0xAA, 0xAA, 0xAA, 0xAA
            ]
        );
    }

    #[test]
    fn rgba_readback_swaps_red_and_blue_for_xrgb8888() {
        // RGBA red (240,16,16) becomes XRGB8888's little-endian B,G,R,X.
        let src = [240, 16, 16, 255];
        let mut dst = [0; 4];
        assert!(copy_rows(&src, 1, 1, true, &mut dst, 4));
        assert_eq!(dst, [16, 16, 240, 255]);
    }

    #[test]
    fn short_buffers_copy_nothing() {
        let src = [7; 16];
        let mut dst = [0; 15];
        // The last row would run past the scanout buffer.
        assert!(!copy_rows(&src, 2, 2, false, &mut dst, 8));
        assert_eq!(dst, [0; 15]);
        // A stride narrower than a row.
        let mut wide = [0; 32];
        assert!(!copy_rows(&src, 2, 2, false, &mut wide, 4));
        // Too little readback.
        assert!(!copy_rows(&src[..8], 2, 2, false, &mut wide, 8));
        assert_eq!(wide, [0; 32]);
    }

    #[test]
    fn copy_formats_are_linear_xrgb8888() {
        let formats = copy_formats();
        assert!(formats.iter().all(|f| f.code == Fourcc::Xrgb8888));
        assert!(formats.contains(&Format {
            code: Fourcc::Xrgb8888,
            modifier: Modifier::Linear
        }));
    }
}
