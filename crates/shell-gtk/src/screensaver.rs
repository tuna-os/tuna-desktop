//! GNOME's screen-saver API, backed by the compositor's confirmed lock state.
//! A bus caller can request a lock, but cannot bypass password authentication.
//!
//! As in gnome-shell (`screenShield.js` `_lockScreenShown`, `shellDBus.js`
//! `LockAsync`), the screen saver is active, and `Lock` returns, only once
//! the lock screen is shown: the compositor's curtain has landed.
//! gnome-settings-daemon blanks the screen on `ActiveChanged(true)`, and
//! wakes it only on input or `WakeUpScreen`, which every unlock emits
//! (`_continueDeactivate`): an unlock without input, such as
//! `loginctl unlock-session`, must not leave the session blanked.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk4::{gio, glib, prelude::*};

const NAME: &str = "org.gnome.ScreenSaver";
const PATH: &str = "/org/gnome/ScreenSaver";
const XML: &str = r#"<node><interface name="org.gnome.ScreenSaver">
  <method name="Lock"/>
  <method name="GetActive"><arg type="b" direction="out"/></method>
  <method name="SetActive"><arg type="b" direction="in"/></method>
  <method name="GetActiveTime"><arg type="u" direction="out"/></method>
  <signal name="ActiveChanged"><arg type="b"/></signal>
  <signal name="WakeUpScreen"/>
</interface></node>"#;

type Lock = Rc<dyn Fn() -> Result<(), &'static str>>;
type Active = Rc<dyn Fn() -> bool>;

pub struct Service {
    conn: RefCell<Option<gio::DBusConnection>>,
    since: RefCell<Option<Instant>>,
    locked: Cell<bool>,
}

/// What one [`Service::sync`] publishes, in order.
#[derive(Debug, PartialEq, Eq)]
struct Signals {
    /// `WakeUpScreen`: the compositor confirmed an unlock.
    wake: bool,
    /// `ActiveChanged` with the new value.
    active: Option<bool>,
}

/// The signals for a sync from `was_locked`/`was_active` to the
/// compositor's confirmed `locked` (`None` while unknown) and `shown`.
/// gnome-shell's `deactivate` emits `wake-up-screen` before it lifts the
/// curtain and only then `active-changed`, so the wake comes first.
fn signals(was_locked: bool, was_active: bool, locked: Option<bool>, shown: bool) -> Signals {
    Signals {
        wake: was_locked && locked == Some(false),
        active: (shown != was_active).then_some(shown),
    }
}

impl Service {
    /// Publish transitions after the control client has received lock state;
    /// `locked` is the compositor's confirmed lock (`None` without a current
    /// snapshot), `shown` whether the lock screen is shown.
    pub fn sync(&self, locked: Option<bool>, shown: bool) {
        let mut since = self.since.borrow_mut();
        let signals = signals(self.locked.get(), since.is_some(), locked, shown);
        if let Some(locked) = locked {
            self.locked.set(locked);
        }
        if let Some(active) = signals.active {
            *since = active.then(Instant::now);
        }
        let conn = self.conn.borrow();
        let Some(conn) = conn.as_ref() else {
            return;
        };
        if signals.wake {
            let _ = conn.emit_signal(None, PATH, NAME, "WakeUpScreen", None);
        }
        if let Some(active) = signals.active {
            let _ = conn.emit_signal(
                None,
                PATH,
                NAME,
                "ActiveChanged",
                Some(&(active,).to_variant()),
            );
        }
    }

    /// shellDBus.js relays screenShield's `wake-up-screen` as
    /// `WakeUpScreen`; gnome-settings-daemon's power plugin answers it by
    /// keeping the display on for a while (`handle_wake_up_screen`).
    pub fn wake_up_screen(&self) {
        if let Some(conn) = self.conn.borrow().as_ref() {
            let _ = conn.emit_signal(None, PATH, NAME, "WakeUpScreen", None);
        }
    }
}

