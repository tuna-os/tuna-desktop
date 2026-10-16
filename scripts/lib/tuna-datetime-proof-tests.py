#!/usr/bin/env python3
"""Fixture proofs for the Date & Time gates (#359); no VM, no clock writes.

The timedatectl/chrony fixtures are real local captures used for parser
shape only -- they prove the gate logic reads the tools' output, never
anything about the Marlin image. Live Marlin evidence comes from
`tuna-datetime-proof.py collect` run inside the guest (see
docs/datetime-proof.md). The fake timedate1 transport below emulates the
guest side, including a stuck write and a revert failure.
"""
import importlib.util
import io
import json
import sys
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
FIXTURES = ROOT / "scripts/lib/fixtures"
spec = importlib.util.spec_from_file_location(
    "datetime_proof", ROOT / "scripts/lib/tuna-datetime-proof.py")
M = importlib.util.module_from_spec(spec)
spec.loader.exec_module(M)

SHOW = (FIXTURES / "datetime-timedatectl-show.txt").read_text()
STATUS = (FIXTURES / "datetime-timedatectl-status.txt").read_text()
UNSYNCED = (FIXTURES / "datetime-timedatectl-status-unsynced.txt").read_text()
CHRONY = (FIXTURES / "datetime-chrony-tracking.txt").read_text()
A11Y = json.loads((FIXTURES / "datetime-a11y-clock.json").read_text())
# Sun 2026-10-11 07:40:35 UTC, the stamp inside the show/status fixtures.
FIXTURE_EPOCH = 1791704435


class FakeTimedate1:
    """Emulates timedate1 + provider units; records every command run."""

    def __init__(self, zone="Etc/UTC", zones=("Etc/UTC", "Pacific/Auckland",
                                             "America/New_York"),
                 ntp="yes", synchronized="yes", service="active",
                 local_rtc="no", timesyncd="active", chronyd="inactive",
                 chrony_output=CHRONY, set_fail=(), stuck=False,
                 revert_fail=False, accept_invalid=False, epoch=FIXTURE_EPOCH):
        self.zone = zone
        self.zones = zones
        self.ntp = ntp
        self.synchronized = synchronized
        self.service = service
        self.local_rtc = local_rtc
        self.units = {"systemd-timesyncd.service": timesyncd,
                      "chronyd.service": chronyd}
        self.chrony_output = chrony_output
        self.set_fail = set(set_fail)
        self.stuck = stuck
        self.revert_fail = revert_fail
        self.accept_invalid = accept_invalid
        self.epoch = epoch
        self.original = zone
        self.log = []

    def __call__(self, argv, timeout=30):
        self.log.append(list(argv))
        if argv[0] == "timedatectl" and argv[1:] == ["show"]:
            return 0, (f"Timezone={self.zone}\nLocalRTC={self.local_rtc}\n"
                       f"CanNTP=yes\nNTP={self.ntp}\n"
                       f"NTPSynchronized={self.synchronized}\n"
                       f"TimeUSec=Sun 2026-10-11 07:40:35 UTC\n"
                       f"RTCTimeUSec=Sun 2026-10-11 07:40:34 UTC\n"), ""
        if argv[:3] == ["timedatectl", "show", "-p"]:
            return 0, self.zone + "\n", ""
        if argv[0] == "timedatectl" and argv[1:] == ["status"]:
            return 0, STATUS.replace("Etc/UTC (UTC, +0000)",
                                     f"{self.zone} (UTC, +0000)").replace(
                "System clock synchronized: yes",
                f"System clock synchronized: {self.synchronized}").replace(
                "NTP service: active", f"NTP service: {self.service}"), ""
        if argv[0] == "timedatectl" and argv[1:] == ["list-timezones"]:
            return 0, "".join(zone + "\n" for zone in self.zones), ""
        if argv[0] == "timedatectl" and argv[1:] == ["timesync-status"]:
            return 0, "Server: 9.9.9.9 (ntp.example)\n", ""
        if argv[0] == "systemctl":
            state = self.units.get(argv[2], "unknown")
            return (0 if state == "active" else 3), state + "\n", ""
        if argv[0] == "chronyc":
            if self.chrony_output is None:
                return 1, "", "chronyc: Could not open socket"
            return 0, self.chrony_output, ""
        if argv[0] == "date":
            return 0, f"{self.epoch}\n", ""
        if argv[0] == "busctl":
            zone = argv[argv.index("s") + 1]
            if zone in self.set_fail:
                return 1, "", "Call failed: access denied"
            if zone == self.original and self.revert_fail:
                return 1, "", "Call failed: owner vanished"
            if zone in self.zones or (zone == M.NEGATIVE_ZONE and self.accept_invalid):
                if not self.stuck:
                    self.zone = zone
                return 0, "", ""
            return 1, "", "Call failed: Invalid time zone"
        raise AssertionError(f"unexpected command: {argv}")

    def busctl_calls(self):
        return [argv for argv in self.log if argv[0] == "busctl"]


