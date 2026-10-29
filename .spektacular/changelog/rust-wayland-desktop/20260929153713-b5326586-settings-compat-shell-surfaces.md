---
created_date: "2026-09-29"
document_status: draft
project: rust-wayland-desktop
spec: 20260929153713-b5326586-settings-compat-shell-surfaces
plan: 20260929153713-b5326586-settings-compat-shell-surfaces
---

# Shell settings, dock, indicators, and wallpaper for Roost

The Roost session now follows GNOME Settings (clock format and
wallpaper), shows a working dock with app tracking and folder
stacks, hosts tray indicators with menus in the top bar, and paints
the Settings wallpaper behind everything.

> Derived from project rust-wayland-desktop, spec/plan 20260929153713-b5326586-settings-compat-shell-surfaces. See the project-level record for the full feature.

## What changed in this repo

- `shell-host` gained a gio-backed settings snapshot feeding the bar
  clock and wallpaper publish, calendar/status popups on the panel
  surface, a `dock` module (favorites, running marks, switch/close,
  folder stacks), and a `watcher` module owning the
  StatusNotifierWatcher role with dbusmenu popups.
- The control protocol reached 0.5 with `CloseWindow` for polite
  dock-initiated close, drained by the hub and ticked by the runtime.
- The compositor draws a wallpaper memory element behind windows
  from a runtime-dir drop file, skipping it under the red recovery
  overlay.
- CI installs `libglib2.0-dev` in nested-session jobs for the gio
  build; new unit, live-bus, and round-trip tests cover each slice.

## Why

Roost reuses GNOME apps and Settings instead of cloning them: one
shared settings universe, native shell surfaces, no GTK in the
shell process.
