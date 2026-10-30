//! DRM leasing (`wp_drm_lease_device_v1` version 1), as Mutter 51 offers
//! it on its native backend (meta-wayland-drm-lease.c over
//! meta-drm-lease.c).
//!
//! - One lease device global for the session's DRM device. It is
//!   advertised even when nothing is leasable, as Mutter creates a global
//!   per KMS device before looking at connectors.
//! - A connector is for lease when it is connected and either carries the
//!   kernel's `non-desktop` property (VR headsets) or is named in
//!   [`DEBUG_LEASE_CONNECTORS_ENV`], a colon-separated list of connector
//!   names (Mutter's `meta_kms_connector_is_non_desktop`). Such a connector
//!   never becomes a desktop output (`meta_gpu_kms_read_current`).
//! - A lease takes, per connector, the first CRTC that drives no desktop
//!   output and no other lease, its primary plane and, when there is one,
//!   its cursor plane (`find_resources_to_lease`); otherwise the client
//!   gets `finished`.
//! - Leased connectors are withdrawn from every client and offered again
//!   when the lease ends: the client destroys it, closes the lease fd (the
//!   kernel destroys the lessee; `update_leases`), or the connector goes
//!   away (`update_connectors`). A paused session withdraws every connector
//!   and ends every lease (`meta_drm_lease_manager_pause`); activation
//!   offers them again.

use smithay::backend::drm::{DrmDevice, DrmDeviceFd, DrmNode};
use smithay::reexports::drm::control::{
    connector, crtc, plane, Device as ControlDevice, PlaneType, ResourceHandle,
};
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::wayland::drm_lease::{
    DrmLease, DrmLeaseBuilder, DrmLeaseHandler, DrmLeaseRequest, DrmLeaseState, LeaseRejected,
};

use crate::State;

/// Mutter's `MUTTER_DEBUG_LEASE_CONNECTORS` under Tuna's prefix:
/// connectors named here (colon-separated) are offered for lease and kept
/// off the desktop. Only this name is read: a value set for GNOME in a
/// shared session environment must not take a monitor away from Tuna.
pub const DEBUG_LEASE_CONNECTORS_ENV: &str = "TUNA_DEBUG_LEASE_CONNECTORS";

/// Connector names from a [`DEBUG_LEASE_CONNECTORS_ENV`] value, split on
/// `:` as Mutter's `g_strsplit`; unset or empty names nothing.
pub fn parse_lease_connectors(raw: Option<&str>) -> Vec<String> {
    match raw {
        Some(raw) if !raw.is_empty() => raw.split(':').map(str::to_owned).collect(),
        _ => Vec::new(),
    }
}

/// The connectors [`DEBUG_LEASE_CONNECTORS_ENV`] names in this process.
pub fn debug_lease_connectors() -> Vec<String> {
    parse_lease_connectors(std::env::var(DEBUG_LEASE_CONNECTORS_ENV).ok().as_deref())
}

/// Mutter's `meta_kms_connector_is_non_desktop`: named in the debug list,
/// or the kernel marks the connector `non-desktop`.
pub fn is_for_lease(name: &str, non_desktop: bool, debug_names: &[String]) -> bool {
    debug_names.iter().any(|n| n == name) || non_desktop
}

/// A connector's name as Mutter and GNOME Settings show it (`HDMI-A-1`).
pub fn connector_name(info: &connector::Info) -> String {
    format!("{}-{}", info.interface().as_str(), info.interface_id())
}

/// Value of a named KMS property of an object.
fn property(dev: &impl ControlDevice, handle: impl ResourceHandle, name: &str) -> Option<u64> {
    let props = dev.get_properties(handle).ok()?;
    let (handles, values) = props.as_props_and_values();
    handles
        .iter()
        .zip(values)
        .find(|(prop, _)| {
            dev.get_property(**prop)
                .is_ok_and(|info| info.name().to_str() == Ok(name))
        })
        .map(|(_, value)| *value)
}

/// The kernel's `non-desktop` connector property (VR headsets).
pub fn non_desktop(dev: &impl ControlDevice, connector: connector::Handle) -> bool {
    property(dev, connector, "non-desktop").is_some_and(|value| value != 0)
}

