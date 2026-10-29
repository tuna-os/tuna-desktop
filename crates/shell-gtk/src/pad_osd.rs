//! gnome-shell 51's pad OSD (`js/ui/padOsd.js`): the full-monitor overlay
//! that draws a drawing tablet pad's libwacom layout with every button,
//! ring and strip labelled with its action, and in edition mode lets the
//! user assign an action to the one they press (`PadOsd`, `PadDiagram`,
//! `ActionEditor`, `ActionComboBox`, `KeybindingEntry`, `PadChooser`).
//!
//! The compositor opens it (`windowManager.js` `_showPadOsd`), forwards
//! the pad events it captures (`vfunc_captured_event`) and refreshes its
//! labels; the shell answers `PadOsdClosed` whenever it closes and asks
//! for another pad's OSD through `RequestPadOsd`.
//!
//! The diagram renders through resvg instead of librsvg. gnome-shell wraps
//! the layout in an SVG that XIncludes it under a `<style>` element; here
//! the same stylesheet is spliced into the layout's root element, which
//! styles the same elements. Label anchors come from the layout's
//! `#LabelX`/`#LeaderX` elements as `_getItemLabelCoords` reads them.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gdk, gio, glib};
use gtk4_layer_shell::{KeyboardMode, Layer, LayerShell};
use resvg::{tiny_skia, usvg};
use tuna_shell_control::{PadOsd as PadOsdInfo, PadOsdEvent, PadOsdLabels};

/// The relocatable schema edition writes (`_startActionEdition`).
pub const PAD_BUTTON_SCHEMA: &str = "org.gnome.desktop.peripherals.tablet.pad-button";

/// gnome-shell 51 `data/theme/pad-osd.css`, the diagram's stylesheet.
pub const PAD_OSD_CSS: &str = ".Leader {
    stroke-width: .5 !important;
    stroke: #535353;
    fill: none !important;
}

.Button {
    stroke-width: .25;
    stroke: #ededed;
    fill: #ededed;
}

.Ring {
    stroke-width: .5 !important;
    stroke: #535353 !important;
    fill: none !important;
}

.Label {
    stroke: none !important;
    stroke-width: .1 !important;
    fill: transparent !important;
}

.TouchStrip, .TouchRing {
    stroke-width: .1 !important;
    stroke: #ededed !important;
    fill: #535353 !important;
}
";

/// `GDesktopPadButtonAction` values and `ActionComboBox`'s labels, in
/// its menu order.
const ACTION_NONE: i32 = 0;
const ACTION_HELP: i32 = 1;
const ACTION_SWITCH_MONITOR: i32 = 2;
const ACTION_KEYBINDING: i32 = 3;
const ACTIONS: [(i32, &str); 4] = [
    (ACTION_NONE, "App defined"),
    (ACTION_HELP, "Show on-screen help"),
    (ACTION_SWITCH_MONITOR, "Switch monitor"),
    (ACTION_KEYBINDING, "Assign keystroke"),
];

/// One labelled pad feature (`Meta.PadFeatureType` plus direction; a
/// button has neither).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item {
    Button(u32),
    /// `ccw` false is `Meta.PadDirection.CW`.
    Ring {
        ring: u32,
        ccw: bool,
    },
    /// `down` false is `Meta.PadDirection.UP`.
    Strip {
        strip: u32,
        down: bool,
    },
}

/// `String.fromCharCode('A'.charCodeAt() + n)`.
fn letter(n: u32) -> char {
    char::from_u32(u32::from(b'A') + n).unwrap_or('?')
}

/// `number > 0 ? (number + 1).toString() : ''`.
fn number_suffix(n: u32) -> String {
    if n > 0 {
        (n + 1).to_string()
    } else {
        String::new()
    }
}

impl Item {
    /// `_getButtonLabels`, `_getRingLabels`, `_getStripLabels`: the
    /// layout's label and leader element ids.
    pub fn element_ids(self) -> (String, String) {
        let suffix = match self {
            Item::Button(button) => letter(button).to_string(),
            Item::Ring { ring, ccw } => {
                format!(
                    "Ring{}{}",
                    number_suffix(ring),
                    if ccw { "CCW" } else { "CW" }
                )
            }
            Item::Strip { strip, down } => format!(
                "Strip{}{}",
                number_suffix(strip),
                if down { "Down" } else { "Up" }
            ),
        };
        (format!("Label{suffix}"), format!("Leader{suffix}"))
    }

    /// The pad-button settings key of `_start*ActionEdition` (`mode` is
    /// ignored for buttons).
    pub fn settings_key(self, mode: u32) -> String {
        match self {
            Item::Button(button) => format!("button{}", letter(button)),
            Item::Ring { ring, ccw } => format!(
                "ring{}-{}-mode-{mode}",
                letter(ring),
                if ccw { "ccw" } else { "cw" }
            ),
            Item::Strip { strip, down } => format!(
                "strip{}-{}-mode-{mode}",
                letter(strip),
                if down { "down" } else { "up" }
            ),
        }
    }

    /// `_endActionEdition`'s follow-up: ring CCW then CW, strip Up then
    /// Down.
    pub fn next_direction(self) -> Option<Item> {
        match self {
            Item::Ring { ring, ccw: true } => Some(Item::Ring { ring, ccw: false }),
            Item::Strip { strip, down: false } => Some(Item::Strip { strip, down: true }),
            _ => None,
        }
    }
}

/// `${settings.path}${key}/`, the edited action's settings path.
pub fn edition_settings_path(settings_path: &str, item: Item, mode: u32) -> String {
    format!("{settings_path}{}/", item.settings_key(mode))
}

/// `_getActionText`: the compositor's label, else "None".
pub fn action_text(labels: &PadOsdLabels, item: Item) -> String {
    let label = match item {
        Item::Button(button) => labels
            .buttons
            .get(button as usize)
            .and_then(|b| b.label.clone()),
        Item::Ring { ring, ccw } => labels
            .rings
            .get(ring as usize)
            .and_then(|r| r[usize::from(ccw)].clone()),
        Item::Strip { strip, down } => labels
            .strips
            .get(strip as usize)
            .and_then(|s| s[usize::from(down)].clone()),
    };
    label.unwrap_or_else(|| "None".to_owned())
}

fn is_mode_switch(labels: &PadOsdLabels, button: u32) -> bool {
    labels
        .buttons
        .get(button as usize)
        .is_some_and(|b| b.mode_switch_group.is_some())
}

/// An element's extents in layout coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Which side of its anchor a label grows to (padOsd.js `LTR`/`RTL`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arrangement {
    Ltr,
    Rtl,
}

/// `_getItemLabelCoords` once both elements are found: left to right when
/// the label starts past the leader's end, anchored at the label's
/// bottom-left; a left-handed (180° rotated) diagram mirrors both.
pub fn label_coords(
    label: Rect,
    leader: Rect,
    left_handed: bool,
    image: (f64, f64),
) -> (f64, f64, Arrangement) {
    let mut arrangement = if label.x > leader.x + leader.width {
        Arrangement::Ltr
    } else {
        Arrangement::Rtl
    };
    let (mut x, mut y) = (label.x, label.y + label.height);
    if left_handed {
        arrangement = match arrangement {
            Arrangement::Ltr => Arrangement::Rtl,
            Arrangement::Rtl => Arrangement::Ltr,
        };
        x = image.0 - x;
        y = image.1 - y;
    }
    (x, y, arrangement)
}

