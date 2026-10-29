//! libinput tablets on the hardware session (P-IN-05, #344).
//!
//! Mutter 51's native backend, `src/backends/native/meta-seat-impl.c`
//! (`process_device_event`, `process_tablet_axis`,
//! `input_device_update_tool`): tools come into proximity on a tablet,
//! their position is the tablet's normalized position mapped onto the
//! desktop (absolute) or their motion added to the last position
//! (relative, and always for mouse and lens tools), axes are sent before
//! a tip goes down and after it comes up, and pressure passes the tool's
//! curve. GNOME's per-device settings apply as each device and tool
//! appears, and again on every settings change
//! (`meta-input-settings.c` `apply_mappable_device_settings`,
//! `apply_stylus_settings`).
//!
//! libinput configuration newer than the pinned bindings (pressure range,
//! 1.26; eraser button, 1.29) is resolved from the running libinput at
//! run time. Mutter 51 requires libinput 1.31, so on GNOME's own systems
//! both exist; an older libinput leaves those two settings unapplied and
//! says so once per tool.
//!
//! Stylus and pad buttons, rings and strips that settings give an action
//! are taken here as Mutter's tablet action mappers take them
//! (`meta-tool-action-mapper.c`, `meta-pad-action-mapper.c`,
//! `meta-tablet-action-mapper.c`; policy in `crate::tablet_actions`), and
//! while the pad OSD is shown pad events are its, not a client's
//! (`events.c`'s `current_pad_osd`).

use std::collections::BTreeSet;
use std::ffi::CStr;

use smithay::backend::input::TabletToolDescriptor;
use smithay::reexports::input as li;
use smithay::reexports::input::event::tablet_pad::{
    RingAxisSource, StripAxisSource, TabletPadEvent, TabletPadEventTrait,
};
use smithay::reexports::input::event::tablet_tool::{
    ProximityState, TabletTool, TabletToolEvent, TabletToolEventTrait, TipState,
};
use smithay::reexports::input::event::{pointer::ButtonState, EventTrait};
use smithay::reexports::input::AsRaw;
use smithay::utils::{Logical, Point};
use smithay::wayland::tablet_manager::{TabletDescriptor, TabletPadDescriptor, TabletPadGroupDescriptor};
use tuna_shell_control::{PadOsd, PadOsdButton, PadOsdEvent, PadOsdLabels, PadOsdPad, PadOsdTarget};

use super::libwacom::{Libwacom, WacomInfo};
use crate::tablet::{
    map_tool_button, stylus_pressure, stylus_settings_path, tablet_settings_path, ButtonAction, DeviceMapping,
    EraserButtonMode, Mapping, Monitor, PressureCurve, StylusSettings, TabletEvent, TabletIdentity,
    TabletSettings, ToolAxes, ToolButton, BTN_STYLUS3,
};
use crate::tablet_actions::{
    cycle_monitor, feature_directions, feature_label, group_switch_osd, mode_switch_label, pad_button_label,
    pad_button_outcome, pad_button_settings_path, pad_feature_settings_path, DirectionTracker, PadButtonOutcome,
    PadDirection, PadFeature,
};
use crate::tablet_settings::{Kind, Snapshot};

/// `meta_display_show_osd`'s icon for tablets and pads.
const TABLET_OSD_ICON: &str = "input-tablet-symbolic";

/// The tool in proximity of one tablet.
struct CurrentTool {
    tool: TabletTool,
    desc: TabletToolDescriptor,
    path: String,
    eraser: bool,
    mouse_or_lens: bool,
    curve: PressureCurve,
    applied: Option<StylusSettings>,
    /// The libinput config this tool's libinput lacked, reported once.
    unsupported_reported: bool,
}

struct TabletDevice {
    device: li::Device,
    desc: TabletDescriptor,
    identity: TabletIdentity,
    vendor: u32,
    product: u32,
    path: String,
    mapping: DeviceMapping,
    mapped_to: Option<String>,
    applied: Option<TabletSettings>,
    tool: Option<CurrentTool>,
    /// Last position, where relative motion continues from.
    pos: Point<f64, Logical>,
    wacom: Option<WacomInfo>,
}

struct PadDevice {
    device: li::Device,
    desc: TabletPadDescriptor,
    /// Stable id the shell's pad OSD names it by.
    id: u32,
    vendor: u32,
    product: u32,
    devnode: String,
    wacom: Option<WacomInfo>,
    /// `PadMappingInfo::group_modes`: each group's mode as the action
    /// mapper last saw it switch.
    group_modes: Vec<u32>,
}

impl PadDevice {
    /// `clutter_input_device_get_mode_switch_button_group`.
    fn mode_switch_group(&self, button: u32) -> Option<usize> {
        self.desc
            .groups
            .iter()
            .position(|group| group.mode_switch_buttons.contains(&button))
    }

    /// libwacom's name, else the device's (`pretty_name` fallbacks).
    fn pretty_name(&self) -> &str {
        self.wacom.as_ref().map_or(self.desc.name.as_str(), |w| w.name.as_str())
    }

    fn button_settings_path(&self, button: u32) -> String {
        pad_button_settings_path(self.vendor, self.product, button)
    }

    /// Every feature path a mode of its group may read.
    fn feature_paths(&self) -> Vec<String> {
        let mut paths = Vec::new();
        for group in &self.desc.groups {
            for (feature, numbers) in [(PadFeature::Ring, &group.rings), (PadFeature::Strip, &group.strips)] {
                for number in numbers {
                    for mode in 0..group.modes.max(1) {
                        for direction in feature_directions(feature) {
                            paths.push(pad_feature_settings_path(
                                self.vendor,
                                self.product,
                                feature,
                                *number,
                                direction,
                                mode,
                            ));
                        }
                    }
                }
            }
        }
        paths
    }

