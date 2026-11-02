//! Colour management on the KMS path: colord devices and profiles,
//! calibration (VCGT) and Night Light white point (P-DS-03, #341).
//!
//! GNOME 51 splits this between Mutter's `MetaColorManager`,
//! `MetaColorDevice`, `MetaColorProfile` and `MetaColorStore`
//! (`src/backends/meta-color-*.c`), all reimplemented here:
//!
//! - Every physical monitor becomes a colord device: `CreateDevice` on
//!   the system bus with object scope `temp`, id `xrandr-{vendor}-{product}-{serial}`
//!   (`generate_cd_device_id`) and Mutter's properties (`Kind=display`,
//!   `Mode=physical`, `Colorspace=rgb`, vendor/model/serial,
//!   `XRANDR_name`, `OutputPriority`, `OutputEdidMd5`, `Embedded` for
//!   panels). An existing device of that id is reused (`FindDeviceById`).
//! - The device profile is generated and written to
//!   `$XDG_DATA_HOME/icc/edid-<md5>.icc` from the EDID primaries, white
//!   and gamma, or `device-<md5(device id)>.icc` with lcms2's sRGB
//!   profile when there is no EDID but the CRTC has a gamma LUT
//!   (`meta_color_store_ensure_device_profile`,
//!   `create_device_icc_profile`), then registered with
//!   `CreateProfileWithFd` as `icc-<file md5>`. Its `MAPPING_device_id`
//!   metadata lets colord soft-add it to the device.
//! - Every other ICC file in `$XDG_DATA_HOME/icc` (not hidden, backup,
//!   `edid-*` or `device-*`) becomes a colord profile too, as Mutter's
//!   colour store does, so GNOME Settings' "Add Profile" import completes.
//! - The device's default profile (the first of colord's `Profiles`) is
//!   the assigned profile (`update_assigned_profile`); its `vcgt`
//!   calibration feeds the gamma ramp.
//! - The white point (`update_white_point`): with a GAMMA_LUT the CRTC
//!   gets `vcgt × blackbody(T)` (or the uncalibrated `i/(n-1) ×
//!   blackbody(T)` ramp), else a CTM with the blackbody scales; the
//!   identity ramp at 6500 K without a profile is only programmed to
//!   clear a previous white point. The blackbody is colord's Planckian
//!   table (`cd_color_get_blackbody_rgb_full` with
//!   `CD_COLOR_BLACKBODY_FLAG_USE_PLANCKIAN`), interpolated per 100 K.
//! - `org.gnome.Mutter.ColorManager.CalibrateMonitor` opens a calibration
//!   session (`meta-color-calibration-session.c`): the white point
//!   switches to 6500 K and `SetCrtcGammaLut` programs raw ramps until
//!   `Stop` or the caller leaves the bus.
//!
//! Divergences, each recorded rather than hidden: Night Light still
//! applies when colord is absent or a device failed to register (Mutter
//! programs no white point until a colord device is ready, so Night
//! Light would silently stop); without GAMMA_LUT or CTM the GL tint
//! remains the last resort (Mutter applies nothing). Builtin-panel EFI
//! colour info (`on_efi_panel_color_info_loaded`) and brightness
//! profiles are not read. Nested (winit) outputs are virtual monitors
//! and get no colord device, as in Mutter (`meta_monitor_is_virtual`).
//!
//! Sources (mutter `51.0`, `138a14fbeef09d49ebf5be8a0cb83b042dd5c841`):
//! `src/backends/meta-color-manager.c`, `meta-color-device.c`,
//! `meta-color-profile.c`, `meta-color-store.c`,
//! `meta-color-calibration-session.c`,
//! `data/dbus-interfaces/org.gnome.Mutter.ColorManager.xml`; colord
//! 1.4.7 `lib/colord/cd-color.c` (blackbody data, LGPL-2.1-or-later) and
//! `src/cd-main.c` (`cd_main_auto_add_from_md`); GNOME Settings 51.0
//! `panels/color/cc-color-calibrate.c`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::edid::MonitorIdentity;
use crate::icc::{self, Vcgt};

/// Mutter's default temperature (`meta_color_manager_get_default_temperature`).
pub const DEFAULT_TEMPERATURE_K: u32 = 6500;

/// colord's Planckian blackbody table, 1000 K to 10000 K in 100 K steps
/// (`blackbody_data_d65plankian`, from redshift's colour ramp).
const BLACKBODY_PLANCKIAN: [[f64; 3]; 91] = [
    [1.0000, 0.1817, 0.0000],
    [1.0000, 0.2550, 0.0000],
    [1.0000, 0.3094, 0.0000],
    [1.0000, 0.3536, 0.0000],
    [1.0000, 0.3909, 0.0000],
    [1.0000, 0.4232, 0.0000],
    [1.0000, 0.4516, 0.0000],
    [1.0000, 0.4768, 0.0000],
    [1.0000, 0.4992, 0.0000],
    [1.0000, 0.5194, 0.0000],
    [1.0000, 0.5436, 0.0868],
    [1.0000, 0.5662, 0.1407],
    [1.0000, 0.5873, 0.1836],
    [1.0000, 0.6072, 0.2214],
    [1.0000, 0.6260, 0.2559],
    [1.0000, 0.6437, 0.2882],
    [1.0000, 0.6605, 0.3187],
    [1.0000, 0.6765, 0.3479],
    [1.0000, 0.6916, 0.3758],
    [1.0000, 0.7060, 0.4027],
    [1.0000, 0.7198, 0.4286],
    [1.0000, 0.7329, 0.4537],
    [1.0000, 0.7454, 0.4779],
    [1.0000, 0.7574, 0.5015],
    [1.0000, 0.7689, 0.5243],
    [1.0000, 0.7799, 0.5464],
    [1.0000, 0.7904, 0.5679],
    [1.0000, 0.8005, 0.5888],
    [1.0000, 0.8102, 0.6092],
    [1.0000, 0.8196, 0.6289],
    [1.0000, 0.8285, 0.6482],
    [1.0000, 0.8372, 0.6669],
    [1.0000, 0.8455, 0.6851],
    [1.0000, 0.8535, 0.7028],
    [1.0000, 0.8612, 0.7201],
    [1.0000, 0.8686, 0.7369],
    [1.0000, 0.8758, 0.7533],
    [1.0000, 0.8827, 0.7692],
    [1.0000, 0.8893, 0.7848],
    [1.0000, 0.8958, 0.7999],
    [1.0000, 0.9020, 0.8147],
    [1.0000, 0.9096, 0.8284],
    [1.0000, 0.9171, 0.8419],
    [1.0000, 0.9244, 0.8552],
    [1.0000, 0.9316, 0.8684],
    [1.0000, 0.9385, 0.8813],
    [1.0000, 0.9454, 0.8940],
    [1.0000, 0.9520, 0.9066],
    [1.0000, 0.9585, 0.9189],
    [1.0000, 0.9649, 0.9311],
    [1.0000, 0.9711, 0.9431],
    [1.0000, 0.9771, 0.9548],
    [1.0000, 0.9831, 0.9664],
    [1.0000, 0.9888, 0.9778],
    [1.0000, 0.9945, 0.9890],
    [1.0000, 1.0000, 1.0000],
    [0.9895, 0.9935, 1.0000],
    [0.9794, 0.9872, 1.0000],
    [0.9698, 0.9812, 1.0000],
    [0.9605, 0.9754, 1.0000],
    [0.9516, 0.9698, 1.0000],
    [0.9430, 0.9644, 1.0000],
    [0.9348, 0.9592, 1.0000],
    [0.9269, 0.9542, 1.0000],
    [0.9193, 0.9494, 1.0000],
    [0.9119, 0.9447, 1.0000],
    [0.9049, 0.9402, 1.0000],
    [0.8981, 0.9358, 1.0000],
    [0.8915, 0.9316, 1.0000],
    [0.8852, 0.9275, 1.0000],
    [0.8791, 0.9236, 1.0000],
    [0.8732, 0.9197, 1.0000],
    [0.8674, 0.9160, 1.0000],
    [0.8619, 0.9125, 1.0000],
    [0.8566, 0.9090, 1.0000],
    [0.8514, 0.9056, 1.0000],
    [0.8464, 0.9023, 1.0000],
    [0.8415, 0.8991, 1.0000],
    [0.8368, 0.8960, 1.0000],
    [0.8323, 0.8930, 1.0000],
    [0.8278, 0.8901, 1.0000],
    [0.8235, 0.8873, 1.0000],
    [0.8194, 0.8845, 1.0000],
    [0.8153, 0.8818, 1.0000],
    [0.8114, 0.8792, 1.0000],
    [0.8075, 0.8767, 1.0000],
    [0.8038, 0.8742, 1.0000],
    [0.8002, 0.8718, 1.0000],
    [0.7967, 0.8694, 1.0000],
    [0.7932, 0.8671, 1.0000],
    [0.7898, 0.8649, 1.0000],
];

