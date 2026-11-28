//! `tuna-greeter`: Tuna Desktop's login screen (#1164, docs/greeter.md).
//!
//! GDM 51's daemon keeps PAM, seats, VT handoff and session start; this
//! client replaces GNOME Shell's `gdm` session mode (`js/gdm/loginDialog.js`)
//! as the trusted client of `tuna-compositor --greeter`. It lists the users
//! AccountsService knows, takes a username after "Not listed?", runs the
//! password conversation through GDM's `UserVerifier` with the lock
//! screen's own AuthPrompt machine (`tuna-auth-prompt`), offers the
//! installed sessions, and asks GDM to start the chosen one.
//!
//! `TUNA_GREETER_TRACE=1` prints the flow's markers to stderr (never an
//! answer).

mod accounts;
mod auth_ui;
mod channel;
mod conflict;
mod conversation;
mod gdm;
mod logind;
mod sessions;
mod smartcard;
mod timed_login;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gio::prelude::*;
use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use libadwaita as adw;
use tuna_auth_prompt::entry::PromptEntry;
use tuna_auth_prompt::{Ask, AuthMachine, Effect, MessageKind, View};

use conversation::{AuthPolicy, Conversations, Input, Mechanism};
use gdm::{Call, Signal};

const LOGIN_SCREEN_SCHEMA: &str = "org.gnome.login-screen";
/// The prompt's question before "Not listed?" has a user
/// (loginDialog.js `_askForUsernameAndBeginVerification`).
const USERNAME_QUESTION: &str = "Username";
/// How long to wait before asking GDM for its channel again.
const RECONNECT: std::time::Duration = std::time::Duration::from_secs(1);
/// The prompt's caps-lock warning (gnome-shell 51.1 shellEntry.js
/// `CapsLockWarning`).
const CAPS_LOCK_WARNING: &str = "Caps lock is on";

thread_local! {
    /// The login screen, alive for the process (GTK's main thread only).
    static GREETER: RefCell<Option<Rc<Greeter>>> = const { RefCell::new(None) };
}

fn trace(line: &str) {
    if std::env::var_os("TUNA_GREETER_TRACE").is_some() {
        eprintln!("tuna-greeter: {line}");
    }
}

/// `org.gnome.login-screen`, GDM's own schema (GDM 51.1.1
/// `data/org.gnome.login-screen.gschema.xml`); its defaults when the
/// schema is not installed.
struct Policy {
    disable_user_list: bool,
    banner: Option<String>,
    logo: Option<String>,
    disable_restart_buttons: bool,
    auth: AuthPolicy,
    /// The schema's settings, when installed (the authentication keys
    /// are followed live, as `ShellUserVerifier._onSettingsChanged`).
    settings: Option<gio::Settings>,
}

impl Default for Policy {
    /// The schema's defaults.
    fn default() -> Self {
        Self {
            disable_user_list: false,
            banner: None,
            logo: None,
            disable_restart_buttons: false,
            auth: AuthPolicy::default(),
            settings: None,
        }
    }
}

impl Policy {
    fn read() -> Self {
        let settings = gio::SettingsSchemaSource::default()
            .and_then(|source| source.lookup(LOGIN_SCREEN_SCHEMA, true))
            .map(|_| gio::Settings::new(LOGIN_SCREEN_SCHEMA));
        let Some(s) = settings else {
            return Self::default();
        };
        Self {
            settings: Some(s.clone()),
            ..Self::parse(|key| s.value(key))
        }
    }

    /// The policy from the schema's keys (`value` reads one): the banner
    /// only when enabled with text (loginDialog.js `_updateBanner`), the
    /// logo only when named, and the authentication keys (at least one
    /// try: `_canRetry` with a fail count of one stops at any
    /// `allowed-failures` below 2).
    fn parse(value: impl Fn(&str) -> glib::Variant) -> Self {
        let boolean = |key: &str| value(key).get::<bool>().unwrap_or(false);
        let string = |key: &str| value(key).get::<String>().unwrap_or_default();
        let banner = boolean("banner-message-enable").then(|| string("banner-message-text"));
        Self {
            disable_user_list: boolean("disable-user-list"),
            banner: banner.filter(|text| !text.trim().is_empty()),
            logo: Some(string("logo")).filter(|path| !path.is_empty()),
            disable_restart_buttons: boolean("disable-restart-buttons"),
            auth: auth_policy(
                value("allowed-failures").get::<i32>().unwrap_or(0),
                boolean("enable-password-authentication"),
                boolean("enable-smartcard-authentication"),
                boolean("enable-switchable-authentication"),
            ),
            settings: None,
        }
    }

    /// `allowed-failures` and `enable-{password,smartcard,switchable}-authentication`.
    fn read_auth(s: &gio::Settings) -> AuthPolicy {
        Self::parse(|key| s.value(key)).auth
    }
}

struct Greeter {
    window: gtk::Window,
    stack: gtk::Stack,
    user_list: gtk::ListBox,
    avatar: adw::Avatar,
    name: gtk::Label,
    entry: Rc<PromptEntry>,
    entry_area: gtk::Overlay,
    button_box: gtk::Box,
    auth_ui: Rc<auth_ui::AuthUi>,
    cancel: gtk::Button,
    next: gtk::Button,
    caps: gtk::Label,
    message: gtk::Label,
    options: gtk::MenuButton,
    session_box: gtk::Box,
    session_buttons: RefCell<Vec<(String, gtk::CheckButton)>>,
    /// Login Options' mechanism section (`_PRIMARY_LOGIN_METHOD_SECTION_NAME`).
    mechanism_box: gtk::Box,
    /// Toggle buttons in a group (`AuthMenuItem`s with GNOME's dot): items
    /// with an action, unlike check buttons, for assistive technologies.
    mechanism_buttons: RefCell<Vec<(Mechanism, gtk::ToggleButton)>>,
    machine: RefCell<AuthMachine>,
    host: Conversations,
    policy: Policy,
    system: RefCell<Option<gio::DBusConnection>>,
    channels: channel::Channels,
    accounts_watch: RefCell<Option<gio::SignalSubscription>>,
    logind_watch: RefCell<Option<gio::SignalSubscription>>,
    smartcard: RefCell<Option<smartcard::Watch>>,
    /// The login screen's own logind session and whether it was active.
    own_session: RefCell<Option<String>>,
    own_active: Cell<Option<bool>>,
    users: RefCell<Vec<accounts::User>>,
    /// Users with a session (the list's `logged-in` ring).
    logged_in: RefCell<Vec<String>>,
    /// The user the prompt verifies.
    prompt_user: RefCell<Option<String>>,
    timed: timed_login::Timed,
    conflict: Rc<conflict::Conflict>,
    /// "Not listed?": the prompt asks for a username, not GDM.
    asking_username: Cell<bool>,
    /// The session picked in Login Options; GDM forgets it on every
    /// reset, so it is selected again after each begin.
    chosen_session: RefCell<Option<String>>,
    applying: Cell<bool>,
    registered: Cell<bool>,
}

impl Greeter {
    fn new(app: &adw::Application) -> Rc<Self> {
        let policy = Policy::read();
        let multiplier = tuna_auth_prompt::parse_message_multiplier(
            std::env::var("GDM_MESSAGE_TIME_MULTIPLIER").ok().as_deref(),
        );
        let host = Conversations::new(policy.auth);
        let auth_ui = auth_ui::AuthUi::new();

        let window = gtk::Window::new();
        window.set_application(Some(app));
        window.set_title(Some("Login Screen"));
        window.add_css_class("tuna-greeter");
        window.add_css_class("login-dialog");
        if gtk4_layer_shell::is_supported() {
            window.init_layer_shell();
            window.set_layer(Layer::Top);
            window.set_namespace(Some("tuna-greeter"));
            for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
                window.set_anchor(edge, true);
            }
            window.set_exclusive_zone(-1);
            window.set_keyboard_mode(KeyboardMode::Exclusive);
        } else {
            window.fullscreen();
        }

        // The login screen's panel (`panelStyle: 'login-screen'`): the
        // clock in the middle, system actions at the right.
        let clock = gtk::Label::new(None);
        clock.add_css_class("clock");
        let panel = gtk::CenterBox::new();
        panel.add_css_class("login-screen-panel");
        panel.set_center_widget(Some(&clock));
        if let Some(power) = power_menu(&policy) {
            panel.set_end_widget(Some(&power));
        }
        let tick = {
            let clock = clock.clone();
            move || {
                if let Ok(now) = glib::DateTime::now_local() {
                    if let Ok(text) = now.format("%a %R") {
                        clock.set_label(&text);
                    }
                }
            }
        };
        tick();
        glib::timeout_add_seconds_local(1, move || {
            tick();
            glib::ControlFlow::Continue
        });

