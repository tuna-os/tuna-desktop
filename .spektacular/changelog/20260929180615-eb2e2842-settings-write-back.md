---
created_date: "2026-09-29"
document_status: draft
---

# Settings write-back

## What was built

Shell surfaces now write the settings they read: clock
format toggles into the shared key with immediate re-read
and bar re-render, wallpaper sets from the shell with
session apply and restart persistence, and Roost-only
knobs persist in a tiny versioned prefs file — all through
the snapshot layer, so paint never touches the backend and
GNOME Settings reads back identical values.

## Why it matters

The shell could show settings but not change them. With
write-back on the snapshot seam, clock, wallpaper, and
Roost knobs are adjustable without hand-edited files or a
Settings clone, staying in the shared desktop universe.

## Deviations from the plan

None.
