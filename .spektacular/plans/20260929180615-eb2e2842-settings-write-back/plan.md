---
created_date: "2026-09-29"
document_status: final
closed_date: "2026-09-29"
---

# Plan: 20260929180615-eb2e2842-settings-write-back

<!-- Metadata -->
<!-- Created: 2026-09-29T21:20:00Z -->
<!-- Commit: e5f7520 -->
<!-- Branch: main -->
<!-- Repository: rust-wayland-desktop (/home/ubuntu/dev/rust-wayland-desktop) -->

## Overview

Roost surfaces gain safe write-back into the settings they
already read: clock format toggle, wallpaper setter, and a
home for Roost-only knobs — all through the snapshot layer
paint code already trusts.

## Conventions

No project conventions apply to this feature — the
always-applied load returned no entries and the spec
carries no design references.

## Architecture & Design Decisions

Writes enter through the same door reads use. The
`SettingsBackend` trait gains write calls beside refresh —
clock format, wallpaper URI — implemented against the
shared desktop schemas, so GNOME Settings reads back
identically what Roost wrote. After each write the layer
re-reads immediately, so paint code keeps seeing snapshot
truth and never touches the backend; failures keep the
last good snapshot with no error storm. Roost-only knobs
live in a tiny prefs surface persisted to XDG state with
the atomic pattern the notification queue proved, edited
through existing UI shapes rather than files.

This beats direct backend writes from UI code (scatters
the seam the snapshot was built to own), a parallel
write-only client (duplicates schema and keep-on-absent
handling), and hand-edited configs (explicitly out). The
fake backend in tests round-trips writes through the same
trait the gio implementation serves, so CI proves the
behavior without touching real dconf. See
`research.md#alternatives-considered-and-rejected`.

## Component Breakdown

- **Backend writes (changed)** — `set_clock_format`
  and `set_wallpaper_uri` on the settings backend,
  implemented against shared schemas, re-read after
  write, failing back to the last good snapshot.
- **Roost prefs (new)** — tiny owned-knob surface over an
  atomic XDG state file; adjustable through shell UI,
  surviving restarts.
- **Shell surfaces (changed)** — clock toggle and
  wallpaper setter trigger writes through the snapshot
  layer; paint keeps reading the snapshot only.

## Data Structures & Interfaces

The backend trait grows write calls returning success
or a quiet failure the layer absorbs:

```
set_clock_format(ClockFormat) -> WriteOutcome
set_wallpaper_uri(String) -> WriteOutcome
```

`WriteOutcome` is applied-then-reread or kept-last-good;
it carries no error UI. Roost prefs are a small
key/value map serialized beside the notification queue
file. No paint-facing types change.

## Implementation Detail

One pattern extended, one small surface added. The
backend trait that reads today also writes tomorrow, with
the same keep-on-absent gentleness on failure; the prefs
surface follows the notification queue's atomic-file
shape. A developer reading the result sees writes and
reads converging on one seam, with UI code calling
(intent, value) pairs and never naming a schema.

## Dependencies

- gio Settings — write calls mirroring the reads
  already used; already depended on, no changes.
- Shared desktop schemas (clock, wallpaper) — write
  targets GNOME Settings also reads; external, no changes.
- XDG state dir — Roost prefs file; runtime, no changes.
- notification-presence plan (landed) — atomic state-file
  pattern reused; no changes needed.

Design documents this plan was built on: none — the spec
carries no design references.

## Testing Approach

Behavioural tests carry both success metrics through
a controlled backend: clock toggle round-trips the shared
key with bar re-render, and wallpaper set from the shell
appears in the session and survives restart. Unit tests
cover prefs persistence, failure fallback to last-good,
and the writable-key allowlist. Heaviest coverage sits on
the backend writes, where the spec's round-trip guarantee
lives. Deliberate gap: no live GNOME Settings
co-verification — agreement is asserted by reading back
through a second backend handle, not by launching the app.

## Milestones & Tasks

### M1 — Shell writes shared settings

