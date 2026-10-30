//! EDID identity and colorimetry for colour management (P-DS-03, #341).
//!
//! Mutter reads the connector's EDID blob through libdisplay-info
//! (`src/backends/edid-parse.c`, `meta_edid_info_new_parse`) and derives
//! the monitor's vendor, product and serial
//! (`set_output_details_from_edid` in `src/backends/meta-output.c`), the
//! blob's MD5 (`meta_output_info_parse_edid`) and the default colour
//! characteristics its colour device turns into an ICC profile
//! (`create_icc_profile_from_edid` in `src/backends/meta-color-device.c`).
//! This module parses the same base-block fields:
//!
//! - manufacturer PNP ID (bytes 8–9), product code (10–11) and serial
//!   number (12–15);
//! - the product-name (`0xFC`) and serial-string (`0xFF`) display
//!   descriptors (strings end at a line feed and lose trailing spaces);
//! - default chromaticity coordinates (bytes 25–34, ten-bit fractions)
//!   and the default gamma (byte 23, `0xFF` = undefined).
//!
//! Mutter's monitor identity then falls back exactly as
//! `set_output_details_from_edid` does: an empty or missing product
//! name becomes `0x%04x` of the product code, a missing serial string
//! `0x%08x` of the serial number. A blob that is not an EDID (short, or
//! a wrong header) still has an MD5, as in Mutter, but no parsed info.
//!
//! Sources (mutter `51.0`, `138a14fbeef09d49ebf5be8a0cb83b042dd5c841`):
//! - <https://gitlab.gnome.org/GNOME/mutter/-/blob/51.0/src/backends/edid-parse.c>
//! - <https://gitlab.gnome.org/GNOME/mutter/-/blob/51.0/src/backends/meta-output.c>

use md5::{Digest, Md5};

/// CIE 1931 xy chromaticity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Chromaticity {
    pub x: f64,
    pub y: f64,
}

/// Default colour characteristics from the EDID base block.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorCharacteristics {
    /// Red, green and blue primaries.
    pub primaries: [Chromaticity; 3],
    /// Default white point.
    pub white: Chromaticity,
    /// Default display gamma; `0.0` when the EDID leaves it undefined.
    pub gamma: f64,
}

/// Parsed base-block EDID fields Mutter uses.
#[derive(Debug, Clone, PartialEq)]
pub struct EdidInfo {
    /// Three-letter PNP manufacturer ID (`meta_monitor_get_vendor`).
    pub manufacturer: String,
    /// Product code.
    pub product_code: u16,
    /// Serial number.
    pub serial_number: u32,
    /// Product-name display descriptor, when present.
    pub product_name: Option<String>,
    /// Serial-string display descriptor, when present.
    pub serial_string: Option<String>,
    /// Default colour characteristics.
    pub color: ColorCharacteristics,
}

impl EdidInfo {
    /// Mutter's `meta_monitor_get_product`: the product name, else the
    /// product code as `0x%04x`.
    pub fn product(&self) -> String {
        match &self.product_name {
            Some(name) if !name.is_empty() => name.clone(),
            _ => format!("0x{:04x}", self.product_code),
        }
    }

    /// Mutter's `meta_monitor_get_serial`: the serial string, else the
    /// serial number as `0x%08x`.
    pub fn serial(&self) -> String {
        match &self.serial_string {
            Some(serial) if !serial.is_empty() => serial.clone(),
            _ => format!("0x{:08x}", self.serial_number),
        }
    }
}

const HEADER: [u8; 8] = [0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00];

