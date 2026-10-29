---
created_date: "2026-09-30"
document_status: draft
---

# Test plan: keyboard-access (manual procedures)

## Physical-keyboard feel and timing

The plan's deliberate gap: automated tests prove routing, focus,
rings, and activation through `on_key` injection, but not how the
chords feel on a real keyboard (repeat rate, chord rollover,
Super-tap disambiguation against window chords).

- **What to measure** — qualitative: every chord in `docs/keymap.md`
  fires exactly once per press, focus rings track without visible
  lag, and Super+D / Alt+F1 never steal keys from a focused client.
- **How** — run the nested session (`docs/nested-session.md`),
  focus a terminal client, and walk the full keymap: Tab through
  the strip and dock, F6 cycle, Super+D / Alt+F1 jumps, Enter on a
  tile / slot / row, Escape unwinds. Type in the overview box and
  confirm search still receives text while arrows move results.
- **Expected result** — no double-fires, no stuck focus, no client
  key loss; Escape from a popup returns to the parked stop and a
  final Escape returns to the terminal.
- **Who / when** — developer acceptance on hardware before release;
  repeat after any keymap or focus-model change.
