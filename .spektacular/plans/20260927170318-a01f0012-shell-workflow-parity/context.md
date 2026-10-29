---
created_date: "2026-09-27"
document_status: final
closed_date: "2026-09-28"
---

# Context: 20260927170318-a01f0012-shell-workflow-parity

## Current State Analysis

The 001 slice is implemented and pushed (agent tasks closed, human
T7 run open): control channel with token-gated intents
(`crates/compositor/src/control.rs:581`), live shell model feed
(`crates/shell-host/src/control.rs:155`), supervised shell binary
with the overview loop (`crates/shell-host/src/panel.rs:204`), window
manager with map/focus/move/resize/workspace
(`crates/compositor/src/windows.rs:62`), dynamic-capable model
(`crates/compositor/src/state.rs`), and a 100-run fault harness with
redacted artifacts. ShellModel (`crates/shell-host/src/model.rs:46`)
holds windows, workspaces, overview flag, and single selection fed by
snapshots and deltas. No search, launcher, notifications, quick
settings, D-Bus clients, or overview rendering exist yet; the panel
is a bare layer-shell strip and workspaces are single-policy.

## Per-Task Technical Notes

### Task: Overview triggers and window presentation

Shell owns `OverviewState { open, query, selection, workspace_view }`
in shell-host; triggers arrive as compositor input intents mapped next
to the overlay evdev table (`compositor/src/overlay.rs`, `keycode`
module) plus a hot-corner pointer region on the panel edge. Rendering
extends the live model feed (`shell-host/src/control.rs:155`,
`model.rs:46`) into a layer-shell overview surface; open/select calls
the existing `activate_selected` path (`control.rs:274`), so R6 holds
with no new focus seam. Tests: trigger mapping unit cases, headless
open/search/select journeys against hub + model, token replay denial
per focus path.

**Complexity**: High
**Token estimate**: ~35k
**Agent strategy**: single agent with reference-survey context; verify
against live hub journeys before closing

### Task: Launcher, search providers, and favorites

New `SearchProvider { id, query(text) -> Results, cancel() }` trait in
shell-host; desktop-entry and window providers implement it with
bounded result caps and generation counters for cancellation. No
parser exists in tree: choose a minimal freedesktop entry reader
(prefer an existing crate over hand-rolled parsing; record the choice
in the task changelog). Launch via spawn + startup-notification/xdg-
activation feedback; running instances resolve through the control
model for switch-to-focus. Favorites persist under
`$XDG_DATA_HOME`. Tests: slow/failing provider harnesses asserting
typing/focus/input stay live, cancel correctness, launch-feedback
shape.

**Complexity**: High
**Token estimate**: ~35k
**Agent strategy**: single agent; provider harness first, then
desktop-entry choice, then favorites persistence

### Task: Dynamic workspaces, switching, and window actions

Promote `StateModel` workspace ids to live sets: create-on-demand and
cleanup-empty policy in `compositor/src/state.rs`, move-window intent
commands through `control.rs:581` alongside activate. MRU ordering
derives shell-side from `WindowEntry.active` transitions (no
compositor change); Alt-Tab renders from it. Snap/maximize/fullscreen
follow the ledger entries; shortcuts and transient/dialog parents get
documented parity per entry. Tests: workspace lifecycle, MRU order
under focus churn, snap/maximize/fullscreen state assertions,
ledger-cited deviations.

**Complexity**: High
**Token estimate**: ~30k
**Agent strategy**: single agent; model policy first, then intents,
then MRU/switcher rendering

### Task: Panel, quick settings, and service states

Extend `shell-host/src/panel.rs:204` surfaces: clock (local),
indicators, and settings tiles behind
`QuickSetting { state: Loading | Ready | Disconnected | Error }`.
Service bindings (NM/UPower/PipeWire) attach only on presence
detection; every tile renders its unavailable state with no blocking
wait. Gestures follow the ledger where hardware allows (Smithay
`pointer_gestures` exists; keyboard paths are the automated proof).
Tests: state-transition cases per tile, absent-service harnesses with
bounded waits, no-hang assertions under dead services.

**Complexity**: Medium
**Token estimate**: ~25k
**Agent strategy**: 2 parallel agents (tiles/states + service seams),
sequential integration

### Task: Notification center behind the freedesktop seam

