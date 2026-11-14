//! `org.gnome.Mutter.InputCapture` (#1040): the private interface
//! xdg-desktop-portal-gnome's InputCapture portal drives (Input Leap,
//! Deskflow). Follows mutter 51's `meta-input-capture.c`,
//! `meta-input-capture-session.c` and
//! `data/dbus-interfaces/org.gnome.Mutter.InputCapture.xml`:
//!
//! - a session is created by the installed portal only (it owns the
//!   user's consent), is unusable while locked, and is closed on lock or
//!   when its owner leaves the bus;
//! - `AddBarrier` accepts a line on exactly one outer monitor edge,
//!   validated as mutter's `check_barrier` against the zones of the
//!   session's current `GetZones` serial;
//! - after `Enable`, relative pointer motion crossing a barrier outward
//!   activates the session ([`Router`]): `Activated` goes out with the
//!   barrier, a new activation id and the crossing point, the pointer
//!   stays put, and pointer and keyboard input reach no client but the
//!   session's EIS receiver (`capture_eis.rs`) until `Release`,
//!   `Disable`, `Close`, a monitor change, lock, or the cancel shortcut
//!   (mutter's `cancel-input-capture` default, Super+Shift+Escape).
//!
//! The state machine ([`SessionState`]) is pure and returns the
//! [`Effect`]s each transition has; [`CaptureShared`] applies them
//! (signals through the non-blocking [`SignalQueue`], device commands to
//! the EIS worker) under the session lock, so the D-Bus thread, the
//! compositor loop and the EIS worker see one order of events.
use super::{OutputSnapshot, Outputs, ToLoop};
use crate::dbus_signals::SignalQueue;
use crate::windows::ManagerInput;
use smithay::utils::{Logical, Point};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::future::Future;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use zbus::object_server::SignalEmitter;
use zbus::{fdo, interface, ObjectServer, OwnedObjectPath, OwnedVariant, Variant};

/// `SupportedCapabilities` as the XML documents it: 1 KEYBOARD,
/// 2 POINTER (4 TOUCH is not captured, as mutter's event routing
/// captures no touch either).
pub(crate) const SUPPORTED_CAPABILITIES: u32 = 1 | 2;
/// Sessions alive at once, as RemoteDesktop bounds its grants.
const MAX_SESSIONS: usize = 64;
/// Barriers per session: one per edge of many monitors, with room.
const MAX_BARRIERS: usize = 1024;

pub(crate) type CaptureSessions = Arc<Mutex<HashMap<u64, Arc<CaptureShared>>>>;
/// `GetZones`' `a(uuii)`: width, height, x, y per logical monitor.
pub(crate) type DbusZones = Vec<(u32, u32, i32, i32)>;

pub(crate) fn session_path(id: u64) -> String {
    format!("/org/gnome/Mutter/InputCapture/Session/u{id}")
}

/// One logical monitor as `GetZones` lists it (`(uuii)`: width,
/// height, x, y), with the scale its EIS pointer region carries.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Zone {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub scale: f64,
}

/// The monitor layout a zones serial describes.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Zones {
    pub rects: Vec<Zone>,
    /// Index of the primary monitor in `rects`.
    pub primary: usize,
}

impl Zones {
    /// Logical monitors in global logical coordinates, rotation-aware.
    pub(crate) fn from_outputs(outputs: &[OutputSnapshot]) -> Self {
        let rects = outputs
            .iter()
            .map(|o| {
                let (width, height) =
                    crate::orientation::logical_extent(o.width, o.height, o.scale, o.transform);
                Zone {
                    x: o.x,
                    y: o.y,
                    width,
                    height,
                    scale: o.scale,
                }
            })
            .collect();
        let primary = outputs.iter().position(|o| o.primary).unwrap_or(0);
        Self { rects, primary }
    }

    fn snapshot(outputs: &Outputs) -> Self {
        outputs
            .lock()
            .map(|outputs| Self::from_outputs(&outputs))
            .unwrap_or_default()
    }

    fn dbus(&self) -> DbusZones {
        self.rects
            .iter()
            .map(|z| (z.width.max(0) as u32, z.height.max(0) as u32, z.x, z.y))
            .collect()
    }
}

/// The side of the barrier no monitor is on: where crossing captures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outward {
    Left,
    Right,
    Up,
    Down,
}

/// mutter's `LineAdjacency`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Adjacency {
    None,
    Overlap,
    Contained(Outward),
    Partial,
}

/// mutter's `get_barrier_adjacency`, in 64-bit arithmetic.
fn adjacency(rect: &Zone, (x1, y1, x2, y2): (i32, i32, i32, i32)) -> Adjacency {
    let (x1, y1, x2, y2) = (i64::from(x1), i64::from(y1), i64::from(x2), i64::from(y2));
    let (rx, ry) = (i64::from(rect.x), i64::from(rect.y));
    let (right, bottom) = (rx + i64::from(rect.width), ry + i64::from(rect.height));
    let (x_min, x_max, y_min, y_max) = (x1.min(x2), x1.max(x2), y1.min(y2), y1.max(y2));
    if x1 == x2 {
        let x = x1;
        if x < rx || x > right || y_max < ry || y_min >= bottom {
            return Adjacency::None;
        }
        if right == x || rx == x {
            if y_max > bottom || y_min < ry {
                Adjacency::Partial
            } else if rx == x {
                Adjacency::Contained(Outward::Left)
            } else {
                Adjacency::Contained(Outward::Right)
            }
        } else {
            Adjacency::Overlap
        }
    } else if y1 == y2 {
        let y = y1;
        if y < ry || y > bottom || x_max < rx || x_min >= right {
            return Adjacency::None;
        }
        if bottom == y || ry == y {
            if x_max > right || x_min < rx {
                Adjacency::Partial
            } else if ry == y {
                Adjacency::Contained(Outward::Up)
            } else {
                Adjacency::Contained(Outward::Down)
            }
        } else {
            Adjacency::Overlap
        }
    } else {
        Adjacency::None
    }
}

/// mutter's `check_barrier`: an axis-aligned line on exactly one
/// monitor edge, overlapping no monitor, and (horizontal) not running
/// on past the right end of the primary monitor's row. Returns the side
/// crossing captures towards. A line on no monitor edge, which mutter
/// answers without a message, is refused with one.
pub(crate) fn check_barrier(
    zones: &Zones,
    line: (i32, i32, i32, i32),
) -> Result<Outward, &'static str> {
    let (x1, y1, x2, y2) = line;
    if x1 != x2 && y1 != y2 {
        return Err("Barrier coordinates not axis aligned");
    }
    if x1 == x2 && y1 == y2 {
        return Err("Barrier cannot be a singularity");
    }
    let mut adjacent = None;
    for rect in &zones.rects {
        match adjacency(rect, line) {
            Adjacency::None => {}
            Adjacency::Contained(outward) => {
                if adjacent.is_some() {
                    return Err("Adjacent to multiple monitor edges");
                }
                adjacent = Some(outward);
            }
            Adjacency::Overlap => return Err("Line overlaps with monitor region"),
            Adjacency::Partial => return Err("Line partially with monitor region"),
        }
    }
    let outward = adjacent.ok_or("Barrier not adjacent to any monitor edge")?;
    if y1 == y2 {
        if let Some(mut monitor) = zones.rects.get(zones.primary).copied() {
            // meta_monitor_manager_get_logical_monitor_neighbor(RIGHT),
            // walked to the end of the row; x strictly grows, so the
            // bound only guards degenerate layouts.
            for _ in 0..zones.rects.len() {
                let right = i64::from(monitor.x) + i64::from(monitor.width);
                let next = zones.rects.iter().find(|other| {
                    i64::from(other.x) == right
                        && i64::from(other.y) < i64::from(monitor.y) + i64::from(monitor.height)
                        && i64::from(monitor.y) < i64::from(other.y) + i64::from(other.height)
                });
                match next {
                    Some(next) => monitor = *next,
                    None => break,
                }
            }
            let fake = Zone {
                x: monitor.x.saturating_add(monitor.width),
                ..monitor
            };
            if adjacency(&fake, line) != Adjacency::None {
                return Err("Line extends into nonexisting monitor region");
            }
        }
    }
    Ok(outward)
}

