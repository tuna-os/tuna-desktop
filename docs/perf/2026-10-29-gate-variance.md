# Why the Marlin performance gate was not reproducible (2026-10-29, #782)

This report covers the paired runs named in #782 and the nightly runs since.
It covers 81 retained sessions from runs 38910158684, 38956044285,
39186104488, 39274768751, 39314384040, 39395305537 and 39571361727. It also
covers the failed nightlies 39750119744 through 40855609152. The analysis uses
`scripts/tuna-perf-gate` and `scripts/lib/perf_budget.py` on the downloaded
`marlin-performance` artifacts.

## 1. The nightly lane measured nothing from 2026-10-22

Every scheduled run from 39750119744 (2026-10-22) to 40667441920 (2026-10-28),
and every pull-request run of #794, failed before a single stock pair
completed. The GNOME session failed in a different way in different runs:

- The frame pacer presented only 122–215 of 240 frames in 15 s. A healthy
  run presents all 240 in 3.9–5.2 s.
- `Notify` timed out after 15 s.
- `gnome-trace-start` failed or timed out.
- "resource samples are missing or incomplete".

The image, package set and runner image were unchanged (`baseline-packages.txt`
is identical in 39571361727 and 40667441920). Only the observer changed: #794
added `--thread-diagnostics` to `tuna-perf.service`. The observer prints one
JSON record a second to journald, and journald forwards it to the serial
console (`ForwardToConsole=yes`, `console=ttyS0`).

| Observer | Median record | Largest | Collection per sample | Unparseable records |
|---|---:|---:|---:|---:|
| Without thread diagnostics (39571361727) | 14.4 KB | 26.9 KB | 0.07 s | 0–2 per session |
| With thread diagnostics (40667441920) | 44.9 KB | 48.0 KB | 0.15 s | 4 per session |

Records at journald's 48 KiB `LineMax` are split, and pushing ~45 KB/s through
the emulated UART stalls the guest. The observer was perturbing the sessions
it measured.

**Fix.** Thread diagnostics are opt-in (`thread_diagnostics` dispatch input)
and only for the diagnostic profile. `tuna-perf-profile-pairs` refuses them for
`stock`. The gate refuses any session recorded with them or with a record over
32 KiB, so such a pair counts as unmeasured. Tests:
`tuna-perf-repeatability-tests.py` (`test_gated_stock_observer_never_records_thread_diagnostics`,
`test_thread_diagnostics_are_refused_for_stock_before_any_acquisition`) and
`tuna-perf-gate-tests.py` (`Observer`).

**A second cause: exiting tasks counted as unreadable.** With thread
diagnostics off, runs 40887247970 and 40890717926 still measured 0 and 1 of 3
stock pairs. Each lost a Tuna session to one sample in about 262: a user process
failed `smaps_rollup` with ESRCH and the re-read showed the same task in S or R
with `PF_EXITING` set and no `VmRSS`/`VmSize`. The task was inside `do_exit()`
after `exit_mm()` but before it became a zombie. The sampler accepted only Z/X
as an exit, so it reported the task as unreadable. `tuna-vm-perf` then refused
the session, and `tuna-perf-profile-pairs` aborts on the first refused session.
That exact state now departs like a zombie; every other failure still refuses
the session (`tuna-perf-tests.py`,
`test_only_an_exiting_task_without_mm_departs_from_a_live_state`).

## 2. The #782 "pass" and "fails" were not budget decisions

Runs 39186104488 and 39274768751 failed in the *diagnostic* acquisition step
("actual kernel observer failed"), not in the gate. They were pull-request runs,
and pull-request runs only report. Run 39314384040 "passed" only because it was
report-only too. Under the current budgets its stock pairs fail
`cpu.notifications.desktop.mean` (3.64) and `cpu.workspaces.total.mean` (1.35).
Re-checking every retained run against `budgets.json`:

| Run | Host CPU | Failing budgets |
|---|---|---|
| 38910158684 (calibration) | EPYC 7763 | none |
| 38956044285 | EPYC 7763 | notifications desktop 3.32, notifications total 2.17 |
| 39186104488 | EPYC 9V74 | settle desktop 2.82; frames missing (2 pairs) |
| 39274768751 | Xeon 8370C | notifications desktop 3.58, typing desktop 1.80, workspaces desktop 1.39, workspaces total 1.35 |
| 39314384040 | EPYC 7763 | notifications desktop 3.64, workspaces total 1.35 |
| 39395305537 | EPYC 7763 | idle total p95 3.00 |
| 39571361727 | EPYC 9V45 | notifications desktop 5.04, notifications total 2.70, typing desktop 1.80 |

The same revision of the measurement code gives a different set of failing
metrics on every run.

## 3. Sources of variance, quantified

**The budgets sit at the measured mean.** The budgets are the calibration
run's medians plus 15%. That run was a low draw for notifications (2.82).
Over the 12 stock pairs on the EPYC 7763 class:

| Metric | Budget | Mean of per-pair ratios | SD | Pairs over budget |
|---|---:|---:|---:|---:|
| `cpu.notifications.desktop.mean` | 3.25 | 3.25 | 0.66 | 42% |
| `cpu.notifications.total.mean` | 2.15 | 2.03 | 0.28 | 33% |
| `cpu.workspaces.total.mean` | 1.35 | 1.29 | 0.11 | 42% |
| `cpu.workspaces.desktop.mean` | 1.35 | 1.24 | 0.11 | 17% |
| `cpu.settle.desktop.mean` | 2.75 | 2.45 | 0.39 | 17% |
| `cpu.idle.total.p95` | 2.30 | 1.96 | 0.75 | 25% |
| `cpu.typing.desktop.mean` | 1.75 | 1.55 | 0.11 | 0% |

