#!/usr/bin/env python3
"""Couple frame labels to stage verdicts; write stage.json.

A state whose stills are all wallpaper-only (indistinguishable from the
bare desktop), duplicates, or unverified is demoted to failed, however
its drive commands exited: the stage verdict must reflect the actual
frame content, never a silent green. Exemptions, each documented:

- panel/desktop: the desktop IS the bare desktop (baseline/duplicate
  of baseline is the expected, honest evidence).
- settings-a11y/clock: the seconds digits change sub-threshold pixels;
  the state is validated by its settings consumer instead.
- transitions/*: clip-led states. Their evidence is the x11grab clip
  plus compositor end-state validators; settled stills are recorded
  with labels for review but do not drive the verdict.

Usage: tuna-visual-finalize.py DIR STAGE LEDGER_IDS WALKTHROUGH_IDS STARTED_UTC FINISHED_UTC
(LEDGER_IDS and WALKTHROUGH_IDS are space-separated, possibly empty.)
Exit status is always 0; failures are recorded in stage.json, never raised.
"""
import json
import os
import sys

EXEMPT_STATES = {("panel", "desktop"), ("settings-a11y", "clock")}
EXEMPT_STAGES = {"transitions"}


def read_jsonl(path):
    rows = []
    if os.path.exists(path):
        with open(path, encoding="utf-8") as handle:
            for line in handle:
                line = line.strip()
                if line:
                    try:
                        rows.append(json.loads(line))
                    except ValueError:
                        pass
    return rows


def main():
    directory, stage, ledger, walkthrough, started, finished = sys.argv[1:7]
    steps = read_jsonl(os.path.join(directory, "steps.jsonl"))
    frames = {}
    frames_path = os.path.join(directory, "frames.json")
    if os.path.exists(frames_path):
        with open(frames_path, encoding="utf-8") as handle:
            try:
                frames = json.load(handle)
            except ValueError:
                frames = {}
    by_state = {}
    for entry in frames.get("frames", []):
        by_state.setdefault(entry.get("state"), []).append(entry)

    for step in steps:
        if step.get("status") != "ok":
            continue
        name = step.get("state")
        if stage in EXEMPT_STAGES or (stage, name) in EXEMPT_STATES:
            step["label_backstop"] = "exempt"
            continue
        stills = [e for e in by_state.get(name, [])
                  if str(e.get("file", "")).endswith(".png")]
        labels = sorted(e.get("label", "?") for e in stills)
        if any(label == "feature" for label in labels):
            step["label_backstop"] = "feature-frame-present"
            continue
        if not stills:
            step["status"] = "failed"
            step["note"] = "demoted: state captured no stills"
        else:
            step["status"] = "failed"
            step["note"] = ("demoted: no feature frame (labels: %s); "
                            "wallpaper-only, not proof" % ", ".join(labels))
        step["label_backstop"] = "demoted"

    with open(os.path.join(directory, "steps.jsonl"), "w", encoding="utf-8") as handle:
        for step in steps:
            handle.write(json.dumps(step) + "\n")

    counts = {"ok": 0, "failed": 0, "skipped": 0}
    for step in steps:
        counts[step.get("status", "failed")] = counts.get(step.get("status"), 0) + 1
    if not steps:
        status = "failed"
    elif counts.get("failed", 0) == 0 and counts.get("skipped", 0) == 0:
        status = "ok"
    elif counts.get("ok", 0) > 0:
        status = "partial"
    else:
        status = "failed"
    manifest = {
        "stage": stage,
        "status": status,
        "started_utc": started,
        "finished_utc": finished,
        "ledger": ledger.split(),
        "walkthrough": walkthrough.split(),
        "steps": steps,
        "observations": read_jsonl(os.path.join(directory, "observations.jsonl")),
        "frame_summary": frames.get("summary", {}),
        "state_counts": counts,
    }
    with open(os.path.join(directory, "stage.json"), "w", encoding="utf-8") as handle:
        json.dump(manifest, handle, indent=2)
        handle.write("\n")
    print("tuna-visual-finalize [%s]: %s (states ok=%d failed=%d skipped=%d)"
          % (stage, status, counts.get("ok", 0), counts.get("failed", 0),
             counts.get("skipped", 0)))
    return 0


if __name__ == "__main__":
    sys.exit(main())
