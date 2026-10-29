//! GNOME 51's keyboard input-source policy (#346, ledger P-IN-06), as
//! gnome-shell 51.0 `js/ui/status/keyboard.js` `InputSourceManager` and
//! `InputSourcePopup` run it:
//!
//! - **MRU order.** Sources are kept most-recently-used first. Activating
//!   one moves it to the front; an interactive switch also saves the
//!   order to `org.gnome.desktop.input-sources mru-sources`, which seeds
//!   the order at the next start (`_updateMruSources`). When `sources`
//!   changes, the remembered order is kept for the sources that remain,
//!   new ones follow in settings order, and the front one is activated.
//! - **The switcher.** `switch-input-source` (Super+Space) opens the
//!   popup over the MRU list with the second entry selected
//!   (`switch-input-source-backward` the last); each further press steps,
//!   Left/Right step, releasing the binding's modifier picks, Escape
//!   cancels. A quick tap therefore flips between the two most recent
//!   sources. Fewer than two sources do nothing. While another shell
//!   popup holds the keyboard (or on the lock screen) it steps to the next
//!   source by position without a popup (`_modifiersSwitcher`).
//! - **Per window.** With `per-window` set, every window (and the
//!   overview, as one more window) remembers the source last active in
//!   it; focusing it activates that source again, and a window seen for
//!   the first time — or since `sources` changed — starts on the first
//!   source (`_setPerWindowInputSource`, `_getNewInputSource`). Turning
//!   the key off forgets every window's source.
//!
//! The compositor holds the sources as the layouts of one XKB keymap, in
//! settings order, and switches between them on the shell's relative
//! `SwitchInputSource` steps. This module tracks the layout the
//! compositor has active (the keymap keeps it across reloads, falling
//! back to the first when it no longer exists — `windows.rs`) and sends
//! the steps that reach the source it activates.
//!
//! Not carried over: IBus input sources take no part (the compositor's
//! keymap holds the XKB sources only, and the shell does not drive IBus
//! engines), and `mru-sources` is saved even without IBus running, where
//! gnome-shell waits for IBus first.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use gtk4 as gtk;
use gtk4::gio;
use gtk4::glib;
use gtk4::prelude::*;
use gtk4_layer_shell::{KeyboardMode, Layer, LayerShell};

/// GNOME's input-sources schema.
pub const SCHEMA: &str = "org.gnome.desktop.input-sources";
/// `SwitcherPopup`'s `POPUP_DELAY_TIMEOUT`.
pub const POPUP_DELAY: Duration = Duration::from_millis(150);
/// `SwitcherPopup`'s `NO_MODS_TIMEOUT` for bindings without a modifier.
pub const NO_MODS_TIMEOUT: Duration = Duration::from_millis(1500);

/// One input source, as `sources` lists it: `('xkb', 'de+nodeadkeys')`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Source {
    pub kind: String,
    pub id: String,
}

impl Source {
    pub fn xkb(id: &str) -> Self {
        Self {
            kind: "xkb".into(),
            id: id.into(),
        }
    }
}

/// What remembers a source with `per-window`: a window, or the overview.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Holder {
    Window(u64),
    Overview,
}

/// The sources the compositor's keymap holds: the XKB ones, in order;
/// none at all means `us` (`KeyboardManager.DEFAULT_LAYOUT`), matching
/// `logic::xkb_from_sources`.
pub fn usable_sources(list: &[(String, String)]) -> Vec<Source> {
    let sources: Vec<Source> = list
        .iter()
        .filter(|(kind, _)| kind == "xkb")
        .map(|(kind, id)| Source {
            kind: kind.clone(),
            id: id.clone(),
        })
        .collect();
    if sources.is_empty() {
        vec![Source::xkb("us")]
    } else {
        sources
    }
}

/// An activation the manager asks for: the source's position in the
/// list, and whether the user chose it (then `mru-sources` is saved).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Activation {
    pub index: usize,
    pub interactive: bool,
}

