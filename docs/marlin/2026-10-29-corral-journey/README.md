# Published Marlin Tuna flavor: daily-driver journey through Corral, 2026-10-29

`scripts/tuna-corral-journey` ([how it works](../../corral-journey.md)) ran this journey on the
published image:

```text
ghcr.io/tuna-os/marlin:tuna
  = ghcr.io/tuna-os/marlin@sha256:6b9b3413f5f3b57d8c2a8164a9e4b401b1c3d5f28f69d0d934ef3d596a2adc12   (OCI index)
    amd64 manifest sha256:2f2e6b490c279b972ccd5359e940bada4f0d16d4cfc2685e33bd8093d8d40412
    org.opencontainers.image.version 8253a80 (tunaOS), created 2026-10-29T08:40:12Z
```

`bootc status` in the guest reported the same index reference and amd64
manifest. The guest carried `tuna-desktop 1.0.0beta.1-1`, GDM 51.1.1,
gnome-shell 51.1 (installed, not running for the user), GNOME Settings
51.1, Nautilus 51.1, libnotify 0.8.9 and kernel 7.2.11-arch1-1.

The VM was `tuna-q-20261029`, a KubeVirt guest on node `karnataka`
(Talos), built with `corral bootc create` (Corral 0dfd85a). It had 4 vCPUs,
6 GiB RAM, a 40 GiB local-path disk, q35 and UEFI without Secure Boot.
The display was bochs-drm at 1280x800 with no GPU, and a USB tablet pointer
was added by the run (see Corral below). The console was the `corral web`
noVNC console, driven by Playwright Chromium at 1:1 scale. Every frame is
the canvas's own pixels. Frames 01, 03 and 22 were also captured through
`virtctl vnc screenshot` and were pixel-identical. The run went from
16:35 to 16:45 UTC, and the script deleted the VM and its disk afterwards.
`corral list` then showed only the pre-existing `punjab`.

The `tuna-test` account (fixture password `tuna`) was added at runtime
over `corral ssh`. It got an AccountsService record that names no session.
GDM offered only `Tuna Desktop` and has `FallbackSession=tuna`.

`manifest.json` holds each step's timestamps, the guest-side checks and
their observed values, the `tuna-*` journal lines the step caused, and
frame hashes. The Review column below is the visual review of each frame,
made after the run.

| # | Step | Script | Review of the frame |
|---|---|---|---|
| 01 | GDM user list | pass | `tuna-test` listed, "Not listed?" below it. TunaOS greeter with the Arch logo |
| 02 | Select user | pass | Enter on the focused row opens the password prompt. No session chooser (one session) |
| 03 | Password → session | pass | Lands in the Tuna overview. logind `gdm-password` Wayland session; `tuna-compositor`, `tuna-shell-gtk` and `tuna-ibus-bridge` run; no `gnome-shell`; `XDG_SESSION_DESKTOP=tuna` |
| 04 | Escape | pass | Desktop with wallpaper and black top bar |
| 05 | Super | pass | Overview with search entry and dash. Compositor logged `overview focus parked` |
| 06 | Super+A | pass | App grid: Settings, Hardware Locality lstopo, Ptyxis, Qt V4L2 test/video utilities |
| 07 | Type "files" | pass | Files as the first result, a Settings "Privacy & Security" row below |
| 08 | Enter | pass | Files window opens (nautilus running) |
| 09 | Search "settings", Enter | pass | GNOME Settings opens on Network |
| 10 | Alt+Tab held | pass | Switcher with Settings and Files, Files selected. Compositor logged `switcher Step` |
| 11 | Alt released | pass | Files raised over Settings (`switcher Commit`) |
| 12 | Alt+Tab tap | pass | Settings back in front |
| 13 | Settings > Appearance | pass | Appearance panel, Style Default selected |
| 14 | Style: Dark | pass | `color-scheme` = `prefer-dark`; Settings and the shell turn dark |
| 15 | Background tile | pass | `picture-uri` changed; desktop wallpaper changes to the selected tile |
| 16 | Super+S | pass, **bug** | Quick settings open (Wired, Power Mode, Night Light, Dark Style, Do Not Disturb). **The Dark Style tile shows off while `color-scheme` is `prefer-dark`** ([#844](https://github.com/tuna-os/tuna-desktop/issues/844)) |
| 17 | Night Light tile | **fail** | Tile turns on and `night-light-enabled` becomes true, but nothing serves `org.gnome.SettingsDaemon.Color` and there is no gsd-color, so Night Light never applies. `1.0.0beta.1` predates the compositor's Night Light; tracked in [#339](https://github.com/tuna-os/tuna-desktop/issues/339) |
| 18 | Volume/brightness sliders | not exercised | The VM has no sound card (no PipeWire sinks) and no backlight; neither slider is offered |
| 19 | `notify-send` | **fail** | `Unknown method 'GetServerInformation'`; no banner ([#843](https://github.com/tuna-os/tuna-desktop/issues/843)) |
| 20 | D-Bus `Notify` | pass | Banner "Corral journey / Tuna qualification" under the top bar |
| 21 | Super+V | pass, **bug** | Date menu with calendar, the notification listed. **The banner stays drawn over the open menu** ([#845](https://github.com/tuna-os/tuna-desktop/issues/845)) |
| 22 | Super+L | **fail** | Lock curtain with clock. `ScreenSaver.GetActive` true, but **logind `LockedHint=no`** ([#846](https://github.com/tuna-os/tuna-desktop/issues/846)) |
| 23 | Wrong password | pass | "Sorry, that didn't work. Please try again."; still locked |
| 24 | Right password | pass | Unlocked back to the session, `GetActive` false |
| 25 | Power button | pass, **bug** | Power menu (Suspend, Restart…, Power Off…, Log Out…) opens, **but the whole popover is translucent**; the Wired menu does the same ([#847](https://github.com/tuna-os/tuna-desktop/issues/847)) |
| 26 | Log Out… | pass | Log Out dialog with 60 s countdown over the dimmed session |
| 27 | Log Out | pass | User session ends; GDM user list back on seat0 |

## Not covered

This run used a software-rendered KubeVirt VM, so it says nothing about
GPUs, multiple outputs, hotplug, suspend, audio or backlight hardware. Two
things were seen but not filed. logind reports `Desktop=` empty for the
Tuna session, although the session's environment has
`XDG_SESSION_DESKTOP=tuna`; nothing here was found that reads it. The app
grid shows the image's `qv4l2` and `lstopo` launchers, which is image
packaging, not Tuna Desktop.

## Corral

Two Corral gaps came up and are fixed in
[tuna-os/corral#659](https://github.com/tuna-os/corral/pull/659):

- `corral bootc create` gave the KubeVirt guest only a relative PS/2
  mouse, so noVNC clicks landed hundreds of pixels off. The run adds the
  USB tablet (`tablet_added_by_run: true`).
- `corral screenshot` refused KubeVirt VMs. With #659 it captures through
  KubeVirt's VNC screenshot. A binary built from that branch captured
  this image's session (1280x800, painted).
