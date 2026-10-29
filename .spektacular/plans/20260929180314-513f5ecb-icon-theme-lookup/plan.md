---
created_date: "2026-09-29"
document_status: final
closed_date: "2026-09-29"
---

# Plan: icon theme lookup

## Overview

Named tray and dock icons resolve through the system GNOME icon themes (raster and vector), with attention states, symbolic recolor, and theme-swap repaint; misses keep letter placeholders.
## Conventions

No repo-specific always-applied knowledge entries exist for this
plan's scope; the plan follows the codebase's established
patterns instead: wire-free core modules with tick-driven edges,
initial-letter fallbacks that never blank, paint-key revision
bumps on state change, and inline test modules with fixture
directories.
## Architecture & Design Decisions

## Options

### A. Central icon cache in a new `icons` module (chosen)

Hand-rolled Freedesktop theme lookup (`index.theme` parsing,
hicolor fallback) plus decode (`image` for raster, `resvg` for
SVG) behind one resolver; `ShellHost` owns a cache keyed
`(theme, name, px)`. `fetch_icon` and dock paint call the
resolver; misses return `Named` and keep the initial fallback.
Theme change (snapshot `icon_theme` flip) clears the cache and
the panel/dock paint keys.

- Pros: one lookup implementation serves tray and dock; disk
  reads bounded by cache; paint keys get a single revision
  source. Grounding: `watcher.rs:500`, `watcher.rs:185`,
  `panel.rs:829`, `settings.rs:118`.
- Cons: new module + cache invalidation paths; `resvg` adds
  compile weight.
- Effort: Medium.

### B. Resolve-on-fetch with icon bytes in paint keys

No central cache: every fetch reads theme dirs, and paint keys
carry icon bytes so changes repaint.

- Pros: no invalidation logic.
- Cons: disk walk per poll tick per item; paint keys bloat with
  pixel data; theme swap still needs a sweep.
- Effort: Low, but wasteful at runtime. Rejected.

### C. Toolkit lookup via GtkIconTheme (rejected)

Use the GTK icon-theme API through a backend.

- Pros: least code, fully spec-compliant lookup.
- Cons: violates the binding GTK-free shell constraint from the
  spec. Not an option, recorded only to close it.

## Decisions

- Cache ownership (research Q1): central `ShellHost` cache (A).
- Attention trigger (Q2): item `Status == "NeedsAttention"`
  selects `AttentionIcon*`, falling back to normal icons.
- Symbolic recolor (Q3): full re-tint of non-transparent pixels
  toward shell `ACCENT`; document as v1 semantics.
- Render path unchanged: resolved art flows through the existing
  `Pixmap` blit; nothing new in the paint passes.
## Component Breakdown

parsing, directory matching, hicolor fallback — and returns a file
path per (theme, name, size). It knows nothing about paint; it
answers "where is this icon" and decodes it to raw pixels.

Artwork cache (new, owned by the shell host): maps (theme, name,
size) to decoded pixels with a size cap. It invalidates wholesale
when the theme snapshot changes and feeds the paint-key revision
that repaints bars and docks.

Settings snapshot (changed): gains the desktop icon-theme key
alongside clock format and wallpaper, so theme switches arrive on
the existing slow tick with no new cadence.

Tray host (changed): asks the resolver for named icons and
attention icons (gated on item status), keeping the initial-letter
fallback for misses. Paint and menu paths are untouched.

Dock painter (changed): asks the resolver for app icon names the
same way, with the same fallback.

SVG rasterizer (new dependency): standalone vector renderer
feeding the resolver's decode step; raster formats reuse the image
decoding already in the tree.
## Data Structures & Interfaces

then hicolor fallback), sourced from the settings snapshot.

Resolved art (bytes plus size): decoded `B,G,R,A` pixels at the
requested cell size, ready for the existing pixmap paint path.
Lookup misses are not a type — the caller keeps its current
initial-letter fallback.

