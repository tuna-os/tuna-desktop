use calloop::generic::Generic;
use calloop::{EventSource, Interest, Mode, Poll, PostAction, Readiness, Token, TokenFactory};
use drm::control::Device;
use std::sync::{Mutex, Weak};
use std::{
    io,
    os::unix::io::{AsFd, AsRawFd, BorrowedFd, OwnedFd},
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc,
    },
};

use crate::backend::drm::{DrmDeviceFd, WeakDrmDeviceFd};
use crate::backend::renderer::sync::{Fence, Interrupted};
use crate::wayland::compositor::{Blocker, BlockerState};

#[derive(Debug)]
pub(super) struct DrmTimelineInner {
    timeline_fd: OwnedFd,
    dev_ctx: Mutex<DrmTimelineDeviceSpecific>,
}

impl DrmTimelineInner {
    pub(super) fn update_device(&self, device: &DrmDeviceFd) -> io::Result<()> {
        let mut ctx = self.dev_ctx.lock().unwrap();
        let mut new = DrmTimelineDeviceSpecific::import(self.timeline_fd.as_fd(), device)?;
        for (point, eventfd) in ctx
            .event_fds
            .iter()
            .flat_map(|(p, fd)| fd.upgrade().map(|fd| (p, fd)))
        {
            device.syncobj_eventfd(new.syncobj, *point, eventfd.as_fd(), false)?;
            new.event_fds.push((*point, Arc::downgrade(&eventfd)));
        }
        // The replacement already owns the registrations. Dropping the old
        // context must destroy its handle without waking those new watches.
        ctx.event_fds.clear();
        *ctx = new;
        Ok(())
    }

    pub(super) fn invalidate(&self) {
        self.dev_ctx.lock().unwrap().invalidate()
    }
}

#[derive(Debug)]
struct DrmTimelineDeviceSpecific {
    device: WeakDrmDeviceFd,
    syncobj: drm::control::syncobj::Handle,
    event_fds: Vec<(u64, Weak<OwnedFd>)>,
}

impl DrmTimelineDeviceSpecific {
    fn import(fd: BorrowedFd<'_>, device: &DrmDeviceFd) -> io::Result<Self> {
        let syncobj = device.fd_to_syncobj(fd, false)?;
        Ok(DrmTimelineDeviceSpecific {
            device: device.downgrade(),
            syncobj,
            event_fds: Vec::new(),
        })
    }

    fn invalidate(&mut self) {
        if let Some(device) = self.device.upgrade() {
            let _ = device.destroy_syncobj(self.syncobj);
        }
        self.device = WeakDrmDeviceFd::new();
        // trigger event fds
        for eventfd in self.event_fds.drain(..).filter_map(|(_, x)| Weak::upgrade(&x)) {
            let _ = rustix::io::write(&eventfd, &1_u64.to_ne_bytes());
        }
    }
}

impl Drop for DrmTimelineDeviceSpecific {
    fn drop(&mut self) {
        self.invalidate();
    }
}

/// DRM timeline syncobj
#[derive(Clone, Debug)]
pub struct DrmTimeline(pub(super) Arc<DrmTimelineInner>);

impl PartialEq for DrmTimeline {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl DrmTimeline {
    /// Import DRM timeline from file descriptor
    pub fn new(device: &DrmDeviceFd, fd: OwnedFd) -> io::Result<Self> {
        let dev_ctx = Mutex::new(DrmTimelineDeviceSpecific::import(fd.as_fd(), device)?);
        Ok(Self(Arc::new(DrmTimelineInner {
            timeline_fd: fd,
            dev_ctx,
        })))
    }

    /// Refer to a point on this timeline without changing its signal state.
    pub fn point(&self, point: u64) -> DrmSyncPoint {
        DrmSyncPoint { timeline: self.clone(), point }
    }

