//! Mutter's alive checks and gnome-shell's close dialog (#1035): a window
//! whose client stops answering pings is reported "not responding", and
//! the user may wait or force it to quit.
//!
//! Reproduced from mutter 51.0:
//! - `src/core/display.c` `meta_display_ping_window`: no ping while
//!   `check-alive-timeout` is 0, one pending ping per window, and a timer
//!   of `check-alive-timeout` milliseconds; `meta_display_ping_timeout`
//!   marks the window not alive and shows its close dialog;
//!   `meta_display_pong_for_serial` marks it alive again.
//! - `src/core/window.c` `meta_window_set_alive` (becoming alive drops the
//!   Wait timer and hides the dialog) and
//!   `meta_window_ensure_close_dialog_timeout` (after Wait the dialog
//!   shows again `check-alive-timeout` later unless the window answered).
//! - `src/core/delete.c` `close_dialog_response_cb` (Force Quit kills,
//!   Wait re-arms), `meta_window_kill` (SIGKILL to the client's pid; a
//!   Wayland client without one gets `WL_DISPLAY_ERROR_NO_MEMORY` posted
//!   on its surface, `meta_window_wayland_kill`) and
//!   `meta_window_free_delete_dialog` (an unmanaged window's dialog hides).
//! - gnome-shell 51.0 `js/ui/closeDialog.js` `vfunc_show`: while shown the
//!   dialog checks the window again every `ALIVE_TIMEOUT` (5 s).
//!
//! Pings go out where mutter sends them (`meta_window_check_alive`):
//! window activation (`meta_window_activate_full`), a button press on a
//! window (`meta_window_handle_ungrabbed_event`), pointer motion over one
//! (`meta_window_check_alive_on_event` from `events.c`) and a close
//! request (`meta_window_delete`); see `windows/alive.rs`.
//!
//! Divergences, stated plainly:
//! - Smithay keeps one outstanding ping per `xdg_wm_base` client, not per
//!   window, and keeps it after our timer gives up on it. A window of a
//!   client that already has a ping out shares that ping, and any valid
//!   pong from the client marks all its windows alive: a late pong to the
//!   ping that timed out dismisses the dialog at once, where mutter waits
//!   for the pong to the shown dialog's next 5 s re-ping.
//! - X11 windows are never pinged: Smithay 0.7's X11 window manager does
//!   not deliver `_NET_WM_PING` replies, so mutter's
//!   `meta_window_x11_ping` cannot be reproduced and no X11 window is
//!   ever reported.
//! - Mutter also marks a window not alive (without the dialog) after 400
//!   motion events during one ping (`MAX_QUEUED_EVENTS`); nothing here
//!   reads `is-alive` apart from the dialog, so that count is not kept.
//! - Nothing is pinged or shown while the session is locked: the lock
//!   clears every check and hides every dialog, and pinging resumes with
//!   the next trigger after unlock.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use tuna_shell_control::CloseDialogResponse;

/// `org.gnome.mutter check-alive-timeout`'s schema default
/// (`data/org.gnome.mutter.gschema.xml.in`), in milliseconds.
pub const DEFAULT_CHECK_ALIVE_TIMEOUT_MS: u32 = 5000;

/// gnome-shell 51 `closeDialog.js` `ALIVE_TIMEOUT`: how often a shown
/// dialog checks its window again.
pub const ALIVE_TIMEOUT: Duration = Duration::from_millis(5000);

/// `meta_window_wayland_kill`'s error code, `WL_DISPLAY_ERROR_NO_MEMORY`
/// (wayland-server generates no server-side `wl_display` enum).
pub const WL_DISPLAY_ERROR_NO_MEMORY: u32 = 2;

/// `meta_window_wayland_kill`'s error message.
pub const KILL_MESSAGE: &str =
    "User requested that we kill you. Sorry. Don't take it too personally.";

/// What the window manager does after a step of the checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// Send this window's client a ping (`META_WINDOW_GET_CLASS->ping`).
    Ping(u64),
    /// Show the close dialog over this window.
    Show(u64),
    /// Hide this window's close dialog.
    Hide(u64),
}