Artwork cache key (theme, name, size triple): the invalidation
unit. A theme change drops every entry; sizes are capped so one
giant request cannot bloat the cache.

Resolver interface: `resolve(theme, name, px) -> Option<Artwork>`
— pure lookup plus decode, no paint knowledge, no network, no
spawn. Attention selection stays with the caller (item status
decides which name to resolve).

Snapshot addition: the settings snapshot gains the icon-theme
string with keep-on-absent semantics, matching clock format and
wallpaper.
## Implementation Detail

modules, following their wire-free-core convention: lookup and
decode stay pure and unit-tested, while the host owns the cache
and the tick-driven refresh. Tray and dock painters call the same
resolver, so both surfaces gain artwork without new paint
patterns. Theme changes flow through the established snapshot
mechanism — the only new pattern is cache keying by theme, which
a reader will find wherever the paint-key revisions are built.
SVG support arrives as a decode backend inside the resolver, not
as a new subsystem.
## Dependencies

and dock paint the resolver plugs into — no changes needed.
- `image` crate (in tree): raster decode — no changes needed.
- Standalone SVG rasterizer (new dependency): vector decode —
  to be added; verify offline availability at implement time.
- Freedesktop Icon Theme specification: the lookup rules —
  read-only reference, no changes.
- System icon themes (Adwaita, hicolor): resolution sources —
  no changes.
- Design documents this plan was built on: none.
## Testing Approach

caps, recolor) against fixture theme directories, plus snapshot
key handling. Behavioural tests extend the existing private-bus
stub with named-only and attention icons, asserting bar pixels and
menu rows end to end. Paint tests assert resolved-art versus
initial-fallback rendering and focus-free repaints on theme swap.
Heaviest coverage sits on the resolver and the tray plug-in,
where the spec's artwork guarantees live. Deliberate gap: no
pixel-perfect theme-conformance suite — the fixture themes cover
representative layouts, not every theme on disk.

Success metrics map: stub artwork and theme-swap repaint become
behavioural tests; lookup latency within one tick becomes a timed
unit test over the fixture set.
## Milestones & Tasks

Tray items that provide only an icon name show real artwork in
the top bar, with attention states and symbolic recolor, while
unresolvable names keep their letter placeholders.
Validation: stub-client indicator test with named-only and
attention icons over the private bus.

## M2 Dock icons resolve

Pinned and running apps show theme artwork in the dock through
the same resolver, and theme switches repaint dock and bar
within one tick.
Validation: resolved-art versus fallback paint tests plus a
theme-swap repaint test.

#### - [x] Task: Theme resolver with raster and vector decode
**Id:** 09d43805-057e-44cf-979f-46a4e3aa056f
**Repo:** rust-wayland-desktop
**Depends on:** none
**Execution:** agent

New resolver answers icon name to decoded pixels through theme
lookup with hicolor fallback, raster decode, and vector
rasterization.

