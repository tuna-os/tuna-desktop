//! GNOME 51's window focus policy (`org.gnome.desktop.wm.preferences`
//! `focus-mode`, `auto-raise`, `auto-raise-delay`, `raise-on-click`,
//! `mouse-button-modifier`, `resize-with-right-button`, plus
//! `org.gnome.mutter focus-change-on-pointer-rest`), as pure decisions.
//!
//! Mutter 51.0 is the reference:
//! - `src/core/prefs.c`: defaults, `meta_prefs_get_mouse_button_resize`
//!   (3 with `resize-with-right-button`, else 2) and `_menu` (the other).
//! - `src/core/meta-accel-parse.c` `meta_parse_modifier`: empty or
//!   `disabled` is no modifier; `<Name>` tokens accumulate, a trailing key
//!   name is ignored, unknown `<…>` tokens are skipped.
//! - `src/core/window.c` `meta_window_handle_ungrabbed_event`: every press
//!   focuses; an unmodified one raises only with `raise-on-click`; with the
//!   whole modifier held, the resize button resizes from the pressed third
//!   (the centre does nothing), the menu button shows the window menu
//!   (raising with `raise-on-click`) and button 1 moves. Not while the
//!   focused window inhibits shortcuts.
//! - `src/core/display.c` `meta_display_handle_window_enter`,
//!   `focus_mouse_mode`, `focus_on_pointer_rest_callback` (25 ms) and
//!   the sticky focus after an activation (unstuck after 50 px of motion):
//!   sloppy and mouse focus on enter, mouse also unfocuses when the pointer
//!   reaches no window; auto-raise is queued only from that path.
//! - `src/compositor/meta-window-drag.c`: a grab raises at its start with
//!   `raise-on-click`; without it, only a grab that never moved 8 px raises
//!   at its end.

use std::time::Duration;

use tuna_shell_control::{MOD_ALT, MOD_CTRL, MOD_LOGO, MOD_SHIFT};

/// GSettings schema holding the window-manager preferences.
pub const WM_SCHEMA: &str = "org.gnome.desktop.wm.preferences";
/// Mutter's own schema (`focus-change-on-pointer-rest`).
pub const MUTTER_SCHEMA: &str = "org.gnome.mutter";

/// Linux pointer button codes.
pub const BUTTON_LEFT: u32 = 0x110;
pub const BUTTON_RIGHT: u32 = 0x111;
pub const BUTTON_MIDDLE: u32 = 0x112;

/// `FOCUS_TIMEOUT_DELAY`: the pointer-rest poll interval.
pub const POINTER_REST_INTERVAL: Duration = Duration::from_millis(25);
/// `STICKY_FOCUS_UNSTICK_DISTANCE`: motion that releases sticky focus.
pub const STICKY_UNSTICK_DISTANCE: f64 = 50.0;
/// `check_threshold_reached`: a grab that moved this far does not raise.
pub const GRAB_RAISE_THRESHOLD: f64 = 8.0;
/// `auto-raise-delay`'s schema range.
pub const MAX_AUTO_RAISE_DELAY_MS: u32 = 10_000;

/// `GDesktopFocusMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FocusMode {
    #[default]
    Click,
    Sloppy,
    Mouse,
}

impl FocusMode {
    /// The GSettings enum nick; anything else is the schema default.
    pub fn from_nick(nick: &str) -> Self {
        match nick {
            "sloppy" => Self::Sloppy,
            "mouse" => Self::Mouse,
            _ => Self::Click,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Click => "click",
            Self::Sloppy => "sloppy",
            Self::Mouse => "mouse",
        }
    }

    /// Whether the pointer moves focus (sloppy or mouse).
    pub fn follows_pointer(self) -> bool {
        self != Self::Click
    }
}

