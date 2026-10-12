# Walkthrough refresh procedure

Rebuilds `docs/walkthrough/` images ONLY from exact, fully successful
visual-capture artifacts. Never refresh from a partial run, from mixed
attempts or built trees, from hash-unvalidated inputs, or from frames
the validators did not confirm as named-state proof; `refresh` refuses
all of those. Checked-in images stay STALE-listed until a refresh
lands: nothing labels them current automatically, and no refresh has
been performed from any run yet.

## Prerequisites

- A `visual-capture` CI run whose conclusion is `success` (build ok,
  all 14 capture jobs ok, manifest ok, zero partial stages). Note its
  run id. A green collector job with failed states inside is a partial
  stage: refresh refuses it by name.
- The run's recorded identities, read from any consumed stage's
  `provenance.json` (never assumed): `source.built_head_sha`,
  `source.built_tree_sha`, `source.event_sha`.

## Exact commands

```sh
RUN=<run id>        # e.g. 38246246572
BRANCH=<branch>     # e.g. w3-visual-capture

# 1. Confirm the run is fully green first (a red or partial run must
#    never feed a refresh).
gh run view "$RUN" --json conclusion --jq .conclusion
#    -> success

# 2. Download the exact artifacts of that run into one bundle dir.
rm -rf visual-exact
gh run download "$RUN" -R tuna-os/tuna-desktop --dir visual-exact

# 3. Read the recorded identities out of the bundle (do not hand-make).
HEAD=$(python3 -c "import json;print(json.load(open('visual-exact/visual-panel/provenance.json'))['source']['built_head_sha'])")
TREE=$(python3 -c "import json;print(json.load(open('visual-exact/visual-panel/provenance.json'))['source']['built_tree_sha'])")
EVENT=$(python3 -c "import json;print(json.load(open('visual-exact/visual-panel/provenance.json'))['source']['event_sha'])")

# 4. Refresh into a scratch dest first and inspect the report.
#    --merge-sha is validated against every consumed provenance and the
#    refresh fails closed on mismatch; omit it to take the recorded
#    event SHA (still cross-checked across stages).
scripts/tuna-walkthrough refresh visual-exact --run-id "$RUN" \
    --merge-sha "$EVENT" --branch "$BRANCH" --dest /tmp/walkthrough-new
scripts/tuna-walkthrough freshness --dest /tmp/walkthrough-new \
    --expect-head "$HEAD" --expect-tree "$TREE" --expect-merge "$EVENT"
#    -> freshness 46/46 current

# 5. Adopt the result into docs/ (images + page + per-frame manifest).
cp /tmp/walkthrough-new/walkthrough/*.png docs/walkthrough/
cp /tmp/walkthrough-new/walkthrough/manifest.json docs/walkthrough/
cp /tmp/walkthrough-new/walkthrough.md docs/walkthrough.md
scripts/tuna-walkthrough check        # existing existence gate, unchanged
scripts/tuna-walkthrough freshness    # expects default to this checkout
```

## What refresh pins and validates

Per run, `docs/walkthrough/manifest.json` records: the pinned run id
and attempt (all consumed stages must share both), the built head and
tree SHAs (agreed across stages; the tree is what was actually built
and captured), the event SHA (the triggering event's commit, e.g. the
PR merge ref; validated, never presented as the tested tree), the
package versions and image identity, each consumed stage's
`stage.json` digest, and per frame the PNG hash, capture outcome,
source stage/artifact and preserved label (with duplicate resolutions).

Before consuming a single byte, refresh hash-validates every input
against its recorded hash (frame bytes vs `frames.json`, duplicate
targets vs their records), walks duplicate chains with cycle
detection, and rejects wallpaper frames, unverified duplicates, and
baseline-labeled frames for any state but the explicit desktop state
(`panel`). A passed `--merge-sha` must equal the recorded event SHA
or the refresh fails.

## Stale exposure

`scripts/tuna-walkthrough freshness` prints a per-frame
CURRENT/STALE/MISSING report and exits 1 unless every frame is
current. CURRENT requires the checked-in bytes to match the recorded
capture hash AND the recorded built tree to equal the expected
identity (explicit `--expect-*` flags, defaulting to this checkout);
a manifest that is merely self-consistent but names a different
source reads STALE. The one exception is adopt drift: trees that
differ only in the walkthrough's own managed paths still read
CURRENT, so the adopt commit itself does not invalidate the images;
any product drift reads STALE. When the expected identity cannot be
determined, every frame reads STALE (fail closed).

The `visual-capture` workflow's manifest job runs freshness on every
run as a visible, non-gating report, so staleness shows in CI output
instead of hiding as silent absence. Until the first refresh lands,
all 46 checked-in images report STALE (no manifest): that is the
honest initial state for images last changed before this pipeline
existed.
