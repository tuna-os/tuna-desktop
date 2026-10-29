---
tags: [packaging, winit, nested, compositor, tunaos, drm]
---

# Nested and TTY sessions use different backends

`tuna-compositor` has two backends. `--backend auto`, the default, picks
DRM/KMS when no host display exists, which is the greetd or GDM case on a
TTY: libseat, udev, GBM/EGL and libinput (`crates/compositor/src/drm.rs`).
It picks Smithay's winit backend inside an existing session, where the
compositor is itself a Wayland or X11 client. `--backend winit|drm` forces
one.

The trap is assuming one path proves the other. The nested proofs (the
GTK shell proof, journeys, parity captures) all run winit under Xvfb, and
DRM is proven only by `scripts/tuna-drm-smoke` on vkms. Real-hardware
behaviour (VT switching, suspend, mixed DPI) still needs the Marlin VM
lane (#68). The Marlin image offers the "Tuna Desktop" session at login on
that basis.

Superseded the 2026-10-01 note that the session was nested-only; the DRM
backend landed in #114.
