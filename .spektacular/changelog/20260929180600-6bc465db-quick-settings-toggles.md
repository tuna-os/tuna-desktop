---
created_date: "2026-09-29"
document_status: draft
---

# Quick settings toggles

## What was built

Tile menus now act: network radio toggles through the
platform service, sound mutes and sets volume, each with a
pending face until re-probe confirms the real state. The
same re-probe picks up changes made outside Roost within
one tick, and absent or refusing services explain
themselves instead of offering dead toggles.

## Why it matters

Tiles previously reported state they could not change.
With toggles beside the probes, state shown and state
changed stay in one place through the honest-state tile
shape, without blocking the shell loop.

## Deviations from the plan

Sound ships against a documented stub contract: no
standard PulseAudio/PipeWire mute D-Bus control exists in
this environment, so a live sound-server binding is future
work.
