//! Colour states and the transforms between them (P-DS-03, #341), as
//! Clutter's colour pipeline in Mutter 51 builds them.
//!
//! A colour state is what `wp_color_manager_v1` image descriptions,
//! outputs and surfaces carry: either parametric (primaries, transfer
//! function, luminances; `ClutterColorStateParams`) or an ICC profile
//! (`ClutterColorStateIcc`). Every state gets a unique id when it is
//! created (`clutter_color_state_get_id`), and two states compare as
//! Clutter's `equals` vfuncs do: parametric ones by colorimetry (to
//! 0.0001), transfer function and luminances (to 10 %), ICC ones by
//! profile checksum.
//!
//! The transform from a surface's state to an output's follows
//! `build_transform_pipeline` (`clutter-color-transform.c`): unpremultiply,
//! the source EOTF (or the ICC TRCs / 3D LUT) to linear BT.2020 D65 via
//! XYZ and Bradford adaptation, the luminance mapping
//! `(dst.ref / src.ref) * (src.max / dst.max)`, the target's inverse
//! EOTF, premultiply. Outputs are always parametric (Mutter's
//! `META_COLOR_MODE_DEFAULT`: sRGB primaries, gamma 2.2, the SDR
//! luminances), so after Clutter's combine pass every transform here
//! is one decode stage, one 3x3 matrix and one encode stage: the shape
//! [`Transform`] holds and the renderer's shader runs.
//!
//! The arithmetic mirrors Mutter's: single-precision ops with the same
//! constants (Clutter's D65 and D50 white points, the Bradford matrix,
//! the PQ constants, sign-preserving powers), so the CPU path in
//! [`Transform::apply`] is the reference the GL path is tested against.
//!
//! Sources (mutter `51.0`): `clutter/clutter/clutter-color-state*.c`,
//! `clutter-color-transform.c`, `clutter-color-op.c`,
//! `clutter-color-pipeline-shader.c`, `clutter-color-utils.c`,
//! `src/backends/meta-color-device.c` (`update_color_state`).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::icc;

/// Named colour spaces (`ClutterColorspace`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Colorspace {
    Srgb,
    Bt2020,
    Ntsc,
    Pal,
    P3,
}

/// CIE xy chromaticities of the three primaries and the white point.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Primaries {
    pub r: [f32; 2],
    pub g: [f32; 2],
    pub b: [f32; 2],
    pub w: [f32; 2],
}

impl Colorspace {
    /// Clutter's primaries tables (`clutter-color-utils.c`).
    pub fn primaries(self) -> Primaries {
        match self {
            Colorspace::Srgb => Primaries {
                r: [0.64, 0.33],
                g: [0.30, 0.60],
                b: [0.15, 0.06],
                w: [0.3127, 0.3290],
            },
            Colorspace::Ntsc => Primaries {
                r: [0.63, 0.34],
                g: [0.31, 0.595],
                b: [0.155, 0.07],
                w: [0.3127, 0.3290],
            },
            Colorspace::Bt2020 => Primaries {
                r: [0.708, 0.292],
                g: [0.170, 0.797],
                b: [0.131, 0.046],
                w: [0.3127, 0.3290],
            },
            Colorspace::P3 => Primaries {
                r: [0.68, 0.32],
                g: [0.265, 0.69],
                b: [0.15, 0.06],
                w: [0.3127, 0.329],
            },
            Colorspace::Pal => Primaries {
                r: [0.64, 0.33],
                g: [0.29, 0.60],
                b: [0.15, 0.06],
                w: [0.3127, 0.329],
            },
        }
    }
}

impl Primaries {
    /// `clutter_primaries_ensure_normalized_range`.
    pub fn clamped(self) -> Self {
        let c = |v: [f32; 2]| [v[0].clamp(0.0, 1.0), v[1].clamp(0.0, 1.0)];
        Self {
            r: c(self.r),
            g: c(self.g),
            b: c(self.b),
            w: c(self.w),
        }
    }

    /// `clutter_primaries_equal`: every chromaticity within 0.0001.
    pub fn approx_eq(&self, other: &Self) -> bool {
        let near = |a: [f32; 2], b: [f32; 2]| {
            approx(a[0], b[0], 0.0001) && approx(a[1], b[1], 0.0001)
        };
        near(self.r, other.r) && near(self.g, other.g) && near(self.b, other.b) && near(self.w, other.w)
    }
}

/// Primaries by name or explicitly (`ClutterColorimetry`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Colorimetry {
    Colorspace(Colorspace),
    Primaries(Primaries),
}

impl Colorimetry {
    pub fn primaries(&self) -> Primaries {
        match self {
            Colorimetry::Colorspace(space) => space.primaries(),
            Colorimetry::Primaries(p) => *p,
        }
    }

