//! GNOME 51's lock screen (`unlockDialog.js`) on ext-session-lock-v1.
//!
//! The compositor owns the lock: it decides when the session is locked,
//! draws the blurred, dimmed wallpaper underneath, and runs the
//! authentication conversations (GDM's part). This module draws GNOME's
//! unlock dialog: the clock curtain (clock, date and the hint that fades
//! in once the session is idle) and the unlock prompt, moved between on
//! GNOME's transition by keys, clicks, drags and touchpad swipes and
//! scrolls (`lock_curtain.rs`), and drives GNOME's `AuthPrompt` flow over the
//! control protocol (`lock_auth.rs`): password and fingerprint side by
//! side, each PAM question in turn, GNOME's messages and Back. When the
//! password conversation is unavailable it falls back to one redacted
//! `Unlock` command. It releases its lock surfaces once the compositor's
//! snapshot says the session is unlocked, never on its own.
//!
//! Measurements are from GNOME Shell 51's own lock screen at 1280x800
//! (docs/gnome-parity.md, states 11 and 11b).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use gtk4_session_lock as session_lock;
use tuna_shell_control::{AuthEvent, AuthService};

use crate::lock_auth::{AuthMachine, Effect, Host, MessageKind, View};
use crate::lock_curtain::{self, Curtain, Page, Source};
use crate::lock_wiggle::Wiggle;
use crate::logic;

/// GNOME's touchpad settings, whose `natural-scroll` turns a swipe's
/// deltas round (swipeTracker.js `TouchpadSwipeGesture`); `None` without
/// the schema, where GNOME's default (natural) holds.
fn touchpad_settings() -> Option<gio::Settings> {
    const SCHEMA: &str = "org.gnome.desktop.peripherals.touchpad";
    gio::SettingsSchemaSource::default()?.lookup(SCHEMA, true)?;
    Some(gio::Settings::new(SCHEMA))
}

/// Sends a password with the single-password `Unlock`; returns the
/// request id.
pub type SendUnlock = Rc<dyn Fn(String) -> Option<u64>>;

/// The compositor's verification commands (`ControlClient`) and the
/// login-screen setting the flow reads. Each command returns its request
/// id, `None` when it could not be sent.
#[derive(Clone)]
pub struct AuthPort {
    pub unlock: SendUnlock,
    pub begin: Rc<dyn Fn(AuthService) -> Option<u64>>,
    pub answer: Rc<dyn Fn(AuthService, u32, String) -> Option<u64>>,
    pub cancel: Rc<dyn Fn() -> Option<u64>>,
    /// `org.gnome.login-screen enable-fingerprint-authentication`.
    pub fingerprint_enabled: Rc<dyn Fn() -> bool>,
}

impl Host for AuthPort {
    fn begin(&self, service: AuthService) -> Option<u64> {
        (self.begin)(service)
    }
    fn answer(&self, service: AuthService, sequence: u32, answer: String) -> Option<u64> {
        (self.answer)(service, sequence, answer)
    }
    fn cancel(&self) -> Option<u64> {
        (self.cancel)()
    }
    fn unlock(&self, password: String) -> Option<u64> {
        (self.unlock)(password)
    }
    fn fingerprint_enabled(&self) -> bool {
        (self.fingerprint_enabled)()
    }
    fn wiggle_ms(&self) -> u64 {
        crate::lock_wiggle::duration_ms()
    }
}

/// One trace line in a single write: the proof reads stderr shared with
/// the compositor, where pieces of separate writes interleave.
fn trace(line: &str) {
    if std::env::var_os("TUNA_LOCK_TRACE").is_some() {
        use std::io::Write;
        let line = format!("tuna-shell-gtk: {line}\n");
        let _ = std::io::stderr().lock().write_all(line.as_bytes());
    }
}

/// GNOME's two prompt entries (authPrompt.js `_passwordEntry`,
/// `_textEntry`): one at a time in the entry area, swapped with the text
/// kept when a question changes between hidden and visible input
/// (`updateEntry`).
struct PromptEntry {
    area: gtk::Overlay,
    password: gtk::PasswordEntry,
    text: gtk::Entry,
    secret: Cell<bool>,
}

impl PromptEntry {
    fn current(&self) -> gtk::Widget {
        if self.secret.get() {
            self.password.clone().upcast()
        } else {
            self.text.clone().upcast()
        }
    }

    fn editable(&self) -> gtk::Editable {
        if self.secret.get() {
            self.password.clone().upcast()
        } else {
            self.text.clone().upcast()
        }
    }

