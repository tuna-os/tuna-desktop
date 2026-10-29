---
created_date: "2026-09-29"
document_status: final
closed_date: "2026-09-29"
---

# Context: 20260929180600-6bc465db-quick-settings-toggles

## Current State Analysis

Tiles probe sysfs roots and report honest Ready /
Disconnected / Error / Loading states, but menus cannot
act: no D-Bus callers to platform services exist yet
(`tiles.rs:8`). The missing piece is toggle callers owned
by probe owners plus menu trigger/pending states.

## Per-Task Technical Notes

### Task: Network radio toggle {#task-network-toggle}

D-Bus caller owned by the network probe owner toggling the
radio flags; menu rows trigger, pending face, re-probe
confirms against the stub platform service. Behavioural
test flips stub state and asserts tile follows.
Complexity: medium. Strategy: agent.

### Task: Sound mute and volume {#task-sound-controls}

D-Bus caller owned by the sound probe owner for mute and
volume; interface resolved at implement time against the
stub, reporting back if unreachable. Mute/unmute round-trip
test. Complexity: medium. Strategy: agent.

### Task: Honest state and explanations {#task-honest-state}

Re-probe picks up outside changes within one tick (assert
tile update); absent services render explaining menu text
and skipped toggles; failed calls report why. Depends on
both toggle tasks. Complexity: low. Strategy: agent.

## Testing Strategy

Stub platform service on a private bus for toggle and
outside-change behavioural tests; unit tests for pending
faces and explanations. Recast per task: network owns the
radio-flip test; sound owns mute round-trip; honesty owns
outside-change and missing-service tests.

## Project References

- Spec `20260929180600-6bc465db-quick-settings-toggles`.
- notification-presence plan (proxy + stub patterns).
- Repo root: `/home/ubuntu/dev/rust-wayland-desktop`.

## Token Management Strategy

| Tier | Token Budget | Agent Strategy |
|------|-------------|----------------|
| Low | ~10k | Single agent, sequential |
| Medium | ~25k | 2-3 parallel agents |

Network and sound are Medium single-agent units in
parallel; honesty is Low after both.

## Migration Notes

None: additive callers; probes and tile shape unchanged.

## Performance Considerations

Short-timeout D-Bus calls off the menu path; pending face
covers latency; no new timers or threads.