/// The blackbody RGB for `temperature` Kelvin, clipped to 1000–10000 K.
pub fn blackbody_rgb(temperature: u32) -> [f64; 3] {
    let temp = temperature.clamp(1000, 10000);
    let quot = (temp / 100 - 10) as usize;
    let rem = temp % 100;
    let a = BLACKBODY_PLANCKIAN[quot];
    if rem == 0 {
        return a;
    }
    let b = BLACKBODY_PLANCKIAN[quot + 1];
    let t = f64::from(rem) / 100.0;
    [
        (1.0 - t) * a[0] + t * b[0],
        (1.0 - t) * a[1] + t * b[1],
        (1.0 - t) * a[2] + t * b[2],
    ]
}

/// One CRTC gamma ramp, `size` entries per channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GammaLut {
    pub red: Vec<u16>,
    pub green: Vec<u16>,
    pub blue: Vec<u16>,
}

impl GammaLut {
    pub fn len(&self) -> usize {
        self.red.len()
    }

    pub fn is_empty(&self) -> bool {
        self.red.is_empty()
    }
}

/// `meta_color_profile_generate_gamma_lut`: the calibration curves
/// scaled by the blackbody colour, or the uncalibrated ramp.
pub fn generate_gamma_lut(vcgt: Option<&Vcgt>, temperature: u32, size: usize) -> GammaLut {
    assert!(size > 1, "a gamma LUT needs at least two entries");
    let bb = blackbody_rgb(temperature);
    let mut lut = GammaLut {
        red: Vec::with_capacity(size),
        green: Vec::with_capacity(size),
        blue: Vec::with_capacity(size),
    };
    for i in 0..size {
        match vcgt {
            Some(Vcgt(curves)) => {
                // cmsFloat32Number input, double scaling, C truncation.
                let x = f64::from((i as f64 / (size - 1) as f64) as f32);
                let channel = |c: usize| {
                    let v = f64::from(curves[c].eval(x) as f32) * bb[c] * 65535.0;
                    v.clamp(0.0, 65535.0) as u16
                };
                lut.red.push(channel(0));
                lut.green.push(channel(1));
                lut.blue.push(channel(2));
            }
            None => {
                let input = ((i * 0xffff) / (size - 1)) as u16;
                lut.red.push((f64::from(input) * bb[0]) as u16);
                lut.green.push((f64::from(input) * bb[1]) as u16);
                lut.blue.push((f64::from(input) * bb[2]) as u16);
            }
        }
    }
    lut
}

/// `meta_ctm_new_from_rgb_scales`: a diagonal CTM in S31.32
/// sign-magnitude (`struct drm_color_ctm`), row-major.
pub fn ctm_from_scales(scales: [f64; 3]) -> [u64; 9] {
    let fixed = |scale: f64| {
        let scale = if !scale.is_finite() || scale < 0.0 {
            0.0
        } else {
            scale.min(1.0)
        };
        (f64::from(scale as f32) * (1u64 << 32) as f64) as u64
    };
    let mut m = [0u64; 9];
    m[0] = fixed(scales[0]);
    m[4] = fixed(scales[1]);
    m[8] = fixed(scales[2]);
    m
}

/// A colord profile as the compositor knows it.
#[derive(Debug, Clone, PartialEq)]
pub struct ProfileInfo {
    /// colord `ProfileId`, e.g. `icc-<md5>`.
    pub id: String,
    pub object_path: String,
    pub filename: Option<String>,
    pub title: Option<String>,
}

/// One monitor's colord side, as the colord thread publishes it.
#[derive(Debug, Clone, Default)]
pub struct DeviceSnapshot {
    pub connector: String,
    pub device_id: String,
    /// colord object path once the device exists.
    pub object_path: Option<String>,
    /// Setup finished (successfully or not): Mutter's `is_ready`.
    pub ready: bool,
    /// The generated EDID/sRGB device profile.
    pub device_profile: Option<ProfileInfo>,
    /// The device's default colord profile.
    pub assigned: Option<ProfileInfo>,
    /// The assigned profile's calibration curves.
    pub calibration: Option<Arc<Vcgt>>,
    /// Last setup failure, for the state file and logs.
    pub error: Option<String>,
}

/// colord service status.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ColordStatus {
    /// Not attempted (nested session, or before the thread starts).
    #[default]
    Idle,
    Connected,
    /// No system bus or no colord: Mutter logs "There is no colord server
    /// available" and registers nothing.
    Unavailable(String),
}

/// An open calibration session (`MetaColorCalibrationSession`).
#[derive(Debug, Clone)]
pub struct CalibrationSession {
    pub connector: String,
    pub object_path: String,
    pub owner: String,
    /// The ramp `SetCrtcGammaLut` last set; `None` until the first call.
    pub lut: Option<GammaLut>,
}

#[derive(Debug, Default)]
struct ShareState {
    colord: ColordStatus,
    devices: Vec<DeviceSnapshot>,
    gamma_sizes: HashMap<String, usize>,
    sessions: Vec<CalibrationSession>,
    monitor_generation: u64,
}

/// State shared between the colord thread, the D-Bus thread and the
/// render loop. Every change bumps [`ColorShare::generation`] and wakes
/// the loop.
#[derive(Debug, Default)]
pub struct ColorShare {
    state: Mutex<ShareState>,
    generation: AtomicU64,
    wake: crate::wake::Wake,
    next_session: AtomicU64,
}

