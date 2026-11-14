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

use futures_lite::StreamExt;
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

/// A wake for whoever acts on a shared flag, run when the flag changes
/// (#503): the sensor client claims and releases on these flags without
/// re-reading them on a timer. Installed once.
#[derive(Default)]
pub struct ChangeHook(std::sync::OnceLock<Box<dyn Fn() + Send + Sync>>);

impl ChangeHook {
    /// Install `wake`; a second call is ignored.
    pub fn set(&self, wake: impl Fn() + Send + Sync + 'static) {
        let _ = self.0.set(Box::new(wake));
    }

    pub fn fire(&self) {
        if let Some(wake) = self.0.get() {
            wake();
        }
    }
}

impl std::fmt::Debug for ChangeHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("ChangeHook")
            .field(&self.0.get().is_some())
            .finish()
    }
}

/// Flags shared between the event loop and the Mutter D-Bus thread.
#[derive(Debug, Default)]
pub struct OrientationShare {
    /// The event loop sets this from [`OrientationPolicy::managed`];
    /// DisplayConfig's `PanelOrientationManaged` reads it.
    pub managed: AtomicBool,
    /// The event loop sets this from `orientation-lock`; the sensor
    /// client releases its claim while set (mutter inhibits tracking
    /// on lock).
    pub inhibited: AtomicBool,
    /// Runs when `inhibited` changes.
    pub changed: ChangeHook,
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
        if self.inhibited.swap(inhibited, Ordering::SeqCst) != inhibited {
            self.changed.fire();
        }
    }
}

/// How often a present sensor is read when its signals could not be
/// subscribed, and a failed claim is retried: the former poll period.
const SENSOR_POLL: Duration = Duration::from_millis(100);

/// What the signal thread tells the client.
#[derive(Default)]
struct Signals {
    /// A signal arrived since the client last read the sensor.
    dirty: AtomicBool,
    /// The subscription still delivers (its connection is open).
    live: AtomicBool,
}

/// Connects the client to the system bus (a private bus in tests).
type Connect = Box<dyn Fn() -> zbus::Result<zbus::Connection> + Send>;

/// Blocking iio-sensor-proxy client for the Mutter D-Bus thread's scan,
/// driven by the bus as mutter's `MetaOrientationManager` is (#503): one
/// system-bus connection hears `NameOwnerChanged` for the service and the
/// sensor's `PropertiesChanged`, and the client reads the sensor only
/// after such a signal or a change of the inhibiting flags; the caller
/// polls it when `wake` runs. It claims the accelerometer while tracking
/// (mutter's claim discipline) and releases on lock. A connection whose
/// signals cannot be subscribed keeps the former polling: a present
/// sensor every 100 ms, an absent service through a new connection every
/// 5 s ([`SensorPoller::next_due`]). Every failure degrades to
/// [`SensorSample::ABSENT`], never to a rotation.
pub struct SensorPoller {
    connect: Connect,
    wake: std::sync::Arc<dyn Fn() + Send + Sync>,
    connection: Option<zbus::Connection>,
    signals: Option<std::sync::Arc<Signals>>,
    claimed: bool,
    /// A claim failed; it is retried every [`SENSOR_POLL`].
    claim_failed: bool,
    next_retry: Instant,
    next_read: Instant,
    inhibited: Option<bool>,
    last: Option<SensorSample>,
}