    /// `clutter_colorimetry_equal`.
    fn approx_eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Colorimetry::Colorspace(a), Colorimetry::Colorspace(b)) => a == b,
            _ => self.primaries().approx_eq(&other.primaries()),
        }
    }
}

/// Named transfer functions (`ClutterTransferFunction`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TransferFunction {
    SrgbPiecewise,
    Gamma22,
    Pq,
    Bt1886,
    Linear,
}

/// A named transfer function or a pure power (`ClutterEOTF`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Eotf {
    Named(TransferFunction),
    Gamma(f32),
}

impl Eotf {
    /// `clutter_eotf_equal`.
    fn approx_eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Eotf::Named(a), Eotf::Named(b)) => a == b,
            (Eotf::Gamma(a), Eotf::Gamma(b)) => approx(*a, *b, 0.0001),
            _ => false,
        }
    }

    /// `clutter_eotf_get_default_luminance`.
    pub fn default_luminance(self) -> Luminance {
        match self {
            Eotf::Named(TransferFunction::Bt1886) => Luminance {
                min: 0.01,
                max: 100.0,
                reference: 100.0,
                mastering_max: 100.0,
            },
            Eotf::Named(TransferFunction::Pq) => Luminance {
                min: 0.005,
                max: 10000.0,
                reference: 203.0,
                mastering_max: 10000.0,
            },
            _ => Luminance::SDR,
        }
    }
}

/// Luminances in cd/m² (`ClutterLuminance`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Luminance {
    pub min: f32,
    pub max: f32,
    pub reference: f32,
    pub mastering_max: f32,
}

impl Luminance {
    /// `sdr_default_luminance`.
    pub const SDR: Self = Self {
        min: 0.2,
        max: 80.0,
        reference: 80.0,
        mastering_max: 80.0,
    };

    /// `clutter_luminance_equal`: ratios within 10 %.
    fn approx_eq(&self, other: &Self) -> bool {
        let near = |a: f32, b: f32| {
            if a == 0.0 || b == 0.0 {
                a == b
            } else {
                approx(a / b, 1.0, 0.1)
            }
        };
        near(self.min, other.min)
            && near(self.max, other.max)
            && near(self.mastering_max, other.mastering_max)
            && near(self.reference, other.reference)
    }
}

/// A parametric colour state (`ClutterColorStateParams`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Params {
    colorimetry: Colorimetry,
    eotf: Eotf,
    /// `CLUTTER_LUMINANCE_TYPE_EXPLICIT` values; `None` derives them
    /// from the transfer function.
    explicit: Option<Luminance>,
}

impl Params {
    /// `clutter_color_state_params_new_from_primitives`: explicit sRGB
    /// primaries collapse to the named space, and luminances apply only
    /// when `min >= 0`, `max > 0` and `ref >= 0` (PQ's maximum is
    /// always `min + 10000`).
    pub fn new(colorimetry: Colorimetry, eotf: Eotf, luminance: Option<Luminance>) -> Self {
        let colorimetry = match colorimetry {
            Colorimetry::Primaries(p) if p.approx_eq(&Colorspace::Srgb.primaries()) => {
                Colorimetry::Colorspace(Colorspace::Srgb)
            }
            other => other,
        };
        let eotf = match eotf {
            Eotf::Gamma(exp) if exp < 1.0 => Eotf::Named(TransferFunction::SrgbPiecewise),
            other => other,
        };
        let explicit = luminance
            .filter(|l| l.min >= 0.0 && l.max > 0.0 && l.reference >= 0.0)
            .map(|l| {
                let max = if eotf == Eotf::Named(TransferFunction::Pq) {
                    l.min + 10000.0
                } else {
                    l.max
                };
                Luminance {
                    min: l.min,
                    max,
                    reference: l.reference,
                    mastering_max: if l.mastering_max > 0.0 {
                        l.mastering_max
                    } else {
                        max
                    },
                }
            });
        Self {
            colorimetry,
            eotf,
            explicit,
        }
    }

    /// `clutter_color_state_params_new (context, colorspace, tf)`.
    pub fn named(space: Colorspace, tf: TransferFunction) -> Self {
        Self::new(Colorimetry::Colorspace(space), Eotf::Named(tf), None)
    }

    pub fn colorimetry(&self) -> Colorimetry {
        self.colorimetry
    }

    pub fn eotf(&self) -> Eotf {
        self.eotf
    }

    /// `clutter_color_state_params_get_luminance`.
    pub fn luminance(&self) -> Luminance {
        self.explicit.unwrap_or_else(|| self.eotf.default_luminance())
    }

