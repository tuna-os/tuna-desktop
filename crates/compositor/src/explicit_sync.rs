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
//! - The release point is signalled when the compositor drops the buffer
//!   (Smithay's renderer `Buffer`, after the next buffer replaced it and
//!   the frames that sampled it are done).

use std::path::PathBuf;

use smithay::backend::drm::DrmDeviceFd;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::sync::Fence;
use smithay::delegate_drm_syncobj;
use smithay::reexports::calloop::{LoopHandle, RegistrationToken};
use smithay::reexports::drm::{Device as _, DriverCapability};
use smithay::reexports::wayland_server::{protocol::wl_surface::WlSurface, Resource, Weak};
use smithay::utils::DeviceFd;
use smithay::wayland::compositor::{add_blocker, with_states, CompositorHandler};
use smithay::wayland::drm_syncobj::{
    pending_commit_error, supports_syncobj_eventfd, DrmSyncPointSource, DrmSyncobjCachedState,
    DrmSyncobjHandler, DrmSyncobjState,
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
        Ok(())
    }
}

/// Ask `renderer` what Mutter asks Cogl and the kernel.
pub fn probe(renderer: &GlesRenderer) -> SyncobjSupport {
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
    support.device = Some(device);
    support
}

impl State {
    /// Advertise `wp_linux_drm_syncobj_manager_v1` (version 1, as Mutter)
    /// when `support` passes Mutter's checks; timelines import on its
    /// device. Returns whether the global exists. Once only.
    pub fn enable_explicit_sync(&mut self, support: SyncobjSupport) -> bool {
        if self.protocols.syncobj.is_some() {
            return true;
        }
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

/// Pre-commit: hold a valid commit whose acquire point has not signalled
/// until it does. Invalid commits are left to Smithay's check, which
/// disconnects the client.
pub(crate) fn hold_until_acquired(state: &mut State, surface: &WlSurface) {
    if state.protocols.syncobj.is_none() {
        return;
    }
    let acquire = with_states(surface, |states| {
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
            add_blocker(surface, blocker);
            state
                .protocols
                .acquire_waits
                .push((source, surface.downgrade()));
        }
        // Mutter applies the commit at once when it cannot get the eventfd.
        Err(err) => eprintln!("tuna-compositor: explicit sync: acquire point eventfd: {err}"),
    }
}

/// Acquire points the event loop is watching, with the surface whose
/// commit each holds.
#[derive(Debug, Default)]
pub(crate) struct AcquireWaits {
    next: u64,
    watching: Vec<(u64, RegistrationToken, Weak<WlSurface>)>,
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
    ) {
        for (source, surface) in queued {
            let id = self.next;
            self.next += 1;
            let watched = surface.clone();
            match handle.insert_source(source, move |(), _, data: &mut L| {
                signalled(data, id, &surface);
                Ok(())
            }) {
                Ok(token) => self.watching.push((id, token, watched)),
                Err(err) => eprintln!(
                    "tuna-compositor: explicit sync: watch acquire point: {}",
                    err.error
                ),
            }
        }
        self.watching.retain(|(_, token, surface)| {
            let alive = surface.upgrade().is_ok();
            if !alive {
                handle.remove(*token);
            }
            alive
        });
    }

    /// Point `id` signalled; its source has removed itself.
    pub(crate) fn signalled(&mut self, id: u64) {
        self.watching.retain(|(watched, _, _)| *watched != id);
    }
}

/// An acquire point on `surface` signalled: apply the commits it held.
pub(crate) fn acquired(state: &mut State, surface: &Weak<WlSurface>) {
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
        }
    }

    /// `meta_wayland_drm_syncobj_manager_new`'s checks, in its order and
    /// words: the first one missing is the reason.
    #[test]
    fn explicit_sync_needs_every_capability_mutter_checks() {
        assert_eq!(capable().verdict(), Ok(()));
        type Without = fn(&mut SyncobjSupport);
        let cases: [(Without, &str); 5] = [
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
