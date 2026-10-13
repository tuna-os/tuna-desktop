//! Touchscreens, touch output mapping and sensor-driven orientation (#345).
//!
//! The seat advertises touch (see [`WindowManager::new`](crate::windows::WindowManager::new)),
//! and touch contacts reach clients as `wl_touch` events while the existing
//! pointer emulation keeps non-touch-aware clients working. GNOME's
//! touchscreen settings (`org.gnome.desktop.peripherals.touchscreen`,
//! flattened onto [`InputSettings`](tuna_shell_control::InputSettings) by
//! the shell) drive output mapping and the orientation lock.
//!
//! Output mapping is compositor-side, like Mutter's: libinput reports the
//! contact in device coordinates and the hardware backend places it onto
//! the selected output's logical rectangle (see [`select_output`] and
//! [`map_onto`]). No libinput option configures this, so
//! `apply_libinput` stays untouched.
//!
//! Orientation follows the accelerometer through iio-sensor-proxy (see
//! [`sensor`]): [`OrientationState`] tracks the sensor reading, freezes it
//! while `orientation-lock` is set, and publishes the applied transform to
//! `org.gnome.Mutter.DisplayConfig`. Three behaviors stay open. Physical
//! output rotation (KMS plane / GL framebuffer transform) is not rendered
//! yet, so the reported transform describes the managed orientation before
//! the panel visibly follows; touch keeps mapping onto the unrotated rect
//! until rotation rendering lands with it. Explicit output triples match
//! connector names only: nothing here carries EDID make/model/serial yet,
//! so an unmatched triple falls back to the primary rather than stranding
//! the touchscreen off-screen. And a voided contact lifts instead of
//! cancelling on the wire: Smithay 0.7 only cancels unframed pending
//! downs, so cancelling a framed contact would strand the client's touch
//! point.

use smithay::utils::{Logical, Point, Rectangle};

use tuna_shell_control::InputSettings;

/// Touchscreen settings resolved for one input round: the hardware
/// backend keeps one and rebuilds it on every `SetInputSettings`.
#[derive(Debug, Clone)]
pub struct TouchscreenConfig {
    /// EDID `[vendor, product, serial]`; all empty is automatic.
    pub output: [String; 3],
    /// `orientation-lock`: freeze the applied orientation.
    pub orientation_lock: bool,
}

impl Default for TouchscreenConfig {
    /// GNOME 51's touchscreen defaults: automatic output mapping and
    /// sensor-driven orientation (unlocked).
    fn default() -> Self {
        Self::new(&InputSettings::default())
    }
}

impl TouchscreenConfig {
    pub fn new(settings: &InputSettings) -> Self {
        Self {
            output: settings.touchscreen_output.clone(),
            orientation_lock: settings.touchscreen_orientation_lock,
        }
    }

    /// Whether no explicit output is configured (automatic mapping).
    pub fn automatic_output(&self) -> bool {
        self.output.iter().all(|part| part.is_empty())
    }
}

/// Pick the output rectangle a touchscreen maps onto. Automatic mapping
/// (an all-empty triple, GNOME's default) takes the primary output, else
/// the first. An explicit triple takes the output whose connector name
/// matches any element; an unmatched triple falls back to the primary
/// rather than stranding the touchscreen off-screen. `None` only when
/// no outputs exist at all.
pub fn select_output(
    outputs: &[(String, Rectangle<i32, Logical>, bool)],
    config: &TouchscreenConfig,
) -> Option<Rectangle<i32, Logical>> {
    if outputs.is_empty() {
        return None;
    }
    let primary = outputs
        .iter()
        .find(|(_, _, primary)| *primary)
        .or_else(|| outputs.first())
        .map(|(_, rect, _)| *rect);
    if config.automatic_output() {
        return primary;
    }
    outputs
        .iter()
        .find(|(name, _, _)| config.output.iter().any(|part| part == name))
        .map(|(_, rect, _)| *rect)
        .or(primary)
}

/// Place a contact reported in its target's coordinate space onto the
/// mapped output: offset by the output's logical origin and clamp to its
/// rectangle, so calibration overshoot lands on the edge instead of a
/// neighboring output. A degenerate (empty) target maps to its origin.
pub fn map_onto(target: Rectangle<i32, Logical>, x: f64, y: f64) -> Point<f64, Logical> {
    if target.size.w <= 0 || target.size.h <= 0 {
        return (f64::from(target.loc.x), f64::from(target.loc.y)).into();
    }
    let (left, top) = (f64::from(target.loc.x), f64::from(target.loc.y));
    let (right, bottom) = (
        left + f64::from(target.size.w),
        top + f64::from(target.size.h),
    );
    ((left + x).clamp(left, right), (top + y).clamp(top, bottom)).into()
}

