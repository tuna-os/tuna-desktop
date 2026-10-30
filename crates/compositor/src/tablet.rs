//! Drawing tablets (P-IN-05, #344): GNOME 51's per-device tablet and
//! stylus settings and how Mutter turns them into what a client sees.
//!
//! Everything here is backend-free policy, ported from Mutter 51.0:
//!
//! - settings paths and keys: `src/backends/meta-input-settings.c`
//!   (`lookup_device_settings`, `lookup_tool_settings`,
//!   `update_stylus_pressure`, `update_tablet_*`);
//! - which monitor a tablet drives and absolute positions: the shared
//!   [`crate::input_mapper`] (`meta-input-mapper.c`,
//!   `meta_monitor_manager_get_monitor_matrix`,
//!   `meta_input_device_native_translate_coordinates_in_impl`), plus
//!   keep-aspect (`update_tablet_keep_aspect`);
//! - the pressure curve: `src/backends/native/meta-bezier.c` and
//!   `meta-input-device-tool-native.c` (`translate_pressure_in_impl`);
//! - stylus buttons: `src/backends/native/meta-seat-impl.c`
//!   (`meta_seat_impl_notify_button_in_impl`) and `meta-backend.c`
//!   (`meta_evdev_tool_button_to_clutter`).
//!
//! Mutter consults libwacom for a device's integration flags; Tuna
//! Desktop does not, so every tablet takes the path Mutter takes for a
//! device libwacom does not know (integrated, not built in).

use crate::input_mapper::{pick_monitor, AbsoluteMapping, InputIdentity, InputKind, Monitor};
use smithay::backend::input::TabletToolDescriptor;
use smithay::utils::{Logical, Point};
use smithay::wayland::tablet_manager::{TabletDescriptor, TabletPadDescriptor};

/// A tool's axes in one frame, already through the stylus settings
/// (pressure on its curve); `None` when the tool lacks the axis or it
/// did not change.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ToolAxes {
    pub pressure: Option<f64>,
    pub distance: Option<f64>,
    pub tilt: Option<(f64, f64)>,
    pub rotation: Option<f64>,
    pub slider: Option<f64>,
    /// Degrees and discrete clicks.
    pub wheel: Option<(f64, i32)>,
}

/// What the hardware backend reports of tablets, in desktop coordinates
/// with GNOME's settings applied; the window manager routes it.
#[derive(Debug, Clone)]
pub enum TabletEvent {
    TabletAdded(TabletDescriptor),
    TabletRemoved(TabletDescriptor),
    /// A pad, with the tablet in its device group (Mutter pairs them).
    PadAdded {
        pad: TabletPadDescriptor,
        tablet: Option<TabletDescriptor>,
    },
    PadRemoved(TabletPadDescriptor),
    ProximityIn {
        tablet: TabletDescriptor,
        tool: TabletToolDescriptor,
        pos: Point<f64, Logical>,
        axes: ToolAxes,
        time: u32,
    },
    ProximityOut {
        tablet: TabletDescriptor,
        tool: TabletToolDescriptor,
        time: u32,
    },
    Motion {
        tablet: TabletDescriptor,
        tool: TabletToolDescriptor,
        pos: Point<f64, Logical>,
        axes: ToolAxes,
        time: u32,
    },
    /// Clutter's primary button: the tip, or a stylus button mapped to it.
    Tip {
        tablet: TabletDescriptor,
        tool: TabletToolDescriptor,
        down: bool,
        pos: Point<f64, Logical>,
        time: u32,
    },
    /// A stylus button with the code its settings map it to.
    Button {
        tablet: TabletDescriptor,
        tool: TabletToolDescriptor,
        button: u32,
        pressed: bool,
        time: u32,
    },
    /// `mode` is the (group, new mode) of a mode-toggle button.
    PadButton {
        pad: TabletPadDescriptor,
        button: u32,
        pressed: bool,
        mode: Option<(usize, u32)>,
        time: u32,
    },
    PadRing {
        pad: TabletPadDescriptor,
        ring: u32,
        angle: Option<f64>,
        finger: bool,
        time: u32,
    },
    PadStrip {
        pad: TabletPadDescriptor,
        strip: u32,
        position: Option<f64>,
        finger: bool,
        time: u32,
    },
    /// A tablet action mapper's `keybinding`: emulate one press or
    /// release of this accelerator on a virtual keyboard
    /// (`meta_tablet_action_mapper_emulate_keybinding`).
    EmulateKeybinding {
        accel: String,
        pressed: bool,
        time: u32,
    },
    /// `switch-monitor` picked the next output: write the tablet's
    /// `output` setting at `path` (vendor, product, serial; all empty is
    /// the span of every monitor).
    SetOutput {
        path: String,
        output: [String; 3],
    },
    /// `meta_display_show_osd`: icon and text on a monitor (`None` is the
    /// current one).
    ShowOsd {
        connector: Option<String>,
        icon: &'static str,
        label: String,
    },
    /// The `help` pad action: show this pad's OSD.
    RequestPadOsd {
        pad: u32,
        time: u32,
    },
    /// A pad event while the pad OSD is shown: the shell's, not a client's.
    PadOsdInput {
        pad: u32,
        event: tuna_shell_control::PadOsdEvent,
        time: u32,
    },
    /// The shown pad OSD's pad is gone.
    ClosePadOsd {
        pad: u32,
    },
}

