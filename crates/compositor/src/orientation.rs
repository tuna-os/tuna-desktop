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
//! - [`OrientationPolicy`]: Mutter's `PanelOrientationManaged`, tracking
//!   inhibition and the orientation change handler.
//! - [`SensorPoller`]: a blocking iio-sensor-proxy client for the
//!   Mutter D-Bus thread, with graceful absent-service handling and
//!   hotplug retry.
//!
//! Baseline: mutter 51.0 `src/backends/meta-orientation-manager.c`
//! (sensor strings, claim discipline, the settings-daemon
//! `orientation-lock` key inhibiting tracking),
//! `src/backends/meta-monitor-manager.c` (`update_panel_orientation_managed`,
//! `sync_orientation_unmanaged_inhibit`, `orientation_changed`,
//! `handle_initial_orientation_change`, `handle_orientation_change`) and
//! `src/backends/meta-monitor-config-store.c` (monitors.xml rotation
//! names). Sensor mapping: `normal`→0, `bottom-up`→180, `left-up`→90,
//! `right-up`→270.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::LazyLock;
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

/// Proof hook: with the compositor state file set (a proof session),
/// this connector counts as the built-in panel, so an emulated
/// accelerometer can drive vkms's virtual connector.
pub const PROOF_BUILTIN_ENV: &str = "TUNA_PROOF_BUILTIN_CONNECTOR";

static PROOF_BUILTIN: LazyLock<Option<String>> = LazyLock::new(|| {
    std::env::var_os("TUNA_COMPOSITOR_STATE")
        .and_then(|_| std::env::var(PROOF_BUILTIN_ENV).ok())
        .filter(|connector| !connector.is_empty())
});

/// Mutter's built-in panel test (`meta_output_is_laptop`: eDP, LVDS and
/// DSI connectors; DisplayConfig's "Built-in display"): the sensor drives
/// these connectors only and touchscreens fall back to them.
pub fn is_builtin_panel(connector: &str) -> bool {
    ["eDP-", "LVDS-", "DSI-"]
        .iter()
        .any(|p| connector.starts_with(p))
        || PROOF_BUILTIN.as_deref() == Some(connector)
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
    /// Frozen while tracking is inhibited: Mutter ignores the sensor
    /// while it does not hold the claim.
    pub orientation: Option<SensorOrientation>,
    /// The accelerometer was claimed in this poll (Mutter's
    /// `::sensor-active`, after which the orientation is applied).
    pub claimed: bool,
}

impl SensorSample {
    pub const ABSENT: Self = Self {
        present: false,
        orientation: None,
        claimed: false,
    };
}

/// Mutter 51's panel-orientation state: whether the sensor manages the
/// built-in panel, whether tracking is inhibited, and what an
/// orientation change does.
#[derive(Debug, Clone, Default)]
pub struct OrientationPolicy {
    /// `orientation-lock` from the shell's input settings.
    pub lock: bool,
    /// The sensor service reports an accelerometer.
    pub sensor_present: bool,
    /// Latest known sensor orientation (`None`: undefined).
    pub sensor: Option<SensorOrientation>,
    /// The seat's touch-mode (`clutter_seat_get_touch_mode`).
    pub touch_mode: bool,
    /// The seat has a touchscreen (`clutter_seat_has_touchscreen`).
    pub has_touchscreen: bool,
    /// `initial_orient_change_done`.
    pub initial_done: bool,
}

impl OrientationPolicy {
    /// `update_panel_orientation_managed`: touch mode, an accelerometer
    /// and a built-in panel. The lock does not unmanage the panel (the
    /// Auto Rotate toggle stays to unlock it).
    pub fn managed(&self, has_builtin: bool) -> bool {
        self.touch_mode && self.sensor_present && has_builtin
    }

    /// Whether orientation tracking is inhibited: the lock
    /// (`orientation_lock_changed`), or an unmanaged panel once the
    /// initial orientation was read (`sync_orientation_unmanaged_inhibit`).
    /// The display power-save inhibition is the D-Bus thread's.
    pub fn inhibited(&self, has_builtin: bool) -> bool {
        self.lock || (!self.managed(has_builtin) && self.initial_done)
    }

    /// `orientation_changed`: the transform the built-in panel takes for
    /// the current reading, if any. `builtin_portrait` is whether the
    /// built-in panel's preferred mode is portrait, `None` without one.
    /// The first change applies to a portrait panel with a touchscreen
    /// even outside touch mode (`handle_initial_orientation_change`);
    /// afterwards only a managed panel follows. An undefined reading
    /// means the normal transform (`meta_orientation_to_transform`).
    pub fn orientation_changed(&mut self, builtin_portrait: Option<bool>) -> Option<u32> {
        let transform = self.sensor.map_or(0, SensorOrientation::transform);
        if !self.initial_done {
            self.initial_done = true;
            if self.has_touchscreen && self.sensor_present && builtin_portrait == Some(true) {
                return Some(transform);
            }
        }
        self.managed(builtin_portrait.is_some())
            .then_some(transform)
    }
}

