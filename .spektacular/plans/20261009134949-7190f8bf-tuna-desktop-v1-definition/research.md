---
created_date: "2026-10-09"
document_status: final
closed_date: "2026-10-09"
---

# Research: 20261009134949-7190f8bf-tuna-desktop-v1-definition

## Alternatives considered and rejected

### Option A: Tier-scoped parity bar

Daily-driver journeys pass while exotic areas defer by ADR.

**Rejected**: user chose ledger-100%-pass plus zero open gaps in the spec interview as the checkable bar.

### Option B: Beat-GNOME-by-10%-everywhere / match-now-beat-later perf bars

Uniform margin or phased targets.

**Rejected**: user chose beat-on-responsiveness, match-on-CPU in the spec interview.

### Option C: Central nightly victory dashboard

One aggregator job recomputing victory over all trackers.

**Rejected**: every instrument already has an owner and lane; an aggregator rots when lanes change shape (architecture decision, recorded in assumptions).

### Option D: Pure spec text, no computed checks

Definition only, hand-maintained list.

**Rejected**: a manual list drifts stale within weeks (architecture decision).

### Option E: Docs-only victory list

No spec checklist.

**Rejected**: user chose spec-carries-checklist with docs mirror.

## Chosen approach — evidence

- Victory = 9 acceptance criteria in spec `20261009134949-7190f8bf-tuna-desktop-v1-definition` (spec store); `docs/v1.md` mirrors it.
- Parity instrument `docs/parity-ledger.md` on origin/main: 63 rows, 14 pass / 31 partial / 18 missing (counted 2026-10-09).
- ~30 open `[GNOME 51 gap]` issues (tuna-os/tuna-desktop issue list, 2026-10-09).
- #545 MERGED (paired baseline: CPU 51–53% vs 17–19%, overview 339–428ms vs 161–211ms); #547 OPEN (budget gate); #273 OPEN (image qualification nightly).
- Prior scope specs a01f0012–a01f0016 (002 shell workflow, 003 hardware/app compat, 004 secure session/a11y, 005 extensions out, 006 perf/packaging/release with R1 paired benchmarks, R2 ≤10% regression rule, R5 packaging, R6 rollback, R8 evidence-linked claims).
- `docs/roadmap.md` tiers, R1–R16, Before-1.0 gate (002+003+004+006 acceptance).
- Repos: rust-wayland-desktop, tunaos, tunaos-packages, corral (registry roots verified). No spec design refs; no always-applied knowledge entries.

## Files examined

- `docs/roadmap.md` — tiers, R1–R16, Before-1.0 gate, spec registry.
- `docs/parity-ledger.md` (origin/main) — 63-row instrument, baseline rule.
- `docs/v1.md` — new mirror page (this program).
- `docs/architecture.md:54` — GNOME 51 Marlin baseline; Marlin first distro.
- `.github/workflows/ci.yml` — existing CI lanes hosting the new checks.
- `scripts/tuna-gtk-shell-proof` — per-capability proof gates feeding ledger rows.
- corral `Justfile` — vmtest lanes for validation runs.

## External references

None — all evidence is repo/issue-local by design.

## Prior plans / specs consulted

- Spec 002 shell-workflow-parity: overview, app grid/favorites, search, panel, workspaces, notifications in 1.0 scope.
- Spec 006 performance-packaging-release: R1–R8 (see evidence); v1 bar deliberately stricter than R2.
- Specs 003/004: hardware/app compat and secure-session/a11y scope per registry.
- Fresh-eyes review of the v1 spec: 1 duplication found and fixed.

## Open assumptions

- Marlin reference hardware/VM identity is declared in perf-lane docs — implement MUST verify before locking targets.
- Ledger/gap counts drift daily; re-count at execution.
- #547 attribution will cover GPU split; else methodology task adds it.
- Corral hosts qualification runs; verify when planning image tasks.

## Drafting assumptions

- Federated glue over central dashboard: per-instrument owners + small CI checks; aggregators rot, text-only drifts.
- Verdict schemas illustrative; exact schemas left to lane owners.
- Corral gets its own task (single-repo rule; VM-only regressions fail independently).
- Counts are snapshots; tasks re-count at execution.
- v1 perf bar vs 006 R2 tension flagged, not reconciled unilaterally.

## Rehydration cues

- Re-read the v1 spec via `spec file read`, `docs/v1.md`, `docs/roadmap.md` Before-1.0 section; check #545/#547/#273 states.
- Re-run ledger counts and the open gap-issue list at execution.
- Skills used: spek-new (spec), spek-plan (this plan).
