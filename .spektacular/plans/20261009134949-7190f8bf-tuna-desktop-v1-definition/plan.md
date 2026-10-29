---
created_date: "2026-10-09"
document_status: final
closed_date: "2026-10-09"
---

# Plan: 20261009134949-7190f8bf-tuna-desktop-v1-definition

<!-- Metadata -->
<!-- Created: 2026-10-09T19:42:15Z -->
<!-- Commit: 1c48ad2d -->
<!-- Branch: roadmap/docs-baseline-ledger -->
<!-- Repository: https://github.com/tuna-os/tuna-desktop.git -->

<!--
  OVERVIEW
  A concise 2-3 sentence summary of the plan.
-->
## Overview

This plan turns the v1 definition — parity with GNOME 51 plus better responsiveness at matching resource cost — into an executable work list: close out the parity ledger and gap issues, prove the performance numbers with GPU-aware methodology, qualify the Marlin image end to end, and back the victory checklist with computed CI checks. It solves the "what is left" problem by giving every remaining item an owner, a lane, and a signal. Everyone driving the roadmap benefits, from gap-fix implementers to whoever finally declares v1.

<!--
  CONVENTIONS
-->
## Conventions

No project conventions apply to this feature.

<!--
  ARCHITECTURE & DESIGN DECISIONS
-->
## Architecture & Design Decisions

The victory machinery is a thin aggregation over tracking that already exists, not a new system. The spec's nine acceptance criteria resolve to four live instruments: the parity ledger in rust-wayland-desktop (`docs/parity-ledger.md`, currently 14 pass / 31 partial / 18 missing), the open `[GNOME 51 gap]` issues (~30, closed or ADR-deferred to reach zero), the paired benchmark lanes plus the #547 budget gate (extended with the CPU/GPU-split and power recording the spec demands), and the Marlin image qualification via #273 with the tunaos/tunaos-packages published-flavor lanes. `docs/v1.md` mirrors the checklist; the spec owns it.

The key trade-off is centralize-vs-federate: a single dashboard job that recomputes victory nightly versus leaving each tracker in place and defining v1 as a query over them. The chosen direction is federated with narrow glue — per-instrument owners and existing lanes stay authoritative, and small CI checks (gap count, ledger pass fraction, perf budgets, image qualification) turn the checklist from a manual page into a computed signal. This beats a central dashboard because every instrument already has an owner and a lane; a new aggregator would rot the moment any lane changed shape. It beats pure federation (spec text only) because without computed checks the list drifts stale within weeks. See research.md#alternatives-considered-and-rejected for the rejected options.

Per-repo placement: rust-wayland-desktop carries ledger closure, gap-issue work, benchmark methodology, and the docs mirror; tunaos and tunaos-packages carry image boot/qualification and published-flavor gates; corral carries VM validation runs. No referenced design constrains this plan (spec carries none).

<!--
  COMPONENT BREAKDOWN
-->
## Component Breakdown

- **Victory checklist (spec + docs mirror).** Owns the v1 definition and its checkable list. The spec copy is authoritative; the docs page mirrors it. Everything below reports into this list.
- **Parity ledger and gap-issue queue.** Owns capability tracking: every GNOME 51 capability carries a pass/partial/missing verdict with proof, and every confirmed gap is an issue worked to closed or ADR-deferred. Feeds the checklist's parity rows.
- **Paired benchmark lanes and budget gate.** Owns the responsiveness and resource evidence: like-for-like Tuna-vs-GNOME runs on the reference hardware/VM, with CPU/GPU cost split and power recorded, plus a nightly budget gate that fails on regression. Feeds the checklist's performance rows.
- **Image qualification lanes.** Owns the shipped-flavor gates in the image and packaging repos: boot, real-greeter login, published-flavor nightly, and recovery/upgrade paths. Feeds the checklist's image and safety rows.
- **Computed victory checks.** Small CI glue (gap count, ledger pass fraction, budget verdict, qualification verdict) that turns the checklist from a hand-maintained page into a computed signal. New — justified because a manual list drifts stale within weeks.
- **Recovery and upgrade proofs.** Owns the safety-net evidence: broken package/config recovery and upgrade-path runs with artifacts. Consumed by the checklist's safety row.

<!--
  DATA STRUCTURES & INTERFACES
-->
## Data Structures & Interfaces

Contracts between the victory components. Shapes are illustrative — field detail belongs in context.md.

