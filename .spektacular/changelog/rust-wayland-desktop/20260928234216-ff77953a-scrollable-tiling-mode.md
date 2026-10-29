---
created_date: "2026-09-29"
document_status: draft
project: rust-wayland-desktop
spec: 20260928234216-ff77953a-scrollable-tiling-mode
plan: 20260928234216-ff77953a-scrollable-tiling-mode
---

# Scrollable-tiling mode

Super+Shift+T now switches the whole session into a scrollable strip of
window columns with 16px gaps: Super+arrows move focus, Super+Shift+arrows
move windows, Super+R cycles column widths, and the pointer wheel scrolls
the strip. Toggling back restores the floating desktop exactly. The
floating session stays the default and behaves as before.

> Derived from project rust-wayland-desktop, spec/plan 20260928234216-ff77953a-scrollable-tiling-mode. See the project-level record for the full feature.

## What changed in this repo

- `crates/compositor`: session mode flag on the window manager, strip
  layout with column geometry and width presets, mode-branched chords,
  wheel-axis input plumbing, and forced strip membership for
  map/maximize/fullscreen/tile paths.
- `crates/compositor/tests`: integration tests over real protocol clients
  for toggle round trips, geometry, chords, presets, and membership.
- Proof harness `scripts/roost-scroll-proof` plus a CI job asserting strip
  geometry in screenshots.
- Follow-up product rename (issue #20): `rwd-*` crates, binaries, scripts,
  and docs became `roost-*`; behavior unchanged.

## Why

Column-oriented users get a niri-style workflow inside the session while
the default floating desktop is untouched — one chord switches disciplines
without restarting anything or changing the shell protocol.
