---
created_date: "2026-09-30"
document_status: final
closed_date: "2026-09-30"
---

# Plan: 20260930163234-5de5360c-rhai-extensions

<!-- Metadata -->
<!-- Created: 2026-09-30T16:41:13Z -->
<!-- Commit: 3683f16 -->
<!-- Branch: main -->
<!-- Repository: https://github.com/hanthor/roost-desktop.git -->

<!--
  OVERVIEW
-->
## Overview

This plan builds the sandboxed Rhai extension system: scripts that contribute bar cells, dock badges, and indicator reactions without rebuilding the shell. It starts with the engine plus a sample cell and a hostile battery, then wires outputs into the shell and adds hot reload. Authors get the GNOME-extensions equivalent; users get safe customization.

<!--
  CONVENTIONS
-->
## Conventions

No project conventions are recorded in the always-applied knowledge store for this repo (verified empty during discovery). The plan follows the codebase's demonstrated conventions instead: temp-dir fixtures with no HOME side effects, plain-struct modules with in-module unit tests, painters reading cached snapshots, and fail-closed behavior with user-visible notes.

<!--
  ARCHITECTURE & DESIGN DECISIONS
-->
## Architecture & Design Decisions

All work lands in the single registered repo `rust-wayland-desktop`, in a new `extensions` module in the shell-host crate plus its integration points (panel strip, dock, indicators). The Rhai crate (1.26.1, cached) is a new dependency of `roost-shell-host` only.

The chosen direction is a sandboxed engine with cached outputs: one Rhai engine with a hand-built allowlist API module as the only registered surface, scripts run off the paint/event paths on the existing update cadence, and painters render from a plain-data cache. Per-call operation budgets disable hangers; mtime polling drives hot reload with no new dependency. This beats synchronous painter calls (disqualified: script code on the frame path violates a hard constraint) and an out-of-process runner (High effort for isolation the in-process allowlist already provides); see research.md#alternatives-considered-and-rejected for the full evidence.

<!--
  COMPONENT BREAKDOWN
-->
## Component Breakdown

- **Extension host (new).** Owns the Rhai engine, the allowlist API module, per-script budgets, and the enabled/disabled state with visible notes. Runs scripts off-path and caches their outputs as plain data.
- **Script directory watcher (new, inside the host).** Owns mtime polling: added, changed, or removed scripts reload without a session restart.
- **Output cache (new, inside the host).** Owns the painted inputs: cell text/icon plus press action, badge text, reaction values. Painters read only this cache, never scripts.
- **Panel/dock/indicator integration (changed).** Owns surfacing cached outputs: strip cells beside system tiles, badges on dock items, reactions as indicator updates.
- **Sample extension plus docs (new).** Owns the proof: a documented bar-cell script that works on a clean session.
- **Hostile battery tests (new).** Owns the sandbox proof: filesystem, network, spawn, and infinite-loop scripts are denied, disabled, and never take the shell down.

<!--
  DATA STRUCTURES & INTERFACES
-->
## Data Structures & Interfaces

No existing structures change. New contracts, all inside the shell-host crate:

- `ExtensionHost` — owns the engine, loaded scripts, budgets, and states (enabled / disabled with note). The shell drives it; painters never see it.
- `ScriptOutput { cell_text, cell_icon, press_action, badge, reaction }` — plain-data cache per script; the only thing painters read.
- `ExtensionState` — per-script lifecycle: loaded, disabled with a user-visible note, plus the mtime that detected its last change.
- Rhai API module shape (pseudocode): `bar_cell(text, icon)`, `on_press(action_id)`, `dock_badge(text)`, `notice(text)` — the complete allowlist; nothing else is registered on the engine.

<!--
  IMPLEMENTATION DETAIL
-->
## Implementation Detail

One new module, existing integration idioms everywhere else.

- The `extensions` module follows the codebase's plain-struct-plus-impl shape (like the model and tiles modules): no new traits, no new async runtime, no new IPC. A developer reading it finds state structs, a drive function called from the update pass, and unit tests beside the code.
- The engine boundary is the one new pattern: all Rhai interaction stays inside the module behind the output cache. Panel, dock, and watcher code keeps its current shape and gains small branches reading cached values — the same way painters already read model snapshots.
- Hot reload introduces no watcher dependency: the host compares directory mtimes during its regular drive, converging with how the shell already polls rather than subscribing.
- The sample extension lives next to the module as a documented `.rhai` file plus a short doc page, following the `docs/keymap.md` precedent of shipping user docs with behavior.

