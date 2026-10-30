//! ICC display profiles: the subset Mutter's colour manager writes and
//! reads (P-DS-03, #341).
//!
//! Mutter generates a per-monitor ICC profile with lcms2 and colord's
//! `CdIcc` (`create_icc_profile_from_edid`, `create_device_icc_profile`
//! in `src/backends/meta-color-device.c`): an RGB display-class v4
//! matrix/TRC profile built by `cmsCreateRGBProfileTHR` from the EDID
//! primaries, white point and gamma, or lcms2's built-in sRGB profile
//! (`cd_icc_create_default_full`) when the monitor has no EDID, plus
//! colord's metadata dictionary (`meta` tag) and localized
//! description/copyright/manufacturer/model (`mluc`). colord reads the
//! dictionary to map the profile to its device (`MAPPING_device_id`).
//!
//! When a profile is assigned to a device Mutter loads it and reads its
//! `vcgt` calibration curves (`meta_color_calibration_new` in
//! `src/backends/meta-color-profile.c`); wp_color_management_v1 ICC
//! image descriptions need the matrix/TRC model of client profiles.
//! This module encodes the profiles Mutter writes and decodes `vcgt`,
//! `meta`, `desc` and the matrix/TRC tags, with the lcms2 tag semantics
//! (`Type_vcgt_Read`, `Type_Dictionary_Read`, `Type_ParametricCurve_Read`).
//!
//! Sources: <https://gitlab.gnome.org/GNOME/mutter/-/blob/51.0/src/backends/meta-color-device.c>,
//! <https://github.com/hughsie/colord/blob/1.4.7/lib/colord/cd-icc.c>,
//! ICC.1:2022 (v4.4) and the Apple `vcgt` tag as lcms2 2.14 reads it.

use md5::{Digest, Md5};

/// D50, the ICC profile connection space illuminant (lcms2 `cmsD50_XYZ`).
pub const D50: [f64; 3] = [0.9642, 1.0, 0.8249];

/// CIE xy.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Xy {
    pub x: f64,
    pub y: f64,
}

impl Xy {
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    /// XYZ with Y = 1.
    pub fn to_xyz(self) -> [f64; 3] {
        [self.x / self.y, 1.0, (1.0 - self.x - self.y) / self.y]
    }
}

/// sRGB / BT.709 primaries and D65 white as lcms2's `cmsCreate_sRGBProfile`.
pub const SRGB_PRIMARIES: [Xy; 3] = [
    Xy::new(0.64, 0.33),
    Xy::new(0.30, 0.60),
    Xy::new(0.15, 0.06),
];
pub const D65: Xy = Xy::new(0.3127, 0.3290);

/// A one-dimensional transfer curve, as the ICC `curv`/`para` types
/// define them.
#[derive(Debug, Clone, PartialEq)]
pub enum Curve {
    /// `curv` with no entries.
    Identity,
    /// `curv` with one entry, or `para` type 0: `y = x^g`.
    Gamma(f64),
    /// `curv` table, linearly interpolated over 0..=1.
    Table(Vec<u16>),
    /// `para` function type 1..=4 with its parameters
    /// `[g, a, b, c, d, e, f]` (unused ones zero).
    Parametric(u16, [f64; 7]),
}

impl Curve {
    /// Evaluate at `x` in 0..=1 (lcms2 `cmsEvalToneCurveFloat`).
    pub fn eval(&self, x: f64) -> f64 {
        match self {
            Curve::Identity => x,
            Curve::Gamma(g) => x.max(0.0).powf(*g),
            Curve::Table(table) => interpolate(table, x),
            Curve::Parametric(kind, p) => {
                let [g, a, b, c, d, e, f] = *p;
                match kind {
                    1 => {
                        if x >= -b / a {
                            (a * x + b).max(0.0).powf(g)
                        } else {
                            0.0
                        }
                    }
                    2 => {
                        if x >= -b / a {
                            (a * x + b).max(0.0).powf(g) + c
                        } else {
                            c
                        }
                    }
                    3 => {
                        if x >= d {
                            (a * x + b).max(0.0).powf(g)
                        } else {
                            c * x
                        }
                    }
                    4 => {
                        if x >= d {
                            (a * x + b).max(0.0).powf(g) + e
                        } else {
                            c * x + f
                        }
                    }
                    _ => x.max(0.0).powf(g),
                }
            }
        }
    }
}

