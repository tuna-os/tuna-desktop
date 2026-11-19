//! The keyring prompt's dialog: GNOME Shell 51's `KeyringDialog`
//! (`components/keyring.js`), a `prompt-dialog` modal over the dimmed
//! screen like the polkit agent's ([`crate::polkit`]): the message and
//! description, a password entry, a confirmation entry when the password
//! is new, a warning line, a choice check box and the cancel and continue
//! buttons. `ShellKeyringPrompt.complete` is here: a new password must be
//! typed twice the same.
//!
//! The dialog knows nothing of D-Bus. [`crate::keyring_prompter`] shows a
//! [`View`] and hears the user's [`Event`].

use std::cell::Cell;
use std::rc::Rc;

use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gdk, glib};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use zeroize::Zeroizing;

use crate::keyring_prompter::{Kind, View};

/// What the user did.
pub enum Event {
    /// Continue on a password prompt, with the choice check box.
    Password {
        secret: Zeroizing<String>,
        choice: bool,
    },
    /// Continue on a confirm prompt.
    Confirm { choice: bool },
    /// Cancel or Escape.
    Cancel,
    /// The dialog refused the input and says why (passwords differ).
    Warning(String),
}

pub const MISMATCH: &str = "Passwords do not match";
pub const BLANK: &str = "Password cannot be blank";

/// `shell_keyring_prompt_complete`'s checks on a new password: the two
/// entries match; with `GNOME_KEYRING_PARANOID` set it is not blank.
/// GNOME's own test there refuses every new password in paranoid mode,
/// whether or not it is blank; the check is applied as the message says.
pub fn check_new_password(
    password: &str,
    confirm: &str,
    paranoid: bool,
) -> Result<(), &'static str> {
    if password != confirm {
        return Err(MISMATCH);
    }
    if paranoid && password.is_empty() {
        return Err(BLANK);
    }
    Ok(())
}

fn paranoid() -> bool {
    std::env::var_os("GNOME_KEYRING_PARANOID").is_some_and(|v| !v.is_empty())
}

pub struct KeyringDialog {
    window: gtk::Window,
    fade: crate::transient::ModalFade,
    message: gtk::Label,
    description: gtk::Label,
    password: gtk::PasswordEntry,
    password_wiggle: crate::lock_wiggle::Wiggle,
    confirm: gtk::PasswordEntry,
    warning: gtk::Label,
    choice: gtk::CheckButton,
    cancel: gtk::Button,
    ok: gtk::Button,
    kind: Cell<Option<Kind>>,
    new_password: Cell<bool>,
    on_event: Rc<dyn Fn(Event)>,
}

fn entry(hint: &str) -> gtk::PasswordEntry {
    let entry = gtk::PasswordEntry::new();
    entry.add_css_class("prompt-dialog-password-entry");
    entry.set_halign(gtk::Align::Center);
    entry.set_property("placeholder-text", hint);
    entry.update_property(&[gtk::accessible::Property::Label(hint)]);
    crate::lockdown::bind_password_entry(&entry);
    entry
}

fn heading(class: &str) -> gtk::Label {
    let label = gtk::Label::new(None);
    label.add_css_class(class);
    label.set_wrap(true);
    label.set_max_width_chars(1);
    label.set_hexpand(true);
    label.set_justify(gtk::Justification::Center);
    label
}

impl KeyringDialog {
    pub fn new(app: &gtk::Application, on_event: Rc<dyn Fn(Event)>) -> Rc<Self> {
        let window = gtk::Window::new();
        window.set_application(Some(app));
        window.add_css_class("tuna-end-session");
        window.add_css_class("tuna-polkit");
        window.add_css_class("tuna-keyring-prompt");
        window.set_title(Some("Keyring Prompt"));
        window.init_layer_shell();
        window.set_layer(Layer::Overlay);
        window.set_namespace(Some(tuna_shell_control::KEYRING_PROMPT_NAMESPACE));
        for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
            window.set_anchor(edge, true);
        }
        window.set_exclusive_zone(-1);
        window.set_keyboard_mode(KeyboardMode::Exclusive);

