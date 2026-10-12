//! AppIndicator tray in the panel (StatusNotifier host).
//!
//! GNOME 51 needs an extension for this; Tuna Desktop ships it natively, as the
//! legacy shell does (ledger P-TR-01). The shell-host `WatcherBus`
//! owns `org.kde.StatusNotifierWatcher` and talks to items. Its calls
//! block, so it lives on a worker thread: the panel only exchanges
//! messages with it, and a hung item can never freeze the shell.

use std::sync::mpsc;
use std::time::Duration;

use gtk4 as gtk;
use gtk4::prelude::*;
use tuna_shell_host::watcher::{pick_pixmap, MenuEntry, WatcherBus};

/// How often the worker re-reads the registered items.
const POLL: Duration = Duration::from_secs(2);

/// One item as the panel draws it.
#[derive(Debug, Clone, PartialEq)]
struct Item {
    service: String,
    title: String,
    icon_name: String,
    /// Largest pixmap as big-endian ARGB32 (`width`, `height`, bytes).
    pixmap: Option<(i32, i32, Vec<u8>)>,
    /// Composed tooltip text, if the item sends any.
    tooltip: Option<String>,
}

enum ToWorker {
    Menu(String),
    Fire(String, i32),
    Activate(String),
    SecondaryActivate(String),
}

enum FromWorker {
    Items(Vec<Item>),
    Menu(String, Vec<MenuEntry>),
}

