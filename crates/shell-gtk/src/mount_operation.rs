//! The shell's mount operation: GNOME Shell 51 `js/ui/shellMountOperation.js`
//! `ShellMountOperation` with its `ShellMountPasswordDialog` (LUKS and
//! VeraCrypt passphrases, PIM, hidden/system volume, keyfiles, remember
//! password) and `ShellMountQuestionDialog`. The automount component
//! mounts hotplugged volumes with it ([`crate::removable_media`]).
//!
//! `GMountOperation`'s own class handlers answer every request
//! `G_MOUNT_OPERATION_UNHANDLED` from an idle, so each handler here stops
//! the emission first, as `Shell.MountOperation` replaces them in C.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk4::prelude::*;
use gtk4::{self as gtk, gio, glib};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

/// `REMEMBER_MOUNT_PASSWORD_KEY` in `org.gnome.shell`.
const REMEMBER_KEY: &str = "remember-mount-password";
/// GNOME's Disks, offered for keyfile volumes.
const DISKS_APP: &str = "org.gnome.DiskUtility.desktop";
const RETRY: &str = "Sorry, that didn’t work. Please try again.";
const BAD_PIM: &str = "The PIM must be a number or empty";

/// `_setLabelsForMessage` (question): the first line is the title, the
/// rest the description.
pub fn question_labels(message: &str) -> (String, String) {
    let mut lines = message.split('\n');
    let title = lines.next().unwrap_or_default().to_owned();
    (title, lines.collect::<Vec<_>>().join("\n"))
}

/// `ShellMountPasswordDialog._init`: line one the title, line two the
/// description, each absent when empty.
pub fn password_labels(message: &str) -> (Option<String>, Option<String>) {
    let mut lines = message.split('\n');
    let mut next = || lines.next().filter(|l| !l.is_empty()).map(str::to_owned);
    let title = next();
    (title, next())
}

/// `_setButtonsForChoices`: GNOME unshifts each choice, so the buttons
/// run last choice first; each answers its own index.
pub fn choice_buttons(choices: &[String]) -> Vec<(i32, String)> {
    choices
        .iter()
        .enumerate()
        .rev()
        .map(|(i, label)| (i as i32, label.clone()))
        .collect()
}

/// `_onEntryActivate`'s PIM: empty is 0, otherwise a decimal number
/// (VeraCrypt's PIM is a non-negative decimal integer); anything else
/// is refused with "The PIM must be a number or empty".
pub fn parse_pim(text: &str) -> Option<u32> {
    let text = text.trim();
    if text.is_empty() {
        return Some(0);
    }
    if !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// The password dialog's answer (`response` signal).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PasswordResponse {
    Cancel,
    Unlock {
        password: String,
        remember: bool,
        hidden_volume: bool,
        system_volume: bool,
        pim: u32,
    },
}

/// A layer-shell modal like the polkit dialog's.
fn modal_window(app: &gtk::Application, title: &str) -> gtk::Window {
    let window = gtk::Window::new();
    window.set_application(Some(app));
    window.add_css_class("tuna-end-session");
    window.add_css_class("tuna-polkit");
    window.add_css_class("tuna-mount-operation");
    window.set_title(Some(title));
    window.init_layer_shell();
    window.set_layer(Layer::Overlay);
    window.set_namespace(Some(tuna_shell_control::MOUNT_OPERATION_NAMESPACE));
    for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
        window.set_anchor(edge, true);
    }
    window.set_exclusive_zone(-1);
    window.set_keyboard_mode(KeyboardMode::Exclusive);
    window
}

fn card() -> (gtk::Box, gtk::Box) {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 18);
    card.add_css_class("modal-dialog");
    card.add_css_class("prompt-dialog");
    card.set_halign(gtk::Align::Center);
    card.set_valign(gtk::Align::Center);
    card.set_size_request(411, -1);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 18);
    content.add_css_class("modal-dialog-content-box");
    card.append(&content);
    (card, content)
}

/// `Dialog.MessageDialogContent`: a title and a wrapped description.
fn message_content(content: &gtk::Box, title: Option<&str>, description: Option<&str>) {
    let header = gtk::Box::new(gtk::Orientation::Vertical, 18);
    for (text, class) in [
        (title, "message-dialog-title"),
        (description, "message-dialog-description"),
    ] {
        let Some(text) = text.filter(|t| !t.is_empty()) else {
            continue;
        };
        let label = gtk::Label::new(Some(text));
        label.add_css_class(class);
        label.set_wrap(true);
        label.set_max_width_chars(1);
        label.set_hexpand(true);
        label.set_justify(gtk::Justification::Center);
        header.append(&label);
    }
    content.append(&header);
}

