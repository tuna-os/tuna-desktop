---
created_date: "2026-09-30"
document_status: final
closed_date: "2026-09-30"
---

# Context: 20260930054036-d6050cbc-xwayland-support


## Current State Analysis

The compositor (`crates/compositor/src/`) manages native Wayland windows only: `windows.rs` holds a `WindowManager` keyed on XDG toplevels with a single reconcile tick, `lib.rs` exposes `State::toplevels()` (xdg-only) for frame production, and snapshots flow to the shell through the unchanged `StateModel`. Smithay is pinned to `=0.7.0` (`Cargo.toml:16`), which ships the additive `xwayland` cargo feature — currently unused. No X11 code, spawn logic, or compatibility-server presence exists anywhere in the tree.

## Per-Task Technical Notes

### Task: Additive xwayland flag and spawn supervisor

- `crates/compositor/Cargo.toml` — new additive `xwayland` feature enabling Smithay's `xwayland` support; everything X11 `#[cfg]`-gated.
- `crates/compositor/src/xwayland.rs` (new) — `XWaylandSupervisor` with `Readiness` (`Idle | Pending { attempts } | Ready { display } | Absent { reason }`); `XWayland::spawn` with empty env vec; bounded restarts then settle `Absent` with one non-fatal log line.
- `crates/compositor/src/runtime.rs` — request supervision on first legacy need; poll readiness each tick.
- `crates/compositor/src/main.rs` — `--xwayland` / `ROOST_XWAYLAND=1` opt-in, default off.

**Complexity**: Medium
**Token estimate**: ~25k
**Agent strategy**: single agent, sequential (supervisor + plumbing + absent-path tests)

### Task: Unify manager around Window handle

- `crates/compositor/src/windows.rs` — `ManagedWindow.surface` becomes Smithay `desktop::Window` (unified over `WindowSurface::{Wayland, X11}`); one reconcile tick ingests native toplevels and XWM `X11ManagerEvent::{Mapped, Unmapped,Configured, PropertyChanged, Destroyed}`; configure/close forwarded per kind. Ingest keys on underlying surfaces (`Window::new_*` mints fresh ids).
- `WindowManager::x11_surfaces()` + `Runtime::render` frame loop — X11 surfaces join `send_frames_surface_tree` because `State::toplevels()` is xdg-only and Xwayland paces content commits on frame.
- Keycodes need no X11 arm: winit reports XKB codespace; translate sub-8 + forward add-8 lands correct evdev wire codes (vendored smithay 0.7 source).

**Complexity**: High
**Token estimate**: ~40k
**Agent strategy**: single agent (manager core is tightly coupled; split would thrash)

### Task: X11 identity mapping

- `crates/compositor/src/xwayland.rs` — pure `map_x11_identity`: title from `X11Surface::title()` with stable untitled fallback; app id from WM_CLASS (class preferred, instance tiebreak, else absent). Re-read on title/class property events into `StateModel::insert/update`; snapshot schema unchanged.

**Complexity**: Low
**Token estimate**: ~10k
**Agent strategy**: single agent, sequential

### Task: Privilege gates for X11 entries

- `crates/compositor/src/control.rs` — fail-closed token gating extended to X11 model entries by window kind.
- `crates/compositor/src/layer.rs` — layer refusal path covers X11 entries; no capture/control global advertised.
- `crates/compositor/tests/x11_gates.rs` (new) — identical activation denials for X11 entries, substring tripwire against future privileged globals, windows never listed as layer surfaces.

**Complexity**: Medium
**Token estimate**: ~20k
**Agent strategy**: single agent, sequential

### Task: Headless unit tests for mapping and policy

- Unit tests beside the supervisor (readiness/restart/absent, empty-PATH spawn) and mapper fallbacks; frame-helper no-X11-side tests. `X11Surface` cannot be constructed headlessly — headed commits covered by the manual procedure only.
- Gates: `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` with no X server on PATH, plus `--features xwayland` clippy/test variants.

**Complexity**: Low
**Token estimate**: ~12k
**Agent strategy**: single agent (test-author sub-agent per implement workflow)

### Task: Live legacy-app verification

- Manual procedure (human executor): Xvfb :99 + `--xwayland` session + xterm on :0; Xvfb PointerRoot focus requires explicit `windowfocus` on the winit window before key injection (harness-only). Checks: map log line, `wl_keyboard.enter`, typed text in screenshots, `_NET_WM_STATE_FOCUSED`, exactly one server start (zero native-only), Alt+Tab Step/Commit + switcher popup.
- Switcher slots render without titles by design (all kinds); overview title display belongs to the overview track.

**Complexity**: Low
**Token estimate**: ~8k
**Agent strategy**: human at a live session

## Testing Strategy

Headless unit tests pin supervisor policy, mapper fallbacks, and gate parity; integration tripwires assert X11 entries meet native denials; the frame helper's no-X11 side is unit-tested while headed commits ride the manual live procedure. Per-task detail above names the files; conventions are the existing suite's (plain asserts, hand-maintained values, shared fixtures).

## Project References

- Spec `20260930054036-d6050cbc-xwayland-support` (final, reconciled).
- Repo `rust-wayland-desktop` (`/home/ubuntu/dev/rust-wayland-desktop`).
- Prior plan run artifacts (same feature): plan/context/research/test-plan/changelogs in store.
- `docs/adr/0001-nested-backend-smithay-pin.md` — Smithay pin rationale.

## Token Management Strategy

| Tier | Token Budget | Agent Strategy |
|------|-------------|----------------|
| Low | ~10k | Single agent, sequential |
| Medium | ~25k | 2-3 parallel agents |
| High | ~50k+ | Parallel analysis, sequential integration |

## Migration Notes

N/A — additive feature behind a default-off flag; no migration.

## Performance Considerations

Frame loop stays O(mapped surfaces); one extra surface list per frame, negligible. No new threads; supervisor poll rides the existing tick.