```
VictoryVerdict {
  checklist_item   // one of the nine acceptance criteria
  status           // verified | open
  evidence         // link to proof, benchmark run, or qualification run
  owner            // who closes it
}
```

Carried from each instrument to the checklist; the computed checks aggregate these into the single signal.

```
BenchmarkResult {
  metric           // overview-open, input-reaction, animation, cpu, memory, power
  tuna, gnome      // paired values from the same run on identical hardware/image
  cpu_split        // cpu_ms, gpu_ms recorded separately
  witnesses        // trace/artifact links
}
```

The paired lane emits one record per metric; the budget gate passes only when responsiveness beats and resources match.

```
GapRecord {
  capability       // user-visible GNOME 51 behavior
  status           // open | closed | deferred(+decision link)
}
```

One per gap issue; zero open is the bar.

```
QualificationVerdict {
  flavor           // published Marlin image flavor
  boot, login      // real-greeter login pass/fail
  recovery         // broken package/config recovery pass/fail with data intact
  artifacts        // run links
}
```

Emitted by the image lanes; consumed by the image and safety checklist rows.

<!--
  IMPLEMENTATION DETAIL
-->
## Implementation Detail

The plan introduces one new pattern and otherwise rides existing ones. The new pattern is computed victory checks: small CI jobs that read the existing trackers (gap-issue count, ledger pass fraction, budget-gate verdict, qualification verdict) and publish a single machine-readable victory signal, so the checklist page reflects instrument state instead of editor discipline. A developer closing a gap flips a ledger row, closes its issue, and watches the signal recompute on the next run — no separate bookkeeping step.

Everything else follows established codebase patterns: proof gates per capability, nightly benchmark and qualification lanes, per-PR ledger updates, and issue-driven gap work with ADR-backed deferrals. No package splits, no new module boundaries, no interface replacements; the only new surface is the narrow check glue, which lives with the lanes it reads.

<!--
  DEPENDENCIES
-->
## Dependencies

