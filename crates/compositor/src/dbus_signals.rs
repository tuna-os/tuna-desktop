//! Ordered, non-blocking D-Bus signal emission (#657).
//!
//! Sending on a zbus connection holds its write lock until the whole
//! message is on the socket, so a bus that stops reading parks the
//! sender for as long as it stays wedged. The event loop must never
//! wait on the bus: GNOME Shell and Mutter emit through GDBus, whose
//! `g_dbus_connection_emit_signal` only queues the message for the
//! GDBus worker thread (gio/gdbusconnection.c, gio/gdbusprivate.c).
//! [`SignalQueue`] is that worker: callers hand it an emission and
//! return at once; one thread sends them in submission order, so a
//! property's last value and a stream's announce-then-close order are
//! kept. A send is never cancelled half-written (that would corrupt
//! the stream for every later message); when the bus is wedged the
//! queue fills and further emissions are dropped instead of blocking.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{Arc, OnceLock};

type Job = Pin<Box<dyn Future<Output = ()> + Send>>;

/// Emissions waiting behind a wedged bus before new ones are dropped.
const CAPACITY: usize = 1024;

/// A FIFO of pending emissions, sent by one worker thread. Clones share
/// the queue; the worker starts on the first emission and exits once
/// every clone is gone.
#[derive(Clone)]
pub struct SignalQueue {
    inner: Arc<Inner>,
}

struct Inner {
    name: &'static str,
    capacity: usize,
    jobs: OnceLock<Option<SyncSender<Job>>>,
    overflowing: AtomicBool,
}

impl SignalQueue {
    /// A queue whose worker thread is called `name`.
    pub fn new(name: &'static str) -> Self {
        Self::with_capacity(name, CAPACITY)
    }

    fn with_capacity(name: &'static str, capacity: usize) -> Self {
        Self {
            inner: Arc::new(Inner {
                name,
                capacity,
                jobs: OnceLock::new(),
                overflowing: AtomicBool::new(false),
            }),
        }
    }

    /// Queue one emission and return without waiting for the bus.
    /// False when it was dropped (the queue is full or has no worker).
    pub fn emit<F>(&self, emission: F) -> bool
    where
        F: Future<Output = zbus::Result<()>> + Send + 'static,
    {
        let Some(jobs) = self.inner.jobs.get_or_init(|| self.inner.spawn()) else {
            return false;
        };
        let job: Job = Box::pin(async move {
            let _ = emission.await;
        });
        match jobs.try_send(job) {
            Ok(()) => {
                if self.inner.overflowing.swap(false, Ordering::Relaxed) {
                    eprintln!(
                        "tuna-compositor: {}: D-Bus accepting signals again",
                        self.inner.name
                    );
                }
                true
            }
            Err(TrySendError::Full(_)) => {
                if !self.inner.overflowing.swap(true, Ordering::Relaxed) {
                    eprintln!(
                        "tuna-compositor: {}: D-Bus is not reading; dropping signals",
                        self.inner.name
                    );
                }
                false
            }
            Err(TrySendError::Disconnected(_)) => false,
        }
    }
}

impl Inner {
    fn spawn(&self) -> Option<SyncSender<Job>> {
        let (jobs, pending) = mpsc::sync_channel::<Job>(self.capacity);
        std::thread::Builder::new()
            .name(self.name.into())
            .spawn(move || {
                for job in pending {
                    zbus::block_on(job);
                }
            })
            .map_err(|e| eprintln!("tuna-compositor: {}: no signal thread: {e}", self.name))
            .ok()?;
        Some(jobs)
    }
}

/// The server end of a peer-to-peer link whose client is a raw socket
/// that completes the SASL handshake and then reads nothing more: a bus
/// that has stopped reading and answering. Keep the socket alive for
/// the test's duration.
#[cfg(test)]
pub(crate) fn wedged_link() -> (zbus::blocking::Connection, std::os::unix::net::UnixStream) {
    use std::io::{BufRead, BufReader, Write};
    let (server_end, client_end) = std::os::unix::net::UnixStream::pair().unwrap();
    let server = std::thread::spawn(move || {
        zbus::blocking::connection::Builder::async_io_unix_stream(server_end)
            .server(zbus::Guid::generate())
            .unwrap()
            .p2p()
            .build()
            .unwrap()
    });
    let uid = rustix::process::geteuid().as_raw().to_string();
    let hex: String = uid.bytes().map(|b| format!("{b:02x}")).collect();
    let mut writer = client_end.try_clone().unwrap();
    writer.write_all(b"\0").unwrap();
    writer
        .write_all(format!("AUTH EXTERNAL {hex}\r\n").as_bytes())
        .unwrap();
    let mut reader = BufReader::new(client_end.try_clone().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    assert!(
        line.starts_with("OK "),
        "server accepted EXTERNAL: {line:?}"
    );
    writer.write_all(b"BEGIN\r\n").unwrap();
    (server.join().unwrap(), client_end)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;
    use std::time::{Duration, Instant};

    const PATH: &str = "/org/tuna/Test";
    const IFACE: &str = "org.tuna.Test";

    fn signal(
        conn: &zbus::blocking::Connection,
        payload: String,
    ) -> impl Future<Output = zbus::Result<()>> + Send + 'static {
        let conn = conn.inner().clone();
        async move {
            conn.emit_signal(None::<()>, PATH, IFACE, "Changed", &(payload,))
                .await
        }
    }

    /// A bus that stops reading wedges the worker mid-send, but emitting
    /// never waits: the queue fills, then drops, and every call returns
    /// at once. `zbus::block_on` on the caller (the pre-#657 code) would
    /// have parked it on the first send that did not fit.
    #[test]
    fn a_wedged_bus_never_blocks_the_emitting_thread() {
        let (conn, _peer) = wedged_link();
        let queue = SignalQueue::with_capacity("tuna-test-signals", 8);
        // Far more than any socket buffer holds.
        let payload = "x".repeat(256 * 1024);
        let started = Instant::now();
        let mut accepted = 0;
        let mut dropped = 0;
        for _ in 0..64 {
            if queue.emit(signal(&conn, payload.clone())) {
                accepted += 1;
            } else {
                dropped += 1;
            }
        }
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(2),
            "emitting waited on the bus for {elapsed:?}"
        );
        assert!(dropped > 0, "the wedged worker never filled its queue");
        assert!(
            accepted <= 8 + 1,
            "at most the capacity plus the send in flight was accepted"
        );
    }

    /// Emissions reach a reading peer complete and in submission order.
    #[test]
    fn emissions_arrive_in_submission_order() {
        let (server_end, client_end) = UnixStream::pair().unwrap();
        let server = std::thread::spawn(move || {
            zbus::blocking::connection::Builder::async_io_unix_stream(server_end)
                .server(zbus::Guid::generate())
                .unwrap()
                .p2p()
                .build()
                .unwrap()
        });
        let client = zbus::blocking::connection::Builder::async_io_unix_stream(client_end)
            .p2p()
            .build()
            .unwrap();
        let server = server.join().unwrap();
        let rule = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .interface(IFACE)
            .unwrap()
            .build();
        let mut received =
            zbus::blocking::MessageIterator::for_match_rule(rule, &client, Some(512)).unwrap();
        let queue = SignalQueue::new("tuna-test-signals");
        for n in 0..200 {
            assert!(queue.emit(signal(&server, n.to_string())));
        }
        for n in 0..200 {
            let message = received.next().unwrap().unwrap();
            let (body,): (String,) = message.body().deserialize().unwrap();
            assert_eq!(body, n.to_string());
        }
    }
}
