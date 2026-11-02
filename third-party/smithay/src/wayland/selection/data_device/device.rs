use std::cell::RefCell;

use tracing::debug;
use wayland_server::{
    protocol::{
        wl_data_device::{self, WlDataDevice},
        wl_seat::WlSeat,
    },
    Client, DataInit, Dispatch, DisplayHandle, Resource,
};

use crate::{
    input::{pointer::Focus, Seat, SeatHandler},
    utils::Serial,
    wayland::{
        compositor,
        seat::WaylandFocus,
        selection::{
            device::SelectionDevice,
            offer::OfferReplySource,
            seat_data::SeatData,
            source::{SelectionSource, SelectionSourceProvider},
            SelectionTarget,
        },
    },
};

use super::{dnd_grab, DataDeviceHandler, DataDeviceState};

/// WlSurface role of drag and drop icon
pub const DND_ICON_ROLE: &str = "dnd_icon";

#[doc(hidden)]
#[derive(Debug)]
pub struct DataDeviceUserData {
    pub(crate) wl_seat: WlSeat,
}

impl<D> Dispatch<WlDataDevice, DataDeviceUserData, D> for DataDeviceState
where
    D: Dispatch<WlDataDevice, DataDeviceUserData>,
    D: DataDeviceHandler,
    D: SeatHandler,
    <D as SeatHandler>::PointerFocus: WaylandFocus,
    <D as SeatHandler>::TouchFocus: WaylandFocus,
    <D as SeatHandler>::KeyboardFocus: WaylandFocus,
    D: 'static,
{
    fn request(
        handler: &mut D,
        client: &Client,
        resource: &WlDataDevice,
        request: wl_data_device::Request,
        data: &DataDeviceUserData,
        dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        let seat = match Seat::<D>::from_resource(&data.wl_seat) {
            Some(seat) => seat,
            None => return,
        };

        match request {
            wl_data_device::Request::StartDrag {
                source,
                origin,
                icon,
                serial,
            } => {
                let serial = Serial::from(serial);
                // An existing DND grab is not an implicit press grab, even if
                // it still carries the press serial that started it.
                if super::client_dnd_active(&seat) {
                    return;
                }
                // A serial alone is not authority: a different client can
                // guess it. The origin must own the actual implicit grab.
                if !resource.id().same_client_as(&origin.id()) {
                    return;
                }
                if let Some(pointer) = seat.get_pointer() {
                    if pointer.has_grab(serial) {
                        let start_data = pointer.grab_start_data().unwrap();
                        let owns_origin = start_data
                            .focus
                            .as_ref()
                            .and_then(|(focus, _)| focus.wl_surface())
                            .is_some_and(|surface| surface.as_ref() == &origin);
                        if !owns_origin {
                            return;
                        }
                        if let Some(ref icon) = icon {
                            if compositor::give_role(icon, DND_ICON_ROLE).is_err() {
                                resource.post_error(
                                    wl_data_device::Error::Role,
                                    "Given surface already has an other role",
                                );
                                return;
                            }
                        }
                        // The origin owns this implicit pointer grab.
                        if let Some(source) = &source {
                            source
                                .data::<super::DataSourceUserData>()
                                .unwrap()
                                .inner
                                .lock()
                                .unwrap()
                                .drag_started = true;
                        }
                        handler.started(source.clone(), icon.clone(), seat.clone());
                        pointer.set_grab(
                            handler,
                            dnd_grab::DnDGrab::new_pointer(dh, start_data, source, origin, seat, icon),
                            serial,
                            Focus::Clear,
                        );
                        return;
                    }
                }
                if let Some(touch) = seat.get_touch() {
                    if touch.has_grab(serial) {
                        let start_data = touch.grab_start_data().unwrap();
                        let owns_origin = start_data
                            .focus
                            .as_ref()
                            .and_then(|(focus, _)| focus.wl_surface())
                            .is_some_and(|surface| surface.as_ref() == &origin);
                        if !owns_origin {
                            return;
                        }
                        if let Some(ref icon) = icon {
                            if compositor::give_role(icon, DND_ICON_ROLE).is_err() {
                                resource.post_error(
                                    wl_data_device::Error::Role,
                                    "Given surface already has an other role",
                                );
                                return;
                            }
                        }
                        // The origin owns this implicit touch grab.
                        if let Some(source) = &source {
                            source
                                .data::<super::DataSourceUserData>()
                                .unwrap()
                                .inner
                                .lock()
                                .unwrap()
                                .drag_started = true;
                        }
                        handler.started(source.clone(), icon.clone(), seat.clone());
                        touch.set_grab(
                            handler,
                            dnd_grab::DnDGrab::new_touch(dh, start_data, source, origin, seat, icon),
                            serial,
                        );
                        return;
                    }
                }
                debug!(serial = ?serial, client = ?client, "denying drag from client without implicit grab");
            }
            wl_data_device::Request::SetSelection { source, .. } => {
                if !handler.allow_selection(source.as_ref(), &seat, dh) {
                    return;
                }
                let seat_data = match seat.get_keyboard() {
                    Some(keyboard) if keyboard.client_of_object_has_focus(&resource.id()) => seat
                        .user_data()
                        .get::<RefCell<SeatData<D::SelectionUserData>>>()
                        .unwrap(),
                    _ => {
                        debug!(
                            client = ?client,
                            "denying setting selection by a non-focused client"
                        );
                        return;
                    }
                };
                if let Some(source) = &source {
                    source
                        .data::<super::DataSourceUserData>()
                        .unwrap()
                        .inner
                        .lock()
                        .unwrap()
                        .used_for_selection = true;
                }
                let source = source.map(SelectionSourceProvider::DataDevice);

                handler.new_selection(
                    SelectionTarget::Clipboard,
                    source.clone().map(|provider| SelectionSource { provider }),
                    seat.clone(),
                );

                // The client has kbd focus, it can set the selection
                seat_data
                    .borrow_mut()
                    .set_clipboard_selection::<D>(dh, source.map(OfferReplySource::Client));
            }
            wl_data_device::Request::Release => seat
                .user_data()
                .get::<RefCell<SeatData<D::SelectionUserData>>>()
                .unwrap()
                .borrow_mut()
                .retain_devices(|ndd| match ndd {
                    SelectionDevice::DataDevice(ndd) => ndd != resource,
                    _ => true,
                }),

            _ => unreachable!(),
        }
    }
}
