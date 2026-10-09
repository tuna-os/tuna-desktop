//! The runtime's side of GNOME's accessibility input aids (#350):
//! backend input passes through the keyboard aids ([`InputAids`]) and
//! then the pointer aids ([`PointerAids`]) before the lock screen,
//! keybindings or any client sees it, and what comes out is routed as if
//! the backend had sent it.

use std::time::Instant;

use tuna_shell_control::Message;

use crate::input_aids::{AidEffect, InputAids};
use crate::pointer_aids::{PointerAids, PointerEffect};
use crate::windows::ManagerInput;

use super::Runtime;

/// The aids' engines and the clock they run on.
pub(super) struct RuntimeAids {
    engine: InputAids,
    pointer: PointerAids,
    epoch: Instant,
}

impl Default for RuntimeAids {
    fn default() -> Self {
        Self {
            engine: InputAids::new(),
            pointer: PointerAids::new(),
            epoch: Instant::now(),
        }
    }
}

impl RuntimeAids {
    fn now_ms(&self) -> u64 {
        self.epoch
            .elapsed()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX)
    }

    fn active(&self) -> bool {
        self.engine.active() || self.pointer.active()
    }
}

impl Runtime {
    /// One event from the hardware or nested backend, filtered by the
    /// aids first. Remote-desktop input does not come this way: GNOME's
    /// aids serve the person at the keyboard and pointer.
    pub(super) fn on_backend_input(&mut self, input: ManagerInput) {
        if !self.input_aids.engine.active() {
            self.route_pointer_aids(input);
            return;
        }
        let now = self.input_aids.now_ms();
        let num_lock = self.manager.num_lock();
        let role = match input {
            ManagerInput::Key { keycode, .. } => self
                .manager
                .base_keysym(&mut self.state, keycode)
                .map(canonical_role)
                .unwrap_or(u32::MAX),
            _ => u32::MAX,
        };
        let effects = self
            .input_aids
            .engine
            .process_classified(input, now, num_lock, role);
        self.apply_aid_effects(effects);
    }

    pub(super) fn reset_input_aid_ownership(&mut self) {
        // Roll back direct strip manipulation before synthetic releases
        // can accidentally commit it during lock/keymap ownership changes.
        self.manager.cancel_strip_pointer(&mut self.state);
        self.dnd_escape_held = false;
        let now = self.input_aids.now_ms();
        let effects = self.input_aids.engine.reset_ownership(now);
        self.apply_aid_effects(effects);
        let effects = self.input_aids.pointer.reset_ownership(now);
        self.apply_pointer_effects(effects);
    }

    pub(super) fn switch_input_source_with_aid_reset(&mut self, backward: bool) {
        self.reset_input_aid_ownership();
        self.manager.switch_input_source(&mut self.state, backward);
    }

    /// The aids' timers (Slow Keys, the Shift-hold shortcut, Mouse Keys
    /// motion, hover and secondary clicks), run every tick.
    pub(super) fn poll_input_aids(&mut self) {
        if !self.input_aids.active() {
            return;
        }
        let now = self.input_aids.now_ms();
        if self.input_aids.engine.active() {
            let effects = self.input_aids.engine.poll(now);
            self.apply_aid_effects(effects);
        }
        if self.input_aids.pointer.active() {
            let pointer = self.manager.pointer_pos();
            let effects = self.input_aids.pointer.poll(now, pointer);
            self.apply_pointer_effects(effects);
        }
    }

    /// GNOME's `org.gnome.desktop.a11y.keyboard` and `.mouse`, live from
    /// the shell.
    pub(super) fn configure_input_aids(
        &mut self,
        keyboard: &tuna_shell_control::KeyboardAids,
        pointer: &tuna_shell_control::PointerAids,
    ) {
        let now = self.input_aids.now_ms();
        let effects = self.input_aids.engine.configure(keyboard, now);
        self.apply_aid_effects(effects);
        let effects = self.input_aids.pointer.configure(pointer, now);
        self.apply_pointer_effects(effects);
    }

    /// The hover-click chooser picked the next dwell's click.
    pub(super) fn set_dwell_click_type(&mut self, click: tuna_shell_control::DwellClick) {
        let effects = self.input_aids.pointer.set_click_type(click);
        self.apply_pointer_effects(effects);
    }

    /// The aids for the state file; nothing while locked.
    pub(super) fn input_aids_summary(&self) -> serde_json::Value {
        if self.is_locked() {
            return serde_json::Value::Null;
        }
        let mut summary = self.input_aids.engine.summary();
        summary["pointer"] = self.input_aids.pointer.summary();
        summary
    }

