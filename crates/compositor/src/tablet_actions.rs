//! What GNOME 51 does with tablet buttons, rings and strips that settings
//! take away from applications (P-IN-05, #344). Backend-free policy,
//! ported from Mutter 51.0:
//!
//! - `src/core/meta-tablet-action-mapper.c`: `cycle_logical_monitors`
//!   (switch-monitor) and `meta_tablet_action_mapper_emulate_keybinding`
//!   with `emulate_modifiers` (keybinding);
//! - `src/core/meta-pad-action-mapper.c`: pad button, ring and strip
//!   settings paths, `meta_pad_action_mapper_handle_button`,
//!   `meta_pad_action_mapper_get_action_direction`, and the labels the
//!   pad OSD shows (`meta_pad_action_mapper_get_button_label`,
//!   `get_feature_label`);
//! - `src/core/meta-accel-parse.c`: `accelerator_parse`;
//! - `src/core/display.c`: `meta_display_notify_pad_group_switch`'s OSD.

use smithay::input::keyboard::xkb;

pub const PAD_BUTTON_SCHEMA: &str = "org.gnome.desktop.peripherals.tablet.pad-button";

/// Clutter's modifier bits that `accelerator_parse` sets.
pub const SHIFT_MASK: u32 = 1 << 0;
pub const CONTROL_MASK: u32 = 1 << 2;
pub const MOD1_MASK: u32 = 1 << 3;
pub const MOD2_MASK: u32 = 1 << 4;
pub const MOD3_MASK: u32 = 1 << 5;
pub const MOD4_MASK: u32 = 1 << 6;
pub const MOD5_MASK: u32 = 1 << 7;
pub const SUPER_MASK: u32 = 1 << 26;
pub const HYPER_MASK: u32 = 1 << 27;
pub const META_MASK: u32 = 1 << 28;

/// Mutter's `META_KEY_ABOVE_TAB` pseudo keysym.
pub const KEY_ABOVE_TAB: u32 = 0x2f72_59c9;

/// One parsed accelerator (`MetaKeyCombo`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyCombo {
    pub keysym: u32,
    pub keycode: u32,
    pub modifiers: u32,
}

/// `meta_parse_accelerator`: GTK's accelerator syntax as Mutter reads it.
/// `None` is a key name xkb does not know.
pub fn parse_accelerator(accel: &str) -> Option<KeyCombo> {
    let mut combo = KeyCombo {
        keysym: 0,
        keycode: 0,
        modifiers: 0,
    };
    if accel.is_empty() || accel == "disabled" {
        return Some(combo);
    }
    // The `<…>` tokens Mutter recognises, longest first where they share
    // a prefix exactly as its if-chain tests them.
    const TOKENS: [(&str, u32); 11] = [
        ("<primary>", CONTROL_MASK),
        ("<control>", CONTROL_MASK),
        ("<shift>", SHIFT_MASK),
        ("<shft>", SHIFT_MASK),
        ("<ctrl>", CONTROL_MASK),
        ("<ctl>", CONTROL_MASK),
        ("<alt>", MOD1_MASK),
        ("<meta>", META_MASK),
        ("<hyper>", HYPER_MASK),
        ("<super>", SUPER_MASK),
        ("", 0),
    ];
    let mut rest = accel.as_bytes();
    while !rest.is_empty() {
        if rest[0] == b'<' {
            let lower: Vec<u8> = rest.iter().take(9).map(u8::to_ascii_lowercase).collect();
            let modx = rest.len() >= 6
                && lower.starts_with(b"<mod")
                && (b'1'..=b'5').contains(&rest[4])
                && rest[5] == b'>';
            if modx {
                mods_from_modx(rest[4], &mut combo.modifiers);
                rest = &rest[6..];
                continue;
            }
            match TOKENS
                .iter()
                .find(|(token, _)| !token.is_empty() && lower.starts_with(token.as_bytes()))
            {
                Some((token, mask)) => {
                    combo.modifiers |= mask;
                    rest = &rest[token.len()..];
                }
                None => {
                    // An unknown `<…>`: skipped through its `>`.
                    let end = rest.iter().position(|b| *b == b'>').map_or(rest.len(), |p| p + 1);
                    rest = &rest[end..];
                }
            }
        } else {
            let name = std::str::from_utf8(rest).ok()?;
            let bytes = name.as_bytes();
            if bytes.len() >= 4
                && bytes[0] == b'0'
                && bytes[1] == b'x'
                && bytes[2].is_ascii_hexdigit()
                && bytes[3].is_ascii_hexdigit()
            {
                // strtoul: the leading hex digits.
                let digits: String = name[2..].chars().take_while(char::is_ascii_hexdigit).collect();
                combo.keycode = u32::from_str_radix(&digits, 16).unwrap_or(u32::MAX);
                return Some(combo);
            }
            if name == "Above_Tab" {
                combo.keysym = KEY_ABOVE_TAB;
                return Some(combo);
            }
            let lookup = |name: &str| xkb::keysym_from_name(name, xkb::KEYSYM_CASE_INSENSITIVE).raw();
            let keysym = match lookup(name) {
                0 => lookup(&format!("XF86{name}")),
                keysym => keysym,
            };
            if keysym == 0 {
                return None;
            }
            combo.keysym = keysym;
            return Some(combo);
        }
    }
    Some(combo)
}

