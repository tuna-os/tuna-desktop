//! Revisioned compositor-owned state model (001 R4, ADR 0002).
//!
//! Plain Rust types only (`u64` revisions/ids, `String` titles): this module
//! intentionally does NOT depend on the `tuna-shell-control` crate (owned by
//! another stream). Field names and shapes are kept mechanical so the later
//! mapping to the wire schema is a direct rename.
//!
//! Model:
//! - [`WindowEntry`]: one mapped window. `id` is generational (monotonic
//!   counter, never reused within a session, even after removal).
//!   `title` is untrusted client input and is length-capped on insert/update.
//! - [`StateModel`]: revision counter, window map, workspace list, focused
//!   window, plus a bounded change log for incremental sync.
//! - Every successful mutation bumps `revision` by exactly one and appends at
//!   least one [`StateChange`] tagged with the new revision.
//! - Shells resync via [`Snapshot`] (full copy + revision) or
//!   [`StateModel::changes_since`]; a [`RevisionGap`] signals the shell must
//!   take a fresh snapshot (ADR 0002: snapshot-on-reconnect, gaps resnapshot).

use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// Maximum retained change-log entries; older entries are pruned.
///
/// A shell holding a revision older than the pruned window gets a
/// [`RevisionGap`] from [`StateModel::changes_since`] and must resnapshot.
pub const MAX_CHANGE_LOG: usize = 1024;

/// Maximum window title size in bytes. Titles are untrusted client input;
/// longer titles are truncated to the longest valid UTF-8 prefix that fits.
pub const MAX_TITLE_LEN: usize = 256;

/// One mapped window as tracked by the compositor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowEntry {
    pub icon: Option<String>,
    /// Generational id: drawn from a session-monotonic counter, never reused.
    pub id: u64,
    /// Untrusted client-provided title, length-capped on insert/update.
    pub title: String,
    /// Client-supplied application id, if known. Activation tokens bind
    /// to this value (ADR 0002).
    pub app_id: Option<String>,
    /// Workspace this window belongs to.
    pub workspace: u32,
    /// Compositor-controlled active hint (e.g. on the active workspace).
    pub active: bool,
    /// Keyboard-focus flag; mirrors [`StateModel::focused`].
    pub focused: bool,
    pub on_all_workspaces: bool,
}

/// A single state transition, tagged with the post-mutation revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateChange {
    /// A window was inserted (carries the full entry, id included).
    WindowInserted { window: WindowEntry },
    /// A window was removed (id only; entry is gone).
    WindowRemoved { id: u64 },
    /// A window's title/workspace/flags changed (carries the new entry).
    WindowUpdated { window: WindowEntry },
    /// Keyboard focus moved (None = no window focused).
    FocusChanged { focused: Option<u64> },
    /// Active workspace switched; empty non-active workspaces were
    /// pruned from the list at the same revision.
    ActiveWorkspaceChanged { previous: u32, active: u32 },
    /// Workspace topology changed; incremental peers must take a snapshot.
    WorkspacesChanged,
}

/// Full copy of the model at one revision, for snapshot-on-reconnect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// Revision this snapshot was taken at.
    pub revision: u64,
    /// All windows, ordered by id.
    pub windows: Vec<WindowEntry>,
    /// Known workspace ids, sorted and deduplicated.
    pub workspaces: Vec<u32>,
    /// Active workspace id.
    pub active: u32,
    /// Focused window id, if any.
    pub focused: Option<u64>,
}

/// Gap signal: the requested revision is unknown (too old and pruned, or
/// from the future). The shell must take a fresh [`Snapshot`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RevisionGap {
    /// The model's current revision; what a fresh snapshot would carry.
    pub expected: u64,
    /// The revision the caller asked about.
    pub got: u64,
}

/// Partial update applied by [`StateModel::update`]; `None` fields are kept.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WindowUpdate {
    /// Replace the title (re-capped as on insert).
    pub title: Option<String>,
    /// Replace the application id (`Some(None)` clears it).
    pub app_id: Option<Option<String>>,
    /// Move the window to another workspace (auto-registered if new).
    pub workspace: Option<u32>,
    /// Replace the active hint.
    pub active: Option<bool>,
    pub on_all_workspaces: Option<bool>,
}

/// Compositor-owned revisioned window/workspace/focus model.
#[derive(Debug)]
pub struct StateModel {
    revision: u64,
    windows: BTreeMap<u64, WindowEntry>,
    workspaces: Vec<u32>,
    fixed_workspace_count: Option<u32>,
    active: u32,
    focused: Option<u64>,
    next_id: u64,
    changes: VecDeque<(u64, StateChange)>,
}

impl Default for StateModel {
    /// Empty model: workspace 0 registered and active, no windows.
    fn default() -> Self {
        Self {
            revision: 0,
            windows: BTreeMap::new(),
            workspaces: vec![0],
            fixed_workspace_count: None,
            active: 0,
            focused: None,
            next_id: 1,
            changes: VecDeque::new(),
        }
    }
}

impl StateModel {
    /// Empty model at revision 0. Ids start at 1 (0 is never issued).
    pub fn new() -> Self {
        Self::default()
    }

    /// Current revision; bumped by exactly one per successful mutation.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Window lookup by generational id.
    pub fn window(&self, id: u64) -> Option<&WindowEntry> {
        self.windows.get(&id)
    }

    /// All windows ordered by id.
    pub fn windows(&self) -> impl Iterator<Item = &WindowEntry> {
        self.windows.values()
    }

    /// Known workspace ids, sorted and deduplicated.
    pub fn workspaces(&self) -> &[u32] {
        &self.workspaces
    }

    /// Fixed workspace count, or None for dynamic workspaces.
    pub fn fixed_workspace_count(&self) -> Option<u32> {
        self.fixed_workspace_count
    }

