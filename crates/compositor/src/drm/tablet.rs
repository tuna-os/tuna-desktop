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

use crate::tablet::{
    map_tool_button, stylus_pressure, stylus_settings_path, tablet_settings_path, DeviceMapping,
    EraserButtonMode, Mapping, Monitor, PressureCurve, StylusSettings, TabletEvent, TabletIdentity,
    TabletSettings, ToolAxes, ToolButton, BTN_STYLUS3,
};
use crate::tablet_settings::{Kind, Snapshot};

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
}

struct PadDevice {
    device: li::Device,
    desc: TabletPadDescriptor,
}

/// Every connected tablet and pad on the hardware seat.
#[derive(Default)]
pub struct Tablets {
    tablets: Vec<TabletDevice>,
    pads: Vec<PadDevice>,
    settings: Snapshot,
    monitors: Vec<Monitor>,
    events: Vec<TabletEvent>,
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
            };
            eprintln!(
                "tuna-compositor: tablet: added {} ({vendor:04x}:{product:04x}) size={:?}",
                tablet.identity.name, tablet.identity.size_mm
            );
            Self::apply_tablet(&mut tablet, &self.settings, &self.monitors);
            self.tablets.push(tablet);
            self.events.push(TabletEvent::TabletAdded(desc.clone()));
            // A pad in the same device group pairs with it now.
            let group = device.device_group();
            for pad in &self.pads {
                if pad.device.device_group() == group {
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
            self.pads.push(PadDevice {
                device: device.clone(),
                desc: desc.clone(),
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
            self.events.push(TabletEvent::PadRemoved(pad.desc));
        }
    }

    /// The settings paths the connected tablets and the tools in
    /// proximity read.
    pub(crate) fn wanted(&self) -> BTreeSet<(Kind, String)> {
        let mut wanted = BTreeSet::new();
        for tablet in &self.tablets {
            wanted.insert((Kind::Tablet, tablet.path.clone()));
            if let Some(tool) = &tablet.tool {
                wanted.insert((Kind::Stylus, tool.path.clone()));
            }
        }
        for pad in &self.pads {
            wanted.insert((
                Kind::Tablet,
                tablet_settings_path(pad.device.id_vendor(), pad.device.id_product()),
            ));
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
                let matrix = crate::tablet::area_matrix(area)
                    .or_else(|| device.config_calibration_default_matrix());
                if let Some(matrix) = matrix {
                    let _ = device.config_calibration_set_matrix(matrix);
                }
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
                let pos = Self::position(tablet, &event, relative, monitors);
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
                    ToolButton::Action(action) => {
                        // meta_tool_action_mapper_handle_button takes these;
                        // the switch-monitor and keybinding actions are not
                        // implemented here, and the press is not delivered.
                        if pressed {
                            eprintln!(
                                "tuna-compositor: tablet: stylus button {:#x} action {action:?} is not implemented; consumed",
                                event.button()
                            );
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// One libinput pad event.
    pub fn pad_event(&mut self, event: TabletPadEvent) {
        let device = EventTrait::device(&event);
        let Some(pad) = self.pads.iter().find(|p| p.device == device) else {
            return;
        };
        let time = event.time();
        match event {
            TabletPadEvent::Button(event) => {
                let button = event.button_number();
                let group = event.mode_group();
                let mode = group
                    .button_is_toggle(button)
                    .then(|| (group.index() as usize, event.mode()));
                self.events.push(TabletEvent::PadButton {
                    pad: pad.desc.clone(),
                    button,
                    pressed: event.button_state()
                        == ButtonState::Pressed,
                    mode,
                    time,
                });
            }
            TabletPadEvent::Ring(event) => {
                let angle = event.position();
                self.events.push(TabletEvent::PadRing {
                    pad: pad.desc.clone(),
                    ring: event.number(),
                    angle: (angle >= 0.0).then_some(angle),
                    finger: event.source() == RingAxisSource::Finger,
                    time,
                });
            }
            TabletPadEvent::Strip(event) => {
                let position = event.position();
                self.events.push(TabletEvent::PadStrip {
                    pad: pad.desc.clone(),
                    strip: event.number(),
                    position: (position >= 0.0).then_some(position),
                    finger: event.source() == StripAxisSource::Finger,
                    time,
                });
            }
            _ => {}
        }
    }

    /// What the compositor state file reports: each tablet's mapping and
    /// tool, each pad.
    pub fn introspect(&self) -> serde_json::Value {
        serde_json::json!({
            "tablets": self.tablets.iter().map(|t| serde_json::json!({
                "name": t.identity.name,
                "id": format!("{:04x}:{:04x}", t.vendor, t.product),
                "settings_path": t.path,
                "size_mm": t.identity.size_mm,
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
                "name": p.desc.name,
                "buttons": p.desc.buttons,
                "groups": p.desc.groups.iter().map(|g| serde_json::json!({
                    "buttons": g.buttons,
                    "rings": g.rings,
                    "strips": g.strips,
                    "modes": g.modes,
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        })
    }
}
