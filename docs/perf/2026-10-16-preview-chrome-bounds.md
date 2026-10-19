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
frame pacing, input geometry or acceptance gate is changed here. The bounded
resampling residual and owner visual decision are disclosed below.

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

## Controlled local result

Measured production source: `c44579cfcdb97aa01660694424b25027159e03cf`.
Baseline: `dade6da21ff038d2325cbd4ed725a3e0d2306296`, whose parents are
exact #696 `2aafe078` and main `f3d794cb` (#629). Only the PreviewChrome
production file differs. Both arms used the identical compositor executable,
SHA256 `73c301836cf47868a2caeccd5d01688c830132871a0d84dc095f5dbaa1382565`.
Baseline/candidate shell SHA256 values:
`77b1c6e51a0dd806babd9a980916290a0814c4fc65dea6b2ee73a599c103b274` /
`0d46038bca14008f33bd41be27f4d05ac49af066bb44f9ca4c5ffdc4f13e91fc`.

The coordinator reserved the idle two-core host; all other owners held local
heavy work. Serial `--release --locked -j1` builds used
`CARGO_PROFILE_RELEASE_DEBUG=line-tables-only` and
`RUSTFLAGS='-C force-frame-pointers=yes'`. GTK 4.14.5, gtk4-layer-shell 1.1.1,
cairo, software GL, vkms DRM `Virtual-1` 1024x768, kernel `7.0.0-1013-aws`.
The unchanged previous phase fixture launched Disks, System Monitor's Resources
tab and Files, sent 20 Super toggles 1.7s apart and ten notifications, then
settled for 30s. `TUNA_PERF_TRACE=1`; no compositor-state file during measurement.
`CLK_TCK=100`, so process CPU ticks/wall seconds are percent of one core.
The alternating order was A1/B1/A2/B2; brackets below are shell CPU.

| Phase | Baseline A1 / A2 | Candidate B1 / B2 |
|---|---:|---:|
| Launches, 24.0s | 9.1 (0.4) / 9.0 (0.4) | 8.4 (0.4) / 9.2 (0.4) |
| Overview, 34.1s | **31.0 (2.7) / 32.7 (2.8)** | **29.2 (2.7) / 30.6 (2.8)** |
| Notifications, 13.2–13.4s | 17.9 (5.6) / 21.3 (8.0) | 21.8 (7.4) / 21.6 (7.6) |
| Final settle, 30.0–30.1s | 10.4 (0.6) / 10.7 (0.5) | 10.8 (0.5) / 10.9 (0.4) |

Overview mean compositor CPU fell **31.85% to 29.90%: −1.95 points / −6.1%**.
The individual paired changes are −1.8 and −2.1 points; shell CPU is unchanged.
This is an overview-only local result, not whole-trace or GNOME parity.
The table retains the other phases' variation; there is no improvement claim
for notifications or final settle, and two repetitions are not a statistical
non-regression result for those phases.

Raw logs and `phases.txt` files are retained at
`/tmp/tuna-cpu-next-lever/cpu-{A1,B1,A2,B2}/`. The reused fixture is
`.worktrees/cpp-scratch/run.sh`, SHA256
`0d35bbd61f328d46b74469d460380f122ffa8ed4e964bf504e645865cbcbe2d5`;
its uinput helper SHA256 is
`d4d975240cac5675c41a444d5cd00bd0428eb5f504dfef2f7d911d31f11f1cd6`.
The fixture and earlier shader scratch were not modified.

## Actual UI, buffer bounds and owner visual decision

Four bounds/fade tests and the release shell build passed. Actual native GTK
fixtures waited for their first painted frame, not just map. At both 1x and
1.25x, actual resize 1024x768 → 960x720 → restore, icons/shadows,
caption/close alignment, shrink after unhover, close delivered to Beta only,
click-through selecting Alpha, and hide/reopen with the two remaining windows
all passed. The first rejected candidate had read allocation-derived
`GtkFixed.child_position()` before allocation; the final implementation reads
the already-stored layout transform. A second rejected candidate shifted
icons at fractional scale; the 120-unit origin grid removes that shift.

Observed final layer bounds, in logical pixels:

| Scale | Baseline full-output area | Candidate no hover | Candidate hovered |
|---|---:|---:|---:|
| 1x | 786,432 | 220,759 (**−71.9%**) | 461,564 (**−41.3%**) |
| 1.25x | 502,866 | 208,164 (**−58.6%**) | 342,706 (**−31.8%**) |

These are actual layer-bound areas, not a direct total shaded-fragment
counter. The final padded origin grid, not the rejected tighter bounds, is
used in every CPU result above.

**Strict bitwise crop comparison fails:** 132/170,858 pixels at 1x differ by
at most one channel unit; 739/261,112 at 1.25x differ by at most two.
Caption and close crops have zero differences at both scales. An independent
1x baseline-only control also has one-unit preview-edge variance. The
fractional residual remains real; it is not dismissed as baseline noise.
No tolerance or existing gate was changed, and this report does not claim
bitwise identity.

The coordinator, exercising delegated owner approval, personally inspected
all six labeled baseline/candidate/difference-times-64 images plus the
channel-count JSON. The owner **accepted the consumer-visible visual behavior
for beta**, observing no visible geometry, caption, close-control or
icon/shadow regression, conditional on exact-source CI and remote paired
qualification. The real bounded resampling residual remains disclosed.

Raw final frames, states and input receipts are under
`/tmp/tuna-cpu-next-lever/ui-baseline-final-v2{,-125}/` and
`/tmp/tuna-cpu-next-lever/ui-candidate-grid-final{,-125}/`.
`/tmp/tuna-cpu-next-lever/visual-review/summary.json` records every compared
region, per-channel changed-pixel counts/maxima and six labeled review PNGs.
The earlier failed candidate frames/receipts are retained separately.

## Remote qualification

See [PR #719](https://github.com/tuna-os/tuna-desktop/pull/719) for immutable
final-source CI/package and paired-run provenance. Old green CI on rejected
`2ef9e552` and canceled paired run `38964843746` are not final-source evidence.
The local overview reduction does not close the existing paired whole-trace,
notification, final-settle or idle CPU gaps.

