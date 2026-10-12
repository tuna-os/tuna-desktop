//! Privacy device enforcement on the PipeWire graph.
//!
//! GTK-free: the [`Tracker`] plans mute/destroy steps from graph
//! scans ([`AudioState::privacy_scan`](crate::audio_state::AudioState::privacy_scan))
//! and the caller executes them — the GLib loop through the existing
//! async `wpctl`/`pw-cli` helpers, blocking test drivers through
//! [`run_blocking`].
//!
//! Enforcement rules (see `docs/privacy-enforcement.md` for the
//! GNOME comparison):
//!
//! - Microphone disabled: every `Audio/Source` reads muted. Capture
//!   streams keep running but capture silence; an unmute attempt is
//!   re-muted on the next graph event. Nothing is destroyed, so
//!   explicit restore just unmutes.
//! - Sound output disabled: every `Audio/Sink` reads muted, same
//!   re-mute and restore shape.
//! - Camera disabled: `Video/Source` nodes and video-capture streams
//!   linked to them are destroyed, existing and newly appearing.
//!   Unlinked capture streams capture nothing and are left alone, so
//!   a screen-share consumer linking up later is never broken by a
//!   camera kill-switch.
//!
//! Restore is explicit and observable: flipping a key back unmutes
//! exactly the nodes this tracker muted (a node the user muted by
//! hand stays muted), and video streams created after restore are
//! no longer destroyed.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
#[cfg(test)]
use std::time::{Duration, Instant};

/// One enforcement step against the PipeWire graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Set a node's mute flag by id (`wpctl set-mute`).
    SetMute {
        /// Node id from `pw-dump`.
        node: u64,
        /// Target mute state (`false` only on restore).
        muted: bool,
    },
    /// Destroy a node by id (`pw-cli destroy`): camera devices and
    /// their capture streams while the camera is disabled.
    DestroyNode {
        /// Node id from `pw-dump`.
        node: u64,
    },
}

impl Action {
    /// The node this action targets (the dispatch queue's dedupe key).
    pub fn node(&self) -> u64 {
        match *self {
            Action::SetMute { node, .. } | Action::DestroyNode { node } => node,
        }
    }
}

/// One graph scan folded against the policy: what must be muted,
/// what must not exist, and what is muted right now. Built by
/// [`AudioState::privacy_scan`](crate::audio_state::AudioState::privacy_scan).
#[derive(Debug, Default)]
pub struct PrivacyScan {
    /// A complete initial dump has established this monitor's graph baseline.
    /// False during disconnect/restart and while that dump is partially read.
    pub authoritative: bool,
    /// PipeWire object.serial identifies the lifetime behind a reusable id.
    pub serials: BTreeMap<u64, u64>,
    /// Audio nodes still present, even when their mute snapshot is stale.
    pub audio_present: BTreeSet<u64>,
    /// Nodes that must read muted right now.
    pub mute_required: BTreeSet<u64>,
    /// Camera nodes/streams that must not exist right now.
    pub destroy: Vec<u64>,
    /// Nodes currently muted.
    pub muted_now: BTreeSet<u64>,
}

/// `wpctl` argv for an action (`None` for non-wpctl actions).
#[cfg(test)]
pub fn wpctl_args(action: &Action) -> Option<[String; 3]> {
    match action {
        Action::SetMute { node, muted } => Some([
            "set-mute".to_owned(),
            node.to_string(),
            if *muted {
                "1".to_owned()
            } else {
                "0".to_owned()
            },
        ]),
        Action::DestroyNode { .. } => None,
    }
}

/// `pw-cli` argv for an action (`None` for non-pw-cli actions).
#[cfg(test)]
pub fn pw_cli_args(action: &Action) -> Option<[String; 2]> {
    match action {
        Action::DestroyNode { node } => Some(["destroy".to_owned(), node.to_string()]),
        Action::SetMute { .. } => None,
    }
}

/// Enforcement state across scans: which nodes this tracker muted,
/// so explicit restore unmutes exactly those.
///
/// Ownership is released only on observed completion: a tracked node
/// stays tracked until a scan shows it no longer muted (or gone from
/// the graph). A restore the caller defers, truncates, or fails to
/// execute therefore re-plans on the next tick instead of being
/// forgotten.
#[derive(Debug, Default)]
pub struct Tracker {
    muted_by_policy: BTreeSet<u64>,
    unsettled_mutes: BTreeSet<u64>,
    serials: BTreeMap<u64, u64>,
}

impl Tracker {
    /// Plan one round: mute what the policy requires, destroy what
    /// must not exist, and unmute tracked nodes the policy no longer
    /// requires muted. A tracked node that is still muted keeps its
    /// ownership — the unmute re-plans every round until the graph
    /// observes it unmuted — while a tracked node that reads unmuted
    /// (restored, whether by us or another party, or gone from the
    /// graph) is forgotten silently.
    pub fn plan(&mut self, scan: &PrivacyScan) -> Vec<Action> {
        if !scan.authoritative {
            // Preserve ownership; callers must also leave queued work alone.
            return Vec::new();
        }
        // Coalesced tombstone/replacement events can reuse an id without an
        // observable absent scan. Never transfer restore ownership to it.
        self.muted_by_policy
            .retain(|id| self.serials.get(id) == scan.serials.get(id));
        self.unsettled_mutes
            .retain(|id| self.serials.get(id) == scan.serials.get(id));
        self.serials.clone_from(&scan.serials);
        let mut actions = Vec::new();
        for id in scan.mute_required.difference(&scan.muted_now) {
            if !scan.serials.contains_key(id) {
                continue;
            }
            actions.push(Action::SetMute {
                node: *id,
                muted: true,
            });
            self.muted_by_policy.insert(*id);
        }
        for id in &scan.destroy {
            if !scan.serials.contains_key(id) {
                continue;
            }
            actions.push(Action::DestroyNode { node: *id });
        }
        let stale: Vec<u64> = self
            .muted_by_policy
            .difference(&scan.mute_required)
            .copied()
            .collect();
        for id in stale {
            if !scan.audio_present.contains(&id) {
                self.muted_by_policy.remove(&id);
                self.unsettled_mutes.remove(&id);
            } else if scan.muted_now.contains(&id) || self.unsettled_mutes.contains(&id) {
                // Still muted: retain ownership so a deferred or
                // failed unmute re-plans on the next tick.
                actions.push(Action::SetMute {
                    node: id,
                    muted: false,
                });
            } else {
                self.muted_by_policy.remove(&id);
            }
        }
        actions
    }

    /// A dispatched mute can still change the graph after a policy lift.
    /// Retain its restore obligation even when the latest scan is unmuted.
    pub fn dispatched(&mut self, action: &Action) {
        if let Action::SetMute { node, muted: true } = action {
            self.unsettled_mutes.insert(*node);
        }
    }

    /// Only a successful compensating unmute settles a dispatched mute.
    /// A failed command may have changed the graph before returning failure.
    #[cfg(test)]
    pub fn completed(&mut self, action: &Action, success: bool) {
        if let Some(serial) = self.serials.get(&action.node()).copied() {
            self.completed_for(action, serial, success);
        }
    }

    /// Async outcomes belong to the serial captured at dispatch, not a
    /// replacement with the same global id observed before completion.
    pub fn completed_for(&mut self, action: &Action, serial: u64, success: bool) {
        if self.serials.get(&action.node()) != Some(&serial) {
            return;
        }
        if success {
            if let Action::SetMute { node, muted: false } = action {
                self.unsettled_mutes.remove(node);
            }
        }
    }

    /// Nodes this tracker muted and has not restored yet (tests).
    #[cfg(test)]
    fn tracked(&self) -> &BTreeSet<u64> {
        &self.muted_by_policy
    }
}

/// Consecutive per-node failures before the queue stops scheduling
/// proactive retry ticks for that node. Ownership stays with the
/// [`Tracker`] (or the live scan, for destroys), so the next graph
/// event or settings change retries — the bound only stops a
/// persistently failing tool from self-perpetuating ticks.
const MAX_ACTION_RETRIES: u32 = 3;

