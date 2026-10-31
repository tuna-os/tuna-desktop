//! GNOME's Mutter D-Bus interfaces for screen sharing (#61).
//!
//! xdg-desktop-portal-gnome shares screens by asking Mutter, over D-Bus:
//! `org.gnome.Mutter.DisplayConfig` lists the monitors for its picker,
//! and `org.gnome.Mutter.ScreenCast` hands back a PipeWire node per
//! stream. Tuna Desktop serves both, so browsers and video calls share screens
//! through the stock GNOME portal. Adapted from niri's
//! `src/dbus/mutter_screen_cast.rs` and `mutter_display_config.rs`
//! (GPL-3.0-or-later, like Tuna Desktop): monitor, area and window streams. Window
//! ids are the ones `org.gnome.Shell.Introspect` lists (`introspect.rs`).
//!
//! The D-Bus side runs on its own thread; casts start and stop on the
//! compositor's event loop (see `screencast.rs`). When a real GNOME
//! session owns these names (the nested preview), the service stays off.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use zbus::object_server::{InterfaceRef, SignalEmitter};
use zbus::zvariant::{DeserializeDict, OwnedObjectPath, OwnedValue, SerializeDict, Type, Value};
use zbus::{fdo, interface, ObjectServer};

pub(crate) mod remote_desktop;
mod remote_eis;
#[cfg(test)]
mod service_channel_tests;
pub use remote_desktop::Input as RemoteInput;

/// One lit output as the D-Bus side describes it.
#[derive(Debug, Clone, PartialEq)]
pub struct OutputSnapshot {
    pub connector: String,
    pub make: String,
    pub model: String,
    /// Mode in physical pixels, refresh in mHz.
    pub width: i32,
    pub height: i32,
    pub refresh_mhz: i32,
    /// Logical position and scale.
    pub x: i32,
    pub y: i32,
    pub scale: f64,
    pub primary: bool,
    /// DisplayConfig transform (0-7, #345).
    pub transform: u32,
}

/// The runtime keeps this current; D-Bus calls read it.
pub type Outputs = Arc<Mutex<Vec<OutputSnapshot>>>;

/// What one stream shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CastTarget {
    /// A whole monitor, by connector name.
    Monitor(String),
    /// One window, by its Introspect id.
    Window(u64),
    /// Part of a monitor (`RecordArea`, what GNOME's screen recorder
    /// asks for): the monitor's connector and the area in its physical
    /// pixels, `(x, y, width, height)`.
    Area(String, (i32, i32, i32, i32)),
}

/// Where a `RecordArea` rectangle (global logical pixels) lands: the
/// output holding its top-left corner, and the area in that output's
/// physical pixels, clipped to it. `None` when no output holds it or
/// nothing of it is left. Rotation-aware end to end (#345):
/// hit-testing uses the rotated logical extent and the physical rect
/// is the logical rect transformed into buffer pixels through
/// Smithay's `to_buffer`, the same logical-to-buffer mapping the
/// renderer uses for the scanout.
pub fn area_on_output(
    outputs: &[OutputSnapshot],
    (x, y, width, height): (i32, i32, i32, i32),
) -> Option<(OutputSnapshot, (i32, i32, i32, i32))> {
    if width <= 0 || height <= 0 {
        return None;
    }
    let extent = |o: &OutputSnapshot| {
        crate::orientation::logical_extent(o.width, o.height, o.scale, o.transform)
    };
    let output = outputs.iter().find(|o| {
        let (w, h) = extent(o);
        x >= o.x && y >= o.y && x < o.x + w && y < o.y + h
    })?;
    let (lw, lh) = extent(output);
    let rect = smithay::utils::Rectangle::<f64, smithay::utils::Logical>::new(
        (
            f64::from(x.saturating_sub(output.x)),
            f64::from(y.saturating_sub(output.y)),
        )
            .into(),
        (f64::from(width), f64::from(height)).into(),
    );
    let area =
        smithay::utils::Size::<f64, smithay::utils::Logical>::from((f64::from(lw), f64::from(lh)));
    let mapped = rect.to_buffer(
        output.scale,
        crate::orientation::smithay_transform(output.transform),
        &area,
    );
    let (left, top) = (
        (mapped.loc.x.round() as i32).max(0),
        (mapped.loc.y.round() as i32).max(0),
    );
    let right = ((mapped.loc.x + mapped.size.w).round() as i32).min(output.width);
    let bottom = ((mapped.loc.y + mapped.size.h).round() as i32).min(output.height);
    (right > left && bottom > top).then(|| {
        (
            output.clone(),
            (
                left,
                top,
                right.saturating_sub(left),
                bottom.saturating_sub(top),
            ),
        )
    })
}

/// The monitor a `RecordMonitor` stream keeps showing after the
/// monitors changed, or `None` when the stream must stop. Mutter's
/// meta-stream-monitor.c `on_monitors_changed`: the same monitor must
/// still be there (`meta_monitor_is_same_as`; the snapshot has no
/// serial, so connector, make and model), and `update_monitor` keeps
/// the stream only while its logical monitor's rect is unchanged, so a
/// refresh or scale switch that keeps the rect keeps the stream and
/// anything else ends in `meta_stream_stop`.
pub fn monitor_after_change<'a>(
    recorded: &OutputSnapshot,
    outputs: &'a [OutputSnapshot],
) -> Option<&'a OutputSnapshot> {
    let rect = |o: &OutputSnapshot| {
        let (w, h) = crate::orientation::logical_extent(o.width, o.height, o.scale, o.transform);
        (o.x, o.y, w, h)
    };
    outputs.iter().find(|o| {
        o.connector == recorded.connector
            && o.make == recorded.make
            && o.model == recorded.model
            && rect(o) == rect(recorded)
    })
}

/// A window as the D-Bus side describes it (Introspect, RecordWindow).
#[derive(Debug, Clone, PartialEq)]
pub struct WindowSnapshot {
    pub id: u64,
    pub title: String,
    /// Wayland app id or X11 class, as the client set it.
    pub app_id: Option<String>,
    /// Desktop application identity inherited from the ultimate transient
    /// parent, independently of this window's raw app ID / X11 class.
    pub application_id: Option<String>,
    /// Optional sandbox identity from the original live process, not app-id/class.
    pub sandboxed_app_id: Option<String>,
    /// Logical size of the window's visible geometry.
    pub width: i32,
    pub height: i32,
    pub focused: bool,
    /// On an inactive workspace.
    pub hidden: bool,
    /// X11 (through Xwayland) rather than native Wayland.
    pub x11: bool,
    /// Has no live transient parent; used for GNOME running-app eligibility.
    pub standalone: bool,
    /// Eligible for GNOME GetWindows enumeration, independently of app eligibility.
    pub introspect_eligible: bool,
}

/// The runtime keeps this current; Introspect and RecordWindow read it.
pub type Windows = Arc<Mutex<Vec<WindowSnapshot>>>;

/// Requests from D-Bus to the event loop.
pub enum ToLoop {
    RemoteInput {
        session_id: u64,
        grant: Arc<AtomicBool>,
        pending: Arc<std::sync::atomic::AtomicU32>,
        input: RemoteInput,
    },
    RemoteStop {
        session_id: u64,
    },
    RemoteKeymap {
        reply: std::sync::mpsc::Sender<String>,
    },
    StartCast {
        session_id: u64,
        grant: Arc<AtomicBool>,
        target: CastTarget,
        /// The monitor a `RecordMonitor` stream was created for, as
        /// Mutter's MetaStreamMonitor holds its MetaMonitor.
        monitor: Option<OutputSnapshot>,
        signal: CastSignals,
    },
    StopCast {
        session_id: u64,
    },
    /// GNOME Settings' Displays panel applied a verified configuration
    /// (method temporary or persistent). The loop applies it and replies
    /// `Err` with the reason when the hardware refused it (nothing
    /// changed then).
    ApplyMonitors {
        config: crate::monitors::MonitorsConfig,
        method: crate::display_config::Method,
        reply: std::sync::mpsc::Sender<Result<(), String>>,
    },
    /// The accelerometer sample changed (iio-sensor-proxy, #345):
    /// whether a sensor is present and its current orientation.
    SensorSample {
        present: bool,
        orientation: Option<crate::orientation::SensorOrientation>,
    },
    /// GNOME Settings Daemon wrote `PowerSaveMode` (#355). The loop
    /// applies it to the outputs and replies once it is published.
    PowerSave {
        mode: i32,
        reply: std::sync::mpsc::Sender<()>,
    },
}

/// GNOME's backend requests this ordinary Wayland connection before owning
/// its portal bus name. Capture admission remains a separate interface.
struct ServiceChannel {
    display: smithay::reexports::wayland_server::DisplayHandle,
    authority: crate::capture_security::Authority,
    clients: HashMap<String, Vec<ServiceConnection>>,
}

struct ServiceConnection {
    alive: Arc<AtomicBool>,
    typed: bool,
}

fn connection_window_tag(options: &HashMap<String, OwnedValue>) -> fdo::Result<Option<String>> {
    // Mutter ignores unknown options and window-tag values of the wrong type.
    let tag = options
        .get("window-tag")
        .and_then(|value| <&str>::try_from(value).ok());
    if tag.is_some_and(|tag| tag.len() > 1024) {
        return Err(fdo::Error::InvalidArgs(
            "window tag exceeds 1024 bytes".into(),
        ));
    }
    Ok(tag.map(str::to_owned))
}

impl ServiceChannel {
    fn open_connection(
        &mut self,
        owner: String,
        typed: bool,
        window_tag: Option<String>,
        credentials: Option<Arc<fdo::ConnectionCredentials>>,
    ) -> fdo::Result<zbus::zvariant::OwnedFd> {
        self.clients.retain(|_, connections| {
            connections.retain(|connection| connection.alive.load(Ordering::SeqCst));
            !connections.is_empty()
        });
        let count: usize = self.clients.values().map(Vec::len).sum();
        let duplicate_provider = typed
            && self
                .clients
                .get(&owner)
                .is_some_and(|connections| connections.iter().any(|connection| connection.typed));
        if count >= 32 || duplicate_provider {
            return Err(fdo::Error::LimitsExceeded(
                "service connection limit".into(),
            ));
        }
        let (server, client) = std::os::unix::net::UnixStream::pair()
            .map_err(|e| fdo::Error::Failed(e.to_string()))?;
        let alive = Arc::new(AtomicBool::new(true));
        let mut client_data = crate::ClientState::service_connection(alive.clone(), window_tag);
        client_data.sandboxed_app_id = credentials
            .clone()
            .map(crate::sandbox_identity::discover_async)
            .unwrap_or_default();
        client_data.original_credentials = credentials;
        client_data.x11_interop = typed;
        self.display
            .insert_client(server, Arc::new(client_data))
            .map_err(|e| fdo::Error::Failed(e.to_string()))?;
        self.clients
            .entry(owner)
            .or_default()
            .push(ServiceConnection { alive, typed });
        Ok(std::os::fd::OwnedFd::from(client).into())
    }
}

