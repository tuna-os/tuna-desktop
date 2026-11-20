//! Which monitor an absolute input device drives, and how its position
//! becomes a desktop position: Mutter 51's input mapper, shared by
//! touchscreens (P-DS-04, #345) and drawing tablets (P-IN-05, #344).
//!
//! Ported from mutter 51.0:
//!
//! - `src/backends/meta-input-mapper.c`: the per-device candidates
//!   (`guess_candidates`, `match_edid`, `match_size`, `match_config`,
//!   `monitor_has_twin`) and the touchscreen fallback to the built-in
//!   monitor when nothing matched;
//! - `src/backends/meta-monitor-manager.c`: the device matrix
//!   (`meta_monitor_manager_get_monitor_matrix`, `calculate_viewport_matrix`,
//!   `transform_matrices`);
//! - `src/backends/native/meta-input-device-native.c`: absolute positions
//!   (`meta_input_device_native_translate_coordinates_in_impl`).
//!
//! The device matrix maps the device's normalized position, in the
//! panel's native orientation, onto the monitor's logical rectangle
//! under the monitor's transform, so a touchscreen or display tablet
//! follows its rotated panel. libwacom-unknown tablets and every
//! touchscreen count as integrated and not built in, as Mutter treats
//! them (`guess_candidates` consults libwacom for tablets only).

use smithay::utils::{Logical, Point, Rectangle};

/// One monitor an input device may be mapped to (`MetaMonitor`).
#[derive(Debug, Clone, PartialEq)]
pub struct Monitor {
    pub connector: String,
    pub vendor: String,
    pub product: String,
    pub serial: String,
    pub width_mm: i32,
    pub height_mm: i32,
    /// The logical monitor's rectangle in the desktop.
    pub rect: Rectangle<i32, Logical>,
    /// wl_output transform (0..=7).
    pub transform: u32,
    /// The current mode's size in pixels.
    pub mode: (i32, i32),
    /// `meta_monitor_manager_get_builtin_monitor`: the laptop panel.
    pub builtin: bool,
}

/// What a device reports for matching (`MetaMapperInputInfo`).
#[derive(Debug, Clone, PartialEq)]
pub struct InputIdentity {
    pub name: String,
    /// Physical size in millimetres, when libinput knows it.
    pub size_mm: Option<(f64, f64)>,
}

/// The device classes Mutter maps (`get_device_settings`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputKind {
    Touchscreen,
    Tablet,
}

/// `MetaOutputMatchType`: each match sets bit `1 << type` of a score.
pub const MATCH_EDID_VENDOR: u32 = 0;
pub const MATCH_EDID_PARTIAL: u32 = 1;
pub const MATCH_EDID_FULL: u32 = 2;
pub const MATCH_SIZE: u32 = 3;
pub const MATCH_CONFIG: u32 = 5;

/// `MAX_SIZE_MATCH_DIFF`.
const MAX_SIZE_MATCH_DIFF: f64 = 0.05;

/// `match_config`: the user's `output` names this monitor (the fourth
/// value, the connector, only disambiguates EDID twins).
fn match_config(output: &[String], monitor: &Monitor, monitors: &[Monitor]) -> bool {
    if output.len() < 3 || output[..3].iter().all(String::is_empty) {
        return false;
    }
    let same =
        |m: &Monitor| m.vendor == output[0] && m.product == output[1] && m.serial == output[2];
    if !same(monitor) {
        return false;
    }
    let twin = monitors
        .iter()
        .any(|m| m.connector != monitor.connector && same(m));
    if output.len() >= 4 && twin {
        return monitor.connector == output[3];
    }
    true
}

