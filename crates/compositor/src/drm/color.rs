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
    fd: Option<smithay::backend::drm::DrmDeviceFd>,
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
        let mut color = Self::default();
        color.crtc = Some(crtc);
        color.fd = Some(drm.device_fd().clone());
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
    fn release_blobs(&mut self, mut destroy: impl FnMut(u64)) {
        for blob in [self.gamma_blob.take(), self.ctm_blob.take()]
            .into_iter()
            .flatten()
        {
            destroy(blob.get());
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

impl Drop for CrtcColor {
    fn drop(&mut self) {
        // Hotplug may destroy an output while the DRM device remains open.
        // Keep the FD alive independently and release only blobs this owner
        // created; the kernel retains references for any still-attached state.
        if let Some(fd) = self.fd.take() {
            self.release_blobs(|blob| {
                let _ = fd.destroy_property_blob(blob);
            });
        }
    }
}

#[cfg(test)]
mod calibration_blob_tests {
    use super::*;

    #[test]
    fn teardown_releases_gamma_and_ctm_ownership_exactly_once() {
        let mut color = CrtcColor::default();
        color.gamma_blob = NonZeroU64::new(41);
        color.ctm_blob = NonZeroU64::new(73);
        let mut released = Vec::new();
        color.release_blobs(|blob| released.push(blob));
        assert_eq!(released, [41, 73]);
        assert!(color.gamma_blob.is_none() && color.ctm_blob.is_none());
        color.release_blobs(|blob| released.push(blob));
        assert_eq!(released, [41, 73]);
    }

    #[test]
    fn absent_or_already_replaced_blobs_are_not_released_again() {
        let mut color = CrtcColor::default();
        let mut released = Vec::new();
        color.release_blobs(|blob| released.push(blob));
        assert!(released.is_empty());
        color.ctm_blob = NonZeroU64::new(83);
        color.release_blobs(|blob| released.push(blob));
        assert_eq!(released, [83]);
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

/// Sink and connector capabilities, independent of render/scanout support.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ConnectorCaps {
    pub edid: crate::edid::HdrCapabilities,
    pub atomic: bool,
    pub default_colorspace: Option<u64>,
    pub bt2020_rgb: Option<u64>,
    pub max_bpc: Option<(u64, u64)>,
    pub hdr_metadata: bool,
}

impl ConnectorCaps {
    pub fn supports_bt2100(self) -> bool {
        self.atomic
            && self.edid.supports_bt2100()
            && self.default_colorspace.is_some()
            && self.bt2020_rgb.is_some()
            && self
                .max_bpc
                .is_some_and(|(min, max)| min <= max && max >= 10)
            && self.hdr_metadata
    }
}

/// Connector encoding. The metadata blob is retained for the lifetime of
/// the scanout surface, including failed commits and SDR/HDR round trips.
pub struct ConnectorColor {
    pub caps: ConnectorCaps,
    connector: connector::Handle,
    sdr_bpc: Option<u64>,
    hdr_blob: Option<NonZeroU64>,
    fd: smithay::backend::drm::DrmDeviceFd,
}

impl ConnectorColor {
    pub fn discover(drm: &DrmDevice, connector: connector::Handle, edid: Option<&[u8]>) -> Self {
        let mut caps = ConnectorCaps {
            edid: edid.map(crate::edid::hdr_capabilities).unwrap_or_default(),
            atomic: drm.is_atomic(),
            ..Default::default()
        };
        let mut sdr_bpc = None;
        if let Ok(props) = drm.get_properties(connector) {
            let (handles, values) = props.as_props_and_values();
            for (&handle, &value) in handles.iter().zip(values) {
                let Ok(info) = drm.get_property(handle) else {
                    continue;
                };
                if !info.mutable() {
                    continue;
                }
                match (info.name().to_str(), info.value_type()) {
                    (Ok("Colorspace"), property::ValueType::Enum(entries)) => {
                        for entry in entries.values().1 {
                            match entry.name().to_str() {
                                Ok("Default") => caps.default_colorspace = Some(entry.value()),
                                Ok("BT2020_RGB") => caps.bt2020_rgb = Some(entry.value()),
                                _ => {}
                            }
                        }
                    }
                    (Ok("max bpc"), property::ValueType::UnsignedRange(min, max)) => {
                        caps.max_bpc = Some((min, max));
                        sdr_bpc = Some(value.clamp(min, max));
                    }
                    (Ok("HDR_OUTPUT_METADATA"), property::ValueType::Blob) => {
                        caps.hdr_metadata = true
                    }
                    _ => {}
                }
            }
        }
        Self {
            caps,
            connector,
            sdr_bpc,
            hdr_blob: None,
            fd: drm.device_fd().clone(),
        }
    }

    pub fn state(
        &mut self,
        mode: crate::display_config::ColorMode,
    ) -> Result<smithay::backend::drm::ConnectorColorState, String> {
        use crate::display_config::ColorMode;
        let hdr = match mode {
            ColorMode::Default => false,
            ColorMode::Bt2100 if self.caps.supports_bt2100() => true,
            _ => {
                return Err(format!(
                    "Color mode {} not supported by connector",
                    mode as u32
                ))
            }
        };
        if hdr && self.hdr_blob.is_none() {
            let blob = self
                .fd
                .create_property_blob(&bt2100_metadata())
                .map_err(|error| format!("HDR metadata blob: {error}"))?;
            self.hdr_blob = NonZeroU64::new(blob.into());
            if self.hdr_blob.is_none() {
                return Err("DRM returned an invalid HDR metadata blob".into());
            }
        }
        Ok(smithay::backend::drm::ConnectorColorState {
            connector: self.connector,
            colorspace: if hdr {
                self.caps.bt2020_rgb
            } else {
                self.caps.default_colorspace
            },
            max_bpc: if hdr {
                self.caps.max_bpc.map(|(min, _)| min.max(10))
            } else {
                self.sdr_bpc
            },
            hdr_metadata: self.caps.hdr_metadata.then(|| {
                if hdr {
                    self.hdr_blob.unwrap().get()
                } else {
                    0
                }
            }),
        })
    }

    /// Actual connector properties and metadata bytes, not the requested
    /// state. Used alongside negotiated scanout precision in native proof.
    pub fn readback(&self, drm: &DrmDevice) -> Option<serde_json::Value> {
        let props = drm.get_properties(self.connector).ok()?;
        let (handles, values) = props.as_props_and_values();
        let (mut colorspace, mut bpc, mut metadata) = (None, None, None);
        for (&handle, &value) in handles.iter().zip(values) {
            let Ok(info) = drm.get_property(handle) else {
                continue;
            };
            match info.name().to_str() {
                Ok("Colorspace") => colorspace = Some(value),
                Ok("max bpc") => bpc = Some(value),
                Ok("HDR_OUTPUT_METADATA") => metadata = Some(value),
                _ => {}
            }
        }
        let bytes = metadata
            .filter(|id| *id != 0)
            .and_then(|id| drm.get_property_blob(id).ok());
        Some(serde_json::json!({
            "colorspace": colorspace,
            "max_bpc": bpc,
            "hdr_metadata_blob": metadata,
            "hdr_metadata_bytes": bytes,
        }))
    }
}

impl Drop for ConnectorColor {
    fn drop(&mut self) {
        if let Some(blob) = self.hdr_blob {
            let _ = self.fd.destroy_property_blob(blob.get());
        }
    }
}

/// Linux `hdr_output_metadata`, metadata type 0 / CTA type-1 PQ.
/// Mutter 51 meta_output_get_color_metadata describes BT.2020 mastering
/// primaries/D65; unspecified mastering luminance and CLL/FALL remain zero.
pub fn bt2100_metadata() -> [u8; 32] {
    let mut bytes = [0u8; 32];
    bytes[4] = 2; // SMPTE ST 2084, not traditional HDR or HLG.
    for (index, coordinate) in [35400u16, 14600, 8500, 39850, 6550, 2300, 15635, 16450]
        .into_iter()
        .enumerate()
    {
        let at = 6 + index * 2;
        bytes[at..at + 2].copy_from_slice(&coordinate.to_ne_bytes());
    }
    bytes
}

#[cfg(test)]
mod hdr_tests {
    use super::*;

    #[test]
    fn capability_intersection_requires_every_native_boundary() {
        let caps = ConnectorCaps {
            edid: crate::edid::hdr_capabilities(&crate::edid::tests::hdr_edid()),
            atomic: true,
            default_colorspace: Some(7),
            bt2020_rgb: Some(29),
            max_bpc: Some((8, 12)),
            hdr_metadata: true,
        };
        assert!(caps.supports_bt2100());
        assert!(!ConnectorCaps {
            atomic: false,
            ..caps
        }
        .supports_bt2100());
        assert!(!ConnectorCaps {
            bt2020_rgb: None,
            ..caps
        }
        .supports_bt2100());
        assert!(!ConnectorCaps {
            default_colorspace: None,
            ..caps
        }
        .supports_bt2100());
        assert!(!ConnectorCaps {
            hdr_metadata: false,
            ..caps
        }
        .supports_bt2100());
        assert!(ConnectorCaps {
            max_bpc: Some((12, 16)),
            ..caps
        }
        .supports_bt2100());
        for range in [None, Some((8, 8)), Some((12, 10))] {
            assert!(!ConnectorCaps {
                max_bpc: range,
                ..caps
            }
            .supports_bt2100());
        }
    }

    #[test]
    fn metadata_has_native_abi_pq_bt2020_and_no_invented_luminance() {
        let bytes = bt2100_metadata();
        assert_eq!(u32::from_ne_bytes(bytes[..4].try_into().unwrap()), 0);
        assert_eq!(&bytes[4..6], &[2, 0]);
        let coordinates: Vec<_> = bytes[6..22]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|v| u16::from_ne_bytes(*v))
            .collect();
        assert_eq!(
            coordinates,
            [35400, 14600, 8500, 39850, 6550, 2300, 15635, 16450]
        );
        assert!(bytes[22..].iter().all(|byte| *byte == 0));
    }
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
