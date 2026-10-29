//! GNOME 51 peripherals settings on libinput devices (#343).
//!
//! Which device class takes which `org.gnome.desktop.peripherals.*` key,
//! and the order the keys are applied in, follow Mutter 51.0
//! `src/backends/meta-input-settings.c` (`apply_device_settings` and the
//! `update_*` helpers); the libinput calls follow
//! `src/backends/native/meta-input-settings-native.c`. Device classes are
//! Mutter's capabilities (`meta-input-device-native.c`
//! `translate_device_capabilities`): a touchpad has tap fingers, and
//! trackballs and pointing sticks carry the udev `ID_INPUT_TRACKBALL` /
//! `ID_INPUT_POINTINGSTICK` property on the device or its parent.
//!
//! What the pinned bindings (input 0.9 / input-sys 1.19) cannot express
//! falls back to the device default and is reported unsupported in the
//! device snapshot: custom acceleration curves, the touchpad
//! disable-while-typing timeout, and sticky (rather than timeout) tap
//! drag lock.

use smithay::reexports::input as li;
use smithay::reexports::input::AsRaw;
use tuna_shell_control as shell;

/// Mutter's `ClutterInputCapabilities` that pick a settings family.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DeviceCaps {
    pub pointer: bool,
    pub touchpad: bool,
    pub trackball: bool,
    pub pointing_stick: bool,
}

impl DeviceCaps {
    /// Classify a libinput device as Mutter's native backend does.
    pub fn of(device: &li::Device) -> Self {
        // This device was created by the libinput udev context used by this backend.
        let udev = unsafe { device.udev_device() };
        let has = |property: &str| {
            udev.as_ref().is_some_and(|dev| {
                dev.property_value(property).is_some()
                    || dev
                        .parent()
                        .as_ref()
                        .is_some_and(|parent| parent.property_value(property).is_some())
            })
        };
        Self {
            pointer: device.has_capability(li::DeviceCapability::Pointer),
            touchpad: device.config_tap_finger_count() > 0,
            trackball: has("ID_INPUT_TRACKBALL"),
            pointing_stick: has("ID_INPUT_POINTINGSTICK"),
        }
    }

    /// POINTER without TOUCHPAD, TRACKBALL or TRACKPOINT: Mutter's mouse
    /// for acceleration, middle-click emulation and scroll-wheel emulation.
    fn is_mouse(self) -> bool {
        self.pointer && !self.touchpad && !self.trackball && !self.pointing_stick
    }

    /// The snapshot's coarse kind; trackballs and pointing sticks are
    /// pointers with their own flags.
    fn kind(self) -> &'static str {
        if self.touchpad {
            "touchpad"
        } else if self.pointer {
            "pointer"
        } else {
            "other"
        }
    }
}

/// Touchpad-only keys (Mutter requires TOUCHPAD for each).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TouchpadPolicy {
    pub tap_to_click: bool,
    pub tap_button_map: shell::TapButtonMap,
    pub tap_and_drag: bool,
    pub tap_and_drag_lock: bool,
    pub disable_while_typing: bool,
    pub send_events: shell::SendEvents,
    pub click_method: shell::TouchpadClickMethod,
}

/// The one scroll-method family a device takes. Mutter applies touchpad
/// scrolling, then mouse and trackball scroll-wheel emulation, then the
/// pointing-stick method, so the last that matches wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollPolicy {
    /// `two-finger-scrolling-enabled` / `edge-scrolling-enabled`, plus
    /// whether any present touchpad supports two-finger scrolling.
    Touchpad {
        two_finger: bool,
        edge: bool,
        two_finger_available: bool,
    },
    /// Mouse or trackball `scroll-wheel-emulation-button` and `-lock`.
    Button(shell::ScrollWheelEmulation),
    /// Pointing-stick `scroll-method`.
    PointingStick(shell::PointingStickScrollMethod),
}

/// One device's libinput configuration, resolved from GNOME's settings
/// for its [`DeviceCaps`]. `None` leaves that option alone: no GNOME key
/// applies to the device class.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LibinputPolicy {
    pub left_handed: Option<bool>,
    pub natural_scroll: Option<bool>,
    /// GNOME's -1..1.
    pub speed: Option<f64>,
    pub accel_profile: Option<shell::PointerAccelProfile>,
    pub middle_click_emulation: Option<bool>,
    pub touchpad: Option<TouchpadPolicy>,
    pub scroll: Option<ScrollPolicy>,
}