fn mods_from_modx(digit: u8, mods: &mut u32) {
    *mods |= [MOD1_MASK, MOD2_MASK, MOD3_MASK, MOD4_MASK, MOD5_MASK][usize::from(digit - b'1')];
}

/// `emulate_modifiers`: the keysyms pressed before, and released after,
/// an emulated keybinding's key. Only these four: Mutter 51 emulates no
/// Super or Hyper.
pub fn modifier_keysyms(modifiers: u32) -> Vec<u32> {
    use xkb::keysyms::{KEY_Alt_L, KEY_Control_L, KEY_Meta_L, KEY_Shift_L};
    [
        (SHIFT_MASK, KEY_Shift_L),
        (CONTROL_MASK, KEY_Control_L),
        (MOD1_MASK, KEY_Alt_L),
        (META_MASK, KEY_Meta_L),
    ]
    .into_iter()
    .filter(|(mask, _)| modifiers & mask != 0)
    .map(|(_, keysym)| keysym)
    .collect()
}

/// `apply_level_modifiers_in_impl`: the keysym held to reach a level.
pub fn level_modifier(level: u32) -> Option<u32> {
    match level {
        1 => Some(xkb::keysyms::KEY_Shift_L),
        2 => Some(xkb::keysyms::KEY_ISO_Level3_Shift),
        _ => None,
    }
}

/// One step of an emulated keybinding, by keysym.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyvalStep {
    pub keysym: u32,
    pub pressed: bool,
}

/// `meta_tablet_action_mapper_emulate_keybinding`: the keyvals notified
/// for one press or release of `accel`, in order. `None` for an
/// accelerator Mutter refuses (it warns and emulates nothing).
pub fn keybinding_steps(accel: &str, pressed: bool) -> Option<Vec<KeyvalStep>> {
    if accel.is_empty() {
        return Some(Vec::new());
    }
    let combo = parse_accelerator(accel)?;
    let mods = modifier_keysyms(combo.modifiers)
        .into_iter()
        .map(|keysym| KeyvalStep { keysym, pressed });
    let key = KeyvalStep {
        keysym: combo.keysym,
        pressed,
    };
    Some(if pressed {
        mods.chain(std::iter::once(key)).collect()
    } else {
        std::iter::once(key).chain(mods).collect()
    })
}

/// `GDesktopPadButtonAction`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PadButtonAction {
    #[default]
    None,
    Help,
    SwitchMonitor,
    Keybinding,
}

impl PadButtonAction {
    pub fn from_nick(nick: &str) -> Option<Self> {
        Some(match nick {
            "none" => Self::None,
            "help" => Self::Help,
            "switch-monitor" => Self::SwitchMonitor,
            "keybinding" => Self::Keybinding,
            _ => return None,
        })
    }
}

/// `org.gnome.desktop.peripherals.tablet.pad-button` at one path.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PadButtonSettings {
    pub action: PadButtonAction,
    pub keybinding: String,
}

/// A pad feature with actions (`MetaPadFeatureType`; dials need
/// libinput 1.26, which the build links against no earlier than 1.25).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PadFeature {
    Ring,
    Strip,
}

