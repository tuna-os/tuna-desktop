---
created_date: "2026-09-30"
document_status: final
closed_date: "2026-09-30"
---

# Feature: 20260930054036-d6050cbc-xwayland-support

<!--
  OVERVIEW
  A concise 2-3 sentence summary of the feature. Answer three questions:
    1. What is being built?
    2. What problem does it solve?
    3. Who benefits and why does it matter?
  Avoid implementation details — this should be readable by any stakeholder.
-->
## Overview

X11-only applications run as first-class windows in the Roost
session: they map, take focus, and receive input alongside native
Wayland clients, so users are never blocked by an app whose
Wayland backend is missing or broken. Sessions without legacy
apps are untouched: nothing new runs, nothing new shows.

<!--
  REQUIREMENTS
  Specific, testable behaviours the feature must deliver.
  Format: bold title on the checkbox line, detail indented below.
  Rules:
    - Use active voice: "Users can...", "The system must..."
    - Each requirement should be independently verifiable
    - Focus on WHAT, not HOW — avoid prescribing implementation
    - Keep each item atomic — one behaviour per line
-->
## Requirements

- [x] **Legacy apps open as managed windows**
  Users can launch an X11-only app and its window maps, takes the
  same z-order position a native window would take, and focuses
  like a native client window.
- [ ] **Window identity carries over**
  The system must show an X11 window's advertised title text and
  its desktop-entry app name in the overview, switcher, and dock.
- [x] **Input reaches legacy windows**
  Users can type and click into a focused X11 window, including
  every modifier chord outside the documented session keymap.
- [x] **On-demand compatibility server**
  The system must start the X compatibility server only when a
  legacy client needs it, and a session without one passes the
  full automated suite with no new presence (no windows,
  indicators, or server log lines).
- [x] **Clean absence**
  Users get a clear, non-blocking state when the compatibility
  server binary is missing: native apps keep working and the
  failure names the missing piece.
- [x] **No privilege upgrade for legacy clients**
  The system must deny X11 clients every privileged surface,
  capture, and input-injection path it denies Wayland clients.

<!--
  CONSTRAINTS
  Hard boundaries the solution must operate within. These are non-negotiable.
  Format: one bullet point per constraint.
  Examples:
    - Must integrate with the existing authentication system
    - Cannot introduce breaking changes to the public API
    - Must support the current minimum supported runtime versions
  Leave blank if there are no constraints.
-->
## Constraints

- [x] **Pinned toolkit release line**
  The solution must build against the pinned Smithay release
  line; moving the release version is a separate architectural
  decision, not part of this spec.
- [x] **One window model**
  X11 windows must join the compositor's existing window,
  workspace, and focus model; no parallel stacking, focus, or
  snapshot path.

<!--
  ACCEPTANCE CRITERIA
  The specific, binary conditions that define "done".
  Format: bold title on the checkbox line, verifiable detail indented below.
  Each criterion must be:
    - Independently verifiable (pass/fail, not subjective)
    - Traceable back to a requirement above
    - Testable by someone who didn't write the code
-->
## Acceptance Criteria

- [x] **Legacy app maps and focuses**
  Launching an X11-only app shows its window in the z-order
  position a native window would take, focused, and accepting
  typed and clicked input.
- [ ] **Identity visible in shell surfaces**
  The X11 window's advertised title text and desktop-entry app
  name appear in the overview, the window switcher, and the
  dock.
- [x] **Unconsumed chords pass through**
  Pressing modifier chords outside the documented session keymap
  delivers them to the focused X11 window instead of consuming
  them.
- [x] **Server starts on demand**
  A native-only session logs no compatibility-server presence;
  launching the first legacy app logs exactly one server start
  and opens its window.
- [x] **Missing binary stays non-fatal**
  With no compatibility server installed, native apps run
  normally and the shell reports the missing piece instead of
  hanging or crashing.
- [x] **Privilege denials hold for X11**
  Unauthorized capture and input-injection attempts from an X11
  client are denied exactly as for native clients, with matching
  denial log lines.

<!--
  TECHNICAL APPROACH
  High-level technical direction to guide the planning agent. Include:
    - Key architectural decisions already made
    - Preferred patterns or technologies if known
    - Integration points with existing systems
    - Known risks or areas of uncertainty
  Format: one bullet point per direction/steer.
  Leave blank if you want the planner to propose the approach.
-->
## Technical Approach

Decided direction the planner may adapt: enable the pinned
toolkit's compatibility-module path (an additive feature flag
within the pinned release, not a version move) with on-demand
server spawn supervised like the shell host, mapping X11 windows
into the existing window model with title and class-derived
identity, and reusing the overview, switcher, and control paths
unchanged. The shell stays X11-ignorant: it sees X11 windows only
as ordinary window-model entries, never parsing X11 protocol.
Automated verification passes with no X server present; live X11
behavior is covered by the manual procedure, never by tests that
assume a display. Known uncertainty: the exact compatibility API
surface and its extra native dependencies (discovery will read
them, not assume them); packaging the server binary belongs to
the release track, noted here only as a dependency.

<!--
  SUCCESS METRICS
  How you will know the feature is working well after delivery. Be specific:
    - Quantitative: "p99 latency < 200ms", "error rate < 0.1%"
    - Behavioural: "users complete the flow without support intervention"
  Format: one bullet point per metric.
  Leave blank if not applicable.
-->
## Success Metrics

- A legacy app (reference: an X11-only toolkit demo) maps, focuses, and takes input on the first try in the nested session.
- The overview and switcher list the legacy window with its title during a mixed native-plus-legacy session.
- A native-only session logs no compatibility-server presence; after the first legacy launch the log shows exactly one server start.
- The full automated suite passes with no X server on PATH.
- Unauthorized capture and layer-shell probes against an X11 client fail exactly as for native clients.

<!--
  NON-GOALS
  Explicitly state what this spec does NOT cover. This is as important as
  the requirements — it prevents scope creep and sets clear expectations.
  Format: one bullet point per exclusion.
  Examples:
    - "Mobile support is out of scope (tracked in #456)"
    - "Internationalisation will be addressed in a follow-up spec"
  Leave blank if there are no explicit exclusions to call out.
-->
## Non-Goals

- Full X11 window-manager extras (EWMH extensions beyond
  map/focus/stack, tray icon embedding, XSETTINGS propagation)
  defer to a follow-up.
- X11-to-Wayland clipboard and drag-and-drop bridging is out of
  scope; each side keeps its own selection behavior. It belongs
  to the roadmap's follow-up item covering shared selections,
  not this spec.
- Packaging and installing the compatibility server binary
  belongs to the release track, not this spec.
- Rootless-vs-rootful display modes: rootless integration only;
  no nested full X desktop.
