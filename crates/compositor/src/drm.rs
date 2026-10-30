//! DRM/KMS hardware session backend (#52).
//!
//! The nested winit backend draws into a window of a host session; this
//! backend owns the display hardware itself, so Tuna Desktop can start from a
//! TTY through greetd:
//!
//! - **Session**: libseat (logind or seatd) grants device access and
//!   reports VT pause/activate. Ctrl+Alt+F1..F12 switches VTs.
//! - **Device**: udev's primary GPU for the seat (or
//!   `TUNA_DRM_DEVICE`), opened through the session; GBM allocates
//!   scanout buffers and EGL/GLES renders into them.
//! - **Outputs**: every connected connector with its preferred mode and
//!   a free CRTC, tiled left to right in the same order the output
//!   inventory uses. Each output renders when its previous page flip
//!   completed (vblank), so frames are paced by the display.
//! - **Input**: libinput on the session's seat. Relative pointer motion
//!   is accumulated here and clamped to the output union, so the window
//!   manager keeps receiving absolute positions like the nested backend.
//!
//! The pointer is drawn in software (an arrow of solid rectangles):
//! there is no host cursor on bare hardware. Client cursor surfaces are
//! not composited yet.

use smithay::backend::allocator::gbm::{GbmAllocator, GbmBufferFlags, GbmDevice};
use smithay::backend::allocator::Fourcc;
use smithay::backend::drm::{
    DrmDevice, DrmDeviceFd, DrmDeviceNotifier, DrmEvent, DrmEventMetadata, DrmEventTime,
    GbmBufferedSurface,
};
use smithay::backend::egl::{EGLContext, EGLDisplay};
use smithay::backend::input::{Event, InputEvent, KeyState, KeyboardKeyEvent, PointerMotionEvent};
use smithay::backend::libinput::{LibinputInputBackend, LibinputSessionInterface};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::session::libseat::{LibSeatSession, LibSeatSessionNotifier};
use smithay::backend::session::{Event as SessionEvent, Session};
use smithay::desktop::utils::OutputPresentationFeedback;
use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
use smithay::reexports::drm::control::{
    self, connector, crtc, Device as ControlDevice, ModeTypeFlags,
};
use smithay::reexports::input::Libinput;
use smithay::reexports::rustix::fs::OFlags;
use smithay::utils::{DeviceFd, Logical, Physical, Point, Rectangle, Size};

use crate::windows::ManagerInput;

pub mod libinput;

/// Scanout surface type: GBM buffers on the DRM device, each queued
/// frame carrying the presentation feedback of what it drew (#89).
pub type ScanoutSurface = GbmBufferedSurface<GbmAllocator<DrmDeviceFd>, OutputPresentationFeedback>;

/// A completed page flip: the frame is on the screen.
pub struct PageFlip {
    /// The flip was the primary output's (it paces fifo and commit
    /// timers, as one output's frame clock does in Mutter).
    pub primary: bool,
    /// Feedback of the surfaces the frame drew.
    pub feedback: Option<OutputPresentationFeedback>,
    /// Kernel vblank timestamp on CLOCK_MONOTONIC, when it gave one.
    pub time: Option<std::time::Duration>,
    /// Kernel vblank sequence.
    pub sequence: u64,
    /// The output's refresh interval.
    pub refresh: std::time::Duration,
}

/// One connector with a CRTC.
pub struct DrmOutput {
    /// Inventory name, e.g. `eDP-1`.
    pub name: String,
    /// CRTC driving the connector.
    pub crtc: crtc::Handle,
    pub connector: connector::Handle,
    native_owner: Option<tuna_shell_control::NativeOutputInfo>,
    /// GBM swapchain bound to the CRTC.
    pub surface: ScanoutSurface,
    /// Protocol output advertised to clients.
    pub output: Output,
    /// The connector's modes in Mutter's order (#340), each with the
    /// kernel mode it programs, and which one is preferred.
    pub modes: Vec<(crate::display_config::ModeSpec, control::Mode)>,
    pub preferred_mode: usize,
    /// Physical size in millimetres (the preferred-scale DPI).
    pub size_mm: (i32, i32),
    /// Mode size in physical pixels.
    pub size: Size<i32, Physical>,
    /// Top-left in the global logical space: GNOME's arrangement from
    /// monitors.xml, else left-to-right.
    pub loc: (i32, i32),
    /// Output scale from monitors.xml (`TUNA_SCALE` without one), #59.
    pub scale: f64,
    /// DisplayConfig transform (0-7) from monitors.xml or Settings, #345.
    pub transform: u32,
    /// Lit; GNOME Settings switched it off when false (#340).
    pub enabled: bool,
    /// The connector leading the mirrored logical monitor this output
    /// clones (same region, scale and mode size); not a wl_output.
    pub mirror_of: Option<String>,
    /// The mode a just-applied mode change replaced, until its first
    /// commit succeeds: a refused commit returns to it.
    rollback_mode: Option<control::Mode>,
    /// A refused commit restored the previous mode since the runtime
    /// last looked ([`DrmBackend::take_mode_reverted`]).
    mode_reverted: bool,
    /// A frame is queued and its page flip has not completed yet.
    pub pending: bool,
    pub(crate) last_frame: Option<crate::native_repaint::FrameSignature>,
    pub(crate) damage_tracker: Option<smithay::backend::renderer::damage::OutputDamageTracker>,
    wake_trace: bool,
}

/// Mutter's mode spec of a kernel mode: resolution, refresh rate as
/// `meta_calculate_drm_mode_refresh_rate` computes it, interlacing.
pub fn mode_spec(mode: &control::Mode) -> crate::display_config::ModeSpec {
    let (width, height) = mode.size();
    let (_, _, htotal) = mode.hsync();
    let (_, _, vtotal) = mode.vsync();
    crate::display_config::ModeSpec {
        width: i32::from(width),
        height: i32::from(height),
        refresh: crate::display_config::drm_refresh_rate(
            mode.clock(),
            htotal,
            vtotal,
            mode.vscan(),
        ),
        interlaced: mode.flags().bits() & crate::display_config::DRM_MODE_FLAG_INTERLACE != 0,
    }
}

