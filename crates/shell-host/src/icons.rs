//! Icon theme lookup: names to pixels, no toolkit.
//!
//! A central resolver answers theme icon names with decoded
//! artwork the existing pixmap paint paths blit directly. Lookup
//! follows the Freedesktop layout (theme dirs, hicolor fallback,
//! `/usr/share/pixmaps`); raster decodes through the `image`
//! crate, SVG through `resvg`. Misses are not a type — callers
//! keep their initial-letter fallback. Everything here is pure:
//! the host owns caching and tick-driven refresh.

use std::path::{Path, PathBuf};

/// Decoded icon artwork in shm `B,G,R,A` order, square at `size`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artwork {
    /// Edge length in pixels.
    pub size: u32,
    /// `size * size * 4` bytes.
    pub argb: Vec<u8>,
}

/// Largest source file decoded (raster and vector alike).
pub const MAX_ICON_BYTES: usize = 8 * 1024 * 1024;
/// Raster extensions decoded through the `image` crate.
const RASTER_EXTS: [&str; 2] = ["png", "jpg"];
/// Largest edge the resolver will produce; bigger requests clamp.
pub const MAX_ICON_PX: u32 = 256;

/// Base icon directories in lookup order: user dir, system dirs,
/// then the legacy pixmaps catch-all.
pub fn theme_dirs() -> Vec<PathBuf> {
    #[cfg(test)]
    if let Some(dirs) = test_dirs::current() {
        return dirs;
    }
    theme_dirs_from(|key| std::env::var_os(key))
}

/// [`theme_dirs`] over an explicit environment lookup.
fn theme_dirs_from(var: impl Fn(&str) -> Option<std::ffi::OsString>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = var("XDG_DATA_HOME").map(PathBuf::from) {
        dirs.push(home.join("icons"));
    } else if let Some(home) = var("HOME").map(PathBuf::from) {
        dirs.push(home.join(".local/share/icons"));
    }
    let data_dirs = var("XDG_DATA_DIRS")
        .map(|dirs| {
            std::env::split_paths(&dirs)
                .map(|dir| dir.join("icons"))
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|| {
            vec![
                PathBuf::from("/usr/local/share/icons"),
                PathBuf::from("/usr/share/icons"),
            ]
        });
    dirs.extend(data_dirs);
    dirs.push(PathBuf::from("/usr/share/pixmaps"));
    dirs
}

/// Icon base directories for test fixtures, per thread. Tests share
/// one process, and others spawn children (bus and portal daemons,
/// apps) that inherit its environment: a fixture that pointed
/// `XDG_DATA_DIRS` or `HOME` at a temporary tree would hand those
/// children a session with no schemas or service files. Fixtures
/// install an override here instead and leave the environment alone.
#[cfg(test)]
pub(crate) mod test_dirs {
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};

    thread_local! {
        static DIRS: RefCell<Option<Vec<PathBuf>>> = const { RefCell::new(None) };
    }

    pub(super) fn current() -> Option<Vec<PathBuf>> {
        DIRS.with(|dirs| dirs.borrow().clone())
    }

    /// Lookup on this thread as in a session whose data home is
    /// `root` and whose system data dirs are empty; cleared on drop.
    pub(crate) struct Fixture;

    impl Fixture {
        pub(crate) fn install(root: &Path) -> Self {
            let dirs = super::theme_dirs_from(|key| match key {
                "XDG_DATA_HOME" => Some(root.into()),
                "XDG_DATA_DIRS" => Some(root.join("empty-dirs").into()),
                "HOME" => Some(root.join("home").into()),
                _ => None,
            });
            DIRS.with(|current| *current.borrow_mut() = Some(dirs));
            Self
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            DIRS.with(|current| *current.borrow_mut() = None);
        }
    }
}

