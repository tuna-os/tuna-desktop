---
created_date: "2026-10-10"
document_status: draft
---

# Context: 20261009134949-7190f8bf-0001-tuna-desktop-v1-definition

**Approved scope reconciliation (2026-10-17):** `docs/adr/0008-v1-beta-track-and-release-scope.md` records the owner's existing decisions: all TunaOS variants, Marlin as GNOME 51 comparison baseline, and waived GPU/total-power comparisons. Hardware-only P-DS-04, P-IN-05, P-DS-03, P-SY-09, P-LK-02 and modem portions of P-ST-10 still require implementation and proof; emulated-device proof is acceptable when labelled, while physical validation is deferred to the post-v1 matrix. Beta qualification is not v1 qualification. All nine tasks remain unchecked; stable task IDs/titles are retained.

<!-- Workflow: 20261009134949-7190f8bf-tuna-desktop-v1-definition -->

## Current State Analysis

The work spans desktop, TunaOS, tunaos-packages, and Corral. At planning time the ledger has 63 rows (14 pass, 31 partial, 18 missing) and the gap search finds 30 open issues; recount during implementation. PR #565's six signal tests pass, but performance and image status are hard-coded pending. Performance PR #547 does not meet the v1 bar, and published-image PR #273 fails at `orca-reset`.

TunaOS PR #3107 proposes the flavor switch. The user decided to drop `marlin:roost` now and use only `marlin:tuna`, with a data-preserving switch path. Package PR #810 published and verified source `d443858` for x86_64. Corral has generic layer and KVM VM targets but no Tuna-specific verdict.

The comparison baseline remains GNOME 51 on Marlin and matched hardware. The published GNOME tag measured 50.5, so CI derives GNOME 51 and retains its manifest. Required latency and guest CPU/memory comparisons remain non-passing until qualified. ADR 0008 waives GPU/total-power comparisons; their absence is not a v1 blocker and grants no superiority claim.

## Per-Task Technical Notes

### Task: Make every GNOME 51 capability pass with proof

**Owner-approved CI acceleration supporting work (2026-10-17):** Continue inside the active v1 workflow. Split lightweight lane selection from expensive workspace checks; preserve successful-check admission where required, immutable source/package provenance and the final gate. Seed job/variant-specific dependency caches only from trusted main, restore them on pull requests, and cover standalone and isolated proof targets plus the native Arch build. Existing selected/full proof coverage, assertions, deadlines, main-run completion and performance-baseline admission stay unchanged. Verify the actual workflow and cache behavior; report observed timing separately from counterfactual overlap. ORAS, paid quota changes, live-session snapshots and unrelated product fixes are not part of this change.

**CI supporting implementation and verification:** `.github/workflows/ci.yml` owns lane dependencies and pinned cache actions; `scripts/ci-select-lanes` remains the selector, with its existing contracts. Keep grid/fingerprint completed-check admission and any other source-discovered safety dependency. Add deterministic opt-in cache targets to their build scripts only where necessary; cache native Arch Cargo home and target under the Arch compiler/package identity, never Ubuntu objects. Exercise selector full/PR paths, final-gate failure behavior, existing affected contract tests, workflow syntax and an exact-head full CI run (use the all-lane PR trigger for a CI-workflow change rather than duplicating it with a manual dispatch). Never infer cached proof success; every proof still runs. Add current operational cache/selection documentation after smoke evidence. These supporting changes do not mark the capability/v1 acceptance criterion complete.

**File changes:**
- `docs/parity-ledger.md:3-107` — implement remaining in-scope behavior, attach stable proof identifiers, and change each row only after the behavior passes.
- `docs/v1.md:36-44` — keep the checklist's ledger and gap-issue links aligned with the evidence source.
- `scripts/tuna-ledger:1-` — enforce that each passing ledger row names an existing passing proof.
- `scripts/lib/tuna-introspect.sh:1-` and `scripts/tuna-journey:1-` — extend the existing journey and evidence helpers to produce proof for the currently partial/missing ledger rows.
- `docs/gnome-parity.md:1-` — store baseline comparisons and reviewer evidence for capabilities whose acceptance is visual or hardware-specific.
- Reconcile live `[GNOME 51 gap]` issues, including the docs accuracy issue #507, against the ledger; the current snapshot is 30 open issues, so re-count at execution.

**Complexity:** High
**Token estimate:** ~30k tokens
**Agent strategy:** Parallel analysis across capability areas, then sequential implementation and ledger integration so each row points at shipped, verified behavior.

### Task: Complete the daily-driver journey on Tuna Desktop

