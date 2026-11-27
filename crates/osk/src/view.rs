//! One drawn keyboard: `Keyboard`'s actor tree (gnome-shell 51.0
//! `js/ui/keyboard.js`), its extended-key and language popups and the
//! emoji pager. Where its typing goes is the owner's: the shell's session
//! keyboard types through the compositor, a prompt's keyboard edits only
//! its prompt ([`crate::prompt`]).

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;

use crate::layout::{self, Action, EmojiSection, Key, Purpose};
use crate::model::{Effect, KeyRef, Keyboard};

/// `KeyContainer`: a 1:1 key takes this many grid cells.
const KEY_SIZE: f64 = 2.0;
/// `KEYBOARD_ANIMATION_TIME`.
pub const KEYBOARD_ANIMATION_TIME_MS: u32 = 150;
/// `$medium_scalable_icon_size`.
const KEY_ICON_SIZE: i32 = 24;
pub const KEYVAL_RETURN: u32 = 0xff0d;
pub const KEYVAL_KP_ENTER: u32 = 0xff8d;
pub const KEYVAL_BACKSPACE: u32 = 0xff08;
/// `zwp_text_input_v3` hints a password field carries.
pub const PASSWORD_HINTS: u32 = 0x40 | 0x80;

/// Where a keyboard's typing goes: commits and keysyms only (the view
/// handles hide, the language menu and the emoji pager itself).
pub type Typing = Rc<dyn Fn(Effect)>;

/// The input sources the language key offers (`LanguageSelectionPopup`).
pub trait Languages {
    /// Every source's name and the current one's index.
    fn menu(&self) -> (Vec<String>, Option<usize>);
    /// Activate the source at `index`.
    fn choose(&self, index: usize);
    /// Open the keyboard settings, where the session may open them
    /// (`allowSettings`); `None` leaves "Keyboard Settings" out.
    fn settings(&self) -> Option<Rc<dyn Fn()>>;
}

/// `Keyboard._relayout`: a third of a landscape monitor's height, a
/// quarter of a portrait one's, never below the keys' own minimum nor
/// above half the monitor.
pub fn keyboard_height(width: i32, height: i32, min: i32) -> i32 {
    let base = if width > height {
        height / 3
    } else {
        height / 4
    };
    base.max(min).min(height / 2)
}

/// A key's accessible name: its label, its string, or what its icon means.
pub fn key_name(key: &Key, latched: bool) -> String {
    if let Some(label) = &key.label {
        return label.clone();
    }
    if let Some(text) = key.commit_string() {
        return match text {
            " " => "Space".to_owned(),
            text => text.to_owned(),
        };
    }
    let name = match key.icon_name.as_deref() {
        Some("osk-shift-symbolic") if latched => "Caps Lock",
        Some("osk-shift-symbolic") => "Shift",
        Some("osk-caps-lock-symbolic") => "Caps Lock",
        Some("osk-delete-symbolic") => "Backspace",
        Some("osk-enter-symbolic") => "Enter",
        Some("osk-hide-symbolic") => "Hide Keyboard",
        Some("osk-emoji-picker-symbolic") => "Emoji",
        Some("osk-layout-symbolic") => "Language",
        Some("go-up-symbolic") => "Up",
        Some("go-down-symbolic") => "Down",
        Some("go-previous-symbolic") => "Left",
        Some("go-next-symbolic") => "Right",
        Some(other) => other,
        None => "Key",
    };
    name.to_owned()
}

fn is_shift_key(key: &Key) -> bool {
    key.action == Some(Action::LevelSwitch)
        && key.icon_name.as_deref() == Some("osk-shift-symbolic")
}

/// `Key._makeKey`: an `St.Button` of style `keyboard-key` that never
/// takes focus, so the text field keeps it.
fn key_button(label: Option<&str>, icon: Option<&str>, name: &str) -> gtk::Button {
    let button = gtk::Button::new();
    button.add_css_class("keyboard-key");
    button.set_focusable(false);
    button.set_focus_on_click(false);
    button.set_can_focus(false);
    button.set_hexpand(true);
    button.set_vexpand(true);
    if let Some(icon) = icon {
        let image = gtk::Image::from_icon_name(icon);
        image.set_pixel_size(KEY_ICON_SIZE);
        button.set_child(Some(&image));
    } else if let Some(label) = label {
        button.set_label(label);
    }
    button.update_property(&[gtk::accessible::Property::Label(name)]);
    button
}

