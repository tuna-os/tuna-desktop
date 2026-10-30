#!/usr/bin/env python3
"""GNOME Date & Time qualification gates for the shipped Marlin image (#359).

Collects time-service evidence on the machine it runs on (designed to run
as root inside the Marlin guest, where timedate1, the NTP provider, polkit
and the panel clock are the shipped ones) and evaluates these gates:

  G-DATETIME-NTP    NTP service present, active, and synchronized
                    (timedatectl; chrony tracking when chronyd serves)
  G-DATETIME-TZ     timezone round-trip through timedate1 SetTimezone --
                    the same bus method GNOME Settings' Date & Time panel
                    calls -- with the original zone restored and verified
  G-DATETIME-TZ-REJECT  invalid timezone rejected, zone left unchanged
  G-DATETIME-RTC    RTC kept in UTC (LocalRTC=no); no claim is made about
                    RTC hardware accuracy and SetLocalRTC is never called
  G-DATETIME-CLOCK  panel clock label matches system time within tick
                    tolerance (an AT-SPI dump of tuna-shell-gtk taken with
                    the epoch)
  G-DATETIME-CLOCK-TZ   the panel clock shows the new zone's wall time
                    within PANEL_FOLLOW_SECONDS of SetTimezone, as
                    GnomeWallClock repaints on a zone change
  G-DATETIME-MANUAL manual time as the Settings panel sets it: SetTime is
                    refused while NTP is on; SetNTP(false) stops the
                    provider; SetTime moves the clock; the real time and
                    NTP are restored afterwards
  G-DATETIME-CLOCK-SET  the panel clock shows the manually set time within
                    PANEL_FOLLOW_SECONDS
  G-DATETIME-POLKIT the desktop user's own non-interactive SetTimezone is
                    allowed exactly when GNOME 51's gnome-control-center
                    polkit rule grants it (privileged group member), and
                    is otherwise refused with an interactive-auth demand
  G-DATETIME-NTP-ABSENT  a stopped provider reads NTP=no with CanNTP=yes;
                    a masked (missing) provider reads CanNTP=no and
                    SetNTP(true) is refused -- the state that hides the
                    panel's Automatic Date & Time row; both are restored

Usage:
  tuna-datetime-proof.py collect --out evidence.json [--read-only]
      [--target-tz ZONE] [--desktop-user USER]
      [--clock-tolerance SECONDS]
  tuna-datetime-proof.py check evidence.json [--assertions assertions.txt]
  tuna-datetime-proof.py panel-dump [--app NAME] [--timeout SECONDS]

`collect` prints nothing on success; `check` prints one
"<ID> pass|fail <detail>" assertion line per gate (the shape
scripts/tuna-ledger consumes) and exits 1 when any gate fails.
`panel-dump` runs as the desktop user and prints {"nodes", "epoch"}
for the shell's accessible labels; `collect --desktop-user` runs it
through the user's systemd manager.
"""
import argparse
import datetime
import json
import os
import pwd
import re
import subprocess
import sys
import time

BUS = "org.freedesktop.timedate1"
OBJECT = "/org/freedesktop/timedate1"
NEGATIVE_ZONE = "Invalid/Zone"
# Round-trip targets, first one differing from the original wins. Both are
# far from Etc/UTC in wall-clock terms so a stuck write is observable.
ROUND_TRIP_ZONES = ("Pacific/Auckland", "America/New_York")
CLOCK_TOLERANCE_SECONDS = 120
# GnomeWallClock repaints at once on a zone change or clock set
# (gnome-desktop 51.0 gnome-wall-clock.c); allow for the dump itself.
PANEL_FOLLOW_SECONDS = 10
# The manual-time jump: over two hours, so the panel clock cannot show
# it by accident, and an odd minute count.
MANUAL_OFFSET_SECONDS = 2 * 3600 + 7 * 60
MANUAL_TOLERANCE_SECONDS = 5
NTP_UNITS = ("systemd-timesyncd.service", "chronyd.service")
# GNOME 51 gnome-control-center panels/common/gnome-control-center.rules.in:
# org.gnome.controlcenter.datetime.configure (which implies the timedate1
# actions) is granted to local active members of @PRIVILEGED_GROUP@.
GNOME_RULES = "/usr/share/polkit-1/rules.d/gnome-control-center.rules"
INTERACTIVE_DENIAL = "interactive authentication"
OUTPUT_LIMIT = 1 << 20


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


