"""Tour public-still error evidence keeps the causal message, not just the type."""
import importlib.machinery
import importlib.util
from pathlib import Path
from types import SimpleNamespace
import tempfile
import unittest


source = Path(__file__).resolve().parents[1] / "tuna-vm-lane"
loader = importlib.machinery.SourceFileLoader("tour_public_still_lane", str(source))
spec = importlib.util.spec_from_loader(loader.name, loader)
lane = importlib.util.module_from_spec(spec)
loader.exec_module(lane)


class PublicStillErrorEvidence(unittest.TestCase):
    def recorder(self):
        out = tempfile.mkdtemp(prefix="tour-public-still-")
        rec = lane.Recorder(SimpleNamespace(cmd=lambda *a, **k: None), out, timeout=5.0)
        rec.frames.append({"sha256": "0" * 64, "requested_monotonic_ns": 1,
                           "completed_monotonic_ns": 2})
        rec.still = lambda card, delay=0.0: rec.stills.__setitem__(card, 0)
        return rec

    def test_observer_error_carries_type_and_message(self):
        rec = self.recorder()
        confirmed = rec.public_still("probe-card", observe=_raise)
        self.assertFalse(confirmed)
        self.assertIn("probe-card", rec.unconfirmed)
        evidence = rec.still_confirmations["probe-card"]["evidence"]
        for side in ("before", "after"):
            self.assertEqual(evidence[side]["observation_error"], "RuntimeError")
            self.assertEqual(evidence[side]["observation_error_detail"], "probe-detail-xyz")

    def test_successful_snapshot_has_no_error_keys(self):
        rec = self.recorder()
        rec.public_still("probe-card", observe=lambda remaining: None)
        evidence = rec.still_confirmations["probe-card"]["evidence"]
        for side in ("before", "after"):
            self.assertIsNone(evidence[side]["observation"])
            self.assertNotIn("observation_error", evidence[side])
            self.assertNotIn("observation_error_detail", evidence[side])


def _raise(remaining):
    raise RuntimeError("probe-detail-xyz")


if __name__ == "__main__":
    unittest.main()
