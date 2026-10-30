//! Versioned shell control protocol (Wave 2 stream 1).
//!
//! Private local IPC between the compositor (authoritative) and the shell
//! host: explicit major/minor negotiation, full snapshots on (re)connect,
//! ordered incremental changes, request-ID-bearing commands, and
//! compositor-issued activation tokens.
//!
//! Wire format per ADR 0002: each frame is a u32 little-endian length prefix
//! followed by a postcard body. Frames larger than [`MAX_FRAME_BYTES`] are
//! rejected before any postcard decoding happens.

pub mod background;
pub mod motion;
pub use motion::{MotionLevel, MotionPolicy};

use serde::{Deserialize, Serialize};

/// Maximum accepted frame body size in bytes (1 MiB, per ADR 0002).
///
/// Enforced in [`decode_frame`] before postcard decoding, so a hostile or
/// buggy peer cannot force a large allocation.
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

/// Maximum window title length in bytes.
///
/// Titles are untrusted text (spec 001); over-long titles are rejected in
/// [`decode_frame`] with [`DecodeError::TitleTooLong`].
pub const MAX_TITLE_LEN: usize = 512;

pub mod layer_namespaces;
pub use layer_namespaces::{
    BANNER_NAMESPACE, DISPLAY_CHANGE_NAMESPACE, DOCK_NAMESPACE, END_SESSION_NAMESPACE,
    FOLDER_DIALOG_NAMESPACE, GTK_BANNERS_NAMESPACE, GTK_PANEL_NAMESPACE, IBUS_CANDIDATES_NAMESPACE,
    KBD_A11Y_NAMESPACE, NETWORK_AGENT_NAMESPACE, OSD_NAMESPACE, OSK_NAMESPACE, OVERVIEW_NAMESPACE,
    PAD_OSD_NAMESPACE, PANEL_NAMESPACE, POINTER_A11Y_NAMESPACE, POLKIT_NAMESPACE,
    PREVIEW_CHROME_NAMESPACE, SCREENSHOT_NAMESPACE, SHORTCUT_CONSENT_NAMESPACE, SWITCHER_NAMESPACE,
    SWITCHER_THUMBNAILS_NAMESPACE, WINDOW_MENU_NAMESPACE, WORKSPACE_POPUP_NAMESPACE,
};

/// Panel height in pixels (compositor ↔ shell contract).
///
/// The shell renders the panel at this height; the compositor uses this to inset
/// the work area and position the Activities trigger strip. Must stay in sync
/// across both implementations.
pub const PANEL_HEIGHT: u32 = 32;

/// Opaque window identifier minted by the compositor.
///
/// Never reused during a compositor session (spec 001); the shell must treat
/// it as an opaque key, never as a pointer, index, or capability.
pub type WindowId = u64;

/// Opaque workspace identifier minted by the compositor.
pub type WorkspaceId = u64;

/// Protocol version with explicit major/minor negotiation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolVersion {
    /// Incompatible change counter. Any mismatch is a hard error.
    pub major: u16,
    /// Backwards-compatible extension counter within a major version.
    pub minor: u16,
}

impl ProtocolVersion {
    /// Version spoken by this crate.
    ///
    /// `0.2` adds a per-window `activation_token` to `WindowInfo`. The
    /// postcard body is positional, so the field addition is
    /// wire-incompatible with `0.1` despite the minor bump; both sides
    /// always ship from this workspace, and a mixed pair fails closed at
    /// decode time.
    ///
    /// `0.3` appends the compositor-to-shell `Overview` message (002
    /// overview triggers). Enum variants serialize by index and the new
    /// variant sits last, so earlier discriminants are untouched.
    ///
    /// `0.4` appends the compositor-to-shell `Switcher` message (002
    /// Alt-Tab drive), again last for the same reason.
    ///
    /// `0.5` appends the shell-to-compositor `CloseWindow` command
    /// (dock quit), again last for the same reason.
    ///
    /// `0.6` appends `locked` to the compositor-to-shell `Snapshot` and
    /// the shell-to-compositor `Lock` command (session-lock). The struct
    /// field sits last and the enum variant sits last, so earlier
    /// positions are untouched; like `0.2`, the body is still
    /// wire-incompatible with older peers despite the minor bump, and
    /// both sides always ship from this workspace.
    ///
    /// `0.7` appends the compositor-to-shell `Outputs` message
    /// (multi-monitor inventory), again last for the same reason.
    ///
    /// `0.8` appends the compositor-to-shell `Environment` message
    /// (session variables such as `DISPLAY` once XWayland is up), again
    /// last for the same reason.
    ///
    /// `0.9` appends the shell-to-compositor `SetIdleTimeout` command
    /// (GNOME's idle and lock settings, #63), again last.
    ///
    /// `0.10` appends the shell-to-compositor `SetOverviewSearch`
    /// command (search hides the workspace view, as in GNOME), again last.
    ///
    /// `0.11` appends the shell-to-compositor `SetInputSettings` command
    /// (GNOME's keyboard, touchpad and mouse settings), again last.
    ///
    /// `0.12` appends the shell-to-compositor `SetOverviewAppGrid`
    /// command (the app grid shrinks the workspaces to thumbnails along
    /// the top, as in GNOME), again last.
    ///
    /// `0.13` appends the compositor-to-shell `OverviewPreviews` message
    /// (where each window preview sits, so the shell can draw GNOME's
    /// app icons, captions and close buttons on them), again last.
    ///
    /// `0.14` appends the shell-to-compositor `Unlock` command (the lock
    /// screen's password, verified by the compositor), again last.
    ///
    /// `0.15` appends the shell-to-compositor `SetAccelerators` command
    /// and the compositor-to-shell `AcceleratorActivated` message
    /// (org.gnome.Shell's GrabAccelerators for gnome-settings-daemon's
    /// media keys), again last.
    ///
    /// `0.16` appends the compositor-to-shell `WindowMenu` message and
    /// the shell-to-compositor `WindowAction` command (GNOME's window
    /// menu), again last.
    ///
    /// `0.17` appends the compositor-to-shell `WorkspacePopup` message
    /// (GNOME's workspace switcher popup), again last.
    ///
    /// `0.18` appends `Maximize`, `ToggleTiledLeft` and `ToggleTiledRight`
    /// to `WindowAction` and the shell-to-compositor `SwitchInputSource`
    /// command (GNOME's rebindable window-manager keys), again last.
    ///
    /// `0.19` appends `StepWindow` and `Key` to `SwitcherAction` and the
    /// shell-to-compositor `SetSwitcherThumbnails` command (GNOME's
    /// window thumbnails in the Alt+Tab switcher), again last.
    ///
    /// `0.20` appends the shell-to-compositor `SetSwitcherKeys` command
    /// (GNOME's rebindable switcher keys), again last.
    ///
    /// `0.21` appends `enable_animations` to `InputSettings`. Like earlier
    /// positional struct extensions, the postcard body changes and both
    /// sides ship together from this workspace.
    ///
    /// `0.22` appends shortcut inhibition consent requests and responses.
    ///
    /// `0.23` appends window/cycle switcher actions and pointer output.
    ///
    /// `0.24` appends window-backed icon metadata to WindowInfo.
    /// Postcard positional structs require peers to upgrade together.
    ///
    /// `0.25` appends mouse/touchpad handedness to InputSettings.
    /// Both peers ship together because the postcard body is positional.
    /// `0.26` appends the shell text direction to InputSettings.
    /// Both positional postcard peers ship together.
    /// `0.27` appends hardware Screen Reader preference/status messages.
    ///
    /// `0.28` replaces `InputSettings::enable_animations` with `motion`,
    /// GNOME's three-state motion policy plus its slow-down factor.
    /// The positional postcard body changes, so both peers ship together.
    /// `0.29` appends private screenshot selection pointer movement.
    /// `0.30` appends live sound/bell and attached-modal brightness preferences.
    /// `0.31` appends workspace and edge-tiling preferences.
    /// `0.32` appends trusted dash icon targets for minimize/restore.
    /// `0.33` appends cursor theme, size, locate-pointer and its keysym preference.
    /// `0.34` appends keyboard accessibility aids and toggle notifications.
    /// `0.35` appends atomic wellbeing scene effects.
    /// `0.36` appends GNOME's pointer accessibility aids to
    /// `InputSettings`, the shell-to-compositor `SetDwellClickType`
    /// command and the compositor-to-shell `DwellClickType` and
    /// `PointerTimeout` messages (hover click's chooser and GNOME's pie
    /// timer). Positional: both peers ship together.
    /// `0.37` appends NativeOutputInventory without altering OutputInfo.
    /// `0.38` appends typed accelerator grabs
    /// (`SetAcceleratorGrabs`) and `AcceleratorDeactivated`.
    /// `0.39` appends the mouse acceleration profile to InputSettings.
    /// Positional postcard peers must ship together.
    /// `0.40` appends GNOME's per-device-class touchpad, mouse, trackball
    /// and pointing-stick settings to InputSettings (send-events, click
    /// method, scroll methods, tap options, middle-click emulation,
    /// scroll-wheel emulation and per-class acceleration).
    /// `MouseAccelProfile` becomes `PointerAccelProfile`; positional
    /// postcard peers must ship together.
    /// `0.41` appends mutter's `center-new-windows` to InputSettings;
    /// positional postcard peers must ship together.
    /// `0.42` appends GNOME's display-change confirmation: compositor-to-shell
    /// `ConfirmDisplayChange` and shell-to-compositor `CompleteDisplayChange`.
    /// `0.43` appends GNOME's on-screen keyboard (#349): the
    /// shell-to-compositor `OskInput` command (typing into the focused
    /// client) and the compositor-to-shell `OnScreenKeyboard` message
    /// (input-method focus, the input-panel request and touch-mode
    /// auto-show); positional postcard peers must ship together.
    /// `0.44` appends the lock screen's authentication conversation (GDM's
    /// UserVerifier shape): shell-to-compositor `BeginVerification`,
    /// `AnswerQuery` and `CancelVerification`, and compositor-to-shell
    /// `AuthEvent`. Positional postcard peers must ship together.
    /// `0.45` appends drawing-tablet shell UI (P-IN-05): compositor-to-shell
    /// `ShowOsd`, `ShowPadOsd`, `PadOsdLabels`, `PadOsdInput`, `ClosePadOsd`
    /// and shell-to-compositor `RequestPadOsd`, `PadOsdClosed`.
    /// Positional postcard peers must ship together.
    pub const CURRENT: Self = Self {
        major: 0,
        minor: 45,
    };

    /// Build a version explicitly (handy for `Hello` probes in tests).
    pub const fn new(major: u16, minor: u16) -> Self {
        Self { major, minor }
    }

    /// Compatibility of a peer version against ours: same major line and a
    /// minor no newer than ours. A newer minor means the peer speaks
    /// messages we may not understand; a different major is always an
    /// error (acceptance criterion A6).
    pub fn is_compatible_with(&self, ours: &ProtocolVersion) -> bool {
        self.major == ours.major && self.minor <= ours.minor
    }
}

/// Version spoken by this crate.
pub const CURRENT_VERSION: ProtocolVersion = ProtocolVersion::CURRENT;

/// Window state owned by the compositor and mirrored to the shell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowInfo {
    /// Opaque compositor-minted id.
    pub id: WindowId,
    /// Untrusted client-supplied text, capped at [`MAX_TITLE_LEN`] bytes.
    pub title: String,
    /// Client-supplied application id, if known.
    pub app_id: Option<String>,
    /// Workspace currently containing the window.
    pub workspace: WorkspaceId,
    /// Whether the window currently holds keyboard focus.
    pub focused: bool,
    /// Fresh one-use activation Bearer [REDACTED] for this window, minted with the
    /// snapshot or delta that carries it. The shell presents it back in
    /// `ActivateWindow`; presenting it consumes it. Several live tokens
    /// for one window may coexist (each snapshot/delta mints anew); each
    /// allows exactly one activation and expires after 30 s. Never logged
    /// (redacted `Debug` on [`ActivationToken`]).
    pub activation_token: String,
    /// Compositor-owned PNG cache path or sanitized theme icon name.
    pub icon: Option<String>,
    /// Sticky windows and windows outside the primary monitor's workspace set.
    #[serde(default)]
    pub on_all_workspaces: bool,
}

/// Workspace state owned by the compositor and mirrored to the shell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    /// Opaque compositor-minted id.
    pub id: WorkspaceId,
    /// Human-readable name, if the compositor assigns one.
    pub name: Option<String>,
    /// Whether this workspace is currently active.
    pub active: bool,
}

/// One compositor-tracked output: name, size, and dock anchor. The
/// inventory crosses to the shell as these records; window migration
/// stays compositor-internal, so no other wire types change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputInfo {
    /// Output name, e.g. `tuna-0`.
    pub name: String,
    /// Output width in physical pixels.
    pub width: i32,
    /// Output height in physical pixels.
    pub height: i32,
    /// Whether the dock anchors here.
    pub primary: bool,
}

/// Actual selected native DRM output authority. Missing inventory means no
/// authority, including nested sessions; connector text alone is insufficient.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeOutputInfo {
    pub name: String,
    /// Actual char-device rdev from the compositor-owned KMS FD.
    pub drm_device: u64,
    /// Actual DRM connector object ID, checked against kernel sysfs.
    pub connector_id: u32,
    /// Canonical sysfs DRM connector directory under that owned device.
    pub connector_sysfs: String,
    pub connector_device: u64,
    pub connector_inode: u64,
}

/// One ordered state mutation between two snapshot revisions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StateOp {
    /// A window appeared (or its full state is restated).
    WindowOpened(WindowInfo),
    /// A window is gone; its id may never be reused this session.
    WindowClosed {
        /// Id of the removed window.
        id: WindowId,
    },
    /// Keyboard focus moved to this window.
    WindowFocused {
        /// Id of the newly focused window.
        id: WindowId,
    },
    /// A window moved to another workspace.
    WindowMoved {
        /// Id of the moved window.
        id: WindowId,
        /// Destination workspace.
        workspace: WorkspaceId,
    },
    /// A workspace was added, removed, renamed, or (de)activated.
    WorkspaceChanged(WorkspaceInfo),
}

