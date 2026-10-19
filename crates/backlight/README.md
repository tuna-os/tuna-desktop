# tuna-backlight

Bounded kernel backlight observations shared by the shell and compositor:
which raw sysfs backlight belongs to which compositor-owned DRM connector,
its range, and post-write readback. Reads confer no restore or policy
authority; callers decide what an observation admits.

## Public API (`src/lib.rs`)

- `inventory(root, outputs)`: raw backlights whose canonical device
  ancestry is exactly one owned connector in `outputs`
  (`tuna_shell_control::NativeOutputInfo`). No first-device fallback;
  duplicate interfaces on one connector withhold capability; more than 64
  entries rejects the whole inventory.
- `readback(device)`: actual value after a write; fails when the connector,
  backlight identity, type or range changed.
- `owner_current(owner)`: the owned connector object is still the same
  connected, enabled sysfs object.
- `scalar(path)`: bounded, non-blocking regular-file `u32` read.
- `Device` (`minimum`, `relative`, `absolute` use GNOME 51's floor:
  `max(1, max_brightness / 100)`, or zero for raw backlights below 99) and
  `BacklightKind`.

Tests are inline in `src/lib.rs`.

## Dependents

`tuna-shell-gtk` (brightness policy and `org.gnome.Shell.Brightness`).

## Docs

[Brightness contract](../../docs/brightness-contract.md).
