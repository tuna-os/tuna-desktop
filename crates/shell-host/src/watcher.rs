//! AppIndicator host: StatusNotifierWatcher role plus item menus.
//!
//! The shell owns the `org.kde.StatusNotifierWatcher` name (GNOME
//! sessions run no watcher, so nothing fights for it) and tracks the
//! items that register. Icons paint into the bar's right edge beside
//! the service tiles; a press opens the item's dbusmenu as a popup.
//!
//! The core ([`IndicatorHost`], layout, paint) is wire-free. The
//! D-Bus edge ([`WatcherBus`]) polls on the panel's slow tick, or, once
//! [`WatcherBus::watch`] is set up, reports registrations, item change
//! signals and vanished items as they happen — the same degradation
//! shape as settings and tiles: no bus, or a taken watcher name, reads
//! as an empty host, never a hang.

use std::sync::{Arc, Mutex};

use crate::overview::{
    blit_glyph, glyph_index, put_pixel, ACCENT, BYTES_PER_PIXEL, DOT_GAP, DOT_MARGIN, DOT_SIZE,
    FONT_SCALE, GLYPH_ADVANCE,
};
use crate::popup::Rect;

/// Well-known watcher name and object path (kde StatusNotifier spec).
pub const WATCHER_NAME: &str = "org.kde.StatusNotifierWatcher";
/// Watcher object path.
pub const WATCHER_PATH: &str = "/StatusNotifierWatcher";
/// Fallback item path when a registration carries no object path.
pub const FALLBACK_ITEM_PATH: &str = "/StatusNotifierItem";
/// Fallback menu path is per-item; empty means no menu.
pub const INDICATOR_CELL: i32 = 28;

/// Wire shape of the item `ToolTip` property: `(sa(iiay)ss)`.
type WireToolTip = (String, Vec<Pixmap>, String, String);

/// One `a(iiay)` entry: width, height, big-endian ARGB32 bytes.
pub type Pixmap = (i32, i32, Vec<u8>);

/// One top-level dbusmenu row for the legacy one-level painter
/// ([`paint_indicator_menu`]): labelled, visible, non-separator rows
/// of the root, submenus arriving as their parent row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuEntry {
    /// dbusmenu id for the `Event` call.
    pub id: i32,
    /// Row label.
    pub label: String,
    /// False rows paint dimmed and never fire.
    pub enabled: bool,
}

/// A dbusmenu row's `toggle-type` with its `toggle-state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Toggle {
    /// No ornament.
    None,
    /// `checkmark`, checked when `toggle-state` is non-zero.
    Check(bool),
    /// `radio`, selected when `toggle-state` is non-zero.
    Radio(bool),
}

/// One visible dbusmenu node, as the AppIndicator extension builds its
/// menu items (`dbusMenu.js` `MenuItemFactory.createItem`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuNode {
    /// dbusmenu id for `Event` and `AboutToShow`.
    pub id: i32,
    /// Label with the dbusmenu mnemonic removed ([`menu_label`]).
    pub label: String,
    /// Insensitive rows show but never fire.
    pub enabled: bool,
    /// `type` `separator`.
    pub separator: bool,
    /// Check or radio ornament.
    pub toggle: Toggle,
    /// `icon-name`, empty when unset.
    pub icon_name: String,
    /// `icon-data` (PNG bytes), used only without `icon-name`.
    pub icon_data: Vec<u8>,
    /// `children-display` `submenu`: the row opens [`MenuNode::children`].
    pub submenu: bool,
    /// Visible children (shown only for submenus).
    pub children: Vec<MenuNode>,
}

/// A dbusmenu label as the AppIndicator extension shows it
/// (`dbusMenu.js` `_updateLabel`): the first underscore that starts a
/// mnemonic (one followed by any other character) is dropped.
pub fn menu_label(label: &str) -> String {
    let mut chars = label.char_indices().peekable();
    while let Some((index, c)) = chars.next() {
        if c == '_' && chars.peek().is_some_and(|(_, next)| *next != '_') {
            let mut out = String::with_capacity(label.len() - 1);
            out.push_str(&label[..index]);
            out.push_str(&label[index + 1..]);
            return out;
        }
    }
    label.to_owned()
}

/// The legacy painter's rows from a menu tree.
pub fn menu_entries(nodes: &[MenuNode]) -> Vec<MenuEntry> {
    nodes
        .iter()
        .filter(|node| !node.separator && !node.label.is_empty())
        .map(|node| MenuEntry {
            id: node.id,
            label: node.label.clone(),
            enabled: node.enabled,
        })
        .collect()
}

/// An indicator icon: a theme name, or raw pixmap bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndicatorIcon {
    /// Icon theme name (painted as its initial, like dock icons).
    Named(String),
    /// `width` x `height` ARGB32 pixels, compositor byte order.
    Pixmap {
        /// Pixmap width.
        width: i32,
        /// Pixmap height.
        height: i32,
        /// Native `B,G,R,A` bytes ready for the shm buffer.
        argb: Vec<u8>,
    },
}

/// One hosted indicator: identity plus its current icon and menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndicatorItem {
    /// Registration string (`bus name` or `bus name/path`).
    pub service: String,
    /// Item title for fallback paint.
    pub title: String,
    /// Current icon.
    pub icon: IndicatorIcon,
    /// Current one-level menu.
    pub menu: Vec<MenuEntry>,
}

/// Raw StatusNotifier properties for one item: identity, status,
/// and both icon channels. Name resolution happens in the caller
/// (which owns the artwork cache), so this stays bus-shaped.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ItemInfo {
    /// Registration id (`bus name/object path`).
    pub service: String,
    /// Item title (the service when the item sends none).
    pub title: String,
    /// Item status (`Active`, `Passive`, `NeedsAttention`).
    pub status: String,
    /// Normal icon theme name.
    pub icon_name: String,
    /// Normal icon pixmaps (`a(iiay)` payload).
    pub icon_pixmap: Vec<Pixmap>,
    /// Attention icon theme name.
    pub attention_name: String,
    /// Attention icon pixmaps.
    pub attention_pixmap: Vec<Pixmap>,
    /// `IconThemePath`: an extra directory the item's icon names
    /// resolve in (empty when unset).
    pub icon_theme_path: String,
    /// `XAyatanaLabel`, shown beside the icon (empty when unset).
    pub label: String,
    /// `IconAccessibleDesc` (Ayatana).
    pub icon_accessible_desc: String,
    /// `AttentionAccessibleDesc` (Ayatana).
    pub attention_accessible_desc: String,
    /// The item's dbusmenu object path; `None` without a menu or for
    /// libappindicator's `/NO_DBUSMENU`.
    pub menu_path: Option<String>,
    /// Tooltip title line (empty when the item sends none).
    pub tooltip_title: String,
    /// Tooltip body text (empty when the item sends none).
    pub tooltip_body: String,
}

impl ItemInfo {
    /// Largest normal pixmap as an icon, if any.
    pub fn pixmap_icon(&self) -> Option<IndicatorIcon> {
        pixmaps_icon(&self.icon_pixmap)
    }

    /// Largest attention pixmap as an icon, if any.
    pub fn attention_pixmap_icon(&self) -> Option<IndicatorIcon> {
        pixmaps_icon(&self.attention_pixmap)
    }

    /// True while the item requests attention.
    pub fn needs_attention(&self) -> bool {
        self.status == "NeedsAttention"
    }

    /// False only for `Passive` items, which the AppIndicator extension
    /// hides (`indicatorStatusIcon.js` `_updateStatus`).
    pub fn visible(&self) -> bool {
        self.status != "Passive"
    }

    /// The icon to show: the attention icon (name, else pixmaps) while
    /// the item needs attention and has one, the normal icon otherwise.
    pub fn current_icon(&self) -> (&str, &[Pixmap]) {
        let attention_set =
            !self.attention_name.is_empty() || pick_pixmap(self.attention_pixmap.clone()).is_some();
        if self.needs_attention() && attention_set {
            (&self.attention_name, &self.attention_pixmap)
        } else {
            (&self.icon_name, &self.icon_pixmap)
        }
    }

    /// The accessible name as the AppIndicator extension computes it
    /// (`appIndicator.js` `accessibleName`): the status's accessible
    /// description, else the label, else the title.
    pub fn accessible_name(&self) -> &str {
        let desc = if self.needs_attention() {
            &self.attention_accessible_desc
        } else {
            &self.icon_accessible_desc
        };
        [desc.as_str(), self.label.as_str(), self.title.as_str()]
            .into_iter()
            .find(|s| !s.is_empty())
            .unwrap_or(self.service.as_str())
    }
}

/// Largest pixmap in a payload as an icon, if any is usable.
fn pixmaps_icon(pixmaps: &[(i32, i32, Vec<u8>)]) -> Option<IndicatorIcon> {
    let (w, h, bytes) = pick_pixmap(pixmaps.to_vec())?;
    Some(IndicatorIcon::Pixmap {
        width: w,
        height: h,
        argb: argb_to_shm(&bytes),
    })
}

/// Wire-free indicator set: register, refresh, remove by service.
#[derive(Debug, Default)]
pub struct IndicatorHost {
    items: Vec<IndicatorItem>,
}

impl IndicatorHost {
    /// Empty host.
    pub fn new() -> Self {
        Self::default()
    }

    /// Current items in registration order.
    pub fn items(&self) -> &[IndicatorItem] {
        &self.items
    }

    /// Item for `service`, if hosted.
    pub fn get(&self, service: &str) -> Option<&IndicatorItem> {
        self.items.iter().find(|item| item.service == service)
    }

    /// Insert or replace the item for `service`.
    pub fn upsert(&mut self, item: IndicatorItem) {
        if let Some(slot) = self
            .items
            .iter_mut()
            .find(|slot| slot.service == item.service)
        {
            *slot = item;
        } else {
            self.items.push(item);
        }
    }

    /// Remove the item for `service`. Returns whether one was hosted.
    pub fn remove(&mut self, service: &str) -> bool {
        let before = self.items.len();
        self.items.retain(|item| item.service != service);
        self.items.len() != before
    }

    /// Drop services no longer registered (vanished clients).
    pub fn retain_registered(&mut self, registered: &[String]) {
        self.items.retain(|item| registered.contains(&item.service));
    }
}

