---
created_date: "2026-09-30"
document_status: final
closed_date: "2026-09-30"
---


# Feature: 20260930211712-51c532cc-debian-packaging-and-release-process

<!--
  OVERVIEW
  A concise 2-3 sentence summary of the feature. Answer three questions:
    1. What is being built?
    2. What problem does it solve?
    3. Who benefits and why does it matter?
  Avoid implementation details — this should be readable by any stakeholder.
-->
## Overview

Roost gains its first installable package for Debian and Ubuntu systems, so testers can install and uninstall the desktop with the system package manager instead of compiling from source. Each release also gets a version number and release notes, so users know what changed and which version they run. This turns today's build-it-yourself session into something shippable for the first time.

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

- [ ] **Install with the system package manager**
  Users can install the Roost desktop session from a package file with the system's package manager, with dependencies resolved automatically.
- [ ] **Uninstall cleanly**
  Users can remove the package and the session entry disappears from the login screen, leaving no Roost-owned program files behind; user configuration and application data are preserved.
- [ ] **Session appears at login**
  After installing the package, the Roost session is selectable from the login screen and launches the full desktop.
- [x] **Versioned releases**
  Every release carries a version number derived from a single source of truth, and the installed binaries report that version.
- [x] **Release notes published**
  Every release ships notes describing what changed, known limitations, and the supported configurations.
- [x] **Repeatable release process**
  A maintainer can cut a release by following a documented checklist that produces an installable, version-stamped package from a clean checkout.
- [ ] **Upgrade preserves user data**
  Upgrading the package to a newer version keeps user configuration and application data intact.

<!--
  CONSTRAINTS
  Hard boundaries the solution must operate within. These are non-negotiable.
  Format: one bullet point per constraint.
  Examples:
    - Must integrate with the existing authentication system
    - Cannot introduce breaking changes to the public API
    - Must support the current minimum supported runtime versions
  Leave blank if there are no constraints.
-->
## Constraints

- Must not replace the display manager or core system services; the package integrates with the existing login stack.
- The session's binaries (compositor, shell host, greeter, session launcher) must install side by side in the same directory so session startup keeps working.
- The version reported by the binaries, the package metadata, and the release notes must agree.
- The build may fetch only declared build dependencies; it must not fetch other resources at build time.
- The package targets current Debian stable and Ubuntu LTS releases.

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

- [ ] **Package installs on a clean system**
  On a clean supported system without dependencies pre-installed, the package installs without errors with dependencies resolved automatically, and all session binaries are present and executable.
- [ ] **Login screen lists Roost**
  After installation, the login screen's session list includes Roost, and selecting it starts the desktop session.
- [ ] **Removal leaves nothing behind**
  After uninstalling the package, no Roost-owned binaries or session files remain on the system, while user configuration and application data are preserved.
- [x] **Binaries report the release version**
  Each installed binary reports the release version, and it matches the version in the release notes.
- [x] **Release notes ship with every release**
  Every release includes notes listing changes, known limitations, and supported configurations.
- [x] **Checklist cut from clean checkout**
  Following the documented release checklist on a clean checkout produces an installable, version-stamped package.
- [ ] **Upgrade keeps user data**
  Installing a newer package version over an older one preserves existing user configuration and application data.

<!--
  TECHNICAL APPROACH
  High-level technical direction to guide the planning agent.
-->
## Technical Approach

- A Debian package that installs the session binaries and the login-screen session entry in the system's standard locations, with the release version stamped from the git tag at build time.
- A maintainer-run release checklist that builds, stamps, and assembles the package plus notes from a clean checkout.
- CI builds the package to keep it from rotting.

<!--
  SUCCESS METRICS
-->
## Success Metrics

- A first-time user installs Roost from the package without touching a compiler.
- Every release has an unambiguous version visible in the product and the notes.
- A maintainer cuts a release in one checklist pass with no ad-hoc steps.

<!--
  NON-GOALS
-->
## Non-Goals

- Non-Debian package formats (RPM, Arch, tarball installers).
- Rollback and safe-session recovery after failed upgrade.
- Benchmarks, soak testing, SBOM, and the full release dossier.
- A hosted package repository (PPA/apt repo); the artifact plus checklist is the deliverable.
- Extension distribution or store mechanics.
