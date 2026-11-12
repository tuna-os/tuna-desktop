# tuna-x11-frames

Title bars for X11 windows.

X11 applications expect the window manager to draw their title bar.
GNOME does it with a helper process, `mutter-x11-frames`: an X11 GTK
program that makes one frame window per application window. The window
manager puts the application's window inside the frame. The title bar is
then part of the window's own surface (it moves, stacks, animates and is
captured with it) and it is a real GTK header bar.

`tuna-x11-frames` is that helper for Tuna Desktop. It is written after
Mutter 51's `src/frames/` and speaks the same X11 protocol, so the
window manager's side of the protocol is the one Mutter documents in
`src/x11/meta-x11-frame.c`.

## The protocol

| Property | On | Written by | Meaning |
|---|---|---|---|
| `_MUTTER_NEEDS_FRAME` (`CARDINAL`) | client window | window manager | This window wants a frame. Deleting it takes the frame away. |
| `_MUTTER_FRAME_FOR` (`WINDOW`) | frame window | frames client, before the frame is mapped | The client this frame is for. |
| `_MUTTER_FRAME_EXTENTS` (4 × `CARDINAL`: left, right, top, bottom) | frame window | frames client | Visible borders around the client, in pixels. In practice only top (the title bar) is non-zero. |
| `_GTK_FRAME_EXTENTS` (4 × `CARDINAL`) | frame window | GDK | Invisible borders: shadow and resize area. Zero when maximized or fullscreen. |

One window's life:

1. The window manager sets `_MUTTER_NEEDS_FRAME` on a client window.
2. The frames client creates a frame window, adds the client to its save
   set, sets `_MUTTER_FRAME_FOR` and `_MUTTER_FRAME_EXTENTS`, and maps
   the frame.
3. The window manager sees the map request of a window carrying
   `_MUTTER_FRAME_FOR`, reparents the client into it at the offset the
   two extents give, and maps both. From then on it sizes the frame
   window; the frames client never sizes it.
4. The frames client keeps the title, the close button and the
   resizable state in step with the client's properties.
5. The frame goes when the window manager deletes `_MUTTER_NEEDS_FRAME`
   (after reparenting the client out), or when the client is destroyed.

If the frames client dies, the X server returns every framed client to
the root window (the save set), and the window manager can ask a new
frames client for frames again.

## What the user's actions become

All of these are stock GTK behaviour on X11. Each request names the
**frame** window and goes to the window manager through the root window.

| Action | Request |
|---|---|
| Drag the title bar, drag an edge | `_NET_WM_MOVERESIZE` |
| Maximize button, double-click (per `Gtk/TitlebarDoubleClick`) | `_NET_WM_STATE` |
| Minimize button | `WM_CHANGE_STATE` |
| Right click, menu key | `_GTK_SHOW_WINDOW_MENU` |
| Close button | `WM_DELETE_WINDOW` sent to the client directly; a client that does not take part in that protocol is disconnected (`XKillClient`) |

The button layout is the XSETTING `Gtk/DecorationLayout` (GNOME's
`button-layout`).

## What it needs from the window manager

GTK decides how to behave from what the window manager advertises:

- `_NET_SUPPORTED` (with a valid `_NET_SUPPORTING_WM_CHECK`) listing
  `_GTK_FRAME_EXTENTS`, `_GTK_SHOW_WINDOW_MENU`, `_NET_WM_MOVERESIZE` and
  the `_NET_WM_STATE` atoms. Without `_GTK_SHOW_WINDOW_MENU` GTK shows
  its own fallback menu; without `_NET_WM_MOVERESIZE` it moves the frame
  window itself.
- An owner of the `_NET_WM_CM_S0` selection (a compositing manager).
  Without it, or without `_GTK_FRAME_EXTENTS`, GTK draws square frames
  with no invisible resize border.
- `_NET_WM_STATE` and `_NET_WM_ALLOWED_ACTIONS` mirrored onto the frame
  window. The focused, maximized, tiled and fullscreen looks come from
  the frame window's own `_NET_WM_STATE`.
- An XSETTINGS manager for `Gtk/DecorationLayout` and
  `Gtk/TitlebarDoubleClick`; otherwise GTK's defaults apply.

## Running

The compositor starts it with Xwayland. By hand:

```sh
DISPLAY=:1 tuna-x11-frames
```

- It needs only `DISPLAY`. `GDK_BACKEND` is ignored: the X11 backend is
  the only one allowed.
- It exits with status 1 when it cannot open an X11 display, and with
  status 0 on `SIGTERM`.
- It asks an X server started with `-terminate` not to stay up on its
  account (XFixes `ClientDisconnectMode`).
- `--version` needs no display.

GTK must be built with its X11 backend (`gtk4-x11.pc` at build time).
`libX11`, `libXi` and `libXfixes` are loaded at run time; GTK's X11
backend already depends on all three.

## Source map

| File | Mutter 51 `src/frames/` | Contents |
|---|---|---|
| `src/main.rs` | `main.c` | Start-up, exit |
| `src/tracker.rs` | `meta-window-tracker.c` | Which windows get frames |
| `src/frame.rs` | `meta-frame.c` | The frame window and what it mirrors |
| `src/header.rs` | `meta-frame-header.c` | The title bar wrapper |
| `src/content.rs` | `meta-frame-content.c` | The placeholder for the client |
| `src/protocol.rs` | (parts of the above) | Property decoding and event decisions; no X server needed |
| `src/server.rs` | (the Xlib calls of the above) | Requests on GDK's X connection, error-trapped |

Each module's documentation lists where it differs from Mutter. In short:
libadwaita is always loaded; the settings portal is not used; replies are
error-checked without an extra round trip; `SIGTERM` exits from the
signal handler.

## Tests

```sh
cargo test -p tuna-x11-frames
```

The unit tests cover `protocol.rs` and the start-up environment.
`tests/cli.rs` runs the binary without a display. Nothing in `cargo test`
talks to an X server: frames on a live Xwayland need a running session.

## Licence

Mutter's frames client is GPL-2.0-or-later, © Red Hat, written by Carlos
Garnacho. This adaptation is distributed under GPL-3.0-or-later with the
rest of the workspace.
