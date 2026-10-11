#!/usr/bin/env python3
"""GNOME Date & Time qualification gates for the shipped Marlin image (#359).

Collects time-service evidence on the machine it runs on (designed to run
inside the Marlin guest, where timedate1, the NTP provider, and the panel
clock are the shipped ones) and evaluates five gates:

  G-DATETIME-NTP    NTP service present, active, and synchronized
                    (timedatectl; chrony tracking when chronyd serves)
  G-DATETIME-TZ     timezone round-trip through timedate1 SetTimezone --
                    the same bus method GNOME Settings' Date & Time panel
                    calls -- with the original zone restored and verified
  G-DATETIME-TZ-REJECT  invalid timezone rejected, zone left unchanged
  G-DATETIME-RTC    RTC kept in UTC (LocalRTC=no); the collector never
                    issues SetTime/SetLocalRTC/SetNTP, and no claim is made
                    about RTC hardware accuracy
  G-DATETIME-CLOCK  panel clock label matches system time within tick
                    tolerance, from an a11y dump plus the epoch captured
                    alongside it

Usage:
  tuna-datetime-proof.py collect --out evidence.json [--read-only]
      [--target-tz ZONE] [--a11y-dump NODES.json --system-epoch SECONDS]
      [--clock-tolerance SECONDS]
  tuna-datetime-proof.py check evidence.json [--assertions assertions.txt]

`collect` prints nothing on success; `check` prints one
"<ID> pass|fail <detail>" assertion line per gate (the shape
scripts/tuna-ledger consumes) and exits 1 when any gate fails.
"""
import argparse
import datetime
import json
import os
import re
import subprocess
import sys

BUS = "org.freedesktop.timedate1"
OBJECT = "/org/freedesktop/timedate1"
NEGATIVE_ZONE = "Invalid/Zone"
# Round-trip targets, first one differing from the original wins. Both are
# far from Etc/UTC in wall-clock terms so a stuck write is observable.
ROUND_TRIP_ZONES = ("Pacific/Auckland", "America/New_York")
CLOCK_TOLERANCE_SECONDS = 120
OUTPUT_LIMIT = 65536


def run(argv, timeout=30):
    """Run argv, returning (returncode, stdout, stderr); bounded output."""
    try:
        completed = subprocess.run(argv, capture_output=True, text=True,
                                   timeout=timeout)
    except subprocess.TimeoutExpired as error:
        raise ValueError(f"command-timeout: {' '.join(argv)}") from error
    out, err = completed.stdout or "", completed.stderr or ""
    if len(out) > OUTPUT_LIMIT or len(err) > OUTPUT_LIMIT:
        raise ValueError(f"command-output-limit: {' '.join(argv)}")
    return completed.returncode, out, err


def show_time_to_epoch(raw):
    """Parse `timedatectl show` TimeUSec/RTCTimeUSec ("Sun 2026-10-11 ... UTC").

    timedate1 prints these stamps in UTC; anything else is returned as None
    so callers record the raw string instead of a misread epoch.
    """
    match = re.fullmatch(r"\w{3} (\d{4})-(\d{2})-(\d{2}) "
                         r"(\d{2}):(\d{2}):(\d{2}) (\S+)", (raw or "").strip())
    if not match or match.group(7) != "UTC":
        return None
    year, month, day, hour, minute, second = (int(part) for part in match.groups()[:6])
    try:
        return int(datetime.datetime(year, month, day, hour, minute, second,
                                     tzinfo=datetime.timezone.utc).timestamp())
    except ValueError:
        return None


def parse_timedatectl_show(text):
    """Parse `timedatectl show` KEY=VALUE output into a dict of strings."""
    fields = {}
    for line in (text or "").splitlines():
        if "=" in line:
            key, _, value = line.partition("=")
            fields[key.strip()] = value.strip()
    return fields