impl DrmOutput {
    pub fn trace_wake_submission(&mut self) {
        if !std::mem::take(&mut self.wake_trace) {
            return;
        }
        let state = self.surface.surface().get_crtc(self.crtc);
        eprintln!(
            "tuna-compositor: drm: wake frame queued {} commit_pending={} crtc={state:?}",
            self.name,
            self.surface.surface().commit_pending(),
        );
    }

    /// Size in logical pixels (mode size over scale, swapped under
    /// 90°/270° rotation).
    pub fn logical_size(&self) -> (i32, i32) {
        crate::orientation::logical_extent(self.size.w, self.size.h, self.scale, self.transform)
    }

    /// Shown as its own wl_output: lit and not a mirror clone.
    pub fn is_leader(&self) -> bool {
        self.enabled && self.mirror_of.is_none()
    }

    /// The output whose lock surface this one shows: a clone shows its
    /// leader's.
    pub fn logical_name(&self) -> &str {
        self.mirror_of.as_deref().unwrap_or(&self.name)
    }

    /// Position of the mode the CRTC is set to (or about to be) in
    /// [`modes`](Self::modes).
    fn current_mode_index(&self) -> Option<usize> {
        let mode = self.surface.pending_mode();
        self.modes.iter().position(|(_, m)| *m == mode).or_else(|| {
            let spec = mode_spec(&mode);
            self.modes.iter().position(|(s, _)| s.equals(&spec))
        })
    }

    /// Keep the current `mode` as the wl_output's only mode, flagged
    /// preferred when it is the monitor's preferred one: Mutter's
    /// wl_output sends the current mode alone, never the list
    /// (`meta-wayland-outputs.c`).
    fn advertise_mode(&self, mode: control::Mode) {
        let mode = Mode::from(mode);
        for other in self.output.modes() {
            if other != mode {
                self.output.delete_mode(other);
            }
        }
        if self
            .modes
            .get(self.preferred_mode)
            .is_some_and(|(_, preferred)| Mode::from(*preferred) == mode)
        {
            self.output.set_preferred(mode);
        }
    }

    /// Turn the CRTC off (GNOME Settings disabled the monitor): queued
    /// frames are dropped, never presented.
    fn switch_off(&mut self) {
        if !self.enabled {
            return;
        }
        for mut feedback in self.surface.discard_pending_frames() {
            feedback.discarded();
        }
        if let Err(error) = self.surface.surface().clear() {
            eprintln!("tuna-compositor: drm: {} off: {error}", self.name);
        }
        self.surface.reset_buffers();
        self.enabled = false;
        self.mirror_of = None;
        self.rollback_mode = None;
        self.pending = false;
        self.last_frame = None;
        self.damage_tracker = None;
    }

    /// A frame in a new mode reached the hardware: the mode stays.
    pub fn mode_committed(&mut self) {
        self.rollback_mode = None;
    }

    /// The first commit in a new mode failed although the atomic test
    /// passed: return to the mode it replaced (Mutter falls back the
    /// same way when a modeset fails). Returns whether it did.
    pub fn revert_failed_mode(&mut self) -> bool {
        let Some(old) = self.rollback_mode.take() else {
            return false;
        };
        let failed = mode_spec(&self.surface.pending_mode()).id();
        if let Err(error) = self.surface.use_mode(old) {
            eprintln!(
                "tuna-compositor: drm: {}: restoring the previous mode failed: {error}",
                self.name
            );
            return false;
        }
        let (w, h) = old.size();
        self.size = (i32::from(w), i32::from(h)).into();
        self.output
            .change_current_state(Some(Mode::from(old)), None, None, None);
        self.advertise_mode(old);
        self.last_frame = None;
        self.damage_tracker = None;
        self.mode_reverted = true;
        eprintln!(
            "tuna-compositor: drm: {}: commit of {failed} failed; back to {}",
            self.name,
            mode_spec(&old).id()
        );
        true
    }
}

/// Hardware session state.
pub struct DrmBackend {
    /// libseat session (device access, VT switching).
    pub session: LibSeatSession,
    /// Opened KMS device.
    pub drm: DrmDevice,
    /// GLES renderer on the device's EGL display.
    pub renderer: GlesRenderer,
    /// Lit outputs, primary first.
    pub outputs: Vec<DrmOutput>,
    /// libinput context, suspended while the session is paused.
    pub libinput: Libinput,
    /// Whether the session currently owns the VT.
    pub active: bool,
    sleep_reset_pending: bool,
    /// DisplayConfig `PowerSaveMode` is not on (#355): nothing is submitted.
    powered_off: bool,
    /// Power saving turned every CRTC off; activation keeps them off.
    outputs_disabled: bool,
    wake_scanout_blocked: bool,
    wake_event_traces: u8,
    pointer: Point<f64, Logical>,
    corner_pressure: crate::corner_pressure::CornerPressure,
    hot_corner_active: bool,
    relative_motion_events: u64,
    ctrl: bool,
    alt: bool,
    /// libinput devices, for GNOME's touchpad and mouse settings (#60).
    devices: Vec<smithay::reexports::input::Device>,
    /// Mutter's seat touch-mode and the last pointing device, for the
    /// on-screen keyboard's auto-show (#349).
    touch_mode: crate::osk::TouchMode,
    input_settings: tuna_shell_control::InputSettings,
    /// Bumped by every applied layout or reverted mode (#340).
    layout_generation: u64,
}

/// Event sources the runtime installs on its loop.
pub struct DrmSources {
    /// Session pause/activate.
    pub session: LibSeatSessionNotifier,
    /// VBlank and device errors.
    pub drm: DrmDeviceNotifier,
    /// Input events.
    pub input: LibinputInputBackend,
}

/// Why the hardware session could not start.
#[derive(Debug)]
pub struct DrmInitError(pub String);

impl std::fmt::Display for DrmInitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "drm backend: {}", self.0)
    }
}

fn err(context: &str) -> impl Fn(String) -> DrmInitError + '_ {
    move |e| DrmInitError(format!("{context}: {e}"))
}

// Evdev keycodes (backend keycodes minus the XKB offset of 8).
const KEY_LEFTCTRL: u32 = 29;
const KEY_RIGHTCTRL: u32 = 97;
const KEY_LEFTALT: u32 = 56;
const KEY_RIGHTALT: u32 = 100;
const KEY_F1: u32 = 59;
const KEY_F10: u32 = 68;
const KEY_F11: u32 = 87;
const KEY_F12: u32 = 88;
const XKB_OFFSET: u32 = 8;