#[interface(name = "org.gnome.Mutter.ServiceChannel")]
impl ServiceChannel {
    async fn open_wayland_service_connection(
        &mut self,
        service_client_type: u32,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> fdo::Result<zbus::zvariant::OwnedFd> {
        let role = crate::capture_security::ServiceClient::from_wire(service_client_type)?;
        let caller = self
            .authority
            .admit_service_connection(conn, &header, role)
            .await?;
        self.open_connection(caller.owner, true, None, Some(caller.credentials))
    }
    async fn open_wayland_connection(
        &mut self,
        options: HashMap<String, OwnedValue>,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> fdo::Result<zbus::zvariant::OwnedFd> {
        let caller = self.authority.admit_connection(conn, &header).await?;
        let tag = connection_window_tag(&options)?;
        self.open_connection(caller.owner, false, tag, Some(caller.credentials))
    }
}

// --- DisplayConfig -----------------------------------------------------

/// `(ssss)a(siiddada{sv})a{sv}`: one connected monitor.
#[derive(Serialize, Type)]
struct Monitor {
    names: (String, String, String, String),
    modes: Vec<Mode>,
    properties: HashMap<String, OwnedValue>,
}

/// `(siiddada{sv})`: one of its modes.
#[derive(Serialize, Type)]
struct Mode {
    id: String,
    width: i32,
    height: i32,
    refresh_rate: f64,
    preferred_scale: f64,
    supported_scales: Vec<f64>,
    properties: HashMap<String, OwnedValue>,
}

/// `(iiduba(ssss)a{sv})`: one logical monitor.
#[derive(Serialize, Type)]
struct LogicalMonitor {
    x: i32,
    y: i32,
    scale: f64,
    transform: u32,
    is_primary: bool,
    monitors: Vec<(String, String, String, String)>,
    properties: HashMap<String, OwnedValue>,
}

/// `(iiduba(ssa{sv}))`: one requested logical monitor.
#[derive(Deserialize, Type)]
struct LogicalMonitorConfiguration {
    x: i32,
    y: i32,
    scale: f64,
    transform: u32,
    is_primary: bool,
    monitors: Vec<(String, String, HashMap<String, OwnedValue>)>,
}

impl LogicalMonitorConfiguration {
    /// The request with GNOME Settings' per-monitor properties read
    /// (`underscanning` b, `color-mode` u), as Mutter's
    /// `create_monitor_config_from_variant` reads them.
    fn request(&self) -> fdo::Result<crate::display_config::LogicalRequest> {
        let monitors = self
            .monitors
            .iter()
            .map(|(connector, mode_id, properties)| {
                let underscanning = properties
                    .get("underscanning")
                    .map(bool::try_from)
                    .transpose()
                    .map_err(|_| fdo::Error::InvalidArgs("underscanning must be b".into()))?;
                let color_mode = properties
                    .get("color-mode")
                    .map(u32::try_from)
                    .transpose()
                    .map_err(|_| fdo::Error::InvalidArgs("color-mode must be u".into()))?;
                Ok(crate::display_config::MonitorRequest {
                    connector: connector.clone(),
                    mode_id: mode_id.clone(),
                    underscanning,
                    color_mode,
                })
            })
            .collect::<fdo::Result<Vec<_>>>()?;
        Ok(crate::display_config::LogicalRequest {
            x: self.x,
            y: self.y,
            scale: self.scale,
            transform: self.transform,
            primary: self.is_primary,
            monitors,
        })
    }
}

struct DisplayConfig {
    display: Arc<crate::display_config::DisplayShare>,
    to_loop: calloop::channel::Sender<ToLoop>,
    orientation: std::sync::Arc<crate::orientation::OrientationShare>,
    power_save: std::sync::Arc<crate::power_save::PowerSaveShare>,
}

fn spec_names(spec: &crate::monitors::MonitorSpec) -> (String, String, String, String) {
    (
        spec.connector.clone(),
        spec.vendor.clone(),
        spec.product.clone(),
        spec.serial.clone(),
    )
}

/// Mutter's `handle_get_current_state` over the published state:
/// every advertised mode with its supported and preferred scales and
/// `is-current`/`is-preferred`/`is-interlaced` only when true; the
/// monitor properties Mutter 51 always sends.
#[allow(clippy::type_complexity)]
fn current_state(
    state: &crate::display_config::DisplayState,
) -> (
    u32,
    Vec<Monitor>,
    Vec<LogicalMonitor>,
    HashMap<String, OwnedValue>,
) {
    use crate::display_config::{should_be_advertised, supported_scales};
    let flag = |name: &str| (name.to_owned(), OwnedValue::from(true));
    let monitors = state
        .monitors
        .iter()
        .map(|monitor| {
            let preferred = monitor.modes[monitor.preferred];
            let modes = monitor
                .modes
                .iter()
                .enumerate()
                .filter(|(_, mode)| should_be_advertised(mode, &preferred))
                .map(|(index, mode)| {
                    let mut properties = HashMap::new();
                    if monitor.current == Some(index) {
                        properties.extend([flag("is-current")]);
                    }
                    if monitor.preferred == index {
                        properties.extend([flag("is-preferred")]);
                    }
                    if mode.interlaced {
                        properties.extend([flag("is-interlaced")]);
                    }
                    Mode {
                        id: mode.id(),
                        width: mode.width,
                        height: mode.height,
                        refresh_rate: f64::from(mode.refresh),
                        preferred_scale: f64::from(state.preferred_scale(monitor, index)),
                        supported_scales: supported_scales(mode.width, mode.height)
                            .into_iter()
                            .map(f64::from)
                            .collect(),
                        properties,
                    }
                })
                .collect();
            let mut properties = HashMap::from([
                ("is-builtin".to_owned(), OwnedValue::from(monitor.builtin)),
                (
                    "display-name".to_owned(),
                    OwnedValue::try_from(Value::from(monitor.display_name.clone()))
                        .expect("string value"),
                ),
                ("is-for-lease".to_owned(), OwnedValue::from(false)),
                // SDR only: HDR colour modes are P-DS-03.
                ("color-mode".to_owned(), OwnedValue::from(0u32)),
                (
                    "supported-color-modes".to_owned(),
                    OwnedValue::try_from(Value::from(vec![0u32])).expect("array value"),
                ),
            ]);
            // Mutter's `(bb)`: enabled, hardware-locked; absent without
            // a privacy screen (#356).
            if let Some(privacy) = monitor.privacy_screen {
                properties.insert(
                    "privacy-screen-state".to_owned(),
                    OwnedValue::try_from(Value::from(zbus::zvariant::Structure::from((
                        privacy.enabled,
                        privacy.locked,
                    ))))
                    .expect("structure value"),
                );
            }
            Monitor {
                names: spec_names(&monitor.spec),
                modes,
                properties,
            }
        })
        .collect();
    let logical = state
        .logical
        .iter()
        .map(|l| LogicalMonitor {
            x: l.x,
            y: l.y,
            scale: l.scale,
            transform: l.transform,
            is_primary: l.primary,
            monitors: l
                .monitors
                .iter()
                .filter_map(|c| state.monitor(c).map(|m| spec_names(&m.spec)))
                .collect(),
            properties: HashMap::new(),
        })
        .collect();
    let properties = HashMap::from([(
        "layout-mode".to_owned(),
        // 1: logical layout (Mutter 51's native default and only mode here).
        OwnedValue::from(1u32),
    )]);
    (state.serial, monitors, logical, properties)
}

fn apply_error(error: crate::display_config::ApplyError) -> fdo::Error {
    match error {
        crate::display_config::ApplyError::AccessDenied(m) => fdo::Error::AccessDenied(m),
        crate::display_config::ApplyError::InvalidArgs(m) => fdo::Error::InvalidArgs(m),
    }
}

/// Emit `MonitorsChanged` for every state change the runtime publishes
/// (an applied, reverted or kept configuration, a mode the kernel
/// refused). Ends when the share drops its sender.
fn spawn_monitors_changed(conn: zbus::Connection, changes: std::sync::mpsc::Receiver<()>) {
    std::thread::spawn(move || {
        let Ok(emitter) = SignalEmitter::new(&conn, "/org/gnome/Mutter/DisplayConfig") else {
            return;
        };
        for () in changes {
            let _ = zbus::block_on(DisplayConfig::monitors_changed(&emitter));
        }
    });
}

/// g-s-d's Night Light properties (#339), served from the compositor
/// on the same bus name: whether the filter applies, and the
/// read-write "disabled until tomorrow". The loop owns the schedule
/// and publishes through the share; this side only reads the applied
/// state and records the disable timestamp.
struct NightLightColor {
    share: std::sync::Arc<crate::night_light::NightLightShare>,
}

#[interface(name = "org.gnome.SettingsDaemon.Color")]
impl NightLightColor {
    /// Whether the filter applies right now.
    #[zbus(property)]
    fn night_light_active(&self) -> bool {
        self.share.active()
    }

    /// Whether "disabled until tomorrow" is recorded. The loop
    /// decides whether it still suppresses (24 h lapse, sunrise).
    #[zbus(property)]
    fn disabled_until_tomorrow(&self) -> bool {
        self.share.disabled_until_tmw().is_some()
    }

    /// Applied colour temperature in Kelvin (6500 when neutral).
    #[zbus(property)]
    fn temperature(&self) -> u32 {
        self.share.temperature()
    }

    /// g-s-d `handle_set_property`: values outside 1000..=10000 K
    /// fail with InvalidArgs; others apply until Night Light's own
    /// temperature next changes.
    #[zbus(property)]
    fn set_temperature(&self, value: u32) -> fdo::Result<()> {
        self.share
            .set_temperature_override(value)
            .map_err(|kelvin| {
                use crate::night_light::{MAX_TEMPERATURE_K, MIN_TEMPERATURE_K};
                fdo::Error::InvalidArgs(if kelvin < MIN_TEMPERATURE_K {
                    format!("{kelvin}K is < min {MIN_TEMPERATURE_K}K")
                } else {
                    format!("{kelvin}K is > max {MAX_TEMPERATURE_K}K")
                })
            })
    }

    /// Local sunrise in fractional hours; -1 before any location.
    #[zbus(property)]
    fn sunrise(&self) -> f64 {
        self.share.sunrise()
    }

    /// Local sunset in fractional hours; -1 before any location.
    #[zbus(property)]
    fn sunset(&self) -> f64 {
        self.share.sunset()
    }

    /// Record (or clear) "disabled until tomorrow". Setting stamps
    /// the current time; a wedged clock records no disable rather
    /// than a bogus one.
    #[zbus(property)]
    fn set_disabled_until_tomorrow(&self, value: bool) {
        let set_at =
            value.then(|| crate::night_light::system_unix_seconds(std::time::SystemTime::now()));
        self.share.set_disabled_until_tmw(set_at.flatten());
    }

    /// Slider preview (#339): force the freshly written temperature
    /// for `duration` seconds; the share expires it automatically and
    /// the schedule resumes. Durations outside 1..=120 are rejected
    /// as InvalidArgs without touching any live preview; a poisoned
    /// share (nothing recorded) reports Failed instead. The owned reader must
    /// return a fresh, request-bound temperature; failures never use stale cache.
    async fn night_light_preview(&self, duration: u32) -> fdo::Result<()> {
        let result = match self.share.request_preview(duration) {
            Ok(request) => request.await,
            Err(error) => Err(error),
        };
        result.map_err(|rejected| {
            use crate::night_light::PreviewRejected;
            match rejected {
                PreviewRejected::InvalidDuration(_) => {
                    fdo::Error::InvalidArgs("Duration is out of the range (0-120].".into())
                }
                PreviewRejected::Unavailable => {
                    fdo::Error::Failed("night light preview unavailable".into())
                }
            }
        })
    }
}

/// The single `PropertiesChanged` entry g-s-d's `emit_property_changed`
/// sends for one change (`Temperature` as `roundf`'s `u`).
fn color_change_property(
    change: crate::night_light::ColorChange,
) -> (&'static str, Value<'static>) {
    use crate::night_light::ColorChange;
    match change {
        ColorChange::NightLightActive(value) => ("NightLightActive", Value::from(value)),
        ColorChange::Temperature(kelvin) => ("Temperature", Value::from(kelvin)),
        ColorChange::DisabledUntilTomorrow(value) => ("DisabledUntilTomorrow", Value::from(value)),
        ColorChange::Sunrise(hours) => ("Sunrise", Value::from(hours)),
        ColorChange::Sunset(hours) => ("Sunset", Value::from(hours)),
    }
}

/// Emit one `org.freedesktop.DBus.Properties.PropertiesChanged` per
/// change on `/org/gnome/SettingsDaemon/Color`, in order and with an
/// empty invalidated list, as `gsd-color-manager.c` does. Ends when the
/// share drops its sender.
fn spawn_color_signals(
    conn: zbus::Connection,
    changes: std::sync::mpsc::Receiver<crate::night_light::ColorChange>,
) {
    std::thread::spawn(move || {
        let Ok(emitter) = SignalEmitter::new(&conn, "/org/gnome/SettingsDaemon/Color") else {
            return;
        };
        let interface =
            zbus::names::InterfaceName::from_static_str_unchecked("org.gnome.SettingsDaemon.Color");
        for change in changes {
            let (name, value) = color_change_property(change);
            let _ = zbus::block_on(fdo::Properties::properties_changed(
                &emitter,
                interface.clone(),
                HashMap::from([(name, value)]),
                std::borrow::Cow::Borrowed(&[]),
            ));
        }
    });
}

#[cfg(test)]
mod color_signal_tests {
    use super::*;
    use crate::night_light::{ColorChange, NightLightShare};

    /// Over a real peer-to-peer D-Bus link: each share change arrives
    /// as its own `PropertiesChanged` with g-s-d's shape, unchanged
    /// writes and `Temperature` property writes send nothing.
    #[test]
    fn share_changes_reach_the_wire_as_gsd_properties_changed() {
        let (server_end, client_end) = std::os::unix::net::UnixStream::pair().unwrap();
        let server = std::thread::spawn(move || {
            zbus::blocking::connection::Builder::async_io_unix_stream(server_end)
                .server(zbus::Guid::generate())
                .unwrap()
                .p2p()
                .build()
                .unwrap()
        });
        let client = zbus::blocking::connection::Builder::async_io_unix_stream(client_end)
            .p2p()
            .build()
            .unwrap();
        let server = server.join().unwrap();
        let share = Arc::new(NightLightShare::default());
        spawn_color_signals(server.inner().clone(), share.subscribe_changes());
        let mut messages = zbus::blocking::MessageIterator::from(&client);

        share.set_disabled_until_tmw(Some(1));
        share.set_disabled_until_tmw(Some(2)); // still set: no signal
        share.set_temperature_override(4000).unwrap(); // gsd: no signal
        share.publish(true, 4000);
        share.publish(true, 3900); // active unchanged: no signal
        share.set_disabled_until_tmw(None);

        let mut seen = Vec::new();
        while seen.len() < 3 {
            let message = messages.next().unwrap().unwrap();
            let header = message.header();
            assert_eq!(
                header.interface().map(|i| i.as_str()),
                Some("org.freedesktop.DBus.Properties")
            );
            assert_eq!(
                header.member().map(|m| m.as_str()),
                Some("PropertiesChanged")
            );
            assert_eq!(
                header.path().map(|p| p.as_str()),
                Some("/org/gnome/SettingsDaemon/Color")
            );
            let (interface, changed, invalidated): (
                String,
                HashMap<String, OwnedValue>,
                Vec<String>,
            ) = message.body().deserialize().unwrap();
            assert_eq!(interface, "org.gnome.SettingsDaemon.Color");
            assert!(invalidated.is_empty());
            assert_eq!(changed.len(), 1);
            let (name, value) = changed.into_iter().next().unwrap();
            seen.push((name, bool::try_from(value).unwrap()));
        }
        assert_eq!(
            seen,
            [
                ("DisabledUntilTomorrow".to_string(), true),
                ("NightLightActive".to_string(), true),
                ("DisabledUntilTomorrow".to_string(), false),
            ]
        );
    }

    #[test]
    fn every_change_maps_to_its_gsd_property_and_type() {
        assert_eq!(
            color_change_property(ColorChange::Temperature(2700)),
            ("Temperature", Value::U32(2700))
        );
        assert_eq!(
            color_change_property(ColorChange::Sunrise(6.5)),
            ("Sunrise", Value::F64(6.5))
        );
        assert_eq!(
            color_change_property(ColorChange::Sunset(19.25)),
            ("Sunset", Value::F64(19.25))
        );
        assert_eq!(
            color_change_property(ColorChange::NightLightActive(false)),
            ("NightLightActive", Value::Bool(false))
        );
    }
}

#[interface(name = "org.gnome.Mutter.DisplayConfig")]
impl DisplayConfig {
    #[allow(clippy::type_complexity)]
    fn get_current_state(
        &self,
    ) -> fdo::Result<(
        u32,
        Vec<Monitor>,
        Vec<LogicalMonitor>,
        HashMap<String, OwnedValue>,
    )> {
        Ok(current_state(&self.display.snapshot()))
    }

    /// GNOME Settings' Displays panel, as Mutter 51 handles it: 0
    /// verifies, 1 applies, 2 applies and asks GNOME Shell to confirm
    /// (reverting after 20 s unless kept). The loop applies the
    /// configuration (testing new modes on the hardware first) and
    /// answers before this call returns.
    async fn apply_monitors_config(
        &self,
        serial: u32,
        method: u32,
        logical_monitor_configs: Vec<LogicalMonitorConfiguration>,
        properties: HashMap<String, OwnedValue>,
    ) -> fdo::Result<()> {
        let method = crate::display_config::Method::from_dbus(method).map_err(apply_error)?;
        let layout_mode = properties
            .get("layout-mode")
            .map(u32::try_from)
            .transpose()
            .map_err(|_| fdo::Error::InvalidArgs("layout-mode must be u".into()))?;
        let requests = logical_monitor_configs
            .iter()
            .map(LogicalMonitorConfiguration::request)
            .collect::<fdo::Result<Vec<_>>>()?;
        let config =
            crate::display_config::verify(&self.display.snapshot(), serial, layout_mode, &requests)
                .map_err(apply_error)?;
        if method == crate::display_config::Method::Verify {
            return Ok(());
        }
        let (reply, applied) = std::sync::mpsc::channel();
        self.to_loop
            .send(ToLoop::ApplyMonitors {
                config,
                method,
                reply,
            })
            .map_err(|_| fdo::Error::Failed("compositor gone".into()))?;
        match applied.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(message)) => Err(fdo::Error::InvalidArgs(message)),
            Err(_) => Err(fdo::Error::Failed(
                "the display configuration was not applied".into(),
            )),
        }
    }

