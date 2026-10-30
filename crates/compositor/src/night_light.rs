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
//! - [`temperature_to_gains`]: colord's Planckian blackbody colour for
//!   a temperature, unity at [`NEUTRAL_TEMPERATURE_K`].
//! - The owned GIO settings reader caches six typed keys and watches changes.
//!   Native calls run on one worker; the render and D-Bus consumers never wait
//!   for native I/O. Failure retains the previous config; deadline expiry
//!   permanently quarantines that reader without spawning a replacement.
//! - [`NightLightController`]: per-tick evaluation (settings
//!   refresh, schedule, smear, temp-disable, preview, 10 K hysteresis)
//!   shared with the D-Bus thread through [`NightLightShare`].
//! - [`tint_program`]: the GL texture program that multiplies every
//!   drawn texel by the gains, used only where no CRTC white point
//!   exists (nested outputs, CRTCs without GAMMA_LUT or CTM). The
//!   override lives for one frame only, so neutral restore is by
//!   construction: disabled means the program is never installed.
//!   Hardware outputs take the temperature as a CRTC gamma ramp or
//!   CTM through [`crate::color_manager`].
//!
//! Schedule math runs in local time over Unix timestamps: the fixed
//! from/to hours are local wall-clock hours and the automatic
//! schedule keys its solar dates off the local date, as GNOME does.
//! The solar instants themselves stay absolute (a sunset is one
//! instant everywhere), and the pure astronomy in [`sun_window`] is
//! still UTC based; only the schedule framing follows the local
//! zone through [`local_offset_seconds`]. Daylight-saving transitions
//! move the window with the wall clock; the multi-day "disabled
//! until tomorrow" clear samples one offset, so a disable straddling
//! a transition can clear up to one DST step early or late (the 24 h
//! lapse still bounds it).
//!
//! # GNOME 51 behaviour and defaults
//!
//! Compared against the 51.0 tags (g-s-d `51.0` =
//! `7f7913cc655b7c7b924d8f277177f95ac32bcd49`, mutter `51.0` =
//! `138a14fbeef09d49ebf5be8a0cb83b042dd5c841`, g-c-c `51.0` commit
//! `6ae712c0454f2586f00821c3486150c1e8feb3e7`):
//!
//! - g-s-d computes a smeared temperature and publishes it on
//!   `org.gnome.SettingsDaemon.Color` (`NightLightActive`,
//!   read-write `Temperature` and `DisabledUntilTomorrow`,
//!   `Sunrise`/`Sunset` in local hours, `NightLightPreview`);
//!   mutter reads `Temperature` and folds it into each output's
//!   gamma LUT (GAMMA_LUT, else CTM). We serve the same interface
//!   on the same bus name from the compositor; on hardware the
//!   temperature reaches the CRTC the same way
//!   ([`crate::color_manager::WhitePoint`]), nested outputs tint in GL.
//!   A `Temperature` write applies until Night Light's own
//!   temperature next changes, as `gsd_color_state_set_temperature`
//!   is overwritten by `notify::temperature`.
//! - Schedules evaluate in the system's local time
//!   (`g_date_time_new_now_local`), DST included; the zone is
//!   re-read each second (see [`local_offset_seconds`]).
//! - Every jump of 10 K or more eases over 5 s in 50 ms steps
//!   (`GSD_NIGHT_LIGHT_SMOOTH_SMEAR`, `gsd_night_light_smooth_cb`;
//!   g-s-d enables smoothing by default), previews included.
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
//!   K); moving it also calls `NightLightPreview(5)`, which forces
//!   the requested temperature for 5 s and then expires back to the
//!   schedule. Like g-s-d, durations outside 1–120 s are rejected
//!   without touching any live preview, and a repeated request
//!   replaces the timer. The deadline is monotonic, so wall-clock
//!   steps cannot prolong it. The share owns the preview state and
//!   its expiry (see [`NightLightShare::set_preview`]); the thin
//!   D-Bus method on `org.gnome.SettingsDaemon.Color` lives with
//!   the mutter.rs owner.
//!
//! Known divergences from GNOME 51, all recorded rather than
//! invented: settings refresh watches changes with a fallback recheck every
//! [`READ_INTERVAL`]; polar
//! day/night read inactive/active where g-s-d keeps its last
//! sunrise/sunset (manual fallback); where the GL tint applies
//! (nested, or CRTCs without GAMMA_LUT/CTM), transient solid feedback
//! (screenshot flashes; the bell and the blank shield are black, so
//! gains are a no-op on them) and the DRM hardware cursor bypass the
//! tint; screenshots and screen casts stay untinted, matching GNOME
//! (gamma applies past the framebuffer); `Temperature` changes are
//! announced once per rounded Kelvin step rather than per sub-Kelvin
//! step; a timezone change applies within a second of the next
//! tick, where g-s-d's cached `GTimeZone` may keep the old zone
//! [INFERENCE: GLib caches the local zone].
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
/// Length of the ease applied to every temperature jump
/// (`GSD_NIGHT_LIGHT_SMOOTH_SMEAR`, 5 s).
pub const SMOOTH_SECONDS: u32 = 5;
/// Step of that ease (`g_timeout_add (50, gsd_night_light_smooth_cb)`).
pub const SMOOTH_STEP: std::time::Duration = std::time::Duration::from_millis(50);
/// Steps in one ease: the last one lands exactly on the target.
const SMOOTH_STEPS: u32 = SMOOTH_SECONDS * 1000 / 50;
/// Schedule and cached-reader fallback cadence. Native change watches also
/// signal the existing runtime wake. A quarantined reader never retries; only
/// enabled schedule evaluation retains this clock obligation.
pub const READ_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);
/// How long a Settings slider preview lasts: the slider calls
/// `NightLightPreview(5)` while dragged, forcing the requested
/// temperature for this many seconds before the schedule resumes.
pub const DEFAULT_PREVIEW_SECONDS: u32 = 5;

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

