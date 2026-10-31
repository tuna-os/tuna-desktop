//! `org.gnome.Shell` on the session bus: the part of GNOME Shell's own
//! D-Bus interface that other GNOME components call (shellDBus.js).
//! gnome-settings-daemon's media keys show the volume and brightness OSD
//! through `ShowOSD`, and open search or the app grid through
//! `FocusSearch` and `ShowApplications`. The same connection carries
//! `/org/gnome/Shell/Introspect`, relayed to the compositor
//! (`introspect_forward.rs`), as GNOME Shell serves it under this name.
//!
//! As in GNOME, those methods answer only allowlisted callers (Settings,
//! the media-keys daemon, the GNOME portal backend). Harnesses set
//! `TUNA_SHELL_DBUS_UNRESTRICTED=1`, GNOME's unsafe mode.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;

use gtk4::gio;
use gtk4::glib;
use gtk4::prelude::*;

use crate::osd::OsdRequest;
use glib::translate::IntoGlib;

pub const NAME: &str = "org.gnome.Shell";
pub const PATH: &str = "/org/gnome/Shell";

/// GNOME's DBusSenderChecker allowlist for these methods.
pub const ALLOWED_SENDERS: &[&str] = &[
    "org.gnome.Settings",
    "org.gnome.SettingsDaemon.MediaKeys",
    "org.freedesktop.impl.portal.desktop.gnome",
];

const XML: &str = r#"<node>
  <interface name="org.gnome.Shell">
    <method name="ShowOSD"><arg type="a{sv}" direction="in" name="params"/></method>
    <method name="FocusSearch"/>
    <method name="ShowApplications"/>
    <method name="GrabAccelerator">
      <arg type="s" direction="in" name="accelerator"/>
      <arg type="u" direction="in" name="modeFlags"/>
      <arg type="u" direction="in" name="grabFlags"/>
      <arg type="u" direction="out" name="action"/>
    </method>
    <method name="GrabAccelerators">
      <arg type="a(suu)" direction="in" name="accelerators"/>
      <arg type="au" direction="out" name="actions"/>
    </method>
    <method name="UngrabAccelerator">
      <arg type="u" direction="in" name="action"/>
      <arg type="b" direction="out" name="success"/>
    </method>
    <method name="UngrabAccelerators">
      <arg type="au" direction="in" name="action"/>
      <arg type="b" direction="out" name="success"/>
    </method>
    <signal name="AcceleratorActivated">
      <arg name="action" type="u"/>
      <arg name="parameters" type="a{sv}"/>
    </signal>
    <signal name="AcceleratorDeactivated">
      <arg name="action" type="u"/>
      <arg name="parameters" type="a{sv}"/>
    </signal>
    <property name="Mode" type="s" access="read"/>
    <property name="OverviewActive" type="b" access="read"/>
    <property name="ShellVersion" type="s" access="read"/>
  </interface>
</node>"#;

/// What the methods act on.
pub trait ShellActions {
    fn show_osd(&self, request: &OsdRequest);
    fn focus_search(&self);
    fn show_applications(&self);
    fn overview_active(&self) -> bool;
    /// Hand the compositor the full set of grabs with their flags: its
    /// connection generation and request id, or `None` if nothing was sent.
    fn set_accelerator_grabs(
        &self,
        grabs: Vec<tuna_shell_control::AcceleratorGrab>,
    ) -> Option<AcceleratorRequest>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AcceleratorRequest {
    pub generation: u64,
    pub id: u64,
}

/// Parse a GNOME accelerator string (`<Super>p`, `XF86AudioRaiseVolume`)
/// into the compositor's keysym and modifier bits; `None` when it does
/// not parse or names no key.
pub fn parse_accelerator(accelerator: &str) -> Option<(u32, u32)> {
    // Mutter's syntax: `<Modifier>` tokens, then a key name.
    let mut rest = accelerator.trim();
    let mut bits = 0;
    while let Some(after) = rest.strip_prefix('<') {
        let (token, tail) = after.split_once('>')?;
        bits |= match token.to_ascii_lowercase().as_str() {
            "shift" => tuna_shell_control::MOD_SHIFT,
            "control" | "ctrl" | "primary" => tuna_shell_control::MOD_CTRL,
            "alt" | "mod1" | "meta" => tuna_shell_control::MOD_ALT,
            "super" | "mod4" | "hyper" => tuna_shell_control::MOD_LOGO,
            _ => return None,
        };
        rest = tail;
    }
    if rest.is_empty() {
        return None;
    }
    // Letters grab as their lower case, as GTK and Mutter parse them.
    // Names GDK lacks (XF86Keyboard) come from xkbcommon, as Mutter's.
    let key = match gtk4::gdk::Key::from_name(rest) {
        Some(key) => key.to_lower().into_glib(),
        None => {
            let sym = xkbcommon::xkb::keysym_from_name(rest, xkbcommon::xkb::KEYSYM_NO_FLAGS);
            if sym.raw() == 0 {
                return None;
            }
            sym.raw()
        }
    };
    Some((key, bits))
}

/// One caller's grab.
#[derive(Debug, Clone)]
struct Grab {
    sender: String,
    keysym: u32,
    mods: u32,
    modes: u32,
    /// GNOME's MetaKeyBindingFlags, within `SUPPORTED_GRAB_FLAGS`.
    flags: u32,
}

/// Every grab by action id (Mutter's keybinding actions).
#[derive(Debug, Default)]
struct Grabs {
    next: u32,
    by_action: BTreeMap<u32, Grab>,
    /// Keys the shell binds itself (its own keybindings): not grabbable.
    reserved: Vec<(u32, u32)>,
}

impl Grabs {
    /// Grab `accelerator` for `sender` without GNOME flags.
    #[cfg(test)]
    fn grab(&mut self, sender: &str, accelerator: &str, modes: u32) -> u32 {
        self.grab_with_flags(sender, accelerator, modes, 0)
    }

    /// Grab `accelerator` for `sender` with GNOME's MetaKeyBindingFlags:
    /// the new action id, or 0 when it does not parse, is grabbed
    /// already, or carries an unsupported flag (as Mutter answers).
    fn grab_with_flags(&mut self, sender: &str, accelerator: &str, modes: u32, flags: u32) -> u32 {
        if flags & !tuna_shell_control::SUPPORTED_GRAB_FLAGS != 0 {
            return 0;
        }
        let Some((keysym, mods)) = parse_accelerator(accelerator) else {
            return 0;
        };
        if self.reserved.contains(&(keysym, mods))
            || self
                .by_action
                .values()
                .any(|g| g.keysym == keysym && g.mods == mods)
            || self.by_action.len() >= tuna_shell_control::MAX_ACCELERATORS
        {
            return 0;
        }
        // Never reuse a retired action or enter the shell's private namespace:
        // a queued activation still carries only this numeric identity.
        let Some(next) = self
            .next
            .checked_add(1)
            .filter(|next| *next < INTERNAL_BASE)
        else {
            return 0;
        };
        self.next = next;
        // GNOME's modes; none given means the normal session.
        let modes = if modes == 0 {
            tuna_shell_control::MODE_NORMAL
        } else {
            modes
        };
        self.by_action.insert(
            self.next,
            Grab {
                sender: sender.to_owned(),
                keysym,
                mods,
                modes,
                flags,
            },
        );
        self.next
    }

