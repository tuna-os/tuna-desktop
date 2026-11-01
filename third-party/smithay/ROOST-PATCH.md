# Roost Smithay 0.7.0 additive patches

Source: published crates.io smithay 0.7.0, upstream tag v0.7.0,
crate VCS commit a166cf4c94b5aedc332a65aa1dd753e8148829c3.
Published crate SHA256: 740cea6927892bc182d5bf70c8f79806c8bc9f68f2fb96e55a30be171b63af98.
Original MIT license retained as LICENSE.txt. Version remains 0.7.0.

Roost issue #219 needs to advertise a reserved X11 display before starting
XWayland. The additive XWaylandSockets owner prepares and retains Smithay's
existing display lock and listening sockets without spawning a process.
Borrowed listening FDs permit readiness watches. XWayland::spawn_on_sockets
consumes that exact owner, retaining queued first-client bytes and Smithay's
private ClientData construction. The existing spawn API delegates unchanged.

The socket preparation patch changes src/xwayland/mod.rs and xserver.rs. The socket
handoff regression test sends real bytes before transferring the owner.
This source lives outside Cargo's registry vendor directory so factory
cargo vendor --locked vendor remains a separate offline dependency closure.

Issue #62 additionally changes src/backend/drm/surface/gbm.rs; its isolated
source diff is retained in SLEEP-RESET-API-PATCH.diff. The additive
GbmBufferedSurface::discard_pending_frames API removes pending_fb, queued_fb
and next_fb without calling submit or marking a buffer presented. It returns
at most two user-data entries so Roost can explicitly discard abandoned
presentation feedback. current_fb remains retained until KMS reset. This
avoids frame_submitted's implicit queued-frame submission during S3 recovery;
Roost resets the buffer pool and KMS state, then drains obsolete queued DRM
vblank completions before queuing the locked scene. Existing APIs and the
pinned 0.7.0 version remain unchanged.

Issue #346 additionally changes src/input/keyboard/mod.rs. The additive
KeyboardHandle::input_discard API advances physical/XKB state without
calling client or grab input, and removes consumed releases from the held
keys advertised on subsequent focus enter. Modifier changes from intercepted
input are retained until the next forwarded event, so a wake shield cannot
leave the focused client with a stale modifier mask. Real wl_keyboard tests
cover consumed releases, focus-enter key arrays, and same-focus modifier
updates without forwarding the consumed event. The pinned version stays 0.7.0.

Issue #410 additionally completes EWMH window-type recognition in
src/xwayland/xwm/mod.rs and surface.rs. The four previously unrecognized
Desktop, Dock, Combo and Dnd atoms now retain their real type instead of
falling through to no recognized type (which consumers treat as Normal).
WmWindowType gains those four variants. The cached property's first recognized
atom still wins in client preference order, and property change handling is
unchanged. Roost uses this complete type metadata to exclude auxiliary X11
roles from GNOME GetWindows while preserving Normal/Dialog/Utility. The
isolated diff is retained in WINDOW-TYPE-API-PATCH.diff. This is a source API
extension at pinned 0.7.0; no upstream match in the tree is exhaustive over
these variants. Factory packaging must include the final patched source.

Issue #655 additionally changes src/wayland/selection/data_device/dnd_grab.rs
and mod.rs. Upstream offers no way to cancel a client drag: unsetting the
DnDGrab runs its drop path, which still delivers wl_data_device.drop to an
accepted offer. The additive client_dnd_active and cancel_client_dnd
functions let Roost implement Mutter's drag grab, where Escape cancels the
drag. Cancellation marks the offer inactive under the grab lock, then
unsets the grab: the target gets leave, the source cancelled, and Accept,
SetActions or Receive requests already in flight cannot revive the offer.
A drop whose origin or source died is cancelled the same way, and the
terminal path runs once. Real socketpair tests in
crates/compositor/tests/data_device.rs cover pointer and touch drags, with
and without an icon or a source, and a normal drop that can still Finish.
The isolated diff is retained in DND-CANCEL-API-PATCH.diff; the pinned
version stays 0.7.0.

Issue #346 additionally changes src/input/keyboard/mod.rs for GNOME's input
sources and remembered Num Lock. The additive
KeyboardHandle::set_xkb_config_keeping_locks replaces the keymap as
set_xkb_config does, but carries the latched and locked modifiers and the
locked layout over to the new state (a layout past the new keymap's last
falls back to the first), as mutter's meta_wayland_keyboard_update_xkb_state
keeps them; clients receive the new keymap with those modifiers. The
additive XkbContext::set_modifier_locked locks or unlocks one named modifier
(Num Lock at startup), keeping the others and the layout, and reports the
change through with_xkb_state's usual modifier and LED notification. The
existing set_xkb_config is unchanged. The isolated diff is retained in
KEYMAP-STATE-API-PATCH.diff; the pinned version stays 0.7.0.

