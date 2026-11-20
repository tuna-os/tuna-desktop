//! The login screen's side of the compositor's control socket
//! (`TUNA_CONTROL_SOCKET`, only this supervised process may hold it).
//!
//! GNOME Shell's login screen is part of Mutter's process, so it switches
//! the screen reader, the keymap and the input aids directly; here those
//! are the control protocol's commands, as the shell sends them for a
//! user: `SetScreenReader` (the compositor runs the trusted Orca, as for
//! the lock screen), `SetInputSettings` (the greeter user's keymap,
//! keyboard and pointer aids and bell) and `SwitchInputSource`. Nothing
//! the compositor sends is needed: frames are read and dropped so the
//! socket never backs up.

use std::cell::{Cell, RefCell};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::rc::Rc;

use gtk4::glib;
use tuna_shell_control::{CommandKind, InputSettings, Message, CURRENT_VERSION};

pub struct Control {
    stream: RefCell<Option<UnixStream>>,
    next_id: Cell<u64>,
}

impl Control {
    /// Connect to the compositor's control socket, when there is one.
    pub fn connect() -> Rc<Self> {
        Self::connect_to(std::env::var_os("TUNA_CONTROL_SOCKET"))
    }

    /// Connect to the socket at `path`; without one (a login screen the
    /// compositor does not supervise), or when it cannot be dialled,
    /// every command is dropped.
    fn connect_to(path: Option<OsString>) -> Rc<Self> {
        let control = Rc::new(Self {
            stream: RefCell::new(None),
            next_id: Cell::new(1),
        });
        if let Some(path) = path {
            match dial(Path::new(&path)) {
                Ok(stream) => control.attach(stream),
                Err(error) => eprintln!("tuna-greeter: compositor control unavailable: {error}"),
            }
        }
        control
    }

    /// Read and drop the compositor's frames from the main loop.
    fn attach(self: &Rc<Self>, stream: UnixStream) {
        let socket = match stream
            .try_clone()
            .map_err(|e| e.to_string())
            .and_then(|dup| gio::Socket::from_fd(dup.into()).map_err(|e| e.to_string()))
        {
            Ok(socket) => socket,
            Err(error) => {
                eprintln!("tuna-greeter: compositor control: {error}");
                return;
            }
        };
        let weak = Rc::downgrade(self);
        let source = gio::prelude::SocketExtManual::create_source(
            &socket,
            glib::IOCondition::IN | glib::IOCondition::HUP | glib::IOCondition::ERR,
            None::<&gio::Cancellable>,
            Some("tuna-greeter control"),
            glib::Priority::DEFAULT,
            move |_, _| {
                let Some(control) = weak.upgrade() else {
                    return glib::ControlFlow::Break;
                };
                if control.drain() {
                    glib::ControlFlow::Continue
                } else {
                    control.stream.borrow_mut().take();
                    eprintln!("tuna-greeter: compositor control closed");
                    glib::ControlFlow::Break
                }
            },
        );
        // The attached source keeps the socket until it returns Break.
        source.attach(None);
        *self.stream.borrow_mut() = Some(stream);
        crate::trace("greeter control-connected");
    }

    /// Read what the compositor sent; `false` once the socket is gone.
    fn drain(&self) -> bool {
        let mut stream = self.stream.borrow_mut();
        let Some(stream) = stream.as_mut() else {
            return false;
        };
        let mut chunk = [0u8; 16384];
        loop {
            match stream.read(&mut chunk) {
                Ok(0) => return false,
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return true,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return false,
            }
        }
    }

    fn send(&self, kind: CommandKind) {
        let id = self.next_id.get();
        self.next_id.set(id + 1);
        let frame = tuna_shell_control::encode_frame(&Message::Command { id, kind });
        if let Some(stream) = self.stream.borrow_mut().as_mut() {
            if let Err(error) = write_frame(stream, &frame) {
                eprintln!("tuna-greeter: compositor control: {error}");
            }
        }
    }

    /// `screen-reader-enabled`: the compositor runs Orca for the login
    /// screen.
    pub fn set_screen_reader(&self, enabled: bool) {
        self.send(CommandKind::SetScreenReader { enabled });
    }

    /// The greeter user's input settings.
    pub fn set_input_settings(&self, settings: InputSettings) {
        self.send(CommandKind::SetInputSettings(Box::new(settings)));
    }

    /// One step to the next (or previous) input source.
    pub fn switch_input_source(&self, backward: bool) {
        self.send(CommandKind::SwitchInputSource { backward });
    }
}

/// Connect to the control socket at `path` and greet it.
fn dial(path: &Path) -> std::io::Result<UnixStream> {
    greet(UnixStream::connect(path)?)
}

/// Say `Hello` at this build's protocol version, then go nonblocking for
/// the main loop.
fn greet(stream: UnixStream) -> std::io::Result<UnixStream> {
    let hello = tuna_shell_control::encode_frame(&Message::Hello {
        version: CURRENT_VERSION,
    });
    (&stream).write_all(&hello)?;
    stream.set_nonblocking(true)?;
    Ok(stream)
}

