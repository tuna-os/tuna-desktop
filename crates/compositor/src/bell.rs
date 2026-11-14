//! Native xdg bell consumers with live policy and bounded player lifetime.
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use std::os::unix::process::CommandExt;
use std::time::{Duration, Instant};
use tuna_shell_control::BellPolicy;

pub const BELL_SOUND: &str = "bell-window-system";

#[derive(Default)]
pub(crate) struct Bell {
    pub rings: u64,
    policy: BellPolicy,
    policy_ready: bool,
    last_visual: Option<Instant>,
    visual: Option<(Instant, u8, Option<WlSurface>)>,
    player: Option<(std::process::Child, Instant)>,
    wayland_display: Option<String>,
}

fn flashes(last: Option<Instant>, now: Instant) -> u8 {
    match last.map(|last| now.saturating_duration_since(last)) {
        Some(gap) if gap < Duration::from_millis(500) => 0,
        Some(gap) if gap < Duration::from_secs(3) => 1,
        _ => 2,
    }
}

/// A 50 ms quad-in leg followed by `repeats` alternating legs.
/// Mutter stops and removes the overlay after the final leg, including a peak.
fn opacity(elapsed: Duration, pulses: u8) -> f32 {
    let ms = elapsed.as_secs_f32() * 1000.0;
    if ms >= (f32::from(pulses) + 1.0) * 50.0 {
        return 0.0;
    }
    let leg = ms % 100.0;
    let fraction = if leg < 50.0 {
        leg / 50.0
    } else {
        (100.0 - leg) / 50.0
    };
    fraction * fraction * (192.0 / 255.0)
}

impl Bell {
    pub(crate) fn set_wayland_display(&mut self, display: &str) {
        self.wayland_display = Some(display.to_owned());
    }

    fn player_command(&self) -> std::process::Command {
        let mut command = std::process::Command::new("canberra-gtk-play");
        if let Some(display) = &self.wayland_display {
            command.env("WAYLAND_DISPLAY", display);
        }
        command
    }

    pub(crate) fn player_deadline(&self) -> Option<Instant> {
        self.player.as_ref().map(|(_, start)| {
            let deadline = *start + Duration::from_secs(5);
            if deadline <= Instant::now() {
                Instant::now() + Duration::from_millis(16)
            } else {
                deadline
            }
        })
    }