**File changes:**
- `scripts/tuna-journey:1-` — extend the end-to-end scenario to cover the full login-to-logout flow without launching a GNOME session.
- `.github/workflows/ci.yml:49-56` — publish the journey verdict and artifacts as a required qualification result.
- `docs/greeter-vm-acceptance.md:1-` — align the real-greeter acceptance path with the end-to-end journey where the checks overlap.

**Complexity:** High
**Token estimate:** ~18k tokens
**Agent strategy:** Parallel analysis of journey coverage and session lifecycle, then sequential integration of the complete no-fallback path.

### Task: Qualify performance against GNOME 51

**File changes:**
- `docs/perf/README.md:1-` — pin exact published and derived baseline identity, matched hardware/image profile, collection limits, guest CPU/memory methodology, and the ADR 0008 GPU/total-power comparison waiver.
- `.github/workflows/performance-baseline.yml:1-` — publish the paired run and strict v1 pass result with baseline provenance.
- `packaging/marlin/perf/tuna-perf-phase:1-`, `packaging/marlin/perf/tuna-gnome-reference:1-`, and `packaging/marlin/perf/tuna-kernel-trace:1-` — retain the workload, reference collection, and kernel evidence needed for matched latency and guest CPU/memory. Available GPU/power diagnostics remain optional and cannot imply superiority.

**Complexity:** High
**Token estimate:** ~24k tokens
**Agent strategy:** Qualify the existing paired latency and guest CPU/memory benchmark, preserving reference identity and exact-head CI admission. GPU/total-power comparisons are not v1 gates.

### Task: Qualify Tuna Desktop package provenance

**File changes:**
- `tunaos-packages:packages/tuna-desktop/package.yaml:1-37` — preserve exact source and vendor asset identity and make the published package version and architecture available to the qualification record.
- `tunaos-packages:.github/workflows/publish-tideforge-arch.yml:1-` and `tunaos-packages:scripts/arch-verify-published.sh:1-` — extend the existing publish/verify path to emit an immutable result for the exact Tuna Desktop artifact.
- `tunaos-packages:tests/test_verify_published_wave.py:1-` — assert that missing or mismatched published package evidence cannot pass.

**Complexity:** Medium
**Token estimate:** ~10k tokens
**Agent strategy:** Single agent, sequential execution; the current package release has already been published and verified, so extend its existing release lane rather than recreate the package build.

### Task: Publish Marlin Tuna without the Roost alias

**File changes:**
- `tunaos:manifests/desktops/roost.yaml:1-` and `tunaos:.github/build-config.yml:1620-` — rename the admitted Marlin flavor and its image reference to Tuna.
- `tunaos:tests/test_roost_admission.py:9-45`, `tunaos:tests/bats/test_resolve_flavor.bats:154-158`, and `tunaos:tests/test_luks_e2e_matrix.py:139-155` — update admission, flavor resolution, and matrix coverage for the new flavor name, while asserting no legacy alias is emitted.
- `tunaos:experiences/roost/README.md:1-` and `tunaos:README.md:1-` — remove the `marlin:roost` publication claim and document the `marlin:tuna` switch path with data preservation.
- TunaOS PR #3107 (`marlin/tuna-flavor`) — carry the user's decision through the open PR and its generated workflow/configuration changes.

**Complexity:** Medium
**Token estimate:** ~16k tokens
**Agent strategy:** Single agent, sequential execution; registry config, generated workflow, admission tests, and docs must agree on one public tag.

### Task: Qualify the published Marlin Tuna image and upgrade path

**File changes:**
- `scripts/tuna-release:1-` and `scripts/check-release-package:1-` — verify the exact release artifacts and preserve installation/configuration recovery information.
- `scripts/tuna-journey:1-` and `.github/workflows/ci.yml:49-56` — qualify the real greeter and desktop session on the published `marlin:tuna` image, not only a locally built package.
- `docs/greeter-vm-acceptance.md:1-` and `docs/release.md:1-` — document install, boot, login, upgrade, rollback/recovery, and user-data checks as a single reproducible qualification path.
- Existing published-image qualification PR #273 — fix the current `orca-reset` lifecycle failure and retain the run artifacts with the published image digest.

**Complexity:** High
**Token estimate:** ~22k tokens
**Agent strategy:** Parallel analysis of greeter, image lifecycle, and data recovery paths, then sequential integration against one immutable published image digest.

### Task: Qualify Marlin Tuna through Corral VM tests

**File changes:**
- `corral:Justfile:30-39` — add or extend the repeatable Corral command that runs the TunaOS image qualification against an explicit image digest.
- `corral:pkg/vmtest/spec.go:46-110` and `corral:pkg/vmtest/result.go:45-95` — express the Tuna readiness/session assertions and return image identity plus machine-readable pass/fail artifacts.
- `corral:pkg/vmtest/e2e_test.go:1-` and related `corral:pkg/vmtest/*_test.go` — cover the Marlin Tuna fixture and host-versus-image failure distinction.