pub fn start(lock: Lock, active: Active) -> Rc<Service> {
    let service = Rc::new(Service {
        conn: RefCell::new(None),
        since: RefCell::new(None),
        locked: Cell::new(false),
    });
    let info = gio::DBusNodeInfo::for_xml(XML)
        .expect("screen-saver interface XML")
        .lookup_interface(NAME)
        .expect("screen-saver interface");
    let svc = service.clone();
    // GIO uses the same shared session connection as org.gnome.Shell, so
    // the object is reachable through both names, as GNOME components expect.
    let _owner = gio::bus_own_name(
        gio::BusType::Session,
        NAME,
        gio::BusNameOwnerFlags::NONE,
        move |conn, _| {
            *svc.conn.borrow_mut() = Some(conn.clone());
            let (lock, active, svc) = (lock.clone(), active.clone(), svc.clone());
            let result = conn
                .register_object(PATH, &info)
                .method_call(move |_, _, _, _, method, params, invocation| {
                    match method {
                        "GetActive" => invocation.return_value(Some(&(active(),).to_variant())),
                        "GetActiveTime" => {
                            let seconds = svc
                                .since
                                .borrow()
                                .map_or(0, |t| t.elapsed().as_secs().min(u32::MAX as u64) as u32);
                            invocation.return_value(Some(&(seconds,).to_variant()));
                        }
                        "Lock" | "SetActive" => {
                            let requested = method == "Lock"
                                || params.get::<(bool,)>().is_some_and(|(value,)| value);
                            // SetActive(false) never unlocks a locked session.
                            if !requested || active() {
                                invocation.return_value(None);
                                return;
                            }
                            if let Err(message) = lock() {
                                invocation.return_dbus_error(
                                    "org.freedesktop.DBus.Error.Failed",
                                    message,
                                );
                                return;
                            }
                            let active = active.clone();
                            let started = Instant::now();
                            let mut reply = Some(invocation);
                            glib::timeout_add_local(Duration::from_millis(50), move || {
                                if active() {
                                    reply.take().unwrap().return_value(None);
                                    glib::ControlFlow::Break
                                } else if started.elapsed() >= Duration::from_secs(5) {
                                    reply.take().unwrap().return_dbus_error(
                                        "org.freedesktop.DBus.Error.Failed",
                                        "compositor did not confirm the lock",
                                    );
                                    glib::ControlFlow::Break
                                } else {
                                    glib::ControlFlow::Continue
                                }
                            });
                        }
                        _ => invocation
                            .return_dbus_error("org.freedesktop.DBus.Error.UnknownMethod", method),
                    }
                })
                .build();
            if let Err(error) = result {
                eprintln!("tuna-shell-gtk: screen-saver interface: {error}");
            }
        },
        |_, _| {},
        |_, _| eprintln!("tuna-shell-gtk: screen-saver name owned elsewhere"),
    );
    service
}

#[cfg(test)]
mod tests {
    use super::{signals, Signals};

    /// Locking publishes nothing until the curtain lands; then the screen
    /// saver goes active (gnome-settings-daemon blanks on it).
    #[test]
    fn active_follows_the_shown_lock_screen() {
        let quiet = Signals {
            wake: false,
            active: None,
        };
        assert_eq!(signals(false, false, Some(true), false), quiet);
        assert_eq!(
            signals(true, false, Some(true), true),
            Signals {
                wake: false,
                active: Some(true)
            }
        );
        assert_eq!(signals(true, true, Some(true), true), quiet);
    }

    /// Every confirmed unlock wakes the screen before the screen saver goes
    /// inactive (`_continueDeactivate`), whether or not the lock screen was
    /// shown; without it `loginctl unlock-session` after the blank left the
    /// unlocked session dark until the next input.
    #[test]
    fn every_confirmed_unlock_wakes_the_screen_first() {
        assert_eq!(
            signals(true, true, Some(false), false),
            Signals {
                wake: true,
                active: Some(false)
            }
        );
        assert_eq!(
            signals(true, false, Some(false), false),
            Signals {
                wake: true,
                active: None
            }
        );
        // Already unlocked, or no current snapshot: nothing to wake.
        assert!(!signals(false, false, Some(false), false).wake);
        assert!(!signals(true, true, None, false).wake);
    }
}
