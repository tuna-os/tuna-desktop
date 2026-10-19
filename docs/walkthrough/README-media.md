# README media provenance

## Marlin VM hero set

The six `hero-*.png` stills in the README are 1280×800 QEMU screendumps from the `marlin-vm` job of [CI run 38898189443](https://github.com/tuna-os/tuna-desktop/actions/runs/38898189443) on [PR #546](https://github.com/tuna-os/tuna-desktop/pull/546) head `604c2b4`. That job builds the TunaOS Marlin image with the Tuna Desktop package the same run built, boots it in a VM and starts the Tuna Desktop session on the DRM/KMS backend through the lane's CI-only greetd initial session. The windows are the GNOME apps the image ships (Files, Disks, System Monitor) on its default wallpaper; the message list is empty because the tour sends no notifications.

`scripts/tuna-vm-lane --tour` drives the session through real QMP input: Super+A for the app grid, panel clicks for quick settings and the calendar, Alt+Tab held down for the switcher, Super for the overview with the three apps, and Super+L for the lock screen. The switcher and window-overview stills are taken only once that view is detected on screen; the run's `capture-timing.json` lists no unconfirmed stills. `scripts/tuna-hero-media 38898189443` installed them from the run's `tuna-tour` artifact, recompressed losslessly with optipng. To refresh, run `scripts/tuna-hero-media <run-id>` against a newer green `marlin-vm` run and update this section.

## Overview demo and nested stills

Captured on 2026-10-04 at 1280×800. These are actual rendered Tuna Desktop frames and input-driven footage, not mockups. The overview demo uses three libadwaita fixture windows (Alpha, Beta, Gamma) from `scripts/lib/tuna-test-window.py`.

The overview PNG, H.264 MP4 and animated GIF were captured in a private D-Bus session and Xvfb display using the GTK compositor/shell binaries from [PR #240](https://github.com/tuna-os/tuna-desktop/pull/240), [CI run 37175952333](https://github.com/tuna-os/tuna-desktop/actions/runs/37175952333), checkout `49a70b0` (PR source `ad87517` merged with workspace-insertion source `bd18c76`). This candidate includes the workspace-insertion work in PR #234. Input opens the overview with Super, selects Beta's live preview, opens the overview again and dismisses it with Escape. Compositor state checks confirm three mapped windows, three overview previews and Beta focus after selection. The video runs for 12 seconds at 15 fps; the GIF is a smaller 640×400, 8 fps derivative. Neither speed establishes a performance benchmark.

The quick-settings and app-grid PNGs come from the successful GTK proof in [CI run 37173873772](https://github.com/tuna-os/tuna-desktop/actions/runs/37173873772), source revision `92987f2`. The service controls and catalog in this proof use fixtures on isolated buses. This run's GTK journey passed; its separate Marlin VM portal gate failed and is not asserted by these screenshots.

To reproduce the UI, run `scripts/tuna-gtk-shell-proof` on a development machine and retain `05-overview.png`, `03-quick-settings.png` and `11-app-grid.png`. For video, record the isolated Xvfb display with ffmpeg's x11grab while driving Super and preview selection through xdotool. Keep the complete frame at 1280×800, encode H.264 with yuv420p and faststart for the MP4, and retain the original capture before deriving the GIF.
