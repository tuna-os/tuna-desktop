---
created_date: "2026-10-20"
document_status: final
closed_date: "2026-10-20"
project: rust-wayland-desktop
spec: 20261019214330-legacy-upgrade-transition-505
plan: 20261019214330-legacy-upgrade-transition-505
---

# Safer legacy upgrade transitions

Tuna Desktop keeps a small temporary bridge for remembered login selections and
already-running sessions, and safely makes old-only settings available under
the current names. Conflicting or unsafe settings stop with recovery guidance
rather than being replaced. Upgrade guidance now explains explicit session
reselection, local authentication-policy handling and recovery, while native
upgrade routes without real evidence remain clearly unqualified.

> Derived from project rust-wayland-desktop (tuna-os/tuna-desktop), spec/plan 20261019214330-legacy-upgrade-transition-505. See the project-level record for the full feature.

## What changed in this repository

- Retained exactly two regular temporary package files: the hidden old session
  entry and old lock-service policy. Old binaries and environment aliases remain
  removed; normal package-manager configuration protections remain in use.
- Added guarded startup adoption for five old-only settings pairs. Descriptor
  checks and atomic relative links preserve existing data, refuse unsafe or
  conflicting state, and leave safe partial work available for inspection/retry.
- Added genuine package-policy negatives and disposable dpkg/private-PAM
  lifecycle evidence, with bounded typed export after private fixture teardown.
- Corrected install, package, release and roadmap guidance and added an upgrade
  guide plus manual native qualification procedures and bridge-retirement gates.
- Repaired supporting package-staging and portal-artifact lifecycle tests and
  made two authentication disconnect fixtures deterministic, without changing
  production authentication policy.

## Why

The former-name cleanup lacked demonstrated continuity for remembered logins,
running old authentication consumers and old-only settings. This change repairs
the bounded mechanisms while preserving administrator decisions and making
untested desktop transitions visible.

## Evidence limits

At reviewed source `5764d5433d426cd72d04339420458e3c28230049`, 12 actual dpkg and
3 private-PAM cases passed alongside all 20 selected native CI jobs. The exact
candidate is version `0.1.0`, older than the published `1.0.0~beta.1`; the observed
beta-to-candidate direction is downgrade. Synthetic lifecycle controls and the
prestarted old-service helper are not native old-desktop proof. Subsequent
`f9a7db0df2e24b656f6fea634ebe68b4e26d4d22` changes only test fixtures and evidence
wording, with unchanged migration sources and passing native workspace/coverage
checks. Final published-head checks remain independently required before merge.

Actual GDM saved-name login/reselection, running pre-rename compositor unlock,
Arch replacement and Marlin activation/rollback remain unqualified. There is no
claim of automatic saved-selection migration or universally safe upgrades, and
no release was published. The project-level record and upgrade guide identify
the exact artifacts, source identities and recovery boundaries.
