//! linux-drm-syncobj v1, explicit sync (P-SY-09, #364, #588), as Mutter 51
//! offers it (`src/wayland/meta-wayland-linux-drm-syncobj.c`).
//!
//! - Advertised at version 1 only when the renderer can honour it, checked
//!   in the order of `meta_wayland_drm_syncobj_manager_new` ([`probe`],
//!   [`SyncobjSupport::verdict`]): the EGL display has
//!   `EGL_ANDROID_native_fence_sync`, the renderer's EGL device names a DRM
//!   render node (else its DRM node), that node opens read-write, has
//!   `DRM_CAP_SYNCOBJ_TIMELINE`, and knows `drmSyncobjEventfd`. Mutter asks
//!   this of whatever renderer its backend has (`meta_wayland_compositor_new`
//!   calls `meta_wayland_drm_syncobj_init` unconditionally; no environment
//!   or debug switch), so Tuna Desktop asks it on the DRM backend and nested
//!   alike. Software rendering and KMS-only devices such as vkms have no
//!   timeline syncobjs: the global stays absent there.
//! - Commits are validated in Mutter's order
//!   (`meta_wayland_surface_explicit_sync_validate`), by the vendored
//!   Smithay (`third-party/smithay/DRM-SYNCOBJ-PATCH.diff`).
//! - A valid commit's acquire point holds the commit until the point
//!   signals: a Smithay blocker released by the point's eventfd, as
//!   Mutter's transaction waits on `meta_wayland_drm_syncobj_create_source`.
//!   A point already signalled applies at once, as there.
//! - Rendered buffers retain the real completion fences of every GLES frame
//!   that sampled them. On final release those GPU fences are merged and
//!   transferred into the client's timeline, never CPU-signalled on reference
//!   drop before GPU completion.

use std::{path::PathBuf, sync::Mutex};

use smithay::backend::drm::DrmDeviceFd;
use smithay::backend::renderer::gles::{Capability, GlesRenderer};
use smithay::backend::renderer::sync::Fence;
use smithay::backend::renderer::ImportDma as _;
use smithay::delegate_drm_syncobj;
use smithay::reexports::calloop::{LoopHandle, RegistrationToken};
use smithay::reexports::drm::control::Device as _;
use smithay::reexports::drm::{Device as _, DriverCapability};
use smithay::reexports::wayland_server::{protocol::wl_surface::WlSurface, Resource, Weak};
use smithay::utils::DeviceFd;
use smithay::wayland::compositor::{
    add_blocker, with_states, Blocker, BlockerState, BufferAssignment, CompositorHandler,
    SurfaceAttributes,
};
use smithay::wayland::drm_syncobj::{
    pending_commit_error, reject_pending_commit, supports_syncobj_eventfd, DrmSyncPointBlocker,
    DrmSyncPointSource, DrmSyncobjCachedState, DrmSyncobjHandler, DrmSyncobjState, DrmTimeline,
};

use crate::State;

/// What the renderer's device offers explicit sync, in the order Mutter
/// asks. A [`probe`] stops at the first missing piece; later fields then
/// keep their defaults.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SyncobjSupport {
    /// The EGL display has `EGL_ANDROID_native_fence_sync` (Cogl's
    /// `COGL_WINSYS_FEATURE_SYNC_FD`).
    pub native_fence_sync: bool,
    /// The renderer's EGL device's DRM render node, else its DRM node.
    pub device_path: Option<PathBuf>,
    /// That node, opened read-write; timelines are imported on it.
    pub device: Option<DrmDeviceFd>,
    /// `drmGetCap(DRM_CAP_SYNCOBJ_TIMELINE)` succeeded and is set.
    pub timeline: bool,
    /// `drmSyncobjEventfd` exists: a probe on handle 0 fails with ENOENT.
    pub eventfd: bool,
    /// A real renderer GPU fence successfully transfers into a DRM timeline.
    pub gpu_release_fence: bool,
}

impl SyncobjSupport {
    /// Mutter's verdict: `Ok` when it creates the global, else why not,
    /// in its words.
    pub fn verdict(&self) -> Result<(), String> {
        if !self.native_fence_sync {
            return Err("Missing 'EGL_ANDROID_native_fence_sync'".into());
        }
        let Some(path) = &self.device_path else {
            return Err("Failed to find EGL device to initialize linux-drm-syncobj-v1".into());
        };
        if self.device.is_none() {
            return Err(format!("Failed to open DRM device {}", path.display()));
        }
        if !self.timeline {
            return Err("Failed to check DRM syncobj timeline capability".into());
        }
        if !self.eventfd {
            return Err(
                "drmSyncobjEventfd failed: linux-drm-syncobj requires eventfd support".into(),
            );
        }
        if !self.gpu_release_fence {
            return Err("Renderer cannot export and transfer GPU release fences".into());
        }
        Ok(())
    }
}

