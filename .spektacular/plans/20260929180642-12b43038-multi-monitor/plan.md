---
created_date: "2026-09-30"
document_status: final
closed_date: "2026-09-30"
---

# Plan: 20260929180642-12b43038-multi-monitor

<!-- Metadata -->
<!-- Created: 2026-09-30T01:37:52Z -->
<!-- Commit: be54491790bedb146fa832939d78a095fb4c6673 -->
<!-- Branch: main -->
<!-- Repository: rust-wayland-desktop (/home/ubuntu/dev/rust-wayland-desktop) -->

<!--
  OVERVIEW
  A concise 2-3 sentence summary of the plan. Answer:
    1. What is being built?
    2. What problem does it solve?
    3. Who benefits?
  No file paths, no commands, no implementation detail. A reviewer should be
  able to decide whether the plan is worth reading in full from this section
  alone.
-->
## Overview

Roost becomes correct on multi-output setups: panel, dock, and wallpaper appear where they should on every connected output, and hot-plugged displays join the session without a restart. Anyone docking a laptop or plugging in a second monitor gets a shell that follows their displays instead of stranding windows on a vanished output.

<!--
  CONVENTIONS
  The project conventions (always-apply standards) that bear on this work,
  each with a one-line reason it applies — and only the relevant ones, not
  the whole knowledge base. Conventions are loaded in full during discovery
  and the relevant subset is chosen as the design is locked, then confirmed
  by the user. Cite a convention inline in the sections it drives. If no
  conventions are relevant (or the project has none), state that plainly,
  e.g. "No project conventions apply to this feature." An empty or generic
  list is a visible signal the knowledge base was not consulted.
-->
## Conventions

No project conventions apply to this feature — the always-applied load returned no entries and the spec carries no design references.

<!--
  ARCHITECTURE & DESIGN DECISIONS
  The chosen design direction in 2-4 short paragraphs. Explain the shape of
  the solution, the key decisions and their trade-offs, and why the chosen
  direction beats the alternatives. Cross-reference
  research.md#alternatives-considered-and-rejected so readers can drill into
  the evidence for rejected options. This is plan.md's load-bearing section —
  a reviewer should be able to spot missing architectural patterns or design
  gaps from this section without needing to read context.md.
-->
## Architecture & Design Decisions

One inventory, surfaces per output. The compositor grows an output inventory beside today's single output — each entry with its size, primary flag, and layer surfaces — fed by the standard global add/remove flow, so hotplug joins live with no restart and no second database. The shell paints panel and wallpaper per output from the inventory (wallpaper cover-cropped per size) and anchors the dock to the primary record, re-anchoring when primary changes. Removing an output migrates its windows to a live output before the entry drops, so none are lost; per-output paint shares the frame budget without stalling siblings.

This beats mirroring (useless), a shell-side display database (forbidden), and per-output shell processes (multiplied buses and caches). The single-output path becomes the one-entry case of the inventory rather than a special case beside it. See `research.md#alternatives-considered-and-rejected`.

<!--
  COMPONENT BREAKDOWN
  The components (new or changed) that make up the solution, with their
  responsibilities and how they interact. One bullet or short paragraph per
  component. Name the component, state what it owns, and describe its
  relationship to the other components. Do not list file paths or line
  numbers here — component responsibilities, not implementation sites.
-->
## Component Breakdown

- **Output inventory (new, compositor)** — entries with size, primary flag, and surfaces; add/remove on global flow; single output becomes the one-entry case.
- **Per-output paint (changed, shell)** — panel and wallpaper painted per inventory entry, wallpaper cropped per size, within the shared frame budget.
- **Dock anchor (changed, shell)** — dock centers on the primary record and re-anchors on change.
- **Window migration (new, compositor)** — removal moves windows to a live output before dropping the entry.

<!--
  DATA STRUCTURES & INTERFACES
  The types, interface signatures, and serialization boundaries introduced or
  changed by the plan. Show type shapes in pseudocode or a short code block
  where useful. Focus on the contract between components, not internal
  representation detail.
