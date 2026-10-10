//! Output rotation and sensor-driven orientation (#345, P-DS-04).
//!
//! GNOME 51's Displays panel rotates outputs through
//! `org.gnome.Mutter.DisplayConfig` (transforms 0-7), persists the
//! arrangement in `monitors.xml`, and — on convertibles with an
//! accelerometer — rotates the built-in panel from iio-sensor-proxy
//! unless orientation lock is set. This module is the shared vocabulary:
//!
//! - [`SensorOrientation`]: iio-sensor-proxy's `AccelerometerOrientation`
//!   strings, with Mutter's exact mapping to output transforms.
//! - Transform helpers: the wire values 0-7 shared by Mutter, the
//!   Wayland protocol and Smithay.
//! - [`OrientationPolicy`]: orientation lock plus the sensor sample,
//!   deciding the effective transform per output.
//! - [`SensorPoller`]: a blocking iio-sensor-proxy client for the
//!   Mutter D-Bus thread, with graceful absent-service handling and
//!   hotplug retry.
//!
//! Baseline: mutter 51.0 `src/backends/meta-orientation-manager.c`
//! (sensor strings, claim discipline, the settings-daemon
//! `orientation-lock` key) and `src/backends/meta-monitor-config-store.c`
//! (monitors.xml rotation names). Sensor mapping: `normal`→0,
//! `bottom-up`→180, `left-up`→90, `right-up`→270.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// GSettings schema holding the orientation-lock key (mutter 51.0
/// `CONF_SCHEMA`; the shell reads it, the compositor receives the
/// value through [`tuna_shell_control::InputSettings::orientation_lock`]).
pub const ORIENTATION_LOCK_SCHEMA: &str = "org.gnome.settings-daemon.peripherals.touchscreen";
/// GSettings key for orientation lock (mutter 51.0 `ORIENTATION_LOCK_KEY`).
pub const ORIENTATION_LOCK_KEY: &str = "orientation-lock";

/// iio-sensor-proxy on the system bus (mutter 51.0 watches this name).
pub const SENSOR_BUS_NAME: &str = "net.hadess.SensorProxy";
/// iio-sensor-proxy object path.
pub const SENSOR_OBJECT_PATH: &str = "/net/hadess/SensorProxy";
/// iio-sensor-proxy interface.
pub const SENSOR_INTERFACE: &str = "net.hadess.SensorProxy";

/// How long an absent sensor service stays quiet before the poller
/// tries the system bus again (hotplug without a reconnect storm).
const SENSOR_RETRY: Duration = Duration::from_secs(5);

/// Mutter's built-in panel test (its DisplayConfig "Built-in display"):
/// the sensor drives these connectors only.
pub fn is_builtin_panel(connector: &str) -> bool {
    ["eDP-", "LVDS-", "DSI-"]
        .iter()
        .any(|p| connector.starts_with(p))
}

/// A Mutter DisplayConfig transform (0-7) the backend can apply.
pub const fn transform_is_supported(transform: u32) -> bool {
    transform <= 7
}

/// Transforms 1, 3, 5 and 7 exchange width and height.
pub const fn transform_swaps_dimensions(transform: u32) -> bool {
    matches!(transform, 1 | 3 | 5 | 7)
}

/// Mutter wire value to Smithay transform (unknown values stay normal).
pub fn smithay_transform(transform: u32) -> smithay::utils::Transform {
    use smithay::utils::Transform as T;
    match transform {
        1 => T::_90,
        2 => T::_180,
        3 => T::_270,
        4 => T::Flipped,
        5 => T::Flipped90,
        6 => T::Flipped180,
        7 => T::Flipped270,
        _ => T::Normal,
    }
}

/// Smithay transform back to the Mutter wire value.
pub fn transform_from_smithay(transform: smithay::utils::Transform) -> u32 {
    use smithay::utils::Transform as T;
    match transform {
        T::Normal => 0,
        T::_90 => 1,
        T::_180 => 2,
        T::_270 => 3,
        T::Flipped => 4,
        T::Flipped90 => 5,
        T::Flipped180 => 6,
        T::Flipped270 => 7,
    }
}

/// Logical size of a physical area at `scale` under `transform`
/// (rounded, never zero; 90°/270° exchange width and height).
pub fn logical_extent(width: i32, height: i32, scale: f64, transform: u32) -> (i32, i32) {
    let (w, h) = crate::runtime::logical_size(width, height, scale);
    if transform_swaps_dimensions(transform) {
        (h, w)
    } else {
        (w, h)
    }
}