/// Resolve one device's configuration from GNOME's settings, with Mutter
/// 51's per-class rules. `two_finger_available` is whether any present
/// touchpad supports two-finger scrolling (Mutter's `two_finger_devices`).
pub fn libinput_policy(
    settings: &shell::InputSettings,
    caps: DeviceCaps,
    two_finger_available: bool,
) -> LibinputPolicy {
    let speed = |milli: i32| f64::from(milli.clamp(-1000, 1000)) / 1000.0;
    let mouse = caps.is_mouse();
    // Later entries win, in `apply_device_settings` order.
    let last = |entries: &[(bool, shell::PointerAccelProfile)]| {
        entries
            .iter()
            .rev()
            .find(|(applies, _)| *applies)
            .map(|e| e.1)
    };
    let accel_profile = last(&[
        (mouse, settings.mouse_accel_profile),
        (caps.touchpad, settings.touchpad.accel_profile),
        (caps.trackball, settings.trackball.accel_profile),
        (caps.pointing_stick, settings.pointing_stick.accel_profile),
    ]);
    let middle_click_emulation = [
        (mouse, settings.mouse.middle_click_emulation),
        (caps.touchpad, settings.touchpad.middle_click_emulation),
        (caps.trackball, settings.trackball.middle_click_emulation),
    ]
    .into_iter()
    .rev()
    .find(|(applies, _)| *applies)
    .map(|entry| entry.1);
    let trackball_button = caps.pointer && caps.trackball && !caps.touchpad && !caps.pointing_stick;
    let scroll = if caps.pointing_stick {
        Some(ScrollPolicy::PointingStick(
            settings.pointing_stick.scroll_method,
        ))
    } else if mouse {
        Some(ScrollPolicy::Button(settings.mouse.scroll_wheel_emulation))
    } else if trackball_button {
        Some(ScrollPolicy::Button(
            settings.trackball.scroll_wheel_emulation,
        ))
    } else if caps.touchpad {
        Some(ScrollPolicy::Touchpad {
            two_finger: settings.touchpad.two_finger_scrolling,
            edge: settings.touchpad.edge_scrolling,
            two_finger_available,
        })
    } else {
        None
    };
    LibinputPolicy {
        left_handed: if caps.touchpad {
            Some(settings.touchpad_left_handed)
        } else if caps.pointer {
            Some(settings.mouse_left_handed)
        } else {
            None
        },
        natural_scroll: if caps.touchpad {
            Some(settings.touchpad_natural_scroll)
        } else if caps.pointer {
            Some(settings.mouse_natural_scroll)
        } else {
            None
        },
        speed: if caps.touchpad {
            Some(speed(settings.touchpad_speed_milli))
        } else if caps.pointing_stick {
            Some(speed(settings.pointing_stick.speed_milli))
        } else if caps.pointer {
            Some(speed(settings.mouse_speed_milli))
        } else {
            None
        },
        accel_profile,
        middle_click_emulation,
        touchpad: caps.touchpad.then_some(TouchpadPolicy {
            tap_to_click: settings.tap_to_click,
            tap_button_map: settings.touchpad.tap_button_map,
            tap_and_drag: settings.touchpad.tap_and_drag,
            tap_and_drag_lock: settings.touchpad.tap_and_drag_lock,
            disable_while_typing: settings.disable_while_typing,
            send_events: settings.touchpad.send_events,
            click_method: settings.touchpad.click_method,
        }),
        scroll,
    }
}

const SCROLL_TWO_FINGER: u32 = li::ffi::libinput_config_scroll_method_LIBINPUT_CONFIG_SCROLL_2FG;
const SCROLL_EDGE: u32 = li::ffi::libinput_config_scroll_method_LIBINPUT_CONFIG_SCROLL_EDGE;

/// Replay Mutter 51's touchpad scroll-method calls on one device:
/// `update_touchpad_two_finger_scroll` (which updates edge scrolling
/// first when two-finger is on, and after when off) and then
/// `update_touchpad_edge_scroll`. Each step clears its own bit from the
/// current method and ORs in its own; libinput accepts only a single
/// supported method (or none), so a combined or unsupported request
/// leaves the current one. Edge scrolling yields when both are on and
/// some touchpad can scroll with two fingers. Methods are libinput's
/// `libinput_config_scroll_method` bits.
pub fn touchpad_scroll_method(
    current: u32,
    supported: u32,
    two_finger: bool,
    edge: bool,
    two_finger_available: bool,
) -> u32 {
    let edge = edge && !(two_finger && two_finger_available);
    let steps: &[(u32, bool)] = if two_finger {
        &[
            (SCROLL_EDGE, edge),
            (SCROLL_TWO_FINGER, true),
            (SCROLL_EDGE, edge),
        ]
    } else {
        &[
            (SCROLL_TWO_FINGER, false),
            (SCROLL_EDGE, edge),
            (SCROLL_EDGE, edge),
        ]
    };
    steps.iter().fold(current, |current, &(bit, on)| {
        let wanted = (current & !bit) | if on { bit } else { 0 };
        if (wanted == 0 || wanted.is_power_of_two()) && supported & wanted == wanted {
            wanted
        } else {
            current
        }
    })
}

