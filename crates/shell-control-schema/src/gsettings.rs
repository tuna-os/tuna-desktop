//! GNOME's input settings read from GSettings (#60), for every client
//! that tells the compositor about them: the shell for the user, and the
//! login screen for GDM's greeter user, as Mutter reads them itself in
//! every session mode. Feature `gio`.

use gio::prelude::*;

use crate::{
    DwellDirection, InputSettings, KeyboardAids, MotionPolicy, PointerAids, PointingStickSettings,
};

/// The schemas [`input_settings`] reads, in the order of its slice.
pub const INPUT_SCHEMAS: [&str; 13] = [
    "org.gnome.desktop.input-sources",
    "org.gnome.desktop.peripherals.keyboard",
    "org.gnome.desktop.peripherals.touchpad",
    "org.gnome.desktop.peripherals.mouse",
    "org.gnome.desktop.interface",
    "org.gnome.desktop.a11y.interface",
    "org.gnome.desktop.wm.preferences",
    "org.gnome.desktop.sound",
    "org.gnome.mutter",
    KEYBOARD_A11Y_SCHEMA,
    MOUSE_A11Y_SCHEMA,
    "org.gnome.desktop.peripherals.trackball",
    "org.gnome.desktop.peripherals.pointingstick",
];
/// GNOME's keyboard accessibility schema.
pub const KEYBOARD_A11Y_SCHEMA: &str = "org.gnome.desktop.a11y.keyboard";
/// GNOME's pointer accessibility schema.
pub const MOUSE_A11Y_SCHEMA: &str = "org.gnome.desktop.a11y.mouse";

/// `schema`'s settings, when the schema is installed.
pub fn settings(schema: &str) -> Option<gio::Settings> {
    let source = gio::SettingsSchemaSource::default()?;
    source.lookup(schema, true)?;
    Some(gio::Settings::new(schema))
}

/// Every [`INPUT_SCHEMAS`] schema that is installed.
pub fn input_schemas() -> Vec<Option<gio::Settings>> {
    INPUT_SCHEMAS.iter().map(|s| settings(s)).collect()
}

fn has(s: &gio::Settings, key: &str) -> bool {
    s.settings_schema()
        .is_some_and(|schema| schema.has_key(key))
}

