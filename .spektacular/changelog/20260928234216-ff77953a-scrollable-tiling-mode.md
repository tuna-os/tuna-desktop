---
created_date: "2026-09-29"
document_status: draft
---

# Scrollable-tiling mode

## What was built

The session gained a whole-session scrollable-tiling mode beside the
default floating desktop. Pressing Super+Shift+T turns all windows into
a horizontal strip of columns (16px gaps, half-width default) that is
navigated with Super+arrows (focus), Super+Shift+arrows (move windows),
Super+R / Super+Shift+R (column width presets 1/3–1/2–2/3), and the
pointer wheel. Maximized, fullscreen, tiled, and dialog windows join as
columns instead of covering the screen. Toggling back restores every
window to its exact floating geometry. A scripted nested-session proof
with screenshot geometry asserts plus a CI job locks the behavior in.

## Why it matters / what it enables

Column-oriented users (niri/PaperWM workflow) get their layout without
leaving the session, while the GNOME-style floating desktop stays the
default and behaves exactly as before. The mode is compositor-local:
no protocol change, and the shell needs no updates.

## Deviations from the plan

- A `WindowLayout::Strip` per-window variant was added as the stash
  vehicle through the existing stash-once/restore path; the mode flag
  itself stays manager-level as planned.
- Strip geometry moved from uniform-width indexing to prefix sums so
  mixed preset widths lay out correctly; offset clamping has a single
  choke point in the strip relayout.
- Proof pixel asserts locate columns relative to the first green run
  (Chromium paints a small chrome margin, so absolute screen coordinates
  are unreliable).
- Product rename RWD -> Roost landed after implementation (issue #20):
  crates, binaries, scripts, and docs now carry `roost-*` names, e.g.
  `scripts/roost-scroll-proof`. Behavior is unchanged.
