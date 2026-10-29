use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use wayland_protocols::wp::tablet::zv2::server::{
    zwp_tablet_seat_v2::ZwpTabletSeatV2,
    zwp_tablet_v2::{self, ZwpTabletV2},
};
use wayland_server::{
    backend::ClientId, protocol::wl_surface::WlSurface, Client, DataInit, Dispatch, DisplayHandle, Resource,
    Weak,
};

use crate::backend::input::Device;

use super::TabletManagerState;

/// Description of graphics tablet device
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct TabletDescriptor {
    /// Tablet device name
    pub name: String,
    /// Tablet device USB (product,vendor) id
    pub usb_id: Option<(u32, u32)>,
    /// Path to the device
    pub syspath: Option<PathBuf>,
}

impl<D: Device> From<&D> for TabletDescriptor {
    #[inline]
    fn from(device: &D) -> Self {
        TabletDescriptor {
            name: device.name(),
            syspath: device.syspath(),
            usb_id: device.usb_id(),
        }
    }
}

/// The device node behind an evdev sysfs path (`.../input/inputN/eventM`
/// is `/dev/input/eventM`), as Mutter sends `clutter_input_device_get_device_node`.
pub(super) fn device_node(syspath: &Path) -> Option<String> {
    let name = syspath.file_name()?.to_str()?;
    name.starts_with("event").then(|| format!("/dev/input/{name}"))
}

/// The kernel bus type (`BUS_*` of linux/input.h) of an evdev sysfs path,
/// read from its parent input device's `id/bustype`.
fn bustype(syspath: &Path) -> Option<u32> {
    let text = std::fs::read_to_string(syspath.join("device/id/bustype")).ok()?;
    u32::from_str_radix(text.trim(), 16).ok().filter(|bus| *bus != 0)
}

#[derive(Debug, Default)]
struct Tablet {
    instances: Vec<Weak<ZwpTabletV2>>,
}

/// Handle to a tablet device
///
/// Tablet represents one graphics tablet device
#[derive(Debug, Default, Clone)]
pub struct TabletHandle {
    inner: Arc<Mutex<Tablet>>,
}

impl TabletHandle {
    pub(super) fn new_instance<D>(
        &mut self,
        client: &Client,
        dh: &DisplayHandle,
        seat: &ZwpTabletSeatV2,
        tablet: &TabletDescriptor,
    ) where
        D: Dispatch<ZwpTabletV2, TabletUserData>,
        D: 'static,
    {
        let wl_tablet = client
            .create_resource::<ZwpTabletV2, _, D>(dh, seat.version(), TabletUserData { handle: self.clone() })
            .unwrap();

        seat.tablet_added(&wl_tablet);

        wl_tablet.name(tablet.name.clone());

        // `usb_id` is (product, vendor); the event is id(vid, pid).
        if let Some((id_product, id_vendor)) = tablet.usb_id {
            wl_tablet.id(id_vendor, id_product);
        }

        if let Some(node) = tablet.syspath.as_deref().and_then(device_node) {
            wl_tablet.path(node);
        }

        if wl_tablet.version() >= u32::from(zwp_tablet_v2::EVT_BUSTYPE_SINCE) {
            // The protocol's enum names the kernel BUS_* values it knows.
            if let Some(bus) = tablet
                .syspath
                .as_deref()
                .and_then(bustype)
                .and_then(|bus| zwp_tablet_v2::Bustype::try_from(bus).ok())
            {
                wl_tablet.bustype(bus);
            }
        }

        wl_tablet.done();

        self.inner.lock().unwrap().instances.push(wl_tablet.downgrade());
    }

    /// The tablet object `client` holds for this device, if any.
    pub(super) fn instance_for(&self, client: &WlSurface) -> Option<ZwpTabletV2> {
        self.inner
            .lock()
            .unwrap()
            .instances
            .iter()
            .find(|i| i.id().same_client_as(&client.id()))
            .and_then(|t| t.upgrade().ok())
    }

    pub(super) fn with_focused_tablet<F>(&self, focus: &WlSurface, cb: F)
    where
        F: Fn(&ZwpTabletV2),
    {
        if let Some(instance) = self.instance_for(focus) {
            cb(&instance);
        }
    }

    /// Tell every client the device is gone; its objects go inert.
    pub(super) fn removed(&self) {
        let instances = std::mem::take(&mut self.inner.lock().unwrap().instances);
        for instance in instances.iter().filter_map(|i| i.upgrade().ok()) {
            instance.removed();
        }
    }
}

/// User data of ZwpTabletV2 object
#[derive(Debug)]
pub struct TabletUserData {
    handle: TabletHandle,
}

impl<D> Dispatch<ZwpTabletV2, TabletUserData, D> for TabletManagerState
where
    D: Dispatch<ZwpTabletV2, TabletUserData>,
    D: 'static,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        _tablet: &ZwpTabletV2,
        _request: zwp_tablet_v2::Request,
        _data: &TabletUserData,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
    }

    fn destroyed(_state: &mut D, _client: ClientId, tablet: &ZwpTabletV2, data: &TabletUserData) {
        data.handle
            .inner
            .lock()
            .unwrap()
            .instances
            .retain(|i| i.id() != Resource::id(tablet));
    }
}