impl ColorShare {
    pub(crate) fn set_wake(&self, wake: &crate::wake::Wake) {
        self.wake.connect(wake);
    }

    fn changed(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.wake.signal();
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    pub fn colord_status(&self) -> ColordStatus {
        self.state
            .lock()
            .map(|s| s.colord.clone())
            .unwrap_or_default()
    }

    pub fn devices(&self) -> Vec<DeviceSnapshot> {
        self.state
            .lock()
            .map(|s| s.devices.clone())
            .unwrap_or_default()
    }

    pub fn device(&self, connector: &str) -> Option<DeviceSnapshot> {
        self.state
            .lock()
            .ok()?
            .devices
            .iter()
            .find(|d| d.connector == connector)
            .cloned()
    }

    fn set_status(&self, status: ColordStatus) {
        if let Ok(mut s) = self.state.lock() {
            s.colord = status;
        }
        self.changed();
    }

    fn update_device(
        &self,
        generation: u64,
        connector: &str,
        update: impl FnOnce(&mut DeviceSnapshot),
    ) {
        if let Ok(mut s) = self.state.lock() {
            if s.monitor_generation != generation {
                return;
            }
            if let Some(device) = s.devices.iter_mut().find(|d| d.connector == connector) {
                update(device);
            }
        }
        self.changed();
    }

    fn set_devices(&self, generation: u64, devices: Vec<DeviceSnapshot>) {
        if let Ok(mut s) = self.state.lock() {
            if s.monitor_generation != generation {
                return;
            }
            s.devices = devices;
        }
        self.changed();
    }

    /// The render loop's CRTC gamma sizes (`GammaLutSize`, and whether a
    /// monitor can be calibrated).
    pub fn set_gamma_sizes(&self, sizes: impl IntoIterator<Item = (String, usize)>) {
        if let Ok(mut s) = self.state.lock() {
            s.gamma_sizes = sizes.into_iter().collect();
        }
    }

    /// A disconnected or replaced sink cannot keep controlling a new
    /// device which later happens to use its connector name.
    fn begin_monitor_update(&self, monitors: &[ColordMonitor], unchanged: &[&str]) -> u64 {
        let mut state = self.state.lock().unwrap();
        state.monitor_generation = state.monitor_generation.wrapping_add(1);
        state.gamma_sizes = monitors
            .iter()
            .map(|monitor| (monitor.identity.connector.clone(), monitor.gamma_lut_size))
            .collect();
        state
            .sessions
            .retain(|session| unchanged.contains(&session.connector.as_str()));
        state
            .devices
            .retain(|device| unchanged.contains(&device.connector.as_str()));
        let generation = state.monitor_generation;
        drop(state);
        self.changed();
        generation
    }

    fn monitor_generation(&self) -> u64 {
        self.state
            .lock()
            .map(|state| state.monitor_generation)
            .unwrap_or_default()
    }

    pub fn sessions(&self) -> Vec<CalibrationSession> {
        self.state
            .lock()
            .map(|s| s.sessions.clone())
            .unwrap_or_default()
    }

    /// The calibration state of `connector`: `Some(None)` while a
    /// session is open without a ramp yet.
    pub fn calibration_for(&self, connector: &str) -> Option<Option<GammaLut>> {
        self.state
            .lock()
            .ok()?
            .sessions
            .iter()
            .find(|s| s.connector == connector)
            .map(|s| s.lut.clone())
    }

    /// `meta_color_device_start_calibration`, for `CalibrateMonitor`.
    pub fn start_calibration(
        &self,
        connector: &str,
        owner: &str,
    ) -> Result<CalibrationSession, String> {
        let mut s = self
            .state
            .lock()
            .map_err(|_| "colour state unavailable".to_owned())?;
        let known = s.devices.iter().any(|d| d.connector == connector)
            || s.gamma_sizes.contains_key(connector);
        if !known {
            return Err(format!("Unknown monitor connector '{connector}'"));
        }
        // Mutter's device is ready once its colord setup finished; we
        // accept a monitor without colord (see the module docs).
        if s.colord == ColordStatus::Connected
            && !s
                .devices
                .iter()
                .any(|d| d.connector == connector && d.ready)
        {
            return Err("Device is not ready".to_owned());
        }
        if s.gamma_sizes.get(connector).copied().unwrap_or(0) == 0 {
            return Err("Device cannot be calibrated".to_owned());
        }
        if s.sessions.iter().any(|x| x.connector == connector) {
            return Err("Device is already being calibrated".to_owned());
        }
        let id = self.next_session.fetch_add(1, Ordering::SeqCst) + 1;
        let session = CalibrationSession {
            connector: connector.to_owned(),
            object_path: format!("/org/gnome/Mutter/ColorManager/Calibration/u{id}"),
            owner: owner.to_owned(),
            lut: None,
        };
        s.sessions.push(session.clone());
        drop(s);
        self.changed();
        Ok(session)
    }

    pub fn gamma_size(&self, connector: &str) -> usize {
        self.state
            .lock()
            .ok()
            .and_then(|s| s.gamma_sizes.get(connector).copied())
            .unwrap_or(0)
    }

    /// `SetCrtcGammaLut` on the session at `object_path`.
    pub fn set_calibration_lut(&self, object_path: &str, lut: GammaLut) -> Result<(), String> {
        let mut s = self
            .state
            .lock()
            .map_err(|_| "colour state unavailable".to_owned())?;
        let session = s
            .sessions
            .iter_mut()
            .find(|x| x.object_path == object_path)
            .ok_or_else(|| "calibration session closed".to_owned())?;
        session.lut = Some(lut);
        drop(s);
        self.changed();
        Ok(())
    }

    /// Close the session at `object_path` (`Stop`, or its owner left).
    pub fn stop_calibration(&self, object_path: &str) -> bool {
        let removed = self.state.lock().is_ok_and(|mut s| {
            let before = s.sessions.len();
            s.sessions.retain(|x| x.object_path != object_path);
            s.sessions.len() != before
        });
        if removed {
            self.changed();
        }
        removed
    }

    /// Close every session `owner` opened.
    pub fn owner_vanished(&self, owner: &str) -> Vec<String> {
        let closed: Vec<String> = self
            .state
            .lock()
            .map(|mut s| {
                let closed = s
                    .sessions
                    .iter()
                    .filter(|x| x.owner == owner)
                    .map(|x| x.object_path.clone())
                    .collect();
                s.sessions.retain(|x| x.owner != owner);
                closed
            })
            .unwrap_or_default();
        if !closed.is_empty() {
            self.changed();
        }
        closed
    }
}

/// What a CRTC can take, from the KMS properties.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CrtcColorCaps {
    /// `GAMMA_LUT_SIZE` (atomic) or the legacy gamma size; 0 when none.
    pub gamma_lut_size: usize,
    /// The CRTC has a `CTM` property.
    pub ctm: bool,
}