    fn approx_eq(&self, other: &Self) -> bool {
        self.colorimetry.approx_eq(&other.colorimetry)
            && self.eotf.approx_eq(&other.eotf)
            && self.luminance().approx_eq(&other.luminance())
    }
}

/// Number of samples per channel of an ICC TRC (`TRC_SAMPLE_COUNT`).
pub const TRC_SAMPLES: usize = 256;
/// Grid points per axis of an ICC 3D LUT (`LUT_3D_SIZE`).
pub const LUT_3D_SIZE: usize = 33;

/// How an ICC profile's device values reach linear BT.2020.
#[derive(Debug, Clone, PartialEq)]
pub enum IccModel {
    /// Matrix/TRC (`cmsIsMatrixShaper`): the TRCs sampled at
    /// [`TRC_SAMPLES`] points (`None` when all three are linear), then
    /// colorants → D65 → BT.2020.
    MatrixShaper {
        trc: Option<Arc<[Vec<f32>; 3]>>,
        to_bt2020: Mat3,
    },
    /// Anything else: the profile's own transform to linear BT.2020,
    /// relative colorimetric, sampled on a [`LUT_3D_SIZE`]³ grid with
    /// red varying fastest.
    Lut3d(Arc<Vec<[f32; 3]>>),
}

/// An ICC colour state (`ClutterColorStateIcc`).
#[derive(Debug, Clone)]
pub struct Icc {
    bytes: Arc<[u8]>,
    checksum: [u8; 16],
    model: IccModel,
}

impl Icc {
    /// `clutter_color_state_icc_new`: an RGB profile with an XYZ PCS,
    /// identified by its header profile ID or the computed one.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let profile = icc::Profile::parse(bytes).map_err(|_| "Couldn't parse ICC profile")?;
        if &profile.color_space != b"RGB " || &profile.pcs != b"XYZ " {
            return Err("ICC profile unsupported".to_owned());
        }
        let mut checksum = [0u8; 16];
        checksum.copy_from_slice(&profile.bytes()[84..100]);
        if checksum == [0; 16] {
            checksum = icc::profile_id(profile.bytes());
        }
        let model = match profile.matrix_shaper() {
            Some(shaper) => matrix_shaper_model(&shaper),
            None => IccModel::Lut3d(Arc::new(lut3d_model(profile.bytes())?)),
        };
        Ok(Self {
            bytes: Arc::from(profile.bytes()),
            checksum,
            model,
        })
    }

    pub fn bytes(&self) -> &Arc<[u8]> {
        &self.bytes
    }

    pub fn model(&self) -> &IccModel {
        &self.model
    }
}

fn matrix_shaper_model(shaper: &icc::MatrixShaper) -> IccModel {
    // `cmsIsToneCurveLinear`: the 16-bit table within 0x0f of identity.
    let linear = |curve: &icc::Curve| {
        (0..4096).all(|i| {
            let x = f64::from(i) / 4095.0;
            ((curve.eval(x) - x) * 65535.0).abs() <= 15.0
        })
    };
    let trc = if shaper.trc.iter().all(linear) {
        None
    } else {
        let sample = |curve: &icc::Curve| -> Vec<f32> {
            (0..TRC_SAMPLES)
                .map(|i| curve.eval(i as f64 / (TRC_SAMPLES - 1) as f64) as f32)
                .collect()
        };
        Some(Arc::new([
            sample(&shaper.trc[0]),
            sample(&shaper.trc[1]),
            sample(&shaper.trc[2]),
        ]))
    };
    // `get_icc_to_linear_bt2020_matrix`: colorants (D50) → D65 → BT.2020.
    let c = shaper.colorants;
    let colorants = [
        [c[0][0], c[1][0], c[2][0]],
        [c[0][1], c[1][1], c[2][1]],
        [c[0][2], c[1][2], c[2][2]],
    ];
    let adapt = icc::bradford(D50_XYZ, D65_XYZ);
    let from_xyz = from_xyz(&Colorspace::Bt2020.primaries());
    IccModel::MatrixShaper {
        trc,
        to_bt2020: to_f32(&icc::mat_mul(&from_xyz, &icc::mat_mul(&adapt, &colorants))),
    }
}