/// Mutter's `meta_clutter_button_to_evdev` (`meta-backend.c`): GNOME's
/// button numbers to evdev codes.
pub fn scroll_button_evcode(button: u32) -> u32 {
    const BTN_LEFT: u32 = 0x110;
    const BTN_RIGHT: u32 = 0x111;
    const BTN_MIDDLE: u32 = 0x112;
    match button {
        1 => BTN_LEFT,
        2 => BTN_MIDDLE,
        3 => BTN_RIGHT,
        n => (n + (BTN_LEFT - 1)).wrapping_sub(4),
    }
}

/// The libinput click method Mutter sets for GNOME's `click-method`;
/// `default` takes the device default. Values are
/// `libinput_config_click_method`.
pub fn click_method_value(method: shell::TouchpadClickMethod, device_default: u32) -> u32 {
    match method {
        shell::TouchpadClickMethod::Default => device_default,
        shell::TouchpadClickMethod::None => {
            li::ffi::libinput_config_click_method_LIBINPUT_CONFIG_CLICK_METHOD_NONE
        }
        shell::TouchpadClickMethod::Areas => {
            li::ffi::libinput_config_click_method_LIBINPUT_CONFIG_CLICK_METHOD_BUTTON_AREAS
        }
        shell::TouchpadClickMethod::Fingers => {
            li::ffi::libinput_config_click_method_LIBINPUT_CONFIG_CLICK_METHOD_CLICKFINGER
        }
    }
}

/// GNOME's `send-events` as libinput's mode.
pub fn send_events_mode(mode: shell::SendEvents) -> li::SendEventsMode {
    match mode {
        shell::SendEvents::Enabled => li::SendEventsMode::ENABLED,
        shell::SendEvents::Disabled => li::SendEventsMode::DISABLED,
        shell::SendEvents::DisabledOnExternalMouse => {
            li::SendEventsMode::DISABLED_ON_EXTERNAL_MOUSE
        }
    }
}

/// GNOME's `tap-button-map`; `default` takes the device default.
pub fn tap_button_map(
    map: shell::TapButtonMap,
    device_default: Option<li::TapButtonMap>,
) -> Option<li::TapButtonMap> {
    match map {
        shell::TapButtonMap::Default => device_default,
        shell::TapButtonMap::Lrm => Some(li::TapButtonMap::LeftRightMiddle),
        shell::TapButtonMap::Lmr => Some(li::TapButtonMap::LeftMiddleRight),
    }
}

/// Mutter's `set_device_accel_profile`: the requested profile if the
/// device has it, else the device default. Custom curves cannot be set
/// through the pinned bindings and take the default as well.
pub fn resolve_accel_profile(
    requested: shell::PointerAccelProfile,
    available: &[li::AccelProfile],
    default: Option<li::AccelProfile>,
) -> Option<li::AccelProfile> {
    let desired = match requested {
        shell::PointerAccelProfile::Flat => Some(li::AccelProfile::Flat),
        shell::PointerAccelProfile::Adaptive => Some(li::AccelProfile::Adaptive),
        shell::PointerAccelProfile::Default | shell::PointerAccelProfile::Custom => None,
    };
    desired
        .filter(|profile| available.contains(profile))
        .or(default)
}

/// Whether this touchpad counts toward Mutter's `two_finger_devices`.
pub fn has_two_finger_scroll(device: &li::Device) -> bool {
    DeviceCaps::of(device).touchpad
        && device
            .config_scroll_methods()
            .contains(&li::ScrollMethod::TwoFinger)
}

fn report(result: Result<(), li::DeviceConfigError>, device: &li::Device, option: &str) {
    if let Err(error) = result {
        eprintln!(
            "tuna-compositor: libinput: {option} rejected for {}: {error:?}",
            device.name()
        );
    }
}

