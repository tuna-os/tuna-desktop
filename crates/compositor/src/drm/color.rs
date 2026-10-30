//! CRTC colour properties: GAMMA_LUT, legacy gamma and CTM, plus the
//! connector EDID blob (P-DS-03, #341).
//!
//! Mutter's native backend programs `GAMMA_LUT` (or the legacy gamma
//! ramp on non-atomic drivers) and `CTM` per CRTC (`meta-crtc-kms.c`,
//! `meta-kms-crtc.c`). Property discovery, the `drm_color_lut` blob
//! layout, the previous-blob bookkeeping and the legacy linear reset are
//! adapted from niri's `GammaProps` and `set_gamma_for_crtc`
//! (`src/backend/tty.rs`, niri 1f03391e, GPL-3.0-or-later, © Ivan
//! Molodetskikh and contributors).

use std::num::NonZeroU64;

use smithay::backend::drm::DrmDevice;
use smithay::reexports::drm::control::{connector, crtc, property, Device as ControlDevice};

use crate::color_manager::{CrtcColorCaps, GammaLut, Programming};

/// The colour properties one CRTC exposes.
#[derive(Debug, Default)]
pub struct CrtcColor {
    crtc: Option<crtc::Handle>,
    gamma_lut: Option<property::Handle>,
    gamma_lut_size: usize,
    legacy_gamma_size: usize,
    ctm: Option<property::Handle>,
    gamma_blob: Option<NonZeroU64>,
    ctm_blob: Option<NonZeroU64>,
}

fn find_props(drm: &DrmDevice, crtc: crtc::Handle) -> Vec<(String, property::Handle, u64)> {
    let Ok(props) = drm.get_properties(crtc) else {
        return Vec::new();
    };
    let (handles, values) = props.as_props_and_values();
    handles
        .iter()
        .zip(values)
        .filter_map(|(&handle, &value)| {
            let info = drm.get_property(handle).ok()?;
            Some((info.name().to_str().ok()?.to_owned(), handle, value))
        })
        .collect()
}

impl CrtcColor {
    /// Discover `crtc`'s colour properties.
    pub fn new(drm: &DrmDevice, crtc: crtc::Handle) -> Self {
        let mut color = Self {
            crtc: Some(crtc),
            ..Default::default()
        };
        for (name, handle, value) in find_props(drm, crtc) {
            match name.as_str() {
                "GAMMA_LUT" => color.gamma_lut = Some(handle),
                "GAMMA_LUT_SIZE" => color.gamma_lut_size = value as usize,
                "CTM" => color.ctm = Some(handle),
                _ => {}
            }
        }
        if color.gamma_lut.is_none() {
            color.gamma_lut_size = 0;
        }
        if !drm.is_atomic() || color.gamma_lut.is_none() {
            color.legacy_gamma_size = drm
                .get_crtc(crtc)
                .map(|info| info.gamma_length() as usize)
                .unwrap_or(0);
        }
        color
    }

    /// What this CRTC can take (`meta_crtc_get_gamma_lut_size`,
    /// `meta_crtc_is_ctm_supported`).
    pub fn caps(&self, atomic: bool) -> CrtcColorCaps {
        let gamma_lut_size = if self.gamma_lut.is_some() && atomic {
            self.gamma_lut_size
        } else {
            self.legacy_gamma_size
        };
        CrtcColorCaps {
            gamma_lut_size,
            ctm: atomic && self.ctm.is_some(),
        }
    }

    /// Clear a ramp or matrix a previous session left behind: the white
    /// point state starts from identity hardware.
    pub fn reset(&mut self, drm: &DrmDevice) {
        if drm.is_atomic() {
            if let Some(prop) = self.gamma_lut {
                let _ = self.set_blob(drm, prop, None, true);
            }
            if let Some(prop) = self.ctm {
                let _ = self.set_blob(drm, prop, None, false);
            }
        } else if self.legacy_gamma_size > 1 {
            let lut = crate::color_manager::generate_gamma_lut(
                None,
                crate::color_manager::DEFAULT_TEMPERATURE_K,
                self.legacy_gamma_size,
            );
            let _ = self.set_legacy(drm, &lut);
        }
    }

