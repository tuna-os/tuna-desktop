---
created_date: "2026-09-29"
document_status: final
closed_date: "2026-09-29"
---

# Plan: 20260929180510-b20ab21e-notification-presence

<!-- Metadata -->
<!-- Created: 2026-09-29T19:22:15Z -->
<!-- Commit: 4a71f28 -->
<!-- Branch: main -->
<!-- Repository: rust-wayland-desktop (/home/ubuntu/dev/rust-wayland-desktop) -->

## Overview

Roost serves the standard desktop notifications interface from
the shell host so unmodified GNOME apps can notify the user.
Banners with working actions appear on the existing banner
surface, a bar marker shows unread presence, and the unread
queue survives shell restarts.

## Conventions

No project conventions apply to this feature — the
always-applied knowledge load for repo rust-wayland-desktop
returned no entries, and topic searches on notifications and
banner found nothing to cite.

## Architecture & Design Decisions

Serve `org.freedesktop.Notifications` inside the shell host
with the tray host's blocking-zbus pattern and back it with the
existing `NotificationCenter` model. A new `intake` module owns
the bus object: `Notify` appends to the center and persists the
unread queue, `CloseNotification` dismisses, `GetCapabilities`
advertises actions/persistence, and banner presses route back as
`ActionInvoked`/`NotificationClosed` signals. Draining happens on
the existing 2s slow tick next to `sync_icon_theme`, so intake
never blocks the event loop and never spawns processes. Bar
presence paints from the center's unread count through the tile
paint keys; banners paint through the existing banner layer
surface and canvas. Persistence is an XDG state file written with
atomic rename on every queue mutation and loaded at host start.

This beats a sidecar daemon (extra process, toolkit dependency,
against the pixel-painted direction) and an async intake task (a
second concurrency model for one endpoint when every zbus call in
the tree is already blocking). It reuses rather than rebuilds:
the center's caps (`MAX_BANNERS=3`, `MAX_HISTORY=50`), the banner
surface reconcile, and the private-bus stub test pattern all
already exist, so the plan wires proven pieces instead of
inventing new ones. See `research.md#alternatives-considered-and-rejected`
for the rejected options with evidence.

## Component Breakdown

- **Intake service (new)** — owns the
  `org.freedesktop.Notifications` bus object: Notify,
  CloseNotification, GetCapabilities, and the ActionInvoked /
  NotificationClosed signals. Forwards accepted items into the
  notification center and persists the unread queue. No policy
  of its own; it is the spec's wire contract and nothing more.
- **Notification center (changed)** — keeps owning the banner
  queue, history, DND gate, and action invocation. Gains a load
  path that rehydrates the unread queue from disk at host start
  and a save hook called on every mutation; bounds stay where
  they are.
- **Bar presence tile (changed)** — derives the unread marker
  from the center's unread count and repaints through the
  existing tile paint keys when the count changes.
- **Banner surface wiring (changed)** — the existing reconcile
  path binds the center's visible banners to the layer surface
  and routes presses to default-action invoke or dismiss, which
  the intake service reports back over the bus.

## Data Structures & Interfaces

The wire contract is the freedesktop interface the intake
serves — no new IDL is invented:

```
Notify(app, id, icon, summary, body, actions[], hints, timeout) -> u32
CloseNotification(id)
GetCapabilities() -> ["actions", "persistence"]
signals: ActionInvoked(id, action_key), NotificationClosed(id, reason)
```

New and changed store shapes in the host:

```
PersistedQueue { version: 1, unread: [PersistedNotification] }
PersistedNotification { app, summary, body, icon_name?, arrived_ms }
```

`NotificationCenter` keeps its existing `Notification` shape and
gains `load(queue)` / `save() -> PersistedQueue`; the intake
service translates wire calls to `notify` / `dismiss` /
`invoke_action` and emits the signals back. The bar tile reads a
single `unread_count()` derived from the center.

## Implementation Detail

