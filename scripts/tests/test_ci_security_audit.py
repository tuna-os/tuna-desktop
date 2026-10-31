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
        install = next(s for s in steps if s.get("name") == "Install pinned cargo-audit")
        self.assertEqual(install["uses"], "./.ci-workflow-tools/.github/actions/ci-tool")
        self.assertEqual(install["with"], {"tool": "cargo-audit", "version": "0.22.2"})
        self.assertNotIn("if", install)
        self.assertNotIn("continue-on-error", install)
        self.assertTrue(any(s.get("name") == "Test the tokenless security gate" for s in steps))
        self.assertFalse(any("rustsec/audit-check" in s.get("uses", "") for s in steps))
        checkout = next(s for s in steps if s.get("uses", "").startswith("actions/checkout@"))
        self.assertFalse(checkout["with"]["persist-credentials"])
        self.assertEqual(checkout["with"]["ref"], "${{ github.event.pull_request.head.sha || github.sha }}")
        events = workflow.get("on", workflow.get(True))
        self.assertIn("pull_request", events)
        self.assertNotIn("pull_request_target", events)


class WorkflowToolCacheTests(unittest.TestCase):
    def action(self):
        import yaml
        return yaml.safe_load((ROOT / ".github/actions/ci-tool/action.yml").read_text())

    def test_consumers_load_workflow_revision_without_replacing_candidate(self):
        import yaml
        for filename in ("ci.yml", "measured-coverage.yml"):
            workflow = yaml.safe_load((ROOT / ".github/workflows" / filename).read_text())
            for name, job in workflow["jobs"].items():
                consumers = [s for s in job["steps"]
                             if s.get("uses", "").endswith("/.github/actions/ci-tool")]
                if not consumers:
                    continue
                with self.subTest(workflow=filename, job=name):
                    tooling = next(s for s in job["steps"]
                                   if s.get("name") == "Check out workflow-owned CI tooling")
                    settings = tooling["with"]
                    self.assertEqual(settings["repository"], "${{ github.repository }}")
                    self.assertEqual(settings["ref"], "${{ github.workflow_sha }}")
                    self.assertFalse(settings["persist-credentials"])
                    self.assertIn("/.github/actions/ci-tool/", settings["sparse-checkout"])
                    self.assertIn("/scripts/ci-install-gtk4-layer-shell", settings["sparse-checkout"])
                    for consumer in consumers:
                        self.assertEqual(consumer["uses"],
                                         f'./{settings["path"]}/.github/actions/ci-tool')
                        self.assertLess(job["steps"].index(tooling), job["steps"].index(consumer))
                    if name in ("check", "grid-cancel-comparison", "gtk-shell"):
                        candidate = job["steps"][0]["with"]
                        self.assertEqual(candidate["ref"],
                                         "${{ github.event.pull_request.head.sha || github.sha }}")
                        self.assertNotIn("path", candidate)

    def test_private_checkout_is_excluded_but_untracked_candidate_source_remains_visible(self):
        exclude_step = next(s for s in self.action()["runs"]["steps"]
                            if s.get("name") == "Exclude private workflow tooling from candidate scans")
        with tempfile.TemporaryDirectory() as tmp:
            candidate = pathlib.Path(tmp)

            def git(*args, cwd=candidate):
                return subprocess.run(["git", *args], cwd=cwd, check=True, text=True,
                                      capture_output=True, timeout=60).stdout

            git("init", "--quiet")
            tracked = candidate / "tracked.py"
            tracked.write_text("print('candidate')\n")
            git("add", tracked.name)
            genuine = candidate / "untracked-candidate.py"
            genuine.write_text("print('untracked candidate')\n")
            ignored = candidate / "existing-private.txt"
            ignored.write_text("already excluded\n")
            exclude = candidate / ".git/info/exclude"
            prior_excludes = "# Preserve this existing local rule\n/existing-private.txt\n"
            exclude.write_text(prior_excludes)
            tooling = candidate / ".ci-workflow-tools"
            tooling.mkdir()
            git("init", "--quiet", cwd=tooling)
            (tooling / "action.yml").write_text("workflow-owned fixture\n")
            git("add", "action.yml", cwd=tooling)
            git("-c", "user.name=CI fixture", "-c", "user.email=fixture@example.invalid",
                "-c", "commit.gpgsign=false", "commit", "--quiet", "-m", "tooling fixture", cwd=tooling)
            listing_args = ("ls-files", "--cached", "--others", "--exclude-standard")
            self.assertIn(".ci-workflow-tools/", git(*listing_args).splitlines())
            subprocess.run(["bash", "-euo", "pipefail", "-c", exclude_step["run"]],
                           cwd=tooling, env=dict(os.environ, GITHUB_WORKSPACE=str(candidate)),
                           check=True, timeout=60)
            paths = git(*listing_args).splitlines()
            self.assertIn(tracked.name, paths)
            self.assertIn(genuine.name, paths)
            self.assertNotIn(ignored.name, paths)
            self.assertFalse(any(p.startswith(".ci-workflow-tools/") for p in paths))
            self.assertTrue(exclude.read_text().startswith(prior_excludes))
            # Match the rename scanner's read of each Git-enumerated path:
            # a nested checkout directory must never reach this file read.
            self.assertEqual({path: (candidate / path).read_bytes() for path in paths},
                             {tracked.name: tracked.read_bytes(), genuine.name: genuine.read_bytes()})

    def test_cold_gtk_uses_workflow_installer_not_older_candidate(self):
        _, job = workflow_check()
        consumer = next(s for s in job["steps"]
                        if s.get("with", {}).get("tool") == "gtk4-layer-shell")
        install = next(s for s in self.action()["runs"]["steps"]
                       if s.get("name") == "Install exact tool on cache miss")
        with tempfile.TemporaryDirectory() as tmp:
            candidate = pathlib.Path(tmp)
            (candidate / "scripts").mkdir()
            (candidate / "scripts/ci-install-gtk4-layer-shell").write_text("exit 99\n")
            action_path = candidate / ".ci-workflow-tools/.github/actions/ci-tool"
            action_path.mkdir(parents=True)
            installer = candidate / ".ci-workflow-tools/scripts/ci-install-gtk4-layer-shell"
            installer.parent.mkdir()
            installer.write_text('#!/bin/sh\nmkdir -p "$TUNA_GTK_DESTDIR"\n'
                                 'printf workflow-owned > "$TUNA_GTK_DESTDIR/installed"\n')
            installer.chmod(0o755)
            stage = candidate / "stage"
            env = dict(os.environ, TOOL=consumer["with"]["tool"],
                       VERSION=consumer["with"]["version"], TOOL_ROOT=str(stage),
                       GITHUB_ACTION_PATH=str(action_path))
            subprocess.run(["bash", "-euo", "pipefail", "-c", install["run"]],
                           cwd=candidate, env=env, check=True, timeout=60)
            self.assertEqual((stage / "installed").read_text(), "workflow-owned")

    def test_identity_binds_workflow_bytes_not_candidate_copies(self):
        _, job = workflow_check()
        consumer = next(s for s in job["steps"] if s.get("name") == "Install pinned cargo-audit")
        identity = next(s for s in self.action()["runs"]["steps"]
                        if s.get("name") == "Identify exact tool environment")
        with tempfile.TemporaryDirectory() as tmp:
            candidate = pathlib.Path(tmp)
            action_path = candidate / ".ci-workflow-tools/.github/actions/ci-tool"
            action_path.mkdir(parents=True)
            metadata = action_path / "action.yml"
            metadata.write_text("workflow action\n")
            installer = candidate / ".ci-workflow-tools/scripts/ci-install-gtk4-layer-shell"
            installer.parent.mkdir()
            installer.write_text("workflow installer\n")
            binaries = candidate / "bin"
            binaries.mkdir()
            for command in ("rustc", "cargo", "dpkg-query"):
                stub = binaries / command
                stub.write_text(f"#!/bin/sh\nprintf '%s\\n' '{command}-identity'\n")
                stub.chmod(0o755)
            output = candidate / "output"
            env = dict(os.environ, PATH=str(binaries) + os.pathsep + os.environ["PATH"],
                       TOOL=consumer["with"]["tool"], VERSION=consumer["with"]["version"],
                       RUNNER_TEMP=tmp, GITHUB_OUTPUT=str(output),
                       GITHUB_ACTION_PATH=str(action_path))

            def key():
                output.write_text("")
                subprocess.run(["bash", "-euo", "pipefail", "-c", identity["run"]],
                               cwd=candidate, env=env, check=True, timeout=60)
                return dict(line.split("=", 1) for line in output.read_text().splitlines())["key"]

            original = key()
            candidate_script = candidate / "scripts/ci-install-gtk4-layer-shell"
            candidate_script.parent.mkdir()
            candidate_script.write_text("older candidate installer\n")
            self.assertEqual(key(), original)
            installer.write_text("changed workflow installer\n")
            changed_installer = key()
            self.assertNotEqual(changed_installer, original)
            metadata.write_text("changed workflow action\n")
            self.assertNotEqual(key(), changed_installer)


if __name__ == "__main__":
    unittest.main()
