---
created_date: "2026-09-30"
document_status: draft
---

# Plan: demo-media-walkthrough

<!-- Metadata -->
<!-- Created: 2026-09-30T22:15:00Z -->
<!-- Commit: 40e0812 -->
<!-- Branch: main -->
<!-- Repository: https://github.com/hanthor/roost-desktop.git -->

## Overview

Every Roost release automatically produces short video clips and screenshots of its core features, published as a docs walkthrough so people can see the desktop before installing. The same media doubles as visual proof across releases.

## Conventions

No always-applied knowledge entries exist; conventions drawn from the codebase
and CI observed during discovery:

- POSIX shell scripts with `set -eu` and shellcheck gating: applies to the
  capture script.
- Journey-script interaction idiom (geometry check, window focus, key/mouse
  injection with settle sleeps, per-feature artifact names): the capture
  script mirrors it.
- Green gates: `cargo fmt --all`, `cargo clippy --workspace --all-targets --
  -D warnings`, `cargo test --workspace`.

## Architecture & Design Decisions

A new capture script drives six fixed-order feature sessions in the nested
compositor under Xvfb, recording each with ffmpeg x11grab while xdotool
replays the pinned interaction sequence, and grabs a matching still per
feature. The script emits one clip plus one screenshot per feature — the same
shots every release unless the feature changed — reusing the journey scripts'
proven interaction idiom (geometry check, window focus, key/mouse injection
with settles) rather than inventing a second harness style.

CI installs ffmpeg, runs the capture, and uploads the bundle as release
assets; the docs walkthrough page embeds the six clips in fixed order with
stills as fallback. The release checklist gains one media step (capture,
attach, link). This beats Wayland-native capture (rejected: the harness is
Xvfb-based) and committed binaries (rejected: bloat, attachments decided);
see `research.md#alternatives-considered-and-rejected`.

## Component Breakdown

- **Capture script** (new): drives the six feature sessions in fixed order under Xvfb, recording each with ffmpeg x11grab while replaying the pinned xdotool sequence, plus one still per feature. Owns shot stability: same interaction, same shots.
- **CI media job** (changed CI): installs ffmpeg, runs the capture, uploads the bundle as release assets. Reuses the journey job's display/input tooling.
- **Walkthrough page** (new docs): six features in fixed order, each clip playable inline with its still as fallback, resolving media to release assets.
- **Release checklist media step** (changed docs): capture, attach, link — added to the maintainer runbook.

## Data Structures & Interfaces

No new persistent data structures. The interfaces:

- **Capture script CLI contract**: fixed feature order, `--artifacts DIR` output layout (one clip plus one still per feature under stable names), `--help`, fail-fast with `set -eu`.
- **Media bundle contract**: stable per-feature filenames so the walkthrough page and release-to-release comparison never churn on paths.
- **Walkthrough page contract**: six sections in fixed order, each embedding its clip with the still as fallback, media URLs pointing at release assets.

## Implementation Detail

The plan introduces one new pattern: **scripted capture as a first-class
harness output**. The capture script mirrors the journey scripts' shape
(POSIX sh, `set -eu`, shellcheck-gated, xdotool interaction with settle
sleeps) with ffmpeg x11grab running alongside each feature segment instead of
scrot alone — one process tree per feature so a failed segment fails loudly
without poisoning the rest.

The walkthrough page follows the existing screenshots page shape (one section
per feature, media inline) but resolves media to release-asset URLs rather
than committed files. Tests follow repo shape: shellcheck for the script, a
contract test pinning the bundle layout and walkthrough section order, and CI
running the capture to prove it works end to end.

## Dependencies

- ffmpeg (x11grab) on the capture/CI machine: used, not changed; installed via apt alongside the existing display tooling.
- Xvfb, xdotool, scrot plus the nested session scripts: reused as the capture stage; no changes needed unless a feature needs a new scripted segment.
- Spec 20260930215748-dba13c8a-release-demo-media-walkthrough: source of truth; final.
- This spec carries no design references.

## Testing Approach

Shellcheck gates the capture script; a bundle-layout contract test asserts the
six stable clip/still names and the walkthrough's six ordered sections without
needing video tooling. CI runs the capture end to end, proving the pinned
sequences still produce media. Spec success metrics map directly: pre-install
preview becomes the published walkthrough page; release-to-release comparison
becomes the stable-name bundle; zero manual work becomes the scripted capture
itself.

