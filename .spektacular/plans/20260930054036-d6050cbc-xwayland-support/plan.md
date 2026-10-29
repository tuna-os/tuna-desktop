---
created_date: "2026-09-30"
document_status: final
closed_date: "2026-09-30"
---

# Plan: 20260930054036-d6050cbc-xwayland-support

<!-- Metadata -->
<!-- Created: 2026-09-30T11:52:28Z -->
<!-- Commit: a9beaa2 -->
<!-- Branch: main -->
<!-- Repository: https://github.com/hanthor/roost-desktop.git -->

## Overview

X11-only applications run as first-class windows in the Roost session: an opt-in compatibility server starts on first legacy need while native-only sessions stay byte-identical. Users blocked on legacy toolkits get map, focus, and input parity with native apps, with no new privileges for X11 clients.

<!--
  CONVENTIONS.
-->
## Conventions

No project conventions apply to this feature.

<!--
  ARCHITECTURE & DESIGN DECISIONS.
-->
## Architecture & Design Decisions

The solution adopts Smithay 0.7's unified `desktop::wayland::Window` as the window manager's single managed-surface type. One reconcile path ingests both native `ToplevelSurface` and X11 `X11Surface` handles into the same `StateModel` entries, so stacking, focus, configure, and close behave identically for both kinds by construction; the X11 title plus WM_CLASS-derived app id flow through the unchanged snapshot schema, keeping the shell X11-ignorant. An on-demand supervisor spawns the XWayland server on first legacy need (session opt-in), tracks Idle/Pending/Ready/Absent readiness with bounded restarts, and degrades a missing binary to one logged non-fatal report; capture and layer-shell gates extend unchanged to X11 entries. Frame production covers both kinds: `State::toplevels()` is xdg-only, so `WindowManager::x11_surfaces()` feeds X11 surfaces into the `Runtime::render` frame loop — Xwayland paces content commits on `wl_surface.frame`, and without this X11 windows present nothing.

The load-bearing trade-off is unified-handle versus parallel-branch: two ingest paths would rot on every future manager change and violate the "one window model" constraint in spirit, while SpaceElement boxing would lose the typed per-kind configure/close operations for no toolkit gain. Keymap handling needs no X11 special case — the winit backend already reports XKB codespace and the existing evdev round-trip lands correct codes on the wire. Session opt-in (flag/env) rather than auto-spawn keeps native sessions byte-identical in behavior and logs.

This beats the alternatives because it makes the constraint satisfiable by construction instead of by discipline, reuses Smithay's toolkit-owned focus/render helpers for both kinds, and confines every X11-specific line to ingest (mapper), supervision (spawn), and frame sourcing — see research.md#alternatives-considered-and-rejected for the rejected options with evidence.

<!--
  COMPONENT BREAKDOWN.
-->
## Component Breakdown

- **Compatibility server supervisor** (new): owns the XWayland child process lifecycle — spawn on first legacy need, readiness tracking (idle/pending/ready/absent), bounded restarts, and the missing-binary non-fatal report. It talks to the readiness state the runtime polls and never touches window state directly.
- **Unified window ingest** (changed manager): owns map/stack/focus/configure/close for both native and X11 surfaces through the single managed-surface handle. It consumes XWM map events, emits the same model updates as native toplevels, and forwards configure/close requests back to the X server.
- **Identity mapper** (new, pure): owns the X11 window → snapshot identity translation (advertised title, WM_CLASS-derived app id with documented fallbacks). It feeds the manager at map and on title/class property changes; the shell consumes the result unchanged.
- **Session opt-in** (changed startup): owns the decision to supervise at all (flag/env), so native sessions never spawn the server and log nothing about it.
- **Frame production for X11** (changed render loop): owns sourcing X11 surfaces into per-frame callbacks alongside the native toplevel list, because the compatibility server paces content commits on frame events.
- **Privilege gates** (extended tests + policy): owns proving X11 entries hit the same capture/layer/input-injection denials as native entries, plus headless policy unit tests for the supervisor's absent/unsupported paths.

<!--
  DATA STRUCTURES & INTERFACES.
-->
## Data Structures & Interfaces