/// lcms2's tabulated-curve float evaluation: the input saturates to a
/// 16-bit word (`_cmsQuickSaturateWord`), `LinLerp1D` interpolates in
/// 16.16 fixed point over the table, and the word scales back to 0..=1.
fn interpolate(table: &[u16], x: f64) -> f64 {
    let n = table.len();
    if n == 0 {
        return x;
    }
    let d = x * 65535.0 + 0.5;
    let input: u32 = if d <= 0.0 {
        0
    } else if d >= 65535.0 {
        0xffff
    } else {
        d.floor() as u32
    };
    let domain = (n - 1) as u32;
    let out = if input == 0xffff || domain == 0 {
        table[n - 1]
    } else {
        let val3 = domain * input;
        // _cmsToFixedDomain
        let val3 = val3 + (val3 + 0x7fff) / 0xffff;
        let cell = (val3 >> 16) as usize;
        let rest = val3 & 0xffff;
        let (l, h) = (
            i64::from(table[cell]),
            i64::from(table[(cell + 1).min(n - 1)]),
        );
        // LinearInterp, in unsigned 32-bit arithmetic as lcms2 does.
        let dif = ((h - l) as u32).wrapping_mul(rest).wrapping_add(0x8000);
        ((dif >> 16) as i64 + l) as u16
    };
    f64::from(out) / 65535.0
}

/// Video card gamma table (`vcgt`), one curve per channel.
#[derive(Debug, Clone, PartialEq)]
pub struct Vcgt(pub [Curve; 3]);

/// A matrix/TRC RGB profile: D50-relative colorants and per-channel TRCs.
#[derive(Debug, Clone, PartialEq)]
pub struct MatrixShaper {
    /// Red, green and blue colorant XYZ (PCS, D50 adapted).
    pub colorants: [[f64; 3]; 3],
    /// Red, green and blue tone reproduction curves (device to linear).
    pub trc: [Curve; 3],
}

/// Why a blob is not a usable ICC profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IccError(pub String);

impl std::fmt::Display for IccError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A decoded profile: header fields and the raw tag table.
#[derive(Debug, Clone)]
pub struct Profile {
    /// Major version byte (2 or 4).
    pub version_major: u8,
    pub device_class: [u8; 4],
    pub color_space: [u8; 4],
    pub pcs: [u8; 4],
    data: Vec<u8>,
    tags: Vec<([u8; 4], usize, usize)>,
}

fn be_u16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn be_u32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn s15f16(b: &[u8], at: usize) -> Option<f64> {
    Some(f64::from(be_u32(b, at)? as i32) / 65536.0)
}

impl Profile {
    /// Parse an ICC profile (v2 or v4), validating header and tag table.
    pub fn parse(bytes: &[u8]) -> Result<Self, IccError> {
        let bad = |why: &str| IccError(why.to_owned());
        if bytes.len() < 132 {
            return Err(bad("profile shorter than the header"));
        }
        if &bytes[36..40] != b"acsp" {
            return Err(bad("missing 'acsp' signature"));
        }
        let size = be_u32(bytes, 0).unwrap_or(0) as usize;
        if size < 132 || size > bytes.len() {
            return Err(bad("header size does not match the data"));
        }
        let data = bytes[..size].to_vec();
        let count = be_u32(&data, 128).unwrap_or(0) as usize;
        if count > (size - 132) / 12 {
            return Err(bad("tag table overruns the profile"));
        }
        let mut tags = Vec::with_capacity(count);
        for i in 0..count {
            let at = 132 + i * 12;
            let sig: [u8; 4] = data[at..at + 4].try_into().unwrap();
            let offset = be_u32(&data, at + 4).unwrap_or(0) as usize;
            let len = be_u32(&data, at + 8).unwrap_or(0) as usize;
            if offset < 128 || len < 8 || offset.checked_add(len).is_none_or(|end| end > size) {
                return Err(bad("tag outside the profile"));
            }
            tags.push((sig, offset, len));
        }
        let mut device_class = [0; 4];
        device_class.copy_from_slice(&data[12..16]);
        let mut color_space = [0; 4];
        color_space.copy_from_slice(&data[16..20]);
        let mut pcs = [0; 4];
        pcs.copy_from_slice(&data[20..24]);
        Ok(Self {
            version_major: data[8],
            device_class,
            color_space,
            pcs,
            data,
            tags,
        })
    }

