//! GNOME's DisplayChangeDialog (gnome-shell js/ui/windowManager.js):
//! "Keep these display settings?" after a persistent monitor change.
//! Revert Settings (Escape) and Keep Changes (default) answer the
//! compositor; at zero the dialog just closes, the compositor reverts on
//! its own timer.
use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk4 as gtk;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

type Answer = Rc<dyn Fn(bool)>;

/// GNOME's countdown description for `secs` remaining (ngettext).
pub fn countdown_text(secs: u32) -> String {
    if secs == 1 {
        format!("Settings changes will revert in {secs} second")
    } else {
        format!("Settings changes will revert in {secs} seconds")
    }
}

/// The pure countdown state: one tick per second.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Countdown {
    remaining: u32,
}

impl Countdown {
    pub fn new(timeout_secs: u32) -> Self {
        Self {
            remaining: timeout_secs,
        }
    }

    pub fn text(&self) -> String {
        countdown_text(self.remaining)
    }

    /// Advance one second: the new description, or `None` once the
    /// countdown reached zero and the dialog should close.
    pub fn tick(&mut self) -> Option<String> {
        self.remaining = self.remaining.saturating_sub(1);
        (self.remaining > 0).then(|| self.text())
    }
}

pub struct DisplayChange {
    window: gtk::Window,
    description: gtk::Label,
    keep: gtk::Button,
    timer: RefCell<Option<glib::SourceId>>,
    answer: Answer,
}

impl DisplayChange {
    pub fn new(app: &gtk::Application, answer: Answer) -> Rc<Self> {
        let window = gtk::Window::new();
        window.set_application(Some(app));
        window.set_title(Some("Keep these display settings?"));
        window.add_css_class("tuna-end-session");
        window.init_layer_shell();
        window.set_layer(Layer::Overlay);
        window.set_namespace(Some(tuna_shell_control::DISPLAY_CHANGE_NAMESPACE));
        for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
            window.set_anchor(edge, true);
        }
        window.set_exclusive_zone(-1);
        window.set_keyboard_mode(KeyboardMode::Exclusive);
        let card = gtk::Box::new(gtk::Orientation::Vertical, 18);
        card.add_css_class("modal-dialog");
        card.set_halign(gtk::Align::Center);
        card.set_valign(gtk::Align::Center);
        card.set_margin_top(24);
        card.set_margin_bottom(24);
        let title = gtk::Label::new(Some("Keep these display settings?"));
        title.add_css_class("message-dialog-title");
        title.set_wrap(true);
        title.set_max_width_chars(50);
        card.append(&title);
        let description = gtk::Label::new(None);
        description.add_css_class("message-dialog-description");
        description.set_wrap(true);
        card.append(&description);
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        buttons.add_css_class("modal-dialog-button-box");
        let revert = gtk::Button::with_label("Revert Settings");
        let keep = gtk::Button::with_label("Keep Changes");
        for button in [&revert, &keep] {
            button.add_css_class("modal-dialog-button");
            buttons.append(button);
        }
        keep.add_css_class("suggested-action");
        card.append(&buttons);
        window.set_child(Some(&card));
        window.set_default_widget(Some(&keep));
        let ui = Rc::new(Self {
            window,
            description,
            keep,
            timer: RefCell::new(None),
            answer,
        });
        let weak = Rc::downgrade(&ui);
        revert.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.respond(false);
            }
        });
        let weak = Rc::downgrade(&ui);
        ui.keep.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.respond(true);
            }
        });
        let keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(&ui);
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gtk::gdk::Key::Escape {
                if let Some(ui) = weak.upgrade() {
                    ui.respond(false);
                }
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        ui.window.add_controller(keys);
        let weak = Rc::downgrade(&ui);
        ui.window.connect_close_request(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.respond(false);
            }
            glib::Propagation::Stop
        });
        ui
    }

    /// Show (or restart) the dialog with a fresh `timeout_secs` countdown.
    pub fn show(self: &Rc<Self>, timeout_secs: u32) {
        self.close();
        if timeout_secs == 0 {
            return;
        }
        let countdown = RefCell::new(Countdown::new(timeout_secs));
        self.description.set_label(&countdown.borrow().text());
        let weak = Rc::downgrade(self);
        let source = glib::timeout_add_local(std::time::Duration::from_secs(1), move || {
            let Some(ui) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            match countdown.borrow_mut().tick() {
                Some(text) => {
                    ui.description.set_label(&text);
                    glib::ControlFlow::Continue
                }
                None => {
                    // The source ends itself; forget it before closing.
                    ui.timer.borrow_mut().take();
                    ui.close();
                    glib::ControlFlow::Break
                }
            }
        });
        *self.timer.borrow_mut() = Some(source);
        self.window.present();
        self.keep.grab_focus();
    }

    /// Hide without answering (countdown expiry, lock).
    pub fn close(&self) {
        if let Some(source) = self.timer.borrow_mut().take() {
            source.remove();
        }
        self.window.set_visible(false);
    }

    fn respond(&self, keep: bool) {
        if !self.window.is_visible() {
            return;
        }
        self.close();
        (self.answer)(keep);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn countdown_matches_gnome_and_closes_at_zero() {
        let mut countdown = Countdown::new(3);
        assert_eq!(
            countdown.text(),
            "Settings changes will revert in 3 seconds"
        );
        assert_eq!(
            countdown.tick().as_deref(),
            Some("Settings changes will revert in 2 seconds")
        );
        assert_eq!(
            countdown.tick().as_deref(),
            Some("Settings changes will revert in 1 second")
        );
        assert_eq!(countdown.tick(), None);
        assert_eq!(countdown.tick(), None);
        assert_eq!(
            Countdown::new(20).text(),
            "Settings changes will revert in 20 seconds"
        );
    }
}
