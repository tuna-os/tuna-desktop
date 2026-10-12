//! Night Light schedule, settings and output tint (#339).
//!
//! GNOME's Night Light warms the display's colour temperature at night.
//! The quick-settings tile writes `night-light-enabled`; this module
//! owns everything downstream of the settings keys: whether the
//! filter is active right now, at what (smeared) temperature, what
//! channel gains that temperature maps to, and the GL tint that
//! applies those gains to every output:
//!
//! - [`NightLightConfig`]: switch, temperature, schedule (manual,
//!   automatic sunset-to-sunrise from a latitude/longitude, or a
//!   fixed from/to window) and the in-memory "disabled until
//!   tomorrow" state. The temperature clamps to
//!   [`MIN_TEMPERATURE_K`]..=[`MAX_TEMPERATURE_K`].
//! - [`temperature_to_gains`]: colour-temperature-to-gains mapping,
//!   normalised so [`NEUTRAL_TEMPERATURE_K`] is unity.
//! - [`load_from_gsettings`]: the production settings reader. It runs
//!   `gsettings list-recursively` over [`COLOR_SCHEMA`] and parses
//!   the keys; any failure leaves the previous config in place
//!   (fail closed).
//! - [`NightLightController`]: per-tick evaluation (settings
//!   refresh, schedule, smear, temp-disable, 10 K hysteresis) shared
//!   with the D-Bus thread through [`NightLightShare`].
//! - [`tint_program`]: the GL texture program that multiplies every
//!   drawn texel by the gains. The override lives for one frame only,
//!   so neutral restore is by construction: disabled means the
//!   program is never installed.
//!
//! All time math is in UTC over Unix timestamps, so daylight-saving
//! transitions cannot shift the window: the same UTC instant always
//! yields the same answer.
//!
//! # GNOME 51 behaviour and defaults
//!
//! Compared against the 51.0 tags (g-s-d `51.0` =
//! `7f7913cc655b7c7b924d8f277177f95ac32bcd49`, mutter `51.0` =
//! `138a14fbeef09d49ebf5be8a0cb83b042dd5c841`, g-c-c `51.0` commit
//! `6ae712c0454f2586f00821c3486150c1e8feb3e7`):
//!
//! - g-s-d computes a smeared temperature and publishes it on
//!   `org.gnome.SettingsDaemon.Color` (`Temperature`,
//!   `NightLightActive`, read-write `DisabledUntilTomorrow`);
//!   mutter reads `Temperature` and folds it into each output's
//!   gamma LUT (GAMMA_LUT, else CTM). We publish the same two
//!   properties on the same bus name from the compositor and apply
//!   the gains in GL instead (pinned smithay 0.7 exposes no gamma
//!   LUT API).
//! - Schema defaults (`data/…plugins.color.gschema.xml.in`): enabled
//!   false, temperature 2700 K, schedule-automatic true, from 20.0,
//!   to 6.0, last-coordinates (91, 181) as the invalid sentinel.
//!   Missing keys fall back to these; invalid coordinates fall back
//!   to the manual from/to window, as g-s-d does.
//! - Edges smear over one hour (`GSD_NIGHT_LIGHT_POLL_SMEAR`), the
//!   smear clamps below both the night and the day length, `from ==
//!   to` reads as the full day (`frac_day_is_between`), changes of
//!   10 K or less are ignored (`GSD_TEMPERATURE_MAX_DELTA`), and the
//!   recheck cadence is 5 s (`GSD_NIGHT_LIGHT_SCHEDULE_TIMEOUT`).
//! - "Disabled until tomorrow" is in-memory (not a settings key);
//!   it resets past 24 h or when a sunrise falls between the
//!   disable time and now.
//! - The Settings slider spans 1700–4700 K (schema range 1000–10000
//!   K); moving it also calls `NightLightPreview(5)`, which we do
//!   not implement (the key write still applies within one poll).
//!
//! Known divergences from GNOME 51, all recorded rather than
//! invented: schedule math is UTC where GNOME uses local time;
//! Kelvin maps through Tanner Helland's approximation where mutter
//! uses colord's Planckian blackbody (`cd_color_get_blackbody_rgb`);
//! settings refresh polls every [`READ_INTERVAL`] where g-s-d gets
//! instant change signals; polar day/night read inactive/active
//! where g-s-d keeps its last sunrise/sunset (manual fallback);
//! transient solid feedback (screenshot flashes; the bell and the
//! blank shield are black, so gains are a no-op on them) and the DRM
//! hardware cursor bypass the tint; screenshots and screen casts stay untinted,
//! matching GNOME (gamma applies past the framebuffer).
//!
//! Sources:
//!
//! - <https://gitlab.gnome.org/GNOME/gnome-settings-daemon/-/blob/51.0/plugins/color/gsd-night-light.c>
//!   (sha256 `ed70f573…fc0ee`)
//! - <https://gitlab.gnome.org/GNOME/gnome-settings-daemon/-/blob/51.0/plugins/color/gsd-night-light-common.c>
//!   (sha256 `0acbeb57…050cd`)
//! - <https://gitlab.gnome.org/GNOME/gnome-settings-daemon/-/blob/51.0/data/org.gnome.settings-daemon.plugins.color.gschema.xml.in>
//!   (sha256 `1ef2a6fe…11214`)
//! - <https://gitlab.gnome.org/GNOME/gnome-settings-daemon/-/blob/51.0/plugins/color/gsd-color-manager.c>
//!   (`org.gnome.SettingsDaemon.Color`, sha256 `0b594f5b…f69fc`)
//! - <https://gitlab.gnome.org/GNOME/mutter/-/blob/51.0/src/backends/meta-color-manager.c>
//!   (sha256 `425772e0…0e43e1`)
//! - <https://gitlab.gnome.org/GNOME/mutter/-/blob/51.0/src/backends/meta-color-device.c>
//!   (sha256 `7363173c…bbb5a1`)
//! - <https://gitlab.gnome.org/GNOME/mutter/-/blob/51.0/src/backends/meta-color-profile.c>
//!   (blackbody LUT, sha256 `ddbe997e…0441fc`)
//! - <https://gitlab.gnome.org/GNOME/gnome-control-center/-/blob/6ae712c0454f2586f00821c3486150c1e8feb3e7/panels/display/cc-night-light-page.c>
//!   (sha256 `6545dc78e8b2d00a7e7b7b5f2131154982cbe19723629be7beecce3e722e6062`)
//! - <https://gitlab.gnome.org/GNOME/gnome-control-center/-/blob/6ae712c0454f2586f00821c3486150c1e8feb3e7/panels/display/cc-night-light-page.blp>
//!   (slider 1700–4700 K, sha256 `82bd9894…2ca32a6`)

/// GSettings schema for GNOME settings-daemon's colour plugin, which
/// owns the `night-light-*` keys.
pub const COLOR_SCHEMA: &str = "org.gnome.settings-daemon.plugins.color";
/// Master switch for the filter.
pub const KEY_ENABLED: &str = "night-light-enabled";
/// Requested colour temperature in Kelvin (uint32).
pub const KEY_TEMPERATURE: &str = "night-light-temperature";
/// Whether the active window follows sunset/sunrise (boolean).
pub const KEY_SCHEDULE_AUTOMATIC: &str = "night-light-schedule-automatic";
/// Fixed window start in hours (double); read when automatic is off.
pub const KEY_SCHEDULE_FROM: &str = "night-light-schedule-from";
/// Fixed window end in hours (double); read when automatic is off.
pub const KEY_SCHEDULE_TO: &str = "night-light-schedule-to";
/// Last geoclue position as a `(latitude, longitude)` pair of
/// doubles; the schema default is the invalid sentinel
/// ([`INVALID_LATITUDE`], [`INVALID_LONGITUDE`]), which reads as
/// "no position yet" and falls back to the fixed window.
pub const KEY_LAST_COORDINATES: &str = "night-light-last-coordinates";

/// GSettings schema owning `color-scheme` (dark-style preference).
pub const INTERFACE_SCHEMA: &str = "org.gnome.desktop.interface";
/// Dark-style preference key; Night Light only ever reads it.
pub const KEY_COLOR_SCHEME: &str = "color-scheme";

/// Coldest temperature the model represents (GNOME Settings minimum).
pub const MIN_TEMPERATURE_K: u32 = 1000;
/// Hottest temperature the model represents (GNOME Settings maximum).
pub const MAX_TEMPERATURE_K: u32 = 10000;
/// Neutral daylight white: [`temperature_to_gains`] returns unity here.
pub const NEUTRAL_TEMPERATURE_K: u32 = 6500;
/// Fallback temperature when no key has been read yet: the schema
/// default (`night-light-temperature` defaults to 2700 K).
pub const DEFAULT_TEMPERATURE_K: u32 = 2700;
/// Invalid-coordinate sentinel from the schema default
/// `night-light-last-coordinates = (91, 181)`: out of range on both
/// axes, so it can never be a real (clamped, wrapped) position.
pub const INVALID_LATITUDE: f64 = 91.0;
/// Longitude half of the invalid-coordinate sentinel.
pub const INVALID_LONGITUDE: f64 = 181.0;
/// Schema default for the fixed window start (hours).
pub const DEFAULT_FROM_HOUR: f64 = 20.0;
/// Schema default for the fixed window end (hours).
pub const DEFAULT_TO_HOUR: f64 = 6.0;
/// Temperature ramp around each schedule edge, in hours
/// (`GSD_NIGHT_LIGHT_POLL_SMEAR`): the filter eases from
/// [`NEUTRAL_TEMPERATURE_K`] to the requested temperature over the
/// hour before `from`/sunset and back over the hour before
/// `to`/sunrise. Clamped below both the night and the day length.
pub const SMEAR_HOURS: f64 = 1.0;
/// Temperature changes at or below this many Kelvin are ignored
/// (`GSD_TEMPERATURE_MAX_DELTA`): during a smear the applied gains
/// step at most every few seconds instead of every frame.
pub const TEMPERATURE_QUANTUM_K: u32 = 10;
/// How often the controller re-reads the settings keys
/// (`GSD_NIGHT_LIGHT_SCHEDULE_TIMEOUT`). GNOME reacts to change
/// signals instantly and rechecks on this cadence; polling on the
/// same cadence bounds our worst-case lag to one interval.
pub const READ_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

