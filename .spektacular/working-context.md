# Working context

Rewritten 2026-10-29 to current state. Earlier session logs (2026-10-15 … 10-17: beta.2 capture, CI acceleration, Muse/Codex lanes, `/tmp/tuna-*` artifacts) were removed as stale; their durable outcomes are kept below. Git history of this file has the full text.

## Project direction
- Tuna Desktop (formerly Roost): Rust Smithay compositor + GTK4/libadwaita shell + greetd greeter, aiming for GNOME 51 parity on TunaOS. GitHub `tuna-os/tuna-desktop` (old `hanthor/roost-desktop` links redirect). Image flavor `marlin:tuna`; `marlin:roost` is no longer published.
- v1 = epic #577: every capability in `docs/parity-ledger.md` passes with proof, no open gaps, faster than GNOME 51 with no more guest CPU/memory on matched hardware, signed packages and qualified images across all 14 TunaOS variants, Corral qualification, public checklist (`docs/v1.md`, `victory-checks`) and owner declaration. ADR 0008: GPU/total-power comparisons waived; emulated-device proof acceptable if labelled; physical matrix post-v1. `v1.0.0-beta.1` is published (2026-10-15); betas tag from main.
- Owner direction (2026-10-29): work autonomously toward completing the roadmap and all epics; do not stop to ask. Owner delegates approvals (earlier: "make your best judgment and approve").

## Spektacular state
- Active workflow: implement `20261009134949-7190f8bf-0001-tuna-desktop-v1-definition`, step `implement`; all nine tasks unchecked. Its plan has a "Status refresh — 2026-10-29" section with current lane verdicts; trust that over older snapshots in the plan/context/research.
- No spec was started for new capability slices; work is tracked on GitHub issues under epic #577 instead.
- Registry: Corral runs on host `karnataka`; the AWS KubeVirt cluster is decommissioned. The tunaos clone (`.spektacular/repos/tunaos`) is a sparse checkout at TunaOS `main`, exposing only `.spektacular/knowledge/` from that repo's own store.
- `repo add --data '{"location": ...}'` takes the CODE folder, not the footprint; passing a footprint creates a nested `.spektacular/.spektacular`. Keep the desktop entry's registry location as `.` (portable; config.yaml is tracked).
- `.spektacular/tmp/v1-recovery/` holds the only recovered Codex transcripts from the 2026-10-15 working-context loss; keep it.

## Where work happens
- The root checkout is on `roadmap/docs-baseline-ledger` (upstream gone) with the user's uncommitted work. Never switch, reset or commit there. Do all code work in `.worktrees/<name>` created from `origin/main`; commit before removing any worktree.
- Build: `export PKG_CONFIG=/usr/bin/pkg-config CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR=<worktree>/target`. One target dir per worktree — a shared target dir let lanes compile against another lane's stale workspace crates. Host: 4 CPU / 11 GB RAM; heavy proofs (nested GTK, VM) run in CI only.
- Known host-only failures, identical on clean main: 19 `user_upgrade::tests::*`, `wallpaper::tests::xml_reference_replacement_changes_epoch_and_rejects_stale_worker`, and `tests/runtime.rs launch_smoke_binds_private_socket_or_skips_without_backend` in tuna-compositor.
- `.worktrees/ci-watch.sh RUN_ID...` waits for runs while tolerating GitHub API outages; plain `gh run watch` loops die on transient `error connecting to api.github.com`.
- Merge practice: exact-head CI green on every lane the change touches, then `gh pr merge --squash --match-head-commit <sha>`; record evidence on the issue. Never weaken assertions, floors or deadlines to go green.

## Current state (2026-10-29)
- Protocol `crates/shell-control-schema` CURRENT 0.39 on main; draft #814 bumps to 0.40. Only one lane at a time may change the wire schema.
- Landed: #810 mouse acceleration profile (merged `c777b107`; V-POINTER-LIVE flat→adaptive passed on the Marlin VM).
- Open drafts: #811 panel-menu BoxPointer open motion (`marlin-vm-orca51` failed, under diagnosis); #812 strip Super+drag installed proof (VM passed; gtk-shell artifact-upload ETIMEDOUT and coverage corrupt-profraw reruns pending); #814 remaining pointer-device policies; #815 Night Light gsd Color properties + PropertiesChanged. In progress: published-flavor nightly fix, focus policy (#347, compositor-side GSettings reader following `night_light_settings.rs`), brightness/PowerSaveMode (#355).
- Red on main: `marlin-tuna-nightly` / `published-flavor` (every run since ≥10-23); `Marlin performance samples` (#782). Coverage occasionally fails on a corrupt `.profraw` merge (seen on unrelated branches) — infrastructure, rerun.
- The 2026-10-13 per-capability audit on #577 is stale for several rows (OSD fades, strip pointer move/resize, Night Light preview/local schedule were already implemented). Verify every gap against current source first.
- Known parity defects to fix (not accept): OSD fade-reversal differs from GNOME (`osdWindow.js` lets fade-out finish); strip has one window per column, no window-height resize, centre-based drop slot, no client min/max hints, no Mod+right-drag resize.

## Durable learnings
- GTK tick callbacks must capture widgets through `glib::WeakRef` (nested-stress leak #658).
- QEMU sends absolute motion to the HID tablet but buttons to the current mouse; send both through one QMP mouse in VM fixtures.
- Only write a GSettings key on an actual user change; unconditional write-back saved stale keyfiles over newer keys (wellbeing/DND).
- Pin the Arch Linux Archive snapshot instead of moving the GNOME baseline when Arch updates GNOME (#668).
- In GitHub Actions Rust caching, a Cargo home inside the workspace makes the cache action drop all registry deps from its keep-set (tiny archives); keep Cargo home outside the workspace (PR #734).
- `gsettings get` prints enum values quoted (`'flat'`); compare accordingly in guest agents.
