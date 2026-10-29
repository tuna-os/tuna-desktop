---
created_date: "2026-09-29"
document_status: draft
---

# Icon theme lookup

## What was built

Roost now resolves tray and dock icon names through the system's
GNOME icon themes instead of painting letter placeholders. A new
theme resolver walks the Freedesktop icon directories (user theme,
hicolor fallback, pixmaps catch-all), decodes raster artwork with
the existing image stack and SVGs with resvg, and re-tints symbolic
icons into the shell accent color. The shell host keeps a bounded
cache keyed by theme, name, and size, synced on the existing slow
tick; tray fetch resolves named and attention icons through it, and
dock paint resolves each app's desktop-entry icon through the same
cache. Unknown names still show the letter fallback, and switching
the system icon theme clears the cache and repaints bar and dock on
the same tick with no session restart.

## Why it matters

Everyday GNOME apps expose only an icon name over the tray bus or
in their desktop entry. Without theme lookup the Roost bar and dock
showed initials for all of them; with it, the shell reads as a
native GNOME citizen — correct app artwork, visible attention
states, and accent-matched symbolic icons — while the shell process
stays free of any GUI toolkit dependency.

## Deviations from the plan

None. One follow-up recorded in the test plan rather than as a
deviation: the "lookup within one tick" success metric is covered
by a manual latency procedure (resolver suite wall time against
the 2s slow tick) instead of an automated timed assertion.