The plan introduces one new module boundary — the intake
service — and otherwise follows the tray host's established
shapes. Intake mirrors the watcher's serve-and-poll rhythm: own
the bus name defensively, translate wire calls into store
mutations, emit signals back. Persistence follows the simplest
durable shape available: serialize the unread queue on mutation,
atomic-rename into place, load once at host start. A developer
reading the result sees the same three-beat pattern the tray
code already uses — bus object, host-owned store, tick-driven
reconcile — with notification specifics (caps, DND, action
routing) living in the center rather than the intake. No new
concurrency, no new runtime, no per-notification processes.

## Dependencies

- zbus (blocking) — bus serve/proxy/emit for intake and
  signals; already depended on, no changes.
- serde_json (workspace) — unread-queue persistence format;
  already depended on by shell-host, no changes.
- XDG state directory — queue file location; runtime
  environment, no code change.
- freedesktop Desktop Notifications Specification — intake
  wire contract; external reference, no changes.
- settings-compat-shell-surfaces plan (landed) — banner layer
  surface, tile paint keys, slow-tick sync points this plan
  wires into; no changes needed.
- icon-theme-lookup plan (landed) — host cache and stub-test
  patterns reused for notification artwork and behavioural
  tests; no changes needed.

Design documents this plan was built on: none — the spec
carries no design references.

## Testing Approach

Unit tests on the center and the persistence round-trip
carry the model guarantees: caps hold under flood, save/load
preserves the unread queue byte-for-byte, DND gates banners.
Behavioural tests extend the existing private-bus stub pattern:
a stub client sends Notify over a private address, asserting the
banner appears, the default action fires exactly once, and close
reasons route back — this covers all three of the spec's success
metrics as automated tests (banner+action, 10k flood with flat
allocation and stable paint keys, restart rehydrate). Heaviest
coverage sits on intake translation and queue bounds, where the
spec's flood and durability guarantees live. Deliberate gap: no
live-compositor frame-timing or pixel-perfect banner
conformance — CI has no compositor; visual spot-checks happen
during packaging.

## Milestones & Tasks

### M1 — Apps can notify the shell

GNOME apps speak the standard notifications interface and the
shell accepts, queues, and shows their banners with working
actions. Validation: stub-client behavioural test shows a banner
and fires its action exactly once.

### M2 — Unread state is visible and durable

The top bar shows a presence marker whenever unread
notifications exist, and the unread queue survives a shell
restart. Validation: marker present/absent tests plus a
save-kill-reload round-trip test.

### M1 — Apps can notify the shell

#### - [x] Task: Intake service on the notifications bus
**Id:** 95e7465e-5858-4714-9caa-42b3f42a928a
**Repo:** rust-wayland-desktop
**Depends on:** none
**Execution:** agent

Serve the standard desktop notifications name from the shell
host and translate wire calls into notification-center
mutations, emitting action and close signals back to callers.