def parse_timedatectl_status(text):
    """Parse `timedatectl status` human output into normalized fields."""
    fields = {}
    for line in (text or "").splitlines():
        if ":" not in line:
            continue
        key, _, value = line.partition(":")
        fields[key.strip()] = value.strip()
    zone = fields.get("Time zone", "")
    return {
        "local_time": fields.get("Local time", ""),
        "universal_time": fields.get("Universal time", ""),
        "rtc_time": fields.get("RTC time", ""),
        "timezone": zone.split(" ", 1)[0] if zone else "",
        "synchronized": fields.get("System clock synchronized", ""),
        "ntp_service": fields.get("NTP service", ""),
        "rtc_in_local_tz": fields.get("RTC in local TZ", ""),
    }


def parse_chrony_tracking(text):
    """Parse `chronyc tracking` into its headline fields (all strings)."""
    fields = {}
    for line in (text or "").splitlines():
        if ":" not in line:
            continue
        key, _, value = line.partition(":")
        fields[key.strip()] = value.strip()
    return {
        "reference_id": fields.get("Reference ID", ""),
        "stratum": fields.get("Stratum", ""),
        "system_time": fields.get("System time", ""),
        "last_offset": fields.get("Last offset", ""),
        "leap_status": fields.get("Leap status", ""),
    }


def system_epoch(run_command):
    code, out, _ = run_command(["date", "+%s"])
    if code != 0:
        raise ValueError("date-epoch-unavailable")
    return int(out.strip())


def read_localtime_link():
    try:
        return os.readlink("/etc/localtime")
    except OSError:
        return ""


def read_adjtime():
    try:
        with open("/etc/adjtime", encoding="utf-8") as handle:
            return handle.read()
    except OSError:
        return ""


def timedate1_timezone(run_command):
    code, out, _ = run_command(["timedatectl", "show", "-p", "Timezone",
                                "--value"])
    if code != 0:
        raise ValueError("timedate1-timezone-unavailable")
    return out.strip()


def list_timezones(run_command):
    code, out, _ = run_command(["timedatectl", "list-timezones"])
    if code != 0:
        raise ValueError("timedate1-timezone-list-unavailable")
    return [line.strip() for line in out.splitlines() if line.strip()]


def set_timezone(run_command, zone):
    """Call timedate1 SetTimezone exactly as GNOME Settings does.

    gnome-control-center's datetime panel writes the zone through
    org.freedesktop.timedate1 SetTimezone(s timezone, b interactive);
    the proof passes interactive=false because the VM lane guest agent
    already runs privileged (it drops to tuna-test via runuser itself),
    so no polkit prompt can or should appear.
    """
    return run_command(["busctl", "call", BUS, OBJECT, BUS, "SetTimezone",
                        "s", zone, "b", "false"])


def unit_active(run_command, unit):
    code, out, _ = run_command(["systemctl", "is-active", unit])
    state = out.strip().splitlines()
    return (state[0] if state else "unknown"), code == 0


def collect_ntp(run_command):
    """Read-only NTP evidence: timedate1 plus whichever provider serves."""
    code, show_text, _ = run_command(["timedatectl", "show"])
    if code != 0:
        raise ValueError("timedatectl-show-unavailable")
    code, status_text, _ = run_command(["timedatectl", "status"])
    if code != 0:
        raise ValueError("timedatectl-status-unavailable")
    show, status = parse_timedatectl_show(show_text), parse_timedatectl_status(status_text)
    timesyncd_state, timesyncd_active = unit_active(run_command, "systemd-timesyncd.service")
    chronyd_state, chronyd_active = unit_active(run_command, "chronyd.service")
    evidence = {
        "show": show,
        "show_raw": show_text,
        "status": status,
        "status_raw": status_text,
        "systemd_timesyncd": timesyncd_state,
        "chronyd": chronyd_state,
    }
    if timesyncd_active:
        code, out, _ = run_command(["timedatectl", "timesync-status"])
        evidence["timesync_status"] = out if code == 0 else ""
        evidence["timesync_status_error"] = "" if code == 0 else out.strip() or f"exit={code}"
    if chronyd_active:
        code, out, err = run_command(["chronyc", "tracking"])
        evidence["chrony_tracking"] = parse_chrony_tracking(out) if code == 0 else {}
        evidence["chrony_tracking_raw"] = out if code == 0 else ""
        evidence["chrony_error"] = "" if code == 0 else (err.strip() or f"exit={code}")
    return evidence