/// `InputSourceManager`'s state, without GTK.
#[derive(Debug, Default)]
pub struct Manager {
    sources: Vec<Source>,
    /// Bumped whenever `sources` changes (GNOME compares the window's
    /// `_inputSources` object with the manager's).
    generation: u64,
    mru: Vec<Source>,
    current: Option<usize>,
    per_window: bool,
    held: HashMap<Holder, (u64, Source)>,
    holder: Option<Holder>,
}

impl Manager {
    pub fn sources(&self) -> &[Source] {
        &self.sources
    }

    pub fn mru(&self) -> &[Source] {
        &self.mru
    }

    pub fn current(&self) -> Option<usize> {
        self.current
    }

    fn index_of(&self, source: &Source) -> Option<usize> {
        self.sources.iter().position(|s| s == source)
    }

    /// `_inputSourcesChanged` + `_updateMruSources`: take the new list,
    /// keep the MRU order for what remains (seeded from `saved` when no
    /// order is known yet), and activate the front source.
    pub fn set_sources(&mut self, sources: Vec<Source>, saved: &[Source]) -> Activation {
        self.sources = sources;
        self.generation += 1;
        self.current = None;
        if self.mru.is_empty() {
            self.mru = saved
                .iter()
                .filter(|s| self.sources.contains(s))
                .cloned()
                .collect();
        }
        let mut rest = self.sources.clone();
        let mut ordered = Vec::new();
        if self.mru.len() > 1 {
            for source in &self.mru {
                if let Some(i) = rest.iter().position(|s| s == source) {
                    ordered.push(rest.remove(i));
                }
            }
        }
        ordered.extend(rest);
        self.mru = ordered;
        let front = self.mru[0].clone();
        self.activate(self.index_of(&front).unwrap_or(0), false)
    }

    /// `activateInputSource` → `_currentInputSourceChanged`: the source
    /// becomes current and moves to the MRU front; with `per-window` the
    /// current window remembers it (`_changePerWindowSource`).
    pub fn activate(&mut self, index: usize, interactive: bool) -> Activation {
        let index = index.min(self.sources.len().saturating_sub(1));
        self.current = Some(index);
        if let Some(source) = self.sources.get(index).cloned() {
            self.mru.retain(|s| *s != source);
            self.mru.insert(0, source.clone());
            if self.per_window {
                if let Some(holder) = self.holder {
                    self.held.insert(holder, (self.generation, source));
                }
            }
        }
        Activation { index, interactive }
    }

    /// `_modifiersSwitcher`: the next source by position, wrapping.
    pub fn next_by_position(&mut self) -> Option<Activation> {
        if self.sources.is_empty() {
            return None;
        }
        let next = self.current.map_or(0, |c| (c + 1) % self.sources.len());
        Some(self.activate(next, true))
    }

    /// `_sourcesPerWindowChanged`: off forgets every window's source.
    pub fn set_per_window(&mut self, on: bool) {
        if self.per_window && !on {
            self.held.clear();
        }
        self.per_window = on;
    }

    pub fn per_window(&self) -> bool {
        self.per_window
    }

    /// `notify::focus-window` / overview showing and hiding →
    /// `_setPerWindowInputSource`. `None` (no window took the focus, or
    /// a shell popup holds the keyboard) keeps the current window.
    pub fn focus(&mut self, holder: Option<Holder>) -> Option<Activation> {
        let holder = holder?;
        if self.holder == Some(holder) {
            return None;
        }
        self.holder = Some(holder);
        if !self.per_window || self.sources.is_empty() {
            return None;
        }
        let remembered = self
            .held
            .get(&holder)
            .filter(|(generation, _)| *generation == self.generation)
            .map(|(_, source)| source.clone());
        let source = match remembered {
            Some(source) => source,
            // `_getNewInputSource`: the same source in the new list, else
            // the first one.
            None => self
                .held
                .get(&holder)
                .and_then(|(_, old)| self.index_of(old).map(|_| old.clone()))
                .unwrap_or_else(|| self.sources[0].clone()),
        };
        let index = self.index_of(&source).unwrap_or(0);
        Some(self.activate(index, false))
    }
}