        // The banner (`banner-message-enable`/`-text`).
        let banner = gtk::Label::new(policy.banner.as_deref());
        banner.add_css_class("login-dialog-banner");
        banner.set_wrap(true);
        banner.set_justify(gtk::Justification::Center);
        banner.set_visible(policy.banner.is_some());

        // The user list page (login-dialog-user-selection-box).
        let user_list = gtk::ListBox::new();
        user_list.add_css_class("login-dialog-user-list");
        user_list.set_selection_mode(gtk::SelectionMode::None);
        user_list.update_property(&[gtk::accessible::Property::Label("Users")]);
        let scroller = gtk::ScrolledWindow::new();
        scroller.add_css_class("login-dialog-user-list-view");
        scroller.set_hscrollbar_policy(gtk::PolicyType::Never);
        scroller.set_propagate_natural_height(true);
        scroller.set_max_content_height(420);
        scroller.set_child(Some(&user_list));
        let not_listed = gtk::Button::with_label("Not listed?");
        not_listed.add_css_class("login-dialog-not-listed-button");
        not_listed.set_halign(gtk::Align::Start);
        let selection = gtk::Box::new(gtk::Orientation::Vertical, 0);
        selection.add_css_class("login-dialog-user-selection-box");
        selection.set_halign(gtk::Align::Center);
        selection.set_valign(gtk::Align::Center);
        selection.append(&scroller);
        selection.append(&not_listed);

        // The prompt page (login-dialog-prompt-layout), the lock screen's
        // AuthPrompt widgets.
        let avatar = adw::Avatar::new(128, None, false);
        avatar.add_css_class("user-icon");
        avatar.set_halign(gtk::Align::Center);
        let name = gtk::Label::new(None);
        name.add_css_class("user-widget-label");
        let cancel = gtk::Button::from_icon_name("go-previous-symbolic");
        cancel.add_css_class("cancel-button");
        cancel.set_valign(gtk::Align::Center);
        cancel.update_property(&[gtk::accessible::Property::Label("Back")]);
        let cancel_room = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        cancel_room.add_css_class("button-box-spacer");
        cancel_room.append(&cancel);
        let password = gtk::PasswordEntry::new();
        password.set_show_peek_icon(true);
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
            // Return submits even when the entry's text delegate keeps
            // activation for itself, as on the lock screen.
            let keys = gtk::EventControllerKey::new();
            keys.set_propagation_phase(gtk::PropagationPhase::Capture);
            let target = widget.downgrade();
            keys.connect_key_pressed(move |_, key, _, _| {
                if matches!(
                    key,
                    gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter | gtk::gdk::Key::ISO_Enter
                ) {
                    if let Some(widget) = target.upgrade().filter(|w| w.is_sensitive()) {
                        widget.emit_by_name::<()>("activate", &[]);
                    }
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            });
            widget.add_controller(keys);
        }
        entry.set_hint(tuna_auth_prompt::DEFAULT_QUESTION);
        let next = gtk::Button::from_icon_name("go-next-symbolic");
        next.add_css_class("next-button");
        next.set_halign(gtk::Align::End);
        next.set_valign(gtk::Align::Center);
        next.set_can_focus(false);
        next.update_property(&[gtk::accessible::Property::Label("Next")]);
        entry_area.add_overlay(&next);
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.add_css_class("button-box-spacer");
        let button_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        button_box.add_css_class("login-dialog-button-box");
        button_box.append(&cancel_room);
        button_box.append(&entry_area);
        button_box.append(&spacer);
        let caps = gtk::Label::new(Some(CAPS_LOCK_WARNING));
        caps.add_css_class("caps-lock-warning-label");
        caps.set_visible(false);
        let message = gtk::Label::new(None);
        message.add_css_class("login-dialog-message");
        message.set_wrap(true);
        message.set_justify(gtk::Justification::Center);
        message.set_yalign(0.0);
        let prompt = gtk::Box::new(gtk::Orientation::Vertical, 0);
        prompt.add_css_class("login-dialog-prompt-layout");
        prompt.set_halign(gtk::Align::Center);
        prompt.set_valign(gtk::Align::Center);
        prompt.append(&avatar);
        prompt.append(&name);
        prompt.append(&button_box);
        prompt.append(&auth_ui.root);
        prompt.append(&caps);
        prompt.append(&message);

        let stack = gtk::Stack::new();
        stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        stack.set_transition_duration(250);
        stack.set_vexpand(true);
        stack.add_named(&selection, Some("users"));
        stack.add_named(&prompt, Some("prompt"));

        // The bottom button group: Login Options (the session type) and
        // the logo.
        let session_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
        session_box.add_css_class("login-dialog-auth-menu-item-section");
        let heading = gtk::Label::new(Some("Session Type"));
        heading.add_css_class("heading");
        heading.set_halign(gtk::Align::Start);
        session_box.append(&heading);
        // The mechanisms come first (`sectionOrder`).
        let mechanism_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
        mechanism_box.add_css_class("login-dialog-auth-menu-item-section");
        let mechanism_heading = gtk::Label::new(Some("Login Options"));
        mechanism_heading.add_css_class("heading");
        mechanism_heading.set_halign(gtk::Align::Start);
        mechanism_box.append(&mechanism_heading);
        mechanism_box.set_visible(false);
        let menu = gtk::Box::new(gtk::Orientation::Vertical, 12);
        menu.append(&mechanism_box);
        menu.append(&session_box);
        let popover = gtk::Popover::new();
        popover.add_css_class("login-dialog-auth-menu-button-popup");
        popover.set_child(Some(&menu));
        let options = gtk::MenuButton::new();
        options.set_icon_name("emblem-system-symbolic");
        options.add_css_class("login-dialog-auth-menu-button");
        options.add_css_class("login-dialog-button");
        options.set_tooltip_text(Some("Login Options"));
        options.update_property(&[gtk::accessible::Property::Label("Login Options")]);
        options.set_popover(Some(&popover));
        options.set_visible(false);
        let bottom = gtk::CenterBox::new();
        bottom.add_css_class("login-dialog-bottom-button-group");
        bottom.set_end_widget(Some(&options));
        if let Some(path) = &policy.logo {
            let logo = gtk::Image::from_file(path);
            logo.add_css_class("login-dialog-logo-bin");
            logo.set_pixel_size(48);
            bottom.set_center_widget(Some(&logo));
        }

        let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
        column.append(&panel);
        column.append(&banner);
        column.append(&stack);
        column.append(&bottom);
        let conflict = conflict::Conflict::new(&column);
        window.set_child(Some(&conflict.toasts));

