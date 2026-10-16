//! The compositor's own pointer (#342): GNOME51/KDE SVG and Xcursor
//! themes from `org.gnome.desktop.interface cursor-theme` and
//! `cursor-size`, crisp at every output scale; `wp_cursor_shape_v1`
//! shapes drawn from that theme; client cursor surfaces; the move and
//! resize cursors of the compositor's own grabs; and `locate-pointer`'s
//! ripples on a lone Ctrl tap (GNOME Shell's `locatePointer.js`).
//!
//! Themes are found the way libXcursor finds them: `XCURSOR_PATH`, or
//! the icon directories of the XDG data dirs, then each theme's
//! `Inherits=` chain from its `index.theme`, then `default`. When no
//! theme has a cursor a built-in arrow, drawn at the requested size,
//! stands in, so the pointer never disappears.
//!
//! The hot-corner ripples of #513 are not on main yet; the ripple drawn
//! here is a small self-contained one and should move onto that shared
//! rendering once it lands.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::memory::{
    MemoryRenderBuffer, MemoryRenderBufferRenderElement,
};
use smithay::backend::renderer::element::surface::{
    render_elements_from_surface_tree, WaylandSurfaceRenderElement,
};
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::input::keyboard::xkb;
use smithay::input::pointer::{CursorIcon, CursorImageStatus, CursorImageSurfaceData};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::Resource;
use smithay::utils::{Logical, Physical, Point, Rectangle, Transform};

use crate::windows::ManagerInput;

/// GNOME 51's `cursor-size` default, also used for 0 and nonsense.
pub const DEFAULT_SIZE: u32 = 24;
/// Largest theme size honored; GNOME Settings offers up to 96.
const MAX_SIZE: u32 = 256;
/// Theme used for an empty `cursor-theme`, and the last stop of every
/// inheritance chain after libXcursor's `default`.
pub const DEFAULT_THEME: &str = "Adwaita";
/// Inheritance chains deeper than this are treated as cycles.
const MAX_INHERIT_DEPTH: usize = 16;
const MAX_THEMES: usize = 64;
const MAX_DIRS: usize = 32;
const MAX_CURSOR_BYTES: usize = 8 * 1024 * 1024;
const MAX_INDEX_BYTES: usize = 64 * 1024;
const MAX_CACHE_ENTRIES: usize = 64;
const MAX_CACHE_BYTES: usize = 64 * 1024 * 1024;

fn theme_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && name != "."
        && name != ".."
        && !name.contains(['/', '\0'])
}

fn read_bounded(path: &Path, limit: usize) -> Option<Vec<u8>> {
    if !std::fs::metadata(path).ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .ok()?
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() <= limit).then_some(bytes)
}

/// The icon directories Xcursor themes are looked up in, in order:
/// `XCURSOR_PATH` when set (`~` expanded), else libXcursor's and the
/// XDG icon-theme spec's directories.
pub fn search_path(var: impl Fn(&str) -> Option<String>) -> Vec<PathBuf> {
    let home = var("HOME").filter(|h| !h.is_empty());
    let expand = |entry: &str| -> Option<PathBuf> {
        match (entry.strip_prefix('~'), &home) {
            (Some(rest), Some(home)) => Some(PathBuf::from(format!("{home}{rest}"))),
            (Some(_), None) => None,
            (None, _) => Some(PathBuf::from(entry)),
        }
    };
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(path) = var("XCURSOR_PATH").filter(|p| !p.is_empty()) {
        dirs.extend(
            path.split(':')
                .filter(|e| !e.is_empty() && e.len() <= 4096)
                .take(MAX_DIRS - 1)
                .filter_map(expand),
        );
    } else {
        let data_home = var("XDG_DATA_HOME")
            .filter(|d| !d.is_empty())
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|h| Path::new(h).join(".local/share")));
        dirs.extend(data_home.map(|d| d.join("icons")));
        dirs.extend(home.as_ref().map(|h| Path::new(h).join(".icons")));
        let data_dirs = var("XDG_DATA_DIRS")
            .filter(|d| !d.is_empty())
            .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
        dirs.extend(
            data_dirs
                .split(':')
                .filter(|d| !d.is_empty() && d.len() <= 4096)
                .take(MAX_DIRS - 4)
                .map(|d| Path::new(d).join("icons")),
        );
        dirs.push(PathBuf::from("/usr/share/pixmaps"));
    }
    // GNOME Shell embeds Adwaita SVG cursors; Tuna ships its own private
    // copy so the default survives removing the GNOME session package.
    dirs.push(PathBuf::from("/usr/share/tuna-desktop/icons"));
    let mut seen = HashSet::new();
    dirs.retain(|d| seen.insert(d.clone()));
    dirs
}

/// The themes `theme` inherits from: the `Inherits=` key of the first
/// `index.theme` for it on the search path.
fn inherits(dirs: &[PathBuf], theme: &str) -> Vec<String> {
    for dir in dirs {
        let Some(text) = read_bounded(&dir.join(theme).join("index.theme"), MAX_INDEX_BYTES)
            .and_then(|bytes| String::from_utf8(bytes).ok())
        else {
            continue;
        };
        let mut in_section = false;
        for line in text.lines().map(str::trim) {
            if line.starts_with('[') {
                in_section = line == "[Icon Theme]";
            } else if let Some(value) = line.strip_prefix("Inherits") {
                let Some(value) = value.trim_start().strip_prefix('=') else {
                    continue;
                };
                if in_section {
                    return value
                        .split([',', ';'])
                        .map(str::trim)
                        .filter(|t| theme_name(t))
                        .take(MAX_THEMES - 2)
                        .map(String::from)
                        .collect();
                }
            }
        }
        return Vec::new();
    }
    Vec::new()
}

/// Every theme a lookup in `theme` visits, in order: the theme, its
/// inheritance depth first, then libXcursor's `default` and GNOME's
/// Adwaita. Cycles and repeats are visited once.
pub fn theme_chain(dirs: &[PathBuf], theme: &str) -> Vec<String> {
    fn visit(dirs: &[PathBuf], theme: &str, depth: usize, out: &mut Vec<String>) {
        if depth > MAX_INHERIT_DEPTH
            || out.len() >= MAX_THEMES - 2
            || out.iter().any(|t| t == theme)
        {
            return;
        }
        out.push(theme.to_owned());
        for parent in inherits(dirs, theme) {
            visit(dirs, &parent, depth + 1, out);
        }
    }
    let mut chain = Vec::new();
    let theme = if !theme_name(theme) {
        DEFAULT_THEME
    } else {
        theme
    };
    visit(dirs, theme, 0, &mut chain);
    // Reserve both fallback names even when malicious inheritance fans out.
    for fallback in ["default", DEFAULT_THEME] {
        if !chain.iter().any(|theme| theme == fallback) {
            if chain.len() < MAX_THEMES - 2 {
                visit(dirs, fallback, 0, &mut chain);
            }
            if !chain.iter().any(|theme| theme == fallback) {
                chain.push(fallback.into());
            }
        }
    }
    chain
}

/// Where a cursor was found: the file and the theme that holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub path: PathBuf,
    pub theme: String,
    pub name: String,
}

/// The first of `names` any theme of `chain` has, preferring earlier
/// names over earlier themes (a shape's standard name in an inherited
/// theme beats a legacy alias in the selected one).
pub fn find_cursor(dirs: &[PathBuf], chain: &[String], names: &[&str]) -> Option<Found> {
    for name in names {
        for theme in chain {
            for dir in dirs {
                let path = dir.join(theme).join("cursors").join(name);
                if path.is_file() {
                    return Some(Found {
                        path,
                        theme: theme.clone(),
                        name: (*name).to_owned(),
                    });
                }
            }
        }
    }
    None
}

/// Preserve selected/inherited theme priority across both formats. The
/// private SVG fallback must never hide a user's earlier legacy cursor.
fn resolve_frames(
    dirs: &[PathBuf],
    chain: &[String],
    names: &[&str],
    wanted: u32,
    raster: u32,
) -> Option<(Found, Vec<(CursorImage, u32)>)> {
    for theme in chain {
        for name in names {
            let selected = std::slice::from_ref(theme);
            let names = &[*name];
            if let Some(frames) = crate::cursor_svg::find_frames(dirs, selected, names, raster) {
                return Some(frames);
            }
            // Malformed files in one directory do not prevent another valid
            // copy or an inherited/default theme from supplying the cursor.
            for dir in dirs {
                if let Some(found) = find_cursor(std::slice::from_ref(dir), selected, names) {
                    if let Some(frames) = read_bounded(&found.path, MAX_CURSOR_BYTES)
                        .and_then(|bytes| decode_frames(&bytes, wanted))
                    {
                        return Some((found, frames));
                    }
                }
            }
        }
    }
    None
}

/// Names to look a shape up by: its CSS name, the legacy X names
/// themes still ship, and the names Mutter falls back to.
pub fn icon_names(icon: CursorIcon) -> Vec<&'static str> {
    let mut names = vec![icon.name()];
    names.extend_from_slice(icon.alt_names());
    let mutter: &[&str] = match icon {
        CursorIcon::Grab | CursorIcon::Grabbing => &["hand2"],
        CursorIcon::Move | CursorIcon::AllScroll => &["dnd-move", "fleur"],
        CursorIcon::Progress => &["left_ptr_watch"],
        CursorIcon::NotAllowed => &["crossed_circle"],
        _ => &[],
    };
    for name in mutter {
        if !names.contains(name) {
            names.push(name);
        }
    }
    names
}

