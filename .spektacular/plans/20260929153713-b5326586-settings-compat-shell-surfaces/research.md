---
created_date: "2026-09-29"
document_status: draft
---

# Research: settings-compat-shell-surfaces

## Alternatives considered and rejected

- Shim schemas duplicating GNOME keys under Roost names (rejected: splits
  the ecosystem; Settings writes would never reach Roost;
  knowledge `learnings/dconf-settings-interop.md` frames it as one option
  but the spec decides for direct honoring).
- Translating settings daemon mirroring GNOME keys into Roost state
  (rejected: new long-lived process + sync failure modes for what is a
  read-mostly mapping).
- GTK widgets for bar/dock/indicators (rejected by user: shell-host stays
  GTK-free and pixel-painted).

## Chosen approach — evidence

- `crates/shell-host/src/tiles.rs:132-140` — clock hardcoded to `%H:%M`
  via jiff; the exact hardcoding the knowledge entry warns against, and
  the smallest slice that proves GSettings reads end to end.
- `crates/shell-host/src/apps.rs` + `favorites.rs` — desktop enumeration
  and pinning already exist: the dock's two inputs.
- Control-socket window state (`crates/compositor/src/control.rs`) —
  running-app side of the dock without new protocol.
- Registry cache holds `gio`/`zbus` crates; `/usr/share/glib-2.0/schemas`
  ships `org.gnome.desktop.interface` + `background` locally.

## Files examined

- `crates/shell-host/src/tiles.rs:64-140` — tile set, clock text, probing.
- `crates/shell-host/src/panel.rs:69-77,183` — layer namespaces, panel height, tile refresh.
- `crates/shell-host/src/overview.rs:279-308` — painted strip: clock + tiles.
- `crates/compositor/src/runtime.rs:450-500` — solid-color background clear (wallpaper has no hook yet).
- `crates/shell-host/src/apps.rs`, `favorites.rs` — launch inputs for dock.

## External references

- StatusNotifierItem/Watcher D-Bus specification — indicator protocol contract.
- GSettings `org.gnome.desktop.interface` (`clock-format`) and
  `org.gnome.desktop.background` (`picture-uri`) keys — settings surface.

## Prior plans / specs consulted

- Knowledge `learnings/dconf-settings-interop.md` — open R9 item this spec decides (honor, don't shim).
- Plan `20260928234216-ff77953a-scrollable-tiling-mode` — control protocol carries no shell settings; settings stay out-of-band by design.

## Open assumptions

- `gsettings-desktop-schemas` present on the marlin image (pulled via the
  GNOME-ecosystem packages in the manifest; verify at plan time).
- Live D-Bus session bus available in the Roost session for GSettings/dconf
  reads (tiles.rs already plans D-Bus service probing).
- Wallpaper rendering needs a compositor background-image hook that does
  not exist yet — scoped to a later milestone, not the GSettings slice.

## Rehydration cues

- Re-read `tiles.rs::clock_text`, `overview.rs::paint_panel`, and the
  knowledge dconf entry above; check `gio` version against the GTK4 stack
  already used by the greeter crate.

## Drafting assumptions

### Autonomous drafting (plan steps)

- **Decision**: drafted all plan sections from the approved spec and settled interview decisions without per-section confirmation rounds.
- **Rationale**: user ordered fully autonomous spec/plan drafting; every section traces to the signed-off spec or explicit user decisions (pixel-painted bar, Rhai deferred).
- **Rejected**: pausing after each section, which stalls the autonomous loop the goal requires.