/// The settings exactly as read, before Mutter's per-key validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawFocusSettings {
    pub focus_mode: String,
    pub auto_raise: bool,
    pub auto_raise_delay_ms: i32,
    pub raise_on_click: bool,
    pub mouse_button_modifier: String,
    pub resize_with_right_button: bool,
    /// `None` when `org.gnome.mutter` is not installed.
    pub focus_change_on_pointer_rest: Option<bool>,
}

/// The effective focus policy. [`Default`] is GNOME 51's session default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FocusPolicy {
    pub mode: FocusMode,
    pub auto_raise: bool,
    pub auto_raise_delay: Duration,
    pub raise_on_click: bool,
    /// `MOD_*` bits; 0 disables every modifier-click action.
    pub modifier: u32,
    pub resize_with_right_button: bool,
    pub focus_change_on_pointer_rest: bool,
}

impl Default for FocusPolicy {
    fn default() -> Self {
        Self {
            mode: FocusMode::Click,
            auto_raise: false,
            auto_raise_delay: Duration::from_millis(500),
            raise_on_click: true,
            modifier: MOD_LOGO,
            resize_with_right_button: false,
            // gnome-shell 51.0 data/00_org.gnome.shell.gschema.override
            // sets it for GNOME sessions (the mutter schema says false).
            focus_change_on_pointer_rest: true,
        }
    }
}

impl FocusPolicy {
    /// Apply a fresh read. An unparsable `mouse-button-modifier` keeps the
    /// previous modifier, as Mutter's `mouse_button_mods_handler` does; a
    /// missing Mutter schema keeps GNOME Shell's pointer-rest default.
    pub fn apply(&mut self, raw: &RawFocusSettings) {
        self.mode = FocusMode::from_nick(&raw.focus_mode);
        self.auto_raise = raw.auto_raise;
        self.auto_raise_delay = Duration::from_millis(u64::from(
            raw.auto_raise_delay_ms
                .clamp(0, MAX_AUTO_RAISE_DELAY_MS as i32) as u32,
        ));
        self.raise_on_click = raw.raise_on_click;
        if let Some(modifier) = parse_modifier(&raw.mouse_button_modifier) {
            self.modifier = modifier;
        }
        self.resize_with_right_button = raw.resize_with_right_button;
        self.focus_change_on_pointer_rest = raw
            .focus_change_on_pointer_rest
            .unwrap_or(Self::default().focus_change_on_pointer_rest);
    }

    /// `meta_prefs_get_mouse_button_resize`, as a Linux button code.
    pub fn resize_button(&self) -> u32 {
        if self.resize_with_right_button {
            BUTTON_RIGHT
        } else {
            BUTTON_MIDDLE
        }
    }

    /// `meta_prefs_get_mouse_button_menu`, as a Linux button code.
    pub fn menu_button(&self) -> u32 {
        if self.resize_with_right_button {
            BUTTON_MIDDLE
        } else {
            BUTTON_RIGHT
        }
    }

    /// What a press on a managed window does.
    ///
    /// `held` is the `MOD_*` state at the press, `inhibited` whether the
    /// focused window inhibits compositor shortcuts, `frame` the window
    /// rectangle `(x, y, w, h)` and `at` the press position.
    pub fn press(
        &self,
        held: u32,
        button: u32,
        inhibited: bool,
        frame: (i32, i32, i32, i32),
        at: (f64, f64),
    ) -> Press {
        let grab = self.modifier;
        let unmodified = held & grab == 0;
        let window_grab = !inhibited && held & grab == grab;
        if unmodified {
            return Press::Focus {
                raise: self.raise_on_click,
            };
        }
        if window_grab && button == self.resize_button() {
            let edges = resize_edges(frame, at);
            if edges != 0 {
                return Press::Resize { edges };
            }
        } else if window_grab && button == self.menu_button() {
            return Press::Menu {
                raise: self.raise_on_click,
            };
        } else if window_grab && button == BUTTON_LEFT {
            return Press::Move;
        }
        // Partially modified, or the resize button in the centre third:
        // focused, never raised, and the client still gets the press.
        Press::Focus { raise: false }
    }