        let card = gtk::Box::new(gtk::Orientation::Vertical, 18);
        card.add_css_class("modal-dialog");
        card.add_css_class("prompt-dialog");
        card.set_halign(gtk::Align::Center);
        card.set_valign(gtk::Align::Center);
        card.set_size_request(411, -1);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 18);
        content.add_css_class("modal-dialog-content-box");
        let header = gtk::Box::new(gtk::Orientation::Vertical, 18);
        let message = heading("message-dialog-title");
        let description = heading("message-dialog-description");
        header.append(&message);
        header.append(&description);
        content.append(&header);

        let fields = gtk::Box::new(gtk::Orientation::Vertical, 6);
        fields.add_css_class("prompt-dialog-password-layout");
        let password = entry("Password");
        let password_wiggle = crate::lock_wiggle::Wiggle::new(&password);
        password_wiggle.set_halign(gtk::Align::Center);
        let confirm = entry("Confirm Password");
        let warning = gtk::Label::new(None);
        warning.add_css_class("prompt-dialog-error-label");
        warning.set_wrap(true);
        warning.set_opacity(0.0);
        fields.append(&password_wiggle);
        fields.append(&confirm);
        fields.append(&warning);
        content.append(&fields);
        let choice = gtk::CheckButton::new();
        content.append(&choice);
        card.append(&content);

        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        buttons.add_css_class("modal-dialog-button-box");
        buttons.set_homogeneous(true);
        let cancel = gtk::Button::with_label("Cancel");
        cancel.add_css_class("modal-dialog-button");
        let ok = gtk::Button::with_label("Continue");
        ok.add_css_class("modal-dialog-button");
        buttons.append(&cancel);
        buttons.append(&ok);
        card.append(&buttons);
        window.set_child(Some(&card));

        let dialog = Rc::new(Self {
            fade: crate::transient::ModalFade::new(&window),
            window,
            message,
            description,
            password,
            password_wiggle,
            confirm,
            warning,
            choice,
            cancel,
            ok,
            kind: Cell::new(None),
            new_password: Cell::new(false),
            on_event,
        });
        let weak = Rc::downgrade(&dialog);
        dialog.cancel.connect_clicked({
            let weak = weak.clone();
            move |_| {
                if let Some(d) = weak.upgrade() {
                    d.cancelled();
                }
            }
        });
        dialog.ok.connect_clicked({
            let weak = weak.clone();
            move |_| {
                if let Some(d) = weak.upgrade() {
                    d.continued();
                }
            }
        });
        // Enter in the password goes on to the confirmation when there is
        // one, and continues otherwise; Enter in the confirmation continues.
        dialog.password.connect_activate({
            let weak = weak.clone();
            move |_| {
                if let Some(d) = weak.upgrade() {
                    if d.confirm.is_visible() {
                        d.confirm.grab_focus();
                    } else {
                        d.continued();
                    }
                }
            }
        });
        dialog.confirm.connect_activate({
            let weak = weak.clone();
            move |_| {
                if let Some(d) = weak.upgrade() {
                    d.continued();
                }
            }
        });
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gdk::Key::Escape {
                if let Some(d) = weak.upgrade() {
                    d.cancelled();
                }
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        dialog.window.add_controller(keys);
        dialog
    }

    fn emit(&self, event: Event) {
        (self.on_event)(event);
    }

    /// Show a prompt (`_onShowPassword` and `_onShowConfirm`): the labels,
    /// the entries empty and live.
    pub fn show(&self, view: &View) {
        let props = &view.props;
        for (label, text) in [
            (&self.message, &props.message),
            (&self.description, &props.description),
        ] {
            label.set_label(text);
            label.set_visible(!text.is_empty());
        }
        self.cancel.set_label(&props.cancel_label);
        self.ok.set_label(&props.continue_label);
        self.choice.set_label(Some(&props.choice_label));
        self.choice.set_visible(!props.choice_label.is_empty());
        self.choice.set_active(props.choice_chosen);
        self.set_warning(&props.warning);
        self.kind.set(Some(view.kind));
        self.new_password.set(props.password_new);
        let password = view.kind == Kind::Password;
        self.password_wiggle.set_visible(password);
        self.confirm.set_visible(password && props.password_new);
        self.password.set_text("");
        self.confirm.set_text("");
        self.set_sensitive(true);
        self.fade.present();
        if password {
            self.password.grab_focus();
        } else {
            self.ok.grab_focus();
        }
    }

    /// The warning line; a new one wiggles the password entry
    /// (`notify::warning` in keyring.js).
    fn set_warning(&self, text: &str) {
        self.warning.set_label(text);
        self.warning
            .set_opacity(if text.is_empty() { 0.0 } else { 1.0 });
        if !text.is_empty() && self.password_wiggle.is_visible() {
            self.password_wiggle.start();
        }
    }

    /// `_updateSensitivity`.
    fn set_sensitive(&self, sensitive: bool) {
        self.password.set_sensitive(sensitive);
        self.confirm.set_sensitive(sensitive);
        self.ok.set_sensitive(sensitive);
        self.ok.set_can_focus(sensitive);
    }

    /// `_onContinueButton`.
    fn continued(&self) {
        let Some(kind) = self.kind.get() else { return };
        let choice = self.choice.is_visible() && self.choice.is_active();
        if kind == Kind::Confirm {
            self.finish(Event::Confirm { choice });
            return;
        }
        let secret = Zeroizing::new(self.password.text().to_string());
        if self.new_password.get() {
            let confirm = Zeroizing::new(self.confirm.text().to_string());
            if let Err(why) = check_new_password(&secret, &confirm, paranoid()) {
                // Let the user correct it.
                self.set_warning(why);
                self.emit(Event::Warning(why.to_owned()));
                self.password.grab_focus();
                return;
            }
        }
        self.finish(Event::Password { secret, choice });
    }

    fn cancelled(&self) {
        if self.kind.get().is_some() {
            self.finish(Event::Cancel);
        }
    }

    /// The prompt is answered: the entries go empty and dead until the
    /// client asks again or stops.
    fn finish(&self, event: Event) {
        self.kind.set(None);
        self.set_sensitive(false);
        self.emit(event);
        self.password.set_text("");
        self.confirm.set_text("");
    }

    /// `_onHidePrompt`.
    pub fn close(&self) {
        self.kind.set(None);
        self.password.set_text("");
        self.confirm.set_text("");
        self.fade.hide();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_password_must_be_typed_twice_the_same() {
        assert_eq!(check_new_password("a", "a", false), Ok(()));
        assert_eq!(check_new_password("a", "b", false), Err(MISMATCH));
        assert_eq!(check_new_password("", "", false), Ok(()));
    }

    #[test]
    fn paranoid_mode_refuses_a_blank_password() {
        assert_eq!(check_new_password("", "", true), Err(BLANK));
        assert_eq!(check_new_password("x", "x", true), Ok(()));
        assert_eq!(check_new_password("x", "", true), Err(MISMATCH));
    }
}
