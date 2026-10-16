#!/usr/bin/env python3
"""ci-select-lanes picks real coverage and stays in step with ci.yml."""
import importlib.machinery
import importlib.util
import unittest
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parents[2]
_loader = importlib.machinery.SourceFileLoader("ci_select_lanes", str(ROOT / "scripts/ci-select-lanes"))
_spec = importlib.util.spec_from_loader(_loader.name, _loader)
m = importlib.util.module_from_spec(_spec)
_loader.exec_module(m)

LANE_IF = "contains(fromJSON(needs.check.outputs.lanes), '{}')"


def pr(*changed, run=0, size=2):
    return m.select("pull_request", run, size, list(changed))[0]


class Selection(unittest.TestCase):
    def test_main_schedule_and_dispatch_run_every_lane(self):
        for event in ("push", "schedule", "workflow_dispatch"):
            self.assertEqual(m.select(event, 7, 2, [])[0], m.LANES)

    def test_shared_inputs_run_every_lane(self):
        for path in (".github/workflows/ci.yml", "Cargo.lock", "Cargo.toml",
                     "third-party/smithay/src/lib.rs", "scripts/ci-select-lanes",
                     "scripts/tuna-nested"):
            self.assertEqual(pr("docs/x.md", path), m.LANES, path)

    def test_docs_only_runs_no_lane(self):
        self.assertEqual(pr("README.md", "docs/perf/x.md", ".github/workflows/fuzz.yml",
                            "scripts/lib/ledger-tests.py", "fuzz/Cargo.toml"), [])

    def test_code_runs_gtk_shell_and_a_rotating_slice_with_producers(self):
        seen = set()
        for run in range(len(m.ROTATION)):
            lanes = pr("crates/compositor/src/lib.rs", run=run)
            self.assertIn("gtk-shell", lanes)
            rotated = set(m.rotation(run, 2))
            self.assertEqual(len(rotated), 2)
            self.assertTrue(rotated <= set(lanes))
            for lane in lanes:
                self.assertTrue(set(m.NEEDS.get(lane, [])) <= set(lanes), (run, lane))
            seen |= rotated
        self.assertEqual(seen, set(m.ROTATION))

    def test_rotation_size_is_clamped(self):
        self.assertEqual(m.rotation(3, 0), [])
        self.assertEqual(sorted(m.rotation(3, 99)), sorted(m.ROTATION))

    def test_owned_paths_run_their_lane_without_rotation(self):
        self.assertEqual(pr("scripts/tuna-scroll-proof"), ["scroll-proof"])
        self.assertEqual(pr("docs/walkthrough.tsv"), ["gtk-shell"])
        self.assertEqual(pr("packaging/marlin/vm-lane/Containerfile"),
                         ["arch-package", "marlin-image", "marlin-vm"])
        self.assertEqual(pr("docs/parity-ledger.md"),
                         ["journey", "drm-smoke", "gtk-shell", "arch-package",
                          "marlin-vm", "parity-ledger"])


class WorkflowSync(unittest.TestCase):
    def setUp(self):
        self.jobs = yaml.safe_load((ROOT / ".github/workflows/ci.yml").read_text())["jobs"]

    @staticmethod
    def needs(job):
        value = job.get("needs", [])
        return [value] if isinstance(value, str) else value

    def test_every_lane_job_is_gated_on_the_selection(self):
        lanes = [name for name in self.jobs if name not in ("check", "ci-gate")]
        self.assertEqual(lanes, m.LANES)
        for name in lanes:
            job = self.jobs[name]
            self.assertEqual(job.get("if"), LANE_IF.format(name), name)
            self.assertIn("check", self.needs(job), name)
            self.assertEqual(set(self.needs(job)) - {"check"}, set(m.NEEDS.get(name, [])), name)
        self.assertTrue(set(m.CORE + m.ROTATION) <= set(m.LANES))

    def test_gate_waits_for_every_job(self):
        gate = self.jobs["ci-gate"]
        self.assertEqual(gate.get("if"), "always()")
        self.assertEqual(set(self.needs(gate)), set(self.jobs) - {"ci-gate"})


if __name__ == "__main__":
    unittest.main()
