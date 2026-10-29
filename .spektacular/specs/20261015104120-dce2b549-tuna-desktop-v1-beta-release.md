---
created_date: "2026-10-15"
document_status: final
closed_date: "2026-10-15"
---

# Feature: 20261015104120-dce2b549-tuna-desktop-v1-beta-release

## Overview

The full v1 bar (every GNOME 51 capability verified, faster than GNOME, all variants qualified) is weeks away, while nothing installable has ever been released. A v1 beta gives testers a tagged, installable Tuna Desktop now, with its gaps stated honestly, and exercises the release pipeline end to end before v1 itself.


## Requirements

- [ ] **Tagged prerelease**
  Maintainers can cut `v1.0.0-beta.N` from main; binaries report `1.0.0-beta.N` and packages order before `1.0.0`.
- [ ] **Installable on Marlin**
  Testers can switch to `marlin:tuna` built from the beta tag and log in through the real greeter.
- [ ] **Honest known limitations**
  The release notes list every non-passing ledger row and open release gate.
- [ ] **Published performance as measured**
  The notes include a fresh paired GNOME 51 run on the beta package, with no superiority claim it doesn't show.
- [ ] **Release pipeline exercised**
  The beta follows docs/release.md end to end (visual review, package check, media, walkthrough).


## Constraints

- The beta must be tagged from main once main CI is reliably green.
- Main CI, including nested stress, must pass on the exact tagged commit.
- The beta must not wait for PR #654 to land.
- A beta never claims a v1 gate as passed.
- Published as a GitHub prerelease.


## Acceptance Criteria

- [ ] **Tag and artifacts agree**
  The GitHub prerelease for the tag carries the package, notes and media; every binary's `--version` matches the tag.
- [ ] **Greeter login on the beta image**
  A recorded run of the `marlin:tuna` beta image shows greeter login into a Tuna session.
- [ ] **Limitations complete**
  Every ledger row not `pass` and every open v1 release gate appears in Known limitations.
- [ ] **Upgrade order holds**
  A package versioned 1.0.0 installed over the beta is treated as an upgrade by both dpkg and pacman.


## Technical Approach

- Use the prerelease versioning merged in PR #664 (tag `vX.Y.Z-beta.N` → binaries report `X.Y.Z-beta.N`, Debian `X.Y.Z~beta.N`, pacman `X.Y.Zbeta.N`).
- Bump the tunaos-packages recipe to the beta tag with the same mapping.
- Generate known limitations from docs/parity-ledger.md rather than hand-writing them.


## Success Metrics

- First installable tagged release exists; at least one external tester reports install success.


## Non-Goals

- Variants other than Marlin (all-TunaOS rollout spec).
- Closing v1 gates.
