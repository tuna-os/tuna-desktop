# Wellbeing (#354) grayscale/fade renderer protocol proposal (0.35)

For root: concrete effect semantics + wire shape for the two #354 effects
that are currently desired-state logs. Protocol 0.35 is reserved for
wellbeing pending the single combined schema; this lane makes no schema
edit. Constants below are the ported GNOME names already pinned in
`crates/shell-gtk/src/wellbeing.rs`.

## Required effect semantics (observable, gate-testable)

Grayscale (screen-time limit reached):
- Trigger on, fade all outputs from full color to full desaturation over
  3.0 s (`GRAYSCALE_FADE_SECONDS`), ending at desaturation factor 1.0
  (`GRAYSCALE_SATURATION` = full desaturation).
- Restore to full color over 3.0 s when the limit no longer applies
  (new day boundary, limit disabled, or allowance restored).
- Scope: every output. Interaction: nightlight (#339) also transforms
  output color; proposed composition order is temperature, then
  desaturation, then brightness (root/compositor to confirm).

Break fade-screen (break taken):
- Trigger on, fade all outputs to brightness multiplier 0.6
  (`BREAK_BRIGHTNESS_FACTOR`) over 3.0 s (`BREAK_FADE_SECONDS`).
- Restore to 1.0 over 3.0 s at break end, skip, or disable.
- Scope: every output; same composition order as above.

Both effects are output-wide renderer state, not per-window: no app can
observe or veto them, and screenshots capture the transformed frame
(pixel proofs read back through the same frame path as the nightlight
identity-control capture).

## Proposed wire shape (0.35)

Recommended: append one struct to the display/color settings push,
following the `SetInputSettings` pattern (sent at start and on every
change; compositor applies live):

```rust
/// Wellbeing output effects (#354). Added in 0.35.
pub struct WellbeingEffects {
    /// Full desaturation while the daily limit is reached.
    pub grayscale: bool,
    /// 0.6 brightness factor while a fading break runs.
    pub fade: bool,
}
```

Alternative (not recommended): two dedicated `CommandKind` variants. A
single struct keeps grayscale+fade atomic across one settings push and
matches how motion/sound/brightness preferences already ride settings.

Open for root: exact host struct to extend (pending 0.33/0.34 combined
schema), per-output vs all-output scope confirmation, composition order
with nightlight, and the GNOME 51 renderer component that owns the
equivalent path (this proposal specifies observable behavior only and
claims no verbatim GNOME internals).

## Acceptance (not from logs)

- Pixel ramp proofs: saturation/brightness move on trigger and restore
  after, sampled from real frames (negative: no change when disabled).
- Gate split: effect-state diagnostics stay as diagnostics; acceptance
  requires the transformed pixels.
- Inventory rows stay `Honored (partial)` until these proofs pass.