/// Apply GNOME's settings to one device, in Mutter's
/// `apply_device_settings` order, skipping options the device lacks as
/// Mutter's native backend does.
pub fn apply(settings: &shell::InputSettings, device: &mut li::Device, two_finger_available: bool) {
    let policy = libinput_policy(settings, DeviceCaps::of(device), two_finger_available);
    if let Some(speed) = policy.speed {
        let _ = device.config_accel_set_speed(speed);
    }
    if let Some(natural) = policy.natural_scroll {
        if device.config_scroll_has_natural_scroll() {
            let _ = device.config_scroll_set_natural_scroll_enabled(natural);
        }
    }
    if let Some(left_handed) = policy.left_handed {
        apply_handedness(device, left_handed);
    }
    if let Some(profile) = policy.accel_profile {
        apply_accel_profile(device, profile);
    }
    if let Some(touchpad) = policy.touchpad {
        apply_touchpad(device, touchpad);
    }
    match policy.scroll {
        Some(ScrollPolicy::Touchpad {
            two_finger,
            edge,
            two_finger_available,
        }) => {
            let bits = |methods: &[li::ScrollMethod]| {
                methods
                    .iter()
                    .fold(0, |bits, method| bits | scroll_bits(*method))
            };
            let current = device.config_scroll_method().map_or(0, scroll_bits);
            let supported = bits(&device.config_scroll_methods());
            let wanted =
                touchpad_scroll_method(current, supported, two_finger, edge, two_finger_available);
            if wanted != current {
                if let Some(method) = scroll_method(wanted) {
                    report(
                        device.config_scroll_set_method(method),
                        device,
                        "touchpad scroll method",
                    );
                }
            }
        }
        Some(ScrollPolicy::Button(emulation)) => apply_scroll_button(device, emulation),
        Some(ScrollPolicy::PointingStick(method)) => {
            let method = match method {
                shell::PointingStickScrollMethod::Default => device.config_scroll_default_method(),
                shell::PointingStickScrollMethod::None => Some(li::ScrollMethod::NoScroll),
                shell::PointingStickScrollMethod::OnButtonDown => {
                    Some(li::ScrollMethod::OnButtonDown)
                }
            };
            if let Some(method) = method {
                report(
                    device.config_scroll_set_method(method),
                    device,
                    "pointing-stick scroll method",
                );
            }
        }
        None => {}
    }
    if let Some(enabled) = policy.middle_click_emulation {
        if device.config_middle_emulation_is_available() {
            report(
                device.config_middle_emulation_set_enabled(enabled),
                device,
                "middle-click emulation",
            );
        }
    }
}

fn apply_touchpad(device: &mut li::Device, policy: TouchpadPolicy) {
    if device.config_tap_finger_count() > 0 {
        let _ = device.config_tap_set_enabled(policy.tap_to_click);
        if let Some(map) = tap_button_map(
            policy.tap_button_map,
            device.config_tap_default_button_map(),
        ) {
            report(
                device.config_tap_set_button_map(map),
                device,
                "tap button map",
            );
        }
        report(
            device.config_tap_set_drag_enabled(policy.tap_and_drag),
            device,
            "tap-and-drag",
        );
        // The bindings only name LIBINPUT_CONFIG_DRAG_LOCK_ENABLED (libinput
        // >= 1.27 calls it ENABLED_TIMEOUT); Mutter 51 asks for STICKY.
        report(
            device.config_tap_set_drag_lock_enabled(policy.tap_and_drag_lock),
            device,
            "tap-and-drag lock",
        );
    }
    if device.config_dwt_is_available() {
        let _ = device.config_dwt_set_enabled(policy.disable_while_typing);
    }
    // libinput itself suspends a DISABLED_ON_EXTERNAL_MOUSE touchpad while
    // an external mouse is present, across mouse hotplug.
    report(
        device.config_send_events_set_mode(send_events_mode(policy.send_events)),
        device,
        "touchpad send-events mode",
    );
    // The bindings' `ClickMethod` has no NONE, so set GNOME's method by its
    // libinput value. `device` owns a live libinput_device reference.
    let status = unsafe {
        let raw = device.as_raw_mut();
        let default = li::ffi::libinput_device_config_click_get_default_method(raw);
        li::ffi::libinput_device_config_click_set_method(
            raw,
            click_method_value(policy.click_method, default),
        )
    };
    if status != li::ffi::libinput_config_status_LIBINPUT_CONFIG_STATUS_SUCCESS {
        eprintln!(
            "tuna-compositor: libinput: touchpad click method {:?} rejected for {}",
            policy.click_method,
            device.name()
        );
    }
}

/// Mutter's `set_scroll_button`: button 0 turns button scrolling off.
fn apply_scroll_button(device: &mut li::Device, emulation: shell::ScrollWheelEmulation) {
    let (method, button) = if emulation.button == 0 {
        (li::ScrollMethod::NoScroll, 0)
    } else {
        (
            li::ScrollMethod::OnButtonDown,
            scroll_button_evcode(emulation.button),
        )
    };
    if let Err(error) = device.config_scroll_set_method(method) {
        if emulation.button != 0 {
            eprintln!(
                "tuna-compositor: libinput: scroll-wheel emulation unsupported for {}: {error:?}",
                device.name()
            );
        }
        return;
    }
    let _ = device.config_scroll_set_button(button);
    let _ = device.config_scroll_set_button_lock(if emulation.lock {
        li::ScrollButtonLockState::Enabled
    } else {
        li::ScrollButtonLockState::Disabled
    });
}

