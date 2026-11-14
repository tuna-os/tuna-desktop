//! GNOME's weather client for the date menu (gnome-shell 51.0
//! `js/misc/weather.js`, class `WeatherClient`, lines 25-327).
//!
//! Weather is available while GNOME Weather (`org.gnome.Weather.desktop`)
//! is installed. The forecast is for Weather's most recent location, or
//! for where GeoClue puts the machine when Weather's automatic location
//! is on, `org.gnome.system.location` `enabled` is set, and (for a
//! Flatpak Weather, or none) the permission store's `gnome` /
//! `geolocation` table grants `org.gnome.Weather` some accuracy. A
//! `GWeather.Info` with GNOME's providers (METAR, MET Norway,
//! OpenWeatherMap) fetches it; a refresh within ten minutes of the last
//! update runs without the "Loading…" state.
//!
//! Weather's two settings come, in GNOME's order of preference, from
//! `org.gnome.shell.weather` (gnome-shell's cache, rewritten from the
//! `AutomaticLocation` and `Locations` properties Weather exports as
//! `org.gnome.Shell.WeatherIntegration`), or, without gnome-shell's
//! schema, from those exported properties while Weather runs, else from
//! Weather's own `org.gnome.Weather` schema. The fallback is Tuna
//! Desktop's.
//!
//! Divergences from weather.js:
//!
//! - libgweather names its client to the weather services in the
//!   User-Agent (`libgweather/<version> <application id> (<contact>)`).
//!   weather.js sends `org.gnome.Shell` and gnome-shell's DOAP file;
//!   this shell sends its own application id and project address, so a
//!   service operator reaches the right project.
//! - The GeoClue client is `Geoclue.Simple`'s D-Bus calls made directly
//!   (`GetClient`, city accuracy, a 100 m distance threshold, `Start`,
//!   `LocationUpdated`); the libgeoclue client library is not linked.
//!   It asks as `org.gnome.Shell`, as weather.js does: GeoClue's stock
//!   configuration lists that id as a system component, which is what
//!   lets the shell read a fix without its own agent prompting for it.
//! - weather.js compares a new location with `GWeather.Info`'s current
//!   one, which starts out as libgweather's default station; this
//!   client compares with the location it last set.
//! - A permission row that names no accuracy denies. In weather.js
//!   `undefined !== 'NONE'` makes such a row grant.
//! - Without the `org.gnome.GWeather4` schema or the locations database
//!   (both ship with libgweather) no `GWeather.Info` is made and the
//!   section stays at "Select Weather Location…".
//!
//! `TUNA_WEATHER_FIXTURE` names a JSON file that stands in for the
//! location and forecast, so the GTK shell proof shows every state
//! without a network (see [`fixture_snapshot`]). The Weather app still
//! has to be installed for the section to show.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use gio::prelude::*;

use crate::app_integration::{self, InstalledApp};
use crate::live_apps::LiveApps;
use crate::location::{ACC_CITY, ENABLED_KEY, LOCATION_SCHEMA};

pub const WEATHER_APP_ID: &str = "org.gnome.Weather.desktop";

const WEATHER_BUS_NAME: &str = "org.gnome.Weather";
const WEATHER_OBJECT_PATH: &str = "/org/gnome/Weather";
const WEATHER_INTEGRATION_IFACE: &str = "org.gnome.Shell.WeatherIntegration";

const SHELL_SCHEMA: &str = "org.gnome.shell.weather";
const WEATHER_SCHEMA: &str = "org.gnome.Weather";
const AUTOMATIC_LOCATION: &str = "automatic-location";
const LOCATIONS: &str = "locations";
/// libgweather's own settings; `GWeather.Info` aborts without them.
const GWEATHER_SCHEMA: &str = "org.gnome.GWeather4";

/// How libgweather names this client to the weather services. These
/// are Tuna Desktop's own, where weather.js has `org.gnome.Shell` and
/// gnome-shell's DOAP address.
const APPLICATION_ID: &str = "org.tuna.Shell";
const CONTACT_INFO: &str = "https://github.com/tuna-os/tuna-desktop";

/// Minimum time between updates to show loading indication, in
/// microseconds (`UPDATE_THRESHOLD`).
const UPDATE_THRESHOLD: i64 = 10 * 60 * 1_000_000;

const PERM_STORE: &str = "org.freedesktop.impl.portal.PermissionStore";
const PERM_PATH: &str = "/org/freedesktop/impl/portal/PermissionStore";
const PERM_TABLE: &str = "gnome";
const PERM_ID: &str = "geolocation";
const WEATHER_PERMISSION: &str = "org.gnome.Weather";

const GEOCLUE: &str = "org.freedesktop.GeoClue2";
const GEOCLUE_MANAGER_PATH: &str = "/org/freedesktop/GeoClue2/Manager";
const GEOCLUE_MANAGER_IFACE: &str = "org.freedesktop.GeoClue2.Manager";
const GEOCLUE_CLIENT_IFACE: &str = "org.freedesktop.GeoClue2.Client";
const GEOCLUE_LOCATION_IFACE: &str = "org.freedesktop.GeoClue2.Location";
const GEOCLUE_DESKTOP_ID: &str = "org.gnome.Shell";
/// `client.distance_threshold`, in metres.
const GEOCLUE_DISTANCE_THRESHOLD: u32 = 100;
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";