/// Shell-to-compositor command payload. Every command carries the shell's
/// request id (`Message::Command.id`); the compositor answers with
/// `Message::CommandResult` bearing the same id. Commands never block
/// compositor input/frame paths (spec R6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommandKind {
    /// Focus and raise a window. Requires a compositor-issued activation
    /// token: a raw Wayland serial is not sufficient (spec 001, ADR 0002).
    ActivateWindow {
        /// Window to activate.
        window: WindowId,
        /// Compositor-minted Bearer [REDACTED] proving a recent user interaction.
        token: ActivationToken,
    },
    /// Make a workspace active.
    FocusWorkspace {
        /// Workspace to activate.
        workspace: WorkspaceId,
    },
    /// Toggle the shell overview open or closed.
    ToggleOverview,
    /// Ask a window's client to close (polite close request; the
    /// client unmaps itself and the compositor drops it on reconcile).
    /// No activation token: closing is not focus, and unknown ids are
    /// per-request denials like every other command.
    CloseWindow {
        /// Window to close.
        window: WindowId,
    },
    /// Lock the session immediately (manual lock from the shell).
    /// Idempotent and tokenless like `ToggleOverview`: the shell is a
    /// trusted local peer, and locking hides rather than reveals.
    /// Unlocking is never granted on request: see `Unlock`, which
    /// carries a password the compositor verifies through session auth.
    Lock,
    /// Lock after this much idle time; `0` never locks on idle (#63).
    /// The shell derives it from GNOME's `idle-delay`, `lock-enabled`
    /// and `lock-delay` keys and resends it on every change. Tokenless:
    /// it only changes when the session locks itself, never unlocks.
    SetIdleTimeout {
        /// Idle milliseconds before locking, `0` for never.
        ms: u64,
    },
    /// The overview's search is showing results (or not): while it is,
    /// the compositor hides the workspace card and window previews, as
    /// GNOME does. UI state; it resets whenever the overview closes.
    SetOverviewSearch {
        /// Whether search results are showing.
        active: bool,
    },
    /// GNOME's input settings, as the shell reads them from GSettings
    /// (#60): keymap, key repeat, pointer devices, hot corner. Sent at
    /// start and on every change; the compositor applies them live.
    SetInputSettings(Box<InputSettings>),
    /// The overview shows the app grid (or not): while it does, the
    /// compositor draws the workspaces as thumbnails along the top, as
    /// GNOME's app grid state does. UI state; it resets whenever the
    /// overview closes.
    SetOverviewAppGrid {
        /// Whether the app grid is showing.
        active: bool,
    },
    /// The lock screen's password: the compositor verifies it through
    /// session auth (PAM, or greetd's daemon) off its event loop and
    /// unlocks only on success. The result comes back as this command's
    /// `CommandResult` (`Applied` unlocked, `Denied` wrong password).
    Unlock {
        /// The typed password; never logged (`Debug` is redacted).
        password: Secret,
    },
    /// Every key combination grabbed through org.gnome.Shell's
    /// GrabAccelerators, the full set each time it changes. The
    /// compositor keeps matching presses from clients and reports them
    /// with `AcceleratorActivated`.
    SetAccelerators {
        /// The grabs, at most [`MAX_ACCELERATORS`].
        accelerators: Vec<Accelerator>,
    },
    /// One of GNOME's window-menu actions on a window. Unknown windows
    /// are denied, like every per-window command.
    WindowAction {
        /// The window.
        window: WindowId,
        /// What to do.
        action: WindowAction,
    },
    /// Switch to the next (or previous) keyboard input source (GNOME's
    /// `switch-input-source` keys). Tokenless: it only changes the
    /// keymap.
    SwitchInputSource {
        /// The previous source instead of the next.
        backward: bool,
    },
    /// Where the open switcher shows window thumbnails (GNOME's
    /// ThumbnailSwitcher): the compositor draws each window, fitted,
    /// into its frame above everything. Empty when none show. At most
    /// [`MAX_SWITCHER_THUMBNAILS`].
    SetSwitcherThumbnails {
        /// The frames, in order.
        thumbnails: Vec<SwitcherThumbnail>,
    },
    /// The switcher's keys (GNOME's `switch-applications`,
    /// `switch-group` and their backward keys), the full set each time
    /// they change. Once sent they replace the compositor's built-in
    /// Alt/Super+Tab and Above_Tab; an empty list unbinds the switcher.
    /// At most [`MAX_SWITCHER_KEYS`].
    SetSwitcherKeys {
        /// Every bound chord.
        keys: Vec<SwitcherKey>,
    },
    /// Trusted shell response to a compositor-issued consent request.
    ShortcutConsent {
        /// The pending request, never a window id.
        request: u64,
        /// Explicit Allow (or a remembered grant), otherwise Deny.
        allow: bool,
    },
    /// Preference from the authenticated supervised shell. Nested runtimes
    /// ignore it; the hardware compositor owns the actual Orca child.
    SetScreenReader { enabled: bool },
    /// GNOME screenshot keyboard selection cursor feedback. Coordinates are
    /// local to the mapped exclusive screenshot overlay in logical pixels,
    /// never global input.
    MoveScreenshotPointer { x: i32, y: i32 },
    /// Trusted shell dash icon bounds in global logical coordinates. Full replacement.
    SetWindowIconGeometries { geometries: Vec<WindowIconGeometry> },
    /// Atomic scene effects from the authenticated shell; cursor excluded.
    SetWellbeingEffects { grayscale: bool, fade: bool },
    /// The hover-click chooser picked the click the next dwell makes.
    SetDwellClickType { click: DwellClick },
    /// Since 0.38: atomic full accelerator replacement with GNOME grab
    /// flags, at most [`MAX_ACCELERATORS`]; supersedes `SetAccelerators`.
    SetAcceleratorGrabs { grabs: Vec<AcceleratorGrab> },
    /// Since 0.42: the display-change dialog's answer (GNOME's
    /// `complete_display_change`): Keep Changes = true, Revert Settings = false.
    CompleteDisplayChange { keep: bool },
    /// Since 0.43: GNOME's on-screen keyboard typing into the client with
    /// keyboard focus (`KeyboardController`, #349). Denied while the
    /// session is locked: the lock screen's keyboard edits only its own
    /// prompt and never reaches the compositor.
    OskInput { input: OskInput },
    /// Since 0.44: start one lock-screen authentication conversation for the
    /// session's own user (GDM's `BeginVerificationForUser`; the user is
    /// never taken from the shell). Its prompts and messages arrive as
    /// [`Message::AuthEvent`]s carrying this command's id. Exactly one
    /// `CommandResult` ends it: `Applied` once this service verified the
    /// user and the session is unlocked, `Denied` when the conversation
    /// stopped without unlocking (failure, cancellation, timeout, an
    /// unavailable service, or the other service winning).
    BeginVerification { service: AuthService },
    /// Since 0.44: the answer to the outstanding [`AuthEvent::Query`] with
    /// this `sequence` (GDM's `AnswerQuery`). Never replayed; `Debug` is
    /// redacted. `Denied` when no such query is outstanding.
    AnswerQuery {
        service: AuthService,
        sequence: u32,
        answer: Secret,
    },
    /// Since 0.44: stop every conversation this connection started (GDM's
    /// `Cancel`); each pending `BeginVerification` is then `Denied`.
    CancelVerification,
    /// Since 0.45: show the pad OSD (`meta_display_request_pad_osd`), from
    /// `org.gnome.Shell.Wacom.PadOsd.Show` or the OSD's pad chooser. Denied
    /// when no such pad exists, it has no libwacom layout, or an OSD is
    /// already shown.
    RequestPadOsd {
        target: PadOsdTarget,
        edition_mode: bool,
    },
    /// Since 0.45: the shown pad OSD closed; the compositor stops routing
    /// pad events to the shell.
    PadOsdClosed { pad: u32 },
}

/// One on-screen keyboard action (gnome-shell 51 `KeyboardController`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OskInput {
    /// `commit(str, modifiers)`: a key's string. With input-method focus
    /// and no latched modifiers the compositor commits it to the focused
    /// text input; otherwise it presses and releases each character's
    /// keysym from the active layout, holding the latched modifier
    /// keysyms around them (X11 clients, terminals, shortcuts). At most
    /// [`MAX_OSK_TEXT`] bytes and [`MAX_OSK_MODIFIERS`] modifiers.
    Commit { text: String, modifiers: Vec<u32> },
    /// `keyvalPress` / `keyvalRelease`: one keysym (Return, BackSpace)
    /// pressed or released through the active layout.
    Keyval { keysym: u32, pressed: bool },
}

/// Bytes one [`OskInput::Commit`] may carry (an emoji ZWJ sequence fits).
pub const MAX_OSK_TEXT: usize = 256;

/// Modifier keysyms one [`OskInput::Commit`] may hold.
pub const MAX_OSK_MODIFIERS: usize = 8;

/// The text field holding input-method focus (mutter's
/// `ClutterInputFocus`), with its text-input-v3 content type: raw
/// `zwp_text_input_v3` purpose and hint values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextFieldFocus {
    pub purpose: u32,
    pub hints: u32,
}

/// Since 0.44: a lock-screen authentication service, GDM's conversation
/// services (`gdm-password`, `gdm-fingerprint`) as GNOME Shell 51's
/// `authServicesLegacy.js` runs them side by side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AuthService {
    /// The PAM conversation of the lock service: password, then whatever
    /// else the installed stack asks (a code, a PIN).
    Password,
    /// fprintd verification of an enrolled print.
    Fingerprint,
}

/// Longest PAM prompt, message or answer text on the wire.
pub const MAX_AUTH_TEXT_LEN: usize = 4096;

/// Since 0.44: one step of an authentication conversation (GDM's
/// UserVerifier signals).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthEvent {
    /// An informational message (`PAM_TEXT_INFO`, GDM `Info`).
    Info { text: String },
    /// An error message (`PAM_ERROR_MSG`, or the verifier's own failure
    /// text; GDM `Problem`).
    Problem { text: String },
    /// A question to answer with [`CommandKind::AnswerQuery`]: hidden input
    /// when `secret` (`PAM_PROMPT_ECHO_OFF`, GDM `SecretInfoQuery`), else
    /// visible (`PAM_PROMPT_ECHO_ON`, GDM `InfoQuery`).
    Query {
        sequence: u32,
        question: String,
        secret: bool,
    },
    /// The fingerprint reader is waiting for a finger: `swipe` when it is a
    /// swipe sensor (fprintd's `scan-type`).
    FingerprintScan { swipe: bool },
    /// This service cannot run for this user (GDM `ServiceUnavailable`):
    /// no reader, no enrolled print, or no conversation backend.
    Unavailable,
}

/// Most buttons, rings, strips or grouped pads one pad OSD message carries.
pub const MAX_PAD_FEATURES: usize = 64;

/// Which pad a [`CommandKind::RequestPadOsd`] names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PadOsdTarget {
    /// The pad's evdev node (`/dev/input/eventN`), as GNOME Settings passes it.
    DeviceNode(String),
    /// A compositor pad id from [`PadOsd::group_pads`].
    Pad(u32),
}

/// One pad in a pad OSD's device group (gnome-shell's PadChooser).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PadOsdPad {
    pub pad: u32,
    pub name: String,
}

/// One pad button as the OSD shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PadOsdButton {
    /// `meta_display_get_pad_button_label`; `None` shows GNOME's "None".
    pub label: Option<String>,
    /// The mode group this button switches, if it is a mode-switch button
    /// (not editable; its release refreshes the labels).
    pub mode_switch_group: Option<u32>,
}

/// Every label of one pad (`meta_display_get_pad_feature_label` for
/// rings and strips, in the current mode).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PadOsdLabels {
    /// Indexed by button number.
    pub buttons: Vec<PadOsdButton>,
    /// Indexed by ring: `[cw, ccw]`.
    pub rings: Vec<[Option<String>; 2]>,
    /// Indexed by strip: `[up, down]`.
    pub strips: Vec<[Option<String>; 2]>,
    /// Current mode of each ring (index = ring), for edition settings paths.
    pub ring_modes: Vec<u32>,
    /// Current mode of each strip (index = strip).
    pub strip_modes: Vec<u32>,
}

/// gnome-shell's `show-pad-osd` arguments (`windowManager.js` `_showPadOsd`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PadOsd {
    /// Compositor pad id, echoed in [`CommandKind::PadOsdClosed`].
    pub pad: u32,
    /// `clutter_input_device_get_device_name`: the OSD title.
    pub name: String,
    /// The libwacom layout SVG (`libwacom_get_layout_filename`).
    pub layout_path: String,
    /// The pad's `org.gnome.desktop.peripherals.tablet` path; edition
    /// writes `org.gnome.desktop.peripherals.tablet.pad-button` below it
    /// (`buttonA/`, `ringA-ccw-mode-0/`, `stripA-up-mode-0/`).
    pub settings_path: String,
    pub left_handed: bool,
    pub edition_mode: bool,
    /// The connector of the tablet's monitor, else the current one.
    pub connector: Option<String>,
    /// Pads in this pad's device group, itself included.
    pub group_pads: Vec<PadOsdPad>,
    pub labels: PadOsdLabels,
}

/// A pad event the shown pad OSD captures (`vfunc_captured_event`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PadOsdEvent {
    Button { button: u32, pressed: bool },
    Ring { ring: u32, mode: u32 },
    Strip { strip: u32, mode: u32 },
}

/// A running window's dash icon target; empty replacement selects monitor fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowIconGeometry {
    pub window: u64,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}
pub const MAX_WINDOW_ICON_GEOMETRIES: usize = 256;

/// Screen Reader process lifecycle, not proof of spoken usability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScreenReaderState {
    Disabled,
    Starting,
    Active,
    Unavailable,
    Conflict,
}

/// Switcher thumbnails held at once.
pub const MAX_SWITCHER_THUMBNAILS: usize = 64;

/// Switcher keys held at once.
pub const MAX_SWITCHER_KEYS: usize = 64;

/// Keysyms carried by [`SwitcherAction::Key`] and consumed by both shell
/// switcher frontends. These are XKB keysyms, not evdev keycodes.
///
/// Preserve the numeric values and the legacy shell-host public re-export.
pub mod switcher_keys {
    pub const LEFT: u32 = 0xff51;
    pub const UP: u32 = 0xff52;
    pub const RIGHT: u32 = 0xff53;
    pub const DOWN: u32 = 0xff54;
    pub const F4: u32 = 0xffc1;
    pub const Q: u32 = 0x71;
    pub const Q_UPPER: u32 = 0x51;
    pub const W: u32 = 0x77;
    pub const W_UPPER: u32 = 0x57;
}

/// Mutter's `Above_Tab` in [`SwitcherKey::keysym`]: the key above Tab,
/// whatever it types in the active layout.
pub const KEYSYM_ABOVE_TAB: u32 = 0x2000_0000;

/// One switcher chord.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwitcherKey {
    /// The key's base-level xkb keysym (lower case for letters), or
    /// [`KEYSYM_ABOVE_TAB`].
    pub keysym: u32,
    /// Exact modifiers: a combination of the `MOD_*` bits. Released,
    /// all but Shift commit the switcher.
    pub mods: u32,
    pub kind: SwitcherKeyKind,
}

/// What a switcher chord does (GNOME's keybinding names).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SwitcherKeyKind {
    /// `switch-applications`: the next app.
    Applications,
    /// `switch-applications-backward`.
    ApplicationsBackward,
    /// `switch-group`: the selected app's next window.
    Group,
    /// `switch-group-backward`.
    GroupBackward,
    /// WindowSwitcherPopup: one item for each window.
    Windows,
    WindowsBackward,
    /// Immediate cycling, without showing a popup.
    CycleWindows,
    CycleWindowsBackward,
    CycleGroup,
    CycleGroupBackward,
}

/// One switcher thumbnail frame, in global logical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwitcherThumbnail {
    /// The window drawn in it.
    pub window: WindowId,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// GNOME's window-menu actions (windowMenu.js) the compositor carries out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WindowAction {
    /// Hide (minimize).
    Minimize,
    /// Maximize, or Restore when maximized.
    ToggleMaximize,
    /// Move with the pointer until a click.
    Move,
    /// Resize from the bottom-right corner with the pointer until a click.
    Resize,
    /// Move to the workspace on the left.
    MoveToWorkspaceLeft,
    /// Move to the workspace on the right.
    MoveToWorkspaceRight,
    /// Always on Top, on or off.
    ToggleAbove,
    /// Always on Visible Workspace (on every workspace), on or off.
    ToggleSticky,
    /// Open the window menu at the window's corner (Alt+Space).
    ShowMenu,
    /// Leave maximized (Alt+F5).
    Unmaximize,
    /// Move to this workspace (GNOME's move-to-workspace keys).
    MoveToWorkspace {
        /// The workspace.
        workspace: u32,
    },
    /// Maximize (GNOME's `maximize` key, Super+Up).
    Maximize,
    /// Tile the left half, or untile (Mutter's `toggle-tiled-left`).
    ToggleTiledLeft,
    /// Tile the right half, or untile (`toggle-tiled-right`).
    ToggleTiledRight,
}

/// Grabbed accelerators held at once.
pub const MAX_ACCELERATORS: usize = 512;

/// Modifier bits in [`Accelerator::mods`] (X11's masks).
pub const MOD_SHIFT: u32 = 1;
pub const MOD_CTRL: u32 = 4;
pub const MOD_ALT: u32 = 8;
pub const MOD_LOGO: u32 = 64;

/// GNOME Shell's action modes (Shell.ActionMode) in [`Accelerator::modes`]:
/// where a grab applies.
pub const MODE_NORMAL: u32 = 1;
pub const MODE_OVERVIEW: u32 = 2;
pub const MODE_LOCK_SCREEN: u32 = 4;
pub const MODE_UNLOCK_SCREEN: u32 = 8;
/// GNOME Shell 51 ActionMode.POPUP, used by its screenshot grab.
pub const MODE_POPUP: u32 = 1 << 7;