def parse_privileged_group(text):
    """The group GNOME's gnome-control-center.rules admits, or None."""
    match = re.search(r'isInGroup\s*\(\s*"([^"]+)"\s*\)', text or "")
    return match.group(1) if match else None


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


def read_gnome_rules():
    try:
        with open(GNOME_RULES, encoding="utf-8") as handle:
            return handle.read()
    except OSError:
        return ""


def timedate1_show(run_command):
    code, out, _ = run_command(["timedatectl", "show"])
    if code != 0:
        raise ValueError("timedatectl-show-unavailable")
    return parse_timedatectl_show(out)


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


def timedate1_call(run_command, method, signature, *args):
    """One timedate1 method call; `busctl call` takes the whole signature
    first, then each argument."""
    return run_command(["busctl", "call", BUS, OBJECT, BUS, method, signature,
                        *(str(arg) for arg in args)])


def set_timezone(run_command, zone):
    """Call timedate1 SetTimezone(s timezone, b interactive) as GNOME
    Settings does (gnome-control-center 51.0
    panels/system/datetime/cc-datetime-page.c queue_set_timezone).

    The collector runs as root, which timedated admits without polkit,
    so interactive is false.
    """
    return timedate1_call(run_command, "SetTimezone", "sb", zone, "false")


def set_ntp(run_command, enabled):
    """SetNTP(b use_ntp, b interactive), cc-datetime-page.c queue_set_ntp."""
    return timedate1_call(run_command, "SetNTP", "bb",
                          "true" if enabled else "false", "false")


def set_time(run_command, epoch_seconds):
    """SetTime(x usec_utc, b relative, b interactive) with an absolute
    time, as cc-datetime-page.c on_timezone_changed/set_time does."""
    return timedate1_call(run_command, "SetTime", "xbb",
                          int(epoch_seconds) * 1_000_000, "false", "false")


def unit_active(run_command, unit):
    code, out, _ = run_command(["systemctl", "is-active", unit])
    state = out.strip().splitlines()
    return (state[0] if state else "unknown"), code == 0


def unit_enabled(run_command, unit):
    _, out, _ = run_command(["systemctl", "is-enabled", unit])
    state = out.strip().splitlines()
    return state[0] if state else "unknown"


def active_ntp_unit(run_command):
    for unit in NTP_UNITS:
        if unit_active(run_command, unit)[1]:
            return unit
    raise ValueError("no-active-ntp-provider")


def as_user(user, *argv):
    """argv run by `user`'s systemd manager: outside any logind session,
    as GNOME 51 launches apps, so polkit resolves the user's graphical
    session and reads the caller's own group credentials."""
    return ["systemd-run", "--user", "--machine", f"{user}@", "--wait",
            "--pipe", "--quiet", *argv]


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
    """Read-only RTC policy evidence."""
    return {
        "local_rtc": show.get("LocalRTC", ""),
        "rtc_time_usec": show.get("RTCTimeUSec", ""),
        "localtime_link": read_localtime_link(),
        "adjtime": read_adjtime(),
    }


def round_trip_target(run_command, original, target=None):
    zones = list_timezones(run_command)
    if target is None:
        target = next((zone for zone in ROUND_TRIP_ZONES if zone != original), None)
    if target is None:
        raise ValueError("no-round-trip-target-differs-from-original")
    if target not in zones:
        raise ValueError(f"round-trip-target-not-listed: {target}")
    return target


