---
created_date: "2026-09-30"
document_status: final
closed_date: "2026-09-30"
---

# Feature: 20260930210650-431b34a7-session-lock-approval

<!--
  OVERVIEW
-->
## Overview

Roost locks idle sessions behind an unlock screen: idle detection blanks and locks the session, and the user returns through the greeter auth path. Unattended machines stop being open doors.

<!--
  REQUIREMENTS
-->
## Requirements

- [x] **Idle locks the session**
  An idle session blanks and locks itself after the configured timeout.
- [x] **Manual lock works**
  Users can lock the session immediately from the shell.
- [x] **Unlock uses session auth**
  Entering credentials unlocks the session; wrong credentials stay locked with no hint about what failed.
- [x] **Lock survives shell restarts**
  A shell crash or restart while locked stays locked.

<!--
  CONSTRAINTS
-->
## Constraints

- Locked state lives in the compositor, never the restartable shell: a shell crash cannot unlock.
- The lock screen shows no window content, titles, or notifications while engaged.
- Authentication reuses the greeter auth path; no parallel credential handling.
- Authentication failures must not indicate what failed.

<!--
  ACCEPTANCE CRITERIA
-->
## Acceptance Criteria

- [x] **Timeout locks**
  With a short idle timeout set, an untouched session shows the lock screen and hides all windows.
- [x] **Password unlocks**
  Entering the correct password dismisses the lock and restores the session intact.
- [x] **Wrong password stays locked**
  Three wrong passwords keep the session locked with no window content leaked.
- [x] **Restart stays locked**
  Restarting the shell process while locked keeps the lock screen up.

<!--
  TECHNICAL APPROACH
-->
## Technical Approach

- Decided direction the planner may adapt: compositor-owned lock flag with an overlay-style exclusive surface, idle tracking from input timestamps, and unlock mediated by the greeter auth path.

<!--
  SUCCESS METRICS
-->
## Success Metrics

- Idle timeout produces the lock surface in real sessions.
- Wrong-credential unlock attempts never reveal content.

<!--
  NON-GOALS
-->
## Non-Goals

- Fingerprint or smartcard unlock factors.
- Remote lock administration.
- Per-user lock customization themes.