/// Mutter's `get_connector_description`: the EDID vendor and product
/// name, else the connector name; empty without an EDID.
pub fn description(edid: Option<&[u8]>, name: &str) -> String {
    let Some(edid) = edid else {
        return String::new();
    };
    let Some(info) = crate::edid::parse(edid) else {
        return name.to_owned();
    };
    let parts: Vec<&str> = [
        Some(info.manufacturer.as_str()),
        info.product_name.as_deref(),
    ]
    .into_iter()
    .flatten()
    .filter(|part| !part.is_empty())
    .collect();
    if parts.is_empty() {
        name.to_owned()
    } else {
        parts.join(" ")
    }
}

/// A connector offered for lease.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseConnector {
    pub handle: connector::Handle,
    pub name: String,
    pub description: String,
}

/// The device's connectors for lease, in the kernel's order.
pub fn leasable_connectors(
    dev: &impl ControlDevice,
    debug_names: &[String],
) -> Vec<LeaseConnector> {
    let Ok(resources) = dev.resource_handles() else {
        return Vec::new();
    };
    resources
        .connectors()
        .iter()
        .filter_map(|&handle| {
            let info = dev.get_connector(handle, false).ok()?;
            // Mutter keeps no connector state for a disconnected one.
            if info.state() == connector::State::Disconnected {
                return None;
            }
            let name = connector_name(&info);
            if !is_for_lease(&name, non_desktop(dev, handle), debug_names) {
                return None;
            }
            let edid = super::color::connector_edid(dev, handle);
            Some(LeaseConnector {
                handle,
                description: description(edid.as_deref(), &name),
                name,
            })
        })
        .collect()
}

/// The lease device global and its leases.
pub struct LeaseDevice {
    state: DrmLeaseState,
    fd: DrmDeviceFd,
    /// An empty builder sharing the device's plane claims.
    template: DrmLeaseBuilder,
    /// CRTCs and primary planes the desktop outputs own.
    desktop_crtcs: Vec<crtc::Handle>,
    desktop_planes: Vec<plane::Handle>,
    debug_names: Vec<String>,
    /// Connectors registered with the global, offered or leased.
    connectors: Vec<connector::Handle>,
    leases: Vec<DrmLease>,
    paused: bool,
}

impl std::fmt::Debug for LeaseDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LeaseDevice")
            .field("connectors", &self.connectors)
            .field("leases", &self.leases.len())
            .field("paused", &self.paused)
            .finish()
    }
}

impl LeaseDevice {
    /// Advertise the global for `drm` and offer its connectors for lease.
    pub fn new(
        dh: &DisplayHandle,
        drm: &DrmDevice,
        desktop_crtcs: Vec<crtc::Handle>,
        desktop_planes: Vec<plane::Handle>,
        debug_names: Vec<String>,
    ) -> Result<Self, String> {
        let node = DrmNode::from_dev_id(drm.device_id()).map_err(|e| e.to_string())?;
        let state = DrmLeaseState::new::<State>(dh, &node).map_err(|e| e.to_string())?;
        let mut device = Self {
            state,
            fd: drm.device_fd().clone(),
            template: DrmLeaseBuilder::new(drm),
            desktop_crtcs,
            desktop_planes,
            debug_names,
            connectors: Vec::new(),
            leases: Vec::new(),
            paused: false,
        };
        device.update_connectors();
        Ok(device)
    }

    /// How many connectors are registered with the global.
    pub fn connector_count(&self) -> usize {
        self.connectors.len()
    }

    /// Mutter's `update_connectors`: offer new connectors for lease,
    /// withdraw vanished ones and end the leases holding them.
    pub fn update_connectors(&mut self) {
        let wanted = if self.paused {
            Vec::new()
        } else {
            leasable_connectors(&self.fd, &self.debug_names)
        };
        let gone: Vec<connector::Handle> = self
            .connectors
            .iter()
            .copied()
            .filter(|handle| !wanted.iter().any(|c| c.handle == *handle))
            .collect();
        let ended: Vec<u32> = self
            .leases
            .iter()
            .filter(|lease| lease.connectors().any(|c| gone.contains(c)))
            .map(DrmLease::id)
            .collect();
        for id in ended {
            self.end_lease(id);
        }
        for handle in gone {
            eprintln!("tuna-compositor: drm-lease: withdrew connector {handle:?}");
            self.state.withdraw_connector(handle);
            self.connectors.retain(|c| *c != handle);
        }
        for connector in wanted {
            if self.connectors.contains(&connector.handle) {
                continue;
            }
            eprintln!(
                "tuna-compositor: drm-lease: offering connector {} ({:?})",
                connector.name, connector.description
            );
            self.connectors.push(connector.handle);
            self.state.add_connector::<State>(
                connector.handle,
                connector.name,
                connector.description,
            );
        }
    }

