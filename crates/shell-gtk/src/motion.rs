//! The shell's live copy of GNOME's motion policy
//! ([`tuna_shell_control::MotionPolicy`]), shared with the compositor
//! through `InputSettings` and read by the shell's own animations.
use std::cell::Cell;

use gtk4 as gtk;
use tuna_shell_control::MotionPolicy;

thread_local! {
    static CURRENT: Cell<MotionPolicy> = Cell::new(MotionPolicy::default());
}

/// The policy most recently derived from GNOME's settings.
pub fn current() -> MotionPolicy {
    CURRENT.with(Cell::get)
}

/// Install a freshly derived policy; GTK's own transitions follow it.
/// GTK has no fade-only mode, so its transitions run only at full motion
/// (they slide as often as they fade).
pub fn set(policy: MotionPolicy) {
    CURRENT.with(|c| c.set(policy));
    if let Some(gtk_settings) = gtk::Settings::default() {
        if gtk_settings.is_gtk_enable_animations() != policy.allows_motion() {
            gtk_settings.set_gtk_enable_animations(policy.allows_motion());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuna_shell_control::gsettings::derive_motion;

    #[test]
    fn the_current_policy_updates_live() {
        assert_eq!(current(), MotionPolicy::default());
        let reduced = derive_motion(Some(true), Some("reduce"), Some("3"));
        CURRENT.with(|c| c.set(reduced));
        assert_eq!(current(), reduced);
        assert!(!current().allows_motion() && current().allows_fades());
        CURRENT.with(|c| c.set(MotionPolicy::default()));
        assert!(current().allows_motion());
    }
}
