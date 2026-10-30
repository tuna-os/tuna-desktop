//! GNOME 51's lock screen (`unlockDialog.js`) on ext-session-lock-v1.
//!
//! The compositor owns the lock: it decides when the session is locked,
//! draws the blurred, dimmed wallpaper underneath, and verifies the
//! password. This module only draws GNOME's unlock dialog: the clock
//! curtain (clock, date and "Click or press a key") and the unlock
//! prompt, moved between on GNOME's transition (`lock_curtain.rs`), and
//! sends what is typed in a redacted `Unlock` command. It releases its
//! lock surfaces once the compositor's snapshot says the session is
//! unlocked, never on its own.
//!
//! Measurements are from GNOME Shell 51's own lock screen at 1280x800
//! (docs/gnome-parity.md, states 11 and 11b).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use gtk4_session_lock as session_lock;

use crate::lock_curtain::{self, Curtain, Page};
use crate::logic;

/// GNOME's wrong-password message (authPrompt.js).
const FAILED: &str = "Sorry, that didn't work. Please try again.";

/// Sends a password; returns the request id.
pub type SendUnlock = Rc<dyn Fn(String) -> Option<u64>>;

/// One monitor's lock window.
struct Screen {
    curtain: Curtain,
    clock: gtk::Label,
    date: gtk::Label,
    entry: gtk::PasswordEntry,
    message: gtk::Label,
    /// GNOME's on-screen keyboard for this prompt (#349), when enabled.
    _keyboard: Option<Rc<crate::osk::LockKeyboard>>,
}

pub struct LockUi {
    app: gtk::Application,
    instance: session_lock::Instance,
    send: SendUnlock,
    screens: RefCell<Vec<Screen>>,
    /// A lock was asked of the compositor and not yet released.
    requested: Cell<bool>,
    /// The password check in flight, by request id.
    pending: Cell<Option<u64>>,
    connection_generation: Cell<Option<u64>>,
    clock_format: Box<dyn Fn() -> logic::ClockFormat>,
    /// Wakes the clock on the minute while the lock screens are up.
    wall_clock: RefCell<Option<crate::wall_clock::WallClock>>,
    /// GNOME's on-screen keyboard, whose lock-screen keys edit only the
    /// unlock prompt (#349).
    osk: RefCell<Option<Rc<crate::osk::Osk>>>,
}

