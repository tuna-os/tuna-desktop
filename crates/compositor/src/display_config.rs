//! Mutter 51's monitor configuration model behind
//! `org.gnome.Mutter.DisplayConfig` (#340): the modes a monitor offers,
//! the scales each mode supports, the checks `ApplyMonitorsConfig` runs
//! before anything changes, and the confirmation that reverts a
//! persistent change GNOME Shell's "Keep these display settings?"
//! dialog did not keep.
//!
//! Sources (mutter 51.0): `src/backends/meta-monitor.c` (mode ids,
//! supported and preferred scales), `native/meta-output-kms.c` and
//! `native/meta-kms-utils.c` (connector modes and their refresh rate),
//! `meta-monitor-manager.c` (`ApplyMonitorsConfig`, confirmation),
//! `meta-monitor-config-manager.c` and `meta-monitor-config-utils.c`
//! (verification, history). Everything here is pure; the runtime owns
//! the hardware and the timer.

use std::collections::VecDeque;
use std::sync::{mpsc, Mutex};

use crate::monitors::{LogicalMonitorConfig, MonitorConfig, MonitorSpec, MonitorsConfig};

/// Mutter's `DEFAULT_DISPLAY_CONFIGURATION_TIMEOUT`: a persistent
/// change reverts unless confirmed within this many seconds.
pub const CONFIRMATION_TIMEOUT_SECS: u32 = 20;

const MIN_INTEGER_SCALE: u32 = 1;
const MAX_INTEGER_SCALE: u32 = 4;
const MAX_DENOMINATOR: u32 = 4;
const MINIMUM_LOGICAL_AREA: i64 = 600 * 600;
/// `meta_monitor_mode_spec_equals`' refresh tolerance.
const MAXIMUM_REFRESH_RATE_DIFF: f32 = 0.001;
const CONFIG_HISTORY_MAX_SIZE: usize = 3;
const UI_SCALE_MOBILE_TARGET_DPI: f32 = 135.0;
const UI_SCALE_LARGE_TARGET_DPI: f32 = 110.0;
const UI_SCALE_LARGE_MIN_SIZE_INCHES: f32 = 20.0;

/// `DRM_MODE_FLAG_INTERLACE`.
pub const DRM_MODE_FLAG_INTERLACE: u32 = 1 << 4;

/// One monitor mode (Mutter's `MetaMonitorModeSpec`): resolution in
/// physical pixels, refresh rate in Hz as Mutter's `float`, and the one
/// configurable flag, interlacing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModeSpec {
    pub width: i32,
    pub height: i32,
    pub refresh: f32,
    pub interlaced: bool,
}

impl ModeSpec {
    /// Mutter's `generate_mode_id`: `WxH[i]@R.RRR`.
    pub fn id(&self) -> String {
        format!(
            "{}x{}{}@{:.3}",
            self.width,
            self.height,
            if self.interlaced { "i" } else { "" },
            self.refresh
        )
    }

    /// `meta_monitor_mode_spec_equals`.
    pub fn equals(&self, other: &ModeSpec) -> bool {
        self.width == other.width
            && self.height == other.height
            && (self.refresh - other.refresh).abs() < MAXIMUM_REFRESH_RATE_DIFF
            && self.interlaced == other.interlaced
    }

    /// The refresh rate in mHz, as `wl_output` and Smithay carry it.
    pub fn refresh_mhz(&self) -> i32 {
        (f64::from(self.refresh) * 1000.0).round() as i32
    }
}

/// `meta_calculate_drm_mode_refresh_rate`: pixel clock (kHz) over the
/// total frame size, with the vertical scan multiplier.
pub fn drm_refresh_rate(clock_khz: u32, htotal: u16, vtotal: u16, vscan: u16) -> f32 {
    if htotal == 0 || vtotal == 0 {
        return 0.0;
    }
    let numerator = f64::from(clock_khz) * 1000.0;
    let mut denominator = f64::from(vtotal) * f64::from(htotal);
    if vscan > 1 {
        denominator *= f64::from(vscan);
    }
    (numerator / denominator) as f32
}

/// One mode as the connector lists it, with its raw DRM flags (the
/// deduplication below prefers modes flagged like the preferred one).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CrtcMode {
    pub spec: ModeSpec,
    pub flags: u32,
}

/// A monitor's modes in Mutter's order: positions into the connector's
/// list, and which of them is preferred.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorModes {
    pub modes: Vec<usize>,
    pub preferred: usize,
}

/// Mutter's monitor modes for a connector: sorted as `compare_modes`
/// sorts the output's modes (largest, then fastest first), the
/// kernel's preferred mode (else the first) marked, and one mode per
/// mode id kept as `meta_monitor_normal_generate_modes` keeps it.
/// `None` when the connector lists no mode.
pub fn monitor_modes(
    crtc_modes: &[CrtcMode],
    kernel_preferred: Option<usize>,
) -> Option<MonitorModes> {
    if crtc_modes.is_empty() {
        return None;
    }
    let mut sorted: Vec<usize> = (0..crtc_modes.len()).collect();
    sorted.sort_by(|&a, &b| {
        let (a, b) = (&crtc_modes[a].spec, &crtc_modes[b].spec);
        b.width
            .cmp(&a.width)
            .then(b.height.cmp(&a.height))
            .then(b.refresh.total_cmp(&a.refresh))
    });
    let preferred_raw = kernel_preferred
        .filter(|index| *index < crtc_modes.len())
        .unwrap_or(sorted[0]);
    let preferred_flags = crtc_modes[preferred_raw].flags;
    let mut list: Vec<usize> = Vec::new();
    let mut preferred: Option<usize> = None;
    for raw in sorted {
        let id = crtc_modes[raw].spec.id();
        let existing = list.iter().position(|&m| crtc_modes[m].spec.id() == id);
        let replace = crtc_modes[raw].flags == preferred_flags
            && preferred.is_none_or(|p| crtc_modes[p].spec.id() != id);
        if let Some(position) = existing {
            if !replace {
                continue;
            }
            list.remove(position);
        }
        list.push(raw);
        if raw == preferred_raw {
            preferred = Some(raw);
        }
    }
    let preferred = preferred.unwrap_or(list[0]);
    Some(MonitorModes {
        preferred: list.iter().position(|&m| m == preferred).unwrap_or(0),
        modes: list,
    })
}

/// `meta_monitor_mode_should_be_advertised`: the preferred resolution,
/// or one leaving at least 600×600 pixels.
pub fn should_be_advertised(mode: &ModeSpec, preferred: &ModeSpec) -> bool {
    (mode.width == preferred.width && mode.height == preferred.height)
        || i64::from(mode.width) * i64::from(mode.height) >= MINIMUM_LOGICAL_AREA
}

fn highest_common_factor(x: u32, y: u32) -> u32 {
    let (mut a, mut b) = (x, y);
    while a > 0 && b > 0 {
        if b > a {
            b %= a;
        } else {
            a %= b;
        }
    }
    a.max(b)
}

fn is_scale_valid_for_size(width: f32, height: f32, scale: f32) -> bool {
    if scale < MIN_INTEGER_SCALE as f32 || scale > MAX_INTEGER_SCALE as f32 {
        return false;
    }
    let w = (width / scale).floor() as i64;
    let h = (height / scale).floor() as i64;
    w * h >= MINIMUM_LOGICAL_AREA
}

