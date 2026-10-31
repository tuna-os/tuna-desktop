# Cursor v1 integration qualification

The v1 cursor lane imports the existing Xcursor/client-surface/shape/locate-pointer implementation onto the combined audio, workspace and idle scheduler source. It preserves sound and modal preferences plus workspace policy and retains cursor preferences alongside keyboard aids in combined private protocol 0.34 (the standalone cursor port used 0.33). The compositor and shell must be built together. The idle scheduler retains active animation service while locate-pointer ripples are running.

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

`G-LOCATE-RIPPLE-PIXELS` captures locate-pointer at 415 ms on the frozen
animation clock and after all rings end. GNOME Shell 51's
[ripples.js](https://github.com/GNOME/gnome-shell/blob/51.0/js/ui/ripples.js)
eases the first ring from scale 0.25 to 1.5 over 830 ms with `EASE_OUT_QUAD`.
At half duration its scale is `0.25 + (1.5 - 0.25) * 0.75 = 1.1875`;
the 54 px disc-plus-glow therefore has radius 32.0625 logical px
(40.078125 physical px at 125%). The pixel checker derives its expected
radius from that curve and the output scale, keeping the 3 px edge tolerance,
lightened-accent checks and no-residue assertion.

The previous 1.375x / 37.125 px expectation was an arithmetic error in the
proof, not the renderer: CI run 41147696888, GTK job 124385584993, captured
left/up ray extents of 33 px and right/down extents of 32 px at 100%.
Those archived pixels match the upstream curve. This diagnosis is not a
fresh runtime qualification of the corrected proof.


## Locked privacy and locate cache bounds

Lock transitions cancel in-flight locate effects and clear activity counters/key arming without changing theme or locate preferences. Locked diagnostics publish a null cursor record. The native journey freezes the owned animation clock while a real Ctrl ripple renders, requests the public D-Bus lock, and checks redaction plus authenticated-unlock nonresurrection. This fresh native regression awaits combined CI.

Locate disc buffers are limited to eight color/scale entries and16MiB of owned pixel storage across the three buffers per entry. Inserting a miss above either limit evicts prior entries; hits reuse their buffers. Raster and rendered side lengths saturate at1024 physical pixels, and nonfinite/nonpositive scales are rejected. This bounds the compositor's cache, not driver texture retention or overall GPU memory.

# Native cursor timing and damage followup

Visible client cursor surfaces join each output's drawn timing and presentation
roots. Frame-only commits with unchanged buffers still request real scanout;
callbacks are sent only after an output containing cursor geometry submits.
Cursor trees also participate in refresh-cycle barriers and commit timing.
Nested cursor callbacks follow successful host submission.

Cursor geometry outside an output is excluded from that output's signatures
and timing demand. Geometry crossing an edge belongs to both outputs. Physical
bounds use the pre-transform output size, including rotated outputs. Pointer
position no longer separately invalidates the complete native drawing state;
cursor, drag, and hover elements retain their own geometry/effect signatures,
so buffer-age history can damage the old and new cursor rectangles.

G-CURSOR-FRAME uses a real Wayland client: an unselected cursor gets no callback,
then 16 frame-request-only commits progress without buffer reattachment,
damage, or fresh input. Native unit regressions bound cursor movement damage
and exercise edge/rotated-output intersection admission. These need exact-head
CI. The nested proof does not qualify installed DRM, hardware frame timing,
physical multi-output rendering, or total GPU/power performance.

Scheduler admission removes the client cursor from generic pending roots, then
adds it back only when unlocked, not fully blanked, selected rather than
overridden, and an individual mapped surface or subsurface intersects an
available output. The geometry walk mirrors renderer rounding and skips
unmapped parents. Refresh-cycle roots remain intact for real refreshes.
Negative unit controls cover hidden/overridden requests, off-output trees with
an empty gap across the display, and visible fractional-scale subsurfaces.
These are source/unit controls awaiting CI; they do not measure an installed
session idle loop or hardware power.