    #[zbus(signal)]
    async fn monitors_changed(ctxt: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(property)]
    fn apply_monitors_config_allowed(&self) -> bool {
        true
    }

    /// The applied display power-save mode (#355): 0 on, 1 standby,
    /// 2 suspend, 3 off, -1 unknown.
    #[zbus(property)]
    fn power_save_mode(&self) -> i32 {
        self.power_save.mode()
    }

    /// gnome-settings-daemon blanks with 3 and unblanks with 0. The loop
    /// owns the outputs; the write completes once it applied the mode, so
    /// the PropertiesChanged that follows carries the applied value.
    /// Mutter ignores -1 (`power_save_mode_changed`).
    #[zbus(property)]
    fn set_power_save_mode(&self, mode: i32) -> fdo::Result<()> {
        if mode == crate::power_save::UNSUPPORTED {
            return Ok(());
        }
        let (reply, applied) = std::sync::mpsc::channel();
        self.to_loop
            .send(ToLoop::PowerSave { mode, reply })
            .map_err(|_| fdo::Error::Failed("compositor gone".into()))?;
        applied
            .recv_timeout(std::time::Duration::from_secs(2))
            .map_err(|_| fdo::Error::Failed("power save mode not applied".into()))
    }

    #[zbus(property)]
    fn panel_orientation_managed(&self) -> bool {
        self.orientation.is_managed()
    }

    /// Night Light applies a live GL tint on every output (#339),
    /// driven by the settings keys and the schedule.
    #[zbus(property)]
    fn night_light_supported(&self) -> bool {
        true
    }
}

// --- ScreenCast ----------------------------------------------------------

type Sessions = Arc<Mutex<Vec<(Session, OwnedObjectPath)>>>;

#[derive(Clone)]
struct ScreenCast {
    remote: remote_desktop::RemoteSessions,
    authority: crate::capture_security::Authority,
    sessions: Sessions,
    outputs: Outputs,
    windows: Windows,
    to_loop: calloop::channel::Sender<ToLoop>,
    signals: crate::dbus_signals::SignalQueue,
    next_id: Arc<AtomicU64>,
}

/// A session's streams with their D-Bus interface handles.
type SessionStreams = Arc<Mutex<Vec<(Stream, InterfaceRef<Stream>)>>>;

#[derive(Clone)]
struct Session {
    owner: String,
    authority: crate::capture_security::Authority,
    id: u64,
    outputs: Outputs,
    windows: Windows,
    to_loop: calloop::channel::Sender<ToLoop>,
    signals: crate::dbus_signals::SignalQueue,
    next_id: Arc<AtomicU64>,
    streams: SessionStreams,
    stopped: Arc<AtomicBool>,
    started: Arc<AtomicBool>,
    /// `Closed` went out: Mutter's meta_dbus_session_close emits it
    /// once, whichever of Stop, a stream's end or revocation came first.
    closed: Arc<AtomicBool>,
}

#[derive(Clone)]
enum Stream {
    Monitor(OutputSnapshot),
    Window(WindowSnapshot),
    /// The output, the requested logical area, and its physical pixels.
    Area(OutputSnapshot, (i32, i32, i32, i32), (i32, i32, i32, i32)),
}

impl Stream {
    fn target(&self) -> CastTarget {
        match self {
            Stream::Monitor(output) => CastTarget::Monitor(output.connector.clone()),
            Stream::Window(window) => CastTarget::Window(window.id),
            Stream::Area(output, _, physical) => {
                CastTarget::Area(output.connector.clone(), *physical)
            }
        }
    }
}

#[derive(Debug, DeserializeDict, Type)]
#[zvariant(signature = "dict")]
struct RecordWindowProperties {
    #[zvariant(rename = "window-id")]
    window_id: Option<u64>,
    #[zvariant(rename = "cursor-mode")]
    _cursor_mode: Option<u32>,
    #[zvariant(rename = "is-recording")]
    _is_recording: Option<bool>,
}

#[derive(Debug, DeserializeDict, Type)]
#[zvariant(signature = "dict")]
struct RecordMonitorProperties {
    #[zvariant(rename = "cursor-mode")]
    _cursor_mode: Option<u32>,
    #[zvariant(rename = "is-recording")]
    _is_recording: Option<bool>,
}

#[derive(Debug, SerializeDict, Type, Value)]
#[zvariant(signature = "dict")]
struct StreamParameters {
    /// Position in logical coordinates.
    position: (i32, i32),
    /// Size in logical coordinates.
    size: (i32, i32),
}

