//! The window manager's side of mutter's alive checks (#1035): xdg-shell
//! pings on GNOME's schedule, pongs, the close dialog's placement for the
//! shell, and Force Quit. The bookkeeping and the GNOME sources it follows
//! are in [`crate::unresponsive`].
//!
//! Ping triggers, as mutter 51 sends them through `meta_window_check_alive`:
//! activation (`window.c` `meta_window_activate_full`), a button press on a
//! window (`meta_window_handle_ungrabbed_event`), pointer motion over it
//! (`events.c` → `meta_window_check_alive_on_event`) and a close request
//! (`delete.c` `meta_window_delete`). The shell's own surfaces are layer
//! and lock surfaces, never pinged; a toplevel of the supervised shell
//! process would be skipped too. Pongs are Smithay's
//! `XdgShellHandler::client_pong` (mutter's `xdg_wm_base_pong` →
//! `meta_display_pong_for_serial`).
//!
//! The dialog: gnome-shell parents it to the window actor, so it covers
//! the window, follows it and hides with it. The shell's GTK dialog is a
//! layer surface, so the compositor sends the window's frame (output
//! local) whenever it changes and hides the dialog while the window is
//! not on screen (minimized, another workspace, the overview).

use std::collections::HashMap;
use std::time::Instant;

use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{DisplayHandle, Resource as _};
use smithay::utils::{Logical, Rectangle, SERIAL_COUNTER};
use smithay::wayland::shell::xdg::ToplevelSurface;
use smithay::wayland::shell::PingError;
use tuna_shell_control::{CloseDialogResponse, Message, MAX_CLOSE_DIALOG_SIDE};

use super::WindowManager;
use crate::unresponsive::{self, AliveChecks, Event, Response};
use crate::State;

/// Where a shown close dialog sits, as last sent to the shell.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Placement {
    app_id: Option<String>,
    connector: Option<String>,
    /// The window's frame, local to `connector`'s output.
    frame: Rectangle<i32, Logical>,
}

/// The manager's alive-check state.
pub(super) struct AliveState {
    checks: AliveChecks,
    /// Client credentials for the shell exemption and Force Quit.
    display: DisplayHandle,
    /// The supervised shell's pid: never pinged, never killed.
    shell_pid: Option<u32>,
    /// Close-dialog messages for the shell, drained by the runtime.
    outbox: Vec<Message>,
    /// Every shown dialog and where the shell has it (`None`: hidden
    /// because its window is off screen).
    placed: HashMap<u64, Option<Placement>>,
}

impl AliveState {
    pub(super) fn new(state: &State) -> Self {
        Self {
            checks: AliveChecks::new(),
            display: state.dh.clone(),
            shell_pid: None,
            outbox: Vec::new(),
            placed: HashMap::new(),
        }
    }

    /// The pid of `surface`'s client from its socket credentials
    /// (`meta_wayland_client_get_pid`).
    fn client_pid(&self, surface: &WlSurface) -> Option<i32> {
        let client = self.display.get_client(surface.id()).ok()?;
        client
            .get_credentials(&self.display)
            .ok()
            .map(|credentials| credentials.pid)
    }
}

impl WindowManager {
    /// `org.gnome.mutter check-alive-timeout` in milliseconds; 0 disables
    /// the check.
    pub fn set_check_alive_timeout(&mut self, ms: u32) {
        self.alive.checks.set_timeout_ms(ms);
    }

    /// The supervised shell's pid, whose windows are never pinged.
    pub fn set_shell_pid(&mut self, pid: Option<u32>) {
        self.alive.shell_pid = pid;
    }

    /// The xdg toplevel `id` pings go to: none for X11 windows (Smithay
    /// delivers no `_NET_WM_PING` replies) and the shell's own windows.
    fn ping_target(&self, id: u64) -> Option<ToplevelSurface> {
        let window = self.windows.get(&id)?;
        let toplevel = window.surface.toplevel()?.clone();
        let shell = self.alive.shell_pid.and_then(|pid| i32::try_from(pid).ok());
        if shell.is_some() && self.alive.client_pid(toplevel.wl_surface()) == shell {
            return None;
        }
        Some(toplevel)
    }