        let greeter = Rc::new(Self {
            window,
            stack,
            user_list,
            avatar,
            name,
            entry,
            entry_area,
            button_box,
            auth_ui,
            cancel,
            next,
            caps,
            message,
            options,
            session_box,
            session_buttons: RefCell::default(),
            mechanism_box,
            mechanism_buttons: RefCell::default(),
            machine: RefCell::new(AuthMachine::new(multiplier)),
            host,
            policy,
            system: RefCell::default(),
            channels: channel::Channels::default(),
            accounts_watch: RefCell::default(),
            logind_watch: RefCell::default(),
            smartcard: RefCell::default(),
            own_session: RefCell::default(),
            own_active: Cell::new(None),
            users: RefCell::default(),
            logged_in: RefCell::default(),
            prompt_user: RefCell::default(),
            timed: timed_login::Timed::default(),
            conflict,
            asking_username: Cell::new(false),
            chosen_session: RefCell::default(),
            applying: Cell::new(false),
            registered: Cell::new(false),
        });
        greeter.wire(&not_listed, &password, &text);
        greeter.wire_conflict();
        greeter.fill_sessions();
        greeter
    }

    fn wire(
        self: &Rc<Self>,
        not_listed: &gtk::Button,
        password: &gtk::PasswordEntry,
        text: &gtk::Entry,
    ) {
        let weak = Rc::downgrade(self);
        self.user_list.connect_row_activated(move |_, row| {
            let Some(greeter) = weak.upgrade() else {
                return;
            };
            let index = usize::try_from(row.index()).unwrap_or(usize::MAX);
            let user = greeter.users.borrow().get(index).cloned();
            if let Some(user) = user {
                greeter.conflict.dismiss_notice();
                greeter.choose_user(&user.name, Some(&user));
            }
        });
        let weak = Rc::downgrade(self);
        not_listed.connect_clicked(move |_| {
            if let Some(greeter) = weak.upgrade() {
                greeter.ask_username();
            }
        });
        let weak = Rc::downgrade(self);
        self.cancel.connect_clicked(move |_| {
            if let Some(greeter) = weak.upgrade() {
                greeter.back();
            }
        });
        let entry = self.entry.clone();
        self.next.connect_clicked(move |_| {
            entry.current().emit_by_name::<()>("activate", &[]);
        });
        for editable in [password.upcast_ref::<gtk::Editable>(), text.upcast_ref()] {
            let weak = Rc::downgrade(self);
            editable.connect_changed(move |editable| {
                let Some(greeter) = weak.upgrade() else {
                    return;
                };
                if greeter.applying.get() || greeter.asking_username.get() {
                    return;
                }
                let has_text = !editable.text().is_empty();
                greeter.drive(|machine, _| machine.text_changed(has_text));
            });
        }
        let weak = Rc::downgrade(self);
        password.connect_activate(move |entry| {
            if let Some(greeter) = weak.upgrade() {
                greeter.submit(entry.text().to_string());
            }
        });
        let weak = Rc::downgrade(self);
        text.connect_activate(move |entry| {
            if let Some(greeter) = weak.upgrade() {
                greeter.submit(entry.text().to_string());
            }
        });
        // Escape is Back (authPrompt.js `cancel`).
        let keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, _| {
            let Some(greeter) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            if key == gtk::gdk::Key::Escape && greeter.conflict.is_open() {
                greeter.cancel_conflict();
                return glib::Propagation::Stop;
            }
            if key == gtk::gdk::Key::Escape && greeter.on_prompt() {
                greeter.back();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        self.window.add_controller(keys);
        // GNOME's caps-lock warning under a hidden entry.
        if let Some(keyboard) = gtk::gdk::Display::default()
            .and_then(|d| d.default_seat())
            .and_then(|s| s.keyboard())
        {
            let weak = Rc::downgrade(self);
            keyboard.connect_caps_lock_state_notify(move |_| {
                if let Some(greeter) = weak.upgrade() {
                    greeter.render();
                }
            });
        }
        let weak = Rc::downgrade(self);
        self.window.connect_map(move |_| {
            if let Some(greeter) = weak.upgrade() {
                greeter.register();
            }
        });
        // GNOME's menus give the key focus back to where it was when they
        // close (grabHelper.js `ungrab`): after Login Options, typing goes
        // to the prompt again, not to the menu button.
        if let Some(popover) = self.options.popover() {
            let weak = Rc::downgrade(self);
            popover.connect_closed(move |_| {
                if let Some(greeter) = weak.upgrade() {
                    if greeter.on_prompt() && greeter.entry.current().is_sensitive() {
                        greeter.entry.focus();
                    }
                }
            });
            // The menu opens on the checked session type, as before the
            // mechanisms got their section; they are a Tab away.
            let weak = Rc::downgrade(self);
            popover.connect_show(move |_| {
                let Some(greeter) = weak.upgrade() else {
                    return;
                };
                let session = greeter
                    .session_buttons
                    .borrow()
                    .iter()
                    .find(|(_, b)| b.is_active())
                    .map(|(_, b)| b.clone().upcast::<gtk::Widget>())
                    .filter(|_| greeter.session_box.is_visible());
                let mechanism = greeter
                    .mechanism_buttons
                    .borrow()
                    .iter()
                    .find(|(_, b)| b.is_active())
                    .map(|(_, b)| b.clone().upcast::<gtk::Widget>());
                if let Some(target) = session.or(mechanism) {
                    glib::idle_add_local_once(move || {
                        target.grab_focus();
                    });
                }
            });
        }
        // The list, the button and web login's Done answer the step;
        // web login's Cancel is Back.
        let (choose, back) = (Rc::downgrade(self), Rc::downgrade(self));
        self.auth_ui.connect(
            move |answer| {
                if let Some(greeter) = choose.upgrade() {
                    greeter.drive(|machine, host| machine.choose(host, answer));
                }
            },
            move || {
                if let Some(greeter) = back.upgrade() {
                    greeter.back();
                }
            },
        );
        if let Some(settings) = &self.policy.settings {
            let weak = Rc::downgrade(self);
            settings.connect_changed(None, move |settings, key| {
                if key.starts_with("enable-") || key == "allowed-failures" {
                    if let Some(greeter) = weak.upgrade() {
                        greeter.host.set_policy(Policy::read_auth(settings));
                        greeter.drive(|_, _| ());
                    }
                }
            });
        }
    }

    fn on_prompt(&self) -> bool {
        self.stack.visible_child_name().as_deref() == Some("prompt")
    }

    /// The sessions of Login Options, and GDM's default among them.
    fn fill_sessions(self: &Rc<Self>) {
        let data_dirs: Vec<std::path::PathBuf> = glib::system_data_dirs();
        let supported = std::env::var("GDM_SUPPORTED_SESSION_TYPES").ok();
        let dirs = sessions::search_dirs(&data_dirs, supported.as_deref());
        let found = sessions::list(&dirs, std::env::var_os("PATH").as_deref());
        trace(&format!(
            "greeter sessions ids={}",
            found
                .iter()
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>()
                .join(",")
        ));
        let mut group: Option<gtk::CheckButton> = None;
        let mut buttons = Vec::new();
        for session in found {
            let button = gtk::CheckButton::with_label(&session.name);
            if let Some(first) = &group {
                button.set_group(Some(first));
            } else {
                group = Some(button.clone());
            }
            let weak = Rc::downgrade(self);
            let id = session.id.clone();
            button.connect_toggled(move |button| {
                let Some(greeter) = weak.upgrade() else {
                    return;
                };
                if button.is_active() && !greeter.applying.get() {
                    greeter.choose_session(&id);
                }
            });
            self.session_box.append(&button);
            buttons.push((session.id, button));
        }
        *self.session_buttons.borrow_mut() = buttons;
    }

    /// GDM's `DefaultSessionNameChanged`: the user's saved session, else
    /// GDM's fallback; shown unless a session was picked.
    fn show_default_session(&self, id: &str) {
        if self.chosen_session.borrow().is_some() {
            return;
        }
        self.mark_session(id);
    }

    fn mark_session(&self, id: &str) {
        let was = self.applying.replace(true);
        for (session, button) in self.session_buttons.borrow().iter() {
            if session == id {
                button.set_active(true);
            }
        }
        self.applying.set(was);
    }

    fn choose_session(self: &Rc<Self>, id: &str) {
        trace(&format!("greeter select-session id={id}"));
        *self.chosen_session.borrow_mut() = Some(id.to_owned());
        self.send(Call::SelectSession {
            session: id.to_owned(),
        });
        self.options.popdown();
    }

    // --- pages -------------------------------------------------------

    /// authPrompt.js `reset()` for good, then loginDialog.js `_onReset`:
    /// the user list (or the username question), or straight to
    /// verification when no username is needed (only smartcard allowed).
    fn show_users(self: &Rc<Self>) {
        self.asking_username.set(false);
        self.drive(|machine, host| machine.stop(host));
        self.host.reset();
        self.host.set_user(None);
        *self.prompt_user.borrow_mut() = None;
        let have_users = !self.users.borrow().is_empty();
        match landing(
            self.host.needs_username(),
            self.policy.disable_user_list,
            have_users,
        ) {
            Landing::NoUser => {
                self.begin_without_user();
                return;
            }
            Landing::Username => {
                self.ask_username();
                return;
            }
            Landing::Users => {}
        }
        self.options.set_visible(false);
        self.stack.set_visible_child_name("users");
        if let Some(row) = self.user_list.row_at_index(0) {
            row.grab_focus();
        }
        trace("greeter page users");
    }

    fn ask_username(self: &Rc<Self>) {
        self.drive(|machine, host| machine.stop(host));
        self.host.reset();
        self.host.set_user(None);
        *self.prompt_user.borrow_mut() = None;
        self.asking_username.set(true);
        self.avatar.set_visible(false);
        self.name.set_visible(false);
        self.options.set_visible(false);
        self.stack.set_visible_child_name("prompt");
        self.render();
        self.entry.focus();
        trace("greeter page username");
    }

    /// `_hideUserListAndBeginVerification`: the conversation finds its
    /// user (a smartcard's certificate).
    fn begin_without_user(self: &Rc<Self>) {
        self.asking_username.set(false);
        self.avatar.set_visible(false);
        self.name.set_visible(false);
        self.options.set_visible(true);
        self.session_box.set_visible(false);
        self.stack.set_visible_child_name("prompt");
        trace("greeter page prompt user=");
        self.drive(|machine, host| machine.show_prompt(host, false));
    }

    fn choose_user(self: &Rc<Self>, name: &str, user: Option<&accounts::User>) {
        self.asking_username.set(false);
        self.host.set_user(Some(name.to_owned()));
        *self.prompt_user.borrow_mut() = Some(name.to_owned());
        // `_updateSessions`: a logged-in user cannot choose a session.
        let logged_in = self.logged_in.borrow().iter().any(|u| u == name);
        self.session_box.set_visible(!logged_in);
        let shown = user.map_or(name, accounts::User::display_name);
        self.avatar.set_text(Some(shown));
        self.avatar.set_custom_image(
            user.and_then(accounts::User::icon)
                .and_then(|path| gtk::gdk::Texture::from_filename(path).ok())
                .as_ref(),
        );
        self.avatar.set_visible(true);
        self.name.set_label(shown);
        self.name.set_visible(true);
        self.options.set_visible(true);
        self.stack.set_visible_child_name("prompt");
        trace(&format!("greeter page prompt user={name}"));
        self.drive(|machine, host| machine.show_prompt(host, false));
    }

    fn back(self: &Rc<Self>) {
        if self.asking_username.get() {
            let have_users = !self.users.borrow().is_empty();
            if landing(true, self.policy.disable_user_list, have_users) == Landing::Users {
                self.show_users();
            }
            return;
        }
        if self.drive(|machine, host| machine.cancel(host)) {
            self.show_users();
        }
    }

    fn submit(self: &Rc<Self>, text: String) {
        if self.applying.get() {
            return;
        }
        if self.asking_username.get() {
            let Some((name, user)) = typed_user(&text, &self.users.borrow()) else {
                return;
            };
            self.entry.clear();
            self.choose_user(&name, user.as_ref());
            return;
        }
        self.drive(|machine, host| machine.submit(host, text));
    }

    // --- the AuthPrompt machine ----------------------------------------

    fn drive<R>(self: &Rc<Self>, input: impl FnOnce(&mut AuthMachine, &Conversations) -> R) -> R {
        let result = input(&mut self.machine.borrow_mut(), &self.host);
        // What the conversations produced on their own (a local step, a
        // soft reset, new mechanisms) is handled before anything shows.
        loop {
            let inputs = self.host.take_inputs();
            if inputs.is_empty() {
                break;
            }
            for input in inputs {
                self.apply_input(input);
            }
        }
        self.render();
        result
    }

    fn feed(self: &Rc<Self>, inputs: Vec<Input>) {
        for input in inputs {
            self.apply_input(input);
        }
        // Also what the conversations queued on their own.
        self.drive(|_, _| ());
    }

    fn apply_input(self: &Rc<Self>, input: Input) {
        let left = to_machine(&mut self.machine.borrow_mut(), &self.host, input);
        match left {
            Left::Nothing => {}
            Left::Verified(request) => {
                if self.channels.is_reauth(request) {
                    // GDM switches to the user's running session.
                    let user = self.prompt_user.borrow().clone().unwrap_or_default();
                    trace(&format!("greeter reauthenticated user={user}"));
                }
            }
            Left::Mechanisms => self.mechanisms_changed(),
            Left::Timer { token, secs } => {
                let weak = Rc::downgrade(self);
                glib::timeout_add_seconds_local_once(
                    u32::try_from(secs).unwrap_or(u32::MAX),
                    move || {
                        if let Some(greeter) = weak.upgrade() {
                            greeter.host.timeout(token);
                            greeter.drive(|_, _| ());
                        }
                    },
                );
            }
        }
    }

    /// loginDialog.js `_onMechanismsChanged`: Login Options lists the
    /// mechanisms, and the selected one (else the first) is selected.
    fn mechanisms_changed(self: &Rc<Self>) {
        let (list, selected) = self.host.mechanisms();
        while let Some(child) = self.mechanism_box.last_child() {
            if self.mechanism_box.first_child().as_ref() == Some(&child) {
                break;
            }
            self.mechanism_box.remove(&child);
        }
        let mut group: Option<gtk::ToggleButton> = None;
        let mut buttons = Vec::new();
        let was = self.applying.replace(true);
        for mechanism in &list {
            let button = gtk::ToggleButton::with_label(&mechanism.name);
            button.add_css_class("flat");
            button.add_css_class("login-dialog-auth-menu-item");
            if let Some(first) = &group {
                button.set_group(Some(first));
            } else {
                group = Some(button.clone());
            }
            button.set_active(selected.as_ref() == Some(mechanism));
            let weak = Rc::downgrade(self);
            let chosen = mechanism.clone();
            button.connect_toggled(move |button| {
                let Some(greeter) = weak.upgrade() else {
                    return;
                };
                if button.is_active() && !greeter.applying.get() {
                    greeter.options.popdown();
                    // Off the toggle: the selection redraws this menu.
                    let (greeter, chosen) = (greeter.clone(), chosen.clone());
                    glib::idle_add_local_once(move || {
                        greeter.select_mechanism(&chosen);
                        greeter.drive(|_, _| ());
                    });
                }
            });
            self.mechanism_box.append(&button);
            buttons.push((mechanism.clone(), button));
        }
        self.applying.set(was);
        self.mechanism_box.set_visible(!list.is_empty());
        *self.mechanism_buttons.borrow_mut() = buttons;
        trace(&format!(
            "greeter mechanisms {}",
            mechanisms_trace(&list, selected.as_ref())
        ));
        if let Some(default) = self.host.default_mechanism() {
            self.select_mechanism(&default);
        }
    }

    /// loginDialog.js `_selectAuthMechanism` → authPrompt.js
    /// `selectMechanism`.
    fn select_mechanism(self: &Rc<Self>, mechanism: &Mechanism) {
        let mut machine = self.machine.borrow_mut();
        if machine.can_select() {
            trace(&format!(
                "greeter select-mechanism service={} role={}",
                mechanism.service,
                mechanism.role.name()
            ));
        }
        select_mechanism(&mut machine, &self.host, mechanism);
    }

    fn apply(&self, view: &View) {
        let asking = self.asking_username.get();
        let (secret, hint, sensitive) = if asking {
            (false, USERNAME_QUESTION, true)
        } else {
            (view.secret, view.hint.as_str(), view.sensitive)
        };
        self.entry.set_secret(secret);
        self.entry
            .set_hint(if asking || view.hinted { hint } else { "" });
        // authPrompt.js: the entry area shows for a question (or a
        // message); web login hides the whole row, Back included.
        self.entry_area.set_visible(asking || view.entry);
        self.button_box
            .set_visible(asking || !matches!(view.ask, Some(Ask::WebLogin { .. })));
        self.auth_ui.render(view);
        if !sensitive && self.entry.has_focus() {
            GtkWindowExt::set_focus(&self.window, gtk::Widget::NONE);
        }
        self.entry.set_sensitive(sensitive);
        self.next.set_can_target(sensitive);
        // The login dialog's Back goes to the user list from any step.
        self.cancel.set_visible(
            view.back
                || !asking
                || (!self.policy.disable_user_list && !self.users.borrow().is_empty()),
        );
        let caps_locked = gtk::gdk::Display::default()
            .and_then(|d| d.default_seat())
            .and_then(|s| s.keyboard())
            .is_some_and(|k| k.is_caps_locked());
        self.caps.set_visible(caps_warning(secret, caps_locked));
        let (text, kind) = match (&view.message, asking) {
            (Some((text, kind)), false) => (text.as_str(), Some(*kind)),
            _ => ("", None),
        };
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

    fn render(self: &Rc<Self>) {
        let (view, effects) = {
            let mut machine = self.machine.borrow_mut();
            (machine.view(), machine.take_effects())
        };
        let was = self.applying.replace(true);
        self.apply(&view);
        for effect in effects {
            match effect {
                Effect::Timer { token, ms } => {
                    let weak = Rc::downgrade(self);
                    glib::timeout_add_local_once(std::time::Duration::from_millis(ms), move || {
                        if let Some(greeter) = weak.upgrade() {
                            greeter.drive(|machine, host| machine.timeout(host, token));
                        }
                    });
                }
                Effect::ClearEntry => self.entry.clear(),
                Effect::Focus => {
                    if view.sensitive && self.on_prompt() && view.ask.is_none() {
                        self.entry.focus();
                    }
                }
                // GNOME's wiggle is motion the greeter does not draw yet.
                Effect::WiggleEntry | Effect::WiggleMessage | Effect::ShowPrompt => {}
                Effect::Announce(text) => self
                    .message
                    .announce(&text, gtk::AccessibleAnnouncementPriority::High),
                Effect::Trace(line) => trace(line.strip_prefix("lock ").unwrap_or(&line)),
                Effect::GiveUp => {
                    trace("greeter give-up");
                    let weak = Rc::downgrade(self);
                    glib::idle_add_local_once(move || {
                        if let Some(greeter) = weak.upgrade() {
                            greeter.show_users();
                        }
                    });
                }
            }
        }
        self.applying.set(was);
        for (call, request) in self.host.take_outgoing() {
            self.send_for(call, request);
        }
    }

    // --- GDM -----------------------------------------------------------

    fn on_signal(self: &Rc<Self>, signal: Signal) {
        if let (Some(service), Signal::SessionOpened { session_id, .. }) =
            (session_to_start(&signal), &signal)
        {
            // loginDialog.js `_onSessionOpened`: a conflicting session's
            // dialog, else `_startSession`.
            self.session_opened(service.to_owned(), session_id.clone());
            return;
        }
        match &signal {
            Signal::DefaultSessionNameChanged { session } => {
                trace(&format!("greeter default-session id={session}"));
                self.show_default_session(session);
            }
            Signal::TimedLoginRequested { user, delay } => {
                self.on_timed_login_requested(user, *delay);
            }
            _ => {
                let inputs = self.host.on_signal(&signal);
                self.feed(inputs);
            }
        }
    }

    /// The greeter's channel closed. A login that verified (or waited on
    /// a conflicting session) has nothing left to start: back to the list.
    fn on_gdm_closed(self: &Rc<Self>) {
        if self.conflict.is_open() {
            self.cancel_conflict();
        } else if self.machine.borrow().succeeded() {
            self.show_users();
        }
    }

    /// Ask GDM for its channel and connect; retried until it works, and
    /// again whenever the channel closes (loginDialog.js
    /// `_ensureGreeterProxy`).
    fn connect(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            loop {
                let Some(greeter) = weak.upgrade() else {
                    return;
                };
                match greeter.try_connect().await {
                    Ok(()) => return,
                    Err(error) => {
                        eprintln!("tuna-greeter: GDM: {}", error.message());
                        drop(greeter);
                        glib::timeout_future(RECONNECT).await;
                    }
                }
            }
        });
    }

    async fn try_connect(self: &Rc<Self>) -> Result<(), glib::Error> {
        let known = self.system.borrow().clone();
        let system = match known {
            Some(system) => system,
            None => gio::bus_get_future(gio::BusType::System).await?,
        };
        *self.system.borrow_mut() = Some(system.clone());
        let address = gdm::open_session(&system).await?;
        let connection = gdm::connect(&address).await?;
        let channel = self.attach(connection);
        // libgdm: the extensions when it makes the UserVerifier proxy,
        // the timed login details when it makes the Greeter proxy.
        let (interface, method, args) = Call::EnableExtensions.wire();
        if let Err(error) = channel
            .connection
            .call_future(
                None,
                gdm::SESSION_PATH,
                interface,
                method,
                Some(&args),
                None,
                gio::DBusCallFlags::NONE,
                -1,
            )
            .await
        {
            eprintln!("tuna-greeter: EnableExtensions: {}", error.message());
        }
        *self.channels.main.borrow_mut() = Some(channel);
        trace("greeter gdm-connected");
        self.send(Call::GetTimedLoginDetails);
        if self.accounts_watch.borrow().is_none() {
            let weak = Rc::downgrade(self);
            let watch = accounts::watch(&system, move || {
                if let Some(greeter) = weak.upgrade() {
                    greeter.load_users();
                }
            });
            *self.accounts_watch.borrow_mut() = Some(watch);
            self.watch_logind(&system).await;
            self.load_users();
        }
        if self.smartcard.borrow().is_none() {
            if let Ok(session) = gio::bus_get_future(gio::BusType::Session).await {
                let weak = Rc::downgrade(self);
                let watch = smartcard::watch(&session, move |inserted, any| {
                    if let Some(greeter) = weak.upgrade() {
                        trace(&format!("greeter smartcard inserted={inserted}"));
                        greeter.host.smartcard_changed(any);
                        greeter.drive(|_, _| ());
                    }
                });
                *self.smartcard.borrow_mut() = Some(watch);
            }
        }
        self.register();
        Ok(())
    }

    /// Follow logind: who is logged in, sessions going away, and the
    /// login screen's own session becoming active again.
    async fn watch_logind(self: &Rc<Self>, system: &gio::DBusConnection) {
        let own = logind::own_session_path(system).await;
        trace(&format!(
            "greeter own-session path={}",
            own.as_deref().unwrap_or_default()
        ));
        *self.own_session.borrow_mut() = own;
        let weak = Rc::downgrade(self);
        let watch = logind::watch(system, move |news| {
            let Some(greeter) = weak.upgrade() else {
                return;
            };
            match news {
                logind::News::SessionNew => greeter.load_logged_in(),
                logind::News::SessionRemoved(id) => {
                    greeter.session_removed(&id);
                    greeter.load_logged_in();
                }
                logind::News::Changed(path) => {
                    if greeter.own_session.borrow().as_deref() == Some(path.as_str()) {
                        greeter.check_own_session();
                    }
                }
            }
        });
        *self.logind_watch.borrow_mut() = Some(watch);
        self.check_own_session();
    }

    /// loginDialog.js `_loginScreenSessionActivated`: back on the login
    /// screen after a switch, a prompt that was verifying starts over.
    fn check_own_session(self: &Rc<Self>) {
        let (Some(system), Some(path)) = (
            self.system.borrow().clone(),
            self.own_session.borrow().clone(),
        ) else {
            return;
        };
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Ok(session) = logind::session(&system, &path).await else {
                return;
            };
            let Some(greeter) = weak.upgrade() else {
                return;
            };
            let was = greeter.own_active.replace(Some(session.active));
            if login_screen_activated(was, session.active) {
                trace("greeter login-screen-activated");
                if greeter.machine.borrow().verifying() {
                    greeter.show_users();
                }
            }
        });
    }

    fn load_logged_in(self: &Rc<Self>) {
        let Some(system) = self.system.borrow().clone() else {
            return;
        };
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let users = logind::logged_in_users(&system).await;
            if let Some(greeter) = weak.upgrade() {
                *greeter.logged_in.borrow_mut() = users;
                greeter.decorate_rows();
            }
        });
    }

    /// The user list's `logged-in` ring (`UserListItem._onUserChanged`).
    fn decorate_rows(&self) {
        let logged_in = self.logged_in.borrow();
        let marks = logged_in_rows(&self.users.borrow(), &logged_in);
        for (index, on) in marks.into_iter().enumerate() {
            let Some(row) = i32::try_from(index)
                .ok()
                .and_then(|i| self.user_list.row_at_index(i))
            else {
                continue;
            };
            set_class(&row, "logged-in", on);
            row.update_property(&[gtk::accessible::Property::Description(if on {
                LOGGED_IN
            } else {
                ""
            })]);
        }
        trace(&format!("greeter logged-in names={}", logged_in.join(",")));
    }

    fn load_users(self: &Rc<Self>) {
        let Some(system) = self.system.borrow().clone() else {
            return;
        };
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let users = match accounts::load(&system).await {
                Ok(users) => users,
                Err(error) => {
                    eprintln!("tuna-greeter: AccountsService: {}", error.message());
                    Vec::new()
                }
            };
            let Some(greeter) = weak.upgrade() else {
                return;
            };
            greeter.set_users(users);
        });
    }

    fn set_users(self: &Rc<Self>, users: Vec<accounts::User>) {
        trace(&format!(
            "greeter users names={}",
            users
                .iter()
                .map(|u| u.name.as_str())
                .collect::<Vec<_>>()
                .join(",")
        ));
        while let Some(row) = self.user_list.row_at_index(0) {
            self.user_list.remove(&row);
        }
        let mut indicators = std::collections::HashMap::new();
        for user in &users {
            let avatar = adw::Avatar::new(64, Some(user.display_name()), false);
            avatar.add_css_class("user-icon");
            avatar.set_custom_image(
                user.icon()
                    .and_then(|path| gtk::gdk::Texture::from_filename(path).ok())
                    .as_ref(),
            );
            let label = gtk::Label::new(Some(user.display_name()));
            label.add_css_class("user-widget-label");
            label.set_xalign(0.0);
            let content = gtk::Box::new(gtk::Orientation::Horizontal, 18);
            content.add_css_class("user-widget");
            content.append(&avatar);
            content.append(&label);
            let indicator = timed_login::indicator();
            let item = gtk::Box::new(gtk::Orientation::Vertical, 0);
            item.append(&content);
            item.append(&indicator);
            indicators.insert(user.name.clone(), indicator);
            let row = gtk::ListBoxRow::new();
            row.add_css_class("login-dialog-user-list-item");
            row.set_child(Some(&item));
            row.update_property(&[gtk::accessible::Property::Label(user.display_name())]);
            self.user_list.append(&row);
        }
        *self.timed.indicators.borrow_mut() = indicators;
        let first_load = self.users.borrow().is_empty() && !self.on_prompt();
        *self.users.borrow_mut() = users;
        self.decorate_rows();
        self.load_logged_in();
        if first_load || !self.on_prompt() {
            self.show_users();
        }
    }

    /// GNOME Shell registers its session at startup and its display once
    /// the login screen is up; the greeter does both once it is mapped
    /// and connected.
    fn register(self: &Rc<Self>) {
        if self.registered.get() || !self.window.is_mapped() {
            return;
        }
        let Some(system) = self.system.borrow().clone() else {
            return;
        };
        self.registered.set(true);
        glib::spawn_future_local(async move {
            gdm::register(&system).await;
            trace("greeter registered");
        });
    }
}

