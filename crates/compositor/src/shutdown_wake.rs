//! Async-signal-safe shutdown transport, retired with its owning runtime.
use std::os::unix::net::UnixStream;
use std::sync::{atomic::AtomicBool, Arc};

pub(crate) struct Handlers(Vec<signal_hook::SigId>);
impl Drop for Handlers {
    fn drop(&mut self) {
        for id in self.0.drain(..) {
            signal_hook::low_level::unregister(id);
        }
    }
}
pub(crate) fn source(
    signals: &[i32],
    shutdown: &Arc<AtomicBool>,
) -> std::io::Result<(calloop::generic::Generic<UnixStream>, Handlers)> {
    let (read, write) = UnixStream::pair()?;
    read.set_nonblocking(true)?;
    let mut handlers = Handlers(Vec::new());
    for &signal in signals {
        handlers
            .0
            .push(signal_hook::flag::register(signal, shutdown.clone())?);
        // signal-hook uses a bounded nonblocking write in the signal handler;
        // no mutex, allocator, worker queue, or session routing runs there.
        handlers.0.push(signal_hook::low_level::pipe::register(
            signal,
            write.try_clone()?,
        )?);
    }
    Ok((
        calloop::generic::Generic::new(read, calloop::Interest::READ, calloop::Mode::Level),
        handlers,
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Read, sync::atomic::Ordering, time::Duration};
    #[test]
    fn signal_wakes_blocked_loop_and_drain_clears_readiness() {
        let shutdown = Arc::new(AtomicBool::new(false));
        let (source, _handlers) = source(&[signal_hook::consts::SIGUSR2], &shutdown).unwrap();
        let mut event_loop = calloop::EventLoop::<usize>::try_new().unwrap();
        event_loop
            .handle()
            .insert_source(source, |_, stream, count| {
                let mut stream = &**stream;
                let mut bytes = [0; 64];
                while matches!(stream.read(&mut bytes), Ok(n) if n > 0) {}
                *count += 1;
                Ok(calloop::PostAction::Continue)
            })
            .unwrap();
        signal_hook::low_level::raise(signal_hook::consts::SIGUSR2).unwrap();
        let mut count = 0;
        event_loop
            .dispatch(Some(Duration::from_secs(1)), &mut count)
            .unwrap();
        assert!(shutdown.load(Ordering::Relaxed));
        assert_eq!(count, 1);
        event_loop
            .dispatch(Some(Duration::ZERO), &mut count)
            .unwrap();
        assert_eq!(count, 1);
    }
}
