---
created_date: "2026-09-30"
document_status: draft
project: rust-wayland-desktop
spec: 20260930153534-e46a8d4a-overview-canvas-paint
plan: 20260930153534-e46a8d4a-overview-canvas-paint
---

# Overview always paints in panel-only sessions

> Derived from project rust-wayland-desktop (spec/plan 20260930153534-e46a8d4a-overview-canvas-paint). See the project-level record for the full feature.

Pressing Super now always shows the overview — window blocks, the
selection border, and favorites — even when the shell runs without a
control socket. Previously the panel-only loop refreshed panel status
alone, so the overview stayed blank in those sessions.

## What changed in this repo

- `crates/shell-host/src/panel.rs`: extracted the shared
  `update_shell_surfaces` pass (overview, switcher, panel status,
  banners) and routed both run loops through it; the fallback loop now
  pumps Wayland at the same cadence instead of blocking on dispatch
  alone.
- Five live regression tests beside the existing paint tests, covering
  paint on open, the panel-only iteration, resize repaint, empty-state
  commit, and typing during paint.

## Why

The overview model already opened correctly; only the paint pass never
ran in panel-only mode. Running the same update pass everywhere closes
that gap with no changes to the painter or the compositor.
