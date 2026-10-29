---
created_date: "2026-10-09"
document_status: final
closed_date: "2026-10-09"
---

# Context: 20261009134949-7190f8bf-tuna-desktop-v1-definition

## Current State Analysis

The v1 instruments exist but none carries the verdict yet. The parity ledger on origin/main holds 63 rows (14 pass / 31 partial / 18 missing) against GNOME 51 as shipped in the Marlin GNOME image; ~30 `[GNOME 51 gap]` issues stand open. The paired baseline is recorded (#545 merged: Tuna trails at CPU 51–53% vs 17–19%, overview 339–428ms vs 161–211ms) while the enforcing gate (#547) and image qualification (#273) are still open. The victory checklist itself is new: the spec owns it, `docs/v1.md` mirrors it. Repos: rust-wayland-desktop (product code at its root), tunaos (image checkout), tunaos-packages (/home/ubuntu/dev/tunaos-packages), corral (VM validation).

## Per-Task Technical Notes

### Task: Computed victory checks
- `.github/workflows/ci.yml` — add check jobs reading open gap count, ledger pass fraction from `docs/parity-ledger.md`, budget verdict, qualification verdicts; publish one machine-readable signal.
- `docs/v1.md` — quote the signal per row instead of hand-edited text.

**Complexity**: Medium
**Token estimate**: ~25k
**Agent strategy**: Single agent, sequential execution.

### Task: Front-page victory link
- Project front page (README or docs index — follow whichever links sibling docs today) — one link to `docs/v1.md`.

**Complexity**: Low
**Token estimate**: ~5k
**Agent strategy**: Single agent, sequential execution.

### Task: Ledger close-out to all-pass
- `docs/parity-ledger.md` — flip rows to pass with proof links (recount at execution; was 14/31/18 on 2026-10-09).
- `crates/` + `scripts/tuna-gtk-shell-proof` — per-row capability work with tests and gates.

**Complexity**: High
**Token estimate**: ~200k
**Agent strategy**: Parallel analysis, sequential integration.

### Task: Gap-issue burndown to zero
- GitHub issue tracker (tuna-os/tuna-desktop, `[GNOME 51 gap]`) — close or ADR-defer each; rerun the GNOME 51 audit (shell, mutter, settings keys) until clean.
- `docs/parity-ledger.md` — row updates per closure.

**Complexity**: High
**Token estimate**: ~150k
**Agent strategy**: Parallel analysis, sequential integration.

### Task: GPU-split benchmark methodology
- Paired benchmark lanes (current baseline shape in #545) — record CPU/GPU-split timings plus total power per run on identical hardware/image family.
- Perf docs — state reference hardware and image family.

**Complexity**: Medium
**Token estimate**: ~40k
**Agent strategy**: 2-3 parallel agents for capture, attribution, docs.

### Task: Nightly perf budget gate
- `.github/workflows/` — complete #547 (nightly GNOME-relative budgets, per-phase/per-process attribution); wire verdict into the victory signal.

**Complexity**: Medium
**Token estimate**: ~30k
**Agent strategy**: Single agent, sequential execution.

### Task: Image qualification end to end
- tunaos: flavor qualification lanes per #273 — install, boot, real-greeter login nightly; recovery and upgrade proofs with artifacts.

**Complexity**: High
**Token estimate**: ~80k
**Agent strategy**: Parallel analysis, sequential integration.

### Task: Published-flavor gates
- tunaos-packages: publish gates — provenance, manifests, qualification results per release; block unqualified flavors.

**Complexity**: Medium
**Token estimate**: ~30k
**Agent strategy**: Single agent, sequential execution.

### Task: VM validation runs
- corral: `Justfile` vmtest targets — run qualification workloads (boot, login, recovery, performance) under VM validation.

**Complexity**: Medium
**Token estimate**: ~30k
**Agent strategy**: Single agent, sequential execution.

### Task: Declare v1
- A person reviews the fully green victory signal across all four repos, confirms every checklist row, and records the v1 declaration with the exact qualified revisions (spec, image flavor, package set). Check: the declaration exists and names revisions.

## Testing Strategy

Per-task granularity: computed checks get contract-style tests on their parsing (ledger fraction, issue count) against fixtures; benchmark methodology gets lane assertions (split fields present or the run fails validation); image tasks rely on their lanes' existing pass/fail plus artifact retention; the human sign-off is verified by the recorded declaration. No new unit tests for tracker schemas. Manual portions (checklist findability, any human-observed qualification) go into the implementation test plan.

## Project References

- Spec: `20261009134949-7190f8bf-tuna-desktop-v1-definition` (spec store; victory checklist authority).
- Program specs 002–006: shell workflow, hardware/app compat, secure session/a11y, extensions (out), perf/packaging/release.
- `docs/roadmap.md` (tiers, R1–R16, Before-1.0 gate), `docs/v1.md` (mirror), `docs/parity-ledger.md` (instrument).
- Issues: #545 baseline (merged), #547 budget gate (open), #273 image qualification (open).
- Repos: rust-wayland-desktop at `/home/ubuntu/dev/rust-wayland-desktop`; tunaos checkout under `.spektacular/repos/tunaos`; tunaos-packages at `/home/ubuntu/dev/tunaos-packages`; corral at `/home/ubuntu/dev/corral`.

## Token Management Strategy

| Tier | Token Budget | Agent Strategy |
|------|-------------|----------------|
| Low | ~10k | Single agent, sequential |
| Medium | ~25k | 2-3 parallel agents |
| High | ~50k+ | Parallel analysis, sequential integration |

Ledger close-out (~200k) and gap burndown (~150k) split per row-cluster at execution; total plan is a multi-wave program, not a single session.

## Migration Notes

None — no existing system is replaced; trackers keep their shape.

## Performance Considerations

The plan's own subject is performance evidence: every benchmark number must be paired (Tuna vs GNOME 51, identical hardware/image), carry the CPU/GPU split with total power, and beat-on-responsiveness/match-on-resources per the spec. The v1 bar deliberately exceeds spec 006 R2 (≤10% regression); flagged for stakeholder sign-off if they ever conflict.
