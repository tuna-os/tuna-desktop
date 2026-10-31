//! DRM/KMS hardware session backend (#52).
//!
//! The nested winit backend draws into a window of a host session; this
//! backend owns the display hardware itself, so Tuna Desktop can start as a
//! display-manager (GDM) session or from a TTY:
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
use smithay::reexports::input::event::TabletToolEvent;
use smithay::reexports::input::Libinput;
use smithay::reexports::rustix::fs::OFlags;
use smithay::utils::{DeviceFd, Logical, Physical, Point, Rectangle, Size};

use crate::windows::ManagerInput;

pub mod color;
pub mod libinput;
pub mod libwacom;
pub mod tablet;

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
    /// GAMMA_LUT/CTM of the CRTC (P-DS-03).
    color: color::CrtcColor,
    /// The connector's EDID blob, read at discovery.
    edid: Option<Vec<u8>>,
    /// The white point programmed on the CRTC.
    white_point: crate::color_manager::WhitePoint,
    /// The CRTC takes no white point: the renderer tints instead.
    pub tint_fallback: bool,
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
    /// Drawing tablets and pads (P-IN-05).
    tablets: tablet::Tablets,
    /// Bumped by every applied layout, reverted mode or connector
    /// hotplug (#340).
    layout_generation: u64,
    /// What lighting a newly connected connector needs (hotplug).
    gbm: GbmDevice<DrmDeviceFd>,
    render_formats: smithay::backend::allocator::format::FormatSet,
    native_device: Option<u64>,
}

/// Connectors a hotplug rescan found connected or gone, by name.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ConnectorChanges {
    pub added: Vec<String>,
    pub removed: Vec<String>,
}

impl ConnectorChanges {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty()
    }
}

/// Event sources the runtime installs on its loop.
pub struct DrmSources {
    /// Session pause/activate.
    pub session: LibSeatSessionNotifier,
    /// VBlank and device errors.
    pub drm: DrmDeviceNotifier,
    /// Input events.
    pub input: LibinputInputBackend,
    /// udev's DRM device changes (connector hotplug); `None` when udev
    /// cannot be monitored.
    pub udev: Option<smithay::backend::udev::UdevBackend>,
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

/// The connected connectors of `outputs` with their modes.
fn startup_connectors(outputs: &[DrmOutput]) -> Vec<crate::display_config::StartupConnector> {
    outputs
        .iter()
        .map(|o| crate::display_config::StartupConnector {
            name: o.name.clone(),
            modes: o.modes.iter().map(|(spec, _)| *spec).collect(),
            preferred: o.preferred_mode,
        })
        .collect()
}

/// The scale of a monitor without a stored configuration: `TUNA_SCALE`,
/// else 1.
pub fn fallback_scale() -> f64 {
    std::env::var("TUNA_SCALE")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .map(crate::runtime::clamp_scale)
        .unwrap_or(1.0)
}

/// What lighting a connector needs from the device: at startup and on
/// hotplug.
struct Lighting<'a> {
    gbm: &'a GbmDevice<DrmDeviceFd>,
    render_formats: &'a smithay::backend::allocator::format::FormatSet,
    native_device: Option<u64>,
    resources: &'a control::ResourceHandles,
}