**Complexity:** Medium
**Token estimate:** ~16k tokens
**Agent strategy:** Single agent, sequential execution; reuse Corral's generic VM test path and keep the image-specific fixture in the TunaOS integration contract.

### Task: Compute the v1 checklist from current evidence

**File changes:**
- `scripts/victory-signal.py:1-` — replace hard-coded pending performance and image fields with reads of the latest verified source-owned results; preserve non-green behavior for missing, failed, stale, or unavailable evidence.
- `scripts/lib/victory-signal-tests.py:1-` — cover all nine acceptance criteria, stale evidence, image identity mismatch, and missing evidence.
- `docs/v1.md:1-` — render computed status and evidence links for every row instead of static checkboxes.
- Current PR #565 version of `scripts/victory-signal.py:74-87` hardcodes performance/image as pending; the existing six tests are useful baseline coverage but do not test those source verdicts.

**Complexity:** Medium
**Token estimate:** ~15k tokens
**Agent strategy:** Single agent, sequential execution; source-owned artifacts and failure modes are documented by the upstream repo tasks, so the aggregator should consume rather than recompute their verdicts.

### Task: Publish a one-click v1 checklist entry

**File changes:**
- `README.md:27-30` — link the public v1 checklist and make the qualification entry obvious from the project front page.
- `docs/v1.md:1-44` — provide the one-action refresh instructions, computed verdict links, and a field for the owner to record the final declaration without editing status values.
- `.github/workflows/ci.yml:1-` — expose the complete checklist refresh as one documented/dispatchable action and publish its artifacts.

**Complexity:** Low
**Token estimate:** ~8k tokens
**Agent strategy:** Single agent, sequential execution.

## Testing Strategy

- **Ledger/gaps:** every cited proof passes; all rows pass; a fresh audit finds no open issues or untracked behavior.
- **Daily use:** the real-greeter journey completes core work, settings, notifications, lock/unlock, and logout without GNOME.
- **Performance:** matched runs beat GNOME response and keep guest CPU/memory at or below it, with immutable provenance. GPU/total-power comparisons are waived under ADR 0008.
- **Package/image:** verify exact package and image digests; install, boot, greeter login, upgrade, and recovery retain user data.
- **Corral:** the KVM run passes image assertions and reports identity plus separate host/image failures.
- **Signal/publication:** contract tests prove missing or stale evidence cannot pass; manual checks cover the front-page link, audit, and owner declaration.

## Project References

- Active spec `20261009134949-7190f8bf-tuna-desktop-v1-definition`, read through `spektacular spec file read`; it defines nine criteria and the strict GNOME 51/Marlin bar.
- Related specs 002, 003, 004, and 006 provide program context; their documents remain historical. No design is referenced.
- Registered roots: `/home/ubuntu/dev/rust-wayland-desktop`, `/home/ubuntu/dev/rust-wayland-desktop/.spektacular/repos/tunaos`, `/home/ubuntu/dev/tunaos-packages`, `/home/ubuntu/dev/corral`.
- Live work (as of 2026-10-29): desktop #565, #547 and #273, TunaOS #3107 and package #810 are all merged; see the plan's "Status refresh — 2026-10-29" for current lane verdicts and open PRs.

## Token Management Strategy

| Tier | Token Budget | Agent Strategy |
|------|-------------|----------------|
| Low | ~10k | Single agent, sequential |
| Medium | ~25k | Parallel analysis, then sequential integration |
| High | ~50k+ | Parallel capability/instrumentation work, sequential release proof |

Capability closure and release qualification are high complexity; each implementation task stays within its assigned repo.

## Migration Notes

Stop publishing `marlin:roost` now. Publish `marlin:tuna`, test the data-preserving switch for existing installs, and retain exact package and image digests in qualification records.

## Performance Considerations

The v1 bar is faster overview/input/animation response with guest CPU/memory no higher than GNOME 51 on matched hardware and image family. Record exact image/package inputs and run provenance, preserving the distinction between published GNOME 50.5 and CI's derived GNOME 51. ADR 0008 waives GPU/total-power comparisons. Missing required latency or CPU/memory evidence remains non-passing; no GPU/power superiority claim is permitted.

## Implementation audit update — 2026-10-10

Source `c387326d` contains 72 ledger rows (23 status-starts-pass, including nested qualification,31 partial,18 missing), not the earlier63/14 snapshot. Reviewed docs changes move implemented size-change animation to partial:23 pass variants,32 partial,17 missing. No new GNOME comparative qualification is claimed. Screenshot keyboard controls already exist with a GTK geometry/capture proof; edge-boundary and output-resize regression fix added. Remaining animation gaps are traced to actual source behavior. Full native/image/hardware proofs remain open.
