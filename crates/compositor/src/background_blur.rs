//! The blur behind ext-background-effect surfaces
//! ([`crate::background_effect`]), drawn the way Mutter 51 does:
//!
//! - `meta_background_effect_paint_blur_region` (meta-background-effect.c)
//!   paints, right before the surface's own content, the framebuffer as
//!   composited so far (everything beneath the surface) blurred, within
//!   the surface's blur region (clipped to the surface), at the surface's
//!   paint opacity. The sample reaches `ceil(2 × radius)` pixels past the
//!   region (`calculate_blur_sample_padding`), clamped to the output.
//! - The blur is clutter-blur.c's: a separable gaussian of sigma
//!   `radius / 2` (vertical pass, then horizontal), on a copy downscaled by
//!   powers of two while sigma exceeds 6 and both sides exceed 256 px
//!   (`clutter_blur_calculate_downscale_factor`), sampling with linear
//!   filtering at `1.5 × sigma` paired taps.
//! - The result goes through `add_blur_framebuffer_effect`
//!   (clutter-paint-nodes.c): saturation 1.25 and 0.015 grain, Mutter's
//!   `DEFAULT_BACKGROUND_BLUR_*` (compositor.c), radius 24 logical pixels
//!   times the surface's scale on the output.
//! - Damage follows `meta_background_blur_expand_redraw_clip`: a repaint
//!   touching a blur's sample area repaints all of it
//!   ([`expand_damage`]), so the blur only ever samples this frame's
//!   pixels, and the scene redraws beneath the surface inside it
//!   (`Occlusion::uncover`; Mutter likewise drops the sample area from the
//!   surface's opaque region).
//!
//! Nothing here runs unless a surface has a committed blur region.

use std::sync::{Arc, Mutex};

use smithay::backend::renderer::gles::{ffi, Capability, GlesError, GlesFrame};
use smithay::utils::{Physical, Rectangle};

pub(crate) type PhysicalRect = Rectangle<i32, Physical>;

/// `DEFAULT_BACKGROUND_BLUR_RADIUS` (compositor.c), logical pixels.
pub const BLUR_RADIUS: f64 = 24.0;
/// `DEFAULT_BACKGROUND_BLUR_SATURATION`.
pub const BLUR_SATURATION: f32 = 1.25;
/// `DEFAULT_BACKGROUND_BLUR_NOISE`.
pub const BLUR_NOISE: f32 = 0.015;
/// clutter-blur.c's `MAX_SIGMA` and `MIN_DOWNSCALE_SIZE`.
const MAX_SIGMA: f32 = 6.0;
const MIN_DOWNSCALE_SIZE: f32 = 256.0;

/// One blurred surface of a frame's scene.
#[derive(Debug, Clone)]
pub(crate) struct Behind {
    /// The scene element (front to back) the blur is drawn under.
    pub index: usize,
    /// The blur region in frame pixels, within the surface.
    pub rects: Vec<PhysicalRect>,
    /// Blur radius in frame pixels.
    pub radius: f32,
    /// The surface's paint opacity.
    pub alpha: f32,
}

impl Behind {
    /// Everything the blur samples (Mutter's paint volume and
    /// redraw-clip expansion).
    pub(crate) fn sample(&self) -> Option<PhysicalRect> {
        sample_area(&self.rects, self.radius)
    }
}

/// The extents of `rects` grown by the sample padding.
fn sample_area(rects: &[PhysicalRect], radius: f32) -> Option<PhysicalRect> {
    let extents = rects.iter().copied().reduce(Rectangle::merge)?;
    let padding = sample_padding(radius);
    Some(Rectangle::new(
        (extents.loc.x - padding, extents.loc.y - padding).into(),
        (extents.size.w + 2 * padding, extents.size.h + 2 * padding).into(),
    ))
}

/// `calculate_blur_sample_padding`: how far past the region the blur reads.
pub(crate) fn sample_padding(radius: f32) -> i32 {
    (radius * 2.0).ceil() as i32
}

/// `clutter_blur_calculate_downscale_factor`, Firefox's rule.
pub(crate) fn downscale_factor(width: f32, height: f32, radius: f32) -> f32 {
    let mut factor = 1.0f32;
    let (mut w, mut h, mut sigma) = (width, height, radius / 2.0);
    while sigma > MAX_SIGMA && w > MIN_DOWNSCALE_SIZE && h > MIN_DOWNSCALE_SIZE {
        factor *= 2.0;
        w = width / factor;
        h = height / factor;
        sigma = radius / (2.0 * factor);
    }
    factor
}