fn worker(rx: mpsc::Receiver<ToWorker>, tx: mpsc::Sender<FromWorker>) {
    let mut bus = WatcherBus::new();
    let mut last: Vec<Item> = Vec::new();
    loop {
        // Requests first, then a poll every POLL.
        match rx.recv_timeout(POLL) {
            Ok(ToWorker::Menu(service)) => {
                let menu = bus.fetch_menu(&service);
                if tx.send(FromWorker::Menu(service, menu)).is_err() {
                    return;
                }
                continue;
            }
            Ok(ToWorker::Fire(service, id)) => {
                bus.fire_menu(&service, id);
                continue;
            }
            Ok(ToWorker::Activate(service)) => {
                bus.activate(&service);
                continue;
            }
            Ok(ToWorker::SecondaryActivate(service)) => {
                bus.secondary_activate(&service);
                continue;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
        if !bus.ensure() {
            continue;
        }
        let registered = bus.registered();
        let live = bus.prune_vanished(&registered);
        let gone: Vec<String> = registered
            .iter()
            .filter(|s| !live.contains(s))
            .cloned()
            .collect();
        bus.forget(&gone);
        let items: Vec<Item> = live
            .iter()
            .filter_map(|service| bus.fetch_info(service))
            .filter(|info| info.status != "Passive")
            .map(|info| {
                let attention = info.needs_attention();
                let (name, pixmaps) = if attention && !info.attention_name.is_empty() {
                    (info.attention_name.clone(), info.attention_pixmap.clone())
                } else {
                    (info.icon_name.clone(), info.icon_pixmap.clone())
                };
                Item {
                    service: info.service.clone(),
                    title: info.title.clone(),
                    icon_name: name,
                    pixmap: pick_pixmap(pixmaps),
                    tooltip: model::tooltip_text(&info.tooltip_title, &info.tooltip_body),
                }
            })
            .collect();
        if items != last {
            last = items.clone();
            if tx.send(FromWorker::Items(items)).is_err() {
                return;
            }
        }
    }
}

fn icon_for(item: &Item) -> gtk::Image {
    if !item.icon_name.is_empty() {
        let image = gtk::Image::from_icon_name(&item.icon_name);
        if item.icon_name.starts_with('/') {
            image.set_from_file(Some(&item.icon_name));
        }
        return image;
    }
    if let Some((w, h, bytes)) = &item.pixmap {
        let texture = gtk::gdk::MemoryTexture::new(
            *w,
            *h,
            gtk::gdk::MemoryFormat::A8r8g8b8,
            &glib::Bytes::from(bytes),
            *w as usize * 4,
        );
        return gtk::Image::from_paintable(Some(&texture));
    }
    gtk::Image::from_icon_name("image-missing")
}

/// The tray box for the panel's right side. Empty (and invisible)
/// until an item registers.
pub fn tray() -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    row.set_visible(false);
    let (to_worker, worker_rx) = mpsc::channel();
    let (worker_tx, from_worker) = mpsc::channel();
    std::thread::Builder::new()
        .name("tuna-tray".into())
        .spawn(move || worker(worker_rx, worker_tx))
        .expect("spawn tray worker");

    let menus: std::rc::Rc<std::cell::RefCell<Vec<(String, gtk::Popover)>>> = Default::default();
    let row2 = row.clone();
    glib::timeout_add_local(Duration::from_millis(100), move || {
        while let Ok(message) = from_worker.try_recv() {
            match message {
                FromWorker::Items(items) => {
                    while let Some(child) = row2.first_child() {
                        row2.remove(&child);
                    }
                    menus.borrow_mut().clear();
                    for item in &items {
                        let popover = gtk::Popover::new();
                        popover.add_css_class("tuna-shell-popover");
                        popover.set_has_arrow(false);
                        let button = gtk::MenuButton::builder()
                            .child(&icon_for(item))
                            .popover(&popover)
                            .always_show_arrow(false)
                            .build();
                        button.add_css_class("panel-button");
                        let label = if item.title.is_empty() {
                            item.service.as_str()
                        } else {
                            item.title.as_str()
                        };
                        button.update_property(&[gtk::accessible::Property::Label(label)]);
                        button.set_tooltip_text(item.tooltip.as_deref());
                        // Alternate click activates the item's secondary action.
                        let secondary_click = gtk::GestureClick::new();
                        secondary_click.set_button(gtk::gdk::BUTTON_SECONDARY);
                        let (to_secondary, service_secondary) =
                            (to_worker.clone(), item.service.clone());
                        secondary_click.connect_pressed(move |_, _, _, _| {
                            let _ = to_secondary
                                .send(ToWorker::SecondaryActivate(service_secondary.clone()));
                        });
                        button.add_controller(secondary_click);
                        // Fresh rows each time the menu opens.
                        let (to, service) = (to_worker.clone(), item.service.clone());
                        popover.connect_show(move |_| {
                            let _ = to.send(ToWorker::Menu(service.clone()));
                        });
                        menus.borrow_mut().push((item.service.clone(), popover));
                        row2.append(&button);
                    }
                    row2.set_visible(!items.is_empty());
                }
                FromWorker::Menu(service, entries) => {
                    let menus = menus.borrow();
                    let Some((_, popover)) = menus.iter().find(|(s, _)| *s == service) else {
                        continue;
                    };
                    let column = gtk::Box::new(gtk::Orientation::Vertical, 2);
                    if entries.is_empty() {
                        // No menu: the item's own activation, as GNOME's
                        // AppIndicator extension does on a primary click.
                        let _ = to_worker.send(ToWorker::Activate(service.clone()));
                        popover.popdown();
                        continue;
                    }
                    for entry in entries {
                        let button = gtk::Button::with_label(&entry.label);
                        button.add_css_class("flat");
                        button.set_sensitive(entry.enabled);
                        let (to, service, popover) =
                            (to_worker.clone(), service.clone(), popover.clone());
                        button.connect_clicked(move |_| {
                            let _ = to.send(ToWorker::Fire(service.clone(), entry.id));
                            popover.popdown();
                        });
                        column.append(&button);
                    }
                    popover.set_child(Some(&column));
                }
            }
        }
        glib::ControlFlow::Continue
    });
    row
}

/// Pure StatusNotifier protocol model: registration, icons, tooltips,
/// menus, and activation with no D-Bus or GTK dependency.
///
/// The live edge (`tuna_shell_host::watcher::WatcherBus`) owns the wire
/// calls and the panel owns the widgets; this module is the contract
/// between them — every parse, diff, and lookup the host needs,
/// testable without a bus or a display.
#[allow(dead_code)] // Panel wiring lands later in #595; tests exercise this module.
pub mod model {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    // Watcher identity stays canonical in `tuna-shell-host`, which owns
    // the live bus edge.
    use tuna_shell_host::watcher::{MenuEntry, FALLBACK_ITEM_PATH};

    /// `org.kde.StatusNotifierItem`: properties plus `Activate`,
    /// `SecondaryActivate`, and `XAyatanaSecondaryActivate`.
    pub const ITEM_INTERFACE: &str = "org.kde.StatusNotifierItem";
    /// `com.canonical.dbusmenu`: `GetLayout` and `Event`.
    pub const MENU_INTERFACE: &str = "com.canonical.dbusmenu";
    /// `org.freedesktop.DBus` signal that retires items whose owner vanishes.
    pub const NAME_OWNER_CHANGED: &str = "NameOwnerChanged";

    /// Item `Status`: `Passive` items stay hidden, `NeedsAttention`
    /// items paint their attention channel.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub enum ItemStatus {
        /// Hidden from the tray.
        Passive,
        /// Painted normally.
        #[default]
        Active,
        /// Painted with the attention channel.
        NeedsAttention,
    }

    impl ItemStatus {
        /// Parse the wire string. Unknown (and empty) values read as
        /// `Active`: the panel shows items it cannot classify, matching
        /// today's `status != "Passive"` filter.
        pub fn parse(status: &str) -> Self {
            match status {
                "Passive" => Self::Passive,
                "NeedsAttention" => Self::NeedsAttention,
                _ => Self::Active,
            }
        }

        /// True while the item requests attention.
        pub fn needs_attention(&self) -> bool {
            *self == Self::NeedsAttention
        }

        /// True while the item asks to stay hidden.
        pub fn is_passive(&self) -> bool {
            *self == Self::Passive
        }
    }

    /// One `a(iiay)` pixmap: big-endian ARGB32 words on the wire.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Pixmap {
        /// Pixmap width in pixels.
        pub width: i32,
        /// Pixmap height in pixels.
        pub height: i32,
        /// `width * height` ARGB32 words, most significant byte first.
        pub argb: Vec<u8>,
    }

    impl Pixmap {
        /// Validate one wire entry; `None` when the dimensions or the
        /// byte count are unusable.
        pub fn from_wire(width: i32, height: i32, argb: Vec<u8>) -> Option<Self> {
            let pixmap = Self {
                width,
                height,
                argb,
            };
            pixmap.is_valid().then_some(pixmap)
        }

        /// Dimensions positive and bytes exactly `width * height * 4`.
        pub fn is_valid(&self) -> bool {
            self.width > 0
                && self.height > 0
                && self.argb.len() == self.width as usize * self.height as usize * 4
        }

        /// Pixel count for largest-wins selection.
        pub fn area(&self) -> i64 {
            self.width as i64 * self.height as i64
        }
    }

    /// Largest valid pixmap, if any is usable.
    pub fn pick_pixmap(pixmaps: &[Pixmap]) -> Option<&Pixmap> {
        pixmaps
            .iter()
            .filter(|pixmap| pixmap.is_valid())
            .max_by_key(|pixmap| pixmap.area())
    }

    /// One icon channel (normal or attention): a theme name plus raw pixmaps.
    #[derive(Debug, Clone, PartialEq, Eq, Default)]
    pub struct IconChannel {
        /// Icon theme name (or an absolute file path, as items in the
        /// wild send). Empty means the channel carries no name.
        pub name: String,
        /// Raw pixmap payloads, largest wins.
        pub pixmaps: Vec<Pixmap>,
    }

    impl IconChannel {
        /// Build from wire parts, dropping malformed pixmaps.
        pub fn parse(name: String, wire: Vec<(i32, i32, Vec<u8>)>) -> Self {
            Self {
                name,
                pixmaps: wire
                    .into_iter()
                    .filter_map(|(width, height, argb)| Pixmap::from_wire(width, height, argb))
                    .collect(),
            }
        }

        /// True when the channel carries neither a name nor a valid pixmap.
        pub fn is_empty(&self) -> bool {
            self.name.is_empty() && !self.pixmaps.iter().any(Pixmap::is_valid)
        }
    }

    /// The item's `ToolTip`: `(sa(iiay)ss)` on the wire — an icon name,
    /// pixmaps, a title, and body text.
    #[derive(Debug, Clone, PartialEq, Eq, Default)]
    pub struct ToolTip {
        /// Tooltip icon theme name.
        pub icon_name: String,
        /// Tooltip icon pixmaps, largest wins.
        pub pixmaps: Vec<Pixmap>,
        /// Tooltip title line.
        pub title: String,
        /// Tooltip body text.
        pub body: String,
    }

    impl ToolTip {
        /// Parse the wire struct, dropping malformed pixmaps.
        pub fn parse(
            icon_name: String,
            wire: Vec<(i32, i32, Vec<u8>)>,
            title: String,
            body: String,
        ) -> Self {
            Self {
                icon_name,
                pixmaps: wire
                    .into_iter()
                    .filter_map(|(width, height, argb)| Pixmap::from_wire(width, height, argb))
                    .collect(),
                title,
                body,
            }
        }

        /// True when every field is empty.
        pub fn is_empty(&self) -> bool {
            self.icon_name.is_empty()
                && self.pixmaps.is_empty()
                && self.title.is_empty()
                && self.body.is_empty()
        }
    }

    /// Compose the tooltip text the panel shows for an item: `None`
    /// when the item sends neither a title nor a body, the single
    /// part alone, or title over body. The icon fields stay out:
    /// GTK tooltips are text.
    pub fn tooltip_text(title: &str, body: &str) -> Option<String> {
        match (title.is_empty(), body.is_empty()) {
            (true, true) => None,
            (false, true) => Some(title.to_owned()),
            (true, false) => Some(body.to_owned()),
            (false, false) => Some(format!("{title}\n{body}")),
        }
    }

    /// A registration string split into its bus name and object path.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct ServiceRef {
        /// The item's bus name (well-known or unique).
        pub bus_name: String,
        /// The item's object path.
        pub object_path: String,
    }

    impl ServiceRef {
        /// Split `bus-name` or `bus-name/object/path`. Bare bus names
        /// (and empty paths) take the fallback item path; empty input
        /// or an empty bus name is not a service.
        pub fn parse(service: &str) -> Option<Self> {
            if service.is_empty() {
                return None;
            }
            let (bus, path) = match service.split_once('/') {
                Some((bus, rest)) if !rest.is_empty() => (bus, format!("/{rest}")),
                Some((bus, _)) => (bus, FALLBACK_ITEM_PATH.to_owned()),
                None => (service, FALLBACK_ITEM_PATH.to_owned()),
            };
            if bus.is_empty() {
                return None;
            }
            Some(Self {
                bus_name: bus.to_owned(),
                object_path: path,
            })
        }

        /// Canonical registration string: the bus name alone when the
        /// path is the fallback, else bus plus path.
        pub fn service(&self) -> String {
            if self.object_path == FALLBACK_ITEM_PATH {
                self.bus_name.clone()
            } else {
                format!("{}{}", self.bus_name, self.object_path)
            }
        }
    }

    /// Parse the item's `Menu` property: empty and `/` both mean the
    /// item exposes no menu.
    pub fn parse_menu_path(path: &str) -> Option<String> {
        if path.is_empty() || path == "/" {
            None
        } else {
            Some(path.to_owned())
        }
    }

    /// One hosted item: identity plus its latest property snapshot.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct TrayItem {
        /// Registration string (`bus name` or `bus name/path`).
        pub service: String,
        /// The item's bus name, for owner-change matching.
        pub bus_name: String,
        /// The item's object path.
        pub object_path: String,
        /// Item title (empty until the first fetch lands).
        pub title: String,
        /// Item status.
        pub status: ItemStatus,
        /// Normal icon channel.
        pub icon: IconChannel,
        /// Attention icon channel.
        pub attention: IconChannel,
        /// Tooltip snapshot.
        pub tooltip: ToolTip,
        /// dbusmenu object path, if the item exposes a menu.
        pub menu_path: Option<String>,
        /// Extra icon lookup dir from the item's `IconThemePath`.
        pub icon_theme_path: Option<String>,
        /// The item's window is its menu (`ItemIsMenu`).
        pub item_is_menu: bool,
    }

    impl TrayItem {
        /// Fresh snapshot for a registration string: active with the
        /// service as its title until the first property fetch lands.
        /// `None` when the service string is not parseable.
        pub fn new(service: &str) -> Option<Self> {
            let parsed = ServiceRef::parse(service)?;
            Some(Self {
                service: service.to_owned(),
                bus_name: parsed.bus_name,
                object_path: parsed.object_path,
                title: String::new(),
                status: ItemStatus::Active,
                icon: IconChannel::default(),
                attention: IconChannel::default(),
                tooltip: ToolTip::default(),
                menu_path: None,
                icon_theme_path: None,
                item_is_menu: false,
            })
        }

        /// Display title: the item's title, or its service when unset.
        pub fn title_or_service(&self) -> &str {
            if self.title.is_empty() {
                &self.service
            } else {
                &self.title
            }
        }
    }

    /// Outcome of a `RegisterStatusNotifierItem` call.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum RegisterOutcome {
        /// First registration: the item joined the host.
        Added,
        /// Re-registration: a no-op, as the spec requires.
        Duplicate,
        /// Unparseable service string: rejected.
        Invalid,
    }

    /// Wire-free item set: register, refresh, and retire by service.
    #[derive(Debug, Default)]
    pub struct TrayRegistry {
        items: Vec<TrayItem>,
    }

    impl TrayRegistry {
        /// Empty host.
        pub fn new() -> Self {
            Self::default()
        }

        /// Current items in registration order.
        pub fn items(&self) -> &[TrayItem] {
            &self.items
        }

        /// Item for `service`, if hosted.
        pub fn get(&self, service: &str) -> Option<&TrayItem> {
            self.items.iter().find(|item| item.service == service)
        }

        /// Register a service. Duplicates are ignored; unparseable
        /// strings are rejected without touching the host.
        pub fn register(&mut self, service: &str) -> RegisterOutcome {
            if self.items.iter().any(|item| item.service == service) {
                return RegisterOutcome::Duplicate;
            }
            match TrayItem::new(service) {
                Some(item) => {
                    self.items.push(item);
                    RegisterOutcome::Added
                }
                None => RegisterOutcome::Invalid,
            }
        }

        /// Retract the item for `service`. Returns whether one was hosted.
        pub fn unregister(&mut self, service: &str) -> bool {
            let before = self.items.len();
            self.items.retain(|item| item.service != service);
            self.items.len() != before
        }

        /// Insert or replace the snapshot for its service, in place.
        pub fn upsert(&mut self, item: TrayItem) {
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

        /// Drop services no longer registered (vanished clients).
        pub fn retain_registered(&mut self, registered: &[String]) {
            self.items.retain(|item| registered.contains(&item.service));
        }

        /// Apply an `org.freedesktop.DBus` `NameOwnerChanged` signal:
        /// services on `name` vanish when `new_owner` is empty. Returns
        /// the vanished services in registration order.
        pub fn apply_name_owner_changed(
            &mut self,
            name: &str,
            _old_owner: &str,
            new_owner: &str,
        ) -> Vec<String> {
            if !new_owner.is_empty() {
                return Vec::new();
            }
            let vanished: Vec<String> = self
                .items
                .iter()
                .filter(|item| item.bus_name == name)
                .map(|item| item.service.clone())
                .collect();
            self.items.retain(|item| item.bus_name != name);
            vanished
        }
    }

    /// Size directories probed per theme, smallest first, with
    /// `scalable` last (vector art scales into any cell).
    const THEME_SIZES: [&str; 9] = [
        "16x16", "22x22", "24x24", "32x32", "48x48", "64x64", "128x128", "256x256", "scalable",
    ];
    /// Contexts probed per size: indicator art lives in `status`,
    /// application art in `apps`.
    const THEME_CONTEXTS: [&str; 2] = ["status", "apps"];
    /// Extensions probed per directory: exact bitmaps before scalable art.
    const THEME_EXTENSIONS: [&str; 3] = ["png", "svg", "xpm"];

    /// Where icon theme lookup searches, in probe order.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct ThemeLookup {
        /// Current theme name (`hicolor` is always probed as fallback).
        pub theme: String,
        /// `<base>/icons` directories in probe order.
        pub icon_dirs: Vec<PathBuf>,
        /// Bare-name fallback directory (`/usr/share/pixmaps`).
        pub pixmaps_dir: PathBuf,
    }

    /// Build the lookup from its inputs: the item's own `IconThemePath`
    /// first, then the desktop's icon dirs, with the pixmaps fallback
    /// last. Pure over its inputs so tests pin the order; the caller
    /// reads the XDG environment.
    pub fn theme_search_dirs(
        theme: &str,
        icon_theme_path: Option<&str>,
        data_home_icons: &str,
        data_dir_icons: &[String],
        pixmaps_dir: &str,
    ) -> ThemeLookup {
        let mut icon_dirs = Vec::new();
        if let Some(extra) = icon_theme_path.filter(|path| !path.is_empty()) {
            icon_dirs.push(PathBuf::from(extra));
        }
        icon_dirs.push(PathBuf::from(data_home_icons));
        icon_dirs.extend(data_dir_icons.iter().map(PathBuf::from));
        ThemeLookup {
            theme: theme.to_owned(),
            icon_dirs,
            pixmaps_dir: PathBuf::from(pixmaps_dir),
        }
    }

    /// Candidate files for `name` in probe order: per directory the
    /// current theme then `hicolor`, sizes small to large, `status`
    /// before `apps`, `png` before `svg` before `xpm`, with the
    /// pixmaps fallback last. Names containing `/` are file paths and
    /// pass through as the single candidate.
    pub fn icon_theme_candidates(name: &str, lookup: &ThemeLookup) -> Vec<PathBuf> {
        if name.contains('/') {
            return vec![PathBuf::from(name)];
        }
        let mut themes = vec![lookup.theme.as_str()];
        if lookup.theme != "hicolor" {
            themes.push("hicolor");
        }
        let mut out = Vec::new();
        for dir in &lookup.icon_dirs {
            for theme in &themes {
                for size in THEME_SIZES {
                    for context in THEME_CONTEXTS {
                        for extension in THEME_EXTENSIONS {
                            out.push(
                                dir.join(theme)
                                    .join(size)
                                    .join(context)
                                    .join(format!("{name}.{extension}")),
                            );
                        }
                    }
                }
            }
        }
        for extension in THEME_EXTENSIONS {
            out.push(lookup.pixmaps_dir.join(format!("{name}.{extension}")));
        }
        out
    }

    /// First candidate that exists, if any. The caller supplies the
    /// filesystem probe so tests run without touching the disk.
    pub fn find_icon_path(
        name: &str,
        lookup: &ThemeLookup,
        exists: impl Fn(&Path) -> bool,
    ) -> Option<PathBuf> {
        icon_theme_candidates(name, lookup)
            .into_iter()
            .find(|candidate| exists(candidate))
    }

    /// One artwork source the panel can paint.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum IconSource {
        /// Icon theme name, resolved through [`icon_theme_candidates`].
        ThemeName(String),
        /// Image file loaded directly.
        FilePath(PathBuf),
        /// Raw pixmap bytes.
        Pixmap(Pixmap),
        /// Nothing usable: the panel paints its missing-icon image.
        Missing,
    }

    /// Append one channel's candidates: the name first (theme names
    /// scale crisply; the panel already prefers them), then the
    /// largest valid pixmap.
    fn push_channel(channel: &IconChannel, out: &mut Vec<IconSource>) {
        if !channel.name.is_empty() {
            if channel.name.contains('/') {
                out.push(IconSource::FilePath(PathBuf::from(&channel.name)));
            } else {
                out.push(IconSource::ThemeName(channel.name.clone()));
            }
        }
        if let Some(best) = pick_pixmap(&channel.pixmaps) {
            out.push(IconSource::Pixmap(best.clone()));
        }
    }

    impl TrayItem {
        /// Ordered artwork candidates: while attention is requested
        /// the attention channel leads and the normal channel backs it
        /// up; otherwise the normal channel alone. Always ends in
        /// [`IconSource::Missing`].
        pub fn icon_candidates(&self) -> Vec<IconSource> {
            let mut out = Vec::new();
            if self.status.needs_attention() && !self.attention.is_empty() {
                push_channel(&self.attention, &mut out);
            }
            push_channel(&self.icon, &mut out);
            out.push(IconSource::Missing);
            out
        }

        /// First candidate the host can show: theme names through
        /// `theme_has`, file paths through `file_exists`, pixmaps
        /// always. Falls back across channels, so an attention name
        /// missing from the theme still paints the normal pixmap
        /// rather than the missing-icon image.
        pub fn select_icon(
            &self,
            theme_has: impl Fn(&str) -> bool,
            file_exists: impl Fn(&Path) -> bool,
        ) -> IconSource {
            self.icon_candidates()
                .into_iter()
                .find(|candidate| match candidate {
                    IconSource::ThemeName(name) => theme_has(name),
                    IconSource::FilePath(path) => file_exists(path),
                    IconSource::Pixmap(_) | IconSource::Missing => true,
                })
                .unwrap_or(IconSource::Missing)
        }
    }

    /// A D-Bus method call the live edge sends for the model: pure
    /// data describing bus, path, interface, member, and arguments.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct MethodCall {
        /// Destination bus name.
        pub bus_name: String,
        /// Target object path.
        pub object_path: String,
        /// Target interface.
        pub interface: &'static str,
        /// Method name.
        pub member: &'static str,
        /// Method arguments.
        pub args: CallArgs,
    }

    /// Arguments for the activation and menu-event calls.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum CallArgs {
        /// `Activate`/`SecondaryActivate(x, y)`: pointer position.
        Position { x: i32, y: i32 },
        /// `XAyatanaSecondaryActivate(timestamp)`: event time in milliseconds.
        Timestamp { timestamp: u32 },
        /// dbusmenu `Event(id, "clicked", timestamp)`: a row press.
        MenuEvent { id: i32, timestamp: u32 },
    }

    /// Primary-click routing for an item.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum PrimaryAction {
        /// Open the item's menu.
        OpenMenu,
        /// Send `Activate` directly.
        Activate,
    }

    impl TrayItem {
        fn call(
            &self,
            interface: &'static str,
            member: &'static str,
            args: CallArgs,
        ) -> MethodCall {
            MethodCall {
                bus_name: self.bus_name.clone(),
                object_path: self.object_path.clone(),
                interface,
                member,
                args,
            }
        }

        /// `Activate(x, y)` on the item interface.
        pub fn activate_call(&self, x: i32, y: i32) -> MethodCall {
            self.call(ITEM_INTERFACE, "Activate", CallArgs::Position { x, y })
        }

        /// `SecondaryActivate(x, y)` on the item interface (middle click).
        pub fn secondary_activate_call(&self, x: i32, y: i32) -> MethodCall {
            self.call(
                ITEM_INTERFACE,
                "SecondaryActivate",
                CallArgs::Position { x, y },
            )
        }

        /// Ayatana's `XAyatanaSecondaryActivate(timestamp)` on the item interface.
        pub fn xayatana_secondary_activate_call(&self, timestamp: u32) -> MethodCall {
            self.call(
                ITEM_INTERFACE,
                "XAyatanaSecondaryActivate",
                CallArgs::Timestamp { timestamp },
            )
        }

        /// dbusmenu `Event` for a row press; `None` when the item
        /// exposes no menu path.
        pub fn menu_event_call(&self, id: i32, timestamp: u32) -> Option<MethodCall> {
            let path = self.menu_path.clone()?;
            Some(MethodCall {
                bus_name: self.bus_name.clone(),
                object_path: path,
                interface: MENU_INTERFACE,
                member: "Event",
                args: CallArgs::MenuEvent { id, timestamp },
            })
        }

        /// Primary-click routing: items with a menu (or `ItemIsMenu`)
        /// open it, menuless items activate — as the panel does today.
        pub fn primary_action(&self) -> PrimaryAction {
            if self.item_is_menu || self.menu_path.is_some() {
                PrimaryAction::OpenMenu
            } else {
                PrimaryAction::Activate
            }
        }
    }

    /// One menu change between two `GetLayout` snapshots.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum MenuDiff {
        /// A row id appeared at `index` in the new menu.
        Added { id: i32, index: usize },
        /// A row id left the menu.
        Removed { id: i32 },
        /// A row id moved from `from` to `to`.
        Moved { id: i32, from: usize, to: usize },
        /// A row id kept its place but changed label or enabled state.
        Changed { id: i32 },
    }

    /// Diff two one-level menus by row id: removals in old order,
    /// additions in new order, then per-row moves and content changes
    /// in new order. Duplicate ids read first-wins on both sides.
    pub fn diff_menu(old: &[MenuEntry], new: &[MenuEntry]) -> Vec<MenuDiff> {
        let mut old_index: HashMap<i32, usize> = HashMap::new();
        for (index, row) in old.iter().enumerate() {
            old_index.entry(row.id).or_insert(index);
        }
        let mut new_index: HashMap<i32, usize> = HashMap::new();
        for (index, row) in new.iter().enumerate() {
            new_index.entry(row.id).or_insert(index);
        }
        let mut ops = Vec::new();
        for (index, row) in old.iter().enumerate() {
            if old_index.get(&row.id) == Some(&index) && !new_index.contains_key(&row.id) {
                ops.push(MenuDiff::Removed { id: row.id });
            }
        }
        for (index, row) in new.iter().enumerate() {
            if new_index.get(&row.id) == Some(&index) && !old_index.contains_key(&row.id) {
                ops.push(MenuDiff::Added { id: row.id, index });
            }
        }
        for (index, row) in new.iter().enumerate() {
            if new_index.get(&row.id) != Some(&index) {
                continue;
            }
            if let Some(&from) = old_index.get(&row.id) {
                if from != index {
                    ops.push(MenuDiff::Moved {
                        id: row.id,
                        from,
                        to: index,
                    });
                }
                let before = &old[from];
                if before.label != row.label || before.enabled != row.enabled {
                    ops.push(MenuDiff::Changed { id: row.id });
                }
            }
        }
        ops
    }
}

