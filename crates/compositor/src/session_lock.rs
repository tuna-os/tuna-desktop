//! ext-session-lock-v1: the shell's lock screen (GNOME 51's curtain and
//! unlock prompt) as lock surfaces. Adapted from niri's handler
//! (GPL-3.0-or-later, like Tuna Desktop).
//!
//! Authority stays with the compositor (ADR: the lock flag is
//! compositor-owned). A lock request is considered only from the
//! supervised shell's pidfd-pinned connection; any other client's is
//! refused on arrival, so it can neither replace nor cancel the shell's.
//! The runtime then grants it only while that pinned process is still the
//! held shell child. A client's unlock never unlocks by itself: the
//! session unlocks only once the compositor has verified the password
//! through PAM and cleared its flag. Until then the lock surfaces are all
//! that is drawn and all that takes input.

use smithay::reexports::wayland_protocols::ext::session_lock::v1::server::{
    ext_session_lock_manager_v1::ExtSessionLockManagerV1,
    ext_session_lock_surface_v1::ExtSessionLockSurfaceV1,
    ext_session_lock_v1::{Error, ExtSessionLockV1, Request},
};
use smithay::reexports::wayland_server::protocol::{wl_output::WlOutput, wl_surface::WlSurface};
use smithay::reexports::wayland_server::{
    backend::ClientId, Client, DataInit, Dispatch, DisplayHandle, Resource,
};
use smithay::wayland::session_lock::{
    ExtLockSurfaceUserData, LockSurface, SessionLockHandler, SessionLockManagerGlobalData,
    SessionLockManagerState, SessionLockState, SessionLocker,
};

use crate::State;

/// The runtime's decision on an ext-session-lock request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockVerdict {
    Approved,
    /// The supervised shell asked, but administrative lockdown (#357)
    /// disabled the lock screen: no Wayland bypass of the policy.
    RefusedByPolicy,
    /// Not the supervised shell (or it is no longer the held child).
    RefusedForeign,
}

/// Only the supervised shell may lock: `requester_pid` is the live
/// `SO_PEERPIDFD`-pinned PID of a connection flagged as the shell's at
/// accept ([`crate::ClientState::pinned_shell_pid`]), and it must still be
/// the shell child the runtime holds. An unknown pid on either side is
/// never a match.
pub fn lock_request_verdict(
    requester_pid: Option<u32>,
    shell_pid: Option<u32>,
    lock_screen_allowed: bool,
) -> LockVerdict {
    let ours = matches!(
        (requester_pid, shell_pid),
        (Some(pid), Some(shell)) if pid == shell
    );
    match (ours, lock_screen_allowed) {
        (true, true) => LockVerdict::Approved,
        (true, false) => LockVerdict::RefusedByPolicy,
        (false, _) => LockVerdict::RefusedForeign,
    }
}

/// Session-lock protocol state the handler and the runtime share.
pub(crate) struct LockProtocol {
    pub(crate) state: SessionLockManagerState,
    /// The supervised shell's lock request awaiting the runtime's
    /// decision. Never another client's: those are refused on arrival.
    pub(crate) pending: Option<SessionLocker>,
    /// Only this runtime-approved client may supply visible lock surfaces.
    pub(crate) owner: Option<ClientId>,
    /// Only the approved lock resource may request release.
    approved_lock: Option<ExtSessionLockV1>,
    /// Lock surfaces by output name.
    pub(crate) surfaces: Vec<(LockSurface, String)>,
    /// The lock client asked to unlock (unlock_and_destroy).
    pub(crate) client_unlocked: bool,
}

impl LockProtocol {
    pub(crate) fn new(dh: &smithay::reexports::wayland_server::DisplayHandle) -> Self {
        Self {
            // Only the shell's lock is ever approved; a client inside a
            // wp_security_context_v1 does not even see the global (#1046).
            state: SessionLockManagerState::new::<State, _>(dh, |client| {
                !crate::ClientState::sandboxed(client)
            }),
            pending: None,
            owner: None,
            approved_lock: None,
            surfaces: Vec::new(),
            client_unlocked: false,
        }
    }
}

/// The client behind a lock request.
fn locker_client(locker: &SessionLocker) -> Option<ClientId> {
    locker.ext_session_lock().client().map(|c| c.id())
}

