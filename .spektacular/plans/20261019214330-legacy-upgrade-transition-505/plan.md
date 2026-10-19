---
created_date: "2026-10-19"
document_status: final
closed_date: "2026-10-19"
---

# Plan: 20261019214330-legacy-upgrade-transition-505

Created: 2026-10-19T22:30:52Z
Commit: 1d6e0c16
Branch: repair/legacy-upgrade-guard
Repository: tuna-os/tuna-desktop

## Overview

Repair legacy upgrades with a narrowly retained login/authentication bridge and non-destructive settings adoption. Existing users receive safe conflict/recovery guidance and honest route-specific evidence instead of an unsupported automatic-migration claim.

## Conventions

Repository contribution and verification rules remain applicable.

## Architecture & Design Decisions

The chosen direction is a narrow two-file package bridge plus guarded, unprivileged settings adoption in the registered rust-wayland-desktop repository. `share/compat/wayland-sessions/roost.desktop` preserves the remembered desktop identifier using `NoDisplay=true` and the current `tuna-session` command; `share/compat/pam.d/roost-lock` preserves the compiled service identity for old processes as a regular package-manager-protected file. Neither operation rewrites a saved selection or a running executable. Debian, Arch and Marlin preserve their distinct configuration and activation semantics.

`crates/compositor/src/user_upgrade.rs` prepares five old-only sibling directory pairs before session writers start. It uses descriptor-anchored checks and atomic relative links, preserves every existing byte, accepts the precise prior-adoption shape, and stops on conflicts or unsafe migration state with recovery guidance. Absolute, owned paths are required for adoption; unsupported relative environments are diagnosed only when migration would be needed, preserving fresh/current-only behavior. Ancestor safety accounts for sticky-directory replacement permissions. This medium-complexity option preserves downgrade access without copying or merging. Copying data would duplicate mutable state (high complexity); renaming data would compromise older readers (lower complexity but unacceptable safety trade-off).

