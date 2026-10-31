//! The runtime's side of GNOME's on-screen keyboard (#349): publish the
//! input-method focus to the shell, and type what the shell's keyboard
//! sends as gnome-shell 51's `KeyboardController` does.

use smithay::input::keyboard::xkb;
use tuna_shell_control::OskInput;

use crate::osk::{keysyms_for, level_modifier, pick_keycode};

use super::Runtime;

/// One keysym the keyboard holds down: its key and the level modifier
/// pressed around it, both in evdev codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Held {
    keysym: u32,
    key: u32,
    modifier: Option<u32>,
}

/// Keys the on-screen keyboard holds (mutter's per-device button count).
#[derive(Debug, Default)]
pub(super) struct RuntimeOsk {
    held: Vec<Held>,
}

/// gnome-shell 51 keyboard.js `KEYBOARD_ANIMATION_TIME`.
const KEYBOARD_ANIMATION_TIME_MS: f64 = 150.0;

impl Runtime {
    /// Hand the shell what its keyboard follows: the focused field (none
    /// while locked, so nothing about the session leaks to the lock
    /// screen), a pending input-panel request and touch-mode auto-show.
    pub(super) fn publish_on_screen_keyboard(&mut self) {
        let locked = self.is_locked();
        let focus = self
            .state
            .seat
            .get_keyboard()
            .and_then(|k| k.current_focus());
        let inputs = &mut self.state.protocols.text_inputs;
        let show = inputs.take_show(focus.as_ref()) && !locked;
        let field = if locked {
            None
        } else {
            inputs.focus(focus.as_ref())
        };
        #[allow(unused_mut)]
        let mut touch_auto = false;
        #[cfg(feature = "drm")]
        if let super::Backend::Drm(drm) = &self.backend {
            touch_auto = drm.touch_mode().auto();
        }
        self.control.set_on_screen_keyboard(field, show, touch_auto);
        self.slide_window_for_keyboard(focus.as_ref(), locked);
    }

    /// gnome-shell 51 `Keyboard.setCursorLocation`: the session keyboard
    /// (the mapped `tuna-osk` layer surface, `keyboardBox`) moves the
    /// focused field's window up when the field's cursor would sit under
    /// it, easing over `KEYBOARD_ANIMATION_TIME`.
    fn slide_window_for_keyboard(
        &mut self,
        focus: Option<&smithay::reexports::wayland_server::protocol::wl_surface::WlSurface>,
        locked: bool,
    ) {
        let keyboard = if locked {
            None
        } else {
            self.session_keyboard_box()
        };
        let cursor = focus.and_then(|surface| {
            let rect = self
                .state
                .protocols
                .text_inputs
                .cursor_rectangle(Some(surface))?;
            self.manager.field_cursor(surface, rect)
        });
        let duration = std::time::Duration::from_secs_f64(
            self.input_settings
                .motion
                .adjust_ms(KEYBOARD_ANIMATION_TIME_MS)
                .max(0.0)
                / 1000.0,
        );
        let now = self.animation_clock.now();
        self.manager.osk_slide(keyboard, cursor, now, duration);
        self.manager.step_osk_slide(now);
    }

    /// The session keyboard's top edge and height while it is mapped.
    fn session_keyboard_box(&self) -> Option<crate::osk::KeyboardBox> {
        let surface = self
            .state
            .panel_surfaces()
            .into_iter()
            .find(|record| record.namespace == tuna_shell_control::OSK_NAMESPACE)?
            .surface;
        let (_, (_, top), _) = crate::layer::layer_layout(&self.state)
            .into_iter()
            .find(|(placed, _, _)| *placed == surface)?;
        let height = self
            .state
            .layer_surfaces()
            .into_iter()
            .find(|layer| layer.wl_surface() == &surface)?
            .current_state()
            .size?
            .h;
        (height > 0).then_some(crate::osk::KeyboardBox { top, height })
    }

