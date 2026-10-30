//! GNOME 51's on-screen keyboard (gnome-shell 51.0 `js/ui/keyboard.js`)
//! drawn with GTK (#349).
//!
//! [`Osk`] is `KeyboardManager`: it follows `screen-keyboard-enabled`
//! and the compositor's touch-mode auto-show, the current input source
//! (the layout group) and the compositor's input-method focus. The
//! session keyboard is a layer surface that never takes the keyboard,
//! so the text field keeps focus while keys are pressed; it types
//! through the compositor (`OskInput`, `KeyboardController`). On the lock
//! screen each lock window embeds its own keyboard, opened by the unlock
//! prompt's focus (`_onKeyFocusChanged`), which edits only that prompt:
//! nothing typed there reaches the compositor or the session, and
//! nothing of the session is drawn.
//!
//! The keys come from GNOME's own layouts ([`crate::osk_layout`]) and
//! behave as GNOME's `Keyboard` ([`crate::osk_model`]): levels, shift and
//! its long-press latch, long-press extended keys, the number levels,
//! modifiers, the emoji pager and the language menu.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::time::Instant;

use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gio, glib};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use tuna_shell_control::{OskInput, TextFieldFocus};

use crate::input_sources::InputSources;
use crate::osk_layout::{self, Action, EmojiSection, Key, Purpose};
use crate::osk_model::{Effect, KeyRef, Keyboard, Presence};

const A11Y_APPLICATIONS_SCHEMA: &str = "org.gnome.desktop.a11y.applications";
const SHOW_KEYBOARD: &str = "screen-keyboard-enabled";
/// `KeyContainer`: a 1:1 key takes this many grid cells.
const KEY_SIZE: f64 = 2.0;
/// `KEYBOARD_ANIMATION_TIME`.
const KEYBOARD_ANIMATION_TIME_MS: u32 = 150;
/// `$medium_scalable_icon_size`.
const KEY_ICON_SIZE: i32 = 24;
const KEYVAL_RETURN: u32 = 0xff0d;
const KEYVAL_KP_ENTER: u32 = 0xff8d;
const KEYVAL_BACKSPACE: u32 = 0xff08;
/// `zwp_text_input_v3` hints a password field carries.
const PASSWORD_HINTS: u32 = 0x40 | 0x80;

/// Where a keyboard's typing goes: commits and keysyms only (the view
/// handles hide, the language menu and the emoji pager itself).
pub type Typing = Rc<dyn Fn(Effect)>;

/// The session keyboard's typing: GNOME's `KeyboardController` through
/// the compositor.
pub fn compositor_input(effect: Effect) -> Vec<OskInput> {
    match effect {
        Effect::Commit { text, modifiers } => vec![OskInput::Commit { text, modifiers }],
        Effect::Keyval(keysym) => vec![
            OskInput::Keyval {
                keysym,
                pressed: true,
            },
            OskInput::Keyval {
                keysym,
                pressed: false,
            },
        ],
        Effect::KeyvalPress(keysym) => vec![OskInput::Keyval {
            keysym,
            pressed: true,
        }],
        Effect::KeyvalRelease(keysym) => vec![OskInput::Keyval {
            keysym,
            pressed: false,
        }],
        Effect::Hide | Effect::LanguageMenu | Effect::ToggleEmoji => Vec::new(),
    }
}

/// An edit of the unlock prompt from the lock screen's keyboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptEdit {
    Insert(String),
    Backspace,
    Activate,
}

