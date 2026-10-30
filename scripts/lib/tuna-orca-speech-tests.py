#!/usr/bin/env python3
"""tuna-orca-speech.py reads speech-dispatcher's log as the proof needs."""
import importlib.machinery
import importlib.util
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
_loader = importlib.machinery.SourceFileLoader("orca_speech", str(ROOT / "scripts/lib/tuna-orca-speech.py"))
_spec = importlib.util.spec_from_loader(_loader.name, _loader)
m = importlib.util.module_from_spec(_spec)
_loader.exec_module(m)

LOG = """=== panel
left control
Top Bar
frame
Activities
toggle button not pressed.
=== panel-next
Date and Time
menu.
=== panel
Top Bar
Activities
=== lock-type
\u25cf
\u25cf
"""


class Sections(unittest.TestCase):
    def test_a_step_is_the_lines_after_its_last_mark_up_to_the_next(self):
        self.assertEqual(m.section(LOG, "panel-next"), ["Date and Time", "menu."])
        self.assertEqual(m.section(LOG, "panel"), ["Top Bar", "Activities"])
        self.assertIsNone(m.section(LOG, "never-marked"))

    def test_expected_lines_match_exactly_and_in_order_around_other_speech(self):
        spoken = m.section(LOG.replace("=== panel\nTop Bar\nActivities\n", ""), "panel")
        self.assertIsNotNone(m.subsequence(spoken, ["Top Bar", "Activities", "toggle button not pressed."]))
        self.assertIsNone(m.subsequence(spoken, ["Activities", "Top Bar"]))
        self.assertIsNone(m.subsequence(spoken, ["Top"]))
        self.assertIsNone(m.subsequence(spoken, ["toggle button not pressed"]))

    def test_a_password_leaks_whole_or_letter_by_letter_but_bullets_do_not(self):
        self.assertEqual(m.leaks(["\u25cf", "\u25cf"], ["tunaspeak"]), [])
        self.assertEqual(m.leaks(["t", "\u25cf"], ["tunaspeak"]), ["t"])
        self.assertEqual(m.leaks(["password tunaspeak"], ["tunaspeak"]), ["password tunaspeak"])
        self.assertEqual(m.leaks(["return"], ["tunaspeak"]), [])

    def test_expect_waits_for_the_lines_and_reports_what_was_said(self):
        with tempfile.NamedTemporaryFile("w", suffix=".log", delete=False) as fh:
            fh.write(LOG)
        self.assertEqual(m.expect(fh.name, "lock-type", 0, ["\u25cf", "\u25cf"]), 0)
        self.assertEqual(m.expect(fh.name, "lock-type", 0, ["\u25cf", "\u25cf", "\u25cf"]), 1)
        Path(fh.name).unlink()


if __name__ == "__main__":
    unittest.main()
