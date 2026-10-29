---
created_date: "2026-09-30"
document_status: draft
project: rust-wayland-desktop
spec: 20260930054036-d6050cbc-xwayland-support
plan: 20260930054036-d6050cbc-xwayland-support
---

# XWayland support in rust-wayland-desktop

> Derived from project rust-wayland-desktop (file), spec/plan 20260930054036-d6050cbc-xwayland-support. See the project-level record for the full feature.

X11-only apps (e.g. xterm, legacy toolkits) now open as normal
windows in a Roost session instead of being unavailable. Start the
session with `--xwayland` (or `ROOST_XWAYLAND=1`) and legacy windows
map, focus, take keyboard/pointer input, and appear in the switcher
alongside native apps; without the flag the session runs exactly as
before with no compatibility server started.

What changed in this repo: new `xwayland` cargo feature on the
compositor plus an on-demand `XWayland` server supervisor
(`crates/compositor/src/xwayland.rs`, readiness Idle/Pending/Ready/Absent
with bounded restarts); the window manager generalized to Smithay's
unified `Window` handle (`windows.rs`, one reconcile path for both
kinds); XWM/shell event handlers, runtime spawn plumbing, and session
opt-in (`lib.rs`, `runtime.rs`, `main.rs`); pure X11→snapshot identity
mapper with fallbacks; shared capture/layer privilege gates extended
to X11 entries (`tests/x11_gates.rs`); headless policy unit tests;
and frame-callback production for X11 surfaces
(`WindowManager::x11_surfaces()` + `Runtime::render` loop) so
Xwayland actually commits window content.

Why: the hardware-and-app-compatibility track requires legacy X11
applications to work in the modern stack, reusing the existing
manager, focus, overview, and switcher paths rather than forking
parallel X11 handling that would rot on every future change.
