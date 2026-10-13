//! Graphics tablets and GNOME Wacom settings (#344).
//!
//! The compositor advertises `zwp_tablet_manager_v2` (see
//! [`Protocols`](crate::protocols::Protocols)), registers libinput tablet
//! devices on the seat, and routes stylus and tool events to clients.
//! GNOME's Wacom settings (`org.gnome.desktop.peripherals.tablet` and
//! `.tablet.stylus`, flattened onto
//! [`InputSettings`](tuna_shell_control::InputSettings) by the shell)
//! drive output mapping, area calibration, aspect handling, pressure
//! curves and stylus button actions.
//!
//! Handedness is applied in libinput (`apply_libinput`), like Mutter:
//! the device rotates its own coordinates, so the mapping math below
//! never rotates. Relative mapping moves the cursor by the tool's
//! deltas; absolute mapping places it from the tool position.
//!
//! Two behaviors stay open. Pad buttons (`.tablet.pad-button` actions)
//! cannot route: Smithay 0.7 drops libinput pad events before they
//! reach any backend, so no compositor change here can see them; that
//! needs pad plumbing in the backend layer first. And the pressure
//! curve's exact Mutter shape is unconfirmed: Tuna interpolates
//! linearly through GNOME's control points (endpoints and feel match;
//! a smoother Mutter spline would differ between the points).

use smithay::{
    backend::input::{TabletToolCapabilities, TabletToolDescriptor, TabletToolType},
    utils::{Logical, Point, Rectangle},
    wayland::tablet_manager::TabletDescriptor,
};

use tuna_shell_control::{InputSettings, StylusButtonAction};

/// A tool event the hardware backend could not attribute to a
/// registered tablet (device gone, or a backend without tablet
/// identity): the cursor still follows; tool protocol events wait for
/// a registered tablet.
pub const UNKNOWN_TABLET: u32 = u32::MAX;

/// Evdev codes for stylus barrel buttons (linux/input-event-codes.h).
pub const BTN_STYLUS: u32 = 0x14b;
pub const BTN_STYLUS2: u32 = 0x14c;

/// Tip contact clicks as the primary button, as in GNOME.
pub const TIP_BUTTON: u32 = 0x110;

/// Evdev button codes synthesized for stylus button actions.
pub const BTN_RIGHT: u32 = 0x111;
pub const BTN_MIDDLE: u32 = 0x112;
pub const BTN_FORWARD: u32 = 0x115;
pub const BTN_BACK: u32 = 0x116;

/// Copy-safe identity of one physical tablet tool: everything the
/// manager needs to re-register the tool with the tablet seat after
/// the event crossed the [`ManagerInput`](crate::windows::ManagerInput)
/// boundary (which stays `Copy`; see there).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TabletToolId {
    pub tool_type: TabletToolType,
    pub hardware_serial: u64,
    pub hardware_id_wacom: u64,
    /// [`TabletToolCapabilities`] bits; unknown bits do not survive the
    /// round trip (see [`TabletToolId::descriptor`]).
    pub capabilities: u32,
}

impl TabletToolId {
    pub fn from_descriptor(tool: &TabletToolDescriptor) -> Self {
        Self {
            tool_type: tool.tool_type,
            hardware_serial: tool.hardware_serial,
            hardware_id_wacom: tool.hardware_id_wacom,
            capabilities: tool.capabilities.bits(),
        }
    }

    pub fn descriptor(&self) -> TabletToolDescriptor {
        TabletToolDescriptor {
            tool_type: self.tool_type,
            hardware_serial: self.hardware_serial,
            hardware_id_wacom: self.hardware_id_wacom,
            capabilities: TabletToolCapabilities::from_bits_truncate(self.capabilities),
        }
    }
}

/// One phase of a tablet tool's contact with the tablet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabletToolPhase {
    /// The tool entered proximity (hovering, may not touch yet).
    ProximityIn,
    /// The tool left proximity.
    ProximityOut,
    /// Hover or contact motion with axis updates.
    Axis,
    /// The tip touched down (click).
    TipDown,
    /// The tip lifted.
    TipUp,
    /// A barrel button press or release.
    Button { button: u32, pressed: bool },
}