    /// The raw bytes of tag `sig` (type signature included).
    pub fn tag(&self, sig: &[u8; 4]) -> Option<&[u8]> {
        self.tags
            .iter()
            .find(|(s, _, _)| s == sig)
            .map(|&(_, at, len)| &self.data[at..at + len])
    }

    pub fn has_tag(&self, sig: &[u8; 4]) -> bool {
        self.tag(sig).is_some()
    }

    /// The profile bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.data
    }

    /// `vcgt` calibration curves (lcms2 `Type_vcgt_Read`).
    pub fn vcgt(&self) -> Option<Vcgt> {
        let tag = self.tag(b"vcgt")?;
        if &tag[..4] != b"vcgt" {
            return None;
        }
        match be_u32(tag, 8)? {
            0 => {
                let channels = be_u16(tag, 12)?;
                let count = be_u16(tag, 14)? as usize;
                let size = be_u16(tag, 16)? as usize;
                if channels != 3 || !(size == 1 || size == 2) || count < 2 {
                    return None;
                }
                let mut curves = Vec::with_capacity(3);
                for c in 0..3 {
                    let mut table = Vec::with_capacity(count);
                    for i in 0..count {
                        let at = 18 + (c * count + i) * size;
                        table.push(if size == 1 {
                            u16::from(*tag.get(at)?) * 257
                        } else {
                            be_u16(tag, at)?
                        });
                    }
                    curves.push(Curve::Table(table));
                }
                let [r, g, b]: [Curve; 3] = curves.try_into().ok()?;
                Some(Vcgt([r, g, b]))
            }
            1 => {
                let mut curves = Vec::with_capacity(3);
                for c in 0..3 {
                    let gamma = s15f16(tag, 12 + c * 12)?;
                    let min = s15f16(tag, 16 + c * 12)?;
                    let max = s15f16(tag, 20 + c * 12)?;
                    // lcms2: type 5 with a = (max - min)^(1/g), e = f = min.
                    let a = (max - min).max(0.0).powf(1.0 / gamma);
                    curves.push(Curve::Parametric(4, [gamma, a, 0.0, 0.0, 0.0, min, min]));
                }
                let [r, g, b]: [Curve; 3] = curves.try_into().ok()?;
                Some(Vcgt([r, g, b]))
            }
            _ => None,
        }
    }

    /// The `meta` dictionary (colord metadata).
    pub fn metadata(&self) -> Vec<(String, String)> {
        let Some(tag) = self.tag(b"meta") else {
            return Vec::new();
        };
        if &tag[..4] != b"dict" {
            return Vec::new();
        }
        let count = be_u32(tag, 8).unwrap_or(0) as usize;
        let record = be_u32(tag, 12).unwrap_or(0) as usize;
        if !matches!(record, 16 | 24 | 32) {
            return Vec::new();
        }
        let string = |offset: Option<u32>, len: Option<u32>| -> Option<String> {
            let (offset, len) = (offset? as usize, len? as usize);
            let raw = tag.get(offset..offset.checked_add(len)?)?;
            let units: Vec<u16> = raw
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_be_bytes(*c))
                .collect();
            Some(String::from_utf16_lossy(&units))
        };
        (0..count)
            .filter_map(|i| {
                let at = 16 + i * record;
                let name = string(be_u32(tag, at), be_u32(tag, at + 4))?;
                let value = string(be_u32(tag, at + 8), be_u32(tag, at + 12))?;
                Some((name, value))
            })
            .collect()
    }

    /// The profile description (`desc`: `mluc` in v4, `desc` in v2).
    pub fn description(&self) -> Option<String> {
        let tag = self.tag(b"desc")?;
        match &tag[..4] {
            b"mluc" => {
                let count = be_u32(tag, 8)?;
                if count == 0 {
                    return None;
                }
                let len = be_u32(tag, 20)? as usize;
                let offset = be_u32(tag, 24)? as usize;
                let raw = tag.get(offset..offset + len)?;
                let units: Vec<u16> = raw
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| u16::from_be_bytes(*c))
                    .collect();
                Some(String::from_utf16_lossy(&units))
            }
            b"desc" => {
                let len = be_u32(tag, 8)? as usize;
                let raw = tag.get(12..12 + len)?;
                let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
                Some(String::from_utf8_lossy(&raw[..end]).into_owned())
            }
            _ => None,
        }
    }

    /// A display-class RGB matrix/TRC model, when the profile is one.
    pub fn matrix_shaper(&self) -> Option<MatrixShaper> {
        if &self.color_space != b"RGB " || &self.pcs != b"XYZ " {
            return None;
        }
        let xyz = |sig: &[u8; 4]| -> Option<[f64; 3]> {
            let tag = self.tag(sig)?;
            if &tag[..4] != b"XYZ " {
                return None;
            }
            Some([s15f16(tag, 8)?, s15f16(tag, 12)?, s15f16(tag, 16)?])
        };
        let colorants = [xyz(b"rXYZ")?, xyz(b"gXYZ")?, xyz(b"bXYZ")?];
        let trc = [
            parse_curve(self.tag(b"rTRC")?)?,
            parse_curve(self.tag(b"gTRC")?)?,
            parse_curve(self.tag(b"bTRC")?)?,
        ];
        Some(MatrixShaper { colorants, trc })
    }

    /// The media white point (`wtpt`), when present.
    pub fn white_point(&self) -> Option<[f64; 3]> {
        let tag = self.tag(b"wtpt")?;
        if &tag[..4] != b"XYZ " {
            return None;
        }
        Some([s15f16(tag, 8)?, s15f16(tag, 12)?, s15f16(tag, 16)?])
    }
}

