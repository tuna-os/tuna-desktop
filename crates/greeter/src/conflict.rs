//! A login whose user already has a session (gnome-shell 51
//! `loginDialog.js` `_onSessionOpened`, `_findConflictingSession`,
//! `_showConflictingSessionDialog`; `conflictingSessionDialog.js`).
//!
//! When GDM opens the session, logind is asked for another graphical,
//! running session of the same user. None: the session starts. One: the
//! "Session Already Running" dialog offers Cancel (back to the user list)
//! and Force Stop (`Greeter.StopConflictingSession`; once logind reports
//! the old session removed, the new one starts). Unanswered for 60 s,
//! the dialog closes with "Login Attempt Timed Out" and the prompt
//! resets.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use libadwaita as adw;

use crate::gdm::Call;
use crate::logind;
use crate::{trace, Greeter};

/// loginDialog.js `_CONFLICTING_SESSION_DIALOG_TIMEOUT`.
const DIALOG_TIMEOUT_SECS: u32 = 60;
/// `_notifyConflictingSessionDialogClosed`'s title and body.
const TIMED_OUT: &str = "Login Attempt Timed Out: login took too long, please try again";

/// The dialog's state: the login it holds back and its timer's run.
#[derive(Debug, Default)]
struct Pending {
    /// The open dialog: the GDM service to start and the conflicting
    /// session's id.
    open: Option<(String, String)>,
    /// Bumped on every open and close: a timer of an older run is stale
    /// (`closed` removes `closeDialogTimeoutId`).
    generation: u64,
}

impl Pending {
    /// The dialog opens; its timer's run.
    fn open(&mut self, service: String, session: String) -> u64 {
        self.generation += 1;
        self.open = Some((service, session));
        self.generation
    }

    fn close(&mut self) -> Option<(String, String)> {
        self.generation += 1;
        self.open.take()
    }

    /// The timer of `run` fired: whether the dialog closes for it ("Login
    /// Attempt Timed Out", the prompt resets).
    fn timed_out(&mut self, run: u64) -> bool {
        if self.generation != run || self.open.is_none() {
            return false;
        }
        self.close();
        true
    }

    /// logind removed session `id`: the service to start when it was the
    /// conflicting one.
    fn removed(&mut self, id: &str) -> Option<String> {
        if self.open.as_ref().is_none_or(|(_, session)| session != id) {
            return None;
        }
        self.close().map(|(service, _)| service)
    }
}

/// conflictingSessionDialog.js's text for the four local/remote cases.
pub fn banner_text(greeter_remote: bool, conflicting_remote: bool, user: &str) -> String {
    match (greeter_remote, conflicting_remote) {
        (true, true) => format!("Remote login is not possible because a remote session is already running for {user}. To login remotely, you must log out from the remote session or force stop it."),
        (false, true) => format!("Login is not possible because a remote session is already running for {user}. To login, you must log out from the remote session or force stop it."),
        (true, false) => format!("Remote login is not possible because a local session is already running for {user}. To login remotely, you must log out from the local session or force stop it."),
        (false, false) => format!("Login is not possible because a session is already running for {user}. To login, you must log out from the session or force stop it."),
    }
}

/// The dialog, over the login screen.
pub struct Conflict {
    pub toasts: adw::ToastOverlay,
    content: gtk::Widget,
    dialog: gtk::Box,
    description: gtk::Label,
    cancel: gtk::Button,
    force: gtk::Button,
    spinner: gtk::Spinner,
    pending: RefCell<Pending>,
    /// "Login Attempt Timed Out", up until a user is chosen
    /// (`_conflictingSessionNotification`).
    notice: RefCell<Option<adw::Toast>>,
}