    /// `meta_window_check_alive`: ping `id` unless the check is disabled,
    /// the session is locked or a ping is already out. Pointer motion
    /// calls this per event, so the cheap checks come first.
    pub(super) fn check_alive(&mut self, id: u64) {
        if self.lock_input_active || !self.alive.checks.can_ping(id) {
            return;
        }
        let Some(toplevel) = self.ping_target(id) else {
            return;
        };
        if self.alive.checks.check_alive(id, Instant::now()) {
            self.ping(id, &toplevel);
        }
    }

    /// `meta_window_wayland_ping`: `xdg_wm_base.ping` to the window's
    /// client. A client with a ping already out shares it (Smithay keeps
    /// one per client); a gone client cancels the check.
    fn ping(&mut self, id: u64, toplevel: &ToplevelSurface) {
        match toplevel.client().send_ping(SERIAL_COUNTER.next_serial()) {
            Ok(()) | Err(PingError::PingAlreadyPending(_)) => {}
            Err(PingError::DeadSurface) => self.alive.checks.cancel_ping(id),
        }
    }

    /// One round of the checks at `now`: apply the pongs that arrived,
    /// forget gone windows, run the timers, and bring the shell's dialogs
    /// up to date. While `locked` nothing pings or shows.
    pub fn run_alive_checks(&mut self, state: &mut State, now: Instant, locked: bool) {
        self.alive
            .checks
            .set_suspended(locked || self.lock_input_active);
        let pongs = std::mem::take(&mut state.pongs);
        if !pongs.is_empty() {
            let answered: Vec<u64> = self
                .alive
                .checks
                .tracked()
                .filter(|id| {
                    self.windows
                        .get(id)
                        .and_then(|window| window.surface.toplevel())
                        .is_some_and(|toplevel| {
                            let client = toplevel.client();
                            pongs.contains(&client)
                        })
                })
                .collect();
            for id in answered {
                self.alive.checks.pong(id);
            }
        }
        let windows = &self.windows;
        self.alive.checks.retain(|id| windows.contains_key(&id));
        for event in self.alive.checks.tick(now) {
            if let Event::Ping(id) = event {
                match self.ping_target(id) {
                    Some(toplevel) => self.ping(id, &toplevel),
                    None => self.alive.checks.cancel_ping(id),
                }
            }
        }
        self.place_close_dialogs(state);
    }

    /// The earliest alive-check timer, for the event loop's wake-up.
    pub fn alive_deadline(&self) -> Option<Instant> {
        self.alive.checks.next_deadline()
    }

    /// Whether `id` answered its last ping (`MetaWindow.is-alive`).
    pub fn is_alive(&self, id: u64) -> bool {
        self.alive.checks.is_alive(id)
    }

    /// Whether an alive check or a close dialog still names window `id`.
    pub(super) fn alive_holds(&self, id: u64) -> bool {
        self.alive.checks.tracked().any(|tracked| tracked == id)
            || self.alive.placed.contains_key(&id)
    }

    /// `ShowCloseDialog` / `HideCloseDialog` messages for the shell.
    pub fn take_close_dialogs(&mut self) -> Vec<Message> {
        std::mem::take(&mut self.alive.outbox)
    }

    /// The shell's answer for `id`'s dialog (`close_dialog_response_cb`):
    /// Wait re-arms it, Force Quit kills the client (`meta_window_kill`).
    /// False when no dialog was shown for `id`.
    pub fn close_dialog_response(&mut self, id: u64, response: CloseDialogResponse) -> bool {
        let outcome = self.alive.checks.respond(id, response, Instant::now());
        if outcome == Response::Ignored {
            return false;
        }
        // The answering shell hid it already; any other one hides too.
        if let Some(Some(_)) = self.alive.placed.remove(&id) {
            self.alive
                .outbox
                .push(Message::HideCloseDialog { window: id });
        }
        if outcome == Response::ForceClose {
            self.kill_window(id);
        }
        true
    }