fn parse_curve(tag: &[u8]) -> Option<Curve> {
    match &tag[..4] {
        b"curv" => {
            let count = be_u32(tag, 8)? as usize;
            match count {
                0 => Some(Curve::Identity),
                1 => Some(Curve::Gamma(f64::from(be_u16(tag, 12)?) / 256.0)),
                _ => {
                    let mut table = Vec::with_capacity(count);
                    for i in 0..count {
                        table.push(be_u16(tag, 12 + i * 2)?);
                    }
                    Some(Curve::Table(table))
                }
            }
        }
        b"para" => {
            let kind = be_u16(tag, 8)?;
            let n = match kind {
                0 => 1,
                1 => 3,
                2 => 4,
                3 => 5,
                4 => 7,
                _ => return None,
            };
            let mut p = [0.0; 7];
            for (i, slot) in p.iter_mut().enumerate().take(n) {
                *slot = s15f16(tag, 12 + i * 4)?;
            }
            Some(if kind == 0 {
                Curve::Gamma(p[0])
            } else {
                Curve::Parametric(kind, p)
            })
        }
        _ => None,
    }
}

/// 3x3 matrix helpers (row-major).
pub type Mat3 = [[f64; 3]; 3];

pub fn mat_mul(a: &Mat3, b: &Mat3) -> Mat3 {
    let mut out = [[0.0; 3]; 3];
    for (i, row) in out.iter_mut().enumerate() {
        for (j, cell) in row.iter_mut().enumerate() {
            *cell = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    out
}

pub fn mat_vec(a: &Mat3, v: [f64; 3]) -> [f64; 3] {
    [
        a[0][0] * v[0] + a[0][1] * v[1] + a[0][2] * v[2],
        a[1][0] * v[0] + a[1][1] * v[1] + a[1][2] * v[2],
        a[2][0] * v[0] + a[2][1] * v[1] + a[2][2] * v[2],
    ]
}

pub fn mat_inverse(m: &Mat3) -> Option<Mat3> {
    let [[a, b, c], [d, e, f], [g, h, i]] = *m;
    let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    if det.abs() < 1e-12 {
        return None;
    }
    let inv = 1.0 / det;
    Some([
        [
            (e * i - f * h) * inv,
            (c * h - b * i) * inv,
            (b * f - c * e) * inv,
        ],
        [
            (f * g - d * i) * inv,
            (a * i - c * g) * inv,
            (c * d - a * f) * inv,
        ],
        [
            (d * h - e * g) * inv,
            (b * g - a * h) * inv,
            (a * e - b * d) * inv,
        ],
    ])
}

/// Bradford cone response (lcms2 `cmsAdaptationMatrix`, Mutter's
/// `compute_chromatic_adaptation`).
const BRADFORD: Mat3 = [
    [0.8951, 0.2664, -0.1614],
    [-0.7502, 1.7135, 0.0367],
    [0.0389, -0.0685, 1.0296],
];

/// Bradford chromatic adaptation from white `src` to white `dst` (XYZ).
pub fn bradford(src: [f64; 3], dst: [f64; 3]) -> Mat3 {
    let inv = mat_inverse(&BRADFORD).expect("Bradford is invertible");
    let s = mat_vec(&BRADFORD, src);
    let d = mat_vec(&BRADFORD, dst);
    let scale = [
        [d[0] / s[0], 0.0, 0.0],
        [0.0, d[1] / s[1], 0.0],
        [0.0, 0.0, d[2] / s[2]],
    ];
    mat_mul(&inv, &mat_mul(&scale, &BRADFORD))
}

/// RGB-to-XYZ matrix of `primaries` with `white` mapping to Y = 1.
pub fn rgb_to_xyz(primaries: &[Xy; 3], white: Xy) -> Option<Mat3> {
    let [r, g, b] = primaries.map(Xy::to_xyz);
    let p = [[r[0], g[0], b[0]], [r[1], g[1], b[1]], [r[2], g[2], b[2]]];
    let s = mat_vec(&mat_inverse(&p)?, white.to_xyz());
    Some([
        [p[0][0] * s[0], p[0][1] * s[1], p[0][2] * s[2]],
        [p[1][0] * s[0], p[1][1] * s[1], p[1][2] * s[2]],
        [p[2][0] * s[0], p[2][1] * s[1], p[2][2] * s[2]],
    ])
}

/// Transfer curve of a generated profile.
#[derive(Debug, Clone, PartialEq)]
pub enum Trc {
    /// `cmsBuildGamma`: a single-entry `curv`.
    Gamma(f64),
    /// lcms2's sRGB parametric type 4.
    Srgb,
}

/// What a generated display profile contains.
#[derive(Debug, Clone, PartialEq)]
pub struct DisplayProfile {
    pub description: String,
    pub copyright: Option<String>,
    pub manufacturer: Option<String>,
    pub model: Option<String>,
    pub primaries: [Xy; 3],
    pub white: Xy,
    pub trc: Trc,
    /// Calibration curves, written as a 16-bit `vcgt` table.
    pub vcgt: Option<[Vec<u16>; 3]>,
    /// colord metadata dictionary, in order.
    pub metadata: Vec<(String, String)>,
}

impl DisplayProfile {
    /// lcms2's built-in sRGB profile as `cd_icc_create_default_full`
    /// leaves it: description "sRGB built-in", standard-space metadata.
    pub fn srgb() -> Self {
        Self {
            description: "sRGB built-in".to_owned(),
            copyright: Some("No copyright, use freely".to_owned()),
            manufacturer: None,
            model: None,
            primaries: SRGB_PRIMARIES,
            white: D65,
            trc: Trc::Srgb,
            vcgt: None,
            metadata: vec![
                ("DATA_source".to_owned(), "standard".to_owned()),
                ("STANDARD_space".to_owned(), "srgb".to_owned()),
            ],
        }
    }

    /// Encode as an ICC v4.3 display profile with a computed profile ID.
    pub fn encode(&self) -> Result<Vec<u8>, IccError> {
        let to_pcs = rgb_to_xyz(&self.primaries, self.white)
            .ok_or_else(|| IccError("degenerate primaries".to_owned()))?;
        let chad = bradford(self.white.to_xyz(), D50);
        let colorants = mat_mul(&chad, &to_pcs);
        let mut tags: Vec<([u8; 4], Vec<u8>)> = Vec::new();
        tags.push((*b"desc", mluc(&self.description)));
        if let Some(text) = &self.copyright {
            tags.push((*b"cprt", mluc(text)));
        }
        if let Some(text) = &self.manufacturer {
            tags.push((*b"dmnd", mluc(text)));
        }
        if let Some(text) = &self.model {
            tags.push((*b"dmdd", mluc(text)));
        }
        tags.push((*b"wtpt", xyz_type(D50)));
        tags.push((*b"chad", sf32(&chad)));
        for (sig, column) in [(*b"rXYZ", 0), (*b"gXYZ", 1), (*b"bXYZ", 2)] {
            tags.push((
                sig,
                xyz_type([
                    colorants[0][column],
                    colorants[1][column],
                    colorants[2][column],
                ]),
            ));
        }
        let curve = match &self.trc {
            Trc::Gamma(g) => curv_gamma(*g),
            Trc::Srgb => para4([
                2.4,
                1.0 / 1.055,
                0.055 / 1.055,
                1.0 / 12.92,
                0.04045,
                0.0,
                0.0,
            ]),
        };
        for sig in [*b"rTRC", *b"gTRC", *b"bTRC"] {
            tags.push((sig, curve.clone()));
        }
        tags.push((*b"chrm", chrm(&self.primaries)));
        if let Some(vcgt) = &self.vcgt {
            tags.push((*b"vcgt", vcgt_table(vcgt)?));
        }
        if !self.metadata.is_empty() {
            tags.push((*b"meta", dict(&self.metadata)));
        }
        Ok(assemble(&tags))
    }
}

fn s15(v: f64) -> [u8; 4] {
    ((v * 65536.0).round() as i32).to_be_bytes()
}

fn utf16be(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_be_bytes).collect()
}

fn mluc(text: &str) -> Vec<u8> {
    let body = utf16be(text);
    let mut t = Vec::with_capacity(28 + body.len());
    t.extend_from_slice(b"mluc\0\0\0\0");
    t.extend_from_slice(&1u32.to_be_bytes());
    t.extend_from_slice(&12u32.to_be_bytes());
    t.extend_from_slice(b"enUS");
    t.extend_from_slice(&(body.len() as u32).to_be_bytes());
    t.extend_from_slice(&28u32.to_be_bytes());
    t.extend_from_slice(&body);
    t
}

fn xyz_type(v: [f64; 3]) -> Vec<u8> {
    let mut t = b"XYZ \0\0\0\0".to_vec();
    for c in v {
        t.extend_from_slice(&s15(c));
    }
    t
}

fn sf32(m: &Mat3) -> Vec<u8> {
    let mut t = b"sf32\0\0\0\0".to_vec();
    for row in m {
        for &c in row {
            t.extend_from_slice(&s15(c));
        }
    }
    t
}

fn curv_gamma(g: f64) -> Vec<u8> {
    let mut t = b"curv\0\0\0\0".to_vec();
    t.extend_from_slice(&1u32.to_be_bytes());
    t.extend_from_slice(&((g * 256.0).round() as u16).to_be_bytes());
    t
}

fn para4(p: [f64; 7]) -> Vec<u8> {
    let mut t = b"para\0\0\0\0".to_vec();
    t.extend_from_slice(&4u16.to_be_bytes());
    t.extend_from_slice(&[0, 0]);
    for c in p {
        t.extend_from_slice(&s15(c));
    }
    t
}

fn chrm(primaries: &[Xy; 3]) -> Vec<u8> {
    let mut t = b"chrm\0\0\0\0".to_vec();
    t.extend_from_slice(&3u16.to_be_bytes());
    t.extend_from_slice(&0u16.to_be_bytes());
    for p in primaries {
        for c in [p.x, p.y] {
            t.extend_from_slice(&((c * 65536.0).round() as u32).to_be_bytes());
        }
    }
    t
}

fn vcgt_table(curves: &[Vec<u16>; 3]) -> Result<Vec<u8>, IccError> {
    let n = curves[0].len();
    if n < 2 || curves.iter().any(|c| c.len() != n) || n > usize::from(u16::MAX) {
        return Err(IccError("vcgt channels differ in length".to_owned()));
    }
    let mut t = b"vcgt\0\0\0\0".to_vec();
    t.extend_from_slice(&0u32.to_be_bytes());
    t.extend_from_slice(&3u16.to_be_bytes());
    t.extend_from_slice(&(n as u16).to_be_bytes());
    t.extend_from_slice(&2u16.to_be_bytes());
    for curve in curves {
        for v in curve {
            t.extend_from_slice(&v.to_be_bytes());
        }
    }
    Ok(t)
}

fn dict(entries: &[(String, String)]) -> Vec<u8> {
    let header = 16 + entries.len() * 16;
    let mut strings = Vec::new();
    let mut records = Vec::new();
    for (name, value) in entries {
        for text in [name, value] {
            let bytes = utf16be(text);
            records.extend_from_slice(&((header + strings.len()) as u32).to_be_bytes());
            records.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
            strings.extend_from_slice(&bytes);
        }
    }
    let mut t = b"dict\0\0\0\0".to_vec();
    t.extend_from_slice(&(entries.len() as u32).to_be_bytes());
    t.extend_from_slice(&16u32.to_be_bytes());
    t.extend_from_slice(&records);
    t.extend_from_slice(&strings);
    t
}

fn pad4(v: &mut Vec<u8>) {
    while !v.len().is_multiple_of(4) {
        v.push(0);
    }
}

fn assemble(tags: &[([u8; 4], Vec<u8>)]) -> Vec<u8> {
    let mut body = Vec::new();
    let table_end = 128 + 4 + tags.len() * 12;
    let mut table = Vec::new();
    for (sig, data) in tags {
        let offset = table_end + body.len();
        table.extend_from_slice(sig);
        table.extend_from_slice(&(offset as u32).to_be_bytes());
        table.extend_from_slice(&(data.len() as u32).to_be_bytes());
        body.extend_from_slice(data);
        pad4(&mut body);
    }
    let size = table_end + body.len();
    let mut out = vec![0u8; 128];
    out[0..4].copy_from_slice(&(size as u32).to_be_bytes());
    out[4..8].copy_from_slice(b"lcms");
    out[8] = 4;
    out[9] = 0x30;
    out[12..16].copy_from_slice(b"mntr");
    out[16..20].copy_from_slice(b"RGB ");
    out[20..24].copy_from_slice(b"XYZ ");
    out[36..40].copy_from_slice(b"acsp");
    // Rendering intent 0 is perceptual (cmsSetHeaderRenderingIntent).
    for (i, c) in D50.iter().enumerate() {
        out[68 + i * 4..72 + i * 4].copy_from_slice(&s15(*c));
    }
    out[80..84].copy_from_slice(b"tuna");
    out.extend_from_slice(&(tags.len() as u32).to_be_bytes());
    out.extend_from_slice(&table);
    out.extend_from_slice(&body);
    let id = profile_id(&out);
    out[84..100].copy_from_slice(&id);
    out
}

/// ICC profile ID: MD5 with flags, rendering intent and ID zeroed
/// (lcms2 `cmsMD5computeID`).
pub fn profile_id(profile: &[u8]) -> [u8; 16] {
    let mut copy = profile.to_vec();
    copy[44..48].fill(0);
    copy[64..68].fill(0);
    copy[84..100].fill(0);
    Md5::digest(&copy).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: f64, b: f64, eps: f64) -> bool {
        (a - b).abs() <= eps
    }

    #[test]
    fn srgb_profile_round_trips_through_the_decoder() {
        let bytes = DisplayProfile::srgb().encode().unwrap();
        let profile = Profile::parse(&bytes).unwrap();
        assert_eq!(profile.version_major, 4);
        assert_eq!(&profile.device_class, b"mntr");
        assert_eq!(profile.description().as_deref(), Some("sRGB built-in"));
        assert_eq!(
            profile.metadata(),
            vec![
                ("DATA_source".to_owned(), "standard".to_owned()),
                ("STANDARD_space".to_owned(), "srgb".to_owned())
            ]
        );
        assert!(profile.vcgt().is_none());
        let shaper = profile.matrix_shaper().unwrap();
        // lcms2's sRGB D50 colorants (cmsCreate_sRGBProfile).
        assert!(near(shaper.colorants[0][0], 0.4361, 2e-4));
        assert!(near(shaper.colorants[0][1], 0.2225, 2e-4));
        assert!(near(shaper.colorants[1][1], 0.7169, 2e-4));
        assert!(near(shaper.colorants[2][2], 0.7141, 2e-4));
        // The sRGB EOTF at mid grey.
        assert!(near(shaper.trc[0].eval(0.5), 0.21404, 1e-4));
        assert_eq!(profile_id(profile.bytes()), profile.bytes()[84..100]);
    }

    #[test]
    fn vcgt_tables_and_formulas_decode_like_lcms() {
        let mut generated = DisplayProfile::srgb();
        let ramp: Vec<u16> = (0..256u32).map(|i| (i * 65535 / 255 / 2) as u16).collect();
        generated.vcgt = Some([ramp.clone(), ramp.clone(), ramp]);
        let profile = Profile::parse(&generated.encode().unwrap()).unwrap();
        let Vcgt(curves) = profile.vcgt().unwrap();
        assert!(near(curves[0].eval(1.0), 0.5, 1e-4));
        assert!(near(curves[2].eval(0.5), 0.25, 1e-3));
        // A formula vcgt: gamma 1, min 0.1, max 0.9.
        let mut tag = b"vcgt\0\0\0\0".to_vec();
        tag.extend_from_slice(&1u32.to_be_bytes());
        for _ in 0..3 {
            for v in [1.0, 0.1, 0.9] {
                tag.extend_from_slice(&s15(v));
            }
        }
        let bytes = assemble(&[(*b"vcgt", tag)]);
        let Vcgt(curves) = Profile::parse(&bytes).unwrap().vcgt().unwrap();
        assert!(near(curves[1].eval(0.0), 0.1, 1e-4));
        assert!(near(curves[1].eval(1.0), 0.9, 1e-4));
    }

    #[test]
    fn malformed_profiles_are_rejected() {
        let bytes = DisplayProfile::srgb().encode().unwrap();
        assert!(Profile::parse(&bytes[..100]).is_err());
        let mut bad = bytes.clone();
        bad[36] = b'x';
        assert!(Profile::parse(&bad).is_err());
        let mut overrun = bytes.clone();
        overrun[128..132].copy_from_slice(&10_000u32.to_be_bytes());
        assert!(Profile::parse(&overrun).is_err());
        let mut tag_out = bytes;
        tag_out[136..140].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(Profile::parse(&tag_out).is_err());
    }

    #[test]
    fn bradford_adapts_d65_to_d50() {
        let m = bradford(D65.to_xyz(), D50);
        let out = mat_vec(&m, D65.to_xyz());
        for (a, b) in out.iter().zip(D50) {
            assert!(near(*a, b, 1e-4));
        }
    }
}
