//! Native xdg and X11 bell consumers with live policy and bounded player lifetime.
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
    visual: Option<Visual>,
    player: Option<(std::process::Child, Instant)>,
    /// X server time of the last admitted X11 bell (Mutter's `last_bell_time`).
    last_x11_time: u32,
}

struct Visual {
    started: Instant,
    pulses: u8,
    leg: u8,
    surface: Option<WlSurface>,
}

impl Visual {
    fn sample(&mut self, now: Instant) -> Option<f32> {
        let elapsed = now.saturating_duration_since(self.started);
        let end = Duration::from_millis((u64::from(self.leg) + 1) * 50);
        if elapsed >= end {
            if self.leg == self.pulses {
                return None;
            }
            // ClutterTimeline emits each leg's endpoint before reversing,
            // carrying overflow into the next leg. Modulo wall time alone
            // can miss every peak on a delayed frame, even the whole flash.
            self.leg += 1;
            return Some(if self.leg % 2 == 1 {
                192.0 / 255.0
            } else {
                0.0
            });
        }
        Some(opacity(elapsed, self.pulses))
    }
}

fn flashes(last: Option<Instant>, now: Instant) -> u8 {
    match last.map(|last| now.saturating_duration_since(last)) {
        Some(gap) if gap < Duration::from_millis(500) => 0,
        Some(gap) if gap < Duration::from_secs(3) => 1,
        _ => 2,
    }
}

/// Mutter's `XSERVER_TIME_IS_BEFORE`: wrapping X server timestamps, where 0
/// is "never".
fn xserver_time_is_before(time1: u32, time2: u32) -> bool {
    time1 == 0
        || (time2 != 0
            && ((time1 < time2 && time2 - time1 < u32::MAX / 2)
                || (time1 > time2 && time1 - time2 > u32::MAX / 2)))
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
                self.visual = Some(Visual {
                    started: now,
                    pulses,
                    leg: 0,
                    surface: self.policy.frame_flash.then_some(surface).flatten(),
                });
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
        let mut command = std::process::Command::new("canberra-gtk-play");
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
    }
    pub fn visual_active(&self) -> bool {
        self.visual.is_some()
    }
    pub fn visual(&mut self, now: Instant) -> Option<(f32, Option<&WlSurface>)> {
        let Some(alpha) = self.visual.as_mut()?.sample(now) else {
            self.visual = None;
            return None;
        };
        Some((alpha, self.visual.as_ref()?.surface.as_ref()))
    }
    /// The live policy, once the shell has sent it.
    pub fn policy(&self) -> Option<&BellPolicy> {
        self.policy_ready.then_some(&self.policy)
    }
    /// Mutter 51 `handle_other_xevent`: an X11 bell within 100 ms of the
    /// last one it handled is dropped (X server time).
    pub fn admit_x11(&mut self, time: u32) -> bool {
        if !xserver_time_is_before(self.last_x11_time, time.wrapping_sub(100)) {
            return false;
        }
        self.last_x11_time = time;
        true
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
    #[test]
    fn delayed_frames_present_leg_endpoints_before_reversing() {
        let started = Instant::now();
        let mut visual = Visual {
            started,
            pulses: 2,
            leg: 0,
            surface: None,
        };
        let sample = |visual: &mut Visual, ms| visual.sample(started + Duration::from_millis(ms));
        // The recorded X11 fullscreen failure sampled 22, 61, 83, 107
        // and 130 ms. Modulo time never reached the required black peak.
        assert_eq!(
            sample(&mut visual, 22),
            Some(opacity(Duration::from_millis(22), 2))
        );
        assert_eq!(sample(&mut visual, 61), Some(192.0 / 255.0));
        assert_eq!(
            sample(&mut visual, 83),
            Some(opacity(Duration::from_millis(83), 2))
        );
        assert_eq!(sample(&mut visual, 107), Some(0.0));
        assert_eq!(
            sample(&mut visual, 130),
            Some(opacity(Duration::from_millis(130), 2))
        );
        assert_eq!(sample(&mut visual, 150), None);
    }

    #[test]
    fn a_frame_delayed_past_the_flash_still_shows_the_peak_and_restores() {
        let started = Instant::now();
        let mut bell = Bell {
            visual: Some(Visual {
                started,
                pulses: 2,
                leg: 0,
                surface: None,
            }),
            ..Default::default()
        };
        // Player housekeeping and wake queries must not consume a frame.
        bell.tick(started + Duration::from_millis(200));
        assert!(bell.visual_active());
        assert_eq!(
            bell.visual(started + Duration::from_millis(200)).unwrap().0,
            192.0 / 255.0
        );
        assert_eq!(
            bell.visual(started + Duration::from_millis(216)).unwrap().0,
            0.0
        );
        assert!(bell.visual(started + Duration::from_millis(232)).is_none());
        assert!(!bell.visual_active());
    }

    #[test]
    fn x11_bells_within_100ms_of_the_last_handled_one_are_dropped() {
        let mut bell = Bell::default();
        assert!(bell.admit_x11(5_000));
        assert!(!bell.admit_x11(5_050));
        assert!(!bell.admit_x11(5_100));
        assert!(bell.admit_x11(5_101));
        // Dropped bells do not extend the window, as in Mutter.
        assert!(!bell.admit_x11(5_150));
        assert!(bell.admit_x11(5_202));
        // The server clock wraps.
        let mut bell = Bell::default();
        assert!(bell.admit_x11(u32::MAX - 20));
        assert!(!bell.admit_x11(30));
        assert!(bell.admit_x11(120));
    }
}