    fn has_focus(&self) -> bool {
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
    fn set_secret(&self, secret: bool) {
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
    fn focus(&self) {
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
    fn set_hint(&self, hint: &str) {
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

    fn set_sensitive(&self, sensitive: bool) {
        self.password.set_sensitive(sensitive);
        self.text.set_sensitive(sensitive);
    }

    fn clear(&self) {
        self.password.set_text("");
        self.text.set_text("");
    }
}

/// One monitor's lock window.
struct Screen {
    /// Destroyed after the screen (see [`LockUi::release_screens`]): the
    /// application would otherwise keep it, and with it a stale prompt in
    /// the accessibility tree.
    window: gtk::Window,
    curtain: Curtain,
    clock: gtk::Label,
    date: gtk::Label,
    entry: Rc<PromptEntry>,
    entry_wiggle: Wiggle,
    cancel: gtk::Button,
    next: gtk::Button,
    caps: gtk::Label,
    message: gtk::Label,
    /// GNOME's on-screen keyboard for this prompt (#349), when enabled.
    _keyboard: Option<Rc<crate::osk::LockKeyboard>>,
    message_wiggle: Wiggle,
    /// The clock's hint; the [`lock_curtain::HintWatch`] holds it weakly.
    _hint: Rc<lock_curtain::Hint>,
}

impl Screen {
    /// Show the shared prompt state on this monitor.
    fn apply(&self, view: &View) {
        self.entry.set_secret(view.secret);
        self.entry.set_hint(&view.hint);
        if !view.sensitive && self.entry.has_focus() {
            // `updateSensitivity(false)` gives the focus to the prompt.
            if let Some(root) = self.entry.current().root() {
                root.set_focus(gtk::Widget::NONE);
            }
        }
        self.entry.set_sensitive(view.sensitive);
        self.next.set_can_target(view.sensitive);
        // authPrompt.js `_updateCancelButton`: Back keeps its room (its
        // spacer-sized well) but is shown, and reachable, only from a
        // second step on. Unmapped, it is also hidden from assistive
        // technologies, which a mapped, transparent button is not (GTK
        // clears the accessible Hidden state on every map).
        self.cancel.set_visible(view.back);
        // `updateEntry`: the caps-lock warning belongs to hidden input.
        self.caps.set_visible(view.secret && caps_locked());
        let (text, kind) = view
            .message
            .as_ref()
            .map_or(("", None), |(text, kind)| (text.as_str(), Some(*kind)));
        if self.message.label() != text {
            self.message.set_label(text);
        }
        set_class(
            &self.message,
            "login-dialog-message-warning",
            kind == Some(MessageKind::Error),
        );
        set_class(
            &self.message,
            "login-dialog-message-hint",
            kind == Some(MessageKind::Hint),
        );
    }
}

fn set_class(widget: &impl IsA<gtk::Widget>, class: &str, on: bool) {
    if on {
        widget.add_css_class(class);
    } else {
        widget.remove_css_class(class);
    }
}

fn caps_locked() -> bool {
    gtk::gdk::Display::default()
        .and_then(|d| d.default_seat())
        .and_then(|s| s.keyboard())
        .is_some_and(|k| k.is_caps_locked())
}

pub struct LockUi {
    app: gtk::Application,
    instance: session_lock::Instance,
    port: AuthPort,
    auth: RefCell<AuthMachine>,
    /// Widgets are being updated from the machine: their own change
    /// signals are not input.
    applying: Cell<bool>,
    screens: RefCell<Vec<Screen>>,
    /// A lock was asked of the compositor and not yet released.
    requested: Cell<bool>,
    connection_generation: Cell<Option<u64>>,
    clock_format: Box<dyn Fn() -> logic::ClockFormat>,
    /// Wakes the clock on the minute while the lock screens are up.
    wall_clock: RefCell<Option<crate::wall_clock::WallClock>>,
    /// GNOME's on-screen keyboard, whose lock-screen keys edit only the
    /// unlock prompt (#349).
    osk: RefCell<Option<Rc<crate::osk::Osk>>>,
    /// What the clocks' hints follow (idle time, power save, touch-mode).
    hints: Rc<lock_curtain::HintWatch>,
}

impl LockUi {
    pub fn new(
        app: &gtk::Application,
        port: AuthPort,
        clock_format: Box<dyn Fn() -> logic::ClockFormat>,
    ) -> Rc<Self> {
        let multiplier = crate::lock_auth::parse_message_multiplier(
            std::env::var("GDM_MESSAGE_TIME_MULTIPLIER").ok().as_deref(),
        );
        let ui = Rc::new(Self {
            app: app.clone(),
            instance: session_lock::Instance::new(),
            port,
            auth: RefCell::new(AuthMachine::new(multiplier)),
            applying: Cell::new(false),
            screens: RefCell::new(Vec::new()),
            requested: Cell::new(false),
            connection_generation: Cell::new(None),
            clock_format,
            wall_clock: RefCell::new(None),
            osk: RefCell::new(None),
            hints: lock_curtain::HintWatch::new(),
        });
        {
            let weak = Rc::downgrade(&ui);
            ui.instance.connect_failed(move |_| {
                eprintln!("tuna-shell-gtk: the compositor refused the session lock");
                if let Some(ui) = weak.upgrade() {
                    ui.release_screens();
                }
            });
        }
        {
            let weak = Rc::downgrade(&ui);
            ui.instance.connect_unlocked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.auth.borrow_mut().forget();
                    ui.release_screens();
                }
            });
        }
        ui
    }

    /// Mutter's seat touch-mode changed: the hints say what it asks.
    pub fn set_touch_mode(&self, touch_mode: bool) {
        self.hints.set_touch_mode(touch_mode);
    }

    /// Give each lock window GNOME's on-screen keyboard for its prompt.
    pub fn set_osk(&self, osk: Rc<crate::osk::Osk>) {
        *self.osk.borrow_mut() = Some(osk);
    }

    /// Drop every lock window. Their entries are wiped at once, which is
    /// no input; the windows are destroyed from the main loop afterwards.
    /// gtk4-session-lock emits `::unlocked` (and `::failed`) before it
    /// sends `ext_session_lock_v1.unlock_and_destroy`
    /// (`gtk_session_lock_instance_unlock`), so destroying them here would
    /// tear the lock surfaces down while the session is still locked.
    fn release_screens(&self) {
        let screens = std::mem::take(&mut *self.screens.borrow_mut());
        let windows: Vec<gtk::Window> = screens.iter().map(|s| s.window.clone()).collect();
        let was_applying = self.applying.replace(true);
        drop(screens);
        self.applying.set(was_applying);
        self.wall_clock.take();
        self.hints.disarm();
        glib::idle_add_local_once(move || {
            for window in windows {
                window.destroy();
            }
        });
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
                let window = self.screen(Some(&self.app.clone()));
                self.instance.assign_window_to_monitor(&window, &monitor);
                window.present();
            }
            self.refresh_clock();
            self.hints.arm();
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
    /// The old connection's conversations went with it; verification begins
    /// again once the new one can carry it ([`LockUi::resume_verification`]).
    pub fn sync_connection_generation(self: &Rc<Self>, generation: Option<u64>) {
        if !generation_changed(&self.connection_generation, generation) {
            return;
        }
        for screen in self.screens.borrow().iter() {
            screen.curtain.show_page(Page::Prompt);
        }
        self.drive(|auth, port| auth.reconnected(port));
    }

    /// The connection carries authentication (a current snapshot): begin
    /// what a reconnect or an unsendable begin left waiting, if the prompt
    /// is up.
    pub fn resume_verification(self: &Rc<Self>) {
        if self.auth.borrow().wants_resume() && self.prompt_up() {
            self.drive(|auth, port| auth.resume(port));
        }
    }

    /// A command result: the end of a conversation, or of a
    /// single-password unlock, when it is one of ours.
    pub fn command_result(self: &Rc<Self>, id: u64, applied: bool) {
        self.drive(|auth, port| {
            auth.result(port, id, applied);
        });
    }

    /// One step of a conversation (`Handled::AuthEvent`).
    pub fn auth_event(self: &Rc<Self>, request: u64, service: AuthService, event: &AuthEvent) {
        self.drive(|auth, port| auth.event(port, request, service, event));
    }

    /// Clock and date, as GNOME's UnlockDialogClock shows them (on each
    /// minute while locked, and when the clock format changes).
    pub fn refresh_clock(&self) {
        let now = crate::wall_clock::now().datetime();
        let time = logic::lock_clock_text(&now, (self.clock_format)());
        let date = logic::lock_date_text(now.date());
        for screen in self.screens.borrow().iter() {
            screen.clock.set_label(&time);
            screen.date.set_label(&date);
        }
    }

    /// Run one input through the authentication machine, then show the
    /// result on every monitor.
    fn drive<R>(self: &Rc<Self>, input: impl FnOnce(&mut AuthMachine, &AuthPort) -> R) -> R {
        let result = input(&mut self.auth.borrow_mut(), &self.port);
        self.render();
        result
    }

    fn render(self: &Rc<Self>) {
        let (view, effects) = {
            let mut auth = self.auth.borrow_mut();
            (auth.view(), auth.take_effects())
        };
        let was_applying = self.applying.replace(true);
        {
            let screens = self.screens.borrow();
            for screen in screens.iter() {
                screen.apply(&view);
            }
            for effect in effects {
                match effect {
                    Effect::Timer { token, ms } => {
                        let weak = Rc::downgrade(self);
                        glib::timeout_add_local_once(
                            std::time::Duration::from_millis(ms),
                            move || {
                                if let Some(ui) = weak.upgrade() {
                                    ui.drive(|auth, port| auth.timeout(port, token));
                                }
                            },
                        );
                    }
                    Effect::ClearEntry => {
                        for screen in screens.iter() {
                            screen.entry.clear();
                        }
                    }
                    Effect::Focus => {
                        for screen in screens.iter() {
                            if view.sensitive && screen.curtain.active() == Page::Prompt {
                                screen.entry.focus();
                            }
                        }
                    }
                    Effect::WiggleEntry => {
                        for screen in screens.iter() {
                            screen.entry_wiggle.start();
                        }
                    }
                    Effect::WiggleMessage => {
                        for screen in screens.iter() {
                            screen.message_wiggle.start();
                        }
                    }
                    // GNOME's assertive notification, once (every monitor
                    // shows the same message).
                    Effect::Announce(text) => {
                        let screen = screens
                            .iter()
                            .find(|s| s.curtain.active() == Page::Prompt)
                            .or_else(|| screens.first());
                        if let Some(screen) = screen {
                            screen
                                .message
                                .announce(&text, gtk::AccessibleAnnouncementPriority::High);
                        }
                    }
                    Effect::ShowPrompt => {
                        for screen in screens.iter() {
                            screen.curtain.show_page(Page::Prompt);
                        }
                    }
                    Effect::Trace(line) => trace(&line),
                }
            }
        }
        self.applying.set(was_applying);
    }

    fn prompt_up(&self) -> bool {
        self.screens
            .borrow()
            .iter()
            .any(|s| s.curtain.active() == Page::Prompt)
    }

    /// The prompt page comes up on one monitor (unlockDialog.js
    /// `_showPrompt`), a printable key that lifted it starting the
    /// answer (`startPreemptiveInput`).
    fn lift(self: &Rc<Self>, curtain: &Curtain, entry: &PromptEntry, preemptive: Option<char>) {
        curtain.show_page(Page::Prompt);
        self.drive(|auth, port| auth.show_prompt(port, preemptive.is_some()));
        entry.focus();
        if let Some(c) = preemptive {
            // `startPreemptiveInput(unichar)`: inserted after the reset, at
            // the end of whatever the reset kept.
            let editable = entry.editable();
            let mut position = editable.text().chars().count() as i32;
            editable.insert_text(&c.to_string(), &mut position);
            editable.set_position(position);
        }
    }

    /// Back or Escape (authPrompt.js `cancel`): a later step goes back to
    /// the first; otherwise the clock comes back.
    fn cancel_prompt(self: &Rc<Self>) {
        if self.drive(|auth, port| auth.cancel(port)) {
            for screen in self.screens.borrow().iter() {
                screen.curtain.show_page(Page::Clock);
            }
        }
    }

    /// Enter or Submit on an entry.
    fn submit(self: &Rc<Self>, text: String) {
        self.drive(|auth, port| auth.submit(port, text));
    }

    /// Build one monitor's lock window: GNOME's unlock-screen top bar
    /// over the clock curtain and the prompt.
    fn screen(self: &Rc<Self>, app: Option<&gtk::Application>) -> gtk::Window {
        let window = gtk::Window::new();
        if let Some(app) = app {
            window.set_application(Some(app));
        }
        window.add_css_class("tuna-lock");
        window.set_title(Some("Lock Screen"));

        // The clock page (unlock-dialog-clock).
        let clock = gtk::Label::new(None);
        clock.add_css_class("unlock-dialog-clock-time");
        let date = gtk::Label::new(None);
        date.add_css_class("unlock-dialog-clock-date");
        // GNOME's hint: transparent until the session has been idle for
        // HINT_TIMEOUT, then faded in (`UnlockDialogClock`).
        let hint = gtk::Label::new(None);
        hint.add_css_class("unlock-dialog-clock-hint");
        hint.set_halign(gtk::Align::Center);
        let hint_fade = lock_curtain::Hint::new(&hint, false);
        self.hints.add(&hint_fade);
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
        // second PAM step on (authPrompt.js `_updateCancelButton`): the
        // room is a well as wide as the right-hand spacer.
        let cancel = gtk::Button::from_icon_name("go-previous-symbolic");
        cancel.add_css_class("cancel-button");
        cancel.set_valign(gtk::Align::Center);
        cancel.set_visible(false);
        cancel.update_property(&[gtk::accessible::Property::Label("Back")]);
        let cancel_room = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        cancel_room.add_css_class("button-box-spacer");
        cancel_room.append(&cancel);
        let password = gtk::PasswordEntry::new();
        // GNOME's StPasswordEntry shows its peek icon unless
        // `disable-show-password` denies, live (#357).
        crate::lockdown::bind_password_entry(&password);
        let text = gtk::Entry::new();
        let entry_area = gtk::Overlay::new();
        entry_area.add_css_class("login-dialog-prompt-entry-area");
        entry_area.set_hexpand(true);
        entry_area.set_child(Some(&password));
        let entry = Rc::new(PromptEntry {
            area: entry_area.clone(),
            password: password.clone(),
            text: text.clone(),
            secret: Cell::new(true),
        });
        for widget in [password.upcast_ref::<gtk::Widget>(), text.upcast_ref()] {
            widget.add_css_class("login-dialog-prompt-entry");
            widget.set_hexpand(true);
            // Handle Return before the internal text/IM delegate: an
            // entry must submit even when that delegate consumes
            // activation. Use the same signal as the arrow, so answers
            // stay in the single handler below. This controller only sees
            // keys focused within the entry, not other lock controls.
            let submit_keys = gtk::EventControllerKey::new();
            submit_keys.set_propagation_phase(gtk::PropagationPhase::Capture);
            {
                let widget = widget.downgrade();
                submit_keys.connect_key_pressed(move |_, key, _, _| {
                    if matches!(
                        key,
                        gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter | gtk::gdk::Key::ISO_Enter
                    ) {
                        if let Some(widget) = widget.upgrade().filter(|w| w.is_sensitive()) {
                            widget.emit_by_name::<()>("activate", &[]);
                        }
                        glib::Propagation::Stop
                    } else {
                        glib::Propagation::Proceed
                    }
                });
            }
            widget.add_controller(submit_keys);
        }
        entry.set_hint(crate::lock_auth::DEFAULT_QUESTION);
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
                entry.current().emit_by_name::<()>("activate", &[]);
            });
        }
        entry_area.add_overlay(&next);
        // A failed answer wiggles the entry area (`wiggle(this._entryArea)`).
        let entry_wiggle = Wiggle::new(&entry_area);
        entry_wiggle.set_hexpand(true);
        // Mirrors the Back button so the entry stays centred.
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.add_css_class("button-box-spacer");
        let button_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        button_box.add_css_class("login-dialog-button-box");
        button_box.append(&cancel_room);
        button_box.append(&entry_wiggle);
        button_box.append(&spacer);
        let caps = gtk::Label::new(None);
        caps.add_css_class("caps-lock-warning-label");
        let message = gtk::Label::new(None);
        message.add_css_class("login-dialog-message");
        message.set_wrap(true);
        message.set_justify(gtk::Justification::Center);
        message.set_yalign(0.0);
        // A fingerprint problem wiggles the message.
        let message_wiggle = Wiggle::new(&message);
        let prompt = gtk::Box::new(gtk::Orientation::Vertical, 0);
        prompt.add_css_class("login-dialog-prompt-layout");
        prompt.set_halign(gtk::Align::Center);
        prompt.set_valign(gtk::Align::Start);
        prompt.append(&avatar);
        prompt.append(&name);
        prompt.append(&button_box);
        prompt.append(&caps);
        prompt.append(&message_wiggle);

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
        let keyboard = self.osk.borrow().as_ref().map(|osk| {
            let prompt = Rc::downgrade(&entry);
            osk.lock_keyboard(Rc::new(move || prompt.upgrade().map(|e| e.editable())))
        });
        if let Some(keyboard) = &keyboard {
            overlay.add_overlay(keyboard.widget());
            for widget in [
                entry.password.upcast_ref::<gtk::Widget>(),
                entry.text.upcast_ref(),
            ] {
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
                widget.add_controller(focus);
            }
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
        // or a wheel step down lifts the curtain (and begins verification),
        // a printable key starting the answer with it; Escape on the
        // prompt cancels (`cancel`), a wheel step up brings the clock
        // back. A vertical drag (pointer, or touch, which the session
        // delivers as the primary button), a three-finger touchpad swipe
        // or a two-finger touchpad scroll moves the curtain.
        let show_prompt = {
            let (weak, curtain, entry) = (Rc::downgrade(self), curtain.clone(), entry.clone());
            move |preemptive: Option<char>| {
                if let Some(ui) = weak.upgrade() {
                    ui.lift(&curtain, &entry, preemptive);
                }
            }
        };
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        {
            let (weak, curtain, entry, show_prompt) = (
                Rc::downgrade(self),
                curtain.clone(),
                entry.clone(),
                show_prompt.clone(),
            );
            keys.connect_key_pressed(move |_, key, _, _| {
                if key == gtk::gdk::Key::Escape && curtain.active() == Page::Prompt {
                    if let Some(ui) = weak.upgrade() {
                        ui.cancel_prompt();
                    }
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
                    ) {
                        trace(&format!(
                            "lock Return received sensitive={}",
                            entry.current().is_sensitive()
                        ));
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
                show_prompt(key.to_unicode().filter(|c| !c.is_control()));
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
                    show_prompt(None);
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
            // This drag began the swipe under way.
            let dragging = Rc::new(Cell::new(false));
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
                let (pan, distance, dragging, curtain) = (
                    pan.clone(),
                    distance.clone(),
                    dragging.clone(),
                    curtain.clone(),
                );
                let weak = Rc::downgrade(self);
                drag.connect_drag_update(move |gesture, _, offset_y| {
                    let time = gesture.current_event_time();
                    let Some(delta) = pan
                        .borrow_mut()
                        .as_mut()
                        .and_then(|pan| pan.motion(offset_y, time))
                    else {
                        return;
                    };
                    if !dragging.get() {
                        let Some(page) = curtain.swipe_begin(Source::Drag) else {
                            // A touchpad swipe has the curtain.
                            return;
                        };
                        gesture.set_state(gtk::EventSequenceState::Claimed);
                        dragging.set(true);
                        distance.set(page);
                        // `_swipeBegin` → `_ensureAuthPrompt`.
                        if let Some(ui) = weak.upgrade() {
                            ui.drive(|auth, port| auth.show_prompt(port, false));
                        }
                    }
                    // Up lifts the curtain (`_getGestureDirFactor`).
                    curtain.swipe_update(-delta, distance.get());
                });
            }
            {
                let (pan, distance, dragging, curtain, entry) = (
                    pan.clone(),
                    distance.clone(),
                    dragging.clone(),
                    curtain.clone(),
                    entry.clone(),
                );
                drag.connect_drag_end(move |gesture, _, _| {
                    let time = gesture.current_event_time();
                    let Some(pan) = pan.borrow_mut().take() else {
                        return;
                    };
                    if !dragging.replace(false) {
                        return;
                    }
                    let velocity = -pan.velocity(time);
                    if curtain.swipe_end(velocity, distance.get(), false) == Some(Page::Prompt) {
                        entry.focus();
                    }
                });
            }
            {
                let curtain = curtain.clone();
                drag.connect_cancel(move |_, _| {
                    if pan.borrow_mut().take().is_some() && dragging.replace(false) {
                        curtain.swipe_end(0.0, distance.get(), true);
                    }
                });
            }
        }
        window.add_controller(drag);
        // `SwipeTracker`'s touchpad gestures: each step moves the curtain
        // over TOUCHPAD_BASE_HEIGHT, a begin brings the prompt in
        // (`_ensureAuthPrompt`) and an end settles on the touchpad
        // projection.
        let touchpad_step = {
            let (weak, curtain, entry) = (Rc::downgrade(self), curtain.clone(), entry.clone());
            move |source: Source, step: lock_curtain::Step| {
                let begin = || {
                    if let Some(ui) = weak.upgrade() {
                        ui.drive(|auth, port| auth.show_prompt(port, false));
                    }
                };
                if curtain.touchpad(source, step, begin) == Some(Page::Prompt) {
                    entry.focus();
                }
            }
        };
        // `TouchpadSwipeGesture`: three fingers, vertical, the deltas
        // turned round under natural scrolling (read as it goes, as
        // GNOME does).
        let swipe = gtk::EventControllerLegacy::new();
        {
            let pad = RefCell::new(lock_curtain::TouchpadSwipe::default());
            let touchpad_step = touchpad_step.clone();
            let settings = touchpad_settings();
            swipe.connect_event(move |_, event| {
                let Some(swipe) = event.downcast_ref::<gtk::gdk::TouchpadEvent>() else {
                    return glib::Propagation::Proceed;
                };
                if event.event_type() != gtk::gdk::EventType::TouchpadSwipe {
                    return glib::Propagation::Proceed;
                }
                let phase = match swipe.gesture_phase() {
                    gtk::gdk::TouchpadGesturePhase::Begin => lock_curtain::SwipePhase::Begin,
                    gtk::gdk::TouchpadGesturePhase::Update => lock_curtain::SwipePhase::Update,
                    _ => lock_curtain::SwipePhase::End,
                };
                let natural = settings
                    .as_ref()
                    .is_none_or(|s| s.boolean("natural-scroll"));
                let (step, handled) = pad.borrow_mut().event(
                    phase,
                    swipe.n_fingers(),
                    swipe.deltas(),
                    event.time(),
                    natural,
                );
                if let Some(step) = step {
                    touchpad_step(Source::Swipe, step);
                }
                if handled {
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            });
        }
        window.add_controller(swipe);
        // Wheel steps switch pages, GNOME's discrete `ScrollController`
        // (down the prompt, up the clock). A touchpad scroll is GNOME's
        // `ScrollGesture`: it follows the fingers and settles on the axis
        // stop, which ends GTK's scroll (`scroll-end`).
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
        {
            let wheel = Rc::new(Cell::new(0.0));
            let gesture = Rc::new(RefCell::new(lock_curtain::ScrollGesture::default()));
            {
                let (curtain, show_prompt, gesture, touchpad_step) = (
                    curtain.clone(),
                    show_prompt.clone(),
                    gesture.clone(),
                    touchpad_step.clone(),
                );
                scroll.connect_scroll(move |controller, dx, dy| {
                    if controller.unit() != gtk::gdk::ScrollUnit::Wheel {
                        // `canHandleEvent`: finger scrolls only.
                        if !controller
                            .current_event_device()
                            .is_some_and(|d| d.source() == gtk::gdk::InputSource::Touchpad)
                        {
                            return glib::Propagation::Proceed;
                        }
                        let time = controller.current_event_time();
                        let step = gesture.borrow_mut().scroll(dx, dy, time);
                        touchpad_step(Source::Scroll, step);
                        return glib::Propagation::Stop;
                    }
                    let steps = wheel.get() + dy;
                    if steps.abs() < 1.0 {
                        wheel.set(steps);
                        return glib::Propagation::Stop;
                    }
                    wheel.set(0.0);
                    if steps > 0.0 {
                        show_prompt(None);
                    } else {
                        curtain.show_page(Page::Clock);
                    }
                    glib::Propagation::Stop
                });
            }
            scroll.connect_scroll_end(move |controller| {
                let step = gesture.borrow_mut().stop(controller.current_event_time());
                touchpad_step(Source::Scroll, step);
            });
        }
        window.add_controller(scroll);
        {
            // GNOME destroys the prompt once the clock is back
            // (`_maybeDestroyAuthPrompt`): nothing typed outlives it, and
            // the conversation ends once no monitor shows the prompt.
            let (weak, entry) = (Rc::downgrade(self), Rc::downgrade(&entry));
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
                    entry.clear();
                }
                if let Some(ui) = weak.upgrade() {
                    if !ui.prompt_up() {
                        ui.drive(|auth, port| auth.stop(port));
                    }
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            cancel.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.cancel_prompt();
                }
            });
        }
        for editable in [password.upcast_ref::<gtk::Editable>(), text.upcast_ref()] {
            {
                let weak = Rc::downgrade(self);
                editable.connect_changed(move |editable| {
                    let Some(ui) = weak.upgrade() else { return };
                    if ui.applying.get() {
                        return;
                    }
                    let has_text = !editable.text().is_empty();
                    ui.drive(|auth, _| auth.text_changed(has_text));
                });
            }
        }
        {
            let weak = Rc::downgrade(self);
            password.connect_activate(move |entry| {
                if let Some(ui) = weak.upgrade() {
                    ui.submit(entry.text().to_string());
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            text.connect_activate(move |entry| {
                if let Some(ui) = weak.upgrade() {
                    ui.submit(entry.text().to_string());
                }
            });
        }
        // GNOME's caps-lock warning under a hidden entry.
        caps.set_label("Caps lock is on.");
        if let Some(display) = gtk::gdk::Display::default() {
            if let Some(seat) = display.default_seat() {
                if let Some(keyboard) = seat.keyboard() {
                    let update = {
                        let (caps, entry) = (caps.clone(), Rc::downgrade(&entry));
                        move |k: &gtk::gdk::Device| {
                            let secret = entry.upgrade().is_some_and(|e| e.secret.get());
                            caps.set_visible(secret && k.is_caps_locked());
                        }
                    };
                    update(&keyboard);
                    keyboard.connect_caps_lock_state_notify(update);
                }
            }
        }

        let screen = Screen {
            window: window.clone(),
            curtain,
            clock,
            date,
            entry,
            entry_wiggle,
            cancel,
            next,
            caps,
            message,
            _keyboard: keyboard,
            message_wiggle,
            _hint: hint_fade,
        };
        screen.apply(&self.auth.borrow().view());
        self.screens.borrow_mut().push(screen);
        window
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        // Never leave typed text behind in a dead window.
        self.entry.clear();
    }
}

fn generation_changed(current: &Cell<Option<u64>>, next: Option<u64>) -> bool {
    let previous = current.replace(next);
    previous.is_some() && previous != next
}

#[cfg(test)]
mod auth_transport_ui_tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[derive(Default)]
    struct Calls {
        next: Cell<u64>,
        begins: RefCell<Vec<(AuthService, u64)>>,
        answers: RefCell<Vec<(u32, String)>>,
        cancels: Cell<u32>,
        unlocks: Cell<u32>,
        /// The id the next `Unlock` gets, `None` = unsendable.
        unlock_next: Cell<Option<u64>>,
    }

    fn port(calls: &Rc<Calls>) -> AuthPort {
        let id = |calls: &Calls| {
            calls.next.set(calls.next.get() + 1);
            calls.next.get()
        };
        AuthPort {
            unlock: {
                let calls = calls.clone();
                Rc::new(move |_password| {
                    calls.unlocks.set(calls.unlocks.get() + 1);
                    calls.unlock_next.get()
                })
            },
            begin: {
                let calls = calls.clone();
                Rc::new(move |service| {
                    let request = id(&calls);
                    calls.begins.borrow_mut().push((service, request));
                    Some(request)
                })
            },
            answer: {
                let calls = calls.clone();
                Rc::new(move |_, sequence, answer| {
                    calls.answers.borrow_mut().push((sequence, answer));
                    Some(id(&calls))
                })
            },
            cancel: {
                let calls = calls.clone();
                Rc::new(move || {
                    calls.cancels.set(calls.cancels.get() + 1);
                    Some(id(&calls))
                })
            },
            fingerprint_enabled: Rc::new(|| true),
        }
    }

    fn lock_ui(calls: &Rc<Calls>) -> (Rc<LockUi>, gtk::Window) {
        let app = gtk::Application::new(None::<&str>, gtk::gio::ApplicationFlags::NON_UNIQUE);
        let ui = LockUi::new(
            &app,
            port(calls),
            Box::new(|| logic::ClockFormat::TwentyFourHour),
        );
        ui.sync_connection_generation(Some(0));
        let window = ui.screen(None);
        window.present();
        (ui, window)
    }

    /// Lift the curtain as a click does and return (password, fingerprint).
    fn lift(ui: &Rc<LockUi>, calls: &Calls, key: Option<char>) -> (u64, u64) {
        let (curtain, entry) = {
            let screens = ui.screens.borrow();
            (screens[0].curtain.clone(), screens[0].entry.clone())
        };
        ui.lift(&curtain, &entry, key);
        let begins = calls.begins.borrow();
        let n = begins.len();
        assert!(n >= 2);
        assert_eq!(begins[n - 2].0, AuthService::Password);
        assert_eq!(begins[n - 1].0, AuthService::Fingerprint);
        (begins[n - 2].1, begins[n - 1].1)
    }

    fn pump_until(limit: Duration, mut done: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + limit;
        let context = glib::MainContext::default();
        while Instant::now() < deadline {
            if done() {
                return true;
            }
            context.iteration(false);
            std::thread::sleep(Duration::from_millis(5));
        }
        done()
    }

    #[test]
    #[ignore = "requires admitted Xvfb GTK proof"]
    fn actual_gtk_auth_send_failure_and_generation_reset() {
        assert!(
            std::env::var_os("TUNA_REQUIRE_AUTH_TRANSPORT_GTK").is_some(),
            "dedicated admission required"
        );
        gtk::init().expect("actual GTK display required");
        let calls = Rc::new(Calls::default());
        let (ui, _window) = lock_ui(&calls);
        // No conversation backend: the password service is unavailable and
        // the prompt falls back to the single-password Unlock.
        let fall_back = |ui: &Rc<LockUi>, password: u64, fingerprint: u64| {
            ui.auth_event(password, AuthService::Password, &AuthEvent::Unavailable);
            ui.command_result(password, false);
            ui.auth_event(
                fingerprint,
                AuthService::Fingerprint,
                &AuthEvent::Unavailable,
            );
            ui.command_result(fingerprint, false);
            assert!(ui.auth.borrow().legacy());
        };
        let (password, fingerprint) = lift(&ui, &calls, None);
        fall_back(&ui, password, fingerprint);
        let entry = ui.screens.borrow()[0].entry.password.clone();
        let pending = || ui.auth.borrow().legacy_pending();
        entry.set_text("controlled-only");
        entry.emit_by_name::<()>("activate", &[]);
        assert_eq!(entry.text(), "");
        assert!(entry.is_sensitive());
        assert_eq!(pending(), None);
        calls.unlock_next.set(Some(9));
        entry.set_text("controlled-only");
        entry.emit_by_name::<()>("activate", &[]);
        assert_eq!(pending(), Some(9));
        assert!(!entry.is_sensitive());
        assert_eq!(calls.unlocks.get(), 2);
        entry.emit_by_name::<()>("activate", &[]);
        assert_eq!(
            calls.unlocks.get(),
            2,
            "stale captured activation while pending cannot send"
        );
        let begins = calls.begins.borrow().len();
        ui.sync_connection_generation(Some(1));
        assert_eq!(pending(), None);
        assert_eq!(entry.text(), "");
        assert!(entry.is_sensitive());
        assert_eq!(
            calls.unlocks.get(),
            2,
            "reset itself must not submit credentials"
        );
        assert_eq!(
            calls.begins.borrow().len(),
            begins,
            "nothing begins before the snapshot"
        );
        assert!(calls.answers.borrow().is_empty());
        ui.resume_verification();
        let (password, fingerprint) = lift(&ui, &calls, None);
        fall_back(&ui, password, fingerprint);
        entry.set_text("explicit-fresh-input");
        entry.emit_by_name::<()>("activate", &[]);
        assert_eq!(calls.unlocks.get(), 3);
    }

    #[test]
    #[ignore = "requires admitted Xvfb GTK proof"]
    fn actual_gtk_two_question_conversation_and_back_soft_reset() {
        assert!(
            std::env::var_os("TUNA_REQUIRE_AUTH_PROMPT_GTK").is_some(),
            "dedicated admission required"
        );
        gtk::init().expect("actual GTK display required");
        crate::motion::set(tuna_shell_control::MotionPolicy::from_gnome(
            false, false, 1.0,
        ));
        let calls = Rc::new(Calls::default());
        let (ui, window) = lock_ui(&calls);
        // A printable key lifts the curtain and starts the answer
        // (`_onKeyPress` → `startPreemptiveInput`).
        let (password, fingerprint) = lift(&ui, &calls, Some('f'));
        let (entry, cancel, message, next) = {
            let s = &ui.screens.borrow()[0];
            (
                s.entry.clone(),
                s.cancel.clone(),
                s.message.clone(),
                s.next.clone(),
            )
        };
        let shown = |entry: &PromptEntry| entry.area.child().expect("an entry");
        let placeholder = |w: &gtk::Widget| w.property::<Option<String>>("placeholder-text");
        let back_targetable = || cancel.is_visible() && cancel.can_target();

        // Step 1: PAM's "Password:" in the hidden entry; Back hidden.
        ui.auth_event(
            password,
            AuthService::Password,
            &AuthEvent::Query {
                sequence: 1,
                question: "Password:".into(),
                secret: true,
            },
        );
        assert!(shown(&entry).is::<gtk::PasswordEntry>());
        assert_eq!(placeholder(&shown(&entry)).as_deref(), Some("Password"));
        assert!(shown(&entry).is_sensitive());
        assert!(!back_targetable());
        // The question refocused the entry: the wake character is kept,
        // unselected, so the next keys follow it.
        let typing = entry.editable();
        assert_eq!(typing.text(), "f");
        assert_eq!(typing.selection_bounds(), None);
        assert_eq!(typing.position(), 1);
        let mut at = typing.position();
        typing.insert_text("irst-secret", &mut at);
        next.emit_clicked();
        assert_eq!(
            *calls.answers.borrow(),
            vec![(1, "first-secret".to_owned())]
        );
        assert!(
            !shown(&entry).is_sensitive(),
            "insensitive until the next question"
        );

        // An info message, then step 2: a visible code entry, Back shown.
        ui.auth_event(
            password,
            AuthService::Password,
            &AuthEvent::Info {
                text: "Open your authenticator app".into(),
            },
        );
        assert_eq!(message.label(), "Open your authenticator app");
        assert!(!message.has_css_class("login-dialog-message-warning"));
        ui.auth_event(
            password,
            AuthService::Password,
            &AuthEvent::Query {
                sequence: 2,
                question: "Verification code: ".into(),
                secret: false,
            },
        );
        assert!(shown(&entry).is::<gtk::Entry>());
        assert!(!shown(&entry).is::<gtk::PasswordEntry>());
        assert_eq!(
            placeholder(&shown(&entry)).as_deref(),
            Some("Verification code")
        );
        assert_eq!(entry.text.text(), "", "the previous answer is gone");
        assert_eq!(entry.password.text(), "");
        assert!(shown(&entry).is_sensitive());
        assert!(back_targetable());
        entry.text.set_text("123456");
        entry.text.emit_by_name::<()>("activate", &[]);
        // The answer waits until the info message has been read (2 s).
        assert_eq!(calls.answers.borrow().len(), 1);
        assert!(pump_until(Duration::from_secs(5), || calls
            .answers
            .borrow()
            .len()
            == 2));
        assert_eq!(calls.answers.borrow()[1], (2, "123456".to_owned()));

        // A fingerprint problem: error style at once.
        ui.auth_event(
            fingerprint,
            AuthService::Fingerprint,
            &AuthEvent::Problem {
                text: "Failed to match fingerprint".into(),
            },
        );
        assert_eq!(message.label(), "Failed to match fingerprint");
        assert!(message.has_css_class("login-dialog-message-warning"));

        // Back at step 2: soft reset to step 1 once both conversations
        // retired.
        cancel.emit_clicked();
        assert_eq!(calls.cancels.get(), 1);
        assert!(shown(&entry).is::<gtk::PasswordEntry>());
        assert_eq!(placeholder(&shown(&entry)).as_deref(), Some("Password"));
        assert!(!back_targetable());
        assert_eq!(message.label(), "");
        assert_eq!(entry.text.text(), "");
        assert_eq!(ui.screens.borrow()[0].curtain.active(), Page::Prompt);
        let begins = calls.begins.borrow().len();
        ui.command_result(password, false);
        ui.command_result(fingerprint, false);
        let (password2, fingerprint2) = {
            let b = calls.begins.borrow();
            assert_eq!(b.len(), begins + 2, "both begin again after retirement");
            (b[begins].1, b[begins + 1].1)
        };
        // The old conversation's steps no longer reach the prompt.
        ui.auth_event(
            password,
            AuthService::Password,
            &AuthEvent::Query {
                sequence: 3,
                question: "Stale:".into(),
                secret: false,
            },
        );
        assert!(shown(&entry).is::<gtk::PasswordEntry>());
        ui.auth_event(
            fingerprint2,
            AuthService::Fingerprint,
            &AuthEvent::FingerprintScan { swipe: false },
        );
        assert_eq!(message.label(), "(or place finger on reader)");
        assert!(message.has_css_class("login-dialog-message-hint"));
        ui.auth_event(
            password2,
            AuthService::Password,
            &AuthEvent::Query {
                sequence: 1,
                question: "Password:".into(),
                secret: true,
            },
        );
        assert!(shown(&entry).is_sensitive());
        assert!(!back_targetable());
        assert_eq!(ui.auth.borrow().prompt_step(), 1);
        // Typed into the hidden entry, then a visible question: nothing
        // hidden is revealed in the visible entry.
        entry.password.set_text("hidden-typed");
        ui.auth_event(
            password2,
            AuthService::Password,
            &AuthEvent::Query {
                sequence: 2,
                question: "Code:".into(),
                secret: false,
            },
        );
        assert!(shown(&entry).is::<gtk::Entry>());
        assert!(!shown(&entry).is::<gtk::PasswordEntry>());
        assert_eq!(entry.text.text(), "", "hidden text is not revealed");
        assert_eq!(entry.password.text(), "");
        assert_eq!(placeholder(&shown(&entry)).as_deref(), Some("Code"));
        assert_eq!(calls.answers.borrow().len(), 2, "nothing sent");
        // The swap itself (`updateEntry`): visible text is carried into
        // the hidden entry, hidden text never into the visible one.
        let lone = PromptEntry {
            area: gtk::Overlay::new(),
            password: gtk::PasswordEntry::new(),
            text: gtk::Entry::new(),
            secret: Cell::new(true),
        };
        lone.area.set_child(Some(&lone.password));
        lone.password.set_text("s3cret");
        lone.set_secret(false);
        assert!(lone.area.child().unwrap().is::<gtk::Entry>());
        assert_eq!(lone.text.text(), "");
        assert_eq!(lone.password.text(), "");
        lone.text.set_text("visible");
        lone.set_secret(true);
        assert!(lone.area.child().unwrap().is::<gtk::PasswordEntry>());
        assert_eq!(lone.password.text(), "visible");
        assert_eq!(lone.text.text(), "");
        // Unlocked: the lock window goes, not just its state.
        entry.text.set_text("typed-after");
        ui.auth.borrow_mut().forget();
        ui.release_screens();
        assert!(ui.screens.borrow().is_empty());
        assert_eq!(entry.text.text(), "", "wiped at once");
        // Destroyed from the main loop, after the session lock's own
        // unlock request (`::unlocked` precedes it).
        assert!(window.is_visible(), "not inside the unlocked handler");
        assert!(
            pump_until(Duration::from_secs(2), || !window.is_visible()),
            "the lock window is destroyed"
        );
        assert_eq!(entry.text.text(), "", "nothing typed survives");
    }
}