/// Map a point in an unrotated `width`×`height` frame to the frame the
/// transform produces (touch/input coordinate hook; windows.rs calls
/// this once rotation-aware touch mapping lands there).
pub fn transform_point(width: i32, height: i32, transform: u32, x: i32, y: i32) -> (i32, i32) {
    let (w, h) = (width.max(1), height.max(1));
    // Flipped variants compose a vertical flip with the rotation.
    let (x, y) = if matches!(transform, 4..=7) {
        (x, h - 1 - y)
    } else {
        (x, y)
    };
    match transform {
        1 | 5 => (h - 1 - y, x),
        2 | 6 => (w - 1 - x, h - 1 - y),
        3 | 7 => (y, w - 1 - x),
        _ => (x.clamp(0, w - 1), y.clamp(0, h - 1)),
    }
}

/// iio-sensor-proxy accelerometer orientation (mutter 51.0
/// `orientation_from_string`; `undefined` and unknown strings parse to
/// `None`, never to a rotation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SensorOrientation {
    Normal,
    BottomUp,
    LeftUp,
    RightUp,
}

impl SensorOrientation {
    /// Parse an `AccelerometerOrientation` property value.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "normal" => Some(Self::Normal),
            "bottom-up" => Some(Self::BottomUp),
            "left-up" => Some(Self::LeftUp),
            "right-up" => Some(Self::RightUp),
            _ => None,
        }
    }

    /// Mutter 51.0 `meta_orientation_to_transform`.
    pub const fn transform(self) -> u32 {
        match self {
            Self::Normal => 0,
            Self::BottomUp => 2,
            Self::LeftUp => 1,
            Self::RightUp => 3,
        }
    }
}

/// One accelerometer reading for the event loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SensorSample {
    /// The service answered and reports an accelerometer.
    pub present: bool,
    /// Current orientation, when the service reports a known one.
    pub orientation: Option<SensorOrientation>,
}

impl SensorSample {
    pub const ABSENT: Self = Self {
        present: false,
        orientation: None,
    };
}

/// Orientation lock plus the latest sensor sample: the effective
/// transform per output. Mirrors mutter: the sensor drives built-in
/// panels only, and only while unlocked.
#[derive(Debug, Clone, Default)]
pub struct OrientationPolicy {
    /// `orientation-lock` from the shell's input settings.
    pub lock: bool,
    /// The sensor service reports an accelerometer.
    pub sensor_present: bool,
    /// Latest known sensor orientation.
    pub sensor: Option<SensorOrientation>,
}

impl OrientationPolicy {
    /// Whether the sensor currently drives a panel: present, unlocked,
    /// and at least one built-in output exists. Feeds Mutter's
    /// `PanelOrientationManaged`.
    pub fn managed(&self, has_builtin_output: bool) -> bool {
        self.sensor_present && !self.lock && has_builtin_output
    }

    /// The transform `connector` should show: the sensor's for
    /// unlocked built-in panels, else the configured `manual` one. An
    /// unknown sensor reading keeps the manual transform.
    pub fn effective(&self, connector: &str, manual: u32) -> u32 {
        if !self.lock && self.sensor_present && is_builtin_panel(connector) {
            self.sensor
                .map(SensorOrientation::transform)
                .unwrap_or(manual)
        } else {
            manual
        }
    }
}

/// Flags shared between the event loop and the Mutter D-Bus thread.
#[derive(Debug, Default)]
pub struct OrientationShare {
    /// The event loop sets this from [`OrientationPolicy::managed`];
    /// DisplayConfig's `PanelOrientationManaged` reads it.
    pub managed: AtomicBool,
    /// The event loop sets this from `orientation-lock`; the sensor
    /// poller releases its claim while set (mutter inhibits tracking
    /// on lock).
    pub inhibited: AtomicBool,
}

impl OrientationShare {
    pub fn is_managed(&self) -> bool {
        self.managed.load(Ordering::SeqCst)
    }

    pub fn set_managed(&self, managed: bool) {
        self.managed.store(managed, Ordering::SeqCst);
    }

    pub fn is_inhibited(&self) -> bool {
        self.inhibited.load(Ordering::SeqCst)
    }

    pub fn set_inhibited(&self, inhibited: bool) {
        self.inhibited.store(inhibited, Ordering::SeqCst);
    }
}

