//! Atomic state snapshots on one owned worker, never on the compositor thread.
//!
//! Only the newest waiting document is retained. The worker owns successful-write
//! deduplication, so a failed publication can be retried by the next submission.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

#[derive(Default)]
struct Pending {
    body: Option<String>,
    stopping: bool,
}

pub(crate) struct Publisher {
    shared: Arc<(Mutex<Pending>, Condvar)>,
    worker: Option<JoinHandle<()>>,
}

impl Publisher {
    pub(crate) fn start(path: PathBuf) -> io::Result<Self> {
        let tmp = path.with_extension("tmp");
        Self::start_with_writer(move |body| write_snapshot(&path, &tmp, body))
    }

    fn start_with_writer(
        mut write: impl FnMut(&str) -> io::Result<()> + Send + 'static,
    ) -> io::Result<Self> {
        let shared = Arc::new((Mutex::new(Pending::default()), Condvar::new()));
        let queue = Arc::clone(&shared);
        let worker = std::thread::Builder::new()
            .name("snapshot-writer".into())
            .spawn(move || {
                let mut last_successful: Option<String> = None;
                let mut publication_failed = false;
                loop {
                    let body = {
                        let (pending, ready) = &*queue;
                        let mut pending = pending.lock().unwrap_or_else(|e| e.into_inner());
                        while pending.body.is_none() && !pending.stopping {
                            pending = ready.wait(pending).unwrap_or_else(|e| e.into_inner());
                        }
                        match pending.body.take() {
                            Some(body) => body,
                            None => return,
                        }
                    };
                    // No queue lock is held while comparing, doing filesystem I/O,
                    // or releasing either the previous or an obsolete document.
                    if last_successful.as_deref() == Some(body.as_str()) {
                        continue;
                    }
                    match write(&body) {
                        Ok(()) => {
                            last_successful = Some(body);
                            if publication_failed {
                                eprintln!("tuna-compositor: state snapshot publication recovered");
                                publication_failed = false;
                            }
                        }
                        Err(error) => {
                            eprintln!("tuna-compositor: could not publish state snapshot: {error}");
                            publication_failed = true;
                        }
                    }
                }
            })?;
        Ok(Self {
            shared,
            worker: Some(worker),
        })
    }

    /// Move a coherent document into the single pending slot without waiting for
    /// worker I/O. Replaced documents are released outside the queue lock.
    pub(crate) fn publish(&self, body: String) {
        let (pending, ready) = &*self.shared;
        let obsolete = {
            let mut pending = pending.lock().unwrap_or_else(|e| e.into_inner());
            pending.body.replace(body)
        };
        ready.notify_one();
        drop(obsolete);
    }
}

impl Drop for Publisher {
    fn drop(&mut self) {
        let (pending, ready) = &*self.shared;
        {
            let mut pending = pending.lock().unwrap_or_else(|e| e.into_inner());
            pending.stopping = true;
        }
        ready.notify_all();
        // Shutdown alone waits for I/O. The worker first drains the newest
        // pending document, even when no further compositor tick will occur.
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn write_snapshot(path: &Path, tmp: &Path, body: &str) -> io::Result<()> {
    std::fs::write(tmp, body)?;
    std::fs::rename(tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::sync::mpsc;
    use std::time::Duration;

    const HARNESS_LIMIT: Duration = Duration::from_secs(5);

    #[test]
    fn real_filesystem_publication_atomically_replaces_complete_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let old = r#"{"rendered_frames":7,"windows":["old"]}"#;
        let latest = r#"{"rendered_frames":8,"windows":["new","coherent"]}"#;
        std::fs::write(&path, old).unwrap();
        let mut previous_file = std::fs::File::open(&path).unwrap();

        let publisher = Publisher::start(path.clone()).unwrap();
        publisher.publish(latest.to_owned());
        drop(publisher);

        let current = std::fs::read_to_string(&path).unwrap();
        assert_eq!(current, latest);
        let parsed: serde_json::Value = serde_json::from_str(&current).unwrap();
        assert_eq!(parsed["rendered_frames"], 8);
        assert_eq!(parsed["windows"], serde_json::json!(["new", "coherent"]));
        // An already-open reader still sees the old complete inode: the final
        // file was replaced, not truncated and rewritten in place.
        let mut previous = String::new();
        previous_file.read_to_string(&mut previous).unwrap();
        assert_eq!(previous, old);
        assert!(!path.with_extension("tmp").exists());
    }

    #[test]
    fn concurrent_reader_observes_only_coherent_monotonic_json_and_final_state() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::Instant;

        const FINAL_FRAME: u64 = 32;

        fn snapshot(frame: u64) -> String {
            serde_json::json!({
                "revision": frame,
                "rendered_frames": frame,
                "windows": [{"frame": frame}, {"frame": frame}],
                "payload": frame.to_string().repeat(2048),
            })
            .to_string()
        }

        fn read_snapshot(path: &Path, previous_frame: u64) -> (u64, String) {
            let body = std::fs::read_to_string(path).unwrap();
            let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
            let frame = parsed["rendered_frames"].as_u64().unwrap();
            assert!(frame >= previous_frame, "published frames went backwards");
            assert!(frame <= FINAL_FRAME);
            // Checking the whole document catches mixed revisions as well as
            // truncated JSON, including the large frame-specific payload.
            assert_eq!(body, snapshot(frame));
            (frame, body)
        }

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(&path, snapshot(0)).unwrap();
        let publisher = Publisher::start(path.clone()).unwrap();
        let stopped = Arc::new(AtomicBool::new(false));
        let reader_stopped = Arc::clone(&stopped);
        let (observed_tx, observed_rx) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let (mut previous_frame, _) = read_snapshot(&path, 0);
            assert_eq!(previous_frame, 0);
            observed_tx.send(0).unwrap();
            let mut progress = Instant::now();
            loop {
                if reader_stopped.load(Ordering::Acquire) {
                    // This read is explicitly after the publisher has drained
                    // and joined, not merely an observation before shutdown.
                    let (frame, body) = read_snapshot(&path, previous_frame);
                    assert_eq!(frame, FINAL_FRAME);
                    return body;
                }
                let (frame, _) = read_snapshot(&path, previous_frame);
                if frame != previous_frame {
                    observed_tx.send(frame).unwrap();
                    previous_frame = frame;
                    progress = Instant::now();
                }
                assert!(
                    progress.elapsed() < HARNESS_LIMIT,
                    "reader made no progress"
                );
                std::thread::yield_now();
            }
        });