/// VT number for an evdev function-key code, if it is F1..F12.
pub fn vt_for_key(keycode: u32) -> Option<i32> {
    match keycode {
        KEY_F1..=KEY_F10 => Some((keycode - KEY_F1 + 1) as i32),
        KEY_F11 => Some(11),
        KEY_F12 => Some(12),
        _ => None,
    }
}

/// Clamp a pointer position into the union of output rectangles.
pub fn clamp_to_outputs(
    pos: Point<f64, Logical>,
    outputs: &[Rectangle<i32, Logical>],
) -> Point<f64, Logical> {
    if outputs.iter().any(|r| r.to_f64().contains(pos)) {
        return pos;
    }
    // Nearest point on the nearest output.
    let mut best = pos;
    let mut best_d = f64::INFINITY;
    for r in outputs {
        let r = r.to_f64();
        let x = pos.x.clamp(r.loc.x, r.loc.x + r.size.w - 1.0);
        let y = pos.y.clamp(r.loc.y, r.loc.y + r.size.h - 1.0);
        let d = (x - pos.x).powi(2) + (y - pos.y).powi(2);
        if d < best_d {
            best_d = d;
            best = (x, y).into();
        }
    }
    best
}

impl DrmBackend {
    pub fn native_outputs(&self) -> Vec<tuna_shell_control::NativeOutputInfo> {
        use std::os::fd::AsFd;
        if !self.active || !self.drm.is_active() {
            return Vec::new();
        }
        let Ok(device) = crate::native_output::device(self.drm.device_fd().as_fd()) else {
            return Vec::new();
        };
        if device != self.drm.device_id() {
            return Vec::new();
        }
        // Identity is captured during actual connector discovery, not scanned
        // every render tick. Shell admission revalidates canonical status/object.
        self.outputs
            .iter()
            .filter_map(|output| output.native_owner.as_ref())
            .filter(|owner| owner.drm_device == device)
            .cloned()
            .collect()
    }

    /// Open the seat, the primary GPU, every connected output, and
    /// libinput. Fails with a readable reason (no seat daemon, no GPU,
    /// nothing connected) so the launcher can report it.
    pub fn new() -> Result<(Self, DrmSources), DrmInitError> {
        let (mut session, session_notifier) =
            LibSeatSession::new().map_err(|e| err("libseat session")(e.to_string()))?;
        let seat = session.seat();
        let path = match std::env::var_os("TUNA_DRM_DEVICE") {
            Some(path) => std::path::PathBuf::from(path),
            None => smithay::backend::udev::primary_gpu(&seat)
                .ok()
                .flatten()
                .or_else(|| {
                    smithay::backend::udev::all_gpus(&seat)
                        .ok()
                        .and_then(|gpus| gpus.into_iter().next())
                })
                .ok_or_else(|| DrmInitError(format!("no GPU on seat {seat}")))?,
        };
        let fd = session
            .open(
                &path,
                OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK,
            )
            .map_err(|e| err("open drm device")(format!("{}: {e}", path.display())))?;
        let fd = DrmDeviceFd::new(DeviceFd::from(fd));
        let (mut drm, drm_notifier) =
            DrmDevice::new(fd.clone(), true).map_err(|e| err("drm device")(e.to_string()))?;
        let gbm = GbmDevice::new(fd.clone()).map_err(|e| err("gbm device")(e.to_string()))?;
        // SAFETY: the GBM device outlives the display; both are held for
        // the backend's lifetime through the renderer's context.
        let egl = unsafe { EGLDisplay::new(gbm.clone()) }
            .map_err(|e| err("egl display")(e.to_string()))?;
        let context = EGLContext::new(&egl).map_err(|e| err("egl context")(e.to_string()))?;
        let render_formats = context.dmabuf_render_formats().clone();
        // SAFETY: the context is current on this thread only.
        let renderer =
            unsafe { GlesRenderer::new(context) }.map_err(|e| err("gles")(e.to_string()))?;

        let resources = drm
            .resource_handles()
            .map_err(|e| err("drm resources")(e.to_string()))?;
        use std::os::fd::AsFd;
        let native_device = crate::native_output::device(drm.device_fd().as_fd())
            .ok()
            .filter(|device| *device == drm.device_id());
        let mut used: Vec<crtc::Handle> = Vec::new();
        let mut outputs = Vec::new();
        for handle in resources.connectors() {
            let Ok(info) = drm.get_connector(*handle, false) else {
                continue;
            };
            if info.state() != connector::State::Connected {
                continue;
            }
            // Mutter's monitor modes (#340): every connector mode,
            // deduplicated and sorted; the session starts in the
            // preferred one.
            let crtc_modes: Vec<crate::display_config::CrtcMode> = info
                .modes()
                .iter()
                .map(|m| crate::display_config::CrtcMode {
                    spec: mode_spec(m),
                    flags: m.flags().bits(),
                })
                .collect();
            let kernel_preferred = info
                .modes()
                .iter()
                .position(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED));
            let Some(listed) = crate::display_config::monitor_modes(&crtc_modes, kernel_preferred)
            else {
                continue;
            };
            let modes: Vec<(crate::display_config::ModeSpec, control::Mode)> = listed
                .modes
                .iter()
                .map(|&i| (crtc_modes[i].spec, info.modes()[i]))
                .collect();
            let preferred_mode = listed.preferred;
            let mode = modes[preferred_mode].1;
            let crtc = info
                .encoders()
                .iter()
                .filter_map(|enc| drm.get_encoder(*enc).ok())
                .flat_map(|enc| resources.filter_crtcs(enc.possible_crtcs()))
                .find(|crtc| !used.contains(crtc));
            let Some(crtc) = crtc else {
                continue;
            };
            let surface = match drm.create_surface(crtc, mode, &[*handle]) {
                Ok(surface) => surface,
                Err(e) => {
                    eprintln!("tuna-compositor: drm: skip connector: {e}");
                    continue;
                }
            };
            let allocator = GbmAllocator::new(
                gbm.clone(),
                GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
            );
            let surface = match GbmBufferedSurface::new(
                surface,
                allocator,
                &[Fourcc::Argb8888, Fourcc::Xrgb8888],
                render_formats.iter().copied(),
            ) {
                Ok(surface) => surface,
                Err(e) => {
                    eprintln!("tuna-compositor: drm: skip connector: {e}");
                    continue;
                }
            };
            used.push(crtc);
            let name = format!("{}-{}", info.interface().as_str(), info.interface_id());
            let (w, h) = mode.size();
            let size: Size<i32, Physical> = (i32::from(w), i32::from(h)).into();
            let (mm_w, mm_h) = info.size().unwrap_or((0, 0));
            let output = Output::new(
                name.clone(),
                PhysicalProperties {
                    size: (mm_w as i32, mm_h as i32).into(),
                    subpixel: Subpixel::Unknown,
                    make: "Tuna Desktop".to_owned(),
                    model: name.clone(),
                },
            );
            let out_mode = Mode::from(mode);
            output.change_current_state(Some(out_mode), None, None, None);
            output.set_preferred(out_mode);
            let native_owner = native_device.and_then(|device| {
                crate::native_output::resolve(
                    std::path::Path::new("/sys"),
                    device,
                    &name,
                    u32::from(*handle),
                )
                .ok()
            });
            outputs.push(DrmOutput {
                name,
                crtc,
                connector: *handle,
                native_owner,
                surface,
                output,
                modes,
                preferred_mode,
                size_mm: (mm_w as i32, mm_h as i32),
                size,
                loc: (0, 0),
                scale: 1.0,
                transform: 0,
                enabled: true,
                mirror_of: None,
                rollback_mode: None,
                mode_reverted: false,
                pending: false,
                last_frame: None,
                damage_tracker: None,
                wake_trace: false,
            });
        }
        if outputs.is_empty() {
            return Err(DrmInitError(format!(
                "no connected output on {}",
                path.display()
            )));
        }
        // GNOME's configuration for exactly these connectors (#59,
        // #340): modes, scale, position, rotation, mirrors and switched
        // off monitors; the rest at TUNA_SCALE (or 1), left to right.
        let names: Vec<String> = outputs.iter().map(|o| o.name.clone()).collect();
        let stored = crate::monitors::load(&names);
        let fallback_scale = std::env::var("TUNA_SCALE")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .map(crate::runtime::clamp_scale)
            .unwrap_or(1.0);
        let connectors: Vec<crate::display_config::StartupConnector> = outputs
            .iter()
            .map(|o| crate::display_config::StartupConnector {
                name: o.name.clone(),
                modes: o.modes.iter().map(|(spec, _)| *spec).collect(),
                preferred: o.preferred_mode,
            })
            .collect();
        let layouts =
            crate::display_config::startup_layouts(stored.as_ref(), &connectors, fallback_scale);