fn apply_accel_profile(device: &mut li::Device, requested: shell::PointerAccelProfile) {
    let available = device.config_accel_profiles();
    let selected =
        resolve_accel_profile(requested, &available, device.config_accel_default_profile());
    let exact = match requested {
        shell::PointerAccelProfile::Flat => Some(li::AccelProfile::Flat),
        shell::PointerAccelProfile::Adaptive => Some(li::AccelProfile::Adaptive),
        shell::PointerAccelProfile::Default | shell::PointerAccelProfile::Custom => None,
    };
    if requested == shell::PointerAccelProfile::Custom {
        eprintln!("tuna-compositor: libinput: custom pointer acceleration is unsupported by the compositor for {}; using device default", device.name());
    } else if exact.is_some() && selected != exact {
        eprintln!("tuna-compositor: libinput: requested pointer acceleration profile unsupported for {}; using device default", device.name());
    }
    if let Some(profile) = selected {
        report(
            device.config_accel_set_profile(profile),
            device,
            "pointer acceleration profile",
        );
    }
}

fn apply_handedness(device: &li::Device, left_handed: bool) {
    if device.config_left_handed_is_available() {
        if let Err(error) = device.config_left_handed_set(left_handed) {
            eprintln!(
                "tuna-compositor: libinput: primary button setting rejected for {}: {error:?}",
                device.name()
            );
        }
    } else if left_handed {
        eprintln!(
            "tuna-compositor: libinput: primary button swapping unsupported for {}",
            device.name()
        );
    }
}

fn scroll_bits(method: li::ScrollMethod) -> u32 {
    match method {
        li::ScrollMethod::NoScroll => {
            li::ffi::libinput_config_scroll_method_LIBINPUT_CONFIG_SCROLL_NO_SCROLL
        }
        li::ScrollMethod::TwoFinger => SCROLL_TWO_FINGER,
        li::ScrollMethod::Edge => SCROLL_EDGE,
        li::ScrollMethod::OnButtonDown => {
            li::ffi::libinput_config_scroll_method_LIBINPUT_CONFIG_SCROLL_ON_BUTTON_DOWN
        }
        _ => 0,
    }
}

fn scroll_method(bits: u32) -> Option<li::ScrollMethod> {
    [
        li::ScrollMethod::NoScroll,
        li::ScrollMethod::TwoFinger,
        li::ScrollMethod::Edge,
        li::ScrollMethod::OnButtonDown,
    ]
    .into_iter()
    .find(|method| scroll_bits(*method) == bits)
}

fn accel_profile_name(profile: li::AccelProfile) -> &'static str {
    match profile {
        li::AccelProfile::Flat => "flat",
        li::AccelProfile::Adaptive => "adaptive",
        _ => "unknown",
    }
}

fn scroll_method_name(method: li::ScrollMethod) -> &'static str {
    match method {
        li::ScrollMethod::NoScroll => "no-scroll",
        li::ScrollMethod::TwoFinger => "two-finger",
        li::ScrollMethod::Edge => "edge",
        li::ScrollMethod::OnButtonDown => "on-button-down",
        _ => "unknown",
    }
}

fn click_method_name(method: Option<li::ClickMethod>) -> &'static str {
    match method {
        None => "none",
        Some(li::ClickMethod::ButtonAreas) => "button-areas",
        Some(li::ClickMethod::Clickfinger) => "clickfinger",
        Some(_) => "unknown",
    }
}

fn send_events_names(mode: li::SendEventsMode) -> Vec<&'static str> {
    let mut names = Vec::new();
    if mode.contains(li::SendEventsMode::DISABLED) {
        names.push("disabled");
    }
    if mode.contains(li::SendEventsMode::DISABLED_ON_EXTERNAL_MOUSE) {
        names.push("disabled-on-external-mouse");
    }
    names
}

