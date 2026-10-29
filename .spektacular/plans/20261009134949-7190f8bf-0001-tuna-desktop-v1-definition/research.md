---
created_date: "2026-10-10"
document_status: draft
---

# Research: 20261009134949-7190f8bf-0001-tuna-desktop-v1-definition

## Alternatives considered and rejected

### Central victory dashboard
Have one new service own every capability, gap, performance, and image verdict.

Rejected: the current instruments already have owners and proof lanes. A second source of truth would drift from the ledger and qualification workflows. Keep narrow aggregation over the existing evidence.

### Documentation-only checklist
Keep a human-maintained checklist and leave statuses to editors.

Rejected: the current signal must identify regressions from live sources. The first v1 victory PR currently computes ledger counts and gap count, but hard-codes performance and image as pending and leaves the checklist boxes static. That does not satisfy the spec's computed-status requirement; the plan must close this gap.

### Tier-scoped or deferred parity
Declare v1 from daily-driver journeys while leaving peripheral tracked capabilities or known gaps for later.

Rejected: the spec requires every tracked capability to pass and no confirmed gap to remain open. The ledger currently has 63 rows: 14 pass, 31 partial, 18 missing. The open gap query returns 30 issues.

### Relaxed performance target
Use the earlier performance plan's provisional regression budgets as the v1 bar.

Rejected: the v1 spec sets the stricter target: responsiveness must beat GNOME 51 and CPU, memory, and power must match or improve. Existing paired evidence has Tuna slower and using more CPU. Keep the user-approved v1 bar; flag any conflict with prior program thresholds for stakeholder resolution, never lower it silently.

## Chosen approach — evidence

- The current v1 spec has nine acceptance criteria: verified capability ledger, zero open gaps and clean audit, daily-driver journey, faster responsiveness, no higher CPU/memory/power, GPU-aware methodology with total power, published Marlin qualification, recovery/upgrade proof, and a one-click public checklist.
- `docs/parity-ledger.md` on current `main` has 63 rows (14 pass, 31 partial, 18 missing); 30 issues match the open `[GNOME 51 gap]` search.
- `docs/perf/README.md` describes paired GNOME/Tuna measurement on one shared Marlin image and an identical 6 GiB, 4-vCPU, 1280x800 QEMU/KVM profile. It retains CPU and process samples and has incomplete/unsupported DRM memory handling, but no qualified total-power result. The published Marlin GNOME tag was measured as 50.5, so the current benchmark derives a CI-only GNOME 51 baseline and retains its package manifest; the v1 methodology must keep that distinction visible. The paired baseline still shows Tuna behind GNOME. The GPU-split budget-gate PR #547 is open/draft and its compare run failed; the v1 requirements are not yet proven.
- The victory-signal PR #565 is draft and three commits behind current `main`. Its local refresh is `/tmp/tuna-v1-refresh` (`codex/v1-victory-refresh`), not published. `scripts/lib/victory-signal-tests.py` passes 6/6 locally. Reviewing `scripts/victory-signal.py` shows only the ledger and issue count are computed; perf/image status is hard-coded pending. The v1 document is one click from README only in the unmerged PR branch.
- Published-image qualification PR #273 remains open and its latest published-flavor lane fails at `orca-reset`; no qualification claim is supported. TunaOS PR #3107 is also open. Package PR #810 is merged, its vendor release exists, and publish run 37881967008 reports successful build, publish, and verify jobs for x86_64. The TunaOS PR says it depends on that package publication. User decided to drop the old `marlin:roost` image alias now; do not add or retain that tag in the Tuna flavor rollout. Preserve an explicit, tested migration path to `marlin:tuna` for users who still run the old image.
- TunaOS current `main` is still on the old flavor; PR #3107 contains the proposed `roost` to `tuna` migration. It is blocked pending the flavor decision and published package prerequisite. The user's alias decision resolves the first blocker.
- Corral `Justfile` has generic `vmtest-layer` and KVM-backed `vmtest-e2e` targets. The current main targets do not emit Tuna-specific install/login/recovery/performance verdicts; that remains to be integrated and run.
- The configured repos are rust-wayland-desktop, tunaos, tunaos-packages, and corral. Member checkouts have user-owned dirty/untracked data, so implementation must use isolated worktrees and preserve the registered roots.
- The approach remains federated: each repo's own lane emits authoritative evidence, with a small signal layer reading those verdicts. Avoid a parallel dashboard or manual status-only checklist.

