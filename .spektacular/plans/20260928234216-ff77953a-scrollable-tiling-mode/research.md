---
created_date: "2026-09-29"
document_status: final
closed_date: "2026-09-29"
---

# Research: 20260928234216-ff77953a-scrollable-tiling-mode

## Alternatives considered and rejected



- **Per-window layout variant** (extend `WindowLayout` with `Strip`): rejected — strip order/offset/widths are session-level state shared across windows (niri `ScrollingSpace` shape); per-window enum cannot hold the strip. Session-level mode flag beside the manager instead (`windows.rs:52-77`).
- **Shell-owned mode flag**: rejected — shell is a separate supervised client and never flips state locally (`shell-host/src/control.rs:325-332` toggle-overview precedent); compositor owns truth, shell mirrors. Mode lives compositor-side.
- **Revisioned mode in `StateModel`**: rejected — model mirrors to shell via snapshots; mode is session UI state like overview intent (`compositor/src/state.rs:1-6` avoids wire deps). Prefer manager-local flag, broadcast only if shell needs display.
- **New activation channel for scroll Enter**: rejected — reuse `activate_selected` token path (`shell-host/src/control.rs:306-317`).
- **In-compositor overview / per-motion IPC / D-Bus daemon**: rejected already by 002 research; same reasoning holds.
- **Mango adoption or dependency**: rejected per spec constraint (reference only); niri `scrolling.rs` alone covers strip/gaps/presets/focus/axis/fullscreen.


## Chosen approach — evidence



- Strip layout function beside the floating arranger in `compositor/src/windows.rs`, same window truth: `ManagedWindow` geometry/layout split (`windows.rs:52-63`), `apply_layout` stash-once (`windows.rs:755-773`), `visible_windows` as the only render feed (`windows.rs:183-199`), `geometry()` as test hook (`windows.rs:893-895`).
- Session mode flag on the manager (smallest blast radius; `set_mode` re-runs layout + `configure` each window). Observers: `on_input`/`on_workspace_key` chord branching, `map`/`apply_layout`/max/fullscreen/tile entry points, `visible_windows`+render offset, `reconcile` focus/transients.
- Chords in compositor `on_workspace_key` chain (`windows.rs:1146-1187`): Super+Shift+T (T=20 free), Super+arrows branch on mode (today=tile halves), Super+Shift+arrows branch on mode (today=move-across-workspaces), Super+R/Super+Shift+R (R=19 free in manager scope; `overlay.rs:88` uses R only while recovery overlay visible). Consume-press/swallow-release per Tab/F4 precedent (`windows.rs:1091-1113`); must not arm/cancel Super-tap (`windows.rs:1063-1066`).
- Niri model (`references/niri/src/layout/scrolling.rs`): `ScrollingSpace` columns+active-index+view-offset (`:36`), `column_xs` accumulator (`:2333`), gaps-twice proportion formula (`resolve_column_width :4536`, `resolve_preset_size :5685`), `toggle_width` preset stepping (`:4927`), `focus_left/right` + `move_left/right` (`:1581-1756`), axis via `on_pointer_axis` + 40-line scroll tracker (`src/input/mod.rs:3109`, `scroll_tracker.rs`), fullscreen isolation via consume-or-expel (`:2869-2900`).
- Wheel is greenfield: no axis handling in `crates/` (only a control.rs doc mention); needs `ManagerInput::Axis` + `translate_input` + dispatch + offset accumulator. Nested winit/X path may lack axis events → spec constraint records keys-only fallback.
- Proof: extend journey-harness pattern (scripted nested session, screenshot geometry asserts, marker-style proof); EGL-only nested backend (`gotchas/winit-backend-requires-egl`).


## Files examined



- `compositor/src/windows.rs:52-86` — ManagedWindow + WindowLayout enum; strip state sits beside, not inside.
- `compositor/src/windows.rs:183-199,893-895` — visible_windows/geometry: render + test hooks.
- `compositor/src/windows.rs:302-361` — map/cascade/place_transient: forced strip replaces cascade; dialogs need a strip rule.
- `compositor/src/windows.rs:462-501,642-773` — configure/apply_layout/restore: stash-once + single-state advertise; preset cycling mirrors this.
- `compositor/src/windows.rs:777-870,928-954` — workspace move/focused-relative, hit-test, ManagerInput (no Axis variant).
- `compositor/src/windows.rs:961-998,1022-1132,1146-1255` — evdev tables (T=20/R=19 free), TriggerState tap machine, on_input chain, on_workspace_key, translate_input.
- `compositor/src/state.rs:26-48,106-184,270-285,401-565` — StateModel/changes/focus/TokenStore; mode must not break per-snapshot token minting.
- `compositor/src/control.rs:621-660,676-824` + `shell-control-schema/src/lib.rs:53-58,113-158,192-261` — apply_command, hub broadcast, schema v0.4 append-last; mode broadcast is a protocol decision.
- `compositor/src/runtime.rs:290-367,403-480` — on_manager_input, tick order, render stacking.
- `compositor/src/lib.rs:55-105,169-172,283-356` — State, WindowRequest drain, TestCompositor helper.
- `shell-host/src/keyboard.rs:16-40,124-218` — XkbFeed; overview-gated via `panel.rs:840-863` (global chords CANNOT ride this path).
- `shell-host/src/panel.rs:961-974,1032-1055,1281-1341` — sync_overview/drive loop: unchanged (regression gate).
- `shell-host/src/model.rs:56-73,184-271` — MRU + switcher: unchanged; strip order vs MRU are separate orderings.
- `compositor/tests/windows.rs:85-90,309-404,644-667,687-990` — fixture/pump/key-sim/chord/layout test patterns to mirror.
- `references/niri/src/layout/scrolling.rs:36,117-187,1581-1756,2333-2417,2630-2652,2869-2900,4522-4990,5685` — strip model to copy.
- `references/niri/src/input/mod.rs:3109`, `scroll_tracker.rs` — axis handling shape.