Issues #503 and #782 additionally change src/backend/renderer/gles/mod.rs
and texture.rs so that a GLES frame creates one texture read fence instead of
one per texture draw. Upstream TextureSync::update_read ran glFenceSync after
every draw. llvmpipe flushes and rasterizes its scene at each fence, so a frame
was rasterized once per drawn element: one GTK4 commit woke the rasterizer
threads about 4.5 times. A frame drawing through an unshared EGL context now
records the textures it reads and, in finish_internal (which Drop also runs
for frames never finished), creates one fence before the frame's flush and
gives it to all of them. An unshared context orders its own commands, and no
other context can reach its textures until the frame ends. Before the fence is
created, the context waits for each texture's previous fence, so the new fence
also covers reads from a context that shared the texture before. A draw
through a shared context (EGLContext::is_shared) still fences while it holds
the texture's read lock, as upstream does, because another thread may take
the write lock as soon as the draw returns; it also fences reads deferred
before the context became shared. Writers still wait for the read fence when
they take the write lock. The read fence, now reference counted, moves out of
the TextureSync RwLock into a ReadSync slot: after a leaked frame, the next
frame's target may hold the write lock of a texture that frame fences.
GL tests in per_frame_read_fence_tests (surfaceless Mesa EGL) cover several
cases: one fence for every texture of a frame, a new fence for each frame,
frames dropped without finish, per-draw fences for shared contexts, and an
upload from a later shared context that must not change a finished frame.
The isolated diff is retained in PER-FRAME-READ-FENCE-PATCH.diff; the
pinned version stays 0.7.0.

Issue #349 additionally changes src/wayland/seat/keyboard.rs and
src/wayland/text_input/mod.rs for GNOME's on-screen keyboard. Upstream
sends zwp_text_input_v3 enter and leave only while an input method
instance exists, so without IBus a text field never learns it has focus
and never enables its text input. Mutter announces text-input focus
unconditionally (meta-wayland-text-input.c), and GNOME's on-screen
keyboard follows that input-method focus; keyboard focus enter and leave
now always send text-input enter and leave, and a text input created
while its client already holds the keyboard (GTK binds it after its first
keyboard enter) is sent enter at once, that instance alone, as
meta_wayland_text_input_create_new_resource does. Input-method
deactivation stays gated on an instance, and text-input requests without
an input method are still discarded by Smithay (Roost tracks them itself
before Smithay dispatches them). The isolated diff is retained in
TEXT-INPUT-FOCUS-API-PATCH.diff; the pinned version stays 0.7.0.

Issue #366 changes src/wayland/session_lock/lock.rs: the lock surface's
pre-commit hook returns early once the client destroyed the
ext_session_lock_surface_v1 role object. After unlock_and_destroy, GTK
destroys the role object and then unmaps the wl_surface (attaches NULL); the
protocol's NULL-buffer and dimension errors apply only while the surface has
the lock-surface role, and posting them on the dead role object disconnected
the shell. The GTK lock-screen proof (scripts/tuna-lock-auth-proof) relocks
after each unlock and covers it. The isolated diff is retained in
LOCK-SURFACE-UNMAP-PATCH.diff; the pinned version stays 0.7.0.

The X11 input-focus fix changes src/xwayland/xwm/surface.rs. Roost's seat
keyboard focus is the X11 window's Xwayland wl_surface, so the X half of
X11Surface's KeyboardTarget enter/leave (SetInputFocus and WM_TAKE_FOCUS by
the ICCCM input mode) never ran: the X server stayed PointerRoot and keys
went to whichever X11 window was under the X pointer. The additive
X11Surface::set_input_focus performs exactly that X half, as Mutter's
meta_x11_display_set_input_focus does; enter/leave are unchanged. The
nested proof G-X11-INPUT-FOCUS (scripts/tuna-gtk-shell-proof) covers it.
The isolated diff is retained in X11-INPUT-FOCUS-API-PATCH.diff; the pinned
version stays 0.7.0.