/// The fixture file the GTK shell proof supplies.
const FIXTURE_ENV: &str = "TUNA_WEATHER_FIXTURE";

/// One entry of `GWeather.Info.get_forecast_list`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Forecast {
    /// `get_value_update`: the time it is for (Unix seconds), `None`
    /// when the entry is not valid.
    pub update: Option<i64>,
    /// `get_symbolic_icon_name`.
    pub icon: Option<String>,
    /// `get_value_temp` in the user's unit.
    pub temp: Option<f64>,
}

/// What the Weather section draws from: the client's getters
/// (`available`, `hasLocation`, `loading`) and the parts of its `info`
/// the section reads.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Snapshot {
    pub available: bool,
    pub has_location: bool,
    pub loading: bool,
    /// `info.is_valid()`.
    pub valid: bool,
    /// `info.network_error()`.
    pub network_error: bool,
    /// `info.location.get_name()`.
    pub location_name: String,
    /// The nearest city's name, looked up only for a location that is
    /// not itself a city and has coordinates.
    pub nearest_city: Option<String>,
    pub forecasts: Vec<Forecast>,
}

/// `_useAutoLocation`.
pub fn use_auto_location(
    requested: bool,
    location_enabled: bool,
    needs_auth: bool,
    authorized: bool,
) -> bool {
    requested && location_enabled && (!needs_auth || authorized)
}

/// `_needsAuth`: only a Weather installed outside Flatpak is trusted
/// with the location without a permission-store grant.
pub fn needs_auth(installed: bool, flatpak: bool) -> bool {
    !installed || flatpak
}

/// Whether the `gnome` / `geolocation` table grants Weather some
/// accuracy (`_onPermStoreChanged`).
pub fn weather_authorized(permissions: &HashMap<String, Vec<String>>) -> bool {
    permissions
        .get(WEATHER_PERMISSION)
        .and_then(|row| row.first())
        .is_some_and(|accuracy| accuracy != "NONE")
}

/// The permissions a store `Changed` signal (`(ssbva{sas})`) carries,
/// when it is for the `gnome` / `geolocation` table.
pub fn changed_permissions(params: &glib::Variant) -> Option<HashMap<String, Vec<String>>> {
    if params.type_().as_str() != "(ssbva{sas})" {
        return None;
    }
    let table = params.child_value(0).get::<String>()?;
    let id = params.child_value(1).get::<String>()?;
    if table != PERM_TABLE || id != PERM_ID {
        return None;
    }
    params.child_value(4).get()
}

/// The permissions in a store `Lookup` reply (`(a{sas}v)`); anything
/// else reads as none, as weather.js goes on with `{}` after an error.
pub fn looked_up_permissions(reply: &glib::Variant) -> HashMap<String, Vec<String>> {
    if reply.type_().as_str() != "(a{sas}v)" {
        return HashMap::new();
    }
    reply.child_value(0).get().unwrap_or_default()
}

/// `update`'s choice: refresh without the loading state while the
/// current forecast is valid and younger than [`UPDATE_THRESHOLD`]
/// (times in microseconds).
pub fn refreshes_quietly(valid: bool, now: i64, last_update: i64) -> bool {
    valid && now - last_update < UPDATE_THRESHOLD
}

/// A GeoClue `Location` object's coordinates in degrees, from its
/// `GetAll` reply's `a{sv}`.
pub fn geoclue_fix(properties: &glib::Variant) -> Option<(f64, f64)> {
    let latitude = app_integration::exported_property(properties, "Latitude", "d")?;
    let longitude = app_integration::exported_property(properties, "Longitude", "d")?;
    Some((latitude.get()?, longitude.get()?))
}

/// The snapshot a fixture file describes at `now` (Unix seconds):
///
/// ```json
/// {"state": "forecast", "location": "Lisbon",
///  "forecasts": [{"in_minutes": 30, "icon": "weather-clear-symbolic", "temp": 21.4}]}
/// ```
///
/// `state` is `none` (no location), `loading`, `forecast`, `offline`
/// (a network error) or `unavailable`. A forecast is for `now` plus
/// `in_minutes`; one without `in_minutes` is not valid. `available` is
/// left for the caller, which knows whether Weather is installed.
/// `None` for anything else.
pub fn fixture_snapshot(json: &str, now: i64) -> Option<Snapshot> {
    let fixture: serde_json::Value = serde_json::from_str(json).ok()?;
    let mut snapshot = Snapshot::default();
    match fixture.get("state")?.as_str()? {
        "none" => return Some(snapshot),
        "loading" => snapshot.loading = true,
        "forecast" => snapshot.valid = true,
        "offline" => snapshot.network_error = true,
        "unavailable" => {}
        _ => return None,
    }
    snapshot.has_location = true;
    snapshot.location_name = fixture
        .get("location")
        .and_then(|name| name.as_str())
        .unwrap_or_default()
        .to_owned();
    if snapshot.valid {
        let forecasts = fixture.get("forecasts").and_then(|list| list.as_array());
        snapshot.forecasts = forecasts
            .into_iter()
            .flatten()
            .map(|forecast| Forecast {
                update: forecast
                    .get("in_minutes")
                    .and_then(|minutes| minutes.as_i64())
                    .map(|minutes| now + minutes * 60),
                icon: forecast
                    .get("icon")
                    .and_then(|icon| icon.as_str())
                    .map(str::to_owned),
                temp: forecast.get("temp").and_then(|temp| temp.as_f64()),
            })
            .collect();
    }
    Some(snapshot)
}