        // Reader readiness and intermediate observations ensure actual reads
        // overlap live worker publication, rather than only checking endpoints.
        assert_eq!(observed_rx.recv_timeout(HARNESS_LIMIT).unwrap(), 0);
        for frame in 1..FINAL_FRAME {
            publisher.publish(snapshot(frame));
            assert_eq!(observed_rx.recv_timeout(HARNESS_LIMIT).unwrap(), frame);
        }
        let final_body = snapshot(FINAL_FRAME);
        publisher.publish(final_body.clone());
        drop(publisher);
        stopped.store(true, Ordering::Release);
        assert_eq!(reader.join().unwrap(), final_body);
    }

    #[test]
    fn graceful_shutdown_drains_latest_json_behind_an_active_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let destination = path.clone();
        let tmp = path.with_extension("tmp");
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut first = true;
        let publisher = Publisher::start_with_writer(move |body| {
            if first {
                first = false;
                entered_tx.send(()).unwrap();
                release_rx.recv_timeout(HARNESS_LIMIT).unwrap();
            }
            write_snapshot(&destination, &tmp, body)
        })
        .unwrap();
        publisher.publish(r#"{"rendered_frames":1}"#.to_owned());
        entered_rx.recv_timeout(HARNESS_LIMIT).unwrap();
        publisher.publish(r#"{"rendered_frames":2}"#.to_owned());
        let latest = r#"{"rendered_frames":3,"complete":true}"#;
        publisher.publish(latest.to_owned());

        let shared = Arc::clone(&publisher.shared);
        let shutdown = std::thread::spawn(move || drop(publisher));
        let stopping = {
            let (pending, ready) = &*shared;
            let pending = pending.lock().unwrap();
            let (pending, _) = ready
                .wait_timeout_while(pending, HARNESS_LIMIT, |pending| !pending.stopping)
                .unwrap();
            pending.stopping
        };
        // Releasing only after Drop has stopped the queue also proves that
        // shutdown does not hold the queue lock while joining its worker.
        let _ = release_tx.send(());
        shutdown.join().unwrap();
        assert!(stopping, "shutdown must stop the queue before joining");
        let actual = std::fs::read_to_string(&path).unwrap();
        assert_eq!(actual, latest);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&actual).unwrap()["rendered_frames"],
            3
        );
    }

    #[test]
    fn blocked_writer_leaves_publish_unblocked_and_only_latest_pending() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (attempt_tx, attempt_rx) = mpsc::channel();
        let mut first = true;
        let publisher = Arc::new(
            Publisher::start_with_writer(move |body| {
                attempt_tx.send(body.to_owned()).unwrap();
                if first {
                    first = false;
                    entered_tx.send(()).unwrap();
                    release_rx.recv_timeout(HARNESS_LIMIT).unwrap();
                }
                Ok(())
            })
            .unwrap(),
        );
        publisher.publish("active".to_owned());
        entered_rx.recv_timeout(HARNESS_LIMIT).unwrap();
        let producer = Arc::clone(&publisher);
        let (published_tx, published_rx) = mpsc::channel();
        let producer = std::thread::spawn(move || {
            for sequence in 0..100 {
                producer.publish(format!("pending-{sequence}"));
            }
            published_tx.send(()).unwrap();
        });
        let published_during_io = published_rx.recv_timeout(HARNESS_LIMIT).is_ok();
        let pending = if published_during_io {
            publisher.shared.0.lock().unwrap().body.clone()
        } else {
            None
        };
        // Always unblock before assertions/joins, including a regression that
        // incorrectly holds the queue mutex across writer I/O.
        let _ = release_tx.send(());
        producer.join().unwrap();
        drop(Arc::try_unwrap(publisher).unwrap_or_else(|_| panic!("producer retained publisher")));

        assert!(published_during_io, "publish waited for the blocked writer");
        assert_eq!(pending.as_deref(), Some("pending-99"));
        let attempts: Vec<_> = attempt_rx.into_iter().collect();
        assert_eq!(attempts, ["active", "pending-99"]);
    }

    #[test]
    fn unchanged_pending_body_is_deduplicated_only_after_success() {
        for succeeds in [true, false] {
            let (entered_tx, entered_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel();
            let (attempt_tx, attempt_rx) = mpsc::channel();
            let mut first = true;
            let publisher = Publisher::start_with_writer(move |body| {
                attempt_tx.send(body.to_owned()).unwrap();
                if first {
                    first = false;
                    entered_tx.send(()).unwrap();
                    release_rx.recv_timeout(HARNESS_LIMIT).unwrap();
                    if !succeeds {
                        return Err(io::Error::from(io::ErrorKind::PermissionDenied));
                    }
                }
                Ok(())
            })
            .unwrap();
            publisher.publish("unchanged".to_owned());
            entered_rx.recv_timeout(HARNESS_LIMIT).unwrap();
            publisher.publish("unchanged".to_owned());
            release_tx.send(()).unwrap();
            drop(publisher);
            let attempts: Vec<_> = attempt_rx.into_iter().collect();
            assert_eq!(attempts.len(), if succeeds { 1 } else { 2 });
            assert!(attempts.iter().all(|body| body == "unchanged"));
        }
    }

    #[test]
    fn unchanged_body_retries_after_real_write_or_rename_failure() {
        for fail_rename in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let parent = dir.path().join("snapshots");
            let path = parent.join("state.json");
            if fail_rename {
                std::fs::create_dir_all(&path).unwrap();
            }
            let destination = path.clone();
            let tmp = path.with_extension("tmp");
            let (attempt_tx, attempt_rx) = mpsc::channel();
            let publisher = Publisher::start_with_writer(move |body| {
                let result = write_snapshot(&destination, &tmp, body);
                attempt_tx.send(result.is_ok()).unwrap();
                result
            })
            .unwrap();
            let body = r#"{"rendered_frames":42}"#;
            publisher.publish(body.to_owned());
            assert!(!attempt_rx.recv_timeout(HARNESS_LIMIT).unwrap());
            if fail_rename {
                std::fs::remove_dir(&path).unwrap();
            } else {
                std::fs::create_dir(&parent).unwrap();
            }
            publisher.publish(body.to_owned());
            drop(publisher);
            assert_eq!(attempt_rx.into_iter().collect::<Vec<_>>(), [true]);
            assert_eq!(std::fs::read_to_string(path).unwrap(), body);
        }
    }

    #[test]
    fn failed_active_body_cannot_replace_a_newer_pending_document() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (attempt_tx, attempt_rx) = mpsc::channel();
        let mut first = true;
        let publisher = Publisher::start_with_writer(move |body| {
            attempt_tx.send(body.to_owned()).unwrap();
            if first {
                first = false;
                entered_tx.send(()).unwrap();
                release_rx.recv_timeout(HARNESS_LIMIT).unwrap();
                return Err(io::Error::from(io::ErrorKind::PermissionDenied));
            }
            Ok(())
        })
        .unwrap();
        publisher.publish(r#"{"rendered_frames":10}"#.to_owned());
        entered_rx.recv_timeout(HARNESS_LIMIT).unwrap();
        publisher.publish(r#"{"rendered_frames":11}"#.to_owned());
        publisher.publish(r#"{"rendered_frames":12}"#.to_owned());
        release_tx.send(()).unwrap();
        drop(publisher);

        let frames: Vec<_> = attempt_rx
            .into_iter()
            .map(|body| {
                serde_json::from_str::<serde_json::Value>(&body).unwrap()["rendered_frames"]
                    .as_u64()
                    .unwrap()
            })
            .collect();
        assert_eq!(frames, [10, 12]);
    }

    #[test]
    fn failed_body_does_not_retry_without_another_submission() {
        let (attempt_tx, attempt_rx) = mpsc::channel();
        let publisher = Publisher::start_with_writer(move |_| {
            attempt_tx.send(()).unwrap();
            Err(io::Error::from(io::ErrorKind::PermissionDenied))
        })
        .unwrap();
        publisher.publish("failed".to_owned());
        attempt_rx.recv_timeout(HARNESS_LIMIT).unwrap();
        drop(publisher);
        assert_eq!(attempt_rx.into_iter().count(), 0);
    }
}
