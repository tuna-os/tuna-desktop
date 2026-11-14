//! An InputCapture session's EIS context (mutter's libeis side of
//! `meta-input-capture-session.c`): the inverse of RemoteDesktop's
//! (`remote_eis.rs`). The client is a *receiver*; the compositor sends
//! it what the [`Router`](super::input_capture::Router) captured, on a
//! "captured relative pointer" (pointer, button, scroll; one region
//! per zone) and a "captured keyboard" (the session keymap), between
//! `start_emulating` with the activation id and `stop_emulating`.
use super::input_capture::{CaptureShared, CapturedInput, EisCommand, EisLink, Zones};
use reis::{
    calloop::{EisRequestSource, EisRequestSourceEvent},
    eis,
    request::{Connection, Device, DeviceCapability, EisRequest, Seat},
};
use std::{
    io,
    os::unix::net::UnixStream,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Weak,
    },
    time::Duration,
};
use zbus::fdo;

struct Worker {
    session: Weak<CaptureShared>,
    connection: Option<Connection>,
    seat: Option<Seat>,
    want_pointer: bool,
    want_keyboard: bool,
    pointer: Option<Device>,
    keyboard: Option<Device>,
    keymap: std::fs::File,
    keymap_len: u32,
    zones: Zones,
    /// The activation the devices emulate for.
    emulating: Option<u32>,
    clock: smithay::utils::Clock<smithay::utils::Monotonic>,
    done: bool,
    bindings: u32,
}

impl Worker {
    fn event(
        &mut self,
        event: EisRequestSourceEvent,
        connection: &Connection,
    ) -> calloop::PostAction {
        self.connection = Some(connection.clone());
        match event {
            EisRequestSourceEvent::Connected => {
                // mutter refuses a sender client on an input capture
                // session.
                if connection.context_type() != eis::handshake::ContextType::Receiver {
                    self.done = true;
                    return calloop::PostAction::Remove;
                }
                self.seat = Some(connection.add_seat(
                    Some("seat0"),
                    DeviceCapability::Pointer
                        | DeviceCapability::Button
                        | DeviceCapability::Scroll
                        | DeviceCapability::Keyboard,
                ));
            }
            EisRequestSourceEvent::Request(EisRequest::Disconnect) => {
                self.done = true;
                return calloop::PostAction::Remove;
            }
            EisRequestSourceEvent::Request(EisRequest::Bind(bind)) => {
                // Rebinding creates devices and keymap FDs: bound churn.
                self.bindings += 1;
                if self.bindings > 32 {
                    self.done = true;
                    return calloop::PostAction::Remove;
                }
                self.want_pointer = bind.capabilities.contains(
                    DeviceCapability::Pointer | DeviceCapability::Button | DeviceCapability::Scroll,
                );
                self.want_keyboard = bind.capabilities.contains(DeviceCapability::Keyboard);
                if !self.want_pointer && !self.want_keyboard {
                    // Nothing could receive the capture: mutter disables.
                    if let Some(session) = self.session.upgrade() {
                        session.disable_by_compositor();
                    }
                } else {
                    self.ensure_devices();
                }
            }
            EisRequestSourceEvent::Request(EisRequest::DeviceClosed(closed)) => {
                if self.pointer.as_ref() == Some(&closed.device) {
                    remove(&mut self.pointer);
                } else if self.keyboard.as_ref() == Some(&closed.device) {
                    remove(&mut self.keyboard);
                }
            }
            // A receiver has nothing else to say.
            EisRequestSourceEvent::Request(_) => {}
        }
        self.flush()
    }