/// Blocking iio-sensor-proxy client, polled from the Mutter D-Bus
/// thread's scan loop. Connects lazily, claims the accelerometer while
/// tracking (mutter's claim discipline), releases on lock, and retries
/// an absent service every few seconds so hotplug just works. Every
/// failure degrades to [`SensorSample::ABSENT`], never to a rotation.
pub struct SensorPoller {
    connection: Option<zbus::blocking::Connection>,
    claimed: bool,
    next_retry: Instant,
    last: Option<SensorSample>,
}

impl SensorPoller {
    pub fn new() -> Self {
        Self {
            connection: None,
            claimed: false,
            next_retry: Instant::now(),
            last: None,
        }
    }

    fn proxy(connection: &zbus::blocking::Connection) -> zbus::Result<zbus::blocking::Proxy<'_>> {
        zbus::blocking::Proxy::new(
            connection,
            SENSOR_BUS_NAME,
            SENSOR_OBJECT_PATH,
            SENSOR_INTERFACE,
        )
    }

    fn release(&mut self) {
        if !self.claimed {
            return;
        }
        self.claimed = false;
        if let Some(connection) = &self.connection {
            if let Ok(proxy) = Self::proxy(connection) {
                let _ = proxy.call_method("ReleaseAccelerometer", &());
            }
        }
    }

    fn drop_connection(&mut self, now: Instant) {
        self.claimed = false;
        self.connection = None;
        self.next_retry = now + SENSOR_RETRY;
    }

    /// Poll the sensor once. Returns the sample only when it changed
    /// since the previous poll, so the caller sends on change.
    pub fn poll(&mut self, inhibited: bool) -> Option<SensorSample> {
        let now = Instant::now();
        if self.connection.is_none() && now >= self.next_retry {
            match zbus::blocking::Connection::system() {
                Ok(connection) => self.connection = Some(connection),
                Err(e) => {
                    eprintln!("tuna-compositor: orientation: no system bus: {e}");
                    self.next_retry = now + SENSOR_RETRY;
                }
            }
        }
        let sample = self.read(inhibited, now);
        if Some(sample) != self.last {
            self.last = Some(sample);
            Some(sample)
        } else {
            None
        }
    }

    /// Run one sensor call against a short-lived proxy (each borrow
    /// ends before the next, so failures can drop the connection).
    fn with_proxy<T>(
        &self,
        call: impl FnOnce(&zbus::blocking::Proxy<'_>) -> zbus::Result<T>,
    ) -> Option<T> {
        let connection = self.connection.as_ref()?;
        let proxy = Self::proxy(connection).ok()?;
        call(&proxy).ok()
    }

    fn read(&mut self, inhibited: bool, now: Instant) -> SensorSample {
        if self.connection.is_none() {
            return SensorSample::ABSENT;
        }
        let present: Option<bool> = self.with_proxy(|proxy| proxy.get_property("HasAccelerometer"));
        let Some(present) = present else {
            // The service is absent or vanished (hotplug): retry later.
            self.drop_connection(now);
            return SensorSample::ABSENT;
        };
        if inhibited {
            self.release();
        } else if present && !self.claimed {
            match self.with_proxy(|proxy| proxy.call_method("ClaimAccelerometer", &())) {
                Some(_) => self.claimed = true,
                None => eprintln!("tuna-compositor: orientation: claim failed"),
            }
        }
        let orientation = if present {
            self.with_proxy(|proxy| proxy.get_property::<String>("AccelerometerOrientation"))
                .and_then(|value| SensorOrientation::parse(&value))
        } else {
            None
        };
        SensorSample {
            present,
            orientation,
        }
    }
}

impl Default for SensorPoller {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for SensorPoller {
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensor_strings_follow_mutter_51() {
        assert_eq!(
            SensorOrientation::parse("normal"),
            Some(SensorOrientation::Normal)
        );
        assert_eq!(
            SensorOrientation::parse("bottom-up"),
            Some(SensorOrientation::BottomUp)
        );
        assert_eq!(
            SensorOrientation::parse("left-up"),
            Some(SensorOrientation::LeftUp)
        );
        assert_eq!(
            SensorOrientation::parse("right-up"),
            Some(SensorOrientation::RightUp)
        );
        assert_eq!(SensorOrientation::parse("undefined"), None);
        assert_eq!(SensorOrientation::parse("sideways"), None);
        assert_eq!(SensorOrientation::parse(""), None);
        // mutter 51.0 meta_orientation_to_transform.
        assert_eq!(SensorOrientation::Normal.transform(), 0);
        assert_eq!(SensorOrientation::BottomUp.transform(), 2);
        assert_eq!(SensorOrientation::LeftUp.transform(), 1);
        assert_eq!(SensorOrientation::RightUp.transform(), 3);
    }

