"""Actual GNOME automatic-timezone stage on the original native desktop.

The fixture is GeoClue's documented static source, not GPS or a replacement
service. The positive operation is performed only by packaged gsd-datetime.
"""
from __future__ import annotations

import functools
import importlib.util
import math
from pathlib import Path
import time


MANUAL_ZONE = "Pacific/Auckland"
AUTOMATIC_ZONE = "America/New_York"
LATITUDE = 40.6893129
LONGITUDE = -74.0445531
NEGATIVE_SECONDS = 10
LOCATION_SECONDS = 30
PANEL_SECONDS = 10
CLOCK_TOLERANCE_SECONDS = 120


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


@functools.lru_cache(maxsize=1)
def clock_parser():
    """Reuse the existing Date & Time clock formats and stale-zone check."""
    source = Path(__file__).with_name("tuna-datetime-proof.py")
    spec = importlib.util.spec_from_file_location("tuna_autotz_clock_parser", source)
    if spec is None or spec.loader is None:
        raise RuntimeError("installed Date & Time clock parser unavailable")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def same_service(observed, original):
    fields = ("pid", "uid", "start_ticks", "exe", "exe_sha256",
              "package", "unique_owner", "unit")
    return (observed.get("active") is True
            and all(observed.get(field) == original.get(field) for field in fields)
            and all(original.get(field) is not None for field in fields))


def local_zone(observed, zone):
    timedate = observed.get("timedate", {})
    localtime = timedate.get("localtime", {})
    digest = localtime.get("sha256", "")
    return (timedate.get("zone") == zone
            and localtime.get("matches_zone") is True
            and isinstance(digest, str) and len(digest) == 64)


def location_distance(latitude, longitude):
    first, second = math.radians(LATITUDE), math.radians(latitude)
    delta_latitude = second - first
    delta_longitude = math.radians(longitude - LONGITUDE)
    haversine = (math.sin(delta_latitude / 2) ** 2
                 + math.cos(first) * math.cos(second)
                 * math.sin(delta_longitude / 2) ** 2)
    return 6371000 * 2 * math.asin(math.sqrt(min(1, max(0, haversine))))


def datetime_location(observed):
    """Admit the real daemon's client, not another app's GeoClue client."""
    matches = []
    for client in observed.get("geoclue", {}).get("clients", []):
        if (client.get("desktop_id") != "gnome-datetime-panel"
                or client.get("active") is not True
                or client.get("evidence_source") != "passive-original-gsd-system-bus"
                or client.get("owner_pid") != observed.get("gsd", {}).get("pid")
                or client.get("owner_uid") != observed.get("gsd", {}).get("uid")
                or not isinstance(client.get("owner_bus_name"), str)
                or not client["owner_bus_name"].startswith(":")):
            continue
        location = client.get("location")
        if location is None:
            continue
        require(isinstance(location, dict), "actual GeoClue location payload is malformed")
        values = [location.get(field) for field in ("latitude", "longitude", "accuracy")]
        if any(isinstance(value, bool) or not isinstance(value, (int, float))
               or not math.isfinite(value) for value in values):
            continue
        latitude, longitude, accuracy = values
        # The daemon requests CITY accuracy. Admit the named point only within
        # the actual reported radius, capped at 50 km; do not demand unblurred
        # GPS precision from GeoClue's privacy-limited city-level client.
        if (not -90 <= latitude <= 90 or not -180 <= longitude <= 180
                or not 0 < accuracy <= 50000):
            continue
        if location_distance(latitude, longitude) <= max(accuracy, 100):
            matches.append(client)
    return matches


def daemon_timezone_call(observed, original):
    return [call for call in observed.get("timezone_calls", [])
            if call.get("zone") == AUTOMATIC_ZONE
            and call.get("interactive") is True
            and isinstance(call.get("sender"), str)
            and call["sender"].startswith(":")
            and call.get("sender_pid") == original.get("pid")
            and call.get("sender_uid") == original.get("uid")]