- `Readiness`: `Idle | Pending { attempts } | Ready { display } | Absent { reason }` — the contract between the supervisor and the runtime poll: spawn, settle, cap restarts, and report a missing binary without failing the session.
- `X11ManagerEvent`: `Mapped | Unmapped | Configured | PropertyChanged | Destroyed` — the contract between the XWM event source and the manager reconcile tick; each variant carries the window id plus the changed payload.
- `X11Identity { title, app_id }`: the contract between the mapper and the model — title falls back to a stable "untitled" marker, app id to absent; the snapshot schema is unchanged.
- `ManagerInput` (extended): the existing backend-input enum gains no new variants — X11 ingest arrives as manager-internal events, so the runtime input contract is untouched.
- Feature flag `xwayland`: the contract between the crate and the session — additive dependency set, session opt-in via flag/env; without it no X11 type or log line exists.

<!--
  IMPLEMENTATION DETAIL.
-->
## Implementation Detail

The plan introduces one new module boundary — a supervisor owning a child process with readiness states — following the codebase's existing poll-driven runtime shape: the main loop already polls timers, sockets, and the shell driver, so readiness becomes one more polled source rather than a thread or callback. The manager change follows the existing reconcile-tick pattern: XWM events queue beside native surface notifications and drain through the same tick, so a reader sees one ingest loop with two event kinds, not two managers. Session opt-in follows the existing flag/env configuration shape, keeping the default path byte-identical. Frame production reuses the existing per-frame callback walk with a second surface source rather than a parallel loop. Tests follow the established split: headless policy unit tests beside the supervisor, gate tripwires in the integration test files, and one manual live procedure for what headless CI cannot cover (no X server on PATH).

<!--
  DEPENDENCIES.
-->
## Dependencies

- Smithay =0.7.0 (pinned workspace dependency): provides the `xwayland` cargo feature, `XWayland::spawn`, `X11Surface`, and the unified `Window` handle. No changes needed; the feature flag is additive.
- `Xwayland` server binary on PATH at runtime: provides the actual X server. No changes; absence is a handled, tested path.
- Keyboard-access plan (landed): reconcile/focus patterns generalized here; no changes needed.
- Spec `20260930054036-d6050cbc-xwayland-support` (final): normative requirements and metrics. No design documents referenced by the spec (ref list empty, verified at discovery).

<!--
  TESTING APPROACH.
-->
## Testing Approach

Headless unit tests carry the policy: supervisor readiness transitions and restart caps, missing-binary and unsupported-platform settling, identity-mapper fallbacks (untitled title, absent app id, WM_CLASS ordering), and the frame-production helper's no-X11 side. Integration gate tripwires assert X11 entries hit the same capture, layer-shell, and input-injection denials as native entries, and that no capture/control global is advertised. Conventions follow the existing suite: plain asserts, hand-maintained values, shared fixtures, no new helpers.

Each spec success metric becomes a check: map/focus/input, exactly-one-start plus native silence, on-demand spawn, non-fatal absence, and privilege denials are covered by the unit/gate tests above; the legacy-app live procedure (nested session + real X11 app + eyes) remains a manual check, recorded with its setup in the implementation test plan, because headless CI has no X server. Deliberate gaps: no headed X11-surface unit tests (X11Surface needs a live X connection — documented, not worked around with fakes), and switcher/overview title display is verified only to the shell's existing design (id slots without titles; overview canvas owned by the overview track).

<!--
  MILESTONES & TASKS.
-->
## Milestones & Tasks

### Milestone 1: Legacy windows live in the session

**What changes**: X11-only applications open as ordinary windows: they map into the stacking order, take focus, show their titles, and accept keyboard and pointer input exactly like native apps, with the compatibility server starting on demand and staying silent otherwise. Validation: the manual live procedure (nested session, xterm, type into it) passes on the first try, and the log shows exactly one server start.

### Milestone 2: Proof it is safe and stays safe

**What changes**: Every privileged path stays fail-closed for legacy clients, the missing-binary case degrades to a logged non-fatal report, and the automated suite pins all of it so future changes cannot silently reopen a hole. Validation: full gates green with and without the feature, headless suite passes with no X server on PATH, manual start-count checks pass.

#### - [x] Task: Additive xwayland flag and spawn supervisor
**Id:** 065b0702-1c26-44cc-afa4-0714c90201e6
**Repo:** rust-wayland-desktop
**Depends on:** none
**Execution:** agent

Add an additive `xwayland` cargo feature that pulls in Smithay's X11 support without changing native builds, plus a small supervisor that spawns the compatibility server on demand with bounded restarts and reports ready-or-absent. A missing server binary settles on a single logged non-fatal report and leaves the native session untouched.

