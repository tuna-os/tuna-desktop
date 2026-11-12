"""No-GPU controls for the magnifier probe's actual readiness callbacks."""
import ast
import json
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
from types import SimpleNamespace
import unittest


ROOT = Path(__file__).resolve().parents[2]
PROBE = ast.parse((ROOT / "scripts/lib/tuna-magnifier-probe.py").read_text())
BOUNDS = next(node for node in PROBE.body
              if isinstance(node, ast.FunctionDef) and node.name == "bounds")
BUILD = next(node for node in PROBE.body
             if isinstance(node, ast.FunctionDef) and node.name == "build")
CALLBACKS = {node.name: node for node in ast.walk(BUILD)
             if isinstance(node, ast.FunctionDef) and node.name in {"report", "request"}}
PROOF = (ROOT / "scripts/tuna-magnifier-proof").read_text()
BOUNDS_SHELL = re.search(r"^bounds\(\) \{\n.*?^\}", PROOF, re.M | re.S).group()


class Widget:
    def __init__(self, rect, allocation=None, parent=None):
        x, y, width, height = rect
        self.rect = SimpleNamespace(origin=SimpleNamespace(x=x, y=y),
                                    size=SimpleNamespace(width=width, height=height))
        self.allocation = allocation if allocation is not None else (width, height)
        self.parent = parent
        self.mapped = True
        self.focus_requests = 0

    def get_mapped(self):
        return self.mapped

    def get_width(self):
        return self.allocation[0]

    def get_height(self):
        return self.allocation[1]

    def compute_bounds(self, _window):
        return True, self.rect

    def is_ancestor(self, ancestor):
        return self.parent is ancestor

    def grab_focus(self):
        self.focus_requests += 1


class MagnifierProbeReadinessTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.requests = Path(self.temp.name) / "request.json"
        self.records = []
        self.button = Widget([9, 9, 80, 34])
        self.area = Widget([9, 43, 600, 344])
        self.entry = Widget([9, 387, 600, 34])
        self.focused = self.button
        self.namespace = {
            "Path": Path, "json": json, "REQUESTS": str(self.requests),
            "button": self.button, "area": self.area, "entry": self.entry,
            "win": SimpleNamespace(get_focus=lambda: self.focused),
            "handled": [-1], "log": self.records.append,
        }
        exec(compile(ast.Module(body=[BOUNDS, CALLBACKS["report"], CALLBACKS["request"]],
                                type_ignores=[]), "actual-magnifier-readiness", "exec"),
             self.namespace)

    def request_entry(self, generation):
        self.requests.write_text(json.dumps({"generation": generation,
                                             "request": "focus-entry"}))
        self.assertTrue(self.namespace["request"]())

    def test_zero_area_cannot_publish_map_bounds(self):
        self.area.allocation = (0, 0)
        self.area.rect.size.width = self.area.rect.size.height = 0
        self.assertTrue(self.namespace["report"]())
        self.assertEqual(self.records, [])

    def test_tiny_css_bounds_do_not_mean_entry_was_allocated(self):
        self.entry.allocation = (0, 0)
        self.entry.rect.origin.x, self.entry.rect.origin.y = -9, -1
        self.entry.rect.size.width, self.entry.rect.size.height = 18, 2
        self.focused = self.entry
        self.request_entry(1)
        self.assertFalse(Path(str(self.requests) + ".ack").exists())
        self.assertEqual(self.records, [])

    def test_unmapped_or_zero_computed_bounds_are_not_ready(self):
        self.entry.mapped = False
        self.assertIsNone(self.namespace["bounds"](self.entry, None))
        self.entry.mapped = True
        self.entry.rect.size.height = 0
        self.assertIsNone(self.namespace["bounds"](self.entry, None))

    def test_eventual_focused_allocated_entry_acknowledges_current_generation(self):
        self.assertFalse(self.namespace["report"]())
        self.request_entry(1)
        ack = Path(str(self.requests) + ".ack")
        self.assertFalse(ack.exists())  # Focus request alone is not readiness.
        self.focused = Widget([9, 387, 600, 34], parent=self.entry)
        self.entry.allocation = (0, 0)
        self.request_entry(1)
        self.assertFalse(ack.exists())
        self.entry.allocation = (600, 34)
        self.request_entry(1)
        self.assertEqual(ack.read_text(), "1")
        self.assertEqual(self.records[-1]["generation"], 1)
        self.assertEqual(self.records[-1]["bounds"]["entry"], [9, 387, 600, 34])
        self.focused = self.button
        self.request_entry(2)
        self.assertEqual(ack.read_text(), "1")
        self.assertEqual(self.records[-1]["generation"], 1)

    @unittest.skipUnless(shutil.which("jq"), "proof bounds reader requires jq")
    def test_proof_rejects_stale_generation_and_reads_current_bounds(self):
        log = Path(self.temp.name) / "probe.jsonl"
        log.write_text(json.dumps({"bounds": {"entry": [-9, -1, 18, 2]},
                                   "generation": 1}) + "\n")

        def read_current():
            return subprocess.run(["/bin/sh", "-c", BOUNDS_SHELL + '\nbounds entry 2',
                                   "bounds-control"], env={"PROBE_LOG": str(log)},
                                  capture_output=True, text=True)

        self.assertNotEqual(read_current().returncode, 0)
        with log.open("a") as out:
            out.write(json.dumps({"bounds": {"entry": [9, 387, 600, 34]},
                                  "generation": 2}) + "\n")
        current = read_current()
        self.assertEqual(current.returncode, 0, current.stderr)
        self.assertEqual(current.stdout.strip(), "9\t387\t600\t34")


if __name__ == "__main__":
    unittest.main()