/// The outcome of a dialog answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Response {
    /// No dialog was shown for the window: a stale answer.
    Ignored,
    /// Wait: the dialog hid and shows again unless the window answers.
    Waiting,
    /// Force Quit: kill the window's client (`meta_window_kill`).
    ForceClose,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dialog {
    Hidden,
    /// Shown; closeDialog.js checks the window again at `recheck`.
    Shown {
        recheck: Instant,
    },
    /// Hidden by Wait: `close_dialog_timeout_id` shows it again at `at`.
    Waiting {
        at: Instant,
    },
}

#[derive(Debug, Clone, Copy)]
struct Window {
    /// When the pending ping times out (`MetaPingData.ping_timeout_id`).
    ping: Option<Instant>,
    /// `MetaWindow.is_alive`.
    alive: bool,
    dialog: Dialog,
}

impl Default for Window {
    fn default() -> Self {
        Self {
            ping: None,
            alive: true,
            dialog: Dialog::Hidden,
        }
    }
}

/// Every window's alive check and close dialog, by window id.
#[derive(Debug)]
pub struct AliveChecks {
    /// `check-alive-timeout`; zero disables the check.
    timeout: Duration,
    /// The session is locked: nothing pings or shows.
    suspended: bool,
    windows: BTreeMap<u64, Window>,
}

impl Default for AliveChecks {
    fn default() -> Self {
        Self::new()
    }
}

impl AliveChecks {
    pub fn new() -> Self {
        Self {
            timeout: Duration::from_millis(DEFAULT_CHECK_ALIVE_TIMEOUT_MS.into()),
            suspended: false,
            windows: BTreeMap::new(),
        }
    }

    /// Apply `check-alive-timeout` (milliseconds). Like mutter's
    /// preference, it applies to the pings and Wait timers that follow.
    pub fn set_timeout_ms(&mut self, ms: u32) {
        self.timeout = Duration::from_millis(ms.into());
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Whether [`check_alive`](Self::check_alive) would ping `id` now:
    /// the check is enabled, not suspended, and no ping is out.
    pub fn can_ping(&self, id: u64) -> bool {
        !self.suspended
            && !self.timeout.is_zero()
            && self
                .windows
                .get(&id)
                .is_none_or(|window| window.ping.is_none())
    }

    /// `meta_display_ping_window`: whether to ping `id` now. Never while
    /// the check is disabled or suspended, and never while a ping to the
    /// window is still pending.
    pub fn check_alive(&mut self, id: u64, now: Instant) -> bool {
        if !self.can_ping(id) {
            return false;
        }
        self.windows.entry(id).or_default().ping = Some(now + self.timeout);
        true
    }

    /// The ping could not be sent (its client is gone): forget it.
    pub fn cancel_ping(&mut self, id: u64) {
        if let Some(window) = self.windows.get_mut(&id) {
            window.ping = None;
        }
    }

    /// The windows with a check or a dialog, in id order.
    pub fn tracked(&self) -> impl Iterator<Item = u64> + '_ {
        self.windows.keys().copied()
    }

    /// `meta_display_pong_for_serial`: `id`'s client answered.
    pub fn pong(&mut self, id: u64) -> Option<Event> {
        let window = self.windows.get_mut(&id)?;
        window.ping = None;
        set_alive(id, window, true)
    }

    /// `MetaWindow.is_alive` (untracked windows are alive).
    pub fn is_alive(&self, id: u64) -> bool {
        self.windows.get(&id).is_none_or(|window| window.alive)
    }

    /// Whether `id`'s close dialog is shown.
    pub fn is_shown(&self, id: u64) -> bool {
        self.windows
            .get(&id)
            .is_some_and(|window| matches!(window.dialog, Dialog::Shown { .. }))
    }

