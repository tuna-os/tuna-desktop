//! The login screen's power actions and end-session dialog (#1164).
//!
//! gnome-shell 51.1 `status/system.js` `ShutdownItem` (the quick-settings
//! power button, "Power Off Menu", headed "Power Off"): Suspend, Restart…
//! and Power Off…; Log Out and Switch User are never available at the
//! login screen. `misc/systemActions.js` offers each when the session
//! manager can do it and, at the login screen, hides all three under
//! `org.gnome.login-screen disable-restart-buttons` (followed live).
//! Suspend acts at once; Restart… and Power Off… open
//! `endSessionDialog.js`'s dialog: the title, the 60-second countdown
//! (in steps of ten until the last ten), Cancel and the action, which also
//! runs when the countdown ends.
//!
//! Deviation (docs/greeter.md): GNOME asks gnome-session
//! (`CanShutdown`, `Shutdown`, …), which asks logind; the greeter asks
//! logind directly (`CanSuspend`/`CanReboot`/`CanPowerOff`,
//! `Suspend`/`Reboot`/`PowerOff` with interactive authorization), as the
//! login session has no inhibitors or other sessions of its own to
//! consider. Power actions exist on hardware only (`TUNA_SESSION_KIND`):
//! a nested login screen's logind is the host's.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gio::prelude::*;
use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;

use crate::i18n::{ctr, ntr, tr};

const LOGIND: &str = "org.freedesktop.login1";
const LOGIND_PATH: &str = "/org/freedesktop/login1";
const LOGIND_MANAGER: &str = "org.freedesktop.login1.Manager";
const LOGIN_SCREEN_SCHEMA: &str = "org.gnome.login-screen";
const DISABLE_RESTART: &str = "disable-restart-buttons";
/// endSessionDialog.js `_totalSecondsToStayOpen`.
pub const COUNTDOWN_S: u32 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Suspend,
    Restart,
    PowerOff,
}

impl Action {
    pub const ALL: [Action; 3] = [Action::Suspend, Action::Restart, Action::PowerOff];

    /// The menu item (system.js `_addSystemAction`).
    fn item(self) -> String {
        match self {
            Action::Suspend => tr("Suspend"),
            Action::Restart => tr("Restart…"),
            Action::PowerOff => tr("Power Off…"),
        }
    }

    /// logind's availability query and action.
    fn logind(self) -> (&'static str, &'static str) {
        match self {
            Action::Suspend => ("CanSuspend", "Suspend"),
            Action::Restart => ("CanReboot", "Reboot"),
            Action::PowerOff => ("CanPowerOff", "PowerOff"),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Action::Suspend => "suspend",
            Action::Restart => "restart",
            Action::PowerOff => "power-off",
        }
    }
}

/// logind's `Can*` answer: available unless `na` or `no`
/// (`ActionAvailability.UNAVAILABLE`).
pub fn available(answer: &str) -> bool {
    matches!(answer, "yes" | "challenge")
}

/// endSessionDialog.js `_roundSecondsToInterval`.
pub fn displayed_seconds(total: u32, left: u32, interval: u32) -> u32 {
    if left <= interval {
        return left;
    }
    let time = left + interval - 1;
    if time > total {
        total
    } else {
        time - time % interval
    }
}

/// The dialog's title, countdown text and confirm button.
pub fn dialog_text(action: Action, seconds: u32) -> (String, String, String) {
    match action {
        Action::Restart => (
            ctr("title", "Restart"),
            ntr(
                "The system will restart automatically in %d second",
                "The system will restart automatically in %d seconds",
                seconds,
            ),
            ctr("button", "Restart"),
        ),
        Action::PowerOff | Action::Suspend => (
            ctr("title", "Power Off"),
            ntr(
                "The system will power off automatically in %d second",
                "The system will power off automatically in %d seconds",
                seconds,
            ),
            ctr("button", "Power Off"),
        ),
    }
}

/// Whether this login screen's logind is its own.
fn hardware() -> bool {
    hardware_session(std::env::var_os("TUNA_SESSION_KIND").as_deref())
}