/// The theme size images are loaded at for an output `scale`: Mutter
/// loads `cursor-size` times the scale rounded up, then draws the image
/// at the theme size, so fractional scales downscale a sharper image.
pub fn load_size(size: u32, scale: f64) -> u32 {
    let factor = if scale.is_finite() && scale > 0.0 {
        scale.ceil() as u32
    } else {
        1
    };
    clamp_size(size).saturating_mul(factor.max(1)).min(4096)
}

/// `cursor-size` as drawn: 0 or out of range is GNOME's default.
pub fn clamp_size(size: u32) -> u32 {
    if size == 0 || size > MAX_SIZE {
        DEFAULT_SIZE
    } else {
        size
    }
}

/// One decoded cursor image, premultiplied ARGB8888.
#[derive(Debug, Clone, PartialEq)]
pub struct CursorImage {
    /// Nominal raster size: Xcursor image size or SVG physical raster size.
    pub nominal: u32,
    pub width: u32,
    pub height: u32,
    pub hotspot: (f64, f64),
    pub pixels: Vec<u8>,
}

impl CursorImage {
    /// Logical size and hotspot when the theme size is `size`: the
    /// image scaled by `size / nominal` (Mutter's effective theme
    /// scale), so a 48px image loaded for scale 2 draws 24 logical.
    pub fn logical(&self, size: u32) -> ((f64, f64), (f64, f64)) {
        let k = f64::from(clamp_size(size)) / f64::from(self.nominal.max(1));
        (
            (f64::from(self.width) * k, f64::from(self.height) * k),
            (self.hotspot.0 * k, self.hotspot.1 * k),
        )
    }
}

/// All animation frames of the nearest nominal Xcursor size, in file order.
/// Every frame keeps its own dimensions, hotspot, pixels and delay.
pub fn decode_frames(bytes: &[u8], wanted: u32) -> Option<Vec<(CursorImage, u32)>> {
    // The dependency allocates before checking remaining input. Validate all
    // chunk dimensions and aggregate bytes, including repeated TOC positions,
    // before passing untrusted theme data to it.
    if !bounded_xcursor(bytes) {
        return None;
    }
    let images = xcursor::parser::parse_xcursor(bytes)?;
    let nearest = images
        .iter()
        .map(|image| image.size)
        .min_by_key(|size| (size.abs_diff(wanted), *size))?;
    let mut frames = Vec::new();
    let mut total_bytes = 0usize;
    for image in images.into_iter().filter(|image| image.size == nearest) {
        let len = (image.width as usize)
            .checked_mul(image.height as usize)?
            .checked_mul(4)?;
        total_bytes = total_bytes.checked_add(len)?;
        if image.width == 0
            || image.height == 0
            || image.width > 4096
            || image.height > 4096
            || image.pixels_rgba.len() < len
            || total_bytes > 64 * 1024 * 1024
            || frames.len() >= 1024
        {
            return None;
        }
        frames.push((
            CursorImage {
                nominal: image.size.max(1),
                width: image.width,
                height: image.height,
                hotspot: (
                    f64::from(image.xhot.min(image.width - 1)),
                    f64::from(image.yhot.min(image.height - 1)),
                ),
                pixels: image.pixels_rgba[..len].to_vec(),
            },
            image.delay,
        ));
    }
    (!frames.is_empty()).then_some(frames)
}

fn bounded_xcursor(bytes: &[u8]) -> bool {
    fn checked(bytes: &[u8]) -> Option<()> {
        if bytes.len() > MAX_CURSOR_BYTES || bytes.get(..4)? != b"Xcur" {
            return None;
        }
        let word = |offset: usize| -> Option<u32> {
            Some(u32::from_le_bytes(
                bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?,
            ))
        };
        let header = word(4)? as usize;
        let count = word(12)? as usize;
        if header < 16 || count > 1024 {
            return None;
        }
        bytes.get(header..header.checked_add(count.checked_mul(12)?)?)?;
        let mut total = 0usize;
        for toc in 0..count {
            let offset = header + toc * 12;
            if word(offset)? != 0xfffd_0002 {
                continue;
            }
            let image = word(offset + 8)? as usize;
            if word(image)? != 36 || word(image + 4)? != 0xfffd_0002 {
                return None;
            }
            let width = word(image + 16)? as usize;
            let height = word(image + 20)? as usize;
            if width == 0 || height == 0 || width > 4096 || height > 4096 {
                return None;
            }
            let size = width.checked_mul(height)?.checked_mul(4)?;
            total = total.checked_add(size)?;
            if total > MAX_CURSOR_BYTES {
                return None;
            }
            bytes.get(image.checked_add(36)?..image.checked_add(36)?.checked_add(size)?)?;
        }
        Some(())
    }
    checked(bytes).is_some()
}

/// Compatibility helper for tests and static callers.
pub fn decode(bytes: &[u8], wanted: u32) -> Option<CursorImage> {
    decode_frames(bytes, wanted)?
        .into_iter()
        .next()
        .map(|(image, _)| image)
}

/// Select the frame using individual delays, wrap at the total cycle, and
/// avoid a zero-delay theme creating a busy loop. Static cursors stay on 0.
pub fn animation_frame(delays: &[u32], elapsed: Duration) -> usize {
    let cycle: u128 = delays.iter().map(|delay| u128::from((*delay).max(1))).sum();
    if delays.len() < 2 || cycle == 0 {
        return 0;
    }
    let mut position = elapsed.as_millis() % cycle;
    for (index, delay) in delays.iter().enumerate() {
        let delay = u128::from((*delay).max(1));
        if position < delay {
            return index;
        }
        position -= delay;
    }
    0
}

/// The built-in arrow for a theme size: black outline, white fill,
/// hotspot at its tip. Used when no theme on the system has a cursor.
pub fn builtin_arrow(nominal: u32) -> CursorImage {
    let nominal = nominal.max(8);
    let unit = f64::from(nominal) / 24.0;
    // GNOME's arrow silhouette in a 24-unit box, tip at (1, 1).
    let outline: [(f64, f64); 7] = [
        (1.0, 1.0),
        (1.0, 18.5),
        (5.5, 14.5),
        (8.5, 21.5),
        (11.5, 20.0),
        (8.5, 13.5),
        (14.0, 13.5),
    ];
    let poly: Vec<(f64, f64)> = outline.iter().map(|(x, y)| (x * unit, y * unit)).collect();
    let border = unit.max(1.0);
    let (w, h) = (nominal, nominal);
    let mut pixels = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let p = (f64::from(x) + 0.5, f64::from(y) + 0.5);
            if !inside(&poly, p) {
                continue;
            }
            let inner = edge_distance(&poly, p) >= border;
            let v = if inner { 255 } else { 0 };
            let i = ((y * w + x) * 4) as usize;
            pixels[i..i + 4].copy_from_slice(&[v, v, v, 255]);
        }
    }
    let tip = (unit.round() as u32).min(w - 1);
    CursorImage {
        nominal,
        width: w,
        height: h,
        hotspot: (f64::from(tip), f64::from(tip)),
        pixels,
    }
}

fn inside(poly: &[(f64, f64)], (px, py): (f64, f64)) -> bool {
    let mut odd = false;
    let mut j = poly.len() - 1;
    for i in 0..poly.len() {
        let ((xi, yi), (xj, yj)) = (poly[i], poly[j]);
        if (yi > py) != (yj > py) && px < (xj - xi) * (py - yi) / (yj - yi) + xi {
            odd = !odd;
        }
        j = i;
    }
    odd
}

