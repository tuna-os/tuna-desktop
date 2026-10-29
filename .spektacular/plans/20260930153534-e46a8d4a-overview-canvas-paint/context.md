---
created_date: "2026-09-30"
document_status: final
closed_date: "2026-09-30"
---

# Context: 20260930153534-e46a8d4a-overview-canvas-paint

## Current State Analysis

The overview pipeline exists but can open blank. The shell host builds an
`OverviewCanvas` (pure pixel logic, unit-tested) and an `OverviewSurface`
holding the Wayland layer-surface plus its configured size
(`crates/shell-host/src/panel.rs:509-527`). `update_overview`
(`panel.rs:1147-1228`) creates the overlay surface on open, stores size
from `LayerSurfaceEvent::Configure` (`panel.rs:3486-3498`), and paints
only once the size is nonzero (`panel.rs:1193-1200`). The compositor
sends a bare configure on map and real geometry on commit
(`crates/compositor/src/layer.rs:67-82`, wired at `lib.rs:244-246`),
draws layer surfaces above windows (`runtime.rs:692`), and sends them
frame callbacks (`runtime.rs:747`). The shell's control-paced loop
drives updates every pass (`panel.rs:3862-3868`), but the pure-panel
fallback loop only refreshes panel status (`panel.rs:3877-3883`).
Existing overview tests cover the model only
(`crates/shell-host/tests/live_roundtrip.rs`); the live socketpair
harness with the real in-process `TestCompositor`
(`compositor/src/lib.rs:346`) is proven by the panel-paint test
(`panel.rs:5727-5775`).

## Per-Task Technical Notes

### Task: Failing overview-paint regression test

- `crates/shell-host/src/panel.rs:~5727` — new `#[test]` beside
  `paint_panel_surface_includes_indicator_pixels`, same imports
  (`TestCompositor`, socketpair, `attach_host`, `pump_server`).
- `crates/shell-host/src/panel.rs:~4031` — `test_host()` gives a
  side-effect-free host; open with `host.model.set_overview_open(true)`
  (pattern at `panel.rs:4897`), then `host.update_overview(None)` and
  `pump_server` rounds (`PUMP_ROUNDS`).
- Read `host.overview` backing pixels (pattern: `backing_pixels` at
  `panel.rs:5741`); assert non-`BG` count is nonzero so the test fails
  pre-fix. Seed model windows via existing model helpers.

**Complexity**: Medium
**Token estimate**: ~25k
**Agent strategy**: Mirror the panel-paint test shape exactly; run
`cargo test -p roost-shell-host live::` to confirm fail-then-suite-green.

### Task: Diagnose and fix the open-to-painted break

- `crates/shell-host/src/panel.rs:3877-3883` — first suspect: fallback
  loop never calls `update_overview`; confirm which loop the prototype
  uses before touching code.
- `crates/shell-host/src/panel.rs:3486-3498` — second suspect:
  configure sizing; `panel.rs:1193-1200` — third: paint gate.
- No compositor changes expected; no `overview.rs` changes allowed.

**Complexity**: Medium
**Token estimate**: ~30k
**Agent strategy**: Change one suspect at a time, re-running the new
test; stop at the first green. Never weaken the test to reach green.

### Task: Resize, empty-state, and typing variants

- Same live test: `comp.state.set_output_size` new size + pump + assert
  repaint (`compositor/src/lib.rs:393`); empty model open asserting
  backdrop/favorites pixels; `host.on_key` sequence asserting complete
  query text.
- Keep pump budgets bounded (`PUMP_ROUNDS`).

**Complexity**: Low
**Token estimate**: ~20k
**Agent strategy**: Extend the one test or add focused siblings beside it.

### Task: Workspace gates green

- `cargo fmt --all`, then
  `cargo clippy --workspace --all-targets -- -D warnings`, then
  `cargo test --workspace`. Fix fallout only, no scope growth.

**Complexity**: Low
**Token estimate**: ~10k
**Agent strategy**: Run in order; each gate must pass unmodified.

## Testing Strategy

Per-task granularity: the regression test task owns the fail-first
proof (pixel assertions on the committed buffer, failing for the right
reason); the fix task owns turning it green without touching the
painter; the variants task owns resize/empty/typing coverage through
the real key and configure paths; the gates task owns the repo-wide
green baseline. No test is mocked at the protocol layer — the
socketpair plus real compositor exercises configure, commit, and paint
end to end in-process.

## Project References

- Spec: `20260930153534-e46a8d4a-overview-canvas-paint` (approved;
  read via `spektacular spec file read`).
- Draft spec: `20260929180544-421dbfd8-overview-app-launch` (adjacent
  launch scope, untouched).
- Knowledge: `gotchas/x11-frame-pump.md` in store `rust-wayland-desktop`
  (symptom-class analogue).
- Repo root: `/home/ubuntu/dev/rust-wayland-desktop` (registered repo
  `rust-wayland-desktop`, product-and-implementation).

## Token Management Strategy

| Tier | Token Budget | Agent Strategy |
|------|-------------|----------------|
| Low | ~10k | Single agent, sequential |
| Medium | ~25k | 2-3 parallel agents |
| High | ~50k+ | Parallel analysis, sequential integration |

Total estimate for this plan: ~85k (one Medium fix task dominates).

## Migration Notes

N/A — no data, config, or protocol migration involved.

## Performance Considerations

Painting stays on the existing update pass with paint-key caching, so
no extra frames are introduced. The typing variant of the regression
test guards the no-dropped-keystrokes constraint directly. The live
test uses bounded pump rounds with no sleeps, keeping suite time flat.