impl TabletEvent {
    /// The event time; device hotplug and compositor-side effects carry none.
    pub fn time(&self) -> Option<u32> {
        match self {
            Self::TabletAdded(_)
            | Self::TabletRemoved(_)
            | Self::PadAdded { .. }
            | Self::PadRemoved(_)
            | Self::SetOutput { .. }
            | Self::ShowOsd { .. }
            | Self::ClosePadOsd { .. } => None,
            Self::ProximityIn { time, .. }
            | Self::ProximityOut { time, .. }
            | Self::Motion { time, .. }
            | Self::Tip { time, .. }
            | Self::Button { time, .. }
            | Self::PadButton { time, .. }
            | Self::PadRing { time, .. }
            | Self::PadStrip { time, .. }
            | Self::EmulateKeybinding { time, .. }
            | Self::RequestPadOsd { time, .. }
            | Self::PadOsdInput { time, .. } => Some(*time),
        }
    }
}

pub const TABLET_SCHEMA: &str = "org.gnome.desktop.peripherals.tablet";
pub const STYLUS_SCHEMA: &str = "org.gnome.desktop.peripherals.tablet.stylus";

/// Kernel button codes (linux/input-event-codes.h).
pub const BTN_LEFT: u32 = 0x110;
pub const BTN_RIGHT: u32 = 0x111;
pub const BTN_MIDDLE: u32 = 0x112;
pub const BTN_BACK: u32 = 0x116;
pub const BTN_FORWARD: u32 = 0x115;
pub const BTN_TOUCH: u32 = 0x14a;
pub const BTN_STYLUS: u32 = 0x14b;
pub const BTN_STYLUS2: u32 = 0x14c;
pub const BTN_STYLUS3: u32 = 0x149;

/// `lookup_device_settings`: one relocatable settings object per
/// vendor:product, in lowercase four-digit hex.
pub fn tablet_settings_path(vendor: u32, product: u32) -> String {
    format!("/org/gnome/desktop/peripherals/tablets/{vendor:04x}:{product:04x}/")
}

/// `lookup_tool_settings`: per tool serial; serial 0 and the Wacom
/// driver's placeholder 1 share the tablet's `default-` path.
pub fn stylus_settings_path(serial: u64, vendor: u32, product: u32) -> String {
    if serial == 0 || serial == 1 {
        format!("/org/gnome/desktop/peripherals/stylus/default-{vendor:04x}:{product:04x}/")
    } else {
        format!("/org/gnome/desktop/peripherals/stylus/{serial:x}/")
    }
}

/// `GDesktopTabletMapping`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mapping {
    #[default]
    Absolute,
    Relative,
}

impl Mapping {
    pub fn from_nick(nick: &str) -> Option<Self> {
        match nick {
            "absolute" => Some(Self::Absolute),
            "relative" => Some(Self::Relative),
            _ => None,
        }
    }
}

/// `org.gnome.desktop.peripherals.tablet`, as read for one device.
#[derive(Debug, Clone, PartialEq)]
pub struct TabletSettings {
    /// `output` when the user set it (vendor, product, serial and
    /// optionally the connector); `None` is Mutter's automatic mapping.
    pub output: Option<Vec<String>>,
    pub mapping: Mapping,
    /// Padding left, right, top, bottom as fractions of the tablet.
    pub area: Vec<f64>,
    pub keep_aspect: bool,
    pub left_handed: bool,
}

impl Default for TabletSettings {
    /// gsettings-desktop-schemas 51 defaults.
    fn default() -> Self {
        Self {
            output: None,
            mapping: Mapping::Absolute,
            area: vec![0.0; 4],
            keep_aspect: false,
            left_handed: false,
        }
    }
}