class ShowParser(unittest.TestCase):
    def test_real_capture_fields_and_epochs(self):
        fields = M.parse_timedatectl_show(SHOW)
        self.assertEqual((fields["Timezone"], fields["LocalRTC"], fields["NTP"],
                          fields["NTPSynchronized"]),
                         ("Etc/UTC", "no", "yes", "yes"))
        self.assertEqual(M.show_time_to_epoch(fields["TimeUSec"]), FIXTURE_EPOCH)
        self.assertEqual(M.show_time_to_epoch(fields["RTCTimeUSec"]), FIXTURE_EPOCH - 1)

    def test_garbage_and_non_utc_stamps_are_not_epochs(self):
        self.assertIsNone(M.show_time_to_epoch("Sun 2026-10-11 07:40:35 CET"))
        self.assertIsNone(M.show_time_to_epoch("not a stamp"))
        self.assertIsNone(M.show_time_to_epoch(""))
        self.assertIsNone(M.show_time_to_epoch(None))


class StatusParser(unittest.TestCase):
    def test_real_capture_fields(self):
        status = M.parse_timedatectl_status(STATUS)
        self.assertEqual((status["timezone"], status["synchronized"],
                          status["ntp_service"], status["rtc_in_local_tz"]),
                         ("Etc/UTC", "yes", "active", "no"))

    def test_unsynced_fixture_differs_only_in_sync_state(self):
        status = M.parse_timedatectl_status(UNSYNCED)
        self.assertEqual((status["synchronized"], status["ntp_service"],
                          status["rtc_in_local_tz"], status["timezone"]),
                         ("no", "inactive", "no", "Etc/UTC"))


class ChronyParser(unittest.TestCase):
    def test_real_tracking_fields(self):
        tracking = M.parse_chrony_tracking(CHRONY)
        self.assertEqual(tracking["stratum"], "4")
        self.assertEqual(tracking["leap_status"], "Normal")
        self.assertIn("169.254.169.123", tracking["reference_id"])
        self.assertTrue(tracking["system_time"].endswith("of NTP time"))


def ntp_evidence(transport):
    with patch.object(M.os, "readlink", return_value="/usr/share/zoneinfo/Etc/UTC"):
        return {"ntp": M.collect_ntp(transport)}