<!--
  DEPENDENCIES
-->
## Dependencies

- `roost-shell-host` (workspace crate) — owns the new extensions module and all integration; the only crate that changes.
- `rhai` 1.26.1 (external crate, already in the offline cargo cache) — the scripting engine; added as a dependency of `roost-shell-host`.
- Spec `rhai-extensions` (signed) — source of truth; approved.
- Draft spec `rhai-extensions` (2026-09-29) — historical scope reference; superseded, untouched.
- Design documents this plan was built on: none (the spec carries no design references; confirmed zero refs, zero unresolved).

<!--
  TESTING APPROACH
-->
## Testing Approach

Strategy: unit tests on the extension host plus a hostile battery, all headless — no display needed because scripts never touch painters directly.

- **Host unit tests (new):** load scripts from temp dirs (no HOME side effects, following the favorites-test precedent): a cell script yields cached cell output; a syntax-error script disables with a note; an infinite-loop script hits the operation budget and disables within the bound; editing a script file re-runs it on the next drive.
- **Hostile battery (new):** scripts attempting filesystem, network, and process access fail (no such functions registered) and disable; the host stays up throughout.
- **Integration (changed):** panel/dock/watcher branches reading the cache get focused assertions that cached values reach paint inputs.
- **Existing gates (unchanged):** the formatting check, the lint with warnings denied, and the full test suite stay green.
- **Spec success metrics, each verified:** sample cell renders and reacts → host test plus sample script run; containment → hostile battery; iteration without restart → reload test.
- **Deliberate gap:** no live Rhai-in-session run (needs a display session); the host tests drive the same code the update pass calls.

<!--
  MILESTONES & TASKS
-->
## Milestones & Tasks

### Milestone 1: Sandboxed engine with a sample cell

**What changes**: Extension scripts run for the first time: a documented sample script contributes a bar cell, hostile scripts are contained, and bad scripts disable with a note. Users get the extension directory and its first working example.

**Validation point:** Host unit tests plus the hostile battery pass; the sample script runs on a clean host.

#### - [x] Task: Sandboxed extension host with sample cell
**Id:** c00d238f-07c0-4d79-a261-42450e4745e7
**Repo:** rust-wayland-desktop
**Depends on:** none
**Execution:** agent

Add the rhai dependency and a new extensions module in the shell host: an engine with a hand-built allowlist API, per-call operation budgets, and enable/disable state with visible notes. Ship a documented sample bar-cell script that runs on a clean host.

