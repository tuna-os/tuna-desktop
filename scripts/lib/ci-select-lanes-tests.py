#!/usr/bin/env python3
"""ci-select-lanes picks real coverage and stays in step with ci.yml."""
import importlib.machinery
import importlib.util
import json
import subprocess
import unittest
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parents[2]
_loader = importlib.machinery.SourceFileLoader("ci_select_lanes", str(ROOT / "scripts/ci-select-lanes"))
_spec = importlib.util.spec_from_loader(_loader.name, _loader)
m = importlib.util.module_from_spec(_spec)
_loader.exec_module(m)

def pr(*changed, run=0, size=2):
    return m.select("pull_request", run, size, list(changed))[0]


class Selection(unittest.TestCase):
    def test_main_schedule_and_dispatch_run_every_lane(self):
        for event in ("push", "schedule", "workflow_dispatch"):
            self.assertEqual(m.select(event, 7, 2, [])[0], m.LANES)

    def test_shared_inputs_run_every_lane(self):
        for path in (".github/workflows/ci.yml", "Cargo.lock", "Cargo.toml",
                     "third-party/smithay/src/lib.rs", "scripts/ci-select-lanes",
                     "scripts/tuna-nested", ".github/actions/ci-tool/action.yml"):
            self.assertEqual(pr("docs/x.md", path), m.LANES, path)

    def test_docs_only_runs_no_lane(self):
        self.assertEqual(pr("README.md", "docs/perf/x.md", ".github/workflows/fuzz.yml",
                            "scripts/lib/ledger-tests.py", "fuzz/Cargo.toml"), [])

    def test_code_runs_gtk_shell_and_a_rotating_slice_with_producers(self):
        seen = set()
        for run in range(len(m.ROTATION)):
            lanes = pr("crates/compositor/src/lib.rs", run=run)
            self.assertIn("gtk-shell", lanes)
            rotated = set(m.rotation(run, 2))
            self.assertEqual(len(rotated), 2)
            self.assertTrue(rotated <= set(lanes))
            for lane in lanes:
                self.assertTrue(set(m.NEEDS.get(lane, [])) <= set(lanes), (run, lane))
            seen |= rotated
        self.assertEqual(seen, set(m.ROTATION))

    def test_rotation_size_is_clamped(self):
        self.assertEqual(m.rotation(3, 0), [])
        self.assertEqual(sorted(m.rotation(3, 99)), sorted(m.ROTATION))

    def test_owned_paths_run_their_lane_without_rotation(self):
        self.assertEqual(pr("scripts/tuna-scroll-proof"), ["scroll-proof"])
        self.assertEqual(pr("docs/walkthrough.tsv"), ["gtk-shell"])
        self.assertEqual(pr("packaging/marlin/vm-lane/Containerfile"),
                         ["arch-package", "marlin-image", "marlin-vm"])
        self.assertEqual(pr("docs/parity-ledger.md"),
                         ["journey", "drm-smoke", "gtk-shell", "orca-speech",
                          "arch-package", "marlin-vm", "parity-ledger"])
        self.assertEqual(pr("scripts/tuna-orca-speech-proof"), ["orca-speech"])
        self.assertEqual(pr("scripts/lib/tuna-orca-speech.py"), ["orca-speech"])


    def test_transition_policy_and_suite_always_run_journey(self):
        for path in ('scripts/lib/transition-package-policy.py',
                     'scripts/lib/transition-package-policy-tests.py'):
            for run in (0, 4, 19):
                with self.subTest(path=path, run=run):
                    self.assertEqual(pr(path, run=run), ['journey'])

    def test_legacy_lifecycle_and_native_pam_helper_always_run_journey(self):
        for path in ('scripts/lib/legacy-upgrade-lifecycle-tests.py',
                     'scripts/lib/legacy-upgrade-pam-probe.c',
                     'scripts/lib/legacy-upgrade-evidence.py',
                     'scripts/lib/legacy-upgrade-evidence-tests.py'):
            for run in (0, 4, 19):
                with self.subTest(path=path, run=run):
                    self.assertEqual(pr(path, run=run), ['journey'])

    def test_transition_assets_run_every_package_consumer_without_rotation(self):
        # tuna-rename: keep-begin
        for path in ('share/compat/wayland-sessions/roost.desktop',
                     'share/compat/pam.d/roost-lock'):
            self.assertEqual(pr(path, run=9), [
                'journey', 'portal-security', 'remote-security', 'niri-parity',
                'arch-package', 'marlin-image', 'marlin-vm'], path)
        # tuna-rename: keep-end

    def test_shipped_pam_conffile_always_runs_journey_and_package_consumers(self):
        # /etc/pam.d/tuna-lock is a dpkg conffile and an Arch backup file:
        # any byte change prompts upgraders, which only journey's disposable
        # Debian lifecycle catches (#899 changed it in a run whose rotation
        # skipped journey and turned it red for every later PR).
        for run in range(len(m.ROTATION)):
            with self.subTest(run=run):
                self.assertEqual(pr("share/pam.d/tuna-lock", run=run), [
                    'fprint-phase-proof', 'journey', 'grid-cancel-comparison', 'gtk-shell',
                    'portal-security', 'remote-security', 'niri-parity', 'arch-package',
                    'marlin-image', 'marlin-vm'])
        # Control: other shipped runtime data still rotates journey.
        self.assertNotIn('journey', pr("share/applications/tuna-settings.desktop", run=1))

    def test_tooling_of_other_workflows_runs_no_lane(self):
        for path in ("justfile", "scripts/tuna-corral-journey", "scripts/lib/corral_raw.py",
                     "scripts/tuna-variant-qualification", "scripts/lib/vm_release.py"):
            self.assertEqual(pr(path, "docs/x.md", run=3), [], path)
        # Control: a script that is not listed still rotates lanes.
        self.assertIn("gtk-shell", pr("scripts/tuna-journey-helper", run=3))

    def test_pull_requests_never_rotate_the_push_only_image_lane(self):
        self.assertNotIn("marlin-image", m.ROTATION)
        for run in range(len(m.ROTATION)):
            self.assertNotIn("marlin-image", pr("crates/compositor/src/lib.rs", run=run))

