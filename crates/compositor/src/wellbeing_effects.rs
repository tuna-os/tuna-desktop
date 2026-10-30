//! GNOME 51 uiGroup wellbeing color effects, before output temperature.
//! Linear transforms preserve premultiplied alpha and source-over compositing.
use smithay::backend::renderer::{
    gles::{GlesRenderer, GlesTexProgram, Uniform, UniformName, UniformType},
    Color32F,
};
use std::time::Duration;
use tuna_shell_control::MotionPolicy;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    pub desaturation: f32,
    pub brightness: f32,
}
impl Default for Sample {
    fn default() -> Self {
        Self {
            desaturation: 0.0,
            brightness: 1.0,
        }
    }
}
impl Sample {
    pub fn color(self, color: Color32F) -> Color32F {
        let gray = color.r() * 0.299 + color.g() * 0.587 + color.b() * 0.114;
        let convert = |v| (v + (gray - v) * self.desaturation) * self.brightness;
        Color32F::new(
            convert(color.r()),
            convert(color.g()),
            convert(color.b()),
            color.a(),
        )
    }
    pub fn active(self) -> bool {
        self != Self::default()
    }
    pub fn bits(self) -> [u32; 2] {
        [self.desaturation.to_bits(), self.brightness.to_bits()]
    }
}
#[derive(Debug, Default)]
pub struct Effects {
    grayscale: bool,
    fade: bool,
    gray_start: Option<Duration>,
    fade_start: Option<Duration>,
    gray_duration: Duration,
    fade_duration: Duration,
}
impl Effects {
    pub fn set(&mut self, grayscale: bool, fade: bool, now: Duration, motion: MotionPolicy) {
        let duration = Duration::from_secs_f64(motion.adjust_ms(3000.0) / 1000.0);
        if grayscale != self.grayscale {
            self.gray_start = grayscale.then_some(now);
            self.gray_duration = duration;
            self.grayscale = grayscale;
        }
        if fade != self.fade {
            self.fade_start = fade.then_some(now);
            self.fade_duration = duration;
            self.fade = fade;
        }
        self.motion(motion);
    }
    pub fn motion(&mut self, motion: MotionPolicy) {
        if !motion.allows_fades() {
            self.gray_start = None;
            self.fade_start = None;
        }
    }
    pub fn sample(&self, now: Duration) -> Sample {
        let progress = |start: Option<Duration>, duration: Duration| {
            let t = start.map_or(1.0, |start| {
                if duration.is_zero() {
                    1.0
                } else {
                    (now.saturating_sub(start).as_secs_f64() / duration.as_secs_f64())
                        .clamp(0.0, 1.0)
                }
            });
            (1.0 - (1.0 - t).powi(2)) as f32
        };
        Sample {
            desaturation: if self.grayscale {
                progress(self.gray_start, self.gray_duration)
            } else {
                0.0
            },
            brightness: if self.fade {
                1.0 - 0.4 * progress(self.fade_start, self.fade_duration)
            } else {
                1.0
            },
        }
    }
    pub fn animating(&self, now: Duration) -> bool {
        [
            (self.gray_start, self.gray_duration),
            (self.fade_start, self.fade_duration),
        ]
        .into_iter()
        .any(|(start, duration)| start.is_some_and(|start| now.saturating_sub(start) < duration))
    }
}

#[derive(Debug, Clone)]
pub struct FrameColor {
    pub program: GlesTexProgram,
    pub gains: crate::night_light::ColorGains,
    pub uniform: [f32; 3],
    pub wellbeing: Sample,
}
struct Cached(Option<GlesTexProgram>);
impl FrameColor {
    pub fn new(
        renderer: &mut GlesRenderer,
        wellbeing: Sample,
        gains: Option<crate::night_light::ColorGains>,
    ) -> Option<Self> {
        if !wellbeing.active() && gains.is_none() {
            return None;
        }
        let gains = gains.unwrap_or(crate::night_light::ColorGains {
            red: 1.0,
            green: 1.0,
            blue: 1.0,
        });
        let program = if let Some(cached) = renderer.egl_context().user_data().get::<Cached>() {
            cached.0.clone()
        } else {
            let result = renderer
                .compile_custom_texture_shader(
                    SHADER,
                    &[
                        UniformName::new("brightness", UniformType::_1f),
                        UniformName::new("nl_gains", UniformType::_3f),
                        UniformName::new("desaturation", UniformType::_1f),
                        UniformName::new("wellbeing_brightness", UniformType::_1f),
                    ],
                )
                .map_err(|error| {
                    eprintln!("tuna-compositor: wellbeing color shader unavailable: {error}")
                })
                .ok();
            renderer
                .egl_context()
                .user_data()
                .insert_if_missing(|| Cached(result.clone()));
            result
        }?;
        Some(Self {
            program,
            gains,
            uniform: crate::night_light::gains_f32(gains),
            wellbeing,
        })
    }
    pub fn uniforms(&self, brightness: f32) -> Vec<Uniform<'static>> {
        vec![
            Uniform::new("brightness", brightness),
            Uniform::new("nl_gains", self.uniform),
            Uniform::new("desaturation", self.wellbeing.desaturation),
            Uniform::new("wellbeing_brightness", self.wellbeing.brightness),
        ]
    }
    pub fn color(&self, color: Color32F) -> Color32F {
        let color = self.wellbeing.color(color);
        let [r, g, b] =
            crate::night_light::apply_gains([color.r(), color.g(), color.b()], self.gains);
        Color32F::new(r, g, b, color.a())
    }
}