#[interface(name = "org.gnome.Mutter.ScreenCast")]
impl ScreenCast {
    async fn create_session(
        &mut self,
        #[zbus(object_server)] server: &ObjectServer,
        properties: HashMap<String, OwnedValue>,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> fdo::Result<OwnedObjectPath> {
        let owner = self.authority.admit(conn, &header).await?;
        if self
            .sessions
            .lock()
            .map_or(true, |sessions| sessions.len() >= 256)
        {
            return Err(fdo::Error::LimitsExceeded(
                "too many capture sessions".into(),
            ));
        }
        let linked = if let Some(value) = properties.get("remote-desktop-session-id") {
            let remote_id = <&str>::try_from(value)
                .map_err(|_| fdo::Error::InvalidArgs("invalid remote session id".into()))?;
            let remote = self
                .remote
                .lock()
                .map_err(|_| fdo::Error::Failed("remote registry unavailable".into()))?;
            let grant = remote
                .get(remote_id)
                .ok_or_else(|| crate::capture_security::denied("unknown remote session"))?;
            if grant.owner != owner
                || grant.stopped.load(Ordering::SeqCst)
                || grant.started.load(Ordering::SeqCst)
            {
                return Err(crate::capture_security::denied(
                    "remote grant cannot accept this stream",
                ));
            }
            Some(grant.stopped.clone())
        } else {
            None
        };
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let path = format!("/org/gnome/Mutter/ScreenCast/Session/u{id}");
        let path =
            OwnedObjectPath::try_from(path).map_err(|e| fdo::Error::Failed(e.to_string()))?;
        let session = Session {
            owner,
            authority: self.authority.clone(),
            id,
            outputs: self.outputs.clone(),
            windows: self.windows.clone(),
            to_loop: self.to_loop.clone(),
            signals: self.signals.clone(),
            next_id: self.next_id.clone(),
            streams: Arc::new(Mutex::new(Vec::new())),
            stopped: linked.unwrap_or_else(|| Arc::new(AtomicBool::new(false))),
            started: Arc::new(AtomicBool::new(false)),
            closed: Arc::new(AtomicBool::new(false)),
        };
        server.at(&path, session.clone()).await?;
        self.sessions
            .lock()
            .map_err(|_| fdo::Error::Failed("session registry unavailable".into()))?
            .push((session, path.clone()));
        Ok(path)
    }

    #[zbus(property)]
    fn version(&self) -> i32 {
        4
    }
}

impl Session {
    fn start_streams(&self) -> fdo::Result<()> {
        self.authority.unlocked()?;
        if self.stopped.load(Ordering::SeqCst) {
            return Err(crate::capture_security::denied("session revoked"));
        }
        if self.started.swap(true, Ordering::SeqCst) {
            return Err(fdo::Error::Failed("capture session already started".into()));
        }
        let streams = self.streams.lock().map(|s| s.clone()).unwrap_or_default();
        for (stream, iface) in streams {
            let _ = self.to_loop.send(ToLoop::StartCast {
                session_id: self.id,
                grant: self.stopped.clone(),
                target: stream.target(),
                monitor: match &stream {
                    Stream::Monitor(output) => Some(output.clone()),
                    Stream::Window(_) | Stream::Area(..) => None,
                },
                signal: CastSignals {
                    stream: iface.signal_emitter().to_owned(),
                    queue: self.signals.clone(),
                    closed: self.closed.clone(),
                },
            });
        }
        Ok(())
    }
}

#[interface(name = "org.gnome.Mutter.ScreenCast.Session")]
impl Session {
    async fn start(&self, #[zbus(header)] header: zbus::message::Header<'_>) -> fdo::Result<()> {
        self.check_owner(&header)?;
        self.start_streams()
    }

    async fn stop(
        &self,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(signal_context)] ctxt: SignalEmitter<'_>,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> fdo::Result<()> {
        self.check_owner(&header)?;
        if self.stopped.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        if !self.closed.swap(true, Ordering::SeqCst) {
            let _ = Session::closed(&ctxt).await;
        }
        let _ = self.to_loop.send(ToLoop::StopCast {
            session_id: self.id,
        });
        let streams = self
            .streams
            .lock()
            .map(|mut s| std::mem::take(&mut *s))
            .unwrap_or_default();
        for (_, iface) in streams {
            let _ = server
                .remove::<Stream, _>(iface.signal_emitter().path())
                .await;
        }
        let _ = server.remove::<Session, _>(ctxt.path()).await;
        Ok(())
    }

