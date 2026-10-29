---
created_date: "2026-09-29"
document_status: draft
---

# Feature: 20260929180544-421dbfd8-overview-app-launch

## Overview

Roost overview search results become actionable: pressing an app
result launches it and pressing an action result runs it, with the
same tracked-launch feedback the dock already shows. Typing a name
and pressing it is the fastest way to start anything.

## Requirements

- [x] **App results launch on press**
  Pressing an app result in overview search launches that app.
- [x] **Action results run on press**
  Pressing an action result runs that action.
- [x] **Launches show feedback**
  Overview-initiated launches show the same starting feedback as
  dock launches until their window maps or the window expires.
- [x] **Failed launches stay visible**
  A result whose launch fails keeps the overview open and reports
  the failure in place instead of vanishing.

## Constraints

- Launches reuse the existing tracked-launch path, not a parallel
  spawner.
- Search never blocks on ranking or filesystem reads while typing.

## Acceptance Criteria

- [x] **Type-and-press launches**
  Typing an installed app name in overview and pressing its result
  spawns the app process.
- [x] **Starting badge appears**
  The launched app shows starting feedback until its first window
  maps.
- [x] **Bad Exec reports inline**
  A result with an unlaunchable entry reports failure in the
  overview instead of closing it.

## Technical Approach

Decided direction the planner may adapt: route overview result
presses through the same launch tracker the dock uses, keyed by
desktop-entry id, with overview dismissal on successful spawn.

## Success Metrics

- Type-and-press launch of an installed app passes in a behavioural
  test.
- Failed launch keeps overview open, asserted by test.

## Non-Goals

- Search ranking changes or new providers.
- Calculator, file, or web shortcuts inside the query box.