/// `add_icc_to_linear_bt2020_op` for LUT-based profiles: the profile's
/// relative colorimetric transform to a linear BT.2020 profile, sampled
/// over the unit cube.
fn lut3d_model(bytes: &[u8]) -> Result<Vec<[f32; 3]>, String> {
    use moxcms::{ColorProfile, Layout, RenderingIntent, ToneReprCurve, TransformOptions};
    let source = ColorProfile::new_from_slice(bytes).map_err(|e| e.to_string())?;
    let mut linear = ColorProfile::new_bt2020();
    for trc in [&mut linear.red_trc, &mut linear.green_trc, &mut linear.blue_trc] {
        *trc = Some(ToneReprCurve::Lut(Vec::new()));
    }
    let transform = source
        .create_transform_f32(
            Layout::Rgb,
            &linear,
            Layout::Rgb,
            TransformOptions {
                rendering_intent: RenderingIntent::RelativeColorimetric,
                ..TransformOptions::default()
            },
        )
        .map_err(|e| e.to_string())?;
    let n = LUT_3D_SIZE;
    let step = 1.0 / (n - 1) as f32;
    let mut input = Vec::with_capacity(n * n * n * 3);
    for k in 0..n {
        for j in 0..n {
            for i in 0..n {
                input.extend_from_slice(&[i as f32 * step, j as f32 * step, k as f32 * step]);
            }
        }
    }
    let mut output = vec![0.0f32; input.len()];
    transform
        .transform(&input, &mut output)
        .map_err(|e| e.to_string())?;
    Ok(output
        .as_chunks::<3>()
        .0
        .iter()
        .map(|c| [c[0], c[1], c[2]])
        .collect())
}

/// What a colour state is.
#[derive(Debug, Clone)]
pub enum Kind {
    Params(Params),
    Icc(Icc),
}

/// A colour state with its creation-unique id.
#[derive(Debug)]
pub struct ColorState {
    id: u64,
    kind: Kind,
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

impl ColorState {
    pub fn new(kind: Kind) -> Arc<Self> {
        Arc::new(Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            kind,
        })
    }

    pub fn params(params: Params) -> Arc<Self> {
        Self::new(Kind::Params(params))
    }

    /// `clutter_color_state_get_id`.
    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn kind(&self) -> &Kind {
        &self.kind
    }

    /// `clutter_color_state_equals`.
    pub fn equals(&self, other: &Self) -> bool {
        if std::ptr::eq(self, other) {
            return true;
        }
        match (&self.kind, &other.kind) {
            (Kind::Params(a), Kind::Params(b)) => a.approx_eq(b),
            (Kind::Icc(a), Kind::Icc(b)) => a.checksum == b.checksum,
            _ => false,
        }
    }

    /// `clutter_color_state_get_luminance`: ICC states are SDR.
    pub fn luminance(&self) -> Luminance {
        match &self.kind {
            Kind::Params(p) => p.luminance(),
            Kind::Icc(_) => Luminance::SDR,
        }
    }
}

/// Clutter's D65 (`CLUTTER_D65_X/Y/Z`).
const D65_XYZ: [f64; 3] = [0.95047, 1.0, 1.08883];
/// Clutter's D50 (`CLUTTER_D50_X/Y/Z`).
const D50_XYZ: [f64; 3] = [0.9642, 1.0, 0.8251];

/// A 3x3 matrix applied to column vectors, row-major.
pub type Mat3 = [[f32; 3]; 3];

fn to_f32(m: &icc::Mat3) -> Mat3 {
    m.map(|row| row.map(|v| v as f32))
}

fn xy_to_xyz(xy: [f32; 2]) -> [f64; 3] {
    let (x, y) = (f64::from(xy[0]), f64::from(xy[1]).max(f64::from(f32::EPSILON)));
    [x / y, 1.0, (1.0 - x - y) / y]
}

/// `clutter_colorimetry_to_XYZ`.
fn to_xyz(p: &Primaries) -> icc::Mat3 {
    let xy = |v: [f32; 2]| icc::Xy::new(f64::from(v[0]), f64::from(v[1]));
    icc::rgb_to_xyz(&[xy(p.r), xy(p.g), xy(p.b)], xy(p.w)).unwrap_or(IDENTITY)
}

/// `clutter_colorimetry_from_XYZ`.
fn from_xyz(p: &Primaries) -> icc::Mat3 {
    icc::mat_inverse(&to_xyz(p)).unwrap_or(IDENTITY)
}

const IDENTITY: icc::Mat3 = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

/// `get_params_to_linear_bt2020`: source → XYZ → D65 → BT.2020.
fn params_to_bt2020(p: &Params) -> icc::Mat3 {
    let primaries = p.colorimetry.primaries();
    let adapt = icc::bradford(xy_to_xyz(primaries.w), D65_XYZ);
    let bt2020 = from_xyz(&Colorspace::Bt2020.primaries());
    icc::mat_mul(&bt2020, &icc::mat_mul(&adapt, &to_xyz(&primaries)))
}