/// Parse an EDID base block. `None` when the blob is too short or lacks
/// the fixed header; a checksum mismatch is tolerated, as libdisplay-info
/// records it as a failure message but still returns the info.
pub fn parse(edid: &[u8]) -> Option<EdidInfo> {
    if edid.len() < 128 || edid[..8] != HEADER {
        return None;
    }
    let id = u16::from_be_bytes([edid[8], edid[9]]);
    let letter = |shift: u16| {
        let code = ((id >> shift) & 0x1f) as u8;
        // 1 is 'A'; libdisplay-info keeps whatever the code maps to.
        char::from(b'A' - 1 + code)
    };
    let manufacturer: String = [letter(10), letter(5), letter(0)].iter().collect();
    let product_code = u16::from_le_bytes([edid[10], edid[11]]);
    let serial_number = u32::from_le_bytes([edid[12], edid[13], edid[14], edid[15]]);
    let mut product_name = None;
    let mut serial_string = None;
    for offset in [54usize, 72, 90, 108] {
        let desc = &edid[offset..offset + 18];
        if desc[0] != 0 || desc[1] != 0 || desc[2] != 0 {
            continue;
        }
        let text = || descriptor_string(&desc[5..18]);
        match desc[3] {
            0xfc => product_name = Some(text()),
            0xff => serial_string = Some(text()),
            _ => {}
        }
    }
    let lo_rg = edid[25];
    let lo_bw = edid[26];
    let coord = |high: u8, low: u8| f64::from(u16::from(high) << 2 | u16::from(low & 0x3)) / 1024.0;
    let color = ColorCharacteristics {
        primaries: [
            Chromaticity {
                x: coord(edid[27], lo_rg >> 6),
                y: coord(edid[28], lo_rg >> 4),
            },
            Chromaticity {
                x: coord(edid[29], lo_rg >> 2),
                y: coord(edid[30], lo_rg),
            },
            Chromaticity {
                x: coord(edid[31], lo_bw >> 6),
                y: coord(edid[32], lo_bw >> 4),
            },
        ],
        white: Chromaticity {
            x: coord(edid[33], lo_bw >> 2),
            y: coord(edid[34], lo_bw),
        },
        gamma: if edid[23] == 0xff {
            0.0
        } else {
            f64::from(u16::from(edid[23]) + 100) / 100.0
        },
    };
    Some(EdidInfo {
        manufacturer,
        product_code,
        serial_number,
        product_name,
        serial_string,
        color,
    })
}

/// A display-descriptor string: up to 13 bytes, ended by a line feed,
/// trailing spaces trimmed (libdisplay-info `decode_display_descriptor_string`).
fn descriptor_string(bytes: &[u8]) -> String {
    let end = bytes
        .iter()
        .position(|&b| b == b'\n')
        .unwrap_or(bytes.len());
    let text: String = bytes[..end]
        .iter()
        .map(|&b| if b.is_ascii() { char::from(b) } else { '?' })
        .collect();
    text.trim_end_matches(' ').to_owned()
}

/// Lowercase hex MD5 of `bytes` (`g_compute_checksum_for_data (G_CHECKSUM_MD5, …)`).
pub fn md5_hex(bytes: &[u8]) -> String {
    let digest = Md5::digest(bytes);
    let mut out = String::with_capacity(32);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// One monitor as Mutter's colour manager sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct MonitorIdentity {
    /// Connector name, e.g. `eDP-1`.
    pub connector: String,
    /// PNP vendor (`meta_monitor_get_vendor`).
    pub vendor: Option<String>,
    /// Vendor name from the PNP database (`meta_backend_get_vendor_name`).
    pub vendor_name: Option<String>,
    pub product: Option<String>,
    pub serial: Option<String>,
    /// MD5 of the raw EDID blob (`meta_monitor_get_edid_checksum_md5`).
    pub edid_md5: Option<String>,
    /// Parsed EDID, when the blob was one.
    pub edid: Option<EdidInfo>,
    /// A laptop panel (`meta_monitor_is_builtin`: eDP, LVDS, DSI).
    pub builtin: bool,
    /// Primary monitor (`OutputPriority`).
    pub primary: bool,
    /// Physical size in millimetres (`meta_monitor_get_physical_dimensions`).
    pub size_mm: (i32, i32),
}

impl MonitorIdentity {
    /// Identity for `connector` from its EDID blob (if any). The vendor
    /// name is looked up with `vendor_name`, Mutter's
    /// `gnome_pnp_ids_get_pnp_id`.
    pub fn new(
        connector: &str,
        edid: Option<&[u8]>,
        primary: bool,
        size_mm: (i32, i32),
        vendor_name: impl Fn(&str) -> Option<String>,
    ) -> Self {
        let parsed = edid.and_then(parse);
        let vendor = parsed.as_ref().map(|info| info.manufacturer.clone());
        Self {
            connector: connector.to_owned(),
            vendor_name: vendor.as_deref().and_then(&vendor_name),
            vendor,
            product: parsed.as_ref().map(EdidInfo::product),
            serial: parsed.as_ref().map(EdidInfo::serial),
            edid_md5: edid.map(md5_hex),
            edid: parsed,
            builtin: is_builtin_connector(connector),
            primary,
            size_mm,
        }
    }

