//! Native HDR composition follows Mutter 51's separate blending/output states:
//! extended sRGB gamma 2.2 in FP16, then one full-output BT.2020/PQ encode.
//! `meta-renderer-view.c:set_color_states`, `clutter-stage-view.c` and
//! `clutter-color-state-params.c:get_blending` at tag 51.0 are the reference.

use std::sync::Arc;

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::gles::{
    ffi, GlesError, GlesFrame, GlesRenderer, GlesTexProgram, GlesTexture, Uniform,
};
use smithay::backend::renderer::{Bind, Frame, Offscreen, Renderer};
use smithay::utils::{Buffer, Physical, Rectangle, Size, Transform};

use crate::color_state::{self, ColorState};

/// A renderable FP16 target and the already compiled final encoding program.
/// Allocation and framebuffer completeness are checked before HDR admission.
pub(crate) struct HdrBuffer {
    pub(crate) texture: GlesTexture,
    pub(crate) blending: Arc<ColorState>,
    program: GlesTexProgram,
    uniforms: Vec<Uniform<'static>>,
    size: Size<i32, Physical>,
}

impl HdrBuffer {
    pub(crate) fn new(
        renderer: &mut GlesRenderer,
        size: Size<i32, Physical>,
    ) -> Result<Self, String> {
        let highp = renderer
            .with_context(|gl| unsafe {
                let mut range = [0; 2];
                let mut precision = 0;
                gl.GetShaderPrecisionFormat(
                    ffi::FRAGMENT_SHADER,
                    ffi::HIGH_FLOAT,
                    range.as_mut_ptr(),
                    &mut precision,
                );
                precision >= 16
            })
            .map_err(|error| error.to_string())?;
        if !highp {
            return Err("HDR requires high-precision fragment arithmetic".into());
        }
        let program = crate::color_render::final_output_program(renderer)
            .ok_or_else(|| "HDR output shader is unavailable".to_owned())?;
        let output = color_state::hdr_output_params();
        let blending = ColorState::params(color_state::blending_params(&output));
        let transform = color_state::transform(&blending, &ColorState::params(output))
            .ok_or_else(|| "HDR output transform is unavailable".to_owned())?;
        let uniforms = crate::color_render::final_output_uniforms(&transform);
        let mut texture = renderer
            .create_buffer(Fourcc::Abgr16161616f, (size.w, size.h).into())
            .map_err(|error| format!("FP16 output allocation: {error}"))?;
        // Allocation alone is insufficient: reject incomplete float FBOs and
        // unsupported rendering before changing connector metadata or feedback.
        {
            let mut target = renderer
                .bind(&mut texture)
                .map_err(|error| format!("FP16 output framebuffer: {error}"))?;
            let mut frame = renderer
                .render(&mut target, size, Transform::Normal)
                .map_err(|error| format!("FP16 output rendering: {error}"))?;
            frame
                .clear([0.0, 0.0, 0.0, 1.0].into(), &[Rectangle::from_size(size)])
                .map_err(|error| error.to_string())?;
            frame
                .finish()
                .map_err(|error| error.to_string())?
                .wait()
                .map_err(|_| "FP16 output probe was interrupted".to_owned())?;
        }
        Ok(Self {
            texture,
            blending,
            program,
            uniforms,
            size,
        })
    }

    pub(crate) fn fits(&self, size: Size<i32, Physical>) -> bool {
        self.size == size
    }

    /// Validate the actual negotiated GBM target, not just an offscreen
    /// texture with the same nominal bit depth. No frame is submitted.
    pub(crate) fn probe_scanout(
        &self,
        renderer: &mut GlesRenderer,
        dmabuf: &mut smithay::backend::allocator::dmabuf::Dmabuf,
    ) -> Result<(), String> {
        let mut target = renderer.bind(dmabuf).map_err(|error| error.to_string())?;
        let mut frame = renderer
            .render(&mut target, self.size, Transform::Normal)
            .map_err(|error| error.to_string())?;
        self.present(&mut frame)
            .map_err(|error| error.to_string())?;
        frame
            .finish()
            .map_err(|error| error.to_string())?
            .wait()
            .map_err(|_| "HDR scanout probe was interrupted".to_owned())?;
        Ok(())
    }

