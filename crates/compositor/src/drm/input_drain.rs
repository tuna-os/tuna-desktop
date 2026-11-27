//! An input source that also hands out its queued events on request.
//!
//! libinput's `udev_assign_seat` and `resume` queue a device-added event
//! for every device already present without making the context's fd
//! readable. An event loop that only dispatches libinput on readiness
//! therefore added the devices present at login, and applied their
//! settings, only at the first later input or hotplug (drm-smoke sessions
//! reported no input devices at all). Mutter processes its seat's events
//! right after assigning it (`meta_seat_impl` `process_events`); here a
//! [`Draining`] source carries a ping that drains the queue on the next
//! loop dispatch: once when it is created, and whenever the backend pings
//! it after a session resume. The drain dispatches the shared libinput
//! context itself and hands each event, converted exactly as Smithay's
//! readiness path converts it, to the same loop callback.
use calloop::ping::{make_ping, Ping, PingSource};
use calloop::{EventSource, Poll, PostAction, Readiness, Token, TokenFactory};
use smithay::backend::input::InputEvent;
use smithay::backend::libinput::{LibinputInputBackend, PointerScrollAxis};
use smithay::reexports::input::{self as libinput, event, Libinput};

/// An event source whose queued events can be taken without fd readiness.
pub trait Drain: EventSource {
    /// Hand every queued event to `callback`.
    fn drain<F>(&mut self, callback: F) -> Result<(), Self::Error>
    where
        F: FnMut(Self::Event, &mut Self::Metadata) -> Self::Ret;
}

/// Smithay's libinput source plus a handle on the same context, which the
/// drain dispatches without waiting for readiness.
pub struct LibinputSource {
    backend: LibinputInputBackend,
    context: Libinput,
}

impl LibinputSource {
    pub fn new(context: Libinput) -> Self {
        Self {
            backend: LibinputInputBackend::new(context.clone()),
            context,
        }
    }
}

impl EventSource for LibinputSource {
    type Event = InputEvent<LibinputInputBackend>;
    type Metadata = ();
    type Ret = ();
    type Error = std::io::Error;

    fn process_events<F>(
        &mut self,
        readiness: Readiness,
        token: Token,
        callback: F,
    ) -> std::io::Result<PostAction>
    where
        F: FnMut(Self::Event, &mut ()),
    {
        self.backend.process_events(readiness, token, callback)
    }

    fn register(&mut self, poll: &mut Poll, factory: &mut TokenFactory) -> calloop::Result<()> {
        self.backend.register(poll, factory)
    }

    fn reregister(&mut self, poll: &mut Poll, factory: &mut TokenFactory) -> calloop::Result<()> {
        self.backend.reregister(poll, factory)
    }

    fn unregister(&mut self, poll: &mut Poll) -> calloop::Result<()> {
        self.backend.unregister(poll)
    }
}

impl Drain for LibinputSource {
    fn drain<F>(&mut self, mut callback: F) -> std::io::Result<()>
    where
        F: FnMut(InputEvent<LibinputInputBackend>, &mut ()),
    {
        self.context.dispatch()?;
        for event in &mut self.context {
            if let Some(event) = smithay_event(event) {
                callback(event, &mut ());
            }
        }
        Ok(())
    }
}