/// `meta_background_blur_expand_redraw_clip`: damage that touches a
/// blur's sample area takes all of it (within `output`), until no blur
/// adds more (one blur's area may reach another's).
pub(crate) fn expand_damage(
    mut damage: Vec<PhysicalRect>,
    blurs: &[Behind],
    output: PhysicalRect,
) -> Vec<PhysicalRect> {
    let samples: Vec<PhysicalRect> = blurs
        .iter()
        .filter_map(Behind::sample)
        .filter_map(|sample| sample.intersection(output))
        .collect();
    if samples.is_empty() {
        return damage;
    }
    let mut taken = vec![false; samples.len()];
    loop {
        let mut grew = false;
        for (sample, taken) in samples.iter().zip(taken.iter_mut()) {
            if !*taken && damage.iter().any(|d| d.overlaps(*sample)) {
                damage.push(*sample);
                *taken = true;
                grew = true;
            }
        }
        if !grew {
            return damage;
        }
    }
}

/// GL objects for the blur, per EGL context.
#[derive(Default)]
struct Cache {
    programs: Option<Option<Programs>>,
    /// Full-size copy, then the two downscaled passes.
    targets: [Target; 3],
}

#[derive(Default, Clone, Copy)]
struct Target {
    texture: ffi::types::GLuint,
    framebuffer: ffi::types::GLuint,
    size: (i32, i32),
    rgb: bool,
}

#[derive(Clone, Copy)]
struct Program {
    id: ffi::types::GLuint,
    dest: ffi::types::GLint,
    source: ffi::types::GLint,
    tex: ffi::types::GLint,
}

#[derive(Clone, Copy)]
struct Programs {
    blur: Program,
    sigma: ffi::types::GLint,
    pixel_step: ffi::types::GLint,
    direction: ffi::types::GLint,
    composite: Program,
    alpha: ffi::types::GLint,
    saturation: ffi::types::GLint,
    noise: ffi::types::GLint,
    noise_size: ffi::types::GLint,
}

struct CacheData(Arc<Mutex<Cache>>);

/// Attribute 0 carries the unit quad in every program.
const POSITION: ffi::types::GLuint = 0;

const VERTEX: &str = r#"
attribute vec2 position;
uniform vec4 dest;
uniform vec4 source;
varying vec2 v_uv;
void main() {
    v_uv = mix(source.xy, source.zw, position);
    gl_Position = vec4(mix(dest.xy, dest.zw, position), 0.0, 1.0);
}
"#;

const PRECISION: &str = r#"
#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif
"#;

/// clutter-blur.c's `gaussian_blur_glsl`, with GLSL ES 1.00's constant
/// loop bound (enough for sigma 85: the kernel never truncates in use).
const BLUR: &str = r#"
uniform sampler2D tex;
uniform float sigma;
uniform float pixel_step;
uniform vec2 direction;
varying vec2 v_uv;
const int MAX_STEPS = 256;
void main() {
    vec3 gauss_coefficient;
    gauss_coefficient.x = 1.0 / (sqrt(2.0 * 3.14159265) * sigma);
    gauss_coefficient.y = exp(-0.5 / (sigma * sigma));
    gauss_coefficient.z = gauss_coefficient.y * gauss_coefficient.y;
    float gauss_coefficient_total = gauss_coefficient.x;
    vec4 ret = texture2D(tex, v_uv) * gauss_coefficient.x;
    gauss_coefficient.xy *= gauss_coefficient.yz;
    int n_steps = int(ceil(1.5 * sigma)) * 2;
    for (int i = 1; i <= MAX_STEPS; i += 2) {
        if (i > n_steps) {
            break;
        }
        float coefficient_subtotal = gauss_coefficient.x;
        gauss_coefficient.xy *= gauss_coefficient.yz;
        coefficient_subtotal += gauss_coefficient.x;
        float gauss_ratio = gauss_coefficient.x / coefficient_subtotal;
        float foffset = float(i) + gauss_ratio;
        vec2 offset = direction * foffset * pixel_step;
        ret += texture2D(tex, v_uv + offset) * coefficient_subtotal;
        ret += texture2D(tex, v_uv - offset) * coefficient_subtotal;
        gauss_coefficient_total += 2.0 * coefficient_subtotal;
        gauss_coefficient.xy *= gauss_coefficient.yz;
    }
    gl_FragColor = ret / gauss_coefficient_total;
}
"#;