/// Find the best file for `name` at `size` px under `theme`,
/// falling back to hicolor. Absolute names bypass theme lookup.
/// Returns `None` when nothing matches.
pub fn find_icon(theme: &str, name: &str, size: u32) -> Option<PathBuf> {
    if name.starts_with('/') {
        let path = PathBuf::from(name);
        return path.is_file().then_some(path);
    }
    if name.contains('/') || name.contains('\0') {
        return None;
    }
    let size = size.clamp(1, MAX_ICON_PX);
    for theme_name in [theme, "hicolor"] {
        if let Some(path) = find_in_theme(theme_name, name, size) {
            return Some(path);
        }
    }
    None
}

/// Scan one theme directory tree for `name`: exact-size raster
/// first, then scalable vector, then anything at all.
fn find_in_theme(theme: &str, name: &str, size: u32) -> Option<PathBuf> {
    let mut scalable: Option<PathBuf> = None;
    let mut any: Option<PathBuf> = None;
    for base in theme_dirs() {
        let root = if base.ends_with("pixmaps") {
            base
        } else {
            base.join(theme)
        };
        let mut stack = vec![(root, 0u32)];
        while let Some((dir, depth)) = stack.pop() {
            if depth > 8 {
                continue;
            }
            let Ok(read) = std::fs::read_dir(&dir) else {
                continue;
            };
            for child in read.flatten() {
                let path = child.path();
                if path.is_dir() {
                    stack.push((path, depth + 1));
                    continue;
                }
                if path.file_stem().and_then(|stem| stem.to_str()) != Some(name) {
                    continue;
                }
                let ext = path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .unwrap_or_default()
                    .to_lowercase();
                if ext == "svg" {
                    if path.components().any(|part| part.as_os_str() == "scalable") {
                        if scalable.is_none() {
                            scalable = Some(path);
                        }
                    } else if any.is_none() {
                        any = Some(path.clone());
                    }
                    continue;
                }
                if !RASTER_EXTS.contains(&ext.as_str()) {
                    continue;
                }
                if dir_size_hint(&dir) == Some(size) {
                    return Some(path);
                }
                if any.is_none() {
                    any = Some(path);
                }
            }
        }
        // Prefer an exact raster hit from an earlier base; vector
        // only wins when no raster matched anywhere.
        if any.as_ref().is_some_and(|path| {
            path.extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| RASTER_EXTS.contains(&ext.to_lowercase().as_str()))
        }) {
            return any;
        }
    }
    any.or(scalable)
}

/// Directory size hint: the largest `NNxNN` component in the path,
/// if the theme lays out sizes that way.
fn dir_size_hint(dir: &Path) -> Option<u32> {
    dir.components()
        .filter_map(|part| {
            let text = part.as_os_str().to_str()?;
            let (w, h) = text.split_once('x')?;
            let w: u32 = w.parse().ok()?;
            let h: u32 = h.parse().ok()?;
            (w == h).then_some(w)
        })
        .max()
}

/// Decode `path` scaled to `px` px square. Raster goes through the
/// `image` crate; SVG renders via `resvg` without font support
/// (`<text>` elements render blank: v1 keeps the link small).
/// `None` on any failure — the caller falls back.
pub fn decode_icon(path: &Path, px: u32) -> Option<Artwork> {
    let px = px.clamp(1, MAX_ICON_PX);
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() > MAX_ICON_BYTES {
        return None;
    }
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_lowercase();
    if ext == "svg" {
        return decode_svg(&bytes, px);
    }
    let image = image::load_from_memory(&bytes).ok()?.to_rgba8();
    let scaled = image::imageops::resize(&image, px, px, image::imageops::FilterType::Triangle);
    Some(Artwork {
        size: px,
        argb: rgba_to_shm(&scaled.into_raw()),
    })
}

