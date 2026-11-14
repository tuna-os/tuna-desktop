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

Issue #729 changes src/wayland/session_lock/lock.rs and mod.rs: the
ext_session_lock_v1 duplicate_output check now tracks outputs per lock
object (SessionLockState) instead of in a manager-wide list that only
unlock_and_destroy cleared. Upstream's list outlived a refused or
superseded lock, so when the shell, which creates its lock surfaces before
Locked as GTK does, locked again with the same wl_output, the new lock
surface was a duplicate_output protocol error that disconnected the whole
shell; the list also grew without bound for any client. The protocol
defines the error per lock object ("more than one lock surface for a given
output"), which is still enforced. Real-client tests in
crates/compositor/tests/session_lock_protocol.rs cover the refused retry,
the superseded pending lock and the per-lock duplicate. The isolated diff
is retained in SESSION-LOCK-OUTPUTS-PATCH.diff; SessionLockManagerState
loses its crate-private field; the pinned version stays 0.7.0.

Issue #1033 changes src/wayland/selection/source.rs: SelectionSource,
the public handle SelectionHandler::new_selection receives, gains an
IsAlive implementation that forwards to its provider. Mutter's clipboard
manager (meta-clipboard-manager.c) takes the clipboard over with a saved
copy when the owning client goes away, so the compositor must notice a
dead clipboard owner. Upstream only drops a dead source lazily, the next
time seat_data.rs send_selection runs, and tells no handler; a client
that destroys its source or disconnects therefore left no trace. The
compositor keeps the SelectionSource it was handed and checks it after
each client dispatch. Real socketpair tests in
crates/compositor/tests/data_device.rs cover a disconnecting owner and a
destroyed source. The isolated diff is retained in
CLIPBOARD-SOURCE-ALIVE-PATCH.diff; the pinned version stays 0.7.0.

Issue #203 changes src/wayland/compositor/handlers.rs: wl_region add and
subtract with an empty, negative or overflowing rectangle, and
wl_surface.damage with a negative size (damage_buffer already returned
early), panicked in
`Size::new` and took the whole session down (found by the nested stress
suite's protocol fuzzer). Mutter hands the rectangle to pixman
(meta-wayland-region.c, mtk_region_union_rectangle), which leaves the
region unchanged for such a rectangle; the handler now ignores it the same
way. The stress suite's `bad-region` adversary covers it. The isolated diff
is retained in REGION-RECT-PATCH.diff; the pinned
version stays 0.7.0.

Issue #203 also changes src/wayland/shm/handlers.rs: wl_shm_pool.resize
with a size of zero or less posted the invalid-size error and then went on
to `NonZeroUsize::try_from(size).unwrap()`, which panicked and took the
whole session down (found by the deeper, error-aware protocol fuzzer).
The handler now returns after the error, as libwayland-server's
shm_pool_resize does. The isolated diff is retained in
SHM-RESIZE-PATCH.diff; the pinned version stays 0.7.0.

Issue #203 also changes src/wayland/shell/xdg/handlers/surface.rs,
surface/toplevel.rs and positioner.rs, all found by the deeper protocol
fuzzer:

- xdg_surface.get_popup accepted any parent, including the popup's own
  xdg_surface or a second xdg_surface on a surface that already was a
  popup. The popup then descended from itself and
  `find_popup_root_surface` (and every other walk up the popup chain)
  looped forever, hanging the whole session. As Mutter's
  xdg_surface_constructor_get_popup (meta-wayland-xdg-shell.c), a parent
  without an xdg role is refused with `invalid_popup_parent`; so is one
  that descends from the new popup.
- xdg_toplevel.set_min_size and set_max_size with a negative size panicked
  in `Size::new`. They now post `invalid_size`, as Mutter's
  xdg_toplevel_set_min_size and xdg_toplevel_set_max_size do.
- xdg_positioner.set_parent_size with a negative size panicked the same
  way. Mutter stores any parent size without an error; a negative one
  cannot be a `Size`, so it is ignored.

crates/compositor/tests/popups.rs covers the popup parent checks. The
isolated diff is retained in XDG-SHELL-FUZZ-PATCH.diff; the pinned version
stays 0.7.0.

Issue #203 also changes src/wayland/compositor/tree.rs: placing a
subsurface above itself removed it from its parent's child list, then
inserted past the end and panicked. Both above-self and below-self now
return an error before changing the list, which the request handler turns
into `wl_subsurface.bad_surface`. This matches Mutter's `is_sibling`
and `subsurface_place` in meta-wayland-subsurface.c. A missing child in
the parent's list likewise returns an error instead of unwrapping.
The real-client regression in crates/compositor/tests/popups.rs covers
both requests and confirms another client is still served afterward.

Issue #1034 changes src/xwayland/xwm/mod.rs and surface.rs for server-side
frames of X11 windows, with the X11 protocol of Mutter 51's frames client
(mutter src/x11/meta-x11-frame.c, events.c, window-x11.c; src/frames/). The
frame handling is additive: without a frames client, and for a compositor
that never calls set_needs_frame, a window is wrapped, mapped, configured
and unmapped as before, with two corrections found while proving the frames
against a live Xwayland. The bare wrapper is created with the client's depth
and visual (create_wrapper; a window of another depth than the root gets
the client's colormap and a border pixel): in a wrapper of the root's depth
the X server composited a 32-bit client without alpha, so a client-decorated
GTK window showed an opaque black box where its shadow is transparent. And
X11Wm::set_xsettings flushes: the changed property used to wait in the
request buffer for the next unrelated request. New atoms:
_MUTTER_NEEDS_FRAME, _MUTTER_FRAME_FOR, _MUTTER_FRAME_EXTENTS,
_GTK_FRAME_EXTENTS, _NET_FRAME_EXTENTS,
_GTK_SHOW_WINDOW_MENU and _NET_CLOSE_WINDOW; _NET_SUPPORTED lists the last
four of them, as Mutter's set_supported_hint does, because GTK draws
client-side shadows only with _GTK_FRAME_EXTENTS supported and asks for the
window menu only with _GTK_SHOW_WINDOW_MENU supported.
X11Surface::set_needs_frame is meta_window_ensure_frame and
meta_window_destroy_frame. A MapRequest for a window whose _MUTTER_FRAME_FOR
names a managed window that wants a frame adopts it
(meta_window_x11_set_frame_xwindow): the client is reparented from the bare
wrapper into the frame at the extents offset, the wrapper is destroyed, and
the frame becomes mapped_window_id, so WL_SURFACE_SERIAL on the frame
associates the Wayland surface. Such a window is never passed to
map_window_request; its own handle from new_window is retired through
destroyed_window. The frame's borders are read as meta_frame_query_borders
reads them and re-read on PropertyNotify; X11Surface::frame_extents returns
them (or, without a frame, the window's own _GTK_FRAME_EXTENTS as invisible
borders), with the Mutter rectangle conversions on the new FrameExtents and
FrameBorder types. X11Surface::configure and geometry keep meaning the
window that carries the surface; under a frame the client is placed inside
it at the extents offset, _NET_FRAME_EXTENTS is published on the client, and
_NET_WM_STATE is mirrored onto the frame. A client keeps its size when a
frame is adopted or lost: the buffer rectangle grows or shrinks by the
borders around the same frame rectangle origin
(FrameExtents::bare_rect_to_buffer_rect, buffer_rect_to_bare_rect), as
Mutter grows the frame around the client; a change of borders before the
map was granted does the same, and afterwards keeps the buffer rectangle
until the compositor lays the window out. A managed client is always mapped
inside what it is mapped onto (adopt_frame and unframe map it after the
reparent): the save-set's own MapWindow for a client handed back to the
root is redirected to this window manager as a MapRequest for a window it
already manages, and the client would stay unmapped in its new wrapper and
in its next frame. The frame's own ConfigureRequest is answered with a
synthetic ConfigureNotify. _NET_WM_STATE, WM_CHANGE_STATE
and _NET_WM_MOVERESIZE messages are accepted for the client or its frame
(X11Surface::is_message_target); XwmHandler gains close_request
(_NET_CLOSE_WINDOW), show_window_menu_request (_GTK_SHOW_WINDOW_MENU) and
frame_changed, each with a default body. Reparents by the window manager
are told from withdrawals by an unmaps_pending count, as in Mutter. Two
things differ from Mutter because an unframed window lives in a wrapper
here, not on the root: when the frames client dies and the X server's
save-set returns a client to the root, the window is wrapped bare again in
place (Mutter unmanages and re-manages it), and X11Wm::request_frames
re-announces unframed windows to a frames client that started after they
were wrapped (_MUTTER_NEEDS_FRAME again plus a synthetic root
ConfigureNotify, which the tracker treats like a configured root child).
The frame keeps the wrapper's event mask (SubstructureNotify,
SubstructureRedirect, PropertyChange) instead of Mutter's EVENT_MASK, since
structure events of root children already arrive through the root.
_NET_WM_ALLOWED_ACTIONS and _NET_WM_SYNC_REQUEST_COUNTER on the frame are
not handled. Pure tests of the border arithmetic, the property parsing and
the frame-id message predicate are in crates/compositor/src/xwayland.rs
(frame_tests). The handshake itself needs a live X server and frames
client; no cargo test covers it. The nested session's X11 frame gates do
(scripts/tuna-gtk-shell-proof G-X11-FRAME, G-X11-FRAME-ACTIONS,
G-X11-FRAME-CSD, G-X11-FRAME-RECOVERY, G-X11-RESTART), against a real
Xwayland and tuna-x11-frames. The isolated diff is retained in
X11-FRAMES-API-PATCH.diff; the pinned version stays 0.7.0.

Issue #1084 changes src/backend/renderer/utils/wayland.rs: the additive
RendererSurfaceState::release_imported_shm_buffer sends wl_buffer.release
for the attached wl_shm buffer once the given renderer context has
imported it, instead of when another buffer replaces it. Mutter copies
SHM buffers when it applies a commit and never holds them
(meta-wayland-surface.c, buffer_held), and GTK's cairo renderer before
4.22 depends on that: when its surface changes size it forgets the buffers
still attached and never frees them on a late release (GTK 8a39f8e6), so
every resize of a cairo-drawn surface leaked one shm pool in the client and
its descriptor in the compositor. The buffer stays attached (the surface
stays mapped and draws from its texture); a per-buffer flag keeps the
release from being sent twice and is cleared when the same buffer is
attached again, so each commit gets one release. Existing APIs and the
drop-time release of buffers nobody imported are unchanged. Roost calls it
for every committed surface tree right after importing it with its only
renderer (crates/compositor/src/buffer_upload.rs); real socketpair tests in
crates/compositor/tests/buffer_release.rs cover a resized layer surface,
re-attached and replaced buffers, a destroyed surface, and a toplevel with
a subsurface. The isolated diff is retained in SHM-EARLY-RELEASE-PATCH.diff;
the pinned version stays 0.7.0.

Issue #364 additionally changes `src/wayland/input_method/{mod.rs,
input_method_handle.rs,input_method_keyboard_grab.rs}`. Registry visibility
and forced binds both enforce the compositor's input-method admission
filter. A seat belongs to one exact live input-method resource, not to all
resources sharing its connection. Unavailable resources ignore requests;
their popup/grab NewIDs are initialized inertly without assigning roles,
installing grabs or emitting events, and destroying them cannot retire
the real owner or its grab. `tests/input_protocols.rs` uses actual Wayland
connections for admitted/public clients, same-connection duplicates,
replacement, child-resource teardown, spoofed application metadata and a
copied native client executable in a separate process. The copy is the
protocol test executable, not an installed IBus package qualification.

The focused source-consumer command is `cargo test --locked --offline -p
tuna-compositor --test input_protocols --test protocols`, using the normal
native features. These protocol/socket tests do not qualify an installed
package, image, supervised production IBus process or graphical IME journey;
those still require the installed native OSK/IBus producer and exact binary
and image identities. A rejection can be reported by libwayland as a fatal
I/O error for `wl_display.invalid_object` (code zero), so the negative
consumers assert terminal connection failure, not only `ProtocolError`.

Destroying any input-method parent also destroys its associated popup/grab
resources, including inert child IDs. Active-owner retirement detaches its
popup and grab before any callback, without holding IM or grab locks across
callouts. Keyboard-grab `unset` clears its recorded ownership, and teardown
checks the installed grab's serial before releasing it: a compositor-owned
replacement or a new input method must not be canceled by old child cleanup.
The native consumers retain child proxies across parent destruction, check
the actual server objects are gone, and exercise both parent-first and
child-first destruction, continued compositor-grab key delivery and a fresh
input method's typing/grab after retirement.

Trusted virtual-keyboard returned keys must not temporarily unset and
reinstall an input-method grab: `unset` now really retires its resource
ownership. `KeyboardHandle::input_forward_to_focus` preserves ordinary
forwarded-key and modifier bookkeeping while delivering directly through the
normal focus/keymap path. Only an active input-method grab takes that route;
compositor-owned grabs keep normal dispatch. The native returned-key consumer
forwards the actual IM keymap FD, returns press/release events over the
trusted virtual keyboard, then exercises further physical input, IM typing
and a compositor-owned grab without loopback or lost ownership.

Permanent grab replacement also sends the retiring live IM grab
`repeat_info(0, 0)` after releasing its state lock. Returned-key forwarding
does not send a pause or change the live repeat rate. The consumer checks
the actual pause event and that a later keyboard repeat-settings change
does not rearm the retired grab. This baseline has no IM weak repeat
registration; integrations which add one must clear only the retiring
resource's registration, never a replacement's.

A standard `grab_keyboard` request from the admitted current IM also
rechecks actual seat ownership: it cannot replace a non-IM compositor grab.
Such a late request receives a tracked inert child without metadata; its
release and parent teardown cannot unset the compositor owner. The returned-
key consumer exercises this race with actual standard requests and verifies
continued physical-key delivery to the compositor grab.

Issue #1088 changes src/wayland/shell/xdg/handlers/surface/toplevel.rs:
the xdg_toplevel's destructor now also drops the xdg_dialog_v1 handle its
user data keeps, as it already drops the decoration's. The dialog object's
own user data is the ToplevelSurface (the xdg_toplevel and its
wl_surface), so the two held each other and only xdg_dialog_v1.destroy
broke the cycle. GTK 4 creates an xdg_dialog_v1 for every toplevel, and an
app that exits (or is killed) just disconnects: every such window's
wl_surface, its role data and the textures its frames were uploaded to
stayed allocated for the rest of the session, about 1.65 MB per closed
libadwaita window. Upstream master has the same cycle. The dialog
object itself stays usable until its client destroys it (its requests
only touch the surface's role data, which outlives the role). Real
socketpair tests in crates/compositor/tests/window_teardown.rs close
windows with and without an xdg_dialog_v1 by destroying every object and
by disconnecting, and check that each surface's server-side data is
dropped. The isolated diff is retained in XDG-DIALOG-CYCLE-PATCH.diff; the
pinned version stays 0.7.0.

Issue #503 additionally changes GlesFrame::render_texture_from_to in
src/backend/renderer/gles/mod.rs: a texture drawn texel for pixel (the
source size, turned by the buffer transform, equals the destination size,
and the source starts at a whole texel) is sampled with GL_NEAREST for that
draw, whatever filters the renderer is set to. Every sample of such a draw
lands on a texel centre, so nearest and linear name the same texel, and
nearest does not fetch and weigh the three texels around it: on llvmpipe
the four gathers of a bilinear fetch are most of the fragment work, and
nearly everything a desktop draws outside an animation is unscaled. Mutter
does the same for unscaled shaped textures and backgrounds
(meta-shaped-texture.c, meta-background-content.c). The renderer's
configured filters are restored after the draw; scaled, stretched or
sub-texel-shifted draws keep them. GL tests in unscaled_nearest_tests
(surfaceless Mesa EGL) read the filter each draw leaves on the texture and
the pixels it produces: an unscaled, a cropped and a quarter-turned draw
are nearest and show only source texels, while shrunk, grown, one-axis
stretched and half-texel-shifted draws stay linear and filtered. The
isolated diff is retained in UNSCALED-NEAREST-PATCH.diff; the pinned
version stays 0.7.0.