fn button_box() -> gtk::Box {
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    buttons.add_css_class("modal-dialog-button-box");
    buttons.set_homogeneous(true);
    buttons
}

fn dialog_button(label: &str) -> gtk::Button {
    let button = gtk::Button::with_label(label);
    button.add_css_class("modal-dialog-button");
    button
}

/// The window fades out, then goes.
fn close_window(window: &gtk::Window, fade: &crate::transient::ModalFade) {
    if !window.is_visible() {
        window.destroy();
        return;
    }
    window.connect_visible_notify(|w| {
        if !w.is_visible() {
            w.destroy();
        }
    });
    fade.hide();
}

/// `org.gnome.shell` when installed with the remember key.
fn shell_settings() -> Option<gio::Settings> {
    let source = gio::SettingsSchemaSource::default()?;
    let schema = source.lookup("org.gnome.shell", true)?;
    schema
        .has_key(REMEMBER_KEY)
        .then(|| gio::Settings::new("org.gnome.shell"))
}

/// Where the password dialog's answer goes (its `response` signal).
type OnPassword = Rc<dyn Fn(PasswordResponse)>;

/// `ShellMountPasswordDialog`.
pub struct PasswordDialog {
    window: gtk::Window,
    fade: crate::transient::ModalFade,
    password: gtk::PasswordEntry,
    wiggle: crate::lock_wiggle::Wiggle,
    pim: Option<gtk::PasswordEntry>,
    hidden_volume: Option<gtk::CheckButton>,
    system_volume: Option<gtk::CheckButton>,
    remember: Option<gtk::CheckButton>,
    spinner: gtk::Spinner,
    error: gtk::Label,
    response: RefCell<Option<OnPassword>>,
}

