---
created_date: "2026-09-30"
document_status: final
closed_date: "2026-09-30"
---

# Feature: 20260930211055-cf73a229-overview-app-launch-approval

<!--
  OVERVIEW
-->
## Overview

Roost overview search results become actionable: pressing an app result launches it and pressing an action result runs it, with the same starting feedback the dock already shows. Typing a name and pressing it is the fastest way to start anything.

<!--
  REQUIREMENTS
-->
## Requirements

- [x] **App results launch on press**
  Pressing an app result in overview search launches that app.
- [x] **Action results run on press**
  Pressing an action result runs that action.
- [x] **Launches show feedback**
  Overview-initiated launches show the same starting feedback as dock launches until their window maps or the window expires.
- [x] **Failed launches stay visible**
  A result whose launch fails keeps the overview open and reports the failure in place instead of vanishing.

<!--
  CONSTRAINTS
-->
## Constraints

- Launches reuse the existing tracked-launch path, not a parallel spawner.
- Search never blocks on ranking or filesystem reads while typing.

<!--
  ACCEPTANCE CRITERIA
-->
## Acceptance Criteria

- [x] **Type-and-press launches**
  Typing an installed app name in overview and pressing its result launches the app.
- [x] **Starting badge appears**
  The launched app shows starting feedback until its first window maps.
- [x] **Failed launch reports inline**
  A result that cannot launch reports the failure in the overview instead of closing it.

<!--
  TECHNICAL APPROACH
-->
## Technical Approach

- Suggested routing: overview result presses through the launch tracker the dock uses (per Constraints), keyed by desktop-entry id, with overview dismissal on successful spawn.

<!--
  SUCCESS METRICS
-->
## Success Metrics

- Users launch apps from typed queries without resorting to the terminal.
- Every launch shows starting feedback until its window maps.

<!--
  NON-GOALS
-->
## Non-Goals

- Search ranking changes or new providers.
- Calculator, file, or web shortcuts inside the query box.
