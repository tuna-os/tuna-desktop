# Wayland protocols

What Tuna Desktop advertises to clients, against what GNOME 51's Mutter
advertises. Ledger row P-SY-06 tracks the gaps.

**Source of the Mutter column.** A `wayland-info` capture of GNOME
Shell 51.0 / Mutter 51.0 (Fedora 45, headless, 1280x800):
`tests/protocols/gnome51-wayland-info.txt`, made by
`scripts/tuna-gnome-wayland-info`. Three globals do not appear headless
(the DRM lease manager needs the native KMS backend, the syncobj manager a
renderer device with timeline syncobjs; the
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
at the version given. A `cond:` global is capability-gated in Mutter too:
it may be absent, but when offered it must be at GNOME's version. Known
gaps are reported and do not fail the stage.

The same stage additionally compares against the native Marlin TSV,
retaining `wayland-info-marlin-compare.txt`. Every native captured global
is represented by a match, a version floor or a recorded deviation; a
source-only floor is never described as a measured native version.

**Golden test.** `crates/compositor/tests/protocols.rs` asserts the exact
list and versions below, as an ordinary client sees them. Adding or
dropping a protocol updates the test and this page together. The one
Xwayland-only global is not in that list; its own tests assert that
ordinary clients never see it.

**Qualification boundary for #364.** This branch contains new implementation
and consumer source, not a completed qualification receipt. Graphical journeys,
installed native-package proof and exact-head CI are still required for this
integrated stack. Focused source/socket checks do not replace them. The retained
captures above describe their recorded older revisions only. P-SY-09 remains partial.

The comparison source is [Mutter 51.0](https://github.com/GNOME/mutter/tree/138a14fbeef09d49ebf5be8a0cb83b042dd5c841).
Actual locked generator inputs are `wayland-server 0.31.14` (core XML maxima:
seat/keyboard 11, fixes 2) and `wayland-protocols 0.32.13` (xdg-shell 7,
text-input-v3 2, dmabuf 6; the other new extension managers are version 1).
`scripts/tuna-protocol-xml-stage` verifies the actual cached `.crate` against
Cargo.lock and compares every resolved protocol file with that archive before
staging the exact checked bytes. It does not require Cargo's optional
`.cargo-checksum.json` sidecar or substitute the distro's protocol XML.
These library maxima are not permission to advertise unimplemented behavior.
This native candidate deliberately retains seat 9, text-input manager 1 and
ShellControl 0.47 until the ordered input/OSK cutover is integrated.


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
| xdg_wm_base | 7 | popups with positioner constraints and grabs; edge states from Mutter's edge constraints: maximized is tiled and constrained on every edge, a half tile on its top, bottom and outer edge (Tuna Desktop pairs no tiles, so the inner edge stays free); constrained states only to version 7 binds |
| zxdg_output_manager_v1 | 3 | |
| zwlr_layer_shell_v1 | 4 | Tuna Desktop's own shell draws through it |
| zwp_linux_dmabuf_v1 | 5 | only when the renderer reports formats; as Mutter, version 5 when the renderer's EGL device names a render node (else its DRM node), with default feedback naming that device and one tranche of the renderer's formats, and version 3 without one; no scanout tranche, since every surface is composited |
| wp_linux_drm_syncobj_manager_v1 | 1 | explicit sync, offered only where Mutter offers it; see below. Not in the golden headless list: the test compositor has no renderer |
| xdg_activation_v1 | 1 | focus policy below |
| wp_viewporter | 1 | |
| wp_fractional_scale_manager_v1 | 1 | preferred scale 1 until #59 |
| wp_single_pixel_buffer_manager_v1 | 1 | |
| wp_cursor_shape_manager_v1 | 2 | shapes accepted; the compositor cursor is still the default arrow |
| zwp_idle_inhibit_manager_v1 | 1 | a live inhibitor holds off the idle lock |
| zwp_text_input_manager_v3 | 1 | text fields reach the input method (#60) |
| zwp_input_method_manager_v2 | 1 | private to the compositor-supervised IBus connection, using the same server-owned transport authorization as virtual keyboard; ordinary applications cannot discover or force-bind it. This is not a public GNOME IME API. One exact live input-method resource owns the seat; unavailable objects and their inert child IDs cannot type, observe surrounding text, reserve popup roles, take grabs or retire the owner |
| zwp_pointer_gestures_v1 | 3 | swipes reach apps; three-finger swipes are the shell's (overview, workspaces) |
| zwp_relative_pointer_manager_v1 | 1 | raw motion from the DRM backend |
| zwp_tablet_manager_v2 | 2 | tablets, tools (pressure, tilt, distance, buttons, tool cursors) and pads (buttons, rings, strips, mode groups, application labels shown on the pad OSD) from libinput on the DRM backend; the vendored Smithay adds pads, labels and version 2 (`third-party/smithay/TABLET-PAD-API-PATCH.diff`); pad dials need libinput 1.26 (the `input` crate's `libinput_1_26` feature), newer than Ubuntu 24.04's 1.25 (#344) |
| zwp_pointer_constraints_v1 | 1 | a locked pointer stays put; a confined one stays in its window |
| ext_session_lock_manager_v1 | 1 | the shell's lock screen; only the supervised shell's lock is granted, and only a password the compositor verified unlocks |
| wp_presentation | 2 | CLOCK_MONOTONIC, as Mutter; see frame timing below |
| wp_fifo_manager_v1 | 1 | a barrier is released on the next refresh cycle |
| wp_commit_timing_manager_v1 | 1 | a timed update applies on the refresh cycle whose next presentation reaches its time |
| xdg_wm_dialog_v1 | 1 | a modal dialog is attached to its parent: focusing the parent focuses the dialog (GNOME's attach-modal-dialogs) |
| zxdg_exporter_v2, zxdg_importer_v2 | 1 | xdg-foreign: the portal parents its dialogs to the app's window, which centres and attaches them like the app's own |
| zxdg_exporter_v1, zxdg_importer_v1 | 1 | GTK 3's xdg-foreign: one handle table with v2, as Mutter, so the portal's v2 import parents its dialogs to a GTK 3 window; revoking an export destroys its imports and clears their parents (`third-party/smithay/XDG-FOREIGN-V1-PATCH.diff`) |
| xdg_system_bell_v1 | 1 | plays GNOME's `bell-window-system` sound through `canberra-gtk-play` when installed (Mutter's audible bell); a burst of rings is one sound |
| xdg_toplevel_tag_manager_v1 | 1 | each window keeps its tag and description |
| xdg_toplevel_drag_manager_v1 | 1 | attached windows follow the actual DND pointer in floating and scroll layouts; attach-before-start, null-buffer unmap, source/owner exit, drop and Escape follow the source lifetime. Implements XML-required `invalid_source`, `toplevel_attached` and `ongoing_drag`, including selection-source ordering and actual implicit-grab/client ownership. Mutter 51 omits some of these errors; that source difference is not an exception to the XML contract. `scripts/tuna-toplevel-drag-proof` is an unexecuted real-Chromium consumer, not a browser pass |
| xdg_session_manager_v1 | 1 | window session save and restore as in Mutter 51 (P-SY-08): a client that opens a session gets its windows back after logout or reboot (workspace, position and size, maximized, tiled, fullscreen, output; minimized is not restored, as in Mutter); store `$XDG_DATA_HOME/tuna/window-sessions.json`, written 3 s after a change and at exit (`crates/compositor/src/xdg_session.rs`) |
| zwp_keyboard_shortcuts_inhibit_manager_v1 | 1 | after Allow, the focused window gets every key, the shell's grabs, Super and Alt+Tab included; Super+Escape (Mutter's restore-shortcuts) takes them back until the window is focused again |
| zwp_xwayland_keyboard_grab_manager_v1 | 1 | Xwayland only, as Mutter (hidden from every other client, so `wayland-info` and the golden list never see it). Rootless Xwayland asks for a grab when an X11 client grabs the keyboard. Grants follow Mutter's `meta_xwayland_grab_is_granted`: see below |
| wp_pointer_warp_v1 | 1 | honoured while the surface has pointer focus from the enter serial it names; nested, the host pointer stays where it is |
| wl_fixes | 1 | `destroy_registry` destroys the registry as Mutter does: the client's id is freed and that registry hears no more globals |
| ext_background_effect_manager_v1 | 1 | empty capabilities until real bound-framebuffer/shader admission, then `blur`; capabilities are revoked on render failure. Double-buffered copied regions, synchronized subsurfaces, clipping/subtraction and object destruction drive the actual GLES blur. Floating-point scenes use precision-keyed FP16 intermediates without clipping extended values; admission/drawing must remain before HDR final output conversion. The actual-output consumer `scripts/tuna-background-effect-proof` is prepared but unexecuted; unsupported exit 77 is not a pass. Overview/Alt+Tab clone blur remains unqualified |
| wp_drm_lease_device_v1 | 1 | native only, after a master-only kernel lease ioctl establishes actual support/authority. Confirmed connected non-desktop or explicitly named connectors stay off the desktop. Leases claim eligible free CRTC/primary/cursor resources, withdraw connectors and revoke before DRM master is dropped; failed kernel revocation cannot report completion or re-offer resources. Prepared D-LEASE checks real vkms lessee FD/ioctls, non-master device FD, independent-client withdrawal, empty/duplicate/stale denial, owner-loss revocation with the FD retained, and actual emulated sysfs disconnect/reappearance. These additions have not run on this integrated head. Physical headsets, monitors.xml `for-lease`, and secondary-GPU lease devices are not qualified |

The negotiated xdg-shell consumer tests use actual v7 and v6 connections and
configure events through tiling, maximize, fullscreen, floating/scroll layout,
repeated requests and role replacement. A v6 bind must not change another
client's v7 edge states or receive the v7-only constrained-state values.
Required XML errors reject nonpositive window geometry on `xdg_surface`,
negative min/max hints on `xdg_toplevel`, and inconsistent final limits only
when the double-buffered state commits. Zero maxima remain unbounded. Unknown,
consumed and stale configure ACKs report `xdg_surface.invalid_serial`.
These required geometry/ACK errors are deliberately stricter than the pinned
Mutter implementation's warning/ignore behavior; they are not claimed as
identical Mutter error handling.

Toplevel-drag source reservations survive cancellation and factory/child
destruction while the data source lives; they do not permit later clipboard
repurposing or a second toplevel-drag association. Incompatible selection use
reports `xdg_toplevel_drag_manager_v1.invalid_source` on the live factory.
After that factory is destroyed it reports the core
`wl_data_source.invalid_source` on the still-live source, rather than fabricating
an error object for a dead factory. This preserves the source contract without
pretending that the error placement is identical to Mutter's implementation.

Foreign/stale-object and failed-destructor consumers decode the actual
`wl_display.error` wire event and exact offending object ID. An EOF, an I/O error,
or a missing interface lookup after proxy destruction is not substituted for
the required event.

### xdg-activation policy

A token is valid only when the client that asks for it holds keyboard
focus and attaches an input serial. It works once, within 10 seconds.
A valid request focuses the window and raises it unless
`org.gnome.desktop.wm.preferences raise-on-click` is off (Mutter activates
these as `META_CLIENT_TYPE_APPLICATION`). This is Mutter's
focus-stealing prevention in its simplest form: a background app cannot
mint its way to the front.

### DMA-BUF import and feedback

Admission uses the actual backend renderer and its formats/device, not a supplied
capability manifest. A compositor without a backend advertises neither DMA-BUF
nor explicit sync. The runtime and protocol handler share the same backend and
GLES texture cache; backend borrows end before Wayland dispatch and before import
completion emits events or protocol errors.

Each imported buffer is synchronously imported into that renderer while its
`zwp_linux_buffer_params_v1` resource is alive. Failed fallible creation sends
`failed`; failed immediate creation posts `invalid_wl_buffer`, including when
the client queues `params.destroy` immediately after creation. Successful import
retains the actual renderer's cached texture, rather than validating with a
second context and importing again. Import is not sampling: acquire-fence waits
and committed YUV/colour conversion still govern later rendering.

The native consumer source binds v5 and v3 separately: only v3 receives legacy
modifier events. It checks v5's add-time equal-modifier rule and v3's older
cancel-before-create behavior, presents a real v3 GBM buffer, and compares v5
default/surface feedback with real device identity and allocatable buffers.
Regular memfds with the accepted allocation's advertised format/modifier and
plane geometry are explicit failed-import controls, never successful DMA-BUF
fixtures. Successful advertised-device, import, rendering and fencing proof is
still required under the named native emulation; compiling this consumer is
not qualification.

### Explicit sync

`wp_linux_drm_syncobj_manager_v1` follows Mutter 51's
`meta-wayland-linux-drm-syncobj.c` (`crates/compositor/src/explicit_sync.rs`):

- **When it is advertised.** Once the renderer exists, on the DRM backend
  and nested alike (Mutter runs the same check for whatever renderer its
  backend has, with no environment or debug switch), in
  `meta_wayland_drm_syncobj_manager_new`'s order: the EGL display has
  `EGL_ANDROID_native_fence_sync`; the renderer's EGL device names a DRM
  render node, else its DRM node; that node opens read-write; it has
  `DRM_CAP_SYNCOBJ_TIMELINE`; `drmSyncobjEventfd` exists (a probe on
  handle 0 fails with `ENOENT`). The first missing piece is logged as
  `tuna-compositor: explicit sync disabled: <Mutter's reason>`; success as
  `tuna-compositor: explicit sync (linux-drm-syncobj v1) on <node>`.
  Software rendering and KMS-only devices have no timeline syncobjs: vkms
  and bochs report `DRM_CAP_SYNCOBJ_TIMELINE` 0 and `drmSyncobjEventfd`
  `EOPNOTSUPP` (checked locally; the drm-smoke stage D-SYNCOBJ asserts the
  global stays absent on vkms).
- **Errors.** `surface_exists` for a second syncobj surface,
  `invalid_timeline` for a syncobj fd the device cannot import, `no_surface`
  after the `wl_surface` is gone. At commit, in Mutter's order: a buffer
  that is not a dmabuf is `unsupported_buffer`, points or not; a dmabuf
  needs an acquire point (`no_acquire_point`) and a release point
  (`no_release_point`); on one timeline the acquire point must be below
  the release point (`conflicting_points`); a point with no buffer (or a
  null one) is `no_buffer`. The vendored Smithay carries this check
  (`third-party/smithay/DRM-SYNCOBJ-PATCH.diff`).
- **Acquire and release.** Valid acquire points hold real commits with Smithay
  blockers/eventfds; already-signalled points apply immediately. Destroyed
  surfaces and failed watch registration retire pending waits. Invalid
  transactions are rejected before they can apply.
- **GPU-safe release.** Frame buffer-access tracking retains the original
  Wayland buffer through rendering. The final GPU fence is exported, imported
  into DRM and transferred to the release timeline; absence of this complete
  path disables the global. Capability probing exercises that actual route,
  not just extension strings.
- **Not yet qualified on this head.** `scripts/tuna-explicit-sync-proof` uses
  real DRM timeline ioctls, offered feedback formats/modifiers, GBM/EGL buffers,
  presentation and independent output captures, including blocked-acquire and
  teardown/error controls. Its existence is not a pass. The finite native VM
  consumer keeps all 19 sync/dmabuf cases and then drives the actual background
  consumer on that same original admitted virtio/virgl DRM session. Independent
  QMP captures check copied/clipped/subtracted blur regions, synchronized
  child/parent commits, unmap/remap, null and destruction with the same oracle as
  `scripts/tuna-background-effect-proof`. Actual EGL/GL/compiler/DRM driver,
  kernel and installed package identities accompany the receipts. Missing blur
  capability or a failed case is a failure, never an unsupported skip or a forced
  scanout capability. This SDR output comparison does not qualify FP16/HDR
  precision; the colour renderer owns that separate evidence. Neither consumer
  has run on the integrated installed image. vkms/Bochs disabled observations
  cannot qualify successful timeline import or release ordering. ADR 0008
  permits named actual emulation, not stand-in devices or manifests.
- **Protocol error placement.** `surface_exists` is posted on the manager,
  as the XML defines it; Mutter 51 posts it on the surface.

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
focused again. `scripts/tuna-shortcut-proof` exercises Allow, Deny, refocus,
emergency restore, stale focus and lock through a real Wayland test client.

### Xwayland keyboard grabs

An X11 app that grabs the keyboard (`XGrabKeyboard`; virtual machine and
remote desktop viewers do) gets the same shortcut inhibition, without the
dialog, when Mutter 51 would grant it (`meta-xwayland-grab-keyboard.c`,
`meta-settings.c`):

- A rule is matched against the window's WM_CLASS class and instance as a
  GLib glob (`*` and `?`, whole name). Rules starting with `!` deny.
- The rules are Mutter's built-in list (`gnome-boxes`, `remote-viewer`,
  `virt-viewer`, `virt-manager`, `vinagre`, `vncviewer`, `Xephyr`),
  followed by `org.gnome.mutter.wayland xwayland-grab-access-rules`, read
  live with the focus settings.
- A deny match refuses. Otherwise the grab is granted when the client sent
  `_XWAYLAND_MAY_GRAB_KEYBOARD` for the window, or when an allow rule matches.
  Anything else is ignored.
- While the granted window has keyboard focus it gets every key, the
  shell's shortcuts included. Super+Escape ends the grab, until the app
  grabs again. Releasing the X11 grab or closing the window also ends it.
  The lock screen keeps its keys, as with the inhibit protocol.
- A grab is decided once, with the rules then in force: at the request,
  or when its X11 window is first known. Later rule changes do not affect it.

As in Mutter, `xwayland-allow-grabs` only adds a focus lock for
override-redirect windows. Tuna Desktop does not show override-redirect
X11 windows, so it does not read that key.

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
| wl_seat | 10 | 9 | negotiated v10 repeat ownership is prepared separately and must integrate with the authoritative IBus repeat/context bridge before advertisement; old clients retain their own repeat policy (#364) |
| zwp_text_input_manager_v3 | 2 | 1 | complete actions/panel semantics and privileged attributed-preedit integration remain on the input/OSK stack; panel OFF needs the single ordered final ShellControl cutover, not text-input disable or a guessed parallel minor (#364) |
| wp_color_manager_v1 (v2), wp_color_representation_manager_v1 | 2, 1 | missing | not in Smithay 0.7: needs a colour-managed renderer (ICC/HDR, YUV conversion); the GLES path is sRGB only (#89) |

| zwp_linux_dmabuf_feedback_v1 scanout tranches | per-surface feedback adds a scanout tranche for a direct-scanout candidate (`meta_wayland_dma_buf_surface_feedback`) | default feedback only | Tuna Desktop has no direct scanout of client buffers: every surface is composited, so a scanout tranche would point clients at planes nothing uses; it comes with direct scanout (perf lane) |
| gtk_shell1 | 7 | missing | deliberate optional GTK compatibility difference, not “GTK4 needs none”: GTK3 own-modal hints, shell D-Bus metadata and decorated titlebar gesture forwarding are not qualified; see below |

## Deliberate differences

- **xdg-decoration is absent,** as in Mutter. GNOME is client-side
  decorations only, and the golden test asserts the absence.
- **layer-shell is present,** unlike Mutter. Tuna Desktop's shell is a separate
  process and draws its panel, overview and banners through it.

### GTK and rootless Xwayland boundaries

[GTK 3.24.49](https://github.com/GNOME/gtk/blob/3.24.49/gdk/wayland/gdkwindow-wayland.c)
prefers xdg-foreign v2 and falls back to v1. The real GTK3/Nautilus portal
consumer records the observed wire version; a v2 journey cannot certify v1.
Socket clients separately exercise cross-version handles, invalid roles,
revocation, unmap and teardown. Neither prepared suite has run on this head.

GTK3's optional `maybe_set_gtk_surface_modal` and D-Bus metadata paths return
when no gtk surface exists, and its titlebar gesture forwarding returns false.
[Mutter's GTK shell implementation](https://github.com/GNOME/mutter/blob/51.0/src/wayland/meta-wayland-gtk-shell.c)
does implement those behaviors. Portal parenting through xdg-foreign is not
proof of GTK3's own-modal, titlebar or global-menu metadata parity.

The old “rootful `-host-grab` only” explanation for omitting Xwayland keyboard
grabs was incorrect: rootless `XGrabKeyboard` also uses this private protocol.
It is now implemented and offered only to the supervised Xwayland client.
Tuna still launches rootless Xwayland, not a rootful nested X server; the
override-redirect focus-lock difference is described above rather than hidden
by advertising an unexercised rootful mode.


`xdg_toplevel_icon_manager_v1` v1 accepts square SHM icons and sanitized theme names, applied on the next surface commit. PNGs live in a private, bounded compositor cache. X11 `_NET_WM_ICON` and legacy square TrueColor `WM_HINTS` pixmaps (24/32-bit color, optional 1-bit transparency mask) are read on a bounded worker for mapped window identities. Palette and unsupported visuals fall back to a generic app icon.