/// One grabbed key combination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Accelerator {
    /// The shell's action id, echoed in `AcceleratorActivated`.
    pub action: u32,
    /// The xkb keysym (lower case for letters).
    pub keysym: u32,
    /// Exact modifiers: a combination of the `MOD_*` bits.
    pub mods: u32,
    /// Action modes it applies in: a combination of the `MODE_*` bits.
    pub modes: u32,
}

/// GNOME MetaKeyBindingFlags supported for external accelerator grabs:
/// duplicate key-downs of a held grab do not activate again.
pub const GRAB_IGNORE_AUTOREPEAT: u32 = 1 << 4;
/// GNOME MetaKeyBindingFlags: the release of the grabbed press is
/// reported as `AcceleratorDeactivated`.
pub const GRAB_TRIGGER_RELEASE: u32 = 1 << 7;
/// Every flag [`AcceleratorGrab::flags`] may carry.
pub const SUPPORTED_GRAB_FLAGS: u32 = GRAB_IGNORE_AUTOREPEAT | GRAB_TRIGGER_RELEASE;

/// One grab with its GNOME flags. A separate type, so the legacy
/// [`Accelerator`] wire body stays unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceleratorGrab {
    pub accelerator: Accelerator,
    /// A combination of [`SUPPORTED_GRAB_FLAGS`].
    pub flags: u32,
}

/// A secret on the wire (the lock screen's password). `Debug` never
/// shows it, so a logged message cannot leak it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Secret(pub String);

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// Outcome of one shell command, matched by request id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommandStatus {
    /// The compositor applied the command.
    Applied,
    /// The compositor refused the command; `reason` is diagnostics only.
    Denied {
        /// Human-readable denial reason (never sensitive content).
        reason: String,
    },
}

/// Machine-readable control-plane error category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorKind {
    /// A frame failed length, encoding, or validation checks.
    MalformedFrame,
    /// Peer speaks an incompatible major version.
    IncompatibleVersion,
    /// A frame exceeded [`MAX_FRAME_BYTES`].
    OversizeFrame,
    /// The activation token was unknown, expired, or already used.
    StaleToken,
    /// `Changes.from_revision` does not match the shell's revision; the
    /// shell must discard state and await a fresh `Snapshot`.
    RevisionGap,
    /// The command id/kind was not understood.
    UnknownCommand,
}

/// Top-level protocol message. This enum is the postcard body of one frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Message {
    /// Version handshake; the first message in both directions.
    Hello {
        /// Peer's protocol version.
        version: ProtocolVersion,
    },
    /// Complete authoritative state. Always sent on (re)connect and after
    /// any revision gap; the shell renders from this, never from
    /// pre-crash assumptions.
    Snapshot {
        /// Monotonic revision of this state.
        revision: u64,
        /// All mapped windows.
        windows: Vec<WindowInfo>,
        /// All workspaces.
        workspaces: Vec<WorkspaceInfo>,
        /// Whether the session is locked. While set, `windows` carries
        /// no content (empty): the lock screen shows no titles, and a
        /// restartable shell must not retain any. The flag is the only
        /// lock state on the wire — never credentials.
        locked: bool,
    },
    /// Ordered incremental updates from `from_revision` to `to_revision`.
    /// A gap against the shell's revision forces a resnapshot.
    Changes {
        /// Revision the shell must currently hold.
        from_revision: u64,
        /// Revision the shell holds after applying `ops` in order.
        to_revision: u64,
        /// Mutations to apply in order.
        ops: Vec<StateOp>,
    },
    /// Shell request; answered by `CommandResult` with the same `id`.
    Command {
        /// Shell-chosen request id, echoed in the result.
        id: u64,
        /// Command payload.
        kind: CommandKind,
    },
    /// Compositor answer to one `Command`, matched by `id`.
    CommandResult {
        /// Echo of `Command.id`.
        id: u64,
        /// Outcome.
        status: CommandStatus,
    },
    /// Typed observable error (ADR 0002); the offending frame is dropped.
    Error {
        /// Category.
        kind: ErrorKind,
        /// Diagnostics; never sensitive application content.
        message: String,
    },
    /// Compositor-to-shell overview intent (002 R1). The compositor
    /// detects Super, Activities-strip, and hot-corner triggers — the
    /// shell never sees global input — and broadcasts the resulting
    /// open state; the shell renders from its model and answers with
    /// intent commands. Carries no revision: it is UI state, not model
    /// truth, and never replaces snapshot resync.
    Overview {
        /// Whether the overview should be open.
        open: bool,
    },
    /// Compositor-to-shell Alt-Tab switcher drive (002 workspaces).
    /// The compositor owns global key state (Alt held, Tab taps,
    /// Alt release, Escape) and the shell owns MRU order and
    /// rendering: each Tab tap broadcasts one `Step`, Alt release
    /// broadcasts `Commit`, Escape broadcasts `Cancel`. The shell
    /// answers a commit with `ActivateWindow` for its selected id.
    /// Carries no revision: UI state, like `Overview`.
    Switcher {
        /// What the switcher should do.
        action: SwitcherAction,
    },
    /// Compositor-to-shell output inventory (multi-monitor). The
    /// compositor broadcasts the full list whenever it changes and
    /// right after each handshake snapshot; the shell reconciles its
    /// per-output surfaces against it. Carries no revision: structural
    /// state, like `Overview` — and it sits last so earlier variant
    /// positions are untouched.
    Outputs {
        /// Every tracked output, primary first.
        outputs: Vec<OutputInfo>,
    },
    /// Compositor-to-shell session environment: variables apps launched
    /// from the shell must inherit, such as `DISPLAY` once XWayland's
    /// window manager is up (#59). The full set each time; sent after
    /// every handshake and whenever it changes. Sits last so earlier
    /// variant positions are untouched.
    Environment {
        /// `(name, value)` pairs, sorted by name.
        vars: Vec<(String, String)>,
    },
    /// Compositor-to-shell overview previews: where each window preview
    /// on the active workspace sits and which one the pointer is over,
    /// so the shell draws GNOME's preview chrome (app icon always;
    /// caption and close button on hover). Empty when the overview is
    /// closed. Sent whenever it changes. Sits last.
    OverviewPreviews {
        /// Previews in drawing order, bottom first.
        previews: Vec<PreviewInfo>,
        /// The preview under the pointer, if any.
        hovered: Option<u64>,
    },
    /// Compositor-to-shell: a grabbed accelerator was pressed (the
    /// press and its release never reached a client). Sits last.
    AcceleratorActivated {
        /// The grab's action id.
        action: u32,
        /// The key event's time, in milliseconds.
        time: u32,
        /// The action mode it fired in (one `MODE_*` bit).
        mode: u32,
    },
    /// Compositor-to-shell: a client asked for its window menu (a header
    /// bar right click); the shell draws GNOME's menu at `x`, `y`
    /// (logical pixels of the output). Sits last.
    WindowMenu {
        /// The window.
        window: WindowId,
        /// Where, in logical output pixels.
        x: i32,
        y: i32,
        /// Whether it is maximized (Restore rather than Maximize).
        maximized: bool,
        /// Whether it is kept above other windows (Always on Top).
        above: bool,
        /// Whether it shows on every workspace (Always on Visible
        /// Workspace).
        sticky: bool,
        /// Whether there is a workspace to its left, and to its right.
        workspace_left: bool,
        workspace_right: bool,
    },
    /// Compositor-to-shell: a workspace key switched workspaces outside
    /// the overview; the shell shows GNOME's switcher popup. Sits last.
    WorkspacePopup {
        /// The active workspace's position.
        index: u32,
        /// How many workspaces there are (GNOME's dynamic count).
        count: u32,
    },
    /// Show consent for this inhibitor; `None` dismisses a stale dialog.
    ShortcutConsent {
        /// Request id and stable application id (empty for unknown apps).
        request: Option<(u64, String)>,
    },
    /// The connector under the pointer, used by per-monitor brightness keys.
    PointerOutput { name: Option<String> },
    /// Actual compositor-owned Screen Reader child status.
    ScreenReader { state: ScreenReaderState },
    /// A keyboard shortcut (Shift five times, Shift held eight seconds,
    /// two modifiers at once) switched an aid; the shell saves the
    /// setting and confirms it as GNOME does.
    KeyboardAidToggled { aid: KeyboardAid, enabled: bool },
    /// The click the next dwell makes, whenever it changes (a one-shot
    /// type returns to a single click once used).
    DwellClickType { click: DwellClick },
    /// A pointer accessibility timeout started (`duration_ms` set) at
    /// the pointer, or stopped (`clicked` when it ran out and acted).
    /// The shell draws GNOME's pie timer around the pointer.
    PointerTimeout {
        kind: PointerTimeoutKind,
        duration_ms: Option<u32>,
        clicked: bool,
        x: i32,
        y: i32,
    },
    /// Full actual native authority; empty explicitly revokes prior authority.
    /// Append-only, sent only to peers advertising minor >=37.
    NativeOutputInventory { outputs: Vec<NativeOutputInfo> },
    /// Since 0.38: release of an originally admitted press of a grab
    /// carrying [`GRAB_TRIGGER_RELEASE`] (GNOME's AcceleratorDeactivated).
    AcceleratorDeactivated { action: u32, time: u32, mode: u32 },
    /// Since 0.42: show GNOME's "Keep these display settings?" dialog with
    /// this countdown; a newer one replaces an open dialog and restarts it.
    /// Sent only to peers advertising minor >=41.
    ConfirmDisplayChange { timeout_secs: u32 },
    /// Since 0.43: what GNOME's on-screen keyboard follows (#349).
    /// `focus` is the enabled text input of the client holding keyboard
    /// focus (none while locked); `show` marks an update carrying the
    /// input-panel ON request (a focused text input enabled again, GTK's
    /// click into its field, as mutter treats text-input-v3 version 1);
    /// `touch_auto` is `KeyboardManager`'s auto-enable: seat touch-mode
    /// with a touchscreen as the last pointing device. Sent on change and
    /// after each handshake snapshot, only to peers advertising minor >= 43.
    OnScreenKeyboard {
        focus: Option<TextFieldFocus>,
        show: bool,
        touch_auto: bool,
    },
    /// Since 0.44: a step of the conversation the shell started with the
    /// `BeginVerification` command `request`. Sent only to peers advertising
    /// minor >=44.
    AuthEvent {
        request: u64,
        service: AuthService,
        event: AuthEvent,
    },
    /// Since 0.45: `meta_display_show_osd`, an icon and label OSD on a
    /// monitor (the tablet mapping notification and pad mode switches).
    /// `connector` `None` is the current monitor. Sent only to peers
    /// advertising minor >=45.
    ShowOsd {
        connector: Option<String>,
        icon: String,
        label: String,
    },
    /// Since 0.45: show gnome-shell's pad OSD (`show-pad-osd`).
    ShowPadOsd(Box<PadOsd>),
    /// Since 0.45: the shown pad's labels changed (mode switch, settings).
    PadOsdLabels { pad: u32, labels: PadOsdLabels },
    /// Since 0.45: a pad event while the OSD is shown, from `pad` (the
    /// shown pad or another pad of its group).
    PadOsdInput { pad: u32, event: PadOsdEvent },
    /// Since 0.45: the shown pad was unplugged; close its OSD.
    ClosePadOsd { pad: u32 },
}

/// The click a hover (dwell) makes: GNOME's dwell click types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum DwellClick {
    #[default]
    Primary,
    Double,
    /// First dwell presses, the next releases.
    Drag,
    Secondary,
}

/// Which pointer accessibility timeout runs (Clutter's timeout types).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PointerTimeoutKind {
    /// Simulated secondary click: a held primary button.
    SecondaryClick,
    /// Hover click waiting to click.
    Dwell,
    /// Hover click waiting for a direction gesture.
    Gesture,
}

/// A hover-click gesture direction (`GDesktopMouseDwellDirection`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DwellDirection {
    Left,
    Right,
    Up,
    Down,
}

/// The keyboard aids GNOME lets the keyboard itself switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeyboardAid {
    /// `stickykeys-enable`.
    StickyKeys,
    /// `slowkeys-enable`.
    SlowKeys,
}

/// GNOME's dynamic workspace count: one empty workspace always follows
/// the last occupied one, and the active one counts too.
pub fn dynamic_workspace_count(max_occupied: Option<u32>, active: u32) -> u32 {
    max_occupied.map_or(1, |m| m + 2).max(active + 1)
}

/// One window preview in the overview, in output logical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewInfo {
    /// The window's model id.
    pub window: u64,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// GNOME sound and window-manager bell preferences, consumed by the compositor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BellPolicy {
    pub audible: bool,
    pub visual: bool,
    pub frame_flash: bool,
    pub event_sounds: bool,
    pub theme: String,
}
impl Default for BellPolicy {
    fn default() -> Self {
        Self {
            audible: true,
            visual: false,
            frame_flash: false,
            event_sounds: true,
            theme: "freedesktop".into(),
        }
    }
}

/// GNOME Shell's workspace and edge-tiling preferences.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspacePolicy {
    pub dynamic: bool,
    pub fixed_count: u32,
    pub only_on_primary: bool,
    pub edge_tiling: bool,
}

impl Default for WorkspacePolicy {
    fn default() -> Self {
        Self {
            dynamic: true,
            fixed_count: 4,
            only_on_primary: true,
            edge_tiling: true,
        }
    }
}

/// GNOME input and bell preferences, flattened for the compositor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputSettings {
    /// xkb layouts, comma separated, in input-source order (`us,de`).
    pub xkb_layout: String,
    /// xkb variants, one per layout (`,nodeadkeys`).
    pub xkb_variant: String,
    /// xkb options, comma separated (`compose:ralt`).
    pub xkb_options: String,
    /// Key repeat on, its delay and interval in milliseconds.
    pub repeat: bool,
    pub repeat_delay_ms: u32,
    pub repeat_interval_ms: u32,
    /// Touchpad.
    pub tap_to_click: bool,
    pub touchpad_natural_scroll: bool,
    /// Pointer speed in thousandths, -1000..=1000 (GNOME's -1..1).
    pub touchpad_speed_milli: i32,
    pub disable_while_typing: bool,
    /// Mouse.
    pub mouse_natural_scroll: bool,
    pub mouse_speed_milli: i32,
    /// `org.gnome.desktop.interface enable-hot-corners`.
    pub hot_corners: bool,
    /// GNOME's motion policy: `enable-animations`, `reduced-motion` and
    /// the slow-down factor, derived by the shell (default full motion).
    #[serde(default)]
    pub motion: MotionPolicy,
    /// GNOME mouse primary button and resolved touchpad orientation.
    #[serde(default)]
    pub mouse_left_handed: bool,
    #[serde(default)]
    pub touchpad_left_handed: bool,
    /// GTK shell default text direction; corners and Activities follow it.
    #[serde(default)]
    pub right_to_left: bool,
    #[serde(default)]
    pub bell: BellPolicy,
    /// Enable the attached-modal parent brightness effect (Mutter preference).
    #[serde(default = "default_attach_modal_dialogs")]
    pub attach_modal_dialogs: bool,
    #[serde(default)]
    pub workspace_policy: WorkspacePolicy,
    /// `orientation-lock` from `org.gnome.settings-daemon.peripherals.touchscreen`
    /// (#345): the shell reads the key, the compositor stops sensor-driven
    /// rotation while set. Appended last: the postcard body is positional.
    /// UNION: this field addition needs the Wave2 minor version bump.
    #[serde(default)]
    pub orientation_lock: bool,
    /// Cursor preferences appended in 0.33; both peers ship together.
    #[serde(default)]
    pub cursor_theme: String,
    #[serde(default)]
    pub cursor_size: u32,
    #[serde(default)]
    pub locate_pointer: bool,
    #[serde(default = "default_locate_pointer_key")]
    pub locate_pointer_key: String,
    /// GNOME keyboard accessibility input aids.
    #[serde(default)]
    pub keyboard_aids: KeyboardAids,
    /// GNOME's pointer accessibility aids.
    #[serde(default)]
    pub pointer_aids: PointerAids,
    /// GNOME mouse acceleration profile; trackballs and pointing sticks have
    /// separate settings and must not inherit this value.
    #[serde(default)]
    pub mouse_accel_profile: PointerAccelProfile,
    /// `org.gnome.desktop.peripherals.touchpad` beyond the keys above.
    #[serde(default)]
    pub touchpad: TouchpadSettings,
    /// `org.gnome.desktop.peripherals.mouse` beyond the keys above.
    #[serde(default)]
    pub mouse: MouseSettings,
    /// `org.gnome.desktop.peripherals.trackball`.
    #[serde(default)]
    pub trackball: TrackballSettings,
    /// `org.gnome.desktop.peripherals.pointingstick`.
    #[serde(default)]
    pub pointing_stick: PointingStickSettings,
    /// `org.gnome.mutter center-new-windows` (0.41): centered automatic
    /// placement, else mutter's first-fit from the work area's corner.
    #[serde(default = "default_center_new_windows")]
    pub center_new_windows: bool,
}