- Program specs 002, 003, 004, 006 (shell workflow, hardware/app compat, secure session/a11y, perf/packaging/release) — the scope this plan drives to done; no changes needed, acceptance required.
- Spec 005 (extensions) — explicitly excluded; tracked only to prevent scope creep.
- Parity ledger and gap-issue queue — the instruments; ledger rows gain pass verdicts as work lands, no structural change.
- Paired benchmark lanes and the #547 budget gate — extended with CPU/GPU-split and power recording before they can carry the verdict.
- Marlin image lanes (#273) and published-flavor pipelines — must qualify flavors; no changes to their shape assumed.
- Corral VM validation — consumes the qualification runs; unchanged.
- Design documents this plan was built on: none.

<!--
  TESTING APPROACH
-->
## Testing Approach

This plan adds almost no new tests of its own: the work is making existing lanes carry the v1 verdict. Coverage concentrates on the glue — the computed checks — because the capability proofs, benchmark runs, and qualification lanes already exist and are owned elsewhere. Load-bearing guarantees, in plain language: the checklist cannot claim a capability its proof did not pass; a new gap issue breaks the signal until closed or deferred; a perf regression fails the gate before it reaches the list.

Metric mapping from the spec's Success Metrics: overview/input/animation beating GNOME and CPU/memory/power matching it are behavioural tests — assertions inside the paired benchmark lanes and the nightly budget gate. Gap count zero and checklist-verified are behavioural tests — the computed checks over the issue tracker and ledger. CPU/GPU-split recording is a behavioural test — runs without the split fields fail validation. Image qualification and recovery are behavioural tests through their existing lanes where automated; any human-observed portion is Manual — captured in the implementation test plan. Checklist findability ("one click from the front door") is Manual — captured in the implementation test plan. Deliberate gap: no new unit tests for tracker schemas — the checks read live tracker state, and unit fixtures would only test the mock.

<!--
  MILESTONES & TASKS
-->
## Milestones & Tasks

### M1 — Victory list goes live
**What changes**: the v1 definition becomes something the whole project can watch. The checklist appears in the spec and on its docs mirror, and the first computed checks start reporting gap count, ledger pass fraction, budget verdict, and qualification verdict as a single signal.

#### - [x] Task: Computed victory checks
**Id:** cc17b7c9-4bc5-478b-a70f-856fecc07076
**Repo:** rust-wayland-desktop
**Depends on:** none
**Execution:** agent

Add the small CI checks that turn the victory checklist into a computed signal: open gap count, ledger pass fraction, benchmark budget verdict, and image qualification verdict, published where the checklist can quote them.

*Technical detail:* [context.md#task-computed-victory-checks](./context.md#task-computed-victory-checks)

**Acceptance criteria**:
- [x] Each checklist row shows a live computed status, not hand-edited text.
- [x] A new gap issue or ledger regression visibly breaks the signal.

#### - [ ] Task: Front-page victory link
**Id:** 52b0a0f3-e004-4b37-a26b-f6c14e16c3f0
**Repo:** rust-wayland-desktop
**Depends on:** none
**Execution:** agent

Link the victory checklist from the project's front page so a newcomer finds it in one click and can name what remains without asking anyone.

*Technical detail:* [context.md#task-front-page-victory-link](./context.md#task-front-page-victory-link)

**Acceptance criteria**:
- [ ] A reader landing on the front page reaches the checklist in one click.

### M2 — Every capability proves itself
**What changes**: the long close-out. Ledger rows move to pass with proofs, gap issues close or carry recorded deferrals, and the daily-driver surface stops needing GNOME anywhere.

#### - [ ] Task: Ledger close-out to all-pass
**Id:** 9c7bd885-8997-49ea-b4d3-d7e63f702072
**Repo:** rust-wayland-desktop
**Depends on:** none
**Execution:** agent

Drive every parity ledger row to pass with its proof linked: the partial and missing rows close one by one through normal capability work, each landing with tests and gates.

*Technical detail:* [context.md#task-ledger-close-out-to-all-pass](./context.md#task-ledger-close-out-to-all-pass)

**Acceptance criteria**:
- [ ] The ledger shows zero partial and zero missing rows.
- [ ] Every pass row links its proof.

#### - [ ] Task: Gap-issue burndown to zero
**Id:** d2d97f00-ed50-4306-91c1-908f87d5f74d
**Repo:** rust-wayland-desktop
**Depends on:** none
**Execution:** agent

Close or ADR-defer every open gap issue, rerunning the audit until it finds no untracked user-visible capability.

*Technical detail:* [context.md#task-gap-issue-burndown-to-zero](./context.md#task-gap-issue-burndown-to-zero)

**Acceptance criteria**:
- [ ] No gap issue remains open without a recorded deferral decision.
- [ ] The latest audit report records no untracked capability.

### M3 — Numbers beat GNOME and the image qualifies
**What changes**: the performance story becomes evidence instead of aspiration. Benchmark runs record the CPU/GPU split with power, responsiveness beats GNOME 51 on identical hardware while resources match, the nightly budget gate holds green, and the published Marlin flavor qualifies through real-greeter login with recovery proven.

#### - [ ] Task: GPU-split benchmark methodology
**Id:** 3f92f7f9-65dc-4d82-8adb-f336ee30727a
**Repo:** rust-wayland-desktop
**Depends on:** none
**Execution:** agent

Extend the paired benchmark lanes so every run records CPU and GPU cost separately with total power on identical hardware and image family, making the verdict meaningful whatever the compositor moves between them.

*Technical detail:* [context.md#task-gpu-split-benchmark-methodology](./context.md#task-gpu-split-benchmark-methodology)

**Acceptance criteria**:
- [ ] Every published run carries CPU/GPU-split numbers plus total power.
- [ ] Methodology doc states the reference hardware and image family.

#### - [ ] Task: Nightly perf budget gate
**Id:** 0b1bd08c-b6d6-4cde-af1d-587a206a98c8
**Repo:** rust-wayland-desktop
**Depends on:**
- 3f92f7f9-65dc-4d82-8adb-f336ee30727a — GPU-split benchmark methodology
**Execution:** agent

Complete the nightly GNOME-relative budget gate: responsiveness beats GNOME, resources match, with per-phase and per-process attribution, failing loudly on regression.

*Technical detail:* [context.md#task-nightly-perf-budget-gate](./context.md#task-nightly-perf-budget-gate)

**Acceptance criteria**:
- [ ] The gate runs nightly and fails on responsiveness or resource regression.
- [ ] Failures attribute to phase and process.

#### - [ ] Task: Image qualification end to end
**Id:** 9d47fee8-6dec-406f-a3f4-8427b56a29af
**Repo:** tunaos
**Depends on:** none
**Execution:** agent

Qualify the Marlin image flavor through install, boot, and real-greeter login on the published nightly, with recovery and upgrade paths proven and artifacted.

*Technical detail:* [context.md#task-image-qualification-end-to-end](./context.md#task-image-qualification-end-to-end)

**Acceptance criteria**:
- [ ] The published flavor passes install, boot, and greeter login nightly.
- [ ] Broken package and config recovery is proven with data intact.

#### - [ ] Task: Published-flavor gates
**Id:** 80eae871-1c4e-435e-ade4-4f323ff62a44
**Repo:** tunaos-packages
**Depends on:** none
**Execution:** agent

Gate published packages so only qualified flavors ship: provenance, manifests, and test results attached to every release.

*Technical detail:* [context.md#task-published-flavor-gates](./context.md#task-published-flavor-gates)

**Acceptance criteria**:
- [ ] Every published flavor carries provenance and qualification results.
- [ ] An unqualified flavor cannot publish.

#### - [ ] Task: VM validation runs
**Id:** f5110c0e-71df-47d7-aea5-4f2963251553
**Repo:** corral
**Depends on:**
- 9d47fee8-6dec-406f-a3f4-8427b56a29af — Image qualification end to end
**Execution:** agent

Run the image qualification workloads under VM validation so boot, login, recovery, and performance claims hold on virtual hardware too.

*Technical detail:* [context.md#task-vm-validation-runs](./context.md#task-vm-validation-runs)

**Acceptance criteria**:
- [ ] Qualification verdicts exist for VM runs alongside hardware runs.
- [ ] A VM-only regression fails the signal.

#### - [ ] Task: Declare v1
**Id:** 0e29d234-9da9-4864-bc70-b9cf531ec386
**Repo:** rust-wayland-desktop
**Depends on:**
- cc17b7c9-4bc5-478b-a70f-856fecc07076 — Computed victory checks
- 52b0a0f3-e004-4b37-a26b-f6c14e16c3f0 — Front-page victory link
- 9c7bd885-8997-49ea-b4d3-d7e63f702072 — Ledger close-out to all-pass
- d2d97f00-ed50-4306-91c1-908f87d5f74d — Gap-issue burndown to zero
- 3f92f7f9-65dc-4d82-8adb-f336ee30727a — GPU-split benchmark methodology
- 0b1bd08c-b6d6-4cde-af1d-587a206a98c8 — Nightly perf budget gate
- 9d47fee8-6dec-406f-a3f4-8427b56a29af — Image qualification end to end
- 80eae871-1c4e-435e-ade4-4f323ff62a44 — Published-flavor gates
- f5110c0e-71df-47d7-aea5-4f2963251553 — VM validation runs
**Execution:** human — stakeholder decision to declare the release

Review the fully green victory signal and declare v1, recording the decision and the exact qualified revisions.

*Technical detail:* [context.md#task-declare-v1](./context.md#task-declare-v1)

**Acceptance criteria**:
- [ ] The declaration names the qualified revisions and is recorded.

<!--
  OPEN QUESTIONS
-->
## Open Questions

- Whether the CI environment can query open gap-issue count and the budget verdict without new API permissions — depends on the check job's runtime context. If not, the implementer should compute the signal from checked-out tracker state inside the job instead, and note the substitution.
- Whether the #547 budget gate's per-process attribution already separates GPU cost — depends on its current implementation. If not, the methodology task extends it; the implementer should not invent a parallel attribution scheme.

<!--
  OUT OF SCOPE
-->
## Out of Scope

- Extension broker and public API (spec 005): stays post-1.0 unless explicitly promoted; tracked in its own spec, not this plan.
- Closing individual ledger rows and gap issues beyond the burndown tasks' scope: each row's capability work runs as normal feature work; this plan tracks the aggregate signal, not each fix.
- Reconciling the v1 performance bar with spec 006 R2 (beat/match vs ≤10% regression): flagged in research; needs stakeholder sign-off if the two ever conflict, tracked as a plan-level decision, not a task.

## Changelog

### 2026-10-09 — Task: Computed victory checks

**What was done**: Added a nightly informational workflow and script that publish the v1 victory signal (ledger pass fraction, open gap count, perf/image verdicts pending on #547/#273), with tests wired into the check job (PR #565).

**Deviations**: None.

**Files changed**:
- `rust-wayland-desktop: scripts/victory-signal.py`
- `rust-wayland-desktop: scripts/lib/victory-signal-tests.py`
- `rust-wayland-desktop: .github/workflows/victory-checks.yml`
- `rust-wayland-desktop: .github/workflows/ci.yml`

**Discoveries**: GitHub search API returns Not Found for this token — `gh issue list --search` works instead. Manually dispatched workflow runs require the file on the default branch, so pre-merge validation is local-only.
