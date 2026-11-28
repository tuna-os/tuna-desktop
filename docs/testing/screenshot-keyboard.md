# Screenshot selection keyboard controls

The selection overlay follows GNOME Shell 51.0 `screenshot.js`: arrows resize the selected edge, Alt moves the area, Ctrl adjusts by one logical pixel, Shift reaches the output edge, and R restores the initial area while preserving the selected edge. Both endpoints remain inside the current output, including after an output shrinks. Crossing an endpoint preserves the oriented selection.

Every key goes through one table that matches `ScreenshotUI`'s Clutter binding pool exactly: the keyval and the held Shift, Ctrl, Alt, Super and Hyper modifiers must equal a binding's (`clutter_binding_pool_activate` masks with `BINDING_MOD_MASK`, so Caps Lock and Num Lock do not count). Return, Space and Ctrl+C capture; S, C, W, V, P and R act only unmodified and lowercase, so Shift+C, Ctrl+S and KP_Enter do nothing (KP_Enter still activates a focused button, as St.Button does); arrows take no modifier, Ctrl, Shift, Alt, Alt+Ctrl or Alt+Shift, and other combinations (Ctrl+Shift, Super) are swallowed without effect. Escape closes with any modifiers, through the UI's grab. Meta is not compared because GTK may report it alongside Alt.

Keyboard adjustment relocates the pointer to the selected edge, or to the area centre during Alt movement. Coordinates use integer logical pixels, as Mutter 51.0 `ClutterSeat.warp_pointer`. The private authenticated shell control socket carries the feedback; it adds no public D-Bus API. The compositor rechecks the lock, the mapped exclusive screenshot layer and local bounds before applying it. Existing pointer constraints remain authoritative. Winit also updates the visible host cursor.

## Screen mode on several monitors

Screen mode follows `_rebuildMonitorBins` and `_moveFocus` in gnome-shell 51.0's `screenshot.js`. When the UI opens, the shell asks the compositor for every output's current picture (`org.tuna.Screenshot.ScreenshotOutputs`, `(directory) -> a(siiiidbs)`: name, logical rectangle, scale, primary, PNG path). Each picture is rendered at that output's own scale and shown frozen on its own overlay layer. The primary output carries the panel and holds the keyboard. The other outputs get keyboard-less overlays in the same `tuna-screenshot-ui` namespace. In Screen mode each overlay is a screen selector: the selected one has a 2px white frame, and the others are dimmed to 50% black (30% while hovered), as `.screenshot-ui-screen-selector` styles them. A click selects a monitor.

`ScreenshotOutputs` stands in for GNOME's in-process stage screenshot, so only the supervised shell may call it, and only while the session is unlocked. A portal call under `disable-save-to-disk` gets GNOME's "Saving to disk is disabled" error first; any other portal or caller is denied. The shell keeps working under that lockdown, clipboard-only, as GNOME's UI does. The pictures go only to `$XDG_RUNTIME_DIR/tuna-screenshot-outputs`. The path must match exactly by component, so another directory or `..` is refused. The directory is created mode 0700 and refused when it is a symlink, owned by another user, or open to others.

The primary monitor starts selected. The selection then holds across openings until the set or layout of monitors changes, because GNOME rebuilds its monitor bins only on `monitors-changed`. GNOME 51 does not start on the monitor under the pointer. Plain arrows move the selection as `st_widget_navigate_focus` does, with no wrap-around:

- the candidates are the monitors lying wholly past the selected one's edge in that direction (0.1px slack);
- the one whose integer midpoint is nearest wins;
- on a tie, the later monitor wins.

Modified arrows do nothing in Screen mode. Return, Space or Ctrl+C saves only the selected output, in its own physical pixels: a scale-2 monitor yields its full mode size, not its logical size. Escape closes every overlay without saving. Recording in Screen mode records the selected monitor. A recording from Selection mode records the area, offset by the primary monitor's position. The area selection and the window selector stay on the primary monitor.

`D-SCREENSHOT-KEYS-MULTI` in `scripts/tuna-drm-dual` (drm-smoke job; see [testing](../testing.md)) proves this on two vkms connectors: one at scale 1 and one at scale 2. The nested session has a single output, so the focused proof below covers only the one-monitor path.

## Focused runtime proof

```sh
scripts/tuna-gtk-shell-proof --screenshot-keys-only \
  --artifacts /tmp/tuna-screenshot-keys
```