class WorkflowEdits(unittest.TestCase):
    """An edit to ci.yml selects the lanes whose job changed, not every lane."""

    BASE = """
on: {pull_request: {}}
x-runner: &runner ubuntu-latest
jobs:
  select-lanes: {runs-on: *runner, steps: [{run: a}]}
  check: {runs-on: *runner, steps: [{run: a}]}
  journey: {runs-on: *runner, steps: [{run: a}]}
  drm-smoke: {runs-on: *runner, steps: [{run: a}]}
  gdm51-native-package: {runs-on: *runner, steps: [{run: a}]}
  ci-gate: {runs-on: *runner, steps: [{run: a}]}
"""

    def lanes(self, old, new):
        return m.workflow_lanes(self.BASE, self.BASE.replace(old, new))

    def test_lane_job_edit_selects_only_that_lane(self):
        text = self.BASE.replace("  drm-smoke: {runs-on: *runner, steps: [{run: a}]}",
                                 "  drm-smoke: {runs-on: *runner, steps: [{run: b}]}")
        self.assertEqual(m.workflow_lanes(self.BASE, text), {"drm-smoke"})
        self.assertEqual(m.select("pull_request", 0, 2, [m.WORKFLOW], {"drm-smoke"})[0], ["drm-smoke"])

    def test_factory_job_belongs_to_the_marlin_vm_lane(self):
        text = self.BASE.replace("  gdm51-native-package: {runs-on: *runner, steps: [{run: a}]}",
                                 "  gdm51-native-package: {runs-on: *runner, steps: [{run: b}]}")
        self.assertEqual(m.workflow_lanes(self.BASE, text), {"marlin-vm"})

    def test_check_and_gate_edits_select_no_lane(self):
        text = self.BASE.replace("  check: {runs-on: *runner, steps: [{run: a}]}",
                                 "  check: {runs-on: *runner, steps: [{run: b}]}")
        self.assertEqual(m.workflow_lanes(self.BASE, text), set())

    def test_shared_edits_fall_back_to_every_lane(self):
        # A changed anchor edits every aliasing job; triggers, select-lanes
        # and unknown jobs are shared; so is anything unparseable.
        self.assertIsNone(self.lanes("ubuntu-latest", "ubuntu-26.04"))
        self.assertIsNone(self.lanes("on: {pull_request: {}}", "on: {push: {}}"))
        self.assertIsNone(m.workflow_lanes(self.BASE, self.BASE.replace(
            "  select-lanes: {runs-on: *runner, steps: [{run: a}]}",
            "  select-lanes: {runs-on: *runner, steps: [{run: b}]}")))
        self.assertIsNone(m.workflow_lanes(self.BASE, self.BASE + "  mystery: {runs-on: *runner}\n"))
        self.assertIsNone(m.workflow_lanes("", self.BASE))
        self.assertEqual(m.select("pull_request", 0, 2, [m.WORKFLOW], None)[0], m.LANES)