/// `MetaPadDirection` for the feature it applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PadDirection {
    Up,
    Down,
    Cw,
    Ccw,
}

fn tag(number: u32) -> char {
    char::from_u32(u32::from(b'A') + number).unwrap_or('?')
}

/// `get_pad_feature_gsettings` for a button: `…/VVVV:PPPP/buttonA/`.
pub fn pad_button_settings_path(vendor: u32, product: u32, button: u32) -> String {
    format!(
        "/org/gnome/desktop/peripherals/tablets/{vendor:04x}:{product:04x}/button{}/",
        tag(button)
    )
}

/// `lookup_pad_feature_settings`: `…/ringA-cw-mode-0/`, `…/stripB-up-mode-1/`.
pub fn pad_feature_settings_path(
    vendor: u32,
    product: u32,
    feature: PadFeature,
    number: u32,
    direction: PadDirection,
    mode: u32,
) -> String {
    let (kind, detail) = match (feature, direction) {
        (PadFeature::Ring, PadDirection::Cw) => ("ring", "cw"),
        (PadFeature::Ring, _) => ("ring", "ccw"),
        (PadFeature::Strip, PadDirection::Up) => ("strip", "up"),
        (PadFeature::Strip, _) => ("strip", "down"),
    };
    format!(
        "/org/gnome/desktop/peripherals/tablets/{vendor:04x}:{product:04x}/{kind}{}-{detail}-mode-{mode}/",
        tag(number)
    )
}

/// The two directions a feature's settings name, in the order
/// `meta_pad_action_mapper_handle_action` reads them (`settings1`,
/// `settings2`): the first is taken on up / clockwise.
pub fn feature_directions(feature: PadFeature) -> [PadDirection; 2] {
    match feature {
        PadFeature::Ring => [PadDirection::Cw, PadDirection::Ccw],
        PadFeature::Strip => [PadDirection::Up, PadDirection::Down],
    }
}

/// `last_pad_action_info` and `meta_pad_action_mapper_get_action_direction`:
/// the direction a ring or strip moved since the last event of the same
/// feature on the same pad. One tracker serves every pad, as in Mutter.
#[derive(Debug, Clone, Default)]
pub struct DirectionTracker {
    last: Option<(u32, PadFeature, u32, f64)>,
}

impl DirectionTracker {
    /// `value` is libinput's ring angle in degrees or strip position,
    /// negative when the finger lifted.
    pub fn direction(&mut self, pad: u32, feature: PadFeature, number: u32, value: f64) -> Option<PadDirection> {
        let (inc, dec, wraparound) = match feature {
            PadFeature::Ring => (PadDirection::Cw, PadDirection::Ccw, true),
            PadFeature::Strip => (PadDirection::Down, PadDirection::Up, false),
        };
        let last_value = self.last.map_or(0.0, |(_, _, _, v)| v);
        let in_range = value >= 0.0 && last_value >= 0.0;
        let mut direction = None;
        if let Some((last_pad, last_feature, last_number, last_value)) = self.last {
            if last_pad == pad && last_feature == feature && last_number == number && in_range {
                let mut delta = value - last_value;
                if wraparound {
                    if delta < -180.0 {
                        delta += 360.0;
                    } else if delta > 180.0 {
                        delta -= 360.0;
                    }
                }
                direction = Some(if delta > 0.0 { inc } else { dec });
            }
        }
        self.last = Some((pad, feature, number, value));
        direction
    }
}

/// What a pad button does (`meta_pad_action_mapper_handle_button`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PadButtonOutcome {
    /// Action `none`: the client receives the button.
    Deliver,
    /// Taken; on press, cycle the tablet's output.
    SwitchMonitor,
    /// Taken; on press, show the pad OSD.
    Help,
    /// Taken; press and release emulate this accelerator.
    Keybinding(String),
}

pub fn pad_button_outcome(settings: &PadButtonSettings) -> PadButtonOutcome {
    match settings.action {
        PadButtonAction::None => PadButtonOutcome::Deliver,
        PadButtonAction::Help => PadButtonOutcome::Help,
        PadButtonAction::SwitchMonitor => PadButtonOutcome::SwitchMonitor,
        PadButtonAction::Keybinding => PadButtonOutcome::Keybinding(settings.keybinding.clone()),
    }
}