    /// `get_current_pad_mode`: features are assumed spread evenly over
    /// the groups.
    fn feature_mode(&self, number: u32) -> u32 {
        let groups = self.group_modes.len();
        if groups == 0 {
            return 0;
        }
        self.group_modes[number as usize % groups]
    }
}

/// Every connected tablet and pad on the hardware seat.
#[derive(Default)]
pub struct Tablets {
    tablets: Vec<TabletDevice>,
    pads: Vec<PadDevice>,
    settings: Snapshot,
    monitors: Vec<Monitor>,
    events: Vec<TabletEvent>,
    next_pad: u32,
    /// The pad action mapper's `last_pad_action_info`.
    directions: DirectionTracker,
    /// The pad whose OSD the shell shows (`display->current_pad_osd`).
    osd: Option<u32>,
    /// libwacom, opened on the first tablet device (`None` inside: absent).
    libwacom: Option<Option<Libwacom>>,
}

fn vendor_product(device: &li::Device) -> (u32, u32) {
    (device.id_vendor(), device.id_product())
}

fn pad_descriptor(device: &li::Device) -> TabletPadDescriptor {
    let buttons = device.tablet_pad_number_of_buttons().max(0) as u32;
    let rings = device.tablet_pad_number_of_rings().max(0) as u32;
    let strips = device.tablet_pad_number_of_strips().max(0) as u32;
    let groups = (0..device.tablet_pad_number_of_mode_groups().max(0) as u32)
        .filter_map(|index| device.tablet_pad_mode_group(index))
        .map(|group| TabletPadGroupDescriptor {
            buttons: (0..buttons).filter(|b| group.has_button(*b)).collect(),
            mode_switch_buttons: (0..buttons)
                .filter(|b| group.has_button(*b) && group.button_is_toggle(*b))
                .collect(),
            rings: (0..rings).filter(|r| group.has_ring(*r)).collect(),
            strips: (0..strips).filter(|s| group.has_strip(*s)).collect(),
            modes: group.number_of_modes(),
        })
        .collect();
    TabletPadDescriptor {
        name: device.name().to_owned(),
        syspath: smithay::backend::input::Device::syspath(device),
        buttons,
        groups,
    }
}

/// The evdev node libwacom and GNOME Settings name a device by.
fn devnode(device: &li::Device) -> String {
    format!("/dev/input/{}", device.sysname())
}

/// A libinput function newer than the pinned bindings, from the libinput
/// this process runs against.
fn libinput_symbol(name: &CStr) -> Option<*mut libc::c_void> {
    // SAFETY: dlsym with RTLD_DEFAULT and a NUL-terminated name only looks
    // the symbol up in the already loaded objects.
    let symbol = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) };
    (!symbol.is_null()).then_some(symbol)
}

type PressureRangeSet = unsafe extern "C" fn(*mut li::ffi::libinput_tablet_tool, f64, f64) -> u32;
type PressureRangeAvailable = unsafe extern "C" fn(*mut li::ffi::libinput_tablet_tool) -> i32;
type EraserGetModes = unsafe extern "C" fn(*mut li::ffi::libinput_tablet_tool) -> u32;
type EraserSetMode = unsafe extern "C" fn(*mut li::ffi::libinput_tablet_tool, u32) -> u32;
type EraserSetButton = unsafe extern "C" fn(*mut li::ffi::libinput_tablet_tool, u32) -> u32;

/// `libinput_tablet_tool_config_pressure_range_set` (libinput 1.26);
/// `None` when this libinput has no such call.
fn set_pressure_range(tool: &TabletTool, range: [f64; 2]) -> Option<bool> {
    let available = libinput_symbol(c"libinput_tablet_tool_config_pressure_range_is_available")?;
    let set = libinput_symbol(c"libinput_tablet_tool_config_pressure_range_set")?;
    // SAFETY: the symbols are libinput's own, with these C signatures
    // (libinput.h since 1.26); the tool pointer is live for the call.
    unsafe {
        let available: PressureRangeAvailable = std::mem::transmute(available);
        let set: PressureRangeSet = std::mem::transmute(set);
        let raw = tool.as_raw_mut();
        if available(raw) == 0 {
            return Some(false);
        }
        Some(set(raw, range[0], range[1]) == 0)
    }
}

/// Whether a tool has a firmware eraser button libinput can turn into a
/// button (`meta_input_device_tool_native_new`); `None` without the call.
fn has_eraser_button(tool: &TabletTool) -> Option<bool> {
    let get_modes = libinput_symbol(c"libinput_tablet_tool_config_eraser_button_get_modes")?;
    // SAFETY: libinput.h since 1.29; the tool pointer is live for the call.
    unsafe {
        let get_modes: EraserGetModes = std::mem::transmute(get_modes);
        // LIBINPUT_CONFIG_ERASER_BUTTON_BUTTON
        Some(get_modes(tool.as_raw_mut()) & 1 != 0)
    }
}