/// What the lock screen's keyboard does to its prompt: insert committed
/// text, delete on BackSpace, submit on Enter; every other key (and the
/// modifiers, which a password ignores) does nothing.
pub fn prompt_edit(effect: &Effect) -> Option<PromptEdit> {
    match effect {
        Effect::Commit { text, .. } => Some(PromptEdit::Insert(text.clone())),
        Effect::Keyval(KEYVAL_BACKSPACE) | Effect::KeyvalPress(KEYVAL_BACKSPACE) => {
            Some(PromptEdit::Backspace)
        }
        Effect::Keyval(KEYVAL_RETURN | KEYVAL_KP_ENTER) => Some(PromptEdit::Activate),
        _ => None,
    }
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
            .get_or_insert_with(|| Rc::new(osk_layout::emoji_sections()))
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
    sources: Option<Rc<InputSources>>,
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
        sources: Option<Rc<InputSources>>,
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
            let (placed, (cols, rows)) = osk_layout::place(level, model.emoji_visible());
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
            osk_layout::place(model.level(), model.emoji_visible()).1
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
        list.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        let settings = gtk::Button::with_label("Keyboard Settings");
        settings.add_css_class("flat");
        settings.set_focusable(false);
        settings.set_can_focus(false);
        {
            let weak = Rc::downgrade(self);
            settings.connect_clicked(move |_| {
                if let Some(view) = weak.upgrade() {
                    view.close_popover();
                }
                if let Err(e) = tuna_shell_host::apps::application_command("gnome-control-center")
                    .arg("keyboard")
                    .spawn()
                {
                    eprintln!("tuna-shell-gtk: could not open keyboard settings: {e}");
                }
            });
        }
        list.append(&settings);
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

/// The session keyboard: a layer surface at the bottom of the monitor
/// that never takes the keyboard (`Main.layoutManager.keyboardBox`).
struct Session {
    window: gtk::Window,
    revealer: gtk::Revealer,
    view: KeyboardView,
    presence: RefCell<Presence>,
    timer: RefCell<Option<glib::SourceId>>,
}

/// A lock window's keyboard: opened by the unlock prompt's focus, and
/// editing only that prompt.
pub struct LockKeyboard {
    revealer: gtk::Revealer,
    view: KeyboardView,
    presence: RefCell<Presence>,
    timer: RefCell<Option<glib::SourceId>>,
    osk: Weak<Osk>,
}

/// `KeyboardManager`.
pub struct Osk {
    settings: Option<gio::Settings>,
    setting: Cell<bool>,
    touch_auto: Cell<bool>,
    locked: Cell<bool>,
    focus: Cell<Option<TextFieldFocus>>,
    sources: Rc<InputSources>,
    send: Rc<dyn Fn(OskInput)>,
    epoch: Instant,
    session: RefCell<Option<Rc<Session>>>,
    locks: RefCell<Vec<Weak<LockKeyboard>>>,
}

impl Osk {
    /// `send` types into the focused client through the compositor.
    pub fn new(sources: Rc<InputSources>, send: Rc<dyn Fn(OskInput)>) -> Rc<Self> {
        let settings = crate::settings(A11Y_APPLICATIONS_SCHEMA).filter(|s| {
            s.settings_schema()
                .is_some_and(|schema| schema.has_key(SHOW_KEYBOARD))
        });
        let osk = Rc::new(Self {
            setting: Cell::new(settings.as_ref().is_some_and(|s| s.boolean(SHOW_KEYBOARD))),
            settings,
            touch_auto: Cell::new(false),
            locked: Cell::new(false),
            focus: Cell::new(None),
            sources: sources.clone(),
            send,
            epoch: Instant::now(),
            session: RefCell::new(None),
            locks: RefCell::new(Vec::new()),
        });
        if let Some(settings) = &osk.settings {
            let weak = Rc::downgrade(&osk);
            settings.connect_changed(Some(SHOW_KEYBOARD), move |settings, key| {
                if let Some(osk) = weak.upgrade() {
                    osk.setting.set(settings.boolean(key));
                    osk.sync_enabled();
                }
            });
        }
        {
            let weak = Rc::downgrade(&osk);
            sources.connect_changed(Rc::new(move || {
                if let Some(osk) = weak.upgrade() {
                    osk.group_changed();
                }
            }));
        }
        osk.sync_enabled();
        osk
    }

    fn now(&self) -> u64 {
        self.epoch
            .elapsed()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX)
    }

    /// `_syncEnabled`: `screen-keyboard-enabled` or touch-mode auto-show.
    pub fn enabled(&self) -> bool {
        self.setting.get() || self.touch_auto.get()
    }

    fn group(&self) -> String {
        self.sources.current_id()
    }

    fn sync_enabled(self: &Rc<Self>) {
        let enabled = self.enabled();
        if enabled && !self.locked.get() {
            self.ensure_session();
        }
        let now = self.now();
        let session = self.session.borrow().clone();
        if let Some(session) = session {
            session
                .presence
                .borrow_mut()
                .set_enabled(enabled && !self.locked.get(), now);
            self.session_changed(&session);
        }
        for lock in self.lock_keyboards() {
            lock.presence.borrow_mut().set_enabled(enabled, now);
            lock.sync();
        }
    }

    fn lock_keyboards(&self) -> Vec<Rc<LockKeyboard>> {
        let mut locks = self.locks.borrow_mut();
        locks.retain(|lock| lock.strong_count() > 0);
        locks.iter().filter_map(Weak::upgrade).collect()
    }

    fn group_changed(&self) {
        let group = self.group();
        if let Some(session) = self.session.borrow().as_ref() {
            session.view.set_group(&group);
        }
        for lock in self.lock_keyboards() {
            lock.view.set_group(&group);
        }
    }

    /// The compositor's `OnScreenKeyboard`: input-method focus, the
    /// input-panel request and touch-mode auto-show.
    pub fn update(self: &Rc<Self>, focus: Option<TextFieldFocus>, show: bool, touch_auto: bool) {
        if touch_auto != self.touch_auto.get() {
            self.touch_auto.set(touch_auto);
            self.sync_enabled();
        }
        self.focus.set(focus);
        let Some(session) = self.session.borrow().clone() else {
            return;
        };
        if let Some(field) = focus {
            session
                .view
                .set_content(Purpose::from_wire(field.purpose), field.hints);
        }
        let now = self.now();
        session.presence.borrow_mut().focus(
            focus.is_some(),
            show,
            focus.map_or(0, |f| f.hints),
            now,
        );
        self.session_changed(&session);
    }

    /// The session locked or unlocked: the session keyboard never shows
    /// over the lock screen.
    pub fn set_locked(self: &Rc<Self>, locked: bool) {
        if self.locked.replace(locked) != locked {
            self.sync_enabled();
        }
    }

    fn ensure_session(self: &Rc<Self>) {
        if self.session.borrow().is_some() {
            return;
        }
        let Some(model) = Keyboard::new(&self.group(), Purpose::Normal, 0) else {
            eprintln!("tuna-shell-gtk: no on-screen keyboard layout loads");
            return;
        };
        let window = gtk::Window::new();
        window.add_css_class("tuna-osk");
        window.set_title(Some("On-Screen Keyboard"));
        window.init_layer_shell();
        window.set_layer(Layer::Overlay);
        window.set_namespace(Some(tuna_shell_control::OSK_NAMESPACE));
        window.set_anchor(Edge::Left, true);
        window.set_anchor(Edge::Right, true);
        window.set_anchor(Edge::Bottom, true);
        // Never the keyboard: the text field keeps it (and its input
        // method) while keys are pressed.
        window.set_keyboard_mode(KeyboardMode::None);
        window.set_focusable(false);
        let send = self.send.clone();
        let typing: Typing = Rc::new(move |effect| {
            for input in compositor_input(effect) {
                send(input);
            }
        });
        let weak = Rc::downgrade(self);
        let hide: Rc<dyn Fn()> = Rc::new(move || {
            if let Some(osk) = weak.upgrade() {
                osk.hide_key();
            }
        });
        let view = KeyboardView::new(model, typing, hide, Some(self.sources.clone()));
        let revealer = gtk::Revealer::new();
        revealer.set_transition_type(gtk::RevealerTransitionType::SlideUp);
        revealer.set_transition_duration(KEYBOARD_ANIMATION_TIME_MS);
        revealer.set_valign(gtk::Align::End);
        revealer.set_child(Some(view.widget()));
        window.set_child(Some(&revealer));
        let session = Rc::new(Session {
            window,
            revealer,
            view,
            presence: RefCell::new(Presence::new()),
            timer: RefCell::new(None),
        });
        {
            // `_animateHideComplete`: the box goes once the slide ends.
            let weak = Rc::downgrade(&session);
            session
                .revealer
                .connect_child_revealed_notify(move |revealer| {
                    if let Some(session) = weak.upgrade() {
                        if !revealer.reveals_child() && !revealer.is_child_revealed() {
                            session.window.set_visible(false);
                        }
                    }
                });
        }
        *self.session.borrow_mut() = Some(session);
    }

    fn hide_key(self: &Rc<Self>) {
        let Some(session) = self.session.borrow().clone() else {
            return;
        };
        let now = self.now();
        session.presence.borrow_mut().hide_key(now);
        self.session_changed(&session);
    }

    /// Apply the session keyboard's presence, and arm its next timer.
    fn session_changed(self: &Rc<Self>, session: &Rc<Session>) {
        let visible = session.presence.borrow().visible();
        if visible && !session.revealer.reveals_child() {
            self.relayout(session);
            session.view.opened();
            session.window.set_visible(true);
            session.revealer.set_reveal_child(true);
        } else if !visible && session.revealer.reveals_child() {
            session.view.closed();
            session.revealer.set_reveal_child(false);
            if !crate::motion::current().animations_enabled() {
                session.window.set_visible(false);
            }
        }
        if let Some(id) = session.timer.borrow_mut().take() {
            id.remove();
        }
        let Some(deadline) = session.presence.borrow().deadline() else {
            return;
        };
        let delay = deadline.saturating_sub(self.now());
        let (weak, osk) = (Rc::downgrade(session), Rc::downgrade(self));
        let id = glib::timeout_add_local_once(std::time::Duration::from_millis(delay), move || {
            let (Some(session), Some(osk)) = (weak.upgrade(), osk.upgrade()) else {
                return;
            };
            session.timer.borrow_mut().take();
            let now = osk.now();
            session.presence.borrow_mut().tick(now);
            osk.session_changed(&session);
        });
        *session.timer.borrow_mut() = Some(id);
    }

    /// `_relayout` on the keyboard's monitor.
    fn relayout(&self, session: &Session) {
        let monitor = gtk::gdk::Display::default().and_then(|display| {
            display
                .monitors()
                .item(0)
                .and_then(|m| m.downcast::<gtk::gdk::Monitor>().ok())
        });
        let Some(monitor) = monitor else { return };
        let geometry = monitor.geometry();
        let min = session
            .view
            .widget()
            .measure(gtk::Orientation::Vertical, geometry.width())
            .0;
        let height = keyboard_height(geometry.width(), geometry.height(), min);
        session.view.widget().set_size_request(-1, height);
        session.window.set_default_size(geometry.width(), height);
    }

    /// A lock window's keyboard for its unlock prompt: `prompt` yields the
    /// entry currently asking (hidden or visible, authPrompt.js
    /// `updateEntry`), or `None` once the prompt is gone.
    pub fn lock_keyboard(
        self: &Rc<Self>,
        prompt: Rc<dyn Fn() -> Option<gtk::Editable>>,
    ) -> Rc<LockKeyboard> {
        let model = Keyboard::new(&self.group(), Purpose::Password, PASSWORD_HINTS)
            .or_else(|| Keyboard::new("us", Purpose::Password, PASSWORD_HINTS));
        let typing: Typing = Rc::new(move |effect| {
            let Some(prompt) = prompt() else {
                return;
            };
            match prompt_edit(&effect) {
                Some(PromptEdit::Insert(text)) => {
                    prompt.delete_selection();
                    let mut position = prompt.position();
                    prompt.insert_text(&text, &mut position);
                    prompt.set_position(position);
                }
                Some(PromptEdit::Backspace) => {
                    if prompt.selection_bounds().is_some() {
                        prompt.delete_selection();
                    } else {
                        let position = prompt.position();
                        if position > 0 {
                            prompt.delete_text(position - 1, position);
                        }
                    }
                }
                Some(PromptEdit::Activate) if prompt.is_sensitive() => {
                    prompt.emit_by_name::<()>("activate", &[]);
                }
                Some(PromptEdit::Activate) => {}
                None => {}
            }
        });
        let lock = Rc::new_cyclic(|weak: &Weak<LockKeyboard>| {
            let hide_weak = weak.clone();
            let hide: Rc<dyn Fn()> = Rc::new(move || {
                if let Some(lock) = hide_weak.upgrade() {
                    lock.hide_key();
                }
            });
            let model = model.expect("the bundled us layout loads");
            let view = KeyboardView::new(model, typing, hide, Some(self.sources.clone()));
            let revealer = gtk::Revealer::new();
            revealer.add_css_class("tuna-osk");
            revealer.set_transition_type(gtk::RevealerTransitionType::SlideUp);
            revealer.set_transition_duration(KEYBOARD_ANIMATION_TIME_MS);
            revealer.set_valign(gtk::Align::End);
            revealer.set_halign(gtk::Align::Fill);
            revealer.set_child(Some(view.widget()));
            LockKeyboard {
                revealer,
                view,
                presence: RefCell::new(Presence::new()),
                timer: RefCell::new(None),
                osk: Rc::downgrade(self),
            }
        });
        lock.presence
            .borrow_mut()
            .set_enabled(self.enabled(), self.now());
        self.locks.borrow_mut().push(Rc::downgrade(&lock));
        lock
    }
}

