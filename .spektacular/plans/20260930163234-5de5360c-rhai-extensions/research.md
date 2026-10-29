---
created_date: "2026-09-30"
document_status: final
closed_date: "2026-09-30"
---

# Research: 20260930163234-5de5360c-rhai-extensions

## Alternatives considered and rejected

### Option A: Lua instead of Rhai

**Rejected**: the draft decided Rhai and it stands uncontested; a bare
Rhai `Engine` has no I/O surface, matching allowlist-by-construction
better than a Lua stdlib that must be stripped.

### Option B: `notify`-crate file watching

**Rejected**: `notify` is not in the offline cargo cache; mtime
polling on the update pass needs no new dependency.

### Option C: Scripts on the paint/event paths

**Rejected** by spec constraint; painters render cached values only.

### Option D: Out-of-process script runner

**Rejected**: new supervision, IPC, and latency for isolation the
in-process allowlist already covers; High effort, no spec-required
gain.

### Option E: Running GJS/Mutter extensions

**Rejected** by spec non-goal.

## Chosen approach — evidence

- `rhai-1.26.1.crate` in the offline cargo cache: dependency
  resolves with no network.
- `crates/shell-host/src/tiles.rs:309` `Tile`/`TileSet`: system
  cells are ServiceKind-keyed; script cells are a parallel
  contribution.
- `crates/shell-host/src/watcher.rs:60` `IndicatorItem`: the shape
  script reactions target.
- `crates/shell-host/src/favorites.rs:104` `data_dir()`: XDG
  pattern the script directory mirrors.
- `crates/shell-host/src/dock.rs`: no badge concept; badges are new
  paint on items.

## Files examined

- `crates/shell-host/src/tiles.rs:309` — Tile/TileSet.
- `crates/shell-host/src/watcher.rs:60` — IndicatorItem.
- `crates/shell-host/src/favorites.rs:104` — data_dir().
- `crates/shell-host/src/dock.rs` — items, no badges.
- `crates/shell-host/src/panel.rs` — update passes (drive hook).
- `crates/shell-host/Cargo.toml` — no scripting deps.

## External references

- Rhai 1.26.1 (Engine, progress-hook budgets, Module, Scope) —
  cached crate; API confirmed at implement time.

## Prior plans / specs consulted

- `20260929180708-225d0ebc-rhai-extensions` (draft): full scope
  source; superseded, untouched.
- `20260930153534-e46a8d4a-overview-canvas-paint`: live-test and
  gate conventions reused.

## Open assumptions

See plan.md § Open Questions (rhai API surface, offline resolve,
paint seams). Implement STOPS on violations.

## Rehydration cues

- Re-read this file plus the signed spec; then `tiles.rs:309`,
  `watcher.rs:60`, `favorites.rs:104`.
- `tar tzf ~/.cargo/registry/cache/*/rhai-1.26.1.crate | head`.
- `cargo test -p roost-shell-host extensions`.

## Drafting assumptions

- Single registered repo; every task carries
  `Repo: rust-wayland-desktop`.
- No design references (verified zero refs/unresolved).
- Always-applied knowledge empty (verified).
- Four tasks use four of five minted ids; the spare is unused
  rather than inventing filler.
