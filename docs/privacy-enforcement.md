# Privacy device enforcement (#356, P-ST-04 slice)

How Tuna Desktop enforces the three `org.gnome.desktop.privacy`
kill-switches — `disable-camera`, `disable-microphone`,
`disable-sound-output` — where GNOME 51 enforces them, and where it
does not. This lane is denial/enforcement only: capture/recording,
privacy screen, USB protection, retention/diagnostics/reporting, and
notification preferences are classified at the end and implemented
elsewhere or not at all.

## GNOME 51 baseline (verified against source)

Schema defaults (all `false` = allowed), from
[`org.gnome.desktop.privacy.gschema.xml.in`](https://github.com/GNOME/gsettings-desktop-schemas/blob/2417a2727f3551d2da405b7fde1d12f7e4fd3d58/schemas/org.gnome.desktop.privacy.gschema.xml.in)
(gsettings-desktop-schemas `2417a27`):

| Key | Summary |
|---|---|
| `disable-camera` | Don't allow applications to access the camera |
| `disable-microphone` | Don't allow applications to access the microphone |
| `disable-sound-output` | Don't allow applications to output sound |

Settings UI: only the camera has a privacy page
([`cc-camera-page.c`](https://github.com/GNOME/gnome-control-center/blob/6ae712c0454f2586f00821c3486150c1e8feb3e7/panels/privacy/camera/cc-camera-page.c),
g-c-c `6ae712c`). It binds the master switch to `disable-camera`
and gates each per-app row on it; there are no microphone or
sound-output pages under `panels/privacy/` (directory listing at
the same revision).

Publication: `xdg-desktop-portal-gnome`
([`lockdown.c`](https://github.com/GNOME/xdg-desktop-portal-gnome/blob/a4f6ed64474ff6fe6c4eb9dbe092d97cafd6ea81/src/lockdown.c),
`a4f6ed6`) binds all three keys to the standard
`org.freedesktop.impl.portal.Lockdown` properties
`DisableCamera`/`DisableMicrophone`/`DisableSoundOutput`
(wire names hyphenated, per
[`Lockdown.xml`](https://github.com/flatpak/xdg-desktop-portal/blob/ee298d1817fed18c208c9ddb638a4aef7bcecc9b/data/org.freedesktop.impl.portal.Lockdown.xml)).

Portal consumers (xdg-desktop-portal `ee298d1`, verified identical
at the installed 1.18.4):

- Camera portal denies **new** `AccessCamera` and
  `OpenPipeWireRemote` sessions with `NOT_ALLOWED`
  ("Camera access disabled") while `DisableCamera` reads true
  ([`camera.c`](https://github.com/flatpak/xdg-desktop-portal/blob/ee298d1817fed18c208c9ddb638a4aef7bcecc9b/desktop-portal/camera.c)).
  Existing sessions are not revoked.
- Device (USB) portal denies microphone/camera/speaker device
  requests on the matching property (`src/device.c` at 1.18.4).
- Microphone capture and sound output otherwise have **no GNOME
  enforcer**: no matches for either key (hyphen or underscore
  spellings) in xdg-desktop-portal (full tree), gnome-shell `js/`
  and `src/` (`eafd268`), gnome-settings-daemon `plugins/`
  (`6301410`), mutter `src/backends`, `src/compositor`, `src/core`
  (`d82671c`, 219 files), pipewire `src/` + `spa`, or wireplumber
  `src/` (all cloned 2026-10-11). Mutter coverage is the plausible
  subset; a full-tree check stays open.

Portal wiring found while proving this (1.18.4 source):

- `.portal` files load from `$XDG_DESKTOP_PORTAL_DIR` or the
  compiled-in portals dir — not from `XDG_DATA_HOME`
  (`src/portal-impl.c` `load_installed_portals`).
- Camera/Device/Location export only when an Access backend is
  configured (`src/xdg-desktop-portal.c`); without one,
  `AccessCamera` fails with `UnknownMethod`, which looks like a
  denial but proves nothing.

## Tuna enforcement design

Settings leg (`crates/shell-host/src/settings.rs`): the
`SettingsBackend` trait gains boolean reads/writes (defaulting to
absent/refused), plus the `PrivacyPolicy` snapshot,
`refresh_privacy`, and per-key writers. Unknown or absent values
keep the snapshot, like every other setting here.

Portal leg (`crates/shell-host/src/lockdown.rs`, served by the
panel on its slow tick): Tuna publishes the standard Lockdown
interface itself at `/org/freedesktop/portal/desktop` under
`org.freedesktop.impl.portal.desktop.tuna`, synced from the same
GSettings keys, with `PropertiesChanged` on every move — the same
contract as GNOME's backend, so a session whose portal
configuration maps Lockdown here gets identical denial without
GNOME's backend installed. A fail-closed Access dialog
(response 1, cancelled) rides the same object so Camera exports;
a prompting UI stays with the desktop backend. Bus-originated
property sets persist back to GSettings on the next poll.

Graph leg (`crates/shell-gtk/src/privacy.rs`,
`audio_state.rs::privacy_scan`, wired into the `pw-dump --monitor`
loop in `services.rs`): on every graph event and every settings
change, the shell mutes what the policy requires and destroys
what must not exist:

- Microphone disabled: every `Audio/Source` reads muted. Capture
  streams keep running but capture silence; an unmute attempt is
  re-muted on the next event. Nothing is destroyed.
- Sound output disabled: every `Audio/Sink` reads muted, same
  re-mute shape.
- Camera disabled: `Video/Source` nodes and video-capture streams
  linked to them are destroyed, existing and newly appearing. An
  unlinked capture stream captures nothing and is left alone, and
  a stream wired to anything but a camera (a screen-share feed)
  is never touched.

Restore is explicit: flipping a key back unmutes exactly the
nodes this shell muted (a node the user muted by hand stays
muted), and video nodes created after restore survive.

Deliberate deltas from GNOME: Tuna kills existing camera nodes
where GNOME denies new portal sessions only, and Tuna mutes
microphone/sound output where GNOME has no enforcer at all. The
Lockdown publication itself matches GNOME exactly.

## Proof matrix

Committed tests (all in-repo, `cargo test -p tuna-shell-host
--lib`, `cargo test -p tuna-shell-gtk --bin tuna-shell-gtk`):

- `settings::tests::privacy_*`: policy defaults, refresh
  keep-on-unknown, write round-trips, string-only backend
  defaults, live `GioBackend` boolean reads (skips without the
  installed schema; read-only).
- `lockdown::tests::*`: poll sync/write-back/refusal-retry;
  private-bus stub portal proving hyphenated wire names,
  introspection, `PropertiesChanged` content, fail-closed
  `AccessDialog`, and name contention; and
  `real_portal_denies_camera_while_disabled_and_readmits_on_restore`,
  which drives the real `xdg-desktop-portal` daemon: allowed
  (past the gate), disabled (exact `NOT_ALLOWED` denial), restored
  (past the gate again). Skips without the daemon or `dbus-daemon`.
- `audio_state::tests::privacy_scan_*`: JSON fixtures for
  mute-required sets, camera/linked-stream destroy,
  screen-share/unlinked exclusion, and unknown-mixer distrust.
- `privacy::tests::plan_*`: tracker mute/remute/restore/forget
  semantics and command lines.
- `privacy::tests::live_pw::*`: real headless PipeWire +
  WirePlumber + GStreamer app streams. The audio leg proves
  existing streams are silenced (not destroyed), a direct unmute
  is re-muted, a late-opened stream is silenced, and restore
  unmutes. The camera leg proves an existing fake-`Video/Source`
  node is destroyed, a late-opened one is destroyed, and a
  post-restore one survives. Skips without the daemons/tools.

Unmeasured (hardware-absent, not failures): a real V4L2/libcamera
device node under a provider that respawns destroyed nodes; the
destroy path is proven against real graph nodes, and the portal
denial needs no camera at all (the gate fires before any hardware
lookup).

## Out-of-scope classification (this slice implements none of these)

- Capture/recording (webcam overlay, mic recording): v2/proposed
  extension; this lane denies, never captures.
- Privacy screen, USB protection: follow-up slice (hardware-gated).
- Retention (`old-files-age`, `remove-old-temp-files`,
  `remove-old-trash-files`): external-service responsibility
  (GNOME: settings-daemon housekeeping); no desktop consumer.
- Diagnostics/reporting (`report-technical-problems`,
  `send-software-usage-stats`): external-service
  responsibilities; no desktop consumer.
- Notification preferences: #348, other owner.
- `hide-identity`, `remember-app-usage`,
  `show-full-name-in-top-bar`, `usb-protection-level`: unchanged
  `Ignored` rows, no consumer claimed.

## Open items

- Packaging (tunaos-packages/tunaos, other repos): ship a
  `.portal`/`portals.conf` mapping Lockdown (and only Lockdown)
  at `org.freedesktop.impl.portal.desktop.tuna` for sessions
  without GNOME's backend. Until then the publisher is correct
  but unreferenced in production.
- A prompting Access UI (production uses the desktop backend's).
- Mutter full-tree consumer check for the two audio keys.
- Installed-image qualification on hardware with a real camera.
