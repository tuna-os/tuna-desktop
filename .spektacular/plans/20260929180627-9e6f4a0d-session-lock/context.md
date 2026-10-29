---
created_date: "2026-09-29"
document_status: final
closed_date: "2026-09-29"
---

# Context: 20260929180627-9e6f4a0d-session-lock

## Current State Analysis

No lock or idle code exists. The compositor Runtime owns
the loop, input entry, overlay, and window manager; the
greeter crate owns greetd auth; the control protocol
carries framed commands with revisioned snapshots. The
missing pieces are the flag, the surface, the trigger,
and the unlock mediation.

## Per-Task Technical Notes

### Task: Compositor lock flag and surface {#task-lock-flag}

Locked flag + idle accumulator on Runtime; exclusive lock
surface reusing the Overlay shape; timeout from input
timestamps and control-command set path; snapshots carry
the flag only. Tests: timeout locks with windows hidden,
shell reconnect stays locked. Complexity: high. Strategy:
agent with compositor focus.

### Task: Manual lock from the shell {#task-manual-lock}

Shell trigger sends the lock command over control;
locked display reads snapshots. Depends on the flag
task. Test: manual lock engages at once. Complexity:
low. Strategy: agent.

### Task: Greetd unlock path {#task-greetd-unlock}

Credential entry on the lock surface verified through
the greeter crate; success clears the flag and restores;
failures stay silent and locked. Auth success stubbed
at the client seam (no live greetd in CI). Depends on
the flag task. Tests: correct unlocks, triple-wrong
stays locked with no leak. Complexity: high. Strategy:
agent.

## Testing Strategy

Timeout-lock and restart-stays-locked behavioural tests;
unlock and triple-wrong tests with auth stubbed at the
client seam. Recast per task: flag task owns lock +
restart tests; manual task owns trigger test; unlock
task owns auth tests.

## Project References

- Spec `20260929180627-9e6f4a0d-session-lock`.
- notification-presence plan (split discipline).
- Repo root: `/home/ubuntu/dev/rust-wayland-desktop`.

## Token Management Strategy

| Tier | Token Budget | Agent Strategy |
|------|-------------|----------------|
| Low | ~10k | Single agent, sequential |
| Medium | ~25k | 2-3 parallel agents |
| High | ~50k+ | Parallel analysis, sequential integration |

Flag and unlock are High single-agent units; manual lock
is Low after the flag.

## Migration Notes

None: new mode; no existing behavior changes.

## Performance Considerations

Idle accrual is timestamp arithmetic on input; lock
surface is one overlay; no new timers beyond the
existing loop tick.