/// One label of the diagram (`PadDiagram._labels`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spot {
    pub item: Item,
    pub x: f64,
    pub y: f64,
    pub arrangement: Arrangement,
}

/// A parsed libwacom layout: its source, size, and label spots.
pub struct Diagram {
    source: String,
    pub width: f64,
    pub height: f64,
    pub spots: Vec<Spot>,
}

impl Diagram {
    /// Load a layout (`set image`): `None` when it does not parse, as
    /// gnome-shell's handle is then null and nothing draws.
    pub fn parse(source: String, left_handed: bool) -> Option<Self> {
        let tree = usvg::Tree::from_str(&source, &usvg::Options::default()).ok()?;
        let doc = roxmltree::Document::parse_with_options(
            &source,
            roxmltree::ParsingOptions {
                allow_dtd: true,
                ..Default::default()
            },
        )
        .ok()?;
        let (width, height) = (
            f64::from(tree.size().width()),
            f64::from(tree.size().height()),
        );
        let root = view_box_transform(&doc, &tree);
        let coords = |item: Item| {
            let (label, leader) = item.element_ids();
            let label = element_bounds(&tree, &doc, root, &label)?;
            let leader = element_bounds(&tree, &doc, root, &leader)?;
            let (x, y, arrangement) = label_coords(label, leader, left_handed, (width, height));
            Some(Spot {
                item,
                x,
                y,
                arrangement,
            })
        };
        let mut spots = Vec::new();
        let mut add = |item: Item| match coords(item) {
            Some(spot) => {
                spots.push(spot);
                true
            }
            None => false,
        };
        // `_initLabels`: each kind stops at its first missing label.
        let mut i = 0;
        while add(Item::Button(i)) {
            i += 1;
        }
        let mut i = 0;
        while add(Item::Ring {
            ring: i,
            ccw: false,
        }) && add(Item::Ring { ring: i, ccw: true })
        {
            i += 1;
        }
        let mut i = 0;
        while add(Item::Strip {
            strip: i,
            down: false,
        }) && add(Item::Strip {
            strip: i,
            down: true,
        }) {
            i += 1;
        }
        // The parsed document borrows `source`; release it to keep it.
        drop(doc);
        Some(Self {
            source,
            width,
            height,
            spots,
        })
    }

    /// The layout under `css` (`_composeStyledDiagram`).
    pub fn styled_tree(&self, css: &str) -> Option<usvg::Tree> {
        let styled = styled_source(&self.source, css)?;
        usvg::Tree::from_str(&styled, &usvg::Options::default()).ok()
    }
}

/// `_cssString`: the stylesheet plus the pressed buttons' highlight.
pub fn diagram_css(active_buttons: &[u32], active_color: &str) -> String {
    let mut css = PAD_OSD_CSS.to_owned();
    for &button in active_buttons {
        let ch = letter(button);
        css += &format!(".{ch}.Leader {{ stroke: {active_color} !important; }}");
        css += &format!(
            ".{ch}.Button {{ stroke: {active_color} !important; fill: {active_color} !important; }}"
        );
    }
    css
}

/// padOsd.js `ACTIVE_COLOR`, `st-lighten(-st-accent-color, 15%)`, resolved
/// as St does (`get_color_from_lighten_term`: HSL luminance + 0.15) so the
/// SVG renderer gets a plain color. `accent` is `#rrggbb`.
pub fn active_color(accent: &str) -> String {
    let hex = accent.trim_start_matches('#');
    let channel = |i: usize| {
        hex.get(i..i + 2)
            .and_then(|c| u8::from_str_radix(c, 16).ok())
            .map_or(0.0, |c| f64::from(c) / 255.0)
    };
    let (r, g, b) = (channel(0), channel(2), channel(4));
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let l = (max + min) / 2.0;
    let (h, s) = if max == min {
        (0.0, 0.0)
    } else {
        let d = max - min;
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
    let l = (l + 0.15).clamp(0.0, 1.0);
    let (r, g, b) = if s == 0.0 {
        (l, l, l)
    } else {
        let q = if l < 0.5 {
            l * (1.0 + s)
        } else {
            l + s - l * s
        };
        let p = 2.0 * l - q;
        let hue = |mut t: f64| {
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
        (hue(h + 1.0 / 3.0), hue(h), hue(h - 1.0 / 3.0))
    };
    let byte = |v: f64| (v * 255.0).round().clamp(0.0, 255.0) as u8;
    format!("#{:02x}{:02x}{:02x}", byte(r), byte(g), byte(b))
}

/// Splice `<style>` in as the root element's first child.
fn styled_source(source: &str, css: &str) -> Option<String> {
    let doc = roxmltree::Document::parse_with_options(
        source,
        roxmltree::ParsingOptions {
            allow_dtd: true,
            ..Default::default()
        },
    )
    .ok()?;
    let start = doc.root_element().range().start;
    // The end of the root start tag: the first `>` outside a quoted value.
    let mut quote = None;
    let mut end = None;
    for (i, c) in source[start..].char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '>') => {
                end = Some(start + i);
                break;
            }
            _ => {}
        }
    }
    let end = end?;
    // A self-closing root has no children to style.
    if source[..end].ends_with('/') {
        return None;
    }
    let mut out = String::with_capacity(source.len() + css.len() + 64);
    out.push_str(&source[..=end]);
    out.push_str("<style type=\"text/css\"><![CDATA[");
    out.push_str(css);
    out.push_str("]]></style>");
    out.push_str(&source[end + 1..]);
    Some(out)
}

/// The root viewBox mapping (uniform `xMidYMid meet`), as usvg applies it
/// to the canvas.
fn view_box_transform(doc: &roxmltree::Document, tree: &usvg::Tree) -> tiny_skia::Transform {
    let Some(vb) = doc
        .root_element()
        .attribute("viewBox")
        .and_then(|v| v.parse::<svgtypes::ViewBox>().ok())
    else {
        return tiny_skia::Transform::identity();
    };
    if vb.w <= 0.0 || vb.h <= 0.0 {
        return tiny_skia::Transform::identity();
    }
    let (w, h) = (
        f64::from(tree.size().width()),
        f64::from(tree.size().height()),
    );
    let s = (w / vb.w).min(h / vb.h);
    let tx = (w - vb.w * s) / 2.0 - vb.x * s;
    let ty = (h - vb.h * s) / 2.0 - vb.y * s;
    tiny_skia::Transform::from_row(s as f32, 0.0, 0.0, s as f32, tx as f32, ty as f32)
}

