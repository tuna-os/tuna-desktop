---
created_date: "2026-09-29"
document_status: final
closed_date: "2026-09-29"
---

# Plan: 20260928234216-ff77953a-scrollable-tiling-mode

<!-- Metadata -->
<!-- Created: 2026-09-29T03:42:39Z -->
<!-- Commit: b7b5d04 -->
<!-- Branch: main -->
<!-- Repository: https://github.com/hanthor/rust-wayland-desktop.git -->

## Overview

RWD gains a whole-session scrollable-tiling mode beside its floating desktop, toggled by a single chord. Windows arrange in a keyboard- and wheel-navigable strip with niri-style proportions, giving column-oriented users their workflow without leaving the session. The floating desktop stays the default and behaves exactly as today.

## Conventions

No project conventions apply to this feature — `spektacular knowledge always-applied --tier repo --filter rust-wayland-desktop` returned zero entries during discovery.

## Architecture & Design Decisions

Three options were weighed against the discovery evidence (see `research.md#alternatives-considered-and-rejected`).

**Option A — manager-local mode + strip layout function (chosen, Medium effort).** A session mode flag on `WindowManager` beside the floating arranger (`windows.rs:52-63`); one strip layout function over the same window truth reusing `apply_layout` stash-once, `visible_windows`, and `configure` (`windows.rs:183-199,462-501,755-773`); chords branch inside `on_workspace_key` (`windows.rs:1146-1187`); wheel arrives via a new `ManagerInput::Axis` variant plumbed through `translate_input` (`windows.rs:1230-1255`). Pros: smallest blast radius — render, focus, tokens, and shell sync paths are untouched; toggle re-runs layout + configure in place, fitting the 2-frame metric. Cons: `windows.rs` grows a second layout discipline; mode-branching must be audited against every chord so nothing leaks into gnome mode.

**Option B — shell-driven mode via control command (rejected, Medium effort).** Shell sends a `SetMode` command; compositor obeys and broadcasts. Pros: shell could display mode. Cons: violates compositor-authority + R6 no-shell-round-trip precedent; adds schema version surface; breaks mode toggle when the shell is absent or restarting under supervision.

**Option C — parallel strip manager type (rejected, High effort).** A second `WindowManager`-scale type owning strip truth. Pros: zero branching in hot paths. Cons: duplicates focus/workspace/reconcile/token machinery and doubles every future layout change; the two managers would still need a shared truth layer, recreating Option A underneath.

The chosen shape keeps every requirement inside `rust-wayland-desktop/crates/compositor` (layout in `windows.rs`, input in `windows.rs` + `runtime.rs` tick wiring, tests in `compositor/tests/windows.rs`), with `shell-host` untouched except where the regression gate forces it (nothing planned). Key trade-offs: strip order/offset/widths stay manager-local and unrevisioned (shell never learns the mode, so no protocol change); Super+Left/Right and Super+Shift+Left/Right branch on mode inside the existing chord chain, keeping gnome behavior bit-identical; preset cycling reuses the stash-once pattern so toggling widths never loses floating geometry; forced membership intercepts the `map`/maximize/fullscreen/tile entry points while still advertising xdg state through the single `configure` path. Niri's `scrolling.rs` is the code model for columns, the gaps-twice proportion formula, preset stepping, and the axis accumulator — copied as shape, not vendored.

## Component Breakdown

- **Session mode flag** (changed, on the window manager): owns which layout discipline is active (`gnome`/`scroll`); every chord branch and layout entry point reads it. No other component stores mode.
- **Strip layout function** (new, beside the floating arranger): owns column geometry — order, per-column widths, 16px gaps, scroll offset — computed from the work area; writes manager geometry like the floating path so rendering, hit-testing, and `configure` need no new sinks.
- **Column width presets** (new, owned by the strip function): owns the 1/3–1/2–2/3 stepping with ±1px tolerance and the 1/2 default; reuses the stash-once pattern so width cycling never loses floating geometry.
- **Chord dispatcher extension** (changed, input chain): owns Super+Shift+T, mode-branched Super+arrows / Super+Shift+arrows, and Super+R cycling; consumes presses and swallows releases per existing precedent, and never arms Super-tap.
- **Wheel axis plumbing** (new, input path): owns pointer-axis events from backend translate through dispatch into a scroll-offset accumulator; degrades to keys-only where the backend yields no axis events.
- **Forced-membership intercepts** (changed, window lifecycle points): own the rule that map/maximize/fullscreen/tile all resolve to column assignments in scroll mode while protocol state still advertises through the single configure path.
- **Scroll proof harness** (new, scripts + CI): owns the scripted nested session asserting strip geometry in screenshots, extending the journey pattern.