/// Ask `renderer` what Mutter asks Cogl and the kernel.
pub fn probe(renderer: &mut GlesRenderer) -> SyncobjSupport {
    let display = renderer.egl_context().display();
    let mut support = SyncobjSupport {
        native_fence_sync: display
            .extensions()
            .iter()
            .any(|e| e == "EGL_ANDROID_native_fence_sync"),
        ..SyncobjSupport::default()
    };
    if !support.native_fence_sync {
        return support;
    }
    support.device_path = smithay::backend::egl::EGLDevice::device_for_display(display)
        .ok()
        .and_then(|device| {
            device
                .render_device_path()
                .or_else(|_| device.drm_device_path())
                .ok()
        });
    let Some(path) = &support.device_path else {
        return support;
    };
    // std opens with O_CLOEXEC, as Mutter's open (O_RDWR | O_CLOEXEC).
    let Ok(file) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
    else {
        return support;
    };
    let device = DrmDeviceFd::new(DeviceFd::from(std::os::fd::OwnedFd::from(file)));
    support.timeline = device
        .get_driver_capability(DriverCapability::TimelineSyncObj)
        .is_ok_and(|cap| cap != 0);
    support.eventfd = support.timeline && supports_syncobj_eventfd(&device);
    if support.eventfd
        && renderer.capabilities().contains(&Capability::ExportFence)
        && renderer.dmabuf_formats().iter().next().is_some()
    {
        let display = renderer.egl_context().display().clone();
        support.gpu_release_fence = renderer
            .with_context(|gl| {
                let Ok(fence) = smithay::backend::egl::fence::EGLFence::create(&display) else {
                    return false;
                };
                unsafe { gl.Flush() };
                let Some(fd) = Fence::export(&fence) else {
                    return false;
                };
                let Ok(handle) = device.create_syncobj(false) else {
                    return false;
                };
                let exported = device.syncobj_to_fd(handle, false);
                let _ = device.destroy_syncobj(handle);
                exported
                    .and_then(|fd| DrmTimeline::new(&device, fd))
                    .and_then(|timeline| {
                        timeline
                            .point(1)
                            .import_sync_file(std::os::fd::AsFd::as_fd(&fd))
                    })
                    .is_ok()
            })
            .unwrap_or(false);
    }
    support.device = Some(device);
    support
}

impl State {
    /// Advertise `wp_linux_drm_syncobj_manager_v1` only after probing the
    /// actual DMA-BUF backend renderer and its native device, as Mutter.
    pub(crate) fn enable_explicit_sync(&mut self) -> bool {
        if self.protocols.syncobj.is_some() {
            return true;
        }
        let Some(backend) = self.protocols.dmabuf_backend.as_ref() else {
            eprintln!("tuna-compositor: explicit sync disabled: no DMA-BUF renderer");
            return false;
        };
        let support = {
            let mut renderer = backend.renderer();
            probe(&mut renderer)
        };
        if let Err(reason) = support.verdict() {
            eprintln!("tuna-compositor: explicit sync disabled: {reason}");
            return false;
        }
        let (Some(path), Some(device)) = (support.device_path, support.device) else {
            return false;
        };
        eprintln!(
            "tuna-compositor: explicit sync (linux-drm-syncobj v1) on {}",
            path.display()
        );
        self.protocols.syncobj = Some(DrmSyncobjState::new::<State>(&self.dh, device));
        true
    }

    /// Acquire points held commits wait on, for the event loop to watch.
    pub(crate) fn take_acquire_waits(&mut self) -> Vec<(DrmSyncPointSource, Weak<WlSurface>)> {
        std::mem::take(&mut self.protocols.acquire_waits)
    }
}

impl DrmSyncobjHandler for State {
    fn drm_syncobj_state(&mut self) -> Option<&mut DrmSyncobjState> {
        self.protocols.syncobj.as_mut()
    }
}
delegate_drm_syncobj!(State);

#[derive(Default)]
struct PendingAcquires(Mutex<Vec<DrmSyncPointBlocker>>);

fn acquire_failure(state: &State, surface: &WlSurface, message: String) {
    reject_pending_commit(surface);
    if let Some(client) = surface.client() {
        client.kill(
            &state.dh,
            smithay::reexports::wayland_server::backend::protocol::ProtocolError {
                code:
                    smithay::reexports::wayland_server::protocol::wl_display::Error::Implementation
                        as u32,
                object_id: 1,
                object_interface: "wl_display".into(),
                message,
            },
        );
    }
}