impl Lighting<'_> {
    /// Light one connected connector at its preferred mode on a CRTC
    /// not in `used`: Mutter's monitor modes, a GBM swapchain and the
    /// wl_output it will be advertised as. `None` when it cannot be lit
    /// (the reason is logged).
    fn open(
        &self,
        drm: &mut DrmDevice,
        handle: connector::Handle,
        info: &connector::Info,
        used: &[crtc::Handle],
    ) -> Option<DrmOutput> {
        // Mutter's monitor modes (#340): every connector mode,
        // deduplicated and sorted; the output starts in the preferred
        // one.
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
        let listed = crate::display_config::monitor_modes(&crtc_modes, kernel_preferred)?;
        let modes: Vec<(crate::display_config::ModeSpec, control::Mode)> = listed
            .modes
            .iter()
            .map(|&i| (crtc_modes[i].spec, info.modes()[i]))
            .collect();
        let preferred_mode = listed.preferred;
        let mode = modes[preferred_mode].1;
        let name = format!("{}-{}", info.interface().as_str(), info.interface_id());
        let Some(crtc) = info
            .encoders()
            .iter()
            .filter_map(|enc| drm.get_encoder(*enc).ok())
            .flat_map(|enc| self.resources.filter_crtcs(enc.possible_crtcs()))
            .find(|crtc| !used.contains(crtc))
        else {
            eprintln!("tuna-compositor: drm: skip connector {name}: no free CRTC");
            return None;
        };
        let surface = match drm.create_surface(crtc, mode, &[handle]) {
            Ok(surface) => surface,
            Err(e) => {
                eprintln!("tuna-compositor: drm: skip connector {name}: {e}");
                return None;
            }
        };
        let allocator = GbmAllocator::new(
            self.gbm.clone(),
            GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
        );
        let surface = match GbmBufferedSurface::new(
            surface,
            allocator,
            &[Fourcc::Argb8888, Fourcc::Xrgb8888],
            self.render_formats.iter().copied(),
        ) {
            Ok(surface) => surface,
            Err(e) => {
                eprintln!("tuna-compositor: drm: skip connector {name}: {e}");
                return None;
            }
        };
        // A ramp or matrix a previous owner left must not stay.
        let mut color = color::CrtcColor::new(drm, crtc);
        color.reset(drm);
        let edid = color::connector_edid(drm, handle);
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
        let native_owner = self.native_device.and_then(|device| {
            crate::native_output::resolve(
                std::path::Path::new("/sys"),
                device,
                &name,
                u32::from(handle),
            )
            .ok()
        });
        Some(DrmOutput {
            name,
            crtc,
            connector: handle,
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
            color,
            edid,
            white_point: Default::default(),
            tint_fallback: false,
        })
    }
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
        let mut outputs: Vec<DrmOutput> = Vec::new();
        for handle in resources.connectors() {
            let Ok(info) = drm.get_connector(*handle, false) else {
                continue;
            };
            if info.state() != connector::State::Connected {
                continue;
            }
            let used: Vec<crtc::Handle> = outputs.iter().map(|o| o.crtc).collect();
            let lighting = Lighting {
                gbm: &gbm,
                render_formats: &render_formats,
                native_device,
                resources: &resources,
            };
            if let Some(out) = lighting.open(&mut drm, *handle, &info, &used) {
                outputs.push(out);
            }
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
        let connectors = startup_connectors(&outputs);
        let names: Vec<String> = connectors.iter().map(|c| c.name.clone()).collect();
        let stored = crate::monitors::load(&names);
        let fallback_scale = fallback_scale();
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
            tablets: Default::default(),
            corner_pressure: Default::default(),
            hot_corner_active: true,
            relative_motion_events: 0,
            layout_generation: 0,
            gbm,
            render_formats,
            native_device,
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
        backend.log_outputs();
        let (first_w, first_h) = backend.outputs[0].logical_size();
        let first_loc = backend.outputs[0].loc;
        backend.pointer = (
            f64::from(first_loc.0) + f64::from(first_w) / 2.0,
            f64::from(first_loc.1) + f64::from(first_h) / 2.0,
        )
            .into();
        // Connector hotplug arrives as a udev change of the DRM device,
        // as Mutter's MetaUdev reports it (`hotplug` signal).
        let udev = match smithay::backend::udev::UdevBackend::new(&seat) {
            Ok(udev) => {
                let device = backend.drm.device_id();
                if !udev.device_list().any(|(id, _)| id == device) {
                    eprintln!(
                        "tuna-compositor: drm: {} is not a GPU of seat {seat}; connector hotplug is not followed",
                        path.display()
                    );
                }
                Some(udev)
            }
            Err(error) => {
                eprintln!("tuna-compositor: drm: udev monitor: {error}; connector hotplug is not followed");
                None
            }
        };
        Ok((
            backend,
            DrmSources {
                session: session_notifier,
                drm: drm_notifier,
                input,
                udev,
            },
        ))
    }

    /// The lit outputs in the log, as the DRM smoke proof reads them.
    pub fn log_outputs(&self) {
        for out in &self.outputs {
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
    }

    /// Whether a udev change concerns this backend's DRM device.
    pub fn is_device(&self, device_id: u64) -> bool {
        self.drm.device_id() == device_id
    }

    /// The connected connectors with their modes, for choosing a
    /// configuration.
    pub fn startup_connectors(&self) -> Vec<crate::display_config::StartupConnector> {
        startup_connectors(&self.outputs)
    }

    /// Connector hotplug (Mutter's `meta_kms_update_states_sync` after
    /// udev's change event): every connector is probed again; outputs
    /// whose connector went away are dropped (queued frames discarded,
    /// their CRTC switched off and free again) and newly connected
    /// connectors are lit at their preferred mode. The runtime then
    /// configures the new monitor set. Nothing is probed off-seat:
    /// activation rescans.
    pub fn rescan_connectors(&mut self) -> ConnectorChanges {
        let mut changes = ConnectorChanges::default();
        if !self.active || !self.drm.is_active() {
            return changes;
        }
        let resources = match self.drm.resource_handles() {
            Ok(resources) => resources,
            Err(error) => {
                eprintln!("tuna-compositor: drm: hotplug: resources: {error}");
                return changes;
            }
        };
        let mut index = 0;
        while index < self.outputs.len() {
            let connected = self
                .drm
                .get_connector(self.outputs[index].connector, true)
                .is_ok_and(|info| info.state() == connector::State::Connected);
            if connected {
                index += 1;
                continue;
            }
            let mut gone = self.outputs.remove(index);
            for mut feedback in gone.surface.discard_pending_frames() {
                feedback.discarded();
            }
            changes.removed.push(gone.name.clone());
        }
        for handle in resources.connectors() {
            if self.outputs.iter().any(|o| o.connector == *handle) {
                continue;
            }
            let Ok(info) = self.drm.get_connector(*handle, true) else {
                continue;
            };
            if info.state() != connector::State::Connected {
                continue;
            }
            let used: Vec<crtc::Handle> = self.outputs.iter().map(|o| o.crtc).collect();
            let lighting = Lighting {
                gbm: &self.gbm,
                render_formats: &self.render_formats,
                native_device: self.native_device,
                resources: &resources,
            };
            if let Some(out) = lighting.open(&mut self.drm, *handle, &info, &used) {
                changes.added.push(out.name.clone());
                self.outputs.push(out);
            }
        }
        if !changes.is_empty() {
            self.layout_generation += 1;
        }
        changes
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

    /// The monitors Mutter's colour manager registers with colord: every
    /// connector with its EDID identity and CRTC gamma size. The primary
    /// is the primary leader (`meta_monitor_is_primary`), the first lit
    /// leader as in [`Self::display_state`], not whichever output is
    /// first: a switched-off or mirroring output is never primary.
    pub fn colord_monitors(&self) -> Vec<crate::color_manager::ColordMonitor> {
        let atomic = self.drm.is_atomic();
        let primary = self
            .outputs
            .iter()
            .find(|o| o.is_leader())
            .map(|o| o.name.as_str());
        self.outputs
            .iter()
            .map(|out| crate::color_manager::ColordMonitor {
                identity: crate::edid::MonitorIdentity::new(
                    &out.name,
                    out.edid.as_deref(),
                    primary == Some(out.name.as_str()),
                    out.size_mm,
                    color::pnp_vendor_name,
                ),
                gamma_lut_size: out.color.caps(atomic).gamma_lut_size,
            })
            .collect()
    }

    /// Each connector's CRTC gamma size (calibration `GammaLutSize`).
    pub fn gamma_sizes(&self) -> Vec<(String, usize)> {
        let atomic = self.drm.is_atomic();
        self.outputs
            .iter()
            .map(|out| (out.name.clone(), out.color.caps(atomic).gamma_lut_size))
            .collect()
    }

    /// Program every lit output's white point (`update_white_point`):
    /// calibration session ramps, else the assigned profile's vcgt
    /// scaled by the Night Light `temperature`. Off-seat nothing is
    /// programmed; activation programs again. Returns whether any
    /// output fell back to the renderer tint.
    pub fn update_white_points(
        &mut self,
        share: &crate::color_manager::ColorShare,
        temperature: u32,
    ) -> bool {
        use crate::color_manager::WhitePointUpdate;
        if !self.active || !self.drm.is_active() {
            return self.outputs.iter().any(|o| o.tint_fallback);
        }
        let atomic = self.drm.is_atomic();
        for out in &mut self.outputs {
            // do_update: an inactive monitor is not updated.
            if !out.enabled {
                continue;
            }
            let device = share.device(&out.name);
            let session = share.calibration_for(&out.name);
            let update = out.white_point.update(
                out.color.caps(atomic),
                device.as_ref().is_some_and(|d| d.assigned.is_some()),
                device.as_ref().and_then(|d| d.calibration.as_deref()),
                session.as_ref().map(Option::as_ref),
                temperature,
            );
            out.tint_fallback = update == WhitePointUpdate::Unsupported;
            if let WhitePointUpdate::Program(programming) = update {
                if let Err(error) = out.color.program(&self.drm, &programming) {
                    eprintln!("tuna-compositor: drm: {} white point: {error}", out.name);
                    out.white_point.invalidate();
                }
            }
        }
        self.outputs.iter().any(|o| o.enabled && o.tint_fallback)
    }

    /// Whether `output` takes Night Light in the renderer (no CRTC
    /// white point).
    pub fn tints(&self, output: &str) -> bool {
        self.outputs
            .iter()
            .any(|o| o.name == output && o.tint_fallback)
    }

    /// The CRTC colour state as the kernel reports it, for the state
    /// file the DRM smoke proof reads (GAMMA_LUT readback).
    pub fn color_state(&self) -> Vec<serde_json::Value> {
        let atomic = self.drm.is_atomic();
        self.outputs
            .iter()
            .map(|out| {
                let caps = out.color.caps(atomic);
                let lut = out.color.read_gamma_lut(&self.drm);
                serde_json::json!({
                    "connector": out.name,
                    "gamma_lut_size": caps.gamma_lut_size,
                    "ctm": caps.ctm,
                    "edid_md5": out.edid.as_deref().map(crate::edid::md5_hex),
                    "tint_fallback": out.tint_fallback,
                    "gamma_lut": lut.map(|l| serde_json::json!({
                        "red": l.red, "green": l.green, "blue": l.blue,
                    })),
                })
            })
            .collect()
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
                    // Another VT owner may have changed the ramp
                    // (meta_onscreen_native_invalidate).
                    out.white_point.invalidate();
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
                if let Some(device) = self.devices.last() {
                    let monitors = self.tablet_monitors();
                    self.tablets.set_monitors(monitors);
                    self.tablets.device_added(device);
                }
                self.recount_touch_devices();
                Vec::new()
            }
            InputEvent::DeviceRemoved { device } => {
                self.corner_pressure.reset();
                let had_two_finger = self.two_finger_available();
                self.devices.retain(|d| *d != device);
                self.tablets.device_removed(&device);
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
            // Drawing tablets (P-IN-05): queued for the window manager.
            // Wrap the event, never `into_tablet_tool_event`: input 0.9
            // builds the enum from the raw pointer and then drops (and
            // destroys) the event it came from.
            InputEvent::TabletToolAxis { event } => {
                self.tablet_tool(TabletToolEvent::Axis(event));
                Vec::new()
            }
            InputEvent::TabletToolProximity { event } => {
                self.tablet_tool(TabletToolEvent::Proximity(event));
                Vec::new()
            }
            InputEvent::TabletToolTip { event } => {
                self.tablet_tool(TabletToolEvent::Tip(event));
                Vec::new()
            }
            InputEvent::TabletToolButton { event } => {
                self.tablet_tool(TabletToolEvent::Button(event));
                Vec::new()
            }
            InputEvent::Special(event) => {
                self.tablets.pad_event(event);
                Vec::new()
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

    /// The lit logical monitors as the tablet mapper matches them: the
    /// DisplayConfig identity (vendor, product, the connector as serial),
    /// physical size, desktop rectangle, transform and mode size.
    fn tablet_monitors(&self) -> Vec<crate::tablet::Monitor> {
        self.outputs
            .iter()
            .filter(|out| out.is_leader())
            .map(|out| {
                let properties = out.output.physical_properties();
                let (w, h) = out.logical_size();
                crate::tablet::Monitor {
                    connector: out.name.clone(),
                    vendor: properties.make,
                    product: properties.model,
                    serial: out.name.clone(),
                    width_mm: out.size_mm.0,
                    height_mm: out.size_mm.1,
                    rect: Rectangle::new(out.loc.into(), (w, h).into()),
                    transform: out.transform,
                    mode: (out.size.w, out.size.h),
                }
            })
            .collect()
    }

    fn tablet_tool(&mut self, event: smithay::reexports::input::event::TabletToolEvent) {
        let monitors = self.tablet_monitors();
        self.tablets.set_monitors(monitors);
        self.tablets.tool_event(event);
    }

    /// Tablet events the last libinput dispatch produced, in order.
    pub fn take_tablet_events(&mut self) -> Vec<crate::tablet::TabletEvent> {
        self.tablets.take_events()
    }

    /// The hardware tablets and pads (pad OSD requests and labels).
    pub fn tablets(&mut self) -> &mut tablet::Tablets {
        &mut self.tablets
    }

    /// The tablet and stylus settings paths to read.
    pub(crate) fn tablet_settings_wanted(
        &self,
    ) -> std::collections::BTreeSet<(crate::tablet_settings::Kind, String)> {
        self.tablets.wanted()
    }

    /// Apply a completed tablet settings read.
    pub(crate) fn set_tablet_settings(&mut self, snapshot: crate::tablet_settings::Snapshot) {
        let monitors = self.tablet_monitors();
        self.tablets.set_monitors(monitors);
        self.tablets.set_settings(snapshot);
    }

    /// Tablets and pads for the compositor state file.
    pub fn tablets_introspect(&self) -> serde_json::Value {
        self.tablets.introspect()
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