    /// Release `sender`'s grab of `action`.
    #[cfg(test)]
    fn ungrab(&mut self, sender: &str, action: u32) -> bool {
        match self.by_action.get(&action) {
            Some(g) if g.sender == sender => {
                self.by_action.remove(&action);
                true
            }
            _ => false,
        }
    }

    fn forget(&mut self, sender: &str) -> bool {
        let before = self.by_action.len();
        self.by_action.retain(|_, g| g.sender != sender);
        before != self.by_action.len()
    }

    #[cfg(test)]
    fn list(&self) -> Vec<tuna_shell_control::Accelerator> {
        self.by_action
            .iter()
            .map(|(action, g)| tuna_shell_control::Accelerator {
                action: *action,
                keysym: g.keysym,
                mods: g.mods,
                modes: g.modes,
            })
            .collect()
    }
}

/// Runs one of the shell's own keybindings by index, with the
/// `MODE_*` bit it fired in (panel focus pauses session chords).
type InternalRun = Rc<dyn Fn(usize, u32)>;

/// Action ids of the shell's own keybindings start here, clear of the
/// ids D-Bus callers get.
pub const INTERNAL_BASE: u32 = 0x4000_0000;

/// The running service: the shell reports accelerator presses here.
pub struct Service {
    conn: RefCell<Option<gio::DBusConnection>>,
    grabs: RefCell<Grabs>,
    /// The desired shell generation and the one last acknowledged by the
    /// compositor. Every outbound full table snapshots the desired Rc,
    /// including tables sent for external callers.
    internal: RefCell<Option<Rc<InternalGeneration>>>,
    installed_internal: RefCell<Option<PendingInternal>>,
    /// Monotonic action identities; retired generations are never reused.
    next_internal: Cell<u32>,
    /// Replacements sent but not yet acknowledged. Until then an event
    /// may name either the installed table or one just applied remotely.
    pending_internal: RefCell<VecDeque<PendingInternal>>,
    actions: Rc<dyn ShellActions>,
    /// NameOwnerChanged, to drop a departed caller's grabs.
    watch: RefCell<Option<gio::SignalSubscription>>,
    /// External mutations are serialized and committed only on their own
    /// compositor acknowledgement. Later tables never contain a failed call.
    calls: RefCell<VecDeque<GrabCall>>,
    pending_call: RefCell<Option<PendingCall>>,
    push_deferred: Cell<bool>,
    /// `/org/gnome/Shell/Introspect`, relayed to the compositor.
    introspect: RefCell<Option<crate::introspect_forward::Relay>>,
}

type GrabReply = Box<dyn FnOnce(Result<glib::Variant, ()>)>;

enum GrabOperation {
    Grab(String, u32, u32),
    GrabMany(Vec<(String, u32, u32)>),
    Ungrab(u32),
    UngrabMany(Vec<u32>),
}

struct GrabCall {
    sender: String,
    operation: GrabOperation,
    reply: GrabReply,
}

enum GrabChange {
    Add(BTreeMap<u32, Grab>),
    Remove(Vec<u32>),
}

struct PendingCall {
    request: AcceleratorRequest,
    sender: String,
    change: GrabChange,
    answer: glib::Variant,
    reply: GrabReply,
    cancelled: bool,
}

struct InternalGeneration {
    grabs: Vec<tuna_shell_control::AcceleratorGrab>,
    indices: BTreeMap<u32, usize>,
    run: InternalRun,
}

struct PendingInternal {
    request: AcceleratorRequest,
    generation: Option<Rc<InternalGeneration>>,
}

impl Service {
    fn send_table(&self, change: Option<&GrabChange>) -> Option<AcceleratorRequest> {
        let grabs = self.grabs.borrow();
        let internal = self.internal.borrow().clone();
        let additions = match change {
            Some(GrabChange::Add(additions)) => additions.len(),
            _ => 0,
        };
        let internal_len = internal
            .as_ref()
            .map_or(0, |generation| generation.grabs.len());
        let removed = match change {
            Some(GrabChange::Remove(ids)) => ids.len(),
            _ => 0,
        };
        let mut typed =
            Vec::with_capacity(grabs.by_action.len() - removed + additions + internal_len);
        let mut append = |action, grab: &Grab| {
            typed.push(tuna_shell_control::AcceleratorGrab {
                accelerator: tuna_shell_control::Accelerator {
                    action,
                    keysym: grab.keysym,
                    mods: grab.mods,
                    modes: grab.modes,
                },
                flags: grab.flags,
            });
        };
        for (action, grab) in &grabs.by_action {
            if !matches!(change, Some(GrabChange::Remove(ids)) if ids.binary_search(action).is_ok())
            {
                append(*action, grab);
            }
        }
        if let Some(GrabChange::Add(additions)) = change {
            for (action, grab) in additions {
                append(*action, grab);
            }
        }
        if let Some(generation) = internal.as_ref() {
            typed.extend(generation.grabs.iter().copied());
        }
        let request = self.actions.set_accelerator_grabs(typed)?;
        // A send can itself redial the control client. This path sends only
        // without an external call in flight; retain no old-link callbacks.
        self.pending_internal
            .borrow_mut()
            .retain(|table| table.request.generation == request.generation);
        if self
            .installed_internal
            .borrow()
            .as_ref()
            .is_some_and(|table| table.request.generation != request.generation)
        {
            self.installed_internal.borrow_mut().take();
        }
        self.pending_internal
            .borrow_mut()
            .push_back(PendingInternal {
                request,
                generation: internal,
            });
        Some(request)
    }

    /// Do not let an internal refresh overwrite an in-flight external
    /// proposal, or include it in a later table before its admission.
    fn push(&self) -> Option<AcceleratorRequest> {
        if self.pending_call.borrow().is_some() {
            self.push_deferred.set(true);
            return None;
        }
        self.send_table(None)
    }

