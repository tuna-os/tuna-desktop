//! Tablet pads (`zwp_tablet_pad_v2`): the buttons, rings and strips on a
//! tablet's frame, grouped in mode groups.
//!
//! Follows Mutter 51 (`src/wayland/meta-wayland-tablet-pad*.c`): a pad is
//! announced to every tablet seat with its groups, rings and strips; it
//! enters the surface with keyboard focus, paired with its tablet, and
//! only that client's objects receive buttons, ring and strip motion and
//! mode switches.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use wayland_protocols::wp::tablet::zv2::server::{
    zwp_tablet_pad_group_v2::{self, ZwpTabletPadGroupV2},
    zwp_tablet_pad_ring_v2::{self, ZwpTabletPadRingV2},
    zwp_tablet_pad_strip_v2::{self, ZwpTabletPadStripV2},
    zwp_tablet_pad_v2::{self, ZwpTabletPadV2},
    zwp_tablet_seat_v2::ZwpTabletSeatV2,
};
use wayland_server::{
    backend::ClientId, protocol::wl_surface::WlSurface, Client, DataInit, Dispatch, DisplayHandle, Resource,
    Weak,
};

use crate::utils::Serial;

use super::{tablet::device_node, TabletHandle, TabletManagerState};

/// One mode group of a pad: the buttons, rings and strips that change
/// meaning together, and how many modes it cycles through.
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct TabletPadGroupDescriptor {
    /// Pad button indices in this group.
    pub buttons: Vec<u32>,
    /// The group's mode-switch (toggle) buttons, which take no feedback.
    pub mode_switch_buttons: Vec<u32>,
    /// Pad-wide ring indices in this group.
    pub rings: Vec<u32>,
    /// Pad-wide strip indices in this group.
    pub strips: Vec<u32>,
    /// Number of modes (at least one).
    pub modes: u32,
}

/// Description of a tablet pad device
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct TabletPadDescriptor {
    /// Pad device name
    pub name: String,
    /// Path to the device
    pub syspath: Option<PathBuf>,
    /// Number of buttons
    pub buttons: u32,
    /// Mode groups, in libinput's order
    pub groups: Vec<TabletPadGroupDescriptor>,
}

/// One client's objects for a pad.
#[derive(Debug)]
struct PadInstance {
    pad: Weak<ZwpTabletPadV2>,
    groups: Vec<Weak<ZwpTabletPadGroupV2>>,
    rings: HashMap<u32, Weak<ZwpTabletPadRingV2>>,
    strips: HashMap<u32, Weak<ZwpTabletPadStripV2>>,
}

#[derive(Debug, Default)]
struct TabletPad {
    desc: Option<TabletPadDescriptor>,
    instances: Vec<PadInstance>,
    focus: Option<WlSurface>,
    /// Current mode of each group.
    modes: Vec<u32>,
    /// Serial of each group's last `mode_switch`; feedback must quote it.
    mode_switch_serials: Vec<u32>,
    /// `set_feedback` labels from the focused client, cleared on focus
    /// change (Mutter's `pad->feedback`, `ring->feedback`, `strip->feedback`).
    button_feedback: HashMap<u32, String>,
    ring_feedback: HashMap<u32, String>,
    strip_feedback: HashMap<u32, String>,
}

impl TabletPad {
    fn focused(&self) -> Option<&PadInstance> {
        let focus = self.focus.as_ref()?;
        self.instances
            .iter()
            .find(|i| i.pad.id().same_client_as(&focus.id()) && i.pad.upgrade().is_ok())
    }

    fn serial_matches(&self, group: usize, serial: u32) -> bool {
        self.mode_switch_serials.get(group) == Some(&serial)
    }
}

/// Handle to a tablet pad device
#[derive(Debug, Default, Clone)]
pub struct TabletPadHandle {
    inner: Arc<Mutex<TabletPad>>,
}

impl TabletPadHandle {
    pub(super) fn new(desc: &TabletPadDescriptor) -> Self {
        let handle = Self::default();
        {
            let mut inner = handle.inner.lock().unwrap();
            inner.desc = Some(desc.clone());
            inner.modes = vec![0; desc.groups.len()];
            inner.mode_switch_serials = vec![0; desc.groups.len()];
        }
        handle
    }

