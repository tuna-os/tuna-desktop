"""Semantic validator controls, not installed-image or automatic-timezone evidence."""
import copy
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest


SOURCE = Path(__file__).resolve().parents[1] / "lib" / "core_apps_autotz.py"
SPEC = importlib.util.spec_from_file_location("core_apps_autotz_controls", SOURCE)
AUTOTZ = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(AUTOTZ)

GUEST_SOURCE = (Path(__file__).resolve().parents[2] / "packaging" / "marlin"
                / "vm-lane" / "core-apps" / "autotz.py")
GUEST_SPEC = importlib.util.spec_from_file_location("guest_autotz_controls", GUEST_SOURCE)
GUEST = importlib.util.module_from_spec(GUEST_SPEC)
GUEST_SPEC.loader.exec_module(GUEST)


def observation():
    daemon = {"active": True, "pid": 102, "uid": 1000, "start_ticks": 500,
              "exe": "/usr/lib/gsd-datetime", "exe_sha256": "a" * 64,
              "package": "gnome-settings-daemon 51.0", "unique_owner": ":1.51",
              "unit": "org.gnome.SettingsDaemon.Datetime.service"}
    client = {"desktop_id": "gnome-datetime-panel", "active": True,
              "owner_pid": 102, "owner_uid": 1000, "owner_bus_name": ":1.99",
              "evidence_source": "passive-original-gsd-system-bus",
              "observation_semantics": "observed-within-interval-not-current-properties",
              "location": {"latitude": 40.6893129, "longitude": -74.0445531, "accuracy": 1.83}}
    return {
        "settings": {"master": True, "automatic": True},
        "timedate": {"zone": "America/New_York",
                     "localtime": {"matches_zone": True, "sha256": "b" * 64}},
        "gsd": daemon,
        "identity": {"shell": {"pid": 101, "uid": 1000}},
        "location_agent_registration": {
            "registered": True, "registration_success_inferred_from_error_absence": False,
            "original_shell_agent_export": {"unique_owner": ":1.50", "pid": 101, "uid": 1000}},
        "monitor": {"ready": True, "live": True,
                    "liveness": {"started_monotonic": 100, "checked_monotonic": 110,
                                 "deadline_monotonic": 144}},
        "geoclue": {"clients": [], "interval_observation": {
            "source": "passive-original-gsd-system-bus",
            "semantics": "observed-within-interval-not-current-properties",
            "started_monotonic": 100, "acknowledged_monotonic": 101,
            "observed_through_monotonic": 110, "deadline_monotonic": 144,
            "events": [], "clients": [client]}},
        "timezone_calls": [{"zone": "America/New_York", "interactive": True,
                            "sender": ":1.99", "sender_pid": 102, "sender_uid": 1000,
                            "monotonic": 109}],
    }


