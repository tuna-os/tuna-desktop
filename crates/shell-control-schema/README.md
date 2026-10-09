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
- Versioning: `ProtocolVersion`, `CURRENT_VERSION` (currently 0.32).
- Dash targets: `WindowIconGeometry`, `MAX_WINDOW_ICON_GEOMETRIES`, and
  appended `CommandKind::SetWindowIconGeometries` full replacements. Unknown
  windows are ignored by the compositor; an empty list selects monitor fallback.
- Messages: `Message`, `StateOp`, `CommandKind`, `CommandStatus`,
  `ErrorKind`.
- State types: `WindowInfo`, `WorkspaceInfo`, `OutputInfo`, `PreviewInfo`,
  `InputSettings`, `WindowId`, `WorkspaceId`, `dynamic_workspace_count`.
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

`InputSettings::pointer_aids` appends GNOME 51's `org.gnome.desktop.a11y.mouse`
(simulated secondary click, hover click and its gesture directions) after the
0.34 keyboard aids. `CommandKind::SetDwellClickType` carries the hover-click
chooser's pick; the compositor reports `Message::DwellClickType` and
`Message::PointerTimeout` (the pie timer). All are appended; both peers ship
together because the postcard body is positional.
