---
created_date: "2026-09-30"
document_status: final
closed_date: "2026-09-30"
---

# XWayland support — project record

X11-only applications now join the Roost session as ordinary windows.
A small on-demand supervisor spawns the XWayland compatibility server
on first legacy need (session opt-in via `--xwayland` /
`ROOST_XWAYLAND=1`) with bounded restarts and exactly one logged
start; the window manager tracks Smithay's unified `Window` handle so
X11 surfaces flow through the same map, stack, focus, configure, and
close paths as native windows, with titles from the X11 title and app
ids from WM_CLASS. Capture and layer-shell probes against legacy
windows hit the same fail-closed gates as native windows.

Legacy-toolkit users stop being blocked while native sessions behave
exactly as before — no server process, no log presence — and the
shell stays X11-ignorant, consuming unchanged snapshot entries. A
missing server binary degrades to a single logged non-fatal report.

Deviations from the plan: (1) the live procedure exposed a real
compositor bug — X11 surfaces never received frame callbacks
(`State::toplevels()` is xdg-only; Xwayland paces content commits on
frame), so X11 windows stayed invisible; fixed with
`WindowManager::x11_surfaces()` plus a frame loop in
`Runtime::render`. (2) No keycode change was needed: the winit
backend already reports XKB codespace and the translate/forward
round-trip lands correct evdev codes on the wire (verified against
smithay 0.7 source and a live protocol trace). (3) Overview opens
invisibly — intent flips and the shell commits a fullscreen overview
layer, but no overview pixels render (transparent canvas, shell-side
gap tracked for the overview track); switcher entries render as id
slots without titles by design, for all window kinds.