/// `meta_input_device_tool_native_set_eraser_button_action_in_impl`.
fn set_eraser_button(tool: &TabletTool, mode: EraserButtonMode) -> Option<()> {
    let set_mode = libinput_symbol(c"libinput_tablet_tool_config_eraser_button_set_mode")?;
    let set_button = libinput_symbol(c"libinput_tablet_tool_config_eraser_button_set_button")?;
    // SAFETY: libinput.h since 1.29; the tool pointer is live for the call.
    unsafe {
        let set_mode: EraserSetMode = std::mem::transmute(set_mode);
        let set_button: EraserSetButton = std::mem::transmute(set_button);
        let raw = tool.as_raw_mut();
        set_mode(
            raw,
            match mode {
                EraserButtonMode::Default => 0,
                EraserButtonMode::Button => 1,
            },
        );
        set_button(raw, BTN_STYLUS3);
    }
    Some(())
}

impl CurrentTool {
    /// `apply_stylus_settings`: the pressure curve and range for the
    /// tool's kind, and its eraser button.
    fn apply(&mut self, settings: &StylusSettings, device_name: &str) {
        if self.applied.as_ref() == Some(settings) {
            return;
        }
        if let Some((curve, range)) = stylus_pressure(settings, self.eraser) {
            self.curve = PressureCurve::new(curve);
            let ranged = set_pressure_range(&self.tool, range);
            let eraser = match has_eraser_button(&self.tool) {
                Some(true) => set_eraser_button(&self.tool, settings.eraser_button_mode),
                Some(false) => Some(()),
                None => None,
            };
            if (ranged.is_none() || eraser.is_none()) && !self.unsupported_reported {
                self.unsupported_reported = true;
                eprintln!(
                    "tuna-compositor: tablet: {device_name}: this libinput has no {}; left at the device default",
                    match (ranged.is_none(), eraser.is_none()) {
                        (true, true) => "pressure range or eraser button configuration",
                        (true, false) => "pressure range configuration",
                        _ => "eraser button configuration",
                    }
                );
            }
        }
        eprintln!(
            "tuna-compositor: tablet: {device_name} {} settings {} curve={:?} range={:?} buttons={:?}/{:?}/{:?}",
            if self.eraser { "eraser" } else { "stylus" },
            self.path,
            if self.eraser { &settings.eraser_pressure_curve } else { &settings.pressure_curve },
            if self.eraser { &settings.eraser_pressure_range } else { &settings.pressure_range },
            settings.button_action,
            settings.secondary_button_action,
            settings.tertiary_button_action,
        );
        self.applied = Some(settings.clone());
    }

    /// `translate_tablet_axes`: what the tool has, pressure through its curve.
    fn axes<E: TabletToolEventTrait>(&self, event: &E) -> ToolAxes {
        let tool = &self.tool;
        ToolAxes {
            pressure: tool.has_pressure().then(|| self.curve.translate(event.pressure())),
            distance: tool.has_distance().then(|| event.distance()),
            tilt: tool.has_tilt().then(|| (event.tilt_x(), event.tilt_y())),
            rotation: tool.has_rotation().then(|| event.rotation()),
            slider: tool.has_slider().then(|| event.slider_position()),
            wheel: (tool.has_wheel() && event.wheel_has_changed())
                .then(|| (event.wheel_delta(), event.wheel_delta_discrete() as i32)),
        }
    }
}

impl Tablets {
    /// libwacom's entry for a device node, opening libwacom once.
    fn wacom(&mut self, device: &li::Device) -> Option<WacomInfo> {
        self.libwacom
            .get_or_insert_with(Libwacom::open)
            .as_ref()?
            .lookup(&devnode(device))
    }

    /// A libinput device arrived: tablets and pads join, and their
    /// settings apply.
    pub fn device_added(&mut self, device: &li::Device) {
        if device.has_capability(li::DeviceCapability::TabletTool) {
            let (vendor, product) = vendor_product(device);
            let desc = TabletDescriptor::from(device);
            let mut tablet = TabletDevice {
                device: device.clone(),
                identity: TabletIdentity {
                    name: device.name().to_owned(),
                    size_mm: device.size(),
                },
                path: tablet_settings_path(vendor, product),
                vendor,
                product,
                mapping: DeviceMapping::new(
                    &TabletIdentity {
                        name: device.name().to_owned(),
                        size_mm: device.size(),
                    },
                    &TabletSettings::default(),
                    &self.monitors,
                )
                .0,
                mapped_to: None,
                applied: None,
                tool: None,
                pos: (0.0, 0.0).into(),
                desc: desc.clone(),
                wacom: self.wacom(device),
            };
            eprintln!(
                "tuna-compositor: tablet: added {} ({vendor:04x}:{product:04x}) size={:?} libwacom={:?}",
                tablet.identity.name, tablet.identity.size_mm, tablet.wacom
            );
            Self::apply_tablet(&mut tablet, &self.settings, &self.monitors);
            self.tablets.push(tablet);
            self.events.push(TabletEvent::TabletAdded(desc.clone()));
            // A pad in the same device group pairs with it now.
            let group = device.device_group();
            for pad in &self.pads {
                if pad.device.device_group() == group {
                    eprintln!(
                        "tuna-compositor: tablet: pad {} paired={}",
                        pad.desc.name, desc.name
                    );
                    self.events.push(TabletEvent::PadAdded {
                        pad: pad.desc.clone(),
                        tablet: Some(desc.clone()),
                    });
                }
            }
        }
        if device.has_capability(li::DeviceCapability::TabletPad) {
            let desc = pad_descriptor(device);
            let group = device.device_group();
            let tablet = self
                .tablets
                .iter()
                .find(|t| t.device.device_group() == group)
                .map(|t| t.desc.clone());
            eprintln!(
                "tuna-compositor: tablet: pad {} buttons={} groups={} paired={}",
                desc.name,
                desc.buttons,
                desc.groups.len(),
                tablet.as_ref().map_or("none", |t| t.name.as_str())
            );
            // Left-handed turns pads too (update_tablet_left_handed).
            if let Some(settings) = self.settings.tablets.get(&tablet_settings_path(
                device.id_vendor(),
                device.id_product(),
            )) {
                let _ = device.clone().config_left_handed_set(settings.left_handed);
            }
            let (vendor, product) = vendor_product(device);
            let wacom = self.wacom(device);
            self.next_pad += 1;
            self.pads.push(PadDevice {
                device: device.clone(),
                desc: desc.clone(),
                id: self.next_pad,
                vendor,
                product,
                devnode: devnode(device),
                wacom,
                group_modes: vec![0; desc.groups.len()],
            });
            self.events.push(TabletEvent::PadAdded { pad: desc, tablet });
        }
    }

