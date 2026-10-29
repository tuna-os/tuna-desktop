---
created_date: "2026-09-30"
document_status: final
closed_date: "2026-09-30"
---

# Feature: 20260930215748-dba13c8a-release-demo-media-walkthrough

## Overview

Every Roost release automatically produces short video clips and screenshots of its core features, published as a docs walkthrough so people can see what the desktop is like before installing. The same media doubles as visual proof that features work and that fixes land from one release to the next.

## Requirements

- [ ] **One clip per core feature**
  Each release produces a short video clip for every core-tour feature: overview, app launch, tiling/scroll mode, quick settings, notifications, and lock screen.
- [ ] **Screenshots accompany clips**
  Each release produces screenshots matching the clips, usable as stills where video cannot play.
- [ ] **Media is captured with no manual work**
  Rebuilding the full clip and screenshot set requires no human recording.
- [ ] **Walkthrough page published per release**
  Users can open a docs walkthrough page for a release and watch each feature clip in order.
- [ ] **Media available as release assets**
  Users can fetch every clip and screenshot from the release.
- [ ] **Releases comparable over time**
  Users can open two releases' walkthroughs side by side and see the same feature set captured the same way, so fixes and regressions are visible.

## Constraints

- Capture must run fully automatically on release infrastructure; no manual recording step.
- No video or image binaries are committed to the repository; published media resolves to release assets.
- The published walkthrough must not depend on expiring CI artifacts.
- Capture reuses the scripted nested sessions; features outside scripted reach stay out until scripted.
- First coverage is the six-feature core tour.

## Acceptance Criteria

- [ ] **Six feature clips exist**
  A release carries one playable clip for each of overview, app launch, tiling/scroll mode, quick settings, notifications, and lock screen.
- [ ] **Stills match the clips**
  Every clip has a corresponding screenshot showing the same feature state.
- [ ] **No manual capture step**
  Rebuilding the media from a clean checkout with no human interaction produces the full set.
- [ ] **Walkthrough reads in order**
  The published walkthrough page presents the six features in a fixed order with each clip playable inline.
- [ ] **Binaries stay out of the repo**
  The repository contains no committed video or image binaries for the walkthrough; all media resolves to release assets.
- [ ] **Same shots every release**
  The same feature produces the same shots on consecutive releases unless the feature changed.

## Technical Approach

- Scripted capture sessions per core-tour feature, extending the existing nested-session journey scripts with screen recording plus stills.
- A capture script that runs the six feature sessions in a fixed order and emits one clip plus one screenshot per feature.
- CI uploads the media bundle as release assets; the docs walkthrough page embeds the assets per release.
- The release checklist gains a media step: capture, attach, link the walkthrough.

## Success Metrics

- A prospective user watches the six clips and understands what Roost is without installing anything.
- A maintainer compares two releases' walkthroughs and spots whether a reported fix landed.
- Media for a release is produced with no manual capture work.

## Non-Goals

- Instrumenting every journey script with media output.
- Narrated or edited marketing video; clips are raw feature captures.
- Hosting videos outside release attachments.