/// The `SwitchInputSource` steps from layout `from` to `to` among `n`:
/// the shorter way round (`true` backward), as many as needed.
pub fn steps(from: usize, to: usize, n: usize) -> (bool, usize) {
    if n <= 1 || from == to {
        return (false, 0);
    }
    let forward = (to + n - from % n) % n;
    let backward = n - forward;
    if backward < forward {
        (true, backward)
    } else {
        (false, forward)
    }
}

/// The layout the compositor keeps after a keymap reload with `n`
/// layouts: the active one while it exists, else the first.
pub fn kept_layout(active: usize, n: usize) -> usize {
    if active < n {
        active
    } else {
        0
    }
}

/// `SwitcherPopup.show` → `_initialSelection`: the second entry, the
/// last going backward, the only one when alone.
pub fn initial_selection(len: usize, backward: bool) -> usize {
    if backward {
        len.saturating_sub(1)
    } else if len == 1 {
        0
    } else {
        1
    }
}

/// `primaryModifier`: the highest bit of a binding's modifier mask.
pub fn primary_modifier(mask: u32) -> u32 {
    if mask == 0 {
        0
    } else {
        1 << (31 - mask.leading_zeros())
    }
}

/// Names for the switcher, as `GnomeXkbInfo.get_layout_info` reads them
/// from xkeyboard-config's rules (`evdev.xml` and `evdev.extras.xml`):
/// the description, and the short description (a variant without one
/// takes its layout's).
#[derive(Debug, Default)]
pub struct XkbNames {
    names: HashMap<String, (String, String)>,
}

impl XkbNames {
    pub fn system() -> Self {
        let mut names = Self::default();
        for file in ["evdev.xml", "evdev.extras.xml"] {
            let path = std::path::Path::new("/usr/share/X11/xkb/rules").join(file);
            if let Ok(text) = std::fs::read_to_string(path) {
                names.add_rules(&text);
            }
        }
        names
    }

    pub fn add_rules(&mut self, text: &str) {
        let Ok(doc) = roxmltree::Document::parse(text) else {
            return;
        };
        let item = |node: roxmltree::Node| {
            let config = node.children().find(|c| c.has_tag_name("configItem"))?;
            let field = |tag: &str| {
                config
                    .children()
                    .find(|c| c.has_tag_name(tag))
                    .and_then(|c| c.text())
                    .map(|t| t.trim().to_owned())
            };
            Some((
                field("name")?,
                field("shortDescription"),
                field("description"),
            ))
        };
        for layout in doc.descendants().filter(|n| n.has_tag_name("layout")) {
            let Some((name, short, description)) = item(layout) else {
                continue;
            };
            let short = short.unwrap_or_else(|| name.clone());
            let description = description.unwrap_or_else(|| name.clone());
            for variant in layout.descendants().filter(|n| n.has_tag_name("variant")) {
                let Some((vname, vshort, vdescription)) = item(variant) else {
                    continue;
                };
                self.names.entry(format!("{name}+{vname}")).or_insert((
                    vdescription.unwrap_or_else(|| format!("{name}+{vname}")),
                    vshort.unwrap_or_else(|| short.clone()),
                ));
            }
            self.names.entry(name).or_insert((description, short));
        }
    }

    /// `(display name, short name)` for an XKB source id.
    pub fn lookup(&self, id: &str) -> (String, String) {
        self.names
            .get(id)
            .cloned()
            .unwrap_or_else(|| (id.to_owned(), id.split('+').next().unwrap_or(id).to_owned()))
    }
}

