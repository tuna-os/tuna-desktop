---
created_date: "2026-09-30"
document_status: final
closed_date: "2026-09-30"
---

# Feature: 20260930211001-be2874f8-keyboard-access-approval

<!--
  OVERVIEW
-->
## Overview

Every Roost shell action gains a keyboard equivalent with visible focus: bar tiles, dock icons, popups, and menus all operate without a pointer, and the keymap is documented in one place.

<!--
  REQUIREMENTS
-->
## Requirements

- [x] **Full keyboard operation**
  Every pointer action on bar, dock, popups, and menus has a keyboard equivalent.
- [x] **Visible focus**
  Keyboard focus in shell surfaces is always visibly indicated.
- [x] **Documented keymap**
  The shell keymap is documented in a single discoverable place.
- [x] **No keyboard traps**
  Focus never gets stuck in a shell surface; Escape always returns to the previously focused window.

<!--
  CONSTRAINTS
-->
## Constraints

- New bindings must not collide with the compositor's existing window-management keys.
- Focus indication reuses the existing accent and paint style.

<!--
  ACCEPTANCE CRITERIA
-->
## Acceptance Criteria

- [x] **Pointer-free session**
  A scripted key-only run opens the calendar, launches an app from the dock, and opens an indicator menu.
- [x] **Focus always visible**
  Every focused shell element paints its focus ring.
- [x] **Escape unwedges**
  From any open popup or menu, Escape returns focus to the previously focused window.

<!--
  TECHNICAL APPROACH
-->
## Technical Approach

- Decided direction the planner may adapt: extend the existing keyboard feed with shell-focus routing, focus rings in the paint passes, and a keymap document as the single reference.

<!--
  SUCCESS METRICS
-->
## Success Metrics

- Keyboard users complete bar, dock, and menu flows without a pointer.
- No shell surface ever lacks a visible focus indicator.

<!--
  NON-GOALS
-->
## Non-Goals

- Screen reader (AT-SPI) bridge: separate future work.
- Key rebinding UI.
- Sticky keys and other accessX features.