/// The KMS programming one output needs.
#[derive(Debug, Clone, PartialEq)]
pub enum Programming {
    /// A gamma ramp (`meta_monitor_set_gamma_lut`).
    GammaLut(GammaLut),
    /// A CTM (`meta_monitor_set_ctm`).
    Ctm([u64; 9]),
}

/// Per-output white point state (`update_white_point`'s
/// `has_white_point` plus the last programming, so a frame repeats no
/// KMS update).
#[derive(Debug, Clone, Default)]
pub struct WhitePoint {
    has_white_point: bool,
    applied: Option<Programming>,
}

/// What [`WhitePoint::update`] decided.
#[derive(Debug, Clone, PartialEq)]
pub enum WhitePointUpdate {
    /// Nothing to program (identical to what is applied, or the
    /// identity skip).
    Unchanged,
    /// Program this.
    Program(Programming),
    /// The CRTC takes neither a LUT nor a CTM: the renderer must tint.
    Unsupported,
}

impl WhitePoint {
    /// The KMS state this output should have: `calibration` is the
    /// assigned profile's `vcgt`, `session` an open calibration session
    /// (`Some(None)` before its first ramp).
    pub fn update(
        &mut self,
        caps: CrtcColorCaps,
        has_profile: bool,
        calibration: Option<&Vcgt>,
        session: Option<Option<&GammaLut>>,
        temperature: u32,
    ) -> WhitePointUpdate {
        if let Some(Some(lut)) = session {
            if caps.gamma_lut_size > 0 && lut.len() == caps.gamma_lut_size {
                return self.program(Programming::GammaLut(lut.clone()));
            }
        }
        let temperature = if session.is_some() {
            DEFAULT_TEMPERATURE_K
        } else {
            temperature
        };
        let identity = !has_profile && temperature == DEFAULT_TEMPERATURE_K;
        if identity && !self.has_white_point && session.is_none() {
            return WhitePointUpdate::Unchanged;
        }
        if caps.gamma_lut_size > 1 {
            self.has_white_point = !identity;
            let lut = generate_gamma_lut(calibration, temperature, caps.gamma_lut_size);
            self.program(Programming::GammaLut(lut))
        } else if caps.ctm {
            self.has_white_point = !identity;
            self.program(Programming::Ctm(ctm_from_scales(blackbody_rgb(
                temperature,
            ))))
        } else {
            WhitePointUpdate::Unsupported
        }
    }

    fn program(&mut self, programming: Programming) -> WhitePointUpdate {
        if self.applied.as_ref() == Some(&programming) {
            return WhitePointUpdate::Unchanged;
        }
        self.applied = Some(programming.clone());
        WhitePointUpdate::Program(programming)
    }

    /// The hardware lost the programming (session resume, mode change):
    /// the next [`WhitePoint::update`] programs it again.
    pub fn invalidate(&mut self) {
        self.applied = None;
    }

    pub fn applied(&self) -> Option<&Programming> {
        self.applied.as_ref()
    }
}

/// A monitor the colord thread registers, with its CRTC gamma size
/// (Mutter generates an sRGB fallback profile only with a gamma LUT).
#[derive(Debug, Clone, PartialEq)]
pub struct ColordMonitor {
    pub identity: MonitorIdentity,
    pub gamma_lut_size: usize,
}

impl ColordMonitor {
    fn same_device(&self, other: &Self) -> bool {
        self.gamma_lut_size == other.gamma_lut_size
            && self.identity.connector == other.identity.connector
            && self.identity.edid_md5 == other.identity.edid_md5
            && self.identity.vendor == other.identity.vendor
            && self.identity.product == other.identity.product
            && self.identity.serial == other.identity.serial
    }
}

/// `g_get_user_data_dir ()/icc`.
pub fn icc_directory() -> Option<PathBuf> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(data.join("icc"))
}

/// The device profile Mutter generates for `monitor`, with its file
/// name, or `None` when Mutter generates none (no EDID and no gamma
/// LUT).
pub fn device_profile(
    monitor: &ColordMonitor,
    file_path: &Path,
) -> Option<Result<icc::DisplayProfile, String>> {
    let identity = &monitor.identity;
    let device_id = identity.colord_device_id();
    match (&identity.edid_md5, &identity.edid) {
        (Some(md5), Some(edid)) => Some(edid_profile(identity, edid, md5, &device_id, file_path)),
        (None, _) if monitor.gamma_lut_size == 0 => None,
        _ => {
            let mut profile = icc::DisplayProfile::srgb();
            profile
                .metadata
                .push(("Filename".to_owned(), file_path.display().to_string()));
            profile
                .metadata
                .push(("MAPPING_device_id".to_owned(), device_id));
            Some(Ok(profile))
        }
    }
}

/// The device profile's file name in the ICC directory.
pub fn device_profile_file_name(monitor: &ColordMonitor) -> Option<String> {
    let identity = &monitor.identity;
    if let Some(md5) = &identity.edid_md5 {
        return Some(format!("edid-{md5}.icc"));
    }
    (monitor.gamma_lut_size > 0).then(|| {
        format!(
            "device-{}.icc",
            crate::edid::md5_hex(identity.colord_device_id().as_bytes())
        )
    })
}

/// `create_icc_profile_from_edid`.
fn edid_profile(
    identity: &MonitorIdentity,
    edid: &crate::edid::EdidInfo,
    md5: &str,
    device_id: &str,
    file_path: &Path,
) -> Result<icc::DisplayProfile, String> {
    let c = &edid.color;
    let zero = |v: f64| v.abs() < f64::from(f32::EPSILON);
    if c.primaries.iter().any(|p| zero(p.x) || zero(p.y)) || zero(c.white.x) || zero(c.white.y) {
        return Err(format!(
            "EDID for {device_id} contains bogus Color Characteristics"
        ));
    }
    if c.gamma + f64::from(f32::EPSILON) < 1.0 || c.gamma > 4.0 {
        return Err(format!(
            "EDID for {device_id} contains bogus Display Transfer Characteristics (GAMMA)"
        ));
    }
    let mut metadata = vec![
        ("Filename".to_owned(), file_path.display().to_string()),
        ("DATA_source".to_owned(), "edid".to_owned()),
        ("EDID_md5".to_owned(), md5.to_owned()),
    ];
    if let Some(product) = &identity.product {
        metadata.push(("EDID_model".to_owned(), product.clone()));
    }
    if let Some(serial) = &identity.serial {
        metadata.push(("EDID_serial".to_owned(), serial.clone()));
    }
    if let Some(vendor) = &identity.vendor {
        metadata.push(("EDID_mnft".to_owned(), vendor.clone()));
    }
    if let Some(name) = &identity.vendor_name {
        metadata.push(("EDID_manufacturer".to_owned(), name.clone()));
    }
    for (key, value) in [
        ("CMF_product", "tuna-desktop"),
        ("CMF_binary", "tuna-desktop"),
        ("CMF_version", env!("CARGO_PKG_VERSION")),
    ] {
        metadata.push((key.to_owned(), value.to_owned()));
    }
    metadata.push(("MAPPING_device_id".to_owned(), device_id.to_owned()));
    let xy = |p: crate::edid::Chromaticity| icc::Xy::new(p.x, p.y);
    Ok(icc::DisplayProfile {
        description: identity.display_name(),
        copyright: Some("This profile is free of known copyright restrictions.".to_owned()),
        manufacturer: Some(
            identity
                .vendor_name
                .clone()
                .or_else(|| identity.vendor.clone())
                .unwrap_or_else(|| "Unknown vendor".to_owned()),
        ),
        model: Some(
            identity
                .product
                .clone()
                .unwrap_or_else(|| "Unknown monitor".to_owned()),
        ),
        primaries: [xy(c.primaries[0]), xy(c.primaries[1]), xy(c.primaries[2])],
        white: xy(c.white),
        trc: icc::Trc::Gamma(c.gamma),
        vcgt: None,
        metadata,
    })
}