    pub fn workspace_allowed(&self, workspace: u32) -> bool {
        self.fixed_workspace_count
            .is_none_or(|count| workspace < count)
    }

    /// GNOME's before-redraw dynamic workspace compaction. `keep_alive`
    /// includes live startup sequences and short-lived dialog grace slots.
    /// Preserve the active workspace and the empty slot after the last occupied
    /// one, remove other empty slots from the end, and keep at least two.
    pub fn reconcile_dynamic_workspaces(&mut self, keep_alive: &[u32]) -> bool {
        self.reconcile_dynamic_workspaces_with_remap(keep_alive)
            .is_some()
    }

    /// The retained old-to-new indices let the runtime move workspace leases
    /// together with the workspace object they protect.
    pub fn reconcile_dynamic_workspaces_with_remap(
        &mut self,
        keep_alive: &[u32],
    ) -> Option<Vec<(u32, u32)>> {
        if self.fixed_workspace_count.is_some() {
            return None;
        }
        let occupied: BTreeSet<u64> = self
            .windows
            .values()
            .filter(|window| !window.on_all_workspaces)
            .map(|window| u64::from(window.workspace))
            .chain(keep_alive.iter().map(|workspace| u64::from(*workspace)))
            .collect();
        let mut slots: Vec<u64> = self
            .workspaces
            .iter()
            .copied()
            .map(u64::from)
            .chain(occupied.iter().copied())
            .chain([u64::from(self.active)])
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if slots.last().is_some_and(|last| occupied.contains(last)) {
            slots.push(slots.last().copied().unwrap_or(0) + 1);
        }
        while slots.len() < 2 {
            slots.push(slots.last().copied().unwrap_or(0) + 1);
        }
        let trailing = occupied
            .iter()
            .next_back()
            .and_then(|last| slots.iter().position(|slot| slot == last))
            .map_or(0, |index| index + 1);
        let protected_empty = slots[trailing];
        for index in (0..slots.len()).rev() {
            if slots.len() == 2 {
                break;
            }
            let slot = slots[index];
            if slot != u64::from(self.active)
                && slot != protected_empty
                && !occupied.contains(&slot)
            {
                slots.remove(index);
            }
        }
        let new_active = slots
            .iter()
            .position(|slot| *slot == u64::from(self.active))
            .unwrap() as u32;
        let moves: Vec<_> = self
            .windows
            .values()
            .filter_map(|window| {
                let workspace = slots
                    .iter()
                    .position(|slot| *slot == u64::from(window.workspace))
                    .map_or(new_active, |index| index as u32);
                (workspace != window.workspace).then_some((window.id, workspace))
            })
            .collect();
        let workspaces: Vec<u32> = (0..slots.len() as u32).collect();
        if moves.is_empty() && self.active == new_active && self.workspaces == workspaces {
            return None;
        }
        let remap = slots
            .iter()
            .enumerate()
            .filter_map(|(index, old)| u32::try_from(*old).ok().map(|old| (old, index as u32)))
            .collect();
        for (id, workspace) in moves {
            self.update(
                id,
                WindowUpdate {
                    workspace: Some(workspace),
                    ..Default::default()
                },
            );
        }
        if self.active != new_active {
            let previous = self.active;
            self.active = new_active;
            self.commit(StateChange::ActiveWorkspaceChanged {
                previous,
                active: new_active,
            });
        }
        self.workspaces = workspaces;
        self.commit(StateChange::WorkspacesChanged);
        Some(remap)
    }

    /// Apply a live fixed-count preference. Windows from removed workspaces
    /// move to the last remaining one; IDs and window contents are preserved.
    pub fn set_fixed_workspace_count(&mut self, count: Option<u32>) -> bool {
        let count = count.map(|count| count.clamp(1, 36));
        if self.fixed_workspace_count == count {
            return false;
        }
        self.fixed_workspace_count = count;
        if let Some(count) = count {
            let last = count - 1;
            let moved: Vec<_> = self
                .windows
                .values()
                .filter(|window| window.workspace > last)
                .map(|window| window.id)
                .collect();
            for id in moved {
                self.update(
                    id,
                    WindowUpdate {
                        workspace: Some(last),
                        ..Default::default()
                    },
                );
            }
            if self.active > last {
                self.set_active_workspace(last);
            }
            self.workspaces = (0..count).collect();
        } else {
            self.prune_workspaces();
        }
        self.commit(StateChange::WorkspacesChanged);
        true
    }

    /// Focused window id, if any.
    pub fn focused(&self) -> Option<u64> {
        self.focused
    }

    /// Active workspace id (defaults to 0; always registered).
    pub fn active_workspace(&self) -> u32 {
        self.active
    }

    /// Switch the active workspace, registering it if new and pruning
    /// empty non-active workspaces at the same revision. Idempotent:
    /// re-selecting the active workspace returns true without a bump.
    /// Appends `ActiveWorkspaceChanged`.
    pub fn set_active_workspace(&mut self, workspace: u32) -> bool {
        if !self.workspace_allowed(workspace) {
            return false;
        }
        if self.active == workspace {
            return true;
        }
        let previous = self.active;
        self.active = workspace;
        self.register_workspace(workspace);
        self.prune_workspaces();
        self.commit(StateChange::ActiveWorkspaceChanged {
            previous,
            active: workspace,
        });
        true
    }

    /// Drop workspace ids with no windows except the active one. The
    /// list converges through snapshots; removals carry no delta op.
    fn prune_workspaces(&mut self) {
        if let Some(count) = self.fixed_workspace_count {
            self.workspaces = (0..count).collect();
            return;
        }
        self.workspaces
            .retain(|id| *id == self.active || self.windows.values().any(|w| w.workspace == *id));
    }

    /// Number of retained change-log entries (bounded by [`MAX_CHANGE_LOG`]).
    pub fn change_log_len(&self) -> usize {
        self.changes.len()
    }