/// Map a colour temperature in Kelvin to per-channel gains: colord's
/// Planckian blackbody colour, as Mutter folds it into the CRTC gamma
/// ramp (`set_blackbody_color_for_temperature` in
/// `meta-color-profile.c`, see [`crate::color_manager::blackbody_rgb`]).
///
/// Warm temperatures (low Kelvin) keep red and dim blue; cool ones do
/// the opposite. The input clamps to
/// [`MIN_TEMPERATURE_K`]..=[`MAX_TEMPERATURE_K`]; 6500 K is exactly
/// unity on every channel. Pure function of its argument.
pub fn temperature_to_gains(kelvin: u32) -> ColorGains {
    let [red, green, blue] = crate::color_manager::blackbody_rgb(clamp_temperature(kelvin));
    ColorGains { red, green, blue }
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
    /// Fixed daily window in local-time hours, as GNOME stores
    /// them: evaluation shifts `unix_seconds` by the local UTC
    /// offset (see [`local_offset_seconds`]) before comparing.
    Fixed {
        /// Window start, hours since 00:00 local, wrapped to 0..24.
        from_hour: f64,
        /// Window end, hours since 00:00 local, wrapped to 0..24.
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

    /// Whether the filter is active at `unix_seconds`, evaluated
    /// in local time through [`local_offset_seconds`].
    ///
    /// Disabled is always inactive, as is a live "disabled until
    /// tomorrow". Manual follows the switch; automatic is active
    /// through the night (sunset to sunrise) and fixed inside its
    /// window, both extended by the smear lead-in before the start
    /// edge. Polar day reads inactive and polar night active,
    /// matching "dark outside" (GNOME instead keeps its last
    /// sunrise/sunset there, i.e. a manual fallback).
    pub fn is_active_at(&self, unix_seconds: i64) -> bool {
        self.is_active_at_offset(unix_seconds, local_offset_seconds(unix_seconds))
    }

    /// [`NightLightConfig::is_active_at`] with an explicit UTC
    /// offset in seconds (east positive): the deterministic core
    /// behind the system-timezone path, and what tests pin. An
    /// offset of zero reads exactly the old UTC framing.
    pub fn is_active_at_offset(&self, unix_seconds: i64, utc_offset_secs: i64) -> bool {
        self.active_temperature_at_offset(unix_seconds, utc_offset_secs)
            .is_some()
    }

    /// Smeared temperature to apply at `unix_seconds`, or `None`
    /// when inactive, evaluated in local time. The temperature eases
    /// from [`NEUTRAL_TEMPERATURE_K`] to the requested one over
    /// [`SMEAR_HOURS`] before the start edge and back over the hour
    /// before the end edge; a manual schedule has no edges and
    /// applies the requested temperature directly.
    pub fn active_temperature_at(&self, unix_seconds: i64) -> Option<u32> {
        self.active_temperature_at_offset(unix_seconds, local_offset_seconds(unix_seconds))
    }

    /// [`NightLightConfig::active_temperature_at`] with an explicit
    /// UTC offset in seconds (east positive).
    pub fn active_temperature_at_offset(
        &self,
        unix_seconds: i64,
        utc_offset_secs: i64,
    ) -> Option<u32> {
        if !self.enabled || self.temp_disabled_at(unix_seconds, utc_offset_secs) {
            return None;
        }
        let target = self.temperature_k();
        match &self.schedule {
            NightLightSchedule::Manual => Some(target),
            NightLightSchedule::Automatic {
                latitude_deg,
                longitude_deg,
            } => automatic_temperature(
                *latitude_deg,
                *longitude_deg,
                target,
                unix_seconds,
                utc_offset_secs,
            ),
            NightLightSchedule::Fixed { from_hour, to_hour } => {
                fixed_temperature(*from_hour, *to_hour, target, unix_seconds, utc_offset_secs)
            }
        }
    }

    /// Gains to apply at `unix_seconds`, or `None` when inactive.
    pub fn active_gains_at(&self, unix_seconds: i64) -> Option<ColorGains> {
        self.active_gains_at_offset(unix_seconds, local_offset_seconds(unix_seconds))
    }

    /// [`NightLightConfig::active_gains_at`] with an explicit UTC
    /// offset in seconds (east positive).
    pub fn active_gains_at_offset(
        &self,
        unix_seconds: i64,
        utc_offset_secs: i64,
    ) -> Option<ColorGains> {
        self.active_temperature_at_offset(unix_seconds, utc_offset_secs)
            .map(temperature_to_gains)
    }

    /// Whether "disabled until tomorrow" suppresses the filter at
    /// `unix_seconds`. Set in the future (a clock step back) stays
    /// suppressing; past 24 h it lapses; otherwise a sunrise instant
    /// in `(set, now]` clears it — the local `to` hour for a fixed
    /// window, the computed sunrise for an automatic one. A manual
    /// schedule has no sunrise, so only the 24 h lapse applies there.
    /// The offset is sampled once for the whole check, so a disable
    /// straddling a DST transition can clear up to one step early or
    /// late; the 24 h lapse still bounds it.
    fn temp_disabled_at(&self, unix_seconds: i64, utc_offset_secs: i64) -> bool {
        let Some(set_at) = self.disabled_until_tmw else {
            return false;
        };
        if unix_seconds <= set_at {
            return true;
        }
        if unix_seconds - set_at > 86_400 {
            return false;
        }
        !sunrise_between(&self.schedule, set_at, unix_seconds, utc_offset_secs)
    }
}

/// Whether a sunrise instant falls in `(set_at, now]`, clearing
/// "disabled until tomorrow". See
/// [`NightLightConfig::is_active_at`]. Fixed `to` hours are local
/// wall-clock hours converted back with `utc_offset_secs`; automatic
/// sunrises key off the local dates around the check.
fn sunrise_between(
    schedule: &NightLightSchedule,
    set_at: i64,
    now: i64,
    utc_offset_secs: i64,
) -> bool {
    let clears = |instant: i64| instant > set_at && instant <= now;
    match schedule {
        // No sunrise on a manual schedule: only the 24 h lapse.
        NightLightSchedule::Manual => false,
        NightLightSchedule::Fixed { to_hour, .. } => {
            let to_seconds = (to_hour.rem_euclid(24.0) * 3600.0).round() as i64;
            let first = (set_at + utc_offset_secs).div_euclid(86_400) - 1;
            let last = (now + utc_offset_secs).div_euclid(86_400) + 1;
            (first..=last).any(|day| clears(day * 86_400 + to_seconds - utc_offset_secs))
        }
        NightLightSchedule::Automatic {
            latitude_deg,
            longitude_deg,
        } => {
            let first = (set_at + utc_offset_secs).div_euclid(86_400) - 1;
            let last = (now + utc_offset_secs).div_euclid(86_400) + 1;
            (first..=last).any(|day| {
                matches!(
                    sun_window_for_day_of_year(
                        *latitude_deg,
                        *longitude_deg,
                        day_of_year_for_days(day)
                    ),
                    Daylight::Normal(window)
                        if clears(
                            day * 86_400 + (window.sunrise_utc_min * 60.0).round() as i64
                        )
                )
            })
        }
    }
}

/// Night length of the fixed local-hour window, in hours. A
/// wrapping window (e.g. 22:00–06:00) spans midnight; `from == to`
/// reads as the full day, matching GNOME's `frac_day_is_between`
/// (equal bounds always match).
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

/// Smeared temperature for the fixed local-hour window at
/// `unix_seconds`, or `None` outside `[from - smear, to)`.
/// `utc_offset_secs` (east positive) shifts the instant into the
/// local wall clock before comparing against the hours.
fn fixed_temperature(
    from_hour: f64,
    to_hour: f64,
    target_k: u32,
    unix_seconds: i64,
    utc_offset_secs: i64,
) -> Option<u32> {
    let from = from_hour.rem_euclid(24.0);
    let len = fixed_night_len_hours(from_hour, to_hour);
    let smear = smear_hours_for(len);
    let now = ((unix_seconds + utc_offset_secs).rem_euclid(86_400) as f64) / 3600.0;
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

/// Night intervals covering `unix_seconds` from the local dates
/// around it: each normal day's sunset paired with the nearest
/// later sunrise within 24 h. Polar days contribute nothing (the
/// caller's polar handling decides those). The returned instants
/// are absolute Unix seconds; only the date selection follows
/// `utc_offset_secs`, so the pairing stays exact across DST. Two
/// days of slack either side keep the covering night paired at any
/// offset (at zero offset the extra days can never cover `now`,
/// so the old UTC results read back exactly).
fn night_intervals(
    latitude_deg: f64,
    longitude_deg: f64,
    unix_seconds: i64,
    utc_offset_secs: i64,
) -> Vec<NightInterval> {
    let day = (unix_seconds + utc_offset_secs).div_euclid(86_400);
    let mut sunsets = Vec::new();
    let mut sunrises = Vec::new();
    for date in [day - 2, day - 1, day, day + 1, day + 2] {
        if let Daylight::Normal(window) =
            sun_window_for_day_of_year(latitude_deg, longitude_deg, day_of_year_for_days(date))
        {
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
/// sunrise)`. The polar check and the night pairing key off the
/// local date (`utc_offset_secs` east positive). Polar day reads
/// inactive, polar night fully active.
fn automatic_temperature(
    latitude_deg: f64,
    longitude_deg: f64,
    target_k: u32,
    unix_seconds: i64,
    utc_offset_secs: i64,
) -> Option<u32> {
    let local_day = (unix_seconds + utc_offset_secs).div_euclid(86_400);
    match sun_window_for_day_of_year(latitude_deg, longitude_deg, day_of_year_for_days(local_day)) {
        Daylight::PolarDay => return None,
        Daylight::PolarNight => return Some(target_k),
        Daylight::Normal(_) => {}
    }
    for interval in night_intervals(latitude_deg, longitude_deg, unix_seconds, utc_offset_secs) {
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
    sun_window_for_day_of_year(latitude_deg, longitude_deg, day_of_year(unix_seconds))
}

/// Sunrise/sunset for a 1-based day of year: the deterministic core
/// of [`sun_window`]. The schedule passes the local date's day of
/// year so the automatic window follows local dates; the returned
/// minutes stay minutes since 00:00 UTC on the date with that day
/// of year, and callers pair them into absolute instants.
fn sun_window_for_day_of_year(latitude_deg: f64, longitude_deg: f64, day_of_year: u16) -> Daylight {
    let lat = latitude_deg.clamp(-90.0, 90.0).to_radians();
    let lon = longitude_deg.rem_euclid(360.0);
    let lon = if lon > 180.0 { lon - 360.0 } else { lon };
    let day_of_year = f64::from(day_of_year);

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
    day_of_year_for_days(unix_seconds.div_euclid(86_400))
}

/// 1-based day of year for a date number (days since 1970-01-01 in
/// whatever frame the caller counts — UTC for [`sun_window`], local
/// for the schedule).
fn day_of_year_for_days(days: i64) -> u16 {
    let (year, _, _) = civil_from_days(days);
    let jan1 = days_from_civil(year, 1, 1);
    (days - jan1 + 1) as u16
}

/// The system's local UTC offset in seconds (east positive) at
/// `unix_seconds`, through the C library's timezone database: the
/// wall clock minus UTC, DST included. A conversion failure reads
/// as zero, i.e. the schedule is evaluated in UTC framing — which
/// can actively apply tint at UTC hours. That is a degraded guess,
/// not fail-closed and not inactive: callers needing inactivity on
/// unknown zone must check for it themselves.
///
/// The zone is re-read (`tzset`) at most once per wall-clock second,
/// so a runtime timezone change (timedated rewriting `/etc/localtime`,
/// or `TZ` changing) moves the schedule on the next tick. glibc's
/// `localtime_r` alone keeps the zone loaded at first use forever.
pub fn local_offset_seconds(unix_seconds: i64) -> i64 {
    refresh_system_timezone(unix_seconds);
    let stamp: libc::time_t = unix_seconds as libc::time_t;
    let mut broken = std::mem::MaybeUninit::<libc::tm>::zeroed();
    // SAFETY: `localtime_r` either fills the struct and returns it,
    // or returns null and leaves our zeroed value alone.
    let filled = unsafe { libc::localtime_r(&stamp, broken.as_mut_ptr()) };
    if filled.is_null() {
        return 0;
    }
    let tm = unsafe { broken.assume_init() };
    let wall = days_from_civil(
        i64::from(tm.tm_year) + 1900,
        i64::from(tm.tm_mon) + 1,
        i64::from(tm.tm_mday),
    ) * 86_400
        + i64::from(tm.tm_hour) * 3600
        + i64::from(tm.tm_min) * 60
        + i64::from(tm.tm_sec);
    wall - unix_seconds
}

/// Re-run `tzset` when the wall-clock second changed since the last
/// call: one `stat` of the zone file per second rather than per frame.
fn refresh_system_timezone(unix_seconds: i64) {
    static LAST_TZSET: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(i64::MIN);
    if LAST_TZSET.swap(unix_seconds, std::sync::atomic::Ordering::Relaxed) != unix_seconds {
        extern "C" {
            fn tzset();
        }
        // SAFETY: `tzset` takes no arguments; glibc serialises it
        // against `localtime_r` with its internal timezone lock.
        unsafe { tzset() };
    }
}

/// Signed whole Unix seconds for the actual system clock, including dates
/// before 1970. Floor negative fractions rather than treating them as a failed
/// clock. Unrepresentable dates have an explicit neutral consumer policy.
pub(crate) fn system_unix_seconds(now: std::time::SystemTime) -> Option<i64> {
    let seconds = match now.duration_since(std::time::UNIX_EPOCH) {
        Ok(value) => i128::from(value.as_secs()),
        Err(error) => {
            let value = error.duration();
            -i128::from(value.as_secs()) - i128::from(value.subsec_nanos() != 0)
        }
    };
    i64::try_from(seconds).ok()
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

/// Longest `NightLightPreview` duration g-s-d accepts: durations
/// outside `1..=MAX_PREVIEW_SECONDS` are rejected with
/// `InvalidArgument` and never touch the recorded preview
/// (`gsd-color-manager.c`, `NightLightPreview` method handler).
pub const MAX_PREVIEW_SECONDS: u32 = 120;

/// Rejection of a `NightLightPreview` request: the recorded preview,
/// if any, is left exactly as it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewRejected {
    /// Duration 0 or above [`MAX_PREVIEW_SECONDS`]; the D-Bus
    /// producer maps this to `InvalidArgs`.
    InvalidDuration(u32),
    /// The share mutex is poisoned: nothing was recorded, and
    /// success is not claimed. The D-Bus producer maps this to
    /// `Failed`, never to `InvalidArgs`.
    Unavailable,
}

/// A live `NightLightPreview` request: force this temperature until
/// the monotonic deadline `until`, then fall back to the schedule.
/// The deadline is a monotonic [`std::time::Instant`] like g-s-d's
/// `g_timeout_add_seconds`: NTP steps and backward wall-clock jumps
/// can neither prolong nor cut it short. In-memory like "disabled
/// until tomorrow": never persisted, never read from settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Preview {
    temperature_k: u32,
    until: std::time::Instant,
}

/// One `org.gnome.SettingsDaemon.Color` property change, raised
/// exactly where g-s-d's `GsdNightLight` notifies (`notify::active`,
/// `::temperature`, `::disabled-until-tmw`, `::sunrise`, `::sunset`)
/// and so where `gsd-color-manager.c` emits one `PropertiesChanged`
/// per property. A `Temperature` property write raises nothing, as in
/// g-s-d's `handle_set_property`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ColorChange {
    /// `NightLightActive` flipped.
    NightLightActive(bool),
    /// Night Light's own (smoothed) temperature moved, rounded.
    Temperature(u32),
    /// `DisabledUntilTomorrow` flipped.
    DisabledUntilTomorrow(bool),
    /// `Sunrise` moved by more than a minute.
    Sunrise(f64),
    /// `Sunset` moved by more than a minute.
    Sunset(f64),
}

/// Live state shared between the compositor loop (which owns the
/// [`NightLightController`]) and the D-Bus thread (which serves
/// `org.gnome.SettingsDaemon.Color`): the loop publishes what it
/// applies, the bus side publishes "disabled until tomorrow" and
/// slider previews.
pub struct NightLightShare {
    /// Whether the filter applies right now (`NightLightActive`).
    active: std::sync::atomic::AtomicBool,
    /// Smeared temperature currently applied, or
    /// [`NEUTRAL_TEMPERATURE_K`] when inactive (`Temperature`).
    temperature: std::sync::atomic::AtomicU32,
    /// Kelvin written to the `Temperature` property, or 0 for none.
    /// Like g-s-d's `gsd_color_state_set_temperature` it applies
    /// until Night Light's own temperature next changes.
    temperature_override: std::sync::atomic::AtomicU32,
    /// `Sunrise`/`Sunset` as `f64` bits: local fractional hours, `-1`
    /// until a location yields one (g-s-d `cached_sunrise/sunset`).
    sunrise_bits: std::sync::atomic::AtomicU64,
    sunset_bits: std::sync::atomic::AtomicU64,
    /// Unix seconds when `DisabledUntilTomorrow` was set, if set.
    disabled_until_tmw: std::sync::Mutex<Option<i64>>,
    /// Live slider preview, if one has not expired yet.
    preview: std::sync::Mutex<Option<Preview>>,
    /// Only controlled ownership fixtures replace preview time; native reads
    /// retain their independent real admission/operation deadlines.
    #[cfg(test)]
    preview_clock_for_test: std::sync::Mutex<Option<std::time::Instant>>,
    /// Loop wake, connected by the runtime: recording a preview
    /// signals it so an idle desktop applies the preview without
    /// waiting for unrelated input. Disconnected (default) pings
    /// nothing; see [`NightLightShare::set_wake`].
    wake: crate::wake::Wake,
    reader: std::sync::Mutex<Option<crate::night_light_settings::Reader>>,
    /// Subscriber for [`ColorChange`]s (the D-Bus side), if any.
    changes: std::sync::Mutex<Option<std::sync::mpsc::Sender<ColorChange>>>,
}

impl Default for NightLightShare {
    /// Inactive, neutral, no temp-disable, no preview, no wake.
    fn default() -> Self {
        Self {
            active: std::sync::atomic::AtomicBool::new(false),
            temperature: std::sync::atomic::AtomicU32::new(NEUTRAL_TEMPERATURE_K),
            temperature_override: std::sync::atomic::AtomicU32::new(0),
            sunrise_bits: std::sync::atomic::AtomicU64::new((-1.0f64).to_bits()),
            sunset_bits: std::sync::atomic::AtomicU64::new((-1.0f64).to_bits()),
            disabled_until_tmw: std::sync::Mutex::new(None),
            preview: std::sync::Mutex::new(None),
            #[cfg(test)]
            preview_clock_for_test: std::sync::Mutex::new(None),
            wake: crate::wake::Wake::default(),
            reader: std::sync::Mutex::new(None),
            changes: std::sync::Mutex::new(None),
        }
    }
}

impl NightLightShare {
    pub(crate) fn start_reader(self: &std::sync::Arc<Self>, wake: &crate::wake::Wake) {
        if let Ok(mut reader) = self.reader.lock() {
            if reader.is_none() {
                *reader = Some(crate::night_light_settings::Reader::new(self, wake));
            }
        }
    }

    pub(crate) fn request_preview(
        &self,
        duration: u32,
    ) -> Result<crate::night_light_settings::PreviewFuture, PreviewRejected> {
        // Invalid duration must never touch the reader, native I/O or live preview.
        if duration == 0 || duration > MAX_PREVIEW_SECONDS {
            return Err(PreviewRejected::InvalidDuration(duration));
        }
        let admitted = std::time::Instant::now();
        let reader = self
            .reader
            .lock()
            .map_err(|_| PreviewRejected::Unavailable)?;
        reader
            .as_ref()
            .ok_or(PreviewRejected::Unavailable)?
            .preview(duration, admitted)
    }

    pub(crate) fn record_preview_until(
        &self,
        temperature_k: u32,
        until: std::time::Instant,
    ) -> Result<(), PreviewRejected> {
        let observed = std::time::Instant::now();
        #[cfg(test)]
        let observed = self.preview_observed_for_test(observed);
        if observed >= until {
            return Err(PreviewRejected::Unavailable);
        }
        let mut preview = self
            .preview
            .lock()
            .map_err(|_| PreviewRejected::Unavailable)?;
        let observed = std::time::Instant::now();
        #[cfg(test)]
        let observed = self.preview_observed_for_test(observed);
        if observed >= until {
            return Err(PreviewRejected::Unavailable);
        }
        *preview = Some(Preview {
            temperature_k: clamp_temperature(temperature_k),
            until,
        });
        self.wake.signal();
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn set_preview_clock_for_test(&self, now: std::time::Instant) {
        *self.preview_clock_for_test.lock().unwrap() = Some(now);
    }

    #[cfg(test)]
    pub(crate) fn preview_observed_for_test(
        &self,
        actual: std::time::Instant,
    ) -> std::time::Instant {
        self.preview_clock_for_test
            .lock()
            .unwrap()
            .unwrap_or(actual)
    }

    #[cfg(test)]
    pub(crate) fn swap_reader_for_test(
        &self,
        reader: Option<crate::night_light_settings::Reader>,
    ) -> Option<crate::night_light_settings::Reader> {
        std::mem::replace(&mut *self.reader.lock().unwrap(), reader)
    }

    #[cfg(test)]
    pub(crate) fn reader_for_test_is_absent(&self) -> bool {
        self.reader.lock().unwrap().is_none()
    }

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
        let was = self
            .active
            .swap(active, std::sync::atomic::Ordering::SeqCst);
        self.temperature
            .store(temperature_k, std::sync::atomic::Ordering::SeqCst);
        if was != active {
            self.emit(ColorChange::NightLightActive(active));
        }
    }

    /// Write `Temperature` from the D-Bus side (g-s-d
    /// `handle_set_property`): reject values outside
    /// [`MIN_TEMPERATURE_K`]..=[`MAX_TEMPERATURE_K`] unchanged, else
    /// apply `kelvin` now and keep it until Night Light's own
    /// temperature next changes.
    pub fn set_temperature_override(&self, kelvin: u32) -> Result<(), u32> {
        if !(MIN_TEMPERATURE_K..=MAX_TEMPERATURE_K).contains(&kelvin) {
            return Err(kelvin);
        }
        self.temperature_override
            .store(kelvin, std::sync::atomic::Ordering::SeqCst);
        self.temperature
            .store(kelvin, std::sync::atomic::Ordering::SeqCst);
        self.wake.signal();
        Ok(())
    }

    /// The live `Temperature` write, if Night Light has not moved since.
    fn temperature_override(&self) -> Option<u32> {
        match self
            .temperature_override
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            0 => None,
            kelvin => Some(kelvin),
        }
    }

    /// Night Light's own temperature changed: drop a `Temperature`
    /// write, as g-s-d's `notify::temperature` overwrites colour state.
    fn clear_temperature_override(&self) {
        self.temperature_override
            .store(0, std::sync::atomic::Ordering::SeqCst);
    }

    /// Read `Sunrise` (local hours, `-1` before any location).
    pub fn sunrise(&self) -> f64 {
        f64::from_bits(self.sunrise_bits.load(std::sync::atomic::Ordering::SeqCst))
    }

    /// Read `Sunset` (local hours, `-1` before any location).
    pub fn sunset(&self) -> f64 {
        f64::from_bits(self.sunset_bits.load(std::sync::atomic::Ordering::SeqCst))
    }

    /// Store and announce each of `Sunrise`/`Sunset` only when it moved
    /// by more than a minute (`GSD_FRAC_DAY_MAX_DELTA`).
    fn publish_sun(&self, sunrise_hours: f64, sunset_hours: f64) {
        if (self.sunrise() - sunrise_hours).abs() > 1.0 / 60.0 {
            self.sunrise_bits
                .store(sunrise_hours.to_bits(), std::sync::atomic::Ordering::SeqCst);
            self.emit(ColorChange::Sunrise(sunrise_hours));
        }
        if (self.sunset() - sunset_hours).abs() > 1.0 / 60.0 {
            self.sunset_bits
                .store(sunset_hours.to_bits(), std::sync::atomic::Ordering::SeqCst);
            self.emit(ColorChange::Sunset(sunset_hours));
        }
    }

    /// Receive every property change g-s-d would announce with
    /// `PropertiesChanged`, in order. A new subscriber replaces the old.
    pub fn subscribe_changes(&self) -> std::sync::mpsc::Receiver<ColorChange> {
        let (sender, receiver) = std::sync::mpsc::channel();
        if let Ok(mut changes) = self.changes.lock() {
            *changes = Some(sender);
        }
        receiver
    }

    fn emit(&self, change: ColorChange) {
        if let Ok(changes) = self.changes.lock() {
            if let Some(sender) = changes.as_ref() {
                let _ = sender.send(change);
            }
        }
    }

    /// Write `DisabledUntilTomorrow` from the D-Bus side: `Some(now)`
    /// when set, `None` when cleared.
    pub fn set_disabled_until_tmw(&self, set_at: Option<i64>) {
        if let Ok(mut set) = self.disabled_until_tmw.lock() {
            let changed = set.is_some() != set_at.is_some();
            *set = set_at;
            if changed {
                self.emit(ColorChange::DisabledUntilTomorrow(set_at.is_some()));
            }
            // A temp-disable write must reach an idle loop the same
            // way a preview does; signal only when recorded.
            self.wake.signal();
        }
    }

    /// Retire only the disable timestamp evaluated by this tick. A new D-Bus
    /// disable recorded concurrently remains owned by that newer write.
    pub(crate) fn clear_disabled_until_if(&self, expected: i64) -> bool {
        let Ok(mut set) = self.disabled_until_tmw.lock() else {
            return false;
        };
        if *set != Some(expected) {
            return false;
        }
        *set = None;
        self.emit(ColorChange::DisabledUntilTomorrow(false));
        self.wake.signal();
        true
    }

    /// Record a `NightLightPreview` from the D-Bus side: force
    /// `temperature_k` (clamped) for `duration_secs` seconds — the
    /// Settings slider passes [`DEFAULT_PREVIEW_SECONDS`] while
    /// dragged. Like g-s-d, a repeated request replaces the prior
    /// timer, and a duration of 0 or above
    /// [`MAX_PREVIEW_SECONDS`] is rejected with
    /// [`PreviewRejected::InvalidDuration`] without touching any
    /// recorded preview. The caller resolves the temperature from
    /// a fresh settings read (the slider already wrote the key);
    /// the loop applies it on the next tick and the preview
    /// expires back to the schedule on its own.
    pub fn set_preview(
        &self,
        temperature_k: u32,
        duration_secs: u32,
    ) -> Result<(), PreviewRejected> {
        self.set_preview_at(temperature_k, std::time::Instant::now(), duration_secs)
    }

    /// [`NightLightShare::set_preview`] with an explicit monotonic
    /// clock: the deterministic seam behind the production path,
    /// and what tests pin.
    pub fn set_preview_at(
        &self,
        temperature_k: u32,
        now: std::time::Instant,
        duration_secs: u32,
    ) -> Result<(), PreviewRejected> {
        if duration_secs == 0 || duration_secs > MAX_PREVIEW_SECONDS {
            return Err(PreviewRejected::InvalidDuration(duration_secs));
        }
        let until = now
            .checked_add(std::time::Duration::from_secs(duration_secs as u64))
            .ok_or(PreviewRejected::InvalidDuration(duration_secs))?;
        let preview = Preview {
            temperature_k: clamp_temperature(temperature_k),
            until,
        };
        match self.preview.lock() {
            Ok(mut guard) => {
                *guard = Some(preview);
            }
            // Poisoned: a previous holder panicked mid-write, so the
            // recorded state is unknown. Report it; never claim the
            // preview was recorded.
            Err(_) => return Err(PreviewRejected::Unavailable),
        }
        // An idle desktop must apply the preview without waiting for
        // unrelated input; a disconnected wake pings nothing.
        self.wake.signal();
        Ok(())
    }

    /// Connect this share to the runtime loop wake, mirroring
    /// `Wallpaper::set_wake`: the runtime calls this once at
    /// construction so preview records wake an idle loop.
    pub(crate) fn set_wake(&self, wake: &crate::wake::Wake) {
        self.wake.connect(wake);
    }

    /// The preview deadline at monotonic `now`, if a preview
    /// record exists: the exact expiry while live, else due-now.
    /// An expired-but-unconsumed record keeps a past-due
    /// obligation — the expiry can fall between the render tick
    /// and `next_wait`, and returning `None` there would lose the
    /// restoring tick. The obligation clears when
    /// [`NightLightShare::preview_temperature`] consumes the
    /// record (one catch-up tick), or on explicit clear. `None`
    /// only when no record exists or the mutex is poisoned
    /// (poison also degrades `preview_temperature` to the
    /// schedule path, so no tint can stick on it).
    pub fn preview_deadline_at(&self, now: std::time::Instant) -> Option<std::time::Instant> {
        let guard = self.preview.lock().ok()?;
        match *guard {
            Some(preview) if now < preview.until => Some(preview.until),
            Some(_) => Some(now),
            None => None,
        }
    }

    /// [`NightLightShare::preview_deadline_at`] at this instant.
    pub fn preview_deadline(&self) -> Option<std::time::Instant> {
        self.preview_deadline_at(std::time::Instant::now())
    }

    /// Clear any recorded preview without applying it.
    pub fn clear_preview(&self) {
        if let Ok(reader) = self.reader.lock() {
            if let Some(reader) = reader.as_ref() {
                reader.cancel_preview();
            }
        }
        if let Ok(mut guard) = self.preview.lock() {
            *guard = None;
            // Restoring the schedule tint must not wait for
            // unrelated input; signal only when cleared.
            self.wake.signal();
        }
    }

    /// The preview temperature when a preview is live at the
    /// monotonic instant `now` (strictly before its deadline), else
    /// `None`. Wall-clock time is never consulted, so clock steps
    /// cannot change the answer. An expired preview clears itself
    /// here, so the schedule resumes on the next tick with no
    /// further calls. A poisoned mutex also yields `None`: the
    /// caller falls through to the schedule, which overwrites any
    /// applied gains, so poison degrades to scheduled tint rather
    /// than a stuck preview.
    pub fn preview_temperature(&self, now: std::time::Instant) -> Option<u32> {
        let mut guard = self.preview.lock().ok()?;
        match *guard {
            Some(preview) if now < preview.until => Some(preview.temperature_k),
            _ => {
                *guard = None;
                None
            }
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
    reader_status: Option<crate::night_light_settings::CacheStatus>,
    /// `(latitude, longitude, local day, offset)` behind the last
    /// published `Sunrise`/`Sunset`, so the solar math runs once per
    /// local day or location instead of every frame.
    sun_key: Option<(u64, u64, i64, i64)>,
    /// g-s-d `smooth_enabled` (on by default, as in g-s-d).
    smooth_enabled: bool,
    /// g-s-d `cached_temperature`: Night Light's own temperature, as
    /// eased toward the schedule or preview target.
    cached_temperature: f64,
    /// `cached_temperature` rounded, as last seen: a change drops a
    /// `Temperature` property write.
    night_light_k: u32,
    /// A running 5 s ease, if any.
    smooth: Option<Smooth>,
}

/// A running ease toward `target` (g-s-d `smooth_timer`/`smooth_id`).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Smooth {
    target: f64,
    started: std::time::Instant,
    steps_done: u32,
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
            reader_status: None,
            sun_key: None,
            smooth_enabled: true,
            cached_temperature: f64::from(NEUTRAL_TEMPERATURE_K),
            night_light_k: NEUTRAL_TEMPERATURE_K,
            smooth: None,
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

    /// The temperature the last tick published (`Temperature`), which
    /// Mutter folds into each CRTC's white point.
    pub fn applied_kelvin(&self) -> u32 {
        self.share.temperature()
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

    /// Actual frame consumer: bounded Rust cache only, never native GIO.
    pub(crate) fn poll_settings(&mut self, now: std::time::Instant) -> bool {
        // This cadence is also the schedule re-evaluation obligation, distinct
        // from native retry. Quarantine never starts another reader operation.
        if self
            .last_read
            .is_none_or(|last| now.saturating_duration_since(last) >= READ_INTERVAL)
        {
            self.last_read = Some(now);
        }
        let Ok(reader) = self.share.reader.lock() else {
            return false;
        };
        let Some(reader) = reader.as_ref() else {
            return false;
        };
        let config = reader.poll(now);
        let status = reader.status();
        if self.reader_status != Some(status) {
            if matches!(
                status,
                crate::night_light_settings::CacheStatus::Unavailable
                    | crate::night_light_settings::CacheStatus::Quarantined
            ) {
                eprintln!("tuna-compositor: night light settings cache {status:?}; retaining prior config");
            }
            self.reader_status = Some(status);
        }
        let Some(mut config) = config else {
            return false;
        };
        config.disabled_until_tmw = self.share.disabled_until_tmw();
        if config == self.config {
            return false;
        }
        self.config = config;
        true
    }

    /// Earliest owned reader admission, pending/live preview expiry, or enabled
    /// schedule recheck. Reader quarantine consumes its native deadline once;
    /// a disabled quarantined reader has no periodic retry obligation.
    /// Unconnected pure-controller callers retain their legacy test cadence.
    pub fn next_deadline_at(&self, now: std::time::Instant) -> Option<std::time::Instant> {
        if let Ok(reader) = self.share.reader.lock() {
            if let Some(reader) = reader.as_ref() {
                let schedule = (self.config.enabled || self.share.disabled_until_tmw().is_some())
                    .then(|| {
                        self.last_read
                            .and_then(|last| last.checked_add(READ_INTERVAL))
                            .unwrap_or(now)
                            .max(now)
                    });
                return reader
                    .next_deadline(now)
                    .into_iter()
                    .chain(self.share.preview_deadline_at(now))
                    .chain(self.smooth_deadline().map(|due| due.max(now)))
                    .chain(schedule)
                    .min();
            }
        }
        let preview = self.share.preview_deadline_at(now);
        // Never-read or stale settings are due now; fresh settings
        // are due one interval after the last read. Either way the
        // arm is at or after `now`, so the loop never spins on the
        // past and never sleeps past a due re-read — including a
        // re-read that would observe a later enable.
        let settings = self
            .last_read
            .and_then(|last| last.checked_add(READ_INTERVAL))
            .filter(|due| *due > now)
            .unwrap_or(now);
        Some(
            preview
                .into_iter()
                .chain(self.smooth_deadline().map(|due| due.max(now)))
                .fold(settings, std::cmp::Ord::min),
        )
    }

    /// [`NightLightController::next_deadline_at`] at this instant.
    pub fn next_deadline(&self) -> Option<std::time::Instant> {
        self.next_deadline_at(std::time::Instant::now())
    }

    /// Evaluate the filter at `unix_seconds` and publish the result
    /// to the share. Returns the gains to install, or `None` for a
    /// neutral frame. Unity gains (inactive, disabled, or exactly
    /// [`NEUTRAL_TEMPERATURE_K`]) also yield `None`: like mutter, we
    /// never program an identity transform. A live slider preview
    /// forces its temperature now (bypassing the schedule and the
    /// hysteresis anchor so each slider step shows immediately);
    /// once it expires the schedule resumes on the next tick.
    pub fn tick(&mut self, unix_seconds: i64) -> Option<ColorGains> {
        self.tick_with_offset(unix_seconds, local_offset_seconds(unix_seconds))
    }

    /// [`NightLightController::tick`] with an explicit UTC offset in
    /// seconds (east positive): the deterministic core behind the
    /// system-timezone path, and what tests pin.
    pub fn tick_with_offset(
        &mut self,
        unix_seconds: i64,
        utc_offset_secs: i64,
    ) -> Option<ColorGains> {
        self.tick_with_clock(unix_seconds, utc_offset_secs, std::time::Instant::now())
    }

    /// [`NightLightController::tick`] with an explicit UTC offset and
    /// an explicit monotonic clock: the deterministic core behind
    /// both production paths, and what preview tests pin. The wall
    /// clock drives the schedule; only the monotonic clock drives
    /// preview expiry.
    pub fn tick_with_clock(
        &mut self,
        unix_seconds: i64,
        utc_offset_secs: i64,
        monotonic_now: std::time::Instant,
    ) -> Option<ColorGains> {
        self.config.disabled_until_tmw = self.share.disabled_until_tmw();
        if let Some(set_at) = self.config.disabled_until_tmw {
            if !self.config.temp_disabled_at(unix_seconds, utc_offset_secs)
                && self.share.clear_disabled_until_if(set_at)
            {
                self.config.disabled_until_tmw = None;
            }
        }
        self.config.disabled_until_tmw = self.share.disabled_until_tmw();
        self.publish_sun_window(unix_seconds, utc_offset_secs);
        if let Some(preview_k) = self.share.preview_temperature(monotonic_now) {
            // A live preview forces its temperature, bypassing the
            // schedule and the hysteresis anchor (g-s-d `forced`).
            self.applied_temperature = Some(preview_k);
            return self.finish_tick(true, preview_k, monotonic_now);
        }
        let target = self
            .config
            .active_temperature_at_offset(unix_seconds, utc_offset_secs);
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
        self.finish_tick(
            active,
            temperature.unwrap_or(NEUTRAL_TEMPERATURE_K),
            monotonic_now,
        )
    }

    /// Smooth toward `desired_k`, honour a `Temperature` write, and
    /// publish/return what applies this frame.
    fn finish_tick(
        &mut self,
        active: bool,
        desired_k: u32,
        monotonic_now: std::time::Instant,
    ) -> Option<ColorGains> {
        self.smooth_toward(f64::from(desired_k), monotonic_now);
        let night_light_k = self.cached_temperature.round() as u32;
        let moved = night_light_k != self.night_light_k;
        if moved {
            self.night_light_k = night_light_k;
            self.share.clear_temperature_override();
        }
        let kelvin = self.share.temperature_override().unwrap_or(night_light_k);
        let gains = (kelvin != NEUTRAL_TEMPERATURE_K).then(|| temperature_to_gains(kelvin));
        self.applied_gains = gains;
        self.share.publish(active, kelvin);
        if moved {
            self.share.emit(ColorChange::Temperature(night_light_k));
        }
        gains
    }

    /// g-s-d's `gsd_night_light_set_temperature` with smoothing on:
    /// a jump of [`TEMPERATURE_QUANTUM_K`] or more eases over
    /// [`SMOOTH_SECONDS`], stepping every [`SMOOTH_STEP`] by
    /// `cached + (target - cached) * elapsed / 5 s` and skipping steps
    /// within the quantum (`gsd_night_light_smooth_cb`); a smaller
    /// jump applies at once. A new target restarts the ease from the
    /// current temperature. Steps sit on the 50 ms grid of the start
    /// instant, so the result does not depend on the frame rate.
    fn smooth_toward(&mut self, desired: f64, now: std::time::Instant) {
        if !self.smooth_enabled {
            self.cached_temperature = desired;
            self.smooth = None;
            return;
        }
        let retarget = match self.smooth {
            Some(smooth) => smooth.target != desired,
            None => self.cached_temperature != desired,
        };
        if retarget {
            self.smooth = None;
            if (desired - self.cached_temperature).abs() < f64::from(TEMPERATURE_QUANTUM_K) {
                self.cached_temperature = desired;
            } else {
                self.smooth = Some(Smooth {
                    target: desired,
                    started: now,
                    steps_done: 0,
                });
            }
        }
        let Some(mut smooth) = self.smooth else {
            return;
        };
        let elapsed = now.saturating_duration_since(smooth.started);
        let due =
            (elapsed.as_millis() / SMOOTH_STEP.as_millis()).min(u128::from(SMOOTH_STEPS)) as u32;
        while smooth.steps_done < due {
            smooth.steps_done += 1;
            let frac = f64::from(smooth.steps_done) / f64::from(SMOOTH_STEPS);
            if frac >= 1.0 {
                self.cached_temperature = smooth.target;
                self.smooth = None;
                return;
            }
            let step = self.cached_temperature + (smooth.target - self.cached_temperature) * frac;
            if (step - self.cached_temperature).abs() > f64::from(TEMPERATURE_QUANTUM_K) {
                self.cached_temperature = step;
            }
        }
        self.smooth = Some(smooth);
    }

    /// Turn g-s-d smoothing off (`gsd_night_light_set_smooth_enabled`),
    /// as its own tests do: every temperature then applies at once.
    pub fn set_smooth_enabled(&mut self, enabled: bool) {
        if !enabled {
            self.smooth = None;
        }
        self.smooth_enabled = enabled;
    }

    /// Next 50 ms smoothing step, while an ease runs.
    fn smooth_deadline(&self) -> Option<std::time::Instant> {
        let smooth = self.smooth?;
        smooth
            .started
            .checked_add(SMOOTH_STEP * (smooth.steps_done + 1))
    }

    /// Publish `Sunrise`/`Sunset` for the automatic schedule as local
    /// fractional hours of the local date (g-s-d
    /// `update_cached_sunrise_sunset`, which only runs for the
    /// automatic schedule and keeps the last values otherwise). Polar
    /// day/night has no sunrise or sunset and keeps the last values.
    fn publish_sun_window(&mut self, unix_seconds: i64, utc_offset_secs: i64) {
        let NightLightSchedule::Automatic {
            latitude_deg,
            longitude_deg,
        } = self.config.schedule
        else {
            return;
        };
        let local_day = (unix_seconds + utc_offset_secs).div_euclid(86_400);
        let key = (
            latitude_deg.to_bits(),
            longitude_deg.to_bits(),
            local_day,
            utc_offset_secs,
        );
        if self.sun_key == Some(key) {
            return;
        }
        self.sun_key = Some(key);
        if let Daylight::Normal(window) =
            sun_window_for_day_of_year(latitude_deg, longitude_deg, day_of_year_for_days(local_day))
        {
            let offset_min = utc_offset_secs as f64 / 60.0;
            let local_hours = |utc_min: f64| ((utc_min + offset_min) / 60.0).rem_euclid(24.0);
            self.share.publish_sun(
                local_hours(window.sunrise_utc_min),
                local_hours(window.sunset_utc_min),
            );
        }
    }

    /// Actual render consumer: every clock outcome consumes monotonic preview
    /// expiry. A valid signed date uses the local schedule. An unrepresentable
    /// wall date retains a live explicit preview, then restores neutral.
    pub(crate) fn tick_at_system_time(
        &mut self,
        wall: std::time::SystemTime,
        monotonic_now: std::time::Instant,
    ) -> Option<ColorGains> {
        if let Some(unix) = system_unix_seconds(wall) {
            return self.tick_with_clock(unix, local_offset_seconds(unix), monotonic_now);
        }
        if let Some(preview_k) = self.share.preview_temperature(monotonic_now) {
            self.applied_temperature = Some(preview_k);
            return self.finish_tick(true, preview_k, monotonic_now);
        }
        self.applied_temperature = None;
        self.applied_gains = None;
        self.cached_temperature = f64::from(NEUTRAL_TEMPERATURE_K);
        self.smooth = None;
        self.share.publish(false, NEUTRAL_TEMPERATURE_K);
        if self.night_light_k != NEUTRAL_TEMPERATURE_K {
            self.night_light_k = NEUTRAL_TEMPERATURE_K;
            self.share
                .emit(ColorChange::Temperature(NEUTRAL_TEMPERATURE_K));
        }
        None
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

/// A controller with g-s-d smoothing off, as g-s-d's own night-light
/// tests run (`gsd_night_light_set_smooth_enabled (nlight, FALSE)`):
/// every temperature applies on the tick that computes it.
#[cfg(test)]
pub(crate) fn unsmoothed_controller(
    share: std::sync::Arc<NightLightShare>,
) -> NightLightController {
    let mut controller = NightLightController::new(share);
    controller.set_smooth_enabled(false);
    controller
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
        // colord's Planckian 1000 K entry.
        let gains = temperature_to_gains(MIN_TEMPERATURE_K);
        assert_eq!(gains.red, 1.0);
        assert_eq!(gains.blue, 0.0);
        assert_eq!(gains.green, 0.1817);
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
        assert!(auto.is_active_at_offset(day, 0));
        assert!(!auto.is_active_at_offset(day + 12 * 3600, 0));
        assert!(auto.is_active_at_offset(day + 23 * 3600, 0));
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
        assert!(!auto.is_active_at_offset(joue + 12 * 3600, 0));
        assert!(auto.is_active_at_offset(noel + 12 * 3600, 0));
        // The poles themselves take the elevation fallback, not NaN.
        assert_eq!(sun_window(90.0, 0.0, joue), Daylight::PolarDay);
        assert_eq!(sun_window(-90.0, 0.0, joue), Daylight::PolarNight);
        auto.schedule = NightLightSchedule::Automatic {
            latitude_deg: 90.0,
            longitude_deg: 0.0,
        };
        assert!(!auto.is_active_at_offset(joue, 0));
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
        assert!(auto.is_active_at_offset(day + 12 * 3600, 0));
        assert!(!auto.is_active_at_offset(day + 2 * 3600, 0));
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
        assert!(night.is_active_at_offset(day + 23 * 3600, 0));
        assert!(night.is_active_at_offset(day + 5 * 3600, 0));
        assert!(!night.is_active_at_offset(day + 12 * 3600, 0));
        assert!(!night.is_active_at_offset(day + 6 * 3600 + 60, 0));
        let evening = NightLightConfig {
            schedule: NightLightSchedule::Fixed {
                from_hour: 20.0,
                to_hour: 23.0,
            },
            ..night.clone()
        };
        assert!(evening.is_active_at_offset(day + 21 * 3600, 0));
        // 19:00 is inside the one-hour smear lead-in (active, but at
        // the neutral temperature); 18:59 is fully outside.
        assert!(evening.is_active_at_offset(day + 19 * 3600, 0));
        assert_eq!(
            evening.active_temperature_at_offset(day + 19 * 3600, 0),
            Some(NEUTRAL_TEMPERATURE_K)
        );
        assert!(!evening.is_active_at_offset(day + 19 * 3600 - 60, 0));
        let full_day = NightLightConfig {
            schedule: NightLightSchedule::Fixed {
                from_hour: 6.0,
                to_hour: 6.0,
            },
            ..night.clone()
        };
        // `from == to` reads as the full day, matching GNOME's
        // `frac_day_is_between` (equal bounds always match).
        assert!(full_day.is_active_at_offset(day + 6 * 3600, 0));
        assert!(full_day.is_active_at_offset(day + 18 * 3600, 0));
        let mut off = night;
        off.enabled = false;
        assert!(!off.is_active_at_offset(day + 23 * 3600, 0));
    }

    #[test]
    fn solar_math_is_steady_across_adjacent_days() {
        // Europe springs forward on 2026-03-29; the underlying UTC
        // solar math must not jump an hour across it (the schedule
        // framing follows local dates on top of these steady days).
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
    }

    #[test]
    fn fixed_window_follows_local_hours() {
        // The same UTC instant reads differently per zone: the
        // from/to hours are wall-clock hours, as GNOME stores them.
        let fixed = fixed_at(22.0, 6.0, 2700);
        let evening_utc = midnight(2026, 3, 20) + 23 * 3600;
        // 23:00 UTC is inside 22:00–06:00 at UTC...
        assert!(fixed.is_active_at_offset(evening_utc, 0));
        // ...still inside at UTC+2 (01:00 local)...
        assert!(fixed.is_active_at_offset(evening_utc, 2 * 3600));
        // ...but outside at UTC-5 (18:00 local, before the smear
        // lead-in, which starts at 21:00 local).
        assert!(!fixed.is_active_at_offset(evening_utc, -5 * 3600));
        // `from == to` still reads as the full day in every zone.
        let full_day = fixed_at(6.0, 6.0, 2700);
        assert!(full_day.is_active_at_offset(evening_utc, -5 * 3600));
        assert!(full_day.is_active_at_offset(evening_utc, 9 * 3600 + 1800));
    }

    #[test]
    fn automatic_window_follows_local_dates() {
        // Solar noon at the equator equinox: midday in absolute
        // terms, filter off in every zone — the offset only changes
        // which local dates key the solar math, never where the sun
        // is. Solar midnight is deep night in every zone alike: the
        // local-date pairing must never drop the covering night,
        // even where the local date differs from the UTC date.
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
        for offset in [0, 12 * 3600, -12 * 3600, 14 * 3600, -8 * 3600] {
            assert!(!auto.is_active_at_offset(day + 12 * 3600, offset));
            assert!(auto.is_active_at_offset(day, offset));
        }
    }

    #[test]
    fn system_path_matches_the_explicit_offset() {
        use std::sync::Arc;
        // Whatever zone this machine is in, the system-timezone
        // entry points must agree with the explicit-offset core at
        // the measured offset.
        let fixed = fixed_at(22.0, 6.0, 2700);
        let auto = NightLightConfig {
            enabled: true,
            temperature_k: DEFAULT_TEMPERATURE_K,
            disabled_until_tmw: None,
            schedule: NightLightSchedule::Automatic {
                latitude_deg: 51.5,
                longitude_deg: -0.12,
            },
        };
        for unix in [
            midnight(2026, 3, 20),
            midnight(2026, 3, 20) + 23 * 3600,
            midnight(2026, 7, 1) + 12 * 3600,
            midnight(2026, 12, 21) + 2 * 3600,
        ] {
            let offset = local_offset_seconds(unix);
            assert_eq!(
                fixed.active_temperature_at(unix),
                fixed.active_temperature_at_offset(unix, offset)
            );
            assert_eq!(
                auto.active_temperature_at(unix),
                auto.active_temperature_at_offset(unix, offset)
            );
            assert_eq!(
                fixed.is_active_at(unix),
                fixed.is_active_at_offset(unix, offset)
            );
            assert_eq!(
                auto.active_gains_at(unix),
                auto.active_gains_at_offset(unix, offset)
            );
            // The controller's system path agrees too, gains and
            // published state alike.
            let share_sys = Arc::new(NightLightShare::default());
            let mut sys = unsmoothed_controller(share_sys.clone());
            sys.config = fixed.clone();
            let share_exp = Arc::new(NightLightShare::default());
            let mut exp = unsmoothed_controller(share_exp.clone());
            exp.config = fixed.clone();
            assert_eq!(sys.tick(unix), exp.tick_with_offset(unix, offset));
            assert_eq!(
                (share_sys.active(), share_sys.temperature()),
                (share_exp.active(), share_exp.temperature())
            );
        }
    }

    #[test]
    fn local_offset_is_a_plausible_zone_offset() {
        // Every real zone sits within ±14 h of UTC, and the lookup
        // is deterministic within the process.
        for unix in [midnight(2026, 3, 20), midnight(2026, 7, 1)] {
            let offset = local_offset_seconds(unix);
            assert!(
                (-14 * 3600..=14 * 3600).contains(&offset),
                "offset={offset}"
            );
            assert_eq!(local_offset_seconds(unix), offset);
        }
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
        assert_eq!(
            night.active_temperature_at_offset(day + 21 * 3600 - 60, 0),
            None
        );
        assert!(!night.is_active_at_offset(day + 21 * 3600 - 60, 0));
        assert_eq!(
            night.active_temperature_at_offset(day + 21 * 3600, 0),
            Some(NEUTRAL_TEMPERATURE_K)
        );
        // Halfway through the lead-in: halfway between 6500 K and 2700 K.
        assert_eq!(
            night.active_temperature_at_offset(day + 21 * 3600 + 1800, 0),
            Some(4600)
        );
        // At `from`: the requested temperature.
        assert_eq!(
            night.active_temperature_at_offset(day + 22 * 3600, 0),
            Some(2700)
        );
        // Mid-window: full temperature.
        assert_eq!(
            night.active_temperature_at_offset(day + 2 * 3600, 0),
            Some(2700)
        );
        // An hour before `to` the trail-off starts; at `to` it ends.
        assert_eq!(
            night.active_temperature_at_offset(day + 5 * 3600, 0),
            Some(2700)
        );
        let trailing = night
            .active_temperature_at_offset(day + 5 * 3600 + 3540, 0)
            .unwrap();
        assert!((2700..6500).contains(&trailing), "trailing={trailing}");
        assert_eq!(night.active_temperature_at_offset(day + 6 * 3600, 0), None);
    }

    #[test]
    fn smear_clamps_to_short_windows() {
        // A 30-minute night: the smear clamps to 30 minutes, so the
        // lead-in starts at 21:30, not 21:00.
        let short = fixed_at(22.0, 22.5, 2700);
        let day = midnight(2026, 3, 20);
        assert_eq!(
            short.active_temperature_at_offset(day + 21 * 3600 + 1740, 0),
            None
        );
        let leading = short
            .active_temperature_at_offset(day + 21 * 3600 + 1860, 0)
            .unwrap();
        assert!((2700..=6500).contains(&leading), "leading={leading}");
        assert_eq!(
            short.active_temperature_at_offset(day + 22 * 3600, 0),
            Some(2700)
        );
        assert_eq!(
            short.active_temperature_at_offset(day + 22 * 3600 + 1800, 0),
            None
        );
        // A full-day window has no edges and no smear.
        let full = fixed_at(6.0, 6.0, 2700);
        assert_eq!(
            full.active_temperature_at_offset(day + 12 * 3600, 0),
            Some(2700)
        );
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
        assert_eq!(
            auto.active_temperature_at_offset(sunset - 2 * 3600, 0),
            None
        );
        let leading = auto.active_temperature_at_offset(sunset - 1800, 0).unwrap();
        assert!((2700..6500).contains(&leading), "leading={leading}");
        // An hour after sunset: full temperature.
        assert_eq!(
            auto.active_temperature_at_offset(sunset + 3600, 0),
            Some(2700)
        );
        // Half an hour before sunrise: ramping back up.
        let trailing = auto
            .active_temperature_at_offset(sunrise - 1800, 0)
            .unwrap();
        assert!((2700..6500).contains(&trailing), "trailing={trailing}");
        assert_eq!(auto.active_temperature_at_offset(sunrise, 0), None);
    }

    #[test]
    fn temp_disable_suppresses_until_sunrise_or_24h() {
        let day = midnight(2026, 3, 20);
        // Fixed 22:00-06:00, disabled at 23:00.
        let mut night = fixed_at(22.0, 6.0, 2700);
        night.disabled_until_tmw = Some(day + 23 * 3600);
        // Still night, no sunrise since: suppressed.
        assert!(!night.is_active_at_offset(day + 26 * 3600, 0));
        // Past the 06:00 sunrise a new night starts unsuppressed.
        assert!(night.is_active_at_offset(day + 47 * 3600, 0));
        // A clock step back never clears the disable.
        assert!(!night.is_active_at_offset(day + 22 * 3600, 0));
        // Manual has no sunrise: the 24 h lapse alone clears it.
        let mut manual = NightLightConfig {
            enabled: true,
            ..NightLightConfig::default()
        };
        let set_at = day + 12 * 3600;
        manual.disabled_until_tmw = Some(set_at);
        assert!(!manual.is_active_at_offset(set_at + 3600, 0));
        assert!(!manual.is_active_at_offset(set_at + 86_400, 0));
        assert!(manual.is_active_at_offset(set_at + 86_401, 0));
    }

    #[test]
    fn temp_disable_sunrise_reset_needs_a_sunrise_between() {
        let day = midnight(2026, 3, 20);
        // Disabled at 05:00, inside the window: 05:30 sees no
        // sunrise since 05:00, so it stays suppressed.
        let mut night = fixed_at(22.0, 6.0, 2700);
        night.disabled_until_tmw = Some(day + 5 * 3600);
        assert!(!night.is_active_at_offset(day + 5 * 3600 + 1800, 0));
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
        assert!(!polar.is_active_at_offset(noel + 20 * 3600, 0));
        assert!(polar.is_active_at_offset(noel + 12 * 3600 + 86_401, 0));
    }

    #[test]
    fn temp_disable_clears_at_the_local_to_hour() {
        // Fixed 22:00–06:00 at UTC-5, disabled at 23:00 local
        // (04:00 UTC). The clearing sunrise is the local 06:00,
        // i.e. 11:00 UTC — a UTC reading would clear at 06:00 UTC
        // instead, five hours early.
        let day = midnight(2026, 3, 20);
        let mut night = fixed_at(22.0, 6.0, 2700);
        night.disabled_until_tmw = Some(day + 4 * 3600);
        // 02:00 local (07:00 UTC): in the window, and past the UTC
        // 06:00 — still suppressed, because the local 06:00 (11:00
        // UTC) has not fallen between the disable and now.
        assert!(!night.is_active_at_offset(day + 7 * 3600, -5 * 3600));
        // 22:46 local the next evening: the local 06:00 fell
        // between (and the 24 h lapse has not: only 23.8 h passed),
        // so the filter runs again.
        assert!(night.is_active_at_offset(day + 100_000, -5 * 3600));
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
        let mut controller = unsmoothed_controller(share.clone());
        // Seed an enabled fixed config directly: refresh tests cover
        // the reader path below.
        controller.config = fixed_at(22.0, 6.0, 2700);
        let day = midnight(2026, 3, 20);
        // Mid-window: full temperature, gains installed, published.
        let gains = controller.tick_with_offset(day + 2 * 3600, 0).unwrap();
        assert_eq!(gains, temperature_to_gains(2700));
        assert!(share.active());
        assert_eq!(share.temperature(), 2700);
        // A 10 K smear step holds the applied temperature.
        controller.config.set_temperature_k(2710);
        assert_eq!(controller.tick_with_offset(day + 2 * 3600, 0), Some(gains));
        assert_eq!(share.temperature(), 2700);
        // An 11 K step republishes.
        controller.config.set_temperature_k(2711);
        let stepped = controller.tick_with_offset(day + 2 * 3600, 0).unwrap();
        assert_eq!(stepped, temperature_to_gains(2711));
        assert_eq!(share.temperature(), 2711);
        // Leaving the window clears everything (neutral restore).
        assert_eq!(controller.tick_with_offset(day + 12 * 3600, 0), None);
        assert!(!share.active());
        assert_eq!(share.temperature(), NEUTRAL_TEMPERATURE_K);
        assert_eq!(controller.effect_bits(), [1.0f32.to_bits(); 3]);
        // Re-entering republishes even at a small delta: the trip
        // through inactive reset the hysteresis anchor.
        controller.config.set_temperature_k(2705);
        let reentered = controller.tick_with_offset(day + 26 * 3600, 0).unwrap();
        assert_eq!(reentered, temperature_to_gains(2705));
    }

    #[test]
    fn controller_never_programs_identity() {
        use std::sync::Arc;
        let share = Arc::new(NightLightShare::default());
        let mut controller = unsmoothed_controller(share.clone());
        controller.config = fixed_at(22.0, 6.0, NEUTRAL_TEMPERATURE_K);
        let day = midnight(2026, 3, 20);
        // Active at the neutral temperature: published as active,
        // but no override to install.
        assert_eq!(controller.tick_with_offset(day + 2 * 3600, 0), None);
        assert!(share.active());
        assert_eq!(share.temperature(), NEUTRAL_TEMPERATURE_K);
        assert_eq!(controller.effect_bits(), [1.0f32.to_bits(); 3]);
        // A fresh controller starts neutral too.
        let fresh = unsmoothed_controller(Arc::new(NightLightShare::default()));
        assert_eq!(fresh.current_gains(), None);
        assert_eq!(fresh.effect_bits(), [1.0f32.to_bits(); 3]);
    }

    #[test]
    fn controller_refresh_preserves_temp_disable_and_fails_closed() {
        use std::sync::Arc;
        use std::time::{Duration, Instant};
        let share = Arc::new(NightLightShare::default());
        let mut controller = unsmoothed_controller(share.clone());
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
        let mut controller = unsmoothed_controller(share.clone());
        controller.config = fixed_at(22.0, 6.0, 2700);
        let day = midnight(2026, 3, 20);
        assert!(controller.tick_with_offset(day + 2 * 3600, 0).is_some());
        // The D-Bus side records a disable: the next tick suppresses.
        share.set_disabled_until_tmw(Some(day + 2 * 3600));
        assert_eq!(controller.tick_with_offset(day + 3 * 3600, 0), None);
        assert!(!share.active());
        // Clearing re-arms the filter.
        share.set_disabled_until_tmw(None);
        assert!(controller.tick_with_offset(day + 3 * 3600, 0).is_some());
    }

    #[test]
    fn preview_forces_temperature_then_expires_to_schedule() {
        use std::sync::Arc;
        use std::time::{Duration, Instant};
        // Disabled config: without a preview nothing ever applies.
        let share = Arc::new(NightLightShare::default());
        let mut controller = unsmoothed_controller(share.clone());
        let noon = midnight(2026, 3, 20) + 12 * 3600;
        let t0 = Instant::now();
        assert_eq!(controller.tick_with_clock(noon, 0, t0), None);
        // The slider drags: five seconds at 2000 K, applied on the
        // next tick even though the filter is off.
        assert_eq!(DEFAULT_PREVIEW_SECONDS, 5);
        assert_eq!(
            share.set_preview_at(2000, t0, DEFAULT_PREVIEW_SECONDS),
            Ok(())
        );
        let gains = controller.tick_with_clock(noon, 0, t0).unwrap();
        assert_eq!(gains, temperature_to_gains(2000));
        assert!(share.active());
        assert_eq!(share.temperature(), 2000);
        // Still live a second before expiry...
        assert!(controller
            .tick_with_clock(noon, 0, t0 + Duration::from_secs(4))
            .is_some());
        // ...then the schedule resumes on its own: disabled means
        // neutral restore with no further calls.
        assert_eq!(
            controller.tick_with_clock(noon, 0, t0 + Duration::from_secs(5)),
            None
        );
        assert!(!share.active());
        assert_eq!(share.temperature(), NEUTRAL_TEMPERATURE_K);
        assert_eq!(share.preview_temperature(t0 + Duration::from_secs(5)), None);
    }

    #[test]
    fn preview_overrides_an_inactive_schedule_and_bypasses_hysteresis() {
        use std::sync::Arc;
        use std::time::{Duration, Instant};
        let share = Arc::new(NightLightShare::default());
        let mut controller = unsmoothed_controller(share.clone());
        controller.config = fixed_at(22.0, 6.0, 2700);
        let noon = midnight(2026, 3, 20) + 12 * 3600;
        let t0 = Instant::now();
        // Midday: the schedule is off.
        assert_eq!(controller.tick_with_clock(noon, 0, t0), None);
        // A preview forces the filter on midday.
        assert_eq!(share.set_preview_at(4500, t0, 30), Ok(()));
        let previewed = controller.tick_with_clock(noon, 0, t0).unwrap();
        assert_eq!(previewed, temperature_to_gains(4500));
        // Each slider step shows immediately: a 10 K move applies at
        // once instead of holding the hysteresis anchor.
        let t1 = t0 + Duration::from_secs(1);
        assert_eq!(share.set_preview_at(4510, t1, 30), Ok(()));
        let stepped = controller.tick_with_clock(noon, 0, t1).unwrap();
        assert_eq!(stepped, temperature_to_gains(4510));
        // Expiry falls back to the (inactive) schedule.
        assert_eq!(
            controller.tick_with_clock(noon, 0, t1 + Duration::from_secs(30)),
            None
        );
        assert!(!share.active());
    }

    #[test]
    fn preview_clamps_and_refresh_keeps_it() {
        use std::sync::Arc;
        use std::time::{Duration, Instant};
        let share = Arc::new(NightLightShare::default());
        let mut controller = unsmoothed_controller(share.clone());
        controller.config = fixed_at(22.0, 6.0, 2700);
        let day = midnight(2026, 3, 20);
        let t0 = Instant::now();
        // Out-of-range temperatures clamp into range.
        assert_eq!(share.set_preview_at(u32::MAX, t0, 60), Ok(()));
        assert_eq!(share.preview_temperature(t0), Some(MAX_TEMPERATURE_K));
        assert_eq!(share.set_preview_at(0, t0, 60), Ok(()));
        assert_eq!(share.preview_temperature(t0), Some(MIN_TEMPERATURE_K));
        share.clear_preview();
        assert_eq!(share.preview_temperature(t0), None);
        // A settings refresh replaces the config but the live
        // preview (which sits in the share, not the config)
        // survives it.
        assert_eq!(share.set_preview_at(3000, t0, 60), Ok(()));
        let now = Instant::now();
        assert!(controller.maybe_refresh(now, || Ok::<_, ReadError>(fixed_at(22.0, 6.0, 4000))));
        assert_eq!(controller.config().temperature_k(), 4000);
        let gains = controller
            .tick_with_clock(day + 2 * 3600, 0, t0 + Duration::from_secs(1))
            .unwrap();
        assert_eq!(gains, temperature_to_gains(3000));
    }

    #[test]
    fn preview_at_neutral_publishes_without_an_override() {
        use std::sync::Arc;
        use std::time::Instant;
        // Like the schedule path, a neutral preview never programs
        // an identity transform — but it still publishes active.
        let share = Arc::new(NightLightShare::default());
        let mut controller = unsmoothed_controller(share.clone());
        let day = midnight(2026, 3, 20);
        let t0 = Instant::now();
        assert_eq!(share.set_preview_at(NEUTRAL_TEMPERATURE_K, t0, 5), Ok(()));
        assert_eq!(controller.tick_with_clock(day, 0, t0), None);
        assert!(share.active());
        assert_eq!(share.temperature(), NEUTRAL_TEMPERATURE_K);
        assert_eq!(controller.effect_bits(), [1.0f32.to_bits(); 3]);
    }

    #[test]
    fn preview_rejects_invalid_durations_without_mutation() {
        use std::sync::Arc;
        use std::time::{Duration, Instant};
        // g-s-d contract: durations outside 1..=120 are InvalidArgument.
        assert_eq!(MAX_PREVIEW_SECONDS, 120);
        let share = Arc::new(NightLightShare::default());
        let t0 = Instant::now();
        for bad in [0, 121, u32::MAX] {
            assert_eq!(
                share.set_preview_at(3000, t0, bad),
                Err(PreviewRejected::InvalidDuration(bad)),
                "duration {bad} must be rejected"
            );
            // Rejection records nothing when no preview exists.
            assert_eq!(share.preview_temperature(t0), None);
        }
        // ...and leaves a live preview exactly as it was.
        for bad in [0, 121, u32::MAX] {
            assert_eq!(share.set_preview_at(3000, t0, 60), Ok(()));
            assert_eq!(
                share.set_preview_at(4000, t0, bad),
                Err(PreviewRejected::InvalidDuration(bad)),
                "duration {bad} must be rejected"
            );
            assert_eq!(share.preview_temperature(t0), Some(3000));
            assert_eq!(
                share.preview_temperature(t0 + Duration::from_secs(60)),
                None,
                "original deadline must be intact after duration {bad}"
            );
        }
        // Boundary durations are accepted.
        assert_eq!(share.set_preview_at(3000, t0, 1), Ok(()));
        assert_eq!(share.set_preview_at(3000, t0, 120), Ok(()));
        // The production entry point validates identically.
        assert_eq!(
            share.set_preview(3000, 0),
            Err(PreviewRejected::InvalidDuration(0))
        );
        assert_eq!(
            share.set_preview(3000, 121),
            Err(PreviewRejected::InvalidDuration(121))
        );
        assert_eq!(share.set_preview(3000, 5), Ok(()));
    }

    #[test]
    fn preview_repeated_request_replaces_the_timer() {
        use std::sync::Arc;
        use std::time::{Duration, Instant};
        // Like g-s-d's g_source_remove + g_timeout_add_seconds: each
        // slider step restarts the countdown from its own instant.
        let share = Arc::new(NightLightShare::default());
        let t0 = Instant::now();
        assert_eq!(share.set_preview_at(2000, t0, 5), Ok(()));
        let t1 = t0 + Duration::from_secs(4);
        assert_eq!(share.set_preview_at(2100, t1, 5), Ok(()));
        assert_eq!(
            share.preview_temperature(t1 + Duration::from_secs(4)),
            Some(2100),
            "replaced timer runs from the second request"
        );
        assert_eq!(share.preview_temperature(t1 + Duration::from_secs(5)), None);
    }

    #[test]
    fn preview_poison_reports_unavailable_without_claiming_success() {
        use std::sync::Arc;
        use std::time::Instant;
        let share = Arc::new(NightLightShare::default());
        let t0 = Instant::now();
        // Poison the preview mutex the honest way: panic while
        // holding the guard, then swallow the unwind.
        let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = share.preview.lock().unwrap();
            panic!("deliberate poison for the unavailable control");
        }));
        assert!(poisoned.is_err());
        // Recording now fails instead of claiming success...
        assert_eq!(
            share.set_preview_at(3000, t0, 5),
            Err(PreviewRejected::Unavailable)
        );
        // ...the deadline query degrades to no deadline...
        assert_eq!(share.preview_deadline_at(t0), None);
        // ...and reads degrade to the schedule path rather than a
        // stuck preview.
        assert_eq!(share.preview_temperature(t0), None);
    }

    #[test]
    fn preview_deadline_tracks_live_preview_exactly() {
        use std::sync::Arc;
        use std::time::{Duration, Instant};
        let share = Arc::new(NightLightShare::default());
        let t0 = Instant::now();
        assert_eq!(share.preview_deadline_at(t0), None);
        assert_eq!(share.set_preview_at(2000, t0, 5), Ok(()));
        assert_eq!(
            share.preview_deadline_at(t0),
            Some(t0 + Duration::from_secs(5))
        );
        // Expired but unconsumed: due-now obligation, not silence —
        // the expiry may have fallen between the render tick and
        // next_wait, and the restoring tick must still run.
        let past = t0 + Duration::from_secs(5);
        assert_eq!(share.preview_deadline_at(past), Some(past));
        // One consuming read clears the obligation.
        assert_eq!(share.preview_temperature(past), None);
        assert_eq!(share.preview_deadline_at(past), None);
    }

    #[test]
    fn next_deadline_prefers_preview_and_bounds_settings_poll() {
        use std::sync::Arc;
        use std::time::{Duration, Instant};
        let share = Arc::new(NightLightShare::default());
        let mut controller = unsmoothed_controller(share.clone());
        let t0 = Instant::now();
        // Disabled, never read, no preview: due now. There is no
        // settings watch, so "no poll while disabled" would miss a
        // later enable without unrelated input.
        assert_eq!(controller.next_deadline_at(t0), Some(t0));
        // Enabled, never read: due now (one catch-up tick).
        controller.config = fixed_at(22.0, 6.0, 2700);
        assert_eq!(controller.next_deadline_at(t0), Some(t0));
        // Fresh read: due exactly one interval later — bounded, no spin.
        // (maybe_refresh reports "changed", so the identical value
        // returns false while still stamping last_read.)
        assert!(!controller.maybe_refresh(t0, || Ok::<_, ReadError>(fixed_at(22.0, 6.0, 2700))));
        assert_eq!(controller.next_deadline_at(t0), Some(t0 + READ_INTERVAL));
        // A live preview expiring sooner wins over the settings arm.
        assert_eq!(share.set_preview_at(2000, t0, 2), Ok(()));
        assert_eq!(
            controller.next_deadline_at(t0),
            Some(t0 + Duration::from_secs(2))
        );
        // Stale settings are due now, never in the past.
        let late = t0 + READ_INTERVAL + Duration::from_secs(60);
        assert_eq!(controller.next_deadline_at(late), Some(late));
    }

    #[test]
    fn disabled_filter_still_polls_and_observes_later_enable() {
        use std::sync::Arc;
        use std::time::{Duration, Instant};
        let share = Arc::new(NightLightShare::default());
        let mut controller = unsmoothed_controller(share.clone());
        let t0 = Instant::now();
        // Disabled with a fresh read: next poll one interval out —
        // bounded even while off.
        assert!(!controller.maybe_refresh(t0, || Ok::<_, ReadError>(NightLightConfig::default())));
        assert!(!controller.config.enabled);
        assert_eq!(controller.next_deadline_at(t0), Some(t0 + READ_INTERVAL));
        // Past the interval the poll is due now, so the loop
        // re-reads and picks up the enable with no other input.
        let late = t0 + READ_INTERVAL + Duration::from_secs(1);
        assert_eq!(controller.next_deadline_at(late), Some(late));
        assert!(controller.maybe_refresh(late, || Ok::<_, ReadError>(fixed_at(22.0, 6.0, 2700))));
        assert!(controller.config.enabled);
    }

    #[test]
    fn expiry_between_tick_and_wait_keeps_one_restoring_tick() {
        use std::sync::Arc;
        use std::time::{Duration, Instant};
        let share = Arc::new(NightLightShare::default());
        let t0 = Instant::now();
        // The render tick applies the live preview...
        assert_eq!(share.set_preview_at(2000, t0, 5), Ok(()));
        assert_eq!(share.preview_temperature(t0), Some(2000));
        // ...then the expiry falls before next_wait runs. The
        // deadline must demand a catch-up tick, not silence.
        let between = t0 + Duration::from_secs(5);
        assert_eq!(share.preview_deadline_at(between), Some(between));
        // That tick consumes the record and restores the schedule;
        // afterwards no preview obligation remains.
        assert_eq!(share.preview_temperature(between), None);
        assert_eq!(share.preview_deadline_at(between), None);
        // And an explicit clear also releases the obligation.
        assert_eq!(share.set_preview_at(2000, between, 5), Ok(()));
        share.clear_preview();
        assert_eq!(share.preview_deadline_at(between), None);
    }

    #[test]
    fn actual_clock_consumer_handles_pre_epoch_and_consumes_monotonic_expiry() {
        use std::time::{Duration, Instant, UNIX_EPOCH};
        assert_eq!(
            system_unix_seconds(UNIX_EPOCH - Duration::from_secs(1)),
            Some(-1)
        );
        assert_eq!(
            system_unix_seconds(UNIX_EPOCH - Duration::from_millis(1)),
            Some(-1)
        );
        assert_eq!(
            system_unix_seconds(UNIX_EPOCH + Duration::from_millis(1)),
            Some(0)
        );
        let share = std::sync::Arc::new(NightLightShare::default());
        let mut controller = unsmoothed_controller(share.clone());
        let t0 = Instant::now();
        share.set_preview_at(2000, t0, 5).unwrap();
        assert_eq!(
            controller.tick_at_system_time(UNIX_EPOCH - Duration::from_secs(1), t0),
            Some(temperature_to_gains(2000))
        );
        assert_eq!(
            controller.tick_at_system_time(
                UNIX_EPOCH - Duration::from_secs(1),
                t0 + Duration::from_secs(5)
            ),
            None
        );
        assert_eq!(share.preview_deadline_at(t0 + Duration::from_secs(5)), None);
        assert!(!share.active());
        assert_eq!(controller.effect_bits(), [1.0f32.to_bits(); 3]);
    }

    #[test]
    fn disable_expiry_clears_property_without_erasing_a_newer_timestamp() {
        let share = std::sync::Arc::new(NightLightShare::default());
        let mut controller = unsmoothed_controller(share.clone());
        let day = midnight(2026, 3, 20);
        controller.config = fixed_at(22.0, 6.0, 2700);
        let previous = day + 23 * 3600;
        share.set_disabled_until_tmw(Some(previous));
        assert_eq!(controller.tick_with_offset(day + 26 * 3600, 0), None);
        assert_eq!(share.disabled_until_tmw(), Some(previous));
        assert!(controller.tick_with_offset(day + 47 * 3600, 0).is_some());
        assert_eq!(share.disabled_until_tmw(), None);
        let newer = day + 48 * 3600;
        share.set_disabled_until_tmw(Some(previous));
        share.set_disabled_until_tmw(Some(newer));
        assert!(!share.clear_disabled_until_if(previous));
        assert_eq!(share.disabled_until_tmw(), Some(newer));
        assert!(share.clear_disabled_until_if(newer));
        assert!(!share.clear_disabled_until_if(newer));
    }

    #[test]
    fn preview_ignores_wall_clock_steps() {
        use std::sync::Arc;
        use std::time::{Duration, Instant};
        // NTP steps and backward jumps cannot prolong or cut the
        // preview: only the monotonic clock drives expiry, and the
        // wall clock is never consulted for liveness.
        let share = Arc::new(NightLightShare::default());
        let mut controller = unsmoothed_controller(share.clone());
        let noon = midnight(2026, 3, 20) + 12 * 3600;
        let t0 = Instant::now();
        assert_eq!(share.set_preview_at(2000, t0, 5), Ok(()));
        // A day forward or back on the wall clock, same instant:
        // identical liveness.
        for wall in [noon, noon + 86_400, noon - 86_400] {
            assert_eq!(
                share.preview_temperature(t0 + Duration::from_secs(4)),
                Some(2000)
            );
            assert!(controller.tick_with_clock(wall, 0, t0).is_some());
        }
        // Past the monotonic deadline the preview is gone no matter
        // what the wall clock claims.
        for wall in [noon, noon + 86_400, noon - 86_400] {
            assert_eq!(
                controller.tick_with_clock(wall, 0, t0 + Duration::from_secs(6)),
                None
            );
        }
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

    #[test]
    fn system_zone_drives_schedule_across_dst_and_runtime_changes() {
        use std::sync::Arc;
        use std::time::{Duration, Instant, UNIX_EPOCH};
        // `TZ` is process-global: run the body in a single-threaded
        // child of this test binary so no sibling test sees it.
        const CHILD: &str = "TUNA_NIGHT_LIGHT_TZ_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "night_light::tests::system_zone_drives_schedule_across_dst_and_runtime_changes",
                    "--test-threads=1",
                ])
                .env(CHILD, "1")
                .status()
                .unwrap();
            assert!(status.success(), "child failed: {status}");
            return;
        }
        // POSIX rules, so no tzdata is needed: US Eastern with DST.
        std::env::set_var("TZ", "EST5EDT,M3.2.0,M11.1.0");
        // 2026-03-08 07:00 UTC: 02:00 EST jumps to 03:00 EDT.
        let dst = midnight(2026, 3, 8) + 7 * 3600;
        assert_eq!(local_offset_seconds(dst - 1), -5 * 3600);
        assert_eq!(local_offset_seconds(dst), -4 * 3600);
        let share = Arc::new(NightLightShare::default());
        let mut controller = unsmoothed_controller(share.clone());
        controller.config = fixed_at(22.0, 3.0, 3000);
        let wall = |unix: i64| UNIX_EPOCH + Duration::from_secs(unix as u64);
        let mono = Instant::now();
        // 01:59:59 EST is inside 22:00–03:00 (before the end smear
        // from 02:00)...
        assert_eq!(
            controller.tick_at_system_time(wall(dst - 1), mono),
            Some(temperature_to_gains(3000))
        );
        // ...and one second later the wall clock reads 03:00 EDT: off.
        assert_eq!(controller.tick_at_system_time(wall(dst), mono), None);
        assert!(!share.active());
        // 01:30 UTC is in the window in UTC framing, but is 20:30 EST
        // locally, before the 21:00 lead-in: the local zone wins.
        let evening = midnight(2026, 3, 8) + 90 * 60;
        assert!(controller.config.is_active_at_offset(evening, 0));
        assert_eq!(controller.tick_at_system_time(wall(evening), mono), None);
        // A runtime zone change moves the schedule on the next tick:
        // 14:00 UTC is 09:00 EST (off) but 23:00 JST (on).
        let afternoon = midnight(2026, 3, 9) + 14 * 3600;
        assert_eq!(controller.tick_at_system_time(wall(afternoon), mono), None);
        std::env::set_var("TZ", "JST-9");
        assert_eq!(
            controller.tick_at_system_time(wall(afternoon + 1), mono),
            Some(temperature_to_gains(3000))
        );
        assert_eq!(share.temperature(), 3000);
    }

    #[test]
    fn temperature_jumps_ease_over_five_seconds_like_gsd() {
        use std::sync::Arc;
        use std::time::{Duration, Instant};
        let share = Arc::new(NightLightShare::default());
        let mut controller = NightLightController::new(share.clone());
        controller.config = NightLightConfig {
            enabled: true,
            temperature_k: 2700,
            disabled_until_tmw: None,
            schedule: NightLightSchedule::Manual,
        };
        let noon = midnight(2026, 3, 20) + 12 * 3600;
        let t0 = Instant::now();
        // The first 50 ms step has not fired yet: still neutral, and
        // the loop is asked to wake for it.
        assert_eq!(controller.tick_with_clock(noon, 0, t0), None);
        assert!(share.active());
        assert_eq!(share.temperature(), NEUTRAL_TEMPERATURE_K);
        controller.last_read = Some(t0);
        assert_eq!(controller.next_deadline_at(t0), Some(t0 + SMOOTH_STEP));
        // gsd_night_light_smooth_cb: cached += (target - cached) * frac.
        let mut expected = f64::from(NEUTRAL_TEMPERATURE_K);
        let mut by_step = Vec::new();
        for step in 1..SMOOTH_STEPS {
            let next = expected + (2700.0 - expected) * f64::from(step) / f64::from(SMOOTH_STEPS);
            if (next - expected).abs() > f64::from(TEMPERATURE_QUANTUM_K) {
                expected = next;
            }
            by_step.push(expected.round() as u32);
        }
        controller.tick_with_clock(noon, 0, t0 + SMOOTH_STEP);
        assert_eq!(share.temperature(), by_step[0]);
        assert_eq!(share.temperature(), 6462);
        // Frame-rate independent: one late tick lands where 16 ms
        // ticks do.
        let mut framed = NightLightController::new(Arc::new(NightLightShare::default()));
        framed.config = controller.config.clone();
        let mut at = Duration::ZERO;
        while at <= Duration::from_millis(2500) {
            framed.tick_with_clock(noon, 0, t0 + at);
            at += Duration::from_millis(16);
        }
        framed.tick_with_clock(noon, 0, t0 + Duration::from_millis(2500));
        controller.tick_with_clock(noon, 0, t0 + Duration::from_millis(2500));
        assert_eq!(share.temperature(), by_step[49]);
        assert_eq!(framed.share.temperature(), by_step[49]);
        assert!(share.temperature() > 2700 && share.temperature() < 6462);
        // At 5 s the ease lands exactly and stops asking for wakes.
        let gains = controller.tick_with_clock(noon, 0, t0 + Duration::from_secs(5));
        assert_eq!(gains, Some(temperature_to_gains(2700)));
        assert_eq!(controller.smooth_deadline(), None);
        // Switching off eases back the same way; NightLightActive
        // drops at once while the colour still eases.
        controller.config.enabled = false;
        let t1 = t0 + Duration::from_secs(6);
        assert_eq!(
            controller.tick_with_clock(noon, 0, t1),
            Some(temperature_to_gains(2700))
        );
        assert!(!share.active());
        assert_eq!(
            controller.tick_with_clock(noon, 0, t1 + Duration::from_secs(5)),
            None
        );
        assert_eq!(share.temperature(), NEUTRAL_TEMPERATURE_K);
    }

    #[test]
    fn temperature_property_applies_until_night_light_moves() {
        use std::sync::Arc;
        use std::time::{Duration, Instant};
        let share = Arc::new(NightLightShare::default());
        let mut controller = unsmoothed_controller(share.clone());
        let noon = midnight(2026, 3, 20) + 12 * 3600;
        let t0 = Instant::now();
        assert_eq!(share.set_temperature_override(999), Err(999));
        assert_eq!(share.set_temperature_override(10_001), Err(10_001));
        assert_eq!(controller.tick_with_clock(noon, 0, t0), None);
        // A write applies at once and survives schedule ticks that do
        // not move Night Light's own temperature...
        assert_eq!(share.set_temperature_override(4000), Ok(()));
        assert_eq!(share.temperature(), 4000);
        for _ in 0..3 {
            assert_eq!(
                controller.tick_with_clock(noon, 0, t0),
                Some(temperature_to_gains(4000))
            );
        }
        assert!(!share.active());
        // ...and Night Light moving (a preview here) overwrites it,
        // and it stays gone after the preview expires.
        share.set_preview_at(2000, t0, 1).unwrap();
        assert_eq!(
            controller.tick_with_clock(noon, 0, t0),
            Some(temperature_to_gains(2000))
        );
        assert_eq!(
            controller.tick_with_clock(noon, 0, t0 + Duration::from_secs(1)),
            None
        );
        assert_eq!(share.temperature(), NEUTRAL_TEMPERATURE_K);
    }

    #[test]
    fn sunrise_and_sunset_publish_local_hours_for_the_automatic_schedule() {
        use std::sync::Arc;
        use std::time::Instant;
        let share = Arc::new(NightLightShare::default());
        let mut controller = unsmoothed_controller(share.clone());
        let noon = midnight(2026, 3, 20) + 12 * 3600;
        // No location yet: g-s-d's -1 sentinel, also for the fixed
        // schedule.
        controller.config = fixed_at(20.0, 6.0, 2700);
        controller.tick_with_clock(noon, 3600, Instant::now());
        assert_eq!((share.sunrise(), share.sunset()), (-1.0, -1.0));
        // Equator, Greenwich, equinox, UTC+1: about 07:00 and 19:00
        // local.
        controller.config.schedule = NightLightSchedule::Automatic {
            latitude_deg: 0.0,
            longitude_deg: 0.0,
        };
        controller.tick_with_clock(noon, 3600, Instant::now());
        assert!((share.sunrise() - 7.0).abs() < 0.25, "{}", share.sunrise());
        assert!((share.sunset() - 19.1).abs() < 0.25, "{}", share.sunset());
        // The same instant in UTC-5 reads five hours earlier.
        controller.tick_with_clock(noon, -5 * 3600, Instant::now());
        assert!((share.sunrise() - 1.0).abs() < 0.25, "{}", share.sunrise());
    }

    #[test]
    fn color_changes_follow_gsd_notifications_only_on_change() {
        use std::sync::Arc;
        use std::time::{Duration, Instant};
        let share = Arc::new(NightLightShare::default());
        let changes = share.subscribe_changes();
        let mut controller = NightLightController::new(share.clone());
        controller.config = NightLightConfig {
            enabled: true,
            temperature_k: 2700,
            disabled_until_tmw: None,
            schedule: NightLightSchedule::Automatic {
                latitude_deg: 0.0,
                longitude_deg: 0.0,
            },
        };
        let midnight_utc = midnight(2026, 3, 20);
        let t0 = Instant::now();
        controller.tick_with_clock(midnight_utc, 0, t0);
        let first: Vec<_> = changes.try_iter().collect();
        assert!(
            matches!(first[0], ColorChange::Sunrise(h) if (h - 6.0).abs() < 0.25),
            "{first:?}"
        );
        assert!(
            matches!(first[1], ColorChange::Sunset(h) if (h - 18.1).abs() < 0.25),
            "{first:?}"
        );
        assert_eq!(first[2..], [ColorChange::NightLightActive(true)]);
        // The same instant again: nothing moved, nothing announced.
        controller.tick_with_clock(midnight_utc, 0, t0);
        assert_eq!(changes.try_iter().count(), 0);
        // Each smoothing step that moves the temperature is announced,
        // ending exactly on the target; Sunrise/Sunset stay quiet the
        // same local day.
        let mut temps = Vec::new();
        for step in 1..=SMOOTH_STEPS {
            controller.tick_with_clock(midnight_utc + 60, 0, t0 + SMOOTH_STEP * step);
            for change in changes.try_iter() {
                match change {
                    ColorChange::Temperature(kelvin) => temps.push(kelvin),
                    other => panic!("unexpected {other:?}"),
                }
            }
        }
        assert_eq!(temps.first(), Some(&6462));
        assert_eq!(temps.last(), Some(&2700));
        assert!(temps.windows(2).all(|pair| pair[0] > pair[1]), "{temps:?}");
        // A Temperature property write announces nothing; neither do
        // ticks that keep Night Light's own temperature.
        share.set_temperature_override(5000).unwrap();
        controller.tick_with_clock(midnight_utc + 60, 0, t0 + Duration::from_secs(6));
        assert_eq!(changes.try_iter().count(), 0);
        // DisabledUntilTomorrow: one signal per flip, then the
        // schedule deactivates and starts easing back.
        share.set_disabled_until_tmw(Some(midnight_utc));
        share.set_disabled_until_tmw(Some(midnight_utc + 1));
        controller.tick_with_clock(midnight_utc + 60, 0, t0 + Duration::from_secs(7));
        assert_eq!(
            changes.try_iter().collect::<Vec<_>>(),
            [
                ColorChange::DisabledUntilTomorrow(true),
                ColorChange::NightLightActive(false),
            ]
        );
        // The loop retiring the disable at sunrise (gsd's recheck) is
        // announced too, exactly once.
        let after_sunrise = midnight_utc + 7 * 3600;
        controller.tick_with_clock(after_sunrise, 0, t0 + Duration::from_secs(7));
        assert_eq!(share.disabled_until_tmw(), None);
        assert_eq!(
            changes.try_iter().collect::<Vec<_>>(),
            [ColorChange::DisabledUntilTomorrow(false)]
        );
        controller.tick_with_clock(after_sunrise, 0, t0 + Duration::from_secs(7));
        assert_eq!(changes.try_iter().count(), 0);
    }
}