impl PasswordDialog {
    pub fn new(app: &gtk::Application, message: &str, flags: gio::AskPasswordFlags) -> Rc<Self> {
        let (title, description) = password_labels(message);
        let window = modal_window(app, title.as_deref().unwrap_or("Unlock"));
        // `lookup_app` (gio 0.22 has no `DesktopAppInfo`).
        let disks = gio::AppInfo::all()
            .into_iter()
            .find(|app| app.id().as_deref() == Some(DISKS_APP));
        let (card, content) = card();
        message_content(&content, title.as_deref(), description.as_deref());

        let tcrypt = flags.contains(gio::AskPasswordFlags::TCRYPT);
        let check = |label: &str| {
            let b = gtk::CheckButton::with_label(label);
            content.append(&b);
            b
        };
        let (hidden_volume, system_volume, keyfiles) = if tcrypt {
            (
                Some(check("Hidden Volume")),
                Some(check("Windows System Volume")),
                Some(check("Uses Keyfiles")),
            )
        } else {
            (None, None, None)
        };
        let keyfiles_label = gtk::Label::new(None);
        keyfiles_label.set_wrap(true);
        keyfiles_label.set_visible(false);
        match &disks {
            Some(disks) => keyfiles_label.set_markup(&format!(
                "To unlock a volume that uses keyfiles, use the <i>{}</i> utility instead",
                glib::markup_escape_text(&disks.name())
            )),
            None => keyfiles_label.set_markup(
                "You need an external utility like <i>Disks</i> to unlock a volume that uses keyfiles",
            ),
        }
        if tcrypt {
            content.append(&keyfiles_label);
        }
        let entry = |hint: &str| {
            let e = gtk::PasswordEntry::new();
            e.add_css_class("prompt-dialog-password-entry");
            e.set_hexpand(true);
            e.set_property("placeholder-text", hint);
            e.update_property(&[gtk::accessible::Property::Label(hint)]);
            crate::lockdown::bind_password_entry(&e);
            e
        };
        let grid = gtk::Grid::new();
        grid.add_css_class("prompt-dialog-password-grid");
        grid.set_row_spacing(6);
        grid.set_column_spacing(6);
        let mut row = 0;
        let pim = tcrypt.then(|| {
            let pim = entry("PIM Number");
            grid.attach(&pim, 0, row, 1, 1);
            row += 1;
            pim
        });
        let password = entry("Password");
        let wiggle = crate::lock_wiggle::Wiggle::new(&password);
        wiggle.set_hexpand(true);
        let spinner = gtk::Spinner::new();
        spinner.set_size_request(16, 16);
        grid.attach(&wiggle, 0, row, 1, 1);
        grid.attach(&spinner, 1, row, 1, 1);
        row += 1;
        let error = gtk::Label::new(None);
        error.add_css_class("prompt-dialog-error-label");
        error.set_wrap(true);
        error.set_opacity(0.0);
        grid.attach(&error, 0, row, 2, 1);
        content.append(&grid);
        let remember = flags
            .contains(gio::AskPasswordFlags::SAVING_SUPPORTED)
            .then(|| {
                let b = check("Remember Password");
                b.set_active(shell_settings().is_some_and(|s| s.boolean(REMEMBER_KEY)));
                b
            });

        let buttons = button_box();
        let cancel = dialog_button("Cancel");
        let unlock = dialog_button("Unlock");
        let open_disks = disks
            .as_ref()
            .map(|d| dialog_button(&format!("Open {}", d.name())));
        buttons.append(&cancel);
        buttons.append(&unlock);
        if let Some(b) = &open_disks {
            b.set_visible(false);
            buttons.append(b);
        }
        card.append(&buttons);
        window.set_child(Some(&card));

        let dialog = Rc::new(Self {
            fade: crate::transient::ModalFade::new(&window),
            window,
            password,
            wiggle,
            pim,
            hidden_volume,
            system_volume,
            remember,
            spinner,
            error,
            response: RefCell::new(None),
        });
        let weak = Rc::downgrade(&dialog);
        cancel.connect_clicked(move |_| {
            if let Some(d) = weak.upgrade() {
                d.respond(PasswordResponse::Cancel);
            }
        });
        let weak = Rc::downgrade(&dialog);
        unlock.connect_clicked(move |_| {
            if let Some(d) = weak.upgrade() {
                d.activate();
            }
        });
        for e in std::iter::once(&dialog.password).chain(dialog.pim.as_ref()) {
            let weak = Rc::downgrade(&dialog);
            e.connect_activate(move |_| {
                if let Some(d) = weak.upgrade() {
                    d.activate();
                }
            });
        }
        if let Some(b) = &open_disks {
            let weak = Rc::downgrade(&dialog);
            let disks = disks.clone();
            b.connect_clicked(move |_| {
                if let Some(disks) = &disks {
                    let context = tuna_shell_host::apps::application_launch_context();
                    let _ = disks.launch(&[], Some(&context));
                }
                if let Some(d) = weak.upgrade() {
                    d.respond(PasswordResponse::Cancel);
                }
            });
        }
        // `_onKeyfilesCheckboxClicked`: keyfile volumes go to Disks.
        if let Some(keyfiles) = keyfiles {
            let weak = Rc::downgrade(&dialog);
            keyfiles.connect_toggled(move |k| {
                let Some(d) = weak.upgrade() else { return };
                let on = k.is_active();
                d.password.set_sensitive(!on);
                if let Some(p) = &d.pim {
                    p.set_sensitive(!on);
                }
                if let Some(r) = &d.remember {
                    r.set_sensitive(!on);
                }
                keyfiles_label.set_visible(on);
                unlock.set_visible(!on);
                if let Some(b) = &open_disks {
                    b.set_visible(on);
                }
            });
        }
        let keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(&dialog);
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gtk::gdk::Key::Escape {
                if let Some(d) = weak.upgrade() {
                    d.respond(PasswordResponse::Cancel);
                }
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        dialog.window.add_controller(keys);
        dialog
    }

    fn set_response(&self, response: Option<OnPassword>) {
        *self.response.borrow_mut() = response;
    }

    fn respond(&self, answer: PasswordResponse) {
        let response = self.response.borrow().clone();
        if let Some(response) = response {
            response(answer);
        }
    }

    pub fn open(&self) {
        self.fade.present();
        self.password.grab_focus();
    }

    /// `reaskPassword`.
    pub fn reask(&self) {
        self.spinner.stop();
        self.password.set_text("");
        self.error.set_label(RETRY);
        self.error.set_opacity(1.0);
        self.wiggle.start();
        self.password.grab_focus();
    }

    /// `_onEntryActivate`.
    fn activate(&self) {
        let mut pim = 0;
        if let Some(entry) = &self.pim {
            match parse_pim(&entry.text()) {
                Some(value) => pim = value,
                None => {
                    entry.set_text("");
                    self.error.set_label(BAD_PIM);
                    self.error.set_opacity(1.0);
                    return;
                }
            }
            self.error.set_opacity(0.0);
        }
        let remember = self.remember.as_ref().is_some_and(|r| r.is_active());
        if let Some(settings) = shell_settings() {
            let _ = settings.set_boolean(REMEMBER_KEY, remember);
        }
        self.spinner.start();
        self.respond(PasswordResponse::Unlock {
            password: self.password.text().to_string(),
            remember,
            hidden_volume: self.hidden_volume.as_ref().is_some_and(|c| c.is_active()),
            system_volume: self.system_volume.as_ref().is_some_and(|c| c.is_active()),
            pim,
        });
    }

    pub fn close(&self) {
        self.set_response(None);
        close_window(&self.window, &self.fade);
    }
}

/// `ShellMountQuestionDialog`.
struct QuestionDialog {
    window: gtk::Window,
    fade: crate::transient::ModalFade,
}

impl QuestionDialog {
    fn open(
        app: &gtk::Application,
        message: &str,
        choices: &[String],
        respond: Rc<dyn Fn(i32)>,
    ) -> Self {
        let (title, description) = question_labels(message);
        let window = modal_window(app, &title);
        let (card, content) = card();
        message_content(&content, Some(&title), Some(&description));
        let buttons = button_box();
        for (choice, label) in choice_buttons(choices) {
            let button = dialog_button(&label);
            let respond = respond.clone();
            button.connect_clicked(move |_| respond(choice));
            buttons.append(&button);
        }
        card.append(&buttons);
        window.set_child(Some(&card));
        let fade = crate::transient::ModalFade::new(&window);
        fade.present();
        Self { window, fade }
    }
}

enum Open {
    Password(Rc<PasswordDialog>),
    Question(QuestionDialog),
}

impl Open {
    fn close(&self) {
        match self {
            Open::Password(d) => d.close(),
            Open::Question(d) => close_window(&d.window, &d.fade),
        }
    }
}

/// `ShellMountOperation`.
pub struct ShellMountOperation {
    op: gio::MountOperation,
    dialog: RefCell<Option<Open>>,
    existing: RefCell<Option<Rc<PasswordDialog>>>,
    drive: RefCell<Option<(gio::Drive, glib::SignalHandlerId)>>,
    closed: Cell<bool>,
}

impl ShellMountOperation {
    pub fn new(
        app: &gtk::Application,
        volume: &gio::Volume,
        existing: Option<Rc<PasswordDialog>>,
    ) -> Rc<Self> {
        let me = Rc::new(Self {
            op: gio::MountOperation::new(),
            dialog: RefCell::new(None),
            existing: RefCell::new(existing),
            drive: RefCell::new(None),
            closed: Cell::new(false),
        });
        let weak = Rc::downgrade(&me);
        let app_for_password = app.clone();
        me.op
            .connect_ask_password(move |op, message, _user, _domain, flags| {
                op.stop_signal_emission_by_name("ask-password");
                if let Some(me) = weak.upgrade() {
                    me.ask_password(&app_for_password, message, flags);
                }
            });
        let weak = Rc::downgrade(&me);
        let app_for_question = app.clone();
        me.op.connect_local("ask-question", false, move |args| {
            let op = args[0].get::<gio::MountOperation>().ok()?;
            op.stop_signal_emission_by_name("ask-question");
            let message = args[1].get::<String>().unwrap_or_default();
            let choices: Vec<String> = args[2].get::<Vec<String>>().unwrap_or_default();
            if let Some(me) = weak.upgrade() {
                me.ask_question(&app_for_question, &message, &choices);
            }
            None
        });
        let weak = Rc::downgrade(&me);
        me.op.connect_aborted(move |_| {
            if let Some(me) = weak.upgrade() {
                me.close();
            }
        });
        if let Some(drive) = volume.drive() {
            let weak = Rc::downgrade(&me);
            let id = drive.connect_disconnected(move |_| {
                if let Some(me) = weak.upgrade() {
                    me.close();
                }
            });
            *me.drive.borrow_mut() = Some((drive, id));
        }
        me
    }

