---
created_date: "2026-09-29"
document_status: final
closed_date: "2026-09-29"
---

# Research: 20260929180627-9e6f4a0d-session-lock

## Alternatives considered and rejected

### Option A: Shell-owned lock flag

Flag in the restartable shell.

**Rejected**: a crash would unlock.

### Option B: External locker process

swaylock-style sidecar with own credentials.

**Rejected**: second credential path.

### Option C: New protocol surface

ext-session-lock style machinery.

**Rejected**: overlay shape already covers it.

## Chosen approach — evidence

Runtime ownership (`runtime.rs:188-201`), Overlay
(`overlay.rs:29-49`), greeter crate, control frames
(`control.rs:129-191,218-291`).

## Files examined

- `compositor/src/runtime.rs:188,196,235,380`.
- `compositor/src/overlay.rs:22-49`.
- `compositor/src/control.rs:129,218,241,308,392`.
- `greeter/src/client.rs, model.rs`.

## External references

- greetd IPC docs — unlock message flow.
- Input timestamps — idle basis.

## Prior plans / specs consulted

- Parity/scrollable plans, notification plan.
- session-lock spec.

## Open assumptions

- greetd reachable for unlock (STOP if not); timeout
  from settings plumbing; overlay hides all by
  construction.

## Drafting assumptions

### Compositor-owned flag, greetd unlock (discovery)

- **Decision**: lock state on Runtime; unlock via greeter crate path
- **Rationale**: crash-safe per constraint; no parallel credentials
- **Rejected**: shell flag, external locker, new protocol surface

### Lock on Runtime, overlay variant, greetd reuse (architecture)
- **Decision**: flag + idle on runtime; lock as overlay shape; auth via greeter crate
- **Rationale**: crash-safe, no new surface machinery, no parallel credentials
- **Rejected**: shell flag, external locker, new protocol surface

### Three tasks, flag first (tasks)
- **Decision**: lock flag/surface, then manual lock and unlock in parallel after
- **Rationale**: both build on the flag; auth success stubbed at client seam
- **Rejected**: single task (mixes lock, trigger, auth outcomes)

## Rehydration cues

- Re-read `runtime.rs:188-260`, `control.rs:218-290`.
- `cargo test -p roost-compositor overlay`.
- Spec: `20260929180627-9e6f4a0d-session-lock`.
