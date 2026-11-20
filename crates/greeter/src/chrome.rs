//! Everything around the login dialog's prompt (#1164): the panel's
//! keyboard indicator and quick settings (`input.rs`, `quick.rs`), the
//! banner and logo (`branding.rs`), the accessibility menu and its
//! effects (`a11y.rs`), and the prompt's on-screen keyboard, over the
//! window's content.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use gtk4 as gtk;
use gtk4::prelude::*;
use tuna_auth_prompt::entry::PromptEntry;
use tuna_osk::prompt::PromptKeyboard;

use crate::branding::Branding;
use crate::control::Control;
use crate::input::InputSources;
use crate::quick::QuickSettings;

pub struct Chrome {
    /// Held for the process: the compositor's control socket and the
    /// prompt's keyboard live as long as the login screen.
    _control: Rc<Control>,
    pub input: Rc<InputSources>,
    pub quick: Rc<QuickSettings>,
    pub branding: Rc<Branding>,
    _keyboard: Rc<PromptKeyboard>,
    /// The accessibility menu button (the bottom-right group).
    pub a11y: gtk::MenuButton,
}

impl Chrome {
    /// `overlay` is the window's root over `content`; the on-screen
    /// keyboard and the end-session dialog sit on it.
    pub fn new(
        window: &gtk::Window,
        overlay: &gtk::Overlay,
        content: &gtk::Widget,
        entry: &Rc<PromptEntry>,
    ) -> Self {
        let control = Control::connect();
        let input = InputSources::new(control.clone());
        let quick = QuickSettings::new(overlay, content);
        let branding = Branding::new();
        // GNOME's keyboard box over the login dialog: it opens when the
        // prompt takes the focus and types into that prompt alone
        // (`_onKeyFocusChanged`), as on the lock screen.
        let prompt = Rc::downgrade(entry);
        let keyboard = PromptKeyboard::new(
            &input.current_id(),
            None,
            Rc::new(move || prompt.upgrade().map(|e| e.editable())),
            false,
        );
        overlay.add_overlay(keyboard.widget());
        let focus = Rc::new(RefCell::new(PromptFocus::default()));
        for (index, widget) in [
            entry.password.upcast_ref::<gtk::Widget>(),
            entry.text.upcast_ref(),
        ]
        .into_iter()
        .enumerate()
        {
            let controller = gtk::EventControllerFocus::new();
            let (enter_focus, enter) = (focus.clone(), Rc::downgrade(&keyboard));
            controller.connect_enter(move |_| entry_focus(&enter_focus, &enter, index, true));
            let (leave_focus, leave) = (focus.clone(), Rc::downgrade(&keyboard));
            controller.connect_leave(move |_| entry_focus(&leave_focus, &leave, index, false));
            widget.add_controller(controller);
        }
        input.connect_changed(follow_group(Rc::downgrade(&keyboard)));
        crate::a11y::follow(window, &control, &keyboard);
        Self {
            _control: control,
            input,
            quick,
            branding,
            _keyboard: keyboard,
            a11y: crate::a11y::menu_button(),
        }
    }

    /// The panel's right side (the `gdm` mode's `keyboard`,
    /// `quickSettings`).
    pub fn panel_end(&self) -> gtk::Box {
        let end = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        end.append(&self.input.button);
        end.append(&self.quick.button);
        end
    }
}

/// What the chrome tells the prompt's keyboard.
pub trait PromptKeys {
    /// `_onKeyFocusChanged`: the prompt gained or lost the key focus.
    fn prompt_focus(self: &Rc<Self>, focused: bool);
    /// `_onGroupChanged`: the current input source's layout.
    fn set_group(&self, group: &str);
}

impl PromptKeys for PromptKeyboard {
    fn prompt_focus(self: &Rc<Self>, focused: bool) {
        PromptKeyboard::prompt_focus(self, focused);
    }

    fn set_group(&self, group: &str) {
        PromptKeyboard::set_group(self, group);
    }
}

