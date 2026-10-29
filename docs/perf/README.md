# Marlin GNOME and Tuna Desktop resource samples

Issue #73 requires comparable measured evidence before any performance claim. The `Marlin performance samples` workflow (manual dispatch or a pull request changing its measurement code) uses one runner, one shared Marlin GNOME image plus Tuna Desktop payload, and the same 6 GiB / four-vCPU / 1280×800 virtio QEMU/KVM profile for both sessions. GNOME runs first, then Tuna Desktop; one boot per session does not establish cold/warm variance. The input package must come from a successful trusted main CI run. The workflow records package SHA256, package source/run, base digest, shared image ID, observer revision, host CPU/kernel and QEMU version.

The published Marlin tag was measured as GNOME Shell 50.5 at digest `sha256:3311d7784a0b0e7cd5486d8b69d974f4a34c3436b9812599470d00f80f805f1b` in [run 37176268295](https://github.com/tuna-os/tuna-desktop/actions/runs/37176268295). It is not the GNOME 51 baseline claimed by older roadmap prose. This benchmark derives a CI-only GNOME 51.0 baseline by resolving every Arch repository, with core-testing and extra-testing enabled together, from one dated [Arch Linux Archive](https://archive.archlinux.org/repos/) snapshot (`ARCH_ARCHIVE_SNAPSHOT` in `packaging/marlin/perf/Containerfile.baseline`), performing one full upgrade/downgrade to that snapshot, and requiring GNOME Shell 51.0 with Mutter 51.0. The snapshot is pinned because Arch and the published Marlin tag moved to the 51.1 point release on 2026-10-14; changing it changes the baseline and requires an ADR. Both sessions use that same derived payload; its package versions and shared image ID are retained. This does not change the shipping Marlin image. The first successful paired guest run is recorded in the dated report below.

The CI-only observer reads every process with the test user's UID, including activated portals and session services. It records per-process identity/start time, name (the kernel `comm` and the basename of `argv[0]`, never the arguments), CPU counters, PSS/RSS, fds and process count once per second. The observer and its descendants are excluded; root/GDM services and kernel allocations are outside this user scope. Reading protected `smaps_rollup` requires the observer to run as root. An incomplete read is a failed measurement, not zero memory.

DRM memory observations follow the [kernel's client usage format](https://www.kernel.org/doc/html/v6.9/gpu/drm-usage-stats.html). The observer retains only driver/device/client identity and standardized memory counters, normalized to bytes. Shared or duplicated handles for the same DRM client are counted once; distinct clients can still account the same shared buffer. Reports therefore label sums as **client-accounted bytes**, preserve each memory region and counter separately, and do not claim unique physical GPU usage. An unsupported driver reports `unavailable`; malformed or unreadable counters report `incomplete`. Neither contributes a measured zero. A supported counter explicitly containing zero remains a valid observation. These counters cover the test UID only and do not measure software renderer memory beyond PSS or memory owned by root services. Actual hardware accounting still requires a qualified run; virtual Bochs scanout may expose none.

The host waits for an actual rendered black panel, allows an equal 30-second post-panel settle, measures 30 seconds without input, runs the native frame-pacer probe, opens Disks, System Monitor and Files by typing in the search, toggles the overview ten times, switches workspaces ten times, cycles windows with Alt+Tab ten times, sends one unmeasured warm-up round and then three measured rounds of ten notifications one second apart, then keeps a final 30-second settle. Each of these phases is bounded by guest-clock markers in `guest-phases.json` (`notifications_warmup_start` marks the warm-up round, which lies outside every phase), and `report.json` breaks CPU down per phase and per process role (`phase_cpu_observed_percent`). Observer records travel on their own virtio-serial port (`org.tuna.perf.0`, a QEMU file chardev retained as `observer-records.log`), never the serial console: journald forwarding them to the emulated 8250 UART stalled QEMU's main loop, and with it frame presentation on both desktops, 0–0.3 s after every record ([run 40931135786](https://github.com/tuna-os/tuna-desktop/actions/runs/40931135786)). The serial console (`serial.log`) keeps boot and session logs only. Corral guests, which have no such port, keep records in the observer unit's journal, read over SSH. Each record ends with ` sha256=` and the SHA-256 of its JSON and carries a `seq` counter; a record whose digest does not match, or a gap or reordering in `seq`, fails the session instead of being dropped. Observer records, raw serial, resource samples, input observations, notification IDs, screenshots and per-session reports are retained for 14 days. The p50/p95/p99 summaries state sample counts; whole-run CPU summaries include startup and workload.

The CI-only guest agent runs only the fixed `/usr/libexec/tuna-perf-phase` helper. It marks the idle boundaries with the guest's CLOCK_BOOTTIME, matching the observer's own clock. Idle CPU excludes any interval crossing either boundary, and fewer than 20 complete intervals fail the measurement. Host phase timestamps remain separate provenance. The fixture sends notifications to the actual interactive user's Notifications service and requires forty distinct nonzero uint32 IDs (warm-up and measured rounds); the service's acceptance and first frame are retained. Acceptance is a workload record, not a claim of notification input-to-frame latency. Guest-agent packages, helper and service are fixture-only and installed identically in both sessions.

Input timings bound the response observed through QMP captures. Each observation records a conservative lower bound, upper bound and capture duration. QMP key injection, capture/poll overhead and host scheduling affect these measurements; they are **not** compositor input-to-frame tracing or a frame-pacing measurement. The software/guest path, only one boot, potentially unavailable DRM memory counters, and missing 24-hour soak remain limitations. The regression budgets below are GNOME-relative ratchets ([ADR 0009](../adr/0009-performance-regression-budgets.md)), not a release claim of parity.

Run a fixture disk with `scripts/tuna-vm-perf --disk disk.raw --desktop gnome --out /tmp/gnome-perf-fresh`, or select `tuna` for the candidate. Each artifact directory must be new. Build `packaging/marlin/perf/Containerfile` with `DESKTOP=gnome` or `DESKTOP=tuna` on the same shared preview image; it adds the test user, autologin, serial logging, observer and fixed guest probes. The fixture never ships in a desktop image.

The [2026-10-04 paired report](2026-10-04-marlin-gnome51/README.md) records the first successful run, including the higher sampled Tuna Desktop CPU and slower median observed overview response. Its package predates the latest roadmap work. Issue #73 remains open.

The paired lane also captures `wayland-info` from each actual interactive
Marlin session. The root observer invokes the tool as the fixture user and
retains the full output as digest-checked journal chunks; missing, partial,
duplicate or corrupted captures fail the run. Each desktop's artifact contains
`wayland-info.txt` and `wayland-info-source.json`, associated with the image,
package and observer provenance in that run. This supplies a native Marlin
protocol reference once a run passes; it does not replace the existing Fedora
headless reference without retaining and reviewing that actual evidence.
The comparison requires KVM and fails rather than accepting software emulation.

The GNOME overview phase also retains the installed shell's own
[Sysprof capture and decoded scope marks](gnome-overview-sysprof.md), with peer
credentials, capture timestamps and a verified digest. These are raw event,
dispatch and presentation scopes; causal latency remains a separate analysis.

The [native cadence and pure idle report](2026-10-04-native-cadence/README.md)
retains a later actual paired GNOME 51/Tuna Desktop run, including all 240 presentation
records per desktop, guest clock boundaries, ten accepted notifications each,
and raw resource samples. It identifies high Tuna Desktop idle CPU and slower median
presentation cadence in the older trusted package. It does not qualify later
repaint changes or close the remaining input tracing and soak requirements.

The [2026-10-08 CPU investigation](2026-10-08-cpu-investigation.md)
attributes the measured CPU to processes and workload phases, records the
overview-card and plane-damage fixes, and ranks the remaining work for #503.

The [2026-10-29 gate variance report](2026-10-29-gate-variance.md) (#782)
quantifies run-to-run variance per metric, records why the nightly lane
measured nothing from 2026-10-22, and traces Tuna's per-frame rasterizer
flushes to Smithay's per-draw texture fences.

## Regression gate (#315)

Tuna Desktop's target is to match or beat GNOME 51 on every metric below, and never to regress. `scripts/tuna-perf-gate` compares each paired run with the budgets in [`budgets.json`](budgets.json). The decision is recorded in [ADR 0009](../adr/0009-performance-regression-budgets.md).

### When it runs

- **Nightly** (03:23 UTC), on the latest trusted main package. Any enforced budget that regresses fails the run.
- **On dispatch**, from any branch. Run `gh workflow run performance-baseline.yml --ref <branch>`, optionally with `-f package_run=<ci run id>`. The run measures main's package with the branch's measurement code. It enforces budgets unless you pass `-f enforce=false`.
- **On a pull request that changes the measurement code**, the existing path trigger runs it in report-only mode.

It never runs on ordinary pull requests: one run takes about three hours on a KVM runner. The cheap part runs in every PR's `check` job: the gate's unit tests (`scripts/lib/tuna-perf-gate-tests.py`) and the ratchet check on `budgets.json`.

### Reading the report

The run's summary page and `perf-artifacts/perf-gate.json` in the `marlin-performance` artifact contain:

- **The budget table.** For each metric: the median over the selected stock-profile pairs of GNOME's value, Tuna's value, and Tuna / max(GNOME, floor), next to the budget ratio and the absolute ceiling. The status is one of these:
  - `pass`.
  - `fail`: over the ratio or the ceiling.
  - `missing`: fewer than three stock-profile pairs measured an enforced metric, which also fails.
  - `warn`: a provisional budget (`enforced: false`) is over its ratio or ceiling, or has too few pairs.
- **Per-role attribution, for each desktop.** The mean CPU of each phase, split into compositor, shell, xwayland, portals, services and apps. GNOME's compositor and shell are one process (`gnome-shell`), so compare Tuna's "desktop" role (compositor plus shell) against it.
- **A 90% confidence interval for every ratio.** A seeded block bootstrap (1000 replicates, blocks of three 1 s intervals) resamples the pairs and, inside each session, the phase's intervals or observations, and recomputes the gated statistic. The same artifact always gives the same interval. `unstable` marks a decision whose interval straddles the budget: the run cannot tell that metric's pass from its fail. The status still follows the point estimate, so the interval loosens nothing.
- **The host CPU model and refused sessions.** GitHub's KVM runners vary (EPYC 7763, EPYC 9V74, EPYC 9V45, Xeon 8370C), and some ratios move by host class; compare runs on the same class. A session whose observer wrote records over 32 KiB, or ran with per-thread diagnostics, is refused with its reason: such an observer perturbs what it measures (#782). Its pair then counts as unmeasured.

The JSON's `meets_target` separately compares the median paired ratio against the target without a denominator floor. Passing a floor-stabilized idle CPU budget does not establish parity. Two measured zero values meet the target; positive Tuna usage against measured zero GNOME usage does not.

Metric names are `cpu.<phase>.<role>.<stat>`, `overview.response_ms.<stat>` and `frame_pacing.interval_ms.<stat>`. CPU is in percent of one core, summed over the role's processes. Only intervals wholly inside a phase count. A process's first interval after it starts is not counted.

| Phase | Workload |
|---|---|
| `idle` | 30 s without input, 30 s after the panel first renders |
| `frames` | Native frame-pacer client presenting 240 frames |
| `typing` | Typing three app names into the search and launching them |
| `overview` | Ten overview open/close cycles |
| `workspaces` | Ten workspace switches each way (animations) |
| `window_cycling` | Ten Alt+Tab presses across three app windows |
| `notifications` | Three rounds of ten notifications one second apart (banner animations), after one unmeasured warm-up round |
| `settle` | 30 s without input, three apps open |

`cpu.whole.total.p50` matches the whole-run figure in older reports. Frame pacing uses the frame-pacer probe's presentation intervals. Overview response is the QMP-observed upper bound described above.

### How budgets ratchet

The file-level `profile` names the calibrated comparison population (`stock` for the initial budgets). Only that profile contributes to checks and proposals. Diagnostic results remain separate: they cannot fill missing stock repeats or mask a stock regression. The ratchet rejects changing this population in place.

Each budget has these fields:

- `max_ratio`: the Tuna / GNOME ratio it must stay under.
- `ceiling`: an absolute limit on Tuna's value, which may be null.
- `floor`: the smallest GNOME value used as a divisor, so 0.0% idle on GNOME does not make every ratio infinite.
- `enforced`, `evidence` (a run URL) and, optionally, `adr` and `note`.

The first budgets are the median ratios over the three stock pairs of [paired run 38910158684](https://github.com/tuna-os/tuna-desktop/actions/runs/38910158684) (trusted main package `78dc07cf`) plus 15%, never below the target ratio of 1.0. It is the first paired run with process names and guest-clock markers for every phase. Its per-phase medians and role attribution are in the [2026-10-15 attribution report](2026-10-15-cpu-parity-attribution.md#budget-baseline-paired-run-38910158684).

- **To tighten** after an improvement, take a passing run's artifact and run:

  ```
  scripts/tuna-perf-gate propose --artifacts perf-artifacts \
    --evidence https://github.com/tuna-os/tuna-desktop/actions/runs/<id> > budgets.new.json
  mv budgets.new.json docs/perf/budgets.json
  ```

  This lowers each budget to the measured ratio plus the margin, never below 1.0, and never raises one. When the run's 90% interval reaches above the point ratio, its upper bound is used instead, so one quiet run cannot set a budget inside its own noise. Commit the result with the run linked in the PR. Enforcing a provisional budget is also a tightening.
- **Loosening** (a higher ratio, ceiling or floor, dropping `enforced`, or removing a budget) needs a new ADR under `docs/adr/`, referenced from the budget's `adr` field, as the roadmap's change control requires. CI's ratchet check (`scripts/tuna-perf-gate ratchet --base <main's budgets.json>`) refuses any change that is not backed by new run evidence, and any loosening without that ADR.

Every budget in the baseline is enforced, since that run measured each one in three pairs. A budget added later without three measured pairs starts provisional (`enforced: false`) and is enforced from the first run that measures it.

### Running it locally

To check budgets against a downloaded artifact, run:

```
gh run download <run id> -n marlin-performance -D perf-artifacts
scripts/tuna-perf-gate check --artifacts perf-artifacts
```

`check` exits 1 on a regression. Add `--report-only` to only print the tables. A full paired measurement needs KVM, rootful podman and about 40 GB of disk. Follow `performance-baseline.yml` step by step: build the shared preview images, then run `scripts/tuna-perf-profile-pairs stock`, which writes the `perf-artifacts/stock` tree that `check` reads. On a workstation, `scripts/tuna-vm-perf --disk disk.raw --desktop tuna --out <dir>` measures one session, and its `report.json` includes the per-phase, per-role breakdown.

### Bounded per-thread idle diagnostics (diagnostic profile, opt-in)

The CI-only sampler takes `--thread-diagnostics` only when a dispatch sets
`thread_diagnostics: true`, and only for the diagnostic profile:
`scripts/tuna-perf-profile-pairs` refuses it for the gated stock profile and the
gate refuses any session recorded with it. Each record grows to about 45 KiB a
second, close to journald's 48 KiB line limit, and is forwarded to the serial
console. From its introduction (2026-10-22) until it became opt-in, every GNOME
session stalled (incomplete frame-pacer presentation, notification calls timing
out after 15 s, truncated sample lines) and the nightly lane measured nothing; see
the [2026-10-29 gate variance report](2026-10-29-gate-variance.md). The workload,
sample interval and budget evaluator are unchanged when it is enabled.

Each process sample may include numeric `thread_diagnostics`: TID, start ticks,
user/system CPU ticks and voluntary/involuntary context-switch counters.
These counters are **not wakeup counts** or causal stack traces. No thread names,
command arguments, environment, kernel addresses or arbitrary proc text are added.

Collection is bounded to 32 thread entries per process and 128 per sample,
including failed/racing reads. Oversized records, unavailable counters,
permission errors, exits and truncation are explicit. A process whose UID or
start ticks changes during collection is rejected by the existing identity gate.
Only compare thread deltas within the same process lifetime and identical
`(tid, start_ticks)`; do not sum thread CPU with process CPU or treat missing
counters as zero. Incomplete snapshots are diagnostic evidence, not complete
thread accounting. The caps can omit relevant tasks: absence from a snapshot
is not evidence of no scheduling activity. `collection_s` includes this observer
overhead.

Use the same retained guest idle-phase boundaries for both desktops. Existing
idle artifacts do not expose compositor frame counts, so these remain unavailable;
overview response traces and active frame-pacer measurements are not idle-frame
evidence. Scheduling attribution still requires a fresh paired capture, and a
counter correlation alone does not establish why a thread ran.

Failure evidence also retains a fixed collection call-site enum, numeric errno,
and PID/UID/start-tick/state-code identity before and after a failed observation
when readable. State codes are the ASCII value of the proc state character.
Identities also retain unsigned 32-bit `stat_flags`, nonnegative signed 32-bit
`stat_num_threads`, and numeric `status_vmrss_present`/`status_vmsize_present`
(1 for an exact field label, 0 for absent). Malformed/out-of-range stat metadata
is null; status presence is null for records exceeding 16,384 characters.
These reuse the existing proc reads without changing their input limits.
Presence does not validate field values, and no memory fallback is derived
from them. One combination does classify an exit: when `smaps_rollup` fails
with ESRCH and the re-read shows the same UID and start ticks with `PF_EXITING`
(0x4) set and both `VmRSS` and `VmSize` absent, the task is inside `do_exit()`
after `exit_mm()` and its memory is already released. It departs like an
observed zombie. Stock runs 40887247970 and 40890717926 lost whole sessions,
and with them every pair, to such tasks still reading S or R. Unknown flags or
presence, a held mm, a changed identity or any other error remain unreadable.
The read stages identify bounded Python proc operations, not a kernel syscall
trace. No exception text, paths or process names are added to these receipts.
At most 128 failure receipts are retained per sample, with an explicit overflow
count; the original unreadable list and complete-sample gate are not truncated
or relaxed. Explicitly observed exits remain excluded from accounting, with a
diagnostic receipt; persistent unreadable identities still fail.
