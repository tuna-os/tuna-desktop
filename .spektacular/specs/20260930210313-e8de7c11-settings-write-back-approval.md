---
created_date: "2026-09-30"
document_status: final
closed_date: "2026-09-30"
---

# Feature: 20260930210313-e8de7c11-settings-write-back-approval

<!--
  OVERVIEW
-->
## Overview

Roost surfaces gain safe write-back into the settings they already read: clock format toggle, wallpaper setter, and a home for Roost-only knobs that persists without hand-edited files. What the shell shows always matches what is stored.

<!--
  REQUIREMENTS
-->
## Requirements

- [x] **Clock format toggles from the shell**
  Users can switch 12/24-hour clock from a shell surface and the change persists.
- [x] **Wallpaper sets from the shell**
  Users can pick session wallpaper from the shell and it applies and persists.
- [x] **Roost-only knobs have a home**
  Roost-specific options persist and are adjustable without hand-editing configuration files.
- [x] **Writes stay in the shared universe**
  Shell writes land in the shared desktop settings so GNOME Settings shows the same values.

<!--
  CONSTRAINTS
-->
## Constraints

- Only keys the shell already reads, plus a small Roost-owned schema, are writable; shell and window-manager keys stay read-only.
- Writes never block paint; failures degrade to the last good snapshot with no user-facing error storm.

<!--
  ACCEPTANCE CRITERIA
-->
## Acceptance Criteria

- [x] **Clock toggle round-trips**
  Toggling clock format in the shell shows the new format on the bar promptly and the choice persists.
- [x] **Wallpaper setter applies**
  Setting wallpaper from the shell updates the session background and survives a session restart.
- [x] **Roost knob persists**
  Changing a Roost-only option, restarting the shell, shows the kept value.
- [x] **GNOME Settings agrees**
  Values written from Roost read back identically in GNOME Settings.

<!--
  TECHNICAL APPROACH
-->
## Technical Approach

- Decided direction the planner may adapt: write calls beside the snapshot refresh on the slow tick path, a tiny Roost prefs surface following existing UI patterns, and immediate snapshot re-read after each write.

<!--
  SUCCESS METRICS
-->
## Success Metrics

- Clock toggle round-trips through the shared settings backend in daily use.
- Wallpaper set from the shell appears across sessions.

<!--
  NON-GOALS
-->
## Non-Goals

- A full Settings app clone.
- Multi-user or permissions management for settings.
