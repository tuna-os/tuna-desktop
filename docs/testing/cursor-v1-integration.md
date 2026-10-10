# Cursor v1 integration qualification

The v1 cursor lane imports the existing Xcursor/client-surface/shape/locate-pointer implementation onto the combined audio, workspace and idle scheduler source. It preserves sound and modal preferences plus workspace policy and appends cursor preferences in private protocol 0.32. The compositor and shell must be built together. The idle scheduler retains active animation service while locate-pointer ripples are running.

No fresh native build or runtime result is claimed for this lane yet. The imported proof checks must run against frozen exact-source executables and include actual captured pixels, owned session isolation and cleanup. Existing state-file-only assertions are insufficient for the full capability.

The new animated Xcursor candidate decodes all frames at the nearest nominal size, preserves individual delays/dimensions/hotspots, reuses renderer buffers, restarts on shape/theme change and reports the selected frame in the diagnostic snapshot. Animation participates in the idle scheduler. Focused standalone tests compiled the exact decoder/timing functions against the locked xcursor0.3.11 parser; these do not qualify the full compositor or actual rendered cursor animation.

Remaining implementation includes GNOME 51 SVG cursor themes and configurable locate-pointer-key; animated Xcursor behavior still needs the full build and actual native pixel proof. Multi-output/fractional scaling and physical hardware need separate qualification. The prior implementation and linked proof names are source candidates rather than release evidence.