impl LockUi {
    pub fn new(
        app: &gtk::Application,
        send: SendUnlock,
        clock_format: Box<dyn Fn() -> logic::ClockFormat>,
    ) -> Rc<Self> {
        let ui = Rc::new(Self {
            app: app.clone(),
            instance: session_lock::Instance::new(),
            send,
            screens: RefCell::new(Vec::new()),
            requested: Cell::new(false),
            pending: Cell::new(None),
            connection_generation: Cell::new(None),
            clock_format,
            wall_clock: RefCell::new(None),
            osk: RefCell::new(None),
        });
        {
            let weak = Rc::downgrade(&ui);
            ui.instance.connect_failed(move |_| {
                eprintln!("tuna-shell-gtk: the compositor refused the session lock");
                if let Some(ui) = weak.upgrade() {
                    ui.screens.borrow_mut().clear();
                    ui.wall_clock.take();
                }
            });
        }
        {
            let weak = Rc::downgrade(&ui);
            ui.instance.connect_unlocked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.screens.borrow_mut().clear();
                    ui.wall_clock.take();
                    ui.pending.set(None);
                }
            });
        }
        ui
    }

    /// Give each lock window GNOME's on-screen keyboard for its prompt.
    pub fn set_osk(&self, osk: Rc<crate::osk::Osk>) {
        *self.osk.borrow_mut() = Some(osk);
    }

    /// Follow the compositor's lock flag: lock when it says locked,
    /// release when it says unlocked.
    pub fn sync(self: &Rc<Self>, locked: bool) {
        if locked && !self.requested.get() {
            self.requested.set(true);
            if !self.instance.lock() {
                eprintln!("tuna-shell-gtk: session lock unavailable");
                return;
            }
            // One lock window per monitor, assigned right after lock()
            // (gtk4-layer-shell 1.1's contract).
            let Some(display) = gtk::gdk::Display::default() else {
                return;
            };
            let monitors = display.monitors();
            for i in 0..monitors.n_items() {
                let Some(monitor) = monitors
                    .item(i)
                    .and_then(|m| m.downcast::<gtk::gdk::Monitor>().ok())
                else {
                    continue;
                };
                let window = self.screen(&self.app.clone());
                self.instance.assign_window_to_monitor(&window, &monitor);
                window.present();
            }
            self.refresh_clock();
            let weak = Rc::downgrade(self);
            *self.wall_clock.borrow_mut() =
                Some(crate::wall_clock::WallClock::new(false, move || {
                    if let Some(ui) = weak.upgrade() {
                        ui.refresh_clock();
                    }
                }));
        } else if !locked && self.requested.get() {
            self.requested.set(false);
            self.instance.unlock();
        }
    }

    /// A transport generation change aborts local entry only. It never releases
    /// the lock surface; only the authoritative locked=false state does that.
    pub fn sync_connection_generation(&self, generation: Option<u64>) {
        if !generation_changed(&self.connection_generation, generation) {
            return;
        }
        self.pending.set(None);
        for screen in self.screens.borrow().iter() {
            screen.curtain.show_page(Page::Prompt);
            reset_prompt(&screen.entry, &screen.message);
        }
    }

    /// A command result arrived: the answer to our password, if it is
    /// the one in flight.
    pub fn command_result(&self, id: u64, applied: bool) {
        if self.pending.get() != Some(id) {
            return;
        }
        self.pending.set(None);
        if std::env::var_os("TUNA_LOCK_TRACE").is_some() {
            eprintln!("tuna-shell-gtk: lock command result applied={applied} request_id={id}");
        }
        for screen in self.screens.borrow().iter() {
            screen.entry.set_sensitive(true);
            if !applied {
                screen.curtain.show_page(Page::Prompt);
                screen.entry.set_text("");
                screen.message.set_label(FAILED);
                screen.entry.grab_focus();
            }
        }
    }

    /// Clock and date, as GNOME's UnlockDialogClock shows them (on each
    /// minute while locked, and when the clock format changes).
    pub fn refresh_clock(&self) {
        let now = jiff::Zoned::now().datetime();
        let time = logic::lock_clock_text(&now, (self.clock_format)());
        let date = logic::lock_date_text(now.date());
        for screen in self.screens.borrow().iter() {
            screen.clock.set_label(&time);
            screen.date.set_label(&date);
        }
    }

    /// Build one monitor's lock window: GNOME's unlock-screen top bar
    /// over the clock curtain and the prompt.
    fn screen(self: &Rc<Self>, app: &gtk::Application) -> gtk::Window {
        let window = gtk::Window::new();
        window.set_application(Some(app));
        window.add_css_class("tuna-lock");
        window.set_title(Some("Lock Screen"));

        // The clock page (unlock-dialog-clock).
        let clock = gtk::Label::new(None);
        clock.add_css_class("unlock-dialog-clock-time");
        let date = gtk::Label::new(None);
        date.add_css_class("unlock-dialog-clock-date");
        let hint = gtk::Label::new(Some("Click or press a key"));
        hint.add_css_class("unlock-dialog-clock-hint");
        hint.set_halign(gtk::Align::Center);
        let clock_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        clock_box.add_css_class("unlock-dialog-clock");
        clock_box.set_halign(gtk::Align::Center);
        clock_box.set_valign(gtk::Align::Start);
        clock_box.append(&clock);
        clock_box.append(&date);
        clock_box.append(&hint);

        // The prompt page (login-dialog-prompt-layout).
        let avatar = gtk::Image::from_icon_name("avatar-default-symbolic");
        avatar.add_css_class("user-icon");
        avatar.set_halign(gtk::Align::Center);
        let name = gtk::Label::new(Some(&logic::real_name()));
        name.add_css_class("user-widget-label");
        // GNOME keeps the Back button's room but shows it only from a
        // second PAM step on; a password is one step, so it stays
        // invisible (authPrompt.js `_updateCancelButton`).
        let cancel = gtk::Button::from_icon_name("go-previous-symbolic");
        cancel.add_css_class("cancel-button");
        cancel.set_valign(gtk::Align::Center);
        cancel.set_opacity(0.0);
        cancel.set_can_target(false);
        cancel.set_can_focus(false);
        cancel.update_property(&[gtk::accessible::Property::Label("Back")]);
        let entry = gtk::PasswordEntry::new();
        entry.add_css_class("login-dialog-prompt-entry");
        entry.set_hexpand(true);
        // GNOME's StPasswordEntry shows its peek icon unless
        // `disable-show-password` denies, live (#357).
        crate::lockdown::bind_password_entry(&entry);
        entry.set_property("placeholder-text", "Password");
        entry.update_property(&[gtk::accessible::Property::Label("Password")]);
        // Handle Return before the internal text/IM delegate: a password
        // field must submit even when that delegate consumes activation.
        // Use the same signal as the arrow, so pending checks and PAM
        // verification stay in the single handler below. This controller
        // only sees keys focused within the entry, not other lock controls.
        let submit_keys = gtk::EventControllerKey::new();
        submit_keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        {
            let entry = entry.downgrade();
            submit_keys.connect_key_pressed(move |_, key, _, _| {
                if matches!(
                    key,
                    gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter | gtk::gdk::Key::ISO_Enter
                ) {
                    if let Some(entry) = entry.upgrade().filter(|entry| entry.is_sensitive()) {
                        entry.emit_by_name::<()>("activate", &[]);
                    }
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            });
        }
        entry.add_controller(submit_keys);
        // The Submit arrow sits inside the entry's right end.
        let next = gtk::Button::from_icon_name("go-next-symbolic");
        next.add_css_class("next-button");
        next.set_halign(gtk::Align::End);
        next.set_valign(gtk::Align::Center);
        next.set_can_focus(false);
        next.update_property(&[gtk::accessible::Property::Label("Submit")]);
        {
            let entry = entry.clone();
            next.connect_clicked(move |_| {
                entry.emit_by_name::<()>("activate", &[]);
            });
        }
        let entry_area = gtk::Overlay::new();
        entry_area.add_css_class("login-dialog-prompt-entry-area");
        entry_area.set_hexpand(true);
        entry_area.set_child(Some(&entry));
        entry_area.add_overlay(&next);
        // Mirrors the Back button so the entry stays centred.
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.add_css_class("button-box-spacer");
        let button_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        button_box.add_css_class("login-dialog-button-box");
        button_box.append(&cancel);
        button_box.append(&entry_area);
        button_box.append(&spacer);
        let caps = gtk::Label::new(None);
        caps.add_css_class("caps-lock-warning-label");
        let message = gtk::Label::new(None);
        message.add_css_class("login-dialog-message");
        message.set_wrap(true);
        message.set_justify(gtk::Justification::Center);
        message.set_yalign(0.0);
        let prompt = gtk::Box::new(gtk::Orientation::Vertical, 0);
        prompt.add_css_class("login-dialog-prompt-layout");
        prompt.set_halign(gtk::Align::Center);
        prompt.set_valign(gtk::Align::Start);
        prompt.append(&avatar);
        prompt.append(&name);
        prompt.append(&button_box);
        prompt.append(&caps);
        prompt.append(&message);

        // The clock curtain over the prompt, on GNOME's transition.
        let curtain = Curtain::new(&clock_box, &prompt);
        curtain.set_vexpand(true);

        // GNOME's #panel.unlock-screen: transparent, the system menu's
        // power icon alone at the right.
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        bar.add_css_class("unlock-screen-panel");
        let power = gtk::Image::from_icon_name("system-shutdown-symbolic");
        power.add_css_class("system-status-icon");
        power.set_hexpand(true);
        power.set_halign(gtk::Align::End);
        bar.append(&power);

        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&curtain));
        bar.set_valign(gtk::Align::Start);
        overlay.add_overlay(&bar);
        // GNOME's keyboard box sits over the lock screen too: it opens
        // when the prompt takes the focus and types into that prompt
        // alone (`_onKeyFocusChanged`).
        let keyboard = self
            .osk
            .borrow()
            .as_ref()
            .map(|osk| osk.lock_keyboard(&entry));
        if let Some(keyboard) = &keyboard {
            overlay.add_overlay(keyboard.widget());
            let focus = gtk::EventControllerFocus::new();
            {
                let keyboard = Rc::downgrade(keyboard);
                focus.connect_enter(move |_| {
                    if let Some(keyboard) = keyboard.upgrade() {
                        keyboard.prompt_focus(true);
                    }
                });
            }
            {
                let keyboard = Rc::downgrade(keyboard);
                focus.connect_leave(move |_| {
                    if let Some(keyboard) = keyboard.upgrade() {
                        keyboard.prompt_focus(false);
                    }
                });
            }
            entry.add_controller(focus);
        }
        window.set_child(Some(&overlay));

        // Vertical placement: GNOME puts the stack a third of the way
        // down (UnlockDialogLayout), which lands the 1280x800 clock at
        // y 275 and the prompt at y 205.
        {
            let (clock_box, prompt) = (clock_box.clone(), prompt.clone());
            let last = Cell::new(-1);
            window.add_tick_callback(move |w, _| {
                let height = w.height();
                if height != last.get() {
                    last.set(height);
                    let (clock_top, prompt_top) = logic::lock_offsets(height);
                    clock_box.set_margin_top(clock_top);
                    prompt.set_margin_top(prompt_top);
                }
                glib::ControlFlow::Continue
            });
        }

        // GNOME's unlockDialog.js: any key but Shift or Caps Lock, a click
        // or a wheel step down lifts the curtain, a printable key starting
        // the password with it; Escape on the prompt, or a wheel step up,
        // brings the clock back. A vertical drag (pointer, or touch, which
        // the session delivers as the primary button) moves the curtain.
        let show_prompt = {
            let (curtain, entry) = (curtain.clone(), entry.clone());
            move || {
                curtain.show_page(Page::Prompt);
                entry.grab_focus();
            }
        };
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        {
            let (curtain, entry, message, show_prompt) = (
                curtain.clone(),
                entry.clone(),
                message.clone(),
                show_prompt.clone(),
            );
            keys.connect_key_pressed(move |_, key, _, _| {
                if key == gtk::gdk::Key::Escape && curtain.active() == Page::Prompt {
                    entry.set_text("");
                    message.set_label("");
                    curtain.show_page(Page::Clock);
                    return glib::Propagation::Stop;
                }
                // A prompt on screen, even on its way off, takes the keys
                // itself (`_onKeyPress`).
                if curtain.active() == Page::Prompt || curtain.prompt_shown() {
                    if key == gtk::gdk::Key::Escape {
                        return glib::Propagation::Stop;
                    }
                    if matches!(
                        key,
                        gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter | gtk::gdk::Key::ISO_Enter
                    ) && std::env::var_os("TUNA_LOCK_TRACE").is_some()
                    {
                        eprintln!(
                            "tuna-shell-gtk: lock Return received sensitive={}",
                            entry.is_sensitive()
                        );
                    }
                    return glib::Propagation::Proceed;
                }
                // Shift and Caps Lock alone do not lift the curtain.
                if matches!(
                    key,
                    gtk::gdk::Key::Shift_L
                        | gtk::gdk::Key::Shift_R
                        | gtk::gdk::Key::Shift_Lock
                        | gtk::gdk::Key::Caps_Lock
                ) {
                    return glib::Propagation::Proceed;
                }
                show_prompt();
                if let Some(c) = key.to_unicode().filter(|c| !c.is_control()) {
                    entry.set_text(&c.to_string());
                    entry.set_position(-1);
                }
                glib::Propagation::Stop
            });
        }
        window.add_controller(keys);
        // `Clutter.ClickGesture` recognizes on release, so a drag that
        // moved the curtain is never also a click.
        let click = gtk::GestureClick::new();
        {
            let (curtain, show_prompt) = (curtain.clone(), show_prompt.clone());
            click.connect_released(move |_, _, _, _| {
                if curtain.active() != Page::Prompt {
                    show_prompt();
                }
            });
        }
        window.add_controller(click);
        // `SwipeTracker`'s `Clutter.PanGesture`: a vertical pointer or
        // touch drag moves the curtain once past its threshold, and the
        // release velocity picks the page.
        let drag = gtk::GestureDrag::new();
        {
            let pan: Rc<RefCell<Option<lock_curtain::Pan>>> = Rc::default();
            let distance = Rc::new(Cell::new(1.0));
            {
                let pan = pan.clone();
                let keys = keyboard.as_ref().map(|k| k.keys().downgrade());
                drag.connect_drag_begin(move |gesture, x, y| {
                    // The keyboard's keys are not the curtain.
                    let on_keys = keys.as_ref().and_then(|k| k.upgrade()).is_some_and(|keys| {
                        gesture
                            .widget()
                            .and_then(|w| w.pick(x, y, gtk::PickFlags::DEFAULT))
                            .is_some_and(|picked| {
                                picked == *keys.upcast_ref::<gtk::Widget>()
                                    || picked.is_ancestor(&keys)
                            })
                    });
                    if on_keys {
                        gesture.set_state(gtk::EventSequenceState::Denied);
                        return;
                    }
                    *pan.borrow_mut() = Some(lock_curtain::Pan::new(gesture.current_event_time()));
                });
            }
            {
                let (pan, distance, curtain) = (pan.clone(), distance.clone(), curtain.clone());
                drag.connect_drag_update(move |gesture, _, offset_y| {
                    let time = gesture.current_event_time();
                    let Some(delta) = pan
                        .borrow_mut()
                        .as_mut()
                        .and_then(|pan| pan.motion(offset_y, time))
                    else {
                        return;
                    };
                    if !curtain.swiping() {
                        gesture.set_state(gtk::EventSequenceState::Claimed);
                        distance.set(curtain.swipe_begin("drag"));
                    }
                    // Up lifts the curtain (`_getGestureDirFactor`).
                    curtain.swipe_update(-delta, distance.get());
                });
            }
            {
                let (pan, distance, curtain, entry) = (
                    pan.clone(),
                    distance.clone(),
                    curtain.clone(),
                    entry.clone(),
                );
                drag.connect_drag_end(move |gesture, _, _| {
                    let time = gesture.current_event_time();
                    let Some(pan) = pan.borrow_mut().take() else {
                        return;
                    };
                    if !pan.recognized() {
                        return;
                    }
                    let velocity = -pan.velocity(time);
                    if curtain.swipe_end("drag", velocity, distance.get(), false)
                        == Some(Page::Prompt)
                    {
                        entry.grab_focus();
                    }
                });
            }
            {
                let curtain = curtain.clone();
                drag.connect_cancel(move |_, _| {
                    let Some(pan) = pan.borrow_mut().take() else {
                        return;
                    };
                    if pan.recognized() {
                        curtain.swipe_end("drag", 0.0, distance.get(), true);
                    }
                });
            }
        }
        window.add_controller(drag);
        // Wheel steps switch pages, GNOME's discrete `ScrollController`
        // (down the prompt, up the clock). Touchpad scrolls stay still:
        // GNOME's `ScrollGesture` follows them and settles when the
        // fingers lift, and the session forwards no axis stop to say so.
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        {
            let wheel = Rc::new(Cell::new(0.0));
            let (curtain, show_prompt) = (curtain.clone(), show_prompt.clone());
            scroll.connect_scroll(move |controller, _, dy| {
                if controller.unit() != gtk::gdk::ScrollUnit::Wheel {
                    return glib::Propagation::Proceed;
                }
                let steps = wheel.get() + dy;
                if steps.abs() < 1.0 {
                    wheel.set(steps);
                    return glib::Propagation::Stop;
                }
                wheel.set(0.0);
                if steps > 0.0 {
                    show_prompt();
                } else {
                    curtain.show_page(Page::Clock);
                }
                glib::Propagation::Stop
            });
        }
        window.add_controller(scroll);
        {
            // GNOME destroys the prompt once the clock is back
            // (`_maybeDestroyAuthPrompt`): nothing typed outlives it.
            let (entry, message) = (entry.downgrade(), message.downgrade());
            let lock_keyboard = keyboard.as_ref().map(Rc::downgrade);
            curtain.connect_settled(move |page| {
                if page != Page::Clock {
                    return;
                }
                // The prompt is gone: so is its keyboard.
                if let Some(keyboard) = lock_keyboard.as_ref().and_then(|k| k.upgrade()) {
                    keyboard.prompt_focus(false);
                }
                if let Some(entry) = entry.upgrade() {
                    entry.set_text("");
                }
                if let Some(message) = message.upgrade() {
                    message.set_label("");
                }
            });
        }
        {
            let (curtain, entry, message) = (curtain.clone(), entry.clone(), message.clone());
            cancel.connect_clicked(move |_| {
                entry.set_text("");
                message.set_label("");
                curtain.show_page(Page::Clock);
            });
        }
        {
            let weak = Rc::downgrade(self);
            let message = message.clone();
            entry.connect_activate(move |entry| {
                let Some(ui) = weak.upgrade() else { return };
                submit_password(entry, &message, &ui.pending, &ui.send);
            });
        }
        // GNOME's caps-lock warning under the entry.
        if let Some(display) = gtk::gdk::Display::default() {
            if let Some(seat) = display.default_seat() {
                if let Some(keyboard) = seat.keyboard() {
                    let update = {
                        let caps = caps.clone();
                        move |k: &gtk::gdk::Device| {
                            caps.set_label("Caps lock is on.");
                            caps.set_visible(k.is_caps_locked());
                        }
                    };
                    update(&keyboard);
                    keyboard.connect_caps_lock_state_notify(update);
                }
            }
        }

        self.screens.borrow_mut().push(Screen {
            curtain,
            clock,
            date,
            entry,
            message,
            _keyboard: keyboard,
        });
        window
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        // Never leave a typed password behind in a dead window.
        self.entry.set_text("");
    }
}

