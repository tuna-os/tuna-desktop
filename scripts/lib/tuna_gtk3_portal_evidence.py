"""Fail-closed evidence checks for the shared installed GTK3 portal journey.

Raw WAYLAND_DEBUG=client remains the authority for which foreign version GDK
actually used. Advertising/binding a global alone is not export evidence.
Never hide v2 to manufacture a v1 toolkit claim.
"""
import hashlib
import json
from pathlib import Path
import re


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def read_case(out, suffix):
    return json.loads(out.with_suffix(suffix).read_text())


def wire_export(text, handle):
    exports = list(re.finditer(
        r"zxdg_exporter_v([12])[@#]\d+\.(export_toplevel|export)\(new id zxdg_exported_v\1[@#](\d+), wl_surface[@#](\d+)\)", text))
    require(len(exports) == 1, "GTK3 wire must contain exactly one actual surface export")
    export, = exports
    version, operation, exported_id, surface_id = export.groups()
    require(operation == ("export_toplevel" if version == "2" else "export"), "Foreign export opcode/version mismatch")
    object_pattern = rf"zxdg_exported_v{version}[@#]{exported_id}"
    received = re.search(object_pattern + r'\.handle\("' + re.escape(handle) + r'"\)', text[export.end():])
    require(received is not None, "GDK callback handle does not match actual wire export event")
    surface_pattern = rf"wl_surface[@#]{surface_id}"
    attached = list(re.finditer(surface_pattern + r"\.attach\(wl_buffer[@#]\d+,", text[:export.start()]))
    require(bool(attached), "Exported GTK3 surface has no actual non-null attached buffer")
    require(re.search(surface_pattern + r"\.commit\(\)", text[attached[-1].end():export.start()]) is not None,
            "GTK3 exported surface did not commit its actual attached buffer before export")
    destroyed = re.search(object_pattern + r"\.destroy\(\)", text[export.end():])
    require(destroyed is not None, "GDK did not destroy its actual export")
    require(destroyed.start() > received.start(), "Export was destroyed before its actual handle event")
    return {"xdg_foreign_version": int(version), "exported_object_id": int(exported_id),
            "surface_object_id": int(surface_id), "real_buffer_committed_before_export": True,
            "real_export_destroyed": True, "v1_toolkit_proof": version == "1"}