impl SessionLockHandler for State {
    fn lock_state(&mut self) -> &mut SessionLockManagerState {
        &mut self.lock_protocol.state
    }

    fn lock(&mut self, confirmation: SessionLocker) {
        if confirmation
            .ext_session_lock()
            .client()
            .as_ref()
            .and_then(crate::ClientState::pinned_shell_pid)
            .is_none()
        {
            // Refused on arrival: dropping the locker sends `finished`,
            // and the shell's pending request, the approved owner and
            // their surfaces are untouched. So no other client's request
            // can cancel the shell's before the runtime judges it.
            return;
        }
        // A newer request from the shell replaces its older one (dropping
        // the old locker tells it that lock failed), and with it any
        // abandoned pending surfaces, but never an approved owner's.
        self.lock_protocol.pending = Some(confirmation);
        self.retain_claimed_surfaces();
    }

    fn unlock(&mut self) {
        // This is protocol-resource cleanup, not the security transition.
        // Runtime::release_lock has already quarantined physical holds and
        // restored the session keyboard owner before permitting this request.
        self.lock_protocol.client_unlocked = true;
        self.lock_protocol.owner = None;
        self.lock_protocol.approved_lock = None;
        self.lock_protocol.surfaces.clear();
    }

    fn new_surface(&mut self, surface: LockSurface, output: WlOutput) {
        let Some(client) = surface.wl_surface().client().map(|c| c.id()) else {
            return;
        };
        let pending = self
            .lock_protocol
            .pending
            .as_ref()
            .is_some_and(|locker| locker_client(locker).as_ref() == Some(&client));
        // Protocol clients may create surfaces before Locked. Keep those
        // pending surfaces, but expose them only after runtime approval.
        if self.lock_protocol.owner.as_ref() != Some(&client) && !pending {
            return;
        }
        let Some(output) = smithay::output::Output::from_resource(&output) else {
            return;
        };
        let name = output.name();
        let size = self
            .output_entries()
            .into_iter()
            .find(|(entry, ..)| *entry == name)
            .and_then(|(_, out, ..)| out.current_mode().map(|m| (m, out.current_scale())))
            .map(|(mode, scale)| {
                let s = scale.fractional_scale();
                (
                    (f64::from(mode.size.w) / s).round() as u32,
                    (f64::from(mode.size.h) / s).round() as u32,
                )
            })
            .unwrap_or((1280, 800));
        surface.with_pending_state(|state| state.size = Some(size.into()));
        surface.send_configure();
        self.lock_protocol
            .surfaces
            .retain(|(existing_surface, existing)| {
                *existing != name
                    || existing_surface
                        .wl_surface()
                        .client()
                        .map(|c| c.id())
                        .as_ref()
                        != Some(&client)
            });
        self.lock_protocol.surfaces.push((surface, name));
    }
}

// Preserve Smithay's manager/surface handling, but gate release before its
// handler: a protocol error must not invoke the compositor's unlock callback.
smithay::reexports::wayland_server::delegate_global_dispatch!(State: [ExtSessionLockManagerV1: SessionLockManagerGlobalData] => SessionLockManagerState);
smithay::reexports::wayland_server::delegate_dispatch!(State: [ExtSessionLockManagerV1: ()] => SessionLockManagerState);
smithay::reexports::wayland_server::delegate_dispatch!(State: [ExtSessionLockSurfaceV1: ExtLockSurfaceUserData] => SessionLockManagerState);

impl Dispatch<ExtSessionLockV1, SessionLockState> for State {
    fn request(
        state: &mut Self,
        client: &Client,
        lock: &ExtSessionLockV1,
        request: Request,
        data: &SessionLockState,
        dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if matches!(request, Request::UnlockAndDestroy)
            && state.lock_protocol.approved_lock.as_ref() != Some(lock)
        {
            lock.post_error(Error::InvalidUnlock, "Lock resource was not approved");
            return;
        }
        <SessionLockManagerState as Dispatch<ExtSessionLockV1, SessionLockState, Self>>::request(
            state, client, lock, request, data, dh, data_init,
        );
    }
}

