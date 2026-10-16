# Where Tuna's remaining CPU goes against GNOME 51 (2026-10-15)

ADR 0008 requires Tuna Desktop's CPU to be no worse than GNOME 51.0 on the paired Marlin VM benchmark. This report attributes the CPU in [paired run 38713249586](https://github.com/tuna-os/tuna-desktop/actions/runs/38713249586) (main `c9d94d15`, stock profile, three alternating pairs) to processes and workload phases. It names the code behind the largest gaps and the fixes in progress. It follows the [2026-10-08 investigation](2026-10-08-cpu-investigation.md).

## Headline numbers (stock profile, median of three pairs)

| Metric | GNOME 51.0 | Tuna |
|---|---:|---:|
| Idle user CPU, mean | 0.14% | 1.41% |
| Whole-run user CPU, mean | 21.9% | 33.0% |

## Method

The observer records per-process `cpu_ticks` once per second, without names. Processes are identified as follows:

- From the journal identifiers in `serial.log`. The compositor, `tuna-shell-gtk` and `tuna-ibus-bridge` log through `tuna-perf-compositor[pid]`, and `gnome-shell[pid]` logs under its own name.
- From the process tree and start time for everything else. Applications are processes that start after the idle phase.

Phase boundaries use the guest idle and notification markers. The host workload markers are shifted onto the guest clock using the idle markers; the drift is under 0.11 s. The script's fixed sleeps give:

| Phase | Duration |
|---|---|
| Settle | 30 s before idle |
| Idle | 30 s |
| Frame-pacer probe and three app launches | about 39 s |
| 20 overview toggles | about 34 s |
| 10 workspace switch pairs | 20 s |
| 10 notifications | about 13 s |
| Final settle with Disks, System Monitor and Files open | 30 s |

## Attribution, stock pair 1 (repeat 2 in brackets), percent of one core

| Phase | tuna-compositor | tuna-shell-gtk | Tuna apps | gnome-shell | GNOME apps |
|---|---:|---:|---:|---:|---:|
| Settle | 0.9 (1.1) | 0.3 (0.3) | 0 | 0.6 (0.7) | 0 |
| Idle | 0.9 (1.1) | 0.3 (0.4) | 0 | 0.2 (0.1) | 0 |
| Probe + launches | 22.1 (20.8) | **22.5 (23.7)** | 14.0 (13.3) | 19.2 (17.0) | 16.7 (15.3) |
| Overview toggles | **29.9 (34.6)** | 4.1 (5.5) | 9.3 (9.0) | 21.3 (26.6) | 8.3 (8.5) |
| Workspace switches | **34.6 (32.6)** | 2.5 (2.6) | 11.8 (11.2) | 28.9 (26.1) | 7.8 (9.0) |
| Notifications | 28.2 (25.7) | **11.8 (9.6)** | 10.4 (11.1) | 12.7 (15.7) | 9.2 (9.0) |
| Final settle, 3 apps | **17.4 (17.4)** | 0.6 (0.9) | 8.6 (10.0) | 5.9 (7.1) | 8.6 (8.3) |
| Whole run | 17.9 (18.1) | 6.5 (7.1) | 7.3 (7.4) | 13.6 (14.1) | 7.2 (7.1) |

Notes on the other processes:

- The IBus bridge uses 0.0–0.2%.
- Tuna's other session services use 0.0–0.6% outside startup.
- The applications cost the same in both sessions. The gap is entirely Tuna's compositor and shell.

Weighted by phase length, Tuna's excess over GNOME comes from these phases:

| Phase | Share of the excess |
|---|---:|
| Probe + launches | about 45% |
| Overview | about 19% |
| Notifications | about 15% |
| Final settle | about 14% |
| Workspace switches | about 9% |
| Idle | under 2% |

## Findings

### 1. Hidden windows keep the compositor repainting at the display rate (fixed: #676)

#531 throttled the frame callbacks of hidden windows (other workspace, minimised or fully occluded) to one per 995 ms. However, `frame_timing::pending_output_work` still counted an unsent callback on any mapped root as repaint work. A hidden window waiting for its throttled callback therefore forced a full scene build, draw and page flip on every vblank until the throttle released it. GTK applications ask again right after each callback, so a hidden window that redraws, such as System Monitor, kept the compositor submitting flips at 75 Hz with nothing visible changing.