/// Short names made unique as `_inputSourcesChanged` does: a name two
/// sources share gets a subscript digit per source (`de₁`, `de₂`).
pub fn unique_short_names(shorts: &[String]) -> Vec<String> {
    shorts
        .iter()
        .enumerate()
        .map(|(i, short)| {
            let same: Vec<usize> = shorts
                .iter()
                .enumerate()
                .filter(|(_, s)| *s == short)
                .map(|(j, _)| j)
                .collect();
            if same.len() > 1 {
                let sub = same.iter().position(|j| *j == i).unwrap_or(0) + 1;
                format!(
                    "{short}{}",
                    char::from_u32(0x2080 + sub as u32).unwrap_or('?')
                )
            } else {
                short.clone()
            }
        })
        .collect()
}

/// The open switcher popup.
struct Popup {
    window: gtk::ApplicationWindow,
    row: gtk::Box,
    items: RefCell<Vec<(usize, gtk::Box)>>,
    selected: Cell<usize>,
    mask: Cell<gtk::gdk::ModifierType>,
    open: Cell<bool>,
    /// Pending timers; each clears its own slot when it fires, so a
    /// close never removes a source that already ran.
    delay: Rc<RefCell<Option<glib::SourceId>>>,
    no_mods: Rc<RefCell<Option<glib::SourceId>>>,
}

/// The shell's input-source manager: GNOME's policy over the compositor.
pub struct InputSources {
    manager: RefCell<Manager>,
    /// The layout the compositor has active, as this shell drove it.
    layout: Cell<usize>,
    /// The keymap last sent (layouts, variants, options) and its size.
    keymap: RefCell<Option<(String, String, String)>>,
    layouts: Cell<usize>,
    settings: Option<gio::Settings>,
    names: XkbNames,
    step: Rc<dyn Fn(bool)>,
    popup: Popup,
}

fn schema() -> Option<gio::Settings> {
    gio::SettingsSchemaSource::default()?.lookup(SCHEMA, true)?;
    Some(gio::Settings::new(SCHEMA))
}

fn source_list(settings: &gio::Settings, key: &str) -> Vec<(String, String)> {
    settings
        .settings_schema()
        .filter(|s| s.has_key(key))
        .and_then(|_| settings.value(key).get::<Vec<(String, String)>>())
        .unwrap_or_default()
}

impl InputSources {
    /// `step(backward)` sends one `SwitchInputSource` to the compositor.
    pub fn new(app: &gtk::Application, step: Rc<dyn Fn(bool)>) -> Rc<Self> {
        let window = gtk::ApplicationWindow::new(app);
        window.add_css_class("tuna-switcher");
        window.add_css_class("tuna-input-source-switcher");
        window.init_layer_shell();
        window.set_layer(Layer::Overlay);
        window.set_namespace(Some("tuna-input-source-switcher"));
        // GNOME's popup is modal: it holds the keyboard to see the
        // binding's modifier released.
        window.set_keyboard_mode(KeyboardMode::Exclusive);
        window.set_exclusive_zone(-1);
        window.set_title(Some("Switch Input Source"));
        let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        list.add_css_class("switcher-list");
        list.add_css_class("input-source-switcher");
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.set_halign(gtk::Align::Center);
        list.append(&row);
        window.set_child(Some(&list));
        let ui = Rc::new(Self {
            manager: RefCell::new(Manager::default()),
            layout: Cell::new(0),
            keymap: RefCell::new(None),
            layouts: Cell::new(1),
            settings: schema(),
            names: XkbNames::system(),
            step,
            popup: Popup {
                window,
                row,
                items: RefCell::new(Vec::new()),
                selected: Cell::new(0),
                mask: Cell::new(gtk::gdk::ModifierType::empty()),
                open: Cell::new(false),
                delay: Rc::new(RefCell::new(None)),
                no_mods: Rc::new(RefCell::new(None)),
            },
        });
        let keys = gtk::EventControllerKey::new();
        {
            let weak = Rc::downgrade(&ui);
            keys.connect_key_pressed(move |_, key, _, _| {
                let Some(ui) = weak.upgrade() else {
                    return glib::Propagation::Proceed;
                };
                use gtk::gdk::Key;
                match key {
                    Key::Left | Key::KP_Left => ui.popup_step(true),
                    Key::Right | Key::KP_Right => ui.popup_step(false),
                    Key::Escape | Key::Tab => ui.popup_close(None),
                    Key::space | Key::Return | Key::KP_Enter | Key::ISO_Enter => ui.popup_finish(),
                    _ => {}
                }
                glib::Propagation::Stop
            });
        }
        {
            let weak = Rc::downgrade(&ui);
            keys.connect_modifiers(move |_, state| {
                if let Some(ui) = weak.upgrade() {
                    ui.modifiers_changed(state);
                }
                glib::Propagation::Proceed
            });
        }
        ui.popup.window.add_controller(keys);
        {
            // `show`'s race check: a modifier released before the popup
            // held the keyboard is never reported, so look once it does.
            let weak = Rc::downgrade(&ui);
            ui.popup.window.connect_is_active_notify(move |window| {
                if !window.is_active() {
                    return;
                }
                let weak = weak.clone();
                glib::timeout_add_local_once(Duration::from_millis(30), move || {
                    let Some(ui) = weak.upgrade() else { return };
                    let state = gtk::gdk::Display::default()
                        .and_then(|d| d.default_seat())
                        .and_then(|s| s.keyboard())
                        .map(|k| k.modifier_state())
                        .unwrap_or_else(gtk::gdk::ModifierType::empty);
                    ui.modifiers_changed(state);
                });
            });
        }
        ui.install_actions();
        ui
    }