fn contains_ci(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

/// `match_edid`: the device name mentions the monitor's vendor, then its
/// serial (full) or a word of its product (partial).
fn match_edid(name: &str, monitor: &Monitor) -> Option<u32> {
    if !contains_ci(name, &monitor.vendor) {
        return None;
    }
    if contains_ci(name, &monitor.serial) {
        return Some(MATCH_EDID_FULL);
    }
    if monitor
        .product
        .split(' ')
        .any(|word| contains_ci(name, word))
    {
        return Some(MATCH_EDID_PARTIAL);
    }
    Some(MATCH_EDID_VENDOR)
}

/// `match_size`: within 5 % of the monitor's physical size.
fn match_size(size_mm: Option<(f64, f64)>, monitor: &Monitor) -> bool {
    let Some((w, h)) = size_mm else {
        return false;
    };
    let w_diff = (1.0 - f64::from(monitor.width_mm) / w).abs();
    let h_diff = (1.0 - f64::from(monitor.height_mm) / h).abs();
    w_diff < MAX_SIZE_MATCH_DIFF && h_diff < MAX_SIZE_MATCH_DIFF
}

/// The monitor a device was mapped to and the score it won with (0 for
/// the touchscreen fallback to the built-in monitor).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Picked {
    pub index: usize,
    pub score: u32,
}

/// `guess_candidates` and `mapping_helper_apply`: the monitor this device
/// drives, or `None` for the whole desktop. `output` is the user value of
/// the device's `output` key (`None` when unset, Mutter's automatic
/// mapping): set, it decides alone; otherwise the name and size pick.
/// A touchscreen nothing matched takes the built-in monitor.
pub fn pick_monitor(
    kind: InputKind,
    device: &InputIdentity,
    output: Option<&[String]>,
    monitors: &[Monitor],
) -> Option<Picked> {
    let mut best: Option<Picked> = None;
    for (index, monitor) in monitors.iter().enumerate() {
        let mut score = 0u32;
        match output {
            None => {
                if let Some(kind) = match_edid(&device.name, monitor) {
                    score |= 1 << kind;
                }
                if match_size(device.size_mm, monitor) {
                    score |= 1 << MATCH_SIZE;
                }
            }
            Some(output) => {
                if match_config(output, monitor, monitors) {
                    score |= 1 << MATCH_CONFIG;
                }
            }
        }
        // g_array_sort is not stable; ties keep the first monitor here.
        if score > 0 && best.is_none_or(|b| score > b.score) {
            best = Some(Picked { index, score });
        }
    }
    best.or_else(|| match kind {
        InputKind::Touchscreen => monitors
            .iter()
            .position(|m| m.builtin)
            .map(|index| Picked { index, score: 0 }),
        InputKind::Tablet => None,
    })
}

/// `transform_matrices` in meta-monitor-manager.c, by wl_output transform.
const TRANSFORM_MATRICES: [[f64; 6]; 8] = [
    [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
    [0.0, -1.0, 1.0, 1.0, 0.0, 0.0],
    [-1.0, 0.0, 1.0, 0.0, -1.0, 1.0],
    [0.0, 1.0, 0.0, -1.0, 0.0, 1.0],
    [-1.0, 0.0, 1.0, 0.0, 1.0, 0.0],
    [0.0, 1.0, 0.0, 1.0, 0.0, 0.0],
    [1.0, 0.0, 0.0, 0.0, -1.0, 1.0],
    [0.0, -1.0, 1.0, -1.0, 0.0, 1.0],
];

/// The desktop's extents: Mutter's screen size, from the origin.
pub fn stage_size(monitors: &[Monitor]) -> (f64, f64) {
    monitors.iter().fold((0.0, 0.0), |(w, h), m| {
        (
            w.max(f64::from(m.rect.loc.x + m.rect.size.w)),
            h.max(f64::from(m.rect.loc.y + m.rect.size.h)),
        )
    })
}

/// How one device's normalized position becomes a desktop position
/// (`MetaInputDeviceNative`'s device matrix and aspect ratios).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AbsoluteMapping {
    /// `meta_monitor_manager_get_monitor_matrix`, or the identity for the
    /// whole desktop.
    pub matrix: [f64; 6],
    /// The mapped monitor's (or desktop's) aspect ratio when the device
    /// keeps its aspect (tablets' keep-aspect); 0 otherwise.
    pub output_ratio: f64,
    /// The device's own width / height, 0 when unknown or unused.
    pub device_ratio: f64,
}