class AutomaticTimezoneSemanticControls(unittest.TestCase):
    def setUp(self):
        self.value = observation()
        self.original = copy.deepcopy(self.value["gsd"])

    def test_complete_payload_is_admitted(self):
        self.assertTrue(AUTOTZ.positive_ready(self.value, self.original))

    def test_no_registration_errors_does_not_prove_actual_agent_admission(self):
        self.value["location_agent_registration"] = {"records": []}
        self.assertFalse(AUTOTZ.positive_ready(self.value, self.original))

    def test_another_or_root_agent_cannot_replace_original_shell_registration(self):
        for field, changed in (("pid", 103), ("uid", 0), ("unique_owner", "gnome-shell")):
            with self.subTest(field=field):
                value = observation()
                value["location_agent_registration"]["original_shell_agent_export"][field] = changed
                self.assertFalse(AUTOTZ.positive_ready(value, self.original))

    def test_rejected_agent_cannot_be_replaced_with_a_matching_export(self):
        self.value["location_agent_registration"]["registered"] = False
        self.assertFalse(AUTOTZ.positive_ready(self.value, self.original))

    def test_root_timezone_write_cannot_stand_in_for_daemon(self):
        self.value["timezone_calls"][0]["sender_uid"] = 0
        self.assertFalse(AUTOTZ.positive_ready(self.value, self.original))

    def test_another_user_process_cannot_stand_in_for_daemon(self):
        self.value["timezone_calls"][0]["sender_pid"] = 103
        self.assertFalse(AUTOTZ.positive_ready(self.value, self.original))

    def test_daemon_restart_or_executable_change_invalidates_caller(self):
        for field, changed in (("start_ticks", 501), ("exe_sha256", "c" * 64),
                               ("unique_owner", ":1.52")):
            with self.subTest(field=field):
                value = observation()
                value["gsd"][field] = changed
                self.assertFalse(AUTOTZ.positive_ready(value, self.original))

    def test_timezone_string_without_matching_localtime_is_not_evidence(self):
        self.value["timedate"]["localtime"]["matches_zone"] = False
        self.assertFalse(AUTOTZ.positive_ready(self.value, self.original))

    def test_weather_client_is_not_datetime_client(self):
        self.value["geoclue"]["interval_observation"]["clients"][0]["desktop_id"] = "org.gnome.Weather"
        self.assertFalse(AUTOTZ.positive_ready(self.value, self.original))

    def test_matching_desktop_id_does_not_replace_actual_client_owner(self):
        for field, changed in (("owner_pid", 103), ("owner_uid", 0),
                               ("owner_bus_name", "gnome-datetime-panel"),
                               ("evidence_source", "observer-owned-GetAll")):
            with self.subTest(field=field):
                value = observation()
                value["geoclue"]["interval_observation"]["clients"][0][field] = changed
                self.assertFalse(AUTOTZ.positive_ready(value, self.original))

    def test_unready_location_is_not_a_pass_or_a_malformed_payload(self):
        self.value["geoclue"]["interval_observation"]["clients"][0]["location"] = None
        self.assertFalse(AUTOTZ.positive_ready(self.value, self.original))

    def test_inactive_or_stale_location_cannot_pass(self):
        for field, changed in (("active", False), ("desktop_id", "unrelated")):
            with self.subTest(field=field):
                value = observation()
                value["geoclue"]["interval_observation"]["clients"][0][field] = changed
                self.assertFalse(AUTOTZ.positive_ready(value, self.original))
        self.value["geoclue"]["interval_observation"]["clients"][0]["location"]["longitude"] = 139.6503
        self.assertFalse(AUTOTZ.positive_ready(self.value, self.original))

    def test_unbounded_or_nonfinite_accuracy_cannot_pass(self):
        for accuracy in (0, -1, 50001, float("inf"), float("nan"), True):
            with self.subTest(accuracy=accuracy):
                value = observation()
                value["geoclue"]["interval_observation"]["clients"][0]["location"]["accuracy"] = accuracy
                self.assertFalse(AUTOTZ.positive_ready(value, self.original))

    def test_private_current_read_does_not_substitute_for_owner_bound_interval(self):
        interval = self.value["geoclue"].pop("interval_observation")
        self.value["geoclue"]["clients"] = interval["clients"]
        self.assertFalse(AUTOTZ.positive_ready(self.value, self.original))

    def test_interval_source_and_semantics_cannot_claim_current_private_properties(self):
        for field, changed in (("source", "observer-root-GetAll"),
                               ("semantics", "current-properties")):
            with self.subTest(field=field):
                value = observation()
                value["geoclue"]["interval_observation"][field] = changed
                self.assertFalse(AUTOTZ.positive_ready(value, self.original))

    def test_no_method_call_means_no_automatic_positive(self):
        self.value["timezone_calls"] = []
        self.assertFalse(AUTOTZ.positive_ready(self.value, self.original))

    def test_negative_control_rejects_mutation_of_zone_or_localtime(self):
        baseline = observation()
        baseline["timedate"]["zone"] = AUTOTZ.MANUAL_ZONE
        baseline["settings"] = {"master": False, "automatic": True}
        AUTOTZ.negative_sample(baseline, baseline, self.original, False, True)
        for field, changed in (("zone", AUTOTZ.AUTOMATIC_ZONE),):
            observed = copy.deepcopy(baseline)
            observed["timedate"][field] = changed
            with self.assertRaisesRegex(RuntimeError, "manual zone"):
                AUTOTZ.negative_sample(observed, baseline, self.original, False, True)
        observed = copy.deepcopy(baseline)
        observed["timedate"]["localtime"]["sha256"] = "d" * 64
        with self.assertRaisesRegex(RuntimeError, "/etc/localtime"):
            AUTOTZ.negative_sample(observed, baseline, self.original, False, True)

    def test_old_zone_clock_must_be_excluded(self):
        self.assertFalse(AUTOTZ.clock_matches(
            {"delta_seconds": 25, "stale_delta_seconds": 30}, exclude_stale=True))
        self.assertTrue(AUTOTZ.clock_matches(
            {"delta_seconds": 25, "stale_delta_seconds": 3600}, exclude_stale=True))
        self.assertFalse(AUTOTZ.clock_matches(
            {"delta_seconds": 121, "stale_delta_seconds": 3600}, exclude_stale=True))


