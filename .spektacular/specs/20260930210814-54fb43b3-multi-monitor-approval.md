---
created_date: "2026-09-30"
document_status: final
closed_date: "2026-09-30"
---

# Feature: 20260930210814-54fb43b3-multi-monitor-approval

<!--
  OVERVIEW
-->
## Overview

Roost becomes correct on multi-output setups: panel, dock, and wallpaper appear where they should on every connected output, and hot-plugged displays join the session without a restart.

<!--
  REQUIREMENTS
-->
## Requirements

- [x] **Panel per output**
  Each connected output shows the panel with correct clock, tiles, and indicators.
- [x] **Dock follows the primary output**
  The dock centers on the primary output and moves when primary changes.
- [x] **Wallpaper covers every output**
  Each output shows the wallpaper cover-cropped to its own size.
- [x] **Hotplug joins live**
  Connecting or disconnecting a display updates the session without restart and without losing windows.

<!--
  CONSTRAINTS
-->
## Constraints

- Output handling uses the compositor's existing output tracking; no parallel display database.
- Per-output paint stays within the current frame budget; no output may stall another.

<!--
  ACCEPTANCE CRITERIA
-->
## Acceptance Criteria

- [x] **Two outputs correct**
  With two outputs connected, panel, dock, and wallpaper are correct on both.
- [x] **Unplug keeps windows**
  Disconnecting an output moves its windows to a live output with none lost.
- [x] **Primary switch moves dock**
  Changing the primary output re-centers the dock on the new primary.

<!--
  TECHNICAL APPROACH
-->
## Technical Approach

- Decided direction the planner may adapt: per-output layer surfaces from the output inventory, wallpaper scaled per output size, dock anchored to the primary output record.

<!--
  SUCCESS METRICS
-->
## Success Metrics

- Two-output layout stays correct in daily multi-monitor use.
- Hotplug never loses a window.

<!--
  NON-GOALS
-->
## Non-Goals

- Per-output wallpapers or profiles.
- Fractional-scale mixed-DPI tuning beyond correct scaling.
- Display configuration UI (a Settings job).
