# Maintained documentation media refresh

The fixed inventory in `scripts/lib/maintained-media.tsv` owns 46 walkthrough
PNGs, six README native-VM hero PNGs, and the README overview MP4/GIF.
It is not a directory-wide adoption rule. Dated diagnostic captures, frozen
reference baselines, the historical `docs/walkthrough/manifest.json`, and
already published versioned release assets are never overwritten.

## Automated producers and normal intake

The ordinary CI run produces:

- `gtk-shell`: the real full nested GTK journey, all 46 raw screenshots,
  original launched-ELF/source identities and successful capture outcomes.
- `marlin-vm` plus `tuna-tour`: the original five-boot native lifecycle,
  six confirmed hero stills and the actual tour video. Receipts bind the
  immutable image, original package, verified base index/platform, original
  nonroot GDM session, framebuffer timing and every retained media byte.
- `overview-demo`: a private Xvfb/private-D-Bus session with the actual
  Alpha/Beta/Gamma libadwaita windows. Physical input opens the overview,
  selects Beta, reopens it and dismisses it with Escape. The 12-second H.264
  recording is retained before deriving its 640×400, 8-fps GIF.

These are different scopes: nested fixtures never qualify an installed
native image, and DEV native media never qualifies a signed shipping release.
Title cards in the tour are not screenshots of the product.

`maintained-media.yml` accepts only a successful `ci.yml` main-push run from
the trusted repository. It rechecks workflow identity, run and attempt,
source, artifact upload digests, decoded media, UI outcomes and immutable
image/package/base evidence. ZIP paths, aliases, symlinks and nonregular
members are rejected before only fixed media/evidence members are extracted.
No artifact executable is extracted or run.

The result enters an ordinary media-refresh PR and the existing full CI,
never a direct-main commit or a gate/publication bypass. Newer main source
makes an older promotion stale instead of allowing it to overwrite current
media. A subsequent successful main CI is still required before claiming
current merged qualification. Generated-media-only evolution does not create
an endless refresh-commit loop and never manufactures a latest source SHA:
the actual captured commit, run, image and package remain recorded.

## Receipts and explicit freshness

Canonical receipts live separately under
`docs/walkthrough/media-manifests/{gtk-walkthrough,vm-tour,overview-demo}.json`.
They retain successful producer outcomes, actual source/run/attempt,
original evidence hashes and exact output hashes. The VM receipt additionally
retains verified OCI index/platform evidence and video hash/inspection.
The original tour video remains in the exact-run artifact, not an invented
checked-in video destination.

Required-source identity hashes actual clean Git mode/type/blob/path inputs,
excluding only the 54 registered generated media paths and three generated
receipt files. Every other tracked source input remains significant. This
does not permit broad documentation drift or silently relabel old footage.

For an independently verified exact run, use the same explicit identity pins
as CI:

```sh
INPUTS=$(python3 scripts/lib/maintained_media.py source-inputs)
scripts/tuna-docs-shots intake GTK_ROOT VM_ROOT OVERVIEW_ROOT \
  --bundle gtk-walkthrough --bundle vm-tour --bundle overview-demo \
  --run-id "$RUN" --attempt "$ATTEMPT" \
  --expect-merge "$BUILT_HEAD" --expect-head "$BUILT_HEAD" --expect-tree "$BUILT_TREE" \
  --expect-source-inputs "$INPUTS" --expect-image "$IMAGE_ID" \
  --expect-package "$PACKAGE_SHA256" --dest STAGING_REPOSITORY_ROOT
scripts/tuna-docs-shots freshness \
  --bundle gtk-walkthrough --bundle vm-tour --bundle overview-demo \
  --run-id "$RUN" --attempt "$ATTEMPT" \
  --expect-merge "$BUILT_HEAD" --expect-head "$BUILT_HEAD" --expect-tree "$BUILT_TREE" \
  --expect-source-inputs "$INPUTS" --expect-image "$IMAGE_ID" \
  --expect-package "$PACKAGE_SHA256" --dest STAGING_REPOSITORY_ROOT
```

Read the pins from independently admitted workflow/build/image evidence;
do not invent them or infer trusted current identity from a receipt alone.
`freshness --source-artifact ROOT` also revalidates original retained media,
including the VM video. Without that flag, canonical hero freshness does not
claim that the source video is locally present.

The old visual-stage refresh and seven-frame `docs-shots` collector remain
diagnostic/staging producers only. They are not alternate canonical-copy
paths, and there is no maintained `docs/shots` destination. Historical visual
manifest run `38293537511` remains historical evidence, not a current-source
or installed-image qualification.