def positive_ready(observed, original):
    return (observed.get("settings") == {"master": True, "automatic": True}
            and same_service(observed.get("gsd", {}), original)
            and local_zone(observed, AUTOMATIC_ZONE)
            and bool(datetime_location(observed))
            and bool(daemon_timezone_call(observed, original)))


def panel_reading(tree, epoch, zone, stale_zone=None):
    require(isinstance(epoch, (int, float)) and not isinstance(epoch, bool),
            "actual timedated epoch missing")
    parser = clock_parser()
    label, delta = parser.best_clock_delta(tree.get("nodes"), epoch, zone)
    stale_label, stale_delta = (parser.best_clock_delta(tree.get("nodes"), epoch, stale_zone)
                                if stale_zone else (None, None))
    return {"label": label, "delta_seconds": delta,
            "stale_label": stale_label, "stale_delta_seconds": stale_delta,
            "epoch": epoch, "zone": zone, "stale_zone": stale_zone}


def clock_matches(reading, exclude_stale=False):
    delta = reading.get("delta_seconds")
    stale = reading.get("stale_delta_seconds")
    return (delta is not None and delta <= CLOCK_TOLERANCE_SECONDS
            and (not exclude_stale
                 or (stale is not None and stale > CLOCK_TOLERANCE_SECONDS)))


def negative_sample(observed, baseline, original, master, automatic):
    require(observed.get("settings") == {"master": master, "automatic": automatic},
            "negative-control location/automatic settings did not take effect")
    require(same_service(observed.get("gsd", {}), original),
            "packaged datetime service changed or stopped during negative control")
    require(local_zone(observed, MANUAL_ZONE),
            "automatic timezone changed the manual zone while disabled")
    require(observed["timedate"]["localtime"]["sha256"]
            == baseline["timedate"]["localtime"]["sha256"],
            "/etc/localtime changed while automatic timezone was disabled")


def hold_manual(desktop, label, baseline, original, master, automatic):
    """Observe the entire fixed negative window; an early unchanged read is not a pass."""
    started = time.monotonic()
    samples = []
    while True:
        observed = desktop.guest("autotz", "snapshot")
        negative_sample(observed, baseline, original, master, automatic)
        samples.append({"elapsed_seconds": time.monotonic() - started, "observed": observed})
        desktop.retain(label + "-sample", samples[-1])
        if samples[-1]["elapsed_seconds"] >= NEGATIVE_SECONDS:
            break
        time.sleep(min(0.5, NEGATIVE_SECONDS - samples[-1]["elapsed_seconds"]))
    require(len(samples) >= 2, "negative-control window has no independent later sample")
    return {"duration_seconds": samples[-1]["elapsed_seconds"], "samples": samples}