-->
## Data Structures & Interfaces

The inventory crosses to the shell as output records the shell already knows how to paint — size, primary, surface handles:

```
OutputRecord { id, size: (w, h), primary: bool }
```

Window migration is compositor-internal (window list re-mapped before entry drop); no new wire types. The dock reads the primary record; panel and wallpaper read each entry.

<!--
  IMPLEMENTATION DETAIL
  High-level only. Sketch new patterns being introduced, major code-shape
  changes, and code-structure UX — enough for a reviewer to spot missing
  patterns or design gaps. This is NOT per-task file:line work — that
  belongs in context.md. If you find yourself writing "in file X at line Y",
  stop and move it to context.md.
-->
## Implementation Detail

The plan generalizes rather than forks: the existing single output becomes inventory entry zero, paint loops gain an output dimension, and removal is add in reverse with migration first. A developer reading the result sees one output path that happens to run N times, with primary as a flag rather than a special case.

<!--
  DEPENDENCIES
  The internal packages, external libraries, upstream specs, or prior plans
  this work depends on. One bullet per dependency with a one-line note on
  what it provides and whether it needs any changes.
-->
## Dependencies

- Compositor runtime — inventory home; extended.
- wl_output globals — standard add/remove flow; used as-is.
- Shell paint paths — per-output dimension; extended.
- Wallpaper decode — per-size crops; extended.

Design documents this plan was built on: none — the spec carries no design references.

<!--
  TESTING APPROACH
  High-level overview of the testing strategy: what kinds of tests
  (unit, integration, contract, regression), which components get the most
  coverage, and what the load-bearing assertions are. Per-task testing
  detail — which specific tests live in which specific files — stays in
  context.md.
-->
## Testing Approach

Behavioural tests with virtual outputs carry both success metrics: two-output layout correctness (panel, dock, wallpaper per output) and hotplug window preservation on add and remove. Unit tests cover inventory add/remove, primary re-anchor, and migration mapping. Heaviest coverage sits on migration and inventory, where the spec's no-loss guarantee lives. Deliberate gap: no live-GPU multi-head tests — CI has one virtual backend; virtual outputs stand in.

<!--
  MILESTONES & TASKS
  2-4 milestones. Each milestone leads with a "What changes" summary
  paragraph describing the user-visible difference when the milestone lands.
  Each task is one unit of work in exactly one registered repo, for one
  executor, and carries **Id:** (from `plan task-id`), **Repo:**,
  **Depends on:** (`none` or `- <id> — <title>` lines) and **Execution:**
  (`agent`, or `human — <reason>`) lines. Each task has a 2-4 sentence
  plain-language summary, a *Technical detail:* link to context.md, and an
  **Acceptance criteria**: checkbox list with outcome statements (not shell
  commands). No file:line references in plan.md task content — those live in
  context.md.
-->
## Milestones & Tasks

### Milestone 1: Every output shows the shell

**What changes**: Panel, dock, and wallpaper paint correctly per output from the inventory, with the dock on primary.

#### - [x] Task: Output inventory in the compositor
**Id:** d9b8edfb-a022-4d6a-80c6-822742bc1b71
**Repo:** rust-wayland-desktop
**Depends on:** none
**Execution:** agent

Replace the single-output state with an inventory of outputs, each holding its size, primary flag, and layer surfaces. Adding an output registers a new entry through the standard global flow; removing one drops its entry. The current single output becomes the one-entry case, so existing behaviour is unchanged when only one display is present.