    pub(super) fn new_instance<D>(&self, client: &Client, dh: &DisplayHandle, seat: &ZwpTabletSeatV2)
    where
        D: Dispatch<ZwpTabletPadV2, TabletPadUserData>,
        D: Dispatch<ZwpTabletPadGroupV2, TabletPadGroupUserData>,
        D: Dispatch<ZwpTabletPadRingV2, TabletPadRingUserData>,
        D: Dispatch<ZwpTabletPadStripV2, TabletPadStripUserData>,
        D: 'static,
    {
        let mut inner = self.inner.lock().unwrap();
        let Some(desc) = inner.desc.clone() else {
            return;
        };
        let version = seat.version();
        let Ok(pad) =
            client.create_resource::<ZwpTabletPadV2, _, D>(dh, version, TabletPadUserData { handle: self.clone() })
        else {
            return;
        };
        seat.pad_added(&pad);
        if let Some(node) = desc.syspath.as_deref().and_then(device_node) {
            pad.path(node);
        }
        pad.buttons(desc.buttons);
        let mut instance = PadInstance {
            pad: pad.downgrade(),
            groups: Vec::new(),
            rings: HashMap::new(),
            strips: HashMap::new(),
        };
        for (group_index, group_desc) in desc.groups.iter().enumerate() {
            let Ok(group) = client.create_resource::<ZwpTabletPadGroupV2, _, D>(
                dh,
                version,
                TabletPadGroupUserData { _private: () },
            ) else {
                continue;
            };
            pad.group(&group);
            group.buttons(
                group_desc
                    .buttons
                    .iter()
                    .flat_map(|button| button.to_ne_bytes())
                    .collect(),
            );
            for ring_index in &group_desc.rings {
                if let Ok(ring) = client.create_resource::<ZwpTabletPadRingV2, _, D>(
                    dh,
                    version,
                    TabletPadRingUserData {
                        handle: self.clone(),
                        index: *ring_index,
                        group: group_index,
                    },
                ) {
                    group.ring(&ring);
                    instance.rings.insert(*ring_index, ring.downgrade());
                }
            }
            for strip_index in &group_desc.strips {
                if let Ok(strip) = client.create_resource::<ZwpTabletPadStripV2, _, D>(
                    dh,
                    version,
                    TabletPadStripUserData {
                        handle: self.clone(),
                        index: *strip_index,
                        group: group_index,
                    },
                ) {
                    group.strip(&strip);
                    instance.strips.insert(*strip_index, strip.downgrade());
                }
            }
            group.modes(group_desc.modes.max(1));
            group.done();
            instance.groups.push(group.downgrade());
        }
        pad.done();
        inner.instances.push(instance);
    }

    /// Move pad focus to `surface` (Mutter follows keyboard focus), paired
    /// with `tablet`. Without a paired tablet or surface the pad has no
    /// focus. The new client learns each group's current mode.
    pub fn set_focus(&self, surface: Option<&WlSurface>, tablet: Option<&TabletHandle>, serial: Serial, time: u32) {
        let mut inner = self.inner.lock().unwrap();
        let surface = surface.filter(|s| s.is_alive());
        if inner.focus.as_ref() == surface {
            return;
        }
        // A new focus starts without the old client's labels.
        inner.button_feedback.clear();
        inner.ring_feedback.clear();
        inner.strip_feedback.clear();
        if let Some(old) = inner.focus.take() {
            if old.is_alive() {
                for instance in inner
                    .instances
                    .iter()
                    .filter(|i| i.pad.id().same_client_as(&old.id()))
                {
                    if let Ok(pad) = instance.pad.upgrade() {
                        pad.leave(serial.into(), &old);
                    }
                }
            }
        }
        let (Some(surface), Some(tablet)) = (surface, tablet) else {
            return;
        };
        let Some(wl_tablet) = tablet.instance_for(surface) else {
            return;
        };
        inner.focus = Some(surface.clone());
        let modes = inner.modes.clone();
        let serial_value: u32 = serial.into();
        for slot in &mut inner.mode_switch_serials {
            *slot = serial_value;
        }
        for instance in inner
            .instances
            .iter()
            .filter(|i| i.pad.id().same_client_as(&surface.id()))
        {
            let Ok(pad) = instance.pad.upgrade() else {
                continue;
            };
            pad.enter(serial.into(), &wl_tablet, surface);
            for (group, mode) in instance.groups.iter().zip(&modes) {
                if let Ok(group) = group.upgrade() {
                    group.mode_switch(time, serial.into(), *mode);
                }
            }
        }
    }