    #[test]
    fn transforms_round_trip_through_smithay() {
        for wire in 0..=7u32 {
            assert_eq!(transform_from_smithay(smithay_transform(wire)), wire);
        }
        assert_eq!(smithay_transform(9), smithay::utils::Transform::Normal);
        assert!(transform_is_supported(7));
        assert!(!transform_is_supported(8));
        assert!(transform_swaps_dimensions(1));
        assert!(!transform_swaps_dimensions(2));
        assert!(transform_swaps_dimensions(3));
        assert!(!transform_swaps_dimensions(4));
        assert!(transform_swaps_dimensions(5));
        assert!(!transform_swaps_dimensions(6));
        assert!(transform_swaps_dimensions(7));
        assert_eq!(logical_extent(1920, 1080, 1.0, 0), (1920, 1080));
        assert_eq!(logical_extent(1920, 1080, 1.0, 1), (1080, 1920));
        assert_eq!(logical_extent(1920, 1080, 2.0, 3), (540, 960));
    }

    #[test]
    fn transformed_points_stay_in_frame() {
        // Corners map onto corners of the (possibly swapped) frame.
        assert_eq!(transform_point(4, 2, 0, 1, 1), (1, 1));
        assert_eq!(transform_point(4, 2, 1, 0, 0), (1, 0));
        assert_eq!(transform_point(4, 2, 2, 0, 0), (3, 1));
        assert_eq!(transform_point(4, 2, 3, 0, 0), (0, 3));
        // 180° and the vertical flip are involutions.
        for t in [2u32, 4] {
            for x in 0..4 {
                for y in 0..2 {
                    let (x2, y2) = transform_point(4, 2, t, x, y);
                    assert_eq!(transform_point(4, 2, t, x2, y2), (x, y));
                }
            }
        }
        // Every mapped point lands inside the produced frame.
        for t in 0..=7u32 {
            let (w, h) = if transform_swaps_dimensions(t) {
                (2, 4)
            } else {
                (4, 2)
            };
            for x in 0..4 {
                for y in 0..2 {
                    let (mx, my) = transform_point(4, 2, t, x, y);
                    assert!(mx >= 0 && mx < w && my >= 0 && my < h, "t={t}");
                }
            }
        }
    }

    #[test]
    fn policy_drives_unlocked_builtin_panels_only() {
        let policy = OrientationPolicy {
            lock: false,
            sensor_present: true,
            sensor: Some(SensorOrientation::LeftUp),
        };
        assert!(policy.managed(true));
        assert!(!policy.managed(false));
        assert_eq!(policy.effective("eDP-1", 0), 1);
        assert_eq!(policy.effective("HDMI-A-1", 0), 0);
        let locked = OrientationPolicy {
            lock: true,
            ..policy.clone()
        };
        assert!(!locked.managed(true));
        assert_eq!(locked.effective("eDP-1", 2), 2);
        let absent = OrientationPolicy {
            sensor_present: false,
            sensor: None,
            ..policy.clone()
        };
        assert!(!absent.managed(true));
        assert_eq!(absent.effective("eDP-1", 2), 2);
        // An unknown sensor reading keeps the configured transform.
        let unknown = OrientationPolicy {
            sensor: None,
            ..policy.clone()
        };
        assert_eq!(unknown.effective("eDP-1", 2), 2);
        assert!(is_builtin_panel("eDP-1"));
        assert!(is_builtin_panel("LVDS-1"));
        assert!(is_builtin_panel("DSI-1"));
        assert!(!is_builtin_panel("HDMI-A-1"));
        assert!(!is_builtin_panel("tuna-0"));
    }

    #[test]
    fn sensor_poll_never_panics_without_a_bus() {
        // No assertion about the sample itself: hardware may or may not
        // be present, and a rotating device could legitimately change
        // the sample between two polls. Polling, releasing and dropping
        // must simply never panic.
        let mut poller = SensorPoller::new();
        let _ = poller.poll(false);
        let _ = poller.poll(true);
        poller.release();
    }
}
