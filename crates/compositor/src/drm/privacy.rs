//! KMS privacy screen properties of a connector (#356): discovery of
//! `privacy-screen sw-state`/`hw-state`, re-reading hw-state after a
//! udev change, and the sw-state write. The policy is
//! [`crate::privacy_screen`]; mutter 51 programs the property in its
//! frame's KMS update (`meta-onscreen-native.c`,
//! `meta_kms_update_set_privacy_screen`), the backend here sets it
//! directly, as it sets `GAMMA_LUT` (`drm/color.rs`).

use smithay::backend::drm::DrmDevice;
use smithay::reexports::drm::control::{connector, property, Device as ControlDevice};

use crate::privacy_screen::{EnumProperty, KmsPrivacyScreen, PrivacyOutput, ScreenState};

pub type PrivacyScreen = KmsPrivacyScreen<property::Handle>;

/// The connector's enum properties, with their entries and values.
pub fn discover(drm: &DrmDevice, connector: connector::Handle) -> PrivacyScreen {
    let Ok(props) = drm.get_properties(connector) else {
        return PrivacyScreen::default();
    };
    let (handles, values) = props.as_props_and_values();
    let properties = handles
        .iter()
        .zip(values)
        .filter_map(|(&handle, &value)| {
            let info = drm.get_property(handle).ok()?;
            let property::ValueType::Enum(entries) = info.value_type() else {
                return None;
            };
            let (_, entries) = entries.values();
            Some(EnumProperty {
                handle,
                name: info.name().to_str().ok()?.to_owned(),
                entries: entries
                    .iter()
                    .filter_map(|e| Some((e.value(), e.name().to_str().ok()?.to_owned())))
                    .collect(),
                value,
            })
        })
        .collect();
    PrivacyScreen::discover(properties)
}

/// Re-read hw-state; returns whether the state changed.
pub fn refresh(drm: &DrmDevice, connector: connector::Handle, screen: &mut PrivacyScreen) -> bool {
    let Some(hw) = screen.hw_handle() else {
        return false;
    };
    let Ok(props) = drm.get_properties(connector) else {
        return false;
    };
    let (handles, values) = props.as_props_and_values();
    match handles.iter().position(|&h| h == hw) {
        Some(index) => screen.refresh(values[index]),
        None => false,
    }
}

/// One output as the privacy screen policy drives it.
pub struct Target<'a> {
    pub drm: &'a DrmDevice,
    pub name: &'a str,
    pub connector: connector::Handle,
    pub active: bool,
    pub screen: &'a mut PrivacyScreen,
}

impl PrivacyOutput for Target<'_> {
    fn label(&self) -> &str {
        self.name
    }

    fn active(&self) -> bool {
        self.active
    }

    fn privacy_state(&self) -> Option<ScreenState> {
        self.screen.state()
    }

    fn switch_privacy(&mut self, enabled: bool) -> Result<(), String> {
        let (drm, connector) = (self.drm, self.connector);
        self.screen.switch(enabled, |handle, value| {
            drm.set_property(connector, handle, value)
                .map_err(|e| format!("setting privacy-screen sw-state: {e}"))
        })
    }
}
