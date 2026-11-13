//! A `wp_security_context_v1` listener that keeps serving after a
//! connection could not be accepted (#1071).
//!
//! Smithay's `SecurityContextListenerSource` returns an `accept` error from
//! `process_events`. calloop hands that to whoever dispatches the loop, and
//! the runtime's loop then stops (`RuntimeError::Loop`): a client that used
//! up the compositor's descriptors (EMFILE, ENFILE) while a sandbox engine's
//! listener was registered ended the whole session. libwayland-server, which
//! Mutter listens through, logs a failed accept and keeps listening
//! (`socket_data` in wayland-server.c).
//!
//! [`RestingListener`] reports the error to its callback instead and takes
//! the listener out of the poll for [`REST`]. The listener is
//! level-triggered, so leaving it registered would wake the loop without
//! pause for as long as the connection cannot be accepted; after the rest
//! the connection is still queued and is accepted once descriptors are free
//! again. Smithay's source registers its listening socket and the context's
//! close fd together, so neither is polled during a rest: a context closed
//! meanwhile is removed when the rest ends.

use std::io;
use std::os::unix::net::UnixStream;
use std::time::Duration;

use calloop::timer::{TimeoutAction, Timer};
use calloop::{EventSource, Poll, PostAction, Readiness, Token, TokenFactory};

/// How long a listener rests after a failed accept.
pub const REST: Duration = Duration::from_millis(250);

/// What a [`RestingListener`] reports.
pub(crate) enum Accept {
    /// A client connected.
    Client(UnixStream),
    /// A connection could not be accepted: the listener rests [`REST`] and
    /// tries again. `repeated` when the attempt before this one failed too.
    Failed { error: io::Error, repeated: bool },
}

/// A listener source whose failed accept rests it instead of failing the
/// event loop.
pub(crate) struct RestingListener<S> {
    listener: S,
    /// Whether the poll holds `listener`.
    listening: bool,
    /// The rest in progress, in the poll in the listener's place.
    rest: Option<Timer>,
    /// Whether the latest accept attempt failed.
    failing: bool,
}

impl<S> RestingListener<S> {
    pub(crate) fn new(listener: S) -> Self {
        Self {
            listener,
            listening: false,
            rest: None,
            failing: false,
        }
    }
}