    pub fn mount_op(&self) -> &gio::MountOperation {
        &self.op
    }

    fn close_existing(&self) {
        if let Some(existing) = self.existing.borrow_mut().take() {
            existing.close();
        }
    }

    /// `_onAskQuestion`.
    fn ask_question(self: &Rc<Self>, app: &gtk::Application, message: &str, choices: &[String]) {
        self.close_existing();
        let weak = Rc::downgrade(self);
        let dialog = QuestionDialog::open(
            app,
            message,
            choices,
            Rc::new(move |choice| {
                if let Some(me) = weak.upgrade() {
                    me.op.set_choice(choice);
                    me.op.reply(gio::MountOperationResult::Handled);
                    me.close();
                }
            }),
        );
        if let Some(old) = self.dialog.replace(Some(Open::Question(dialog))) {
            old.close();
        }
    }

    /// `_onAskPassword`: a re-ask keeps the dialog the failed attempt used.
    fn ask_password(
        self: &Rc<Self>,
        app: &gtk::Application,
        message: &str,
        flags: gio::AskPasswordFlags,
    ) {
        let existing = self.existing.borrow_mut().take();
        let dialog = match existing {
            Some(dialog) => {
                dialog.reask();
                dialog
            }
            None => PasswordDialog::new(app, message, flags),
        };
        let op = self.op.clone();
        dialog.set_response(Some(Rc::new(move |answer| match answer {
            PasswordResponse::Cancel => op.reply(gio::MountOperationResult::Aborted),
            PasswordResponse::Unlock {
                password,
                remember,
                hidden_volume,
                system_volume,
                pim,
            } => {
                op.set_password_save(if remember {
                    gio::PasswordSave::Permanently
                } else {
                    gio::PasswordSave::Never
                });
                op.set_password(Some(&password));
                op.set_is_tcrypt_hidden_volume(hidden_volume);
                op.set_is_tcrypt_system_volume(system_volume);
                op.set_pim(pim);
                op.reply(gio::MountOperationResult::Handled);
            }
        })));
        dialog.open();
        *self.dialog.borrow_mut() = Some(Open::Password(dialog));
    }

