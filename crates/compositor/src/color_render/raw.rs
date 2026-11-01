// SPDX-License-Identifier: GPL-3.0-or-later
// Layout reference: Mutter 51.0, Copyright (C) 2018 Barco NV;
// authored by Niels De Graef, GPL-2.0-or-later (GPL-3.0-or-later selected).
//! Zero-copy DRM component views for identity YCbCr; no driver YUV conversion.
use super::*;

#[derive(Debug)]
pub(super) struct RawPlanes {
    pub(super) _views: Vec<Dmabuf>,
    pub(super) textures: Vec<GlesTexture>,
    pub(super) layout: f32,
}

impl RawPlanes {
    pub(super) fn with_bindings<'frame, 'buffer, R>(
        &self,
        frame: &mut GlesFrame<'frame, 'buffer>,
        draw: impl FnOnce(&mut GlesFrame<'frame, 'buffer>) -> R,
    ) -> R {
        // Do not pad with repeated textures: recursively taking an RwLock
        // read lock could deadlock if a shared-context writer were waiting.
        match self.textures.as_slice() {
            [a] => frame.with_texture_bindings(&[(2, a)], draw),
            [a, b] => frame.with_texture_bindings(&[(2, a), (3, b)], draw),
            [a, b, c] => frame.with_texture_bindings(&[(2, a), (3, b), (4, c)], draw),
            _ => unreachable!("raw layouts have one to three component views"),
        }
    }
}

#[derive(Default)]
struct RawCache(RefCell<HashMap<WeakDmabuf, Rc<RawPlanes>>>);

pub(super) fn prune(renderer: &GlesRenderer) {
    if let Some(cache) = renderer.egl_context().user_data().get::<RawCache>() {
        cache
            .0
            .borrow_mut()
            .retain(|weak, _| weak.upgrade().is_some());
    }
}
// (source plane, RGB-view fourcc, horizontal divisor, vertical divisor).
// Independent DRM layout implementation; reference Mutter 51.0
// meta-multi-texture-format.c:346-608 and meta-cogl-drm-formats.h:72-75.
// DRM GR88, not RG88, samples little-endian bytes as .rg.
type PlaneView = (usize, Fourcc, i32, i32);
fn raw_layout(format: Fourcc) -> Option<(f32, &'static [PlaneView], usize)> {
    use Fourcc::*;
    Some(match format {
        Nv12 => (1.0, &[(0, R8, 1, 1), (1, Gr88, 2, 2)], 2),
        Nv21 => (2.0, &[(0, R8, 1, 1), (1, Gr88, 2, 2)], 2),
        Nv16 => (1.0, &[(0, R8, 1, 1), (1, Gr88, 2, 1)], 2),
        Nv61 => (2.0, &[(0, R8, 1, 1), (1, Gr88, 2, 1)], 2),
        Nv24 => (1.0, &[(0, R8, 1, 1), (1, Gr88, 1, 1)], 2),
        Nv42 => (2.0, &[(0, R8, 1, 1), (1, Gr88, 1, 1)], 2),
        P010 | P012 | P016 => (1.0, &[(0, R16, 1, 1), (1, Gr1616, 2, 2)], 2),
        P210 => (1.0, &[(0, R16, 1, 1), (1, Gr1616, 2, 1)], 2),
        Yuv420 => (3.0, &[(0, R8, 1, 1), (1, R8, 2, 2), (2, R8, 2, 2)], 3),
        Yvu420 => (4.0, &[(0, R8, 1, 1), (1, R8, 2, 2), (2, R8, 2, 2)], 3),
        Yuv422 => (3.0, &[(0, R8, 1, 1), (1, R8, 2, 1), (2, R8, 2, 1)], 3),
        Yvu422 => (4.0, &[(0, R8, 1, 1), (1, R8, 2, 1), (2, R8, 2, 1)], 3),
        Yuv444 => (3.0, &[(0, R8, 1, 1), (1, R8, 1, 1), (2, R8, 1, 1)], 3),
        Yvu444 => (4.0, &[(0, R8, 1, 1), (1, R8, 1, 1), (2, R8, 1, 1)], 3),
        Yuv410 => (3.0, &[(0, R8, 1, 1), (1, R8, 4, 4), (2, R8, 4, 4)], 3),
        Yvu410 => (4.0, &[(0, R8, 1, 1), (1, R8, 4, 4), (2, R8, 4, 4)], 3),
        Yuv411 => (3.0, &[(0, R8, 1, 1), (1, R8, 4, 1), (2, R8, 4, 1)], 3),
        Yvu411 => (4.0, &[(0, R8, 1, 1), (1, R8, 4, 1), (2, R8, 4, 1)], 3),
        Ayuv => (5.0, &[(0, Argb8888, 1, 1)], 1),
        Xyuv8888 => (6.0, &[(0, Argb8888, 1, 1)], 1),
        Yuyv => (7.0, &[(0, Gr88, 1, 1), (0, Argb8888, 2, 1)], 1),
        Yvyu => (8.0, &[(0, Gr88, 1, 1), (0, Argb8888, 2, 1)], 1),
        Uyvy => (9.0, &[(0, Argb8888, 2, 1), (0, Gr88, 1, 1)], 1),
        Vyuy => (10.0, &[(0, Argb8888, 2, 1), (0, Gr88, 1, 1)], 1),
        _ => return None,
    })
}