Issue #341 (wp_color_management_v1 / wp_color_representation_v1) adds two
small APIs. RendererSurfaceState::invalidate_buffer
(src/backend/renderer/utils/wayland.rs) drops the cached textures and marks
the whole buffer damaged, so a surface whose image description or colour
representation changed on commit repaints without new client damage, as
Mutter redraws an actor whose colour state changed. The isolated diff is
retained in COLOR-DAMAGE-API-PATCH.diff.

The same issue adds EGL YCbCr conversion hints at dmabuf import
(src/backend/egl, src/backend/renderer/gles). EGLDisplay::create_image_from_dmabuf
takes an optional YuvConversion (EGL_YUV_COLOR_SPACE_HINT_EXT and
EGL_SAMPLE_RANGE_HINT_EXT, BT.601/709/2020 at full or narrow range), a
YuvConversionHint in a surface's data_map selects it for that surface's
buffers, and the GLES renderer's dmabuf cache re-imports a buffer when its
conversion changes. Without a hint the driver default applies, as before,
only for ordinary imports; identity YCbCr never uses that converted result.
The isolated diff is retained in YUV-CONVERSION-API-PATCH.diff; the pinned
version stays 0.7.0.

Identity YCbCr additionally uses the isolated RAW-YUV-PLANES-API-PATCH.diff:
Dmabuf::flags exposes interlacing/orientation metadata,
GlesRenderer::import_dmabuf_raw imports a single RGB component view only
when EGL advertises non-external sampling for its exact format/modifier,
and GlesFrame::with_texture_bindings holds the additional textures' read
locks through the draw, waits for uploads, retains texture lifetimes and
uses the existing per-frame/shared-context read-fence policy. It allocates
no per-draw binding/lock vectors. EGL/GL view-import errors are returned,
never retried through a driver YUV conversion. These APIs are independently
implemented Smithay plumbing under the retained MIT license, not copied
Mutter shader code. The pinned version remains 0.7.0.

The compositor builds zero-copy views using duplicated dma-buf descriptors,
original plane offsets/strides and explicit linear layout; it never copies
production pixels to the CPU. Representable layouts are NV12/NV21,
NV16/NV61, NV24/NV42, P010/P012/P016/P210, 8-bit three-plane YUV/YVU
410/411/420/422/444, AYUV/XYUV8888 and packed YUYV/YVYU/UYVY/VYUY.
R16/GR1616 views preserve normalized high-bit-depth samples as Mutter does,
provided the EGL driver actually advertises and accepts those views.
Opaque/implicit modifiers, auxiliary compression planes, interlacing,
unsupported packed/tiled layouts, and unsupported raw RGB views are
refused with a representation pixel-format error and are not drawn through
a guessed default conversion. Explicitly linear, subsampling-aligned sizes
are required; this deliberately does not claim every advertised original
YUV modifier is separately importable as RGB component planes.

Layout/source evidence is Mutter 51.0 commit
138a14fbeef09d49ebf5be8a0cb83b042dd5c841 from the official GNOME GitLab
repository: src/compositor/meta-multi-texture-format.c:34-43,346-608 and
src/common/meta-cogl-drm-formats.h:72-75. The former is GPL-2.0-or-later,
Copyright 2018 Barco NV, authored by Niels De Graef. Compositor shader/layout
sources retain that attribution and select GPL-3.0-or-later; the upstream
identity-limited shader's unmatched parentheses are not reproduced.
Limited identity expands each distinct raw Y/U/V channel independently as
`(sample - 16/255) * (255/219)` before alpha and colour transforms.

GPU regressions in color_render/raw_tests.rs cover every sampling layout,
distinct out-of-neutral channels, full/limited boundaries and transparent
AYUV. An opt-in real dma-buf qualification test uses Linux DMA-heap
allocation plus DMA_BUF_IOCTL_SYNC and hardware GBM/EGL imports:
set TUNA_COLOR_DMABUF_HEAP to an accessible DMA heap and
TUNA_COLOR_DRM_RENDER_NODE to the intended DRM render node, then run
real_dmabuf_identity_component_import_qualification with the drm feature.
Explicit qualification fails if any required raw view cannot be imported;
without those environment variables it reports a skip, not qualification.
No execution of these added regressions is asserted by this patch record.

