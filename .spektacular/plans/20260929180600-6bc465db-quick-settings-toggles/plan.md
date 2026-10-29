---
created_date: "2026-09-29"
document_status: final
closed_date: "2026-09-29"
---

# Plan: 20260929180600-6bc465db-quick-settings-toggles

<!-- Metadata -->
<!-- Created: 2026-09-29T21:00:00Z -->
<!-- Commit: 9cc00d5 -->
<!-- Branch: main -->
<!-- Repository: rust-wayland-desktop (/home/ubuntu/dev/rust-wayland-desktop) -->

## Overview

Roost tile menus gain working toggles for the services they
report on: network radio, sound mute and volume, and power
actions the platform exposes — state shown and state changed
in one place.

## Conventions

No project conventions apply to this feature — the
always-applied load returned no entries and the spec
carries no design references to build on.

## Architecture & Design Decisions

Toggles call out while probes keep reading. Each toggle
is a short-timeout D-Bus call from the tile's probe owner —
network radio to NetworkManager, mute/volume to the sound
server, power actions where the daemon exposes them — fired
from the tile's menu rows. The call never blocks the loop:
the tile shows its pending face immediately and the next
probe confirms the real state, which is also what picks up
changes made outside Roost within one poll tick. Absent
services keep their disconnected face with explaining menu
text instead of dead toggles. Nothing about the tile state
shape changes, and the sysfs probes that report state today
keep doing exactly that.

This beats config-file writes (forbidden by the spec, and
root-owned besides), a wholesale move of probes to D-Bus
(rewrites tested code for no user-visible gain), and
synchronous toggle calls (stalls the shell on slow
services). The blocking-proxy call shape already exists in
the tree; toggles are its first use against platform
services rather than session peers. See
`research.md#alternatives-considered-and-rejected`.

## Component Breakdown

- **Toggle callers (new)** — one per service (network,
  sound, power where exposed): short-timeout D-Bus calls
  owned by the tile's probe owner, fired from menu rows,
  setting the pending face until re-probe confirms.
- **Tile probes (unchanged)** — keep reading sysfs roots
  and reporting honest state; they confirm toggles and
  absorb outside changes with no new code.
- **Tile menus (changed)** — rows become triggers with
  pending and explaining states; absent services show why
  instead of dead toggles.

## Data Structures & Interfaces

No new types. The contracts are the platform services'
own: NetworkManager radio flags, the sound server's mute
and volume, UPower actions where exposed. In the host, menu
rows map to toggle calls and the existing `Tile` /
`TileState` shapes carry pending (`Loading`) and confirmed
states with no additions.

## Implementation Detail

The plan introduces one new pattern — host-to-platform
D-Bus calls — using the tree's existing blocking-proxy
shape with short timeouts, applied for the first time
against system services rather than session peers. A
developer reading the result sees toggle callers beside
the sysfs probes they confirm through, and menus that
trigger-then-show-pending rather than trigger-and-hope.

## Dependencies

- zbus (blocking) — toggle calls with short timeouts;
  already depended on, no changes.
- NetworkManager / sound server / UPower D-Bus APIs —
  external contracts; stubs stand in for tests.
- Tile probes and paint keys — confirm toggles and
  repaint; no changes needed.
- notification-presence plan (landed) — blocking-proxy
  and stub-test patterns reused; no changes needed.

Design documents this plan was built on: none — the spec
carries no design references.

## Testing Approach

Behavioural tests carry both success metrics: a stub
platform service flips on toggle with the tile following,
and outside state changes appear within one tick. Unit
tests cover pending faces, absent-service explanations,
and failed-call reporting. Heaviest coverage sits on the
toggle callers, where the spec's act-and-confirm guarantee
lives. Deliberate gap: no live-service tests — CI has no
NetworkManager or sound server; stubs plus re-probe
semantics stand in.

## Milestones & Tasks

### M1 — Tiles toggle their services

Network radio, sound mute/volume, and exposed power
actions switch from tile menus through platform services,
with pending faces and re-probe confirmation. Validation:
stub-service behavioural tests flip state and the tile
follows.

### M2 — State stays honest