/// `meta_pad_action_mapper_get_button_label` for a button that is no
/// mode switch (callers label mode switches with [`mode_switch_label`]).
pub fn pad_button_label(settings: &PadButtonSettings) -> Option<String> {
    match settings.action {
        PadButtonAction::Keybinding => Some(settings.keybinding.clone()),
        PadButtonAction::SwitchMonitor => Some("Switch monitor".into()),
        PadButtonAction::Help => Some("Show on-screen help".into()),
        PadButtonAction::None => None,
    }
}

/// The mode-switch button label (`get_button_label`'s group branch).
pub fn mode_switch_label(group: u32, n_groups: usize) -> String {
    if n_groups > 1 {
        format!("Mode Switch (Group {group})")
    } else {
        "Mode Switch".into()
    }
}

/// `get_ring_label` / `get_strip_label`: only keybindings label features.
pub fn feature_label(settings: Option<&PadButtonSettings>) -> Option<String> {
    settings
        .map(|s| s.keybinding.clone())
        .filter(|keybinding| !keybinding.is_empty())
}

/// `meta_display_notify_pad_group_switch`'s OSD text.
pub fn group_switch_osd(name: &str, mode: u32, n_modes: u32) -> String {
    let dots: Vec<&str> = (0..n_modes).map(|i| if i == mode { "●" } else { "○" }).collect();
    format!("{name}\n\n{}", dots.join(" "))
}