/// Minutes per day; sunrise/sunset are minutes since 00:00 UTC.
const MINUTES_PER_DAY: f64 = 1440.0;
/// Official sunrise/sunset zenith (90°50'): the Sun's upper limb on
/// the horizon, the standard observatory definition.
const SUN_ZENITH_DEG: f64 = 90.833;

/// Clamp a requested temperature into [`MIN_TEMPERATURE_K`]
/// ..=[`MAX_TEMPERATURE_K`].
pub fn clamp_temperature(kelvin: u32) -> u32 {
    kelvin.clamp(MIN_TEMPERATURE_K, MAX_TEMPERATURE_K)
}

/// GNOME's `org.gnome.desktop.interface color-scheme` values.
///
/// Night Light applies orthogonally to every scheme: the mapping is
/// read-only context for callers (e.g. a shell toggle deciding what
/// to display), never a second switch. Nothing here writes the key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorScheme {
    /// `default`: follow the platform default style.
    #[default]
    Default,
    /// `prefer-dark`.
    PreferDark,
    /// `prefer-light`.
    PreferLight,
}

impl ColorScheme {
    /// Parse a `color-scheme` string; unknown values yield `None`
    /// rather than guessing.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "default" => Some(Self::Default),
            "prefer-dark" => Some(Self::PreferDark),
            "prefer-light" => Some(Self::PreferLight),
            _ => None,
        }
    }

    /// Back to the exact GSettings string.
    pub fn as_gsettings(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::PreferDark => "prefer-dark",
            Self::PreferLight => "prefer-light",
        }
    }
}

/// Per-channel multipliers for a colour temperature, each in 0..=1.
///
/// A gain of 1.0 leaves the channel untouched; lower values dim it.
/// The GL tint ([`tint_program`]) multiplies every drawn texel by
/// these; unity gains mean no override is installed at all.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorGains {
    /// Red multiplier.
    pub red: f64,
    /// Green multiplier.
    pub green: f64,
    /// Blue multiplier.
    pub blue: f64,
}

/// Map a colour temperature in Kelvin to per-channel gains.
///
/// Warm temperatures (low Kelvin) keep red and dim blue; cool ones do
/// the opposite. The input clamps to
/// [`MIN_TEMPERATURE_K`]..=[`MAX_TEMPERATURE_K`], and the output
/// normalises against [`NEUTRAL_TEMPERATURE_K`], so 6500 K is exactly
/// unity on every channel. Pure function of its argument.
pub fn temperature_to_gains(kelvin: u32) -> ColorGains {
    let kelvin = clamp_temperature(kelvin);
    let (r, g, b) = raw_channels(kelvin);
    let (nr, ng, nb) = raw_channels(NEUTRAL_TEMPERATURE_K);
    ColorGains {
        red: (r / nr).clamp(0.0, 1.0),
        green: (g / ng).clamp(0.0, 1.0),
        blue: (b / nb).clamp(0.0, 1.0),
    }
}

/// Unnormalised Tanner Helland warm/cool channels for `kelvin`.
fn raw_channels(kelvin: u32) -> (f64, f64, f64) {
    let t = f64::from(kelvin) / 100.0;
    let red = if t <= 66.0 {
        255.0
    } else {
        329.698_727_446 * (t - 60.0).powf(-0.133_204_759_2)
    };
    let green = if t <= 66.0 {
        99.470_802_586_1 * t.ln() - 161.119_568_166_1
    } else {
        288.122_169_528_3 * (t - 60.0).powf(-0.075_514_849_2)
    };
    let blue = if t >= 66.0 {
        255.0
    } else if t <= 19.0 {
        0.0
    } else {
        138.517_731_223_1 * (t - 10.0).ln() - 305.044_792_730_7
    };
    (
        red.clamp(0.0, 255.0),
        green.clamp(0.0, 255.0),
        blue.clamp(0.0, 255.0),
    )
}

/// When the filter may be active.
#[derive(Debug, Clone, PartialEq)]
pub enum NightLightSchedule {
    /// Manual on/off: active exactly when [`NightLightConfig::enabled`].
    Manual,
    /// Sunset-to-sunrise window for a location; needs no clock beyond
    /// the Unix timestamp (geoclue would supply the coordinates; this
    /// model only consumes them).
    Automatic {
        /// Degrees north, clamped to -90..=90.
        latitude_deg: f64,
        /// Degrees east, wrapped to -180..=180.
        longitude_deg: f64,
    },
    /// Fixed daily window in UTC hours so the math stays
    /// DST-agnostic (GNOME stores local-time hours; without a
    /// timezone database UTC is the unambiguous reading).
    Fixed {
        /// Window start, hours since 00:00 UTC, wrapped to 0..24.
        from_hour: f64,
        /// Window end, hours since 00:00 UTC, wrapped to 0..24.
        to_hour: f64,
    },
}

/// Night Light preference state: switch, temperature, schedule and
/// the in-memory "disabled until tomorrow" state.
///
/// Held by [`crate::state::StateModel`] as a plain (unrevisioned)
/// display preference; the settings reader fills it from the
/// [`COLOR_SCHEMA`] keys (except `disabled_until_tmw`, which has no
/// key and only arrives over D-Bus).
#[derive(Debug, Clone, PartialEq)]
pub struct NightLightConfig {
    /// Master switch (`night-light-enabled`).
    pub enabled: bool,
    /// Requested temperature in Kelvin; always clamped (see
    /// [`NightLightConfig::set_temperature_k`]).
    temperature_k: u32,
    /// When the filter may be active.
    pub schedule: NightLightSchedule,
    /// Unix seconds when "disabled until tomorrow" was set, or
    /// `None`. In-memory like GNOME's `disabled-until-tmw`: never
    /// persisted, never read from settings. While set, the filter
    /// stays off until 24 h pass or a sunrise falls between the set
    /// time and now (see [`NightLightConfig::is_active_at`]).
    pub disabled_until_tmw: Option<i64>,
}

impl Default for NightLightConfig {
    /// Off, default temperature, manual schedule, no temp-disable.
    fn default() -> Self {
        Self {
            enabled: false,
            temperature_k: DEFAULT_TEMPERATURE_K,
            schedule: NightLightSchedule::Manual,
            disabled_until_tmw: None,
        }
    }
}

impl NightLightConfig {
    /// Requested temperature, clamped to the representable range.
    pub fn temperature_k(&self) -> u32 {
        clamp_temperature(self.temperature_k)
    }

    /// Store a requested temperature, clamped to the representable range.
    pub fn set_temperature_k(&mut self, kelvin: u32) {
        self.temperature_k = clamp_temperature(kelvin);
    }

    /// Whether the filter is active at `unix_seconds` (UTC).
    ///
    /// Disabled is always inactive, as is a live "disabled until
    /// tomorrow". Manual follows the switch; automatic is active
    /// through the night (sunset to sunrise) and fixed inside its
    /// window, both extended by the smear lead-in before the start
    /// edge. Polar day reads inactive and polar night active,
    /// matching "dark outside" (GNOME instead keeps its last
    /// sunrise/sunset there, i.e. a manual fallback).
    pub fn is_active_at(&self, unix_seconds: i64) -> bool {
        self.active_temperature_at(unix_seconds).is_some()
    }

    /// Smeared temperature to apply at `unix_seconds`, or `None`
    /// when inactive. The temperature eases from
    /// [`NEUTRAL_TEMPERATURE_K`] to the requested one over
    /// [`SMEAR_HOURS`] before the start edge and back over the hour
    /// before the end edge; a manual schedule has no edges and
    /// applies the requested temperature directly.
    pub fn active_temperature_at(&self, unix_seconds: i64) -> Option<u32> {
        if !self.enabled || self.temp_disabled_at(unix_seconds) {
            return None;
        }
        let target = self.temperature_k();
        match &self.schedule {
            NightLightSchedule::Manual => Some(target),
            NightLightSchedule::Automatic {
                latitude_deg,
                longitude_deg,
            } => automatic_temperature(*latitude_deg, *longitude_deg, target, unix_seconds),
            NightLightSchedule::Fixed { from_hour, to_hour } => {
                fixed_temperature(*from_hour, *to_hour, target, unix_seconds)
            }
        }
    }

    /// Gains to apply at `unix_seconds`, or `None` when inactive.
    pub fn active_gains_at(&self, unix_seconds: i64) -> Option<ColorGains> {
        self.active_temperature_at(unix_seconds)
            .map(temperature_to_gains)
    }

