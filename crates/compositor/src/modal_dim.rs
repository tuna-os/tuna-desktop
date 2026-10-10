//! GNOME Shell 51 attached-modal parent brightness, without copying surfaces.
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexProgram, UniformName, UniformType};
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tuna_shell_control::motion::MotionPolicy;

/// GNOME stores brightness in an 8-bit CoglColor: floor(127 * 0.7) /127.
pub const DIM_MULTIPLIER: f32 = 88.0 / 127.0;
#[derive(Clone, Debug)]
struct Transition {
    target: bool,
    from: f32,
    start: Duration,
    duration: Duration,
    value: f32,
}
impl Transition {
    fn advance(&mut self, now: Duration) {
        let p = if self.duration.is_zero() {
            1.0
        } else {
            (now.saturating_sub(self.start).as_secs_f32() / self.duration.as_secs_f32())
                .clamp(0.0, 1.0)
        };
        let to = if self.target { DIM_MULTIPLIER } else { 1.0 };
        self.value = self.from + (to - self.from) * (1.0 - (1.0 - p).powi(2));
    }
}
pub struct ModalDim {
    enabled: bool,
    entries: HashMap<u64, Transition>,
}
impl Default for ModalDim {
    fn default() -> Self {
        Self {
            enabled: true,
            entries: HashMap::new(),
        }
    }
}
impl ModalDim {
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }
    /// Desired membership is a set, so additional dialogs never restart a fade.
    pub fn step(
        &mut self,
        alive: &HashSet<u64>,
        parents: &HashSet<u64>,
        now: Duration,
        policy: MotionPolicy,
        animate: bool,
    ) {
        self.entries.retain(|id, _| alive.contains(id));
        for t in self.entries.values_mut() {
            t.advance(now);
        }
        for id in alive {
            let target = self.enabled && parents.contains(id);
            let old = self.entries.get(id);
            if old.is_none() && !target {
                continue;
            }
            let changed = old.is_none_or(|t| t.target != target);
            if changed {
                let from = old.map_or(1.0, |t| t.value);
                let duration = if self.enabled && animate && policy.allows_fades() {
                    Duration::from_secs_f64(
                        policy.adjust_ms(if target { 500.0 } else { 250.0 }) / 1000.0,
                    )
                } else {
                    Duration::ZERO
                };
                let mut t = Transition {
                    target,
                    from,
                    start: now,
                    duration,
                    value: from,
                };
                t.advance(now);
                self.entries.insert(*id, t);
            } else if !self.enabled || !animate || !policy.allows_fades() {
                let t = self.entries.get_mut(id).unwrap();
                t.duration = Duration::ZERO;
                t.advance(now);
            }
        }
        self.entries.retain(|_, t| t.target || t.value < 1.0);
    }
    pub fn multiplier(&self, id: u64) -> f32 {
        self.entries.get(&id).map_or(1.0, |t| t.value)
    }
    pub fn running(&self) -> bool {
        self.entries
            .values()
            .any(|t| t.value != if t.target { DIM_MULTIPLIER } else { 1.0 })
    }
    pub fn to_json(&self) -> serde_json::Value {
        let mut entries:Vec<_> = self.entries.iter().map(|(id,t)|serde_json::json!({"parent":id,"dimmed":t.target,"multiplier":t.value,"duration_ms":t.duration.as_millis()})).collect();
        entries.sort_by_key(|v| v["parent"].as_u64());
        serde_json::json!({"enabled":self.enabled,"parents":entries,"running":self.running(),"target_multiplier":DIM_MULTIPLIER})
    }
}

// Own shader follows Smithay's default formats and premultiplied alpha. Dimming
// multiplies RGB only, preserving shadows/transparent pixels and all transforms.
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
#if defined(DEBUG_FLAGS)
    if (tint == 1.0) color = vec4(0.0,0.2,0.0,0.2) + color * 0.8;