/// `Key`: the button inside its padded `key-container`.
fn key_container(button: &gtk::Button) -> gtk::Box {
    let container = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    container.add_css_class("key-container");
    container.set_hexpand(true);
    container.set_vexpand(true);
    container.append(button);
    container
}

/// The grid cell where `units` keys start on `KeyContainer`'s grid:
/// Clutter's integer attach truncates, as GJS passes the product.
fn cell(units: f64) -> i32 {
    (units * KEY_SIZE) as i32
}

/// The cell span of `units` keys, at least one cell.
fn cells(units: f64) -> i32 {
    cell(units).max(1)
}

thread_local! {
    static EMOJI: RefCell<Option<Rc<Vec<EmojiSection>>>> = const { RefCell::new(None) };
}

/// GNOME's emoji list, parsed once on first use.
fn emoji_sections() -> Rc<Vec<EmojiSection>> {
    EMOJI.with(|cell| {
        cell.borrow_mut()
            .get_or_insert_with(|| Rc::new(layout::emoji_sections()))
            .clone()
    })
}

/// One emoji page: `EmojiPager._initPagingInfo`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EmojiPage {
    section: usize,
    page: usize,
    pages: usize,
    start: usize,
    end: usize,
}

fn emoji_pages(sections: &[EmojiSection], per_page: usize) -> Vec<EmojiPage> {
    let per_page = per_page.max(1);
    let mut pages = Vec::new();
    for (section, s) in sections.iter().enumerate() {
        let count = s.keys.len().div_ceil(per_page);
        for page in 0..count {
            pages.push(EmojiPage {
                section,
                page,
                pages: count,
                start: page * per_page,
                end: ((page + 1) * per_page).min(s.keys.len()),
            });
        }
    }
    pages
}

/// What a drawn level shows: layout, level, latch, modifiers, emoji key.
type Drawn = (String, String, bool, Vec<u32>, bool);

struct View {
    model: RefCell<Keyboard>,
    root: gtk::Box,
    stack: gtk::Stack,
    /// `AspectContainer` around the level's `KeyContainer`.
    frame: gtk::AspectFrame,
    emoji_frame: gtk::AspectFrame,
    emoji: RefCell<Option<EmojiPanel>>,
    /// The open extended-key or language popup (`NoGrabPopup`).
    popover: RefCell<Option<gtk::Popover>>,
    typing: Typing,
    hide: Rc<dyn Fn()>,
    sources: Option<Rc<dyn Languages>>,
    /// What the drawn level shows, to redraw only on change.
    drawn: RefCell<Option<Drawn>>,
}

/// One drawn keyboard: `Keyboard`'s actor tree.
#[derive(Clone)]
pub struct KeyboardView(Rc<View>);

impl KeyboardView {
    /// `typing` gets commits and keysyms; `hide` is the hide key.
    pub fn new(
        model: Keyboard,
        typing: Typing,
        hide: Rc<dyn Fn()>,
        sources: Option<Rc<dyn Languages>>,
    ) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_widget_name("keyboard");
        root.add_css_class("osk-keyboard");
        root.set_focusable(false);
        root.set_can_focus(false);
        let frame = gtk::AspectFrame::new(0.5, 0.5, 1.0, false);
        frame.set_hexpand(true);
        frame.set_vexpand(true);
        let emoji_frame = gtk::AspectFrame::new(0.5, 0.5, 1.0, false);
        emoji_frame.set_hexpand(true);
        emoji_frame.set_vexpand(true);
        let stack = gtk::Stack::new();
        stack.set_vexpand(true);
        stack.add_named(&frame, Some("keys"));
        stack.add_named(&emoji_frame, Some("emoji"));
        root.append(&stack);
        let view = Rc::new(View {
            model: RefCell::new(model),
            root,
            stack,
            frame,
            emoji_frame,
            emoji: RefCell::new(None),
            popover: RefCell::new(None),
            typing,
            hide,
            sources,
            drawn: RefCell::new(None),
        });
        // `NoGrabPopup`: a press anywhere else closes the open popup.
        let outside = gtk::GestureClick::new();
        outside.set_propagation_phase(gtk::PropagationPhase::Capture);
        {
            let weak = Rc::downgrade(&view);
            outside.connect_pressed(move |gesture, _, x, y| {
                let Some(view) = weak.upgrade() else { return };
                let picked = gesture
                    .widget()
                    .and_then(|w| w.pick(x, y, gtk::PickFlags::DEFAULT));
                let inside = view.popover.borrow().as_ref().is_some_and(|p| {
                    picked
                        .as_ref()
                        .is_some_and(|w| w == p.upcast_ref::<gtk::Widget>() || w.is_ancestor(p))
                });
                if !inside {
                    view.close_popover();
                }
            });
        }
        view.root.add_controller(outside);
        view.render();
        Self(view)
    }

    pub fn widget(&self) -> &gtk::Box {
        &self.0.root
    }

    /// `_onGroupChanged`.
    pub fn set_group(&self, group: &str) {
        self.0.model.borrow_mut().set_group(group);
        self.0.render();
    }

    /// Purpose and hints of the focused field.
    pub fn set_content(&self, purpose: Purpose, hints: u32) {
        self.0.model.borrow_mut().set_content(purpose, hints);
        self.0.render();
    }

    /// `_open`: always the keys, never the emoji.
    pub fn opened(&self) {
        self.0.model.borrow_mut().opened();
        self.0.show_page();
        self.0.render();
    }

    /// `_close`: modifiers off, a held delete released, popups closed.
    pub fn closed(&self) {
        self.0.close_popover();
        let effects = self.0.model.borrow_mut().closed();
        self.0.apply(effects, None);
        self.0.render();
    }
}