impl TabletSettings {
    /// `update_tablet_area`: four paddings, or nothing applies.
    pub fn area(&self) -> Option<[f64; 4]> {
        <[f64; 4]>::try_from(self.area.as_slice()).ok()
    }
}

/// libinput's calibration matrix for a tablet area
/// (`meta_input_settings_native_set_tablet_area`); `None` restores the
/// device default.
pub fn area_matrix(area: [f64; 4]) -> Option<[f32; 6]> {
    let [left, right, top, bottom] = area;
    if left == 0.0 && right == 0.0 && top == 0.0 && bottom == 0.0 {
        return None;
    }
    let scale_x = (1.0 / (1.0 - (left + right))) as f32;
    let scale_y = (1.0 / (1.0 - (top + bottom))) as f32;
    let offset_x = -(left as f32) * scale_x;
    let offset_y = -(top as f32) * scale_y;
    Some([scale_x, 0.0, offset_x, 0.0, scale_y, offset_y])
}

/// `GDesktopStylusButtonAction`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonAction {
    Default,
    Middle,
    Right,
    Back,
    Forward,
    SwitchMonitor,
    Keybinding,
}

impl ButtonAction {
    pub fn from_nick(nick: &str) -> Option<Self> {
        Some(match nick {
            "default" => Self::Default,
            "middle" => Self::Middle,
            "right" => Self::Right,
            "back" => Self::Back,
            "forward" => Self::Forward,
            "switch-monitor" => Self::SwitchMonitor,
            "keybinding" => Self::Keybinding,
            _ => return None,
        })
    }
}

/// `GDesktopStylusEraserButtonMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EraserButtonMode {
    #[default]
    Default,
    Button,
}

impl EraserButtonMode {
    pub fn from_nick(nick: &str) -> Option<Self> {
        match nick {
            "default" => Some(Self::Default),
            "button" => Some(Self::Button),
            _ => None,
        }
    }
}

/// `org.gnome.desktop.peripherals.tablet.stylus`, as read for one tool.
#[derive(Debug, Clone, PartialEq)]
pub struct StylusSettings {
    pub pressure_curve: Vec<i32>,
    pub pressure_range: Vec<i32>,
    pub eraser_pressure_curve: Vec<i32>,
    pub eraser_pressure_range: Vec<i32>,
    pub eraser_button_mode: EraserButtonMode,
    pub eraser_button_action: ButtonAction,
    pub button_action: ButtonAction,
    pub secondary_button_action: ButtonAction,
    pub tertiary_button_action: ButtonAction,
    /// `*-keybinding`: the accelerator a `keybinding` action emulates
    /// (`meta_input_settings_get_tool_button_action`).
    pub eraser_button_keybinding: String,
    pub button_keybinding: String,
    pub secondary_button_keybinding: String,
    pub tertiary_button_keybinding: String,
}

impl Default for StylusSettings {
    /// gsettings-desktop-schemas 51 defaults.
    fn default() -> Self {
        Self {
            pressure_curve: vec![0, 0, 100, 100],
            pressure_range: vec![0, 100],
            eraser_pressure_curve: vec![0, 0, 100, 100],
            eraser_pressure_range: vec![0, 100],
            eraser_button_mode: EraserButtonMode::Default,
            eraser_button_action: ButtonAction::Default,
            button_action: ButtonAction::Right,
            secondary_button_action: ButtonAction::Middle,
            tertiary_button_action: ButtonAction::Default,
            eraser_button_keybinding: String::new(),
            button_keybinding: String::new(),
            secondary_button_keybinding: String::new(),
            tertiary_button_keybinding: String::new(),
        }
    }
}

/// What `update_stylus_pressure` hands the backend: the curve's two
/// control points and the libinput pressure range, or `None` when the
/// stored values are rejected (the tool keeps what it had).
pub fn stylus_pressure(settings: &StylusSettings, eraser: bool) -> Option<([f64; 4], [f64; 2])> {
    let (curve, range) = if eraser {
        (
            &settings.eraser_pressure_curve,
            &settings.eraser_pressure_range,
        )
    } else {
        (&settings.pressure_curve, &settings.pressure_range)
    };
    let curve = <[i32; 4]>::try_from(curve.as_slice()).ok()?;
    let range = <[i32; 2]>::try_from(range.as_slice()).ok()?;
    // `guint32` percentages: negative values wrap huge, then clamp to 1.
    let percent = |p: i32| (f64::from(p as u32) / 100.0).clamp(0.0, 1.0);
    let range = [percent(range[0]), percent(range[1])];
    if range[0] >= range[1] {
        return None;
    }
    let curve = curve.map(|c| f64::from(c) / 100.0);
    // set_pressure_curve_in_impl returns early outside [0, 1].
    if curve.iter().any(|c| !(0.0..=1.0).contains(c)) {
        return None;
    }
    Some((curve, range))
}

