---
created_date: "2026-10-09"
document_status: final
closed_date: "2026-10-09"
---

# Feature: 20261009134949-7190f8bf-tuna-desktop-v1-definition

<!--
  OVERVIEW
  A concise 2-3 sentence summary of the feature. Answer three questions:
    1. What is being built?
    2. What problem does it solve?
    3. Who benefits and why does it matter?
  Avoid implementation details — this should be readable by any stakeholder.
-->
## Overview

Tuna Desktop v1 means a desktop you can switch to without losing anything GNOME 51 does, and that feels faster while doing it: every tracked capability verified working, no known gaps left open, and responsiveness — opening the overview, reacting to input, playing animations — measurably quicker than GNOME on the same machine while using no more CPU. This definition turns today's scattered tracking — the parity checklist, the gap issues, and the performance baselines — into one checkable victory list, so everyone on the roadmap can see exactly what remains before v1 can be declared.

<!--
  REQUIREMENTS
  Specific, testable behaviours the feature must deliver.
  Format: bold title on the checkbox line, detail indented below.
  Rules:
    - Use active voice: "Users can...", "The system must..."
    - Each requirement should be independently verifiable
    - Focus on WHAT, not HOW — avoid prescribing implementation
    - Keep each item atomic — one behaviour per line
-->
## Requirements

- [ ] **Every tracked capability verifies as passing**
  A reviewer can see each tracked GNOME 51 capability marked verified by its own proof, with none left partial or missing.
- [ ] **No known gaps remain open**
  A reviewer can confirm every confirmed GNOME 51 gap is closed or explicitly deferred with a recorded decision, and a fresh audit finds no untracked user-visible capability.
- [ ] **Daily work needs no GNOME fallback**
  Users can launch apps, switch windows and workspaces, handle notifications, change settings, and lock/unlock entirely in Tuna Desktop without dropping back to GNOME.
- [ ] **Responsiveness beats GNOME on the same machine**
  Users get faster overview opens, input reaction, and animations than GNOME 51 on identical hardware, as the published benchmarks show.
- [ ] **Resource use matches GNOME**
  Users get that responsiveness without higher CPU, memory, or power draw than GNOME 51 under the same workload.
- [ ] **Comparisons account for where work runs**
  A reviewer can see head-to-head numbers built from user-visible results and total resource cost — frame timing, power, GPU and CPU together — so moving work between CPU and GPU reads as architecture, never as an unearned advantage or penalty.
- [ ] **Shipped image qualifies end to end**
  Users can install the published Marlin image flavor, boot it, log in through the real greeter, and see it pass its published qualification run.
- [ ] **Release safety nets hold**
  Users can recover from a broken package or configuration without losing data, over a proven upgrade path.
- [ ] **Victory list is public and current**
  Contributors can open the single victory checklist at any time and tell exactly what remains before v1.

<!--
  CONSTRAINTS
  Hard boundaries the solution must operate within. These are non-negotiable.
  Format: one bullet point per constraint.
  Leave blank if there are no constraints.
-->
## Constraints

- Baseline stays GNOME 51 as shipped in the Marlin GNOME image; parity and performance claims name no other version.
- Head-to-head numbers come from identical hardware and the same image family; cross-machine comparisons cannot declare victory.
- The v1 target is the Marlin image flavor next to its GNOME sibling, not a bare binary on arbitrary distributions.
- The victory checklist lives in this spec with a docs-page mirror; there is no third list.

<!--
  ACCEPTANCE CRITERIA
  The specific, binary conditions that define "done".
  Format: bold title on the checkbox line, verifiable detail indented below.
  Each criterion must be:
    - Independently verifiable (pass/fail, not subjective)
    - Traceable back to a requirement above
    - Testable by someone who didn't write the code
-->
## Acceptance Criteria

- [ ] **Checklist shows every capability verified**
  Opening the victory checklist shows every tracked capability marked verified with its proof linked, and none marked partial or missing.
- [ ] **Gap count reads zero**
  The open gap list is empty, and the latest audit report records no untracked user-visible capability.
- [ ] **A day's work completes without GNOME**
  A tester following the daily-driver script — launch, switch, notify, configure, lock — finishes every task without opening a GNOME session.
- [ ] **Responsiveness numbers beat GNOME**
  The published benchmark run shows faster overview, input reaction, and animations than the same run against GNOME 51 on identical hardware.
- [ ] **Resource numbers match GNOME**
  The same published run shows CPU, memory, and power at or under GNOME 51's figures for the same workload.
- [ ] **Methodology survives the GPU question**
  The published method records CPU and GPU cost separately alongside total power, and its conclusion still holds when work moves between them.
- [ ] **Image run passes end to end**
  The published image qualification shows install, boot, real-greeter login, and a passing run on the shipped flavor.
- [ ] **Recovery run restores data**
  A tester breaking the package and configuration per the recovery script gets a working desktop back with user data intact, over the published upgrade path.
- [ ] **Checklist is one click from the front door**
  A new contributor finds the victory checklist from the project's front page and can name what's left without asking anyone.

<!--
  TECHNICAL APPROACH
  High-level technical direction to guide the planning agent.
  Format: one bullet point per direction/steer.
  Leave blank if you want the planner to propose the approach.
-->
## Technical Approach

- Build the victory checklist on the existing tracking — the parity checklist, gap issues, and benchmark runs — rather than inventing a parallel system.
- Run all head-to-head measurement on the Marlin reference hardware and VM images, like for like against the GNOME sibling, reusing the existing performance lanes.
- Shape the benchmark methodology around the GPU question from the start: CPU and GPU cost recorded separately with total power, so the numbers stay meaningful whatever the compositor moves between them.

<!--
  SUCCESS METRICS
  How you will know the feature is working well after delivery. Be specific:
    - Quantitative: "p99 latency < 200ms", "error rate < 0.1%"
    - Behavioural: "users complete the flow without support intervention"
  Format: one bullet point per metric.
  Leave blank if not applicable.
-->
## Success Metrics

- Overview opens faster than GNOME's measured 161–211ms band on identical hardware, sustained across the published benchmark runs.
- Input reaction and animation timing beat the matching GNOME 51 figures on the same runs.
- CPU stays at or under GNOME's 17–19% under the same workload; memory and power draw do not exceed GNOME's figures.
- Open gap count reads zero, and the victory checklist shows every tracked capability verified.
- The published methodology records CPU and GPU cost separately with total power on every run, so the resource verdict survives the GPU split.

<!--
  NON-GOALS
  Explicitly state what this spec does NOT cover.
  Format: one bullet point per exclusion.
  Leave blank if there are no explicit exclusions to call out.
-->
## Non-Goals

- Extension broker and public API stay post-1.0 unless explicitly promoted; nothing in v1 depends on them.
