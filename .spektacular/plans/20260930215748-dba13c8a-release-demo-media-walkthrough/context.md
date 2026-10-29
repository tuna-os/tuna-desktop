---
created_date: "2026-09-30"
document_status: draft
---

# Context: demo-media-walkthrough

## Current State Analysis

Capture stack is Xvfb + xdotool + scrot stills (scripts/roost-journey pattern); no video capture anywhere. ffmpeg 6.1.1 available via apt, not installed. docs/shots + docs/screenshots.md hold committed stills curated by scripts/roost-docs-shots; the release walkthrough will use release attachments instead. Six core-tour features; journey covers overview/search/launch, scroll-proof covers tiling/scroll; quick settings, notifications, lock need new scripted segments. Release process in docs/release.md; package guard in CI journey job.

## Per-Task Technical Notes

### Task: Build the feature capture script {#task-capture-script}

- New `scripts/roost-capture`: POSIX sh, `set -eu`, `--help`, `--artifacts DIR`.
  Per feature (overview, app-launch, tiling-scroll, quick-settings,
  notifications, lock-screen): start ffmpeg x11grab on the Xvfb display in the
  background, replay the pinned xdotool sequence (mirror
  scripts/roost-journey focus/key/sleep idiom), stop ffmpeg, `scrot` the still.
  Stable names: `<feature>.mp4` + `<feature>.png`. Fail fast per segment.
- Needs ffmpeg + existing journey deps; lock-screen segment needs the lock
  flow scripted (check the session-lock plan's harness hooks first).

**Complexity**: Medium
**Token estimate**: ~15k
**Agent strategy**: Single agent; test with `--help` plus shellcheck locally (no display needed for lint).

### Task: Publish the walkthrough page {#task-walkthrough-page}

- New `docs/walkthrough.md`: six sections in fixed order mirroring
  docs/screenshots.md shape, each with clip embed + still fallback. Media URLs
  target release assets (placeholder pattern documented for the first release).
- No binaries committed.

**Complexity**: Low
**Token estimate**: ~6k
**Agent strategy**: Single agent; review against spec criteria.

### Task: Pin the bundle and page contract in tests {#task-bundle-contract}

- Static contract test (release_contract.rs pattern): stable filenames exist
  in the script, six sections in order in the walkthrough, no binary media
  committed under docs (walkthrough assets excluded).

**Complexity**: Low
**Token estimate**: ~6k
**Agent strategy**: Single agent; `cargo test` subset.

### Task: Add the media step to the release checklist {#task-release-media-step}

- docs/release.md: capture → upload as release assets → link walkthrough.

**Complexity**: Low
**Token estimate**: ~4k
**Agent strategy**: Fold into the walkthrough task execution.

### Task: Guard media in CI {#task-ci-media}

- ci.yml journey-adjacent step: apt install ffmpeg, run capture, upload bundle
  artifact. Reuses journey job's display tooling.

**Complexity**: Low
**Token estimate**: ~5k
**Agent strategy**: Single agent; mirror the package-guard step shape.

## Testing Strategy

Shellcheck gates the capture script; a bundle-layout contract test asserts the
six stable clip/still names and the walkthrough's six ordered sections without
needing video tooling. CI runs the capture end to end, proving the pinned
sequences still produce media. Spec success metrics map directly: pre-install
preview becomes the published walkthrough page; release-to-release comparison
becomes the stable-name bundle; zero manual work becomes the scripted capture
itself.

Deliberate gaps: no automated visual-diff between releases (humans compare the
walkthroughs); no clean-runner media test outside CI (release checklist step).
## Project References

- Spec: specs/20260930215748-dba13c8a-release-demo-media-walkthrough.md (final).
- User decisions: release attachments, six-feature core tour.
- Repo: rust-wayland-desktop at /home/ubuntu/dev/rust-wayland-desktop.
- No design references; no packaging knowledge entries.

## Token Management Strategy

| Tier | Token Budget | Agent Strategy |
|------|-------------|----------------|
| Low | ~10k | Single agent, sequential |
| Medium | ~25k | 2-3 parallel agents |
| High | ~50k+ | Parallel analysis, sequential integration |

Capture script is Medium; the rest Low; execute sequentially in dependency order.

## Migration Notes

N/A — new capability, no migration.

## Performance Considerations

Capture runs six nested sessions plus ffmpeg encoding; CI job needs a generous timeout (mirror the 25-minute journey jobs).