/// The compositor's `TUNA_SESSION_KIND` (`hardware` or `nested`, as the
/// shell reads it): anything but `hardware` is nested.
fn hardware_session(kind: Option<&std::ffi::OsStr>) -> bool {
    kind.is_some_and(|kind| kind == "hardware")
}

/// Which of [`Action::ALL`] the menu offers, from logind's answers, and
/// whether the power button shows at all: `systemActions.js` hides every
/// action under `disable-restart-buttons`, and with none left the
/// quick-settings button goes too.
fn offered(can: [bool; 3], disabled: bool) -> ([bool; 3], bool) {
    let shown = can.map(|can| can && !disabled);
    (shown, shown.contains(&true))
}

/// The power button and its menu, and the dialog.
pub struct Power {
    /// The quick-settings power button ("Power Off Menu").
    pub button: gtk::ToggleButton,
    /// Its menu, revealed under the quick settings' top row.
    pub menu: gtk::Revealer,
    items: Vec<(Action, gtk::Button)>,
    can: RefCell<[bool; 3]>,
    disabled: Cell<bool>,
    settings: Option<gio::Settings>,
    dialog: Dialog,
    /// Closes the quick settings before an action.
    pub close: RefCell<Option<Box<dyn Fn()>>>,
}

impl Power {
    /// `overlay` shows the dialog above `beneath`, which it makes
    /// insensitive while open.
    pub fn new(overlay: &gtk::Overlay, beneath: &gtk::Widget) -> Rc<Self> {
        let header_icon = gtk::Image::from_icon_name("system-shutdown-symbolic");
        let header = gtk::Label::new(Some(&ctr("title", "Power Off")));
        header.add_css_class("header");
        let title = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        title.add_css_class("quick-settings-menu-header");
        title.append(&header_icon);
        title.append(&header);
        let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
        column.add_css_class("quick-settings-menu");
        column.append(&title);
        let mut items = Vec::new();
        for action in Action::ALL {
            let item = gtk::Button::with_label(&action.item());
            item.add_css_class("flat");
            item.add_css_class("popup-menu-item");
            if let Some(label) = item.child().and_downcast::<gtk::Label>() {
                label.set_xalign(0.0);
            }
            column.append(&item);
            items.push((action, item));
        }
        let menu = gtk::Revealer::new();
        menu.set_child(Some(&column));
        let button = gtk::ToggleButton::new();
        button.set_icon_name("system-shutdown-symbolic");
        button.add_css_class("icon-button");
        button.update_property(&[gtk::accessible::Property::Label(&tr("Power Off Menu"))]);
        let settings = tuna_shell_control::gsettings::settings(LOGIN_SCREEN_SCHEMA);
        let power = Rc::new(Self {
            button,
            menu,
            items,
            can: RefCell::new([false; 3]),
            disabled: Cell::new(false),
            settings,
            dialog: Dialog::new(overlay, beneath),
            close: RefCell::default(),
        });
        {
            let menu = power.menu.clone();
            power.button.connect_toggled(move |button| {
                menu.set_reveal_child(button.is_active());
            });
        }
        for (action, item) in &power.items {
            let weak = Rc::downgrade(&power);
            let action = *action;
            item.connect_clicked(move |_| {
                if let Some(power) = weak.upgrade() {
                    power.activate(action);
                }
            });
        }
        if let Some(settings) = &power.settings {
            if settings
                .settings_schema()
                .is_some_and(|s| s.has_key(DISABLE_RESTART))
            {
                power.disabled.set(settings.boolean(DISABLE_RESTART));
                let weak = Rc::downgrade(&power);
                settings.connect_changed(Some(DISABLE_RESTART), move |s, key| {
                    if let Some(power) = weak.upgrade() {
                        power.disabled.set(s.boolean(key));
                        power.sync();
                    }
                });
            }
        }
        power.sync();
        power.refresh();
        power
    }