/// GNOME's input settings from [`input_schemas`]; `right_to_left` is the
/// toolkit's default direction.
pub fn input_settings(all: &[Option<gio::Settings>], right_to_left: bool) -> InputSettings {
    let get = |index: usize| all.get(index).and_then(Option::as_ref);
    let mut out = InputSettings::default();
    if let Some(sources) = get(0) {
        let list: Vec<(String, String)> = sources.value("sources").get().unwrap_or_default();
        let (layout, variant) = xkb_from_sources(&list);
        out.xkb_layout = layout;
        out.xkb_variant = variant;
        out.xkb_options = sources
            .strv("xkb-options")
            .iter()
            .map(|o| o.to_string())
            .collect::<Vec<_>>()
            .join(",");
    }
    if let Some(kbd) = get(1) {
        out.repeat = kbd.boolean("repeat");
        out.repeat_delay_ms = kbd.uint("delay");
        out.repeat_interval_ms = kbd.uint("repeat-interval");
    }
    if let Some(pad) = get(2) {
        out.tap_to_click = pad.boolean("tap-to-click");
        out.touchpad_natural_scroll = pad.boolean("natural-scroll");
        out.touchpad_speed_milli = (pad.double("speed") * 1000.0).round() as i32;
        out.disable_while_typing = pad.boolean("disable-while-typing");
        out.touchpad = crate::TouchpadSettings {
            send_events: crate::SendEvents::from_gsettings(&pad.string("send-events")),
            click_method: crate::TouchpadClickMethod::from_gsettings(&pad.string("click-method")),
            edge_scrolling: pad.boolean("edge-scrolling-enabled"),
            two_finger_scrolling: pad.boolean("two-finger-scrolling-enabled"),
            tap_and_drag: pad.boolean("tap-and-drag"),
            tap_and_drag_lock: pad.boolean("tap-and-drag-lock"),
            tap_button_map: crate::TapButtonMap::from_gsettings(&pad.string("tap-button-map")),
            middle_click_emulation: pad.boolean("middle-click-emulation"),
            accel_profile: crate::PointerAccelProfile::from_gsettings(&pad.string("accel-profile")),
        };
    }
    // GNOME's 0..24 `scroll-wheel-emulation-button` and its lock; the
    // mouse schema gained them in gsettings-desktop-schemas 51.
    let wheel_emulation = |s: &gio::Settings| crate::ScrollWheelEmulation {
        button: if has(s, "scroll-wheel-emulation-button") {
            u32::try_from(s.int("scroll-wheel-emulation-button")).unwrap_or(0)
        } else {
            0
        },
        lock: has(s, "scroll-wheel-emulation-button-lock")
            && s.boolean("scroll-wheel-emulation-button-lock"),
    };
    if let Some(mouse) = get(3) {
        out.mouse_natural_scroll = mouse.boolean("natural-scroll");
        out.mouse_speed_milli = (mouse.double("speed") * 1000.0).round() as i32;
        out.mouse_left_handed = mouse.boolean("left-handed");
        out.mouse_accel_profile =
            crate::PointerAccelProfile::from_gsettings(&mouse.string("accel-profile"));
        out.mouse = crate::MouseSettings {
            middle_click_emulation: mouse.boolean("middle-click-emulation"),
            scroll_wheel_emulation: wheel_emulation(mouse),
        };
    }
    if let Some(trackball) = get(11) {
        out.trackball = crate::TrackballSettings {
            accel_profile: crate::PointerAccelProfile::from_gsettings(
                &trackball.string("accel-profile"),
            ),
            middle_click_emulation: trackball.boolean("middle-click-emulation"),
            scroll_wheel_emulation: wheel_emulation(trackball),
        };
    }
    if let Some(stick) = get(12) {
        out.pointing_stick = PointingStickSettings {
            speed_milli: (stick.double("speed") * 1000.0).round() as i32,
            accel_profile: crate::PointerAccelProfile::from_gsettings(
                &stick.string("accel-profile"),
            ),
            scroll_method: crate::PointingStickScrollMethod::from_gsettings(
                &stick.string("scroll-method"),
            ),
        };
    }
    out.touchpad_left_handed = touchpad_left_handed(
        get(2)
            .map(|pad| pad.string("left-handed"))
            .as_deref()
            .unwrap_or("mouse"),
        out.mouse_left_handed,
    );
    if let Some(iface) = get(4) {
        out.hot_corners = iface.boolean("enable-hot-corners");
        // GNOME's cursor and locate-pointer keys (#342): the compositor
        // draws its own cursors from them.
        out.cursor_theme = iface.string("cursor-theme").to_string();
        out.cursor_size = u32::try_from(iface.int("cursor-size")).unwrap_or(0);
        out.locate_pointer = iface.boolean("locate-pointer");
    }
    out.right_to_left = right_to_left;
    out.motion = motion_policy(get(4), get(5));
    if let Some(mutter) = get(8) {
        if has(mutter, "attach-modal-dialogs") {
            out.attach_modal_dialogs = mutter.boolean("attach-modal-dialogs");
        }
        if has(mutter, "center-new-windows") {
            out.center_new_windows = mutter.boolean("center-new-windows");
        }
        if has(mutter, "locate-pointer-key") {
            out.locate_pointer_key = mutter.string("locate-pointer-key").to_string();
        }
        if has(mutter, "dynamic-workspaces") {
            out.workspace_policy.dynamic = mutter.boolean("dynamic-workspaces");
        }
        if has(mutter, "workspaces-only-on-primary") {
            out.workspace_policy.only_on_primary = mutter.boolean("workspaces-only-on-primary");
        }
        if has(mutter, "edge-tiling") {
            out.workspace_policy.edge_tiling = mutter.boolean("edge-tiling");
        }
    }
    if let Some(wm) = get(6) {
        out.workspace_policy.fixed_count = wm.int("num-workspaces").clamp(1, 36) as u32;
        out.bell.audible = wm.boolean("audible-bell");
        out.bell.visual = wm.boolean("visual-bell");
        out.bell.frame_flash = wm.string("visual-bell-type") == "frame-flash";
    }
    if let Some(sound) = get(7) {
        out.bell.event_sounds = sound.boolean("event-sounds");
        out.bell.theme = sound.string("theme-name").to_string();
    }
    out.keyboard_aids = keyboard_aids(get(9));
    out.pointer_aids = pointer_aids(get(10));
    out
}

