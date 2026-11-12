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

Issue #364 changes src/wayland/shell/xdg/mod.rs: xdg_wm_base is created at
version 7, as Mutter 51 advertises it (META_XDG_WM_BASE_VERSION). Version 7
adds only the four xdg_toplevel constrained_* states. A configure keeps
every state a client's bind can name and drops the rest: tiled states below
version 2, suspended below 6 and constrained below 7, the same version
gates as Mutter's fill_states (meta-wayland-xdg-shell.c). Roost sets the
states from Mutter's edge constraints. Real-client tests in
crates/compositor/tests/windows.rs and
crates/compositor/tests/support/xdg_version_boundaries.rs bind versions 7 and 6
and consume actual configure arrays through layout changes, repeated requests,
simultaneous binds and teardown/replacement.

The same patch validates the locked XML's required request/error boundaries in
handlers/surface.rs and handlers/surface/toplevel.rs. Nonpositive window
geometry reports xdg_surface.InvalidSize; negative min/max hints report
xdg_toplevel.InvalidSize before constructing a Size (the #203 check above,
so that hunk lives in XDG-SHELL-FUZZ-PATCH.diff). Final double-buffered
min/max relations are checked at commit, not eagerly between valid requests;
zero maxima remain unbounded. Unknown, consumed or stale configure serials
report xdg_surface.InvalidSerial on the actual surface. The pinned Mutter
source instead warns/ignores invalid geometry and handles ACKs differently;
these XML-required errors are not claimed to match that implementation.
Socket consumers exercise the exact error target/code and valid double-buffered
changes, rather than constructing expected enum sets. The isolated diff,
applied after XDG-SHELL-FUZZ-PATCH.diff, is retained in
XDG-WM-BASE-7-PATCH.diff; the pinned version stays 0.7.0.

Issue #364 additionally changes src/wayland/xdg_foreign/mod.rs and
handlers.rs: zxdg_exporter_v1 and zxdg_importer_v1 join the v2 globals,
sharing one handle table, as Mutter's meta-wayland-legacy-xdg-foreign.c
calls the same export and import functions as v2. GTK 3 exports with v1 and
the portal imports with v2, so each version imports the other's handles.
Each import now parents at most one child (Mutter's imported->parent_of;
upstream kept one child per export, so a second importer replaced the
first's relationship), and revoking an export sends destroyed to every
import and clears the parents they set, as meta_wayland_xdg_exported_destroy
does; upstream only cleared the parent. An import of an unknown handle is
destroyed at once, as before. An invalid v1 parent posts error 0 on the
import, Mutter's WL_DISPLAY_ERROR_INVALID_OBJECT, since zxdg_imported_v1 has
no error enum. Real socketpair tests in
crates/compositor/tests/protocol_gaps.rs cover v1 export with v2 import, v2
export with v1 import, revocation and unknown handles. The isolated diff is
retained in XDG-FOREIGN-V1-PATCH.diff; the pinned version stays 0.7.0.

Issues #364 and #588 change src/wayland/drm_lease/mod.rs, with a crate-private
accessor in src/backend/drm/device/mod.rs and a crate-private re-export in
src/backend/drm/mod.rs, for wp_drm_lease_device_v1 as Mutter 51 offers it
(meta-wayland-drm-lease.c, meta-drm-lease.c). DrmLeaseBuilder is now Clone
and shares its device's plane claims: a compositor keeps an empty builder and
claims planes on a clone (DrmLeaseBuilder::claim_plane) when a request
arrives, as the lease handler has no access to the DrmDevice. The additive
DrmLeaseState::revoke_lease ends a lease from the compositor's side (finished,
connectors offered again), for Mutter's update_leases (a lessee the kernel
destroyed because its client closed the lease fd) and update_connectors (a
leased connector unplugged). Request validation follows
wp_drm_lease_request_request_connector and wp_drm_lease_request_submit: a
connector of another lease device posts wrong_device and a repeated one
duplicate_connector when it is requested (upstream checked only at submit
and posted wrong_device for a withdrawn connector); an empty submit posts
empty_lease and stops (upstream went on to call the handler); a connector
withdrawn since it was requested fails the lease with finished. A binding
that cannot open a non-master DRM fd stays pending without events, as
Mutter's lease_device_bind keeps it, until
DrmLeaseState::retry_pending_binds (session activation); upstream sent the
released destructor from inside the bind, which made wayland-backend panic
on the destroyed object, and refused to create the global when the
compositor could not open the node, where Mutter advertises it anyway.
drm-smoke's D-LEASE leases the vkms connector through it with a real
client. The isolated diff is retained in DRM-LEASE-PATCH.diff; the pinned
version stays 0.7.0.