/// A user row's description when the user has a session.
const LOGGED_IN: &str = "Logged in";

/// `allowed-failures` and `enable-{password,smartcard,switchable}-authentication`;
/// every login gets at least one try.
fn auth_policy(
    allowed_failures: i32,
    password: bool,
    smartcard: bool,
    switchable: bool,
) -> AuthPolicy {
    AuthPolicy {
        allowed_failures: u32::try_from(allowed_failures).unwrap_or(0).max(1),
        password,
        smartcard,
        switchable,
    }
}

/// Where the login dialog lands on a reset (`_onReset`).
#[derive(Debug, PartialEq, Eq)]
enum Landing {
    /// No username is needed (smartcard alone is allowed): verification
    /// begins without one (`_hideUserListAndBeginVerification`).
    NoUser,
    /// "Not listed?"'s question: the list is disabled or empty.
    Username,
    Users,
}

fn landing(needs_username: bool, disable_user_list: bool, have_users: bool) -> Landing {
    if !needs_username {
        Landing::NoUser
    } else if disable_user_list || !have_users {
        Landing::Username
    } else {
        Landing::Users
    }
}

/// What is left of a conversation input once the machine took its part.
#[derive(Debug, PartialEq, Eq)]
enum Left {
    Nothing,
    /// The request verified.
    Verified(u64),
    /// Login Options' mechanisms changed.
    Mechanisms,
    /// Call `Conversations::timeout(token)` after `secs`.
    Timer {
        token: u64,
        secs: u64,
    },
}

