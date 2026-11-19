# tuna-output

The compositor-owned native output identity: which DRM connector, on which
KMS character device and canonical sysfs object, a lit native output is.
Hardware-facing crates use it without depending on the shell-control wire
protocol.

## Public API (`src/lib.rs`)

- `NativeOutputInfo`: connector name, char-device rdev (`drm_device`), DRM
  connector object ID, canonical sysfs connector path and its
  device/inode identity. Missing inventory means no authority; connector
  text alone is insufficient.

The serde field order is part of the shell-control wire format
(`Message::NativeOutputInventory`, protocol 0.37): fields may only be
appended, with a protocol minor bump. Tests are inline in `src/lib.rs` and
pin the postcard body and the JSON keys.

## Dependents

`tuna-shell-control` (re-exports the type and carries it on the wire),
`tuna-backlight`, `tuna-compositor`, `tuna-shell-host`, `tuna-shell-gtk`.

Stability: internal, not published, no SemVer promise — see the
[API stability policy](../../docs/api-stability.md).