## Data Structures & Interfaces

Contracts between the new strip machinery and the existing manager (pseudocode shapes; field detail lives in `context.md`):

```
enum SessionMode { Gnome, Scroll }            // the mode flag; default Gnome

struct StripColumn {                           // one tiling column
    windows: Vec<WindowId>,                    // stacked windows, first = visible
    width: ColumnWidth,                        // Proportion(f64) | Fixed(px)
    preset_idx: usize,                         // position in [1/3, 1/2, 2/3]
}

struct StripState {                            // whole-session strip, manager-local
    columns: Vec<StripColumn>,                 // left-to-right order
    active: usize,                             // focused column index
    offset: f64,                               // horizontal view offset, px
}
```

- `SessionMode` is read by every chord branch and layout entry point; only the toggle flips it, followed by a full re-layout.
- `StripState` is rebuilt from window truth on toggle and incrementally maintained on map/unmap/move; nothing about it crosses the control protocol (no schema change).
- `ManagerInput::Axis { orientation, amount }` extends the input enum so wheel events travel the same translate → dispatch path as keys; the accumulator folds amounts into `StripState.offset`.
- Column geometry resolves as `(work_width − gaps) × proportion − gaps` per the niri formula; `configure` advertises the resolved size with no managed xdg state (same as floating).

## Implementation Detail

The work introduces one new pattern — a second layout discipline behind a session mode flag — and otherwise follows existing codebase patterns throughout.

A developer reading the changed manager sees two layout functions over one window truth: the floating arranger as today, plus a strip function that assigns column rectangles instead of free geometry. The mode flag reads like the overview-intent flag (session UI state, flipped in one place, observed everywhere), and the toggle path mirrors the existing toggle flows: flip, re-run layout over all windows, reconfigure each. Chord handling extends the established consume-press/swallow-release table rather than inventing dispatch; the new chords are indistinguishable in shape from the Alt-Tab and workspace-switch entries. Preset cycling follows the stash-once/restore discipline already used for maximize and tile, so a reader familiar with those paths will recognize the width-cycling code immediately.

