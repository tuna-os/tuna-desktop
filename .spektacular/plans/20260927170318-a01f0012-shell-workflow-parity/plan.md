---
created_date: "2026-09-27"
document_status: final
closed_date: "2026-09-28"
---

# Plan: 20260927170318-a01f0012-shell-workflow-parity

<!-- Metadata -->
<!-- Created: 2026-09-28T16:20:55Z -->
<!-- Commit: 10e8339 -->
<!-- Branch: main -->
<!-- Repository: https://github.com/hanthor/rust-wayland-desktop -->

## Overview

This plan builds the daily-driver shell workflows — overview, search
and launch, panel and quick settings, dynamic workspaces, and
notifications — as a thick shell client over the tested 001
contracts. It closes the gap between a booting nested session and a
GNOME-baseline desktop a user can actually work in.

## Conventions

No project conventions apply to this feature.

## Architecture & Design Decisions

Build every 002 workflow as a thick shell client over the 001
contracts, keeping the compositor a validator. shell-host grows the
overview (Super/hot-corner/Activities triggers routed from compositor
input, window/workspace/search rendering from its live model),
desktop-entry launching with async bounded search providers, a local
notification banner/history store behind the freedesktop-shaped API,
and quick-settings surfaces that lead with loading/disconnected/error
states; the compositor gains only trigger routing, dynamic-workspace
model operations, and intent commands (activate/move/select) gated by
the existing token policy. No per-motion IPC: grabs and selection run
client-side, exactly as the reference compositors do.

The key trade-off is slice discipline against the parity ambition:
notifications stay a local store (the bus daemon is 004's, and no
D-Bus client exists in tree), service-backed quick settings follow per
service with presence detection rather than blocking the slice, and
gestures/screen-reader runs stay human-reviewed while keyboard flows
and state machines are automated. This beats the in-compositor
overview (it would duplicate the GLES path and break the supervised
shell/recovery story from 001) and beats building D-Bus daemons now
(it would stand up privileged infrastructure 004 owns, against
services absent in CI). Rejected paths with evidence are in
research.md#alternatives-considered-and-rejected.

No project conventions apply to this feature: the repo's
always-applied knowledge load returned no convention entries, so the
shape follows the spec, ADRs 0001–0003, and the contracts encoded in
code and tests.

## Component Breakdown

- **Overview shell** (changed): owns the Activities trigger
  handling, overview open/close state, and window/workspace/search
  rendering from the live shell model. It consumes the control feed
  and issues only intent commands, never motion streams.
- **Launcher and search** (new): owns desktop-entry discovery,
  favorites pinning, async bounded search providers with
  cancellation, launch feedback, and switch-to-instance. Results
  render from the overview shell and never block typing or focus.
- **Panel and quick settings** (changed): owns the clock, system
  indicators, and settings surfaces with explicit loading,
  disconnected, and error states. Live service bindings attach per
  service behind presence detection; the slice never hangs on an
  absent service.
- **Notification center** (new): owns the local banner queue,
  history, Do Not Disturb, actions, and dismissal, behind a
  freedesktop-shaped API that the 004 bus daemon will implement. No
  focus theft, ever.
- **Window and workspace engine** (changed): owns dynamic workspace
  operations, move-window actions, Alt-Tab MRU, snap/maximize/
  fullscreen semantics, and keyboard shortcuts. The compositor keeps
  focus/geometry authority; the shell keeps intent and presentation.
- **Parity ledger and journeys** (new): owns the discrepancy ledger
  (baseline version, environment, severity, owner per entry) and the
  scripted plus human-reviewed journey matrix that A1/A5/A6 verify
  against.

## Data Structures & Interfaces

Reuse the 001 wire and model contracts; add only the workflow
seams between shell presentation and compositor authority.

```text
OverviewState { open, query, selection, workspace_view }
SearchProvider { id, query(text) -> Results, cancel() }
SearchResult { title, app_id, action: Launch | Focus(window) }
FavoriteEntry { app_id, pinned }
Notification { id, title, body, actions[], urgency, history }
QuickSetting { id, state: Loading | Ready | Disconnected | Error }
Intent { ActivateWindow(window, token) | MoveWindow | SelectWorkspace | ToggleOverview }
LedgerEntry { journey, baseline, environment, severity, owner, status }
```