class NtpGate(unittest.TestCase):
    def result(self, evidence):
        return next(item for item in M.evaluate(evidence)
                    if item[0] == "G-DATETIME-NTP")

    def test_timesyncd_provider_passes(self):
        gate, passed, detail = self.result(ntp_evidence(FakeTimedate1()))
        self.assertTrue(passed, detail)
        self.assertIn("provider=systemd-timesyncd", detail)

    def test_chronyd_provider_reports_tracking(self):
        transport = FakeTimedate1(timesyncd="inactive", chronyd="active")
        evidence = ntp_evidence(transport)
        self.assertEqual(evidence["ntp"]["chrony_tracking"]["leap_status"], "Normal")
        gate, passed, detail = self.result(evidence)
        self.assertTrue(passed, detail)
        self.assertIn("stratum=4", detail)

    def test_unsynchronized_or_disabled_ntp_fails(self):
        for kwargs in ({"synchronized": "no"}, {"synchronized": "no", "service": "inactive",
                                                "ntp": "no"}):
            gate, passed, detail = self.result(ntp_evidence(FakeTimedate1(**kwargs)))
            self.assertFalse(passed, kwargs)

    def test_chronyd_without_tracking_fails(self):
        transport = FakeTimedate1(timesyncd="inactive", chronyd="active", chrony_output=None)
        gate, passed, detail = self.result(ntp_evidence(transport))
        self.assertFalse(passed)
        self.assertIn("chrony-tracking-missing", detail)

    def test_missing_ntp_evidence_is_not_a_pass(self):
        gate, passed, detail = self.result({})
        self.assertFalse(passed)
        self.assertIn("?", detail)


class RoundTrip(unittest.TestCase):
    def run_trip(self, transport, **kwargs):
        with patch.object(M.os, "readlink",
                          return_value=f"/usr/share/zoneinfo/{transport.zone}"):
            return M.collect_timezone_round_trip(transport, **kwargs)

    def test_set_verify_revert_verify_leaves_original(self):
        transport = FakeTimedate1()
        record = self.run_trip(transport)
        self.assertEqual(record["original"], "Etc/UTC")
        self.assertEqual(record["target"], "Pacific/Auckland")
        self.assertEqual(record["observed"], "Pacific/Auckland")
        self.assertTrue(record["revert_verified"])
        self.assertEqual(transport.zone, "Etc/UTC")
        calls = transport.busctl_calls()
        self.assertEqual(len(calls), 2)
        # The exact Settings writer shape: bus, path, interface, method, signature.
        self.assertEqual(calls[0][:8],
                         ["busctl", "call", M.BUS, M.OBJECT, M.BUS, "SetTimezone",
                          "s", "Pacific/Auckland"])
        self.assertEqual(calls[0][8:], ["b", "false"])
        self.assertEqual(calls[1][7], "Etc/UTC")
        methods = {word for argv in transport.log for word in argv}
        for writer in ("SetTime", "SetLocalRTC", "SetNTP"):
            self.assertNotIn(writer, methods)

    def test_explicit_target_honored_and_first_differs_skipped(self):
        transport = FakeTimedate1(zone="Pacific/Auckland")
        record = self.run_trip(transport)
        self.assertEqual(record["target"], "America/New_York")
        record = self.run_trip(FakeTimedate1(), target="America/New_York")
        self.assertEqual(record["target"], "America/New_York")

    def test_unlisted_target_fails_before_any_write(self):
        transport = FakeTimedate1()
        with self.assertRaisesRegex(ValueError, "round-trip-target-not-listed"):
            self.run_trip(transport, target="Moon/Crater")
        self.assertEqual(transport.busctl_calls(), [])

    def test_failed_set_still_reverts(self):
        transport = FakeTimedate1(set_fail=("Pacific/Auckland",))
        with self.assertRaisesRegex(ValueError, "timezone-set-failed"):
            self.run_trip(transport)
        self.assertEqual(transport.zone, "Etc/UTC")
        self.assertEqual(len(transport.busctl_calls()), 2)

    def test_stuck_write_is_detected_and_still_reverts(self):
        transport = FakeTimedate1(stuck=True)
        with self.assertRaisesRegex(ValueError, "timezone-set-unobserved"):
            self.run_trip(transport)
        self.assertEqual(len(transport.busctl_calls()), 2)

    def test_failed_revert_is_loud(self):
        transport = FakeTimedate1(revert_fail=True)
        with self.assertRaisesRegex(ValueError, "timezone-revert-unverified"):
            self.run_trip(transport)

    def test_evaluate_passes_only_verified_round_trips(self):
        good = {"timezone_round_trip": {"original": "Etc/UTC", "target": "Pacific/Auckland",
                                        "observed": "Pacific/Auckland", "revert_verified": True}}
        gate, passed, _ = next(item for item in M.evaluate(good) if item[0] == "G-DATETIME-TZ")
        self.assertTrue(passed)
        for bad in ({}, {"timezone_round_trip": {}},
                    {"timezone_round_trip": {**good["timezone_round_trip"], "revert_verified": False}},
                    {"timezone_round_trip": {**good["timezone_round_trip"],
                                             "observed": "Etc/UTC"}}):
            gate, passed, detail = next(item for item in M.evaluate(bad)
                                        if item[0] == "G-DATETIME-TZ")
            self.assertFalse(passed, bad)