New ground is confined to two places: the axis input variant (the first pointer-axis plumbing in the tree, shaped as a small accumulator feeding the strip offset) and the strip state itself (columns + active index + float offset, modeled on niri's `ScrollingSpace` but sized to this manager). Both are manager-local; no new module boundary is introduced, and the shell side of the tree is deliberately untouched — a reader diffing shell code will find nothing.

## Dependencies

- Smithay 0.7.0 (pinned) + GLES/Mesa swrast — the compositor stack the strip renders through; no changes, no version movement per spec constraint.
- `rwd-shell-control` schema v0.4 — unchanged; the plan adds no message or command (mode stays compositor-local).
- Final spec `specs/20260928234216-ff77953a-scrollable-tiling-mode.md` — the binding contract; must already be final (it is).
- 002 screenshot journeys (`rwd-journey`, `rwd-app-content`) green on main — the regression floor; must stay green throughout.
- Niri reference checkout (`references/niri`, behavior model only) — no code vendored, no dependency added.
- Design documents this plan was built on: none (the spec carries no design references).

## Testing Approach

Unit tests cover the strip math (proportion formula, preset stepping with tolerance, offset accumulation) and the chord table (each new chord consumed, releases swallowed, Super-tap unaffected). Integration tests over real protocol clients — mirroring the existing `windows.rs` harness (fixture + bounded pump, no sleeps) — cover toggle re-layout, focus/slot motion, preset cycling, and forced membership for maximized/fullscreen maps. A scripted nested-session proof extends the journey pattern with screenshot geometry asserts (gaps, widths, overflow) for scroll mode. Heaviest coverage lands on the strip function and chord branching, since a leak there regresses gnome mode.

Spec success metrics map as follows: full re-layout within 2 frames with no window loss → behavioural test asserting the pre/post window sets are identical and every geometry resolves to a column (frame bound measured in the proof harness log); scroll-mode screenshot proof green in CI → behavioural test in the new proof job; zero regressions across gnome journeys, focus/clipboard/switcher, and workspace tests → existing suites kept green as the merge gate, no new tests needed for what already passes.

## Milestones & Tasks

### Milestone 1: Scroll mode toggles and lays out the strip

**What changes:** Pressing Super+Shift+T turns the whole session into a horizontal strip of windows with 16px gaps and half-width columns, and pressing it again restores the floating desktop exactly. Every open window survives the round trip. Validation: integration tests assert identical window sets and column geometry across toggles; gnome-mode suites stay green.

### Milestone 2: Strip navigation, presets, and wheel

**What changes:** In scroll mode the arrow keys move focus along the strip, Shift+arrows move windows between slots, Super+R cycles column widths through 1/3–1/2–2/3, and the pointer wheel scrolls the strip — maximized and fullscreen windows join as columns instead of covering the screen. Validation: chord/preset/membership integration tests plus screenshot geometry asserts in the scripted proof.

### Milestone 3: Scroll proof green in CI

**What changes:** Nothing visible changes; the scripted scroll proof runs in CI alongside the existing journeys and passes, locking the behavior in. Validation: the new proof job is green on main with gnome-mode journeys still green.

#### - [x] Task: Strip layout function with session mode flag
**Id:** df74f233-7b2c-434b-ba87-259b601c5d8e
**Repo:** rust-wayland-desktop
**Depends on:** none
**Execution:** agent

Add the scroll-mode layout discipline beside the floating arranger: a session mode flag defaulting to gnome, a strip function computing column geometry from the work area, and a Super+Shift+T toggle that re-layouts every window in place. No shell or protocol changes.

*Technical detail:* [context.md#task-strip-layout-function-with-session-mode-flag](./context.md#task-strip-layout-function-with-session-mode-flag)

**Acceptance criteria**:
- [x] Toggling twice returns every window to its exact floating geometry
- [x] Fresh sessions behave exactly as before with zero code-path changes in gnome mode

#### - [x] Task: Toggle and geometry integration tests
**Id:** 85bfbb29-701e-4aea-90b6-f51285611aa4
**Repo:** rust-wayland-desktop
**Depends on:**
- df74f233-7b2c-434b-ba87-259b601c5d8e — Strip layout function with session mode flag
**Execution:** agent

Cover the toggle round trip and column geometry over real protocol clients: identical window sets across toggles, 16px gaps, half-width default columns, overflow instead of squeezing. Follows the existing fixture+pump harness with no sleeps.

*Technical detail:* [context.md#task-toggle-and-geometry-integration-tests](./context.md#task-toggle-and-geometry-integration-tests)

**Acceptance criteria**:
- [x] Toggle round trip loses no window and resolves every geometry to a column
- [x] Geometry asserts match the spec values (gaps, default width, overflow)

#### - [x] Task: Mode-branched chord handling
**Id:** d5793155-e6d5-4793-b5cb-ce92bb77b8e4
**Repo:** rust-wayland-desktop
**Depends on:**
- df74f233-7b2c-434b-ba87-259b601c5d8e — Strip layout function with session mode flag
**Execution:** agent

Branch the chord table on session mode: Super+arrows move strip focus and Super+Shift+arrows move windows between slots in scroll mode, while gnome bindings stay bit-identical. Consume presses and swallow releases per existing precedent without arming Super-tap.

*Technical detail:* [context.md#task-mode-branched-chord-handling](./context.md#task-mode-branched-chord-handling)

**Acceptance criteria**:
- [x] Each new chord is consumed and never reaches clients half-formed
- [x] Gnome-mode chords behave exactly as before the change

#### - [x] Task: Wheel axis plumbing with keys fallback
**Id:** 64db4aba-f2cb-46f9-b603-8caebf0ba916
**Repo:** rust-wayland-desktop
**Depends on:**
- df74f233-7b2c-434b-ba87-259b601c5d8e — Strip layout function with session mode flag
**Execution:** agent

Plumb pointer-axis events from backend translate through dispatch into a scroll-offset accumulator, and verify whether the nested input path actually delivers axis events. If it does not, record keys-only fallback per the spec constraint instead of forcing the plumbing.

*Technical detail:* [context.md#task-wheel-axis-plumbing-with-keys-fallback](./context.md#task-wheel-axis-plumbing-with-keys-fallback)

**Acceptance criteria**:
- [x] Wheel ticks visibly scroll the strip where axis events arrive, or the keys-only fallback is recorded
- [x] Focus stays on the same window across scrolls

#### - [x] Task: Forced strip membership intercepts
**Id:** 29f080da-f83a-4109-9265-95fe24f021af
**Repo:** rust-wayland-desktop
**Depends on:**
- df74f233-7b2c-434b-ba87-259b601c5d8e — Strip layout function with session mode flag
**Execution:** agent

Route map, maximize, fullscreen, and tile entry points to column assignments in scroll mode, keeping protocol state advertised through the single configure path. Dialogs get a defined strip rule instead of floating centering.

*Technical detail:* [context.md#task-forced-strip-membership-intercepts](./context.md#task-forced-strip-membership-intercepts)

**Acceptance criteria**:
- [x] No window ever renders above the strip in scroll mode
- [x] Maximized and fullscreen windows join as same-height columns

#### - [x] Task: Preset width cycling
**Id:** a5f43cd4-7f8a-4a4c-9268-2d61668d7ae4
**Repo:** rust-wayland-desktop
**Depends on:**
- df74f233-7b2c-434b-ba87-259b601c5d8e — Strip layout function with session mode flag
**Execution:** agent

Cycle the focused column through the 1/3–1/2–2/3 presets with Super+R forward and Super+Shift+R backward, reusing the stash-once discipline so cycling never loses floating geometry.

*Technical detail:* [context.md#task-preset-width-cycling](./context.md#task-preset-width-cycling)

**Acceptance criteria**:
- [x] Each cycle step visibly changes the focused column width in preset order
- [x] Toggling back to gnome mode restores exact floating geometry

#### - [x] Task: Scroll proof harness and CI job
**Id:** 89eba268-7086-438f-8a2e-06c3ce7ef4fa
**Repo:** rust-wayland-desktop
**Depends on:**
- 85bfbb29-701e-4aea-90b6-f51285611aa4 — Toggle and geometry integration tests
- d5793155-e6d5-4793-b5cb-ce92bb77b8e4 — Mode-branched chord handling
- 64db4aba-f2cb-46f9-b603-8caebf0ba916 — Wheel axis plumbing with keys fallback
- 29f080da-f83a-4109-9265-95fe24f021af — Forced strip membership intercepts
- a5f43cd4-7f8a-4a4c-9268-2d61668d7ae4 — Preset width cycling
**Execution:** agent

Extend the journey pattern with a scripted scroll proof asserting strip geometry in screenshots, wired as a CI job beside the existing journeys.

*Technical detail:* [context.md#task-scroll-proof-harness-and-ci-job](./context.md#task-scroll-proof-harness-and-ci-job)

**Acceptance criteria**:
- [x] The proof job passes on main with all existing journeys still green
- [x] Screenshots prove gaps, widths, and overflow per the spec

#### - [x] Task: Human visual review of scroll frames
**Id:** bf4e0dfe-3e92-4819-91ec-41f3772621e4
**Repo:** rust-wayland-desktop
**Depends on:**
- 89eba268-7086-438f-8a2e-06c3ce7ef4fa — Scroll proof harness and CI job
**Execution:** human — judging animation smoothness, gap rhythm, and focus readability needs human eyes; pixel asserts cannot score visual quality

Look at the proof screenshots and confirm the strip reads clearly: even gaps, legible focus highlight, sane overflow behavior at the screen edges.

*Technical detail:* [context.md#task-human-visual-review-of-scroll-frames](./context.md#task-human-visual-review-of-scroll-frames)

**Acceptance criteria**:
- [x] Reviewer confirms the strip looks correct in all proof frames

**Review record (2026-09-29):** Agent visual sign-off performed at the user's explicit direction, substituting for human eyes: all five CI-built frames from run 36525694070 (scroll-proof-shots/) inspected as images. 01-float: floating cascade baseline, panel clock left + indicators right intact. 02-strip: two-column strip, even 16px gaps, dark gap background consistent with the shell theme. 03-preset: narrower preset columns, steady gap rhythm. 04-wheel: strip offset by exactly one column (first window a sliver at the left edge), focus steady. 05-back: cascade restored, matching 01-float. No new shell chrome introduced by scroll mode, so no new HIG surface. One pre-existing nit (topmost floating window can clip the panel clock in 01-float) is unchanged shared-surface behavior, out of scope for this spec.

## Open Questions

- Whether axis events arrive on the nested input path — depends on exercising the winit/X backend during the wheel task; if absent, the implementer records keys-only fallback per the spec constraint and keeps the stub.
- Whether advertising fullscreen xdg-state while rendering as a column confuses real clients — depends on exercising clients beyond the test harness during forced-membership work; if confused, the implementer isolates like niri consume-or-expel and notes the deviation.
- Whether per-window `configure` on toggle fits the 2-frame metric under load — depends on measuring in the proof harness; if over, the implementer STOPs and asks rather than batching silently.

## Out of Scope

- Per-workspace modes (spec Non-Goals; explicitly whole-session) — no follow-up tracked.
- Touchpad-gesture parity beyond wheel/keys (spec Non-Goals) — future spec if demanded.
- Remembering per-window float state across toggles (spec Non-Goals; forced strip chosen) — no follow-up tracked.
- Shell mode display or settings UI for the toggle (chosen design leaves shell untouched) — future spec if demanded.
- wgpu rendering migration (spec constraint pins GLES/swrast) — out of scope for this plan entirely.


## Changelog

### 2026-09-29 — Task: Strip layout function with session mode flag

**What was done**: Added the session mode flag (Gnome default/Scroll), strip column geometry across the work area, and a Super+Shift+T toggle that re-lays out every window in place.

**Deviations**: Added a `WindowLayout::Strip` per-window variant as the stash vehicle through the existing stash-once/restore path; the session mode flag itself stays manager-level as planned.

**Files changed**:
- `crates/compositor/src/windows.rs`
- `crates/compositor/tests/windows.rs`

**Discoveries**: Super+T without Shift only disarms Super-tap (existing test unaffected); consumed-press/release-forward rule from Tab/F4 applies cleanly to the toggle chord.

### 2026-09-29 — Task: Toggle and geometry integration tests

**What was done**: Four integration tests over real protocol clients: gnome default, chord-driven toggle round trip with exact floating restore, strip geometry (616x768 columns, 16px gaps, overflow) with configure advertise, and Super+T-without-Shift no-toggle.

**Deviations**: None.

**Files changed**:
- `crates/compositor/tests/windows.rs`

**Discoveries**: None.

### 2026-09-29 — Task: Mode-branched chord handling

**What was done**: Super+Left/Right move strip focus without reordering columns and Super+Shift+Left/Right move the focused window one slot (with follow-focus) in scroll mode; Up/Down keep gnome meaning; focus never raises stacking in scroll mode.

**Deviations**: None.

**Files changed**:
- `crates/compositor/src/windows.rs`
- `crates/compositor/tests/windows.rs`

**Discoveries**: Pointer click/pointer-motion focus still raises stacking in scroll mode (apply_focus raise is only gated for the focus call path used by arrows) — pointer-path parity falls to a later task if needed.

### 2026-09-29 — Task: Wheel axis plumbing with keys fallback

**What was done**: Backend PointerAxis events translate to a new ManagerInput::Axis variant driving a clamped strip-offset accumulator with re-layout; focus stays put; gnome mode ignores axis; offset resets on entering scroll.

**Deviations**: None. Backend delivery through the nested path is still unverified (unit tests drive ManagerInput directly) — the M3 scroll proof exercises it via xdotool wheel clicks.

**Files changed**:
- `crates/compositor/src/windows.rs`
- `crates/compositor/tests/windows.rs`

**Discoveries**: winit backend yields PointerAxis (LineDelta→v120, PixelDelta→continuous); axis-amount helper prefers v120, falls back to pixels.

### 2026-09-29 — Task: Forced strip membership intercepts

**What was done**: map(), set_maximized/set_fullscreen/set_tiled (both directions), and client WindowRequest drains resolve to strip columns in scroll mode; dialogs join as adjacent columns; Super+Down re-resolves instead of restoring floating mid-scroll.

**Deviations**: Super+Down needed its own membership branch (test agent caught restore_window bypass).

**Files changed**:
- `crates/compositor/src/windows.rs`
- `crates/compositor/tests/windows.rs`

**Discoveries**: Strip configure advertises size with no managed xdg state; clients tolerate the mismatch in tests (real-client verification falls to the proof task).

### 2026-09-29 — Task: Preset width cycling

**What was done**: Per-window preset slots cycled with Super+R forward / Super+Shift+R backward (wrapping, 1/3–1/2–2/3); geometry moved to prefix sums so mixed widths lay out correctly; relayout clamps the offset; unmap closes ranks; gnome mode forwards R untouched.

**Deviations**: Geometry refactor from uniform-width indexing to prefix sums (required by mixed presets); offset clamp moved into relayout_strip as the single choke point.

**Files changed**:
- `crates/compositor/src/windows.rs`
- `crates/compositor/tests/windows.rs`

**Discoveries**: None.

### 2026-09-29 — Task: Scroll proof harness and CI job

**What was done**: New scripts/rwd-scroll-proof (three Chromium windows, toggle/preset/wheel/back with gap-geometry pixel asserts) plus a scroll-proof CI job with frame upload; passes locally end to end.

**Deviations**: Pixel asserts locate columns relative to the first green run (Chromium paints a small chrome margin, so absolute screen coordinates are unreliable); wheel falls back to keys-only note when the nested backend yields no axis (delivered locally: shift 810).

**Files changed**:
- `scripts/rwd-scroll-proof`
- `.github/workflows/ci.yml`

**Discoveries**: Nested X delivers wheel button clicks as axis events through winit (xdotool click 4/5 works); columns render at spec geometry on real clients.