/// Cap on dispatched-but-unreported actions. [`ActionQueue::take`]
/// leaves work queued past this headroom instead of piling up
/// subprocesses while a tool hangs; completions (or their timeouts)
/// free it again.
const MAX_INFLIGHT_ACTIONS: usize = 256;

/// Cap on remembered per-node failure counts. Eviction only re-arms
/// proactive retries (the safe direction); ownership is unaffected.
const MAX_TRACKED_FAILURES: usize = 4096;

/// Bounded per-tick dispatch buffer between [`Tracker::plan`] and the
/// async executors.
///
/// Each tick offers the fresh plan and takes at most a budget of
/// actions to dispatch; the rest stays queued and drains on later
/// ticks instead of being dropped. The queue holds at most one action
/// per node — a fresh action for a node supersedes its queued one,
/// and queued actions for nodes with no fresh demand are dropped as
/// achieved, moot, or superseded — so its length never exceeds one
/// scan's demand. Dispatched actions are remembered until reported,
/// so a re-plan never duplicates an identical in-flight command while
/// still queueing changed intent behind the old command when the policy changes.
#[derive(Debug, Default)]
pub struct ActionQueue {
    queued: VecDeque<Action>,
    inflight: BTreeMap<u64, Action>,
    failures: BTreeMap<u64, u32>,
}

impl ActionQueue {
    /// Reconcile fresh demand against retained work: fresh actions win
    /// per node (replacing the queued one), queued actions for nodes
    /// with no fresh demand are dropped, and fresh actions identical
    /// to an in-flight command are skipped (the executor's report
    /// settles those; queueing another copy would only pile up
    /// duplicate subprocesses).
    pub fn offer(&mut self, actions: Vec<Action>) {
        let mut fresh = BTreeMap::new();
        for action in actions {
            if self.inflight.get(&action.node()) == Some(&action) {
                continue;
            }
            fresh.insert(action.node(), action);
        }
        self.queued
            .retain(|action| fresh.contains_key(&action.node()));
        let inflight = &self.inflight;
        self.failures
            .retain(|node, _| fresh.contains_key(node) || inflight.contains_key(node));
        for (node, action) in fresh {
            if let Some(slot) = self.queued.iter_mut().find(|queued| queued.node() == node) {
                *slot = action;
            } else {
                self.queued.push_back(action);
            }
        }
    }

    /// Take up to `limit` actions to dispatch this tick, oldest first.
    /// Taken actions read as in-flight until [`ActionQueue::report`]
    /// settles them; past the in-flight cap nothing is taken and the
    /// work stays queued for a later tick.
    pub fn take(&mut self, limit: usize) -> Vec<Action> {
        let headroom = MAX_INFLIGHT_ACTIONS.saturating_sub(self.inflight.len());
        let limit = limit.min(headroom);
        let queued = self.queued.len();
        let mut batch = Vec::with_capacity(limit.min(queued));
        for _ in 0..queued {
            let Some(action) = self.queued.pop_front() else {
                break;
            };
            // Opposite intents must wait for the old command to finish;
            // otherwise a late mute can overwrite a completed restore.
            if batch.len() >= limit || self.inflight.contains_key(&action.node()) {
                self.queued.push_back(action);
                continue;
            }
            self.inflight.insert(action.node(), action);
            batch.push(action);
        }
        batch
    }

    /// Report a dispatched action's outcome. Returns true when the
    /// caller should schedule another drain tick: queued work remains,
    /// or this failure still has proactive retries left. Past the
    /// retry bound the work stays owned (the tracker re-plans it on
    /// the next graph event or settings change) but no tick is
    /// scheduled for it. A stale report — one whose action no longer
    /// matches what is in flight for its node — never clears newer
    /// intent, and success clears that node's failure count.
    pub fn report(&mut self, action: &Action, success: bool) -> bool {
        if self.inflight.get(&action.node()) == Some(action) {
            self.inflight.remove(&action.node());
        }
        if success {
            self.failures.remove(&action.node());
        } else {
            let count = self
                .failures
                .get(&action.node())
                .copied()
                .unwrap_or(0)
                .saturating_add(1);
            if !self.failures.contains_key(&action.node())
                && self.failures.len() >= MAX_TRACKED_FAILURES
            {
                self.failures.pop_first();
            }
            self.failures.insert(action.node(), count);
            if count <= MAX_ACTION_RETRIES {
                return true;
            }
        }
        !self.queued.is_empty()
    }

    /// Queued (undispatched) actions (tests).
    #[cfg(test)]
    fn len(&self) -> usize {
        self.queued.len()
    }

    /// True when nothing is queued (in-flight commands settle through
    /// their own reports, not through ticks).
    pub fn is_empty(&self) -> bool {
        self.queued.is_empty()
    }

    /// Nodes with remembered failure counts (tests).
    #[cfg(test)]
    fn failure_nodes(&self) -> usize {
        self.failures.len()
    }
}

fn serial_matches(expected: u64, observed: Option<u64>) -> bool {
    observed == Some(expected)
}

/// Execute against a verified bound Node proxy, never a reusable global id.
/// Each worker has one two-second deadline for discovery, identity and mutation.
fn run_bound(action: Action, serial: u64) -> bool {
    use pipewire::spa::pod::{self, serialize::PodSerializer, Pod, Property};
    use pipewire::{context::ContextRc, core::CoreRc, loop_::Timeout, main_loop::MainLoopRc};
    use std::{
        cell::{Cell, RefCell},
        rc::Rc,
        time::{Duration, Instant},
    };

    fn roundtrip(loop_: &MainLoopRc, core: &CoreRc, failed: &Cell<bool>, end: Instant) -> bool {
        let Ok(sequence) = core.sync(0) else {
            return false;
        };
        let done = Rc::new(Cell::new(false));
        let observed = done.clone();
        let _done = core
            .add_listener_local()
            .done(move |id, seq| {
                if id == pipewire::core::PW_ID_CORE && seq == sequence {
                    observed.set(true);
                }
            })
            .register();
        while !done.get() && !failed.get() && Instant::now() < end {
            loop_
                .loop_()
                .iterate(Timeout::Finite(Duration::from_millis(25)));
        }
        done.get() && !failed.get()
    }

    let Ok(id) = u32::try_from(action.node()) else {
        return false;
    };
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(pipewire::init);
    let Ok(loop_) = MainLoopRc::new(None) else {
        return false;
    };
    struct Entered<'a>(&'a pipewire::loop_::Loop);
    impl Drop for Entered<'_> {
        fn drop(&mut self) {
            // SAFETY: matched to enter below on this worker thread.
            unsafe { self.0.leave() };
        }
    }
    // SAFETY: one worker owns this loop, and the guard pairs leave on every
    // return path. No callbacks enter this loop recursively.
    unsafe { loop_.loop_().enter() };
    let _entered = Entered(loop_.loop_());
    let Ok(context) = ContextRc::new(&loop_, None) else {
        return false;
    };
    let Ok(core) = context.connect_rc(None) else {
        return false;
    };
    let Ok(registry) = core.get_registry_rc() else {
        return false;
    };
    let failed = Rc::new(Cell::new(false));
    let errors = failed.clone();
    let _errors = core
        .add_listener_local()
        .error(move |_, _, _, _| errors.set(true))
        .register();
    let bound = Rc::new(RefCell::new(None));
    let target = bound.clone();
    let observed = Rc::new(Cell::new(None));
    let info_serial = observed.clone();
    let lookup = registry.clone();
    let _registry = registry
        .add_listener_local()
        .global(move |global| {
            if global.id != id || global.type_ != pipewire::types::ObjectType::Node {
                return;
            }
            if global
                .props
                .and_then(|props| props.get("object.serial"))
                .and_then(|value| value.parse::<u64>().ok())
                != Some(serial)
            {
                return;
            }
            let Ok(node) = lookup.bind::<pipewire::node::Node, _>(global) else {
                return;
            };
            let observed = info_serial.clone();
            let listener = node
                .add_listener_local()
                .info(move |info| {
                    observed.set(
                        info.props()
                            .and_then(|props| props.get("object.serial"))
                            .and_then(|value| value.parse::<u64>().ok()),
                    );
                })
                .register();
            *target.borrow_mut() = Some((node, listener));
        })
        .register();
    let end = Instant::now() + Duration::from_secs(2);
    // First barrier discovers globals; binds sent from those callbacks need
    // their own second barrier before the Node's authoritative info arrives.
    if !roundtrip(&loop_, &core, &failed, end)
        || !roundtrip(&loop_, &core, &failed, end)
        || !serial_matches(serial, observed.get())
    {
        return false;
    }
    let Some((node, listener)) = bound.borrow_mut().take() else {
        return false;
    };
    match action {
        Action::SetMute { muted, .. } => {
            let object = pod::Object {
                type_: pipewire::spa::sys::SPA_TYPE_OBJECT_Props,
                id: pipewire::spa::sys::SPA_PARAM_Props,
                properties: vec![Property {
                    key: pipewire::spa::sys::SPA_PROP_mute,
                    flags: pod::PropertyFlags::empty(),
                    value: pod::Value::Bool(muted),
                }],
            };
            let Ok((cursor, _)) = PodSerializer::serialize(
                std::io::Cursor::new(Vec::new()),
                &pod::Value::Object(object),
            ) else {
                return false;
            };
            let buffer = cursor.into_inner();
            let Some(param) = Pod::from_bytes(&buffer) else {
                return false;
            };
            node.set_param(pipewire::spa::param::ParamType::Props, 0, param);
            // Keep the proxy alive until the mutation's ordered barrier; if
            // the object disappears, the request fails, never retargets by id.
            let ok = roundtrip(&loop_, &core, &failed, end);
            drop(listener);
            drop(node);
            ok
        }
        Action::DestroyNode { .. } => {
            drop(listener);
            if core.destroy_object(node).is_err() {
                return false;
            }
            roundtrip(&loop_, &core, &failed, end)
        }
    }
}

