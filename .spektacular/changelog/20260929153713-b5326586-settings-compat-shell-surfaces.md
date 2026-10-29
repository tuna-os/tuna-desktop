---
created_date: "2026-09-29"
document_status: final
closed_date: "2026-09-29"
---

# Settings-compatible shell surfaces

Roost now honors shared GNOME desktop settings instead of owning a
parallel settings universe, and reimplements the top bar, app dock,
and app indicators as native pixel-painted shell surfaces. GNOME
users keep their Settings workflows and tray-dependent apps, and the
session stops looking like a bare developer preview.

## What was built

- **Settings snapshot**: the shell reads `org.gnome.desktop.interface`
  clock format and `org.gnome.desktop.background` wallpaper URI
  through gio on the slow tick; the bar clock renders 12/24-hour
  accordingly and degrades to previous output with no bus or schema.
- **Bar popups**: month calendar from the clock, control menus from
  the service tiles, on the existing layer surface with shell-popup
  dismissal and pointer plumbing.
- **Dock**: pinned launch, running marks, switch, and polite close
  (new `CloseWindow` control message, protocol 0.5) on a bottom
  layer surface, plus macOS-like folder stacks (`stack:<dir>`
  favorites opening a launchable grid, plain files via `xdg-open`).
- **Indicator host**: owns the `org.kde.StatusNotifierWatcher` role,
  paints item icons left of the sound tile, and opens one-level
  dbusmenu popups (`Activate`/`Event` on press).
- **Wallpaper hook**: the compositor draws the Settings wallpaper
  behind everything from a runtime-dir drop file the shell
  publishes; failures fall back to the solid clear.

## Deviations from the plan

- Folder stacks were added per direct user request, recorded in the
  dock task entry rather than as a new task.
- Watcher D-Bus signal emission is unwired in v1 (single-host
  session); dbusmenu nesting flattens to one level.
- CI's nested-session jobs needed `libglib2.0-dev` for the gio
  build; icon theme lookup moved to roadmap issue #21.

Verified: `cargo fmt`, `cargo clippy --workspace --all-targets --
-D warnings`, full `cargo test --workspace`, live stub-indicator
test over a private bus, and a fully green CI run (check, journey,
app-content, scroll-proof, docs-shots).
