---
created_date: "2026-09-29"
document_status: final
closed_date: "2026-09-29"
---

# Context: 20260929180510-b20ab21e-notification-presence

## Current State Analysis

`NotificationCenter` exists as a tested in-memory model
(banners, history, DND, actions) but is wired to nothing: no
bus intake serves `org.freedesktop.Notifications`, no bar tile
reads the unread count, and the queue is lost on restart. The
banner layer surface (`BANNER_NAMESPACE`), its reconcile entry,
and the banner pixel canvas all exist and are reused as-is.

## Per-Task Technical Notes

### Task: Intake service on the notifications bus {#task-intake-service}

New module beside watcher: zbus blocking object serving
`org.freedesktop.Notifications` at a fixed path; `Notify`
maps hints/timeout to `NotificationCenter::notify`, returns
the id; `CloseNotification` calls `dismiss`; `GetCapabilities`
returns actions+persistence; banner-side invoke/dismiss emit
`ActionInvoked`/`NotificationClosed`. Host `ensure()` mirrors
`WatcherBus::ensure` (serve_at, DoNotQueue name request) and
drains on the slow tick. Tests extend the private-bus stub
with a notifying client asserting banner, single-fire action,
and close reason. Complexity: medium. Strategy: agent.

### Task: Banner paint and press routing {#task-banner-routing}

Wire the existing reconcile entry to the center's visible
banners; banner canvas paints summary/body; press hit-testing
maps action row vs dismiss affordance; focus never leaves the
current surface. Depends on the intake task for live items.
Tests assert pixels, press outcomes, and focus stability.
Complexity: medium. Strategy: agent.

### Task: Bar presence marker {#task-presence-marker}

Bar tile reads `unread_count()`; tile paint key gains the
count so the strip repaints on change; no marker at zero.
Depends on the intake task for queue population. Tests assert
marker present/absent and repaint within one tick.
Complexity: low. Strategy: agent.

### Task: Queue persistence and flood bounds {#task-persistence-bounds}

Center gains `load`/`save` over a versioned JSON queue file
under the XDG state dir, atomic rename on mutation, loaded at
host start; flood test drives 10k notifies asserting bounded
lengths, flat allocation, and stable paint keys; restart test
saves, rebuilds the host, and asserts the unread list.
Depends on the intake task. Complexity: medium. Strategy:
agent.

## Testing Strategy

Unit tests on the center and persistence round-trip; stub-client
behavioural tests over a private bus address for banner, action,
close, flood (10k, flat allocation, stable paint keys), and
restart rehydrate. Recast per task: intake task owns the stub
banner/action/close tests; banner task owns pixels/press/focus
tests; marker task owns present/absent/repaint tests;
persistence task owns round-trip, flood, and restart tests.

## Project References

- Spec `20260929180510-b20ab21e-notification-presence`
  (requirements, constraints, metrics, non-goals).
- settings-compat-shell-surfaces plan (banner surface, tiles,
  slow tick); icon-theme-lookup plan (host cache, stub tests).
- Repo root: `/home/ubuntu/dev/rust-wayland-desktop`
  (registered repo `rust-wayland-desktop`).

## Token Management Strategy

| Tier | Token Budget | Agent Strategy |
|------|-------------|----------------|
| Low | ~10k | Single agent, sequential |
| Medium | ~25k | 2-3 parallel agents |

Intake, banner, and marker tasks are Low/Medium single-agent
units; persistence+bounds is Medium (flood test is the heavy
assertion). Tasks 2-4 parallelize after task 1.

## Migration Notes

None: additive feature; the center's existing API is extended,
not changed.

## Performance Considerations

Intake is tick-drained blocking I/O with bounded per-call
decode; the queue is length-capped; persistence writes one
small JSON file per mutation. No new timers or threads.