/// Mutter's colour store file filter (`is_file_info_icc_profile`,
/// `should_ignore_store_file`): an ICC profile by name or magic, not
/// hidden, not a backup, not a generated device profile.
pub fn is_store_profile(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    if name.starts_with('.') || name.ends_with('~') {
        return false;
    }
    if name.starts_with("edid-") || name.starts_with("device-") {
        return false;
    }
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".icc") || lower.ends_with(".icm") {
        return true;
    }
    let mut magic = [0u8; 40];
    std::fs::File::open(path)
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut magic))
        .is_ok_and(|()| &magic[36..40] == b"acsp")
}

// ---------------------------------------------------------------------
// colord client
// ---------------------------------------------------------------------

const CD_SERVICE: &str = "org.freedesktop.ColorManager";
const CD_PATH: &str = "/org/freedesktop/ColorManager";
const CD_DEVICE: &str = "org.freedesktop.ColorManager.Device";
const CD_PROFILE: &str = "org.freedesktop.ColorManager.Profile";

/// Which bus the colord client uses.
#[derive(Debug, Clone)]
pub enum Bus {
    /// The system bus (`DBUS_SYSTEM_BUS_ADDRESS` honoured).
    System,
    /// An explicit address (tests).
    Address(String),
}

enum Event {
    DeviceChanged(String),
    ColordOwner(bool),
    MonitorsChanged(u64, Vec<ColordMonitor>),
    Shutdown,
}

/// Handle to the colord thread; dropping it leaves the thread running
/// for the process lifetime (temp-scope colord objects die with the
/// connection).
pub struct ColordClient {
    events: std::sync::mpsc::Sender<Event>,
    thread: Option<std::thread::JoinHandle<()>>,
    monitors: Vec<ColordMonitor>,
}

impl ColordClient {
    /// Start registering `monitors` with colord on `bus`, profiles in
    /// `icc_dir`. Progress lands in `share`.
    pub fn start(
        share: Arc<ColorShare>,
        monitors: Vec<ColordMonitor>,
        bus: Bus,
        icc_dir: Option<PathBuf>,
    ) -> Self {
        let (events, rx) = std::sync::mpsc::channel();
        let tx = events.clone();
        let initial = monitors.clone();
        let thread = std::thread::Builder::new()
            .name("tuna-colord".into())
            .spawn(move || run(share, monitors, bus, icc_dir, rx, tx))
            .ok();
        Self {
            events,
            thread,
            monitors: initial,
        }
    }

    /// Reconcile actual connector discovery without disturbing unchanged
    /// colord assignments or recreating the worker on every layout change.
    pub fn update_monitors(&mut self, share: &ColorShare, monitors: Vec<ColordMonitor>) {
        if self.monitors == monitors {
            return;
        }
        let unchanged: Vec<_> = monitors
            .iter()
            .filter(|monitor| self.monitors.iter().any(|old| old.same_device(monitor)))
            .map(|monitor| monitor.identity.connector.as_str())
            .collect();
        let generation = share.begin_monitor_update(&monitors, &unchanged);
        let _ = self
            .events
            .send(Event::MonitorsChanged(generation, monitors.clone()));
        self.monitors = monitors;
    }