/// One validated barrier (mutter's `InputCaptureBarrier`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Barrier {
    pub id: u32,
    pub line: (i32, i32, i32, i32),
    pub outward: Outward,
}

impl Barrier {
    /// Where pointer motion `from` → `to` crosses this barrier outward
    /// (both ends inclusive, as mutter's `meta_line2_intersects_with`).
    /// Motion along or away from the edge never crosses, so a pointer
    /// resting on the edge does not trip it by leaving.
    pub(crate) fn crossing(&self, from: (f64, f64), to: (f64, f64)) -> Option<(f64, f64)> {
        if !(from.0.is_finite() && from.1.is_finite() && to.0.is_finite() && to.1.is_finite()) {
            return None;
        }
        let (x1, y1, x2, y2) = self.line;
        // Project onto (across, along) the barrier.
        let (vertical, sign) = match self.outward {
            Outward::Left => (true, -1.0),
            Outward::Right => (true, 1.0),
            Outward::Up => (false, -1.0),
            Outward::Down => (false, 1.0),
        };
        let split = |p: (f64, f64)| if vertical { p } else { (p.1, p.0) };
        let (fa, fb) = split(from);
        let (ta, tb) = split(to);
        let (edge, lo, hi) = if vertical {
            (f64::from(x1), f64::from(y1.min(y2)), f64::from(y1.max(y2)))
        } else {
            (f64::from(y1), f64::from(x1.min(x2)), f64::from(x1.max(x2)))
        };
        if (ta - fa) * sign <= 0.0 || (fa - edge) * sign > 0.0 || (ta - edge) * sign < 0.0 {
            return None;
        }
        let t = (edge - fa) / (ta - fa);
        let along = fb + t * (tb - fb);
        if !(lo..=hi).contains(&along) {
            return None;
        }
        Some(if vertical {
            (edge, along)
        } else {
            (along, edge)
        })
    }
}

/// mutter's `InputCaptureState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    Init,
    Enabled,
    Activated,
    Closed,
}

/// What a transition does outside the state: signals and EIS commands.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Effect {
    /// The EIS devices exist (`ensure_eis_devices`) or are removed.
    Devices(bool),
    /// The EIS pointer's regions follow the new monitor layout.
    Zones(Zones),
    Activated {
        barrier_id: u32,
        activation_id: u32,
        cursor: (f64, f64),
    },
    Deactivated {
        activation_id: u32,
    },
    /// The compositor disabled the session on its own (the cancel
    /// shortcut, a monitor change, an EIS client binding nothing).
    /// mutter 51 declares this signal without emitting it; the portal
    /// forwards it as the InputCapture portal's `Disabled`, which tells
    /// the application to enable again.
    Disabled,
    ZonesChanged,
    Closed,
}

/// One session's state machine (mutter's `MetaInputCaptureSession`
/// fields), free of D-Bus, threads and devices.
#[derive(Debug)]
pub(crate) struct SessionState {
    phase: Phase,
    barriers: BTreeMap<u32, Barrier>,
    zones: Zones,
    zones_serial: u32,
    activation_id: u32,
    /// `release_all_barriers`: the next motion passes the barriers
    /// (mutter's `META_BARRIER_STATE_RELEASE`).
    released: bool,
    cancel_requested: bool,
    keys_pressed: u32,
    buttons_pressed: u32,
}

impl SessionState {
    pub(crate) fn new(zones: Zones) -> Self {
        Self {
            phase: Phase::Init,
            barriers: BTreeMap::new(),
            zones,
            zones_serial: 0,
            activation_id: 0,
            released: false,
            cancel_requested: false,
            keys_pressed: 0,
            buttons_pressed: 0,
        }
    }

    pub(crate) fn phase(&self) -> Phase {
        self.phase
    }

    /// The current activation's id while activated.
    pub(crate) fn activation(&self) -> Option<u32> {
        (self.phase == Phase::Activated).then_some(self.activation_id)
    }

    /// mutter's `on_monitors_changed`: a new layout bumps the serial,
    /// disables the session and says so.
    pub(crate) fn sync_zones(&mut self, zones: Zones) -> Vec<Effect> {
        if self.phase == Phase::Closed || zones == self.zones {
            return Vec::new();
        }
        self.zones_serial = self.zones_serial.wrapping_add(1);
        self.zones = zones.clone();
        let mut effects = vec![Effect::Zones(zones)];
        effects.extend(self.disable_by_compositor());
        effects.push(Effect::ZonesChanged);
        effects
    }

    /// `GetZones`: the serial and the zones it describes.
    pub(crate) fn zones(&self) -> (u32, &Zones) {
        (self.zones_serial, &self.zones)
    }

    /// `AddBarrier`, as mutter's `handle_add_barrier` orders its checks.
    pub(crate) fn add_barrier(
        &mut self,
        serial: u32,
        line: (i32, i32, i32, i32),
    ) -> fdo::Result<u32> {
        if self.zones_serial != serial {
            return Err(fdo::Error::BadAddress("State out of date".into()));
        }
        if self.phase != Phase::Init {
            return Err(fdo::Error::AccessDenied("Session already enabled".into()));
        }
        let outward =
            check_barrier(&self.zones, line).map_err(|e| fdo::Error::AccessDenied(e.into()))?;
        if self.barriers.len() >= MAX_BARRIERS {
            return Err(fdo::Error::LimitsExceeded("too many barriers".into()));
        }
        let id = (1..=u32::MAX)
            .find(|id| !self.barriers.contains_key(id))
            .ok_or_else(|| fdo::Error::LimitsExceeded("too many barriers".into()))?;
        self.barriers.insert(id, Barrier { id, line, outward });
        Ok(id)
    }

    /// `ClearBarriers`: in any state, as mutter's (an enabled session
    /// keeps running with nothing left to cross).
    pub(crate) fn clear_barriers(&mut self) {
        self.barriers.clear();
    }

    /// `Enable`.
    pub(crate) fn enable(&mut self) -> fdo::Result<Vec<Effect>> {
        if self.phase != Phase::Init {
            return Err(fdo::Error::Failed("Already enabled".into()));
        }
        self.phase = Phase::Enabled;
        self.cancel_requested = false;
        self.released = false;
        Ok(vec![Effect::Devices(true)])
    }

    /// `Disable` from the owner.
    pub(crate) fn disable_requested(&mut self) -> fdo::Result<Vec<Effect>> {
        if !matches!(self.phase, Phase::Enabled | Phase::Activated) {
            return Err(fdo::Error::Failed("Session not enabled".into()));
        }
        Ok(self.disable())
    }

    /// `Release`: ends the capture; the next motion passes the barrier.
    pub(crate) fn release(&mut self) -> fdo::Result<Vec<Effect>> {
        if self.phase != Phase::Activated {
            return Err(fdo::Error::AccessDenied("Capture not active".into()));
        }
        self.released = true;
        Ok(self.deactivate())
    }