    fn prepare(&self, sender: &str, operation: GrabOperation) -> (GrabChange, glib::Variant) {
        let mut grabs = self.grabs.borrow_mut();
        let before = grabs.next;
        let answer = match operation {
            GrabOperation::Grab(accel, modes, flags) => {
                (grabs.grab_with_flags(sender, &accel, modes, flags),).to_variant()
            }
            GrabOperation::GrabMany(list) => {
                let actions: Vec<u32> = list
                    .iter()
                    .map(|(accel, modes, flags)| {
                        grabs.grab_with_flags(sender, accel, *modes, *flags)
                    })
                    .collect();
                (actions,).to_variant()
            }
            GrabOperation::Ungrab(action) => {
                let ok = grabs
                    .by_action
                    .get(&action)
                    .is_some_and(|g| g.sender == sender);
                return (
                    GrabChange::Remove(if ok { vec![action] } else { Vec::new() }),
                    (ok,).to_variant(),
                );
            }
            GrabOperation::UngrabMany(mut actions) => {
                let original_len = actions.len();
                actions.sort_unstable();
                actions.dedup();
                let mut ok = actions.len() == original_len;
                actions.retain(|action| {
                    let owned = grabs
                        .by_action
                        .get(action)
                        .is_some_and(|g| g.sender == sender);
                    ok &= owned;
                    owned
                });
                return (GrabChange::Remove(actions), (ok,).to_variant());
            }
        };
        // Reserve monotonic action ids, but detach the proposal from the
        // committed model until its exact request is acknowledged.
        let additions = grabs.by_action.split_off(&before.saturating_add(1));
        (GrabChange::Add(additions), answer)
    }

    fn queue_call(&self, sender: String, operation: GrabOperation, reply: GrabReply) {
        self.calls.borrow_mut().push_back(GrabCall {
            sender,
            operation,
            reply,
        });
        self.start_next_call();
    }

    fn start_next_call(&self) {
        while self.pending_call.borrow().is_none() {
            let Some(call) = self.calls.borrow_mut().pop_front() else {
                break;
            };
            let (change, answer) = self.prepare(&call.sender, call.operation);
            if let Some(request) = self.send_table(Some(&change)) {
                *self.pending_call.borrow_mut() = Some(PendingCall {
                    request,
                    sender: call.sender,
                    change,
                    answer,
                    reply: call.reply,
                    cancelled: false,
                });
            } else {
                (call.reply)(Err(()));
            }
        }
    }

    fn flush_deferred(&self) {
        if !self.push_deferred.replace(false) {
            return;
        }
        let _ = self.push();
    }

    /// Match only this request's answer. An unrelated later success must
    /// not admit a denied, unanswered, or old-link table.
    /// The compositor handles/writes results in request order on one socket.
    /// Main dispatches a drain's accelerator events before its ordered
    /// results, so both installed and pending snapshots remain resolvable
    /// for those events until their own result retires the old generation.
    pub fn command_result(&self, id: u64, applied: bool) {
        let internal = {
            let mut pending = self.pending_internal.borrow_mut();
            pending
                .iter()
                .position(|table| table.request.id == id)
                .and_then(|i| pending.remove(i))
        };
        if let Some(table) = internal.filter(|_| applied) {
            *self.installed_internal.borrow_mut() = Some(table);
        }
        let call = {
            let mut pending = self.pending_call.borrow_mut();
            if pending.as_ref().is_some_and(|call| call.request.id == id) {
                pending.take()
            } else {
                None
            }
        };
        if let Some(call) = call {
            if applied && !call.cancelled {
                let mut grabs = self.grabs.borrow_mut();
                match call.change {
                    GrabChange::Add(mut additions) => grabs.by_action.append(&mut additions),
                    GrabChange::Remove(actions) => {
                        for action in actions {
                            grabs.by_action.remove(&action);
                        }
                    }
                }
                drop(grabs);
                (call.reply)(Ok(call.answer));
            } else {
                (call.reply)(Err(()));
            }
            self.flush_deferred();
            self.start_next_call();
        }
    }

    fn forget_sender(&self, sender: &str) {
        let changed = self.grabs.borrow_mut().forget(sender);
        let mut cancelled = false;
        if let Some(call) = self.pending_call.borrow_mut().as_mut() {
            if call.sender == sender {
                call.cancelled = true;
                cancelled = true;
            }
        }
        loop {
            let call = {
                let mut calls = self.calls.borrow_mut();
                calls
                    .iter()
                    .position(|call| call.sender == sender)
                    .and_then(|i| calls.remove(i))
            };
            let Some(call) = call else { break };
            (call.reply)(Err(()));
        }
        if changed || cancelled {
            self.push_deferred.set(true);
            if self.pending_call.borrow().is_none() {
                self.flush_deferred();
            }
        }
    }

    /// Observe the live stream on every pump, not only a generation change
    /// within `poll`: an intervening write can already have redialed it.
    /// New-link admissions survive; old-link proposals fail without commit.
    pub fn control_changed(&self, generation: Option<u64>) {
        let mut interrupted = false;
        {
            let mut pending = self.pending_internal.borrow_mut();
            let before = pending.len();
            pending.retain(|table| Some(table.request.generation) == generation);
            interrupted |= before != pending.len();
        }
        {
            let mut installed = self.installed_internal.borrow_mut();
            if installed
                .as_ref()
                .is_some_and(|table| Some(table.request.generation) != generation)
            {
                installed.take();
                interrupted = true;
            }
        }
        let call = {
            let mut pending = self.pending_call.borrow_mut();
            if pending
                .as_ref()
                .is_some_and(|call| Some(call.request.generation) != generation)
            {
                pending.take()
            } else {
                None
            }
        };
        if let Some(call) = call {
            interrupted = true;
            (call.reply)(Err(()));
        }
        if generation.is_none() {
            loop {
                let call = self.calls.borrow_mut().pop_front();
                let Some(call) = call else { break };
                (call.reply)(Err(()));
            }
            self.push_deferred.set(false);
        } else if interrupted {
            self.push_deferred.set(true);
            if self.pending_call.borrow().is_none() {
                self.flush_deferred();
                self.start_next_call();
            }
        }
    }

    pub fn release_replies(&self) {
        self.control_changed(None);
    }

