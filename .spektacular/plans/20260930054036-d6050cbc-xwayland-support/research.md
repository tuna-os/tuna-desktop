---
created_date: "2026-09-30"
document_status: final
closed_date: "2026-09-30"
---

# Research: 20260930054036-d6050cbc-xwayland-support


Carried forward from the first plan run (verified by implementation
since): unified Smithay 0.7 `Window` handle, `XWayland::spawn`
readiness mapping, `X11Surface` identity source, no shell/schema
change for snapshots. The reconciled spec changes no scope — 12/14
boxes ticked, 2 identity-display boxes open for the overview track.

## Alternatives considered and rejected

### Option A: Parallel X11 branch in reconcile

Keep native paths untouched; add a parallel X11 ingest branch keyed
by X11 window id. **Rejected**: forks stacking/focus into two paths
every future manager change must update in lockstep; violates the
"one window model" constraint in spirit
(`crates/compositor/src/windows.rs` single reconcile tick).

### Option B: SpaceElement-level boxing

Manage `Box<dyn SpaceElement>`. **Rejected**: loses typed
configure/close/geometry ops per kind; more complexity, less toolkit
support than the unified handle.

### Option C: No XWayland integration (native-only)

**Rejected**: spec's core problem (users blocked on X11-only
toolkits) stands; signed-off spec requires v1 parity.

## Chosen approach — evidence

Smithay =0.7.0 (pinned, ADR 0001) `xwayland` cargo feature
(additive) + unified `desktop::wayland::Window` over
`WindowSurface::{Wayland, X11}` — verified against vendored 0.7
registry source AND by the shipped implementation. `XWayland::spawn`
reports Ready/Error for on-demand/absent mapping. `X11Surface`
title/class/instance/pid + `wl_surface()` feed the identity mapper;
`StateModel::insert/update` take X11 entries with no shell change.
Frame production: `WindowManager::x11_surfaces()` feeds X11 surfaces
into the `Runtime::render` frame loop — REQUIRED: Xwayland paces
content commits on `wl_surface.frame`, and `State::toplevels()` is
xdg-only, so without this X11 windows present nothing (proven live:
1x1 placeholder only until the fix, full-size commits after).

## Files examined

- `Cargo.toml:16` — smithay `=0.7.0` pin; additive flag extension point.
- `crates/compositor/Cargo.toml:8` — single workspace pin consumed.
- `crates/compositor/src/windows.rs` — manager ingest, reconcile,
  identity reads, `x11_surfaces()` frame-production helper, keycode
  round-trip (winit XKB codespace → evdev tables → evdev on wire).
- `crates/compositor/src/xwayland.rs` — supervisor
  (Idle/Pending/Ready/Absent, bounded restarts), XWM handlers.
- `crates/compositor/src/runtime.rs` — spawn plumbing, session
  opt-in, render frame loop incl. X11 surfaces.
- `crates/compositor/src/control.rs` — fail-closed token gating.
- `crates/compositor/src/layer.rs` — layer refusal path.
- `crates/compositor/src/lib.rs` — `State::toplevels()` (xdg-only;
  the reason X11 needs its own frame source).
- `docs/adr/0001-nested-backend-smithay-pin.md` — pin rationale.
- `crates/compositor/tests/x11_gates.rs`, `tests/windows.rs` —
  gate/unit conventions (plain asserts, hand-maintained values,
  TestCompositor fixture).

## External references

- Smithay 0.7 vendored registry source (`xwayland` module,
  `desktop::wayland::Window`, winit `key_code() = key + 8`) — API
  truth for 0.7; upstream docs differ across versions.
- Live protocol traces (nested Xvfb + Xwayland + xterm, xdotool
  drive): enter/key/modifier flow, full-size damage commits after
  the frame fix, `roost-shell-overview` layer lifecycle.

## Prior plans / specs consulted

- Spec `20260930054036-d6050cbc-xwayland-support` (reconciled,
  final): 6 requirements, 2 constraints, 6 metrics; identity-display
  boxes open for the overview track.
- First-run plan/context/research/test-plan/changelogs (same
  feature): substance reproduced here; deviations recorded there
  (frame fix, keycode exoneration, invisible-overview shell gap,
  switcher titleless-by-design).
- Plan `keyboard-access` — reconcile/focus patterns; shell-host
  tests must never fire Gio-backed production paths.

## Open assumptions

- `X11Surface` cannot be constructed headlessly (needs live X);
  frame-helper tests cover the no-X11 side only; content commits
  verified by the manual live procedure (test-plan.md).
- WM_CLASS edge cases: class preferred, instance tiebreak, else
  absent app id rather than guessing.
- Headless harness X focus: Xvfb uses PointerRoot focus, so live
  key injection needs explicit `windowfocus` on the winit window
  (harness-only concern, not compositor behavior).

## Rehydration cues

- Spec: `spektacular spec file read 20260930054036-d6050cbc-xwayland-support`.
- Smithay 0.7 API truth: vendored registry source, not upstream docs.
- Gates: `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` (no X server on PATH); plus `--features xwayland` clippy/test variants.

## Drafting assumptions

### Unified Window over parallel X11 branch (architecture)

- **Decision**: adopt Smithay's unified `Window` type as the manager's single managed-surface type.
- **Rationale**: the "one window model" constraint becomes satisfiable by construction; toolkit-owned focus/render helpers then cover both kinds; matches the shipped implementation.
- **Rejected**: parallel X11 reconcile branch (forks stacking/focus, rots on future changes); SpaceElement-level boxing (loses typed configure/close ops).

### No conventions selected (architecture)

- **Decision**: conventions.md records "none apply".
- **Rationale**: repo tier's always-applied knowledge list is empty; spec constraints already capture the applicable rules.
- **Rejected**: inventing conventions from spec constraints.
