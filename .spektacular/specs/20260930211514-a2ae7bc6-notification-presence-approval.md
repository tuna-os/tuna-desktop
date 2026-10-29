---
created_date: "2026-09-30"
document_status: final
closed_date: "2026-09-30"
---

# Feature: 20260930211514-a2ae7bc6-notification-presence-approval

<!--
  OVERVIEW
-->
## Overview

Roost surfaces app notifications in the session: a bar presence marker, dismissible banners with working actions, and a queue that survives shell restarts. GNOME apps that notify over the standard desktop interface will reach the user instead of vanishing.

<!--
  REQUIREMENTS
-->
## Requirements

- [x] **Bar shows notification presence**
  Users can see from the top bar that unread notifications exist.
- [x] **Banners appear for new notifications**
  A new notification shows a dismissible banner without stealing keyboard focus.
- [x] **Default actions work**
  Activating a banner's default action invokes it on the owning app and dismisses the banner.
- [x] **Close actions work**
  Users can dismiss a banner without activating it, and the owning app is told it closed.
- [x] **Queue survives shell restarts**
  Notifications still unread when the shell process restarts are still listed afterwards.
- [x] **Flooding clients are bounded**
  A client sending notifications without limit cannot grow shell memory without bound or hang the bar; excess items are dropped oldest-first with no session impact.

<!--
  CONSTRAINTS
-->
## Constraints

- Banners reuse the existing banner layer surface and the pixel-painted style; no toolkit in the shell process.
- Intake speaks the standard desktop notifications interface so unmodified GNOME apps work.
- Intake never blocks the shell event loop and never spawns processes per notification.

<!--
  ACCEPTANCE CRITERIA
-->
## Acceptance Criteria

- [x] **Unread marker visible**
  With an unread notification queued, the bar shows the presence marker; with none queued, it does not.
- [x] **Banner shows and dismisses**
  Sending a test notification shows a banner that dismisses on press without moving keyboard focus.
- [x] **Default action fires**
  Pressing a banner's action row invokes the owning app's action exactly once.
- [x] **Restart keeps the queue**
  Killing and restarting the shell process preserves the unread list shown before the restart.
- [x] **Flood stays bounded**
  Sending ten thousand notifications keeps shell memory flat and the bar responsive; newest items are kept.

<!--
  TECHNICAL APPROACH
-->
## Technical Approach

- Suggested direction: serve the standard notifications name on the session bus from the shell host, back the center with a bounded persistent queue drained on the slow tick, paint banners through the existing banner canvas, and route default/close reasons back over the bus.

<!--
  SUCCESS METRICS
-->
## Success Metrics

- App notifications arrive, banner, and act in daily use.
- Floods and restarts never lose the unread queue or hang the bar.

<!--
  NON-GOALS
-->
## Non-Goals

- Notification history browser or do-not-disturb schedules: future work.
- Inline replies and buttons beyond default/close actions.
- Sound or haptic alerts on notification arrival.
