---
created_date: "2026-09-29"
document_status: final
closed_date: "2026-09-29"
---

# Plan: 20260929180627-9e6f4a0d-session-lock

<!-- Metadata -->
<!-- Created: 2026-09-29T21:40:00Z -->
<!-- Commit: 0af9f78 -->
<!-- Branch: main -->
<!-- Repository: rust-wayland-desktop (/home/ubuntu/dev/rust-wayland-desktop) -->

## Overview

Roost locks idle sessions behind an unlock screen owned
by the compositor: timeout or manual trigger raises an
exclusive surface, and only the login authentication path
lowers it again.

## Conventions

No project conventions apply to this feature — the
always-applied load returned no entries and the spec
carries no design references.

## Architecture & Design Decisions

The compositor owns the lock the way it owns the
emergency overlay: a locked flag on the runtime that
raises an exclusive lock surface above all windows,
blanking content, titles, and notifications by
construction. Idle time accrues from input timestamps at
the compositor's input entry; crossing the timeout — or a
manual lock command arriving over the control channel —
sets the flag. Unlock collects credentials on the lock
surface and verifies them through the greeter crate's
greetd path, the same authentication login uses; wrong
credentials stay locked with no hint, and a shell restart
changes nothing because the flag never lived in the
shell. Snapshots tell the shell locked-or-not without
carrying anything the lock hides.

This beats a shell-owned flag (a crash would unlock),
an external locker (a second credential path), and a new
protocol surface (machinery for what the overlay shape
already does). The failure mode is closed by design:
every path that cannot authenticate leaves the flag set.
See `research.md#alternatives-considered-and-rejected`.

## Component Breakdown

- **Lock state (new, compositor)** — locked flag plus
  idle accumulator on the runtime; set by timeout or
  control command, cleared only by successful auth.
- **Lock surface (new, compositor)** — exclusive overlay
  reusing the Overlay shape; credential entry, no content
  beneath.
- **Unlock auth (changed, greeter)** — greetd session path
  reused for verification; no new credential handling.
- **Shell lock UI (changed, shell)** — manual lock
  trigger and locked-state display driven by snapshots.

## Data Structures & Interfaces

Lock state crosses the compositor/shell boundary as a
flag in the revisioned snapshot — never window content,
never titles. Control messages carry lock requests one
way and the flag the other:

```
LockRequested / UnlockAttempt(result) / LockedFlag(bool)
```

Credential bytes never leave the compositor side except
into the greetd socket. No new paint types; the shell
renders a locked indicator from the flag alone.

## Implementation Detail

The plan extends two existing shapes rather than
inventing: the runtime gains a flag the way it carries
other session booleans, and the overlay gains a lock
variant the way it carries emergency content. Auth stays
entirely inside the greeter crate's message flow. A
developer reading the result sees lock as one more
runtime-owned mode with an exclusive surface, verified
through the login path.

## Dependencies

- Compositor runtime + overlay — lock flag and
  surface home; extended, not replaced.
- Greeter crate greetd client — unlock verification;
  reused as-is.
- Control protocol frames — lock commands and flag;
  extended with new messages.
- greetd daemon — auth counterparty; external, no changes.

Design documents this plan was built on: none — the spec
carries no design references.

## Testing Approach

Behavioural tests carry both success metrics: an idle
timeout produces the lock surface with windows hidden,
and wrong-credential attempts assert no content leak.
Unit tests cover flag transitions, manual lock, and
shell-restart persistence (flag survives shell
reconnect). Heaviest coverage sits on the lock surface
and auth-failure paths, where the spec's guarantees
live. Deliberate gap: no live-greetd unlock test — CI
has no greetd daemon; auth success is stubbed at the
client seam.

## Milestones & Tasks

### M1 — Sessions lock and stay locked

Idle timeout and manual lock raise the compositor lock
surface hiding all content; shell restarts cannot clear
it. Validation: timeout-lock and restart-stays-locked
behavioural tests.

### M2 — Only real auth unlocks

Unlock verifies through the greetd path; wrong
credentials stay locked leaking nothing. Validation:
password-unlock and triple-wrong-password tests.

### M1 — Sessions lock and stay locked

#### - [x] Task: Compositor lock flag and surface
**Id:** 299e46f2-2372-4bbe-9f00-b37a3621f72a
**Repo:** rust-wayland-desktop
**Depends on:** none
**Execution:** agent

Add the locked flag and idle accumulator to the
compositor runtime with an exclusive lock surface hiding
all content, set by timeout or control command.

