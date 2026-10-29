---
created_date: "2026-09-29"
document_status: draft
---

# Test plan: icon theme lookup

## Manual procedure 1 — cold lookup latency fits within one slow tick

**What to measure**: a cold `resolve()` (theme-dir walk plus decode, no
warm cache) completes well within the 2s slow-tick budget that
`ShellHost::update_panel_status` enforces
(`crates/shell-host/src/panel.rs`).

**How**: run the resolver unit suite, which exercises cold lookups
over fixture theme trees (raster, SVG, symbolic, hicolor fallback,
pixmaps catch-all):

```
cargo test -p roost-shell-host icons
```

**Expected result**: exit 0 with all tests passing; the `icons`
test binary reports its total wall time, which must read far below
2s (at authoring time: 11 passed in ~0.01s, i.e. three orders of
magnitude inside budget). If the reported time ever approaches the
2s tick, file a performance follow-up instead of raising the
threshold.

**Who / when**: shell maintainer, pre-release, on the reference
x86_64 runner.

## Deliberate non-goal (no procedure)

Pixel-perfect conformance against every icon theme on disk is
explicitly out of scope: fixture themes cover representative
layouts (index.theme dirs, hicolor fallback, pixmaps), not all
third-party themes. Visual spot-checks of new themes happen
ad-hoc during distro packaging, not as a release gate.