The proof owns a mode-0700 runtime directory, session bus and accessibility bus. It exports the explicit accessibility address to the shell and queries. Each fixture sends real keys through the exclusive GTK overlay, checks the accessible selected area, compositor pointer state and visible host cursor, saves a real PNG, and checks its dimensions. It also checks modified navigation in Screen mode leaves the pointer and workspace unchanged. Source/binary hashes and session identities are retained with the observations. Supplied source binaries can be preserved with `TUNA_PROOF_BIN_DIR`; record their actual build commit with `TUNA_PROOF_BINARY_SOURCE_COMMIT`. Installed-package mode rejects this override.

These fixtures are local nested GTK evidence. They do not qualify a Marlin installed image, native DRM cursor rendering, multiple outputs/scales, a paired GNOME capture, hardware performance or power usage. The complete merged source must still pass the normal CI GTK journey.

## GTK accessibility lifetime

Local GTK 4.14.5, gtk4-layer-shell 1.1.1 and libadwaita 1.5.0 exposed a real screenshot reopen/capture crash. Attached GDB stopped at SIGSEGV in `g_variant_builder_add_value`, reached through GTK's AT-SPI root method while encoding an object reference. The retained diagnostic is `/tmp/tuna-v1-screenshot-attach-backtrace.txt`, SHA-256 `535a2c9795a6307dd61bc7beeeae96a34ce64e3b054cb4e10d9a1816e7356f41`.

[GTK 4.14.5 `gtkatspiroot.c`](https://github.com/GNOME/gtk/blob/4.14.5/gtk/a11y/gtkatspiroot.c) `GetChildAtIndex` can fall through to a hidden final toplevel when a cached index exceeds the new visible count, then encode its null/unrealized accessibility path. The same handling exists in [GTK 4.23.3](https://github.com/GNOME/gtk/blob/4.23.3/gtk/a11y/gtkatspiroot.c), the version declared by the registered RPM package source `src/gnome-51/gtk4/gtk4.spec` (release 3). Marlin's actual Arch GTK version must be recorded from the installed image; the RPM pin does not establish that image's dependency version.

The screenshot UI retires its GtkWindow on close and creates a fresh layer window on reopen, preserving the overlay content and key controller. Compositor close requests use the same retirement path. Direct native destruction retires the slot on unrealize; GTK's final object destroy signal occurs too late while the UI still holds a reference. This removes the retired screenshot toplevel from GTK's accessibility root rather than leaving an unrealized hidden window in its list. The proof also waits for actual layer map/unmap before walking the tree. This application lifecycle correction does not fix GTK's general stale-index behaviour for unrelated windows; unrestricted screen-reader queries during every possible window transition remain a dependency qualification limitation.

## Local evidence, 2026-10-10

Workspace packages were rebuilt from source under the build/freeze guard before preserving the binaries, with third-party dependency artifacts retained. The final compiled implementation is `f73a9272186ec9074ad1163298121fa35d914368`.

- `/tmp/tuna-v1-screenshot-proof-surface`: `G-SCREENSHOT-KEYS` and `G-SCREENSHOT-KEYS-SCREEN` passed. Six real PNGs, overlay screenshots, accessible observations, compositor state and visible host cursor coordinates are retained. The native shell completed with zero restarts. `screenshot-keys-source.json` and `session-isolation.json` record binary hashes and separate build/session identities.
- `/tmp/tuna-v1-screenshot-lifecycle-proof-surface`: the ignored `external_close_and_destroy_retire_the_screenshot_window` test ran as the authenticated supervised GTK shell and passed. It maps three actual layer windows and proves a native close request, direct destruction and fresh reopening. `manifest.json` records GTK 4.14.5, the exact build commit, test/compositor hashes and private bus/runtime identities. The supervisor may relaunch this deliberately exiting test process; that is not a shell crash.
- The same final GTK test binary passed all 120 default tests, with the one Wayland lifecycle test ignored by default. Earlier focused checks passed schema 49, protocol integration 29, host wire 20 and compositor Unix-socket 14 tests; installed-package provenance tests passed 38, including denial of supplied source binaries in installed mode.

These local results retain the qualification limitations above. In particular, the application lifecycle fix does not establish that every arbitrary screen-reader query during unrelated GTK window transitions is safe.