fn edge_distance(poly: &[(f64, f64)], (px, py): (f64, f64)) -> f64 {
    let mut best = f64::MAX;
    for i in 0..poly.len() {
        let (ax, ay) = poly[i];
        let (bx, by) = poly[(i + 1) % poly.len()];
        let (dx, dy) = (bx - ax, by - ay);
        let len = dx * dx + dy * dy;
        let t = if len > 0.0 {
            (((px - ax) * dx + (py - ay) * dy) / len).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let (cx, cy) = (ax + t * dx, ay + t * dy);
        best = best.min((px - cx).hypot(py - cy));
    }
    best
}

/// A decoded frame and its reusable renderer buffer.
struct LoadedFrame {
    image: CursorImage,
    buffer: MemoryRenderBuffer,
}

/// A loaded cursor ready to draw without allocating on every animation tick.
struct Loaded {
    frames: Vec<LoadedFrame>,
    delays: Vec<u32>,
    theme: Option<String>,
    name: String,
}

impl Loaded {
    fn new(image: CursorImage, theme: Option<String>, name: String) -> Self {
        Self::from_frames(vec![(image, 0)], theme, name)
    }

    fn from_frames(frames: Vec<(CursorImage, u32)>, theme: Option<String>, name: String) -> Self {
        let mut delays = Vec::with_capacity(frames.len());
        let frames = frames
            .into_iter()
            .map(|(image, delay)| {
                delays.push(delay);
                let buffer = MemoryRenderBuffer::from_slice(
                    &image.pixels,
                    Fourcc::Argb8888,
                    (image.width as i32, image.height as i32),
                    1,
                    Transform::Normal,
                    None,
                );
                LoadedFrame { image, buffer }
            })
            .collect();
        Self {
            frames,
            delays,
            theme,
            name,
        }
    }
}

/// What the pointer showed last on the output under it, for the state
/// file the proofs read.
#[derive(Debug, Clone, PartialEq)]
struct Shown {
    /// `named` (theme image), `surface` (client buffer) or `hidden`.
    kind: &'static str,
    /// Shape name for `named`.
    name: Option<&'static str>,
    /// Which shape requested it: `client` (cursor-shape protocol),
    /// `grab` (the compositor's move/resize), or `default`.
    source: &'static str,
    /// Theme holding the image, `builtin` for the fallback arrow.
    theme: Option<String>,
    /// The theme file's name (`left_ptr` for a theme without `default`).
    file: Option<String>,
    /// Image buffer size in pixels.
    image: Option<(u32, u32)>,
    /// Drawn size in logical pixels.
    logical: Option<(f64, f64)>,
    scale: f64,
    animation_frame: usize,
    animation_frames: usize,
}

/// Where the cursor image is coming from this frame.
#[derive(Debug, Clone, PartialEq)]
pub enum Pointer {
    Hidden,
    /// A shape: requested by the client under the pointer, the
    /// compositor's own grab, or the default arrow.
    Named(CursorIcon, &'static str),
    Surface(WlSurface),
}

impl Pointer {
    /// What to show: a compositor grab's cursor wins, then the focused
    /// client's request, then the default arrow over no client.
    pub fn resolve(status: &CursorImageStatus, grab: Option<CursorIcon>, focused: bool) -> Self {
        if let Some(icon) = grab {
            return Self::Named(icon, "grab");
        }
        if !focused {
            return Self::Named(CursorIcon::Default, "default");
        }
        match status {
            CursorImageStatus::Hidden => Self::Hidden,
            CursorImageStatus::Named(icon) => Self::Named(*icon, "client"),
            CursorImageStatus::Surface(surface) if surface.is_alive() => {
                Self::Surface(surface.clone())
            }
            CursorImageStatus::Surface(_) => Self::Named(CursorIcon::Default, "default"),
        }
    }
}

/// The cursor elements of one output's frame, drawn above everything.
#[derive(Default)]
pub struct CursorElements {
    /// Locate-pointer ripples and theme images, physical (scale 1).
    pub images: Vec<MemoryRenderBufferRenderElement<GlesRenderer>>,
    /// A client's cursor surface, drawn at the output scale.
    pub surface: Vec<WaylandSurfaceRenderElement<GlesRenderer>>,
}

/// GNOME's cursor theme state and locate-pointer, owned by the runtime.
pub struct Cursor {
    theme: String,
    size: u32,
    dirs: Vec<PathBuf>,
    chain: Vec<String>,
    /// Images by load size and shape; `None` caches a miss.
    cache: HashMap<(u32, CursorIcon), Option<Loaded>>,
    builtin: HashMap<u32, Loaded>,
    shown: Option<Shown>,
    pub locate: LocatePointer,
    animation_icon: Option<CursorIcon>,
    animation_started: Duration,
}

impl Default for Cursor {
    fn default() -> Self {
        Self::new()
    }
}

impl Cursor {
    pub fn new() -> Self {
        let mut cursor = Self {
            theme: String::new(),
            size: DEFAULT_SIZE,
            dirs: Vec::new(),
            chain: Vec::new(),
            cache: HashMap::new(),
            builtin: HashMap::new(),
            shown: None,
            locate: LocatePointer::default(),
            animation_icon: None,
            animation_started: Duration::ZERO,
        };
        cursor.set_theme(DEFAULT_THEME, DEFAULT_SIZE);
        cursor
    }

    /// Follow GNOME's `cursor-theme` and `cursor-size` live. A change
    /// rescans the search path, so a theme installed mid-session is
    /// found.
    pub fn set_theme(&mut self, theme: &str, size: u32) {
        let size = clamp_size(size);
        if theme == self.theme && size == self.size && !self.dirs.is_empty() {
            return;
        }
        self.theme = if theme_name(theme) {
            theme
        } else {
            DEFAULT_THEME
        }
        .to_owned();
        self.size = size;
        self.dirs = search_path(|k| std::env::var(k).ok());
        self.chain = theme_chain(&self.dirs, &self.theme);
        self.cache.clear();
        self.builtin.clear();
        self.animation_icon = None;
        eprintln!(
            "tuna-compositor: cursor theme {:?} size {size} (themes {:?})",
            self.theme, self.chain
        );
    }

    /// Follow GNOME's `locate-pointer`.
    pub fn set_locate_pointer(&mut self, enabled: bool) {
        self.locate.set_enabled(enabled);
    }

    /// The image for `icon` at an output `scale`: from the theme chain,
    /// else the theme's default arrow, else the built-in one.
    fn loaded(&mut self, icon: CursorIcon, scale: f64) -> &Loaded {
        let wanted = load_size(self.size, scale);
        let raster_size = crate::cursor_svg::raster_size(self.size, scale);
        for icon in [icon, CursorIcon::Default] {
            let key = (raster_size, icon);
            if !self.cache.contains_key(&key) {
                let loaded = resolve_frames(
                    &self.dirs,
                    &self.chain,
                    &icon_names(icon),
                    wanted,
                    raster_size,
                )
                .map(|(found, frames)| Loaded::from_frames(frames, Some(found.theme), found.name));
                let bytes = |loaded: &Loaded| {
                    loaded
                        .frames
                        .iter()
                        .map(|frame| frame.image.pixels.len())
                        .sum::<usize>()
                };
                let used = self.cache.values().flatten().map(bytes).sum::<usize>();
                let incoming = loaded.as_ref().map(bytes).unwrap_or(0);
                if self.cache.len() >= MAX_CACHE_ENTRIES
                    || used.saturating_add(incoming) > MAX_CACHE_BYTES
                {
                    self.cache.clear();
                }
                self.cache.insert(key, loaded);
            }
            if self.cache.get(&key).is_some_and(Option::is_some) {
                return self.cache[&key].as_ref().unwrap();
            }
        }
        let builtin_bytes = self
            .builtin
            .values()
            .flat_map(|loaded| &loaded.frames)
            .map(|frame| frame.image.pixels.len())
            .sum::<usize>();
        let incoming = wanted as usize * wanted as usize * 4;
        if !self.builtin.contains_key(&wanted)
            && (self.builtin.len() >= MAX_CACHE_ENTRIES
                || builtin_bytes.saturating_add(incoming) > MAX_CACHE_BYTES)
        {
            self.builtin.clear();
        }
        self.builtin
            .entry(wanted)
            .or_insert_with(|| Loaded::new(builtin_arrow(wanted), None, "default".into()))
    }

    /// The cursor elements for one output whose top-left is `offset`
    /// (global logical) at `scale`, with `pointer` in global logical
    /// coordinates. `now` is the animation clock, `accent` GNOME's
    /// accent color for the ripples. Records what was shown when the
    /// pointer is on this output.
    #[allow(clippy::too_many_arguments)]
    pub fn elements(
        &mut self,
        renderer: &mut GlesRenderer,
        what: &Pointer,
        pointer: Point<f64, Logical>,
        output: Rectangle<i32, Logical>,
        scale: f64,
        now: Duration,
        accent: [f32; 3],
    ) -> CursorElements {
        let mut out = CursorElements::default();
        let on_output = output.to_f64().contains(pointer);
        let to_physical = |p: Point<f64, Logical>| -> Point<f64, Physical> {
            (
                (p.x - f64::from(output.loc.x)) * scale,
                (p.y - f64::from(output.loc.y)) * scale,
            )
                .into()
        };
        let mut shown = Shown {
            kind: "hidden",
            name: None,
            source: "client",
            theme: None,
            file: None,
            image: None,
            logical: None,
            scale,
            animation_frame: 0,
            animation_frames: 0,
        };
        if !matches!(what, Pointer::Named(..)) {
            self.animation_icon = None;
        }
        match what {
            Pointer::Hidden => {}
            Pointer::Named(icon, source) => {
                let size = self.size;
                if self.animation_icon != Some(*icon) {
                    self.animation_icon = Some(*icon);
                    self.animation_started = now;
                }
                let elapsed = now.saturating_sub(self.animation_started);
                let loaded = self.loaded(*icon, scale);
                let index = animation_frame(&loaded.delays, elapsed);
                let frame = &loaded.frames[index];
                let ((w, h), (hx, hy)) = frame.image.logical(size);
                let at = to_physical(pointer - Point::from((hx, hy)));
                let physical = (
                    (w * scale).round().max(1.0) as i32,
                    (h * scale).round().max(1.0) as i32,
                );
                let src = Rectangle::from_size(
                    (f64::from(frame.image.width), f64::from(frame.image.height)).into(),
                );
                if let Ok(element) = MemoryRenderBufferRenderElement::from_buffer(
                    renderer,
                    at,
                    &frame.buffer,
                    None,
                    Some(src),
                    Some(physical.into()),
                    Kind::Cursor,
                ) {
                    out.images.push(element);
                }
                shown = Shown {
                    kind: "named",
                    name: Some(icon.name()),
                    source,
                    theme: Some(loaded.theme.clone().unwrap_or_else(|| "builtin".into())),
                    file: Some(loaded.name.clone()),
                    image: Some((frame.image.width, frame.image.height)),
                    logical: Some((w, h)),
                    scale,
                    animation_frame: index,
                    animation_frames: loaded.frames.len(),
                };
            }
            Pointer::Surface(surface) => {
                let hotspot = smithay::wayland::compositor::with_states(surface, |states| {
                    states
                        .data_map
                        .get::<CursorImageSurfaceData>()
                        .and_then(|d| d.lock().ok().map(|a| a.hotspot))
                })
                .unwrap_or_default();
                let at = to_physical(pointer - hotspot.to_f64());
                out.surface = render_elements_from_surface_tree(
                    renderer,
                    surface,
                    at.to_i32_round(),
                    scale,
                    1.0,
                    Kind::Cursor,
                );
                let size = surface_size(surface);
                shown = Shown {
                    kind: "surface",
                    name: None,
                    source: "client",
                    theme: None,
                    file: None,
                    image: size.map(|(_, buffer)| buffer),
                    logical: size.map(|(logical, _)| logical),
                    scale,
                    animation_frame: 0,
                    animation_frames: 0,
                };
            }
        }
        // GNOME draws the ripples above the windows, under the pointer.
        let ripples = self.locate.elements(renderer, now, accent, scale, |p| {
            output.to_f64().contains(p).then(|| to_physical(p))
        });
        if !ripples.is_empty() {
            self.locate.frames += 1;
        }
        out.images.splice(0..0, ripples);
        if on_output {
            self.shown = Some(shown);
        }
        out
    }

    /// Hand a client cursor surface its frame callbacks after a frame
    /// showed it.
    pub fn frame_done(what: &Pointer, time: Duration) {
        let Pointer::Surface(surface) = what else {
            return;
        };
        use smithay::wayland::compositor::{
            with_surface_tree_downward, SurfaceAttributes, TraversalAction,
        };
        let ms = time.as_millis() as u32;
        with_surface_tree_downward(
            surface,
            (),
            |_, _, _| TraversalAction::DoChildren(()),
            |_, states, _| {
                let callbacks = std::mem::take(
                    &mut states
                        .cached_state
                        .get::<SurfaceAttributes>()
                        .current()
                        .frame_callbacks,
                );
                for callback in callbacks {
                    callback.done(ms);
                }
            },
            |_, _, _| true,
        );
    }

    /// Whether anything time-driven is on screen (ripples), so the
    /// frame must be repainted even without input.
    pub fn animating(&self, now: Duration) -> bool {
        self.locate.active(now)
            || self
                .shown
                .as_ref()
                .is_some_and(|shown| shown.animation_frames > 1)
    }

    /// A lock is a privacy boundary, including a D-Bus lock without input.
    /// Keep preference/theme caches, but discard activity and rendered history.
    pub fn on_lock(&mut self) {
        self.locate.on_lock();
        self.shown = None;
        self.animation_icon = None;
    }

    pub fn diagnostics(&self, now: Duration, locked: bool) -> serde_json::Value {
        if locked {
            serde_json::Value::Null
        } else {
            self.snapshot(now)
        }
    }

    /// The state-file record: settings, what the pointer shows, and
    /// locate-pointer.
    pub fn snapshot(&self, now: Duration) -> serde_json::Value {
        let shown = self.shown.as_ref();
        serde_json::json!({
            "theme": self.theme,
            "size": self.size,
            "themes": self.chain,
            "kind": shown.map(|s| s.kind),
            "name": shown.and_then(|s| s.name),
            "source": shown.map(|s| s.source),
            "image_theme": shown.and_then(|s| s.theme.clone()),
            "file": shown.and_then(|s| s.file.clone()),
            "image": shown.and_then(|s| s.image).map(|(w, h)| [w, h]),
            "logical": shown.and_then(|s| s.logical).map(|(w, h)| [w, h]),
            "scale": shown.map(|s| s.scale),
            "animation_frame": shown.map(|s| s.animation_frame),
            "animation_frames": shown.map(|s| s.animation_frames),
            "locate_pointer": self.locate.snapshot(now),
        })
    }
}

/// Draw one output's cursor elements over the finished scene: the
/// ripples and theme images at physical scale, a client surface at the
/// output `scale`.
pub fn draw(
    frame: &mut smithay::backend::renderer::gles::GlesFrame<'_, '_>,
    elements: &CursorElements,
    scale: f64,
    damage: &[Rectangle<i32, Physical>],
) -> Result<(), smithay::backend::renderer::gles::GlesError> {
    use smithay::backend::renderer::utils::draw_render_elements;
    if !elements.images.is_empty() {
        draw_render_elements::<GlesRenderer, _, _>(frame, 1.0, &elements.images, damage)?;
    }
    if !elements.surface.is_empty() {
        draw_render_elements::<GlesRenderer, _, _>(frame, scale, &elements.surface, damage)?;
    }
    Ok(())
}

/// The cursor's passes for the native backend's repaint signature and
/// damage tracker, bottom to top, so a shape, size or ripple change
/// repaints and damages its area. Trees wholly off this output are left out.
pub(crate) fn tracked(
    elements: &CursorElements,
    scale: f64,
    bounds: Rectangle<i32, Physical>,
) -> [Vec<crate::native_repaint::Tracked<'_>>; 2] {
    use crate::native_repaint::Tracked;
    use smithay::backend::renderer::element::Element;
    [
        elements
            .images
            .iter()
            .rev()
            .filter(|e| visible_geometry(e.geometry(1.0.into()), bounds))
            .map(|e| Tracked::new(e, 1.0))
            .collect(),
        elements
            .surface
            .iter()
            .rev()
            .filter(|e| visible_geometry(e.geometry(scale.into()), bounds))
            .map(|e| Tracked::new(e, scale))
            .collect(),
    ]
}

/// Exclude cursor trees wholly outside this output from timing and damage.
/// A cursor straddling an edge belongs to every output it visibly intersects.
pub(crate) fn presented_surface(
    elements: &CursorElements,
    what: &Pointer,
    scale: f64,
    bounds: Rectangle<i32, Physical>,
) -> Option<WlSurface> {
    use smithay::backend::renderer::element::Element;
    let Pointer::Surface(surface) = what else {
        return None;
    };
    elements
        .surface
        .iter()
        .any(|e| visible_geometry(e.geometry(scale.into()), bounds))
        .then(|| surface.clone())
}

fn visible_geometry(geometry: Rectangle<i32, Physical>, bounds: Rectangle<i32, Physical>) -> bool {
    geometry
        .intersection(bounds)
        .is_some_and(|intersection| !intersection.is_empty())
}

/// A cursor surface's logical size and its buffer's pixel size.
#[allow(clippy::type_complexity)]
fn surface_size(surface: &WlSurface) -> Option<((f64, f64), (u32, u32))> {
    smithay::wayland::compositor::with_states(surface, |states| {
        let data = states
            .data_map
            .get::<smithay::backend::renderer::utils::RendererSurfaceStateUserData>()?;
        let data = data.lock().ok()?;
        let logical = data.surface_size()?;
        let buffer = data.buffer_size()?;
        let k = data.buffer_scale().max(1);
        Some((
            (f64::from(logical.w), f64::from(logical.h)),
            ((buffer.w * k).max(0) as u32, (buffer.h * k).max(0) as u32),
        ))
    })
}

/// The cursor for the compositor's own grabs, as Mutter's window drag
/// shows: the move cursor, or the resize cursor of the dragged edges
/// (xdg ResizeEdge bits: top 1, bottom 2, left 4, right 8).
pub fn grab_icon(resize_edges: Option<u32>) -> CursorIcon {
    let Some(edges) = resize_edges else {
        return CursorIcon::Move;
    };
    let (top, bottom, left, right) = (
        edges & 1 != 0,
        edges & 2 != 0,
        edges & 4 != 0,
        edges & 8 != 0,
    );
    match (top, bottom, left, right) {
        (true, _, true, _) => CursorIcon::NwResize,
        (true, _, _, true) => CursorIcon::NeResize,
        (_, true, true, _) => CursorIcon::SwResize,
        (_, true, _, true) => CursorIcon::SeResize,
        (true, ..) => CursorIcon::NResize,
        (_, true, ..) => CursorIcon::SResize,
        (_, _, true, _) => CursorIcon::WResize,
        (_, _, _, true) => CursorIcon::EResize,
        _ => CursorIcon::Default,
    }
}

/// evdev `KEY_LEFTCTRL`: Mutter's default `locate-pointer-key` is
/// `Control_L`.
#[cfg(test)]
pub const LOCATE_POINTER_KEYCODE: u32 = 29;

/// `ripples.js`: each ring's `(delay, duration, start scale, start
/// opacity, final scale)` in ms, as `locatePointer.js` plays them.
pub const RIPPLES: [(f64, f64, f64, f64, f64); 3] = [
    (0.0, 830.0, 0.25, 1.0, 1.5),
    (50.0, 1000.0, 0.0, 0.7, 1.25),
    (350.0, 1000.0, 0.0, 0.3, 1.0),
];
/// `.ripple-pointer-location`: a 50px disc with a 2px glow.
pub const RIPPLE_SIZE: f64 = 50.0;
const RIPPLE_GLOW: f64 = 2.0;

/// One ring's `(scale, opacity)` `elapsed_ms` after the tap, `None`
/// once finished. A ring shows its start values during its delay;
/// opacity eases in-quad from `sqrt(start)` to 0, scale out-quad.
/// Ripples play even with animations off (`animationRequired`).
pub fn ripple_ring(ring: usize, elapsed_ms: f64) -> Option<(f64, f64)> {
    let (delay, duration, start_scale, start_opacity, final_scale) = *RIPPLES.get(ring)?;
    if elapsed_ms >= delay + duration {
        return None;
    }
    let t = ((elapsed_ms - delay) / duration).clamp(0.0, 1.0);
    let out_quad = 1.0 - (1.0 - t) * (1.0 - t);
    let in_quad = t * t;
    let scale = start_scale + (final_scale - start_scale) * out_quad;
    let opacity = start_opacity.sqrt() * (1.0 - in_quad);
    Some((scale, opacity))
}

fn ripples_end_ms() -> f64 {
    RIPPLES
        .iter()
        .map(|(delay, duration, ..)| delay + duration)
        .fold(0.0, f64::max)
}

// Limit the three renderer buffers per locate color/scale variant.
const MAX_DISC_SIDE: u32 = 1024;
const MAX_DISC_ENTRIES: usize = 8;
const MAX_DISC_BYTES: usize = 16 * 1024 * 1024;

fn disc_side(scale: f64) -> Option<u32> {
    (scale.is_finite() && scale > 0.0).then(|| {
        ((RIPPLE_SIZE + 2.0 * RIPPLE_GLOW) * 1.5 * scale)
            .ceil()
            .clamp(1.0, f64::from(MAX_DISC_SIDE)) as u32
    })
}

fn disc_bytes(side: u32) -> usize {
    side as usize * side as usize * 4 * 3
}

fn disc_cache_must_clear(sides: impl Iterator<Item = u32>, incoming: u32) -> bool {
    let (count, bytes) = sides.fold((0usize, 0usize), |(count, bytes), side| {
        (count + 1, bytes.saturating_add(disc_bytes(side)))
    });
    count >= MAX_DISC_ENTRIES || bytes.saturating_add(disc_bytes(incoming)) > MAX_DISC_BYTES
}

/// GNOME's `locate-pointer`: a Ctrl press and release with nothing
/// else in between plays the ripples at the pointer. The key still
/// reaches the client, as in Mutter.
#[derive(Default)]
pub struct LocatePointer {
    enabled: bool,
    held: HashSet<u32>,
    armed: Option<u32>,
    key_name: String,
    keysym: Option<u32>,
    /// Where and when the ripples started.
    playing: Option<(Point<f64, Logical>, Duration)>,
    /// Ripples started this session, for the proofs.
    count: u64,
    /// Frames that drew ripples, for the proofs.
    frames: u64,
    discs: HashMap<(u32, [u8; 3]), [MemoryRenderBuffer; 3]>,
}

impl LocatePointer {
    pub fn set_key(&mut self, name: &str) {
        if self.key_name == name {
            return;
        }
        self.key_name = name.into();
        // xkbcommon's helper panics on embedded NUL; an invalid setting
        // disables the binding rather than inventing another key.
        self.keysym = if name.is_empty() || name.contains('\0') {
            None
        } else {
            let symbol = xkb::keysym_from_name(name, 0).raw();
            (symbol != 0).then_some(symbol)
        };
        self.reset_keys();
    }

    pub fn matches_keysym(&self, symbols: impl IntoIterator<Item = u32>) -> bool {
        self.keysym
            .is_some_and(|wanted| symbols.into_iter().any(|symbol| symbol == wanted))
    }

    #[cfg(test)]
    fn feed(&mut self, input: &ManagerInput, pointer: Point<f64, Logical>, now: Duration) -> bool {
        let matches_key = matches!(
            input,
            ManagerInput::Key {
                keycode: LOCATE_POINTER_KEYCODE,
                ..
            }
        );
        self.feed_matched(input, matches_key, pointer, now)
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled {
            self.armed = None;
            self.playing = None;
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Feed one input event; returns whether the ripples started.
    pub fn feed_matched(
        &mut self,
        input: &ManagerInput,
        matches_key: bool,
        pointer: Point<f64, Logical>,
        now: Duration,
    ) -> bool {
        match *input {
            ManagerInput::Key {
                keycode, pressed, ..
            } => {
                if pressed {
                    let alone = self.held.is_empty();
                    if !self.held.insert(keycode) {
                        return false; // Repeated press keeps an existing lone tap armed.
                    }
                    self.armed = (matches_key && alone).then_some(keycode);
                    false
                } else {
                    self.held.remove(&keycode);
                    let fire = self.armed == Some(keycode);
                    self.armed = None;
                    if fire && self.enabled {
                        self.playing = Some((pointer, now));
                        self.count += 1;
                        return true;
                    }
                    false
                }
            }
            ManagerInput::Button { .. }
            | ManagerInput::Axis { .. }
            | ManagerInput::SwipeBegin { .. } => {
                self.armed = None;
                false
            }
            _ => false,
        }
    }

    /// Forget held keys a lock or grab swallowed.
    pub fn reset_keys(&mut self) {
        self.held.clear();
        self.armed = None;
    }

    pub fn on_lock(&mut self) {
        self.reset_keys();
        self.playing = None;
        self.count = 0;
        self.frames = 0;
    }

    fn elapsed_ms(&self, now: Duration) -> Option<f64> {
        let (_, at) = self.playing?;
        let ms = now.saturating_sub(at).as_secs_f64() * 1000.0;
        (ms < ripples_end_ms()).then_some(ms)
    }

    pub fn active(&self, now: Duration) -> bool {
        self.elapsed_ms(now).is_some()
    }

    fn cached_discs(
        &mut self,
        side: u32,
        color: [u8; 3],
        accent: [f32; 3],
    ) -> &[MemoryRenderBuffer; 3] {
        let side = side.clamp(1, MAX_DISC_SIDE);
        let key = (side, color);
        if !self.discs.contains_key(&key)
            && disc_cache_must_clear(self.discs.keys().map(|(side, _)| *side), side)
        {
            self.discs.clear();
        }
        self.discs.entry(key).or_insert_with(|| {
            let pixels = ripple_disc(
                side,
                accent,
                RIPPLE_GLOW / (RIPPLE_SIZE + 2.0 * RIPPLE_GLOW),
            );
            std::array::from_fn(|_| {
                MemoryRenderBuffer::from_slice(
                    &pixels,
                    Fourcc::Argb8888,
                    (side as i32, side as i32),
                    1,
                    Transform::Normal,
                    None,
                )
            })
        })
    }

    fn elements(
        &mut self,
        renderer: &mut GlesRenderer,
        now: Duration,
        accent: [f32; 3],
        scale: f64,
        place: impl Fn(Point<f64, Logical>) -> Option<Point<f64, Physical>>,
    ) -> Vec<MemoryRenderBufferRenderElement<GlesRenderer>> {
        let Some(elapsed) = self.elapsed_ms(now) else {
            self.playing = None;
            return Vec::new();
        };
        let Some((center, _)) = self.playing else {
            return Vec::new();
        };
        let Some(center) = place(center) else {
            return Vec::new();
        };
        // The disc is drawn once at its largest on-screen size.
        let Some(largest) = disc_side(scale) else {
            return Vec::new();
        };
        let key_color = accent.map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8);
        let discs = self.cached_discs(largest, key_color, accent);
        let mut out = Vec::new();
        // Ring 3 is on top in GNOME (`set_child_above_sibling`); the
        // caller draws front to back.
        for ring in (0..RIPPLES.len()).rev() {
            let Some((ring_scale, opacity)) = ripple_ring(ring, elapsed) else {
                continue;
            };
            let side = ((RIPPLE_SIZE + 2.0 * RIPPLE_GLOW) * ring_scale * scale)
                .min(f64::from(MAX_DISC_SIDE));
            if side < 1.0 || opacity <= 0.0 {
                continue;
            }
            let at: Point<f64, Physical> = (center.x - side / 2.0, center.y - side / 2.0).into();
            let src = Rectangle::from_size((f64::from(largest), f64::from(largest)).into());
            let size = (side.round() as i32, side.round() as i32);
            if let Ok(element) = MemoryRenderBufferRenderElement::from_buffer(
                renderer,
                at,
                &discs[ring],
                Some(opacity as f32),
                Some(src),
                Some(size.into()),
                Kind::Unspecified,
            ) {
                out.push(element);
            }
        }
        out
    }

    fn snapshot(&self, now: Duration) -> serde_json::Value {
        serde_json::json!({
            "enabled": self.enabled,
            "key": self.key_name,
            "key_valid": self.keysym.is_some(),
            "count": self.count,
            "frames": self.frames,
            "active": self.active(now),
            "at": self.playing.filter(|_| self.active(now)).map(|(p, _)| [p.x, p.y]),
        })
    }
}

/// HSL lightening as Sass's `lighten`: `amount` added to lightness.
pub fn lighten(rgb: [f32; 3], amount: f32) -> [f32; 3] {
    let [r, g, b] = rgb.map(|c| c.clamp(0.0, 1.0));
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let d = max - min;
    let (h, s) = if d == 0.0 {
        (0.0, 0.0)
    } else {
        let s = if l > 0.5 {
            d / (2.0 - max - min)
        } else {
            d / (max + min)
        };
        let h = if max == r {
            (g - b) / d + if g < b { 6.0 } else { 0.0 }
        } else if max == g {
            (b - r) / d + 2.0
        } else {
            (r - g) / d + 4.0
        };
        (h / 6.0, s)
    };
    let l = (l + amount).clamp(0.0, 1.0);
    if s == 0.0 {
        return [l, l, l];
    }
    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    let hue = |mut t: f32| {
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    [hue(h + 1.0 / 3.0), hue(h), hue(h - 1.0 / 3.0)]
}

/// `.ripple-pointer-location` as premultiplied ARGB8888, `size` square:
/// the accent lightened 30% at 30% opacity, edged by a glow of the
/// accent lightened 20% (`glow` is its share of the radius).
pub fn ripple_disc(size: u32, accent: [f32; 3], glow: f64) -> Vec<u8> {
    let fill = lighten(accent, 0.3);
    let edge = lighten(accent, 0.2);
    let r = f64::from(size) / 2.0;
    let disc = r * (1.0 - glow);
    let mut out = vec![0u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            let d = (f64::from(x) + 0.5 - r).hypot(f64::from(y) + 0.5 - r);
            let (color, alpha) = if d <= disc - 1.0 {
                (fill, 0.3)
            } else if d <= disc {
                // The disc's antialiased rim, on the glow.
                (edge, 0.3 + 0.5 * (disc - d))
            } else if d < r {
                (edge, 0.8 * (1.0 - (d - disc) / (r - disc)))
            } else {
                continue;
            };
            let a = alpha.clamp(0.0, 1.0) as f32;
            let i = ((y * size + x) * 4) as usize;
            let byte = |c: f32| (c * a * 255.0).round() as u8;
            out[i..i + 4].copy_from_slice(&[
                byte(color[2]),
                byte(color[1]),
                byte(color[0]),
                (a * 255.0).round() as u8,
            ]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_edge_intersections_include_neighbors_but_not_off_output_motion() {
        let bounds = Rectangle::from_size((1280, 800).into());
        assert!(visible_geometry(
            Rectangle::new((1270, 40).into(), (24, 24).into()),
            bounds
        ));
        assert!(visible_geometry(
            Rectangle::new((-10, 40).into(), (24, 24).into()),
            bounds
        ));
        assert!(!visible_geometry(
            Rectangle::new((1280, 40).into(), (24, 24).into()),
            bounds
        ));
        assert!(!visible_geometry(
            Rectangle::new((-24, 40).into(), (24, 24).into()),
            bounds
        ));
        // Pre-transform physical bounds apply to rotated/fractional outputs.
        let rotated = Rectangle::from_size((800, 1280).into());
        assert!(visible_geometry(
            Rectangle::new((795, 1260).into(), (30, 30).into()),
            rotated
        ));
        assert!(!visible_geometry(
            Rectangle::new((805, 1260).into(), (30, 30).into()),
            rotated
        ));
    }

    #[test]
    fn animated_cursors_keep_each_frame_of_the_nearest_size() {
        let mut bytes = xcursor_file(&[(24, 24), (48, 48), (24, 32)]);
        let first = 16 + 12 * 3;
        bytes[first + 32..first + 36].copy_from_slice(&30u32.to_le_bytes());
        let third = first + 36 + 24 * 24 * 4 + 36 + 48 * 48 * 4;
        bytes[third + 32..third + 36].copy_from_slice(&70u32.to_le_bytes());
        let frames = decode_frames(&bytes, 25).unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(
            (frames[0].0.width, frames[0].0.hotspot, frames[0].1),
            (24, (6.0, 3.0), 30)
        );
        assert_eq!(
            (frames[1].0.width, frames[1].0.hotspot, frames[1].1),
            (32, (8.0, 4.0), 70)
        );
        assert_eq!(decode_frames(&bytes, 48).unwrap().len(), 1);
    }

    #[test]
    fn cursor_animation_obeys_each_delay_and_wraps_at_exact_boundaries() {
        for (millis, expected) in [
            (0, 0),
            (29, 0),
            (30, 1),
            (99, 1),
            (100, 2),
            (119, 2),
            (120, 0),
            (150, 1),
        ] {
            assert_eq!(
                animation_frame(&[30, 70, 20], Duration::from_millis(millis)),
                expected
            );
        }
        assert_eq!(animation_frame(&[], Duration::MAX), 0);
        assert_eq!(animation_frame(&[0], Duration::MAX), 0);
    }

    #[test]
    fn zero_cursor_delays_are_bounded_and_large_times_do_not_overflow() {
        assert_eq!(animation_frame(&[0, 0], Duration::from_millis(0)), 0);
        assert_eq!(animation_frame(&[0, 0], Duration::from_millis(1)), 1);
        assert_eq!(animation_frame(&[0, 0], Duration::from_millis(2)), 0);
        assert!(animation_frame(&[u32::MAX, u32::MAX], Duration::MAX) < 2);
    }

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| map.get(k).cloned()
    }

    /// Xcursor file bytes with one image per `(nominal, side)`.
    fn xcursor_file(images: &[(u32, u32)]) -> Vec<u8> {
        let mut out = Vec::new();
        let word = |out: &mut Vec<u8>, v: u32| out.extend_from_slice(&v.to_le_bytes());
        out.extend_from_slice(b"Xcur");
        word(&mut out, 16);
        word(&mut out, 0x1_0000);
        word(&mut out, images.len() as u32);
        let mut position = 16 + 12 * images.len() as u32;
        for (nominal, side) in images {
            word(&mut out, 0xfffd_0002);
            word(&mut out, *nominal);
            word(&mut out, position);
            position += 36 + side * side * 4;
        }
        for (nominal, side) in images {
            for v in [
                36,
                0xfffd_0002,
                *nominal,
                1,
                *side,
                *side,
                side / 4,
                side / 8,
                0,
            ] {
                word(&mut out, v);
            }
            for _ in 0..side * side {
                word(&mut out, 0xff10_2030);
            }
        }
        out
    }

    fn theme(root: &Path, name: &str, inherits: Option<&str>, cursors: &[&str]) {
        let dir = root.join(name);
        std::fs::create_dir_all(dir.join("cursors")).unwrap();
        if let Some(parent) = inherits {
            std::fs::write(
                dir.join("index.theme"),
                format!("[Icon Theme]\nName={name}\nInherits={parent}\n"),
            )
            .unwrap();
        }
        for cursor in cursors {
            std::fs::write(
                dir.join("cursors").join(cursor),
                xcursor_file(&[(24, 24), (48, 48)]),
            )
            .unwrap();
        }
    }

    #[test]
    fn selected_legacy_theme_beats_bundled_svg_and_corrupt_files_fall_back() {
        let dir = tempfile::tempdir().unwrap();
        theme(dir.path(), "Legacy", None, &["left_ptr"]);
        let svg = dir.path().join("Adwaita/cursors_scalable/default");
        std::fs::create_dir_all(&svg).unwrap();
        std::fs::write(svg.join("default.svg"), r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24"><rect width="24" height="24" fill="red"/></svg>"#).unwrap();
        std::fs::write(
            svg.join("metadata.json"),
            r#"[{"filename":"default.svg","nominal_size":24}]"#,
        )
        .unwrap();
        let dirs = &[dir.path().into()];
        let chain = &["Legacy".into(), "Adwaita".into()];
        assert_eq!(
            resolve_frames(dirs, chain, &["default", "left_ptr"], 24, 24)
                .unwrap()
                .0
                .theme,
            "Legacy"
        );
        std::fs::write(dir.path().join("Legacy/cursors/left_ptr"), b"corrupt").unwrap();
        assert_eq!(
            resolve_frames(dirs, chain, &["default", "left_ptr"], 24, 24)
                .unwrap()
                .0
                .theme,
            "Adwaita"
        );
        // SVG is preferred when both valid formats exist in that same theme.
        theme(dir.path(), "Adwaita", None, &["default"]);
        let (found, frames) =
            resolve_frames(dirs, chain, &["default", "left_ptr"], 24, 24).unwrap();
        assert!(found.path.ends_with("metadata.json"));
        assert_eq!(&frames[0].0.pixels[..4], &[0, 0, 255, 255]);
    }

    #[test]
    fn theme_work_is_bounded_and_fallback_names_survive_fanout() {
        let tmp = tempfile::tempdir().unwrap();
        let parents = (0..200)
            .map(|i| format!("theme{i}"))
            .collect::<Vec<_>>()
            .join(",");
        theme(tmp.path(), "Huge", Some(&parents), &[]);
        let chain = theme_chain(&[tmp.path().into()], "Huge");
        assert!(chain.len() <= MAX_THEMES);
        assert!(chain.iter().any(|name| name == "default"));
        assert!(chain.iter().any(|name| name == DEFAULT_THEME));
        for invalid in ["..", ".", "a\0b", "../escape"] {
            assert_eq!(theme_chain(&[], invalid)[0], DEFAULT_THEME);
        }
        let path = (0..200)
            .map(|i| format!("/dir{i}"))
            .collect::<Vec<_>>()
            .join(":");
        assert!(search_path(env(&[("XCURSOR_PATH", &path)])).len() <= MAX_DIRS);
        let index = tmp.path().join("Huge/index.theme");
        std::fs::write(&index, vec![b'x'; MAX_INDEX_BYTES + 1]).unwrap();
        assert!(inherits(&[tmp.path().into()], "Huge").is_empty());
        assert_eq!(load_size(256, f64::MAX), 4096);
    }

    #[test]
    fn malformed_xcursor_cannot_allocate_before_validation() {
        let mut bytes = xcursor_file(&[(24, 24)]);
        assert!(bounded_xcursor(&bytes));
        bytes[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(!bounded_xcursor(&bytes));
        let mut bytes = xcursor_file(&[(24, 24)]);
        bytes[44..48].copy_from_slice(&32767u32.to_le_bytes());
        assert!(!bounded_xcursor(&bytes));
        assert!(decode_frames(&bytes, 24).is_none());
        assert!(!bounded_xcursor(&xcursor_file(&[(24, 24)])[..40]));
        // Repeated TOC entries must count every parser allocation, even if
        // they all reference one small on-disk chunk.
        let source = xcursor_file(&[(24, 256)]);
        let count = 80u32;
        let mut repeated = source[..16].to_vec();
        repeated[12..16].copy_from_slice(&count.to_le_bytes());
        for _ in 0..count {
            for word in [0xfffd_0002u32, 24, 16 + count * 12] {
                repeated.extend(word.to_le_bytes());
            }
        }
        repeated.extend_from_slice(&source[28..]);
        assert!(repeated.len() < MAX_CURSOR_BYTES);
        assert!(!bounded_xcursor(&repeated));
    }

    #[test]
    fn search_path_follows_xcursor_path_then_the_xdg_icon_dirs() {
        let dirs = search_path(env(&[
            ("HOME", "/home/u"),
            ("XCURSOR_PATH", "~/.icons:/opt/icons::/opt/icons"),
        ]));
        assert_eq!(
            dirs,
            [
                PathBuf::from("/home/u/.icons"),
                PathBuf::from("/opt/icons"),
                PathBuf::from("/usr/share/tuna-desktop/icons")
            ]
        );
        let dirs = search_path(env(&[
            ("HOME", "/home/u"),
            ("XDG_DATA_DIRS", "/run/host/share:/usr/share"),
        ]));
        assert_eq!(
            dirs,
            [
                "/home/u/.local/share/icons",
                "/home/u/.icons",
                "/run/host/share/icons",
                "/usr/share/icons",
                "/usr/share/pixmaps",
                "/usr/share/tuna-desktop/icons"
            ]
            .map(PathBuf::from)
        );
        let dirs = search_path(env(&[("HOME", "/h"), ("XDG_DATA_HOME", "/data")]));
        assert_eq!(dirs[0], PathBuf::from("/data/icons"));
        assert!(dirs.contains(&PathBuf::from("/usr/local/share/icons")));
    }

    #[test]
    fn themes_resolve_through_their_inheritance_chain_then_default() {
        let tmp = tempfile::tempdir().unwrap();
        let (user, system) = (tmp.path().join("user"), tmp.path().join("system"));
        theme(&user, "Fancy", Some("Middle"), &["pointer"]);
        theme(&system, "Middle", Some("Base, Fancy"), &[]);
        theme(&system, "Base", None, &["default", "text", "xterm"]);
        theme(&system, "default", Some("Base"), &["crosshair"]);
        let dirs = vec![user, system];
        let chain = theme_chain(&dirs, "Fancy");
        // Cycle back to Fancy is visited once; default and Adwaita close it.
        assert_eq!(chain, ["Fancy", "Middle", "Base", "default", "Adwaita"]);
        let found = |names: &[&str]| find_cursor(&dirs, &chain, names);
        assert_eq!(found(&["pointer"]).unwrap().theme, "Fancy");
        assert_eq!(found(&["default", "left_ptr"]).unwrap().theme, "Base");
        // The standard name in an ancestor beats a legacy alias.
        let text = found(&icon_names(CursorIcon::Text)).unwrap();
        assert_eq!((text.theme.as_str(), text.name.as_str()), ("Base", "text"));
        assert_eq!(found(&["crosshair"]).unwrap().theme, "default");
        assert!(found(&["wait"]).is_none());
        // An empty or path-like theme name means GNOME's default.
        assert_eq!(theme_chain(&dirs, "")[0], DEFAULT_THEME);
        assert_eq!(theme_chain(&dirs, "../x")[0], DEFAULT_THEME);
    }

    #[test]
    fn missing_themes_fall_back_to_default_then_the_builtin_arrow() {
        let tmp = tempfile::tempdir().unwrap();
        theme(tmp.path(), "default", Some("Base"), &[]);
        theme(tmp.path(), "Base", None, &["left_ptr"]);
        let dirs = vec![tmp.path().to_path_buf()];
        let chain = theme_chain(&dirs, "NotInstalled");
        assert_eq!(chain, ["NotInstalled", "default", "Base", "Adwaita"]);
        let found = find_cursor(&dirs, &chain, &icon_names(CursorIcon::Default)).unwrap();
        assert_eq!(
            (found.theme.as_str(), found.name.as_str()),
            ("Base", "left_ptr")
        );
        let arrow = builtin_arrow(48);
        assert_eq!((arrow.width, arrow.height, arrow.nominal), (48, 48, 48));
        assert_eq!(arrow.hotspot, (2.0, 2.0));
        let px = |x: u32, y: u32| arrow.pixels[((y * 48 + x) * 4) as usize..][..4].to_vec();
        assert_eq!(px(2, 2), [0, 0, 0, 255], "the tip is outline");
        assert_eq!(px(8, 20), [255, 255, 255, 255], "the body is filled");
        assert_eq!(px(40, 4), [0, 0, 0, 0], "outside is clear");
    }

    #[test]
    fn decode_picks_the_nearest_nominal_size() {
        let bytes = xcursor_file(&[(24, 24), (32, 32), (48, 48)]);
        assert_eq!(decode(&bytes, 24).unwrap().width, 24);
        assert_eq!(decode(&bytes, 30).unwrap().width, 32);
        assert_eq!(decode(&bytes, 96).unwrap().width, 48);
        let image = decode(&bytes, 48).unwrap();
        assert_eq!((image.nominal, image.hotspot), (48, (12.0, 6.0)));
        assert_eq!(&image.pixels[..4], &[0x30, 0x20, 0x10, 0xff]);
        assert!(decode(b"not a cursor", 24).is_none());
    }

    #[test]
    fn cursors_load_at_the_ceiled_scale_and_draw_at_the_theme_size() {
        assert_eq!(load_size(24, 1.0), 24);
        assert_eq!(load_size(24, 1.25), 48);
        assert_eq!(load_size(24, 2.0), 48);
        assert_eq!(load_size(48, 1.5), 96);
        assert_eq!(load_size(0, 1.0), DEFAULT_SIZE);
        assert_eq!(load_size(24, f64::NAN), 24);
        let image = |nominal, side| CursorImage {
            nominal,
            width: side,
            height: side,
            hotspot: (f64::from(side / 4), f64::from(side / 8)),
            pixels: Vec::new(),
        };
        // 200%: the 48px image draws 24 logical, 48 physical: 1:1 crisp.
        assert_eq!(image(48, 48).logical(24), ((24.0, 24.0), (6.0, 3.0)));
        // 125%: the 48px image draws 24 logical (30 physical), downscaled.
        assert_eq!(image(48, 48).logical(24).0, (24.0, 24.0));
        // A theme lacking the size keeps the requested one: 32px images
        // for size 48 scale up to 48 logical, hotspot with them.
        assert_eq!(image(32, 32).logical(48), ((48.0, 48.0), (12.0, 6.0)));
        // Size 48 at scale 1 draws twice size 24.
        assert_eq!(image(48, 48).logical(48).0, (48.0, 48.0));
    }

    #[test]
    fn shapes_map_to_theme_names_mutter_uses() {
        assert_eq!(
            icon_names(CursorIcon::Default)[..2],
            ["default", "left_ptr"]
        );
        assert!(icon_names(CursorIcon::Text).contains(&"xterm"));
        assert!(icon_names(CursorIcon::Grabbing).contains(&"hand2"));
        assert!(icon_names(CursorIcon::Move).contains(&"fleur"));
        assert_eq!(grab_icon(None), CursorIcon::Move);
        assert_eq!(grab_icon(Some(1 | 4)), CursorIcon::NwResize);
        assert_eq!(grab_icon(Some(2 | 8)), CursorIcon::SeResize);
        assert_eq!(grab_icon(Some(8)), CursorIcon::EResize);
        assert_eq!(grab_icon(Some(1)), CursorIcon::NResize);
    }

    #[test]
    fn grabs_beat_client_shapes_and_no_focus_shows_the_default() {
        let named = CursorImageStatus::Named(CursorIcon::Crosshair);
        assert_eq!(
            Pointer::resolve(&named, None, true),
            Pointer::Named(CursorIcon::Crosshair, "client")
        );
        assert_eq!(
            Pointer::resolve(&named, None, false),
            Pointer::Named(CursorIcon::Default, "default")
        );
        assert_eq!(
            Pointer::resolve(&named, Some(CursorIcon::SeResize), true),
            Pointer::Named(CursorIcon::SeResize, "grab")
        );
        assert_eq!(
            Pointer::resolve(&CursorImageStatus::Hidden, None, true),
            Pointer::Hidden
        );
    }

    #[test]
    fn a_lone_ctrl_tap_locates_the_pointer_only_when_enabled() {
        let key = |keycode, pressed| ManagerInput::Key {
            keycode,
            pressed,
            time: 0,
        };
        let at: Point<f64, Logical> = (100.0, 50.0).into();
        let now = Duration::from_secs(1);
        let mut locate = LocatePointer::default();
        assert!(!locate.feed(&key(29, true), at, now));
        assert!(!locate.feed(&key(29, false), at, now), "disabled");
        locate.set_enabled(true);
        locate.feed(&key(29, true), at, now);
        assert!(locate.feed(&key(29, false), at, now));
        assert!(locate.active(now + Duration::from_millis(1300)));
        assert!(!locate.active(now + Duration::from_millis(1350)));
        // Ctrl+C is a shortcut, not a tap.
        locate.feed(&key(29, true), at, now);
        locate.feed(&key(46, true), at, now);
        locate.feed(&key(46, false), at, now);
        assert!(!locate.feed(&key(29, false), at, now));
        // Ctrl pressed while another key is held is no tap either.
        locate.feed(&key(42, true), at, now);
        locate.feed(&key(29, true), at, now);
        assert!(!locate.feed(&key(29, false), at, now));
        locate.feed(&key(42, false), at, now);
        // A click while Ctrl is down (Ctrl+click) cancels it.
        locate.feed(&key(29, true), at, now);
        locate.feed(
            &ManagerInput::Button {
                button: 0x110,
                pressed: true,
                time: 0,
            },
            at,
            now,
        );
        assert!(!locate.feed(&key(29, false), at, now));
        assert_eq!(locate.count, 1);
        // Right Ctrl is not Mutter's default key.
        locate.feed(&key(97, true), at, now);
        assert!(!locate.feed(&key(97, false), at, now));
    }

    #[test]
    fn locate_disc_cache_evicts_on_both_entry_and_byte_limits() {
        let mut locate = LocatePointer::default();
        for i in 0..100u8 {
            locate.cached_discs(32, [i, 0, 0], [0.0; 3]);
            assert!(locate.discs.len() <= MAX_DISC_ENTRIES);
            assert!(
                locate
                    .discs
                    .keys()
                    .map(|(side, _)| disc_bytes(*side))
                    .sum::<usize>()
                    <= MAX_DISC_BYTES
            );
        }
        locate.discs.clear();
        locate.cached_discs(1024, [0; 3], [0.0; 3]);
        locate.cached_discs(512, [1; 3], [0.0; 3]);
        assert_eq!(locate.discs.len(), 2);
        // A hit reuses buffers rather than evicting under the byte limit.
        locate.cached_discs(1024, [0; 3], [0.0; 3]);
        assert_eq!(locate.discs.len(), 2);
        locate.cached_discs(512, [2; 3], [0.0; 3]);
        assert_eq!(locate.discs.len(), 1);
        assert!(locate.discs.contains_key(&(512, [2; 3])));
        for invalid in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(disc_side(invalid).is_none());
        }
        assert_eq!(disc_side(f64::MAX), Some(MAX_DISC_SIDE));
    }

    #[test]
    fn locking_during_a_ripple_clears_activity_and_redacts_all_cursor_history() {
        let mut cursor = Cursor::new();
        cursor.locate.set_enabled(true);
        cursor.locate.set_key("Control_L");
        let key = |pressed| ManagerInput::Key {
            keycode: 29,
            pressed,
            time: 0,
        };
        let now = Duration::from_secs(1);
        cursor.locate.feed(&key(true), (100.0, 50.0).into(), now);
        assert!(cursor.locate.feed(&key(false), (100.0, 50.0).into(), now));
        cursor.locate.frames = 3;
        assert_eq!(
            cursor.diagnostics(now, false)["locate_pointer"]["at"],
            serde_json::json!([100.0, 50.0])
        );
        // Even before transition cleanup, publication fails closed.
        assert!(cursor.diagnostics(now, true).is_null());
        cursor.locate.feed(&key(true), (100.0, 50.0).into(), now);
        cursor.on_lock();
        assert!(!cursor.animating(now));
        assert!(cursor.locate.held.is_empty());
        assert!(cursor.locate.armed.is_none());
        assert!(cursor.locate.playing.is_none());
        assert!(cursor.shown.is_none());
        assert_eq!((cursor.locate.count, cursor.locate.frames), (0, 0));
        assert!(cursor.locate.enabled());
        assert!(cursor.locate.matches_keysym([0xffe3]));
        assert!(!cursor.locate.feed(&key(false), (100.0, 50.0).into(), now));
        assert!(cursor.diagnostics(now, false)["locate_pointer"]["at"].is_null());
    }

    #[test]
    fn configured_locate_keys_and_invalid_names() {
        let mut locate = LocatePointer::default();
        locate.set_key("Control_L");
        assert!(locate.matches_keysym([0xffe3]));
        assert!(!locate.matches_keysym([0xffe4]));
        locate.set_key("Control_R");
        assert!(locate.matches_keysym([0xffe4]));
        assert!(!locate.matches_keysym([0xffe3]));
        for name in ["", "NotARealKeysym", "Control_L\0"] {
            locate.set_key(name);
            assert!(!locate.matches_keysym([0, 0xffe3, 0xffe4]));
        }
    }

    #[test]
    fn locate_resolves_control_through_real_swapped_caps_keymap() {
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let mut locate = LocatePointer::default();
        locate.set_key("Control_L");
        for (options, left_control, caps_lock) in [
            (None, true, false),
            (Some("ctrl:swapcaps".into()), false, true),
        ] {
            let keymap = xkb::Keymap::new_from_names(
                &context,
                "",
                "",
                "us",
                "",
                options,
                xkb::KEYMAP_COMPILE_NO_FLAGS,
            )
            .expect("system XKB us keymap");
            let state = xkb::State::new(&keymap);
            let matches = |evdev: u32| {
                let code = (evdev + 8).into();
                locate.matches_keysym(
                    keymap
                        .key_get_syms_by_level(code, state.key_get_layout(code), 0)
                        .iter()
                        .map(|symbol| symbol.raw()),
                )
            };
            assert_eq!(matches(29), left_control);
            assert_eq!(matches(58), caps_lock);
        }
    }

    #[test]
    fn locate_matches_remapped_physical_keys_and_repeat_preserves_tap() {
        let key = |keycode, pressed| ManagerInput::Key {
            keycode,
            pressed,
            time: 0,
        };
        let mut locate = LocatePointer::default();
        locate.set_enabled(true);
        let point = (10.0, 20.0).into();
        let now = Duration::ZERO;
        // XKB has mapped Control_L to Caps Lock's evdev position.
        assert!(!locate.feed_matched(&key(58, true), true, point, now));
        assert!(!locate.feed_matched(&key(58, true), true, point, now));
        assert!(locate.feed_matched(&key(58, false), true, point, now));
        // A non-matching physical left Ctrl never triggers.
        locate.feed_matched(&key(29, true), false, point, now);
        assert!(!locate.feed_matched(&key(29, false), false, point, now));
        // Releasing a different physical key cannot fire an armed tap.
        locate.feed_matched(&key(58, true), true, point, now);
        assert!(!locate.feed_matched(&key(97, false), true, point, now));
        assert!(!locate.feed_matched(&key(58, false), true, point, now));
        locate.feed_matched(&key(58, true), true, point, now);
        locate.set_key("Control_R");
        assert!(!locate.feed_matched(&key(58, false), true, point, now));
        assert_eq!(locate.count, 1);
    }

    #[test]
    fn ripples_follow_gnome_timing() {
        assert_eq!(ripple_ring(0, 0.0), Some((0.25, 1.0)));
        // Ring 2 waits at its start values for its 50 ms delay.
        let (scale, opacity) = ripple_ring(1, 25.0).unwrap();
        assert_eq!(scale, 0.0);
        assert!((opacity - 0.7f64.sqrt()).abs() < 1e-9);
        let (scale, opacity) = ripple_ring(0, 415.0).unwrap();
        assert!((scale - (0.25 + 1.25 * 0.75)).abs() < 1e-9);
        assert!((opacity - 0.75).abs() < 1e-9);
        assert!(ripple_ring(0, 830.0).is_none());
        assert!(ripple_ring(2, 1349.0).is_some());
        assert!(ripple_ring(2, 1350.0).is_none());
        assert_eq!(ripples_end_ms(), 1350.0);
    }

    #[test]
    fn ripple_disc_is_a_translucent_lightened_accent() {
        let accent = [53.0 / 255.0, 132.0 / 255.0, 228.0 / 255.0];
        let light = lighten(accent, 0.3);
        assert!(light.iter().zip(accent).all(|(l, a)| *l >= a));
        assert_eq!(lighten([0.5, 0.5, 0.5], 0.25), [0.75, 0.75, 0.75]);
        let disc = ripple_disc(54, accent, 2.0 / 54.0);
        let center = ((27 * 54 + 27) * 4) as usize;
        assert!(
            (76..=77).contains(&disc[center + 3]),
            "30% opacity in the middle"
        );
        assert_eq!(&disc[..4], &[0, 0, 0, 0], "corners are clear");
    }
}
