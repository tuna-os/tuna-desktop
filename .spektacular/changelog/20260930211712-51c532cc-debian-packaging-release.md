---
created_date: "2026-09-30"
document_status: final
closed_date: "2026-09-30"
---

# Debian packaging and release process — project record

## What was built

Roost's first installable artifact: `scripts/roost-release` builds the four
release binaries with the version stamped from the git tag (`ROOST_VERSION`,
crate-version fallback) and assembles `roost_X.Y.Z_amd64.deb` via dpkg-deb,
with generated control metadata and a stamp-agreement gate. All four binaries
report `--version`/`-V` (12 unit tests). `scripts/check-release-package`
asserts contents, exec bits, and version agreement. `docs/release.md` holds
the maintainer checklist and notes template; `docs/install.md` documents the
package install/upgrade/uninstall flow. CI builds and checks the package on
every run.

## Why it matters

Installs were source-only (`cargo build` plus manual `install(1)` commands).
Testers on Debian/Ubuntu can now install with the system package manager,
and every release carries a matching version and notes.

## Deviations from the plan

None material. glib dependency declared as `libglib2.0-0t64 | libglib2.0-0`
to span bookworm and trixie/noble. EGL/GL/GBM stay in Depends although they
are dlopened at startup, matching the documented prerequisite set. No tags
exist yet, so the first verified artifact used the crate fallback (0.1.0).
