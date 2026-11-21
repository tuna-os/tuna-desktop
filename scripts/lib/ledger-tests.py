#!/usr/bin/env python3
"""Ledger validation cannot declare an empty or partially qualified v1."""
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "tuna-ledger"
HEADER = "| ID | GNOME 51 behavior | Tuna Desktop status | Tests | Evidence / notes | Deviation and owner |\n|---|---|---|---|---|---|\n"

class LedgerQualification(unittest.TestCase):
    def check(self, rows, *, complete=False, assertions="G-TEST pass test\n", header=HEADER, extra=()):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "ledger").write_text(header + rows)
            (root / "tests").write_text("module::exists: test\n")
            (root / "assertions").write_text(assertions)
            argv = [sys.executable, str(SCRIPT), "check", "--ledger", str(root / "ledger"),
                    "--tests", str(root / "tests"), "--assertions", str(root / "assertions")]
            for index, text in enumerate(extra):
                (root / f"extra-{index}").write_text(text)
                argv += ["--assertions", str(root / f"extra-{index}")]
            if complete:
                argv.append("--require-complete")
            return subprocess.run(argv, capture_output=True, text=True, timeout=60)

    def row(self, status="pass", refs="proof:G-TEST", ident="P-TEST", behavior="A real behavior"):
        return f"| {ident} | {behavior} | {status} | {refs} | evidence | — |\n"

    def test_renamed_or_missing_header_reports_schema_error_without_traceback(self):
        for header in (HEADER.replace("GNOME 51 behavior", "Behavior"), ""):
            for complete in (False, True):
                result = self.check(self.row(), complete=complete, header=header)
                self.assertEqual(result.returncode, 1)
                self.assertIn("ledger header must be", result.stderr)
                self.assertNotIn("Traceback", result.stderr)

    def test_empty_ledger_is_never_success(self):
        for complete in (False, True):
            result = self.check("", complete=complete)
            self.assertEqual(result.returncode, 1)
            self.assertIn("no capability rows", result.stderr)

    def test_typo_or_empty_status_is_rejected(self):
        for status in ("passing", "", "unknown", "green"):
            self.assertEqual(self.check(self.row(status)).returncode, 1)

    def test_blank_id_or_behavior_is_rejected(self):
        self.assertEqual(self.check(self.row(ident="")).returncode, 1)
        self.assertEqual(self.check(self.row(behavior="")).returncode, 1)

    def test_v1_rejects_partial_missing_deviation_untested_and_nested_pass(self):
        for status in ("partial", "missing", "deviation", "untested", "pass (nested)"):
            result = self.check(self.row(status), complete=True)
            self.assertEqual(result.returncode, 1, status)
            self.assertIn("v1 requires pass", result.stderr)

    def test_incremental_check_still_accepts_known_incomplete_states(self):
        for status in ("partial", "missing", "deviation", "untested", "pass (nested)"):
            self.assertEqual(self.check(self.row(status)).returncode, 0, status)

    def test_all_unqualified_pass_rows_still_need_real_passing_proof(self):
        result = self.check(self.row(), complete=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.check(self.row(), complete=True, assertions="G-TEST fail test\n").returncode, 1)
        self.assertEqual(self.check(self.row(refs=""), complete=True).returncode, 1)
        self.assertEqual(self.check(self.row(refs="cargo:gone"), complete=True).returncode, 1)

    def test_a_citation_passes_only_with_the_lane_file_that_records_it(self):
        # A partial row's citations are checked too: a gate recorded only by
        # another lane's assertions.txt (the VM legs: marlin-vm-orca51,
        # marlin-vm-autotz) fails until that file is an input.
        row = self.row(status="partial", refs="proof:G-TEST, proof:V-ORCA-NATIVE-KEYBOARD")
        withheld = self.check(row)
        self.assertEqual(withheld.returncode, 1)
        self.assertIn("P-TEST: proof assertion 'V-ORCA-NATIVE-KEYBOARD' did not pass", withheld.stderr)
        fed = self.check(row, extra=["V-ORCA-NATIVE-KEYBOARD pass spoken command\n"])
        self.assertEqual(fed.returncode, 0, fed.stderr)
        # A failure in any input wins over a pass in another.
        self.assertEqual(self.check(row, extra=["V-ORCA-NATIVE-KEYBOARD pass a\n",
                                                "V-ORCA-NATIVE-KEYBOARD fail b\n"]).returncode, 1)

if __name__ == "__main__":
    unittest.main()