    /// Whether "disabled until tomorrow" suppresses the filter at
    /// `unix_seconds`. Set in the future (a clock step back) stays
    /// suppressing; past 24 h it lapses; otherwise a sunrise instant
    /// in `(set, now]` clears it — the `to` hour for a fixed window,
    /// the computed sunrise for an automatic one. A manual schedule
    /// has no sunrise, so only the 24 h lapse applies there.
    fn temp_disabled_at(&self, unix_seconds: i64) -> bool {
        let Some(set_at) = self.disabled_until_tmw else {
            return false;
        };
        if unix_seconds <= set_at {
            return true;
        }
        if unix_seconds - set_at > 86_400 {
            return false;
        }
        !sunrise_between(&self.schedule, set_at, unix_seconds)
    }
}

/// Whether a sunrise instant falls in `(set_at, now]`, clearing
/// "disabled until tomorrow". See
/// [`NightLightConfig::is_active_at`].
fn sunrise_between(schedule: &NightLightSchedule, set_at: i64, now: i64) -> bool {
    let clears = |instant: i64| instant > set_at && instant <= now;
    match schedule {
        // No sunrise on a manual schedule: only the 24 h lapse.
        NightLightSchedule::Manual => false,
        NightLightSchedule::Fixed { to_hour, .. } => {
            let to_seconds = (to_hour.rem_euclid(24.0) * 3600.0).round() as i64;
            let first = set_at.div_euclid(86_400) - 1;
            let last = now.div_euclid(86_400) + 1;
            (first..=last).any(|day| clears(day * 86_400 + to_seconds))
        }
        NightLightSchedule::Automatic {
            latitude_deg,
            longitude_deg,
        } => {
            let first = set_at.div_euclid(86_400) - 1;
            let last = now.div_euclid(86_400) + 1;
            (first..=last).any(|day| {
                matches!(
                    sun_window(*latitude_deg, *longitude_deg, day * 86_400),
                    Daylight::Normal(window)
                        if clears(
                            day * 86_400 + (window.sunrise_utc_min * 60.0).round() as i64
                        )
                )
            })
        }
    }
}

/// Night length of the fixed UTC-hour window, in hours. A wrapping
/// window (e.g. 22:00–06:00) spans midnight; `from == to` reads as
/// the full day, matching GNOME's `frac_day_is_between` (equal
/// bounds always match).
fn fixed_night_len_hours(from_hour: f64, to_hour: f64) -> f64 {
    let len = (to_hour - from_hour).rem_euclid(24.0);
    if len == 0.0 {
        24.0
    } else {
        len
    }
}

/// Smear for a night of `night_len_hours`: [`SMEAR_HOURS`] clamped
/// below both the night and the day length, so the lead-in and
/// trail-off ramps never overlap. A full-day window has no edges
/// and no smear.
fn smear_hours_for(night_len_hours: f64) -> f64 {
    SMEAR_HOURS.min(night_len_hours.min(24.0 - night_len_hours).max(0.0))
}

/// GNOME's `linear_interpolate(DEFAULT, target, factor)`: `factor`
/// 1 is neutral, 0 is the requested temperature.
fn lerp_temperature(neutral_k: u32, target_k: u32, factor: f64) -> u32 {
    clamp_temperature(
        ((f64::from(neutral_k) - f64::from(target_k)) * factor.clamp(0.0, 1.0)
            + f64::from(target_k))
        .round() as u32,
    )
}

/// Smeared temperature for the fixed UTC-hour window at
/// `unix_seconds`, or `None` outside `[from - smear, to)`.
fn fixed_temperature(
    from_hour: f64,
    to_hour: f64,
    target_k: u32,
    unix_seconds: i64,
) -> Option<u32> {
    let from = from_hour.rem_euclid(24.0);
    let len = fixed_night_len_hours(from_hour, to_hour);
    let smear = smear_hours_for(len);
    let now = (unix_seconds.rem_euclid(86_400) as f64) / 3600.0;
    // Hours since the window start, in [0, 24).
    let since_from = (now - from).rem_euclid(24.0);
    if since_from >= len && since_from < 24.0 - smear {
        return None;
    }
    if smear > 0.0 && since_from >= 24.0 - smear {
        // Lead-in ramp: neutral at the smear start, target at `from`.
        let factor = 1.0 - (since_from - (24.0 - smear)) / smear;
        return Some(lerp_temperature(NEUTRAL_TEMPERATURE_K, target_k, factor));
    }
    if smear > 0.0 && since_from >= len - smear && since_from < len {
        // Trail-off ramp: target an hour before `to`, neutral at `to`.
        let factor = (since_from - (len - smear)) / smear;
        return Some(lerp_temperature(NEUTRAL_TEMPERATURE_K, target_k, factor));
    }
    Some(target_k)
}

/// One night as Unix seconds: sunset to the next sunrise.
struct NightInterval {
    start: i64,
    end: i64,
}

/// Night intervals covering `unix_seconds` from the UTC dates
/// around it: each normal day's sunset paired with the nearest
/// later sunrise within 24 h. Polar days contribute nothing (the
/// caller's polar handling decides those).
fn night_intervals(latitude_deg: f64, longitude_deg: f64, unix_seconds: i64) -> Vec<NightInterval> {
    let day = unix_seconds.div_euclid(86_400);
    let mut sunsets = Vec::new();
    let mut sunrises = Vec::new();
    for date in [day - 1, day, day + 1] {
        if let Daylight::Normal(window) = sun_window(latitude_deg, longitude_deg, date * 86_400) {
            sunsets.push(date * 86_400 + (window.sunset_utc_min * 60.0).round() as i64);
            sunrises.push(date * 86_400 + (window.sunrise_utc_min * 60.0).round() as i64);
        }
    }
    sunrises.sort_unstable();
    sunsets
        .into_iter()
        .filter_map(|sunset| {
            sunrises
                .iter()
                .find(|sunrise| **sunrise > sunset && **sunrise - sunset <= 86_400)
                .map(|sunrise| NightInterval {
                    start: sunset,
                    end: *sunrise,
                })
        })
        .collect()
}

/// Smeared temperature for the automatic schedule at
/// `unix_seconds`, or `None` outside every `[sunset - smear,
/// sunrise)`. Polar day reads inactive, polar night fully active.
fn automatic_temperature(
    latitude_deg: f64,
    longitude_deg: f64,
    target_k: u32,
    unix_seconds: i64,
) -> Option<u32> {
    match sun_window(latitude_deg, longitude_deg, unix_seconds) {
        Daylight::PolarDay => return None,
        Daylight::PolarNight => return Some(target_k),
        Daylight::Normal(_) => {}
    }
    for interval in night_intervals(latitude_deg, longitude_deg, unix_seconds) {
        let night = (interval.end - interval.start) as f64;
        if night <= 0.0 || night > 86_400.0 {
            continue;
        }
        let smear = smear_hours_for(night / 3600.0) * 3600.0;
        let now = unix_seconds as f64;
        let start = interval.start as f64;
        let end = interval.end as f64;
        if now < start - smear || now >= end {
            continue;
        }
        if smear > 0.0 && now < start {
            let factor = 1.0 - (now - (start - smear)) / smear;
            return Some(lerp_temperature(NEUTRAL_TEMPERATURE_K, target_k, factor));
        }
        if smear > 0.0 && now >= end - smear {
            let factor = (now - (end - smear)) / smear;
            return Some(lerp_temperature(NEUTRAL_TEMPERATURE_K, target_k, factor));
        }
        return Some(target_k);
    }
    None
}

/// Daylight outcome for one UTC date at one location.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Daylight {
    /// Sunrise and sunset straddle the date normally.
    Normal(SunWindow),
    /// The sun never sets (polar day): no Night Light window.
    PolarDay,
    /// The sun never rises (polar night): Night Light all day.
    PolarNight,
}

/// Sunrise and sunset for one UTC date, in minutes since 00:00 UTC.
///
/// Each endpoint normalises to 0..1440; east of Greenwich sunrise can
/// read later than sunset (the daylight interval wraps midnight) and
/// the night-interval pairing handles that.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SunWindow {
    /// Sunrise, minutes since 00:00 UTC.
    pub sunrise_utc_min: f64,
    /// Sunset, minutes since 00:00 UTC.
    pub sunset_utc_min: f64,
}

/// Sunrise/sunset for the UTC date containing `unix_seconds`.
///
/// Standard low-precision solar math (the observatory approximation
/// behind NOAA's sunrise/sunset calculator): equation of time plus
/// declination from the day of year, then the hour angle for
/// [`SUN_ZENITH_DEG`]. Latitude clamps to the poles and longitude
/// wraps around the antimeridian. At the poles the hour-angle
/// denominator vanishes, so the day/night call falls back to comparing
/// the Sun's elevation against the zenith directly.
pub fn sun_window(latitude_deg: f64, longitude_deg: f64, unix_seconds: i64) -> Daylight {
    let lat = latitude_deg.clamp(-90.0, 90.0).to_radians();
    let lon = longitude_deg.rem_euclid(360.0);
    let lon = if lon > 180.0 { lon - 360.0 } else { lon };
    let day_of_year = f64::from(day_of_year(unix_seconds));

    let gamma = 2.0 * std::f64::consts::PI / 365.0 * (day_of_year - 1.0);
    let eqtime = 229.18
        * (0.000_075 + 0.001_868 * gamma.cos()
            - 0.032_077 * gamma.sin()
            - 0.014_615 * (2.0 * gamma).cos()
            - 0.040_849 * (2.0 * gamma).sin());
    let decl = 0.006_918 - 0.399_912 * gamma.cos() + 0.070_257 * gamma.sin()
        - 0.006_758 * (2.0 * gamma).cos()
        + 0.000_907 * (2.0 * gamma).sin()
        - 0.002_697 * (3.0 * gamma).cos()
        + 0.001_48 * (3.0 * gamma).sin();

    let cos_zenith = SUN_ZENITH_DEG.to_radians().cos();
    let numerator = cos_zenith - lat.sin() * decl.sin();
    let denominator = lat.cos() * decl.cos();
    if denominator.abs() < 1e-12 {
        // At (or vanishingly near) a pole the sun circles at a fixed
        // elevation: up all day when above the zenith, else polar night.
        return if lat.sin() * decl.sin() > cos_zenith {
            Daylight::PolarDay
        } else {
            Daylight::PolarNight
        };
    }
    let cos_hour_angle = numerator / denominator;
    if cos_hour_angle < -1.0 {
        return Daylight::PolarDay;
    }
    if cos_hour_angle > 1.0 {
        return Daylight::PolarNight;
    }
    let hour_angle_deg = cos_hour_angle.acos().to_degrees();
    let sunrise = (720.0 - 4.0 * (lon + hour_angle_deg) - eqtime).rem_euclid(MINUTES_PER_DAY);
    let sunset = (720.0 - 4.0 * (lon - hour_angle_deg) - eqtime).rem_euclid(MINUTES_PER_DAY);
    Daylight::Normal(SunWindow {
        sunrise_utc_min: sunrise,
        sunset_utc_min: sunset,
    })
}