/// Live libinput state for one device: what libinput itself reports
/// back, with per-option availability, so proofs assert applied device
/// state rather than the settings echo and unsupported options are
/// visible instead of silently ignored. Only configuration, no input.
pub fn device_state(device: &li::Device) -> serde_json::Value {
    let caps = DeviceCaps::of(device);
    let tapping = device.config_tap_finger_count() > 0;
    // The bindings' getter panics on libinput >= 1.27's STICKY value, so
    // read the raw state. `device` owns a live libinput_device reference.
    let drag_lock =
        unsafe { li::ffi::libinput_device_config_tap_get_drag_lock_enabled(device.as_raw_mut()) };
    let mode = device.config_send_events_mode();
    serde_json::json!({
        "name": device.name(),
        "kind": caps.kind(),
        "trackball": caps.trackball,
        "pointing_stick": caps.pointing_stick,
        "tap_fingers": device.config_tap_finger_count(),
        "left_handed": device.config_left_handed(),
        "handed_available": device.config_left_handed_is_available(),
        "tap_enabled": device.config_tap_enabled(),
        "tap_button_map": tapping.then(|| match device.config_tap_button_map() {
            Some(li::TapButtonMap::LeftMiddleRight) => "lmr",
            _ => "lrm",
        }),
        "tap_drag_enabled": device.config_tap_drag_enabled(),
        "tap_drag_lock": match drag_lock {
            0 => "disabled",
            1 => "timeout",
            2 => "sticky",
            _ => "unknown",
        },
        "tap_drag_lock_sticky_supported_by_compositor": false,
        "natural_scroll": device.config_scroll_natural_scroll_enabled(),
        "natural_available": device.config_scroll_has_natural_scroll(),
        "accel_speed": device.config_accel_speed(),
        "accel_available": device.config_accel_is_available(),
        "accel_profile": device.config_accel_profile().map(accel_profile_name),
        "accel_profiles": device.config_accel_profiles().into_iter().map(accel_profile_name).collect::<Vec<_>>(),
        "accel_custom_supported_by_compositor": false,
        "dwt_enabled": device.config_dwt_enabled(),
        "dwt_available": device.config_dwt_is_available(),
        "dwt_timeout_supported_by_compositor": false,
        "middle_emulation_enabled": device.config_middle_emulation_enabled(),
        "middle_emulation_available": device.config_middle_emulation_is_available(),
        "send_events_mode": send_events_names(mode).first().copied().unwrap_or("enabled"),
        "send_events_modes": send_events_names(device.config_send_events_modes()),
        "click_method": (!device.config_click_methods().is_empty())
            .then(|| click_method_name(device.config_click_method())),
        "click_methods": device.config_click_methods().into_iter().map(|m| click_method_name(Some(m))).collect::<Vec<_>>(),
        "scroll_method": device.config_scroll_method().map(scroll_method_name),
        "scroll_methods": device.config_scroll_methods().into_iter().map(scroll_method_name).collect::<Vec<_>>(),
        "scroll_button": device.config_scroll_button(),
        "scroll_button_lock": device.config_scroll_button_lock() == li::ScrollButtonLockState::Enabled,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use shell::{
        InputSettings, PointerAccelProfile, PointingStickScrollMethod, ScrollWheelEmulation,
        SendEvents, TapButtonMap, TouchpadClickMethod,
    };

    const MOUSE: DeviceCaps = DeviceCaps {
        pointer: true,
        touchpad: false,
        trackball: false,
        pointing_stick: false,
    };
    const TOUCHPAD: DeviceCaps = DeviceCaps {
        pointer: true,
        touchpad: true,
        trackball: false,
        pointing_stick: false,
    };
    const TRACKBALL: DeviceCaps = DeviceCaps {
        pointer: true,
        touchpad: false,
        trackball: true,
        pointing_stick: false,
    };
    const POINTING_STICK: DeviceCaps = DeviceCaps {
        pointer: true,
        touchpad: false,
        trackball: false,
        pointing_stick: true,
    };

    /// Distinct values for every device family, so a swapped family shows.
    fn pointer_settings() -> InputSettings {
        let mut settings = InputSettings {
            tap_to_click: true,
            touchpad_natural_scroll: true,
            touchpad_speed_milli: -500,
            disable_while_typing: false,
            touchpad_left_handed: true,
            mouse_natural_scroll: false,
            mouse_speed_milli: 250,
            mouse_left_handed: false,
            mouse_accel_profile: PointerAccelProfile::Flat,
            ..InputSettings::default()
        };
        settings.touchpad.send_events = SendEvents::DisabledOnExternalMouse;
        settings.touchpad.click_method = TouchpadClickMethod::Areas;
        settings.touchpad.edge_scrolling = true;
        settings.touchpad.two_finger_scrolling = false;
        settings.touchpad.tap_and_drag = false;
        settings.touchpad.tap_and_drag_lock = true;
        settings.touchpad.tap_button_map = TapButtonMap::Lmr;
        settings.touchpad.middle_click_emulation = true;
        settings.touchpad.accel_profile = PointerAccelProfile::Adaptive;
        settings.mouse.middle_click_emulation = false;
        settings.mouse.scroll_wheel_emulation = ScrollWheelEmulation {
            button: 2,
            lock: true,
        };
        settings.trackball.accel_profile = PointerAccelProfile::Custom;
        settings.trackball.middle_click_emulation = true;
        settings.trackball.scroll_wheel_emulation = ScrollWheelEmulation {
            button: 8,
            lock: false,
        };
        settings.pointing_stick.speed_milli = 750;
        settings.pointing_stick.accel_profile = PointerAccelProfile::Default;
        settings.pointing_stick.scroll_method = PointingStickScrollMethod::None;
        settings
    }

    #[test]
    fn touchpad_policy_carries_the_touchpad_keys() {
        let policy = libinput_policy(&pointer_settings(), TOUCHPAD, true);
        assert_eq!(policy.left_handed, Some(true));
        assert_eq!(policy.natural_scroll, Some(true));
        assert_eq!(policy.speed, Some(-0.5));
        assert_eq!(policy.accel_profile, Some(PointerAccelProfile::Adaptive));
        assert_eq!(policy.middle_click_emulation, Some(true));
        assert_eq!(
            policy.touchpad,
            Some(TouchpadPolicy {
                tap_to_click: true,
                tap_button_map: TapButtonMap::Lmr,
                tap_and_drag: false,
                tap_and_drag_lock: true,
                disable_while_typing: false,
                send_events: SendEvents::DisabledOnExternalMouse,
                click_method: TouchpadClickMethod::Areas,
            })
        );
        assert_eq!(
            policy.scroll,
            Some(ScrollPolicy::Touchpad {
                two_finger: false,
                edge: true,
                two_finger_available: true,
            })
        );
    }

    #[test]
    fn pointer_policy_carries_the_mouse_keys() {
        let policy = libinput_policy(&pointer_settings(), MOUSE, true);
        assert_eq!(policy.left_handed, Some(false));
        assert_eq!(policy.natural_scroll, Some(false));
        assert_eq!(policy.speed, Some(0.25));
        assert_eq!(policy.accel_profile, Some(PointerAccelProfile::Flat));
        assert_eq!(policy.middle_click_emulation, Some(false));
        assert_eq!(policy.touchpad, None);
        assert_eq!(
            policy.scroll,
            Some(ScrollPolicy::Button(ScrollWheelEmulation {
                button: 2,
                lock: true,
            }))
        );
    }

    #[test]
    fn trackball_policy_takes_mouse_motion_and_trackball_buttons() {
        // Mutter: mouse speed, natural scroll and handedness (POINTER
        // minus TOUCHPAD[|TRACKPOINT]); trackball acceleration, middle
        // click and scroll-wheel emulation.
        let policy = libinput_policy(&pointer_settings(), TRACKBALL, false);
        assert_eq!(policy.left_handed, Some(false));
        assert_eq!(policy.natural_scroll, Some(false));
        assert_eq!(policy.speed, Some(0.25));
        assert_eq!(policy.accel_profile, Some(PointerAccelProfile::Custom));
        assert_eq!(policy.middle_click_emulation, Some(true));
        assert_eq!(policy.touchpad, None);
        assert_eq!(
            policy.scroll,
            Some(ScrollPolicy::Button(ScrollWheelEmulation {
                button: 8,
                lock: false,
            }))
        );
    }

    #[test]
    fn pointing_stick_policy_takes_its_own_speed_profile_and_scroll_method() {
        // Mutter: pointing-stick speed, acceleration and scroll method;
        // mouse handedness and natural scroll; no middle-click key.
        let policy = libinput_policy(&pointer_settings(), POINTING_STICK, false);
        assert_eq!(policy.left_handed, Some(false));
        assert_eq!(policy.natural_scroll, Some(false));
        assert_eq!(policy.speed, Some(0.75));
        assert_eq!(policy.accel_profile, Some(PointerAccelProfile::Default));
        assert_eq!(policy.middle_click_emulation, None);
        assert_eq!(policy.touchpad, None);
        assert_eq!(
            policy.scroll,
            Some(ScrollPolicy::PointingStick(PointingStickScrollMethod::None))
        );
    }

    #[test]
    fn other_devices_are_left_alone() {
        let policy = libinput_policy(&pointer_settings(), DeviceCaps::default(), true);
        assert_eq!(
            policy,
            LibinputPolicy {
                left_handed: None,
                natural_scroll: None,
                speed: None,
                accel_profile: None,
                middle_click_emulation: None,
                touchpad: None,
                scroll: None,
            }
        );
    }

    #[test]
    fn pointer_speed_clamps_to_gnomes_range() {
        let mut settings = pointer_settings();
        settings.mouse_speed_milli = 5000;
        settings.touchpad_speed_milli = -5000;
        settings.pointing_stick.speed_milli = 5000;
        assert_eq!(libinput_policy(&settings, MOUSE, false).speed, Some(1.0));
        assert_eq!(
            libinput_policy(&settings, TOUCHPAD, false).speed,
            Some(-1.0)
        );
        assert_eq!(
            libinput_policy(&settings, POINTING_STICK, false).speed,
            Some(1.0)
        );
    }

    #[test]
    fn later_mutter_classes_win_on_combined_capabilities() {
        // apply_device_settings order: trackball and pointing-stick
        // settings land after the touchpad's.
        let settings = pointer_settings();
        let both = DeviceCaps {
            trackball: true,
            pointing_stick: true,
            ..TOUCHPAD
        };
        let policy = libinput_policy(&settings, both, true);
        assert_eq!(policy.accel_profile, Some(PointerAccelProfile::Default));
        assert_eq!(policy.middle_click_emulation, Some(true));
        assert_eq!(
            policy.scroll,
            Some(ScrollPolicy::PointingStick(PointingStickScrollMethod::None))
        );
        // A pointer-less trackball takes no scroll-wheel emulation.
        let bare = DeviceCaps {
            pointer: false,
            ..TRACKBALL
        };
        assert_eq!(libinput_policy(&settings, bare, true).scroll, None);
    }

    const NONE: u32 = 0;
    const TWO: u32 = SCROLL_TWO_FINGER;
    const EDGE: u32 = SCROLL_EDGE;

    #[test]
    fn touchpad_scrolling_replays_mutters_two_finger_then_edge_sequence() {
        let both = TWO | EDGE;
        // Two-finger preferred when both are on and available.
        assert_eq!(touchpad_scroll_method(EDGE, both, true, true, true), TWO);
        assert_eq!(touchpad_scroll_method(NONE, both, true, false, true), TWO);
        // Two-finger off: edge if asked, else none.
        assert_eq!(touchpad_scroll_method(TWO, both, false, true, true), EDGE);
        assert_eq!(touchpad_scroll_method(TWO, both, false, false, true), NONE);
        // Both on but no touchpad can scroll with two fingers: libinput
        // refuses the combined method, edge scrolling stays.
        assert_eq!(touchpad_scroll_method(EDGE, EDGE, true, true, false), EDGE);
        // This pad lacks two-finger while another has it: edge yields.
        assert_eq!(touchpad_scroll_method(EDGE, EDGE, true, true, true), NONE);
        // Two-finger off, edge asked but unsupported: no scrolling at all.
        assert_eq!(touchpad_scroll_method(TWO, TWO, false, true, true), NONE);
    }

    #[test]
    fn scroll_buttons_map_like_meta_clutter_button_to_evdev() {
        assert_eq!(scroll_button_evcode(1), 0x110); // BTN_LEFT
        assert_eq!(scroll_button_evcode(2), 0x112); // BTN_MIDDLE
        assert_eq!(scroll_button_evcode(3), 0x111); // BTN_RIGHT
        assert_eq!(scroll_button_evcode(8), 0x113); // BTN_SIDE
        assert_eq!(scroll_button_evcode(9), 0x114); // BTN_EXTRA
    }

    #[test]
    fn touchpad_enums_map_to_libinput_values() {
        let areas = li::ffi::libinput_config_click_method_LIBINPUT_CONFIG_CLICK_METHOD_BUTTON_AREAS;
        let finger = li::ffi::libinput_config_click_method_LIBINPUT_CONFIG_CLICK_METHOD_CLICKFINGER;
        assert_eq!(
            click_method_value(TouchpadClickMethod::Default, areas),
            areas
        );
        assert_eq!(click_method_value(TouchpadClickMethod::None, areas), 0);
        assert_eq!(
            click_method_value(TouchpadClickMethod::Areas, finger),
            areas
        );
        assert_eq!(
            click_method_value(TouchpadClickMethod::Fingers, areas),
            finger
        );
        assert_eq!(
            send_events_mode(SendEvents::Enabled),
            li::SendEventsMode::ENABLED
        );
        assert_eq!(
            send_events_mode(SendEvents::Disabled),
            li::SendEventsMode::DISABLED
        );
        assert_eq!(
            send_events_mode(SendEvents::DisabledOnExternalMouse),
            li::SendEventsMode::DISABLED_ON_EXTERNAL_MOUSE
        );
        let lmr = Some(li::TapButtonMap::LeftMiddleRight);
        let lrm = Some(li::TapButtonMap::LeftRightMiddle);
        assert_eq!(tap_button_map(TapButtonMap::Default, lmr), lmr);
        assert_eq!(tap_button_map(TapButtonMap::Lrm, lmr), lrm);
        assert_eq!(tap_button_map(TapButtonMap::Lmr, lrm), lmr);
    }

    #[test]
    fn supported_profiles_and_unsupported_fallback_use_device_default() {
        use li::AccelProfile::{Adaptive, Flat};
        let available = [Flat, Adaptive];
        assert_eq!(
            resolve_accel_profile(PointerAccelProfile::Default, &available, Some(Adaptive)),
            Some(Adaptive)
        );
        assert_eq!(
            resolve_accel_profile(PointerAccelProfile::Flat, &available, Some(Adaptive)),
            Some(Flat)
        );
        assert_eq!(
            resolve_accel_profile(PointerAccelProfile::Adaptive, &available, Some(Flat)),
            Some(Adaptive)
        );
        assert_eq!(
            resolve_accel_profile(PointerAccelProfile::Flat, &[Adaptive], Some(Adaptive)),
            Some(Adaptive)
        );
        assert_eq!(
            resolve_accel_profile(PointerAccelProfile::Custom, &available, Some(Adaptive)),
            Some(Adaptive)
        );
        assert_eq!(
            resolve_accel_profile(PointerAccelProfile::Flat, &[], None),
            None
        );
    }
}
