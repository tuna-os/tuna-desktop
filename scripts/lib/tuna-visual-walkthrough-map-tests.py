#!/usr/bin/env python3
"""Walkthrough visual map matches what the producer emits.

Pins the producer<->TSV filename correspondence for all 46 walkthrough
states: every docs/walkthrough-visual.tsv row must name a frame its
stage in scripts/tuna-visual-capture emits via exactly one `shot`
call, and every id the producer claims in stage_walkthrough() must be
mapped. A rename on either side without the other fails here instead
of at refresh time ("refresh refused: ... missing ...").

Stages legitimately emit extra evidence frames beyond the walkthrough
set (before/after pairs, non-walkthrough stages), so the pinned
reverse direction is the claimed-id set, not the raw shot set.
"""
import re
import unittest
from collections import defaultdict
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
PRODUCER = ROOT / "scripts" / "tuna-visual-capture"
VISUAL_TSV = ROOT / "docs" / "walkthrough-visual.tsv"

STAGE_HEADER = re.compile(r"^# --- stage: (\S+)")
SHOT_CALL = re.compile(r"\bshot ([a-z0-9][a-z0-9-]*)")
WALKTHROUGH_CLAIM = re.compile(r'\s*(\S+)\) echo "(.*)" ;;')


def visual_map():
    mapping = {}
    with open(VISUAL_TSV, encoding="utf-8") as handle:
        for line in handle:
            if line.strip() and not line.startswith("#"):
                ident, stage, frame = line.rstrip("\n").split("\t")[:3]
                mapping[ident] = (stage, frame)
    return mapping


def producer_shots():
    """Shot basenames per stage, attributed by stage header."""
    shots = defaultdict(list)
    stage = None
    with open(PRODUCER, encoding="utf-8") as handle:
        for line in handle:
            header = STAGE_HEADER.match(line)
            if header:
                stage = header.group(1)
                continue
            if line.lstrip().startswith("#"):
                continue
            match = SHOT_CALL.search(line)
            if match:
                assert stage is not None, "shot before any stage header: %s" % line
                shots[stage].append(match.group(1))
    return dict(shots)


def claimed_ids():
    """Walkthrough ids each stage claims, from stage_walkthrough()."""
    text = PRODUCER.read_text(encoding="utf-8")
    body = re.search(r"stage_walkthrough\(\) \{(.*?)\n\}", text, re.S).group(1)
    claimed = defaultdict(list)
    for line in body.splitlines():
        match = WALKTHROUGH_CLAIM.match(line)
        if match:
            for ident in match.group(2).split():
                claimed[ident].append(match.group(1))
    return dict(claimed)


class VisualMapCase(unittest.TestCase):
    def test_all_46_mapped_frames_produced_exactly_once(self):
        mapping = visual_map()
        self.assertEqual(len(mapping), 46)
        shots = producer_shots()
        for ident in sorted(mapping):
            stage, frame = mapping[ident]
            with self.subTest(id=ident):
                produced = shots.get(stage, [])
                hits = [name for name in produced if name + ".png" == frame]
                self.assertEqual(
                    len(hits), 1,
                    "%s: stage %s emits %s, want exactly one %s" % (
                        ident, stage, sorted(n + ".png" for n in produced), frame))

    def test_claimed_ids_match_map(self):
        mapping = visual_map()
        claimed = claimed_ids()
        self.assertEqual(sorted(claimed), sorted(mapping))
        for ident in sorted(claimed):
            with self.subTest(id=ident):
                self.assertIn(mapping[ident][0], claimed[ident])


if __name__ == "__main__":
    unittest.main()