/// GNOME's `GDesktopPointerAccelProfile`, shared by the mouse, touchpad,
/// trackball and pointing-stick schemas.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PointerAccelProfile {
    #[default]
    Default,
    Flat,
    Adaptive,
    /// Custom curves are not supported by the current libinput bindings.
    Custom,
}

impl PointerAccelProfile {
    /// Decode the GSettings enum nick, falling back to the device default.
    pub fn from_gsettings(value: &str) -> Self {
        match value {
            "flat" => Self::Flat,
            "adaptive" => Self::Adaptive,
            "custom" => Self::Custom,
            _ => Self::Default,
        }
    }
}

/// GNOME's `GDesktopDeviceSendEvents` (touchpad `send-events`).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SendEvents {
    #[default]
    Enabled,
    Disabled,
    /// libinput suspends the touchpad while an external mouse is plugged.
    DisabledOnExternalMouse,
}

impl SendEvents {
    /// Decode the GSettings enum nick; unknown nicks keep GNOME's default.
    pub fn from_gsettings(value: &str) -> Self {
        match value {
            "disabled" => Self::Disabled,
            "disabled-on-external-mouse" => Self::DisabledOnExternalMouse,
            _ => Self::Enabled,
        }
    }
}

/// GNOME's `GDesktopTouchpadClickMethod` (touchpad `click-method`).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TouchpadClickMethod {
    /// The device's own default method.
    Default,
    None,
    /// Software button areas.
    Areas,
    /// Clickfinger: the finger count picks the button (GNOME's default).
    #[default]
    Fingers,
}

impl TouchpadClickMethod {
    /// Decode the GSettings enum nick; unknown nicks keep GNOME's default.
    pub fn from_gsettings(value: &str) -> Self {
        match value {
            "default" => Self::Default,
            "none" => Self::None,
            "areas" => Self::Areas,
            _ => Self::Fingers,
        }
    }
}

/// GNOME's `GDesktopTouchpadTapButtonMap` (touchpad `tap-button-map`).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TapButtonMap {
    #[default]
    Default,
    /// One, two, three fingers: left, right, middle.
    Lrm,
    /// One, two, three fingers: left, middle, right.
    Lmr,
}

impl TapButtonMap {
    /// Decode the GSettings enum nick; unknown nicks keep GNOME's default.
    pub fn from_gsettings(value: &str) -> Self {
        match value {
            "lrm" => Self::Lrm,
            "lmr" => Self::Lmr,
            _ => Self::Default,
        }
    }
}

/// GNOME's `GDesktopPointingStickScrollMethod` (pointing-stick
/// `scroll-method`).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PointingStickScrollMethod {
    #[default]
    Default,
    None,
    OnButtonDown,
}

impl PointingStickScrollMethod {
    /// Decode the GSettings enum nick; unknown nicks keep GNOME's default.
    pub fn from_gsettings(value: &str) -> Self {
        match value {
            "none" => Self::None,
            "on-button-down" => Self::OnButtonDown,
            _ => Self::Default,
        }
    }
}

/// GNOME's `scroll-wheel-emulation-button` and `-lock` (mouse and
/// trackball schemas). `button` is GNOME's button number, 0 for off.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScrollWheelEmulation {
    pub button: u32,
    pub lock: bool,
}

/// GNOME 51 `org.gnome.desktop.peripherals.touchpad` keys not carried by
/// the flat touchpad fields of [`InputSettings`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TouchpadSettings {
    pub send_events: SendEvents,
    pub click_method: TouchpadClickMethod,
    pub edge_scrolling: bool,
    pub two_finger_scrolling: bool,
    pub tap_and_drag: bool,
    pub tap_and_drag_lock: bool,
    pub tap_button_map: TapButtonMap,
    pub middle_click_emulation: bool,
    pub accel_profile: PointerAccelProfile,
}

impl Default for TouchpadSettings {
    /// GNOME 51's defaults.
    fn default() -> Self {
        Self {
            send_events: SendEvents::Enabled,
            click_method: TouchpadClickMethod::Fingers,
            edge_scrolling: false,
            two_finger_scrolling: true,
            tap_and_drag: true,
            tap_and_drag_lock: false,
            tap_button_map: TapButtonMap::Default,
            middle_click_emulation: false,
            accel_profile: PointerAccelProfile::Default,
        }
    }
}

/// GNOME 51 `org.gnome.desktop.peripherals.mouse` keys not carried by the
/// flat mouse fields of [`InputSettings`]. Defaults are GNOME 51's.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MouseSettings {
    pub middle_click_emulation: bool,
    pub scroll_wheel_emulation: ScrollWheelEmulation,
}

/// GNOME 51 `org.gnome.desktop.peripherals.trackball`. Defaults are GNOME
/// 51's.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackballSettings {
    pub accel_profile: PointerAccelProfile,
    pub middle_click_emulation: bool,
    pub scroll_wheel_emulation: ScrollWheelEmulation,
}

/// GNOME 51 `org.gnome.desktop.peripherals.pointingstick`. Defaults are
/// GNOME 51's.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PointingStickSettings {
    /// Pointer speed in thousandths, -1000..=1000 (GNOME's -1..1).
    pub speed_milli: i32,
    pub accel_profile: PointerAccelProfile,
    pub scroll_method: PointingStickScrollMethod,
}

/// GNOME's `org.gnome.desktop.a11y.mouse`, flattened for the compositor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PointerAids {
    /// `secondary-click-enabled` and `secondary-click-time` (ms).
    pub secondary_click: bool,
    pub secondary_click_ms: u32,
    /// `dwell-click-enabled`, `dwell-time` (ms) and `dwell-threshold`
    /// (pixels).
    pub dwell_click: bool,
    pub dwell_ms: u32,
    pub dwell_threshold: u32,
    /// `dwell-mode`: a direction gesture picks the click, instead of the
    /// panel chooser (`window`).
    pub dwell_gesture: bool,
    /// `dwell-gesture-single`, `-double`, `-drag` and `-secondary`
    /// (`None` for GNOME's `none`).
    pub gesture_single: Option<DwellDirection>,
    pub gesture_double: Option<DwellDirection>,
    pub gesture_drag: Option<DwellDirection>,
    pub gesture_secondary: Option<DwellDirection>,
}

impl Default for PointerAids {
    /// GNOME 51's defaults.
    fn default() -> Self {
        Self {
            secondary_click: false,
            secondary_click_ms: 1200,
            dwell_click: false,
            dwell_ms: 1200,
            dwell_threshold: 10,
            dwell_gesture: false,
            gesture_single: Some(DwellDirection::Left),
            gesture_double: Some(DwellDirection::Up),
            gesture_drag: Some(DwellDirection::Down),
            gesture_secondary: Some(DwellDirection::Right),
        }
    }
}

/// GNOME's `org.gnome.desktop.a11y.keyboard`, flattened for the
/// compositor's input pipeline. Delays are milliseconds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyboardAids {
    /// `enable`: the keyboard may switch Sticky and Slow Keys.
    pub shortcuts: bool,
    /// `feature-state-change-beep`.
    pub feature_beep: bool,
    /// `stickykeys-enable`, `stickykeys-two-key-off`, `stickykeys-modifier-beep`.
    pub sticky: bool,
    pub sticky_two_key_off: bool,
    pub sticky_beep: bool,
    /// `slowkeys-enable`, `slowkeys-delay` and its three beeps.
    pub slow: bool,
    pub slow_delay_ms: u32,
    pub slow_beep_press: bool,
    pub slow_beep_accept: bool,
    pub slow_beep_reject: bool,
    /// `bouncekeys-enable`, `bouncekeys-delay`, `bouncekeys-beep-reject`.
    pub bounce: bool,
    pub bounce_delay_ms: u32,
    pub bounce_beep_reject: bool,
    /// `togglekeys-enable`.
    pub toggle_beep: bool,
    /// `mousekeys-enable`, `mousekeys-max-speed` (pixels per second),
    /// `mousekeys-accel-time` and `mousekeys-init-delay`.
    pub mouse: bool,
    pub mouse_max_speed: u32,
    pub mouse_accel_ms: u32,
    pub mouse_init_delay_ms: u32,
}

impl Default for KeyboardAids {
    /// GNOME 51's defaults: every aid off.
    fn default() -> Self {
        Self {
            shortcuts: false,
            feature_beep: false,
            sticky: false,
            sticky_two_key_off: false,
            sticky_beep: false,
            slow: false,
            slow_delay_ms: 300,
            slow_beep_press: false,
            slow_beep_accept: false,
            slow_beep_reject: false,
            bounce: false,
            bounce_delay_ms: 300,
            bounce_beep_reject: false,
            toggle_beep: false,
            mouse: false,
            mouse_max_speed: 10,
            mouse_accel_ms: 300,
            mouse_init_delay_ms: 300,
        }
    }
}

fn default_locate_pointer_key() -> String {
    "Control_L".into()
}

fn default_attach_modal_dialogs() -> bool {
    true
}

/// Mutter 51's `center-new-windows` default.
fn default_center_new_windows() -> bool {
    true
}

impl Default for InputSettings {
    /// GNOME 51's defaults.
    fn default() -> Self {
        Self {
            xkb_layout: "us".into(),
            xkb_variant: String::new(),
            xkb_options: String::new(),
            repeat: true,
            repeat_delay_ms: 500,
            repeat_interval_ms: 30,
            // GNOME 51.0 enables touchpad tap by default
            // (org.gnome.desktop.peripherals.touchpad tap-to-click).
            tap_to_click: true,
            touchpad_natural_scroll: true,
            touchpad_speed_milli: 0,
            disable_while_typing: true,
            mouse_natural_scroll: false,
            mouse_speed_milli: 0,
            hot_corners: true,
            motion: MotionPolicy::default(),
            mouse_left_handed: false,
            touchpad_left_handed: false,
            right_to_left: false,
            bell: BellPolicy::default(),
            attach_modal_dialogs: true,
            workspace_policy: WorkspacePolicy::default(),
            // GNOME default: convertibles auto-rotate out of the box.
            orientation_lock: false,
            keyboard_aids: KeyboardAids::default(),
            cursor_theme: "Adwaita".into(),
            cursor_size: 24,
            locate_pointer: false,
            locate_pointer_key: default_locate_pointer_key(),
            pointer_aids: PointerAids::default(),
            mouse_accel_profile: PointerAccelProfile::Default,
            touchpad: TouchpadSettings::default(),
            mouse: MouseSettings::default(),
            trackball: TrackballSettings::default(),
            pointing_stick: PointingStickSettings::default(),
            center_new_windows: true,
        }
    }
}

/// One Alt-Tab switcher drive event (compositor to shell).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SwitcherAction {
    /// Advance the switcher selection (`forward`: Tab vs Shift+Tab).
    /// Opens the switcher when closed.
    Step {
        /// True for Tab, false for Shift+Tab.
        forward: bool,
    },
    /// Activate the current selection; closes the switcher.
    Commit,
    /// Close the switcher without activating.
    Cancel,
    /// GNOME's `switch-group` (Alt+Above_Tab): step through the selected
    /// app's windows, showing their thumbnails. Opens the switcher when
    /// closed.
    StepWindow {
        /// True for Above_Tab, false with Shift.
        forward: bool,
    },
    /// Another key pressed while the switcher is open (its keysym): the
    /// switcher holds the keyboard, as GNOME's does (arrows, Q, W, F4).
    Key {
        /// The xkb keysym.
        keysym: u32,
    },
    /// Window switcher: every window is a separate entry.
    StepAllWindows { forward: bool },
    /// Cycle immediately without a popup, optionally within the focused app.
    Cycle { forward: bool, group: bool },
}

/// Unified error type for the control protocol, spoken by both compositor
/// and shell-host endpoints.
///
/// This type spans wire-level failures (Io, Decode, Unexpected), semantic
/// protocol violations (Remote, WouldBlock), and application-level failures
/// (NoActiveWindow). Both peers use this type so errors are consistently
/// named across the channel boundary and callers can handle either side's
/// failures uniformly.
///
/// Nonblocking contract (spec R6): `WouldBlock` surfaces immediately when
/// a socket is not ready; on write backpressure, the connection should be
/// dropped rather than retried (a partial frame may remain buffered).
/// Not `Clone`/`PartialEq`: the `Io` variant carries [`std::io::Error`],
/// which implements neither. Match on `Io(e) if e.kind() == ...` instead of
/// comparing errors for equality.
#[derive(Debug)]
pub enum ControlError {
    /// Underlying socket I/O failure.
    Io(std::io::Error),
    /// Frame failed schema validation (oversize, malformed, stale major,
    /// over-long title). The offending frame is dropped.
    Decode(DecodeError),
    /// Nonblocking socket not ready (spec R6). On a write that returns
    /// this, the connection holds a partial frame and must be dropped.
    WouldBlock,
    /// Peer closed the connection or sent a message the current step
    /// cannot use (unexpected or out-of-order).
    Unexpected(String),
    /// The peer (compositor when called from shell-host) answered with a
    /// typed [`Message::Error`] where a response was required.
    Remote {
        /// Machine-readable category.
        kind: ErrorKind,
        /// Compositor diagnostics (never sensitive content).
        message: String,
    },
    /// Application-level constraint violated: e.g., a shell-host command
    /// like `send_activation` needs a selected window but none exists.
    AppConstraint(String),
}

impl std::fmt::Display for ControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "control socket I/O: {e}"),
            Self::Decode(e) => write!(f, "control decode: {e}"),
            Self::WouldBlock => write!(f, "control socket not ready"),
            Self::Unexpected(e) => write!(f, "unexpected message or state: {e}"),
            Self::Remote { kind, message } => {
                write!(f, "remote error ({kind:?}): {message}")
            }
            Self::AppConstraint(e) => write!(f, "constraint: {e}"),
        }
    }
}

impl std::error::Error for ControlError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Decode(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for ControlError {
    fn from(e: std::io::Error) -> Self {
        if e.kind() == std::io::ErrorKind::WouldBlock {
            Self::WouldBlock
        } else {
            Self::Io(e)
        }
    }
}

impl From<DecodeError> for ControlError {
    fn from(e: DecodeError) -> Self {
        Self::Decode(e)
    }
}

/// Compositor-minted activation Bearer [REDACTED] authorizing one privileged window action.
///
/// Opaque to the shell: minting, expiry (30 s), one-use removal, seat
/// binding, and `app_id` matching are compositor policy (ADR 0002), not
/// protocol behavior. `Debug` is redacted so tokens never land in logs
/// (spec R7).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivationToken(pub String);

impl ActivationToken {
    /// Wrap a compositor-minted token string.
    pub fn new(token: String) -> Self {
        Self(token)
    }
}

impl std::fmt::Debug for ActivationToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ActivationToken([redacted])")
    }
}

