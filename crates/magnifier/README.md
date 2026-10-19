# tuna-magnifier

Shared magnifier configuration for `tuna-shell-gtk` and `tuna-compositor`.
The shell mirrors supported GNOME accessibility settings into a session JSON
snapshot; the compositor consumes that snapshot. This is a file boundary,
not a control-protocol message or a renderer.

## Public API

- `TrackingMode` and `ScreenPosition` map the supported GNOME enum values.
- `MagnifierConfig` carries enablement, magnification factor, screen position,
  pointer/focus/caret tracking, edge scrolling and lens mode. `sanitized()`
  clamps factors to the supported range; `active()` tests whether resampling
  is needed. `DEFAULT_MAG_FACTOR`, `MIN_MAG_FACTOR` and `MAX_MAG_FACTOR`
  expose the factor defaults/range.
- `path()` resolves `$XDG_RUNTIME_DIR/tuna-magnifier.json`, with a UID-suffixed
  temporary-file fallback for a bare harness. `load()` returns sanitized
  defaults when the file is absent, unreadable or invalid; `mtime()` exposes
  its modification time. `save()` writes a temporary file then renames it.

GSettings/dconf remains persistent configuration; the shell republishes the
snapshot at startup and on changes. The JSON file is session-scoped.

## Dependents and limits

Both [the GTK shell](../shell-gtk) and [the compositor](../compositor) depend
on this crate. The configuration crate itself does not prove on-screen
magnification, caret tracking, hardware behavior or lock-screen isolation.
Cross-hairs and color filters are outside this configuration slice. See
[accessibility behavior and remaining evidence](../../docs/accessibility-shell.md),
[the settings inventory](../../docs/settings-desktop-inventory.md) and
[the source/tests](src/lib.rs) for exact contracts.