/// Mutter's `MetaBezier`, sampled at `N_PRESSURECURVE_POINTS`, in the
/// same 14.18 fixed point so pressures match GNOME's bit for bit.
#[derive(Debug, Clone, PartialEq)]
pub struct PressureCurve {
    points: Vec<f64>,
}

const N_PRESSURECURVE_POINTS: usize = 256;
const BEZIER_MAX_LENGTH: i32 = 1 << 18;
const CBZ_T_Q: i32 = 18;

fn cbz_mul(x: i32, y: i32) -> i32 {
    ((x >> 3).wrapping_mul(y >> 3)) >> 12
}

#[derive(Debug, Clone, Copy)]
struct Coefficients {
    ax: i32,
    bx: i32,
    cx: i32,
    dx: i32,
    ay: i32,
    by: i32,
    cy: i32,
    dy: i32,
}

impl Coefficients {
    fn t2(a: i32, b: i32, c: i32, d: i32, t: i32) -> i32 {
        let pow2 = cbz_mul(t, t);
        let pow3 = cbz_mul(pow2, t);
        (a.wrapping_mul(pow3)
            .wrapping_add(b.wrapping_mul(pow2))
            .wrapping_add(c.wrapping_mul(t))
            >> CBZ_T_Q)
            .wrapping_add(d)
    }
    fn at(&self, t: i32) -> (i32, i32) {
        (
            Self::t2(self.ax, self.bx, self.cx, self.dx, t),
            Self::t2(self.ay, self.by, self.cy, self.dy, t),
        )
    }
}

impl PressureCurve {
    /// `meta_bezier_init` for control points (x1, y1, x2, y2) in [0, 1].
    pub fn new(curve: [f64; 4]) -> Self {
        let n = N_PRESSURECURVE_POINTS as f64;
        let (x0, y0) = (0.0f64, 0.0f64);
        let [x1, y1, x2, y2] = curve.map(|c| c * n);
        let (x3, y3) = (n, n);
        let c = Coefficients {
            dx: x0 as i32,
            dy: y0 as i32,
            cx: (3.0 * (x1 - x0)) as i32,
            cy: (3.0 * (y1 - y0)) as i32,
            bx: (3.0 * (x2 - x1) - (3.0 * (x1 - x0)).trunc()) as i32,
            by: (3.0 * (y2 - y1) - (3.0 * (y1 - y0)).trunc()) as i32,
            ax: (x3 - 3.0 * x2 + 3.0 * x1 - x0) as i32,
            ay: (y3 - 3.0 * y2 + 3.0 * y1 - y0) as i32,
        };
        Self {
            points: Self::sample(&c),
        }
    }

    /// `meta_bezier_sample`.
    fn sample(b: &Coefficients) -> Vec<f64> {
        const N: usize = N_PRESSURECURVE_POINTS;
        const MIN_EPSILON: f64 = 0.00001;
        let mut points = vec![-1.0f64; N];
        let mut ts = vec![-1.0f64; N];
        ts[0] = 0.0;
        ts[N - 1] = 1.0;
        points[0] = 0.0;
        let (_, last_y) = b.at(BEZIER_MAX_LENGTH);
        points[N - 1] = f64::from(last_y) / N as f64;
        let mut epsilon = 1.0 / N as f64;
        let mut t = epsilon;
        loop {
            for i in 1..N {
                if ts[i] == -1.0 {
                    t = ts[i - 1];
                    epsilon /= 2.0;
                    break;
                }
            }
            while t < 1.0 {
                let (x, y) = b.at((t * f64::from(BEZIER_MAX_LENGTH)) as i32);
                if x >= 0 && (x as usize) < N {
                    let x = x as usize;
                    if points[x.saturating_sub(1)] == -1.0 && epsilon > MIN_EPSILON {
                        break;
                    }
                    if points[x] == -1.0 {
                        points[x] = f64::from(y) / N as f64;
                        ts[x] = t;
                    }
                }
                t += epsilon;
            }
            if !(t < 1.0 && epsilon > MIN_EPSILON) {
                break;
            }
        }
        // Linear interpolation of the remaining points, with Mutter's
        // integer truncation.
        let mut i = 0;
        while i < N - 1 {
            i += 1;
            let mut prev = points[i - 1] as i32;
            if points[i] == -1.0 {
                let mut next_idx = i;
                let mut next;
                loop {
                    next_idx += 1;
                    next = points[next_idx] as i32;
                    if !(f64::from(next) == -1.0 && next_idx < N - 1) {
                        break;
                    }
                }
                let delta = (next - prev) / (next_idx - i + 1) as i32;
                while i < next_idx {
                    points[i] = f64::from(prev + delta);
                    prev = points[i] as i32;
                    i += 1;
                }
            }
        }
        points
    }