    fn apply_aid_effects(&mut self, effects: Vec<AidEffect>) {
        for effect in effects {
            match effect {
                AidEffect::Input(input) => {
                    let before = matches!(input, ManagerInput::Key { .. })
                        .then(|| self.manager.keyboard_led_state())
                        .flatten();
                    self.route_pointer_aids(input);
                    let after = self.manager.keyboard_led_state();
                    let changed = matches!((before, after), (Some(a), Some(b)) if a != b);
                    // Smithay updates the seat XKB state synchronously before
                    // returning from input(), including intercepted keys.
                    let bells = self.input_aids.engine.led_changed(changed);
                    self.apply_aid_effects(bells);
                }
                AidEffect::PointerMove { dx, dy, time } => {
                    let pos = self.manager.pointer_pos();
                    let pos = self.clamp_to_outputs(pos.x + dx, pos.y + dy);
                    self.route_pointer_aids(ManagerInput::Motion {
                        pos: pos.into(),
                        time,
                    });
                }
                AidEffect::Bell => self.state.ring_bell(),
                AidEffect::Toggled { aid, enabled } => {
                    eprintln!("tuna-compositor: keyboard aid {aid:?} switched enabled={enabled}");
                    self.control
                        .queue_message(Message::KeyboardAidToggled { aid, enabled });
                }
            }
        }
    }

    /// Pointer events pass the pointer aids on their way in.
    fn route_pointer_aids(&mut self, input: ManagerInput) {
        let pointer_event = matches!(
            input,
            ManagerInput::Motion { .. } | ManagerInput::Button { .. }
        );
        if !pointer_event || !self.input_aids.pointer.active() {
            self.on_manager_input(input);
            return;
        }
        let now = self.input_aids.now_ms();
        let effects = self.input_aids.pointer.process(input, now);
        self.apply_pointer_effects(effects);
    }

    fn apply_pointer_effects(&mut self, effects: Vec<PointerEffect>) {
        for effect in effects {
            match effect {
                PointerEffect::Input(input) => self.on_manager_input(input),
                PointerEffect::TimeoutStarted {
                    kind,
                    duration_ms,
                    pos,
                } => self.control.queue_message(Message::PointerTimeout {
                    kind,
                    duration_ms: Some(duration_ms),
                    clicked: false,
                    x: pos.x.round() as i32,
                    y: pos.y.round() as i32,
                }),
                PointerEffect::TimeoutStopped { kind, clicked } => {
                    let pos = self.manager.pointer_pos();
                    self.control.queue_message(Message::PointerTimeout {
                        kind,
                        duration_ms: None,
                        clicked,
                        x: pos.x.round() as i32,
                        y: pos.y.round() as i32,
                    });
                }
                PointerEffect::ClickType(click) => {
                    self.control
                        .queue_message(Message::DwellClickType { click });
                }
            }
        }
    }

    /// Keep a Mouse Keys move on some output: the nearest point of the
    /// nearest output when it would leave them all.
    fn clamp_to_outputs(&self, x: f64, y: f64) -> (f64, f64) {
        let mut best = (x, y);
        let mut best_distance = f64::INFINITY;
        for output in &self.state.outputs {
            let (left, top) = (f64::from(output.loc.0), f64::from(output.loc.1));
            let right = left + f64::from(output.size.w) - 1.0;
            let bottom = top + f64::from(output.size.h) - 1.0;
            let clamped = (
                x.clamp(left, right.max(left)),
                y.clamp(top, bottom.max(top)),
            );
            let distance = (clamped.0 - x).powi(2) + (clamped.1 - y).powi(2);
            if distance < best_distance {
                best_distance = distance;
                best = clamped;
            }
        }
        best
    }
}

