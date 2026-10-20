---
created_date: "2026-10-19"
document_status: final
closed_date: "2026-10-19"
---

# Feature: 20261019214330-legacy-upgrade-transition-505

## Overview

Repair upgrades from the desktop's former name so existing users retain access to their remembered login session, can unlock a session already running during an upgrade, and keep their saved settings. Keep only the small temporary compatibility bridge necessary for those transitions, with explicit upgrade and recovery evidence before removing it.

## Requirements

- [ ] **Remembered-session access and explicit reselection**
  Users with a remembered legacy session retain access to Tuna Desktop and receive instructions to explicitly select Tuna Desktop; documentation does not claim automatic saved-name migration.
- [x] **Non-destructive user-state adoption**
  Users with old-only settings, including beta installed but never launched, retain those settings without overwrite, merging or deletion. Existing new-path data and already-adopted state remain intact.
- [x] **Safe failure and recovery**
  Unsafe ownership, symlink and conflicting-state cases stop with recovery guidance rather than silently creating replacement defaults.
- [ ] **Administrator authentication policy**
  Upgrades preserve administrator customizations and deliberate removals where the package manager permits; unavoidable temporary retention is explicit.
- [ ] **Running-session continuity**
  Users whose desktop was already running before the rename can retain access to its required authentication service across the package transition.
- [x] **Separate qualification and retirement**
  Debian, Arch and Marlin routes are qualified separately. Untested routes remain explicitly unqualified. Later bridge removal requires verified migration or safe upgrade prerequisites.

## Constraints

- Packages must temporarily retain exactly `/usr/share/wayland-sessions/roost.desktop` and `/etc/pam.d/roost-lock` as regular files, including on fresh installations; old binaries, environment aliases and other legacy session/authentication paths must remain removed.
- Qualification must use genuine package-policy and upgrade/downgrade tests and disposable fixture roots or VMs, never live user-machine or host authentication/session configuration.
- Upgrade handling must preserve existing user data without exception and must preserve administrator authentication customizations and deliberate removals where the package manager permits, explicitly documenting any unavoidable reinstatement.
- Adoption must not introduce privileged or cross-user access.
- The repair must not force logout or reboot.
- Native support claims must have actual route-specific evidence, including GDM login/reselection and actual old-compositor unlock; clean-install, private PAM-service and source-mock results cannot substitute for those outcomes.

## Acceptance Criteria

- [x] **Exact transition package contents**
  Packages containing the two regular transition files are accepted; forbidden old binary, session and authentication paths and dangling/invalid transition links are rejected. Fresh-install documentation discloses both retained files.
- [ ] **Saved-session continuity and reselection**
  A remembered legacy selection remains usable; explicitly selecting Tuna Desktop persists on a second login. Native GDM support remains explicitly unqualified until these outcomes are observed.
- [x] **Preserved settings and safe failure**
  Fresh, old-only, beta-installed-never-launched and previously adopted users retain access to the appropriate data. Existing data is not overwritten, merged or deleted; unsafe/conflicting states stop with recovery guidance; supported adoption leaves downgrade data available.
- [ ] **Configuration and running-session behavior**
  Upgrade and downgrade preserve unchanged, modified, deleted and obsolete authentication configuration according to the documented package-manager rules. A running old-service consumer can use retained service policy; a missing service and an intentionally denying local policy cannot be bypassed. Native old-compositor unlock remains explicitly unqualified until observed.
- [x] **Route-specific evidence and retirement criteria**
  Documentation identifies tested package/runtime versions and artifacts, separates Debian, Arch and Marlin outcomes, and lists gaps explicitly. Each bridge's removal requires verified migration or a safe prerequisite for its old consumers across the advertised routes.

## Technical Approach

- No additional technical mechanism has been mandated beyond the captured narrow bridge, settings safety and qualification boundaries; the implementation plan supplies the detailed design.

## Success Metrics

- All implemented package-policy and settings-safety tests pass, and forbidden mutations fail.
- No qualification test overwrites, merges or deletes pre-existing user data.
- Each advertised upgrade route has reproducible evidence or an explicit unqualified disposition; documentation contains no unsupported automatic-migration claim.

## Non-Goals

- Automatically rewrite every user's saved login choice.
- Publish a release.
- Change unrelated main-incident, peer-authentication or sha2 repair lanes.
