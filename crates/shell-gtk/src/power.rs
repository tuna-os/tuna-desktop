//! Power menu and end-session dialog (GNOME 51 shape, #62, #589).
//!
//! The quick-settings power button opens an in-panel menu: Suspend,
//! Restart…, Power Off…, Log Out. As GNOME Shell 51's `systemActions.js`,
//! each row asks the session manager on the session bus
//! (`org.gnome.SessionManager`: the compositor's `session_manager`, or
//! gnome-session where it runs): `Suspend()`, `Reboot()`, `Shutdown()`
//! and `Logout(0)`. The manager suspends at once; for the rest it checks
//! `disable-log-out`, `logout-prompt` and its logout inhibitors and opens
//! the dialog below, and it carries the confirmed end of the session out
//! itself (logind's `PowerOff`/`Reboot`, or the session leader's exit).
//!
//! The dialog is GNOME Shell 51's `org.gnome.SessionManager.EndSessionDialog`
//! on the shell's bus connection (endSessionDialog.js): a dimmed
//! full-screen layer with the 60-second countdown (shown in steps of ten
//! until the last ten seconds), the applications whose logout inhibitors
//! the manager passed ("Some applications are busy or have unsaved
//! work"), Cancel, and the action. The manager opens it for the menu and
//! for gnome-settings-daemon's power, logout, reboot and shutdown keys.
//! As in GNOME, Cancel emits `Canceled` then `Closed`, a confirmation or
//! the countdown's end emits `ConfirmedShutdown`, `ConfirmedReboot` or
//! `ConfirmedLogout`, and `Close` emits `Closed`; the shell itself never
//! ends the session.
//!
//! In the nested preview (`TUNA_SESSION_KIND=nested`) logind's session
//! is the host's: only Log Out is offered. It goes through the session
//! manager when that is this compositor's (the compositor's manager ends
//! only itself there); inside a host desktop whose session manager owns
//! the name, the dialog opens locally and Log Out ends this compositor,
//! never the host session.
//!
//! Before sleep, logind's `PrepareForSleep(true)` locks the screen, so
//! the session resumes locked from any suspend (menu, lid, idle).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gio::prelude::*;
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

const LOGIND: &str = "org.freedesktop.login1";
const MANAGER_PATH: &str = "/org/freedesktop/login1";
const MANAGER_IFACE: &str = "org.freedesktop.login1.Manager";
/// GNOME's end-session countdown.
pub const COUNTDOWN_S: u32 = 60;

/// The session manager the menu asks (systemActions.js `_session`).
const SESSION_MANAGER: &str = "org.gnome.SessionManager";
const SESSION_MANAGER_PATH: &str = "/org/gnome/SessionManager";
const INHIBITOR_IFACE: &str = "org.gnome.SessionManager.Inhibitor";
/// `GnomeSession.InhibitFlags.LOGOUT`.
const INHIBIT_LOGOUT: u32 = 1;
/// GNOME's application section title (endSessionDialog.js).
pub const BUSY_APPLICATIONS: &str = "Some applications are busy or have unsaved work";

/// The seconds the countdown shows with `left` of `total` remaining:
/// endSessionDialog.js `_roundSecondsToInterval(total, left, 10)`, which
/// steps by ten (rounding up) until the last ten seconds.
pub fn display_seconds(total: u32, left: u32) -> u32 {
    const INTERVAL: u32 = 10;
    if left <= INTERVAL {
        return left;
    }
    let time = left + INTERVAL - 1;
    if time > total {
        total
    } else {
        time - time % INTERVAL
    }
}

/// GNOME Shell's end-session dialog object on the shell's connection.
pub const END_SESSION_DIALOG_PATH: &str = "/org/gnome/SessionManager/EndSessionDialog";
const END_SESSION_DIALOG_IFACE: &str = "org.gnome.SessionManager.EndSessionDialog";
const END_SESSION_DIALOG_XML: &str = r#"<node>
  <interface name="org.gnome.SessionManager.EndSessionDialog">
    <method name="Open">
      <arg type="u" direction="in"/>
      <arg type="u" direction="in"/>
      <arg type="u" direction="in"/>
      <arg type="ao" direction="in"/>
    </method>
    <method name="Close"/>
    <signal name="ConfirmedLogout"/>
    <signal name="ConfirmedReboot"/>
    <signal name="ConfirmedShutdown"/>
    <signal name="Canceled"/>
    <signal name="Closed"/>
  </interface>
</node>"#;