*Technical detail:* [context.md#task-intake-service](./context.md#task-intake-service)

**Acceptance criteria**:
- [x] Stub client Notify shows a banner in the shell
- [x] Default action press fires the stub callback exactly once
- [x] Close press tells the stub its notification closed

#### - [x] Task: Banner paint and press routing
**Id:** 049bca2a-9811-41a0-8df5-d18c88ecfdd7
**Repo:** rust-wayland-desktop
**Depends on:**
- 95e7465e-5858-4714-9caa-42b3f42a928a — Intake service on the notifications bus
**Execution:** agent

Bind the center's visible banners to the banner layer surface
and route banner presses to action invoke or dismiss without
stealing keyboard focus.

*Technical detail:* [context.md#task-banner-routing](./context.md#task-banner-routing)

**Acceptance criteria**:
- [x] Banner paints summary and body on the layer surface
- [x] Pressing the action row invokes, pressing dismiss closes
- [x] Banner never takes keyboard focus

### M2 — Unread state is visible and durable

#### - [x] Task: Bar presence marker
**Id:** 15ec2abd-1f75-4e86-b31a-981198c64143
**Repo:** rust-wayland-desktop
**Depends on:**
- 95e7465e-5858-4714-9caa-42b3f42a928a — Intake service on the notifications bus
**Execution:** agent

Paint an unread marker in the top bar derived from the center's
unread count, repainting through the tile paint keys when the
count changes.

*Technical detail:* [context.md#task-presence-marker](./context.md#task-presence-marker)

**Acceptance criteria**:
- [x] Marker visible with unread queued, hidden when none
- [x] Marker repaints within one tick of queue changes

#### - [x] Task: Queue persistence and flood bounds
**Id:** 92db57ff-c018-4584-92d8-b8cdcff1d343
**Repo:** rust-wayland-desktop
**Depends on:**
- 95e7465e-5858-4714-9caa-42b3f42a928a — Intake service on the notifications bus
**Execution:** agent

Persist the unread queue across shell restarts and prove the
flood bound: ten thousand notifications keep memory flat and
the bar responsive with newest items kept.

*Technical detail:* [context.md#task-persistence-bounds](./context.md#task-persistence-bounds)

**Acceptance criteria**:
- [x] Restart preserves the unread list shown before it
- [x] Ten thousand notifications keep memory flat, newest kept

## Open Questions

- Whether a live Roost session contends with another
  holder of the notifications bus name — only discoverable by
  running the shell in a real session. If the name is taken,
  the implementer keeps the silent-yield behavior and reports
  back rather than fighting for the name.
- Whether the pinned zbus version's blocking signal emission
  covers the ActionInvoked/NotificationClosed shapes cleanly —
  only discoverable by compiling the intake object. If it does
  not, the implementer reports back rather than adding an
  async dependency.

## Out of Scope

Notification history browser and do-not-disturb schedules,
inline replies and buttons beyond default/close actions, and
sound or haptic alerts on arrival are out of scope; the spec's
Non-Goals section governs.

## Changelog

### 2026-09-29 — Task: Intake service on the notifications bus

**What was done**: New intake module serves the standard
notifications bus name with blocking zbus, translating Notify /
Close / Capabilities into notification-center mutations and
emitting action/close signals back; host drains it on the slow
tick. Stub-client behavioural tests cover banner display,
single-fire default action, and close routing.

**Deviations**: none.

**Files changed**:
- `crates/shell-host/src/intake.rs`
- `crates/shell-host/src/lib.rs`
- `crates/shell-host/src/panel.rs`

**Discoveries**: none.

### 2026-09-29 — Task: Banner paint and press routing

**What was done**: Banner canvas paints real summary/body text
with urgency and expanded states; the reconcile path binds
visible banners to the layer surface; presses map to
default-action invoke or dismiss and never touch keyboard
focus. Tests cover paint pixels, hit mapping, press outcomes,
and focus stability.

**Deviations**: none.

**Files changed**:
- `crates/shell-host/src/overview.rs`
- `crates/shell-host/src/panel.rs`
- `crates/shell-host/src/notifications.rs`
- `crates/shell-host/src/lib.rs`

**Discoveries**: none.

### 2026-09-29 — Task: Bar presence marker

**What was done**: Bar tile reads the center's unread count
into the panel paint key so the strip repaints within one tick
of queue changes; the marker square paints only while unread
is nonzero. Tests cover key tracking, visible/hidden states,
and clean-at-zero pixels.

**Deviations**: none.

**Files changed**:
- `crates/shell-host/src/panel.rs`
- `crates/shell-host/src/overview.rs`
- `crates/shell-host/src/notifications.rs`

**Discoveries**: none.

### 2026-09-29 — Task: Queue persistence and flood bounds

**What was done**: Center gains load/save over a versioned
JSON queue file under the XDG state dir with atomic rename on
mutation, loaded at host start. Tests cover persist-back,
restart rehydrate of the unread list, and a ten-thousand
flood staying bounded with newest kept.

**Deviations**: none.

**Files changed**:
- `crates/shell-host/src/notifications.rs`
- `crates/shell-host/src/panel.rs`

**Discoveries**: none.
