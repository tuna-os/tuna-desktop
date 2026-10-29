---
created_date: "2026-10-15"
document_status: final
closed_date: "2026-10-15"
---

# Feature: 20261015103717-bb9d15bd-tuna-desktop-all-tunaos-rollout

## Overview

Tuna Desktop ships today only as an experimental Marlin (Arch) flavor, built from a package pinned to an October 9 commit. This spec makes Tuna Desktop an installable desktop flavor on every TunaOS variant — all 14 — from one tagged release, so any TunaOS user can pick it alongside GNOME without a separate build path per distro.


## Requirements

- [ ] **Every variant offers a Tuna flavor**
  Users of yellowfin, albacore, skipjack, wahoo, bonito, hummingbird, sailfin, guppy, bonito-rawhide, gurnard, grouper, marlin, flounder and flounder-sid can switch to a `<variant>:tuna` image, unless that variant carries an explicit unsupported decision (see below).
- [ ] **One release feeds every variant**
  Each variant's image installs the Tuna Desktop package built from the same tagged desktop release, identified by version, source commit and signature.
- [ ] **Native packages per package target**
  The package factory builds signed Tuna Desktop packages for every package target the variants consume (arch, el10, eln, fedora, rawhide, hummingbird, opensuse, noble, ubuntu, trixie, debian, gentoo).
- [ ] **Real login on every variant**
  Each variant image boots to its greeter, lists Tuna Desktop, logs in with a password, and reaches a working session with portals and lock/unlock.
- [ ] **Unsupported variants are explicit**
  A variant/architecture that cannot qualify is published as unsupported with a reason, never silently shipped.
- [ ] **Switching keeps user data**
  Users switching from `<variant>:gnome` to `<variant>:tuna` (and back) keep their home directory and settings.


## Constraints

- Release pin is a tagged desktop release (vX.Y.Z or vX.Y.Z-beta.N), never an untagged commit.
- The TunaOS repo owns image flavors; tunaos-packages owns package recipes; tuna-desktop owns the source and its packaging files. No cross-repo copies of build logic.
- Package versions must order prereleases before finals on every target (Debian `~`, pacman hyphen-less, RPM `~`).
- No variant is admitted without qualification evidence from its own image digest.
- This spec supersedes the v1 definition spec's (20261009134949-7190f8bf-0001-tuna-desktop-v1-definition) 'Marlin image flavor only' constraint (owner decision 2026-10-10).


## Acceptance Criteria

- [ ] **Matrix all green or explicitly unsupported**
  The published variant matrix lists all 14 variants with a qualification result linked to an exact image digest; every cell is pass or a recorded unsupported decision.
- [ ] **Package provenance verifies**
  For each package target, the published package's version, source commit, vendor assets and signature verify against the release tag.
- [ ] **Greeter login works per variant**
  A qualification run per variant shows greeter listing, password login, a Tuna session, a portal-backed file chooser and lock/unlock.
- [ ] **Switch round-trip preserves data**
  A gnome→tuna→gnome bootc switch on one variant per package family keeps a seeded home directory intact.


## Technical Approach

- Reuse the epic #577 tracking: tunaOS #3195-#3207 per variant, tunaos-packages #828-#838 per target, PR #3226 as the matrix guard.
- Extend the existing tideforge recipe (`packages/tuna-desktop/package.yaml`, today `targets: [arch]`) rather than adding per-distro recipes.
- Qualification runs through Corral VMs so results are machine-readable and reproducible.
- Roll out in waves by package family (arch → fedora/el → debian/ubuntu → opensuse/gentoo), Marlin first.


## Success Metrics

- 14/14 variants pass or carry a recorded unsupported decision.
- Time from desktop tag to all variants published under one day once the matrix is green.


## Non-Goals

- Desktop feature work (owned by the v1 definition plan).
- aarch64 and non-x86_64 architectures unless a variant already ships them.
- ISO/installer matrix expansion.