    /// Insert a window, returning its generational id.
    ///
    /// The title is capped to [`MAX_TITLE_LEN`] bytes first; the workspace
    /// is registered in the workspace list if new. Bumps the revision and
    /// appends `WindowInserted`.
    pub fn insert(&mut self, title: &str, app_id: Option<&str>, workspace: u32) -> u64 {
        let workspace = self
            .fixed_workspace_count
            .map_or(workspace, |count| workspace.min(count - 1));
        let id = self.next_id();
        self.register_workspace(workspace);
        let window = WindowEntry {
            on_all_workspaces: false,
            icon: None,
            id,
            title: cap_title(title),
            app_id: app_id.map(str::to_owned),
            workspace,
            active: false,
            focused: Some(id) == self.focused,
        };
        self.windows.insert(id, window.clone());
        self.commit(StateChange::WindowInserted { window });
        id
    }

    /// Remove a window. Returns false (no revision bump, no change) when the
    /// id is unknown. Clearing the focused window also clears focus and
    /// appends `FocusChanged` under the same revision.
    pub fn remove(&mut self, id: u64) -> bool {
        if self.windows.remove(&id).is_none() {
            return false;
        }
        self.prune_workspaces();
        self.commit(StateChange::WindowRemoved { id });
        if self.focused == Some(id) {
            self.focused = None;
            for window in self.windows.values_mut() {
                window.focused = false;
            }
            let revision = self.revision;
            self.push_change(revision, StateChange::FocusChanged { focused: None });
        }
        true
    }

    /// Apply a partial [`WindowUpdate`]. Returns false (no bump, no change)
    /// when the id is unknown. A workspace move registers the workspace if
    /// new. Appends `WindowUpdated`.
    pub fn update(&mut self, id: u64, patch: WindowUpdate) -> bool {
        if patch
            .workspace
            .is_some_and(|workspace| !self.workspace_allowed(workspace))
        {
            return false;
        }
        let Some(window) = self.windows.get_mut(&id) else {
            return false;
        };
        if let Some(title) = patch.title {
            window.title = cap_title(&title);
        }
        if let Some(app_id) = patch.app_id {
            window.app_id = app_id;
        }
        if let Some(workspace) = patch.workspace {
            window.workspace = workspace;
        }
        if let Some(active) = patch.active {
            window.active = active;
        }
        if let Some(on_all_workspaces) = patch.on_all_workspaces {
            window.on_all_workspaces = on_all_workspaces;
        }
        let window = window.clone();
        if let Some(workspace) = patch.workspace {
            self.register_workspace(workspace);
        }
        self.windows.insert(id, window.clone());
        self.commit(StateChange::WindowUpdated { window });
        true
    }

    /// Insert a workspace before `at`, shifting later windows and the active
    /// workspace, then place the dragged window in the new slot.
    pub fn insert_workspace_and_move(&mut self, id: u64, at: u32) -> bool {
        if self.fixed_workspace_count.is_some()
            || !self.windows.contains_key(&id)
            || self.workspaces.iter().any(|&w| w >= at && w == u32::MAX)
        {
            return false;
        }
        let shifted: Vec<_> = self
            .windows
            .values()
            .filter(|w| w.id != id && w.workspace >= at)
            .map(|w| (w.id, w.workspace + 1))
            .collect();
        for workspace in &mut self.workspaces {
            if *workspace >= at {
                *workspace += 1;
            }
        }
        for (id, workspace) in shifted {
            self.update(
                id,
                WindowUpdate {
                    workspace: Some(workspace),
                    ..Default::default()
                },
            );
        }
        if self.active >= at {
            let previous = self.active;
            self.active += 1;
            self.commit(StateChange::ActiveWorkspaceChanged {
                previous,
                active: self.active,
            });
        }
        self.update(
            id,
            WindowUpdate {
                workspace: Some(at),
                ..Default::default()
            },
        );
        self.prune_workspaces();
        true
    }

    /// Move keyboard focus. `None` unfocuses. Returns false (no bump, no
    /// change) when targeting an unknown window. Updates the windows'
    /// `focused` flags, bumps the revision, appends `FocusChanged`.
    pub fn set_focused(&mut self, focused: Option<u64>) -> bool {
        if let Some(id) = focused {
            if !self.windows.contains_key(&id) {
                return false;
            }
        }
        if self.focused == focused {
            return true;
        }
        self.focused = focused;
        for window in self.windows.values_mut() {
            window.focused = Some(window.id) == focused;
        }
        self.commit(StateChange::FocusChanged { focused });
        true
    }

    /// Full copy of the model at the current revision.
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            revision: self.revision,
            windows: self.windows.values().cloned().collect(),
            workspaces: self.workspaces.clone(),
            active: self.active,
            focused: self.focused,
        }
    }

    /// Changes with revision strictly greater than `rev`, oldest first.
    ///
    /// Returns [`RevisionGap`] (caller must resnapshot) when `rev` is newer
    /// than the current revision or older than the oldest retained change.
    pub fn changes_since(&self, rev: u64) -> Result<Vec<(u64, StateChange)>, RevisionGap> {
        if rev > self.revision {
            return Err(RevisionGap {
                expected: self.revision,
                got: rev,
            });
        }
        if let Some((oldest, _)) = self.changes.front() {
            if rev.saturating_add(1) < *oldest {
                return Err(RevisionGap {
                    expected: self.revision,
                    got: rev,
                });
            }
        }
        Ok(self
            .changes
            .iter()
            .filter(|(r, _)| *r > rev)
            .cloned()
            .collect())
    }

    pub fn set_icon(&mut self, id: u64, icon: Option<String>) {
        if let Some(window) = self.windows.get_mut(&id) {
            if window.icon != icon {
                window.icon = icon;
                let window = window.clone();
                self.commit(StateChange::WindowUpdated { window });
            }
        }
    }

    fn next_id(&mut self) -> u64 {
        let id = self.next_id.max(1);
        self.next_id = id.wrapping_add(1).max(1);
        id
    }

    fn register_workspace(&mut self, workspace: u32) {
        if let Err(pos) = self.workspaces.binary_search(&workspace) {
            self.workspaces.insert(pos, workspace);
        }
    }

    fn commit(&mut self, change: StateChange) {
        self.revision = self.revision.wrapping_add(1);
        self.push_change(self.revision, change);
    }

    fn push_change(&mut self, revision: u64, change: StateChange) {
        self.changes.push_back((revision, change));
        while self.changes.len() > MAX_CHANGE_LOG {
            self.changes.pop_front();
        }
    }
}