/// `get_params_from_linear_bt2020`: BT.2020 → XYZ → target white → target.
fn params_from_bt2020(p: &Params) -> icc::Mat3 {
    let primaries = p.colorimetry.primaries();
    let adapt = icc::bradford(D65_XYZ, xy_to_xyz(primaries.w));
    let bt2020 = to_xyz(&Colorspace::Bt2020.primaries());
    icc::mat_mul(&from_xyz(&primaries), &icc::mat_mul(&adapt, &bt2020))
}

/// The first stage: device values to linear light.
#[derive(Debug, Clone, PartialEq)]
pub enum Decode {
    None,
    /// `gamma_power`: sign-preserving `|x|^p`.
    Power(f32),
    /// `srgb_piecewise_eotf`.
    Srgb,
    /// `pq_eotf` (normalized to 10000 cd/m²).
    Pq,
    /// `curve_1d`: per-channel [`TRC_SAMPLES`]-point curves.
    Curve(Arc<[Vec<f32>; 3]>),
    /// `3d_lut` (tetrahedral) followed by `clamp_unit`.
    Lut3d(Arc<Vec<[f32; 3]>>),
}

/// The last stage: linear light to the target's encoding.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Encode {
    None,
    Power(f32),
    /// `srgb_piecewise_inv_eotf`.
    Srgb,
    /// `pq_inv_eotf`.
    Pq,
}

/// A combined colour pipeline: decode, matrix, encode, between
/// unpremultiply and premultiply.
#[derive(Debug, Clone, PartialEq)]
pub struct Transform {
    pub decode: Decode,
    pub matrix: Mat3,
    pub encode: Encode,
}

/// `create_eotf_op`.
fn decode_of(eotf: Eotf) -> Decode {
    match eotf {
        Eotf::Gamma(exp) if approx(exp, 1.0, f32::EPSILON) => Decode::None,
        Eotf::Gamma(exp) => Decode::Power(exp),
        Eotf::Named(TransferFunction::SrgbPiecewise) => Decode::Srgb,
        Eotf::Named(TransferFunction::Pq) => Decode::Pq,
        Eotf::Named(TransferFunction::Bt1886) => Decode::Power(2.4),
        Eotf::Named(TransferFunction::Gamma22) => Decode::Power(2.2),
        Eotf::Named(TransferFunction::Linear) => Decode::None,
    }
}

/// `create_inv_eotf_op`.
fn encode_of(eotf: Eotf) -> Encode {
    match eotf {
        Eotf::Gamma(exp) if approx(exp, 1.0, f32::EPSILON) => Encode::None,
        Eotf::Gamma(exp) => Encode::Power(1.0 / exp),
        Eotf::Named(TransferFunction::SrgbPiecewise) => Encode::Srgb,
        Eotf::Named(TransferFunction::Pq) => Encode::Pq,
        Eotf::Named(TransferFunction::Bt1886) => Encode::Power(1.0 / 2.4),
        Eotf::Named(TransferFunction::Gamma22) => Encode::Power(1.0 / 2.2),
        Eotf::Named(TransferFunction::Linear) => Encode::None,
    }
}

/// `clutter_color_transform_new` from `source` to a parametric
/// `target`; `None` when the states are equal (an empty pipeline).
pub fn transform(source: &ColorState, target: &ColorState) -> Option<Transform> {
    if source.equals(target) {
        return None;
    }
    let Kind::Params(dst) = target.kind() else {
        // Outputs are parametric (`META_COLOR_MODE_DEFAULT`).
        return None;
    };
    let (decode, to_bt2020) = match source.kind() {
        Kind::Params(src) => (decode_of(src.eotf), params_to_bt2020(src)),
        Kind::Icc(icc) => match &icc.model {
            IccModel::MatrixShaper { trc, to_bt2020 } => (
                trc.clone().map_or(Decode::None, Decode::Curve),
                to_bt2020.map(|row| row.map(f64::from)),
            ),
            IccModel::Lut3d(lut) => (Decode::Lut3d(lut.clone()), IDENTITY),
        },
    };
    // `add_luminance_mapping_op`.
    let (src_lum, dst_lum) = (source.luminance(), target.luminance());
    let factor = if src_lum.max == dst_lum.max && src_lum.reference == dst_lum.reference {
        1.0
    } else {
        f64::from((dst_lum.reference / src_lum.reference) * (src_lum.max / dst_lum.max))
    };
    let scaled = to_bt2020.map(|row| row.map(|v| v * factor));
    Some(Transform {
        decode,
        matrix: to_f32(&icc::mat_mul(&params_from_bt2020(dst), &scaled)),
        encode: encode_of(dst.eotf),
    })
}

fn approx(a: f32, b: f32, epsilon: f32) -> bool {
    (a - b).abs() < epsilon
}

fn power(v: f32, p: f32) -> f32 {
    let r = v.abs().powf(p);
    if v < 0.0 {
        -r
    } else {
        r
    }
}