/// 1-based day of year for the UTC date containing `unix_seconds`
/// (Howard Hinnant's days-from-civil round trip, leap-year aware).
fn day_of_year(unix_seconds: i64) -> u16 {
    let days = unix_seconds.div_euclid(86_400);
    let (year, _, _) = civil_from_days(days);
    let jan1 = days_from_civil(year, 1, 1);
    (days - jan1 + 1) as u16
}

/// Days since 1970-01-01 for a proleptic Gregorian date.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9).rem_euclid(12);
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Proleptic Gregorian (year, month, day) for days since 1970-01-01.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// The settings keys as read, before schedule resolution. Every
/// field falls back to its schema default when the key is missing,
/// so an older or partial schema still yields a usable config.
#[derive(Debug, Clone, PartialEq)]
pub struct RawSettings {
    /// `night-light-enabled`.
    pub enabled: bool,
    /// `night-light-temperature` (clamped on the way into the config).
    pub temperature_k: u32,
    /// `night-light-schedule-automatic`.
    pub automatic: bool,
    /// `night-light-schedule-from`, hours.
    pub from_hour: f64,
    /// `night-light-schedule-to`, hours.
    pub to_hour: f64,
    /// `night-light-last-coordinates`.
    pub latitude_deg: f64,
    /// `night-light-last-coordinates`.
    pub longitude_deg: f64,
}

impl RawSettings {
    /// The schema defaults: off, 2700 K, automatic, 20:00–06:00, no
    /// position yet.
    pub fn schema_defaults() -> Self {
        Self {
            enabled: false,
            temperature_k: DEFAULT_TEMPERATURE_K,
            automatic: true,
            from_hour: DEFAULT_FROM_HOUR,
            to_hour: DEFAULT_TO_HOUR,
            latitude_deg: INVALID_LATITUDE,
            longitude_deg: INVALID_LONGITUDE,
        }
    }

    /// Resolve to a config: automatic with valid coordinates keeps
    /// the sunset/sunrise schedule, otherwise (manual mode or the
    /// invalid sentinel) the fixed from/to window — g-s-d falls back
    /// to the manual settings exactly so.
    pub fn into_config(self) -> NightLightConfig {
        let automatic = self.automatic
            && (-90.0..=90.0).contains(&self.latitude_deg)
            && (-180.0..=180.0).contains(&self.longitude_deg);
        NightLightConfig {
            enabled: self.enabled,
            temperature_k: clamp_temperature(self.temperature_k),
            schedule: if automatic {
                NightLightSchedule::Automatic {
                    latitude_deg: self.latitude_deg,
                    longitude_deg: self.longitude_deg,
                }
            } else {
                NightLightSchedule::Fixed {
                    from_hour: self.from_hour,
                    to_hour: self.to_hour,
                }
            },
            // No key carries this; a refresh must not clear the live one.
            disabled_until_tmw: None,
        }
    }
}

/// A settings read failed: the caller keeps its previous config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadError(pub String);

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "night light settings read failed: {}", self.0)
    }
}

impl std::error::Error for ReadError {}

/// Parse `gsettings list-recursively org.gnome.settings-daemon.plugins.color`
/// output: one `<schema> <key> <value>` line per key. Unknown keys
/// are ignored; missing keys take [`RawSettings::schema_defaults`];
/// a malformed line or value fails the whole read (fail closed).
pub fn parse_list_recursively(text: &str) -> Result<RawSettings, ReadError> {
    let mut raw = RawSettings::schema_defaults();
    let malformed = |line: &str| ReadError(format!("malformed line: {line:?}"));
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.splitn(3, ' ');
        let (Some(schema), Some(key), Some(value)) = (parts.next(), parts.next(), parts.next())
        else {
            return Err(malformed(line));
        };
        if schema != COLOR_SCHEMA {
            return Err(ReadError(format!("unexpected schema: {schema:?}")));
        }
        let value = value.trim();
        match key {
            KEY_ENABLED => raw.enabled = parse_bool(value).ok_or_else(|| malformed(line))?,
            KEY_TEMPERATURE => {
                raw.temperature_k = parse_uint(value).ok_or_else(|| malformed(line))?;
            }
            KEY_SCHEDULE_AUTOMATIC => {
                raw.automatic = parse_bool(value).ok_or_else(|| malformed(line))?;
            }
            KEY_SCHEDULE_FROM => {
                raw.from_hour = value.parse::<f64>().map_err(|_| malformed(line))?;
            }
            KEY_SCHEDULE_TO => {
                raw.to_hour = value.parse::<f64>().map_err(|_| malformed(line))?;
            }
            KEY_LAST_COORDINATES => {
                (raw.latitude_deg, raw.longitude_deg) =
                    parse_coordinates(value).ok_or_else(|| malformed(line))?;
            }
            _ => {}
        }
    }
    Ok(raw)
}

