# Compositor idle wake scheduling

The compositor waits for the earliest real obligation or a registered event.
An idle session can block without the former unconditional 16 ms tick.
Active animations, refresh protocol obligations, buffered control work and
failed watches retain the existing 16 ms service cap. This is source behavior,
not evidence of a measured CPU, GPU, power or GNOME performance advantage.

## Sources and deadlines

| Obligation | Wake or deadline |
| --- | --- |
| Native and nested Wayland clients | Listening socket and accepted-client display epoll descriptor |
| Shell control | Hub epoll listener/accepted sockets; bounded continuation for handshake phases, complete buffered frames, dirty broadcasts and model changes |
| Owned shell, Orca, IBus/XIM, bell player | Owned unreaped-child pidfds; readable source removes itself; failed observation or deferred reap retains active fallback |
| Sleep monitor | Coalesced ping after publishing lock/wake flags; unlock generation invalidates before the lock flag |
| Orca observation, IBus address probe, wallpaper identity/decode/card, X11 icons | Coalesced worker result/disconnect ping; result sender retires before final completion ping, including unwind |
| IdleMonitor D-Bus | Registration/removal ping and earliest unfired idle interval; active watches wait for real input |
| Shell/IME/Orca/Xwayland restart | Existing retry deadline and attempt budget, no timer for exhausted/disabled supervision |
| Wallpaper source identity | Existing one-second observation interval for the current source, excluded while its worker is pending |
| X11 icons | Existing 500 ms probe interval while XWM is ready, bounded full-queue retry |
| Lock and idle blank | Actual idle threshold; 16 ms only during blank fade; held inhibitors suppress deadlines and their removal resets the input clock anchor |
| Overview, strip/column, workspace, size change, window lifecycle, modal parent brightness, tile preview, screenshot flash/reply | Active motion cap on real animation time; held workspace swipe waits for input; manual proof time waits for its file source |
| Workspace grace and explicit leases | Earliest real monotonic expiry; leases retire before drag-paused reconciliation; startup identities wait for producer events |
| Bell visuals/player timeout | Real-time visual service and existing five-second player limit, independent of a manual animation clock |
| Cast | Existing 33 ms streaming cadence; failed/no-buffer capture uses active bounded retry; PipeWire and revocation sources already wake calloop |
| Frame callbacks, FIFO/commit timer obligations | Existing refresh service cap, with release still performed only by the existing drawn-frame / KMS pageflip paths |
| KMS presentation | Registered DRM pageflip source; pending output scanout is never fabricated or overwritten by this scheduler |
| Shutdown | signal-hook nonblocking self-pipe plus atomic flag; handlers unregister with their runtime |

Client keyboard repeats remain governed by wl_keyboard.repeat_info. The
scheduler does not invent a compositor repeat timer or release a presentation
barrier merely because wall time passed. Hidden/pending protocol obligations
can conservatively retain refresh service rather than being treated as an
unconditionally idle scene.

## File watches and fallback

Idle-blank, wallpaper, and instrumented animation-clock/swipe files use
parent-directory inotify watches. Target edits, atomic rename, deletion,
recreation and metadata changes rescan the cache. Overflow rescans; directory
move/removal, watch loss, read errors, or the bounded callback cap retire the
source and restore active polling. The callback cap deliberately invalidates
the cache because a prefetched later watch-loss event cannot safely be lost.

Reads reject symlinks, special files, oversize and invalid UTF-8. Small proof
and idle signals keep their 4096-byte limit; wallpaper retains its existing
16 KiB contract. The wire schema and writer contracts do not change.

## Qualification

Focused source tests cover deadline ordering/manual time/inhibition removal,
worker completion and failure publication order, real child pidfd exit/removal,
signal wake/drain, control buffered frames/phases, IdleMonitor registration,
wallpaper identity eligibility, and watch rename/delete/loss/flood fallback.
They do not establish runtime rendering or performance equivalence.
Workspace deadline tests additionally cover earliest dialog/explicit expiry,
manual animation-clock independence and expired-lease retirement. Startup and
explicit lease APIs still need their genuine producer connections; a timer
hook does not claim that missing integration.

Run the compositor library suite and strict Clippy for both supported feature
sets, then build and freeze exact-source binaries under the shared build guard.
Re-run matched native delivered input/control, manual animation clock, lock/
blank/inhibitor, screenshot feedback, casts, sleep/auth generation and shell
restart proofs. GTK retains its separate allocation/reconciliation polling;
this batch makes no GTK idle optimization claim. Finally run a matched native
paired CPU/power benchmark with valid counters and presentation receipts.