/// Structural admission; EGL must additionally support each non-external view.
/// Opaque/implicit modifiers cannot safely be reinterpreted: auxiliary planes
/// and addressing belong to the original pixel format.
pub(crate) fn identity_ycbcr_supported(dmabuf: &Dmabuf) -> bool {
    if !cfg!(target_endian = "little")
        || dmabuf.format().modifier != Modifier::Linear
        || dmabuf.flags().contains(DmabufFlags::INTERLACED)
    {
        return false;
    }
    let Some((_, views, count)) = raw_layout(dmabuf.format().code) else {
        return false;
    };
    dmabuf.num_planes() == count
        && dmabuf.size().w > 0
        && dmabuf.size().h > 0
        && views
            .iter()
            .all(|(_, _, h, v)| dmabuf.size().w % h == 0 && dmabuf.size().h % v == 0)
}

pub(super) fn raw_planes(
    renderer: &mut GlesRenderer,
    dmabuf: &Dmabuf,
) -> Result<Rc<RawPlanes>, GlesError> {
    if !identity_ycbcr_supported(dmabuf) {
        return Err(GlesError::UnsupportedPixelLayout);
    }
    renderer
        .egl_context()
        .user_data()
        .insert_if_missing(RawCache::default);
    {
        let cache = renderer
            .egl_context()
            .user_data()
            .get::<RawCache>()
            .unwrap();
        let mut cache = cache.0.borrow_mut();
        cache.retain(|weak, _| weak.upgrade().is_some());
        if let Some(raw) = cache.get(&dmabuf.weak()) {
            return Ok(raw.clone());
        }
    }
    let (layout, planes, _) = raw_layout(dmabuf.format().code).unwrap();
    let mut views = Vec::with_capacity(planes.len());
    let mut textures = Vec::with_capacity(planes.len());
    for &(index, format, hsub, vsub) in planes {
        let mut view = Dmabuf::builder(
            (dmabuf.size().w / hsub, dmabuf.size().h / vsub),
            format,
            Modifier::Linear,
            dmabuf.flags(),
        );
        let fd = dmabuf
            .handles()
            .nth(index)
            .unwrap()
            .try_clone_to_owned()
            .map_err(|_| GlesError::UnsupportedPixelLayout)?;
        view.add_plane(
            fd,
            0,
            dmabuf.offsets().nth(index).unwrap(),
            dmabuf.strides().nth(index).unwrap(),
        );
        let view = view.build().unwrap();
        textures.push(renderer.import_dmabuf_raw(&view)?);
        views.push(view);
    }
    let raw = Rc::new(RawPlanes {
        _views: views,
        textures,
        layout,
    });
    renderer
        .egl_context()
        .user_data()
        .get::<RawCache>()
        .unwrap()
        .0
        .borrow_mut()
        .insert(dmabuf.weak(), raw.clone());
    Ok(raw)
}
