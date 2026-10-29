---
created_date: "2026-09-29"
document_status: final
closed_date: "2026-09-29"
---

# Context: icon theme lookup

## Current State Analysis

Tray and dock paint initials for named icons; pixmap blit, slow
tick, settings snapshot, and private-bus stub tests exist. The
snapshot lacks the icon-theme key and paint keys omit icon
revisions.

## Per-Task Technical Notes

## Resolver {#task-resolver}

New module beside watcher and dock: index.theme parsing over XDG
icon dirs plus `/usr/share/pixmaps`, size matching with hicolor
fallback, decode via the in-tree image library for raster and the
new vector dependency for SVG, file-size refusal bound, pure
`resolve(theme, name, px)` with no paint knowledge. Tests use
fixture theme trees.

## Snapshot and cache {#task-snapshot-cache}

Settings snapshot gains the desktop icon-theme string with
keep-on-absent semantics; host cache keyed (theme, name, size)
with total-size cap; theme flip clears cache and bumps panel and
dock paint revisions so bars repaint.

## Tray icons {#task-tray-icons}

`fetch_icon` resolves named icons through the cache, reads item
`Status` for attention selection (`NeedsAttention` picks
attention art with normal fallback), re-tints symbolic art toward
the shell accent at fetch time; paint path unchanged. Extend the
private-bus stub with named-only and attention cases.

## Dock artwork {#task-dock-artwork}

Dock paint resolves `AppEntry` icon names through the same cache
at cell size; misses keep initials. Fixtures gain icon-carrying
entries; paint tests cover resolved versus fallback.

## Testing Strategy

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

## Project References

Single repo rust-wayland-desktop: shell-host (watcher, dock,
settings, panel, popup, tiles, apps) plus the image decoding
precedent in the compositor wallpaper hook.

## Token Management Strategy

Resolver decode is bounded per call (size caps, file-size
refusal); cache is size-capped; no new timers or threads.

## Migration Notes

None: additive feature, no existing behavior changes except
artwork replacing initials where themes resolve.

## Performance Considerations

Lookups happen on the slow tick behind a cache; paint passes see
only cache hits. Theme swap cost is one full invalidation.

