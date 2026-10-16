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

/// The GL frame transform for an output transform: the damage tracker
/// holds the output (surface-rotation) transform and Smithay inverts
/// it for rendering (`OutputDamageTracker::render_output` in the
/// vendored `third-party/smithay/src/backend/renderer/damage/mod.rs`
/// renders with the inverted transform), so the manual repaint path
/// must render inverted too. Self-inverse for 0, 2, 4 and 6.
pub fn render_transform(transform: u32) -> smithay::utils::Transform {
    smithay_transform(transform).invert()
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
/// transform produces, in pixel indices. This is Smithay
/// `Transform::transform_point_in` (vendored
/// `third-party/smithay/src/utils/geometry.rs`) adjusted from its
/// logical-edge convention to pixel centers: each `area - point` edge
/// becomes `size - 1 - point`. Directions built on it: [`touch_point`]
/// maps panel pixels back to logical, and the scanout/capture remaps
/// use Smithay's continuous rect mapping for the same direction
/// (`native_repaint` and `mutter`).
///
/// Inputs clamp to the frame first, so `i32` extremes and zero or
/// negative sizes can neither panic nor wrap: every result lands
/// inside the produced frame.
pub fn transform_point(width: i32, height: i32, transform: u32, x: i32, y: i32) -> (i32, i32) {
    let (w, h) = (width.max(1), height.max(1));
    let (x, y) = (x.clamp(0, w - 1), y.clamp(0, h - 1));
    match transform {
        1 => (h - 1 - y, x),
        2 => (w - 1 - x, h - 1 - y),
        3 => (y, w - 1 - x),
        4 => (w - 1 - x, y),
        5 => (y, x),
        6 => (x, h - 1 - y),
        7 => (h - 1 - y, w - 1 - x),
        _ => (x, y),
    }
}

/// A panel-native touch/absolute-pointer pixel to the logical pixel
/// shown under it: buffer space back to logical space, which inverts
/// the transform (Smithay `Point<Buffer>::to_logical` inverts the
/// same way). The hardware input path remaps primary-output touch
/// through this; unknown transforms stay identity.
pub fn touch_point(width: i32, height: i32, transform: u32, x: i32, y: i32) -> (i32, i32) {
    transform_point(
        width,
        height,
        transform_from_smithay(smithay_transform(transform).invert()),
        x,
        y,
    )
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
        // 180° and the single-axis flips are involutions.
        for t in [2u32, 4, 6] {
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
    fn flipped_transforms_match_smithay_pixel_centers() {
        // Smithay `transform_point_in` on a 4x2 frame, adjusted to
        // pixel centers: Flipped mirrors x (not y), Flipped180
        // mirrors y (not x). Each is pinned at three points so a
        // wrong-axis regression fails loudly.
        assert_eq!(transform_point(4, 2, 4, 0, 0), (3, 0));
        assert_eq!(transform_point(4, 2, 4, 3, 1), (0, 1));
        assert_eq!(transform_point(4, 2, 4, 1, 0), (2, 0));
        assert_eq!(transform_point(4, 2, 5, 0, 0), (0, 0));
        assert_eq!(transform_point(4, 2, 5, 3, 1), (1, 3));
        assert_eq!(transform_point(4, 2, 5, 1, 0), (0, 1));
        assert_eq!(transform_point(4, 2, 6, 0, 0), (0, 1));
        assert_eq!(transform_point(4, 2, 6, 3, 1), (3, 0));
        assert_eq!(transform_point(4, 2, 6, 3, 0), (3, 1));
        assert_eq!(transform_point(4, 2, 7, 0, 0), (1, 3));
        assert_eq!(transform_point(4, 2, 7, 3, 1), (0, 0));
        assert_eq!(transform_point(4, 2, 7, 1, 0), (1, 2));
        // The plain rotations, for the same strictness.
        assert_eq!(transform_point(4, 2, 1, 3, 1), (0, 3));
        assert_eq!(transform_point(4, 2, 2, 1, 0), (2, 1));
        assert_eq!(transform_point(4, 2, 3, 3, 1), (1, 0));
        // Unknown transforms stay a clamped identity.
        assert_eq!(transform_point(4, 2, 9, 7, -3), (3, 0));
    }

    #[test]
    fn transform_point_matches_smithay_on_every_pixel() {
        // Strict correspondence with the vendored Smithay
        // `transform_point_in`: pixel centers map in doubled space
        // (`2x+1`), where Smithay's logical-edge formulas apply
        // exactly, and `(result - 1) / 2` maps back.
        for &(w, h) in &[(4, 2), (2, 4), (1, 1), (5, 3), (3, 5)] {
            for t in 0..=7u32 {
                let smithay = smithay_transform(t);
                for x in 0..w {
                    for y in 0..h {
                        let doubled = smithay::utils::Point::<i64, smithay::utils::Logical>::from(
                            (2 * i64::from(x) + 1, 2 * i64::from(y) + 1),
                        );
                        let area = smithay::utils::Size::<i64, smithay::utils::Logical>::from((
                            2 * i64::from(w),
                            2 * i64::from(h),
                        ));
                        let mapped = smithay.transform_point_in(doubled, &area);
                        let expected = (((mapped.x - 1) / 2) as i32, ((mapped.y - 1) / 2) as i32);
                        assert_eq!(
                            transform_point(w, h, t, x, y),
                            expected,
                            "t={t} ({x},{y}) on {w}x{h}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn transform_point_never_panics_or_wraps_on_extremes() {
        // `i32` extremes, zero and negative sizes, every transform
        // including unknown ones: clamping keeps every subtraction
        // in range, so the test completing is the overflow proof,
        // and every result lands inside the produced frame.
        let extremes = [i32::MIN, -1, 0, 1, 2, 4, 1080, 1920, i32::MAX];
        for &w in &extremes {
            for &h in &extremes {
                for t in 0..=9u32 {
                    let (cw, ch) = (w.max(1), h.max(1));
                    let (fw, fh) = if transform_swaps_dimensions(t) {
                        (ch, cw)
                    } else {
                        (cw, ch)
                    };
                    for &x in &extremes {
                        for &y in &extremes {
                            let (mx, my) = transform_point(w, h, t, x, y);
                            assert!(
                                mx >= 0 && mx < fw && my >= 0 && my < fh,
                                "t={t} ({x},{y}) on {w}x{h} -> ({mx},{my})"
                            );
                            let (tx, ty) = touch_point(w, h, t, x, y);
                            assert!(
                                tx >= 0 && tx < fw && ty >= 0 && ty < fh,
                                "touch t={t} ({x},{y}) on {w}x{h} -> ({tx},{ty})"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn touch_points_invert_the_panel_to_logical() {
        // Panel pixel back to the logical pixel shown there: the
        // inverse mapping, pinned per transform on a 4x2 panel.
        assert_eq!(touch_point(4, 2, 0, 3, 1), (3, 1));
        assert_eq!(touch_point(4, 2, 1, 3, 0), (0, 0));
        assert_eq!(touch_point(4, 2, 2, 3, 1), (0, 0));
        assert_eq!(touch_point(4, 2, 3, 3, 0), (1, 3));
        assert_eq!(touch_point(4, 2, 4, 3, 0), (0, 0));
        assert_eq!(touch_point(4, 2, 5, 3, 1), (0, 0));
        assert_eq!(touch_point(4, 2, 6, 3, 0), (3, 1));
        assert_eq!(touch_point(4, 2, 7, 3, 1), (1, 3));
        assert_eq!(touch_point(4, 2, 9, 7, -3), (3, 0));
        // Touch is exactly the inverted-wire forward map, everywhere.
        for t in 0..=7u32 {
            let inverted = transform_from_smithay(smithay_transform(t).invert());
            for x in 0..4 {
                for y in 0..2 {
                    assert_eq!(
                        touch_point(4, 2, t, x, y),
                        transform_point(4, 2, inverted, x, y),
                        "t={t} ({x},{y})"
                    );
                }
            }
        }
    }

    #[test]
    fn render_transforms_follow_smithay_damage_tracker() {
        // The frame renders inverted, exactly as Smithay's
        // `render_output` inverts the tracker's output transform.
        use smithay::utils::Transform as T;
        for (wire, frame) in [
            (0, T::Normal),
            (1, T::_270),
            (2, T::_180),
            (3, T::_90),
            (4, T::Flipped),
            (5, T::Flipped270),
            (6, T::Flipped180),
            (7, T::Flipped90),
        ] {
            assert_eq!(render_transform(wire), frame, "wire={wire}");
        }
        assert_eq!(render_transform(9), T::Normal);
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