/// A tablet tool sample crossing from backend to manager: plain data,
/// no backend types, so every backend (and unit test) builds it.
#[derive(Debug, Clone, Copy)]
pub struct TabletToolSample {
    /// Hardware backend's tablet id, or [`UNKNOWN_TABLET`].
    pub tablet: u32,
    pub tool: TabletToolId,
    pub phase: TabletToolPhase,
    /// Absolute position across `area` (the event's mapped size).
    pub pos: Point<f64, Logical>,
    /// Logical size the absolute position maps onto.
    pub area: smithay::utils::Size<i32, Logical>,
    /// Relative motion since the last sample (relative mapping mode).
    pub delta: Point<f64, Logical>,
    /// Raw pressure in `0..=1` (the manager applies the curve).
    pub pressure: f64,
    pub tilt: (f64, f64),
    pub time: u32,
}

/// Tablet settings resolved for one input round: the manager keeps one
/// and rebuilds it on every `SetInputSettings`.
#[derive(Debug, Clone)]
pub struct TabletConfig {
    /// Relative (`relative`) vs absolute mapping.
    pub relative: bool,
    /// EDID `[vendor, product, serial]`; all empty is automatic.
    pub output: [String; 3],
    /// Calibration padding in thousandths of a percent, applied to
    /// left, right, top, bottom.
    pub area_milli: [i32; 4],
    pub keep_aspect: bool,
    pub pressure_curve: [i32; 4],
    pub eraser_pressure_curve: [i32; 4],
    pub button_actions: [StylusButtonAction; 3],
}

impl Default for TabletConfig {
    /// GNOME 51's tablet defaults: absolute mapping onto the primary
    /// output, full area, linear pressure, default button actions.
    fn default() -> Self {
        Self::new(&InputSettings::default())
    }
}

impl TabletConfig {
    pub fn new(settings: &InputSettings) -> Self {
        Self {
            relative: settings.tablet_mapping_relative,
            output: settings.tablet_output.clone(),
            area_milli: settings.tablet_area_milli,
            keep_aspect: settings.tablet_keep_aspect,
            pressure_curve: settings.tablet_pressure_curve,
            eraser_pressure_curve: settings.tablet_eraser_pressure_curve,
            button_actions: [
                settings.tablet_button_action,
                settings.tablet_secondary_button_action,
                settings.tablet_tertiary_button_action,
            ],
        }
    }

    /// Whether no explicit output is configured (automatic mapping).
    pub fn automatic_output(&self) -> bool {
        self.output.iter().all(|part| part.is_empty())
    }

    /// Pressure curve for one tool type: erasers get their own curve.
    pub fn curve_for(&self, tool_type: TabletToolType) -> [i32; 4] {
        if tool_type == TabletToolType::Eraser {
            self.eraser_pressure_curve
        } else {
            self.pressure_curve
        }
    }
}

/// Pick the output rectangle a tablet maps onto. Automatic mapping
/// (an all-empty triple, GNOME's default) takes the primary output,
/// else the first. An explicit triple takes the output whose connector
/// name matches any element; nothing configured here carries EDID
/// make/model/serial, so an unmatched triple falls back to the
/// primary rather than stranding the tablet off-screen. `None` only
/// when no outputs exist at all.
pub fn select_output(
    outputs: &[(String, Rectangle<i32, Logical>, bool)],
    config: &TabletConfig,
) -> Option<Rectangle<i32, Logical>> {
    if outputs.is_empty() {
        return None;
    }
    let primary = outputs
        .iter()
        .find(|(_, _, primary)| *primary)
        .or_else(|| outputs.first())
        .map(|(_, rect, _)| *rect);
    if config.automatic_output() {
        return primary;
    }
    outputs
        .iter()
        .find(|(name, _, _)| config.output.iter().any(|part| part == name))
        .map(|(_, rect, _)| *rect)
        .or(primary)
}