    /// Type one action from the shell's keyboard. The hub refuses these
    /// while locked; a lock that lands between command and tick drops it.
    pub(super) fn osk_input(&mut self, input: OskInput) {
        if self.is_locked() {
            return;
        }
        match input {
            OskInput::Keyval { keysym, pressed } => self.osk_keyval(keysym, pressed),
            OskInput::Commit { text, modifiers } => {
                // `KeyboardController.commit`: without input-method focus
                // (X11 clients) or with modifiers latched, raw keys;
                // otherwise the input method commits the string.
                let focus = self
                    .state
                    .seat
                    .get_keyboard()
                    .and_then(|k| k.current_focus());
                if modifiers.is_empty()
                    && self
                        .state
                        .protocols
                        .text_inputs
                        .commit(focus.as_ref(), &text)
                {
                    return;
                }
                for modifier in &modifiers {
                    self.osk_keyval(*modifier, true);
                }
                for keysym in keysyms_for(&text) {
                    self.osk_keyval(keysym, true);
                    self.osk_keyval(keysym, false);
                }
                for modifier in &modifiers {
                    self.osk_keyval(*modifier, false);
                }
            }
        }
    }

    /// mutter's `notify_keyval`: the keysym's key in the current layout,
    /// with Shift or AltGr around it for its level. A keysym the layout
    /// lacks types nothing, as in mutter; repeated presses and stray
    /// releases are ignored like its button count.
    fn osk_keyval(&mut self, keysym: u32, pressed: bool) {
        let time = (crate::state::system_millis() & u64::from(u32::MAX)) as u32;
        if !pressed {
            let Some(index) = self.osk.held.iter().position(|h| h.keysym == keysym) else {
                return;
            };
            let held = self.osk.held.remove(index);
            self.manager
                .keyboard_key(&mut self.state, held.key, false, time);
            if let Some(modifier) = held.modifier {
                self.manager
                    .keyboard_key(&mut self.state, modifier, false, time);
            }
            return;
        }
        if self.osk.held.iter().any(|h| h.keysym == keysym) {
            return;
        }
        let Some(keyboard) = self.state.seat.get_keyboard() else {
            return;
        };
        let picked = keyboard.with_xkb_state(&mut self.state, |context| {
            let xkb = context.xkb().lock().ok()?;
            // SAFETY: keymap and state are borrowed only while the XKB
            // mutex is held; no reference escapes this closure.
            let (keymap, layout) = unsafe {
                (
                    xkb.keymap(),
                    xkb.state().serialize_layout(xkb::STATE_LAYOUT_EFFECTIVE),
                )
            };
            let (key, level) = pick_keycode(keymap, layout, keysym)?;
            let modifier = level_modifier(level)
                .and_then(|modifier| pick_keycode(keymap, layout, modifier))
                .map(|(code, _)| code);
            Some((key, modifier))
        });
        let Some((key, modifier)) = picked else {
            eprintln!(
                "tuna-compositor: osk: no keycode for keysym {keysym:#x} in the current layout"
            );
            return;
        };
        // Smithay codes are XKB (evdev + 8); the manager takes evdev.
        let held = Held {
            keysym,
            key: key.saturating_sub(8),
            modifier: modifier.map(|code| code.saturating_sub(8)),
        };
        if let Some(modifier) = held.modifier {
            self.manager
                .keyboard_key(&mut self.state, modifier, true, time);
        }
        self.manager
            .keyboard_key(&mut self.state, held.key, true, time);
        self.osk.held.push(held);
    }

    /// Lift every key the keyboard holds (the session locked, or the
    /// shell went away mid-press), as mutter disposes its virtual device.
    pub(super) fn release_osk_keys(&mut self) {
        for keysym in self.osk.held.iter().map(|h| h.keysym).collect::<Vec<_>>() {
            self.osk_keyval(keysym, false);
        }
    }
}