/// The blur node's pipeline: opacity as a premultiplied colour, then
/// clutter-paint-nodes.c's `blur_framebuffer_effect` snippet.
const COMPOSITE: &str = r#"
uniform sampler2D tex;
uniform float alpha;
uniform float saturation;
uniform float noise;
uniform vec2 noise_size;
varying vec2 v_uv;
float random_noise(vec2 position) {
    return fract(52.9829189 * fract(dot(position, vec2(0.06711056, 0.00583715))));
}
void main() {
    vec4 color_out = texture2D(tex, v_uv) * alpha;
    if (color_out.a > 0.0) {
        vec3 color = color_out.rgb / color_out.a;
        float luma = dot(color, vec3(0.299, 0.587, 0.114));
        color = mix(vec3(luma), color, saturation);
        if (noise > 0.0) {
            vec2 grain_coord = floor(v_uv * noise_size);
            float uniform_noise = 2.0 * random_noise(grain_coord) - 1.0;
            float grain = 0.5 * sign(uniform_noise) * (1.0 - sqrt(1.0 - abs(uniform_noise)));
            color += vec3(grain * noise);
        }
        color = clamp(color, vec3(0.0), vec3(1.0));
        color_out.rgb = color * color_out.a;
    }
    gl_FragColor = color_out;
}
"#;

unsafe fn shader(gl: &ffi::Gles2, kind: ffi::types::GLenum, source: &[&str]) -> Option<u32> {
    let shader = gl.CreateShader(kind);
    let pointers: Vec<*const ffi::types::GLchar> =
        source.iter().map(|s| s.as_ptr().cast()).collect();
    let lengths: Vec<ffi::types::GLint> = source.iter().map(|s| s.len() as i32).collect();
    gl.ShaderSource(
        shader,
        source.len() as i32,
        pointers.as_ptr(),
        lengths.as_ptr(),
    );
    gl.CompileShader(shader);
    let mut ok = 0;
    gl.GetShaderiv(shader, ffi::COMPILE_STATUS, &mut ok);
    if ok == 0 {
        let mut log = vec![0u8; 4096];
        let mut len = 0;
        gl.GetShaderInfoLog(shader, log.len() as i32, &mut len, log.as_mut_ptr().cast());
        eprintln!(
            "tuna-compositor: background blur shader: {}",
            String::from_utf8_lossy(&log[..len.max(0) as usize])
        );
        gl.DeleteShader(shader);
        return None;
    }
    Some(shader)
}

unsafe fn program(gl: &ffi::Gles2, fragment: &str) -> Option<Program> {
    let vertex = shader(gl, ffi::VERTEX_SHADER, &[VERTEX])?;
    let Some(fragment) = shader(gl, ffi::FRAGMENT_SHADER, &[PRECISION, fragment]) else {
        gl.DeleteShader(vertex);
        return None;
    };
    let id = gl.CreateProgram();
    gl.AttachShader(id, vertex);
    gl.AttachShader(id, fragment);
    gl.BindAttribLocation(id, POSITION, c"position".as_ptr());
    gl.LinkProgram(id);
    gl.DetachShader(id, vertex);
    gl.DetachShader(id, fragment);
    gl.DeleteShader(vertex);
    gl.DeleteShader(fragment);
    let mut ok = 0;
    gl.GetProgramiv(id, ffi::LINK_STATUS, &mut ok);
    if ok == 0 {
        eprintln!("tuna-compositor: background blur program failed to link");
        gl.DeleteProgram(id);
        return None;
    }
    Some(Program {
        id,
        dest: gl.GetUniformLocation(id, c"dest".as_ptr()),
        source: gl.GetUniformLocation(id, c"source".as_ptr()),
        tex: gl.GetUniformLocation(id, c"tex".as_ptr()),
    })
}