impl View {
    fn render(self: &Rc<Self>) {
        let state = {
            let model = self.model.borrow();
            (
                model.layout().name.clone(),
                model.level_name().to_owned(),
                model.latched(),
                model.modifiers().to_vec(),
                model.emoji_visible(),
            )
        };
        if self.drawn.borrow().as_ref() == Some(&state) {
            return;
        }
        self.close_popover();
        self.model.borrow_mut().keys_rebuilt();
        let grid = gtk::Grid::new();
        grid.set_row_homogeneous(true);
        grid.set_column_homogeneous(true);
        grid.set_hexpand(true);
        grid.set_vexpand(true);
        let ratio = {
            let model = self.model.borrow();
            let level = model.level();
            let (placed, (cols, rows)) = layout::place(level, model.emoji_visible());
            for p in &placed {
                let key = &level.rows[p.row][p.index];
                let kref = KeyRef {
                    row: p.row,
                    index: p.index,
                };
                let widget = self.level_key(key, kref, &model);
                grid.attach(
                    &widget,
                    cell(p.left),
                    cell(p.top),
                    cells(p.width),
                    cells(p.height),
                );
            }
            (cols, rows)
        };
        if ratio.1 > 0.0 {
            self.frame.set_ratio((ratio.0 / ratio.1) as f32);
            self.emoji_frame.set_ratio((ratio.0 / ratio.1) as f32);
        }
        self.frame.set_child(Some(&grid));
        *self.drawn.borrow_mut() = Some(state);
        // `setRatio` on the emoji selection: its pager follows the level.
        if let Some(panel) = self.emoji.borrow().as_ref() {
            panel.set_ratio(ratio.0, ratio.1);
        }
    }

    /// One key of the level, wired as `_addRowKeys` wires it.
    fn level_key(self: &Rc<Self>, key: &Key, kref: KeyRef, model: &Keyboard) -> gtk::Box {
        let shift = is_shift_key(key);
        let latched_shift = shift && model.latched();
        let icon = if latched_shift {
            Some("osk-caps-lock-symbolic")
        } else {
            key.icon_name.as_deref()
        };
        let label = key.label.as_deref().or(key.commit_string());
        let button = key_button(label, icon, &key_name(key, latched_shift));
        if key.action.is_some() || key.keyval.is_some() {
            button.add_css_class("default-key");
        }
        let modifier_on = key.action == Some(Action::Modifier)
            && key.keyval.is_some_and(|k| model.modifier_active(k));
        if latched_shift || modifier_on {
            button.add_css_class("latched");
        }
        {
            let weak = Rc::downgrade(self);
            button.connect_clicked(move |button| {
                if let Some(view) = weak.upgrade() {
                    view.release(kref, button.upcast_ref());
                }
            });
        }
        let long_press = gtk::GestureLongPress::new();
        {
            let weak = Rc::downgrade(self);
            let button = button.downgrade();
            let extended = key.extended().to_vec();
            long_press.connect_pressed(move |gesture, _, _| {
                let (Some(view), Some(button)) = (weak.upgrade(), button.upgrade()) else {
                    return;
                };
                let effects = view.model.borrow_mut().long_press(kref);
                view.apply(effects, Some(button.upcast_ref()));
                if !extended.is_empty() {
                    // The popup stays for a tap, as GNOME's long press
                    // cancels the key's own click.
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                    view.extended_popup(&button, &extended);
                } else {
                    view.render();
                }
            });
        }
        button.add_controller(long_press);
        key_container(&button)
    }

