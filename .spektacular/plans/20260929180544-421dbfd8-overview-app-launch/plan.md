---
created_date: "2026-09-29"
document_status: final
closed_date: "2026-09-29"
---

# Plan: 20260929180544-421dbfd8-overview-app-launch

<!-- Metadata -->
<!-- Created: 2026-09-29T20:40:00Z -->
<!-- Commit: 3c610d6 -->
<!-- Branch: main -->
<!-- Repository: rust-wayland-desktop (/home/ubuntu/dev/rust-wayland-desktop) -->

## Overview

Roost overview search results become actionable: pressing an
app result launches it and pressing an action result runs it,
with the same tracked-launch feedback the dock already shows.

## Conventions

No project conventions apply to this feature — the
always-applied knowledge load returned no entries and the
one topic hit (dconf interop) does not bear on launching.

## Architecture & Design Decisions

Overview presses join the dock's launch path instead of
growing a second one. A result press resolves its
desktop-entry id (or runnable action), hands it to the
host-owned `LaunchTracker`, and dismisses the overview on
successful spawn — so starting feedback, window mapping, and
expiry behave exactly as dock launches do today. Spawn
failure keeps the overview open and reports the failure
inline on the pressed result. Nothing changes in search
ranking, providers, or the query box: the plan wires the
existing result model to the existing tracker.

This beats an overview-local spawner (duplicated fork,
feedback, and expiry bookkeeping that would drift from the
dock's) and launch-on-highlight (accidental spawns during
keyboard navigation). Press stays the explicit commit
gesture, and the fail-closed spawn precedent gives the
inline-failure path a tested shape. See
`research.md#alternatives-considered-and-rejected` for detail.

## Component Breakdown

- **Result press router (changed)** — maps a pressed
  overview result to its desktop-entry id or runnable action
  and hands it to the launch tracker; dismisses the overview
  on successful spawn, keeps it open with an inline failure
  otherwise.
- **Launch tracker (unchanged)** — keeps owning spawn,
  starting feedback, expiry, and bounds; gains no new API
  unless the overview needs a handle it does not already
  expose.
- **Overview shell (changed)** — renders the inline failure
  state on the pressed result and the shared starting
  feedback for overview-initiated launches.

## Data Structures & Interfaces

No new types. The contracts in play are the existing
ones: `SearchResult::launch(title, app_id)` carries the
desktop-entry id from query to press; `LaunchTracker::launch`
accepts an entry and reports the started pid or a spawn
error; the overview reads tracker state for starting
feedback exactly as the dock does.

## Implementation Detail

No new patterns. The work follows the dock-press shape:
resolve id, call into the tracker, react to the outcome.
The only new code-structure element is the press router
sitting between the overview result list and the tracker,
plus an inline-failure render state on results. A developer
reading the result sees overview presses and dock presses
converging on one tracker call.

## Dependencies

- Launch tracker (`apps.rs`) — spawn, feedback, bounds;
  no changes expected.
- Search result model (`search.rs`) — launch intent carrier;
  no changes expected.
- Dock press path (`dock.rs`) — the shape to mirror; read-only
  reference.
- freedesktop Desktop Entry Specification — Exec semantics
  for the bad-entry case; external reference.

Design documents this plan was built on: none — the spec
carries no design references.

## Testing Approach

Behavioural tests carry both of the spec's success
metrics: type-and-press launch of a fixture app asserts the
spawned process, and a bad-Exec press asserts the overview
stays open with inline failure. Unit tests cover press
routing (result → entry id → tracker call) and the
failure-render state. Heaviest coverage sits on the router,
where the spec's launch guarantees live. Deliberate gap: no
live-compositor window-mapping assertion — starting feedback
until first window maps is asserted via tracker state, not
pixels.

## Milestones & Tasks

### M1 — Search results launch

Pressing an app or action result in the overview launches it
with the same starting feedback dock launches show.
Validation: behavioural test types an app name, presses the
result, and observes the spawned process with starting
feedback.

### M2 — Failures stay visible

A result that cannot launch keeps the overview open and
reports the failure inline on the result. Validation: bad-Exec
press test asserts overview open plus inline failure.

### M1 — Search results launch

#### - [x] Task: Overview press launches apps
**Id:** 7333837e-8b9a-4c4a-9b17-1e40253eecbc
**Repo:** rust-wayland-desktop
**Depends on:** none
**Execution:** agent

Route overview app-result presses through the shared launch
tracker and dismiss the overview on successful spawn, with
the same starting feedback dock launches show.

*Technical detail:* [context.md#task-press-launch](./context.md#task-press-launch)

**Acceptance criteria**:
- [x] Type-and-press on an installed app spawns its process
- [x] Launched app shows starting feedback until its window maps

#### - [x] Task: Overview action results run
**Id:** 384affb9-aad7-4ecd-897c-912fec2a1757
**Repo:** rust-wayland-desktop
**Depends on:**
- 7333837e-8b9a-4c4a-9b17-1e40253eecbc — Overview press launches apps
**Execution:** agent

Pressing an action result runs that action through the same
router, dismissing the overview on success.

*Technical detail:* [context.md#task-action-run](./context.md#task-action-run)

**Acceptance criteria**:
- [x] Pressing an action result runs its command

### M2 — Failures stay visible

#### - [x] Task: Failed launches report inline
**Id:** 14ed8d86-c714-4568-9664-989913495e8d
**Repo:** rust-wayland-desktop
**Depends on:**
- 7333837e-8b9a-4c4a-9b17-1e40253eecbc — Overview press launches apps
**Execution:** agent

A result whose launch fails keeps the overview open and
reports the failure inline on the pressed result instead of
vanishing.

*Technical detail:* [context.md#task-inline-failure](./context.md#task-inline-failure)

**Acceptance criteria**:
- [x] Bad-Exec press keeps overview open with inline failure

## Open Questions

- Whether the tracker handle is reachable from the
  overview press path without new plumbing — only
  discoverable by wiring it. If the overview cannot reach
  the host-owned tracker, the implementer reports back
  rather than cloning tracker state.

## Out of Scope

Search ranking changes, new providers, and calculator,
file, or web shortcuts inside the query box are out of
scope; the spec's Non-Goals section governs.

## Changelog

### 2026-09-29 — Task: Overview press launches apps

**What was done**: New press router maps a pressed overview
result to its desktop entry and launches through the shared
tracker with overview dismissal on success; starting feedback
matches dock launches. Tests cover typed-app launch with
feedback, press miss as no-op, and spawn failure keeping the
overview open.

**Deviations**: none.

**Files changed**:
- `crates/shell-host/src/overview.rs`
- `crates/shell-host/src/panel.rs`

**Discoveries**: an Enter-key spawn path already existed;
the router unifies press handling in the dock_press shape.

### 2026-09-29 — Task: Overview action results run

**What was done**: Press router covers both runnable action
kinds — launch through the shared tracker, focus through the
token-gated activation path — with an exhaustive match so
future non-runnable variants fail closed at compile time.
Test asserts action press runs its command and dismisses.

**Deviations**: none.

**Files changed**:
- `crates/shell-host/src/overview.rs`
- `crates/shell-host/src/panel.rs`

**Discoveries**: none.

### 2026-09-29 — Task: Failed launches report inline

**What was done**: Spawn failure records inline failure state
rendered as a marker on the pressed result row at the next
repaint, with the overview kept open; only the failed row is
marked. Tests cover open-overview, inline report, and
row isolation.

**Deviations**: none.

**Files changed**:
- `crates/shell-host/src/overview.rs`
- `crates/shell-host/src/panel.rs`

**Discoveries**: none.