/// Accelerometer orientation, as iio-sensor-proxy reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Orientation {
    /// Upright (the sensor's `normal`, and the state before any reading).
    #[default]
    Normal,
    /// Upside down (`bottom-up`).
    BottomUp,
    /// Rotated 90 degrees counter-clockwise from normal (`right-up`):
    /// the device's right edge points up.
    RightUp,
    /// Rotated 90 degrees clockwise from normal (`left-up`): the
    /// device's left edge points up.
    LeftUp,
}

impl Orientation {
    /// Parse one `AccelerometerOrientation` value. `undefined` and
    /// unknown values are not orientations: the caller keeps the last
    /// reading, as Mutter does.
    pub fn parse_sensor(value: &str) -> Option<Self> {
        match value {
            "normal" => Some(Self::Normal),
            "bottom-up" => Some(Self::BottomUp),
            "right-up" => Some(Self::RightUp),
            "left-up" => Some(Self::LeftUp),
            _ => None,
        }
    }

    /// The `wl_output.transform` value (`DisplayConfig` reports this).
    /// A device rotated 90 degrees clockwise (`left-up`) needs its
    /// content rotated 90 degrees counter-clockwise to stay upright,
    /// which is transform 1; mirrored for `right-up`. Matches Mutter's
    /// orientation manager.
    pub fn transform(self) -> u32 {
        match self {
            Self::Normal => 0,
            Self::LeftUp => 1,
            Self::BottomUp => 2,
            Self::RightUp => 3,
        }
    }

    /// The orientation a `wl_output.transform` value describes, if it
    /// is one of the four accelerometer orientations (flipped and
    /// unknown transforms are not).
    pub fn from_transform(transform: u32) -> Option<Self> {
        match transform {
            0 => Some(Self::Normal),
            1 => Some(Self::LeftUp),
            2 => Some(Self::BottomUp),
            3 => Some(Self::RightUp),
            _ => None,
        }
    }
}

/// Sensor-driven orientation with GNOME's orientation lock: the sensor
/// reading flows to the applied orientation while unlocked; locking
/// freezes the applied orientation, and unlocking applies the latest
/// sensor reading at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrientationState {
    sensor: Orientation,
    applied: Orientation,
    locked: bool,
}

impl Default for OrientationState {
    fn default() -> Self {
        Self {
            sensor: Orientation::Normal,
            applied: Orientation::Normal,
            locked: false,
        }
    }
}

impl OrientationState {
    /// The latest sensor reading.
    pub fn sensor(self) -> Orientation {
        self.sensor
    }

    /// The orientation in effect (frozen while locked).
    pub fn applied(self) -> Orientation {
        self.applied
    }

    /// Whether `orientation-lock` is set.
    pub fn locked(self) -> bool {
        self.locked
    }

    /// Record one sensor reading. It applies immediately unless the
    /// orientation is locked.
    pub fn on_sensor(&mut self, orientation: Orientation) {
        self.sensor = orientation;
        if !self.locked {
            self.applied = orientation;
        }
    }

    /// Apply `orientation-lock`. Locking freezes the applied
    /// orientation; unlocking applies the latest sensor reading.
    pub fn set_locked(&mut self, locked: bool) {
        self.locked = locked;
        if !locked {
            self.applied = self.sensor;
        }
    }
}

/// The accelerometer through iio-sensor-proxy (`net.hadess.SensorProxy`).
///
/// Read-only property polling over the session bus: absent bus, absent
/// proxy, or no accelerometer all read as no sensor (`None`), so
/// desktops without one (and macOS builds) simply stay at `Normal`.
/// The claim is held for the D-Bus thread's lifetime; the proxy only
/// updates its orientation while claimed.
pub mod sensor {
    use super::Orientation;

    const BUS_NAME: &str = "net.hadess.SensorProxy";
    const PATH: &str = "/net/hadess/SensorProxy";
    const INTERFACE: &str = "net.hadess.SensorProxy";