#[cfg(test)]
mod tests {
    use super::model::*;
    use tuna_shell_host::watcher::{MenuEntry, FALLBACK_ITEM_PATH};

    fn row(id: i32, label: &str, enabled: bool) -> MenuEntry {
        MenuEntry {
            id,
            label: label.to_owned(),
            enabled,
        }
    }

    /// `w` x `h` wire words of filler.
    fn wire_pixmap(w: i32, h: i32, seed: u8) -> (i32, i32, Vec<u8>) {
        (w, h, vec![seed; w as usize * h as usize * 4])
    }

    fn named_item(service: &str, name: &str) -> TrayItem {
        let mut item = TrayItem::new(service).expect("test service parses");
        item.icon.name = name.to_owned();
        item
    }

    fn lookup(theme: &str) -> ThemeLookup {
        theme_search_dirs(
            theme,
            None,
            "/home/u/.local/share/icons",
            &["/usr/share/icons".to_owned()],
            "/usr/share/pixmaps",
        )
    }

    #[test]
    fn register_adds_duplicate_rejects_invalid() {
        let mut host = TrayRegistry::new();
        assert!(host.items().is_empty());
        assert_eq!(host.register("org.test.App/Item"), RegisterOutcome::Added);
        assert_eq!(
            host.register("org.test.App/Item"),
            RegisterOutcome::Duplicate
        );
        assert_eq!(host.items().len(), 1);
        assert_eq!(
            host.get("org.test.App/Item").unwrap().bus_name,
            "org.test.App"
        );
        // A second service joins; unparseable strings are rejected.
        assert_eq!(host.register("org.test.Other"), RegisterOutcome::Added);
        assert_eq!(host.register(""), RegisterOutcome::Invalid);
        assert_eq!(host.register("/Item"), RegisterOutcome::Invalid);
        assert_eq!(host.items().len(), 2);
        assert_eq!(host.items()[0].service, "org.test.App/Item");
        assert_eq!(host.items()[1].service, "org.test.Other");
    }