    /// Stop the thread, deleting what it registered (`meta_color_device_dispose`).
    pub fn shutdown(mut self) {
        let _ = self.events.send(Event::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Registered {
    monitor: ColordMonitor,
    device_path: Option<String>,
}

struct Colord {
    conn: zbus::blocking::Connection,
    share: Arc<ColorShare>,
    icc_dir: Option<PathBuf>,
    /// Profiles this client created: id → object path.
    owned_profiles: HashMap<String, String>,
    /// Store files already registered, with (len, mtime) when seen.
    store_seen: HashMap<PathBuf, (u64, Option<std::time::SystemTime>)>,
    devices: Vec<Registered>,
    monitor_generation: u64,
}

fn error_name(e: &zbus::Error) -> Option<&str> {
    match e {
        zbus::Error::MethodError(name, _, _) => Some(name.as_str()),
        _ => None,
    }
}

fn is_already_exists(e: &zbus::Error) -> bool {
    error_name(e).is_some_and(|n| n.ends_with(".AlreadyExists"))
}

fn run(
    share: Arc<ColorShare>,
    monitors: Vec<ColordMonitor>,
    bus: Bus,
    icc_dir: Option<PathBuf>,
    rx: std::sync::mpsc::Receiver<Event>,
    tx: std::sync::mpsc::Sender<Event>,
) {
    let builder = match &bus {
        Bus::System => zbus::blocking::connection::Builder::system(),
        Bus::Address(address) => zbus::blocking::connection::Builder::address(address.as_str()),
    };
    let conn = match builder.and_then(|b| b.method_timeout(Duration::from_secs(15)).build()) {
        Ok(conn) => conn,
        Err(e) => {
            eprintln!("tuna-compositor: colour: no system bus for colord: {e}");
            share.set_status(ColordStatus::Unavailable(e.to_string()));
            return;
        }
    };
    // cd_client_get_has_server: colord activates on its first call.
    if let Err(e) = conn.call_method(
        Some(CD_SERVICE),
        CD_PATH,
        Some(CD_SERVICE),
        "GetDevices",
        &(),
    ) {
        eprintln!("tuna-compositor: colour: There is no colord server available: {e}");
        share.set_status(ColordStatus::Unavailable(e.to_string()));
        return;
    }
    share.set_status(ColordStatus::Connected);
    spawn_signal_forwarders(&conn, tx);
    let mut colord = Colord {
        conn,
        share,
        icc_dir,
        owned_profiles: HashMap::new(),
        store_seen: HashMap::new(),
        monitor_generation: 0,
        devices: monitors
            .into_iter()
            .map(|monitor| Registered {
                monitor,
                device_path: None,
            })
            .collect(),
    };
    colord.publish_initial();
    if let Some(dir) = &colord.icc_dir {
        if let Err(e) = std::fs::create_dir_all(dir) {
            eprintln!("tuna-compositor: colour: Failed to monitor ICC directory: {e}");
        }
    }
    colord.scan_store();
    colord.register_all();
    loop {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(Event::DeviceChanged(path)) => colord.device_changed(&path),
            Ok(Event::MonitorsChanged(generation, monitors)) => {
                colord.update_monitors(generation, monitors)
            }
            Ok(Event::ColordOwner(true)) => {
                // colord restarted: every temp-scope object is gone.
                eprintln!("tuna-compositor: colour: colord restarted, registering again");
                colord.owned_profiles.clear();
                colord.store_seen.clear();
                colord.publish_initial();
                colord.scan_store();
                colord.register_all();
            }
            Ok(Event::ColordOwner(false)) => {
                colord
                    .share
                    .set_status(ColordStatus::Unavailable("colord left the bus".into()));
            }
            Ok(Event::Shutdown) => {
                colord.delete_all();
                return;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => colord.scan_store(),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn spawn_signal_forwarders(conn: &zbus::blocking::Connection, tx: std::sync::mpsc::Sender<Event>) {
    let changed = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender(CD_SERVICE)
        .and_then(|b| b.interface(CD_SERVICE))
        .and_then(|b| b.member("DeviceChanged"))
        .map(|b| b.build());
    if let Ok(rule) = changed {
        if let Ok(iter) = zbus::blocking::MessageIterator::for_match_rule(rule, conn, Some(64)) {
            let tx = tx.clone();
            let _ = std::thread::Builder::new()
                .name("tuna-colord-signals".into())
                .spawn(move || {
                    for message in iter.flatten() {
                        if let Ok(path) = message
                            .body()
                            .deserialize::<zbus::zvariant::OwnedObjectPath>()
                        {
                            if tx.send(Event::DeviceChanged(path.to_string())).is_err() {
                                return;
                            }
                        }
                    }
                });
        }
    }
    if let Ok(proxy) = zbus::blocking::fdo::DBusProxy::new(conn) {
        if let Ok(stream) = proxy.receive_name_owner_changed_with_args(&[(0, CD_SERVICE)]) {
            let _ = std::thread::Builder::new()
                .name("tuna-colord-owner".into())
                .spawn(move || {
                    for signal in stream {
                        let Ok(args) = signal.args() else { continue };
                        let owned = args.new_owner().is_some();
                        if tx.send(Event::ColordOwner(owned)).is_err() {
                            return;
                        }
                    }
                });
        }
    }
}

impl Colord {
    fn publish_initial(&self) {
        self.share.set_devices(
            self.monitor_generation,
            self.devices
                .iter()
                .map(|r| DeviceSnapshot {
                    connector: r.monitor.identity.connector.clone(),
                    device_id: r.monitor.identity.colord_device_id(),
                    ..Default::default()
                })
                .collect(),
        );
    }

    fn update_monitors(&mut self, generation: u64, monitors: Vec<ColordMonitor>) {
        if self.share.monitor_generation() != generation {
            return;
        }
        self.monitor_generation = generation;
        let mut old = std::mem::take(&mut self.devices);
        let old_snapshots = self.share.devices();
        let mut snapshots = Vec::with_capacity(monitors.len());
        for monitor in monitors {
            let connector = &monitor.identity.connector;
            if let Some(index) = old
                .iter()
                .position(|registered| registered.monitor.same_device(&monitor))
            {
                let mut registered = old.remove(index);
                let snapshot = old_snapshots
                    .iter()
                    .find(|snapshot| &snapshot.connector == connector);
                if snapshot.is_none() {
                    // Discovery can revoke a sink while an older event
                    // is still queued. Re-read its existing colord
                    // registration rather than publishing an unready
                    // placeholder indefinitely.
                    registered.device_path = None;
                }
                snapshots.push(snapshot.cloned().unwrap_or_else(|| DeviceSnapshot {
                    connector: connector.clone(),
                    device_id: monitor.identity.colord_device_id(),
                    ..Default::default()
                }));
                registered.monitor = monitor;
                self.devices.push(registered);
            } else {
                snapshots.push(DeviceSnapshot {
                    connector: connector.clone(),
                    device_id: monitor.identity.colord_device_id(),
                    ..Default::default()
                });
                self.devices.push(Registered {
                    monitor,
                    device_path: None,
                });
            }
        }
        // Remove old registrations before registering a replacement with
        // the same connector/device id. The persistent colord mapping is
        // retained by colord, just as it is across a compositor restart.
        for registered in old {
            if let Some(path) = registered.device_path {
                if let Ok(object) = zbus::zvariant::ObjectPath::try_from(path.as_str()) {
                    let _ = self.call(CD_PATH, CD_SERVICE, "DeleteDevice", &(object,));
                }
            }
        }
        self.share.set_devices(self.monitor_generation, snapshots);
        for index in 0..self.devices.len() {
            if self.devices[index].device_path.is_none() {
                self.register(index);
            }
        }
    }

    fn call<B>(
        &self,
        path: &str,
        iface: &str,
        method: &str,
        body: &B,
    ) -> zbus::Result<zbus::message::Message>
    where
        B: serde::Serialize + zbus::zvariant::DynamicType,
    {
        self.conn
            .call_method(Some(CD_SERVICE), path, Some(iface), method, body)
    }

    fn object_path(
        &self,
        path: &str,
        iface: &str,
        method: &str,
        arg: &str,
    ) -> zbus::Result<String> {
        let reply = self.call(path, iface, method, &(arg,))?;
        Ok(reply
            .body()
            .deserialize::<zbus::zvariant::OwnedObjectPath>()?
            .to_string())
    }

    fn property<T>(&self, path: &str, iface: &str, name: &str) -> Option<T>
    where
        T: TryFrom<zbus::zvariant::OwnedValue>,
    {
        let reply = self
            .conn
            .call_method(
                Some(CD_SERVICE),
                path,
                Some("org.freedesktop.DBus.Properties"),
                "Get",
                &(iface, name),
            )
            .ok()?;
        let value: zbus::zvariant::OwnedValue = reply.body().deserialize().ok()?;
        T::try_from(value).ok()
    }

    fn register_all(&mut self) {
        for i in 0..self.devices.len() {
            self.register(i);
        }
    }

    /// `create_cd_device` → `setup_created_cd_device` → profile and
    /// assignment.
    fn register(&mut self, index: usize) {
        let monitor = self.devices[index].monitor.clone();
        let identity = &monitor.identity;
        let connector = identity.connector.clone();
        let device_id = identity.colord_device_id();
        let props = device_properties(identity);
        let created = self
            .call(
                CD_PATH,
                CD_SERVICE,
                "CreateDevice",
                &(device_id.as_str(), "temp", &props),
            )
            .and_then(|reply| {
                Ok(reply
                    .body()
                    .deserialize::<zbus::zvariant::OwnedObjectPath>()?
                    .to_string())
            });
        let path = match created {
            Ok(path) => path,
            Err(e) if is_already_exists(&e) => {
                match self.object_path(CD_PATH, CD_SERVICE, "FindDeviceById", &device_id) {
                    Ok(path) => path,
                    Err(e) => {
                        self.fail(
                            &connector,
                            format!("Failed to find existing colord device for '{device_id}': {e}"),
                        );
                        return;
                    }
                }
            }
            Err(e) => {
                self.fail(
                    &connector,
                    format!("Failed to create colord device for '{device_id}': {e}"),
                );
                return;
            }
        };
        self.devices[index].device_path = Some(path.clone());
        self.share
            .update_device(self.monitor_generation, &connector, |d| {
                d.object_path = Some(path.clone())
            });
        let device_profile = self.ensure_device_profile(&monitor);
        let error = device_profile.as_ref().err().cloned();
        self.share
            .update_device(self.monitor_generation, &connector, |d| {
                d.device_profile = device_profile.ok().flatten();
                d.error = error;
            });
        self.update_assigned(index);
        self.share
            .update_device(self.monitor_generation, &connector, |d| d.ready = true);
        eprintln!("tuna-compositor: colour: device {device_id} ({connector}) registered at {path}");
    }

    fn fail(&self, connector: &str, error: String) {
        eprintln!("tuna-compositor: colour: {error}");
        // notify_ready (FALSE) still marks the device ready.
        self.share
            .update_device(self.monitor_generation, connector, |d| {
                d.error = Some(error);
                d.ready = true;
            });
    }

    /// `meta_color_store_ensure_device_profile`: generate, save and
    /// register the device profile.
    fn ensure_device_profile(
        &mut self,
        monitor: &ColordMonitor,
    ) -> Result<Option<ProfileInfo>, String> {
        let Some(dir) = self.icc_dir.clone() else {
            return Ok(None);
        };
        let Some(name) = device_profile_file_name(monitor) else {
            return Ok(None);
        };
        let path = dir.join(name);
        let Some(profile) = device_profile(monitor, &path) else {
            return Ok(None);
        };
        let bytes = profile?.encode().map_err(|e| e.to_string())?;
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("Failed to ensure data directory: {e}"))?;
        let tmp = path.with_extension("icc.tmp");
        std::fs::write(&tmp, &bytes)
            .and_then(|()| std::fs::rename(&tmp, &path))
            .map_err(|e| format!("Failed to write ICC profile to {}: {e}", path.display()))?;
        self.create_profile(&path, &bytes).map(Some)
    }

    /// `create_cd_profile`: `CreateProfileWithFd` as `icc-<md5>`.
    fn create_profile(&mut self, path: &Path, bytes: &[u8]) -> Result<ProfileInfo, String> {
        let checksum = crate::edid::md5_hex(bytes);
        let id = format!("icc-{checksum}");
        let filename = path.display().to_string();
        let file =
            std::fs::File::open(path).map_err(|e| format!("Failed to open {filename}: {e}"))?;
        let mut props: HashMap<&str, &str> = HashMap::new();
        props.insert("Filename", &filename);
        props.insert("FILE_checksum", &checksum);
        let fd = zbus::zvariant::Fd::from(std::os::fd::AsFd::as_fd(&file));
        let created = self
            .call(
                CD_PATH,
                CD_SERVICE,
                "CreateProfileWithFd",
                &(id.as_str(), "temp", fd, &props),
            )
            .and_then(|reply| {
                Ok(reply
                    .body()
                    .deserialize::<zbus::zvariant::OwnedObjectPath>()?
                    .to_string())
            });
        let object_path = match created {
            Ok(object_path) => {
                self.owned_profiles.insert(id.clone(), object_path.clone());
                object_path
            }
            Err(e) if is_already_exists(&e) => self
                .object_path(CD_PATH, CD_SERVICE, "FindProfileById", &id)
                .map_err(|e| format!("Failed to find colord profile {id}: {e}"))?,
            Err(e) => return Err(format!("Failed to create colord color profile {id}: {e}")),
        };
        Ok(ProfileInfo {
            title: self.property::<String>(&object_path, CD_PROFILE, "Title"),
            id,
            object_path,
            filename: Some(filename),
        })
    }

    /// Scan the ICC directory for profiles not yet registered.
    fn scan_store(&mut self) {
        let Some(dir) = self.icc_dir.clone() else {
            return;
        };
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            // Symlinks register their target, as Mutter's store does.
            let target = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
            let Ok(meta) = std::fs::metadata(&target) else {
                continue;
            };
            if !meta.is_file() || !is_store_profile(&path) {
                continue;
            }
            let stamp = (meta.len(), meta.modified().ok());
            if self.store_seen.get(&target) == Some(&stamp) {
                continue;
            }
            let Ok(bytes) = std::fs::read(&target) else {
                continue;
            };
            self.store_seen.insert(target.clone(), stamp);
            if let Err(e) = icc::Profile::parse(&bytes) {
                eprintln!(
                    "tuna-compositor: colour: Failed to parse ICC profile '{}': {e}",
                    target.display()
                );
                continue;
            }
            match self.create_profile(&target, &bytes) {
                Ok(profile) => eprintln!(
                    "tuna-compositor: colour: Created colord profile '{}' from '{}'",
                    profile.id,
                    target.display()
                ),
                Err(e) => eprintln!("tuna-compositor: colour: {e}"),
            }
        }
    }

    fn device_changed(&mut self, path: &str) {
        if let Some(index) = self
            .devices
            .iter()
            .position(|r| r.device_path.as_deref() == Some(path))
        {
            self.update_assigned(index);
        }
    }

    /// `update_assigned_profile`: the default (first) colord profile,
    /// loaded with its calibration.
    fn update_assigned(&mut self, index: usize) {
        let Some(path) = self.devices[index].device_path.clone() else {
            return;
        };
        let connector = self.devices[index].monitor.identity.connector.clone();
        let profiles: Vec<zbus::zvariant::OwnedObjectPath> = self
            .property(&path, CD_DEVICE, "Profiles")
            .unwrap_or_default();
        let Some(default) = profiles.first().map(|p| p.to_string()) else {
            self.share
                .update_device(self.monitor_generation, &connector, |d| {
                    d.assigned = None;
                    d.calibration = None;
                });
            return;
        };
        if self
            .share
            .device(&connector)
            .and_then(|d| d.assigned)
            .is_some_and(|a| a.object_path == default)
        {
            return;
        }
        let filename: Option<String> = self
            .property::<String>(&default, CD_PROFILE, "Filename")
            .filter(|f| !f.is_empty());
        let id: String = self
            .property(&default, CD_PROFILE, "ProfileId")
            .unwrap_or_default();
        let title: Option<String> = self.property(&default, CD_PROFILE, "Title");
        let info = ProfileInfo {
            id: id.clone(),
            object_path: default.clone(),
            filename: filename.clone(),
            title,
        };
        // ensure_colord_profile: a non-local profile cannot be assigned.
        let loaded = match &filename {
            None => Err("Tried to assign non-local profile".to_owned()),
            Some(file) => std::fs::read(file)
                .map_err(|e| format!("{file}: {e}"))
                .and_then(|bytes| icc::Profile::parse(&bytes).map_err(|e| format!("{file}: {e}"))),
        };
        match loaded {
            Ok(profile) => {
                let calibration = profile.vcgt().map(Arc::new);
                eprintln!(
                    "tuna-compositor: colour: {connector} assigned profile {id} ({}){}",
                    filename.as_deref().unwrap_or(""),
                    if calibration.is_some() {
                        " with vcgt"
                    } else {
                        ""
                    }
                );
                self.share
                    .update_device(self.monitor_generation, &connector, |d| {
                        d.assigned = Some(info);
                        d.calibration = calibration;
                    });
            }
            Err(e) => {
                // ensure_default_profile_cb: the warning keeps a NULL
                // assigned profile.
                eprintln!("tuna-compositor: colour: Failed to create color profile from colord profile: {e}");
                self.share
                    .update_device(self.monitor_generation, &connector, |d| {
                        d.assigned = None;
                        d.calibration = None;
                        d.error = Some(e);
                    });
            }
        }
    }

    fn delete_all(&mut self) {
        for r in &self.devices {
            if let Some(path) = &r.device_path {
                let object = zbus::zvariant::ObjectPath::try_from(path.as_str());
                if let Ok(object) = object {
                    let _ = self.call(CD_PATH, CD_SERVICE, "DeleteDevice", &(object,));
                }
            }
        }
        for path in self.owned_profiles.values() {
            if let Ok(object) = zbus::zvariant::ObjectPath::try_from(path.as_str()) {
                let _ = self.call(CD_PATH, CD_SERVICE, "DeleteProfile", &(object,));
            }
        }
    }
}

/// `generate_color_device_props`.
pub fn device_properties(identity: &MonitorIdentity) -> HashMap<&'static str, String> {
    let mut props = HashMap::new();
    props.insert("Kind", "display".to_owned());
    props.insert("Mode", "physical".to_owned());
    props.insert("Colorspace", "rgb".to_owned());
    // cd_client_create_device sends NULL values as "".
    props.insert(
        "Vendor",
        identity
            .vendor_name
            .clone()
            .or_else(|| identity.vendor.clone())
            .unwrap_or_default(),
    );
    props.insert("Model", identity.product.clone().unwrap_or_default());
    props.insert("Serial", identity.serial.clone().unwrap_or_default());
    props.insert("XRANDR_name", identity.connector.clone());
    props.insert(
        "OutputPriority",
        if identity.primary {
            "primary"
        } else {
            "secondary"
        }
        .to_owned(),
    );
    if let Some(md5) = &identity.edid_md5 {
        props.insert("OutputEdidMd5", md5.clone());
    }
    if identity.builtin {
        props.insert("Embedded", String::new());
    }
    props
}

// ---------------------------------------------------------------------
// org.gnome.Mutter.ColorManager
// ---------------------------------------------------------------------

/// `/org/gnome/Mutter/ColorManager`.
pub struct MutterColorManager {
    pub share: Arc<ColorShare>,
}

#[zbus::interface(name = "org.gnome.Mutter.ColorManager")]
impl MutterColorManager {
    /// `handle_calibrate_monitor`.
    async fn calibrate_monitor(
        &self,
        connector: String,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(object_server)] server: &zbus::ObjectServer,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> zbus::fdo::Result<zbus::zvariant::OwnedObjectPath> {
        let owner = header
            .sender()
            .map(|s| s.to_string())
            .ok_or_else(|| zbus::fdo::Error::Failed("no sender".into()))?;
        let session = self
            .share
            .start_calibration(&connector, &owner)
            .map_err(|e| {
                if e.starts_with("Unknown monitor") {
                    zbus::fdo::Error::InvalidArgs(e)
                } else {
                    zbus::fdo::Error::Failed(e)
                }
            })?;
        let path = zbus::zvariant::OwnedObjectPath::try_from(session.object_path.clone())
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        let object = CalibrationObject {
            share: self.share.clone(),
            object_path: session.object_path.clone(),
            gamma_lut_size: self.share.gamma_size(&connector) as u32,
        };
        if let Err(e) = server.at(&path, object).await {
            self.share.stop_calibration(&session.object_path);
            return Err(zbus::fdo::Error::Failed(e.to_string()));
        }
        watch_owner(conn.clone(), self.share.clone(), owner);
        Ok(path)
    }
}

/// Close `owner`'s sessions when it leaves the bus
/// (`meta_dbus_session_watcher`).
fn watch_owner(conn: zbus::Connection, share: Arc<ColorShare>, owner: String) {
    let _ = std::thread::Builder::new()
        .name("tuna-calibration-owner".into())
        .spawn(move || {
            let conn = zbus::blocking::Connection::from(conn);
            let Ok(proxy) = zbus::blocking::fdo::DBusProxy::new(&conn) else {
                return;
            };
            let Ok(stream) = proxy.receive_name_owner_changed_with_args(&[(0, owner.as_str())])
            else {
                return;
            };
            // The caller may already be gone.
            let gone = |proxy: &zbus::blocking::fdo::DBusProxy<'_>| {
                zbus::names::BusName::try_from(owner.as_str())
                    .ok()
                    .is_some_and(|name| !proxy.name_has_owner(name).unwrap_or(false))
            };
            let close = |share: &ColorShare| {
                for path in share.owner_vanished(&owner) {
                    if let Ok(path) = zbus::zvariant::ObjectPath::try_from(path.as_str()) {
                        let _ = conn.object_server().remove::<CalibrationObject, _>(path);
                    }
                }
            };
            if gone(&proxy) {
                close(&share);
                return;
            }
            for signal in stream {
                if signal.args().is_ok_and(|a| a.new_owner().is_none()) {
                    eprintln!(
                        "tuna-compositor: colour: D-Bus client with active sessions vanished"
                    );
                    close(&share);
                    return;
                }
                if share.sessions().iter().all(|s| s.owner != owner) {
                    return;
                }
            }
        });
}

/// One calibration session object.
pub struct CalibrationObject {
    share: Arc<ColorShare>,
    object_path: String,
    gamma_lut_size: u32,
}

#[zbus::interface(name = "org.gnome.Mutter.ColorManager.Calibration")]
impl CalibrationObject {
    /// `handle_set_crtc_gamma_lut`.
    fn set_crtc_gamma_lut(
        &self,
        red: Vec<u16>,
        green: Vec<u16>,
        blue: Vec<u16>,
    ) -> zbus::fdo::Result<()> {
        if red.len() != green.len() || red.len() != blue.len() {
            return Err(zbus::fdo::Error::InvalidArgs(
                "gamma channels differ in length".into(),
            ));
        }
        self.share
            .set_calibration_lut(&self.object_path, GammaLut { red, green, blue })
            .map_err(zbus::fdo::Error::Failed)
    }

    /// `handle_stop`: close the session.
    async fn stop(
        &self,
        #[zbus(object_server)] server: &zbus::ObjectServer,
    ) -> zbus::fdo::Result<()> {
        self.share.stop_calibration(&self.object_path);
        if let Ok(path) = zbus::zvariant::ObjectPath::try_from(self.object_path.as_str()) {
            let _ = server.remove::<CalibrationObject, _>(path).await;
        }
        Ok(())
    }

    #[zbus(property)]
    fn gamma_lut_size(&self) -> u32 {
        self.gamma_lut_size
    }

    #[zbus(signal)]
    async fn stopped(emitter: &zbus::object_server::SignalEmitter<'_>) -> zbus::Result<()>;
}

#[cfg(test)]
mod tests;
