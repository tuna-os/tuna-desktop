"""Semantic validator controls, not installed-image or automatic-timezone evidence."""
import copy
import importlib.util
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock


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


class LocaltimeStateControls(unittest.TestCase):
    """Real temporary files and mocked D-Bus, not native timezone evidence."""

    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.localtime = self.root / "localtime"
        self.zones = self.root / "zoneinfo"
        for zone in ("UTC", "America/New_York", "Pacific/Auckland"):
            path = self.zones / zone
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(("fixture-zone:" + zone).encode())
        self.zone = "UTC"

        def path(value):
            return {"/etc/localtime": self.localtime,
                    "/usr/share/zoneinfo": self.zones}.get(str(value), Path(value))

        patches = (
            mock.patch.object(GUEST, "Path", side_effect=path),
            mock.patch.object(GUEST, "_call", side_effect=lambda *a, **k: ({"Timezone": self.zone},)),
            mock.patch.object(GUEST, "_digest",
                              side_effect=lambda p: hashlib.sha256(Path(p).read_bytes()).hexdigest()),
            mock.patch.object(GUEST, "_gio", return_value=(mock.Mock(), mock.Mock())),
        )
        for patch in patches:
            patch.start()
            self.addCleanup(patch.stop)
        self.context = object()
        self.state = {"original": {"timedate": self.snapshot(),
                                   "settings": {"master": False, "automatic": False}}}

    def snapshot(self):
        return GUEST._timedate(object())

    def set_timezone(self, context, argv, **kwargs):
        if argv[0] == "busctl":
            self.zone = argv[-2]
            self.localtime.unlink(missing_ok=True)
            self.localtime.symlink_to(self.zones / self.zone)

    def test_absent_localtime_saves_explicit_default_utc_without_fabricated_digest(self):
        self.assertEqual(self.snapshot()["localtime"],
                         {"present": False, "resolved": None,
                          "sha256": None, "matches_zone": False})
        self.assertEqual(self.snapshot()["zone"], "UTC")
        GUEST._digest.assert_not_called()

    def test_absence_cannot_prove_automatic_or_manual_zone(self):
        observed = observation()
        for zone in (AUTOTZ.MANUAL_ZONE, AUTOTZ.AUTOMATIC_ZONE):
            observed["timedate"] = dict(self.snapshot(), zone=zone)
            self.assertFalse(AUTOTZ.local_zone(observed, zone))
        self.assertFalse(AUTOTZ.positive_ready(observed, observed["gsd"]))

    def test_absence_disagreeing_with_timedate1_fails(self):
        for zone in ("Etc/UTC", "America/New_York", "Pacific/Auckland"):
            self.zone = zone
            with self.subTest(zone=zone), self.assertRaisesRegex(RuntimeError, "disagrees"):
                self.snapshot()

    def test_dangling_symlink_is_not_implicit_utc(self):
        self.localtime.symlink_to(self.root / "missing-zone")
        with self.assertRaises(FileNotFoundError):
            self.snapshot()

    def test_missing_zoneinfo_is_still_fatal(self):
        self.localtime.write_bytes(b"fixture-zone:UTC")
        (self.zones / "UTC").unlink()
        with self.assertRaises(FileNotFoundError):
            self.snapshot()

    def test_unreadable_localtime_is_still_fatal(self):
        self.localtime.symlink_to(self.zones / "UTC")
        with mock.patch.object(GUEST, "_digest", side_effect=PermissionError):
            with self.assertRaises(PermissionError):
                self.snapshot()

    def test_present_symlink_and_regular_file_keep_real_hash_evidence(self):
        for zone in ("UTC", "America/New_York"):
            self.zone = zone
            for symlink in (True, False):
                self.localtime.unlink(missing_ok=True)
                if symlink:
                    self.localtime.symlink_to(self.zones / zone)
                else:
                    self.localtime.write_bytes((self.zones / zone).read_bytes())
                with self.subTest(zone=zone, symlink=symlink):
                    actual = self.snapshot()["localtime"]
                    self.assertTrue(actual["present"])
                    self.assertTrue(actual["matches_zone"])
                    self.assertEqual(actual["sha256"],
                                     hashlib.sha256((self.zones / zone).read_bytes()).hexdigest())
        self.localtime.write_bytes(b"wrong-zone")
        self.assertFalse(self.snapshot()["localtime"]["matches_zone"])

    def test_cleanup_restores_actual_absence_and_readback_after_utc_write(self):
        self.zone = "America/New_York"
        self.localtime.symlink_to(self.zones / self.zone)
        with mock.patch.object(GUEST, "_command", side_effect=self.set_timezone) as command, \
                mock.patch.object(GUEST, "_stop_monitor"), \
                mock.patch.object(GUEST, "_settings", return_value=self.state["original"]["settings"]), \
                mock.patch.object(GUEST, "_persist"), \
                mock.patch.object(GUEST, "_snapshot",
                                  side_effect=lambda *a, **k: {"timedate": self.snapshot()}):
            restored = GUEST._restore(self.context, self.root, self.state)
        self.assertTrue(restored["restored"])
        self.assertTrue(self.state["restore"]["restored"])
        self.assertEqual(restored["timedate"]["localtime"],
                         self.state["original"]["timedate"]["localtime"])
        self.assertFalse(self.localtime.exists())
        self.assertFalse(self.localtime.is_symlink())
        writes = [call.args[1] for call in command.call_args_list
                  if call.args[1][0] == "busctl"]
        self.assertEqual(len(writes), 1)
        self.assertEqual(writes[0][-3:], ["sb", "UTC", "false"])

    def test_cleanup_does_not_remove_an_original_explicit_utc_file(self):
        self.localtime.symlink_to(self.zones / "UTC")
        self.state["original"]["timedate"] = self.snapshot()
        with mock.patch.object(GUEST, "_command", side_effect=self.set_timezone):
            GUEST._restore_timezone(self.context, self.root, self.state)
        self.assertTrue(self.localtime.is_symlink())
        self.assertEqual(self.snapshot()["localtime"],
                         self.state["original"]["timedate"]["localtime"])

    def test_cleanup_already_absent_utc_needs_no_file_removal(self):
        with mock.patch.object(GUEST, "_command"):
            GUEST._restore_timezone(self.context, self.root, self.state)
        self.assertEqual(self.snapshot()["localtime"],
                         self.state["original"]["timedate"]["localtime"])

    def test_cleanup_wrong_returned_zone_cannot_remove_file(self):
        self.zone = "America/New_York"
        self.localtime.symlink_to(self.zones / self.zone)
        with mock.patch.object(GUEST, "_command"):
            with self.assertRaisesRegex(RuntimeError, "did not return to UTC"):
                GUEST._restore_timezone(self.context, self.root, self.state)
        self.assertEqual(self.localtime.resolve(), self.zones / self.zone)

    def test_cleanup_preserves_mismatched_file_and_reports_failure(self):
        def wrong_file(context, argv, **kwargs):
            if argv[0] == "busctl":
                self.localtime.write_bytes(b"not-the-utc-zone")

        with mock.patch.object(GUEST, "_command", side_effect=wrong_file), \
                mock.patch.object(GUEST, "_stop_monitor"), \
                mock.patch.object(GUEST, "_settings", return_value=self.state["original"]["settings"]), \
                mock.patch.object(GUEST, "_persist"):
            with self.assertRaisesRegex(RuntimeError, "restoration failed"):
                GUEST._restore(self.context, self.root, self.state)
        self.assertFalse(self.state["restore"]["restored"])
        self.assertEqual(self.localtime.read_bytes(), b"not-the-utc-zone")

    def test_failed_cleanup_write_cannot_remove_file_or_claim_restoration(self):
        self.localtime.symlink_to(self.zones / "America/New_York")
        with mock.patch.object(GUEST, "_command", side_effect=RuntimeError("write failed")):
            with self.assertRaisesRegex(RuntimeError, "write failed"):
                GUEST._restore_timezone(self.context, self.root, self.state)
        self.assertEqual(self.localtime.resolve(), self.zones / "America/New_York")


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

    def test_unevaluable_status_names_its_causal_origin(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            state = {"monitor": dict(self.monitor, nonce="b" * 32),
                     "nonce": "a" * 32, "boot_id": self.monitor["boot_id"]}
            status = GUEST._monitor_status(directory, state)
            self.assertFalse(status["live"])
            functions = [(site["module"], site["function"]) for site in status["unevaluable_sites"]]
            self.assertEqual(functions, [("autotz.py", "_monitor_status")])
            with self.assertRaisesRegex(RuntimeError, "unevaluable") as raised:
                GUEST._require_live_monitor(status)
            self.assertEqual(raised.exception.autotz_cause_sites, status["unevaluable_sites"])

    @unittest.skipUnless(os.geteuid() == 0, "production control files require root ownership")
    def test_dead_monitor_origin_is_the_liveness_check(self):
        impossible = int(Path("/proc/sys/kernel/pid_max").read_text().strip())
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            state = {"monitor": dict(self.monitor, pid=impossible),
                     "nonce": self.monitor["nonce"], "boot_id": self.monitor["boot_id"]}
            GUEST._write_json(directory / "ready.json", dict(self.ready, pid=impossible))
            status = GUEST._monitor_status(directory, state)
            self.assertFalse(status["live"])
            functions = [(site["module"], site["function"]) for site in status["unevaluable_sites"]]
            self.assertEqual(functions, [("autotz.py", "_monitor_status"),
                                         ("autotz.py", "_monitor_liveness")])

    def test_failure_receipt_appends_validated_cause_sites(self):
        guest_dir = str(Path(GUEST.__file__).resolve().parent.parent / "tuna-native-session")
        sys.path.insert(0, guest_dir)
        try:
            import autotz_guest
        finally:
            sys.path.remove(guest_dir)
            sys.modules.pop("autotz_guest", None)
            sys.modules.pop("native_session", None)
        cause = [{"module": "autotz.py", "line": 490, "function": "_monitor_liveness"},
                 {"module": "autotz.py", "line": 655, "function": "_proc_identity"}]
        error = RuntimeError("unevaluable")
        error.autotz_cause_sites = cause
        receipt = autotz_guest.error_receipt(error, ["autotz", "configure"])
        self.assertEqual(receipt["sites"], cause)
        error = RuntimeError("unevaluable")
        error.autotz_cause_sites = cause + [{"module": "evil.py", "line": 1, "function": "f"},
                                            {"module": "autotz.py", "line": 0, "function": "f"},
                                            {"module": "autotz.py", "line": 1, "function": "bad-name!"},
                                            "not-a-site",
                                            {"module": "autotz.py", "line": 491, "function": "_monitor_status",
                                             "extra": 1}]
        receipt = autotz_guest.error_receipt(error, ["autotz", "watch-start"])
        self.assertEqual(receipt["sites"], cause)
        error = RuntimeError("unevaluable")
        error.autotz_cause_sites = [{"module": "autotz.py", "line": 100 + index,
                                     "function": "frame%d" % index} for index in range(5)]
        receipt = autotz_guest.error_receipt(error, ["autotz", "snapshot"])
        self.assertEqual(len(receipt["sites"]), 4)
        self.assertEqual(receipt["sites"][0]["line"], 101)


if __name__ == "__main__":
    unittest.main()