def collect_timezone_round_trip(run_command, target=None, observe=None, record=None):
    """Set a new zone through timedate1, verify, revert, verify the revert.

    `observe(record)`, when given, runs while the new zone is in force
    (the panel clock follow-up). The revert runs in a finally block: a
    failed verification still restores the original zone, and the
    evidence says whether the restoration itself was observed.
    """
    original = timedate1_timezone(run_command)
    target = round_trip_target(run_command, original, target)
    record = {} if record is None else record
    record.update({"original": original, "target": target, "reverted": False,
                   "revert_verified": False})
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
        if observe is not None:
            observe(record)
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


def collect_manual_time(run_command, offset=MANUAL_OFFSET_SECONDS,
                        monotonic=time.monotonic, observe=None, record=None):
    """Manual time the way the Settings panel sets it, then restored.

    The panel turns Automatic Date & Time off (SetNTP false), then writes
    an absolute time (SetTime). While NTP is on, timedated refuses
    SetTime, which is why GNOME keeps the Date & Time row insensitive
    then. The real time is carried across the jump on the monotonic
    clock and written back before NTP is restored, so the guest never
    depends on a network resync. `observe(record)` runs while the manual
    time is in force.
    """
    show = timedate1_show(run_command)
    unit = active_ntp_unit(run_command)
    record = {} if record is None else record
    record.update({"offset": offset, "unit": unit, "ntp_before": show.get("NTP", ""),
                   "zone": show.get("Timezone", ""),
                   "enabled_before": unit_enabled(run_command, unit)})
    if show.get("NTP") != "yes":
        raise ValueError("manual-time-needs-ntp-on-first")
    code, _, err = set_time(run_command, system_epoch(run_command))
    record["set_while_ntp"] = {"exit": code, "error": err.strip()}
    base, base_monotonic = system_epoch(run_command), monotonic()

    def real_now():
        return base + (monotonic() - base_monotonic)

    clock_moved = False
    try:
        code, _, err = set_ntp(run_command, False)
        record["ntp_off"] = {"exit": code, "error": err.strip()}
        if code != 0:
            raise ValueError(f"set-ntp-false-failed: {err.strip()}")
        record["ntp_after_off"] = timedate1_show(run_command).get("NTP", "")
        record["unit_after_off"] = unit_active(run_command, unit)[0]
        target = int(real_now()) + offset
        code, _, err = set_time(run_command, target)
        record["set"] = {"exit": code, "error": err.strip(), "target": target}
        if code != 0:
            raise ValueError(f"set-time-failed: {err.strip()}")
        clock_moved = True
        record["set_delta"] = system_epoch(run_command) - (real_now() + offset)
        if observe is not None:
            observe(record)
    finally:
        if clock_moved:
            code, _, err = set_time(run_command, round(real_now()))
            record["restore_set"] = {"exit": code, "error": err.strip()}
            record["restored_delta"] = system_epoch(run_command) - real_now()
        code, _, err = set_ntp(run_command, True)
        record["ntp_on"] = {"exit": code, "error": err.strip()}
        record["ntp_restored"] = timedate1_show(run_command).get("NTP", "")
        record["unit_restored"] = unit_active(run_command, unit)[0]
        record["enabled_after"] = unit_enabled(run_command, unit)
    return record