class Rejection(unittest.TestCase):
    def test_invalid_zone_rejected_and_zone_unchanged(self):
        transport = FakeTimedate1()
        record = M.collect_timezone_rejection(transport)
        self.assertEqual(record["zone"], M.NEGATIVE_ZONE)
        self.assertTrue(record["rejected"])
        self.assertTrue(record["unchanged"])
        self.assertEqual((record["before"], record["after"]), ("Etc/UTC", "Etc/UTC"))
        gate, passed, detail = next(item for item in M.evaluate(
            {"timezone_rejection": record}) if item[0] == "G-DATETIME-TZ-REJECT")
        self.assertTrue(passed, detail)

    def test_accepted_or_moved_invalid_zone_fails_the_gate(self):
        record = M.collect_timezone_rejection(FakeTimedate1(accept_invalid=True))
        self.assertFalse(record["rejected"])
        gate, passed, _ = next(item for item in M.evaluate(
            {"timezone_rejection": record}) if item[0] == "G-DATETIME-TZ-REJECT")
        self.assertFalse(passed)
        moved = {"timezone_rejection": {"zone": M.NEGATIVE_ZONE, "exit": 1,
                                        "rejected": True, "before": "Etc/UTC",
                                        "after": "Pacific/Auckland", "unchanged": False}}
        gate, passed, _ = next(item for item in M.evaluate(moved)
                                if item[0] == "G-DATETIME-TZ-REJECT")
        self.assertFalse(passed)
        gate, passed, _ = next(item for item in M.evaluate({})
                                if item[0] == "G-DATETIME-TZ-REJECT")
        self.assertFalse(passed)


class RtcGate(unittest.TestCase):
    def result(self, rtc):
        return next(item for item in M.evaluate({"rtc": rtc})
                    if item[0] == "G-DATETIME-RTC")

    def test_utc_policy_passes(self):
        gate, passed, detail = self.result({"local_rtc": "no",
                                            "localtime_link": "/usr/share/zoneinfo/Etc/UTC"})
        self.assertTrue(passed, detail)
        self.assertIn("no RTC hardware claim", detail)

    def test_local_rtc_or_missing_policy_fails(self):
        gate, passed, _ = self.result({"local_rtc": "yes"})
        self.assertFalse(passed)
        gate, passed, _ = self.result({})
        self.assertFalse(passed)
        gate, passed, _ = next(item for item in M.evaluate({})
                                if item[0] == "G-DATETIME-RTC")
        self.assertFalse(passed)

    def test_collector_never_writes_a_clock(self):
        transport = FakeTimedate1()
        with patch.object(M.os, "readlink", return_value="/usr/share/zoneinfo/Etc/UTC"), \
             patch.object(M, "read_adjtime", return_value="0.0 0 0\n0\nUTC\n"):
            M.collect_ntp(transport)
            M.collect_rtc(transport, {"LocalRTC": "no"})
            M.collect_timezone_round_trip(transport)
            M.collect_timezone_rejection(transport)
        methods = {word for argv in transport.log for word in argv}
        self.assertIn("SetTimezone", methods)
        for writer in ("SetTime", "SetLocalRTC", "SetNTP"):
            self.assertNotIn(writer, methods)