    #[test]
    fn unregister_retracts_only_known_services() {
        let mut host = TrayRegistry::new();
        host.register("org.test.App");
        assert!(host.unregister("org.test.App"));
        assert!(!host.unregister("org.test.App"));
        assert!(!host.unregister("org.test.Stranger"));
        assert!(host.items().is_empty());
    }

    #[test]
    fn fresh_item_defaults_active_with_service_title() {
        let item = TrayItem::new("org.test.App").expect("test service parses");
        assert_eq!(item.bus_name, "org.test.App");
        assert_eq!(item.object_path, FALLBACK_ITEM_PATH);
        assert_eq!(item.status, ItemStatus::Active);
        assert_eq!(item.title_or_service(), "org.test.App");
        let mut titled = item;
        titled.title = "App".to_owned();
        assert_eq!(titled.title_or_service(), "App");
        assert!(TrayItem::new("").is_none());
    }

    #[test]
    fn upsert_replaces_in_place_and_retain_prunes() {
        let mut host = TrayRegistry::new();
        host.register("org.test.A");
        host.register("org.test.B");
        let mut updated = named_item("org.test.A", "a-icon");
        updated.title = "A".to_owned();
        host.upsert(updated);
        assert_eq!(host.items().len(), 2);
        assert_eq!(host.items()[0].service, "org.test.A");
        assert_eq!(host.get("org.test.A").unwrap().title, "A");
        // Unknown services upsert at the tail.
        host.upsert(named_item("org.test.C", "c-icon"));
        assert_eq!(host.items()[2].service, "org.test.C");
        host.retain_registered(&["org.test.B".to_owned()]);
        assert_eq!(host.items().len(), 1);
        assert_eq!(host.items()[0].service, "org.test.B");
    }