/// Parse a GSettings boolean (`true`/`false`).
fn parse_bool(value: &str) -> Option<bool> {
    match value {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

/// Parse a GSettings uint32, with or without an explicit `uint32` tag.
fn parse_uint(value: &str) -> Option<u32> {
    value
        .strip_prefix("uint32 ")
        .unwrap_or(value)
        .trim()
        .parse::<u32>()
        .ok()
}

/// Parse a `(latitude, longitude)` GVariant tuple.
fn parse_coordinates(value: &str) -> Option<(f64, f64)> {
    let inner = value.strip_prefix('(')?.strip_suffix(')')?;
    let mut parts = inner.split(',');
    let latitude = parts.next()?.trim().parse::<f64>().ok()?;
    let longitude = parts.next()?.trim().parse::<f64>().ok()?;
    if parts.next().is_some() {
        return None;
    }
    if !latitude.is_finite() || !longitude.is_finite() {
        return None;
    }
    Some((latitude, longitude))
}

/// Read the settings through `run` (usually a `gsettings` call) and
/// resolve them to a config.
pub fn load_from_reader(
    run: impl FnOnce() -> Result<String, ReadError>,
) -> Result<NightLightConfig, ReadError> {
    parse_list_recursively(&run()?).map(RawSettings::into_config)
}

/// Production settings reader: one `gsettings list-recursively`
/// call over [`COLOR_SCHEMA`]. Reads hit the local dconf database;
/// a missing schema, a missing binary or unparsable output all fail
/// closed (the caller keeps its previous config).
pub fn load_from_gsettings() -> Result<NightLightConfig, ReadError> {
    load_from_reader(|| {
        let output = std::process::Command::new("gsettings")
            .args(["list-recursively", COLOR_SCHEMA])
            .output()
            .map_err(|e| ReadError(format!("cannot run gsettings: {e}")))?;
        if !output.status.success() {
            return Err(ReadError(format!(
                "gsettings exited with {}",
                output.status
            )));
        }
        String::from_utf8(output.stdout)
            .map_err(|_| ReadError("gsettings output is not UTF-8".to_owned()))
    })
}

/// Live state shared between the compositor loop (which owns the
/// [`NightLightController`]) and the D-Bus thread (which serves
/// `org.gnome.SettingsDaemon.Color`): the loop publishes what it
/// applies, the bus side publishes "disabled until tomorrow".
pub struct NightLightShare {
    /// Whether the filter applies right now (`NightLightActive`).
    active: std::sync::atomic::AtomicBool,
    /// Smeared temperature currently applied, or
    /// [`NEUTRAL_TEMPERATURE_K`] when inactive (`Temperature`).
    temperature: std::sync::atomic::AtomicU32,
    /// Unix seconds when `DisabledUntilTomorrow` was set, if set.
    disabled_until_tmw: std::sync::Mutex<Option<i64>>,
}

impl Default for NightLightShare {
    /// Inactive, neutral, no temp-disable.
    fn default() -> Self {
        Self {
            active: std::sync::atomic::AtomicBool::new(false),
            temperature: std::sync::atomic::AtomicU32::new(NEUTRAL_TEMPERATURE_K),
            disabled_until_tmw: std::sync::Mutex::new(None),
        }
    }
}

impl NightLightShare {
    /// Read `NightLightActive`.
    pub fn active(&self) -> bool {
        self.active.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Read `Temperature`.
    pub fn temperature(&self) -> u32 {
        self.temperature.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Read `DisabledUntilTomorrow` (whether a disable is recorded;
    /// the loop decides whether it still suppresses).
    pub fn disabled_until_tmw(&self) -> Option<i64> {
        self.disabled_until_tmw.lock().ok().and_then(|set| *set)
    }

    /// Publish what the loop applies.
    pub fn publish(&self, active: bool, temperature_k: u32) {
        self.active
            .store(active, std::sync::atomic::Ordering::SeqCst);
        self.temperature
            .store(temperature_k, std::sync::atomic::Ordering::SeqCst);
    }

    /// Write `DisabledUntilTomorrow` from the D-Bus side: `Some(now)`
    /// when set, `None` when cleared.
    pub fn set_disabled_until_tmw(&self, set_at: Option<i64>) {
        if let Ok(mut set) = self.disabled_until_tmw.lock() {
            *set = set_at;
        }
    }
}

/// Per-tick Night Light evaluation: settings refresh, schedule,
/// smear, temp-disable and the 10 K hysteresis.
///
/// The override installed from [`NightLightController::tick`] lives
/// for one frame only, so neutral restore needs no cleanup: when the
/// filter is off the tick yields `None` and the program is never
/// installed; when the session ends the GL context — and with it any
/// installed override — dies with the process.
pub struct NightLightController {
    config: NightLightConfig,
    share: std::sync::Arc<NightLightShare>,
    last_read: Option<std::time::Instant>,
    applied_temperature: Option<u32>,
    applied_gains: Option<ColorGains>,
}

impl NightLightController {
    /// Fresh controller: default config, nothing applied.
    pub fn new(share: std::sync::Arc<NightLightShare>) -> Self {
        Self {
            config: NightLightConfig::default(),
            share,
            last_read: None,
            applied_temperature: None,
            applied_gains: None,
        }
    }

    /// The config driving the last tick.
    pub fn config(&self) -> &NightLightConfig {
        &self.config
    }

    /// Gains from the last tick (`None` means no override).
    pub fn current_gains(&self) -> Option<ColorGains> {
        self.applied_gains
    }

    /// Re-read the settings when [`READ_INTERVAL`] elapsed since the
    /// last read (always on the first call). A successful read
    /// replaces the config but preserves the live "disabled until
    /// tomorrow"; a failed read keeps everything. Returns whether
    /// the config changed.
    pub fn maybe_refresh(
        &mut self,
        now: std::time::Instant,
        load: impl FnOnce() -> Result<NightLightConfig, ReadError>,
    ) -> bool {
        if let Some(last) = self.last_read {
            if now.duration_since(last) < READ_INTERVAL {
                return false;
            }
        }
        self.last_read = Some(now);
        let Ok(mut config) = load() else {
            return false;
        };
        config.disabled_until_tmw = self.config.disabled_until_tmw;
        if config == self.config {
            return false;
        }
        self.config = config;
        true
    }

    /// Evaluate the filter at `unix_seconds` and publish the result
    /// to the share. Returns the gains to install, or `None` for a
    /// neutral frame. Unity gains (inactive, disabled, or exactly
    /// [`NEUTRAL_TEMPERATURE_K`]) also yield `None`: like mutter, we
    /// never program an identity transform.
    pub fn tick(&mut self, unix_seconds: i64) -> Option<ColorGains> {
        self.config.disabled_until_tmw = self.share.disabled_until_tmw();
        let target = self.config.active_temperature_at(unix_seconds);
        let active = target.is_some();
        // 10 K hysteresis: hold the applied temperature across small
        // smear steps; any transition through inactive republishes.
        let temperature = match (target, self.applied_temperature) {
            (Some(next), Some(current))
                if (next as i32 - current as i32).abs() <= TEMPERATURE_QUANTUM_K as i32 =>
            {
                Some(current)
            }
            (next, _) => next,
        };
        self.applied_temperature = temperature;
        let gains = temperature
            .filter(|kelvin| *kelvin != NEUTRAL_TEMPERATURE_K)
            .map(temperature_to_gains);
        self.applied_gains = gains;
        self.share
            .publish(active, temperature.unwrap_or(NEUTRAL_TEMPERATURE_K));
        gains
    }

    /// The last applied gains as `f32` bits (unity when neutral):
    /// appended to the frame signature so a temperature step
    /// repaints, while an unchanged filter sleeps.
    pub fn effect_bits(&self) -> [u32; 3] {
        let [r, g, b] = self.applied_gains.map(gains_f32).unwrap_or([1.0, 1.0, 1.0]);
        [r.to_bits(), g.to_bits(), b.to_bits()]
    }
}

/// Gains as `f32` for the GL uniform.
pub fn gains_f32(gains: ColorGains) -> [f32; 3] {
    [gains.red as f32, gains.green as f32, gains.blue as f32]
}

/// Multiply an sRGB triple (accent, clear colour) by the gains.
pub fn apply_gains(rgb: [f32; 3], gains: ColorGains) -> [f32; 3] {
    let [r, g, b] = gains_f32(gains);
    [rgb[0] * r, rgb[1] * g, rgb[2] * b]
}

// Own shader follows Smithay's default formats and premultiplied alpha,
// like the modal-dim one: it multiplies RGB by the per-element
// brightness and the output-wide night-light gains.
const TINT_SHADER: &str = r#"#version 100
//_DEFINES_
#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif
precision mediump float;
#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif
uniform float alpha;
uniform float brightness;
uniform vec3 nl_gains;
varying vec2 v_coords;
#if defined(DEBUG_FLAGS)
uniform float tint;
#endif
void main() {
    vec4 color = texture2D(tex, v_coords);
#if defined(NO_ALPHA)
    color = vec4(color.rgb, 1.0) * alpha;
#else
    color = color * alpha;
#endif
    color.rgb *= brightness;
    color.rgb *= nl_gains;
#if defined(DEBUG_FLAGS)
    if (tint == 1.0) color = vec4(0.0,0.2,0.0,0.2) + color * 0.8;
#endif
    gl_FragColor = color;
}"#;

struct TintProgram(Option<smithay::backend::renderer::gles::GlesTexProgram>);

/// Compile (once per renderer, then cached) the texture program
/// that applies the night-light gains. `None` when the shader
/// fails to compile: the caller draws untinted.
pub fn tint_program(
    renderer: &mut smithay::backend::renderer::gles::GlesRenderer,
) -> Option<smithay::backend::renderer::gles::GlesTexProgram> {
    use smithay::backend::renderer::gles::{UniformName, UniformType};
    if let Some(p) = renderer.egl_context().user_data().get::<TintProgram>() {
        return p.0.clone();
    }
    let p = renderer
        .compile_custom_texture_shader(
            TINT_SHADER,
            &[
                UniformName::new("brightness", UniformType::_1f),
                UniformName::new("nl_gains", UniformType::_3f),
            ],
        )
        .map_err(|e| eprintln!("tuna-compositor: night light shader unavailable: {e}"))
        .ok();
    renderer
        .egl_context()
        .user_data()
        .insert_if_missing(|| TintProgram(p.clone()));
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 00:00 UTC on a Gregorian date, as a Unix timestamp.
    fn midnight(year: i64, month: i64, day: i64) -> i64 {
        days_from_civil(year, month, day) * 86_400
    }

    #[test]
    fn temperature_clamps_to_range() {
        assert_eq!(clamp_temperature(0), MIN_TEMPERATURE_K);
        assert_eq!(clamp_temperature(999), MIN_TEMPERATURE_K);
        assert_eq!(clamp_temperature(1000), 1000);
        assert_eq!(clamp_temperature(4500), 4500);
        assert_eq!(clamp_temperature(6500), 6500);
        assert_eq!(clamp_temperature(10000), 10000);
        assert_eq!(clamp_temperature(10001), MAX_TEMPERATURE_K);
        assert_eq!(clamp_temperature(u32::MAX), MAX_TEMPERATURE_K);
    }

    #[test]
    fn neutral_temperature_is_unity() {
        let gains = temperature_to_gains(NEUTRAL_TEMPERATURE_K);
        assert!((gains.red - 1.0).abs() < 1e-12);
        assert!((gains.green - 1.0).abs() < 1e-12);
        assert!((gains.blue - 1.0).abs() < 1e-12);
    }

    #[test]
    fn coldest_boundary_keeps_red_kills_blue() {
        let gains = temperature_to_gains(MIN_TEMPERATURE_K);
        assert_eq!(gains.red, 1.0);
        assert_eq!(gains.blue, 0.0);
        assert!(
            gains.green > 0.2 && gains.green < 0.35,
            "green={}",
            gains.green
        );
    }

    #[test]
    fn hottest_boundary_keeps_blue_dims_red() {
        let gains = temperature_to_gains(MAX_TEMPERATURE_K);
        assert_eq!(gains.blue, 1.0);
        assert!(gains.red < gains.green, "{gains:?}");
        assert!(gains.green < 1.0, "{gains:?}");
    }

    #[test]
    fn gains_stay_bounded_and_monotonic() {
        let mut previous = temperature_to_gains(MIN_TEMPERATURE_K);
        let mut kelvin = MIN_TEMPERATURE_K + 500;
        while kelvin <= MAX_TEMPERATURE_K {
            let gains = temperature_to_gains(kelvin);
            for channel in [gains.red, gains.green, gains.blue] {
                assert!((0.0..=1.0).contains(&channel), "{kelvin}K: {gains:?}");
            }
            assert!(gains.blue >= previous.blue - 1e-12, "{kelvin}K: {gains:?}");
            assert!(gains.red <= previous.red + 1e-12, "{kelvin}K: {gains:?}");
            previous = gains;
            kelvin += 500;
        }
    }

    #[test]
    fn manual_follows_the_switch() {
        let mut config = NightLightConfig::default();
        assert!(!config.is_active_at(midnight(2026, 3, 20)));
        config.enabled = true;
        assert!(config.is_active_at(midnight(2026, 3, 20)));
        assert!(config.is_active_at(midnight(2026, 3, 20) + 12 * 3600));
        assert!(config.active_gains_at(midnight(2026, 3, 20)).is_some());
        config.enabled = false;
        assert!(config.active_gains_at(midnight(2026, 3, 20)).is_none());
    }

    #[test]
    fn equinox_day_is_roughly_six_to_six_utc() {
        let Daylight::Normal(window) = sun_window(0.0, 0.0, midnight(2026, 3, 20)) else {
            panic!("equator equinox must be a normal day");
        };
        assert!((window.sunrise_utc_min - 360.0).abs() < 20.0, "{window:?}");
        assert!((window.sunset_utc_min - 1080.0).abs() < 20.0, "{window:?}");
        let auto = NightLightConfig {
            enabled: true,
            temperature_k: DEFAULT_TEMPERATURE_K,
            disabled_until_tmw: None,
            schedule: NightLightSchedule::Automatic {
                latitude_deg: 0.0,
                longitude_deg: 0.0,
            },
        };
        let day = midnight(2026, 3, 20);
        assert!(auto.is_active_at(day));
        assert!(!auto.is_active_at(day + 12 * 3600));
        assert!(auto.is_active_at(day + 23 * 3600));
    }

    #[test]
    fn polar_day_is_inactive_and_polar_night_is_active() {
        let joue = midnight(2026, 6, 21);
        assert_eq!(sun_window(78.2, 15.6, joue), Daylight::PolarDay);
        let noel = midnight(2026, 12, 21);
        assert_eq!(sun_window(78.2, 15.6, noel), Daylight::PolarNight);
        // Southern winter mirrors northern summer.
        assert_eq!(sun_window(-78.2, 15.6, joue), Daylight::PolarNight);
        let mut auto = NightLightConfig {
            enabled: true,
            temperature_k: DEFAULT_TEMPERATURE_K,
            disabled_until_tmw: None,
            schedule: NightLightSchedule::Automatic {
                latitude_deg: 78.2,
                longitude_deg: 15.6,
            },
        };
        assert!(!auto.is_active_at(joue + 12 * 3600));
        assert!(auto.is_active_at(noel + 12 * 3600));
        // The poles themselves take the elevation fallback, not NaN.
        assert_eq!(sun_window(90.0, 0.0, joue), Daylight::PolarDay);
        assert_eq!(sun_window(-90.0, 0.0, joue), Daylight::PolarNight);
        auto.schedule = NightLightSchedule::Automatic {
            latitude_deg: 90.0,
            longitude_deg: 0.0,
        };
        assert!(!auto.is_active_at(joue));
    }

    #[test]
    fn wrapped_daylight_interval_stays_correct() {
        // Near the antimeridian the UTC daylight interval wraps
        // midnight; the night test must still follow local darkness.
        let day = midnight(2026, 3, 20);
        let Daylight::Normal(window) = sun_window(-36.8, 170.0, day) else {
            panic!("Auckland equinox must be a normal day");
        };
        assert!(window.sunrise_utc_min > window.sunset_utc_min, "{window:?}");
        let auto = NightLightConfig {
            enabled: true,
            temperature_k: DEFAULT_TEMPERATURE_K,
            disabled_until_tmw: None,
            schedule: NightLightSchedule::Automatic {
                latitude_deg: -36.8,
                longitude_deg: 170.0,
            },
        };
        assert!(auto.is_active_at(day + 12 * 3600));
        assert!(!auto.is_active_at(day + 2 * 3600));
    }

    #[test]
    fn fixed_window_handles_wrap_and_full_day() {
        let night = NightLightConfig {
            enabled: true,
            temperature_k: DEFAULT_TEMPERATURE_K,
            disabled_until_tmw: None,
            schedule: NightLightSchedule::Fixed {
                from_hour: 22.0,
                to_hour: 6.0,
            },
        };
        let day = midnight(2026, 3, 20);
        assert!(night.is_active_at(day + 23 * 3600));
        assert!(night.is_active_at(day + 5 * 3600));
        assert!(!night.is_active_at(day + 12 * 3600));
        assert!(!night.is_active_at(day + 6 * 3600 + 60));
        let evening = NightLightConfig {
            schedule: NightLightSchedule::Fixed {
                from_hour: 20.0,
                to_hour: 23.0,
            },
            ..night.clone()
        };
        assert!(evening.is_active_at(day + 21 * 3600));
        // 19:00 is inside the one-hour smear lead-in (active, but at
        // the neutral temperature); 18:59 is fully outside.
        assert!(evening.is_active_at(day + 19 * 3600));
        assert_eq!(
            evening.active_temperature_at(day + 19 * 3600),
            Some(NEUTRAL_TEMPERATURE_K)
        );
        assert!(!evening.is_active_at(day + 19 * 3600 - 60));
        let full_day = NightLightConfig {
            schedule: NightLightSchedule::Fixed {
                from_hour: 6.0,
                to_hour: 6.0,
            },
            ..night.clone()
        };
        // `from == to` reads as the full day, matching GNOME's
        // `frac_day_is_between` (equal bounds always match).
        assert!(full_day.is_active_at(day + 6 * 3600));
        assert!(full_day.is_active_at(day + 18 * 3600));
        let mut off = night;
        off.enabled = false;
        assert!(!off.is_active_at(day + 23 * 3600));
    }

    #[test]
    fn schedule_is_dst_agnostic() {
        // Europe springs forward on 2026-03-29; UTC solar math must
        // not jump an hour across it.
        let before = midnight(2026, 3, 28);
        let after = midnight(2026, 3, 29);
        let Daylight::Normal(a) = sun_window(51.5, 0.0, before) else {
            panic!("London in March must be a normal day");
        };
        let Daylight::Normal(b) = sun_window(51.5, 0.0, after) else {
            panic!("London in March must be a normal day");
        };
        assert!(
            (a.sunrise_utc_min - b.sunrise_utc_min).abs() < 10.0,
            "{a:?} {b:?}"
        );
        assert!(
            (a.sunset_utc_min - b.sunset_utc_min).abs() < 10.0,
            "{a:?} {b:?}"
        );
        // Same UTC time-of-day, same answer on both sides.
        let fixed = NightLightConfig {
            enabled: true,
            temperature_k: DEFAULT_TEMPERATURE_K,
            disabled_until_tmw: None,
            schedule: NightLightSchedule::Fixed {
                from_hour: 22.0,
                to_hour: 6.0,
            },
        };
        assert_eq!(
            fixed.is_active_at(before + 23 * 3600),
            fixed.is_active_at(after + 23 * 3600)
        );
    }

    #[test]
    fn color_scheme_round_trips_and_rejects_unknown() {
        for (text, scheme) in [
            ("default", ColorScheme::Default),
            ("prefer-dark", ColorScheme::PreferDark),
            ("prefer-light", ColorScheme::PreferLight),
        ] {
            assert_eq!(ColorScheme::parse(text), Some(scheme));
            assert_eq!(scheme.as_gsettings(), text);
        }
        assert_eq!(ColorScheme::parse("prefer-dark "), None);
        assert_eq!(ColorScheme::parse("dark"), None);
        assert_eq!(ColorScheme::parse(""), None);
    }

    #[test]
    fn civil_round_trip_covers_leap_day() {
        assert_eq!(civil_from_days(days_from_civil(2026, 3, 20)), (2026, 3, 20));
        assert_eq!(civil_from_days(days_from_civil(2024, 2, 29)), (2024, 2, 29));
        assert_eq!(
            civil_from_days(days_from_civil(1969, 12, 31)),
            (1969, 12, 31)
        );
        assert_eq!(day_of_year(midnight(2024, 2, 29)), 60);
        assert_eq!(day_of_year(midnight(2024, 12, 31)), 366);
        assert_eq!(day_of_year(midnight(2026, 1, 1)), 1);
    }

    /// Enabled fixed-window config at the requested temperature.
    fn fixed_at(from_hour: f64, to_hour: f64, temperature_k: u32) -> NightLightConfig {
        NightLightConfig {
            enabled: true,
            temperature_k,
            disabled_until_tmw: None,
            schedule: NightLightSchedule::Fixed { from_hour, to_hour },
        }
    }

    #[test]
    fn defaults_match_the_gnome_schema() {
        assert_eq!(DEFAULT_TEMPERATURE_K, 2700);
        assert_eq!(DEFAULT_FROM_HOUR, 20.0);
        assert_eq!(DEFAULT_TO_HOUR, 6.0);
        let raw = RawSettings::schema_defaults();
        assert!(!raw.enabled);
        assert!(raw.automatic);
        // The sentinel is out of range on both axes, so resolution
        // falls back to the fixed window.
        assert_eq!(
            raw.into_config().schedule,
            NightLightSchedule::Fixed {
                from_hour: DEFAULT_FROM_HOUR,
                to_hour: DEFAULT_TO_HOUR,
            }
        );
    }

    #[test]
    fn fixed_edges_smear_over_one_hour() {
        let night = fixed_at(22.0, 6.0, 2700);
        let day = midnight(2026, 3, 20);
        // Before the lead-in: fully off. At its exact start the
        // filter is active but still neutral.
        assert_eq!(night.active_temperature_at(day + 21 * 3600 - 60), None);
        assert!(!night.is_active_at(day + 21 * 3600 - 60));
        assert_eq!(
            night.active_temperature_at(day + 21 * 3600),
            Some(NEUTRAL_TEMPERATURE_K)
        );
        // Halfway through the lead-in: halfway between 6500 K and 2700 K.
        assert_eq!(
            night.active_temperature_at(day + 21 * 3600 + 1800),
            Some(4600)
        );
        // At `from`: the requested temperature.
        assert_eq!(night.active_temperature_at(day + 22 * 3600), Some(2700));
        // Mid-window: full temperature.
        assert_eq!(night.active_temperature_at(day + 2 * 3600), Some(2700));
        // An hour before `to` the trail-off starts; at `to` it ends.
        assert_eq!(night.active_temperature_at(day + 5 * 3600), Some(2700));
        let trailing = night.active_temperature_at(day + 5 * 3600 + 3540).unwrap();
        assert!((2700..6500).contains(&trailing), "trailing={trailing}");
        assert_eq!(night.active_temperature_at(day + 6 * 3600), None);
    }

    #[test]
    fn smear_clamps_to_short_windows() {
        // A 30-minute night: the smear clamps to 30 minutes, so the
        // lead-in starts at 21:30, not 21:00.
        let short = fixed_at(22.0, 22.5, 2700);
        let day = midnight(2026, 3, 20);
        assert_eq!(short.active_temperature_at(day + 21 * 3600 + 1740), None);
        let leading = short.active_temperature_at(day + 21 * 3600 + 1860).unwrap();
        assert!((2700..=6500).contains(&leading), "leading={leading}");
        assert_eq!(short.active_temperature_at(day + 22 * 3600), Some(2700));
        assert_eq!(short.active_temperature_at(day + 22 * 3600 + 1800), None);
        // A full-day window has no edges and no smear.
        let full = fixed_at(6.0, 6.0, 2700);
        assert_eq!(full.active_temperature_at(day + 12 * 3600), Some(2700));
    }

    #[test]
    fn automatic_edges_smear_around_sunset_and_sunrise() {
        let auto = NightLightConfig {
            enabled: true,
            temperature_k: 2700,
            disabled_until_tmw: None,
            schedule: NightLightSchedule::Automatic {
                latitude_deg: 0.0,
                longitude_deg: 0.0,
            },
        };
        let day = midnight(2026, 3, 20);
        let Daylight::Normal(window) = sun_window(0.0, 0.0, day) else {
            panic!("equator equinox must be a normal day");
        };
        let sunset = day + (window.sunset_utc_min * 60.0).round() as i64;
        let sunrise = day + (window.sunrise_utc_min * 60.0).round() as i64;
        // Two hours before sunset: off. Half an hour in: ramping.
        assert_eq!(auto.active_temperature_at(sunset - 2 * 3600), None);
        let leading = auto.active_temperature_at(sunset - 1800).unwrap();
        assert!((2700..6500).contains(&leading), "leading={leading}");
        // An hour after sunset: full temperature.
        assert_eq!(auto.active_temperature_at(sunset + 3600), Some(2700));
        // Half an hour before sunrise: ramping back up.
        let trailing = auto.active_temperature_at(sunrise - 1800).unwrap();
        assert!((2700..6500).contains(&trailing), "trailing={trailing}");
        assert_eq!(auto.active_temperature_at(sunrise), None);
    }

    #[test]
    fn temp_disable_suppresses_until_sunrise_or_24h() {
        let day = midnight(2026, 3, 20);
        // Fixed 22:00-06:00, disabled at 23:00.
        let mut night = fixed_at(22.0, 6.0, 2700);
        night.disabled_until_tmw = Some(day + 23 * 3600);
        // Still night, no sunrise since: suppressed.
        assert!(!night.is_active_at(day + 26 * 3600));
        // Past the 06:00 sunrise a new night starts unsuppressed.
        assert!(night.is_active_at(day + 47 * 3600));
        // A clock step back never clears the disable.
        assert!(!night.is_active_at(day + 22 * 3600));
        // Manual has no sunrise: the 24 h lapse alone clears it.
        let mut manual = NightLightConfig {
            enabled: true,
            ..NightLightConfig::default()
        };
        let set_at = day + 12 * 3600;
        manual.disabled_until_tmw = Some(set_at);
        assert!(!manual.is_active_at(set_at + 3600));
        assert!(!manual.is_active_at(set_at + 86_400));
        assert!(manual.is_active_at(set_at + 86_401));
    }

    #[test]
    fn temp_disable_sunrise_reset_needs_a_sunrise_between() {
        let day = midnight(2026, 3, 20);
        // Disabled at 05:00, inside the window: 05:30 sees no
        // sunrise since 05:00, so it stays suppressed.
        let mut night = fixed_at(22.0, 6.0, 2700);
        night.disabled_until_tmw = Some(day + 5 * 3600);
        assert!(!night.is_active_at(day + 5 * 3600 + 1800));
        // Disabled crossing a polar night (no sunrise anywhere):
        // only the 24 h lapse clears it.
        let mut polar = NightLightConfig {
            enabled: true,
            temperature_k: DEFAULT_TEMPERATURE_K,
            disabled_until_tmw: None,
            schedule: NightLightSchedule::Automatic {
                latitude_deg: 78.2,
                longitude_deg: 15.6,
            },
        };
        let noel = midnight(2026, 12, 21);
        polar.disabled_until_tmw = Some(noel + 12 * 3600);
        assert!(!polar.is_active_at(noel + 20 * 3600));
        assert!(polar.is_active_at(noel + 12 * 3600 + 86_401));
    }

    /// `gsettings list-recursively` output over the color schema.
    fn list_recursively() -> String {
        format!(
            "\
{COLOR_SCHEMA} night-light-enabled true
{COLOR_SCHEMA} night-light-temperature uint32 3000
{COLOR_SCHEMA} night-light-schedule-automatic false
{COLOR_SCHEMA} night-light-schedule-from 21.5
{COLOR_SCHEMA} night-light-schedule-to 6.25
{COLOR_SCHEMA} night-light-last-coordinates (91.0, 181.0)
{COLOR_SCHEMA} recalibrate-display-threshold uint32 0
"
        )
    }

    #[test]
    fn reader_parses_list_recursively_and_ignores_unknown_keys() {
        let raw = parse_list_recursively(&list_recursively()).unwrap();
        assert!(raw.enabled);
        assert_eq!(raw.temperature_k, 3000);
        assert!(!raw.automatic);
        assert_eq!(raw.from_hour, 21.5);
        assert_eq!(raw.to_hour, 6.25);
        let config = raw.into_config();
        assert!(config.enabled);
        assert_eq!(config.temperature_k(), 3000);
        assert_eq!(
            config.schedule,
            NightLightSchedule::Fixed {
                from_hour: 21.5,
                to_hour: 6.25,
            }
        );
        assert_eq!(config.disabled_until_tmw, None);
    }

    #[test]
    fn reader_falls_back_to_schema_defaults_and_fixed_window() {
        // Empty output: every key missing, all schema defaults.
        let empty = parse_list_recursively("").unwrap();
        assert_eq!(empty, RawSettings::schema_defaults());
        // Automatic with the invalid sentinel falls back to fixed.
        let sentinel = parse_list_recursively(&format!(
            "{COLOR_SCHEMA} night-light-enabled true\n\
             {COLOR_SCHEMA} night-light-schedule-automatic true\n"
        ))
        .unwrap()
        .into_config();
        assert_eq!(
            sentinel.schedule,
            NightLightSchedule::Fixed {
                from_hour: DEFAULT_FROM_HOUR,
                to_hour: DEFAULT_TO_HOUR,
            }
        );
        // Automatic with real coordinates keeps the solar schedule.
        let positioned = parse_list_recursively(&format!(
            "{COLOR_SCHEMA} night-light-enabled true\n\
             {COLOR_SCHEMA} night-light-schedule-automatic true\n\
             {COLOR_SCHEMA} night-light-last-coordinates (51.5, -0.12)\n"
        ))
        .unwrap()
        .into_config();
        assert_eq!(
            positioned.schedule,
            NightLightSchedule::Automatic {
                latitude_deg: 51.5,
                longitude_deg: -0.12,
            }
        );
    }

    #[test]
    fn reader_fails_closed_on_malformed_output() {
        for bad in [
            "night-light-enabled true\n",
            &format!("{COLOR_SCHEMA} night-light-enabled yes\n"),
            &format!("{COLOR_SCHEMA} night-light-temperature warm\n"),
            &format!("{COLOR_SCHEMA} night-light-schedule-from noon\n"),
            &format!("{COLOR_SCHEMA} night-light-last-coordinates (51.5,)\n"),
            &format!("{COLOR_SCHEMA} night-light-last-coordinates 51.5\n"),
            "org.gnome.desktop.interface color-scheme 'prefer-dark'\n",
        ] {
            assert!(parse_list_recursively(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn controller_holds_small_steps_and_republishes_transitions() {
        use std::sync::Arc;
        let share = Arc::new(NightLightShare::default());
        let mut controller = NightLightController::new(share.clone());
        // Seed an enabled fixed config directly: refresh tests cover
        // the reader path below.
        controller.config = fixed_at(22.0, 6.0, 2700);
        let day = midnight(2026, 3, 20);
        // Mid-window: full temperature, gains installed, published.
        let gains = controller.tick(day + 2 * 3600).unwrap();
        assert_eq!(gains, temperature_to_gains(2700));
        assert!(share.active());
        assert_eq!(share.temperature(), 2700);
        // A 10 K smear step holds the applied temperature.
        controller.config.set_temperature_k(2710);
        assert_eq!(controller.tick(day + 2 * 3600), Some(gains));
        assert_eq!(share.temperature(), 2700);
        // An 11 K step republishes.
        controller.config.set_temperature_k(2711);
        let stepped = controller.tick(day + 2 * 3600).unwrap();
        assert_eq!(stepped, temperature_to_gains(2711));
        assert_eq!(share.temperature(), 2711);
        // Leaving the window clears everything (neutral restore).
        assert_eq!(controller.tick(day + 12 * 3600), None);
        assert!(!share.active());
        assert_eq!(share.temperature(), NEUTRAL_TEMPERATURE_K);
        assert_eq!(controller.effect_bits(), [1.0f32.to_bits(); 3]);
        // Re-entering republishes even at a small delta: the trip
        // through inactive reset the hysteresis anchor.
        controller.config.set_temperature_k(2705);
        let reentered = controller.tick(day + 26 * 3600).unwrap();
        assert_eq!(reentered, temperature_to_gains(2705));
    }

    #[test]
    fn controller_never_programs_identity() {
        use std::sync::Arc;
        let share = Arc::new(NightLightShare::default());
        let mut controller = NightLightController::new(share.clone());
        controller.config = fixed_at(22.0, 6.0, NEUTRAL_TEMPERATURE_K);
        let day = midnight(2026, 3, 20);
        // Active at the neutral temperature: published as active,
        // but no override to install.
        assert_eq!(controller.tick(day + 2 * 3600), None);
        assert!(share.active());
        assert_eq!(share.temperature(), NEUTRAL_TEMPERATURE_K);
        assert_eq!(controller.effect_bits(), [1.0f32.to_bits(); 3]);
        // A fresh controller starts neutral too.
        let fresh = NightLightController::new(Arc::new(NightLightShare::default()));
        assert_eq!(fresh.current_gains(), None);
        assert_eq!(fresh.effect_bits(), [1.0f32.to_bits(); 3]);
    }

    #[test]
    fn controller_refresh_preserves_temp_disable_and_fails_closed() {
        use std::sync::Arc;
        use std::time::{Duration, Instant};
        let share = Arc::new(NightLightShare::default());
        let mut controller = NightLightController::new(share.clone());
        let now = Instant::now();
        let fixed = fixed_at(22.0, 6.0, 3000);
        // First call always reads.
        assert!(controller.maybe_refresh(now, || Ok::<_, ReadError>(fixed.clone())));
        assert_eq!(controller.config().temperature_k(), 3000);
        // Within the interval: no re-read, no change.
        assert!(
            !controller.maybe_refresh(now + Duration::from_secs(1), || Ok::<_, ReadError>(
                fixed_at(22.0, 6.0, 4000)
            ),)
        );
        assert_eq!(controller.config().temperature_k(), 3000);
        // A temp-disable survives a successful refresh...
        controller.config.disabled_until_tmw = Some(1_700_000_000);
        assert!(
            controller.maybe_refresh(now + READ_INTERVAL, || Ok::<_, ReadError>(fixed_at(
                22.0, 6.0, 4000
            )),)
        );
        assert_eq!(controller.config().temperature_k(), 4000);
        assert_eq!(controller.config().disabled_until_tmw, Some(1_700_000_000));
        // ...and a failed read keeps the whole config.
        assert!(
            !controller.maybe_refresh(now + 2 * READ_INTERVAL, || Err::<NightLightConfig, _>(
                ReadError("dconf gone".to_owned())
            ),)
        );
        assert_eq!(controller.config().temperature_k(), 4000);
    }

    #[test]
    fn controller_follows_the_shared_temp_disable() {
        use std::sync::Arc;
        let share = Arc::new(NightLightShare::default());
        let mut controller = NightLightController::new(share.clone());
        controller.config = fixed_at(22.0, 6.0, 2700);
        let day = midnight(2026, 3, 20);
        assert!(controller.tick(day + 2 * 3600).is_some());
        // The D-Bus side records a disable: the next tick suppresses.
        share.set_disabled_until_tmw(Some(day + 2 * 3600));
        assert_eq!(controller.tick(day + 3 * 3600), None);
        assert!(!share.active());
        // Clearing re-arms the filter.
        share.set_disabled_until_tmw(None);
        assert!(controller.tick(day + 3 * 3600).is_some());
    }

    #[test]
    fn apply_gains_multiplies_channels() {
        let warm = temperature_to_gains(2700);
        let [r, g, b] = apply_gains([1.0, 1.0, 1.0], warm);
        assert_eq!([r, g, b], gains_f32(warm));
        assert!(r > g && g > b, "{r} {g} {b}");
        let neutral = apply_gains([0.5, 0.25, 1.0], temperature_to_gains(6500));
        assert_eq!(neutral, [0.5, 0.25, 1.0]);
    }

    /// Headless GL harness: surfaceless EGL, or `None` when this
    /// machine has no EGL (the test skips instead of failing).
    #[cfg(feature = "drm")]
    fn headless_renderer() -> Option<smithay::backend::renderer::gles::GlesRenderer> {
        use smithay::backend::egl::native::EGLSurfacelessDisplay;
        use smithay::backend::egl::{EGLContext, EGLDisplay};
        let display = unsafe { EGLDisplay::new(EGLSurfacelessDisplay) }.ok()?;
        let context = EGLContext::new(&display).ok()?;
        unsafe { smithay::backend::renderer::gles::GlesRenderer::new(context) }.ok()
    }

    /// Draw a fullscreen 2x2 white texture through `program` (or the
    /// default one) and read back the first pixel (RGBA in memory).
    #[cfg(feature = "drm")]
    fn draw_white_pixel(
        renderer: &mut smithay::backend::renderer::gles::GlesRenderer,
        program: Option<&smithay::backend::renderer::gles::GlesTexProgram>,
        gains: [f32; 3],
    ) -> Option<[u8; 4]> {
        use smithay::backend::allocator::Fourcc;
        use smithay::backend::renderer::{Bind, ExportMem, Frame, ImportMem, Offscreen, Renderer};
        use smithay::utils::{Buffer, Physical, Rectangle, Size, Transform};
        let buffer_size: Size<i32, Buffer> = (2, 2).into();
        let output_size: Size<i32, Physical> = (2, 2).into();
        let white = renderer
            .import_memory(&[255u8; 16], Fourcc::Abgr8888, buffer_size, false)
            .ok()?;
        let mut target: smithay::backend::renderer::gles::GlesTexture =
            renderer.create_buffer(Fourcc::Abgr8888, buffer_size).ok()?;
        let mut bound = renderer.bind(&mut target).ok()?;
        let full_out = Rectangle::from_size(output_size);
        {
            let mut frame = renderer
                .render(&mut bound, output_size, Transform::Normal)
                .ok()?;
            if let Some(program) = program {
                frame.override_default_tex_program(
                    program.clone(),
                    vec![
                        smithay::backend::renderer::gles::Uniform::new("brightness", 1.0f32),
                        smithay::backend::renderer::gles::Uniform::new("nl_gains", gains),
                    ],
                );
            }
            frame
                .render_texture_from_to(
                    &white,
                    Rectangle::<f64, Buffer>::from_size((2.0, 2.0).into()),
                    full_out,
                    &[full_out],
                    &[],
                    Transform::Normal,
                    1.0,
                    None,
                    &[],
                )
                .ok()?;
            frame.clear_tex_program_override();
            let _ = frame.finish().ok()?;
        }
        let region = Rectangle::from_size(buffer_size);
        let mapping = renderer
            .copy_framebuffer(&bound, region, Fourcc::Abgr8888)
            .ok()?;
        let bytes = renderer.map_texture(&mapping).ok()?;
        Some([bytes[0], bytes[1], bytes[2], bytes[3]])
    }

    /// The production tint path end to end on the GPU: warm gains
    /// visibly warm a white texel, while unity gains and the default
    /// program leave it exactly white (identity control).
    #[cfg(feature = "drm")]
    #[test]
    fn tint_shader_warms_and_identity_is_exact() {
        let Some(mut renderer) = headless_renderer() else {
            eprintln!("night light GL test skipped: no surfaceless EGL");
            return;
        };
        let Some(program) = tint_program(&mut renderer) else {
            panic!("night light shader failed to compile");
        };
        // Untinted baseline: exact white.
        assert_eq!(
            draw_white_pixel(&mut renderer, None, [1.0; 3]),
            Some([255, 255, 255, 255])
        );
        // Unity gains through the tint program: bit-exact identity.
        let identity = draw_white_pixel(&mut renderer, Some(&program), [1.0; 3]).unwrap();
        assert_eq!(identity, [255, 255, 255, 255]);
        // Warm gains: red untouched, green and blue pulled down.
        let warm = temperature_to_gains(2700);
        let pixel = draw_white_pixel(&mut renderer, Some(&program), gains_f32(warm)).unwrap();
        let expect = |gain: f64| (255.0 * gain).round() as i32;
        for (channel, gain) in [
            (pixel[0], warm.red),
            (pixel[1], warm.green),
            (pixel[2], warm.blue),
        ] {
            assert!(
                (i32::from(channel) - expect(gain)).abs() <= 2,
                "pixel={pixel:?} gains={warm:?}"
            );
        }
        assert_eq!(pixel[3], 255);
        assert!(pixel[0] > pixel[1] && pixel[1] > pixel[2], "{pixel:?}");
        // Release the GL context before the surfaceless display drops.
        drop(renderer);
    }
}