/// Write a whole frame on the nonblocking socket, waiting out a full
/// buffer (a frame is small; the compositor reads every turn).
fn write_frame(stream: &mut UnixStream, mut frame: &[u8]) -> std::io::Result<()> {
    while !frame.is_empty() {
        match stream.write(frame) {
            Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
            Ok(n) => frame = &frame[n..],
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// A greeter's control connection to a test's compositor end.
#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    /// A connected `Control` and the compositor's end of its socket.
    pub(crate) fn pair() -> (Rc<Control>, UnixStream) {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let control = Control::connect_to(None);
        control.attach(greet(ours).unwrap());
        (control, theirs)
    }

    /// The next whole frame the compositor end reads, raw.
    pub(crate) fn read_frame(stream: &mut UnixStream) -> Vec<u8> {
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut frame = vec![0u8; 4];
        stream.read_exact(&mut frame).unwrap();
        let len = u32::from_le_bytes(frame[..4].try_into().unwrap()) as usize;
        frame.resize(4 + len, 0);
        stream.read_exact(&mut frame[4..]).unwrap();
        frame
    }

    /// The next message, decoded as the compositor decodes it.
    pub(crate) fn read_message(stream: &mut UnixStream) -> Message {
        tuna_shell_control::decode_frame(&read_frame(stream)).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;

    fn hello() -> Message {
        Message::Hello {
            version: CURRENT_VERSION,
        }
    }

    #[test]
    fn commands_reach_the_socket_as_the_schemas_frames_after_hello() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("control");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let control = Control::connect_to(Some(path.into_os_string()));
        assert!(control.stream.borrow().is_some());
        let (mut compositor, _) = listener.accept().unwrap();
        assert_eq!(read_message(&mut compositor), hello());

        let settings = InputSettings {
            xkb_layout: "us,de".to_owned(),
            xkb_variant: ",".to_owned(),
            ..InputSettings::default()
        };
        control.set_screen_reader(true);
        control.set_input_settings(settings.clone());
        control.switch_input_source(true);
        control.set_screen_reader(false);
        let expected = [
            CommandKind::SetScreenReader { enabled: true },
            CommandKind::SetInputSettings(Box::new(settings)),
            CommandKind::SwitchInputSource { backward: true },
            CommandKind::SetScreenReader { enabled: false },
        ];
        for (id, kind) in (1..).zip(expected) {
            let message = Message::Command { id, kind };
            let frame = read_frame(&mut compositor);
            assert_eq!(frame, tuna_shell_control::encode_frame(&message));
            assert_eq!(tuna_shell_control::decode_frame(&frame).unwrap(), message);
        }
    }

    #[test]
    fn without_a_control_socket_commands_are_dropped() {
        let control = Control::connect_to(None);
        assert!(control.stream.borrow().is_none());
        control.set_screen_reader(true);
        control.switch_input_source(false);
        assert!(!control.drain());
    }

    #[test]
    fn a_socket_that_cannot_be_dialled_leaves_the_greeter_without_control() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing");
        assert_eq!(
            dial(&missing).unwrap_err().kind(),
            std::io::ErrorKind::NotFound
        );
        let control = Control::connect_to(Some(missing.into_os_string()));
        assert!(control.stream.borrow().is_none());
        control.set_screen_reader(true);

        // A compositor gone since it bound the socket, and a plain file.
        let stale = dir.path().join("stale");
        drop(std::os::unix::net::UnixListener::bind(&stale).unwrap());
        let file = dir.path().join("file");
        std::fs::write(&file, b"").unwrap();
        for path in [stale, file] {
            assert_eq!(
                dial(&path).unwrap_err().kind(),
                std::io::ErrorKind::ConnectionRefused,
                "{path:?}"
            );
            assert!(Control::connect_to(Some(path.into_os_string()))
                .stream
                .borrow()
                .is_none());
        }
    }

    #[test]
    fn write_frame_waits_out_a_full_socket_buffer() {
        let (mut writer, mut reader) = UnixStream::pair().unwrap();
        writer.set_nonblocking(true).unwrap();
        // Far beyond the socket buffer: the writer must wait for the reader.
        let frame: Vec<u8> = (0..4 << 20).map(|i: u32| i as u8).collect();
        let len = frame.len();
        let read = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(50));
            let mut got = vec![0u8; len];
            reader.read_exact(&mut got).unwrap();
            got
        });
        write_frame(&mut writer, &frame).unwrap();
        assert_eq!(read.join().unwrap(), frame);
    }

    #[test]
    fn write_frame_reports_a_closed_compositor() {
        let (mut writer, reader) = UnixStream::pair().unwrap();
        writer.set_nonblocking(true).unwrap();
        drop(reader);
        assert_eq!(
            write_frame(&mut writer, b"frame").unwrap_err().kind(),
            std::io::ErrorKind::BrokenPipe
        );
    }

    #[test]
    fn compositor_frames_are_read_and_dropped_until_it_closes() {
        let (control, mut compositor) = pair();
        assert_eq!(read_message(&mut compositor), hello());
        compositor.write_all(&[7u8; 40_000]).unwrap();
        assert!(control.drain());
        // Nothing is left to read: a second drain finds the socket empty.
        assert!(control.drain());
        drop(compositor);
        assert!(!control.drain());
    }
}