With the true notification ratio equal to its budget, a median of three pairs
passes about half the time. No acquisition method can make that decision
reproducible. Only lowering Tuna's notification CPU can.

**Short phases are noisy because they are short.** The notifications phase
gave 12 whole 1 s intervals. Across 21 EPYC 7763 sessions, the spread of
GNOME's per-session notification mean (SD 2.1 points, CV 19%) is what the
intervals' own sampling noise predicts (expected SE 2.6). The spread is
measurement noise, not boot-to-boot drift. The same holds for workspaces,
overview and typing (ratio of observed to expected spread 0.3–1.0).

**The host class moves the ratios.** GitHub's KVM runners were EPYC 7763,
EPYC 9V74, EPYC 9V45 and Xeon 8370C. On the 9V45, GNOME's notification CPU
falls to 7.1% against 11.3% on the 7763, while Tuna's barely moves (28 vs 35).
The ratio then reaches 5.0. Typing on the Xeon and the 9V45 gives 1.80 against
1.47–1.67 on the 7763. Pairing within a run removes only part of this.

**Quantization.** `cpu.idle.*.p95` takes whole percents (1 s intervals at
100 Hz ticks), so its ratio jumps in steps of 1/1, 2/1 and 3/1. A budget of 2.3
fails whenever one pair's p95 reaches 3%.

**Serial console load.** Even without thread diagnostics, the observer writes
about 14 KB/s for GNOME and about 10 KB/s for Tuna to the serial console. About
a third of sessions lose one or two garbled records. This load runs as root,
outside the measured user scope, so it is not counted. It is a residual
asymmetric perturbation. The console drops runs of bytes inside a line: in 20
sessions of runs 40887247970–40914419860, 18 of 5,430 records were damaged.
One of them still parsed, with a process missing `cpu_ticks`, and crashed the
session's summary (40914419860). Each record now carries a SHA-256 of its
JSON, and `tuna-vm-perf` drops any record whose digest does not match, as it
already dropped unparseable ones.

**Not measured yet.** #820 (gsd plugins in the Tuna session) merged after every
run analysed here. Its plugins count as `services`, so they affect the `total`
metrics, not `desktop`. The first runs with main's package after #820 are
listed below.

Pinning CPUs was not applied. The runner has four logical CPUs (two cores with
SMT) and the guest has four vCPUs, so there is no spare CPU to isolate QEMU or
the host harness on. Changing the VM profile would invalidate every calibrated
budget.

## 4. Methodology changes (no budget loosened)

- Thread diagnostics are off in the gated profile, and the gate refuses
  perturbed sessions (section 1).
- The notifications phase is one unmeasured warm-up round of ten, then three
  measured rounds of ten. That gives about 36 intervals instead of 12, and the
  first-banner and first-screenshot costs fall outside the window.
- Every gate row has a seeded block-bootstrap 90% interval (1000 replicates,
  blocks of three intervals, over pairs and intervals). A decision whose
  interval straddles the budget is marked `unstable`. Statuses still follow
  the point estimate. Applied to the retained runs, every failure in the table
  above is `unstable`, except notifications on the EPYC 9V45 run
  (interval 4.26–6.79).
- `propose` tightens only to the interval's upper bound plus the margin, so a
  quiet run cannot set a budget inside its own noise. This is the guard
  against the failure mode that created the current notification budget.
- The gate reports the host CPU model.

## 5. Where Tuna's notification and settle CPU goes

Per-thread counters exist from the #794 runs (39530935434, 39539861809). The
gate refuses these sessions, but they serve as diagnostics. In the final settle
the compositor's four llvmpipe rasterizer threads each wake about 84 times a
second (1.8% each), and the main thread uses about 1%. gnome-shell's
rasterizer threads wake about 21 times a second (0.3–0.4% each), with System
Monitor drawing in both sessions. During notifications Tuna's threads wake
about 160 times a second against GNOME's 28–47.

A local replica reproduces the pattern. It ran release `tuna-compositor` nested
on Xvfb with llvmpipe and a GTK4 client committing four frames a second. The
compositor drew one frame per commit, but each frame woke every llvmpipe thread
about 4.5 times. `perf record -e sched:sched_wakeup --call-graph dwarf`
attributes 63 of 64 rasterizer wakeups to
`smithay::backend::renderer::gles::texture::TextureSync::update_read`, called
from `GlesFrame::render_texture_from_to`. Smithay creates a `glFenceSync` after
every texture draw. llvmpipe flushes and rasterizes its scene for each fence,
so every frame is rasterized once per drawn element instead of once. Upstream
Smithay (85de6d6, 2026-10-28) does the same. Mutter does not fence per draw.

The fix belongs in the vendored `third-party/smithay`: collect the textures
read in a frame and create their read fences once, in
`GlesFrame::finish_internal`. Tuna creates no shared GL contexts
(`drm.rs` and `night_light.rs` each create one context), so in-context GL
ordering already covers mid-frame writes. This is not yet implemented or
measured.

The shell's notification cost is a separate item. Each arrival and expiry
rebuilt every card of every notification group in the calendar's history list,
even with the calendar closed. With the warm-up and measured rounds, one group
holds up to 40 cards. The list is now built only while it is mapped, and it
catches up when the calendar opens.
