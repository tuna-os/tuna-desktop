//! The compositor's side of GNOME's on-screen keyboard (#349).
//!
//! GNOME Shell's keyboard (gnome-shell 51.0 `js/ui/keyboard.js`) follows
//! mutter's input-method focus: the text field whose client holds the
//! keyboard and has enabled its text input. Mutter tracks text-input-v3
//! itself whether or not IBus runs (`meta-wayland-text-input.c`), so
//! [`TextInputs`] watches every `zwp_text_input_v3` request before
//! Smithay sees it; the shell hears the result as
//! `Message::OnScreenKeyboard`. The keyboard then types through
//! `KeyboardController`'s two paths: a key's string is committed to the
//! focused text input, and keysyms are pressed on the seat keyboard via
//! the active layout ([`pick_keycode`], mutter's virtual input device),
//! which reaches X11 clients, terminals and shortcuts alike.
//! [`TouchMode`] is `KeyboardManager`'s touch-mode auto-show.

use smithay::input::keyboard::xkb;
use smithay::reexports::wayland_protocols::wp::text_input::zv3::server::zwp_text_input_v3::{
    self, ZwpTextInputV3,
};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::Resource;
use tuna_shell_control::TextFieldFocus;

/// `zwp_text_input_v3` hint `on_screen_input_provided` (version 2):
/// mutter's `CLUTTER_INPUT_CONTENT_HINT_INHIBIT_OSK`.
pub const HINT_INHIBIT_OSK: u32 = 0x400;

#[derive(Debug, Default, Clone, Copy)]
struct Pending {
    enable: Option<bool>,
    content: Option<(u32, u32)>,
    /// `set_cursor_rectangle`: surface-local x, y, width, height.
    cursor: Option<Option<(i32, i32, i32, i32)>>,
}

#[derive(Debug)]
struct Tracked {
    resource: ZwpTextInputV3,
    pending: Pending,
    enabled: bool,
    /// Committed (hint, purpose).
    content: (u32, u32),
    /// Committed cursor rectangle, surface-local (mutter's
    /// `meta_wayland_text_input_set_cursor_rect`).
    cursor: Option<(i32, i32, i32, i32)>,
    /// `commit` requests seen: the serial `done` must echo.
    commits: u32,
}

/// Every live text input with the state its client committed, as
/// mutter's `MetaWaylandTextInput` keeps it.
#[derive(Debug, Default)]
pub struct TextInputs {
    tracked: Vec<Tracked>,
    /// A focused text input was enabled again (input-panel ON).
    show: bool,
}

impl TextInputs {
    /// One request from a client, before Smithay handles it. `focus` is
    /// the surface holding keyboard focus: as in mutter
    /// (`client_matches_focus`), state from other clients never applies.
    pub fn request(
        &mut self,
        resource: &ZwpTextInputV3,
        request: &zwp_text_input_v3::Request,
        focus: Option<&WlSurface>,
    ) {
        let index = match self.tracked.iter().position(|t| t.resource == *resource) {
            Some(index) => index,
            None => {
                self.tracked.push(Tracked {
                    resource: resource.clone(),
                    pending: Pending::default(),
                    enabled: false,
                    content: (0, 0),
                    cursor: None,
                    commits: 0,
                });
                self.tracked.len() - 1
            }
        };
        let entry = &mut self.tracked[index];
        match request {
            zwp_text_input_v3::Request::Enable => {
                // Enabling resets the field's state (the protocol's rule).
                entry.pending = Pending {
                    enable: Some(true),
                    content: Some((0, 0)),
                    cursor: Some(None),
                };
            }
            zwp_text_input_v3::Request::Disable => entry.pending.enable = Some(false),
            zwp_text_input_v3::Request::SetContentType { hint, purpose } => {
                let hint = match hint {
                    smithay::reexports::wayland_server::WEnum::Value(hint) => hint.bits(),
                    smithay::reexports::wayland_server::WEnum::Unknown(bits) => *bits,
                };
                entry.pending.content = Some((hint, u32::from(*purpose)));
            }
            zwp_text_input_v3::Request::SetCursorRectangle {
                x,
                y,
                width,
                height,
            } => entry.pending.cursor = Some(Some((*x, *y, *width, *height))),
            zwp_text_input_v3::Request::Commit => {
                entry.commits = entry.commits.wrapping_add(1);
                let pending = std::mem::take(&mut entry.pending);
                let focused =
                    focus.is_some_and(|surface| surface.id().same_client_as(&resource.id()));
                if !focused {
                    return;
                }
                match pending.enable {
                    // Mutter: enabling an already focused version-1 text
                    // input asks for the input panel (GTK re-enables on a
                    // click into its field, `activate_osk`).
                    Some(true) if entry.enabled => self.show = true,
                    Some(enabled) => entry.enabled = enabled,
                    None => {}
                }
                if let Some(content) = pending.content {
                    entry.content = content;
                }
                if let Some(cursor) = pending.cursor {
                    entry.cursor = cursor;
                }
            }
            zwp_text_input_v3::Request::Destroy => self.destroyed(resource),
            _ => {}
        }
    }