/// Pre-commit: hold a valid commit whose acquire point has not signalled
/// until it does. Invalid commits are left to Smithay's check, which
/// disconnects the client.
pub(crate) fn hold_until_acquired(state: &mut State, surface: &WlSurface) {
    if state.protocols.syncobj.is_none() {
        return;
    }
    let acquire = with_states(surface, |states| {
        states
            .data_map
            .insert_if_missing_threadsafe(PendingAcquires::default);
        let mut waits = states
            .data_map
            .get::<PendingAcquires>()
            .unwrap()
            .0
            .lock()
            .unwrap();
        waits.retain(|wait| wait.state() == BlockerState::Pending);
        if matches!(
            states
                .cached_state
                .get::<SurfaceAttributes>()
                .pending()
                .buffer,
            Some(BufferAssignment::Removed)
        ) {
            for wait in waits.drain(..) {
                wait.cancel();
            }
        }
        let acquire = states
            .cached_state
            .get::<DrmSyncobjCachedState>()
            .pending()
            .acquire_point
            .clone()?;
        pending_commit_error(states).is_none().then_some(acquire)
    });
    let Some(acquire) = acquire else {
        return;
    };
    if acquire.is_signaled() {
        return;
    }
    match acquire.generate_blocker() {
        Ok((blocker, source)) => {
            with_states(surface, |states| {
                states
                    .data_map
                    .get::<PendingAcquires>()
                    .unwrap()
                    .0
                    .lock()
                    .unwrap()
                    .push(blocker.clone());
            });
            add_blocker(surface, blocker);
            state
                .protocols
                .acquire_waits
                .push((source, surface.downgrade()));
        }
        Err(err) => acquire_failure(
            state,
            surface,
            format!("Cannot wait for explicit-sync acquire: {err}"),
        ),
    }
}

/// Acquire points the event loop is watching, with the surface whose
/// commit each holds.
#[derive(Debug, Default)]
pub(crate) struct AcquireWaits {
    next: u64,
    watching: Vec<(u64, RegistrationToken, Weak<WlSurface>, DrmSyncPointBlocker)>,
}

impl AcquireWaits {
    /// Watch the `queued` acquire points on the loop; `signalled` runs
    /// with a point's id once it signals. Points of surfaces that are gone
    /// stop being watched, so a client that never signals leaves no
    /// eventfds behind.
    pub(crate) fn watch<L: 'static>(
        &mut self,
        handle: &LoopHandle<'static, L>,
        queued: Vec<(DrmSyncPointSource, Weak<WlSurface>)>,
        signalled: fn(&mut L, u64, &Weak<WlSurface>),
    ) -> Vec<Weak<WlSurface>> {
        let mut failed = Vec::new();
        for (source, surface) in queued {
            let id = self.next;
            self.next += 1;
            let watched = surface.clone();
            let blocker = source.blocker();
            match handle.insert_source(source, move |(), _, data: &mut L| {
                signalled(data, id, &surface);
                Ok(())
            }) {
                Ok(token) => self.watching.push((id, token, watched, blocker)),
                Err(err) => {
                    eprintln!(
                        "tuna-compositor: explicit sync: watch acquire point: {}",
                        err.error
                    );
                    drop(err);
                    failed.push(watched);
                }
            }
        }
        self.watching.retain(|(_, token, surface, blocker)| {
            let keep = surface.upgrade().is_ok() && blocker.state() == BlockerState::Pending;
            if !keep {
                handle.remove(*token);
            }
            keep
        });
        failed
    }

    /// Point `id` signalled; its source has removed itself.
    pub(crate) fn signalled(&mut self, id: u64) {
        self.watching.retain(|(watched, _, _, _)| *watched != id);
    }
}

/// A calloop registration failure must cancel/disconnect, not authorize an
/// unacquired transaction or leave it waiting without an event source.
pub(crate) fn acquire_watch_failed(state: &mut State, surface: &Weak<WlSurface>) {
    if let Ok(surface) = surface.upgrade() {
        acquire_failure(
            state,
            &surface,
            "Cannot register explicit-sync acquire wait".into(),
        );
    }
    acquired(state, surface);
}

/// An acquire point on `surface` signalled: apply the commits it held.
pub(crate) fn acquired(state: &mut State, surface: &Weak<WlSurface>) {
    if let Ok(surface) = surface.upgrade() {
        let cancelled = with_states(&surface, |states| {
            states
                .data_map
                .get::<PendingAcquires>()
                .is_some_and(|waits| {
                    waits
                        .0
                        .lock()
                        .unwrap()
                        .iter()
                        .any(|wait| wait.state() == BlockerState::Cancelled)
                })
        });
        if cancelled {
            acquire_failure(
                state,
                &surface,
                "Explicit-sync acquire device was invalidated".into(),
            );
        }
    }
    if let Some(client) = surface.upgrade().ok().and_then(|s| s.client()) {
        let dh = state.dh.clone();
        state
            .client_compositor_state(&client)
            .blocker_cleared(state, &dh);
    }
}