*Technical detail:* [context.md#task-lock-flag](./context.md#task-lock-flag)

**Acceptance criteria**:
- [x] Idle timeout shows the lock screen and hides all windows
- [x] Shell restart while locked keeps the lock screen up

#### - [x] Task: Manual lock from the shell
**Id:** 58be1e9e-ef9e-4ed7-99aa-749788413884
**Repo:** rust-wayland-desktop
**Depends on:**
- 299e46f2-2372-4bbe-9f00-b37a3621f72a — Compositor lock flag and surface
**Execution:** agent

Lock the session immediately from a shell surface via the
control channel, with locked-state display from snapshots.

*Technical detail:* [context.md#task-manual-lock](./context.md#task-manual-lock)

**Acceptance criteria**:
- [x] Manual lock engages the lock screen at once

### M2 — Only real auth unlocks

#### - [x] Task: Greetd unlock path
**Id:** 7a0f83af-ecc9-4f91-96e4-4826e15436ac
**Repo:** rust-wayland-desktop
**Depends on:**
- 299e46f2-2372-4bbe-9f00-b37a3621f72a — Compositor lock flag and surface
**Execution:** agent

Verify unlock credentials through the greeter crate's
greetd path; correct credentials restore the session,
wrong ones stay locked leaking nothing.

*Technical detail:* [context.md#task-greetd-unlock](./context.md#task-greetd-unlock)

**Acceptance criteria**:
- [x] Correct password dismisses the lock intact
- [x] Three wrong passwords stay locked with no content leaked

## Open Questions

- Whether unlock can reach a live greetd daemon in a
  Roost session — only discoverable at runtime. If not,
  lock engages with unlock deferred and reported rather
  than invented.

## Out of Scope

Fingerprint or smartcard factors, remote lock
administration, and per-user lock themes are out of scope;
the spec's Non-Goals section governs.

## Changelog

### 2026-09-29 — Task: Compositor lock flag and surface

**What was done**: New pure session-lock module (flag plus
idle accumulator) wired into the runtime with an exclusive
lock surface hiding all content; idle timeout and control
Lock command engage it; snapshots carry the locked flag
only; shell reconnect while locked stays locked. Tests
cover timeout, idempotence, activity reset, content
hiding, and reconnect persistence.

**Deviations**: control protocol bumped 0.5 to 0.6
(trailing locked flag on snapshots, trailing Lock
command) — wire-incompatible with older peers by design,
both sides ship from this workspace.

**Files changed**:
- `crates/compositor/src/lock.rs`
- `crates/compositor/src/runtime.rs`
- `crates/compositor/src/control.rs`
- `crates/compositor/src/lib.rs`
- `crates/compositor/tests/lock.rs`
- `crates/shell-control-schema/src/lib.rs`
- `crates/shell-host/src/control.rs`

**Discoveries**: protocol follows the 0.2 precedent —
trailing fields with version bump, peers upgraded in lockstep.

### 2026-09-29 — Task: Manual lock from the shell

**What was done**: Shell trigger plus snapshot display —
`ControlClient::lock()` sends the tokenless command and
the display reads the locked flag. The trigger surface is
a lock row in the power tile menu: the press arms a
request the run loop's driver sends, engaging the screen
when the snapshot lands. Tests cover send, at-once
engage, row arming, and drive-through-hub delivery.

**Deviations**: the trigger surface was missing from the
first pass (client method only) and was added on review:
power-menu lock row plus drive step, following the dock
action arm-and-drive shape.

**Files changed**:
- `crates/shell-host/src/control.rs`
- `crates/shell-host/src/popup.rs`
- `crates/shell-host/src/panel.rs`
- `crates/shell-host/src/tiles.rs`

**Discoveries**: none.

### 2026-09-29 — Task: Greetd unlock path

**What was done**: Unlock mediation reuses login auth
through a client seam — correct credentials clear the
flag, restore the session, and hide the surface; every
failure mode denies silently leaving flag, surface, and
hub mirror untouched. Tests cover correct unlock,
single/triple wrong passwords, daemon-unreachable,
mid-conversation drop, bounded prompts, and lock/unlock
restore integration.

**Deviations**: none.

**Files changed**:
- `crates/compositor/src/unlock.rs`
- `crates/compositor/src/runtime.rs`
- `crates/compositor/src/control.rs`
- `crates/compositor/tests/lock.rs`
- `crates/compositor/Cargo.toml`

**Discoveries**: none.
