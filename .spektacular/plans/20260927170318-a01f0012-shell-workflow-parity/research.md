---
created_date: "2026-09-27"
document_status: final
closed_date: "2026-09-28"
---

# Research: 20260927170318-a01f0012-shell-workflow-parity

## Alternatives considered and rejected

### GNOME Shell JS as implementation reference

Unavailable: gnome-shell is absent from references (mutter tree is
compositor-only); parity comes from the pinned baseline journeys plus
the discrepancy ledger, not from reading shell code.

**Rejected**: out of tree.

### Full org.freedesktop.Notifications D-Bus daemon in 002

No D-Bus/zbus in tree; spec R5/R8 plus 004 own the service
boundary. The slice keeps notifications as an in-process
banner/history store behind the freedesktop-shaped API; the bus
daemon moves to 004.

**Rejected**: privileged infrastructure 004 owns, against services
absent in CI.

### Service-backed quick settings first

All references keep NM/UPower/PipeWire clients in shell or settings-
daemon processes over D-Bus, never the compositor — but each needs a
new client crate and a live service. The slice leads with
loading/disconnected/error states (A3 allows them) plus the local
clock; live bindings follow per service with presence detection.

**Rejected**: blocking the slice on services absent in CI.

### In-compositor overview (niri shape)

RWD shell is a separate supervised client by ADR 0003 and 001; the
overview state and rendering stay in shell-host while the compositor
routes triggers and owns focus (R6).

**Rejected**: duplicates the GLES path and breaks the
supervised-shell recovery story.

### Per-motion IPC for pointer interactions

The spec flags it as the architecture risk; references do grabs
in-process. The slice keeps grabs and selection client-side in the
shell and sends intent commands (activate/move/select) over control,
never motion streams.

**Rejected**: spec-flagged risk with reference consensus against it.

## Chosen approach — evidence

- Shell stays a separate client; compositor owns focus via tokens:
  `crates/compositor/src/control.rs:581` (`apply_command` gates on
  validator), `crates/shell-host/src/control.rs:274`
  (`activate_selected`), `crates/shell-host/src/model.rs:46`
  (`ShellModel` windows/workspaces/overview/selected).
- Overview triggers route through compositor input:
  `crates/compositor/src/windows.rs:418` (`ManagerInput::Key`), evdev
  mapping precedent `crates/compositor/src/overlay.rs` (`keycode`
  module); niri binds `Action::{Toggle,Open,Close}Overview` plus a hot
  corner (`references/niri/src/input/mod.rs:~2294,~2441`).
- Dynamic workspaces extend the model: `StateModel` already carries
  workspace ids (`crates/compositor/src/state.rs`), single-workspace
  policy today; mutter precedent `dynamic-workspaces` plus MRU
  (`references/mutter/src/core/workspace.c:243`).
- Desktop-entry launching plus async search providers: no parser or
  search in tree; niri/cosmic both externalize launchers over
  layer-shell and xdg-activation
  (`references/niri/src/utils/spawning.rs`,
  `references/cosmic-comp/src/wayland/protocols/toplevel_management.rs:58-71`).
- Notifications center as local store first:
  `org.freedesktop.Notifications` is owned by external daemons in
  every reference; 004 owns RWD service integration.
- Toolkit GTK4/libadwaita: greeter precedent
  (`crates/greeter/Cargo.toml:9-10`, workspace `gtk4 = "0.11"`),
  architecture doc names GTK/libadwaita apps; AT-SPI/IME ride on GTK.
- Alt-Tab MRU derivable shell-side from `WindowEntry.active`
  transitions (no compositor change, R6 intact); niri keeps an MRU UI
  list (`references/niri/src/ui/mru.rs`).

## Files examined

- `crates/shell-host/src/model.rs:46` — ShellModel: windows,
  workspaces, overview flag, single selection; snapshot/delta entry.
- `crates/shell-host/src/control.rs:155` — ControlClient owns model
  plus revision; snapshot/delta/gap/activation flows live.
- `crates/shell-host/src/panel.rs:204` — run_panel_with_control:
  layer surface plus 200 Hz control sync loop.
- `crates/compositor/src/windows.rs:62` — WindowManager: map, focus,
  move, resize, workspace, seat routing.
- `crates/compositor/src/state.rs` — StateModel revisions, change
  log, TokenStore; workspace membership exists.
- `crates/compositor/src/control.rs:633` — ControlHub nonblocking
  multi-session driver (extendable for overview/dash intents).
- `docs/architecture.md:9` — independent GNOME-inspired session,
  GTK/libadwaita apps, no GNOME Shell extensions or private APIs.
- `docs/adr/0002-shell-control-ipc-envelope.md:32` — token policy
  reused for every focus change (A4).
- `.spektacular/specs/20260927170316-a01f0010` (via CLI) — 000 is
  Draft; pinned baseline release/distro/GPU unselected; toolkit
  spike R3 open.