class WorkflowSync(unittest.TestCase):
    def setUp(self):
        self.jobs = yaml.safe_load((ROOT / ".github/workflows/ci.yml").read_text())["jobs"]

    @staticmethod
    def needs(job):
        value = job.get("needs", [])
        return [value] if isinstance(value, str) else value


    def test_journey_invokes_real_transition_package_suite(self):
        commands = [step.get('run', '') for step in self.jobs['journey']['steps']]
        self.assertIn('python3 scripts/lib/transition-package-policy-tests.py', commands)
        installs = [command for command in commands if 'apt-get install' in command]
        self.assertTrue(any('build-essential' in command and 'desktop-file-utils' in command for command in installs))

    def test_gate_waits_for_every_job(self):
        gate = self.jobs["ci-gate"]
        self.assertEqual(gate.get("if"), "always()")
        self.assertEqual(set(self.needs(gate)), set(self.jobs) - {"ci-gate"})

    def test_no_lane_scripts_are_not_run_by_a_lane(self):
        # NO_LANE promises that editing a listed tool cannot change a lane's
        # result. Follow every script name mentioned by a lane job (and,
        # transitively, by the scripts it names; python modules by stem) and
        # fail if one of them is a NO_LANE file.
        lane_jobs = {name: job for name, job in self.jobs.items()
                     if name not in ("check", "select-lanes", "ci-gate")}
        tracked = subprocess.run(["git", "ls-files", "justfile", "scripts"], cwd=ROOT, check=True,
                                 capture_output=True, text=True).stdout.split()
        texts = {}
        for path in tracked:
            try:
                texts[path] = (ROOT / path).read_text(errors="ignore")
            except OSError:
                pass

        def mentioned(path, text):
            names = {Path(path).name}
            if path.endswith(".py"):
                stem = Path(path).stem
                names |= {stem, stem.replace("_", "-")}
            return any(name in text for name in names)

        reached, pending = set(), [json.dumps(lane_jobs)]
        while pending:
            text = pending.pop()
            for path, body in texts.items():
                if path not in reached and mentioned(path, text):
                    reached.add(path)
                    pending.append(body)
        reached_no_lane = sorted(path for path in reached
                                 if m.matches(path, m.NO_LANE) and path in texts
                                 and not m.matches(path, ["scripts/tests/*", "scripts/lib/*-tests.py"]))
        self.assertEqual(reached_no_lane, [])

    def test_native_reference_artifacts_exist_only_off_pull_requests(self):
        # A pull request builds the primary marlin-vm leg only; the two
        # reference members that native-reference-context downloads are
        # produced on the other events.
        self.assertIn("pull_request", self.jobs["native-reference-context"]["if"])


if __name__ == "__main__":
    unittest.main()