    /// Bind the shell's own keys: `accelerators[i]` (accelerator, modes,
    /// grab flags) runs `run(i, mode)`. Keys that do not parse are skipped.
    pub fn set_internal(&self, accelerators: &[(String, u32, u32)], run: InternalRun) {
        let mut list = Vec::new();
        let mut reserved = Vec::new();
        let mut indices = BTreeMap::new();
        for (i, (accel, modes, flags)) in accelerators.iter().enumerate() {
            if let Some((keysym, mods)) = parse_accelerator(accel) {
                let action = self.next_internal.get();
                let Some(next) = action.checked_add(1) else {
                    // Exhaustion disables new bindings instead of reusing an id.
                    break;
                };
                self.next_internal.set(next);
                reserved.push((keysym, mods));
                indices.insert(action, i);
                list.push(tuna_shell_control::AcceleratorGrab {
                    accelerator: tuna_shell_control::Accelerator {
                        action,
                        keysym,
                        mods,
                        modes: *modes,
                    },
                    flags: *flags,
                });
            }
        }
        self.grabs.borrow_mut().reserved = reserved;
        *self.internal.borrow_mut() = Some(Rc::new(InternalGeneration {
            grabs: list,
            indices,
            run,
        }));
        let _ = self.push();
    }

    /// A grabbed accelerator fired: one of the shell's own runs here;
    /// a caller's is signalled to that caller, as GNOME does (unicast
    /// AcceleratorActivated).
    pub fn accelerator_activated(&self, action: u32, time: u32, mode: u32) {
        self.accelerator_event(action, time, mode, false);
    }

    /// A `GRAB_TRIGGER_RELEASE` grab's key was released: signalled to
    /// its caller as AcceleratorDeactivated. The shell's own bindings
    /// act on press only.
    pub fn accelerator_deactivated(&self, action: u32, time: u32, mode: u32) {
        self.accelerator_event(action, time, mode, true);
    }