    /// `borrowDialog`: the password dialog, detached from this operation.
    pub fn borrow_dialog(&self) -> Option<Rc<PasswordDialog>> {
        match self.dialog.borrow_mut().take() {
            Some(Open::Password(dialog)) => {
                dialog.set_response(None);
                Some(dialog)
            }
            Some(other) => {
                *self.dialog.borrow_mut() = Some(other);
                None
            }
            None => None,
        }
    }

    /// `close`.
    pub fn close(&self) {
        if self.closed.replace(true) {
            return;
        }
        self.close_existing();
        if let Some(dialog) = self.dialog.borrow_mut().take() {
            dialog.close();
        }
        if let Some((drive, id)) = self.drive.borrow_mut().take() {
            drive.disconnect(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mount_prompts_split_messages_like_gnome() {
        assert_eq!(
            password_labels("Authentication Required\nEnter a passphrase to unlock “STICK”"),
            (
                Some("Authentication Required".to_owned()),
                Some("Enter a passphrase to unlock “STICK”".to_owned())
            )
        );
        // Only two lines are used; empty ones are absent.
        assert_eq!(
            password_labels("\nsecond\nthird"),
            (None, Some("second".to_owned()))
        );
        assert_eq!(
            question_labels("Title\nline one\nline two"),
            ("Title".to_owned(), "line one\nline two".to_owned())
        );
    }

    #[test]
    fn question_buttons_run_last_choice_first() {
        let choices = vec!["Cancel".to_owned(), "Unmount Anyway".to_owned()];
        assert_eq!(
            choice_buttons(&choices),
            vec![(1, "Unmount Anyway".to_owned()), (0, "Cancel".to_owned())]
        );
    }

    #[test]
    fn pim_is_empty_or_a_decimal_number() {
        assert_eq!(parse_pim(""), Some(0));
        assert_eq!(parse_pim(" 485 "), Some(485));
        assert_eq!(parse_pim("12ab"), None);
        assert_eq!(parse_pim("-5"), None);
        assert_eq!(parse_pim("99999999999"), None);
    }
}