/// Cells for `count` indicators extending left from `right_x` (the
/// tile row's left edge) in a `strip_h`-high strip.
pub fn indicator_cells(right_x: i32, strip_h: i32, count: usize) -> Vec<Rect> {
    (0..count)
        .map(|index| {
            let x = right_x - (index as i32 + 1) * INDICATOR_CELL;
            Rect {
                x,
                y: (strip_h - INDICATOR_CELL) / 2,
                w: INDICATOR_CELL,
                h: INDICATOR_CELL,
            }
        })
        .collect()
}

/// Left edge a `count`-cell indicator row starts from, given the
/// surface `width`: mirror of the tile packing in
/// [`crate::popup::panel_layout`] (three dots from the right
/// margin), so cells sit exactly left of the sound tile.
pub fn indicator_right_x(width: i32) -> i32 {
    width - DOT_MARGIN - 3 * DOT_SIZE - 2 * DOT_GAP - 4 - 4
}

/// Cell index under the strip point, if any.
pub fn indicator_at(cells: &[Rect], x: i32, y: i32) -> Option<usize> {
    cells.iter().position(|cell| cell.contains(x, y))
}

/// Paint indicator cells: pixmaps blit centered and clipped, named
/// icons paint their initial like dock icons.
pub fn paint_indicators(pixels: &mut [u8], width: i32, cells: &[Rect], items: &[IndicatorItem]) {
    let stride = width as usize * BYTES_PER_PIXEL;
    for (cell, item) in cells.iter().zip(items.iter()) {
        // Cell frame so the indicator reads as a tile.
        for dx in 0..cell.w {
            put_pixel(pixels, stride, cell.x + dx, cell.y, ACCENT);
            put_pixel(pixels, stride, cell.x + dx, cell.y + cell.h - 1, ACCENT);
        }
        match &item.icon {
            IndicatorIcon::Pixmap {
                width,
                height,
                argb,
            } => {
                let w = (*width).min(cell.w - 4).max(1);
                let h = (*height).min(cell.h - 4).max(1);
                let ox = cell.x + (cell.w - w) / 2;
                let oy = cell.y + (cell.h - h) / 2;
                // Nearest-sample the pixmap into the cell.
                for dy in 0..h {
                    for dx in 0..w {
                        let sx = (dx * *width / w).clamp(0, *width - 1) as usize;
                        let sy = (dy * *height / h).clamp(0, *height - 1) as usize;
                        let from = (sy * *width as usize + sx) * BYTES_PER_PIXEL;
                        let into =
                            ((oy + dy) as usize * stride) + ((ox + dx) as usize * BYTES_PER_PIXEL);
                        if let (Some(src), Some(dst)) = (
                            argb.get(from..from + BYTES_PER_PIXEL),
                            pixels.get_mut(into..into + BYTES_PER_PIXEL),
                        ) {
                            dst.copy_from_slice(src);
                        }
                    }
                }
            }
            IndicatorIcon::Named(name) => {
                let face = name.chars().next().unwrap_or('?');
                if let Some(glyph) = glyph_index(face) {
                    let gx = cell.x + (cell.w - 4 * FONT_SCALE) / 2;
                    let gy = cell.y + (cell.h - 5 * FONT_SCALE) / 2;
                    blit_glyph(pixels, stride, gx, gy, glyph, ACCENT);
                }
            }
        }
    }
}

/// Menu rows for an indicator popup: labels truncated to the box.
pub fn paint_indicator_menu(pixels: &mut [u8], stride: usize, rect: &Rect, item: &IndicatorItem) {
    const DIM: [u8; 4] = [0x4a, 0x44, 0x44, 0xff];
    const ROW_H: i32 = 24;
    let max_chars = ((rect.w - 24) / GLYPH_ADVANCE).max(1) as usize;
    for (row, entry) in item.menu.iter().enumerate() {
        let gy = rect.y + 10 + row as i32 * ROW_H;
        if gy + ROW_H > rect.y + rect.h {
            break;
        }
        let color = if entry.enabled { ACCENT } else { DIM };
        let mut gx = rect.x + 12;
        for ch in entry.label.chars().take(max_chars) {
            if ch == ' ' {
                gx += GLYPH_ADVANCE;
                continue;
            }
            if let Some(glyph) = glyph_index(ch) {
                blit_glyph(pixels, stride, gx, gy, glyph, color);
            }
            gx += GLYPH_ADVANCE;
        }
    }
}

/// Menu row under the popup point, if any.
pub fn menu_row_at(rect: &Rect, y: i32, count: usize) -> Option<usize> {
    const ROW_H: i32 = 24;
    if count == 0 || y < rect.y + 10 {
        return None;
    }
    let row = ((y - rect.y - 10) / ROW_H) as usize;
    (row < count).then_some(row)
}

/// Paint box for indicator-menu row `index`: the inverse of
/// [`menu_row_at`], so the keyboard focus ring lands where presses
/// do. Slightly inset from the menu box edges.
pub fn menu_row_rect(rect: &Rect, index: usize, count: usize) -> Option<Rect> {
    const ROW_H: i32 = 24;
    if index >= count {
        return None;
    }
    Some(Rect {
        x: rect.x + 4,
        y: rect.y + 10 + index as i32 * ROW_H,
        w: (rect.w - 8).max(0),
        h: ROW_H,
    })
}

/// Split a registration string into bus name and object path.
pub fn split_service(service: &str) -> (String, String) {
    match service.split_once('/') {
        Some((bus, path)) => (bus.to_owned(), format!("/{path}")),
        None => (service.to_owned(), FALLBACK_ITEM_PATH.to_owned()),
    }
}

/// Whether a `NameOwnerChanged(name, _, new_owner)` means one of the
/// registered `items` lost its bus name (the item went away).
fn vanished_item(items: &[String], name: &str, new_owner: &str) -> bool {
    new_owner.is_empty() && items.iter().any(|item| split_service(item).0 == name)
}

/// Pick the largest pixmap from an `IconPixmap` (`a(iiay)`) payload.
pub fn pick_pixmap(pixmaps: Vec<(i32, i32, Vec<u8>)>) -> Option<(i32, i32, Vec<u8>)> {
    pixmaps
        .into_iter()
        .filter(|(w, h, bytes)| {
            *w > 0 && *h > 0 && bytes.len() == *w as usize * *h as usize * BYTES_PER_PIXEL
        })
        .max_by_key(|(w, h, _)| *w as i64 * *h as i64)
}

/// Convert SNI pixmap bytes (big-endian ARGB32 words) to shm
/// `B,G,R,A` order.
pub fn argb_to_shm(argb: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(argb.len());
    let (chunks, _) = argb.as_chunks::<4>();
    // Wire bytes are [A, R, G, B]; little-endian ARGB8888 wants
    // [B, G, R, A].
    for px in chunks {
        out.push(px[3]);
        out.push(px[2]);
        out.push(px[1]);
        out.push(px[0]);
    }
    out
}

/// Parse one dbusmenu layout struct `(id, props, children)` into the
/// root's visible children, recursively.
pub fn parse_menu_layout(layout: &zbus::zvariant::OwnedValue) -> Vec<MenuNode> {
    let Ok(root) = zbus::zvariant::Value::try_from(layout) else {
        return Vec::new();
    };
    match menu_node_fields(&root) {
        Some((_, _, children)) => menu_children(children),
        None => Vec::new(),
    }
}

/// A node struct's `(id, props, children)`, looking through the variant
/// wrapper `av` children arrive in.
fn menu_node_fields<'a>(
    value: &'a zbus::zvariant::Value<'a>,
) -> Option<(
    i32,
    &'a zbus::zvariant::Dict<'a, 'a>,
    &'a zbus::zvariant::Array<'a>,
)> {
    use zbus::zvariant::Value;
    let node = match value {
        Value::Value(inner) => return menu_node_fields(inner),
        Value::Structure(node) => node,
        _ => return None,
    };
    match node.fields() {
        [Value::I32(id), Value::Dict(props), Value::Array(children), ..] => {
            Some((*id, props, children))
        }
        _ => None,
    }
}

fn menu_children(children: &zbus::zvariant::Array<'_>) -> Vec<MenuNode> {
    children
        .inner()
        .iter()
        .filter_map(parse_menu_node)
        .collect()
}

/// One property's value, looking through its variant wrapper.
fn menu_prop<'a>(
    props: &'a zbus::zvariant::Dict<'a, 'a>,
    key: &str,
) -> Option<&'a zbus::zvariant::Value<'a>> {
    use zbus::zvariant::Value;
    let (_, value) = props
        .iter()
        .find(|(k, _)| matches!(k, Value::Str(k) if k.as_str() == key))?;
    Some(match value {
        Value::Value(inner) => inner,
        other => other,
    })
}

// Values of the wrong type read as unset, as the extension's
// `PropertyStore` (`dbusMenu.js` `MandatedTypes`) ignores them.
fn menu_bool(props: &zbus::zvariant::Dict<'_, '_>, key: &str) -> Option<bool> {
    match menu_prop(props, key)? {
        zbus::zvariant::Value::Bool(value) => Some(*value),
        _ => None,
    }
}

fn menu_str(props: &zbus::zvariant::Dict<'_, '_>, key: &str) -> String {
    match menu_prop(props, key) {
        Some(zbus::zvariant::Value::Str(value)) => value.as_str().to_owned(),
        _ => String::new(),
    }
}