unsafe fn programs(gl: &ffi::Gles2) -> Option<Programs> {
    let blur = program(gl, BLUR)?;
    let Some(composite) = program(gl, COMPOSITE) else {
        gl.DeleteProgram(blur.id);
        return None;
    };
    let uniform =
        |program: &Program, name: &std::ffi::CStr| gl.GetUniformLocation(program.id, name.as_ptr());
    Some(Programs {
        sigma: uniform(&blur, c"sigma"),
        pixel_step: uniform(&blur, c"pixel_step"),
        direction: uniform(&blur, c"direction"),
        alpha: uniform(&composite, c"alpha"),
        saturation: uniform(&composite, c"saturation"),
        noise: uniform(&composite, c"noise"),
        noise_size: uniform(&composite, c"noise_size"),
        blur,
        composite,
    })
}

/// (Re)allocate `target` at `size`; RGB when it receives a
/// `glCopyTexSubImage2D` from a framebuffer that may lack alpha.
unsafe fn ensure(gl: &ffi::Gles2, target: &mut Target, size: (i32, i32), rgb: bool) -> bool {
    if target.texture != 0 && target.size == size && target.rgb == rgb {
        return true;
    }
    if target.texture == 0 {
        gl.GenTextures(1, &mut target.texture);
        gl.GenFramebuffers(1, &mut target.framebuffer);
    }
    let format = if rgb { ffi::RGB } else { ffi::RGBA };
    gl.BindTexture(ffi::TEXTURE_2D, target.texture);
    gl.TexImage2D(
        ffi::TEXTURE_2D,
        0,
        format as i32,
        size.0,
        size.1,
        0,
        format,
        ffi::UNSIGNED_BYTE,
        std::ptr::null(),
    );
    gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MIN_FILTER, ffi::LINEAR as i32);
    gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MAG_FILTER, ffi::LINEAR as i32);
    gl.TexParameteri(
        ffi::TEXTURE_2D,
        ffi::TEXTURE_WRAP_S,
        ffi::CLAMP_TO_EDGE as i32,
    );
    gl.TexParameteri(
        ffi::TEXTURE_2D,
        ffi::TEXTURE_WRAP_T,
        ffi::CLAMP_TO_EDGE as i32,
    );
    gl.BindFramebuffer(ffi::FRAMEBUFFER, target.framebuffer);
    gl.FramebufferTexture2D(
        ffi::FRAMEBUFFER,
        ffi::COLOR_ATTACHMENT0,
        ffi::TEXTURE_2D,
        target.texture,
        0,
    );
    let complete = gl.CheckFramebufferStatus(ffi::FRAMEBUFFER) == ffi::FRAMEBUFFER_COMPLETE;
    target.size = size;
    target.rgb = rgb;
    if !complete {
        gl.DeleteFramebuffers(1, &target.framebuffer);
        gl.DeleteTextures(1, &target.texture);
        *target = Target::default();
    }
    complete
}

/// A frame-space rectangle in window (viewport) pixels, through the
/// frame's own projection: rotated and flipped targets included.
fn to_window(projection: &[f32; 9], viewport: [i32; 4], rect: PhysicalRect) -> PhysicalRect {
    let map = |x: i32, y: i32| {
        let (x, y) = (x as f32, y as f32);
        let nx = projection[0] * x + projection[3] * y + projection[6];
        let ny = projection[1] * x + projection[4] * y + projection[7];
        (
            viewport[0] as f32 + (nx + 1.0) / 2.0 * viewport[2] as f32,
            viewport[1] as f32 + (ny + 1.0) / 2.0 * viewport[3] as f32,
        )
    };
    let (ax, ay) = map(rect.loc.x, rect.loc.y);
    let (bx, by) = map(rect.loc.x + rect.size.w, rect.loc.y + rect.size.h);
    let (x0, x1) = (ax.min(bx).round() as i32, ax.max(bx).round() as i32);
    let (y0, y1) = (ay.min(by).round() as i32, ay.max(by).round() as i32);
    Rectangle::new((x0, y0).into(), (x1 - x0, y1 - y0).into())
}

