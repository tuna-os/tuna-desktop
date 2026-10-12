# Cursor v1 integration qualification

The v1 cursor lane imports the existing Xcursor/client-surface/shape/locate-pointer implementation onto the combined audio, workspace and idle scheduler source. It preserves sound and modal preferences plus workspace policy and appends cursor preferences in private protocol 0.33. The compositor and shell must be built together. The idle scheduler retains active animation service while locate-pointer ripples are running.

No fresh native build or runtime result is claimed for this lane yet. The imported proof checks must run against frozen exact-source executables and include actual captured pixels, owned session isolation and cleanup. Existing state-file-only assertions are insufficient for the full capability.

The candidate preserves animation delays, hotspots and per-frame pixels for legacy Xcursor and GNOME Shell51/KDE scalable SVG themes. The configured locate key is resolved through the active XKB map, with tap/chord/click/lock cancellation. These retained production tests and actual pixel proof scripts require a fresh combined CI build on this candidate; old standalone results are not qualification of this port.

Theme lookup is limited to32 directories,64 inherited themes and16 inheritance levels, retaining both default fallback names. Index files are limited to64KiB and legacy cursor files to8MiB. Legacy TOC counts, chunk extents and aggregate pixels are validated before the upstream parser allocates. SVG metadata/frame files, dimensions and decoded pixels are bounded, and both external and embedded image references are disabled; SVGZ is rejected to bound decompression. Named pipes and nonregular theme files are rejected. Each decoded theme and built-in cache has an aggregate64MiB pixel limit and64 entries; renderer buffers add memory beyond those pixel counts. Extreme output scales saturate the raster dimension at4096. This is bounded rendering support, not a measured startup/CPU/memory win.

Primary source: [GNOME Shell51 st-cursor.c](https://github.com/GNOME/gnome-shell/blob/51.0/src/st/st-cursor.c), [default metadata](https://github.com/GNOME/gnome-shell/blob/51.0/data/cursor-theme/icons/Adwaita/cursors_scalable/default/metadata.json), [default SVG](https://github.com/GNOME/gnome-shell/blob/51.0/data/cursor-theme/icons/Adwaita/cursors_scalable/default/default.svg). SVG support is registered by Shell as the Mutter cursor extension; examining only Mutter’s default Xcursor backend misses it.

SVG and animated Xcursor behavior still needs the full build and actual native pixel proof. Multi-output/fractional scaling and physical hardware need separate qualification. The prior implementation and linked proof names are source candidates rather than release evidence.


## Bundled GNOME51 assets

The private fallback theme under `/usr/share/tuna-desktop/icons/Adwaita`
contains the unmodified GNOME Shell51.0 cursor assets from upstream revision
`2177bdf9624b2d285de7c1d34274073d3769d6b8`: 151 SVG frames, 35 shape
metadata files, and two directory aliases. Embedded MIT attribution to Jakub
Steiner is preserved, with upstream repository licensing and per-file hashes.
User and system theme lookup remains ahead of this fallback, including legacy aliases; SVG wins within a theme, not across an earlier selected legacy theme. Corrupt legacy files continue to the next copy or inherited theme.

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
Animated fixture assertions additionally capture a second real frame without pointer input for legacy and SVG themes. All assertions await a fresh native CI journey on this port; fractional-output pixels, multiple physical outputs and hardware measurements remain separate qualification.


## Locked privacy and locate cache bounds

Lock transitions cancel in-flight locate effects and clear activity counters/key arming without changing theme or locate preferences. Locked diagnostics publish a null cursor record. The native journey freezes the owned animation clock while a real Ctrl ripple renders, requests the public D-Bus lock, and checks redaction plus authenticated-unlock nonresurrection. This fresh native regression awaits combined CI.

Locate disc buffers are limited to eight color/scale entries and16MiB of owned pixel storage across the three buffers per entry. Inserting a miss above either limit evicts prior entries; hits reuse their buffers. Raster and rendered side lengths saturate at1024 physical pixels, and nonfinite/nonpositive scales are rejected. This bounds the compositor's cache, not driver texture retention or overall GPU memory.