def validate_case(out, method, decision, lifetime):
    out = Path(out)
    waiting = read_case(out, ".waiting.json")
    exported = read_case(out, ".parent-export.json")
    toolkit = read_case(out, ".toolkit.json")
    before = read_case(out, ".multiple-apps-before.json")
    parent = exported["parent"]
    require(parent["toolkit"] == "gtk3" and parent["mode"] == "wayland" and parent["xid"] is None,
            "Not an actual GTK3 ordinary Wayland caller")
    require(toolkit["pid"] == parent["pid"] and toolkit["uid"] == parent["uid"] != 0
            and toolkit["toolkit"] == "gtk3" and toolkit["version"][0] == 3,
            "Installed GTK3 library evidence does not identify original ordinary caller")
    require({record["name"] for record in toolkit["libraries"]} == {"libgtk-3.so", "libgdk-3.so"},
            "Both actual installed GTK3/GDK3 libraries are required")
    for library in toolkit["libraries"]:
        path = Path(library["path"])
        info = path.stat()
        require(info.st_uid == 0 and not info.st_mode & 0o022 and library["package"]
                and hashlib.sha256(path.read_bytes()).hexdigest() == library["sha256"],
                "Installed GTK3 library identity changed during journey")
    require(waiting["method"] == method and waiting["decision"] == decision
            and waiting["parent"]["id"] == parent["id"] and waiting["parent"]["pid"] == parent["pid"],
            "Portal request did not retain original GTK3 caller")
    require(before["parent"] == parent and before["unrelated"]["pid"] != parent["pid"]
            and before["unrelated"]["clicks"] == before["unrelated"]["keys"] == 1
            and before["scene"]["focused"] == before["unrelated_window_id"],
            "Independent real application B lacks its positive input/focus control")
    provider = waiting["provider"]
    require(provider["executable"] == str(Path("/usr/bin/nautilus").resolve(strict=True))
            and provider["pid"] not in (parent["pid"], before["unrelated"]["pid"]),
            "Real installed Nautilus C must be independent from A and B")
    initial = read_case(out, ".parent-before.json")
    require(initial["pid"] == parent["pid"] and initial["clicks"] == initial["keys"] == 1,
            "GTK3 original caller lacks both actual positive input controls")
    wire = wire_export(out.with_suffix(".log").read_text(), exported["handle"].removeprefix("wayland:"))
    require(exported["handle"].startswith("wayland:"), "No actual GDK-exported Wayland handle")
    if lifetime == "revoked":
        revoked = read_case(out, ".revoked-export.json")
        negative = read_case(out, ".revoked-parent.json")
        require(revoked["handle"] == exported["handle"] and revoked["parent"]["export_revoked"],
                "Negative did not reuse GDK's actual revoked handle")
        observations = negative["observations"]
        require(observations and observations[-1]["input"]["sequence"] >= negative["before"]["sequence"] + 10,
                "Revoked-handle negative lacks fresh original-caller observations")
        for row in observations:
            child, = [window for window in row["scene"]["windows"] if window["id"] == row["dialog_id"]]
            denied = (parent["id"], before["unrelated_window_id"])
            require(child["app_id"] == "org.gnome.Nautilus" and row["scene"]["focused"] == child["id"]
                    and child.get("parent_window_id") not in denied and child.get("application_window_id") not in denied,
                    "Revoked-handle negative retained an A/B parent association")
        after = read_case(out, ".revoked-after.json")
        require(after["parent"]["id"] == parent["id"] and after["parent"]["pid"] == parent["pid"]
                and after["parent_clicks"] == after["parent_keys"] == 1
                and after["unrelated"]["pid"] == before["unrelated"]["pid"]
                and after["unrelated"]["clicks"] == after["unrelated"]["keys"] == 1
                and all(window["id"] != after["dialog_id"] for window in after["scene"]["windows"]),
                "Actual revoked-handle cancellation lost A/B or retained Nautilus C")
    else:
        scene = read_case(out, ".wayland-parent.json")
        a, = [window for window in scene["windows"] if window["id"] == parent["id"]]
        c, = [window for window in scene["windows"] if window.get("parent_window_id") == parent["id"]
              and window.get("app_id") == "org.gnome.Nautilus"]
        require(c["application_window_id"] == a["id"] and scene["focused"] == c["id"],
                "Nautilus C is not parented/focused to original GTK3 A")
        ax, ay, aw, ah = a["rect"]
        cx, cy, cw, ch = c["rect"]
        require(abs(2 * cx + cw - (2 * ax + aw)) <= 1 and abs(2 * cy + ch - (2 * ay + ah)) <= 1,
                "Actual Nautilus C was not centered on original committed GTK3 geometry")
        blocked = read_case(out, ".parent-blocked.json")
        require(blocked["before"]["pid"] == blocked["after"]["pid"] == parent["pid"]
                and blocked["before"]["clicks"] == blocked["after"]["clicks"] == 1
                and blocked["after"]["sequence"] >= blocked["before"]["sequence"] + 10
                and blocked["scene"]["focused"] == c["id"],
                "Original GTK3 parent lacks the unchanged denied-input negative control")
        restored = read_case(out, ".multiple-apps-after.json")
        require(restored["parent"] == parent and restored["parent_clicks"] == restored["parent_keys"] == 2
                and restored["unrelated_before"] == before["unrelated"]
                and restored["unrelated_after"]["clicks"] == restored["unrelated_after"]["keys"] == 1
                and restored["scene"]["focused"] == parent["id"],
                "Original GTK3 pointer/keyboard input did not restore without leakage to B")
        probes = read_case(out, ".parent-pointer.json")
        require(len(probes) == 2 and all(probe["phase"] == "before_single_click"
                and probe["target"]["measurement"] == "gtk3-translated-sole-child-native-window"
                and probe["target"]["client_after_paint_count"] > 0 for probe in probes),
                "GTK3 actual measured-buffer pointer controls are incomplete")
        for probe in probes:
            require(probe["parent_rect"][2:] == probe["target"]["surface_size"]
                    == probe["target"]["client_last_painted_size"],
                    "GTK3 input used placeholder rather than currently committed real-buffer geometry")
    if decision == "close":
        closed = read_case(out, ".request-close.json")
        require(closed["provider"] == provider and not closed["response_emitted"]
                and len(closed["observations"]) == 10
                and all("failure" not in row for row in closed["observations"]),
                "Request.Close lacks its unchanged no-response teardown control")
    else:
        response = read_case(out, ".response.json")
        require(response["provider"] == provider and response["response"] == (0 if decision == "grant" else 1),
                "Actual Nautilus response/provider changed")
        if decision == "grant":
            content = read_case(out, ".content.json")
            path = Path(content["path"])
            require(path.stat().st_size == content["bytes"]
                    and hashlib.sha256(path.read_bytes()).hexdigest() == content["sha256"],
                    "Application file bytes do not match actual portal grant")
        else:
            require(not response["results"].get("uris"), "Cancel returned a file grant")
    return {"method": method, "decision": decision, "export_lifetime": lifetime,
            "caller": parent, "provider": provider, "installed_toolkit": toolkit,
            "wire": wire, "artifact_prefix": str(out)}
