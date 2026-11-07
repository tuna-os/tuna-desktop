import copy
import importlib.util
import json
from pathlib import Path
import os
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).parents[2]
spec = importlib.util.spec_from_file_location("comparison", ROOT / "scripts/lib/grid_cancel_comparison.py")
contract = importlib.util.module_from_spec(spec)
spec.loader.exec_module(contract)
runner_spec = importlib.util.spec_from_file_location("runner", ROOT / "scripts/tuna-grid-cancel-comparison", loader=__import__("importlib.machinery", fromlist=["SourceFileLoader"]).SourceFileLoader("runner", str(ROOT / "scripts/tuna-grid-cancel-comparison")))
runner = importlib.util.module_from_spec(runner_spec)
runner_spec.loader.exec_module(runner)


class Identity(unittest.TestCase):
    def setUp(self):
        self.sha = "a" * 40
        self.run = dict(id=100, run_attempt=1, repository=dict(full_name=contract.REPO),
                        head_sha=self.sha, path=contract.WORKFLOW, event="workflow_dispatch",
                        status="in_progress", conclusion=None)
        self.jobs = [dict(id=200, name=contract.JOB, status="in_progress", conclusion=None,
                          head_sha=self.sha, run_id=100, run_attempt=1),
                     dict(id=201, name="check", status="completed", conclusion="success",
                          head_sha=self.sha, run_id=100, run_attempt=1)]
        self.commit = dict(sha=self.sha, tree=dict(sha="b" * 40))

    def validate(self, run=None, jobs=None, commit=None):
        return contract.validate_identity(self.run if run is None else run,
            self.jobs if jobs is None else jobs, self.commit if commit is None else commit,
            self.sha, 100, 1, "workflow_dispatch", self.sha, self.sha)

    def test_running_own_job_is_admitted_without_fabricated_terminal_success(self):
        self.assertEqual(self.validate()["job_id"], 200)

    def test_pr_merge_source_is_distinct_from_api_event_head(self):
        event_head = "c" * 40
        run = dict(self.run, event="pull_request", head_sha=event_head)
        jobs = [dict(job, head_sha=event_head) for job in self.jobs]
        for status in ("in_progress", "queued"):
            result = contract.validate_identity(dict(run, status=status), jobs, self.commit,
                self.sha, 100, 1, "pull_request", event_head, self.sha)
            self.assertEqual(result["head"], self.sha)
            self.assertEqual(result["event_head"], event_head)
            self.assertEqual(result["tree"], self.commit["tree"]["sha"])
            self.assertEqual((result["job_id"], result["check_job_id"]), (200, 201))

    def test_pr_merge_rejects_checked_checkout_commit_and_api_head_mismatches(self):
        event_head = "c" * 40
        run = dict(self.run, event="pull_request", head_sha=event_head)
        jobs = [dict(job, head_sha=event_head) for job in self.jobs]
        cases = [
            (run, jobs, self.commit, self.sha, event_head),
            (run, jobs, self.commit, event_head, self.sha),
            (run, jobs, dict(self.commit, sha=event_head), self.sha, self.sha),
            (dict(run, head_sha=self.sha), jobs, self.commit, self.sha, self.sha),
        ]
        for index in range(2):
            wrong_jobs = copy.deepcopy(jobs)
            wrong_jobs[index]["head_sha"] = self.sha
            cases.append((run, wrong_jobs, self.commit, self.sha, self.sha))
        for bad_run, bad_jobs, commit, checkout, checked in cases:
            with self.subTest(run=bad_run, jobs=bad_jobs, checkout=checkout, checked=checked), self.assertRaises(ValueError):
                contract.validate_identity(bad_run, bad_jobs, commit, checkout, 100, 1,
                    "pull_request", event_head, checked)
        with self.assertRaises(ValueError):
            contract.validate_identity(run, jobs, self.commit, self.sha, 100, 1,
                "pull_request", self.sha, self.sha)

    def test_event_and_checked_shas_are_explicit_full_hex_values(self):
        for bad in (None, "", "a" * 39, "a" * 41, "g" * 40, 100):
            for event_head, checked in ((bad, self.sha), (self.sha, bad)):
                with self.subTest(event_head=event_head, checked=checked), self.assertRaises(ValueError):
                    contract.validate_identity(self.run, self.jobs, self.commit, self.sha, 100, 1,
                        "workflow_dispatch", event_head, checked)
        with self.assertRaises(TypeError):
            contract.validate_identity(self.run, self.jobs, self.commit, self.sha, 100, 1, "workflow_dispatch")

    def test_queued_run_requires_exact_running_job_and_green_check(self):
        run = dict(self.run, status="queued")
        self.assertEqual(self.validate(run=run)["job_id"], 200)
        for index, key, value in ((0, "status", "queued"), (0, "status", "completed"),
                                 (0, "conclusion", "success"), (0, "head_sha", "c" * 40),
                                 (0, "run_id", 101), (0, "run_attempt", 2),
                                 (1, "conclusion", "failure"), (1, "conclusion", None),
                                 (1, "status", "queued"), (1, "head_sha", "c" * 40)):
            jobs = copy.deepcopy(self.jobs)
            jobs[index][key] = value
            with self.subTest(index=index, key=key), self.assertRaises(ValueError):
                self.validate(run=run, jobs=jobs)
        for jobs in ([], self.jobs[:1], self.jobs + [self.jobs[0]], self.jobs + [self.jobs[1]]):
            with self.subTest(jobs=jobs), self.assertRaises(ValueError):
                self.validate(run=run, jobs=jobs)
        for status in ("completed", "waiting", "pending", "requested", "unknown", None):
            with self.subTest(status=status), self.assertRaises(ValueError):
                self.validate(run=dict(run, status=status))
        with self.assertRaises(ValueError):
            self.validate(run=run, commit=dict(self.commit, sha="c" * 40))

    def test_wrong_run_source_event_workflow_and_attempt_fail(self):
        for key, value in (("id", 101), ("run_attempt", 2), ("head_sha", "c" * 40),
                           ("path", "wrong"), ("event", "push"), ("status", "completed"),
                           ("conclusion", "success"), ("repository", dict(full_name="foreign/repo"))):
            with self.subTest(key=key), self.assertRaises(ValueError):
                self.validate(run=dict(self.run, **{key: value}))

    def test_schedule_event_admitted(self):
        run = dict(self.run, event="schedule")
        self.assertEqual(contract.validate_identity(run, self.jobs, self.commit, self.sha, 100, 1, "schedule", self.sha, self.sha)["job_id"], 200)

    def test_unsupported_event_rejected(self):
        run = dict(self.run, event="fork")
        with self.assertRaises(ValueError):
            contract.validate_identity(run, self.jobs, self.commit, self.sha, 100, 1, "fork", self.sha, self.sha)

    def test_jobs_fail_closed(self):
        for index, key, value in ((0, "name", "foreign"), (0, "status", "queued"),
                                 (0, "head_sha", "c" * 40), (0, "run_id", 101),
                                 (0, "run_attempt", 2), (1, "conclusion", "failure"),
                                 (1, "status", "in_progress"), (1, "head_sha", "c" * 40)):
            jobs = copy.deepcopy(self.jobs)
            jobs[index][key] = value
            with self.subTest(index=index, key=key), self.assertRaises(ValueError): self.validate(jobs=jobs)

    def test_missing_and_duplicate_job_fail(self):
        for jobs in ([], self.jobs[:1], self.jobs + [self.jobs[0]], self.jobs + [self.jobs[1]]):
            with self.subTest(jobs=jobs), self.assertRaises(ValueError): self.validate(jobs=jobs)

    def test_commit_source_mismatch_fails(self):
        with self.assertRaises(ValueError): self.validate(commit=dict(self.commit, sha="c" * 40))

    def test_mismatch_preserves_observed_public_fields_without_extra_payload(self):
        run = dict(self.run, status="completed", private_payload="excluded fixture value")
        with self.assertRaises(ValueError) as error:
            self.validate(run=run)
        receipt = json.loads(str(error.exception).split(": ", 1)[1])
        self.assertEqual(receipt["expected"]["status"], ["in_progress", "queued"])
        self.assertEqual(receipt["observed"]["status"], "completed")
        self.assertEqual(receipt["observed"]["head_sha"], self.sha)
        self.assertEqual(receipt["observed"]["commit_sha"], self.sha)
        self.assertNotIn("private_payload", receipt["observed"])

    def test_current_identity_admits_exact_checked_merge_and_api_event_head(self):
        event_head = "c" * 40
        source = dict(sha=self.sha, tree=self.commit["tree"]["sha"], lock_sha256="lock")
        run = dict(self.run, event="pull_request", head_sha=event_head)
        jobs = [dict(job, head_sha=event_head) for job in self.jobs]
        environment = dict(GITHUB_REPOSITORY=contract.REPO, GITHUB_JOB=contract.JOB,
            GITHUB_RUN_ID="100", GITHUB_RUN_ATTEMPT="1", GITHUB_EVENT_NAME="pull_request",
            GITHUB_SHA=self.sha, TUNA_CI_EVENT_HEAD_SHA=event_head, TUNA_CI_CHECKED_SHA=self.sha)
        responses = {
            "actions/runs/100/attempts/1/jobs?per_page=100": dict(total_count=2, jobs=jobs),
            "actions/runs/100": run,
            f"git/commits/{self.sha}": self.commit,
        }
        with patch.dict(os.environ, environment, clear=True), \
                patch.object(contract, "source_tuple", return_value=source), \
                patch.object(contract, "api", side_effect=responses.__getitem__):
            result = contract.current_identity(ROOT)
        self.assertEqual((result["head"], result["event_head"]), (self.sha, event_head))

    def test_current_identity_requires_both_env_shas_and_matching_checkout_tree(self):
        source = dict(sha=self.sha, tree=self.commit["tree"]["sha"], lock_sha256="lock")
        environment = dict(GITHUB_REPOSITORY=contract.REPO, GITHUB_JOB=contract.JOB,
            GITHUB_RUN_ID="100", GITHUB_RUN_ATTEMPT="1", GITHUB_EVENT_NAME="workflow_dispatch",
            GITHUB_SHA=self.sha, TUNA_CI_EVENT_HEAD_SHA=self.sha, TUNA_CI_CHECKED_SHA=self.sha)
        responses = {
            "actions/runs/100/attempts/1/jobs?per_page=100": dict(total_count=2, jobs=self.jobs),
            "actions/runs/100": self.run,
            f"git/commits/{self.sha}": self.commit,
        }
        for name in ("TUNA_CI_EVENT_HEAD_SHA", "TUNA_CI_CHECKED_SHA", "GITHUB_SHA"):
            missing = dict(environment)
            del missing[name]
            with self.subTest(missing=name), patch.dict(os.environ, missing, clear=True), \
                    patch.object(contract, "source_tuple", return_value=source), \
                    patch.object(contract, "api") as api, self.assertRaises(KeyError):
                contract.current_identity(ROOT)
            api.assert_not_called()
        for name in ("GITHUB_SHA", "TUNA_CI_CHECKED_SHA"):
            mismatched = dict(environment, **{name: "c" * 40})
            with self.subTest(mismatched=name), patch.dict(os.environ, mismatched, clear=True), \
                    patch.object(contract, "source_tuple", return_value=source), \
                    patch.object(contract, "api") as api, self.assertRaises(ValueError):
                contract.current_identity(ROOT)
            api.assert_not_called()
        for checkout in (dict(source, sha="c" * 40), dict(source, tree="c" * 40)):
            current_responses = dict(responses)
            current_responses[f"git/commits/{checkout['sha']}"] = self.commit
            with self.subTest(checkout=checkout), patch.dict(os.environ, environment, clear=True), \
                    patch.object(contract, "source_tuple", return_value=checkout), \
                    patch.object(contract, "api", side_effect=current_responses.__getitem__), self.assertRaises(ValueError):
                contract.current_identity(ROOT)

    def test_current_identity_refuses_incomplete_bounded_job_page(self):
        environment = dict(GITHUB_REPOSITORY=contract.REPO, GITHUB_JOB=contract.JOB,
            GITHUB_RUN_ID="100", GITHUB_RUN_ATTEMPT="1", GITHUB_EVENT_NAME="workflow_dispatch",
            GITHUB_SHA=self.sha, TUNA_CI_EVENT_HEAD_SHA=self.sha, TUNA_CI_CHECKED_SHA=self.sha)
        for page in (dict(total_count=101, jobs=self.jobs), dict(jobs=self.jobs)):
            with self.subTest(page=page), patch.dict(os.environ, environment, clear=True), \
                    patch.object(contract, "source_tuple", return_value=dict(sha=self.sha)), \
                    patch.object(contract, "api", return_value=page), self.assertRaises(ValueError):
                contract.current_identity(ROOT)