impl AbsoluteMapping {
    /// `mapper_input_info_set_output`: the matrix onto `monitor` (the
    /// identity without one), with ratios off, and the aspect ratio of
    /// the monitor's mode (or the desktop) a keep-aspect device uses.
    pub fn new(monitor: Option<&Monitor>, monitors: &[Monitor]) -> (Self, f64) {
        let stage = stage_size(monitors);
        let (matrix, (width, height)) = match monitor {
            Some(monitor) if stage.0 > 0.0 && stage.1 > 0.0 => {
                // calculate_viewport_matrix, then multiply_matrix.
                let a = [
                    f64::from(monitor.rect.size.w) / stage.0,
                    0.0,
                    f64::from(monitor.rect.loc.x) / stage.0,
                    0.0,
                    f64::from(monitor.rect.size.h) / stage.1,
                    f64::from(monitor.rect.loc.y) / stage.1,
                ];
                let t = TRANSFORM_MATRICES[(monitor.transform & 7) as usize];
                (
                    [
                        a[0] * t[0] + a[1] * t[3],
                        a[0] * t[1] + a[1] * t[4],
                        a[0] * t[2] + a[1] * t[5] + a[2],
                        a[3] * t[0] + a[4] * t[3],
                        a[3] * t[1] + a[4] * t[4],
                        a[3] * t[2] + a[4] * t[5] + a[5],
                    ],
                    monitor.mode,
                )
            }
            _ => (
                [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                (stage.0 as i32, stage.1 as i32),
            ),
        };
        let ratio = if height > 0 {
            f64::from(width) / f64::from(height)
        } else {
            0.0
        };
        (
            Self {
                matrix,
                output_ratio: 0.0,
                device_ratio: 0.0,
            },
            ratio,
        )
    }

    fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        let m = self.matrix;
        (m[0] * x + m[1] * y + m[2], m[3] * x + m[4] * y + m[5])
    }

