---
created_date: "2026-09-30"
document_status: final
closed_date: "2026-09-30"
---

# Context: 20260929180654-154da51a-keyboard-access

## Current State Analysis

The shell has exactly one keyboard path — seat keyboard to `ShellHost::on_key` (`crates/shell-host/src/panel.rs:1660`) via `XkbFeed` (`crates/shell-host/src/keyboard.rs:1-156`) — resolving only Text, Erase, Submit, and Dismiss. There is no focus model anywhere (no cursors, no selection state, no return stack), presses are pointer-only, and the dock/banner surfaces take no keyboard focus (`KeyboardInteractivity::None`). Paint has a reusable selection pattern (overview/switcher `BLOCK_SELECTED` + 4px `ACCENT`) but no focus ring; paint keys carry no focus field. No keymap document exists. The plan extends this funnel with vocabulary, hangs cursors and rings off it, and documents the result — one input path throughout.

## Per-Task Technical Notes

### Task: Key vocabulary and routing

- `crates/shell-host/src/keyboard.rs` (156 lines) — navigation vocabulary (arrows, Tab/Shift+Tab, F6, Enter, new chords) alongside Text/Erase/Submit/Dismiss.
- `crates/shell-host/src/panel.rs` (`on_key`, ~1660-1700) — route new actions; existing gating unchanged.
- Collision tests: shell-host tests-mod plus `crates/compositor/tests/windows.rs` chord pins.

**Complexity**: Medium
**Token estimate**: ~8k
**Agent strategy**: Single agent, sequential execution — vocabulary first, then routing, then collision tests.

### Task: Panel and dock keyboard focus

- `crates/shell-host/src/panel.rs` — `ShellFocus` cursors, F6 cycling, Enter activation via pointer-equivalent calls.
- `crates/shell-host/src/dock.rs` + paint passes — ring paint in accent style; `PanelPaintKey`/`DockPaintKey` gain focus fields.
- Interactivity: dock `None` to `OnDemand`.

**Complexity**: High
**Token estimate**: ~12k
**Agent strategy**: Single agent, sequential execution — cursors, then rings, then key extension.

### Task: Popups, overview navigation, and Escape return

- `crates/shell-host/src/popup.rs` (row cursor), `overview.rs` (result cursor), `panel.rs` (return stack, Escape split).
- Trap tests: focus after surface close always lands live.

**Complexity**: High
**Token estimate**: ~12k
**Agent strategy**: Single agent, sequential execution — popup rows, then overview results, then Escape unwind.

### Task: Keymap document and scripted key-only run

- New keymap doc (location per repo docs convention — check `docs/` at implement time).
- Scripted run test through `on_key` injection in the headless harness.

**Complexity**: Low
**Token estimate**: ~5k
**Agent strategy**: Single agent, sequential execution — doc first, then scripted run.

## Testing Strategy

The scripted key-only run carries the headline metric through `on_key` injection: calendar open, dock app launch, indicator menu open. Per task: vocabulary gets resolution unit tests plus collision pins; focus gets cursor-movement tests and buffer-sampled ring tests with an enumeration guard (any focusable surface without a ring fails); Escape gets unwind and trap tests. Heaviest coverage sits on the focus model and Escape unwind, where the no-trap guarantee lives. Deliberate gap: no AT-SPI bridge (a non-goal) and no physical-keyboard timing.

## Project References

- Spec: keyboard-access (`20260929180654-154da51a-keyboard-access`) — scope, metrics, non-goals.
- Prior plans consulted: multi-monitor plan (tick/verify/changelog loop precedent).
- Repo root: `/home/ubuntu/dev/rust-wayland-desktop` (registered name `rust-wayland-desktop`).
- Design documents referenced: none — the spec carries no design references.

## Token Management Strategy

| Tier | Token Budget | Agent Strategy |
|------|-------------|----------------|
| Low | ~10k | Single agent, sequential |
| Medium | ~25k | 2-3 parallel agents |
| High | ~50k+ | Parallel analysis, sequential integration |

Task estimates: vocabulary ~8k (Medium, single agent), panel/dock focus ~12k (Medium, single agent), popups/Escape ~12k (High, single agent), doc+run ~5k (Low, single agent). Total ~37k across four sequential tasks.

## Migration Notes

No migration needed. Keyboard focus is additive UI state; pointer paths and existing key behavior are unchanged. The keymap document is new.

## Performance Considerations

Focus navigation is local state updates plus repaints on cursor moves only (paint keys carry the cursor, so the steady state costs one comparison per tick). No wire traffic, no revision bumps, no per-frame work.