def collect_ntp_absent(run_command, record=None):
    """A stopped, then a missing (runtime-masked) NTP provider, restored.

    gnome-control-center binds the Automatic Date & Time switch to
    timedate1's NTP property and shows the row only while CanNTP is true
    (cc-datetime-page.c is_ntp_available/on_can_ntp_changed). The mask
    lives in /run, so even an interrupted run cannot outlive a reboot.
    """
    unit = active_ntp_unit(run_command)
    record = {} if record is None else record
    record["unit"] = unit

    def settle(ntp, can_ntp, timeout=5.0):
        # timedated learns of unit state changes asynchronously; poll the
        # expected pair for a bounded time and keep the last reading.
        deadline = time.monotonic() + timeout
        while True:
            show = timedate1_show(run_command)
            reading = {"NTP": show.get("NTP", ""), "CanNTP": show.get("CanNTP", "")}
            if reading == {"NTP": ntp, "CanNTP": can_ntp} or time.monotonic() >= deadline:
                return reading
            time.sleep(0.25)

    try:
        # A provider just restarted (the manual-time probe's SetNTP) must
        # have settled first, or stopping it cancels a start job instead.
        run_command(["systemctl", "start", unit])
        record["before"] = settle("yes", "yes")
        code, _, err = run_command(["systemctl", "stop", unit])
        record["stopped"] = {"exit": code, "error": err.strip(), **settle("no", "yes")}
        code, _, err = run_command(["systemctl", "mask", "--runtime", "--now", unit])
        record["masked"] = {"exit": code, "error": err.strip(), **settle("no", "no")}
        code, _, err = set_ntp(run_command, True)
        record["masked_set_ntp"] = {"exit": code, "error": err.strip()}
    finally:
        code, _, err = run_command(["systemctl", "unmask", "--runtime", unit])
        record["unmask"] = {"exit": code, "error": err.strip()}
        code, _, err = run_command(["systemctl", "start", unit])
        record["restored"] = {"exit": code, "error": err.strip(), **settle("yes", "yes"),
                              "unit": unit_active(run_command, unit)[0]}
    return record


def collect_polkit(run_command, user, target=None, rules=None, record=None):
    """The desktop user's own SetTimezone, judged against GNOME's rule.

    Interactive authorization is off, so polkit answers from rules alone:
    a privileged-group member is granted
    org.gnome.controlcenter.datetime.configure (implying
    org.freedesktop.timedate1.set-timezone); anyone else gets
    auth_admin_keep, which a non-interactive caller sees as a demand for
    interactive authentication.
    """
    rules = read_gnome_rules() if rules is None else rules
    group = parse_privileged_group(rules)
    if group is None:
        raise ValueError(f"gnome-polkit-rule-unreadable: {GNOME_RULES}")
    code, out, err = run_command(as_user(user, "id", "-Gn"))
    if code != 0:
        raise ValueError(f"desktop-user-groups-unavailable: {err.strip()}")
    groups = out.split()
    original = timedate1_timezone(run_command)
    target = round_trip_target(run_command, original, target)
    record = {} if record is None else record
    record.update({"user": user, "group": group, "groups": groups,
                   "expected": "allowed" if group in groups else "denied",
                   "before": original, "target": target})
    try:
        code, _, err = run_command(as_user(
            user, "busctl", "--allow-interactive-authorization=false", "call",
            BUS, OBJECT, BUS, "SetTimezone", "sb", target, "false"))
        record["exit"], record["error"] = code, err.strip()
        record["after"] = timedate1_timezone(run_command)
    finally:
        if timedate1_timezone(run_command) != original:
            code, _, err = set_timezone(run_command, original)
            record["revert_exit"], record["revert_error"] = code, err.strip()
        record["restored"] = timedate1_timezone(run_command) == original
    return record


CLOCK_LABEL = re.compile(r"(\d{1,2})[:\u2236](\d{2})(?:[:\u2236](\d{2}))?"
                         r"(?:\s*([AaPp][Mm]))?\s*$")


