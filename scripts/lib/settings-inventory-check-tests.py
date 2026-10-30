#!/usr/bin/env python3
"""Negative and positive cases for scripts/tuna-settings-inventory-check."""
from importlib.machinery import SourceFileLoader
from importlib.util import module_from_spec, spec_from_loader
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]
_loader = SourceFileLoader("inventory_check", str(ROOT / "scripts/tuna-settings-inventory-check"))
check = module_from_spec(spec_from_loader("inventory_check", _loader))
_loader.exec_module(check)

TESTS = {"a_real_test"}
GATES = {"G-REAL", "J-REAL"}


def problems(status, owner):
    return check.check_owner("k", status, owner, TESTS, GATES)


class Owner(unittest.TestCase):
    def test_implemented_with_existing_ids_passes(self):
        self.assertEqual(problems("Honored", "implemented — mutter 51 `x.c:1`; proof:G-REAL, cargo:a_real_test"), [])
        self.assertEqual(problems("Honored", "implemented — mutter 51 `x.c`; cargo:mod::tests::a_real_test"), [])

    def test_implemented_needs_a_test_or_proof(self):
        self.assertTrue(problems("Honored", "implemented — mutter 51 `x.c:1`"))

    def test_unknown_ids_fail(self):
        self.assertTrue(problems("Honored", "implemented — mutter 51 `x.c`; cargo:no_such_test"))
        self.assertTrue(problems("Honored", "implemented — mutter 51 `x.c`; proof:G-NOT-EMITTED"))
        self.assertTrue(problems("Honored", "implemented — mutter 51 `x.c`; journey:J-MISSING"))

    def test_category_must_be_known_and_cited(self):
        self.assertTrue(problems("Ignored", "legacy — mutter 51 `x.c`"))
        self.assertTrue(problems("Ignored", "deprecated — nobody reads it"))

    def test_status_and_category_agree(self):
        self.assertTrue(problems("Ignored", "implemented — mutter 51 `x.c`; proof:G-REAL"))
        self.assertTrue(problems("Honored", "toolkit — gtk 4.22 `gdksettings-wayland.c`"))
        self.assertTrue(problems("Via GTK", "unimplemented — mutter 51 `x.c`; #1"))
        self.assertEqual(problems("Via GTK", "toolkit — gtk 4.22 `gdksettings-wayland.c`"), [])

    def test_unimplemented_links_an_issue(self):
        self.assertTrue(problems("Ignored", "unimplemented — gnome-shell 51 `a.js`"))
        self.assertEqual(problems("Ignored", "unimplemented — gnome-shell 51 `a.js`; #365"), [])

    def test_settings_exposed_keys_need_observable_proof_or_issue(self):
        exposed = "implemented — mutter 51 `x.c`; cargo:a_real_test [GNOME Settings: mouse]"
        self.assertTrue(problems("Honored", exposed))
        self.assertEqual(problems("Honored", exposed.replace(" [", "; observable proof pending #365 [")), [])
        self.assertEqual(problems("Honored", exposed.replace("cargo:a_real_test", "proof:G-REAL")), [])

    def test_three_cell_rows_are_rejected(self):
        with self.assertRaises(ValueError):
            list(check.parse("## org.gnome.desktop.x\n\n| k | Honored | reason |\n"))

    def test_repository_inventory_passes(self):
        tests, gates = check.known_ids(ROOT)
        text = (ROOT / "docs/settings-desktop-inventory.md").read_text(encoding="utf-8")
        found = [p for row in check.parse(text) for p in check.check_owner(row[1], row[2], row[3], tests, gates)]
        self.assertEqual(found, [])


if __name__ == "__main__":
    unittest.main()
