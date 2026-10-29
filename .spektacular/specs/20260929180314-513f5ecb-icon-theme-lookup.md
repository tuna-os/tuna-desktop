---
created_date: "2026-09-29"
document_status: draft
---

# Feature: 20260929180314-513f5ecb-icon-theme-lookup

## Overview

Roost resolves tray and dock icon names through the system's GNOME
icon themes, rendering real application icons in the top bar and
dock instead of letter placeholders. Everyday GNOME apps that only
provide an icon name will look correct, including attention states
and symbolic icons matched to the shell accent.

## Requirements

- [x] **Named tray icons render**
  A tray item that provides only an icon name shows that icon's
  artwork in the top bar instead of a letter placeholder.
- [x] **Named dock icons render**
  Pinned and running apps that provide only an icon name show that
  icon's artwork in the dock.
- [x] **Attention states show**
  Users can tell when an app requests attention (e.g. a messaging
  app with unread items) from its bar or dock icon.
- [x] **Symbolic icons match the shell**
  Symbolic-style icons render in the shell accent color so they read
  as part of the bar rather than foreign artwork.
- [x] **Unresolvable names degrade honestly**
  An icon name with no match anywhere keeps the current letter
  placeholder; the slot never renders blank or broken.
- [x] **Theme changes apply**
  Switching the system icon theme updates bar and dock icons without
  restarting the session.

## Constraints

- The shell process stays free of any GUI toolkit dependency; icon
  lookup and rendering use standalone libraries only.
- Lookups honor the Freedesktop icon theme layout and the user's
  configured theme order, falling back through hicolor.
- Lookup failures are silent in the session: no dialogs, no log
  spam beyond a single debug line per missing name.
- Decoded artwork is bounded in memory: oversized source files are
  refused rather than decoded.

## Acceptance Criteria

- [x] **IconName-only tray item shows artwork**
  A stub indicator exposing only an icon name displays that icon's
  pixels in the bar; no letter placeholder is visible.
- [x] **Dock shows app artwork**
  A pinned app with only an icon name displays that icon's pixels
  in the dock.
- [x] **Attention icon appears on request**
  When a stub item switches to its attention icon, the bar cell
  changes to the attention artwork within one poll tick.
- [x] **Symbolic icons use the accent**
  A symbolic icon's painted pixels match the shell accent color,
  sampled from the bar buffer.
- [x] **Unknown names keep placeholders**
  An item naming an icon that exists in no theme directory still
  shows its letter placeholder, with no error surfaced to the
  session.
- [x] **Theme switch refreshes icons**
  After changing the system icon theme, bar and dock icons show the
  new theme's artwork without a session restart.

## Technical Approach

Decided direction the planner may adapt: resolve names with a
small theme-index reader over the standard theme directories,
decode raster artwork with the image library already in use for
wallpapers, render vector artwork with a standalone SVG rasterizer,
and feed resulting pixels through the existing pixmap paint path so
initials remain the automatic fallback. Cache resolved artwork keyed
by theme and name, invalidated when the theme changes.

## Success Metrics

- A stub tray item exposing only an icon name shows that icon's
  artwork in the bar in a behavioural test.
- Switching the system icon theme updates all bar and dock icons
  within one poll tick, asserted by test.
- Name lookups for the full default tray set complete in under one
  slow-tick interval, so icons appear on first paint after
  registration.

## Non-Goals

- Shipping a Roost icon set: system themes are the source of truth.
- Animated icons: first frame only.
- Cursor themes: untouched by this feature.
- Per-app icon overrides or user icon pinning: future work.
