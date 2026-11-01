"""Semantic validator controls, not installed-image or automatic-timezone evidence."""
import copy
import importlib.util
from pathlib import Path
import unittest


SOURCE = Path(__file__).resolve().parents[1] / "lib" / "core_apps_autotz.py"
SPEC = importlib.util.spec_from_file_location("core_apps_autotz_controls", SOURCE)
AUTOTZ = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(AUTOTZ)


def observation():
    daemon = {"active": True, "pid": 102, "uid": 1000, "start_ticks": 500,
              "exe": "/usr/lib/gsd-datetime", "exe_sha256": "a" * 64,
              "package": "gnome-settings-daemon 51.0", "unique_owner": ":1.51",
              "unit": "org.gnome.SettingsDaemon.Datetime.service"}
    return {
        "settings": {"master": True, "automatic": True},
        "timedate": {"zone": "America/New_York",
                     "localtime": {"matches_zone": True, "sha256": "b" * 64}},
        "gsd": daemon,
        "geoclue": {"clients": [{"desktop_id": "gnome-datetime-panel", "active": True,
                                 "owner_pid": 102, "owner_uid": 1000, "owner_bus_name": ":1.99",
                                 "evidence_source": "passive-original-gsd-system-bus",
                                 "location": {"latitude": 40.6893129, "longitude": -74.0445531,
                                              "accuracy": 1.83}}]},
        "timezone_calls": [{"zone": "America/New_York", "interactive": True,
                            "sender": ":1.99", "sender_pid": 102, "sender_uid": 1000}],
    }


class AutomaticTimezoneSemanticControls(unittest.TestCase):
    def setUp(self):
        self.value = observation()
        self.original = copy.deepcopy(self.value["gsd"])

    def test_complete_payload_is_admitted(self):
        self.assertTrue(AUTOTZ.positive_ready(self.value, self.original))

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
        self.value["geoclue"]["clients"][0]["desktop_id"] = "org.gnome.Weather"
        self.assertFalse(AUTOTZ.positive_ready(self.value, self.original))

    def test_matching_desktop_id_does_not_replace_actual_client_owner(self):
        for field, changed in (("owner_pid", 103), ("owner_uid", 0),
                               ("owner_bus_name", "gnome-datetime-panel"),
                               ("evidence_source", "observer-owned-GetAll")):
            with self.subTest(field=field):
                value = observation()
                value["geoclue"]["clients"][0][field] = changed
                self.assertFalse(AUTOTZ.positive_ready(value, self.original))

    def test_unready_location_is_not_a_pass_or_a_malformed_payload(self):
        self.value["geoclue"]["clients"][0]["location"] = None
        self.assertFalse(AUTOTZ.positive_ready(self.value, self.original))

    def test_inactive_or_stale_location_cannot_pass(self):
        for field, changed in (("active", False), ("desktop_id", "unrelated")):
            with self.subTest(field=field):
                value = observation()
                value["geoclue"]["clients"][0][field] = changed
                self.assertFalse(AUTOTZ.positive_ready(value, self.original))
        self.value["geoclue"]["clients"][0]["location"]["longitude"] = 139.6503
        self.assertFalse(AUTOTZ.positive_ready(self.value, self.original))

    def test_unbounded_or_nonfinite_accuracy_cannot_pass(self):
        for accuracy in (0, -1, 50001, float("inf"), float("nan"), True):
            with self.subTest(accuracy=accuracy):
                value = observation()
                value["geoclue"]["clients"][0]["location"]["accuracy"] = accuracy
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


if __name__ == "__main__":
    unittest.main()