    fn accelerator_event(&self, action: u32, time: u32, mode: u32, released: bool) {
        if action >= INTERNAL_BASE {
            if released {
                return;
            }
            let find = |generation: &InternalGeneration| {
                generation
                    .indices
                    .get(&action)
                    .map(|index| (*index, generation.run.clone()))
            };
            let handler = self
                .installed_internal
                .borrow()
                .as_ref()
                .and_then(|table| table.generation.as_deref().and_then(find))
                .or_else(|| {
                    self.pending_internal
                        .borrow()
                        .iter()
                        .find_map(|table| table.generation.as_deref().and_then(find))
                });
            if let Some((index, run)) = handler {
                run(index, mode);
            }
            return;
        }
        let Some(sender) = self
            .grabs
            .borrow()
            .by_action
            .get(&action)
            .map(|g| g.sender.clone())
        else {
            return;
        };
        let Some(conn) = self.conn.borrow().clone() else {
            return;
        };
        let _ = conn.emit_signal(
            Some(&sender),
            PATH,
            "org.gnome.Shell",
            if released {
                "AcceleratorDeactivated"
            } else {
                "AcceleratorActivated"
            },
            Some(&accelerator_signal_body(action, time, mode)),
        );
    }
}

/// The `(ua{sv})` body of AcceleratorActivated/AcceleratorDeactivated;
/// a `(u32, Variant)` tuple would box the dictionary as `(uv)`, which
/// GNOME's portal drops.
fn accelerator_signal_body(action: u32, time: u32, mode: u32) -> glib::Variant {
    let params = glib::VariantDict::new(None);
    params.insert("timestamp", time);
    params.insert("action-mode", mode);
    params.insert("device-node", "");
    params.insert("device-id", 0u32);
    glib::Variant::tuple_from_iter([action.to_variant(), params.end()])
}

/// Unpack ShowOSD's `a{sv}` the way shellDBus.js does.
pub fn parse_osd(params: &glib::Variant) -> OsdRequest {
    let dict = glib::VariantDict::new(Some(params));
    OsdRequest {
        icon: dict.lookup::<String>("icon").ok().flatten(),
        label: dict.lookup::<String>("label").ok().flatten(),
        level: dict.lookup::<f64>("level").ok().flatten(),
        max_level: dict.lookup::<f64>("max_level").ok().flatten(),
    }
}

/// Whether `sender` (a unique bus name) owns one of the allowlisted names.
fn sender_allowed(conn: &gio::DBusConnection, sender: Option<&str>) -> bool {
    if std::env::var_os("TUNA_SHELL_DBUS_UNRESTRICTED").is_some_and(|v| v == "1") {
        return true;
    }
    let Some(sender) = sender else {
        return false;
    };
    ALLOWED_SENDERS.iter().any(|name| {
        conn.call_sync(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "GetNameOwner",
            Some(&(*name,).to_variant()),
            Some(glib::VariantTy::new("(s)").unwrap()),
            gio::DBusCallFlags::NONE,
            500,
            gio::Cancellable::NONE,
        )
        .ok()
        .and_then(|v| v.get::<(String,)>())
        .is_some_and(|(owner,)| owner == sender)
    })
}

/// Own `org.gnome.Shell` and serve it for the session. Another owner
/// (a real GNOME Shell) keeps the name.
pub fn start(actions: Rc<dyn ShellActions>) -> Rc<Service> {
    let service = Rc::new(Service {
        conn: RefCell::new(None),
        grabs: RefCell::new(Grabs::default()),
        internal: RefCell::new(None),
        next_internal: Cell::new(INTERNAL_BASE),
        installed_internal: RefCell::new(None),
        pending_internal: RefCell::new(VecDeque::new()),
        actions: actions.clone(),
        watch: RefCell::new(None),
        calls: RefCell::new(VecDeque::new()),
        pending_call: RefCell::new(None),
        push_deferred: Cell::new(false),
        introspect: RefCell::new(None),
    });
    let node = match gio::DBusNodeInfo::for_xml(XML) {
        Ok(node) => node,
        Err(e) => {
            eprintln!("tuna-shell-gtk: org.gnome.Shell interface: {e}");
            return service;
        }
    };
    let Some(info) = node.lookup_interface("org.gnome.Shell") else {
        return service;
    };
    let svc = service.clone();
    let id = gio::bus_own_name(
        gio::BusType::Session,
        NAME,
        gio::BusNameOwnerFlags::NONE,
        move |conn, _| {
            *svc.conn.borrow_mut() = Some(conn.clone());
            let calls = actions.clone();
            let props = actions.clone();
            let grabs_svc = svc.clone();
            let registered = conn
                .register_object(PATH, &info)
                .method_call(move |conn, sender, _, _, method, params, invocation| {
                    if !sender_allowed(&conn, sender) {
                        invocation.return_dbus_error(
                            "org.freedesktop.DBus.Error.AccessDenied",
                            &format!("{method} is not allowed"),
                        );
                        return;
                    }
                    let sender = sender.unwrap_or_default().to_owned();
                    let mut operation = None;
                    let reply = match method {
                        "ShowOSD" => {
                            let request = params
                                .try_child_value(0)
                                .map(|p| parse_osd(&p))
                                .unwrap_or_default();
                            calls.show_osd(&request);
                            None
                        }
                        "FocusSearch" => {
                            calls.focus_search();
                            None
                        }
                        "ShowApplications" => {
                            calls.show_applications();
                            None
                        }
                        "GrabAccelerator" => {
                            let Some((accel, modes, flags)) = params.get::<(String, u32, u32)>()
                            else {
                                invocation.return_dbus_error(
                                    "org.freedesktop.DBus.Error.InvalidArgs",
                                    method,
                                );
                                return;
                            };
                            operation = Some(GrabOperation::Grab(accel, modes, flags));
                            None
                        }
                        "GrabAccelerators" => {
                            let Some((list,)) = params.get::<(Vec<(String, u32, u32)>,)>() else {
                                invocation.return_dbus_error(
                                    "org.freedesktop.DBus.Error.InvalidArgs",
                                    method,
                                );
                                return;
                            };
                            operation = Some(GrabOperation::GrabMany(list));
                            None
                        }
                        "UngrabAccelerator" => {
                            let action = params.get::<(u32,)>().map(|(a,)| a).unwrap_or(0);
                            operation = Some(GrabOperation::Ungrab(action));
                            None
                        }
                        "UngrabAccelerators" => {
                            let list = params
                                .get::<(Vec<u32>,)>()
                                .map(|(l,)| l)
                                .unwrap_or_default();
                            operation = Some(GrabOperation::UngrabMany(list));
                            None
                        }
                        _ => {
                            invocation.return_dbus_error(
                                "org.freedesktop.DBus.Error.UnknownMethod",
                                method,
                            );
                            return;
                        }
                    };
                    if let Some(operation) = operation {
                        grabs_svc.queue_call(
                            sender,
                            operation,
                            Box::new(move |result| match result {
                                Ok(reply) => invocation.return_value(Some(&reply)),
                                Err(()) => invocation.return_dbus_error(
                                    "org.gnome.Shell.Error.Failed",
                                    "The compositor did not install the accelerator table",
                                ),
                            }),
                        );
                    } else {
                        invocation.return_value(reply.as_ref());
                    }
                })
                .property(move |_, _, _, _, property| match property {
                    "Mode" => "user".to_variant(),
                    "OverviewActive" => props.overview_active().to_variant(),
                    "ShellVersion" => "51.0".to_variant(),
                    _ => ().to_variant(),
                })
                .build();
            if let Err(e) = registered {
                eprintln!("tuna-shell-gtk: org.gnome.Shell object: {e}");
            }
            // A caller that leaves the bus loses its grabs, as in GNOME.
            let leave_svc = svc.clone();
            let weak = Rc::downgrade(&leave_svc);
            let subscription = conn.subscribe_to_signal(
                Some("org.freedesktop.DBus"),
                Some("org.freedesktop.DBus"),
                Some("NameOwnerChanged"),
                Some("/org/freedesktop/DBus"),
                None,
                gio::DBusSignalFlags::NONE,
                move |signal| {
                    let Some(svc) = weak.upgrade() else { return };
                    if let Some((name, _, new_owner)) =
                        signal.parameters.get::<(String, String, String)>()
                    {
                        if new_owner.is_empty() {
                            svc.forget_sender(&name);
                        }
                    }
                },
            );
            *leave_svc.watch.borrow_mut() = Some(subscription);
            match crate::introspect_forward::register(&conn) {
                Ok(relay) => *leave_svc.introspect.borrow_mut() = Some(relay),
                Err(e) => eprintln!("tuna-shell-gtk: org.gnome.Shell Introspect relay: {e}"),
            }
        },
        |_, _| {},
        |_, _| eprintln!("tuna-shell-gtk: org.gnome.Shell is owned elsewhere"),
    );
    // The name is held for the session: an OwnerId releases nothing
    // when it drops.
    let _ = id;
    service
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn show_osd_params_unpack_like_gnome() {
        let dict = glib::VariantDict::new(None);
        dict.insert("icon", "audio-volume-medium-symbolic");
        dict.insert("level", 0.5f64);
        dict.insert("max_level", 1.5f64);
        let request = parse_osd(&dict.end());
        assert_eq!(
            request,
            OsdRequest {
                icon: Some("audio-volume-medium-symbolic".into()),
                label: None,
                level: Some(0.5),
                max_level: Some(1.5),
            }
        );
        let dict = glib::VariantDict::new(None);
        dict.insert("label", "English (US)");
        let request = parse_osd(&dict.end());
        assert_eq!(request.label.as_deref(), Some("English (US)"));
        assert_eq!(request.icon, None);
    }

    #[test]
    fn accelerator_signal_body_matches_the_introspected_signature() {
        let body = accelerator_signal_body(7, 1234, 2);
        assert_eq!(body.type_().as_str(), "(ua{sv})");
        assert_eq!(body.child_value(0).get::<u32>(), Some(7));
        let params = glib::VariantDict::new(Some(&body.child_value(1)));
        assert_eq!(params.lookup::<u32>("timestamp").ok().flatten(), Some(1234));
        assert_eq!(params.lookup::<u32>("action-mode").ok().flatten(), Some(2));
    }

    #[test]
    fn accelerators_parse_like_gnome() {
        let (vol, mods) = parse_accelerator("XF86AudioRaiseVolume").unwrap();
        assert_eq!((vol, mods), (0x1008FF13, 0));
        let (p, mods) = parse_accelerator("<Super>p").unwrap();
        assert_eq!(p, u32::from(b'p'));
        assert_eq!(mods, tuna_shell_control::MOD_LOGO);
        let (_, mods) = parse_accelerator("<Primary><Alt>Delete").unwrap();
        assert_eq!(
            mods,
            tuna_shell_control::MOD_CTRL | tuna_shell_control::MOD_ALT
        );
        assert!(parse_accelerator("not a key").is_none());
    }

    #[test]
    fn grabs_belong_to_their_caller() {
        let mut grabs = Grabs::default();
        let a = grabs.grab(":1.5", "XF86AudioMute", 0);
        assert!(a > 0);
        // The same combination cannot be grabbed twice.
        assert_eq!(grabs.grab(":1.6", "XF86AudioMute", 0), 0);
        assert_eq!(grabs.grab(":1.6", "garbage", 0), 0);
        assert!(!grabs.ungrab(":1.6", a), "only the owner ungrabs");
        let list = grabs.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].modes, tuna_shell_control::MODE_NORMAL);
        assert!(grabs.forget(":1.5"));
        assert!(grabs.list().is_empty());
    }

    #[test]
    fn external_actions_never_reuse_or_enter_internal_namespace() {
        let mut grabs = Grabs {
            next: INTERNAL_BASE - 2,
            ..Grabs::default()
        };
        let last = grabs.grab(":1.5", "XF86AudioMute", 0);
        assert_eq!(last, INTERNAL_BASE - 1);
        assert!(grabs.ungrab(":1.5", last));
        assert_eq!(grabs.grab(":1.6", "XF86AudioMute", 0), 0);
        assert!(grabs.list().is_empty());
        grabs.next = u32::MAX;
        assert_eq!(grabs.grab(":1.6", "XF86AudioMute", 0), 0);
    }

    /// Records the last table sent and numbers each send like the
    /// control client's request ids.
    #[derive(Default)]
    struct RecordedActions(
        RefCell<Vec<tuna_shell_control::Accelerator>>,
        Cell<u64>,
        Cell<bool>,
        Cell<u64>,
    );

    impl ShellActions for RecordedActions {
        fn show_osd(&self, _: &OsdRequest) {}
        fn focus_search(&self) {}
        fn show_applications(&self) {}
        fn overview_active(&self) -> bool {
            false
        }
        fn set_accelerator_grabs(
            &self,
            grabs: Vec<tuna_shell_control::AcceleratorGrab>,
        ) -> Option<AcceleratorRequest> {
            self.1.set(self.1.get() + 1);
            if self.2.get() {
                return None;
            }
            *self.0.borrow_mut() = grabs.into_iter().map(|g| g.accelerator).collect();
            Some(AcceleratorRequest {
                generation: self.3.get(),
                id: self.1.get(),
            })
        }
    }

    fn test_service(actions: Rc<RecordedActions>) -> Service {
        Service {
            conn: RefCell::new(None),
            grabs: RefCell::new(Grabs::default()),
            internal: RefCell::new(None),
            next_internal: Cell::new(INTERNAL_BASE),
            installed_internal: RefCell::new(None),
            pending_internal: RefCell::new(VecDeque::new()),
            actions,
            watch: RefCell::new(None),
            calls: RefCell::new(VecDeque::new()),
            pending_call: RefCell::new(None),
            push_deferred: Cell::new(false),
            introspect: RefCell::new(None),
        }
    }

    type CallResult = Rc<RefCell<Option<Result<glib::Variant, ()>>>>;

    fn queued(service: &Service, operation: GrabOperation) -> CallResult {
        let result = Rc::new(RefCell::new(None));
        let reply = result.clone();
        service.queue_call(
            ":1.5".into(),
            operation,
            Box::new(move |r| {
                assert!(
                    reply.borrow_mut().replace(r).is_none(),
                    "call answered twice"
                );
            }),
        );
        result
    }

    fn play_grab() -> GrabOperation {
        GrabOperation::Grab("XF86AudioPlay".into(), tuna_shell_control::MODE_NORMAL, 0)
    }

    #[test]
    fn grab_replies_wait_for_their_own_ack_and_do_not_leak_denied_mutations() {
        let actions = Rc::new(RecordedActions::default());
        let service = test_service(actions.clone());
        let first = queued(&service, play_grab());
        let second = queued(&service, GrabOperation::Grab("XF86AudioNext".into(), 1, 0));
        assert!(first.borrow().is_none());
        assert!(second.borrow().is_none());
        assert_eq!(
            actions.1.get(),
            1,
            "second proposal must wait for the first"
        );
        assert!(
            service.grabs.borrow().by_action.is_empty(),
            "unacknowledged model was committed"
        );
        service.command_result(99, true);
        assert!(
            first.borrow().is_none(),
            "unrelated later success admitted the grab"
        );
        service.command_result(1, true);
        assert_eq!(
            first
                .borrow()
                .as_ref()
                .unwrap()
                .as_ref()
                .unwrap()
                .get::<(u32,)>(),
            Some((1,))
        );
        assert_eq!(actions.1.get(), 2);
        service.command_result(2, false);
        assert!(second.borrow().as_ref().unwrap().is_err());
        assert_eq!(service.grabs.borrow().by_action.len(), 1);
        assert!(
            !service.grabs.borrow().by_action.contains_key(&2),
            "denied action still has an activation recipient"
        );
        let _ = service.push();
        assert_eq!(
            actions.0.borrow().len(),
            1,
            "a later table installed a failed grab"
        );
        assert_eq!(actions.0.borrow()[0].action, 1);
    }

    #[test]
    fn lost_or_unsent_grabs_fail_and_cannot_reappear_in_later_tables() {
        let actions = Rc::new(RecordedActions::default());
        let service = test_service(actions.clone());
        actions.2.set(true);
        let unsent = queued(&service, play_grab());
        assert!(unsent.borrow().as_ref().unwrap().is_err());
        actions.2.set(false);
        let pending = queued(&service, play_grab());
        let waiting = queued(&service, GrabOperation::Grab("XF86AudioNext".into(), 1, 0));
        service.release_replies();
        assert!(pending.borrow().as_ref().unwrap().is_err());
        assert!(waiting.borrow().as_ref().unwrap().is_err());
        service.command_result(2, true);
        let _ = service.push();
        assert!(
            actions.0.borrow().is_empty(),
            "old-link or unsent actions leaked into a later push"
        );
        assert!(service.grabs.borrow().by_action.is_empty());
        let retry = queued(&service, play_grab());
        let request = actions.1.get();
        service.command_result(request, true);
        assert_eq!(
            retry
                .borrow()
                .as_ref()
                .unwrap()
                .as_ref()
                .unwrap()
                .get::<(u32,)>(),
            Some((3,)),
            "a failed admission must not recycle its action identity"
        );
    }

    #[test]
    fn denied_ungrab_preserves_its_installed_model_and_later_activation_recipient() {
        let actions = Rc::new(RecordedActions::default());
        let service = test_service(actions.clone());
        let grab = queued(&service, play_grab());
        service.command_result(1, true);
        assert!(grab.borrow().as_ref().unwrap().is_ok());
        let ungrab = queued(&service, GrabOperation::Ungrab(1));
        assert!(ungrab.borrow().is_none());
        assert!(service.grabs.borrow().by_action.contains_key(&1));
        service.command_result(2, false);
        assert!(ungrab.borrow().as_ref().unwrap().is_err());
        let _ = service.push();
        assert_eq!(actions.0.borrow()[0].action, 1);
        assert_eq!(service.grabs.borrow().by_action[&1].sender, ":1.5");
        let ungrab = queued(&service, GrabOperation::UngrabMany(vec![1]));
        service.command_result(actions.1.get(), true);
        assert_eq!(
            ungrab
                .borrow()
                .as_ref()
                .unwrap()
                .as_ref()
                .unwrap()
                .get::<(bool,)>(),
            Some((true,))
        );
        let _ = service.push();
        assert!(actions.0.borrow().is_empty());
        assert!(service.grabs.borrow().by_action.is_empty());
    }

    #[test]
    fn internal_refresh_during_a_denied_external_grab_never_publishes_the_proposal() {
        let actions = Rc::new(RecordedActions::default());
        let service = test_service(actions.clone());
        let grab = queued(&service, play_grab());
        service.set_internal(
            &[("<Super>n".into(), tuna_shell_control::MODE_NORMAL, 0)],
            Rc::new(|_, _| {}),
        );
        assert_eq!(
            actions.1.get(),
            1,
            "internal refresh overwrote a pending external admission"
        );
        service.command_result(1, false);
        assert!(grab.borrow().as_ref().unwrap().is_err());
        assert_eq!(actions.1.get(), 2);
        assert_eq!(actions.0.borrow().len(), 1);
        assert!(actions.0.borrow()[0].action >= INTERNAL_BASE);
        assert!(service.grabs.borrow().by_action.is_empty());
    }

    #[test]
    fn external_table_snapshots_a_previously_denied_or_unsent_internal_generation() {
        for unsent in [false, true] {
            let actions = Rc::new(RecordedActions::default());
            let service = test_service(actions.clone());
            let old_calls = Rc::new(Cell::new(0));
            let calls = old_calls.clone();
            service.set_internal(
                &[("<Super>n".into(), tuna_shell_control::MODE_NORMAL, 0)],
                Rc::new(move |_, _| calls.set(calls.get() + 1)),
            );
            service.command_result(1, true);
            let old = actions.0.borrow()[0].action;
            let new_calls = Rc::new(Cell::new(0));
            let calls = new_calls.clone();
            actions.2.set(unsent);
            service.set_internal(
                &[("<Super>n".into(), tuna_shell_control::MODE_NORMAL, 0)],
                Rc::new(move |_, _| calls.set(calls.get() + 1)),
            );
            if !unsent {
                service.command_result(2, false);
            }
            actions.2.set(false);
            let grab = queued(&service, play_grab());
            assert!(grab.borrow().is_none());
            let new = actions
                .0
                .borrow()
                .iter()
                .find(|a| a.action >= INTERNAL_BASE)
                .unwrap()
                .action;
            let desired = service.internal.borrow().clone().unwrap();
            let pending = service.pending_internal.borrow();
            let snapshot = pending
                .iter()
                .find(|table| table.request.id == 3)
                .and_then(|table| table.generation.as_ref())
                .unwrap();
            assert!(
                Rc::ptr_eq(&desired, snapshot),
                "outbound table copied or omitted its generation"
            );
            drop(pending);
            // Control events are grouped before results in Drained, including
            // an event from the just-applied table in the same socket drain.
            service.accelerator_activated(new, 1, tuna_shell_control::MODE_NORMAL);
            assert_eq!(
                new_calls.get(),
                1,
                "new internal identity had no callback before its own ack (unsent={unsent})"
            );
            service.accelerator_activated(old, 2, tuna_shell_control::MODE_NORMAL);
            assert_eq!(old_calls.get(), 1);
            service.command_result(3, true);
            assert!(grab.borrow().as_ref().unwrap().is_ok());
            service.accelerator_activated(old, 3, tuna_shell_control::MODE_NORMAL);
            assert_eq!(
                old_calls.get(),
                1,
                "acknowledged replacement retained the old callback"
            );
            service.accelerator_activated(new, 4, tuna_shell_control::MODE_NORMAL);
            assert_eq!(new_calls.get(), 2);
        }
    }

    #[test]
    fn departed_caller_cannot_acquire_a_pending_grab() {
        let actions = Rc::new(RecordedActions::default());
        let service = test_service(actions.clone());
        let grab = queued(&service, play_grab());
        let waiting = queued(&service, play_grab());
        service.forget_sender(":1.5");
        assert!(waiting.borrow().as_ref().unwrap().is_err());
        service.command_result(1, true);
        assert!(grab.borrow().as_ref().unwrap().is_err());
        assert!(
            actions.0.borrow().is_empty(),
            "departed owner's remote proposal was not retired"
        );
        assert!(service.grabs.borrow().by_action.is_empty());
    }

    #[test]
    fn redial_between_pumps_fails_old_admission_but_preserves_new_link_calls() {
        let actions = Rc::new(RecordedActions::default());
        let service = test_service(actions.clone());
        service.set_internal(
            &[("<Super>n".into(), tuna_shell_control::MODE_NORMAL, 0)],
            Rc::new(|_, _| {}),
        );
        service.command_result(1, true);
        let old_call = queued(&service, play_grab()); // request 2, generation 0
        let new_call = queued(&service, GrabOperation::Grab("XF86AudioNext".into(), 1, 0));
        // Another control command redials between pumps. The pump sees an
        // already-new generation both before and after draining.
        actions.3.set(1);
        service.control_changed(Some(1));
        assert!(old_call.borrow().as_ref().unwrap().is_err());
        assert!(new_call.borrow().is_none());
        let new_request = service.pending_call.borrow().as_ref().unwrap().request;
        assert_eq!(
            new_request,
            AcceleratorRequest {
                generation: 1,
                id: 4
            }
        );
        service.control_changed(Some(1));
        assert!(
            new_call.borrow().is_none(),
            "new-link request was cancelled with the old one"
        );
        service.command_result(2, true);
        assert!(
            new_call.borrow().is_none(),
            "old-link ack admitted the new request"
        );
        service.command_result(3, true); // internal restoration, not the external call
        assert!(new_call.borrow().is_none());
        service.command_result(4, true);
        assert_eq!(
            new_call
                .borrow()
                .as_ref()
                .unwrap()
                .as_ref()
                .unwrap()
                .get::<(u32,)>(),
            Some((2,))
        );
        assert!(!service.grabs.borrow().by_action.contains_key(&1));
        assert!(service.grabs.borrow().by_action.contains_key(&2));
    }

    #[test]
    fn a_send_that_redials_snapshots_the_new_stream_before_the_next_pump() {
        let actions = Rc::new(RecordedActions::default());
        let service = test_service(actions.clone());
        service.set_internal(
            &[("<Super>n".into(), tuna_shell_control::MODE_NORMAL, 0)],
            Rc::new(|_, _| {}),
        );
        service.command_result(1, true);
        let calls = Rc::new(Cell::new(0));
        let fired = calls.clone();
        actions.3.set(1);
        service.set_internal(
            &[("<Super>n".into(), tuna_shell_control::MODE_NORMAL, 0)],
            Rc::new(move |_, _| fired.set(fired.get() + 1)),
        );
        let new = actions.0.borrow()[0].action;
        service.control_changed(Some(1));
        assert_eq!(
            actions.1.get(),
            2,
            "new-link snapshot was needlessly replayed"
        );
        service.accelerator_activated(new, 1, tuna_shell_control::MODE_NORMAL);
        assert_eq!(
            calls.get(),
            1,
            "new snapshot was cancelled before its own ack"
        );
        service.command_result(2, true);
        service.accelerator_activated(new, 2, tuna_shell_control::MODE_NORMAL);
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn internal_rebind_keeps_installed_actions_until_replacement_is_applied() {
        let actions = Rc::new(RecordedActions::default());
        let service = test_service(actions.clone());
        let old_calls = Rc::new(Cell::new(0));
        let calls = old_calls.clone();
        service.set_internal(
            &[("<Super>n".into(), tuna_shell_control::MODE_NORMAL, 0)],
            Rc::new(move |_, _| calls.set(calls.get() + 1)),
        );
        service.command_result(1, true);
        let old = actions.0.borrow()[0].action;
        let new_calls = Rc::new(Cell::new(0));
        let calls = new_calls.clone();
        service.set_internal(
            &[("<Super>n".into(), tuna_shell_control::MODE_NORMAL, 0)],
            Rc::new(move |_, _| calls.set(calls.get() + 1)),
        );
        let new = actions.0.borrow()[0].action;
        // The control frame is queued: input still meets the old table.
        service.accelerator_activated(old, 1, tuna_shell_control::MODE_NORMAL);
        assert_eq!(
            old_calls.get(),
            1,
            "the installed binding disappeared before acknowledgement"
        );
        // Applied and an activation can be read in one drain. Main dispatches
        // activations before results, so the new identity must also resolve.
        service.accelerator_activated(new, 2, tuna_shell_control::MODE_NORMAL);
        assert_eq!(new_calls.get(), 1);
        service.command_result(2, true);
        service.accelerator_activated(old, 3, tuna_shell_control::MODE_NORMAL);
        assert_eq!(
            old_calls.get(),
            1,
            "retired identity survived its acknowledged replacement"
        );
        service.accelerator_activated(new, 4, tuna_shell_control::MODE_NORMAL);
        assert_eq!(new_calls.get(), 2);
    }

    #[test]
    fn retired_internal_actions_cannot_call_a_rebound_callback() {
        let actions = Rc::new(RecordedActions::default());
        let service = test_service(actions.clone());
        let old_calls = Rc::new(RefCell::new(Vec::new()));
        let calls = old_calls.clone();
        service.set_internal(
            &[(
                "XF86AudioRaiseVolume".into(),
                tuna_shell_control::MODE_NORMAL,
                0,
            )],
            Rc::new(move |index, mode| calls.borrow_mut().push((index, mode))),
        );
        service.command_result(1, true);
        let retired = actions.0.borrow()[0].action;
        service.accelerator_activated(retired, 1, tuna_shell_control::MODE_NORMAL);
        assert_eq!(old_calls.borrow().len(), 1);

        let new_calls = Rc::new(RefCell::new(Vec::new()));
        let calls = new_calls.clone();
        service.set_internal(
            &[
                ("not a key".into(), tuna_shell_control::MODE_NORMAL, 0),
                ("XF86AudioMute".into(), tuna_shell_control::MODE_NORMAL, 0),
            ],
            Rc::new(move |index, mode| calls.borrow_mut().push((index, mode))),
        );
        let current = actions.0.borrow()[0].action;
        assert_ne!(retired, current);
        service.command_result(2, true);
        service.accelerator_activated(retired, 2, tuna_shell_control::MODE_NORMAL);
        service.accelerator_activated(u32::MAX, 3, tuna_shell_control::MODE_NORMAL);
        assert!(new_calls.borrow().is_empty());
        assert_eq!(old_calls.borrow().len(), 1);
        service.accelerator_activated(current, 4, tuna_shell_control::MODE_NORMAL);
        assert_eq!(*new_calls.borrow(), [(1, tuna_shell_control::MODE_NORMAL)]);

        service.set_internal(&[], Rc::new(|_, _| panic!("disabled binding ran")));
        service.command_result(3, true);
        service.accelerator_activated(current, 5, tuna_shell_control::MODE_NORMAL);
        assert!(actions.0.borrow().is_empty());
    }

    #[test]
    fn internal_action_exhaustion_does_not_wrap_or_keep_old_bindings() {
        let actions = Rc::new(RecordedActions::default());
        let service = test_service(actions.clone());
        service.next_internal.set(u32::MAX - 1);
        service.set_internal(
            &[
                ("XF86AudioMute".into(), tuna_shell_control::MODE_NORMAL, 0),
                (
                    "XF86AudioRaiseVolume".into(),
                    tuna_shell_control::MODE_NORMAL,
                    0,
                ),
            ],
            Rc::new(|_, _| {}),
        );
        assert_eq!(actions.0.borrow().len(), 1);
        assert_eq!(actions.0.borrow()[0].action, u32::MAX - 1);
        service.set_internal(
            &[("XF86AudioMute".into(), tuna_shell_control::MODE_NORMAL, 0)],
            Rc::new(|_, _| panic!("exhausted binding ran")),
        );
        assert!(actions.0.borrow().is_empty());
        service.command_result(2, true);
        service.accelerator_activated(u32::MAX - 1, 1, tuna_shell_control::MODE_NORMAL);
    }
}

