---
created_date: "2026-09-29"
document_status: final
closed_date: "2026-09-29"
---

# Research: 20260929180600-6bc465db-quick-settings-toggles

## Alternatives considered and rejected

### Option A: Config-file writes

Toggle via rfkill/ALSA state files.

**Rejected**: forbidden by spec; root-owned paths.

### Option B: Move probes to D-Bus

Rewrite all probes onto service APIs.

**Rejected**: rewrites tested code for no visible gain.

### Option C: Synchronous toggle calls

Block the loop on the service round trip.

**Rejected**: stalls the shell; spec requires pending.

## Chosen approach — evidence

Toggle callers beside sysfs probes (`tiles.rs:164,199,
250`), blocking-proxy shape (`watcher.rs:527`), stub-test
precedent, re-probe confirmation.

## Files examined

- `shell-host/src/tiles.rs:18,31,46,164,199,250` —
  model, states, probes.
- `shell-host/src/watcher.rs:527` — proxy pattern.
- panel/popup menu and tile-key positions per researcher.

## External references

- NetworkManager, sound-server, UPower D-Bus APIs —
  toggle contracts; stubs stand in for tests.

## Prior plans / specs consulted

- notification-presence, icon-theme-lookup plans.
- quick-settings-toggles spec.

## Open assumptions

- Stub service adequate stand-in; sound interface
  resolved at implement; pending reuses Loading.

## Drafting assumptions

### Probes stay filesystem, toggles go D-Bus (discovery)

- **Decision**: keep sysfs probes; add D-Bus toggle calls only
- **Rationale**: probes are tested and honest; wholesale D-Bus
  migration is unneeded scope
- **Rejected**: full probe rewrite, config-file writes

### Pending reuses Loading (discovery)

- **Decision**: toggle-in-flight shows Loading, no new state
- **Rationale**: shape reuse per spec constraint
- **Rejected**: new Pending variant

### Toggle callers beside probes (architecture)
- **Decision**: new D-Bus call sites owned by probe owners; probes unchanged
- **Rationale**: smallest change that honors act-through-services + state-follows-reality
- **Rejected**: probe rewrite, config writes, sync calls

### Three tasks, honesty last (tasks)
- **Decision**: network, sound, then honest-state depending on both
- **Rationale**: honesty tests need both toggles in place
- **Rejected**: folding honesty into each toggle (duplicates outside-change tests)

## Rehydration cues

- Re-read `tiles.rs:1-120`.
- `cargo test -p roost-shell-host tiles`.
- Spec: `20260929180600-6bc465db-quick-settings-toggles`.