/// A libinput event as Smithay 0.7's `LibinputInputBackend::process_events`
/// hands it over (`third-party/smithay/src/backend/libinput/mod.rs`), or
/// `None` where Smithay drops it. Kept in step with that match so drained
/// events reach the loop callback exactly as readiness-dispatched ones do.
fn smithay_event(event: libinput::Event) -> Option<InputEvent<LibinputInputBackend>> {
    use event::gesture::{GestureHoldEvent, GesturePinchEvent, GestureSwipeEvent};
    use event::{
        DeviceEvent, EventTrait, GestureEvent, KeyboardEvent, PointerEvent, SwitchEvent,
        TabletToolEvent, TouchEvent,
    };
    Some(match event {
        libinput::Event::Device(DeviceEvent::Added(added)) => InputEvent::DeviceAdded {
            device: added.device(),
        },
        libinput::Event::Device(DeviceEvent::Removed(removed)) => InputEvent::DeviceRemoved {
            device: removed.device(),
        },
        libinput::Event::Touch(TouchEvent::Down(event)) => InputEvent::TouchDown { event },
        libinput::Event::Touch(TouchEvent::Motion(event)) => InputEvent::TouchMotion { event },
        libinput::Event::Touch(TouchEvent::Up(event)) => InputEvent::TouchUp { event },
        libinput::Event::Touch(TouchEvent::Cancel(event)) => InputEvent::TouchCancel { event },
        libinput::Event::Touch(TouchEvent::Frame(event)) => InputEvent::TouchFrame { event },
        libinput::Event::Keyboard(KeyboardEvent::Key(event)) => InputEvent::Keyboard { event },
        libinput::Event::Pointer(PointerEvent::Motion(event)) => {
            InputEvent::PointerMotion { event }
        }
        libinput::Event::Pointer(PointerEvent::MotionAbsolute(event)) => {
            InputEvent::PointerMotionAbsolute { event }
        }
        libinput::Event::Pointer(PointerEvent::ScrollWheel(event)) => InputEvent::PointerAxis {
            event: PointerScrollAxis::Wheel(event),
        },
        libinput::Event::Pointer(PointerEvent::ScrollFinger(event)) => InputEvent::PointerAxis {
            event: PointerScrollAxis::Finger(event),
        },
        libinput::Event::Pointer(PointerEvent::ScrollContinuous(event)) => {
            InputEvent::PointerAxis {
                event: PointerScrollAxis::Continuous(event),
            }
        }
        libinput::Event::Pointer(PointerEvent::Button(event)) => {
            InputEvent::PointerButton { event }
        }
        libinput::Event::Gesture(GestureEvent::Swipe(GestureSwipeEvent::Begin(event))) => {
            InputEvent::GestureSwipeBegin { event }
        }
        libinput::Event::Gesture(GestureEvent::Swipe(GestureSwipeEvent::Update(event))) => {
            InputEvent::GestureSwipeUpdate { event }
        }
        libinput::Event::Gesture(GestureEvent::Swipe(GestureSwipeEvent::End(event))) => {
            InputEvent::GestureSwipeEnd { event }
        }
        libinput::Event::Gesture(GestureEvent::Pinch(GesturePinchEvent::Begin(event))) => {
            InputEvent::GesturePinchBegin { event }
        }
        libinput::Event::Gesture(GestureEvent::Pinch(GesturePinchEvent::Update(event))) => {
            InputEvent::GesturePinchUpdate { event }
        }
        libinput::Event::Gesture(GestureEvent::Pinch(GesturePinchEvent::End(event))) => {
            InputEvent::GesturePinchEnd { event }
        }
        libinput::Event::Gesture(GestureEvent::Hold(GestureHoldEvent::Begin(event))) => {
            InputEvent::GestureHoldBegin { event }
        }
        libinput::Event::Gesture(GestureEvent::Hold(GestureHoldEvent::End(event))) => {
            InputEvent::GestureHoldEnd { event }
        }
        libinput::Event::Tablet(TabletToolEvent::Axis(event)) => {
            InputEvent::TabletToolAxis { event }
        }
        libinput::Event::Tablet(TabletToolEvent::Proximity(event)) => {
            InputEvent::TabletToolProximity { event }
        }
        libinput::Event::Tablet(TabletToolEvent::Tip(event)) => InputEvent::TabletToolTip { event },
        libinput::Event::Tablet(TabletToolEvent::Button(event)) => {
            InputEvent::TabletToolButton { event }
        }
        libinput::Event::Switch(SwitchEvent::Toggle(event)) => InputEvent::SwitchToggle { event },
        libinput::Event::TabletPad(event) => InputEvent::Special(event),
        _ => return None,
    })
}

