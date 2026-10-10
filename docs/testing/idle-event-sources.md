# Compositor idle wake plumbing

The compositor retains its 16 ms dispatch timeout. This change establishes
wake sources and removes repetitive idle-blank file reads; it does not yet
make the compositor block indefinitely at idle or demonstrate a CPU win.

ControlHub keeps a Linux epoll set for its listener and each admitted accepted
stream. A duplicated epoll descriptor wakes calloop; no accepted stream is
duplicated by the watcher. Closing the original stream automatically removes
its interest, including failed handshakes and peer-gate eviction. Edge
notifications are drained in bounded batches; parsing, peer policy and the
one-command-per-session behavior remain in ControlHub::poll. The periodic
safety tick still processes handshake phases and already-buffered commands.
Do not widen the timeout without accounting for those tasks.

The Wayland display's accepted-client epoll descriptor now wakes nested and
native sessions. The listening socket alone reports new connections, not
requests on existing clients.

The idle-blank signal is initially read after installing a parent-directory
inotify watch. Target-file edits, atomic rename, deletion, recreation and
metadata changes invalidate the cache. Queue overflow forces a conservative
rescan. Directory removal/move, watch loss and read errors remove the loop
source and restore the prior per-tick fallback. Watch creation failure also
retains fallback. Reads still reject symlinks, special files, oversize and
invalid UTF-8 signals. An invalid cache is parsed with the existing disabled
blanking fallback. The wire schema and writer contract do not change.

## Before relaxing idle dispatch

Further work must provide wakeable sleep/Orca/wallpaper worker completion,
idle-watch registration and child-exit observation, and schedule actual lock,
blank fade, animation, screencast, restart and protocol deadlines. Shutdown
signals must interrupt the loop. GTK control reconciliation needs socket
readiness plus bounded allocation/animation follow-ups so switcher thumbnail
geometry remains correct after GTK layout and during scrolling. A simple
no-message early return or a larger fixed timeout is insufficient.

Qualification needs stable exact-source binaries, delivered input/control
and timer regression proofs, and a matched native paired CPU benchmark.
Nested wake tests cannot establish a physical GPU/power or GNOME performance
advantage.