This matches the steady state in the final settle: compositor 17.4% against gnome-shell 6–7%, with the same application cost in both sessions. It also contributes to the overview and workspace phases, where whole workspaces of windows are hidden.

The fix changes three things:

- Callbacks of throttled (hidden, configured) windows no longer count as output work. Their FIFO barriers and commit timers still do.
- Those callbacks are sent by a separate `hidden_frame_beat`, at most once per `HIDDEN_FRAME_INTERVAL`, without a scanout.
- Visible windows keep getting their callbacks with each queued frame.

The `protocol_gaps` tests cover a hidden callback that is not repaint work and a hidden FIFO barrier that is.

### 2. The compositor loop wakes every 16 ms (fixed: #678)

`run()` dispatches with a fixed 16 ms timeout and then always runs `Runtime::tick`. Each tick:

- reads `tuna-idle-blank`;
- polls the control hub, supervisors, the IdleMonitor and worker channels;
- rebuilds the scene for every output that has no flip pending.

That is 62 wakeups per second with nothing on screen, and 0.9–1.1% of a core in the idle phase. It is Tuna's whole idle gap together with finding 3.

#678 makes the native (DRM) loop demand-driven:

- **Control socket:** the control hub keeps an epoll set of its listener and sessions, which the loop registers. `ControlHub::flush` sends anything queued during the tick before the loop sleeps, and `next_poll` reports buffered frames and owed broadcasts.
- **Child exits and termination:** SIGCHLD, SIGTERM and SIGINT wake the loop through a signal-hook socket pair.
- **Runtime files:** `tuna-idle-blank` and the wallpaper drop file wake the loop through inotify on `XDG_RUNTIME_DIR`, and `tuna-idle-blank` is read only when it changed. When the compositor's own state document lives in that directory, it is not watched and the file is read on every tick.
- **Worker threads:** the IdleMonitor D-Bus thread, the logind sleep thread, the Orca watchers and the wallpaper identity, decode and card workers ping the loop.
- **Deadlines:** `Runtime::next_wake` sleeps until the earliest of the session lock, the idle blank (display rate while it fades), Mutter idle watches, the Orca and IME restart gates, the hidden-window beat, the shell restart backoff and the control retry. Timed deadlines are floored at 16 ms and every sleep is capped at 500 ms. The cap serves producers without a source: the XWayland supervisor, the X11 icon reader and a stalled client flush.
- **Animations:** the loop keeps the 16 ms pace for 500 ms after a queued frame or input event, and while the overview, a workspace slide, the strip spring, a size change, a swipe, the recovery overlay or a screen cast is active.
- **Idle inhibitor:** the end of an inhibitor restarts the idle clock, so a session that slept through the inhibited stretch does not lock at once.

The nested winit backend and proof-instrumented sessions (`TUNA_ANIMATION_CLOCK`, `TUNA_PROOF_SWIPE_INPUT`) keep the fixed 16 ms pace.

### 3. The shell wakes on several timers (fixed: #677)

`tuna-shell-gtk` runs these timers while idle:

- 16 ms: drains the control socket and re-syncs the pills, switcher, lock UI and preview chrome every time.
- 250 ms: notification tick, which makes a synchronous `GetIdletime` D-Bus call while a banner is showing.
- 250 ms: DND sync.
- 1 s: clock.
- 100 ms: tray channel poll.
- 2 s: IBus panel poll.

Together they cost 0.27–0.37% of a core at idle, and they also run through every workload phase. #677 replaced them with:

- an fd watch on the control socket;
- change signals for notifications, DND and the tray;
- a wall-clock timer for the clock that fires on the minute.

### 4. Shell bursts on the first overview search (fixed: #693)

When the overview first opens and "disks" is typed, `tuna-shell-gtk` uses 0.9–2.0 cores for about 7 s, about 8.3 CPU-seconds in total. gnome-shell uses about 4.8 CPU-seconds for the same input. Later searches cost about 1.5 CPU-seconds each. This is the largest single item in the probe and launch phase. It needs a profile of the search and app-grid build: icon lookups, provider calls, grid construction. **Owner: tuna-shell-gtk.**

#### tuna-shell-gtk: profile and fix (#693)

**Method.** The fixture has no in-guest profiler for Tuna, so the workload was replayed locally. The setup was the Marlin image with the CI-built `tuna-desktop` package, Disks and System Monitor added, and `tuna-compositor --backend winit` under Xvfb. GL was llvmpipe, as on the VM. `xdotool` drove the same input: Super, type the query, Return and 8 s, for "disks", "system monitor" and "files"; then 20 overview toggles, 10 workspace switch pairs and 10 `Notify` calls. CPU per phase was read from `/proc/<pid>/stat`. `perf record -g -p <shell>` ran on one phase at a time.