class OriginalMonitorLifetimeControls(unittest.TestCase):
    """Exercise real Linux processes/clock; these are not native-app receipts."""

    def setUp(self):
        self.child = subprocess.Popen(
            [sys.executable, "-u", "-c",
             "import os,sys; print('ready',flush=True); "
             "sys.stdin.readline(); os.execv('/usr/bin/sleep',['sleep','30'])",
             "--monitor", "a" * 32],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
        self.addCleanup(self.stop_child)
        self.assertEqual(self.child.stdout.readline().strip(), "ready")
        actual = GUEST._proc_identity(self.child.pid)
        now = time.monotonic()
        self.monitor = {key: actual[key] for key in
                        ("pid", "start_ticks", "exe", "exe_device", "exe_inode", "argv")}
        self.monitor.update(nonce="a" * 32, boot_id=GUEST._boot(),
                            launch_monotonic=now - 0.01, lifetime_seconds=GUEST.MONITOR_SECONDS)
        self.ready = dict(self.monitor, ready=True, started_monotonic=now,
                          monotonic=now, deadline_monotonic=now + GUEST.MONITOR_SECONDS - 1)

    def stop_child(self):
        if self.child.poll() is None:
            self.child.kill()
        self.child.wait(timeout=5)
        self.child.stdin.close()
        self.child.stdout.close()

    def live(self):
        return GUEST._monitor_liveness(self.monitor, self.ready, expected_uid=os.geteuid())

    def test_original_process_is_live_only_inside_admitted_interval(self):
        result = self.live()
        self.assertEqual(result["process"]["pid"], self.child.pid)
        self.assertLess(result["checked_monotonic"], result["deadline_monotonic"])
        self.ready["deadline_monotonic"] = time.monotonic() + 0.02
        time.sleep(0.03)
        with self.assertRaisesRegex(RuntimeError, "deadline"):
            self.live()
        self.assertIsNone(self.child.poll())

    def test_fixed_lifetime_cannot_be_renewed_by_readiness(self):
        self.ready["deadline_monotonic"] += 1
        with self.assertRaisesRegex(RuntimeError, "deadline"):
            self.live()

    def test_persisted_readiness_cannot_admit_dead_original_process(self):
        self.child.kill()
        self.child.wait(timeout=5)
        with self.assertRaises((OSError, RuntimeError)):
            self.live()

    def test_same_pid_exec_replacement_is_not_original_monitor(self):
        self.child.stdin.write("exec\n")
        self.child.stdin.flush()
        deadline = time.monotonic() + 3
        while GUEST._proc_identity(self.child.pid)["exe"] == self.monitor["exe"]:
            if time.monotonic() >= deadline:
                self.fail("owned child did not perform its requested exec")
            time.sleep(0.005)
        with self.assertRaisesRegex(RuntimeError, "identity differs"):
            self.live()

    def test_reused_start_identity_and_foreign_uid_are_rejected(self):
        self.monitor["start_ticks"] += 1
        self.ready["start_ticks"] += 1
        with self.assertRaisesRegex(RuntimeError, "start_ticks"):
            self.live()
        self.monitor["start_ticks"] -= 1
        self.ready["start_ticks"] -= 1
        with self.assertRaisesRegex(RuntimeError, "UID"):
            GUEST._monitor_liveness(self.monitor, self.ready, expected_uid=os.geteuid() + 1)

    def test_other_nonce_or_missing_readiness_is_unevaluable(self):
        self.ready["nonce"] = "b" * 32
        with self.assertRaisesRegex(RuntimeError, "nonce"):
            self.live()
        with self.assertRaisesRegex(RuntimeError, "readiness"):
            GUEST._monitor_liveness(self.monitor, None, expected_uid=os.geteuid())
        with self.assertRaisesRegex(RuntimeError, "unevaluable"):
            GUEST._require_live_monitor(None)

    def test_retirement_denies_even_a_still_live_original_process(self):
        self.monitor["stopped"] = True
        self.assertIsNone(self.child.poll())
        with self.assertRaisesRegex(RuntimeError, "retired"):
            self.live()

    @unittest.skipUnless(os.geteuid() == 0, "production control files require root ownership")
    def test_ended_monitor_and_historical_client_do_not_replace_current_state(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            state = {"monitor": self.monitor, "nonce": self.monitor["nonce"],
                     "boot_id": self.monitor["boot_id"], "gsd": {"pid": 102, "uid": 1000}}
            GUEST._write_json(directory / "ready.json", self.ready)
            admitted = GUEST._monitor_status(directory, state)
            GUEST._require_live_monitor(admitted)
            current = {"clients": [{"path": "/client/1", "active": False}],
                       "errors": [{"operation": "GetAll", "error": "AccessDenied"}]}
            event = {"nonce": state["nonce"], "boot_id": state["boot_id"],
                     "service": None, "gsd_pid": 102, "gsd_uid": 1000,
                     "monotonic": time.monotonic(),
                     "clients": [{"path": "/client/1", "active": True}]}
            log = directory / "geoclue.jsonl"
            log.write_text(json.dumps(event) + "\n")
            log.chmod(0o600)
            observed = GUEST._merge_geoclue_observation(
                copy.deepcopy(current), directory, state, admitted)
            self.assertEqual(observed["clients"], current["clients"])
            self.assertEqual(observed["errors"], current["errors"])
            self.assertTrue(observed["interval_observation"]["clients"][0]["active"])
            self.assertEqual(observed["interval_observation"]["semantics"],
                             "observed-within-interval-not-current-properties")
            # An ended record wins even while the original process remains live.
            GUEST._write_json(directory / "monitor-ended.json", {"pid": self.child.pid})
            ended = GUEST._monitor_status(directory, state)
            with self.assertRaisesRegex(RuntimeError, "unevaluable"):
                GUEST._require_live_monitor(ended)
            with self.assertRaisesRegex(RuntimeError, "unevaluable"):
                GUEST._merge_geoclue_observation(copy.deepcopy(current), directory, state, ended)
            restored = GUEST._merge_geoclue_observation(
                copy.deepcopy(current), directory, state, ended, restoration=True)
            self.assertEqual(restored["clients"], current["clients"])
            self.assertEqual(restored["errors"], current["errors"])
            self.assertEqual(restored["historical_observation_events"], [event])
            self.assertNotIn("interval_observation", restored)

    @unittest.skipUnless(os.geteuid() == 0, "production monitor is root owned")
    def test_owned_pidfd_retirement_invalidates_persisted_ready_receipt(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            state = {"monitor": self.monitor, "nonce": self.monitor["nonce"],
                     "boot_id": self.monitor["boot_id"]}
            GUEST._write_json(directory / "ready.json", self.ready)
            GUEST._require_live_monitor(GUEST._monitor_status(directory, state))
            GUEST._stop_monitor(directory, state)
            self.child.wait(timeout=5)
            self.assertTrue(state["monitor"]["stopped"])
            self.assertTrue(GUEST._read_json(directory / "ready.json")["ready"])
            with self.assertRaisesRegex(RuntimeError, "unevaluable"):
                GUEST._require_live_monitor(GUEST._monitor_status(directory, state))


if __name__ == "__main__":
    unittest.main()