/// Truncate an untrusted title to [`MAX_TITLE_LEN`] bytes on a UTF-8
/// boundary.
fn cap_title(title: &str) -> String {
    if title.len() <= MAX_TITLE_LEN {
        return title.to_owned();
    }
    let mut end = MAX_TITLE_LEN;
    while !title.is_char_boundary(end) {
        end -= 1;
    }
    title[..end].to_owned()
}

/// Reason a token validation was denied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenyReason {
    /// `now_ms - issued_at_ms` exceeds `max_age_ms` (future-dated tokens
    /// count as expired: their age cannot be established).
    Expired,
    /// Token is bound to a different seat than the activating one.
    SeatMismatch,
    /// Token's `app_id` does not match the activating app.
    AppMismatch,
}

/// Outcome of [`TokenPolicy::validate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenDecision {
    Allow,
    Deny { reason: DenyReason },
}

/// Activation-token policy (ADR 0002: 30 s expiry, seat binding plus
/// `app_id` match required, log-and-deny on mismatch).
///
/// Pure function of its arguments: no clock reads, no store access.
/// One-use enforcement (remove the token after the first successful
/// activation) is caller-side: validate first, and on `Allow` remove the
/// token from the compositor's token table before acting on it.
pub struct TokenPolicy;

/// Stateful one-use activation-token store (ADR 0002, 001 T2).
///
/// [`TokenPolicy`] stays a pure function; this owner mints unguessable
/// tokens, binds them to a seat plus `app_id`, and removes each token on
/// its first successful presentation. Operates on plain strings so the
/// state model keeps its no-wire-dependency seam; `control.rs` adapts the
/// wire activation-token type to `&str` at the boundary.
///
/// Time comes from an injected millisecond clock (`SystemTime` in
/// production, manual in tests) so expiry stays deterministic under test.
pub struct TokenStore {
    // `RefCell` (not a lock): the store is only touched from the single
    // compositor thread, and the [`Session`](crate::control::Session)
    // validator needs shared-reference access with one-use removal.
    tokens: std::cell::RefCell<std::collections::HashMap<String, StoredToken>>,
    max_age_ms: u64,
}

/// One minted token and the presentation it authorizes. The minting
/// `purpose` is a caller-side label only; authorization binds seat,
/// `app_id`, and age.
#[derive(Debug, Clone)]
struct StoredToken {
    seat: String,
    app_id: Option<String>,
    issued_at_ms: u64,
}

impl TokenStore {
    /// Empty store with the default 30 s token lifetime.
    pub fn new() -> Self {
        Self {
            tokens: std::cell::RefCell::new(std::collections::HashMap::new()),
            max_age_ms: TokenPolicy::MAX_AGE_MS,
        }
    }

    /// Number of live (unconsumed) tokens.
    pub fn len(&self) -> usize {
        self.tokens.borrow().len()
    }

    /// Whether no live tokens are held.
    pub fn is_empty(&self) -> bool {
        self.tokens.borrow().is_empty()
    }

    /// Mint a token for `purpose` on `seat` for `app_id`, valid from
    /// `now_ms`. The token is a 128-bit random hex string.
    pub fn issue(&self, _purpose: &str, seat: &str, app_id: Option<&str>, now_ms: u64) -> String {
        let mut tokens = self.tokens.borrow_mut();
        Self::prune_in(&mut tokens, self.max_age_ms, now_ms);
        let token = format!("{:032x}", rand::random::<u128>());
        tokens.insert(
            token.clone(),
            StoredToken {
                seat: seat.to_owned(),
                app_id: app_id.map(str::to_owned),
                issued_at_ms: now_ms,
            },
        );
        token
    }

    /// Validate one presentation: unknown tokens deny as expired,
    /// mismatches deny with their reason, and an allowed token is removed
    /// (one-use) before returning.
    pub fn consume(
        &self,
        token: &str,
        seat: &str,
        app_id: Option<&str>,
        now_ms: u64,
    ) -> TokenDecision {
        let mut tokens = self.tokens.borrow_mut();
        Self::prune_in(&mut tokens, self.max_age_ms, now_ms);
        let Some(stored) = tokens.get(token) else {
            return TokenDecision::Deny {
                reason: DenyReason::Expired,
            };
        };
        let decision = TokenPolicy::validate(
            stored.issued_at_ms,
            now_ms,
            self.max_age_ms,
            stored.seat == seat,
            stored.app_id.as_deref() == app_id,
        );
        if decision == TokenDecision::Allow {
            tokens.remove(token);
        }
        decision
    }

    /// Minting closure for [`crate::control::Session`]: mints one token
    /// per window per snapshot or delta, bound to `seat` and the window's
    /// `app_id`, using the system clock. By-value `Rc` so live sessions
    /// can hold the closure without borrowing the runtime.
    pub fn minter(
        self: std::rc::Rc<Self>,
        seat: String,
    ) -> impl Fn(Option<&str>) -> String + 'static {
        move |app_id| self.issue("activate-window", &seat, app_id, system_millis())
    }

    /// Validator closure for [`crate::control::Session`]: checks one
    /// presentation against this store with the system clock, removing
    /// each allowed token (one-use). The expected `app_id` is the target
    /// window's, supplied by the caller from the model.
    pub fn validator(
        self: std::rc::Rc<Self>,
        seat: String,
    ) -> impl Fn(&str, Option<&str>) -> bool + 'static {
        move |token, app_id| {
            self.consume(token, &seat, app_id, system_millis()) == TokenDecision::Allow
        }
    }

    fn prune_in(
        tokens: &mut std::collections::HashMap<String, StoredToken>,
        max_age_ms: u64,
        now_ms: u64,
    ) {
        tokens.retain(|_, stored| now_ms.saturating_sub(stored.issued_at_ms) <= max_age_ms);
    }
}