- `.spektacular/plans/20260927170317-a01f0011-001-.../plan.md` (via
  CLI) — 001 closed agent tasks T1–T6 plus fixes, T7 human pending.

## External references

- niri `src/input/mod.rs`, `src/layout/*`, `src/ui/mru.rs`,
  `src/a11y.rs`, `src/dbus/freedesktop_a11y.rs` — overview triggers,
  dynamic workspaces, MRU, AT announcements; launchers external.
- cosmic-comp `src/shell/*`, `src/input/actions.rs`,
  `src/wayland/protocols/{toplevel_management,a11y}.rs`,
  `src/shell/zoom.rs` — external launcher/switcher protocols,
  edge-snap engine, zoom; applets out of tree.
- mutter `src/core/{keybindings.c,workspace.c,window.c,prefs.c}`,
  `src/backends/meta-a11y-manager.c` — keybinding dispatch, dynamic
  workspaces plus MRU, window state machine, a11y bus; shell UI out
  of tree (`org.gnome.Shell` D-Bus boundary).
- freedesktop startup-notification plus xdg-activation — launch
  feedback without new protocol (mutter
  `core/startup-notification.c`).

## Prior plans / specs consulted

- 000 program-baseline-and-spikes (spec, Draft): baseline unselected,
  toolkit spike open — 002 proceeds on provisional assumptions
  (GTK4/libadwaita per greeter precedent; journeys recorded against
  whatever GNOME the reviewer runs, ledger captures deltas).
- 001 nested-compositor-shell-recovery (plan, final): control, token,
  supervision, and overlay contracts are the floor 002 builds on; T7
  human run still open.
- 004 secure-session-accessibility-integration (spec): owns
  notification/service integration plus production identity — 002
  must not build the daemon or privileged APIs.

## Open assumptions

- Pinned GNOME baseline gets selected during 002 (owner: user); until
  then journeys record reviewer environment plus version in the
  ledger. If no baseline is chosen, STOP the parity-claim tasks, keep
  building to the spec's letter.
- GTK4/libadwaita is the shell toolkit (greeter precedent); a 000
  spike reversal would reshape rendering tasks — assume it holds.
- System services (NM/UPower/PipeWire) are absent in CI: unavailable
  states are the automated proof; live-service runs are manual.
- Touchpad gestures plus screen-reader runs are human-reviewed (A1/A5
  allow it); automation covers keyboard flows and state machines.

## Drafting assumptions

### Single-repo scope (discovery)
- **Decision**: all 002 changes land in `rust-wayland-desktop`.
- **Rationale**: repo roster has one member repo; shell, compositor,
  schema, and docs all live there.
- **Rejected**: splitting shell UI into a new crate/repo — premature;
  extraction seams (model/control split) already exist.

### No design references (discovery)
- **Decision**: record "spec carries no design refs" at Dependencies.
- **Rationale**: `design ref list` returned zero refs, zero
  unresolved.
- **Rejected**: inventing a design doc unasked.

### Child survey scope (discovery)
- **Decision**: one read-only reference-survey child, no code contact.
- **Rationale**: references are large; file-path patterns were
  needed, implementation detail was not.
- **Rejected**: per-surface children (overhead) and doing it inline
  (slower, same result).

### Baseline provisional (discovery)
- **Decision**: plan proceeds with journeys recording reviewer
  environment; baseline selection is an open assumption, not a
  blocker.
- **Rationale**: 000 is Draft with no selected baseline; 001 set the
  precedent of provisional progress with STOP rules.
- **Rejected**: blocking 002 on 000 completion.

### Thick shell client over 001 contracts (architecture)
- **Decision**: all 002 UI in shell-host; compositor gains only
  trigger routing, dynamic workspaces, and token-gated intents.
- **Rationale**: preserves ADR 0003 supervision shape and R6 focus
  authority; every reference keeps shell UI out of the compositor.
- **Rejected**: in-compositor overview (duplicates GLES path, breaks
  recovery story); D-Bus daemons now (004 owns them, services absent
  in CI); per-motion IPC (spec-flagged risk, grabs stay client-side).

### Rendering and entry-parsing defaults (open questions)
- **Decision**: gtk4-layer-shell is the default overview/panel
  renderer; a minimal freedesktop entry reader (existing crate
  preferred) is the default launcher source — both pending the 000
  spike, implementer confirms or STOPs on reversal.
- **Rationale**: standard GTK-on-layer-shell path; avoids inventing
  rendering or parsing.
- **Rejected**: deciding nothing and parking toolkit choice as an
  open question.

## Rehydration cues

- Re-read: 002 spec (CLI), this file, `assumptions.md` in this dir,
  `crates/shell-host/src/model.rs`, 001 plan changelog (T3–T6).
- Re-run: `cargo test --workspace` (134 green at 002 planning start).
- Skill: `spek-design` if a journey/ledger format needs a design doc.
