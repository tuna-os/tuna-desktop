# tuna-shell-control

The versioned private IPC protocol between the compositor (authoritative)
and the shell: major/minor version negotiation, a full snapshot on every
(re)connect, ordered incremental changes, request-ID-bearing commands, and
compositor-issued activation tokens.

Wire format (ADR 0002): a `u32` little-endian length prefix followed by a
postcard body. Frames larger than `MAX_FRAME_BYTES` (1 MiB) are rejected
before decoding.

## Public API (`src/lib.rs`)

- Framing: `encode_frame`, `decode_frame`, `DecodeError`, `MAX_FRAME_BYTES`.
- Versioning: `ProtocolVersion`, `CURRENT_VERSION` (currently 0.41).
- Dash targets: `WindowIconGeometry`, `MAX_WINDOW_ICON_GEOMETRIES`, and
  appended `CommandKind::SetWindowIconGeometries` full replacements. Unknown
  windows are ignored by the compositor; an empty list selects monitor fallback.
- Messages: `Message`, `StateOp`, `CommandKind`, `CommandStatus`,
  `ErrorKind`.
- State types: `WindowInfo`, `WorkspaceInfo`, `OutputInfo`, `PreviewInfo`,
  `NativeOutputInfo`, `InputSettings`, `WindowId`, `WorkspaceId`,
  `dynamic_workspace_count`.
- Pointer devices: `InputSettings::mouse_accel_profile` plus the appended
  `touchpad`, `mouse`, `trackball` and `pointing_stick` groups
  (`TouchpadSettings`, `MouseSettings`, `TrackballSettings`,
  `PointingStickSettings`) carry GNOME 51's per-device-class peripherals
  keys. Every acceleration key uses `PointerAccelProfile`: the compositor
  supports default, flat, and adaptive; custom curves are unsupported and
  fall back to the device default.
- Tokens and secrets: `ActivationToken`, `TokenMeta`, `Secret`.
- Switcher and keybindings: `SwitcherAction`, `SwitcherKey`,
  `SwitcherThumbnail`, `WindowAction`, `Accelerator`, and the `MOD_*` and
  `MODE_*` constants.

Its only dependencies are `postcard` and `serde`. Tests are inline in
`src/lib.rs`.

## Dependents

`tuna-compositor` (server side), `tuna-shell-host` (client in `control`),
and `tuna-shell-gtk`.

## Docs

[ADR 0002](../../docs/adr/0002-shell-control-ipc-envelope.md) (envelope and
activation tokens),
[ADR 0005](../../docs/adr/0005-control-socket-peer-authentication.md)
(control-socket peer authentication),
[architecture](../../docs/architecture.md).

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
