---
created_date: "2026-09-29"
document_status: draft
---

## Overview

Roost honors the shared GNOME desktop settings instead of owning a parallel settings universe, growing the shell from its present painted panel toward calendar menus, a dock, and app indicators. The first shippable slice proves the settings read path through the bar clock; later slices add popups, the dock, and the indicator host with a compositor wallpaper hook.

## Conventions

No project conventions apply to this feature — `spektacular knowledge always-applied --tier repo --filter rust-wayland-desktop` returned zero entries during discovery.

## Architecture & Design Decisions

Three options were weighed against the discovery evidence (see `research.md#alternatives-considered-and-rejected`).

**Option A — shell-side settings read layer + painted surface growth (chosen, Medium effort).** A small settings module in `shell-host` reads shared GNOME keys through the platform settings backend and feeds the existing painted panel/dock/indicator surfaces; the dock composes the existing desktop enumeration, favorites, and control-socket window state; indicators implement the standard watcher/item D-Bus roles. Pros: no new processes, no protocol change, each slice ships independently. Cons: `shell-host` grows a settings dependency and three new surfaces over time.

**Option B — settings daemon translating GNOME keys into Roost state (rejected, High effort).** A long-lived broker mirrors GNOME keys into a Roost schema. Pros: single read path for all surfaces. Cons: sync failure modes, another supervised process, read-mostly mapping doesn't justify it.

**Option C — GTK shell surfaces (rejected).** Rebuild bar/dock/indicators as GTK widgets. Pros: free widgets, HIG patterns. Cons: contradicts the decided pixel-painted constraint and pulls GTK into the supervised shell process.

The chosen shape keeps every requirement inside the shell-host crate plus a compositor background hook for wallpaper later: settings reads feed paint inputs (clock format first, wallpaper when the hook exists); the dock and indicators are new painted layer surfaces reusing existing inputs; nothing crosses the control protocol except window state the shell already receives. The knowledge entry on dconf interop (`learnings/dconf-settings-interop.md`) is decided here: honor shared schemas directly, no shims.

## Component Breakdown

- **Settings read layer** (new, in the shell): owns platform settings keys and exposes plain paint inputs (clock format, wallpaper URI); services poll it on the existing slow tick so a stuck settings backend can never hang the shell.
- **Calendar + status menus** (new, on the existing bar surface): own popup content for the clock and service tiles; reuse the panel's layer surface and dismissal behavior.
- **Dock** (new surface): owns pinned launch, running-app tracking from control-socket state, and switch/quit actions; composes desktop enumeration and favorites rather than re-deriving them.
- **Indicator host** (new, in the shell): owns the watcher role and item menus; renders icons into the bar's right edge beside the existing presence tiles.
- **Background hook** (changed, compositor): owns image-backed background painting behind the solid clear; wallpaper URI flows out-of-band, never through the control protocol.

## Data Structures & Interfaces

