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
//! Calls newer than the pinned bindings' libinput 1.19 API are resolved
//! from the libinput the compositor runs against (custom acceleration
//! curves, 1.23; the disable-while-typing timeout, 1.31), or passed as
//! raw values (sticky tap drag lock, 1.27). Where the running libinput
//! lacks one, the device keeps its default and the device snapshot
//! reports the option unavailable.

use std::ffi::CStr;

use smithay::reexports::input as li;
use smithay::reexports::input::AsRaw;
use tuna_shell_control as shell;

/// A libinput function newer than the pinned bindings, from the libinput
/// this process runs against.
pub(super) fn libinput_symbol(name: &CStr) -> Option<*mut libc::c_void> {
    // SAFETY: dlsym with RTLD_DEFAULT and a NUL-terminated name only looks
    // the symbol up in the already loaded objects.
    let symbol = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) };
    (!symbol.is_null()).then_some(symbol)
}

const CONFIG_SUCCESS: u32 = li::ffi::libinput_config_status_LIBINPUT_CONFIG_STATUS_SUCCESS;
const ACCEL_NONE: u32 = li::ffi::libinput_config_accel_profile_LIBINPUT_CONFIG_ACCEL_PROFILE_NONE;
const ACCEL_FLAT: u32 = li::ffi::libinput_config_accel_profile_LIBINPUT_CONFIG_ACCEL_PROFILE_FLAT;
const ACCEL_ADAPTIVE: u32 =
    li::ffi::libinput_config_accel_profile_LIBINPUT_CONFIG_ACCEL_PROFILE_ADAPTIVE;
/// `LIBINPUT_CONFIG_ACCEL_PROFILE_CUSTOM` (libinput.h since 1.23).
const ACCEL_CUSTOM: u32 = 1 << 2;
/// `LIBINPUT_ACCEL_TYPE_MOTION` (libinput.h since 1.23).
const ACCEL_TYPE_MOTION: u32 = 1;
const DRAG_LOCK_DISABLED: u32 =
    li::ffi::libinput_config_drag_lock_state_LIBINPUT_CONFIG_DRAG_LOCK_DISABLED;
/// `LIBINPUT_CONFIG_DRAG_LOCK_ENABLED_TIMEOUT`, the bindings' `ENABLED`.
const DRAG_LOCK_TIMEOUT: u32 =
    li::ffi::libinput_config_drag_lock_state_LIBINPUT_CONFIG_DRAG_LOCK_ENABLED;
/// `LIBINPUT_CONFIG_DRAG_LOCK_ENABLED_STICKY` (libinput.h since 1.27);
/// older libinput rejects it as invalid.
const DRAG_LOCK_STICKY: u32 = 2;

type AccelCreate = unsafe extern "C" fn(u32) -> *mut libc::c_void;
type AccelDestroy = unsafe extern "C" fn(*mut libc::c_void);
// libinput does not write through `points` (Mutter casts away const too).
type AccelSetPoints = unsafe extern "C" fn(*mut libc::c_void, u32, f64, usize, *const f64) -> u32;
type AccelApply = unsafe extern "C" fn(*mut li::ffi::libinput_device, *mut libc::c_void) -> u32;
type DwtSetTimeout = unsafe extern "C" fn(*mut li::ffi::libinput_device, u32) -> u32;
type DwtGetTimeout = unsafe extern "C" fn(*mut li::ffi::libinput_device) -> u32;

/// Whether this libinput has the custom acceleration calls (1.23).
fn custom_accel_calls_present() -> bool {
    [
        c"libinput_config_accel_create",
        c"libinput_config_accel_set_points",
        c"libinput_device_config_accel_apply",
        c"libinput_config_accel_destroy",
    ]
    .into_iter()
    .all(|name| libinput_symbol(name).is_some())
}

