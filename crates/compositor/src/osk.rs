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

/// `ZWP_TEXT_INPUT_V3_SHOW_INPUT_PANEL_SINCE_VERSION`: from this version a
/// client asks for the input panel itself instead of re-enabling.
const SHOW_INPUT_PANEL_SINCE: u32 = 2;

/// mutter's `ClutterInputPanelState` as its text input sets it
/// (`clutter_input_focus_set_input_panel_state`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputPanel {
    On,
    Off,
}

#[derive(Debug, Default, Clone, Copy)]
struct Pending {
    enable: Option<bool>,
    content: Option<(u32, u32)>,
    /// `show_input_panel` (true) or `hide_input_panel` (false), the last
    /// one before `commit` (mutter's `show_panel`).
    panel: Option<bool>,
}

#[derive(Debug)]
struct Tracked {
    resource: ZwpTextInputV3,
    pending: Pending,
    enabled: bool,
    /// Committed (hint, purpose).
    content: (u32, u32),
    /// `commit` requests seen: the serial `done` must echo.
    commits: u32,
}

/// Every live text input with the state its client committed, as
/// mutter's `MetaWaylandTextInput` keeps it.
#[derive(Debug, Default)]
pub struct TextInputs {
    tracked: Vec<Tracked>,
    /// The input-panel state the last focused commit asked for.
    panel: Option<InputPanel>,
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
                    commits: 0,
                });
                self.tracked.len() - 1
            }
        };
        let entry = &mut self.tracked[index];
        match request {
            zwp_text_input_v3::Request::Enable => {
                // Enabling resets the field's state (the protocol's
                // rule); a panel request made before it stands, as in
                // mutter's text_input_enable.
                entry.pending.enable = Some(true);
                entry.pending.content = Some((0, 0));
            }
            zwp_text_input_v3::Request::Disable => entry.pending.enable = Some(false),
            zwp_text_input_v3::Request::SetContentType { hint, purpose } => {
                let hint = match hint {
                    smithay::reexports::wayland_server::WEnum::Value(hint) => hint.bits(),
                    smithay::reexports::wayland_server::WEnum::Unknown(bits) => *bits,
                };
                entry.pending.content = Some((hint, u32::from(*purpose)));
            }
            zwp_text_input_v3::Request::ShowInputPanel => entry.pending.panel = Some(true),
            zwp_text_input_v3::Request::HideInputPanel => entry.pending.panel = Some(false),
            zwp_text_input_v3::Request::Commit => {
                entry.commits = entry.commits.wrapping_add(1);
                let pending = std::mem::take(&mut entry.pending);
                let focused =
                    focus.is_some_and(|surface| surface.id().same_client_as(&resource.id()));
                if !focused {
                    return;
                }
                // mutter's text_input_commit_state.
                let mut enable_panel_v1 = false;
                match pending.enable {
                    // Enabling an already focused version-1 text input
                    // asks for the input panel (GTK re-enables on a click
                    // into its field, `activate_osk`); version 2 asks with
                    // show_input_panel instead.
                    Some(true) if entry.enabled => {
                        enable_panel_v1 = resource.version() < SHOW_INPUT_PANEL_SINCE;
                    }
                    Some(enabled) => entry.enabled = enabled,
                    None => {}
                }
                if let Some(content) = pending.content {
                    entry.content = content;
                }
                // Without input-method focus the panel request is dropped.
                if !entry.enabled {
                    return;
                }
                match pending.panel {
                    Some(true) => self.panel = Some(InputPanel::On),
                    Some(false) => self.panel = Some(InputPanel::Off),
                    None if enable_panel_v1 => self.panel = Some(InputPanel::On),
                    None => {}
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
        self.panel = None;
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

    /// Take the pending input-panel request; only a focused field may make
    /// it.
    pub fn take_panel(&mut self, focus: Option<&WlSurface>) -> Option<InputPanel> {
        self.panel.take().filter(|_| self.active(focus).is_some())
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
}