    /// Ask logind again (ShutdownItem's `forceUpdate` when it maps).
    pub fn refresh(self: &Rc<Self>) {
        if !hardware() {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Ok(system) = gio::bus_get_future(gio::BusType::System).await else {
                return;
            };
            let mut can = [false; 3];
            for (slot, action) in can.iter_mut().zip(Action::ALL) {
                let (query, _) = action.logind();
                *slot = system
                    .call_future(
                        Some(LOGIND),
                        LOGIND_PATH,
                        LOGIND_MANAGER,
                        query,
                        None,
                        Some(glib::VariantTy::new("(s)").expect("valid type")),
                        gio::DBusCallFlags::NONE,
                        -1,
                    )
                    .await
                    .ok()
                    .and_then(|reply| reply.child_value(0).get::<String>())
                    .is_some_and(|answer| available(&answer));
            }
            if let Some(power) = weak.upgrade() {
                *power.can.borrow_mut() = can;
                power.sync();
            }
        });
    }

    fn sync(&self) {
        let (shown, any) = offered(*self.can.borrow(), self.disabled.get());
        for ((action, item), shown) in self.items.iter().zip(shown) {
            item.set_visible(shown);
            crate::trace(&format!(
                "greeter power {} available={shown}",
                action.name()
            ));
        }
        if !any {
            self.button.set_active(false);
        }
        self.button.set_visible(any);
    }

    fn activate(self: &Rc<Self>, action: Action) {
        self.button.set_active(false);
        if let Some(close) = self.close.borrow().as_ref() {
            close();
        }
        crate::trace(&format!("greeter power activate={}", action.name()));
        match action {
            Action::Suspend => call(action),
            Action::Restart | Action::PowerOff => self.dialog.open(action),
        }
    }
}