*Technical detail:* [context.md#task-output-inventory-in-the-compositor](./context.md#task-output-inventory-in-the-compositor)

**Acceptance criteria**:

- [x] Two virtual outputs each report their own size and primary flag from the inventory
- [x] Adding and removing an output updates the inventory with no restart and no second database
- [x] Single-output sessions behave exactly as before

#### - [x] Task: Per-output shell paint and dock anchor
**Id:** 20b7f905-29a9-4a77-a6d8-e06b37bc5443
**Repo:** rust-wayland-desktop
**Depends on:**
- d9b8edfb-a022-4d6a-80c6-822742bc1b71 — Output inventory in the compositor
**Execution:** agent

Paint the panel and wallpaper once per inventory entry instead of once globally, cropping the wallpaper per output size, and anchor the dock to the primary record so it re-anchors when the primary changes. Per-output paint shares the frame budget so no output stalls its siblings.

*Technical detail:* [context.md#task-per-output-shell-paint-and-dock-anchor](./context.md#task-per-output-shell-paint-and-dock-anchor)

**Acceptance criteria**:

- [x] Panel, dock, and wallpaper appear correctly on each of two virtual outputs
- [x] Switching primary moves the dock to the new primary output
- [x] Wallpaper fills each output at its own size with no stretching

### Milestone 2: Hotplug preserves the session

**What changes**: Outputs join and leave live; removal migrates windows with none lost; primary switches move the dock.

#### - [x] Task: Hotplug window migration and tests
**Id:** 59f8de5c-60a2-41e4-8c93-04a4c9a80917
**Repo:** rust-wayland-desktop
**Depends on:**
- d9b8edfb-a022-4d6a-80c6-822742bc1b71 — Output inventory in the compositor
- 20b7f905-29a9-4a77-a6d8-e06b37bc5443 — Per-output shell paint and dock anchor
**Execution:** agent

Migrate windows off a removed output onto a live one before its inventory entry drops, so no window is lost on hotplug, and cover the whole feature with virtual-output tests: layout correctness, add/remove preservation, and primary switching.

*Technical detail:* [context.md#task-hotplug-window-migration-and-tests](./context.md#task-hotplug-window-migration-and-tests)

**Acceptance criteria**:

- [x] Removing an output preserves every open window on a remaining output
- [x] Hotplug add and remove round-trips pass on virtual outputs with no session restart
- [x] Full workspace gates stay green

<!--
  OPEN QUESTIONS
  Strictly for questions that genuinely cannot be resolved until
  implementation begins. Anything resolvable by asking the user, reading the
  code, or running a quick experiment must be resolved now — not parked
  here. If this section is empty, that is the expected outcome of a healthy
  planning pass.
-->
## Open Questions

No open questions — every uncertainty in this plan was resolvable from the code (single-output shape in `runtime.rs`, `lib.rs`, `layer.rs`) or settled as a spec constraint (compositor inventory, no shell-side database, window preservation). The one genuine unknown — live-GPU multi-head fidelity — is recorded as a deliberate testing gap, not a question.

<!--
  OUT OF SCOPE
  Explicit exclusions agreed during planning. Each bullet states what is NOT
  being done and, where useful, where it is tracked instead. This is as
  important as the requirements — it prevents scope creep and sets clear
  expectations for reviewers.
-->
## Out of Scope

Out of scope for this plan:

- Mirrored / cloned outputs — the spec requires independent per-output surfaces.
- Per-output shell processes — one shell paints all outputs from the single inventory.
- Output layout configuration UI — arrangement and primary selection stay at protocol defaults; a settings surface is future work.
- HiDPI per-output scale factors — sizes vary, scale handling is unchanged.
- Live-GPU multi-head validation — CI exercises virtual outputs only.

## Changelog

### 2026-09-30 — Task: Output inventory in the compositor

**What was done**: The compositor now tracks outputs in an inventory on `State` — one entry per output with name, protocol handle, size, and primary flag — instead of a single output field. Launch registers `roost-0` as entry zero; layer arrange and window areas read the primary entry.

**Deviations**: Added `set_primary` alongside the planned add/remove (primary switching needs it in the next task); fixed an `unused_mut` lint in the untouched `lock.rs` test and formatting in the new test file to keep gates green.

**Files changed**:
- `crates/compositor/src/lib.rs`
- `crates/compositor/src/runtime.rs`
- `crates/compositor/src/layer.rs`
- `crates/compositor/src/windows.rs`
- `crates/compositor/tests/outputs.rs`
- `crates/compositor/tests/lock.rs`

**Discoveries**: `state.output_size` had four readers, not two — `windows.rs` work-area/fullscreen paths also used it. `set_output_size` was never updated on backend resize (pre-existing quirk, left alone). The public Smithay `Output` path is `smithay::output::Output`, not `smithay::wayland::output::Output`. Smithay needs one `Output::new` + `create_global` per output on the shared `OutputManagerState`; dropping the last `Output` handle removes the global.

### 2026-09-30 — Task: Per-output shell paint and dock anchor

**What was done**: The shell now paints one panel per bound output (primary keeps the legacy surface, others become extras with their own backing and paint keys) and anchors the single dock to the primary record, rebinding on primary switches. A new `Message::Outputs` wire message (protocol 0.7) carries the inventory; the hub broadcasts it on change and to newcomers, and the shell binds `wl_output` globals plus `xdg_output` names to place its surfaces. The compositor arranges each layer surface against its bound output (with global-space offsets) and caches wallpaper crops per output size.

**Deviations**: Chose a new `Outputs` message over stuffing the Snapshot (avoids touching the revision/delta machinery; empty means unknown so old peers and pre-inventory frames stay silent). Added auto-tiling locations to entries (needed for correct multi-output placement). Render still presents primary only (single nested window); per-output wallpaper correctness is proven at the crop/cache level.

**Files changed**:
- `crates/shell-control-schema/src/lib.rs`
- `crates/compositor/src/control.rs`
- `crates/compositor/src/lib.rs`
- `crates/compositor/src/layer.rs`
- `crates/compositor/src/wallpaper.rs`
- `crates/compositor/src/runtime.rs`
- `crates/shell-host/src/control.rs`
- `crates/shell-host/src/panel.rs`
- `crates/compositor/tests/outputs.rs`
- `crates/compositor/tests/control.rs`
- `crates/compositor/tests/layer.rs`

**Discoveries**: The shell learns sizes only via layer-shell configure — per-output paint required per-output surfaces, not a paint loop. `Output::from_resource` + `name()` map client surfaces to entries; `Output::owns` is the alternative. xdg-output (`wayland_protocols::xdg::xdg_output::zv1`) names shell-side outputs; sizes ride configure so position/size events are ignored. The hub must not send empty inventories to newcomers (broke strict frame-reading tests). Shell presses/popups stay primary-only (noted gap). Crate edition predates let-chains — nested `if let` only.

### 2026-09-30 — Task: Hotplug window migration and tests

**What was done**: Removing an output now preserves every window: floating windows centered on the removed slice shift size-preserved into the survivor slice, and derived layouts re-resolve against the new primary after failover (two-phase: migrate, remove, re-apply). Real outputs un-advertise their `wl_output` global on removal so clients see the hotplug live. Seven migration tests cover floating shifts, maximized re-resolution (including the primary-removal regression), no-op cases, add/remove round-trips, and real global teardown.

**Deviations**: The plan assumed a per-output window list to re-map; none exists (windows live in workspaces with no output affinity), so migration is geometric instead — same outcome, no new associations. Split derived re-resolution into its own step after finding that re-applying before failover sizes windows for the dying primary.

**Files changed**:
- `crates/compositor/src/lib.rs`
- `crates/compositor/src/windows.rs`
- `crates/compositor/src/runtime.rs`
- `crates/compositor/tests/migration.rs`

**Discoveries**: `DisplayHandle::disable_global` + `remove_global` un-advertise; the `GlobalId` must be stored at `create_global` time (it was discarded). `GlobalId` lives in `wayland_server::backend`, is Clone but not Copy. Real-global removal races in-flight client binds — tests need a sync barrier first (documented `remove_global` race, not a bug). No production hotplug event source exists (nested backend has one window), so the live path is the tested API plus the per-tick inventory publish; a future backend listener calls migrate/remove in the documented order.