Connector withdrawal only retires its public offer, never the active kernel
lease reference. A failed revoke therefore keeps its resource ownership across
unplug/reappearance and session resume; newly reappearing connectors stay
disabled until revocation succeeds. The compositor withdraws disappeared offers
before revoking their leases, avoiding a transient re-offer during unplug.
Live connector announcements use the same metadata and connector `done`
boundary as bind/resume. The real D-LEASE client rejects device `done` before
every live connector has completed its own metadata.

Issues #364 and #588 additionally change
src/wayland/selection/data_device/mod.rs and dnd_grab.rs for
xdg_toplevel_drag_v1, which Roost implements outside Smithay
(crates/compositor/src/toplevel_drag.rs). Mutter's drag_grab_release
(meta-wayland-data-device.c) reports dnd_drop_performed to the source of a
drag that moves a toplevel before it resolves the drop, then cancels the
source as usual when no target accepted; upstream reports dnd_drop_performed
only after an accepted drop. The additive
ClientDndGrabHandler::drop_performed_without_target hook, asked once on a
release that was not cancelled while the source lives, lets the handler opt a
drag in: the source then hears dnd_drop_performed before the target's drop,
exactly once, and cancelled afterwards if nothing accepted. The default
returns false, keeping upstream behaviour for every other drag; Escape
cancellation never reports a drop. Real socketpair tests in
crates/compositor/tests/protocol_gaps.rs cover an unaccepted and an accepted
toplevel drag drop. The isolated diff is retained in
XDG-TOPLEVEL-DRAG-PATCH.diff; the pinned version stays 0.7.0.

Issues #364 and #588 change src/wayland/drm_syncobj/mod.rs: a commit on a
surface with a wp_linux_drm_syncobj_surface_v1 is checked as Mutter 51's
meta_wayland_surface_explicit_sync_validate (meta-wayland-linux-drm-syncobj.c)
does, and as the protocol's "if and only if" requires: a non-dmabuf buffer
is unsupported_buffer whether or not points are set; a dmabuf without an
acquire point is no_acquire_point (upstream let it through), then
no_release_point; on one timeline acquire must be below release
(conflicting_points); a point without a buffer is no_buffer (upstream
called a lone release point no_acquire_point). Only the first error is
posted. The check is the public pure function commit_error, with
pending_commit_error reading it from a surface's pending state, so Roost's
acquire-point pre-commit hook skips commits the check refuses. Unit tests in
crates/compositor/src/explicit_sync.rs cover every case; real socketpair
tests in crates/compositor/tests/protocol_gaps.rs cover surface_exists,
invalid_timeline, unsupported_buffer and no_acquire_point. The isolated
diff is retained in DRM-SYNCOBJ-PATCH.diff; the pinned version stays 0.7.0.

Issues #364 and #89 change src/xwayland/xwm/mod.rs and surface.rs: the XWM
interns `_XWAYLAND_MAY_GRAB_KEYBOARD` and records a client message of that
type for a known window (data.l[0] non-zero allows, zero revokes), read with
the additive `X11Surface::xwayland_may_grab_keyboard`. This is Mutter 51's
`xwayland-may-grab-keyboard` window property, set from the same client
message in src/x11/events.c; `meta_xwayland_grab_is_granted` grants a
zwp_xwayland_keyboard_grab_v1 to such a window unless a deny rule matches.
Smithay's xwayland_keyboard_grab module is used unchanged (its global is
already visible to the Xwayland client only); Roost overrides its grab
handler. The real Xwayland test in crates/compositor/src/xwayland_grab.rs
sends the message from an X11 client. The isolated diff is retained in
XWAYLAND-MAY-GRAB-PATCH.diff; the pinned version stays 0.7.0.

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