    pub fn device_removed(&mut self, device: &li::Device) {
        if let Some(index) = self.tablets.iter().position(|t| t.device == *device) {
            let tablet = self.tablets.remove(index);
            if let Some(tool) = tablet.tool {
                self.events.push(TabletEvent::ProximityOut {
                    tablet: tablet.desc.clone(),
                    tool: tool.desc,
                    time: 0,
                });
            }
            eprintln!("tuna-compositor: tablet: removed {}", tablet.identity.name);
            self.events.push(TabletEvent::TabletRemoved(tablet.desc));
        }
        if let Some(index) = self.pads.iter().position(|p| p.device == *device) {
            let pad = self.pads.remove(index);
            // gnome-shell's PadOsd closes when its pad is removed.
            if self.osd == Some(pad.id) {
                self.osd = None;
                self.events.push(TabletEvent::ClosePadOsd { pad: pad.id });
            }
            self.events.push(TabletEvent::PadRemoved(pad.desc));
        }
    }

    /// The settings paths the connected tablets and the tools in
    /// proximity read, and every pad action path.
    pub(crate) fn wanted(&self) -> BTreeSet<(Kind, String)> {
        let mut wanted = BTreeSet::new();
        for tablet in &self.tablets {
            wanted.insert((Kind::Tablet, tablet.path.clone()));
            if let Some(tool) = &tablet.tool {
                wanted.insert((Kind::Stylus, tool.path.clone()));
            }
        }
        for pad in &self.pads {
            wanted.insert((Kind::Tablet, tablet_settings_path(pad.vendor, pad.product)));
            for button in 0..pad.desc.buttons {
                wanted.insert((Kind::PadButton, pad.button_settings_path(button)));
            }
            for path in pad.feature_paths() {
                wanted.insert((Kind::PadButton, path));
            }
        }
        wanted
    }

    /// `apply_mappable_device_settings` for one tablet: mapping, area,
    /// keep-aspect, left-handed.
    fn apply_tablet(tablet: &mut TabletDevice, snapshot: &Snapshot, monitors: &[Monitor]) {
        let settings = snapshot.tablets.get(&tablet.path).cloned().unwrap_or_default();
        let (mapping, picked) = DeviceMapping::new(&tablet.identity, &settings, monitors);
        let mapped_to = picked.map(|index| monitors[index].connector.clone());
        let changed = tablet.applied.as_ref() != Some(&settings)
            || tablet.mapping != mapping
            || tablet.mapped_to != mapped_to;
        tablet.mapping = mapping;
        tablet.mapped_to = mapped_to;
        if !changed {
            return;
        }
        let device = &mut tablet.device;
        if let Some(area) = settings.area() {
            if device.config_calibration_has_matrix() {
                // libinput reports no default matrix when it is the
                // identity; restoring the default then means the identity.
                let matrix = crate::tablet::area_matrix(area)
                    .or_else(|| device.config_calibration_default_matrix())
                    .unwrap_or([1.0, 0.0, 0.0, 0.0, 1.0, 0.0]);
                let _ = device.config_calibration_set_matrix(matrix);
            }
        }
        if device.config_left_handed_is_available() {
            let _ = device.config_left_handed_set(settings.left_handed);
        }
        eprintln!(
            "tuna-compositor: tablet: {} settings {} mapping={:?} output={} keep_aspect={} area={:?} left_handed={}",
            tablet.identity.name,
            tablet.path,
            settings.mapping,
            tablet.mapped_to.as_deref().unwrap_or("desktop"),
            settings.keep_aspect,
            settings.area,
            settings.left_handed,
        );
        tablet.applied = Some(settings);
    }

    /// A completed settings read: apply what changed.
    pub(crate) fn set_settings(&mut self, snapshot: Snapshot) {
        self.settings = snapshot;
        self.reapply();
    }

    /// The monitors changed: remap every tablet.
    pub fn set_monitors(&mut self, monitors: Vec<Monitor>) {
        if self.monitors == monitors {
            return;
        }
        self.monitors = monitors;
        self.reapply();
    }

    fn reapply(&mut self) {
        for tablet in &mut self.tablets {
            Self::apply_tablet(tablet, &self.settings, &self.monitors);
            let name = tablet.identity.name.clone();
            if let Some(tool) = &mut tablet.tool {
                let settings = self.settings.styluses.get(&tool.path).cloned().unwrap_or_default();
                tool.apply(&settings, &name);
            }
        }
        for pad in &self.pads {
            let path = tablet_settings_path(pad.device.id_vendor(), pad.device.id_product());
            if let Some(settings) = self.settings.tablets.get(&path) {
                if pad.device.config_left_handed_is_available() {
                    let _ = pad.device.clone().config_left_handed_set(settings.left_handed);
                }
            }
        }
    }