impl LockKeyboard {
    /// The widget to overlay at the bottom of the lock window.
    pub fn widget(&self) -> &gtk::Revealer {
        &self.revealer
    }

    /// The keyboard's own keys, so the lock window's drag ignores them.
    pub fn keys(&self) -> &gtk::Box {
        self.view.widget()
    }

    fn now(&self) -> u64 {
        self.osk.upgrade().map_or(0, |osk| osk.now())
    }

    /// `_onKeyFocusChanged`: the unlock prompt gained or lost the focus.
    pub fn prompt_focus(self: &Rc<Self>, focused: bool) {
        let now = self.now();
        self.presence.borrow_mut().key_focus(focused, now);
        self.sync();
    }

    fn hide_key(self: &Rc<Self>) {
        let now = self.now();
        self.presence.borrow_mut().hide_key(now);
        self.sync();
    }

    fn sync(self: &Rc<Self>) {
        let visible = self.presence.borrow().visible();
        if visible && !self.revealer.reveals_child() {
            if let Some(window) = self.revealer.root() {
                let height = window.height();
                let width = window.width();
                let min = self
                    .view
                    .widget()
                    .measure(gtk::Orientation::Vertical, width)
                    .0;
                self.view
                    .widget()
                    .set_size_request(-1, keyboard_height(width, height, min));
            }
            self.view.opened();
            self.revealer.set_reveal_child(true);
        } else if !visible && self.revealer.reveals_child() {
            self.view.closed();
            self.revealer.set_reveal_child(false);
        }
        if let Some(id) = self.timer.borrow_mut().take() {
            id.remove();
        }
        let Some(deadline) = self.presence.borrow().deadline() else {
            return;
        };
        let delay = deadline.saturating_sub(self.now());
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(std::time::Duration::from_millis(delay), move || {
            let Some(lock) = weak.upgrade() else { return };
            lock.timer.borrow_mut().take();
            let now = lock.now();
            lock.presence.borrow_mut().tick(now);
            lock.sync();
        });
        *self.timer.borrow_mut() = Some(id);
    }
}