fn srgb_eotf(v: f32) -> f32 {
    let a = v.abs();
    let r = if a <= 0.04045 {
        a / 12.92
    } else {
        ((a + 0.055) / 1.055).powf(12.0 / 5.0)
    };
    if v < 0.0 {
        -r
    } else {
        r
    }
}

fn srgb_inv_eotf(v: f32) -> f32 {
    let a = v.abs();
    let r = if a <= 0.0031308 {
        a * 12.92
    } else {
        a.powf(5.0 / 12.0) * 1.055 - 0.055
    };
    if v < 0.0 {
        -r
    } else {
        r
    }
}

const PQ_C1: f32 = 0.8359375;
const PQ_C2: f32 = 18.8515625;
const PQ_C3: f32 = 18.6875;
const PQ_M1: f32 = 0.1593017;
const PQ_M2: f32 = 78.84375;

fn pq_eotf(v: f32) -> f32 {
    let p = v.clamp(0.0, 1.0).powf(1.0 / PQ_M2);
    let num = (p - PQ_C1).max(0.0);
    let den = PQ_C2 - PQ_C3 * p;
    (num / den).powf(1.0 / PQ_M1)
}

fn pq_inv_eotf(v: f32) -> f32 {
    let p = v.clamp(0.0, 1.0).powf(PQ_M1);
    ((PQ_C1 + PQ_C2 * p) / (1.0 + PQ_C3 * p)).powf(PQ_M2)
}

/// `curve_1d` with linear filtering between samples.
fn curve(samples: &[f32], v: f32) -> f32 {
    let n = samples.len();
    let x = v.clamp(0.0, 1.0) * (n - 1) as f32;
    let i = (x.floor() as usize).min(n - 1);
    let j = (i + 1).min(n - 1);
    let t = x - i as f32;
    samples[i] + (samples[j] - samples[i]) * t
}

/// `sample_3d_lut_SIZE`: tetrahedral interpolation, as the shader does.
fn lut3d(lut: &[[f32; 3]], c: [f32; 3]) -> [f32; 3] {
    let n = LUT_3D_SIZE;
    let max = (n - 1) as f32;
    let scaled = c.map(|v| v.clamp(0.0, 1.0) * max);
    let lo = scaled.map(|v| v.floor());
    let hi = lo.map(|v| (v + 1.0).min(max));
    let t = [scaled[0] - lo[0], scaled[1] - lo[1], scaled[2] - lo[2]];
    let at = |x: f32, y: f32, z: f32| lut[(z as usize * n + y as usize) * n + x as usize];
    let v000 = at(lo[0], lo[1], lo[2]);
    let v111 = at(hi[0], hi[1], hi[2]);
    let [x0, y0, z0] = lo;
    let [x1, y1, z1] = hi;
    let combine = |terms: [([f32; 3], [f32; 3]); 3]| {
        [0, 1, 2].map(|k| {
            v000[k]
                + t[0] * (terms[0].0[k] - terms[0].1[k])
                + t[1] * (terms[1].0[k] - terms[1].1[k])
                + t[2] * (terms[2].0[k] - terms[2].1[k])
        })
    };
    if t[0] > t[1] {
        if t[1] > t[2] {
            let v100 = at(x1, y0, z0);
            let v110 = at(x1, y1, z0);
            combine([(v100, v000), (v110, v100), (v111, v110)])
        } else if t[0] > t[2] {
            let v100 = at(x1, y0, z0);
            let v101 = at(x1, y0, z1);
            combine([(v100, v000), (v111, v101), (v101, v100)])
        } else {
            let v001 = at(x0, y0, z1);
            let v101 = at(x1, y0, z1);
            combine([(v101, v001), (v111, v101), (v001, v000)])
        }
    } else if t[2] > t[1] {
        let v001 = at(x0, y0, z1);
        let v011 = at(x0, y1, z1);
        combine([(v111, v011), (v011, v001), (v001, v000)])
    } else if t[2] > t[0] {
        let v010 = at(x0, y1, z0);
        let v011 = at(x0, y1, z1);
        combine([(v111, v011), (v010, v000), (v011, v010)])
    } else {
        let v010 = at(x0, y1, z0);
        let v110 = at(x1, y1, z0);
        combine([(v110, v010), (v010, v000), (v111, v110)])
    }
}