    #[test]
    fn name_owner_changed_vanishes_owned_services() {
        let mut host = TrayRegistry::new();
        host.register("org.test.App/One");
        host.register("org.test.App/Two");
        host.register("org.test.Kept");
        let vanished = host.apply_name_owner_changed("org.test.App", ":1.7", "");
        assert_eq!(
            vanished,
            vec!["org.test.App/One".to_owned(), "org.test.App/Two".to_owned()]
        );
        assert_eq!(host.items().len(), 1);
        assert_eq!(host.items()[0].service, "org.test.Kept");
    }

    #[test]
    fn name_owner_changed_ignores_new_owner_and_strangers() {
        let mut host = TrayRegistry::new();
        host.register("org.test.App");
        // A new owner is a handover, not a loss; strangers match nothing.
        assert!(host
            .apply_name_owner_changed("org.test.App", "", ":1.9")
            .is_empty());
        assert!(host
            .apply_name_owner_changed("org.test.Stranger", ":1.1", "")
            .is_empty());
        assert_eq!(host.items().len(), 1);
    }

    #[test]
    fn diff_menu_empty_for_identical() {
        let menu = vec![row(1, "Open", true), row(2, "Quit", false)];
        assert!(diff_menu(&menu, &menu).is_empty());
        assert!(diff_menu(&[], &[]).is_empty());
    }

