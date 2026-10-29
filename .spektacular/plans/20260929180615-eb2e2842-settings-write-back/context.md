---
created_date: "2026-09-29"
document_status: final
closed_date: "2026-09-29"
---

# Context: 20260929180615-eb2e2842-settings-write-back

## Current State Analysis

The snapshot layer reads clock, wallpaper, and icon theme
through a backend trait with keep-on-absent refresh, but
nothing writes: no set calls exist on the trait and no
prefs surface exists for Roost knobs. Clock paint and the
wallpaper publish path consume the snapshot; they need
trigger surfaces, not rewrites.

## Per-Task Technical Notes

### Task: Clock format write-back {#task-clock-write}

Extend the backend trait with clock writes against the
shared schema; shell toggle triggers write + re-read; bar
repaints on the tick. Round-trip test through a controlled
backend asserts key flip and re-render. Complexity:
medium. Strategy: agent.

### Task: Wallpaper setter {#task-wallpaper-set}

Extend the backend trait with wallpaper-URI writes;
picker surface triggers write + session apply via the
existing publish path; restart test asserts persistence.
Independent of the clock task. Complexity: medium.
Strategy: agent.

### Task: Roost prefs surface {#task-roost-prefs}

Atomic JSON prefs file under XDG state plus shell editing
surface; change-restart-keeps-value test. Depends on the
clock task for the surface pattern. Complexity: low.
Strategy: agent.

## Testing Strategy

Controlled-backend round-trip tests for clock and
wallpaper; prefs change-restart test; failure-fallback
unit tests. Recast per task: clock owns round-trip +
re-render; wallpaper owns apply + restart; prefs owns
persistence.

## Project References

- Spec `20260929180615-eb2e2842-settings-write-back`.
- notification-presence plan (atomic files, stub tests).
- Repo root: `/home/ubuntu/dev/rust-wayland-desktop`.

## Token Management Strategy

| Tier | Token Budget | Agent Strategy |
|------|-------------|----------------|
| Low | ~10k | Single agent, sequential |
| Medium | ~25k | 2-3 parallel agents |

Clock and wallpaper are Medium units in parallel; prefs
is Low after clock.

## Migration Notes

None: additive writes; reads unchanged.

## Performance Considerations

Writes happen on user action, never on paint; re-read is
one backend pass; no new timers or threads.
