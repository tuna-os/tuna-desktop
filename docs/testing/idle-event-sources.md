# Compositor idle wake scheduling

The native loop (#503, #678) sleeps until an event source fires or
`Runtime::next_wake` says something time-driven is due, at most 500 ms
(`IDLE_WAKE`). While anything moves it ticks at the 16 ms display rate
(`FRAME_BUDGET`), and it keeps that pace for 500 ms (`ACTIVITY_LINGER`) after
the last input event. The nested (winit) backend and proof-instrumented
sessions (an animation-clock or synthetic-swipe file with a state document)
always run at 16 ms. This is source behavior, not evidence of a measured CPU,
GPU, power or GNOME performance advantage.

## Idle ticks

When nothing is due within `IDLE_WAKE`, the loop waits on calloop's own
descriptor without dispatching (`wake::ready_within`). A source that becomes
ready is dispatched and followed by a full tick, as always. If none does,
nothing was received and nothing was due, and the tick that follows
(`Runtime::idle_tick`) is an idle one:

- It runs what a tick has always polled: a stalled client flush, the control
  hub's token reissue, the supervisors.
- It does not rebuild or draw the scene, and does not look at the wallpaper.
- It draws after all only when the tick itself read something that arrived
  after the wait ended (client requests, a control frame or peer) or when its
  polls set something moving.

A producer that changes what is drawn must therefore wake the loop (an event
source or `wake::Wake`) or report its deadline to `producer_wake`. Whatever a
worker publishes with a `Wake` is never lost to an idle tick: the idle wait
does not dispatch, so the ping stays pending and the next wait ends at once in
a full tick.

The loop keeps to full ticks, as before, while a calloop timer is armed
(`Runtime::arm_timer`; calloop timers are not descriptors, so arm them only
there), while the runtime directory is not watched, and while the wallpaper is
a slideshow, which follows the wall clock.

## Page-flip ticks

The DRM device's events have an event loop of their own, which the main loop
holds as one descriptor (`FlipLoop`). A full dispatch serves them as before.
When the loop waits itself (nothing moves, no calloop timer is armed, the
runtime directory is watched, no slideshow) and the wait ends with only the
DRM device ready, before any timed producer is due, the page flips are served
alone (`Runtime::dispatch_flips`): their presentation feedback and released
commits reach the clients, and no tick runs (`Runtime::flip_tick`). A full
tick follows only when the flips changed something: a refresh cycle applied a
commit that a fifo barrier or commit timer held back, an event was not a
completed flip, or the last render had to leave an output without its frame
because that output's previous flip was pending. Whatever else arrives
meanwhile stays pending for the full tick that follows, as after an idle
wait. This is Mutter's frame clock going idle after a presentation that
leaves nothing to draw: a client committing ten times a second costs ten
ticks, not twenty (#503).

## Event sources

| Producer | Source |
| --- | --- |
| Native and nested Wayland clients | Listening socket and accepted-client display epoll descriptor |
| Shell control | Hub epoll set (`control_readiness`): listener and every accepted socket from accept on |
| Child exits (shell, IME/XIM, Orca, XWayland, bell player), SIGTERM, SIGINT | One signal-hook socket pair (`wake::signal_source`) |
| Idle-blank file, wallpaper drop | inotify on `XDG_RUNTIME_DIR` (`wake::file_watch`); each file is read again only after a change there. Not watched when the state document is written there; then both are read every tick |
| Wallpaper picture | inotify on the picture's directory, each symbolic link's on the way and the file itself, placed by the observation worker before it reads anything, which then waits on it (`wallpaper::SourceWatch`); one wake per change, then one observation |
| `org.gnome.desktop.lockdown` `disable-lock-screen` | GSettings `changed` on an owned GIO thread (`lockdown_settings`), then the `wake::Wake` ping; no `gsettings` process |
| X11 window icons | `PropertyNotify` for `_NET_WM_ICON` and `WM_HINTS` on the icon worker's own X connection (`x11_icons`), then the `wake::Wake` ping; the worker is told of the windows only when they change |
| Worker threads: IdleMonitor registrations, `org.gnome.SessionManager` inhibitors (taken, released or their owner gone), Orca watchers and departures, wallpaper identity/decode/card and source watch, Night Light, focus policy and tablet settings, IME address probe, PAM, logind sleep | One per-runtime coalesced `wake::Wake` ping; completion wakes fire after the result is published, including on unwind |
| KMS presentation | DRM page-flip source |

The colord thread is not part of the loop. It blocks on its channel: colord
signals, and an inotify watch on the ICC store (`color_manager::StoreWatch`),
one scan per burst of writes. A store on a filesystem inotify cannot vouch for
(anything but a local one), one that holds a symbolic link, or one without a
watch is still scanned every 500 ms.

The Mutter D-Bus thread is not part of the loop either (#503). Its
revocation scan runs every 100 ms only while a ScreenCast session, a
RemoteDesktop grant or an InputCapture session exists (the bound on stream
teardown after a lock or an owner leaving); with none it blocks on
`mutter::ScanWake`, which creating one of them notifies. The orientation
sensor client (`orientation::SensorPoller`) shares that thread: one
system-bus connection hears `NameOwnerChanged` for `net.hadess.SensorProxy`
and the sensor's `PropertiesChanged`, as mutter's `MetaOrientationManager`
does, and the orientation-lock and power-save flags notify the same wake
when they change (`orientation::ChangeHook`). The sensor is read only after
one of these. Fallbacks, each documented in the source: a connection whose
match rules cannot be added (or whose signal thread ended) polls as before,
a present sensor every 100 ms and an absent service through a new connection
every 5 s; a failed claim is retried every 100 ms; no system bus is retried
every 5 s.

## Shell-side sources

| Worker | Source | Former poll |
| --- | --- | --- |
| `tuna-privacy-portal` (portal `Lockdown` publisher) | Its own GLib main loop: `changed` on one held `org.gnome.desktop.privacy` GSettings, the staged wake of a bus-originated set (`LockdownBus::on_staged`), `g_bus_watch_name` for the backend name vanishing. Retries every 100 ms only while a staged set was refused or the name could not be taken with no other owner | 100 ms sleep, three new `GSettings` per pass |
| `tuna-ibus-bridge` main loop | One `poll` over the Wayland socket and GLib's own descriptors with GLib's timeout (`g_main_context_prepare`/`query`/`check`/`dispatch`); IBus signals and the bus closing wake GLib's descriptors. A departed IBus is still looked for with its 250 ms → 5 s backoff | 50 ms poll timeout |
| PipeWire monitor (`pw-dump --monitor`) restart | With no PipeWire socket (`PIPEWIRE_REMOTE`, `PIPEWIRE_RUNTIME_DIR`/`XDG_RUNTIME_DIR`), a GLib directory monitor (inotify) restarts it when the socket is created. A socket that exists (a failure of another kind) or a directory that cannot be watched keeps the 2 s retry | a `pw-dump` process every 2 s |


## Time-driven wakes (`next_wake`)

Frame pace while: activity linger, overview/strip/workspace/size-change/window
lifecycle/modal-dim motion, cursor or wellbeing-effect animation, screenshot
flash or pending reply, tile preview, bell visual, shell swipe, overlay, screen
casts, or a PAM worker that has not published its terminal phase.

Otherwise the earliest of, never sooner than one frame: the lock timeout and
idle-blank threshold (16 ms during the blank fade; none while an inhibitor is
held), IdleMonitor, idle-notify and Orca deadlines, workspace leases, the
wallpaper's safety re-observation (every 5 s, only for a source no watch
vouches for and for a slideshow), Night Light preview expiry and settings
re-read, the focus-policy and tablet settings re-reads, focus and alive-check
deadlines,
the window-session save, the lock conversation's deadline, bell player limit,
XWayland restart backoff, the X11 icon retry (only while the X server did not
answer), IME restart gates (not while locked), the hidden-window and
power-save frame beats, shell restart backoff, the input aids' timers (Slow
Keys acceptance, the Shift-hold shortcut, Mouse Keys motion, hover and
secondary clicks), and the control hub's `next_poll` (at once for a buffered
frame, 16 ms while a handshake or broadcast is owed).

