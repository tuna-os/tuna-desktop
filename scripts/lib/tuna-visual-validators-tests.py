#!/usr/bin/env python3
"""Every named capture state declares a consumer+render validator.

Static regression over scripts/tuna-visual-capture: each st_* state
function must call a require_* validator (or fail with an explicit
validator-miss note), so no state can return to silent green on drive
commands alone. The label backstop in tuna-visual-finalize.py is the
second layer; this test pins the first.
"""
import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "tuna-visual-capture"
VALIDATOR = re.compile(r"require_(jq|cstate|frame_differs|nonempty_file|absent)\b"
                       r"|validator miss")


class ValidatorsCase(unittest.TestCase):
    def state_bodies(self):
        text = SCRIPT.read_text(encoding="utf-8")
        bodies = {}
        current = None
        depth = 0
        for line in text.splitlines():
            start = re.match(r"^(st_[a-z0-9_]+)\(\) \{$", line)
            if start and current is None:
                current = start.group(1)
                bodies[current] = []
                depth = 1
                continue
            if current is not None:
                bodies[current].append(line)
                depth += line.count("{") - line.count("}")
                if depth <= 0 or line == "}":
                    current = None
        return bodies

    def test_every_state_has_a_validator(self):
        bodies = self.state_bodies()
        text = SCRIPT.read_text(encoding="utf-8")
        driven = set(re.findall(r"run_state\s+\S+\s+(st_[a-z0-9_]+)", text))
        self.assertGreater(len(driven), 50, "expected all driven states, found %d" % len(driven))
        self.assertTrue(driven <= set(bodies),
                        "driven states without bodies: %s" % sorted(driven - set(bodies)))
        missing = sorted(name for name in driven
                         if not VALIDATOR.search("\n".join(bodies[name])))
        self.assertEqual(missing, [], "states without a validator: %s" % missing)

    def test_helpers_exist(self):
        text = SCRIPT.read_text(encoding="utf-8")
        for helper in ("require_jq", "require_cstate", "require_frame_differs",
                       "require_nonempty_file", "require_absent"):
            self.assertIn("%s() {" % helper, text)


if __name__ == "__main__":
    unittest.main()
