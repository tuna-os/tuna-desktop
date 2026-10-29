---
created_date: "2026-10-10"
document_status: draft
---

# Plan: 20261009134949-7190f8bf-0001-tuna-desktop-v1-definition

<!-- Workflow: 20261009134949-7190f8bf-tuna-desktop-v1-definition -->
<!-- Created: 2026-10-10T13:27:05Z -->
<!-- Commit: 1c48ad2d -->
<!-- Branch: roadmap/docs-baseline-ledger -->
<!-- Repository: https://github.com/tuna-os/tuna-desktop.git -->

**Originating specification:** `20261009134949-7190f8bf-tuna-desktop-v1-definition`. The plan filename adds the approved `0001` slug; its specification retains the original workflow identifier.

## Overview

**Approved scope reconciliation (2026-10-17):** `docs/adr/0008-v1-beta-track-and-release-scope.md` records the owner's existing decisions: all TunaOS variants, Marlin as GNOME 51 comparison baseline, and waived GPU/total-power comparisons. Hardware-only P-DS-04, P-IN-05, P-DS-03, P-SY-09, P-LK-02 and modem portions of P-ST-10 still require implementation and proof; emulated-device proof is acceptable when labelled, while physical validation is deferred to the post-v1 matrix. Beta qualification is not v1 qualification. All nine tasks remain unchecked; stable task IDs/titles are retained.

This plan takes Tuna Desktop from a partially proven desktop to a qualified v1 daily driver across TunaOS variants. It closes the GNOME 51 capability and gap bar, proves faster matched responsiveness without higher guest CPU or memory, and qualifies immutable published Tuna Desktop images through install, real-greeter session, and variant-specific proof. Marlin remains the GNOME 51 comparison baseline and carries upgrade/recovery qualification. Contributors and release owners get one public checklist backed by current evidence, so they can see exactly whether v1 is ready to declare.

## Conventions

No project conventions apply to this feature.

## Architecture & Design Decisions

