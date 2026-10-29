---
created_date: "2026-09-29"
document_status: draft
---

# Test plan — scrollable-tiling mode

Manual metric (from plan Testing Approach; everything else is automated):
human visual review of the scroll proof frames.

## Procedure: review scroll proof frames

- **What to measure** — the strip looks correct: two 616px columns with a
  16px gap in `02-strip`, first column 826px in `03-preset`, wheel shift
  visible between `04-wheel` frames, and `05-back` identical to GNOME mode.
- **How** — run `scripts/roost-scroll-proof --artifacts /tmp/scroll-review`
  (needs Xvfb + xdotool per `docs/screenshots.md`), then inspect
  `01-float.png`, `02-strip.png`, `03-preset.png`, `04-wheel.png`,
  `05-back.png` in the artifacts dir. The script already asserts geometry
  numerically; this review catches visual wrongness numbers miss
  (overlapping chrome, clipped text, flicker).
- **Expected result** — pass when the strip matches the niri-fidelity
  reference (gaps, widths, overflow) in every frame; file findings as
  follow-up tasks otherwise.
- **Who / when** — maintainer, whenever the strip layout, presets, or proof
  harness change, and pre-release.
