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

### 1. Hidden windows keep the compositor repainting at the display rate (fix: `perf/hidden-window-callbacks`)

#531 throttled the frame callbacks of hidden windows (other workspace, minimised or fully occluded) to one per 995 ms. However, `frame_timing::pending_output_work` still counted an unsent callback on any mapped root as repaint work. A hidden window waiting for its throttled callback therefore forced a full scene build, draw and page flip on every vblank until the throttle released it. GTK applications ask again right after each callback, so a hidden window that redraws, such as System Monitor, kept the compositor submitting flips at 75 Hz with nothing visible changing.

This matches the steady state in the final settle: compositor 17.4% against gnome-shell 6–7%, with the same application cost in both sessions. It also contributes to the overview and workspace phases, where whole workspaces of windows are hidden.

The fix changes three things:

- Callbacks of throttled (hidden, configured) windows no longer count as output work. Their FIFO barriers and commit timers still do.
- Those callbacks are sent by a separate `hidden_frame_beat`, at most once per `HIDDEN_FRAME_INTERVAL`, without a scanout.
- Visible windows keep getting their callbacks with each queued frame.

The `protocol_gaps` tests cover a hidden callback that is not repaint work and a hidden FIFO barrier that is.

### 2. The compositor loop wakes every 16 ms (fix in progress: `perf/compositor-demand-driven-loop`)

`run()` dispatches with a fixed 16 ms timeout and then always runs `Runtime::tick`. Each tick:

- reads `tuna-idle-blank`;
- polls the control hub, supervisors, the IdleMonitor and worker channels;
- rebuilds the scene for every output that has no flip pending.

That is 62 wakeups per second with nothing on screen, and 0.9–1.1% of a core in the idle phase. It is Tuna's whole idle gap together with finding 3.

The branch makes the loop demand-driven:

- **Control socket:** the control hub keeps an epoll set of its listener and sessions, which the loop registers. A new `ControlHub::flush` sends anything queued during the tick, and `next_poll` reports buffered frames and owed broadcasts.
- **Child exits and termination:** SIGCHLD, SIGTERM and SIGINT wake the loop through a signal-hook socket pair.
- **Runtime files:** `tuna-idle-blank` and the wallpaper drop file wake the loop through inotify on `XDG_RUNTIME_DIR`.
- **Worker threads:** the IdleMonitor D-Bus thread, the logind sleep thread, the Orca watchers and the wallpaper identity, decode and card workers ping the loop.
- **Deadlines:** the supervisors and the IdleMonitor expose their next deadlines. The IBus bridge and XIM restart gates, the Orca restart gate and the idle watches are done. The lock timeout, idle blank, shortcut-consent expiry and hidden-beat deadlines still have to be added to `next_wake`.
- **Animations:** `OverviewMotion::animating` keeps the frame cadence while the overview is still moving.

Still to do:

- Wire `next_wake` and the sources into `runtime.rs`, including the idle-inhibitor transition, so that an ended inhibitor does not lock at once.
- Build, test, and measure before and after on vkms.

### 3. The shell wakes on several timers (fix delegated: `perf/shell-gtk-demand-driven`)

`tuna-shell-gtk` runs these timers while idle:

- 16 ms: drains the control socket and re-syncs the pills, switcher, lock UI and preview chrome every time.
- 250 ms: notification tick, which makes a synchronous `GetIdletime` D-Bus call while a banner is showing.
- 250 ms: DND sync.
- 1 s: clock.
- 100 ms: tray channel poll.
- 2 s: IBus panel poll.

Together they cost 0.27–0.37% of a core at idle, and they also run through every workload phase. They are being replaced with:

- an fd watch on the control socket;
- change signals for notifications, DND and the tray;
- a wall-clock timer for the clock that fires on the minute.

### 4. Shell bursts on the first overview search (open)

When the overview first opens and "disks" is typed, `tuna-shell-gtk` uses 0.9–2.0 cores for about 7 s, about 8.3 CPU-seconds in total. gnome-shell uses about 4.8 CPU-seconds for the same input. Later searches cost about 1.5 CPU-seconds each. This is the largest single item in the probe and launch phase. It needs a profile of the search and app-grid build: icon lookups, provider calls, grid construction. **Owner: tuna-shell-gtk.**

### 5. Compositor cost per committed frame under workload (open)

With System Monitor redrawing in the foreground (104–122 s), the compositor uses 14–46% against gnome-shell's 7–23%. Part of this is finding 1. The rest scales with ticks per commit: the client commit, the page flip and the 16 ms timer each rebuild the scene and its signature. Findings 1 and 2 reduce both. Confirming the rest needs the in-guest `perf record` proposed in the 2026-10-08 report. **Owner: tuna-compositor.**

## Remaining gap and owners

| Gap | Owner | Status |
|---|---|---|
| Idle 1.41% vs 0.14% | tuna-compositor (~1.0 points) and tuna-shell-gtk (~0.3 points) | Findings 2 and 3 in progress |
| Final settle 17% vs 6% | tuna-compositor | Finding 1 implemented; measurement pending |
| Probe + launch | tuna-shell-gtk | Finding 4 open |
| Notifications | tuna-compositor and tuna-shell-gtk (+15 points over gnome-shell) | Open; finding 3 removes the shell's 250 ms tick |
| Overview and workspace switches | tuna-compositor (+5–8 points) | Findings 1 and 2 expected to help |

## Verification still needed

None of the fixes above has a paired run yet. Each branch needs:

1. A CI package.
2. A `performance-baseline.yml` dispatch with `package_source=candidate`, `package_run=<ci run>` and `candidate_commit=<sha>`. Run 38713249586 used the defaults (`package_source=main`).
3. The resulting paired medians, added to this report.

For local measurement on this shared 2-core host, run the CI-built `tuna-deb` inside `localhost/cpuparity-measure:base` (an Ubuntu 26.04 image with the package's dependencies installed) against vkms. Host release builds did not finish within an hour under a load average of 35.