impl Conflict {
    /// Wrap the login screen's `content`.
    pub fn new(content: &impl IsA<gtk::Widget>) -> Rc<Self> {
        let title = gtk::Label::new(Some("Session Already Running"));
        title.add_css_class("conflicting-session-dialog-title");
        title.add_css_class("title-2");
        let description = gtk::Label::new(None);
        description.add_css_class("conflicting-session-dialog-desc");
        description.set_wrap(true);
        description.set_justify(gtk::Justification::Center);
        let warning = gtk::Label::new(Some(
            "Force stopping will quit any running apps and processes, and could result in data loss",
        ));
        warning.add_css_class("conflicting-session-dialog-desc-warning");
        warning.set_wrap(true);
        warning.set_justify(gtk::Justification::Center);
        let cancel = gtk::Button::with_label("Cancel");
        cancel.add_css_class("suggested-action");
        let force = gtk::Button::with_label("Force Stop");
        force.add_css_class("destructive-action");
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        buttons.set_homogeneous(true);
        buttons.append(&cancel);
        buttons.append(&force);
        let spinner = gtk::Spinner::new();
        spinner.set_visible(false);
        let dialog = gtk::Box::new(gtk::Orientation::Vertical, 12);
        dialog.add_css_class("conflicting-session-dialog");
        dialog.add_css_class("modal-dialog");
        dialog.set_halign(gtk::Align::Center);
        dialog.set_valign(gtk::Align::Center);
        dialog.set_size_request(420, -1);
        dialog.append(&title);
        dialog.append(&description);
        dialog.append(&warning);
        dialog.append(&spinner);
        dialog.append(&buttons);
        dialog.set_visible(false);
        dialog.set_accessible_role(gtk::AccessibleRole::AlertDialog);
        dialog.update_property(&[gtk::accessible::Property::Label("Session Already Running")]);
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(content));
        overlay.add_overlay(&dialog);
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&overlay));
        Rc::new(Self {
            toasts,
            content: content.as_ref().clone().upcast(),
            dialog,
            description,
            cancel,
            force,
            spinner,
            pending: RefCell::default(),
            notice: RefCell::default(),
        })
    }

    /// loginDialog.js `_onUserListActivated`: the timed-out notice goes.
    pub fn dismiss_notice(&self) {
        if let Some(toast) = self.notice.borrow_mut().take() {
            toast.dismiss();
        }
    }

    pub fn is_open(&self) -> bool {
        self.pending.borrow().open.is_some()
    }

    fn hide(&self) {
        self.dialog.set_visible(false);
        self.content.set_sensitive(true);
        self.spinner.set_visible(false);
        self.spinner.set_spinning(false);
    }

    fn close(&self) -> Option<(String, String)> {
        self.hide();
        self.pending.borrow_mut().close()
    }
}

impl Greeter {
    /// Hook the dialog's buttons up.
    pub(crate) fn wire_conflict(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.conflict.cancel.connect_clicked(move |_| {
            if let Some(greeter) = weak.upgrade() {
                greeter.cancel_conflict();
            }
        });
        let weak = Rc::downgrade(self);
        self.conflict.force.connect_clicked(move |_| {
            let Some(greeter) = weak.upgrade() else {
                return;
            };
            trace("greeter conflict force-stop");
            let c = &greeter.conflict;
            c.cancel.set_sensitive(false);
            c.force.set_sensitive(false);
            c.spinner.set_visible(true);
            c.spinner.set_spinning(true);
            greeter.send(Call::StopConflictingSession);
        });
    }

    /// The dialog's Cancel (also Escape): back to the user list.
    pub(crate) fn cancel_conflict(self: &Rc<Self>) {
        if self.conflict.close().is_some() {
            trace("greeter conflict cancel");
            self.abandon_opened();
        }
    }

    /// authPrompt.js `reset()` after a session opened: the verifier's
    /// `reset()` cancels at GDM (which closes the opened session), then
    /// the user list.
    fn abandon_opened(self: &Rc<Self>) {
        self.send(Call::Cancel);
        self.show_users();
    }