    #[test]
    fn diff_menu_reports_added_and_removed() {
        let old = vec![row(1, "Open", true), row(2, "Quit", true)];
        let new = vec![row(2, "Quit", true), row(3, "About", true)];
        assert_eq!(
            diff_menu(&old, &new),
            vec![
                MenuDiff::Removed { id: 1 },
                MenuDiff::Added { id: 3, index: 1 },
                MenuDiff::Moved {
                    id: 2,
                    from: 1,
                    to: 0
                },
            ]
        );
    }

    #[test]
    fn diff_menu_reports_reorder() {
        let old = vec![row(1, "A", true), row(2, "B", true), row(3, "C", true)];
        let new = vec![row(3, "C", true), row(1, "A", true), row(2, "B", true)];
        assert_eq!(
            diff_menu(&old, &new),
            vec![
                MenuDiff::Moved {
                    id: 3,
                    from: 2,
                    to: 0
                },
                MenuDiff::Moved {
                    id: 1,
                    from: 0,
                    to: 1
                },
                MenuDiff::Moved {
                    id: 2,
                    from: 1,
                    to: 2
                },
            ]
        );
    }

    #[test]
    fn diff_menu_reports_label_and_enabled_changes() {
        let old = vec![row(1, "Open", true), row(2, "Quit", true)];
        let new = vec![row(1, "Open File", true), row(2, "Quit", false)];
        assert_eq!(
            diff_menu(&old, &new),
            vec![MenuDiff::Changed { id: 1 }, MenuDiff::Changed { id: 2 }]
        );
    }

    #[test]
    fn icon_candidates_attention_leads_then_normal() {
        let mut item = named_item("org.test.App", "app");
        item.status = ItemStatus::NeedsAttention;
        item.attention = IconChannel::parse("alarm".to_owned(), vec![wire_pixmap(2, 2, 0x11)]);
        item.icon.pixmaps = vec![Pixmap::from_wire(1, 1, vec![0u8; 4]).expect("valid")];
        let candidates = item.icon_candidates();
        assert_eq!(candidates[0], IconSource::ThemeName("alarm".to_owned()));
        assert!(matches!(candidates[1], IconSource::Pixmap(_)));
        assert_eq!(candidates[2], IconSource::ThemeName("app".to_owned()));
        assert!(matches!(candidates[3], IconSource::Pixmap(_)));
        assert_eq!(*candidates.last().unwrap(), IconSource::Missing);
    }

