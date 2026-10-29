---
created_date: "2026-09-29"
document_status: final
closed_date: "2026-09-29"
---

# Research: 20260929180544-421dbfd8-overview-app-launch

## Alternatives considered and rejected

### Option A: Overview-local spawner

Separate fork/feedback bookkeeping in the overview path.

**Rejected**: duplicates the tracker; two spawners drift
(spec constraint forbids it).

### Option B: Launch on highlight

Spawn without press.

**Rejected**: accidental spawns during keyboard navigation.

### Option C: Blocking spawn on UI thread

Spawn inline in press handling.

**Rejected**: fork/exec can stall; dock path already avoids
this.

## Chosen approach — evidence

Press router into `LaunchTracker` (`apps.rs:315,326`),
`SearchResult::launch` intent (`search.rs:60`), dock shape
(`dock.rs:286`), overview state (`panel.rs:860,1152`),
fail-closed precedent (`apps.rs:509`).

## Files examined

- `shell-host/src/apps.rs:270,315` — spawn, tracker.
- `shell-host/src/search.rs:49,92` — result model.
- `shell-host/src/dock.rs:286` — dock press shape.
- `shell-host/src/panel.rs:860,1152` — overview, query.

## External references

- freedesktop.org Desktop Entry Specification — Exec
  semantics for the bad-entry case.

## Prior plans / specs consulted

- icon-theme-lookup, notification-presence plans.
- overview-app-launch spec.

## Open assumptions

- Tracker reachable from press path (verify at implement).
- `SearchAction` covers runnable actions (scope if wider).
- Bad Exec detected at spawn (fail-closed).

## Drafting assumptions

### Reuse tracker, no parallel spawner (discovery)

- **Decision**: route overview presses through LaunchTracker
- **Rationale**: spec constraint forbids a parallel spawner;
  dock precedent is tested
- **Rejected**: overview-local spawn path

### Fail-closed at spawn time (discovery)

- **Decision**: detect bad Exec via spawn failure, surface inline
- **Rationale**: matches launch_missing_binary_fails_closed precedent
- **Rejected**: Exec pre-validation (second parser, drift risk)

### Tracker shared, press commits (architecture)
- **Decision**: overview presses feed the shared tracker; press is the commit gesture
- **Rationale**: one spawner, one feedback path; no accidental launches
- **Rejected**: local spawner, launch-on-highlight

### Three tasks, press router first (tasks)
- **Decision**: press-launch, action-run, inline-failure as three agent tasks
- **Rationale**: action and failure both build on the router; parallelize after task 1
- **Rejected**: single task (mixes three acceptance outcomes)

## Rehydration cues

- Re-read `apps.rs:270-340`, `dock.rs:286-330`.
- `cargo test -p roost-shell-host launch`.
- Spec: `20260929180544-421dbfd8-overview-app-launch`.
