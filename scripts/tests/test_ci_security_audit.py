#!/usr/bin/env python3
"""Exercise audit exit propagation without a network or GitHub token."""
import os
import pathlib
import subprocess
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]


class SecurityAuditTests(unittest.TestCase):
    def test_audit_success_and_failures_are_preserved(self):
        with tempfile.TemporaryDirectory() as tmp:
            cargo = pathlib.Path(tmp) / "cargo"
            cargo.write_text("#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$AUDIT_ARGS\"\nexit \"$AUDIT_STATUS\"\n")
            cargo.chmod(0o755)
            args = pathlib.Path(tmp) / "args"
            env = dict(os.environ, PATH=tmp + os.pathsep + os.environ["PATH"], AUDIT_ARGS=str(args))
            env.pop("GITHUB_TOKEN", None)
            for status in (0, 1, 2, 101):
                with self.subTest(status=status):
                    result = subprocess.run(["bash", str(ROOT / "scripts/ci-security-audit")],
                                            cwd=tmp, env=dict(env, AUDIT_STATUS=str(status)))
                    self.assertEqual(result.returncode, status)
                    self.assertEqual(args.read_text().splitlines(),
                                     ["audit", "--file", str(ROOT / "Cargo.lock")])

    def test_workflow_keeps_gate_required_and_read_only(self):
        import yaml
        workflow = yaml.safe_load((ROOT / ".github/workflows/ci.yml").read_text())
        job = workflow["jobs"]["check"]
        steps = job["steps"]
        audit = next(s for s in steps if s.get("name") == "Audit locked dependencies")
        self.assertEqual(audit["run"], "bash scripts/ci-security-audit")
        self.assertNotIn("continue-on-error", audit)
        self.assertNotIn("continue-on-error", job)
        self.assertNotIn("if", audit)
        self.assertEqual(job.get("permissions", workflow["permissions"]), {"contents": "read"})
        self.assertTrue(any(s.get("run") == "cargo install cargo-audit --version 0.22.2 --locked" for s in steps))
        self.assertTrue(any(s.get("run") == "python3 scripts/tests/test_ci_security_audit.py" for s in steps))
        self.assertFalse(any("rustsec/audit-check" in s.get("uses", "") for s in steps))


if __name__ == "__main__":
    unittest.main()
