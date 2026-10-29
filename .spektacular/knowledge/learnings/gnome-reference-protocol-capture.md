---
tags: [gnome-reference, protocols, wayland-info, baseline, wayland]
---

# The GNOME 51 reference container also captures protocols

The headless GNOME 51 container behind `scripts/tuna-gnome-reference`
(`tests/gnome-reference`) serves more than pixel parity. It captures the
Wayland globals and versions Mutter 51 offers: `scripts/tuna-gnome-wayland-info`
runs `wayland-info` against it, and the result
(`tests/protocols/gnome51-wayland-info.txt`, `gnome51-globals.tsv`) is the
evidence P-SY-06 compares Tuna Desktop against (`scripts/tuna-wayland-info-compare`,
proof stage G-WAYLAND-INFO).

Headless Mutter leaves things out. It offers no hardware-only or
Xwayland-only globals: `wp_drm_lease_device_v1`,
`wp_linux_drm_syncobj_manager_v1` and `zwp_xwayland_keyboard_grab_manager_v1`
are absent. It also advertises linux-dmabuf only at v3, without the v4+
feedback it gives on real hardware. Those entries come from Mutter 51's
sources (names in `libmutter-51.so`) and must be marked as source-derived,
with their version treated as a minimum rather than a reading.