Deliberate gaps: no automated visual-diff between releases (humans compare the
walkthroughs); no clean-runner media test outside CI (release checklist step).

## Milestones & Tasks

### Milestone 1: Six feature clips captured automatically

**What changes**: Running one command on a machine with a display produces a short clip plus a screenshot for each core-tour feature, with the same shots every time.

#### - [ ] Task: Build the feature capture script
**Id:** bf753444-b3a9-416d-b67a-53f9eea39772
**Repo:** rust-wayland-desktop
**Depends on:** none
**Execution:** agent

Create the POSIX capture script that drives the six core-tour feature sessions in fixed order, recording one clip plus one still per feature under stable names.

*Technical detail:* [context.md#task-capture-script](./context.md#task-capture-script)

**Acceptance criteria**:
- [ ] The script runs end to end and emits six clips plus six stills under stable names.
- [ ] The script passes shellcheck and fails fast on any failed segment.

### Milestone 2: Walkthrough published from release assets

**What changes**: The docs walkthrough page presents the six features in order with playable clips, resolving media to release assets with no binaries in the repo.

#### - [ ] Task: Publish the walkthrough page
**Id:** 10fe3113-e481-4a44-8127-97e491d4e9fa
**Repo:** rust-wayland-desktop
**Depends on:**
- bf753444-b3a9-416d-b67a-53f9eea39772 — Build the feature capture script
**Execution:** agent

Write the docs walkthrough page presenting the six features in fixed order with playable clips and still fallbacks resolving to release assets.

*Technical detail:* [context.md#task-walkthrough-page](./context.md#task-walkthrough-page)

**Acceptance criteria**:
- [ ] The page presents all six features in order with playable clips.
- [ ] No binary media is committed to the repo; all media resolves to release assets.

#### - [ ] Task: Pin the bundle and page contract in tests
**Id:** 556ea5bf-ef3c-4c22-be87-8f448a17e2a4
**Repo:** rust-wayland-desktop
**Depends on:**
- 10fe3113-e481-4a44-8127-97e491d4e9fa — Publish the walkthrough page
**Execution:** agent

Add a contract test asserting the stable bundle layout and the walkthrough's six ordered sections without needing video tooling.

*Technical detail:* [context.md#task-bundle-contract](./context.md#task-bundle-contract)

**Acceptance criteria**:
- [ ] The test asserts six stable clip/still names and six ordered walkthrough sections.

### Milestone 3: Release process and CI guard the media

**What changes**: The release checklist includes capture-attach-link, and CI captures and uploads the bundle so rot fails the build.

#### - [ ] Task: Add the media step to the release checklist
**Id:** e2c48c36-5c25-4d8f-b3d0-098d8684f45c
**Repo:** rust-wayland-desktop
**Depends on:**
- 556ea5bf-ef3c-4c22-be87-8f448a17e2a4 — Pin the bundle and page contract in tests
**Execution:** agent

Extend the release runbook with the capture-attach-link media step.

*Technical detail:* [context.md#task-release-media-step](./context.md#task-release-media-step)

**Acceptance criteria**:
- [ ] The checklist covers capture, upload as release assets, and walkthrough linking with no ad-hoc steps.

#### - [ ] Task: Guard media in CI
**Id:** e0e34e22-53d2-4a0d-a4ff-b9476e3e9f83
**Repo:** rust-wayland-desktop
**Depends on:**
- e2c48c36-5c25-4d8f-b3d0-098d8684f45c — Add the media step to the release checklist
**Execution:** agent

Add a CI step that installs ffmpeg, runs the capture, and uploads the bundle so media rot fails the build.

*Technical detail:* [context.md#task-ci-media](./context.md#task-ci-media)

**Acceptance criteria**:
- [ ] A full CI run with the new media step is green.

## Open Questions

No open questions. Resolved during planning: ffmpeg x11grab over Wayland-native
capture (see research.md); release attachments over committed binaries (user
decision); six-feature tour first (user decision).

## Out of Scope

- Instrumenting every journey script; narrated/edited video; non-attachment hosting.
- Automated visual diffing between releases (humans compare walkthroughs).
- Features unreachable from scripted nested sessions.
