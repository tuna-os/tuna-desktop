#!/usr/bin/env python3
"""Fixture proofs for the Date & Time gates (#359); no VM, no clock writes.

The timedatectl/chrony fixtures are real local captures used for parser
shape only -- they prove the gate logic reads the tools' output, never
anything about the Marlin image. Live Marlin evidence comes from
`tuna-datetime-proof.py collect` run inside the guest (see
docs/datetime-proof.md). The fake timedate1 transport below emulates the
guest side: timedated's refusals, the NTP provider unit, polkit's answer
to the desktop user, and the panel clock, including the failures each
gate must catch.
"""
import datetime
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
GNOME_RULES = ('polkit.addRule(function(action, subject) {\n'
               '  if ((action.id == "org.gnome.controlcenter.datetime.configure") &&\n'
               '      subject.local && subject.active && subject.isInGroup ("wheel")) {\n'
               '    return polkit.Result.YES; } });\n')
DENIAL = ("Access denied as the requested operation requires interactive "
          "authentication. However, interactive authentication has not been "
          "enabled by the calling program.")


def has_zoneinfo():
    try:
        import zoneinfo
        zoneinfo.ZoneInfo("Pacific/Auckland")
        return True
    except Exception:
        return False


class FakeTimedate1:
    """Emulates timedated, the NTP unit, polkit and the panel clock.

    Every command run is logged. The panel clock shows the wall time of
    `panel_zone` at `panel_shift` from the true clock; `follows` makes it
    track zone and clock changes the way GnomeWallClock does.
    """

    def __init__(self, zone="Etc/UTC", zones=("Etc/UTC", "Pacific/Auckland",
                                             "America/New_York"),
                 ntp="yes", synchronized="yes", service="active",
                 local_rtc="no", timesyncd="active", chronyd="absent",
                 chrony_output=CHRONY, set_fail=(), stuck=False,
                 revert_fail=False, accept_invalid=False, epoch=FIXTURE_EPOCH,
                 groups=("tuna-test",), settime_with_ntp=False,
                 settime_ignored=False, stop_ignored=False, mask_ignored=False,
                 follows=True):
        self.zone = zone
        self.zones = zones
        self.ntp_override = None if ntp == "yes" else ntp
        self.synchronized = synchronized
        self.service = service
        self.local_rtc = local_rtc
        self.units = {"systemd-timesyncd.service": timesyncd,
                      "chronyd.service": chronyd}
        self.enabled = {"systemd-timesyncd.service": "enabled",
                        "chronyd.service": "disabled"}
        self.masked = set()
        self.chrony_output = chrony_output
        self.set_fail = set(set_fail)
        self.stuck = stuck
        self.revert_fail = revert_fail
        self.accept_invalid = accept_invalid
        self.epoch = epoch
        self.clock_offset = 0
        self.original = zone
        self.groups = groups
        self.settime_with_ntp = settime_with_ntp
        self.settime_ignored = settime_ignored
        self.stop_ignored = stop_ignored
        self.mask_ignored = mask_ignored
        self.follows = follows
        self.panel_zone = zone
        self.panel_shift = 0
        self.log = []

    def now(self):
        return self.epoch + self.clock_offset

    def provider(self):
        return next((unit for unit, state in self.units.items() if state == "active"), None)

    def ntp(self):
        if self.ntp_override is not None:
            return self.ntp_override
        return "yes" if self.provider() else "no"

    def can_ntp(self):
        # timedated's CanNTP: some ntp-units.d unit is installed and loadable.
        return "yes" if any(unit not in self.masked and state != "absent"
                            for unit, state in self.units.items()) else "no"

    def panel(self):
        if self.follows:
            self.panel_zone, self.panel_shift = self.zone, self.clock_offset
        zone = (datetime.timezone.utc if self.panel_zone in ("UTC", "Etc/UTC")
                else __import__("zoneinfo").ZoneInfo(self.panel_zone))
        shown = datetime.datetime.fromtimestamp(self.epoch + self.panel_shift, tz=zone)
        return {"nodes": [{"role": "label", "name": shown.strftime("%b %d  %-I:%M %p"),
                           "showing": True}], "epoch": self.now()}

    def timedate1(self, argv, as_user=None):
        method, signature, args = argv[5], argv[6], argv[7:]
        if as_user is not None and "wheel" not in self.groups:
            return 1, "", f"Call failed: {DENIAL}"
        if method == "SetTimezone":
            assert signature == "sb" and len(args) == 2, argv
            zone = args[0]
            if zone in self.set_fail:
                return 1, "", "Call failed: access denied"
            if zone == self.original and self.revert_fail and self.zone != zone:
                return 1, "", "Call failed: owner vanished"
            if zone in self.zones or (zone == M.NEGATIVE_ZONE and self.accept_invalid):
                if not self.stuck:
                    self.zone = zone
                return 0, "", ""
            return 1, "", "Call failed: Invalid time zone"
        if method == "SetNTP":
            assert signature == "bb" and len(args) == 2, argv
            unit = "systemd-timesyncd.service"
            if args[0] == "true":
                if self.can_ntp() == "no":
                    return 1, "", "Call failed: NTP not supported"
                self.units[unit], self.enabled[unit] = "active", "enabled"
            else:
                self.units[unit], self.enabled[unit] = "inactive", "disabled"
            return 0, "", ""
        if method == "SetTime":
            assert signature == "xbb" and len(args) == 3, argv
            if self.ntp() == "yes" and not self.settime_with_ntp:
                return 1, "", "Call failed: Automatic time synchronization is enabled"
            if not self.settime_ignored:
                self.clock_offset = int(args[0]) // 1_000_000 - self.epoch
            return 0, "", ""
        raise AssertionError(f"unexpected timedate1 call: {argv}")

    def __call__(self, argv, timeout=30):
        self.log.append(list(argv))
        if argv[0] == "systemd-run":
            assert argv[:7] == ["systemd-run", "--user", "--machine", "tuna-test@",
                                "--wait", "--pipe", "--quiet"], argv
            inner = argv[7:]
            if inner == ["id", "-Gn"]:
                return 0, " ".join(self.groups) + "\n", ""
            if inner[:4] == ["busctl", "--allow-interactive-authorization=false", "call", M.BUS]:
                return self.timedate1([inner[0], *inner[2:]], as_user="tuna-test")
            if inner[-1] == "panel-dump":
                return 0, json.dumps(self.panel()), ""
            raise AssertionError(f"unexpected user command: {argv}")
        if argv[0] == "timedatectl" and argv[1:] == ["show"]:
            return 0, (f"Timezone={self.zone}\nLocalRTC={self.local_rtc}\n"
                       f"CanNTP={self.can_ntp()}\nNTP={self.ntp()}\n"
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
            verb, unit = argv[1], argv[-1]
            if verb == "is-active":
                state = self.units.get(unit, "unknown").replace("absent", "inactive")
                return (0 if state == "active" else 3), state + "\n", ""
            if verb == "is-enabled":
                return 0, self.enabled.get(unit, "unknown") + "\n", ""
            if verb == "stop":
                if not self.stop_ignored:
                    self.units[unit] = "inactive"
                return 0, "", ""
            if verb == "start":
                if unit in self.masked:
                    return 1, "", "Unit is masked."
                self.units[unit] = "active"
                return 0, "", ""
            if verb == "mask":
                if not self.mask_ignored:
                    self.masked.add(unit)
                self.units[unit] = "inactive"
                return 0, "", ""
            if verb == "unmask":
                self.masked.discard(unit)
                return 0, "", ""
            raise AssertionError(f"unexpected systemctl: {argv}")
        if argv[0] == "chronyc":
            if self.chrony_output is None:
                return 1, "", "chronyc: Could not open socket"
            return 0, self.chrony_output, ""
        if argv[0] == "date":
            return 0, f"{self.now()}\n", ""
        if argv[0] == "busctl":
            return self.timedate1(argv)
        raise AssertionError(f"unexpected command: {argv}")

    def busctl_calls(self):
        return [argv for argv in self.log if argv[0] == "busctl"]

    def methods(self):
        return {argv[5] for argv in self.log if argv[0] == "busctl"}


def frozen():
    """A monotonic clock that never advances: the fake clock jumps only
    when SetTime says so."""
    return 100.0


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


def gate(evidence, gate_id, tolerance=M.CLOCK_TOLERANCE_SECONDS):
    return next(item for item in M.evaluate(evidence, tolerance) if item[0] == gate_id)


class NtpGate(unittest.TestCase):
    def test_timesyncd_provider_passes(self):
        _, passed, detail = gate(ntp_evidence(FakeTimedate1()), "G-DATETIME-NTP")
        self.assertTrue(passed, detail)
        self.assertIn("provider=systemd-timesyncd", detail)

    def test_chronyd_provider_reports_tracking(self):
        evidence = ntp_evidence(FakeTimedate1(timesyncd="inactive", chronyd="active"))
        self.assertEqual(evidence["ntp"]["chrony_tracking"]["leap_status"], "Normal")
        _, passed, detail = gate(evidence, "G-DATETIME-NTP")
        self.assertTrue(passed, detail)
        self.assertIn("stratum=4", detail)

    def test_unsynchronized_or_disabled_ntp_fails(self):
        for kwargs in ({"synchronized": "no"}, {"synchronized": "no", "service": "inactive",
                                                "ntp": "no"}):
            _, passed, _ = gate(ntp_evidence(FakeTimedate1(**kwargs)), "G-DATETIME-NTP")
            self.assertFalse(passed, kwargs)

    def test_chronyd_without_tracking_fails(self):
        transport = FakeTimedate1(timesyncd="inactive", chronyd="active", chrony_output=None)
        _, passed, detail = gate(ntp_evidence(transport), "G-DATETIME-NTP")
        self.assertFalse(passed)
        self.assertIn("chrony-tracking-missing", detail)

    def test_missing_ntp_evidence_is_not_a_pass(self):
        _, passed, detail = gate({}, "G-DATETIME-NTP")
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
        self.assertEqual((record["original"], record["target"], record["observed"]),
                         ("Etc/UTC", "Pacific/Auckland", "Pacific/Auckland"))
        self.assertTrue(record["revert_verified"])
        self.assertEqual(transport.zone, "Etc/UTC")
        calls = transport.busctl_calls()
        self.assertEqual(len(calls), 2)
        # busctl's own shape: the whole signature, then every argument --
        # the Settings writer SetTimezone(s timezone, b interactive).
        self.assertEqual(calls[0], ["busctl", "call", M.BUS, M.OBJECT, M.BUS,
                                    "SetTimezone", "sb", "Pacific/Auckland", "false"])
        self.assertEqual(calls[1][7], "Etc/UTC")
        self.assertEqual(transport.methods(), {"SetTimezone"})

    def test_explicit_target_honored_and_first_differs_skipped(self):
        record = self.run_trip(FakeTimedate1(zone="Pacific/Auckland"))
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
        with self.assertRaisesRegex(ValueError, "timezone-revert-unverified"):
            self.run_trip(FakeTimedate1(revert_fail=True))

    def test_evaluate_passes_only_verified_round_trips(self):
        good = {"timezone_round_trip": {"original": "Etc/UTC", "target": "Pacific/Auckland",
                                        "observed": "Pacific/Auckland", "revert_verified": True}}
        self.assertTrue(gate(good, "G-DATETIME-TZ")[1])
        for bad in ({}, {"timezone_round_trip": {}},
                    {"timezone_round_trip": {**good["timezone_round_trip"], "revert_verified": False}},
                    {"timezone_round_trip": {**good["timezone_round_trip"],
                                             "observed": "Etc/UTC"}}):
            self.assertFalse(gate(bad, "G-DATETIME-TZ")[1], bad)


class Rejection(unittest.TestCase):
    def test_invalid_zone_rejected_and_zone_unchanged(self):
        record = M.collect_timezone_rejection(FakeTimedate1())
        self.assertEqual(record["zone"], M.NEGATIVE_ZONE)
        self.assertTrue(record["rejected"] and record["unchanged"])
        _, passed, detail = gate({"timezone_rejection": record}, "G-DATETIME-TZ-REJECT")
        self.assertTrue(passed, detail)

    def test_accepted_or_moved_invalid_zone_fails_the_gate(self):
        record = M.collect_timezone_rejection(FakeTimedate1(accept_invalid=True))
        self.assertFalse(record["rejected"])
        self.assertFalse(gate({"timezone_rejection": record}, "G-DATETIME-TZ-REJECT")[1])
        moved = {"timezone_rejection": {"zone": M.NEGATIVE_ZONE, "exit": 1,
                                        "rejected": True, "before": "Etc/UTC",
                                        "after": "Pacific/Auckland", "unchanged": False}}
        self.assertFalse(gate(moved, "G-DATETIME-TZ-REJECT")[1])
        self.assertFalse(gate({}, "G-DATETIME-TZ-REJECT")[1])


class RtcGate(unittest.TestCase):
    def test_utc_policy_passes(self):
        _, passed, detail = gate({"rtc": {"local_rtc": "no",
                                          "localtime_link": "/usr/share/zoneinfo/Etc/UTC"}},
                                 "G-DATETIME-RTC")
        self.assertTrue(passed, detail)
        self.assertIn("no RTC hardware claim", detail)

    def test_local_rtc_or_missing_policy_fails(self):
        self.assertFalse(gate({"rtc": {"local_rtc": "yes"}}, "G-DATETIME-RTC")[1])
        self.assertFalse(gate({"rtc": {}}, "G-DATETIME-RTC")[1])
        self.assertFalse(gate({}, "G-DATETIME-RTC")[1])


class ClockGate(unittest.TestCase):
    def result(self, clock, tolerance=M.CLOCK_TOLERANCE_SECONDS):
        return gate({"panel_clock": clock}, "G-DATETIME-CLOCK", tolerance)

    def test_showing_clock_label_matches_system_time(self):
        self.assertEqual(M.panel_clock_labels(A11Y), ["Sun 07:40"])
        _, passed, detail = self.result({"nodes": A11Y, "system_epoch": FIXTURE_EPOCH,
                                         "timezone": "Etc/UTC"})
        self.assertTrue(passed, detail)
        self.assertIn("delta 35s", detail)

    def test_twelve_hour_labels_with_gnome_spacing(self):
        # The shell's 12-hour text, as captured on Marlin: en space and
        # figure space between date and time.
        for name, delta in (("Oct 11\u2002\u20077:40 AM", 35), ("7:40\u202fAM", 35),
                            ("Oct 11 7:40 PM", 43165), ("12:05 AM", 27335)):
            nodes = [{"role": "label", "name": name, "showing": True}]
            self.assertEqual(round(M.clock_delta_seconds(FIXTURE_EPOCH, name, "Etc/UTC")),
                             delta, name)
            self.assertEqual(M.panel_clock_labels(nodes), [name])
        self.assertIsNone(M.clock_delta_seconds(FIXTURE_EPOCH, "13:40 PM", "Etc/UTC"))
        self.assertIsNone(M.clock_delta_seconds(FIXTURE_EPOCH, "0:40 AM", "Etc/UTC"))

    def test_minute_boundary_straddle_counts_either_way(self):
        self.assertTrue(self.result({"nodes": A11Y, "system_epoch": FIXTURE_EPOCH + 30,
                                     "timezone": "Etc/UTC"})[1])
        early = [dict(node, name="Sun 07:41") if node.get("name") == "Sun 07:40" else node
                 for node in A11Y]
        self.assertTrue(self.result({"nodes": early, "system_epoch": FIXTURE_EPOCH - 30,
                                     "timezone": "Etc/UTC"})[1])

    def test_seconds_label_and_midnight_wrap(self):
        nodes = [dict(node, name="07:40:33") if node.get("name") == "Sun 07:40" else node
                 for node in A11Y]
        _, passed, detail = self.result({"nodes": nodes, "system_epoch": FIXTURE_EPOCH,
                                         "timezone": "Etc/UTC"})
        self.assertTrue(passed, detail)
        self.assertIn("delta 2s", detail)
        midnight = [dict(node, name="00:00") if node.get("name") == "Sun 07:40" else node
                    for node in A11Y]
        self.assertFalse(self.result({"nodes": midnight, "system_epoch": FIXTURE_EPOCH,
                                      "timezone": "Etc/UTC"})[1])

    def test_non_utc_zone_resolves_wall_time(self):
        if not has_zoneinfo():
            self.skipTest("zoneinfo database unavailable")
        # FIXTURE_EPOCH is 03:40:35 EDT on 2026-10-11.
        nodes = [dict(node, name="Sun 03:40") if node.get("name") == "Sun 07:40" else node
                 for node in A11Y]
        self.assertTrue(self.result({"nodes": nodes, "system_epoch": FIXTURE_EPOCH,
                                     "timezone": "America/New_York"})[1])

    def test_stale_unknown_or_missing_clock_fails(self):
        stale = [dict(node, name="Sun 06:10") if node.get("name") == "Sun 07:40" else node
                 for node in A11Y]
        _, passed, detail = self.result({"nodes": stale, "system_epoch": FIXTURE_EPOCH,
                                         "timezone": "Etc/UTC"})
        self.assertFalse(passed)
        self.assertIn("delta", detail)
        self.assertFalse(self.result({"nodes": A11Y, "system_epoch": FIXTURE_EPOCH,
                                      "timezone": "Moon/Crater"})[1])
        for clock in ({}, {"nodes": []}, {"nodes": A11Y},
                      {"nodes": A11Y, "system_epoch": FIXTURE_EPOCH}):
            self.assertFalse(self.result(clock)[1], clock)

    def test_non_labels_and_malformed_nodes_ignored(self):
        nodes = [{"role": "button", "name": "12:34", "showing": True},
                 {"role": "label", "name": "Volume 50%", "showing": True},
                 {"role": "label", "name": "25:99", "showing": True},
                 "not-a-node", None]
        self.assertEqual(M.panel_clock_labels(nodes), ["25:99"])
        self.assertIsNone(M.clock_delta_seconds(FIXTURE_EPOCH, "25:99", "Etc/UTC"))


class ManualTime(unittest.TestCase):
    def test_settings_sequence_moves_and_restores_the_clock(self):
        transport = FakeTimedate1()
        record = M.collect_manual_time(transport, monotonic=frozen)
        _, passed, detail = gate({"manual_time": record}, "G-DATETIME-MANUAL")
        self.assertTrue(passed, detail)
        self.assertNotEqual(record["set_while_ntp"]["exit"], 0)
        self.assertEqual((record["ntp_after_off"], record["unit_after_off"]), ("no", "inactive"))
        self.assertEqual(record["set_delta"], 0)
        self.assertEqual(transport.clock_offset, 0, "real time written back")
        self.assertEqual((transport.ntp(), transport.enabled["systemd-timesyncd.service"]),
                         ("yes", "enabled"))
        writes = [argv[5:] for argv in transport.busctl_calls()]
        target = FIXTURE_EPOCH + M.MANUAL_OFFSET_SECONDS
        self.assertEqual(writes, [
            ["SetTime", "xbb", str(FIXTURE_EPOCH * 1_000_000), "false", "false"],
            ["SetNTP", "bb", "false", "false"],
            ["SetTime", "xbb", str(target * 1_000_000), "false", "false"],
            ["SetTime", "xbb", str(FIXTURE_EPOCH * 1_000_000), "false", "false"],
            ["SetNTP", "bb", "true", "false"]])
        self.assertNotIn("SetLocalRTC", transport.methods())

    def test_each_broken_step_fails_the_gate(self):
        for kwargs, why in (({"settime_with_ntp": True}, "SetTime refused while NTP on"),
                            ({"settime_ignored": True}, "clock moved by the offset")):
            record = M.collect_manual_time(FakeTimedate1(**kwargs), monotonic=frozen)
            _, passed, detail = gate({"manual_time": record}, "G-DATETIME-MANUAL")
            self.assertFalse(passed, kwargs)
            self.assertIn(why, detail)
        record = M.collect_manual_time(FakeTimedate1(), monotonic=frozen)
        for key, value, why in (("ntp_restored", "no", "NTP restored"),
                                ("restored_delta", 3600, "real time written back"),
                                ("enabled_after", "disabled", "provider enablement restored")):
            _, passed, detail = gate({"manual_time": dict(record, **{key: value})},
                                     "G-DATETIME-MANUAL")
            self.assertFalse(passed, key)
            self.assertIn(why, detail)
        self.assertFalse(gate({}, "G-DATETIME-MANUAL")[1])

    def test_a_failed_set_still_restores_ntp(self):
        transport = FakeTimedate1()
        original = transport.timedate1

        def refuse_settime(argv, as_user=None):
            if argv[5] == "SetTime" and transport.ntp() == "no":
                return 1, "", "Call failed: refused"
            return original(argv, as_user)
        transport.timedate1 = refuse_settime
        with self.assertRaisesRegex(ValueError, "set-time-failed"):
            M.collect_manual_time(transport, monotonic=frozen)
        self.assertEqual((transport.ntp(), transport.clock_offset), ("yes", 0))


class NtpAbsent(unittest.TestCase):
    def test_stopped_and_masked_provider_read_like_gnome_expects(self):
        transport = FakeTimedate1()
        with patch.object(M.time, "sleep"):
            record = M.collect_ntp_absent(transport)
        _, passed, detail = gate({"ntp_absent": record}, "G-DATETIME-NTP-ABSENT")
        self.assertTrue(passed, detail)
        self.assertEqual(record["masked"]["CanNTP"], "no")
        self.assertIn("NTP not supported", record["masked_set_ntp"]["error"])
        self.assertEqual(transport.masked, set())
        self.assertEqual(transport.units["systemd-timesyncd.service"], "active")

    def test_unobserved_stop_or_mask_fails(self):
        clock = iter(range(0, 10_000, 10))
        for kwargs, why in (({"stop_ignored": True}, "stopped"),
                            ({"mask_ignored": True}, "masked")):
            with patch.object(M.time, "sleep"), \
                 patch.object(M.time, "monotonic", lambda: next(clock)):
                record = M.collect_ntp_absent(FakeTimedate1(**kwargs))
            _, passed, detail = gate({"ntp_absent": record}, "G-DATETIME-NTP-ABSENT")
            self.assertFalse(passed, kwargs)
            self.assertIn(f"failed: {why}", detail)
        self.assertFalse(gate({}, "G-DATETIME-NTP-ABSENT")[1])

    def test_no_active_provider_is_unevaluable(self):
        with self.assertRaisesRegex(ValueError, "no-active-ntp-provider"):
            M.collect_ntp_absent(FakeTimedate1(timesyncd="inactive"))


class Polkit(unittest.TestCase):
    def probe(self, groups):
        transport = FakeTimedate1(groups=groups)
        record = M.collect_polkit(transport, "tuna-test", rules=GNOME_RULES)
        return transport, record, gate({"polkit": record}, "G-DATETIME-POLKIT")

    def test_rule_group_parsed_from_gnome_rules(self):
        self.assertEqual(M.parse_privileged_group(GNOME_RULES), "wheel")
        self.assertIsNone(M.parse_privileged_group("polkit.addRule(function() {});"))
        with self.assertRaisesRegex(ValueError, "gnome-polkit-rule-unreadable"):
            M.collect_polkit(FakeTimedate1(), "tuna-test", rules="")

    def test_non_admin_is_refused_and_zone_unchanged(self):
        transport, record, (_, passed, detail) = self.probe(("tuna-test",))
        self.assertEqual(record["expected"], "denied")
        self.assertTrue(passed, detail)
        self.assertEqual(transport.zone, "Etc/UTC")
        user_call = next(argv for argv in transport.log if argv[0] == "systemd-run"
                         and "busctl" in argv)
        self.assertIn("--allow-interactive-authorization=false", user_call)

    def test_admin_is_allowed_and_the_zone_restored(self):
        transport, record, (_, passed, detail) = self.probe(("tuna-test", "wheel"))
        self.assertEqual((record["expected"], record["after"]), ("allowed", "Pacific/Auckland"))
        self.assertTrue(passed, detail)
        self.assertEqual(transport.zone, "Etc/UTC")

    def test_outcome_against_the_rule_fails(self):
        _, record, _ = self.probe(("tuna-test",))
        allowed_anyway = dict(record, exit=0, error="", after=record["target"])
        self.assertFalse(gate({"polkit": allowed_anyway}, "G-DATETIME-POLKIT")[1])
        other_failure = dict(record, error="Call failed: Connection timed out")
        self.assertFalse(gate({"polkit": other_failure}, "G-DATETIME-POLKIT")[1])
        _, record, _ = self.probe(("tuna-test", "wheel"))
        refused = dict(record, exit=1, error=DENIAL, after=record["before"])
        self.assertFalse(gate({"polkit": refused}, "G-DATETIME-POLKIT")[1])
        self.assertFalse(gate({}, "G-DATETIME-POLKIT")[1])


@unittest.skipUnless(has_zoneinfo(), "zoneinfo database unavailable")
class PanelFollow(unittest.TestCase):
    def bundle(self, **kwargs):
        transport = FakeTimedate1(**kwargs)
        with patch.object(M.os, "readlink", return_value="/usr/share/zoneinfo/Etc/UTC"), \
             patch.object(M, "read_adjtime", return_value=""), \
             patch.object(M, "read_gnome_rules", return_value=GNOME_RULES), \
             patch.object(M.pwd, "getpwuid", return_value=type("P", (), {"pw_name": "root"})), \
             patch.object(M.time, "sleep"):
            ticks = iter(range(0, 10_000))
            bundle = M.collect(transport, desktop_user="tuna-test",
                               monotonic=lambda: next(ticks))
        return transport, bundle

    def test_a_following_panel_passes_both_gates(self):
        _, bundle = self.bundle()
        for gate_id in ("G-DATETIME-CLOCK", "G-DATETIME-CLOCK-TZ", "G-DATETIME-CLOCK-SET"):
            _, passed, detail = gate(bundle, gate_id)
            self.assertTrue(passed, f"{gate_id}: {detail}")
        self.assertEqual(len(bundle["timezone_round_trip"]["panel"]), 1)

    def test_a_panel_stuck_in_the_old_zone_fails_after_the_deadline(self):
        transport, bundle = self.bundle(follows=False)
        _, passed, detail = gate(bundle, "G-DATETIME-CLOCK-TZ")
        self.assertFalse(passed)
        self.assertIn("vs the unrepainted reading", detail)
        self.assertGreater(len(bundle["timezone_round_trip"]["panel"]), 1)
        self.assertFalse(gate(bundle, "G-DATETIME-CLOCK-SET")[1])
        self.assertEqual(transport.zone, "Etc/UTC")

    def test_a_panel_wrong_before_the_change_cannot_pass(self):
        _, bundle = self.bundle()
        bundle["panel_clock"]["system_epoch"] += 3 * 3600
        self.assertFalse(gate(bundle, "G-DATETIME-CLOCK")[1])
        _, _, detail = gate(bundle, "G-DATETIME-CLOCK-TZ")
        self.assertFalse(gate(bundle, "G-DATETIME-CLOCK-TZ")[1])
        self.assertIn("already wrong before the change", detail)


class CollectModes(unittest.TestCase):
    def test_read_only_collect_skips_writes(self):
        transport = FakeTimedate1()
        with patch.object(M.os, "readlink", return_value="/usr/share/zoneinfo/Etc/UTC"), \
             patch.object(M, "read_adjtime", return_value=""):
            bundle = M.collect(transport, read_only=True)
        self.assertTrue(bundle["read_only"])
        for key in ("timezone_round_trip", "timezone_rejection", "manual_time",
                    "ntp_absent", "polkit"):
            self.assertNotIn(key, bundle)
        self.assertEqual(bundle["ntp"]["show"]["Timezone"], "Etc/UTC")
        self.assertEqual(transport.busctl_calls(), [])

    def test_a_failing_probe_keeps_its_partial_record_and_error(self):
        transport = FakeTimedate1(set_fail=("Pacific/Auckland",))
        with patch.object(M.os, "readlink", return_value="/usr/share/zoneinfo/Etc/UTC"), \
             patch.object(M, "read_adjtime", return_value=""), patch.object(M.time, "sleep"):
            bundle = M.collect(transport, monotonic=frozen)
        self.assertEqual(bundle["timezone_round_trip"]["set_error"], "Call failed: access denied")
        self.assertIn("timezone-set-failed", bundle["errors"]["timezone_round_trip"])
        _, passed, detail = gate(bundle, "G-DATETIME-TZ")
        self.assertFalse(passed)
        self.assertIn("probe error: timezone-set-failed", detail)
        self.assertTrue(gate(bundle, "G-DATETIME-MANUAL")[1])

    def test_bounded_command_rejects_runaway_output(self):
        with self.assertRaisesRegex(ValueError, "command-output-limit"):
            M.run([sys.executable, "-c", f"print('x' * {M.OUTPUT_LIMIT + 10})"])


class CheckCli(unittest.TestCase):
    def check(self, bundle):
        with tempfile.TemporaryDirectory() as directory:
            evidence = Path(directory) / "evidence.json"
            assertions = Path(directory) / "assertions.txt"
            evidence.write_text(json.dumps(bundle))
            out = io.StringIO()
            with redirect_stdout(out):
                code = M.main(["check", str(evidence), "--assertions", str(assertions)])
            return code, out.getvalue(), assertions.read_text()

    @unittest.skipUnless(has_zoneinfo(), "zoneinfo database unavailable")
    def test_full_passing_bundle_emits_every_gate(self):
        _, bundle = PanelFollow.bundle(PanelFollow())
        code, out, saved = self.check(bundle)
        self.assertEqual(out, saved)
        gates = [line.split()[:2] for line in out.splitlines()]
        self.assertEqual(gates, [[gate_id, "pass"] for gate_id in (
            "G-DATETIME-NTP", "G-DATETIME-TZ", "G-DATETIME-TZ-REJECT", "G-DATETIME-RTC",
            "G-DATETIME-CLOCK", "G-DATETIME-CLOCK-TZ", "G-DATETIME-MANUAL",
            "G-DATETIME-CLOCK-SET", "G-DATETIME-POLKIT", "G-DATETIME-NTP-ABSENT")])
        self.assertEqual(code, 0)

    def test_incomplete_bundle_fails_loudly(self):
        code, out, _ = self.check(ntp_evidence(FakeTimedate1()))
        self.assertEqual(code, 1)
        self.assertIn("G-DATETIME-NTP pass", out)
        for gate_id in ("G-DATETIME-TZ", "G-DATETIME-CLOCK", "G-DATETIME-CLOCK-TZ",
                        "G-DATETIME-MANUAL", "G-DATETIME-POLKIT", "G-DATETIME-NTP-ABSENT"):
            self.assertIn(f"{gate_id} fail", out)


if __name__ == "__main__":
    unittest.main()
