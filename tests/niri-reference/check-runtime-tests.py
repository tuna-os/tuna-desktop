#!/usr/bin/env python3
import importlib.util
from pathlib import Path
import unittest

SPEC = importlib.util.spec_from_file_location("runtime_admission", Path(__file__).with_name("check-runtime.py"))
RUNTIME = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RUNTIME)


class RuntimeAdmission(unittest.TestCase):
    def test_all_libraries_required(self):
        calls = []
        receipt = RUNTIME.admit(calls.append)
        self.assertTrue(receipt["admitted"])
        self.assertEqual(calls, list(RUNTIME.LIBRARIES))
        self.assertEqual(receipt["loaded"], calls)

    def test_missing_library_refuses_and_preserves_cause(self):
        for missing in RUNTIME.LIBRARIES:
            with self.subTest(missing=missing):
                def load(library):
                    if library == missing:
                        raise OSError("fixture missing library")
                receipt = RUNTIME.admit(load)
                self.assertFalse(receipt["admitted"])
                self.assertEqual(receipt["missing"], {missing: "fixture missing library"})
                self.assertEqual(len(receipt["loaded"]), len(RUNTIME.LIBRARIES) - 1)


if __name__ == "__main__":
    unittest.main()
