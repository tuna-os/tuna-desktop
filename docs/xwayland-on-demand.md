# XWayland socket activation

The compositor reserves a display lock and both pathname/abstract X11 listening
sockets when X11 compatibility is enabled. It advertises DISPLAY to applications
before starting a process. Native Wayland startup remains idle. Read readiness
on a listening socket records the first X11 need without accepting its queued
connection; the exact socket owner is handed to Smithay to start XWayland.

The version remains Smithay 0.7.0, with additive APIs documented in
third-party/smithay/ROOST-PATCH.md. Internal XWaylandClientData and process
construction stay within Smithay. No fork or new upstream version is used.

Advertised display and actual X11 window-manager readiness are separate. Helpers
that connect to X11, including icon readers, must wait for window-manager
readiness instead of treating DISPLAY advertisement as permission to connect.
The native IBus bridge remains available from startup. Its XIM helper is a
separate X11 client: it starts only after the window manager is ready and is
stopped and reaped on server loss before a fresh helper can start. Native bridge,
address probe and supervised shell discard inherited IBUS_ADDRESS; only the XIM
helper receives the address discovered for the private Wayland-named bus.

The frames client (`tuna-x11-frames`, issue #1034) is the other supervised X11
client. Mutter 51 draws no X11 title bars itself: `mutter-x11-frames` creates a
GTK frame window for each decorated X11 window and Mutter reparents the window
into it. Tuna follows the same protocol (`_MUTTER_NEEDS_FRAME` on the window,
`_MUTTER_FRAME_FOR` and `_MUTTER_FRAME_EXTENTS` on the frame, the frame's
`_GTK_FRAME_EXTENTS` for its shadow, `_NET_FRAME_EXTENTS` back on the window);
the window-manager half is an additive patch to the vendored Smithay
(`X11-FRAMES-API-PATCH.diff`). Lifecycle, in `crates/compositor/src/x11_frames.rs`:

- It starts only after the window manager is ready, with `DISPLAY` naming the
  server and `GDK_BACKEND=x11`, so native startup and the idle reserved sockets
  still own no extra process. The binary is `tuna-x11-frames` beside the
  compositor, else on `PATH`. `TUNA_X11_FRAMES` names another client (the stock
  `mutter-x11-frames` speaks the same protocol and serves as the oracle); an
  empty value or `0` runs none.
- An exit restarts it after 500 ms, doubling to 5 s at most, three times; a
  client that ran for a minute gets that budget back. On server loss it is
  killed and reaped before a fresh one starts for the next server.
- With no frames client (not installed, turned off, budget spent) X11 windows
  map undecorated, exactly as before. While one runs, a window that wants a
  frame maps once the frame arrived, or bare after 1.5 s without one, and is
  framed when a frame arrives later.
- A frames client that dies leaves its windows mapped: the X server returns
  them to the root window, the window manager wraps them bare where they
  were (and maps them there: the X server's own attempt arrives as a
  `MapRequest` for a window that is already managed), and offers them to the
  restarted client.
- A window keeps its client size when a frame comes or goes. The frame is put
  around it with the title bar above, at the same window origin; without the
  frame the window is shorter by the title bar.
- Smithay keeps an unframed window in a wrapper, not on the root window where
  the frames client looks when it starts. While the client starts (10 s) and
  while a window waits, the window manager re-announces such windows every
  100 ms (`X11Wm::request_frames`): `_MUTTER_NEEDS_FRAME` again and a synthetic
  `ConfigureNotify` on the root, which the client's window tracker answers as
  it would for a newly configured root child.

The window manager now lists `_GTK_FRAME_EXTENTS` and `_GTK_SHOW_WINDOW_MENU`
in `_NET_SUPPORTED`, as Mutter does. GTK X11 applications therefore draw their
own shadows and report them in `_GTK_FRAME_EXTENTS`; the compositor treats that
margin, like a frame's, as outside the window's rectangle. The title-bar keys
of `org.gnome.desktop.wm.preferences` (`button-layout` and the double, middle
and right click actions) are published on the window manager's XSETTINGS
selection as `Gtk/DecorationLayout`, `Gtk/TitlebarDoubleClick`,
`Gtk/TitlebarMiddleClick` and `Gtk/TitlebarRightClick`, translated as
gnome-settings-daemon does.

Three things a frame's header bar needs from the compositor, each as Mutter
does it:

- A drag or edge resize starts from where the press landed
  (`_NET_WM_MOVERESIZE` names that point; GTK sends it only after its drag
  threshold), so the window does not trail the pointer.
- The X11 window follows every move of its window, not only resizes: the
  header bar and X11 applications work in root coordinates.
- The pointer reaches a window's surface on the invisible border around it,
  where the surface's input region covers it: the resize handles GTK keeps
  in the shadow. This holds for every window, native ones too
  (`WindowManager::window_at`; `G-RESIZE-BORDER`); shadow outside the input
  region passes the pointer to what is beneath.
- Minimizing an X11 window sets `WM_STATE` to IconicState and adds
  `_NET_WM_STATE_HIDDEN` on the client and its frame, and restoring it
  undoes both (`meta_window_x11_set_wm_state`,
  `meta_window_x11_set_net_wm_state`). The window stays mapped: the overview
  and the switcher still draw it.

A window without a frame is wrapped in a window of its own depth and visual,
so a client-decorated X11 window keeps the alpha channel its shadow is drawn
in.

Override-redirect windows (menus, tooltips, dropdowns) are not managed and get
no frame. As Mutter does for its unmanaged windows, the compositor draws each
one where the X server has it (`crates/compositor/src/windows/unmanaged.rs`):
above every window and the top bar, below the shell's own menus and dialogs
(GNOME Shell's place for Mutter's top window group), with the window it
belongs to (`WM_TRANSIENT_FOR`, else the same client process), and not in the
overview or behind the lock screen. It takes the pointer by its input region
and nothing else: no keyboard focus, no entry in the window list, the
overview, the switcher or a workspace. Screenshots, screen casts and the
magnifier draw the same scene and so include it. An X11 pointer grab promises
its client every press; Xwayland hears only of presses on its own surfaces,
so while such a window shows and a client holds the grab
(`X11Wm::pointer_grabbed`), a press on a native window or the desktop is
handed to the X server instead and dismisses the menu, as on an X server.
Presses on the shell's own surfaces stay the shell's: a menu stays open
through a click on the top bar, as in GNOME.

`scripts/tuna-gtk-shell-proof` gates all of this in the nested session
(`G-X11-FRAME`, `G-X11-FRAME-ACTIONS`, `G-X11-FRAME-CSD`,
`G-X11-FRAME-RECOVERY`, `G-X11-MENU`, `G-X11-TOOLTIP`, and the frames
client's part of `G-X11-DEMAND` and `G-X11-RESTART`); `--x11-frames-only`
runs the frame, menu and tooltip gates alone. They read the X server with
`xprop` and `xwininfo`, the frames client's accessible tree, the output's
pixels, and the compositor's state file, where each X11 window carries
`x11_framed`, `x11_frame_extents` (left, right, top, bottom) and
`x11_buffer_rect` (the X11 window carrying the surface: `rect` plus the
invisible borders), every window `surface_rect` and `input_rect`, and
`x11_unmanaged` lists the override-redirect windows drawn.

The proof's session names a launcher of its own in `TUNA_X11_FRAMES`
(`x11f_launcher` in `scripts/lib/tuna-x11-frames-proof.sh`). It starts the
`tuna-x11-frames` the supervisor would have started, as the same process.
`G-X11-FRAME-RECOVERY` holds that start after it killed the frames client,
until it has read and clicked the bare windows, so it does not have to catch
them within the 500 ms restart delay. The hold ends with the click: the window
manager offers its windows to a starting frames client for ten seconds only.

The proof's session names a launcher of its own in `TUNA_X11_FRAMES`
(`x11f_launcher` in `scripts/lib/tuna-x11-frames-proof.sh`). It starts the
`tuna-x11-frames` the supervisor would have started, as the same process.
`G-X11-FRAME-RECOVERY` holds that start after it killed the frames client,
until it has read and clicked the bare windows, so it does not have to catch
them within the 500 ms restart delay. The hold ends with the click: the window
manager offers its windows to a starting frames client for ten seconds only.

On server/manager failure the compositor removes the old source, compatibility
windows and client, then retries on the same display with the existing bounded
restart budget (initial attempt plus three retries, 500 ms exponential backoff
capped at 5 s). Native windows keep their IDs. Graceful shutdown drops owned
listening sockets and display lock. Missing XWayland/socket preparation is
non-fatal to the native session.

The GTK proof requires native startup without an owned XWayland process,
first-client mapping through the reserved sockets, real process termination,
compatibility-window cleanup and same-display reconnect while native IDs stay
unchanged, then socket/lock cleanup at graceful shutdown. These gates passed
on candidate `3719d3f39a657e6f447c000cff94e23fec41d710` in
[CI 37192378020](https://github.com/tuna-os/tuna-desktop/actions/runs/37192378020),
along with package and Marlin VM checks; all 13 current-head checks succeeded
before merge. Physical GPU/VT behavior is separate.