    /// `Close`, lock or owner loss. Idempotent.
    pub(crate) fn close(&mut self) -> Vec<Effect> {
        if self.phase == Phase::Closed {
            return Vec::new();
        }
        let mut effects = self.disable();
        self.phase = Phase::Closed;
        effects.push(Effect::Closed);
        effects
    }

    /// Pointer motion `from` → `to` (unclamped): an enabled session's
    /// closest crossed barrier activates it (mutter's `on_barrier_hit`).
    pub(crate) fn motion(&mut self, from: (f64, f64), to: (f64, f64)) -> Vec<Effect> {
        if self.phase != Phase::Enabled {
            return Vec::new();
        }
        if std::mem::take(&mut self.released) {
            return Vec::new();
        }
        let distance = |p: (f64, f64)| (p.0 - from.0).powi(2) + (p.1 - from.1).powi(2);
        let Some((barrier_id, cursor)) = self
            .barriers
            .values()
            .filter_map(|b| b.crossing(from, to).map(|p| (b.id, p)))
            .min_by(|a, b| distance(a.1).total_cmp(&distance(b.1)))
        else {
            return Vec::new();
        };
        self.phase = Phase::Activated;
        self.activation_id = self.activation_id.wrapping_add(1);
        // Presses count from this activation on: releases of keys held
        // into an earlier one never reach this session.
        self.keys_pressed = 0;
        self.buttons_pressed = 0;
        vec![Effect::Activated {
            barrier_id,
            activation_id: self.activation_id,
            cursor,
        }]
    }

    /// A key went to the EIS client (`update_keys_pressed`).
    pub(crate) fn captured_key(&mut self, pressed: bool) -> Vec<Effect> {
        self.keys_pressed = if pressed {
            self.keys_pressed.saturating_add(1)
        } else {
            self.keys_pressed.saturating_sub(1)
        };
        self.maybe_disable_cancelled()
    }

    /// A button went to the EIS client (`update_buttons_pressed`).
    pub(crate) fn captured_button(&mut self, pressed: bool) -> Vec<Effect> {
        self.buttons_pressed = if pressed {
            self.buttons_pressed.saturating_add(1)
        } else {
            self.buttons_pressed.saturating_sub(1)
        };
        self.maybe_disable_cancelled()
    }

    /// The cancel shortcut (`meta_input_capture_session_notify_cancelled`):
    /// the session disables once nothing captured is held.
    pub(crate) fn cancel(&mut self) -> Vec<Effect> {
        if self.cancel_requested {
            return Vec::new();
        }
        self.cancel_requested = true;
        self.maybe_disable_cancelled()
    }

    /// The compositor turns an enabled session off on its own.
    pub(crate) fn disable_by_compositor(&mut self) -> Vec<Effect> {
        if !matches!(self.phase, Phase::Enabled | Phase::Activated) {
            return Vec::new();
        }
        let mut effects = self.disable();
        effects.push(Effect::Disabled);
        effects
    }

    fn maybe_disable_cancelled(&mut self) -> Vec<Effect> {
        if !self.cancel_requested || self.keys_pressed != 0 || self.buttons_pressed != 0 {
            return Vec::new();
        }
        self.disable_by_compositor()
    }

    fn deactivate(&mut self) -> Vec<Effect> {
        self.phase = Phase::Enabled;
        vec![Effect::Deactivated {
            activation_id: self.activation_id,
        }]
    }

    /// mutter's `meta_input_capture_session_disable`: barriers stay
    /// registered for the next `Enable`; the EIS devices go.
    fn disable(&mut self) -> Vec<Effect> {
        let mut effects = match self.phase {
            Phase::Init | Phase::Closed => return Vec::new(),
            Phase::Activated => self.deactivate(),
            Phase::Enabled => Vec::new(),
        };
        self.phase = Phase::Init;
        self.released = false;
        effects.push(Effect::Devices(false));
        effects
    }
}

/// Input diverted to the session's EIS receiver.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum CapturedInput {
    Motion {
        dx: f64,
        dy: f64,
    },
    Button {
        button: u32,
        pressed: bool,
    },
    /// Wheel notches, in v120 units.
    ScrollDiscrete {
        dx: i32,
        dy: i32,
    },
    /// Continuous scroll in logical pixels, and the axes it finished on.
    Scroll {
        dx: f64,
        dy: f64,
        stop: (bool, bool),
    },
    Key {
        key: u32,
        pressed: bool,
    },
}

/// Commands to the EIS worker thread.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum EisCommand {
    Start(u32),
    Stop,
    Devices(bool),
    Zones(Zones),
    Input(CapturedInput),
    Close,
}

/// The session's link to its EIS worker.
pub(crate) struct EisLink {
    pub commands: calloop::channel::Sender<EisCommand>,
    pub alive: Arc<AtomicBool>,
}

#[derive(Clone)]
pub(crate) struct SessionSignals {
    pub emitter: SignalEmitter<'static>,
    pub queue: SignalQueue,
}

/// One session as the D-Bus thread, the loop and its EIS worker share it.
pub(crate) struct CaptureShared {
    pub id: u64,
    /// The portal's unique bus name.
    pub owner: String,
    state: Mutex<SessionState>,
    signals: Option<SessionSignals>,
    eis: Mutex<Option<EisLink>>,
}

impl CaptureShared {
    pub(crate) fn new(
        id: u64,
        owner: String,
        zones: Zones,
        signals: Option<SessionSignals>,
    ) -> Self {
        Self {
            id,
            owner,
            state: Mutex::new(SessionState::new(zones)),
            signals,
            eis: Mutex::new(None),
        }
    }