fn menu_bytes(props: &zbus::zvariant::Dict<'_, '_>, key: &str) -> Vec<u8> {
    match menu_prop(props, key) {
        Some(zbus::zvariant::Value::Array(bytes)) => bytes
            .inner()
            .iter()
            .map_while(|byte| match byte {
                zbus::zvariant::Value::U8(byte) => Some(*byte),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// One node to a [`MenuNode`]; `None` for invisible or malformed nodes.
fn parse_menu_node(value: &zbus::zvariant::Value<'_>) -> Option<MenuNode> {
    let (id, props, children) = menu_node_fields(value)?;
    if !menu_bool(props, "visible").unwrap_or(true) {
        return None;
    }
    let state = matches!(
        menu_prop(props, "toggle-state"),
        Some(zbus::zvariant::Value::I32(state)) if *state != 0
    );
    let toggle = match menu_str(props, "toggle-type").as_str() {
        "checkmark" => Toggle::Check(state),
        "radio" => Toggle::Radio(state),
        _ => Toggle::None,
    };
    let submenu = menu_str(props, "children-display") == "submenu";
    Some(MenuNode {
        id,
        label: menu_label(&menu_str(props, "label")),
        enabled: menu_bool(props, "enabled").unwrap_or(true),
        separator: menu_str(props, "type") == "separator",
        toggle,
        icon_name: menu_str(props, "icon-name"),
        icon_data: menu_bytes(props, "icon-data"),
        submenu,
        children: if submenu {
            menu_children(children)
        } else {
            Vec::new()
        },
    })
}

/// What changed, as [`WatcherBus::watch`] reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// An item registered (or re-registered), changed a property or
    /// left the bus: re-read the items.
    Items,
    /// The dbusmenu served by this unique bus name changed its layout
    /// or its items (`LayoutUpdated`, `ItemsPropertiesUpdated`).
    Menu(String),
}

/// Called when the hosted items may have changed ([`WatcherBus::watch`]).
#[derive(Clone)]
struct Wake(Arc<dyn Fn(Change) + Send + Sync>);

impl std::fmt::Debug for Wake {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Wake")
    }
}

/// Shared watcher registrations (the interface object and the poll
/// path read the same state).
#[derive(Debug, Default)]
struct WatcherState {
    /// Registered item ids (`bus/path`), in registration order.
    items: Vec<String>,
    /// Told about new registrations, once [`WatcherBus::watch`] ran.
    wake: Option<Wake>,
}

/// How a `RegisterStatusNotifierItem` argument names its item, read as
/// the AppIndicator extension reads it (`statusNotifierWatcher.js`
/// `RegisterStatusNotifierItemAsync`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Registration {
    /// The item lives at this bus name and object path.
    At(String, String),
    /// This well-known name's current owner serves the item at the path.
    Owner(String, String),
}

/// Parse a registration from the caller `sender`: an object path alone
/// (libayatana-appindicator) names the caller's object; `name/path`
/// and a bare name (default path) name another connection, a
/// well-known name standing for its current owner except the spec's own
/// `org.freedesktop.StatusNotifierItem-PID-ID`. `None` for anything else.
pub fn parse_registration(service: &str, sender: &str) -> Option<Registration> {
    use zbus::names::BusName;
    use zbus::zvariant::ObjectPath;
    if service.starts_with('/') {
        ObjectPath::try_from(service).ok()?;
        BusName::try_from(sender).ok()?;
        return Some(Registration::At(sender.to_owned(), service.to_owned()));
    }
    let (name, path) = match service.split_once('/') {
        Some((name, path)) => (name, format!("/{path}")),
        None => (service, FALLBACK_ITEM_PATH.to_owned()),
    };
    ObjectPath::try_from(path.as_str()).ok()?;
    match BusName::try_from(name).ok()? {
        BusName::Unique(_) => Some(Registration::At(name.to_owned(), path)),
        BusName::WellKnown(_) if path == FALLBACK_ITEM_PATH && spec_item_name(name) => {
            Some(Registration::At(name.to_owned(), path))
        }
        BusName::WellKnown(_) => Some(Registration::Owner(name.to_owned(), path)),
    }
}

/// `org.freedesktop.StatusNotifierItem-<pid>-<id>`, the spec's item name
/// (the extension's `BUS_SNI_NAME_REGEX`).
fn spec_item_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("org.freedesktop.StatusNotifierItem-") else {
        return false;
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    rest.split_once('-')
        .is_some_and(|(pid, id)| digits(pid) && digits(id))
}

/// The `org.kde.StatusNotifierWatcher` object: registration, the three
/// spec properties and the registration signals. This shell is the
/// only host; additional hosts are refused, as by the AppIndicator
/// extension.
#[derive(Debug)]
struct Watcher {
    state: Arc<Mutex<WatcherState>>,
}

#[zbus::interface(interface = "org.kde.StatusNotifierWatcher")]
impl Watcher {
    /// Register an item ([`parse_registration`]). Registering again
    /// re-reads the item.
    async fn register_status_notifier_item(
        &self,
        service: String,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(signal_emitter)] emitter: zbus::object_server::SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        let sender = header.sender().map(|s| s.as_str()).unwrap_or_default();
        let refused =
            || zbus::fdo::Error::InvalidArgs(format!("cannot register an item for '{service}'"));
        let (bus, path) = match parse_registration(&service, sender).ok_or_else(refused)? {
            Registration::At(bus, path) => (bus, path),
            Registration::Owner(name, path) => {
                let name = zbus::names::BusName::try_from(name).map_err(|_| refused())?;
                let owner = zbus::fdo::DBusProxy::new(conn)
                    .await?
                    .get_name_owner(name)
                    .await?;
                (owner.to_string(), path)
            }
        };
        let id = format!("{bus}{path}");
        let (fresh, wake) = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| zbus::fdo::Error::Failed("watcher state poisoned".into()))?;
            let fresh = !state.items.contains(&id);
            if fresh {
                state.items.push(id.clone());
            }
            (fresh, state.wake.clone())
        };
        if fresh {
            Self::status_notifier_item_registered(&emitter, &id).await?;
            self.registered_status_notifier_items_changed(&emitter)
                .await?;
        }
        if let Some(Wake(wake)) = wake {
            wake(Change::Items);
        }
        Ok(())
    }

    /// Other hosts are not supported: this shell hosts every item.
    fn register_status_notifier_host(&self, _service: String) -> zbus::fdo::Result<()> {
        Err(zbus::fdo::Error::NotSupported(
            "Registering additional notification hosts is not supported".into(),
        ))
    }

    /// Registered item ids (`bus/path`).
    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> Vec<String> {
        self.state
            .lock()
            .map(|state| state.items.clone())
            .unwrap_or_default()
    }

    /// This shell is the host.
    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> bool {
        true
    }

    /// Watcher protocol version (always zero per the spec).
    #[zbus(property)]
    fn protocol_version(&self) -> i32 {
        0
    }

    #[zbus(signal)]
    async fn status_notifier_item_registered(
        emitter: &zbus::object_server::SignalEmitter<'_>,
        service: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn status_notifier_item_unregistered(
        emitter: &zbus::object_server::SignalEmitter<'_>,
        service: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn status_notifier_host_registered(
        emitter: &zbus::object_server::SignalEmitter<'_>,
    ) -> zbus::Result<()>;
}

/// Bound on one call to an item: a hung item stalls the caller (the
/// tray's worker thread, never the shell's main loop) at most this
/// long per call.
const CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Item and menu interfaces.
const ITEM_INTERFACE: &str = "org.kde.StatusNotifierItem";
const MENU_INTERFACE: &str = "com.canonical.dbusmenu";

/// Optional item methods, from the item's introspection: the
/// extension's `supportsActivation` and `_hasAyatanaSecondaryActivate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemMethods {
    /// `Activate` (assumed when the item cannot be introspected, as the
    /// extension then tries it).
    pub activate: bool,
    /// `XAyatanaSecondaryActivate`, preferred over `SecondaryActivate`.
    pub ayatana_secondary: bool,
    /// `ProvideXdgActivationToken`.
    pub activation_token: bool,
}

impl ItemMethods {
    /// Read the methods from an item's introspection XML; `None` (no
    /// XML) assumes `Activate` only.
    pub fn from_introspection(xml: Option<&str>) -> Self {
        let Some(xml) = xml else {
            return Self {
                activate: true,
                ayatana_secondary: false,
                activation_token: false,
            };
        };
        let has = |method: &str| {
            xml.contains(&format!("name=\"{method}\"")) || xml.contains(&format!("name='{method}'"))
        };
        Self {
            activate: has("Activate"),
            ayatana_secondary: has("XAyatanaSecondaryActivate"),
            activation_token: has("ProvideXdgActivationToken"),
        }
    }
}

type Props = std::collections::HashMap<String, zbus::zvariant::OwnedValue>;

/// One property of a `GetAll` reply, `None` when absent or mistyped.
fn prop<T>(props: &Props, key: &str) -> Option<T>
where
    T: TryFrom<zbus::zvariant::Value<'static>>,
{
    let value: zbus::zvariant::Value<'static> = props.get(key)?.try_clone().ok()?.into();
    T::try_from(value).ok()
}

/// An item's `Menu` property: the spec types it as an object path
/// (`o`), some items send a plain string; libappindicator's
/// `/NO_DBUSMENU` and an empty path mean no menu.
fn menu_path_of(props: &Props) -> Option<String> {
    let path = prop::<zbus::zvariant::OwnedObjectPath>(props, "Menu")
        .map(|path| path.as_str().to_owned())
        .or_else(|| prop::<String>(props, "Menu"))?;
    (!path.is_empty() && path != "/" && path != "/NO_DBUSMENU").then_some(path)
}

/// D-Bus edge behind the host: owns the watcher role when the name
/// is free and talks to the registered items.
pub struct WatcherBus {
    conn: Option<zbus::blocking::Connection>,
    state: Arc<Mutex<WatcherState>>,
}

impl Default for WatcherBus {
    /// Disconnected edge, like [`WatcherBus::new`].
    fn default() -> Self {
        Self::new()
    }
}

impl WatcherBus {
    /// Disconnected edge; [`WatcherBus::ensure`] connects.
    pub fn new() -> Self {
        Self {
            conn: None,
            state: Arc::new(Mutex::new(WatcherState::default())),
        }
    }

    /// Connect the session bus, serve the watcher object, and take
    /// the watcher name without queueing. Returns true while we own
    /// the role. A taken name (or no bus) reads as no host: the bar
    /// keeps its tiles and nothing else changes.
    pub fn ensure(&mut self) -> bool {
        if self.conn.is_some() {
            return true;
        }
        let conn = match zbus::blocking::connection::Builder::session() {
            Ok(builder) => builder.method_timeout(CALL_TIMEOUT),
            Err(e) => {
                eprintln!("tuna-shell-host: indicator bus unavailable: {e}");
                return false;
            }
        };
        let watcher = Watcher {
            state: self.state.clone(),
        };
        let conn = match conn.serve_at(WATCHER_PATH, watcher) {
            Ok(conn) => conn,
            Err(e) => {
                eprintln!("tuna-shell-host: indicator serve failed: {e}");
                return false;
            }
        };
        let conn = match conn.build() {
            Ok(conn) => conn,
            Err(e) => {
                eprintln!("tuna-shell-host: indicator connect failed: {e}");
                return false;
            }
        };
        let reply = conn
            .request_name_with_flags(WATCHER_NAME, zbus::fdo::RequestNameFlags::DoNotQueue.into());
        match reply {
            Ok(zbus::fdo::RequestNameReply::PrimaryOwner) => {
                // Items waiting for a host (KStatusNotifierItem,
                // libayatana-appindicator) register on this signal.
                if let Ok(watcher) = conn.object_server().interface::<_, Watcher>(WATCHER_PATH) {
                    let _ = zbus::block_on(Watcher::status_notifier_host_registered(
                        watcher.signal_emitter(),
                    ));
                }
                self.conn = Some(conn);
                true
            }
            Ok(other) => {
                eprintln!("tuna-shell-host: watcher name taken ({other:?}), indicators off");
                false
            }
            Err(e) => {
                eprintln!("tuna-shell-host: watcher name request failed: {e}");
                false
            }
        }
    }

