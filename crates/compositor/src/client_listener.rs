//! The session's Wayland socket, still serving after a connection could not
//! be accepted.
//!
//! Smithay's `ListeningSocketSource` hands an `accept` error to the event
//! loop, which then stops: a client flood that ran out of descriptors
//! (EMFILE) ended the whole session. libwayland-server, which Mutter listens
//! through, logs a failed accept and keeps listening (`socket_data` in
//! wayland-server.c). Here the socket also rests briefly after a failure, so
//! a connection that cannot be accepted yet is not retried in a busy loop;
//! it is accepted once descriptors are free again.

use std::cell::Cell;
use std::io;
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::rc::Rc;
use std::time::Duration;

use calloop::generic::Generic;
use calloop::timer::{TimeoutAction, Timer};
use calloop::{Interest, LoopHandle, Mode, PostAction, RegistrationToken};
use smithay::reexports::wayland_server::ListeningSocket;

/// How long the socket rests after a failed accept.
const PAUSE: Duration = Duration::from_millis(250);

/// Accept every pending connection, stopping at the first failure.
fn drain(
    mut accept: impl FnMut() -> io::Result<Option<UnixStream>>,
    mut on_client: impl FnMut(UnixStream),
) -> io::Result<()> {
    loop {
        match accept() {
            Ok(Some(stream)) => on_client(stream),
            Ok(None) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

/// Listen on `$XDG_RUNTIME_DIR/name`, handing each accepted client to
/// `on_client`.
pub fn listen<D: 'static>(
    handle: &LoopHandle<'static, D>,
    name: &str,
    on_client: impl FnMut(UnixStream, &mut D) + 'static,
) -> io::Result<RegistrationToken> {
    let socket = ListeningSocket::bind(name).map_err(io::Error::other)?;
    insert(handle, socket, ListeningSocket::accept, on_client)
}

/// [`listen`] on any readable `socket` whose connections `accept` takes.
fn insert<D: 'static, S: AsFd + 'static>(
    handle: &LoopHandle<'static, D>,
    socket: S,
    mut accept: impl FnMut(&S) -> io::Result<Option<UnixStream>> + 'static,
    mut on_client: impl FnMut(UnixStream, &mut D) + 'static,
) -> io::Result<RegistrationToken> {
    let token: Rc<Cell<Option<RegistrationToken>>> = Rc::default();
    let own = token.clone();
    let rest_handle = handle.clone();
    let mut failing = false;
    let registered = handle
        .insert_source(
            Generic::new(socket, Interest::READ, Mode::Level),
            move |_, socket, data| match drain(|| accept(socket), |s| on_client(s, data)) {
                Ok(()) => {
                    failing = false;
                    Ok(PostAction::Continue)
                }
                Err(error) => {
                    if !failing {
                        eprintln!(
                            "tuna-compositor: cannot accept a Wayland client ({error}); \
                             the socket rests {} ms and retries",
                            PAUSE.as_millis()
                        );
                    }
                    failing = true;
                    let Some(token) = own.get() else {
                        return Ok(PostAction::Continue);
                    };
                    let enable = rest_handle.clone();
                    let rested =
                        rest_handle.insert_source(Timer::from_duration(PAUSE), move |_, _, _| {
                            let _ = enable.enable(&token);
                            TimeoutAction::Drop
                        });
                    // Without a timer, keep listening rather than stay off.
                    Ok(if rested.is_ok() {
                        PostAction::Disable
                    } else {
                        PostAction::Continue
                    })
                }
            },
        )
        .map_err(|e| io::Error::other(e.error))?;
    token.set(Some(registered));
    Ok(registered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::os::unix::net::UnixListener;
    use std::time::Instant;

    fn emfile() -> io::Error {
        io::Error::from_raw_os_error(libc::EMFILE)
    }

    #[test]
    fn accepts_every_pending_connection() {
        let mut queue: VecDeque<io::Result<Option<UnixStream>>> = VecDeque::from([
            Ok(Some(UnixStream::pair().unwrap().0)),
            Err(io::Error::from(io::ErrorKind::Interrupted)),
            Ok(Some(UnixStream::pair().unwrap().0)),
            Ok(None),
        ]);
        let mut accepted = 0;
        drain(|| queue.pop_front().unwrap(), |_| accepted += 1).unwrap();
        assert_eq!(accepted, 2);
        assert!(queue.is_empty());
    }

    /// The regression (#203): with descriptors exhausted, a failed accept
    /// ended the event loop and the session. Now the loop keeps running,
    /// the socket rests, and the waiting client is accepted afterwards.
    #[test]
    fn exhausted_descriptors_pause_the_socket_without_ending_the_loop() {
        let dir = std::env::temp_dir().join(format!("tuna-listener-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("socket");
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut event_loop: calloop::EventLoop<'static, Vec<UnixStream>> =
            calloop::EventLoop::try_new().unwrap();
        let failures = Rc::new(Cell::new(2));
        let remaining = failures.clone();
        let attempts = Rc::new(Cell::new(0));
        let counted = attempts.clone();
        insert(
            &event_loop.handle(),
            listener,
            move |listener: &UnixListener| {
                counted.set(counted.get() + 1);
                if remaining.get() > 0 {
                    remaining.set(remaining.get() - 1);
                    return Err(emfile());
                }
                match listener.accept() {
                    Ok((stream, _)) => Ok(Some(stream)),
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(None),
                    Err(e) => Err(e),
                }
            },
            |stream, accepted: &mut Vec<UnixStream>| accepted.push(stream),
        )
        .unwrap();
        let _client = UnixStream::connect(&path).unwrap();
        let mut accepted = Vec::new();
        let started = Instant::now();
        while accepted.is_empty() && started.elapsed() < Duration::from_secs(5) {
            event_loop
                .dispatch(Some(Duration::from_millis(20)), &mut accepted)
                .expect("a failed accept must not fail the event loop");
        }
        assert_eq!(
            accepted.len(),
            1,
            "the waiting client is accepted afterwards"
        );
        assert_eq!(failures.get(), 0);
        // Two failures rest the socket twice instead of spinning on it.
        assert!(started.elapsed() >= PAUSE * 2);
        assert!(attempts.get() <= 4, "{} accept attempts", attempts.get());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