def collect_rtc(run_command, show):
    """Read-only RTC policy evidence. No clock is ever written here."""
    return {
        "local_rtc": show.get("LocalRTC", ""),
        "rtc_time_usec": show.get("RTCTimeUSec", ""),
        "localtime_link": read_localtime_link(),
        "adjtime": read_adjtime(),
    }


def collect_timezone_round_trip(run_command, target=None):
    """Set a new zone through timedate1, verify, revert, verify the revert.

    The revert runs in a finally block: a failed verification still
    restores the original zone, and the evidence says whether the
    restoration itself was observed.
    """
    original = timedate1_timezone(run_command)
    zones = list_timezones(run_command)
    if target is None:
        target = next((zone for zone in ROUND_TRIP_ZONES if zone != original), None)
    if target is None:
        raise ValueError("no-round-trip-target-differs-from-original")
    if target not in zones:
        raise ValueError(f"round-trip-target-not-listed: {target}")
    record = {"original": original, "target": target, "reverted": False,
              "revert_verified": False}
    try:
        code, _, err = set_timezone(run_command, target)
        record["set_exit"] = code
        record["set_error"] = err.strip()
        if code != 0:
            raise ValueError(f"timezone-set-failed: {target}: {err.strip()}")
        observed = timedate1_timezone(run_command)
        record["observed"] = observed
        record["observed_link"] = read_localtime_link()
        if observed != target:
            raise ValueError(f"timezone-set-unobserved: wanted {target}, got {observed}")
    finally:
        code, _, err = set_timezone(run_command, original)
        record["revert_exit"] = code
        record["revert_error"] = err.strip()
        if code == 0:
            try:
                record["reverted"] = timedate1_timezone(run_command) == original
                record["reverted_link"] = read_localtime_link()
                record["revert_verified"] = record["reverted"]
            except ValueError as error:
                record["revert_error"] = str(error)
    if not record["revert_verified"]:
        raise ValueError(f"timezone-revert-unverified: {record['revert_error']}")
    return record


def collect_timezone_rejection(run_command, zone=NEGATIVE_ZONE):
    """The negative control: an invalid zone must be rejected in place."""
    before = timedate1_timezone(run_command)
    code, _, err = set_timezone(run_command, zone)
    after = timedate1_timezone(run_command)
    return {"zone": zone, "exit": code, "error": err.strip(),
            "before": before, "after": after,
            "rejected": code != 0, "unchanged": before == after}


CLOCK_LABEL = re.compile(r"(\d{1,2}):(\d{2})(?::(\d{2}))?\s*$")


def panel_clock_labels(nodes):
    """Showing clock-shaped labels from a tuna-a11y-dump.py node list.

    Same family as the G-SETTINGS-CLOCK predicate: a showing label whose
    name ends in HH:MM[:SS]. Hidden nodes and non-clock labels are ignored.
    """
    labels = []
    for node in nodes or []:
        if not isinstance(node, dict):
            continue
        if node.get("role") != "label" or not node.get("showing"):
            continue
        name = node.get("name") or ""
        if CLOCK_LABEL.search(name):
            labels.append(name)
    return labels


