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

## Live CI renderer proof

The existing native freeze build invocation must additionally include
`--example tuna-wellbeing-proof-shell`. Retain its original Cargo JSON output
and frozen bundle; the wellbeing proof admits the extra example from that
same receipt only after all original executable hashes, source commit and
Cargo.lock match. Original freeze receipts remain immutable. Run before live
accessibility creates artifacts inside the checkout:

```sh
/usr/bin/python3 scripts/tests/test_wellbeing_bundle.py
/usr/bin/python3 scripts/lib/wellbeing-pixels-tests.py
dbus-run-session -- /usr/bin/python3 scripts/tuna-wellbeing-proof \
  --bin-dir "$RUNNER_TEMP/native-minimize/frozen" \
  --build-receipt "$RUNNER_TEMP/native-minimize/build.json" \
  --artifacts "$RUNNER_TEMP/native-wellbeing" --display :111
```

This proof performs actual capture and pixel checks. Its supervised synthetic
shell owns the authenticated control connection and authorized Screenshot
D-Bus calls; the native C color client sets a real client cursor. Commands go
through protocol 0.35, not a compositor state-file override. Frozen animation
time drives independent and combined effect entry, midpoint, endpoint,
immediate disabled restoration, reduced motion and animations-off. Both host
`scrot` captures and saved Screenshot PNGs are checked; saved output has no
cursor region. The host cursor remains colored. Held real Super+drag creates
the translucent tile pass; real native windows on two dynamic workspaces expose the overview workspace
indicator solid and window texture preview. Required regions are admitted by
source geometry and checked against actual chromatic producer pixels.

Artifacts live outside the checkout in a fresh directory and include original
freeze/build-receipt hashes, exact compositor/proof-peer/client identities,
boot ID and process start ticks, per-image SHA receipts, passive frame admission
and cleanup status. The proof refuses existing displays, mismatched/stale/dirty
source, ambiguous/missing examples and overwritten bundles. Child processes
have parent-exit signals; an owned pidfd tracks the supervised peer. Source and
process identities are checked again before a pass. Interruptions enter owned
cleanup and cannot emit the acceptance receipt.

Seven admission/source tests and four pixel checker tests pass locally. The
new example and live proof still await exact-head CI; these tests use fake
artifact bytes and synthetic pixels and do not count as native acceptance.
Retain the reconciled boxed InputSettings API before combined CI. This proof
qualifies renderer behavior only when it runs successfully; GTK GSettings,
usage timers, notifications, screen-cast transport, installed DRM and hardware
acceptance remain separate.