impl<S> EventSource for RestingListener<S>
where
    S: EventSource<Event = UnixStream, Metadata = (), Ret = (), Error = io::Error>,
{
    type Event = Accept;
    type Metadata = ();
    type Ret = ();
    type Error = io::Error;

    fn process_events<F>(
        &mut self,
        readiness: Readiness,
        token: Token,
        mut callback: F,
    ) -> io::Result<PostAction>
    where
        F: FnMut(Accept, &mut ()),
    {
        if let Some(rest) = &mut self.rest {
            let mut over = false;
            rest.process_events(readiness, token, |_, _| {
                over = true;
                TimeoutAction::Drop
            })?;
            if !over {
                // An event the listener's descriptors had queued before
                // they left the poll; they report again once back in it.
                return Ok(PostAction::Continue);
            }
            // A timer that fired has left the timer wheel: dropping it
            // leaves nothing registered, and `reregister` then puts the
            // listener back.
            self.rest = None;
            return Ok(PostAction::Reregister);
        }
        let processed = self.listener.process_events(readiness, token, |stream, _| {
            callback(Accept::Client(stream), &mut ())
        });
        match processed {
            Ok(action) => {
                self.failing = false;
                Ok(action)
            }
            Err(error) => {
                let repeated = std::mem::replace(&mut self.failing, true);
                callback(Accept::Failed { error, repeated }, &mut ());
                self.rest = Some(Timer::from_duration(REST));
                Ok(PostAction::Reregister)
            }
        }
    }

    fn register(
        &mut self,
        poll: &mut Poll,
        token_factory: &mut TokenFactory,
    ) -> calloop::Result<()> {
        match &mut self.rest {
            Some(rest) => rest.register(poll, token_factory),
            None => {
                self.listener.register(poll, token_factory)?;
                self.listening = true;
                Ok(())
            }
        }
    }

    fn reregister(
        &mut self,
        poll: &mut Poll,
        token_factory: &mut TokenFactory,
    ) -> calloop::Result<()> {
        // Not the parts' own `reregister`: this is where the listener and
        // its rest take each other's place in the poll.
        self.unregister(poll)?;
        self.register(poll, token_factory)
    }

    fn unregister(&mut self, poll: &mut Poll) -> calloop::Result<()> {
        if let Some(rest) = &mut self.rest {
            rest.unregister(poll)?;
        }
        if std::mem::take(&mut self.listening) {
            self.listener.unregister(poll)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use calloop::generic::Generic;
    use calloop::{EventLoop, Interest, Mode};
    use std::cell::Cell;
    use std::os::unix::net::UnixListener;
    use std::rc::Rc;
    use std::time::Instant;

    /// Smithay's accept loop on a real listening socket, whose first
    /// `failures` attempts fail as they do with no descriptor left.
    struct Flaky {
        socket: Generic<UnixListener>,
        failures: u32,
        attempts: Rc<Cell<u32>>,
    }

    impl EventSource for Flaky {
        type Event = UnixStream;
        type Metadata = ();
        type Ret = ();
        type Error = io::Error;

        fn process_events<F>(
            &mut self,
            readiness: Readiness,
            token: Token,
            mut callback: F,
        ) -> io::Result<PostAction>
        where
            F: FnMut(UnixStream, &mut ()),
        {
            let failures = &mut self.failures;
            let attempts = &self.attempts;
            self.socket.process_events(readiness, token, |_, socket| {
                attempts.set(attempts.get() + 1);
                if *failures > 0 {
                    *failures -= 1;
                    return Err(io::Error::from_raw_os_error(libc::EMFILE));
                }
                loop {
                    match socket.accept() {
                        Ok((stream, _)) => callback(stream, &mut ()),
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            return Ok(PostAction::Continue);
                        }
                        Err(error) => return Err(error),
                    }
                }
            })
        }

        fn register(
            &mut self,
            poll: &mut Poll,
            token_factory: &mut TokenFactory,
        ) -> calloop::Result<()> {
            self.socket.register(poll, token_factory)
        }

        fn reregister(
            &mut self,
            poll: &mut Poll,
            token_factory: &mut TokenFactory,
        ) -> calloop::Result<()> {
            self.socket.reregister(poll, token_factory)
        }

        fn unregister(&mut self, poll: &mut Poll) -> calloop::Result<()> {
            self.socket.unregister(poll)
        }
    }

    #[derive(Default)]
    struct Seen {
        clients: Vec<UnixStream>,
        /// The OS error of each failed accept, and whether it was reported
        /// as a repeat.
        failures: Vec<(Option<i32>, bool)>,
    }

    /// The regression (#1071): a failed accept ended the event loop. Now the
    /// loop keeps running, nothing wakes it while the listener rests, and
    /// the waiting client is accepted afterwards.
    #[test]
    fn a_failed_accept_rests_the_listener_without_ending_or_spinning_the_loop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("socket");
        let listener = UnixListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        let attempts = Rc::new(Cell::new(0));
        let mut event_loop: EventLoop<'static, Seen> = EventLoop::try_new().unwrap();
        event_loop
            .handle()
            .insert_source(
                RestingListener::new(Flaky {
                    socket: Generic::new(listener, Interest::READ, Mode::Level),
                    failures: 2,
                    attempts: attempts.clone(),
                }),
                |accept, _, seen: &mut Seen| match accept {
                    Accept::Client(stream) => seen.clients.push(stream),
                    Accept::Failed { error, repeated } => {
                        seen.failures.push((error.raw_os_error(), repeated));
                    }
                },
            )
            .map_err(|error| error.error)
            .unwrap();
        let _client = UnixStream::connect(&path).unwrap();

        let mut seen = Seen::default();
        let mut wakeups = 0;
        let started = Instant::now();
        while seen.clients.is_empty() && started.elapsed() < Duration::from_secs(5) {
            // A dispatch that may block far longer than a rest: it returns
            // only when a source woke the loop.
            event_loop
                .dispatch(Some(Duration::from_secs(2)), &mut seen)
                .expect("a failed accept must not fail the event loop");
            wakeups += 1;
        }
        let elapsed = started.elapsed();

        assert_eq!(seen.clients.len(), 1, "the waiting client is accepted");
        assert_eq!(
            seen.failures,
            [(Some(libc::EMFILE), false), (Some(libc::EMFILE), true)]
        );
        assert_eq!(attempts.get(), 3, "one accept attempt per rest");
        assert!(elapsed >= REST * 2, "two failures rest twice: {elapsed:?}");
        // Failure, rest over, failure, rest over, accepted: the pending
        // connection, readable throughout, did not wake the loop meanwhile.
        assert!(wakeups <= 8, "{wakeups} wakeups in {elapsed:?}");
    }
}