    pub fn set_policy(&mut self, policy: BellPolicy) {
        if !policy.visual {
            self.visual = None;
        }
        if !policy.audible || !policy.event_sounds {
            self.stop_player();
        }
        self.policy = policy;
        self.policy_ready = true;
    }
    pub fn ring(&mut self, surface: Option<WlSurface>) {
        self.rings = self.rings.saturating_add(1);
        let now = Instant::now();
        self.tick(now);
        if self.policy_ready && self.policy.visual {
            let pulses = flashes(self.last_visual, now);
            if pulses > 0 {
                self.last_visual = Some(now);
                self.visual = Some((
                    now,
                    pulses,
                    self.policy.frame_flash.then_some(surface).flatten(),
                ));
            }
        }
        if !self.audible_enabled()
            || self.player.is_some()
            || std::env::var_os("TUNA_BELL").is_some_and(|v| v == "0")
        {
            return;
        }
        let theme = format!(
            "canberra.xdg-theme.name={}",
            self.policy.theme.chars().take(256).collect::<String>()
        );
        let mut command = self.player_command();
        let parent = std::process::id() as libc::pid_t;
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0
                    || libc::getppid() != parent
                {
                    libc::_exit(127);
                }
                libc::signal(libc::SIGALRM, libc::SIG_DFL);
                libc::alarm(5);
                Ok(())
            });
        }
        self.player = command
            .args([
                "--id",
                BELL_SOUND,
                "--description",
                "Bell event",
                "--property",
                &theme,
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .ok()
            .map(|child| (child, now));
    }
    pub fn tick(&mut self, now: Instant) {
        if let Some((child, started)) = self.player.as_mut() {
            if now.saturating_duration_since(*started) >= Duration::from_secs(5) {
                let _ = child.kill();
            }
            if !matches!(child.try_wait(), Ok(None)) {
                self.player = None;
            }
        }
        if self.visual.as_ref().is_some_and(|(start, pulses, _)| {
            now.saturating_duration_since(*start)
                >= Duration::from_millis((u64::from(*pulses) + 1) * 50)
        }) {
            self.visual = None;
        }
    }
    pub fn visual(&self, now: Instant) -> Option<(f32, Option<&WlSurface>)> {
        let (start, pulses, surface) = self.visual.as_ref()?;
        Some((
            opacity(now.saturating_duration_since(*start), *pulses),
            surface.as_ref(),
        ))
    }
    pub fn clear_visual(&mut self) {
        self.visual = None;
    }
    fn audible_enabled(&self) -> bool {
        self.policy_ready && self.policy.audible && self.policy.event_sounds
    }
    fn stop_player(&mut self) {
        if let Some((mut child, _)) = self.player.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
impl Drop for Bell {
    fn drop(&mut self) {
        self.stop_player();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn player_uses_private_display_without_changing_process_environment() {
        let ambient = std::env::var_os("WAYLAND_DISPLAY");
        let mut bell = Bell::default();
        bell.set_wayland_display("tuna-private-bell");
        let command = bell.player_command();
        assert!(command.get_envs().any(|(key, value)| {
            key == "WAYLAND_DISPLAY" && value == Some(std::ffi::OsStr::new("tuna-private-bell"))
        }));
        assert_eq!(std::env::var_os("WAYLAND_DISPLAY"), ambient);
    }

    #[test]
    fn global_rate_limit_and_double_pulse_threshold() {
        let now = Instant::now();
        assert_eq!(flashes(None, now), 2);
        assert_eq!(flashes(Some(now), now + Duration::from_millis(499)), 0);
        assert_eq!(flashes(Some(now), now + Duration::from_millis(500)), 1);
        assert_eq!(flashes(Some(now), now + Duration::from_millis(2999)), 1);
        assert_eq!(flashes(Some(now), now + Duration::from_millis(3000)), 2);
    }
    #[test]
    fn pulse_opacity_is_bounded_and_expires() {
        assert_eq!(opacity(Duration::ZERO, 2), 0.0);
        assert_eq!(opacity(Duration::from_millis(50), 2), 192.0 / 255.0);
        assert_eq!(opacity(Duration::from_millis(100), 2), 0.0);
        assert_eq!(
            opacity(Duration::from_millis(25), 2),
            (192.0 / 255.0) * 0.25
        );
        assert_eq!(
            opacity(Duration::from_millis(125), 2),
            (192.0 / 255.0) * 0.25
        );
        assert_eq!(opacity(Duration::from_millis(150), 2), 0.0);
        assert_eq!(opacity(Duration::from_millis(200), 2), 0.0);
        assert_eq!(opacity(Duration::from_millis(100), 1), 0.0);
    }
    #[test]
    fn first_bell_is_silent_until_trusted_policy_arrives() {
        let mut bell = Bell::default();
        assert!(!bell.audible_enabled());
        bell.ring(None);
        assert!(bell.player.is_none());
        assert!(bell.visual(Instant::now()).is_none());
        assert_eq!(bell.rings, 1);
        bell.set_policy(BellPolicy {
            visual: true,
            ..Default::default()
        });
        assert!(bell.audible_enabled());
        bell.set_policy(BellPolicy {
            audible: false,
            visual: true,
            ..Default::default()
        });
        bell.ring(None);
        assert!(bell.visual(Instant::now()).is_some());
        assert!(bell.player.is_none());
    }
    #[test]
    fn disabling_visual_policy_cancels_inflight_flash() {
        let mut bell = Bell::default();
        bell.set_policy(BellPolicy {
            visual: true,
            audible: false,
            ..Default::default()
        });
        bell.ring(None);
        assert!(bell.visual(Instant::now()).is_some());
        bell.set_policy(BellPolicy {
            audible: false,
            ..Default::default()
        });
        assert!(bell.visual(Instant::now()).is_none());
        assert_eq!(bell.rings, 1);
    }
}