/// The action an `Open` dialog type asks for: gnome-session's
/// `GsmShellEndSessionDialogType` (0 log out, 1 shutdown, 2 restart).
/// GNOME Shell's type 3 is its own offline-update restart, which it
/// derives from a restart when gnome-software has an update scheduled;
/// Tuna has no offline updates, so it is an unknown type here.
pub fn dialog_action(kind: u32) -> Option<Action> {
    match kind {
        0 => Some(Action::LogOut),
        1 => Some(Action::PowerOff),
        2 => Some(Action::Restart),
        _ => None,
    }
}

/// The signal a confirmed `action` emits (endSessionDialog.js `DialogContent`).
pub fn confirmed_signal(action: Action) -> Option<&'static str> {
    match action {
        Action::LogOut => Some("ConfirmedLogout"),
        Action::PowerOff => Some("ConfirmedShutdown"),
        Action::Restart => Some("ConfirmedReboot"),
        Action::Suspend => None,
    }
}

/// One end-session action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Suspend,
    Restart,
    PowerOff,
    LogOut,
}

impl Action {
    /// Dialog title and confirm button label.
    pub fn title(self) -> &'static str {
        match self {
            Action::Suspend => "Suspend",
            Action::Restart => "Restart",
            Action::PowerOff => "Power Off",
            Action::LogOut => "Log Out",
        }
    }

    /// GNOME's countdown sentence for `seconds` left.
    pub fn countdown_text(self, seconds: u32) -> String {
        let unit = if seconds == 1 { "second" } else { "seconds" };
        match self {
            Action::Restart => {
                format!("The system will restart automatically in {seconds} {unit}")
            }
            Action::PowerOff => {
                format!("The system will power off automatically in {seconds} {unit}")
            }
            // GNOME 51's strings, without a closing period.
            Action::LogOut => format!("You will be logged out automatically in {seconds} {unit}"),
            Action::Suspend => String::new(),
        }
    }
}

/// Which menu rows a session offers.
pub fn menu_actions(nested: bool) -> &'static [Action] {
    if nested {
        &[Action::LogOut]
    } else {
        &[
            Action::Suspend,
            Action::Restart,
            Action::PowerOff,
            Action::LogOut,
        ]
    }
}

/// Whether the power menu itself shows under a lockdown policy: any
/// visible row keeps it (GNOME 51 `status/system.js` ShutdownItem
/// `_sync`), so a nested session (whose only row is Log Out) shows no
/// menu while `disable-log-out` denies, and hardware keeps Suspend.
pub fn power_menu_visible_for(actions: &[Action], logout_allowed: bool) -> bool {
    actions
        .iter()
        .any(|action| action_allowed(*action, logout_allowed))
}

/// Whether `action` may run while `logout_allowed` reflects the live
/// `disable-log-out` key. GNOME 51 gates every session-ending action
/// on it: gnome-session 51.0 `gsm-manager.c` answers `CanShutdown` and
/// `CanReboot` `UNAVAILABLE` and refuses `Shutdown`, `Reboot` and
/// `Logout` ("Logout has been locked down") while it is set, and
/// gnome-shell's `systemActions.js` re-queries both on the key's
/// change, hiding Restart… and Power Off… with Log Out…. Suspend
/// (`CanSuspend`) ignores the key and always runs.
pub fn action_allowed(action: Action, logout_allowed: bool) -> bool {
    action == Action::Suspend || logout_allowed
}

/// Whether a pending end-session dialog must die under the live policy:
/// a Log Out, Restart or Power Off countdown open when `disable-log-out`
/// engages (gnome-session refuses to leave the Running phase then).
/// An idle dialog never cancels.
pub fn should_cancel_pending(pending: Option<Action>, logout_allowed: bool) -> bool {
    matches!(pending, Some(action) if !action_allowed(action, logout_allowed))
}

/// Whether this shell runs inside another desktop.
pub fn is_nested() -> bool {
    std::env::var("TUNA_SESSION_KIND").map_or(true, |kind| kind != "hardware")
}

/// The session manager method a menu row calls and its arguments
/// (systemActions.js `activateSuspend`, `activateRestart`,
/// `activatePowerOff`, `activateLogout`).
pub fn manager_call(action: Action) -> (&'static str, Option<glib::Variant>) {
    match action {
        Action::Suspend => ("Suspend", None),
        Action::Restart => ("Reboot", None),
        Action::PowerOff => ("Shutdown", None),
        Action::LogOut => ("Logout", Some((0u32,).to_variant())),
    }
}

