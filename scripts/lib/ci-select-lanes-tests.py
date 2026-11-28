#!/usr/bin/env python3
"""ci-select-lanes picks real coverage and stays in step with ci.yml."""
import importlib.machinery
import importlib.util
import json
import re
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
    def test_main_schedule_and_dispatch_run_every_lane_but_the_queue_smoke(self):
        # Their full gtk-shell job proves every gate the smoke does.
        battery = [lane for lane in m.LANES if lane != "gtk-shell-smoke"]
        for event in ("push", "schedule", "workflow_dispatch"):
            self.assertEqual(m.select(event, 7, 2, [])[0], battery)

    def test_a_merge_group_runs_the_fast_battery_only(self):
        # The queue waits for check and one GTK session, whatever the queued
        # pull requests changed; the push to main the group becomes runs
        # every lane.
        self.assertEqual(m.MERGE_QUEUE, ["gtk-shell-smoke"])
        for changed in ([], ["docs/x.md"], ["Cargo.lock", "crates/compositor/src/lib.rs"]):
            for run in range(4):
                self.assertEqual(m.select("merge_group", run, 2, changed)[0], ["gtk-shell-smoke"])

    def test_desktop_proofs_replace_their_three_lanes(self):
        # One job runs the grid-cancel comparison, app content and the
        # scroll-mode strip; each proof's inputs select it.
        for old in ("app-content", "scroll-proof", "grid-cancel-comparison"):
            self.assertNotIn(old, m.LANES)
        self.assertIn("desktop-proofs", m.CORE)
        for path in ("scripts/tuna-app-content", "scripts/tuna-scroll-proof",
                     "scripts/tests/test_grid_cancel_comparison.py"):
            for run in (0, 4):
                self.assertEqual(pr(path, run=run), ["desktop-proofs"], path)
        self.assertEqual(m.NEEDS["docs-shots"], ["journey", "desktop-proofs"])

    def test_shared_inputs_run_every_lane(self):
        for path in (".github/workflows/ci.yml", "Cargo.lock", "Cargo.toml",
                     "third-party/smithay/src/lib.rs", "scripts/ci-select-lanes",
                     "scripts/tuna-nested", ".github/actions/ci-tool/action.yml",
                     "scripts/lib/tuna-proof-session", "scripts/lib/tuna-proof-preflight"):
            self.assertEqual(pr("docs/x.md", path), m.LANES, path)

    def test_docs_only_runs_no_lane(self):
        self.assertEqual(pr("README.md", "docs/perf/x.md", ".github/workflows/fuzz.yml",
                            "scripts/lib/ledger-tests.py", "fuzz/Cargo.toml"), [])

    def test_doc_reference_resolver_runs_no_lane(self):
        # #1219: the check job runs the resolver, its self-test and allowlist;
        # no proof lane reads them.
        for run in (0, 4):
            self.assertEqual(pr("scripts/tuna-doc-refs", "scripts/lib/tuna-doc-refs-tests.py",
                                "scripts/lib/tuna-doc-refs-allowlist.txt", run=run), [])

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
        self.assertEqual(pr("scripts/tuna-scroll-proof"), ["desktop-proofs"])
        # The vkms gates (D-*) live in the DRM smoke.
        self.assertEqual(pr("scripts/tuna-drm-smoke"), ["drm-smoke"])
        # D-INPUT-AT-STARTUP's helper and its tests run in the DRM smoke.
        for path in ("scripts/lib/drm_input_startup.py", "scripts/tests/test_drm_input_startup.py"):
            for run in (0, 4, 19):
                with self.subTest(path=path, run=run):
                    self.assertEqual(pr(path, run=run), ["drm-smoke"])
        # The two-output vkms leg (scripts/tuna-drm-dual) runs in drm-smoke.
        for path in ("scripts/tuna-drm-dual", "scripts/lib/drm_dual.py",
                     "scripts/lib/drm_kms_readback.py", "scripts/tests/test_drm_dual.py"):
            for run in (0, 4, 19):
                with self.subTest(path=path, run=run):
                    self.assertEqual(pr(path, run=run), ["drm-smoke"])
        self.assertEqual(pr("docs/walkthrough.tsv"), ["gtk-shell"])
        self.assertEqual(pr("scripts/tuna-greeter-proof"), ["journey", "gtk-shell"])
        self.assertEqual(pr("scripts/lib/tuna-gdm-mock.py"), ["journey", "gtk-shell"])
        self.assertEqual(pr("scripts/lib/org.gnome.login-screen.gschema.xml"), ["journey", "gtk-shell"])
        # The greeter's unit tests run in check; its proof in gtk-shell.
        self.assertIn("gtk-shell", pr("crates/greeter/src/main.rs"))
        self.assertEqual(pr("packaging/marlin/vm-lane/Containerfile"),
                         ["arch-package", "marlin-image", "marlin-vm"])
        # The Marlin VM gates (V-*) live in the VM lane.
        self.assertEqual(pr("scripts/tuna-vm-lane"), ["arch-package", "marlin-vm"])
        # The nested GTK gates (G-*) live in the GTK shell proof; journey
        # runs it against the installed package.
        self.assertEqual(pr("scripts/tuna-gtk-shell-proof"), ["journey", "gtk-shell"])
        # G-VISUAL-BELL, G-X11-BELL, G-XSETTINGS-SOUNDS, G-QS-VOLUME-FEEDBACK
        # (#353) are sourced into it, with their pixel reader.
        for path in ("scripts/lib/tuna-sound-alerts-proof.sh", "scripts/lib/tuna-bell-pixels.py"):
            self.assertEqual(pr(path), ["journey", "gtk-shell"], path)
        # G-OSK-* live in the OSK proof the gtk-shell job runs.
        for path in ("scripts/tuna-osk-proof", "scripts/lib/tuna-osk-probe.py",
                     "scripts/lib/tuna-osk-proof.py"):
            self.assertEqual(pr(path), ["gtk-shell"], path)
        self.assertEqual(pr("scripts/lib/vm_unlock_wake.py"), ["arch-package", "marlin-vm"])
        # The nested GTK gates (G-*) live in the GTK shell proof; journey
        # runs it against the installed package.
        self.assertEqual(pr("scripts/tuna-gtk-shell-proof"), ["journey", "gtk-shell"])
        # MP-*, WL-*, X11-* window-motion gates and the capture they pair with.
        for path in ("scripts/tuna-motion-pairing-proof", "scripts/tuna-x11-icon-geometry-proof",
                     "scripts/lib/motion_pairing.py",
                     "tests/gnome-reference/motion-reference.json"):
            self.assertEqual(pr(path), ["gtk-shell"], path)
        self.assertEqual(pr("docs/parity-ledger.md"),
                         ["journey", "drm-smoke", "hotplug", "gtk-shell", "orca-speech",
                          "remote-security", "arch-package", "marlin-vm", "parity-ledger"])
        self.assertEqual(pr("scripts/tuna-orca-speech-proof"), ["orca-speech"])
        self.assertEqual(pr("scripts/lib/tuna-orca-speech.py"), ["orca-speech"])
        # The VM lane's host-side gate modules, their guest probes and V-TRAY's fixture.
        for path in ("scripts/lib/vm_app_integration.py", "scripts/lib/vm_desktop_gates.py",
                     "packaging/marlin/vm-lane/tuna-vm-desktop-gates", "scripts/lib/vm_wm_gates.py",
                     "packaging/marlin/vm-lane/tuna-vm-wm-gates"):
            self.assertIn("marlin-vm", pr(path, size=0), path)
        self.assertEqual(pr("scripts/lib/vm_desktop_gates.py"), ["arch-package", "marlin-vm"])
        self.assertEqual(pr("scripts/lib/vm_wm_gates.py"), ["arch-package", "marlin-vm"])
        self.assertEqual(pr("packaging/marlin/vm-lane/tuna-vm-wm-gates"),
                         ["arch-package", "marlin-image", "marlin-vm"])
        self.assertEqual(pr("scripts/lib/tuna-tray-item-stub.py"),
                         ["journey", "gtk-shell", "arch-package", "marlin-vm"])
        # The GTK proof sources the top bar and dash slice; only gtk-shell
        # runs its checker (#1198, #1199).
        self.assertEqual(pr("scripts/lib/tuna-panel-dash-proof.sh"), ["journey", "gtk-shell"])
        self.assertEqual(pr("scripts/lib/tuna-panel-dash-check.py"), ["gtk-shell"])
        # The guest's /usr/libexec/tuna-datetime-proof (G-DATETIME-*).
        self.assertEqual(pr("scripts/lib/tuna-datetime-proof.py"),
                         ["arch-package", "marlin-image", "marlin-vm"])
        # The hotplug storm left NO_LANE when ci.yml began running it (#203).
        self.assertEqual(pr("scripts/tuna-hotplug-stress"), ["hotplug"])
        self.assertEqual(pr("scripts/lib/tuna_process_metrics.py"), ["hotplug"])

    def test_hotplug_code_always_runs_the_hotplug_lane_beside_the_code_lanes(self):
        # #203: window re-homing, lock surfaces on new outputs, colord
        # devices and casts across connector hotplug have no nested proof;
        # a change to them must not wait for the rotation to reach hotplug.
        for path in ("crates/compositor/src/drm.rs", "crates/compositor/src/drm/libinput.rs",
                     "crates/compositor/src/windows.rs", "crates/compositor/src/color_manager.rs",
                     "crates/compositor/src/color_manager/tests.rs",
                     "crates/compositor/src/session_lock.rs", "crates/compositor/src/screencast.rs",
                     "crates/compositor/src/mutter.rs", "crates/compositor/src/mutter/remote_bus_tests.rs",
                     "crates/compositor/src/runtime/display.rs", "crates/shell-gtk/src/lock.rs"):
            for run in range(len(m.ROTATION)):
                with self.subTest(path=path, run=run):
                    lanes = pr(path, run=run)
                    self.assertIn("hotplug", lanes)
                    self.assertTrue(set(m.CORE + m.rotation(run, 2)) <= set(lanes))
        # Control: other compositor code reaches hotplug only by rotation.
        self.assertNotIn("hotplug", pr("crates/compositor/src/lib.rs", run=0))
        self.assertIn("hotplug", m.ROTATION)

    def test_touch_fixtures_run_drm_smoke_without_rotation(self):
        # P-DS-04's emulated devices and settings are D-TOUCH/D-ORIENT's
        # alone: an edit runs that lane, not gtk-shell plus a rotation.
        for path in ("scripts/lib/tuna-uinput-touchscreen.py", "scripts/lib/tuna-touch-client.py",
                     "scripts/lib/tuna-mock-sensor-proxy.py",
                     "tests/settings/gnome51-peripherals-touchscreen.gschema.xml"):
            for run in range(len(m.ROTATION)):
                self.assertEqual(pr(path, run=run), ["drm-smoke"], (path, run))

    def test_protocol_gate_inputs_run_the_gtk_shell_proof(self):
        # The Mutter 51 protocol gates (--protocols-only) and G-WAYLAND-INFO
        # run in the gtk-shell job: their client and references select it.
        for run in range(len(m.ROTATION)):
            for path in ("crates/compositor/examples/protocol-probe.rs",
                         "tests/protocols/gnome51-globals.tsv",
                         "tests/protocols/marlin-gnome51-globals.tsv"):
                self.assertIn("gtk-shell", pr(path, run=run), (path, run))
        self.assertEqual(pr("scripts/tuna-wayland-info-compare"), ["gtk-shell"])

    def test_vm_legs_follow_the_event_and_the_ledger(self):
        def legs(event, lanes):
            return [leg["artifact"] for leg in m.vm_matrix(event, lanes)["include"]]
        primary = ["marlin-vm", "marlin-vm-autotz"]
        self.assertEqual(legs("pull_request", pr("packaging/marlin/vm-lane/Containerfile")), primary)
        self.assertEqual(legs("pull_request", []), primary)
        # The ledger cites the Orca 51 leg's V-ORCA-* gates, so a pull request
        # running it builds that leg too; the references stay off pull requests.
        for lanes in (pr("docs/parity-ledger.md"), pr("scripts/tuna-ledger"), pr("Cargo.lock")):
            self.assertEqual(legs("pull_request", lanes), primary + ["marlin-vm-orca51"])
        for event in ("push", "schedule", "workflow_dispatch"):
            self.assertEqual(legs(event, m.LANES), primary + ["marlin-vm-orca51", "native-gnome51-reference",
                                                              "native-tuna-reference"])

    def test_cli_prints_lanes_and_vm_matrix(self):
        def run(*argv):
            out = subprocess.run([str(ROOT / "scripts/ci-select-lanes"), *argv], check=True,
                                 capture_output=True, text=True, cwd=ROOT).stdout.splitlines()
            self.assertEqual([line.split("=", 1)[0] for line in out], ["lanes", "vm_matrix"])
            return [json.loads(line.split("=", 1)[1]) for line in out]
        lanes, matrix = run("--event", "pull_request", "--draft")
        self.assertEqual((lanes, matrix), ([], m.vm_matrix("pull_request", [])))
        lanes, matrix = run("--event", "push")
        battery = [lane for lane in m.LANES if lane not in m.MERGE_QUEUE]
        self.assertEqual((lanes, matrix), (battery, m.vm_matrix("push", battery)))
        lanes, _ = run("--event", "merge_group")
        self.assertEqual(lanes, ["gtk-shell-smoke"])


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
                    'journey', 'desktop-proofs', 'gtk-shell',
                    'portal-security', 'remote-security', 'niri-parity', 'arch-package',
                    'marlin-image', 'marlin-vm'])
        # Control: other shipped runtime data still rotates journey.
        self.assertNotIn('journey', pr("share/applications/tuna-settings.desktop", run=1))

    def test_shipped_login_screen_always_runs_journey_and_package_consumers(self):
        # #1164: both packages and the images ship the greeter's user unit
        # and its inactive gnome-login drop-in; journey checks the installed
        # layout, the images copy the drop-in into place.
        for path in ("share/systemd/user/tuna-greeter.service",
                     "share/greeter/gnome-login.session.conf"):
            for run in range(len(m.ROTATION)):
                with self.subTest(path=path, run=run):
                    self.assertEqual(pr(path, run=run), [
                        'journey', 'desktop-proofs', 'gtk-shell',
                        'portal-security', 'remote-security', 'niri-parity', 'arch-package',
                        'marlin-image', 'marlin-vm'])
        # Control: the session target beside it still rotates journey.
        self.assertNotIn('journey', pr("share/systemd/user/tuna-session.target", run=1))

    def test_package_suites_the_journey_job_runs_select_journey(self):
        for path in ("scripts/lib/installed-deb-proof-tests.py",
                     "scripts/lib/deb-clean-install-tests.py",
                     "scripts/lib/release-deb-ownership-tests.py"):
            for run in (0, 4, 19):
                with self.subTest(path=path, run=run):
                    self.assertEqual(pr(path, run=run), ['journey'])

    def test_fresh_accounts_journey_runs_the_vm_lane(self):
        # tuna-vm-lane --fresh-accounts imports it; its unit suite stays a
        # check-job test.
        for run in (0, 4, 19):
            with self.subTest(run=run):
                self.assertEqual(pr("scripts/lib/fresh_accounts.py", run=run),
                                 ["arch-package", "marlin-vm"])
        self.assertEqual(pr("scripts/tests/test_fresh_accounts.py"), [])

    def test_tooling_of_other_workflows_runs_no_lane(self):
        for path in ("justfile", "scripts/tuna-corral-journey", "scripts/lib/corral_raw.py",
                     "scripts/tuna-variant-qualification", "scripts/lib/vm_release.py"):
            self.assertEqual(pr(path, "docs/x.md", run=3), [], path)
        # Control: a script that is not listed still rotates lanes.
        self.assertIn("gtk-shell", pr("scripts/tuna-journey-helper", run=3))

    def test_edited_tests_run_the_lane_that_executes_them(self):
        # #1108: a scripts/tests/ edit used to run no lane, so a gtk-shell
        # test change first ran on main after merge.
        self.assertEqual(pr("scripts/tests/test_notify_callback_contract.py"), ["gtk-shell"])
        self.assertEqual(pr("scripts/tests/test_native_product.py"),
                         ["arch-package", "marlin-vm"])
        self.assertEqual(pr("scripts/tests/test_grid_cancel_proof.py"), ["desktop-proofs"])
        self.assertEqual(pr("scripts/tests/test_colord_startup.py"), ["drm-smoke"])
        # Control: a test only the always-on check job runs adds no lane.
        self.assertEqual(pr("scripts/tests/test_disk_guard.py"), [])

    def test_remote_desktop_reference_inputs_always_run_remote_security(self):
        # #1169/#1172: the reference harness and the helpers only it runs
        # rotated remote-security in two runs out of nine, so their gates
        # (R-EIS-REVOKE, R-GRD-ENABLE) first ran after merge.
        for path in ("tests/remote-desktop-reference/run.sh",
                     "tests/remote-desktop-reference/Containerfile",
                     "scripts/tuna-remote-security",
                     "scripts/lib/tuna-portal-remote-client.py",
                     "scripts/lib/tuna-remote-ei.c",
                     "scripts/lib/tuna-remote-security-client.py"):
            for run in range(len(m.ROTATION)):
                with self.subTest(path=path, run=run):
                    self.assertEqual(pr(path, run=run), ["remote-security", "arch-package"])
        for run in range(len(m.ROTATION)):
            self.assertEqual(pr("scripts/lib/tuna-service-identity-probe.c", run=run),
                             ["gtk-shell", "remote-security", "arch-package"])
        # Control: another portal helper still rotates.
        self.assertIn("gtk-shell", pr("scripts/lib/tuna-portal-consent.py", run=0))

    def test_uinput_emulators_run_drm_smoke_without_rotation(self):
        # Only tuna-drm-smoke drives them; as code paths they rotated lanes
        # and could skip drm-smoke.
        for name in ("keyboard", "touchpad", "tablet"):
            for run in range(len(m.ROTATION)):
                self.assertEqual(pr(f"scripts/lib/tuna-uinput-{name}.py", run=run),
                                 ["drm-smoke"], name)
    def test_native_desktop_driver_runs_the_vm_lane_that_drives_it(self):
        # #1013: a shared-driver change runs the VM lane whose autotz profile
        # drives it, not whatever slice the rotation picks.
        for run in range(len(m.ROTATION)):
            with self.subTest(run=run):
                self.assertEqual(pr("scripts/lib/vm_native_desktop.py", run=run),
                                 ["arch-package", "marlin-vm"])

    def test_proof_example_clients_run_the_lane_that_runs_them(self):
        # G-SUBSURFACE-STACKING's client runs only in the gtk-shell job's
        # --subsurface-only slice; the check job compiles it.
        for run in (0, 4, 19):
            with self.subTest(run=run):
                self.assertEqual(pr("crates/compositor/examples/tuna-subsurface-client.rs", run=run),
                                 ["gtk-shell"])
        # Control: the compositor code it proves still rotates lanes.
        lanes = pr("crates/compositor/src/runtime_surface_order_tests.rs", run=3)
        self.assertIn("gtk-shell", lanes)
        self.assertGreater(len(lanes), 1)

    def test_screenshot_screen_selectors_run_the_two_output_leg(self):
        # D-SCREENSHOT-KEYS-MULTI (scripts/tuna-drm-dual, drm-smoke) proves
        # the per-monitor screen selectors; the GTK shell proof their keys.
        for path in ("crates/shell-gtk/src/screenshot_ui.rs",
                     "crates/shell-gtk/src/screenshot_screens.rs"):
            for run in range(len(m.ROTATION)):
                with self.subTest(path=path, run=run):
                    self.assertEqual(pr(path, run=run),
                                     ["drm-smoke", "desktop-proofs", "gtk-shell"])
        # Control: the area selection's keys still rotate lanes.
        lanes = pr("crates/shell-gtk/src/screenshot_selection.rs", run=3)
        self.assertIn("gtk-shell", lanes)
        self.assertEqual(set(m.rotation(3, 2)) - set(lanes), set())

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

    ARTIFACTS = """
jobs:
  select-lanes: {runs-on: x, steps: [{run: a}]}
  check:
    runs-on: x
    steps: [{run: a}, {uses: actions/upload-artifact@v1, with: {name: test-list}}]
  arch-package:
    runs-on: x
    needs: [select-lanes, check]
    steps: [{run: a}, {uses: actions/upload-artifact@v1, with: {name: tuna-arch}}]
  portal-security:
    runs-on: x
    needs: [select-lanes, arch-package]
    steps: [{uses: actions/download-artifact@v1, with: {name: tuna-arch}}]
  marlin-vm:
    runs-on: x
    needs: [select-lanes, arch-package]
    steps:
      - uses: actions/download-artifact@v1
        with: {name: tuna-arch}
      - uses: actions/upload-artifact@v1
        with: {name: "${{ matrix.artifact }}"}
  gtk-shell:
    runs-on: x
    needs: [select-lanes]
    outputs: {proof_artifact_id: "${{ steps.up.outputs.artifact-id }}"}
    steps: [{run: a}]
  parity-ledger:
    runs-on: x
    needs: [select-lanes, check, gtk-shell, marlin-vm]
    steps:
      - uses: actions/download-artifact@v1
        with: {name: test-list}
      - uses: actions/download-artifact@v1
        with: {artifact-ids: "${{ needs.gtk-shell.outputs.proof_artifact_id }}"}
      - uses: actions/download-artifact@v1
        with: {name: marlin-vm}
  journey:
    runs-on: x
    needs: [select-lanes]
    steps: [{run: a}]
"""

    def artifact_lanes(self, job):
        text = self.ARTIFACTS.replace(f"  {job}:", f"  {job}:\n    env: {{X: '1'}}", 1)
        return m.workflow_lanes(self.ARTIFACTS, text)

    def test_producer_edit_selects_the_jobs_that_download_its_artifacts(self):
        self.assertEqual(m.artifact_consumers(yaml.safe_load(self.ARTIFACTS)["jobs"])["arch-package"],
                         {"portal-security", "marlin-vm"})
        # arch-package -> portal-security and marlin-vm -> (matrix upload) parity-ledger.
        self.assertEqual(self.artifact_lanes("arch-package"),
                         {"arch-package", "portal-security", "marlin-vm", "parity-ledger"})
        self.assertEqual(self.artifact_lanes("portal-security"), {"portal-security"})

    def test_check_edit_selects_the_consumer_of_its_test_list(self):
        self.assertEqual(self.artifact_lanes("check"), {"parity-ledger"})

    def test_a_job_that_selects_an_artifact_by_output_id_is_a_consumer(self):
        self.assertEqual(self.artifact_lanes("gtk-shell"), {"gtk-shell", "parity-ledger"})

    def test_a_job_not_needing_the_producer_is_not_a_consumer(self):
        # journey downloads nothing and needs nothing from the others.
        self.assertEqual(self.artifact_lanes("journey"), {"journey"})
        text = self.ARTIFACTS.replace("needs: [select-lanes, arch-package]\n    steps:\n      - uses",
                                      "needs: [select-lanes]\n    steps:\n      - uses", 1)
        self.assertEqual(m.artifact_consumers(yaml.safe_load(text)["jobs"])["arch-package"], {"portal-security"})

    def test_real_workflow_boundaries(self):
        base = (ROOT / ".github/workflows/ci.yml").read_text()

        def lanes(job):
            marker = base.index(f"\n  {job}:")
            steps = base.index("\n    steps:", marker)
            return m.workflow_lanes(base, base[:steps] + "\n    env:\n      X: '1'" + base[steps:])
        self.assertEqual(lanes("arch-package"),
                         {"arch-package", "portal-security", "remote-security", "niri-parity",
                          "marlin-image", "marlin-vm", "parity-ledger"})
        self.assertEqual(lanes("check"), {"parity-ledger"})
        self.assertEqual(lanes("ci-gate"), set())
        # The journey no longer waits for check, yet its shots still feed the ledger and docs.
        self.assertEqual(lanes("journey"), {"journey", "parity-ledger", "docs-shots"})
        # The consolidated proofs' shots feed docs-shots; the smoke feeds nothing.
        self.assertEqual(lanes("desktop-proofs"), {"desktop-proofs", "docs-shots"})
        self.assertEqual(lanes("gtk-shell-smoke"), {"gtk-shell-smoke"})
        # The smoke aliases gtk-shell's apt step: editing it reruns both.
        anchor = base.index("&gtk-shell-apt")
        end = base.index("\n", base.index("run:", anchor))
        self.assertEqual(m.workflow_lanes(base, base[:end] + " xterm" + base[end:]),
                         {"gtk-shell", "gtk-shell-smoke", "parity-ledger"})


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

    def test_journey_run_lib_suites_select_journey(self):
        # A scripts/lib/*-tests.py suite the journey job runs must select
        # journey when edited, or it first runs after merge.
        tracked = subprocess.run(["git", "ls-files", "scripts/lib/*-tests.py"], cwd=ROOT, check=True,
                                 capture_output=True, text=True).stdout.split()
        commands = [step.get('run', '') for step in self.jobs['journey']['steps']]
        run = [path for path in tracked if f'python3 {path}' in commands]
        self.assertIn('scripts/lib/deb-clean-install-tests.py', run)
        for path in run:
            with self.subTest(test=path):
                self.assertEqual(pr(path, size=0), ['journey'])

    def test_journey_proves_the_installed_greeter(self):
        # The login screen from the installed .deb's own binaries, after
        # the install step, gating even when an earlier proof failed.
        steps = self.jobs['journey']['steps']
        names = [step.get('name', '') for step in steps]
        install = names.index('Install exact Debian-format candidate and isolated companion fixtures')
        greeter = next(i for i, step in enumerate(steps)
                       if 'scripts/tuna-greeter-proof' in step.get('run', ''))
        self.assertGreater(greeter, install)
        step = steps[greeter]
        self.assertEqual(step['env']['TUNA_PROOF_BIN_DIR'], '/usr/bin')
        self.assertEqual(step['if'], '${{ !cancelled() }}')
        self.assertEqual(step['timeout-minutes'], 10)
        self.assertIn('scripts/tuna-greeter-proof --artifacts journey-artifacts/greeter', step['run'])
        self.assertIn('dpkg-query -S /usr/bin/tuna-compositor /usr/bin/tuna-greeter', step['run'])
        ledger = ' '.join(step.get('run', '') for step in self.jobs['parity-ledger']['steps'])
        self.assertIn('--assertions ledger-in/journey/greeter/assertions.txt', ledger)
        downloads = [step.get('with', {}) for step in self.jobs['parity-ledger']['steps']]
        self.assertIn({'name': 'journey-shots', 'path': 'ledger-in/journey'}, downloads)

    def test_arch_package_smoke_runs_the_installed_greeter(self):
        smoke = next(step['run'] for step in self.jobs['arch-package']['steps']
                     if 'pacman -U' in step.get('run', ''))
        self.assertIn('tuna-greeter; do "$bin" --version; done', smoke)
        self.assertIn('test -f /usr/lib/systemd/user/tuna-greeter.service', smoke)
        self.assertIn('test -f /usr/share/tuna-desktop/greeter/gnome-login.session.conf', smoke)
        self.assertIn('test ! -e /etc/systemd/user/gnome-session@gnome-login.target.d', smoke)

    def test_gate_waits_for_every_job(self):
        gate = self.jobs["ci-gate"]
        # Every run but a cancelled (superseded) one: failed and skipped jobs
        # still reach the gate.
        self.assertEqual(gate.get("if"), "${{ !cancelled() }}")
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

    def test_lane_run_tests_select_their_lane(self):
        # #1108: every scripts/tests/ file a lane job names (by path or module
        # stem) must select that lane when edited. Direct mentions only: the
        # transitive walk above also crosses shared proof scripts, which would
        # claim far more lanes than run the file.
        tracked = subprocess.run(["git", "ls-files", "scripts/tests"], cwd=ROOT, check=True,
                                 capture_output=True, text=True).stdout.split()
        self.assertTrue(tracked)
        for name, job in self.jobs.items():
            lane = m.WORKFLOW_JOB_LANE.get(name)
            if lane is None:
                continue
            text = json.dumps(job)
            for path in tracked:
                stem = Path(path).stem
                if path.endswith(".py") and any(n in text for n in (path, stem, stem.replace("_", "-"))):
                    with self.subTest(job=name, test=path):
                        self.assertIn(lane, pr(path, size=0))

    def test_native_reference_artifacts_exist_only_off_pull_requests(self):
        # A pull request builds no paired reference leg (vm_matrix); the two
        # reference members that native-reference-context downloads are
        # produced on the other events.
        self.assertIn("pull_request", self.jobs["native-reference-context"]["if"])

    def test_select_lanes_takes_the_vm_matrix_from_the_selector(self):
        job = self.jobs["select-lanes"]
        self.assertEqual(job["outputs"]["vm_matrix"], "${{ steps.lanes.outputs.vm_matrix }}")
        step = next(step for step in job["steps"] if step.get("id") == "lanes")
        self.assertNotIn("vm_matrix=", step["run"])
        self.assertIn("scripts/ci-select-lanes --draft", step["run"])

    def test_parity_ledger_reads_every_vm_leg_assertion_file_it_cites(self):
        # The ledger cites V-AUTOTZ (marlin-vm-autotz) and the Orca 51 V-ORCA-*
        # gates (marlin-vm-orca51): each leg's artifact must be downloaded,
        # passed with --assertions, and built whenever parity-ledger runs.
        job = self.jobs["parity-ledger"]
        self.assertIn("marlin-vm", self.needs(job))
        downloads = {step["with"]["name"]: step["with"]["path"] for step in job["steps"]
                     if "actions/download-artifact" in str(step.get("uses", ""))
                     and "name" in step.get("with", {})}
        command = next(step["run"] for step in job["steps"] if "tuna-ledger check" in str(step.get("run", "")))
        inputs = re.findall(r"--assertions (\S+)", command)
        legs = {leg["artifact"] for leg in [*m.VM_PRIMARY, m.VM_ORCA51, *m.VM_REFERENCES]}
        read = {name for name in downloads if name in legs}
        self.assertEqual(read, {"marlin-vm", "marlin-vm-autotz", "marlin-vm-orca51"})
        for name in read:
            self.assertIn(f"{downloads[name]}/assertions.txt", inputs, name)
        for lanes in (pr("docs/parity-ledger.md"), pr("scripts/tuna-ledger"), pr("Cargo.lock")):
            built = {leg["artifact"] for leg in m.vm_matrix("pull_request", lanes)["include"]}
            self.assertLessEqual(read, built, lanes)
        for event in ("push", "schedule", "workflow_dispatch"):
            built = {leg["artifact"] for leg in m.vm_matrix(event, m.LANES)["include"]}
            self.assertLessEqual(read, built, event)

    def test_vm_matrix_names_the_artifacts_the_vm_job_uploads(self):
        uploads = m._upload_names(self.jobs["marlin-vm"])
        self.assertIn("${{ matrix.artifact }}", uploads)
        self.assertEqual(self.jobs["marlin-vm"]["strategy"]["matrix"],
                         "${{ fromJSON(needs.select-lanes.outputs.vm_matrix) }}")

    def test_the_merge_queue_gets_its_required_check(self):
        # A repository rule requiring ci-gate from the merge queue holds
        # every queued pull request until this workflow reports on the
        # merge group; a workflow that does not trigger on it never does.
        # PyYAML reads the key `on` as True.
        workflow = yaml.safe_load((ROOT / ".github/workflows/ci.yml").read_text())
        triggers = workflow.get("on", workflow.get(True))
        self.assertIn("merge_group", triggers)
        # The gate must not be skipped for the event, and a merge group's
        # jobs take GitHub-hosted ubuntu-24.04 (never a self-hosted slot).
        self.assertEqual(self.jobs["ci-gate"]["if"], "${{ !cancelled() }}")
        self.assertIn("github.event_name != 'merge_group'", self.jobs["check"]["runs-on"])
        self.assertTrue(self.jobs["check"]["runs-on"].endswith("|| 'ubuntu-24.04' }}"))
        # Canonical media is compared with the group's base, not with a
        # pull request base or a push's previous head, neither of which a
        # merge group has.
        media = next(s for s in self.jobs["check"]["steps"]
                     if "MEDIA_BASE_SHA" in (s.get("env") or {}))
        self.assertIn("github.event.merge_group.base_sha", media["env"]["MEDIA_BASE_SHA"])

    def test_gtk_shell_slices_run_on_binaries_this_job_built(self):
        # The slices run when an unrelated earlier step failed and skipped
        # the full proof, whose build used to be the only one: they then
        # launched a compositor with no tuna-shell-gtk and all failed
        # A11Y-PANEL. Their binaries come from a build step of their own.
        steps = self.jobs["gtk-shell"]["steps"]
        build = [s.get("id") for s in steps].index("proof-binaries")
        self.assertEqual(steps[build].get("if"), "${{ !cancelled() }}")
        proof = (ROOT / "scripts/tuna-gtk-shell-proof").read_text()
        packages = "-p tuna-compositor -p tuna-wallpaper-svg -p tuna-shell-gtk -p tuna-shell-host"
        self.assertIn(packages, steps[build]["run"])
        self.assertIn(packages, proof)
        full = next(i for i, s in enumerate(steps)
                    if s.get("run", "").strip() == "scripts/tuna-gtk-shell-proof --artifacts gtk-shell-artifacts")
        self.assertLess(build, full)
        slices = [(i, s) for i, s in enumerate(steps) if "TUNA_PROOF_BIN_DIR" in (s.get("env") or {})]
        self.assertGreaterEqual(len(slices), 5)
        for index, step in slices:
            with self.subTest(step=step.get("name")):
                self.assertGreater(index, build)
                self.assertEqual(step.get("if"),
                                 "${{ !cancelled() && steps.proof-binaries.outcome == 'success' }}")

    def test_a_merge_group_runs_only_jobs_whose_needs_it_runs(self):
        # Every lane job is gated on its lane; on a merge group only
        # select-lanes, check, the smoke and the gate run, and none of them
        # waits on a job the group skips (a skipped need skips the job).
        lanes = m.select("merge_group", 0, 2, [])[0]
        pattern = r"contains\(fromJSON\(needs\.select-lanes\.outputs\.lanes\), '([^']+)'\)"
        running = set()
        for name, job in self.jobs.items():
            gated = re.findall(pattern, str(job.get("if", "")))
            if name not in ("select-lanes", "check", "ci-gate"):
                self.assertTrue(gated, f"{name} runs whatever the selection")
            if not gated or any(lane in lanes for lane in gated):
                running.add(name)
        self.assertEqual(running, {"select-lanes", "check", "gtk-shell-smoke", "ci-gate"})
        for name in running - {"ci-gate"}:
            self.assertLessEqual(set(self.needs(self.jobs[name])), running, name)

    def test_the_smoke_runs_its_slice_on_gtk_shells_setup(self):
        job = self.jobs["gtk-shell-smoke"]
        # Beside check, so the queue waits for the longer of the two.
        self.assertEqual(self.needs(job), ["select-lanes"])
        runs = [str(step.get("run", "")) for step in job["steps"]]
        self.assertTrue(any("scripts/tuna-gtk-shell-proof --smoke-only" in run for run in runs))
        self.assertIn("--smoke-only)", (ROOT / "scripts/tuna-gtk-shell-proof").read_text())
        gtk = self.jobs["gtk-shell"]["steps"]
        for step in job["steps"]:
            if "apt-get" in str(step.get("run", "")) or "Ubuntu archive" in str(step.get("name", "")):
                self.assertIn(step, gtk)
        cache = next(s for s in job["steps"] if "rust-cache" in str(s.get("uses", "")))
        gtk_cache = next(s for s in gtk if "rust-cache" in str(s.get("uses", "")))
        # rust-cache keys a job's cache `<key>-<job id>`: restore gtk-shell's.
        self.assertEqual(cache["with"]["shared-key"], gtk_cache["with"]["key"] + "-gtk-shell")
        self.assertEqual(cache["with"]["save-if"], "false")

    def test_desktop_proofs_keep_each_proof_and_artifact(self):
        for old in ("app-content", "scroll-proof", "grid-cancel-comparison"):
            self.assertNotIn(old, self.jobs)
        job = self.jobs["desktop-proofs"]
        steps = job["steps"]
        runs = [str(step.get("run", "")) for step in steps]

        def index(command):
            return next(i for i, run in enumerate(runs) if command in run)
        grid = index("scripts/tuna-grid-cancel-comparison")
        # The comparison admits a clean checkout, so it runs before the
        # other proofs; they reuse its fixed build and run when it failed.
        for command in ("scripts/tuna-app-content --artifacts", "scripts/tuna-scroll-proof --artifacts"):
            step = steps[index(command)]
            with self.subTest(command=command):
                self.assertGreater(index(command), grid)
                self.assertEqual(step.get("if"),
                                 "${{ !cancelled() && steps.desktop-deps.outcome == 'success' }}")
                self.assertEqual(step["env"]["CARGO_TARGET_DIR"], job["env"]["CARGO_TARGET_DIR"] + "/fixed")
        self.assertLessEqual({"grid-cancel-comparison", "app-content-shots", "scroll-proof-shots"},
                             set(m._upload_names(job)))
        # The comparison admits only the job it runs in.
        contract = (ROOT / "scripts/lib/grid_cancel_comparison.py").read_text()
        self.assertIn('\nJOB = "desktop-proofs"\n', contract)


if __name__ == "__main__":
    unittest.main()
