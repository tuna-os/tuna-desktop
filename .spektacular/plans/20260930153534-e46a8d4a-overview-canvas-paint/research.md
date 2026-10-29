---
created_date: "2026-09-30"
document_status: final
closed_date: "2026-09-30"
---

# Research: 20260930153534-e46a8d4a-overview-canvas-paint

## Alternatives considered and rejected

### Option A: Compositor render-loop redesign

Per-output scan-out and damage-tracked pacing. **Rejected**: the render
path already draws every layer-layout surface above windows
(`crates/compositor/src/runtime.rs:692`) and sends frame callbacks to
layer surfaces (`runtime.rs:747`); the render path is not the break,
and redesign is an explicit spec non-goal.

### Option B: Rewriting the canvas painter

Replacing `OverviewCanvas` in `crates/shell-host/src/overview.rs`.
**Rejected**: the painter is unit-tested pixel logic (backdrop, blocks,
selection border, favorites, query, failure marker, painted at
`crates/shell-host/src/panel.rs:1206-1216`); no evidence it is wrong.
The gate before it (`panel.rs:1196`, nonzero configured size) is the
suspect, not the painter.

### Option C: X11-surface handling for overview

Treating the overview like an X11 window. **Rejected**: the overview is
a layer-shell surface, not an X11 window; the x11 frame-pump gotcha
(`gotchas/x11-frame-pump.md`) does not apply, though the symptom class
(correct identity/focus, no pixels) matches and motivated the check.

### Option D: Model-semantics change

Changing what the overview model tracks. **Rejected** by spec
constraint: the model is the source of truth and stays untouched.

## Chosen approach — evidence

Reproduce-then-fix the narrowest break in the open-to-painted chain
with a failing live test first. Supporting evidence:

- Paint gate: `crates/shell-host/src/panel.rs:1193-1200` returns early
  until the configured size is nonzero; size arrives only via configure
  (`panel.rs:3486-3498`).
- Server sends bare configure on map, real geometry on commit
  (`crates/compositor/src/layer.rs:67-82`, wired at `lib.rs:244-246`).
- Control loop drives updates every pass (`panel.rs:3862-3868`); the
  pure-panel fallback loop calls only `update_panel_status`
  (`panel.rs:3877-3883`) — prime suspect #1.
- Live harness: `TestCompositor` is the real compositor in-process
  (`crates/compositor/src/lib.rs:346`); precedent test at
  `panel.rs:5727-5775`; existing overview tests cover model only
  (`crates/shell-host/tests/live_roundtrip.rs`).

## Files examined

- `crates/shell-host/src/panel.rs:1147` — `update_overview`: create,
  size, paint, commit.
- `crates/shell-host/src/panel.rs:3486` — Configure dispatch.
- `crates/shell-host/src/panel.rs:3862` — run loops.
- `crates/shell-host/src/overview.rs:52` — `OverviewCanvas` painter.
- `crates/compositor/src/layer.rs:67` — `arrange_after_commit`.
- `crates/compositor/src/layer.rs:164` — `layer_layout`.
- `crates/compositor/src/runtime.rs:645` — `render` with layer
  surfaces and frame callbacks.
- `crates/compositor/src/lib.rs:244` — commit handler wiring.
- `crates/compositor/src/lib.rs:346` — `TestCompositor`.
- `crates/shell-host/tests/live_roundtrip.rs` — model-only coverage.

## External references

None — all findings are first-party code plus the project's own
knowledge entry `gotchas/x11-frame-pump.md`.

## Prior plans / specs consulted

- `20260929180544-421dbfd8-overview-app-launch` (spec): owns
  app-launch; this plan stays out. Learned: launch feedback assumes a
  visible overview.
- `20260930054036-d6050cbc-xwayland-support` (prior plan/implement):
  fixed the compositor-side invisible-window class; its frame-loop pump
  is the model for the shell-side regression test.

## Open assumptions

See plan.md § Open Questions (run-loop identity; pump budget for
configures). If either proves wrong, implement must STOP and ask.

## Rehydration cues

- Re-read `panel.rs:1147-1228` then `panel.rs:3862-3885`; then
  `layer.rs:67-82`.
- Test pattern: copy `paint_panel_surface_includes_indicator_pixels`
  (`panel.rs:5727`), swap panel paint for overview open + pump +
  pixel assert.
- `cargo test -p roost-shell-host live::` runs the harness tests.

## Drafting assumptions

- Single registered repo, so every task carries
  `Repo: rust-wayland-desktop`; no cross-repo split needed.
- No design references on the spec (verified zero refs/unresolved),
  stated explicitly in Dependencies.
- Always-applied knowledge store is empty (verified); Conventions
  records codebase-demonstrated practice instead.
- Four tasks suffice; two extra minted ids were left unused rather
  than inventing filler tasks.
