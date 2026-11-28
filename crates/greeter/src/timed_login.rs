//! Timed login as gnome-shell 51's login dialog runs it
//! (`loginDialog.js` `_onTimedLoginRequested`, `_startTimedLogin`):
//! GDM's `TimedLoginRequested(user, delay)` (the answer to
//! `GetTimedLoginDetails`) starts a countdown on the user's row (or on
//! the prompt when the user list is disabled). With a delay over 5 s the
//! indicator waits until nobody touched the screen for 5 s
//! (`_TIMED_LOGIN_IDLE_THRESHOLD`), cancels an open prompt and runs for
//! the rest; any key or click starts it over. When it runs out the
//! greeter asks GDM to `BeginAutoLogin(user)`.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;

use crate::gdm::Call;
use crate::{trace, Greeter};

/// loginDialog.js `_TIMED_LOGIN_IDLE_THRESHOLD`.
pub const IDLE_THRESHOLD_SECS: i32 = 5;
/// The indicator's frame (`showTimedLoginIndicator`'s 33 ms).
const FRAME: Duration = Duration::from_millis(33);

#[derive(Default)]
pub struct Timed {
    request: RefCell<Option<(String, i32)>>,
    /// Bumped on every start: older runs stop.
    generation: Cell<u64>,
    /// A run is in progress (`_timedLoginBatch`).
    running: Cell<bool>,
    captured: Cell<bool>,
    /// Each listed user's indicator (`UserListItem._timedLoginIndicator`).
    pub indicators: RefCell<HashMap<String, gtk::ProgressBar>>,
}

/// One user row's timed login indicator.
pub fn indicator() -> gtk::ProgressBar {
    let bar = gtk::ProgressBar::new();
    bar.add_css_class("login-dialog-timed-login-indicator");
    bar.update_property(&[gtk::accessible::Property::Label("Automatic login")]);
    bar.set_visible(false);
    bar
}

/// The animation length and whether it waits for 5 idle seconds first.
pub fn schedule(delay: i32) -> (bool, i32) {
    if delay > IDLE_THRESHOLD_SECS {
        (true, delay - IDLE_THRESHOLD_SECS)
    } else {
        (false, delay)
    }
}

impl Greeter {
    pub(crate) fn on_timed_login_requested(self: &Rc<Self>, user: &str, delay: i32) {
        trace(&format!(
            "greeter timed-login requested user={user} delay={delay}"
        ));
        if self.timed.running.get() {
            return;
        }
        *self.timed.request.borrow_mut() = Some((user.to_owned(), delay));
        self.start_timed_login(true);
        if self.timed.captured.replace(true) {
            return;
        }
        // Start over on any key or click (the stage's capture actions).
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, _, _, _| {
            if let Some(greeter) = weak.upgrade() {
                greeter.restart_timed_login();
            }
            glib::Propagation::Proceed
        });
        self.window.add_controller(keys);
        let click = gtk::GestureClick::new();
        click.set_button(0);
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        click.connect_pressed(move |_, _, _, _| {
            if let Some(greeter) = weak.upgrade() {
                greeter.restart_timed_login();
            }
        });
        self.window.add_controller(click);
    }

    fn restart_timed_login(self: &Rc<Self>) {
        if self.timed.request.borrow().is_some() {
            trace("greeter timed-login restart");
            self.start_timed_login(false);
        }
    }

    fn hide_timed_indicators(&self) {
        for bar in self.timed.indicators.borrow().values() {
            bar.set_visible(false);
            bar.set_fraction(0.0);
        }
        self.auth_ui.timed.set_visible(false);
        self.auth_ui.timed.set_fraction(0.0);
    }

    fn start_timed_login(self: &Rc<Self>, first_run: bool) {
        let Some((user, delay)) = self.timed.request.borrow().clone() else {
            return;
        };
        let first_run = first_run && !self.timed.running.get();
        let generation = self.timed.generation.get() + 1;
        self.timed.generation.set(generation);
        self.timed.running.set(true);
        self.hide_timed_indicators();
        let weak = Rc::downgrade(self);
        let current = move || {
            weak.upgrade()
                .filter(|g| g.timed.generation.get() == generation)
        };
        glib::spawn_future_local(async move {
            // `_waitForItemForUser`.
            loop {
                let Some(greeter) = current() else {
                    return;
                };
                if greeter.policy.disable_user_list
                    || greeter.users.borrow().iter().any(|u| u.name == user)
                {
                    break;
                }
                drop(greeter);
                glib::timeout_future(Duration::from_millis(200)).await;
            }
            let (wait, seconds) = schedule(delay);
            if wait {
                glib::timeout_future_seconds(IDLE_THRESHOLD_SECS as u32).await;
            }
            let Some(greeter) = current() else {
                return;
            };
            let bar = if greeter.policy.disable_user_list {
                greeter.auth_ui.timed.clone()
            } else {
                if wait && greeter.on_prompt() {
                    // `this._authPrompt.cancel()`.
                    greeter.back();
                }
                let row = greeter
                    .users
                    .borrow()
                    .iter()
                    .position(|u| u.name == user)
                    .and_then(|i| greeter.user_list.row_at_index(i32::try_from(i).ok()?));
                if wait || first_run {
                    if let Some(row) = row {
                        row.grab_focus();
                    }
                }
                let Some(bar) = greeter.timed.indicators.borrow().get(&user).cloned() else {
                    greeter.timed.running.set(false);
                    return;
                };
                bar
            };
            trace(&format!(
                "greeter timed-login indicator user={user} secs={seconds}"
            ));
            bar.set_fraction(0.0);
            bar.set_visible(true);
            drop(greeter);
            let start = Instant::now();
            let total = Duration::from_secs(u64::try_from(seconds.max(0)).unwrap_or(0));
            loop {
                let elapsed = start.elapsed();
                if current().is_none() {
                    return;
                }
                if elapsed >= total {
                    break;
                }
                bar.set_fraction(elapsed.as_secs_f64() / total.as_secs_f64());
                glib::timeout_future(FRAME).await;
            }
            bar.set_fraction(1.0);
            let Some(greeter) = current() else {
                return;
            };
            greeter.timed.running.set(false);
            trace(&format!("greeter timed-login begin user={user}"));
            greeter.send(Call::BeginAutoLogin { user });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_delays_wait_for_five_idle_seconds() {
        assert_eq!(schedule(30), (true, 25));
        assert_eq!(schedule(6), (true, 1));
        assert_eq!(schedule(5), (false, 5));
        assert_eq!(schedule(2), (false, 2));
    }
}