impl Default for TokenStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Milliseconds since the Unix epoch, for token issue/expiry in live
/// sessions. Tests pass explicit timestamps instead.
pub fn system_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl TokenPolicy {
    /// Default token lifetime: 30 s (ADR 0002).
    pub const MAX_AGE_MS: u64 = 30_000;

    /// Validate a token presentation. All inputs are caller-supplied so the
    /// function stays pure and unit-testable.
    pub fn validate(
        issued_at_ms: u64,
        now_ms: u64,
        max_age_ms: u64,
        seat_ok: bool,
        app_ok: bool,
    ) -> TokenDecision {
        if !seat_ok {
            return TokenDecision::Deny {
                reason: DenyReason::SeatMismatch,
            };
        }
        if !app_ok {
            return TokenDecision::Deny {
                reason: DenyReason::AppMismatch,
            };
        }
        let age = now_ms.saturating_sub(issued_at_ms);
        let future_dated = issued_at_ms > now_ms;
        if future_dated || age > max_age_ms {
            return TokenDecision::Deny {
                reason: DenyReason::Expired,
            };
        }
        TokenDecision::Allow
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revision_monotonic_across_mutations() {
        let mut m = StateModel::new();
        assert_eq!(m.revision(), 0);
        let a = m.insert("a", None, 1);
        assert_eq!(m.revision(), 1);
        m.update(
            a,
            WindowUpdate {
                title: Some("a2".into()),
                ..Default::default()
            },
        );
        assert_eq!(m.revision(), 2);
        assert!(m.set_focused(Some(a)));
        assert_eq!(m.revision(), 3);
        assert!(m.remove(a));
        assert_eq!(m.revision(), 4);
        // Failed ops leave the revision untouched.
        assert!(!m.remove(999));
        assert!(!m.update(999, WindowUpdate::default()));
        assert!(!m.set_focused(Some(999)));
        assert_eq!(m.revision(), 4);
    }

    #[test]
    fn changes_since_returns_tail_and_detects_gaps() {
        let mut m = StateModel::new();
        let a = m.insert("a", None, 1);
        let b = m.insert("b", None, 2);
        let tail = m.changes_since(1).unwrap();
        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0].0, 2);
        assert!(matches!(
            tail[0].1,
            StateChange::WindowInserted { ref window } if window.id == b
        ));
        assert!(m.changes_since(0).unwrap().len() >= 2);
        assert!(m.changes_since(2).unwrap().is_empty());
        // Future revision is an unknown gap.
        assert_eq!(
            m.changes_since(99).unwrap_err(),
            RevisionGap {
                expected: 2,
                got: 99
            }
        );
        let _ = a;
    }

    #[test]
    fn prune_beyond_cap_forces_gap_then_resnapshot() {
        let mut m = StateModel::new();
        for i in 0..(MAX_CHANGE_LOG as u64 + 10) {
            m.insert(&format!("w{i}"), None, 1);
        }
        assert_eq!(m.change_log_len(), MAX_CHANGE_LOG);
        let oldest: u64 = m.changes.front().unwrap().0;
        // Revision just before the retained window is too old.
        assert_eq!(
            m.changes_since(oldest - 2).unwrap_err(),
            RevisionGap {
                expected: m.revision(),
                got: oldest - 2
            }
        );
        // Newest revisions still stream incrementally.
        assert!(!m.changes_since(m.revision() - 1).unwrap().is_empty());
    }

    #[test]
    fn snapshot_is_complete_and_ordered() {
        let mut m = StateModel::new();
        let b = m.insert("b", None, 2);
        let a = m.insert("a", None, 1);
        assert!(m.set_focused(Some(a)));
        let snap = m.snapshot();
        assert_eq!(snap.revision, m.revision());
        assert_eq!(snap.workspaces, vec![0, 1, 2]);
        assert_eq!(snap.active, 0);
        assert_eq!(snap.focused, Some(a));
        let ids: Vec<u64> = snap.windows.iter().map(|w| w.id).collect();
        assert_eq!(ids, vec![a.min(b), a.max(b)]);
        let by_id: BTreeMap<u64, &WindowEntry> = snap.windows.iter().map(|w| (w.id, w)).collect();
        assert_eq!(by_id[&a].title, "a");
        assert!(by_id[&a].focused);
        assert!(!by_id[&b].focused);
    }

    #[test]
    fn ids_are_generational_and_never_reused() {
        let mut m = StateModel::new();
        let a = m.insert("a", None, 1);
        let b = m.insert("b", None, 1);
        assert!(m.remove(a));
        let c = m.insert("c", None, 1);
        assert!(c != a && c != b && c > b);
        assert!(m.window(a).is_none());
        // Update/remove of the retired id stay no-ops with no revision bump.
        let rev = m.revision();
        assert!(!m.update(
            a,
            WindowUpdate {
                title: Some("ghost".into()),
                ..Default::default()
            }
        ));
        assert_eq!(m.revision(), rev);
    }

    #[test]
    fn removing_focused_window_clears_focus() {
        let mut m = StateModel::new();
        let a = m.insert("a", None, 1);
        assert!(m.set_focused(Some(a)));
        assert!(m.remove(a));
        assert_eq!(m.focused(), None);
        assert_eq!(m.snapshot().focused, None);
    }

    #[test]
    fn token_allow_and_deny_reasons() {
        assert_eq!(
            TokenPolicy::validate(1000, 5000, TokenPolicy::MAX_AGE_MS, true, true),
            TokenDecision::Allow
        );
        // Boundary: exactly max age still allows.
        assert_eq!(
            TokenPolicy::validate(
                0,
                TokenPolicy::MAX_AGE_MS,
                TokenPolicy::MAX_AGE_MS,
                true,
                true
            ),
            TokenDecision::Allow
        );
        assert_eq!(
            TokenPolicy::validate(
                0,
                TokenPolicy::MAX_AGE_MS + 1,
                TokenPolicy::MAX_AGE_MS,
                true,
                true
            ),
            TokenDecision::Deny {
                reason: DenyReason::Expired
            }
        );
        assert_eq!(
            TokenPolicy::validate(1000, 5000, TokenPolicy::MAX_AGE_MS, false, true),
            TokenDecision::Deny {
                reason: DenyReason::SeatMismatch
            }
        );
        assert_eq!(
            TokenPolicy::validate(1000, 5000, TokenPolicy::MAX_AGE_MS, true, false),
            TokenDecision::Deny {
                reason: DenyReason::AppMismatch
            }
        );
        // Future-dated token: age cannot be established, denied as expired.
        assert_eq!(
            TokenPolicy::validate(9000, 5000, TokenPolicy::MAX_AGE_MS, true, true),
            TokenDecision::Deny {
                reason: DenyReason::Expired
            }
        );
    }

    #[test]
    fn token_store_allows_fresh_token_once_then_denies_replay() {
        let store = TokenStore::new();
        assert!(store.is_empty());
        let token = store.issue("activate", "seat0", Some("app1"), 1_000);
        assert_eq!(store.len(), 1);
        assert_eq!(
            store.consume(&token, "seat0", Some("app1"), 1_000),
            TokenDecision::Allow
        );
        assert!(store.is_empty());
        // One-use: the consumed token is unknown, denied as expired.
        assert_eq!(
            store.consume(&token, "seat0", Some("app1"), 1_000),
            TokenDecision::Deny {
                reason: DenyReason::Expired
            }
        );
    }

    #[test]
    fn token_store_denies_seat_and_app_mismatch_without_consuming() {
        let store = TokenStore::new();
        let token = store.issue("activate", "seat0", Some("app1"), 1_000);
        assert_eq!(
            store.consume(&token, "other-seat", Some("app1"), 1_000),
            TokenDecision::Deny {
                reason: DenyReason::SeatMismatch
            }
        );
        assert_eq!(
            store.consume(&token, "seat0", Some("other-app"), 1_000),
            TokenDecision::Deny {
                reason: DenyReason::AppMismatch
            }
        );
        // Anonymous token presented with an app id also mismatches.
        let anon = store.issue("activate", "seat0", None, 1_000);
        assert_eq!(
            store.consume(&anon, "seat0", Some("app1"), 1_000),
            TokenDecision::Deny {
                reason: DenyReason::AppMismatch
            }
        );
        // Mismatches do not consume: both tokens still validate.
        assert_eq!(
            store.consume(&token, "seat0", Some("app1"), 1_000),
            TokenDecision::Allow
        );
        assert_eq!(
            store.consume(&anon, "seat0", None, 1_000),
            TokenDecision::Allow
        );
    }

    #[test]
    fn token_store_denies_expired_and_unknown_tokens() {
        let store = TokenStore::new();
        let token = store.issue("activate", "seat0", Some("app1"), 0);
        assert_eq!(
            store.consume(&token, "seat0", Some("app1"), TokenPolicy::MAX_AGE_MS + 1),
            TokenDecision::Deny {
                reason: DenyReason::Expired
            }
        );
        assert_eq!(
            store.consume("never-issued", "seat0", Some("app1"), 1_000),
            TokenDecision::Deny {
                reason: DenyReason::Expired
            }
        );
    }

    #[test]
    fn token_store_validator_closure_allows_once_then_denies() {
        use std::rc::Rc;
        let store = Rc::new(TokenStore::new());
        // Live closures use the system clock, so issue fresh.
        let token = store.issue("activate", "seat0", Some("app1"), system_millis());
        let validate = store.clone().validator("seat0".to_owned());
        assert!(validate(&token, Some("app1")));
        assert!(!validate(&token, Some("app1")));
        // Wrong app is denied even with a fresh token.
        let other = store.issue("activate", "seat0", Some("app1"), system_millis());
        assert!(!validate(&other, Some("app2")));
    }

    #[test]
    fn title_capped_on_insert_and_update() {
        let mut m = StateModel::new();
        let long = "x".repeat(MAX_TITLE_LEN + 100);
        let id = m.insert(&long, None, 1);
        assert_eq!(m.window(id).unwrap().title.len(), MAX_TITLE_LEN);
        let exact = "y".repeat(MAX_TITLE_LEN);
        assert!(m.update(
            id,
            WindowUpdate {
                title: Some(exact.clone()),
                ..Default::default()
            }
        ));
        assert_eq!(m.window(id).unwrap().title, exact);
        // Multibyte: truncation lands on a char boundary.
        let emoji = "é".repeat(MAX_TITLE_LEN);
        let id2 = m.insert(&emoji, None, 1);
        let title = m.window(id2).unwrap().title.clone();
        assert!(title.len() <= MAX_TITLE_LEN);
        assert!(emoji.starts_with(&title));
    }

    #[test]
    fn active_workspace_defaults_to_zero_and_is_registered() {
        let m = StateModel::new();
        assert_eq!(m.active_workspace(), 0);
        assert_eq!(m.workspaces(), &[0]);
        assert_eq!(m.snapshot().active, 0);
    }

    #[test]
    fn set_active_registers_prunes_and_logs() {
        let mut m = StateModel::new();
        let a = m.insert("a", None, 0);
        let b = m.insert("b", None, 5);
        // Idempotent reselect: no bump, no change.
        let rev = m.revision();
        assert!(m.set_active_workspace(0));
        assert_eq!(m.revision(), rev);
        // Switch registers nothing new here (5 known) and logs one change.
        assert!(m.set_active_workspace(5));
        assert_eq!(m.active_workspace(), 5);
        assert_eq!(m.workspaces(), &[0, 5]);
        let tail = m.changes_since(rev).unwrap();
        assert!(matches!(
            tail.last().unwrap().1,
            StateChange::ActiveWorkspaceChanged {
                previous: 0,
                active: 5
            }
        ));
        // Switching away prunes the emptied workspace 0... except it
        // still holds window a. Move a over, then 0 drops on next switch.
        assert!(m.update(
            a,
            WindowUpdate {
                workspace: Some(5),
                ..Default::default()
            }
        ));
        assert!(m.set_active_workspace(0));
        assert_eq!(m.workspaces(), &[0, 5]);
        assert!(m.set_active_workspace(5));
        assert_eq!(m.workspaces(), &[5]);
        let _ = b;
    }

    #[test]
    fn remove_prunes_emptied_workspaces() {
        let mut m = StateModel::new();
        let a = m.insert("a", None, 3);
        assert_eq!(m.workspaces(), &[0, 3]);
        assert!(m.remove(a));
        assert_eq!(m.workspaces(), &[0]);
    }
}