/// Draw one quad: `dest` in window pixels of `viewport`, `source` in
/// texture coordinates.
unsafe fn quad(
    gl: &ffi::Gles2,
    program: &Program,
    viewport: [i32; 4],
    dest: PhysicalRect,
    source: [f32; 4],
) {
    let ndc = |v: i32, origin: i32, extent: i32| (v - origin) as f32 / extent as f32 * 2.0 - 1.0;
    gl.Uniform4f(
        program.dest,
        ndc(dest.loc.x, viewport[0], viewport[2]),
        ndc(dest.loc.y, viewport[1], viewport[3]),
        ndc(dest.loc.x + dest.size.w, viewport[0], viewport[2]),
        ndc(dest.loc.y + dest.size.h, viewport[1], viewport[3]),
    );
    gl.Uniform4f(program.source, source[0], source[1], source[2], source[3]);
    gl.DrawArrays(ffi::TRIANGLE_STRIP, 0, 4);
}

const UNIT_QUAD: [f32; 8] = [0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0];

/// Paint, under the surface about to be drawn, what the frame holds so
/// far blurred, within `rects` (frame pixels: the blur region, less what
/// is not repainted). `output` bounds the sample.
pub(crate) fn draw_behind(
    frame: &mut GlesFrame<'_, '_>,
    rects: &[PhysicalRect],
    radius: f32,
    alpha: f32,
    output: PhysicalRect,
) -> Result<(), GlesError> {
    if alpha <= 0.0 || rects.is_empty() {
        return Ok(());
    }
    let Some(sample) = sample_area(rects, radius).and_then(|sample| sample.intersection(output))
    else {
        return Ok(());
    };
    let user_data = frame.egl_context().user_data();
    user_data.insert_if_missing_threadsafe(|| CacheData(Arc::new(Mutex::new(Cache::default()))));
    let Some(cache) = user_data.get::<CacheData>().map(|data| data.0.clone()) else {
        return Ok(());
    };
    let blit = frame.capabilities().contains(&Capability::Blit);
    let instancing = frame.capabilities().contains(&Capability::Instancing);
    let projection = *frame.projection();
    frame.with_context(|gl| unsafe {
        let mut cache = cache.lock().unwrap();
        if cache.programs.is_none() {
            cache.programs = Some(programs(gl));
        }
        let Some(Some(programs)) = cache.programs else {
            return;
        };
        let mut framebuffer = 0;
        gl.GetIntegerv(ffi::FRAMEBUFFER_BINDING, &mut framebuffer);
        let mut viewport = [0i32; 4];
        gl.GetIntegerv(ffi::VIEWPORT, viewport.as_mut_ptr());
        let mut scissor = [0i32; 4];
        gl.GetIntegerv(ffi::SCISSOR_BOX, scissor.as_mut_ptr());
        let scissor_test = gl.IsEnabled(ffi::SCISSOR_TEST) == ffi::TRUE;
        let blend = gl.IsEnabled(ffi::BLEND) == ffi::TRUE;

        let source = to_window(&projection, viewport, sample);
        if source.size.w <= 0 || source.size.h <= 0 {
            return;
        }
        let factor = downscale_factor(source.size.w as f32, source.size.h as f32, radius);
        let scaled = (
            ((source.size.w as f32 / factor).floor() as i32).max(1),
            ((source.size.h as f32 / factor).floor() as i32).max(1),
        );
        let [copy, vertical, horizontal] = &mut cache.targets;
        if !ensure(gl, copy, (source.size.w, source.size.h), !blit)
            || !ensure(gl, vertical, scaled, false)
            || !ensure(gl, horizontal, scaled, false)
        {
            gl.BindFramebuffer(ffi::FRAMEBUFFER, framebuffer as u32);
            gl.BindTexture(ffi::TEXTURE_2D, 0);
            return;
        }
        let (copy, vertical, horizontal) = (*copy, *vertical, *horizontal);

        gl.Disable(ffi::SCISSOR_TEST);
        gl.Disable(ffi::BLEND);
        gl.ActiveTexture(ffi::TEXTURE0);
        // What the frame holds beneath the surface.
        if blit {
            gl.BindFramebuffer(ffi::READ_FRAMEBUFFER, framebuffer as u32);
            gl.BindFramebuffer(ffi::DRAW_FRAMEBUFFER, copy.framebuffer);
            gl.BlitFramebuffer(
                source.loc.x,
                source.loc.y,
                source.loc.x + source.size.w,
                source.loc.y + source.size.h,
                0,
                0,
                source.size.w,
                source.size.h,
                ffi::COLOR_BUFFER_BIT,
                ffi::NEAREST,
            );
        } else {
            gl.BindFramebuffer(ffi::FRAMEBUFFER, framebuffer as u32);
            gl.BindTexture(ffi::TEXTURE_2D, copy.texture);
            gl.CopyTexSubImage2D(
                ffi::TEXTURE_2D,
                0,
                0,
                0,
                source.loc.x,
                source.loc.y,
                source.size.w,
                source.size.h,
            );
        }

        gl.BindBuffer(ffi::ARRAY_BUFFER, 0);
        gl.EnableVertexAttribArray(POSITION);
        if instancing {
            gl.VertexAttribDivisor(POSITION, 0);
        }
        gl.VertexAttribPointer(
            POSITION,
            2,
            ffi::FLOAT,
            ffi::FALSE,
            0,
            UNIT_QUAD.as_ptr().cast(),
        );

        // Vertical, then horizontal, as clutter-blur.c.
        let sigma = radius / 2.0 / factor;
        gl.UseProgram(programs.blur.id);
        gl.Uniform1i(programs.blur.tex, 0);
        gl.Uniform1f(programs.sigma, sigma);
        let full = [0.0, 0.0, 1.0, 1.0];
        let target_viewport = [0, 0, scaled.0, scaled.1];
        for (from, to, direction, step) in [
            (copy, vertical, (0.0, 1.0), 1.0 / scaled.1 as f32),
            (vertical, horizontal, (1.0, 0.0), 1.0 / scaled.0 as f32),
        ] {
            gl.BindFramebuffer(ffi::FRAMEBUFFER, to.framebuffer);
            gl.Viewport(0, 0, scaled.0, scaled.1);
            gl.BindTexture(ffi::TEXTURE_2D, from.texture);
            gl.Uniform1f(programs.pixel_step, step);
            gl.Uniform2f(programs.direction, direction.0, direction.1);
            quad(
                gl,
                &programs.blur,
                target_viewport,
                Rectangle::from_size(scaled.into()),
                full,
            );
        }

        // Back into the frame under the surface, premultiplied over.
        gl.BindFramebuffer(ffi::FRAMEBUFFER, framebuffer as u32);
        gl.Viewport(viewport[0], viewport[1], viewport[2], viewport[3]);
        gl.Enable(ffi::BLEND);
        gl.BlendFunc(ffi::ONE, ffi::ONE_MINUS_SRC_ALPHA);
        gl.UseProgram(programs.composite.id);
        gl.Uniform1i(programs.composite.tex, 0);
        gl.Uniform1f(programs.alpha, alpha.min(1.0));
        gl.Uniform1f(programs.saturation, BLUR_SATURATION);
        gl.Uniform1f(programs.noise, BLUR_NOISE);
        gl.Uniform2f(
            programs.noise_size,
            source.size.w as f32,
            source.size.h as f32,
        );
        gl.BindTexture(ffi::TEXTURE_2D, horizontal.texture);
        for rect in rects {
            let Some(rect) = rect.intersection(sample) else {
                continue;
            };
            let dest = to_window(&projection, viewport, rect);
            let u = |x: i32| (x - source.loc.x) as f32 / source.size.w as f32;
            let v = |y: i32| (y - source.loc.y) as f32 / source.size.h as f32;
            quad(
                gl,
                &programs.composite,
                viewport,
                dest,
                [
                    u(dest.loc.x),
                    v(dest.loc.y),
                    u(dest.loc.x + dest.size.w),
                    v(dest.loc.y + dest.size.h),
                ],
            );
        }

        // Smithay's frame state as it was.
        gl.DisableVertexAttribArray(POSITION);
        gl.BindTexture(ffi::TEXTURE_2D, 0);
        gl.UseProgram(0);
        gl.Scissor(scissor[0], scissor[1], scissor[2], scissor[3]);
        if scissor_test {
            gl.Enable(ffi::SCISSOR_TEST);
        }
        if !blend {
            gl.Disable(ffi::BLEND);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> PhysicalRect {
        Rectangle::new((x, y).into(), (w, h).into())
    }

    fn behind(rects: Vec<PhysicalRect>, radius: f32) -> Behind {
        Behind {
            index: 0,
            rects,
            radius,
            alpha: 1.0,
        }
    }

    #[test]
    fn samples_reach_twice_the_radius_past_the_region() {
        assert_eq!(sample_padding(24.0), 48);
        assert_eq!(sample_padding(24.0 * 1.25), 60);
        let blur = behind(vec![rect(100, 100, 50, 20), rect(100, 200, 10, 10)], 24.0);
        assert_eq!(blur.sample(), Some(rect(52, 52, 146, 206)));
        assert_eq!(behind(Vec::new(), 24.0).sample(), None);
    }

    #[test]
    fn downscaling_follows_clutter_blur() {
        // sigma 12 > 6: halve while both sides stay over 256.
        assert_eq!(downscale_factor(1000.0, 800.0, 24.0), 2.0);
        assert_eq!(downscale_factor(200.0, 800.0, 24.0), 1.0);
        assert_eq!(downscale_factor(4000.0, 4000.0, 96.0), 8.0);
        assert_eq!(downscale_factor(4000.0, 4000.0, 12.0), 1.0);
    }

    #[test]
    fn damage_touching_a_sample_repaints_all_of_it() {
        let output = rect(0, 0, 1280, 800);
        // Region 200x100 at (300, 300): samples (252, 252) 296x196.
        let blurs = [behind(vec![rect(300, 300, 200, 100)], 24.0)];
        // Damage away from the sample: untouched, nothing to redo.
        let far = vec![rect(0, 0, 10, 10)];
        assert_eq!(expand_damage(far.clone(), &blurs, output), far);
        // A clock ticking under the padding repaints the whole sample.
        let near = vec![rect(260, 260, 4, 4)];
        let expanded = expand_damage(near, &blurs, output);
        assert!(expanded.contains(&rect(252, 252, 296, 196)), "{expanded:?}");
        // Clamped to the output.
        let edge = [behind(vec![rect(0, 0, 20, 20)], 24.0)];
        let expanded = expand_damage(vec![rect(0, 0, 1, 1)], &edge, output);
        assert!(expanded.contains(&rect(0, 0, 68, 68)), "{expanded:?}");
        // One blur's sample reaching another's takes both.
        let chained = [
            behind(vec![rect(100, 100, 10, 10)], 24.0),
            behind(vec![rect(200, 100, 10, 10)], 24.0),
        ];
        let expanded = expand_damage(vec![rect(55, 105, 1, 1)], &chained, output);
        assert!(expanded.contains(&rect(52, 52, 106, 106)), "{expanded:?}");
        assert!(expanded.contains(&rect(152, 52, 106, 106)), "{expanded:?}");
        // No blur: the damage as it was.
        assert_eq!(expand_damage(far.clone(), &[], output), far);
    }

    /// Headless GL: surfaceless EGL, or `None` when this machine has
    /// none (the test skips instead of failing), like Night Light's.
    #[cfg(feature = "drm")]
    fn headless_renderer() -> Option<smithay::backend::renderer::gles::GlesRenderer> {
        use smithay::backend::egl::native::EGLSurfacelessDisplay;
        use smithay::backend::egl::{EGLContext, EGLDisplay};
        let display = unsafe { EGLDisplay::new(EGLSurfacelessDisplay) }.ok()?;
        let context = EGLContext::new(&display).ok()?;
        unsafe { smithay::backend::renderer::gles::GlesRenderer::new(context) }.ok()
    }

    /// What a frame of `size` (buffer pixels) under `transform` reads
    /// back: 4 px black and white stripes (sharp edges everywhere), then
    /// the blur over `blur` (frame pixels) at `radius` and `alpha`, then
    /// `mark` filled red. RGBA per buffer pixel.
    #[cfg(feature = "drm")]
    #[allow(clippy::too_many_arguments)]
    fn draw(
        renderer: &mut smithay::backend::renderer::gles::GlesRenderer,
        size: (i32, i32),
        transform: smithay::utils::Transform,
        blur: &[PhysicalRect],
        radius: f32,
        alpha: f32,
        mark: Option<PhysicalRect>,
    ) -> Vec<u8> {
        use smithay::backend::allocator::Fourcc;
        use smithay::backend::renderer::{Bind, Color32F, ExportMem, Frame, Offscreen, Renderer};
        use smithay::utils::{Buffer, Size};
        let buffer: Size<i32, Buffer> = size.into();
        let mut target: smithay::backend::renderer::gles::GlesTexture =
            renderer.create_buffer(Fourcc::Abgr8888, buffer).unwrap();
        let mut bound = renderer.bind(&mut target).unwrap();
        {
            let mut frame = renderer.render(&mut bound, size.into(), transform).unwrap();
            let frame_size = transform.transform_size(Size::<i32, Physical>::from(size));
            let full = Rectangle::from_size(frame_size);
            frame.clear(Color32F::BLACK, &[full]).unwrap();
            let stripes: Vec<PhysicalRect> = (0..frame_size.w)
                .step_by(8)
                .map(|x| rect(x, 0, 4, frame_size.h))
                .collect();
            frame
                .clear(Color32F::new(1.0, 1.0, 1.0, 1.0), &stripes)
                .unwrap();
            draw_behind(&mut frame, blur, radius, alpha, full).unwrap();
            if let Some(mark) = mark {
                Frame::draw_solid(&mut frame, full, &[mark], Color32F::new(1.0, 0.0, 0.0, 1.0))
                    .unwrap();
            }
            let _ = frame.finish().unwrap();
        }
        let mapping = renderer
            .copy_framebuffer(&bound, Rectangle::from_size(buffer), Fourcc::Abgr8888)
            .unwrap();
        renderer.map_texture(&mapping).unwrap().to_vec()
    }

    /// Buffer pixels where `a` and `b` differ.
    #[cfg(feature = "drm")]
    fn changed(a: &[u8], b: &[u8]) -> Vec<usize> {
        a.chunks(4)
            .zip(b.chunks(4))
            .enumerate()
            .filter(|(_, (a, b))| a != b)
            .map(|(i, _)| i)
            .collect()
    }

    /// The blur pass on the GPU: under the region, what was drawn before
    /// (sharp stripes) comes out smoothed to an even grey; every pixel
    /// outside the region is bit-identical, and without a region (or at
    /// opacity 0) the frame is identical. Rotated targets put the blur
    /// exactly where Smithay draws the same frame rectangle; a large
    /// sample takes clutter-blur's downscaled path.
    #[cfg(feature = "drm")]
    #[test]
    fn blur_smooths_what_is_behind_the_region_and_nothing_else() {
        use smithay::utils::Transform;
        let Some(mut renderer) = headless_renderer() else {
            eprintln!("background blur GL test skipped: no surfaceless EGL");
            return;
        };
        let cases = [
            ((160, 120), Transform::Normal, rect(40, 30, 64, 48), 8.0),
            ((160, 120), Transform::_90, rect(30, 40, 48, 64), 8.0),
            ((160, 120), Transform::Flipped180, rect(40, 30, 64, 48), 8.0),
            // Sample 496x396 at sigma 12: downscaled by 2.
            ((640, 480), Transform::Normal, rect(120, 90, 400, 300), 24.0),
        ];
        for (size, transform, region, radius) in cases {
            let label = format!("{size:?} {transform:?}");
            let sharp = draw(&mut renderer, size, transform, &[], radius, 1.0, None);
            // Control: no region, or an invisible surface, draws nothing.
            assert_eq!(
                draw(&mut renderer, size, transform, &[], radius, 1.0, None),
                sharp,
                "{label}"
            );
            assert_eq!(
                draw(&mut renderer, size, transform, &[region], radius, 0.0, None),
                sharp,
                "{label}"
            );
            // Where the frame rectangle lands in the buffer.
            let marked = draw(
                &mut renderer,
                size,
                transform,
                &[],
                radius,
                1.0,
                Some(region),
            );
            let inside = changed(&sharp, &marked);
            let area = (region.size.w * region.size.h) as usize;
            assert_eq!(inside.len(), area, "{label}");
            let blurred = draw(&mut renderer, size, transform, &[region], radius, 1.0, None);
            assert_eq!(
                changed(&sharp, &blurred),
                inside,
                "{label}: only the region changes"
            );
            // Sharp black and white become an even grey.
            for &i in &inside {
                let before = &sharp[i * 4..i * 4 + 4];
                assert!(
                    before[..3] == [0, 0, 0] || before[..3] == [255, 255, 255],
                    "{label}: {before:?}"
                );
                let pixel = &blurred[i * 4..i * 4 + 4];
                for channel in &pixel[..3] {
                    assert!(
                        (i32::from(*channel) - 128).abs() <= 16,
                        "{label}: pixel {i} = {pixel:?}"
                    );
                }
                assert_eq!(pixel[3], 255, "{label}");
            }
        }
        // Release the GL context before the surfaceless display drops.
        drop(renderer);
    }
}