#[cfg(test)]
mod grab_flag_tests {
    use super::*;

    #[test]
    fn unsupported_flags_return_zero_without_installing_and_reply_indexes_remain_stable() {
        let mut grabs = Grabs::default();
        let requests = [("r", 128), ("s", 8), ("t", 16), ("r", 128), ("u", 0)];
        let actions: Vec<_> = requests
            .iter()
            .map(|(key, flags)| grabs.grab_with_flags(":1.70", key, 1, *flags))
            .collect();
        assert_eq!(actions, [1, 0, 2, 0, 3]);
        assert_eq!(grabs.by_action.len(), 3);
        assert!(!grabs
            .by_action
            .values()
            .any(|grab| grab.keysym == u32::from(b's')));
        assert_eq!(grabs.by_action[&1].flags, 128);
        for flags in [1, 2, 4, 8, 32, 64, 256, u32::MAX] {
            assert_eq!(grabs.grab_with_flags(":1.71", "v", 1, flags), 0);
        }
    }

    #[test]
    fn exhausted_action_ids_do_not_recycle_into_old_or_internal_owners() {
        let mut grabs = Grabs {
            next: INTERNAL_BASE - 1,
            ..Default::default()
        };
        assert_eq!(grabs.grab_with_flags(":1.72", "r", 1, 128), 0);
        assert!(grabs.by_action.is_empty());
    }
}