/// `meta_monitor_calculate_supported_scales` in logical layout mode
/// (no constraint, Mutter 51's native default): every fraction with a
/// denominator up to 4 between 1 and 4 that divides both dimensions
/// exactly and leaves at least 600×600 logical pixels, ascending; 1
/// when none does.
pub fn supported_scales(width: i32, height: i32) -> Vec<f32> {
    let mut scales = Vec::new();
    let (Ok(w), Ok(h)) = (u32::try_from(width), u32::try_from(height)) else {
        return vec![1.0];
    };
    for denominator in 1..=MAX_DENOMINATOR {
        for numerator in MIN_INTEGER_SCALE * denominator..=MAX_INTEGER_SCALE * denominator {
            if (w * denominator) % numerator != 0 || (h * denominator) % numerator != 0 {
                continue;
            }
            if highest_common_factor(numerator, denominator) > 1 {
                continue;
            }
            let scale = numerator as f32 / denominator as f32;
            if !is_scale_valid_for_size(w as f32, h as f32, scale) {
                continue;
            }
            scales.push(scale);
        }
    }
    scales.sort_by(f32::total_cmp);
    if scales.is_empty() {
        scales.push(1.0);
    }
    scales
}

/// `meta_monitor_has_aspect_as_size`: an EDID that stores the aspect
/// ratio where the physical size belongs.
fn has_aspect_as_size(width_mm: i32, height_mm: i32) -> bool {
    matches!(
        (width_mm, height_mm),
        (1600, 900) | (1600, 1000) | (160, 90) | (160, 100) | (16, 9) | (16, 10)
    )
}

/// Mutter's `calculate_scale`: the supported scale closest to the one
/// making the UI as big as on a 135 DPI laptop (110 DPI from 20 inches
/// up); 1 without a physical size.
pub fn calculate_scale(mode: &ModeSpec, width_mm: i32, height_mm: i32) -> f32 {
    if has_aspect_as_size(width_mm, height_mm) || width_mm <= 0 || height_mm <= 0 {
        return 1.0;
    }
    let (wmm, hmm) = (width_mm as f32, height_mm as f32);
    let diag_inches = (wmm * wmm + hmm * hmm).sqrt() / 25.4;
    let target_dpi = if diag_inches < UI_SCALE_LARGE_MIN_SIZE_INCHES {
        UI_SCALE_MOBILE_TARGET_DPI
    } else {
        UI_SCALE_LARGE_TARGET_DPI
    };
    let (wpx, hpx) = (mode.width as f32, mode.height as f32);
    let physical_dpi = (wpx * wpx + hpx * hpx).sqrt() / diag_inches;
    let perfect_scale = physical_dpi / target_dpi;
    let scales = supported_scales(mode.width, mode.height);
    let mut best = scales[0];
    let mut best_error = (scales[0] - perfect_scale).abs();
    for scale in scales.into_iter().skip(1) {
        let error = (scale - perfect_scale).abs();
        if error < best_error {
            best = scale;
            best_error = error;
        }
    }
    best
}

/// One connected monitor as GetCurrentState lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct MonitorState {
    pub spec: MonitorSpec,
    pub display_name: String,
    pub builtin: bool,
    pub width_mm: i32,
    pub height_mm: i32,
    /// Mutter's monitor modes (all; GetCurrentState advertises the
    /// ones [`should_be_advertised`] keeps).
    pub modes: Vec<ModeSpec>,
    pub preferred: usize,
    /// The mode it shows; `None` while it is off.
    pub current: Option<usize>,
}

/// One logical monitor: a region of the desktop and the monitors
/// showing it (several when mirrored).
#[derive(Debug, Clone, PartialEq)]
pub struct LogicalState {
    pub x: i32,
    pub y: i32,
    pub scale: f64,
    pub transform: u32,
    pub primary: bool,
    /// Connectors; the first leads.
    pub monitors: Vec<String>,
}

/// What `GetCurrentState` reports, with Mutter's configuration serial.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DisplayState {
    pub serial: u32,
    pub monitors: Vec<MonitorState>,
    pub logical: Vec<LogicalState>,
}

impl DisplayState {
    pub fn monitor(&self, connector: &str) -> Option<&MonitorState> {
        self.monitors.iter().find(|m| m.spec.connector == connector)
    }

    /// The logical monitor showing this connector.
    pub fn logical_of(&self, connector: &str) -> Option<&LogicalState> {
        self.logical
            .iter()
            .find(|l| l.monitors.iter().any(|m| m == connector))
    }

    /// The preferred scale GetCurrentState gives a mode: the current
    /// scale for the current mode (Mutter records it as that mode's
    /// default), else [`calculate_scale`].
    pub fn preferred_scale(&self, monitor: &MonitorState, mode: usize) -> f32 {
        if monitor.current == Some(mode) {
            if let Some(logical) = self.logical_of(&monitor.spec.connector) {
                return logical.scale as f32;
            }
        }
        calculate_scale(&monitor.modes[mode], monitor.width_mm, monitor.height_mm)
    }

    /// The configuration in effect, as monitors.xml stores it.
    pub fn live_config(&self) -> MonitorsConfig {
        let spec_of = |connector: &str| {
            self.monitor(connector)
                .map(|m| m.spec.clone())
                .unwrap_or_else(|| MonitorSpec::connector(connector))
        };
        let mode_of = |connector: &str| {
            self.monitor(connector)
                .and_then(|m| m.current.map(|index| m.modes[index]))
        };
        MonitorsConfig {
            logical: self
                .logical
                .iter()
                .map(|l| LogicalMonitorConfig {
                    x: l.x,
                    y: l.y,
                    scale: l.scale,
                    transform: l.transform,
                    primary: l.primary,
                    monitors: l
                        .monitors
                        .iter()
                        .map(|c| MonitorConfig {
                            spec: spec_of(c),
                            mode: mode_of(c),
                        })
                        .collect(),
                })
                .collect(),
            disabled: self
                .monitors
                .iter()
                .filter(|m| m.current.is_none())
                .map(|m| m.spec.clone())
                .collect(),
        }
    }
}

/// The state shared with the D-Bus thread. Publishing a different
/// state bumps the serial (Mutter's `manager->serial`) and announces
/// `MonitorsChanged`.
#[derive(Default)]
pub struct DisplayShare {
    state: Mutex<DisplayState>,
    changes: Mutex<Option<mpsc::Sender<()>>>,
}

impl DisplayShare {
    pub fn snapshot(&self) -> DisplayState {
        self.state.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// Store the runtime's state; returns whether it changed.
    pub fn publish(&self, mut state: DisplayState) -> bool {
        let Ok(mut current) = self.state.lock() else {
            return false;
        };
        state.serial = current.serial;
        if *current == state {
            return false;
        }
        state.serial = current.serial.wrapping_add(1).max(1);
        *current = state;
        drop(current);
        if let Ok(changes) = self.changes.lock() {
            if let Some(sender) = changes.as_ref() {
                let _ = sender.send(());
            }
        }
        true
    }

    /// One message per published change. A new subscriber replaces the old.
    pub fn subscribe_changes(&self) -> mpsc::Receiver<()> {
        let (sender, receiver) = mpsc::channel();
        if let Ok(mut changes) = self.changes.lock() {
            *changes = Some(sender);
        }
        receiver
    }
}

/// `ApplyMonitorsConfig`'s `method`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Verify,
    Temporary,
    Persistent,
}