    /// `meta_window_kill`: SIGKILL to the client's process, else (no pid,
    /// our own process, the shell, or kill failed) the protocol error
    /// `meta_window_wayland_kill` posts on the window's surface.
    fn kill_window(&self, id: u64) {
        let Some(toplevel) = self
            .windows
            .get(&id)
            .and_then(|window| window.surface.toplevel())
        else {
            return;
        };
        let surface = toplevel.wl_surface();
        let pid = unresponsive::kill_target(
            self.alive.client_pid(surface),
            std::process::id(),
            self.alive.shell_pid,
        );
        if let Some(pid) = pid.and_then(rustix::process::Pid::from_raw) {
            if rustix::process::kill_process(pid, rustix::process::Signal::KILL).is_ok() {
                eprintln!("tuna-compositor: window {id} not responding; killed its client");
                return;
            }
        }
        surface.post_error(
            unresponsive::WL_DISPLAY_ERROR_NO_MEMORY,
            unresponsive::KILL_MESSAGE,
        );
        eprintln!("tuna-compositor: window {id} not responding; disconnected its client");
    }

    /// Where `id`'s dialog belongs: over its frame on the output it
    /// overlaps most, or nowhere while the window is off screen.
    fn close_dialog_placement(&self, state: &State, id: u64) -> Option<Placement> {
        if self.overview_open {
            return None;
        }
        let (_, _, geometry) = self
            .visible_entries()
            .into_iter()
            .find(|(visible, _, _)| *visible == id)?;
        let frame = self.shifted(id, geometry);
        if frame.size.w <= 0 || frame.size.h <= 0 {
            return None;
        }
        let output = state.output_slices().into_iter().max_by_key(|(_, area)| {
            area.intersection(frame)
                .map_or(0_i64, |i| i64::from(i.size.w) * i64::from(i.size.h))
        });
        let (connector, origin) = match output {
            Some((name, area)) => (Some(name), area.loc),
            None => (None, (0, 0).into()),
        };
        let side = |n: i32| n.clamp(1, MAX_CLOSE_DIALOG_SIDE);
        Some(Placement {
            app_id: self.model.window(id).and_then(|entry| entry.app_id.clone()),
            connector,
            frame: Rectangle::new(
                (frame.loc.x - origin.x, frame.loc.y - origin.y).into(),
                (side(frame.size.w), side(frame.size.h)).into(),
            ),
        })
    }

    /// Bring the shell's dialogs in line with the shown ones: show new
    /// ones, move ones whose window moved, hide ones whose window left
    /// the screen, answered, went away, or the lock took.
    fn place_close_dialogs(&mut self, state: &State) {
        let shown: Vec<u64> = self.alive.checks.shown().collect();
        let gone: Vec<u64> = self
            .alive
            .placed
            .keys()
            .copied()
            .filter(|id| !shown.contains(id))
            .collect();
        for id in gone {
            if let Some(Some(_)) = self.alive.placed.remove(&id) {
                self.alive
                    .outbox
                    .push(Message::HideCloseDialog { window: id });
            }
        }
        for id in shown {
            let placement = self.close_dialog_placement(state, id);
            let previous = self.alive.placed.get(&id);
            if previous == Some(&placement) {
                continue;
            }
            match &placement {
                Some(placement) => self.alive.outbox.push(Message::ShowCloseDialog {
                    window: id,
                    app_id: placement.app_id.clone(),
                    connector: placement.connector.clone(),
                    x: placement.frame.loc.x,
                    y: placement.frame.loc.y,
                    width: placement.frame.size.w,
                    height: placement.frame.size.h,
                }),
                None => {
                    if previous.is_some_and(Option::is_some) {
                        self.alive
                            .outbox
                            .push(Message::HideCloseDialog { window: id });
                    }
                }
            }
            self.alive.placed.insert(id, placement);
        }
    }
}