    /// Every window whose close dialog is shown, in id order.
    pub fn shown(&self) -> impl Iterator<Item = u64> + '_ {
        self.windows
            .iter()
            .filter(|(_, window)| matches!(window.dialog, Dialog::Shown { .. }))
            .map(|(id, _)| *id)
    }

    /// Run the timers due at `now`: a ping that timed out marks its window
    /// not alive and shows its dialog (`meta_display_ping_timeout`), a
    /// Wait timer shows it again (`meta_window_close_dialog_timeout`), and
    /// a shown dialog re-pings every [`ALIVE_TIMEOUT`] (closeDialog.js).
    pub fn tick(&mut self, now: Instant) -> Vec<Event> {
        let mut events = Vec::new();
        if self.suspended {
            return events;
        }
        let timeout = self.timeout;
        for (&id, window) in &mut self.windows {
            if window.ping.is_some_and(|deadline| deadline <= now) {
                window.ping = None;
                // `meta_window_set_alive (FALSE)` changes nothing else.
                window.alive = false;
                if !matches!(window.dialog, Dialog::Shown { .. }) {
                    window.dialog = Dialog::Shown {
                        recheck: now + ALIVE_TIMEOUT,
                    };
                    events.push(Event::Show(id));
                }
            }
            if let Dialog::Waiting { at } = window.dialog {
                if at <= now {
                    window.dialog = Dialog::Shown {
                        recheck: now + ALIVE_TIMEOUT,
                    };
                    events.push(Event::Show(id));
                }
            }
            if let Dialog::Shown { recheck } = window.dialog {
                if recheck <= now {
                    window.dialog = Dialog::Shown {
                        recheck: now + ALIVE_TIMEOUT,
                    };
                    // `meta_window_check_alive` from the dialog's timer.
                    if !timeout.is_zero() && window.ping.is_none() {
                        window.ping = Some(now + timeout);
                        events.push(Event::Ping(id));
                    }
                }
            }
        }
        events
    }

    /// `close_dialog_response_cb` for `id`'s shown dialog, which hides
    /// (`meta_close_dialog_response`). Wait re-arms the dialog
    /// (`meta_window_ensure_close_dialog_timeout`); Force Quit asks the
    /// caller to kill the client.
    pub fn respond(&mut self, id: u64, response: CloseDialogResponse, now: Instant) -> Response {
        let timeout = self.timeout;
        let Some(window) = self.windows.get_mut(&id) else {
            return Response::Ignored;
        };
        if !matches!(window.dialog, Dialog::Shown { .. }) {
            return Response::Ignored;
        }
        window.dialog = Dialog::Hidden;
        match response {
            CloseDialogResponse::ForceClose => Response::ForceClose,
            CloseDialogResponse::Wait => {
                if !window.alive && !timeout.is_zero() {
                    window.dialog = Dialog::Waiting { at: now + timeout };
                }
                Response::Waiting
            }
        }
    }

    /// Forget every window `live` rejects (unmanaged: its pings and
    /// timers end, `meta_window_free_delete_dialog` hides its dialog).
    pub fn retain(&mut self, mut live: impl FnMut(u64) -> bool) -> Vec<Event> {
        let mut events = Vec::new();
        self.windows.retain(|&id, window| {
            let keep = live(id);
            if !keep && matches!(window.dialog, Dialog::Shown { .. }) {
                events.push(Event::Hide(id));
            }
            keep
        });
        events
    }

    /// Suspend the checks while the session is locked: every shown dialog
    /// hides and every check is dropped, so nothing pings, times out or
    /// shows until unlock.
    pub fn set_suspended(&mut self, suspended: bool) -> Vec<Event> {
        if suspended == self.suspended {
            return Vec::new();
        }
        self.suspended = suspended;
        if !suspended {
            return Vec::new();
        }
        let hidden = self.shown().map(Event::Hide).collect();
        self.windows.clear();
        hidden
    }

    /// The earliest timer: a ping timing out, a Wait timer, or a shown
    /// dialog's re-ping.
    pub fn next_deadline(&self) -> Option<Instant> {
        if self.suspended {
            return None;
        }
        self.windows
            .values()
            .flat_map(|window| {
                let dialog = match window.dialog {
                    Dialog::Hidden => None,
                    Dialog::Shown { recheck } => Some(recheck),
                    Dialog::Waiting { at } => Some(at),
                };
                [window.ping, dialog]
            })
            .flatten()
            .min()
    }
}

