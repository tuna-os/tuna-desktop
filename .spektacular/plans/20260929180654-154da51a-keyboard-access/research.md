---
created_date: "2026-09-30"
document_status: final
closed_date: "2026-09-30"
---

# Research: 20260929180654-154da51a-keyboard-access

## Alternatives considered and rejected

### Option A: Compositor-routed shell keys

Route new chords through the compositor's key feed and forward
actions over the control socket.

**Rejected**: shell UI state (which popup row is focused) lives
in the shell; round-tripping every arrow press adds latency and
a wire protocol for what is local UI navigation. Compositor
keeps window chords; shell owns its focus keys — the same split
as the switcher, mirrored.

### Option B: One global focus ring style per surface

**Rejected**: the constraint says reuse the existing accent and
paint style, so rings follow the per-surface selection pattern
already in `overview.rs`, not a new global widget.

### Option C: Minimal status-quo-plus-doc

Overview search keys plus a keymap document only.

**Rejected**: fails the spec's requirements (full operation,
visible focus, no traps) outright.

## Chosen approach — evidence

- One keyboard path exists: seat to `on_key` via `XkbFeed`
  (`panel.rs:1660`, `keyboard.rs:1-156`); vocabulary extends
  there, routing stays in one funnel.
- No focus model exists (no cursors anywhere); the plan adds
  `ShellFocus` beside the funnel rather than threading focus
  through pointer code.
- Selection-highlight pattern in `overview.rs:21-27,129-138`
  is the ring precedent; `ACCENT` const reused.
- Free binding surface verified: Super+{tap,PgUp/PgDn,arrows,
  R,T} and Alt+{Tab,F4} taken (`windows.rs` bindings, pinned
  by `tests/windows.rs`); F6, Super+D, Alt+F1, Ctrl/Alt+letter
  free.
- Recovery overlay (`overlay.rs:63-118`) is the in-tree
  keyboard-navigation precedent to mirror.
- Escape today only closes; the return stack is new state, not
  a behavior flip of existing closes.

## Files examined

- `compositor/src/windows.rs` (bindings, feed, switcher)
- `compositor/src/runtime.rs` (Escape closes overview, overlay feed)
- `compositor/src/overlay.rs` (navigation precedent)
- `shell-host/src/panel.rs` (on_key, interactivity, paint keys)
- `shell-host/src/keyboard.rs` (XkbFeed vocabulary)
- `shell-host/src/popup.rs`, `model.rs`, `overview.rs`,
  `dock.rs` (pointer-only model, no cursors)

## External references

- None. evdev keycodes with X11 = evdev+8 normalization is
  established in-tree (`translate_input`, `XKB_EVDEV_OFFSET`).

## Prior plans / specs consulted

- multi-monitor plan: tick/verify/changelog loop precedent.
- keyboard-access spec: scope, metrics, non-goals.

## Open assumptions

- Shell focus keys need no compositor involvement beyond what
  exists (keyboard already reaches the shell surfaces).
- Dock interactivity can move to OnDemand without breaking the
  pointer paths.
- A scripted key-only run is drivable in the existing headless
  test harness (key injection through `on_key`).

## Drafting assumptions

### Shell-owned focus with local navigation (discovery)
- **Decision**: focus model + vocabulary in the shell; compositor keeps window chords; collision rule pinned by tests both sides
- **Rationale**: UI state is shell-local; round-trips add latency and wire surface for local navigation
- **Rejected**: compositor-routed keys, global ring widget, minimal doc-only

## Rehydration cues

- Re-read `panel.rs:1660-1700` (on_key gating) before designing vocabulary routing.
- `cargo test -p roost-shell-host --lib keyboard` baseline.
- Spec: `20260929180654-154da51a-keyboard-access`.