#endif
    gl_FragColor = color;
}"#;
struct Program(Option<GlesTexProgram>);
pub fn program(renderer: &mut GlesRenderer) -> Option<GlesTexProgram> {
    if let Some(p) = renderer.egl_context().user_data().get::<Program>() {
        return p.0.clone();
    }
    let p = renderer
        .compile_custom_texture_shader(SHADER, &[UniformName::new("brightness", UniformType::_1f)])
        .map_err(|e| eprintln!("tuna-compositor: modal dim shader unavailable: {e}"))
        .ok();
    renderer
        .egl_context()
        .user_data()
        .insert_if_missing(|| Program(p.clone()));
    p
}
#[cfg(test)]
mod tests {
    use super::*;
    use tuna_shell_control::motion::MotionLevel;
    fn p(level: MotionLevel) -> MotionPolicy {
        MotionPolicy::new(level, 1.0)
    }
    #[test]
    fn live_attach_preference_disables_immediately_and_reenables_fade() {
        let mut dim = ModalDim::default();
        let alive = HashSet::from([1]);
        let parents = alive.clone();
        let policy = MotionPolicy::default();
        dim.step(&alive, &parents, Duration::ZERO, policy, true);
        dim.step(&alive, &parents, Duration::from_millis(500), policy, true);
        assert_eq!(dim.multiplier(1), DIM_MULTIPLIER);
        dim.set_enabled(false);
        dim.step(&alive, &parents, Duration::from_millis(500), policy, true);
        assert_eq!(dim.multiplier(1), 1.0);
        assert!(!dim.running());
        dim.set_enabled(true);
        dim.step(&alive, &parents, Duration::from_millis(500), policy, true);
        assert!(dim.running());
        dim.step(&alive, &parents, Duration::from_millis(1000), policy, true);
        assert_eq!(dim.multiplier(1), DIM_MULTIPLIER);
    }
    #[test]
    fn parent_dims_500ms_and_undims_250ms_quad() {
        let mut d = ModalDim::default();
        let ids = HashSet::from([1]);
        let parents = ids.clone();
        d.step(&ids, &parents, Duration::ZERO, p(MotionLevel::Full), true);
        assert_eq!(d.multiplier(1), 1.0);
        d.step(
            &ids,
            &parents,
            Duration::from_millis(250),
            p(MotionLevel::Full),
            true,
        );
        assert!((d.multiplier(1) - (1.0 + (DIM_MULTIPLIER - 1.0) * 0.75)).abs() < 1e-6);
        d.step(
            &ids,
            &parents,
            Duration::from_millis(500),
            p(MotionLevel::Full),
            true,
        );
        assert_eq!(d.multiplier(1), DIM_MULTIPLIER);
        d.step(
            &ids,
            &HashSet::new(),
            Duration::from_millis(500),
            p(MotionLevel::Full),
            true,
        );
        d.step(
            &ids,
            &HashSet::new(),
            Duration::from_millis(750),
            p(MotionLevel::Full),
            true,
        );
        assert_eq!(d.multiplier(1), 1.0);
        assert!(!d.running());
    }
    #[test]
    fn overlapping_modal_membership_does_not_restart_and_reversal_is_continuous() {
        let mut d = ModalDim::default();
        let ids = HashSet::from([1]);
        d.step(&ids, &ids, Duration::ZERO, p(MotionLevel::Full), true);
        d.step(
            &ids,
            &ids,
            Duration::from_millis(250),
            p(MotionLevel::Full),
            true,
        );
        let mid = d.multiplier(1);
        d.step(
            &ids,
            &HashSet::new(),
            Duration::from_millis(250),
            p(MotionLevel::Full),
            true,
        );
        assert_eq!(d.multiplier(1), mid);
        d.step(
            &ids,
            &HashSet::new(),
            Duration::from_millis(375),
            p(MotionLevel::Full),
            true,
        );
        let reversed = d.multiplier(1);
        assert!(reversed > mid);
        d.step(
            &ids,
            &ids,
            Duration::from_millis(375),
            p(MotionLevel::Full),
            true,
        );
        assert_eq!(d.multiplier(1), reversed);
    }
    #[test]
    fn reduced_motion_preserves_fade_off_and_suppression_snap_and_parent_death_clears() {
        let mut d = ModalDim::default();
        let ids = HashSet::from([1]);
        d.step(&ids, &ids, Duration::ZERO, p(MotionLevel::FadeOnly), true);
        assert!(d.running());
        d.step(&ids, &ids, Duration::ZERO, p(MotionLevel::Off), true);
        assert_eq!(d.multiplier(1), DIM_MULTIPLIER);
        d.step(
            &ids,
            &HashSet::new(),
            Duration::ZERO,
            p(MotionLevel::Full),
            false,
        );
        assert_eq!(d.multiplier(1), 1.0);
        d.step(&ids, &ids, Duration::ZERO, p(MotionLevel::Full), false);
        assert_eq!(d.multiplier(1), DIM_MULTIPLIER);
        d.step(
            &HashSet::new(),
            &ids,
            Duration::ZERO,
            p(MotionLevel::Full),
            true,
        );
        assert_eq!(d.multiplier(1), 1.0);
    }
}