impl Drop for LockKeyboard {
    fn drop(&mut self) {
        if let Some(id) = self.timer.borrow_mut().take() {
            id.remove();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effects_become_compositor_input_like_keyboard_controller() {
        assert_eq!(
            compositor_input(Effect::Commit {
                text: "Q".into(),
                modifiers: vec![0xffe3],
            }),
            vec![OskInput::Commit {
                text: "Q".into(),
                modifiers: vec![0xffe3],
            }]
        );
        assert_eq!(
            compositor_input(Effect::Keyval(KEYVAL_RETURN)),
            vec![
                OskInput::Keyval {
                    keysym: KEYVAL_RETURN,
                    pressed: true,
                },
                OskInput::Keyval {
                    keysym: KEYVAL_RETURN,
                    pressed: false,
                },
            ]
        );
        assert_eq!(
            compositor_input(Effect::KeyvalPress(KEYVAL_BACKSPACE)),
            vec![OskInput::Keyval {
                keysym: KEYVAL_BACKSPACE,
                pressed: true,
            }]
        );
        for local in [Effect::Hide, Effect::LanguageMenu, Effect::ToggleEmoji] {
            assert!(compositor_input(local).is_empty());
        }
    }

    #[test]
    fn the_lock_keyboard_only_edits_its_prompt() {
        assert_eq!(
            prompt_edit(&Effect::Commit {
                text: "a".into(),
                modifiers: vec![0xffe3],
            }),
            Some(PromptEdit::Insert("a".into()))
        );
        assert_eq!(
            prompt_edit(&Effect::Keyval(KEYVAL_BACKSPACE)),
            Some(PromptEdit::Backspace)
        );
        assert_eq!(
            prompt_edit(&Effect::KeyvalPress(KEYVAL_BACKSPACE)),
            Some(PromptEdit::Backspace)
        );
        assert_eq!(prompt_edit(&Effect::KeyvalRelease(KEYVAL_BACKSPACE)), None);
        assert_eq!(
            prompt_edit(&Effect::Keyval(KEYVAL_RETURN)),
            Some(PromptEdit::Activate)
        );
        // Arrows, Tab and the rest never leave the prompt.
        assert_eq!(prompt_edit(&Effect::Keyval(0xff51)), None);
        assert_eq!(prompt_edit(&Effect::Keyval(0xff09)), None);
    }

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
        let layout = osk_layout::parse(include_str!("../osk-layouts/us.json")).unwrap();
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
        let sections = osk_layout::parse_emoji_sections(include_str!("../osk-layouts/emoji.json"))
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