    /// The input-sources settings changed (or were sent first): mirror
    /// the keymap the compositor now has, follow `per-window`, and on a
    /// new `sources` list re-run GNOME's MRU update.
    pub fn sync(&self, keymap: (String, String, String)) {
        let Some(settings) = self.settings.as_ref() else {
            return;
        };
        let list = source_list(settings, "sources");
        let sources = usable_sources(&list);
        if self.keymap.borrow().as_ref() != Some(&keymap) {
            let n = sources.len().max(1);
            self.layout.set(kept_layout(self.layout.get(), n));
            self.layouts.set(n);
            *self.keymap.borrow_mut() = Some(keymap);
        }
        let per_window = settings
            .settings_schema()
            .is_some_and(|s| s.has_key("per-window"))
            && settings.boolean("per-window");
        self.manager.borrow_mut().set_per_window(per_window);
        if self.manager.borrow().sources() == sources.as_slice() {
            return;
        }
        let saved: Vec<Source> = source_list(settings, "mru-sources")
            .into_iter()
            .map(|(kind, id)| Source { kind, id })
            .collect();
        let activation = self.manager.borrow_mut().set_sources(sources, &saved);
        self.popup_close(None);
        self.apply(activation);
    }

    /// The focused window or the overview, after every control update.
    pub fn focus(&self, holder: Option<Holder>) {
        let activation = self.manager.borrow_mut().focus(holder);
        if let Some(activation) = activation {
            self.apply(activation);
        }
    }

    /// `switch-input-source(-backward)` fired in `mode` from the
    /// accelerator `accelerator` (`_switchInputSource`).
    pub fn binding(&self, backward: bool, mode: u32, accelerator: &str) {
        if self.popup.open.get() {
            self.popup_step(backward);
            return;
        }
        if self.manager.borrow().mru().len() < 2 {
            return;
        }
        if mode & (tuna_shell_control::MODE_POPUP | tuna_shell_control::MODE_LOCK_SCREEN) != 0 {
            let activation = self.manager.borrow_mut().next_by_position();
            if let Some(activation) = activation {
                self.apply(activation);
            }
            return;
        }
        let mask = gtk::accelerator_parse(accelerator)
            .map(|(_, mods)| mods.bits())
            .unwrap_or(0);
        self.popup_show(backward, primary_modifier(mask));
    }

    fn apply(&self, activation: Activation) {
        let n = self.layouts.get();
        let (backward, count) = steps(self.layout.get(), activation.index, n);
        for _ in 0..count {
            (self.step)(backward);
        }
        if n > 0 {
            self.layout.set(activation.index % n);
        }
        if activation.interactive {
            self.save_mru();
        }
    }