    #[test]
    fn icon_candidates_normal_only_without_attention() {
        let mut item = named_item("org.test.App", "app");
        item.attention = IconChannel::parse("alarm".to_owned(), Vec::new());
        let candidates = item.icon_candidates();
        assert_eq!(
            candidates,
            vec![IconSource::ThemeName("app".to_owned()), IconSource::Missing,]
        );
        // Requested attention with an empty attention channel paints normal art.
        item.status = ItemStatus::NeedsAttention;
        item.attention = IconChannel::default();
        assert_eq!(
            item.icon_candidates(),
            vec![IconSource::ThemeName("app".to_owned()), IconSource::Missing,]
        );
    }

    #[test]
    fn select_icon_prefers_theme_name_then_pixmap() {
        let mut item = named_item("org.test.App", "app");
        item.icon.pixmaps = vec![Pixmap::from_wire(2, 2, vec![0x11u8; 16]).expect("valid")];
        assert_eq!(
            item.select_icon(|name| name == "app", |_| false),
            IconSource::ThemeName("app".to_owned())
        );
        // The name missing from the theme falls through to the pixmap.
        assert!(matches!(
            item.select_icon(|_| false, |_| false),
            IconSource::Pixmap(_)
        ));
    }

    #[test]
    fn select_icon_falls_back_across_channels() {
        let mut item = named_item("org.test.App", "app");
        item.status = ItemStatus::NeedsAttention;
        item.attention = IconChannel::parse("alarm".to_owned(), Vec::new());
        // Attention art missing from the theme paints the normal pixmap...
        item.icon.pixmaps = vec![Pixmap::from_wire(2, 2, vec![0x11u8; 16]).expect("valid")];
        assert!(matches!(
            item.select_icon(|_| false, |_| false),
            IconSource::Pixmap(_)
        ));
        // ...and the normal name when no pixmap backs it.
        item.icon.pixmaps.clear();
        assert_eq!(
            item.select_icon(|name| name == "app", |_| false),
            IconSource::ThemeName("app".to_owned())
        );
    }

    #[test]
    fn select_icon_absolute_path_uses_file_probe() {
        let item = named_item("org.test.App", "/opt/app/icon.png");
        assert_eq!(
            item.select_icon(|_| false, |path| path.as_os_str() == "/opt/app/icon.png"),
            IconSource::FilePath("/opt/app/icon.png".into())
        );
        assert_eq!(item.select_icon(|_| false, |_| false), IconSource::Missing);
    }

    #[test]
    fn select_icon_missing_when_channels_empty() {
        let mut item = TrayItem::new("org.test.App").expect("test service parses");
        assert_eq!(item.select_icon(|_| true, |_| true), IconSource::Missing);
        // Corrupt pixmaps never select, even alone in a channel.
        item.status = ItemStatus::NeedsAttention;
        item.attention.pixmaps = vec![Pixmap {
            width: 8,
            height: 8,
            argb: vec![0u8; 7],
        }];
        assert_eq!(item.select_icon(|_| true, |_| true), IconSource::Missing);
    }

    #[test]
    fn service_ref_parse_and_round_trip() {
        assert_eq!(
            ServiceRef::parse("org.test.App"),
            Some(ServiceRef {
                bus_name: "org.test.App".to_owned(),
                object_path: FALLBACK_ITEM_PATH.to_owned(),
            })
        );
        assert_eq!(
            ServiceRef::parse("org.test.App/Custom/Item"),
            Some(ServiceRef {
                bus_name: "org.test.App".to_owned(),
                object_path: "/Custom/Item".to_owned(),
            })
        );
        // A trailing slash carries no path: fall back, like a bare bus name.
        assert_eq!(
            ServiceRef::parse("org.test.App/"),
            Some(ServiceRef {
                bus_name: "org.test.App".to_owned(),
                object_path: FALLBACK_ITEM_PATH.to_owned(),
            })
        );
        assert_eq!(ServiceRef::parse(""), None);
        assert_eq!(ServiceRef::parse("/Item"), None);
        // Canonical strings round-trip through the parser.
        for service in ["org.test.App", "org.test.App/Custom/Item", ":1.42/Menu"] {
            let parsed = ServiceRef::parse(service).expect("test service parses");
            let again = ServiceRef::parse(&parsed.service()).expect("canonical parses");
            assert_eq!(parsed, again);
        }
    }

    #[test]
    fn status_parse_defaults_unknown_to_active() {
        assert_eq!(ItemStatus::parse("Passive"), ItemStatus::Passive);
        assert!(ItemStatus::parse("Passive").is_passive());
        assert_eq!(ItemStatus::parse("Active"), ItemStatus::Active);
        assert_eq!(
            ItemStatus::parse("NeedsAttention"),
            ItemStatus::NeedsAttention
        );
        assert!(ItemStatus::parse("NeedsAttention").needs_attention());
        for unknown in ["", "passive", "NeedsAttention ", "Blinking"] {
            assert_eq!(
                ItemStatus::parse(unknown),
                ItemStatus::Active,
                "{unknown:?} must paint normally"
            );
        }
        assert_eq!(ItemStatus::default(), ItemStatus::Active);
    }

    #[test]
    fn channel_and_tooltip_parse_drop_bad_pixmaps() {
        let channel = IconChannel::parse(
            "app".to_owned(),
            vec![wire_pixmap(2, 2, 0x11), (8, 8, vec![0u8; 7])],
        );
        assert_eq!(channel.name, "app");
        assert_eq!(channel.pixmaps.len(), 1);
        assert_eq!(
            (channel.pixmaps[0].width, channel.pixmaps[0].height),
            (2, 2)
        );
        assert!(IconChannel::default().is_empty());
        assert!(!channel.is_empty());

        let tooltip = ToolTip::parse(
            "tip".to_owned(),
            vec![(0, 8, Vec::new())],
            "Title".to_owned(),
            "Body".to_owned(),
        );
        assert!(tooltip.pixmaps.is_empty());
        assert_eq!(tooltip.title, "Title");
        assert_eq!(tooltip.body, "Body");
        assert!(!tooltip.is_empty());
        assert!(ToolTip::default().is_empty());
    }

    #[test]
    fn tooltip_text_composes_title_and_body() {
        assert_eq!(tooltip_text("", ""), None);
        assert_eq!(tooltip_text("Title", ""), Some("Title".to_owned()));
        assert_eq!(tooltip_text("", "Body"), Some("Body".to_owned()));
        assert_eq!(
            tooltip_text("Title", "Body"),
            Some("Title\nBody".to_owned())
        );
    }