fn to_machine(machine: &mut AuthMachine, host: &Conversations, input: Input) -> Left {
    match input {
        Input::Event {
            request,
            service,
            event,
        } => machine.event(host, request, service, &event),
        Input::Result { request, applied } => {
            machine.result(host, request, applied);
            if applied {
                return Left::Verified(request);
            }
        }
        Input::Ask {
            request,
            sequence,
            ask,
        } => machine.ask(host, request, sequence, ask),
        Input::Restart => machine.restart(host),
        Input::Mechanisms => return Left::Mechanisms,
        Input::Timer { token, secs } => return Left::Timer { token, secs },
    }
    Left::Nothing
}

/// `list=SERVICE:ROLE,… selected=SERVICE:ROLE` of the mechanisms trace.
fn mechanisms_trace(list: &[Mechanism], selected: Option<&Mechanism>) -> String {
    let item = |m: &Mechanism| format!("{}:{}", m.service, m.role.name());
    format!(
        "list={} selected={}",
        list.iter().map(item).collect::<Vec<_>>().join(","),
        selected.map(item).unwrap_or_default()
    )
}

/// authPrompt.js `selectMechanism`: unless verification succeeded or an
/// answer is being checked, the chosen mechanism starts at step 0 (the old
/// step stays when nothing changed). Whether it changed.
fn select_mechanism(
    machine: &mut AuthMachine,
    host: &Conversations,
    mechanism: &Mechanism,
) -> bool {
    if !machine.can_select() {
        return false;
    }
    let old = machine.prompt_step();
    machine.set_prompt_step(0);
    if host.select(mechanism) {
        return true;
    }
    machine.set_prompt_step(old);
    false
}