/// logind's action, interactive as gnome-session asks it.
fn call(action: Action) {
    let (_, method) = action.logind();
    crate::trace(&format!("greeter logind {method}"));
    glib::spawn_future_local(async move {
        let Ok(system) = gio::bus_get_future(gio::BusType::System).await else {
            return;
        };
        if let Err(error) = system
            .call_future(
                Some(LOGIND),
                LOGIND_PATH,
                LOGIND_MANAGER,
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
}

/// endSessionDialog.js's dialog over the login screen.
#[derive(Clone)]
struct Dialog {
    shade: gtk::Box,
    frame: gtk::Box,
    beneath: gtk::Widget,
    title: gtk::Label,
    description: gtk::Label,
    cancel: gtk::Button,
    confirm: gtk::Button,
    state: Rc<DialogState>,
}

#[derive(Default)]
struct DialogState {
    action: Cell<Option<Action>>,
    left: Cell<u32>,
    timer: RefCell<Option<glib::SourceId>>,
}

impl Dialog {
    fn new(overlay: &gtk::Overlay, beneath: &gtk::Widget) -> Self {
        let title = gtk::Label::new(None);
        title.add_css_class("message-dialog-title");
        title.set_wrap(true);
        let description = gtk::Label::new(None);
        description.add_css_class("message-dialog-description");
        description.set_wrap(true);
        description.set_justify(gtk::Justification::Center);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.add_css_class("message-dialog-content");
        content.append(&title);
        content.append(&description);
        let cancel = gtk::Button::with_label(&tr("Cancel"));
        cancel.add_css_class("modal-dialog-button");
        cancel.set_hexpand(true);
        let confirm = gtk::Button::new();
        confirm.add_css_class("modal-dialog-button");
        confirm.set_hexpand(true);
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        buttons.add_css_class("modal-dialog-button-box");
        buttons.set_homogeneous(true);
        buttons.append(&cancel);
        buttons.append(&confirm);
        let frame = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .accessible_role(gtk::AccessibleRole::Dialog)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build();
        frame.add_css_class("end-session-dialog");
        frame.append(&content);
        frame.append(&buttons);
        let shade = gtk::Box::new(gtk::Orientation::Vertical, 0);
        shade.add_css_class("modal-dialog-shade");
        shade.append(&frame);
        shade.set_visible(false);
        // Clicks on the shade go nowhere else.
        shade.add_controller(gtk::GestureClick::new());
        overlay.add_overlay(&shade);
        let this = Self {
            shade,
            frame,
            beneath: beneath.clone(),
            title,
            description,
            cancel,
            confirm,
            state: Rc::default(),
        };
        {
            let dialog = this.clone();
            this.cancel.connect_clicked(move |_| dialog.close("cancel"));
        }
        {
            let dialog = this.clone();
            this.confirm.connect_clicked(move |_| dialog.confirm());
        }
        let keys = gtk::EventControllerKey::new();
        {
            let dialog = this.clone();
            keys.connect_key_pressed(move |_, key, _, _| {
                if key == gtk::gdk::Key::Escape {
                    dialog.close("cancel");
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            });
        }
        this.shade.add_controller(keys);
        this
    }

    fn open(&self, action: Action) {
        self.state.action.set(Some(action));
        self.state.left.set(COUNTDOWN_S);
        self.render();
        self.beneath.set_sensitive(false);
        self.shade.set_visible(true);
        self.cancel.grab_focus();
        crate::trace(&format!("greeter end-session open={}", action.name()));
        let dialog = self.clone();
        let id = glib::timeout_add_seconds_local(1, move || {
            let left = dialog.state.left.get().saturating_sub(1);
            dialog.state.left.set(left);
            if left == 0 {
                dialog.state.timer.borrow_mut().take();
                dialog.confirm();
                return glib::ControlFlow::Break;
            }
            dialog.render();
            glib::ControlFlow::Continue
        });
        if let Some(old) = self.state.timer.borrow_mut().replace(id) {
            old.remove();
        }
    }

    fn render(&self) {
        let Some(action) = self.state.action.get() else {
            return;
        };
        let seconds = displayed_seconds(COUNTDOWN_S, self.state.left.get(), 10);
        let (title, description, confirm) = dialog_text(action, seconds);
        self.title.set_label(&title);
        self.description.set_label(&description);
        self.confirm.set_label(&confirm);
        self.frame
            .update_property(&[gtk::accessible::Property::Label(&title)]);
    }

    fn confirm(&self) {
        let Some(action) = self.state.action.get() else {
            return;
        };
        self.close("confirm");
        call(action);
    }

    fn close(&self, how: &str) {
        let Some(action) = self.state.action.take() else {
            return;
        };
        if let Some(id) = self.state.timer.borrow_mut().take() {
            id.remove();
        }
        crate::trace(&format!("greeter end-session {how}={}", action.name()));
        self.shade.set_visible(false);
        self.beneath.set_sensitive(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logind_answers_are_gnomes_availability() {
        assert!(available("yes"));
        assert!(available("challenge"));
        assert!(!available("no"));
        assert!(!available("na"));
    }

    #[test]
    fn the_countdown_shows_tens_until_the_last_ten() {
        assert_eq!(displayed_seconds(60, 60, 10), 60);
        assert_eq!(displayed_seconds(60, 59, 10), 60);
        assert_eq!(displayed_seconds(60, 51, 10), 60);
        assert_eq!(displayed_seconds(60, 50, 10), 50);
        assert_eq!(displayed_seconds(60, 11, 10), 20);
        assert_eq!(displayed_seconds(60, 10, 10), 10);
        assert_eq!(displayed_seconds(60, 3, 10), 3);
    }

    #[test]
    fn the_dialog_says_what_gnome_says() {
        assert_eq!(
            dialog_text(Action::Restart, 60),
            (
                "Restart".to_owned(),
                "The system will restart automatically in 60 seconds".to_owned(),
                "Restart".to_owned()
            )
        );
        assert_eq!(
            dialog_text(Action::PowerOff, 1).1,
            "The system will power off automatically in 1 second"
        );
    }

    #[test]
    fn disable_restart_buttons_hides_every_action_and_the_button() {
        let all = [true; 3];
        assert_eq!(offered(all, false), (all, true));
        assert_eq!(offered(all, true), ([false; 3], false));
        // logind's own answers: only what it can do shows.
        assert_eq!(
            offered([false, true, false], false),
            ([false, true, false], true)
        );
        // Nothing available (a nested login screen never asks logind).
        assert_eq!(offered([false; 3], false), ([false; 3], false));
        assert_eq!(offered([false; 3], true), ([false; 3], false));
    }

    #[test]
    fn only_a_hardware_session_offers_power_actions() {
        use std::ffi::OsStr;
        assert!(hardware_session(Some(OsStr::new("hardware"))));
        for kind in ["nested", "", "Hardware", "hardware "] {
            assert!(!hardware_session(Some(OsStr::new(kind))), "{kind:?}");
        }
        assert!(!hardware_session(None));
    }
}
