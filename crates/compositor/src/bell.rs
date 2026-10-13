//! The xdg-system-bell: GNOME's bell sound gated by the shell's live
//! bell policy, plus the visual flash state a frame draws.
//!
//! Audible rings play `bell-window-system` through libcanberra's player
//! when `audible-bell` and `event-sounds` are both on, at most one at a
//! time; a burst of rings is one sound. Visual rings record a Mutter-style
//! flash (fullscreen, or the ringing window for `frame-flash`) that the
//! render path consumes through [`Bell::visual`]. Policy arrives with the
//! shell's input settings; until then GNOME's defaults apply.
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use std::time::{Duration, Instant};
use tuna_shell_control::BellPolicy;

/// GNOME's bell sound (Mutter's `meta_bell_notify`).
pub const BELL_SOUND: &str = "bell-window-system";

/// The system bell: rings counted, sounds played through libcanberra's
/// `canberra-gtk-play` when it is installed, at most one at a time.
#[derive(Default)]
pub(crate) struct Bell {
    rings: u64,
    flashes: u64,
    policy: BellPolicy,
    last_audible: Option<Instant>,
    last_visual: Option<Instant>,
    visual: Option<(Instant, u8, Option<WlSurface>)>,
    player: Option<(std::process::Child, Instant)>,
}

/// How many flash pulses a ring earns: none inside 500 ms of the last
/// flash (one burst is one flash), one inside 3 s, else two.
fn pulse_count(last: Option<Instant>, now: Instant) -> u8 {
    match last.map(|last| now.saturating_duration_since(last)) {
        Some(gap) if gap < Duration::from_millis(500) => 0,
        Some(gap) if gap < Duration::from_secs(3) => 1,
        _ => 2,
    }
}

