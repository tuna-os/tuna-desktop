#!/usr/bin/env python3
"""scripts/lib/tuna_process_metrics.py attributes a process tree's memory:
totals for the growth gates, PSS by kind per process, and the largest
rises between two samples."""
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts/lib"))
import tuna_process_metrics as pm  # noqa: E402


def sample(**processes):
    return {"per_process": {name: {"pss_by_kind_kib": kinds} for name, kinds in processes.items()}}


class MappingKinds(unittest.TestCase):
    def test_mappings_are_named_by_their_owner(self):
        self.assertEqual(pm.mapping_kind(""), "anon")
        self.assertEqual(pm.mapping_kind("[heap]"), "heap")
        self.assertEqual(pm.mapping_kind("[stack]"), "stack")
        self.assertEqual(pm.mapping_kind("[anon:glibc: malloc arena]"), "anon")
        self.assertEqual(pm.mapping_kind("/memfd:gdk-wayland (deleted)"), "shm")
        self.assertEqual(pm.mapping_kind("/dev/shm/pool"), "shm")
        self.assertEqual(pm.mapping_kind("/dev/dri/card1"), "gpu")
        self.assertEqual(pm.mapping_kind("/usr/lib/libc.so.6"), "file")


class LiveTree(unittest.TestCase):
    def test_a_process_tree_is_sampled_per_process(self):
        # The child reports ready only once its interpreter has loaded, so
        # its own mappings stop changing before it is sampled.
        child = subprocess.Popen([sys.executable, "-c",
                                  "import sys; print('ready', flush=True); sys.stdin.read()"],
                                 stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
        try:
            self.assertEqual(child.stdout.readline().strip(), "ready")
            self.assertEqual(pm.tree(os.getpid()) & {os.getpid(), child.pid},
                             {os.getpid(), child.pid})
            value = pm.metrics(child.pid)
            # smaps and smaps_rollup are separate reads, and shared pages'
            # PSS moves whenever another process maps or drops the same
            # libraries (CI: 2898 vs 2844 KiB). Compare the per-mapping
            # kinds only with a rollup that held still across the smaps read.
            for _ in range(20):
                before = pm.rollup(child.pid)["pss_kib"]
                kinds = pm.process_memory(child.pid)
                after = pm.rollup(child.pid)["pss_kib"]
                if before == after:
                    break
            else:
                self.fail("the child's PSS never held still across one smaps read")
        finally:
            child.stdin.close()
            child.wait()
        self.assertEqual(value["processes"], 1)
        (name, process), = value["per_process"].items()
        self.assertTrue(name.endswith(f":{child.pid}"))
        self.assertEqual(value["pss_kib"], process["pss_kib"])
        self.assertEqual(value["fds"], process["fds"])
        self.assertEqual(value["compositor_fds"], process["fds"])
        self.assertGreater(process["pss_kib"], 0)
        self.assertGreaterEqual(process["fds"], 3)
        self.assertGreater(process["maps"], 0)
        # The kernel's split adds up to PSS within one rollup read.
        split = process["pss_anon_kib"] + process["pss_file_kib"] + process["pss_shmem_kib"]
        self.assertAlmostEqual(split, process["pss_kib"], delta=8)
        self.assertGreater(process["pss_anon_kib"], 0)
        # The per-mapping kinds add up to PSS (each smaps line rounds down,
        # so allow a KiB per mapping).
        self.assertAlmostEqual(sum(kinds.values()), before, delta=process["maps"])
        self.assertIn("file", process["pss_by_kind_kib"])

    def test_a_process_that_exited_is_left_out(self):
        child = subprocess.Popen([sys.executable, "-c", "pass"])
        child.wait()
        value = pm.metrics(child.pid)
        self.assertEqual(value["per_process"], {})
        self.assertEqual(value["pss_kib"], 0)

    def test_steps_and_smaps_cover_the_tree(self):
        child = subprocess.Popen([sys.executable, "-c", "import sys; sys.stdin.read()"],
                                 stdin=subprocess.PIPE)
        try:
            light = pm.tree_rollup(child.pid)
            with tempfile.TemporaryDirectory() as directory:
                pm.save_smaps(child.pid, directory, "round0")
                saved = {path.name: path.read_text() for path in Path(directory).iterdir()}
        finally:
            child.stdin.close()
            child.wait()
        (name, values), = light.items()
        self.assertTrue(name.endswith(f":{child.pid}"))
        self.assertEqual(set(values), {"pss_kib", "rss_kib", "pss_anon_kib", "pss_file_kib",
                                       "pss_shmem_kib"})
        self.assertGreater(values["pss_kib"], 0)
        comm = name.rsplit(":", 1)[0]
        self.assertEqual(list(saved), [f"smaps-round0-{comm}-{child.pid}.txt"])
        self.assertIn("Pss:", saved[f"smaps-round0-{comm}-{child.pid}.txt"])


class LargestRises(unittest.TestCase):
    def test_rises_are_ranked_by_process_and_kind(self):
        before = sample(**{"tuna-compositor:10": {"heap": 1000, "shm": 500},
                           "tuna-shell-gtk:11": {"heap": 2000}})
        after = sample(**{"tuna-compositor:10": {"heap": 1200, "shm": 4500, "gpu": 300},
                          "tuna-shell-gtk:11": {"heap": 1500}})
        self.assertEqual(pm.largest_rises(before, after, 3),
                         [(4000, "tuna-compositor", "shm"), (300, "tuna-compositor", "gpu"),
                          (200, "tuna-compositor", "heap")])
        self.assertEqual(pm.largest_rises(before, after, 4)[-1], (-500, "tuna-shell-gtk", "heap"))

    def test_a_restarted_process_keeps_its_name(self):
        before = sample(**{"tuna-shell-gtk:11": {"heap": 2000}})
        after = sample(**{"tuna-shell-gtk:42": {"heap": 2600}})
        self.assertEqual(pm.largest_rises(before, after, 1), [(600, "tuna-shell-gtk", "heap")])


if __name__ == "__main__":
    unittest.main()
