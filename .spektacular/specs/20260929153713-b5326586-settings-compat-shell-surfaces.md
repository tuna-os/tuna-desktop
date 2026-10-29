---
created_date: "2026-09-29"
document_status: final
closed_date: "2026-09-29"
---

# Feature: 20260929153713-b5326586-settings-compat-shell-surfaces

<!--
  OVERVIEW
  A concise 2-3 sentence summary of the feature. Answer three questions:
    1. What is being built?
    2. What problem does it solve?
    3. Who benefits and why does it matter?
  Avoid implementation details — this should be readable by any stakeholder.
-->
## Overview

Roost becomes compatible with GNOME Settings by honoring the shared GNOME desktop settings instead of owning a parallel settings universe, while reimplementing the GNOME top bar, app dock, and app indicators as native shell surfaces. GNOME users keep their Settings workflows and tray-dependent apps on a modern compositor stack, and Roost stops looking like a bare developer preview in daily use.

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

- [x] **Shared GNOME settings are honored**
  Changes made in GNOME Settings to wallpaper, clock format, and interface preferences take effect in the Roost session without a Roost-side settings UI.
- [ ] **Roost-specific settings have a home**
  Roost-only settings persist and are adjustable without hand-editing configuration files.
- [x] **Top bar carries calendar and status menus**
  Users can open a calendar from the bar clock and reach network, sound, and power controls from bar status menus.
- [x] **Dock launches and tracks apps**
  Users can launch pinned and searched applications from a dock, see which applications are running, and switch to or quit them from the dock.
- [x] **App indicators appear in the bar**
  Applications with tray icons get a visible indicator in the top bar with working menus.

<!--
  CONSTRAINTS
  Hard boundaries the solution must operate within. These are non-negotiable.
  Format: one bullet point per constraint.
-->
## Constraints

- Roost must not honor GNOME Shell and Mutter settings keys.
- The shell UI must stay pixel-painted; shell-host must not depend on GTK.
- The work must land in the single registered repo; it must not require cross-repo changes.
- Roost must not run real GNOME Shell extensions or attempt compatibility with them.

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

- [x] **Wallpaper follows Settings**
  Setting a wallpaper in GNOME Settings changes the Roost session background with no restart.
- [x] **Clock format follows Settings**
  The bar clock switches between 12- and 24-hour display according to the GNOME clock-format setting.
- [x] **Calendar opens from the clock**
  Activating the bar clock opens a month calendar; it closes on dismissal like other shell popups.
- [x] **Dock app lifecycle works**
  Pinning, launching, switching to, and quitting an app from the dock all behave, and running apps are marked.
- [x] **Indicator menus work**
  An indicator client with a menu shows that menu on activation and actions trigger in the client.

<!--
  TECHNICAL APPROACH
  High-level technical direction to guide the planning agent.
  Format: one bullet point per direction/steer.
-->
## Technical Approach

- Read shared GNOME settings through the platform settings backend and apply them to the painted shell surfaces.
- Keep Roost-only settings in a dedicated schema with a small native prefs UI.
- Build in slice order — settings read, bar growth, dock, indicators — so each slice ships independently against the existing layer-shell panel and control-socket window state.

<!--
  SUCCESS METRICS
  How you will know the feature is working well after delivery.
  Format: one bullet point per metric.
-->
## Success Metrics

- **Settings round trip** — a wallpaper and clock-format change in GNOME Settings is visible in the session with no Roost-side action.
- **Dock task completion** — pin, launch, switch, and quit from the dock succeed against real applications.
- **Indicator presence** — a standard indicator client shows its icon and menu in the bar.

<!--
  NON-GOALS
  Explicitly state what this spec does NOT cover.
  Format: one bullet point per exclusion.
-->
## Non-Goals

- Reimplementing the GNOME Settings application itself.
- The Rhai extension system (separate future spec).