    fn command(&mut self, command: EisCommand) -> calloop::PostAction {
        match command {
            EisCommand::Start(activation) => {
                self.emulating = Some(activation);
                for device in self.devices() {
                    device.start_emulating(activation);
                }
            }
            EisCommand::Stop => {
                if self.emulating.take().is_some() {
                    for device in self.devices() {
                        device.stop_emulating();
                    }
                }
            }
            EisCommand::Devices(true) => self.ensure_devices(),
            EisCommand::Devices(false) => {
                remove(&mut self.pointer);
                remove(&mut self.keyboard);
            }
            EisCommand::Zones(zones) => {
                self.zones = zones;
                // New regions need a new device.
                if let Some(pointer) = self.pointer.take() {
                    pointer.remove();
                    self.ensure_devices();
                }
            }
            EisCommand::Input(input) => self.input(input),
            EisCommand::Close => {
                self.done = true;
                return calloop::PostAction::Remove;
            }
        }
        self.flush()
    }

    fn devices(&self) -> impl Iterator<Item = &Device> {
        self.pointer.iter().chain(self.keyboard.iter())
    }

    fn flush(&mut self) -> calloop::PostAction {
        if let Some(connection) = &self.connection {
            if connection.flush().is_err() {
                self.done = true;
                return calloop::PostAction::Remove;
            }
        }
        calloop::PostAction::Continue
    }

    /// mutter's `ensure_eis_devices`.
    fn ensure_devices(&mut self) {
        let Some(seat) = self.seat.clone() else {
            return;
        };
        if !self.want_pointer {
            remove(&mut self.pointer);
        } else if self.pointer.is_none() {
            let origin = (
                self.zones.rects.iter().map(|z| z.x).min().unwrap_or(0),
                self.zones.rects.iter().map(|z| z.y).min().unwrap_or(0),
            );
            let pointer = seat.add_device(
                Some("captured relative pointer"),
                eis::device::DeviceType::Virtual,
                DeviceCapability::Pointer | DeviceCapability::Button | DeviceCapability::Scroll,
                |device| {
                    for zone in &self.zones.rects {
                        device.device().region(
                            zone.x.abs_diff(origin.0),
                            zone.y.abs_diff(origin.1),
                            zone.width.max(1) as u32,
                            zone.height.max(1) as u32,
                            zone.scale as f32,
                        );
                    }
                },
            );
            self.pointer = Some(self.resume(pointer));
        }
        if !self.want_keyboard {
            remove(&mut self.keyboard);
        } else if self.keyboard.is_none() {
            let keyboard = seat.add_device(
                Some("captured keyboard"),
                eis::device::DeviceType::Virtual,
                DeviceCapability::Keyboard.into(),
                |device| {
                    if let Some(keyboard) = device.interface::<eis::Keyboard>() {
                        super::remote_eis::send_keymap(&keyboard, &self.keymap, self.keymap_len);
                    }
                },
            );
            self.keyboard = Some(self.resume(keyboard));
        }
    }

    fn resume(&self, device: Device) -> Device {
        device.resumed();
        if let Some(activation) = self.emulating {
            device.start_emulating(activation);
        }
        device
    }

    /// mutter's `meta_input_capture_session_process_event`, after the
    /// router: one event and its frame. Nothing goes out unless the
    /// devices are emulating for an activation.
    fn input(&mut self, input: CapturedInput) {
        if self.emulating.is_none() {
            return;
        }
        let time = Duration::from(self.clock.now()).as_micros() as u64;
        let device = match input {
            CapturedInput::Key { .. } => self.keyboard.as_ref(),
            _ => self.pointer.as_ref(),
        };
        let Some(device) = device else {
            return;
        };
        match input {
            CapturedInput::Motion { dx, dy } => {
                let Some(pointer) = device.interface::<eis::Pointer>() else {
                    return;
                };
                pointer.motion_relative(dx as f32, dy as f32);
            }
            CapturedInput::Button { button, pressed } => {
                let Some(buttons) = device.interface::<eis::Button>() else {
                    return;
                };
                buttons.button(
                    button,
                    if pressed {
                        eis::button::ButtonState::Press
                    } else {
                        eis::button::ButtonState::Released
                    },
                );
            }
            CapturedInput::ScrollDiscrete { dx, dy } => {
                let Some(scroll) = device.interface::<eis::Scroll>() else {
                    return;
                };
                scroll.scroll_discrete(dx, dy);
            }
            CapturedInput::Scroll { dx, dy, stop } => {
                let Some(scroll) = device.interface::<eis::Scroll>() else {
                    return;
                };
                if dx != 0.0 || dy != 0.0 {
                    scroll.scroll(dx as f32, dy as f32);
                }
                if stop.0 || stop.1 {
                    scroll.scroll_stop(u32::from(stop.0), u32::from(stop.1), 0);
                }
            }
            CapturedInput::Key { key, pressed } => {
                let Some(keyboard) = device.interface::<eis::Keyboard>() else {
                    return;
                };
                keyboard.key(
                    key,
                    if pressed {
                        eis::keyboard::KeyState::Press
                    } else {
                        eis::keyboard::KeyState::Released
                    },
                );
            }
        }
        device.frame(time);
    }
}