/// Compositor-side bookkeeping attached to a minted token (ADR 0002).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenMeta {
    /// Why the token was minted (e.g. `"activate-window"`).
    pub purpose: String,
    /// Expected client `app_id`; activation is denied on mismatch.
    pub app_id: Option<String>,
    /// Mint time in milliseconds since the Unix epoch; expiry is 30 s.
    pub issued_at_ms: u64,
}

/// Framing/decoding failure. Every variant is observable and typed so both
/// sides can log, count, and drop the offending frame (ADR 0002).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// A typed collection exceeds its admission bound.
    CollectionTooLong { len: usize, max: usize },
    /// Fewer than 4 prefix bytes, or a body shorter than the prefix claims.
    Truncated {
        /// Bytes required.
        expected: usize,
        /// Bytes present.
        actual: usize,
    },
    /// Declared body length exceeds [`MAX_FRAME_BYTES`]. Checked before any
    /// postcard decoding, so the declared length alone triggers it.
    Oversize {
        /// Declared body length.
        len: usize,
        /// The cap ([`MAX_FRAME_BYTES`]).
        max: usize,
    },
    /// postcard decode failure or trailing bytes after the declared body.
    Malformed(postcard::Error),
    /// `Hello` major differs from [`CURRENT_VERSION`].major.
    IncompatibleVersion {
        /// Version the peer offered.
        got: ProtocolVersion,
        /// Version we speak.
        current: ProtocolVersion,
    },
    /// A window title exceeded [`MAX_TITLE_LEN`] bytes.
    TitleTooLong {
        /// Title length in bytes.
        len: usize,
        /// The cap ([`MAX_TITLE_LEN`]).
        max: usize,
    },
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CollectionTooLong { len, max } => {
                write!(f, "collection of {len} records exceeds {max}")
            }
            Self::Truncated { expected, actual } => {
                write!(f, "truncated frame: need {expected} bytes, have {actual}")
            }
            Self::Oversize { len, max } => {
                write!(f, "frame of {len} bytes exceeds cap of {max} bytes")
            }
            Self::Malformed(e) => write!(f, "malformed frame body: {e}"),
            Self::IncompatibleVersion { got, current } => write!(
                f,
                "incompatible peer major {}.{} (ours {}.{})",
                got.major, got.minor, current.major, current.minor
            ),
            Self::TitleTooLong { len, max } => {
                write!(f, "title of {len} bytes exceeds cap of {max} bytes")
            }
        }
    }
}

impl std::error::Error for DecodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Malformed(e) => Some(e),
            _ => None,
        }
    }
}

impl From<postcard::Error> for DecodeError {
    fn from(e: postcard::Error) -> Self {
        Self::Malformed(e)
    }
}

/// Encode one message as a length-prefixed frame: u32-LE body length
/// followed by the postcard body.
///
/// Panics only if the message cannot be encoded or exceeds
/// [`MAX_FRAME_BYTES`]; both indicate a local programming error, since the
/// cap bounds every message this crate can legally emit.
pub fn encode_frame(msg: &Message) -> Vec<u8> {
    // Grow-on-demand so small control messages avoid the 1 MiB buffer.
    // postcard::to_slice reports SerializeBufferFull when the buffer is
    // too small; that is the only recoverable encode error here.
    let mut cap = 1024usize;
    loop {
        let mut buf = vec![0u8; cap];
        match postcard::to_slice(msg, &mut buf) {
            Ok(used) => {
                let body_len = used.len();
                let mut frame = Vec::with_capacity(4 + body_len);
                frame.extend_from_slice(&(body_len as u32).to_le_bytes());
                frame.extend_from_slice(used);
                return frame;
            }
            Err(postcard::Error::SerializeBufferFull) => {
                cap *= 4;
                assert!(
                    cap <= MAX_FRAME_BYTES,
                    "encoded message exceeds {MAX_FRAME_BYTES}-byte frame cap"
                );
            }
            Err(e) => panic!("shell control encode failed: {e}"),
        }
    }
}

/// Owned credential frame. Drop wipes this encoder's sole allocation; this is
/// not a claim about caller/GTK/decoded/kernel copies or generic encode_frame.
pub struct SensitiveFrame(Vec<u8>);
impl SensitiveFrame {
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
    fn wipe(&mut self) {
        wipe_auth_bytes(&mut self.0);
    }
}
impl Drop for SensitiveFrame {
    fn drop(&mut self) {
        self.wipe();
    }
}
impl std::fmt::Debug for SensitiveFrame {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.write_str("SensitiveFrame(<redacted>)")
    }
}
struct OwnedUnlock(Message);
impl OwnedUnlock {
    fn wipe(&mut self) {
        if let Message::Command {
            kind:
                CommandKind::Unlock { password }
                | CommandKind::AnswerQuery {
                    answer: password, ..
                },
            ..
        } = &mut self.0
        {
            // SAFETY: exclusively owned string bytes become valid UTF-8 NULs.
            unsafe {
                wipe_auth_bytes(password.0.as_mut_vec());
            }
        }
    }
    /// One wiped buffer sized from the secret: no growth scratch or copy.
    fn encode(&self, secret: &Secret) -> Result<SensitiveFrame, ControlError> {
        if secret.0.len() > MAX_FRAME_BYTES {
            return Err(ControlError::Unexpected(
                "authentication frame exceeds bound".into(),
            ));
        }
        let cap = secret.0.len().saturating_add(64).min(MAX_FRAME_BYTES);
        let mut frame = SensitiveFrame(vec![0u8; cap + 4]);
        let used = postcard::to_slice(&self.0, &mut frame.0[4..])
            .map_err(|_| ControlError::Unexpected("authentication frame encoding refused".into()))?
            .len();
        frame.0[..4].copy_from_slice(&(used as u32).to_le_bytes());
        frame.0.truncate(used + 4);
        Ok(frame)
    }
}
impl Drop for OwnedUnlock {
    fn drop(&mut self) {
        self.wipe();
    }
}
fn wipe_auth_bytes(bytes: &mut [u8]) {
    for byte in bytes {
        unsafe {
            std::ptr::write_volatile(byte, 0);
        }
    }
}
/// Encode the unchanged Unlock wire shape with one wiped buffer, no growth
/// scratch or copied frame. Ownership includes input on errors/early denial.
pub fn encode_unlock_frame(id: u64, password: Secret) -> Result<SensitiveFrame, ControlError> {
    let message = OwnedUnlock(Message::Command {
        id,
        kind: CommandKind::Unlock { password },
    });
    let Message::Command {
        kind: CommandKind::Unlock { password },
        ..
    } = &message.0
    else {
        unreachable!()
    };
    message.encode(password)
}

/// Encode an `AnswerQuery` with the same single wiped buffer as
/// [`encode_unlock_frame`]; the owned answer is wiped on every path.
pub fn encode_answer_frame(
    id: u64,
    service: AuthService,
    sequence: u32,
    answer: Secret,
) -> Result<SensitiveFrame, ControlError> {
    let message = OwnedUnlock(Message::Command {
        id,
        kind: CommandKind::AnswerQuery {
            service,
            sequence,
            answer,
        },
    });
    let Message::Command {
        kind: CommandKind::AnswerQuery { answer, .. },
        ..
    } = &message.0
    else {
        unreachable!()
    };
    message.encode(answer)
}

/// Decode one length-prefixed frame into a message.
///
/// Checks run in wire order: prefix present, declared length within
/// [`MAX_FRAME_BYTES`] (before any postcard work), body complete, body
/// well-formed with no trailing bytes, titles within [`MAX_TITLE_LEN`],
/// and `Hello` major matching [`CURRENT_VERSION`].major.
pub fn decode_frame(bytes: &[u8]) -> Result<Message, DecodeError> {
    if bytes.len() < 4 {
        return Err(DecodeError::Truncated {
            expected: 4,
            actual: bytes.len(),
        });
    }
    let mut prefix = [0u8; 4];
    prefix.copy_from_slice(&bytes[..4]);
    let len = u32::from_le_bytes(prefix) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(DecodeError::Oversize {
            len,
            max: MAX_FRAME_BYTES,
        });
    }
    let body = &bytes[4..];
    if body.len() < len {
        return Err(DecodeError::Truncated {
            expected: len,
            actual: body.len(),
        });
    }
    if body.len() > len {
        return Err(DecodeError::Malformed(
            postcard::Error::DeserializeBadEncoding,
        ));
    }
    let (msg, trailing): (Message, &[u8]) = postcard::take_from_bytes(&body[..len])?;
    if !trailing.is_empty() {
        return Err(DecodeError::Malformed(
            postcard::Error::DeserializeBadEncoding,
        ));
    }
    validate_titles(&msg)?;
    if let Message::Hello { version } = &msg {
        if version.major != CURRENT_VERSION.major {
            return Err(DecodeError::IncompatibleVersion {
                got: *version,
                current: CURRENT_VERSION,
            });
        }
    }
    Ok(msg)
}