impl Method {
    pub fn from_dbus(method: u32) -> Result<Self, ApplyError> {
        match method {
            0 => Ok(Self::Verify),
            1 => Ok(Self::Temporary),
            2 => Ok(Self::Persistent),
            other => Err(ApplyError::InvalidArgs(format!(
                "Invalid configuration method {other}"
            ))),
        }
    }
}

/// Why a configuration was refused, with Mutter's D-Bus error class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyError {
    AccessDenied(String),
    InvalidArgs(String),
}

/// One monitor of a requested logical monitor.
#[derive(Debug, Clone, PartialEq)]
pub struct MonitorRequest {
    pub connector: String,
    pub mode_id: String,
    pub underscanning: Option<bool>,
    pub color_mode: Option<u32>,
}

/// One requested logical monitor (`(iiduba(ssa{sv}))`).
#[derive(Debug, Clone, PartialEq)]
pub struct LogicalRequest {
    pub x: i32,
    pub y: i32,
    pub scale: f64,
    pub transform: u32,
    pub primary: bool,
    pub monitors: Vec<MonitorRequest>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rect {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

impl Rect {
    /// `mtk_rectangle_overlap`.
    fn overlaps(&self, other: &Rect) -> bool {
        !(self.x + self.w <= other.x
            || other.x + other.w <= self.x
            || self.y + self.h <= other.y
            || other.y + other.h <= self.y)
    }

    /// `mtk_rectangle_is_adjacent_to`.
    fn is_adjacent_to(&self, other: &Rect) -> bool {
        let (x1, y1, x2, y2) = (self.x, self.y, self.x + self.w, self.y + self.h);
        let (ox1, oy1, ox2, oy2) = (other.x, other.y, other.x + other.w, other.y + other.h);
        ((x1 == ox2 || x2 == ox1) && !(y2 <= oy1 || y1 >= oy2))
            || ((y1 == oy2 || y2 == oy1) && !(x2 <= ox1 || x1 >= ox2))
    }
}

/// `is_connected_to_all`: every logical monitor reachable from `start`
/// through shared edges.
fn is_connected_to_all(start: usize, rects: &[Rect]) -> bool {
    let mut seen = vec![false; rects.len()];
    let mut stack = vec![start];
    seen[start] = true;
    while let Some(at) = stack.pop() {
        for (index, rect) in rects.iter().enumerate() {
            if !seen[index] && index != at && rects[at].is_adjacent_to(rect) {
                seen[index] = true;
                stack.push(index);
            }
        }
    }
    seen.into_iter().all(|s| s)
}

fn is_rotated(transform: u32) -> bool {
    transform % 2 == 1
}

fn invalid(message: impl Into<String>) -> ApplyError {
    ApplyError::InvalidArgs(message.into())
}

/// Mutter's `ApplyMonitorsConfig` checks, in its order: the serial,
/// the layout mode, each logical monitor
/// (`create_logical_monitor_config_from_variant` and
/// `meta_verify_logical_monitor_config`), then the whole arrangement
/// (`meta_verify_logical_monitor_config_list`). Monitors left out are
/// disabled; at least one must stay on. Returns the configuration to
/// apply, every mode resolved.
pub fn verify(
    state: &DisplayState,
    serial: u32,
    layout_mode: Option<u32>,
    requested: &[LogicalRequest],
) -> Result<MonitorsConfig, ApplyError> {
    if serial != state.serial {
        return Err(ApplyError::AccessDenied(
            "The requested configuration is based on stale information".into(),
        ));
    }
    // Only Mutter's logical layout (its native default) is supported;
    // physical layout mode is not advertised.
    if !matches!(layout_mode, None | Some(1)) {
        return Err(invalid("Can't set layout mode"));
    }
    let mut logical = Vec::new();
    let mut rects = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    for request in requested {
        let mut monitors = Vec::new();
        for monitor in &request.monitors {
            let Some(state_monitor) = state.monitor(&monitor.connector) else {
                return Err(invalid(format!(
                    "Invalid connector '{}' specified",
                    monitor.connector
                )));
            };
            let Some(mode) = state_monitor
                .modes
                .iter()
                .find(|m| m.id() == monitor.mode_id)
                .copied()
            else {
                return Err(invalid(format!(
                    "Invalid mode '{}' specified",
                    monitor.mode_id
                )));
            };
            if monitor.underscanning == Some(true) {
                return Err(invalid("Underscanning requested but unsupported"));
            }
            // GNOME Settings always sends `color-mode`; only the
            // default (SDR) mode is supported (HDR is P-DS-03).
            if let Some(color_mode) = monitor.color_mode.filter(|c| *c != 0) {
                return Err(invalid(format!("Color mode {color_mode} not supported")));
            }
            if seen.contains(&monitor.connector.as_str()) {
                return Err(invalid(format!(
                    "connector '{}' configured more than once",
                    monitor.connector
                )));
            }
            seen.push(&monitor.connector);
            monitors.push(MonitorConfig {
                spec: state_monitor.spec.clone(),
                mode: Some(mode),
            });
        }
        let Some(first) = monitors.first() else {
            return Err(invalid("Empty logical monitor"));
        };
        if !crate::orientation::transform_is_supported(request.transform) {
            return Err(invalid(format!(
                "transform {} is not a Mutter transform (0-7)",
                request.transform
            )));
        }
        let mode = first.mode.expect("resolved above");
        // find_monitor_mode_scale: the requested scale must be one the
        // first monitor's mode supports.
        let wanted = request.scale as f32;
        let Some(scale) = supported_scales(mode.width, mode.height)
            .into_iter()
            .find(|s| (s - wanted).abs() < f32::EPSILON)
        else {
            return Err(invalid(format!(
                "Scale {} not valid for resolution {}x{}",
                wanted, mode.width, mode.height
            )));
        };
        // derive_logical_monitor_size.
        let (rw, rh) = if is_rotated(request.transform) {
            (mode.height, mode.width)
        } else {
            (mode.width, mode.height)
        };
        let width = (rw as f32 / scale).round() as i32;
        let height = (rh as f32 / scale).round() as i32;
        // meta_verify_logical_monitor_config.
        if request.x < 0 || request.y < 0 {
            return Err(invalid(format!(
                "Invalid logical monitor position ({}, {})",
                request.x, request.y
            )));
        }
        if monitors.iter().any(|m| {
            let other = m.mode.expect("resolved above");
            other.width != mode.width || other.height != mode.height
        }) {
            return Err(invalid("Monitors modes in logical monitor not equal"));
        }
        let (layout_w, layout_h) = if is_rotated(request.transform) {
            (height, width)
        } else {
            (width, height)
        };
        let scaled_w = mode.width as f32 / scale;
        let scaled_h = mode.height as f32 / scale;
        if scaled_w.floor() != scaled_w || scaled_h.floor() != scaled_h {
            return Err(invalid("Scaled logical monitor size is fractional"));
        }
        if mode.width != (layout_w as f32 * scale).round() as i32
            || mode.height != (layout_h as f32 * scale).round() as i32
        {
            return Err(invalid(
                "Monitor mode size doesn't match scaled monitor layout",
            ));
        }
        rects.push(Rect {
            x: request.x,
            y: request.y,
            w: width,
            h: height,
        });
        logical.push(LogicalMonitorConfig {
            x: request.x,
            y: request.y,
            scale: f64::from(scale),
            transform: request.transform,
            primary: request.primary,
            monitors,
        });
    }
    verify_list(&logical, &rects)?;
    let disabled = state
        .monitors
        .iter()
        .filter(|m| !seen.contains(&m.spec.connector.as_str()))
        .map(|m| m.spec.clone())
        .collect();
    Ok(MonitorsConfig { logical, disabled })
}

/// `meta_verify_logical_monitor_config_list`.
fn verify_list(logical: &[LogicalMonitorConfig], rects: &[Rect]) -> Result<(), ApplyError> {
    if logical.is_empty() {
        return Err(invalid("Monitors config incomplete"));
    }
    let mut has_primary = false;
    let (mut min_x, mut min_y) = (i32::MAX, i32::MAX);
    for (index, (config, rect)) in logical.iter().zip(rects).enumerate() {
        if rects[..index].iter().any(|r| r.overlaps(rect)) {
            return Err(invalid("Logical monitors overlap"));
        }
        if has_primary && config.primary {
            return Err(invalid("Config contains multiple primary logical monitors"));
        }
        has_primary |= config.primary;
        if !is_connected_to_all(index, rects) {
            return Err(invalid("Logical monitors not adjacent"));
        }
        min_x = min_x.min(rect.x);
        min_y = min_y.min(rect.y);
    }
    if min_x != 0 || min_y != 0 {
        return Err(invalid("Logical monitors positions are offset"));
    }
    if !has_primary {
        return Err(invalid("Config is missing primary logical"));
    }
    Ok(())
}

/// What one connector shows after a configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct OutputLayout {
    pub connector: String,
    /// `None`: the output is off.
    pub enabled: Option<EnabledLayout>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EnabledLayout {
    /// `None` keeps the current mode (an arrangement without modes).
    pub mode: Option<ModeSpec>,
    pub loc: (i32, i32),
    pub scale: f64,
    pub transform: u32,
    pub primary: bool,
    /// The connector leading the mirrored logical monitor this one
    /// clones; `None` for a leader.
    pub mirror_of: Option<String>,
}

/// Per-connector layout of a configuration: the first monitor of each
/// logical monitor leads it, the rest mirror it; disabled monitors
/// are off. Connectors the configuration does not mention are absent.
pub fn output_layouts(config: &MonitorsConfig) -> Vec<OutputLayout> {
    let mut layouts = Vec::new();
    for logical in &config.logical {
        let leader = logical.monitors.first().map(|m| m.spec.connector.clone());
        for (index, monitor) in logical.monitors.iter().enumerate() {
            layouts.push(OutputLayout {
                connector: monitor.spec.connector.clone(),
                enabled: Some(EnabledLayout {
                    mode: monitor.mode,
                    loc: (logical.x, logical.y),
                    scale: logical.scale,
                    transform: logical.transform,
                    primary: logical.primary,
                    mirror_of: if index == 0 { None } else { leader.clone() },
                }),
            });
        }
    }
    for spec in &config.disabled {
        if !layouts.iter().any(|l| l.connector == spec.connector) {
            layouts.push(OutputLayout {
                connector: spec.connector.clone(),
                enabled: None,
            });
        }
    }
    layouts
}

/// A connector a hardware session found lit, with its modes.
#[derive(Debug, Clone, PartialEq)]
pub struct StartupConnector {
    pub name: String,
    pub modes: Vec<ModeSpec>,
    pub preferred: usize,
}

/// The layout a hardware session starts with (#59, #340): the stored
/// configuration for these connectors where it fits (modes, mirrors,
/// disabled monitors), every other connector at its preferred mode and
/// `fallback_scale`, left to right after the rest. Lenient where
/// Mutter would drop the whole stored configuration, as Tuna Desktop
/// always was: a stored mode the connector lacks becomes the preferred
/// one, a clone whose mode size differs from its leader's stands
/// alone, and a configuration switching everything off is ignored.
/// Exactly one leader is primary. In connector order.
pub fn startup_layouts(
    stored: Option<&MonitorsConfig>,
    connectors: &[StartupConnector],
    fallback_scale: f64,
) -> Vec<OutputLayout> {
    let connector = |name: &str| connectors.iter().find(|c| c.name == name);
    let mut layouts: Vec<OutputLayout> = stored
        .map(output_layouts)
        .unwrap_or_default()
        .into_iter()
        .filter(|l| connector(&l.connector).is_some())
        .collect();
    for layout in &mut layouts {
        let Some(c) = connector(&layout.connector) else {
            continue;
        };
        if let Some(enabled) = &mut layout.enabled {
            enabled.mode = enabled
                .mode
                .filter(|m| c.modes.iter().any(|x| x.equals(m)))
                .or(Some(c.modes[c.preferred]));
        }
    }
    if !layouts.iter().any(|l| l.enabled.is_some()) {
        layouts.clear();
    }
    let size = |layouts: &[OutputLayout], name: &str| {
        layouts
            .iter()
            .find(|l| l.connector == name)
            .and_then(|l| l.enabled.as_ref())
            .filter(|e| e.mirror_of.is_none())
            .and_then(|e| e.mode)
            .map(|m| (m.width, m.height))
    };
    let mut unplaced: Vec<String> = Vec::new();
    let snapshot = layouts.clone();
    for layout in &mut layouts {
        let Some(enabled) = &mut layout.enabled else {
            continue;
        };
        let Some(leader) = enabled.mirror_of.clone() else {
            continue;
        };
        let own = enabled.mode.map(|m| (m.width, m.height));
        if size(&snapshot, &leader).is_none() || size(&snapshot, &leader) != own {
            enabled.mirror_of = None;
            enabled.primary = false;
            unplaced.push(layout.connector.clone());
        }
    }
    let width = |e: &EnabledLayout| {
        e.mode
            .map(|m| crate::orientation::logical_extent(m.width, m.height, e.scale, e.transform).0)
            .unwrap_or(0)
    };
    let mut next_x = layouts
        .iter()
        .filter(|l| !unplaced.contains(&l.connector))
        .filter_map(|l| l.enabled.as_ref())
        .filter(|e| e.mirror_of.is_none())
        .map(|e| e.loc.0 + width(e))
        .max()
        .unwrap_or(0);
    for layout in &mut layouts {
        if !unplaced.contains(&layout.connector) {
            continue;
        }
        if let Some(enabled) = &mut layout.enabled {
            enabled.loc = (next_x, 0);
            next_x += width(enabled);
        }
    }
    for c in connectors {
        if layouts.iter().any(|l| l.connector == c.name) {
            continue;
        }
        let enabled = EnabledLayout {
            mode: Some(c.modes[c.preferred]),
            loc: (next_x, 0),
            scale: fallback_scale,
            transform: 0,
            primary: false,
            mirror_of: None,
        };
        next_x += width(&enabled);
        layouts.push(OutputLayout {
            connector: c.name.clone(),
            enabled: Some(enabled),
        });
    }
    let is_leader = |l: &OutputLayout| l.enabled.as_ref().is_some_and(|e| e.mirror_of.is_none());
    let primary = layouts
        .iter()
        .position(|l| is_leader(l) && l.enabled.as_ref().is_some_and(|e| e.primary))
        .or_else(|| layouts.iter().position(is_leader));
    for (index, layout) in layouts.iter_mut().enumerate() {
        if let Some(enabled) = &mut layout.enabled {
            enabled.primary = Some(index) == primary;
        }
    }
    layouts.sort_by_key(|l| connectors.iter().position(|c| c.name == l.connector));
    layouts
}

/// Mutter's current configuration and its history
/// (`MetaMonitorConfigManager`), plus the pending confirmation of a
/// persistent change (`persistent_timeout_id`).
#[derive(Debug, Default)]
pub struct ConfigManager {
    current: Option<MonitorsConfig>,
    history: VecDeque<MonitorsConfig>,
    pending: Option<u64>,
    generation: u64,
}

/// What GNOME Shell's answer to the confirmation dialog does.
#[derive(Debug, Clone, PartialEq)]
pub enum Confirmation {
    /// Nothing awaited an answer.
    Ignored,
    /// "Keep Changes": save the current configuration.
    Save(MonitorsConfig),
    /// "Revert Settings": restore the previous configuration.
    Restore,
}

impl ConfigManager {
    pub fn current(&self) -> Option<&MonitorsConfig> {
        self.current.as_ref()
    }

    /// The configuration found in effect before anything was applied.
    pub fn ensure_current(&mut self, live: MonitorsConfig) {
        if self.current.is_none() {
            self.current = Some(live);
        }
    }

    /// Whether a persistent change awaits its confirmation.
    pub fn awaiting_confirmation(&self) -> bool {
        self.pending.is_some()
    }

    /// A temporary or persistent apply begins: Mutter drops the
    /// pending confirmation timeout first.
    pub fn cancel_confirmation(&mut self) {
        self.pending = None;
    }

    /// `meta_monitor_config_manager_set_current`: the previous
    /// configuration joins the history (at most three).
    fn set_current(&mut self, config: MonitorsConfig) {
        if let Some(previous) = self.current.take() {
            self.history.push_front(previous);
            self.history.truncate(CONFIG_HISTORY_MAX_SIZE);
        }
        self.current = Some(config);
    }

    /// The configuration was applied. A persistent one now awaits
    /// confirmation; the returned generation identifies its timeout.
    pub fn applied(&mut self, config: MonitorsConfig, method: Method) -> Option<u64> {
        match method {
            Method::Verify => None,
            Method::Temporary => {
                self.set_current(config);
                None
            }
            Method::Persistent => {
                self.set_current(config);
                self.generation += 1;
                self.pending = Some(self.generation);
                self.pending
            }
        }
    }

    /// `meta_monitor_manager_confirm_configuration`.
    pub fn confirm(&mut self, keep: bool) -> Confirmation {
        if self.pending.take().is_none() {
            return Confirmation::Ignored;
        }
        match (keep, self.current.clone()) {
            (true, Some(current)) => Confirmation::Save(current),
            (true, None) => Confirmation::Ignored,
            (false, _) => Confirmation::Restore,
        }
    }

    /// The confirmation timeout fired (`save_config_timeout`); true
    /// when it still counts and the previous configuration returns.
    pub fn timed_out(&mut self, generation: u64) -> bool {
        if self.pending == Some(generation) {
            self.pending = None;
            true
        } else {
            false
        }
    }

    /// `meta_monitor_config_manager_pop_previous`.
    pub fn pop_previous(&mut self) -> Option<MonitorsConfig> {
        self.history.pop_front()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(width: i32, height: i32, refresh: f32) -> ModeSpec {
        ModeSpec {
            width,
            height,
            refresh,
            interlaced: false,
        }
    }

    fn monitor(connector: &str, modes: Vec<ModeSpec>, current: Option<usize>) -> MonitorState {
        MonitorState {
            spec: MonitorSpec {
                connector: connector.into(),
                vendor: "Tuna Desktop".into(),
                product: connector.into(),
                serial: connector.into(),
            },
            display_name: connector.into(),
            builtin: false,
            width_mm: 0,
            height_mm: 0,
            modes,
            preferred: 0,
            current,
        }
    }

    /// A laptop panel and an external monitor side by side.
    fn two_monitors() -> DisplayState {
        DisplayState {
            serial: 7,
            monitors: vec![
                monitor(
                    "eDP-1",
                    vec![
                        mode(1920, 1080, 60.0),
                        mode(1920, 1080, 48.0),
                        mode(1280, 720, 60.0),
                    ],
                    Some(0),
                ),
                monitor(
                    "HDMI-A-1",
                    vec![mode(1920, 1080, 60.0), mode(1280, 1024, 75.025)],
                    Some(0),
                ),
            ],
            logical: vec![
                LogicalState {
                    x: 0,
                    y: 0,
                    scale: 1.0,
                    transform: 0,
                    primary: true,
                    monitors: vec!["eDP-1".into()],
                },
                LogicalState {
                    x: 1920,
                    y: 0,
                    scale: 1.0,
                    transform: 0,
                    primary: false,
                    monitors: vec!["HDMI-A-1".into()],
                },
            ],
        }
    }

    fn request(
        x: i32,
        y: i32,
        scale: f64,
        primary: bool,
        monitors: &[(&str, &str)],
    ) -> LogicalRequest {
        LogicalRequest {
            x,
            y,
            scale,
            transform: 0,
            primary,
            monitors: monitors
                .iter()
                .map(|(connector, mode_id)| MonitorRequest {
                    connector: (*connector).into(),
                    mode_id: (*mode_id).into(),
                    underscanning: Some(false),
                    color_mode: Some(0),
                })
                .collect(),
        }
    }

    fn error(result: Result<MonitorsConfig, ApplyError>) -> String {
        match result {
            Err(ApplyError::InvalidArgs(message)) | Err(ApplyError::AccessDenied(message)) => {
                message
            }
            Ok(config) => panic!("accepted {config:?}"),
        }
    }

    #[test]
    fn mode_ids_and_refresh_follow_mutter() {
        // 1920x1080 CEA timing: 148.5 MHz over 2200x1125.
        let refresh = drm_refresh_rate(148_500, 2200, 1125, 0);
        assert_eq!(refresh, 60.0);
        assert_eq!(mode(1920, 1080, refresh).id(), "1920x1080@60.000");
        // NTSC-rate 1080p: 148.352 MHz.
        let ntsc = drm_refresh_rate(148_352, 2200, 1125, 0);
        assert_eq!(mode(1920, 1080, ntsc).id(), "1920x1080@59.940");
        let interlaced = ModeSpec {
            interlaced: true,
            ..mode(1920, 1080, 60.0)
        };
        assert_eq!(interlaced.id(), "1920x1080i@60.000");
        // vscan multiplies the frame; zero totals give no rate.
        assert_eq!(drm_refresh_rate(148_500, 2200, 1125, 2), 30.0);
        assert_eq!(drm_refresh_rate(148_500, 0, 1125, 0), 0.0);
        assert!(mode(1920, 1080, 60.0004).equals(&mode(1920, 1080, 60.0)));
        assert!(!mode(1920, 1080, 59.94).equals(&mode(1920, 1080, 60.0)));
        assert!(!interlaced.equals(&mode(1920, 1080, 60.0)));
        assert_eq!(mode(1920, 1080, ntsc).refresh_mhz(), 59_940);
    }

    #[test]
    fn connector_modes_sort_and_deduplicate_like_mutter() {
        let crtc = |w, h, r, flags| CrtcMode {
            spec: mode(w, h, r),
            flags,
        };
        // Kernel order is arbitrary; the preferred mode is listed third
        // and a same-id twin with other sync flags precedes it.
        let modes = [
            crtc(1280, 720, 60.0, 0b0101),
            crtc(1920, 1080, 60.0, 0b1010),
            crtc(1920, 1080, 60.0, 0b0101),
            crtc(1920, 1080, 75.0, 0b0101),
            crtc(640, 480, 59.94, 0b0101),
        ];
        let result = monitor_modes(&modes, Some(2)).unwrap();
        let ids: Vec<String> = result.modes.iter().map(|&i| modes[i].spec.id()).collect();
        assert_eq!(
            ids,
            [
                "1920x1080@75.000",
                "1920x1080@60.000",
                "1280x720@60.000",
                "640x480@59.940"
            ]
        );
        // The kernel's preferred mode replaced its twin and stays preferred.
        assert_eq!(result.modes[result.preferred], 2);
        // Without a kernel preference the largest, fastest mode is.
        let result = monitor_modes(&modes, None).unwrap();
        assert_eq!(
            modes[result.modes[result.preferred]].spec.id(),
            "1920x1080@75.000"
        );
        assert!(monitor_modes(&[], None).is_none());
        // Small modes stay hidden unless they are the preferred size.
        assert!(!should_be_advertised(
            &mode(640, 480, 60.0),
            &mode(1920, 1080, 60.0)
        ));
        assert!(should_be_advertised(
            &mode(640, 480, 60.0),
            &mode(640, 480, 75.0)
        ));
        assert!(should_be_advertised(
            &mode(800, 600, 60.0),
            &mode(1920, 1080, 60.0)
        ));
    }

    #[test]
    fn supported_scales_match_mutter_fractions() {
        // 1.75 does not divide 1920 evenly; 2 still leaves 960x540.
        assert_eq!(
            supported_scales(1920, 1080),
            [1.0, 1.25, 4.0 / 3.0, 1.5, 5.0 / 3.0, 2.0]
        );
        // 1280x800 (the nested proof window) includes 125 percent.
        assert_eq!(
            supported_scales(1280, 800),
            [1.0, 1.25, 4.0 / 3.0, 5.0 / 3.0]
        );
        // vkms' 1024x768 default: 4/3 leaves 768x576.
        assert_eq!(supported_scales(1024, 768), [1.0, 4.0 / 3.0]);
        let uhd = supported_scales(3840, 2160);
        assert_eq!((uhd[0], *uhd.last().unwrap(), uhd.len()), (1.0, 4.0, 12));
        // Too small for any scale but 1 (the fallback).
        assert_eq!(supported_scales(800, 600), [1.0]);
        assert_eq!(supported_scales(640, 480), [1.0]);
    }

    #[test]
    fn preferred_scale_follows_dpi() {
        // A 13.3" 2560x1600 panel: ~227 DPI → about 1.68 → 5/3.
        assert_eq!(
            calculate_scale(&mode(2560, 1600, 60.0), 286, 179),
            5.0 / 3.0
        );
        // A 27" 1440p desktop monitor: ~109 DPI → 1.
        assert_eq!(calculate_scale(&mode(2560, 1440, 60.0), 597, 336), 1.0);
        // No size or an aspect stored as size: 1.
        assert_eq!(calculate_scale(&mode(3840, 2160, 60.0), 0, 0), 1.0);
        assert_eq!(calculate_scale(&mode(3840, 2160, 60.0), 160, 90), 1.0);
    }

    #[test]
    fn verify_accepts_modes_mirroring_and_disabling() {
        let state = two_monitors();
        // A different resolution and refresh rate on the panel.
        let config = verify(
            &state,
            7,
            None,
            &[
                request(0, 0, 1.0, true, &[("eDP-1", "1280x720@60.000")]),
                request(1280, 0, 1.0, false, &[("HDMI-A-1", "1920x1080@60.000")]),
            ],
        )
        .unwrap();
        assert_eq!(
            config.logical[0].monitors[0].mode,
            Some(mode(1280, 720, 60.0))
        );
        assert!(config.disabled.is_empty());
        let refresh = verify(
            &state,
            7,
            None,
            &[
                request(0, 0, 1.0, true, &[("eDP-1", "1920x1080@48.000")]),
                request(1920, 0, 1.0, false, &[("HDMI-A-1", "1920x1080@60.000")]),
            ],
        )
        .unwrap();
        assert_eq!(refresh.logical[0].monitors[0].mode.unwrap().refresh, 48.0);
        // Mirroring: one logical monitor, a shared resolution.
        let mirrored = verify(
            &state,
            7,
            Some(1),
            &[request(
                0,
                0,
                1.25,
                true,
                &[
                    ("eDP-1", "1920x1080@60.000"),
                    ("HDMI-A-1", "1920x1080@60.000"),
                ],
            )],
        )
        .unwrap();
        assert_eq!(mirrored.logical.len(), 1);
        assert_eq!(mirrored.logical[0].monitors.len(), 2);
        assert_eq!(mirrored.logical[0].scale, 1.25);
        let layouts = output_layouts(&mirrored);
        assert_eq!(
            layouts[1].enabled.as_ref().unwrap().mirror_of.as_deref(),
            Some("eDP-1")
        );
        // Disabling: the omitted monitor is off.
        let single = verify(
            &state,
            7,
            None,
            &[request(
                0,
                0,
                1.0,
                true,
                &[("HDMI-A-1", "1280x1024@75.025")],
            )],
        )
        .unwrap();
        assert_eq!(single.disabled.len(), 1);
        assert_eq!(single.disabled[0].connector, "eDP-1");
        assert_eq!(
            output_layouts(&single)
                .iter()
                .map(|l| (l.connector.as_str(), l.enabled.is_some()))
                .collect::<Vec<_>>(),
            [("HDMI-A-1", true), ("eDP-1", false)]
        );
        // Rotation swaps the logical size the neighbour must touch.
        let mut rotated = request(0, 0, 1.0, true, &[("eDP-1", "1920x1080@60.000")]);
        rotated.transform = 1;
        assert!(verify(
            &state,
            7,
            None,
            &[
                rotated,
                request(1080, 0, 1.0, false, &[("HDMI-A-1", "1920x1080@60.000")])
            ]
        )
        .is_ok());
    }

    #[test]
    fn verify_rejects_what_mutter_rejects() {
        let state = two_monitors();
        let side = |x| request(x, 0, 1.0, false, &[("HDMI-A-1", "1920x1080@60.000")]);
        let panel = |id: &str| request(0, 0, 1.0, true, &[("eDP-1", id)]);
        let ok = || vec![panel("1920x1080@60.000"), side(1920)];
        assert!(verify(&state, 7, None, &ok()).is_ok());
        // Stale serial: AccessDenied.
        assert_eq!(
            verify(&state, 6, None, &ok()),
            Err(ApplyError::AccessDenied(
                "The requested configuration is based on stale information".into()
            ))
        );
        assert_eq!(
            error(verify(&state, 7, Some(2), &ok())),
            "Can't set layout mode"
        );
        // Disabling everything.
        assert_eq!(
            error(verify(&state, 7, None, &[])),
            "Monitors config incomplete"
        );
        assert_eq!(
            error(verify(
                &state,
                7,
                None,
                &[
                    panel("1920x1080@60.000"),
                    request(1920, 0, 1.0, false, &[("DP-9", "1920x1080@60.000")])
                ]
            )),
            "Invalid connector 'DP-9' specified"
        );
        for id in [
            "",
            "1920x1080@59.940",
            "1920x1080i@60.000",
            "1024x768@60.000",
        ] {
            assert_eq!(
                error(verify(&state, 7, None, &[panel(id), side(1920)])),
                format!("Invalid mode '{id}' specified")
            );
        }
        // Mirrors need one shared resolution.
        assert_eq!(
            error(verify(
                &state,
                7,
                None,
                &[request(
                    0,
                    0,
                    1.0,
                    true,
                    &[
                        ("eDP-1", "1920x1080@60.000"),
                        ("HDMI-A-1", "1280x1024@75.025")
                    ]
                )]
            )),
            "Monitors modes in logical monitor not equal"
        );
        // Scales the mode does not support.
        let mut half = ok();
        half[0].scale = 0.5;
        assert_eq!(
            error(verify(&state, 7, None, &half)),
            "Scale 0.5 not valid for resolution 1920x1080"
        );
        let mut odd = ok();
        odd[0].scale = 1.2;
        assert!(error(verify(&state, 7, None, &odd)).starts_with("Scale 1.2 not valid"));
        assert_eq!(
            error(verify(
                &state,
                7,
                None,
                &[panel("1920x1080@60.000"), side(1000)]
            )),
            "Logical monitors overlap"
        );
        assert_eq!(
            error(verify(
                &state,
                7,
                None,
                &[panel("1920x1080@60.000"), side(2000)]
            )),
            "Logical monitors not adjacent"
        );
        let mut both = ok();
        both[1].primary = true;
        assert_eq!(
            error(verify(&state, 7, None, &both)),
            "Config contains multiple primary logical monitors"
        );
        let mut neither = ok();
        neither[0].primary = false;
        assert_eq!(
            error(verify(&state, 7, None, &neither)),
            "Config is missing primary logical"
        );
        assert_eq!(
            error(verify(
                &state,
                7,
                None,
                &[
                    request(-1920, 0, 1.0, true, &[("eDP-1", "1920x1080@60.000")]),
                    side(0)
                ]
            )),
            "Invalid logical monitor position (-1920, 0)"
        );
        assert_eq!(
            error(verify(
                &state,
                7,
                None,
                &[
                    request(0, 10, 1.0, true, &[("eDP-1", "1920x1080@60.000")]),
                    request(1920, 10, 1.0, false, &[("HDMI-A-1", "1920x1080@60.000")])
                ]
            )),
            "Logical monitors positions are offset"
        );
        let mut duplicate = ok();
        duplicate[1].monitors[0].connector = "eDP-1".into();
        duplicate[1].monitors[0].mode_id = "1920x1080@60.000".into();
        assert_eq!(
            error(verify(&state, 7, None, &duplicate)),
            "connector 'eDP-1' configured more than once"
        );
        let mut underscan = ok();
        underscan[0].monitors[0].underscanning = Some(true);
        assert_eq!(
            error(verify(&state, 7, None, &underscan)),
            "Underscanning requested but unsupported"
        );
        let mut hdr = ok();
        hdr[0].monitors[0].color_mode = Some(1);
        assert_eq!(
            error(verify(&state, 7, None, &hdr)),
            "Color mode 1 not supported"
        );
        let mut empty = ok();
        empty[1].monitors.clear();
        assert_eq!(
            error(verify(&state, 7, None, &empty)),
            "Empty logical monitor"
        );
        for transform in 0..=7u32 {
            let mut rotated = vec![panel("1920x1080@60.000")];
            rotated[0].transform = transform;
            let config = verify(&state, 7, None, &rotated).unwrap();
            assert_eq!(config.logical[0].transform, transform);
        }
        let mut bad = vec![panel("1920x1080@60.000")];
        bad[0].transform = 8;
        assert_eq!(
            error(verify(&state, 7, None, &bad)),
            "transform 8 is not a Mutter transform (0-7)"
        );
        assert!(Method::from_dbus(3).is_err());
        assert_eq!(Method::from_dbus(0), Ok(Method::Verify));
    }

    fn config(scale: f64) -> MonitorsConfig {
        let state = two_monitors();
        let mut config = state.live_config();
        config.logical[0].scale = scale;
        config
    }

    #[test]
    fn persistent_changes_await_confirmation_then_save_or_revert() {
        let mut manager = ConfigManager::default();
        manager.ensure_current(config(1.0));
        // Keep Changes saves what was applied.
        let generation = manager.applied(config(1.25), Method::Persistent).unwrap();
        assert!(manager.awaiting_confirmation());
        assert_eq!(manager.confirm(true), Confirmation::Save(config(1.25)));
        assert!(!manager.awaiting_confirmation());
        // The timer of an answered change no longer reverts.
        assert!(!manager.timed_out(generation));
        assert_eq!(manager.confirm(true), Confirmation::Ignored);
        // Revert Settings restores the configuration before it.
        manager.applied(config(1.5), Method::Persistent).unwrap();
        assert_eq!(manager.confirm(false), Confirmation::Restore);
        assert_eq!(manager.pop_previous(), Some(config(1.25)));
        // The 20 s timeout reverts an unanswered change.
        manager.applied(config(1.25), Method::Temporary);
        let generation = manager.applied(config(1.75), Method::Persistent).unwrap();
        assert!(manager.timed_out(generation));
        assert_eq!(manager.pop_previous(), Some(config(1.25)));
        assert_eq!(manager.confirm(false), Confirmation::Ignored);
    }

    #[test]
    fn a_newer_apply_supersedes_the_pending_confirmation() {
        let mut manager = ConfigManager::default();
        manager.ensure_current(config(1.0));
        let first = manager.applied(config(1.25), Method::Persistent).unwrap();
        // Mutter clears the timeout before applying anything else.
        manager.cancel_confirmation();
        manager.applied(config(1.5), Method::Temporary);
        assert!(!manager.timed_out(first));
        assert_eq!(manager.confirm(true), Confirmation::Ignored);
        // A second persistent change gets its own timeout.
        let second = manager.applied(config(1.75), Method::Persistent).unwrap();
        assert_ne!(first, second);
        assert!(!manager.timed_out(first));
        assert!(manager.timed_out(second));
        // Verify never touches the configuration or history.
        assert_eq!(manager.applied(config(2.0), Method::Verify), None);
        assert_eq!(manager.current(), Some(&config(1.75)));
        // History holds three configurations, newest first.
        assert_eq!(manager.pop_previous(), Some(config(1.5)));
        assert_eq!(manager.pop_previous(), Some(config(1.25)));
        assert_eq!(manager.pop_previous(), Some(config(1.0)));
        assert_eq!(manager.pop_previous(), None);
    }

    #[test]
    fn publishing_bumps_the_serial_only_on_change() {
        let share = DisplayShare::default();
        let changes = share.subscribe_changes();
        assert_eq!(share.snapshot().serial, 0);
        assert!(share.publish(two_monitors()));
        assert_eq!(share.snapshot().serial, 1);
        assert!(changes.try_recv().is_ok());
        // The incoming serial is ignored; equal content is no change.
        assert!(!share.publish(two_monitors()));
        assert!(changes.try_recv().is_err());
        let mut moved = two_monitors();
        moved.logical[1].x = 1280;
        assert!(share.publish(moved.clone()));
        assert_eq!(share.snapshot().serial, 2);
        assert_eq!(share.snapshot().logical, moved.logical);
    }

    #[test]
    fn live_config_lists_modes_and_disabled_monitors() {
        let mut state = two_monitors();
        state.monitors[1].current = None;
        state.logical.truncate(1);
        let live = state.live_config();
        assert_eq!(live.logical.len(), 1);
        assert_eq!(
            live.logical[0].monitors[0].mode,
            Some(mode(1920, 1080, 60.0))
        );
        assert_eq!(live.disabled[0].connector, "HDMI-A-1");
        // The current mode's preferred scale is the scale in use.
        state.logical[0].scale = 1.5;
        assert_eq!(state.preferred_scale(&state.monitors[0], 0), 1.5);
        assert_eq!(state.preferred_scale(&state.monitors[0], 2), 1.0);
    }

    fn startup_connectors() -> Vec<StartupConnector> {
        let connector = |name: &str, modes: Vec<ModeSpec>| StartupConnector {
            name: name.into(),
            modes,
            preferred: 0,
        };
        vec![
            connector("eDP-1", vec![mode(1920, 1080, 60.0), mode(1280, 720, 60.0)]),
            connector(
                "HDMI-A-1",
                vec![mode(1920, 1080, 60.0), mode(1280, 1024, 75.025)],
            ),
            connector("DP-1", vec![mode(2560, 1440, 60.0)]),
        ]
    }

    /// Connector, then mode id, position, primary and leader when lit.
    type Summary = Vec<(String, Option<(String, (i32, i32), bool, Option<String>)>)>;

    fn summary(layouts: &[OutputLayout]) -> Summary {
        layouts
            .iter()
            .map(|l| {
                (
                    l.connector.clone(),
                    l.enabled
                        .as_ref()
                        .map(|e| (e.mode.unwrap().id(), e.loc, e.primary, e.mirror_of.clone())),
                )
            })
            .collect()
    }

    fn stored(logical: Vec<LogicalMonitorConfig>, disabled: &[&str]) -> MonitorsConfig {
        MonitorsConfig {
            logical,
            disabled: disabled.iter().map(|c| MonitorSpec::connector(c)).collect(),
        }
    }

    fn stored_logical(
        x: i32,
        primary: bool,
        monitors: &[(&str, Option<ModeSpec>)],
    ) -> LogicalMonitorConfig {
        LogicalMonitorConfig {
            x,
            y: 0,
            scale: 1.0,
            transform: 0,
            primary,
            monitors: monitors
                .iter()
                .map(|(c, m)| MonitorConfig {
                    spec: MonitorSpec::connector(c),
                    mode: *m,
                })
                .collect(),
        }
    }

    #[test]
    fn sessions_start_with_the_stored_modes_mirrors_and_disabled_monitors() {
        let connectors = startup_connectors();
        let lit = |id: &str, x: i32, primary: bool, mirror: Option<&str>| {
            Some((id.to_owned(), (x, 0), primary, mirror.map(str::to_owned)))
        };
        // Nothing stored: preferred modes left to right at TUNA_SCALE.
        let linear = startup_layouts(None, &connectors, 1.25);
        assert_eq!(
            summary(&linear),
            [
                ("eDP-1".to_owned(), lit("1920x1080@60.000", 0, true, None)),
                (
                    "HDMI-A-1".to_owned(),
                    lit("1920x1080@60.000", 1536, false, None)
                ),
                (
                    "DP-1".to_owned(),
                    lit("2560x1440@60.000", 3072, false, None)
                ),
            ]
        );
        assert_eq!(linear[0].enabled.as_ref().unwrap().scale, 1.25);
        // A stored mirror with a switched-off third monitor.
        let mirrored = stored(
            vec![stored_logical(
                0,
                true,
                &[
                    ("eDP-1", Some(mode(1920, 1080, 60.0))),
                    ("HDMI-A-1", Some(mode(1920, 1080, 60.0))),
                ],
            )],
            &["DP-1"],
        );
        assert_eq!(
            summary(&startup_layouts(Some(&mirrored), &connectors, 1.0)),
            [
                ("eDP-1".to_owned(), lit("1920x1080@60.000", 0, true, None)),
                (
                    "HDMI-A-1".to_owned(),
                    lit("1920x1080@60.000", 0, false, Some("eDP-1"))
                ),
                ("DP-1".into(), None),
            ]
        );
        // A clone whose mode size differs stands alone, after the rest;
        // a stored mode the connector lacks becomes the preferred one.
        let mismatched = stored(
            vec![stored_logical(
                0,
                true,
                &[
                    ("eDP-1", Some(mode(3000, 2000, 60.0))),
                    ("HDMI-A-1", Some(mode(1280, 1024, 75.025))),
                ],
            )],
            &["DP-1"],
        );
        assert_eq!(
            summary(&startup_layouts(Some(&mismatched), &connectors, 1.0)),
            [
                ("eDP-1".to_owned(), lit("1920x1080@60.000", 0, true, None)),
                (
                    "HDMI-A-1".to_owned(),
                    lit("1280x1024@75.025", 1920, false, None)
                ),
                ("DP-1".into(), None),
            ]
        );
        // Everything stored off (or only absent monitors lit): linear.
        let dark = stored(
            vec![stored_logical(0, true, &[("DP-9", None)])],
            &["eDP-1", "HDMI-A-1", "DP-1"],
        );
        assert_eq!(
            summary(&startup_layouts(Some(&dark), &connectors, 1.25)),
            summary(&linear)
        );
        // A stored mode change and a missing primary flag.
        let laptop_720 = stored(
            vec![
                stored_logical(0, false, &[("eDP-1", Some(mode(1280, 720, 60.0)))]),
                stored_logical(1280, false, &[("HDMI-A-1", None)]),
            ],
            &[],
        );
        assert_eq!(
            summary(&startup_layouts(Some(&laptop_720), &connectors, 1.0)),
            [
                ("eDP-1".to_owned(), lit("1280x720@60.000", 0, true, None)),
                (
                    "HDMI-A-1".to_owned(),
                    lit("1920x1080@60.000", 1280, false, None)
                ),
                (
                    "DP-1".to_owned(),
                    lit("2560x1440@60.000", 3200, false, None)
                ),
            ]
        );
    }
}