    /// `meta_bezier_lookup`.
    fn lookup(&self, pos: f64) -> f64 {
        let n = self.points.len() as i32;
        let low_idx = ((pos * f64::from(n)) as i32).clamp(0, n - 1);
        let high_idx = (low_idx + 1).clamp(0, n - 1);
        let low = self.points[low_idx as usize];
        let high = self.points[high_idx as usize];
        low + (high - low) * (pos - pos.trunc())
    }

    /// `meta_input_device_tool_native_translate_pressure_in_impl`: the
    /// clamped pressure scaled by the curve's value there.
    pub fn translate(&self, pressure: f64) -> f64 {
        let pressure = pressure.clamp(0.0, 1.0);
        pressure * self.lookup(pressure)
    }
}

impl Default for PressureCurve {
    fn default() -> Self {
        Self::new([0.0, 0.0, 1.0, 1.0])
    }
}

/// What a stylus button press becomes (`meta_seat_impl_notify_button_in_impl`
/// with `meta_tool_action_mapper_handle_button`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolButton {
    /// Clutter's primary button: the tool's down/up.
    Tip,
    /// `zwp_tablet_tool_v2.button` with this code.
    Button(u32),
    /// Taken by the compositor's tablet action mapper, never delivered:
    /// `switch-monitor`, or `keybinding` with the slot's accelerator.
    Action(ButtonAction, String),
}

/// Map a physical stylus button through the tool's settings. `eraser_button`
/// is the code libinput reports for a tool with an eraser button (Mutter
/// hardcodes BTN_STYLUS3 before libinput 1.32).
pub fn map_tool_button(
    evdev: u32,
    settings: &StylusSettings,
    eraser_button: Option<u32>,
    mouse_or_lens: bool,
) -> ToolButton {
    let (action, keybinding) = if eraser_button == Some(evdev) {
        (
            settings.eraser_button_action,
            &settings.eraser_button_keybinding,
        )
    } else {
        // meta_evdev_tool_button_to_clutter, then the slots
        // set_stylus_button_map fills from the settings keys. Codes with
        // no slot look up nothing: Mutter's default action.
        let clutter = match evdev {
            BTN_TOUCH | BTN_LEFT => 1,
            BTN_STYLUS | BTN_MIDDLE => 2,
            BTN_STYLUS2 | BTN_RIGHT => 3,
            BTN_STYLUS3 => 8,
            _ => 0,
        };
        let keyed = if mouse_or_lens {
            match clutter {
                3 => Some((settings.button_action, &settings.button_keybinding)),
                2 => Some((
                    settings.secondary_button_action,
                    &settings.secondary_button_keybinding,
                )),
                _ => None,
            }
        } else {
            match clutter {
                2 => Some((settings.button_action, &settings.button_keybinding)),
                3 => Some((
                    settings.secondary_button_action,
                    &settings.secondary_button_keybinding,
                )),
                8 => Some((
                    settings.tertiary_button_action,
                    &settings.tertiary_button_keybinding,
                )),
                _ => None,
            }
        };
        keyed.unwrap_or((ButtonAction::Default, &settings.button_keybinding))
    };
    match action {
        ButtonAction::Default => ToolButton::Tip,
        ButtonAction::Middle => ToolButton::Button(BTN_STYLUS),
        ButtonAction::Right => ToolButton::Button(BTN_STYLUS2),
        ButtonAction::Back => ToolButton::Button(BTN_BACK),
        ButtonAction::Forward => ToolButton::Button(BTN_FORWARD),
        ButtonAction::SwitchMonitor => ToolButton::Action(action, String::new()),
        ButtonAction::Keybinding => ToolButton::Action(action, keybinding.clone()),
    }
}

/// How one tablet's normalized position becomes a desktop position: the
/// shared input mapper's matrix plus GNOME's keep-aspect and mapping mode.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DeviceMapping {
    pub absolute: AbsoluteMapping,
    pub mode: Mapping,
}