/// Ask the session manager for `action`. `Shutdown` and `Reboot` answer
/// once the dialog is confirmed or cancelled, so the call never times
/// out; a cancelled request is quiet (`logErrorUnlessCancelled`).
fn call_manager(bus: &gio::DBusConnection, action: Action) {
    let (method, args) = manager_call(action);
    eprintln!("tuna-shell-gtk: power menu: {method} via {SESSION_MANAGER}");
    bus.call(
        Some(SESSION_MANAGER),
        SESSION_MANAGER_PATH,
        SESSION_MANAGER,
        method,
        args.as_ref(),
        None,
        gio::DBusCallFlags::NONE,
        i32::MAX,
        None::<&gio::Cancellable>,
        move |reply| match reply {
            Ok(_) => {}
            Err(e) if e.matches(gio::IOErrorEnum::Cancelled) => {
                eprintln!("tuna-shell-gtk: power menu: {method} cancelled");
            }
            Err(e) => eprintln!("tuna-shell-gtk: power menu: {method} failed: {e}"),
        },
    );
}

/// Whether the session manager on `bus` is this shell's compositor
/// (the parent process), whose manager ends only itself in a nested
/// preview; a host desktop's would end the host session.
async fn manager_is_compositors(bus: &gio::DBusConnection) -> bool {
    let call = |method: &'static str, arg: String| {
        bus.call_future(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            method,
            Some(&(arg,).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            5000,
        )
    };
    let Ok(owner) = call("GetNameOwner", SESSION_MANAGER.to_owned()).await else {
        return false;
    };
    let Some(owner) = owner.child_value(0).get::<String>() else {
        return false;
    };
    let Ok(pid) = call("GetConnectionUnixProcessID", owner).await else {
        return false;
    };
    pid.child_value(0).get::<u32>() == Some(unsafe { libc::getppid() } as u32)
}

/// End the supervising compositor, this process's parent, which shuts
/// down cleanly on SIGTERM. An adoptive parent is never signalled.
fn end_compositor() {
    let parent = unsafe { libc::getppid() };
    let comm = std::fs::read_to_string(format!("/proc/{parent}/comm")).unwrap_or_default();
    if parent > 1 && is_compositor_comm(&comm) {
        unsafe {
            libc::kill(parent, libc::SIGTERM);
        }
    } else {
        eprintln!("tuna-shell-gtk: parent {parent} is not the compositor; Log Out not sent");
    }
}

/// Whether a `/proc/<pid>/comm` value names the compositor. The kernel
/// keeps the first 15 bytes of the executable name, which is all of
/// `tuna-compositor`.
fn is_compositor_comm(comm: &str) -> bool {
    comm.trim_end() == "tuna-compositor"
}

/// The nested preview's own Log Out, when the session manager on its bus
/// is a host desktop's: end this compositor, never the host session.
/// Nothing else is offered there.
fn perform_local(action: Action) {
    if action == Action::LogOut {
        end_compositor();
    }
}

/// One inhibitor the manager passed `Open`, as endSessionDialog.js
/// `_onInhibitorLoaded` lists it: an installed application (its app id
/// names a desktop file) holding a logout inhibitor, with its reason.
/// Services and other flags are left out.
async fn inhibiting_app(
    bus: &gio::DBusConnection,
    path: &str,
) -> Option<(tuna_shell_host::apps::AppEntry, String)> {
    let get = |method: &'static str| {
        bus.call_future(
            Some(SESSION_MANAGER),
            path,
            INHIBITOR_IFACE,
            method,
            None,
            None,
            gio::DBusCallFlags::NONE,
            5000,
        )
    };
    let app_id = get("GetAppId").await.ok()?.child_value(0).get::<String>()?;
    let app = crate::notify::desktop_app(app_id.trim_end_matches(".desktop"))?;
    let flags = get("GetFlags").await.ok()?.child_value(0).get::<u32>()?;
    if flags & INHIBIT_LOGOUT == 0 {
        return None;
    }
    let reason = get("GetReason")
        .await
        .ok()?
        .child_value(0)
        .get::<String>()?;
    Some((app, reason))
}

/// The power surfaces: the menu rows for quick settings, the dialog.
pub struct PowerUi {
    nested: bool,
    dialog: gtk::ApplicationWindow,
    /// The modal fade in and out (`ModalDialog`, 100 ms).
    fade: crate::transient::ModalFade,
    title: gtk::Label,
    body: gtk::Label,
    confirm: gtk::Button,
    /// "Some applications are busy or have unsaved work" and its list.
    applications: gtk::Box,
    application_list: gtk::Box,
    /// Inhibitors still listed or loading (`_applications.length`).
    inhibitors: Cell<usize>,
    pending: Cell<Option<Action>>,
    /// The open dialog is the nested preview's own (no manager waits on
    /// its answer; it acts itself).
    local: Cell<bool>,
    /// Counts dialogs opened, so a stale inhibitor load is dropped.
    serial: Cell<u64>,
    total: Cell<u32>,
    left: Cell<u32>,
    timer: RefCell<Option<glib::SourceId>>,
    /// The session bus, where the dialog's D-Bus signals go.
    bus: RefCell<Option<gio::DBusConnection>>,
}

