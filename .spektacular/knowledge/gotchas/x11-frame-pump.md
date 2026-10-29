---
tags: [smithay, compositor, xwayland, x11]
---

# Pump x11_surfaces() on the frame loop — toplevels() is xdg-only

Smithay's `Space::toplevels()` (and the xdg-shell window map behind it)
never yields X11 windows. An XWayland window that is mapped, committed,
and correctly stacked will still paint nothing if the render loop only
walks the xdg toplevel list: the symptom is an "invisible window" whose
`WM_NAME`/`WM_CLASS` resolve fine and which even takes focus, but which
never produces pixels.

What to do instead: enumerate `x11_surfaces()` alongside the xdg
toplevels on every frame tick and submit both to the renderer. Xwayland
paces surface commits on the compositor's frame callback, so the pump
must live in the frame loop, not in a map/commit hook — hooking
map or commit alone still drops frames and leaves the window blank.

Applies to `crates/compositor` (`windows.rs`, `runtime.rs`, `xwayland.rs`).
Found while integrating on-demand XWayland supervision (XWayland plan,
Sept 2026): the first implementation walked only xdg toplevels and
showed exactly this blank-window symptom under `DISPLAY=:99` xterm.
