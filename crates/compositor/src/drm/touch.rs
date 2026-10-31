//! Touchscreens on the hardware session (P-DS-04, #345).
//!
//! Mutter 51 maps every touchscreen onto one monitor
//! (`src/backends/meta-input-mapper.c`): the device's
//! `org.gnome.desktop.peripherals.touchscreen` settings at
//! `/org/gnome/desktop/peripherals/touchscreens/VVVV:PPPP/`
//! (`get_device_settings`) name the monitor by EDID in `output`;
//! without a user value the device name and size pick one, and a
//! touchscreen nothing matched takes the built-in panel. A contact's
//! position is the device's normalized position through that monitor's
//! matrix, which carries the monitor's rotation
//! (`src/backends/native/meta-seat-impl.c` `LIBINPUT_EVENT_TOUCH_DOWN`/
//! `_MOTION`, `meta_input_device_native_translate_coordinates_in_impl`).
//! The policy is the shared [`crate::input_mapper`].

use std::collections::{BTreeMap, BTreeSet};

use smithay::reexports::input as li;
use smithay::utils::{Logical, Point};

use crate::input_mapper::{
    pick_monitor, stage_size, AbsoluteMapping, InputIdentity, InputKind, Monitor, Picked,
};
use crate::tablet_settings::Kind;

/// `get_device_settings`: a touchscreen's relocatable settings path, by
/// vendor:product in lowercase four-digit hex.
pub fn touchscreen_settings_path(vendor: u32, product: u32) -> String {
    format!("/org/gnome/desktop/peripherals/touchscreens/{vendor:04x}:{product:04x}/")
}

struct Touchscreen {
    device: li::Device,
    identity: InputIdentity,
    vendor: u32,
    product: u32,
    path: String,
    mapping: AbsoluteMapping,
    picked: Option<(String, u32)>,
}

/// Every touchscreen on the hardware seat and the monitor each drives.
#[derive(Default)]
pub struct Touchscreens {
    devices: Vec<Touchscreen>,
    monitors: Vec<Monitor>,
    stage: (f64, f64),
    /// The layout generation `monitors` was taken at.
    generation: Option<u64>,
    /// User values of each path's `output` key (`None`: automatic).
    outputs: BTreeMap<String, Option<Vec<String>>>,
}

impl Touchscreens {
    /// A libinput device arrived: a touchscreen joins and is mapped.
    pub fn device_added(&mut self, device: &li::Device) {
        if !device.has_capability(li::DeviceCapability::Touch) {
            return;
        }
        let (vendor, product) = (device.id_vendor(), device.id_product());
        let mut touchscreen = Touchscreen {
            device: device.clone(),
            identity: InputIdentity {
                name: device.name().to_owned(),
                size_mm: device.size(),
            },
            vendor,
            product,
            path: touchscreen_settings_path(vendor, product),
            mapping: AbsoluteMapping::new(None, &[]).0,
            picked: None,
        };
        eprintln!(
            "tuna-compositor: touchscreen: added {} ({vendor:04x}:{product:04x}) size={:?}",
            touchscreen.identity.name, touchscreen.identity.size_mm
        );
        Self::map(&mut touchscreen, &self.outputs, &self.monitors);
        self.devices.push(touchscreen);
    }

    pub fn device_removed(&mut self, device: &li::Device) {
        if let Some(index) = self.devices.iter().position(|t| t.device == *device) {
            let gone = self.devices.remove(index);
            eprintln!(
                "tuna-compositor: touchscreen: removed {}",
                gone.identity.name
            );
        }
    }

    /// The settings paths the connected touchscreens read.
    pub(crate) fn wanted(&self) -> BTreeSet<(Kind, String)> {
        self.devices
            .iter()
            .map(|t| (Kind::Touchscreen, t.path.clone()))
            .collect()
    }

    /// A completed settings read: remap what changed.
    pub(crate) fn set_outputs(&mut self, outputs: BTreeMap<String, Option<Vec<String>>>) {
        if self.outputs == outputs {
            return;
        }
        self.outputs = outputs;
        self.remap();
    }

    /// Whether the monitors were taken at layout `generation`.
    pub fn monitors_current(&self, generation: u64) -> bool {
        self.generation == Some(generation)
    }

    /// The lit monitors at layout `generation`.
    pub fn set_monitors(&mut self, generation: u64, monitors: Vec<Monitor>) {
        self.generation = Some(generation);
        if monitors == self.monitors {
            return;
        }
        self.monitors = monitors;
        self.stage = stage_size(&self.monitors);
        self.remap();
    }

    fn remap(&mut self) {
        for touchscreen in &mut self.devices {
            Self::map(touchscreen, &self.outputs, &self.monitors);
        }
    }

    /// `guess_candidates` and `mapper_input_info_set_output` for one
    /// touchscreen (touchscreens keep no aspect ratio).
    fn map(
        touchscreen: &mut Touchscreen,
        outputs: &BTreeMap<String, Option<Vec<String>>>,
        monitors: &[Monitor],
    ) {
        let output = outputs.get(&touchscreen.path).and_then(Option::as_deref);
        let picked = pick_monitor(
            InputKind::Touchscreen,
            &touchscreen.identity,
            output,
            monitors,
        );
        touchscreen.mapping = AbsoluteMapping::new(picked.map(|p| &monitors[p.index]), monitors).0;
        let picked =
            picked.map(|Picked { index, score }| (monitors[index].connector.clone(), score));
        if touchscreen.picked != picked {
            eprintln!(
                "tuna-compositor: touchscreen: {} mapped to {} (score {})",
                touchscreen.identity.name,
                picked.as_ref().map_or("the desktop", |(c, _)| c.as_str()),
                picked.as_ref().map_or(0, |(_, s)| *s),
            );
            touchscreen.picked = picked;
        }
    }

    /// Where a contact of `device` lands, given its position normalized
    /// over the device; `None` for a device that is no touchscreen.
    pub fn position(&self, device: &li::Device, norm: (f64, f64)) -> Option<Point<f64, Logical>> {
        let touchscreen = self.devices.iter().find(|t| t.device == *device)?;
        Some(touchscreen.mapping.translate(norm, self.stage))
    }

    /// The desktop extents a contact is normalized over.
    pub fn stage(&self) -> (f64, f64) {
        self.stage
    }

    /// Touchscreens for the compositor state file.
    pub fn introspect(&self) -> serde_json::Value {
        self.devices
            .iter()
            .map(|t| {
                serde_json::json!({
                    "name": t.identity.name,
                    "vendor": t.vendor,
                    "product": t.product,
                    "path": t.path,
                    "output": t.picked.as_ref().map(|(connector, _)| connector),
                    "score": t.picked.as_ref().map_or(0, |(_, score)| *score),
                    "size_mm": t.identity.size_mm.map(|(w, h)| [w, h]),
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn touchscreen_settings_paths_follow_mutter() {
        assert_eq!(
            touchscreen_settings_path(0x4f3, 0x2c82),
            "/org/gnome/desktop/peripherals/touchscreens/04f3:2c82/"
        );
    }
}