/// An element's canvas extents (`get_position_sub`/`get_dimensions_sub`):
/// drawn elements from the rendered tree, stroke included; `<text>` from
/// its anchor, font size and length (resvg is built without text shaping).
fn element_bounds(
    tree: &usvg::Tree,
    doc: &roxmltree::Document,
    root: tiny_skia::Transform,
    id: &str,
) -> Option<Rect> {
    if let Some(node) = tree.node_by_id(id) {
        let r = node.abs_stroke_bounding_box();
        return Some(Rect {
            x: f64::from(r.x()),
            y: f64::from(r.y()),
            width: f64::from(r.width()),
            height: f64::from(r.height()),
        });
    }
    let element = doc
        .descendants()
        .find(|n| n.is_element() && n.attribute("id") == Some(id))?;
    if element.tag_name().name() != "text" {
        return None;
    }
    text_bounds(element, root)
}

/// One property from an element or its ancestors: `style` wins over the
/// presentation attribute.
fn inherited<'a>(node: roxmltree::Node<'a, 'a>, property: &str) -> Option<&'a str> {
    node.ancestors().filter(|n| n.is_element()).find_map(|n| {
        n.attribute("style")
            .and_then(|style| {
                style.split(';').find_map(|decl| {
                    let (name, value) = decl.split_once(':')?;
                    (name.trim() == property).then(|| value.trim())
                })
            })
            .or_else(|| n.attribute(property))
    })
}

/// The first number of a coordinate or length list (`52`, `8px`).
fn first_number(value: &str) -> Option<f64> {
    let first = value
        .split(|c: char| c == ',' || c.is_whitespace())
        .find(|s| !s.is_empty())?;
    let digits = first.trim_end_matches(|c: char| c.is_ascii_alphabetic() || c == '%');
    digits.parse().ok()
}

/// A text label's ink box estimate: capitals 0.7em tall standing on the
/// baseline, 0.6em per character, placed by `text-anchor`, then every
/// transform up to the canvas.
fn text_bounds(text: roxmltree::Node, root: tiny_skia::Transform) -> Option<Rect> {
    let positioned = std::iter::once(text)
        .chain(text.descendants().filter(|n| n.has_tag_name("tspan")))
        .find(|n| n.attribute("x").is_some() || n.attribute("y").is_some())
        .unwrap_or(text);
    let coord = |name| {
        positioned
            .attribute(name)
            .and_then(first_number)
            .unwrap_or(0.0)
    };
    let (x, y) = (coord("x"), coord("y"));
    let font_size = inherited(text, "font-size")
        .and_then(first_number)
        .unwrap_or(16.0);
    let chars: String = text.descendants().filter_map(|n| n.text()).collect();
    let width = chars.trim().chars().count() as f64 * 0.6 * font_size;
    let ascent = 0.7 * font_size;
    let left = match inherited(positioned, "text-anchor") {
        Some("middle") => x - width / 2.0,
        Some("end") => x - width,
        _ => x,
    };
    let mut ts = root;
    let chain: Vec<_> = text.ancestors().filter(|n| n.is_element()).collect();
    for node in chain.iter().rev() {
        if let Some(t) = node
            .attribute("transform")
            .and_then(|t| t.parse::<svgtypes::Transform>().ok())
        {
            ts = ts.pre_concat(tiny_skia::Transform::from_row(
                t.a as f32, t.b as f32, t.c as f32, t.d as f32, t.e as f32, t.f as f32,
            ));
        }
    }
    let mut corners = [
        tiny_skia::Point::from_xy(left as f32, (y - ascent) as f32),
        tiny_skia::Point::from_xy((left + width) as f32, (y - ascent) as f32),
        tiny_skia::Point::from_xy(left as f32, y as f32),
        tiny_skia::Point::from_xy((left + width) as f32, y as f32),
    ];
    ts.map_points(&mut corners);
    let xs = corners.iter().map(|p| f64::from(p.x));
    let ys = corners.iter().map(|p| f64::from(p.y));
    let (x0, x1) = (
        xs.clone().fold(f64::MAX, f64::min),
        xs.fold(f64::MIN, f64::max),
    );
    let (y0, y1) = (
        ys.clone().fold(f64::MAX, f64::min),
        ys.fold(f64::MIN, f64::max),
    );
    Some(Rect {
        x: x0,
        y: y0,
        width: x1 - x0,
        height: y1 - y0,
    })
}

/// `_updateDiagramScale` plus `_allocateChild`: where a child of natural
/// size `natural` goes for `spot` in an `area`-sized diagram.
pub fn place(
    spot: (f64, f64, Arrangement),
    natural: (i32, i32),
    area: (f64, f64),
    image: (f64, f64),
) -> gdk::Rectangle {
    let scale = (area.0 / image.0).min(area.1 / image.1);
    let x = spot.0 * scale + area.0 / 2.0 - image.0 / 2.0 * scale;
    let y = spot.1 * scale + area.1 / 2.0 - image.1 / 2.0 * scale;
    let x1 = match spot.2 {
        Arrangement::Ltr => x,
        Arrangement::Rtl => x - f64::from(natural.0),
    };
    let y1 = y - f64::from(natural.1) / 2.0;
    gdk::Rectangle::new(x1.round() as i32, y1.round() as i32, natural.0, natural.1)
}

/// `vfunc_repaint`: the layout scaled to fit `size` pixels, centered, and
/// turned 180° for left-handed use.
pub fn render(tree: &usvg::Tree, size: (u32, u32), left_handed: bool) -> Option<tiny_skia::Pixmap> {
    let mut pixmap = tiny_skia::Pixmap::new(size.0, size.1)?;
    let (iw, ih) = (tree.size().width(), tree.size().height());
    let (w, h) = (size.0 as f32, size.1 as f32);
    let scale = (w / iw).min(h / ih);
    let mut ts = tiny_skia::Transform::from_translate(w / 2.0, h / 2.0).pre_scale(scale, scale);
    if left_handed {
        ts = ts.pre_rotate(180.0);
    }
    let ts = ts.pre_translate(-iw / 2.0, -ih / 2.0);
    resvg::render(tree, ts, &mut pixmap.as_mut());
    Some(pixmap)
}

/// Whether the pad-button schema is installed: without it there is no
/// edition (the Edit button stays insensitive).
fn edition_available() -> bool {
    gio::SettingsSchemaSource::default()
        .and_then(|source| source.lookup(PAD_BUTTON_SCHEMA, true))
        .is_some()
}

/// `-st-accent-color` from `org.gnome.desktop.interface accent-color`.
fn current_accent() -> &'static str {
    let name = gio::SettingsSchemaSource::default()
        .and_then(|source| source.lookup("org.gnome.desktop.interface", true))
        .filter(|schema| schema.has_key("accent-color"))
        .map(|_| {
            gio::Settings::new("org.gnome.desktop.interface")
                .string("accent-color")
                .to_string()
        })
        .unwrap_or_default();
    crate::logic::accent_hex(&name)
}

/// The monitor with `connector`, if any.
pub fn monitor_for_connector(connector: &str) -> Option<gdk::Monitor> {
    let monitors = gdk::Display::default()?.monitors();
    (0..monitors.n_items())
        .filter_map(|i| monitors.item(i)?.downcast::<gdk::Monitor>().ok())
        .find(|m| m.connector().as_deref() == Some(connector))
}