`OverviewState` is shell-local UI state fed by the control model;
`SearchProvider` is the async bounded trait every provider (entries,
windows, future extension results) implements with cancellation.
`Notification`/`QuickSetting` are local presentation types behind the
freedesktop-shaped seams 004 will serve. `Intent` reuses the control
channel's token-gated commands — every focus change still clears the
001 token policy. `LedgerEntry` makes each parity deviation explicit
with its baseline, environment, severity, and owner.

## Implementation Detail

Introduce a shell-presentation pattern: the shell-host crate grows
UI-state modules (overview, search, notifications, quick settings)
that read the tested control model and render into layer-shell
surfaces, while the compositor keeps owning protocol truth. The one
new stateful pattern is the async provider trait — every search and
service source implements query-plus-cancel with bounded results, so
slow providers degrade to loading states instead of blocking typing,
focus, or frames. Dynamic workspaces promote the model's workspace
ids from labels to live sets with create/cleanup policy.

A developer reading the result sees the 001 shape extended, not
replaced: backend events in, model changes out on the compositor
side; snapshot truth in, intent commands out on the shell side. New
code follows the existing nonblocking, fail-closed, redacted-
diagnostics patterns; the only genuinely new seam is the provider
trait plus the ledger convention for recording parity deviations with
baseline, environment, severity, and owner.

## Dependencies

- 002 shell workflow parity spec: source of truth for R1-R8 and
  A1-A6; no spec changes assumed, otherwise STOP.
- 001 nested compositor/shell recovery (agent tasks closed, T7 human
  run open): provides control/token/supervision/overlay contracts;
  no changes, 002 builds on them as-is.
- 000 baseline and spikes (Draft): provides provisional assumptions
  only — no pinned baseline and no settled toolkit verdict yet, so
  journeys record reviewer environment and a STOP rule guards
  parity claims.
- 004 secure session (spec): owns notification/service integration
  and production identity; 002 builds local-store-first seams and
  must not create the bus daemon or privileged APIs.
- Existing workspace crates (`compositor`, `shell-control-schema`,
  `shell-host`): provide tested models and the control channel;
  compositor gains trigger routing + dynamic workspaces, shell-host
  gains the workflow UI, schema stays frozen absent proven wire gap.
- GTK4/libadwaita workspace pins: provide shell rendering and
  AT-SPI/IME behavior; no change, provisional pending the 000 spike.
- Pinned GNOME baseline plus target distro/GPU: provides the
  comparison behavior for the ledger; missing — journeys record
  environment until it is selected.
- System services (NetworkManager, UPower, PipeWire): provide live
  quick-settings content; absent in CI, so unavailable states are
  the automated proof and live runs stay manual.
- Design documents this plan was built on: none.

## Testing Approach

Concentrate automation on state machines and contracts: overview
open/close/selection, search provider cancellation and bounds, MRU
ordering, notification queue/history/DND rules, quick-setting state
transitions, dynamic-workspace policy, and token-gated focus for
every focus-changing action. The 001 suites already cover control
framing, snapshots, and restart budgets; extend them only where
workflow intents expose gaps. Journey evidence (screenshots/
recordings plus the ledger) is versioned per run with baseline
version and environment.

Load-bearing guarantees in plain language: typing never waits on a
provider; every focus change clears the token policy; the shell
renders only compositor truth; unavailable services show honest
states instead of hanging; notifications never steal focus; ledger
entries always name baseline, environment, severity, and owner.
Tests follow project conventions: deterministic headless cases with
bounded pump budgets, manual clocks, and redacted artifacts.

Acceptance mapping (the spec carries no separate Success Metrics
section, so A1-A6 are the handoff): A1 journey matrix — automated
state-machine journeys plus human-reviewed runs with versioned
evidence, Manual where real clients or services are required. A2
search responsiveness — automated slow/failing-provider cases
asserting typing, focus, and input stay live. A3 existing services —
automated unavailable/error-state cases; live-service runs Manual.
A4 input and focus — automated stale/replay token cases for every
intent. A5 accessibility/localization — automated keyboard flows;
screen-reader, large-text, reduced-motion, contrast, touch, and RTL
runs Manual, recorded and reviewed. A6 evidence — the ledger and
versioned artifacts are reviewed, not asserted.

## Milestones & Tasks

### Milestone 1: An overview that opens, searches, and launches

**What changes**: pressing Super, clicking Activities, or pushing the
pointer into the hot corner opens an overview showing windows,
workspaces, favorites, and a search field; typing filters
desktop-entry and window results without ever stalling, Enter launches
or focuses, and Escape returns to the prior app. Validation: scripted
open/search/launch/return journeys plus a human pass with versioned
evidence.

