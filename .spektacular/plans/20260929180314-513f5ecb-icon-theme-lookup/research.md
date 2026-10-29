---
created_date: "2026-09-29"
document_status: final
closed_date: "2026-09-29"
---

# Research: icon theme lookup
## Alternatives considered and rejected

Toolkit lookup via GtkIconTheme (violates the binding GTK-free shell constraint) and resolve-on-fetch with icon bytes in paint keys (disk walk per tick, bloated keys); both rejected in architecture.
## Chosen approach — evidence

Central resolver module plus ShellHost cache keyed (theme, name, size): one lookup serves tray (`watcher.rs` fetch) and dock paint; misses keep initial fallbacks; theme flips clear cache and bump paint keys on the existing slow tick. resvg/usvg/tiny-skia present in the cargo cache (offline-feasible); no theme-lookup crate cached, so index.theme parsing is hand-rolled; `image` already decodes raster for the wallpaper hook.
## Files examined

shell-host watcher.rs (icon enum, paint, fetch), dock.rs (initials, fixtures), apps.rs (icon field), settings.rs (snapshot keys, refresh), panel.rs (paint keys, tick), popup.rs (layout mirror), overview.rs (accent, glyphs), workspace and crate manifests, Cargo.lock.
## External references

Freedesktop Icon Theme Specification (lookup order and hicolor fallback); resvg/usvg crate docs for the vector backend.
## Prior plans / specs consulted

settings-compat-shell-surfaces plan and spec (snapshot, tray host, dock, wallpaper precedent); icon-theme-lookup spec (requirements, constraints, metrics).
## Open assumptions

Cache on ShellHost with revision bumps; attention via Status==NeedsAttention; full re-tint for symbolic v1; pinned vector version resolves offline (else raster-only first).
## Drafting assumptions

- `resvg` 0.45/0.47 in the cargo cache resolves offline like
  `zbus` and `image` did; verify at implement time, fall back to
  raster-only with SVG logged if the version won't pin.
- `Status` property reads on the slow tick are cheap enough to
  join the existing per-item fetch; no new cadence.
- Theme Switches are rare: full cache clear on `icon_theme`
  change is acceptable (no per-name diffing).

### Cache lives on ShellHost (components)
- **Decision**: cache owned by host, not passed as resolver arg into paint fns
- **Rationale**: single invalidation point on theme change; paint keys read one revision

### Four tasks not five (tasks)
- **Decision**: tray and dock share one resolver/cache pair of tasks, no separate SVG task
- **Rationale**: vector decode is a resolver backend, independently untestable as a user outcome
## Rehydration cues

Resolver module beside watcher/dock; cache key (theme, name, px); theme key in snapshot; attention read on tick fetch; paint revisions in panel and dock keys; fixture theme trees in resolver tests; live private-bus stub extended with named/attention cases.