    fn release(self: &Rc<Self>, kref: KeyRef, anchor: &gtk::Widget) {
        self.close_popover();
        let effects = self.model.borrow_mut().release(kref);
        self.apply(effects, Some(anchor));
        self.render();
    }

    fn apply(self: &Rc<Self>, effects: Vec<Effect>, anchor: Option<&gtk::Widget>) {
        for effect in effects {
            match effect {
                Effect::Hide => (self.hide)(),
                Effect::LanguageMenu => {
                    if let Some(anchor) = anchor {
                        self.language_popup(anchor);
                    }
                }
                Effect::ToggleEmoji => self.show_page(),
                typing => (self.typing)(typing),
            }
        }
    }

    /// `_setEmojiActive`: the keys or the emoji selection.
    fn show_page(self: &Rc<Self>) {
        let active = self.model.borrow().emoji_active();
        if active {
            self.ensure_emoji();
            self.stack.set_visible_child_name("emoji");
        } else {
            self.stack.set_visible_child_name("keys");
        }
    }

    fn ensure_emoji(self: &Rc<Self>) {
        if self.emoji.borrow().is_some() {
            return;
        }
        let weak = Rc::downgrade(self);
        let panel = EmojiPanel::new(weak);
        self.emoji_frame.set_child(Some(&panel.inner.root));
        let (cols, rows) = {
            let model = self.model.borrow();
            layout::place(model.level(), model.emoji_visible()).1
        };
        panel.set_ratio(cols, rows.max(1.0));
        *self.emoji.borrow_mut() = Some(panel);
    }

    fn close_popover(&self) {
        if let Some(popover) = self.popover.borrow_mut().take() {
            popover.popdown();
            popover.unparent();
        }
    }

    fn popup(&self, anchor: &gtk::Widget, child: &gtk::Widget, class: &str) {
        self.close_popover();
        let popover = gtk::Popover::new();
        // `NoGrabPopup`: no grab, so the text field keeps the keyboard.
        popover.set_autohide(false);
        popover.set_can_focus(false);
        popover.set_focusable(false);
        popover.set_position(gtk::PositionType::Top);
        popover.add_css_class(class);
        popover.set_child(Some(child));
        popover.set_parent(anchor);
        popover.popup();
        *self.popover.borrow_mut() = Some(popover);
    }

    /// `Key._ensureExtendedKeysPopup`: the key's other strings, each the
    /// size of the key, in a row above it.
    fn extended_popup(self: &Rc<Self>, anchor: &gtk::Button, extended: &[String]) {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        row.add_css_class("key-container");
        let (w, h) = (anchor.width(), anchor.height());
        for text in extended {
            let button = key_button(Some(text), None, text);
            button.set_size_request(w, h);
            let weak = Rc::downgrade(self);
            let text = text.clone();
            button.connect_clicked(move |button| {
                let Some(view) = weak.upgrade() else { return };
                view.close_popover();
                let effects = view.model.borrow_mut().extended(&text);
                view.apply(effects, Some(button.upcast_ref()));
                view.render();
            });
            row.append(&button);
        }
        self.popup(anchor.upcast_ref(), row.upcast_ref(), "keyboard-subkeys");
    }

