//! GNOME Shell 51 minimize/unminimize: 400 ms ease-out-expo.
//! Full-motion restore remains opaque; reduced motion and monitor-sized actors fade.
use std::time::Duration;
use tuna_shell_control::motion::MotionPolicy;
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Effect {
    Minimize,
    Unminimize,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub icon_weight: f64,
    pub alpha: f32,
}
pub fn duration(policy: MotionPolicy) -> Duration {
    if policy.allows_fades() {
        Duration::from_secs_f64(policy.adjust_ms(400.0) / 1000.0)
    } else {
        Duration::ZERO
    }
}
/// Clutter's ease-out-expo has exact endpoints and 1 - 2^-10t inside.
///
/// Without motion (reduced motion or a monitor-sized window) GNOME Shell 51
/// eases the restore's opacity on the still-hidden actor
/// (`windowManager.js:1243-1251` never calls `actor.show()`), so Clutter
/// skips the implicit transition and the window appears at full opacity.
pub fn frame(effect: Effect, progress: f64, policy: MotionPolicy, monitor_sized: bool) -> Frame {
    let p = if policy.allows_fades() {
        progress.clamp(0.0, 1.0)
    } else {
        1.0
    };
    let ease = if p >= 1.0 {
        1.0
    } else {
        1.0 - 2f64.powf(-10.0 * p)
    };
    let motion = policy.allows_motion() && !monitor_sized;
    Frame {
        icon_weight: if motion {
            match effect {
                Effect::Minimize => ease,
                Effect::Unminimize => 1.0 - ease,
            }
        } else {
            0.0
        },
        alpha: match effect {
            Effect::Minimize => ((255.0 * (1.0 - ease)) as u8) as f32 / 255.0,
            Effect::Unminimize => 1.0,
        },
    }
}
/// Whether a restore moves at all; otherwise it is instant (see [`frame`]).
pub fn restore_animates(policy: MotionPolicy, monitor_sized: bool) -> bool {
    policy.allows_motion() && !monitor_sized
}
/// Mutter 51: fullscreen or an exact whole-screen frame. A normal window
/// matching only one monitor in a multi-monitor desktop still uses motion.
pub fn monitor_sized(
    fullscreen: bool,
    frame: smithay::utils::Rectangle<i32, smithay::utils::Logical>,
    screen: (i32, i32),
) -> bool {
    fullscreen || frame == smithay::utils::Rectangle::new((0, 0).into(), screen.into())
}
/// GNOME's missing-icon fallback is the monitor's upper leading corner.
pub fn fallback(
    monitor: smithay::utils::Rectangle<i32, smithay::utils::Logical>,
    rtl: bool,
) -> crate::size_change::RectF {
    smithay::utils::Rectangle::new(
        (
            f64::from(monitor.loc.x) + if rtl { f64::from(monitor.size.w) } else { 0.0 },
            f64::from(monitor.loc.y),
        )
            .into(),
        (0.0, 0.0).into(),
    )
}
/// The destination GNOME Shell 51 minimizes toward (`windowManager.js`
/// `_minimizeWindow`, lines 1183-1201): `meta_window_get_icon_geometry`,
/// else the monitor's upper leading corner at scale 0. Mutter sets icon
/// geometry only from X11 `_NET_WM_ICON_GEOMETRY` (`src/x11/window-props.c`
/// `reload_icon_geometry`, line 295), so `client_icon` is that property and
/// wins whenever set. GNOME Shell never sets one on Wayland; Tuna's dash
/// lives in the overview, so its published icon counts only while it is on
/// screen.
pub fn target(
    client_icon: Option<smithay::utils::Rectangle<i32, smithay::utils::Logical>>,
    dash_icon: Option<smithay::utils::Rectangle<i32, smithay::utils::Logical>>,
    dash_visible: bool,
    monitor: smithay::utils::Rectangle<i32, smithay::utils::Logical>,
    rtl: bool,
) -> crate::size_change::RectF {
    match (client_icon, dash_icon) {
        (Some(icon), _) => icon.to_f64(),
        (None, Some(icon)) if dash_visible => icon.to_f64(),
        _ => fallback(monitor, rtl),
    }
}
/// What besides the window decides a minimize destination.
#[derive(Clone, Copy, Debug)]
pub struct TargetContext {
    pub right_to_left: bool,
    pub dash_visible: bool,
}
/// Bounds of both layers when a resize crossfade is interrupted.
pub fn bounds(
    a: crate::size_change::RectF,
    b: crate::size_change::RectF,
) -> crate::size_change::RectF {
    let x = a.loc.x.min(b.loc.x);
    let y = a.loc.y.min(b.loc.y);
    smithay::utils::Rectangle::new(
        (x, y).into(),
        (
            (a.loc.x + a.size.w).max(b.loc.x + b.size.w) - x,
            (a.loc.y + a.size.h).max(b.loc.y + b.size.h) - y,
        )
            .into(),
    )
}
pub fn geometry(
    normal: crate::size_change::RectF,
    icon: crate::size_change::RectF,
    weight: f64,
) -> crate::size_change::RectF {
    let lerp = |a: f64, b: f64| a + (b - a) * weight;
    smithay::utils::Rectangle::new(
        (
            lerp(normal.loc.x, icon.loc.x),
            lerp(normal.loc.y, icon.loc.y),
        )
            .into(),
        (
            lerp(normal.size.w, icon.size.w),
            lerp(normal.size.h, icon.size.h),
        )
            .into(),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    use tuna_shell_control::motion::MotionLevel;
    #[test]
    fn mutter_monitor_sized_excludes_normal_single_monitor_on_multi_output_desktop() {
        let rect = smithay::utils::Rectangle::new((0, 0).into(), (1280, 800).into());
        assert!(monitor_sized(true, rect, (2560, 800)));
        assert!(monitor_sized(false, rect, (1280, 800)));
        assert!(!monitor_sized(false, rect, (2560, 800)));
    }
    #[test]
    fn resize_capture_bounds_preserve_both_layers_and_fractional_edges() {
        let a = smithay::utils::Rectangle::new((-10.5, 20.25).into(), (500.0, 300.0).into());
        let b = smithay::utils::Rectangle::new((10.0, -5.5).into(), (600.0, 450.0).into());
        let r = bounds(a, b);
        assert_eq!(r.loc, (-10.5, -5.5).into());
        assert_eq!(r.size, (620.5, 450.0).into());
        let integer: smithay::utils::Rectangle<i32, smithay::utils::Logical> = r.to_i32_up();
        assert_eq!(integer.loc, (-11, -6).into());
        assert_eq!(integer.size, (621, 451).into());
    }
    #[test]
    fn icon_target_geometry_and_rtl_fallback_use_global_logical_monitor_bounds() {
        let monitor = smithay::utils::Rectangle::new((-1920, 100).into(), (1920, 1080).into());
        assert_eq!(fallback(monitor, false).loc, (-1920.0, 100.0).into());
        assert_eq!(fallback(monitor, true).loc, (0.0, 100.0).into());
        let normal = smithay::utils::Rectangle::new((-1800.0, 200.0).into(), (500.0, 350.0).into());
        let icon = smithay::utils::Rectangle::new((-1000.0, 1050.0).into(), (64.0, 64.0).into());
        assert_eq!(geometry(normal, icon, 0.0), normal);
        assert_eq!(geometry(normal, icon, 1.0), icon);
        assert_eq!(geometry(normal, icon, 0.5).size, (282.0, 207.0).into());
    }
    #[test]
    fn hidden_dash_icons_fall_back_to_the_gnome_leading_corner() {
        let monitor = smithay::utils::Rectangle::new((0, 0).into(), (1280, 800).into());
        let icon = smithay::utils::Rectangle::new((600, 720).into(), (64, 64).into());
        let corner = fallback(monitor, false);
        assert_eq!(target(None, Some(icon), false, monitor, false), corner);
        assert_eq!(target(None, None, true, monitor, false), corner);
        assert_eq!(
            target(None, None, false, monitor, true),
            fallback(monitor, true)
        );
        assert_eq!(
            target(None, Some(icon), true, monitor, false),
            icon.to_f64()
        );
    }
    #[test]
    fn x11_icon_geometry_wins_like_mutter_get_icon_geometry() {
        let monitor = smithay::utils::Rectangle::new((0, 0).into(), (1280, 800).into());
        let dash = smithay::utils::Rectangle::new((600, 720).into(), (64, 64).into());
        let client = smithay::utils::Rectangle::new((40, 760).into(), (32, 24).into());
        for dash_visible in [false, true] {
            for rtl in [false, true] {
                assert_eq!(
                    target(Some(client), Some(dash), dash_visible, monitor, rtl),
                    client.to_f64()
                );
                assert_eq!(
                    target(Some(client), None, dash_visible, monitor, rtl),
                    client.to_f64()
                );
            }
        }
    }
    #[test]
    fn minimize_400ms_expo_moves_scales_and_fades() {
        let p = MotionPolicy::default();
        assert_eq!(duration(p), Duration::from_millis(400));
        assert_eq!(
            frame(Effect::Minimize, 0.0, p, false),
            Frame {
                icon_weight: 0.0,
                alpha: 1.0
            }
        );
        let f = frame(Effect::Minimize, 0.5, p, false);
        assert_eq!(f.icon_weight, 0.96875);
        assert_eq!(f.alpha, 7.0 / 255.0);
        assert_eq!(
            frame(Effect::Minimize, 1.0, p, false),
            Frame {
                icon_weight: 1.0,
                alpha: 0.0
            }
        );
    }
    #[test]
    fn clutter_guint_opacity_truncates_quarter_expo_samples() {
        let policy = MotionPolicy::default();
        assert_eq!(
            frame(Effect::Minimize, 0.25, policy, false).alpha,
            45.0 / 255.0
        );
        assert_eq!(
            frame(Effect::Minimize, 0.25, policy, true).alpha,
            45.0 / 255.0
        );
    }
    #[test]
    fn unminimize_motion_stays_opaque_and_restores_geometry() {
        let p = MotionPolicy::default();
        assert_eq!(
            frame(Effect::Unminimize, 0.0, p, false),
            Frame {
                icon_weight: 1.0,
                alpha: 1.0
            }
        );
        assert_eq!(
            frame(Effect::Unminimize, 0.5, p, false),
            Frame {
                icon_weight: 0.03125,
                alpha: 1.0
            }
        );
        assert_eq!(
            frame(Effect::Unminimize, 1.0, p, false),
            Frame {
                icon_weight: 0.0,
                alpha: 1.0
            }
        );
    }
    #[test]
    fn reduced_and_monitor_sized_minimize_fades_and_restore_shows_at_once() {
        let reduced = MotionPolicy {
            level: MotionLevel::FadeOnly,
            ..Default::default()
        };
        for (p, monitor) in [(reduced, false), (MotionPolicy::default(), true)] {
            assert_eq!(
                frame(Effect::Minimize, 0.5, p, monitor),
                Frame {
                    icon_weight: 0.0,
                    alpha: 7.0 / 255.0
                }
            );
            assert!(!restore_animates(p, monitor));
            for progress in [0.0, 0.25, 1.0] {
                assert_eq!(
                    frame(Effect::Unminimize, progress, p, monitor),
                    Frame {
                        icon_weight: 0.0,
                        alpha: 1.0
                    }
                );
            }
        }
        assert!(restore_animates(MotionPolicy::default(), false));
    }
    #[test]
    fn off_is_immediate_and_slowdown_scales_duration() {
        let off = MotionPolicy {
            level: MotionLevel::Off,
            ..Default::default()
        };
        assert_eq!(duration(off), Duration::ZERO);
        assert_eq!(frame(Effect::Minimize, 0.0, off, false).alpha, 0.0);
        assert_eq!(frame(Effect::Unminimize, 0.0, off, false).alpha, 1.0);
        assert_eq!(
            duration(MotionPolicy {
                slowdown_milli: 2000,
                ..Default::default()
            }),
            Duration::from_millis(800)
        );
    }
}
