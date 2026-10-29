---
created_date: "2026-09-29"
document_status: draft
---

# Feature: 20260929180600-6bc465db-quick-settings-toggles

## Overview

Roost tile menus gain working toggles for the services they report
on: network radio, sound mute and volume, and power actions the
platform exposes. State shown and state changed stay in one place,
through the same honest-state tile shape.

## Requirements

- [x] **Network toggles work**
  Users can switch wireless and wired networking on and off from
  the network tile menu.
- [x] **Sound controls work**
  Users can mute and set output volume from the sound tile menu.
- [x] **Tile state follows reality**
  After a toggle, the tile shows the service's actual new state,
  including changes made outside Roost.
- [x] **Unavailable services explain**
  A toggle for an absent service or a failed call reports why
  instead of silently doing nothing.

## Constraints

- Toggles act through the platform services (network manager,
  sound server, power daemon), never by writing config files.
- Toggle calls never block the shell event loop; slow calls show
  a pending tile state.
- The tile state shape (Ready/Disconnected/Error/Loading) is
  reused, not replaced.

## Acceptance Criteria

- [x] **Wifi toggles the radio**
  Toggling wifi off in the menu changes the real radio state,
  verified against the platform service, and the tile follows.
- [x] **Mute flips output**
  Muting from the menu silences output and unmuting restores it.
- [x] **Outside changes appear**
  Changing wifi state outside Roost updates the tile within one
  poll tick.
- [x] **Missing service explains**
  With no sound server present, the sound menu says so instead of
  offering dead toggles.

## Technical Approach

Decided direction the planner may adapt: D-Bus calls to the
platform services from the existing tile-probe owners, with menu
rows as the trigger surface and re-probe confirming the new state.

## Success Metrics

- Wifi toggle flips the stub platform service and the tile
  follows, in a behavioural test.
- Outside state changes appear within one tick in test.

## Non-Goals

- Full network list and connection editor (a GNOME Settings job).
- Per-app volume mixer.
- Power profiles beyond what the platform daemon exposes.
