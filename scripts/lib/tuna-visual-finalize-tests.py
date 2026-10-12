#!/usr/bin/env python3
"""Verdict coupling: labels drive stage status; partial is exposed.

Covers scripts/lib/tuna-visual-finalize.py: wallpaper-only states are
demoted (never silent green), documented exemptions hold, and stage
status plus per-state counts expose ok/partial/failed honestly.
"""
import json
import sys
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
FINALIZE = ROOT / "scripts" / "lib" / "tuna-visual-finalize.py"


class FinalizeCase(unittest.TestCase):
    def finalize(self, root, stage, steps, frames):
        directory = Path(root)
        (directory / "steps.jsonl").write_text(
            "\n".join(json.dumps(step) for step in steps) + "\n", encoding="utf-8")
        (directory / "frames.json").write_text(
            json.dumps({"summary": {}, "frames": frames}), encoding="utf-8")
        result = subprocess.run(
            [sys.executable, str(FINALIZE), str(directory), stage, "P-X-01", "",
             "2026-10-12T11:00:00Z", "2026-10-12T11:01:00Z"],
            capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        return json.loads((directory / "stage.json").read_text())

    def step(self, name, status="ok"):
        return {"state": name, "status": status, "seconds": 1, "note": ""}

    def frame(self, name, state, label):
        return {"file": name, "state": state, "label": label}

    def test_wallpaper_only_state_is_demoted(self):
        with tempfile.TemporaryDirectory() as directory:
            manifest = self.finalize(
                directory, "calendar",
                [self.step("date-menu"), self.step("with-notification")],
                [self.frame("calendar-menu.png", "date-menu", "feature"),
                 self.frame("calendar-with-notification.png", "with-notification",
                            "indistinguishable-from-baseline")])
            self.assertEqual(manifest["status"], "partial")
            by_state = {s["state"]: s for s in manifest["steps"]}
            self.assertEqual(by_state["date-menu"]["status"], "ok")
            self.assertEqual(by_state["with-notification"]["status"], "failed")
            self.assertIn("wallpaper-only", by_state["with-notification"]["note"])
            self.assertEqual(manifest["state_counts"], {"ok": 1, "failed": 1, "skipped": 0})

    def test_duplicate_only_state_is_demoted(self):
        with tempfile.TemporaryDirectory() as directory:
            manifest = self.finalize(
                directory, "settings-a11y",
                [self.step("background")],
                [self.frame("settings-wallpaper.png", "background", "duplicate")])
            self.assertEqual(manifest["status"], "failed")
            self.assertIn("wallpaper-only", manifest["steps"][0]["note"])

    def test_desktop_and_clock_exemptions_hold(self):
        with tempfile.TemporaryDirectory() as directory:
            panel = self.finalize(
                directory, "panel", [self.step("desktop")],
                [self.frame("panel-desktop.png", "desktop", "duplicate")])
            self.assertEqual(panel["status"], "ok")
            clock = self.finalize(
                directory, "settings-a11y", [self.step("clock")],
                [self.frame("settings-clock-seconds.png", "clock",
                            "indistinguishable-from-baseline")])
            self.assertEqual(clock["status"], "ok")

    def test_transitions_are_clip_led(self):
        with tempfile.TemporaryDirectory() as directory:
            manifest = self.finalize(
                directory, "transitions", [self.step("switcher-fade")],
                [self.frame("trans-switcher.webm", "switcher-fade", "clip"),
                 self.frame("trans-switcher-after.png", "switcher-fade", "duplicate")])
            self.assertEqual(manifest["status"], "ok")

    def test_all_failed_stays_failed(self):
        with tempfile.TemporaryDirectory() as directory:
            manifest = self.finalize(
                directory, "input",
                [self.step("preedit", "failed"), self.step("candidates", "skipped")],
                [])
            self.assertEqual(manifest["status"], "failed")
            self.assertEqual(manifest["state_counts"], {"ok": 0, "failed": 1, "skipped": 1})

    def test_all_ok_stays_ok(self):
        with tempfile.TemporaryDirectory() as directory:
            manifest = self.finalize(
                directory, "panel",
                [self.step("desktop"), self.step("tray")],
                [self.frame("panel-desktop.png", "desktop", "duplicate"),
                 self.frame("panel-tray-menu.png", "tray", "feature")])
            self.assertEqual(manifest["status"], "ok")
            self.assertEqual(manifest["ledger"], ["P-X-01"])


if __name__ == "__main__":
    unittest.main()