#[cfg(test)]
mod tests {
    use super::SyncobjSupport;
    use smithay::reexports::wayland_protocols::wp::linux_drm_syncobj::v1::server::wp_linux_drm_syncobj_surface_v1::Error;
    use smithay::wayland::drm_syncobj::{commit_error, CommitBuffer};

    fn capable() -> SyncobjSupport {
        let null = std::fs::File::open("/dev/null").unwrap();
        SyncobjSupport {
            native_fence_sync: true,
            device_path: Some("/dev/dri/renderD128".into()),
            device: Some(smithay::backend::drm::DrmDeviceFd::new(
                smithay::utils::DeviceFd::from(std::os::fd::OwnedFd::from(null)),
            )),
            timeline: true,
            eventfd: true,
            gpu_release_fence: true,
        }
    }

    /// `meta_wayland_drm_syncobj_manager_new`'s checks, in its order and
    /// words: the first one missing is the reason.
    #[test]
    fn explicit_sync_needs_every_capability_mutter_checks() {
        assert_eq!(capable().verdict(), Ok(()));
        type Without = fn(&mut SyncobjSupport);
        let cases: [(Without, &str); 6] = [
            (
                |s| s.native_fence_sync = false,
                "Missing 'EGL_ANDROID_native_fence_sync'",
            ),
            (
                |s| s.device_path = None,
                "Failed to find EGL device to initialize linux-drm-syncobj-v1",
            ),
            (
                |s| s.device = None,
                "Failed to open DRM device /dev/dri/renderD128",
            ),
            (
                |s| s.timeline = false,
                "Failed to check DRM syncobj timeline capability",
            ),
            (
                |s| s.eventfd = false,
                "drmSyncobjEventfd failed: linux-drm-syncobj requires eventfd support",
            ),
            (
                |s| s.gpu_release_fence = false,
                "Renderer cannot export and transfer GPU release fences",
            ),
        ];
        for (without, reason) in cases {
            let mut support = capable();
            without(&mut support);
            assert_eq!(support.verdict(), Err(reason.to_owned()));
        }
        // Order: no native fence sync wins over everything after it.
        assert_eq!(
            SyncobjSupport::default().verdict(),
            Err("Missing 'EGL_ANDROID_native_fence_sync'".to_owned())
        );
    }

    /// `meta_wayland_surface_explicit_sync_validate`: the error each commit
    /// raises, timelines named by number.
    #[test]
    fn explicit_sync_commit_checks_follow_mutters_order() {
        let check = |buffer, acquire: Option<(u8, u64)>, release: Option<(u8, u64)>| {
            commit_error(
                buffer,
                acquire.as_ref().map(|(t, p)| (t, *p)),
                release.as_ref().map(|(t, p)| (t, *p)),
            )
            .map(|(error, _)| error)
        };
        use CommitBuffer::{Dmabuf, None as NoBuffer, Other};
        // A dmabuf with both points, acquire before release on one
        // timeline or on two: applied.
        assert_eq!(check(Dmabuf, Some((1, 1)), Some((1, 2))), None);
        assert_eq!(check(Dmabuf, Some((1, 5)), Some((2, 1))), None);
        // Nothing to synchronise: fine.
        assert_eq!(check(NoBuffer, None, None), None);
        // Any other buffer type, points or not, comes first.
        assert_eq!(check(Other, None, None), Some(Error::UnsupportedBuffer));
        assert_eq!(
            check(Other, Some((1, 1)), Some((1, 2))),
            Some(Error::UnsupportedBuffer)
        );
        // A dmabuf needs both points, acquire first.
        assert_eq!(check(Dmabuf, None, None), Some(Error::NoAcquirePoint));
        assert_eq!(
            check(Dmabuf, None, Some((1, 2))),
            Some(Error::NoAcquirePoint)
        );
        assert_eq!(
            check(Dmabuf, Some((1, 1)), None),
            Some(Error::NoReleasePoint)
        );
        // Same timeline: release strictly after acquire.
        assert_eq!(
            check(Dmabuf, Some((1, 2)), Some((1, 2))),
            Some(Error::ConflictingPoints)
        );
        assert_eq!(
            check(Dmabuf, Some((1, 3)), Some((1, 2))),
            Some(Error::ConflictingPoints)
        );
        // Points without a buffer (none attached, or a null one).
        assert_eq!(check(NoBuffer, Some((1, 1)), None), Some(Error::NoBuffer));
        assert_eq!(check(NoBuffer, None, Some((1, 2))), Some(Error::NoBuffer));
        assert_eq!(
            check(NoBuffer, Some((1, 1)), Some((1, 2))),
            Some(Error::NoBuffer)
        );
    }
}