const SHADER: &str = r#"#version 100
//_DEFINES_
#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif
precision mediump float;
#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif
uniform float alpha;
uniform float brightness;
uniform vec3 nl_gains;
uniform float desaturation;
uniform float wellbeing_brightness;
varying vec2 v_coords;
#if defined(DEBUG_FLAGS)
uniform float tint;
#endif
void main() {
    vec4 color = texture2D(tex, v_coords);
#if defined(NO_ALPHA)
    color = vec4(color.rgb, 1.0) * alpha;
#else
    color = color * alpha;
#endif
    color.rgb *= brightness;
    float gray = dot(color.rgb, vec3(0.299, 0.587, 0.114));
    color.rgb = mix(color.rgb, vec3(gray), desaturation) * wellbeing_brightness;
    color.rgb *= nl_gains;
#if defined(DEBUG_FLAGS)
    if (tint == 1.0) color = vec4(0.0,0.2,0.0,0.2) + color * 0.8;
#endif
    gl_FragColor = color;
}"#;

#[cfg(test)]
mod tests {
    use super::*;
    use tuna_shell_control::MotionLevel;
    #[test]
    fn entry_exit_and_motion_policy_match_effects() {
        for level in [MotionLevel::Full, MotionLevel::FadeOnly] {
            let mut e = Effects::default();
            e.set(true, true, Duration::ZERO, MotionPolicy::new(level, 1.0));
            assert_eq!(e.sample(Duration::ZERO), Sample::default());
            let mid = e.sample(Duration::from_millis(1500));
            assert_eq!(mid.desaturation, 0.75);
            assert!((mid.brightness - 0.7).abs() < 0.00001);
            e.set(
                false,
                false,
                Duration::from_millis(1500),
                MotionPolicy::default(),
            );
            assert_eq!(e.sample(Duration::from_millis(1500)), Sample::default());
            assert!(!e.animating(Duration::from_millis(1500)));
        }
        let mut e = Effects::default();
        e.set(
            true,
            true,
            Duration::ZERO,
            MotionPolicy::new(MotionLevel::Off, 1.0),
        );
        assert_eq!(e.sample(Duration::ZERO).desaturation, 1.0);
        assert!(!e.animating(Duration::ZERO));
    }
    #[test]
    fn premultiplied_transform_distributes_over_source_over() {
        let effect = Sample {
            desaturation: 0.75,
            brightness: 0.7,
        };
        let foreground = Color32F::new(0.2, 0.1, 0.05, 0.25);
        let background = Color32F::new(0.1, 0.3, 0.5, 0.75);
        let over = |a: Color32F, b: Color32F| {
            Color32F::new(
                a.r() + b.r() * (1.0 - a.a()),
                a.g() + b.g() * (1.0 - a.a()),
                a.b() + b.b() * (1.0 - a.a()),
                a.a() + b.a() * (1.0 - a.a()),
            )
        };
        let first = effect.color(over(foreground, background));
        let second = over(effect.color(foreground), effect.color(background));
        for (a, b) in [
            (first.r(), second.r()),
            (first.g(), second.g()),
            (first.b(), second.b()),
            (first.a(), second.a()),
        ] {
            assert!((a - b).abs() < 0.000001);
        }
        let mut effects = Effects::default();
        effects.set(true, false, Duration::ZERO, MotionPolicy::default());
        effects.set(
            true,
            true,
            Duration::from_secs(1),
            MotionPolicy::new(tuna_shell_control::MotionLevel::Full, 2.0),
        );
        assert_eq!(
            effects.sample(Duration::from_secs(3)).desaturation,
            1.0,
            "the independent fade ramp must not lengthen grayscale"
        );
    }

    #[test]
    fn repeat_atomic_commands_do_not_restart_and_disabled_restores_identity() {
        let mut e = Effects::default();
        e.set(true, false, Duration::ZERO, MotionPolicy::default());
        e.set(true, false, Duration::from_secs(1), MotionPolicy::default());
        assert!((e.sample(Duration::from_secs(1)).desaturation - 5.0 / 9.0).abs() < 0.00001);
        e.set(false, true, Duration::from_secs(1), MotionPolicy::default());
        assert_eq!(e.sample(Duration::from_secs(1)).desaturation, 0.0);
        let c = Color32F::new(0.2, 0.1, 0.0, 0.25);
        assert_eq!(Sample::default().color(c), c);
        let gray = Sample {
            desaturation: 1.0,
            brightness: 0.6,
        }
        .color(c);
        assert_eq!(gray.a(), c.a());
        assert!((gray.r() - gray.g()).abs() < 0.00001);
        assert!(gray.r() <= gray.a());
    }
}