## Files examined

- `rust-wayland-desktop:docs/parity-ledger.md` — 63 capability rows; status counts 14/31/18.
- `rust-wayland-desktop:docs/perf/README.md:3-29` — shared-image GNOME 51 benchmark, same VM profile, sampled process/memory data, DRM-counter limits, and measurement limitations.
- `rust-wayland-desktop:docs/perf/2026-10-08-cpu-investigation.md:43-97` — identified compositor and shell CPU costs, fixes, and open steady-state work.
- `rust-wayland-desktop:scripts/victory-signal.py:21-93` — computes ledger/gap counts but hard-codes perf and image pending.
- `rust-wayland-desktop:scripts/lib/victory-signal-tests.py` — tests the computed signal contract; 6/6 passed in the isolated refreshed branch.
- `rust-wayland-desktop:docs/v1.md:20-47` — the public nine-item checklist and current evidence locations; file exists only in PR #565 until merge.
- `rust-wayland-desktop:.github/workflows/performance-baseline.yml` and `docs/perf/README.md` — baseline workflow and paired-measurement constraints.
- `tunaos:.github/build-config.yml` — current main flavor configuration; old Roost flavor remains until PR #3107 lands.
- `tunaos:PR #3107` — flavor rename and image-tag plan; owner decision was to drop the old tag now.
- `tunaos-packages:packages/tuna-desktop/package.yaml` — package recipe already merged through #810.
- `tunaos-packages:workflow run 37881967008` — package build, publish, and verification all succeeded for x86_64.
- `corral:Justfile:25-39` — generic layer and KVM VM-test targets, without Tuna qualification verdicts.
- `rust-wayland-desktop:PR #547`, `PR #273`, `PR #565`, `tunaos:PR #3107` — live states, failed jobs, remaining dependencies, and scope.

## External references

