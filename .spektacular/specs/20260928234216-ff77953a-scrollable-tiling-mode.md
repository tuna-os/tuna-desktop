---
created_date: "2026-09-28"
document_status: final
closed_date: "2026-09-29"
---

# Feature: 20260928234216-ff77953a-scrollable-tiling-mode

## Overview
<!--
  OVERVIEW
  A concise 2-3 sentence summary of the feature. Answer three questions:
    1. What is being built?
    2. What problem does it solve?
    3. Who benefits and why does it matter?
  Avoid implementation details — this should be readable by any stakeholder.
-->

RWD gains a scrollable-tiling window mode alongside the default floating desktop. Windows arrange in a horizontal strip you scroll through with the keyboard or pointer wheel, with left/right column navigation, so users who think in columns instead of overlapping windows get that workflow without leaving RWD. The standard floating desktop stays the default and behaves exactly as today; the strip is a whole-session opt-in.
## Requirements
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

- [x] **Two session-wide modes with a floating default**
  Users can run the session in `gnome` mode (today's floating behavior, unchanged) or `scroll` mode; a fresh session always starts in `gnome` mode.
- [x] **Single-chord mode toggle**
  Users can switch the whole session between `gnome` and `scroll` with Super+Shift+T, and every open window re-layouts into the newly active mode.
- [x] **Strip focus motion**
  In `scroll` mode, users can move focus to the neighboring window left/right with the Super+arrow keys (Super+Left focuses the column to the left, Super+Right the column to the right).
- [x] **Strip window motion**
  In `scroll` mode, users can move the focused window one slot left/right along the strip with Super+Shift+arrow keys.
- [x] **Wheel scrolling**
  In `scroll` mode, users can scroll the strip with the pointer wheel; the focused window follows the visible position.
- [x] **Forced strip membership**
  In `scroll` mode, the system must place every open window — floating, maximized, tiled, or fullscreen — as a strip column, with no float-above layer.
- [x] **Column proportions**
  In `scroll` mode, the strip uses these column geometry values: 16px gaps, the focused column is not force-centered, column-width presets of 1/3, 1/2, and 2/3 of the output width, and new columns default to 1/2 of the output width.
- [x] **Preset-width cycling**
  In `scroll` mode, users can cycle the focused column through the 1/3, 1/2, and 2/3 width presets with Super+R (forward) and Super+Shift+R (backward).
- [x] **Shared desktop surfaces keep working**
  The panel, overview, window switcher, and search-to-launch must behave the same in both modes (regression gate: see Constraints).
## Constraints
<!--
  CONSTRAINTS
  Hard boundaries the solution must operate within. These are non-negotiable.
  Format: one bullet point per constraint.
  Examples:
    - Must integrate with the existing authentication system
    - Cannot introduce breaking changes to the public API
    - Must support the current minimum supported runtime versions
  Leave blank if there are no constraints.
-->

- Must not regress the 002 screenshot journeys (they run in the default gnome mode; CI stays green).
- Must keep focus, overview-focus parking, Alt+Tab MRU, clipboard, and close paths working identically in both modes.
- Must scroll by wheel where pointer-axis events reach the compositor; where the nested/X input path lacks them, keys alone are the fallback and the plan records that.
- Must be GPL-3.0-or-later and pass the repo gates (fmt, clippy -D warnings, workspace tests, shellcheck, CI journeys).
- Must not migrate or alter existing sessions' stored state; gnome mode stays the default and behaves exactly as today.
- Must not add Mango as a dependency (reference only).
- Must keep smithay 0.7.0 pinned with GLES+Mesa swrast rendering (no wgpu migration in this spec).
## Acceptance Criteria
<!--
  ACCEPTANCE CRITERIA
  The specific, binary conditions that define "done".
  Format: bold title on the checkbox line, verifiable detail indented below.
  Each criterion must be:
    - Independently verifiable (pass/fail, not subjective)
    - Traceable back to a requirement above
    - Testable by someone who didn't write the code
-->

- [x] **Fresh sessions float**
  A newly started session shows overlapping windows and no strip; no action is needed to get floating behavior.
- [x] **Super+Shift+T toggles the whole session**
  With three windows open, pressing Super+Shift+T arranges all three into a horizontal strip; pressing it again returns all three to overlapping positions.
- [x] **Super+arrows move strip focus**
  In scroll mode with three columns, Super+Right moves focus one column right (the visible focus highlight follows); Super+Left moves it back.
- [x] **Super+Shift+arrows move the window**
  In scroll mode, Super+Shift+Right moves the focused window one slot right (its neighbors visibly swap); Super+Shift+Left reverses it.
- [x] **Wheel scrolls the strip**
  In scroll mode with more columns than fit on screen, a wheel tick visibly scrolls the strip and the focus highlight stays on the same window after scrolling.
- [x] **Maximized windows become columns**
  Maximizing a window in scroll mode places it as a strip column (same height as neighbors, 16px gaps) instead of covering the screen; no window ever renders above the strip.
- [x] **Column geometry is observable**
  In scroll mode, a screenshot shows 16px gaps between columns; a single new column spans half the output width; three open columns overflow the output width instead of squeezing to fit.
- [x] **Preset widths cycle with Super+R**
  In scroll mode, Super+R resizes the focused column through 1/3, 1/2, and 2/3 of the output width in order (each step visibly changes its width); Super+Shift+R steps backward through the same presets.
- [x] **Shared surfaces unchanged, journeys green**
  Panel clock, overview search-to-launch, and Alt+Tab behave identically in both modes (regression gate: see Constraints).
## Technical Approach
<!--
  TECHNICAL APPROACH
  High-level technical direction to guide the planning agent. Include:
    - Key architectural decisions already made
    - Preferred patterns or technologies if known
    - Integration points with existing systems
    - Known risks or areas of uncertainty
  Format: one bullet point per direction/steer.
  Leave blank if you want the planner to propose the approach.
-->

- Strip layout lives in the compositor crate alongside the floating arranger (same window truth, two layout functions), with the mode flag on session state so Super+Shift+T re-layouts everything in place.
- Scroll-mode key handling extends the existing shell-host keyboard feed (Super+arrows, Super+Shift+arrows, Super+R / Super+Shift+R); wheel arrives as pointer-axis events on the compositor input path.
- Niri is the behavior model and Mango is reference-only (per Constraints: neither becomes a dependency); proportion and stacking lessons carry over within the pinned stack.
- Reuse the journey harness pattern for scroll-mode proof: a scripted nested session asserting strip geometry in screenshots plus marker-style launch proof.
- Open uncertainty for the planner: where strip scroll offset lives (compositor vs shell model) and how forced strip membership interacts with fullscreen protocol state.
## Success Metrics
<!--
  SUCCESS METRICS
  How you will know the feature is working well after delivery. Be specific:
    - Quantitative: "p99 latency < 200ms", "error rate < 0.1%", "users complete the flow without support intervention"
  Format: one bullet point per metric.
  Leave blank if not applicable.
-->

- Mode toggle completes a full re-layout within 2 frames with no window loss (every pre-toggle window present post-toggle in both directions).
- Scroll-mode screenshot proof passes in CI alongside the existing journeys (strip geometry assertions green).
- Zero regressions: gnome-mode journeys, focus/clipboard/switcher paths, and workspace tests stay green.
## Non-Goals
<!--
  NON-GOALS
  Explicitly state what this spec does NOT cover. This is as important as
  the requirements — it prevents scope creep and sets clear expectations.
  Format: one bullet point per exclusion.
  Examples:
    - "Mobile support is out of scope (tracked in #456)"
    - "Internationalisation will be addressed in a follow-up spec"
  Leave blank if there are no explicit exclusions to call out.
-->

- Per-workspace modes (explicitly whole-session).
- Touchpad-gesture parity beyond wheel/keys in v1.
- Remembering per-window float state across mode switches (forced strip was chosen instead).
