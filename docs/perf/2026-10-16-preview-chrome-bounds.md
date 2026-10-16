# Bounded overview chrome, beta.2 CPU follow-up

This is a follow-up to #696, not a claim of CPU parity or final-v1 parity.
The #696 paired run [38927781532](https://github.com/tuna-os/tuna-desktop/actions/runs/38927781532)
reported whole-trace Tuna 28.3% versus GNOME 22.8% user CPU, a +5.5-point
median paired gap. Its overview, notification and final-settle gaps remain
open (+7.4, +17.5 and +10.9 points respectively).

## Evidence and selected lever

The existing per-frame attribution in
[the compositor CPU report](2026-10-15-cpu-parity-attribution.md#compositor-phases-profile-and-fixes-696)
finds 87% of overview shaded pixels in approximately ten full-output
transition frames. `PreviewChrome` contributed a full-output transparent GTK
buffer on every such frame, despite drawing only app icons and the hovered
preview's caption and close button.

The change bounds that same GTK layer to the union of its children's natural
sizes. It keeps the existing widgets, CSS, fractional output coordinates,
200ms hover fade, keyboard mode, exclusive-zone policy and 32px close input
region. A 12px bleed retains the CSS 4px-blur/2px-offset shadows. The layer is
anchored top/left before the union's origin, snapped down to a 120-logical-pixel
grid. Fractional-scale protocol values use 120ths, so this keeps the layer
origin on the physical pixel grid at every advertised fractional scale.
The bounded extra padding is less than 120 logical pixels per leading edge.
Both children and the input region are translated by that same integer
origin. Explicit natural-size requests allow the buffer to shrink after a
larger hovered layout. Empty chrome maps no surface. There is still one
surface, not one window per icon.

No shader fast path was selected: Smithay's current texture shader multiplies
sampled color by a runtime alpha, while the inspected Mesa llvmpipe linear
blit fast paths require classified RGBA/RGB1 blits and nearest/clamp sampling.
That source inspection alone does not establish an equivalent shader rewrite
or a measured advantage. No shader, compositor runtime, occlusion policy,
frame pacing, input geometry, visuals or acceptance gate is changed here.

## Measurement contract

Compare immutable baseline and candidate executables with the same compositor,
GTK renderer, display size and workload, using alternating runs on an idle
host. Record compositor and shell process CPU separately; do not equate the
local workload with the paired Marlin result. The actual UI proof must cover
icons and shadows, hovered caption/close alignment, close-button delivery and
click-through to the preview, plus hide/reopen and surface shrink. CI package
and paired-performance dispatches must name the exact source SHA.

The remaining CPU gap stays open unless the complete paired acceptance
criteria are met; reducing a transparent buffer alone cannot close the
notification or visible-System-Monitor final-settle gaps.
