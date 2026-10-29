//! Display power saving: `org.gnome.Mutter.DisplayConfig`'s read-write
//! `PowerSaveMode` (#355). GNOME Settings Daemon's power plugin blanks
//! the screen by writing 3 (off) on idle, on lid/blank actions and before
//! suspend, and writes 0 (on) when the user returns or the system resumes
//! (`gsd-power-manager.c`, `disable_monitors`/`enable_monitors`).
//!
//! Mirrors Mutter 51:
//! - `meta-monitor-manager.c` `power_save_mode_changed`: a write of -1
//!   (unknown) is ignored, an unsupported manager never leaves -1, and any
//!   other value becomes the mode; a write equal to the mode changes
//!   nothing.
//! - `meta-monitor-manager-native.c`: standby (1), suspend (2) and off (3)
//!   all disable the KMS device (every CRTC off); on (0) leaves it to the
//!   next frame to modeset the outputs back.
//! - `meta-onscreen-native.c`/`meta-renderer-native.c`: while the mode is
//!   not on nothing is scanned out and frames complete on a 100 ms dummy
//!   page flip, so clients keep receiving (slow) frame callbacks.
//! - `meta-monitor-manager.c`: orientation tracking is inhibited while the
//!   mode is not on.
//!
//! The event loop owns the applied mode; the D-Bus thread reads it
//! through [`PowerSaveShare`].

use std::sync::atomic::{AtomicI32, Ordering};
use std::time::Duration;

/// Mutter's `META_POWER_SAVE_ON`.
pub const ON: i32 = 0;
/// Mutter's `META_POWER_SAVE_UNSUPPORTED` (the D-Bus "unknown").
pub const UNSUPPORTED: i32 = -1;

/// Mutter's dummy page flip while powered off
/// (`meta_renderer_native_queue_power_save_page_flip`, `timeout_ms = 100`).
pub const FRAME_INTERVAL: Duration = Duration::from_millis(100);

/// What a requested mode does to the outputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    /// Ignored (-1, or equal to the current mode).
    Unchanged,
    /// Scan out again: the next frame modesets every output.
    PowerOn,
    /// Stop scanning out. `disable_outputs` is Mutter's native switch:
    /// only standby, suspend and off turn the CRTCs off.
    PowerOff { disable_outputs: bool },
}

/// The applied mode, owned by the event loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PowerSave {
    mode: i32,
}

impl Default for PowerSave {
    /// Mutter starts every monitor manager on.
    fn default() -> Self {
        Self { mode: ON }
    }
}

impl PowerSave {
    pub fn mode(self) -> i32 {
        self.mode
    }

    /// Whether outputs are scanned out (Mutter compares against ON).
    pub fn is_on(self) -> bool {
        self.mode == ON
    }

    /// Apply a `PowerSaveMode` write.
    pub fn request(&mut self, mode: i32) -> Transition {
        if mode == UNSUPPORTED || self.mode == UNSUPPORTED || mode == self.mode {
            return Transition::Unchanged;
        }
        self.mode = mode;
        if mode == ON {
            Transition::PowerOn
        } else {
            Transition::PowerOff {
                disable_outputs: matches!(mode, 1..=3),
            }
        }
    }
}

/// The applied mode, published by the loop for DisplayConfig's getter and
/// the orientation poller.
#[derive(Debug, Default)]
pub struct PowerSaveShare {
    mode: AtomicI32,
}

impl PowerSaveShare {
    pub fn mode(&self) -> i32 {
        self.mode.load(Ordering::SeqCst)
    }

    pub fn is_on(&self) -> bool {
        self.mode() == ON
    }

    pub fn publish(&self, mode: i32) {
        self.mode.store(mode, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_on_like_mutter() {
        let state = PowerSave::default();
        assert_eq!(state.mode(), ON);
        assert!(state.is_on());
        assert_eq!(PowerSaveShare::default().mode(), ON);
    }

    #[test]
    fn gsd_blank_and_unblank_turn_outputs_off_and_on() {
        let mut state = PowerSave::default();
        assert_eq!(
            state.request(3),
            Transition::PowerOff {
                disable_outputs: true
            }
        );
        assert_eq!(state.mode(), 3);
        assert!(!state.is_on());
        assert_eq!(state.request(3), Transition::Unchanged);
        assert_eq!(state.request(ON), Transition::PowerOn);
        assert!(state.is_on());
        assert_eq!(state.request(ON), Transition::Unchanged);
    }

    #[test]
    fn standby_and_suspend_disable_outputs_like_off() {
        for mode in [1, 2] {
            let mut state = PowerSave::default();
            assert_eq!(
                state.request(mode),
                Transition::PowerOff {
                    disable_outputs: true
                }
            );
            assert_eq!(state.mode(), mode);
        }
        let mut state = PowerSave::default();
        state.request(1);
        assert_eq!(
            state.request(3),
            Transition::PowerOff {
                disable_outputs: true
            }
        );
        assert_eq!(state.mode(), 3);
    }

    #[test]
    fn unknown_is_never_entered_or_left() {
        let mut state = PowerSave::default();
        assert_eq!(state.request(UNSUPPORTED), Transition::Unchanged);
        assert_eq!(state.mode(), ON);
        state.request(3);
        assert_eq!(state.request(UNSUPPORTED), Transition::Unchanged);
        assert_eq!(state.mode(), 3);
        let mut unsupported = PowerSave { mode: UNSUPPORTED };
        assert_eq!(unsupported.request(3), Transition::Unchanged);
        assert_eq!(unsupported.request(ON), Transition::Unchanged);
        assert_eq!(unsupported.mode(), UNSUPPORTED);
    }

    #[test]
    fn values_outside_mutters_switch_stop_scanout_without_disabling() {
        let mut state = PowerSave::default();
        assert_eq!(
            state.request(7),
            Transition::PowerOff {
                disable_outputs: false
            }
        );
        assert_eq!(state.mode(), 7);
        assert!(!state.is_on());
        assert_eq!(state.request(ON), Transition::PowerOn);
    }

    #[test]
    fn share_publishes_the_applied_mode() {
        let share = PowerSaveShare::default();
        share.publish(3);
        assert_eq!(share.mode(), 3);
        assert!(!share.is_on());
        share.publish(ON);
        assert!(share.is_on());
    }
}