    /// `_updateMruSettings`.
    fn save_mru(&self) {
        let Some(settings) = self.settings.as_ref() else {
            return;
        };
        if !settings
            .settings_schema()
            .is_some_and(|s| s.has_key("mru-sources"))
        {
            return;
        }
        let list: Vec<(String, String)> = self
            .manager
            .borrow()
            .mru()
            .iter()
            .map(|s| (s.kind.clone(), s.id.clone()))
            .collect();
        if source_list(settings, "mru-sources") != list {
            let _ = settings.set_value("mru-sources", &list.to_variant());
        }
    }

    fn popup_show(&self, backward: bool, mask: u32) {
        let mru: Vec<Source> = self.manager.borrow().mru().to_vec();
        let sources: Vec<Source> = self.manager.borrow().sources().to_vec();
        while let Some(child) = self.popup.row.first_child() {
            self.popup.row.remove(&child);
        }
        let shorts: Vec<String> = sources.iter().map(|s| self.names.lookup(&s.id).1).collect();
        let shorts = unique_short_names(&shorts);
        let mut items = Vec::new();
        for source in &mru {
            let Some(index) = sources.iter().position(|s| s == source) else {
                continue;
            };
            let (display, _) = self.names.lookup(&source.id);
            let item = gtk::Box::new(gtk::Orientation::Vertical, 6);
            item.add_css_class("item-box");
            let symbol = gtk::Label::new(Some(&shorts[index]));
            symbol.add_css_class("input-source-switcher-symbol");
            symbol.set_size_request(96, 96);
            let text = gtk::Label::new(Some(&display));
            item.append(&symbol);
            item.append(&text);
            item.update_property(&[gtk::accessible::Property::Label(&display)]);
            self.popup.row.append(&item);
            items.push((index, item));
        }
        if items.is_empty() {
            return;
        }
        let selected = initial_selection(items.len(), backward);
        *self.popup.items.borrow_mut() = items;
        self.popup.open.set(true);
        self.popup
            .mask
            .set(gtk::gdk::ModifierType::from_bits_truncate(mask));
        self.select(selected);
        // Held for the keyboard but invisible for POPUP_DELAY, so a quick
        // tap never flashes it.
        self.popup.window.set_opacity(0.0);
        self.popup.window.present();
        if let Some(old) = self.popup.delay.borrow_mut().take() {
            old.remove();
        }
        let window = self.popup.window.downgrade();
        let slot = self.popup.delay.clone();
        let id = glib::timeout_add_local_once(POPUP_DELAY, move || {
            slot.borrow_mut().take();
            if let Some(window) = window.upgrade() {
                window.set_opacity(1.0);
            }
        });
        *self.popup.delay.borrow_mut() = Some(id);
        if mask == 0 {
            self.reset_no_mods();
        }
    }

    fn reset_no_mods(&self) {
        if let Some(old) = self.popup.no_mods.borrow_mut().take() {
            old.remove();
        }
        let window = self.popup.window.downgrade();
        let slot = self.popup.no_mods.clone();
        let id = glib::timeout_add_local_once(NO_MODS_TIMEOUT, move || {
            slot.borrow_mut().take();
            // The timer holds only the window: finish through its action.
            if let Some(window) = window.upgrade() {
                let _ = window.activate_action("input-source.finish", None);
            }
        });
        *self.popup.no_mods.borrow_mut() = Some(id);
    }

    fn select(&self, index: usize) {
        self.popup.selected.set(index);
        for (i, (_, item)) in self.popup.items.borrow().iter().enumerate() {
            if i == index {
                item.add_css_class("selected");
            } else {
                item.remove_css_class("selected");
            }
        }
    }

    fn popup_step(&self, backward: bool) {
        let len = self.popup.items.borrow().len();
        if !self.popup.open.get() || len == 0 {
            return;
        }
        let current = self.popup.selected.get();
        let next = if backward {
            (current + len - 1) % len
        } else {
            (current + 1) % len
        };
        self.select(next);
        // A key press shows the popup at once (`_showImmediately`).
        self.popup.window.set_opacity(1.0);
        if self.popup.mask.get().is_empty() {
            self.reset_no_mods();
        }
    }