impl Transform {
    /// Run the pipeline on one premultiplied RGBA colour, as
    /// `clutter_color_pipeline_do_transform` and the shader do.
    pub fn apply(&self, rgba: [f32; 4]) -> [f32; 4] {
        let a = rgba[3];
        let mut c = [rgba[0], rgba[1], rgba[2]];
        if a > 0.0 {
            c = c.map(|v| v / a);
        }
        c = match &self.decode {
            Decode::None => c,
            Decode::Power(p) => c.map(|v| power(v, *p)),
            Decode::Srgb => c.map(srgb_eotf),
            Decode::Pq => c.map(pq_eotf),
            Decode::Curve(curves) => [
                curve(&curves[0], c[0]),
                curve(&curves[1], c[1]),
                curve(&curves[2], c[2]),
            ],
            Decode::Lut3d(lut) => lut3d(lut, c).map(|v| v.clamp(0.0, 1.0)),
        };
        let m = &self.matrix;
        c = [
            m[0][0] * c[0] + m[0][1] * c[1] + m[0][2] * c[2],
            m[1][0] * c[0] + m[1][1] * c[1] + m[1][2] * c[2],
            m[2][0] * c[0] + m[2][1] * c[1] + m[2][2] * c[2],
        ];
        c = match self.encode {
            Encode::None => c,
            Encode::Power(p) => c.map(|v| power(v, p)),
            Encode::Srgb => c.map(srgb_inv_eotf),
            Encode::Pq => c.map(pq_inv_eotf),
        };
        [c[0] * a, c[1] * a, c[2] * a, a]
    }
}

/// The colour state of every output: `update_color_state` for
/// `META_COLOR_MODE_DEFAULT` (sRGB, gamma 2.2, the gamma 2.2 default
/// luminances, reference factor 1). Each output owns one object, as
/// each Mutter colour device does.
pub fn output_params() -> Params {
    Params::new(
        Colorimetry::Colorspace(Colorspace::Srgb),
        Eotf::Named(TransferFunction::Gamma22),
        Some(Luminance::SDR),
    )
}