/// `PadDiagram`: the styled layout with its labels and the action editor
/// laid over it.
struct DiagramView {
    diagram: Diagram,
    left_handed: bool,
    active_color: String,
    active: RefCell<Vec<u32>>,
    styled: RefCell<Option<usvg::Tree>>,
    /// The last paint: its pixel size and surface.
    cache: RefCell<Option<((i32, i32), gtk::cairo::ImageSurface)>>,
    area: gtk::DrawingArea,
    overlay: gtk::Overlay,
    labels: Vec<(gtk::Label, Spot)>,
    editor: gtk::Widget,
    /// Index into `labels` of the label being edited (`_curEdited`).
    edited: Cell<Option<usize>>,
}

impl DiagramView {
    fn new(diagram: Diagram, left_handed: bool, editor: gtk::Widget) -> Rc<Self> {
        let area = gtk::DrawingArea::new();
        area.set_hexpand(true);
        area.set_vexpand(true);
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&area));
        overlay.set_hexpand(true);
        overlay.set_vexpand(true);
        let labels: Vec<_> = diagram
            .spots
            .iter()
            .map(|spot| {
                let label = gtk::Label::new(None);
                label.add_css_class("pad-osd-label");
                overlay.add_overlay(&label);
                (label, *spot)
            })
            .collect();
        editor.set_visible(false);
        overlay.add_overlay(&editor);
        let view = Rc::new(Self {
            diagram,
            left_handed,
            active_color: active_color(current_accent()),
            active: RefCell::default(),
            styled: RefCell::new(None),
            cache: RefCell::new(None),
            area,
            overlay,
            labels,
            editor,
            edited: Cell::new(None),
        });
        view.restyle();
        let weak = Rc::downgrade(&view);
        view.area.set_draw_func(move |area, cr, w, h| {
            if let Some(view) = weak.upgrade() {
                view.draw(area, cr, w, h);
            }
        });
        let weak = Rc::downgrade(&view);
        view.overlay
            .connect_get_child_position(move |overlay, child| {
                let view = weak.upgrade()?;
                let spot = if *child == view.editor {
                    view.labels.get(view.edited.get()?)?.1
                } else {
                    view.labels
                        .iter()
                        .find(|(l, _)| l.upcast_ref::<gtk::Widget>() == child)?
                        .1
                };
                let (_, natural) = child.preferred_size();
                Some(place(
                    (spot.x, spot.y, spot.arrangement),
                    (natural.width(), natural.height()),
                    (f64::from(overlay.width()), f64::from(overlay.height())),
                    (view.diagram.width, view.diagram.height),
                ))
            });
        view
    }

    fn draw(&self, area: &gtk::DrawingArea, cr: &gtk::cairo::Context, w: i32, h: i32) {
        let factor = area.scale_factor().max(1);
        let (pw, ph) = (w * factor, h * factor);
        if pw <= 0 || ph <= 0 {
            return;
        }
        let mut cache = self.cache.borrow_mut();
        if cache.as_ref().is_none_or(|(size, _)| *size != (pw, ph)) {
            *cache = None;
            let styled = self.styled.borrow();
            let Some(tree) = styled.as_ref() else {
                return;
            };
            let Some(pixmap) = render(tree, (pw as u32, ph as u32), self.left_handed) else {
                return;
            };
            // tiny-skia's premultiplied RGBA into cairo's premultiplied
            // native-endian ARGB32 (B, G, R, A in memory).
            let mut data = pixmap.take();
            for px in data.as_chunks_mut::<4>().0 {
                px.swap(0, 2);
            }
            let Ok(surface) = gtk::cairo::ImageSurface::create_for_data(
                data,
                gtk::cairo::Format::ARgb32,
                pw,
                ph,
                pw * 4,
            ) else {
                return;
            };
            *cache = Some(((pw, ph), surface));
        }
        if let Some((_, surface)) = cache.as_ref() {
            cr.save().ok();
            cr.scale(1.0 / f64::from(factor), 1.0 / f64::from(factor));
            if cr.set_source_surface(surface, 0.0, 0.0).is_ok() {
                cr.paint().ok();
            }
            cr.restore().ok();
        }
    }

    /// `_invalidateSvg`.
    fn restyle(&self) {
        let css = diagram_css(&self.active.borrow(), &self.active_color);
        *self.styled.borrow_mut() = self.diagram.styled_tree(&css);
        self.cache.borrow_mut().take();
        self.area.queue_draw();
    }

    fn activate_button(&self, button: u32) {
        self.active.borrow_mut().push(button);
        self.restyle();
    }

    fn deactivate_button(&self, button: u32) {
        self.active.borrow_mut().retain(|b| *b != button);
        self.restyle();
    }

    /// `updateLabels`.
    fn update_labels(&self, text: impl Fn(Item) -> String) {
        for (label, spot) in &self.labels {
            label.set_label(&text(spot.item));
        }
        self.overlay.queue_allocate();
    }

    /// `startEdition`: the editor takes the label's place.
    fn start_edition(&self, item: Item) {
        if self.edited.get().is_some() {
            return;
        }
        let Some(index) = self.labels.iter().position(|(_, s)| s.item == item) else {
            return;
        };
        self.edited.set(Some(index));
        self.editor.set_visible(true);
        self.labels[index].0.set_visible(false);
        self.overlay.queue_allocate();
    }

    /// `stopEdition`.
    fn stop_edition(&self, text: &str) {
        self.editor.set_visible(false);
        if let Some(index) = self.edited.take() {
            let label = &self.labels[index].0;
            label.set_label(text);
            label.set_visible(true);
        }
        self.overlay.queue_allocate();
    }
}

/// `ActionEditor` with its `ActionComboBox` and `KeybindingEntry`.
struct ActionEditor {
    root: gtk::Box,
    combo: gtk::MenuButton,
    combo_label: gtk::Label,
    items: Vec<(i32, gtk::Button)>,
    entry: gtk::Entry,
    settings: RefCell<Option<gio::Settings>>,
    action: Cell<i32>,
    keybinding: RefCell<String>,
    on_done: RefCell<Option<Rc<dyn Fn()>>>,
}