The original-archive policy checker and genuine package tests in `scripts/` distinguish the exact two retained names from forbidden paths, including links and ambiguous archive names. Disposable dpkg and private-PAM tests prove their own mechanisms, while documentation in `packaging/README.md` and `docs/legacy-upgrade.md` leaves native GDM, old-compositor unlock, Arch replacement and Marlin activation unqualified. See [rejected alternatives](research.md#alternatives-considered-and-rejected) for why broad restoration, saved-session rewriting and destructive migration were rejected. Repository contribution and test boundaries still apply.

## Component Breakdown

- **Transition package assets:** preserve exactly the saved-session identifier and old PAM-service identity; package builders retain ordinary configuration protections.
- **Startup adoption guard:** resolves relevant roots, classifies all pairs, verifies ownership and ancestry, and creates only safe no-replace links before writers begin.
- **Original-archive policy:** validates the package compatibility allowance before extraction, alongside existing ownership/ELF/dependency checks.
- **Disposable lifecycle evidence:** actual dpkg, native private-PAM and filesystem fixtures establish observable mechanism outcomes and preserve before/after evidence.
- **Upgrade documentation:** explains explicit reselection, conflicts/recovery, manager-specific behavior, native gaps and measurable bridge-retirement prerequisites.

## Data Structures & Interfaces

- **Startup API:** a fallible adoption entry point returns success for untouched/safely adopted states and an actionable error for unsafe migration or conflicts. It does not expose a privileged migration service or modify settings formats.
- **Pair classification:** each legacy/current sibling pair is absent, current-only, eligible old-only, already-adopted, or blocked. Descriptor-backed eligible records are revalidated before creation.
- **Policy interface:** the package checker consumes an original `.deb` archive and exits nonzero with a path-specific diagnostic on a violation. No package schema or runtime IPC wire format changes.
- **Evidence ledger:** each test records command, version/artifact identity, observed result and qualification boundary; unexecuted native routes remain explicit.

## Implementation Detail

Keep migration isolated from settings readers: one startup guard inspects all five pairs before mutation and creates relative links only for safe old-only directories. Revalidate descriptors and entries before each atomic no-replace operation. On late failure, leave any created safe links intact and report recovery; deleting them during rollback would violate the preservation guarantee. Missing or unsupported environment does not invent a substitute path or rewrite unrelated readers.

Use regular compatibility assets rather than binary aliases or authentication symlinks. Package-manager conffile/backup behavior governs administrator changes; no maintainer migration hooks or global session rewrites are introduced. Original archive validation precedes extraction and uses precise path/content policy, while existing checker responsibilities remain intact.

Tests use the production safety module and actual package tooling. Private PAM initializes authentication after package replacement in a prestarted old-service process so it exercises the compiled name rather than a preloaded configuration. Documentation distinguishes these observable mechanisms from unavailable end-to-end desktop qualification.

## Dependencies

- Existing compositor rustix/libc dependencies provide anchored filesystem operations; no additional runtime service/library is planned.
- Existing Python 3, dpkg-deb, dpkg, compiler, ldd and desktop-file validation tools support genuine disposable tests; libpam with pam_start_confdir supports private policy tests.
- Native GDM/old-compositor, Arch pacman and Marlin deployment environments are required for their respective support claims and remain unavailable locally.
- Merged #775 is the repair baseline. Later disjoint main changes must be integrated before publication; their fixes are outside this patch.
- Design documents this plan was built on: none.

## Testing Approach

Use production-module real-filesystem tests for all five pairs, idempotence, conflict/unsafe states, relative-environment no-op versus blocked adoption, permissions, ancestry and creation races. Compare pre-existing bytes and entries before and after; new links are the only permitted migration mutation. This verifies the metric that qualification never overwrites, merges or deletes user data.

Build genuine small Debian packages and run the actual checker for accepted payloads and individual forbidden/ambiguous/link-path mutations. Exercise real dpkg isolated-root fresh/pre-rename/beta/removed-layout/repaired-layout upgrades and downgrades with unchanged, modified, deleted and obsolete conffiles. Run native private-PAM tests with a prestarted old-service executable across on-disk replacement, retained/missing/denying service cases. Together these behavioral checks verify implemented package-policy and safety tests pass and forbidden mutations fail.

Run existing focused Python/shell/rename tests and required Rust formatting, clippy and workspace tests; record environmental blocks separately. Existing fixture conventions remain useful for unit coverage, but mocks are not package lifecycle evidence.

Every advertised route receives a reproducible result or explicit unqualified disposition; review documentation for unsupported automatic-migration claims. Actual GDM saved-name login/reselection/second login, old-compositor lock/unlock, Arch replacement/downgrade and Marlin activation/rollback are **Manual — captured in the implementation test plan**. These tests require unavailable native environments and cannot be passed using the mechanism fixtures.

## Milestones & Phases

### Milestone 1: Retain the narrow login and authentication bridge
Users receive only the two transitional identities still needed by old saved/running sessions, with package-manager configuration protections. Validation: exact allowance accepts genuine packages and rejects every forbidden path case.

#### - [x] Phase 1.1: Package the exact transition bridge
**Repo:** rust-wayland-desktop
Retain the two old identities required by saved and running sessions. Reject every additional legacy compatibility artifact and preserve package-manager configuration behavior.
*Technical detail:* [context.md#phase-11](./context.md#phase-11-package-the-exact-transition-bridge)
**Acceptance criteria**:
- [x] Genuine valid packages contain exactly the allowed regular transition files.
- [x] Forbidden, malformed, linked and missing transition paths are rejected.
- [x] Debian and Arch declare configuration protections, and fresh-install checks expect only the narrow bridge.

### Milestone 2: Keep existing settings safely available
Old-only users retain their settings through guarded adoption; current-only and already-adopted states remain usable; conflicts fail with recovery guidance and no data mutation. Validation: production-module filesystem state and race tests.

#### - [x] Phase 2.1: Adopt old-only settings safely
**Repo:** rust-wayland-desktop
Preserve old-only settings using non-destructive startup adoption. Preserve fresh, current-only and already-adopted behavior; block conflicting or unsafe migration with actionable recovery information.
*Technical detail:* [context.md#phase-21](./context.md#phase-21-adopt-old-only-settings-safely)
**Acceptance criteria**:
- [x] All five old-only pairs remain accessible at current paths and to older readers.
- [x] No pre-existing byte or entry is overwritten, merged or deleted.
- [x] Unsafe/conflicting states fail before new settings writers start; idempotent and non-adoption states remain usable.

### Milestone 3: Make upgrade and recovery limits explicit
Users and reviewers can distinguish verified disposable mechanisms from unqualified native routes, with accurate recovery and bridge-retirement requirements. Validation: lifecycle evidence, focused regressions and independent code/evidence review.
#### - [ ] Phase 3.1: Verify lifecycle behavior and document limits
**Repo:** rust-wayland-desktop
Exercise disposable upgrades, downgrades and running old-service behavior, and explain observed results and safe recovery. Keep all unavailable native routes visibly unqualified and define evidence required to retire the bridge.
*Technical detail:* [context.md#phase-31](./context.md#phase-31-verify-lifecycle-behavior-and-document-limits)
**Acceptance criteria**:
- [ ] Actual dpkg and private-PAM evidence records positive, negative and administrator-policy outcomes without host changes.
- [ ] Documentation correctly explains explicit reselection, conflicts, package-manager distinctions and bridge retirement.
- [ ] Independent review receives the scoped patch, manifests, passing/failing checks and exact native gaps.

## Open Questions

No unresolved design questions. Package-manager outcomes and native capability gaps are observations to record, not assumed passes. Any evidence requiring a material scope change stops for owner approval.

## Out of Scope

- Automatically rewrite saved login selections or restore broad legacy binaries/environment aliases.
- Change unrelated incident, peer-authentication or sha2 repair work.
- Modify host/user-machine authentication or sessions, force logout/reboot, or publish a release/image.
- Claim native GDM, actual old-compositor, Arch or Marlin upgrade qualification without the separately required evidence.

## Changelog

### 2026-10-19 — Phase 1.1: Package the exact transition bridge

**What was done**: Restored only the regular hidden legacy session and protected PAM service, and enforced their exact contents and archive shape before extraction. Genuine package/checker and Arch staging tests independently pass (94 tests), including forbidden names, malformed archives, dangling links and permissive builder umasks.

**Deviations**: None. Archive/staging fixtures do not qualify installed lifecycle or native login/unlock behavior.

**Files changed**:
- `share/compat/wayland-sessions/roost.desktop`
- `share/compat/pam.d/roost-lock`
- `scripts/tuna-release`
- `packaging/arch/PKGBUILD`
- `packaging/marlin/Containerfile`
- `scripts/check-release-package`
- `scripts/check-deb-clean-install`
- `scripts/lib/transition-package-policy.py`
- `scripts/lib/transition-package-policy-tests.py`
- `scripts/lib/installed-deb-proof-tests.py`
- `scripts/lib/deb-clean-install-tests.py`
- `scripts/rename-to-tuna`
- `scripts/lib/rename-to-tuna-tests.py`
- `scripts/ci-select-lanes`
- `scripts/lib/ci-select-lanes-tests.py`
- `.github/workflows/ci.yml`

**Discoveries**: Writable critical archive ancestors required explicit rejection and builder directory modes; the negative tests now cover both. No broader convention was introduced. Latest main fb4abac3 was integrated without altering its unrelated fixes.

### 2026-10-19 — Phase 2.1: Adopt old-only settings safely

**What was done**: Added an unprivileged five-pair startup guard using pinned directory descriptors, no-follow checks, all-pair preflight and atomic no-replace relative links. Existing data remains intact on conflicts, unsafe paths and late failures; exact prior adoption and valid current-only relative directories remain usable. Dedicated and independent exact-production-module verification passes 39 filesystem/resolver/race tests and clippy, with full-workspace formatting and rename checks green.

**Deviations**: No design change. Full workspace clippy/tests are blocked by unavailable glib-2.0 development metadata. The module harness executes production code and real filesystems but does not qualify native compositor startup/settings readers. Foreign UID/root/set-id rejection tests model identities rather than changing real privileges.

**Files changed**:
- `crates/compositor/src/user_upgrade.rs`
- `crates/compositor/src/user_upgrade_tests.rs`
- `crates/compositor/src/lib.rs`
- `crates/compositor/src/main.rs`

**Discoveries**: The later state/tuna/session.json feature was introduced after the rename with its current name, so it is not a sixth legacy pair. The guard intentionally does not establish a boundary against later mutations by the same owner or root. Source-only startup ordering and external harness results remain distinct from native session proof.