class ClockGate(unittest.TestCase):
    def result(self, clock, tolerance=M.CLOCK_TOLERANCE_SECONDS):
        return next(item for item in M.evaluate({"panel_clock": clock}, tolerance)
                    if item[0] == "G-DATETIME-CLOCK")

    def test_showing_clock_label_matches_system_time(self):
        self.assertEqual(M.panel_clock_labels(A11Y), ["Sun 07:40"])
        gate, passed, detail = self.result({"nodes": A11Y, "system_epoch": FIXTURE_EPOCH,
                                            "timezone": "Etc/UTC"})
        self.assertTrue(passed, detail)
        self.assertIn("delta 35s", detail)

    def test_minute_boundary_straddle_counts_either_way(self):
        # Epoch just past the flip, label not yet flipped: 65 s, still consistent.
        gate, passed, detail = self.result({"nodes": A11Y, "system_epoch": FIXTURE_EPOCH + 30,
                                            "timezone": "Etc/UTC"})
        self.assertTrue(passed, detail)
        # Epoch just before the flip, label already flipped.
        early = [dict(node, name="Sun 07:41") if node.get("name") == "Sun 07:40" else node
                 for node in A11Y]
        gate, passed, detail = self.result({"nodes": early, "system_epoch": FIXTURE_EPOCH - 30,
                                            "timezone": "Etc/UTC"})
        self.assertTrue(passed, detail)

    def test_seconds_label_and_midnight_wrap(self):
        nodes = [dict(node, name="07:40:33") if node.get("name") == "Sun 07:40" else node
                 for node in A11Y]
        gate, passed, detail = self.result({"nodes": nodes, "system_epoch": FIXTURE_EPOCH,
                                            "timezone": "Etc/UTC"})
        self.assertTrue(passed, detail)
        self.assertIn("delta 2s", detail)
        midnight = [dict(node, name="00:00") if node.get("name") == "Sun 07:40" else node
                    for node in A11Y]
        gate, passed, _ = self.result({"nodes": midnight, "system_epoch": FIXTURE_EPOCH,
                                       "timezone": "Etc/UTC"})
        self.assertFalse(passed)

    def test_non_utc_zone_resolves_wall_time(self):
        try:
            import zoneinfo
            zoneinfo.ZoneInfo("America/New_York")
        except Exception:
            self.skipTest("zoneinfo database unavailable")
        # FIXTURE_EPOCH is 03:40:35 EDT on 2026-10-11.
        nodes = [dict(node, name="Sun 03:40") if node.get("name") == "Sun 07:40" else node
                 for node in A11Y]
        gate, passed, detail = self.result({"nodes": nodes, "system_epoch": FIXTURE_EPOCH,
                                            "timezone": "America/New_York"})
        self.assertTrue(passed, detail)

    def test_stale_unknown_or_missing_clock_fails(self):
        stale = [dict(node, name="Sun 06:10") if node.get("name") == "Sun 07:40" else node
                 for node in A11Y]
        gate, passed, detail = self.result({"nodes": stale, "system_epoch": FIXTURE_EPOCH,
                                            "timezone": "Etc/UTC"})
        self.assertFalse(passed)
        self.assertIn("delta", detail)
        gate, passed, _ = self.result({"nodes": A11Y, "system_epoch": FIXTURE_EPOCH,
                                       "timezone": "Moon/Crater"})
        self.assertFalse(passed)
        for clock in ({}, {"nodes": []}, {"nodes": A11Y},
                      {"nodes": A11Y, "system_epoch": FIXTURE_EPOCH}):
            gate, passed, _ = self.result(clock)
            self.assertFalse(passed, clock)

    def test_non_labels_and_malformed_nodes_ignored(self):
        nodes = [{"role": "button", "name": "12:34", "showing": True},
                 {"role": "label", "name": "Volume 50%", "showing": True},
                 {"role": "label", "name": "25:99", "showing": True},
                 "not-a-node", None]
        self.assertEqual(M.panel_clock_labels(nodes), ["25:99"])
        self.assertIsNone(M.clock_delta_seconds(FIXTURE_EPOCH, "25:99", "Etc/UTC"))