impl ActionEditor {
    fn new() -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        root.add_css_class("pad-osd-action-editor");
        let combo = gtk::MenuButton::new();
        let combo_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let combo_label = gtk::Label::new(None);
        combo_label.add_css_class("combo-box-label");
        combo_label.set_xalign(0.0);
        combo_box.append(&combo_label);
        let arrow = gtk::Image::from_icon_name("pan-down-symbolic");
        combo_box.append(&arrow);
        combo.set_child(Some(&combo_box));
        let menu = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let items: Vec<_> = ACTIONS
            .iter()
            .map(|(action, label)| {
                let item = gtk::Button::with_label(label);
                item.add_css_class("flat");
                if let Some(child) = item.child().and_downcast::<gtk::Label>() {
                    child.set_xalign(0.0);
                }
                menu.append(&item);
                (*action, item)
            })
            .collect();
        let popover = gtk::Popover::new();
        popover.set_child(Some(&menu));
        popover.set_has_arrow(false);
        combo.set_popover(Some(&popover));
        root.append(&combo);
        let entry = gtk::Entry::new();
        entry.set_placeholder_text(Some("New shortcut…"));
        entry.set_width_chars(10);
        root.append(&entry);
        let done = gtk::Button::with_label("Done");
        done.set_hexpand(false);
        root.append(&done);
        let editor = Rc::new(Self {
            root,
            combo,
            combo_label,
            items,
            entry,
            settings: RefCell::new(None),
            action: Cell::new(ACTION_NONE),
            keybinding: RefCell::new(String::new()),
            on_done: RefCell::new(None),
        });
        editor.set_action_label(ACTION_NONE);
        for (action, item) in &editor.items {
            let (weak, action) = (Rc::downgrade(&editor), *action);
            item.connect_clicked(move |_| {
                if let Some(editor) = weak.upgrade() {
                    // `_onActionSelected`.
                    editor.set_action_label(action);
                    editor.combo.popdown();
                    editor.action.set(action);
                    editor.update_entry();
                }
            });
        }
        // `KeybindingEntry.vfunc_captured_event`: a key press names the
        // shortcut (`Meta.accelerator_name`).
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(&editor);
        keys.connect_key_pressed(move |_, key, _, state| {
            let Some(editor) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            let name = gtk::accelerator_name(key, state & gtk::accelerator_get_default_mod_mask());
            editor.entry.set_text(&name);
            *editor.keybinding.borrow_mut() = name.to_string();
            glib::Propagation::Stop
        });
        editor.entry.add_controller(keys);
        let weak = Rc::downgrade(&editor);
        done.connect_clicked(move |_| {
            let Some(editor) = weak.upgrade() else {
                return;
            };
            // `_onEditingDone`.
            editor.store_settings();
            editor.close();
            let done = editor.on_done.borrow().clone();
            if let Some(done) = done {
                done();
            }
        });
        editor
    }

    fn set_action_label(&self, action: i32) {
        let label = ACTIONS
            .iter()
            .find(|(a, _)| *a == action)
            .map_or(ACTIONS[0].1, |(_, l)| *l);
        self.combo_label.set_label(label);
    }

    /// `_updateKeybindingEntryState`.
    fn update_entry(&self) {
        if self.action.get() == ACTION_KEYBINDING {
            self.entry.set_text(&self.keybinding.borrow());
            self.entry.set_visible(true);
            self.entry.grab_focus();
        } else {
            self.entry.set_visible(false);
        }
    }

    /// `setSettings`: help and switch-monitor apply to buttons only.
    fn set_settings(&self, settings: gio::Settings, is_button: bool) {
        self.action.set(settings.enum_("action"));
        *self.keybinding.borrow_mut() = settings.string("keybinding").to_string();
        *self.settings.borrow_mut() = Some(settings);
        self.set_action_label(self.action.get());
        self.update_entry();
        for (action, item) in &self.items {
            if matches!(*action, ACTION_HELP | ACTION_SWITCH_MONITOR) {
                item.set_sensitive(is_button);
            }
        }
    }

    /// `_storeSettings`.
    fn store_settings(&self) {
        let Some(settings) = self.settings.borrow().clone() else {
            return;
        };
        let action = self.action.get();
        if let Err(e) = settings.set_enum("action", action) {
            eprintln!("tuna-shell-gtk: pad OSD: action: {e}");
        }
        let keybinding = self.keybinding.borrow().clone();
        if action == ACTION_KEYBINDING && !keybinding.is_empty() {
            if let Err(e) = settings.set_string("keybinding", &keybinding) {
                eprintln!("tuna-shell-gtk: pad OSD: keybinding: {e}");
            }
        } else {
            settings.reset("keybinding");
        }
    }

    /// `close`.
    fn close(&self) {
        self.combo.popdown();
        self.root.set_visible(false);
    }
}

/// What the OSD tells the compositor.
#[derive(Clone)]
pub struct Callbacks {
    /// `PadOsdClosed { pad }`.
    pub closed: Rc<dyn Fn(u32)>,
    /// `RequestPadOsd { target: Pad(pad), edition_mode }`.
    pub request: Rc<dyn Fn(u32, bool)>,
}

/// Runs once a shown OSD closed.
type OnClosed = Rc<dyn Fn(&Rc<Shown>)>;

/// One shown OSD (`PadOsd`).
struct Shown {
    info: PadOsdInfo,
    labels: RefCell<PadOsdLabels>,
    window: gtk::Window,
    title: gtk::Label,
    tip: gtk::Label,
    edit: gtk::Button,
    edition_mode: Cell<bool>,
    edition_available: bool,
    diagram: Option<Rc<DiagramView>>,
    editor: Rc<ActionEditor>,
    /// `_editedAction`: the item and its mode.
    edited: Cell<Option<(Item, u32)>>,
    closed: Cell<bool>,
    on_closed: OnClosed,
    request: Rc<dyn Fn(u32, bool)>,
}

impl Shown {
    /// `_syncEditionMode`.
    fn sync_edition_mode(&self) {
        let edition = self.edition_mode.get();
        self.edit.set_sensitive(!edition && self.edition_available);
        self.edit
            .set_opacity(if edition { 128.0 / 255.0 } else { 1.0 });
        if edition {
            self.title.set_label("Press a button to configure");
            self.tip.set_label("Press Esc to exit");
        } else {
            self.title.set_label(&self.info.name);
            self.tip.set_label("Press any key to exit");
        }
    }

    fn set_edition_mode(&self, edition: bool) {
        if self.edition_mode.replace(edition) != edition {
            self.sync_edition_mode();
        }
    }

    /// `_updateActionLabels`.
    fn update_action_labels(&self) {
        if let Some(diagram) = &self.diagram {
            let labels = self.labels.borrow();
            diagram.update_labels(|item| action_text(&labels, item));
        }
    }

    /// `_endActionEdition`.
    fn end_action_edition(&self) {
        self.editor.close();
        if let Some((item, mode)) = self.edited.take() {
            let text = action_text(&self.labels.borrow(), item);
            if let Some(diagram) = &self.diagram {
                diagram.stop_edition(&text);
            }
            if let Some(next) = item.next_direction() {
                self.start_action_edition(next, mode);
            }
        }
    }

    /// `_startActionEdition`.
    fn start_action_edition(&self, item: Item, mode: u32) {
        if !self.edition_available {
            return;
        }
        if self.edited.get().is_some_and(|(edited, _)| edited == item) {
            return;
        }
        self.end_action_edition();
        self.edited.set(Some((item, mode)));
        let path = edition_settings_path(&self.info.settings_path, item, mode);
        let settings = gio::Settings::with_path(PAD_BUTTON_SCHEMA, &path);
        self.editor
            .set_settings(settings, matches!(item, Item::Button(_)));
        if let Some(diagram) = &self.diagram {
            diagram.start_edition(item);
        }
    }

