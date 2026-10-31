//! AppIndicator tray in the panel (StatusNotifier host).
//!
//! GNOME 51 ships no tray; Ubuntu and Fedora add one with the
//! AppIndicator extension (`ubuntu/gnome-shell-extension-appindicator`),
//! and this follows that extension (ledger P-TR-01): items show newest
//! first after the screen-recording indicator (`addToStatusArea(id,
//! icon, 1)`), `Passive` items hide, the attention icon replaces the
//! normal one, `XAyatanaLabel` shows beside the icon and the dbusmenu
//! is a full menu — separators, check and radio ornaments, row icons and
//! nested submenus — kept live by `LayoutUpdated` and
//! `ItemsPropertiesUpdated`, announced with `opened`/`closed` and
//! `AboutToShow` (`dbusMenu.js`, `indicatorStatusIcon.js`).
//!
//! Pointer: a primary click toggles the menu (after the double-click
//! time when the item can `Activate`, so a double click activates
//! instead); a secondary click toggles the menu; a middle click is the
//! item's secondary action; scrolling is the item's `Scroll`. Keyboard:
//! the bar's focus walk (Ctrl+Alt+Tab) reaches every item; Enter or
//! Space opens the menu, or activates an item that has none (the
//! double click's keyboard route, P-A11Y-02); arrows walk the rows;
//! Right/Left open and close a submenu; Escape closes.
//!
//! The shell-host `WatcherBus` owns `org.kde.StatusNotifierWatcher`
//! and talks to items. Its calls block, so it lives on a worker thread:
//! the panel only exchanges messages with it, and a hung item can never
//! freeze the shell. The worker re-reads the items when they change (a
//! registration, an item's change signal, a menu's change signal, an
//! item leaving the bus) and its messages wake the main loop, so neither
//! side runs on a timer once the watcher is ours.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use gtk4 as gtk;
use gtk4::prelude::*;
use tuna_shell_host::watcher::{
    pick_pixmap, split_service, Change, ItemInfo, ItemMethods, MenuNode, Toggle, WatcherBus,
};

/// While the watcher is not ours (no bus, another watcher holds the
/// name) or its change signals are unavailable, how often the worker
/// tries again and re-reads the items.
const RETRY: Duration = Duration::from_secs(2);

/// Panel status icons are 16px (`.system-status-icon`).
const ICON_SIZE: i32 = 16;

/// One item as the panel draws it.
#[derive(Debug, Clone, PartialEq)]
struct Item {
    service: String,
    accessible_name: String,
    icon_name: String,
    icon_theme_path: String,
    /// Largest pixmap as big-endian ARGB32 (`width`, `height`, bytes).
    pixmap: Option<(i32, i32, Vec<u8>)>,
    /// `XAyatanaLabel`, empty when unset.
    label: String,
    /// Composed tooltip text, if the item sends any.
    tooltip: Option<String>,
    /// The visible menu; empty without one.
    menu: Vec<MenuNode>,
    methods: ItemMethods,
}

impl Item {
    fn from_info(info: &ItemInfo, methods: ItemMethods, menu: Vec<MenuNode>) -> Self {
        let (icon_name, pixmaps) = info.current_icon();
        Self {
            service: info.service.clone(),
            accessible_name: info.accessible_name().to_owned(),
            icon_name: icon_name.to_owned(),
            icon_theme_path: info.icon_theme_path.clone(),
            pixmap: pick_pixmap(pixmaps.to_vec()),
            label: info.label.clone(),
            tooltip: tooltip_text(&info.tooltip_title, &info.tooltip_body),
            menu,
            methods,
        }
    }

    fn same_icon(&self, other: &Self) -> bool {
        self.icon_name == other.icon_name
            && self.icon_theme_path == other.icon_theme_path
            && self.pixmap == other.pixmap
    }
}