class BuildRoles(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root, self.target = Path(self.tmp.name) / "source", Path(self.tmp.name) / "target"
        self.root.mkdir()
        self.target.mkdir()
        self.messages = []
        for name, (package, kind, src) in contract.ROLES.items():
            binary = self.target / name
            binary.write_bytes((name + " owned fixture").encode())
            self.messages.append(dict(reason="compiler-artifact", executable=str(binary),
                profile=dict(test=False), package_id=f"path+{(self.root / Path(*Path(src).parts[:2])).as_uri()}#{package}@0.1.0",
                target=dict(name=name, kind=[kind], src_path=str(self.root / src))))

    def tearDown(self): self.tmp.cleanup()

    def test_exact_roles_positive(self):
        self.assertEqual(set(contract.select_artifacts(self.messages, self.root, self.target)), set(contract.ROLES))

    def test_missing_role_fails(self):
        with self.assertRaises(ValueError): contract.select_artifacts(self.messages[:-1], self.root, self.target)

    def test_wrong_test_kind_package_source_and_binary_location_fail(self):
        cases = []
        for mutation in (dict(profile=dict(test=True)), dict(profile={}), dict(package_id="wrong-package@0.1.0"),
                         dict(package_id="path+file:///foreign/crate#tuna-compositor@0.1.0"),
                         dict(target=dict(name="tuna-compositor", kind=["example"], src_path=str(self.root / "crates/compositor/src/main.rs"))),
                         dict(target=dict(name="tuna-compositor", kind=["bin"], src_path=str(self.root / "foreign.rs")))):
            cases.append(dict(self.messages[0], **mutation))
        foreign = self.root / "foreign-bin"
        foreign.write_bytes(b"foreign")
        cases.append(dict(self.messages[0], executable=str(foreign)))
        for message in cases:
            with self.subTest(message=message), self.assertRaises(ValueError):
                contract.select_artifacts([message] + self.messages[1:], self.root, self.target)

    def test_identical_repeat_only_can_coalesce(self):
        contract.select_artifacts(self.messages + [self.messages[0]], self.root, self.target)
        foreign = self.target / "alternate"
        foreign.write_bytes(b"alternate")
        with self.assertRaises(ValueError):
            contract.select_artifacts(self.messages + [dict(self.messages[0], executable=str(foreign))], self.root, self.target)


class OldNegative(unittest.TestCase):
    def setUp(self):
        self.before = dict(client_dnd_active=True, locked=False, overview_open=True,
                           overview_app_grid=True, rendered_frames=1)
        self.after = dict(self.before, overview_open=False, rendered_frames=2)

    def test_actual_discriminator_positive(self): contract.old_negative(self.before, self.after)

    def test_missing_wrong_stale_or_fixed_result_never_counts_old_negative(self):
        for key, value in (("client_dnd_active", False), ("locked", True), ("overview_open", True),
                           ("rendered_frames", 1), ("rendered_frames", True)):
            with self.subTest(key=key), self.assertRaises(ValueError):
                contract.old_negative(self.before, dict(self.after, **{key: value}))
        for state in ({}, [], None):
            with self.subTest(state=state), self.assertRaises(ValueError): contract.old_negative(self.before, state)

    def test_invalid_before_drag_rejects(self):
        for key, value in (("client_dnd_active", False), ("overview_open", False), ("overview_app_grid", False)):
            with self.subTest(key=key), self.assertRaises(ValueError):
                contract.old_negative(dict(self.before, **{key: value}), self.after)


class AssertionVerdict(unittest.TestCase):
    def test_exact_role_positive(self):
        contract.validate_assertions(["G-PANEL pass actual", "G-GRID-DRAG-OLD-NEG pass button still held"], "old")
        contract.validate_assertions(["G-PANEL pass actual", "G-GRID-DRAG-CANCEL pass before release"], "fixed")

    def test_failure_elsewhere_never_old_negative(self):
        for lines in (["G-PANEL fail missing", "G-GRID-DRAG-OLD-NEG pass false"],
                      ["G-GRID-DRAG-CANCEL pass wrong role"], [],
                      ["G-GRID-DRAG-OLD-NEG pass a", "G-GRID-DRAG-OLD-NEG pass b"],
                      ["G-GRID-DRAG-OLD-NEG pass-prefix wrong"], [None]):
            with self.subTest(lines=lines), self.assertRaises(ValueError): contract.validate_assertions(lines, "old")

    def test_fixed_never_accepts_old_close(self):
        with self.assertRaises(ValueError):
            contract.validate_assertions(["G-GRID-DRAG-CANCEL pass fake", "G-GRID-DRAG-OLD-NEG pass old"], "fixed")


class CliGuard(unittest.TestCase):
    def test_old_negative_without_admitted_only_mode_fails_before_bootstrap(self):
        import os
        for flags in (["--expect-grid-cancel-old-negative"], ["--grid-cancel-only"],
                      ["--grid-cancel-admission", "/missing"],
                      ["--grid-cancel-only", "--installed-package", "--grid-cancel-admission", "/missing"]):
            result = subprocess.run([str(ROOT / "scripts/tuna-gtk-shell-proof"), *flags],
                env=dict(os.environ, TUNA_PROOF_PRIVATE_BUS="1"), text=True, capture_output=True, timeout=5)
            with self.subTest(flags=flags):
                self.assertNotEqual(result.returncode, 0)
                self.assertTrue("require" in result.stderr)


class OwnedCleanup(unittest.TestCase):
    def test_real_owned_python_child_cleanup(self):
        # Python process fixture only; no Cargo, GUI or product runtime.
        import os
        import uuid
        token = str(uuid.uuid4())
        child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(30)"],
                                 env=dict(os.environ, TUNA_GRID_COMPARISON_OWNER=token), start_new_session=True)
        try:
            self.assertTrue(runner.cleanup_owned(child, token))
            self.assertFalse(runner.owned_processes(token))
        finally:
            child.wait(timeout=3)


if __name__ == "__main__": unittest.main()