Local `Notification { id, title, body, actions[], urgency, history }`
store in shell-host with banner queue, DND gate, expand/dismiss, and
action dispatch; the public API mirrors the freedesktop notification
shape so the 004 daemon can serve it unchanged. Never focuses on
arrival; actions replay-guard like activation tokens with typed
errors; logs carry ids/counts only. Tests: queue/history/DND/action
rules, no-focus assertions, redaction scan mirroring the 001 harness
convention.

**Complexity**: Medium
**Token estimate**: ~25k
**Agent strategy**: single agent; store rules first, then banner
rendering, then seam review against 004 spec

### Task: Accessibility, localization, and ledger evidence

GTK4/libadwaita widgets carry AT-SPI exposure; add keyboard-only
paths for every journey, large-text/reduced-motion/high-contrast
coverage, touch targets, RTL mirroring, and localized strings.
Ledger convention: `LedgerEntry { journey, baseline, environment,
severity, owner, status }` recorded under docs with versioned
journey evidence. Tests: automated keyboard flows; human runs for
screen reader, contrast, motion, touch, RTL — recorded and reviewed
per A5, never hand-waved.

**Complexity**: Medium
**Token estimate**: ~25k
**Agent strategy**: single agent for automated flows; human runs
scheduled with the reviewer, evidence versioned per run

### Task: Live-session parity verification

Human runbook: `scripts/rwd-nested` journey plus real apps, live
services, gestures, and assistive tech on the pinned baseline (or the
recorded reviewer environment until 000 selects it). Record baseline
version, environment, and per-acceptance-item results in the ledger;
any failure without an accepted entry blocks 002 close.

**Complexity**: Low
**Token estimate**: ~5k
**Agent strategy**: human runbook execution; agent prepares the build
and records results

## Testing Strategy

Per task: overview — trigger mapping cases plus headless
open/search/select journeys with token denial; launcher — slow and
failing provider harnesses proving typing/focus/input stay live plus
cancel and feedback shape; workspaces — lifecycle, MRU order under
churn, snap/maximize/fullscreen states; panel — tile state
transitions plus absent-service bounded-wait harnesses proving no
hangs; notifications — queue/history/DND/action rules plus no-focus
and redaction scans; accessibility — automated keyboard flows with
human screen-reader/contrast/motion/touch/RTL runs recorded per A5.
The 001 control, snapshot, and supervision suites extend only where
new intents expose gaps. Journey evidence versions baseline and
environment per run.

## Project References

- 002 shell workflow parity spec (this plan's source of truth).
- 000 program baseline and spikes (Draft): provisional assumptions.
- 001 nested compositor/shell recovery plan (final): T1–T6 + fixes
  implemented and pushed; T7 human run open.
- 004 secure session spec: notification/service integration owner.
- ADRs 0001–0003 (Decided): Smithay pin, control envelope, nested
  supervision.
- Architecture: recovery-and-lock section of `docs/architecture.md`;
  `docs/nested-session.md` journey doc.
- Knowledge: `decisions/smithay-070-for-nested-slice.md`,
  `learnings/dconf-settings-interop.md`.
- Repo root: `/home/ubuntu/dev/rust-wayland-desktop`.
- References (read-only, original code only): niri, cosmic-comp,
  mutter under `/home/ubuntu/dev/references/`.

## Token Management Strategy

| Tier | Token Budget | Agent Strategy |
|------|-------------|----------------|
| Low | ~10k | Single agent, sequential |
| Medium | ~25k | 2-3 parallel agents |
| High | ~50k+ | Parallel analysis, sequential integration |

Task tiers: overview High (~35k); launcher High (~35k);
workspaces High (~30k); panel Medium (~25k, 2 parallel agents);
notifications Medium (~25k); accessibility Medium (~25k);
live verification Low (~5k, human).

## Migration Notes

N/A — greenfield workflows on 001 contracts; no data or API
migrations.

## Performance Considerations

- Search providers are async, bounded, and cancellable: typing,
  focus, and input never wait on a provider (A2).
- Overview layer-shell commits batch under result churn; ack-backlog
  behavior is an open question with a STOP rule.
- The 200 Hz shell control poll is provisional pacing for the shell
  binary only; compositor critical paths never wait on it.
- Change-log bursts from rapid intents may force gap resnapshots;
  frequency is measured during implementation with a STOP rule.