/// Mutter's `set_device_accel_profile` custom branch: a motion curve
/// applied to the device. False when libinput lacks the calls or rejects
/// the curve, so the caller falls back to the device default.
fn apply_custom_curve(device: &li::Device, curve: &shell::CustomAccelCurve) -> bool {
    let (Some(create), Some(set_points), Some(apply), Some(destroy)) = (
        libinput_symbol(c"libinput_config_accel_create"),
        libinput_symbol(c"libinput_config_accel_set_points"),
        libinput_symbol(c"libinput_device_config_accel_apply"),
        libinput_symbol(c"libinput_config_accel_destroy"),
    ) else {
        return false;
    };
    // SAFETY: the symbols are libinput's own with these C signatures
    // (libinput.h since 1.23); the config is created, used and destroyed
    // here, `speeds` outlives the call and the device pointer is live.
    unsafe {
        let create: AccelCreate = std::mem::transmute(create);
        let set_points: AccelSetPoints = std::mem::transmute(set_points);
        let apply: AccelApply = std::mem::transmute(apply);
        let destroy: AccelDestroy = std::mem::transmute(destroy);
        let config = create(ACCEL_CUSTOM);
        if config.is_null() {
            return false;
        }
        let applied = set_points(
            config,
            ACCEL_TYPE_MOTION,
            curve.step,
            curve.speeds.len(),
            curve.speeds.as_ptr(),
        ) == CONFIG_SUCCESS
            && apply(device.as_raw_mut(), config) == CONFIG_SUCCESS;
        destroy(config);
        applied
    }
}

/// `libinput_device_config_dwt_set_timeout` (libinput 1.31); `None` when
/// this libinput has no such call, else whether libinput accepted it.
fn set_dwt_timeout(device: &li::Device, millis: u32) -> Option<bool> {
    let set = libinput_symbol(c"libinput_device_config_dwt_set_timeout")?;
    // SAFETY: libinput.h since 1.31; the device pointer is live.
    unsafe {
        let set: DwtSetTimeout = std::mem::transmute(set);
        Some(set(device.as_raw_mut(), millis) == CONFIG_SUCCESS)
    }
}

