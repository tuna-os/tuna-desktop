//! gnome-shell 51's CloseDialog (`js/ui/closeDialog.js`, created by
//! `js/ui/windowManager.js` `_createCloseDialog` for mutter's
//! `meta_window_show_close_dialog`): over a window that stopped answering
//! pings, "“%s” Is Not Responding" with Force Quit (the default button,
//! Enter) and Wait (Escape). The compositor decides when it shows, moves
//! and hides (`crates/compositor/src/unresponsive.rs`; it also re-checks
//! the window every `ALIVE_TIMEOUT` while shown) and gets the answer as
//! `RespondCloseDialog`; the dialog hides as it answers
//! (`meta_close_dialog_response`).
//!
//! Divergences, stated plainly: gnome-shell parents the dialog to the
//! window actor, so it covers the window, stacks with it, dims only the
//! window's surface (a -0.3 `BrightnessContrastEffect`) and takes key
//! focus with the window (`vfunc_focus`). Here it is a layer surface over
//! the window's frame on the Top layer: it covers and dims the window
//! (30% black, which is what brightness -0.3 does) and follows it as the
//! compositor resends the frame, but it stays above windows stacked over
//! its window and takes the keyboard when clicked, not with its window.
//! Its 150 ms `EASE_OUT_BACK` scale-in is not drawn.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use gtk::prelude::*;
use gtk4 as gtk;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use tuna_shell_control::CloseDialogResponse;
use tuna_shell_host::control::CloseDialogRequest;

/// closeDialog.js `_createDialogContent` description.
pub const DESCRIPTION: &str =
    "You may choose to wait a short while for it to continue or force the app to quit entirely";
/// closeDialog.js default button.
pub const FORCE_QUIT: &str = "Force Quit";
/// closeDialog.js Escape button.
pub const WAIT: &str = "Wait";

/// closeDialog.js title, `_('“%s” Is Not Responding').format(name)`.
pub fn title(app_name: &str) -> String {
    format!("“{app_name}” Is Not Responding")
}

/// `Shell.WindowTracker`'s app name for the window
/// (`shell_app_get_name`): its desktop entry's name, else the
/// window-backed app's, which is the window's class (a Wayland app id),
/// else `C_("program", "Unknown")`.
pub fn app_name(entry_name: Option<&str>, app_id: Option<&str>) -> String {
    let named = |name: &&str| !name.is_empty();
    entry_name
        .filter(named)
        .or(app_id.filter(named))
        .unwrap_or("Unknown")
        .to_owned()
}

/// `Dialog.Dialog` key bindings: the default button's Return, KP_Enter
/// and ISO_Enter, and Wait's Escape, whichever button has focus.
pub fn response_for_key(key: gtk::gdk::Key) -> Option<CloseDialogResponse> {
    use gtk::gdk::Key;
    if [Key::Return, Key::KP_Enter, Key::ISO_Enter].contains(&key) {
        Some(CloseDialogResponse::ForceClose)
    } else if key == Key::Escape {
        Some(CloseDialogResponse::Wait)
    } else {
        None
    }
}

/// One compositor update for the dialogs, in arrival order.
pub enum Update {
    Show(CloseDialogRequest),
    Hide(u64),
}

/// The shell's answer: Force Quit or Wait for a window.
pub type Answer = Rc<dyn Fn(u64, CloseDialogResponse)>;

/// One shown dialog: a layer surface over its window's frame.
struct Dialog {
    window: gtk::Window,
    shade: gtk::Box,
    card: gtk::Box,
    title: gtk::Label,
    force_quit: gtk::Button,
}

/// Every shown close dialog, by window id.
pub struct CloseDialogs {
    app: gtk::Application,
    answer: Answer,
    shown: RefCell<HashMap<u64, Dialog>>,
    /// The control connection the shown dialogs came from.
    generation: Cell<Option<u64>>,
}

impl CloseDialogs {
    pub fn new(app: &gtk::Application, answer: Answer) -> Rc<Self> {
        Rc::new(Self {
            app: app.clone(),
            answer,
            shown: RefCell::default(),
            generation: Cell::new(None),
        })
    }

    /// A new control connection replays the dialogs still shown; the old
    /// connection's go first.
    pub fn sync_connection(&self, generation: Option<u64>) {
        if self.generation.replace(generation) != generation {
            self.hide_all();
        }
    }

    /// Apply the compositor's updates, in order.
    pub fn apply(self: &Rc<Self>, updates: Vec<Update>, name: impl Fn(Option<&str>) -> String) {
        for update in updates {
            match update {
                Update::Show(request) => {
                    let app = name(request.app_id.as_deref());
                    self.show(&request, &app);
                }
                Update::Hide(window) => self.hide(window),
            }
        }
    }

    /// Show the dialog over `request`'s window, or move it there.
    fn show(self: &Rc<Self>, request: &CloseDialogRequest, app_name: &str) {
        let mut shown = self.shown.borrow_mut();
        let dialog = shown
            .entry(request.window)
            .or_insert_with(|| self.build(request.window));
        let title = title(app_name);
        dialog.title.set_label(&title);
        dialog.card.update_property(&[
            gtk::accessible::Property::Label(&title),
            gtk::accessible::Property::Description(DESCRIPTION),
        ]);
        let monitor = request
            .connector
            .as_deref()
            .and_then(crate::pad_osd::monitor_for_connector);
        dialog.window.set_monitor(monitor.as_ref());
        dialog.window.set_margin(Edge::Left, request.x);
        dialog.window.set_margin(Edge::Top, request.y);
        dialog.shade.set_size_request(request.width, request.height);
        dialog
            .window
            .set_default_size(request.width, request.height);
        let first = !dialog.window.is_visible();
        dialog.window.present();
        if first {
            // `Dialog.addButton`: the default button holds initial focus.
            dialog.force_quit.grab_focus();
        }
    }