Clock format toggle and wallpaper setter write through
the snapshot layer into shared keys, with immediate
re-read and failure fallback. Validation: round-trip
behavioural tests through a controlled backend.

### M2 — Roost knobs have a home

Roost-only options persist in a tiny prefs surface
surviving restarts, adjustable without hand-edited files.
Validation: change-restart-keeps-value test.

### M1 — Shell writes shared settings

#### - [x] Task: Clock format write-back
**Id:** 3de83cbc-3e1b-4195-91a4-6a9d710ee648
**Repo:** rust-wayland-desktop
**Depends on:** none
**Execution:** agent

Add clock-format writes to the settings backend with
immediate re-read, wired to a shell toggle surface, so the
bar re-renders within one tick and GNOME Settings agrees.

*Technical detail:* [context.md#task-clock-write](./context.md#task-clock-write)

**Acceptance criteria**:
- [x] Shell toggle flips the shared key and bar re-renders within one tick

#### - [x] Task: Wallpaper setter
**Id:** b0bb5a8b-bb67-418d-b664-19ffccc1b414
**Repo:** rust-wayland-desktop
**Depends on:** none
**Execution:** agent

Add wallpaper-URI writes to the settings backend, wired to
a shell picker surface, so the session background updates
and survives restart.

*Technical detail:* [context.md#task-wallpaper-set](./context.md#task-wallpaper-set)

**Acceptance criteria**:
- [x] Shell-set wallpaper applies and survives session restart

### M2 — Roost knobs have a home

#### - [x] Task: Roost prefs surface
**Id:** 187c17f0-5eba-481b-9476-e697325a60fb
**Repo:** rust-wayland-desktop
**Depends on:**
- 3de83cbc-3e1b-4195-91a4-6a9d710ee648 — Clock format write-back
**Execution:** agent

Persist Roost-only options in an atomic state file with a
shell editing surface, surviving restarts without
hand-edited files.

*Technical detail:* [context.md#task-roost-prefs](./context.md#task-roost-prefs)

**Acceptance criteria**:
- [x] Changed Roost option shows the kept value after shell restart

## Open Questions

- Whether the wallpaper publish path picks up a
  backend-written URI without extra plumbing — only
  discoverable by wiring it. If not, the implementer
  extends the publish trigger and reports back.

## Out of Scope

A Settings app clone, window-manager or
shell-extension key writes, and multi-user or permissions
management are out of scope; the spec's Non-Goals section
governs.

## Changelog

### 2026-09-29 — Task: Clock format write-back

**What was done**: Backend trait gains a guarded string
write; shell clock toggle writes the shared key with
immediate re-read and bar re-render within one tick;
refused writes keep the last good snapshot. Tests cover
key flip, re-read, re-render, and refusal fallback.

**Deviations**: none.

**Files changed**:
- `crates/shell-host/src/settings.rs`
- `crates/shell-host/src/panel.rs`
- `crates/shell-host/src/popup.rs`
- `crates/shell-host/src/tiles.rs`

**Discoveries**: none.

### 2026-09-29 — Task: Wallpaper setter

**What was done**: Wallpaper-URI writes through the
backend against the shared schema with blank refusal;
shell picker triggers write with session apply through
the existing publish path and restart persistence via
re-read. Tests cover apply, restart, round-trip, and
refusal fallback.

**Deviations**: none.

**Files changed**:
- `crates/shell-host/src/settings.rs`
- `crates/shell-host/src/tiles.rs`
- `crates/shell-host/src/panel.rs`
- `crates/shell-host/src/popup.rs`

**Discoveries**: none.

### 2026-09-29 — Task: Roost prefs surface

**What was done**: New prefs module with a tiny versioned
schema (clock weekday knob) over an atomic XDG state file;
shell surface edits it; missing/corrupt/skewed files read
as defaults. Tests cover defaults, round-trip atomicity,
and change-restart persistence.

**Deviations**: none.

**Files changed**:
- `crates/shell-host/src/prefs.rs`
- `crates/shell-host/src/lib.rs`
- `crates/shell-host/src/panel.rs`
- `crates/shell-host/src/popup.rs`

**Discoveries**: none.