/// Render SVG bytes to `px` px square artwork.
fn decode_svg(bytes: &[u8], px: u32) -> Option<Artwork> {
    // resvg 0.48: usvg parses, the tree renders into tiny-skia.
    let tree = resvg::usvg::Tree::from_data(bytes, &resvg::usvg::Options::default()).ok()?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(px, px)?;
    let scale = px as f32 / tree.size().width().max(tree.size().height()).max(1.0);
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    let mut argb = pixmap.take();
    // tiny-skia stores premultiplied pixels; unpremultiply back to
    // straight alpha for the shm buffer.
    for px in argb.as_chunks_mut::<4>().0 {
        let a = px[3] as u32;
        if a == 0 || a == 255 {
            continue;
        }
        // tiny-skia order is RGBA premultiplied.
        px[0] = ((px[0] as u32 * 255 / a).min(255)) as u8;
        px[1] = ((px[1] as u32 * 255 / a).min(255)) as u8;
        px[2] = ((px[2] as u32 * 255 / a).min(255)) as u8;
    }
    // RGBA straight -> B,G,R,A.
    let (chunks, _) = argb.as_chunks::<4>();
    let mut out = Vec::with_capacity(argb.len());
    for px in chunks {
        out.push(px[2]);
        out.push(px[1]);
        out.push(px[0]);
        out.push(px[3]);
    }
    Some(Artwork {
        size: px,
        argb: out,
    })
}

/// RGBA bytes to shm `B,G,R,A` order.
fn rgba_to_shm(rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgba.len());
    let (chunks, _) = rgba.as_chunks::<4>();
    for px in chunks {
        out.push(px[2]);
        out.push(px[1]);
        out.push(px[0]);
        out.push(px[3]);
    }
    out
}

/// Resolve `name` under `theme` to `px` px artwork: find plus
/// decode. `None` means keep the caller's fallback.
pub fn resolve(theme: &str, name: &str, px: u32) -> Option<Artwork> {
    let path = find_icon(theme, name, px)?;
    decode_icon(&path, px)
}

