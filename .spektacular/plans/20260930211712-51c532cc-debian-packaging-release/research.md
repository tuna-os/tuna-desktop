---
created_date: "2026-09-30"
document_status: draft
---

# Research: debian-packaging-release

## Alternatives considered and rejected

### Option A: cargo-deb manifest-driven packaging

Manifest-driven .deb generation from Cargo metadata.

**Rejected**: adds a build-time tool that must itself be fetched, violating the declared-dependencies-only build constraint; hides the staging layout the spec requires to match the documented install locations.

### Option B: Full debian/ source package (debhelper/dh)

Maintainer-style source packaging for distro archives.

**Rejected**: archive-inclusion overhead for a deliverable that is one installable artifact plus checklist; a hosted apt repo is an explicit non-goal. Revisit if a PPA becomes a goal.

## Chosen approach — evidence

- `dpkg-deb` present on dev machine and Ubuntu CI runners; `cargo-deb` not installed — zero new tooling needed.
- scripts/roost-nested header: POSIX sh, `set -eu`, shellcheck-gated in ci.yml journey job — the convention the release script follows.
- docs/install.md documents the exact staging layout (four binaries side by side, roost.desktop session entry).
- crates/compositor/src/main.rs:19 and crates/compositor/src/bin/roost-session.rs:51 show hand-rolled arg parsing — `--version` follows suit with no new dependency.

## Files examined

- `Cargo.toml` — workspace members, no version plumbing.
- `crates/compositor/Cargo.toml`, `crates/greeter/Cargo.toml`, `crates/shell-host/Cargo.toml` — all 0.1.0.
- `crates/compositor/src/main.rs:19` — hand-rolled arg loop.
- `crates/compositor/src/bin/roost-session.rs:51` — arg scan, `--help` precedent.
- `crates/shell-host/src/main.rs`, `crates/greeter/src/main.rs` — binary entry points.
- `share/wayland-sessions/roost.desktop` — session entry, `Exec=roost-session`.
- `docs/install.md` — source install + layout + greetd wiring.
- `.github/workflows/ci.yml` — check + journey jobs, shellcheck gate.
- `scripts/roost-nested` — script style convention.

## External references

None.

## Prior plans / specs consulted

- specs/20260930211712-51c532cc-debian-packaging-and-release-process.md — the source-of-truth spec.
- specs/20260927170322-a01f0016-performance-packaging-and-release.md — parent epic (R5 packaging, R7 provenance).

## Open assumptions

- Exact Debian dependency package names verified at implement time against bookworm/trixie and noble; if names differ, the implement run records the matrix.
- amd64 first; arm64 follow-up.

## Drafting assumptions

- Binary package named `roost`: short, matches the session/desktop-file name;
  alternative `roost-session` rejected as redundant with the session concept.
- First slice targets amd64 only: matches current dev/CI machines; arm64 parked
  as out of scope rather than designed now.
- `ROOST_VERSION` environment variable as the injection point: no new files, no
  build.rs, works with plain `cargo build`; crate version as fallback keeps
  unstamped dev builds working.
- Exact Debian dependency package names verified at implement time against
  bookworm/trixie and noble rather than pinned now: avoids asserting names that
  differ across releases.

## Rehydration cues

- Re-read specs/20260930211712-51c532cc-debian-packaging-and-release-process.md for scope.
- Re-read .spektacular/tmp or plans/debian-packaging-release/context.md for file:line detail.
- `git describe --tags` needs a tag to stamp a real version; dev builds use the crate fallback.
