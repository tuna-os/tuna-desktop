---
created_date: "2026-09-30"
document_status: final
closed_date: "2026-09-30"
---

# Research: 20260929180642-12b43038-multi-monitor

## Alternatives considered and rejected

### Option A: Mirrored outputs

Same pixels everywhere: trivially "correct" but useless; the spec demands per-output panel/dock/wallpaper.

**Rejected**: fails the spec's core requirement.

### Option B: Shell-side output database

The shell tracks displays itself alongside the compositor's tracking.

**Rejected**: violates the spec constraint (use the compositor's tracking, no parallel database); the inventory lives in the compositor (`crates/compositor/src/runtime.rs`).

### Option C: Per-output shell processes

One shell process per output.

**Rejected**: multiplies every bus name, cache, and queue; the host is already multi-surface capable. One shell, per-output surfaces.

## Chosen approach — evidence

- Today exactly one output: `Runtime{output: Output}` (`runtime.rs:194`), created as `"roost-0"` (:249) with one `create_global` (:268). The plan grows an output inventory beside it — never a second database.
- Per-output layer surfaces for panel/dock/wallpaper from the inventory; wallpaper cover-cropped per output size (decode path at `wallpaper.rs:134`).
- Dock anchors to the primary output record; primary changes re-anchor.
- Hotplug add/remove updates the inventory live; removal migrates windows to a live output (none lost), tested with virtual outputs.

## Files examined

- `compositor/src/runtime.rs:62,194,249,268` — single output creation.
- `compositor/src/wallpaper.rs:134` — decode for per-size crops.
- `shell-host` panel/dock paint paths — single-surface assumptions to lift.
- `compositor/src/lib.rs:79-81,316-324` — single `output_size` geometry.
- `compositor/src/layer.rs:40-65,146-178` — single-output arrange.
- `compositor/src/windows.rs` (1808 lines), `compositor/src/state.rs` (904 lines) — window mapping for migration.
- Researcher session log for window/output mapping detail.

## External references

- Wayland `wl_output` globals — one per output; hotplug via global add/remove, the standard mechanism.
- Smithay output handling docs — matches in-tree usage.

## Prior plans / specs consulted

- session-lock plan: compositor-owned state discipline.
- icon-theme-lookup plan: per-size artwork precedent.
- multi-monitor spec: scope, metrics, non-goals.

## Open assumptions

- Virtual outputs in tests adequately stand in for hotplug (no GPU in CI).
- One frame budget shared across outputs; per-output paint must not stall siblings (assert via test timing bounds, not live frames).
- Primary output record exists or is added beside the inventory.

## Drafting assumptions

### Inventory in compositor, one shell (discovery)
- **Decision**: output inventory beside Runtime.output; per-output surfaces, one shell
- **Rationale**: spec constraint forbids parallel database; shell already multi-surface
- **Rejected**: mirroring, shell-side DB, per-output processes

## Rehydration cues

- Re-read `runtime.rs:190-280` (output creation) before designing the inventory.
- `cargo test -p roost-compositor output` baseline.
- Spec: `20260929180642-12b43038-multi-monitor`.