#[cfg(test)]
mod fixed_workspace_tests {
    use super::*;

    #[test]
    fn fixed_workspaces_keep_empty_slots_and_reject_out_of_range_commands() {
        let mut model = StateModel::new();
        assert!(model.set_fixed_workspace_count(Some(4)));
        assert_eq!(model.workspaces(), &[0, 1, 2, 3]);
        assert!(model.set_active_workspace(3));
        let revision = model.revision();
        assert!(!model.set_active_workspace(4));
        assert_eq!(model.revision(), revision);
        assert!(model.set_active_workspace(0));
        assert_eq!(model.workspaces(), &[0, 1, 2, 3]);
    }

    #[test]
    fn shrinking_fixed_workspaces_preserves_window_ids_titles_and_focus() {
        let mut model = StateModel::new();
        model.set_fixed_workspace_count(Some(6));
        let first = model.insert("First", Some("org.tuna.First"), 5);
        let second = model.insert("Second", Some("org.tuna.Second"), 4);
        model.set_active_workspace(5);
        model.set_focused(Some(first));
        assert!(model.set_fixed_workspace_count(Some(2)));
        assert_eq!(model.active_workspace(), 1);
        assert_eq!(model.workspaces(), &[0, 1]);
        assert_eq!(model.focused(), Some(first));
        assert_eq!(model.window(first).unwrap().title, "First");
        assert_eq!(model.window(second).unwrap().title, "Second");
        assert!(model.windows().all(|window| window.workspace == 1));
        assert!(matches!(
            model.changes.back().unwrap().1,
            StateChange::WorkspacesChanged
        ));
    }

