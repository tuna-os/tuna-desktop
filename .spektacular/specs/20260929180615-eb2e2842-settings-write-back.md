---
created_date: "2026-09-29"
document_status: draft
---

# Feature: 20260929180615-eb2e2842-settings-write-back

## Overview

Roost surfaces gain safe write-back into the settings they already
read: clock format toggle, wallpaper setter, and a home for
Roost-only knobs that persists without hand-edited files. Writes go
through the same snapshot layer that reads do, so paint code never
touches the settings backend directly.

## Requirements

- [x] **Clock format toggles from the shell**
  Users can switch 12/24-hour clock from a shell surface and the
  change persists.
- [x] **Wallpaper sets from the shell**
  Users can pick session wallpaper from the shell and it applies
  and persists.
- [x] **Roost-only knobs have a home**
  Roost-specific options persist and are adjustable without
  hand-editing configuration files.
- [x] **Writes stay in the shared universe**
  Shell writes land in the shared desktop settings so GNOME
  Settings shows the same values.

## Constraints

- Only keys the shell already reads, plus a small Roost-owned
  schema, are writable; shell and window-manager keys stay
  read-only.
- Writes never block paint; failures degrade to the last good
  snapshot with no user-facing error storm.

## Acceptance Criteria

- [x] **Clock toggle round-trips**
  Toggling clock format in the shell flips the shared key and the
  bar re-renders within one tick.
- [x] **Wallpaper setter applies**
  Setting wallpaper from the shell updates the session background
  and survives a session restart.
- [x] **Roost knob persists**
  Changing a Roost-only option, restarting the shell, shows the
  kept value.
- [x] **GNOME Settings agrees**
  Values written from Roost read back identically in GNOME
  Settings.

## Technical Approach

Decided direction the planner may adapt: write calls beside the
snapshot refresh on the slow tick path, a tiny Roost prefs surface
following existing UI patterns, and immediate snapshot re-read
after each write.

## Success Metrics

- Clock toggle round-trips through a controlled settings backend
  in test.
- Wallpaper set from the shell appears in the session, asserted by
  the wallpaper round-trip test extended to writes.

## Non-Goals

- A full Settings app clone.
- Writing window-manager or shell-extension keys.
- Multi-user or permissions management for settings.
