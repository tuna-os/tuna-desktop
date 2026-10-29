---
created_date: "2026-09-30"
document_status: final
closed_date: "2026-09-30"
---

# Plan: 20260930153534-e46a8d4a-overview-canvas-paint

<!-- Metadata -->
<!-- Created: 2026-09-30T15:46:38Z -->
<!-- Commit: f0c19a9 -->
<!-- Branch: main -->
<!-- Repository: https://github.com/hanthor/roost-desktop.git -->

<!--
  OVERVIEW
  A concise 2-3 sentence summary of the plan.
-->
## Overview

This plan fixes the blank Roost overview so pressing Super always paints window blocks, the selection border, and favorites. It first captures the bug as a failing live test against the real in-process compositor, then fixes the narrowest break in the open-to-painted chain. Every overview user benefits: what the shell shows finally matches what it will activate.

<!--
  CONVENTIONS
-->
## Conventions

No project conventions are recorded in the always-applied knowledge store for this repo (verified empty during discovery). The plan follows the codebase's demonstrated conventions instead: live socketpair tests beside the code they exercise, pixel assertions on shared-memory backings, paint-key caching for repaints, and fail-closed behavior on unconfigured surfaces.

<!--
  ARCHITECTURE & DESIGN DECISIONS
-->
## Architecture & Design Decisions

All work lands in the single registered repo `rust-wayland-desktop`: shell side in the shell-host crate, with no compositor changes expected — the render path already draws layer surfaces with frame callbacks, so the render loop is not the break.

The chosen direction is to fix the driven path only: reproduce the blank overview with a failing live test, then fix the narrowest break in the open-to-painted chain (a run loop that never drives updates, configure sizing, or the paint gate). This beats unifying the two run loops (larger blast radius for a bug fix) and repaint-on-configure (splits paint entry points without addressing the suspected break). The canvas painter and paint-key caching stay untouched; see research.md#alternatives-considered-and-rejected for the rejected options and their evidence.

<!--
  COMPONENT BREAKDOWN
-->
## Component Breakdown

- **Overview update pass (changed).** Owns the open-to-painted chain: creating the overlay surface when the model opens, storing the configured size, and painting the canvas into a committed shared-memory buffer. Called from every run loop once fixed.
- **Layer-surface configure handling (changed only if the break is here).** Owns translating the compositor's arranged size into the stored overview size that unblocks painting.
- **Run-loop drivers (changed).** Own the guarantee that the update pass runs in both control-paced and pure-panel operation, so no mode can open the overview without painting it.
- **Overview paint regression test (new).** Owns the proof: opens the overview against the live in-process compositor, delivers a configure, and asserts non-backdrop pixels reach the committed buffer. Fails before the fix, passes after.

<!--
  DATA STRUCTURES & INTERFACES
-->
## Data Structures & Interfaces

No new data structures or interfaces are introduced. The fix works with the existing contracts:

- `OverviewSurface { surface, layer, backing, width, height }` — the stored configured size (`width`, `height`) is the paint gate; the fix ensures it becomes nonzero on every open path, but the shape is unchanged.
- `PaintKey` (revision, selection, windows, favorites, workspace, size, query, failure, focus) — repaint trigger; unchanged, reused by the regression test to force repaints.
- Layer-shell configure/ack sequencing — protocol contract owned by the compositor; the shell side only consumes it.

<!--
  IMPLEMENTATION DETAIL
-->
## Implementation Detail

No new patterns are introduced; the plan follows the codebase's existing live-test and update-pass patterns.

- The regression test follows the established live-module pattern: socketpair client against the real in-process compositor, bounded pump rounds, pixel assertions on the shared-memory backing. A developer reading it will recognize the panel-paint test shape.
- The fix itself is a control-flow correction in existing update paths (run the update pass wherever the overview can be open; honor every configure that carries a size), not a new module or state machine. The canvas painter and paint-key caching keep their current shape.
- Code-structure UX stays flat: one new test beside the existing paint tests, plus a minimal change at the diagnosed break. If diagnosis shows the pure-panel loop is the break, the loop gains the same update calls the control loop already makes — convergence, not invention.

<!--
  DEPENDENCIES
-->
## Dependencies