## External references



- niri upstream `default-config.kdl` (fetched 2026-09-29): gaps 16, `center-focused-column "never"`, presets 1/3–1/2–2/3, default 1/2 — grounds the spec's geometry values.
- niri `scrolling.rs` (5734 lines, single-file strip implementation with stable pub fns) — primary code model.
- knowledge `decisions/smithay-070-for-nested-slice.md`, `learnings/smithay-070-pin-set.md` (pins: smithay 0.7.0, calloop 0.14.4, protocols 0.32.13, MSRV 1.80.1) — plan to the pins.
- knowledge `gotchas/winit-backend-requires-egl.md` — EGL-only nested backend constrains the proof harness.


## Prior plans / specs consulted



- `specs/20260928234216-ff77953a-scrollable-tiling-mode.md` (final) — binding contract for this plan.
- `specs/20260927170318-a01f0012-shell-workflow-parity.md` + final plan/research — tiling previously excluded until journeys pass (now passed); compositor-authority + token intents + evdev dispatch precedent; in-compositor overview / per-motion IPC / D-Bus rejected.
- `specs/20260927170317-a01f0011-001-nested-compositor-shell-recovery.md` + research — R6 no-shell-round-trip, token policy, same-UID dev assumption.
- `specs/20260927170316-a01f0010-program-baseline-and-spikes.md` — provisional assumptions for unselected baseline.
- No scroll plan exists (8 features listed, none scroll) — greenfield under a final spec.


## Open assumptions



- Scroll offset + strip order/widths live manager-local (not revisioned); shell needs NO mode display. If implement finds shell must show mode, STOP and ask (protocol version bump).
- Wheel axis events arrive through the nested winit/X path. If absent, keys-only fallback per spec constraint (implement verifies first, records which).
- Toggle re-layout (`configure` per window) fits the 2-frame metric; measure in proof, not assumed.
- Fullscreen xdg-state stays advertised while rendering as column; clients tolerate it (verify in proof; isolate like niri consume-or-expel if not).
- Focus motion does not raise/reorder (strip order separate from floating stacking).


## Drafting assumptions

### Global chords live in the compositor, not shell-host (discovery)
- **Decision**: Super+Shift+T, Super+arrows, Super+R dispatch in `compositor/src/windows.rs` input chain.
- **Rationale**: shell `on_key` is gated on overview-open (`panel.rs:840-863`); closed-overview chords cannot ride it.
- **Rejected**: spec tech-steer line "extends the shell-host keyboard feed" — contradicts the gate; architecture step resolves.

### Mode flag is manager-local, unrevisioned (discovery)
- **Decision**: session mode on `WindowManager`, not `StateModel`, no wire broadcast unless shell display needed.
- **Rationale**: mirrors overview-intent pattern (UI state outside revisioned truth); avoids schema version bump.
- **Rejected**: revisioned model field (would mirror to shell and mint deltas for UI state).

### Manager-local strip beats shell-driven and parallel-manager (architecture)
- **Decision**: Option A — mode flag + strip fn on WindowManager; chords in on_workspace_key; Axis variant for wheel.
- **Rationale**: smallest blast radius, no protocol change, toggle fits 2-frame metric; B breaks authority/R6, C duplicates truth machinery.
- **Rejected**: shell-driven mode command; parallel strip manager type (see research.md alternatives).

### Niri single-file model over Mango/cosmic (discovery)
- **Decision**: model strip on niri `scrolling.rs` only.
- **Rationale**: one file covers columns/gaps/presets/focus/axis/fullscreen with stable pub fns; Mango adds nothing.
- **Rejected**: spreading modeling across Mango/cosmic-comp/mutter checkouts.

## Rehydration cues



- Re-read this file + `spektacular spec file read 20260928234216-ff77953a-scrollable-tiling-mode`.
- Re-open `compositor/src/windows.rs` (layout/input) and `references/niri/src/layout/scrolling.rs` (model).
- `spektacular plan goto --data '{"step":"architecture"}'` to continue the workflow.