    #[test]
    fn fixed_workspace_moves_cannot_expand_topology_or_partially_edit_a_window() {
        let mut model = StateModel::new();
        model.set_fixed_workspace_count(Some(2));
        let window = model.insert("Original", None, 0);
        let revision = model.revision();
        assert!(!model.update(
            window,
            WindowUpdate {
                title: Some("Changed".into()),
                workspace: Some(2),
                ..Default::default()
            }
        ));
        assert_eq!(model.revision(), revision);
        assert_eq!(model.window(window).unwrap().title, "Original");
        assert!(!model.insert_workspace_and_move(window, 1));
        assert_eq!(model.workspaces(), &[0, 1]);
    }

    #[test]
    fn removing_last_window_preserves_fixed_slots_and_dynamic_reenable_prunes_them() {
        let mut model = StateModel::new();
        model.set_fixed_workspace_count(Some(4));
        let window = model.insert("Window", None, 2);
        model.remove(window);
        assert_eq!(model.workspaces(), &[0, 1, 2, 3]);
        model.set_fixed_workspace_count(None);
        assert_eq!(model.workspaces(), &[0]);
        assert!(model.set_active_workspace(5));
        assert_eq!(model.active_workspace(), 5);
    }

    #[test]
    fn fixed_count_clamps_to_schema_range_and_insertion_uses_last_valid_slot() {
        let mut model = StateModel::new();
        model.set_fixed_workspace_count(Some(0));
        assert_eq!(model.fixed_workspace_count(), Some(1));
        let window = model.insert("Window", None, u32::MAX);
        assert_eq!(model.window(window).unwrap().workspace, 0);
        model.set_fixed_workspace_count(Some(u32::MAX));
        assert_eq!(model.fixed_workspace_count(), Some(36));
        assert_eq!(model.workspaces().len(), 36);
        let revision = model.revision();
        assert!(!model.set_fixed_workspace_count(Some(36)));
        assert_eq!(model.revision(), revision);
    }
}

#[cfg(test)]
mod dynamic_workspace_tests {
    use super::*;

    #[test]
    fn idle_has_two_slots_and_reconciliation_is_idempotent() {
        let mut model = StateModel::new();
        assert!(model.reconcile_dynamic_workspaces(&[]));
        assert_eq!(model.workspaces(), &[0, 1]);
        let revision = model.revision();
        assert!(!model.reconcile_dynamic_workspaces(&[]));
        assert_eq!(model.revision(), revision);
    }