The end of an idle inhibitor restarts the idle clock, so a session that slept
through the inhibited stretch never locks the moment it ends. Hidden windows'
frame callbacks wait for the hidden beat and never force a repaint.

## Periodic work that remains at idle

Read from the code on 2026-11-12. One local measurement the same day (release
build, DRM backend on vkms with llvmpipe, GTK shell, one idle GTK window, a
1.6 MB picture set, no input, three 60 s windows, medians) puts numbers on
the first rows; it is a single noisy host, not the paired GNOME lane:

- The compositor used 0.35 s of CPU a minute, as much as before the idle
  tick (0.33 s); what went away is the `gsettings` child every 2 s (30 a
  minute, 0.25 s of child CPU through a counting wrapper) and half of the
  context switches (1507 against 3274 a minute over all threads).
- The loop thread woke 3.4 times a second: per second one idle tick, one full
  tick for the wallpaper re-observation and one for its completion.
- The wallpaper identity workers (one thread a second) took about 0.12 s of
  that minute, the loop and unnamed threads 0.10 s, the Mutter D-Bus threads
  0.06 s at 10.8 wake-ups a second for the scan.
- With Xwayland started by one `xclock`, the compositor used 0.27 s a minute
  against 0.46 s before: the 500 ms X11 icon probe is gone, and the loop
  thread woke 3.6 times a second instead of 5.4.