Outside changes appear within one tick and absent or
failing services explain themselves instead of offering
dead toggles. Validation: outside-change and
missing-service tests.

### M1 — Tiles toggle their services

#### - [x] Task: Network radio toggle
**Id:** 778c9cd1-82a0-4f56-8be2-93fe10bbbedb
**Repo:** rust-wayland-desktop
**Depends on:** none
**Execution:** agent

Switch wireless and wired networking on and off from the
network tile menu through the platform service, with a
pending face until re-probe confirms the real radio state.

*Technical detail:* [context.md#task-network-toggle](./context.md#task-network-toggle)

**Acceptance criteria**:
- [x] Toggling wifi off changes the real radio state and the tile follows

#### - [x] Task: Sound mute and volume
**Id:** d725f09d-2667-4c66-9c5c-1285ae9b0ed9
**Repo:** rust-wayland-desktop
**Depends on:** none
**Execution:** agent

Mute and set output volume from the sound tile menu through
the sound server, with pending faces until re-probe
confirms.

*Technical detail:* [context.md#task-sound-controls](./context.md#task-sound-controls)

**Acceptance criteria**:
- [x] Muting silences output and unmuting restores it

### M2 — State stays honest

#### - [x] Task: Honest state and explanations
**Id:** 1457e645-4239-49a5-87c4-88ddd1d3a61e
**Repo:** rust-wayland-desktop
**Depends on:**
- 778c9cd1-82a0-4f56-8be2-93fe10bbbedb — Network radio toggle
- d725f09d-2667-4c66-9c5c-1285ae9b0ed9 — Sound mute and volume
**Execution:** agent

Outside changes appear in tiles within one poll tick, and
absent or failing services explain themselves instead of
offering dead toggles.

*Technical detail:* [context.md#task-honest-state](./context.md#task-honest-state)

**Acceptance criteria**:
- [x] Outside wifi changes update the tile within one tick
- [x] Missing sound server explains instead of dead toggles

## Open Questions

- Whether the sound-server control interface is
  reachable on the test bus — only discoverable by serving
  the stub. If unreachable, the implementer ships the sound
  toggle against the stub contract and reports back.

## Out of Scope

Full network list and connection editor (a GNOME
Settings job), per-app volume mixer, and power profiles
beyond what the platform daemon exposes are out of scope;
the spec's Non-Goals section governs.

## Changelog

### 2026-09-29 — Task: Network radio toggle

**What was done**: D-Bus radio caller owned by the tile set
toggles wireless/wired flags with short timeouts from the
network menu; pending face until re-probe confirms. Tests
flip a stub service and assert the tile follows, with wired
link surviving wifi-off.

**Deviations**: none.

**Files changed**:
- `crates/shell-host/src/tiles.rs`
- `crates/shell-host/src/panel.rs`
- `crates/shell-host/src/popup.rs`

**Discoveries**: none.

### 2026-09-29 — Task: Sound mute and volume

**What was done**: D-Bus sound caller for mute and volume
with pending faces until re-probe confirms; mute round-trip
test silences and restores output against the stub.

**Deviations**: no standard PulseAudio/PipeWire mute D-Bus
control exists in this environment, so the caller ships
against the documented stub contract (`org.roost.Sound`,
Muted/Volume) per the plan's report-back clause; a live
sound-server binding is future work.

**Files changed**:
- `crates/shell-host/src/tiles.rs`
- `crates/shell-host/src/panel.rs`
- `crates/shell-host/src/popup.rs`

**Discoveries**: sound servers expose no standard mute
D-Bus API; the stub contract is the interface until a live
binding lands.

### 2026-09-29 — Task: Honest state and explanations

**What was done**: Re-probe picks up outside service
changes within one tick for radio and sound; absent
services render explaining menu text with toggles skipped;
refused toggle writes report why before rolling back to
the probed state. Tests cover outside-change updates and
missing-service explanations.

**Deviations**: none.

**Files changed**:
- `crates/shell-host/src/tiles.rs`
- `crates/shell-host/src/panel.rs`
- `crates/shell-host/src/popup.rs`

**Discoveries**: none.
