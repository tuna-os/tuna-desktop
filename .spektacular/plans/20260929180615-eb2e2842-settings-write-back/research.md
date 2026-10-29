---
created_date: "2026-09-29"
document_status: final
closed_date: "2026-09-29"
---

# Research: 20260929180615-eb2e2842-settings-write-back

## Alternatives considered and rejected

### Option A: Direct backend writes from UI

Paint/menu code calls gio set directly.

**Rejected**: scatters the snapshot seam.

### Option B: Parallel write-only client

Separate settings writer.

**Rejected**: duplicates schema handling.

### Option C: Hand-edited configs for Roost knobs

Files the user edits by hand.

**Rejected**: spec requires no hand-editing.

## Chosen approach — evidence

Backend trait extension (`settings.rs:36,71,138`),
publish path (`panel.rs:1535`), wallpaper decode
(`compositor/wallpaper.rs:134`), atomic-file precedent
(notification queue).

## Files examined

- `shell-host/src/settings.rs:16,36,71,78,138,175` —
  model, backend, reads, refresh, tests.
- `shell-host/src/panel.rs:1096,1535` — publish.
- `compositor/src/wallpaper.rs:28,134` — path, decode.

## External references

- gio::Settings write API — mirrors reads.
- XDG basedir — prefs location.

## Prior plans / specs consulted

- settings-compat, notification-presence plans.
- settings-write-back spec.

## Open assumptions

- Fake backend adequate; Settings agreement via second
  handle; prefs stay tiny.

## Drafting assumptions

### Writes extend the backend trait (discovery)

- **Decision**: set_* calls beside refresh on SettingsBackend
- **Rationale**: one backend seam; paint stays on snapshot truth
- **Rejected**: direct gio writes from UI, parallel client

### Fake backend for write tests (discovery)

- **Decision**: round-trip through a controlled backend in test
- **Rationale**: same seam as refresh tests; no dconf in CI
- **Rejected**: live dconf writes in tests

### Writes on the backend seam, prefs atomic-file (architecture)
- **Decision**: extend trait + tiny prefs surface; immediate re-read after write
- **Rationale**: one seam owns schema truth; paint never blocks on writes
- **Rejected**: direct UI writes, parallel client, hand-edited configs

### Three tasks, clock and wallpaper parallel (tasks)
- **Decision**: clock, wallpaper independent; prefs depends on clock for surface pattern
- **Rationale**: wallpaper needs no clock code; prefs reuses the toggle surface shape
- **Rejected**: single task (mixes three acceptance outcomes)

## Rehydration cues

- Re-read `settings.rs:36-150`.
- `cargo test -p roost-shell-host settings`.
- Spec: `20260929180615-eb2e2842-settings-write-back`.