/// GTK completion polling keeps native PipeWire discovery off its main loop.
pub fn run_async(action: Action, serial: u64, done: impl FnOnce(bool) + 'static) {
    let (send, receive) = std::sync::mpsc::channel();
    if std::thread::Builder::new()
        .name("tuna-privacy".into())
        .spawn(move || {
            let _ = send.send(run_bound(action, serial));
        })
        .is_err()
    {
        done(false);
        return;
    }
    let mut done = Some(done);
    glib::timeout_add_local(std::time::Duration::from_millis(25), move || {
        let outcome = match receive.try_recv() {
            Ok(ok) => ok,
            Err(std::sync::mpsc::TryRecvError::Empty) => return glib::ControlFlow::Continue,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => false,
        };
        if let Some(done) = done.take() {
            done(outcome);
        }
        glib::ControlFlow::Break
    });
}

/// Bound on one blocking enforcement command (tests only).
#[cfg(test)]
const RUN_TIMEOUT: Duration = Duration::from_secs(5);

/// Run one action to completion, blocking (test drivers only; the
/// shell dispatches through the async GLib helpers instead).
/// The child sees `runtime`/`address` as its PipeWire runtime and
/// session bus, so parallel harnesses never share audio state.
/// `false` when the tool is missing, fails, or overruns its bound.
#[cfg(test)]
pub fn run_blocking(action: &Action, runtime: &std::path::Path, address: &str) -> bool {
    let (tool, args): (&str, Vec<String>) = match action {
        Action::SetMute { .. } => {
            let Some(argv) = wpctl_args(action) else {
                return false;
            };
            ("wpctl", argv.into())
        }
        Action::DestroyNode { .. } => {
            let Some(argv) = pw_cli_args(action) else {
                return false;
            };
            ("pw-cli", argv.into())
        }
    };
    let mut child = match std::process::Command::new(tool)
        .args(&args)
        .env("XDG_RUNTIME_DIR", runtime)
        .env("DBUS_SESSION_BUS_ADDRESS", address)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return false,
    };
    let deadline = Instant::now() + RUN_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    return false;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(mute_required: &[u64], destroy: &[u64], muted_now: &[u64]) -> PrivacyScan {
        PrivacyScan {
            authoritative: true,
            serials: mute_required
                .iter()
                .chain(muted_now)
                .map(|id| (*id, *id))
                .collect(),
            audio_present: mute_required.iter().chain(muted_now).copied().collect(),
            mute_required: mute_required.iter().copied().collect(),
            destroy: destroy.to_vec(),
            muted_now: muted_now.iter().copied().collect(),
        }
    }

    #[test]
    fn command_lines_target_the_node() {
        assert_eq!(
            wpctl_args(&Action::SetMute {
                node: 42,
                muted: true
            }),
            Some(["set-mute".to_owned(), "42".to_owned(), "1".to_owned()])
        );
        assert_eq!(
            wpctl_args(&Action::SetMute {
                node: 42,
                muted: false
            }),
            Some(["set-mute".to_owned(), "42".to_owned(), "0".to_owned()])
        );
        assert_eq!(
            pw_cli_args(&Action::DestroyNode { node: 61 }),
            Some(["destroy".to_owned(), "61".to_owned()])
        );
        assert_eq!(wpctl_args(&Action::DestroyNode { node: 61 }), None);
        assert_eq!(
            pw_cli_args(&Action::SetMute {
                node: 42,
                muted: true
            }),
            None
        );
    }

    #[test]
    fn plan_mutes_unmuted_destroys_cameras_and_ignores_the_rest() {
        let mut tracker = Tracker::default();
        let actions = tracker.plan(&scan(&[48, 49], &[60, 61], &[49]));
        assert_eq!(
            actions,
            vec![
                Action::SetMute {
                    node: 48,
                    muted: true
                },
                Action::DestroyNode { node: 60 },
                Action::DestroyNode { node: 61 },
            ]
        );
        // Only the node this tracker muted is tracked: 49 was
        // already muted (by the user, say) and stays untracked.
        assert_eq!(tracker.tracked(), &BTreeSet::from([48]));
        // A steady scan plans nothing.
        assert!(tracker.plan(&scan(&[48, 49], &[], &[48, 49])).is_empty());
    }

    #[test]
    fn plan_remutes_a_bypass_attempt() {
        let mut tracker = Tracker::default();
        assert_eq!(tracker.plan(&scan(&[50], &[], &[])).len(), 1);
        // The node reads muted: steady.
        assert!(tracker.plan(&scan(&[50], &[], &[50])).is_empty());
        // Something unmuted it behind our back: mute it again.
        assert_eq!(
            tracker.plan(&scan(&[50], &[], &[])),
            vec![Action::SetMute {
                node: 50,
                muted: true
            }]
        );
    }

    #[test]
    fn plan_restore_unmutes_only_tracked_nodes() {
        let mut tracker = Tracker::default();
        assert_eq!(tracker.plan(&scan(&[48], &[], &[])).len(), 1);
        assert!(tracker.plan(&scan(&[48], &[], &[48])).is_empty());
        // Policy lifted: the tracked node is still muted, so an
        // unmute plans — and ownership is retained, so a deferred or
        // failed restore re-plans on the next tick instead of being
        // forgotten. A node muted by hand (49) is not ours to touch.
        let unmute = vec![Action::SetMute {
            node: 48,
            muted: false,
        }];
        assert_eq!(tracker.plan(&scan(&[], &[], &[48, 49])), unmute);
        assert_eq!(tracker.tracked(), &BTreeSet::from([48]));
        assert_eq!(tracker.plan(&scan(&[], &[], &[48, 49])), unmute);
        assert_eq!(tracker.tracked(), &BTreeSet::from([48]));
        // Observed unmuted: forgotten silently.
        assert!(tracker.plan(&scan(&[], &[], &[49])).is_empty());
        assert!(tracker.tracked().is_empty());
        assert!(tracker.plan(&scan(&[], &[], &[49])).is_empty());
    }

    #[test]
    fn plan_forgets_vanished_nodes_silently() {
        let mut tracker = Tracker::default();
        assert_eq!(tracker.plan(&scan(&[48], &[], &[])).len(), 1);
        // The node left the graph (neither required nor muted):
        // no unmute for a node that is gone.
        assert!(tracker.plan(&scan(&[], &[], &[])).is_empty());
        assert!(tracker.tracked().is_empty());
    }

    fn mute(node: u64) -> Action {
        Action::SetMute { node, muted: true }
    }

    fn unmute(node: u64) -> Action {
        Action::SetMute { node, muted: false }
    }

    #[test]
    fn queue_coalesces_supersedes_and_drops_stale() {
        let mut queue = ActionQueue::default();
        // One slot per node no matter how often the same demand is
        // offered: re-plans never grow the queue.
        for _ in 0..1000 {
            queue.offer(vec![mute(7)]);
        }
        assert_eq!(queue.len(), 1);
        // Fresh intent supersedes queued intent for the same node.
        queue.offer(vec![unmute(7)]);
        assert_eq!(queue.take(64), vec![unmute(7)]);
        assert!(queue.is_empty());
        // Queued work for nodes with no fresh demand is dropped as
        // achieved, moot, or superseded — never dispatched stale.
        queue.offer(vec![mute(8), mute(9)]);
        queue.offer(vec![mute(8)]);
        assert_eq!(queue.len(), 1);
        queue.offer(vec![]);
        assert!(queue.is_empty());
    }

    #[test]
    fn queue_skips_identical_inflight_and_serializes_changed_intent() {
        let mut queue = ActionQueue::default();
        queue.offer(vec![mute(7), mute(8)]);
        assert_eq!(queue.take(1), vec![mute(7)]);
        // A re-plan while 7's mute flies: the duplicate is skipped,
        // the still-queued demand coalesces.
        queue.offer(vec![mute(7), mute(8)]);
        assert_eq!(queue.len(), 1);
        assert_eq!(queue.take(64), vec![mute(8)]);
        // The restore waits behind the mute, without blocking other nodes.
        queue.offer(vec![unmute(7)]);
        assert!(queue.take(64).is_empty());
        assert!(queue.report(&mute(7), true));
        assert_eq!(queue.take(64), vec![unmute(7)]);
        queue.offer(vec![unmute(7), mute(9)]);
        assert_eq!(queue.len(), 1);
        assert!(queue.report(&unmute(7), true));
        assert_eq!(queue.take(64), vec![mute(9)]);
        assert!(!queue.report(&mute(9), true));
    }

    #[test]
    fn policy_lift_before_mute_completion_keeps_serialized_restore() {
        fn lifted() -> PrivacyScan {
            PrivacyScan {
                authoritative: true,
                serials: BTreeMap::from([(7, 7)]),
                audio_present: BTreeSet::from([7]),
                ..PrivacyScan::default()
            }
        }
        let mut tracker = Tracker::default();
        let mut queue = ActionQueue::default();
        queue.offer(tracker.plan(&scan(&[7], &[], &[])));
        let action = queue.take(1)[0];
        tracker.dispatched(&action);
        // Lift before the graph has observed the in-flight mute.
        queue.offer(tracker.plan(&lifted()));
        assert_eq!(tracker.tracked(), &BTreeSet::from([7]));
        assert!(queue.take(1).is_empty());
        tracker.completed(&action, true);
        assert!(queue.report(&action, true));
        // Even a stale unmuted scan cannot discard the restore obligation.
        queue.offer(tracker.plan(&lifted()));
        let restore = queue.take(1)[0];
        assert_eq!(restore, unmute(7));
        tracker.dispatched(&restore);
        tracker.completed(&restore, true);
        assert!(!queue.report(&restore, true));
        assert!(tracker.plan(&lifted()).is_empty());
        assert!(tracker.tracked().is_empty());
    }

    #[test]
    fn failed_restore_after_policy_lift_retains_ownership() {
        fn lifted() -> PrivacyScan {
            PrivacyScan {
                authoritative: true,
                serials: BTreeMap::from([(7, 7)]),
                audio_present: BTreeSet::from([7]),
                ..PrivacyScan::default()
            }
        }
        let mut tracker = Tracker::default();
        tracker.plan(&scan(&[7], &[], &[]));
        tracker.dispatched(&mute(7));
        tracker.completed(&mute(7), false);
        assert_eq!(tracker.plan(&lifted()), vec![unmute(7)]);
        tracker.completed(&unmute(7), false);
        assert_eq!(tracker.plan(&lifted()), vec![unmute(7)]);
        tracker.completed(&unmute(7), true);
        assert!(tracker.plan(&lifted()).is_empty());
    }

    #[test]
    fn vanished_node_releases_dispatched_mute_ownership() {
        let mut tracker = Tracker::default();
        tracker.plan(&scan(&[7], &[], &[]));
        tracker.dispatched(&mute(7));
        assert!(tracker
            .plan(&PrivacyScan {
                authoritative: true,
                ..PrivacyScan::default()
            })
            .is_empty());
        assert!(tracker.tracked().is_empty());
        assert!(tracker.unsettled_mutes.is_empty());
    }

    #[test]
    fn restarted_partial_monitor_preserves_restore_until_authoritative_dump() {
        use crate::audio_state::AudioState;
        use tuna_shell_host::settings::PrivacyPolicy;

        let mut tracker = Tracker::default();
        let mut queue = ActionQueue::default();
        tracker.plan(&scan(&[7], &[], &[]));
        tracker.dispatched(&mute(7));
        tracker.completed(&mute(7), true);
        queue.offer(vec![unmute(7)]);
        // AudioUi::retry replaces the cache. A pending drain or settings
        // callback during the two-second gap must not reconcile empty demand.
        let mut graph = AudioState::default();
        let policy = PrivacyPolicy::default();
        let initial = graph.privacy_scan(&policy);
        assert!(!initial.authoritative);
        assert!(tracker.plan(&initial).is_empty());
        assert_eq!(tracker.tracked(), &BTreeSet::from([7]));
        assert_eq!(queue.len(), 1);
        assert!(!graph.feed(b"[{\"id\":7,\"info\":").unwrap());
        let partial = graph.privacy_scan(&policy);
        assert!(!partial.authoritative);
        assert!(tracker.plan(&partial).is_empty());
        assert_eq!(tracker.tracked(), &BTreeSet::from([7]));
        assert!(graph
            .feed(
                br#"{"props":{"media.class":"Audio/Source","object.serial":7},"params":{"Props":[{"mute":true}]}}}]"#
            )
            .unwrap());
        let resumed = graph.privacy_scan(&policy);
        assert!(resumed.authoritative);
        assert_eq!(tracker.plan(&resumed), vec![unmute(7)]);
        queue.offer(tracker.plan(&resumed));
        assert_eq!(queue.take(1), vec![unmute(7)]);
        // Only a complete authoritative empty baseline confirms disappearance.
        let mut gone = AudioState::default();
        assert!(gone.feed(b"[]").unwrap());
        assert!(tracker.plan(&gone.privacy_scan(&policy)).is_empty());
        assert!(tracker.tracked().is_empty());
    }

    #[test]
    fn coalesced_reused_id_does_not_inherit_restore_ownership() {
        use crate::audio_state::AudioState;
        use tuna_shell_host::settings::PrivacyPolicy;
        let mut graph = AudioState::default();
        let mut tracker = Tracker::default();
        let mut queue = ActionQueue::default();
        graph.feed(br#"[{"id":7,"info":{"props":{"media.class":"Audio/Source","object.serial":100},"params":{"Props":[{"mute":false}]}}}]"#).unwrap();
        let denied = PrivacyPolicy {
            microphone_disabled: true,
            ..PrivacyPolicy::default()
        };
        queue.offer(tracker.plan(&graph.privacy_scan(&denied)));
        let old_mute = queue.take(1)[0];
        tracker.dispatched(&old_mute);
        // A single monitor read can remove old7 and introduce a user-muted
        // replacement7. No intermediate empty scan is visible to the tracker.
        graph.feed(br#"[{"id":7,"info":null},{"id":7,"info":{"props":{"media.class":"Audio/Source","object.serial":101},"params":{"Props":[{"mute":true}]}}}]"#).unwrap();
        let lifted = graph.privacy_scan(&PrivacyPolicy::default());
        assert!(tracker.plan(&lifted).is_empty());
        assert!(tracker.tracked().is_empty());
        queue.offer(tracker.plan(&lifted));
        assert!(queue.take(1).is_empty());
        // A late completion from old100 cannot acquire/release ownership101.
        tracker.completed_for(&old_mute, 100, true);
        assert!(tracker.plan(&lifted).is_empty());
    }

    #[test]
    fn stale_bound_identity_never_mutates_replacement_and_other_nodes_progress() {
        let mut mutations = Vec::new();
        let mut bound_apply = |expected, bound_serial, action| {
            if serial_matches(expected, Some(bound_serial)) {
                mutations.push(action);
            }
        };
        // A queued old100 intent is refused if lookup now binds replacement101.
        bound_apply(100, 101, unmute(7));
        bound_apply(200, 200, mute(8));
        assert_eq!(mutations, vec![mute(8)]);
        assert!(!serial_matches(100, None));
        // A previously verified bound100 remains that resource if the global
        // id is reused; native requests use the proxy, never another id lookup.
        assert!(serial_matches(100, Some(100)));
        assert!(!serial_matches(100, Some(101)));
    }

    #[test]
    fn blocked_restore_does_not_block_other_nodes() {
        let mut queue = ActionQueue::default();
        queue.offer(vec![mute(7)]);
        assert_eq!(queue.take(1), vec![mute(7)]);
        queue.offer(vec![unmute(7), mute(8)]);
        assert_eq!(queue.take(2), vec![mute(8)]);
        assert!(queue.report(&mute(7), true));
        assert_eq!(queue.take(2), vec![unmute(7)]);
    }

    #[test]
    fn queue_bounds_inflight_and_failure_memory() {
        let mut queue = ActionQueue::default();
        // Past the in-flight cap nothing is taken; the work stays
        // queued for a later tick instead of piling up subprocesses.
        queue.offer((0..600).map(mute).collect());
        let batch = queue.take(600);
        assert_eq!(batch.len(), super::MAX_INFLIGHT_ACTIONS);
        assert_eq!(queue.len(), 600 - super::MAX_INFLIGHT_ACTIONS);
        for action in &batch {
            assert!(queue.report(action, true));
        }
        // Headroom refills as reports land: the backlog drains in
        // bounded batches, nothing dropped.
        let batch = queue.take(600);
        assert_eq!(batch.len(), super::MAX_INFLIGHT_ACTIONS);
        assert_eq!(queue.len(), 600 - 2 * super::MAX_INFLIGHT_ACTIONS);
        for action in &batch {
            assert!(queue.report(action, true));
        }
        let rest = queue.take(600);
        assert_eq!(rest.len(), 600 - 2 * super::MAX_INFLIGHT_ACTIONS);
        assert!(queue.is_empty());
        for action in &rest {
            assert!(!queue.report(action, true));
        }
        // Failure memory is capped; eviction only re-arms proactive
        // retries, and success clears the count.
        for id in 0..(super::MAX_TRACKED_FAILURES as u64 + 100) {
            queue.report(&mute(100_000 + id), false);
        }
        assert_eq!(queue.failure_nodes(), super::MAX_TRACKED_FAILURES);
        assert!(queue.report(&mute(7), false));
        assert!(queue.report(&mute(7), false));
        assert!(!queue.report(&mute(7), true));
        assert!(queue.report(&mute(7), false));
    }

    /// Production-path restore cycles: a fake graph behind the real
    /// [`AudioState`](crate::audio_state::AudioState) feed/scan, the
    /// real [`Tracker`], and the real [`ActionQueue`], with a scripted
    /// executor standing in for `wpctl`. Node JSON matches the
    /// `pw-dump` shape the scanner parses.
    mod restore_cycle {
        use std::cell::RefCell;
        use std::collections::{BTreeMap, BTreeSet};

        use serde_json::json;
        use tuna_shell_host::settings::PrivacyPolicy;

        use super::{mute, unmute, Action, ActionQueue, Tracker};
        use crate::audio_state::AudioState;

        /// A fake PipeWire graph: node id → (media class, muted).
        struct FakeGraph {
            nodes: BTreeMap<u64, (&'static str, bool)>,
            state: AudioState,
        }

        impl FakeGraph {
            fn new(nodes: &[(u64, &'static str, bool)]) -> Self {
                let mut graph = Self {
                    nodes: nodes
                        .iter()
                        .map(|&(id, class, muted)| (id, (class, muted)))
                        .collect(),
                    state: AudioState::default(),
                };
                graph.publish();
                graph
            }

            /// Feed the current graph as one `pw-dump` batch.
            fn publish(&mut self) {
                let batch: Vec<serde_json::Value> = self
                    .nodes
                    .iter()
                    .map(|(&id, (class, muted))| {
                        json!({"id": id,
                            "info": {"props": {"media.class": class, "object.serial": id, "node.name": format!("node-{id}")},
                            "params": {"Props": [{"mute": muted}]}}})
                    })
                    .collect();
                self.state
                    .feed(&serde_json::Value::Array(batch).to_string().into_bytes())
                    .unwrap();
            }

            fn muted(&self, id: u64) -> bool {
                self.nodes[&id].1
            }

            /// The scripted `wpctl`: apply the mute flag, unless this
            /// action is scripted to fail.
            fn execute(&mut self, action: &Action, fail: &dyn Fn(&Action) -> bool) -> bool {
                if fail(action) {
                    return false;
                }
                match *action {
                    Action::SetMute { node, muted } => {
                        if let Some(slot) = self.nodes.get_mut(&node) {
                            slot.1 = muted;
                            true
                        } else {
                            false
                        }
                    }
                    Action::DestroyNode { node } => self.nodes.remove(&node).is_some(),
                }
            }
        }

        struct Rig {
            graph: FakeGraph,
            tracker: Tracker,
            queue: ActionQueue,
            dispatched: Vec<Action>,
        }

        impl Rig {
            /// Run ticks until the tracker and queue settle or `rounds`
            /// run out. Each tick mirrors `AudioUi::enforce_privacy`:
            /// scan → plan → offer → take(`budget`) → execute →
            /// report. Returns how often a report scheduled another
            /// tick.
            fn settle(
                &mut self,
                policy: &PrivacyPolicy,
                budget: usize,
                rounds: usize,
                fail: &dyn Fn(&Action) -> bool,
            ) -> usize {
                let mut reticks = 0;
                for _ in 0..rounds {
                    let scan = self.graph.state.privacy_scan(policy);
                    let planned = self.tracker.plan(&scan);
                    let demanded = !planned.is_empty();
                    self.queue.offer(planned);
                    let batch = self.queue.take(budget);
                    for action in &batch {
                        self.tracker.dispatched(action);
                        let ok = self.graph.execute(action, fail);
                        self.tracker.completed(action, ok);
                        if self.queue.report(action, ok) {
                            reticks += 1;
                        }
                        self.dispatched.push(*action);
                    }
                    self.graph.publish();
                    if !demanded && batch.is_empty() && self.queue.is_empty() {
                        return reticks;
                    }
                }
                panic!("restore did not settle within {rounds} ticks");
            }
        }

        fn denied() -> PrivacyPolicy {
            PrivacyPolicy {
                microphone_disabled: true,
                sound_output_disabled: true,
                ..PrivacyPolicy::default()
            }
        }

        /// 100 policy devices plus 3 the user muted by hand.
        fn big_graph() -> (Rig, BTreeSet<u64>, Vec<u64>) {
            let mut nodes = Vec::new();
            for i in 0..50 {
                nodes.push((100 + i, "Audio/Sink", false));
            }
            for i in 0..50 {
                nodes.push((200 + i, "Audio/Source", false));
            }
            let user_muted = vec![300, 301, 302];
            for &id in &user_muted {
                nodes.push((id, "Audio/Sink", true));
            }
            let policy: BTreeSet<u64> = nodes
                .iter()
                .filter(|(id, _, _)| !user_muted.contains(id))
                .map(|(id, _, _)| *id)
                .collect();
            (
                Rig {
                    graph: FakeGraph::new(&nodes),
                    tracker: Tracker::default(),
                    queue: ActionQueue::default(),
                    dispatched: Vec::new(),
                },
                policy,
                user_muted,
            )
        }

        #[test]
        fn disable_beyond_per_tick_cap_restores_every_node() {
            let (mut rig, policy, user_muted) = big_graph();
            let allowed = PrivacyPolicy::default();

            rig.settle(&denied(), 64, 50, &|_| false);
            for &id in &policy {
                assert!(rig.graph.muted(id), "policy node {id} muted while disabled");
            }
            for &id in &user_muted {
                assert!(
                    rig.graph.muted(id),
                    "user mute {id} preserved while disabled"
                );
            }
            // Exactly the policy nodes are owned: never a user mute.
            assert_eq!(rig.tracker.tracked(), &policy);

            // Lift: the first tick dispatches a bounded batch and
            // retains ownership of everything it cannot dispatch yet.
            let scan = rig.graph.state.privacy_scan(&allowed);
            let planned = rig.tracker.plan(&scan);
            let planned_nodes: BTreeSet<u64> = planned.iter().map(|action| action.node()).collect();
            assert_eq!(planned_nodes, policy);
            assert!(planned
                .iter()
                .all(|action| *action == unmute(action.node())));
            rig.queue.offer(planned);
            let batch = rig.queue.take(64);
            assert_eq!(batch.len(), 64);
            assert_eq!(rig.queue.len(), 36);
            assert_eq!(rig.tracker.tracked(), &policy);
            for action in &batch {
                assert!(rig.graph.execute(action, &|_| false));
                rig.queue.report(action, true);
                rig.dispatched.push(*action);
            }
            rig.graph.publish();

            // The remainder drains on later ticks: every policy node
            // by identity, no user mute touched, nothing left owned.
            rig.settle(&allowed, 64, 50, &|_| false);
            for &id in &policy {
                assert!(!rig.graph.muted(id), "policy node {id} restored");
            }
            for &id in &user_muted {
                assert!(rig.graph.muted(id), "user mute {id} preserved");
            }
            assert!(rig.tracker.tracked().is_empty());
            assert!(rig.queue.is_empty());
            let touched: BTreeSet<u64> =
                rig.dispatched.iter().map(|action| action.node()).collect();
            assert!(touched.is_disjoint(&user_muted.into_iter().collect()));
            assert!(policy.is_subset(&touched));
        }

        #[test]
        fn failed_unmute_retries_then_recovers() {
            let (mut rig, policy, user_muted) = big_graph();
            rig.settle(&denied(), 64, 50, &|_| false);
            assert_eq!(rig.tracker.tracked(), &policy);

            // Two nodes fail their first two unmutes, then recover.
            let attempts = RefCell::new(BTreeMap::new());
            let flaky: BTreeSet<u64> = [100, 200].into_iter().collect();
            let fail = |action: &Action| {
                let mut attempts = attempts.borrow_mut();
                let count = attempts.entry(action.node()).or_insert(0);
                *count += 1;
                flaky.contains(&action.node()) && *count <= 2
            };
            let reticks = rig.settle(&PrivacyPolicy::default(), 64, 50, &fail);
            assert!(
                reticks > 0,
                "failures schedule retry ticks instead of stalling"
            );
            let attempts = attempts.borrow();
            assert_eq!(attempts[&100], 3);
            assert_eq!(attempts[&200], 3);
            for &id in &policy {
                assert!(!rig.graph.muted(id), "policy node {id} restored");
            }
            for &id in &user_muted {
                assert!(rig.graph.muted(id), "user mute {id} preserved");
            }
            assert!(rig.tracker.tracked().is_empty());
            assert!(rig.queue.is_empty());
        }

        #[test]
        fn persistent_failure_backs_off_without_losing_ownership() {
            let mut rig = Rig {
                graph: FakeGraph::new(&[(100, "Audio/Sink", false), (300, "Audio/Sink", true)]),
                tracker: Tracker::default(),
                queue: ActionQueue::default(),
                dispatched: Vec::new(),
            };
            rig.settle(&denied(), 64, 50, &|_| false);
            assert_eq!(rig.tracker.tracked(), &BTreeSet::from([100]));

            // The unmute always fails: proactive retries stop after
            // the bound, but ownership is retained — every later
            // external tick re-plans it.
            for round in 1..=5 {
                let scan = rig.graph.state.privacy_scan(&PrivacyPolicy::default());
                let planned = rig.tracker.plan(&scan);
                assert_eq!(
                    planned,
                    vec![unmute(100)],
                    "round {round}: the stuck node re-plans"
                );
                rig.queue.offer(planned);
                let batch = rig.queue.take(64);
                assert_eq!(batch, vec![unmute(100)]);
                for action in &batch {
                    let ok = rig.graph.execute(action, &|_| true);
                    assert!(!ok);
                    assert_eq!(
                        rig.queue.report(action, ok),
                        round <= super::super::MAX_ACTION_RETRIES,
                        "round {round}: a tick is scheduled only within the retry bound"
                    );
                }
                rig.graph.publish();
            }
            assert_eq!(rig.tracker.tracked(), &BTreeSet::from([100]));
            assert!(rig.queue.is_empty());

            // Healing the executor restores it on the next tick, and
            // the user's mute was never touched.
            rig.settle(&PrivacyPolicy::default(), 64, 50, &|_| false);
            assert!(!rig.graph.muted(100));
            assert!(rig.graph.muted(300));
            assert!(rig.tracker.tracked().is_empty());
            assert!(rig.queue.is_empty());
        }

        #[test]
        fn destroy_leg_survives_the_queue_unchanged() {
            let (mut rig, _, _) = big_graph();
            // Destroys are untracked by design, but they still flow
            // through the queue in full: nothing truncated away.
            let cameras: Vec<Action> = (0..100)
                .map(|i| Action::DestroyNode { node: 1000 + i })
                .collect();
            rig.queue.offer(cameras.clone());
            let mut drained = Vec::new();
            while !rig.queue.is_empty() {
                drained.extend(rig.queue.take(64));
            }
            assert_eq!(drained, cameras);
            for action in &drained {
                assert!(!rig.queue.report(action, true));
            }
            // And the mute helper stays honest about its own shape.
            assert_eq!(
                mute(7),
                Action::SetMute {
                    node: 7,
                    muted: true
                }
            );
        }
    }

    /// Live enforcer against a real headless PipeWire + WirePlumber:
    /// virtual sink/source devices, real GStreamer app streams, and
    /// the real `wpctl`/`pw-cli` executors. Each run owns its
    /// runtime dir, bus, and daemons, so parallel tests never share
    /// audio state. Skips (loudly) when any daemon or tool is
    /// missing.
    mod live_pw {
        use std::path::{Path, PathBuf};
        use std::process::{Child, Command, Stdio};
        use std::time::{Duration, Instant};

        use super::{run_blocking, Action, Tracker};
        use crate::audio_state::AudioState;
        use tuna_shell_host::settings::PrivacyPolicy;

        /// Daemons and tools this harness owns for one run.
        struct Harness {
            _dir: tempfile::TempDir,
            runtime: PathBuf,
            address: String,
            daemons: Vec<Child>,
            gst_children: Vec<Child>,
        }

        impl Harness {
            fn command(&self, tool: &str) -> Command {
                let mut command = Command::new(tool);
                command
                    .env("XDG_RUNTIME_DIR", &self.runtime)
                    .env("DBUS_SESSION_BUS_ADDRESS", &self.address)
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null());
                command
            }

            /// Current graph as parsed state (the enforcement input).
            fn dump(&self) -> Option<AudioState> {
                let output = self.command("pw-dump").output().ok()?;
                if !output.status.success() {
                    return None;
                }
                let mut state = AudioState::default();
                state.feed(&output.stdout).ok()?;
                Some(state)
            }

            /// Current graph as raw objects (the assertion oracle,
            /// independent of the scan under test).
            fn objects(&self) -> Option<Vec<serde_json::Value>> {
                let output = self.command("pw-dump").output().ok()?;
                if !output.status.success() {
                    return None;
                }
                serde_json::from_slice(&output.stdout).ok()
            }

            /// Mute flag of the node named `name`, when present with
            /// mixer state.
            fn muted(&self, name: &str) -> Option<bool> {
                self.objects()?.iter().find_map(|object| {
                    let props = &object["info"]["props"];
                    if props["node.name"].as_str() != Some(name) {
                        return None;
                    }
                    object["info"]["params"]["Props"]
                        .as_array()?
                        .first()
                        .and_then(|props| props["mute"].as_bool())
                })
            }

            /// Ids of nodes with this media class (and optional name).
            fn nodes(&self, class: &str, name: Option<&str>) -> Vec<u64> {
                self.objects()
                    .unwrap_or_default()
                    .iter()
                    .filter(|object| {
                        object["info"]["props"]["media.class"].as_str() == Some(class)
                            && name.is_none_or(|name| {
                                object["info"]["props"]["node.name"].as_str() == Some(name)
                            })
                    })
                    .filter_map(|object| object["id"].as_u64())
                    .collect()
            }

            /// Drive the tracker to a steady state under `policy`:
            /// dump, scan, plan, execute, until a round plans
            /// nothing. Returns the actions executed.
            fn settle(&self, tracker: &mut Tracker, policy: &PrivacyPolicy) -> Vec<Action> {
                let mut executed = Vec::new();
                let deadline = Instant::now() + Duration::from_secs(30);
                while let Some(state) = self.dump() {
                    let actions = tracker.plan(&state.privacy_scan(policy));
                    if actions.is_empty() || Instant::now() >= deadline {
                        break;
                    }
                    for action in &actions {
                        if run_blocking(action, &self.runtime, &self.address) {
                            executed.push(*action);
                        }
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
                executed
            }

            /// Wait up to `timeout` for `condition` on the live
            /// graph (polls `pw-dump`, never sleeps blind).
            fn wait_for(&self, timeout: Duration, condition: impl Fn() -> bool) -> bool {
                let deadline = Instant::now() + timeout;
                while Instant::now() < deadline {
                    if condition() {
                        return true;
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
                condition()
            }

            /// Start a GStreamer pipeline kept alive for the run.
            fn gst(&mut self, args: &[&str]) -> bool {
                match self
                    .command("gst-launch-1.0")
                    .args(args)
                    .env("GST_DEBUG", "0")
                    .stdout(Stdio::null())
                    .spawn()
                {
                    Ok(child) => {
                        self.gst_children.push(child);
                        true
                    }
                    Err(_) => false,
                }
            }

            fn kill_all(children: &mut Vec<Child>) {
                for child in children.iter_mut() {
                    let _ = child.kill();
                }
                for child in children.iter_mut() {
                    let _ = child.wait();
                }
                children.clear();
            }
        }

        impl Drop for Harness {
            fn drop(&mut self) {
                Harness::kill_all(&mut self.gst_children);
                Harness::kill_all(&mut self.daemons);
            }
        }

        /// True when `tool` spawns (exit status ignored: presence,
        /// not behavior).
        fn tool_present(tool: &str) -> bool {
            Command::new(tool)
                .arg("--version")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok()
        }

        /// PipeWire configuration: the installed default plus two
        /// virtual null devices, so the harness never depends on
        /// hardware. `None` when no default config is installed.
        fn virtual_config(dir: &Path) -> Option<PathBuf> {
            let system = std::fs::read_to_string("/usr/share/pipewire/pipewire.conf").ok()?;
            let objects = r#"context.objects = [
    { factory = adapter
      args = {
          factory.name     = support.null-audio-sink
          node.name        = "virt-sink"
          node.description = "Virtual Sink"
          media.class      = "Audio/Sink"
          audio.channels   = 2
          audio.position   = [ FL FR ]
      }
    }
    { factory = adapter
      args = {
          factory.name     = support.null-audio-sink
          node.name        = "virt-source"
          node.description = "Virtual Source"
          media.class      = "Audio/Source"
          audio.channels   = 1
          audio.position   = [ MONO ]
      }
    }
]
"#;
            let mut lines: Vec<&str> = system.lines().collect();
            let start = lines
                .iter()
                .position(|line| line.starts_with("context.objects"))?;
            let end = lines
                .iter()
                .enumerate()
                .skip(start)
                .find(|(_, line)| line.trim() == "]")
                .map(|(index, _)| index)?;
            lines.splice(start..=end, objects.lines());
            let path = dir.join("pipewire.conf");
            std::fs::write(&path, lines.join("\n")).ok()?;
            Some(path)
        }

        /// Private bus daemon plus its address, or `None` when no
        /// daemon is usable (the caller skips the test).
        fn private_bus() -> Option<(String, Child)> {
            let mut child = Command::new("dbus-daemon")
                .args(["--session", "--nofork", "--print-address=1"])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .ok()?;
            let stdout = child.stdout.take()?;
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let mut reader = std::io::BufReader::new(stdout);
                use std::io::BufRead;
                let mut line = String::new();
                let line = reader.read_line(&mut line).ok().map(|_| line);
                let _ = tx.send(line);
            });
            match rx.recv_timeout(Duration::from_secs(10)) {
                Ok(Some(line)) if !line.trim().is_empty() => Some((line.trim().to_owned(), child)),
                _ => {
                    let _ = child.kill();
                    None
                }
            }
        }

        /// Spawn the harness daemons: private bus, headless
        /// PipeWire with virtual devices, headless WirePlumber.
        /// `None` sketches which leg is missing (the caller skips).
        fn harness() -> Option<Harness> {
            for tool in [
                "dbus-daemon",
                "pipewire",
                "wireplumber",
                "wpctl",
                "pw-dump",
                "pw-cli",
                "gst-launch-1.0",
            ] {
                if !tool_present(tool) {
                    eprintln!("SKIP live privacy test: {tool} not on PATH");
                    return None;
                }
            }
            let dir = tempfile::tempdir().expect("privacy harness dir");
            let runtime = dir.path().join("run");
            std::fs::create_dir_all(&runtime).expect("harness runtime dir");
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700))
                    .expect("harness runtime dir mode");
            }
            let Some(config) = virtual_config(dir.path()) else {
                eprintln!("SKIP live privacy test: no /usr/share/pipewire/pipewire.conf");
                return None;
            };
            let Some((address, daemon)) = private_bus() else {
                eprintln!("SKIP live privacy test: no usable dbus-daemon on PATH");
                return None;
            };
            let mut children = vec![daemon];
            let spawn = |children: &mut Vec<Child>,
                         tool: &str,
                         args: &[&str],
                         address: &str,
                         runtime: &Path| {
                Command::new(tool)
                    .args(args)
                    .env("XDG_RUNTIME_DIR", runtime)
                    .env("DBUS_SESSION_BUS_ADDRESS", address)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .ok()
                    .map(|child| children.push(child))
                    .is_some()
            };
            if !spawn(
                &mut children,
                "pipewire",
                &["-c", &config.to_string_lossy()],
                &address,
                &runtime,
            ) {
                Harness::kill_all(&mut children);
                eprintln!("SKIP live privacy test: pipewire would not start");
                return None;
            }
            if !spawn(&mut children, "wireplumber", &[], &address, &runtime) {
                Harness::kill_all(&mut children);
                eprintln!("SKIP live privacy test: wireplumber would not start");
                return None;
            }
            let harness = Harness {
                _dir: dir,
                runtime,
                address,
                daemons: children,
                gst_children: Vec::new(),
            };
            if !harness.wait_for(Duration::from_secs(30), || {
                !harness.nodes("Audio/Sink", Some("virt-sink")).is_empty()
                    && !harness
                        .nodes("Audio/Source", Some("virt-source"))
                        .is_empty()
            }) {
                eprintln!("SKIP live privacy test: virtual devices never appeared");
                return None;
            }
            Some(harness)
        }

        #[test]
        fn live_enforcer_mutes_bypass_and_restores_devices() {
            let Some(mut harness) = harness() else {
                return;
            };
            let mut tracker = Tracker::default();
            let allowed = PrivacyPolicy::default();

            // Baseline: an allowed policy plans nothing.
            assert!(
                harness.settle(&mut tracker, &allowed).is_empty(),
                "allowed policy enforces nothing"
            );
            assert_eq!(harness.muted("virt-sink"), Some(false));
            assert_eq!(harness.muted("virt-source"), Some(false));

            // Real app streams, opened before the policy lands.
            assert!(harness.gst(&[
                "audiotestsrc",
                "wave=silence",
                "!",
                "pipewiresink",
                "stream-properties=props,media.class=Stream/Output/Audio",
            ]));
            assert!(harness.gst(&[
                "pipewiresrc",
                "stream-properties=props,media.class=Stream/Input/Audio",
                "!",
                "fakesink",
            ]));
            assert!(harness.wait_for(Duration::from_secs(30), || {
                !harness.nodes("Stream/Output/Audio", None).is_empty()
                    && !harness.nodes("Stream/Input/Audio", None).is_empty()
            }));

            // Disable microphone and sound output: existing streams
            // keep their nodes, but both devices read muted —
            // capture and playback go silent.
            let denied = PrivacyPolicy {
                microphone_disabled: true,
                sound_output_disabled: true,
                ..PrivacyPolicy::default()
            };
            let actions = harness.settle(&mut tracker, &denied);
            assert!(
                actions
                    .iter()
                    .any(|action| matches!(action, Action::SetMute { muted: true, .. })),
                "disabling mutes devices, got {actions:?}"
            );
            assert_eq!(harness.muted("virt-sink"), Some(true));
            assert_eq!(harness.muted("virt-source"), Some(true));
            assert!(
                !harness.nodes("Stream/Output/Audio", None).is_empty(),
                "playback streams are silenced, not destroyed"
            );
            assert!(
                !harness.nodes("Stream/Input/Audio", None).is_empty(),
                "capture streams are silenced, not destroyed"
            );

            // Negative control: a direct unmute behind the
            // enforcer's back is re-muted, not honored.
            let source = harness.nodes("Audio/Source", Some("virt-source"))[0].to_string();
            let unmuted = harness
                .command("wpctl")
                .args(["set-mute", &source, "0"])
                .output()
                .map(|output| output.status.success())
                .unwrap_or(false);
            assert!(unmuted, "bypass attempt runs");
            harness.settle(&mut tracker, &denied);
            assert_eq!(
                harness.muted("virt-source"),
                Some(true),
                "unmute attempt while disabled is re-muted"
            );

            // A stream opened after the policy lands is silenced
            // too: the device it would use stays muted.
            assert!(harness.gst(&[
                "pipewiresrc",
                "stream-properties=props,media.class=Stream/Input/Audio",
                "!",
                "fakesink",
            ]));
            harness.settle(&mut tracker, &denied);
            assert_eq!(harness.muted("virt-source"), Some(true));

            // Explicit restore re-exposes the actual capability:
            // both devices unmute, and a fresh capture stream sees
            // an unmuted source.
            harness.settle(&mut tracker, &allowed);
            assert_eq!(harness.muted("virt-sink"), Some(false));
            assert_eq!(harness.muted("virt-source"), Some(false));
            assert!(harness.gst(&[
                "pipewiresrc",
                "stream-properties=props,media.class=Stream/Input/Audio",
                "!",
                "fakesink",
            ]));
            assert!(
                harness.settle(&mut tracker, &allowed).is_empty(),
                "restored policy enforces nothing"
            );
            assert_eq!(harness.muted("virt-source"), Some(false));
        }

        #[test]
        fn live_enforcer_destroys_cameras_existing_and_new() {
            let Some(mut harness) = harness() else {
                return;
            };
            let mut tracker = Tracker::default();
            let allowed = PrivacyPolicy::default();
            let camera_off = PrivacyPolicy {
                camera_disabled: true,
                ..PrivacyPolicy::default()
            };

            // A fake camera device (a real graph node published as
            // `Video/Source`) survives while allowed.
            assert!(harness.gst(&[
                "videotestsrc",
                "pattern=black",
                "!",
                "pipewiresink",
                "stream-properties=props,media.class=Video/Source,node.name=fake-camera",
            ]));
            assert!(harness.wait_for(Duration::from_secs(30), || {
                !harness
                    .nodes("Video/Source", Some("fake-camera"))
                    .is_empty()
            }));
            assert!(
                harness.settle(&mut tracker, &allowed).is_empty(),
                "allowed policy keeps the camera node"
            );
            assert!(
                !harness
                    .nodes("Video/Source", Some("fake-camera"))
                    .is_empty(),
                "camera node survives while allowed"
            );

            // Disable: the existing camera node is destroyed.
            let actions = harness.settle(&mut tracker, &camera_off);
            assert!(
                actions
                    .iter()
                    .any(|action| matches!(action, Action::DestroyNode { .. })),
                "disabling destroys camera nodes, got {actions:?}"
            );
            assert!(
                harness.wait_for(Duration::from_secs(10), || {
                    harness
                        .nodes("Video/Source", Some("fake-camera"))
                        .is_empty()
                }),
                "existing camera node is destroyed while disabled"
            );

            // A camera opened after the policy lands is destroyed
            // too: newly opened streams cannot bypass it.
            assert!(harness.gst(&[
                "videotestsrc",
                "pattern=black",
                "!",
                "pipewiresink",
                "stream-properties=props,media.class=Video/Source,node.name=fake-camera-2",
            ]));
            assert!(harness.wait_for(Duration::from_secs(30), || {
                !harness
                    .nodes("Video/Source", Some("fake-camera-2"))
                    .is_empty()
            }));
            harness.settle(&mut tracker, &camera_off);
            assert!(
                harness.wait_for(Duration::from_secs(10), || {
                    harness
                        .nodes("Video/Source", Some("fake-camera-2"))
                        .is_empty()
                }),
                "newly opened camera node is destroyed while disabled"
            );

            // Explicit restore: a fresh camera node survives.
            harness.settle(&mut tracker, &allowed);
            assert!(harness.gst(&[
                "videotestsrc",
                "pattern=black",
                "!",
                "pipewiresink",
                "stream-properties=props,media.class=Video/Source,node.name=fake-camera-3",
            ]));
            assert!(harness.wait_for(Duration::from_secs(30), || {
                !harness
                    .nodes("Video/Source", Some("fake-camera-3"))
                    .is_empty()
            }));
            assert!(
                harness.settle(&mut tracker, &allowed).is_empty(),
                "restored policy destroys nothing"
            );
            assert!(
                !harness
                    .nodes("Video/Source", Some("fake-camera-3"))
                    .is_empty(),
                "camera node survives after restore"
            );
        }
    }
}