impl DeviceMapping {
    /// The monitor this tablet drives (`None`: the whole desktop) and how
    /// its position maps there (`update_tablet_keep_aspect`: the
    /// monitor's ratio only for keep-aspect absolute mapping).
    pub fn new(
        device: &InputIdentity,
        settings: &TabletSettings,
        monitors: &[Monitor],
    ) -> (Self, Option<usize>) {
        let picked = pick_monitor(
            InputKind::Tablet,
            device,
            settings.output.as_deref(),
            monitors,
        )
        .map(|picked| picked.index);
        let (mut absolute, ratio) = AbsoluteMapping::new(picked.map(|i| &monitors[i]), monitors);
        if settings.keep_aspect && settings.mapping == Mapping::Absolute {
            absolute.output_ratio = ratio;
        }
        absolute.device_ratio = device
            .size_mm
            .filter(|(_, h)| *h > 0.0)
            .map_or(0.0, |(w, h)| w / h);
        (
            Self {
                absolute,
                mode: settings.mapping,
            },
            picked,
        )
    }

    /// `translate_coordinates_in_impl` for a libinput position already
    /// normalized to [0, 1] (`get_x_transformed` over the desktop).
    pub fn translate(&self, norm: (f64, f64), stage: (f64, f64)) -> Point<f64, Logical> {
        self.absolute.translate(norm, stage)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input_mapper::stage_size;
    use smithay::utils::Rectangle;

    fn monitor(connector: &str, x: i32, w: i32, h: i32) -> Monitor {
        Monitor {
            connector: connector.into(),
            vendor: "GSM".into(),
            product: "LG ULTRAFINE".into(),
            serial: connector.into(),
            width_mm: 600,
            height_mm: 340,
            rect: Rectangle::new((x, 0).into(), (w, h).into()),
            transform: 0,
            mode: (w, h),
            builtin: false,
        }
    }

    fn pen() -> InputIdentity {
        InputIdentity {
            name: "Wacom Intuos Pro M Pen".into(),
            size_mm: Some((224.0, 148.0)),
        }
    }

    #[test]
    fn settings_paths_follow_mutters_lookup() {
        assert_eq!(
            tablet_settings_path(0x56a, 0x357),
            "/org/gnome/desktop/peripherals/tablets/056a:0357/"
        );
        assert_eq!(
            stylus_settings_path(0, 0x56a, 0x357),
            "/org/gnome/desktop/peripherals/stylus/default-056a:0357/"
        );
        assert_eq!(
            stylus_settings_path(1, 0x56a, 0x357),
            "/org/gnome/desktop/peripherals/stylus/default-056a:0357/"
        );
        assert_eq!(
            stylus_settings_path(0x8a4c1d2, 0x56a, 0x357),
            "/org/gnome/desktop/peripherals/stylus/8a4c1d2/"
        );
    }

    #[test]
    fn automatic_external_tablets_span_the_desktop_and_a_set_output_picks_one() {
        let monitors = [
            monitor("DP-1", 0, 1920, 1080),
            monitor("HDMI-A-1", 1920, 1280, 1024),
        ];
        // Automatic: no name or size matches, the whole desktop.
        let (mapping, picked) = DeviceMapping::new(&pen(), &TabletSettings::default(), &monitors);
        assert_eq!(picked, None);
        let stage = stage_size(&monitors);
        assert_eq!(stage, (3200.0, 1080.0));
        assert_eq!(mapping.translate((0.5, 0.5), stage), (1600.0, 540.0).into());
        // GNOME Settings writes vendor, product, serial and connector.
        let settings = TabletSettings {
            output: Some(vec![
                "GSM".into(),
                "LG ULTRAFINE".into(),
                "HDMI-A-1".into(),
                "HDMI-A-1".into(),
            ]),
            ..TabletSettings::default()
        };
        let (mapping, picked) = DeviceMapping::new(&pen(), &settings, &monitors);
        assert_eq!(picked, Some(1));
        assert_eq!(mapping.translate((0.0, 0.0), stage), (1920.0, 0.0).into());
        assert_eq!(
            mapping.translate((1.0, 1.0), stage),
            (3200.0, 1024.0).into()
        );
        // Never leaves the mapped monitor.
        assert_eq!(
            mapping.translate((1.0, 1.0), stage),
            mapping.translate((1.0, 1.0), stage)
        );
        // An unknown monitor falls back to the whole desktop.
        let settings = TabletSettings {
            output: Some(vec!["XYZ".into(), "Nope".into(), "1".into()]),
            ..TabletSettings::default()
        };
        assert_eq!(DeviceMapping::new(&pen(), &settings, &monitors).1, None);
        // The all-empty default written back explicitly is still no match.
        let settings = TabletSettings {
            output: Some(vec![String::new(); 3]),
            ..TabletSettings::default()
        };
        assert_eq!(DeviceMapping::new(&pen(), &settings, &monitors).1, None);
    }

    #[test]
    fn rotated_monitors_rotate_the_tablet() {
        let mut portrait = monitor("DP-1", 0, 1080, 1920);
        portrait.transform = 1; // 90°
        portrait.mode = (1920, 1080);
        let monitors = [portrait];
        let settings = TabletSettings {
            output: Some(vec!["GSM".into(), "LG ULTRAFINE".into(), "DP-1".into()]),
            ..TabletSettings::default()
        };
        let (mapping, _) = DeviceMapping::new(&pen(), &settings, &monitors);
        let stage = stage_size(&monitors);
        // Mutter's 90° matrix: the tablet's top-left lands top-right.
        assert_eq!(mapping.translate((0.0, 0.0), stage), (1080.0, 0.0).into());
        assert_eq!(
            mapping.translate((1.0, 0.0), stage),
            (1080.0, 1920.0).into()
        );
    }

    #[test]
    fn keep_aspect_uses_the_monitor_ratio_only_for_absolute_mapping() {
        let monitors = [monitor("DP-1", 0, 1920, 1080)];
        let stage = stage_size(&monitors);
        let wide = InputIdentity {
            name: "Pen".into(),
            size_mm: Some((320.0, 100.0)), // 3.2 : 1, wider than 16:9
        };
        let settings = TabletSettings {
            keep_aspect: true,
            ..TabletSettings::default()
        };
        let (mapping, _) = DeviceMapping::new(&wide, &settings, &monitors);
        assert!((mapping.absolute.output_ratio - 1920.0 / 1080.0).abs() < 1e-9);
        // The right part of the tablet runs off the screen and clamps.
        let ratio = 3.2 / (1920.0 / 1080.0);
        let p = mapping.translate((0.25, 0.5), stage);
        assert!((p.x - 0.25 * ratio * 1920.0).abs() < 1e-6);
        assert_eq!(mapping.translate((0.9, 0.5), stage).x, 1920.0);
        // A tall tablet scales y instead.
        let tall = InputIdentity {
            name: "Pen".into(),
            size_mm: Some((100.0, 100.0)),
        };
        let (mapping, _) = DeviceMapping::new(&tall, &settings, &monitors);
        let p = mapping.translate((0.5, 0.25), stage);
        assert!((p.y - 0.25 * (1920.0 / 1080.0) * 1080.0).abs() < 1e-6);
        assert!((p.x - 960.0).abs() < 1e-6);
        // Relative mapping or keep-aspect off: no ratio.
        for settings in [
            TabletSettings {
                keep_aspect: true,
                mapping: Mapping::Relative,
                ..TabletSettings::default()
            },
            TabletSettings::default(),
        ] {
            let (mapping, _) = DeviceMapping::new(&wide, &settings, &monitors);
            assert_eq!(mapping.absolute.output_ratio, 0.0);
            assert_eq!(mapping.translate((0.25, 0.5), stage), (480.0, 540.0).into());
        }
    }

    #[test]
    fn tablet_area_becomes_libinputs_calibration_matrix() {
        assert_eq!(area_matrix([0.0; 4]), None);
        let m = area_matrix([0.1, 0.1, 0.25, 0.25]).unwrap();
        // libinput maps raw r to m*r + offset: the area's edges to 0 and 1.
        let x = |raw: f32| m[0] * raw + m[2];
        let y = |raw: f32| m[4] * raw + m[5];
        assert!(x(0.1).abs() < 1e-6 && (x(0.9) - 1.0).abs() < 1e-6);
        assert!(y(0.25).abs() < 1e-6 && (y(0.75) - 1.0).abs() < 1e-6);
        let short = TabletSettings {
            area: vec![0.1, 0.1],
            ..TabletSettings::default()
        };
        assert_eq!(short.area(), None);
    }

    #[test]
    fn pressure_curves_scale_pressure_as_mutter_does() {
        let linear = PressureCurve::default();
        // Mutter multiplies the pressure by the curve's value there: the
        // default straight curve squares it.
        for p in [0.0, 0.25, 0.5, 0.75] {
            let got = linear.translate(p);
            assert!((got - p * p).abs() < 0.01, "{p} -> {got}");
        }
        assert!((linear.translate(1.0) - 1.0).abs() < 0.01);
        assert_eq!(linear.translate(-1.0), 0.0);
        // A soft curve (GNOME Settings' "soft" end) raises light strokes.
        let soft = PressureCurve::new([0.0, 0.75, 0.25, 1.0]);
        assert!(soft.translate(0.25) > linear.translate(0.25) * 2.0);
        let firm = PressureCurve::new([0.75, 0.0, 1.0, 0.25]);
        assert!(firm.translate(0.5) < linear.translate(0.5));
        // Monotonic over the range.
        let mut last = 0.0;
        for i in 0..=100 {
            let v = soft.translate(f64::from(i) / 100.0);
            assert!(v + 1e-9 >= last, "{i}: {v} < {last}");
            last = v;
        }
    }

    #[test]
    fn pressure_settings_are_validated_like_update_stylus_pressure() {
        let mut settings = StylusSettings::default();
        assert_eq!(
            stylus_pressure(&settings, false),
            Some(([0.0, 0.0, 1.0, 1.0], [0.0, 1.0]))
        );
        settings.eraser_pressure_range = vec![10, 80];
        settings.eraser_pressure_curve = vec![0, 50, 50, 100];
        assert_eq!(
            stylus_pressure(&settings, true),
            Some(([0.0, 0.5, 0.5, 1.0], [0.1, 0.8]))
        );
        for bad in [vec![50, 50], vec![60, 40], vec![0], vec![0, 100, 3]] {
            settings.pressure_range = bad;
            assert_eq!(stylus_pressure(&settings, false), None);
        }
        settings.pressure_range = vec![0, 250];
        assert_eq!(stylus_pressure(&settings, false).unwrap().1, [0.0, 1.0]);
        settings.pressure_curve = vec![0, 0, 120, 100];
        assert_eq!(stylus_pressure(&settings, false), None);
        settings.pressure_curve = vec![0, 0, 100];
        assert_eq!(stylus_pressure(&settings, false), None);
    }

    #[test]
    fn stylus_buttons_follow_gnome_51_defaults_and_settings() {
        let defaults = StylusSettings::default();
        // GNOME 51: the lower button right-clicks, the upper middle-clicks,
        // the third (default) acts as the tip.
        assert_eq!(
            map_tool_button(BTN_STYLUS, &defaults, None, false),
            ToolButton::Button(BTN_STYLUS2)
        );
        assert_eq!(
            map_tool_button(BTN_STYLUS2, &defaults, None, false),
            ToolButton::Button(BTN_STYLUS)
        );
        assert_eq!(
            map_tool_button(BTN_STYLUS3, &defaults, None, false),
            ToolButton::Tip
        );
        assert_eq!(
            map_tool_button(BTN_TOUCH, &defaults, None, false),
            ToolButton::Tip
        );
        let custom = StylusSettings {
            button_action: ButtonAction::Back,
            secondary_button_action: ButtonAction::Forward,
            tertiary_button_action: ButtonAction::SwitchMonitor,
            eraser_button_action: ButtonAction::Keybinding,
            eraser_button_keybinding: "<Control>z".into(),
            tertiary_button_keybinding: "unused".into(),
            ..StylusSettings::default()
        };
        assert_eq!(
            map_tool_button(BTN_STYLUS, &custom, None, false),
            ToolButton::Button(BTN_BACK)
        );
        assert_eq!(
            map_tool_button(BTN_STYLUS2, &custom, None, false),
            ToolButton::Button(BTN_FORWARD)
        );
        assert_eq!(
            map_tool_button(BTN_STYLUS3, &custom, None, false),
            ToolButton::Action(ButtonAction::SwitchMonitor, String::new())
        );
        // An eraser button follows its own keys.
        assert_eq!(
            map_tool_button(BTN_STYLUS3, &custom, Some(BTN_STYLUS3), false),
            ToolButton::Action(ButtonAction::Keybinding, "<Control>z".into())
        );
        let keyed = StylusSettings {
            secondary_button_action: ButtonAction::Keybinding,
            secondary_button_keybinding: "<Alt>Tab".into(),
            ..StylusSettings::default()
        };
        assert_eq!(
            map_tool_button(BTN_STYLUS2, &keyed, None, false),
            ToolButton::Action(ButtonAction::Keybinding, "<Alt>Tab".into())
        );
        // Mouse and lens tools: primary key on the right button.
        assert_eq!(
            map_tool_button(BTN_STYLUS2, &custom, None, true),
            ToolButton::Button(BTN_BACK)
        );
        assert_eq!(
            map_tool_button(BTN_STYLUS, &custom, None, true),
            ToolButton::Button(BTN_FORWARD)
        );
        // Mouse-tool buttons map through their pointer slots; anything
        // else hits no slot and acts as the primary button.
        assert_eq!(
            map_tool_button(BTN_RIGHT, &custom, None, true),
            ToolButton::Button(BTN_BACK)
        );
        assert_eq!(map_tool_button(0x113, &custom, None, true), ToolButton::Tip);
    }
}
