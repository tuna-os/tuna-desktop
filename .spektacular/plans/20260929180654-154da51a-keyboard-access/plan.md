---
created_date: "2026-09-30"
document_status: final
closed_date: "2026-09-30"
---

# Plan: 20260929180654-154da51a-keyboard-access

<!-- Metadata -->
<!-- Created: 2026-09-30T04:05:00Z -->
<!-- Commit: f5faee9 -->
<!-- Branch: main -->
<!-- Repository: rust-wayland-desktop (/home/ubuntu/dev/rust-wayland-desktop) -->

<!--
  OVERVIEW
  A concise 2-3 sentence summary of the plan.
-->
## Overview

Every Roost shell action gains a keyboard equivalent with visible focus: bar tiles, dock icons, popups, and menus all operate without a pointer, and the keymap is documented in one place. Anyone who drives their desktop from the keyboard gets a shell that never traps focus and always shows where it is.

<!--
  CONVENTIONS
-->
## Conventions

No project conventions apply to this feature — the always-applied load returned no entries and the spec carries no design references.

<!--
  ARCHITECTURE & DESIGN DECISIONS
-->
## Architecture & Design Decisions

Shell-owned focus, compositor-owned windows. The shell gains a focus model — one cursor per surface (panel strip, dock, popup rows, overview results) plus an Escape-return stack — fed by an extended key vocabulary in its existing `on_key` path, so arrow and Tab presses navigate locally with no round-trip to the compositor and no wire change. Focus rings paint with the existing accent selection pattern, and the chosen chords live only on the free surface the research mapped (F6 region cycle, Super+D dock, Alt+F1 panel, arrows/Tab/Enter/Escape within a focused surface), leaving every compositor window chord untouched. Escape splits in two: close (what exists) unwinds the return stack first, so a popup Escape returns to the previous shell focus and a final Escape returns to the previously focused window instead of closing everything. The keymap gets its own document as the single reference.

This beats compositor-routed shell keys (a wire protocol for local UI navigation, with per-press latency) and the minimal status-quo-plus-doc (fails the requirements outright). The collision rule is structural, not advisory: shell chords never use Super+{tap,PgUp,PgDn,arrows,R,T} or Alt+{Tab,F4}, pinned by tests on both sides. See `research.md#alternatives-considered-and-rejected`.

<!--
  COMPONENT BREAKDOWN
-->
## Component Breakdown

- **Key vocabulary (changed, shell)** — `KeyAction` and `on_key` learn navigation: arrows, Tab/Shift+Tab, F6, Enter, and the new chords; existing Text/Erase/Submit/Dismiss paths keep their behavior.
- **Focus model (new, shell)** — cursors for panel strip, dock slots, popup rows, and overview results, plus the Escape-return stack; Tab cycles within a surface, F6 cycles surfaces, Enter activates, Escape unwinds.
- **Focus rings (changed, shell)** — per-surface ring paint in the existing accent style; paint keys gain the focus fields so stale highlights are impossible.
- **Keymap document (new, docs)** — the single discoverable reference for every shell and compositor chord.

<!--
  DATA STRUCTURES & INTERFACES
-->
## Data Structures & Interfaces

The focus model is shell-local UI state, not a wire type — nothing crosses the control socket:

```
ShellFocus {
  region: Panel | Dock | Popup | Overview,  // F6 cycles
  cursor: usize,                            // arrows/Tab move
  return_stack: Vec<Region>,                // Escape unwinds
}
```

Paint keys (`PanelPaintKey`, `DockPaintKey`, popup keys) gain the focused cursor, so any focus change repaints by construction. No revision, no snapshot, no new messages.

<!--
  IMPLEMENTATION DETAIL
-->
## Implementation Detail

The work extends the existing `on_key` funnel rather than adding input paths: vocabulary first, then cursors riding it, then rings riding the cursors through the paint keys, then the document describing what shipped. A developer reading the result sees one key funnel with a focus model beside it, and `Escape` with two halves — unwind the stack, then close. Pointer paths are untouched; keyboard and pointer share the same activation calls.

<!--
  DEPENDENCIES
-->
## Dependencies

- Shell keyboard feed (`keyboard.rs`, `panel.rs on_key`) — extended in place.
- Overview/panel/dock/popup paint passes — ring additions.
- Compositor window chords — collision boundary only; no changes.

Design documents this plan was built on: none — the spec carries no design references.

<!--
  TESTING APPROACH
-->
## Testing Approach