    fn proxy<'a>(conn: &'a zbus::blocking::Connection) -> zbus::Result<zbus::blocking::Proxy<'a>> {
        zbus::blocking::Proxy::new(conn, BUS_NAME, PATH, INTERFACE)
    }

    /// Claim the accelerometer so the proxy updates its orientation.
    /// Retried by the caller until it holds; `false` leaves the last
    /// orientation in place.
    pub fn claim(conn: &zbus::blocking::Connection) -> bool {
        let Ok(proxy) = proxy(conn) else {
            return false;
        };
        proxy.call_method("ClaimAccelerometer", &()).is_ok()
    }

    /// The current accelerometer orientation, if the proxy serves one.
    pub fn read(conn: &zbus::blocking::Connection) -> Option<Orientation> {
        let proxy = proxy(conn).ok()?;
        let has: bool = proxy.get_property("HasAccelerometer").ok()?;
        if !has {
            return None;
        }
        let value: String = proxy.get_property("AccelerometerOrientation").ok()?;
        Orientation::parse_sensor(&value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Logical> {
        Rectangle::new((x, y).into(), (w, h).into())
    }

    fn outputs() -> Vec<(String, Rectangle<i32, Logical>, bool)> {
        vec![
            ("eDP-1".to_owned(), rect(0, 0, 1512, 944), true),
            ("HDMI-A-1".to_owned(), rect(1512, 0, 1920, 1080), false),
        ]
    }

    fn config(output: [&str; 3]) -> TouchscreenConfig {
        TouchscreenConfig {
            output: output.map(str::to_owned),
            orientation_lock: false,
        }
    }

    #[test]
    fn automatic_mapping_takes_the_primary_output() {
        let target = select_output(&outputs(), &config(["", "", ""])).unwrap();
        assert_eq!((target.loc.x, target.size.w), (0, 1512));
        // No primary flagged: the first output wins.
        let unflagged: Vec<_> = outputs()
            .into_iter()
            .map(|(name, rect, _)| (name, rect, false))
            .collect();
        let target = select_output(&unflagged, &TouchscreenConfig::default()).unwrap();
        assert_eq!(target.loc.x, 0);
        assert!(select_output(&[], &TouchscreenConfig::default()).is_none());
    }

    #[test]
    fn explicit_triples_match_connectors_and_fall_back_to_primary() {
        let target = select_output(&outputs(), &config(["HDMI-A-1", "", ""])).unwrap();
        assert_eq!((target.loc.x, target.size.w), (1512, 1920));
        // Any element matches.
        let target = select_output(&outputs(), &config(["", "eDP-1", ""])).unwrap();
        assert_eq!(target.loc.x, 0);
        // Unmatched: the primary, never off-screen.
        let target = select_output(&outputs(), &config(["DP-9", "0x1234", "99"])).unwrap();
        assert_eq!((target.loc.x, target.size.w), (0, 1512));
    }

    #[test]
    fn contacts_land_on_their_output_and_clamp_to_its_edges() {
        let target = rect(1512, 0, 1920, 1080);
        let at: Point<f64, Logical> = map_onto(target, 960.0, 540.0);
        assert_eq!((at.x, at.y), (2472.0, 540.0));
        let clamped: Point<f64, Logical> = map_onto(target, -40.0, 9000.0);
        assert_eq!((clamped.x, clamped.y), (1512.0, 1080.0));
        let empty: Point<f64, Logical> = map_onto(rect(8, 16, 0, 0), 5.0, 5.0);
        assert_eq!((empty.x, empty.y), (8.0, 16.0));
    }

    #[test]
    fn sensor_orientations_parse_and_round_trip_through_transforms() {
        for (name, orientation, transform) in [
            ("normal", Orientation::Normal, 0),
            ("left-up", Orientation::LeftUp, 1),
            ("bottom-up", Orientation::BottomUp, 2),
            ("right-up", Orientation::RightUp, 3),
        ] {
            assert_eq!(Orientation::parse_sensor(name), Some(orientation));
            assert_eq!(orientation.transform(), transform);
            assert_eq!(Orientation::from_transform(transform), Some(orientation));
        }
        // Undefined and unknown readings keep the last orientation.
        assert_eq!(Orientation::parse_sensor("undefined"), None);
        assert_eq!(Orientation::parse_sensor("face-up"), None);
        assert_eq!(Orientation::parse_sensor(""), None);
        // Flipped transforms are not accelerometer orientations.
        for transform in [4, 5, 6, 7, 8, u32::MAX] {
            assert_eq!(Orientation::from_transform(transform), None);
        }
    }

    #[test]
    fn orientation_lock_freezes_and_unlocking_reapplies_the_sensor() {
        let mut state = OrientationState::default();
        assert_eq!(
            (state.sensor(), state.applied()),
            (Orientation::Normal, Orientation::Normal)
        );
        state.on_sensor(Orientation::LeftUp);
        assert_eq!(state.applied(), Orientation::LeftUp);
        // Locking freezes the applied orientation.
        state.set_locked(true);
        state.on_sensor(Orientation::BottomUp);
        assert_eq!(state.sensor(), Orientation::BottomUp);
        assert_eq!(state.applied(), Orientation::LeftUp);
        // Unlocking applies the latest sensor reading at once.
        state.set_locked(false);
        assert_eq!(state.applied(), Orientation::BottomUp);
        // Locking before any reading holds normal.
        let mut fresh = OrientationState::default();
        fresh.set_locked(true);
        fresh.on_sensor(Orientation::RightUp);
        assert_eq!(fresh.applied(), Orientation::Normal);
    }

    #[test]
    fn touchscreen_config_follows_the_shell_settings() {
        let settings = InputSettings {
            touchscreen_output: ["GSM".to_owned(), "5b09".to_owned(), "31337".to_owned()],
            touchscreen_orientation_lock: true,
            ..InputSettings::default()
        };
        let config = TouchscreenConfig::new(&settings);
        assert!(!config.automatic_output());
        assert!(config.orientation_lock);
        assert!(TouchscreenConfig::default().automatic_output());
    }
}
