# Wellbeing compositor effects (#354)

Protocol 0.35 appends one authenticated command:
`SetWellbeingEffects { grayscale: bool, fade: bool }`. Both values replace the
previous pair atomically. A repeated pair does not restart an animation;
changing one effect does not change the other effect's entry duration. The
shell owns screen-time/break policy and notifications; the compositor owns
pixels. This patch does not qualify those shell policy consumers.

## Reference and rendering

GNOME Shell 51.0 [timeLimitsManager.js](https://raw.githubusercontent.com/GNOME/gnome-shell/51.0/js/misc/timeLimitsManager.js)
lines 1104/1125/1143–1153 applies desaturation to uiGroup, enters over three
seconds with ease-out quadratic, and disables the effect immediately on exit.
[breakManager.js](https://raw.githubusercontent.com/GNOME/gnome-shell/51.0/js/misc/breakManager.js)
lines 691/738/768/805–823 applies brightness to the same group, entering toward
0.6 over three seconds and disabling immediately on exit. GNOME's Cogl color
property is byte quantized; Tuna keeps the requested 0.6 multiplier as float.

Clutter's [desaturation shader](https://raw.githubusercontent.com/GNOME/mutter/51.0/clutter/clutter/clutter-desaturate-effect.c)
uses 0.299/0.587/0.114 luminance weights. Its
[brightness shader](https://raw.githubusercontent.com/GNOME/mutter/51.0/clutter/clutter/clutter-brightness-contrast-effect.c)
preserves premultiplied alpha. Both transforms are linear on RGB and distribute
over source-over blending, so Tuna applies the same transform to every source
rather than allocating another full-output framebuffer. Scene texture groups
include wallpaper, shell/window surfaces, previews, and retained snapshots.
CPU colors include clear/decor, tile/focus solids, visual bell and screenshot
flash. The black blank shield is unchanged. Modal brightness remains per-window
and composes with the wellbeing multiplier. Cursor images, client cursor trees
and locate ripples are drawn afterward and receive no wellbeing transform.

Output temperature follows the wellbeing transform. Early temperature tinting
of accent/focus colors is removed to keep that ordering. All scene solids now
follow output temperature too, including white feedback. Captures apply the
wellbeing transform with unity temperature, retaining the existing output
versus capture temperature boundary. Raw retained window buffers stay raw and
are transformed when shown; screenshots, output/window casts and screenshot
selection captures use the current wellbeing sample.

GNOME's [effect easing](https://raw.githubusercontent.com/GNOME/gnome-shell/51.0/js/ui/environment.js)
calls [adjustAnimationTime](https://raw.githubusercontent.com/GNOME/gnome-shell/51.0/js/misc/animationUtils.js):
enable-animations off gives zero duration, otherwise the slowdown factor
adjusts it. These color effects retain their fade under Tuna's fade-only
motion policy. Switching animations off settles existing entry transitions;
turning animations back on does not replay them. Transition factors join native
repaint signatures; animation demand ends at the target. Shader compilation
failure is logged and leaves effects unapplied, so it cannot count as a pass.

## Actual pixel proof interface

`scripts/lib/wellbeing-pixels.py BASELINE.png CURRENT.png --regions regions.json
--desaturation FACTOR --brightness MULTIPLIER --receipt result.json` requires
frozen neutral/affected framebuffer images and explicit chromatic texture,
solid and unchanged cursor regions. A region is
`{"name":"window", "kind":"texture", "rect":[x,y,width,height]}`; `kind` may
also be `solid` or `cursor`. Every pixel must match the transform within two
byte levels; gray fixtures, missing controls, effect leaks onto the cursor,
and solid passes left unchanged fail. Receipts record both PNG hashes.

The live harness must freeze source/clock/fixture geometry, disable output
Night Light, and capture entry 0, midpoint (desaturation 0.75 / brightness
0.7), endpoint (1.0 / 0.6), independent effects, immediate off, and animations-off
negative controls. Actual shell/window/wallpaper/solid regions are required;
casts and saved screenshots need their own receipts. Diagnostics can expose
transition state but do not establish visual acceptance.

Four focused Python checker tests pass. Rust transition/codec/color tests and
real compositor framebuffer acceptance await exact-head CI. Installed DRM,
multi-output pixels, hardware power/performance and full #354 acceptance remain
unqualified.