    /// `_onSessionOpened`.
    pub(crate) fn session_opened(self: &Rc<Self>, service: String, session_id: String) {
        trace(&format!(
            "greeter session-opened service={service} session={session_id}"
        ));
        let system = self.system.borrow().clone();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let found = match (&system, session_id.is_empty()) {
                (Some(system), false) => logind::conflicting_session(system, &session_id).await,
                _ => Ok(None),
            };
            let Some(greeter) = weak.upgrade() else {
                return;
            };
            match found {
                Ok(None) => greeter.start_session(&service),
                Ok(Some(conflicting)) => {
                    let remote = match &system {
                        Some(system) => match logind::own_session_path(system).await {
                            Some(path) => {
                                logind::session(system, &path).await.is_ok_and(|s| s.remote)
                            }
                            None => false,
                        },
                        None => false,
                    };
                    greeter.show_conflict(service, conflicting, remote);
                }
                Err(error) => {
                    eprintln!(
                        "tuna-greeter: Failed to start session '{session_id}': {}",
                        error.message()
                    );
                    greeter.abandon_opened();
                }
            }
        });
    }

    /// `_startSession`.
    pub(crate) fn start_session(self: &Rc<Self>, service: &str) {
        self.send(Call::StartSessionWhenReady {
            service: service.to_owned(),
        });
    }

    fn show_conflict(self: &Rc<Self>, service: String, session: logind::Session, remote: bool) {
        trace(&format!(
            "greeter conflict user={} session={} remote={}",
            session.name, session.id, session.remote
        ));
        let c = &self.conflict;
        c.description
            .set_label(&banner_text(remote, session.remote, &session.name));
        c.cancel.set_sensitive(true);
        c.force.set_sensitive(true);
        c.content.set_sensitive(false);
        c.dialog.set_visible(true);
        c.cancel.grab_focus();
        let run = c.pending.borrow_mut().open(service, session.id);
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(
            Duration::from_secs(u64::from(DIALOG_TIMEOUT_SECS)),
            move || {
                let Some(greeter) = weak.upgrade() else {
                    return;
                };
                if !greeter.conflict.pending.borrow_mut().timed_out(run) {
                    return;
                }
                greeter.conflict.hide();
                trace("greeter conflict timed-out");
                let toast = adw::Toast::new(TIMED_OUT);
                // `Urgency.CRITICAL`: messageTray.js never times its
                // banner out; it stays until dismissed.
                toast.set_timeout(0);
                greeter.conflict.toasts.add_toast(toast.clone());
                *greeter.conflict.notice.borrow_mut() = Some(toast);
                greeter.abandon_opened();
            },
        );
    }

    /// logind removed a session: the conflicting one's end starts ours.
    pub(crate) fn session_removed(self: &Rc<Self>, id: &str) {
        let service = self.conflict.pending.borrow_mut().removed(id);
        if let Some(service) = service {
            self.conflict.hide();
            trace(&format!("greeter conflict stopped session={id}"));
            self.start_session(&service);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_banner_names_who_and_where() {
        assert!(banner_text(false, false, "ada")
            .starts_with("Login is not possible because a session is already running for ada."));
        assert!(banner_text(false, true, "ada").contains("a remote session is already running"));
        assert!(banner_text(true, false, "ada")
            .starts_with("Remote login is not possible because a local session"));
        assert!(banner_text(true, true, "ada").contains("log out from the remote session"));
    }

    #[test]
    fn an_unanswered_dialog_times_out_after_a_minute() {
        assert_eq!(DIALOG_TIMEOUT_SECS, 60, "loginDialog.js");
        assert!(TIMED_OUT.starts_with("Login Attempt Timed Out"));
        let mut pending = Pending::default();
        let run = pending.open("gdm-password".into(), "9".into());
        assert!(pending.timed_out(run));
        assert!(pending.open.is_none(), "the dialog closed");
        assert!(!pending.timed_out(run), "once");
    }

    #[test]
    fn a_closed_dialogs_timer_does_nothing() {
        let mut pending = Pending::default();
        let run = pending.open("gdm-password".into(), "9".into());
        assert_eq!(
            pending.close(),
            Some(("gdm-password".into(), "9".into())),
            "Cancel"
        );
        assert!(!pending.timed_out(run));
        // A later dialog is not closed by the first one's timer.
        let first = pending.open("gdm-password".into(), "9".into());
        pending.close();
        let second = pending.open("gdm-password".into(), "9".into());
        assert!(!pending.timed_out(first));
        assert!(pending.open.is_some());
        assert!(pending.timed_out(second));
    }

    #[test]
    fn the_conflicting_sessions_end_starts_the_login_and_stops_the_timer() {
        let mut pending = Pending::default();
        assert_eq!(pending.removed("9"), None, "no dialog");
        let run = pending.open("gdm-password".into(), "9".into());
        assert_eq!(pending.removed("c42"), None, "another session");
        assert_eq!(pending.removed("9"), Some("gdm-password".into()));
        assert!(pending.open.is_none());
        assert!(!pending.timed_out(run));
        assert_eq!(pending.removed("9"), None, "started once");
    }
}
