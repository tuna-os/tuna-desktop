//! GNOME Shell 51 `st-cursor.c` / KDE scalable cursor format.
//! Metadata and SVGs are loaded once per shape and physical output size.

use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

use crate::cursor::{clamp_size, CursorImage, Found};

type Frames = Vec<(CursorImage, u32)>;

const MAX_FILE_BYTES: usize = 1024 * 1024;
const MAX_FRAMES: usize = 1024;
const MAX_FRAME_SIDE: u32 = 4096;
const MAX_PIXELS_BYTES: usize = 64 * 1024 * 1024;

#[derive(Deserialize)]
struct Frame {
    filename: String,
    nominal_size: f64,
    #[serde(default)]
    hotspot_x: f64,
    #[serde(default)]
    hotspot_y: f64,
    #[serde(default)]
    delay: u32,
}

pub fn raster_size(size: u32, scale: f64) -> u32 {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    (f64::from(clamp_size(size)) * scale)
        .ceil()
        .clamp(1.0, 4096.0) as u32
}

fn read_bounded(path: &Path) -> Option<Vec<u8>> {
    if !std::fs::metadata(path).ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .ok()?
        .take((MAX_FILE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() <= MAX_FILE_BYTES).then_some(bytes)
}

pub fn load_frames(metadata: &Path, wanted: u32) -> Option<Frames> {
    if wanted == 0 || wanted > MAX_FRAME_SIDE {
        return None;
    }
    let descriptions: Vec<Frame> = serde_json::from_slice(&read_bounded(metadata)?).ok()?;
    if descriptions.is_empty() || descriptions.len() > MAX_FRAMES {
        return None;
    }
    let dir = metadata.parent()?;
    let mut total = 0usize;
    let mut frames = Vec::with_capacity(descriptions.len());
    for description in descriptions {
        // Theme-directory symlinks remain valid aliases. Metadata frame names
        // must be local relative paths, never absolute paths or parent escapes.
        let file = Path::new(&description.filename);
        if description.filename.is_empty()
            || file
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
            || !description.nominal_size.is_finite()
            || description.nominal_size <= 0.0
            || !description.hotspot_x.is_finite()
            || !description.hotspot_y.is_finite()
        {
            return None;
        }
        let options = resvg::usvg::Options {
            // Cursor vectors have no authority to load arbitrary external files.
            image_href_resolver: resvg::usvg::ImageHrefResolver {
                resolve_string: Box::new(|_, _| None),
                resolve_data: Box::new(|_, _, _| None),
            },
            ..Default::default()
        };
        // Plain SVG only: accepting SVGZ here would inflate a tiny input
        // before the parser's tree limits could apply.
        let svg = read_bounded(&dir.join(file))?;
        let tree = resvg::usvg::Tree::from_str(std::str::from_utf8(&svg).ok()?, &options).ok()?;
        let factor = f64::from(wanted) / description.nominal_size;
        let width = (f64::from(tree.size().width()) * factor).ceil();
        let height = (f64::from(tree.size().height()) * factor).ceil();
        let hotspot = (
            description.hotspot_x * factor,
            description.hotspot_y * factor,
        );
        if !width.is_finite()
            || !height.is_finite()
            || width < 1.0
            || height < 1.0
            || width > f64::from(MAX_FRAME_SIDE)
            || height > f64::from(MAX_FRAME_SIDE)
            || !hotspot.0.is_finite()
            || !hotspot.1.is_finite()
            || hotspot.0.abs() > f64::from(MAX_FRAME_SIDE)
            || hotspot.1.abs() > f64::from(MAX_FRAME_SIDE)
        {
            return None;
        }
        let (width, height) = (width as u32, height as u32);
        total = total.checked_add(
            (width as usize)
                .checked_mul(height as usize)?
                .checked_mul(4)?,
        )?;
        if total > MAX_PIXELS_BYTES {
            return None;
        }
        let mut pixels = resvg::tiny_skia::Pixmap::new(width, height)?;
        resvg::render(
            &tree,
            resvg::tiny_skia::Transform::from_scale(factor as f32, factor as f32),
            &mut pixels.as_mut(),
        );
        let mut pixels = pixels.take();
        // tiny-skia premultiplied RGBA -> compositor premultiplied ARGB8888.
        for pixel in pixels.as_chunks_mut::<4>().0 {
            pixel.swap(0, 2);
        }
        frames.push((
            CursorImage {
                nominal: wanted,
                width,
                height,
                hotspot,
                pixels,
            },
            description.delay,
        ));
    }
    Some(frames)
}

pub fn find_frames(
    dirs: &[PathBuf],
    chain: &[String],
    names: &[&str],
    wanted: u32,
) -> Option<(Found, Frames)> {
    for name in names {
        for theme in chain {
            for dir in dirs {
                let path = dir
                    .join(theme)
                    .join("cursors_scalable")
                    .join(name)
                    .join("metadata.json");
                if let Some(frames) = load_frames(&path, wanted) {
                    return Some((
                        Found {
                            path,
                            theme: theme.clone(),
                            name: (*name).into(),
                        },
                        frames,
                    ));
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("red.svg"),
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="12"><rect width="24" height="12" fill="red"/></svg>"#).unwrap();
        std::fs::write(dir.path().join("green.svg"),
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="12"><rect width="24" height="12" fill="lime"/></svg>"#).unwrap();
        dir
    }

    #[test]
    fn scalable_frames_preserve_nominal_geometry_hotspots_delays_and_argb() {
        let dir = fixture();
        let metadata = dir.path().join("metadata.json");
        std::fs::write(
            &metadata,
            r#"[
          {"filename":"red.svg","nominal_size":24,"hotspot_x":2.5,"hotspot_y":3.5,"delay":25},
          {"filename":"green.svg","nominal_size":24,"hotspot_x":4,"hotspot_y":5,"delay":75}
        ]"#,
        )
        .unwrap();
        let frames = load_frames(&metadata, 48).unwrap();
        assert_eq!(frames.len(), 2);
        let first = &frames[0].0;
        assert_eq!((first.width, first.height), (48, 24));
        assert_eq!(first.hotspot, (5.0, 7.0));
        assert_eq!(first.logical(24), ((24.0, 12.0), (2.5, 3.5)));
        assert_eq!(&first.pixels[..4], &[0, 0, 255, 255]);
        assert_eq!(&frames[1].0.pixels[..4], &[0, 255, 0, 255]);
        assert_eq!((frames[0].1, frames[1].1), (25, 75));
        // Fractional output rasterization uses30physical px, preserving2.5logical hotspot.
        let frame = &load_frames(&metadata, raster_size(24, 1.25)).unwrap()[0].0;
        assert_eq!((frame.width, frame.height), (30, 15));
        assert_eq!(frame.logical(24), ((24.0, 12.0), (2.5, 3.5)));
    }

    #[test]
    fn transparent_svg_pixels_stay_premultiplied() {
        let dir = fixture();
        std::fs::write(dir.path().join("red.svg"),
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="12"><rect width="24" height="12" fill="red" opacity="0.5"/></svg>"#).unwrap();
        let metadata = dir.path().join("metadata.json");
        std::fs::write(&metadata, r#"[{"filename":"red.svg","nominal_size":24}]"#).unwrap();
        let frames = load_frames(&metadata, 24).unwrap();
        let pixel = &frames[0].0.pixels[..4];
        assert_eq!(pixel[0], 0);
        assert_eq!(pixel[1], 0);
        assert_eq!(pixel[2], pixel[3]);
        assert!((127..=128).contains(&pixel[3]));
        assert_eq!(frames[0].1, 0);
    }

    #[test]
    fn malformed_missing_escape_and_excessive_frames_are_rejected() {
        let dir = fixture();
        let metadata = dir.path().join("metadata.json");
        for text in [
            "{}",
            "[]",
            "null",
            "[{}]",
            r#"[{"filename":"missing.svg","nominal_size":24}]"#,
            r#"[{"filename":"../red.svg","nominal_size":24}]"#,
            r#"[{"filename":"/red.svg","nominal_size":24}]"#,
            r#"[{"filename":"red.svg","nominal_size":0}]"#,
            r#"[{"filename":"red.svg","nominal_size":0.000001}]"#,
            r#"[{"filename":"red.svg","nominal_size":24,"delay":-1}]"#,
        ] {
            std::fs::write(&metadata, text).unwrap();
            assert!(load_frames(&metadata, 24).is_none(), "{text}");
        }
        let descriptions =
            vec![serde_json::json!({"filename":"red.svg","nominal_size":24}); MAX_FRAMES + 1];
        std::fs::write(&metadata, serde_json::to_vec(&descriptions).unwrap()).unwrap();
        assert!(load_frames(&metadata, 24).is_none());
        assert!(load_frames(&metadata, 0).is_none());
        assert!(load_frames(&metadata, 4097).is_none());
    }

    #[test]
    fn inheritance_and_directory_aliases_find_svg_metadata() {
        let dir = fixture();
        let shape = dir.path().join("Base/cursors_scalable/default");
        std::fs::create_dir_all(&shape).unwrap();
        std::fs::copy(dir.path().join("red.svg"), shape.join("red.svg")).unwrap();
        std::fs::write(
            shape.join("metadata.json"),
            r#"[{"filename":"red.svg","nominal_size":24}]"#,
        )
        .unwrap();
        std::os::unix::fs::symlink("default", shape.parent().unwrap().join("left_ptr")).unwrap();
        let (found, frames) = find_frames(
            &[dir.path().into()],
            &["Selected".into(), "Base".into()],
            &["missing", "left_ptr"],
            24,
        )
        .unwrap();
        assert_eq!(found.theme, "Base");
        assert_eq!(found.name, "left_ptr");
        assert_eq!(frames.len(), 1);
    }

    #[test]
    fn physical_raster_size_tracks_fractional_scales_and_is_bounded() {
        for (scale, expected) in [
            (1.0, 24),
            (1.25, 30),
            (1.5, 36),
            (2.0, 48),
            (f64::NAN, 24),
            (0.0, 24),
            (f64::INFINITY, 24),
            (1000.0, 4096),
        ] {
            assert_eq!(raster_size(24, scale), expected);
        }
    }
}