def panel_clock_labels(nodes):
    """Showing clock-shaped labels from a panel dump's node list.

    Same family as the G-SETTINGS-CLOCK predicate: a showing label whose
    name ends in a 24-hour HH:MM[:SS] or a 12-hour H:MM[:SS] AM/PM
    (GnomeWallClock's two formats). Hidden nodes and non-clock labels
    are ignored.
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
    meridiem = (match.group(4) or "").upper()
    if meridiem:
        if not 1 <= hour <= 12:
            return None
        hour = hour % 12 + (12 if meridiem == "PM" else 0)
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


def best_clock_delta(nodes, epoch, zone):
    """(label, delta) of the panel label closest to `epoch` in `zone`."""
    deltas = [(label, clock_delta_seconds(epoch, label, zone))
              for label in panel_clock_labels(nodes)]
    deltas = [(label, delta) for label, delta in deltas if delta is not None]
    return min(deltas, key=lambda item: item[1]) if deltas else (None, None)


def panel_dump(app="tuna-shell-gtk", timeout=10.0, limit=5000):
    """The shell's accessible nodes over AT-SPI, with the epoch after.

    Runs as the desktop user. Each node: role, name, showing -- the
    fields the clock predicates read.
    """
    import gi
    gi.require_version("Atspi", "2.0")
    from gi.repository import Atspi
    Atspi.set_timeout(2000, 2000)
    deadline = time.monotonic() + timeout
    while True:
        desktop = Atspi.get_desktop(0)
        target = None
        for index in range(desktop.get_child_count()):
            child = desktop.get_child_at_index(index)
            if child is not None and child.get_name() == app:
                target = child
                break
        if target is not None:
            break
        if time.monotonic() >= deadline:
            raise ValueError(f"panel-app-not-on-accessibility-bus: {app}")
        time.sleep(0.5)
    nodes, stack = [], [target]
    while stack and len(nodes) < limit:
        node = stack.pop()
        try:
            states = node.get_state_set()
            nodes.append({"role": node.get_role_name(), "name": node.get_name() or "",
                          "showing": states.contains(Atspi.StateType.SHOWING)})
            count = node.get_child_count()
        except Exception:
            continue
        for index in range(count - 1, -1, -1):
            child = node.get_child_at_index(index)
            if child is not None:
                stack.append(child)
    return {"nodes": nodes, "epoch": int(time.time())}


def dump_panel_as(run_command, user):
    """panel_dump() in `user`'s session: in-process when that is us."""
    if pwd.getpwuid(os.getuid()).pw_name == user:
        return panel_dump()
    code, out, err = run_command(as_user(user, sys.executable, os.path.abspath(__file__),
                                         "panel-dump"), timeout=60)
    if code != 0:
        raise ValueError(f"panel-dump-failed: {err.strip() or f'exit={code}'}")
    return json.loads(out)


def follow_panel(dump, zone, stale_zone, stale_shift=0, deadline_seconds=PANEL_FOLLOW_SECONDS,
                 tolerance=CLOCK_TOLERANCE_SECONDS, monotonic=time.monotonic):
    """Dump the panel until a clock label shows the wall time of `zone`
    and no longer the stale one -- the wall time of `stale_zone`
    `stale_shift` seconds earlier, what an unrepainted clock still shows
    -- or the deadline passes; every attempt is kept."""
    attempts = []
    deadline = monotonic() + deadline_seconds
    while True:
        snapshot = dump()
        label, delta = best_clock_delta(snapshot["nodes"], snapshot["epoch"], zone)
        stale = (None if label is None else
                 clock_delta_seconds(snapshot["epoch"] - stale_shift, label, stale_zone))
        attempts.append({"epoch": snapshot["epoch"], "zone": zone,
                         "stale_zone": stale_zone, "stale_shift": stale_shift,
                         "labels": panel_clock_labels(snapshot["nodes"]),
                         "label": label, "delta": delta, "stale_delta": stale})
        if followed(attempts[-1], tolerance) or monotonic() >= deadline:
            return attempts
        time.sleep(1)


def followed(attempt, tolerance):
    """The label shows the new wall time, distinguishable from the old."""
    return (attempt.get("delta") is not None and attempt["delta"] <= tolerance
            and attempt.get("stale_delta") is not None and attempt["stale_delta"] > tolerance)