/// `libinput_device_config_dwt_get_timeout` (libinput 1.31).
fn dwt_timeout(device: &li::Device) -> Option<u32> {
    let get = libinput_symbol(c"libinput_device_config_dwt_get_timeout")?;
    // SAFETY: libinput.h since 1.31; the device pointer is live.
    unsafe {
        let get: DwtGetTimeout = std::mem::transmute(get);
        Some(get(device.as_raw_mut()))
    }
}

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
    pub disable_while_typing_timeout_ms: u32,
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
#[derive(Debug, Clone, PartialEq)]
pub struct LibinputPolicy {
    pub left_handed: Option<bool>,
    pub natural_scroll: Option<bool>,
    /// GNOME's -1..1.
    pub speed: Option<f64>,
    pub accel_profile: Option<shell::PointerAccelProfile>,
    /// The same class's `custom-accel-config`, after Mutter's fallback.
    pub custom_accel: Option<shell::CustomAccelCurve>,
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
    // Later entries win, in `apply_device_settings` order; each class
    // passes its own profile and curve (`update_pointer_accel_profile`).
    let curves = &settings.custom_accel;
    let accel = [
        (mouse, settings.mouse_accel_profile, &curves.mouse),
        (
            caps.touchpad,
            settings.touchpad.accel_profile,
            &curves.touchpad,
        ),
        (
            caps.trackball,
            settings.trackball.accel_profile,
            &curves.trackball,
        ),
        (
            caps.pointing_stick,
            settings.pointing_stick.accel_profile,
            &curves.pointing_stick,
        ),
    ]
    .into_iter()
    .rev()
    .find(|(applies, ..)| *applies);
    let accel_profile = accel.map(|(_, profile, _)| profile);
    let custom_accel = accel.map(|(_, _, curve)| curve.or_mutter_fallback());
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
        custom_accel,
        middle_click_emulation,
        touchpad: caps.touchpad.then_some(TouchpadPolicy {
            tap_to_click: settings.tap_to_click,
            tap_button_map: settings.touchpad.tap_button_map,
            tap_and_drag: settings.touchpad.tap_and_drag,
            tap_and_drag_lock: settings.touchpad.tap_and_drag_lock,
            disable_while_typing: settings.disable_while_typing,
            disable_while_typing_timeout_ms: settings.disable_while_typing_timeout_ms,
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

/// Mutter's `set_device_accel_profile` choice: the requested profile if
/// the device has it, else the device default. Values and the
/// `available` mask are `libinput_config_accel_profile` bits.
pub fn resolve_accel_profile(
    requested: shell::PointerAccelProfile,
    available: u32,
    default: u32,
) -> u32 {
    let desired = match requested {
        shell::PointerAccelProfile::Flat => ACCEL_FLAT,
        shell::PointerAccelProfile::Adaptive => ACCEL_ADAPTIVE,
        shell::PointerAccelProfile::Custom => ACCEL_CUSTOM,
        shell::PointerAccelProfile::Default => default,
    };
    if available & desired == 0 {
        default
    } else {
        desired
    }
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
        apply_accel_profile(device, profile, policy.custom_accel.as_ref());
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
        apply_drag_lock(device, policy.tap_and_drag_lock);
    }
    if device.config_dwt_is_available() {
        let _ = device.config_dwt_set_enabled(policy.disable_while_typing);
        // libinput without the call keeps its own timeout; the snapshot
        // reports `dwt_timeout_available: false`.
        if set_dwt_timeout(device, policy.disable_while_typing_timeout_ms) == Some(false) {
            eprintln!(
                "tuna-compositor: libinput: disable-while-typing timeout {} ms rejected for {}",
                policy.disable_while_typing_timeout_ms,
                device.name()
            );
        }
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
    // libinput rejects a button the device lacks (INVALID); Mutter ignores
    // that and the device keeps its previous scroll button.
    report(
        device.config_scroll_set_button(button),
        device,
        "scroll-wheel emulation button",
    );
    let _ = device.config_scroll_set_button_lock(if emulation.lock {
        li::ScrollButtonLockState::Enabled
    } else {
        li::ScrollButtonLockState::Disabled
    });
}

/// Mutter's `set_tap_and_drag_lock_enabled`: sticky drag lock or none.
/// A libinput older than 1.27 has no sticky mode and rejects it; that
/// device gets libinput's timeout drag lock, which the snapshot shows.
fn apply_drag_lock(device: &li::Device, enabled: bool) {
    let set = |state: u32| {
        // SAFETY: `device` owns a live libinput_device reference; libinput
        // validates the state value.
        unsafe {
            li::ffi::libinput_device_config_tap_set_drag_lock_enabled(device.as_raw_mut(), state)
        }
    };
    if !enabled {
        if set(DRAG_LOCK_DISABLED) != CONFIG_SUCCESS {
            eprintln!(
                "tuna-compositor: libinput: tap-and-drag lock rejected for {}",
                device.name()
            );
        }
    } else if set(DRAG_LOCK_STICKY) != CONFIG_SUCCESS {
        eprintln!(
            "tuna-compositor: libinput: sticky tap-and-drag lock unsupported for {}; using the timeout drag lock",
            device.name()
        );
        if set(DRAG_LOCK_TIMEOUT) != CONFIG_SUCCESS {
            eprintln!(
                "tuna-compositor: libinput: tap-and-drag lock rejected for {}",
                device.name()
            );
        }
    }
}

/// Mutter's `set_device_accel_profile`, custom curves included.
fn apply_accel_profile(
    device: &li::Device,
    requested: shell::PointerAccelProfile,
    curve: Option<&shell::CustomAccelCurve>,
) {
    let raw = device.as_raw_mut();
    // SAFETY: `device` owns a live libinput_device reference.
    let (available, default) = unsafe {
        (
            li::ffi::libinput_device_config_accel_get_profiles(raw),
            li::ffi::libinput_device_config_accel_get_default_profile(raw),
        )
    };
    let mut profile = resolve_accel_profile(requested, available, default);
    if requested != shell::PointerAccelProfile::Default && profile == default {
        let wanted = resolve_accel_profile(requested, u32::MAX, default);
        if wanted != default {
            eprintln!("tuna-compositor: libinput: requested pointer acceleration profile unsupported for {}; using device default", device.name());
        }
    }
    if profile == ACCEL_CUSTOM && !curve.is_some_and(|curve| apply_custom_curve(device, curve)) {
        eprintln!(
            "tuna-compositor: libinput: custom pointer acceleration {curve:?} rejected for {}; using device default",
            device.name()
        );
        profile = default;
    }
    if profile != ACCEL_NONE {
        // SAFETY: as above.
        let status = unsafe { li::ffi::libinput_device_config_accel_set_profile(raw, profile) };
        if status != CONFIG_SUCCESS {
            eprintln!(
                "tuna-compositor: libinput: pointer acceleration profile rejected for {}",
                device.name()
            );
        }
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

/// A `libinput_config_accel_profile` value's GNOME nick; `None` for a
/// device without acceleration.
fn accel_profile_name(profile: u32) -> Option<&'static str> {
    match profile {
        ACCEL_NONE => None,
        ACCEL_FLAT => Some("flat"),
        ACCEL_ADAPTIVE => Some("adaptive"),
        ACCEL_CUSTOM => Some("custom"),
        _ => Some("unknown"),
    }
}

fn accel_profile_names(profiles: u32) -> Vec<&'static str> {
    [ACCEL_FLAT, ACCEL_ADAPTIVE, ACCEL_CUSTOM]
        .into_iter()
        .filter(|bit| profiles & bit != 0)
        .filter_map(accel_profile_name)
        .collect()
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
    let raw = device.as_raw_mut();
    // The bindings' getters panic on libinput >= 1.27's STICKY drag lock
    // and drop 1.23's CUSTOM profile, so read the raw values. `device`
    // owns a live libinput_device reference.
    let (drag_lock, profile, profiles) = unsafe {
        (
            li::ffi::libinput_device_config_tap_get_drag_lock_enabled(raw),
            li::ffi::libinput_device_config_accel_get_profile(raw),
            li::ffi::libinput_device_config_accel_get_profiles(raw),
        )
    };
    let mode = device.config_send_events_mode();
    // Which of BTN_LEFT..BTN_TASK the device has: a scroll-wheel emulation
    // button is only accepted where it exists.
    let buttons: Vec<u32> = if caps.pointer {
        (0x110..=0x117)
            .filter(|&code| device.pointer_has_button(code) == Ok(true))
            .collect()
    } else {
        Vec::new()
    };
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
        "natural_scroll": device.config_scroll_natural_scroll_enabled(),
        "natural_available": device.config_scroll_has_natural_scroll(),
        "accel_speed": device.config_accel_speed(),
        "accel_available": device.config_accel_is_available(),
        "accel_profile": accel_profile_name(profile),
        "accel_profiles": accel_profile_names(profiles),
        // libinput lists `custom` from 1.23, alongside the calls that set it.
        "accel_custom_available": profiles & ACCEL_CUSTOM != 0 && custom_accel_calls_present(),
        "dwt_enabled": device.config_dwt_enabled(),
        "dwt_available": device.config_dwt_is_available(),
        "dwt_timeout_ms": device.config_dwt_is_available().then(|| dwt_timeout(device)).flatten(),
        "dwt_timeout_available": device.config_dwt_is_available()
            && libinput_symbol(c"libinput_device_config_dwt_set_timeout").is_some(),
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
        "pointer_buttons": buttons,
        "scroll_button_lock": device.config_scroll_button_lock() == li::ScrollButtonLockState::Enabled,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use shell::{
        CustomAccelCurve, InputSettings, PointerAccelProfile, PointingStickScrollMethod,
        ScrollWheelEmulation, SendEvents, TapButtonMap, TouchpadClickMethod,
    };

    fn curve(step: f64, speeds: &[f64]) -> CustomAccelCurve {
        CustomAccelCurve {
            step,
            speeds: speeds.to_vec(),
        }
    }

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
        settings.disable_while_typing_timeout_ms = 1500;
        settings.custom_accel.mouse = curve(0.5, &[0.0, 1.0, 2.5]);
        settings.custom_accel.touchpad = curve(2.0, &[0.0, 4.0]);
        settings.custom_accel.trackball = curve(0.25, &[0.0, 0.5, 1.0]);
        // Invalid: Mutter's fallback curve.
        settings.custom_accel.pointing_stick = curve(0.0, &[1.0]);
        settings
    }

    #[test]
    fn touchpad_policy_carries_the_touchpad_keys() {
        let policy = libinput_policy(&pointer_settings(), TOUCHPAD, true);
        assert_eq!(policy.left_handed, Some(true));
        assert_eq!(policy.natural_scroll, Some(true));
        assert_eq!(policy.speed, Some(-0.5));
        assert_eq!(policy.accel_profile, Some(PointerAccelProfile::Adaptive));
        assert_eq!(policy.custom_accel, Some(curve(2.0, &[0.0, 4.0])));
        assert_eq!(policy.middle_click_emulation, Some(true));
        assert_eq!(
            policy.touchpad,
            Some(TouchpadPolicy {
                tap_to_click: true,
                tap_button_map: TapButtonMap::Lmr,
                tap_and_drag: false,
                tap_and_drag_lock: true,
                disable_while_typing: false,
                disable_while_typing_timeout_ms: 1500,
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
        assert_eq!(policy.custom_accel, Some(curve(0.5, &[0.0, 1.0, 2.5])));
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
        assert_eq!(policy.custom_accel, Some(curve(0.25, &[0.0, 0.5, 1.0])));
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
        assert_eq!(policy.custom_accel, Some(curve(1.0, &[0.0, 1.0])));
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
                custom_accel: None,
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
        assert_eq!(
            policy.custom_accel,
            Some(curve(1.0, &[0.0, 1.0])),
            "the winning class's curve rides with its profile"
        );
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
        let (flat, adaptive, custom) = (ACCEL_FLAT, ACCEL_ADAPTIVE, ACCEL_CUSTOM);
        let all = flat | adaptive | custom;
        let resolve = resolve_accel_profile;
        assert_eq!(
            resolve(PointerAccelProfile::Default, all, adaptive),
            adaptive
        );
        assert_eq!(resolve(PointerAccelProfile::Flat, all, adaptive), flat);
        assert_eq!(resolve(PointerAccelProfile::Adaptive, all, flat), adaptive);
        assert_eq!(resolve(PointerAccelProfile::Custom, all, adaptive), custom);
        // A profile the device lacks takes the device default.
        assert_eq!(
            resolve(PointerAccelProfile::Flat, adaptive, adaptive),
            adaptive
        );
        // libinput before 1.23 lists no custom profile.
        assert_eq!(
            resolve(PointerAccelProfile::Custom, flat | adaptive, adaptive),
            adaptive
        );
        // No acceleration at all.
        assert_eq!(
            resolve(PointerAccelProfile::Flat, 0, ACCEL_NONE),
            ACCEL_NONE
        );
    }

    #[test]
    fn snapshot_names_every_profile_libinput_reports() {
        assert_eq!(accel_profile_name(ACCEL_NONE), None);
        assert_eq!(accel_profile_name(ACCEL_CUSTOM), Some("custom"));
        assert_eq!(
            accel_profile_names(ACCEL_FLAT | ACCEL_ADAPTIVE | ACCEL_CUSTOM),
            ["flat", "adaptive", "custom"]
        );
        assert_eq!(accel_profile_names(ACCEL_ADAPTIVE), ["adaptive"]);
    }
}