    /// What the pointer entering `target` (`None`: no window, i.e. the
    /// desktop or shell chrome) does, before pointer-rest delays it.
    /// `sticky` holds while an activation's sticky focus has seen no motion.
    pub fn enter(&self, target: Option<u64>, focused: Option<u64>, sticky: bool) -> Enter {
        if !self.mode.follows_pointer() || sticky {
            return Enter::Nothing;
        }
        self.focus_mouse_mode(target, focused)
    }

    /// `focus_mouse_mode`: focus the window under the pointer (queueing
    /// auto-raise), or in mouse mode unfocus over no window.
    pub fn focus_mouse_mode(&self, target: Option<u64>, focused: Option<u64>) -> Enter {
        match target {
            Some(id) => Enter::Focus {
                id,
                auto_raise: self.auto_raise.then_some(self.auto_raise_delay),
            },
            None if self.mode == FocusMode::Mouse && focused.is_some() => Enter::Unfocus,
            None => Enter::Nothing,
        }
    }

    /// Whether an interactive grab raises its window when it starts.
    pub fn grab_raises_at_start(&self) -> bool {
        self.raise_on_click
    }

    /// Whether a finished grab that travelled `distance` (the largest axis
    /// offset from its start) raises its window now.
    pub fn grab_raises_at_end(&self, distance: f64) -> bool {
        !self.raise_on_click && distance < GRAB_RAISE_THRESHOLD
    }
}

/// A press decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Press {
    /// Focus, raise when asked, deliver the press to the client.
    Focus { raise: bool },
    /// Modifier move; the press and its release stay with the compositor.
    Move,
    /// Modifier resize from these xdg `ResizeEdge` bits.
    Resize { edges: u32 },
    /// Modifier window menu.
    Menu { raise: bool },
}

/// An enter decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Enter {
    Nothing,
    /// Focus without raising; auto-raise after the delay when set.
    Focus {
        id: u64,
        auto_raise: Option<Duration>,
    },
    Unfocus,
}

/// xdg_toplevel `ResizeEdge` bits.
pub const EDGE_TOP: u32 = 1;
pub const EDGE_BOTTOM: u32 = 2;
pub const EDGE_LEFT: u32 = 4;
pub const EDGE_RIGHT: u32 = 8;

/// The edges a modifier resize grabs: Mutter splits the frame into thirds
/// with integer division, so the centre third of either axis grabs neither
/// edge on it and the middle cell grabs none.
pub fn resize_edges(frame: (i32, i32, i32, i32), at: (f64, f64)) -> u32 {
    let (x, y, w, h) = frame;
    let mut edges = 0;
    if at.0 < f64::from(x + w / 3) {
        edges |= EDGE_LEFT;
    }
    if at.0 > f64::from(x + 2 * w / 3) {
        edges |= EDGE_RIGHT;
    }
    if at.1 < f64::from(y + h / 3) {
        edges |= EDGE_TOP;
    }
    if at.1 > f64::from(y + 2 * h / 3) {
        edges |= EDGE_BOTTOM;
    }
    edges
}

/// `meta_parse_modifier` onto the modifiers the compositor tracks. `None`
/// for a value Mutter rejects (a key name xkb does not know). `<Primary>`,
/// `<Control>`, `<Ctrl>`, `<Ctl>` are Control; `<Shift>`, `<Shft>` Shift;
/// `<Alt>`, `<Mod1>`, `<Meta>` Alt; `<Super>`, `<Mod4>`, `<Hyper>` Super
/// (xkeyboard-config's evdev modifier map). `<Mod2>`, `<Mod3>` and
/// `<Mod5>` (Num Lock, unbound, ISO Level 3) are not window modifiers
/// here and contribute nothing — a deliberate narrowing.
pub fn parse_modifier(accel: &str) -> Option<u32> {
    if accel.is_empty() || accel == "disabled" {
        return Some(0);
    }
    let mut rest = accel;
    let mut mods = 0;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix('<') {
            let Some(end) = after.find('>') else {
                // Mutter's scan consumes the unterminated token.
                break;
            };
            let name = after[..end].to_ascii_lowercase();
            mods |= match name.as_str() {
                "primary" | "control" | "ctrl" | "ctl" => MOD_CTRL,
                "shift" | "shft" => MOD_SHIFT,
                "alt" | "mod1" | "meta" => MOD_ALT,
                "super" | "mod4" | "hyper" => MOD_LOGO,
                _ => 0,
            };
            rest = &after[end + 1..];
        } else {
            // The key part names one key; only its validity matters.
            if !valid_key_name(rest) {
                return None;
            }
            break;
        }
    }
    Some(mods)
}