    /// Program the white point.
    pub fn program(&mut self, drm: &DrmDevice, programming: &Programming) -> Result<(), String> {
        match programming {
            Programming::GammaLut(lut) => {
                if let (Some(prop), true) = (self.gamma_lut, drm.is_atomic()) {
                    if lut.len() != self.gamma_lut_size {
                        return Err(format!(
                            "gamma LUT has {} entries, GAMMA_LUT_SIZE is {}",
                            lut.len(),
                            self.gamma_lut_size
                        ));
                    }
                    let mut data = Vec::with_capacity(lut.len() * 8);
                    for i in 0..lut.len() {
                        // struct drm_color_lut { red, green, blue, reserved }.
                        for v in [lut.red[i], lut.green[i], lut.blue[i], 0] {
                            data.extend_from_slice(&v.to_ne_bytes());
                        }
                    }
                    self.set_blob(drm, prop, Some(&data), true)
                } else {
                    self.set_legacy(drm, lut)
                }
            }
            Programming::Ctm(matrix) => {
                let prop = self.ctm.ok_or("the CRTC has no CTM property")?;
                let data: Vec<u8> = matrix.iter().flat_map(|v| v.to_ne_bytes()).collect();
                self.set_blob(drm, prop, Some(&data), false)
            }
        }
    }

    fn set_blob(
        &mut self,
        drm: &DrmDevice,
        prop: property::Handle,
        data: Option<&[u8]>,
        gamma: bool,
    ) -> Result<(), String> {
        let crtc = self.crtc.ok_or("no CRTC")?;
        use std::os::fd::AsFd;
        let blob = match data {
            Some(data) => {
                let created = drm_ffi::mode::create_property_blob(
                    drm.device_fd().as_fd(),
                    &mut data.to_vec(),
                )
                .map_err(|e| format!("creating the property blob: {e}"))?;
                NonZeroU64::new(u64::from(created.blob_id))
            }
            None => None,
        };
        let value = blob.map(NonZeroU64::get).unwrap_or(0);
        if let Err(e) = drm.set_property(crtc, prop, property::Value::Blob(value).into()) {
            if value != 0 {
                let _ = drm.destroy_property_blob(value);
            }
            return Err(format!(
                "setting {}: {e}",
                if gamma { "GAMMA_LUT" } else { "CTM" }
            ));
        }
        let previous = if gamma {
            std::mem::replace(&mut self.gamma_blob, blob)
        } else {
            std::mem::replace(&mut self.ctm_blob, blob)
        };
        if let Some(old) = previous {
            let _ = drm.destroy_property_blob(old.get());
        }
        Ok(())
    }

    fn set_legacy(&mut self, drm: &DrmDevice, lut: &GammaLut) -> Result<(), String> {
        let crtc = self.crtc.ok_or("no CRTC")?;
        if lut.len() != self.legacy_gamma_size || lut.len() < 2 {
            return Err(format!(
                "gamma ramp has {} entries, the CRTC takes {}",
                lut.len(),
                self.legacy_gamma_size
            ));
        }
        drm.set_gamma(crtc, &lut.red, &lut.green, &lut.blue)
            .map_err(|e| format!("setting the legacy gamma ramp: {e}"))
    }

    /// The `GAMMA_LUT` blob the kernel currently has, decoded (proofs).
    pub fn read_gamma_lut(&self, drm: &DrmDevice) -> Option<GammaLut> {
        let crtc = self.crtc?;
        let prop = self.gamma_lut?;
        let (_, handle, value) = find_props(drm, crtc)
            .into_iter()
            .find(|(_, h, _)| *h == prop)?;
        let _ = handle;
        if value == 0 {
            return None;
        }
        let data = drm.get_property_blob(value).ok()?;
        let mut lut = GammaLut {
            red: Vec::new(),
            green: Vec::new(),
            blue: Vec::new(),
        };
        for entry in data.as_chunks::<8>().0 {
            let word = |i: usize| u16::from_ne_bytes([entry[i], entry[i + 1]]);
            lut.red.push(word(0));
            lut.green.push(word(2));
            lut.blue.push(word(4));
        }
        Some(lut)
    }
}

/// The connector's `EDID` blob, when it has one.
pub fn connector_edid(drm: &DrmDevice, connector: connector::Handle) -> Option<Vec<u8>> {
    let props = drm.get_properties(connector).ok()?;
    let (handles, values) = props.as_props_and_values();
    for (&handle, &value) in handles.iter().zip(values) {
        let info = drm.get_property(handle).ok()?;
        if info.name().to_str() == Ok("EDID") {
            if value == 0 {
                return None;
            }
            return drm.get_property_blob(value).ok().filter(|b| !b.is_empty());
        }
    }
    None
}

/// gnome-desktop's `gnome_pnp_ids_get_pnp_id`: the udev hwdb vendor of a
/// PNP ID (`acpi:XXX:` → `ID_VENDOR_FROM_DATABASE`).
pub fn pnp_vendor_name(pnp: &str) -> Option<String> {
    let hwdb = udev::Hwdb::new().ok()?;
    let modalias = format!("acpi:{pnp}:");
    hwdb.query_one(modalias.as_str(), "ID_VENDOR_FROM_DATABASE")
        .and_then(|v| v.to_str())
        .map(str::to_owned)
}
