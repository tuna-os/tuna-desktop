# tuna-magnifier

Shared magnifier configuration for `tuna-shell-gtk` and `tuna-compositor`.
The shell mirrors supported GNOME accessibility settings into a session JSON
snapshot; the compositor consumes that snapshot. This is a file boundary,
not a control-protocol message or a renderer.

## Public API

- `TrackingMode` and `ScreenPosition` map the supported GNOME enum values.
- `MagnifierConfig` carries every `org.gnome.desktop.a11y.magnifier` key:
  enablement, magnification factor, screen position, pointer/focus/caret
  tracking, edge scrolling, lens mode, cross-hairs (shown, thickness, color,
  opacity, length, clip) and the color effects (invert lightness, saturation,
  per-channel brightness and contrast). `cross_hairs_rgba()` parses the color
  like `Cogl.Color.from_string`.
- `zoom_step()` is gsd-media-keys' Super+Alt+=/- step (plus or minus one,
  rounded, `None` outside the schema range).
- `sanitized()` clamps values to the schema ranges; `active()` is the master
  switch (any factor, since effects and cross-hairs apply at 1.0 too).
  `DEFAULT_MAG_FACTOR`, `MIN_MAG_FACTOR` and `MAX_MAG_FACTOR` expose the
  factor defaults/range.
- `path()` resolves `$XDG_RUNTIME_DIR/tuna-magnifier.json`, with a UID-suffixed
  temporary-file fallback for a bare harness. `load()` returns sanitized
  defaults when the file is absent, unreadable or invalid; `mtime()` exposes
  its modification time. `save()` writes a temporary file then renames it.

GSettings/dconf remains persistent configuration; the shell republishes the
snapshot at startup and on changes. The JSON file is session-scoped.

## Dependents and limits

Both [the GTK shell](../shell-gtk) and [the compositor](../compositor) depend
on this crate; the compositor's `magnifier.rs` renders the live zoom region
and `focus_caret.rs` feeds it AT-SPI focus and caret events. This crate holds
configuration only. Live behavior is proven by `scripts/tuna-magnifier-proof`;
accepted differences from GNOME 51 (touch mapped through the zoom, caret and
window-centre fallback for non-AT-SPI apps, per-monitor regions, shell popover
focus untracked) and the remaining evidence are in
[accessibility behavior](../../docs/accessibility-shell.md),
[the settings inventory](../../docs/settings-desktop-inventory.md) and
[the source/tests](src/lib.rs).

Stability: internal, not published, no SemVer promise; the session JSON
file is a cross-process contract — see the
[API stability policy](../../docs/api-stability.md).
