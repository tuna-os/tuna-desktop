---
created_date: "2026-09-30"
document_status: final
closed_date: "2026-09-30"
---

# Context: 20260929180642-12b43038-multi-monitor

## Current State Analysis

Today the compositor owns exactly one output: `Runtime{ output: Output }` (`crates/compositor/src/runtime.rs:194`), created as `"roost-0"` (:249) with a single `create_global` (:268). Geometry for layer-surface arrange lives in `State::output_size` (`crates/compositor/src/lib.rs:79-81`), set once via `set_output_size` (:316-324), and `crates/compositor/src/layer.rs` arranges every layer surface against that single size (:40-65, :146-178). The shell paints panel (`crates/shell-host/src/panel.rs`), dock (`crates/shell-host/src/dock.rs`), and wallpaper (`crates/compositor/src/wallpaper.rs`, decode at :134) once globally. Window state lives in `crates/compositor/src/windows.rs` (1808 lines) with protocol state in `crates/compositor/src/state.rs` (904 lines). The plan grows an output inventory beside the single output — entry zero is today's path — and gives paint loops an output dimension.

## Per-Task Technical Notes

### Task: Output inventory in the compositor

- `crates/compositor/src/runtime.rs:189-233` — replace the single `output: Output` session field with an inventory (one entry per output holding size, primary flag, and surfaces); session build registers the first entry.
- `crates/compositor/src/lib.rs:79-81,316-324` — replace `output_size` with per-entry geometry; `set_output_size` becomes per-entry update.
- `crates/compositor/src/layer.rs:40-65,146-178` — arrange layer surfaces against the entry for their output instead of the single `state.output_size`.

**Complexity**: Medium
**Token estimate**: ~8k
**Agent strategy**: Single agent, sequential execution — inventory shape first, then arrange call sites.

### Task: Per-output shell paint and dock anchor

- `crates/shell-host/src/panel.rs` (5285 lines) — loop panel paint once per inventory entry instead of once globally.
- `crates/compositor/src/wallpaper.rs` (323 lines, decode at :134) — crop per output size (cover-crop) per entry.
- `crates/shell-host/src/dock.rs` (850 lines) — read the primary record for centering; re-anchor on primary change.

**Complexity**: Medium
**Token estimate**: ~8k
**Agent strategy**: Single agent, sequential execution — panel loop, then wallpaper crop, then dock anchor.

### Task: Hotplug window migration and tests

- `crates/compositor/src/windows.rs` (1808 lines) and `crates/compositor/src/state.rs` (904 lines) — on output removal, re-map that output's windows to a live output before dropping the entry.
- New/changed tests: virtual two-output layout test (panel, dock, wallpaper per output), hotplug add/remove preservation test, primary-switch dock test.

**Complexity**: High
**Token estimate**: ~12k
**Agent strategy**: Parallel analysis of window mapping and test scaffolding, sequential integration — migration first, then tests.

## Testing Strategy

Behavioural tests with virtual outputs carry both success metrics: two-output layout correctness and hotplug window preservation on add and remove. Per task: inventory task gets unit tests on add/remove and the one-entry single-output case; paint task gets the two-virtual-output layout test plus the primary-switch dock test; migration task gets the add/remove preservation test. Heaviest coverage sits on migration and inventory, where the spec's no-loss guarantee lives. Deliberate gap: no live-GPU multi-head tests — CI has one virtual backend; virtual outputs stand in.

## Project References

- Spec: multi-monitor (`20260929180642-12b43038-multi-monitor`) — scope, success metrics, non-goals.
- Prior plans consulted: session-lock plan (compositor-owned state discipline), icon-theme-lookup plan (per-size artwork precedent).
- Repo root: `/home/ubuntu/dev/rust-wayland-desktop` (registered name `rust-wayland-desktop`).
- Design documents referenced: none — the spec carries no design references.

## Token Management Strategy

| Tier | Token Budget | Agent Strategy |
|------|-------------|----------------|
| Low | ~10k | Single agent, sequential |
| Medium | ~25k | 2-3 parallel agents |
| High | ~50k+ | Parallel analysis, sequential integration |

Task estimates: inventory ~8k (Medium strategy, single agent), paint ~8k (Medium strategy, single agent), migration ~12k (High strategy, migration first then tests). Total ~28k across three sequential tasks.

## Migration Notes

No migration needed. The single-output path becomes inventory entry zero; existing sessions and tests see identical behaviour with one output. No stored state changes shape.

## Performance Considerations

One frame budget is shared across outputs; per-output paint must not stall siblings (assert via test timing bounds, not live frames). Wallpaper decode happens per output size — crops, not fresh decodes per frame. Window migration is O(windows on the removed output) at removal time only.