/// Reject messages carrying titles over [`MAX_TITLE_LEN`] bytes.
fn validate_titles(msg: &Message) -> Result<(), DecodeError> {
    fn check_len(len: usize) -> Result<(), DecodeError> {
        if len > MAX_PAD_FEATURES {
            return Err(DecodeError::CollectionTooLong {
                len,
                max: MAX_PAD_FEATURES,
            });
        }
        Ok(())
    }
    fn check_pad_labels(labels: &PadOsdLabels) -> Result<(), DecodeError> {
        for len in [
            labels.buttons.len(),
            labels.rings.len(),
            labels.strips.len(),
            labels.ring_modes.len(),
            labels.strip_modes.len(),
        ] {
            check_len(len)?;
        }
        let texts = labels
            .buttons
            .iter()
            .filter_map(|b| b.label.as_deref())
            .chain(
                labels
                    .rings
                    .iter()
                    .chain(&labels.strips)
                    .flatten()
                    .flatten()
                    .map(String::as_str),
            );
        for text in texts {
            check_title(text)?;
        }
        Ok(())
    }
    fn check_title(title: &str) -> Result<(), DecodeError> {
        if title.len() > MAX_TITLE_LEN {
            return Err(DecodeError::TitleTooLong {
                len: title.len(),
                max: MAX_TITLE_LEN,
            });
        }
        Ok(())
    }

    fn check_window(w: &WindowInfo) -> Result<(), DecodeError> {
        check_title(&w.title)
    }

    match msg {
        Message::Snapshot { windows, .. } => {
            for w in windows {
                check_window(w)?;
            }
        }
        Message::Changes { ops, .. } => {
            for op in ops {
                if let StateOp::WindowOpened(w) = op {
                    check_window(w)?;
                }
            }
        }
        Message::Command {
            kind: CommandKind::SetInputSettings(settings),
            ..
        } => {
            check_title(&settings.xkb_layout)?;
            check_title(&settings.xkb_variant)?;
            check_title(&settings.xkb_options)?;
            check_title(&settings.cursor_theme)?;
            check_title(&settings.locate_pointer_key)?;
        }
        Message::Command {
            kind: CommandKind::SetWindowIconGeometries { geometries },
            ..
        } if geometries.len() > MAX_WINDOW_ICON_GEOMETRIES
            || geometries.iter().any(|g| {
                g.width <= 0
                    || g.height <= 0
                    || g.width > 16384
                    || g.height > 16384
                    || g.x.checked_add(g.width).is_none()
                    || g.y.checked_add(g.height).is_none()
            }) =>
        {
            return Err(DecodeError::Malformed(
                postcard::Error::DeserializeBadEncoding,
            ));
        }
        Message::Command {
            kind: CommandKind::SetSwitcherThumbnails { thumbnails },
            ..
        } if thumbnails.len() > MAX_SWITCHER_THUMBNAILS => {
            return Err(DecodeError::Malformed(
                postcard::Error::DeserializeBadEncoding,
            ));
        }
        Message::Command {
            kind: CommandKind::SetSwitcherKeys { keys },
            ..
        } if keys.len() > MAX_SWITCHER_KEYS => {
            return Err(DecodeError::Malformed(
                postcard::Error::DeserializeBadEncoding,
            ));
        }
        Message::Command {
            kind:
                CommandKind::OskInput {
                    input: OskInput::Commit { text, modifiers },
                },
            ..
        } if text.len() > MAX_OSK_TEXT || modifiers.len() > MAX_OSK_MODIFIERS => {
            return Err(DecodeError::Malformed(
                postcard::Error::DeserializeBadEncoding,
            ));
        }
        Message::Command {
            kind: CommandKind::SetAccelerators { accelerators },
            ..
        } if accelerators.len() > MAX_ACCELERATORS => {
            return Err(DecodeError::Malformed(
                postcard::Error::DeserializeBadEncoding,
            ));
        }
        Message::Command {
            kind: CommandKind::AnswerQuery { answer, .. },
            ..
        } if answer.0.len() > MAX_AUTH_TEXT_LEN => {
            return Err(DecodeError::TitleTooLong {
                len: answer.0.len(),
                max: MAX_AUTH_TEXT_LEN,
            });
        }
        Message::AuthEvent { event, .. } => {
            let text = match event {
                AuthEvent::Info { text } | AuthEvent::Problem { text } => text.as_str(),
                AuthEvent::Query { question, .. } => question.as_str(),
                AuthEvent::FingerprintScan { .. } | AuthEvent::Unavailable => "",
            };
            if text.len() > MAX_AUTH_TEXT_LEN {
                return Err(DecodeError::TitleTooLong {
                    len: text.len(),
                    max: MAX_AUTH_TEXT_LEN,
                });
            }
        }
        Message::Command {
            kind: CommandKind::SetAcceleratorGrabs { grabs },
            ..
        } if grabs.len() > MAX_ACCELERATORS
            || grabs.iter().any(|g| g.flags & !SUPPORTED_GRAB_FLAGS != 0) =>
        {
            return Err(DecodeError::Malformed(
                postcard::Error::DeserializeBadEncoding,
            ));
        }
        Message::Command {
            kind:
                CommandKind::RequestPadOsd {
                    target: PadOsdTarget::DeviceNode(node),
                    ..
                },
            ..
        } => check_title(node)?,
        Message::Hello { .. }
        | Message::AcceleratorActivated { .. }
        | Message::AcceleratorDeactivated { .. }
        | Message::ConfirmDisplayChange { .. }
        | Message::WindowMenu { .. }
        | Message::WorkspacePopup { .. }
        | Message::PointerOutput { .. }
        | Message::ScreenReader { .. }
        | Message::KeyboardAidToggled { .. }
        | Message::DwellClickType { .. }
        | Message::PointerTimeout { .. }
        | Message::OnScreenKeyboard { .. }
        | Message::ShortcutConsent { .. }
        | Message::Command { .. }
        | Message::CommandResult { .. }
        | Message::Error { .. }
        | Message::Overview { .. }
        | Message::Switcher { .. }
        | Message::Outputs { .. }
        | Message::OverviewPreviews { .. } => {}
        // Bounded native authority: at most 64 lit outputs, short names.
        Message::NativeOutputInventory { outputs } => {
            if outputs.len() > 64 {
                return Err(DecodeError::CollectionTooLong {
                    len: outputs.len(),
                    max: 64,
                });
            }
            for output in outputs {
                check_title(&output.name)?;
                check_title(&output.connector_sysfs)?;
            }
        }
        // Names and values share the title bound: short by nature.
        Message::Environment { vars } => {
            for (name, value) in vars {
                check_title(name)?;
                check_title(value)?;
            }
        }
        Message::ShowOsd {
            connector,
            icon,
            label,
        } => {
            check_title(icon)?;
            check_title(label)?;
            if let Some(connector) = connector {
                check_title(connector)?;
            }
        }
        Message::ShowPadOsd(osd) => {
            check_title(&osd.name)?;
            check_title(&osd.layout_path)?;
            check_title(&osd.settings_path)?;
            if let Some(connector) = &osd.connector {
                check_title(connector)?;
            }
            check_len(osd.group_pads.len())?;
            for pad in &osd.group_pads {
                check_title(&pad.name)?;
            }
            check_pad_labels(&osd.labels)?;
        }
        Message::PadOsdLabels { labels, .. } => check_pad_labels(labels)?,
        Message::PadOsdInput { .. } | Message::ClosePadOsd { .. } => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_window(id: WindowId) -> WindowInfo {
        WindowInfo {
            on_all_workspaces: false,
            icon: None,
            id,
            title: format!("Terminal {id}"),
            app_id: Some("org.example.Terminal".to_owned()),
            workspace: 1,
            focused: id == 7,
            activation_token: format!("token-{id}"),
        }
    }

    fn sample_workspace(id: WorkspaceId) -> WorkspaceInfo {
        WorkspaceInfo {
            id,
            name: Some(format!("ws{id}")),
            active: id == 1,
        }
    }

    #[test]
    fn native_inventory_is_appended_and_bounds_authority_collection() {
        let legacy = Message::Outputs {
            outputs: vec![OutputInfo {
                name: "n".into(),
                width: 1,
                height: 1,
                primary: true,
            }],
        };
        // Existing Outputs index8 and unchanged positional OutputInfo body.
        assert_eq!(&encode_frame(&legacy)[4..], &[8, 1, 1, b'n', 2, 2, 1]);
        let owner = NativeOutputInfo {
            name: "eDP-1".into(),
            drm_device: 226,
            connector_id: 39,
            connector_sysfs: "/sys/owned/connector".into(),
            connector_device: 1,
            connector_inode: 2,
        };
        roundtrip(&Message::NativeOutputInventory {
            outputs: vec![owner.clone()],
        });
        assert!(matches!(
            decode_frame(&encode_frame(&Message::NativeOutputInventory {
                outputs: vec![owner; 65]
            })),
            Err(DecodeError::CollectionTooLong { len: 65, max: 64 })
        ));
    }

    fn sample_token() -> ActivationToken {
        ActivationToken::new("opaque-token-123".to_owned())
    }

    fn roundtrip(msg: &Message) {
        let frame = encode_frame(msg);
        let back = decode_frame(&frame).expect("roundtrip must decode");
        assert_eq!(&back, msg);
    }

    #[test]
    fn boxed_input_settings_preserve_inline_positional_wire_and_strict_decode() {
        // The old inline SetInputSettings is variant7. Box serializes
        // transparently; allocation changes must not change protocol0.34.
        struct InlineSettings<'a>(&'a InputSettings);
        impl Serialize for InlineSettings<'_> {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_newtype_variant("CommandKind", 7, "SetInputSettings", self.0)
            }
        }
        struct InlineCommand<'a>(&'a InputSettings);
        impl Serialize for InlineCommand<'_> {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                use serde::ser::SerializeStructVariant;
                let mut command =
                    serializer.serialize_struct_variant("Message", 3, "Command", 2)?;
                command.serialize_field("id", &42u64)?;
                command.serialize_field("kind", &InlineSettings(self.0))?;
                command.end()
            }
        }
        let settings = InputSettings {
            xkb_layout: "de".into(),
            keyboard_aids: KeyboardAids {
                sticky: true,
                toggle_beep: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let kind = CommandKind::SetInputSettings(Box::new(settings.clone()));
        let mut old = vec![0; MAX_FRAME_BYTES];
        let mut new = vec![0; MAX_FRAME_BYTES];
        assert_eq!(
            postcard::to_slice(&InlineSettings(&settings), &mut old).unwrap(),
            postcard::to_slice(&kind, &mut new).unwrap()
        );
        let message = Message::Command { id: 42, kind };
        let legacy_body = postcard::to_slice(&InlineCommand(&settings), &mut old).unwrap();
        let mut legacy_frame = (legacy_body.len() as u32).to_le_bytes().to_vec();
        legacy_frame.extend_from_slice(legacy_body);
        assert_eq!(encode_frame(&message), legacy_frame);
        assert_eq!(decode_frame(&legacy_frame).unwrap(), message);
        roundtrip(&message);
        let mut trailing = legacy_frame;
        trailing.push(0);
        let length = (trailing.len() - 4) as u32;
        trailing[..4].copy_from_slice(&length.to_le_bytes());
        assert!(matches!(
            decode_frame(&trailing),
            Err(DecodeError::Malformed(_))
        ));
    }

    #[test]
    fn combined_bell_and_modal_preferences_survive_positional_wire() {
        for attach in [false, true] {
            roundtrip(&Message::Command {
                id: 1,
                kind: CommandKind::SetInputSettings(Box::new(InputSettings {
                    attach_modal_dialogs: attach,
                    bell: BellPolicy {
                        audible: false,
                        visual: true,
                        frame_flash: true,
                        event_sounds: false,
                        theme: "custom".into(),
                    },
                    ..Default::default()
                })),
            });
        }
    }

    #[test]
    fn multitasking_preferences_survive_positional_wire() {
        for fixed_count in [1, 4, 36] {
            for enabled in [false, true] {
                roundtrip(&Message::Command {
                    id: 90,
                    kind: CommandKind::SetInputSettings(Box::new(InputSettings {
                        workspace_policy: WorkspacePolicy {
                            dynamic: enabled,
                            fixed_count,
                            only_on_primary: !enabled,
                            edge_tiling: enabled,
                        },
                        ..Default::default()
                    })),
                });
            }
        }
    }

    #[test]
    fn all_workspace_window_state_survives_delta_wire() {
        for enabled in [false, true] {
            let mut window = sample_window(7);
            window.on_all_workspaces = enabled;
            roundtrip(&Message::Changes {
                from_revision: 1,
                to_revision: 2,
                ops: vec![StateOp::WindowOpened(window)],
            });
        }
    }

    #[test]
    fn attach_modal_dialogs_survives_positional_control_wire() {
        for enabled in [true, false] {
            let settings = InputSettings {
                attach_modal_dialogs: enabled,
                ..Default::default()
            };
            let mut buffer = [0u8; 256];
            let body = postcard::to_slice(&settings, &mut buffer).unwrap();
            let decoded: InputSettings = postcard::from_bytes(body).unwrap();
            assert_eq!(decoded, settings);
        }
        assert!(InputSettings::default().attach_modal_dialogs);
    }

    #[test]
    fn center_new_windows_survives_positional_control_wire() {
        for enabled in [true, false] {
            let settings = InputSettings {
                center_new_windows: enabled,
                ..Default::default()
            };
            let mut buffer = [0u8; 512];
            let body = postcard::to_slice(&settings, &mut buffer).unwrap();
            let decoded: InputSettings = postcard::from_bytes(body).unwrap();
            assert_eq!(decoded.center_new_windows, enabled);
            assert_eq!(decoded, settings);
        }
        assert!(InputSettings::default().center_new_windows);
    }
    #[test]
    fn secrets_never_reach_debug_output() {
        let kind = CommandKind::Unlock {
            password: Secret("hunter2".into()),
        };
        let shown = format!("{kind:?}");
        assert!(!shown.contains("hunter2"), "{shown}");
        assert!(shown.contains("redacted"));
    }

    #[test]
    fn current_version_is_0_45() {
        assert_eq!(CURRENT_VERSION, ProtocolVersion::new(0, 45));
    }

    #[test]
    fn pointer_acceleration_nicks_and_wire_roundtrip() {
        for (nick, profile) in [
            ("default", PointerAccelProfile::Default),
            ("flat", PointerAccelProfile::Flat),
            ("adaptive", PointerAccelProfile::Adaptive),
            ("custom", PointerAccelProfile::Custom),
            ("unknown", PointerAccelProfile::Default),
        ] {
            assert_eq!(PointerAccelProfile::from_gsettings(nick), profile);
            roundtrip(&Message::Command {
                id: 1,
                kind: CommandKind::SetInputSettings(Box::new(InputSettings {
                    mouse_accel_profile: profile,
                    touchpad: TouchpadSettings {
                        accel_profile: profile,
                        ..TouchpadSettings::default()
                    },
                    trackball: TrackballSettings {
                        accel_profile: profile,
                        ..TrackballSettings::default()
                    },
                    pointing_stick: PointingStickSettings {
                        accel_profile: profile,
                        ..PointingStickSettings::default()
                    },
                    ..InputSettings::default()
                })),
            });
        }
    }

    #[test]
    fn device_setting_nicks_follow_gsettings_desktop_schemas_51() {
        // gdesktop-enums.h 51.0 nicks; unknown values keep the schema default.
        assert_eq!(SendEvents::from_gsettings("enabled"), SendEvents::Enabled);
        assert_eq!(SendEvents::from_gsettings("disabled"), SendEvents::Disabled);
        assert_eq!(
            SendEvents::from_gsettings("disabled-on-external-mouse"),
            SendEvents::DisabledOnExternalMouse
        );
        assert_eq!(SendEvents::from_gsettings("bogus"), SendEvents::Enabled);
        for (nick, method) in [
            ("default", TouchpadClickMethod::Default),
            ("none", TouchpadClickMethod::None),
            ("areas", TouchpadClickMethod::Areas),
            ("fingers", TouchpadClickMethod::Fingers),
            ("bogus", TouchpadClickMethod::Fingers),
        ] {
            assert_eq!(TouchpadClickMethod::from_gsettings(nick), method);
        }
        for (nick, map) in [
            ("default", TapButtonMap::Default),
            ("lrm", TapButtonMap::Lrm),
            ("lmr", TapButtonMap::Lmr),
            ("bogus", TapButtonMap::Default),
        ] {
            assert_eq!(TapButtonMap::from_gsettings(nick), map);
        }
        for (nick, method) in [
            ("default", PointingStickScrollMethod::Default),
            ("none", PointingStickScrollMethod::None),
            ("on-button-down", PointingStickScrollMethod::OnButtonDown),
            ("bogus", PointingStickScrollMethod::Default),
        ] {
            assert_eq!(PointingStickScrollMethod::from_gsettings(nick), method);
        }
    }

    #[test]
    fn device_settings_default_to_gnome_51_and_survive_the_positional_wire() {
        let defaults = InputSettings::default();
        assert_eq!(
            defaults.touchpad,
            TouchpadSettings {
                send_events: SendEvents::Enabled,
                click_method: TouchpadClickMethod::Fingers,
                edge_scrolling: false,
                two_finger_scrolling: true,
                tap_and_drag: true,
                tap_and_drag_lock: false,
                tap_button_map: TapButtonMap::Default,
                middle_click_emulation: false,
                accel_profile: PointerAccelProfile::Default,
            }
        );
        assert_eq!(
            defaults.mouse,
            MouseSettings {
                middle_click_emulation: false,
                scroll_wheel_emulation: ScrollWheelEmulation {
                    button: 0,
                    lock: false
                },
            }
        );
        assert_eq!(defaults.trackball, TrackballSettings::default());
        assert_eq!(defaults.pointing_stick, PointingStickSettings::default());
        roundtrip(&Message::Command {
            id: 3,
            kind: CommandKind::SetInputSettings(Box::new(InputSettings {
                touchpad: TouchpadSettings {
                    send_events: SendEvents::DisabledOnExternalMouse,
                    click_method: TouchpadClickMethod::None,
                    edge_scrolling: true,
                    two_finger_scrolling: false,
                    tap_and_drag: false,
                    tap_and_drag_lock: true,
                    tap_button_map: TapButtonMap::Lmr,
                    middle_click_emulation: true,
                    accel_profile: PointerAccelProfile::Flat,
                },
                mouse: MouseSettings {
                    middle_click_emulation: true,
                    scroll_wheel_emulation: ScrollWheelEmulation {
                        button: 2,
                        lock: true,
                    },
                },
                trackball: TrackballSettings {
                    accel_profile: PointerAccelProfile::Adaptive,
                    middle_click_emulation: true,
                    scroll_wheel_emulation: ScrollWheelEmulation {
                        button: 8,
                        lock: false,
                    },
                },
                pointing_stick: PointingStickSettings {
                    speed_milli: -750,
                    accel_profile: PointerAccelProfile::Flat,
                    scroll_method: PointingStickScrollMethod::None,
                },
                ..defaults.clone()
            })),
        });
        // Positional: the four device classes append 19 bytes at their
        // defaults after the 0.39 body, so a 0.39 peer's frame (which ends
        // at `mouse_accel_profile`) is rejected instead of misread.
        let current = encode_frame(&Message::Command {
            id: 3,
            kind: CommandKind::SetInputSettings(Box::new(defaults)),
        });
        let mut old = current[..current.len() - 19].to_vec();
        let body = u32::try_from(old.len() - 4).unwrap();
        old[..4].copy_from_slice(&body.to_le_bytes());
        assert!(decode_frame(&old).is_err());
        assert!(decode_frame(&current).is_ok());
    }

    #[test]
    fn wellbeing_effects_are_atomic_bounded_and_strictly_decoded() {
        for grayscale in [false, true] {
            for fade in [false, true] {
                let msg = Message::Command {
                    id: 71,
                    kind: CommandKind::SetWellbeingEffects { grayscale, fade },
                };
                roundtrip(&msg);
                let mut frame = encode_frame(&msg);
                let n = frame.len();
                frame[n - 1] = 2; // postcard bool accepts only 0/1
                assert!(decode_frame(&frame).is_err());
                let mut frame = encode_frame(&msg);
                frame.push(0);
                let body_len = (frame.len() - 4) as u32;
                frame[..4].copy_from_slice(&body_len.to_le_bytes());
                assert!(
                    decode_frame(&frame).is_err(),
                    "trailing fields must not be silently accepted"
                );
            }
        }
    }

    #[test]
    fn dash_icon_targets_roundtrip_and_reject_oversize_invalid_bounds() {
        let icon = WindowIconGeometry {
            window: 4,
            x: -900,
            y: 700,
            width: 64,
            height: 64,
        };
        roundtrip(&Message::Command {
            id: 77,
            kind: CommandKind::SetWindowIconGeometries {
                geometries: vec![icon],
            },
        });
        for geometries in [
            vec![icon; MAX_WINDOW_ICON_GEOMETRIES + 1],
            vec![WindowIconGeometry { width: 0, ..icon }],
            vec![WindowIconGeometry { height: -1, ..icon }],
            vec![WindowIconGeometry {
                width: 16385,
                ..icon
            }],
            vec![WindowIconGeometry {
                height: 16385,
                ..icon
            }],
            vec![WindowIconGeometry {
                x: i32::MAX,
                ..icon
            }],
            vec![WindowIconGeometry {
                y: i32::MAX,
                ..icon
            }],
        ] {
            let bytes = encode_frame(&Message::Command {
                id: 77,
                kind: CommandKind::SetWindowIconGeometries { geometries },
            });
            assert!(decode_frame(&bytes).is_err());
        }
    }

    #[test]
    fn dash_icon_targets_accept_empty_and_maximum_replacements_but_no_trailing_fields() {
        let icon = WindowIconGeometry {
            window: 4,
            x: i32::MIN,
            y: -500,
            width: 16384,
            height: 16384,
        };
        for geometries in [Vec::new(), vec![icon; MAX_WINDOW_ICON_GEOMETRIES]] {
            let message = Message::Command {
                id: 77,
                kind: CommandKind::SetWindowIconGeometries { geometries },
            };
            roundtrip(&message);
            let mut bytes = encode_frame(&message);
            bytes.push(0);
            let size = (bytes.len() - 4) as u32;
            bytes[..4].copy_from_slice(&size.to_le_bytes());
            assert!(decode_frame(&bytes).is_err(), "undeclared postcard fields");
        }
    }

    #[test]
    fn dash_icon_targets_reject_unknown_fields_and_command_variants() {
        use serde::de::value::{Error, MapDeserializer, StrDeserializer};
        let fields = [
            ("window", 4_u64),
            ("x", 10),
            ("y", 20),
            ("width", 64),
            ("height", 64),
        ];
        let valid = MapDeserializer::<_, Error>::new(fields.into_iter());
        assert!(WindowIconGeometry::deserialize(valid).is_ok());
        let invalid =
            MapDeserializer::<_, Error>::new(fields.into_iter().chain([("unexpected", 1)]));
        assert!(WindowIconGeometry::deserialize(invalid).is_err());
        assert!(CommandKind::deserialize(StrDeserializer::<Error>::new(
            "SetUnrecognizedIconGeometries"
        ))
        .is_err());
    }

    #[test]
    fn cursor_settings_survive_the_positional_control_wire() {
        for (theme, size, locate_pointer) in [("Adwaita", 24, false), ("DMZ-White", 48, true)] {
            roundtrip(&Message::Command {
                id: 1,
                kind: CommandKind::SetInputSettings(Box::new(InputSettings {
                    cursor_theme: theme.into(),
                    cursor_size: size,
                    locate_pointer,
                    right_to_left: true,
                    ..InputSettings::default()
                })),
            });
        }
    }

    #[test]
    fn locate_key_defaults_roundtrips_and_bounds() {
        assert_eq!(InputSettings::default().locate_pointer_key, "Control_L");
        for name in ["Control_L", "Control_R", "Super_L", "", "NoSuchKeysym"] {
            roundtrip(&Message::Command {
                id: 1,
                kind: CommandKind::SetInputSettings(Box::new(InputSettings {
                    locate_pointer_key: name.into(),
                    ..InputSettings::default()
                })),
            });
        }
        let frame = encode_frame(&Message::Command {
            id: 1,
            kind: CommandKind::SetInputSettings(Box::new(InputSettings {
                locate_pointer_key: "x".repeat(MAX_TITLE_LEN + 1),
                ..InputSettings::default()
            })),
        });
        assert!(matches!(
            decode_frame(&frame),
            Err(DecodeError::TitleTooLong { .. })
        ));
    }

    #[test]
    fn oversized_cursor_theme_names_are_rejected() {
        let frame = encode_frame(&Message::Command {
            id: 1,
            kind: CommandKind::SetInputSettings(Box::new(InputSettings {
                cursor_theme: "x".repeat(MAX_TITLE_LEN + 1),
                ..InputSettings::default()
            })),
        });
        assert!(matches!(
            decode_frame(&frame),
            Err(DecodeError::TitleTooLong { .. })
        ));
    }

    #[test]
    fn screen_reader_lifecycle_survives_control_wire() {
        for enabled in [false, true] {
            roundtrip(&Message::Command {
                id: 73,
                kind: CommandKind::SetScreenReader { enabled },
            });
        }
        for state in [
            ScreenReaderState::Disabled,
            ScreenReaderState::Starting,
            ScreenReaderState::Active,
            ScreenReaderState::Unavailable,
            ScreenReaderState::Conflict,
        ] {
            roundtrip(&Message::ScreenReader { state });
        }
    }

    #[test]
    fn live_bell_policy_survives_the_positional_control_wire() {
        roundtrip(&Message::Command {
            id: 1,
            kind: CommandKind::SetInputSettings(Box::new(InputSettings {
                bell: BellPolicy {
                    audible: false,
                    visual: true,
                    frame_flash: true,
                    event_sounds: false,
                    theme: "custom".into(),
                },
                ..InputSettings::default()
            })),
        });
    }

    #[test]
    fn shell_text_direction_survives_the_positional_control_wire() {
        for right_to_left in [false, true] {
            roundtrip(&Message::Command {
                id: 1,
                kind: CommandKind::SetInputSettings(Box::new(InputSettings {
                    right_to_left,
                    ..InputSettings::default()
                })),
            });
        }
    }

    #[test]
    fn keyboard_aids_survive_the_positional_control_wire() {
        roundtrip(&Message::Command {
            id: 1,
            kind: CommandKind::SetInputSettings(Box::new(InputSettings {
                keyboard_aids: KeyboardAids {
                    shortcuts: true,
                    sticky: true,
                    sticky_two_key_off: true,
                    slow: true,
                    slow_delay_ms: 450,
                    bounce: true,
                    bounce_delay_ms: 120,
                    mouse: true,
                    mouse_max_speed: 900,
                    ..KeyboardAids::default()
                },
                right_to_left: true,
                ..InputSettings::default()
            })),
        });
        for aid in [KeyboardAid::StickyKeys, KeyboardAid::SlowKeys] {
            for enabled in [false, true] {
                roundtrip(&Message::KeyboardAidToggled { aid, enabled });
            }
        }
    }

    #[test]
    fn pointer_aids_survive_the_positional_control_wire() {
        roundtrip(&Message::Command {
            id: 1,
            kind: CommandKind::SetInputSettings(Box::new(InputSettings {
                pointer_aids: PointerAids {
                    secondary_click: true,
                    secondary_click_ms: 700,
                    dwell_click: true,
                    dwell_ms: 900,
                    dwell_threshold: 4,
                    dwell_gesture: true,
                    gesture_drag: None,
                    ..PointerAids::default()
                },
                keyboard_aids: KeyboardAids {
                    sticky: true,
                    ..KeyboardAids::default()
                },
                ..InputSettings::default()
            })),
        });
        for click in [
            DwellClick::Primary,
            DwellClick::Double,
            DwellClick::Drag,
            DwellClick::Secondary,
        ] {
            roundtrip(&Message::Command {
                id: 2,
                kind: CommandKind::SetDwellClickType { click },
            });
            roundtrip(&Message::DwellClickType { click });
        }
        for kind in [
            PointerTimeoutKind::SecondaryClick,
            PointerTimeoutKind::Dwell,
            PointerTimeoutKind::Gesture,
        ] {
            roundtrip(&Message::PointerTimeout {
                kind,
                duration_ms: Some(1200),
                clicked: false,
                x: 10,
                y: -3,
            });
            roundtrip(&Message::PointerTimeout {
                kind,
                duration_ms: None,
                clicked: true,
                x: 0,
                y: 0,
            });
        }
    }

    #[test]
    fn motion_policy_survives_the_positional_control_wire() {
        for level in [MotionLevel::Full, MotionLevel::FadeOnly, MotionLevel::Off] {
            for slowdown in [0.5, 1.0, 4.0] {
                roundtrip(&Message::Command {
                    id: 1,
                    kind: CommandKind::SetInputSettings(Box::new(InputSettings {
                        motion: MotionPolicy::new(level, slowdown),
                        right_to_left: true,
                        ..InputSettings::default()
                    })),
                });
            }
        }
    }

    #[test]
    fn input_handedness_survives_the_positional_control_wire() {
        for mouse in [false, true] {
            for touchpad in [false, true] {
                roundtrip(&Message::Command {
                    id: 1,
                    kind: CommandKind::SetInputSettings(Box::new(InputSettings {
                        mouse_left_handed: mouse,
                        touchpad_left_handed: touchpad,
                        ..InputSettings::default()
                    })),
                });
            }
        }
    }

    #[test]
    fn version_compat_same_major_minor_not_newer() {
        let ours = ProtocolVersion::CURRENT;
        assert!(ours.is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 0).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 1).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 2).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 3).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 4).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 5).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 6).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 7).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 8).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 9).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 10).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 11).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 12).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 13).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 14).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 15).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 16).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 17).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 18).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 19).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 20).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 21).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 22).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 23).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 24).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 25).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 26).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 27).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 28).is_compatible_with(&ours));
        assert!(ProtocolVersion::new(0, 29).is_compatible_with(&ours));
        assert!(!ProtocolVersion::new(ours.major, ours.minor + 1).is_compatible_with(&ours));
        assert!(!ProtocolVersion::new(1, 4).is_compatible_with(&ours));
        assert!(!ProtocolVersion::new(1, 0).is_compatible_with(&ours));
    }

    #[test]
    fn dynamic_workspaces_end_with_one_empty() {
        assert_eq!(dynamic_workspace_count(None, 0), 1);
        assert_eq!(dynamic_workspace_count(Some(0), 0), 2);
        assert_eq!(dynamic_workspace_count(Some(0), 1), 2);
        assert_eq!(dynamic_workspace_count(Some(2), 0), 4);
    }

    #[test]
    fn roundtrip_environment() {
        roundtrip(&Message::Environment {
            vars: vec![("DISPLAY".to_owned(), ":0".to_owned())],
        });
        roundtrip(&Message::Environment { vars: Vec::new() });
    }

    #[test]
    fn oversize_environment_values_are_rejected() {
        let frame = encode_frame(&Message::Environment {
            vars: vec![("DISPLAY".to_owned(), "x".repeat(MAX_TITLE_LEN + 1))],
        });
        assert!(matches!(
            decode_frame(&frame),
            Err(DecodeError::TitleTooLong { .. })
        ));
    }

    #[test]
    fn roundtrip_switcher() {
        for action in [
            SwitcherAction::Step { forward: true },
            SwitcherAction::Step { forward: false },
            SwitcherAction::Commit,
            SwitcherAction::Cancel,
        ] {
            roundtrip(&Message::Switcher { action });
        }
    }

    #[test]
    fn roundtrip_hello() {
        roundtrip(&Message::Hello {
            version: ProtocolVersion::CURRENT,
        });
    }

    #[test]
    fn roundtrip_snapshot() {
        roundtrip(&Message::Snapshot {
            revision: 42,
            windows: vec![sample_window(7), sample_window(8)],
            workspaces: vec![sample_workspace(1), sample_workspace(2)],
            locked: false,
        });
        // Locked snapshots carry the flag with no window content.
        roundtrip(&Message::Snapshot {
            revision: 43,
            windows: vec![],
            workspaces: vec![sample_workspace(1)],
            locked: true,
        });
    }

    #[test]
    fn roundtrip_changes() {
        roundtrip(&Message::Changes {
            from_revision: 42,
            to_revision: 45,
            ops: vec![
                StateOp::WindowOpened(sample_window(9)),
                StateOp::WindowFocused { id: 9 },
                StateOp::WindowMoved {
                    id: 7,
                    workspace: 2,
                },
                StateOp::WindowClosed { id: 8 },
                StateOp::WorkspaceChanged(sample_workspace(2)),
            ],
        });
    }

    #[test]
    fn roundtrip_command() {
        for kind in [
            CommandKind::ActivateWindow {
                window: 7,
                token: sample_token(),
            },
            CommandKind::FocusWorkspace { workspace: 2 },
            CommandKind::ToggleOverview,
            CommandKind::CloseWindow { window: 7 },
            CommandKind::MoveScreenshotPointer { x: 475, y: 399 },
            CommandKind::Lock,
        ] {
            roundtrip(&Message::Command { id: 99, kind });
        }
    }

    #[test]
    fn roundtrip_command_result() {
        roundtrip(&Message::CommandResult {
            id: 99,
            status: CommandStatus::Applied,
        });
        roundtrip(&Message::CommandResult {
            id: 100,
            status: CommandStatus::Denied {
                reason: "stale token".to_owned(),
            },
        });
    }

    #[test]
    fn roundtrip_error() {
        roundtrip(&Message::Error {
            kind: ErrorKind::RevisionGap,
            message: "have 3, need 5; resnapshot".to_owned(),
        });
    }

    #[test]
    fn rejects_oversize_before_decode() {
        // Declared length over the cap with no body at all: must still be
        // Oversize (never Truncated or Malformed), proving the cap runs
        // before postcard decoding.
        let mut bytes = ((MAX_FRAME_BYTES + 1) as u32).to_le_bytes().to_vec();
        assert!(matches!(
            decode_frame(&bytes),
            Err(DecodeError::Oversize { .. })
        ));
        // Garbage body under a huge declared length: still Oversize.
        bytes.extend_from_slice(&[0xFF; 8]);
        assert!(matches!(
            decode_frame(&bytes),
            Err(DecodeError::Oversize { .. })
        ));
    }

    #[test]
    fn rejects_truncated_frames() {
        let frame = encode_frame(&Message::Hello {
            version: ProtocolVersion::CURRENT,
        });
        assert!(frame.len() > 4);
        // Short prefix.
        assert!(matches!(
            decode_frame(&frame[..2]),
            Err(DecodeError::Truncated { .. })
        ));
        // Empty input.
        assert!(matches!(
            decode_frame(&[]),
            Err(DecodeError::Truncated { .. })
        ));
        // Body cut short.
        assert!(matches!(
            decode_frame(&frame[..frame.len() - 1]),
            Err(DecodeError::Truncated { .. })
        ));
    }

    #[test]
    fn rejects_trailing_bytes_as_malformed() {
        let mut frame = encode_frame(&Message::Command {
            id: 1,
            kind: CommandKind::ToggleOverview,
        });
        frame.push(0x00);
        assert!(matches!(
            decode_frame(&frame),
            Err(DecodeError::Malformed(_))
        ));
    }

    #[test]
    fn rejects_stale_major() {
        for version in [ProtocolVersion::new(1, 1), ProtocolVersion::new(9, 0)] {
            let frame = encode_frame(&Message::Hello { version });
            match decode_frame(&frame) {
                Err(DecodeError::IncompatibleVersion { got, current }) => {
                    assert_eq!(got, version);
                    assert_eq!(current, CURRENT_VERSION);
                }
                other => panic!("expected IncompatibleVersion, got {other:?}"),
            }
        }
    }

    #[test]
    fn rejects_overlong_title_at_decode() {
        let mut window = sample_window(7);
        window.title = "x".repeat(MAX_TITLE_LEN + 1);
        let msg = Message::Snapshot {
            revision: 1,
            windows: vec![window],
            workspaces: vec![],
            locked: false,
        };
        let frame = encode_frame(&msg);
        match decode_frame(&frame) {
            Err(DecodeError::TitleTooLong { len, max }) => {
                assert_eq!(len, MAX_TITLE_LEN + 1);
                assert_eq!(max, MAX_TITLE_LEN);
            }
            other => panic!("expected TitleTooLong, got {other:?}"),
        }
    }

    #[test]
    fn accepts_title_at_exact_cap() {
        let mut window = sample_window(7);
        window.title = "x".repeat(MAX_TITLE_LEN);
        let msg = Message::Snapshot {
            revision: 1,
            windows: vec![window],
            workspaces: vec![],
            locked: false,
        };
        roundtrip(&msg);
    }

    #[test]
    fn rejects_overlong_title_in_changes_op() {
        let mut window = sample_window(7);
        window.title = "y".repeat(MAX_TITLE_LEN + 64);
        let msg = Message::Changes {
            from_revision: 1,
            to_revision: 2,
            ops: vec![StateOp::WindowOpened(window)],
        };
        let frame = encode_frame(&msg);
        assert!(matches!(
            decode_frame(&frame),
            Err(DecodeError::TitleTooLong { .. })
        ));
    }

    #[test]
    fn token_debug_is_redacted() {
        let shown = format!("{:?}", sample_token());
        assert!(!shown.contains("opaque-token-123"), "token leaked: {shown}");
    }

    #[test]
    fn control_error_display_io() {
        let err = ControlError::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "connection refused",
        ));
        let shown = format!("{err}");
        assert!(shown.contains("control socket I/O"));
    }

    #[test]
    fn control_error_display_decode() {
        let decode_err = DecodeError::Truncated {
            expected: 100,
            actual: 50,
        };
        let err = ControlError::Decode(decode_err);
        let shown = format!("{err}");
        assert!(shown.contains("control decode"));
    }

    #[test]
    fn control_error_display_would_block() {
        let err = ControlError::WouldBlock;
        let shown = format!("{err}");
        assert!(shown.contains("control socket not ready"));
    }

    #[test]
    fn control_error_display_unexpected() {
        let err = ControlError::Unexpected("out of order".to_owned());
        let shown = format!("{err}");
        assert!(shown.contains("unexpected message or state"));
        assert!(shown.contains("out of order"));
    }

    #[test]
    fn control_error_display_remote() {
        let err = ControlError::Remote {
            kind: ErrorKind::RevisionGap,
            message: "revision 5 needed".to_owned(),
        };
        let shown = format!("{err}");
        assert!(shown.contains("remote error"));
        assert!(shown.contains("revision 5 needed"));
    }

    #[test]
    fn control_error_display_app_constraint() {
        let err = ControlError::AppConstraint("no active window".to_owned());
        let shown = format!("{err}");
        assert!(shown.contains("constraint"));
        assert!(shown.contains("no active window"));
    }

    #[test]
    fn control_error_from_io_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "permission denied");
        let err: ControlError = io_err.into();
        assert!(matches!(err, ControlError::Io(_)));
    }

    #[test]
    fn control_error_from_would_block_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::WouldBlock, "would block");
        let err: ControlError = io_err.into();
        assert!(matches!(err, ControlError::WouldBlock));
    }

    #[test]
    fn control_error_from_decode_error() {
        let decode_err = DecodeError::Oversize {
            len: 2_000_000,
            max: MAX_FRAME_BYTES,
        };
        let err: ControlError = decode_err.into();
        assert!(matches!(err, ControlError::Decode(_)));
    }

    #[test]
    fn control_error_source_io() {
        use std::error::Error;
        let io_err = std::io::Error::new(std::io::ErrorKind::BrokenPipe, "broken pipe");
        let err = ControlError::Io(io_err);
        assert!(err.source().is_some());
    }

    #[test]
    fn control_error_source_decode() {
        use std::error::Error;
        let decode_err = DecodeError::Malformed(postcard::Error::DeserializeBadEncoding);
        let err = ControlError::Decode(decode_err);
        assert!(err.source().is_some());
    }

    #[test]
    fn control_error_source_other_variants() {
        use std::error::Error;
        let err = ControlError::WouldBlock;
        assert!(err.source().is_none());
        let err = ControlError::Unexpected("test".to_owned());
        assert!(err.source().is_none());
        let err = ControlError::AppConstraint("test".to_owned());
        assert!(err.source().is_none());
    }

    #[test]
    fn activation_token_debug_is_redacted() {
        let token = ActivationToken::new("secret-token-xyz".to_owned());
        let shown = format!("{:?}", token);
        assert!(!shown.contains("secret-token-xyz"), "token leaked: {shown}");
        assert!(shown.contains("redacted"));
    }

    #[test]
    fn activation_token_equality() {
        let token1 = ActivationToken::new("token123".to_owned());
        let token2 = ActivationToken::new("token123".to_owned());
        let token3 = ActivationToken::new("other".to_owned());
        assert_eq!(token1, token2);
        assert_ne!(token1, token3);
    }
}