    /// Query the last signalled timeline point
    pub fn query_signalled_point(&self) -> io::Result<u64> {
        let ctx = self.0.dev_ctx.lock().unwrap();
        let device = ctx
            .device
            .upgrade()
            .ok_or::<io::Error>(io::ErrorKind::InvalidInput.into())?;

        let mut points = [0];
        device.syncobj_timeline_query(&[ctx.syncobj], &mut points, false)?;
        Ok(points[0])
    }
}

/// Point on a DRM timeline syncobj
#[derive(Clone, Debug)]
pub struct DrmSyncPoint {
    pub(super) timeline: DrmTimeline,
    pub(super) point: u64,
}

impl DrmSyncPoint {
    /// Create an eventfd that will be signaled by the syncpoint
    pub fn eventfd(&self) -> io::Result<Arc<OwnedFd>> {
        let fd = rustix::event::eventfd(
            0,
            rustix::event::EventfdFlags::CLOEXEC | rustix::event::EventfdFlags::NONBLOCK,
        )?;
        let mut ctx = self.timeline.0.dev_ctx.lock().unwrap();
        ctx.device
            .upgrade()
            .ok_or::<io::Error>(io::ErrorKind::InvalidInput.into())?
            .syncobj_eventfd(ctx.syncobj, self.point, fd.as_fd(), false)?;

        let fd = Arc::new(fd);
        ctx.event_fds.retain(|(_, fd)| fd.upgrade().is_some());
        ctx.event_fds.push((self.point, Arc::downgrade(&fd)));
        Ok(fd)
    }

    /// Signal the sync point.
    pub fn signal(&self) -> io::Result<()> {
        let ctx = self.timeline.0.dev_ctx.lock().unwrap();
        ctx.device
            .upgrade()
            .ok_or::<io::Error>(io::ErrorKind::InvalidInput.into())?
            .syncobj_timeline_signal(&[ctx.syncobj], &[self.point])
    }

    /// Wait for sync point.
    pub fn wait(&self, timeout_nsec: i64) -> io::Result<()> {
        let ctx = self.timeline.0.dev_ctx.lock().unwrap();
        ctx.device
            .upgrade()
            .ok_or::<io::Error>(io::ErrorKind::InvalidInput.into())?
            .syncobj_timeline_wait(&[ctx.syncobj], &[self.point], timeout_nsec, false, false, false)?;
        Ok(())
    }

    /// Export DRM sync file for sync point.
    pub fn export_sync_file(&self) -> io::Result<OwnedFd> {
        let ctx = self.timeline.0.dev_ctx.lock().unwrap();
        let Some(device) = ctx.device.upgrade() else {
            return Err(io::ErrorKind::InvalidInput.into());
        };

        let syncobj = device.create_syncobj(false)?;
        if let Err(err) = device.syncobj_timeline_transfer(ctx.syncobj, syncobj, self.point, 0) {
            let _ = device.destroy_syncobj(syncobj);
            return Err(err);
        };
        let res = device.syncobj_to_fd(syncobj, true);
        let _ = device.destroy_syncobj(syncobj);
        res
    }

    /// Attach an actual GPU sync-file fence to this timeline point. Import
    /// requires an existing binary syncobj; FD_TO_HANDLE with handle zero
    /// does not import a sync-file.
    pub fn import_sync_file(&self, fd: BorrowedFd<'_>) -> io::Result<()> {
        #[repr(C)]
        struct Import {
            handle: u32,
            flags: u32,
            fd: i32,
            pad: u32,
            point: u64,
        }
        const IMPORT: rustix::ioctl::Opcode =
            rustix::ioctl::opcode::read_write::<Import>(b'd', 0xc2);
        let ctx = self.timeline.0.dev_ctx.lock().unwrap();
        let device = ctx.device.upgrade().ok_or(io::ErrorKind::InvalidInput)?;
        let binary = device.create_syncobj(false)?;
        let mut data = Import {
            handle: binary.into(), flags: 1, fd: fd.as_raw_fd(), pad: 0, point: 0,
        };
        // SAFETY: DRM_IOCTL_SYNCOBJ_FD_TO_HANDLE uses this fixed-width
        // structure; flags=IMPORT_SYNC_FILE and handle names the binary object.
        let result = unsafe {
            rustix::ioctl::ioctl(device.as_fd(), rustix::ioctl::Updater::<IMPORT, _>::new(&mut data))
        }.map_err(io::Error::from).and_then(|()| {
            device.syncobj_timeline_transfer(binary, ctx.syncobj, 0, self.point)
        });
        let _ = device.destroy_syncobj(binary);
        result
    }
    /// Create an [`calloop::EventSource`] and [`Blocker`] for this sync point.
    ///
    /// This will fail if `drmSyncobjEventfd` isn't supported by the device. See
    /// [`supports_syncobj_eventfd`](super::supports_syncobj_eventfd).
    pub fn generate_blocker(&self) -> io::Result<(DrmSyncPointBlocker, DrmSyncPointSource)> {
        let fd = self.eventfd()?;
        let signal = Arc::new(AtomicU8::new(0));
        let blocker = DrmSyncPointBlocker {
            signal: signal.clone(),
        };
        let source = DrmSyncPointSource {
            source: Generic::new(fd, Interest::READ, Mode::Level),
            signal,
            point: self.clone(),
        };
        Ok((blocker, source))
    }
}

impl Fence for DrmSyncPoint {
    fn is_signaled(&self) -> bool {
        self.timeline
            .query_signalled_point()
            .ok()
            .is_some_and(|point| point >= self.point)
    }