        let mut libinput = Libinput::new_with_udev(LibinputSessionInterface::from(session.clone()));
        libinput
            .udev_assign_seat(&seat)
            .map_err(|()| DrmInitError(format!("libinput: cannot assign seat {seat}")))?;
        let input = LibinputInputBackend::new(libinput.clone());
        let mut backend = Self {
            session,
            drm,
            renderer,
            outputs,
            libinput,
            active: true,
            sleep_reset_pending: false,
            powered_off: false,
            outputs_disabled: false,
            wake_scanout_blocked: false,
            wake_event_traces: 0,
            pointer: (0.0, 0.0).into(),
            ctrl: false,
            alt: false,
            devices: Vec::new(),
            touch_mode: Default::default(),
            input_settings: Default::default(),
            corner_pressure: Default::default(),
            hot_corner_active: true,
            relative_motion_events: 0,
            layout_generation: 0,
        };
        if let Err(error) = backend.apply_layout(&layouts) {
            // A stored mode the kernel refused: Mutter falls back to its
            // default configuration the same way.
            eprintln!("tuna-compositor: drm: stored display configuration not applied: {error}");
            let linear = crate::display_config::startup_layouts(None, &connectors, fallback_scale);
            backend
                .apply_layout(&linear)
                .map_err(|e| DrmInitError(format!("output layout: {e}")))?;
        }
        for out in &backend.outputs {
            if !out.enabled {
                eprintln!("tuna-compositor: drm: connector {} switched off", out.name);
                continue;
            }
            eprintln!(
                "tuna-compositor: drm: output {} {}x{} at {},{} scale {} transform {}{}",
                out.name,
                out.size.w,
                out.size.h,
                out.loc.0,
                out.loc.1,
                out.scale,
                out.transform,
                out.mirror_of
                    .as_ref()
                    .map(|leader| format!(" mirroring {leader}"))
                    .unwrap_or_default(),
            );
        }
        let (first_w, first_h) = backend.outputs[0].logical_size();
        let first_loc = backend.outputs[0].loc;
        backend.pointer = (
            f64::from(first_loc.0) + f64::from(first_w) / 2.0,
            f64::from(first_loc.1) + f64::from(first_h) / 2.0,
        )
            .into();
        Ok((
            backend,
            DrmSources {
                session: session_notifier,
                drm: drm_notifier,
                input,
            },
        ))
    }

    /// Apply a layout (#340), all or nothing. Every new mode is tested
    /// on the hardware first: Smithay's `use_mode` makes an atomic
    /// TEST_ONLY commit with a test buffer and keeps the old mode when
    /// the kernel refuses; one refusal returns every output already
    /// switched to its previous mode. Then outputs switch off, the rest
    /// take their position, scale, rotation and mirror leader, and the
    /// primary leader moves first. The next frame commits the modes;
    /// should that commit still fail, the output returns to its old
    /// mode ([`DrmOutput::revert_failed_mode`]).
    pub fn apply_layout(
        &mut self,
        layouts: &[crate::display_config::OutputLayout],
    ) -> Result<(), String> {
        let mut plan = Vec::with_capacity(layouts.len());
        for layout in layouts {
            let index = self
                .outputs
                .iter()
                .position(|o| o.name == layout.connector)
                .ok_or_else(|| format!("connector '{}' not found", layout.connector))?;
            let target = match &layout.enabled {
                None => None,
                Some(enabled) => {
                    let out = &self.outputs[index];
                    let mode = match enabled.mode {
                        None => out.surface.pending_mode(),
                        Some(spec) => out
                            .modes
                            .iter()
                            .find(|(s, _)| s.equals(&spec))
                            .map(|(_, m)| *m)
                            .ok_or_else(|| {
                                format!("mode {} not available on {}", spec.id(), out.name)
                            })?,
                    };
                    Some((enabled, mode))
                }
            };
            plan.push((index, target));
        }
        if plan.iter().all(|(_, target)| target.is_none()) {
            return Err("at least one output must stay on".into());
        }
        // Test phase: nothing visible changes yet.
        let mut tested: Vec<(usize, control::Mode)> = Vec::new();
        for (index, target) in &plan {
            let Some((_, mode)) = target else {
                continue;
            };
            let result = {
                let out = &mut self.outputs[*index];
                let old = out.surface.pending_mode();
                if old == *mode && out.enabled {
                    continue;
                }
                out.surface.use_mode(*mode).map(|()| old).map_err(|error| {
                    format!(
                        "{}: the kernel refused {}: {error}",
                        out.name,
                        mode_spec(mode).id()
                    )
                })
            };
            match result {
                Ok(old) => tested.push((*index, old)),
                Err(message) => {
                    for (index, old) in tested.into_iter().rev() {
                        let _ = self.outputs[index].surface.use_mode(old);
                    }
                    return Err(message);
                }
            }
        }
        // Commit phase.
        for (index, target) in plan {
            let out = &mut self.outputs[index];
            let Some((layout, mode)) = target else {
                out.switch_off();
                continue;
            };
            let replaced = tested
                .iter()
                .find(|(i, _)| *i == index)
                .map(|(_, old)| *old);
            if !out.enabled {
                out.surface.reset_buffers();
                out.pending = false;
                out.rollback_mode = None;
            } else if replaced.is_some() {
                out.rollback_mode = replaced;
            }
            out.enabled = true;
            out.mirror_of = layout.mirror_of.clone();
            let (w, h) = mode.size();
            out.size = (i32::from(w), i32::from(h)).into();
            out.scale = crate::runtime::clamp_scale(layout.scale);
            out.loc = layout.loc;
            out.transform = if crate::orientation::transform_is_supported(layout.transform) {
                layout.transform
            } else {
                0
            };
            out.output.change_current_state(
                Some(Mode::from(mode)),
                Some(crate::orientation::smithay_transform(out.transform)),
                Some(if out.scale == 1.0 {
                    smithay::output::Scale::Integer(1)
                } else {
                    smithay::output::Scale::Fractional(out.scale)
                }),
                Some(out.loc.into()),
            );
            out.advertise_mode(mode);
            out.last_frame = None;
            out.damage_tracker = None;
        }
        // GNOME's primary monitor first: it carries the top bar and
        // paces the frame clock.
        let primary = layouts.iter().find(|l| {
            l.enabled
                .as_ref()
                .is_some_and(|e| e.primary && e.mirror_of.is_none())
        });
        if let Some(position) =
            primary.and_then(|p| self.outputs.iter().position(|o| o.name == p.connector))
        {
            let primary = self.outputs.remove(position);
            self.outputs.insert(0, primary);
        }
        self.layout_generation += 1;
        Ok(())
    }

    /// Bumped by every applied layout and reverted mode: the runtime
    /// republishes the display state when it moves.
    pub fn layout_generation(&self) -> u64 {
        self.layout_generation
    }

    /// Whether a refused commit restored an output's previous mode
    /// since the last call.
    pub fn take_mode_reverted(&mut self) -> bool {
        let mut reverted = false;
        for out in &mut self.outputs {
            reverted |= std::mem::take(&mut out.mode_reverted);
        }
        if reverted {
            self.layout_generation += 1;
        }
        reverted
    }

    /// What `org.gnome.Mutter.DisplayConfig` reports (#340): every
    /// connector with its modes, and one logical monitor per lit leader
    /// with its clones. The primary leader is first.
    pub fn display_state(&self) -> crate::display_config::DisplayState {
        use crate::display_config::{DisplayState, LogicalState, MonitorState};
        let monitors = self
            .outputs
            .iter()
            .map(|out| {
                let properties = out.output.physical_properties();
                let builtin = crate::orientation::is_builtin_panel(&out.name);
                MonitorState {
                    spec: crate::monitors::MonitorSpec {
                        connector: out.name.clone(),
                        vendor: properties.make,
                        product: properties.model.clone(),
                        // Mutter's serial; the connector keeps session
                        // restore stable.
                        serial: out.name.clone(),
                    },
                    display_name: if builtin {
                        "Built-in display".to_owned()
                    } else {
                        properties.model
                    },
                    builtin,
                    width_mm: out.size_mm.0,
                    height_mm: out.size_mm.1,
                    modes: out.modes.iter().map(|(spec, _)| *spec).collect(),
                    preferred: out.preferred_mode,
                    current: out.enabled.then(|| out.current_mode_index()).flatten(),
                }
            })
            .collect();
        let logical = self
            .outputs
            .iter()
            .filter(|out| out.is_leader())
            .enumerate()
            .map(|(index, leader)| LogicalState {
                x: leader.loc.0,
                y: leader.loc.1,
                scale: leader.scale,
                transform: leader.transform,
                primary: index == 0,
                monitors: std::iter::once(leader.name.clone())
                    .chain(
                        self.outputs
                            .iter()
                            .filter(|o| {
                                o.enabled && o.mirror_of.as_deref() == Some(leader.name.as_str())
                            })
                            .map(|o| o.name.clone()),
                    )
                    .collect(),
            })
            .collect();
        DisplayState {
            serial: 0,
            monitors,
            logical,
        }
    }

    /// The mode each CRTC is actually set to, as the kernel reports it
    /// (`drmModeGetCrtc`): what the DRM smoke proof compares against
    /// the mode GNOME Settings asked for.
    pub fn crtc_modes(&self) -> Vec<(String, Option<String>)> {
        self.outputs
            .iter()
            .map(|out| {
                let mode = self
                    .drm
                    .get_crtc(out.crtc)
                    .ok()
                    .and_then(|info| info.mode())
                    .map(|mode| mode_spec(&mode).id());
                (out.name.clone(), mode)
            })
            .collect()
    }

    /// Output rectangles in the global logical space.
    pub fn output_rects(&self) -> Vec<Rectangle<i32, Logical>> {
        self.outputs
            .iter()
            .filter(|o| o.enabled)
            .map(|o| Rectangle::new(o.loc.into(), o.logical_size().into()))
            .collect()
    }

    /// Current pointer position (global logical space).
    pub fn pointer(&self) -> Point<f64, Logical> {
        self.pointer
    }

    /// Session pause/activate from libseat.
    pub fn on_session_event(&mut self, event: SessionEvent) {
        self.corner_pressure.reset();
        match event {
            SessionEvent::PauseSession => {
                eprintln!("tuna-compositor: drm: session paused");
                self.active = false;
                self.libinput.suspend();
                self.drm.pause();
            }
            SessionEvent::ActivateSession => {
                eprintln!("tuna-compositor: drm: session activated");
                if self.libinput.resume().is_err() {
                    eprintln!("tuna-compositor: drm: libinput resume failed");
                }
                // Display power saving (#355) keeps every CRTC off across
                // a VT switch: the next owner may have lit them.
                if let Err(e) = self.drm.activate(self.outputs_disabled) {
                    eprintln!("tuna-compositor: drm: activate failed: {e}");
                }
                for out in &mut self.outputs {
                    out.surface.reset_buffers();
                    out.pending = false;
                    out.last_frame = None;
                    out.damage_tracker = None;
                }
                self.active = true;
                if self.sleep_reset_pending {
                    self.resume_from_sleep();
                }
            }
        }
    }

    /// Recover scanout after real system sleep, independently of VT events.
    pub fn resume_from_sleep(&mut self) {
        self.wake_scanout_blocked = true;
        if !self.active {
            // Never program KMS off-seat; retain the wake until activation.
            self.sleep_reset_pending = true;
            return;
        }
        self.sleep_reset_pending = false;
        eprintln!("tuna-compositor: drm: system wake scanout reset");
        let trace = std::env::var_os("TUNA_LOCK_TRACE").is_some();
        self.wake_event_traces = if trace { 2 } else { 0 };
        if let Err(error) = self.retire_scanout(trace) {
            eprintln!("tuna-compositor: drm: wake {error}");
            // An unsuccessful reset cannot retire old kernel completions.
            // Stay masked without submitting a frame whose feedback could
            // be attached to an abandoned flip. VT activation can recover.
            self.sleep_reset_pending = true;
            return;
        }
        self.wake_scanout_blocked = false;
        if trace {
            for out in &self.outputs {
                eprintln!(
                    "tuna-compositor: drm: wake reset {} atomic={} commit_pending={} crtc={:?}",
                    out.name,
                    self.drm.is_atomic(),
                    out.surface.surface().commit_pending(),
                    self.drm.get_crtc(out.crtc),
                );
            }
        }
    }

    /// Drop pending and queued flips, turn every CRTC off and drain the
    /// completions the disable commit retired. The next frame commits the
    /// retained modes and surfaces again.
    fn retire_scanout(&mut self, trace: bool) -> Result<(), String> {
        // Pending and queued flips can be lost across S3. Drop both without
        // submitting an old queued scene or claiming presentation. Ordinary
        // frame_submitted() would submit queued_fb as a side effect.
        for out in &mut self.outputs {
            for mut feedback in out.surface.discard_pending_frames() {
                feedback.discarded();
            }
            out.surface.reset_buffers();
            out.pending = false;
            out.last_frame = None;
            out.damage_tracker = None;
            out.wake_trace = trace;
        }
        // Reset actual connector/plane state too: an active VT does not imply
        // that the kernel restored its framebuffer.
        self.drm
            .reset_state()
            .map_err(|error| format!("KMS reset failed: {error}"))?;
        // The blocking disable commit has retired previous scanout. Drain
        // its already queued completions before the first new submission.
        // A valid modeset event can report the last vblank before submission,
        // so its timestamp cannot identify an abandoned pre-sleep frame.
        let count = drain_reset_events(|| self.drm.receive_events().map(|events| events.count()))
            .map_err(|error| format!("event drain failed: {error}"))?;
        if trace {
            eprintln!("tuna-compositor: drm: wake obsolete events drained={count}");
        }
        Ok(())
    }

    /// DisplayConfig `PowerSaveMode` left on (#355): no frame is submitted
    /// until [`power_on`](Self::power_on). With `disable_outputs` every
    /// CRTC turns off, as Mutter's `meta_kms_device_disable`; off-seat the
    /// next activation does it.
    pub fn power_off(&mut self, disable_outputs: bool) {
        self.powered_off = true;
        if !disable_outputs || self.outputs_disabled {
            return;
        }
        self.outputs_disabled = true;
        if !self.active {
            return;
        }
        if let Err(error) = self.retire_scanout(false) {
            eprintln!("tuna-compositor: drm: power save {error}");
            // Same recovery as an unsuccessful wake reset: never attach new
            // feedback to a flip the disable may not have retired.
            self.wake_scanout_blocked = true;
            self.sleep_reset_pending = true;
        }
    }

    /// DisplayConfig `PowerSaveMode` back on (#355): every output repaints
    /// in full, and that frame modesets the CRTCs back on.
    pub fn power_on(&mut self) {
        self.powered_off = false;
        self.outputs_disabled = false;
        for out in &mut self.outputs {
            out.last_frame = None;
            out.damage_tracker = None;
        }
    }

    /// A failed wake reset/drain must not submit until recovery succeeds;
    /// display power saving submits nothing (#355).
    pub fn scanout_ready(&self) -> bool {
        self.active && !self.wake_scanout_blocked && !self.powered_off
    }

    /// Page flip completion: the output may render again, and what its
    /// frame drew has been presented.
    pub fn on_drm_event(
        &mut self,
        event: DrmEvent,
        metadata: &mut Option<DrmEventMetadata>,
    ) -> Option<PageFlip> {
        match event {
            DrmEvent::VBlank(crtc) => {
                if self.wake_event_traces != 0 {
                    self.wake_event_traces -= 1;
                    eprintln!(
                        "tuna-compositor: drm: wake pageflip crtc={crtc:?} metadata={metadata:?}",
                    );
                }
                let index = self.outputs.iter().position(|o| o.crtc == crtc)?;
                let out = &mut self.outputs[index];
                let feedback = match out.surface.frame_submitted() {
                    Ok(feedback) => feedback,
                    Err(e) => {
                        eprintln!("tuna-compositor: drm: frame_submitted: {e}");
                        None
                    }
                };
                out.pending = false;
                let (time, sequence) = match metadata.as_ref() {
                    Some(meta) => (
                        match meta.time {
                            DrmEventTime::Monotonic(time) => Some(time),
                            DrmEventTime::Realtime(_) => None,
                        },
                        u64::from(meta.sequence),
                    ),
                    None => (None, 0),
                };
                Some(PageFlip {
                    primary: index == 0,
                    feedback,
                    time,
                    sequence,
                    refresh: crate::frame_timing::refresh_of(&out.output),
                })
            }
            DrmEvent::Error(e) => {
                eprintln!("tuna-compositor: drm: device error: {e}");
                None
            }
        }
    }

    /// Translate one libinput event into manager inputs, handling the
    /// pieces only the hardware backend owns: relative pointer motion
    /// (accumulated, clamped) and Ctrl+Alt+F<n> VT switching (consumed).
    pub fn translate(
        &mut self,
        event: InputEvent<LibinputInputBackend>,
        layout: &[(Rectangle<i32, Logical>, bool)],
    ) -> Vec<ManagerInput> {
        self.note_touch_mode(&event);
        match event {
            InputEvent::PointerMotion { event } => {
                self.relative_motion_events = self.relative_motion_events.saturating_add(1);
                let delta = event.delta();
                let rects = self.output_rects();
                let (attempt, corner) = self.corner_pressure.motion(
                    self.pointer,
                    delta,
                    event.time() / 1000,
                    layout,
                    self.input_settings.right_to_left,
                    self.active && self.hot_corner_active && self.input_settings.hot_corners,
                );
                self.pointer = clamp_to_outputs(attempt, &rects);
                let mut inputs = vec![
                    ManagerInput::Motion {
                        pos: self.pointer,
                        time: (event.time() / 1000) as u32,
                    },
                    // Raw motion for relative-pointer clients (#89).
                    ManagerInput::RelativeMotion {
                        delta,
                        delta_unaccel: event.delta_unaccel(),
                        utime: event.time(),
                    },
                ];
                if let Some(pos) = corner {
                    inputs.push(ManagerInput::CornerPressure {
                        pos,
                        time: (event.time() / 1000) as u32,
                    });
                }
                inputs
            }
            InputEvent::DeviceAdded { device } => {
                // Mutter counts a two-finger touchpad before applying its settings.
                self.devices.push(device);
                let two_finger = self.two_finger_available();
                if let Some(device) = self.devices.last_mut() {
                    libinput::apply(&self.input_settings, device, two_finger);
                }
                self.recount_touch_devices();
                Vec::new()
            }
            InputEvent::DeviceRemoved { device } => {
                self.corner_pressure.reset();
                let had_two_finger = self.two_finger_available();
                self.devices.retain(|d| *d != device);
                // Mutter reapplies every setting once the last touchpad that
                // scrolls with two fingers leaves (edge scrolling may return).
                if had_two_finger && !self.two_finger_available() {
                    let settings = self.input_settings.clone();
                    self.apply_input_settings(&settings);
                }
                self.recount_touch_devices();
                Vec::new()
            }
            // Touchpad swipes (#60): three fingers are the shell's.
            InputEvent::GestureSwipeBegin { event } => {
                use smithay::backend::input::GestureBeginEvent;
                vec![ManagerInput::SwipeBegin {
                    fingers: event.fingers(),
                    time: (event.time() / 1000) as u32,
                }]
            }
            InputEvent::GestureSwipeUpdate { event } => {
                use smithay::backend::input::GestureSwipeUpdateEvent;
                vec![ManagerInput::SwipeUpdate {
                    delta: event.delta(),
                    time: (event.time() / 1000) as u32,
                }]
            }
            InputEvent::GestureSwipeEnd { event } => {
                use smithay::backend::input::GestureEndEvent;
                vec![ManagerInput::SwipeEnd {
                    cancelled: event.cancelled(),
                    time: (event.time() / 1000) as u32,
                }]
            }
            InputEvent::Keyboard { event } => {
                let code = u32::from(event.key_code()).saturating_sub(XKB_OFFSET);
                let pressed = event.state() == KeyState::Pressed;
                match code {
                    KEY_LEFTCTRL | KEY_RIGHTCTRL => self.ctrl = pressed,
                    KEY_LEFTALT | KEY_RIGHTALT => self.alt = pressed,
                    _ => {}
                }
                if pressed && self.ctrl && self.alt {
                    if let Some(vt) = vt_for_key(code) {
                        if let Err(e) = self.session.change_vt(vt) {
                            eprintln!("tuna-compositor: drm: change_vt({vt}): {e}");
                        }
                        return Vec::new();
                    }
                }
                crate::windows::translate_input::<LibinputInputBackend>(
                    InputEvent::Keyboard { event },
                    self.primary_size(),
                )
            }
            other => {
                let mut inputs = crate::windows::translate_input(other, self.primary_size());
                // Rotation (#345): absolute devices report panel-native
                // pixels; remap them into the rotated logical frame the
                // manager works in. Touch and absolute pointers arrive
                // as Motion here; relative motion has its own arm.
                if let Some(primary) = self.outputs.first() {
                    if primary.transform != 0 {
                        let (w, h) = (primary.size.w, primary.size.h);
                        let transform = primary.transform;
                        for input in &mut inputs {
                            if let ManagerInput::Motion { pos, .. } = input {
                                let (x, y) = crate::orientation::touch_point(
                                    w,
                                    h,
                                    transform,
                                    pos.x.round() as i32,
                                    pos.y.round() as i32,
                                );
                                pos.x = f64::from(x);
                                pos.y = f64::from(y);
                            }
                        }
                    }
                }
                for input in &inputs {
                    if let ManagerInput::Motion { pos, .. } = input {
                        self.corner_pressure.observe_position(*pos);
                        self.pointer = *pos;
                    }
                }
                inputs
            }
        }
    }

    /// GNOME's touchpad and mouse settings on every device, now and as
    /// devices arrive (#60).
    /// Count native relative delivery without recording movement or input text.
    pub fn relative_motion_events(&self) -> u64 {
        self.relative_motion_events
    }

    pub fn set_hot_corner_active(&mut self, active: bool) {
        self.hot_corner_active = active;
        if !active {
            self.corner_pressure.reset();
        }
    }

    pub fn apply_input_settings(&mut self, settings: &tuna_shell_control::InputSettings) {
        if self.input_settings.hot_corners != settings.hot_corners
            || self.input_settings.right_to_left != settings.right_to_left
        {
            self.corner_pressure.reset();
        }
        self.input_settings = settings.clone();
        let two_finger = self.two_finger_available();
        for device in &mut self.devices {
            libinput::apply(settings, device, two_finger);
        }
    }

    /// Whether any present touchpad scrolls with two fingers (Mutter's
    /// `two_finger_devices`), which makes edge scrolling yield.
    fn two_finger_available(&self) -> bool {
        self.devices.iter().any(libinput::has_two_finger_scroll)
    }

    /// The seat's touch-mode state (#349).
    pub fn touch_mode(&self) -> crate::osk::TouchMode {
        self.touch_mode
    }

    /// Follow mutter's tablet-mode switch and GNOME's
    /// `last-device-changed`, which skips keyboards.
    fn note_touch_mode(&mut self, event: &InputEvent<LibinputInputBackend>) {
        use smithay::backend::input::{Switch, SwitchState, SwitchToggleEvent};
        use smithay::reexports::input::DeviceCapability;
        let device = match event {
            InputEvent::SwitchToggle { event } => {
                if SwitchToggleEvent::switch(event) == Some(Switch::TabletMode) {
                    self.touch_mode.tablet_mode =
                        SwitchToggleEvent::state(event) == SwitchState::On;
                }
                None
            }
            InputEvent::PointerMotion { event } => Some(Event::device(event)),
            InputEvent::PointerMotionAbsolute { event } => Some(Event::device(event)),
            InputEvent::PointerButton { event } => Some(Event::device(event)),
            InputEvent::PointerAxis { event } => Some(Event::device(event)),
            InputEvent::TouchDown { event } => Some(Event::device(event)),
            InputEvent::TouchMotion { event } => Some(Event::device(event)),
            InputEvent::GestureSwipeBegin { event } => Some(Event::device(event)),
            InputEvent::GesturePinchBegin { event } => Some(Event::device(event)),
            InputEvent::GestureHoldBegin { event } => Some(Event::device(event)),
            InputEvent::TabletToolAxis { event } => Some(Event::device(event)),
            InputEvent::TabletToolTip { event } => Some(Event::device(event)),
            _ => None,
        };
        if let Some(device) = device {
            self.touch_mode.last_touchscreen = device.has_capability(DeviceCapability::Touch);
        }
    }

    /// Recount the device classes mutter's `update_touch_mode` reads.
    fn recount_touch_devices(&mut self) {
        use smithay::reexports::input::{
            event::switch::Switch as LibinputSwitch, DeviceCapability,
        };
        self.touch_mode.has_touchscreen = self
            .devices
            .iter()
            .any(|d| d.has_capability(DeviceCapability::Touch));
        self.touch_mode.has_pointer = self
            .devices
            .iter()
            .any(|d| d.has_capability(DeviceCapability::Pointer));
        self.touch_mode.has_tablet_switch = self.devices.iter().any(|d| {
            d.has_capability(DeviceCapability::Switch)
                && d.switch_has_switch(LibinputSwitch::TabletMode) == Ok(true)
        });
    }

    /// Live libinput state for every tracked device (#343), see
    /// [`libinput::device_state`].
    pub fn input_device_states(&self) -> Vec<serde_json::Value> {
        self.devices.iter().map(libinput::device_state).collect()
    }

    /// Move the drawn cursor to where the window manager put the pointer.
    pub fn set_pointer(&mut self, pos: Point<f64, Logical>) {
        if pos != self.pointer {
            self.corner_pressure.observe_position(pos);
        }
        self.pointer = pos;
    }

    fn primary_size(&self) -> Size<i32, Logical> {
        self.outputs
            .first()
            .map(|o| (o.size.w, o.size.h).into())
            .unwrap_or_else(|| (1, 1).into())
    }
}

