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

Issue #353 changes Cargo.toml (the xwayland feature enables x11rb's xkb
protocol) and src/xwayland/xwm/mod.rs, additively. X11Wm::select_bell_events
does what Mutter 51's init_x11_bell does (src/x11/meta-x11-display.c): select
XkbBellNotify on the core keyboard and turn off its AudibleBell control, so
X11 bells reach the new XwmHandler::bell callback (default: ignore) with the
window the client named and the X server time, and Xwayland never rings them
itself. X11Wm::release_xsettings_selection gives up the _XSETTINGS_S0
selection start_wm takes: Mutter never owns XSETTINGS, and
gnome-settings-daemon's xsettings plugin exits while another manager holds
it. The isolated diff is retained in XKB-BELL-XSETTINGS-API-PATCH.diff; the
pinned version stays 0.7.0.
