---
created_date: "2026-09-30"
document_status: draft
---

# Research: demo-media-walkthrough

## Alternatives considered and rejected

### Option A: wf-recorder / Wayland-native capture

Record via a Wayland capture protocol.

**Rejected**: the scripted sessions run nested under Xvfb with X input injection; x11grab records exactly what the harness drives with no new compositor-side protocol.

### Option B: Committing clips to docs/

Check walkthrough media into the repo.

**Rejected**: repo bloat; spec constraint and user decision both say release attachments.

### Option C: Instrumenting every journey script

Add media output to journey, app-content, scroll-proof.

**Rejected**: six-feature core tour first (user decision); the capture script drives its own sessions.

## Chosen approach — evidence

- scripts/roost-journey:83-230: geometry check, window search/focus, key/mouse injection with settles, scrot helper — the idiom the capture script mirrors.
- scripts/roost-docs-shots: `collect`/`regen` curation with fail-on-missing; CI freshness gate via git diff on docs/shots.
- `apt-cache policy ffmpeg`: 6.1.1 candidate — installable on runners.
- docs/screenshots.md: per-feature section shape the walkthrough follows.

## Files examined

- `scripts/roost-journey` (287+ lines) — interaction + stills idiom.
- `scripts/roost-docs-shots` (98 lines) — curation, stable names, freshness gate.
- `scripts/roost-app-content`, `scripts/roost-scroll-proof` — feature coverage.
- `docs/screenshots.md`, `docs/shots/` — existing stills page.
- `.github/workflows/ci.yml` — journey/app-content/scroll-proof/docs-shots jobs.
- `docs/release.md` — checklist to extend.

## External references

None.

## Prior plans / specs consulted

- specs/20260930215748-dba13c8a-release-demo-media-walkthrough.md — source-of-truth spec.
- specs/20260930211712-51c532cc-debian-packaging-and-release-process.md — release process owner.

## Open assumptions

- ffmpeg x11grab against the Xvfb display works on ubuntu-latest runners; verified at implement time by the CI run.
- Lock-screen segment scriptable via the session-lock harness hooks; confirmed at implement time.

## Drafting assumptions

- ffmpeg x11grab as the recorder: matches the Xvfb-based harness; no new
  compositor protocol.
- Stable per-feature bundle filenames: walkthrough links and
  release-to-release comparison never churn on paths.
- Walkthrough resolves media to release assets: keeps binaries out of the repo
  per the spec constraint and user decision.
- Capture as its own script reusing journey idiom rather than modifying every
  journey script: six-feature scope stays contained.

## Rehydration cues

- Re-read the spec for scope; plan context for file detail.
- Capture needs a display: Xvfb on CI, real session locally.