    /// The source texture is already the fully blended scene (including the
    /// cursor, shell, effects and wallpaper), never a per-surface PQ blend.
    pub(crate) fn present(&self, frame: &mut GlesFrame<'_, '_>) -> Result<(), GlesError> {
        let output = Rectangle::from_size(self.size);
        let source = Rectangle::<f64, Buffer>::from_size(
            (f64::from(self.size.w), f64::from(self.size.h)).into(),
        );
        frame.render_texture_from_to(
            &self.texture,
            source,
            output,
            &[output],
            &[],
            Transform::Normal,
            1.0,
            Some(&self.program),
            &self.uniforms,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::backend::renderer::{Color32F, ExportMem, ImportMem};

    fn unavailable(reason: &str) {
        assert!(
            std::env::var_os("TUNA_REQUIRE_COLOR_GL").is_none(),
            "required native colour GPU qualification unavailable: {reason}",
        );
        eprintln!("native colour GPU test skipped: {reason}");
    }

    fn draw_sources(
        renderer: &mut GlesRenderer,
        target: &mut GlesTexture,
        opaque: &GlesTexture,
        alpha: &GlesTexture,
        transform: &color_state::Transform,
    ) {
        let program = crate::color_render::final_output_program(renderer).unwrap();
        let uniforms = crate::color_render::final_output_uniforms(transform);
        let mut bound = renderer.bind(target).unwrap();
        let mut frame = renderer
            .render(&mut bound, (3, 1).into(), Transform::Normal)
            .unwrap();
        frame
            .clear(
                Color32F::new(1.0, 1.0, 1.0, 1.0),
                &[Rectangle::from_size((3, 1).into())],
            )
            .unwrap();
        for (x, texture) in [(0, opaque), (1, alpha)] {
            let destination = Rectangle::new((x, 0).into(), (1, 1).into());
            frame
                .render_texture_from_to(
                    texture,
                    Rectangle::<f64, Buffer>::from_size((1.0, 1.0).into()),
                    destination,
                    &[Rectangle::from_size((1, 1).into())],
                    &[],
                    Transform::Normal,
                    1.0,
                    Some(&program),
                    &uniforms,
                )
                .unwrap();
        }
        frame.finish().unwrap().wait().unwrap();
    }

    /// Actual EGL shader consumers, FP16 alpha blending, ten-bit output
    /// framebuffer and packed pixel readback. The strict native CI lane sets
    /// TUNA_REQUIRE_COLOR_GL so missing driver support cannot silently pass.
    #[test]
    fn fp16_blend_then_ten_bit_pq_preserves_hdr_and_sdr_reference_white() {
        use smithay::backend::egl::native::EGLSurfacelessDisplay;
        use smithay::backend::egl::{EGLContext, EGLDisplay};
        let renderer = (|| {
            let display = unsafe { EGLDisplay::new(EGLSurfacelessDisplay) }.ok()?;
            let context = EGLContext::new(&display).ok()?;
            unsafe { GlesRenderer::new(context) }.ok()
        })();
        let Some(mut renderer) = renderer else {
            unavailable("surfaceless EGL renderer");
            return;
        };
        let mut hdr = match HdrBuffer::new(&mut renderer, (3, 1).into()) {
            Ok(buffer) => buffer,
            Err(error) => {
                unavailable(&error);
                return;
            }
        };
        let mut encoded: GlesTexture =
            match renderer.create_buffer(Fourcc::Abgr2101010, (3, 1).into()) {
                Ok(texture) => texture,
                Err(error) => {
                    unavailable(&error.to_string());
                    return;
                }
            };
        let opaque = renderer
            .import_memory(
                &[192, 192, 192, 255],
                Fourcc::Abgr8888,
                (1, 1).into(),
                false,
            )
            .unwrap();
        let alpha = renderer
            .import_memory(&[96, 96, 96, 128], Fourcc::Abgr8888, (1, 1).into(), false)
            .unwrap();
        let source = ColorState::params(color_state::hdr_output_params());
        let to_blending = color_state::transform(&source, &hdr.blending).unwrap();
        draw_sources(
            &mut renderer,
            &mut hdr.texture,
            &opaque,
            &alpha,
            &to_blending,
        );
        // The magnifier's identity-copy route uses Smithay's default shader,
        // not the colour override. Its sampler must preserve FP16 values >1.
        let mut copied: GlesTexture = renderer
            .create_buffer(Fourcc::Abgr16161616f, (3, 1).into())
            .unwrap();
        {
            let mut bound = renderer.bind(&mut copied).unwrap();
            let mut frame = renderer
                .render(&mut bound, (3, 1).into(), Transform::Normal)
                .unwrap();
            let whole = Rectangle::from_size((3, 1).into());
            frame
                .render_texture_from_to(
                    &hdr.texture,
                    Rectangle::<f64, Buffer>::from_size((3.0, 1.0).into()),
                    whole,
                    &[whole],
                    &[],
                    Transform::Normal,
                    1.0,
                    None,
                    &[],
                )
                .unwrap();
            frame.finish().unwrap().wait().unwrap();
        }
        hdr.texture = copied;
        let mut bound = renderer.bind(&mut encoded).unwrap();
        {
            let mut frame = renderer
                .render(&mut bound, (3, 1).into(), Transform::Normal)
                .unwrap();
            hdr.present(&mut frame).unwrap();
            frame.finish().unwrap().wait().unwrap();
        }
        let mapping = renderer
            .copy_framebuffer(
                &bound,
                Rectangle::from_size((3, 1).into()),
                Fourcc::Abgr2101010,
            )
            .unwrap();
        let bytes = renderer.map_texture(&mapping).unwrap();
        let pixels: Vec<[f32; 3]> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .take(3)
            .map(|bytes| {
                let packed = u32::from_ne_bytes(*bytes);
                [packed & 1023, (packed >> 10) & 1023, (packed >> 20) & 1023]
                    .map(|v| v as f32 / 1023.0)
            })
            .collect();
        let bright = to_blending.apply([192.0 / 255.0, 192.0 / 255.0, 192.0 / 255.0, 1.0]);
        let half = to_blending.apply([96.0 / 255.0, 96.0 / 255.0, 96.0 / 255.0, 128.0 / 255.0]);
        let blended = [
            half[0] + 1.0 - half[3],
            half[1] + 1.0 - half[3],
            half[2] + 1.0 - half[3],
            1.0,
        ];
        let output = ColorState::params(color_state::hdr_output_params());
        let encode = color_state::transform(&hdr.blending, &output).unwrap();
        let decode = color_state::transform(&output, &hdr.blending).unwrap();
        for (pixel, expected) in pixels.iter().zip([bright, blended, [1.0; 4]]) {
            let want = encode.apply(expected);
            for channel in 0..3 {
                assert!(
                    (pixel[channel] - want[channel]).abs() <= 3.0 / 1023.0,
                    "{pixel:?} != {want:?}"
                );
            }
            let decoded = decode.apply([pixel[0], pixel[1], pixel[2], 1.0]);
            assert!((decoded[0] - expected[0]).abs() < 0.06);
        }
        assert!(pixels[0][0] > pixels[1][0] && pixels[1][0] > pixels[2][0]);
        assert!(
            bright[0] > 2.0,
            "HDR source must really exceed SDR blending range"
        );
        eprintln!("NATIVE_HDR_FP16_10BIT actual_pq_rgb={pixels:?} blending_opaque={bright:?} blending_alpha={blended:?}");
        drop(bound);
        // The same HDR sources drawn to the fixed SDR target clip, while
        // reference white remains white. This is a real framebuffer control,
        // not a transform-only numerical assertion.
        let sdr = ColorState::params(color_state::output_params());
        let to_sdr = color_state::transform(&source, &sdr).unwrap();
        let mut capture: GlesTexture = renderer
            .create_buffer(Fourcc::Abgr8888, (3, 1).into())
            .unwrap();
        draw_sources(&mut renderer, &mut capture, &opaque, &alpha, &to_sdr);
        let bound = renderer.bind(&mut capture).unwrap();
        let mapping = renderer
            .copy_framebuffer(
                &bound,
                Rectangle::from_size((3, 1).into()),
                Fourcc::Abgr8888,
            )
            .unwrap();
        let bytes = renderer.map_texture(&mapping).unwrap();
        assert!(bytes[..12]
            .as_chunks::<4>()
            .0
            .iter()
            .all(|pixel| pixel[..3].iter().all(|channel| *channel >= 253)));
    }
}