    /// The focused client's `set_feedback` label for a button
    /// (`meta_wayland_tablet_pad_get_button_label`'s client part).
    pub fn button_label(&self, button: u32) -> Option<String> {
        self.inner.lock().unwrap().button_feedback.get(&button).cloned()
    }

    /// The focused client's label for ring `index`.
    pub fn ring_label(&self, index: u32) -> Option<String> {
        self.inner.lock().unwrap().ring_feedback.get(&index).cloned()
    }

    /// The focused client's label for strip `index`.
    pub fn strip_label(&self, index: u32) -> Option<String> {
        self.inner.lock().unwrap().strip_feedback.get(&index).cloned()
    }

    /// The surface the pad is focused on.
    pub fn focus(&self) -> Option<WlSurface> {
        self.inner.lock().unwrap().focus.clone()
    }

    /// A pad button changed state.
    pub fn button(&self, button: u32, pressed: bool, time: u32) {
        let inner = self.inner.lock().unwrap();
        if let Some(pad) = inner.focused().and_then(|i| i.pad.upgrade().ok()) {
            pad.button(
                time,
                button,
                if pressed {
                    zwp_tablet_pad_v2::ButtonState::Pressed
                } else {
                    zwp_tablet_pad_v2::ButtonState::Released
                },
            );
        }
    }

    /// A group switched to `mode` (its mode-toggle button was pressed).
    pub fn mode_switch(&self, group: usize, mode: u32, serial: Serial, time: u32) {
        let mut inner = self.inner.lock().unwrap();
        let Some(current) = inner.modes.get_mut(group) else {
            return;
        };
        *current = mode;
        if let Some(slot) = inner.mode_switch_serials.get_mut(group) {
            *slot = serial.into();
        }
        if let Some(group) = inner
            .focused()
            .and_then(|i| i.groups.get(group))
            .and_then(|g| g.upgrade().ok())
        {
            group.mode_switch(time, serial.into(), mode);
        }
    }

    /// Ring `index` moved to `angle` degrees, or stopped (`None`).
    /// `finger` marks a finger on the ring as the source.
    pub fn ring(&self, index: u32, angle: Option<f64>, finger: bool, time: u32) {
        let inner = self.inner.lock().unwrap();
        if let Some(ring) = inner
            .focused()
            .and_then(|i| i.rings.get(&index))
            .and_then(|r| r.upgrade().ok())
        {
            if finger {
                ring.source(zwp_tablet_pad_ring_v2::Source::Finger);
            }
            match angle {
                Some(angle) => ring.angle(angle),
                None => ring.stop(),
            }
            ring.frame(time);
        }
    }

    /// Strip `index` moved to `position` (0..=1), or stopped (`None`).
    pub fn strip(&self, index: u32, position: Option<f64>, finger: bool, time: u32) {
        let inner = self.inner.lock().unwrap();
        if let Some(strip) = inner
            .focused()
            .and_then(|i| i.strips.get(&index))
            .and_then(|s| s.upgrade().ok())
        {
            if finger {
                strip.source(zwp_tablet_pad_strip_v2::Source::Finger);
            }
            match position {
                Some(position) => strip.position((position.clamp(0.0, 1.0) * 65535.0) as u32),
                None => strip.stop(),
            }
            strip.frame(time);
        }
    }