class CollectModes(unittest.TestCase):
    def test_read_only_collect_skips_writes(self):
        transport = FakeTimedate1()
        with patch.object(M.os, "readlink", return_value="/usr/share/zoneinfo/Etc/UTC"), \
             patch.object(M, "read_adjtime", return_value=""):
            bundle = M.collect(transport, read_only=True)
        self.assertTrue(bundle["read_only"])
        self.assertNotIn("timezone_round_trip", bundle)
        self.assertNotIn("timezone_rejection", bundle)
        self.assertEqual(bundle["ntp"]["show"]["Timezone"], "Etc/UTC")
        self.assertEqual(transport.busctl_calls(), [])

    def test_panel_dump_requires_captured_epoch(self):
        transport = FakeTimedate1()
        with tempfile.NamedTemporaryFile("w", suffix=".json") as dump:
            json.dump(A11Y, dump)
            dump.flush()
            with patch.object(M.os, "readlink", return_value="x"), \
                 patch.object(M, "read_adjtime", return_value=""):
                with self.assertRaisesRegex(ValueError, "needs --system-epoch"):
                    M.collect(transport, read_only=True, a11y_dump=dump.name)
                bundle = M.collect(transport, read_only=True, a11y_dump=dump.name,
                                   system_epoch_value=FIXTURE_EPOCH)
        self.assertEqual(bundle["panel_clock"]["system_epoch"], FIXTURE_EPOCH)
        self.assertEqual(bundle["panel_clock"]["timezone"], "Etc/UTC")

    def test_bounded_command_rejects_runaway_output(self):
        with self.assertRaisesRegex(ValueError, "command-output-limit"):
            M.run([sys.executable, "-c", "print('x' * 70000)"])


class CheckCli(unittest.TestCase):
    def full_bundle(self):
        transport = FakeTimedate1(timesyncd="inactive", chronyd="active")
        with patch.object(M.os, "readlink", return_value="/usr/share/zoneinfo/Etc/UTC"), \
             patch.object(M, "read_adjtime", return_value=""):
            bundle = M.collect(transport)
        bundle["panel_clock"] = {"nodes": A11Y, "system_epoch": FIXTURE_EPOCH,
                                 "timezone": "Etc/UTC",
                                 "tolerance": M.CLOCK_TOLERANCE_SECONDS}
        return bundle

    def check(self, bundle):
        with tempfile.TemporaryDirectory() as directory:
            evidence = Path(directory) / "evidence.json"
            assertions = Path(directory) / "assertions.txt"
            evidence.write_text(json.dumps(bundle))
            out = io.StringIO()
            with redirect_stdout(out):
                code = M.main(["check", str(evidence), "--assertions", str(assertions)])
            return code, out.getvalue(), assertions.read_text()

    def test_full_passing_bundle_emits_five_passes(self):
        code, out, saved = self.check(self.full_bundle())
        self.assertEqual(code, 0)
        self.assertEqual(out, saved)
        lines = out.splitlines()
        self.assertEqual(len(lines), 5)
        for line, gate in zip(lines, ("G-DATETIME-NTP", "G-DATETIME-TZ",
                                      "G-DATETIME-TZ-REJECT", "G-DATETIME-RTC",
                                      "G-DATETIME-CLOCK")):
            self.assertTrue(line.startswith(f"{gate} pass "), line)

    def test_incomplete_bundle_fails_loudly(self):
        bundle = self.full_bundle()
        del bundle["timezone_round_trip"]
        bundle["panel_clock"] = {"nodes": []}
        code, out, _ = self.check(bundle)
        self.assertEqual(code, 1)
        self.assertIn("G-DATETIME-TZ fail", out)
        self.assertIn("G-DATETIME-CLOCK fail", out)
        self.assertIn("G-DATETIME-NTP pass", out)


if __name__ == "__main__":
    unittest.main()