impl SensorPoller {
    /// A client on the system bus; `wake` runs (on a signal thread) when
    /// the next [`poll`](Self::poll) has something to read.
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self::with_bus(
            wake,
            Box::new(|| zbus::block_on(zbus::Connection::system())),
        )
    }

    fn with_bus(wake: impl Fn() + Send + Sync + 'static, connect: Connect) -> Self {
        let now = Instant::now();
        Self {
            connect,
            wake: std::sync::Arc::new(wake),
            connection: None,
            signals: None,
            claimed: false,
            claim_failed: false,
            next_retry: now,
            next_read: now,
            inhibited: None,
            last: None,
        }
    }

    /// Plain reads: a property cache would add a match rule and a
    /// `GetAll` per proxy.
    fn proxy(connection: &zbus::Connection) -> zbus::Result<zbus::Proxy<'_>> {
        zbus::block_on(
            zbus::proxy::Builder::new(connection)
                .destination(SENSOR_BUS_NAME)
                .path(SENSOR_OBJECT_PATH)
                .interface(SENSOR_INTERFACE)
                .cache_properties(zbus::proxy::CacheProperties::No)
                .build(),
        )
    }

    fn release(&mut self) {
        if !self.claimed {
            return;
        }
        self.claimed = false;
        if let Some(connection) = &self.connection {
            if let Ok(proxy) = Self::proxy(connection) {
                let _ = zbus::block_on(proxy.call_method("ReleaseAccelerometer", &()));
            }
        }
    }

    fn live(&self) -> bool {
        self.signals
            .as_ref()
            .is_some_and(|signals| signals.live.load(Ordering::SeqCst))
    }

    fn drop_connection(&mut self, now: Instant) {
        self.claimed = false;
        self.signals = None;
        if let Some(connection) = self.connection.take() {
            // Ends a signal thread still reading it.
            let _ = zbus::block_on(connection.close());
        }
        self.next_retry = now + SENSOR_RETRY;
    }

    /// When the next [`poll`](Self::poll) reads without a signal: never
    /// while the subscription delivers (unless a claim failed), else the
    /// former polling.
    pub fn next_due(&self) -> Option<Instant> {
        if self.claim_failed {
            return Some(self.next_read);
        }
        if self.live() {
            return None;
        }
        Some(if self.connection.is_none() {
            self.next_retry
        } else {
            self.next_read
        })
    }

    /// Read the sensor if a signal arrived, the flags changed or a
    /// fallback poll is due ([`next_due`](Self::next_due)). Returns the
    /// sample only when it changed since the previous read, so the
    /// caller sends on change.
    pub fn poll(&mut self, inhibited: bool) -> Option<SensorSample> {
        let now = Instant::now();
        let signalled = self
            .signals
            .as_ref()
            .is_some_and(|signals| signals.dirty.swap(false, Ordering::SeqCst));
        let due = signalled
            || self.inhibited != Some(inhibited)
            || self.next_due().is_some_and(|due| now >= due);
        if !due {
            return None;
        }
        self.inhibited = Some(inhibited);
        if self.connection.is_none() && now >= self.next_retry {
            match (self.connect)() {
                Ok(connection) => {
                    self.signals = subscribe(&connection, self.wake.clone());
                    self.connection = Some(connection);
                }
                Err(e) => {
                    eprintln!("tuna-compositor: orientation: no system bus: {e}");
                    self.next_retry = now + SENSOR_RETRY;
                }
            }
        }
        let sample = self.read(inhibited, now);
        self.next_read = now + SENSOR_POLL;
        if Some(sample) != self.last {
            self.last = Some(sample);
            Some(sample)
        } else {
            None
        }
    }

    /// Run one sensor call against a short-lived proxy (each borrow
    /// ends before the next, so failures can drop the connection).
    fn with_proxy<T>(&self, call: impl FnOnce(&zbus::Proxy<'_>) -> zbus::Result<T>) -> Option<T> {
        let connection = self.connection.as_ref()?;
        let proxy = Self::proxy(connection).ok()?;
        call(&proxy).ok()
    }

    fn read(&mut self, inhibited: bool, now: Instant) -> SensorSample {
        self.claim_failed = false;
        if self.connection.is_none() {
            return SensorSample::ABSENT;
        }
        let present: Option<bool> =
            self.with_proxy(|proxy| zbus::block_on(proxy.get_property("HasAccelerometer")));
        let Some(present) = present else {
            // The service is absent or vanished (hotplug). A live
            // subscription hears it return; otherwise retry later.
            if self.live() {
                self.claimed = false;
            } else {
                self.drop_connection(now);
            }
            return SensorSample::ABSENT;
        };
        if inhibited {
            self.release();
        } else if present && !self.claimed {
            match self
                .with_proxy(|proxy| zbus::block_on(proxy.call_method("ClaimAccelerometer", &())))
            {
                Some(_) => self.claimed = true,
                None => {
                    eprintln!("tuna-compositor: orientation: claim failed");
                    self.claim_failed = true;
                }
            }
        }
        let orientation = if present {
            self.with_proxy(|proxy| {
                zbus::block_on(proxy.get_property::<String>("AccelerometerOrientation"))
            })
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

/// Subscribe `connection` to the service's owner changes and the sensor's
/// property changes, and spawn the thread that marks each one and runs
/// `wake`. `None` when the subscription cannot be placed (the client
/// then polls as before).
fn subscribe(
    connection: &zbus::Connection,
    wake: std::sync::Arc<dyn Fn() + Send + Sync>,
) -> Option<std::sync::Arc<Signals>> {
    let rules = (|| -> zbus::Result<_> {
        Ok([
            zbus::MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .sender("org.freedesktop.DBus")
                .interface("org.freedesktop.DBus")
                .member("NameOwnerChanged")
                .arg(0, SENSOR_BUS_NAME)
                .build()?,
            zbus::MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .interface("org.freedesktop.DBus.Properties")
                .member("PropertiesChanged")
                .path(SENSOR_OBJECT_PATH)
                .build()?,
        ])
    })();
    let placed = rules.and_then(|rules| {
        // The stream exists before the rules, so no signal falls between.
        let messages = zbus::MessageStream::from(connection);
        zbus::block_on(async {
            let bus = zbus::fdo::DBusProxy::new(connection).await?;
            for rule in rules {
                bus.add_match_rule(rule).await?;
            }
            Ok(messages)
        })
    });
    let messages = match placed {
        Ok(messages) => messages,
        Err(e) => {
            eprintln!("tuna-compositor: orientation: no sensor signals, polling: {e}");
            return None;
        }
    };
    let signals = std::sync::Arc::new(Signals {
        dirty: AtomicBool::new(false),
        live: AtomicBool::new(true),
    });
    let thread_signals = signals.clone();
    let spawned = std::thread::Builder::new()
        .name("tuna-orientation".into())
        .spawn(move || {
            let mut messages = messages;
            zbus::block_on(async {
                while let Some(message) = messages.next().await {
                    let Ok(message) = message else {
                        continue;
                    };
                    let header = message.header();
                    let ours = header.message_type() == zbus::message::Type::Signal
                        && header.member().is_some_and(|member| {
                            member.as_str() == "NameOwnerChanged"
                                || member.as_str() == "PropertiesChanged"
                        });
                    if ours {
                        thread_signals.dirty.store(true, Ordering::SeqCst);
                        wake();
                    }
                }
            });
            // The connection closed: the client falls back to polling.
            thread_signals.live.store(false, Ordering::SeqCst);
            thread_signals.dirty.store(true, Ordering::SeqCst);
            wake();
        });
    if let Err(e) = spawned {
        eprintln!("tuna-compositor: orientation: no signal thread, polling: {e}");
        return None;
    }
    Some(signals)
}

impl Default for SensorPoller {
    fn default() -> Self {
        Self::new(|| {})
    }
}

impl Drop for SensorPoller {
    fn drop(&mut self) {
        self.release();
        if let Some(connection) = self.connection.take() {
            let _ = zbus::block_on(connection.close());
        }
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
        let mut poller = SensorPoller::default();
        let _ = poller.poll(false);
        let _ = poller.poll(true);
        poller.release();
    }

    /// A stand-in iio-sensor-proxy counting what the client asks.
    struct FakeSensor {
        orientation: String,
        reads: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        claims: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        releases: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    #[zbus::interface(name = "net.hadess.SensorProxy")]
    impl FakeSensor {
        #[zbus(property)]
        fn has_accelerometer(&self) -> bool {
            self.reads.fetch_add(1, Ordering::SeqCst);
            true
        }

        #[zbus(property)]
        fn accelerometer_orientation(&self) -> String {
            self.orientation.clone()
        }

        fn claim_accelerometer(&self) {
            self.claims.fetch_add(1, Ordering::SeqCst);
        }

        fn release_accelerometer(&self) {
            self.releases.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// #503: the client reads the sensor after `NameOwnerChanged` and
    /// `PropertiesChanged` and after a flag change, and never on a timer:
    /// with nothing signalled nothing is due and nothing is read (the
    /// former scan read it every 100 ms, and re-connected every 5 s while
    /// the service was absent).
    #[test]
    fn the_sensor_client_reads_on_signals_and_flag_changes_only() {
        use std::io::{BufRead, BufReader};
        use std::process::{Child, Command, Stdio};
        use std::sync::atomic::AtomicUsize;
        use std::sync::Arc;

        struct Bus(Child);
        impl Drop for Bus {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mut daemon = Bus(Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("private test bus starts"));
        let mut address = String::new();
        BufReader::new(daemon.0.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        let address = address.trim().to_owned();
        let wakes = Arc::new(AtomicUsize::new(0));
        let woken = |from: usize| {
            let deadline = Instant::now() + Duration::from_secs(10);
            while wakes.load(Ordering::SeqCst) == from {
                assert!(Instant::now() < deadline, "no wake");
                std::thread::sleep(Duration::from_millis(2));
            }
        };
        let mut client = {
            let (wakes, address) = (wakes.clone(), address.clone());
            SensorPoller::with_bus(
                move || {
                    wakes.fetch_add(1, Ordering::SeqCst);
                },
                Box::new(move || {
                    zbus::block_on(zbus::connection::Builder::address(address.as_str()).build())
                }),
            )
        };

        // No service: one read, then the subscription needs no timer.
        assert_eq!(client.poll(false), Some(SensorSample::ABSENT));
        assert_eq!(client.next_due(), None, "a live subscription polls nothing");
        assert_eq!(client.poll(false), None);

        // The service appears: its owner change wakes the client, which
        // claims and reads.
        let (reads, claims, releases) = (
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicUsize::new(0)),
        );
        let before = wakes.load(Ordering::SeqCst);
        let service = zbus::block_on(
            zbus::connection::Builder::address(address.as_str())
                .serve_at(
                    SENSOR_OBJECT_PATH,
                    FakeSensor {
                        orientation: "normal".into(),
                        reads: reads.clone(),
                        claims: claims.clone(),
                        releases: releases.clone(),
                    },
                )
                .name(SENSOR_BUS_NAME)
                .build(),
        )
        .unwrap();
        woken(before);
        assert_eq!(
            client.poll(false),
            Some(SensorSample {
                present: true,
                orientation: Some(SensorOrientation::Normal),
            })
        );
        assert_eq!(claims.load(Ordering::SeqCst), 1, "tracking claims");

        // Quiet: nothing is due and nothing is read.
        let read = reads.load(Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(client.next_due(), None);
        assert_eq!(client.poll(false), None);
        assert_eq!(reads.load(Ordering::SeqCst), read, "no signal, no read");

        // The device turns: `PropertiesChanged` wakes the client.
        let before = wakes.load(Ordering::SeqCst);
        let sensor = zbus::block_on(
            service
                .object_server()
                .interface::<_, FakeSensor>(SENSOR_OBJECT_PATH),
        )
        .unwrap();
        zbus::block_on(async {
            sensor.get_mut().await.orientation = "left-up".into();
            sensor
                .get()
                .await
                .accelerometer_orientation_changed(sensor.signal_emitter())
                .await
        })
        .unwrap();
        woken(before);
        assert_eq!(
            client.poll(false),
            Some(SensorSample {
                present: true,
                orientation: Some(SensorOrientation::LeftUp),
            })
        );

        // Orientation lock: the flag change alone releases the claim.
        assert_eq!(client.poll(true), None, "the sample holds");
        assert_eq!(releases.load(Ordering::SeqCst), 1, "locking releases");

        // The service leaves: its owner change reads as absent.
        let before = wakes.load(Ordering::SeqCst);
        drop(sensor);
        drop(service);
        woken(before);
        assert_eq!(client.poll(true), Some(SensorSample::ABSENT));
        assert_eq!(client.next_due(), None);
    }
}
