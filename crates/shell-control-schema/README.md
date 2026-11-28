# tuna-shell-control

The versioned private IPC protocol between the compositor (authoritative)
and the shell: major/minor version negotiation, a full snapshot on every
(re)connect, ordered incremental changes, request-ID-bearing commands, and
compositor-issued activation tokens.

Wire format (ADR 0002): a `u32` little-endian length prefix followed by a
postcard body. Frames larger than `MAX_FRAME_BYTES` (1 MiB) are rejected
before decoding.

## Public API (`src/lib.rs`)

- Envelope (`src/framing.rs`, ADR 0004 seam 1): `framing::encode`,
  `framing::decode`, `framing::complete_frame_len`, `framing::body_len`,
  `framing::FrameError`, `framing::PREFIX_LEN` and `MAX_FRAME_BYTES` (also
  at the crate root). Generic over any serde type, and imports nothing from
  the message schema; `FrameError` covers only truncated, oversize and
  malformed frames.
- Messages on the wire: `encode_frame`, `decode_frame` and `DecodeError`
  apply the envelope to `Message`, then add the schema checks (title and
  collection bounds, `Hello` major). `DecodeError` keeps the envelope
  variants (`From<FrameError>`) so existing callers match unchanged.
- Versioning: `ProtocolVersion`, `CURRENT_VERSION`. The current number is
  `ProtocolVersion::CURRENT` in `src/lib.rs`, whose doc comment lists every
  minor; the wire-change rules are in the
  [API stability policy](../../docs/api-stability.md#shell-control-protocol-tuna-shell-control).
- Workspace swipe progress (0.48): `Message::WorkspaceProgress { position,
  duration_us }` (peers >=48 only) carries a finger-following workspace
  swipe's position in `WORKSPACE_PROGRESS_SCALE`ths of a workspace on begin,
  each update and release (with the settle duration), so the shell's
  Activities dots follow the fingers as GNOME 51's do.
- Display-change confirmation (0.42): `Message::ConfirmDisplayChange
  { timeout_secs }` (sent only to peers advertising minor >=42) asks the shell
  for GNOME's "Keep these display settings?" dialog; the shell answers with
  `CommandKind::CompleteDisplayChange { keep }`.
- Lock-screen authentication conversation (0.44), GDM's UserVerifier shape:
  `CommandKind::BeginVerification { service }` (`AuthService::Password` or
  `Fingerprint`) starts one conversation for the session's own user and is
  answered once (`Applied` = unlocked, `Denied` = stopped);
  `Message::AuthEvent { request, service, event }` (peers >=44 only) carries
  its `Info`, `Problem`, `Query { sequence, question, secret }`,
  `FingerprintScan { swipe }` and `Unavailable` steps;
  `CommandKind::AnswerQuery { service, sequence, answer }` answers one query
  (`encode_answer_frame` wipes its single buffer); `CancelVerification`
  stops them all. Texts are bounded by `MAX_AUTH_TEXT_LEN`.
- Dash targets: `WindowIconGeometry`, `MAX_WINDOW_ICON_GEOMETRIES`, and
  appended `CommandKind::SetWindowIconGeometries` full replacements. Unknown
  windows are ignored by the compositor; an empty list selects monitor fallback.
- Overview swipe (0.49): `Message::OverviewSwipe { state, settle_us }`
  (peers >=49 only) carries GNOME's overview controls state while a
  touchpad swipe holds it (`settle_us: None`) and where and how fast its
  release settles, in `OVERVIEW_STATE_SCALE`ths up to `MAX_OVERVIEW_STATE`
  (the app grid); the shell's app grid follows it above the window picker.
- Close dialog (0.50): `Message::ShowCloseDialog { window, app_id,
  connector, x, y, width, height }` (peers >=50 only) asks for gnome-shell's
  "“%s” Is Not Responding" dialog over a window that missed mutter's
  `check-alive-timeout`, resent as the window moves; `HideCloseDialog
  { window }` dismisses it (a late pong, the window gone or hidden, the
  lock). The shell answers `CommandKind::RespondCloseDialog { window,
  response }` with `CloseDialogResponse::ForceClose` or `Wait`. Texts are
  bounded by `MAX_TITLE_LEN`, sides by `MAX_CLOSE_DIALOG_SIDE`.
- Lock screen shown (0.51): `Message::LockScreenShown { shown }` (peers
  >=51 only) says the session is locked and the lock curtain has landed
  (screenShield.js `_lockScreenShown`); `false` once it unlocks. The shell
  reports `org.gnome.ScreenSaver` active only while it is shown, because
  gnome-settings-daemon blanks the screen on `ActiveChanged(true)`.
- Shell bus identity (0.52): `CommandKind::DeclareBusName { unique_name }`
  (peers >=52 only) names the shell's session-bus connection. The
  compositor admits D-Bus capture and Introspect calls as the shell only
  from names the supervised shell's pidfd-pinned control session declared,
  and denies the command from any other session (ADR 0010).
- Mutter preferences and launches (0.53): `InputSettings::auto_maximize`
  and `overlay_key` (mutter's `auto-maximize` and `overlay-key`), and
  `CommandKind::BeginStartupSequence { app_id, wm_class }` (peers >=53
  only): an app launch's startup sequence, the busy cursor until a window
  of the app maps or 15 s pass. Strings are bounded by `MAX_TITLE_LEN`.
- Remote access (0.54): `Message::RemoteAccess { handles }` (peers >=54
  only) carries every active grant whole whenever the set changes, as
  `RemoteAccessHandle { kind, id, owner, owner_name, recording, targets }`
  (mutter's `MetaRemoteAccessHandle`: `RemoteAccessKind::ScreenCast`,
  `RemoteDesktop` or `InputCapture`; `RemoteAccessOwner::Shell`, `Portal`
  or `RemoteDesktopDaemon`; `RemoteAccessTarget::Monitor`, `Window`,
  `Area` or `Input`). The shell's Stop answers
  `CommandKind::StopRemoteAccess { kind, id }`, which revokes that grant
  permanently. Bounded by `MAX_REMOTE_ACCESS_HANDLES`,
  `MAX_REMOTE_ACCESS_TARGETS` and `MAX_TITLE_LEN`.
- Screen wake (0.55): `CommandKind::WakeUpScreen` lifts the idle fade off
  the locked, blanked screen when a notification reaches the lock screen's
  list (screenShield.js `_wakeUpScreen`); it is `Denied` while unlocked,
  never unlocks and does not count as user activity.
- Input-panel OFF (0.62): `Message::HideInputPanel` (peers >=62 only)
  follows the `OnScreenKeyboard` update of a text-input-v3 version 2
  client's `hide_input_panel`, so the shell's keyboard closes at once.
- Messages: `Message`, `StateOp`, `CommandKind`, `CommandStatus`,
  `ErrorKind`.
- State types: `WindowInfo`, `WorkspaceInfo`, `OutputInfo`, `PreviewInfo`,
  `NativeOutputInfo` (re-exported from `tuna-output`), `InputSettings`,
  `WindowId`, `WorkspaceId`, `dynamic_workspace_count`.
- Pointer devices: `InputSettings::mouse_accel_profile` plus the appended
  `touchpad`, `mouse`, `trackball` and `pointing_stick` groups
  (`TouchpadSettings`, `MouseSettings`, `TrackballSettings`,
  `PointingStickSettings`) carry GNOME 51's per-device-class peripherals
  keys. Every acceleration key uses `PointerAccelProfile`: default, flat,
  adaptive, and custom.
- Custom acceleration and typing timeout (0.56): `InputSettings::custom_accel`
  (`CustomAccelConfigs`, one `CustomAccelCurve { step, speeds }` per pointer
  class, GNOME's `custom-accel-config`) and
  `InputSettings::disable_while_typing_timeout_ms` are appended after
  `overlay_key`. A class whose profile is `Custom` gets its curve
  after `CustomAccelCurve::or_mutter_fallback`; a curve libinput rejects
  leaves the device default. Positional: both peers ship together.
- Tokens and secrets: `ActivationToken`, `TokenMeta`, `Secret`.
- Switcher and keybindings: `SwitcherAction`, `SwitcherKey`,
  `SwitcherThumbnail`, `WindowAction`, `Accelerator`, and the `MOD_*` and
  `MODE_*` constants.

Its dependencies are `postcard`, `serde` and `tuna-output`. Schema tests
are inline in `src/lib.rs` and in
`crates/shell-control-schema/tests/protocol_schema_tests.rs`; the
envelope's tests, including the check that `src/framing.rs` stays
message-agnostic, are in
`crates/shell-control-schema/tests/framing_tests.rs`.

## Dependents

`tuna-compositor` (server side), `tuna-shell-host` (client in `control`),
and `tuna-shell-gtk`.

## Docs

[ADR 0002](../../docs/adr/0002-shell-control-ipc-envelope.md) (envelope and
activation tokens),
[ADR 0005](../../docs/adr/0005-control-socket-peer-authentication.md)
(control-socket peer authentication),
[architecture](../../docs/architecture.md),
[API stability and wire versioning](../../docs/api-stability.md) (this crate
is internal; its wire format is the cross-process contract).

## Wellbeing scene effects (0.35)

`CommandKind::SetWellbeingEffects { grayscale: bool, fade: bool }` appends a
single atomic command. Earlier command indices, 0.34 keyboard/cursor fields,
and strict trailing-byte rejection remain intact. Both booleans are required;
postcard rejects values other than 0/1. The authenticated shell sends the full
pair initially and whenever wellbeing state changes. The compositor owns the
three-second entry animation and immediate exit; the cursor is excluded.

## Pointer accessibility aids (0.36)

`InputSettings::pointer_aids` (GNOME's `org.gnome.desktop.a11y.mouse`) is
appended after `keyboard_aids`; `CommandKind::SetDwellClickType` is appended
for the hover-click chooser, and the compositor reports
`Message::DwellClickType` and `Message::PointerTimeout` (pie timer) as the
last two message variants. Positional: both peers ship together.

## Native output inventory (0.37)

`Message::NativeOutputInventory { outputs: Vec<NativeOutputInfo> }` appends
the compositor-owned DRM connector identity (char-device rdev, connector
object ID and canonical sysfs object) for each lit native output. `OutputInfo`
and the `Outputs` message are byte-for-byte unchanged. The compositor sends it
only to peers advertising minor 37 or later; an empty list explicitly revokes
earlier authority, and nested sessions never send a non-empty one. Decoding
rejects more than 64 records (`DecodeError::CollectionTooLong`).


## Accelerator grab flags and release (0.38)

`CommandKind::SetAcceleratorGrabs { grabs: Vec<AcceleratorGrab> }` and
`Message::AcceleratorDeactivated { action, time, mode }` append in 0.38:
atomic grab replacement with GNOME's `IGNORE_AUTOREPEAT` and
`TRIGGER_RELEASE` flags (`SUPPORTED_GRAB_FLAGS`; anything else, or more than
`MAX_ACCELERATORS`, fails decoding), and the release of a `TRIGGER_RELEASE`
grab's press. The legacy `SetAccelerators` body is unchanged.

The compositor sends release events only to peers advertising minor 38 or
later and rejects typed grabs from older peers. `NativeOutputInventory` stays
at its shipped 0.37 position; this slice adds no monitor-identity or brightness
journal messages.
