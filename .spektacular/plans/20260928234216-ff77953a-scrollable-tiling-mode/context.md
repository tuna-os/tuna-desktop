---
created_date: "2026-09-29"
document_status: final
closed_date: "2026-09-29"
---

# Context: 20260928234216-ff77953a-scrollable-tiling-mode

## Current State Analysis

Floating ("gnome") is the only layout: one workspace arranger in the compositor manager with cascade placement, stash-once maximize/tile/fullscreen, and an evdev chord table (no T/R bindings, no pointer-axis handling anywhere). Shell-host mirrors truth over the v0.4 control protocol and owns overview/switcher/search; its key feed is overview-gated, so global chords live compositor-side. No scroll/strip/mode/preset code exists; shell-host stays untouched.

## Per-Task Technical Notes

# context.md task detail — scrollable-tiling-mode (all files in repo rust-wayland-desktop)

### Task: Strip layout function with session mode flag

**Complexity**: Medium
**Agent strategy**: single agent, sequential (one task = one checkout area; tasks within a milestone parallelize only if dependency-free)

Mode flag on `WindowManager` (`crates/compositor/src/windows.rs:52`, beside `ManagedWindow`), default `Gnome`. Strip function next to the floating arranger: build from `work_area()` (`windows.rs:680-686`), column x-accumulator modeled on niri `column_xs()` (`references/niri/src/layout/scrolling.rs:2333`), width via `(work.w − gaps) × p − gaps` (`scrolling.rs:4536`). Toggle path: flip flag → re-run layout over all windows → `configure()` each (`windows.rs:462-501`); mirror `reconcile` tick convergence (`windows.rs:205-250`). Proportions: gaps 16, presets [1/3, 1/2, 2/3], default 1/2 (spec; niri upstream default-config.kdl verified).

### Task: Toggle and geometry integration tests

**Complexity**: Medium
**Agent strategy**: single agent, sequential (one task = one checkout area; tasks within a milestone parallelize only if dependency-free)

Mirror `crates/compositor/tests/windows.rs`: `two_windows()` fixture (`:404`), `connect`/`map_toplevel`/`sync_client`/`pump` (`:309-376`), `layout_windows` fixture (`:818`), `has_state` wire matcher (`:85-90`); layout precedents `maximize_fills_work_area_and_restores` (`:825`), `tile_halves_toggle_and_fullscreen_cover` (`:859`). Assert via `geometry()` (`windows.rs:893-895`).

### Task: Mode-branched chord handling

**Complexity**: Medium
**Agent strategy**: single agent, sequential (one task = one checkout area; tasks within a milestone parallelize only if dependency-free)

Branch inside `on_workspace_key` (`windows.rs:1146-1187`); T=20/R=19 consts beside the evdev table (`windows.rs:961-998`); shift_held pattern from `track_workspace_modifiers` (`windows.rs:874-880`); consume/swallow per Tab/F4 (`windows.rs:1091-1113`); keep Super-tap disarm intact (`windows.rs:1063-1066`); `is_arrow()` + `toggle_tiled()` template (`windows.rs:1190-1204`). Gnome branch must stay byte-identical (journeys run in gnome default).

### Task: Wheel axis plumbing with keys fallback

**Complexity**: Medium
**Agent strategy**: single agent, sequential (one task = one checkout area; tasks within a milestone parallelize only if dependency-free)

New `ManagerInput::Axis` variant (`windows.rs:939-954` has Key/Motion/Button only); extend `translate_input` (`windows.rs:1230-1255`); accumulator shaped on niri `scroll_tracker.rs` (40 lines) fed from `on_pointer_axis`-equivalent (`references/niri/src/input/mod.rs:3109`); offset folds into `StripState.offset`. First verify axis delivery on the nested path (EGL-only backend per `gotchas/winit-backend-requires-egl`); absent → record keys-only fallback, keep stub.

### Task: Forced strip membership intercepts

**Complexity**: Medium
**Agent strategy**: single agent, sequential (one task = one checkout area; tasks within a milestone parallelize only if dependency-free)

Intercept `map()` (`windows.rs:302-329`, replaces cascade `DEFAULT 800x600`/`CASCADE_STEP`), `set_maximized/set_fullscreen/set_tiled` (`windows.rs:712-750`), and `drain_window_requests` (`lib.rs:169-172`, `WindowRequest:90-105`); isolate-then-size per niri `set_fullscreen/set_maximized` (`scrolling.rs:2869-2900`); dialogs: replace `place_transient` centering (`windows.rs:335-361`) with a strip rule. Advertise via single `configure` path; watch fullscreen-state-vs-column rendering.

### Task: Preset width cycling

**Complexity**: Medium
**Agent strategy**: single agent, sequential (one task = one checkout area; tasks within a milestone parallelize only if dependency-free)

Step `preset_column_widths` with ±1px tolerance per niri `toggle_width` (`scrolling.rs:2630`, `Column::toggle_width :4927`); stash-once so `restore` survives cycling (`apply_layout :755-773`, `restore_window :738-750`); clear-on-manual-resize per `set_column_width` (`scrolling.rs:4990`).

### Task: Scroll proof harness and CI job

**Complexity**: Medium
**Agent strategy**: single agent, sequential (one task = one checkout area; tasks within a milestone parallelize only if dependency-free)

Extend `scripts/rwd-journey` pattern (Xvfb + xdotool + scrot + marker asserts); new script or flag for scroll geometry shots; CI job beside journey/app-content; asserts: 16px gaps, 1/2 default width, 3-column overflow, preset widths after Super+R.

### Task: Human visual review of scroll frames

**Complexity**: Medium
**Agent strategy**: single agent, sequential (one task = one checkout area; tasks within a milestone parallelize only if dependency-free)

Frames from the proof artifacts (`journey-artifacts/`-style dir); project convention is human review of journey frames. No code.

## Testing Strategy

Unit: strip math + chord table. Integration over real protocol clients (fixture + bounded pump, no sleeps): toggle round trips, motion, presets, membership. Scripted nested proof with screenshot geometry asserts + CI job. Regression floor: gnome journeys, focus/clipboard/switcher, workspace suites stay green. Human review of proof frames for visual quality.

## Project References

- Spec: `specs/20260928234216-ff77953a-scrollable-tiling-mode.md` (final) — binding contract.
- Repos: `rust-wayland-desktop` at `/home/ubuntu/dev/rust-wayland-desktop` (single repo; all work lands here).
- References (read-only models): `references/niri` (`src/layout/scrolling.rs`, input axis, default-config.kdl proportions).
- Knowledge: smithay-0.7.0 pin entries, `gotchas/winit-backend-requires-egl.md` (EGL-only nested backend; wheel-vs-keys fallback).
- Prior: shell-workflow-parity plan (compositor-authority, token intents, rejected D-Bus/per-motion IPC).

## Token Management Strategy

| Tier | Token Budget | Agent Strategy |
|------|-------------|----------------|
| Low | ~10k | Single agent, sequential |
| Medium | ~25k | 2-3 parallel agents |
| High | ~50k+ | Parallel analysis, sequential integration |

Implementation tasks here are Medium: one agent per task, sequential within dependency chains; M2 tasks parallelize after the strip foundation lands.

## Migration Notes

None — no stored-state migration per spec constraint; gnome mode is the default and behaves exactly as today.

## Performance Considerations

Toggle must re-layout all windows within 2 frames; `configure` per window per toggle is the cost center — measured in the proof harness, with a STOP-and-ask if over budget. Strip offset math is O(columns) per frame; no per-frame allocation discipline is imposed beyond existing manager practice.