/// `inner` plus a ping that drains it on the next dispatch.
pub struct Draining<S> {
    inner: S,
    ping: PingSource,
}

impl<S> Draining<S> {
    /// Wrap `inner`. The returned [`Ping`] drains it again; it is already
    /// pinged once, so the loop's first dispatch takes what `inner`
    /// queued before it was inserted.
    pub fn new(inner: S) -> std::io::Result<(Self, Ping)> {
        let (ping, source) = make_ping()?;
        ping.ping();
        Ok((
            Self {
                inner,
                ping: source,
            },
            ping,
        ))
    }
}

impl<S: Drain> EventSource for Draining<S>
where
    S::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    type Event = S::Event;
    type Metadata = S::Metadata;
    type Ret = S::Ret;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn process_events<F>(
        &mut self,
        readiness: Readiness,
        token: Token,
        mut callback: F,
    ) -> Result<PostAction, Self::Error>
    where
        F: FnMut(Self::Event, &mut Self::Metadata) -> Self::Ret,
    {
        let mut drain = false;
        self.ping
            .process_events(readiness, token, |(), &mut ()| drain = true)?;
        if drain {
            self.inner.drain(&mut callback).map_err(Into::into)?;
        }
        // A token that is not the inner source's (the ping's) is ignored.
        self.inner
            .process_events(readiness, token, &mut callback)
            .map_err(Into::into)
    }

    fn register(&mut self, poll: &mut Poll, factory: &mut TokenFactory) -> calloop::Result<()> {
        self.inner.register(poll, factory)?;
        self.ping.register(poll, factory)
    }

    fn reregister(&mut self, poll: &mut Poll, factory: &mut TokenFactory) -> calloop::Result<()> {
        self.inner.reregister(poll, factory)?;
        self.ping.reregister(poll, factory)
    }

    fn unregister(&mut self, poll: &mut Poll) -> calloop::Result<()> {
        self.inner.unregister(poll)?;
        self.ping.unregister(poll)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use calloop::{EventLoop, Interest, Mode};
    use std::cell::RefCell;
    use std::os::fd::AsFd;
    use std::os::unix::net::UnixStream;
    use std::rc::Rc;
    use std::time::Duration;

    /// libinput's shape: events queued in user space behind an fd that
    /// stays unreadable until the device side writes.
    struct Queued {
        queue: Rc<RefCell<Vec<u32>>>,
        fd: UnixStream,
        token: Option<Token>,
    }

    impl Queued {
        fn new(queue: Rc<RefCell<Vec<u32>>>) -> (Self, UnixStream) {
            let (fd, peer) = UnixStream::pair().unwrap();
            fd.set_nonblocking(true).unwrap();
            (
                Self {
                    queue,
                    fd,
                    token: None,
                },
                peer,
            )
        }
    }

    impl EventSource for Queued {
        type Event = u32;
        type Metadata = ();
        type Ret = ();
        type Error = std::io::Error;

        fn process_events<F>(
            &mut self,
            _: Readiness,
            token: Token,
            mut callback: F,
        ) -> std::io::Result<PostAction>
        where
            F: FnMut(u32, &mut ()),
        {
            if Some(token) == self.token {
                let mut byte = [0u8; 64];
                let _ = std::io::Read::read(&mut self.fd, &mut byte);
                self.drain(&mut callback)?;
            }
            Ok(PostAction::Continue)
        }

        fn register(&mut self, poll: &mut Poll, factory: &mut TokenFactory) -> calloop::Result<()> {
            self.token = Some(factory.token());
            unsafe {
                poll.register(
                    self.fd.as_fd(),
                    Interest::READ,
                    Mode::Level,
                    self.token.unwrap(),
                )
            }
        }

        fn reregister(
            &mut self,
            poll: &mut Poll,
            factory: &mut TokenFactory,
        ) -> calloop::Result<()> {
            self.token = Some(factory.token());
            poll.reregister(
                self.fd.as_fd(),
                Interest::READ,
                Mode::Level,
                self.token.unwrap(),
            )
        }

        fn unregister(&mut self, poll: &mut Poll) -> calloop::Result<()> {
            self.token = None;
            poll.unregister(self.fd.as_fd())
        }
    }

    impl Drain for Queued {
        fn drain<F>(&mut self, mut callback: F) -> std::io::Result<()>
        where
            F: FnMut(u32, &mut ()),
        {
            let queued: Vec<u32> = self.queue.borrow_mut().drain(..).collect();
            for event in queued {
                callback(event, &mut ());
            }
            Ok(())
        }
    }

    fn dispatch(event_loop: &mut EventLoop<'_, Vec<u32>>, seen: &mut Vec<u32>) {
        event_loop
            .dispatch(Some(Duration::from_millis(50)), seen)
            .unwrap();
    }

    #[test]
    fn a_readiness_only_source_never_delivers_what_it_queued_before_the_loop() {
        // The old behaviour: devices present at startup wait for input.
        let mut event_loop: EventLoop<'_, Vec<u32>> = EventLoop::try_new().unwrap();
        let (source, _peer) = Queued::new(Rc::new(RefCell::new(vec![1, 2])));
        event_loop
            .handle()
            .insert_source(source, |event, _, seen: &mut Vec<u32>| seen.push(event))
            .unwrap();
        let mut seen = Vec::new();
        dispatch(&mut event_loop, &mut seen);
        dispatch(&mut event_loop, &mut seen);
        assert!(seen.is_empty());
    }

    #[test]
    fn queued_events_arrive_on_the_first_dispatch_without_readiness() {
        let mut event_loop: EventLoop<'_, Vec<u32>> = EventLoop::try_new().unwrap();
        let (source, _peer) = Queued::new(Rc::new(RefCell::new(vec![1, 2])));
        let (source, _ping) = Draining::new(source).unwrap();
        event_loop
            .handle()
            .insert_source(source, |event, _, seen: &mut Vec<u32>| seen.push(event))
            .unwrap();
        let mut seen = Vec::new();
        dispatch(&mut event_loop, &mut seen);
        assert_eq!(seen, [1, 2]);
        // Drained once: nothing is delivered twice.
        dispatch(&mut event_loop, &mut seen);
        assert_eq!(seen, [1, 2]);
    }

    #[test]
    fn a_ping_drains_what_a_resume_queued_and_readiness_still_works() {
        let mut event_loop: EventLoop<'_, Vec<u32>> = EventLoop::try_new().unwrap();
        let queue = Rc::new(RefCell::new(Vec::new()));
        let (source, mut peer) = Queued::new(queue.clone());
        let (source, ping) = Draining::new(source).unwrap();
        event_loop
            .handle()
            .insert_source(source, |event, _, seen: &mut Vec<u32>| seen.push(event))
            .unwrap();
        let mut seen = Vec::new();
        dispatch(&mut event_loop, &mut seen);
        assert!(seen.is_empty());
        // A resume re-queues the devices without readiness: the ping
        // takes them on the next dispatch.
        queue.borrow_mut().extend([7, 8]);
        dispatch(&mut event_loop, &mut seen);
        assert!(seen.is_empty(), "nothing drains before the ping");
        ping.ping();
        dispatch(&mut event_loop, &mut seen);
        assert_eq!(seen, [7, 8]);
        // Device input still dispatches on readiness.
        queue.borrow_mut().push(9);
        std::io::Write::write_all(&mut peer, b"x").unwrap();
        dispatch(&mut event_loop, &mut seen);
        assert_eq!(seen, [7, 8, 9]);
    }
}