fn generation_changed(current: &Cell<Option<u64>>, next: Option<u64>) -> bool {
    let previous = current.replace(next);
    previous.is_some() && previous != next
}
fn reset_prompt(entry: &gtk::PasswordEntry, message: &gtk::Label) {
    entry.set_text("");
    entry.set_sensitive(true);
    entry.grab_focus();
    message.set_label(FAILED);
}
fn submit_password(
    entry: &gtk::PasswordEntry,
    message: &gtk::Label,
    pending: &Cell<Option<u64>>,
    send: &SendUnlock,
) {
    if pending.get().is_some() {
        return;
    }
    if std::env::var_os("TUNA_LOCK_TRACE").is_some() {
        eprintln!("tuna-shell-gtk: lock password submitted");
    }
    let password = entry.text().to_string();
    message.set_label("");
    if let Some(root) = entry.root() {
        root.set_focus(gtk::Widget::NONE);
    }
    entry.set_sensitive(false);
    match send(password) {
        Some(id) => pending.set(Some(id)),
        None => reset_prompt(entry, message),
    }
}
#[cfg(test)]
mod auth_transport_ui_tests {
    use super::*;
    #[test]
    #[ignore = "requires admitted Xvfb GTK proof"]
    fn actual_gtk_auth_send_failure_and_generation_reset() {
        assert!(
            std::env::var_os("TUNA_REQUIRE_AUTH_TRANSPORT_GTK").is_some(),
            "dedicated admission required"
        );
        gtk::init().expect("actual GTK display required");
        let entry = gtk::PasswordEntry::new();
        let message = gtk::Label::new(None);
        let pending = Rc::new(Cell::new(None));
        let next = Rc::new(Cell::new(None));
        let sends = Rc::new(Cell::new(0));
        let send: SendUnlock = {
            let next = next.clone();
            let sends = sends.clone();
            Rc::new(move |_password| {
                sends.set(sends.get() + 1);
                next.get()
            })
        };
        {
            let pending = pending.clone();
            let message = message.clone();
            let send = send.clone();
            entry.connect_activate(move |entry| submit_password(entry, &message, &pending, &send));
        }
        entry.set_text("controlled-only");
        entry.emit_by_name::<()>("activate", &[]);
        assert_eq!(entry.text(), "");
        assert!(entry.is_sensitive());
        assert_eq!(pending.get(), None);
        next.set(Some(9));
        entry.set_text("controlled-only");
        entry.emit_by_name::<()>("activate", &[]);
        assert_eq!(pending.get(), Some(9));
        assert!(!entry.is_sensitive());
        assert_eq!(sends.get(), 2);
        entry.emit_by_name::<()>("activate", &[]);
        assert_eq!(
            sends.get(),
            2,
            "stale captured activation while pending cannot send"
        );
        let generation = Cell::new(Some(0));
        let reset = gtk::Button::new();
        {
            let pending = pending.clone();
            let entry = entry.clone();
            let message = message.clone();
            reset.connect_clicked(move |_| {
                if generation_changed(&generation, Some(1)) {
                    pending.set(None);
                    reset_prompt(&entry, &message);
                }
            });
        }
        reset.emit_clicked();
        assert_eq!(pending.get(), None);
        assert_eq!(entry.text(), "");
        assert!(entry.is_sensitive());
        assert_eq!(sends.get(), 2, "reset itself must not submit credentials");
        entry.set_text("explicit-fresh-input");
        entry.emit_by_name::<()>("activate", &[]);
        assert_eq!(sends.get(), 3);
    }
}