/// Serve one receiver client on `socket` for `session` (ConnectToEIS).
pub(crate) fn start(
    socket: UnixStream,
    session: &Arc<CaptureShared>,
    keymap: String,
) -> fdo::Result<()> {
    let failed = |e: io::Error| fdo::Error::Failed(format!("Failed to create EIS context: {e}"));
    let (keymap, keymap_len) =
        super::remote_eis::sealed_keymap("tuna-input-capture-keymap", &keymap).map_err(failed)?;
    let context = eis::Context::new(socket).map_err(failed)?;
    let (commands, receiver) = calloop::channel::channel();
    let alive = Arc::new(AtomicBool::new(true));
    let (zones, emulating) = session.attach_eis(EisLink {
        commands,
        alive: alive.clone(),
    })?;
    let weak = Arc::downgrade(session);
    let thread_alive = alive.clone();
    std::thread::Builder::new()
        .name(format!("tuna-capture-eis-{}", session.id))
        .stack_size(256 * 1024)
        .spawn(move || {
            let _alive = AliveGuard(thread_alive);
            let mut worker = Worker {
                session: weak,
                connection: None,
                seat: None,
                want_pointer: false,
                want_keyboard: false,
                pointer: None,
                keyboard: None,
                keymap,
                keymap_len,
                zones,
                emulating,
                clock: smithay::utils::Clock::new(),
                done: false,
                bindings: 0,
            };
            let Ok(mut event_loop) = calloop::EventLoop::<Worker>::try_new() else {
                return;
            };
            let handle = event_loop.handle();
            let inserted = handle
                .insert_source(
                    EisRequestSource::new(context, 1),
                    |event, connection, worker| {
                        Ok(match event {
                            Ok(event) => worker.event(event, connection),
                            Err(_) => {
                                worker.done = true;
                                calloop::PostAction::Remove
                            }
                        })
                    },
                )
                .is_ok()
                && handle
                    .insert_source(receiver, |event, _, worker: &mut Worker| match event {
                        calloop::channel::Event::Msg(command) => {
                            let _ = worker.command(command);
                        }
                        calloop::channel::Event::Closed => worker.done = true,
                    })
                    .is_ok();
            if !inserted {
                return;
            }
            while !worker.done {
                if event_loop
                    .dispatch(Some(Duration::from_millis(100)), &mut worker)
                    .is_err()
                {
                    break;
                }
            }
            if let Some(connection) = worker.connection.take() {
                connection.disconnected(reis::ei::connection::DisconnectReason::Disconnected, None);
                let _ = connection.flush();
            }
        })
        .map_err(|e| {
            alive.store(false, Ordering::SeqCst);
            fdo::Error::Failed(format!("Failed to create EIS context: {e}"))
        })?;
    Ok(())
}

/// The session may take a new client once this worker is gone.
struct AliveGuard(Arc<AtomicBool>);
impl Drop for AliveGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// mutter's `clear_eis_pointer`/`clear_eis_keyboard`.
fn remove(device: &mut Option<Device>) {
    if let Some(device) = device.take() {
        device.remove();
    }
}