    /// Run one transition and apply its effects under the session lock
    /// (state, then EIS link: the only lock order).
    fn transition<T>(
        &self,
        f: impl FnOnce(&mut SessionState) -> fdo::Result<(T, Vec<Effect>)>,
    ) -> fdo::Result<T> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| fdo::Error::Failed("input capture state unavailable".into()))?;
        let (value, effects) = f(&mut state)?;
        self.apply(effects);
        Ok(value)
    }

    fn effects(&self, f: impl FnOnce(&mut SessionState) -> Vec<Effect>) {
        let _ = self.transition(|state| Ok(((), f(state))));
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.state
            .lock()
            .map_or(true, |state| state.phase() == Phase::Closed)
    }

    pub(crate) fn activation(&self) -> Option<u32> {
        self.state.lock().ok().and_then(|state| state.activation())
    }

    pub(crate) fn sync_zones(&self, zones: Zones) {
        self.effects(|state| state.sync_zones(zones));
    }

    pub(crate) fn get_zones(&self, zones: Zones) -> fdo::Result<(u32, DbusZones)> {
        self.transition(|state| {
            let effects = state.sync_zones(zones);
            let (serial, zones) = state.zones();
            Ok(((serial, zones.dbus()), effects))
        })
    }

    pub(crate) fn add_barrier(
        &self,
        zones: Zones,
        serial: u32,
        line: (i32, i32, i32, i32),
    ) -> fdo::Result<u32> {
        // A layout change the scan has not reported yet still makes
        // the caller's serial stale.
        self.sync_zones(zones);
        self.transition(|state| state.add_barrier(serial, line).map(|id| (id, Vec::new())))
    }

    pub(crate) fn clear_barriers(&self) -> fdo::Result<()> {
        self.transition(|state| {
            state.clear_barriers();
            Ok(((), Vec::new()))
        })
    }

    pub(crate) fn enable(&self) -> fdo::Result<()> {
        self.transition(|state| state.enable().map(|e| ((), e)))
    }

    pub(crate) fn disable(&self) -> fdo::Result<()> {
        self.transition(|state| state.disable_requested().map(|e| ((), e)))
    }

    pub(crate) fn release(&self) -> fdo::Result<()> {
        self.transition(|state| state.release().map(|e| ((), e)))
    }

    pub(crate) fn close(&self) {
        self.effects(SessionState::close);
    }

    pub(crate) fn disable_by_compositor(&self) {
        self.effects(SessionState::disable_by_compositor);
    }

    /// Motion on the loop; the new activation id when it activated.
    fn motion(&self, from: (f64, f64), to: (f64, f64)) -> Option<u32> {
        self.transition(|state| {
            let effects = state.motion(from, to);
            let activated = effects.iter().find_map(|effect| match effect {
                Effect::Activated { activation_id, .. } => Some(*activation_id),
                _ => None,
            });
            Ok((activated, effects))
        })
        .ok()
        .flatten()
    }

    fn captured_key(&self, key: u32, pressed: bool) {
        // The key reaches the client before a cancel can stop emulating.
        self.send_input(CapturedInput::Key { key, pressed });
        self.effects(|state| state.captured_key(pressed));
    }

    fn captured_button(&self, button: u32, pressed: bool) {
        self.send_input(CapturedInput::Button { button, pressed });
        self.effects(|state| state.captured_button(pressed));
    }

    fn cancel(&self) {
        self.effects(SessionState::cancel);
    }

    fn send_input(&self, input: CapturedInput) {
        self.eis_send(EisCommand::Input(input));
    }

    /// Attach a new EIS worker; the zones and the activation it starts
    /// from. One live client per session (mutter disconnects others).
    pub(crate) fn attach_eis(&self, link: EisLink) -> fdo::Result<(Zones, Option<u32>)> {
        let state = self
            .state
            .lock()
            .map_err(|_| fdo::Error::Failed("input capture state unavailable".into()))?;
        if state.phase() == Phase::Closed {
            return Err(crate::capture_security::denied("session closed"));
        }
        let mut eis = self
            .eis
            .lock()
            .map_err(|_| fdo::Error::Failed("EIS link unavailable".into()))?;
        if eis
            .as_ref()
            .is_some_and(|link| link.alive.load(Ordering::SeqCst))
        {
            return Err(fdo::Error::LimitsExceeded(
                "one EIS client per input capture session".into(),
            ));
        }
        *eis = Some(link);
        Ok((state.zones().1.clone(), state.activation()))
    }

    fn eis_send(&self, command: EisCommand) {
        let Ok(mut eis) = self.eis.lock() else {
            return;
        };
        if let Some(link) = eis.as_ref() {
            if link.commands.send(command).is_err() {
                *eis = None;
            }
        }
    }

    fn emit<F, Fut>(&self, signal: F)
    where
        F: FnOnce(SignalEmitter<'static>) -> Fut,
        Fut: Future<Output = zbus::Result<()>> + Send + 'static,
    {
        if let Some(signals) = &self.signals {
            signals.queue.emit(signal(signals.emitter.clone()));
        }
    }

    fn apply(&self, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::Devices(on) => self.eis_send(EisCommand::Devices(on)),
                Effect::Zones(zones) => self.eis_send(EisCommand::Zones(zones)),
                Effect::Activated {
                    barrier_id,
                    activation_id,
                    cursor,
                } => {
                    self.emit(move |e| async move {
                        CaptureSession::activated(&e, barrier_id, activation_id, cursor).await
                    });
                    self.eis_send(EisCommand::Start(activation_id));
                }
                Effect::Deactivated { activation_id } => {
                    self.eis_send(EisCommand::Stop);
                    self.emit(move |e| async move {
                        CaptureSession::deactivated(&e, activation_id).await
                    });
                }
                Effect::Disabled => {
                    self.emit(|e| async move { CaptureSession::disabled(&e).await });
                }
                Effect::ZonesChanged => {
                    self.emit(|e| async move { CaptureSession::zones_changed(&e).await });
                }
                Effect::Closed => {
                    self.eis_send(EisCommand::Close);
                    self.emit(|e| async move { CaptureSession::closed(&e).await });
                }
            }
        }
    }
}

/// `/org/gnome/Mutter/InputCapture`.
pub(crate) struct InputCapture {
    pub authority: crate::capture_security::Authority,
    pub sessions: CaptureSessions,
    pub outputs: Outputs,
    pub to_loop: calloop::channel::Sender<ToLoop>,
    pub signals: SignalQueue,
    pub next_id: Arc<AtomicU64>,
    /// Wakes the revocation scan for the new session.
    pub scan: Arc<super::ScanWake>,
}