/// Map a normalized tablet position (`0..=1` on each axis) onto the
/// mapped output rectangle: calibration padding insets the active
/// area, then `keep-aspect` letterboxes against the tablet's physical
/// aspect so motion never stretches. Positions outside the active
/// area clamp to its edges; a degenerate calibration (padding that
/// eats the whole area) or an unknown tablet size falls back to the
/// full-area mapping rather than dividing by zero.
pub fn map_absolute(
    nx: f64,
    ny: f64,
    config: &TabletConfig,
    tablet_size_mm: (f64, f64),
    target: Rectangle<i32, Logical>,
) -> Point<f64, Logical> {
    let nx = nx.clamp(0.0, 1.0);
    let ny = ny.clamp(0.0, 1.0);
    let pad = |milli: i32| f64::from(milli.clamp(0, 100_000)) / 100_000.0;
    let (left, right, top, bottom) = (
        pad(config.area_milli[0]),
        pad(config.area_milli[1]),
        pad(config.area_milli[2]),
        pad(config.area_milli[3]),
    );
    let (mut u, mut v) = if left + right >= 1.0 || top + bottom >= 1.0 {
        (nx, ny)
    } else {
        (
            ((nx - left) / (1.0 - left - right)).clamp(0.0, 1.0),
            ((ny - top) / (1.0 - top - bottom)).clamp(0.0, 1.0),
        )
    };
    if config.keep_aspect {
        let (tw, th) = tablet_size_mm;
        let (target_w, target_h) = (f64::from(target.size.w), f64::from(target.size.h));
        if tw > 0.0 && th > 0.0 && target_w > 0.0 && target_h > 0.0 {
            // Restrict the tablet area to the output's aspect: the
            // centered sub-area maps onto the whole output, so one
            // millimeter moves the cursor equally far on both axes.
            let tablet_aspect = tw / th;
            let target_aspect = target_w / target_h;
            let fx = (target_aspect / tablet_aspect).min(1.0);
            let fy = (tablet_aspect / target_aspect).min(1.0);
            u = ((u - (1.0 - fx) / 2.0) / fx).clamp(0.0, 1.0);
            v = ((v - (1.0 - fy) / 2.0) / fy).clamp(0.0, 1.0);
        }
    }
    (
        f64::from(target.loc.x) + u * f64::from(target.size.w),
        f64::from(target.loc.y) + v * f64::from(target.size.h),
    )
        .into()
}

/// Clamp a relative-motion target into the union of outputs (nearest
/// point on the nearest output), so a relative-mapped stylus cannot
/// drift the cursor off-screen.
pub fn clamp_to_outputs(
    pos: Point<f64, Logical>,
    outputs: &[(String, Rectangle<i32, Logical>, bool)],
) -> Point<f64, Logical> {
    if outputs
        .iter()
        .any(|(_, rect, _)| rect.to_f64().contains(pos))
    {
        return pos;
    }
    let mut best = pos;
    let mut best_d = f64::INFINITY;
    for (_, rect, _) in outputs {
        let rect = rect.to_f64();
        let x = pos.x.clamp(rect.loc.x, rect.loc.x + rect.size.w - 1.0);
        let y = pos.y.clamp(rect.loc.y, rect.loc.y + rect.size.h - 1.0);
        let d = (x - pos.x).powi(2) + (y - pos.y).powi(2);
        if d < best_d {
            best_d = d;
            best = (x, y).into();
        }
    }
    best
}

/// Apply a stylus pressure curve: piecewise-linear through GNOME's
/// `(x1, y1)`, `(x2, y2)` control points in percent, pinned at
/// `(0, 0)` and `(100, 100)`. Degenerate points (out of range, out of
/// order) clamp and sort so the curve stays a total function.
pub fn apply_pressure_curve(raw: f64, curve: &[i32; 4]) -> f64 {
    let raw = raw.clamp(0.0, 1.0);
    let clamp = |v: i32| f64::from(v.clamp(0, 100));
    let (mut x1, mut y1, mut x2, mut y2) = (
        clamp(curve[0]),
        clamp(curve[1]),
        clamp(curve[2]),
        clamp(curve[3]),
    );
    if x1 > x2 {
        std::mem::swap(&mut x1, &mut x2);
        std::mem::swap(&mut y1, &mut y2);
    }
    let points = [(0.0, 0.0), (x1, y1), (x2, y2), (100.0, 100.0)];
    let x = raw * 100.0;
    for pair in points.windows(2) {
        let [(ax, ay), (bx, by)] = *pair else {
            continue;
        };
        if x <= bx || bx == 100.0 {
            if bx <= ax {
                return (by / 100.0).clamp(0.0, 1.0);
            }
            let t = ((x - ax) / (bx - ax)).clamp(0.0, 1.0);
            return ((ay + t * (by - ay)) / 100.0).clamp(0.0, 1.0);
        }
    }
    1.0
}

/// Which configured action a barrel button carries: the first barrel
/// button is primary, the second secondary, anything else tertiary.
pub fn stylus_action(button: u32, config: &TabletConfig) -> StylusButtonAction {
    if button == BTN_STYLUS {
        config.button_actions[0]
    } else if button == BTN_STYLUS2 {
        config.button_actions[1]
    } else {
        config.button_actions[2]
    }
}