def clock_delta_seconds(system_epoch_value, label, zone_name):
    """Seconds between the system epoch and the wall time a label shows.

    The label carries no date, so the wall time is resolved in the given
    zone on the system's own day; a minute-boundary straddle where the
    label already flipped (or has not yet) counts the shorter way round
    by also trying the adjacent days. Returns None when the label has
    no parseable time or the zone is unknown.
    """
    match = CLOCK_LABEL.search(label or "")
    if match is None:
        return None
    if zone_name in ("UTC", "Etc/UTC"):
        zone = datetime.timezone.utc
    else:
        try:
            import zoneinfo
            zone = zoneinfo.ZoneInfo(zone_name)
        except Exception:
            return None
    hour, minute, second = (int(part) if part is not None else 0
                            for part in ((match.group(1), match.group(2),
                                          match.group(3))))
    if hour > 23 or minute > 59 or second > 60:
        return None
    system = datetime.datetime.fromtimestamp(system_epoch_value, tz=zone)
    candidates = []
    for shift in (-1, 0, 1):
        day = system.date() + datetime.timedelta(days=shift)
        try:
            shown = datetime.datetime(day.year, day.month, day.day, hour,
                                      minute, second, tzinfo=zone)
        except ValueError:
            continue
        candidates.append(abs((system - shown).total_seconds()))
    return min(candidates) if candidates else None


def evaluate(evidence, clock_tolerance=CLOCK_TOLERANCE_SECONDS):
    """Evaluate the five gates from a collect() bundle.

    Returns [(gate_id, passed, detail)]; detail is one line for assertions.
    A gate whose inputs are absent fails as unevaluable: missing evidence
    is never a pass.
    """
    results = []
    ntp = evidence.get("ntp") or {}
    show, status = ntp.get("show") or {}, ntp.get("status") or {}
    provider = ("chronyd" if ntp.get("chronyd") == "active"
                else "systemd-timesyncd" if ntp.get("systemd_timesyncd") == "active"
                else "unidentified")
    ntp_detail = (f"NTP={show.get('NTP', '?')} service={status.get('ntp_service', '?')} "
                  f"synchronized={status.get('synchronized', show.get('NTPSynchronized', '?'))} "
                  f"provider={provider}")
    if provider == "chronyd":
        tracking = ntp.get("chrony_tracking") or {}
        if not tracking.get("leap_status"):
            results.append(("G-DATETIME-NTP", False,
                            f"{ntp_detail} chrony-tracking-missing: "
                            f"{ntp.get('chrony_error', '?')}"))
        else:
            ntp_detail += (f" stratum={tracking.get('stratum', '?')} "
                           f"leap={tracking.get('leap_status', '?')}")
            results.append(("G-DATETIME-NTP",
                            show.get("NTP") == "yes"
                            and status.get("ntp_service") == "active"
                            and status.get("synchronized", show.get("NTPSynchronized")) == "yes",
                            ntp_detail))
    else:
        results.append(("G-DATETIME-NTP",
                        show.get("NTP") == "yes"
                        and status.get("ntp_service") == "active"
                        and status.get("synchronized", show.get("NTPSynchronized")) == "yes",
                        ntp_detail))
    trip = evidence.get("timezone_round_trip") or {}
    if trip.get("observed") == trip.get("target") and trip.get("revert_verified"):
        results.append(("G-DATETIME-TZ", True,
                        f"{trip.get('original')} -> {trip.get('target')} observed, "
                        f"reverted to {trip.get('original')} and verified"))
    else:
        results.append(("G-DATETIME-TZ", False,
                        f"round-trip unevaluable or broken: {json.dumps(trip, sort_keys=True)}"))
    reject = evidence.get("timezone_rejection") or {}
    if reject.get("rejected") and reject.get("unchanged"):
        results.append(("G-DATETIME-TZ-REJECT", True,
                        f"{reject.get('zone')} rejected (exit={reject.get('exit')}), "
                        f"zone stayed {reject.get('after')}"))
    else:
        results.append(("G-DATETIME-TZ-REJECT", False,
                        f"negative control unevaluable or broken: "
                        f"{json.dumps(reject, sort_keys=True)}"))
    rtc = evidence.get("rtc") or {}
    if rtc.get("local_rtc") == "no":
        results.append(("G-DATETIME-RTC", True,
                        f"LocalRTC=no ({rtc.get('localtime_link', '?')}); "
                        f"no clock writes issued, no RTC hardware claim"))
    else:
        results.append(("G-DATETIME-RTC", False,
                        f"RTC policy unevaluable or not UTC: {json.dumps(rtc, sort_keys=True)}"))
    clock = evidence.get("panel_clock") or {}
    labels = panel_clock_labels(clock.get("nodes"))
    if not labels:
        results.append(("G-DATETIME-CLOCK", False, "no showing clock label in a11y dump"))
    elif clock.get("system_epoch") is None or not clock.get("timezone"):
        results.append(("G-DATETIME-CLOCK", False, "panel dump lacks captured system epoch/timezone"))
    else:
        deltas = [(label, clock_delta_seconds(clock["system_epoch"], label, clock["timezone"]))
                  for label in labels]
        deltas = [(label, delta) for label, delta in deltas if delta is not None]
        if not deltas:
            results.append(("G-DATETIME-CLOCK", False,
                            f"unparseable clock labels: {labels}"))
        else:
            label, delta = min(deltas, key=lambda item: item[1])
            results.append(("G-DATETIME-CLOCK", delta <= clock_tolerance,
                            f"panel {label!r} vs system epoch {clock['system_epoch']} "
                            f"({clock['timezone']}): delta {delta:.0f}s, tolerance {clock_tolerance}s"))
    return results