    /// colord `DeviceId`, `generate_cd_device_id`: `xrandr`, then the
    /// vendor name (else PNP ID), product and serial that exist, or the
    /// connector when none does.
    pub fn colord_device_id(&self) -> String {
        let mut id = String::from("xrandr");
        if self.vendor.is_none() && self.product.is_none() && self.serial.is_none() {
            id.push('-');
            id.push_str(&self.connector);
            return id;
        }
        if let Some(vendor) = &self.vendor {
            id.push('-');
            id.push_str(self.vendor_name.as_deref().unwrap_or(vendor));
        }
        for part in [&self.product, &self.serial].into_iter().flatten() {
            id.push('-');
            id.push_str(part);
        }
        id
    }

    /// `meta_monitor_make_display_name`: "Built-in display" for panels;
    /// else the vendor name (PNP ID, "Unknown"/"Unknown Display") with
    /// the diagonal in inches, or the product when the EDID stores an
    /// aspect ratio as the size.
    pub fn display_name(&self) -> String {
        if self.builtin {
            return "Built-in display".to_owned();
        }
        let (w, h) = self.size_mm;
        let mut inches = None;
        let mut product = None;
        if w > 0 && h > 0 {
            if crate::display_config::has_aspect_as_size(w, h) {
                product = self.product.as_deref();
            } else {
                let d = f64::from(w).hypot(f64::from(h)) / 25.4;
                inches = Some(diagonal_to_str(d));
            }
        }
        let vendor = match (&self.vendor, &self.vendor_name) {
            (Some(_), Some(name)) => name.clone(),
            (Some(pnp), None) => pnp.clone(),
            (None, _) if inches.is_some() => "Unknown".to_owned(),
            (None, _) => "Unknown Display".to_owned(),
        };
        match (inches, product) {
            (Some(inches), _) => format!("{vendor} {inches}"),
            (None, Some(product)) => format!("{vendor} {product}"),
            (None, None) => vendor,
        }
    }
}

/// Mutter's `diagonal_to_str`: a known laptop diagonal to one decimal,
/// else whole inches.
fn diagonal_to_str(d: f64) -> String {
    for known in [12.1, 13.3, 15.6] {
        if (known - d).abs() < 0.1 {
            return format!("{known:.1}\"");
        }
    }
    format!("{}\"", (d + 0.5) as i32)
}