impl PowerUi {
    /// Build the dialog; `lock` locks the session (before sleep).
    pub fn new(app: &gtk::Application, lock: Rc<dyn Fn()>) -> Rc<Self> {
        let nested = is_nested();
        let dialog = gtk::ApplicationWindow::new(app);
        dialog.add_css_class("tuna-end-session");
        dialog.init_layer_shell();
        dialog.set_layer(Layer::Overlay);
        dialog.set_namespace(Some(tuna_shell_control::END_SESSION_NAMESPACE));
        for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
            dialog.set_anchor(edge, true);
        }
        // Cover the top bar too: a modal dims the whole screen.
        dialog.set_exclusive_zone(-1);
        dialog.set_keyboard_mode(KeyboardMode::Exclusive);
        dialog.set_title(Some("End Session"));
        // GNOME's ModalDialog (.modal-dialog.end-session-dialog): a 24em
        // card, the title and countdown centered, two 43px buttons.
        let card = gtk::Box::new(gtk::Orientation::Vertical, 18);
        card.add_css_class("modal-dialog");
        card.set_halign(gtk::Align::Center);
        card.set_valign(gtk::Align::Center);
        card.set_size_request(400, -1);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 18);
        content.add_css_class("modal-dialog-content-box");
        let title = gtk::Label::new(None);
        title.add_css_class("message-dialog-title");
        let body = gtk::Label::new(None);
        body.add_css_class("message-dialog-description");
        body.set_wrap(true);
        body.set_max_width_chars(1);
        body.set_hexpand(true);
        body.set_justify(gtk::Justification::Center);
        content.append(&title);
        content.append(&body);
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        buttons.add_css_class("modal-dialog-button-box");
        buttons.set_homogeneous(true);
        let cancel = gtk::Button::with_label("Cancel");
        cancel.add_css_class("modal-dialog-button");
        let confirm = gtk::Button::with_label("");
        confirm.add_css_class("modal-dialog-button");
        buttons.append(&cancel);
        buttons.append(&confirm);
        // endSessionDialog.js `_applicationSection`: a Dialog.ListSection
        // under the message, shown while inhibitors are listed.
        let applications = gtk::Box::new(gtk::Orientation::Vertical, 18);
        applications.add_css_class("dialog-list");
        let applications_title = gtk::Label::new(Some(BUSY_APPLICATIONS));
        applications_title.add_css_class("dialog-list-title");
        applications_title.set_wrap(true);
        applications_title.set_max_width_chars(1);
        applications_title.set_hexpand(true);
        applications_title.set_justify(gtk::Justification::Center);
        let application_list = gtk::Box::new(gtk::Orientation::Vertical, 12);
        application_list.add_css_class("dialog-list-box");
        let scroll = gtk::ScrolledWindow::new();
        scroll.add_css_class("dialog-list-scrollview");
        scroll.set_hscrollbar_policy(gtk::PolicyType::Never);
        scroll.set_propagate_natural_height(true);
        scroll.set_max_content_height(200);
        scroll.set_child(Some(&application_list));
        applications.append(&applications_title);
        applications.append(&scroll);
        applications.set_visible(false);
        card.append(&content);
        card.append(&applications);
        card.append(&buttons);
        dialog.set_child(Some(&card));

        let ui = Rc::new(Self {
            fade: crate::transient::ModalFade::new(&dialog),
            nested,
            dialog,
            title,
            body,
            confirm,
            applications,
            application_list,
            inhibitors: Cell::new(0),
            pending: Cell::new(None),
            local: Cell::new(false),
            serial: Cell::new(0),
            total: Cell::new(0),
            left: Cell::new(0),
            timer: RefCell::new(None),
            bus: RefCell::new(None),
        });
        {
            let me = ui.clone();
            cancel.connect_clicked(move |_| me.cancel());
        }
        {
            let me = ui.clone();
            ui.confirm.connect_clicked(move |_| me.run_pending());
        }
        {
            // Escape cancels, as in GNOME.
            let keys = gtk::EventControllerKey::new();
            let me = ui.clone();
            keys.connect_key_pressed(move |_, key, _, _| {
                if key == gtk::gdk::Key::Escape {
                    me.cancel();
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            });
            ui.dialog.add_controller(keys);
        }
        // System bus: the before-sleep lock.
        gio::bus_get(
            gio::BusType::System,
            None::<&gio::Cancellable>,
            move |conn| {
                let Ok(conn) = conn else {
                    return;
                };
                let sub = conn.subscribe_to_signal(
                    Some(LOGIND),
                    Some(MANAGER_IFACE),
                    Some("PrepareForSleep"),
                    Some(MANAGER_PATH),
                    None,
                    gio::DBusSignalFlags::NONE,
                    move |signal| {
                        let going = signal
                            .parameters
                            .try_child_value(0)
                            .and_then(|v| v.get::<bool>())
                            .unwrap_or(false);
                        if going {
                            lock();
                        }
                    },
                );
                // Held for the session, with the subscription.
                std::mem::forget(sub);
                std::mem::forget(conn);
            },
        );
        // Administrative lockdown (#357 follow-up): a pending Log Out,
        // Restart or Power Off dialog dies when `disable-log-out`
        // engages, so an already-open countdown can never end the
        // session after the flip.
        if let Some(lockdown) = crate::lockdown::settings() {
            let weak = Rc::downgrade(&ui);
            lockdown.connect_changed(None, move |_, _| {
                if let Some(me) = weak.upgrade() {
                    me.apply_lockdown(crate::lockdown::read().entry_visibility().log_out);
                }
            });
            // Keep the settings object (and its signal) alive.
            std::mem::forget(lockdown);
        }
        serve_end_session_dialog(&ui);
        ui
    }

    /// Menu rows for this session, wired to their actions. Each row
    /// calls `close_menu` first (the quick-settings popover goes away).
    /// The actions the power menu offers here.
    pub fn actions(&self) -> &'static [Action] {
        menu_actions(self.nested)
    }

    /// Run a menu choice through the session manager, as GNOME Shell
    /// 51's `systemActions.js`: Suspend at once, the rest through the
    /// manager's end-session dialog. A Log Out, Restart or Power Off
    /// while `disable-log-out` denies is refused (the action is not
    /// available), live per activation.
    pub fn activate(self: &Rc<Self>, action: Action) {
        if !action_allowed(action, crate::lockdown::read().entry_visibility().log_out) {
            eprintln!("tuna-shell-gtk: {action:?} refused by administrator policy");
            return;
        }
        let Some(bus) = self.bus.borrow().clone() else {
            eprintln!("tuna-shell-gtk: no session bus for {action:?}");
            return;
        };
        if !self.nested {
            call_manager(&bus, action);
            return;
        }
        let me = self.clone();
        glib::spawn_future_local(async move {
            if manager_is_compositors(&bus).await {
                call_manager(&bus, action);
            } else {
                eprintln!(
                    "tuna-shell-gtk: power menu: the session manager is not this compositor's; {action:?} stays local"
                );
                me.ask_for(action, COUNTDOWN_S, Vec::new(), true);
            }
        });
    }

    /// Open the dialog for `action` with a fresh countdown of `seconds`
    /// (`Open`'s `seconds_to_stay_open`), listing the applications behind
    /// `inhibitors`; `local` when the nested preview opens it itself.
    /// A Log Out, Restart or Power Off request while `disable-log-out`
    /// denies never opens (live read per open, so no stale path can
    /// bypass it). Whether it opened.
    fn ask_for(
        self: &Rc<Self>,
        action: Action,
        seconds: u32,
        inhibitors: Vec<String>,
        local: bool,
    ) -> bool {
        if !action_allowed(action, crate::lockdown::read().entry_visibility().log_out) {
            return false;
        }
        self.stop_timer();
        let serial = self.serial.get() + 1;
        self.serial.set(serial);
        self.pending.set(Some(action));
        self.local.set(local);
        self.total.set(seconds);
        self.left.set(seconds);
        self.title.set_text(action.title());
        self.confirm.set_label(action.title());
        self.body
            .set_text(&action.countdown_text(display_seconds(seconds, seconds)));
        self.list_inhibitors(inhibitors, serial);
        self.fade.present();
        // Cancel holds the focus when the dialog opens, as in GNOME.
        if let Some(cancel) = self.confirm.prev_sibling().and_downcast::<gtk::Button>() {
            cancel.grab_focus();
        }
        let me = self.clone();
        let id = glib::timeout_add_local(Duration::from_secs(1), move || {
            let left = me.left.get().saturating_sub(1);
            me.left.set(left);
            if let Some(action) = me.pending.get() {
                me.body
                    .set_text(&action.countdown_text(display_seconds(me.total.get(), left)));
            }
            if left == 0 {
                me.timer.borrow_mut().take();
                me.run_pending();
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
        *self.timer.borrow_mut() = Some(id);
        true
    }

    /// Fill the application section from the manager's inhibitors
    /// (endSessionDialog.js `OpenAsync`, `_onInhibitorLoaded`): shown
    /// while any is still loading or names an application that inhibits
    /// logging out.
    fn list_inhibitors(self: &Rc<Self>, inhibitors: Vec<String>, serial: u64) {
        while let Some(row) = self.application_list.first_child() {
            self.application_list.remove(&row);
        }
        self.inhibitors.set(inhibitors.len());
        self.applications.set_visible(!inhibitors.is_empty());
        let Some(bus) = self.bus.borrow().clone() else {
            return;
        };
        for path in inhibitors {
            let (me, bus) = (self.clone(), bus.clone());
            glib::spawn_future_local(async move {
                let app = inhibiting_app(&bus, &path).await;
                if me.serial.get() != serial {
                    return;
                }
                match app {
                    Some((app, reason)) => me.add_application(&app, &reason),
                    None => {
                        let left = me.inhibitors.get().saturating_sub(1);
                        me.inhibitors.set(left);
                        me.applications.set_visible(left > 0);
                    }
                }
            });
        }
    }

    /// One Dialog.ListSectionItem: the app's 64 px icon, name and reason.
    fn add_application(&self, app: &tuna_shell_host::apps::AppEntry, reason: &str) {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.add_css_class("dialog-list-item");
        let icon = app
            .icon
            .as_deref()
            .and_then(crate::notify::source_icon)
            .map(|icon| gtk::Image::from_gicon(&icon))
            .unwrap_or_else(|| gtk::Image::from_icon_name("application-x-executable"));
        icon.set_pixel_size(64);
        let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
        text.set_valign(gtk::Align::Center);
        let name = gtk::Label::new(Some(&app.name));
        name.add_css_class("dialog-list-item-title");
        name.set_xalign(0.0);
        let description = gtk::Label::new(Some(reason));
        description.add_css_class("dialog-list-item-description");
        description.set_xalign(0.0);
        description.set_wrap(true);
        text.append(&name);
        text.append(&description);
        row.append(&icon);
        row.append(&text);
        self.application_list.append(&row);
    }

    fn stop_timer(&self) {
        if let Some(id) = self.timer.borrow_mut().take() {
            id.remove();
        }
    }

    /// A signal for the session manager awaiting this dialog's answer;
    /// the nested preview's own dialog has none.
    fn emit(&self, signal: &str) {
        if self.local.get() {
            return;
        }
        if let Some(bus) = self.bus.borrow().as_ref() {
            let _ = bus.emit_signal(
                None,
                END_SESSION_DIALOG_PATH,
                END_SESSION_DIALOG_IFACE,
                signal,
                None,
            );
        }
    }

    fn cancel(&self) {
        self.stop_timer();
        let open = self.pending.take().is_some();
        self.fade.hide();
        if open {
            self.emit("Canceled");
            self.emit("Closed");
        }
    }

    /// `Close`: dismiss without an answer.
    fn close(&self) {
        self.stop_timer();
        let open = self.pending.take().is_some();
        self.fade.hide();
        if open {
            self.emit("Closed");
        }
    }

    /// Cancel a pending session-ending dialog while `disable-log-out`
    /// denies. The live watcher calls this on every policy change;
    /// harmless when nothing is pending.
    pub fn apply_lockdown(&self, logout_allowed: bool) {
        if should_cancel_pending(self.pending.get(), logout_allowed) {
            self.cancel();
        }
    }

    fn run_pending(&self) {
        self.stop_timer();
        self.fade.hide();
        if let Some(action) = self.pending.take() {
            // Re-check at fire time: an action denied since the dialog
            // opened must not end the session (gnome-session refuses
            // `end_phase` from Running while locked down).
            if !action_allowed(action, crate::lockdown::read().entry_visibility().log_out) {
                eprintln!("tuna-shell-gtk: {action:?} refused by administrator policy");
                self.emit("Canceled");
                self.emit("Closed");
                return;
            }
            if let Some(signal) = confirmed_signal(action) {
                self.emit(signal);
            }
            // The manager ends the session on the confirmation; only the
            // nested preview's own dialog acts here.
            if self.local.get() {
                perform_local(action);
            }
        }
    }
}

/// Export the dialog as GNOME Shell does, on the shared session
/// connection that also carries `org.gnome.Shell`.
fn serve_end_session_dialog(ui: &Rc<PowerUi>) {
    let info = gio::DBusNodeInfo::for_xml(END_SESSION_DIALOG_XML)
        .ok()
        .and_then(|node| node.lookup_interface(END_SESSION_DIALOG_IFACE));
    let Some(info) = info else {
        eprintln!("tuna-shell-gtk: end-session dialog interface did not parse");
        return;
    };
    let weak = Rc::downgrade(ui);
    gio::bus_get(
        gio::BusType::Session,
        None::<&gio::Cancellable>,
        move |conn| {
            let Ok(conn) = conn else {
                return;
            };
            let Some(ui) = weak.upgrade() else {
                return;
            };
            *ui.bus.borrow_mut() = Some(conn.clone());
            let weak = Rc::downgrade(&ui);
            let registered = conn
                .register_object(END_SESSION_DIALOG_PATH, &info)
                .method_call(move |_, _, _, _, method, params, invocation| {
                    let Some(ui) = weak.upgrade() else {
                        invocation.return_dbus_error(
                            "org.freedesktop.DBus.Error.Failed",
                            "the shell is shutting down",
                        );
                        return;
                    };
                    match method {
                        "Open" => {
                            let Some((kind, seconds, inhibitors)) = open_args(&params) else {
                                invocation.return_dbus_error(
                                    "org.freedesktop.DBus.Error.InvalidArgs",
                                    "Open takes (uuuao)",
                                );
                                return;
                            };
                            let Some(action) = dialog_action(kind) else {
                                // endSessionDialog.js ModalDialogError.UNKNOWN_TYPE.
                                invocation.return_dbus_error(
                                    "org.gnome.Shell.ModalDialog.Error.UnknownType",
                                    "Unknown dialog type requested",
                                );
                                return;
                            };
                            if ui.ask_for(action, seconds, inhibitors, false) {
                                eprintln!(
                                    "tuna-shell-gtk: end-session dialog: {} opened over D-Bus",
                                    action.title()
                                );
                                invocation.return_value(None);
                            } else {
                                invocation.return_dbus_error(
                                    "org.freedesktop.DBus.Error.AccessDenied",
                                    "Ending the session is disabled by the administrator",
                                );
                            }
                        }
                        "Close" => {
                            ui.close();
                            invocation.return_value(None);
                        }
                        _ => invocation
                            .return_dbus_error("org.freedesktop.DBus.Error.UnknownMethod", method),
                    }
                })
                .build();
            if let Err(e) = registered {
                eprintln!("tuna-shell-gtk: end-session dialog object: {e}");
            }
        },
    );
}

/// `Open`'s dialog type, seconds to stay open and inhibitor object
/// paths, when the call is `(uuuao)`.
fn open_args(params: &glib::Variant) -> Option<(u32, u32, Vec<String>)> {
    if params.type_().as_str() != "(uuuao)" {
        return None;
    }
    let inhibitors = params
        .child_value(3)
        .iter()
        .filter_map(|path| path.str().map(str::to_owned))
        .collect();
    Some((
        params.child_value(0).get()?,
        params.child_value(2).get()?,
        inhibitors,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_process_running_the_compositor_binary_is_recognised_by_its_comm() {
        let dir = std::env::temp_dir().join(format!("tuna-comm-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("tuna-compositor");
        // A symlink, not a copy: the kernel names the process after the
        // path it executes, and a freshly written executable can fail with
        // ETXTBSY when another test thread forks while it is still open.
        std::os::unix::fs::symlink("/bin/sleep", &exe).unwrap();
        let mut child = std::process::Command::new(&exe).arg("5").spawn().unwrap();
        let comm = std::fs::read_to_string(format!("/proc/{}/comm", child.id())).unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(is_compositor_comm(&comm), "comm was {comm:?}");
        assert!(!is_compositor_comm("tuna-shell-gtk\n"));
    }

    #[test]
    fn nested_sessions_only_offer_log_out() {
        assert_eq!(menu_actions(true), [Action::LogOut]);
        assert_eq!(
            menu_actions(false),
            [
                Action::Suspend,
                Action::Restart,
                Action::PowerOff,
                Action::LogOut
            ]
        );
    }

    #[test]
    fn lockdown_hides_nested_menu_but_keeps_hardware_menu() {
        // Negative: nested sessions lose their only row and show no menu.
        assert!(!power_menu_visible_for(menu_actions(true), false));
        // Positive: allowed policies show both menus.
        assert!(power_menu_visible_for(menu_actions(true), true));
        assert!(power_menu_visible_for(menu_actions(false), true));
        // Hardware keeps its power rows while Log Out hides.
        assert!(power_menu_visible_for(menu_actions(false), false));
    }

    #[test]
    fn disable_log_out_denies_every_session_ending_action_but_suspend() {
        // Open/fire gate (gnome-session 51 CanShutdown/CanReboot and
        // Logout): Log Out, Restart and Power Off need the policy;
        // Suspend has no lockdown gate and always runs.
        for action in [Action::LogOut, Action::Restart, Action::PowerOff] {
            assert!(!action_allowed(action, false), "{action:?} denied");
            assert!(action_allowed(action, true), "{action:?} allowed");
        }
        assert!(action_allowed(Action::Suspend, false));
        assert!(action_allowed(Action::Suspend, true));
        // Pending-dialog denial: an open session-ending countdown dies
        // when the policy engages; Suspend and an idle dialog never do.
        for action in [Action::LogOut, Action::Restart, Action::PowerOff] {
            assert!(should_cancel_pending(Some(action), false), "{action:?}");
            assert!(!should_cancel_pending(Some(action), true), "{action:?}");
        }
        assert!(!should_cancel_pending(None, false));
        assert!(!should_cancel_pending(Some(Action::Suspend), false));
    }

    #[test]
    fn end_session_dialog_types_and_signals_match_gnome_51() {
        // gnome-session 51 GsmShellEndSessionDialogType.
        assert_eq!(dialog_action(0), Some(Action::LogOut));
        assert_eq!(dialog_action(1), Some(Action::PowerOff));
        assert_eq!(dialog_action(2), Some(Action::Restart));
        assert_eq!(dialog_action(3), None);
        assert_eq!(dialog_action(u32::MAX), None);
        // endSessionDialog.js confirm buttons' signals.
        assert_eq!(
            confirmed_signal(Action::PowerOff),
            Some("ConfirmedShutdown")
        );
        assert_eq!(confirmed_signal(Action::Restart), Some("ConfirmedReboot"));
        assert_eq!(confirmed_signal(Action::LogOut), Some("ConfirmedLogout"));
        assert_eq!(confirmed_signal(Action::Suspend), None);
        // The interface parses and declares exactly GNOME's members.
        let node = gio::DBusNodeInfo::for_xml(END_SESSION_DIALOG_XML).unwrap();
        let iface = node.lookup_interface(END_SESSION_DIALOG_IFACE).unwrap();
        assert!(iface.lookup_method("Open").is_some());
        assert!(iface.lookup_method("Close").is_some());
        // gsm-shell.c sends `(uuu@ao)`: type, timestamp, seconds, inhibitors.
        let inhibitors = glib::Variant::array_from_iter_with_type(
            glib::VariantTy::OBJECT_PATH,
            [
                glib::variant::ObjectPath::try_from("/org/gnome/SessionManager/Inhibitor1")
                    .unwrap()
                    .to_variant(),
            ],
        );
        let open = glib::Variant::tuple_from_iter([
            1u32.to_variant(),
            0u32.to_variant(),
            60u32.to_variant(),
            inhibitors,
        ]);
        assert_eq!(
            open_args(&open),
            Some((1, 60, vec!["/org/gnome/SessionManager/Inhibitor1".into()]))
        );
        assert_eq!(open_args(&(1u32, 0u32, 60u32).to_variant()), None);
        for signal in [
            "ConfirmedLogout",
            "ConfirmedReboot",
            "ConfirmedShutdown",
            "Canceled",
            "Closed",
        ] {
            assert!(iface.lookup_signal(signal).is_some(), "{signal}");
        }
    }

    #[test]
    fn countdown_text_matches_gnome() {
        assert_eq!(
            Action::PowerOff.countdown_text(60),
            "The system will power off automatically in 60 seconds"
        );
        assert_eq!(
            Action::Restart.countdown_text(1),
            "The system will restart automatically in 1 second"
        );
        assert_eq!(
            Action::LogOut.countdown_text(5),
            "You will be logged out automatically in 5 seconds"
        );
    }

    #[test]
    fn countdown_steps_by_ten_until_the_last_ten_seconds_like_gnome() {
        // endSessionDialog.js _roundSecondsToInterval(60, left, 10).
        let shown: Vec<u32> = (1..=60)
            .rev()
            .map(|left| display_seconds(60, left))
            .collect();
        let mut expected = Vec::new();
        for step in [60, 50, 40, 30, 20] {
            expected.extend(std::iter::repeat_n(step, 10));
        }
        expected.extend((1..=10).rev());
        assert_eq!(shown, expected);
        // A shorter Open rounds up no higher than its own total.
        assert_eq!(display_seconds(15, 15), 15);
        assert_eq!(display_seconds(15, 12), 15);
        assert_eq!(display_seconds(15, 10), 10);
        assert_eq!(display_seconds(60, 0), 0);
    }

    #[test]
    fn menu_rows_ask_the_session_manager_like_system_actions() {
        // systemActions.js: SuspendAsync(), RebootAsync(), ShutdownAsync()
        // and LogoutAsync(0) on org.gnome.SessionManager.
        assert_eq!(manager_call(Action::Suspend), ("Suspend", None));
        assert_eq!(manager_call(Action::Restart), ("Reboot", None));
        assert_eq!(manager_call(Action::PowerOff), ("Shutdown", None));
        assert_eq!(
            manager_call(Action::LogOut),
            ("Logout", Some((0u32,).to_variant()))
        );
    }
}