/// Evdev button code synthesized for one stylus action. `Default`
/// context-clicks, matching the X11 driver's Button 3 barrel default
/// and GNOME's hover-then-barrel right click.
pub fn action_button(action: StylusButtonAction) -> u32 {
    match action {
        StylusButtonAction::Default | StylusButtonAction::Right => BTN_RIGHT,
        StylusButtonAction::Middle => BTN_MIDDLE,
        StylusButtonAction::Back => BTN_BACK,
        StylusButtonAction::Forward => BTN_FORWARD,
    }
}

/// A tablet device arriving or leaving, drained from the hardware
/// backend by the runtime and applied to the manager: owned data, so
/// it travels beside [`ManagerInput`](crate::windows::ManagerInput)
/// (which stays `Copy`). The manager owns all protocol registration;
/// the backend only describes its devices.
#[derive(Debug, Clone)]
pub enum TabletDeviceChange {
    /// A tablet arrived: the manager registers it on the seat and
    /// keeps its physical size (for keep-aspect).
    Added {
        id: u32,
        tablet: TabletDescriptor,
        size_mm: (f64, f64),
    },
    Removed {
        id: u32,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Logical> {
        Rectangle::new((x, y).into(), (w, h).into())
    }

    fn outputs() -> Vec<(String, Rectangle<i32, Logical>, bool)> {
        vec![
            ("eDP-1".to_owned(), rect(0, 0, 1920, 1080), true),
            ("DP-1".to_owned(), rect(1920, 0, 1920, 1080), false),
        ]
    }

    #[test]
    fn automatic_mapping_takes_primary_explicit_takes_named() {
        let config = TabletConfig::default();
        assert_eq!(
            select_output(&outputs(), &config),
            Some(rect(0, 0, 1920, 1080))
        );
        let mut named = config.clone();
        named.output = ["x".to_owned(), "DP-1".to_owned(), String::new()];
        assert_eq!(
            select_output(&outputs(), &named),
            Some(rect(1920, 0, 1920, 1080))
        );
    }

    #[test]
    fn unmatched_output_falls_back_to_primary_never_nowhere() {
        let config = TabletConfig {
            output: ["nope".to_owned(), "nope".to_owned(), "nope".to_owned()],
            ..TabletConfig::default()
        };
        assert_eq!(
            select_output(&outputs(), &config),
            Some(rect(0, 0, 1920, 1080))
        );
        assert_eq!(select_output(&[], &config), None);
    }

    #[test]
    fn absolute_mapping_fills_target_by_default() {
        let config = TabletConfig::default();
        let target = rect(1920, 0, 1920, 1080);
        let corners = [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0), (0.5, 0.5)];
        let points: Vec<Point<f64, Logical>> = corners
            .into_iter()
            .map(|(x, y)| map_absolute(x, y, &config, (152.0, 95.0), target))
            .collect();
        assert_eq!(points[0], (1920.0, 0.0).into());
        assert_eq!(points[1], (3840.0, 0.0).into());
        assert_eq!(points[2], (1920.0, 1080.0).into());
        assert_eq!(points[3], (3840.0, 1080.0).into());
        assert_eq!(points[4], (2880.0, 540.0).into());
    }

    #[test]
    fn area_padding_insets_active_region_and_clamps_edges() {
        // 10% left, 20% top: the active area starts there.
        let mut config = TabletConfig {
            area_milli: [10_000, 0, 20_000, 0],
            ..TabletConfig::default()
        };
        let target = rect(0, 0, 1000, 1000);
        assert_eq!(
            map_absolute(0.1, 0.2, &config, (100.0, 100.0), target),
            (0.0, 0.0).into()
        );
        assert_eq!(
            map_absolute(1.0, 1.0, &config, (100.0, 100.0), target),
            (1000.0, 1000.0).into()
        );
        // Inside the dead padding clamps to the edge, not past it.
        assert_eq!(
            map_absolute(0.0, 0.0, &config, (100.0, 100.0), target),
            (0.0, 0.0).into()
        );
        // Padding that eats the area falls back to full-area mapping.
        config.area_milli = [60_000, 60_000, 0, 0];
        assert_eq!(
            map_absolute(0.25, 0.5, &config, (100.0, 100.0), target),
            (250.0, 500.0).into()
        );
    }