The scripted key-only run carries the headline metric: open calendar, launch a dock app, open an indicator menu, all through `on_key` injection in the headless harness. Paint tests sample surface buffers for the focus ring on every shell surface; an enumeration test fails if a focusable surface lacks a ring. Collision tests pin the free-surface rule on both crates. Heaviest coverage sits on the focus model and Escape unwind, where the no-trap guarantee lives. Deliberate gap: no AT-SPI bridge (a non-goal) and no physical-keyboard timing.

<!--
  MILESTONES & TASKS
-->
## Milestones & Tasks

### Milestone 1: Shell surfaces take keyboard focus

**What changes**: Panel strip and dock navigate by keyboard with visible rings; F6 cycles regions, arrows/Tab move, Enter activates.

#### - [x] Task: Key vocabulary and routing
**Id:** 43db0956-dee3-4c47-9c28-505db61a6209
**Repo:** rust-wayland-desktop
**Depends on:** none
**Execution:** agent

Extend the shell's key vocabulary with navigation (arrows, Tab/Shift+Tab, F6, Enter, and the new chords) and route them through the existing `on_key` funnel, keeping today's Text/Erase/Submit/Dismiss behavior byte-identical. Pin the collision rule with tests on both crates: shell chords never touch the compositor's window keys.

*Technical detail:* [context.md#task-key-vocabulary-and-routing](./context.md#task-key-vocabulary-and-routing)

**Acceptance criteria**:

- [x] Arrow, Tab, F6, and Enter presses resolve to navigation actions in unit tests
- [x] Existing search/Submit/Dismiss behavior unchanged (existing suites green)
- [x] Collision tests fail if a shell chord overlaps a compositor window chord

#### - [x] Task: Panel and dock keyboard focus
**Id:** ec1514b4-ed10-4fc1-9bb7-ec5159dabafe
**Repo:** rust-wayland-desktop
**Depends on:**
- 43db0956-dee3-4c47-9c28-505db61a6209 — Key vocabulary and routing
**Execution:** agent

Add panel-strip and dock cursors to the focus model with visible focus rings in the existing accent style, F6 region cycling, and Enter activation through the same calls the pointer uses. Paint keys gain the focus fields so stale highlights are impossible.

*Technical detail:* [context.md#task-panel-and-dock-keyboard-focus](./context.md#task-panel-and-dock-keyboard-focus)

**Acceptance criteria**:

- [x] Arrow/Tab movement across tiles and dock slots asserted in tests
- [x] Every focused tile and slot paints its ring, sampled from buffers
- [x] Enter on a focused tile or slot fires the pointer-equivalent action

### Milestone 2: Popups, Escape, and the keymap

**What changes**: Popup rows and overview results navigate; Escape unwinds to the previous window; the keymap document lands; the scripted key-only run passes end to end.

#### - [x] Task: Popups, overview navigation, and Escape return
**Id:** 317437b8-224e-4b6b-aaaf-606b10c24469
**Repo:** rust-wayland-desktop
**Depends on:**
- ec1514b4-ed10-4fc1-9bb7-ec5159dabafe — Panel and dock keyboard focus
**Execution:** agent

Navigate popup rows and overview results by keyboard and split Escape in two: unwind the return stack first (popup to previous shell focus), then return to the previously focused window — replacing close-everything for keyboard flows while keeping existing close behavior for pointer flows.

*Technical detail:* [context.md#task-popups-overview-navigation-and-escape-return](./context.md#task-popups-overview-navigation-and-escape-return)

**Acceptance criteria**:

- [x] Popup rows and overview results navigate and activate by keyboard in tests
- [x] Escape from any popup or menu returns to the previously focused window
- [x] Focus never sticks in a closed surface (trap tests)

#### - [x] Task: Keymap document and scripted key-only run
**Id:** 52540d55-bfe6-445c-bee3-3294d293f921
**Repo:** rust-wayland-desktop
**Depends on:**
- 317437b8-224e-4b6b-aaaf-606b10c24469 — Popups, overview navigation, and Escape return
**Execution:** agent

Write the keymap as the single discoverable reference for every shell and compositor chord, and prove the headline metric with a scripted key-only run (open calendar, launch a dock app, open an indicator menu) through `on_key` injection in the headless harness.

*Technical detail:* [context.md#task-keymap-document-and-scripted-key-only-run](./context.md#task-keymap-document-and-scripted-key-only-run)

**Acceptance criteria**:

- [x] Keymap document lists every shell and compositor chord in one place
- [x] Scripted key-only run completes all three flows in a behavioural test
- [x] Full workspace gates stay green

<!--
  OPEN QUESTIONS
-->
## Open Questions

No open questions — the collision surface, vocabulary gap, and paint pattern were all resolvable from the code, and the chord choices sit on verified-free surface. The one genuine unknown — physical-keyboard feel and timing — is out of scope for automated tests, not a question.

<!--
  OUT OF SCOPE
-->
## Out of Scope

Out of scope for this plan:

- Screen reader (AT-SPI) bridge — separate future work per the spec.
- Key rebinding UI — chords are fixed; rebinding is future work.
- Sticky keys and other accessX features.
- Compositor window-chord changes — the collision boundary is tested, not moved.
- Pointer-path changes — keyboard shares activation calls but pointer behavior is untouched.

## Changelog

### 2026-09-30 — Task: Keymap document and scripted key-only run

**What was done**: `docs/keymap.md` lists every shell chord (focus movement, activation, split Escape) and every compositor window chord, verified against both crates' key tables, with the collision rule. Hosted indicator cells joined the panel strip stops (Enter shares `open_indicator_menu`; rings, settle, and paint keys follow). The headline run `key_only_run_opens_launches_and_activates` drives all three flows through `on_key` plus the run-loop drains: calendar open/walk/Escape, Super+D dock launch through the tracker, indicator Tabs plus in-place activation. Five tests added (indicator stops, menu row fire/restore, unhost settle, cell ring, the run); gates green (504 passed, 0 failed; fmt; clippy -D warnings).

**Deviations**: None.

**Files changed**:
- `crates/shell-host/src/panel.rs`
- `docs/keymap.md`


### 2026-09-30 — Task: Popups, overview navigation, and Escape return

**What was done**: `FocusRegion` grew `Popup`/`Overview` plus a bounded `return_stack`. Keyboard-opened popups park the strip cursor and take row focus (calendar footers, network/sound/lock rows, indicator rows all fire through index-based twins of the press path); overview open parks and takes result focus over a refreshed hit cache, with movement keys routed to rows while open and Enter taking the focused row (top hit elsewhere). Escape splits in two: popup closes first restoring the parked cursor, then overview dismisses onto the focused window. `settle_focus` after every state change pops dead surfaces through the stack (draining dead tops), so focus never sticks. Rings on popup rows (`tile_row_rect`/`menu_row_rect` inverses) and overview rows (`draw_result_focus`); all three paint keys carry focus. Fifteen tests cover row/result movement, split Escape, trap fallback, live intent park/restore, and ring sampling.

**Deviations**: Shell tests never fire Gio-backed calendar rows (format/weekday toggles write shared desktop state); row activation end to end is proven by the in-memory power lock row, flips by the tiles backend tests.

**Files changed**:
- `crates/shell-host/src/panel.rs`
- `crates/shell-host/src/popup.rs`
- `crates/shell-host/src/watcher.rs`
- `crates/shell-host/src/overview.rs`


### 2026-09-30 — Task: Panel and dock keyboard focus

**What was done**: `ShellFocus` cursor (`Panel`/`Dock` regions) rides the queued navigation vocabulary via `step_focus` in the run-loop driver — Tab/Shift+Tab and arrows walk stops with wrap, F6 flips regions (skipping an itemless dock), Super+D and Alt+F1 jump regions, Up/Down cross between strip and dock. Closed-overview Enter arms `pending_shell_submit` and activates through the pointer-equivalent calls (`PopupState::toggle`, now public, for strip stops; left-press for dock slots); overview-open Enter keeps its top-hit path. Focus rings paint in the accent style on both surfaces, `PanelPaintKey`/`DockPaintKey` carry the cursor so any move repaints, and the dock surface moved to `OnDemand` interactivity. Thirteen tests cover movement, activation, rings sampled from buffers, key-carried repaint, and focus clearing on an emptied dock.

**Deviations**: None.

**Files changed**:
- `crates/shell-host/src/panel.rs`
- `crates/shell-host/src/popup.rs`


### 2026-09-30 — Task: Key vocabulary and routing

**What was done**: `KeyAction` grew nine navigation variants (arrows, Tab/Shift+Tab, F6, Super+D, Alt+F1) resolved keysym-first with keycode-tracked modifiers, and `on_key` queues them into `pending_nav` (drained by `take_nav`) in both overview states while search actions keep their gating byte-identical.

**Deviations**: None.

**Files changed**:
- `crates/shell-host/src/keyboard.rs`
- `crates/shell-host/src/panel.rs`

**Discoveries**: Shift+Tab arrives as `ISO_Left_Tab` (xkb level 2), not Tab-with-Shift — caught by the new tests, fixed with a dedicated arm. Super+Shift+D yields `KEY_D`, covered alongside `KEY_d`. Modifier tracking by evdev keycode (mirroring the compositor) keeps chords layout-independent.
