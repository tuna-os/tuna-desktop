---
created_date: "2026-09-30"
document_status: final
closed_date: "2026-09-30"
---

# Context: 20260930163234-5de5360c-rhai-extensions

## Current State Analysis

No extension system exists. The shell host paints system tiles from
`TileSet` (`crates/shell-host/src/tiles.rs:309`), dock items in
`dock.rs` (no badge concept today), and indicator items shaped like
`watcher.rs:60 IndicatorItem`. There is no scripting dependency
anywhere in the workspace, but `rhai 1.26.1` sits in the offline cargo
cache so the new dependency resolves without network. Script
directories follow the existing XDG `data_dir()` pattern
(`favorites.rs:104`); there is no `notify` crate cached, so reload is
mtime polling on the update pass (`panel.rs` update passes).

## Per-Task Technical Notes

### Task: Sandboxed extension host with sample cell

- `crates/shell-host/Cargo.toml` — add `rhai = "1.26"`.
- New `crates/shell-host/src/extensions.rs` — `ExtensionHost`,
  `ScriptOutput`, `ExtensionState`, API module, budget hook, drive.
- `crates/shell-host/src/lib.rs` — `pub mod extensions;`.
- Sample `.rhai` script + `docs/extensions.md` (follows
  `docs/keymap.md` precedent).

**Complexity**: High
**Token estimate**: ~50k
**Agent strategy**: Confirm the cached rhai 1.26.1 API first
(progress hook, module registration); temp-dir script dirs in tests.

### Task: Hostile battery and clean disable

- `crates/shell-host/src/extensions.rs` in-module tests: fs/net/
  spawn attempts, syntax error, infinite loop; disable-note
  assertions; host-alive assertions.

**Complexity**: Medium
**Token estimate**: ~25k
**Agent strategy**: One test per hostile class; never weaken an
assertion to reach green.

### Task: Panel, dock, and indicator integration

- `crates/shell-host/src/panel.rs` — strip cells from cache;
  `crates/shell-host/src/dock.rs` — item badges;
  `crates/shell-host/src/watcher.rs` caller — reaction updates.
- Grep painters for engine calls to prove the off-path constraint.

**Complexity**: Medium
**Token estimate**: ~30k
**Agent strategy**: Cache-reading branches only; painter shapes stay.

### Task: Hot reload and workspace gates

- `crates/shell-host/src/extensions.rs` — mtime poll in drive;
  reload test with real file edits in a temp dir.
- Gates: formatting check, lint with warnings denied, full suite.

**Complexity**: Low
**Token estimate**: ~15k
**Agent strategy**: Poll, don't subscribe; bounded drives in tests.

## Testing Strategy

Per-task granularity: the host task owns cell output, syntax-error
disable, and hang disable; the battery task owns fs/net/spawn denial
plus visible notes with the host alive; the integration task owns
cache-to-paint assertions plus the off-path grep review; the reload
task owns edit-applies-without-restart with real files and the
repo-wide green baseline. Temp dirs everywhere; no HOME effects.

## Project References

- Spec: `20260930163234-5de5360c-rhai-extensions` (approved; read via
  `spektacular spec file read`).
- Draft spec: `20260929180708-225d0ebc-rhai-extensions` (historical
  scope reference, superseded).
- Repo root: `/home/ubuntu/dev/rust-wayland-desktop` (registered repo
  `rust-wayland-desktop`, product-and-implementation).

## Token Management Strategy

| Tier | Token Budget | Agent Strategy |
|------|-------------|----------------|
| Low | ~10k | Single agent, sequential |
| Medium | ~25k | 2-3 parallel agents |
| High | ~50k+ | Parallel analysis, sequential integration |

Total estimate for this plan: ~120k (one High host task dominates).

## Migration Notes

N/A — greenfield module; no data, config, or protocol migration.

## Performance Considerations

Scripts run off the paint/event paths on poll cadence with cached
outputs, so frame timing is unaffected. Operation budgets bound
per-call script cost; mtime polling is a directory stat per drive,
negligible next to painting. The typing-responsiveness constraint is
guarded by keeping script execution out of the input path entirely.