    /// A text input object went away.
    pub fn destroyed(&mut self, resource: &ZwpTextInputV3) {
        self.tracked.retain(|t| t.resource != *resource);
    }

    /// Keyboard focus moved: text inputs are disabled until their client
    /// enables them again after `enter`, as text-input-v3 requires.
    pub fn focus_changed(&mut self) {
        for entry in &mut self.tracked {
            entry.enabled = false;
            entry.pending = Pending::default();
        }
        self.show = false;
    }

    fn active(&self, focus: Option<&WlSurface>) -> Option<&Tracked> {
        let focus = focus?;
        self.tracked.iter().find(|t| {
            t.enabled && t.resource.is_alive() && focus.id().same_client_as(&t.resource.id())
        })
    }

    /// The focused field (mutter's `ClutterInputFocus`), if any.
    pub fn focus(&self, focus: Option<&WlSurface>) -> Option<TextFieldFocus> {
        self.active(focus).map(|t| TextFieldFocus {
            purpose: t.content.1,
            hints: t.content.0,
        })
    }

    /// The focused field's cursor rectangle, local to `focus`
    /// (mutter's input-focus cursor location, GNOME's `FocusTracker`).
    pub fn cursor_rectangle(&self, focus: Option<&WlSurface>) -> Option<(i32, i32, i32, i32)> {
        self.active(focus)?.cursor
    }

    /// Take the input-panel ON request; only a focused field may make it.
    pub fn take_show(&mut self, focus: Option<&WlSurface>) -> bool {
        std::mem::take(&mut self.show) && self.active(focus).is_some()
    }

    /// Commit `text` to the focused field (`Main.inputMethod.commit`):
    /// `commit_string` then `done` with the serial its client expects.
    /// Returns false when no field has input-method focus.
    pub fn commit(&self, focus: Option<&WlSurface>, text: &str) -> bool {
        let Some(active) = self.active(focus) else {
            return false;
        };
        active.resource.commit_string(Some(text.to_owned()));
        active.resource.done(active.commits);
        true
    }
}

/// The keysyms a string types (`KeyboardController._getKeyvalsFromString`):
/// `Clutter.unicode_to_keysym` per character, which xkbcommon's
/// `utf32_to_keysym` mirrors.
pub fn keysyms_for(text: &str) -> Vec<u32> {
    text.chars()
        .map(|c| xkb::utf32_to_keysym(u32::from(c)).raw())
        .filter(|keysym| *keysym != 0)
        .collect()
}

/// mutter's `pick_keycode_for_keyval_in_current_group_in_impl`: the
/// first (keycode, shift level) in `layout` producing `keysym`, scanning
/// keycodes upward and levels within each.
pub fn pick_keycode(keymap: &xkb::Keymap, layout: u32, keysym: u32) -> Option<(u32, u32)> {
    let min = keymap.min_keycode().raw();
    let max = keymap.max_keycode().raw();
    // Mutter's loop stops before max_keycode; kept for identical picks.
    for code in min..max {
        let keycode = xkb::Keycode::new(code);
        for level in 0..keymap.num_levels_for_key(keycode, layout) {
            if keymap
                .key_get_syms_by_level(keycode, layout, level)
                .iter()
                .any(|sym| sym.raw() == keysym)
            {
                return Some((code, level));
            }
        }
    }
    None
}

/// The modifier keysym mutter presses around a key on `level`
/// (`apply_level_modifiers_in_impl`): Shift for level 1, AltGr
/// (ISO_Level3_Shift) for level 2, nothing otherwise.
pub fn level_modifier(level: u32) -> Option<u32> {
    match level {
        1 => Some(xkb::keysyms::KEY_Shift_L),
        2 => Some(xkb::keysyms::KEY_ISO_Level3_Shift),
        _ => None,
    }
}

/// Mutter's seat touch-mode (`update_touch_mode`) and GNOME's
/// `_lastDeviceIsTouchscreen`, combined as `KeyboardManager._syncEnabled`
/// auto-enables the keyboard.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TouchMode {
    pub has_touchscreen: bool,
    pub has_pointer: bool,
    pub has_tablet_switch: bool,
    pub tablet_mode: bool,
    /// The last non-keyboard device used was a touchscreen.
    pub last_touchscreen: bool,
}

impl TouchMode {
    /// `update_touch_mode`: no touchscreen, no touch mode; a tablet-mode
    /// switch decides when present; otherwise touch mode while no
    /// pointer or touchpad is attached.
    pub fn touch_mode(&self) -> bool {
        if !self.has_touchscreen {
            false
        } else if self.has_tablet_switch {
            self.tablet_mode
        } else {
            !self.has_pointer
        }
    }