    /// `LanguageSelectionPopup`: every input source, the current one
    /// marked, and the keyboard settings.
    fn language_popup(self: &Rc<Self>, anchor: &gtk::Widget) {
        let Some(sources) = self.sources.clone() else {
            return;
        };
        let (names, current) = sources.menu();
        let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        list.add_css_class("osk-language-menu");
        for (index, name) in names.iter().enumerate() {
            let item = gtk::Button::new();
            item.add_css_class("flat");
            item.set_focusable(false);
            item.set_can_focus(false);
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            let dot = gtk::Image::from_icon_name("ornament-dot-checked-symbolic");
            dot.set_opacity(if current == Some(index) { 1.0 } else { 0.0 });
            row.append(&dot);
            let label = gtk::Label::new(Some(name));
            label.set_xalign(0.0);
            row.append(&label);
            item.set_child(Some(&row));
            item.update_property(&[gtk::accessible::Property::Label(name)]);
            let (weak, sources) = (Rc::downgrade(self), sources.clone());
            item.connect_clicked(move |_| {
                if let Some(view) = weak.upgrade() {
                    view.close_popover();
                }
                sources.choose(index);
            });
            list.append(&item);
        }
        if let Some(open) = sources.settings() {
            list.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
            let settings = gtk::Button::with_label("Keyboard Settings");
            settings.add_css_class("flat");
            settings.set_focusable(false);
            settings.set_can_focus(false);
            let weak = Rc::downgrade(self);
            settings.connect_clicked(move |_| {
                if let Some(view) = weak.upgrade() {
                    view.close_popover();
                }
                open();
            });
            list.append(&settings);
        }
        self.popup(anchor, list.upcast_ref(), "osk-language-popup");
    }

    fn emoji_selected(self: &Rc<Self>, text: &str, anchor: &gtk::Widget) {
        self.close_popover();
        let effects = self.model.borrow_mut().emoji(text);
        self.apply(effects, Some(anchor));
    }

    /// `EmojiSelection`'s ABC key and the emoji page's toggle.
    fn emoji_toggle(self: &Rc<Self>) {
        let active = !self.model.borrow().emoji_active();
        self.model.borrow_mut().set_emoji_active(active);
        self.show_page();
    }
}

struct EmojiInner {
    view: Weak<View>,
    root: gtk::Box,
    pager: gtk::Box,
    dots: gtk::Box,
    bottom: gtk::Grid,
    sections: Rc<Vec<EmojiSection>>,
    section_buttons: RefCell<Vec<gtk::Button>>,
    pages: RefCell<Vec<EmojiPage>>,
    cols: Cell<usize>,
    rows: Cell<usize>,
    current: Cell<usize>,
}

/// `EmojiSelection`: the pager of one section's page, its page dots, and
/// the bottom row (ABC, one key per section, hide).
struct EmojiPanel {
    inner: Rc<EmojiInner>,
}

impl EmojiPanel {
    fn new(view: Weak<View>) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("emoji-panel");
        let pager = gtk::Box::new(gtk::Orientation::Vertical, 0);
        pager.set_vexpand(true);
        let dots = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        dots.add_css_class("page-indicator");
        dots.set_halign(gtk::Align::Center);
        let bottom = gtk::Grid::new();
        bottom.set_column_homogeneous(true);
        bottom.set_row_homogeneous(true);
        root.append(&pager);
        root.append(&dots);
        root.append(&bottom);
        let inner = Rc::new(EmojiInner {
            view,
            root,
            pager,
            dots,
            bottom,
            sections: emoji_sections(),
            section_buttons: RefCell::new(Vec::new()),
            pages: RefCell::new(Vec::new()),
            cols: Cell::new(10),
            rows: Cell::new(3),
            current: Cell::new(0),
        });
        inner.build_bottom_row();
        // `EmojiPager`'s swipe tracker: a horizontal swipe or scroll turns
        // the page, wrapping around.
        let swipe = gtk::GestureSwipe::new();
        {
            let weak = Rc::downgrade(&inner);
            swipe.connect_swipe(move |_, vx, vy| {
                let Some(inner) = weak.upgrade() else { return };
                if vx.abs() > vy.abs() && vx.abs() > 200.0 {
                    inner.step(vx < 0.0);
                }
            });
        }
        inner.pager.add_controller(swipe);
        let scroll = gtk::EventControllerScroll::new(
            gtk::EventControllerScrollFlags::BOTH_AXES | gtk::EventControllerScrollFlags::DISCRETE,
        );
        {
            let weak = Rc::downgrade(&inner);
            scroll.connect_scroll(move |_, dx, dy| {
                let Some(inner) = weak.upgrade() else {
                    return glib::Propagation::Proceed;
                };
                let delta = if dx != 0.0 { dx } else { dy };
                if delta != 0.0 {
                    inner.step(delta > 0.0);
                }
                glib::Propagation::Stop
            });
        }
        inner.pager.add_controller(scroll);
        Self { inner }
    }

    /// `EmojiSelection.setRatio(nCols, nRows)`: the pager takes the whole
    /// columns and all rows but the bottom row's.
    fn set_ratio(&self, cols: f64, rows: f64) {
        let inner = &self.inner;
        let cols = (cols.floor() as usize).max(1);
        let rows = (rows.floor() as usize).saturating_sub(1).max(1);
        if inner.cols.get() == cols && inner.rows.get() == rows && !inner.pages.borrow().is_empty()
        {
            return;
        }
        inner.cols.set(cols);
        inner.rows.set(rows);
        *inner.pages.borrow_mut() = emoji_pages(&inner.sections, cols * rows);
        inner.set_page(0);
    }
}