#### - [ ] Task: Overview triggers and window presentation
**Id:** 998d8d30-db43-42f4-b242-f6bb4a6c5048
**Repo:** rust-wayland-desktop
**Depends on:** none
**Execution:** agent

Route Super, Activities, and hot-corner triggers from compositor
input to the shell, and render windows, workspaces, favorites, and
search from the live model with open/close/return semantics. This
turns the model-only overview into the visible R1 slice.

*Technical detail:* [context.md#task-overview-triggers-and-window-presentation](./context.md#task-overview-triggers-and-window-presentation)

**Acceptance criteria**:

- [ ] Super, Activities, and hot corner open the overview; repeat or Escape returns to the prior app
- [ ] Overview presents current windows, workspaces, favorites, and search from live snapshots
- [ ] Every focus change clears the token policy

#### - [ ] Task: Launcher, search providers, and favorites
**Id:** add5ac47-e57c-462b-b68e-9446d92ab5e4
**Repo:** rust-wayland-desktop
**Depends on:**
- 998d8d30-db43-42f4-b242-f6bb4a6c5048 — Overview triggers and window presentation
**Execution:** agent

Add desktop-entry discovery, favorites pinning, async bounded search
providers with cancellation, launch feedback, and switch-to-instance,
all rendering through the overview without blocking typing or focus.

*Technical detail:* [context.md#task-launcher-search-providers-and-favorites](./context.md#task-launcher-search-providers-and-favorites)

**Acceptance criteria**:

- [ ] Grid and search results launch entries, switch to instances, and show feedback
- [ ] Slow or failing providers never block typing, focus, or input; results are cancellable and bounded
- [ ] Favorites pin and unpin persistently

### Milestone 2: Window, workspace, panel, and notification workflows

**What changes**: workspaces go dynamic with move-window actions and
an Alt-Tab MRU switcher; snap, maximize, and fullscreen follow the
ledger; the panel shows clock, indicators, and quick settings with
honest loading and error states; notifications arrive as banners with
history, Do Not Disturb, and actions that never steal focus.
Validation: automated state-machine suites plus human gesture and
service runs recorded in the ledger.

#### - [ ] Task: Dynamic workspaces, switching, and window actions
**Id:** 2f9df458-9090-40a1-ac79-a46afa924e55
**Repo:** rust-wayland-desktop
**Depends on:**
- 998d8d30-db43-42f4-b242-f6bb4a6c5048 — Overview triggers and window presentation
**Execution:** agent

Promote workspaces to live dynamic sets with move-window actions, an
Alt-Tab MRU switcher, and snap/maximize/fullscreen semantics per the
ledger, keeping focus and geometry authority in the compositor.

*Technical detail:* [context.md#task-dynamic-workspaces-switching-and-window-actions](./context.md#task-dynamic-workspaces-switching-and-window-actions)

**Acceptance criteria**:

- [ ] Dynamic workspaces create and clean up; move-window actions follow the ledger
- [ ] Alt-Tab switches by recency; snap, maximize, and fullscreen behave per ledger
- [ ] Keyboard shortcuts and transient/dialog parents follow documented parity

#### - [ ] Task: Panel, quick settings, and service states
**Id:** 5922d769-efb9-41f9-bd0f-c606899c2625
**Repo:** rust-wayland-desktop
**Depends on:**
- 998d8d30-db43-42f4-b242-f6bb4a6c5048 — Overview triggers and window presentation
**Execution:** agent

Build clock, indicators, and quick-settings surfaces with explicit
loading, disconnected, and error states, attaching live services per
service behind presence detection without ever hanging the shell.

*Technical detail:* [context.md#task-panel-quick-settings-and-service-states](./context.md#task-panel-quick-settings-and-service-states)

**Acceptance criteria**:

- [ ] Clock, calendar, and supported controls render with honest unavailable and error states
- [ ] Absent services never hang the shell; live bindings attach only when present
- [ ] Touchpad gestures follow the ledger where hardware allows

#### - [ ] Task: Notification center behind the freedesktop seam
**Id:** 7138243e-06ab-4882-99cb-5d574782de46
**Repo:** rust-wayland-desktop
**Depends on:**
- 998d8d30-db43-42f4-b242-f6bb4a6c5048 — Overview triggers and window presentation
**Execution:** agent

Add the local banner queue, history, Do Not Disturb, actions,
expansion, and dismissal behind a freedesktop-shaped API the 004 bus
daemon will implement, with no focus theft and content-safe logs.

*Technical detail:* [context.md#task-notification-center-behind-the-freedesktop-seam](./context.md#task-notification-center-behind-the-freedesktop-seam)

**Acceptance criteria**:

- [ ] Banners, history, DND, actions, and dismissal behave per spec with no focus theft
- [ ] Private content never enters logs; the seam matches the 004 daemon contract
- [ ] Stale and replayed notification actions are denied with typed errors

### Milestone 3: Accessible, localized, ledger-complete parity

**What changes**: every journey works keyboard-only, large text,
reduced motion, high contrast, touch targets, RTL, and localized
strings ship; each deviation from the pinned baseline carries a
ledger entry with severity and owner, and the journey evidence is
versioned with baseline and environment. Validation: human
accessibility review plus ledger completeness check — no journey
without evidence or an accepted entry.

#### - [ ] Task: Accessibility, localization, and ledger evidence
**Id:** aa46f8a5-59f9-4c7c-a835-b3f1be1181df
**Repo:** rust-wayland-desktop
**Depends on:**
- add5ac47-e57c-462b-b68e-9446d92ab5e4 — Launcher, search providers, and favorites
- 2f9df458-9090-40a1-ac79-a46afa924e55 — Dynamic workspaces, switching, and window actions
- 5922d769-efb9-41f9-bd0f-c606899c2625 — Panel, quick settings, and service states
- 7138243e-06ab-4882-99cb-5d574782de46 — Notification center behind the freedesktop seam
**Execution:** agent

Cover keyboard-only flows, large text, reduced motion, high contrast,
touch targets, RTL, and localized strings across every journey, and
close the ledger so each deviation names baseline, environment,
severity, and owner with versioned evidence.

*Technical detail:* [context.md#task-accessibility-localization-and-ledger-evidence](./context.md#task-accessibility-localization-and-ledger-evidence)

**Acceptance criteria**:

- [ ] Every journey passes keyboard-only with recorded screen-reader, contrast, motion, and RTL review
- [ ] No parity deviation lacks a ledger entry with severity and owner
- [ ] Journey evidence is versioned with baseline version and test environment

#### - [ ] Task: Live-session parity verification
**Id:** 679824e2-644e-4f67-96e7-8cc04fb3cfd1
**Repo:** rust-wayland-desktop
**Depends on:**
- aa46f8a5-59f9-4c7c-a835-b3f1be1181df — Accessibility, localization, and ledger evidence
**Execution:** human — verification only a person can do (eyes and ears on a live host session with real apps, services, and assistive tech)

Confirm the overview, search, launch, panel, workspace, gesture,
notification, and accessibility journeys by eye and ear on a live
host session with real application clients and services, and record
the outcome in the ledger. This closes the runs automation cannot
claim.

*Technical detail:* [context.md#task-live-session-parity-verification](./context.md#task-live-session-parity-verification)

**Acceptance criteria**:

- [ ] scripted and human journeys pass against the pinned baseline or carry accepted ledger entries
- [ ] Gestures, services, and assistive-tech runs are recorded with environment details
- [ ] Results are versioned as acceptance evidence with baseline version

## Open Questions

- Whether the layer-shell configure/ack round-trip sustains a
  full overview surface with search-result churn at frame rate
  without ack backlog — only discoverable by exercising it under
  load. Depends on overview rendering volume. When the implementer
  hits it: profile ack latency under churn; if backlog breaks input,
  STOP and ask the user whether to batch result commits or cap
  visible results.
- Whether the retained change-log window covers rapid overview
  select/activate bursts without forcing gap resnapshots
  mid-journey — depends on burst patterns only live use reveals.
  When the implementer hits it: measure resnapshot frequency under
  burst scripts; if resnapshot storms appear, STOP and ask the user
  whether to widen the window or batch intents.

## Out of Scope

- Production session replacement — 002 replaces no existing
  session (spec Non-Goal); the session story stays where it is.
- Search-provider extension API — window/desktop providers are
  built-in seams only; third-party extension providers are not a
  002 deliverable (spec Non-Goal).
- Full desktop parity — shell workflow parity stops at the listed
  journeys; lock screen, settings app, file manager, and terminal
  remain explicitly out (spec Non-Goal).
- Trusted Wayland extension protocols — no privileged protocol
  work in 002 (spec Non-Goal).
- Notification/service bus daemons — the local store and
  unavailable-state seams are 002; the daemons belong to the 004
  secure-session plan.
- Gesture and assistive-tech automation — human-reviewed by design
  (A1/A5), not an automation gap to close later in this plan.