    fn wait(&self) -> Result<(), Interrupted> {
        self.wait(i64::MAX).map_err(|_| Interrupted)
    }

    fn is_exportable(&self) -> bool {
        true
    }

    fn export(&self) -> Option<OwnedFd> {
        self.export_sync_file().ok()
    }
}

/// Event source generating an event when a [`DrmSyncPoint`] is signalled..
#[derive(Debug)]
pub struct DrmSyncPointSource {
    source: Generic<Arc<OwnedFd>>,
    signal: Arc<AtomicU8>,
    point: DrmSyncPoint,
}

impl DrmSyncPointSource {
    /// A handle with which the compositor can cancel a never-submitted
    /// buffer's wait (for example when the surface is unmapped).
    pub fn blocker(&self) -> DrmSyncPointBlocker {
        DrmSyncPointBlocker { signal: self.signal.clone() }
    }
}

impl Drop for DrmSyncPointSource {
    fn drop(&mut self) {
        let _ = self.signal.compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire);
    }
}

impl EventSource for DrmSyncPointSource {
    type Event = ();
    type Metadata = ();
    type Ret = Result<(), std::io::Error>;
    type Error = io::Error;

    fn process_events<C>(
        &mut self,
        readiness: Readiness,
        token: Token,
        mut callback: C,
    ) -> Result<PostAction, Self::Error>
    where
        C: FnMut(Self::Event, &mut Self::Metadata) -> Self::Ret,
    {
        let signal = &self.signal;
        let point = &self.point;
        self.source.process_events(readiness, token, |_, _| {
            // An invalidated device wakes the eventfd too. It must cancel,
            // not authorize reading an unacquired buffer.
            let _ = signal.compare_exchange(
                0, if point.is_signaled() { 1 } else { 2 }, Ordering::AcqRel, Ordering::Acquire,
            );
            callback((), &mut ())?;
            Ok(PostAction::Remove)
        })
    }

    fn register(&mut self, poll: &mut Poll, token_factory: &mut TokenFactory) -> calloop::Result<()> {
        self.source.register(poll, token_factory)?;
        Ok(())
    }

    fn reregister(&mut self, poll: &mut Poll, token_factory: &mut TokenFactory) -> calloop::Result<()> {
        self.source.reregister(poll, token_factory)?;
        Ok(())
    }

    fn unregister(&mut self, poll: &mut Poll) -> calloop::Result<()> {
        self.source.unregister(poll)?;
        Ok(())
    }
}

/// [`Blocker`] implementation for an accompaning [`DrmSyncPointSource`]
#[derive(Debug, Clone)]
pub struct DrmSyncPointBlocker {
    signal: Arc<AtomicU8>,
}

impl DrmSyncPointBlocker {
    /// Cancel a commit without allowing its buffer to be sampled.
    pub fn cancel(&self) {
        let _ = self.signal.compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire);
    }
}

impl Blocker for DrmSyncPointBlocker {
    fn state(&self) -> BlockerState {
        match self.signal.load(Ordering::Acquire) {
            1 => BlockerState::Released,
            2 => BlockerState::Cancelled,
            _ => BlockerState::Pending,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancelled_acquire_never_becomes_released() {
        let blocker = DrmSyncPointBlocker { signal: Arc::new(AtomicU8::new(0)) };
        assert_eq!(blocker.state(), BlockerState::Pending);
        blocker.cancel();
        assert_eq!(blocker.state(), BlockerState::Cancelled);
        // A late event must not revive a commit discarded by an unmap.
        let _ = blocker.signal.compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire);
        assert_eq!(blocker.state(), BlockerState::Cancelled);
    }

    #[test]
    fn signalled_acquire_remains_released_when_watch_is_retired() {
        let blocker = DrmSyncPointBlocker { signal: Arc::new(AtomicU8::new(1)) };
        blocker.cancel();
        assert_eq!(blocker.state(), BlockerState::Released);
    }
}