/// loginDialog.js `_loginScreenSessionActivated`: the login screen's own
/// session became active again (back from a user's session).
fn login_screen_activated(was: Option<bool>, active: bool) -> bool {
    active && was == Some(false)
}

/// Each listed user's `logged-in` ring (`UserListItem._onUserChanged`).
fn logged_in_rows(users: &[accounts::User], logged_in: &[String]) -> Vec<bool> {
    users.iter().map(|u| logged_in.contains(&u.name)).collect()
}

fn set_class(widget: &impl IsA<gtk::Widget>, class: &str, on: bool) {
    if on {
        widget.add_css_class(class);
    } else {
        widget.remove_css_class(class);
    }
}

/// authPrompt.js `_updateEntry`: the caps-lock warning is part of a
/// secret question only, shown while the lock is on (`CapsLockWarning`).
fn caps_warning(secret: bool, caps_locked: bool) -> bool {
    secret && caps_locked
}

/// "Not listed?": the username typed (trimmed; nothing for blank input),
/// and that user's list entry when the list has one (its name and avatar
/// then show on the prompt).
fn typed_user(text: &str, users: &[accounts::User]) -> Option<(String, Option<accounts::User>)> {
    let name = text.trim();
    if name.is_empty() {
        return None;
    }
    let user = users.iter().find(|u| u.name == name).cloned();
    Some((name.to_owned(), user))
}

/// loginDialog.js `_onSessionOpened`: GDM opened the session of a
/// conversation it verified (`SessionOpened` comes after
/// `VerificationComplete`); that service's session is to start. No other
/// signal starts one.
fn session_to_start(signal: &Signal) -> Option<&str> {
    match signal {
        Signal::SessionOpened { service, .. } => Some(service),
        _ => None,
    }
}

/// Whether the panel offers the power actions: on hardware only (a nested
/// login screen's logind seat is the host's, `TUNA_SESSION_KIND`) and not
/// under `disable-restart-buttons`.
fn offers_power(session_kind: Option<&str>, policy: &Policy) -> bool {
    session_kind == Some("hardware") && !policy.disable_restart_buttons
}

/// The system menu's power actions, when [`offers_power`].
fn power_menu(policy: &Policy) -> Option<gtk::MenuButton> {
    if !offers_power(std::env::var("TUNA_SESSION_KIND").ok().as_deref(), policy) {
        return None;
    }
    let menu = gtk::Box::new(gtk::Orientation::Vertical, 0);
    for (label, method) in [
        ("Suspend", "Suspend"),
        ("Restart…", "Reboot"),
        ("Power Off…", "PowerOff"),
    ] {
        let button = gtk::Button::with_label(label);
        button.add_css_class("flat");
        button.connect_clicked(move |_| {
            glib::spawn_future_local(async move {
                let Ok(system) = gio::bus_get_future(gio::BusType::System).await else {
                    return;
                };
                if let Err(error) = system
                    .call_future(
                        Some("org.freedesktop.login1"),
                        "/org/freedesktop/login1",
                        "org.freedesktop.login1.Manager",
                        method,
                        Some(&(true,).to_variant()),
                        None,
                        gio::DBusCallFlags::ALLOW_INTERACTIVE_AUTHORIZATION,
                        -1,
                    )
                    .await
                {
                    eprintln!("tuna-greeter: {method}: {}", error.message());
                }
            });
        });
        menu.append(&button);
    }
    let popover = gtk::Popover::new();
    popover.set_child(Some(&menu));
    let button = gtk::MenuButton::new();
    button.set_icon_name("system-shutdown-symbolic");
    button.add_css_class("flat");
    button.update_property(&[gtk::accessible::Property::Label("Power Off / Log Out")]);
    button.set_popover(Some(&popover));
    Some(button)
}

/// Release version stamped at build time: `TUNA_VERSION` (a `vX.Y.Z` tag
/// or plain `X.Y.Z`) wins, otherwise the crate version, as the other
/// binaries do it.
fn normalize_version<'a>(raw: Option<&'a str>, fallback: &'a str) -> &'a str {
    match raw {
        Some(v) if !v.is_empty() => v.strip_prefix('v').unwrap_or(v),
        _ => fallback,
    }
}

