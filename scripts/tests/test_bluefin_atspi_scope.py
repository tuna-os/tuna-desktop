#!/usr/bin/env python3
import copy, json, sys, unittest
from pathlib import Path
ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts/lib"))
import bluefin_atspi_scope as scope  # noqa: E402


class Scope(unittest.TestCase):
    def setUp(self):
        self.data = json.loads(scope.MANIFEST.read_text())

    def test_committed_manifest_is_valid(self):
        self.assertEqual(scope.validate(self.data), [])

    def test_inventories_every_pinned_scenario(self):
        counts = {}
        for s in self.data["scenarios"]:
            counts[s["feature"]] = counts.get(s["feature"], 0) + 1
        self.assertEqual(counts, {"gnome_files.feature": 8, "gnome_settings.feature": 12,
                                  "gnome_accessibility.feature": 5, "orca.feature": 4})

    def test_floating_revision_rejected(self):
        d = copy.deepcopy(self.data); d["upstream"]["revision"] = "main"
        self.assertTrue(scope.validate(d))

    def test_retry_or_fallback_policy_rejected(self):
        for key in scope.FORBIDDEN_POLICY:
            d = copy.deepcopy(self.data); d["policy"][key] = True
            self.assertTrue(scope.validate(d), key)

    def test_missing_reason_rejected(self):
        d = copy.deepcopy(self.data); d["scenarios"][0]["reason"] = ""
        self.assertTrue(scope.validate(d))

    def test_gsettings_and_panel_only_not_counted_as_portable(self):
        for s in self.data["scenarios"]:
            if "gsettings" in s["upstream_tags"] or s["scenario"].endswith("settings panel is accessible"):
                self.assertEqual(s["disposition"], "insufficient-alone", s["scenario"])


if __name__ == "__main__":
    unittest.main()