    /// `vfunc_captured_event` for pad events.
    fn input(self: &Rc<Self>, pad: u32, event: PadOsdEvent) {
        let own = pad == self.info.pad;
        let edition = self.edition_mode.get();
        match event {
            PadOsdEvent::Button {
                button,
                pressed: true,
            } if own => {
                if let Some(diagram) = &self.diagram {
                    diagram.activate_button(button);
                }
                // Buttons that switch between modes cannot be edited.
                if edition && !is_mode_switch(&self.labels.borrow(), button) {
                    self.start_action_edition(Item::Button(button), 0);
                }
                return;
            }
            PadOsdEvent::Button {
                button,
                pressed: false,
            } if own => {
                if let Some(diagram) = &self.diagram {
                    diagram.deactivate_button(button);
                }
                if is_mode_switch(&self.labels.borrow(), button) {
                    self.end_action_edition();
                    self.update_action_labels();
                }
                return;
            }
            PadOsdEvent::Strip { strip, mode } if own && edition => {
                self.start_action_edition(Item::Strip { strip, down: false }, mode);
            }
            PadOsdEvent::Ring { ring, mode } if own && edition => {
                self.start_action_edition(Item::Ring { ring, ccw: true }, mode);
            }
            _ => {}
        }
        // An event from another pad of the group shows that pad's OSD.
        if self.info.group_pads.iter().any(|p| p.pad == pad) {
            self.request_for_other_pad(pad);
        }
    }

    /// `_requestForOtherPad`.
    fn request_for_other_pad(self: &Rc<Self>, pad: u32) {
        if pad == self.info.pad || !self.info.group_pads.iter().any(|p| p.pad == pad) {
            return;
        }
        let edition = self.edition_mode.get();
        self.close();
        (self.request)(pad, edition);
    }

    /// A key press (`vfunc_captured_event`): any key closes, in edition
    /// mode only Escape, which first ends an edition under way.
    fn key(self: &Rc<Self>, key: gdk::Key) -> glib::Propagation {
        if self.edition_mode.get() && key != gdk::Key::Escape {
            return glib::Propagation::Proceed;
        }
        if self.edited.get().is_some() {
            self.end_action_edition();
        } else {
            self.close();
        }
        glib::Propagation::Stop
    }

    /// `destroy` → `_onDestroy` → `closed`.
    fn close(self: &Rc<Self>) {
        if self.closed.replace(true) {
            return;
        }
        self.editor.close();
        self.window.destroy();
        (self.on_closed)(self);
    }

    /// Replaced by a fresh OSD for the same pad: no `closed` answer.
    fn dismiss(&self) {
        if !self.closed.replace(true) {
            self.editor.close();
            self.window.destroy();
        }
    }
}

/// The pad OSD service: one OSD at a time.
pub struct PadOsdUi {
    app: gtk::Application,
    callbacks: Callbacks,
    current: RefCell<Option<Rc<Shown>>>,
}

impl PadOsdUi {
    pub fn new(app: &gtk::Application, callbacks: Callbacks) -> Rc<Self> {
        Rc::new(Self {
            app: app.clone(),
            callbacks,
            current: RefCell::new(None),
        })
    }

    fn current(&self) -> Option<Rc<Shown>> {
        self.current.borrow().clone()
    }

    /// Show the OSD the compositor opened (`_showPadOsd`).
    pub fn show(self: &Rc<Self>, info: PadOsdInfo) {
        let old = self.current.borrow_mut().take();
        if let Some(old) = old {
            if old.info.pad == info.pad {
                old.dismiss();
            } else {
                old.close();
            }
        }
        let shown = self.build(info);
        *self.current.borrow_mut() = Some(shown.clone());
        shown.window.present();
    }

    /// `PadOsdLabels`: the shown pad's labels changed.
    pub fn labels(&self, pad: u32, labels: PadOsdLabels) {
        if let Some(shown) = self.current().filter(|s| s.info.pad == pad) {
            *shown.labels.borrow_mut() = labels;
            shown.update_action_labels();
        }
    }

    /// `PadOsdInput`: a captured pad event.
    pub fn input(&self, pad: u32, event: PadOsdEvent) {
        if let Some(shown) = self.current() {
            shown.input(pad, event);
        }
    }

    /// `ClosePadOsd`: the shown pad went away.
    pub fn close_pad(&self, pad: u32) {
        if let Some(shown) = self.current().filter(|s| s.info.pad == pad) {
            shown.close();
        }
    }

    /// Decline an OSD the compositor opened: it closed at once.
    pub fn refuse(&self, pad: u32) {
        (self.callbacks.closed)(pad);
    }

    /// Close whatever is shown (the session locked).
    pub fn close(&self) {
        if let Some(shown) = self.current() {
            shown.close();
        }
    }

    fn build(self: &Rc<Self>, info: PadOsdInfo) -> Rc<Shown> {
        let window = gtk::Window::new();
        window.set_application(Some(&self.app));
        window.set_title(Some("Pad OSD"));
        window.add_css_class("tuna-pad-osd");
        window.init_layer_shell();
        window.set_layer(Layer::Overlay);
        window.set_namespace(Some(tuna_shell_control::PAD_OSD_NAMESPACE));
        for edge in [
            gtk4_layer_shell::Edge::Top,
            gtk4_layer_shell::Edge::Bottom,
            gtk4_layer_shell::Edge::Left,
            gtk4_layer_shell::Edge::Right,
        ] {
            window.set_anchor(edge, true);
        }
        window.set_exclusive_zone(-1);
        // `Main.pushModal`.
        window.set_keyboard_mode(KeyboardMode::Exclusive);
        let monitor = info.connector.as_deref().and_then(monitor_for_connector);
        window.set_monitor(monitor.as_ref());

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("pad-osd-window");
        let title_box = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        title_box.add_css_class("pad-osd-title-box");
        title_box.set_halign(gtk::Align::Center);
        let label_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
        label_box.add_css_class("pad-osd-title-menu-box");
        let title = gtk::Label::new(Some(&info.name));
        title.add_css_class("pad-osd-title");
        let tip = gtk::Label::new(None);
        label_box.append(&title);
        label_box.append(&tip);
        title_box.append(&label_box);
        root.append(&title_box);

        let editor = ActionEditor::new();
        let diagram = std::fs::read_to_string(&info.layout_path)
            .ok()
            .and_then(|source| Diagram::parse(source, info.left_handed))
            .map(|d| DiagramView::new(d, info.left_handed, editor.root.clone().upcast()));
        match &diagram {
            Some(view) => root.append(&view.overlay),
            None => {
                eprintln!(
                    "tuna-shell-gtk: pad OSD: cannot load layout {}",
                    info.layout_path
                );
                let filler = gtk::Box::new(gtk::Orientation::Vertical, 0);
                filler.set_vexpand(true);
                root.append(&filler);
            }
        }

        let edit = gtk::Button::with_label("Edit…");
        edit.set_halign(gtk::Align::Center);
        edit.set_valign(gtk::Align::Center);
        root.append(&edit);
        window.set_child(Some(&root));

        let pad_chooser = (info.group_pads.len() > 1).then(|| pad_chooser(&info));
        let weak_ui = Rc::downgrade(self);
        let closed = self.callbacks.closed.clone();
        let on_closed: OnClosed = Rc::new(move |shown: &Rc<Shown>| {
            if let Some(ui) = weak_ui.upgrade() {
                let mut current = ui.current.borrow_mut();
                if current.as_ref().is_some_and(|c| Rc::ptr_eq(c, shown)) {
                    current.take();
                }
            }
            closed(shown.info.pad);
        });
        let shown = Rc::new(Shown {
            labels: RefCell::new(info.labels.clone()),
            edition_mode: Cell::new(info.edition_mode),
            info,
            window,
            title,
            tip,
            edit,
            edition_available: edition_available(),
            diagram,
            editor: editor.clone(),
            edited: Cell::new(None),
            closed: Cell::new(false),
            on_closed,
            request: self.callbacks.request.clone(),
        });
        if let Some((button, entries)) = pad_chooser {
            title_box.append(&button);
            for (pad, item) in entries {
                let (weak, button) = (Rc::downgrade(&shown), button.clone());
                item.connect_clicked(move |_| {
                    button.popdown();
                    if let Some(shown) = weak.upgrade() {
                        shown.request_for_other_pad(pad);
                    }
                });
            }
        }
        let weak = Rc::downgrade(&shown);
        *editor.on_done.borrow_mut() = Some(Rc::new(move || {
            if let Some(shown) = weak.upgrade() {
                shown.end_action_edition();
            }
        }));
        let weak = Rc::downgrade(&shown);
        shown.edit.connect_clicked(move |_| {
            if let Some(shown) = weak.upgrade() {
                shown.set_edition_mode(true);
            }
        });
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(&shown);
        keys.connect_key_pressed(move |_, key, _, _| match weak.upgrade() {
            Some(shown) => shown.key(key),
            None => glib::Propagation::Proceed,
        });
        shown.window.add_controller(keys);
        let weak = Rc::downgrade(&shown);
        shown.window.connect_close_request(move |_| {
            if let Some(shown) = weak.upgrade() {
                shown.close();
            }
            glib::Propagation::Stop
        });
        shown.update_action_labels();
        shown.sync_edition_mode();
        shown
    }
}