Contracts between the settings layer and the painted surfaces (shapes; field detail lives in the plan's context):

```
enum ClockFormat { TwelveHour, TwentyFourHour }   // from the desktop clock-format key; default 24h
struct ShellSettings {                             // snapshot the paint code reads
    clock_format: ClockFormat,                     // TwelveHour renders `h:MM`, else `HH:MM`
    wallpaper_uri: Option<String>,                 // applied when the background hook exists
}
```

- `ShellSettings` is a plain snapshot refreshed on the slow tick; paint functions take it by value and never touch the settings backend, so surfaces stay deterministic and testable without D-Bus.
- Dock running state reuses the window list the shell already receives over the control socket — no new message or command.
- Indicator icons/menus arrive over the standard watcher/item roles; the host keeps only icon pixels and menu layouts, never client internals.

## Implementation Detail

The work introduces one new pattern — a settings snapshot behind the paint boundary — and otherwise follows existing codebase patterns throughout.

A developer reading the changed shell sees the same painted surfaces as today, except their variable inputs (clock text, tile states, wallpaper) now resolve through one snapshot refreshed on the slow tick that already drives tile probing. The settings backend is called only from that tick path, so a missing bus, missing schema, or slow daemon degrades to defaults exactly like an unreachable service does today. Milestones land in slice order so each is independently shippable: clock format proves the read path, the calendar exercises popup content on the bar, the dock composes existing inputs into a new surface, and indicators add the first D-Bus server role the shell owns.

## Dependencies

- Platform settings backend (`gio`-family crate, cached in the offline registry) — read-only access to shared GNOME keys; no settings UI of its own.
- `gsettings-desktop-schemas` on the image — provides `org.gnome.desktop.*` at runtime; verify presence on marlin at plan time.
- D-Bus session bus in the Roost session — carries settings reads and the indicator watcher/item roles.
- Final spec `specs/20260929153713-b5326586-settings-compat-shell-surfaces.md` — the binding contract.
- Knowledge `learnings/dconf-settings-interop.md` — the open interop item this plan decides.
- This spec carries no design references.

## Testing Approach

Unit tests cover the snapshot mapping (clock-format key to rendered text, missing-bus defaults) and popup/dock/indicator paint against fixed snapshots — no D-Bus in unit tests. Integration tests over the existing shell harnesses cover dock lifecycle against real desktop entries and indicator menu delivery. Heaviest coverage lands on the settings snapshot boundary, since every surface reads through it. Success-metric mapping: settings round trip → behavioural test with a controlled settings backend; dock task completion → behavioural test against real applications; indicator presence → behavioural test with a stub indicator client.

## Milestones & Tasks

### Milestone 1: Settings read proves out in the clock

**What changes:** The bar clock follows the GNOME clock-format setting; everything else looks identical. Validation: unit tests on the snapshot mapping plus a round-trip test against a controlled settings backend.

### Milestone 2: Bar popups and dock

**What changes:** The clock opens a calendar, status tiles open control menus, and a dock launches, marks, switches, and quits apps. Validation: popup behavior tests and dock lifecycle tests against real desktop entries.

### Milestone 3: Indicators and wallpaper hook

**What changes:** Tray-style apps show working indicators in the bar, and the Settings wallpaper appears behind the session. Validation: stub-client indicator test and wallpaper round-trip test.

#### - [x] Task: Settings snapshot with clock-format read
**Id:** 14b861ec-52e1-41f1-a6dc-fa3c52a52c7d
**Repo:** rust-wayland-desktop
**Depends on:** none
**Execution:** agent

Add the shell settings snapshot fed by the platform settings backend on the existing slow tick, and render the bar clock from it (12/24-hour per the desktop key, 24-hour default). Degrade to defaults with no bus or schema.

*Technical detail:* [context.md#task-settings-snapshot-with-clock-format-read](./context.md#task-settings-snapshot-with-clock-format-read)

**Acceptance criteria**:
- [x] Bar clock follows the GNOME clock-format setting with no restart
- [x] Missing bus or schema renders exactly as today

#### - [x] Task: Calendar and status-menu popups
**Id:** 1adc1ed0-1b52-481f-aa38-3c4ab4bcf54c
**Repo:** rust-wayland-desktop
**Depends on:**
- 14b861ec-52e1-41f1-a6dc-fa3c52a52c7d — Settings snapshot with clock-format read
**Execution:** agent

Open a month calendar from the bar clock and control menus from the service tiles on the existing layer surface, with shell-popup dismissal.

*Technical detail:* [context.md#task-calendar-and-status-menu-popups](./context.md#task-calendar-and-status-menu-popups)

**Acceptance criteria**:
- [x] Clock activation opens a dismissible month calendar
- [x] Service tiles open their control menus

#### - [x] Task: Dock surface with app lifecycle
**Id:** 3b6fa9e4-3b2a-417f-af52-1713bd7838a4
**Repo:** rust-wayland-desktop
**Depends on:**
- 1adc1ed0-1b52-481f-aa38-3c4ab4bcf54c — Calendar and status-menu popups
**Execution:** agent

Add the painted dock composing desktop enumeration, favorites, and control-socket window state: pin, launch, running marks, switch, quit.

*Technical detail:* [context.md#task-dock-surface-with-app-lifecycle](./context.md#task-dock-surface-with-app-lifecycle)

**Acceptance criteria**:
- [x] Pin, launch, switch, and quit from the dock behave against real apps
- [x] Running apps are marked in the dock

#### - [x] Task: Indicator host and wallpaper hook
**Id:** 46f72cf3-75c6-4160-94c3-82f2105ee976
**Repo:** rust-wayland-desktop
**Depends on:**
- 3b6fa9e4-3b2a-417f-af52-1713bd7838a4 — Dock surface with app lifecycle
**Execution:** agent

Implement the indicator watcher role with item menus in the bar, and the compositor image-background hook fed by the snapshot wallpaper URI.

*Technical detail:* [context.md#task-indicator-host-and-wallpaper-hook](./context.md#task-indicator-host-and-wallpaper-hook)

**Acceptance criteria**:
- [x] A stub indicator client shows its icon and working menu in the bar
- [x] The Settings wallpaper renders behind the session

## Open Questions

- Whether the marlin image carries `gsettings-desktop-schemas` — assumed present via the GNOME-ecosystem packages; the implementer verifies and records either way.
- Whether a D-Bus session bus is up before the shell's first tick — the snapshot defaults cover absence, so this shapes testing, not design.

## Out of Scope

- The Rhai extension system (future spec).
- GNOME Shell/Mutter key semantics beyond non-honoring.
- Settings write-back from Roost surfaces (read-only in this plan).

## Changelog

### 2026-09-29 — Task: Settings snapshot with clock-format read

**What was done**: New `shell-host` settings module reading shared GNOME keys through `gio` behind a plain snapshot the paint code takes by value; the bar clock renders 12/24-hour from the desktop key on the existing slow tick, degrading to the previous output with no bus or schema.

**Deviations**: Wallpaper URI is carried in the snapshot but not applied (no compositor hook yet, as planned for the later task); `Settings::string` does not exist in gio 0.18, so reads go through `value().str()` behind a schema/key guard.

**Files changed**:
- `crates/shell-host/src/settings.rs`
- `crates/shell-host/src/tiles.rs`
- `crates/shell-host/src/lib.rs`
- `crates/shell-host/Cargo.toml`
- `Cargo.toml`
- `Cargo.lock`

**Discoveries**: `gio` 0.18 needs `SettingsExt` in scope and schema lookup up front to stay quiet on missing schemas; live round trip verified by flipping the host `clock-format` key (`24h`→`12h`→`24h`).

### 2026-09-29 — Task: Calendar and status-menu popups

**What was done**: New wire-free `popup` module (hit layout mirroring the painted strip, toggle state machine, Monday-first month grid, painted calendar and service menus) plus first-ever shell pointer plumbing (`wl_pointer` capability-gated like the keyboard, press coordinates from motion tracking, Escape dismissal with and without the overview open). The panel surface grows by the popup band on open and shrinks back on close, exclusive zone unchanged.

**Deviations**: `paint_panel` gained a strip-height parameter so the popup band stays clean (existing callers updated); popup activation is press-only, no hover.

**Files changed**:
- `crates/shell-host/src/popup.rs`
- `crates/shell-host/src/panel.rs`
- `crates/shell-host/src/overview.rs`
- `crates/shell-host/src/lib.rs`

**Discoveries**: `wl_pointer::Button` carries no coordinates — position must be tracked from Enter/Motion; let-chains are unavailable (edition 2021), so the press gate uses a tuple `if let`.

### 2026-09-29 — Task: Dock surface with app lifecycle

**What was done**: New wire-free `dock` module composing favorites, desktop entries, and control-socket window state into items, with a press matrix (left launch/switch, middle close, right pin) and a painted bottom layer surface that grows on configure. Polite window close travels a new `CloseWindow` control message (protocol 0.5) drained by the hub and ticked by the runtime. Folder stacks extend the dock: a `stack:<dir>` favorite opens a macOS-like grid above the strip (launchable entries first, plain files via `xdg-open`), and the surface grows by the grid band while open.

**Deviations**: Stacks were not in the approved task text — added per direct user request ("file stacks like macos has in the dock") and recorded here rather than as a new task. Middle-click close needs the compositor runtime tick to actually close; the shell only arms the action.

**Files changed**:
- `crates/shell-host/src/dock.rs`
- `crates/shell-host/src/panel.rs`
- `crates/shell-host/src/apps.rs`
- `crates/shell-host/src/lib.rs`
- `crates/shell-control-schema/src/lib.rs`
- `crates/compositor/src/control.rs`
- `crates/compositor/src/runtime.rs`

**Discoveries**: `DesktopEntry::from_path` plus the existing launchability filter gives single-file parsing for stack cells with no new dependency; unreadable stack directories must read as empty so paint never fails. Stack items must never claim windows (their `xdg-open` children are untracked) or same-titled windows vanish into the stack. Clippy's `manual_range_contains` and `cloned_ref_to_slice_refs` fire on grid hit-test and test code — write those in the preferred form first pass.

### 2026-09-29 — Task: Indicator host and wallpaper hook

**What was done**: New wire-free `watcher` module owning the `org.kde.StatusNotifierWatcher` role over `zbus` blocking calls polled on the slow tick: item registration, largest-pixmap icon fetch, one-level dbusmenu rows, and `Activate`/`Event` calls. Icons paint in cells left of the sound tile; presses open indicator menus as popups (empty menus activate directly). The compositor renders the Settings wallpaper as a memory element behind everything, fed by a runtime-dir drop file the shell publishes on snapshot change; the recovery overlay keeps its red.

**Deviations**: Watcher signal emission unwired in v1 (single-host session; our host reads shared state directly). dbusmenu nesting flattens to one level. CI's nested-session jobs needed `libglib2.0-dev` for the gio build (fixed in ci.yml). Icon theme lookup deferred to roadmap issue #21.

**Files changed**:
- `crates/shell-host/src/watcher.rs`
- `crates/shell-host/src/panel.rs`
- `crates/shell-host/src/popup.rs`
- `crates/shell-host/src/tiles.rs`
- `crates/shell-host/src/lib.rs`
- `crates/shell-host/Cargo.toml`
- `crates/compositor/src/wallpaper.rs`
- `crates/compositor/src/runtime.rs`
- `crates/compositor/src/lib.rs`
- `crates/compositor/Cargo.toml`
- `Cargo.toml`
- `Cargo.lock`
- `.github/workflows/ci.yml`

**Discoveries**: `zbus` 5 blocking `Proxy::call` takes three generics (`M, B, R`) — turbofish as `call::<&str, _, ()>`; `Dict::get` needs a sized key (`Str::from`). Mixing smithay element types in one list needs DMA import bounds the winit backend lacks — wallpaper draws in its own pass. A private `dbus-daemon --session` gives live stub-client tests with skip fallback when no daemon exists.
