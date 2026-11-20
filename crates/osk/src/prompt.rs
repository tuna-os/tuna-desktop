//! An authentication prompt's keyboard (the lock screen's and the login
//! screen's): opened by the prompt's focus (`_onKeyFocusChanged`) while
//! `screen-keyboard-enabled` is set, and editing only that prompt.
//! Nothing typed on it reaches the compositor or a session.

use std::cell::RefCell;
use std::rc::{Rc, Weak};
use std::time::Instant;

use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;

use crate::layout::Purpose;
use crate::model::{Effect, Keyboard, Presence};
use crate::view::{
    keyboard_height, KeyboardView, Languages, Typing, KEYBOARD_ANIMATION_TIME_MS, KEYVAL_BACKSPACE,
    KEYVAL_KP_ENTER, KEYVAL_RETURN, PASSWORD_HINTS,
};

/// An edit of the prompt from its keyboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptEdit {
    Insert(String),
    Backspace,
    Activate,
}

/// What a prompt's keyboard does to its prompt: insert committed text,
/// delete on BackSpace, submit on Enter; every other key (and the
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

/// A prompt's keyboard, to overlay at the bottom of its window.
pub struct PromptKeyboard {
    revealer: gtk::Revealer,
    view: KeyboardView,
    presence: RefCell<Presence>,
    timer: RefCell<Option<glib::SourceId>>,
    epoch: Instant,
}

impl PromptKeyboard {
    /// `group` is the current input source's layout; `prompt` yields the
    /// entry currently asking (hidden or visible, authPrompt.js
    /// `updateEntry`), or `None` once the prompt is gone; `enabled` is
    /// `screen-keyboard-enabled`.
    pub fn new(
        group: &str,
        languages: Option<Rc<dyn Languages>>,
        prompt: Rc<dyn Fn() -> Option<gtk::Editable>>,
        enabled: bool,
    ) -> Rc<Self> {
        let model = Keyboard::new(group, Purpose::Password, PASSWORD_HINTS)
            .or_else(|| Keyboard::new("us", Purpose::Password, PASSWORD_HINTS))
            .expect("the bundled us layout loads");
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
                Some(PromptEdit::Activate) | None => {}
            }
        });
        let keyboard = Rc::new_cyclic(|weak: &Weak<PromptKeyboard>| {
            let hide_weak = weak.clone();
            let hide: Rc<dyn Fn()> = Rc::new(move || {
                if let Some(keyboard) = hide_weak.upgrade() {
                    keyboard.hide_key();
                }
            });
            let view = KeyboardView::new(model, typing, hide, languages);
            let revealer = gtk::Revealer::new();
            revealer.add_css_class("tuna-osk");
            revealer.set_transition_type(gtk::RevealerTransitionType::SlideUp);
            revealer.set_transition_duration(KEYBOARD_ANIMATION_TIME_MS);
            revealer.set_valign(gtk::Align::End);
            revealer.set_halign(gtk::Align::Fill);
            revealer.set_child(Some(view.widget()));
            PromptKeyboard {
                revealer,
                view,
                presence: RefCell::new(Presence::new()),
                timer: RefCell::new(None),
                epoch: Instant::now(),
            }
        });
        let now = keyboard.now();
        keyboard.presence.borrow_mut().set_enabled(enabled, now);
        keyboard
    }

    /// The widget to overlay at the bottom of the prompt's window.
    pub fn widget(&self) -> &gtk::Revealer {
        &self.revealer
    }

    /// The keyboard's own keys, so the window's drag ignores them.
    pub fn keys(&self) -> &gtk::Box {
        self.view.widget()
    }

    fn now(&self) -> u64 {
        self.epoch
            .elapsed()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX)
    }

    /// `screen-keyboard-enabled` (or touch-mode auto-show) changed.
    pub fn set_enabled(self: &Rc<Self>, enabled: bool) {
        let now = self.now();
        self.presence.borrow_mut().set_enabled(enabled, now);
        self.sync();
    }

    /// The current input source changed (`_onGroupChanged`).
    pub fn set_group(&self, group: &str) {
        self.view.set_group(group);
    }

    /// `_onKeyFocusChanged`: the prompt gained or lost the focus.
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
            let Some(keyboard) = weak.upgrade() else {
                return;
            };
            keyboard.timer.borrow_mut().take();
            let now = keyboard.now();
            keyboard.presence.borrow_mut().tick(now);
            keyboard.sync();
        });
        *self.timer.borrow_mut() = Some(id);
    }
}

impl Drop for PromptKeyboard {
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
    fn the_prompt_keyboard_only_edits_its_prompt() {
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
}
