//! GNOME's prompt entries as GTK widgets, shared by the unlock dialog and
//! the login dialog (feature `gtk`).

use std::cell::Cell;

use gtk4 as gtk;
use gtk4::prelude::*;

/// GNOME's two prompt entries (authPrompt.js `_passwordEntry`,
/// `_textEntry`): one at a time in the entry area, swapped with the text
/// kept when a question changes between hidden and visible input
/// (`updateEntry`).
pub struct PromptEntry {
    pub area: gtk::Overlay,
    pub password: gtk::PasswordEntry,
    pub text: gtk::Entry,
    pub secret: Cell<bool>,
}

impl PromptEntry {
    pub fn current(&self) -> gtk::Widget {
        if self.secret.get() {
            self.password.clone().upcast()
        } else {
            self.text.clone().upcast()
        }
    }

    pub fn editable(&self) -> gtk::Editable {
        if self.secret.get() {
            self.password.clone().upcast()
        } else {
            self.text.clone().upcast()
        }
    }

    pub fn has_focus(&self) -> bool {
        let current = self.current();
        current
            .root()
            .and_then(|root| root.focus())
            .is_some_and(|focus| focus == current || focus.is_ancestor(&current))
    }

    /// Swap to the hidden or the visible entry (authPrompt.js
    /// `updateEntry`). Visible text moves into the hidden entry with its
    /// cursor and selection; hidden text is never revealed: going visible
    /// empties both.
    pub fn set_secret(&self, secret: bool) {
        if self.secret.get() == secret {
            return;
        }
        let (old, focused) = (self.editable(), self.has_focus());
        let carried = secret.then(|| (old.text(), old.position(), old.selection_bounds()));
        self.secret.set(secret);
        let new = self.editable();
        self.area.set_child(Some(&self.current()));
        match carried {
            Some((text, position, selection)) => {
                new.set_text(&text);
                new.set_position(position);
                if let Some((start, end)) = selection {
                    new.select_region(start, end);
                }
            }
            None => new.set_text(""),
        }
        // The inactive entry keeps no copy of what was typed.
        old.set_text("");
        if focused {
            self.focus();
        }
    }

    /// Give the shown entry the keyboard focus without selecting what is
    /// typed (Clutter's `grab_key_focus`). GtkText's `grab_focus` selects
    /// all whenever the focus was not inside it, including when it was
    /// the text itself, so a re-focus while typing would have the next
    /// key replace the start of the answer (the wake character).
    pub fn focus(&self) {
        match self
            .editable()
            .delegate()
            .and_then(|d| d.downcast::<gtk::Text>().ok())
        {
            Some(text) => {
                text.grab_focus_without_selecting();
            }
            None => {
                self.current().grab_focus();
            }
        }
    }

    /// The question as the hint and accessible label of both entries;
    /// unchanged, nothing is touched (no accessibility churn).
    pub fn set_hint(&self, hint: &str) {
        let current = self.password.property::<Option<String>>("placeholder-text");
        if current.as_deref() == Some(hint) && self.text.placeholder_text().as_deref() == Some(hint)
        {
            return;
        }
        self.password.set_property("placeholder-text", hint);
        self.text.set_placeholder_text(Some(hint));
        self.password
            .update_property(&[gtk::accessible::Property::Label(hint)]);
        self.text
            .update_property(&[gtk::accessible::Property::Label(hint)]);
    }

    pub fn set_sensitive(&self, sensitive: bool) {
        self.password.set_sensitive(sensitive);
        self.text.set_sensitive(sensitive);
    }

    pub fn clear(&self) {
        self.password.set_text("");
        self.text.set_text("");
    }
}