    /// Tell every client the pad is gone; its objects go inert.
    pub(super) fn removed(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.focus = None;
        for instance in std::mem::take(&mut inner.instances) {
            if let Ok(pad) = instance.pad.upgrade() {
                pad.removed();
            }
        }
    }
}

/// User data of ZwpTabletPadV2 object
#[derive(Debug)]
pub struct TabletPadUserData {
    handle: TabletPadHandle,
}

/// User data of ZwpTabletPadGroupV2 object
#[derive(Debug)]
pub struct TabletPadGroupUserData {
    _private: (),
}

/// User data of ZwpTabletPadRingV2 object
#[derive(Debug)]
pub struct TabletPadRingUserData {
    handle: TabletPadHandle,
    index: u32,
    group: usize,
}

/// User data of ZwpTabletPadStripV2 object
#[derive(Debug)]
pub struct TabletPadStripUserData {
    handle: TabletPadHandle,
    index: u32,
    group: usize,
}

impl<D> Dispatch<ZwpTabletPadV2, TabletPadUserData, D> for TabletManagerState
where
    D: Dispatch<ZwpTabletPadV2, TabletPadUserData>,
    D: 'static,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        _pad: &ZwpTabletPadV2,
        request: zwp_tablet_pad_v2::Request,
        data: &TabletPadUserData,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        // tablet_pad_set_feedback: only for the current mode, never for a
        // mode-switch button. (A button taken by a pad action is labelled
        // by the action first, so storing its feedback changes nothing.)
        if let zwp_tablet_pad_v2::Request::SetFeedback {
            button,
            description,
            serial,
        } = request
        {
            let mut inner = data.handle.inner.lock().unwrap();
            let Some((group, desc)) = inner.desc.as_ref().and_then(|desc| {
                desc.groups
                    .iter()
                    .enumerate()
                    .find(|(_, g)| g.buttons.contains(&button))
                    .map(|(i, g)| (i, g.clone()))
            }) else {
                return;
            };
            if !inner.serial_matches(group, serial) || desc.mode_switch_buttons.contains(&button) {
                return;
            }
            inner.button_feedback.insert(button, description);
        }
    }

    fn destroyed(_state: &mut D, _client: ClientId, pad: &ZwpTabletPadV2, data: &TabletPadUserData) {
        data.handle
            .inner
            .lock()
            .unwrap()
            .instances
            .retain(|i| i.pad.id() != pad.id());
    }
}

impl<D> Dispatch<ZwpTabletPadGroupV2, TabletPadGroupUserData, D> for TabletManagerState
where
    D: Dispatch<ZwpTabletPadGroupV2, TabletPadGroupUserData>,
    D: 'static,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        _group: &ZwpTabletPadGroupV2,
        _request: zwp_tablet_pad_group_v2::Request,
        _data: &TabletPadGroupUserData,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
    }
}

impl<D> Dispatch<ZwpTabletPadRingV2, TabletPadRingUserData, D> for TabletManagerState
where
    D: Dispatch<ZwpTabletPadRingV2, TabletPadRingUserData>,
    D: 'static,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        _ring: &ZwpTabletPadRingV2,
        request: zwp_tablet_pad_ring_v2::Request,
        data: &TabletPadRingUserData,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        if let zwp_tablet_pad_ring_v2::Request::SetFeedback { description, serial } = request {
            let mut inner = data.handle.inner.lock().unwrap();
            if inner.serial_matches(data.group, serial) {
                inner.ring_feedback.insert(data.index, description);
            }
        }
    }
}

impl<D> Dispatch<ZwpTabletPadStripV2, TabletPadStripUserData, D> for TabletManagerState
where
    D: Dispatch<ZwpTabletPadStripV2, TabletPadStripUserData>,
    D: 'static,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        _strip: &ZwpTabletPadStripV2,
        request: zwp_tablet_pad_strip_v2::Request,
        data: &TabletPadStripUserData,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        if let zwp_tablet_pad_strip_v2::Request::SetFeedback { description, serial } = request {
            let mut inner = data.handle.inner.lock().unwrap();
            if inner.serial_matches(data.group, serial) {
                inner.strip_feedback.insert(data.index, description);
            }
        }
    }
}