    #[test]
    fn keep_aspect_restricts_tablet_area_to_output_aspect() {
        let config = TabletConfig {
            keep_aspect: true,
            ..TabletConfig::default()
        };
        // Square tablet onto a 2:1 output: full width, restricted
        // height, so the middle of the tablet is the middle.
        let middle = map_absolute(0.5, 0.5, &config, (100.0, 100.0), rect(0, 0, 2000, 1000));
        assert_eq!(middle, (1000.0, 500.0).into());
        // Top and bottom quarters of the tablet clamp: only the
        // middle half is active.
        let top = map_absolute(0.5, 0.0, &config, (100.0, 100.0), rect(0, 0, 2000, 1000));
        assert_eq!(top, (1000.0, 0.0).into());
        let active_top = map_absolute(0.5, 0.25, &config, (100.0, 100.0), rect(0, 0, 2000, 1000));
        assert_eq!(active_top, (1000.0, 0.0).into());
        // Unknown tablet size cannot letterbox: falls back to fill.
        let fill = map_absolute(0.5, 0.0, &config, (0.0, 0.0), rect(0, 0, 2000, 1000));
        assert_eq!(fill, (1000.0, 0.0).into());
    }

    #[test]
    fn pressure_curve_pins_ends_and_hits_control_points() {
        assert_eq!(apply_pressure_curve(0.0, &[25, 10, 75, 90]), 0.0);
        assert_eq!(apply_pressure_curve(1.0, &[25, 10, 75, 90]), 1.0);
        assert!((apply_pressure_curve(0.25, &[25, 10, 75, 90]) - 0.10).abs() < 1e-9);
        assert!((apply_pressure_curve(0.75, &[25, 10, 75, 90]) - 0.90).abs() < 1e-9);
        // Linear default is identity; degenerate curves stay total.
        assert!((apply_pressure_curve(0.3, &[0, 0, 100, 100]) - 0.3).abs() < 1e-9);
        assert!((0.0..=1.0).contains(&apply_pressure_curve(0.5, &[90, 0, 10, 100])));
        assert!((0.0..=1.0).contains(&apply_pressure_curve(-1.0, &[0, 0, 100, 100])));
        assert!((0.0..=1.0).contains(&apply_pressure_curve(2.0, &[0, 0, 100, 100])));
    }

    #[test]
    fn eraser_uses_its_own_curve() {
        let config = TabletConfig {
            pressure_curve: [50, 0, 50, 100],
            eraser_pressure_curve: [0, 50, 100, 50],
            ..TabletConfig::default()
        };
        assert_eq!(config.curve_for(TabletToolType::Pen), [50, 0, 50, 100]);
        assert_eq!(config.curve_for(TabletToolType::Eraser), [0, 50, 100, 50]);
    }

    #[test]
    fn barrel_buttons_map_to_configured_evdev_codes() {
        let config = TabletConfig {
            button_actions: [
                StylusButtonAction::Default,
                StylusButtonAction::Middle,
                StylusButtonAction::Back,
            ],
            ..TabletConfig::default()
        };
        assert_eq!(
            stylus_action(BTN_STYLUS, &config),
            StylusButtonAction::Default
        );
        assert_eq!(
            stylus_action(BTN_STYLUS2, &config),
            StylusButtonAction::Middle
        );
        assert_eq!(stylus_action(0x149, &config), StylusButtonAction::Back);
        assert_eq!(action_button(StylusButtonAction::Default), BTN_RIGHT);
        assert_eq!(action_button(StylusButtonAction::Middle), BTN_MIDDLE);
        assert_eq!(action_button(StylusButtonAction::Right), BTN_RIGHT);
        assert_eq!(action_button(StylusButtonAction::Back), BTN_BACK);
        assert_eq!(action_button(StylusButtonAction::Forward), BTN_FORWARD);
    }

    #[test]
    fn tool_id_round_trips_through_the_copy_boundary() {
        let tool = TabletToolDescriptor {
            tool_type: TabletToolType::Pen,
            hardware_serial: 1234,
            hardware_id_wacom: 5678,
            capabilities: TabletToolCapabilities::PRESSURE | TabletToolCapabilities::TILT,
        };
        let id = TabletToolId::from_descriptor(&tool);
        assert_eq!(id.descriptor(), tool);
        // Unknown capability bits are dropped, never invented.
        let wide = TabletToolId {
            capabilities: u32::MAX,
            ..id
        };
        assert!(
            wide.descriptor().capabilities.bits() & !(TabletToolCapabilities::all().bits()) == 0
        );
    }

    #[test]
    fn relative_targets_clamp_into_the_output_union() {
        let clamped = clamp_to_outputs((5000.0, 5000.0).into(), &outputs());
        assert_eq!(clamped, (3839.0, 1079.0).into());
        let inside = clamp_to_outputs((100.0, 100.0).into(), &outputs());
        assert_eq!(inside, (100.0, 100.0).into());
    }
}
