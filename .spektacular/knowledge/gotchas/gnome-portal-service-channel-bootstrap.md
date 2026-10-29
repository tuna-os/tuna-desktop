---
tags: [dbus, portals, wayland, compatibility]
---

# GNOME portal ServiceChannel bootstrap precedes bus-name ownership

GNOME portal 46 initializes its native GTK Wayland display by calling org.gnome.Mutter.ServiceChannel.OpenWaylandServiceConnection with service type 1; the signature is (u) -> h. This happens before the backend acquires its well-known portal bus name. Requiring that name at bootstrap rejects the installed legitimate backend.

Authenticate the unique sender with its D-Bus process credentials and the installed backend executable and ordinary session UID. Bound connection creation and prune disconnected owners. The returned connection is an ordinary Wayland client: it grants no shell, input-method, capture, or remote-input authority. Capture admission remains separately subject to portal ownership, consent, and lock checks.

Reference: docs/portal-service-channel.md and crates/compositor/src/mutter.rs; upstream Mutter 46 ServiceChannel XML and xdg-desktop-portal-gnome 46 externalwindow-wayland.c.
