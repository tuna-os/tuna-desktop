//! Wake plumbing only: control parsing and policy remain in ControlHub::poll.
//! The retained 16 ms tick still handles buffered frames and handshake phases.
use rustix::event::epoll;
use std::os::fd::{AsFd, OwnedFd};

pub(crate) struct ControlReadiness(OwnedFd);
impl ControlReadiness {
    pub fn new(listener: impl AsFd) -> std::io::Result<Self> {
        let this = Self(epoll::create(epoll::CreateFlags::CLOEXEC)?);
        this.add(listener)?;
        Ok(this)
    }
    pub fn add(&self, socket: impl AsFd) -> std::io::Result<()> {
        // No stream duplicates: closing the last original socket retires its
        // interest automatically, including failed handshakes and gate eviction.
        epoll::add(
            &self.0,
            socket,
            epoll::EventData::new_u64(0),
            epoll::EventFlags::IN | epoll::EventFlags::ET,
        )?;
        Ok(())
    }
    pub fn source(&self) -> std::io::Result<calloop::generic::Generic<OwnedFd>> {
        Ok(calloop::generic::Generic::new(
            self.0.as_fd().try_clone_to_owned()?,
            calloop::Interest::READ,
            calloop::Mode::Level,
        ))
    }
}

pub(crate) fn drain(fd: impl AsFd) -> std::io::Result<()> {
    // One bounded nonblocking receipt batch; level readiness schedules another
    // dispatch if more receipts remain. No policy work on this callback.
    let mut events = [epoll::Event {
        flags: epoll::EventFlags::empty(),
        data: epoll::EventData::new_u64(0),
    }; 64];
    epoll::wait(
        fd,
        &mut events,
        Some(&rustix::event::Timespec {
            tv_sec: 0,
            tv_nsec: 0,
        }),
    )?;
    Ok(())
}

#[cfg(test)]
#[path = "control_readiness_test_peer.rs"]
pub(crate) mod test_peer;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    use std::time::Duration;
    #[test]
    fn accepted_control_socket_wakes_drains_and_closure_retires_interest() {
        let (mut receiver, mut sender) = UnixStream::pair().unwrap();
        receiver.set_nonblocking(true).unwrap();
        let readiness = ControlReadiness::new(&receiver).unwrap();
        let mut event_loop = calloop::EventLoop::<usize>::try_new().unwrap();
        event_loop
            .handle()
            .insert_source(readiness.source().unwrap(), |_, fd, count| {
                drain(fd).unwrap();
                *count += 1;
                Ok(calloop::PostAction::Continue)
            })
            .unwrap();
        let mut count = 0;
        sender.write_all(b"hello").unwrap();
        event_loop
            .dispatch(Some(Duration::from_secs(1)), &mut count)
            .unwrap();
        assert_eq!(count, 1);
        let mut buffer = [0; 16];
        assert_eq!(receiver.read(&mut buffer).unwrap(), 5);
        event_loop
            .dispatch(Some(Duration::ZERO), &mut count)
            .unwrap();
        assert_eq!(count, 1, "drained socket must not spin");
        sender.write_all(b"again").unwrap();
        event_loop
            .dispatch(Some(Duration::from_secs(1)), &mut count)
            .unwrap();
        assert_eq!(count, 2);
        drop(receiver);
        event_loop
            .dispatch(Some(Duration::ZERO), &mut count)
            .unwrap();
        assert_eq!(count, 2, "closed original fd must remove interest");
        test_peer::wait_for_peer_close(&mut sender, Duration::from_secs(1))
            .expect("watch must not hold the peer alive");
    }
}