/// `meta_window_set_alive`: becoming alive drops the Wait timer and hides
/// a shown dialog.
fn set_alive(id: u64, window: &mut Window, alive: bool) -> Option<Event> {
    if window.alive == alive {
        return None;
    }
    window.alive = alive;
    if !alive {
        return None;
    }
    let shown = matches!(window.dialog, Dialog::Shown { .. });
    window.dialog = Dialog::Hidden;
    shown.then_some(Event::Hide(id))
}

/// The process `meta_window_kill` may SIGKILL for a client with `pid`:
/// none for an unknown pid, init, the compositor itself (an in-process
/// client) or the supervised shell; the caller falls back to the
/// protocol error then.
pub fn kill_target(pid: Option<i32>, own: u32, shell: Option<u32>) -> Option<i32> {
    let pid = pid.filter(|pid| *pid > 1)?;
    let raw = u32::try_from(pid).ok()?;
    (raw != own && Some(raw) != shell).then_some(pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    fn checks(timeout_ms: u32) -> AliveChecks {
        let mut checks = AliveChecks::new();
        checks.set_timeout_ms(timeout_ms);
        checks
    }

    #[test]
    fn schema_default_is_five_seconds() {
        assert_eq!(AliveChecks::new().timeout(), Duration::from_secs(5));
    }

    #[test]
    fn unanswered_ping_shows_the_dialog_after_check_alive_timeout() {
        let t0 = Instant::now();
        let mut checks = checks(5000);
        assert!(checks.check_alive(1, t0));
        assert!(checks.tick(t0 + 4999 * MS).is_empty());
        assert!(checks.is_alive(1));
        assert_eq!(checks.tick(t0 + 5000 * MS), vec![Event::Show(1)]);
        assert!(!checks.is_alive(1));
        assert!(checks.is_shown(1));
        // Shown once: a later timeout while shown shows nothing new.
        assert!(checks.tick(t0 + 5001 * MS).is_empty());
    }

    #[test]
    fn pong_before_the_timeout_never_shows() {
        let t0 = Instant::now();
        let mut checks = checks(5000);
        assert!(checks.check_alive(1, t0));
        assert_eq!(checks.pong(1), None);
        assert!(checks.tick(t0 + 60_000 * MS).is_empty());
        assert!(checks.is_alive(1));
        assert_eq!(checks.next_deadline(), None);
    }

    #[test]
    fn late_pong_hides_the_shown_dialog() {
        let t0 = Instant::now();
        let mut checks = checks(5000);
        checks.check_alive(1, t0);
        checks.tick(t0 + 5000 * MS);
        assert_eq!(checks.pong(1), Some(Event::Hide(1)));
        assert!(checks.is_alive(1));
        assert!(!checks.is_shown(1));
        assert_eq!(checks.pong(1), None);
    }

    #[test]
    fn one_ping_per_window_at_a_time() {
        let t0 = Instant::now();
        let mut checks = checks(5000);
        assert!(checks.check_alive(1, t0));
        assert!(!checks.check_alive(1, t0 + 1000 * MS));
        // The first ping's timer stands.
        assert_eq!(checks.next_deadline(), Some(t0 + 5000 * MS));
        assert!(checks.check_alive(2, t0 + 1000 * MS));
        checks.pong(1);
        assert!(checks.check_alive(1, t0 + 2000 * MS));
    }

    #[test]
    fn zero_timeout_never_pings() {
        let t0 = Instant::now();
        let mut checks = checks(0);
        assert!(!checks.check_alive(1, t0));
        assert!(checks.tick(t0 + 60_000 * MS).is_empty());
        assert_eq!(checks.next_deadline(), None);
    }

    #[test]
    fn shown_dialog_repings_every_alive_timeout() {
        let t0 = Instant::now();
        let mut checks = checks(2000);
        checks.check_alive(1, t0);
        let shown = t0 + 2000 * MS;
        assert_eq!(checks.tick(shown), vec![Event::Show(1)]);
        assert_eq!(checks.next_deadline(), Some(shown + ALIVE_TIMEOUT));
        assert_eq!(checks.tick(shown + ALIVE_TIMEOUT), vec![Event::Ping(1)]);
        // That ping times out: the dialog stays, nothing new shows.
        assert!(checks.tick(shown + ALIVE_TIMEOUT + 2000 * MS).is_empty());
        assert!(checks.is_shown(1));
        assert_eq!(checks.tick(shown + 2 * ALIVE_TIMEOUT), vec![Event::Ping(1)]);
    }

    #[test]
    fn wait_hides_and_shows_again_after_the_timeout_unless_alive() {
        let t0 = Instant::now();
        let mut checks = checks(5000);
        checks.check_alive(1, t0);
        checks.tick(t0 + 5000 * MS);
        let waited = t0 + 6000 * MS;
        assert_eq!(
            checks.respond(1, CloseDialogResponse::Wait, waited),
            Response::Waiting
        );
        assert!(!checks.is_shown(1));
        assert_eq!(checks.next_deadline(), Some(waited + 5000 * MS));
        assert!(checks.tick(waited + 4999 * MS).is_empty());
        assert_eq!(checks.tick(waited + 5000 * MS), vec![Event::Show(1)]);
        // Wait again, then the window answers: the timer is gone.
        let again = waited + 6000 * MS;
        checks.respond(1, CloseDialogResponse::Wait, again);
        assert!(checks.check_alive(1, again));
        assert_eq!(checks.pong(1), None);
        assert!(checks.tick(again + 60_000 * MS).is_empty());
        assert!(checks.is_alive(1));
    }

    #[test]
    fn answers_apply_only_to_a_shown_dialog() {
        let t0 = Instant::now();
        let mut checks = checks(5000);
        assert_eq!(
            checks.respond(1, CloseDialogResponse::ForceClose, t0),
            Response::Ignored
        );
        checks.check_alive(1, t0);
        assert_eq!(
            checks.respond(1, CloseDialogResponse::ForceClose, t0),
            Response::Ignored
        );
        checks.tick(t0 + 5000 * MS);
        assert_eq!(
            checks.respond(1, CloseDialogResponse::ForceClose, t0 + 5000 * MS),
            Response::ForceClose
        );
        assert!(!checks.is_shown(1));
        assert_eq!(
            checks.respond(1, CloseDialogResponse::Wait, t0 + 5000 * MS),
            Response::Ignored
        );
    }

    #[test]
    fn lock_hides_every_dialog_and_stops_the_checks() {
        let t0 = Instant::now();
        let mut checks = checks(5000);
        checks.check_alive(1, t0);
        checks.check_alive(2, t0);
        checks.tick(t0 + 5000 * MS);
        checks.pong(2);
        assert_eq!(checks.set_suspended(true), vec![Event::Hide(1)]);
        assert!(checks.set_suspended(true).is_empty());
        assert!(!checks.check_alive(3, t0 + 6000 * MS));
        assert!(checks.tick(t0 + 60_000 * MS).is_empty());
        assert_eq!(checks.next_deadline(), None);
        assert!(checks.set_suspended(false).is_empty());
        assert!(checks.is_alive(1));
        assert!(checks.check_alive(1, t0 + 61_000 * MS));
    }

    #[test]
    fn a_gone_window_hides_its_dialog() {
        let t0 = Instant::now();
        let mut checks = checks(5000);
        checks.check_alive(1, t0);
        checks.check_alive(2, t0);
        checks.tick(t0 + 5000 * MS);
        assert_eq!(checks.retain(|id| id == 2), vec![Event::Hide(1)]);
        assert_eq!(checks.tracked().collect::<Vec<_>>(), vec![2]);
    }

    #[test]
    fn kill_never_targets_the_compositor_the_shell_or_init() {
        assert_eq!(kill_target(Some(4242), 100, Some(200)), Some(4242));
        assert_eq!(kill_target(Some(100), 100, Some(200)), None);
        assert_eq!(kill_target(Some(200), 100, Some(200)), None);
        assert_eq!(kill_target(Some(1), 100, None), None);
        assert_eq!(kill_target(Some(-5), 100, None), None);
        assert_eq!(kill_target(None, 100, None), None);
    }
}