Use a federated evidence model: each repository remains authoritative for its own capability, performance, package, image, or VM proof. The desktop victory signal reads the newest verified results, records their revisions, and reports pass, fail, pending, or unknown; missing, stale, or inaccessible evidence never passes. This keeps the existing ledger and qualification lanes as sources of truth without creating a second dashboard (research.md#alternatives-considered-and-rejected).

In rust-wayland-desktop, finish the GNOME 51 ledger and gap audit, complete the no-fallback daily-driver journey, and qualify paired responsiveness and resource use. ADR 0008 supersedes the original GPU/total-power comparison requirement: faster response and guest CPU/memory at or below GNOME remain required on matched hardware and image family. Retain exact baseline and run provenance; GPU/power diagnostics, if available, do not qualify superiority or gate v1.

The published Marlin flavor uses only `marlin:tuna`, per the user's decision to drop `marlin:roost` now. ADR 0008 extends delivery qualification to every TunaOS variant: each supplies its own immutable image digest and install/real-greeter/session/portal/lock verdict, or an explicitly recorded unsupported reason. tunaos-packages supplies traceable published package evidence; TunaOS owns the flavor and image promotion; desktop lanes verify install, real-greeter login, upgrade, and data-preserving recovery; Corral reports its own VM verdict. The final public checklist aggregates those source-owned results and includes the owner's v1 declaration after every criterion passes.

## Component Breakdown

- **Capability ledger and gap audit:** tracks every GNOME 51 behavior, proof, and issue disposition.
- **Daily-driver journey:** proves login, core work, settings, notifications, lock/unlock, and logout stay in Tuna Desktop.
- **Paired performance gate:** compares matched GNOME/Tuna responsiveness and guest CPU/memory with pinned provenance; GPU/total-power comparisons are waived by ADR 0008.
- **Package and variant qualification:** verifies published packages and immutable Tuna images for every TunaOS variant, including real-greeter/session/portal/lock proof; Marlin additionally qualifies upgrade and data-preserving recovery; no `marlin:roost` alias.
- **Corral VM qualification:** independently boots the published image and emits a machine-readable verdict.
- **Victory signal and public checklist:** aggregates current source-owned evidence and shows the nine v1 criteria from the project front page.
- **Owner declaration:** records the exact qualified revisions after all automated gates pass; review remains a manual test-plan item, not an implementation task.

## Data Structures & Interfaces

Use source-lane artifact formats where possible. The checklist needs only this shared contract:

```text
Verdict = { status: pass | fail | pending | unknown,
            revision, run_url, artifacts[], note }
Criterion = { id, verdicts[] }
BenchmarkPair = { workload, tuna, gnome, units, cpu, memory,
                  gpu?, total_power?, hardware, image_family, revisions, run_url }
ImageQualification = { flavor, image_digest, package_versions[],
                       install, boot, greeter, recovery, upgrade,
                       user_data_intact, run_url, artifacts[] }
VictorySignal = { generated, source_revisions[], criteria[],
                  ledger_counts, open_gap_count, overall_status }
```

Every criterion links immutable evidence and exact revisions. Missing, stale, or inaccessible results stay pending/unknown; the aggregator maps lane verdicts and does not recompute domain-specific checks.

## Implementation Detail

Finish each existing proof lane in its owning repository and publish a revision-bound verdict. The desktop signal consumes those verdicts, computes the nine v1 statuses, and renders the public checklist; it treats absent, stale, or failed artifacts as non-green.

Extend the existing ledger, journey, paired benchmark, package publisher, Marlin flavor, image qualification, and Corral VM runner. Do not duplicate domain checks in the aggregator. The release path is `marlin:tuna` with a tested data-preserving switch; the old `marlin:roost` alias is dropped immediately. All performance claims retain matched hardware/image identity and guest CPU/memory evidence. GPU/total-power comparisons are waived for v1; no GPU/power superiority claim follows. The Marlin-named delivery tasks retain their stable IDs/titles but include all-variant qualification under ADR 0008.

## Dependencies

- **GNOME 51 Marlin baseline:** exact reference image, package set, and matched hardware for parity and performance claims.
- **Parity ledger and `[GNOME 51 gap]` issues:** capability inventory, proof references, dispositions, and fresh audit; all counts rechecked during implementation.
- **GitHub Actions and artifacts:** authoritative run verdicts for performance, package, image, and Corral lanes; inaccessible evidence stays non-green.
- **Performance lane (#547, merged 2026-10-19):** the nightly GNOME-relative budget gate (`Marlin performance samples`) exists but is red on main and intermittent (#782); `victory-checks` still reports the perf verdict as pending. Needs matched latency and guest CPU/memory evidence with pinned baseline identity and a passing v1 verdict; GPU/total-power comparisons are waived by ADR 0008.
- **TunaOS flavor (#3107, merged 2026-10-11):** `marlin:tuna` is the published flavor and `marlin:roost` is no longer published.
- **tunaos-packages (#810, merged):** published and verified package source `d443858`; extend provenance evidence, do not rebuild the already-published package. Only the Arch target is packaged; the other native package targets are tracked on epic #577.
- **Published-image lane (#273, merged 2026-10-16):** runs nightly as `marlin-tuna-nightly`; its `published-flavor` job has failed on every main run since at least 2026-10-23, and `victory-checks` reports the image verdict as pending.
- **Corral VM runner:** runs on host `karnataka`; the AWS KubeVirt cluster is decommissioned. Generic layer and KVM checks exist; the image-specific verdict depends on the published Tuna image.
- **Related specs:** 002, 003, 004, and 006 provide program context; v1's stricter performance target stands. Extension broker/API spec 005 is out of scope. The v1 spec references no design document.

## Testing Approach

Use contract tests for ledger references, verdict parsing, provenance, and missing/stale evidence; integration tests for gap, package, image, benchmark, and Corral result ingestion; and end-to-end journeys for daily use and published-image recovery. Every result must identify the exact source revision, package/image, workload, and run.

The success checks are: all ledger rows pass with proof and the gap audit is clean; the daily-driver journey finishes without GNOME; paired overview, input, and animation response beat GNOME 51; guest CPU and memory stay at or below GNOME. GPU/total-power comparisons are waived by ADR 0008. Incomplete required pairs or missing image evidence cannot pass; every TunaOS variant needs its own qualification or recorded unsupported reason.

Manual acceptance items, captured in the implementation test plan:

- A new contributor finds the checklist from the front page in one click and can identify remaining work.
- A fresh reviewer checks for untracked user-visible GNOME behavior and confirms proof links resolve.
- The release owner records the exact qualified revisions and final v1 declaration.
- Any physical-device check unavailable in CI records the device and run; a green build alone is not qualification evidence.

## Milestones & Tasks



### Milestone 1: Daily use covers the GNOME 51 capabilities without fallback

**What changes**: Tuna Desktop supports the full set of in-scope GNOME 51 capabilities needed for daily use, with a passing proof for every ledger row and no open GNOME gap. The everyday journey can be completed on Tuna Desktop without switching to GNOME.

**Validation point**: The capability ledger is fully passing, a fresh gap audit finds zero open GNOME 51 gaps, and the complete daily-driver journey passes without invoking the GNOME session.

#### - [ ] Task: Make every GNOME 51 capability pass with proof
**Id:** 3a5dd52d-a432-4a40-8136-bb72a35dedc1
**Repo:** rust-wayland-desktop
**Depends on:** none
**Execution:** agent

Finish the partial and missing ledger rows with reproducible proofs. Reconcile confirmed gaps and produce a fresh audit with no untracked behavior.

**Owner-approved CI acceleration supporting work (2026-10-17):** Continue inside the active v1 workflow. Split lightweight lane selection from expensive workspace checks; preserve successful-check admission where required, immutable source/package provenance and the final gate. Seed job/variant-specific dependency caches only from trusted main, restore them on pull requests, and cover standalone and isolated proof targets plus the native Arch build. Existing selected/full proof coverage, assertions, deadlines, main-run completion and performance-baseline admission stay unchanged. Verify the actual workflow and cache behavior; report observed timing separately from counterfactual overlap. ORAS, paid quota changes, live-session snapshots and unrelated product fixes are not part of this change.

*Technical detail:* [context.md#task-make-every-gnome-51-capability-pass-with-proof](./context.md#task-make-every-gnome-51-capability-pass-with-proof)

**Acceptance criteria**:
- [ ] Every tracked capability passes with proof; confirmed gap issues are fixed or explicitly dispositioned, the open list is empty, and the fresh audit finds no untracked behavior.

#### - [ ] Task: Complete the daily-driver journey on Tuna Desktop
**Id:** e89a76d3-be6c-4de5-ac0f-a2924eadfefb
**Repo:** rust-wayland-desktop
**Depends on:**
- 3a5dd52d-a432-4a40-8136-bb72a35dedc1 — Make every GNOME 51 capability pass with proof
**Execution:** agent

Prove the full login-to-logout daily flow in Tuna Desktop. The journey must not launch or fall back to GNOME.

*Technical detail:* [context.md#task-complete-the-daily-driver-journey-on-tuna-desktop](./context.md#task-complete-the-daily-driver-journey-on-tuna-desktop)

**Acceptance criteria**:
- [ ] The real-greeter journey completes launch, switching, notifications, settings, lock/unlock, and logout without a GNOME session.


### Milestone 2: Tuna Desktop meets the GNOME 51 responsiveness and resource bar

**What changes**: On the same hardware and image family, Tuna Desktop feels more responsive than the GNOME 51 baseline while using no more guest CPU or memory. GPU/total-power comparisons are waived for v1. The performance evidence identifies the exact published and derived baseline inputs used for the comparison.

**Validation point**: A paired run on the documented matched profile passes the v1 latency and guest CPU/memory checks against the pinned GNOME 51 baseline, with reproducible artifacts and a green performance verdict.

#### - [ ] Task: Qualify performance against GNOME 51
**Id:** bb94fd76-fadc-4ff9-a9d7-dd4e2dfa8a22
**Repo:** rust-wayland-desktop
**Depends on:** none
**Execution:** agent

Complete the paired performance lane on identical hardware and image family. Report responsiveness, guest CPU/memory, and exact baseline provenance. Preserve available GPU/power diagnostics without making a comparison or superiority claim.

*Technical detail:* [context.md#task-qualify-performance-against-gnome-51](./context.md#task-qualify-performance-against-gnome-51)

**Acceptance criteria**:
- [ ] Paired runs beat GNOME responsiveness and keep guest CPU/memory at or below GNOME, with reproducible artifacts. GPU/total-power comparisons are waived under ADR 0008.


### Milestone 3: Published TunaOS variants qualify, with Marlin upgrade and recovery

**What changes**: The published `marlin:tuna` image boots into a working real greeter and Tuna Desktop session, and users can recover from package or configuration changes while retaining their data across the documented upgrade path. Package provenance, the Marlin flavor, and VM qualification all contribute evidence to the same release checklist. The old `marlin:roost` tag is dropped immediately; the documented switch and recovery path targets `marlin:tuna`.

**Validation point**: A published image passes install, boot, real-greeter login, upgrade, and package/configuration recovery checks with user data intact; package and image provenance are verified; the Corral VM qualification passes; and the checklist can be completed with a recorded owner declaration.

#### - [ ] Task: Qualify Tuna Desktop package provenance
**Id:** e1d238bc-fa73-4fa3-abc3-9e968a9c5256
**Repo:** tunaos-packages
**Depends on:** none
**Execution:** agent

Make the published package's source, vendor assets, signature, architecture, and verification result traceable. The Marlin image lane must identify and install that exact package revision.

*Technical detail:* [context.md#task-qualify-tuna-desktop-package-provenance](./context.md#task-qualify-tuna-desktop-package-provenance)

**Acceptance criteria**:
- [ ] The published package is verifiable by exact source, assets, signature, version, and architecture before image qualification.

#### - [ ] Task: Publish Marlin Tuna without the Roost alias
**Id:** 539e4a24-2ebc-421d-8a72-f8112941d06b
**Repo:** tunaos
**Depends on:**
- e1d238bc-fa73-4fa3-abc3-9e968a9c5256 — Qualify Tuna Desktop package provenance
**Execution:** agent

Promote the flavor as `marlin:tuna` and remove `marlin:roost`. Deliver Tuna Desktop across all TunaOS variants under ADR 0008; qualify each immutable image or record its unsupported reason. Document a manual switch path for existing Marlin installs that preserves user data.

*Technical detail:* [context.md#task-publish-marlin-tuna-without-the-roost-alias](./context.md#task-publish-marlin-tuna-without-the-roost-alias)

**Acceptance criteria**:
- [ ] TunaOS publishes `marlin:tuna` with no `marlin:roost` alias and a tested data-preserving switch path; all TunaOS variants have immutable delivery/qualification evidence or an explicitly recorded unsupported reason.

#### - [ ] Task: Qualify the published Marlin Tuna image and upgrade path
**Id:** fd41584d-86c6-4ce8-be06-a43e42d5d5c1
**Repo:** rust-wayland-desktop
**Depends on:**
- e1d238bc-fa73-4fa3-abc3-9e968a9c5256 — Qualify Tuna Desktop package provenance
- 539e4a24-2ebc-421d-8a72-f8112941d06b — Publish Marlin Tuna without the Roost alias
**Execution:** agent

Qualify each immutable published TunaOS variant image through installation, boot, real-greeter login, session, portals, and lock, or record why that variant cannot support Tuna Desktop. Marlin additionally qualifies upgrade and package/configuration recovery with user data preserved.

*Technical detail:* [context.md#task-qualify-the-published-marlin-tuna-image-and-upgrade-path](./context.md#task-qualify-the-published-marlin-tuna-image-and-upgrade-path)

**Acceptance criteria**:
- [ ] The exact published image passes install, boot, greeter login, upgrade, and recovery with data intact and linked artifacts.

#### - [ ] Task: Qualify Marlin Tuna through Corral VM tests
**Id:** 62f9d9fc-60b9-4f82-ade6-22fbc39e44b2
**Repo:** corral
**Depends on:**
- fd41584d-86c6-4ce8-be06-a43e42d5d5c1 — Qualify the published Marlin Tuna image and upgrade path
**Execution:** agent

Run the published image in Corral and emit an independent machine-readable result. Keep host limitations distinct from image failures.

*Technical detail:* [context.md#task-qualify-marlin-tuna-through-corral-vm-tests](./context.md#task-qualify-marlin-tuna-through-corral-vm-tests)

**Acceptance criteria**:
- [ ] Corral passes image readiness, greeter, session, and recovery checks against the exact digest and reports host/image failures separately.


### Milestone 4: The v1 checklist reports the real state of every requirement

**What changes**: The public v1 checklist shows a live, evidence-backed result for each acceptance criterion, capability, performance requirement, and image qualification requirement. A missing, stale, or failing proof stays visibly open, and the checklist is reachable from the project front page.

**Validation point**: A clean run of the one-click checklist command reports each row from current evidence, detects a deliberately missing proof as non-passing, and the documented public entry point reaches the checklist.

#### - [ ] Task: Compute the v1 checklist from current evidence
**Id:** 049f792c-6776-440d-b834-c66a0104a24b
**Repo:** rust-wayland-desktop
**Depends on:**
- 3a5dd52d-a432-4a40-8136-bb72a35dedc1 — Make every GNOME 51 capability pass with proof
- e89a76d3-be6c-4de5-ac0f-a2924eadfefb — Complete the daily-driver journey on Tuna Desktop
- bb94fd76-fadc-4ff9-a9d7-dd4e2dfa8a22 — Qualify performance against GNOME 51
- e1d238bc-fa73-4fa3-abc3-9e968a9c5256 — Qualify Tuna Desktop package provenance
- 539e4a24-2ebc-421d-8a72-f8112941d06b — Publish Marlin Tuna without the Roost alias
- fd41584d-86c6-4ce8-be06-a43e42d5d5c1 — Qualify the published Marlin Tuna image and upgrade path
- 62f9d9fc-60b9-4f82-ade6-22fbc39e44b2 — Qualify Marlin Tuna through Corral VM tests
**Execution:** agent

Compute each checklist status from current, verified source-owned results. Link the evidence and keep missing, stale, failed, or inaccessible results non-green.

*Technical detail:* [context.md#task-compute-the-v1-checklist-from-current-evidence](./context.md#task-compute-the-v1-checklist-from-current-evidence)

**Acceptance criteria**:
- [ ] Every v1 criterion shows a status and linked proof derived from current evidence; missing or stale results never pass.

#### - [ ] Task: Publish a one-click v1 checklist entry
**Id:** d34a73f3-53a7-4535-9c75-001df2616b7e
**Repo:** rust-wayland-desktop
**Depends on:**
- 049f792c-6776-440d-b834-c66a0104a24b — Compute the v1 checklist from current evidence
**Execution:** agent

Link the public checklist from the project front page and provide one documented refresh action. Show the owner declaration field without hand-edited statuses.

*Technical detail:* [context.md#task-publish-a-one-click-v1-checklist-entry](./context.md#task-publish-a-one-click-v1-checklist-entry)

**Acceptance criteria**:
- [ ] A new contributor reaches the live checklist in one click and one documented action refreshes all evidence and exposes the final declaration.


## Open Questions

None. Implementation-time discovery does not change the v1 acceptance bar: when a required hardware counter, baseline artifact, or published-image proof is unavailable, report that criterion as pending or failed and stop before declaring v1 complete.


## Out of Scope

- A public extension broker or extension API is excluded by the spec's Non-Goals section; this plan does not create one.
- Compatibility publication of the old `marlin:roost` image tag is excluded by the user's decision to drop it now; the supported image path is `marlin:tuna` with a documented data-preserving switch path.
- A centralized service that re-runs or owns package, image, performance, and VM checks is excluded. Each registered repo remains authoritative for its proof, and the desktop checklist only aggregates current verified results.
- Qualification outside TunaOS and desktop baselines other than GNOME 51 is excluded. All TunaOS variants are in delivery scope; parity/performance comparison remains GNOME 51 on Marlin with matched hardware.

## Status refresh — 2026-10-29

Current facts that supersede the dated snapshots elsewhere in this plan and its context/research. All nine tasks remain unchecked.

- **Releases:** `v1.0.0-beta.1` is published (2026-10-15); betas are tagged from main under ADR 0008. Beta qualification is not v1 qualification.
- **Victory signal:** `scripts/victory-signal.py` and `docs/v1.md` are on main (#565) and run as `victory-checks`. Latest main run (2026-10-28): ledger 73 rows — 14 pass, 11 pass (nested), 44 partial, 4 missing; perf verdict pending; image verdict pending.
- **Red lanes on main:** `marlin-tuna-nightly` / `published-flavor` (published-image fixture build; under repair) and `Marlin performance samples` (#782).
- **Capability work landed since the last snapshot:** mouse acceleration profile (desktop #810, protocol 0.39). Open draft PRs at the time of this refresh: remaining pointer-device policies (#814, protocol 0.40), panel-menu BoxPointer open motion (#811), strip Super+drag installed proof (#812), Night Light gsd Color properties and signals (#815).
- **Stale-audit warning:** the 2026-10-13 per-capability closure audit on epic #577 is out of date for several rows (e.g. OSD fades, strip pointer move/resize, Night Light preview/local-time schedule were already implemented). Verify each gap against current source before implementing it.
- **Infrastructure:** Corral runs on `karnataka`; the AWS KubeVirt cluster is gone. The `marlin-roost-nightly` workflow is obsolete (last run 2026-10-08 on a branch).