#[cfg(test)]
mod sensitive_frame_tests {
    use super::*;
    #[test]
    fn dedicated_unlock_encoder_is_legacy_wire_equivalent() {
        for text in [
            String::new(),
            "owned-test-password".into(),
            "語".repeat(4096),
        ] {
            let expected = encode_frame(&Message::Command {
                id: u64::MAX,
                kind: CommandKind::Unlock {
                    password: Secret(text.clone()),
                },
            });
            let frame = encode_unlock_frame(u64::MAX, Secret(text)).unwrap();
            assert_eq!(frame.as_bytes(), expected);
        }
    }
    #[test]
    fn sensitive_frame_and_input_drop_wipe_their_owned_buffers() {
        let mut message = OwnedUnlock(Message::Command {
            id: 1,
            kind: CommandKind::Unlock {
                password: Secret("owned-test-password".into()),
            },
        });
        message.wipe();
        let Message::Command {
            kind: CommandKind::Unlock { password },
            ..
        } = &message.0
        else {
            panic!()
        };
        assert!(password.0.as_bytes().iter().all(|b| *b == 0));
        let mut frame = encode_unlock_frame(1, Secret("owned-test-password".into())).unwrap();
        frame.wipe();
        assert!(frame.as_bytes().iter().all(|b| *b == 0));
        // Calls the SAME wipe method used by Drop; never inspect freed memory.
        assert!(!format!("{frame:?}").contains("owned-test-password"));
    }
    #[test]
    fn oversize_input_and_body_overhead_fail_closed_without_panic() {
        assert!(encode_unlock_frame(1, Secret("p".repeat(MAX_FRAME_BYTES + 1))).is_err());
        assert!(encode_unlock_frame(u64::MAX, Secret("p".repeat(MAX_FRAME_BYTES))).is_err());
    }
}