    async fn record_monitor(
        &mut self,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(header)] header: zbus::message::Header<'_>,
        connector: &str,
        _properties: RecordMonitorProperties,
    ) -> fdo::Result<OwnedObjectPath> {
        self.check_owner(&header)?;
        self.authority.unlocked()?;
        let output = self
            .outputs
            .lock()
            .ok()
            .and_then(|o| o.iter().find(|o| o.connector == connector).cloned())
            .ok_or_else(|| fdo::Error::Failed("no such monitor".into()))?;
        self.add_stream(server, Stream::Monitor(output)).await
    }

    /// Part of the screen, in global logical pixels: what GNOME's screen
    /// recorder (screencastService.js) asks for, the whole monitor
    /// included.
    #[allow(clippy::too_many_arguments)] // fixed Mutter D-Bus signature plus authenticated caller
    async fn record_area(
        &mut self,
        #[zbus(object_server)] server: &ObjectServer,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        _properties: RecordMonitorProperties,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> fdo::Result<OwnedObjectPath> {
        self.check_owner(&header)?;
        self.authority.unlocked()?;
        let outputs = self.outputs.lock().map(|o| o.clone()).unwrap_or_default();
        let (output, physical) = area_on_output(&outputs, (x, y, width, height))
            .ok_or_else(|| fdo::Error::Failed("the area is on no monitor".into()))?;
        self.add_stream(
            server,
            Stream::Area(output, (x, y, width, height), physical),
        )
        .await
    }

    /// GNOME's window share: the portal's picker lists windows from
    /// `org.gnome.Shell.Introspect.GetWindows` and passes the chosen id.
    async fn record_window(
        &mut self,
        #[zbus(object_server)] server: &ObjectServer,
        properties: RecordWindowProperties,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> fdo::Result<OwnedObjectPath> {
        self.check_owner(&header)?;
        self.authority.unlocked()?;
        let id = properties
            .window_id
            .ok_or_else(|| fdo::Error::InvalidArgs("window-id is required".into()))?;
        let window = self
            .windows
            .lock()
            .ok()
            .and_then(|w| w.iter().find(|w| w.id == id).cloned())
            .ok_or_else(|| fdo::Error::Failed("no such window".into()))?;
        self.add_stream(server, Stream::Window(window)).await
    }

    #[zbus(signal)]
    async fn closed(ctxt: &SignalEmitter<'_>) -> zbus::Result<()>;
}

impl Session {
    fn check_owner(&self, header: &zbus::message::Header<'_>) -> fdo::Result<()> {
        if crate::capture_security::owns_session(
            &self.owner,
            header.sender().map(|sender| sender.as_str()),
            self.stopped.load(Ordering::SeqCst),
        ) {
            Ok(())
        } else {
            Err(crate::capture_security::denied(
                "session belongs to another caller or was revoked",
            ))
        }
    }
    async fn add_stream(
        &self,
        server: &ObjectServer,
        stream: Stream,
    ) -> fdo::Result<OwnedObjectPath> {
        if self.started.load(Ordering::SeqCst) {
            return Err(fdo::Error::Failed("capture session already started".into()));
        }
        if self
            .streams
            .lock()
            .map_or(true, |streams| streams.len() >= 64)
        {
            return Err(fdo::Error::LimitsExceeded(
                "too many capture streams".into(),
            ));
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let path = format!("/org/gnome/Mutter/ScreenCast/Stream/u{id}");
        let path =
            OwnedObjectPath::try_from(path).map_err(|e| fdo::Error::Failed(e.to_string()))?;
        server.at(&path, stream.clone()).await?;
        let iface = server.interface::<_, Stream>(&path).await?;
        if let Ok(mut streams) = self.streams.lock() {
            streams.push((stream, iface));
        }
        Ok(path)
    }
}

#[interface(name = "org.gnome.Mutter.ScreenCast.Stream")]
impl Stream {
    #[zbus(signal)]
    pub async fn pipe_wire_stream_added(ctxt: &SignalEmitter<'_>, node_id: u32)
        -> zbus::Result<()>;

    #[zbus(property)]
    fn parameters(&self) -> StreamParameters {
        match self {
            Stream::Monitor(o) => StreamParameters {
                position: (o.x, o.y),
                size: (
                    (f64::from(o.width) / o.scale).round() as i32,
                    (f64::from(o.height) / o.scale).round() as i32,
                ),
            },
            // A window has no place on a monitor layout.
            Stream::Window(w) => StreamParameters {
                position: (0, 0),
                size: (w.width.max(1), w.height.max(1)),
            },
            Stream::Area(_, (x, y, w, h), _) => StreamParameters {
                position: (*x, *y),
                size: (*w, *h),
            },
        }
    }
}

/// A cast stream's signals, sent from the event loop through the mutter
/// thread's [`SignalQueue`](crate::dbus_signals::SignalQueue): the loop
/// never waits on the bus (#657).
#[derive(Clone)]
pub struct CastSignals {
    stream: SignalEmitter<'static>,
    queue: crate::dbus_signals::SignalQueue,
    /// The session's `Closed` latch, shared with its D-Bus object.
    closed: Arc<AtomicBool>,
}

impl CastSignals {
    /// Announce the stream's PipeWire node.
    pub fn stream_added(&self, node_id: u32) {
        let stream = self.stream.clone();
        self.queue
            .emit(async move { Stream::pipe_wire_stream_added(&stream, node_id).await });
    }

    /// Tell the session's client it ended; the signal goes out on the
    /// session, after anything this stream already queued, once per
    /// session however many of its streams end.
    pub fn session_closed(&self, session_id: u64) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        let path = format!("/org/gnome/Mutter/ScreenCast/Session/u{session_id}");
        if let Ok(session) = SignalEmitter::new(self.stream.connection(), path) {
            self.queue
                .emit(async move { Session::closed(&session).await });
        }
    }
}

/// How long the revocation scan waits for the bus to vouch for a
/// capture owner. A bus that cannot answer in time revokes the grant
/// (fail closed) rather than stalling every other revocation (#657).
const OWNER_CHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Whether each of `owners` is still an admitted capture owner, asked
/// concurrently on the connection's executor and bounded by `timeout`
/// overall. An unanswered check counts as gone. The checks themselves
/// are not cancelled: dropping a send mid-write would corrupt the link.
fn owners_alive(
    authority: &crate::capture_security::Authority,
    conn: &zbus::Connection,
    owners: &[String],
    timeout: std::time::Duration,
) -> Vec<bool> {
    let pending: Vec<_> = owners
        .iter()
        .map(|owner| {
            let (reply, answer) = std::sync::mpsc::sync_channel(1);
            let (authority, task_conn, owner) = (authority.clone(), conn.clone(), owner.clone());
            conn.executor()
                .spawn(
                    async move {
                        let _ = reply.send(authority.owner_alive(&task_conn, &owner).await);
                    },
                    "tuna-capture-owner-check",
                )
                .detach();
            answer
        })
        .collect();
    let deadline = std::time::Instant::now() + timeout;
    pending
        .into_iter()
        .map(|answer| {
            answer
                .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                .unwrap_or(false)
        })
        .collect()
}

/// Serve DisplayConfig and ScreenCast on the session bus from a thread.
/// Returns the channel the event loop reads cast requests from.
#[allow(clippy::too_many_arguments)] // one share per service the thread serves
pub fn start(
    outputs: Outputs,
    windows: Windows,
    authority: crate::capture_security::Authority,
    display: smithay::reexports::wayland_server::DisplayHandle,
    orientation: std::sync::Arc<crate::orientation::OrientationShare>,
    night_light: std::sync::Arc<crate::night_light::NightLightShare>,
    power_save: std::sync::Arc<crate::power_save::PowerSaveShare>,
    display_state: Arc<crate::display_config::DisplayShare>,
    color: Arc<crate::color_manager::ColorShare>,
) -> calloop::channel::Channel<ToLoop> {
    let (to_loop, from_dbus) = calloop::channel::channel();
    let _ = std::thread::Builder::new()
        .name("tuna-mutter-dbus".into())
        .spawn(move || {
            let display_config = DisplayConfig {
                display: display_state.clone(),
                to_loop: to_loop.clone(),
                orientation: orientation.clone(),
                power_save: power_save.clone(),
            };
            // Accelerometer for sensor-driven orientation (#345): polled
            // with the admission scan, reported to the loop on change.
            let mut sensor = crate::orientation::SensorPoller::new();
            let sessions: Sessions = Default::default();
            let remote: remote_desktop::RemoteSessions = Default::default();
            let next_id = Arc::new(AtomicU64::new(1));
            // Signals the loop and the revocation scan send without
            // waiting on the bus (#657).
            let signals = crate::dbus_signals::SignalQueue::new("tuna-mutter-signals");
            let screencast = ScreenCast {
                remote: remote.clone(),
                sessions: sessions.clone(),
                authority: authority.clone(),
                outputs: outputs.clone(),
                windows,
                to_loop: to_loop.clone(),
                signals: signals.clone(),
                next_id: next_id.clone(),
            };
            let remote_desktop = remote_desktop::RemoteDesktop {
                authority: authority.clone(),
                remote: remote.clone(),
                captures: sessions.clone(),
                outputs: outputs.clone(),
                to_loop: to_loop.clone(),
                next_id,
            };
            let service_channel = ServiceChannel {
                display,
                authority: authority.clone(),
                clients: HashMap::new(),
            };
            let night_light_color = NightLightColor {
                share: night_light.clone(),
            };
            let conn = match zbus::blocking::connection::Builder::session()
                .and_then(|b| b.serve_at("/org/gnome/Mutter/ScreenCast", screencast))
                .and_then(|b| b.serve_at("/org/gnome/Mutter/DisplayConfig", display_config))
                .and_then(|b| b.serve_at("/org/gnome/Mutter/RemoteDesktop", remote_desktop))
                .and_then(|b| b.serve_at("/org/gnome/Mutter/ServiceChannel", service_channel))
                .and_then(|b| b.serve_at("/org/gnome/SettingsDaemon/Color", night_light_color))
                .and_then(|b| {
                    b.serve_at(
                        "/org/gnome/Mutter/ColorManager",
                        crate::color_manager::MutterColorManager {
                            share: color.clone(),
                        },
                    )
                })
                .and_then(|b| b.build())
            {
                Ok(conn) => conn,
                Err(e) => {
                    eprintln!("tuna-compositor: mutter D-Bus: no session bus: {e}");
                    return;
                }
            };
            let flags = zbus::fdo::RequestNameFlags::DoNotQueue.into();
            // GNOME Settings' calibration (P-DS-03): independent of the
            // portal names below.
            match conn.request_name_with_flags("org.gnome.Mutter.ColorManager", flags) {
                Ok(zbus::fdo::RequestNameReply::PrimaryOwner) => {
                    eprintln!("tuna-compositor: serving org.gnome.Mutter.ColorManager");
                }
                _ => eprintln!(
                    "tuna-compositor: org.gnome.Mutter.ColorManager is taken; calibration off"
                ),
            }
            for name in [
                "org.gnome.Mutter.DisplayConfig",
                "org.gnome.Mutter.ScreenCast",
                "org.gnome.Mutter.RemoteDesktop",
                "org.gnome.Mutter.ServiceChannel",
            ] {
                match conn.request_name_with_flags(name, flags) {
                    Ok(zbus::fdo::RequestNameReply::PrimaryOwner) => {
                        eprintln!("tuna-compositor: serving {name}");
                    }
                    _ => {
                        eprintln!("tuna-compositor: {name} is taken; screen sharing off");
                        return;
                    }
                }
            }
            // Every published configuration change, as Mutter emits
            // MonitorsChanged from its monitor manager.
            spawn_monitors_changed(conn.inner().clone(), display_state.subscribe_changes());
            // Night Light properties (#339): independent of the
            // portal names above. When a real settings daemon owns
            // this name its temp-disable wins and ours stays
            // local-only; the tint still follows the settings keys.
            let color_owned =
                match conn.request_name_with_flags("org.gnome.SettingsDaemon.Color", flags) {
                    Ok(zbus::fdo::RequestNameReply::PrimaryOwner) => {
                        eprintln!("tuna-compositor: serving org.gnome.SettingsDaemon.Color");
                        true
                    }
                    _ => {
                        eprintln!(
                            "tuna-compositor: org.gnome.SettingsDaemon.Color is taken; \
                         night light D-Bus properties off"
                        );
                        false
                    }
                };
            if color_owned {
                spawn_color_signals(conn.inner().clone(), night_light.subscribe_changes());
            }
            // Revoke on lock, unique-name disconnect or loss of the trusted
            // service identity. 100ms admission scan plus one compositor tick
            // bounds stream teardown; frames additionally fail closed on lock.
            // A bus that stops answering revokes within OWNER_CHECK_TIMEOUT.
            loop {
                std::thread::sleep(std::time::Duration::from_millis(100));
                let remote_snapshot: Vec<_> = remote
                    .lock()
                    .map(|s| s.clone())
                    .unwrap_or_default()
                    .into_iter()
                    .collect();
                let owners: Vec<String> = remote_snapshot
                    .iter()
                    .map(|(_, grant)| grant.owner.clone())
                    .collect();
                let alive = owners_alive(&authority, conn.inner(), &owners, OWNER_CHECK_TIMEOUT);
                for ((remote_id, grant), alive) in remote_snapshot.into_iter().zip(alive) {
                    if alive && !grant.stopped.load(Ordering::SeqCst) {
                        continue;
                    }
                    grant.stopped.store(true, Ordering::SeqCst);
                    let _ = to_loop.send(ToLoop::RemoteStop {
                        session_id: grant.id,
                    });
                    let path = format!("/org/gnome/Mutter/RemoteDesktop/Session/u{}", grant.id);
                    if let Ok(signal) = SignalEmitter::new(conn.inner(), path.clone()) {
                        signals.emit(async move {
                            remote_desktop::RemoteSession::closed(&signal).await
                        });
                    }
                    let _ = conn
                        .object_server()
                        .remove::<remote_desktop::RemoteSession, _>(path.as_str());
                    if let Ok(mut remote) = remote.lock() {
                        remote.remove(&remote_id);
                    }
                }
                let snapshot = sessions.lock().map(|s| s.clone()).unwrap_or_default();
                let owners: Vec<String> = snapshot
                    .iter()
                    .map(|(session, _)| session.owner.clone())
                    .collect();
                let alive = owners_alive(&authority, conn.inner(), &owners, OWNER_CHECK_TIMEOUT);
                for ((session, path), alive) in snapshot.into_iter().zip(alive) {
                    if alive && !session.stopped.load(Ordering::SeqCst) {
                        continue;
                    }
                    session.stopped.store(true, Ordering::SeqCst);
                    let _ = to_loop.send(ToLoop::StopCast {
                        session_id: session.id,
                    });
                    if !session.closed.swap(true, Ordering::SeqCst) {
                        if let Ok(signal) = SignalEmitter::new(conn.inner(), path.clone()) {
                            signals.emit(async move { Session::closed(&signal).await });
                        }
                    }
                    let streams = session
                        .streams
                        .lock()
                        .map(|mut s| std::mem::take(&mut *s))
                        .unwrap_or_default();
                    for (_, iface) in streams {
                        let _ = conn
                            .object_server()
                            .remove::<Stream, _>(iface.signal_emitter().path());
                    }
                    let _ = conn.object_server().remove::<Session, _>(&path);
                    if let Ok(mut sessions) = sessions.lock() {
                        sessions.retain(|(s, _)| s.id != session.id);
                    }
                }
                // Mutter inhibits orientation tracking while the display
                // power-save mode is not on (#355).
                if let Some(sample) = sensor.poll(orientation.is_inhibited() || !power_save.is_on())
                {
                    let _ = to_loop.send(ToLoop::SensorSample {
                        present: sample.present,
                        orientation: sample.orientation,
                    });
                }
            }
        });
    from_dbus
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bus daemon's half of the owner check, answering every PID
    /// query with one process.
    struct AnsweringBus(u32);

    #[zbus::interface(name = "org.freedesktop.DBus")]
    impl AnsweringBus {
        #[zbus(name = "GetConnectionUnixProcessID")]
        fn get_connection_unix_process_id(&self, _name: String) -> u32 {
            self.0
        }
    }

    /// #657: a bus that stops answering revokes every capture owner
    /// within one bound, checked concurrently rather than one blocked
    /// call after another; an answering bus still vouches for the shell.
    #[test]
    fn owner_checks_are_bounded_and_fail_closed_on_a_wedged_bus() {
        let authority = crate::capture_security::Authority::default();
        authority.publish(false, Some(4242));
        let owners: Vec<String> = [":1.7", ":1.8", ":1.9"].map(String::from).to_vec();
        let timeout = std::time::Duration::from_millis(400);

        let (wedged, _peer) = crate::dbus_signals::wedged_link();
        let started = std::time::Instant::now();
        let alive = owners_alive(&authority, wedged.inner(), &owners, timeout);
        let elapsed = started.elapsed();
        assert_eq!(
            alive,
            [false, false, false],
            "unanswered owners are revoked"
        );
        assert!(
            elapsed >= timeout,
            "the checks waited on the bus: {elapsed:?}"
        );
        assert!(
            elapsed < timeout * 2,
            "three wedged checks share one bound: {elapsed:?}"
        );

        let (server_end, client_end) = std::os::unix::net::UnixStream::pair().unwrap();
        let bus = std::thread::spawn(move || {
            zbus::blocking::connection::Builder::async_io_unix_stream(server_end)
                .server(zbus::Guid::generate())
                .unwrap()
                .p2p()
                .serve_at("/org/freedesktop/DBus", AnsweringBus(4242))
                .unwrap()
                .build()
                .unwrap()
        });
        let caller = zbus::blocking::connection::Builder::async_io_unix_stream(client_end)
            .p2p()
            .build()
            .unwrap();
        let _bus = bus.join().unwrap();
        let alive = owners_alive(
            &authority,
            caller.inner(),
            &owners,
            std::time::Duration::from_secs(10),
        );
        assert_eq!(
            alive,
            [true, true, true],
            "the shell's own sessions survive"
        );
        authority.publish(true, Some(4242));
        let alive = owners_alive(
            &authority,
            caller.inner(),
            &owners,
            std::time::Duration::from_secs(10),
        );
        assert_eq!(alive, [false, false, false], "a locked session revokes");
    }

    #[test]
    fn color_properties_round_trip_through_the_share() {
        use crate::night_light::NightLightShare;
        let share = Arc::new(NightLightShare::default());
        let color = NightLightColor {
            share: share.clone(),
        };
        assert!(!color.night_light_active());
        assert!(!color.disabled_until_tomorrow());
        // The loop publishes what it applies; the bus side reads it.
        share.publish(true, 2700);
        assert!(color.night_light_active());
        // Setting the property stamps now; clearing drops the stamp.
        color.set_disabled_until_tomorrow(true);
        assert!(color.disabled_until_tomorrow());
        let set_at = share.disabled_until_tmw().unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        assert!((now - set_at).abs() < 60, "set_at={set_at} now={now}");
        color.set_disabled_until_tomorrow(false);
        assert!(!color.disabled_until_tomorrow());
        assert_eq!(share.disabled_until_tmw(), None);
    }

    #[test]
    fn color_temperature_and_sun_properties_follow_gsd() {
        use crate::night_light::NightLightShare;
        let share = Arc::new(NightLightShare::default());
        let color = NightLightColor {
            share: share.clone(),
        };
        // g-s-d: 6500 K neutral, -1 sunrise/sunset before a location.
        assert_eq!(color.temperature(), 6500);
        assert_eq!((color.sunrise(), color.sunset()), (-1.0, -1.0));
        // Out-of-range writes fail as InvalidArgs with g-s-d's text.
        assert_eq!(
            color.set_temperature(999),
            Err(fdo::Error::InvalidArgs("999K is < min 1000K".into()))
        );
        assert_eq!(
            color.set_temperature(10_001),
            Err(fdo::Error::InvalidArgs("10001K is > max 10000K".into()))
        );
        assert_eq!(color.temperature(), 6500);
        assert_eq!(color.set_temperature(4000), Ok(()));
        assert_eq!(color.temperature(), 4000);
    }

    fn snapshot(connector: &str) -> OutputSnapshot {
        OutputSnapshot {
            connector: connector.into(),
            make: String::new(),
            model: String::new(),
            width: 1920,
            height: 1080,
            refresh_mhz: 60_000,
            x: 0,
            y: 0,
            scale: 1.0,
            primary: true,
            transform: 0,
        }
    }

    /// One monitor as GNOME Settings sends it (`color-mode` and
    /// `underscanning` always present).
    fn logical(connector: &str, scale: f64) -> LogicalMonitorConfiguration {
        LogicalMonitorConfiguration {
            x: 0,
            y: 0,
            scale,
            transform: 0,
            is_primary: true,
            monitors: vec![(
                connector.into(),
                "1920x1080@60.000".into(),
                HashMap::from([
                    ("color-mode".to_owned(), OwnedValue::from(0u32)),
                    ("underscanning".to_owned(), OwnedValue::from(false)),
                ]),
            )],
        }
    }

    fn spec(connector: &str) -> crate::monitors::MonitorSpec {
        crate::monitors::MonitorSpec {
            connector: connector.into(),
            vendor: "Tuna Desktop".into(),
            product: connector.into(),
            serial: connector.into(),
        }
    }

    fn mode(width: i32, height: i32, refresh: f32) -> crate::display_config::ModeSpec {
        crate::display_config::ModeSpec {
            width,
            height,
            refresh,
            interlaced: false,
        }
    }

    /// A panel (1080p at 60 and 59.94 Hz, 720p, 640x480) and an
    /// external monitor, side by side.
    fn display_state() -> crate::display_config::DisplayState {
        use crate::display_config::{DisplayState, LogicalState, MonitorState};
        let monitor = |connector: &str, modes, builtin| MonitorState {
            spec: spec(connector),
            display_name: if builtin {
                "Built-in display".into()
            } else {
                connector.into()
            },
            builtin,
            width_mm: 344,
            height_mm: 194,
            modes,
            preferred: 0,
            current: Some(0),
            privacy_screen: None,
        };
        DisplayState {
            serial: 3,
            monitors: vec![
                monitor(
                    "eDP-1",
                    vec![
                        mode(1920, 1080, 60.0),
                        mode(1920, 1080, 59.94),
                        mode(1280, 720, 60.0),
                        mode(640, 480, 59.94),
                    ],
                    true,
                ),
                monitor("HDMI-A-1", vec![mode(1920, 1080, 60.0)], false),
            ],
            logical: vec![
                LogicalState {
                    x: 0,
                    y: 0,
                    scale: 1.5,
                    transform: 0,
                    primary: true,
                    monitors: vec!["eDP-1".into()],
                },
                LogicalState {
                    x: 1280,
                    y: 0,
                    scale: 1.0,
                    transform: 0,
                    primary: false,
                    monitors: vec!["HDMI-A-1".into()],
                },
            ],
        }
    }

    /// What `ApplyMonitorsConfig` decides before anything changes.
    fn check(
        state: &crate::display_config::DisplayState,
        requests: &[LogicalMonitorConfiguration],
    ) -> fdo::Result<crate::monitors::MonitorsConfig> {
        let requests = requests
            .iter()
            .map(LogicalMonitorConfiguration::request)
            .collect::<fdo::Result<Vec<_>>>()?;
        crate::display_config::verify(state, state.serial, None, &requests).map_err(apply_error)
    }

    #[test]
    fn display_settings_apply_requested_modes_and_disabled_outputs() {
        let state = display_state();
        let valid = || {
            let mut external = logical("HDMI-A-1", 1.0);
            external.x = 1280;
            external.is_primary = false;
            vec![logical("eDP-1", 1.5), external]
        };
        let config = check(&state, &valid()).unwrap();
        assert_eq!(config.logical.len(), 2);
        assert!(config.disabled.is_empty());
        // Modes nobody advertised are refused, never ignored.
        for id in ["", "1920x1080@50.000", "2560x1440@60.000"] {
            let mut requested = valid();
            requested[0].monitors[0].1 = id.into();
            assert_eq!(
                check(&state, &requested),
                Err(fdo::Error::InvalidArgs(format!(
                    "Invalid mode '{id}' specified"
                ))),
            );
        }
        // An advertised fractional refresh rate and another resolution
        // are applied as requested.
        let mut requested = valid();
        requested[0].monitors[0].1 = "1920x1080@59.940".into();
        let config = check(&state, &requested).unwrap();
        assert_eq!(
            config.logical[0].monitors[0].mode,
            Some(mode(1920, 1080, 59.94))
        );
        let mut requested = valid();
        requested[0].scale = 1.0;
        requested[0].monitors[0].1 = "1280x720@60.000".into();
        let config = check(&state, &requested).unwrap();
        assert_eq!(
            config.logical[0].monitors[0].mode,
            Some(mode(1280, 720, 60.0))
        );
        // Omitting an output disables it.
        let config = check(&state, &valid()[..1]).unwrap();
        assert_eq!(config.disabled, vec![spec("HDMI-A-1")]);
        let mut duplicate = valid();
        duplicate.push(logical("eDP-1", 2.0));
        assert!(check(&state, &duplicate).is_err(), "duplicate output");
    }

    #[test]
    fn display_settings_requests_are_checked() {
        let state = display_state();
        let ok = check(&state, &[logical("eDP-1", 1.5)]).unwrap();
        let (laptop, monitor) = ok.find("eDP-1").unwrap();
        assert_eq!(
            (monitor.spec.connector.as_str(), laptop.scale),
            ("eDP-1", 1.5)
        );
        assert_eq!(
            check(&state, &[logical("DP-9", 1.0)]),
            Err(fdo::Error::InvalidArgs(
                "Invalid connector 'DP-9' specified".into()
            ))
        );
        assert_eq!(
            check(&state, &[logical("eDP-1", 0.5)]),
            Err(fdo::Error::InvalidArgs(
                "Scale 0.5 not valid for resolution 1920x1080".into()
            ))
        );
        assert_eq!(
            check(&state, &[]),
            Err(fdo::Error::InvalidArgs("Monitors config incomplete".into()))
        );
        // Mirroring needs a mode both monitors have.
        let mirrored = |first: &str| {
            let mut config = logical("eDP-1", 1.0);
            config.monitors[0].1 = first.into();
            config
                .monitors
                .push(("HDMI-A-1".into(), "1920x1080@60.000".into(), HashMap::new()));
            config
        };
        let config = check(&state, &[mirrored("1920x1080@60.000")]).unwrap();
        assert_eq!(config.logical[0].monitors.len(), 2);
        assert_eq!(
            check(&state, &[mirrored("1280x720@60.000")]),
            Err(fdo::Error::InvalidArgs(
                "Monitors modes in logical monitor not equal".into()
            ))
        );
        // Ill-typed properties and unsupported features are refused.
        let mut typed = logical("eDP-1", 1.0);
        typed.monitors[0].2.insert(
            "color-mode".into(),
            OwnedValue::try_from(Value::from("bt2100")).unwrap(),
        );
        assert_eq!(
            check(&state, &[typed]),
            Err(fdo::Error::InvalidArgs("color-mode must be u".into()))
        );
        let mut underscan = logical("eDP-1", 1.0);
        underscan.monitors[0]
            .2
            .insert("underscanning".into(), OwnedValue::from(true));
        assert_eq!(
            check(&state, &[underscan]),
            Err(fdo::Error::InvalidArgs(
                "Underscanning requested but unsupported".into()
            ))
        );
        // A stale serial is AccessDenied, as Mutter answers it.
        let requests = [logical("eDP-1", 1.0).request().unwrap()];
        assert_eq!(
            crate::display_config::verify(&state, 2, None, &requests).map_err(apply_error),
            Err(fdo::Error::AccessDenied(
                "The requested configuration is based on stale information".into()
            ))
        );
    }

    #[test]
    fn display_config_signatures_match_mutter_xml() {
        // org.gnome.Mutter.DisplayConfig.xml (mutter 51.0).
        assert_eq!(
            <Vec<Monitor> as Type>::SIGNATURE.to_string(),
            "a((ssss)a(siiddada{sv})a{sv})"
        );
        assert_eq!(
            <Vec<LogicalMonitor> as Type>::SIGNATURE.to_string(),
            "a(iiduba(ssss)a{sv})"
        );
        assert_eq!(
            <Vec<LogicalMonitorConfiguration> as Type>::SIGNATURE.to_string(),
            "a(iiduba(ssa{sv}))"
        );
    }

    #[test]
    fn current_state_lists_every_mode_like_mutter() {
        let mut state = display_state();
        state.monitors[0].current = Some(1);
        let (serial, monitors, logical, properties) = current_state(&state);
        assert_eq!(serial, 3);
        assert_eq!(monitors.len(), 2);
        let panel = &monitors[0];
        assert_eq!(
            panel.names,
            (
                "eDP-1".into(),
                "Tuna Desktop".into(),
                "eDP-1".into(),
                "eDP-1".into()
            )
        );
        // 640x480 leaves less than 600x600 and is not the preferred
        // size, so Mutter does not advertise it.
        let ids: Vec<&str> = panel.modes.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            ["1920x1080@60.000", "1920x1080@59.940", "1280x720@60.000"]
        );
        fn flags(m: &Mode) -> Vec<&str> {
            let mut keys: Vec<&str> = m.properties.keys().map(String::as_str).collect();
            keys.sort_unstable();
            keys
        }
        assert_eq!(flags(&panel.modes[0]), ["is-preferred"]);
        assert_eq!(flags(&panel.modes[1]), ["is-current"]);
        assert!(flags(&panel.modes[2]).is_empty());
        assert!((panel.modes[1].refresh_rate - 59.94).abs() < 1e-4);
        assert_eq!(
            panel.modes[0].supported_scales,
            [
                1.0,
                1.25,
                f64::from(4.0f32 / 3.0),
                1.5,
                f64::from(5.0f32 / 3.0),
                2.0
            ]
        );
        // The current mode prefers the scale in use; others follow DPI
        // (a 15.6" 1080p panel: ~141 DPI, 1).
        assert_eq!(panel.modes[1].preferred_scale, 1.5);
        assert_eq!(panel.modes[0].preferred_scale, 1.0);
        assert!(bool::try_from(&panel.properties["is-builtin"]).unwrap());
        assert_eq!(
            <&str>::try_from(&panel.properties["display-name"]).unwrap(),
            "Built-in display"
        );
        assert_eq!(u32::try_from(&panel.properties["color-mode"]).unwrap(), 0);
        // No privacy screen: no `privacy-screen-state` (GNOME Settings
        // hides its row).
        assert!(!panel.properties.contains_key("privacy-screen-state"));
        assert_eq!(logical.len(), 2);
        assert_eq!((logical[0].scale, logical[0].is_primary), (1.5, true));
        assert_eq!(logical[1].monitors[0].0, "HDMI-A-1");
        assert_eq!(u32::try_from(&properties["layout-mode"]).unwrap(), 1);
        // A disabled monitor stays listed with no current mode.
        state.monitors[1].current = None;
        state.logical.truncate(1);
        let (_, monitors, logical, _) = current_state(&state);
        assert!(monitors[1]
            .modes
            .iter()
            .all(|m| !m.properties.contains_key("is-current")));
        assert_eq!(logical.len(), 1);
    }

    #[test]
    fn current_state_reports_privacy_screens_as_mutter_does() {
        use crate::privacy_screen::ScreenState;
        let mut state = display_state();
        state.monitors[0].privacy_screen = Some(ScreenState {
            enabled: true,
            locked: false,
        });
        state.monitors[1].privacy_screen = Some(ScreenState {
            enabled: false,
            locked: true,
        });
        let (_, monitors, _, _) = current_state(&state);
        // gnome-control-center 51 `cc-display-config.c` reads `(bb)`.
        let read = |m: &Monitor| {
            let value = &m.properties["privacy-screen-state"];
            assert_eq!(value.value_signature().to_string(), "(bb)");
            let Value::Structure(fields) = &**value else {
                panic!("privacy-screen-state is not a structure");
            };
            let [Value::Bool(enabled), Value::Bool(locked)] = fields.fields() else {
                panic!("privacy-screen-state is not (bb)");
            };
            (*enabled, *locked)
        };
        assert_eq!(read(&monitors[0]), (true, false));
        assert_eq!(read(&monitors[1]), (false, true));
    }

    #[test]
    fn record_areas_land_on_their_monitor() {
        let mut right = snapshot("HDMI-A-1");
        right.x = 1920;
        right.scale = 2.0;
        let outputs = [snapshot("eDP-1"), right];
        let (output, area) = area_on_output(&outputs, (480, 300, 320, 200)).unwrap();
        assert_eq!(
            (output.connector.as_str(), area),
            ("eDP-1", (480, 300, 320, 200))
        );
        // The second monitor at scale 2 (960x540 logical): physical pixels.
        let (output, area) = area_on_output(&outputs, (1920 + 10, 20, 100, 50)).unwrap();
        assert_eq!(
            (output.connector.as_str(), area),
            ("HDMI-A-1", (20, 40, 200, 100))
        );
        // Clipped to the monitor it starts on.
        let (_, area) = area_on_output(&outputs, (1800, 1000, 400, 400)).unwrap();
        assert_eq!(area, (1800, 1000, 120, 80));
        assert!(area_on_output(&outputs, (-50, 0, 10, 10)).is_none());
        assert!(area_on_output(&outputs, (0, 0, 0, 10)).is_none());
    }

    #[test]
    fn builtin_panels_follow_mutter() {
        use crate::orientation::is_builtin_panel;
        assert!(is_builtin_panel("eDP-1"));
        assert!(is_builtin_panel("LVDS-1"));
        assert!(!is_builtin_panel("HDMI-A-1"));
        assert!(!is_builtin_panel("tuna-0"));
    }

    #[test]
    fn display_rotation_validates_mutter_transforms() {
        let state = display_state();
        for transform in 0..=7u32 {
            let mut request = logical("eDP-1", 1.0);
            request.transform = transform;
            let ok = check(&state, &[request]).unwrap();
            assert_eq!(ok.logical[0].transform, transform);
        }
        let mut request = logical("eDP-1", 1.0);
        request.transform = 8;
        assert!(check(&state, &[request]).is_err(), "transform range");
    }

    #[test]
    fn record_areas_hit_test_rotated_monitors() {
        // A 90° panel shows a 1080x1920 logical frame.
        let mut portrait = snapshot("eDP-1");
        portrait.transform = 1;
        let outputs = [portrait];
        assert!(area_on_output(&outputs, (100, 100, 50, 50)).is_some());
        // Past the swapped width (an unrotated 1920-wide frame would hit).
        assert!(area_on_output(&outputs, (1500, 100, 50, 50)).is_none());
        // Past the unrotated height but inside the rotated one; the area
        // still fits the physical remap below the fold.
        assert!(area_on_output(&outputs, (100, 1000, 50, 50)).is_some());
    }

    #[test]
    fn record_areas_remap_into_rotated_scanouts() {
        // A 90° 1920x1080 panel: logical 1080x1920, buffer 1920x1080.
        // Logical top-left lands at buffer top-right (rotated).
        let mut portrait = snapshot("eDP-1");
        portrait.transform = 1;
        let outputs = [portrait];
        let (_, area) = area_on_output(&outputs, (0, 0, 100, 50)).unwrap();
        assert_eq!(area, (1870, 0, 50, 100));
        // 180° lands at buffer bottom-right; the flip mirrors x only.
        let mut upside = snapshot("eDP-1");
        upside.transform = 2;
        let (_, area) = area_on_output(&[upside], (0, 0, 100, 50)).unwrap();
        assert_eq!(area, (1820, 1030, 100, 50));
        let mut flipped = snapshot("eDP-1");
        flipped.transform = 4;
        let (_, area) = area_on_output(&[flipped], (0, 0, 100, 50)).unwrap();
        assert_eq!(area, (1820, 0, 100, 50));
        // Rotation composes with scale: a 90° panel at scale 2 shows
        // 540x960 logical pixels.
        let mut small = snapshot("eDP-1");
        small.transform = 1;
        small.scale = 2.0;
        let (_, area) = area_on_output(&[small], (0, 0, 100, 50)).unwrap();
        assert_eq!(area, (1820, 0, 100, 200));
        // An area overhanging the logical edge clips to the buffer.
        let (_, area) = area_on_output(&outputs, (1000, 1900, 200, 100)).unwrap();
        assert_eq!(area, (0, 1000, 20, 80));
    }

    /// The real D-Bus surface gnome-settings-daemon drives (#355): Set
    /// reaches the loop, Get and PropertiesChanged carry the applied
    /// mode, and -1 is ignored as Mutter's `power_save_mode_changed` does.
    #[test]
    fn power_save_mode_round_trips_over_a_private_bus() {
        use crate::power_save::{PowerSave, PowerSaveShare, Transition};
        use std::io::{BufRead, BufReader};
        use std::process::{Child, Command, Stdio};
        use std::time::Duration;

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
        let connect = |builder: zbus::blocking::connection::Builder<'static>| {
            builder
                .method_timeout(Duration::from_secs(5))
                .build()
                .unwrap()
        };

        // The loop side: apply each write and publish what was applied.
        let (to_loop, from_dbus) = calloop::channel::channel();
        let share = Arc::new(PowerSaveShare::default());
        let transitions = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let event_loop_thread = std::thread::spawn({
            let share = share.clone();
            let transitions = transitions.clone();
            let stop = stop.clone();
            move || {
                let mut event_loop: calloop::EventLoop<PowerSave> =
                    calloop::EventLoop::try_new().unwrap();
                event_loop
                    .handle()
                    .insert_source(from_dbus, move |event, _, state: &mut PowerSave| {
                        if let calloop::channel::Event::Msg(ToLoop::PowerSave { mode, reply }) =
                            event
                        {
                            let transition = state.request(mode);
                            share.publish(state.mode());
                            transitions.lock().unwrap().push(transition);
                            reply.send(()).unwrap();
                        }
                    })
                    .unwrap();
                let mut state = PowerSave::default();
                while !stop.load(Ordering::SeqCst) {
                    event_loop
                        .dispatch(Some(Duration::from_millis(20)), &mut state)
                        .unwrap();
                }
            }
        });

        let display_config = DisplayConfig {
            display: Default::default(),
            to_loop,
            orientation: Default::default(),
            power_save: share.clone(),
        };
        let _server = connect(
            zbus::blocking::connection::Builder::address(address.trim())
                .unwrap()
                .serve_at("/org/gnome/Mutter/DisplayConfig", display_config)
                .unwrap()
                .name("org.gnome.Mutter.DisplayConfig")
                .unwrap(),
        );
        let client = connect(zbus::blocking::connection::Builder::address(address.trim()).unwrap());
        let properties = zbus::blocking::fdo::PropertiesProxy::builder(&client)
            .destination("org.gnome.Mutter.DisplayConfig")
            .unwrap()
            .path("/org/gnome/Mutter/DisplayConfig")
            .unwrap()
            .build()
            .unwrap();
        let interface =
            zbus::names::InterfaceName::from_static_str("org.gnome.Mutter.DisplayConfig").unwrap();

        // Mutter's introspection: a read-write int32.
        let xml: String = client
            .call_method(
                Some("org.gnome.Mutter.DisplayConfig"),
                "/org/gnome/Mutter/DisplayConfig",
                Some("org.freedesktop.DBus.Introspectable"),
                "Introspect",
                &(),
            )
            .unwrap()
            .body()
            .deserialize()
            .unwrap();
        assert!(
            xml.contains(r#"<property name="PowerSaveMode" type="i" access="readwrite"/>"#),
            "{xml}"
        );

        let (changed_tx, changed) = std::sync::mpsc::channel();
        let signals = properties.receive_properties_changed().unwrap();
        std::thread::spawn(move || {
            for signal in signals {
                let args = signal.args().unwrap();
                if let Some(value) = args.changed_properties().get("PowerSaveMode") {
                    let mode = i32::try_from(value).unwrap();
                    if changed_tx.send(mode).is_err() {
                        return;
                    }
                }
            }
        });
        let get =
            || i32::try_from(properties.get(interface.clone(), "PowerSaveMode").unwrap()).unwrap();
        let set = |mode: i32| {
            properties
                .set(interface.clone(), "PowerSaveMode", mode.into())
                .unwrap()
        };
        let signal = || changed.recv_timeout(Duration::from_secs(5)).unwrap();

        assert_eq!(get(), 0, "Mutter starts on");
        // gsd's disable_monitors.
        set(3);
        assert_eq!(get(), 3);
        assert_eq!(signal(), 3);
        // gsd's enable_monitors.
        set(0);
        assert_eq!(get(), 0);
        assert_eq!(signal(), 0);
        // Unknown never enters: the write changes nothing in the loop.
        set(-1);
        assert_eq!(get(), 0);
        assert_eq!(signal(), 0);
        assert_eq!(
            *transitions.lock().unwrap(),
            vec![
                Transition::PowerOff {
                    disable_outputs: true
                },
                Transition::PowerOn,
            ]
        );

        stop.store(true, Ordering::SeqCst);
        event_loop_thread.join().unwrap();
    }

    /// The bus surface GNOME Settings drives (#340): Mutter's argument
    /// types, verification that never reaches the loop, a stale serial
    /// refused as AccessDenied, and an apply the loop answers, announced
    /// with MonitorsChanged under a new serial.
    #[test]
    fn display_config_applies_and_announces_over_a_private_bus() {
        use crate::display_config::{DisplayShare, Method};
        use std::io::{BufRead, BufReader};
        use std::process::{Child, Command, Stdio};
        use std::time::Duration;

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
        let connect = |builder: zbus::blocking::connection::Builder<'static>| {
            builder
                .method_timeout(Duration::from_secs(5))
                .build()
                .unwrap()
        };
        const DEST: &str = "org.gnome.Mutter.DisplayConfig";
        const PATH: &str = "/org/gnome/Mutter/DisplayConfig";

        let share = Arc::new(DisplayShare::default());
        assert!(share.publish(display_state()));
        // The loop side: show the requested mode and publish it.
        let (to_loop, from_dbus) = calloop::channel::channel();
        let applied = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let event_loop_thread = std::thread::spawn({
            let share = share.clone();
            let applied = applied.clone();
            let stop = stop.clone();
            move || {
                let mut event_loop: calloop::EventLoop<()> = calloop::EventLoop::try_new().unwrap();
                event_loop
                    .handle()
                    .insert_source(from_dbus, move |event, _, _: &mut ()| {
                        if let calloop::channel::Event::Msg(ToLoop::ApplyMonitors {
                            config,
                            method,
                            reply,
                        }) = event
                        {
                            let mode = config.logical[0].monitors[0].mode.unwrap();
                            let mut state = share.snapshot();
                            state.monitors[0].current =
                                state.monitors[0].modes.iter().position(|m| m.equals(&mode));
                            share.publish(state);
                            applied.lock().unwrap().push((mode.id(), method));
                            reply.send(Ok(())).unwrap();
                        }
                    })
                    .unwrap();
                while !stop.load(Ordering::SeqCst) {
                    event_loop
                        .dispatch(Some(Duration::from_millis(20)), &mut ())
                        .unwrap();
                }
            }
        });

        let display_config = DisplayConfig {
            display: share.clone(),
            to_loop,
            orientation: Default::default(),
            power_save: Default::default(),
        };
        let server = connect(
            zbus::blocking::connection::Builder::address(address.trim())
                .unwrap()
                .serve_at(PATH, display_config)
                .unwrap()
                .name(DEST)
                .unwrap(),
        );
        spawn_monitors_changed(server.inner().clone(), share.subscribe_changes());
        let client = connect(zbus::blocking::connection::Builder::address(address.trim()).unwrap());

        let xml: String = client
            .call_method(
                Some(DEST),
                PATH,
                Some("org.freedesktop.DBus.Introspectable"),
                "Introspect",
                &(),
            )
            .unwrap()
            .body()
            .deserialize()
            .unwrap();
        for signature in [
            "a((ssss)a(siiddada{sv})a{sv})",
            "a(iiduba(ssss)a{sv})",
            "a(iiduba(ssa{sv}))",
            "<signal name=\"MonitorsChanged\"",
        ] {
            assert!(xml.contains(signature), "{signature} missing from {xml}");
        }

        type ModeOut = (
            String,
            i32,
            i32,
            f64,
            f64,
            Vec<f64>,
            HashMap<String, OwnedValue>,
        );
        type MonitorOut = (
            (String, String, String, String),
            Vec<ModeOut>,
            HashMap<String, OwnedValue>,
        );
        type LogicalOut = (
            i32,
            i32,
            f64,
            u32,
            bool,
            Vec<(String, String, String, String)>,
            HashMap<String, OwnedValue>,
        );
        type StateOut = (
            u32,
            Vec<MonitorOut>,
            Vec<LogicalOut>,
            HashMap<String, OwnedValue>,
        );
        let get_state = || -> StateOut {
            client
                .call_method(Some(DEST), PATH, Some(DEST), "GetCurrentState", &())
                .unwrap()
                .body()
                .deserialize()
                .unwrap()
        };
        let current = |state: &StateOut| -> Vec<String> {
            state.1[0]
                .1
                .iter()
                .filter(|m| m.6.contains_key("is-current"))
                .map(|m| m.0.clone())
                .collect()
        };
        let state = get_state();
        assert_eq!(state.0, 1);
        assert_eq!(state.1.len(), 2);
        assert_eq!(state.2.len(), 2);
        assert_eq!(current(&state), ["1920x1080@60.000"]);

        let signals = zbus::blocking::Proxy::new(&client, DEST, PATH, DEST)
            .unwrap()
            .receive_signal("MonitorsChanged")
            .unwrap();
        let (changed_tx, changed) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for _ in signals {
                if changed_tx.send(()).is_err() {
                    return;
                }
            }
        });
        let apply = |serial: u32, method: u32, mode_id: &str| {
            let monitors = vec![(
                "eDP-1".to_owned(),
                mode_id.to_owned(),
                HashMap::<String, OwnedValue>::new(),
            )];
            client.call_method(
                Some(DEST),
                PATH,
                Some(DEST),
                "ApplyMonitorsConfig",
                &(
                    serial,
                    method,
                    vec![(0i32, 0i32, 1.0f64, 0u32, true, monitors)],
                    HashMap::<String, OwnedValue>::new(),
                ),
            )
        };
        let error_name = |result: zbus::Result<zbus::message::Message>| match result {
            Err(zbus::Error::MethodError(name, _, _)) => name.to_string(),
            other => panic!("expected a D-Bus error, got {other:?}"),
        };
        assert_eq!(
            error_name(apply(0, 1, "1280x720@60.000")),
            "org.freedesktop.DBus.Error.AccessDenied"
        );
        assert_eq!(
            error_name(apply(1, 0, "640x400@60.000")),
            "org.freedesktop.DBus.Error.InvalidArgs"
        );
        // Verify answers without reaching the loop or changing anything.
        apply(1, 0, "1280x720@60.000").unwrap();
        assert!(applied.lock().unwrap().is_empty());
        assert!(changed.recv_timeout(Duration::from_millis(300)).is_err());
        // Temporary: applied by the loop before the call returns, then
        // announced; the serial moves on.
        apply(1, 1, "1280x720@60.000").unwrap();
        assert_eq!(
            *applied.lock().unwrap(),
            vec![("1280x720@60.000".to_owned(), Method::Temporary)]
        );
        changed.recv_timeout(Duration::from_secs(5)).unwrap();
        let state = get_state();
        assert_eq!(state.0, 2);
        assert_eq!(current(&state), ["1280x720@60.000"]);
        assert_eq!(
            error_name(apply(1, 2, "1920x1080@60.000")),
            "org.freedesktop.DBus.Error.AccessDenied"
        );
        apply(2, 2, "1920x1080@59.940").unwrap();
        assert_eq!(
            applied.lock().unwrap().last(),
            Some(&("1920x1080@59.940".to_owned(), Method::Persistent))
        );

        stop.store(true, Ordering::SeqCst);
        event_loop_thread.join().unwrap();
    }

    /// A running cast for [`crate::screencast::end_gone`] without
    /// PipeWire: its session's real `CastSignals`.
    struct FakeCast {
        session_id: u64,
        target: CastTarget,
        monitor: Option<OutputSnapshot>,
        signal: CastSignals,
    }

    impl crate::screencast::Running for FakeCast {
        fn session_id(&self) -> u64 {
            self.session_id
        }
        fn target(&self) -> &CastTarget {
            &self.target
        }
        fn monitor(&self) -> Option<&OutputSnapshot> {
            self.monitor.as_ref()
        }
        fn failed(&self) -> bool {
            false
        }
        fn close(&self) {
            self.signal.session_closed(self.session_id);
        }
    }

    /// Mutter's meta-stream-monitor.c `on_monitors_changed`: a monitor
    /// stream survives a mode switch that keeps its logical rect, and
    /// when its monitor is unplugged `meta_stream_stop` ends it and the
    /// session sends `Closed` (once) over a real peer-to-peer link.
    /// Window casts and area casts on the unplugged monitor stay.
    #[test]
    fn monitor_casts_stop_when_their_monitor_is_unplugged() {
        let (server_end, client_end) = std::os::unix::net::UnixStream::pair().unwrap();
        let server = std::thread::spawn(move || {
            zbus::blocking::connection::Builder::async_io_unix_stream(server_end)
                .server(zbus::Guid::generate())
                .unwrap()
                .p2p()
                .build()
                .unwrap()
        });
        let client = zbus::blocking::connection::Builder::async_io_unix_stream(client_end)
            .p2p()
            .build()
            .unwrap();
        let server = server.join().unwrap();
        let mut messages = zbus::blocking::MessageIterator::from(&client);
        let queue = crate::dbus_signals::SignalQueue::new("cast-test-signals");
        let signals = |stream: u64| CastSignals {
            stream: SignalEmitter::new(
                server.inner(),
                format!("/org/gnome/Mutter/ScreenCast/Stream/u{stream}"),
            )
            .unwrap(),
            queue: queue.clone(),
            closed: Arc::new(AtomicBool::new(false)),
        };

        let a = snapshot("eDP-1");
        let mut b = snapshot("DP-1");
        b.x = 1920;
        b.primary = false;
        let b_session = signals(1);
        let fake = |session_id, target, monitor, signal| FakeCast {
            session_id,
            target,
            monitor,
            signal,
        };
        let mut casts = vec![
            fake(
                1,
                CastTarget::Monitor("DP-1".into()),
                Some(b.clone()),
                b_session.clone(),
            ),
            fake(
                2,
                CastTarget::Monitor("eDP-1".into()),
                Some(a.clone()),
                signals(2),
            ),
            fake(
                3,
                CastTarget::Area("DP-1".into(), (0, 0, 64, 64)),
                None,
                signals(3),
            ),
            fake(4, CastTarget::Window(7), None, signals(4)),
        ];
        let grants: HashMap<u64, Arc<AtomicBool>> = (1..=4)
            .map(|id| (id, Arc::new(AtomicBool::new(false))))
            .collect();
        let ids = |casts: &[FakeCast]| casts.iter().map(|c| c.session_id).collect::<Vec<_>>();

        // A mode switch that keeps both logical rects: B at another
        // refresh, A at twice the pixels and scale 2.
        let mut b_refresh = b.clone();
        b_refresh.refresh_mhz = 30_000;
        let mut a_hidpi = a.clone();
        (a_hidpi.width, a_hidpi.height, a_hidpi.scale) = (3840, 2160, 2.0);
        let modes = [a_hidpi.clone(), b_refresh];
        crate::screencast::end_gone(&mut casts, &grants, &modes, |_| true);
        assert_eq!(ids(&casts), [1, 2, 3, 4], "a mode switch keeps every cast");
        assert_eq!(
            monitor_after_change(&a, &modes).map(|o| (o.width, o.height)),
            Some((3840, 2160)),
            "the kept stream follows the new mode"
        );
        // A rect change (B at scale 2 is half the logical size) is what
        // `update_monitor` refuses.
        let mut b_scaled = b.clone();
        b_scaled.scale = 2.0;
        assert!(monitor_after_change(&b, &[a.clone(), b_scaled]).is_none());

        // B is unplugged.
        let unplugged = [a_hidpi];
        crate::screencast::end_gone(&mut casts, &grants, &unplugged, |_| true);
        assert_eq!(ids(&casts), [2, 3, 4], "only B's monitor cast ends");
        assert!(grants[&1].load(Ordering::SeqCst), "its session is revoked");
        assert!((2..=4).all(|id| !grants[&id].load(Ordering::SeqCst)));
        // Closing the session again (Stop, revocation) sends nothing.
        b_session.session_closed(1);
        signals(9).stream_added(99);

        let mut seen: Vec<(String, String)> = Vec::new();
        while seen.last().map(|(member, _)| member.as_str()) != Some("PipeWireStreamAdded") {
            let message = messages.next().unwrap().unwrap();
            let header = message.header();
            seen.push((
                header.member().unwrap().to_string(),
                header.path().unwrap().to_string(),
            ));
        }
        assert_eq!(
            seen,
            [
                (
                    "Closed".to_string(),
                    "/org/gnome/Mutter/ScreenCast/Session/u1".to_string()
                ),
                (
                    "PipeWireStreamAdded".to_string(),
                    "/org/gnome/Mutter/ScreenCast/Stream/u9".to_string()
                ),
            ]
        );
    }
}