enum ToWorker {
    /// A menu row was activated.
    Fire(String, i32, Option<String>),
    /// `Activate`, with an activation token for the window it opens.
    Activate(String, Option<String>),
    /// The secondary action at this event time.
    SecondaryActivate(String, u32, Option<String>),
    /// `Scroll(delta, orientation)`.
    Scroll(String, i32, &'static str),
    /// The menu (`0`) or one of its submenus opened (true) or closed.
    Shown(String, i32, bool),
    /// The items or a menu may have changed: re-read them.
    Changed(Change),
}

/// Answer one panel request; the worker re-reads when this returns
/// true (the item asked for its menu to be read again).
fn serve(bus: &WatcherBus, worker: &mut Worker, request: ToWorker) -> bool {
    let token = |service: &str, token: Option<String>, methods: Option<ItemMethods>| {
        if let (Some(token), Some(methods)) = (token, methods) {
            if methods.activation_token {
                bus.provide_activation_token(service, &token);
            }
        }
    };
    match request {
        ToWorker::Fire(service, id, activation) => {
            token(&service, activation, worker.methods.get(&service).copied());
            if let Some(path) = worker.menu_paths.get(&service) {
                bus.menu_event(&service, path, id, "clicked");
            }
            false
        }
        ToWorker::Activate(service, activation) => {
            token(&service, activation, worker.methods.get(&service).copied());
            bus.activate(&service);
            false
        }
        ToWorker::SecondaryActivate(service, time, activation) => {
            let methods = worker.methods.get(&service).copied();
            token(&service, activation, methods);
            if let Some(methods) = methods {
                bus.secondary_activate(&service, methods, time);
            }
            false
        }
        ToWorker::Scroll(service, delta, orientation) => {
            bus.scroll(&service, delta, orientation);
            false
        }
        ToWorker::Shown(service, id, open) => {
            let Some(path) = worker.menu_paths.get(&service).cloned() else {
                return false;
            };
            // `dbusMenu.js` `_onOpenStateChanged`: the event, then
            // `AboutToShow`, whose answer may ask for a fresh layout.
            bus.menu_event(&service, &path, id, if open { "opened" } else { "closed" });
            if open && bus.about_to_show(&service, &path, id) {
                worker.stale_menus.insert(service);
                return true;
            }
            false
        }
        ToWorker::Changed(Change::Items) => true,
        ToWorker::Changed(Change::Menu(sender)) => {
            // Only items served from `sender` (or from a well-known
            // name, whose owner we cannot tell apart) re-read the menu.
            for service in worker.menus.keys() {
                let (bus_name, _) = split_service(service);
                if bus_name == sender || !bus_name.starts_with(':') {
                    worker.stale_menus.insert(service.clone());
                }
            }
            true
        }
    }
}

/// What the worker remembers between re-reads.
#[derive(Default)]
struct Worker {
    last: Vec<Item>,
    methods: HashMap<String, ItemMethods>,
    menu_paths: HashMap<String, String>,
    menus: HashMap<String, Vec<MenuNode>>,
    stale_menus: HashSet<String>,
}

/// Re-read the registered items and send them on when they changed;
/// false once the panel is gone.
fn publish(bus: &WatcherBus, worker: &mut Worker, tx: &async_channel::Sender<Vec<Item>>) -> bool {
    let registered = bus.registered();
    let live = bus.prune_vanished(&registered);
    let gone: Vec<String> = registered
        .iter()
        .filter(|s| !live.contains(s))
        .cloned()
        .collect();
    bus.forget(&gone);
    for service in &gone {
        worker.methods.remove(service);
        worker.menu_paths.remove(service);
        worker.menus.remove(service);
        worker.stale_menus.remove(service);
    }
    let mut items = Vec::new();
    for service in &live {
        let Some(info) = bus.fetch_info(service) else {
            continue;
        };
        if !info.visible() {
            continue;
        }
        let methods = *worker
            .methods
            .entry(service.clone())
            .or_insert_with(|| bus.item_methods(service));
        let menu = match &info.menu_path {
            Some(path) => {
                let fresh = worker.menu_paths.get(service) == Some(path)
                    && !worker.stale_menus.contains(service);
                match worker.menus.get(service).filter(|_| fresh) {
                    Some(menu) => menu.clone(),
                    None => {
                        let menu = bus.menu_tree(service, path).unwrap_or_default();
                        worker.menu_paths.insert(service.clone(), path.clone());
                        worker.menus.insert(service.clone(), menu.clone());
                        menu
                    }
                }
            }
            None => {
                worker.menu_paths.remove(service);
                worker.menus.remove(service);
                Vec::new()
            }
        };
        worker.stale_menus.remove(service);
        items.push(Item::from_info(&info, methods, menu));
    }
    // Each item goes in at the same place ahead of the earlier ones
    // (`addToStatusArea(id, icon, 1)`): newest first.
    items.reverse();
    if items == worker.last {
        return true;
    }
    worker.last = items.clone();
    tx.send_blocking(items).is_ok()
}

/// `wake` feeds `rx`: the watcher's change signals arrive as
/// [`ToWorker::Changed`].
fn worker(
    rx: mpsc::Receiver<ToWorker>,
    wake: mpsc::Sender<ToWorker>,
    tx: async_channel::Sender<Vec<Item>>,
) {
    let mut bus = WatcherBus::new();
    let mut state = Worker::default();
    let (mut connected, mut watching, mut stale) = (false, false, true);
    loop {
        if !connected && bus.ensure() {
            connected = true;
            let wake = wake.clone();
            watching = bus.watch(move |change| {
                let _ = wake.send(ToWorker::Changed(change));
            });
        }
        if connected && stale {
            // A burst of change signals costs one re-read; requests
            // queued meanwhile are answered first.
            while let Ok(request) = rx.try_recv() {
                serve(&bus, &mut state, request);
            }
            if !publish(&bus, &mut state, &tx) {
                return;
            }
        }
        let request = if watching {
            match rx.recv() {
                Ok(request) => request,
                Err(_) => return,
            }
        } else {
            match rx.recv_timeout(RETRY) {
                Ok(request) => request,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    stale = true;
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        };
        stale = serve(&bus, &mut state, request);
    }
}

/// Show `item`'s icon in `image`: its icon name (looked up in its
/// `IconThemePath` first, as the extension's per-item theme does, or
/// read from a file for an absolute path), else its pixmap.
fn set_icon(image: &gtk::Image, item: &Item) {
    if !item.icon_name.is_empty() {
        if item.icon_name.starts_with('/') {
            image.set_from_file(Some(&item.icon_name));
            return;
        }
        if !item.icon_theme_path.is_empty() {
            let shared = gtk::IconTheme::for_display(&image.display());
            let theme = gtk::IconTheme::new();
            theme.set_theme_name(Some(shared.theme_name().as_str()));
            let mut path = vec![std::path::PathBuf::from(&item.icon_theme_path)];
            path.extend(shared.search_path());
            let path: Vec<&std::path::Path> = path.iter().map(|p| p.as_path()).collect();
            theme.set_search_path(&path);
            if theme.has_icon(&item.icon_name) {
                let paintable = theme.lookup_icon(
                    &item.icon_name,
                    &[],
                    ICON_SIZE,
                    image.scale_factor(),
                    gtk::TextDirection::None,
                    gtk::IconLookupFlags::empty(),
                );
                image.set_paintable(Some(&paintable));
                return;
            }
        }
        image.set_icon_name(Some(&item.icon_name));
        return;
    }
    if let Some((w, h, bytes)) = &item.pixmap {
        let texture = gtk::gdk::MemoryTexture::new(
            *w,
            *h,
            gtk::gdk::MemoryFormat::A8r8g8b8,
            &glib::Bytes::from(bytes),
            *w as usize * 4,
        );
        image.set_paintable(Some(&texture));
        return;
    }
    image.set_icon_name(Some("image-missing"));
}

/// An activation token for the window `slot`'s item opens, when the
/// item takes one (`ProvideXdgActivationToken`).
fn activation_token(slot: &Slot) -> Option<String> {
    if !slot.item.borrow().methods.activation_token {
        return None;
    }
    slot.button
        .display()
        .app_launch_context()
        .startup_notify_id(None::<&gtk::gio::AppInfo>, &[])
        .map(String::from)
}

/// One item in the bar.
struct Slot {
    service: String,
    button: gtk::MenuButton,
    image: gtk::Image,
    label: gtk::Label,
    popover: gtk::Popover,
    item: RefCell<Item>,
    /// A single click's menu toggle waiting out the double-click time.
    pending: RefCell<Option<glib::SourceId>>,
    /// Submenus open in the shown menu, kept across live updates.
    open_submenus: RefCell<HashSet<i32>>,
    /// The rows of the shown menu by id, to keep focus across updates.
    rows: RefCell<HashMap<i32, gtk::Widget>>,
}

struct Tray {
    row: gtk::Box,
    slots: RefCell<Vec<Rc<Slot>>>,
    to_worker: mpsc::Sender<ToWorker>,
    on_popdown: Rc<dyn Fn()>,
    escape_guard: Rc<Cell<crate::panel_focus::EscapeGuard>>,
    on_tray_escape: Rc<dyn Fn(&gtk::MenuButton) -> glib::Propagation>,
}

impl Slot {
    fn has_menu(&self) -> bool {
        !self.item.borrow().menu.is_empty()
    }

    fn cancel_pending(&self) {
        if let Some(source) = self.pending.borrow_mut().take() {
            source.remove();
        }
    }

    fn toggle_menu(&self) {
        if self.popover.is_visible() {
            self.button.popdown();
        } else if self.has_menu() {
            self.button.popup();
        }
    }
}

impl Tray {
    fn send(&self, request: ToWorker) {
        let _ = self.to_worker.send(request);
    }

    fn activate(&self, slot: &Slot) {
        if slot.item.borrow().methods.activate {
            self.send(ToWorker::Activate(
                slot.service.clone(),
                activation_token(slot),
            ));
        }
    }

    fn slot(self: &Rc<Self>, item: &Item) -> Rc<Slot> {
        let popover = crate::transient::MenuPopover::new_popover();
        popover.add_css_class("tuna-shell-popover");
        popover.add_css_class("tuna-tray-menu");
        popover.set_has_arrow(false);
        let image = gtk::Image::new();
        image.set_pixel_size(ICON_SIZE);
        image.add_css_class("system-status-icon");
        let label = gtk::Label::new(None);
        label.set_visible(false);
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        content.append(&image);
        content.append(&label);
        let button = gtk::MenuButton::builder()
            .child(&content)
            .popover(&popover)
            .always_show_arrow(false)
            .build();
        button.add_css_class("panel-button");
        let slot = Rc::new(Slot {
            service: item.service.clone(),
            button,
            image,
            label,
            popover,
            item: RefCell::new(item.clone()),
            pending: RefCell::new(None),
            open_submenus: RefCell::new(HashSet::new()),
            rows: RefCell::new(HashMap::new()),
        });
        self.wire(&slot);
        self.show_item(&slot, item, true);
        slot
    }

    fn wire(self: &Rc<Self>, slot: &Rc<Slot>) {
        let weak_tray = Rc::downgrade(self);
        let weak_slot = Rc::downgrade(slot);
        let parts = move || Some((weak_tray.upgrade()?, weak_slot.upgrade()?));

        // Pointer, as `indicatorStatusIcon.js` `vfunc_button_press_event`.
        // Claiming every press in the capture phase keeps the button's
        // own toggle out of it.
        let clicks = gtk::GestureClick::new();
        clicks.set_button(0);
        clicks.set_propagation_phase(gtk::PropagationPhase::Capture);
        {
            let parts = parts.clone();
            clicks.connect_pressed(move |gesture, n_press, _, _| {
                let Some((tray, slot)) = parts() else {
                    return;
                };
                gesture.set_state(gtk::EventSequenceState::Claimed);
                slot.cancel_pending();
                let activates = slot.item.borrow().methods.activate;
                match gesture.current_button() {
                    gtk::gdk::BUTTON_MIDDLE => {
                        slot.button.popdown();
                        tray.send(ToWorker::SecondaryActivate(
                            slot.service.clone(),
                            gesture.current_event_time(),
                            activation_token(&slot),
                        ));
                    }
                    gtk::gdk::BUTTON_SECONDARY => slot.toggle_menu(),
                    gtk::gdk::BUTTON_PRIMARY if n_press % 2 == 0 && activates => {
                        slot.button.popdown();
                        tray.activate(&slot);
                    }
                    gtk::gdk::BUTTON_PRIMARY if slot.has_menu() && activates => {
                        // Wait out a possible double click.
                        let wait = gtk::Settings::default()
                            .map(|s| s.gtk_double_click_time())
                            .unwrap_or(400)
                            .max(0) as u64;
                        let weak = Rc::downgrade(&slot);
                        let source =
                            glib::timeout_add_local_once(Duration::from_millis(wait), move || {
                                if let Some(slot) = weak.upgrade() {
                                    slot.pending.borrow_mut().take();
                                    slot.toggle_menu();
                                }
                            });
                        *slot.pending.borrow_mut() = Some(source);
                    }
                    gtk::gdk::BUTTON_PRIMARY => slot.toggle_menu(),
                    _ => {}
                }
            });
        }
        slot.button.add_controller(clicks);

        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
        {
            let parts = parts.clone();
            scroll.connect_scroll(move |_, dx, dy| {
                let Some((tray, slot)) = parts() else {
                    return glib::Propagation::Proceed;
                };
                // `appIndicator.js` `scroll`: the floored delta per axis.
                for (delta, orientation) in [(dx, "horizontal"), (dy, "vertical")] {
                    if delta != 0.0 {
                        tray.send(ToWorker::Scroll(
                            slot.service.clone(),
                            delta.floor() as i32,
                            orientation,
                        ));
                    }
                }
                glib::Propagation::Stop
            });
        }
        slot.button.add_controller(scroll);

        // Enter/Space on an item without a menu: its activation.
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        {
            let parts = parts.clone();
            keys.connect_key_pressed(move |_, key, _, _| {
                use gtk::gdk::Key;
                let Some((tray, slot)) = parts() else {
                    return glib::Propagation::Proceed;
                };
                let enter = matches!(
                    key,
                    Key::Return | Key::KP_Enter | Key::ISO_Enter | Key::space | Key::KP_Space
                );
                if !enter || slot.has_menu() {
                    return glib::Propagation::Proceed;
                }
                tray.activate(&slot);
                glib::Propagation::Stop
            });
        }
        slot.button.add_controller(keys);

        {
            let parts = parts.clone();
            slot.popover.connect_show(move |_| {
                let Some((tray, slot)) = parts() else {
                    return;
                };
                if !slot.has_menu() {
                    // An assistive technology's press on an item without
                    // a menu: its activation, as Enter.
                    let popover = slot.popover.clone();
                    glib::idle_add_local_once(move || popover.popdown());
                    tray.activate(&slot);
                    return;
                }
                tray.send(ToWorker::Shown(slot.service.clone(), 0, true));
                let mut state = tray.escape_guard.get();
                state.note_shown();
                tray.escape_guard.set(state);
            });
        }
        {
            let parts = parts.clone();
            slot.popover.connect_closed(move |_| {
                let Some((tray, slot)) = parts() else {
                    return;
                };
                if slot.has_menu() {
                    tray.send(ToWorker::Shown(slot.service.clone(), 0, false));
                }
                let closed: Vec<i32> = slot.open_submenus.borrow_mut().drain().collect();
                for id in closed {
                    tray.send(ToWorker::Shown(slot.service.clone(), id, false));
                }
                // Closing the menu closes its submenus
                // (`_onMenuOpenStateChanged`).
                tray.fill_menu(&slot);
                (tray.on_popdown)();
            });
        }
        {
            let escape_keys = gtk::EventControllerKey::new();
            escape_keys.set_propagation_phase(gtk::PropagationPhase::Capture);
            escape_keys.connect_key_pressed(move |_, key, _, _| {
                if key != gtk::gdk::Key::Escape {
                    return glib::Propagation::Proceed;
                }
                match parts() {
                    Some((tray, slot)) => (tray.on_tray_escape)(&slot.button),
                    None => glib::Propagation::Proceed,
                }
            });
            slot.popover.add_controller(escape_keys);
        }
    }

    /// Bring `slot` up to date with `item` without replacing the button,
    /// so an open menu stays open across icon and menu updates.
    fn show_item(self: &Rc<Self>, slot: &Rc<Slot>, item: &Item, fresh: bool) {
        let previous = slot.item.replace(item.clone());
        if fresh || !previous.same_icon(item) {
            set_icon(&slot.image, item);
        }
        slot.label.set_text(&item.label);
        slot.label.set_visible(!item.label.is_empty());
        slot.button.set_tooltip_text(item.tooltip.as_deref());
        slot.button
            .update_property(&[gtk::accessible::Property::Label(&item.accessible_name)]);
        if fresh || previous.menu != item.menu {
            if item.menu.is_empty() {
                slot.button.popdown();
            }
            self.fill_menu(slot);
        }
    }

    /// (Re)build the menu rows, keeping open submenus and the focused
    /// row.
    fn fill_menu(self: &Rc<Self>, slot: &Rc<Slot>) {
        let focused = slot
            .rows
            .borrow()
            .iter()
            .find(|(_, row)| row.has_focus())
            .map(|(id, _)| *id);
        slot.rows.borrow_mut().clear();
        let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
        column.add_css_class("popup-menu-content");
        let menu = slot.item.borrow().menu.clone();
        let open: HashSet<i32> = slot.open_submenus.borrow().clone();
        self.rows(slot, &column, &menu, &open);
        // Submenus that no longer exist are closed.
        let ids = slot.rows.borrow().keys().copied().collect::<HashSet<_>>();
        slot.open_submenus
            .borrow_mut()
            .retain(|id| ids.contains(id));
        slot.popover.set_child(Some(&column));
        if let Some(row) = focused.and_then(|id| slot.rows.borrow().get(&id).cloned()) {
            row.grab_focus();
        }
    }

    fn rows(
        self: &Rc<Self>,
        slot: &Rc<Slot>,
        parent: &gtk::Box,
        nodes: &[MenuNode],
        open: &HashSet<i32>,
    ) {
        // Opening one submenu closes its open siblings
        // (`PopupMenu._setOpenedSubMenu`).
        let siblings: Rc<RefCell<Vec<(i32, gtk::Button, gtk::Revealer)>>> = Rc::default();
        for node in nodes {
            if node.separator {
                let separator = gtk::Box::new(gtk::Orientation::Vertical, 0);
                separator.add_css_class("popup-separator-menu-item");
                let line = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                line.add_css_class("popup-separator-menu-item-separator");
                line.set_valign(gtk::Align::Center);
                line.set_vexpand(true);
                separator.append(&line);
                parent.append(&separator);
                continue;
            }
            let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            let label = gtk::Label::new(Some(&node.label));
            label.set_xalign(0.0);
            label.set_hexpand(true);
            if node.submenu {
                content.append(&label);
                let arrow = gtk::Image::from_icon_name("pan-end-symbolic");
                arrow.add_css_class("popup-menu-arrow");
                content.append(&arrow);
                let header = gtk::Button::builder()
                    .child(&content)
                    .accessible_role(gtk::AccessibleRole::MenuItem)
                    .build();
                header.add_css_class("popup-menu-item");
                header.add_css_class("popup-submenu-menu-item");
                header.set_sensitive(node.enabled);
                header.update_property(&[gtk::accessible::Property::Label(&node.label)]);
                let children = gtk::Box::new(gtk::Orientation::Vertical, 0);
                children.add_css_class("popup-sub-menu");
                self.rows(slot, &children, &node.children, open);
                let revealer = gtk::Revealer::builder()
                    .child(&children)
                    .transition_type(gtk::RevealerTransitionType::SlideDown)
                    .reveal_child(open.contains(&node.id))
                    .build();
                let set_open = {
                    let (tray, slot, siblings) =
                        (Rc::downgrade(self), Rc::downgrade(slot), siblings.clone());
                    let id = node.id;
                    move |wanted: bool| {
                        let (Some(tray), Some(slot)) = (tray.upgrade(), slot.upgrade()) else {
                            return;
                        };
                        for (other, header, revealer) in siblings.borrow().iter() {
                            let is_open = revealer.reveals_child();
                            let want = wanted && *other == id;
                            if is_open == want {
                                continue;
                            }
                            revealer.set_reveal_child(want);
                            set_expanded(header, want);
                            if want {
                                slot.open_submenus.borrow_mut().insert(*other);
                            } else {
                                slot.open_submenus.borrow_mut().remove(other);
                            }
                            tray.send(ToWorker::Shown(slot.service.clone(), *other, want));
                        }
                    }
                };
                let set_open = Rc::new(set_open);
                {
                    let (set_open, revealer) = (set_open.clone(), revealer.clone());
                    header.connect_clicked(move |_| set_open(!revealer.reveals_child()));
                }
                // `PopupSubMenuMenuItem`'s binding: Right opens the
                // submenu and focuses its first row; `PopupSubMenu`:
                // Left inside it closes it and focuses the header. The
                // revealer maps its rows on the animation's first frame,
                // and only mapped rows take focus, so the focus waits for
                // that map.
                let focus_on_map = Rc::new(Cell::new(false));
                {
                    let focus_on_map = focus_on_map.clone();
                    children.connect_map(move |children| {
                        if focus_on_map.replace(false) {
                            children.child_focus(gtk::DirectionType::TabForward);
                        }
                    });
                }
                let keys = gtk::EventControllerKey::new();
                {
                    let (set_open, children) = (set_open.clone(), children.clone());
                    keys.connect_key_pressed(move |_, key, _, modifiers| {
                        // Held modifiers (not locks) leave the key alone,
                        // as `PopupMenu._onKeyPress` does.
                        let plain = !modifiers.intersects(
                            gtk::gdk::ModifierType::SHIFT_MASK
                                | gtk::gdk::ModifierType::CONTROL_MASK
                                | gtk::gdk::ModifierType::ALT_MASK
                                | gtk::gdk::ModifierType::SUPER_MASK
                                | gtk::gdk::ModifierType::META_MASK,
                        );
                        match key {
                            gtk::gdk::Key::Right | gtk::gdk::Key::KP_Right if plain => {
                                set_open(true);
                                if children.is_mapped() {
                                    children.child_focus(gtk::DirectionType::TabForward);
                                } else {
                                    focus_on_map.set(true);
                                }
                                glib::Propagation::Stop
                            }
                            _ => glib::Propagation::Proceed,
                        }
                    });
                }
                header.add_controller(keys);
                let back = gtk::EventControllerKey::new();
                {
                    let (set_open, header) = (set_open.clone(), header.clone());
                    back.connect_key_pressed(move |_, key, _, _| match key {
                        gtk::gdk::Key::Left | gtk::gdk::Key::KP_Left => {
                            set_open(false);
                            header.grab_focus();
                            glib::Propagation::Stop
                        }
                        _ => glib::Propagation::Proceed,
                    });
                }
                children.add_controller(back);
                set_expanded(&header, open.contains(&node.id));
                siblings
                    .borrow_mut()
                    .push((node.id, header.clone(), revealer.clone()));
                slot.rows
                    .borrow_mut()
                    .insert(node.id, header.clone().upcast());
                parent.append(&header);
                parent.append(&revealer);
                continue;
            }
            let ornament = gtk::Image::new();
            ornament.add_css_class("popup-menu-ornament");
            ornament.set_pixel_size(16);
            let (role, checked) = match node.toggle {
                Toggle::None => (gtk::AccessibleRole::MenuItem, None),
                Toggle::Check(on) => {
                    if on {
                        ornament.set_icon_name(Some("ornament-check-symbolic"));
                    }
                    (gtk::AccessibleRole::MenuItemCheckbox, Some(on))
                }
                Toggle::Radio(on) => {
                    if on {
                        ornament.set_icon_name(Some("ornament-dot-checked-symbolic"));
                    }
                    (gtk::AccessibleRole::MenuItemRadio, Some(on))
                }
            };
            content.append(&ornament);
            content.append(&label);
            if !node.icon_name.is_empty() {
                let icon = gtk::Image::from_icon_name(&node.icon_name);
                icon.add_css_class("popup-menu-icon");
                content.append(&icon);
            } else if !node.icon_data.is_empty() {
                if let Ok(texture) =
                    gtk::gdk::Texture::from_bytes(&glib::Bytes::from(&node.icon_data))
                {
                    let icon = gtk::Image::from_paintable(Some(&texture));
                    icon.add_css_class("popup-menu-icon");
                    content.append(&icon);
                }
            }
            let button = gtk::Button::builder()
                .child(&content)
                .accessible_role(role)
                .build();
            button.add_css_class("popup-menu-item");
            button.set_sensitive(node.enabled);
            button.update_property(&[gtk::accessible::Property::Label(&node.label)]);
            if let Some(on) = checked {
                button.update_state(&[gtk::accessible::State::Checked(if on {
                    gtk::AccessibleTristate::True
                } else {
                    gtk::AccessibleTristate::False
                })]);
            }
            {
                let (tray, slot) = (Rc::downgrade(self), Rc::downgrade(slot));
                let id = node.id;
                button.connect_clicked(move |_| {
                    let (Some(tray), Some(slot)) = (tray.upgrade(), slot.upgrade()) else {
                        return;
                    };
                    tray.send(ToWorker::Fire(
                        slot.service.clone(),
                        id,
                        activation_token(&slot),
                    ));
                    slot.button.popdown();
                });
            }
            slot.rows
                .borrow_mut()
                .insert(node.id, button.clone().upcast());
            parent.append(&button);
        }
    }

    fn update(self: &Rc<Self>, items: Vec<Item>) {
        let mut old = std::mem::take(&mut *self.slots.borrow_mut());
        let mut slots = Vec::with_capacity(items.len());
        for item in &items {
            let slot = match old.iter().position(|s| s.service == item.service) {
                Some(index) => {
                    let slot = old.remove(index);
                    self.show_item(&slot, item, false);
                    slot
                }
                None => {
                    let slot = self.slot(item);
                    self.row.append(&slot.button);
                    slot
                }
            };
            slots.push(slot);
        }
        // Items that left take their menus with them.
        for slot in old {
            slot.cancel_pending();
            slot.button.popdown();
            self.row.remove(&slot.button);
        }
        let mut previous: Option<gtk::Widget> = None;
        for slot in &slots {
            self.row
                .reorder_child_after(&slot.button, previous.as_ref());
            previous = Some(slot.button.clone().upcast());
        }
        self.row.set_visible(!slots.is_empty());
        *self.slots.borrow_mut() = slots;
    }
}

fn set_expanded(header: &gtk::Button, open: bool) {
    header.update_state(&[gtk::accessible::State::Expanded(Some(open))]);
    if open {
        header.add_css_class("open");
    } else {
        header.remove_css_class("open");
    }
}

/// The tray box for the panel's right side. Empty (and invisible)
/// until an item registers. `on_popdown` runs whenever a tray popover
/// closes, so keyboard panel focus can return to the client.
/// `escape_guard` is the shared Escape credit: a tray popover showing
/// bounds stale credit, as for the built-in menus (G-PANEL-FOCUS).
/// `on_tray_escape` runs the panel's Escape unwind for the given tray
/// button: the popover's grab delivers Escape to the popover, never
/// to the panel window controller, so each tray menu intercepts it
/// the same way the built-ins do.
pub fn tray(
    on_popdown: Rc<dyn Fn()>,
    escape_guard: Rc<Cell<crate::panel_focus::EscapeGuard>>,
    on_tray_escape: Rc<dyn Fn(&gtk::MenuButton) -> glib::Propagation>,
) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    row.set_visible(false);
    let (to_worker, worker_rx) = mpsc::channel();
    let wake = to_worker.clone();
    // The worker's messages wake the main loop (no polling).
    let (worker_tx, from_worker) = async_channel::unbounded();
    std::thread::Builder::new()
        .name("tuna-tray".into())
        .spawn(move || worker(worker_rx, wake, worker_tx))
        .expect("spawn tray worker");
    let tray = Rc::new(Tray {
        row: row.clone(),
        slots: RefCell::new(Vec::new()),
        to_worker,
        on_popdown,
        escape_guard,
        on_tray_escape,
    });
    glib::spawn_future_local(async move {
        while let Ok(items) = from_worker.recv().await {
            tray.update(items);
        }
    });
    row
}

/// Compose the text tooltip without duplicating the canonical host protocol model.
fn tooltip_text(title: &str, body: &str) -> Option<String> {
    match (title.is_empty(), body.is_empty()) {
        (true, true) => None,
        (false, true) => Some(title.to_owned()),
        (true, false) => Some(body.to_owned()),
        (false, false) => Some(format!("{title}\n{body}")),
    }
}

#[cfg(test)]
mod tests {
    use super::tooltip_text;
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
}