    /// Mutter's `update_leases`: a lessee the kernel no longer lists (its
    /// client closed the lease fd) ends with `finished`.
    pub fn update_leases(&mut self) {
        if self.leases.is_empty() {
            return;
        }
        let Ok(lessees) = self.fd.list_lessees() else {
            return;
        };
        let ended: Vec<u32> = self
            .leases
            .iter()
            .map(DrmLease::id)
            .filter(|id| !lessees.iter().any(|lessee| lessee.get() == *id))
            .collect();
        for id in ended {
            eprintln!("tuna-compositor: drm-lease: lessee {id} disappeared");
            self.end_lease(id);
        }
    }

    /// The device changed (udev): hotplug and lessee changes.
    pub fn device_changed(&mut self) {
        self.update_leases();
        self.update_connectors();
    }

    /// Session paused: withdraw every connector, end every lease.
    pub fn pause(&mut self) {
        self.paused = true;
        self.update_connectors();
        let ended: Vec<u32> = self.leases.iter().map(DrmLease::id).collect();
        for id in ended {
            self.end_lease(id);
        }
    }

    /// Session active again: offer the connectors for lease again.
    pub fn resume(&mut self) {
        self.paused = false;
        self.update_connectors();
        // Bindings that could not open the card get their DRM fd now
        // (Mutter's on_active_session_changed).
        self.state.retry_pending_binds::<State>();
    }

    fn end_lease(&mut self, id: u32) {
        self.state.revoke_lease::<State>(id);
        self.leases.retain(|lease| lease.id() != id);
    }

    /// Mutter's `find_resources_to_lease`.
    fn lease_resources(&self, request: &DrmLeaseRequest) -> Result<DrmLeaseBuilder, String> {
        if self.paused {
            return Err("the session is paused".to_owned());
        }
        let resources = self.fd.resource_handles().map_err(|e| e.to_string())?;
        let planes = self.fd.plane_handles().map_err(|e| e.to_string())?;
        let mut taken: Vec<crtc::Handle> = self
            .leases
            .iter()
            .flat_map(|lease| lease.crtcs().copied())
            .collect();
        let mut builder = self.template.clone();
        for &handle in &request.connectors {
            if !self.connectors.contains(&handle) {
                return Err(format!("connector {handle:?} is not for lease"));
            }
            let info = self
                .fd
                .get_connector(handle, false)
                .map_err(|e| e.to_string())?;
            let possible: Vec<crtc::Handle> = info
                .encoders()
                .iter()
                .filter_map(|encoder| self.fd.get_encoder(*encoder).ok())
                .flat_map(|encoder| resources.filter_crtcs(encoder.possible_crtcs()))
                .collect();
            let crtc = resources
                .crtcs()
                .iter()
                .copied()
                .find(|crtc| {
                    !self.desktop_crtcs.contains(crtc)
                        && !taken.contains(crtc)
                        && possible.contains(crtc)
                })
                .ok_or_else(|| format!("no CRTC to lease with connector {handle:?}"))?;
            let plane_for = |kind: PlaneType| {
                planes.iter().copied().find_map(|plane| {
                    if self.desktop_planes.contains(&plane)
                        || property(&self.fd, plane, "type") != Some(kind as u64)
                    {
                        return None;
                    }
                    let info = self.fd.get_plane(plane).ok()?;
                    if !resources
                        .filter_crtcs(info.possible_crtcs())
                        .contains(&crtc)
                    {
                        return None;
                    }
                    builder.claim_plane(plane, crtc).map(|claim| (plane, claim))
                })
            };
            let primary = plane_for(PlaneType::Primary)
                .ok_or_else(|| format!("no primary plane to lease with connector {handle:?}"))?;
            // Mutter leases without a cursor plane when there is none.
            let cursor = plane_for(PlaneType::Cursor);
            taken.push(crtc);
            builder.add_connector(handle);
            builder.add_crtc(crtc);
            builder.add_plane(primary.0, primary.1);
            if let Some((plane, claim)) = cursor {
                builder.add_plane(plane, claim);
            }
        }
        Ok(builder)
    }
}