impl State {
    /// The shell's lock request the runtime has not yet decided on, with
    /// the live pinned pid of its connection, read now: `None` once the
    /// process that opened the connection has exited.
    pub fn take_lock_request(&mut self) -> Option<(SessionLocker, Option<u32>)> {
        let locker = self.lock_protocol.pending.take()?;
        let pid = locker
            .ext_session_lock()
            .client()
            .as_ref()
            .and_then(crate::ClientState::pinned_shell_pid);
        Some((locker, pid))
    }

    /// Drop the lock surfaces nobody may still show: those of a client
    /// that is neither the approved owner nor waiting on a lock request.
    fn retain_claimed_surfaces(&mut self) {
        let lock = &mut self.lock_protocol;
        let pending = lock.pending.as_ref().and_then(locker_client);
        let owner = &lock.owner;
        lock.surfaces.retain(|(surface, _)| {
            let client = surface.wl_surface().client().map(|c| c.id());
            client.is_some() && (client == *owner || client == pending)
        });
    }

    /// Resolve a request after the runtime has checked the supervised PID.
    /// Denial cannot replace the current owner's surfaces or focus targets.
    pub fn resolve_lock_request(&mut self, locker: SessionLocker, approved: bool) {
        if approved {
            self.lock_protocol.approved_lock = Some(locker.ext_session_lock().clone());
            self.lock_protocol.owner = locker_client(&locker);
            self.lock_protocol.client_unlocked = false;
        }
        self.retain_claimed_surfaces();
        if approved {
            locker.lock();
        }
    }

    fn approved_lock_surface(&self, surface: &LockSurface) -> bool {
        surface.alive()
            && surface
                .wl_surface()
                .client()
                .map(|c| c.id())
                .as_ref()
                .is_some_and(|id| Some(id) == self.lock_protocol.owner.as_ref())
    }

    /// The lock surface for `output`, if the approved client made one.
    pub fn lock_surface_for(&self, output: &str) -> Option<WlSurface> {
        self.lock_protocol
            .surfaces
            .iter()
            .find(|(surface, name)| name == output && self.approved_lock_surface(surface))
            .map(|(surface, _)| surface.wl_surface().clone())
    }

    /// Every live lock surface.
    pub fn lock_surfaces(&self) -> Vec<WlSurface> {
        self.lock_protocol
            .surfaces
            .iter()
            .filter(|(s, _)| self.approved_lock_surface(s))
            .map(|(s, _)| s.wl_surface().clone())
            .collect()
    }

    /// Whether any lock surface is up.
    pub fn has_lock_surfaces(&self) -> bool {
        self.lock_protocol
            .surfaces
            .iter()
            .any(|(s, _)| self.approved_lock_surface(s))
    }

    /// Whether the lock client asked to unlock since the last check.
    pub fn take_client_unlock(&mut self) -> bool {
        std::mem::take(&mut self.lock_protocol.client_unlocked)
    }
}

#[cfg(test)]
mod tests {
    use super::{lock_request_verdict, LockVerdict};

    #[test]
    fn only_the_supervised_shell_pid_may_lock() {
        assert_eq!(
            lock_request_verdict(Some(4242), Some(4242), true),
            LockVerdict::Approved
        );
        assert_eq!(
            lock_request_verdict(Some(4243), Some(4242), true),
            LockVerdict::RefusedForeign,
            "any other client is refused"
        );
    }

    #[test]
    fn an_unknown_pid_is_never_the_shell() {
        // No live pinned shell connection, or no supervised shell running
        // (crashed, being restarted): nobody may lock.
        assert_eq!(
            lock_request_verdict(None, Some(4242), true),
            LockVerdict::RefusedForeign
        );
        assert_eq!(
            lock_request_verdict(Some(4242), None, true),
            LockVerdict::RefusedForeign
        );
        assert_eq!(
            lock_request_verdict(None, None, true),
            LockVerdict::RefusedForeign
        );
    }

    #[test]
    fn lockdown_policy_refuses_even_the_shell_and_reports_why() {
        assert_eq!(
            lock_request_verdict(Some(4242), Some(4242), false),
            LockVerdict::RefusedByPolicy
        );
        assert_eq!(
            lock_request_verdict(Some(1), Some(4242), false),
            LockVerdict::RefusedForeign,
            "a foreign client is refused as foreign, not by policy"
        );
    }
}