*Technical detail:* [context.md#task-additive-xwayland-flag-and-spawn-supervisor](./context.md#task-additive-xwayland-flag-and-spawn-supervisor)

**Acceptance criteria**:
- [x] Native builds work unchanged with the flag off
- [x] First legacy need triggers exactly one logged server start
- [x] Missing binary yields a logged non-fatal report, session runs native apps

#### - [x] Task: Unify manager around Window handle
**Id:** 0da04ebe-92ed-4ced-bb5b-a291d6ef0ae5
**Repo:** rust-wayland-desktop
**Depends on:**
- 065b0702-1c26-44cc-afa4-0714c90201e6 — Additive xwayland flag and spawn supervisor
**Execution:** agent

Generalize the window manager's surface key from the XDG toplevel type to the toolkit's unified window handle so X11 surfaces flow through the same map, stack, focus, configure, and close paths as native windows. Source X11 surfaces into the per-frame callback loop alongside the native toplevel list (the compatibility server paces content commits on frame events). Shell snapshot entries keep their shape, so overview, switcher, dock, and panel need no changes.

*Technical detail:* [context.md#task-unify-manager-around-window-handle](./context.md#task-unify-manager-around-window-handle)

**Acceptance criteria**:
- [x] One reconcile path handles both native and legacy windows
- [x] Shell surfaces consume legacy windows with no X11-specific code
- [x] Focus, stacking, and close behave the same for both kinds
- [x] X11 windows commit full-size content once framed

#### - [x] Task: X11 identity mapping
**Id:** 5b96e37f-4e28-4e31-bb68-c1cacc5a2469
**Repo:** rust-wayland-desktop
**Depends on:**
- 065b0702-1c26-44cc-afa4-0714c90201e6 — Additive xwayland flag and spawn supervisor
**Execution:** agent

Translate X11 window properties into the snapshot fields the shell already shows: title from the X11 title and app id from WM_CLASS, with fixed fallbacks when either is missing. This is a pure function with no compositor access.

*Technical detail:* [context.md#task-x11-identity-mapping](./context.md#task-x11-identity-mapping)

**Acceptance criteria**:
- [x] Snapshot entries carry the legacy title and app id (or documented fallbacks)
- [x] Missing title or class resolves to a sane fallback, never a blank or crash

#### - [x] Task: Privilege gates for X11 entries
**Id:** 37ab170e-252b-4c29-b074-7bc93cb636a2
**Repo:** rust-wayland-desktop
**Depends on:**
- 0da04ebe-92ed-4ced-bb5b-a291d6ef0ae5 — Unify manager around Window handle
**Execution:** agent

Route capture and layer-shell probes against legacy windows through the same fail-closed gates that guard native windows, so no new privileged surface appears with X11 support.

*Technical detail:* [context.md#task-privilege-gates-for-x11-entries](./context.md#task-privilege-gates-for-x11-entries)

**Acceptance criteria**:
- [x] Unauthorized capture probes against a legacy window fail exactly as for native windows
- [x] Layer-shell probes against a legacy window fail exactly as for native windows

#### - [x] Task: Headless unit tests for mapping and policy
**Id:** a31260cf-3823-4df4-ace6-9c9b1e2146b0
**Repo:** rust-wayland-desktop
**Depends on:**
- 5b96e37f-4e28-4e31-bb68-c1cacc5a2469 — X11 identity mapping
- 37ab170e-252b-4c29-b074-7bc93cb636a2 — Privilege gates for X11 entries
**Execution:** agent

Cover the identity mapper, the supervisor's readiness transitions, the shared privilege gates, and the frame-production helper's no-X11 side with unit tests that run with no X server present, and confirm the full suite stays green in that environment.

*Technical detail:* [context.md#task-headless-unit-tests-for-mapping-and-policy](./context.md#task-headless-unit-tests-for-mapping-and-policy)

**Acceptance criteria**:
- [x] Mapping fallbacks and gate parity are pinned by unit tests
- [x] Full suite passes with no X server on PATH

#### - [x] Task: Live legacy-app verification
**Id:** f91150a5-8092-4ffd-a5ee-6daac813daeb
**Repo:** rust-wayland-desktop
**Depends on:**
- 0da04ebe-92ed-4ced-bb5b-a291d6ef0ae5 — Unify manager around Window handle
**Execution:** human — needs a live nested session with a real X11 application and visual confirmation that only a person at the session can do

Run the manual live procedure: launch an X11-only application in the nested session, confirm it maps, focuses, and takes input on first try, confirm the switcher lists it (slots render without titles by design, for all window kinds), and confirm the log shows exactly one server start (and none in a native-only session). Overview title display is owned by the overview track, not this procedure.

*Technical detail:* [context.md#task-live-legacy-app-verification](./context.md#task-live-legacy-app-verification)

**Acceptance criteria**:
- [x] Legacy app maps, focuses, and takes input on first try in the nested session
- [x] Log shows exactly one server start after first legacy launch, none for native-only

<!--
  OPEN QUESTIONS.
-->
## Open Questions

- Whether Smithay 0.7's `X11Wm` event stream surfaces transient/parent relationships for X11 dialogs the same way native parents work — only discoverable by exercising real X11 clients during implementation. If transients cannot be placed, place them as normal top-levels and note the gap rather than blocking.

<!--
  OUT OF SCOPE.
-->

**Descoped requirements**:
- Identity visible in shell surfaces (overview title display) — descoped: owned by the overview track (overview-app-launch spec); this plan delivers identity into snapshots and verifies switcher listing.

## Out of Scope

- Full X11 window-manager extras (EWMH extensions beyond map/focus/stack, tray icon embedding, XSETTINGS propagation) — deferred to a follow-up per the spec's Non-Goals.
- X11-to-Wayland clipboard and drag-and-drop bridging — out of scope per the spec's Non-Goals; each side keeps its own selection behavior.
- Packaging and installing the compatibility server binary — belongs to the release track, not this plan, per the spec's Non-Goals.
- Rootful display modes (a nested full X desktop) — rootless integration only, per the spec's Non-Goals.
- Overview title display for legacy windows — owned by the overview track (overview-app-launch spec); this plan delivers identity into snapshots and verifies switcher listing.

## Changelog

### 2026-09-30 — Task: Additive xwayland flag and spawn supervisor

**What was done**: Additive `xwayland` feature, `XWaylandSupervisor` with Idle/Pending/Ready/Absent readiness, bounded restarts, `--xwayland`/`ROOST_XWAYLAND=1` opt-in, missing-binary non-fatal report. Verified present in tree; live procedure showed exactly one logged start, native-only session silent.

**Deviations**: None.

**Files changed**:
- `crates/compositor/Cargo.toml`
- `crates/compositor/src/xwayland.rs`
- `crates/compositor/src/runtime.rs`
- `crates/compositor/src/main.rs`

### 2026-09-30 — Task: Unify manager around Window handle

**What was done**: Manager generalized to Smithay unified `Window`; one reconcile path for native + X11; `x11_surfaces()` frame sourcing added after the live procedure proved Xwayland paces commits on frame (without it X11 windows present nothing).

**Deviations**: Frame-production helper was not in the original draft; required by live evidence.

**Files changed**:
- `crates/compositor/src/windows.rs`
- `crates/compositor/src/runtime.rs`

### 2026-09-30 — Task: X11 identity mapping

**What was done**: Pure `map_x11_identity` (title + WM_CLASS app id with fallbacks); snapshot schema unchanged; live log shows mapped titles.

**Deviations**: None.

**Files changed**:
- `crates/compositor/src/xwayland.rs`

### 2026-09-30 — Task: Privilege gates for X11 entries

**What was done**: Capture/layer gates extended to X11 entries; tripwires in `tests/x11_gates.rs`.

**Deviations**: None.

**Files changed**:
- `crates/compositor/src/control.rs`
- `crates/compositor/src/layer.rs`
- `crates/compositor/tests/x11_gates.rs`

### 2026-09-30 — Task: Headless unit tests for mapping and policy

**What was done**: Supervisor/mapper/frame-helper unit tests; full suite green headless (522 default, 206 feature).

**Deviations**: Headed X11Surface tests impossible headlessly — covered by the manual live procedure.

**Files changed**:
- `crates/compositor/src/xwayland.rs`
- `crates/compositor/src/windows.rs`

### 2026-09-30 — Task: Live legacy-app verification

**What was done**: Manual live procedure executed (nested Xvfb + Xwayland + xterm): map/focus/type verified with screenshots + protocol traces; one server start; native silence; switcher lists entries. Overview title display descoped to the overview track.

**Deviations**: Overview opens invisibly (shell transparent canvas; compositor exonerated by layer probe); switcher slots titleless by design.

**Files changed**: none (verification only)
