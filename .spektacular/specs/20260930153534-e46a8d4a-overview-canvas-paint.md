---
created_date: "2026-09-30"
document_status: final
closed_date: "2026-09-30"
---

# Feature: 20260930153534-e46a8d4a-overview-canvas-paint

<!--
  OVERVIEW
  A concise 2-3 sentence summary of the feature. Answer three questions:
    1. What is being built?
    2. What problem does it solve?
    3. Who benefits and why does it matter?
  Avoid implementation details — this should be readable by any stakeholder.
-->
## Overview

Pressing Super opens the desktop overview so users can see every open window and pick one. Today the overview can open with a blank canvas — no window blocks, no selection border, no favorites — leaving search results and keyboard navigation operating on something invisible. This spec makes the open overview always paint: window blocks, selection state, and favorites appear on every open, so what you see matches what the shell will activate.

<!--
  REQUIREMENTS
  Specific, testable behaviours the feature must deliver.
  Format: bold title on the checkbox line, detail indented below.
  Rules:
    - Use active voice: "Users can...", "The system must..."
    - Each requirement should be independently verifiable
    - Focus on WHAT, not HOW — avoid prescribing implementation
    - Keep each item atomic — one behaviour per line
-->
## Requirements

- [x] **Open overview paints window blocks**
  Users can press Super and see one block per open window in grid order within one frame of the overview opening.
- [x] **Selection is visible**
  Users can see which window is selected: the selected block carries a bright border that follows keyboard movement.
- [x] **Favorites stay visible**
  Users can see their pinned favorites as small squares on the open overview, matching the saved favorites list.
- [x] **Paint survives output changes**
  Users can plug, unplug, or resize an output and the open overview repaints at the new size instead of freezing or blanking.
- [x] **Empty overview still paints**
  Users can open the overview with no windows open and see the backdrop, search field affordance, and favorites rather than a black screen.
- [x] **Paint never blocks typing**
  Users can type search queries while the overview paints; a slow paint never drops or delays keystrokes.

<!--
  CONSTRAINTS
  Hard boundaries the solution must operate within. These are non-negotiable.
  Format: one bullet point per constraint.
-->
## Constraints

- The solution must paint shared-memory pixel buffers and must not add a toolkit (no GTK) dependency.
- The solution must treat the existing overview model (windows, selection, favorites, search) as the source of truth and must not change model semantics.
- The compositor's layer-shell protocol behavior is fixed: the shell must work with configure/acknowledge sequencing as the compositor sends it.
- No keystroke may be dropped or delayed by painting; input stays on the existing path.
- Locked sessions hide everything, including the overview; the paint path must not leak pixels while locked.

<!--
  ACCEPTANCE CRITERIA
  The specific, binary conditions that define "done".
  Format: bold title on the checkbox line, verifiable detail indented below.
  Each criterion must be:
    - Independently verifiable (pass/fail, not subjective)
    - Traceable back to a requirement above
    - Testable by someone who didn't write the code
-->
## Acceptance Criteria

- [x] **Blocks appear on open**
  Opening the overview with three windows open shows three window blocks; a pixel sample of the overview surface differs from the bare backdrop.
- [x] **Border follows selection**
  Moving the selection with the keyboard moves the bright border to the newly selected window's block; the previously selected block loses it.
- [x] **Favorites render**
  With two pinned favorites, the open overview shows two favorite squares matching the saved order.
- [x] **Repaint after output change**
  Resizing the output while the overview is open repaints the canvas at the new size with no frozen or blank frame.
- [x] **Empty state paints**
  Opening the overview with no windows shows backdrop plus favorites with zero crashes and zero black frames.
- [x] **Typing stays responsive**
  Typing a five-character query during paint shows all five characters with no dropped keystrokes.

<!--
  TECHNICAL APPROACH
  High-level technical direction to guide the planning agent.
  Format: one bullet point per direction/steer.
-->
## Technical Approach

- Decided direction the planner may adapt: reproduce the blank overview first with a failing test on the existing in-process compositor-plus-socket harness, then fix the narrowest break in the open-to-painted chain (surface creation, configure sizing, paint gating, or the panel loop that drives updates).
- Keep the existing canvas painter and paint-key caching untouched where possible; add a paint regression test that opens the overview, delivers a configure, and asserts non-backdrop pixels reach the committed buffer.

<!--
  SUCCESS METRICS
  How you will know the feature is working well after delivery.
  Format: one bullet point per metric.
-->
## Success Metrics

- Opening the overview with windows open paints window blocks on the first visible frame, verified by a behavioural test.
- Zero blank-overview reports: opening, resizing output, and opening with no windows all paint, covered by regression tests.
- Typing a query during paint drops zero keystrokes, asserted by test.

<!--
  NON-GOALS
  Explicitly state what this spec does NOT cover.
  Format: one bullet point per exclusion.
-->
## Non-Goals

- Search ranking changes or new search providers.
- Text, icon, or wallpaper rendering inside the overview canvas (waits on the toolkit verdict).
- App-launch behavior itself (owned by the overview-app-launch spec).
- Compositor render-loop redesign or per-output scan-out work.