/// `clutter_context_get_default_color_state`: what surfaces without an
/// image description are.
pub fn default_params() -> Params {
    Params::named(Colorspace::Srgb, TransferFunction::Gamma22)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: f32, b: f32, eps: f32) -> bool {
        (a - b).abs() <= eps
    }

    fn output() -> Arc<ColorState> {
        ColorState::params(output_params())
    }

    #[test]
    fn default_surfaces_need_no_transform_on_sdr_outputs() {
        let default = ColorState::params(default_params());
        let output = output();
        assert_ne!(default.id(), output.id(), "ids are per object");
        assert!(default.equals(&output));
        assert!(transform(&default, &output).is_none());
        // Explicit sRGB primaries collapse to the named space.
        let explicit = ColorState::params(Params::new(
            Colorimetry::Primaries(Colorspace::Srgb.primaries()),
            Eotf::Named(TransferFunction::Gamma22),
            None,
        ));
        assert!(explicit.equals(&output));
        // set_tf_power(2.2) arrives named, a power 2.4 does not.
        assert!(!ColorState::params(Params::new(
            Colorimetry::Colorspace(Colorspace::Srgb),
            Eotf::Gamma(2.4),
            None
        ))
        .equals(&output));
    }

    #[test]
    fn luminances_compare_within_ten_percent_and_pq_max_is_fixed() {
        let lum = |reference| {
            Params::new(
                Colorimetry::Colorspace(Colorspace::Srgb),
                Eotf::Named(TransferFunction::Gamma22),
                Some(Luminance {
                    min: 0.2,
                    max: 80.0,
                    reference,
                    mastering_max: 0.0,
                }),
            )
        };
        assert!(lum(85.0).approx_eq(&output_params()));
        assert!(!lum(100.0).approx_eq(&output_params()));
        let pq = Params::new(
            Colorimetry::Colorspace(Colorspace::Bt2020),
            Eotf::Named(TransferFunction::Pq),
            Some(Luminance {
                min: 0.01,
                max: 1000.0,
                reference: 203.0,
                mastering_max: 0.0,
            }),
        );
        assert_eq!(pq.luminance().max, 10000.01);
        assert_eq!(pq.luminance().mastering_max, 10000.01);
    }

    #[test]
    fn bt2020_matrix_matches_the_reference_conversion() {
        // BT.2020 linear → sRGB linear (ITU-R BT.2087), Clutter's D65
        // rounding leaving it within 1e-3.
        let source = ColorState::params(Params::named(Colorspace::Bt2020, TransferFunction::Linear));
        let target = ColorState::params(Params::named(Colorspace::Srgb, TransferFunction::Linear));
        let t = transform(&source, &target).unwrap();
        let expected = [
            [1.6605, -0.5876, -0.0728],
            [-0.1246, 1.1329, -0.0083],
            [-0.0182, -0.1006, 1.1187],
        ];
        for (row, want) in t.matrix.iter().zip(expected) {
            for (v, w) in row.iter().zip(want) {
                assert!(near(*v, w, 1e-3), "{:?}", t.matrix);
            }
        }
        assert_eq!(t.decode, Decode::None);
        assert_eq!(t.encode, Encode::None);
    }

    #[test]
    fn pq_reference_white_lands_on_sdr_white() {
        let source = ColorState::params(Params::named(Colorspace::Bt2020, TransferFunction::Pq));
        let t = transform(&source, &output()).unwrap();
        assert_eq!(t.decode, Decode::Pq);
        assert_eq!(t.encode, Encode::Power(1.0 / 2.2));
        // 203 cd/m² in PQ (BT.2408 reference white).
        let white = pq_inv_eotf(203.0 / 10000.0);
        assert!(near(white, 0.58069, 1e-4));
        let out = t.apply([white, white, white, 1.0]);
        for v in &out[..3] {
            assert!(near(*v, 1.0, 2e-3), "{out:?}");
        }
        // Premultiplied input stays premultiplied.
        let half = t.apply([white * 0.5, white * 0.5, white * 0.5, 0.5]);
        assert!(near(half[0], 0.5, 1e-3) && half[3] == 0.5, "{half:?}");
    }

    #[test]
    fn srgb_linear_and_compound_power_round_trip_through_gamma() {
        let linear = ColorState::params(Params::named(Colorspace::Srgb, TransferFunction::Linear));
        let t = transform(&linear, &output()).unwrap();
        let out = t.apply([0.214, 0.5, 1.0, 1.0]);
        assert!(near(out[0], 0.214f32.powf(1.0 / 2.2), 2e-3), "{out:?}");
        assert!(near(out[2], 1.0, 2e-3));
        let piecewise =
            ColorState::params(Params::named(Colorspace::Srgb, TransferFunction::SrgbPiecewise));
        let t = transform(&piecewise, &output()).unwrap();
        assert_eq!(t.decode, Decode::Srgb);
        let out = t.apply([0.5, 0.5, 0.5, 1.0]);
        assert!(near(out[0], srgb_eotf(0.5).powf(1.0 / 2.2), 2e-3), "{out:?}");
    }

    #[test]
    fn negative_values_keep_their_sign_through_powers() {
        assert_eq!(power(-0.25, 0.5), -0.5);
        assert!(srgb_eotf(-0.5) < 0.0 && srgb_inv_eotf(-0.5) < 0.0);
    }

    #[test]
    fn matrix_shaper_icc_matches_the_parametric_path() {
        let bytes = icc::DisplayProfile::srgb().encode().unwrap();
        let icc_state = ColorState::new(Kind::Icc(Icc::parse(&bytes).unwrap()));
        let Kind::Icc(parsed) = icc_state.kind() else {
            unreachable!()
        };
        assert!(matches!(
            parsed.model(),
            IccModel::MatrixShaper { trc: Some(_), .. }
        ));
        let again = ColorState::new(Kind::Icc(Icc::parse(&bytes).unwrap()));
        assert!(icc_state.equals(&again), "same checksum");
        let params =
            ColorState::params(Params::named(Colorspace::Srgb, TransferFunction::SrgbPiecewise));
        let via_icc = transform(&icc_state, &output()).unwrap();
        let via_params = transform(&params, &output()).unwrap();
        for c in [[0.2, 0.4, 0.8, 1.0], [0.9, 0.1, 0.5, 1.0]] {
            let (a, b) = (via_icc.apply(c), via_params.apply(c));
            for k in 0..3 {
                assert!(near(a[k], b[k], 4e-3), "{a:?} vs {b:?}");
            }
        }
    }

    #[test]
    fn icc_needs_rgb_with_an_xyz_pcs() {
        let mut bytes = icc::DisplayProfile::srgb().encode().unwrap();
        bytes[20..24].copy_from_slice(b"Lab ");
        assert_eq!(Icc::parse(&bytes).unwrap_err(), "ICC profile unsupported");
        assert!(Icc::parse(b"not a profile").is_err());
    }

    #[test]
    fn tetrahedral_lut_reproduces_an_identity_grid() {
        let n = LUT_3D_SIZE;
        let step = 1.0 / (n - 1) as f32;
        let mut lut = Vec::new();
        for k in 0..n {
            for j in 0..n {
                for i in 0..n {
                    lut.push([i as f32 * step, j as f32 * step, k as f32 * step]);
                }
            }
        }
        for c in [[0.3, 0.7, 0.1], [0.9, 0.2, 0.55], [0.5, 0.5, 0.5], [0.1, 0.4, 0.9]] {
            let out = lut3d(&lut, c);
            for k in 0..3 {
                assert!(near(out[k], c[k], 1e-5), "{c:?} → {out:?}");
            }
        }
    }
}
