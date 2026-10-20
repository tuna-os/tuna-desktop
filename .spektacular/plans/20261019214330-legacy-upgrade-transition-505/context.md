---
created_date: "2026-10-19"
document_status: final
closed_date: "2026-10-19"
---

# Context: 20261019214330-legacy-upgrade-transition-505

## Current State Analysis

Baseline #775 removed the bridge and settings adoption; packaging README incorrectly claims saved-name migration. Runtime readers are mixed on relative XDG/HOME paths.

## Per-Phase Technical Notes

### Phase 1.1: Package the exact transition bridge
**File changes:** Add `share/compat/wayland-sessions/roost.desktop` (NoDisplay=true, current Exec/TryExec/identity) and `share/compat/pam.d/roost-lock` (regular old service with auth/account include login). Update installation/conffiles in `scripts/tuna-release` and backup/install arrays in `packaging/arch/PKGBUILD`. Add focused original-archive `scripts/lib/transition-package-policy.py` and genuine fixture tests; invoke before extraction in `scripts/check-release-package`. Update `scripts/check-deb-clean-install`, `scripts/lib/installed-deb-proof-tests.py`, `scripts/lib/deb-clean-install-tests.py`, `scripts/rename-to-tuna`, `scripts/lib/rename-to-tuna-tests.py`, `packaging/marlin/Containerfile`. Add necessary `.github/workflows/ci.yml`, `scripts/ci-select-lanes` and selector-test wiring. Do not change unrelated peer-auth/sha2 files.
**Complexity:** Medium. Package policy and staged content require independent review.
**Tests:** Real root-metadata .deb fixtures with native version-printing ELF binaries; original malformed/duplicate/traversal, forbidden old binary/session/PAM, symlink/dangling/missing/content negatives against the actual checker. Existing unit fixtures remain unit evidence. Check Arch staged-layout where practical, never claim pacman lifecycle from it.

### Phase 2.1: Adopt old-only settings safely
**File changes:** Add `crates/compositor/src/user_upgrade.rs`; export in `crates/compositor/src/lib.rs`; call from `crates/compositor/src/main.rs` after help/version/internal PAM exits and before desktop writers. Five pairs: config roost→tuna, cache roost→tuna, data roost→tuna and roost-shell→tuna-shell, state roost-shell→tuna-shell. Use existing rustix/libc; no additional runtime dependency.
**Behavior:** Inspect all pairs first. Neither/current-only no mutation; safe old-only directory gets atomic relative current→old symlink; exact already-adopted sibling link is idempotent. Distinct old/new, wrong type/owner, dangling/escaping links, unsafe ancestry stop with pair-specific recovery. Require absolute anchored paths for adoption; preserve current-only relative configuration when no adoption occurs, and report unsupported migration rather than redirecting data to another base. Ownership/other-writer checks include ancestor replacement with sticky semantics. Refuse privileged/set-id adoption. Recheck before create; late failure leaves only safe completed links, no rollback deletion.
**Complexity:** High. Filesystem safety and race handling require independent review.
**Tests:** Production module against disposable roots; all state cases/pairs, absolute XDG/HOME fallback, unsupported relative migration/current-only no-op, missing HOME, symlink/ownership/mode/sticky ancestry, permission errors and destination creation races. Before/after manifests verify bytes unchanged and downgrade paths available. Representative favorites/notification/monitor bytes through links are byte-path evidence; native reader assertions only when buildable.

### Phase 3.1: Verify lifecycle behavior and document limits
**File changes:** Add focused disposable lifecycle fixture suite under `scripts/lib/` and wire CI. Correct `packaging/README.md`; add `docs/legacy-upgrade.md`, link from install/release guidance. Write implementation test plan/changelog through CLI only.
**Tests:** Real dpkg explicit private root/admindir/log with no maintainer scripts: fresh/pre-rename/beta/beta→removed→repair, unchanged/modified/deleted/obsolete PAM, downgrade. Record tool versions/package hashes/status/bytes. Native private PAM uses pam_start_confdir and explicit deny fallback, with old-name process started before executable replacement but pam_start after replacement; prove retained/missing/denying policy. No real credentials/accounts or host PAM access.
**Qualification:** GDM saved-name login/reselection/second login, actual old compositor unlock, Arch replacement/deletion/downgrade and Marlin activation/rollback remain unqualified until native evidence. Documentation must not substitute sources or fixtures for those outcomes. Debian/Arch local removals differ; no forced confmiss/confnew or lifecycle cleanup hooks. Marlin retains prior login-capable deployment for future recovery tests.
**Complexity:** Medium. Run targeted Rust/Python/syntax/rename checks and required fmt/clippy/workspace attempts. Review the integrated source and evidence before publication, preserving unrelated main-branch changes.

## Testing Strategy
Phase 1 checks genuine archive policy and existing package unit regressions. Phase 2 checks production adoption behavior on real disposable filesystems. Phase 3 records real dpkg/private-PAM outcomes and native gaps, then runs regression and independent review.

## Project References
Approved migration spec under #505; merged #775 baseline; GDM 51 and package-manager primary sources recorded in research. No referenced design documents.

## Migration Notes
Never enumerate homes, modify host PAM, force session end, or overwrite/delete conflicting data. Preserve admin removals per manager; package-only tests do not qualify native login/unlock. Relative non-adoption states remain usable; unsupported automatic migration receives recovery guidance.

## Performance Considerations
The guard checks a fixed five pairs once before startup, without walking stored data trees. No steady-state writer or IPC overhead is introduced.