The independent native producer example tuna-color-yuv-client exercises
the complete wl_buffer import and colour-representation commit path with
real DMA-heap storage. It supports NV12/P010/AYUV, identity and
BT.601/709/2020 at both ranges, and an unset-coefficients BT.709-limited
control. It refuses an original YUV format not advertised with explicit
LINEAR; no fallback to SHM, memfd or another coefficient matrix exists.
The READY line contains independently calculated expected RGB, not a
producer-side rendering result. The capture proof's external-yuv mode can
consume it as --color-client target/debug/examples/tuna-color-yuv-client;
select formats/coefficients through TUNA_COLOR_YUV_FORMAT and
TUNA_COLOR_YUV_COEFFICIENTS (or the example's --format/--coeff options).

A concrete headless positive-qualification invocation is:
`TUNA_COLOR_DMABUF_HEAP=/dev/dma_heap/system TUNA_COLOR_DRM_RENDER_NODE=/dev/dri/renderD128 cargo test -p tuna-compositor --features drm real_dmabuf_identity_component_import_qualification -- --nocapture`.
The selected GBM device needs no connected monitor. A primary card such as
/dev/dri/card0 can be supplied instead of a render node; the code does not
require a DRM render-node type. However vkms/swrast alone does not establish
positive YUV capability: the active EGL driver must advertise and accept
the original YUV format and its non-external RGB component views, and the
kernel must expose an accessible real DMA heap. Missing capabilities are
reported failures/skips, not a successful pixel qualification. A native
compositor using that device can run the producer with, for example,
`TUNA_COLOR_DMABUF_HEAP=/dev/dma_heap/system TUNA_COLOR_YUV_FORMAT=p010 TUNA_COLOR_YUV_COEFFICIENTS=identity-limited target/debug/examples/tuna-color-yuv-client`;
the consumer must compare actual scanout/captured pixels to READY.

Native FP16 composition also requires the default Smithay copy shader to
preserve extended-range texture samples. The isolated
FP16-SAMPLER-PRECISION-PATCH.diff changes only the sampler2D and
samplerExternalOES precision qualifiers in
src/backend/renderer/gles/shaders/implicit/texture.frag to highp where
GL_FRAGMENT_PRECISION_HIGH is available. GLES2 implementations without
fragment highp retain the original SDR sampler declarations; native HDR
admission separately rejects insufficient fragment precision.
This independently implemented MIT-licensed shader qualifier change does
not change ordinary normalized SDR sampling or the pinned version.

## Native HDR connector transactions

`NATIVE-HDR-API-PATCH.diff` extends the existing atomic DRM surface, rather
than issuing independent HDR property writes. `ConnectorColorState` is
staged in pending state, included in the real-framebuffer atomic test and
modesetting commit, and promoted to current state only on success. It
programs discovered `Colorspace`, `max bpc` and `HDR_OUTPUT_METADATA`
properties together; blob ID zero clears HDR on the SDR transition.
Callers retain metadata blob ownership until both states release it.

`GbmBufferedSurface::test_next_buffer` tests the actual negotiated GBM
framebuffer, including ten-bit format/modifier allocation. Buffer resets
do not pretend to renegotiate precision: the compositor keeps a negotiated
ten-bit swapchain for both SDR and HDR on capable connectors. Mode-change
rollback retains the old committed mode blob until the real atomic commit
succeeds and releases superseded pending mode blobs.

The implementation reuses this vendored Smithay backend. Behavioral source
references are official Mutter 51.0 commit
`138a14fbeef09d49ebf5be8a0cb83b042dd5c841`:
`meta-output-kms.c:547-600`, `meta-output.c:536-575` and
`meta-kms-impl-device-atomic.c:229-292`. This is a local patch, not a claim
of upstream acceptance or physical HDR display qualification. The
compositor's `color_output` regression exercises real EGL FP16 blending,
ten-bit PQ rendering and pixel readback; `TUNA_REQUIRE_COLOR_GL=1` makes
missing GPU capabilities fail the selected qualification lane.

VT activation re-reads each surface's current KMS state, invalidating the
cached connector encoding even if the EDID and property capabilities are
unchanged. The next framebuffer therefore commits the retained pending
HDR properties instead of issuing a plain page flip with another VT's
signaling. Replaced current-state mode blobs are released unless pending
still owns them.

Discovery independently probes actual EGL renderability for every
negotiated GBM target. A failed ten-bit candidate is destroyed before
creating and testing a fresh eight-bit surface; disabling HDR never
retains a known-unrenderable target as an SDR fallback. FP16/shader
qualification remains a separate gate. Hotplug teardown also releases
the CRTC calibration gamma/CTM blobs using an independently retained DRM
FD. Focused regressions cover candidate release/fallback, connector
encoding state/request transitions and exactly-once calibration cleanup;
no execution is asserted here.
