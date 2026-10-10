//! Coalesced local UI work, with an optional genuine one-shot deadline.
//! Socket callbacks dispatch directly: deferring a level-triggered socket to
//! an idle source would leave it readable and starve the idle dispatcher.
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Instant;

#[derive(Default)]
pub struct UiWake {
    pending: Cell<bool>,
    callback: RefCell<Option<Rc<dyn Fn()>>>,
    timer: RefCell<Option<(Instant, glib::SourceId)>>,
}

impl UiWake {
    pub fn install(self: &Rc<Self>, callback: impl Fn() + 'static) {
        *self.callback.borrow_mut() = Some(Rc::new(callback));
        self.pending.set(false);
        self.request();
    }

    pub fn request(self: &Rc<Self>) {
        if self.callback.borrow().is_none() || self.pending.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            if let Some(wake) = weak.upgrade() {
                if wake.pending.get() {
                    wake.dispatch();
                }
            }
        });
    }

    pub fn dispatch(&self) {
        self.pending.set(false);
        let callback = self.callback.borrow().clone();
        if let Some(callback) = callback {
            callback();
        }
    }

    pub fn at(self: &Rc<Self>, deadline: Option<Instant>) {
        if self.timer.borrow().as_ref().map(|(at, _)| *at) == deadline {
            return;
        }
        if let Some((_, timer)) = self.timer.borrow_mut().take() {
            timer.remove();
        }
        if let Some(at) = deadline {
            let weak = Rc::downgrade(self);
            let timer = glib::timeout_add_local_once(
                at.saturating_duration_since(Instant::now()),
                move || {
                    if let Some(wake) = weak.upgrade() {
                        wake.timer.borrow_mut().take();
                        wake.request();
                    }
                },
            );
            *self.timer.borrow_mut() = Some((at, timer));
        }
    }
}

impl Drop for UiWake {
    fn drop(&mut self) {
        if let Some((_, timer)) = self.timer.get_mut().take() {
            timer.remove();
        }
    }
}

/// Main-context socket source. The caller drains synchronously and returns
/// false after terminal transport failure. GSocket retains the duplicated fd
/// until the source is destroyed; no raw borrowed-fd watcher can outlive it.
pub fn watch_socket(
    socket: &gio::Socket,
    drain: impl FnMut(glib::IOCondition) -> bool + 'static,
) -> glib::Source {
    watch_socket_conditions(
        socket,
        glib::IOCondition::IN | glib::IOCondition::HUP | glib::IOCondition::ERR,
        drain,
    )
}

pub fn watch_socket_conditions(
    socket: &gio::Socket,
    condition: glib::IOCondition,
    mut drain: impl FnMut(glib::IOCondition) -> bool + 'static,
) -> glib::Source {
    let source = gio::prelude::SocketExtManual::create_source(
        socket,
        condition,
        None::<&gio::Cancellable>,
        Some("tuna-control-ready"),
        glib::Priority::DEFAULT,
        move |_, condition| {
            if drain(condition) {
                glib::ControlFlow::Continue
            } else {
                glib::ControlFlow::Break
            }
        },
    );
    source.attach(None);
    source
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::fd::AsFd;
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    // One test owns the default main context throughout: no GTK display,
    // compositor or installed services are needed for scheduling semantics.
    #[test]
    fn coalescing_deadline_cancellation_socket_drain_and_hup_retirement() {
        let context = glib::MainContext::default();
        let _owner = context.acquire().unwrap();
        let wake = Rc::new(UiWake::default());
        let calls = Rc::new(Cell::new(0));
        let count = calls.clone();
        wake.install(move || count.set(count.get() + 1));
        for _ in 0..100 {
            wake.request();
        }
        while context.pending() {
            context.iteration(false);
        }
        assert_eq!(calls.get(), 1);
        wake.dispatch();
        assert_eq!(calls.get(), 2);
        wake.at(Some(Instant::now() + Duration::from_millis(10)));
        wake.at(None);
        std::thread::sleep(Duration::from_millis(20));
        while context.pending() {
            context.iteration(false);
        }
        assert_eq!(calls.get(), 2);
        wake.at(Some(Instant::now()));
        while context.pending() {
            context.iteration(false);
        }
        assert_eq!(calls.get(), 3);
        assert!(wake.timer.borrow().is_none());
        wake.request();
        wake.dispatch(); // Direct readiness consumes the pending idle work.
        while context.pending() {
            context.iteration(false);
        }
        assert_eq!(calls.get(), 4);

        let (mut stream, mut peer) = UnixStream::pair().unwrap();
        stream.set_nonblocking(true).unwrap();
        let socket = gio::Socket::from_fd(stream.as_fd().try_clone_to_owned().unwrap()).unwrap();
        let reads = Rc::new(Cell::new(0));
        let received = Rc::new(Cell::new(0));
        let callback_reads = reads.clone();
        let callback_received = received.clone();
        let source = watch_socket(&socket, move |_| {
            callback_reads.set(callback_reads.get() + 1);
            let mut bytes = [0u8; 32];
            loop {
                match stream.read(&mut bytes) {
                    Ok(0) => return false,
                    Ok(n) => callback_received.set(callback_received.get() + n),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return true,
                    Err(error) => panic!("socket read failed: {error}"),
                }
            }
        });
        while context.pending() {
            context.iteration(false);
        }
        assert_eq!(reads.get(), 0, "an empty socket never dispatches");
        peer.write_all(&[1; 100]).unwrap();
        while context.pending() {
            context.iteration(false);
        }
        assert_eq!(received.get(), 100);
        assert_eq!(reads.get(), 1, "drain avoids readable-source spin");
        drop(peer);
        while context.pending() {
            context.iteration(false);
        }
        assert!(source.is_destroyed());
        assert_eq!(reads.get(), 2);
        for _ in 0..20 {
            context.iteration(false);
        }
        assert_eq!(reads.get(), 2, "HUP source stays retired");
    }
}
