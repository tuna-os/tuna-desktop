---
created_date: "2026-09-29"
document_status: final
closed_date: "2026-09-29"
---

# Context: 20260929180544-421dbfd8-overview-app-launch

## Current State Analysis

Search results already carry launch intent
(`SearchResult::launch`) and the dock owns a tested
tracked-launch path (`LaunchTracker`, `dock_press`), but
nothing connects an overview press to a spawn: results are
informational only. Overview open state and query plumbing
exist; the missing piece is the press router plus inline
failure rendering.

## Per-Task Technical Notes

### Task: Overview press launches apps {#task-press-launch}

Wire the overview result press to resolve the app entry by
id and call `LaunchTracker::launch`, mirroring `dock_press`;
dismiss overview on Ok, keep tracker-owned feedback.
Behavioural test types a fixture app name, presses, asserts
spawn plus starting feedback. Complexity: medium. Strategy:
agent.


### Task: Overview action results run {#task-action-run}

Extend the router to runnable `SearchAction`s; run and
dismiss on success. Depends on the press-launch task.
Test asserts the action command runs. Complexity: low.
Strategy: agent.


### Task: Failed launches report inline {#task-inline-failure}

On spawn Err, keep overview open and render failure on the
result; fail-closed precedent at `apps.rs:509`. Depends on
the press-launch task. Test asserts open overview plus
inline failure. Complexity: low. Strategy: agent.

## Testing Strategy

Behavioural type-and-press test with a fixture app plus a
bad-Exec inline-failure test; unit tests on press routing
and failure state. Recast per task: press task owns spawn +
feedback tests; action task owns action-run test; failure
task owns open-overview + inline-failure tests.

## Project References

- Spec `20260929180544-421dbfd8-overview-app-launch`.
- icon-theme-lookup and notification-presence plans
  (fixture tests, stub patterns, paint-key discipline).
- Repo root: `/home/ubuntu/dev/rust-wayland-desktop`.

## Token Management Strategy

| Tier | Token Budget | Agent Strategy |
|------|-------------|----------------|
| Low | ~10k | Single agent, sequential |
| Medium | ~25k | 2-3 parallel agents |

Press task is Medium single-agent; action and failure are
Low and parallelize after it.

## Migration Notes

None: additive wiring; no existing behavior changes.

## Performance Considerations

Press handling reuses the existing spawn path; no new
timers, threads, or query-time work. Search stays
non-blocking per the spec constraint.
