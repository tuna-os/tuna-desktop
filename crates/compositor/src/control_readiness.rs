//! Wake plumbing only: control parsing and policy remain in ControlHub::poll.
//! Kernel readiness remains active until its bytes are consumed. Buffered
//! complete frames and handshake phases use bounded runtime continuations.
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
            // ControlConn returns one complete frame per bounded hub poll. A
            // partial next frame can remain in userspace while the kernel still
            // has bytes; retain readiness without requiring a new write/edge.
            epoll::EventFlags::IN,
        )?;
        Ok(())
    }
    /// Recreate the previous inner-edge flags solely for the kernel regression.
    #[cfg(test)]
    pub(crate) fn old_edge_fixture(socket: impl AsFd) -> std::io::Result<Self> {
        let this = Self::new(&socket)?;
        epoll::modify(
            &this.0,
            socket,
            epoll::EventData::new_u64(0),
            epoll::EventFlags::IN | epoll::EventFlags::ET,
        )?;
        Ok(this)
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
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;
    use std::time::Duration;
    #[test]
    fn accepted_control_socket_wakes_drains_and_closure_retires_interest() {
        fn members(readiness: &ControlReadiness) -> usize {
            std::fs::read_to_string(format!("/proc/self/fdinfo/{}", readiness.0.as_raw_fd()))
                .unwrap()
                .lines()
                .filter(|line| line.starts_with("tfd:"))
                .count()
        }
        fn inner_receipts(readiness: &ControlReadiness) -> usize {
            let mut events = [epoll::Event {
                flags: epoll::EventFlags::empty(),
                data: epoll::EventData::new_u64(0),
            }; 1];
            epoll::wait(
                &readiness.0,
                &mut events,
                Some(&rustix::event::Timespec {
                    tv_sec: 0,
                    tv_nsec: 0,
                }),
            )
            .unwrap()
        }

        let (receiver, mut sender) = UnixStream::pair().unwrap();
        receiver.set_nonblocking(true).unwrap();
        sender.set_nonblocking(true).unwrap();
        let inherited_description = receiver.try_clone().unwrap();
        let readiness = ControlReadiness::new(&receiver).unwrap();
        assert_eq!(members(&readiness), 1);
        let mut event_loop =
            calloop::EventLoop::<(Option<UnixStream>, [u8; 16], usize)>::try_new().unwrap();
        event_loop
            .handle()
            .insert_source(
                readiness.source().unwrap(),
                |_, fd, (receiver, bytes, used)| {
                    drain(fd).unwrap();
                    if let Some(receiver) = receiver.as_mut() {
                        loop {
                            assert!(*used < bytes.len(), "unexpected extra control input");
                            match receiver.read(&mut bytes[*used..]) {
                                Ok(0) => break,
                                Ok(count) => *used += count,
                                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                                    break
                                }
                                Err(error) => panic!("control consumer failed: {error}"),
                            }
                        }
                    }
                    Ok(calloop::PostAction::Continue)
                },
            )
            .unwrap();
        let mut consumer = (Some(receiver), [0; 16], 0);
        sender.write_all(b"hello").unwrap();
        event_loop
            .dispatch(Some(Duration::from_secs(1)), &mut consumer)
            .unwrap();
        assert_eq!(&consumer.1[..consumer.2], b"hello");
        assert_eq!(
            inner_receipts(&readiness),
            0,
            "consumed bytes must not leave inner readiness"
        );
        sender.write_all(b"again").unwrap();
        event_loop
            .dispatch(Some(Duration::from_secs(1)), &mut consumer)
            .unwrap();
        assert_eq!(&consumer.1[..consumer.2], b"helloagain");
        assert_eq!(inner_receipts(&readiness), 0);

        drop(consumer.0.take());
        assert_eq!(
            members(&readiness),
            1,
            "another reference keeps the file description registered"
        );
        assert_eq!(
            sender.read(&mut [0_u8; 1]).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock,
            "closing one descriptor must not fake last-peer closure"
        );
        drop(inherited_description);
        test_peer::wait_for_peer_close(&mut sender, Duration::from_secs(1))
            .expect("watch must not hold the last peer description alive");
        assert_eq!(
            members(&readiness),
            0,
            "last-description closure removes kernel interest"
        );
        assert_eq!(inner_receipts(&readiness), 0);
        event_loop
            .dispatch(Some(Duration::ZERO), &mut consumer)
            .unwrap();
        assert_eq!(&consumer.1[..consumer.2], b"helloagain");
    }
}
