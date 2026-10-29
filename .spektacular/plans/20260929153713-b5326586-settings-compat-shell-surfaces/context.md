---
created_date: "2026-09-29"
document_status: draft
---

## Current State Analysis

The shell paints a top-anchored Activities panel with a hardcoded 24-hour clock and service-presence tiles; desktop enumeration, favorites, and control-socket window state already exist. No settings backend, dock, indicator host, or image background hook exists yet.

## Per-Task Technical Notes

### Task: Settings snapshot with clock-format read

Files: `crates/shell-host/src/` (new settings module beside `tiles.rs`), `tiles.rs::clock_text`, `panel.rs` slow tick. Complexity: Low. Strategy: snapshot struct behind the paint boundary; unit tests with injected backend values plus missing-backend defaults; round-trip test against a controlled settings store.

### Task: Calendar and status-menu popups

Files: `crates/shell-host/src/panel.rs`, `overview.rs::paint_panel`. Complexity: Medium. Strategy: popup content as new paint branches on the existing layer surface; reuse dismissal paths; test activation/dismissal and content rendering.

### Task: Dock surface with app lifecycle

Files: `crates/shell-host/src/` (new dock module), `apps.rs`, `favorites.rs`, control-socket window state. Complexity: High. Strategy: compose existing inputs; new layer surface with its own namespace; integration tests with real desktop entries in a temp dir; lifecycle against stub launches where a display is unavailable.

### Task: Indicator host and wallpaper hook

Files: `crates/shell-host/src/` (new indicator module, `zbus` server role), `crates/compositor/src/` (background image hook near `runtime.rs` clear path). Complexity: High. Strategy: watcher/item roles per the standard spec; stub client tests; wallpaper via snapshot URI with format probing and solid-color fallback.

## Testing Strategy

Unit tests cover the snapshot mapping (clock-format key to rendered text, missing-bus defaults) and popup/dock/indicator paint against fixed snapshots — no D-Bus in unit tests. Integration tests over the existing shell harnesses cover dock lifecycle against real desktop entries and indicator menu delivery. Heaviest coverage lands on the settings snapshot boundary, since every surface reads through it. Success-metric mapping: settings round trip → behavioural test with a controlled settings backend; dock task completion → behavioural test against real applications; indicator presence → behavioural test with a stub indicator client.

## Project References

- Spec `specs/20260929153713-b5326586-settings-compat-shell-surfaces.md` (binding contract).
- Knowledge `learnings/dconf-settings-interop.md` (decided here: honor, don't shim).

## Token Management Strategy

Single-repo, shell-host-focused slices; compositor touched only for the wallpaper hook. Keep each task's diff reviewable in one pass; no new workspace members planned.

## Migration Notes

No migration: new surfaces and schemas only. The hardcoded clock is replaced in place by the snapshot read with identical default output.

## Performance Considerations

Settings reads happen only on the existing slow tick; paint paths take snapshots by value and never block on D-Bus. Indicator icon payloads stay small (pixels + menu layouts).