def run(desktop, *, manage_fixture=True):
    """Use the original boot; the Core profile may own the shared fixture."""
    result = {"scope": "actual-gsd-datetime-geoclue-static-source",
              "static_emulation_not_gps": True, "source_timezone": AUTOMATIC_ZONE,
              "manual_timezone": MANUAL_ZONE, "qualified": False,
              "fixture_restoration_owner": "stage" if manage_fixture else "profile",
              "profile_restoration_required": not manage_fixture}
    saved = False
    fixture_attempted = False
    failure = None
    cleanup_errors = []
    try:
        shell = desktop.shell_actor()
        original = desktop.guest("autotz", "save")
        saved = True
        result["original"] = original
        gsd = original["gsd"]
        require(gsd.get("active") is True and gsd.get("uid") == shell["uid"],
                "original user's packaged gsd-datetime is not active")
        desktop.guest("autotz", "configure", master=False, automatic=False)
        if manage_fixture:
            fixture_attempted = True
            result["fixture"] = desktop.guest("autotz", "fixture-prepare")
        else:
            result["fixture"] = desktop.guest("autotz", "fixture-observe")
        require(result["fixture"].get("prepared") is True,
                "official GeoClue static source was not admitted")
        baseline = desktop.guest("autotz", "manual-baseline")
        require(local_zone(baseline, MANUAL_ZONE), "manual negative-control zone not observed")
        require(same_service(baseline.get("gsd", {}), gsd),
                "manual baseline no longer has the original datetime daemon")
        result["manual_baseline"] = baseline
        baseline_tree = desktop.tree(shell)
        baseline_clock = panel_reading(baseline_tree, baseline["timedate"]["epoch"], MANUAL_ZONE)
        if not clock_matches(baseline_clock):
            def read_manual_clock():
                observed = desktop.guest("autotz", "snapshot")
                return panel_reading(desktop.tree(shell), observed["timedate"]["epoch"], MANUAL_ZONE)
            baseline_clock = desktop.await_state(
                "autotz-manual-panel-clock", read_manual_clock, clock_matches, timeout=PANEL_SECONDS)
        result["manual_clock"] = baseline_clock
        desktop.observe("autotz-manual-baseline", shell)

        desktop.guest("autotz", "configure", master=False, automatic=True)
        result["master_off"] = hold_manual(
            desktop, "autotz-master-off", baseline, gsd, False, True)
        desktop.guest("autotz", "configure", master=True, automatic=False)
        result["automatic_off"] = hold_manual(
            desktop, "autotz-automatic-off", baseline, gsd, True, False)

        monitor = desktop.guest("autotz", "watch-start")
        require(monitor.get("monitor", {}).get("ready") is True,
                "actual timedate1 caller monitor did not acknowledge readiness")
        result["monitor"] = monitor
        desktop.guest("autotz", "configure", master=True, automatic=True)
        actual = desktop.await_state(
            "autotz-geoclue-daemon-timezone",
            lambda: desktop.guest("autotz", "snapshot"),
            lambda value: positive_ready(value, gsd), timeout=LOCATION_SECONDS)
        result["automatic"] = actual

        def read_changed_clock():
            observed = desktop.guest("autotz", "snapshot")
            require(positive_ready(observed, gsd),
                    "automatic-timezone provenance changed before clock observation")
            tree = desktop.tree(shell)
            return {"clock": panel_reading(tree, observed["timedate"]["epoch"],
                                           AUTOMATIC_ZONE, MANUAL_ZONE),
                    "snapshot": observed, "tree": tree}
        result["panel"] = desktop.await_state(
            "autotz-panel-follows-service", read_changed_clock,
            lambda value: clock_matches(value["clock"], exclude_stale=True),
            timeout=PANEL_SECONDS)
        result["frame"] = desktop.observe("autotz-automatic-new-york", shell)
    except Exception as error:
        failure = error
        result["failure"] = {"type": type(error).__name__, "detail": str(error)}
    finally:
        if saved:
            try:
                desktop.guest("autotz", "configure", master=False, automatic=False)
            except Exception as error:
                cleanup_errors.append({"operation": "disable-before-restore", "detail": str(error)})
        if fixture_attempted:
            try:
                # A dead or changed service must not prevent restoring owned
                # durable files. The fixture helper validates its saved nonce.
                restored = desktop.guest("autotz", "fixture-restore")
                result["fixture_restored"] = restored
                require(restored.get("restored") is True,
                        "original GeoClue files/service restoration was not verified")
            except Exception as error:
                cleanup_errors.append({"operation": "fixture-restore", "detail": str(error)})
        if saved and manage_fixture:
            try:
                restored = desktop.guest("autotz", "restore")
                result["settings_restored"] = restored
                require(restored.get("restored") is True,
                        "original timezone/settings restoration was not verified")
            except Exception as error:
                cleanup_errors.append({"operation": "settings-restore", "detail": str(error)})
        result["cleanup_errors"] = cleanup_errors
        result["qualified"] = failure is None and not cleanup_errors
        desktop.retain("automatic-timezone-stage", result)
    if failure is not None:
        raise RuntimeError("actual automatic-timezone stage failed; retained original evidence") from failure
    if cleanup_errors:
        raise RuntimeError("actual automatic-timezone fixture restoration failed: " + str(cleanup_errors))
    return result