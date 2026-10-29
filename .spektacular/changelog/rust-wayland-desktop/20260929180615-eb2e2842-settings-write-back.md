---
created_date: "2026-09-29"
document_status: draft
project: rust-wayland-desktop
spec: 20260929180615-eb2e2842-settings-write-back
plan: 20260929180615-eb2e2842-settings-write-back
---

# Settings write-back in rust-wayland-desktop

Clock format, wallpaper, and Roost-only options are now
adjustable from the shell and persist — written through
the snapshot layer into shared settings GNOME Settings
agrees with.

> Derived from project rust-wayland-desktop (plan store), spec/plan 20260929180615-eb2e2842-settings-write-back. See the project-level record for the full feature.

## What changed in this repo

- Backend trait gains guarded writes with immediate
  re-read and last-good fallback (`settings.rs`).
- Clock toggle and wallpaper picker surfaces with
  session apply (`popup.rs`, `panel.rs`, `tiles.rs`).
- New atomic prefs module for Roost-owned knobs
  (`prefs.rs`).

## Why

Showing settings without changing them is half a shell;
writes belong on the same seam reads use.