    /// Item ids registered since the edge started, oldest first.
    pub fn registered(&self) -> Vec<String> {
        self.state
            .lock()
            .map(|state| state.items.clone())
            .unwrap_or_default()
    }

    /// Call `wake` whenever the hosted items may have changed (a
    /// registration, any item's change signal — `NewIcon`, `NewStatus`,
    /// `NewTitle`... — or a registered item's bus name losing its owner)
    /// and whenever a dbusmenu changes. The caller re-reads then,
    /// instead of on a timer. The signal listeners block on their own
    /// threads for the connection's life; `wake` runs on those and on
    /// zbus's. Returns false without a connection
    /// ([`WatcherBus::ensure`] first) or when the bus refuses the match
    /// rules, so the caller keeps polling.
    pub fn watch(&self, wake: impl Fn(Change) + Send + Sync + 'static) -> bool {
        let Some(conn) = self.conn.as_ref() else {
            return false;
        };
        #[derive(Clone, Copy)]
        enum Listen {
            Items,
            Menus,
            Owners,
        }
        let rules = (|| -> zbus::Result<_> {
            let signals = |interface: &'static str| -> zbus::Result<zbus::MatchRule<'static>> {
                Ok(zbus::MatchRule::builder()
                    .msg_type(zbus::message::Type::Signal)
                    .interface(interface)?
                    .build())
            };
            let owners = zbus::MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .sender("org.freedesktop.DBus")?
                .interface("org.freedesktop.DBus")?
                .member("NameOwnerChanged")?
                .build();
            Ok([
                (
                    zbus::blocking::MessageIterator::for_match_rule(
                        signals(ITEM_INTERFACE)?,
                        conn,
                        None,
                    )?,
                    Listen::Items,
                ),
                (
                    zbus::blocking::MessageIterator::for_match_rule(
                        signals(MENU_INTERFACE)?,
                        conn,
                        None,
                    )?,
                    Listen::Menus,
                ),
                (
                    zbus::blocking::MessageIterator::for_match_rule(owners, conn, None)?,
                    Listen::Owners,
                ),
            ])
        })();
        let listeners = match rules {
            Ok(listeners) => listeners,
            Err(e) => {
                eprintln!("tuna-shell-host: indicator signals unavailable: {e}");
                return false;
            }
        };
        let wake = Wake(Arc::new(wake));
        if let Ok(mut state) = self.state.lock() {
            state.wake = Some(wake.clone());
        }
        for (messages, listen) in listeners {
            let (wake, state) = (wake.clone(), self.state.clone());
            let spawned = std::thread::Builder::new()
                .name("tuna-tray-signals".into())
                .spawn(move || {
                    for message in messages {
                        let Ok(message) = message else {
                            continue;
                        };
                        let change = match listen {
                            Listen::Items => Change::Items,
                            Listen::Menus => {
                                let header = message.header();
                                let member = header.member().map(|m| m.as_str());
                                if !matches!(
                                    member,
                                    Some("LayoutUpdated" | "ItemsPropertiesUpdated")
                                ) {
                                    continue;
                                }
                                let Some(sender) = header.sender() else {
                                    continue;
                                };
                                Change::Menu(sender.as_str().to_owned())
                            }
                            Listen::Owners => {
                                let Ok((name, _, new_owner)) =
                                    message.body().deserialize::<(String, String, String)>()
                                else {
                                    continue;
                                };
                                let items =
                                    state.lock().map(|s| s.items.clone()).unwrap_or_default();
                                if !vanished_item(&items, &name, &new_owner) {
                                    continue;
                                }
                                Change::Items
                            }
                        };
                        (wake.0)(change);
                    }
                });
            if let Err(e) = spawned {
                eprintln!("tuna-shell-host: indicator signal thread: {e}");
                return false;
            }
        }
        true
    }

    /// Call `method` on the object `path` of `service`'s bus name and
    /// read its reply; `None` on any failure.
    fn call<B, R>(
        &self,
        service: &str,
        path: &str,
        interface: &str,
        method: &str,
        body: &B,
    ) -> Option<R>
    where
        B: zbus::export::serde::Serialize + zbus::zvariant::DynamicType,
        R: for<'d> zbus::export::serde::Deserialize<'d> + zbus::zvariant::Type,
    {
        let conn = self.conn.as_ref()?;
        let (bus, _) = split_service(service);
        let reply = conn
            .call_method(Some(bus.as_str()), path, Some(interface), method, body)
            .ok()?;
        reply.body().deserialize::<R>().ok()
    }

    /// Call an item method; true when it answered without an error.
    fn call_item<B>(&self, service: &str, method: &str, body: &B) -> bool
    where
        B: zbus::export::serde::Serialize + zbus::zvariant::DynamicType,
    {
        let Some(conn) = self.conn.as_ref() else {
            return false;
        };
        let (bus, path) = split_service(service);
        conn.call_method(
            Some(bus.as_str()),
            path.as_str(),
            Some(ITEM_INTERFACE),
            method,
            body,
        )
        .is_ok()
    }