/// The prompt's two entries (hidden and visible text, authPrompt.js
/// `updateEntry`) as one focus target, as GNOME's keyboard follows the one
/// key focus: when the prompt swaps them, the keyboard ends up told the
/// prompt has the focus whichever order GTK reports the old entry's leave
/// and the new one's enter in (per-entry events would leave it closed
/// after an enter that came first).
#[derive(Default)]
struct PromptFocus([bool; 2]);

impl PromptFocus {
    /// Entry `entry` gained or lost the focus: the prompt's focus, when
    /// that changed it.
    fn set(&mut self, entry: usize, focused: bool) -> Option<bool> {
        let before = self.0.contains(&true);
        self.0[entry] = focused;
        let after = self.0.contains(&true);
        (before != after).then_some(after)
    }
}

/// Entry `entry`'s focus changed: tell the keyboard, while there is one,
/// when the prompt's did.
fn entry_focus<K: PromptKeys>(
    focus: &RefCell<PromptFocus>,
    keyboard: &Weak<K>,
    entry: usize,
    focused: bool,
) {
    let changed = focus.borrow_mut().set(entry, focused);
    if let (Some(focused), Some(keyboard)) = (changed, keyboard.upgrade()) {
        keyboard.prompt_focus(focused);
    }
}

/// The input source changed: the keyboard, while there is one, switches
/// to its layout.
fn follow_group<K: PromptKeys + 'static>(keyboard: Weak<K>) -> impl Fn(&str) + 'static {
    move |group| {
        if let Some(keyboard) = keyboard.upgrade() {
            keyboard.set_group(group);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Heard(RefCell<Vec<String>>);

    impl PromptKeys for Heard {
        fn prompt_focus(self: &Rc<Self>, focused: bool) {
            self.0.borrow_mut().push(format!("focus={focused}"));
        }

        fn set_group(&self, group: &str) {
            self.0.borrow_mut().push(format!("group={group}"));
        }
    }

    const PASSWORD: usize = 0;
    const TEXT: usize = 1;

    #[test]
    fn the_keyboard_follows_the_prompts_focus_whatever_the_entry_order() {
        let heard = Rc::new(Heard::default());
        let keyboard = Rc::downgrade(&heard);
        let focus = RefCell::new(PromptFocus::default());
        let event = |entry, focused| {
            entry_focus(&focus, &keyboard, entry, focused);
            heard.0.borrow_mut().drain(..).collect::<Vec<_>>()
        };
        assert_eq!(event(TEXT, true), ["focus=true"]);
        // The username prompt becomes the password prompt, leave first...
        assert_eq!(event(TEXT, false), ["focus=false"]);
        assert_eq!(event(PASSWORD, true), ["focus=true"]);
        // ...or enter first: the prompt keeps the focus throughout.
        assert!(event(TEXT, true).is_empty());
        assert!(event(PASSWORD, false).is_empty());
        // A repeated enter changes nothing.
        assert!(event(TEXT, true).is_empty());
        // The focus goes to the panel, then comes back.
        assert_eq!(event(TEXT, false), ["focus=false"]);
        assert!(event(TEXT, false).is_empty());
        assert_eq!(event(PASSWORD, true), ["focus=true"]);
        assert_eq!(event(PASSWORD, false), ["focus=false"]);
    }

    #[test]
    fn the_keyboard_follows_the_current_input_source() {
        let heard = Rc::new(Heard::default());
        let follow = follow_group(Rc::downgrade(&heard));
        follow("de");
        follow("us");
        assert_eq!(*heard.0.borrow(), ["group=de", "group=us"]);
    }

    #[test]
    fn a_dropped_keyboard_hears_nothing() {
        let heard = Rc::new(Heard::default());
        let keyboard = Rc::downgrade(&heard);
        let follow = follow_group(keyboard.clone());
        let focus = RefCell::new(PromptFocus::default());
        drop(heard);
        follow("de");
        entry_focus(&focus, &keyboard, PASSWORD, true);
        // The prompt's focus is still followed for a later keyboard.
        assert_eq!(focus.borrow_mut().set(PASSWORD, false), Some(false));
    }
}
