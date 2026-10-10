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
    input_source: RefCell<Option<glib::Source>>,
    output_source: RefCell<Option<glib::Source>>,
}

impl UiWake {
    pub fn shutdown(&self) {
        self.pending.set(false);
        self.retire_transport();
        if let Some((_, timer)) = self.timer.borrow_mut().take() {
            timer.remove();
        }
        let callback = self.callback.borrow_mut().take();
        drop(callback); // Drop UI captures after releasing the callback borrow.
    }

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

    pub fn own_input_source(&self, source: glib::Source) {
        if let Some(old) = self.input_source.replace(Some(source)) {
            old.destroy();
        }
    }

    pub fn own_output_source(&self, source: glib::Source) {
        if let Some(old) = self.output_source.replace(Some(source)) {
            old.destroy();
        }
    }

    pub fn retire_output_source(&self) {
        if let Some(source) = self.output_source.borrow_mut().take() {
            source.destroy();
        }
    }

    /// Terminal failures may occur in a buffered continuation after kernel IN
    /// has cleared. Explicitly destroy both sources: waiting for another HUP
    /// would retain the duplicated endpoint and leave the peer connected.
    pub fn retire_transport(&self) {
        if let Some(source) = self.input_source.borrow_mut().take() {
            source.destroy();
        }
        self.retire_output_source();
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
        self.shutdown();
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
    use gio::prelude::*;
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

        let shutdown_wake = Rc::new(UiWake::default());
        let after_shutdown = Rc::new(Cell::new(0));
        let observed = after_shutdown.clone();
        shutdown_wake.install(move || observed.set(observed.get() + 1));
        shutdown_wake.at(Some(Instant::now() + Duration::from_millis(10)));
        shutdown_wake.shutdown();
        std::thread::sleep(Duration::from_millis(20));
        while context.pending() {
            context.iteration(false);
        }
        assert_eq!(
            after_shutdown.get(),
            0,
            "shutdown cancels queued UI and deadlines"
        );
        assert!(shutdown_wake.callback.borrow().is_none());
        assert!(shutdown_wake.timer.borrow().is_none());

        struct RequestOnDrop(std::rc::Weak<UiWake>);
        impl Drop for RequestOnDrop {
            fn drop(&mut self) {
                if let Some(wake) = self.0.upgrade() {
                    wake.request();
                }
            }
        }
        let reentrant = Rc::new(UiWake::default());
        let ui_capture = RequestOnDrop(Rc::downgrade(&reentrant));
        reentrant.install(move || {
            let _ui = &ui_capture;
        });
        reentrant.shutdown(); // UI destructors may emit local notifications.
        assert!(!reentrant.pending.get());

        // Exercise the production lifetime contract with a real application
        // signal: local build scope ends, application owns wake, shutdown
        // breaks callbacks/UI references and destroys both transport sources.
        let application = gio::Application::new(None, gio::ApplicationFlags::NON_UNIQUE);
        let (stream, _peer) = UnixStream::pair().unwrap();
        let socket = gio::Socket::from_fd(stream.as_fd().try_clone_to_owned().unwrap()).unwrap();
        let input = watch_socket(&socket, |_| true);
        let output = watch_socket(&socket, |_| true);
        let retained_ui = Rc::new(());
        let weak_ui = Rc::downgrade(&retained_ui);
        let lifetime_calls = Rc::new(Cell::new(0));
        let weak_wake = {
            let wake = Rc::new(UiWake::default());
            let owner = wake.clone();
            application.connect_shutdown(move |_| owner.shutdown());
            let calls = lifetime_calls.clone();
            wake.install(move || {
                let _ui = &retained_ui;
                calls.set(calls.get() + 1);
            });
            wake.own_input_source(input.clone());
            wake.own_output_source(output.clone());
            Rc::downgrade(&wake)
        };
        assert!(
            weak_wake.upgrade().is_some(),
            "application owns wake after build scope ends"
        );
        while context.pending() {
            context.iteration(false);
        }
        assert_eq!(lifetime_calls.get(), 1);
        application.emit_by_name::<()>("shutdown", &[]);
        assert!(input.is_destroyed() && output.is_destroyed());
        assert!(
            weak_ui.upgrade().is_none(),
            "shutdown releases UI callback captures"
        );
        let wake = weak_wake.upgrade().unwrap();
        assert!(wake.input_source.borrow().is_none());
        assert!(wake.output_source.borrow().is_none());
        assert!(wake.callback.borrow().is_none());
        drop(wake);
        drop(application);
        assert!(
            weak_wake.upgrade().is_none(),
            "application cleanup releases dispatcher"
        );

        // All bytes fit in the first kernel read. A128-frame drain stops
        // before the malformed prefetched frame; its decode error occurs in
        // the idle continuation with no further IN/HUP event from the peer.
        let (stream, mut peer) = UnixStream::pair().unwrap();
        peer.set_nonblocking(true).unwrap();
        let socket = gio::Socket::from_fd(stream.as_fd().try_clone_to_owned().unwrap()).unwrap();
        let client = tuna_shell_host::control::ControlClient::new(stream).unwrap();
        let client = Rc::new(RefCell::new(Some(client)));
        let wake = Rc::new(UiWake::default());
        let weak_wake = Rc::downgrade(&wake);
        let dispatcher_client = client.clone();
        let handled = Rc::new(Cell::new(0));
        let handled_callback = handled.clone();
        wake.install(move || {
            let wake = weak_wake.upgrade().unwrap();
            let mut client = dispatcher_client.borrow_mut();
            for index in 0..128 {
                let result = client.as_mut().unwrap().poll();
                match result {
                    Ok(_) => handled_callback.set(handled_callback.get() + 1),
                    Err(tuna_shell_host::control::ControlError::Io(error))
                        if error.kind() == std::io::ErrorKind::WouldBlock =>
                    {
                        break
                    }
                    Err(tuna_shell_host::control::ControlError::Decode(_)) => {
                        *client = None;
                        wake.retire_transport();
                        break;
                    }
                    result => panic!("unexpected decode test result: {result:?}"),
                }
                if index == 127 {
                    wake.request();
                }
            }
        });
        // Consume the initialization dispatch before publishing test bytes.
        while context.pending() {
            context.iteration(false);
        }
        let weak_wake = Rc::downgrade(&wake);
        let callback_client = client.clone();
        let source = watch_socket(&socket, move |_| {
            weak_wake.upgrade().unwrap().dispatch();
            callback_client.borrow().is_some()
        });
        wake.own_input_source(source.clone());
        let hello = tuna_shell_control::encode_frame(&tuna_shell_control::Message::Hello {
            version: tuna_shell_control::CURRENT_VERSION,
        });
        let mut bytes = hello.repeat(128);
        bytes.extend_from_slice(&[1, 0, 0, 0, 255]);
        peer.write_all(&bytes).unwrap();
        while context.pending() {
            context.iteration(false);
        }
        assert_eq!(handled.get(), 128);
        assert!(client.borrow().is_none());
        assert!(
            source.is_destroyed(),
            "buffered decode failure must retire the quiet input source"
        );
        assert!(wake.input_source.borrow().is_none());
        drop(source);
        drop(socket);
        let mut byte = [0];
        assert_eq!(
            peer.read(&mut byte).unwrap(),
            0,
            "duplicated endpoint must close without requiring peer HUP"
        );
    }
}