fn release_version() -> &'static str {
    normalize_version(option_env!("TUNA_VERSION"), env!("CARGO_PKG_VERSION"))
}

/// What `--version` (or `-V`) anywhere on the command line prints.
fn version_line(mut args: impl Iterator<Item = String>) -> Option<String> {
    args.any(|a| a == "--version" || a == "-V")
        .then(|| format!("tuna-greeter {}", release_version()))
}

fn main() -> glib::ExitCode {
    if let Some(line) = version_line(std::env::args()) {
        println!("{line}");
        return glib::ExitCode::SUCCESS;
    }
    glib::set_prgname(Some("tuna-greeter"));
    let app = adw::Application::builder()
        .application_id("org.tuna.Greeter")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.connect_activate(|app| {
        adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);
        if let Some(display) = gtk::gdk::Display::default() {
            let provider = gtk::CssProvider::new();
            provider.load_from_string(include_str!("style.css"));
            gtk::style_context_add_provider_for_display(
                &display,
                &provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
        let greeter = Greeter::new(app);
        greeter.window.present();
        greeter.connect();
        GREETER.with(|g| *g.borrow_mut() = Some(greeter));
    });
    app.run_with_args::<&str>(&[])
}

#[cfg(test)]
mod tests {
    use super::*;
    use conversation::{PASSWORD_SERVICE, SMARTCARD_SERVICE};

    /// GDM 51.1.1's own `org.gnome.login-screen`, as the proof installs it.
    const SCHEMA: &str = include_str!("../../../scripts/lib/org.gnome.login-screen.gschema.xml");

    fn schema_default(key: &str) -> &'static str {
        let at = SCHEMA
            .find(&format!("<key name=\"{key}\""))
            .unwrap_or_else(|| panic!("{key} is not in the schema"));
        let rest = &SCHEMA[at..];
        let start = rest.find("<default>").unwrap() + "<default>".len();
        let end = rest.find("</default>").unwrap();
        rest[start..end].trim()
    }

    #[test]
    fn the_schemas_defaults_are_the_default_policy() {
        let on = |key| schema_default(key) == "true";
        assert_eq!(
            auth_policy(
                schema_default("allowed-failures").parse().unwrap(),
                on("enable-password-authentication"),
                on("enable-smartcard-authentication"),
                on("enable-switchable-authentication"),
            ),
            AuthPolicy::default()
        );
    }

    #[test]
    fn allowed_failures_leaves_at_least_one_try() {
        assert_eq!(auth_policy(5, true, true, true).allowed_failures, 5);
        assert_eq!(auth_policy(1, true, true, true).allowed_failures, 1);
        assert_eq!(auth_policy(0, true, true, true).allowed_failures, 1);
        assert_eq!(auth_policy(-3, true, true, true).allowed_failures, 1);
        assert_eq!(
            auth_policy(3, false, true, false),
            AuthPolicy {
                allowed_failures: 3,
                password: false,
                smartcard: true,
                switchable: false
            }
        );
    }

    #[test]
    fn a_reset_lands_on_the_list_the_question_or_no_user() {
        assert_eq!(landing(true, false, true), Landing::Users);
        assert_eq!(
            landing(true, true, true),
            Landing::Username,
            "disable-user-list"
        );
        assert_eq!(
            landing(true, false, false),
            Landing::Username,
            "nobody listed"
        );
        for (list_off, users) in [(false, true), (true, true), (false, false)] {
            assert_eq!(
                landing(false, list_off, users),
                Landing::NoUser,
                "smartcard alone needs no username"
            );
        }
        // Only smartcard allowed, switchable off: no username.
        let host = Conversations::new(auth_policy(3, false, true, false));
        assert_eq!(landing(host.needs_username(), false, true), Landing::NoUser);
        let host = Conversations::new(auth_policy(3, false, true, true));
        assert_eq!(landing(host.needs_username(), false, true), Landing::Users);
    }

    #[test]
    fn only_a_return_to_an_active_login_screen_starts_over() {
        assert!(login_screen_activated(Some(false), true));
        assert!(!login_screen_activated(None, true), "first look");
        assert!(!login_screen_activated(Some(true), true));
        assert!(!login_screen_activated(Some(true), false));
        assert!(!login_screen_activated(Some(false), false));
    }

    #[test]
    fn users_with_a_session_are_ringed() {
        let user = |name: &str| accounts::User {
            path: format!("/org/freedesktop/Accounts/User{name}"),
            name: name.into(),
            real_name: String::new(),
            icon_file: String::new(),
            system_account: false,
            locked: false,
        };
        let users = [user("ada"), user("mary"), user("annie")];
        assert_eq!(
            logged_in_rows(&users, &["annie".into(), "mary".into(), "root".into()]),
            vec![false, true, true]
        );
        assert_eq!(logged_in_rows(&users, &[]), vec![false; 3]);
        assert_eq!(LOGGED_IN, "Logged in");
    }

    /// The login dialog's loop: the machine takes the conversations'
    /// inputs; what is left of them comes back.
    fn pump(machine: &mut AuthMachine, host: &Conversations, inputs: Vec<Input>) -> Vec<Left> {
        let mut left: Vec<Left> = inputs
            .into_iter()
            .map(|input| to_machine(machine, host, input))
            .collect();
        loop {
            let inputs = host.take_inputs();
            if inputs.is_empty() {
                return left;
            }
            left.extend(inputs.into_iter().map(|i| to_machine(machine, host, i)));
        }
    }

    fn calls(host: &Conversations) -> Vec<Call> {
        host.take_outgoing().into_iter().map(|(c, _)| c).collect()
    }

    #[test]
    fn login_options_select_the_first_mechanism_and_start_it() {
        let host = Conversations::new(auth_policy(3, true, true, false));
        let mut machine = AuthMachine::new(1.0);
        host.set_user(Some("hedy".into()));
        machine.show_prompt(&host, false);
        assert_eq!(
            pump(&mut machine, &host, Vec::new()),
            vec![Left::Mechanisms]
        );
        assert!(calls(&host).is_empty(), "nothing selected yet");
        let (list, selected) = host.mechanisms();
        assert_eq!(
            mechanisms_trace(&list, selected.as_ref()),
            "list=gdm-password:password,gdm-smartcard:smartcard selected="
        );
        let default = host.default_mechanism().unwrap();
        assert!(select_mechanism(&mut machine, &host, &default));
        assert_eq!(machine.prompt_step(), 0);
        assert!(pump(&mut machine, &host, Vec::new()).contains(&Left::Mechanisms));
        let sent = calls(&host);
        assert_eq!(
            sent.last(),
            Some(&Call::BeginVerificationForUser {
                service: PASSWORD_SERVICE.into(),
                user: "hedy".into()
            }),
            "{sent:?}"
        );
        let (list, selected) = host.mechanisms();
        assert_eq!(
            mechanisms_trace(&list, selected.as_ref()),
            "list=gdm-password:password,gdm-smartcard:smartcard selected=gdm-password:password"
        );
        assert_eq!(host.default_mechanism(), None, "one is selected");
        // The selected one again changes nothing and keeps the step.
        let step = machine.prompt_step();
        assert!(!select_mechanism(&mut machine, &host, &list[0]));
        assert_eq!(machine.prompt_step(), step);
        assert!(host.take_inputs().is_empty());
        // Smartcard: a soft reset begins gdm-smartcard once gdm-password
        // stopped.
        assert!(select_mechanism(&mut machine, &host, &list[1]));
        pump(&mut machine, &host, Vec::new());
        let inputs = host.on_signal(&Signal::ConversationStopped {
            service: PASSWORD_SERVICE.into(),
        });
        pump(&mut machine, &host, inputs);
        let sent = calls(&host);
        assert_eq!(
            sent.last(),
            Some(&Call::BeginVerificationForUser {
                service: SMARTCARD_SERVICE.into(),
                user: "hedy".into()
            }),
            "{sent:?}"
        );
    }

    #[test]
    fn no_mechanism_is_chosen_while_an_answer_is_checked() {
        let host = Conversations::new(auth_policy(3, true, true, false));
        let mut machine = AuthMachine::new(1.0);
        host.set_user(Some("ada".into()));
        machine.show_prompt(&host, false);
        pump(&mut machine, &host, Vec::new());
        select_mechanism(&mut machine, &host, &host.default_mechanism().unwrap());
        pump(&mut machine, &host, Vec::new());
        calls(&host);
        let inputs = host.on_signal(&Signal::SecretInfoQuery {
            service: PASSWORD_SERVICE.into(),
            query: "Password: ".into(),
        });
        pump(&mut machine, &host, inputs);
        machine.submit(&host, "secret".into());
        assert!(matches!(
            calls(&host).as_slice(),
            [Call::AnswerQuery { .. }]
        ));
        assert!(!machine.can_select());
        let smartcard = host.mechanisms().0[1].clone();
        assert!(!select_mechanism(&mut machine, &host, &smartcard));
        assert_eq!(host.mechanisms().1.unwrap().service, PASSWORD_SERVICE);
        // Verified: what is left says so (a re-authentication's trace).
        let inputs = host.on_signal(&Signal::VerificationComplete {
            service: PASSWORD_SERVICE.into(),
        });
        let left = pump(&mut machine, &host, inputs);
        assert!(matches!(left.as_slice(), [Left::Verified(_)]), "{left:?}");
        assert!(machine.succeeded());
        assert!(!select_mechanism(&mut machine, &host, &smartcard));
    }

    fn keys(entries: &[(&str, glib::Variant)]) -> impl Fn(&str) -> glib::Variant {
        // GDM 51.1.1's defaults, overridden by `entries`.
        let mut values: std::collections::HashMap<String, glib::Variant> = [
            ("allowed-failures", 3i32.to_variant()),
            ("disable-user-list", false.to_variant()),
            ("banner-message-enable", false.to_variant()),
            ("banner-message-text", "".to_variant()),
            ("logo", "".to_variant()),
            ("disable-restart-buttons", false.to_variant()),
            ("enable-password-authentication", true.to_variant()),
            ("enable-smartcard-authentication", true.to_variant()),
            ("enable-switchable-authentication", true.to_variant()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .collect();
        for (key, value) in entries {
            values.insert((*key).to_owned(), value.clone());
        }
        move |key| values[key].clone()
    }

    #[test]
    fn login_screen_keys_make_the_policy() {
        let defaults = Policy::parse(keys(&[]));
        let none = Policy::default();
        assert_eq!(defaults.auth, none.auth);
        assert_eq!(defaults.disable_user_list, none.disable_user_list);
        assert_eq!(defaults.banner, none.banner);
        assert_eq!(defaults.logo, none.logo);
        assert_eq!(
            defaults.disable_restart_buttons,
            none.disable_restart_buttons
        );
        assert_eq!(none.auth.allowed_failures, 3, "the schema's default");

        let set = Policy::parse(keys(&[
            ("allowed-failures", 1i32.to_variant()),
            ("disable-user-list", true.to_variant()),
            ("banner-message-enable", true.to_variant()),
            ("banner-message-text", "Authorized use only.".to_variant()),
            ("logo", "/usr/share/pixmaps/logo.png".to_variant()),
            ("disable-restart-buttons", true.to_variant()),
        ]));
        assert_eq!(set.auth.allowed_failures, 1);
        assert!(set.disable_user_list);
        assert_eq!(set.banner.as_deref(), Some("Authorized use only."));
        assert_eq!(set.logo.as_deref(), Some("/usr/share/pixmaps/logo.png"));
        assert!(set.disable_restart_buttons);
    }

    #[test]
    fn banner_needs_enable_and_text_and_failures_allow_one_try() {
        let text_only = Policy::parse(keys(&[("banner-message-text", "Hi".to_variant())]));
        assert_eq!(text_only.banner, None, "banner-message-enable is off");
        let blank = Policy::parse(keys(&[
            ("banner-message-enable", true.to_variant()),
            ("banner-message-text", "  ".to_variant()),
        ]));
        assert_eq!(blank.banner, None);
        for failures in [0, -4] {
            let policy = Policy::parse(keys(&[("allowed-failures", failures.to_variant())]));
            assert_eq!(
                policy.auth.allowed_failures, 1,
                "{failures}: one try, as _canRetry"
            );
        }
        // A mistyped value (not the schema's) reads as the key's zero.
        let wrong = Policy::parse(keys(&[("disable-user-list", "yes".to_variant())]));
        assert!(!wrong.disable_user_list);
    }

    #[test]
    fn power_actions_need_hardware_and_restart_buttons() {
        let allowed = Policy::default();
        let disabled = Policy {
            disable_restart_buttons: true,
            ..Policy::default()
        };
        assert!(offers_power(Some("hardware"), &allowed));
        assert!(!offers_power(Some("hardware"), &disabled));
        assert!(!offers_power(Some("nested"), &allowed), "the host's logind");
        assert!(!offers_power(None, &allowed));
        assert!(!offers_power(Some("Hardware"), &allowed));
    }

    #[test]
    fn the_caps_lock_warning_belongs_to_secret_questions() {
        assert!(caps_warning(true, true));
        assert!(!caps_warning(true, false));
        assert!(
            !caps_warning(false, true),
            "a visible question (Username) warns of nothing"
        );
        assert!(!caps_warning(false, false));
        assert_eq!(CAPS_LOCK_WARNING, "Caps lock is on");
    }

    #[test]
    fn a_typed_username_is_trimmed_and_matched_to_the_list() {
        let ada = accounts::User {
            path: "/org/freedesktop/Accounts/User1001".into(),
            name: "ada".into(),
            real_name: "Ada Lovelace".into(),
            ..accounts::User::default()
        };
        let users = [ada.clone()];
        assert_eq!(
            typed_user("  ada \n", &users),
            Some(("ada".into(), Some(ada)))
        );
        assert_eq!(typed_user("dave", &users), Some(("dave".into(), None)));
        assert_eq!(typed_user("Ada", &users), Some(("Ada".into(), None)));
        assert_eq!(typed_user("   ", &users), None);
        assert_eq!(typed_user("", &users), None);
    }

    #[test]
    fn only_session_opened_starts_a_session_after_verification() {
        let service = conversation::PASSWORD_SERVICE.to_owned();
        let opened = Signal::SessionOpened {
            service: service.clone(),
            session_id: "c42".into(),
        };
        assert_eq!(session_to_start(&opened), Some("gdm-password"));
        for signal in [
            Signal::VerificationComplete {
                service: service.clone(),
            },
            Signal::ConversationStarted {
                service: service.clone(),
            },
            Signal::DefaultSessionNameChanged {
                session: "tuna".into(),
            },
            Signal::Reset,
        ] {
            assert_eq!(session_to_start(&signal), None, "{signal:?}");
        }
        // The conversation completes on VerificationComplete; SessionOpened
        // is no input of the prompt's machine.
        // gdm-password selected (Login Options' first), as the dialog does.
        let host = Conversations::new(auth_policy(3, true, true, false));
        host.set_user(Some("ada".into()));
        let begin = || {
            tuna_auth_prompt::Host::begin(&host, tuna_shell_control::AuthService::Password).unwrap()
        };
        begin();
        assert!(host.select(&host.default_mechanism().unwrap()));
        tuna_auth_prompt::Host::cancel(&host);
        let request = begin();
        assert_eq!(
            host.on_signal(&Signal::VerificationComplete { service }),
            vec![Input::Result {
                request,
                applied: true
            }]
        );
        assert_eq!(host.on_signal(&opened), Vec::new());
        let (interface, method, args) = Call::StartSessionWhenReady {
            service: "gdm-password".into(),
        }
        .wire();
        assert_eq!((interface, method), (gdm::GREETER, "StartSessionWhenReady"));
        assert_eq!(args, ("gdm-password", true).to_variant());
    }

    #[test]
    fn version_is_asked_anywhere_and_normalized() {
        let args = |list: &[&str]| list.iter().map(|a| (*a).to_owned()).collect::<Vec<_>>();
        let expected = format!("tuna-greeter {}", release_version());
        assert_eq!(
            version_line(args(&["tuna-greeter", "--version"]).into_iter()),
            Some(expected.clone())
        );
        assert_eq!(
            version_line(args(&["tuna-greeter", "x", "-V"]).into_iter()),
            Some(expected)
        );
        assert_eq!(version_line(args(&["tuna-greeter"]).into_iter()), None);
        assert_eq!(
            version_line(args(&["tuna-greeter", "--versions"]).into_iter()),
            None
        );
        assert_eq!(normalize_version(Some("v51.2.0"), "0.1.0"), "51.2.0");
        assert_eq!(normalize_version(Some("51.2.0"), "0.1.0"), "51.2.0");
        assert_eq!(normalize_version(Some(""), "0.1.0"), "0.1.0");
        assert_eq!(normalize_version(None, "0.1.0"), "0.1.0");
    }
}
