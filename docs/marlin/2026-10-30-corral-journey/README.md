# Published Marlin Tuna flavor: daily-driver journey through Corral, 2026-10-30

`scripts/tuna-corral-journey` ([how it works](../../corral-journey.md)) ran this journey on the
published image:

```text
ghcr.io/tuna-os/marlin:tuna
  = ghcr.io/tuna-os/marlin@sha256:ae6363c185015849701328fc38694f5ddcc0b4c804034a1d8d05577f1c70cf7d   (OCI index)
    amd64 manifest sha256:a182a20b732b6f8954be4afd7c0b47fa67306143100b5377d0b015a873415beb
    org.opencontainers.image.version cbb3569 (tunaOS), created 2026-10-29T21:46:44Z
```

`bootc status` in the guest reported the same index reference and amd64
manifest. The guest carried `tuna-desktop 1.0.0beta.1-1`, GDM 51.1.1,
gnome-shell 51.1 (installed, not running for the user), GNOME Settings
51.1, Nautilus 51.1, libnotify 0.8.9 and kernel 7.2.11-arch1-1.

The VM was `tuna-q-20261030`, a KubeVirt guest on node `karnataka` (k3s),
built with `corral bootc create` from Corral 76b6dd5, installed with
Corral's `scripts/install.sh`. It had 4 vCPUs, 6 GiB RAM, a 40 GiB disk,
q35 and UEFI without Secure Boot, and the USB tablet that `corral bootc
create` now adds. The display was bochs-drm at 1280x800 with no GPU.

Only Corral commands touched the VM; the script used no `kubectl` and no
`virtctl`:

- `corral bootc create --user tuna-test --password-stdin` created the
  account (fixture password `tuna`) in the installed system. The image
  itself is not changed. GDM listed the account with no other set-up.
- `corral wait --boot-complete --unit gdm.service --exec <greeter on
  seat0> --paint` reported ready after 165 s (`ready` in
  `manifest.json`).
- `corral info --json` gave the VM facts (`vm` in `manifest.json`).
- `corral console` served the VM's console page on a free port.
  Playwright Chromium drove that page at 1:1 scale, so every frame is
  the canvas's own pixels. Frames 01, 03 and 22 were also captured with
  `corral screenshot` and were pixel-identical.
- `corral delete --wait` removed the VM, its VMI and its disk, and
  `corral list` then showed only the pre-existing `punjab`.

The run went from 08:45 to 08:57 UTC. GDM offered only `Tuna Desktop` and
has `FallbackSession=tuna`.

`manifest.json` holds each step's timestamps, the guest-side checks and
their observed values, the `tuna-*` journal lines the step caused, and
frame hashes. The Review column below is the visual review of each frame,
made after the run.

| # | Step | Script | Review of the frame |
|---|---|---|---|
| 01 | GDM user list | pass | `tuna-test` listed, "Not listed?" below it. TunaOS greeter with the Arch logo |
| 02 | Select user | pass | The password prompt opens. No session chooser (one session) |
| 03 | Password → session | pass | Lands in the Tuna overview. logind `gdm-password` Wayland session; `tuna-compositor`, `tuna-shell-gtk` and `tuna-ibus-bridge` run; no `gnome-shell`; `XDG_SESSION_DESKTOP=tuna` |
| 04 | Escape | pass | Desktop with wallpaper and black top bar |
| 05 | Super | pass | Overview with search entry and dash |
| 06 | Super+A | pass | App grid: Settings, Bazaar, Firefox, Hardware Locality lstopo, Ptyxis, Qt V4L2 test/video utilities |
| 07 | Type "files" | pass | Files as the first result, a Settings "Privacy & Security" row below |
| 08 | Enter | pass | Files window opens (nautilus running) |
| 09 | Search "settings", Enter | pass | GNOME Settings opens on Network |
| 10 | Alt+Tab held | pass | Switcher with Settings and Files |
| 11 | Alt released | pass | Files raised over Settings |
| 12 | Alt+Tab tap | pass | Settings back in front |
| 13 | Settings > Appearance | pass | Appearance panel, Style Default selected |
| 14 | Style: Dark | pass | `color-scheme` = `prefer-dark`; Settings and the shell turn dark |
| 15 | Background tile | pass | `picture-uri` changed; desktop wallpaper changes to the selected tile |
| 16 | Super+S | pass, **bug** | Quick settings open. **The Dark Style tile shows off while `color-scheme` is `prefer-dark`** ([#844](https://github.com/tuna-os/tuna-desktop/issues/844)) |
| 17 | Night Light tile | **fail** | `night-light-enabled` becomes true, but nothing serves `org.gnome.SettingsDaemon.Color`, so Night Light never applies. `1.0.0beta.1` predates the compositor's Night Light; tracked in [#339](https://github.com/tuna-os/tuna-desktop/issues/339) |
| 18 | Volume/brightness sliders | not exercised | The VM has no sound card (no PipeWire sinks) and no backlight; neither slider is offered |
| 19 | `notify-send` | **fail** | `Unknown method 'GetServerInformation'`; no banner ([#843](https://github.com/tuna-os/tuna-desktop/issues/843)) |
| 20 | D-Bus `Notify` | pass | Banner "Corral journey / Tuna qualification" under the top bar |
| 21 | Super+V | pass, **bug** | Date menu with calendar, the notification listed. **The banner stays drawn over the open menu** ([#845](https://github.com/tuna-os/tuna-desktop/issues/845)) |
| 22 | Super+L | **fail** | Lock curtain with clock. `ScreenSaver.GetActive` true, but **logind `LockedHint=no`** ([#846](https://github.com/tuna-os/tuna-desktop/issues/846)) |
| 23 | Wrong password | pass | "Sorry, that didn't work. Please try again."; still locked |
| 24 | Right password | pass | Unlocked back to the session, `GetActive` false |
| 25 | Power button | pass, **bug** | Power menu (Suspend, Restart…, Power Off…, Log Out…) opens, **but the whole popover is translucent** ([#847](https://github.com/tuna-os/tuna-desktop/issues/847)) |
| 26 | Log Out… | pass | Log Out dialog with 60 s countdown over the dimmed session |
| 27 | Log Out | pass | User session ends; GDM user list back on seat0 |

The three failures and three visual bugs are the same ones the
2026-10-29 run found on the earlier `8253a80` build; the image still
carries `tuna-desktop 1.0.0beta.1-1`.

## Not covered

This run used a software-rendered KubeVirt VM, so it says nothing about
GPUs, multiple outputs, hotplug, suspend, audio or backlight hardware.
logind reports `Desktop=` empty for the Tuna session, although the
session's environment has `XDG_SESSION_DESKTOP=tuna`; nothing here was
found that reads it.
