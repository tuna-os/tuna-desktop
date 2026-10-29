---
created_date: "2026-09-30"
document_status: final
closed_date: "2026-09-30"
---

# Feature: 20260930211259-a483f8c5-quick-settings-toggles-approval

<!--
  OVERVIEW
-->
## Overview

Roost tile menus gain working toggles for the services they report on: network radio, sound mute and volume. State shown and state changed stay in one place; tiles keep the same look and states.

<!--
  REQUIREMENTS
-->
## Requirements

- [x] **Network toggles work**
  Users can switch wireless networking on and off from the network tile menu; wired link state is reported but never dropped by a wifi toggle.
- [x] **Sound controls work**
  Users can mute and set output volume from the sound tile menu.
- [x] **Tile state follows reality**
  After a toggle, the tile shows the service's actual new state, including changes made outside Roost.
- [x] **Unavailable services explain**
  A toggle for an absent service or a failed call reports why instead of silently doing nothing.

<!--
  CONSTRAINTS
-->
## Constraints

- Toggles act through the platform services (network manager, sound server, power daemon), never by writing config files.
- Toggle calls never block the shell event loop; slow calls show a pending tile state.
- The tile state shape (Ready/Disconnected/Error/Loading) is reused, not replaced.

<!--
  ACCEPTANCE CRITERIA
-->
## Acceptance Criteria

- [x] **Wifi toggles the radio**
  Toggling wifi off in the menu changes the real radio state and the tile follows.
- [x] **Mute flips output**
  Muting from the menu silences output and unmuting restores it.
- [x] **Volume nudges output level**
  Volume up and down rows raise and lower the output level stepwise.
- [x] **Outside changes appear**
  Changing wifi state outside Roost updates the tile within a few seconds.
- [x] **Missing service explains**
  With no sound server present, the sound menu says so instead of offering dead toggles.

<!--
  TECHNICAL APPROACH
-->
## Technical Approach

- Suggested direction: D-Bus calls to the platform services from the existing tile-probe owners, with menu rows as the trigger surface and re-probe confirming the new state.

<!--
  SUCCESS METRICS
-->
## Success Metrics

- Wifi, mute, and volume rows work first time in daily use.
- Tiles never show a stale service state.
- Wifi toggles never drop a wired link.

<!--
  NON-GOALS
-->
## Non-Goals

- Full network list and connection editor (a GNOME Settings job).
- Per-app volume mixer.
- Power profiles beyond what the platform daemon exposes.