/// Mutter accepts a raw keycode (`0x…`), `Above_Tab`, or an xkb keysym
/// name (case-insensitive, also with an `XF86` prefix).
fn valid_key_name(name: &str) -> bool {
    use smithay::input::keyboard::xkb;
    let keycode = name.len() >= 4
        && name.starts_with("0x")
        && name.as_bytes()[2].is_ascii_hexdigit()
        && name.as_bytes()[3].is_ascii_hexdigit();
    let known = |name: &str| xkb::keysym_from_name(name, xkb::KEYSYM_CASE_INSENSITIVE).raw() != 0;
    keycode || name == "Above_Tab" || known(name) || known(&format!("XF86{name}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw() -> RawFocusSettings {
        RawFocusSettings {
            focus_mode: "click".into(),
            auto_raise: false,
            auto_raise_delay_ms: 500,
            raise_on_click: true,
            mouse_button_modifier: "<Super>".into(),
            resize_with_right_button: false,
            focus_change_on_pointer_rest: Some(true),
        }
    }

    const FRAME: (i32, i32, i32, i32) = (100, 100, 300, 300);
    const CORNER: (f64, f64) = (390.0, 390.0);

    #[test]
    fn schema_defaults_are_gnome_51_session_defaults() {
        let mut policy = FocusPolicy::default();
        assert_eq!(policy.mode, FocusMode::Click);
        assert!(!policy.auto_raise && policy.raise_on_click);
        assert_eq!(policy.auto_raise_delay, Duration::from_millis(500));
        assert_eq!(policy.modifier, MOD_LOGO);
        assert!(!policy.resize_with_right_button);
        assert!(policy.focus_change_on_pointer_rest);
        let before = policy;
        policy.apply(&raw());
        assert_eq!(policy, before);
    }

    #[test]
    fn modifier_parses_like_meta_parse_modifier() {
        assert_eq!(parse_modifier("<Super>"), Some(MOD_LOGO));
        assert_eq!(parse_modifier("<alt>"), Some(MOD_ALT));
        assert_eq!(parse_modifier("<Mod1>"), Some(MOD_ALT));
        assert_eq!(parse_modifier("<Meta>"), Some(MOD_ALT));
        assert_eq!(parse_modifier("<Hyper>"), Some(MOD_LOGO));
        assert_eq!(
            parse_modifier("<Primary><Shift>"),
            Some(MOD_CTRL | MOD_SHIFT)
        );
        assert_eq!(parse_modifier("<Ctl><Shft>"), Some(MOD_CTRL | MOD_SHIFT));
        assert_eq!(parse_modifier("<Super>a"), Some(MOD_LOGO));
        assert_eq!(parse_modifier("<Bogus><Alt>"), Some(MOD_ALT));
        assert_eq!(parse_modifier(""), Some(0));
        assert_eq!(parse_modifier("disabled"), Some(0));
        assert_eq!(parse_modifier("<Mod5>"), Some(0));
        assert_eq!(parse_modifier("<Super>NotAKey"), None);
    }

    #[test]
    fn unparsable_modifier_keeps_the_previous_one() {
        let mut policy = FocusPolicy::default();
        policy.apply(&RawFocusSettings {
            mouse_button_modifier: "<Alt>".into(),
            ..raw()
        });
        assert_eq!(policy.modifier, MOD_ALT);
        policy.apply(&RawFocusSettings {
            mouse_button_modifier: "<Alt>NotAKey".into(),
            ..raw()
        });
        assert_eq!(policy.modifier, MOD_ALT);
    }

    #[test]
    fn delay_clamps_to_the_schema_range_and_missing_mutter_keeps_rest() {
        let mut policy = FocusPolicy::default();
        policy.apply(&RawFocusSettings {
            auto_raise_delay_ms: 20_000,
            focus_change_on_pointer_rest: None,
            ..raw()
        });
        assert_eq!(policy.auto_raise_delay, Duration::from_millis(10_000));
        assert!(policy.focus_change_on_pointer_rest);
        policy.apply(&RawFocusSettings {
            auto_raise_delay_ms: -5,
            focus_change_on_pointer_rest: Some(false),
            ..raw()
        });
        assert_eq!(policy.auto_raise_delay, Duration::ZERO);
        assert!(!policy.focus_change_on_pointer_rest);
    }

    #[test]
    fn unmodified_press_focuses_and_raises_only_with_raise_on_click() {
        let mut policy = FocusPolicy::default();
        assert_eq!(
            policy.press(0, BUTTON_LEFT, false, FRAME, CORNER),
            Press::Focus { raise: true }
        );
        policy.raise_on_click = false;
        assert_eq!(
            policy.press(MOD_SHIFT, BUTTON_RIGHT, false, FRAME, CORNER),
            Press::Focus { raise: false }
        );
    }

    #[test]
    fn modifier_buttons_move_resize_and_show_the_menu() {
        let mut policy = FocusPolicy::default();
        let press = |policy: &FocusPolicy, button| {
            policy.press(MOD_LOGO | MOD_SHIFT, button, false, FRAME, CORNER)
        };
        assert_eq!(press(&policy, BUTTON_LEFT), Press::Move);
        assert_eq!(
            press(&policy, BUTTON_MIDDLE),
            Press::Resize {
                edges: EDGE_RIGHT | EDGE_BOTTOM
            }
        );
        assert_eq!(press(&policy, BUTTON_RIGHT), Press::Menu { raise: true });
        policy.resize_with_right_button = true;
        policy.raise_on_click = false;
        assert_eq!(
            press(&policy, BUTTON_RIGHT),
            Press::Resize {
                edges: EDGE_RIGHT | EDGE_BOTTOM
            }
        );
        assert_eq!(press(&policy, BUTTON_MIDDLE), Press::Menu { raise: false });
        // Other buttons are delivered.
        assert_eq!(press(&policy, 0x113), Press::Focus { raise: false });
    }

    #[test]
    fn modifier_change_moves_the_grab_to_the_new_key() {
        let mut policy = FocusPolicy::default();
        policy.apply(&RawFocusSettings {
            mouse_button_modifier: "<Alt>".into(),
            ..raw()
        });
        assert_eq!(
            policy.press(MOD_ALT, BUTTON_LEFT, false, FRAME, CORNER),
            Press::Move
        );
        assert_eq!(
            policy.press(MOD_LOGO, BUTTON_LEFT, false, FRAME, CORNER),
            Press::Focus { raise: true },
            "Super alone is an ordinary click once Alt is the modifier"
        );
        // A two-modifier grab needs both; one of them is partial.
        policy.modifier = MOD_ALT | MOD_CTRL;
        assert_eq!(
            policy.press(MOD_ALT, BUTTON_LEFT, false, FRAME, CORNER),
            Press::Focus { raise: false }
        );
        // Disabled: every press is unmodified.
        policy.modifier = 0;
        assert_eq!(
            policy.press(MOD_ALT | MOD_LOGO, BUTTON_LEFT, false, FRAME, CORNER),
            Press::Focus { raise: true }
        );
    }

    #[test]
    fn inhibited_shortcuts_disable_modifier_actions() {
        let policy = FocusPolicy::default();
        assert_eq!(
            policy.press(MOD_LOGO, BUTTON_LEFT, true, FRAME, CORNER),
            Press::Focus { raise: false }
        );
    }

    #[test]
    fn resize_thirds_follow_mutter_integer_division() {
        assert_eq!(resize_edges(FRAME, (250.0, 250.0)), 0);
        assert_eq!(resize_edges(FRAME, (150.0, 250.0)), EDGE_LEFT);
        assert_eq!(resize_edges(FRAME, (250.0, 150.0)), EDGE_TOP);
        assert_eq!(resize_edges(FRAME, (150.0, 150.0)), EDGE_LEFT | EDGE_TOP);
        // x + w / 3 with w = 301 is 200: 199.5 is left, 200 is not.
        let odd = (100, 100, 301, 301);
        assert_eq!(resize_edges(odd, (199.5, 250.0)), EDGE_LEFT);
        assert_eq!(resize_edges(odd, (200.0, 250.0)), 0);
        // The centre third grabs nothing, so the press is delivered.
        assert_eq!(
            FocusPolicy::default().press(MOD_LOGO, BUTTON_MIDDLE, false, FRAME, (250.0, 250.0)),
            Press::Focus { raise: false }
        );
    }

    #[test]
    fn click_mode_ignores_enter() {
        let policy = FocusPolicy::default();
        assert_eq!(policy.enter(Some(2), Some(1), false), Enter::Nothing);
        assert_eq!(policy.enter(None, Some(1), false), Enter::Nothing);
    }

    #[test]
    fn sloppy_focuses_on_enter_and_keeps_focus_over_no_window() {
        let policy = FocusPolicy {
            mode: FocusMode::Sloppy,
            ..FocusPolicy::default()
        };
        assert_eq!(
            policy.enter(Some(2), Some(1), false),
            Enter::Focus {
                id: 2,
                auto_raise: None
            }
        );
        assert_eq!(policy.enter(None, Some(2), false), Enter::Nothing);
    }

    #[test]
    fn mouse_unfocuses_over_no_window() {
        let policy = FocusPolicy {
            mode: FocusMode::Mouse,
            ..FocusPolicy::default()
        };
        assert_eq!(policy.enter(None, Some(2), false), Enter::Unfocus);
        assert_eq!(policy.enter(None, None, false), Enter::Nothing);
    }

    #[test]
    fn auto_raise_is_queued_only_by_pointer_focus() {
        let mut policy = FocusPolicy {
            auto_raise: true,
            auto_raise_delay: Duration::from_millis(300),
            ..FocusPolicy::default()
        };
        assert_eq!(policy.enter(Some(2), Some(1), false), Enter::Nothing);
        policy.mode = FocusMode::Sloppy;
        assert_eq!(
            policy.enter(Some(2), Some(1), false),
            Enter::Focus {
                id: 2,
                auto_raise: Some(Duration::from_millis(300))
            }
        );
    }

    #[test]
    fn sticky_activation_focus_blocks_stationary_enter() {
        let policy = FocusPolicy {
            mode: FocusMode::Mouse,
            ..FocusPolicy::default()
        };
        assert_eq!(policy.enter(Some(2), Some(1), true), Enter::Nothing);
        assert_eq!(policy.enter(None, Some(1), true), Enter::Nothing);
    }

    #[test]
    fn grabs_raise_at_start_or_after_a_short_grab_without_raise_on_click() {
        let mut policy = FocusPolicy::default();
        assert!(policy.grab_raises_at_start());
        assert!(!policy.grab_raises_at_end(0.0));
        policy.raise_on_click = false;
        assert!(!policy.grab_raises_at_start());
        assert!(policy.grab_raises_at_end(7.9));
        assert!(!policy.grab_raises_at_end(8.0));
    }
}
