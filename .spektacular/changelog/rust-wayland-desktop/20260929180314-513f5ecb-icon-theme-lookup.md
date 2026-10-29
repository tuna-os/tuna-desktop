---
created_date: "2026-09-29"
document_status: draft
project: rust-wayland-desktop
spec: 20260929180314-513f5ecb-icon-theme-lookup
plan: 20260929180314-513f5ecb-icon-theme-lookup
---

# Icon theme lookup in rust-wayland-desktop

Tray and dock slots now show real application artwork from the
system's GNOME icon themes instead of letter placeholders, with
attention states, accent-matched symbolic icons, and live
repaints on theme switches.

> Derived from project rust-wayland-desktop (plan store), spec/plan 20260929180314-513f5ecb-icon-theme-lookup. See the project-level record for the full feature.

## What changed in this repo

- New `icons` module in shell-host: Freedesktop theme lookup with
  raster decode, SVG render, symbolic re-tint, and bounded file
  refusal (`crates/shell-host/src/icons.rs`).
- Settings snapshot carries the desktop icon-theme key and the host
  caches resolved art keyed by theme, name, and size, synced on the
  slow tick (`settings.rs`, `panel.rs`).
- Tray fetch resolves named and attention icons through the cache
  (`watcher.rs`); dock paint resolves desktop-entry icons through
  the same cache with initial fallbacks (`dock.rs`).

## Why

GNOME apps expose only icon names; resolving them through the
user's theme is what makes the Roost bar and dock look correct
without pulling a GUI toolkit into the shell process.