def evaluate(evidence, clock_tolerance=CLOCK_TOLERANCE_SECONDS):
    """Evaluate every gate from a collect() bundle.

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
    if trip.get("target") and trip.get("observed") == trip.get("target") and trip.get("revert_verified"):
        results.append(("G-DATETIME-TZ", True,
                        f"{trip.get('original')} -> {trip.get('target')} observed, "
                        f"reverted to {trip.get('original')} and verified"))
    else:
        results.append(("G-DATETIME-TZ", False,
                        f"round-trip unevaluable or broken: "
                        f"{json.dumps({k: v for k, v in trip.items() if k != 'panel'}, sort_keys=True)}"))
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
                        f"no RTC hardware claim"))
    else:
        results.append(("G-DATETIME-RTC", False,
                        f"RTC policy unevaluable or not UTC: {json.dumps(rtc, sort_keys=True)}"))
    clock = evidence.get("panel_clock") or {}
    labels = panel_clock_labels(clock.get("nodes"))
    if not labels:
        results.append(("G-DATETIME-CLOCK", False, "no showing clock label in the panel dump"))
    elif clock.get("system_epoch") is None or not clock.get("timezone"):
        results.append(("G-DATETIME-CLOCK", False, "panel dump lacks captured system epoch/timezone"))
    else:
        label, delta = best_clock_delta(clock["nodes"], clock["system_epoch"], clock["timezone"])
        if label is None:
            results.append(("G-DATETIME-CLOCK", False,
                            f"unparseable clock labels: {labels}"))
        else:
            results.append(("G-DATETIME-CLOCK", delta <= clock_tolerance,
                            f"panel {label!r} vs system epoch {clock['system_epoch']} "
                            f"({clock['timezone']}): delta {delta:.0f}s, tolerance {clock_tolerance}s"))
    baseline = results[-1][1]
    results.append(evaluate_clock_follow(
        "G-DATETIME-CLOCK-TZ", trip.get("panel"), clock_tolerance,
        lambda: f"{trip.get('original')} -> {trip.get('target')}", baseline))
    manual = evidence.get("manual_time") or {}
    results.append(evaluate_manual(manual))
    results.append(evaluate_clock_follow(
        "G-DATETIME-CLOCK-SET", manual.get("panel"), clock_tolerance,
        lambda: f"SetTime +{manual.get('offset')}s", baseline))
    results.append(evaluate_polkit(evidence.get("polkit") or {}))
    results.append(evaluate_ntp_absent(evidence.get("ntp_absent") or {}))
    errors = evidence.get("errors") or {}
    return [(gate, passed, detail if passed or not errors.get(GATE_PROBES.get(gate))
             else f"{detail}; probe error: {errors[GATE_PROBES[gate]]}")
            for gate, passed, detail in results]


# The collect() probe whose error explains a failing gate.
GATE_PROBES = {
    "G-DATETIME-TZ": "timezone_round_trip",
    "G-DATETIME-CLOCK-TZ": "timezone_round_trip",
    "G-DATETIME-TZ-REJECT": "timezone_rejection",
    "G-DATETIME-CLOCK": "panel_clock",
    "G-DATETIME-MANUAL": "manual_time",
    "G-DATETIME-CLOCK-SET": "manual_time",
    "G-DATETIME-POLKIT": "polkit",
    "G-DATETIME-NTP-ABSENT": "ntp_absent",
}


def evaluate_clock_follow(gate, attempts, tolerance, what, baseline):
    """The last panel dump must show the new wall time, not the old one,
    within PANEL_FOLLOW_SECONDS of the change -- from a panel that showed
    the right time before it (`baseline`, the G-DATETIME-CLOCK result)."""
    if not attempts:
        return (gate, False, "no panel dumps taken after the change (needs --desktop-user)")
    last = attempts[-1]
    span = last["epoch"] - attempts[0]["epoch"]
    if last.get("delta") is None:
        return (gate, False, f"{what()}: no parseable clock label in {last.get('labels')} "
                             f"after {len(attempts)} dump(s)")
    passed = (baseline and followed(last, tolerance)
              and span <= PANEL_FOLLOW_SECONDS + 5)
    stale = last.get("stale_delta")
    return (gate, bool(passed),
            f"{what()}: panel {last['label']!r} vs {last['zone']} wall time: delta "
            f"{last['delta']:.0f}s, vs the unrepainted reading: "
            f"{'?' if stale is None else f'{stale:.0f}s'}; {len(attempts)} dump(s) over "
            f"{span}s (tolerance {tolerance}s, deadline {PANEL_FOLLOW_SECONDS}s)"
            + ("" if baseline else "; the panel was already wrong before the change"))


def evaluate_manual(manual):
    gate = "G-DATETIME-MANUAL"
    if not manual.get("set"):
        return (gate, False, f"manual time unevaluable: {json.dumps(manual, sort_keys=True)}")
    tolerance = MANUAL_TOLERANCE_SECONDS
    checks = {
        "SetTime refused while NTP on": (manual.get("set_while_ntp") or {}).get("exit", 0) != 0,
        "SetNTP(false) took NTP off": manual.get("ntp_after_off") == "no",
        "provider stopped": manual.get("unit_after_off") in ("inactive", "failed"),
        "SetTime accepted": manual["set"].get("exit") == 0,
        "clock moved by the offset": manual.get("set_delta") is not None
        and abs(manual["set_delta"]) <= tolerance,
        "real time written back": manual.get("restored_delta") is not None
        and abs(manual["restored_delta"]) <= tolerance,
        "NTP restored": manual.get("ntp_restored") == "yes"
        and manual.get("unit_restored") == "active",
        "provider enablement restored": manual.get("enabled_after") == manual.get("enabled_before"),
    }
    failed = [name for name, ok in checks.items() if not ok]
    detail = (f"{manual.get('unit')}: SetTime while NTP on -> exit "
              f"{(manual.get('set_while_ntp') or {}).get('exit')}; NTP off; "
              f"+{manual.get('offset')}s observed (delta {manual.get('set_delta')}s); "
              f"restored (delta {manual.get('restored_delta')}s), NTP={manual.get('ntp_restored')}")
    if failed:
        detail += f"; failed: {', '.join(failed)}"
    return (gate, not failed, detail)


def evaluate_polkit(polkit):
    gate = "G-DATETIME-POLKIT"
    expected = polkit.get("expected")
    if expected not in ("allowed", "denied") or "exit" not in polkit:
        return (gate, False, f"polkit probe unevaluable: {json.dumps(polkit, sort_keys=True)}")
    if expected == "allowed":
        passed = (polkit["exit"] == 0 and polkit.get("after") == polkit.get("target")
                  and polkit.get("restored"))
        outcome = f"allowed (zone -> {polkit.get('after')}, restored={polkit.get('restored')})"
    else:
        passed = (polkit["exit"] != 0 and INTERACTIVE_DENIAL in (polkit.get("error") or "")
                  and polkit.get("after") == polkit.get("before") and polkit.get("restored"))
        outcome = f"refused ({polkit.get('error')!r}), zone stayed {polkit.get('after')}"
    member = "in" if expected == "allowed" else "not in"
    return (gate, bool(passed),
            f"{polkit.get('user')} ({member} {polkit.get('group')}) non-interactive "
            f"SetTimezone {polkit.get('target')}: expected {expected}, {outcome}")


def evaluate_ntp_absent(absent):
    gate = "G-DATETIME-NTP-ABSENT"
    stopped, masked = absent.get("stopped") or {}, absent.get("masked") or {}
    restored = absent.get("restored") or {}
    if not stopped or not masked or not restored:
        return (gate, False, f"missing-service probe unevaluable: {json.dumps(absent, sort_keys=True)}")
    checks = {
        "stopped: NTP=no CanNTP=yes": stopped.get("NTP") == "no" and stopped.get("CanNTP") == "yes",
        "masked: NTP=no CanNTP=no": masked.get("NTP") == "no" and masked.get("CanNTP") == "no",
        "masked: SetNTP(true) refused": (absent.get("masked_set_ntp") or {}).get("exit", 0) != 0,
        "restored: NTP=yes CanNTP=yes active": restored.get("NTP") == "yes"
        and restored.get("CanNTP") == "yes" and restored.get("unit") == "active",
    }
    failed = [name for name, ok in checks.items() if not ok]
    detail = (f"{absent.get('unit')}: stopped NTP={stopped.get('NTP')}/CanNTP={stopped.get('CanNTP')}; "
              f"masked NTP={masked.get('NTP')}/CanNTP={masked.get('CanNTP')}, SetNTP(true) "
              f"{(absent.get('masked_set_ntp') or {}).get('error')!r}; restored "
              f"NTP={restored.get('NTP')}/CanNTP={restored.get('CanNTP')}")
    if failed:
        detail += f"; failed: {', '.join(failed)}"
    return (gate, not failed, detail)


def collect(run_command=run, *, target_tz=None, desktop_user=None,
            clock_tolerance=CLOCK_TOLERANCE_SECONDS, read_only=False,
            dump=None, monotonic=time.monotonic):
    """Gather the evidence bundle on this machine.

    Each write probe restores what it changed before the next starts;
    a probe that cannot run records its error and the gate it feeds
    fails as unevaluable.
    """
    ntp = collect_ntp(run_command)
    evidence = {
        "ntp": ntp,
        "rtc": collect_rtc(run_command, ntp["show"]),
        "collected_at_epoch": system_epoch(run_command),
        "read_only": read_only,
        "errors": {},
    }

    def probe(key, function):
        # The record is in the bundle before the probe runs, so a probe
        # that fails halfway still leaves what it observed.
        record = evidence[key] = {}
        try:
            function(record)
        except ValueError as error:
            evidence["errors"][key] = str(error)

    if desktop_user is not None and dump is None:
        dump = lambda: dump_panel_as(run_command, desktop_user)  # noqa: E731
    if dump is not None:
        def panel_clock(record):
            snapshot = dump()
            record.update({"nodes": snapshot["nodes"], "system_epoch": snapshot["epoch"],
                           "timezone": ntp["show"].get("Timezone", ""),
                           "tolerance": clock_tolerance})
        probe("panel_clock", panel_clock)
    if read_only:
        return evidence

    def follow(zone_key, stale_zone_key, shift_key=None):
        if dump is None:
            return None
        return lambda record: record.__setitem__("panel", follow_panel(
            dump, record[zone_key], record[stale_zone_key],
            record[shift_key] if shift_key else 0,
            tolerance=clock_tolerance, monotonic=monotonic))

    probe("timezone_round_trip", lambda record: collect_timezone_round_trip(
        run_command, target=target_tz, observe=follow("target", "original"), record=record))
    probe("timezone_rejection", lambda record: record.update(
        collect_timezone_rejection(run_command)))
    if desktop_user is not None:
        probe("polkit", lambda record: collect_polkit(
            run_command, desktop_user, target=target_tz, record=record))
    probe("manual_time", lambda record: collect_manual_time(
        run_command, monotonic=monotonic, observe=follow("zone", "zone", "offset"),
        record=record))
    probe("ntp_absent", lambda record: collect_ntp_absent(run_command, record=record))
    return evidence


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="command", required=True)
    get = sub.add_parser("collect", help="gather evidence on this machine")
    get.add_argument("--out", required=True)
    get.add_argument("--read-only", action="store_true",
                     help="skip every write probe")
    get.add_argument("--target-tz", default=None)
    get.add_argument("--desktop-user", default=None,
                     help="the logged-in desktop user: panel dumps and the polkit probe")
    get.add_argument("--clock-tolerance", type=int, default=CLOCK_TOLERANCE_SECONDS)
    check = sub.add_parser("check", help="evaluate gates from evidence")
    check.add_argument("evidence")
    check.add_argument("--assertions", default=None)
    check.add_argument("--clock-tolerance", type=int, default=CLOCK_TOLERANCE_SECONDS)
    dump = sub.add_parser("panel-dump", help="the shell's accessible labels, as the desktop user")
    dump.add_argument("--app", default="tuna-shell-gtk")
    dump.add_argument("--timeout", type=float, default=10.0)
    args = parser.parse_args(argv)
    if args.command == "panel-dump":
        json.dump(panel_dump(args.app, args.timeout), sys.stdout)
        sys.stdout.write("\n")
        return 0
    if args.command == "collect":
        bundle = collect(target_tz=args.target_tz, desktop_user=args.desktop_user,
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