*Technical detail:* [context.md#task-sandboxed-extension-host-with-sample-cell](./context.md#task-sandboxed-extension-host-with-sample-cell)

**Acceptance criteria**:
- [x] The sample script contributes a bar cell on a clean host
- [x] No filesystem, network, or process function is reachable from scripts
- [x] A hanging script is disabled within the time bound

#### - [x] Task: Hostile battery and clean disable
**Id:** caa86937-27e6-4872-9280-74c353fa839c
**Repo:** rust-wayland-desktop
**Depends on:**
- c00d238f-07c0-4d79-a261-42450e4745e7 — Sandboxed extension host with sample cell
**Execution:** agent

Prove containment with automated tests: scripts attempting filesystem, network, and spawn access are denied and disabled, syntax-error and infinite-loop scripts disable with a visible note, and the host stays up through all of it.

*Technical detail:* [context.md#task-hostile-battery-and-clean-disable](./context.md#task-hostile-battery-and-clean-disable)

**Acceptance criteria**:
- [x] Hostile scripts are denied, disabled, and never take the host down
- [x] Bad scripts show a visible disable note

### Milestone 2: Shell integration and hot reload

**What changes**: Script outputs reach the screen — strip cells, dock badges, and indicator reactions update from cached values — and editing a script applies without restart. The extension system is complete end to end.

**Validation point:** Integration assertions plus the reload test pass; workspace gates are green.

#### - [x] Task: Panel, dock, and indicator integration
**Id:** 0eeef59a-4acd-4961-ac50-284058d43aae
**Repo:** rust-wayland-desktop
**Depends on:**
- c00d238f-07c0-4d79-a261-42450e4745e7 — Sandboxed extension host with sample cell
**Execution:** agent

Surface cached script outputs in the shell: strip cells beside system tiles, badges on dock items, and reactions as indicator updates. Painters read the cache only, never scripts.

*Technical detail:* [context.md#task-panel-dock-and-indicator-integration](./context.md#task-panel-dock-and-indicator-integration)

**Acceptance criteria**:
- [x] Cached cell, badge, and reaction values reach paint inputs
- [x] No script code runs on paint or event paths

#### - [x] Task: Hot reload and workspace gates
**Id:** a3daf4d1-40c1-4c6a-a74b-5ad8c11e95a2
**Repo:** rust-wayland-desktop
**Depends on:**
- caa86937-27e6-4872-9280-74c353fa839c — Hostile battery and clean disable
- 0eeef59a-4acd-4961-ac50-284058d43aae — Panel, dock, and indicator integration
**Execution:** agent

Drive reload by script-directory mtime polling and prove edits apply without restart, then run the full verification gates over the finished work.

*Technical detail:* [context.md#task-hot-reload-and-workspace-gates](./context.md#task-hot-reload-and-workspace-gates)

**Acceptance criteria**:
- [x] Editing a script updates its output without restart
- [x] Formatting, lint, and the full test suite pass

<!--
  OPEN QUESTIONS
-->
## Open Questions

- The exact rhai 1.26.1 API surface (progress-hook name, module registration calls) — implement confirms against the cached crate first and STOPS on sandbox-relevant surprises.
- Whether the offline cargo registry resolves the rhai dependency without network access — implement tries offline resolution first and STOPS if it cannot resolve.
- The precise paint-input seams in dock.rs and watcher.rs — implement reads them at task time; the plan assumes small cache-reading branches suffice.

<!--
  OUT OF SCOPE
-->
## Out of Scope

- Running real GNOME (GJS/Mutter) extensions (spec non-goal).
- A graphical extension store or one-click installs (spec non-goal).
- Live Rhai-in-session runs needing a display (deliberate testing gap; host tests drive the same code).

## Changelog

### 2026-09-30 — Task: Sandboxed extension host with sample cell

**What was done**: Added the rhai dependency (1.26.1, offline-resolved) and a new extensions module: hardened engine (dummy module resolver, captured print, operation budget, allowlist API), enable/disable states with notes, plus a documented sample cell script and user docs.

**Deviations**: The press handler initially never drained the script-call sink back into cached output; fixed with a merge-on-press before any test depended on it.

**Files changed**:
- `Cargo.toml`, `crates/shell-host/Cargo.toml`, `crates/shell-host/src/lib.rs`, `crates/shell-host/src/extensions.rs`, `crates/shell-host/sample-extensions/hello_cell.rhai`, `docs/extensions.md`

**Discoveries**: `Engine::new()` installs a file module resolver (must be replaced — the key sandbox finding); default print/debug go to stdout (must be overridden); progress-hook halt surfaces as a generic termination carrying the budget message.

### 2026-09-30 — Task: Hostile battery and clean disable

**What was done**: Automated battery — filesystem, network, spawn, and disk-import attempts denied and disabled; infinite loop budgeted and disabled; syntax errors disabled with notes; host stays up throughout.

**Deviations**: None.

**Files changed**:
- `crates/shell-host/src/extensions.rs` (tests)

**Discoveries**: None beyond the engine hardening above.

### 2026-09-30 — Task: Panel, dock, and indicator integration

**What was done**: Script cells paint in the strip beside indicators with press routing, badges paint on the dock row, notices surface as notification banners once each; paint keys extended; engine audit confirms script code runs only on the slow tick.

**Deviations**: Press dispatch was first wired synchronously into the press handler, then deliberately deferred to a slow-tick queue when the off-path audit flagged script code on the event path. Dock badges paint as end-of-row cells (no per-app mapping in v1) rather than on individual items.

**Files changed**:
- `crates/shell-host/src/panel.rs`

**Discoveries**: Indicator cells are fixed-size leftward rows — the ideal seam for script cells; dock slots are centered by count, so badges must continue past the last item rather than join the count.

### 2026-09-30 — Task: Hot reload and workspace gates

**What was done**: Directory mtime polling on the slow tick; edits apply and removals drop on the next drive (tested with real files). Full gates green: formatting, clippy with warnings denied, all 32 workspace test binaries.

**Deviations**: None.

**Files changed**:
- `crates/shell-host/src/panel.rs` (tests)

**Discoveries**: None.
