# Compositor idle wake scheduling

The native loop (#503, #678) sleeps until an event source fires or
`Runtime::next_wake` says something time-driven is due, at most 500 ms
(`IDLE_WAKE`). While anything moves it ticks at the 16 ms display rate
(`FRAME_BUDGET`), and it keeps that pace for 500 ms (`ACTIVITY_LINGER`) after
the last queued frame or input event. The nested (winit) backend and
proof-instrumented sessions (an animation-clock or synthetic-swipe file with a
state document) always run at 16 ms. This is source behavior, not evidence of a
measured CPU, GPU, power or GNOME performance advantage.

## Event sources

| Producer | Source |
| --- | --- |
| Native and nested Wayland clients | Listening socket and accepted-client display epoll descriptor |
| Shell control | Hub epoll set (`control_readiness`): listener and every accepted socket from accept on |
| Child exits (shell, IME/XIM, Orca, XWayland, bell player), SIGTERM, SIGINT | One signal-hook socket pair (`wake::signal_source`) |
| Idle-blank file, wallpaper drop | inotify on `XDG_RUNTIME_DIR` (`wake::file_watch`); not watched when the state document is written there, then the blank file is read every tick |
| Worker threads: IdleMonitor registrations, Orca watchers and departures, wallpaper identity/decode/card, Night Light, IME address probe, X11 icons, PAM, logind sleep | One per-runtime coalesced `wake::Wake` ping; completion wakes fire after the result is published, including on unwind |
| KMS presentation | DRM page-flip source |

## Time-driven wakes (`next_wake`)

Frame pace while: activity linger, overview/strip/workspace/size-change/window
lifecycle/modal-dim motion, cursor or wellbeing-effect animation, screenshot
flash or pending reply, tile preview, bell visual, shell swipe, overlay, screen
casts, or a PAM worker that has not published its terminal phase.

Otherwise the earliest of, never sooner than one frame: the lock timeout and
idle-blank threshold (16 ms during the blank fade; none while an inhibitor is
held), IdleMonitor and Orca deadlines, workspace leases, wallpaper source
re-observation, Night Light preview expiry, bell player limit, XWayland restart
backoff, X11 icon probe, IME restart gates (not while locked), the hidden-window
frame beat, shell restart backoff, and the control hub's `next_poll` (at once
for a buffered frame, 16 ms while a handshake or broadcast is owed).

The end of an idle inhibitor restarts the idle clock, so a session that slept
through the inhibited stretch never locks the moment it ends. Hidden windows'
frame callbacks wait for the hidden beat and never force a repaint.

## Qualification

Source tests cover the signal source, directory watch, worker completion
ordering, control readiness and buffered frames, IdleMonitor registration,
workspace lease expiry and wallpaper identity eligibility. They do not establish
runtime rendering or performance equivalence; run the native input/control,
lock/blank/inhibitor, screenshot, cast, sleep and shell-restart proofs, then a
matched native CPU/power benchmark.
