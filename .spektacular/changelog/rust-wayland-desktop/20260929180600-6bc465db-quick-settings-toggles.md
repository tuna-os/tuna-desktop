---
created_date: "2026-09-29"
document_status: draft
project: rust-wayland-desktop
spec: 20260929180600-6bc465db-quick-settings-toggles
plan: 20260929180600-6bc465db-quick-settings-toggles
---

# Quick settings toggles in rust-wayland-desktop

Tile menus now switch network radio, mute and volume, with
pending states, re-probe confirmation, outside-change
pickup, and explanations for missing services.

> Derived from project rust-wayland-desktop (plan store), spec/plan 20260929180600-6bc465db-quick-settings-toggles. See the project-level record for the full feature.

## What changed in this repo

- D-Bus radio and sound callers owned by the tile set with
  short timeouts and pending faces (`tiles.rs`).
- Menu rows as trigger surfaces with pending and
  explaining states (`popup.rs`, `panel.rs`).
- Outside-change pickup and missing-service explanations
  with rollback to probed state on refusal.

## Why

Tiles must change the state they show, through platform
services rather than config files, without stalling the
shell.
