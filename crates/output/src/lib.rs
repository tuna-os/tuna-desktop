//! Native output authority record (#784).
//!
//! [`NativeOutputInfo`] names the DRM connector a compositor-owned KMS
//! device actually drives. The compositor discovers it, the shell-control
//! protocol carries it (`Message::NativeOutputInventory`), and the backlight
//! reader and brightness services match sysfs devices against it. None of
//! those consumers needs the shell-control wire protocol to name an output,
//! so the record lives here, with no Wayland, GTK or framing dependency.
//!
//! Stability: internal. This crate is not published and makes no SemVer
//! promise for its Rust API. The serde field order is part of the
//! shell-control wire format: postcard encodes struct fields positionally,
//! so fields may only be appended, and only with a protocol minor bump
//! (`docs/api-stability.md`).

use serde::{Deserialize, Serialize};

/// Actual selected native DRM output authority. Missing inventory means no
/// authority, including nested sessions; connector text alone is insufficient.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeOutputInfo {
    pub name: String,
    /// Actual char-device rdev from the compositor-owned KMS FD.
    pub drm_device: u64,
    /// Actual DRM connector object ID, checked against kernel sysfs.
    pub connector_id: u32,
    /// Canonical sysfs DRM connector directory under that owned device.
    pub connector_sysfs: String,
    pub connector_device: u64,
    pub connector_inode: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owner() -> NativeOutputInfo {
        NativeOutputInfo {
            name: "eDP-1".into(),
            drm_device: 226,
            connector_id: 39,
            connector_sysfs: "/sys/owned/connector".into(),
            connector_device: 1,
            connector_inode: 2,
        }
    }

    /// Postcard is positional: these bytes are the shell-control wire body
    /// of one record. Reordering, renaming into a different type, or
    /// inserting a field changes them and breaks peers.
    #[test]
    fn postcard_body_is_positional_and_unchanged() {
        let mut buf = [0u8; 64];
        let bytes = postcard::to_slice(&owner(), &mut buf).unwrap();
        let mut expected = vec![5];
        expected.extend_from_slice(b"eDP-1");
        expected.extend_from_slice(&[226, 1, 39, 20]);
        expected.extend_from_slice(b"/sys/owned/connector");
        expected.extend_from_slice(&[1, 2]);
        assert_eq!(bytes, &expected[..]);
        let back: NativeOutputInfo = postcard::from_bytes(bytes).unwrap();
        assert_eq!(back, owner());
    }

    /// The brightness service reads the inventory as JSON from the session
    /// runtime directory; the field names are that file's keys.
    #[test]
    fn json_keys_match_the_session_inventory_file() {
        let json = serde_json::to_value(owner()).unwrap();
        let mut keys: Vec<_> = json.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "connector_device",
                "connector_id",
                "connector_inode",
                "connector_sysfs",
                "drm_device",
                "name",
            ]
        );
        let back: NativeOutputInfo = serde_json::from_value(json).unwrap();
        assert_eq!(back, owner());
    }
}