/// Flags shared between the event loop and the Mutter D-Bus thread.
#[derive(Debug, Default)]
pub struct OrientationShare {
    /// The event loop sets this from [`OrientationPolicy::managed`];
    /// DisplayConfig's `PanelOrientationManaged` reads it.
    pub managed: AtomicBool,
    /// The event loop sets this from [`OrientationPolicy::inhibited`];
    /// the sensor poller releases its claim while set.
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
        let mut claimed = false;
        if inhibited {
            self.release();
        } else if present && !self.claimed {
            match self.with_proxy(|proxy| proxy.call_method("ClaimAccelerometer", &())) {
                Some(_) => {
                    self.claimed = true;
                    claimed = true;
                }
                None => eprintln!("tuna-compositor: orientation: claim failed"),
            }
        }
        // Mutter reads the orientation only while it holds the claim
        // (`iio_properties_changed_idle`); an unclaimed sensor keeps the
        // last reading, and a lost accelerometer is undefined.
        let orientation = if !present {
            None
        } else if self.claimed {
            self.with_proxy(|proxy| proxy.get_property::<String>("AccelerometerOrientation"))
                .and_then(|value| SensorOrientation::parse(&value))
        } else {
            self.last.and_then(|last| last.orientation)
        };
        SensorSample {
            present,
            orientation,
            claimed,
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
    fn panel_orientation_is_managed_in_touch_mode_and_survives_the_lock() {
        let mut policy = OrientationPolicy {
            sensor_present: true,
            sensor: Some(SensorOrientation::LeftUp),
            touch_mode: true,
            has_touchscreen: true,
            ..OrientationPolicy::default()
        };
        // update_panel_orientation_managed: touch mode, an accelerometer
        // and a built-in panel.
        assert!(policy.managed(true));
        assert!(!policy.managed(false));
        // The lock inhibits tracking but leaves the panel managed, so
        // GNOME Shell's Auto Rotate toggle stays to unlock it.
        policy.lock = true;
        assert!(policy.managed(true));
        assert!(policy.inhibited(true));
        policy.lock = false;
        assert!(!policy.inhibited(true));
        // A laptop out of touch mode is unmanaged; tracking stops only
        // after the initial orientation was read.
        policy.touch_mode = false;
        assert!(!policy.managed(true));
        assert!(!policy.inhibited(true));
        policy.initial_done = true;
        assert!(policy.inhibited(true));
        policy.touch_mode = true;
        policy.sensor_present = false;
        assert!(!policy.managed(true));
        assert!(is_builtin_panel("eDP-1"));
        assert!(is_builtin_panel("LVDS-1"));
        assert!(is_builtin_panel("DSI-1"));
        assert!(!is_builtin_panel("HDMI-A-1"));
        assert!(!is_builtin_panel("tuna-0"));
    }

    #[test]
    fn orientation_changes_rotate_only_a_managed_builtin_panel() {
        let tablet = OrientationPolicy {
            sensor_present: true,
            sensor: Some(SensorOrientation::RightUp),
            touch_mode: true,
            has_touchscreen: true,
            ..OrientationPolicy::default()
        };
        let mut policy = tablet.clone();
        assert_eq!(policy.orientation_changed(Some(false)), Some(3));
        assert!(policy.initial_done);
        // Undefined is the normal transform (meta_orientation_to_transform).
        policy.sensor = None;
        assert_eq!(policy.orientation_changed(Some(false)), Some(0));
        // No built-in panel: nothing to rotate.
        assert_eq!(tablet.clone().orientation_changed(None), None);
        // A docked laptop (no touch mode) ignores the sensor ...
        let mut docked = OrientationPolicy {
            touch_mode: false,
            ..tablet.clone()
        };
        assert_eq!(docked.orientation_changed(Some(false)), None);
        assert_eq!(docked.orientation_changed(Some(true)), None);
        // ... except a native-portrait panel's first reading
        // (handle_initial_orientation_change).
        let mut portrait = OrientationPolicy {
            touch_mode: false,
            ..tablet.clone()
        };
        assert_eq!(portrait.orientation_changed(Some(true)), Some(3));
        assert_eq!(portrait.orientation_changed(Some(true)), None);
        let mut no_touchscreen = OrientationPolicy {
            touch_mode: false,
            has_touchscreen: false,
            ..tablet
        };
        assert_eq!(no_touchscreen.orientation_changed(Some(true)), None);
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