/// GNOME's keyboard accessibility settings, flattened for the compositor.
/// Without the schema every aid stays off (GNOME's defaults).
pub fn keyboard_aids(settings: Option<&gio::Settings>) -> KeyboardAids {
    let Some(s) = settings else {
        return KeyboardAids::default();
    };
    let ms = |key: &str| s.int(key).max(0) as u32;
    KeyboardAids {
        shortcuts: s.boolean("enable"),
        feature_beep: s.boolean("feature-state-change-beep"),
        sticky: s.boolean("stickykeys-enable"),
        sticky_two_key_off: s.boolean("stickykeys-two-key-off"),
        sticky_beep: s.boolean("stickykeys-modifier-beep"),
        slow: s.boolean("slowkeys-enable"),
        slow_delay_ms: ms("slowkeys-delay"),
        slow_beep_press: s.boolean("slowkeys-beep-press"),
        slow_beep_accept: s.boolean("slowkeys-beep-accept"),
        slow_beep_reject: s.boolean("slowkeys-beep-reject"),
        bounce: s.boolean("bouncekeys-enable"),
        bounce_delay_ms: ms("bouncekeys-delay"),
        bounce_beep_reject: s.boolean("bouncekeys-beep-reject"),
        toggle_beep: s.boolean("togglekeys-enable"),
        mouse: s.boolean("mousekeys-enable"),
        mouse_max_speed: ms("mousekeys-max-speed"),
        mouse_accel_ms: ms("mousekeys-accel-time"),
        mouse_init_delay_ms: ms("mousekeys-init-delay"),
    }
}

/// A dwell gesture's direction (`org.gnome.desktop.a11y.mouse`).
pub fn dwell_direction(name: &str) -> Option<DwellDirection> {
    match name {
        "left" => Some(DwellDirection::Left),
        "right" => Some(DwellDirection::Right),
        "up" => Some(DwellDirection::Up),
        "down" => Some(DwellDirection::Down),
        _ => None,
    }
}

/// GNOME's seconds as the compositor's milliseconds.
pub fn seconds_ms(seconds: f64) -> u32 {
    (seconds * 1000.0).round().clamp(0.0, f64::from(u32::MAX)) as u32
}

/// GNOME's pointer accessibility settings, flattened for the compositor.
/// Without the schema both aids stay off (GNOME's defaults).
pub fn pointer_aids(settings: Option<&gio::Settings>) -> PointerAids {
    let Some(s) = settings else {
        return PointerAids::default();
    };
    PointerAids {
        secondary_click: s.boolean("secondary-click-enabled"),
        secondary_click_ms: seconds_ms(s.double("secondary-click-time")),
        dwell_click: s.boolean("dwell-click-enabled"),
        dwell_ms: seconds_ms(s.double("dwell-time")),
        dwell_threshold: s.int("dwell-threshold").max(0) as u32,
        dwell_gesture: s.string("dwell-mode") == "gesture",
        gesture_single: dwell_direction(&s.string("dwell-gesture-single")),
        gesture_double: dwell_direction(&s.string("dwell-gesture-double")),
        gesture_drag: dwell_direction(&s.string("dwell-gesture-drag")),
        gesture_secondary: dwell_direction(&s.string("dwell-gesture-secondary")),
    }
}

/// `org.gnome.desktop.peripherals.touchpad left-handed`: `left`, `right`,
/// or `mouse` (follow the mouse).
pub fn touchpad_left_handed(policy: &str, mouse_left_handed: bool) -> bool {
    match policy {
        "left" => true,
        "right" => false,
        _ => mouse_left_handed,
    }
}

/// GNOME's input sources (`[('xkb', 'us'), ('xkb', 'de+nodeadkeys')]`)
/// as one xkb keymap: comma-separated layouts and variants in order.
/// Non-xkb sources (IBus engines) are skipped; none at all means `us`.
pub fn xkb_from_sources(sources: &[(String, String)]) -> (String, String) {
    let parts: Vec<(&str, &str)> = sources
        .iter()
        .filter(|(kind, _)| kind == "xkb")
        .map(|(_, id)| id.split_once('+').unwrap_or((id.as_str(), "")))
        .collect();
    if parts.is_empty() {
        return ("us".to_owned(), String::new());
    }
    let layouts: Vec<&str> = parts.iter().map(|(l, _)| *l).collect();
    let variants: Vec<&str> = parts.iter().map(|(_, v)| *v).collect();
    let variants = if variants.iter().all(|v| v.is_empty()) {
        String::new()
    } else {
        variants.join(",")
    };
    (layouts.join(","), variants)
}

