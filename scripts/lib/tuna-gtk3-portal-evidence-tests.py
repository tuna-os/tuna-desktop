#!/usr/bin/python3
"""Unit parser cases only: synthetic logs never qualify a toolkit/portal run.

The installed consumer producer is tuna-gtk3-portal-proof.py. These tests guard
its evidence parser against turning registry advertisements, mismatched handles,
null buffers or unrelated surfaces into a claimed real GTK3 surface export.
"""
import unittest

from tuna_gtk3_portal_evidence import wire_export


class WireEvidenceTests(unittest.TestCase):
    def trace(self, version=2, marker="@"):
        operation = "export_toplevel" if version == 2 else "export"
        return (f"[1] -> wl_surface{marker}24.attach(wl_buffer{marker}30, 0, 0)\n"
                f"[2] -> wl_surface{marker}24.commit()\n"
                f"[3] -> zxdg_exporter_v{version}{marker}20.{operation}(new id zxdg_exported_v{version}{marker}38, wl_surface{marker}24)\n"
                f'[4] zxdg_exported_v{version}{marker}38.handle("parser-fixture-only")\n'
                f"[5] -> zxdg_exported_v{version}{marker}38.destroy()\n")

    def test_actual_export_selects_version_not_advertisement(self):
        advertisements = ('wl_registry@2.global(8, "zxdg_exporter_v1", 1)\n'
                          'wl_registry@2.global(9, "zxdg_exporter_v2", 1)\n')
        result = wire_export(advertisements + self.trace(), "parser-fixture-only")
        self.assertEqual(result["xdg_foreign_version"], 2)
        self.assertFalse(result["v1_toolkit_proof"])
        self.assertEqual(result["surface_object_id"], 24)

    def test_v1_and_both_libwayland_object_id_formats(self):
        for version in (1, 2):
            for marker in ("@", "#"):
                with self.subTest(version=version, marker=marker):
                    result = wire_export(self.trace(version, marker), "parser-fixture-only")
                    self.assertEqual(result["xdg_foreign_version"], version)
                    self.assertEqual(result["v1_toolkit_proof"], version == 1)

    def test_registry_only_cannot_qualify(self):
        with self.assertRaisesRegex(RuntimeError, "exactly one"):
            wire_export('wl_registry@2.global(9, "zxdg_exporter_v2", 1)', "parser-fixture-only")

    def test_callback_handle_must_match_wire_event(self):
        with self.assertRaisesRegex(RuntimeError, "callback handle"):
            wire_export(self.trace(), "different-parser-fixture")

    def test_null_or_unrelated_surface_buffer_cannot_qualify(self):
        for changed in (self.trace().replace("attach(wl_buffer@30", "attach(nil"),
                        self.trace().replace("wl_surface@24.attach", "wl_surface@25.attach")):
            with self.assertRaisesRegex(RuntimeError, "non-null attached buffer"):
                wire_export(changed, "parser-fixture-only")

    def test_attachment_without_same_surface_commit_cannot_qualify(self):
        with self.assertRaisesRegex(RuntimeError, "did not commit"):
            wire_export(self.trace().replace("wl_surface@24.commit()", "wl_surface@25.commit()"), "parser-fixture-only")

    def test_export_requires_destroy_of_same_object(self):
        with self.assertRaisesRegex(RuntimeError, "destroy"):
            wire_export(self.trace().replace("zxdg_exported_v2@38.destroy", "zxdg_exported_v2@39.destroy"), "parser-fixture-only")

    def test_duplicate_export_cannot_qualify(self):
        with self.assertRaisesRegex(RuntimeError, "exactly one"):
            wire_export(self.trace() + self.trace(), "parser-fixture-only")

    def test_destroy_before_handle_event_cannot_qualify(self):
        lines = self.trace().splitlines()
        lines[3], lines[4] = lines[4], lines[3]
        with self.assertRaisesRegex(RuntimeError, "before its actual handle"):
            wire_export("\n".join(lines), "parser-fixture-only")


if __name__ == "__main__":
    unittest.main()
