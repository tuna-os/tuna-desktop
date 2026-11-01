# Wayland protocols

What Tuna Desktop advertises to clients, against what GNOME 51's Mutter
advertises. Ledger row P-SY-06 tracks the gaps.

**Source of the Mutter column.** A `wayland-info` capture of GNOME
Shell 51.0 / Mutter 51.0 (Fedora 45, headless, 1280x800):
`tests/protocols/gnome51-wayland-info.txt`, made by
`scripts/tuna-gnome-wayland-info`. Three globals do not appear headless
(the DRM lease and syncobj managers need the native KMS backend; the
Xwayland keyboard grab is offered to Xwayland only). They are listed from
the interfaces built into the same `libmutter-51.so`, marked `source` in
`tests/protocols/gnome51-globals.tsv`. This is upstream GNOME 51 in a
Fedora container, not a capture from the Marlin baseline VM.

**Native Marlin reference.** [Run 37196669447](https://github.com/tuna-os/tuna-desktop/actions/runs/37196669447)
also captured GNOME Shell/Mutter 51.0 in the derived Marlin GNOME 51
baseline on the shared four-vCPU, 6 GiB, 1280×800 QEMU/KVM profile.
`tests/protocols/marlin-gnome-wayland-info.txt` retains its full output;
`marlin-roost-wayland-info.txt` retains the same run's candidate capture.
`marlin-gnome51-source.json` records capture digests, image/package identity,
the trusted package source and the actual observer checkout. The Tuna Desktop
package is source `7546647e5967825f522a352ee781e0908f889d1e`, so this retained
candidate capture does not certify subsequent implementation changes.

This native session exposes 41 globals. Its existing 40 match the headless
capture's versions; `wp_drm_lease_device_v1` is additionally observed at v1.
`tests/protocols/marlin-gnome51-globals.tsv` preserves the same explicit
Tuna Desktop version floors and deviations, marks the lease as captured, and
keeps the two still-unobserved syncobj/Xwayland-private entries as
source-derived floors. Bochs scanout still exposes dmabuf v3, so this
VM does not establish the dmabuf feedback version of a GPU-capable Mutter
backend. The headless reference remains a separate regression input.

**Comparison.** The proof stage G-WAYLAND-INFO runs `wayland-info`
against the nested Tuna Desktop session (saved as the `wayland-info.txt`
artifact) and `scripts/tuna-wayland-info-compare` checks it against
`tests/protocols/gnome51-globals.tsv`. Every global marked `match` must
be present at GNOME's version or newer, and every `min:` global at least
at the version given. Known gaps are reported and do not fail the stage.

The same stage additionally compares against the native Marlin TSV,
retaining `wayland-info-marlin-compare.txt`. Every native captured global
is represented by a match, a version floor or a recorded deviation; a
source-only floor is never described as a measured native version.

**Golden test.** `crates/compositor/tests/protocols.rs` asserts the exact
list and versions below. Adding or dropping a protocol updates the test
and this page together.

## Advertised by Tuna Desktop

| Global | Version | Notes |
|---|---|---|
| wl_compositor | 6 | preferred buffer scale sent per surface, for the output it is on |
| wl_subcompositor | 1 | |
| wl_shm | 2 | |
| wl_seat | 9 | |
| wl_output | 4 | one per connected output |
| wl_data_device_manager | 3 | |
| zwp_primary_selection_device_manager_v1 | 1 | |
| xdg_wm_base | 6 | popups with positioner constraints and grabs |
| zxdg_output_manager_v1 | 3 | |
| zwlr_layer_shell_v1 | 4 | Tuna Desktop's own shell draws through it |
| zwp_linux_dmabuf_v1 | 3 | only when the renderer reports formats |
| xdg_activation_v1 | 1 | focus policy below |
| wp_viewporter | 1 | |
| wp_fractional_scale_manager_v1 | 1 | preferred scale 1 until #59 |
| wp_single_pixel_buffer_manager_v1 | 1 | |
| wp_cursor_shape_manager_v1 | 2 | shapes accepted; the compositor cursor is still the default arrow |
| zwp_idle_inhibit_manager_v1 | 1 | a live inhibitor holds off the idle lock |
| zwp_text_input_manager_v3 | 1 | text fields reach the input method (#60) |
| zwp_input_method_manager_v2 | 1 | IMEs such as fcitx5; candidate popups are tracked like any popup |
| zwp_pointer_gestures_v1 | 3 | swipes reach apps; three-finger swipes are the shell's (overview, workspaces) |
| zwp_relative_pointer_manager_v1 | 1 | raw motion from the DRM backend |
| zwp_tablet_manager_v2 | 2 | tablets, tools (pressure, tilt, distance, buttons, tool cursors) and pads (buttons, rings, strips, mode groups, application labels shown on the pad OSD) from libinput on the DRM backend; the vendored Smithay adds pads, labels and version 2 (`third-party/smithay/TABLET-PAD-API-PATCH.diff`); pad dials need libinput 1.26 (the `input` crate's `libinput_1_26` feature), newer than Ubuntu 24.04's 1.25 (#344) |
| zwp_pointer_constraints_v1 | 1 | a locked pointer stays put; a confined one stays in its window |
| ext_session_lock_manager_v1 | 1 | the shell's lock screen; only the supervised shell's lock is granted, and only a password the compositor verified unlocks |
| wp_presentation | 2 | CLOCK_MONOTONIC, as Mutter; see frame timing below |
| wp_fifo_manager_v1 | 1 | a barrier is released on the next refresh cycle |
| wp_commit_timing_manager_v1 | 1 | a timed update applies on the refresh cycle whose next presentation reaches its time |
| wp_color_manager_v1 | 2 | parametric and ICC (matrix/TRC and 33³ LUT) source descriptions; output descriptions track the admitted per-output SDR or BT.2100/PQ encoding, with change events and surface preference feedback |
| wp_color_representation_manager_v1 | 1 | premultiplied and straight alpha; identity and BT.601/709/2020 full/limited coefficients; identity YCbCr samples raw linear component views or is refused, never guessed through an EGL driver default |
| xdg_wm_dialog_v1 | 1 | a modal dialog is attached to its parent: focusing the parent focuses the dialog (GNOME's attach-modal-dialogs) |
| zxdg_exporter_v2, zxdg_importer_v2 | 1 | xdg-foreign: the portal parents its dialogs to the app's window, which centres and attaches them like the app's own |
| xdg_system_bell_v1 | 1 | plays GNOME's `bell-window-system` sound through `canberra-gtk-play` when installed (Mutter's audible bell); a burst of rings is one sound |
| xdg_toplevel_tag_manager_v1 | 1 | each window keeps its tag and description |
| xdg_session_manager_v1 | 1 | window session save and restore as in Mutter 51 (P-SY-08): a client that opens a session gets its windows back after logout or reboot (workspace, position and size, maximized, tiled, fullscreen, output; minimized is not restored, as in Mutter); store `$XDG_DATA_HOME/tuna/window-sessions.json`, written 3 s after a change and at exit (`crates/compositor/src/xdg_session.rs`) |
| zwp_keyboard_shortcuts_inhibit_manager_v1 | 1 | after Allow, the focused window gets every key, the shell's grabs, Super and Alt+Tab included; Super+Escape (Mutter's restore-shortcuts) takes them back until the window is focused again |
| wp_pointer_warp_v1 | 1 | honoured while the surface has pointer focus from the enter serial it names; nested, the host pointer stays where it is |

### Colour reference and source notices

The colour protocol, transform and representation reference is official
[Mutter 51.0, `138a14fbeef09d49ebf5be8a0cb83b042dd5c841`](https://gitlab.gnome.org/GNOME/mutter/-/tree/138a14fbeef09d49ebf5be8a0cb83b042dd5c841):
`src/wayland/meta-wayland-color-{management,representation}.c`,
`src/compositor/meta-multi-texture-format.c`, the `clutter/clutter/clutter-color-*`
implementation, and `src/backends/meta-color-device.c`. The retained
[native Marlin GNOME 51 identity](../tests/protocols/marlin-gnome51-source.json)
records the comparison image/package digests; it is a historical protocol
baseline, not an HDR/calibration measurement of this candidate.

Rust adaptations retain the original copyright notices in the corresponding
modules: Intel Corporation (2022), Red Hat (2023–2026), SUSE Software Solutions
Germany GmbH (2024), and Barco NV (2018, Niels De Graef). Mutter's Wayland and
multi-texture originals are GPL-2.0-or-later; the Clutter originals are
LGPL-2.0-or-later. The adaptations select GPL-3.0-or-later (LGPL v2 section 3
permits the GPL conversion), under the repository's [LICENSE](../LICENSE),
without warranty. The independently licensed vendored Smithay additions and
isolated patch records remain under its MIT terms; see
[`ROOST-PATCH.md`](../third-party/smithay/ROOST-PATCH.md).
`moxcms` 0.8.1, used to evaluate non-matrix ICC source profiles, is pinned by
Cargo.lock and licensed BSD-3-Clause OR Apache-2.0.

### Colour output and capture contract

The native output path retains colord profile assignment, generated EDID/sRGB
device profiles, imported ICC profiles and calibration sessions. Assigned VCGT
curves reach the actual CRTC gamma ramp, separately from the output colour mode,
as in Mutter 51; Night Light is not a substitute for profile/calibration support.
Connector removal/replacement revokes calibration immediately, and generation
fences prevent an older colord operation from restoring the previous sink's
assignment. Unchanged monitors retain their registrations.

`DisplayConfig` and `monitors.xml` carry default SDR and BT.2100 modes.
Native-SDR mode 2 remains unsupported rather than being advertised with an
incomplete asset-conversion path. HDR requires the intersection of checked CTA
BT.2020 RGB/PQ/type-1 capabilities, atomic connector properties and discovered enum
values, usable ≥10-bpc scanout, and successful FP16/ten-bit renderer probes.
The complete scene blends in extended-range sRGB/gamma 2.2 FP16, then converts
once to BT.2020/PQ. Output metadata, colorspace and bpc are staged with the
atomic framebuffer transaction; SDR clears HDR metadata. Failed admission or
commit retains/reverts the working configuration, and persistent confirmation
waits for an actual hardware commit. Resume restores connector signaling.

Screenshots declare their actual SDR BT.709 primaries/gamma-2.2 PNG encoding
(`cHRM` and `gAMA`, not a false sRGB or HDR tag). Default screencasts likewise
offer full-range BGRx/BT.709/gamma22 independently of the physical output's
profile or HDR state. Following Mutter's monitor-format policy, an active
qualified HDR monitor additionally offers a ten-bit BT.2020/PQ capture; window
and area streams remain SDR. The negotiated encoding drives a real FP16 render
and final output pass, not metadata attached to eight-bit SDR bytes.

EGL's YCbCr import hints do not include identity coefficients and permit driver
guesses when no hint is supplied. Applying an inverse matrix to an already
converted/clipped RGB sample cannot recover the original channels. Identity
YCbCr therefore uses zero-copy component views with retained texture lifetime
and synchronization, rejecting unsupported layouts/modifiers or failed imports.
The supported raw layouts are recorded in the vendor patch notes; this is not a
claim that every EGL driver can import them. Mutter 51.0's three
`identity_limited_to_rgb` assignments have unmatched opening parentheses;
Tuna uses the intended `(255/219) * (channel - 16/255)` expression and tests
distinct channels, range boundaries and alpha rather than matching that syntax
error or special-casing neutral fixtures.

Qualification receipts must name the emulation and exact executable/source/image
identities. `D-COLOR-HDR`, `D-COLOR-YUV`, `D-COLOR-YUV-CAPTURE` and
`D-COLOR-CAPTURE` are executable pixel/consumer gates, not passes merely because
the scripts exist. Existing vkms/real-colord gamma readback remains separate.
[ADR 0008 item 5](adr/0008-v1-beta-track-and-release-scope.md) permits the v1 row
to pass on actual named emulated-device evidence; physical HDR link quality,
instrumented calibration accuracy and physical hotplug remain unqualified until
measured in the post-v1 hardware support matrix. Virtual/SDR captures never
establish a physical HDR claim.

### xdg-activation policy

A token is valid only when the client that asks for it holds keyboard
focus and attaches an input serial. It works once, within 10 seconds.
A valid request focuses the window and raises it unless
`org.gnome.desktop.wm.preferences raise-on-click` is off (Mutter activates
these as `META_CLIENT_TYPE_APPLICATION`). This is Mutter's
focus-stealing prevention in its simplest form: a background app cannot
mint its way to the front.

### Frame timing

presentation-time, fifo and commit-timing follow the frame path
(`crates/compositor/src/frame_timing.rs`):

- Feedback is taken from the surfaces a frame drew. On hardware it is
  marked presented at that output's page flip with the kernel's vblank
  time and sequence and Mutter's KMS flags (vsync, hardware clock,
  hardware completion). Nested, it is marked when the host takes the
  frame, timed by the compositor's clock, with no flags (Mutter's nested
  backends cannot vouch for the host either). A surface not drawn (a
  hidden workspace, the lock screen up) keeps its feedback until it is,
  or until a newer update discards it.
- Fifo barriers and commit timers advance once per refresh cycle (the
  primary output's page flip; nested, each frame) on every surface that
  receives frame callbacks, drawn or not, so a client on a hidden
  workspace never stalls.

### Keyboard shortcuts inhibit

Tuna Desktop asks before letting a focused, mapped app take shortcuts. The GTK
shell shows a modal with Deny and Allow and explains Super+Escape.
Only Allow activates the inhibitor; Deny cannot be undone by refocusing.
Known desktop apps use GNOME's PermissionStore (`gnome` table,
`shortcuts-inhibitor` id, `GRANTED`/`DENIED` values), so remembered grants
skip the dialog. Unknown apps are never remembered. An unavailable store
opens the dialog. Background requests, stale responses, destroyed surfaces
and requests outstanding at lock or a switch to another app fail closed.
The non-GTK fallback shell has no consent UI and keeps requests inactive.
Super+Escape always takes granted shortcuts back until the window is
focused again. GNOME's configurable Xwayland exemption rules are not
implemented. `scripts/tuna-shortcut-proof` exercises Allow, Deny, refocus,
emergency restore, stale focus and lock through a real Wayland test client.

## GNOME D-Bus interfaces

GNOME's portal backend (xdg-desktop-portal-gnome) talks to GNOME Shell
and Mutter over D-Bus, not Wayland. Tuna Desktop serves those interfaces itself,
as niri does, so the stock portal works.

| Interface | Tuna Desktop | Notes |
|---|---|---|
| org.gnome.Shell.Screenshot | Screenshot, ScreenshotWindow | saves a PNG of the screen or the focused window; the name is left to a real GNOME Shell when one runs (nested preview) |
| org.gnome.Mutter.ScreenCast | CreateSession, RecordMonitor, RecordWindow, Start, Stop | monitor and window streams over PipeWire (BGRx, shared memory, up to 30 fps), adapted from niri; window streams follow resizes and end when the window closes |
| org.gnome.Mutter.DisplayConfig | GetCurrentState, ApplyMonitorsConfig | the portal's monitor picker and GNOME Settings' Displays panel (modes, refresh, scale, position, rotation, mirroring, disabling; Mutter 51 verify/temporary/persistent with confirmation) |
| org.gnome.Shell.Introspect | yes | window list for the portal's window picker and running apps (with `sandboxed-app-id`) for its Background portal; desktop-portal callers only, like GNOME Shell (#61). As in GNOME 51, the same `/org/gnome/Shell/Introspect` object also answers under `org.gnome.Shell`, where xdg-desktop-portal-gnome's Background portal asks: the GTK shell relays it to the compositor, which admits the original caller with the same rule (#410) |
| org.freedesktop.PolicyKit1.AuthenticationAgent | BeginAuthentication, CancelAuthentication | the GTK shell registers as this session's polkit agent (system bus, again whenever polkit restarts) and shows GNOME's Authentication Required dialog; passwords go to polkit's helper as libpolkit-agent-1 sends them: through `/run/polkit/agent-helper.socket` (socket-activated `polkit-agent-helper@.service`, polkit ≥ 127) first, else the setuid `polkit-agent-helper-1`. A wrong password restarts the conversation as GNOME does; a helper that fails before asking waits for Authenticate instead of respawning (#884) |
| org.gnome.Mutter.IdleMonitor | GetIdletime, AddIdleWatch, AddUserActiveWatch, RemoveWatch, WatchFired | the session's idle time and watches, served by the compositor; gnome-settings-daemon dims and blanks on it, and banners wait for an away user |
| org.gnome.Shell | ShowOSD, FocusSearch, ShowApplications, GrabAccelerator(s), UngrabAccelerator(s), AcceleratorActivated, AcceleratorDeactivated; Mode, OverviewActive, ShellVersion | served by the GTK shell: gnome-settings-daemon's media keys grab their keys here (the compositor keeps them from apps, in the action modes they ask for, the lock screen included) and show the volume and brightness OSD; GNOME's caller allowlist (Settings, media keys, the GNOME portal backend); a caller's grabs go when it leaves the bus. Grab flags `IGNORE_AUTOREPEAT` and `TRIGGER_RELEASE` are honoured (others refuse the grab): a held key's release reaches its caller as AcceleratorDeactivated, never across a lock or to a re-created grab |

## Gaps against Mutter

From the GNOME 51 capture. The globals not listed here are offered at
GNOME's version.

| Protocol | GNOME 51 | Tuna Desktop | Why, and where tracked |
|---|---|---|---|
| xdg_wm_base | 7 | 6 | Smithay 0.7 creates the global at 6 and implements nothing newer; bumps with Smithay (#89) |
| wl_seat | 10 | 9 | Smithay 0.7 creates the seat at 9 and implements nothing newer; bumps with Smithay (#89) |
| zwp_text_input_manager_v3 | 2 | 1 | Smithay 0.7 implements v1 only; bumps with Smithay (#60) |
| zxdg_exporter_v1, zxdg_importer_v1 | 1 | missing | GTK3 exports with v1; Smithay 0.7 implements v2 only and keeps its handle table private, so a v1 handle could not be imported by the portal's v2 (#61) |
| xdg_toplevel_drag_manager_v1 | 1 | missing | not in Smithay 0.7: a window attached to a drag-and-drop operation and moved with it (detachable browser tabs) (#89) |
| ext_background_effect_manager_v1 | 1 | missing | not in Smithay 0.7: needs a blur pass in the renderer (#89) |
| wl_fixes | 1 | missing | not in wayland-server 0.31; its `destroy_registry` frees a registry object, which wayland-backend owns (#89) |
| wp_drm_lease_device_v1 | 1 (native Marlin capture) | missing | lease needs connectors to hand out for VR headsets; recorded deviation, not exercised by the nested session (#68, #89) |
| wp_linux_drm_syncobj_manager_v1 | source-derived floor 1; absent in this VM capture | missing | explicit sync needs the DRM backend's syncobj import; GPU-capable native acceptance remains open (#68, #89) |
| zwp_xwayland_keyboard_grab_manager_v1 (Xwayland only; from source) | — | missing | only rootful Xwayland with `-host-grab` uses it, and Mutter allows grabs only per `xwayland-grab-access-rules`; Tuna Desktop runs rootless Xwayland (#89) |
| gtk_shell1 | 7 | missing | not planned: GTK4 needs none of it on a GNOME session |
| zwp_linux_dmabuf_v1 feedback (v4+) | native backend only | version 3 | #89 |

## Deliberate differences

- **xdg-decoration is absent,** as in Mutter. GNOME is client-side
  decorations only, and the golden test asserts the absence.
- **layer-shell is present,** unlike Mutter. Tuna Desktop's shell is a separate
  process and draws its panel, overview and banners through it.

`xdg_toplevel_icon_manager_v1` v1 accepts square SHM icons and sanitized theme names, applied on the next surface commit. PNGs live in a private, bounded compositor cache. X11 `_NET_WM_ICON` and legacy square TrueColor `WM_HINTS` pixmaps (24/32-bit color, optional 1-bit transparency mask) are read on a bounded worker for mapped window identities. Palette and unsupported visuals fall back to a generic app icon.