    #[test]
    fn single_trailing_pair_with_active_trailing() {
        // The departed-workspace grace lease (J-WS-NEXT) only has to
        // survive the first post-switch compaction: once the pair is
        // kept, an all-empty [0, 1] with active trailing is a stable
        // fixed point with or without the lease.
        let mut model = StateModel::new();
        model.reconcile_dynamic_workspaces(&[]);
        model.set_active_workspace(1);
        model.reconcile_dynamic_workspaces(&[0]);
        assert_eq!(model.workspaces(), &[0, 1]);
        assert_eq!(model.active_workspace(), 1);
        let revision = model.revision();
        assert!(!model.reconcile_dynamic_workspaces(&[0]));
        assert!(!model.reconcile_dynamic_workspaces(&[]));
        assert_eq!(model.revision(), revision);
        assert_eq!(model.active_workspace(), 1);
    }

    #[test]
    fn occupying_last_slot_adds_one_empty_tail() {
        let mut model = StateModel::new();
        model.reconcile_dynamic_workspaces(&[]);
        model.insert("First", None, 0);
        assert!(!model.reconcile_dynamic_workspaces(&[]));
        model.insert("Second", None, 1);
        assert!(model.reconcile_dynamic_workspaces(&[]));
        assert_eq!(model.workspaces(), &[0, 1, 2]);
        assert!(!model.reconcile_dynamic_workspaces(&[]));
    }

    #[test]
    fn gaps_compact_without_changing_window_identity_contents_or_focus() {
        let mut model = StateModel::new();
        let first = model.insert("First", Some("org.tuna.First"), 0);
        let last = model.insert("Last", Some("org.tuna.Last"), 4);
        model.set_focused(Some(last));
        model.set_active_workspace(4);
        assert!(model.reconcile_dynamic_workspaces(&[]));
        assert_eq!(model.workspaces(), &[0, 1, 2]);
        assert_eq!(model.active_workspace(), 1);
        assert_eq!(model.focused(), Some(last));
        assert_eq!(model.window(first).unwrap().workspace, 0);
        let window = model.window(last).unwrap();
        assert_eq!(window.workspace, 1);
        assert_eq!(window.title, "Last");
        assert_eq!(window.app_id.as_deref(), Some("org.tuna.Last"));
        assert!(matches!(
            model.changes.back().unwrap().1,
            StateChange::WorkspacesChanged
        ));
    }

    #[test]
    fn active_empty_slot_remains_between_occupied_slots() {
        let mut model = StateModel::new();
        model.insert("First", None, 0);
        model.insert("Last", None, 4);
        model.set_active_workspace(2);
        model.reconcile_dynamic_workspaces(&[]);
        assert_eq!(model.workspaces(), &[0, 1, 2, 3]);
        assert_eq!(model.active_workspace(), 1);
    }

    #[test]
    fn empty_slots_after_active_and_trailing_slot_are_removed() {
        let mut model = StateModel::new();
        model.insert("First", None, 0);
        model.workspaces = (0..7).collect();
        model.set_active_workspace(5);
        // set_active_workspace eagerly prunes legacy slots; exercise the tracker topology.
        model.workspaces = (0..7).collect();
        model.reconcile_dynamic_workspaces(&[]);
        assert_eq!(model.workspaces(), &[0, 1, 2]);
        assert_eq!(model.active_workspace(), 2);
    }

    #[test]
    fn keep_alive_preserves_empty_startup_and_dialog_slots_until_released() {
        let mut model = StateModel::new();
        model.insert("First", None, 0);
        model.reconcile_dynamic_workspaces(&[1, 2]);
        assert_eq!(model.workspaces(), &[0, 1, 2, 3]);
        assert!(!model.reconcile_dynamic_workspaces(&[1, 2]));
        assert!(model.reconcile_dynamic_workspaces(&[]));
        assert_eq!(model.workspaces(), &[0, 1]);
    }

    #[test]
    fn all_workspace_windows_do_not_keep_empty_slots_alive() {
        let mut model = StateModel::new();
        let sticky = model.insert("Sticky", None, 4);
        model.update(
            sticky,
            WindowUpdate {
                on_all_workspaces: Some(true),
                ..Default::default()
            },
        );
        model.reconcile_dynamic_workspaces(&[]);
        assert_eq!(model.workspaces(), &[0, 1]);
        // The retained second empty slot becomes index 1; sticky home does not
        // count as occupied and does not force a third slot.
        assert_eq!(model.window(sticky).unwrap().workspace, 1);
        assert!(model.window(sticky).unwrap().on_all_workspaces);
        assert!(!model.reconcile_dynamic_workspaces(&[]));
    }

    #[test]
    fn remap_preserves_external_lease_identity_when_preceding_empty_slot_is_removed() {
        let mut model = StateModel::new();
        model.insert("First", None, 0);
        model.set_active_workspace(3);
        let remap = model.reconcile_dynamic_workspaces_with_remap(&[4]).unwrap();
        assert_eq!(remap, [(0, 0), (3, 1), (4, 2), (5, 3)]);
        assert_eq!(model.active_workspace(), 1);
        assert_eq!(model.workspaces(), &[0, 1, 2, 3]);
        assert!(model
            .reconcile_dynamic_workspaces_with_remap(&[2])
            .is_none());
    }

    #[test]
    fn fixed_policy_never_compacts_or_adds_dynamic_slots() {
        let mut model = StateModel::new();
        model.set_fixed_workspace_count(Some(4));
        let revision = model.revision();
        assert!(!model.reconcile_dynamic_workspaces(&[100]));
        assert_eq!(model.workspaces(), &[0, 1, 2, 3]);
        assert_eq!(model.revision(), revision);
    }

    #[test]
    fn maximum_external_workspace_id_compacts_without_overflow() {
        let mut model = StateModel::new();
        let window = model.insert("Far", None, u32::MAX);
        model.set_active_workspace(u32::MAX);
        model.reconcile_dynamic_workspaces(&[]);
        assert_eq!(model.workspaces(), &[0, 1]);
        assert_eq!(model.active_workspace(), 0);
        assert_eq!(model.window(window).unwrap().workspace, 0);
        assert!(!model.reconcile_dynamic_workspaces(&[]));
    }
}
