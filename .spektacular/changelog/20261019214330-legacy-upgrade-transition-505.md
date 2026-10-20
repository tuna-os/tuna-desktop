---
created_date: "2026-10-20"
document_status: final
closed_date: "2026-10-20"
---

# Legacy upgrade transition repair

## What was built

Tuna Desktop temporarily retains exactly the hidden old session entry and the
old lock-service policy so remembered logins and old running consumers still
have their required names. Old binaries and environment aliases remain removed.
A startup guard safely adopts five old-only settings directories with relative
links after ownership, ancestry, conflict and race checks; it never overwrites,
merges or deletes existing data. Unsafe state stops with recovery guidance.

Genuine original-archive tests enforce the exact compatibility allowance and
reject malformed, linked and forbidden package paths. Debian conffiles and Arch
backup declarations preserve ordinary administrator-policy handling. Real
disposable dpkg and private-PAM tests record unchanged, modified, deleted,
obsolete, missing and denying-policy outcomes, including the changed-policy
prompt and explicit keep-local recovery control. Bounded typed manifests are
exported only after private fixtures finish; private configuration and arbitrary
command output are excluded.

The upgrade guide explains explicit Tuna Desktop reselection, manager-specific
configuration handling, settings conflicts, downgrade access and bridge-removal
gates. A manual test plan covers native GDM, actual old-compositor unlock, Arch
replacement/downgrade and Marlin activation/rollback. Those four routes remain
unqualified until genuine native observations are independently reviewed.

## Why it matters

Removing all former-name paths had left remembered-session, running-service and
old-only settings transitions without demonstrated recovery. This bounded repair
keeps the minimum required names and preserves data while exposing evidence gaps
instead of claiming automatic migration or universal upgrade support.

## Verification and provenance

At source `5764d5433d426cd72d04339420458e3c28230049`, all 20 selected native CI jobs
passed. [Lifecycle artifact 12231976713](https://github.com/tuna-os/tuna-desktop/actions/runs/39347223962/artifacts/12231976713)
contains 12 real dpkg and 3 native private-PAM passing cases; ZIP SHA-256 is
`06fdbbc25164a8a9e162a7689468c7787d8512accb02ebf8ceb2308249a760c0`.
The exact-built candidate SHA-256 is
`4cf37b83c8b4b13f4f240a4565f26ffdf45dfe5b6d60f295f674b962030c84d6`;
the original published beta SHA-256 is
`b2b9ec7c9c5eb9f0e2003e48e458a440a551836d31a831972b8561a43e15ed01`.
The candidate version `0.1.0` is older than beta `1.0.0~beta.1`: beta-to-candidate
is dpkg downgrade, and the reverse is upgrade. A beta-to-newer release candidate
requires separate artifact-bound evidence. Synthetic packages isolate
pre-rename, removed-bridge and changed-policy controls; the prestarted native-PAM
consumer is a synthetic helper, not the actual old compositor.

Head `f9a7db0df2e24b656f6fea634ebe68b4e26d4d22` changes only two authentication test
fixtures (13 inserted lines) and upgrade-guide evidence wording from that tested
source. Migration production, lifecycle and exporter bytes are unchanged; its
native fmt/clippy/workspace and coverage checks passed. Prior failed coverage
attempts remain failures. Later final-head checks are not claimed here and must
all pass before merge. Detailed source hashes and check links are in the plan's
phase 3 implementation record and the upgrade guide.

## Deviations from the plan

No migration design change. Supporting backlog-validation repairs made package
staging deterministic under permissive umask, retired portal writers before
artifact ownership/collection, and made two disconnect test fixtures explicitly
shut down their shared socket endpoint. No production authentication policy was
changed. The original beta PAM bytes are retained exactly to avoid needless
conffile decisions. Native support and correctly versioned release qualification
remain separate gates; no release or live-user operation was performed.

## Affected repository

`rust-wayland-desktop` / `tuna-os/tuna-desktop` is the sole affected repository.
The phase-by-phase file list and outcomes are recorded in plan
`20261019214330-legacy-upgrade-transition-505`; its test-plan artifact identifies
the still-unexecuted native procedures.