/// GNOME's motion policy, purely from the raw keys; `None` is a key the
/// installed schemas lack (older GNOME), which keeps GNOME's default.
pub fn derive_motion(
    enable_animations: Option<bool>,
    reduced_motion: Option<&str>,
    slowdown_env: Option<&str>,
) -> MotionPolicy {
    MotionPolicy::from_gnome(
        enable_animations.unwrap_or(true),
        reduced_motion == Some("reduce"),
        MotionPolicy::parse_slowdown(slowdown_env),
    )
}

/// GNOME's motion policy from `org.gnome.desktop.interface` and
/// `org.gnome.desktop.a11y.interface` and GNOME Shell's slow-down
/// variable. GNOME 51's Reduced Motion is an enum independent of
/// enable-animations; keys missing from older schema sets are never read.
pub fn motion_policy(
    interface: Option<&gio::Settings>,
    a11y: Option<&gio::Settings>,
) -> MotionPolicy {
    let enable = interface
        .filter(|s| has(s, "enable-animations"))
        .map(|s| s.boolean("enable-animations"));
    let reduced = a11y
        .filter(|s| has(s, "reduced-motion"))
        .map(|s| s.string("reduced-motion"));
    let slowdown = std::env::var(crate::motion::SLOWDOWN_ENV).ok();
    derive_motion(enable, reduced.as_deref(), slowdown.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MotionLevel;

    #[test]
    fn gnome_keys_combine_into_three_levels() {
        let level = |enable, reduced| derive_motion(enable, reduced, None).level;
        assert_eq!(level(Some(true), Some("no-preference")), MotionLevel::Full);
        assert_eq!(level(Some(true), Some("reduce")), MotionLevel::FadeOnly);
        assert_eq!(level(Some(false), Some("no-preference")), MotionLevel::Off);
        assert_eq!(level(Some(false), Some("reduce")), MotionLevel::Off);
        // Older schema sets: missing keys keep GNOME's defaults.
        assert_eq!(level(None, None), MotionLevel::Full);
        assert_eq!(level(None, Some("reduce")), MotionLevel::FadeOnly);
        assert_eq!(level(Some(false), None), MotionLevel::Off);
    }

    #[test]
    fn slowdown_variable_scales_shell_durations() {
        let policy = derive_motion(Some(true), Some("reduce"), Some("2"));
        assert_eq!(policy.adjust_ms(150.0), 300.0);
        assert_eq!(
            derive_motion(Some(true), None, Some("bogus")).slowdown(),
            1.0
        );
        assert_eq!(
            derive_motion(Some(false), None, Some("2")).adjust_ms(150.0),
            0.0
        );
    }

    #[test]
    fn touchpad_handedness_overrides_or_tracks_mouse_changes() {
        for mouse in [false, true] {
            assert!(touchpad_left_handed("left", mouse));
            assert!(!touchpad_left_handed("right", mouse));
            assert_eq!(touchpad_left_handed("mouse", mouse), mouse);
        }
    }

    fn src(kind: &str, id: &str) -> (String, String) {
        (kind.to_owned(), id.to_owned())
    }

    #[test]
    fn input_sources_become_one_keymap() {
        assert_eq!(xkb_from_sources(&[]), ("us".into(), String::new()));
        assert_eq!(
            xkb_from_sources(&[src("xkb", "us"), src("xkb", "de")]),
            ("us,de".into(), String::new())
        );
        assert_eq!(
            xkb_from_sources(&[src("xkb", "us"), src("xkb", "de+nodeadkeys")]),
            ("us,de".into(), ",nodeadkeys".into())
        );
        assert_eq!(
            xkb_from_sources(&[src("ibus", "anthy"), src("xkb", "fr+bepo")]),
            ("fr".into(), "bepo".into())
        );
    }
}

#[cfg(test)]
mod pointer_tests {
    use super::*;

    #[test]
    fn settings_convert_like_gnome() {
        assert_eq!(seconds_ms(1.2), 1200);
        assert_eq!(seconds_ms(-1.0), 0);
        assert_eq!(dwell_direction("up"), Some(DwellDirection::Up));
        assert_eq!(dwell_direction("none"), None);
        assert_eq!(pointer_aids(None), PointerAids::default());
        assert_eq!(keyboard_aids(None), KeyboardAids::default());
    }
}