- [Linux DRM usage statistics](https://www.kernel.org/doc/html/v6.9/gpu/drm-usage-stats.html) — the benchmark docs use its client counters and correctly avoid interpreting shared memory totals as unique physical GPU usage.
- GitHub Actions runs and PRs cited above — current authoritative CI and dependency evidence; no separate external research was needed.

## Prior plans / specs consulted

- The active v1 specification was read through `spektacular spec file read`; its requirements, constraints, acceptance criteria, and metrics are the scope for this plan.
- The previous v1 plan/context/research were read through the Spektacular CLI while its implementation workflow was active. They provided prior investigation and design decisions, but their state was stale; this plan refreshes counts, lane status, the alias decision, and the incomplete computed signal.
- The v1 spec references no design documents (`spektacular design ref list` returned zero refs and zero unresolved).

## Open assumptions

- Re-count the ledger and gap issues when each task runs; they change as PRs land.
- The user chose to stop publishing `marlin:roost`; the current old tag may remain pointing at its last image. The upgrade proof must tell existing users how to switch to `marlin:tuna` and verify that path.
- The benchmark environment must demonstrate total power and meaningful CPU/GPU accounting on the stated reference hardware. The current VM may not expose usable GPU counters; if so, the plan must require qualified hardware evidence instead of substituting zero or inferring a win.
- Performance gate status and image qualification status must be read from their actual published run verdicts. A missing lane or missing artifact is `pending`/`unknown`, never pass.
- The open issue/ledger burndown is a multi-wave program. Every row/issue still needs its own proof and normal review; this plan's aggregate tasks do not make that evidence optional.
- The current benchmark baseline's GNOME/Marlin version and earlier spec-006 threshold must be reconciled with the v1 target before a release declaration; v1's stricter spec remains the target unless the user explicitly changes it.

## Drafting assumptions

### Ledger/gap counts are a snapshot (discovery)
- **Decision**: record 14/31/18 rows and 30 gap issues as 2026-10-10 values; plan tasks re-count at execution.
- **Rationale**: both drift daily; freezing counts would rot.
- **Rejected**: re-auditing exhaustively inside discovery.

### v1 perf bar vs spec 006 R2 (discovery)
- **Decision**: keep the stricter interview bar (beat responsiveness, match CPU) and flag the tension with 006's ≤10%-regression rule in research; do not reconcile unilaterally.
- **Rationale**: user explicitly chose it after seeing current numbers; reconciliation needs user sign-off at walkthrough if it bites.
- **Rejected**: silently downgrading to 006 R2.

### Federated glue over central dashboard (architecture)
- **Decision**: per-instrument owners + small CI checks; no new aggregator system.
- **Rationale**: every instrument already has an owner and lane; aggregators rot when lanes change shape, while text-only federation goes stale.
- **Rejected**: central nightly dashboard (rot risk); pure spec text (drift risk).

### Corral gets its own task (tasks)
- **Decision**: nine tasks; VM validation is separate in corral depending on image qualification.
- **Rationale**: single-repo rule; VM-only regressions must fail the signal independently.
- **Rejected**: folding VM runs into the tunaos task.

### Verdict schemas are illustrative (data_structures)
- **Decision**: sketch field shapes; leave exact schemas to implementers of each lane.
- **Rationale**: lanes already emit artifacts; prescribing schemas now would fight their formats.
- **Rejected**: fully specified schemas (premature).

### Federated source-owned verdicts (architecture)
- **Decision**: each repository's existing lane owns its result; the desktop signal reads the latest verified results and reports unknown when evidence is missing.
- **Rationale**: the four repos already own the ledger, paired runs, image publication, and VM runner. A second dashboard would duplicate those authorities.
- **Rejected**: a central dashboard that recomputes all evidence, and a manual checklist whose status can drift.

### Four user-visible delivery milestones (milestones)
- **Decision**: group the v1 work into public evidence, daily-driver capability closure, matched performance qualification, and published-image/recovery qualification.
- **Rationale**: these are independently reviewable outcomes and follow the evidence dependencies from desktop behavior through release qualification.
- **Rejected**: organizing milestones by repository, because the user-facing v1 result spans repositories and should be reviewed by outcome.

### Checklist aggregator follows source-owned proof (tasks)
- **Decision**: finish the desktop capability, daily-driver, performance, package, flavor, image, and VM proofs before computing the final checklist and exposing its public entry.
- **Rationale**: the final verdict can then consume immutable source-owned artifacts and avoid treating planned or missing evidence as passed.
- **Rejected**: building the final signal first with placeholder states, because PR #565 already demonstrates that such a signal can look complete while performance and image remain hard-coded pending.

## Rehydration cues

- Start with `spektacular repo list` and use the reported roots; never use the dirty member checkout as the implementation worktree.
- Read current plan artifacts with `spektacular plan file read`, and read knowledge only through the knowledge CLI.
- Re-check #565, #547, #273, package publish run 37881967008, and TunaOS #3107 before implementation; external status may change.
- Recount `docs/parity-ledger.md` and `[GNOME 51 gap]` issues at execution.
- Use the `spek-plan` workflow for this refresh, then `spek-implement` only after the walkthrough is explicitly approved.

## Implementation audit update — 2026-10-10

Source `c387326d` contains 72 ledger rows (23 status-starts-pass, including nested qualification,31 partial,18 missing), not the earlier63/14 snapshot. Reviewed docs changes move implemented size-change animation to partial:23 pass variants,32 partial,17 missing. No new GNOME comparative qualification is claimed. Screenshot keyboard controls already exist with a GTK geometry/capture proof; edge-boundary and output-resize regression fix added. Remaining animation gaps are traced to actual source behavior. Full native/image/hardware proofs remain open.

## Owner-approved CI acceleration — 2026-10-17

The owner approved lightweight lane selection and Rust cache policy/coverage and chose to continue inside this v1 workflow rather than overwrite its state with a separate spec. This is supporting work, not completion of any v1 qualification criterion. Implementation PR #721, source9d4b6d6025406668cd7684f09dd6a67059fcbed0, full PR CI38991264581.

The deeper source audit found real successful-current-check admission in `scripts/lib/auth_proof_identity.py` as well as grid/fingerprint helpers. Consequently GTK retains its check prerequisite. The earlier exploratory 8.9–12.5-minute overlap estimate omitted that edge and is not an expected or measured speedup for this implementation. Journey, Arch and independent proofs can start early; mutable image publication still waits for check, and all artifact consumers retain producers. The final gate explicitly requires selector/check success, including rejecting skipped infrastructure.

Caches stay job/native-environment-specific and write only from non-PR main runs. Additional coverage is standalone Smithay/reis, separate grid old/fixed and fingerprint targets, and Arch Cargo home/target using installed native package inventory. Fresh Cargo/source/executable admission and runtime proofs are mandatory regardless of cache status. New namespaces can be cold until trusted main seeds them; no paid quota changes or ORAS infrastructure were introduced.

Local verification:54 existing affected tests pass; actual CLI selection yields all16 lanes for dispatch/CI changes and no proof lanes for docs-only PRs. Actual gate shell accepts all-success/docs-only and rejects failed/skipped selector, skipped check, failed/cancelled proofs. actionlint's six unknown ubuntu-26.04 labels and one SC2024 also exist in the unchanged baseline; no diagnostic suppression or runner change was applied.

Early actual CI observation:selector completes and six independent jobs start at01:32:07,11s after creation, while check (started01:31:58) is still running. Full result and cache/latency interpretation remain pending. One all-lane PR workflow is used rather than launching a duplicate manual full run.

## CI acceleration execution evidence — 2026-10-17

Owner-approved supporting work only; the nine v1 acceptance tasks remain unchecked and the implement workflow remains open.

- Primary PR721 source9d4b6d60 passed all19 jobs/all16 real proof lanes without retry, then merged as73ebfed07f341c5ef1a7ef50727ecb6d05533fe7. Trusted-main seed38994805232 also passed all19. The selector unblocks independent proofs without waiting for check; GTK retains the successful current-run check dependency required by executable/source admission. No receipt, source identity, deadline or final gate was weakened.
- All ten primary PR cache invocations were restore-only. Main seed inventory verified7,800,143,786 bytes across ten entries, but Arch was only960 bytes: host-context Cargo paths differed from the JavaScript action's container paths. Follow-through PR724 derives Cargo home/target from the actual container GITHUB_WORKSPACE, publishes through GITHUB_ENV, restores before builder chown and uses a new arch-container prefix to avoid the immutable empty entry. A real Arch container smoke and later GHA action/builder paths agree under /__w. Actual corrected main seeding is still pending qualification/merge.
- All four auxiliary workflow locations now have native identities and trusted-main-only saves. Fuzz preserves intentional shared nightly dependencies and both real sanitizer budgets; stress retains10PR/300scheduled rounds; coverage retains compiler/LLVM/floors/security cases; visual retains all14 stages/manifest/triggers. Workflows without main-push triggers seed on their existing scheduled/manual main runs, not on PRs.
- Auxiliary-only fa01929e passed all five relevant workflows. Adding the Arch correction selected all16 proofs at0b81107c; four auxiliaries passed, but actual GTK native-auth PAM ownership exited early and the gate failed. Successful main and failing PR compositor SHA256 were identical (ef945dd418540be7498df644dcbae886f56c3bfe8f5946069e0c5710d14729d6); this does not establish a stale executable or explain the early exit.
- Native investigation found a separate definite fixture race: started-N existed before its PID write. An untraced real proof captured ParseIntError(kind Empty). Deterministic real-PAM producer/consumer smoke failed before and passed after atomic publication, including occupied slots, actual PID, no overwrite and mode0600. Private write/close plus no-replace link now publishes complete C markers; parent PID uses the existing atomic publisher. Supervised libtest panics/child exit status are retained. Forty consecutive full native proofs passed after this fix with existing deadlines on two available CPUs/fresh Mesa caches. Own throwaway producer driver was removed.
- f5e433bc qualification: four auxiliaries plus strategy passed21 jobs and complete GTK passed33m06, including all native roles. FullCI39003485052 nevertheless failed portal-security and VM V-LIFECYCLE; ci-gate rejected and ledger skipped. Portal's genuine consent/settings/background pixel cases passed; its restarted shell never claimed org.gnome.Shell before the existing30s deadline. VM passed four pristine boots and locked/masked shell-loss safety, but the replacement shell did not complete normal startup or unlock before the existing15s transition deadline. Both last logged software-renderer selection; actual startup deadlock/cause remains unproven. Do not apply speculative socket cleanup or larger timeouts as a fix.
- Two read-only artifact/source investigations returned evidence; neither changed files or reran anything. Their categorical cache-causality exclusions and proposed timeout extension are not accepted as proof: Arch was cold and no cache mismatch is observed, but absence of shipped-code edits alone is not a causal demonstration. Preserve actual proof failures rather than waiving them.
- External main changed during f5 CI: #718 ->19825b93, #606 ->dfdd155b. Preserved both via clean rebase. Current candidate34dbe8347fe9741629c58bc047ea12ece824d111 passed cargo fmt and actual three-role native proof with fresh source/hash admission. New exact-head CI39008087006, coverage39008086983, visual39008086978, fuzz39008087007, stress39008086979 and strategy39008087048 are qualifying this updated source. This is not a retry of the failed old head.

### Observed cache/job timing, not a successful pipeline speedup

| Job | First cold PR minutes | f5 successful job minutes | f5 cache |
| --- | ---: | ---: | --- |
| check | 11.97 | 6.03 | restored |
| fingerprint | 3.60 | 1.65 | restored |
| scroll | 6.82 | 3.28 | restored |
| app-content | 4.33 | 2.05 | restored |
| DRM | 4.50 | 2.88 | restored |
| greeter | 1.68 | 0.90 | restored |
| journey | 39.68 | 27.03 | restored |
| GTK | 32.18 | 33.10 | restored; live proof dominates |
| grid comparison | 12.75 | 13.70 | missed; earlier warm0b job7.15 |

These are uncontrolled job observations; failed whole-run elapsed is NOT a speedup claim. At f5 check waited14.75min before starting its6.03min work; GTK started24.75min after creation. Scheduling delays and real GTK proof duration dominate remaining elapsed. Eight actual f5 restores and ten save-if:false were observed; Arch's changed prefix was intentionally cold. Grid missed the identical seeded key/version; old entry disappeared and a new main entry was created05:40. Inventory then contained14 entries/9,223,627,733 bytes, including five legacy PR-scoped copies/3,484,839,940 bytes. Continuing old-branch writers compete with main caches; the precise deletion/eviction mechanism was not directly observed. No unrelated cache deletion, paid quota change, run cancellation, ORAS infrastructure or resumed-session proof was introduced.

### Follow-through qualification and Arch keep-set correction

PR724 exact34dbe834 passed all40 checks, including all19 CI jobs/all16 real lanes, without retry. Portal, VM and GTK passed. It was normally squash-merged with a head-match guard as58ed90b502c6019c9cec4965124d27d323342f91; no admin override. Qualification: https://github.com/tuna-os/tuna-desktop/pull/724#issuecomment-6230847300 . Successful full CI39008087006 elapsed45.90min; that is not a controlled before/after speedup. Seven primary caches restored; check/fingerprint/new Arch missed. All15 actual PR cache invocations remained restore-only.

Normal merged-main CI39011018952 restored nine caches and missed Arch; all ten writer guards were true. Main measured coverage39011018788 passed and seeded720,515,080 bytes under its Ubuntu24.04 identity; strategy39011018838 passed. Main CI was NOT green: GTK failed G-IME-PROPERTIES-LIFECYCLE lock-phase deadline, final gate rejected, ledger skipped. Portal and VM passed. Retained lifecycle log shows the observer waiting to find the initial '中' indicator in fresh_menu/press, not a completed lock request; compositor snapshot locked=false. This does not establish product/cache causality and no timeout, assertion, product fix or retry was applied.

Arch still uploaded only983 bytes (cache9083191864): container path agreement alone was insufficient. The pinned action's actual Workspace.getPackagesOutsideWorkspaceRoot uses !pkg.manifest_path.startsWith(workspace.root). Our Cargo home beneath that root placed registry dependency manifests in the excluded set. Its save then passed an empty dependency keep-set to target/registry cleanup. This is a demonstrated cleanup defect in our integration, not a native-rustup warning or unexplained makepkg environment reset.

Before/after smoke used a real Cargo build and the exact pinned bundled CacheConfig/Workspace/target/registry cleanup, tar/zstd archive, restore and real rebuild. Bundles were verified against upstream Git blob hashes. In-workspace Cargo home retained zero dependency packages, yielded629 bytes and recompiled itoa; external Cargo home retained itoa, yielded57,359 bytes and Cargo reported Fresh itoa after restoration. The real executable also produced its expected value. A broken unrelated host nightly toolchain initially prevented action configuration; the successful smoke then used the installed native compiler directly, exercising the supported no-rustup path used by Arch. No installed toolchains were changed.

New source cfc8891d285c0f893539271e1cd6790eaa1d0d6b, PR734, moves Arch Cargo home to container-derived RUNNER_TEMP outside the workspace, chowns it with the workspace for the existing builder, and starts arch-container-v2 to avoid immutable empty snapshots. Target stays in the workspace. Native fingerprint and writer boundary remain unchanged; all real commands/admission/deadlines remain. Existing docs explain the keep-set boundary. Actual selector selects all16 lanes; actionlint has only the same six runner-inventory and one SC2024 diagnostics. Normal exact-head CI39014103988 is qualifying this changed source. Real substantive Arch main upload remains pending; do not claim that coverage or a whole-pipeline speedup yet. All9 v1 acceptance tasks remain unchecked.

### Qualified Arch retention and latest measured outcome

PR734 exactcfc8891d285c0f893539271e1cd6790eaa1d0d6b passed CI39014103988, all19 jobs/all16 real proof lanes, without retry. GTK, portal, VM, ledger and final gate all passed. Nine caches actually restored; Arch v2 was cold; all ten PR invocations were restore-only. Real Arch action and builder used /__w/_temp/ci-arch-cargo and package build/install/version checks passed. Main was still qualified base58ed90b5, PR CLEAN/MERGEABLE, and guarded normal squash merge produced2617bf54e3efb083122197d9c657a1452ebe551b at08:17:45Z, with no admin override.

The normal main run39016825550's actual Arch job117350600224 succeeded at08:28:34Z on exact2617bf54. Its direct completed-job log records save-if:true and Sent516497262 of516497262 bytes. GitHub cache inventory independently confirms archive9084951788, refs/heads/main, created08:28:28.493309Z, v0-rust-arch-container-v2 nativefdffbb99/compiler-envf40fec4f/dependencyae817277, size516,497,262 bytes. This establishes substantive Arch retention, unlike prior960/983byte archives. Full normal main CI39016825550 then completed SUCCESS on exact2617bf54: all19 jobs/all16 real lanes, elapsed40m31s, nine actual restores/one Arch miss, all ten trusted-main writer guards true. Completed-job upload log and cache API independently agree on516,497,262 bytes. The terminal watcher encountered a TLS handshake timeout near completion; direct GitHub API resolved the actual SUCCESS and all19 final job conclusions. No CI retry or duplicate dispatch occurred. Supporting CI acceleration verification is complete; no broader v1 acceptance task is marked done.

Latest qualified full PR elapsed42.13min versus first cold49.48min is an uncontrolled observation, not a promised speedup. Source/product changes, scheduling and cache retention differ. Actual cache restore steps took4–29seconds. GTK's main shell proof step took23.40min; journey's installed preferred-session qualification took21.85min. These live proof costs, not registry transport, dominate the remaining critical path. No ORAS or resumed-session verdict reuse was added.

| Job | First cold PR minutes | Latest qualified PR minutes |
| --- | ---: | ---: |
| check | 11.97 | 6.27 |
| grid comparison | 12.75 | 7.42 |
| fingerprint | 3.60 | 1.87 |
| scroll | 6.82 | 3.20 |
| journey | 39.68 | 27.58 |
| GTK shell | 32.18 | 33.23 |

Latest inventory lists15 main archives totaling9,794,854,773 bytes plus one legacyPR707 archive1,145,115,970 bytes. This is a point-in-time inventory, not an eviction guarantee. Earlier observed misses remain real; no unrelated cache deletion or quota/billing change. Existing scheduled auxiliary main writers can seed normally; no duplicate dispatch was introduced. Own pinned-action/makepkg reproduction scaffolds were removed after proof publication; actual failed-runtime artifacts remain retained. All nine v1 acceptance tasks stay unchecked; this completes supporting CI scope, not v1.
