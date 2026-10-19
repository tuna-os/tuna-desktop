# ADR 0009: GNOME-relative performance budgets with a ratchet

- Status: Decided
- Decision date: 2026-10-16
- Owner: project
- Related issues: #315 regression gate, #503 CPU below GNOME, #73 performance evidence

## Context

Tuna Desktop must use no more CPU than GNOME 51, pace frames at least as well, and open the overview at least as fast, and it must not regress once it gets there. The paired lane (`performance-baseline.yml`) already measures GNOME 51 and Tuna Desktop on one runner, one image and one VM profile. It was manual and only reported. Absolute thresholds would fail on every runner change. A threshold set at the target would fail every night until the work in #503 lands, so nobody would read it.

The roadmap's change control says performance thresholds change only with repeated comparable evidence and a recorded ADR. This ADR introduces the thresholds.

## Decision

Budgets live in `docs/perf/budgets.json`. Each one is a metric for one workload phase, such as the overview phase's total user CPU or the overview response p95. The budget is a ratio to the GNOME 51 session in the same job, plus an optional absolute ceiling. The gate takes the median, over the job's completed stock-profile pairs, of Tuna / max(GNOME, floor). The floor keeps a near-zero GNOME value (idle CPU) from inflating the ratio.

The budget file names its calibrated `profile` (initially `stock`). Checks and proposals use only that profile; diagnostic captures remain available for attribution and cannot substitute for missing stock evidence. Changing the profile requires a separately calibrated budget rather than relabeling the existing gate.

The target ratio is 1.0. The first budgets are the median ratio over the three stock pairs of paired run 38910158684 (trusted main package `78dc07cf`) plus a 15% margin, never below 1.0. That is the first run of the paired route whose observer names every process and whose workload marks every phase on the guest clock; its medians are recorded in [docs/perf/2026-10-15-cpu-parity-attribution.md](../perf/2026-10-15-cpu-parity-attribution.md#budget-baseline-paired-run-38910158684). Absolute ceilings are raised to Tuna's measured value plus the margin where that run already exceeds them. From then on budgets only ratchet tighter:

- Tightening needs a newer paired run URL as `evidence`. `scripts/tuna-perf-gate propose` computes the tightened values.
- Loosening a ratio, ceiling, floor or enforcement, or removing a budget, also needs a new ADR referenced by `adr`.
- CI (`check` job) refuses any other change.

Every budget starts enforced, including the per-role "desktop" (compositor plus shell) metrics and the window-cycling phase, because the baseline run measured all of them in three pairs. A budget added later without three measured pairs is recorded with `enforced: false` and only warns; enforcing it is a tightening.

A nightly scheduled run measures the latest trusted main package and fails on any enforced budget. Pull requests do not run it, except the existing trigger on measurement-code changes, which only reports. The run can be dispatched on any branch.

## Alternatives

- **Absolute thresholds only.** Rejected: runner CPU and QEMU changes would move them, and they say nothing about GNOME parity.
- **Thresholds at the target (≤ GNOME) from day one.** Rejected: red every night until #503 is done, so a real regression would be invisible.
- **Fail only after two consecutive regressing nights** (the investigation report's proposal). Not adopted for now. The median over three stock pairs plus the margin absorbs one disturbed pair in one run. Diagnostic-profile pairs stay separate and cannot fill missing stock repeats or change its median. Revisit if nightly runs flap.
- **Run the paired lane on every PR.** Rejected: one run takes about three hours on a KVM runner, and the org shares 60 job slots.

## Consequences

A merged regression shows up the next morning, not at release time. The budgets record the current gap to GNOME in the open, and every tightening is tied to a run. The gate measures one virtual configuration with a software renderer. It does not replace hardware qualification or input-to-frame tracing. Overview response is a QMP-observed upper bound, as described in [docs/perf/README.md](../perf/README.md).