    /// Hide `window`'s dialog without answering.
    pub fn hide(&self, window: u64) {
        let dialog = self.shown.borrow_mut().remove(&window);
        if let Some(dialog) = dialog {
            dialog.window.destroy();
        }
    }

    /// Hide every dialog without answering (lock, lost connection).
    pub fn hide_all(&self) {
        let dialogs: Vec<Dialog> = self.shown.borrow_mut().drain().map(|(_, d)| d).collect();
        for dialog in dialogs {
            dialog.window.destroy();
        }
    }

    /// `meta_close_dialog_response`: hide, then answer.
    fn respond(&self, window: u64, response: CloseDialogResponse) {
        let Some(dialog) = self.shown.borrow_mut().remove(&window) else {
            return;
        };
        dialog.window.destroy();
        (self.answer)(window, response);
    }

    fn build(self: &Rc<Self>, id: u64) -> Dialog {
        let window = gtk::Window::new();
        window.set_application(Some(&self.app));
        window.set_title(Some("Not Responding"));
        window.add_css_class("tuna-close-dialog");
        window.init_layer_shell();
        window.set_layer(Layer::Top);
        window.set_namespace(Some(tuna_shell_control::CLOSE_DIALOG_NAMESPACE));
        window.set_anchor(Edge::Top, true);
        window.set_anchor(Edge::Left, true);
        // Margins count from the output's edge, under the top bar too.
        window.set_exclusive_zone(-1);
        window.set_keyboard_mode(KeyboardMode::OnDemand);
        // The window dimmed as by closeDialog.js FROZEN_WINDOW_BRIGHTNESS.
        let shade = gtk::Box::new(gtk::Orientation::Vertical, 0);
        shade.add_css_class("close-dialog-shade");
        let card = gtk::Box::new(gtk::Orientation::Vertical, 18);
        card.set_property("accessible-role", gtk::AccessibleRole::AlertDialog);
        card.add_css_class("modal-dialog");
        card.add_css_class("close-dialog");
        card.set_halign(gtk::Align::Center);
        card.set_valign(gtk::Align::Center);
        card.set_vexpand(true);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.add_css_class("message-dialog-content");
        let title = gtk::Label::new(None);
        title.add_css_class("message-dialog-title");
        title.set_wrap(true);
        title.set_justify(gtk::Justification::Center);
        title.set_max_width_chars(40);
        content.append(&title);
        let description = gtk::Label::new(Some(DESCRIPTION));
        description.add_css_class("message-dialog-description");
        description.set_wrap(true);
        description.set_justify(gtk::Justification::Center);
        description.set_max_width_chars(40);
        content.append(&description);
        card.append(&content);
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        buttons.set_homogeneous(true);
        buttons.add_css_class("modal-dialog-button-box");
        let force_quit = gtk::Button::with_label(FORCE_QUIT);
        let wait = gtk::Button::with_label(WAIT);
        for button in [&force_quit, &wait] {
            button.add_css_class("modal-dialog-button");
            button.set_hexpand(true);
            buttons.append(button);
        }
        force_quit.add_css_class("suggested-action");
        card.append(&buttons);
        shade.append(&card);
        window.set_child(Some(&shade));
        window.set_default_widget(Some(&force_quit));
        for (button, response) in [
            (&force_quit, CloseDialogResponse::ForceClose),
            (&wait, CloseDialogResponse::Wait),
        ] {
            let weak = Rc::downgrade(self);
            button.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.respond(id, response);
                }
            });
        }
        // Dialog.Dialog binds the keys for the whole dialog, so they
        // answer before the focused button sees them.
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, _| match response_for_key(key) {
            Some(response) => {
                if let Some(ui) = weak.upgrade() {
                    ui.respond(id, response);
                }
                glib::Propagation::Stop
            }
            None => glib::Propagation::Proceed,
        });
        window.add_controller(keys);
        // closeDialog.js answers Wait when the shell shuts down.
        let weak = Rc::downgrade(self);
        window.connect_close_request(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.respond(id, CloseDialogResponse::Wait);
            }
            glib::Propagation::Stop
        });
        Dialog {
            window,
            shade,
            card,
            title,
            force_quit,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_match_gnome_shell_51() {
        assert_eq!(title("Text Editor"), "“Text Editor” Is Not Responding");
        assert_eq!(
            DESCRIPTION,
            "You may choose to wait a short while for it to continue or force the app to quit entirely"
        );
        assert_eq!((FORCE_QUIT, WAIT), ("Force Quit", "Wait"));
    }

    #[test]
    fn app_name_follows_the_window_tracker() {
        assert_eq!(
            app_name(Some("Text Editor"), Some("org.gnome.TextEditor")),
            "Text Editor"
        );
        assert_eq!(app_name(None, Some("hung-tool")), "hung-tool");
        assert_eq!(app_name(Some(""), Some("hung-tool")), "hung-tool");
        assert_eq!(app_name(None, None), "Unknown");
        assert_eq!(app_name(None, Some("")), "Unknown");
    }

    #[test]
    fn enter_forces_quit_and_escape_waits() {
        for key in [
            gtk::gdk::Key::Return,
            gtk::gdk::Key::KP_Enter,
            gtk::gdk::Key::ISO_Enter,
        ] {
            assert_eq!(response_for_key(key), Some(CloseDialogResponse::ForceClose));
        }
        assert_eq!(
            response_for_key(gtk::gdk::Key::Escape),
            Some(CloseDialogResponse::Wait)
        );
        assert_eq!(response_for_key(gtk::gdk::Key::space), None);
    }
}
