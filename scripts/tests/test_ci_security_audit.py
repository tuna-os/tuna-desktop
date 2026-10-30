#!/usr/bin/env python3
"""Exercise audit exit propagation without a network or GitHub token."""
import os
import pathlib
import subprocess
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]


def workflow_check():
    import yaml
    workflow = yaml.safe_load((ROOT / ".github/workflows/ci.yml").read_text())
    return workflow, workflow["jobs"]["check"]


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
                                            cwd=tmp, env=dict(env, AUDIT_STATUS=str(status)), timeout=60)
                    self.assertEqual(result.returncode, status)
                    self.assertEqual(args.read_text().splitlines(),
                                     ["audit", "--file", str(ROOT / "Cargo.lock")])

    def test_inline_workflow_audits_older_exact_heads_and_preserves_failures(self):
        _, job = workflow_check()
        command = next(s["run"] for s in job["steps"] if s.get("name") == "Audit locked dependencies")
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            candidate = root / "older candidate"
            candidate.mkdir()
            (candidate / "Cargo.lock").write_text("# Exact candidate lockfile\n")
            cargo = root / "cargo"
            cargo.write_text("#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$AUDIT_ARGS\"\nexit \"$AUDIT_STATUS\"\n")
            cargo.chmod(0o755)
            args = root / "args"
            env = dict(os.environ, PATH=tmp + os.pathsep + os.environ["PATH"],
                       AUDIT_ARGS=str(args), GITHUB_WORKSPACE=str(candidate))
            env.pop("GITHUB_TOKEN", None)
            for status in (0, 1, 2, 101):
                with self.subTest(status=status):
                    result = subprocess.run(["bash", "-euo", "pipefail", "-c", command], cwd=tmp,
                                            env=dict(env, AUDIT_STATUS=str(status)), timeout=60)
                    self.assertEqual(result.returncode, status)
                    self.assertEqual(args.read_text().splitlines(),
                                     ["audit", "--file", str(candidate / "Cargo.lock")])

    def test_optional_self_test_only_tolerates_absence_not_failure(self):
        _, job = workflow_check()
        command = next(s["run"] for s in job["steps"] if s.get("name") == "Test the tokenless security gate")
        with tempfile.TemporaryDirectory() as tmp:
            candidate = pathlib.Path(tmp)
            argv = ["bash", "-euo", "pipefail", "-c", command]
            absent = subprocess.run(argv, cwd=tmp, text=True, capture_output=True, timeout=60)
            self.assertEqual(absent.returncode, 0)
            self.assertIn("locked dependency audit remains required", absent.stdout)
            tests = candidate / "scripts/tests"
            tests.mkdir(parents=True)
            for status in (0, 1, 2):
                with self.subTest(status=status):
                    (tests / "test_ci_security_audit.py").write_text(f"raise SystemExit({status})\n")
                    result = subprocess.run(argv, cwd=tmp, text=True, capture_output=True, timeout=60)
                    self.assertEqual(result.returncode, status)
                    self.assertNotIn("::notice::", result.stdout)

    def test_workflow_keeps_gate_required_and_read_only(self):
        workflow, job = workflow_check()
        steps = job["steps"]
        audit = next(s for s in steps if s.get("name") == "Audit locked dependencies")
        self.assertEqual(audit["run"], 'cargo audit --file "$GITHUB_WORKSPACE/Cargo.lock"')
        self.assertNotIn("continue-on-error", audit)
        self.assertNotIn("continue-on-error", job)
        self.assertNotIn("if", audit)
        self.assertEqual(job.get("permissions", workflow["permissions"]), {"contents": "read"})
        self.assertTrue(any(s.get("run") == "cargo install cargo-audit --version 0.22.2 --locked" for s in steps))
        self.assertTrue(any(s.get("name") == "Test the tokenless security gate" for s in steps))
        self.assertFalse(any("rustsec/audit-check" in s.get("uses", "") for s in steps))
        checkout = next(s for s in steps if s.get("uses", "").startswith("actions/checkout@"))
        self.assertFalse(checkout["with"]["persist-credentials"])
        self.assertEqual(checkout["with"]["ref"], "${{ github.event.pull_request.head.sha || github.sha }}")
        events = workflow.get("on", workflow.get(True))
        self.assertIn("pull_request", events)
        self.assertNotIn("pull_request_target", events)


if __name__ == "__main__":
    unittest.main()