#[interface(name = "org.gnome.Mutter.InputCapture")]
impl InputCapture {
    /// mutter's `handle_create_session`; like mutter, the requested
    /// capabilities do not change what the session captures.
    async fn create_session(
        &mut self,
        capabilities: u32,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> fdo::Result<OwnedObjectPath> {
        let _ = capabilities;
        let owner = self.authority.admit_portal(conn, &header).await?;
        if self
            .sessions
            .lock()
            .map_or(true, |sessions| sessions.len() >= MAX_SESSIONS)
        {
            return Err(fdo::Error::LimitsExceeded(
                "too many input capture sessions".into(),
            ));
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let path = OwnedObjectPath::try_from(session_path(id))
            .map_err(|e| fdo::Error::Failed(e.to_string()))?;
        // Session signals go to the portal only.
        let destination = zbus::names::BusName::try_from(owner.clone())
            .map_err(|e| fdo::Error::Failed(e.to_string()))?;
        let emitter = SignalEmitter::new(conn, path.as_str())?
            .to_owned()
            .set_destination(destination);
        let shared = Arc::new(CaptureShared::new(
            id,
            owner,
            Zones::snapshot(&self.outputs),
            Some(SessionSignals {
                emitter,
                queue: self.signals.clone(),
            }),
        ));
        server
            .at(
                &path,
                CaptureSession {
                    shared: shared.clone(),
                    sessions: self.sessions.clone(),
                    authority: self.authority.clone(),
                    outputs: self.outputs.clone(),
                    to_loop: self.to_loop.clone(),
                },
            )
            .await?;
        self.sessions
            .lock()
            .map_err(|_| fdo::Error::Failed("input capture registry unavailable".into()))?
            .insert(id, shared);
        self.scan.notify();
        Ok(path)
    }

    #[zbus(property)]
    fn supported_capabilities(&self) -> u32 {
        SUPPORTED_CAPABILITIES
    }
}

/// `/org/gnome/Mutter/InputCapture/Session/u<id>`.
pub(crate) struct CaptureSession {
    pub shared: Arc<CaptureShared>,
    pub sessions: CaptureSessions,
    pub authority: crate::capture_security::Authority,
    pub outputs: Outputs,
    pub to_loop: calloop::channel::Sender<ToLoop>,
}

impl CaptureSession {
    /// mutter's `check_permission` (the session's peer), unrevoked and
    /// unlocked.
    fn admitted(&self, header: &zbus::message::Header<'_>) -> fdo::Result<()> {
        if !crate::capture_security::owns_session(
            &self.shared.owner,
            header.sender().map(|s| s.as_str()),
            self.shared.is_closed(),
        ) {
            return Err(crate::capture_security::denied("Permission denied"));
        }
        self.authority.unlocked()
    }
}

/// `Release`'s `cursor_position` option, `(dd)`.
fn cursor_position(options: &HashMap<String, OwnedVariant>) -> fdo::Result<Option<(f64, f64)>> {
    let Some(value) = options.get("cursor_position") else {
        return Ok(None);
    };
    let invalid = || fdo::Error::InvalidArgs("cursor_position must be (dd)".into());
    let Variant::Structure(position) = &**value else {
        return Err(invalid());
    };
    let [Variant::F64(x), Variant::F64(y)] = position.fields() else {
        return Err(invalid());
    };
    super::remote_desktop::finite_pair(*x, *y)?;
    Ok(Some((*x, *y)))
}

#[interface(name = "org.gnome.Mutter.InputCapture.Session")]
impl CaptureSession {
    async fn get_zones(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> fdo::Result<(u32, DbusZones)> {
        self.admitted(&header)?;
        self.shared.get_zones(Zones::snapshot(&self.outputs))
    }

    async fn add_barrier(
        &self,
        serial: u32,
        position: (i32, i32, i32, i32),
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> fdo::Result<u32> {
        self.admitted(&header)?;
        self.shared
            .add_barrier(Zones::snapshot(&self.outputs), serial, position)
    }

    async fn clear_barriers(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> fdo::Result<()> {
        self.admitted(&header)?;
        self.shared.clear_barriers()
    }

    async fn enable(&self, #[zbus(header)] header: zbus::message::Header<'_>) -> fdo::Result<()> {
        self.admitted(&header)?;
        self.shared.enable()
    }

    async fn disable(&self, #[zbus(header)] header: zbus::message::Header<'_>) -> fdo::Result<()> {
        self.admitted(&header)?;
        self.shared.disable()
    }

    /// A receiver EIS context the captured input goes to.
    #[zbus(name = "ConnectToEIS")]
    async fn connect_to_eis(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> fdo::Result<zbus::OwnedFd> {
        self.admitted(&header)?;
        let (reply, answer) = std::sync::mpsc::channel();
        self.to_loop
            .send(ToLoop::RemoteKeymap { reply })
            .map_err(|_| fdo::Error::Failed("compositor gone".into()))?;
        let keymap = answer
            .recv_timeout(std::time::Duration::from_secs(2))
            .map_err(|_| fdo::Error::Failed("keyboard unavailable".into()))?;
        self.admitted(&header)?;
        let (server, client) = UnixStream::pair()
            .map_err(|e| fdo::Error::Failed(format!("Failed to create socket: {e}")))?;
        super::capture_eis::start(server, &self.shared, keymap)?;
        Ok(std::os::fd::OwnedFd::from(client).into())
    }

    /// mutter's `handle_release`: ends the capture and warps the
    /// pointer to `cursor_position` when given.
    async fn release(
        &self,
        options: HashMap<String, OwnedVariant>,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> fdo::Result<()> {
        self.admitted(&header)?;
        let cursor = cursor_position(&options)?;
        self.shared.release()?;
        if let Some((x, y)) = cursor {
            self.to_loop
                .send(ToLoop::InputCaptureWarp { x, y })
                .map_err(|_| fdo::Error::Failed("compositor gone".into()))?;
        }
        Ok(())
    }

    /// Only the owner, but also while locked and after a lock or the
    /// revocation scan already closed the session: nothing is left then.
    async fn close(
        &self,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> fdo::Result<()> {
        if header
            .sender()
            .is_none_or(|sender| sender.as_str() != self.shared.owner)
        {
            return Err(crate::capture_security::denied("Permission denied"));
        }
        self.shared.close();
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.remove(&self.shared.id);
        }
        let _ = server
            .remove::<CaptureSession, _>(session_path(self.shared.id).as_str())
            .await;
        Ok(())
    }

    #[zbus(signal)]
    pub async fn activated(
        emitter: &SignalEmitter<'_>,
        barrier_id: u32,
        activation_id: u32,
        cursor_position: (f64, f64),
    ) -> zbus::Result<()>;
    #[zbus(signal)]
    pub async fn deactivated(emitter: &SignalEmitter<'_>, activation_id: u32) -> zbus::Result<()>;
    #[zbus(signal)]
    pub async fn zones_changed(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
    #[zbus(signal)]
    pub async fn disabled(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
    #[zbus(signal)]
    pub async fn closed(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

/// The compositor loop's side (mutter's event router and
/// `meta_display_process_captured_input`): backend input passes here
/// before anything else sees it.
#[derive(Default)]
pub(crate) struct Router {
    sessions: CaptureSessions,
    active: Option<(Arc<CaptureShared>, u32)>,
    /// Pressed into a capture and not yet released: their releases go
    /// where the presses went, never to a local client.
    held_keys: BTreeSet<u32>,
    held_buttons: BTreeSet<u32>,
}

/// What [`Router::route`] left of one backend event's inputs.
#[derive(Debug, Default)]
pub(crate) struct Routed {
    /// For the local seat, in order.
    pub deliver: Vec<ManagerInput>,
    /// Some input went to a capture (still user activity).
    pub captured: bool,
}

impl Router {
    /// The registry the D-Bus thread fills.
    pub(crate) fn sessions(&self) -> CaptureSessions {
        self.sessions.clone()
    }

    /// One backend event's inputs. `from` is the pointer before them;
    /// `motion_free` is false while a pointer constraint holds the
    /// pointer (it cannot cross anything). Nothing is captured, and no
    /// barrier can activate, while `locked`.
    pub(crate) fn route(
        &mut self,
        inputs: Vec<ManagerInput>,
        from: Point<f64, Logical>,
        locked: bool,
        motion_free: bool,
    ) -> Routed {
        if locked {
            self.active = None;
        }
        let delta = inputs.iter().find_map(|input| match input {
            ManagerInput::RelativeMotion { delta, .. } => Some(*delta),
            _ => None,
        });
        let mut routed = Routed::default();
        let mut activated_now = false;
        for input in inputs {
            if let Some(session) = self.active_session() {
                // The motion that reached the edge is not the client's.
                if activated_now && matches!(input, ManagerInput::RelativeMotion { .. }) {
                    routed.captured = true;
                    continue;
                }
                if self.capture(&session, input) {
                    routed.captured = true;
                    continue;
                }
            }
            match input {
                ManagerInput::Key {
                    keycode,
                    pressed: false,
                    ..
                } if self.held_keys.remove(&keycode) => continue,
                ManagerInput::Button {
                    button,
                    pressed: false,
                    ..
                } if self.held_buttons.remove(&button) => continue,
                ManagerInput::Motion { .. } if !locked && motion_free => {
                    if let Some(delta) = delta {
                        let to = (from.x + delta.x, from.y + delta.y);
                        activated_now = self.barrier_motion((from.x, from.y), to);
                    }
                }
                _ => {}
            }
            routed.deliver.push(input);
        }
        routed
    }

    /// Lock: every session closes at once; the D-Bus scan then drops
    /// their objects.
    pub(crate) fn revoke_all(&mut self) {
        self.active = None;
        let sessions: Vec<_> = self
            .sessions
            .lock()
            .map(|s| s.values().cloned().collect())
            .unwrap_or_default();
        for session in sessions {
            session.close();
        }
    }

    fn active_session(&mut self) -> Option<Arc<CaptureShared>> {
        let (session, id) = self.active.as_ref()?;
        if session.activation() == Some(*id) {
            Some(session.clone())
        } else {
            self.active = None;
            None
        }
    }

    fn barrier_motion(&mut self, from: (f64, f64), to: (f64, f64)) -> bool {
        let sessions: Vec<_> = self
            .sessions
            .lock()
            .map(|s| s.values().cloned().collect())
            .unwrap_or_default();
        for session in sessions {
            if let Some(id) = session.motion(from, to) {
                self.active = Some((session, id));
                return true;
            }
        }
        false
    }

    /// Divert one input to the active session; false leaves it local
    /// (gestures, as mutter's `process_event` passes them, and releases
    /// of presses that came before the capture).
    fn capture(&mut self, session: &CaptureShared, input: ManagerInput) -> bool {
        use crate::windows::{
            ESCAPE_KEYCODE, SHIFT_LEFT_KEYCODE, SHIFT_RIGHT_KEYCODE, SUPER_LEFT_KEYCODE,
            SUPER_RIGHT_KEYCODE,
        };
        match input {
            // The pointer stays where it met the barrier.
            ManagerInput::Motion { .. } | ManagerInput::CornerPressure { .. } => true,
            ManagerInput::RelativeMotion { delta, .. } => {
                session.send_input(CapturedInput::Motion {
                    dx: delta.x,
                    dy: delta.y,
                });
                true
            }
            ManagerInput::Button {
                button, pressed, ..
            } => {
                if pressed {
                    if !self.held_buttons.insert(button) {
                        return true;
                    }
                } else if !self.held_buttons.remove(&button) {
                    return false;
                }
                session.captured_button(button, pressed);
                true
            }
            ManagerInput::Axis {
                horizontal,
                vertical,
                stop,
                ..
            } => {
                let wheel = |v: f64| v == 0.0 || crate::windows::is_v120_wheel(v);
                let input = if (horizontal != 0.0 || vertical != 0.0)
                    && wheel(horizontal)
                    && wheel(vertical)
                    && !stop.any()
                {
                    CapturedInput::ScrollDiscrete {
                        dx: horizontal as i32,
                        dy: vertical as i32,
                    }
                } else {
                    CapturedInput::Scroll {
                        dx: horizontal,
                        dy: vertical,
                        stop: (stop.horizontal, stop.vertical),
                    }
                };
                session.send_input(input);
                true
            }
            ManagerInput::Key {
                keycode, pressed, ..
            } => {
                if pressed {
                    let held =
                        |a: u32, b: u32| self.held_keys.contains(&a) || self.held_keys.contains(&b);
                    if keycode == ESCAPE_KEYCODE
                        && held(SUPER_LEFT_KEYCODE, SUPER_RIGHT_KEYCODE)
                        && held(SHIFT_LEFT_KEYCODE, SHIFT_RIGHT_KEYCODE)
                    {
                        session.cancel();
                    }
                    if !self.held_keys.insert(keycode) {
                        return true;
                    }
                } else if !self.held_keys.remove(&keycode) {
                    return false;
                }
                session.captured_key(keycode, pressed);
                true
            }
            ManagerInput::SwipeBegin { .. }
            | ManagerInput::SwipeUpdate { .. }
            | ManagerInput::SwipeEnd { .. } => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zone(x: i32, y: i32, width: i32, height: i32) -> Zone {
        Zone {
            x,
            y,
            width,
            height,
            scale: 1.0,
        }
    }

    /// Two 1920x1080 monitors side by side, the left one primary.
    fn two() -> Zones {
        Zones {
            rects: vec![zone(0, 0, 1920, 1080), zone(1920, 0, 1920, 1080)],
            primary: 0,
        }
    }

    #[test]
    fn barriers_validate_as_mutters_check_barrier() {
        let zones = two();
        // Outer edges, portal-style inclusive ends.
        assert_eq!(check_barrier(&zones, (0, 0, 0, 1079)), Ok(Outward::Left));
        assert_eq!(
            check_barrier(&zones, (3840, 0, 3840, 1079)),
            Ok(Outward::Right)
        );
        assert_eq!(check_barrier(&zones, (0, 0, 1919, 0)), Ok(Outward::Up));
        assert_eq!(
            check_barrier(&zones, (1920, 1080, 3839, 1080)),
            Ok(Outward::Down)
        );
        for (line, error) in [
            ((0, 0, 10, 10), "Barrier coordinates not axis aligned"),
            ((5, 5, 5, 5), "Barrier cannot be a singularity"),
            ((1920, 0, 1920, 1079), "Adjacent to multiple monitor edges"),
            ((100, 0, 100, 1079), "Line overlaps with monitor region"),
            ((0, -10, 0, 1079), "Line partially with monitor region"),
            (
                (5000, 0, 5000, 100),
                "Barrier not adjacent to any monitor edge",
            ),
            // The top edge of the row's last monitor may not run on to
            // where a further monitor would start (mutter's fake layout).
            (
                (1920, 0, 3840, 0),
                "Line extends into nonexisting monitor region",
            ),
        ] {
            assert_eq!(check_barrier(&zones, line), Err(error), "{line:?}");
        }
        // The same edge, one pixel short, is fine.
        assert_eq!(check_barrier(&zones, (1920, 0, 3839, 0)), Ok(Outward::Up));
    }

    #[test]
    fn zones_follow_scale_and_rotation() {
        let output = |x, transform, primary| OutputSnapshot {
            connector: "X".into(),
            make: String::new(),
            model: String::new(),
            width: 3840,
            height: 2160,
            refresh_mhz: 60_000,
            x,
            y: 0,
            scale: 2.0,
            primary,
            transform,
        };
        let zones = Zones::from_outputs(&[output(0, 0, false), output(1920, 1, true)]);
        assert_eq!(zones.primary, 1);
        assert_eq!(
            zones.dbus(),
            vec![(1920, 1080, 0, 0), (1080, 1920, 1920, 0)]
        );
    }

    #[test]
    fn crossings_count_outward_motion_only() {
        let right = Barrier {
            id: 1,
            line: (1920, 0, 1920, 1079),
            outward: Outward::Right,
        };
        assert_eq!(
            right.crossing((1919.5, 100.0), (1925.5, 106.0)),
            Some((1920.0, 100.5))
        );
        // Away from, along, short of, or beside the edge: nothing.
        assert_eq!(right.crossing((1920.0, 100.0), (1910.0, 100.0)), None);
        assert_eq!(right.crossing((1919.0, 100.0), (1919.0, 200.0)), None);
        assert_eq!(right.crossing((1900.0, 100.0), (1919.9, 100.0)), None);
        assert_eq!(right.crossing((1919.0, 1100.0), (1925.0, 1100.0)), None);
        assert_eq!(right.crossing((1919.0, f64::NAN), (1925.0, 0.0)), None);
        // Resting on the left edge and pushing out crosses it.
        let left = Barrier {
            id: 2,
            line: (0, 0, 0, 1079),
            outward: Outward::Left,
        };
        assert_eq!(left.crossing((0.0, 50.0), (-3.0, 50.0)), Some((0.0, 50.0)));
        let top = Barrier {
            id: 3,
            line: (0, 0, 1919, 0),
            outward: Outward::Up,
        };
        assert_eq!(top.crossing((10.0, 2.0), (14.0, -2.0)), Some((12.0, 0.0)));
    }

    fn enabled_with_left_barrier() -> SessionState {
        let mut state = SessionState::new(two());
        assert_eq!(state.add_barrier(0, (0, 0, 0, 1079)).unwrap(), 1);
        assert_eq!(state.enable().unwrap(), vec![Effect::Devices(true)]);
        state
    }

    #[test]
    fn add_barrier_checks_serial_state_and_geometry_in_mutters_order() {
        let mut state = SessionState::new(two());
        assert!(matches!(
            state.add_barrier(1, (0, 0, 0, 1079)),
            Err(fdo::Error::BadAddress(_))
        ));
        assert!(matches!(
            state.add_barrier(0, (100, 0, 100, 1079)),
            Err(fdo::Error::AccessDenied(_))
        ));
        assert_eq!(state.add_barrier(0, (0, 0, 0, 1079)).unwrap(), 1);
        assert_eq!(state.add_barrier(0, (3840, 0, 3840, 1079)).unwrap(), 2);
        state.barriers.remove(&1);
        // The lowest free id, as find_available_barrier_id.
        assert_eq!(state.add_barrier(0, (0, 0, 0, 500)).unwrap(), 1);
        state.enable().unwrap();
        assert!(matches!(
            state.add_barrier(0, (0, 600, 0, 700)),
            Err(fdo::Error::AccessDenied(_))
        ));
        assert!(matches!(state.enable(), Err(fdo::Error::Failed(_))));
    }

    #[test]
    fn activation_release_and_disable_follow_mutter() {
        let mut state = SessionState::new(two());
        state.add_barrier(0, (0, 0, 0, 1079)).unwrap();
        // Not enabled: crossing does nothing, release and disable fail.
        assert!(state.motion((1.0, 10.0), (-5.0, 10.0)).is_empty());
        assert!(matches!(state.release(), Err(fdo::Error::AccessDenied(_))));
        assert!(matches!(
            state.disable_requested(),
            Err(fdo::Error::Failed(_))
        ));
        state.enable().unwrap();
        assert_eq!(
            state.motion((1.0, 10.0), (-5.0, 10.0)),
            vec![Effect::Activated {
                barrier_id: 1,
                activation_id: 1,
                cursor: (0.0, 10.0)
            }]
        );
        assert_eq!(state.activation(), Some(1));
        // Already activated: further crossings change nothing.
        assert!(state.motion((0.0, 10.0), (-5.0, 10.0)).is_empty());
        assert_eq!(
            state.release().unwrap(),
            vec![Effect::Deactivated { activation_id: 1 }]
        );
        assert_eq!(state.phase(), Phase::Enabled);
        // The released barrier lets the next motion pass, then holds again.
        assert!(state.motion((0.0, 10.0), (-5.0, 10.0)).is_empty());
        assert!(matches!(
            state.motion((0.0, 10.0), (-5.0, 10.0)).as_slice(),
            [Effect::Activated {
                activation_id: 2,
                ..
            }]
        ));
        // Disable while activated deactivates first; barriers survive.
        assert_eq!(
            state.disable_requested().unwrap(),
            vec![
                Effect::Deactivated { activation_id: 2 },
                Effect::Devices(false)
            ]
        );
        assert_eq!(state.phase(), Phase::Init);
        state.enable().unwrap();
        assert_eq!(state.motion((1.0, 10.0), (-5.0, 10.0)).len(), 1);
        assert_eq!(state.activation(), Some(3));
    }

    #[test]
    fn close_is_final_and_idempotent() {
        let mut state = enabled_with_left_barrier();
        state.motion((1.0, 10.0), (-5.0, 10.0));
        assert_eq!(
            state.close(),
            vec![
                Effect::Deactivated { activation_id: 1 },
                Effect::Devices(false),
                Effect::Closed
            ]
        );
        assert!(state.close().is_empty());
        assert!(state.enable().is_err());
        assert!(state.motion((1.0, 10.0), (-5.0, 10.0)).is_empty());
        assert!(state.sync_zones(Zones::default()).is_empty());
    }

    #[test]
    fn a_monitor_change_bumps_the_serial_and_disables() {
        let mut state = enabled_with_left_barrier();
        assert!(state.sync_zones(two()).is_empty());
        let one = Zones {
            rects: vec![zone(0, 0, 1920, 1080)],
            primary: 0,
        };
        assert_eq!(
            state.sync_zones(one.clone()),
            vec![
                Effect::Zones(one.clone()),
                Effect::Devices(false),
                Effect::Disabled,
                Effect::ZonesChanged
            ]
        );
        assert_eq!(state.zones().0, 1);
        assert!(matches!(
            state.add_barrier(0, (0, 0, 0, 1079)),
            Err(fdo::Error::BadAddress(_))
        ));
        // A disabled session hears the change without Disabled.
        assert_eq!(
            state.sync_zones(two()),
            vec![Effect::Zones(two()), Effect::ZonesChanged]
        );
    }

    #[test]
    fn cancel_waits_for_every_captured_press_to_release() {
        let mut state = enabled_with_left_barrier();
        state.motion((1.0, 10.0), (-5.0, 10.0));
        assert!(state.captured_key(true).is_empty());
        assert!(state.captured_button(true).is_empty());
        assert!(state.cancel().is_empty());
        assert!(state.captured_key(false).is_empty());
        assert_eq!(
            state.captured_button(false),
            vec![
                Effect::Deactivated { activation_id: 1 },
                Effect::Devices(false),
                Effect::Disabled
            ]
        );
        assert_eq!(state.phase(), Phase::Init);
        // Enabling again forgets the cancel.
        state.enable().unwrap();
        state.motion((1.0, 10.0), (-5.0, 10.0));
        assert!(state.captured_key(true).is_empty());
        assert!(state.captured_key(false).is_empty());
        assert_eq!(state.activation(), Some(2));
    }

    fn router_with(session: &Arc<CaptureShared>) -> Router {
        let router = Router::default();
        router
            .sessions
            .lock()
            .unwrap()
            .insert(session.id, session.clone());
        router
    }

    fn motion(x: f64, y: f64, dx: f64, dy: f64) -> Vec<ManagerInput> {
        vec![
            ManagerInput::Motion {
                pos: (x, y).into(),
                time: 1,
            },
            ManagerInput::RelativeMotion {
                delta: (dx, dy).into(),
                delta_unaccel: (dx, dy).into(),
                utime: 1000,
            },
        ]
    }

    fn key(keycode: u32, pressed: bool) -> Vec<ManagerInput> {
        vec![ManagerInput::Key {
            keycode,
            pressed,
            time: 2,
        }]
    }

    /// A session with an attached EIS link whose commands land in the
    /// returned channel.
    fn session_with_eis() -> (Arc<CaptureShared>, calloop::channel::Channel<EisCommand>) {
        let session = Arc::new(CaptureShared::new(1, ":1.1".into(), two(), None));
        let (commands, channel) = calloop::channel::channel();
        session
            .attach_eis(EisLink {
                commands,
                alive: Arc::new(AtomicBool::new(true)),
            })
            .unwrap();
        (session, channel)
    }

    fn drain(channel: &calloop::channel::Channel<EisCommand>) -> Vec<EisCommand> {
        std::iter::from_fn(|| channel.try_recv().ok()).collect()
    }

    /// An enabled session with a left-edge barrier, routed by a router.
    fn armed() -> (
        Arc<CaptureShared>,
        calloop::channel::Channel<EisCommand>,
        Router,
    ) {
        let (session, channel) = session_with_eis();
        assert_eq!(session.add_barrier(two(), 0, (0, 0, 0, 1079)).unwrap(), 1);
        session.enable().unwrap();
        assert_eq!(drain(&channel), vec![EisCommand::Devices(true)]);
        let router = router_with(&session);
        (session, channel, router)
    }

    fn route(router: &mut Router, inputs: Vec<ManagerInput>, from: (f64, f64)) -> Routed {
        router.route(inputs, from.into(), false, true)
    }

    #[test]
    fn router_captures_from_the_crossing_until_release() {
        let (session, channel, mut router) = armed();
        // Inside the screen: delivered as usual.
        let routed = route(&mut router, motion(5.0, 10.0, -4.0, 0.0), (9.0, 10.0));
        assert_eq!(routed.deliver.len(), 2);
        assert!(!routed.captured);
        // The crossing: the pointer motion is delivered, its relative
        // part goes nowhere, and the session activates.
        let routed = route(&mut router, motion(0.0, 10.0, -6.0, 0.0), (1.0, 10.0));
        assert!(matches!(
            routed.deliver.as_slice(),
            [ManagerInput::Motion { .. }]
        ));
        assert_eq!(drain(&channel), vec![EisCommand::Start(1)]);
        assert_eq!(session.activation(), Some(1));
        // Captured: nothing local, everything to EIS.
        let routed = route(&mut router, motion(0.0, 10.0, 3.0, 2.0), (0.0, 10.0));
        assert!(routed.deliver.is_empty() && routed.captured);
        assert!(route(&mut router, key(30, true), (0.0, 10.0))
            .deliver
            .is_empty());
        assert!(route(&mut router, key(30, false), (0.0, 10.0))
            .deliver
            .is_empty());
        let wheel = vec![ManagerInput::Axis {
            horizontal: 0.0,
            vertical: 240.0,
            stop: crate::windows::AxisStop::NONE,
            time: 3,
        }];
        assert!(route(&mut router, wheel, (0.0, 10.0)).deliver.is_empty());
        assert_eq!(
            drain(&channel),
            vec![
                EisCommand::Input(CapturedInput::Motion { dx: 3.0, dy: 2.0 }),
                EisCommand::Input(CapturedInput::Key {
                    key: 30,
                    pressed: true
                }),
                EisCommand::Input(CapturedInput::Key {
                    key: 30,
                    pressed: false
                }),
                EisCommand::Input(CapturedInput::ScrollDiscrete { dx: 0, dy: 240 }),
            ]
        );
        // Gestures stay local (mutter's process_event passes them), and
        // so does the release of a key pressed before the capture.
        let swipe = vec![ManagerInput::SwipeBegin {
            fingers: 3,
            time: 4,
        }];
        assert_eq!(route(&mut router, swipe, (0.0, 10.0)).deliver.len(), 1);
        assert_eq!(
            route(&mut router, key(31, false), (0.0, 10.0))
                .deliver
                .len(),
            1
        );
        // A key held into the capture releases into it even after it ends.
        assert!(route(&mut router, key(32, true), (0.0, 10.0))
            .deliver
            .is_empty());
        session.release().unwrap();
        assert!(route(&mut router, key(32, false), (0.0, 10.0))
            .deliver
            .is_empty());
        assert_eq!(
            drain(&channel),
            vec![
                EisCommand::Input(CapturedInput::Key {
                    key: 32,
                    pressed: true
                }),
                EisCommand::Stop
            ]
        );
        // The released barrier lets one motion through, then holds again.
        assert_eq!(
            route(&mut router, motion(0.0, 10.0, -6.0, 0.0), (0.0, 10.0))
                .deliver
                .len(),
            2
        );
        route(&mut router, motion(0.0, 10.0, -6.0, 0.0), (0.0, 10.0));
        assert_eq!(drain(&channel), vec![EisCommand::Start(2)]);
    }

    #[test]
    fn the_cancel_shortcut_disables_once_everything_is_released() {
        use crate::windows::{ESCAPE_KEYCODE, SHIFT_LEFT_KEYCODE, SUPER_LEFT_KEYCODE};
        let (session, channel, mut router) = armed();
        route(&mut router, motion(0.0, 10.0, -6.0, 0.0), (1.0, 10.0));
        for code in [SUPER_LEFT_KEYCODE, SHIFT_LEFT_KEYCODE, ESCAPE_KEYCODE] {
            assert!(route(&mut router, key(code, true), (0.0, 10.0))
                .deliver
                .is_empty());
        }
        for code in [ESCAPE_KEYCODE, SHIFT_LEFT_KEYCODE] {
            assert!(route(&mut router, key(code, false), (0.0, 10.0))
                .deliver
                .is_empty());
        }
        assert_eq!(session.activation(), Some(1));
        assert!(
            route(&mut router, key(SUPER_LEFT_KEYCODE, false), (0.0, 10.0))
                .deliver
                .is_empty()
        );
        assert_eq!(session.activation(), None);
        assert!(drain(&channel).ends_with(&[EisCommand::Stop, EisCommand::Devices(false)]));
        // Disabled: input is local again and the edge captures nothing.
        assert_eq!(
            route(&mut router, key(30, true), (0.0, 10.0)).deliver.len(),
            1
        );
        route(&mut router, motion(0.0, 10.0, -6.0, 0.0), (0.0, 10.0));
        assert_eq!(session.activation(), None);
    }

    #[test]
    fn lock_and_pointer_constraints_never_capture() {
        let (session, channel, mut router) = armed();
        // A constrained pointer crosses nothing.
        router.route(
            motion(0.0, 10.0, -6.0, 0.0),
            (1.0, 10.0).into(),
            false,
            false,
        );
        assert_eq!(session.activation(), None);
        // Locked: the crossing activates nothing and everything is local.
        let routed = router.route(motion(0.0, 10.0, -6.0, 0.0), (1.0, 10.0).into(), true, true);
        assert_eq!(routed.deliver.len(), 2);
        assert_eq!(session.activation(), None);
        // An active capture ends the moment the lock routes input.
        route(&mut router, motion(0.0, 10.0, -6.0, 0.0), (1.0, 10.0));
        assert_eq!(session.activation(), Some(1));
        let routed = router.route(key(30, true), (0.0, 10.0).into(), true, true);
        assert_eq!(routed.deliver.len(), 1);
        // Locking closes every session for good.
        router.revoke_all();
        assert!(session.is_closed());
        assert!(drain(&channel).ends_with(&[
            EisCommand::Stop,
            EisCommand::Devices(false),
            EisCommand::Close
        ]));
        route(&mut router, motion(0.0, 10.0, -6.0, 0.0), (1.0, 10.0));
        assert_eq!(session.activation(), None);
        assert!(session.enable().is_err());
    }

    #[test]
    fn one_live_eis_client_per_session() {
        let (session, _channel) = session_with_eis();
        let (commands, _second) = calloop::channel::channel();
        let alive = Arc::new(AtomicBool::new(true));
        assert!(matches!(
            session.attach_eis(EisLink {
                commands: commands.clone(),
                alive: alive.clone()
            }),
            Err(fdo::Error::LimitsExceeded(_))
        ));
        // Once the first worker is gone a new client may connect.
        session
            .eis
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .alive
            .store(false, Ordering::SeqCst);
        assert!(session.attach_eis(EisLink { commands, alive }).is_ok());
    }

    #[test]
    fn release_cursor_position_must_be_a_finite_pair() {
        let options = |value: Variant<'static>| {
            HashMap::from([(
                "cursor_position".to_owned(),
                OwnedVariant::try_from(value).unwrap(),
            )])
        };
        assert_eq!(cursor_position(&HashMap::new()).unwrap(), None);
        assert_eq!(
            cursor_position(&options(Variant::from((12.5, 40.0)))).unwrap(),
            Some((12.5, 40.0))
        );
        for bad in [
            Variant::from((1u32, 2u32)),
            Variant::from(3.0),
            Variant::from((f64::NAN, 0.0)),
        ] {
            assert!(cursor_position(&options(bad)).is_err());
        }
    }
}