    pub fn take_events(&mut self) -> Vec<TabletEvent> {
        std::mem::take(&mut self.events)
    }

    /// Where a tool event puts the tool: mapped onto the desktop, or moved
    /// by its motion (relative mapping and mouse or lens tools).
    fn position<E: TabletToolEventTrait>(
        tablet: &mut TabletDevice,
        event: &E,
        relative: bool,
        monitors: &[Monitor],
    ) -> Point<f64, Logical> {
        let stage = crate::tablet::stage_size(monitors);
        if stage.0 <= 0.0 || stage.1 <= 0.0 {
            return tablet.pos;
        }
        let pos = if relative {
            let rects: Vec<_> = monitors.iter().map(|m| m.rect).collect();
            super::clamp_to_outputs(
                (tablet.pos.x + event.dx(), tablet.pos.y + event.dy()).into(),
                &rects,
            )
        } else {
            let norm = (
                event.x_transformed(stage.0 as u32) / stage.0,
                event.y_transformed(stage.1 as u32) / stage.1,
            );
            tablet.mapping.translate(norm, stage)
        };
        tablet.pos = pos;
        pos
    }

    /// One libinput tablet tool event.
    pub fn tool_event(&mut self, event: TabletToolEvent, pointer: Point<f64, Logical>) {
        let device = EventTrait::device(&event);
        let Some(tablet) = self.tablets.iter_mut().find(|t| t.device == device) else {
            return;
        };
        let monitors = &self.monitors;
        let time = TabletToolEventTrait::time(&event);
        let mut cycle = None;
        match event {
            TabletToolEvent::Proximity(event) => {
                let tool = TabletToolEventTrait::tool(&event);
                let desc = smithay::backend::input::TabletToolEvent::tool(&event);
                if event.proximity_state() == ProximityState::Out {
                    if let Some(current) = tablet.tool.take() {
                        self.events.push(TabletEvent::ProximityOut {
                            tablet: tablet.desc.clone(),
                            tool: current.desc,
                            time,
                        });
                    }
                    return;
                }
                let kind = tool.tool_type();
                let eraser = kind == Some(li::event::tablet_tool::TabletToolType::Eraser);
                let mouse_or_lens = matches!(
                    kind,
                    Some(li::event::tablet_tool::TabletToolType::Mouse)
                        | Some(li::event::tablet_tool::TabletToolType::Lens)
                );
                let path = stylus_settings_path(tool.serial(), tablet.vendor, tablet.product);
                let mut current = CurrentTool {
                    tool,
                    desc: desc.clone(),
                    path,
                    eraser,
                    mouse_or_lens,
                    curve: PressureCurve::default(),
                    applied: None,
                    unsupported_reported: false,
                };
                // Applied at once from the last read; a first tool's own
                // path reads in the background and applies when it lands.
                let settings = self.settings.styluses.get(&current.path).cloned().unwrap_or_default();
                current.apply(&settings, &tablet.identity.name);
                eprintln!(
                    "tuna-compositor: tablet: {} tool {:?} serial={:x} in proximity",
                    tablet.identity.name, desc.tool_type, desc.hardware_serial
                );
                if tablet.tool.is_none() && (mouse_or_lens || tablet.mapping.mode == Mapping::Relative) {
                    // Relative motion starts where the pointer is.
                    tablet.pos = pointer;
                }
                let relative = mouse_or_lens || tablet.mapping.mode == Mapping::Relative;
                // Mutter moves nothing on proximity (process_device_event
                // only notifies it): libinput's delta on entering is the jump
                // from where the tool left, which relative mode must drop.
                let pos = if relative {
                    tablet.pos
                } else {
                    Self::position(tablet, &event, relative, monitors)
                };
                let axes = current.axes(&event);
                tablet.tool = Some(current);
                self.events.push(TabletEvent::ProximityIn {
                    tablet: tablet.desc.clone(),
                    tool: desc,
                    pos,
                    axes,
                    time,
                });
            }
            TabletToolEvent::Axis(event) => {
                let Some(current) = &tablet.tool else {
                    return;
                };
                let relative = current.mouse_or_lens || tablet.mapping.mode == Mapping::Relative;
                let (desc, axes) = (current.desc.clone(), current.axes(&event));
                let pos = Self::position(tablet, &event, relative, monitors);
                self.events.push(TabletEvent::Motion {
                    tablet: tablet.desc.clone(),
                    tool: desc,
                    pos,
                    axes,
                    time,
                });
            }
            TabletToolEvent::Tip(event) => {
                let Some(current) = &tablet.tool else {
                    return;
                };
                let relative = current.mouse_or_lens || tablet.mapping.mode == Mapping::Relative;
                let (desc, axes) = (current.desc.clone(), current.axes(&event));
                let down = event.tip_state() == TipState::Down;
                // Axes before the tip goes down, after it comes up.
                let pos = Self::position(tablet, &event, relative, monitors);
                let motion = TabletEvent::Motion {
                    tablet: tablet.desc.clone(),
                    tool: desc.clone(),
                    pos,
                    axes,
                    time,
                };
                let tip = TabletEvent::Tip {
                    tablet: tablet.desc.clone(),
                    tool: desc,
                    down,
                    pos,
                    time,
                };
                if down {
                    self.events.extend([motion, tip]);
                } else {
                    self.events.extend([tip, motion]);
                }
            }
            TabletToolEvent::Button(event) => {
                let Some(current) = &tablet.tool else {
                    return;
                };
                let settings = current.applied.clone().unwrap_or_default();
                let eraser_button = has_eraser_button(&current.tool)
                    .unwrap_or(false)
                    .then_some(BTN_STYLUS3);
                let pressed = event.button_state() == ButtonState::Pressed;
                let desc = current.desc.clone();
                match map_tool_button(event.button(), &settings, eraser_button, current.mouse_or_lens) {
                    ToolButton::Tip => self.events.push(TabletEvent::Tip {
                        tablet: tablet.desc.clone(),
                        tool: desc,
                        down: pressed,
                        pos: tablet.pos,
                        time,
                    }),
                    ToolButton::Button(button) => self.events.push(TabletEvent::Button {
                        tablet: tablet.desc.clone(),
                        tool: desc,
                        button,
                        pressed,
                        time,
                    }),
                    ToolButton::Action(action, accel) => {
                        // meta_tool_action_mapper_handle_button. Mouse and
                        // lens tools follow the mouse settings there and
                        // reach it with Clutter button 0, which events.c
                        // drops: nothing happens.
                        if !current.mouse_or_lens {
                            match action {
                                ButtonAction::SwitchMonitor if pressed => {
                                    cycle = Some((
                                        tablet.vendor,
                                        tablet.product,
                                        tablet
                                            .wacom
                                            .as_ref()
                                            .map_or_else(|| tablet.identity.name.clone(), |w| w.name.clone()),
                                        tablet.wacom.as_ref().is_some_and(|w| w.integrated),
                                    ));
                                }
                                ButtonAction::Keybinding => self.events.push(TabletEvent::EmulateKeybinding {
                                    accel,
                                    pressed,
                                    time,
                                }),
                                _ => {}
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        if let Some((vendor, product, name, integrated)) = cycle {
            self.cycle_output(vendor, product, &name, integrated);
        }
    }

    /// `meta_tablet_action_mapper_cycle_tablet_output`: map the device to
    /// the monitor after the one its `output` names (from the span of all
    /// monitors to the first; integrated tablets never take the span), and
    /// say so on screen.
    fn cycle_output(&mut self, vendor: u32, product: u32, name: &str, integrated: bool) {
        let path = tablet_settings_path(vendor, product);
        let settings = self.settings.tablets.get(&path).cloned().unwrap_or_default();
        // meta_tablet_action_mapper_find_monitor: exactly three values,
        // not all empty, naming a monitor's EDID.
        let current = settings
            .output
            .as_ref()
            .filter(|o| o.len() == 3 && o.iter().any(|v| !v.is_empty()))
            .and_then(|o| {
                self.monitors
                    .iter()
                    .position(|m| m.vendor == o[0] && m.product == o[1] && m.serial == o[2])
            });
        let Some(next) = cycle_monitor(current, self.monitors.len(), integrated) else {
            return;
        };
        let output = next.map_or_else(Default::default, |index| {
            let monitor = &self.monitors[index];
            [monitor.vendor.clone(), monitor.product.clone(), monitor.serial.clone()]
        });
        let connector = next.map(|index| self.monitors[index].connector.clone());
        eprintln!(
            "tuna-compositor: tablet: {name} switch-monitor to {}",
            connector.as_deref().unwrap_or("all monitors")
        );
        self.events.push(TabletEvent::SetOutput { path, output });
        // meta_display_show_tablet_mapping_notification.
        self.events.push(TabletEvent::ShowOsd {
            connector,
            icon: TABLET_OSD_ICON,
            label: name.to_owned(),
        });
    }

    /// The monitor a pad's tablet is mapped to (`lookup_tablet_monitor`).
    fn pad_connector(&self, index: usize) -> Option<String> {
        let group = self.pads[index].device.device_group();
        self.tablets
            .iter()
            .find(|t| t.device.device_group() == group)
            .and_then(|t| t.mapped_to.clone())
    }

    /// `meta_pad_action_mapper_handle_button`; `true` when taken.
    fn pad_button_action(&mut self, index: usize, button: u32, pressed: bool, mode: u32, time: u32) -> bool {
        if let Some(group) = self.pads[index].mode_switch_group(button) {
            let n_modes = self.pads[index].desc.groups[group].modes;
            if pressed && n_modes > 0 {
                let label = group_switch_osd(self.pads[index].pretty_name(), mode, n_modes);
                let connector = self.pad_connector(index);
                self.events.push(TabletEvent::ShowOsd {
                    connector,
                    icon: TABLET_OSD_ICON,
                    label,
                });
                self.pads[index].group_modes[group] = mode;
            }
        }
        let pad = &self.pads[index];
        let settings = self
            .settings
            .pad_buttons
            .get(&pad.button_settings_path(button))
            .cloned()
            .unwrap_or_default();
        match pad_button_outcome(&settings) {
            PadButtonOutcome::Deliver => false,
            PadButtonOutcome::SwitchMonitor => {
                if pressed {
                    let (vendor, product) = (pad.vendor, pad.product);
                    let name = pad.pretty_name().to_owned();
                    let integrated = pad.wacom.as_ref().is_some_and(|w| w.integrated);
                    self.cycle_output(vendor, product, &name, integrated);
                }
                true
            }
            PadButtonOutcome::Help => {
                if pressed {
                    self.events.push(TabletEvent::RequestPadOsd { pad: pad.id, time });
                }
                true
            }
            PadButtonOutcome::Keybinding(accel) => {
                self.events.push(TabletEvent::EmulateKeybinding { accel, pressed, time });
                true
            }
        }
    }

    /// `meta_pad_action_mapper_handle_action`: a ring or strip with a
    /// keybinding in either direction is taken; moving it types the one
    /// for the direction it moved. `true` when taken.
    fn pad_feature_action(
        &mut self,
        index: usize,
        feature: PadFeature,
        number: u32,
        mode: u32,
        value: f64,
        time: u32,
    ) -> bool {
        let pad = &self.pads[index];
        let accel = |direction| {
            self.settings
                .pad_buttons
                .get(&pad_feature_settings_path(
                    pad.vendor,
                    pad.product,
                    feature,
                    number,
                    direction,
                    mode,
                ))
                .map(|s| s.keybinding.clone())
                .unwrap_or_default()
        };
        let [first, second] = feature_directions(feature);
        let (accel1, accel2) = (accel(first), accel(second));
        let handled = !accel1.is_empty() || !accel2.is_empty();
        let id = pad.id;
        if let Some(direction) = self.directions.direction(id, feature, number, value) {
            let accel = match direction {
                PadDirection::Up | PadDirection::Cw => accel1,
                PadDirection::Down | PadDirection::Ccw => accel2,
            };
            if !accel.is_empty() {
                for pressed in [true, false] {
                    self.events.push(TabletEvent::EmulateKeybinding {
                        accel: accel.clone(),
                        pressed,
                        time,
                    });
                }
            }
        }
        handled
    }

    /// One libinput pad event (events.c's pad branch): the action mapper
    /// first (only mode switches while the pad OSD is shown), then the
    /// OSD or the focused client.
    pub fn pad_event(&mut self, event: TabletPadEvent) {
        let device = EventTrait::device(&event);
        let Some(index) = self.pads.iter().position(|p| p.device == device) else {
            return;
        };
        let time = event.time();
        let osd_open = self.osd.is_some();
        let id = self.pads[index].id;
        match event {
            TabletPadEvent::Button(event) => {
                let button = event.button_number();
                let pressed = event.button_state() == ButtonState::Pressed;
                let mode = event.mode();
                let switch_group = self.pads[index].mode_switch_group(button);
                // Clients learn a mode switch whatever happens to the
                // button next (meta_wayland_compositor_update runs first).
                if let Some(group) = switch_group {
                    self.events.push(TabletEvent::PadButton {
                        pad: self.pads[index].desc.clone(),
                        button,
                        pressed,
                        mode: Some((group, mode)),
                        time,
                    });
                }
                let taken = (!osd_open || switch_group.is_some())
                    && self.pad_button_action(index, button, pressed, mode, time);
                if taken {
                    return;
                }
                if osd_open {
                    self.events.push(TabletEvent::PadOsdInput {
                        pad: id,
                        event: PadOsdEvent::Button { button, pressed },
                        time,
                    });
                } else if switch_group.is_none() {
                    self.events.push(TabletEvent::PadButton {
                        pad: self.pads[index].desc.clone(),
                        button,
                        pressed,
                        mode: None,
                        time,
                    });
                }
            }
            TabletPadEvent::Ring(event) => {
                let (ring, mode, angle) = (event.number(), event.mode(), event.position());
                if osd_open {
                    self.events.push(TabletEvent::PadOsdInput {
                        pad: id,
                        event: PadOsdEvent::Ring { ring, mode },
                        time,
                    });
                } else if !self.pad_feature_action(index, PadFeature::Ring, ring, mode, angle, time) {
                    self.events.push(TabletEvent::PadRing {
                        pad: self.pads[index].desc.clone(),
                        ring,
                        angle: (angle >= 0.0).then_some(angle),
                        finger: event.source() == RingAxisSource::Finger,
                        time,
                    });
                }
            }
            TabletPadEvent::Strip(event) => {
                let (strip, mode, position) = (event.number(), event.mode(), event.position());
                if osd_open {
                    self.events.push(TabletEvent::PadOsdInput {
                        pad: id,
                        event: PadOsdEvent::Strip { strip, mode },
                        time,
                    });
                } else if !self.pad_feature_action(index, PadFeature::Strip, strip, mode, position, time) {
                    self.events.push(TabletEvent::PadStrip {
                        pad: self.pads[index].desc.clone(),
                        strip,
                        position: (position >= 0.0).then_some(position),
                        finger: event.source() == StripAxisSource::Finger,
                        time,
                    });
                }
            }
            _ => {}
        }
    }

    /// Every pad the shell may ask an OSD for: id and device node.
    pub fn pad_targets(&self) -> Vec<(u32, String)> {
        self.pads.iter().map(|p| (p.id, p.devnode.clone())).collect()
    }

    /// The pad a shell request names.
    pub fn resolve_pad(&self, target: &PadOsdTarget) -> Option<u32> {
        self.pads
            .iter()
            .find(|p| match target {
                PadOsdTarget::Pad(id) => p.id == *id,
                PadOsdTarget::DeviceNode(node) => p.devnode == *node,
            })
            .map(|p| p.id)
    }

    /// The pad whose OSD is shown.
    pub fn pad_osd_shown(&self) -> Option<u32> {
        self.osd
    }

    /// The shell showed (`Some`) or closed (`None`) the pad OSD.
    pub fn set_pad_osd_shown(&mut self, pad: Option<u32>) {
        self.osd = pad;
    }

    /// `meta_display_request_pad_osd`: what `show-pad-osd` carries, or
    /// `None` when Mutter shows nothing (an OSD is up, the pad is gone,
    /// or libwacom has no layout for it). Labels are the action mapper's;
    /// the caller adds clients' feedback.
    pub fn pad_osd(&self, pad: u32, edition_mode: bool) -> Option<(TabletPadDescriptor, PadOsd)> {
        if self.osd.is_some() {
            return None;
        }
        let index = self.pads.iter().position(|p| p.id == pad)?;
        let device = &self.pads[index];
        let layout_path = device.wacom.as_ref()?.layout.clone()?;
        let settings_path = tablet_settings_path(device.vendor, device.product);
        let left_handed = self
            .settings
            .tablets
            .get(&settings_path)
            .is_some_and(|s| s.left_handed);
        let group = device.device.device_group();
        let group_pads = self
            .pads
            .iter()
            .filter(|p| p.id == pad || p.device.device_group() == group)
            .map(|p| PadOsdPad {
                pad: p.id,
                name: p.desc.name.clone(),
            })
            .collect();
        let (desc, labels) = self.pad_osd_labels(pad)?;
        Some((
            desc,
            PadOsd {
                pad,
                name: device.desc.name.clone(),
                layout_path,
                settings_path,
                left_handed,
                edition_mode,
                connector: self.pad_connector(index),
                group_pads,
                labels,
            },
        ))
    }

    /// The action mapper's labels for a pad in its current modes
    /// (`meta_pad_action_mapper_get_button_label`, `get_feature_label`).
    pub fn pad_osd_labels(&self, pad: u32) -> Option<(TabletPadDescriptor, PadOsdLabels)> {
        let device = self.pads.iter().find(|p| p.id == pad)?;
        let label = |path: String| self.settings.pad_buttons.get(&path).cloned().unwrap_or_default();
        let buttons = (0..device.desc.buttons)
            .map(|button| match device.mode_switch_group(button) {
                Some(group) => PadOsdButton {
                    label: Some(mode_switch_label(group as u32, device.desc.groups.len())),
                    mode_switch_group: Some(group as u32),
                },
                None => PadOsdButton {
                    label: pad_button_label(&label(device.button_settings_path(button))),
                    mode_switch_group: None,
                },
            })
            .collect();
        let count = |feature: PadFeature| {
            device
                .desc
                .groups
                .iter()
                .flat_map(|g| match feature {
                    PadFeature::Ring => g.rings.clone(),
                    PadFeature::Strip => g.strips.clone(),
                })
                .max()
                .map_or(0, |n| n + 1)
        };
        let features = |feature: PadFeature| -> (Vec<[Option<String>; 2]>, Vec<u32>) {
            (0..count(feature))
                .map(|number| {
                    let mode = device.feature_mode(number);
                    let [first, second] = feature_directions(feature).map(|direction| {
                        feature_label(Some(&label(pad_feature_settings_path(
                            device.vendor,
                            device.product,
                            feature,
                            number,
                            direction,
                            mode,
                        ))))
                    });
                    ([first, second], mode)
                })
                .unzip()
        };
        let (rings, ring_modes) = features(PadFeature::Ring);
        let (strips, strip_modes) = features(PadFeature::Strip);
        Some((
            device.desc.clone(),
            PadOsdLabels {
                buttons,
                rings,
                strips,
                ring_modes,
                strip_modes,
            },
        ))
    }

    /// What the compositor state file reports: each tablet's mapping and
    /// tool, each pad, and the pad OSD.
    pub fn introspect(&self) -> serde_json::Value {
        serde_json::json!({
            "tablets": self.tablets.iter().map(|t| serde_json::json!({
                "name": t.identity.name,
                "id": format!("{:04x}:{:04x}", t.vendor, t.product),
                "settings_path": t.path,
                "size_mm": t.identity.size_mm,
                "libwacom": t.wacom.as_ref().map(|w| &w.name),
                "mapping": format!("{:?}", t.mapping.mode).to_lowercase(),
                "output": t.mapped_to,
                "keep_aspect_ratio": t.mapping.output_ratio,
                "area": t.applied.as_ref().map(|s| s.area.clone()),
                "left_handed": t.applied.as_ref().map(|s| s.left_handed),
                "calibration": t.device.config_calibration_matrix(),
                "tool": t.tool.as_ref().map(|tool| serde_json::json!({
                    "type": format!("{:?}", tool.desc.tool_type).to_lowercase(),
                    "serial": format!("{:x}", tool.desc.hardware_serial),
                    "settings_path": tool.path,
                    "pressure_curve": tool.applied.as_ref().map(|s| if tool.eraser {
                        s.eraser_pressure_curve.clone()
                    } else {
                        s.pressure_curve.clone()
                    }),
                })),
            })).collect::<Vec<_>>(),
            "pads": self.pads.iter().map(|p| serde_json::json!({
                "id": p.id,
                "name": p.desc.name,
                "devnode": p.devnode,
                "libwacom": p.wacom.as_ref().map(|w| &w.name),
                "layout": p.wacom.as_ref().and_then(|w| w.layout.as_ref()),
                "buttons": p.desc.buttons,
                "group_modes": p.group_modes,
                "groups": p.desc.groups.iter().map(|g| serde_json::json!({
                    "buttons": g.buttons,
                    "mode_switch_buttons": g.mode_switch_buttons,
                    "rings": g.rings,
                    "strips": g.strips,
                    "modes": g.modes,
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "pad_osd": self.osd,
        })
    }
}