    /// `autoEnabled = seat.touch_mode && lastDeviceIsTouchscreen`.
    pub fn auto(&self) -> bool {
        self.touch_mode() && self.last_touchscreen
    }
}

/// gnome-shell 51 `Keyboard`'s window slide (`setCursorLocation`,
/// `_setFocusWindow`, `_animateWindow`): while the keyboard shows and the
/// focused field's cursor would sit under it, the field's window moves up
/// by the keyboard's height, and back where it started when the keyboard
/// goes, the field leaves the keyboard's reach or another window takes
/// the slide. A window the user moves is let go where it is
/// (`_onFocusWindowMoving`).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct WindowSlide {
    /// `_focusWindow`, `_focusWindowStartY` and the keyboard height the
    /// window was moved up by.
    focus: Option<(u64, i32, i32)>,
}

/// The keyboard as `setCursorLocation` measures it, global logical.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyboardBox {
    /// `keyboardY1`: the keyboard's top edge.
    pub top: i32,
    /// `keyboardBox.height`.
    pub height: i32,
}

/// The focused field: its window and the cursor's global y and height.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldCursor {
    pub window: u64,
    pub y: i32,
    pub height: i32,
}

impl WindowSlide {
    /// The window shifted for the keyboard, if any.
    pub fn window(&self) -> Option<u64> {
        self.focus.map(|(id, _, _)| id)
    }

    /// One look at the keyboard and the field. `window_y` is a window's
    /// current frame y. Returns the moves to make: `(window, final y)`.
    pub fn update(
        &mut self,
        keyboard: Option<KeyboardBox>,
        cursor: Option<FieldCursor>,
        window_y: impl Fn(u64) -> Option<i32>,
    ) -> Vec<(u64, i32)> {
        // `_animateHide` slides the window back; `_close` then clears
        // the cursor location.
        let Some(keyboard) = keyboard else {
            return self.set_focus(None, 0, &window_y);
        };
        let Some(cursor) = cursor else {
            return self.set_focus(None, keyboard.height, &window_y);
        };
        if let Some((id, start, moved)) = self.focus.filter(|f| f.0 == cursor.window) {
            if cursor.y + cursor.height + keyboard.height < keyboard.top {
                return self.set_focus(None, keyboard.height, &window_y);
            }
            // `_animateWindow` moves by `keyboardBox.height`: the layer's
            // first configure can be smaller than the keyboard it settles
            // at, so the window follows the settled height.
            if moved != keyboard.height {
                self.focus = Some((id, start, keyboard.height));
                return vec![(id, start - keyboard.height)];
            }
            return Vec::new();
        }
        let target = (cursor.y + cursor.height >= keyboard.top).then_some(cursor.window);
        self.set_focus(target, keyboard.height, &window_y)
    }

    /// `_setFocusWindow` with the keyboard showing.
    fn set_focus(
        &mut self,
        window: Option<u64>,
        height: i32,
        window_y: &impl Fn(u64) -> Option<i32>,
    ) -> Vec<(u64, i32)> {
        if self.window() == window {
            return Vec::new();
        }
        let mut moves = Vec::new();
        if let Some((id, start, _)) = self.focus.take() {
            moves.push((id, start));
        }
        if let Some((id, start)) = window.and_then(|id| window_y(id).map(|y| (id, y))) {
            self.focus = Some((id, start, height));
            moves.push((id, start - height));
        }
        moves
    }