impl EmojiInner {
    fn build_bottom_row(self: &Rc<Self>) {
        let abc = key_button(Some("ABC"), None, "ABC");
        abc.add_css_class("default-key");
        {
            let view = self.view.clone();
            abc.connect_clicked(move |_| {
                if let Some(view) = view.upgrade() {
                    view.emoji_toggle();
                }
            });
        }
        // ABC is 1.5 keys wide; sections one each; hide one.
        self.bottom.attach(&key_container(&abc), 0, 0, 3, 1);
        let mut col = 3;
        let mut buttons = Vec::new();
        for (index, section) in self.sections.iter().enumerate() {
            let button = key_button(Some(&section.label), None, &section.label);
            let weak = Rc::downgrade(self);
            button.connect_clicked(move |_| {
                if let Some(inner) = weak.upgrade() {
                    inner.show_section(index);
                }
            });
            self.bottom.attach(&key_container(&button), col, 0, 2, 1);
            col += 2;
            buttons.push(button);
        }
        let hide = key_button(None, Some("osk-hide-symbolic"), "Hide Keyboard");
        hide.add_css_class("default-key");
        hide.add_css_class("hide-key");
        {
            let view = self.view.clone();
            hide.connect_clicked(move |_| {
                if let Some(view) = view.upgrade() {
                    (view.hide)();
                }
            });
        }
        self.bottom.attach(&key_container(&hide), col, 0, 2, 1);
        *self.section_buttons.borrow_mut() = buttons;
    }

    fn show_section(self: &Rc<Self>, section: usize) {
        let index = self
            .pages
            .borrow()
            .iter()
            .position(|p| p.section == section && p.page == 0);
        if let Some(index) = index {
            self.set_page(index);
        }
    }

    /// `_nextPage` / `_prevPage`, wrapping round all sections.
    fn step(self: &Rc<Self>, forward: bool) {
        let n = self.pages.borrow().len();
        if n == 0 {
            return;
        }
        let current = self.current.get();
        let next = if forward {
            (current + 1) % n
        } else {
            (current + n - 1) % n
        };
        self.set_page(next);
    }