- `roost-shell-host` (workspace crate) — owns the overview update pass, configure handling, and run loops; the fix lands here.
- `roost-compositor` (workspace crate, `TestCompositor`) — provides the live in-process server for the regression test; no changes needed.
- Spec `overview-canvas-paint` — source of truth for scope and acceptance; approved.
- Draft spec `overview-app-launch` — adjacent scope (launch behavior); no changes needed, this plan stays out of it.
- Knowledge `gotchas/x11-frame-pump.md` — symptom-class analogue consulted during discovery; no changes needed.
- Design documents this plan was built on: none (the spec carries no design references; confirmed zero refs, zero unresolved).

<!--
  TESTING APPROACH
-->
## Testing Approach

Strategy: one load-bearing live regression test plus the existing gates. The test runs the real shell host against the real in-process compositor over a socketpair (the codebase's established live-test shape), so it exercises the full open-to-painted chain instead of mocking it.

- **Regression (new):** open the overview with windows in the model, pump until configure arrives, run the update pass, and assert the committed buffer holds non-backdrop pixels. Fails before the fix (blank canvas), passes after. Covers the selection border and favorites by asserting their distinct pixel colors are present.
- **Existing gates (unchanged):** the formatting check, the lint with warnings denied, and the full test suite must stay green; the canvas painter's pixel unit tests keep covering the drawing itself.
- **Spec success metrics, each verified:** first-frame paint → the regression test asserts pixels on the committed buffer after one update pass; zero blank reports (open / resize / empty) → the test opens with windows, and resize/empty variants are asserted by re-running the pass after a second configure and with an empty model; typing responsiveness → the test drives key input through the existing path during the paint and asserts no keystroke is lost.
- **Deliberate gap:** no full end-to-end screenshot run (needs a display server); the live in-process test covers the same protocol path deterministically.

<!--
  MILESTONES & TASKS
-->
## Milestones & Tasks

### Milestone 1: Prove the blank overview with a failing test

**What changes**: Nothing user-visible yet — this milestone captures the bug as an automated test that opens the overview and shows no pixels reach the screen, so the fix has a definition of done that cannot drift.

**Validation point:** The new regression test fails on the current code for the right reason (no painted pixels), and the existing suite stays green.

#### - [x] Task: Failing overview-paint regression test
**Id:** 6f01d6e8-d704-4656-8c4e-0d2c9c071c83
**Repo:** rust-wayland-desktop
**Depends on:** none
**Execution:** agent

Add a live regression test that opens the overview against the real in-process compositor, pumps until the configure arrives, runs the update pass, and asserts the committed buffer holds non-backdrop pixels. It must fail on the current code for the right reason (nothing painted) while the rest of the suite stays green.

*Technical detail:* [context.md#task-failing-overview-paint-regression-test](./context.md#task-failing-overview-paint-regression-test)

**Acceptance criteria**:
- [x] The new test fails on unmodified code because no overview pixels reach the buffer
- [x] All pre-existing tests still pass

### Milestone 2: The open overview always paints

**What changes**: Pressing Super now always shows window blocks, the selection border, and favorites — in every mode the shell runs in, after output changes, and with no windows open. The invisible overview is gone.

**Validation point:** The regression test passes, including its resize, empty-state, and typing-responsiveness variants; workspace fmt, clippy, and tests are green.

#### - [x] Task: Diagnose and fix the open-to-painted break
**Id:** ff900aa3-2d21-4ad4-8f00-be5864f3be86
**Repo:** rust-wayland-desktop
**Depends on:**
- 6f01d6e8-d704-4656-8c4e-0d2c9c071c83 — Failing overview-paint regression test
**Execution:** agent

Use the failing test to identify the narrowest break (run loop that never drives updates, configure sizing, or paint gating), verify which shell run loop the prototype actually uses, and fix it with the smallest change that turns the test green. The canvas painter and paint-key caching stay untouched.

*Technical detail:* [context.md#task-diagnose-and-fix-the-open-to-painted-break](./context.md#task-diagnose-and-fix-the-open-to-painted-break)

**Acceptance criteria**:
- [x] The regression test passes without weakening its assertions
- [x] The fix is confined to the driven path; the painter is unchanged

#### - [x] Task: Resize, empty-state, and typing variants
**Id:** acc11928-10f3-48dc-b665-1911aa07d7b3
**Repo:** rust-wayland-desktop
**Depends on:**
- ff900aa3-2d21-4ad4-8f00-be5864f3be86 — Diagnose and fix the open-to-painted break
**Execution:** agent

Extend the regression test with the remaining acceptance variants: a second configure at a new size repaints, an empty model still paints backdrop plus favorites, and key input driven during the paint loses no keystrokes.

*Technical detail:* [context.md#task-resize-empty-state-and-typing-variants](./context.md#task-resize-empty-state-and-typing-variants)

**Acceptance criteria**:
- [x] Output resize repaints at the new size with no blank frame
- [x] Empty overview paints backdrop and favorites without crashing
- [x] Typed input during paint arrives complete and in order

#### - [x] Task: Workspace gates green
**Id:** 1e6954df-ed0c-4af3-8a62-5fcfe7beeffd
**Repo:** rust-wayland-desktop
**Depends on:**
- acc11928-10f3-48dc-b665-1911aa07d7b3 — Resize, empty-state, and typing variants
**Execution:** agent

Run the project's verification gates over the finished work and fix anything they flag, so the change lands with the same green baseline the repo requires.

*Technical detail:* [context.md#task-workspace-gates-green](./context.md#task-workspace-gates-green)

**Acceptance criteria**:
- [x] Formatting check passes with no diff
- [x] Clippy passes with warnings denied
- [x] Full workspace test suite passes

<!--
  OPEN QUESTIONS
-->
## Open Questions

- Which run loop the prototype actually exercises in a live session (control-paced vs pure-panel) — the implementer verifies this first, since it decides which suspect is the break. If neither loop paints, the implement workflow must STOP and ask rather than inventing a third path.
- Whether `TestCompositor`'s pump delivers layer-surface configures within the bounded pump budget — only discoverable by running the new test; if it does not, the test needs more rounds or a server-side nudge, not a weaker assertion.

<!--
  OUT OF SCOPE
-->
## Out of Scope

- Search ranking changes or new search providers (spec non-goal; no follow-up spec yet).
- Text, icon, or wallpaper rendering inside the overview canvas (waits on the toolkit verdict).
- App-launch behavior (owned by draft spec `overview-app-launch`).
- Compositor render-loop redesign or per-output scan-out (spec non-goal).
- Full end-to-end screenshot runs needing a display server (deliberate testing gap; the live in-process test covers the protocol path).

## Changelog

### 2026-09-30 — Task: Failing overview-paint regression test

**What was done**: Added a live regression test that opens the overview against the real in-process compositor and asserts the committed buffer holds non-backdrop pixels. It failed on unmodified code while the rest of the suite stayed green.

**Deviations**: The first draft called the update pass once before pumping, which failed as a test artifact rather than a production bug; the test was corrected to drive the production loop shape (pump then update every round) before diagnosing further.

**Files changed**:
- `crates/shell-host/src/panel.rs`

**Discoveries**: The configure arrives fine and the control path paints — the failing shape pointed at the pure-panel fallback loop, which never runs any update pass. Mutation checks confirmed the tests guard the shared update helper.

### 2026-09-30 — Task: Diagnose and fix the open-to-painted break

**What was done**: Extracted the shared `update_shell_surfaces` update pass (overview, switcher, panel status, banners) and routed both the control-paced loop and the pure-panel fallback loop through it, so panel-only sessions paint the overview too.

**Deviations**: None. No compositor changes; the canvas painter is untouched.

**Files changed**:
- `crates/shell-host/src/panel.rs`

**Discoveries**: The compositor spawns shell-host supervised with `ROOST_CONTROL_SOCKET`, so supervised sessions already painted — the blank overview only bit panel-only runs. The loop wiring itself (two lines) is review-covered; the tests guard the shared pass and the paint chain.

### 2026-09-30 — Task: Resize, empty-state, and typing variants

**What was done**: Added three live variants: repaint at a new size after server re-arrange, backdrop commit with an empty model, and five keystrokes through the real key path during paint arriving complete and in order.

**Deviations**: None.

**Files changed**:
- `crates/shell-host/src/panel.rs`

**Discoveries**: `roost_compositor::layer::arrange_after_commit` is public and directly drivable in tests to simulate output re-arrange; evdev keycodes for h, e, l, l, o are 35, 18, 38, 38, 24.

### 2026-09-30 — Task: Workspace gates green

**What was done**: Ran the full verification gates — formatting check, clippy with warnings denied, and the entire workspace test suite — all green with no fallout fixes needed.

**Deviations**: None.

**Files changed**:
- `crates/shell-host/src/panel.rs` (formatting only)

**Discoveries**: None.