    #[test]
    fn pick_pixmap_takes_largest_valid() {
        assert!(pick_pixmap(&[]).is_none());
        let small = Pixmap::from_wire(2, 2, vec![0x11u8; 16]).expect("valid");
        let large = Pixmap::from_wire(4, 4, vec![0x22u8; 64]).expect("valid");
        let corrupt = Pixmap {
            width: 32,
            height: 32,
            argb: vec![0u8; 7],
        };
        assert_eq!(
            pick_pixmap(&[small.clone(), corrupt, large.clone()]),
            Some(&large)
        );
        assert!(pick_pixmap(&[small]).is_some());
    }

    #[test]
    fn menu_path_parse_treats_root_as_none() {
        assert_eq!(parse_menu_path(""), None);
        assert_eq!(parse_menu_path("/"), None);
        assert_eq!(parse_menu_path("/Menu"), Some("/Menu".to_owned()));
    }

    #[test]
    fn activation_calls_target_item_interface() {
        let item = TrayItem::new("org.test.App/Item").expect("test service parses");
        assert_eq!(
            item.activate_call(3, 4),
            MethodCall {
                bus_name: "org.test.App".to_owned(),
                object_path: "/Item".to_owned(),
                interface: ITEM_INTERFACE,
                member: "Activate",
                args: CallArgs::Position { x: 3, y: 4 },
            }
        );
        let secondary = item.secondary_activate_call(0, 0);
        assert_eq!(secondary.member, "SecondaryActivate");
        assert_eq!(secondary.interface, ITEM_INTERFACE);
        assert_eq!(
            item.xayatana_secondary_activate_call(1234),
            MethodCall {
                bus_name: "org.test.App".to_owned(),
                object_path: "/Item".to_owned(),
                interface: ITEM_INTERFACE,
                member: "XAyatanaSecondaryActivate",
                args: CallArgs::Timestamp { timestamp: 1234 },
            }
        );
    }

    #[test]
    fn menu_event_call_needs_menu_path() {
        let mut item = TrayItem::new("org.test.App").expect("test service parses");
        assert_eq!(item.menu_event_call(2, 99), None);
        item.menu_path = parse_menu_path("/Menu");
        assert_eq!(
            item.menu_event_call(2, 99),
            Some(MethodCall {
                bus_name: "org.test.App".to_owned(),
                object_path: "/Menu".to_owned(),
                interface: MENU_INTERFACE,
                member: "Event",
                args: CallArgs::MenuEvent {
                    id: 2,
                    timestamp: 99
                },
            })
        );
    }

    #[test]
    fn primary_action_routes_menu_vs_activate() {
        let mut item = TrayItem::new("org.test.App").expect("test service parses");
        assert_eq!(item.primary_action(), PrimaryAction::Activate);
        item.menu_path = parse_menu_path("/Menu");
        assert_eq!(item.primary_action(), PrimaryAction::OpenMenu);
        item.menu_path = None;
        item.item_is_menu = true;
        assert_eq!(item.primary_action(), PrimaryAction::OpenMenu);
    }

    #[test]
    fn theme_search_dirs_orders_item_path_first() {
        let with_extra = theme_search_dirs(
            "Tuna",
            Some("/opt/app/icons"),
            "/home/u/.local/share/icons",
            &["/usr/share/icons".to_owned()],
            "/usr/share/pixmaps",
        );
        assert_eq!(with_extra.theme, "Tuna");
        assert_eq!(
            with_extra.icon_dirs,
            vec![
                std::path::PathBuf::from("/opt/app/icons"),
                std::path::PathBuf::from("/home/u/.local/share/icons"),
                std::path::PathBuf::from("/usr/share/icons"),
            ]
        );
        assert_eq!(
            with_extra.pixmaps_dir,
            std::path::PathBuf::from("/usr/share/pixmaps")
        );
        // No item path (or an empty one) leaves the desktop order alone.
        assert_eq!(lookup("Tuna").icon_dirs.len(), 2);
        let empty_extra = theme_search_dirs("Tuna", Some(""), "/a", &[], "/p");
        assert_eq!(empty_extra.icon_dirs.len(), 1);
    }

    #[test]
    fn icon_theme_candidates_order_and_hicolor_dedup() {
        let candidates = icon_theme_candidates("app", &lookup("Tuna"));
        // Per directory: the theme, then hicolor; sizes small to large;
        // `status` before `apps`; `png` before `svg` before `xpm`.
        assert_eq!(
            candidates[0],
            std::path::PathBuf::from("/home/u/.local/share/icons/Tuna/16x16/status/app.png")
        );
        assert_eq!(
            candidates[1],
            std::path::PathBuf::from("/home/u/.local/share/icons/Tuna/16x16/status/app.svg")
        );
        assert_eq!(
            candidates[3],
            std::path::PathBuf::from("/home/u/.local/share/icons/Tuna/16x16/apps/app.png")
        );
        assert!(
            candidates
                .iter()
                .any(|path| path.as_os_str() == "/usr/share/icons/hicolor/scalable/apps/app.xpm"),
            "hicolor fallback must appear in the second directory"
        );
        // Two dirs x two themes x nine sizes x two contexts x three
        // extensions, plus the three pixmaps files.
        assert_eq!(candidates.len(), 2 * 2 * 9 * 2 * 3 + 3);
        assert_eq!(
            candidates.last().unwrap().as_os_str(),
            "/usr/share/pixmaps/app.xpm"
        );
        // The hicolor theme is not probed twice.
        let hicolor = icon_theme_candidates("app", &lookup("hicolor"));
        assert_eq!(hicolor.len(), 2 * 9 * 2 * 3 + 3);
    }

    #[test]
    fn icon_theme_candidates_pass_path_names_through() {
        assert_eq!(
            icon_theme_candidates("/opt/app/icon.png", &lookup("Tuna")),
            vec![std::path::PathBuf::from("/opt/app/icon.png")]
        );
    }

    #[test]
    fn find_icon_path_returns_first_existing() {
        let available = std::collections::HashSet::from([
            "/usr/share/icons/hicolor/24x24/status/app.png",
            "/usr/share/pixmaps/app.png",
        ]);
        assert_eq!(
            find_icon_path("app", &lookup("Tuna"), |path| available
                .contains(path.to_str().unwrap_or_default())),
            Some(std::path::PathBuf::from(
                "/usr/share/icons/hicolor/24x24/status/app.png"
            ))
        );
        assert_eq!(find_icon_path("missing", &lookup("Tuna"), |_| false), None);
    }
}