    /// `translate_coordinates_in_impl` for a libinput position already
    /// normalized to [0, 1] (`get_x_transformed` over the desktop).
    pub fn translate(&self, norm: (f64, f64), stage: (f64, f64)) -> Point<f64, Logical> {
        let (mut x, mut y) = norm;
        if self.output_ratio > 0.0 && self.device_ratio > 0.0 {
            let ratio = self.device_ratio / self.output_ratio;
            if ratio > 1.0 {
                x *= ratio;
            } else if ratio < 1.0 {
                y *= 1.0 / ratio;
            }
        }
        let (min_x, min_y) = self.apply(0.0, 0.0);
        let (max_x, max_y) = self.apply(1.0, 1.0);
        let (x, y) = self.apply(x, y);
        (
            x.clamp(min_x.min(max_x), min_x.max(max_x)) * stage.0,
            y.clamp(min_y.min(max_y), min_y.max(max_y)) * stage.1,
        )
            .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(connector: &str, x: i32, w: i32, h: i32) -> Monitor {
        Monitor {
            connector: connector.into(),
            vendor: "GSM".into(),
            product: "LG ULTRAFINE".into(),
            serial: connector.into(),
            width_mm: 600,
            height_mm: 340,
            rect: Rectangle::new((x, 0).into(), (w, h).into()),
            transform: 0,
            mode: (w, h),
            builtin: false,
        }
    }

    fn pen() -> InputIdentity {
        InputIdentity {
            name: "Wacom Intuos Pro M Pen".into(),
            size_mm: Some((224.0, 148.0)),
        }
    }

    fn touchscreen() -> InputIdentity {
        InputIdentity {
            name: "ELAN9008:00 04F3:2C82".into(),
            size_mm: Some((293.0, 165.0)),
        }
    }

    fn index(picked: Option<Picked>) -> Option<usize> {
        picked.map(|p| p.index)
    }

    #[test]
    fn edid_twins_are_told_apart_by_the_connector() {
        let mut a = monitor("DP-1", 0, 1920, 1080);
        let mut b = monitor("DP-2", 1920, 1920, 1080);
        a.serial = "SAME".into();
        b.serial = "SAME".into();
        let monitors = [a, b];
        let output = |connector: &str| {
            vec![
                "GSM".to_owned(),
                "LG ULTRAFINE".to_owned(),
                "SAME".to_owned(),
                connector.to_owned(),
            ]
        };
        let pick = |output: &[String]| {
            index(pick_monitor(
                InputKind::Tablet,
                &pen(),
                Some(output),
                &monitors,
            ))
        };
        assert_eq!(pick(&output("DP-2")), Some(1));
        assert_eq!(pick(&output("DP-1")), Some(0));
        // Three values match the first twin.
        assert_eq!(pick(&output("DP-2")[..3]), Some(0));
    }

    #[test]
    fn display_tablets_find_their_screen_by_name_or_size() {
        let mut cintiq = monitor("DP-2", 1920, 1920, 1080);
        cintiq.vendor = "WAC".into();
        cintiq.product = "Cintiq 16".into();
        cintiq.width_mm = 345;
        cintiq.height_mm = 194;
        let monitors = [monitor("DP-1", 0, 1920, 1080), cintiq];
        let by_name = InputIdentity {
            name: "Wacom Cintiq 16 Pen".into(),
            size_mm: None,
        };
        // "Wacom" holds "WAC" and "Cintiq" a product word.
        let picked = pick_monitor(InputKind::Tablet, &by_name, None, &monitors);
        assert_eq!(
            picked,
            Some(Picked {
                index: 1,
                score: 1 << MATCH_EDID_PARTIAL
            })
        );
        let by_size = InputIdentity {
            name: "Generic display pen".into(),
            size_mm: Some((344.0, 195.0)),
        };
        assert_eq!(
            pick_monitor(InputKind::Tablet, &by_size, None, &monitors),
            Some(Picked {
                index: 1,
                score: 1 << MATCH_SIZE
            })
        );
        // A set output wins over the guess.
        let settings_output = vec!["GSM".into(), "LG ULTRAFINE".into(), "DP-1".into()];
        assert_eq!(
            index(pick_monitor(
                InputKind::Tablet,
                &by_name,
                Some(&settings_output),
                &monitors
            )),
            Some(0)
        );
    }

    #[test]
    fn touchscreens_fall_back_to_the_builtin_panel() {
        let mut panel = monitor("eDP-1", 0, 1920, 1080);
        panel.vendor = "BOE".into();
        panel.product = "0x0a1c".into();
        panel.builtin = true;
        let monitors = [monitor("DP-1", 1920, 2560, 1440), panel];
        // Automatic: no EDID name or size match, so the built-in panel
        // (guess_candidates' touchscreen fallback, score 0).
        assert_eq!(
            pick_monitor(InputKind::Touchscreen, &touchscreen(), None, &monitors),
            Some(Picked { index: 1, score: 0 })
        );
        // A tablet nothing matched spans the desktop instead.
        assert_eq!(
            pick_monitor(InputKind::Tablet, &touchscreen(), None, &monitors),
            None
        );
        // A configured output decides (MATCH_CONFIG) …
        let external = vec!["GSM".into(), "LG ULTRAFINE".into(), "DP-1".into()];
        assert_eq!(
            pick_monitor(
                InputKind::Touchscreen,
                &touchscreen(),
                Some(&external),
                &monitors
            ),
            Some(Picked {
                index: 0,
                score: 1 << MATCH_CONFIG
            })
        );
        // … and one naming no monitor falls back to the panel too.
        let unknown = vec!["XYZ".into(), "Nope".into(), "1".into()];
        assert_eq!(
            pick_monitor(
                InputKind::Touchscreen,
                &touchscreen(),
                Some(&unknown),
                &monitors
            ),
            Some(Picked { index: 1, score: 0 })
        );
        // A screen of the touchscreen's size wins over the fallback.
        let mut portable = monitor("DP-2", 0, 1920, 1080);
        portable.width_mm = 294;
        portable.height_mm = 166;
        let monitors = [monitors[1].clone(), portable];
        assert_eq!(
            pick_monitor(InputKind::Touchscreen, &touchscreen(), None, &monitors),
            Some(Picked {
                index: 1,
                score: 1 << MATCH_SIZE
            })
        );
        // No built-in panel and no match: the whole desktop.
        assert_eq!(
            pick_monitor(
                InputKind::Touchscreen,
                &touchscreen(),
                None,
                &[monitor("DP-1", 0, 1920, 1080)]
            ),
            None
        );
    }

    #[test]
    fn touch_on_a_rotated_panel_lands_where_mutter_puts_it() {
        // A 1920x1080 panel at transform t covers this logical rect.
        let place = |t: u32| {
            let mut panel = monitor("eDP-1", 0, 1920, 1080);
            panel.builtin = true;
            panel.transform = t;
            if t % 2 == 1 {
                panel.rect = Rectangle::new((0, 0).into(), (1080, 1920).into());
            }
            let monitors = [panel];
            let (mapping, _) = AbsoluteMapping::new(Some(&monitors[0]), &monitors);
            let stage = stage_size(&monitors);
            move |nx: f64, ny: f64| {
                let p = mapping.translate((nx, ny), stage);
                (p.x, p.y)
            }
        };
        // The panel's native corners: (0,0) top-left, (1,1) bottom-right.
        let normal = place(0);
        assert_eq!(normal(0.25, 0.75), (480.0, 810.0));
        // 90°: the panel's bottom-left shows the logical top-left.
        let quarter = place(1);
        assert_eq!(quarter(0.0, 1.0), (0.0, 0.0));
        assert_eq!(quarter(0.0, 0.0), (1080.0, 0.0));
        assert_eq!(quarter(0.25, 0.1), (0.9 * 1080.0, 0.25 * 1920.0));
        let half = place(2);
        assert_eq!(half(0.25, 0.75), (1440.0, 270.0));
        let three = place(3);
        assert_eq!(three(1.0, 0.0), (0.0, 0.0));
        assert_eq!(three(0.25, 0.1), (0.1 * 1080.0, 0.75 * 1920.0));
    }

    #[test]
    fn mutters_device_matrix_agrees_with_the_rendered_frame() {
        // Smithay renders an output with its transform inverted
        // (`orientation::render_transform`), so the logical point shown
        // on panel pixel p is the transform applied to p in the panel
        // frame. Mutter's matrix must name that same logical point for
        // every transform, or a touch would hit the wrong content.
        use smithay::utils::Size;
        let (w, h) = (8.0, 4.0);
        for t in 0..=7u32 {
            let swapped = crate::orientation::transform_swaps_dimensions(t);
            let (lw, lh) = if swapped { (h, w) } else { (w, h) };
            let mut panel = monitor("eDP-1", 0, lw as i32, lh as i32);
            panel.transform = t;
            panel.mode = (w as i32, h as i32);
            let monitors = [panel];
            let (mapping, _) = AbsoluteMapping::new(Some(&monitors[0]), &monitors);
            let stage = stage_size(&monitors);
            for (px, py) in [(0.0, 0.0), (8.0, 0.0), (0.0, 4.0), (2.0, 1.0), (6.5, 3.25)] {
                let shown = crate::orientation::smithay_transform(t)
                    .transform_point_in(Point::<f64, Logical>::from((px, py)), &Size::from((w, h)));
                let touched = mapping.translate((px / w, py / h), stage);
                assert!(
                    (touched.x - shown.x).abs() < 1e-9 && (touched.y - shown.y).abs() < 1e-9,
                    "t={t} panel ({px},{py}): touch {touched:?}, shown {shown:?}"
                );
            }
        }
    }
}
