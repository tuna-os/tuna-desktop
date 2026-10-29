---
created_date: "2026-09-29"
document_status: draft
---

# Feature: 20260929180510-b20ab21e-notification-presence

## Overview

Roost surfaces app notifications in the session: a bar presence
marker, dismissible banners with working actions, and a queue that
survives shell restarts. GNOME apps that notify over the standard
desktop interface will reach the user instead of vanishing.

## Requirements

- [x] **Bar shows notification presence**
  Users can see from the top bar that unread notifications exist.
- [x] **Banners appear for new notifications**
  A new notification shows a dismissible banner without stealing
  keyboard focus.
- [x] **Default actions work**
  Activating a banner's default action invokes it on the owning app
  and dismisses the banner.
- [x] **Close actions work**
  Users can dismiss a banner without activating it, and the owning
  app is told it closed.
- [x] **Queue survives shell restarts**
  Notifications still unread when the shell process restarts are
  still listed afterwards.
- [x] **Flooding clients are bounded**
  A client sending notifications without limit cannot grow shell
  memory without bound or hang the bar; excess items are dropped
  oldest-first with no session impact.

## Constraints

- Banners reuse the existing banner layer surface and the
  pixel-painted style; no toolkit in the shell process.
- Intake speaks the standard desktop notifications interface so
  unmodified GNOME apps work.
- Intake never blocks the shell event loop and never spawns
  processes per notification.

## Acceptance Criteria

- [x] **Unread marker visible**
  With an unread notification queued, the bar shows the presence
  marker; with none queued, it does not.
- [x] **Banner shows and dismisses**
  Sending a test notification shows a banner that dismisses on
  press without moving keyboard focus.
- [x] **Default action fires**
  Pressing a banner's action row invokes the owning stub client's
  action callback exactly once.
- [x] **Restart keeps the queue**
  Killing and restarting the shell process preserves the unread
  list shown before the restart.
- [x] **Flood stays bounded**
  Sending ten thousand notifications keeps shell memory flat and
  the bar responsive; newest items are kept.

## Technical Approach

Decided direction the planner may adapt: serve the standard
notifications name on the session bus from the shell host,
back NotificationCenter with a bounded persistent queue drained on
the slow tick, paint banners through the existing banner canvas,
and route default/close reasons back over the bus.

## Success Metrics

- A stub client notification shows a banner and fires its action
  in a behavioural test.
- A flood of ten thousand notifications keeps memory flat and the
  bar responsive in test.
- Shell restart preserves the unread queue, asserted by test.

## Non-Goals

- Notification history browser or do-not-disturb schedules: future work.
- Inline replies and buttons beyond default/close actions.
- Sound or haptic alerts on notification arrival.