def collect(run_command=run, *, target_tz=None, a11y_dump=None,
            system_epoch_value=None, clock_tolerance=CLOCK_TOLERANCE_SECONDS,
            read_only=False):
    """Gather the full evidence bundle on this machine."""
    ntp = collect_ntp(run_command)
    evidence = {
        "ntp": ntp,
        "rtc": collect_rtc(run_command, ntp["show"]),
        "collected_at_epoch": system_epoch(run_command),
        "read_only": read_only,
    }
    if not read_only:
        evidence["timezone_round_trip"] = collect_timezone_round_trip(
            run_command, target=target_tz)
        evidence["timezone_rejection"] = collect_timezone_rejection(run_command)
    if a11y_dump is not None:
        if system_epoch_value is None:
            raise ValueError("panel gate needs --system-epoch captured with the dump")
        with open(a11y_dump, encoding="utf-8") as handle:
            nodes = json.load(handle)
        evidence["panel_clock"] = {
            "nodes": nodes,
            "system_epoch": system_epoch_value,
            "timezone": ntp["show"].get("Timezone", ""),
            "tolerance": clock_tolerance,
        }
    return evidence


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="command", required=True)
    get = sub.add_parser("collect", help="gather evidence on this machine")
    get.add_argument("--out", required=True)
    get.add_argument("--read-only", action="store_true",
                     help="skip the timezone write probes")
    get.add_argument("--target-tz", default=None)
    get.add_argument("--a11y-dump", default=None)
    get.add_argument("--system-epoch", type=int, default=None)
    get.add_argument("--clock-tolerance", type=int, default=CLOCK_TOLERANCE_SECONDS)
    check = sub.add_parser("check", help="evaluate gates from evidence")
    check.add_argument("evidence")
    check.add_argument("--assertions", default=None)
    check.add_argument("--clock-tolerance", type=int, default=CLOCK_TOLERANCE_SECONDS)
    args = parser.parse_args(argv)
    if args.command == "collect":
        bundle = collect(target_tz=args.target_tz, a11y_dump=args.a11y_dump,
                         system_epoch_value=args.system_epoch,
                         clock_tolerance=args.clock_tolerance,
                         read_only=args.read_only)
        with open(args.out, "w", encoding="utf-8") as handle:
            json.dump(bundle, handle, indent=2, sort_keys=True)
            handle.write("\n")
        return 0
    with open(args.evidence, encoding="utf-8") as handle:
        results = evaluate(json.load(handle), clock_tolerance=args.clock_tolerance)
    lines = [f"{gate} {'pass' if passed else 'fail'} {detail}" for gate, passed, detail in results]
    text = "\n".join(lines) + "\n"
    if args.assertions:
        with open(args.assertions, "w", encoding="utf-8") as handle:
            handle.write(text)
    sys.stdout.write(text)
    return 0 if all(passed for _, passed, _ in results) else 1


if __name__ == "__main__":
    sys.exit(main())