/// `PadChooser`: a toggle with a menu of the group's other pads.
fn pad_chooser(info: &PadOsdInfo) -> (gtk::MenuButton, Vec<(u32, gtk::Button)>) {
    let button = gtk::MenuButton::new();
    button.add_css_class("pad-chooser-button");
    button.set_icon_name("pan-down-symbolic");
    let menu = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let entries: Vec<_> = info
        .group_pads
        .iter()
        .filter(|p| p.pad != info.pad)
        .map(|p| {
            let item = gtk::Button::with_label(&p.name);
            item.add_css_class("flat");
            menu.append(&item);
            (p.pad, item)
        })
        .collect();
    let popover = gtk::Popover::new();
    popover.set_child(Some(&menu));
    popover.set_has_arrow(false);
    button.set_popover(Some(&popover));
    (button, entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Button A reads left to right (its label right of the leader),
    /// button B right to left (label left of the leader, end-anchored);
    /// one ring with both directions; one strip with only its Up label.
    const FIXTURE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100" style="font-size:8">
  <g id="GroupA">
    <rect id="ButtonA" class="A Button" x="10" y="10" width="20" height="10"/>
    <path id="LeaderA" class="A Leader" d="M 32 15 L 40 15"/>
    <text id="LabelA" class="A Label" x="42" y="18" style="text-anchor:start">A</text>
  </g>
  <g transform="translate(0,-2)">
    <rect id="ButtonB" class="B Button" x="170" y="40" width="20" height="10"/>
    <path id="LeaderB" class="B Leader" d="M 160 45 L 168 45"/>
    <text id="LabelB" class="B Label" x="158" y="48" style="text-anchor:end">B</text>
  </g>
  <path id="LeaderRingCW" class="Leader" d="M 100 60 L 110 60"/>
  <rect id="LabelRingCW" class="Label" x="112" y="56" width="10" height="6"/>
  <path id="LeaderRingCCW" class="Leader" d="M 100 70 L 110 70"/>
  <rect id="LabelRingCCW" class="Label" x="112" y="66" width="10" height="6"/>
  <path id="LeaderStripUp" class="Leader" d="M 50 80 L 60 80"/>
  <rect id="LabelStripUp" class="Label" x="62" y="76" width="10" height="6"/>
</svg>"#;

    fn rect(x: f64, y: f64, width: f64, height: f64) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn label_coords_follow_get_item_label_coords() {
        let leader = rect(30.0, 14.0, 10.0, 1.0);
        // Past the leader's end: left to right from the label's bottom-left.
        assert_eq!(
            label_coords(rect(42.0, 12.0, 6.0, 6.0), leader, false, (200.0, 100.0)),
            (42.0, 18.0, Arrangement::Ltr)
        );
        // Starting at the leader's end exactly is not past it.
        assert_eq!(
            label_coords(rect(40.0, 12.0, 6.0, 6.0), leader, false, (200.0, 100.0)).2,
            Arrangement::Rtl
        );
        assert_eq!(
            label_coords(rect(20.0, 12.0, 6.0, 6.0), leader, false, (200.0, 100.0)),
            (20.0, 18.0, Arrangement::Rtl)
        );
        // Left-handed: mirrored through the center, direction flipped.
        assert_eq!(
            label_coords(rect(42.0, 12.0, 6.0, 6.0), leader, true, (200.0, 100.0)),
            (158.0, 82.0, Arrangement::Rtl)
        );
        assert_eq!(
            label_coords(rect(20.0, 12.0, 6.0, 6.0), leader, true, (200.0, 100.0)),
            (180.0, 82.0, Arrangement::Ltr)
        );
    }

    #[test]
    fn diagram_reads_labels_from_the_layout_like_init_labels() {
        let diagram = Diagram::parse(FIXTURE.to_owned(), false).expect("fixture parses");
        assert_eq!((diagram.width, diagram.height), (200.0, 100.0));
        let items: Vec<Item> = diagram.spots.iter().map(|s| s.item).collect();
        // The strip's Down label is missing: Up stays, the strips stop.
        assert_eq!(
            items,
            [
                Item::Button(0),
                Item::Button(1),
                Item::Ring {
                    ring: 0,
                    ccw: false
                },
                Item::Ring { ring: 0, ccw: true },
                Item::Strip {
                    strip: 0,
                    down: false
                },
            ]
        );
        let a = diagram.spots[0];
        // A start-anchored text label sits on its baseline at its x.
        assert_eq!(a.arrangement, Arrangement::Ltr);
        assert!(
            (a.x - 42.0).abs() < 1e-3 && (a.y - 18.0).abs() < 1e-3,
            "{a:?}"
        );
        let b = diagram.spots[1];
        // End-anchored, left of its leader, under the group's translate.
        assert_eq!(b.arrangement, Arrangement::Rtl);
        assert!(b.x < 158.0, "{b:?}");
        assert!((b.y - 46.0).abs() < 1e-3, "{b:?}");
        let ring_cw = diagram.spots[2];
        assert_eq!(ring_cw.arrangement, Arrangement::Ltr);
        assert!((ring_cw.x - 112.0).abs() < 1e-3 && (ring_cw.y - 62.0).abs() < 1e-3);

        let mirrored = Diagram::parse(FIXTURE.to_owned(), true).expect("fixture parses");
        let a = mirrored.spots[0];
        assert_eq!(a.arrangement, Arrangement::Rtl);
        assert!(
            (a.x - 158.0).abs() < 1e-3 && (a.y - 82.0).abs() < 1e-3,
            "{a:?}"
        );
        assert_eq!(mirrored.spots[1].arrangement, Arrangement::Ltr);
    }

    #[test]
    fn element_ids_follow_pad_diagram_names() {
        assert_eq!(
            Item::Button(2).element_ids(),
            ("LabelC".to_owned(), "LeaderC".to_owned())
        );
        assert_eq!(
            Item::Ring { ring: 0, ccw: true }.element_ids(),
            ("LabelRingCCW".to_owned(), "LeaderRingCCW".to_owned())
        );
        assert_eq!(
            Item::Ring {
                ring: 1,
                ccw: false
            }
            .element_ids(),
            ("LabelRing2CW".to_owned(), "LeaderRing2CW".to_owned())
        );
        assert_eq!(
            Item::Strip {
                strip: 1,
                down: true
            }
            .element_ids(),
            ("LabelStrip2Down".to_owned(), "LeaderStrip2Down".to_owned())
        );
    }

    #[test]
    fn edition_settings_paths_follow_start_action_edition() {
        let base = "/org/gnome/desktop/peripherals/tablets/056a:0357/";
        assert_eq!(
            edition_settings_path(base, Item::Button(0), 3),
            format!("{base}buttonA/")
        );
        assert_eq!(
            edition_settings_path(base, Item::Ring { ring: 1, ccw: true }, 2),
            format!("{base}ringB-ccw-mode-2/")
        );
        assert_eq!(
            edition_settings_path(
                base,
                Item::Ring {
                    ring: 0,
                    ccw: false
                },
                0
            ),
            format!("{base}ringA-cw-mode-0/")
        );
        assert_eq!(
            edition_settings_path(
                base,
                Item::Strip {
                    strip: 0,
                    down: false
                },
                1
            ),
            format!("{base}stripA-up-mode-1/")
        );
        assert_eq!(
            edition_settings_path(
                base,
                Item::Strip {
                    strip: 1,
                    down: true
                },
                0
            ),
            format!("{base}stripB-down-mode-0/")
        );
    }

    #[test]
    fn edition_follows_up_on_the_next_direction() {
        assert_eq!(
            Item::Ring { ring: 0, ccw: true }.next_direction(),
            Some(Item::Ring {
                ring: 0,
                ccw: false
            })
        );
        assert_eq!(
            Item::Ring {
                ring: 0,
                ccw: false
            }
            .next_direction(),
            None
        );
        assert_eq!(
            Item::Strip {
                strip: 1,
                down: false
            }
            .next_direction(),
            Some(Item::Strip {
                strip: 1,
                down: true
            })
        );
        assert_eq!(
            Item::Strip {
                strip: 1,
                down: true
            }
            .next_direction(),
            None
        );
        assert_eq!(Item::Button(0).next_direction(), None);
    }

    #[test]
    fn action_text_falls_back_to_none() {
        let labels = PadOsdLabels {
            buttons: vec![tuna_shell_control::PadOsdButton {
                label: Some("Undo".into()),
                mode_switch_group: None,
            }],
            rings: vec![[Some("Zoom in".into()), None]],
            strips: vec![],
            ring_modes: vec![0],
            strip_modes: vec![],
        };
        assert_eq!(action_text(&labels, Item::Button(0)), "Undo");
        assert_eq!(action_text(&labels, Item::Button(1)), "None");
        assert_eq!(
            action_text(
                &labels,
                Item::Ring {
                    ring: 0,
                    ccw: false
                }
            ),
            "Zoom in"
        );
        assert_eq!(
            action_text(&labels, Item::Ring { ring: 0, ccw: true }),
            "None"
        );
        assert_eq!(
            action_text(
                &labels,
                Item::Strip {
                    strip: 0,
                    down: false
                }
            ),
            "None"
        );
    }

    #[test]
    fn active_buttons_restyle_leader_and_button() {
        assert_eq!(diagram_css(&[], "#78aded"), PAD_OSD_CSS);
        let css = diagram_css(&[0, 2], "#78aded");
        let extra = css
            .strip_prefix(PAD_OSD_CSS)
            .expect("base stylesheet first");
        assert_eq!(
            extra,
            ".A.Leader { stroke: #78aded !important; }\
             .A.Button { stroke: #78aded !important; fill: #78aded !important; }\
             .C.Leader { stroke: #78aded !important; }\
             .C.Button { stroke: #78aded !important; fill: #78aded !important; }"
        );
    }

    #[test]
    fn active_color_lightens_the_accent_like_st() {
        // Adwaita blue #3584e4 at HSL luminance + 15%.
        assert_eq!(active_color("#3584e4"), "#78aded");
        // Grey stays grey.
        assert_eq!(active_color("#808080"), "#a6a6a6");
        assert_eq!(active_color("#ffffff"), "#ffffff");
    }

    /// The highlight reaches the rendered pixels: button A turns from the
    /// stylesheet's #ededed to the active color.
    #[test]
    fn active_button_renders_in_the_active_color() {
        let diagram = Diagram::parse(FIXTURE.to_owned(), false).expect("fixture parses");
        let pixel = |css: &str| {
            let tree = diagram.styled_tree(css).expect("styled");
            let pixmap = render(&tree, (200, 100), false).expect("pixmap");
            let p = pixmap.pixel(20, 15).expect("in bounds");
            (p.red(), p.green(), p.blue(), p.alpha())
        };
        assert_eq!(pixel(&diagram_css(&[], "#ff0000")), (0xed, 0xed, 0xed, 255));
        assert_eq!(pixel(&diagram_css(&[0], "#ff0000")), (255, 0, 0, 255));
        // Left-handed turns the diagram 180°: button A lands bottom right.
        let tree = diagram.styled_tree(&diagram_css(&[0], "#ff0000")).unwrap();
        let turned = render(&tree, (200, 100), true).unwrap();
        let p = turned.pixel(180, 85).unwrap();
        assert_eq!((p.red(), p.green(), p.blue()), (255, 0, 0));
    }

    #[test]
    fn children_are_placed_like_allocate_child() {
        // A 400x100 area over a 200x100 layout: scale 1, centered by 100.
        let ltr = place(
            (42.0, 18.0, Arrangement::Ltr),
            (30, 10),
            (400.0, 100.0),
            (200.0, 100.0),
        );
        assert_eq!(ltr, gdk::Rectangle::new(142, 13, 30, 10));
        let rtl = place(
            (42.0, 18.0, Arrangement::Rtl),
            (30, 10),
            (400.0, 100.0),
            (200.0, 100.0),
        );
        assert_eq!(rtl, gdk::Rectangle::new(112, 13, 30, 10));
        // Twice the size: scale 2.
        let big = place(
            (42.0, 18.0, Arrangement::Ltr),
            (30, 10),
            (400.0, 200.0),
            (200.0, 100.0),
        );
        assert_eq!(big, gdk::Rectangle::new(84, 31, 30, 10));
    }
}