/// Re-tint artwork toward `color` (v1 symbolic semantics): RGB
/// becomes the tint, alpha is preserved. Transparent pixels stay
/// transparent.
pub fn retint(art: &Artwork, color: [u8; 4]) -> Artwork {
    let (chunks, _) = art.argb.as_chunks::<4>();
    let mut out = Vec::with_capacity(art.argb.len());
    for px in chunks {
        out.push(color[0]);
        out.push(color[1]);
        out.push(color[2]);
        out.push(px[3]);
    }
    Artwork {
        size: art.size,
        argb: out,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    /// Unlikely stems so `/usr/share/pixmaps` (always on the lookup
    /// path) can never shadow the fixture.
    const TOOL: &str = "tuna-icontool";
    const VEC: &str = "tuna-iconvec";
    const OTHER: &str = "tuna-iconother";
    const SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64"><rect width="64" height="64" fill="#ff0000"/></svg>"##;

    /// Point lookup on this thread at `tmp`, away from real system dirs.
    fn install(tmp: &tempfile::TempDir) -> test_dirs::Fixture {
        test_dirs::Fixture::install(tmp.path())
    }

    fn write_png(path: &std::path::Path, size: u32, pixel: [u8; 4]) {
        std::fs::create_dir_all(path.parent().expect("fixture parent")).expect("mkdirs");
        let mut img = image::RgbaImage::new(size, size);
        for px in img.pixels_mut() {
            *px = image::Rgba(pixel);
        }
        img.save(path).expect("save png");
    }

    fn write_bytes(path: &std::path::Path, bytes: &[u8]) {
        std::fs::create_dir_all(path.parent().expect("fixture parent")).expect("mkdirs");
        std::fs::write(path, bytes).expect("write fixture");
    }

    /// `<tmp>/icons/{Test,hicolor}` tree; callers [`install`] it.
    fn fixture() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        let icons = tmp.path().join("icons");
        write_png(
            &icons.join("Test/16x16/apps").join(format!("{TOOL}.png")),
            16,
            [255, 0, 0, 255],
        );
        write_png(
            &icons.join("Test/32x32/apps").join(format!("{TOOL}.png")),
            32,
            [0, 255, 0, 255],
        );
        write_bytes(
            &icons.join("Test/scalable/apps").join(format!("{TOOL}.svg")),
            SVG.as_bytes(),
        );
        write_bytes(
            &icons.join("Test/scalable/apps").join(format!("{VEC}.svg")),
            SVG.as_bytes(),
        );
        write_png(
            &icons
                .join("hicolor/48x48/apps")
                .join(format!("{OTHER}.png")),
            48,
            [0, 0, 255, 255],
        );
        tmp
    }

    fn ext(path: &std::path::Path) -> &str {
        path.extension().and_then(|ext| ext.to_str()).unwrap_or("")
    }

    #[test]
    fn exact_size_raster_wins() {
        let tmp = fixture();
        let _dirs = install(&tmp);
        let found16 = find_icon("Test", TOOL, 16).expect("16px icon");
        assert!(
            found16.ends_with(format!("16x16/apps/{TOOL}.png")),
            "got {found16:?}"
        );
        let found32 = find_icon("Test", TOOL, 32).expect("32px icon");
        assert!(
            found32.ends_with(format!("32x32/apps/{TOOL}.png")),
            "got {found32:?}"
        );
    }

    #[test]
    fn any_raster_beats_scalable_without_exact_match() {
        let tmp = fixture();
        let _dirs = install(&tmp);
        // No 24x24 dir exists, but rasters do: any raster must win
        // over the scalable vector, not the svg.
        let found = find_icon("Test", TOOL, 24).expect("24px icon");
        assert_eq!(ext(&found), "png", "got {found:?}");
        assert!(
            !found
                .components()
                .any(|part| part.as_os_str() == "scalable"),
            "got {found:?}"
        );
    }

    #[test]
    fn svg_only_name_resolves_to_svg() {
        let tmp = fixture();
        let _dirs = install(&tmp);
        let found = find_icon("Test", VEC, 48).expect("svg icon");
        assert_eq!(ext(&found), "svg", "got {found:?}");
    }

    #[test]
    fn absolute_path_bypasses_lookup() {
        let tmp = fixture();
        let _dirs = install(&tmp);
        let real = tmp
            .path()
            .join("icons/Test/16x16/apps")
            .join(format!("{TOOL}.png"));
        let found = find_icon("NoSuchTheme", real.to_str().expect("utf8"), 16);
        assert_eq!(found, Some(real));
        // Absolute but missing is still a miss, not a fallback.
        let missing = tmp.path().join("nope.png");
        assert_eq!(find_icon("Test", missing.to_str().expect("utf8"), 16), None);
    }

    #[test]
    fn slash_null_and_missing_names_return_none() {
        let tmp = fixture();
        let _dirs = install(&tmp);
        assert_eq!(find_icon("Test", "apps/tool", 16), None);
        assert_eq!(find_icon("Test", "tool\0x", 16), None);
        assert_eq!(find_icon("Test", "no-such-tuna-icon", 16), None);
    }

    #[test]
    fn decode_png_scales_to_requested_px() {
        let tmp = fixture();
        let path = tmp
            .path()
            .join("icons/Test/16x16/apps")
            .join(format!("{TOOL}.png"));
        let art = decode_icon(&path, 16).expect("decode png");
        assert_eq!(art.size, 16);
        assert_eq!(art.argb.len(), 16 * 16 * 4);
        // Opaque red source in shm B,G,R,A order.
        assert_eq!(&art.argb[..4], &[0, 0, 255, 255]);
        let small = decode_icon(&path, 8).expect("decode scaled png");
        assert_eq!(small.size, 8);
        assert_eq!(small.argb.len(), 8 * 8 * 4);
    }

    #[test]
    fn decode_svg_scales_to_requested_px() {
        let tmp = fixture();
        let path = tmp
            .path()
            .join("icons/Test/scalable/apps")
            .join(format!("{VEC}.svg"));
        let art = decode_icon(&path, 24).expect("decode svg");
        assert_eq!(art.size, 24);
        assert_eq!(art.argb.len(), 24 * 24 * 4);
        // Full-bleed opaque red rect in shm B,G,R,A order.
        assert_eq!(&art.argb[..4], &[0, 0, 255, 255]);
    }

    #[test]
    fn decode_rejects_oversized_and_garbage() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let big = tmp.path().join("big.png");
        write_bytes(&big, &vec![0u8; MAX_ICON_BYTES + 1]);
        assert_eq!(decode_icon(&big, 16), None);
        let garbage = tmp.path().join("garbage.png");
        write_bytes(&garbage, b"this is not an image");
        assert_eq!(decode_icon(&garbage, 16), None);
        let bad_svg = tmp.path().join("bad.svg");
        write_bytes(&bad_svg, b"<svg nope");
        assert_eq!(decode_icon(&bad_svg, 16), None);
    }

    #[test]
    fn resolve_end_to_end() {
        let tmp = fixture();
        let _dirs = install(&tmp);
        let art = resolve("Test", TOOL, 16).expect("resolve");
        assert_eq!(art.size, 16);
        assert_eq!(art.argb.len(), 16 * 16 * 4);
        assert_eq!(&art.argb[..4], &[0, 0, 255, 255]);
        // hicolor fallback for a name the theme itself lacks.
        let other = resolve("Test", OTHER, 48).expect("hicolor resolve");
        assert_eq!(other.size, 48);
        assert_eq!(other.argb.len(), 48 * 48 * 4);
        assert_eq!(&other.argb[..4], &[255, 0, 0, 255]);
        assert_eq!(resolve("Test", "no-such-tuna-icon", 16), None);
    }

    #[test]
    fn theme_dirs_read_data_home_then_home_then_data_dirs() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |key: &str| {
                pairs
                    .iter()
                    .find(|(name, _)| *name == key)
                    .map(|(_, value)| OsString::from(value))
            }
        };
        let dirs = |paths: &[&str]| paths.iter().map(PathBuf::from).collect::<Vec<_>>();
        assert_eq!(
            theme_dirs_from(env(&[
                ("XDG_DATA_HOME", "/d"),
                ("HOME", "/h"),
                ("XDG_DATA_DIRS", "/a:/b"),
            ])),
            dirs(&["/d/icons", "/a/icons", "/b/icons", "/usr/share/pixmaps"])
        );
        assert_eq!(
            theme_dirs_from(env(&[("HOME", "/h")])),
            dirs(&[
                "/h/.local/share/icons",
                "/usr/local/share/icons",
                "/usr/share/icons",
                "/usr/share/pixmaps",
            ])
        );
    }

    /// Fixtures redirect lookup without touching the process
    /// environment other tests' children inherit, and only on the
    /// installing thread.
    #[test]
    fn fixture_dirs_leave_the_process_environment_alone() {
        let keys = ["XDG_DATA_HOME", "XDG_DATA_DIRS", "HOME"];
        let before = keys.map(std::env::var_os);
        let tmp = fixture();
        let dirs = install(&tmp);
        assert_eq!(keys.map(std::env::var_os), before);
        assert_eq!(theme_dirs()[0], tmp.path().join("icons"));
        assert!(find_icon("Test", TOOL, 16).is_some());
        let elsewhere = std::thread::spawn(theme_dirs)
            .join()
            .expect("lookup thread");
        assert!(!elsewhere.contains(&tmp.path().join("icons")));
        drop(dirs);
        assert!(!theme_dirs().contains(&tmp.path().join("icons")));
    }

    #[test]
    fn retint_applies_tint_and_preserves_alpha() {
        let art = Artwork {
            size: 1,
            argb: vec![10, 20, 30, 255, 40, 50, 60, 0],
        };
        let out = retint(&art, [1, 2, 3, 4]);
        assert_eq!(out.size, 1);
        assert_eq!(out.argb, vec![1, 2, 3, 255, 1, 2, 3, 0]);
    }

    #[test]
    fn dir_size_hint_reads_largest_square() {
        assert_eq!(
            dir_size_hint(std::path::Path::new("/tmp/icons/Test/16x16/apps")),
            Some(16)
        );
        assert_eq!(
            dir_size_hint(std::path::Path::new("/tmp/icons/Test/32x32/apps")),
            Some(32)
        );
        assert_eq!(
            dir_size_hint(std::path::Path::new("/tmp/icons/Test/scalable/apps")),
            None
        );
    }
}