/// `_locationsEqual`.
fn locations_equal(
    one: Option<&libgweather::Location>,
    two: Option<&libgweather::Location>,
) -> bool {
    match (one, two) {
        (None, None) => true,
        (Some(one), Some(two)) => one.equal(two),
        _ => false,
    }
}

fn now_seconds() -> i64 {
    glib::real_time() / 1_000_000
}

/// `get_value_update`: the time a forecast entry is for. `time_t` is 64
/// bits on this target and narrower on others.
#[allow(clippy::unnecessary_cast)]
fn update_time(forecast: &libgweather::Info) -> Option<i64> {
    forecast.value_update().map(|time| time as i64)
}

pub struct WeatherClient {
    fixture: Option<PathBuf>,
    world: Option<libgweather::Location>,
    info: Option<libgweather::Info>,
    /// The location last given to `info`.
    location: RefCell<Option<libgweather::Location>>,
    loading: Cell<bool>,
    location_valid: Cell<bool>,
    /// Microseconds since the epoch; 0 before the first update.
    last_update: Cell<i64>,
    auto_location_requested: Cell<bool>,
    most_recent_location: RefCell<Option<libgweather::Location>>,
    gclue_starting: Cell<bool>,
    gclue_started: Cell<bool>,
    /// GeoClue's latest fix (`Geoclue.Simple`'s `location`), in degrees.
    gclue_fix: Cell<Option<(f64, f64)>>,
    /// Whether fixes move the forecast (`_gclueLocationChangedId`).
    gclue_monitoring: Cell<bool>,
    needs_auth: Cell<bool>,
    weather_authorized: Cell<bool>,
    location_settings: Option<gio::Settings>,
    shell: Option<gio::Settings>,
    weather_app: Option<gio::Settings>,
    /// The `AutomaticLocation` and `Locations` Weather last exported;
    /// kept only without gnome-shell's schema to cache them in.
    exported: RefCell<Option<(bool, Vec<glib::Variant>)>>,
    app: RefCell<Option<InstalledApp>>,
    changed: RefCell<Option<Rc<dyn Fn()>>>,
}

impl WeatherClient {
    pub fn new(apps: &Rc<LiveApps>) -> Rc<Self> {
        let fixture = std::env::var_os(FIXTURE_ENV).map(PathBuf::from);
        let real = fixture.is_none();
        let world = real.then(libgweather::Location::world).flatten();
        // `GWeather.Info` needs its own schema and is no use without the
        // locations database.
        let info = (world.is_some() && crate::settings(GWEATHER_SCHEMA).is_some()).then(|| {
            libgweather::Info::builder()
                .application_id(APPLICATION_ID)
                .contact_info(CONTACT_INFO)
                .enabled_providers(
                    libgweather::Provider::METAR
                        | libgweather::Provider::MET_NO
                        | libgweather::Provider::OWM,
                )
                .build()
        });
        let client = Rc::new(Self {
            fixture,
            world,
            info,
            location: RefCell::default(),
            loading: Cell::new(false),
            location_valid: Cell::new(false),
            last_update: Cell::new(0),
            auto_location_requested: Cell::new(false),
            most_recent_location: RefCell::default(),
            gclue_starting: Cell::new(false),
            gclue_started: Cell::new(false),
            gclue_fix: Cell::new(None),
            gclue_monitoring: Cell::new(false),
            needs_auth: Cell::new(true),
            weather_authorized: Cell::new(false),
            location_settings: app_integration::settings_with_keys(LOCATION_SCHEMA, &[ENABLED_KEY]),
            shell: app_integration::settings_with_keys(
                SHELL_SCHEMA,
                &[AUTOMATIC_LOCATION, LOCATIONS],
            ),
            weather_app: app_integration::settings_with_keys(
                WEATHER_SCHEMA,
                &[AUTOMATIC_LOCATION, LOCATIONS],
            ),
            exported: RefCell::default(),
            app: RefCell::default(),
            changed: RefCell::default(),
        });
        if real {
            client.follow_weather();
        }
        {
            let weak = Rc::downgrade(&client);
            apps.connect_changed(move || {
                if let Some(client) = weak.upgrade() {
                    client.on_installed_changed();
                }
            });
        }
        client.on_installed_changed();
        client
    }

