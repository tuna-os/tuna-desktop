---
created_date: "2026-09-30"
document_status: final
closed_date: "2026-09-30"
---

# Test plan: 20260929180642-12b43038-multi-monitor

Both success metrics are covered by automated behavioural tests;
no manual procedure is executable against the nested backend today
(it presents one window). The suites carrying each metric:

- Two-output layout correctness — `roost-compositor --test outputs`
  (inventory, geometry, failover), `--test layer` (bound-panel
  end-to-end arrange at its output's size and offset), shell-host
  `output_plan` planner tests + control `Outputs` intake test,
  wallpaper two-size cover tests.
- Hotplug window preservation — `roost-compositor --test migration`
  (floating shifts, maximized re-resolution incl. primary removal,
  no-op cases, add/remove round-trip, real global teardown).

## Deferred hardware procedure (no manual run required now)

The plan records one deliberate gap: no live-GPU multi-head tests
(CI has one virtual backend). When a presenting backend with two or
more outputs lands, validate once on real hardware before calling
multi-monitor done:

- **What**: two physical displays on one session.
- **How**: `cargo run -p roost-compositor --bin roost-session`,
  open one window per display, unplug the secondary, re-plug it.
- **Expected**: panel and wallpaper on each display, dock on the
  primary only; after unplug every window is visible on the
  remaining display with focus and workspaces intact; after
  re-plug the layout restores with no restart and no error spam
  in the session log; neither display visibly stalls while both
  repaint (frame-budget sharing).
- **Who / when**: pre-release hardware check, run by whoever owns
  the release on a dual-display machine.