/// Interpret configured keysyms rather than assuming Linux key locations.
fn canonical_role(sym: u32) -> u32 {
    use smithay::input::keyboard::keysyms::{
        KEY_Alt_L as ALT_L, KEY_Alt_R as ALT_R, KEY_Caps_Lock as CAPS_LOCK,
        KEY_Control_L as CONTROL_L, KEY_Control_R as CONTROL_R, KEY_Hyper_L as HYPER_L,
        KEY_Hyper_R as HYPER_R, KEY_ISO_Level3_Shift as ISO_LEVEL3_SHIFT, KEY_KP_Add as KP_ADD,
        KEY_KP_Begin as KP_BEGIN, KEY_KP_Decimal as KP_DECIMAL, KEY_KP_Delete as KP_DELETE,
        KEY_KP_Divide as KP_DIVIDE, KEY_KP_Down as KP_DOWN, KEY_KP_End as KP_END,
        KEY_KP_Home as KP_HOME, KEY_KP_Insert as KP_INSERT, KEY_KP_Left as KP_LEFT,
        KEY_KP_Multiply as KP_MULTIPLY, KEY_KP_Page_Down as KP_PAGE_DOWN,
        KEY_KP_Page_Up as KP_PAGE_UP, KEY_KP_Right as KP_RIGHT, KEY_KP_Subtract as KP_SUBTRACT,
        KEY_KP_Up as KP_UP, KEY_Meta_L as META_L, KEY_Meta_R as META_R, KEY_Num_Lock as NUM_LOCK,
        KEY_Shift_L as SHIFT_L, KEY_Shift_Lock as SHIFT_LOCK, KEY_Shift_R as SHIFT_R,
        KEY_Super_L as SUPER_L, KEY_Super_R as SUPER_R, KEY_KP_0 as KP_0, KEY_KP_1 as KP_1,
        KEY_KP_2 as KP_2, KEY_KP_3 as KP_3, KEY_KP_4 as KP_4, KEY_KP_5 as KP_5, KEY_KP_6 as KP_6,
        KEY_KP_7 as KP_7, KEY_KP_8 as KP_8, KEY_KP_9 as KP_9,
    };
    match sym {
        SHIFT_L => 42,
        SHIFT_R => 54,
        CONTROL_L => 29,
        CONTROL_R => 97,
        ALT_L | ALT_R => 56,
        ISO_LEVEL3_SHIFT => 100,
        SUPER_L | SUPER_R => 125,
        META_L | META_R => 200,
        HYPER_L | HYPER_R => 201,
        CAPS_LOCK | SHIFT_LOCK => 58,
        NUM_LOCK => 69,
        KP_HOME | KP_7 => 71,
        KP_UP | KP_8 => 72,
        KP_PAGE_UP | KP_9 => 73,
        KP_LEFT | KP_4 => 75,
        KP_RIGHT | KP_6 => 77,
        KP_END | KP_1 => 79,
        KP_DOWN | KP_2 => 80,
        KP_PAGE_DOWN | KP_3 => 81,
        KP_BEGIN | KP_5 => 76,
        KP_ADD => 78,
        KP_INSERT | KP_0 => 82,
        KP_DELETE | KP_DECIMAL => 83,
        KP_DIVIDE => 98,
        KP_MULTIPLY => 55,
        KP_SUBTRACT => 74,
        _ => u32::MAX,
    }
}

#[cfg(test)]
mod tests {
    use super::canonical_role;
    use smithay::input::keyboard::keysyms::*;

    #[test]
    fn configured_symbols_determine_aid_roles() {
        assert_eq!(canonical_role(KEY_Control_L), 29);
        assert_eq!(canonical_role(KEY_Alt_R), 56);
        assert_eq!(canonical_role(KEY_Meta_L), 200);
        assert_eq!(canonical_role(KEY_Hyper_R), 201);
        assert_eq!(canonical_role(KEY_KP_Up), 72);
        assert_eq!(canonical_role(KEY_ISO_Level3_Shift), 100);
        assert_eq!(canonical_role(KEY_a), u32::MAX);
    }
}

pub(super) fn keymap_changed(
    old: &tuna_shell_control::InputSettings,
    new: &tuna_shell_control::InputSettings,
) -> bool {
    old.xkb_layout != new.xkb_layout
        || old.xkb_variant != new.xkb_variant
        || old.xkb_options != new.xkb_options
}

#[cfg(test)]
mod keymap_tests {
    use super::keymap_changed;
    use tuna_shell_control::InputSettings;

    #[test]
    fn semantic_map_changes_reset_but_repeat_and_aids_preferences_do_not() {
        let old = InputSettings::default();
        for new in [
            InputSettings {
                xkb_layout: "de".into(),
                ..old.clone()
            },
            InputSettings {
                xkb_variant: "dvorak".into(),
                ..old.clone()
            },
            InputSettings {
                xkb_options: "ctrl:swapcaps".into(),
                ..old.clone()
            },
        ] {
            assert!(keymap_changed(&old, &new));
        }
        let mut new = old.clone();
        new.repeat_delay_ms += 100;
        new.keyboard_aids.sticky = true;
        assert!(!keymap_changed(&old, &new));
    }
}