    /// The user moved `window`, or it went away: let it go where it is.
    pub fn let_go(&mut self, window: u64) {
        if self.window() == Some(window) {
            self.focus = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keymap(layout: &str, variant: &str) -> xkb::Keymap {
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        xkb::Keymap::new_from_names(
            &context,
            "evdev",
            "pc105",
            layout,
            variant,
            None,
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        )
        .expect("xkeyboard-config keymap")
    }

    #[test]
    fn keysyms_follow_unicode_like_clutter() {
        assert_eq!(keysyms_for("q"), vec![u32::from('q')]);
        assert_eq!(keysyms_for("Q"), vec![u32::from('Q')]);
        // Latin-1 maps to its legacy keysym, the rest to 0x1000000 + cp.
        assert_eq!(keysyms_for("é"), vec![0xe9]);
        assert_eq!(keysyms_for("€"), vec![xkb::keysyms::KEY_EuroSign]);
        assert_eq!(keysyms_for("\u{1F600}"), vec![0x0100_0000 + 0x1F600]);
        assert_eq!(keysyms_for(" "), vec![xkb::keysyms::KEY_space]);
    }

    #[test]
    fn keysyms_pick_keys_and_levels_like_mutter() {
        let us = keymap("us", "");
        // KEY_Q is evdev 16, xkb 24; capital Q is its shift level.
        assert_eq!(pick_keycode(&us, 0, u32::from('q')), Some((24, 0)));
        assert_eq!(pick_keycode(&us, 0, u32::from('Q')), Some((24, 1)));
        assert_eq!(
            pick_keycode(&us, 0, xkb::keysyms::KEY_Return),
            Some((36, 0))
        );
        assert_eq!(pick_keycode(&us, 0, 0xe9), None);
        assert_eq!(level_modifier(1), Some(xkb::keysyms::KEY_Shift_L));
        assert_eq!(level_modifier(0), None);
        // A German layout swaps Y and Z: z sits on KEY_Y (xkb 29).
        let de = keymap("de", "");
        assert_eq!(pick_keycode(&de, 0, u32::from('z')), Some((29, 0)));
        // AltGr (level 2) types @ on KEY_Q in de.
        assert_eq!(pick_keycode(&de, 0, u32::from('@')), Some((24, 2)));
        assert_eq!(level_modifier(2), Some(xkb::keysyms::KEY_ISO_Level3_Shift));
    }

    #[test]
    fn touch_mode_follows_mutter_and_the_last_device() {
        let mut mode = TouchMode::default();
        assert!(!mode.touch_mode());
        mode.has_touchscreen = true;
        assert!(mode.touch_mode(), "touchscreen alone is touch mode");
        mode.has_pointer = true;
        assert!(
            !mode.touch_mode(),
            "a pointer or touchpad leaves touch mode"
        );
        mode.has_tablet_switch = true;
        assert!(!mode.touch_mode(), "a laptop switch decides: off");
        mode.tablet_mode = true;
        assert!(mode.touch_mode(), "folded into a tablet: on");
        assert!(!mode.auto(), "auto-show also needs a touch last");
        mode.last_touchscreen = true;
        assert!(mode.auto());
        mode.has_touchscreen = false;
        assert!(!mode.auto());
    }

    #[test]
    fn window_slides_up_for_a_covered_field_like_gnome_shell() {
        let ys = |id: u64| Some(if id == 1 { 500 } else { 100 });
        let keyboard = Some(KeyboardBox {
            top: 720,
            height: 360,
        });
        let mut slide = WindowSlide::default();
        // A field above the keyboard: nothing moves.
        let above = FieldCursor {
            window: 1,
            y: 600,
            height: 20,
        };
        assert!(slide.update(keyboard, Some(above), ys).is_empty());
        // Under the keyboard's top: up by the keyboard's height.
        let covered = FieldCursor {
            window: 1,
            y: 800,
            height: 20,
        };
        assert_eq!(slide.update(keyboard, Some(covered), ys), vec![(1, 140)]);
        // The shifted field (now at 440) stays put while near the keyboard.
        let shifted = FieldCursor { y: 440, ..covered };
        assert!(slide.update(keyboard, Some(shifted), ys).is_empty());
        // Far enough above that it would clear the keyboard unshifted:
        // back to where it started.
        let high = FieldCursor { y: 300, ..covered };
        assert_eq!(slide.update(keyboard, Some(high), ys), vec![(1, 500)]);
        // Another window's covered field takes the slide.
        assert_eq!(slide.update(keyboard, Some(covered), ys), vec![(1, 140)]);
        let other = FieldCursor {
            window: 2,
            y: 730,
            height: 20,
        };
        assert_eq!(
            slide.update(keyboard, Some(other), ys),
            vec![(1, 500), (2, -260)]
        );
        // The keyboard hides: back down.
        assert_eq!(slide.update(None, Some(other), ys), vec![(2, 100)]);
        assert_eq!(slide.window(), None);
        // A window the user moves is let go where it is.
        slide.update(keyboard, Some(covered), ys);
        slide.let_go(1);
        assert!(slide.update(None, None, ys).is_empty());
    }

    #[test]
    fn slid_window_follows_the_keyboards_settled_height() {
        let ys = |_| Some(500);
        let mut slide = WindowSlide::default();
        let field = FieldCursor {
            window: 1,
            y: 640,
            height: 20,
        };
        let first = KeyboardBox {
            top: 600,
            height: 200,
        };
        assert_eq!(slide.update(Some(first), Some(field), ys), vec![(1, 300)]);
        let settled = KeyboardBox {
            top: 534,
            height: 266,
        };
        let shifted = FieldCursor { y: 440, ..field };
        assert_eq!(
            slide.update(Some(settled), Some(shifted), ys),
            vec![(1, 234)]
        );
        let settled_field = FieldCursor { y: 374, ..field };
        assert!(slide.update(Some(settled), Some(settled_field), ys).is_empty());
        assert_eq!(slide.update(None, None, ys), vec![(1, 500)]);
    }
}
