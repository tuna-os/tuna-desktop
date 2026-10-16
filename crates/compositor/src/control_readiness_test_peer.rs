//! Test-only bounded observation of the last peer socket reference closing.
use std::io::{self, Read};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

pub(crate) fn wait_for_peer_close(stream: &mut UnixStream, timeout: Duration) -> io::Result<()> {
    stream.set_nonblocking(true)?;
    let deadline = Instant::now() + timeout;
    let mut buffer = [0_u8; 4096];
    loop {
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "peer socket remained open after retirement",
            ));
        }
        match stream.read(&mut buffer) {
            Ok(0) => return Ok(()),
            // Drain replies emitted before eviction; they do not imply a leak.
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::ConnectionReset => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(
                    Duration::from_millis(1)
                        .min(deadline.saturating_duration_since(Instant::now())),
                );
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn transient_peer_reference_and_queued_replies_do_not_fail_retirement() {
        let (mut client, mut peer) = UnixStream::pair().unwrap();
        peer.write_all(b"reply queued before eviction").unwrap();
        let inherited_reference = peer.try_clone().unwrap();
        drop(peer);
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(10));
            drop(inherited_reference);
        });
        wait_for_peer_close(&mut client, Duration::from_secs(1)).unwrap();
        release.join().unwrap();
    }

    #[test]
    fn persistent_duplicate_peer_reference_fails_with_a_bounded_timeout() {
        let (mut client, peer) = UnixStream::pair().unwrap();
        let duplicate = peer.try_clone().unwrap();
        drop(peer);
        assert_eq!(
            wait_for_peer_close(&mut client, Duration::from_millis(20))
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
        drop(duplicate);
        wait_for_peer_close(&mut client, Duration::from_secs(1)).unwrap();
    }
}