    /// The constructor's subscriptions, in weather.js's order.
    fn follow_weather(self: &Rc<Self>) {
        self.watch_permission_store();
        if let Some(settings) = &self.location_settings {
            let weak = Rc::downgrade(self);
            settings.connect_changed(Some(ENABLED_KEY), move |_, _| {
                if let Some(client) = weak.upgrade() {
                    client.update_auto_location();
                }
            });
        }
        if let Some(info) = &self.info {
            let weak = Rc::downgrade(self);
            info.connect_updated(move |_| {
                if let Some(client) = weak.upgrade() {
                    client.loading.set(false);
                    client.last_update.set(glib::real_time());
                    client.emit();
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            app_integration::watch_exported(
                WEATHER_BUS_NAME,
                WEATHER_OBJECT_PATH,
                WEATHER_INTEGRATION_IFACE,
                move |properties| {
                    if let Some(client) = weak.upgrade() {
                        client.on_weather_properties_changed(properties);
                    }
                },
            );
        }
        for settings in [&self.shell, &self.weather_app].into_iter().flatten() {
            let weak = Rc::downgrade(self);
            settings.connect_changed(None, move |_, key| {
                let Some(client) = weak.upgrade() else {
                    return;
                };
                match key {
                    AUTOMATIC_LOCATION => client.on_automatic_location_changed(),
                    LOCATIONS => client.on_locations_changed(),
                    _ => {}
                }
            });
        }
        self.on_automatic_location_changed();
        self.on_locations_changed();
    }

    /// Call `f` on weather.js's `changed`.
    pub fn connect_changed(&self, f: impl Fn() + 'static) {
        *self.changed.borrow_mut() = Some(Rc::new(f));
    }

    fn emit(&self) {
        let f = self.changed.borrow().clone();
        if let Some(f) = f {
            f();
        }
    }

    /// `activateApp`.
    pub fn activate_app(&self) {
        if let Some(app) = self.app.borrow().as_ref() {
            if let Err(e) = tuna_shell_host::apps::launch(&app.entry) {
                eprintln!("tuna-shell-gtk: could not open Weather: {e}");
            }
        }
    }

    /// `update`: called each time the section is mapped.
    pub fn update(&self) {
        if self.fixture.is_some() {
            // The proof may have rewritten the file.
            self.emit();
            return;
        }
        let Some(info) = &self.info else { return };
        if !self.location_valid.get() {
            return;
        }
        if refreshes_quietly(info.is_valid(), glib::real_time(), self.last_update.get()) {
            info.update();
        } else {
            self.load_info();
        }
    }

    /// The client's state as the section reads it.
    pub fn snapshot(&self) -> Snapshot {
        let available = self.app.borrow().is_some();
        if let Some(path) = &self.fixture {
            let mut snapshot = std::fs::read_to_string(path)
                .ok()
                .and_then(|json| fixture_snapshot(&json, now_seconds()))
                .unwrap_or_default();
            snapshot.available = available;
            return snapshot;
        }
        let mut snapshot = Snapshot {
            available,
            has_location: self.location_valid.get(),
            loading: self.loading.get(),
            ..Snapshot::default()
        };
        let (Some(info), Some(location)) = (&self.info, self.location.borrow().clone()) else {
            return snapshot;
        };
        snapshot.location_name = location.name().map(String::from).unwrap_or_default();
        if location.level() != libgweather::LocationLevel::City && location.has_coords() {
            let (latitude, longitude) = location.coords();
            snapshot.nearest_city = self
                .world
                .as_ref()
                .and_then(|world| app_integration::nearest_city(world, latitude, longitude))
                .and_then(|city| city.name())
                .map(String::from);
        }
        snapshot.valid = info.is_valid();
        snapshot.network_error = info.network_error();
        if snapshot.valid {
            snapshot.forecasts = info
                .forecast_list()
                .iter()
                .map(|forecast| Forecast {
                    update: update_time(forecast),
                    icon: app_integration::symbolic_icon_name(forecast),
                    temp: forecast.value_temp(libgweather::TemperatureUnit::Default),
                })
                .collect();
        }
        snapshot
    }

    fn use_auto_location(&self) -> bool {
        use_auto_location(
            self.auto_location_requested.get(),
            self.location_settings
                .as_ref()
                .is_some_and(|settings| settings.boolean(ENABLED_KEY)),
            self.needs_auth.get(),
            self.weather_authorized.get(),
        )
    }

    /// Weather's `automatic-location`, from the first source that has it.
    fn automatic_location_setting(&self) -> bool {
        app_integration::effective_setting(
            self.shell
                .as_ref()
                .map(|shell| shell.boolean(AUTOMATIC_LOCATION)),
            self.exported.borrow().as_ref().map(|(auto, _)| *auto),
            self.weather_app
                .as_ref()
                .map(|weather| weather.boolean(AUTOMATIC_LOCATION)),
        )
        .unwrap_or(false)
    }

    /// Weather's serialized `locations`, from the first source that has
    /// them.
    fn serialized_locations(&self) -> Vec<glib::Variant> {
        app_integration::effective_setting(
            self.shell
                .as_ref()
                .map(|shell| app_integration::unpack_av(&shell.value(LOCATIONS))),
            self.exported
                .borrow()
                .as_ref()
                .map(|(_, locations)| locations.clone()),
            self.weather_app
                .as_ref()
                .map(|weather| app_integration::unpack_av(&weather.value(LOCATIONS))),
        )
        .unwrap_or_default()
    }

    /// `_onWeatherPropertiesChanged`: copy what Weather exports into
    /// gnome-shell's cache, or keep it here without one.
    fn on_weather_properties_changed(self: &Rc<Self>, properties: &glib::Variant) {
        let automatic = app_integration::exported_property(properties, "AutomaticLocation", "b")
            .and_then(|value| value.get::<bool>());
        let locations = app_integration::exported_property(properties, "Locations", "av");
        let (Some(automatic), Some(locations)) = (automatic, locations) else {
            return;
        };
        match &self.shell {
            Some(shell) => {
                let stored = shell
                    .set_boolean(AUTOMATIC_LOCATION, automatic)
                    .and_then(|()| shell.set_value(LOCATIONS, &locations));
                if let Err(e) = stored {
                    eprintln!("tuna-shell-gtk: could not store weather settings: {e}");
                }
            }
            None => {
                *self.exported.borrow_mut() =
                    Some((automatic, app_integration::unpack_av(&locations)));
                self.on_automatic_location_changed();
                self.on_locations_changed();
            }
        }
    }

    /// `_onInstalledChanged`.
    fn on_installed_changed(self: &Rc<Self>) {
        let had_app = self.app.borrow().is_some();
        let app = app_integration::installed_app(WEATHER_APP_ID);
        let have_app = app.is_some();
        let needs_auth = needs_auth(have_app, app.as_ref().is_some_and(|app| app.flatpak));
        *self.app.borrow_mut() = app;
        if had_app != have_app {
            self.emit();
        }
        if self.needs_auth.replace(needs_auth) != needs_auth {
            self.update_auto_location();
        }
    }

    /// `_loadInfo`.
    fn load_info(&self) {
        let Some(info) = &self.info else { return };
        self.loading.set(true);
        self.emit();
        info.update();
    }

    /// `_setLocation`.
    fn set_location(&self, location: Option<libgweather::Location>) {
        let Some(info) = &self.info else { return };
        if locations_equal(self.location.borrow().as_ref(), location.as_ref()) {
            return;
        }
        info.abort();
        info.set_location(location.as_ref());
        let valid = location.is_some();
        self.location_valid.set(valid);
        *self.location.borrow_mut() = location;
        if valid {
            self.load_info();
        } else {
            self.emit();
        }
    }

    fn set_most_recent_location(&self) {
        let location = self.most_recent_location.borrow().clone();
        self.set_location(location);
    }

    /// `_updateLocationMonitoring`.
    fn update_location_monitoring(&self) {
        if self.use_auto_location() {
            if self.gclue_monitoring.get() || !self.gclue_started.get() {
                return;
            }
            self.gclue_monitoring.set(true);
            self.on_gclue_location_changed();
        } else {
            self.gclue_monitoring.set(false);
        }
    }

    /// `_onGClueLocationChanged`.
    fn on_gclue_location_changed(&self) {
        if let Some((latitude, longitude)) = self.gclue_fix.get() {
            self.set_location(app_integration::detached_location(latitude, longitude));
        }
    }

    /// `_onAutomaticLocationChanged`.
    fn on_automatic_location_changed(self: &Rc<Self>) {
        let automatic = self.automatic_location_setting();
        if self.auto_location_requested.replace(automatic) == automatic {
            return;
        }
        self.update_auto_location();
    }

    /// `_updateAutoLocation`.
    fn update_auto_location(self: &Rc<Self>) {
        if self.fixture.is_some() {
            return;
        }
        self.update_location_monitoring();
        if self.use_auto_location() {
            self.start_gclue_service();
        } else {
            self.set_most_recent_location();
        }
    }

    /// `_onLocationsChanged`.
    fn on_locations_changed(&self) {
        let most_recent = self.world.as_ref().and_then(|world| {
            let serialized = self.serialized_locations().into_iter().next()?;
            app_integration::deserialize_location(world, &serialized)
        });
        if locations_equal(
            self.most_recent_location.borrow().as_ref(),
            most_recent.as_ref(),
        ) {
            return;
        }
        *self.most_recent_location.borrow_mut() = most_recent;
        if !self.use_auto_location() || !self.gclue_started.get() {
            self.set_most_recent_location();
        }
    }

    /// `_onPermStoreChanged`.
    fn permissions_changed(self: &Rc<Self>, permissions: &HashMap<String, Vec<String>>) {
        self.weather_authorized.set(weather_authorized(permissions));
        self.update_auto_location();
    }

    /// The constructor's permission-store proxy: look the table up
    /// (starting the store, as the proxy does) and follow `Changed`.
    fn watch_permission_store(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        gio::bus_get(
            gio::BusType::Session,
            None::<&gio::Cancellable>,
            move |conn| {
                let (Ok(conn), Some(client)) = (conn, weak.upgrade()) else {
                    return;
                };
                let weak = Rc::downgrade(&client);
                let sub = conn.subscribe_to_signal(
                    Some(PERM_STORE),
                    Some(PERM_STORE),
                    Some("Changed"),
                    Some(PERM_PATH),
                    None,
                    gio::DBusSignalFlags::NONE,
                    move |signal| {
                        let permissions = changed_permissions(signal.parameters);
                        if let (Some(client), Some(permissions)) = (weak.upgrade(), permissions) {
                            client.permissions_changed(&permissions);
                        }
                    },
                );
                // The subscription lives as long as the shell.
                std::mem::forget(sub);
                let weak = Rc::downgrade(&client);
                let bus = conn.clone();
                conn.call(
                    Some(PERM_STORE),
                    PERM_PATH,
                    PERM_STORE,
                    "Lookup",
                    Some(&(PERM_TABLE, PERM_ID).to_variant()),
                    None,
                    gio::DBusCallFlags::NONE,
                    5000,
                    None::<&gio::Cancellable>,
                    move |reply| {
                        let Some(client) = weak.upgrade() else {
                            return;
                        };
                        match reply {
                            Ok(reply) => client.permissions_changed(&looked_up_permissions(&reply)),
                            Err(_) => client.permission_lookup_failed(&bus),
                        }
                    },
                );
            },
        );
    }

    /// A failed lookup: with no store on the bus (it could not be
    /// started, likely because xdg-desktop-portal is not installed)
    /// access is not restricted; a store that answered with an error
    /// grants nothing.
    fn permission_lookup_failed(self: &Rc<Self>, bus: &gio::DBusConnection) {
        let weak = Rc::downgrade(self);
        bus.call(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "NameHasOwner",
            Some(&(PERM_STORE,).to_variant()),
            glib::VariantTy::new("(b)").ok(),
            gio::DBusCallFlags::NONE,
            5000,
            None::<&gio::Cancellable>,
            move |reply| {
                let Some(client) = weak.upgrade() else {
                    return;
                };
                let owned = reply
                    .ok()
                    .and_then(|reply| reply.child_value(0).get::<bool>())
                    .unwrap_or(false);
                if owned {
                    client.permissions_changed(&HashMap::new());
                } else {
                    client.weather_authorized.set(true);
                    client.update_auto_location();
                }
            },
        );
    }

    /// `_startGClueService`: one attempt for the shell's lifetime, as
    /// weather.js never clears `_gclueStarting`.
    fn start_gclue_service(self: &Rc<Self>) {
        if self.gclue_starting.replace(true) {
            return;
        }
        let client = self.clone();
        gio::bus_get(
            gio::BusType::System,
            None::<&gio::Cancellable>,
            move |conn| match conn {
                Ok(conn) => client.get_gclue_client(conn),
                Err(e) => client.gclue_failed(&e.to_string()),
            },
        );
    }

    /// The failure branch of `_startGClueService`.
    fn gclue_failed(&self, why: &str) {
        eprintln!("tuna-shell-gtk: failed to connect to Geoclue2 service: {why}");
        self.set_most_recent_location();
    }

    fn get_gclue_client(self: &Rc<Self>, conn: gio::DBusConnection) {
        let client = self.clone();
        let bus = conn.clone();
        conn.call(
            Some(GEOCLUE),
            GEOCLUE_MANAGER_PATH,
            GEOCLUE_MANAGER_IFACE,
            "GetClient",
            None,
            glib::VariantTy::new("(o)").ok(),
            gio::DBusCallFlags::NONE,
            -1,
            None::<&gio::Cancellable>,
            move |reply| match reply {
                Ok(reply) => match reply.child_value(0).str() {
                    Some(path) => client.start_gclue_client(&bus, path),
                    None => client.gclue_failed("malformed GetClient reply"),
                },
                Err(e) => client.gclue_failed(&e.to_string()),
            },
        );
    }

    /// Configure the client as weather.js does and start it; fixes
    /// arrive as `LocationUpdated`.
    fn start_gclue_client(self: &Rc<Self>, conn: &gio::DBusConnection, path: &str) {
        let weak = Rc::downgrade(self);
        let bus = conn.clone();
        let sub = conn.subscribe_to_signal(
            Some(GEOCLUE),
            Some(GEOCLUE_CLIENT_IFACE),
            Some("LocationUpdated"),
            Some(path),
            None,
            gio::DBusSignalFlags::NONE,
            move |signal| {
                let new = signal.parameters.try_child_value(1);
                let new = new.as_ref().and_then(|path| path.str());
                if let (Some(client), Some(new)) = (weak.upgrade(), new) {
                    client.read_gclue_location(&bus, new);
                }
            },
        );
        // The subscription lives as long as the shell.
        std::mem::forget(sub);
        for (property, value) in [
            ("DesktopId", GEOCLUE_DESKTOP_ID.to_variant()),
            ("RequestedAccuracyLevel", ACC_CITY.to_variant()),
            ("DistanceThreshold", GEOCLUE_DISTANCE_THRESHOLD.to_variant()),
        ] {
            conn.call(
                Some(GEOCLUE),
                path,
                PROPERTIES,
                "Set",
                Some(&(GEOCLUE_CLIENT_IFACE, property, value).to_variant()),
                None,
                gio::DBusCallFlags::NONE,
                5000,
                None::<&gio::Cancellable>,
                |_| {},
            );
        }
        let weak = Rc::downgrade(self);
        conn.call(
            Some(GEOCLUE),
            path,
            GEOCLUE_CLIENT_IFACE,
            "Start",
            None,
            None,
            gio::DBusCallFlags::NONE,
            -1,
            None::<&gio::Cancellable>,
            move |reply| {
                if let (Err(e), Some(client)) = (reply, weak.upgrade()) {
                    client.gclue_failed(&e.to_string());
                }
            },
        );
    }

    /// A new fix: the first one is `Geoclue.Simple.new` resolving, later
    /// ones are its `notify::location`.
    fn read_gclue_location(self: &Rc<Self>, conn: &gio::DBusConnection, path: &str) {
        let weak = Rc::downgrade(self);
        conn.call(
            Some(GEOCLUE),
            path,
            PROPERTIES,
            "GetAll",
            Some(&(GEOCLUE_LOCATION_IFACE,).to_variant()),
            glib::VariantTy::new("(a{sv})").ok(),
            gio::DBusCallFlags::NONE,
            5000,
            None::<&gio::Cancellable>,
            move |reply| {
                let fix = reply
                    .ok()
                    .and_then(|reply| reply.try_child_value(0))
                    .and_then(|properties| geoclue_fix(&properties));
                let (Some(client), Some(fix)) = (weak.upgrade(), fix) else {
                    return;
                };
                client.gclue_fix.set(Some(fix));
                if !client.gclue_started.replace(true) {
                    client.update_location_monitoring();
                } else if client.gclue_monitoring.get() {
                    client.on_gclue_location_changed();
                }
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn permissions(rows: &[(&str, &[&str])]) -> HashMap<String, Vec<String>> {
        rows.iter()
            .map(|(app, row)| {
                (
                    (*app).to_owned(),
                    row.iter().map(|s| (*s).to_owned()).collect(),
                )
            })
            .collect()
    }

    #[test]
    fn auto_location_needs_the_request_the_switch_and_the_grant() {
        // Requested, Location on, a host-installed Weather: no grant needed.
        assert!(use_auto_location(true, true, false, false));
        // A Flatpak Weather needs the permission store's grant.
        assert!(!use_auto_location(true, true, true, false));
        assert!(use_auto_location(true, true, true, true));
        // The master switch and the request each veto it.
        assert!(!use_auto_location(true, false, false, true));
        assert!(!use_auto_location(false, true, false, true));
    }

    #[test]
    fn only_a_host_installed_weather_skips_authorization() {
        assert!(!needs_auth(true, false));
        assert!(needs_auth(true, true));
        assert!(needs_auth(false, false));
    }

    #[test]
    fn weather_is_authorized_by_its_accuracy_row() {
        assert!(weather_authorized(&permissions(&[(
            "org.gnome.Weather",
            &["CITY", "1700000000"]
        )])));
        assert!(weather_authorized(&permissions(&[(
            "org.gnome.Weather",
            &["EXACT"]
        )])));
        assert!(!weather_authorized(&permissions(&[(
            "org.gnome.Weather",
            &["NONE", "1700000000"]
        )])));
        // Another app's grant, no row at all, and a row naming no
        // accuracy all leave Weather unauthorized.
        assert!(!weather_authorized(&permissions(&[(
            "org.gnome.Maps",
            &["EXACT"]
        )])));
        assert!(!weather_authorized(&HashMap::new()));
        assert!(!weather_authorized(&permissions(&[(
            "org.gnome.Weather",
            &[]
        )])));
    }

    fn variant(ty: &str, text: &str) -> glib::Variant {
        glib::Variant::parse(glib::VariantTy::new(ty).ok(), text).unwrap()
    }

    #[test]
    fn store_changes_count_only_for_the_geolocation_table() {
        let changed = variant(
            "(ssbva{sas})",
            "('gnome', 'geolocation', false, <byte 0>, {'org.gnome.Weather': ['CITY', '0']})",
        );
        assert_eq!(
            changed_permissions(&changed),
            Some(permissions(&[("org.gnome.Weather", &["CITY", "0"])]))
        );
        for other in [
            "('gnome', 'shortcuts-inhibitor', false, <byte 0>, {'org.gnome.Weather': ['CITY']})",
            "('location', 'geolocation', false, <byte 0>, {'org.gnome.Weather': ['CITY']})",
        ] {
            assert_eq!(changed_permissions(&variant("(ssbva{sas})", other)), None);
        }
        // A signal of another shape is ignored, not misread.
        assert_eq!(
            changed_permissions(&("gnome", "geolocation").to_variant()),
            None
        );
    }

    #[test]
    fn lookup_replies_unpack_and_anything_else_grants_nothing() {
        let reply = variant(
            "(a{sas}v)",
            "({'org.gnome.Weather': ['EXACT', '12']}, <byte 0>)",
        );
        assert!(weather_authorized(&looked_up_permissions(&reply)));
        assert!(looked_up_permissions(&variant("(a{sas}v)", "({}, <byte 0>)")).is_empty());
        assert!(looked_up_permissions(&("org.gnome.Weather", "EXACT").to_variant()).is_empty());
    }

    #[test]
    fn a_recent_valid_forecast_refreshes_without_loading() {
        const MINUTE: i64 = 60 * 1_000_000;
        let now = 1_800_000_000 * 1_000_000;
        assert!(refreshes_quietly(true, now, now - 9 * MINUTE));
        // Ten minutes is the threshold itself: loading shows again.
        assert!(!refreshes_quietly(true, now, now - 10 * MINUTE));
        assert!(!refreshes_quietly(true, now, 0));
        // An invalid forecast always reloads visibly.
        assert!(!refreshes_quietly(false, now, now - MINUTE));
    }

    #[test]
    fn geoclue_fix_reads_both_coordinates() {
        let location = variant(
            "a{sv}",
            "{'Latitude': <38.72>, 'Longitude': <-9.14>, 'Accuracy': <25000.0>, 'Description': <''>}",
        );
        assert_eq!(geoclue_fix(&location), Some((38.72, -9.14)));
        assert_eq!(
            geoclue_fix(&variant("a{sv}", "{'Latitude': <38.72>}")),
            None
        );
        // Coordinates of the wrong type are no fix.
        assert_eq!(
            geoclue_fix(&variant(
                "a{sv}",
                "{'Latitude': <'38.72'>, 'Longitude': <'-9.14'>}"
            )),
            None
        );
    }

    #[test]
    fn fixture_states_map_to_the_client_states() {
        let now = 1_800_000_000;
        assert_eq!(
            fixture_snapshot(r#"{"state": "none"}"#, now),
            Some(Snapshot::default())
        );
        let loading = fixture_snapshot(r#"{"state": "loading", "location": "Lisbon"}"#, now)
            .expect("loading fixture");
        assert!(loading.has_location && loading.loading && !loading.valid);
        assert_eq!(loading.location_name, "Lisbon");
        let offline = fixture_snapshot(r#"{"state": "offline", "location": "Lisbon"}"#, now)
            .expect("offline fixture");
        assert!(offline.has_location && offline.network_error && !offline.valid);
        let unavailable =
            fixture_snapshot(r#"{"state": "unavailable", "location": "Lisbon"}"#, now)
                .expect("unavailable fixture");
        assert!(unavailable.has_location && !unavailable.network_error && !unavailable.valid);
        // Forecasts only belong to the forecast state.
        assert!(fixture_snapshot(
            r#"{"state": "offline", "forecasts": [{"in_minutes": 5, "temp": 1}]}"#,
            now
        )
        .expect("offline fixture")
        .forecasts
        .is_empty());
    }

    #[test]
    fn fixture_forecasts_are_placed_relative_to_now() {
        let now = 1_800_000_000;
        let snapshot = fixture_snapshot(
            r#"{"state": "forecast", "location": "Lisbon", "forecasts": [
                {"in_minutes": 30, "icon": "weather-clear-symbolic", "temp": 21.4},
                {"in_minutes": -10, "temp": -3},
                {"icon": "weather-fog-symbolic"}]}"#,
            now,
        )
        .expect("forecast fixture");
        assert!(snapshot.has_location && snapshot.valid && !snapshot.loading);
        assert_eq!(
            snapshot.forecasts,
            [
                Forecast {
                    update: Some(now + 1800),
                    icon: Some("weather-clear-symbolic".to_owned()),
                    temp: Some(21.4),
                },
                Forecast {
                    update: Some(now - 600),
                    icon: None,
                    temp: Some(-3.0),
                },
                Forecast {
                    update: None,
                    icon: Some("weather-fog-symbolic".to_owned()),
                    temp: None,
                },
            ]
        );
    }

    #[test]
    fn unusable_fixtures_are_rejected() {
        let now = 1_800_000_000;
        assert_eq!(fixture_snapshot("not json", now), None);
        assert_eq!(fixture_snapshot(r#"{"location": "Lisbon"}"#, now), None);
        assert_eq!(fixture_snapshot(r#"{"state": "sunny"}"#, now), None);
        assert_eq!(fixture_snapshot(r#"{"state": 3}"#, now), None);
    }
}