| Source | Period | Work per wake | Event that could replace it |
| --- | --- | --- | --- |
| `Wallpaper::maintenance_deadline` | 1 s while a picture is set | A full tick, a `tuna-wallpaper-identity` thread that reads and SHA-256-hashes the whole source file, then a second full tick for its completion | inotify on the source file and its directory, with the hash only after an event; a deadline for slideshow steps |
| Mutter D-Bus thread scan (`mutter::start`) | 100 ms only while a session, grant or capture exists; none at idle since #503 | Three mutex snapshots of the remote, cast and input-capture sessions; owner checks when any exist | `NameOwnerChanged` for owners while sessions exist |
| `orientation::SensorPoller` | None since #503 while its signals are subscribed; the former 100 ms / 5 s polling only as the fallback above | One read after each signal or flag change | Done (#503) |
| Settings re-reads: Night Light, focus policy, tablet (with a tablet) | 5 s each | A full tick, an idle source on the reader's GIO thread, a second full tick for the completion | Their `changed` subscriptions already deliver changes; the re-read only heals a lost signal or a failed first read |
| Control hub token reissue | 15 s | `model.reissue` and a delta per window, on the next tick | A deadline in `next_poll` |
| Idle fallback | 500 ms when nothing else is due | An idle tick: polls, no frame | Nothing to replace; it is the safety net |
| Hidden-window beat | 995 ms while a hidden window waits on a frame callback | Frame callbacks to hidden surfaces | None (deliberate, as a throttle) |
| Orca name watch | 1 s while the screen reader runs | One bus query | `NameOwnerChanged` |
| `blocking` crate pool threads (zbus connects through `blocking::unblock`) | 500 ms per pool thread, for the life of the process (two in the compositor, one in the shell on the vkms replica) | A condvar timeout with no work; `blocking` 1.7 only retires threads above its limit | Connecting the bus socket ourselves and handing zbus the stream (`Builder::unix_stream`) at every connection site |
| Shell brightness (`brightness_service::start`, `services::brightness`) | 500 ms each | A backlight inventory under `/sys/class/backlight` plus the slider's re-read | Kernel `backlight` uevents (every sysfs write and hotkey change emits one) and output-inventory changes; fixture roots (`TUNA_BACKLIGHT_ROOT`) would keep the poll |
| Blanked or locked desktop (measured 2026-11-13, idle-delay 30 s, vkms) | Shell main thread about 65 wake-ups a second, compositor main thread about 88 (against 7 and 38 awake) | Not attributed yet: the shell's main loop wakes at frame pace while the screen is blanked | Mutter stops the frame clock while outputs are off |

## Qualification

Source tests cover the signal source, directory watch, worker completion
ordering, control readiness and buffered frames, IdleMonitor registration,
workspace lease expiry and wallpaper identity eligibility; the quiet-loop
probe (`wake::tests`), the lockdown watch and its absence of wakes
(`lockdown_settings::tests`), X11 icons following `PropertyNotify` on a real
Xvfb with no probe in between (`x11_icons::tests`), the colord thread scanning
its store on change events only (`color_manager::tests`), the watched
wallpaper drop (`wallpaper::tests`), the wake on a changed
`org.gnome.SessionManager` inhibitor (`session_manager::tests`) and the input
aids' deadlines (`input_aids::tests`, `pointer_aids::tests`).

The #503 idle-pollers lane added: the Mutter scan's wait
(`mutter::tests::the_scan_waits_for_a_notify_or_its_deadline`), the sensor
client following `NameOwnerChanged`, `PropertiesChanged` and flag changes
on a private bus with no read in between
(`orientation::tests::the_sensor_client_reads_on_signals_and_flag_changes_only`),
the portal publisher sleeping until a setting or a bus set changes, at most
one wake-up a second (`privacy_portal::tests`, shell), the IBus bridge's
wait (`tuna-ibus-bridge` `tests::the_loop_sleeps_until_the_socket_or_glib_has_something`)
and the PipeWire socket watch (`services::pipewire_socket_tests`).

The nested (winit) backend never takes the idle path: `producer_wake` answers
one frame for it before any deadline is read, so no Xvfb proof exercises an
idle tick, and none can regress on one. Only `--backend drm` does. On
2026-11-12 the idle path was exercised by hand on the kernel's vkms card with
llvmpipe, the way `scripts/tuna-drm-smoke` starts a session (private seatd,
uinput keyboard, state document outside the runtime directory): the loop
thread's `ppoll` timeouts showed the quiet waits, and with nothing else
happening a client's single frame after 3 s of silence was presented at the
next vblank, GSettings changes (`disable-lock-screen`, `focus-mode`,
`cursor-size`) applied within a tick, `ext_idle_notifier_v1` fired at its
timeout, the idle blank began at its delay and faded by drawing, an idle
inhibitor taken through `org.gnome.SessionManager` lit the blanked scanout
again (it stayed black before that inhibitor woke the loop), a held key kept
repeating, a 1500 ms Slow Keys delay was met within 10 ms, a notification
banner appeared and the panel clock crossed a minute. That run is not a
checked-in proof, and vkms is not hardware: lock and unlock, screen casts,
sleep and resume, hotplug, a shell restart and a matched CPU and power
benchmark on a real GPU remain to be run on the native lanes.

A session that cannot watch its runtime directory (for one, a user out of
inotify instances) logs `runtime dir watch unavailable` and keeps to full
ticks, as before the idle tick existed.