/// A 50 ms quad-in leg followed by `pulses` alternating legs.
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
    /// Times the system bell rang, audible or not.
    pub(crate) fn rings(&self) -> u64 {
        self.rings
    }

    /// Visual flashes recorded while `visual-bell` was on.
    pub(crate) fn flashes(&self) -> u64 {
        self.flashes
    }

    /// The shell's latest bell policy. Disabling the visual bell drops
    /// an in-flight flash; disabling sound stops the player.
    pub(crate) fn set_policy(&mut self, policy: BellPolicy) {
        if !policy.visual {
            self.visual = None;
        }
        self.policy = policy;
        if !self.audible_enabled() {
            self.stop_player();
        }
    }

    pub(crate) fn ring(&mut self, surface: Option<WlSurface>) {
        self.rings = self.rings.saturating_add(1);
        let now = Instant::now();
        self.tick(now);
        if self.policy.visual {
            let pulses = pulse_count(self.last_visual, now);
            if pulses > 0 {
                self.last_visual = Some(now);
                self.flashes = self.flashes.saturating_add(1);
                self.visual = Some((
                    now,
                    pulses,
                    self.policy.frame_flash.then_some(surface).flatten(),
                ));
            }
        }
        if !self.audible_enabled() {
            return;
        }
        // A burst of rings is one sound, as a held key would otherwise
        // queue a sound per repeat.
        if self
            .last_audible
            .is_some_and(|last| now.duration_since(last) < Duration::from_millis(100))
        {
            return;
        }
        self.last_audible = Some(now);
        if self.player.is_some() {
            return;
        }
        if std::env::var_os("TUNA_BELL").is_some_and(|v| v == "0") {
            return;
        }
        let theme = format!(
            "canberra.xdg-theme.name={}",
            self.policy.theme.chars().take(256).collect::<String>()
        );
        self.player = std::process::Command::new("canberra-gtk-play")
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

    /// Reap the player (killing it past five seconds) and drop an
    /// expired flash. Rings call this; the frame path calls it too once
    /// it draws the flash.
    pub(crate) fn tick(&mut self, now: Instant) {
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
                >= Duration::from_millis(u64::from(*pulses) + 1) * 50
        }) {
            self.visual = None;
        }
    }

    /// The live flash: its opacity now and the window it targets, if
    /// `visual-bell-type` is `frame-flash` (`None` is fullscreen).
    pub(crate) fn visual(&self, now: Instant) -> Option<(f32, Option<&WlSurface>)> {
        let (start, pulses, surface) = self.visual.as_ref()?;
        Some((
            opacity(now.saturating_duration_since(*start), *pulses),
            surface.as_ref(),
        ))
    }

    /// Drop an in-flight flash (the session locked over it).
    pub(crate) fn clear_visual(&mut self) {
        self.visual = None;
    }

    fn audible_enabled(&self) -> bool {
        self.policy.audible && self.policy.event_sounds
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

    fn silent() -> BellPolicy {
        BellPolicy {
            audible: false,
            ..BellPolicy::default()
        }
    }

    #[test]
    fn gnome_default_policy_rings_audible_without_a_flash() {
        let bell = Bell::default();
        assert!(bell.audible_enabled());
        assert_eq!(bell.rings(), 0);
        assert_eq!(bell.flashes(), 0);
    }

    #[test]
    fn disabled_audible_counts_rings_but_spawns_no_player() {
        let mut bell = Bell::default();
        bell.set_policy(silent());
        assert!(!bell.audible_enabled());
        bell.ring(None);
        bell.ring(None);
        assert_eq!(bell.rings(), 2);
        assert!(bell.player.is_none());
        assert!(bell.visual(Instant::now()).is_none());
    }

    #[test]
    fn event_sounds_off_silences_the_bell_like_gnome() {
        let mut bell = Bell::default();
        bell.set_policy(BellPolicy {
            event_sounds: false,
            ..BellPolicy::default()
        });
        assert!(!bell.audible_enabled());
        bell.ring(None);
        assert_eq!(bell.rings(), 1);
        assert!(bell.player.is_none());
    }

    #[test]
    fn visual_rings_flash_and_bursts_share_one_flash() {
        let mut bell = Bell::default();
        bell.set_policy(BellPolicy {
            audible: false,
            visual: true,
            ..BellPolicy::default()
        });
        bell.ring(None);
        assert_eq!(bell.flashes(), 1);
        assert!(bell.visual(Instant::now()).is_some());
        bell.ring(None);
        assert_eq!(bell.rings(), 2);
        assert_eq!(bell.flashes(), 1);
    }

    #[test]
    fn disabling_visual_policy_cancels_inflight_flash() {
        let mut bell = Bell::default();
        bell.set_policy(BellPolicy {
            audible: false,
            visual: true,
            ..BellPolicy::default()
        });
        bell.ring(None);
        assert!(bell.visual(Instant::now()).is_some());
        bell.set_policy(silent());
        assert!(bell.visual(Instant::now()).is_none());
        assert_eq!(bell.rings(), 1);
    }

    #[test]
    fn flash_pulse_thresholds_match_mutter() {
        let now = Instant::now();
        assert_eq!(pulse_count(None, now), 2);
        assert_eq!(pulse_count(Some(now), now + Duration::from_millis(499)), 0);
        assert_eq!(pulse_count(Some(now), now + Duration::from_millis(500)), 1);
        assert_eq!(pulse_count(Some(now), now + Duration::from_millis(2999)), 1);
        assert_eq!(pulse_count(Some(now), now + Duration::from_millis(3000)), 2);
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
    fn expired_flash_clears_on_tick() {
        let mut bell = Bell::default();
        bell.set_policy(BellPolicy {
            audible: false,
            visual: true,
            ..BellPolicy::default()
        });
        bell.ring(None);
        let start = Instant::now();
        bell.tick(start + Duration::from_millis(100));
        assert!(bell.visual(start + Duration::from_millis(100)).is_some());
        bell.tick(start + Duration::from_secs(1));
        assert!(bell.visual(start + Duration::from_secs(1)).is_none());
    }
}