impl DrmLeaseHandler for State {
    fn drm_lease_state(&mut self, _node: DrmNode) -> &mut DrmLeaseState {
        &mut self
            .protocols
            .drm_lease
            .as_mut()
            .expect("lease requests come only through the lease device's global")
            .state
    }

    fn lease_request(
        &mut self,
        _node: DrmNode,
        request: DrmLeaseRequest,
    ) -> Result<DrmLeaseBuilder, LeaseRejected> {
        let Some(device) = self.protocols.drm_lease.as_ref() else {
            return Err(LeaseRejected::default());
        };
        device.lease_resources(&request).map_err(|reason| {
            eprintln!("tuna-compositor: drm-lease: lease refused: {reason}");
            LeaseRejected::default()
        })
    }

    fn new_active_lease(&mut self, _node: DrmNode, lease: DrmLease) {
        if let Some(device) = self.protocols.drm_lease.as_mut() {
            eprintln!(
                "tuna-compositor: drm-lease: lease {} of {:?}",
                lease.id(),
                lease.connectors().collect::<Vec<_>>()
            );
            device.leases.push(lease);
        }
    }

    fn lease_destroyed(&mut self, _node: DrmNode, lease_id: u32) {
        if let Some(device) = self.protocols.drm_lease.as_mut() {
            eprintln!("tuna-compositor: drm-lease: lease {lease_id} ended");
            device.leases.retain(|lease| lease.id() != lease_id);
        }
    }
}
smithay::delegate_drm_lease!(State);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutter_debug_list_syntax() {
        assert!(parse_lease_connectors(None).is_empty());
        assert!(parse_lease_connectors(Some("")).is_empty());
        assert_eq!(parse_lease_connectors(Some("Virtual-1")), ["Virtual-1"]);
        assert_eq!(
            parse_lease_connectors(Some("HDMI-A-1:DP-2")),
            ["HDMI-A-1", "DP-2"]
        );
        // g_strsplit keeps empty fields; they match no connector name.
        assert_eq!(parse_lease_connectors(Some("DP-1:")), ["DP-1", ""]);
    }

    #[test]
    fn non_desktop_connectors_are_for_lease() {
        assert!(is_for_lease("DP-1", true, &[]));
        assert!(is_for_lease("DP-1", true, &["HDMI-A-1".to_owned()]));
    }

    #[test]
    fn named_desktop_connectors_are_for_lease() {
        let names = parse_lease_connectors(Some("HDMI-A-1:Virtual-2"));
        assert!(is_for_lease("Virtual-2", false, &names));
        assert!(is_for_lease("HDMI-A-1", false, &names));
    }

    #[test]
    fn other_desktop_connectors_stay_on_the_desktop() {
        assert!(!is_for_lease("Virtual-2", false, &[]));
        let names = parse_lease_connectors(Some("HDMI-A-1:"));
        assert!(!is_for_lease("Virtual-2", false, &names));
        // Exact names only, as g_str_equal.
        assert!(!is_for_lease("HDMI-A-10", false, &names));
        assert!(!is_for_lease("hdmi-a-1", false, &names));
    }

    #[test]
    fn descriptions_follow_mutter() {
        assert_eq!(description(None, "DP-1"), "");
        assert_eq!(description(Some(&[0; 16]), "DP-1"), "DP-1");
        let mut edid = vec![0u8; 128];
        edid[..8].copy_from_slice(&[0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00]);
        // "VLV" (Valve) packed as three five-bit letters.
        let id: u16 = (22 << 10) | (12 << 5) | 22;
        edid[8..10].copy_from_slice(&id.to_be_bytes());
        edid[54..59].copy_from_slice(&[0, 0, 0, 0xfc, 0]);
        edid[59..72].copy_from_slice(b"Index HMD\n   ");
        assert_eq!(description(Some(&edid), "DP-1"), "VLV Index HMD");
    }
}
