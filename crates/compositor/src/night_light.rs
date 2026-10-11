//! Night Light schedule model (#339).
//!
//! GNOME's Night Light warms the display's colour temperature at night.
//! The quick-settings tile already writes
//! `night-light-enabled`; this module adds the schedule side: whether
//! the filter is active right now, at what temperature, and what
//! channel gains that temperature maps to. It is pure scheduling
//! arithmetic over Unix timestamps:
//!
//! - [`NightLightConfig`]: manual on/off plus the automatic
//!   sunset-to-sunrise window from a latitude/longitude, or a fixed
//!   from/to window. The temperature clamps to
//!   [`MIN_TEMPERATURE_K`]..=[`MAX_TEMPERATURE_K`].
//! - [`temperature_to_gains`]: colour-temperature-to-gains mapping,
//!   normalised so [`NEUTRAL_TEMPERATURE_K`] is unity.
//! - Key inventory ([`COLOR_SCHEMA`], [`INTERFACE_SCHEMA`]): the
//!   GSettings names this model mirrors. Read-only: nothing here
//!   talks to dconf and no daemon runs; a future settings reader
//!   fills [`NightLightConfig`] from these keys.
//!
//! All time math is in UTC over Unix timestamps, so daylight-saving
//! transitions cannot shift the window: the same UTC instant always
//! yields the same answer. Nothing here touches gamma: applying the
//! gains to an output (mutter's gamma control, #89) and any hardware
//! brightness behaviour stay unimplemented.

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
/// Fallback temperature when no key has been read yet.
pub const DEFAULT_TEMPERATURE_K: u32 = 4500;

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
/// These are the schedule model's output only: no output driver
/// consumes them yet (#89 gamma control).
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

/// Night Light preference state: switch, temperature and schedule.
///
/// Held by [`crate::state::StateModel`] as a plain (unrevisioned)
/// display preference; a settings reader fills it from the
/// [`COLOR_SCHEMA`] keys.
#[derive(Debug, Clone, PartialEq)]
pub struct NightLightConfig {
    /// Master switch (`night-light-enabled`).
    pub enabled: bool,
    /// Requested temperature in Kelvin; always clamped (see
    /// [`NightLightConfig::set_temperature_k`]).
    temperature_k: u32,
    /// When the filter may be active.
    pub schedule: NightLightSchedule,
}

impl Default for NightLightConfig {
    /// Off, default temperature, manual schedule.
    fn default() -> Self {
        Self {
            enabled: false,
            temperature_k: DEFAULT_TEMPERATURE_K,
            schedule: NightLightSchedule::Manual,
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
    /// Disabled is always inactive. Manual follows the switch;
    /// automatic is active through the night (sunset to sunrise);
    /// fixed is active inside its window. Polar day reads inactive
    /// and polar night active, matching "dark outside".
    pub fn is_active_at(&self, unix_seconds: i64) -> bool {
        if !self.enabled {
            return false;
        }
        match &self.schedule {
            NightLightSchedule::Manual => true,
            NightLightSchedule::Automatic {
                latitude_deg,
                longitude_deg,
            } => match sun_window(*latitude_deg, *longitude_deg, unix_seconds) {
                Daylight::PolarDay => false,
                Daylight::PolarNight => true,
                Daylight::Normal(window) => window.is_night(unix_seconds),
            },
            NightLightSchedule::Fixed { from_hour, to_hour } => {
                fixed_is_active(*from_hour, *to_hour, unix_seconds)
            }
        }
    }

    /// Gains to apply at `unix_seconds`, or `None` when inactive.
    pub fn active_gains_at(&self, unix_seconds: i64) -> Option<ColorGains> {
        self.is_active_at(unix_seconds)
            .then(|| temperature_to_gains(self.temperature_k()))
    }
}

/// Whether `unix_seconds` falls in the UTC-hour window
/// [`from_hour`, `to_hour`). Wrapping windows (e.g. 22:00-06:00)
/// work; an empty window (`from == to`) is never active.
fn fixed_is_active(from_hour: f64, to_hour: f64, unix_seconds: i64) -> bool {
    let from = from_hour.rem_euclid(24.0);
    let len = (to_hour - from_hour).rem_euclid(24.0);
    if len == 0.0 {
        return false;
    }
    let now = (unix_seconds.rem_euclid(86_400) as f64) / 3600.0;
    (now - from).rem_euclid(24.0) < len
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
/// [`SunWindow::is_night`] handles that.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SunWindow {
    /// Sunrise, minutes since 00:00 UTC.
    pub sunrise_utc_min: f64,
    /// Sunset, minutes since 00:00 UTC.
    pub sunset_utc_min: f64,
}

impl SunWindow {
    /// Whether `unix_seconds` falls in the night (sunset to sunrise).
    pub fn is_night(&self, unix_seconds: i64) -> bool {
        let night_len = (self.sunrise_utc_min - self.sunset_utc_min).rem_euclid(MINUTES_PER_DAY);
        let since_set =
            (day_minute(unix_seconds) - self.sunset_utc_min).rem_euclid(MINUTES_PER_DAY);
        since_set < night_len
    }
}

/// Minutes since 00:00 UTC for a Unix timestamp.
fn day_minute(unix_seconds: i64) -> f64 {
    (unix_seconds.rem_euclid(86_400) as f64) / 60.0
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
            schedule: NightLightSchedule::Automatic {
                latitude_deg: -36.8,
                longitude_deg: 170.0,
            },
        };
        assert!(auto.is_active_at(day + 12 * 3600));
        assert!(!auto.is_active_at(day + 2 * 3600));
    }

    #[test]
    fn fixed_window_handles_wrap_and_empty() {
        let night = NightLightConfig {
            enabled: true,
            temperature_k: DEFAULT_TEMPERATURE_K,
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
        assert!(!evening.is_active_at(day + 19 * 3600));
        let empty = NightLightConfig {
            schedule: NightLightSchedule::Fixed {
                from_hour: 6.0,
                to_hour: 6.0,
            },
            ..night.clone()
        };
        assert!(!empty.is_active_at(day + 6 * 3600));
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
}
