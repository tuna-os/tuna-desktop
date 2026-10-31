//! Private, ordered OSK text delivery to the supervised IBus context.
use crate::{ClientState, State};
use smithay::reexports::wayland_server::{
    backend::GlobalId, Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};
use smithay::wayland::input_method::InputMethodSeat;

#[allow(dead_code, unused_imports, non_camel_case_types, clippy::all)]
mod server {
    use wayland_server;
    pub mod __interfaces {
        use wayland_server::backend as wayland_backend;
        wayland_scanner::generate_interfaces!("protocols/tuna-osk-input-v1.xml");
    }
    use self::__interfaces::*;
    wayland_scanner::generate_server_code!("protocols/tuna-osk-input-v1.xml");
}
use server::tuna_osk_input_v1::{self, TunaOskInputV1};

pub(crate) struct OskIme {
    _global: GlobalId,
    bridge: Option<TunaOskInputV1>,
}
impl OskIme {
    pub(crate) fn new(dh: &DisplayHandle) -> Self {
        Self {
            _global: dh.create_global::<State, TunaOskInputV1, ()>(1, ()),
            bridge: None,
        }
    }

    /// Transfer owned text to the authorized active context, or return it for
    /// the caller's native/no-context raw fallback without copying.
    pub(crate) fn send(&self, state: &State, text: String) -> Result<(), String> {
        let Some(bridge) = self.bridge.as_ref().filter(|b| b.is_alive()) else {
            return Err(text);
        };
        let Some(client) = bridge.client() else {
            return Err(text);
        };
        let Some(serial) = state.seat.input_method().instance_serial(&client) else {
            return Err(text);
        };
        bridge.text(serial, text);
        Ok(())
    }

    pub(crate) fn current(&self, state: &State, serial: u32) -> bool {
        self.bridge
            .as_ref()
            .and_then(|b| b.client())
            .is_some_and(|client| {
                state.seat.input_method().instance_serial(&client) == Some(serial)
            })
    }
}
fn authorized(client: &Client) -> bool {
    client
        .get_data::<ClientState>()
        .is_some_and(|data| data.ime_bridge)
}
impl GlobalDispatch<TunaOskInputV1, ()> for State {
    fn bind(
        state: &mut Self,
        _dh: &DisplayHandle,
        client: &Client,
        resource: New<TunaOskInputV1>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        if authorized(client) {
            state.protocols.osk_ime.bridge = Some(data_init.init(resource, ()));
        } else {
            data_init.post_error(
                resource,
                0u32,
                "requires supervised input-method connection",
            );
        }
    }
    fn can_view(client: Client, _data: &()) -> bool {
        authorized(&client)
    }
}
impl Dispatch<TunaOskInputV1, ()> for State {
    fn request(
        state: &mut Self,
        client: &Client,
        resource: &TunaOskInputV1,
        request: tuna_osk_input_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        if !authorized(client) || state.protocols.osk_ime.bridge.as_ref() != Some(resource) {
            return;
        }
        if let tuna_osk_input_v1::Request::Unhandled { serial, keysym } = request {
            if !state.protocols.shortcut_locked && state.protocols.osk_ime.current(state, serial) {
                state.osk_ime_unhandled(keysym);
            }
        }
    }
}

impl State {
    /// Run a declined key in protocol request order, before any later IBus
    /// commit. Queuing until the next runtime tick would reorder mixed text.
    fn osk_ime_unhandled(&mut self, keysym: u32) {
        use crate::osk::{level_modifier, pick_keycode};
        use smithay::backend::input::KeyState;
        use smithay::input::keyboard::{xkb, FilterResult};
        use smithay::wayland::input_method::InputMethodKeyboardGrab;
        let Some(keyboard) = self.seat.get_keyboard() else {
            return;
        };
        if self
            .protocols
            .text_inputs
            .focus(keyboard.current_focus().as_ref())
            .is_none()
        {
            return;
        }
        let picked = keyboard.with_xkb_state(self, |context| {
            let xkb = context.xkb().lock().ok()?;
            // SAFETY: keymap/state stay borrowed under the XKB mutex.
            let (map, layout) = unsafe {
                (
                    xkb.keymap(),
                    xkb.state().serialize_layout(xkb::STATE_LAYOUT_EFFECTIVE),
                )
            };
            let (key, level) = pick_keycode(map, layout, keysym)?;
            let modifier = level_modifier(level)
                .and_then(|sym| pick_keycode(map, layout, sym))
                .map(|(code, _)| code);
            Some((key, modifier))
        });
        let Some((key, modifier)) = picked else {
            return;
        };
        let grab = keyboard
            .with_grab(|serial, grab| {
                grab.downcast_ref::<InputMethodKeyboardGrab>()
                    .cloned()
                    .map(|grab| (serial, grab))
            })
            .flatten();
        if grab.is_some() {
            keyboard.unset_grab(self);
        }
        let time = crate::state::system_millis() as u32;
        for (code, pressed) in modifier
            .map(|m| (m, true))
            .into_iter()
            .chain([(key, true), (key, false)])
            .chain(modifier.map(|m| (m, false)))
        {
            keyboard.input::<(), _>(
                self,
                code.into(),
                if pressed {
                    KeyState::Pressed
                } else {
                    KeyState::Released
                },
                smithay::utils::SERIAL_COUNTER.next_serial(),
                time,
                |_, _, _| FilterResult::Forward,
            );
        }
        if let Some((serial, grab)) = grab {
            keyboard.set_grab(self, grab, serial);
        }
    }
}