    /// Raw item properties, read in one `GetAll` as the extension's
    /// proxy does. `None` when the item vanished or its properties are
    /// unreadable.
    pub fn fetch_info(&self, service: &str) -> Option<ItemInfo> {
        let (_, path) = split_service(service);
        let props: Props = self.call(
            service,
            &path,
            "org.freedesktop.DBus.Properties",
            "GetAll",
            &(ITEM_INTERFACE,),
        )?;
        let text = |key: &str| prop::<String>(&props, key).unwrap_or_default();
        let pixmaps = |key: &str| prop::<Vec<(i32, i32, Vec<u8>)>>(&props, key).unwrap_or_default();
        let (tooltip_title, tooltip_body) = prop::<WireToolTip>(&props, "ToolTip")
            .map(|(_, _, title, body)| (title, body))
            .unwrap_or_default();
        Some(ItemInfo {
            service: service.to_owned(),
            title: Some(text("Title"))
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| service.to_owned()),
            status: text("Status"),
            icon_name: text("IconName"),
            icon_pixmap: pixmaps("IconPixmap"),
            attention_name: text("AttentionIconName"),
            attention_pixmap: pixmaps("AttentionIconPixmap"),
            icon_theme_path: text("IconThemePath"),
            label: text("XAyatanaLabel"),
            icon_accessible_desc: text("IconAccessibleDesc"),
            attention_accessible_desc: text("AttentionAccessibleDesc"),
            menu_path: menu_path_of(&props),
            tooltip_title,
            tooltip_body,
        })
    }

    /// The item's optional methods ([`ItemMethods`]).
    pub fn item_methods(&self, service: &str) -> ItemMethods {
        let (_, path) = split_service(service);
        let xml: Option<String> = self.call(
            service,
            &path,
            "org.freedesktop.DBus.Introspectable",
            "Introspect",
            &(),
        );
        ItemMethods::from_introspection(xml.as_deref())
    }

    /// Fetch one item's live properties into an [`IndicatorItem`].
    /// `None` when the item vanished or its properties unreadable.
    /// Theme names stay unresolved here: the host resolves them
    /// through its artwork cache after this returns.
    pub fn fetch_item(&self, service: &str) -> Option<IndicatorItem> {
        let info = self.fetch_info(service)?;
        let icon = info.pixmap_icon().unwrap_or_else(|| {
            if info.icon_name.is_empty() {
                IndicatorIcon::Named(info.service.clone())
            } else {
                IndicatorIcon::Named(info.icon_name.clone())
            }
        });
        Some(IndicatorItem {
            service: info.service,
            title: info.title,
            icon,
            menu: Vec::new(),
        })
    }

    /// The whole menu tree at `menu_path` (`GetLayout(0, -1, [])`: every
    /// level, every property); `None` when unreadable.
    pub fn menu_tree(&self, service: &str, menu_path: &str) -> Option<Vec<MenuNode>> {
        let conn = self.conn.as_ref()?;
        let (bus, _) = split_service(service);
        let reply = conn
            .call_method(
                Some(bus.as_str()),
                menu_path,
                Some(MENU_INTERFACE),
                "GetLayout",
                &(0i32, -1i32, Vec::<String>::new()),
            )
            .ok()?;
        // The reply is `(u(ia{sv}av))`, a revision and the layout struct
        // (not a variant): read it dynamically and hand the struct on.
        let body = reply.body();
        let top = body.deserialize::<zbus::zvariant::Structure>().ok()?;
        let layout = top.fields().get(1)?.try_to_owned().ok()?;
        Some(parse_menu_layout(&layout))
    }

    /// The legacy painter's one-level rows. Empty when the item has no
    /// menu or the layout is unreadable.
    pub fn fetch_menu(&self, service: &str) -> Vec<MenuEntry> {
        self.fetch_info(service)
            .and_then(|info| self.menu_tree(service, info.menu_path.as_deref()?))
            .map(|tree| menu_entries(&tree))
            .unwrap_or_default()
    }

    /// dbusmenu `AboutToShow(id)` before showing a menu or submenu (some
    /// items, Dropbox's among them, fill their menus only then); true
    /// when the item says the layout needs re-reading.
    pub fn about_to_show(&self, service: &str, menu_path: &str, id: i32) -> bool {
        self.call::<_, bool>(service, menu_path, MENU_INTERFACE, "AboutToShow", &(id,))
            .unwrap_or(false)
    }

    /// A dbusmenu `Event` (`clicked`, `opened`, `closed`) for row `id`.
    pub fn menu_event(&self, service: &str, menu_path: &str, id: i32, kind: &str) -> bool {
        let Some(conn) = self.conn.as_ref() else {
            return false;
        };
        let (bus, _) = split_service(service);
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as u32)
            .unwrap_or(0);
        conn.call_method(
            Some(bus.as_str()),
            menu_path,
            Some(MENU_INTERFACE),
            "Event",
            &(id, kind, zbus::zvariant::Value::I32(0), stamp),
        )
        .is_ok()
    }

    /// A row press for the legacy painter.
    pub fn fire_menu(&self, service: &str, id: i32) -> bool {
        self.fetch_info(service)
            .and_then(|info| info.menu_path)
            .is_some_and(|path| self.menu_event(service, &path, id, "clicked"))
    }

    /// `Activate(x, y)`: the item's primary action.
    pub fn activate(&self, service: &str) -> bool {
        self.call_item(service, "Activate", &(0i32, 0i32))
    }

    /// The item's secondary action, `XAyatanaSecondaryActivate(time)`
    /// when it has one, `SecondaryActivate(x, y)` otherwise
    /// (`appIndicator.js` `secondaryActivate`).
    pub fn secondary_activate(&self, service: &str, methods: ItemMethods, time: u32) -> bool {
        (methods.ayatana_secondary
            && self.call_item(service, "XAyatanaSecondaryActivate", &(time,)))
            || self.call_item(service, "SecondaryActivate", &(0i32, 0i32))
    }

    /// `Scroll(delta, orientation)` (`vertical` or `horizontal`).
    pub fn scroll(&self, service: &str, delta: i32, orientation: &str) -> bool {
        self.call_item(service, "Scroll", &(delta, orientation))
    }

    /// `ProvideXdgActivationToken(token)`, so the window the following
    /// call opens may take focus.
    pub fn provide_activation_token(&self, service: &str, token: &str) -> bool {
        self.call_item(service, "ProvideXdgActivationToken", &(token,))
    }

    /// The ids among `services` whose bus name still has an owner.
    /// Without a bus this is a no-op (nothing was ever registered).
    pub fn prune_vanished(&self, services: &[String]) -> Vec<String> {
        let Some(conn) = self.conn.as_ref() else {
            return services.to_owned();
        };
        services
            .iter()
            .filter(|service| {
                let (bus, _) = split_service(service);
                conn.call_method(
                    Some("org.freedesktop.DBus"),
                    "/org/freedesktop/DBus",
                    Some("org.freedesktop.DBus"),
                    "GetNameOwner",
                    &(bus.as_str(),),
                )
                .is_ok()
            })
            .cloned()
            .collect()
    }

    /// Unregister items that left the bus, announcing each with
    /// `StatusNotifierItemUnregistered` as the extension does.
    pub fn forget(&self, gone: &[String]) {
        let removed: Vec<String> = match self.state.lock() {
            Ok(mut state) => {
                let removed = state
                    .items
                    .iter()
                    .filter(|item| gone.contains(item))
                    .cloned()
                    .collect();
                state.items.retain(|item| !gone.contains(item));
                removed
            }
            Err(_) => return,
        };
        if removed.is_empty() {
            return;
        }
        let Some(conn) = self.conn.as_ref() else {
            return;
        };
        let Ok(watcher) = conn.object_server().interface::<_, Watcher>(WATCHER_PATH) else {
            return;
        };
        let emitter = watcher.signal_emitter();
        for service in &removed {
            let _ = zbus::block_on(Watcher::status_notifier_item_unregistered(emitter, service));
        }
        let _ = zbus::block_on(
            watcher
                .get()
                .registered_status_notifier_items_changed(emitter),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overview::{BG, BYTES_PER_PIXEL};

    fn item(service: &str, icon: IndicatorIcon) -> IndicatorItem {
        IndicatorItem {
            service: service.to_owned(),
            title: format!("{service} title"),
            icon,
            menu: Vec::new(),
        }
    }

    #[test]
    fn host_upsert_replaces_remove_drops_and_retain_prunes() {
        let mut host = IndicatorHost::new();
        assert!(host.items().is_empty());
        host.upsert(item("a.service", IndicatorIcon::Named("a".to_owned())));
        host.upsert(item("b.service", IndicatorIcon::Named("b".to_owned())));
        assert_eq!(host.items().len(), 2);
        assert_eq!(host.get("a.service").unwrap().title, "a.service title");
        // Same service replaces in place: no duplicate, new icon wins.
        host.upsert(item(
            "a.service",
            IndicatorIcon::Pixmap {
                width: 1,
                height: 1,
                argb: vec![0xff, 0xff, 0xff, 0xff],
            },
        ));
        assert_eq!(host.items().len(), 2);
        assert!(matches!(
            host.get("a.service").unwrap().icon,
            IndicatorIcon::Pixmap { .. }
        ));
        assert!(host.get("missing.service").is_none());
        // Registration order is stable.
        assert_eq!(host.items()[0].service, "a.service");
        assert_eq!(host.items()[1].service, "b.service");
        assert!(host.remove("a.service"));
        assert!(!host.remove("a.service"));
        assert_eq!(host.items().len(), 1);
        host.retain_registered(&["b.service".to_owned(), "c.service".to_owned()]);
        assert_eq!(host.items().len(), 1);
        host.retain_registered(&["gone.service".to_owned()]);
        assert!(host.items().is_empty());
    }

    #[test]
    fn split_service_defaults_to_fallback_item_path() {
        assert_eq!(
            split_service("org.example.Item"),
            ("org.example.Item".to_owned(), FALLBACK_ITEM_PATH.to_owned())
        );
        assert_eq!(
            split_service("org.example.Item/StatusNotifierItem"),
            (
                "org.example.Item".to_owned(),
                "/StatusNotifierItem".to_owned()
            )
        );
    }

    #[test]
    fn only_a_registered_item_losing_its_name_counts_as_vanished() {
        let items = vec![
            ":1.42".to_owned(),
            "org.example.Item/StatusNotifierItem".to_owned(),
        ];
        assert!(vanished_item(&items, ":1.42", ""));
        assert!(vanished_item(&items, "org.example.Item", ""));
        // A new owner is not a vanishing, and other names are not ours.
        assert!(!vanished_item(&items, "org.example.Item", ":1.50"));
        assert!(!vanished_item(&items, ":1.7", ""));
        assert!(!vanished_item(&[], ":1.42", ""));
    }

    #[test]
    fn pick_pixmap_takes_largest_valid_entry() {
        assert!(pick_pixmap(Vec::new()).is_none());
        let valid = |w: i32, h: i32| {
            (
                w,
                h,
                vec![0x11u8; w as usize * h as usize * BYTES_PER_PIXEL],
            )
        };
        // The 32x32 entry is corrupt (short bytes): skipped, not picked.
        let picked = pick_pixmap(vec![valid(8, 8), (32, 32, vec![0u8; 7]), valid(16, 16)]);
        assert_eq!(picked.map(|(w, h, _)| (w, h)), Some((16, 16)));
        assert!(pick_pixmap(vec![(0, 8, vec![]), (-1, 4, vec![1, 2, 3])]).is_none());
    }

    #[test]
    fn argb_to_shm_reorders_one_known_pixel() {
        // Opaque red on the wire (big-endian ARGB: A, R, G, B) becomes
        // B, G, R, A in a little-endian ARGB8888 buffer.
        assert_eq!(
            argb_to_shm(&[0xff, 0xcc, 0x00, 0x00]),
            vec![0x00, 0x00, 0xcc, 0xff]
        );
        assert_eq!(
            argb_to_shm(&[0xaa, 0x11, 0x22, 0x33, 0xff, 0x44, 0x55, 0x66]),
            vec![0x33, 0x22, 0x11, 0xaa, 0x66, 0x55, 0x44, 0xff]
        );
        // A trailing partial word has no full pixel and is dropped.
        assert_eq!(argb_to_shm(&[0xaa, 0x11, 0x22, 0x33, 0x00]).len(), 4);
        assert!(argb_to_shm(&[]).is_empty());
    }

    fn item_info(status: &str) -> ItemInfo {
        ItemInfo {
            service: "test.service".to_owned(),
            title: "Test".to_owned(),
            status: status.to_owned(),
            ..ItemInfo::default()
        }
    }

    #[test]
    fn registrations_resolve_like_the_appindicator_extension() {
        let sender = ":1.42";
        // libayatana-appindicator registers its object path alone.
        assert_eq!(
            parse_registration("/org/ayatana/NotificationItem/probe", sender),
            Some(Registration::At(
                sender.to_owned(),
                "/org/ayatana/NotificationItem/probe".to_owned()
            ))
        );
        // A unique name with or without a path needs no lookup.
        assert_eq!(
            parse_registration(":1.7/Item", sender),
            Some(Registration::At(":1.7".to_owned(), "/Item".to_owned()))
        );
        assert_eq!(
            parse_registration(":1.7", sender),
            Some(Registration::At(
                ":1.7".to_owned(),
                FALLBACK_ITEM_PATH.to_owned()
            ))
        );
        // The spec's own item name stays as it is.
        assert_eq!(
            parse_registration("org.freedesktop.StatusNotifierItem-4242-1", sender),
            Some(Registration::At(
                "org.freedesktop.StatusNotifierItem-4242-1".to_owned(),
                FALLBACK_ITEM_PATH.to_owned()
            ))
        );
        // Other well-known names stand for their current owner (KDE's
        // org.kde.StatusNotifierItem-PID-ID included).
        assert_eq!(
            parse_registration("org.kde.StatusNotifierItem-4242-1", sender),
            Some(Registration::Owner(
                "org.kde.StatusNotifierItem-4242-1".to_owned(),
                FALLBACK_ITEM_PATH.to_owned()
            ))
        );
        assert_eq!(
            parse_registration("org.example.App/StatusNotifierItem", sender),
            Some(Registration::Owner(
                "org.example.App".to_owned(),
                "/StatusNotifierItem".to_owned()
            ))
        );
        // Nothing a bus name or object path can be is refused.
        for bad in [
            "",
            "notaname",
            "/bad//path",
            "org.example.App/bad path",
            "/trailing/",
        ] {
            assert_eq!(parse_registration(bad, sender), None, "{bad:?}");
        }
        // A path with no usable sender is refused too.
        assert_eq!(parse_registration("/Item", ""), None);
    }

    #[test]
    fn menu_labels_drop_the_first_mnemonic_like_the_extension() {
        assert_eq!(menu_label("_Quit"), "Quit");
        assert_eq!(menu_label("Save _As"), "Save As");
        // Only the first mnemonic underscore goes.
        assert_eq!(menu_label("_A_b"), "A_b");
        // A doubled underscore is skipped over, as the extension's
        // `/_([^_])/` skips it.
        assert_eq!(menu_label("a__b"), "a_b");
        assert_eq!(menu_label("trailing_"), "trailing_");
        assert_eq!(menu_label("_"), "_");
        assert_eq!(menu_label("plain"), "plain");
        assert_eq!(menu_label("_É"), "É");
    }

    #[test]
    fn accessible_name_and_icon_follow_status_like_the_extension() {
        let mut info = item_info("Active");
        info.icon_name = "normal".to_owned();
        info.attention_name = "attention".to_owned();
        assert_eq!(info.current_icon().0, "normal");
        assert_eq!(info.accessible_name(), "Test");
        info.label = "42%".to_owned();
        assert_eq!(info.accessible_name(), "42%");
        info.icon_accessible_desc = "Battery".to_owned();
        info.attention_accessible_desc = "Battery low".to_owned();
        assert_eq!(info.accessible_name(), "Battery");
        info.status = "NeedsAttention".to_owned();
        assert_eq!(info.current_icon().0, "attention");
        assert_eq!(info.accessible_name(), "Battery low");
        // Attention without an attention icon keeps the normal one.
        info.attention_name.clear();
        assert_eq!(info.current_icon().0, "normal");
        // A pixmap-only attention icon counts.
        info.attention_pixmap = vec![(1, 1, vec![0xff; 4])];
        assert_eq!(info.current_icon(), ("", &info.attention_pixmap[..]));
        assert!(info.visible());
        info.status = "Passive".to_owned();
        assert!(!info.visible());
    }

    #[test]
    fn item_methods_come_from_introspection() {
        let ayatana = r#"<node><interface name="org.kde.StatusNotifierItem">
            <method name="Scroll"/><method name="SecondaryActivate"/>
            <method name="XAyatanaSecondaryActivate"/>
            <method name="ProvideXdgActivationToken"/></interface></node>"#;
        assert_eq!(
            ItemMethods::from_introspection(Some(ayatana)),
            ItemMethods {
                activate: false,
                ayatana_secondary: true,
                activation_token: true,
            }
        );
        let kde = "<node><interface name='org.kde.StatusNotifierItem'>\
            <method name='Activate'/><method name='SecondaryActivate'/></interface></node>";
        assert_eq!(
            ItemMethods::from_introspection(Some(kde)),
            ItemMethods {
                activate: true,
                ayatana_secondary: false,
                activation_token: false,
            }
        );
        // No introspection: try Activate, as the extension does.
        assert!(ItemMethods::from_introspection(None).activate);
    }

    /// SNI (`a(iiay)`) payload bytes: `w` x `h` words of filler.
    fn pixmap_bytes(w: i32, h: i32, seed: u8) -> Vec<u8> {
        vec![seed; w as usize * h as usize * BYTES_PER_PIXEL]
    }

    #[test]
    fn item_info_needs_attention_only_for_needs_attention() {
        assert!(item_info("NeedsAttention").needs_attention());
        for status in ["Active", "Passive", "", "needsattention", "NeedsAttention "] {
            assert!(
                !item_info(status).needs_attention(),
                "status {status:?} must not read as attention"
            );
        }
    }

    #[test]
    fn item_info_pixmap_icon_picks_largest_valid() {
        // Same selection semantics as `pick_pixmap`: corrupt and
        // empty entries lose to the largest well-formed payload.
        let small = pixmap_bytes(2, 2, 0x11);
        let large = pixmap_bytes(4, 4, 0x22);
        let mut info = item_info("Active");
        info.icon_pixmap = vec![(2, 2, small), (8, 8, vec![0u8; 7]), (4, 4, large.clone())];
        assert_eq!(
            info.pixmap_icon(),
            Some(IndicatorIcon::Pixmap {
                width: 4,
                height: 4,
                argb: argb_to_shm(&large),
            })
        );
        // No usable entry means no icon.
        info.icon_pixmap = vec![(8, 8, vec![0u8; 7])];
        assert_eq!(info.pixmap_icon(), None);
    }

    #[test]
    fn item_info_attention_pixmap_icon_none_when_empty() {
        assert_eq!(item_info("NeedsAttention").attention_pixmap_icon(), None);
        let mut info = item_info("NeedsAttention");
        info.attention_pixmap = vec![(0, 8, Vec::new())];
        assert_eq!(info.attention_pixmap_icon(), None);
    }

    /// Test-only dbusmenu layout builder: mirrors the wire shape the
    /// parser reads (root node struct with id, `a{sv}` props, and an
    /// `av` children array), built from [`zbus::zvariant::Value`].
    mod layout_fixture {
        use zbus::zvariant::{Array, Dict, OwnedValue, Signature, Str, StructureBuilder, Value};

        fn str_value(text: &'static str) -> Value<'static> {
            Value::Str(Str::from(text))
        }

        /// One `(id, props, children)` node struct value.
        pub fn node(
            id: i32,
            props: Vec<(&'static str, Value<'static>)>,
            children: Vec<Value<'static>>,
        ) -> Value<'static> {
            let mut dict = Dict::new(&Signature::Str, &Signature::Variant);
            for (key, value) in props {
                dict.append(Value::Str(Str::from(key)), Value::Value(Box::new(value)))
                    .expect("test props fit a{sv}");
            }
            let mut kids = Array::new(&Signature::Variant);
            for child in children {
                kids.append(Value::Value(Box::new(child)))
                    .expect("test children fit av");
            }
            Value::Structure(
                StructureBuilder::new()
                    .append_field(Value::I32(id))
                    .append_field(Value::Dict(dict))
                    .append_field(Value::Array(kids))
                    .build()
                    .expect("test node builds"),
            )
        }

        /// Root layout: one kept row, one disabled row, an empty
        /// label, a separator, an invisible row, a row with children
        /// but no `children-display` (shown as a plain row, as the
        /// extension does), and a submenu holding toggles, an icon, a
        /// mistyped property and a nested submenu.
        pub fn layout() -> OwnedValue {
            let grandchild = node(61, vec![("label", str_value("Sub"))], Vec::new());
            let nested = node(
                75,
                vec![
                    ("label", str_value("Deeper")),
                    ("children-display", str_value("submenu")),
                ],
                vec![node(751, vec![("label", str_value("Leaf"))], Vec::new())],
            );
            let options = node(
                7,
                vec![
                    ("label", str_value("_Options")),
                    ("children-display", str_value("submenu")),
                ],
                vec![
                    node(
                        71,
                        vec![
                            ("label", str_value("Show _Hidden")),
                            ("toggle-type", str_value("checkmark")),
                            ("toggle-state", Value::I32(1)),
                        ],
                        Vec::new(),
                    ),
                    node(
                        72,
                        vec![
                            ("label", str_value("Small")),
                            ("toggle-type", str_value("radio")),
                            ("toggle-state", Value::I32(0)),
                        ],
                        Vec::new(),
                    ),
                    node(
                        73,
                        vec![
                            ("label", str_value("Large")),
                            ("toggle-type", str_value("radio")),
                            ("toggle-state", Value::I32(1)),
                            ("icon-name", str_value("zoom-in-symbolic")),
                        ],
                        Vec::new(),
                    ),
                    // A mistyped `enabled` reads as unset (enabled).
                    node(
                        74,
                        vec![("label", str_value("Typo")), ("enabled", str_value("no"))],
                        Vec::new(),
                    ),
                    nested,
                ],
            );
            let root = node(
                0,
                Vec::new(),
                vec![
                    node(
                        1,
                        vec![
                            ("label", str_value("Open")),
                            ("enabled", Value::Bool(true)),
                            ("visible", Value::Bool(true)),
                            ("type", str_value("standard")),
                        ],
                        Vec::new(),
                    ),
                    node(
                        2,
                        vec![
                            ("label", str_value("Quit")),
                            ("enabled", Value::Bool(false)),
                        ],
                        Vec::new(),
                    ),
                    node(3, vec![("label", str_value(""))], Vec::new()),
                    node(
                        4,
                        vec![
                            ("label", str_value("sep")),
                            ("type", str_value("separator")),
                        ],
                        Vec::new(),
                    ),
                    node(
                        5,
                        vec![
                            ("label", str_value("Hidden")),
                            ("visible", Value::Bool(false)),
                        ],
                        Vec::new(),
                    ),
                    // Missing enabled/visible/type read as enabled,
                    // visible, non-separator.
                    node(6, vec![("label", str_value("More"))], vec![grandchild]),
                    options,
                ],
            );
            OwnedValue::try_from(root).expect("test layout converts")
        }
    }

    fn row(id: i32, label: &str) -> MenuNode {
        MenuNode {
            id,
            label: label.to_owned(),
            enabled: true,
            separator: false,
            toggle: Toggle::None,
            icon_name: String::new(),
            icon_data: Vec::new(),
            submenu: false,
            children: Vec::new(),
        }
    }

    #[test]
    fn menu_layout_builds_the_whole_tree_like_the_extension() {
        let tree = parse_menu_layout(&layout_fixture::layout());
        let options = MenuNode {
            submenu: true,
            children: vec![
                MenuNode {
                    toggle: Toggle::Check(true),
                    ..row(71, "Show Hidden")
                },
                MenuNode {
                    toggle: Toggle::Radio(false),
                    ..row(72, "Small")
                },
                MenuNode {
                    toggle: Toggle::Radio(true),
                    icon_name: "zoom-in-symbolic".to_owned(),
                    ..row(73, "Large")
                },
                row(74, "Typo"),
                MenuNode {
                    submenu: true,
                    children: vec![row(751, "Leaf")],
                    ..row(75, "Deeper")
                },
            ],
            ..row(7, "Options")
        };
        assert_eq!(
            tree,
            vec![
                row(1, "Open"),
                MenuNode {
                    enabled: false,
                    ..row(2, "Quit")
                },
                row(3, ""),
                MenuNode {
                    separator: true,
                    ..row(4, "sep")
                },
                // Children without `children-display: submenu` are not
                // shown (`MenuItemFactory.createItem`).
                row(6, "More"),
                options,
            ]
        );
    }

    #[test]
    fn legacy_rows_keep_labeled_top_level_entries() {
        let rows = menu_entries(&parse_menu_layout(&layout_fixture::layout()));
        let entry = |id, label: &str, enabled| MenuEntry {
            id,
            label: label.to_owned(),
            enabled,
        };
        assert_eq!(
            rows,
            vec![
                entry(1, "Open", true),
                entry(2, "Quit", false),
                entry(6, "More", true),
                entry(7, "Options", true),
            ]
        );
    }

    #[test]
    fn menu_layout_rejects_non_structures() {
        let garbage =
            zbus::zvariant::OwnedValue::try_from(zbus::zvariant::Value::U32(7)).expect("converts");
        assert!(parse_menu_layout(&garbage).is_empty());
    }

    #[test]
    fn indicator_cells_round_trip_through_hit_test() {
        let cells = indicator_cells(1000, 32, 3);
        assert_eq!(cells.len(), 3);
        for (index, cell) in cells.iter().enumerate() {
            assert_eq!((cell.w, cell.h), (INDICATOR_CELL, INDICATOR_CELL));
            let cx = cell.x + cell.w / 2;
            let cy = cell.y + cell.h / 2;
            assert_eq!(indicator_at(&cells, cx, cy), Some(index));
        }
        // Cells extend left from the right edge in order.
        assert!(cells[0].x > cells[1].x);
        assert!(indicator_at(&cells, 0, 0).is_none());
        assert!(indicator_at(&cells, cells[0].x + 1, 1000).is_none());
    }

    fn non_bg_count(pixels: &[u8]) -> usize {
        let (chunks, _) = pixels.as_chunks::<BYTES_PER_PIXEL>();
        chunks.iter().filter(|pixel| **pixel != BG).count()
    }

    #[test]
    fn paint_indicators_and_menu_leave_non_bg_pixels() {
        let named = item("named.service", IndicatorIcon::Named("test".to_owned()));
        let pixmap = item(
            "pixmap.service",
            IndicatorIcon::Pixmap {
                width: 2,
                height: 2,
                argb: vec![0xff; 2 * 2 * BYTES_PER_PIXEL],
            },
        );
        let items = [named, pixmap];
        let cells = indicator_cells(400, 32, items.len());
        let mut pixels = vec![0u8; 400 * 32 * BYTES_PER_PIXEL];
        for (i, byte) in pixels.iter_mut().enumerate() {
            *byte = BG[i % BYTES_PER_PIXEL];
        }
        paint_indicators(&mut pixels, 400, &cells, &items);
        assert!(
            non_bg_count(&pixels) > 0,
            "named and pixmap icons must paint over the strip face"
        );

        let rect = crate::popup::Rect {
            x: 8,
            y: 40,
            w: 300,
            h: 200,
        };
        let stride = 320 * BYTES_PER_PIXEL;
        let with_rows = IndicatorItem {
            menu: vec![MenuEntry {
                id: 1,
                label: "quit".to_owned(),
                enabled: true,
            }],
            ..item("menu.service", IndicatorIcon::Named("m".to_owned()))
        };
        let mut menu_pixels = vec![0u8; 320 * 260 * BYTES_PER_PIXEL];
        for (i, byte) in menu_pixels.iter_mut().enumerate() {
            *byte = BG[i % BYTES_PER_PIXEL];
        }
        paint_indicator_menu(&mut menu_pixels, stride, &rect, &with_rows);
        assert!(
            non_bg_count(&menu_pixels) > 0,
            "a labeled row must paint glyph pixels"
        );
        let empty_rows = IndicatorItem {
            menu: Vec::new(),
            ..item("menu.service", IndicatorIcon::Named("m".to_owned()))
        };
        let mut empty_pixels = vec![0u8; 320 * 260 * BYTES_PER_PIXEL];
        for (i, byte) in empty_pixels.iter_mut().enumerate() {
            *byte = BG[i % BYTES_PER_PIXEL];
        }
        paint_indicator_menu(&mut empty_pixels, stride, &rect, &empty_rows);
        assert_eq!(non_bg_count(&empty_pixels), 0);
    }

    #[test]
    fn menu_row_at_respects_top_pad_and_count() {
        let rect = crate::popup::Rect {
            x: 8,
            y: 40,
            w: 300,
            h: 200,
        };
        assert_eq!(menu_row_at(&rect, 49, 2), None);
        assert_eq!(menu_row_at(&rect, 50, 2), Some(0));
        assert_eq!(menu_row_at(&rect, 73, 2), Some(0));
        assert_eq!(menu_row_at(&rect, 74, 2), Some(1));
        assert_eq!(menu_row_at(&rect, 10_000, 2), None);
        assert_eq!(menu_row_at(&rect, 60, 0), None);
    }

    /// Live stub over a private session bus: a stub
    /// StatusNotifierItem plus its dbusmenu, served in-process, drive
    /// the real [`WatcherBus`] edge. Skips (loudly) when no bus
    /// daemon or connection is available.
    mod live_bus {
        use std::io::BufRead;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Arc, Mutex};
        use std::time::Duration;

        use super::layout_fixture;
        use super::{
            argb_to_shm, menu_entries, parse_menu_layout, IndicatorIcon, IndicatorItem,
            ItemMethods, MenuEntry, WatcherBus, WireToolTip, FALLBACK_ITEM_PATH, WATCHER_NAME,
            WATCHER_PATH,
        };

        const STUB_NAME: &str = "org.test.TunaIndicatorStub";
        const ITEM_PATH: &str = "/StatusNotifierItem";
        const MENU_PATH: &str = "/Menu";

        /// 2x2 SNI pixmap words in A,R,G,B order.
        const PIXMAP: [u8; 16] = [
            0xff, 0x10, 0x20, 0x30, 0xff, 0x40, 0x50, 0x60, 0xff, 0x70, 0x80, 0x90, 0xff, 0xa0,
            0xb0, 0xc0,
        ];

        /// 1x1 attention pixmap, distinct from the normal icon.
        const ATTENTION_PIXMAP: [u8; 4] = [0xee, 0x11, 0x22, 0x33];

        struct StubItem {
            activated: Arc<AtomicBool>,
            secondary: Arc<AtomicBool>,
            icon_name: Arc<Mutex<String>>,
            scrolled: Arc<Mutex<Vec<(i32, String)>>>,
        }

        #[zbus::interface(name = "org.kde.StatusNotifierItem")]
        impl StubItem {
            #[zbus(property)]
            fn title(&self) -> String {
                "StubIndicator".to_owned()
            }

            #[zbus(property)]
            fn status(&self) -> String {
                "NeedsAttention".to_owned()
            }

            #[zbus(property)]
            fn icon_name(&self) -> String {
                self.icon_name.lock().expect("test lock").clone()
            }

            #[zbus(property)]
            fn tool_tip(&self) -> WireToolTip {
                (
                    String::new(),
                    Vec::new(),
                    "Stub tip".to_owned(),
                    "Stub body".to_owned(),
                )
            }

            #[zbus(property)]
            fn icon_pixmap(&self) -> Vec<(i32, i32, Vec<u8>)> {
                vec![(2, 2, PIXMAP.to_vec())]
            }

            #[zbus(property)]
            fn attention_icon_name(&self) -> String {
                "stub-attention".to_owned()
            }

            #[zbus(property)]
            fn attention_icon_pixmap(&self) -> Vec<(i32, i32, Vec<u8>)> {
                vec![(1, 1, ATTENTION_PIXMAP.to_vec())]
            }

            #[zbus(property)]
            /// Typed `o`, as the StatusNotifierItem spec has it.
            fn menu(&self) -> zbus::zvariant::OwnedObjectPath {
                zbus::zvariant::OwnedObjectPath::try_from(MENU_PATH).expect("valid path")
            }

            fn activate(&self, _x: i32, _y: i32) {
                self.activated.store(true, Ordering::SeqCst);
            }

            fn secondary_activate(&self, _x: i32, _y: i32) {
                self.secondary.store(true, Ordering::SeqCst);
            }

            fn scroll(&self, delta: i32, orientation: String) {
                self.scrolled
                    .lock()
                    .expect("test lock")
                    .push((delta, orientation));
            }

            #[zbus(property, name = "XAyatanaLabel")]
            fn x_ayatana_label(&self) -> String {
                "42%".to_owned()
            }

            #[zbus(property)]
            fn icon_theme_path(&self) -> String {
                "/opt/stub/icons".to_owned()
            }
        }

        type SpecLayout = (
            i32,
            std::collections::HashMap<String, zbus::zvariant::OwnedValue>,
            Vec<zbus::zvariant::OwnedValue>,
        );

        /// The fixture's root node as the typed spec struct.
        fn spec_layout(root: zbus::zvariant::OwnedValue) -> SpecLayout {
            use zbus::zvariant::{Structure, Value};
            let value: Value<'static> = root.into();
            let root = Structure::try_from(value).expect("root struct");
            let mut fields = root.into_fields().into_iter();
            let id = i32::try_from(fields.next().expect("id")).expect("i32 id");
            let props = std::collections::HashMap::<String, zbus::zvariant::OwnedValue>::try_from(
                fields.next().expect("props"),
            )
            .expect("props dict");
            let children =
                Vec::<zbus::zvariant::OwnedValue>::try_from(fields.next().expect("children"))
                    .expect("children array");
            (id, props, children)
        }

        struct StubMenu {
            events: Arc<Mutex<Vec<(i32, String)>>>,
        }

        #[zbus::interface(name = "com.canonical.dbusmenu")]
        impl StubMenu {
            fn get_layout(
                &self,
                _parent: i32,
                _depth: i32,
                _names: Vec<String>,
            ) -> (u32, SpecLayout) {
                // The spec's reply: `(u(ia{sv}av))`, a struct, not a variant.
                (1, spec_layout(layout_fixture::layout()))
            }

            fn about_to_show(&self, _id: i32) -> bool {
                true
            }

            fn event(&self, id: i32, kind: String, _data: zbus::zvariant::OwnedValue, _stamp: u32) {
                self.events.lock().expect("test lock").push((id, kind));
            }
        }

        /// Process-global session-bus address while a live test owns
        /// the bus; restores the prior value on drop.
        struct BusAddressGuard {
            prior: Option<String>,
        }

        impl BusAddressGuard {
            fn point_at(address: &str) -> Self {
                let prior = std::env::var("DBUS_SESSION_BUS_ADDRESS").ok();
                std::env::set_var("DBUS_SESSION_BUS_ADDRESS", address);
                Self { prior }
            }
        }

        impl Drop for BusAddressGuard {
            fn drop(&mut self) {
                match &self.prior {
                    Some(value) => std::env::set_var("DBUS_SESSION_BUS_ADDRESS", value),
                    None => std::env::remove_var("DBUS_SESSION_BUS_ADDRESS"),
                }
            }
        }

        /// Private bus daemon plus its address, or `None` when no
        /// daemon is usable (the caller skips the test).
        fn private_bus() -> Option<(String, std::process::Child)> {
            let mut child = std::process::Command::new("dbus-daemon")
                .args(["--session", "--nofork", "--print-address=1"])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
                .ok()?;
            let stdout = child.stdout.take()?;
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let mut reader = std::io::BufReader::new(stdout);
                let mut line = String::new();
                let line = reader.read_line(&mut line).ok().map(|_| line);
                let _ = tx.send(line);
            });
            match rx.recv_timeout(Duration::from_secs(10)) {
                Ok(Some(line)) if !line.trim().is_empty() => Some((line.trim().to_owned(), child)),
                _ => {
                    let _ = child.kill();
                    None
                }
            }
        }

        #[test]
        fn stub_item_serves_icon_and_menu_over_private_bus() {
            let Some((address, mut daemon)) = private_bus() else {
                eprintln!("SKIP live indicator test: no usable dbus-daemon on PATH");
                return;
            };
            let _bus_address = BusAddressGuard::point_at(&address);

            let activated = Arc::new(AtomicBool::new(false));
            let secondary = Arc::new(AtomicBool::new(false));
            let stub_icon = Arc::new(Mutex::new(String::new()));
            let scrolled = Arc::new(Mutex::new(Vec::new()));
            let events = Arc::new(Mutex::new(Vec::new()));
            let mut stub: Option<zbus::blocking::Connection> = None;
            for _ in 0..50 {
                match zbus::blocking::connection::Builder::address(address.as_str()) {
                    Ok(builder) => {
                        match builder
                            .serve_at(
                                ITEM_PATH,
                                StubItem {
                                    activated: activated.clone(),
                                    secondary: secondary.clone(),
                                    icon_name: stub_icon.clone(),
                                    scrolled: scrolled.clone(),
                                },
                            )
                            .and_then(|builder| {
                                builder.serve_at(
                                    MENU_PATH,
                                    StubMenu {
                                        events: events.clone(),
                                    },
                                )
                            })
                            .and_then(|builder| builder.name(STUB_NAME))
                            .and_then(|builder| builder.build())
                        {
                            Ok(conn) => {
                                stub = Some(conn);
                                break;
                            }
                            Err(_) => std::thread::sleep(Duration::from_millis(100)),
                        }
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(100)),
                }
            }
            let Some(stub_conn) = stub else {
                let _ = daemon.kill();
                eprintln!("SKIP live indicator test: private bus refused connections");
                return;
            };

            // A well-known `name/path` registration stands for the
            // name's owner: the item's id is its unique name and path.
            let named = format!("{STUB_NAME}{ITEM_PATH}");
            let unique = stub_conn.unique_name().expect("stub has a unique name");
            let service = format!("{unique}{ITEM_PATH}");
            assert_eq!(
                super::super::split_service(&service),
                (unique.to_string(), ITEM_PATH.to_owned())
            );
            assert_eq!(ITEM_PATH, FALLBACK_ITEM_PATH);

            // The watcher's own signals, read from a second client so
            // the stub can later leave the bus.
            let observer = zbus::blocking::connection::Builder::address(address.as_str())
                .and_then(|builder| builder.build())
                .expect("observer connects");
            let watcher_signals = zbus::MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .interface(WATCHER_NAME)
                .expect("interface rule")
                .build();
            let signals =
                zbus::blocking::MessageIterator::for_match_rule(watcher_signals, &observer, None)
                    .expect("watcher signals subscribe");
            let (signal_tx, signal_rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                for message in signals.flatten() {
                    let header = message.header();
                    let member = header.member().map(|m| m.to_string()).unwrap_or_default();
                    let arg = message.body().deserialize::<(String,)>().ok().map(|a| a.0);
                    if signal_tx.send((member, arg)).is_err() {
                        return;
                    }
                }
            });
            let next_signal = || signal_rx.recv_timeout(Duration::from_secs(10)).ok();

            let mut bus = WatcherBus::new();
            let mut owned = false;
            let start = std::time::Instant::now();
            while start.elapsed() < Duration::from_secs(20) {
                if bus.ensure() {
                    owned = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            assert!(owned, "host takes the watcher role on a private bus");
            assert_eq!(
                next_signal(),
                Some(("StatusNotifierHostRegistered".to_owned(), None)),
                "taking the role announces the host"
            );

            // Honest empty state: nothing registered yet, nothing hosted.
            assert!(bus.registered().is_empty());

            // The stub registers through the served D-Bus method, as a
            // real item does: by name, again by name, and by its object
            // path alone (libayatana-appindicator's form). All three are
            // one item, announced once.
            let watcher =
                zbus::blocking::Proxy::new(&stub_conn, WATCHER_NAME, WATCHER_PATH, WATCHER_NAME)
                    .expect("watcher proxy builds");
            for registration in [named.as_str(), named.as_str(), ITEM_PATH] {
                watcher
                    .call::<&str, _, ()>("RegisterStatusNotifierItem", &(registration,))
                    .expect("register over D-Bus");
            }
            assert_eq!(bus.registered(), vec![service.clone()]);
            assert_eq!(
                next_signal(),
                Some((
                    "StatusNotifierItemRegistered".to_owned(),
                    Some(service.clone())
                ))
            );
            let watched: Vec<String> = watcher
                .get_property("RegisteredStatusNotifierItems")
                .expect("watcher items property reads");
            assert_eq!(watched, vec![service.clone()]);
            assert!(watcher
                .get_property::<bool>("IsStatusNotifierHostRegistered")
                .expect("host property reads"));
            // Unresolvable names and other hosts are refused.
            assert!(watcher
                .call::<&str, _, ()>("RegisterStatusNotifierItem", &("org.test.TunaGhost/Item",))
                .is_err());
            assert!(watcher
                .call::<&str, _, ()>("RegisterStatusNotifierItem", &("not a name",))
                .is_err());
            assert!(watcher
                .call::<&str, _, ()>("RegisterStatusNotifierHost", &("org.test.Host",))
                .is_err());
            assert_eq!(bus.registered(), vec![service.clone()]);

            let item: IndicatorItem = bus.fetch_item(&service).expect("stub item fetches");
            assert_eq!(item.service, service);
            assert_eq!(item.title, "StubIndicator");
            assert_eq!(
                item.icon,
                IndicatorIcon::Pixmap {
                    width: 2,
                    height: 2,
                    argb: argb_to_shm(&PIXMAP),
                }
            );

            let info = bus.fetch_info(&service).expect("stub info fetches");
            assert_eq!(info.service, service);
            assert_eq!(info.title, "StubIndicator");
            assert_eq!(info.status, "NeedsAttention");
            assert!(info.needs_attention());
            assert_eq!(info.icon_name, String::new());
            assert_eq!(info.icon_pixmap, vec![(2, 2, PIXMAP.to_vec())]);
            assert_eq!(info.attention_name, "stub-attention");
            assert_eq!(
                info.attention_pixmap,
                vec![(1, 1, ATTENTION_PIXMAP.to_vec())]
            );
            assert_eq!(info.tooltip_title, "Stub tip");
            assert_eq!(info.tooltip_body, "Stub body");
            assert_eq!(info.label, "42%");
            assert_eq!(info.icon_theme_path, "/opt/stub/icons");
            assert_eq!(info.menu_path.as_deref(), Some(MENU_PATH));
            assert_eq!(info.accessible_name(), "42%");
            assert_eq!(
                info.attention_pixmap_icon(),
                Some(IndicatorIcon::Pixmap {
                    width: 1,
                    height: 1,
                    argb: argb_to_shm(&ATTENTION_PIXMAP),
                })
            );

            // The whole tree, every level, and the legacy rows from it.
            assert_eq!(
                bus.menu_tree(&service, MENU_PATH),
                Some(parse_menu_layout(&layout_fixture::layout()))
            );
            let menu = bus.fetch_menu(&service);
            assert_eq!(
                menu,
                menu_entries(&parse_menu_layout(&layout_fixture::layout()))
            );
            assert_eq!(
                menu.first(),
                Some(&MenuEntry {
                    id: 1,
                    label: "Open".to_owned(),
                    enabled: true,
                })
            );
            assert!(bus.about_to_show(&service, MENU_PATH, 0));
            assert!(bus.menu_event(&service, MENU_PATH, 0, "opened"));

            let methods = bus.item_methods(&service);
            assert_eq!(
                methods,
                ItemMethods {
                    activate: true,
                    ayatana_secondary: false,
                    activation_token: false,
                }
            );
            assert!(bus.activate(&service));
            assert!(activated.load(Ordering::SeqCst));
            assert!(bus.secondary_activate(&service, methods, 0));
            assert!(secondary.load(Ordering::SeqCst));
            assert!(bus.scroll(&service, -2, "vertical"));
            assert_eq!(
                *scrolled.lock().expect("test lock"),
                vec![(-2, "vertical".to_owned())]
            );
            // Without the method the item answers an error.
            assert!(!bus.provide_activation_token(&service, "token"));
            assert!(bus.fire_menu(&service, 2));
            assert_eq!(
                *events.lock().expect("test lock"),
                vec![(0, "opened".to_owned()), (2, "clicked".to_owned())]
            );

            // Icon updates flow: the next fetch sees the new name.
            *stub_icon.lock().expect("test lock") = "stub-updated".to_owned();
            let updated = bus.fetch_info(&service).expect("updated info fetches");
            assert_eq!(updated.icon_name, "stub-updated");
            assert_eq!(updated.tooltip_title, "Stub tip");

            // Our stub owns its name; the ghost never existed.
            assert_eq!(
                bus.prune_vanished(&[service.clone(), "org.test.TunaGhost/Item".to_owned()]),
                vec![service.clone()]
            );

            // Disappearance: the stub leaving the bus prunes it from the
            // host, and forgetting announces it and leaves an honest
            // empty state. The watcher proxy clones the stub's
            // connection, so both handles must drop to disconnect.
            drop(watcher);
            drop(stub_conn);
            let start = std::time::Instant::now();
            let mut live = vec![service.clone()];
            while !live.is_empty() && start.elapsed() < Duration::from_secs(20) {
                live = bus.prune_vanished(std::slice::from_ref(&service));
                if !live.is_empty() {
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
            assert!(live.is_empty(), "released name prunes from the host");
            bus.forget(std::slice::from_ref(&service));
            assert!(
                bus.registered().is_empty(),
                "forgotten service leaves an empty host"
            );
            assert_eq!(
                next_signal(),
                Some((
                    "StatusNotifierItemUnregistered".to_owned(),
                    Some(service.clone())
                ))
            );
            drop(observer);

            let _ = daemon.kill();
        }
    }
}