/// Bound nonblocking 1024-byte DRM event reads after the synchronous reset.
/// No new frames are submitted until the old queue reaches WouldBlock.
fn drain_reset_events(
    mut receive: impl FnMut() -> std::io::Result<usize>,
) -> std::io::Result<usize> {
    let mut drained = 0;
    for _ in 0..32 {
        match receive() {
            Ok(0) => return Ok(drained),
            Ok(count) => drained += count,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(drained),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::other(
        "DRM event queue did not become empty after reset",
    ))
}

#[cfg(test)]
mod wake_event_tests {
    use super::drain_reset_events;
    use std::io::{Error, ErrorKind};

    #[test]
    fn old_completions_are_drained_before_new_submission() {
        let mut reads = [
            Ok(2),
            Err(Error::from(ErrorKind::Interrupted)),
            Ok(1),
            Err(Error::from(ErrorKind::WouldBlock)),
        ]
        .into_iter();
        assert_eq!(drain_reset_events(|| reads.next().unwrap()).unwrap(), 3);
        assert!(reads.next().is_none());
    }

    #[test]
    fn unbounded_or_failed_drain_never_qualifies_as_empty() {
        let mut calls = 0;
        assert!(drain_reset_events(|| {
            calls += 1;
            Ok(1)
        })
        .is_err());
        assert_eq!(calls, 32);
        assert_eq!(
            drain_reset_events(|| Err(Error::from(ErrorKind::PermissionDenied)))
                .unwrap_err()
                .kind(),
            ErrorKind::PermissionDenied
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn function_keys_map_to_vts() {
        assert_eq!(vt_for_key(KEY_F1), Some(1));
        assert_eq!(vt_for_key(KEY_F10), Some(10));
        assert_eq!(vt_for_key(KEY_F11), Some(11));
        assert_eq!(vt_for_key(KEY_F12), Some(12));
        assert_eq!(vt_for_key(30), None);
    }

    #[test]
    fn pointer_clamps_into_the_output_union() {
        let outputs = [
            Rectangle::new((0, 0).into(), (1920, 1080).into()),
            Rectangle::new((1920, 0).into(), (1280, 800).into()),
        ];
        let inside: Point<f64, Logical> = (2000.0, 100.0).into();
        assert_eq!(clamp_to_outputs(inside, &outputs), inside);
        // Below the shorter right-hand output: pulled up onto it.
        let below = clamp_to_outputs((2500.0, 1000.0).into(), &outputs);
        assert_eq!(below, (2500.0, 799.0).into());
        // Far left: onto the left edge of the first output.
        let left = clamp_to_outputs((-50.0, 10.0).into(), &outputs);
        assert_eq!(left, (0.0, 10.0).into());
    }
}