#[cfg(test)]
mod accelerator_release_wire_tests {
    use super::*;
    fn grab(flags: u32) -> AcceleratorGrab {
        AcceleratorGrab {
            accelerator: Accelerator {
                action: 7,
                keysym: 114,
                mods: 0,
                modes: MODE_NORMAL,
            },
            flags,
        }
    }
    #[test]
    fn typed_grab_and_release_roundtrip_without_changing_legacy() {
        for message in [
            Message::Command {
                id: 9,
                kind: CommandKind::SetAcceleratorGrabs {
                    grabs: vec![grab(GRAB_TRIGGER_RELEASE | GRAB_IGNORE_AUTOREPEAT)],
                },
            },
            Message::AcceleratorDeactivated {
                action: 7,
                time: 200,
                mode: MODE_NORMAL,
            },
            Message::Command {
                id: 10,
                kind: CommandKind::SetAccelerators {
                    accelerators: vec![grab(0).accelerator],
                },
            },
        ] {
            assert_eq!(decode_frame(&encode_frame(&message)).unwrap(), message);
        }
    }
    #[test]
    fn typed_grab_decode_rejects_unknown_flags_and_oversized_lists() {
        for grabs in [vec![grab(1 << 3)], vec![grab(0); MAX_ACCELERATORS + 1]] {
            assert!(decode_frame(&encode_frame(&Message::Command {
                id: 11,
                kind: CommandKind::SetAcceleratorGrabs { grabs }
            }))
            .is_err());
        }
    }
    #[test]
    fn appended_variants_follow_the_0_36_positions() {
        let index = |message: &Message| encode_frame(message)[4];
        let last = index(&Message::PointerTimeout {
            kind: PointerTimeoutKind::Dwell,
            duration_ms: None,
            clicked: false,
            x: 0,
            y: 0,
        });
        for (offset, message) in [
            Message::NativeOutputInventory {
                outputs: Vec::new(),
            },
            Message::AcceleratorDeactivated {
                action: 7,
                time: 200,
                mode: MODE_NORMAL,
            },
            Message::ConfirmDisplayChange { timeout_secs: 20 },
        ]
        .iter()
        .enumerate()
        {
            assert_eq!(index(message), last + 1 + offset as u8);
        }
        let command = |kind: CommandKind| {
            let mut buffer = [0_u8; 64];
            postcard::to_slice(&kind, &mut buffer).unwrap()[0]
        };
        assert_eq!(
            command(CommandKind::SetAcceleratorGrabs { grabs: Vec::new() }),
            command(CommandKind::SetDwellClickType {
                click: DwellClick::Primary,
            }) + 1
        );
        assert_eq!(
            command(CommandKind::CompleteDisplayChange { keep: true }),
            command(CommandKind::SetAcceleratorGrabs { grabs: Vec::new() }) + 1
        );
        for message in [
            Message::ConfirmDisplayChange { timeout_secs: 20 },
            Message::Command {
                id: 12,
                kind: CommandKind::CompleteDisplayChange { keep: false },
            },
        ] {
            assert_eq!(decode_frame(&encode_frame(&message)).unwrap(), message);
        }
    }
}

#[cfg(test)]
mod on_screen_keyboard_wire_tests {
    use super::*;

    #[test]
    fn osk_messages_roundtrip_and_sit_last() {
        for message in [
            Message::Command {
                id: 3,
                kind: CommandKind::OskInput {
                    input: OskInput::Commit {
                        text: "é".into(),
                        modifiers: vec![0xffe3],
                    },
                },
            },
            Message::Command {
                id: 4,
                kind: CommandKind::OskInput {
                    input: OskInput::Keyval {
                        keysym: 0xff0d,
                        pressed: true,
                    },
                },
            },
            Message::OnScreenKeyboard {
                focus: Some(TextFieldFocus {
                    purpose: 8,
                    hints: 0x200,
                }),
                show: true,
                touch_auto: false,
            },
            Message::OnScreenKeyboard {
                focus: None,
                show: false,
                touch_auto: true,
            },
        ] {
            assert_eq!(decode_frame(&encode_frame(&message)).unwrap(), message);
        }
        let index = |message: &Message| encode_frame(message)[4];
        assert_eq!(
            index(&Message::OnScreenKeyboard {
                focus: None,
                show: false,
                touch_auto: false,
            }),
            index(&Message::ConfirmDisplayChange { timeout_secs: 0 }) + 1
        );
        let command = |kind: CommandKind| {
            let mut buffer = [0_u8; 64];
            postcard::to_slice(&kind, &mut buffer).unwrap()[0]
        };
        assert_eq!(
            command(CommandKind::OskInput {
                input: OskInput::Keyval {
                    keysym: 0,
                    pressed: false,
                },
            }),
            command(CommandKind::CompleteDisplayChange { keep: false }) + 1
        );
    }

    #[test]
    fn osk_commit_decode_rejects_oversized_text_and_modifiers() {
        for input in [
            OskInput::Commit {
                text: "x".repeat(MAX_OSK_TEXT + 1),
                modifiers: Vec::new(),
            },
            OskInput::Commit {
                text: "x".into(),
                modifiers: vec![0xffe1; MAX_OSK_MODIFIERS + 1],
            },
        ] {
            assert!(decode_frame(&encode_frame(&Message::Command {
                id: 5,
                kind: CommandKind::OskInput { input },
            }))
            .is_err());
        }
        let at_cap = Message::Command {
            id: 6,
            kind: CommandKind::OskInput {
                input: OskInput::Commit {
                    text: "x".repeat(MAX_OSK_TEXT),
                    modifiers: vec![0xffe1; MAX_OSK_MODIFIERS],
                },
            },
        };
        assert_eq!(decode_frame(&encode_frame(&at_cap)).unwrap(), at_cap);
    }
}

#[cfg(test)]
mod auth_conversation_wire_tests {
    use super::*;

    #[test]
    fn auth_conversation_is_appended_and_round_trips() {
        let command = |kind: &CommandKind| {
            let mut buffer = [0_u8; 256];
            postcard::to_slice(kind, &mut buffer).unwrap()[0]
        };
        // 0.44 sits after 0.43's on-screen keyboard.
        let osk = command(&CommandKind::OskInput {
            input: OskInput::Keyval {
                keysym: 0,
                pressed: false,
            },
        });
        let begin = CommandKind::BeginVerification {
            service: AuthService::Password,
        };
        let answer = CommandKind::AnswerQuery {
            service: AuthService::Password,
            sequence: 2,
            answer: Secret("one-time-code".into()),
        };
        assert_eq!(command(&begin), osk + 1);
        assert_eq!(command(&answer), osk + 2);
        assert_eq!(command(&CommandKind::CancelVerification), osk + 3);
        let message_index = |message: &Message| {
            let mut buffer = [0_u8; 256];
            postcard::to_slice(message, &mut buffer).unwrap()[0]
        };
        let event = Message::AuthEvent {
            request: 9,
            service: AuthService::Fingerprint,
            event: AuthEvent::FingerprintScan { swipe: true },
        };
        assert_eq!(
            message_index(&event),
            message_index(&Message::OnScreenKeyboard {
                focus: None,
                show: false,
                touch_auto: false,
            }) + 1
        );
        for message in [
            event,
            Message::AuthEvent {
                request: 9,
                service: AuthService::Password,
                event: AuthEvent::Query {
                    sequence: 1,
                    question: "Verification code:".into(),
                    secret: false,
                },
            },
            Message::AuthEvent {
                request: 9,
                service: AuthService::Password,
                event: AuthEvent::Info {
                    text: "A code was sent".into(),
                },
            },
            Message::AuthEvent {
                request: 9,
                service: AuthService::Password,
                event: AuthEvent::Problem {
                    text: "Failed".into(),
                },
            },
            Message::AuthEvent {
                request: 9,
                service: AuthService::Fingerprint,
                event: AuthEvent::Unavailable,
            },
            Message::Command { id: 3, kind: begin },
            Message::Command {
                id: 4,
                kind: answer,
            },
            Message::Command {
                id: 5,
                kind: CommandKind::CancelVerification,
            },
        ] {
            assert_eq!(decode_frame(&encode_frame(&message)).unwrap(), message);
        }
    }

    #[test]
    fn answer_frames_match_the_generic_encoding_and_never_debug_the_answer() {
        let expected = encode_frame(&Message::Command {
            id: 77,
            kind: CommandKind::AnswerQuery {
                service: AuthService::Password,
                sequence: 3,
                answer: Secret("owned-code".into()),
            },
        });
        let frame =
            encode_answer_frame(77, AuthService::Password, 3, Secret("owned-code".into())).unwrap();
        assert_eq!(frame.as_bytes(), expected);
        let kind = CommandKind::AnswerQuery {
            service: AuthService::Password,
            sequence: 3,
            answer: Secret("owned-code".into()),
        };
        assert!(!format!("{kind:?}").contains("owned-code"));
        let mut owned = OwnedUnlock(Message::Command { id: 1, kind });
        owned.wipe();
        let Message::Command {
            kind: CommandKind::AnswerQuery { answer, .. },
            ..
        } = &owned.0
        else {
            unreachable!()
        };
        assert!(answer.0.bytes().all(|byte| byte == 0));
    }

    #[test]
    fn oversized_auth_texts_are_refused_on_decode() {
        let long = "x".repeat(MAX_AUTH_TEXT_LEN + 1);
        for message in [
            Message::AuthEvent {
                request: 1,
                service: AuthService::Password,
                event: AuthEvent::Info { text: long.clone() },
            },
            Message::AuthEvent {
                request: 1,
                service: AuthService::Password,
                event: AuthEvent::Query {
                    sequence: 1,
                    question: long.clone(),
                    secret: true,
                },
            },
            Message::Command {
                id: 1,
                kind: CommandKind::AnswerQuery {
                    service: AuthService::Password,
                    sequence: 1,
                    answer: Secret(long.clone()),
                },
            },
        ] {
            assert!(matches!(
                decode_frame(&encode_frame(&message)),
                Err(DecodeError::TitleTooLong { .. })
            ));
        }
        let exact = "x".repeat(MAX_AUTH_TEXT_LEN);
        let fits = Message::AuthEvent {
            request: 1,
            service: AuthService::Password,
            event: AuthEvent::Problem { text: exact },
        };
        assert_eq!(decode_frame(&encode_frame(&fits)).unwrap(), fits);
    }
}

#[cfg(test)]
mod tablet_osd_wire_order_tests {
    use super::*;

    #[test]
    fn tablet_osd_variants_follow_the_0_44_auth_conversation() {
        let tag = |bytes: &[u8]| bytes[0];
        let mut buffer = [0_u8; 256];
        let cancel =
            tag(postcard::to_slice(&CommandKind::CancelVerification, &mut buffer).unwrap());
        let request = CommandKind::RequestPadOsd {
            target: PadOsdTarget::Pad(1),
            edition_mode: false,
        };
        assert_eq!(
            tag(postcard::to_slice(&request, &mut buffer).unwrap()),
            cancel + 1
        );
        let closed = CommandKind::PadOsdClosed { pad: 1 };
        assert_eq!(
            tag(postcard::to_slice(&closed, &mut buffer).unwrap()),
            cancel + 2
        );
        let auth = Message::AuthEvent {
            request: 1,
            service: AuthService::Password,
            event: AuthEvent::Unavailable,
        };
        let auth = tag(postcard::to_slice(&auth, &mut buffer).unwrap());
        let show = Message::ShowOsd {
            connector: None,
            icon: String::new(),
            label: String::new(),
        };
        assert_eq!(
            tag(postcard::to_slice(&show, &mut buffer).unwrap()),
            auth + 1
        );
        let close = Message::ClosePadOsd { pad: 1 };
        assert_eq!(
            tag(postcard::to_slice(&close, &mut buffer).unwrap()),
            auth + 5
        );
    }
}