*Technical detail:* [resolver](#task-resolver)

**Acceptance criteria**:
- [x] Named icons resolve to pixels from fixture themes
- [x] Misses return cleanly with no fallback breakage
- [x] Oversized sources are refused within bounds

#### - [x] Task: Snapshot icon theme and host cache
**Id:** ef6476cb-3f1c-4a27-8199-a774119795ec
**Repo:** rust-wayland-desktop
**Depends on:**
- 09d43805-057e-44cf-979f-46a4e3aa056f — Theme resolver with raster and vector decode
**Execution:** agent

Snapshot carries the theme key; the host caches resolved art and
invalidates on theme change with paint-key revision bumps.

*Technical detail:* [snapshot-cache](#task-snapshot-cache)

**Acceptance criteria**:
- [x] Theme key applies and persists absence honestly
- [x] Theme switch clears cache and repaints

#### - [x] Task: Tray named and attention icons
**Id:** b2b120fe-c0d0-4a76-adb7-99368256e819
**Repo:** rust-wayland-desktop
**Depends on:**
- ef6476cb-3f1c-4a27-8199-a774119795ec — Snapshot icon theme and host cache
**Execution:** agent

Tray fetch resolves named icons and attention icons gated on
item status, with symbolic recolor and the initial fallback.

*Technical detail:* [tray-icons](#task-tray-icons)

**Acceptance criteria**:
- [x] Stub named-only item shows artwork in the bar
- [x] Attention state swaps artwork within one tick
- [x] Unknown names keep placeholders

#### - [x] Task: Dock artwork and repaint
**Id:** 0215facd-2976-4b5f-b0c3-7f0bc3c10836
**Repo:** rust-wayland-desktop
**Depends on:**
- ef6476cb-3f1c-4a27-8199-a774119795ec — Snapshot icon theme and host cache
**Execution:** agent

Dock paint resolves app icon names through the same cache and
repaints on theme switches.

*Technical detail:* [dock-artwork](#task-dock-artwork)

**Acceptance criteria**:
- [x] Pinned app shows theme artwork in the dock
- [x] Theme switch repaints the dock within one tick
## Open Questions

offline in the build environment — only discoverable by adding
the dependency and building. If it does not resolve, the
implementer implements raster-only lookup first (SVG names miss
cleanly) and reports back rather than vendoring sources.
## Out of Scope

per-app icon overrides, and a Roost-shipped icon set are out of
scope; the spec's Non-Goals section governs.

## Changelog

### 2026-09-29 — Task: Theme resolver with raster and vector decode

**What was done**: New wire-free `icons` module: Freedesktop
theme lookup (theme dirs, hicolor fallback, pixmaps catch-all,
exact-size preference), raster decode via the `image` crate, SVG
render via `resvg` with premultiplied-alpha unwrap, and full
re-tint for symbolic v1. 11 unit tests over fixture theme trees.

**Deviations**: none.

**Files changed**:
- `crates/shell-host/src/icons.rs`
- `crates/shell-host/src/lib.rs`
- `crates/shell-host/Cargo.toml`
- `Cargo.toml`
- `Cargo.lock`

**Discoveries**: `resvg` 0.48 resolves online (network was
available, so the offline-fallback assumption did not trigger);
`tiny-skia` pixmaps are premultiplied and need an unwrap pass
before the straight-alpha shm buffer. Directory-walk depth must
be tracked per stack entry, not with a global counter.

### 2026-09-29 — Task: Snapshot icon theme and host cache

**What was done**: Snapshot carries the desktop icon-theme key
(default Adwaita, keep-on-absent); the host caches resolved art
keyed (theme, name, size) with a 512-entry bound, syncs the cache
on the slow tick, and bumps panel and dock paint revisions on a
theme flip. Tests cover defaults, key apply/absence/blank, cache
clear-and-bump on flip, no-op when unchanged, and cache hits.

**Deviations**: none.

**Files changed**:
- `crates/shell-host/src/settings.rs`
- `crates/shell-host/src/panel.rs`

**Discoveries**: none beyond the plan.

### 2026-09-29 — Task: Tray named and attention icons

**What was done**: Tray fetch resolves named-only icons through the host cache and picks attention artwork when the item status is NeedsAttention, with symbolic re-tint at fetch time; unknown names keep placeholders.

**Deviations**: none.

**Files changed**:
- `crates/shell-host/src/watcher.rs`
- `crates/shell-host/src/panel.rs`

**Discoveries**: none.

### 2026-09-29 — Task: Dock artwork and repaint

**What was done**: Dock paint resolves desktop-entry icon names through the host icon cache at cell size with symbolic re-tint; misses keep the initial fallback. Theme flips drop cached art and rearm the dock repaint on the same tick.

**Deviations**: none.

**Files changed**:
- `crates/shell-host/src/dock.rs`
- `crates/shell-host/src/panel.rs`

**Discoveries**: none.