    fn modifiers_changed(&self, state: gtk::gdk::ModifierType) {
        if !self.popup.open.get() {
            return;
        }
        let mask = self.popup.mask.get();
        if mask.is_empty() {
            self.reset_no_mods();
        } else if !state.intersects(mask) {
            self.popup_finish();
        }
    }

    /// `_finish`: activate the selected source, interactively.
    fn popup_finish(&self) {
        let chosen = self
            .popup
            .items
            .borrow()
            .get(self.popup.selected.get())
            .map(|(index, _)| *index);
        self.popup_close(chosen);
    }

    fn popup_close(&self, chosen: Option<usize>) {
        if !self.popup.open.replace(false) {
            return;
        }
        for slot in [&self.popup.delay, &self.popup.no_mods] {
            if let Some(id) = slot.borrow_mut().take() {
                id.remove();
            }
        }
        self.popup.window.set_visible(false);
        if let Some(index) = chosen {
            let activation = self.manager.borrow_mut().activate(index, true);
            self.apply(activation);
        }
    }

    /// The popup's finish as a window action, for timers that hold only
    /// the window.
    fn install_actions(self: &Rc<Self>) {
        let finish = gio::SimpleAction::new("finish", None);
        let weak = Rc::downgrade(self);
        finish.connect_activate(move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.popup_finish();
            }
        });
        let group = gio::SimpleActionGroup::new();
        group.add_action(&finish);
        self.popup
            .window
            .insert_action_group("input-source", Some(&group));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn xkb(ids: &[&str]) -> Vec<Source> {
        ids.iter().map(|id| Source::xkb(id)).collect()
    }

    #[test]
    fn usable_sources_are_the_keymaps_xkb_layouts() {
        let list = vec![
            ("ibus".to_owned(), "anthy".to_owned()),
            ("xkb".to_owned(), "de+nodeadkeys".to_owned()),
        ];
        assert_eq!(usable_sources(&list), xkb(&["de+nodeadkeys"]));
        assert_eq!(usable_sources(&[]), xkb(&["us"]));
    }

    #[test]
    fn mru_seeds_from_settings_then_follows_activations_like_gnome() {
        let mut m = Manager::default();
        // First start: mru-sources orders the list; the front activates.
        let a = m.set_sources(xkb(&["us", "de", "fr"]), &xkb(&["fr", "us"]));
        assert_eq!(m.mru(), xkb(&["fr", "us", "de"]).as_slice());
        assert_eq!(
            a,
            Activation {
                index: 2,
                interactive: false
            }
        );
        // An interactive pick moves to the front.
        assert_eq!(
            m.activate(1, true),
            Activation {
                index: 1,
                interactive: true
            }
        );
        assert_eq!(m.mru(), xkb(&["de", "fr", "us"]).as_slice());
        // A sources change keeps the known order; new ones follow.
        let a = m.set_sources(xkb(&["us", "fr", "it"]), &[]);
        assert_eq!(m.mru(), xkb(&["fr", "us", "it"]).as_slice());
        assert_eq!(m.sources()[a.index], Source::xkb("fr"));
        // Unknown saved entries are dropped; one known entry keeps
        // settings order (GNOME reorders only for two or more).
        let mut fresh = Manager::default();
        fresh.set_sources(xkb(&["us", "de"]), &xkb(&["xx", "de"]));
        assert_eq!(fresh.mru(), xkb(&["us", "de"]).as_slice());
    }

    #[test]
    fn the_switcher_selects_the_second_most_recent_and_wraps() {
        assert_eq!(initial_selection(3, false), 1);
        assert_eq!(initial_selection(3, true), 2);
        assert_eq!(initial_selection(1, false), 0);
        // Popup mode steps by position, wrapping.
        let mut m = Manager::default();
        m.set_sources(xkb(&["us", "de", "fr"]), &[]);
        assert_eq!(m.next_by_position().map(|a| a.index), Some(1));
        assert_eq!(m.next_by_position().map(|a| a.index), Some(2));
        assert_eq!(m.next_by_position().map(|a| a.index), Some(0));
        assert_eq!(primary_modifier(0), 0);
        assert_eq!(primary_modifier(0b1001), 0b1000);
    }

    #[test]
    fn per_window_restores_each_windows_source_and_starts_new_ones_first() {
        let mut m = Manager::default();
        m.set_sources(xkb(&["us", "de"]), &[]);
        // Off: focus changes nothing.
        assert_eq!(m.focus(Some(Holder::Window(1))), None);
        m.set_per_window(true);
        // Window A: first seen → first source; the user picks German.
        assert_eq!(m.focus(Some(Holder::Window(2))).map(|a| a.index), Some(0));
        m.activate(1, true);
        // Window B: first seen → first source again.
        assert_eq!(m.focus(Some(Holder::Window(3))).map(|a| a.index), Some(0));
        // A popup holding the keyboard (no window) keeps B current.
        assert_eq!(m.focus(None), None);
        // Back to A: German returns; to B: US.
        assert_eq!(m.focus(Some(Holder::Window(2))).map(|a| a.index), Some(1));
        assert_eq!(m.focus(Some(Holder::Window(3))).map(|a| a.index), Some(0));
        // The overview is one more window.
        assert_eq!(m.focus(Some(Holder::Overview)).map(|a| a.index), Some(0));
        m.activate(1, true);
        assert_eq!(m.focus(Some(Holder::Window(3))).map(|a| a.index), Some(0));
        assert_eq!(m.focus(Some(Holder::Overview)).map(|a| a.index), Some(1));
        // A sources change maps remembered sources into the new list.
        m.set_sources(xkb(&["de", "fr"]), &[]);
        assert_eq!(
            m.focus(Some(Holder::Window(2)))
                .map(|a| m.sources()[a.index].clone()),
            Some(Source::xkb("de"))
        );
        // Off forgets every window.
        m.set_per_window(false);
        m.set_per_window(true);
        assert_eq!(m.focus(Some(Holder::Window(3))).map(|a| a.index), Some(0));
        assert_eq!(m.focus(Some(Holder::Window(2))).map(|a| a.index), Some(0));
    }

    #[test]
    fn steps_take_the_short_way_and_reloads_keep_the_layout() {
        assert_eq!(steps(0, 0, 3), (false, 0));
        assert_eq!(steps(0, 1, 3), (false, 1));
        assert_eq!(steps(0, 2, 3), (true, 1));
        assert_eq!(steps(2, 0, 4), (false, 2));
        assert_eq!(steps(0, 3, 4), (true, 1));
        assert_eq!(steps(0, 1, 1), (false, 0));
        assert_eq!(kept_layout(1, 2), 1);
        assert_eq!(kept_layout(2, 2), 0);
    }

    #[test]
    fn names_come_from_xkeyboard_config_rules() {
        let mut names = XkbNames::default();
        names.add_rules(
            r#"<xkbConfigRegistry><layoutList>
            <layout><configItem><name>de</name><shortDescription>de</shortDescription>
            <description>German</description></configItem>
            <variantList><variant><configItem><name>nodeadkeys</name>
            <description>German (no dead keys)</description></configItem></variant></variantList>
            </layout></layoutList></xkbConfigRegistry>"#,
        );
        assert_eq!(names.lookup("de"), ("German".into(), "de".into()));
        assert_eq!(
            names.lookup("de+nodeadkeys"),
            ("German (no dead keys)".into(), "de".into())
        );
        assert_eq!(names.lookup("zz+q"), ("zz+q".into(), "zz".into()));
        assert_eq!(
            unique_short_names(&["de".into(), "us".into(), "de".into()]),
            vec!["de₁".to_owned(), "us".into(), "de₂".into()]
        );
    }
}