    /// `setCurrentPage` → `_generatePanel` and `page-changed`.
    fn set_page(self: &Rc<Self>, index: usize) {
        let Some(page) = self.pages.borrow().get(index).copied() else {
            return;
        };
        self.current.set(index);
        while let Some(child) = self.pager.first_child() {
            self.pager.remove(&child);
        }
        let grid = gtk::Grid::new();
        grid.add_css_class("emoji-page");
        grid.set_column_homogeneous(true);
        grid.set_row_homogeneous(true);
        grid.set_vexpand(true);
        let cols = self.cols.get();
        // The expander keeps proportions on a partly filled page.
        let expander = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        grid.attach(&expander, 0, 0, cols as i32, self.rows.get() as i32);
        let keys = &self.sections[page.section].keys[page.start..page.end];
        for (i, key) in keys.iter().enumerate() {
            let button = key_button(Some(&key.label), None, &key.label);
            {
                let (view, label) = (self.view.clone(), key.label.clone());
                button.connect_clicked(move |button| {
                    if let Some(view) = view.upgrade() {
                        view.emoji_selected(&label, button.upcast_ref());
                    }
                });
            }
            if !key.variants.is_empty() {
                let long_press = gtk::GestureLongPress::new();
                let (view, variants) = (self.view.clone(), key.variants.clone());
                let anchor = button.downgrade();
                long_press.connect_pressed(move |gesture, _, _| {
                    let (Some(view), Some(anchor)) = (view.upgrade(), anchor.upgrade()) else {
                        return;
                    };
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                    let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                    row.add_css_class("key-container");
                    for variant in &variants {
                        let b = key_button(Some(variant), None, variant);
                        b.set_size_request(anchor.width(), anchor.height());
                        let (weak, variant) = (Rc::downgrade(&view), variant.clone());
                        b.connect_clicked(move |b| {
                            if let Some(view) = weak.upgrade() {
                                view.emoji_selected(&variant, b.upcast_ref());
                            }
                        });
                        row.append(&b);
                    }
                    view.popup(anchor.upcast_ref(), row.upcast_ref(), "keyboard-subkeys");
                });
                button.add_controller(long_press);
            }
            grid.attach(
                &key_container(&button),
                (i % cols) as i32,
                (i / cols) as i32,
                1,
                1,
            );
        }
        self.pager.append(&grid);
        // Page dots for the section, and the section's key latched.
        while let Some(child) = self.dots.first_child() {
            self.dots.remove(&child);
        }
        if page.pages > 1 {
            for p in 0..page.pages {
                let dot = gtk::Image::from_icon_name("pager-checked-symbolic");
                dot.add_css_class("page-indicator-icon");
                dot.set_opacity(if p == page.page { 1.0 } else { 0.5 });
                self.dots.append(&dot);
            }
        }
        for (i, button) in self.section_buttons.borrow().iter().enumerate() {
            if i == page.section {
                button.add_css_class("latched");
            } else {
                button.remove_css_class("latched");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyboard_takes_a_third_or_a_quarter_of_the_monitor() {
        assert_eq!(keyboard_height(1280, 800, 100), 266);
        assert_eq!(keyboard_height(800, 1280, 100), 320);
        // Never below the keys' minimum, never above half the monitor.
        assert_eq!(keyboard_height(1280, 800, 300), 300);
        assert_eq!(keyboard_height(1280, 800, 500), 400);
    }

    #[test]
    fn keys_are_named_for_assistive_technologies() {
        let layout = layout::parse(include_str!("../osk-layouts/us.json")).unwrap();
        let default = layout.level("default").unwrap();
        assert_eq!(key_name(&default.rows[0][0], false), "q");
        let shift = default.rows[2]
            .iter()
            .find(|k| is_shift_key(k))
            .expect("shift key");
        assert_eq!(key_name(shift, false), "Shift");
        assert_eq!(key_name(shift, true), "Caps Lock");
        let names: Vec<String> = default.rows[3].iter().map(|k| key_name(k, false)).collect();
        assert!(names.contains(&"?123".to_owned()), "{names:?}");
        assert!(names.contains(&"Space".to_owned()), "{names:?}");
        assert!(names.contains(&"Hide Keyboard".to_owned()), "{names:?}");
        assert_eq!(
            key_name(default.rows[0].last().unwrap(), false),
            "Backspace"
        );
        assert_eq!(key_name(default.rows[1].last().unwrap(), false), "Enter");
    }

    #[test]
    fn emoji_pages_split_each_section_like_the_pager() {
        let sections = layout::parse_emoji_sections(include_str!("../osk-layouts/emoji.json"))
            .expect("emoji.json");
        let pages = emoji_pages(&sections, 30);
        assert_eq!(pages[0].section, 0);
        assert_eq!(pages[0].page, 0);
        assert_eq!(pages[0].start, 0);
        for page in &pages {
            assert!(page.end - page.start <= 30);
            assert!(page.end > page.start);
            assert!(page.page < page.pages);
        }
        // Every key of every section is on exactly one page.
        for (i, section) in sections.iter().enumerate() {
            let covered: usize = pages
                .iter()
                .filter(|p| p.section == i)
                .map(|p| p.end - p.start)
                .sum();
            assert_eq!(covered, section.keys.len());
        }
    }

    #[test]
    fn key_cells_truncate_like_clutter_attach() {
        assert_eq!(cells(1.0), 2);
        assert_eq!(cells(1.5), 3);
        assert_eq!(cells(0.5), 1);
        assert_eq!(cells(6.5), 13);
        assert_eq!(cells(1.25), 2);
        assert_eq!(cell(0.0), 0);
        assert_eq!(cell(0.5), 1);
    }
}
