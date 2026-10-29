---
created_date: "2026-09-29"
document_status: final
closed_date: "2026-09-29"
---

# Research: 20260929180510-b20ab21e-notification-presence

## Alternatives considered and rejected

### Option A: Async zbus intake task

A dedicated async task serving the bus name.

**Rejected**: every zbus call in shell-host is already
blocking (`watcher.rs:438,466`); a second concurrency model
for one endpoint is unjustified.

### Option B: Sidecar notification daemon

Reuse mako/dunst as a child process.

**Rejected**: extra process plus toolkit dependency, against
the pixel-painted direction and the no-spawn constraint.

### Option C: Unbounded queue with history browser

Full history UI on top of an unbounded store.

**Rejected**: history browser is an explicit spec non-goal;
bounds stay at `MAX_BANNERS=3` / `MAX_HISTORY=50`.

## Chosen approach — evidence

Serve `org.freedesktop.Notifications` with the
`WatcherBus::ensure()` pattern (`watcher.rs:462-491`),
translate into `NotificationCenter` (`notifications.rs:112`),
paint via the banner surface (`panel.rs:87-88,317-329`) and
canvas (`overview.rs:460-486`), persist with serde_json
(already a shell-host dep), drain on the 2s slow tick
(`panel.rs:906-921`).

## Files examined

- `shell-host/src/notifications.rs:112` — center model, caps,
  DND, actions; tested, unwired.
- `shell-host/src/watcher.rs:462` — blocking serve + name
  request template.
- `shell-host/src/watcher.rs:1243` — private-bus stub test
  pattern.
- `shell-host/src/panel.rs:87,317,583` — banner namespace,
  surface, reconcile.
- `shell-host/src/overview.rs:460` — banner canvas.
- `shell-host/src/panel.rs:906` — slow-tick drain point.

## External references

- freedesktop.org Desktop Notifications Specification —
  the intake wire contract unmodified GNOME apps speak.
- zbus blocking API docs — matches in-tree usage; no new
  dependency.

## Prior plans / specs consulted

- settings-compat-shell-surfaces plan: banner surface, tile
  keys, slow tick.
- icon-theme-lookup plan: host cache, stub behavioural tests.
- notification-presence spec: scope, metrics, non-goals.

## Open assumptions

- No rival daemon holds the bus name in a Roost session;
  silent yield on DoNotQueue failure.
- XDG state home writable; atomic rename suffices for
  queue durability.
- Flood test without live compositor (paint-key stability,
  not frame timing).
- Bus round-trip cost negligible (verified in test).

## Drafting assumptions

### Blocking intake over async (discovery)

- **Decision**: plan blocking zbus intake drained on the slow
  tick, not an async task
- **Rationale**: every zbus call in shell-host is already
  blocking; matches the tray host precedent
- **Rejected**: async runtime for one endpoint (second
  concurrency model)

### No concurrent daemon fight assumed (discovery)

- **Decision**: assume no other notification daemon holds the
  bus name; yield silently on DoNotQueue failure
- **Rationale**: Roost session replaces the GNOME shell, which
  is the usual name holder
- **Rejected**: name-contention UI (out of scope)

### Flood test without live compositor (discovery)

- **Decision**: 10k-flood test asserts model bounds +
  paint-key stability, not frame timing
- **Rationale**: CI has no live compositor; frame timing is not
  measurable there
- **Rejected**: frame-timing gate (unmeasurable in CI)

### Intake inside shell host, blocking (architecture)
- **Decision**: bus object lives in shell-host, blocking zbus, tick-drained
- **Rationale**: matches every existing zbus call; no new runtime, no sidecar
- **Rejected**: async task, external daemon (see research alternatives)

### Four tasks, two milestones (tasks)
- **Decision**: intake, banner routing, marker, persistence+bounds as four agent tasks
- **Rationale**: banner/marker/persistence all depend on intake but are independent of each other, so they parallelize after task 1
- **Rejected**: folding marker into intake (mixes wire and paint outcomes)

## Rehydration cues

- Re-read `notifications.rs:112-251` and
  `watcher.rs:462-530` before designing intake.
- `cargo test -p roost-shell-host notifications` for the
  model baseline.
- Spec: `20260929180510-b20ab21e-notification-presence`.