**Hotspots, main package (share of tuna-shell-gtk samples).** The app grid, providers and `.desktop` scans were not significant. `OverviewUi::rebuild_grid` was about 1% of the toggle samples in a symbolized build. Search-provider calls are asynchronous and showed no measurable shell cost. The app list is cached by `LiveApps`.

| Phase | libLLVM (llvmpipe shader JIT) | libgallium (llvmpipe) | kernel | libc | GTK |
|---|---:|---:|---:|---:|---:|
| First search ("disks") | 77% | 9% | 5% | 7% | <1% |
| Overview toggles | 24% | 6% | 34% | 15% | 8% |
| Notifications | 27% | — | 30% | 14% | 9% |

**Cause.** GSK skips its GL renderer when `GL_RENDERER` is llvmpipe ("Not using GL: renderer is llvmpipe"). Marlin has no Vulkan driver, and in that case GSK's fallback chain (`gskrenderer.c`) still takes llvmpipe GL before cairo. LLVM then compiles each new shader variant the shell's surfaces need. This also accounts for the 0.9–2.0 cores: llvmpipe runs its own threads.

**Fix (#693).** Before any window is realized, the shell reads `GL_RENDERER` from a GL context on its display, which is GSK's own check. On llvmpipe or softpipe it sets `GSK_RENDERER=cairo`. An explicit `GSK_RENDERER` still wins, and hardware GL is unchanged. The `gtk-shell` CI lane runs on llvmpipe, so it exercises the cairo path, and all its gates pass: G-OV-SEARCH, G-OV-APPGRID, the G-GRID-* gates and G-NOTIFY-*.

**Local replica, tuna-shell-gtk CPU-seconds, median of three alternating pairs.** Main package vs candidate package `a84fb35a`:

| Phase | Main | #693 |
|---|---:|---:|
| Startup to idle | 0.94 | 0.96 |
| Search 1 / 2 / 3 | 2.90 / 1.11 / 2.00 | 0.10 / 0.10 / 0.13 |
| 20 overview toggles | 1.38 | 0.55 |
| Workspace switches | 0.51 | 0.21 |
| 10 notifications | 0.83 | 0.37 |
| Whole workload | 9.56 | 2.54 |

Startup with `tuna-session`'s startup overview: 2.31–2.45 s on main against 1.16–1.24 s with #693.

**Paired Marlin run.** [38852155131](https://github.com/tuna-os/tuna-desktop/actions/runs/38852155131) used package [38846235169](https://github.com/tuna-os/tuna-desktop/actions/runs/38846235169) (`a84fb35a`). The workflow ref was the #697 branch, because every run of a post-#689 package was refused before #697 landed. All three diagnostic pairs completed. Stock pair 3 failed on the GNOME side ("resource samples are missing or incomplete"), so the stock figures are two pairs. GNOME on these runners was slower than in 38823409355: gnome-shell used 29–30 CPU-seconds in stock against 22–23. Compare the gaps within each run.

| | Main 38823409355 (stock, 3 pairs) | #693 38852155131 (stock, 2 pairs) |
|---|---:|---:|
| tuna-shell-gtk, whole run, CPU-seconds | 8.35 / 7.95 / 7.48 | 5.48 / 5.11 |
| — probe + launches | 4.59 / 3.86 / 4.03 | 1.90 / 1.57 |
| — overview toggles | 1.32 / 1.27 / 1.07 | 1.09 / 1.06 |
| — notifications | 1.12 / 1.12 / 1.03 | 0.87 / 0.88 |
| — settle (startup) | 0.62 / 0.91 / 0.88 | 1.08 / 1.14 |
| Whole run, Tuna vs GNOME | 24.9 vs 16.5 (+8.4) | 28.2 vs 22.4 (+5.8), 27.6 vs 22.4 (+5.2) |

The diagnostic pairs agree. tuna-shell-gtk used 8.31/7.40/8.17 CPU-seconds on main and 5.89/5.71/5.52 with #693. The whole-run gap went from +8.4/+7.3/+9.6 to +7.5/+7.9/+5.4.

In the VM the search bursts drop from 2.6/1.1/0.9 CPU-seconds to 0.65/0.76/0.42. Locally they cost 0.1 each, so the VM still pays for something the replica lacks. The live search providers are one difference: they do not answer in the container. The remaining shell cost:

- **Per-tick resync once app windows exist.** This is 2–9 ticks per second through the overview, workspace and notification phases. The compositor sends a `WindowUpdated` delta for every window on every tick, which #696 fixes compositor-side.
- **Startup.** In the VM, startup is 0.2–0.5 CPU-seconds above main. The probe initializes GDK's GL, about 0.1 s locally. A surfaceless EGL probe would cost about half of that.
- **Notifications, 0.9 s.** Each update rebuilds the whole history list, and the banner slide is drawn by cairo.

### 5. Compositor cost per committed frame under workload (open)

With System Monitor redrawing in the foreground (104–122 s), the compositor uses 14–46% against gnome-shell's 7–23%. Part of this is finding 1. The rest scales with ticks per commit: the client commit, the page flip and the 16 ms timer each rebuild the scene and its signature. Findings 1 and 2 reduce both. Confirming the rest needs the in-guest `perf record` proposed in the 2026-10-08 report. **Owner: tuna-compositor.**

## Paired results after the fixes

Each fix was measured on its own, with a candidate package from an exact-head `ci.yml` dispatch, and then all three together on main. All runs use the stock profile and report the median of three alternating pairs. The GNOME session's own whole-run CPU moves by several points between runs (16.5–25.4%) on the shared runners, so each fix is judged by the Tuna − GNOME gap within its own run.

| Build | Package (ci run) | Paired run | Idle, Tuna vs GNOME | Idle gap | Whole run, Tuna vs GNOME | Whole-run gap |
|---|---|---|---:|---:|---:|---:|
| Baseline, main `c9d94d15` | main push | [38713249586](https://github.com/tuna-os/tuna-desktop/actions/runs/38713249586) | 1.41 vs 0.14 | +1.27 | 33.0 vs 21.9 | +11.1 |
| #676 only, `4ae5cf6d` | [38745633085](https://github.com/tuna-os/tuna-desktop/actions/runs/38745633085) | [38770801094](https://github.com/tuna-os/tuna-desktop/actions/runs/38770801094) | 1.83 vs 0.10 | +1.73 | 33.6 vs 23.3 | +10.3 |
| #677 only, `e3216ad0` | [38770584654](https://github.com/tuna-os/tuna-desktop/actions/runs/38770584654) | [38798510135](https://github.com/tuna-os/tuna-desktop/actions/runs/38798510135) | 1.17 vs 0.14 | +1.03 | 35.6 vs 25.4 | +10.2 |
| #678 on #676, `fb5721b4` | [38767907838](https://github.com/tuna-os/tuna-desktop/actions/runs/38767907838) | [38797046345](https://github.com/tuna-os/tuna-desktop/actions/runs/38797046345) | 0.55 vs 0.10 | +0.45 | 31.5 vs 22.6 | +8.9 |
| All three, main `b9a6d554` | [38801524963](https://github.com/tuna-os/tuna-desktop/actions/runs/38801524963) | [38823409355](https://github.com/tuna-os/tuna-desktop/actions/runs/38823409355) | 0.21 vs 0.17 | +0.04 | 24.9 vs 16.5 | +8.4 |

Per-repeat means (Tuna; GNOME):

- #676: idle 1.86/1.21/1.83 (0.17/0.10/0.10); whole 34.5/31.9/33.6 (22.8/23.3/23.6).
- #677: idle 1.07/1.17/1.17 (0.17/0.07/0.14); whole 35.3/36.0/35.6 (25.6/24.0/25.4).
- #678: idle 0.55/0.66/0.55 (0.10/0.38/0.07); whole 31.5/31.4/31.9 (21.4/23.4/22.6).
- Main: idle 0.21/0.38/0.17 (0.07/0.31/0.17); whole 24.9/25.7/22.0 (17.9/16.5/16.2).

The three single-fix runs (38770801094, 38797046345 and 38798510135) end in failure. In each, all six stock reports were written before the diagnostic profile failed: on GNOME's kernel observer loss in the first run and on "resource samples are missing or incomplete" in the other two. The stock medians above come from those complete reports. Run 38823409355 succeeded.

What the runs show:

- **Idle reached parity.** The idle gap went from +1.27 to +0.04 points (0.21% vs 0.17%), which is inside the pair-to-pair spread. #678 removed most of it (+0.45 with #676 alone underneath), and #677 removed the shell's part.
- **#676 alone did not move the whole run** beyond noise (+10.3 against +11.1). In the combined run the final settle is still 14–17% for tuna-compositor against 6–7% for gnome-shell (see below).
- **The whole-run gap is 8.4 points** (24.9% vs 16.5%, 1.5× GNOME), down from 11.1.

### Attribution, combined main run 38823409355, stock pair 1 (pair 2 in brackets), percent of one core

This uses the same method as above. Idle and whole-run totals match `report.json`.

| Phase | tuna-compositor | tuna-shell-gtk | Tuna apps | gnome-shell | GNOME apps |
|---|---:|---:|---:|---:|---:|
| Settle | 0.2 (0.1) | 0.1 (0.0) | 0 | 1.2 (0.3) | 0 |
| Idle | 0.1 (0.2) | 0.0 (0.1) | 0 | 0.1 (0.3) | 0 |
| Probe + launches | 16.4 (17.4) | **12.1 (10.4)** | 9.0 (7.1) | 15.4 (14.1) | 9.1 (8.9) |
| Overview toggles | **31.0 (30.4)** | 4.4 (4.1) | 7.0 (7.5) | 19.2 (19.4) | 6.1 (5.9) |
| Workspace switches | 25.1 (28.8) | 2.9 (2.6) | 10.0 (10.7) | 27.0 (26.7) | 5.5 (4.1) |
| Notifications | **23.6 (25.6)** | **8.3 (9.2)** | 7.6 (7.0) | 8.9 (5.9) | 8.1 (6.6) |
| Final settle, 3 apps | **14.2 (17.3)** | 0.3 (0.6) | 6.0 (7.5) | 7.1 (6.4) | 7.1 (7.0) |
| Whole run | 14.5 (15.6) | 4.2 (4.0) | 5.2 (5.1) | 11.7 (11.0) | 4.8 (4.4) |

Weighted by phase length, Tuna's excess is split as follows:

| Phase | Share of the excess | tuna-compositor − gnome-shell, points | tuna-shell-gtk, points |
|---|---:|---:|---:|
| Probe + launches | 33% (24%) | +1.1 (+3.4) | +12.1 (+10.4) |
| Overview toggles | 32% (26%) | +11.9 (+11.0) | +4.4 (+4.1) |
| Notifications | 18% (19%) | +14.7 (+19.7) | +8.3 (+9.2) |
| Final settle | 12% (19%) | +7.1 (+10.8) | +0.3 (+0.6) |
| Workspace switches | 7% (12%) | −1.8 (+2.2) | +2.9 (+2.6) |
| Idle | 0% | 0 | 0 |

The compositor column subtracts gnome-shell, because gnome-shell does the work of both Tuna processes. Over the whole run the gap is 12.7 (17.4) CPU-seconds. tuna-compositor accounts for 5.5 (8.9) of it and tuna-shell-gtk for 8.4 (8.0). Applications add about 1. GNOME's heavier startup services give back 1–2.

## Remaining gap and owners

| Gap (combined run) | Owner | Status |
|---|---|---|
| Idle 0.21% vs 0.17% | — | At parity after #676, #677 and #678 |
| Probe + launch: shell-gtk 10–12% of a core, compositor +1–3 points | tuna-shell-gtk | Finding 4 open (first overview search and app-grid build) |
| Overview toggles: compositor 31% vs gnome-shell 19% | tuna-compositor | Open; finding 5 (scene rebuild and draw per frame) |
| Notifications: compositor +15–20 points, shell-gtk 8–9% | tuna-compositor and tuna-shell-gtk | Open; the shell's 250 ms tick is gone (#677), and the banner path still needs a profile |
| Final settle: compositor 14–17% vs gnome-shell 6–7% | tuna-compositor | Open; still about 2× after #676. Finding 5: per-commit cost of the visible System Monitor |
| Workspace switches | tuna-compositor and apps | Within noise for the compositor; Tuna's apps use +5 points |

The next step for both owners is the in-guest `perf record` proposed in the 2026-10-08 report. Run it on the overview, notification and final-settle phases (compositor) and on the first search (shell-gtk).

For local measurement on this shared 2-core host, run the CI-built `tuna-deb` inside `localhost/cpuparity-measure:base` (an Ubuntu 26.04 image with the package's dependencies installed) against vkms. Host release builds did not finish within an hour under a load average of 35.