/// `meta_output_is_builtin`: laptop panel connector types.
pub fn is_builtin_connector(connector: &str) -> bool {
    ["eDP-", "LVDS-", "DSI-"]
        .iter()
        .any(|prefix| connector.starts_with(prefix))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A Dell U2415-like base block: `DEL`, product 0xa0b1, serial
    /// 0x12345678, name `DELL U2415`, serial string `7MT0175A1B2L`,
    /// gamma 2.2, primaries near sRGB.
    pub(crate) fn sample_edid() -> Vec<u8> {
        let mut e = vec![0u8; 128];
        e[..8].copy_from_slice(&HEADER);
        // D=4 E=5 L=12: 00100 00101 01100
        let id: u16 = (4 << 10) | (5 << 5) | 12;
        e[8..10].copy_from_slice(&id.to_be_bytes());
        e[10..12].copy_from_slice(&0xa0b1u16.to_le_bytes());
        e[12..16].copy_from_slice(&0x1234_5678u32.to_le_bytes());
        e[18] = 1;
        e[19] = 4;
        e[23] = 120; // gamma 2.20
        let set = |e: &mut Vec<u8>, hi: usize, v: f64| -> u8 {
            let raw = (v * 1024.0).round() as u16;
            e[hi] = (raw >> 2) as u8;
            (raw & 3) as u8
        };
        let rx = set(&mut e, 27, 0.640);
        let ry = set(&mut e, 28, 0.330);
        let gx = set(&mut e, 29, 0.300);
        let gy = set(&mut e, 30, 0.600);
        let bx = set(&mut e, 31, 0.150);
        let by = set(&mut e, 32, 0.060);
        let wx = set(&mut e, 33, 0.3127);
        let wy = set(&mut e, 34, 0.3290);
        e[25] = rx << 6 | ry << 4 | gx << 2 | gy;
        e[26] = bx << 6 | by << 4 | wx << 2 | wy;
        let descriptor = |e: &mut Vec<u8>, at: usize, tag: u8, text: &str| {
            e[at..at + 3].fill(0);
            e[at + 3] = tag;
            let mut body = [b' '; 13];
            body[..text.len()].copy_from_slice(text.as_bytes());
            if text.len() < 13 {
                body[text.len()] = b'\n';
            }
            e[at + 5..at + 18].copy_from_slice(&body);
        };
        descriptor(&mut e, 72, 0xfc, "DELL U2415");
        descriptor(&mut e, 90, 0xff, "7MT0175A1B2L");
        let sum: u8 = e[..127].iter().fold(0u8, |a, b| a.wrapping_add(*b));
        e[127] = 0u8.wrapping_sub(sum);
        e
    }

    #[test]
    fn edid_identity_matches_mutter_fields() {
        let info = parse(&sample_edid()).expect("valid EDID");
        assert_eq!(info.manufacturer, "DEL");
        assert_eq!(info.product(), "DELL U2415");
        assert_eq!(info.serial(), "7MT0175A1B2L");
        assert!((info.color.gamma - 2.2).abs() < 1e-9);
        assert!((info.color.primaries[0].x - 0.640).abs() < 1.0 / 1024.0);
        assert!((info.color.primaries[2].y - 0.060).abs() < 1.0 / 1024.0);
        assert!((info.color.white.x - 0.3127).abs() < 1.0 / 1024.0);
    }

    #[test]
    fn missing_descriptors_fall_back_to_codes() {
        let mut edid = sample_edid();
        edid[72..108].fill(1);
        edid[23] = 0xff;
        let info = parse(&edid).unwrap();
        assert_eq!(info.product(), "0xa0b1");
        assert_eq!(info.serial(), "0x12345678");
        assert_eq!(info.color.gamma, 0.0);
    }

    #[test]
    fn non_edid_blobs_keep_an_md5_but_no_info() {
        assert!(parse(&[0u8; 127]).is_none());
        assert!(parse(&[0u8; 128]).is_none());
        let id = MonitorIdentity::new("HDMI-A-1", Some(&[1, 2, 3]), false, (0, 0), |_| None);
        assert_eq!(
            id.edid_md5.as_deref(),
            Some("5289df737df57326fcdd22597afb1fac")
        );
        assert!(id.edid.is_none());
        assert_eq!(id.colord_device_id(), "xrandr-HDMI-A-1");
    }

    #[test]
    fn colord_device_ids_follow_the_naming_spec() {
        let edid = sample_edid();
        let named = MonitorIdentity::new("DP-1", Some(&edid), true, (527, 296), |pnp| {
            (pnp == "DEL").then(|| "Dell Inc.".to_owned())
        });
        assert_eq!(
            named.colord_device_id(),
            "xrandr-Dell Inc.-DELL U2415-7MT0175A1B2L"
        );
        assert_eq!(named.display_name(), "Dell Inc. 24\"");
        let unnamed = MonitorIdentity::new("DP-1", Some(&edid), true, (160, 90), |_| None);
        assert_eq!(
            unnamed.colord_device_id(),
            "xrandr-DEL-DELL U2415-7MT0175A1B2L"
        );
        assert_eq!(unnamed.display_name(), "DEL DELL U2415");
        let virtual_ = MonitorIdentity::new("Virtual-1", None, true, (0, 0), |_| None);
        assert_eq!(virtual_.colord_device_id(), "xrandr-Virtual-1");
        assert_eq!(virtual_.display_name(), "Unknown Display");
        assert!(virtual_.edid_md5.is_none());
        let panel = MonitorIdentity::new("eDP-1", Some(&edid), true, (294, 165), |_| None);
        assert_eq!(panel.display_name(), "Built-in display");
        let laptop = MonitorIdentity::new("DP-2", None, false, (344, 194), |_| None);
        assert_eq!(laptop.display_name(), "Unknown 15.6\"");
    }

    #[test]
    fn builtin_connectors_are_panels() {
        assert!(is_builtin_connector("eDP-1"));
        assert!(is_builtin_connector("LVDS-1"));
        assert!(is_builtin_connector("DSI-1"));
        assert!(!is_builtin_connector("DP-1"));
        assert!(!is_builtin_connector("Virtual-1"));
    }
}
