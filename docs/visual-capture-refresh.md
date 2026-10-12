# Walkthrough refresh procedure

Rebuilds `docs/walkthrough/` images ONLY from exact, fully successful
visual-capture artifacts. Never refresh from a partial run, from mixed
attempts, or from frames the labeler did not confirm as feature proof;
`refresh` refuses all of those. Checked-in images stay STALE-listed
until a refresh lands: nothing labels them current automatically.

## Prerequisites

- A `visual-capture` CI run whose conclusion is `success` (build ok,
  all 14 capture jobs ok, manifest ok). Note its run id.
- The tested merge SHA for that run (for PR runs this is the merge
  commit; record it distinctly from the branch head, never conflated).

## Exact commands

```sh
RUN=<run id>        # e.g. 38240110439
MERGE=<merge sha>   # e.g. 7b6c85922af14f4c239a4f958262b657adc6b03f
BRANCH=<branch>     # e.g. w3-visual-capture

# 1. Confirm the run is fully green first (a red run must never feed a refresh).
gh run view "$RUN" --json conclusion --jq .conclusion
#    -> success

# 2. Download the exact artifacts of that run into one bundle dir.
rm -rf visual-exact
gh run download "$RUN" -R tuna-os/tuna-desktop --dir visual-exact

# 3. Refresh into a scratch dest first and inspect the report.
scripts/tuna-walkthrough refresh visual-exact --run-id "$RUN" \
    --merge-sha "$MERGE" --branch "$BRANCH" --dest /tmp/walkthrough-new
scripts/tuna-walkthrough freshness --dest /tmp/walkthrough-new
#    -> freshness 46/46 current

# 4. Adopt the result into docs/ (images + page + per-frame manifest).
cp /tmp/walkthrough-new/walkthrough/*.png docs/walkthrough/
cp /tmp/walkthrough-new/walkthrough/manifest.json docs/walkthrough/
cp /tmp/walkthrough-new/walkthrough.md docs/walkthrough.md
scripts/tuna-walkthrough check        # existing existence gate, unchanged
scripts/tuna-walkthrough freshness    # -> freshness 46/46 current
```

## What refresh pins

Per run, `docs/walkthrough/manifest.json` records: the pinned run id
and attempt (all consumed stages must share both), the branch head SHA
and tested merge SHA as distinct fields, the source tree-diff hash, the
CI package versions and image identity, each consumed stage's
`stage.json` digest, and per frame the PNG hash, capture outcome,
source stage/artifact and frame label (with duplicate resolutions).

## Stale exposure

`scripts/tuna-walkthrough freshness` prints a per-frame
CURRENT/STALE/MISSING report and exits 1 unless every frame is current.
The `visual-capture` workflow's manifest job runs it on every run as a
visible, non-gating report (`|| true`), so staleness shows in CI output
instead of hiding as silent absence. Until the first refresh lands, all
46 checked-in images report STALE (no manifest): that is the honest
initial state for images last changed before this pipeline existed.
