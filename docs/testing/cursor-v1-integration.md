# Cursor v1 integration qualification

The v1 cursor lane imports the existing Xcursor/client-surface/shape/locate-pointer implementation onto the combined audio, workspace and idle scheduler source. It preserves sound and modal preferences plus workspace policy and appends cursor preferences in private protocol 0.33. The compositor and shell must be built together. The idle scheduler retains active animation service while locate-pointer ripples are running.

No fresh native build or runtime result is claimed for this lane yet. The imported proof checks must run against frozen exact-source executables and include actual captured pixels, owned session isolation and cleanup. Existing state-file-only assertions are insufficient for the full capability.

The new animated Xcursor candidate decodes all frames at the nearest nominal size, preserves individual delays/dimensions/hotspots, reuses renderer buffers, restarts on shape/theme change and reports the selected frame in the diagnostic snapshot. Animation participates in the idle scheduler. Focused standalone tests compiled the exact decoder/timing functions against the locked xcursor0.3.11 parser; these do not qualify the full compositor or actual rendered cursor animation.

The configurable `org.gnome.mutter locate-pointer-key` candidate resolves keysym names against the active keyboard map, including remapped physical keys; invalid/empty/NUL names disable the binding safely. Tap arming remembers the physical press identity, preserves repeated presses, and cancels on chords, clicks, lock or preference changes. Four focused tests compiled the production tap/key-resolution methods against the cached xkbcommon crate and passed, including the real system `us` keymap with `ctrl:swapcaps`. This standalone harness substitutes the renderer/input container types and does not qualify Runtime integration or actual rendered ripples. New schema round-trip/bounds tests and full-workspace qualification are pending.

The SVG candidate uses GNOME Shell51/KDE `cursors_scalable/<shape>/metadata.json`, per-frame SVG filenames, nominal sizes, floating hotspots and delays. It rasterizes vectors at the actual fractional physical target rather than scaling a legacy bitmap and prefers valid SVGs over Xcursor fallback. Parsing/files/frame counts/dimensions/total decoded pixels are bounded; external file image references are disabled. Five exact-module standalone regressions pass, plus a sixth test loading the official GNOME51 Adwaita default at24,30,48 physical nominal sizes (32,40,64 image dimensions), preserving8x7 logical hotspots. The production SVG module is compiled directly with cached locked dependencies; the containing compositor/GTK integration and real native rendered proof are still pending.

Primary source: [GNOME Shell51 st-cursor.c](https://github.com/GNOME/gnome-shell/blob/51.0/src/st/st-cursor.c), [default metadata](https://github.com/GNOME/gnome-shell/blob/51.0/data/cursor-theme/icons/Adwaita/cursors_scalable/default/metadata.json), [default SVG](https://github.com/GNOME/gnome-shell/blob/51.0/data/cursor-theme/icons/Adwaita/cursors_scalable/default/default.svg). SVG support is registered by Shell as the Mutter cursor extension; examining only Mutter’s default Xcursor backend misses it.

SVG and animated Xcursor behavior still needs the full build and actual native pixel proof. Multi-output/fractional scaling and physical hardware need separate qualification. The prior implementation and linked proof names are source candidates rather than release evidence.


## Bundled GNOME51 assets

The private fallback theme under `/usr/share/tuna-desktop/icons/Adwaita`
contains the unmodified GNOME Shell51.0 cursor assets from upstream revision
`2177bdf9624b2d285de7c1d34274073d3769d6b8`: 151 SVG frames, 35 shape
metadata files, and two directory aliases. Embedded MIT attribution to Jakub
Steiner is preserved, with upstream repository licensing and per-file hashes.
User and system theme lookup remains ahead of this fallback.

Both the Debian release script and Arch recipe stage this tree. CI verifies
its manifest and rejection regressions; the Debian package checker verifies
it again after archive extraction. These checks establish data integrity,
not native rendered pixels or installed-image parity. The package-factory
recipe must update its source archive and vendor pins to the final published
v1 source before an actual package can include this new tree.


## Native pixel assertions

The GTK runtime journey now captures the actual X host framebuffer for the
24px and48px legacy and scalable cursor cases, checking the complete colored
rectangle, its dimensions and its hotspot against the physical pointer.
A distinct SVG color proves it replaced the inherited legacy image; a client
cursor surface and requested crosshair are also checked. The checker retries
actual framebuffer captures for at most10 seconds to allow the presentation
acknowledgment, and writes PNG plus pixel receipt for each case. It rejects
cropped, displaced and incorrectly colored fixtures in four focused tests.
These assertions await a successful native CI journey; they do not cover
animated-frame pixels, fractional-output pixels or physical GPU measurements.