/// `cycle_logical_monitors`: from the monitor `output` names (`None` for
/// the span of all monitors) to the next. Integrated tablets skip the
/// span. `None` result is the span; with no monitors there is nothing.
pub fn cycle_monitor(current: Option<usize>, count: usize, skip_all: bool) -> Option<Option<usize>> {
    if count == 0 {
        return None;
    }
    Some(match current {
        None => Some(0),
        Some(index) if index + 1 < count => Some(index + 1),
        Some(_) if skip_all => Some(0),
        Some(_) => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use xkb::keysyms::*;

    #[test]
    fn accelerators_parse_like_mutter() {
        let combo = parse_accelerator("<Primary><Shift>z").unwrap();
        assert_eq!(combo.keysym, KEY_z);
        assert_eq!(combo.modifiers, CONTROL_MASK | SHIFT_MASK);
        assert_eq!(parse_accelerator("<ctl><Alt>Delete").unwrap().modifiers, CONTROL_MASK | MOD1_MASK);
        assert_eq!(parse_accelerator("<Super>a").unwrap().modifiers, SUPER_MASK);
        assert_eq!(parse_accelerator("<Mod4>a").unwrap().modifiers, MOD4_MASK);
        assert_eq!(parse_accelerator("<Meta>a").unwrap().modifiers, META_MASK);
        // Case-insensitive names, the XF86 fallback, unknown tokens skipped.
        assert_eq!(parse_accelerator("<Bogus>F5").unwrap().keysym, KEY_F5);
        assert_eq!(parse_accelerator("AudioMute").unwrap().keysym, KEY_XF86AudioMute);
        assert_eq!(parse_accelerator("Above_Tab").unwrap().keysym, KEY_ABOVE_TAB);
        assert_eq!(parse_accelerator("0x26").unwrap().keycode, 0x26);
        assert_eq!(parse_accelerator("disabled"), Some(KeyCombo { keysym: 0, keycode: 0, modifiers: 0 }));
        assert_eq!(parse_accelerator("<Control>NotAKey"), None);
    }

    #[test]
    fn keybindings_press_modifiers_first_and_release_them_last() {
        assert_eq!(
            keybinding_steps("<Control><Super>z", true).unwrap(),
            vec![
                KeyvalStep { keysym: KEY_Control_L, pressed: true },
                KeyvalStep { keysym: KEY_z, pressed: true },
            ],
            "Mutter 51 emulates no Super"
        );
        assert_eq!(
            keybinding_steps("<Shift><Alt>Tab", false).unwrap(),
            vec![
                KeyvalStep { keysym: KEY_Tab, pressed: false },
                KeyvalStep { keysym: KEY_Shift_L, pressed: false },
                KeyvalStep { keysym: KEY_Alt_L, pressed: false },
            ]
        );
        assert_eq!(keybinding_steps("", true), Some(Vec::new()));
        assert_eq!(keybinding_steps("<Shift>NoSuchKey", true), None);
    }

    #[test]
    fn pad_settings_paths_follow_mutter() {
        assert_eq!(
            pad_button_settings_path(0x56a, 0x357, 0),
            "/org/gnome/desktop/peripherals/tablets/056a:0357/buttonA/"
        );
        assert_eq!(
            pad_feature_settings_path(0x56a, 0x357, PadFeature::Ring, 0, PadDirection::Ccw, 3),
            "/org/gnome/desktop/peripherals/tablets/056a:0357/ringA-ccw-mode-3/"
        );
        assert_eq!(
            pad_feature_settings_path(0x56a, 0x357, PadFeature::Strip, 1, PadDirection::Up, 0),
            "/org/gnome/desktop/peripherals/tablets/056a:0357/stripB-up-mode-0/"
        );
    }

    #[test]
    fn ring_and_strip_directions_follow_mutter() {
        let mut tracker = DirectionTracker::default();
        // The first event of a feature only records it.
        assert_eq!(tracker.direction(1, PadFeature::Ring, 0, 10.0), None);
        assert_eq!(tracker.direction(1, PadFeature::Ring, 0, 20.0), Some(PadDirection::Cw));
        assert_eq!(tracker.direction(1, PadFeature::Ring, 0, 15.0), Some(PadDirection::Ccw));
        // Wrapping past 0° is still clockwise.
        assert_eq!(tracker.direction(1, PadFeature::Ring, 0, 350.0), Some(PadDirection::Ccw));
        assert_eq!(tracker.direction(1, PadFeature::Ring, 0, 5.0), Some(PadDirection::Cw));
        // A lifted finger (-1) and the next touch give nothing.
        assert_eq!(tracker.direction(1, PadFeature::Ring, 0, -1.0), None);
        assert_eq!(tracker.direction(1, PadFeature::Ring, 0, 90.0), None);
        // Another feature restarts; strips grow downwards.
        assert_eq!(tracker.direction(1, PadFeature::Strip, 0, 0.5), None);
        assert_eq!(tracker.direction(1, PadFeature::Strip, 0, 0.6), Some(PadDirection::Down));
        assert_eq!(tracker.direction(1, PadFeature::Strip, 0, 0.2), Some(PadDirection::Up));
        assert_eq!(tracker.direction(2, PadFeature::Strip, 0, 0.3), None);
    }

    #[test]
    fn pad_buttons_and_labels_follow_mutter() {
        let none = PadButtonSettings::default();
        assert_eq!(pad_button_outcome(&none), PadButtonOutcome::Deliver);
        assert_eq!(pad_button_label(&none), None);
        let keys = PadButtonSettings {
            action: PadButtonAction::Keybinding,
            keybinding: "<Control>z".into(),
        };
        assert_eq!(pad_button_outcome(&keys), PadButtonOutcome::Keybinding("<Control>z".into()));
        assert_eq!(pad_button_label(&keys).as_deref(), Some("<Control>z"));
        let help = PadButtonSettings {
            action: PadButtonAction::Help,
            keybinding: String::new(),
        };
        assert_eq!(pad_button_label(&help).as_deref(), Some("Show on-screen help"));
        assert_eq!(mode_switch_label(1, 2), "Mode Switch (Group 1)");
        assert_eq!(mode_switch_label(0, 1), "Mode Switch");
        assert_eq!(feature_label(Some(&none)), None);
        assert_eq!(feature_label(Some(&keys)).as_deref(), Some("<Control>z"));
        assert_eq!(group_switch_osd("Pad", 1, 4), "Pad\n\n○ ● ○ ○");
    }

    #[test]
    fn monitors_cycle_through_the_span_unless_integrated() {
        assert_eq!(cycle_monitor(None, 2, false), Some(Some(0)));
        assert_eq!(cycle_monitor(Some(0), 2, false), Some(Some(1)));
        assert_eq!(cycle_monitor(Some(1), 2, false), Some(None));
        assert_eq!(cycle_monitor(Some(1), 2, true), Some(Some(0)));
        assert_eq!(cycle_monitor(None, 0, false), None);
    }
}
