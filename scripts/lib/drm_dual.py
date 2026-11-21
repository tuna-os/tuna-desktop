"""Pure checks of scripts/tuna-drm-dual, the two-output drm-smoke leg.

Each check takes what the leg reads (the kernel's KMS readback from
drm_kms_readback.py, the compositor's TUNA_COMPOSITOR_STATE document,
a GTK client's reports, a saved monitors.xml, a saved screenshot's
pixels) and returns a list of
problems; an empty list passes. They hold no I/O so
scripts/tests/test_drm_dual.py runs them on recorded and on old-behaviour
inputs without a kernel.
"""
import math
import xml.etree.ElementTree as ET

# wp_fractional_scale_v1 sends the preferred scale in 120ths.
FRACTIONAL_DENOMINATOR = 120
# tuna_shell_control::SCREENSHOT_NAMESPACE, the screenshot UI's layers.
SCREENSHOT_NAMESPACE = "tuna-screenshot-ui"
# The marker client's colour (magenta, which the desktop never shows) and
# how much of a capture it must cover: at least MARKER_MIN_SHARE of the
# output showing it, at most MARKER_MAX_SHARE of one that does not.
MARKER_RGB = (255, 0, 255)
MARKER_MIN_SHARE = 0.05
MARKER_MAX_SHARE = 0.005


def configfs_script(path, pipes):
    """sh lines creating a vkms configfs device at `path` with `pipes`
    plane/CRTC/encoder/connector pipes, every connector connected, each
    encoder bound to its own CRTC (Linux 6.19+ vkms configfs layout)."""
    lines = [f"C={path}", 'mkdir "$C"']
    for i in range(pipes):
        lines += [
            f'mkdir "$C/planes/plane{i}" "$C/crtcs/crtc{i}" "$C/encoders/encoder{i}" "$C/connectors/connector{i}"',
            f'echo 1 > "$C/planes/plane{i}/type"',
            f'ln -s "$C/crtcs/crtc{i}" "$C/planes/plane{i}/possible_crtcs"',
            f'ln -s "$C/crtcs/crtc{i}" "$C/encoders/encoder{i}/possible_crtcs"',
            f'ln -s "$C/encoders/encoder{i}" "$C/connectors/connector{i}/possible_encoders"',
            f'echo 1 > "$C/connectors/connector{i}/status"',
        ]
    lines.append('echo 1 > "$C/enabled"')
    return "\n".join(lines)


def configfs_teardown(path):
    return "\n".join([
        f"C={path}",
        'echo 0 > "$C/enabled" || true',
        'rm -f "$C"/planes/*/possible_crtcs/* "$C"/encoders/*/possible_crtcs/* "$C"/connectors/*/possible_encoders/*',
        'rmdir "$C"/planes/* "$C"/crtcs/* "$C"/encoders/* "$C"/connectors/* "$C"',
    ])


def apply_config(logical):
    """ApplyMonitorsConfig's logical monitors (`a(iiduba(ssa{sv}))`) from
    [(x, y, scale, primary, [(connector, mode_id), ...])], transform 0."""
    return [(x, y, float(scale), 0, bool(primary), [(name, mode, {}) for name, mode in monitors])
            for x, y, scale, primary, monitors in logical]


def current_modes(state_reply):
    """{connector: (mode_id, width, height, supported_scales)} for the
    current mode of each monitor in a GetCurrentState reply."""
    _serial, monitors, _logical, _props = state_reply
    out = {}
    for spec, modes, _ in monitors:
        current = [m for m in modes if m[6].get("is-current")]
        if len(current) == 1:
            mode = current[0]
            out[spec[0]] = (mode[0], mode[1], mode[2], list(mode[5]))
    return out


def fractional_scale(width, height, supported):
    """The supported scale nearest GNOME's 125 percent that is fractional
    (not an integer) and gives an integral logical size, or None."""
    candidates = [s for s in supported
                  if 1 < s < 2 and abs(s - round(s)) > 1e-6
                  and abs(width / s - round(width / s)) < 1e-3
                  and abs(height / s - round(height / s)) < 1e-3]
    return min(candidates, key=lambda s: abs(s - 1.25)) if candidates else None


def mixed_scale_mode(modes):
    """The smallest progressive mode of a GetCurrentState mode list
    (`a(siiddada{sv})`) that supports both scale 2 and a fractional scale:
    (mode_id, width, height, refresh, fractional), or None. Mutter only
    offers a scale whose logical size is at least 800x480 in area."""
    usable = []
    for mode_id, width, height, refresh, _preferred, supported, props in modes:
        if props.get("is-interlaced") or not any(abs(s - 2.0) < 1e-6 for s in supported):
            continue
        frac = fractional_scale(width, height, supported)
        if frac is not None:
            usable.append((width * height, (mode_id, width, height, refresh, frac)))
    return min(usable)[1] if usable else None


def wire_scale(scale):
    """The scale a client receives over wp_fractional_scale_v1."""
    return round(scale * FRACTIONAL_DENOMINATOR) / FRACTIONAL_DENOMINATOR


def by_connector(readback):
    return {row["connector"]: row for row in readback}


def lit(row):
    """Scanning out: connected, on an enabled, active CRTC with a framebuffer."""
    return bool(row and row["connection"] == "connected" and row["crtc"] is not None
                and row["enabled"] and row["active"] and row["fb"])


def check_extended(readback, modes):
    """Every connector in `modes` ({name: "WxH"}) lit on its own CRTC in
    that mode."""
    rows = by_connector(readback)
    problems = []
    for name, mode in modes.items():
        row = rows.get(name)
        if not lit(row):
            problems.append(f"{name} is not lit in the kernel: {row}")
        elif row["mode"] != mode:
            problems.append(f"{name} runs {row['mode']} in the kernel, not {mode}")
    crtcs = [rows[n]["crtc"] for n in modes if lit(rows.get(n))]
    if len(set(crtcs)) != len(crtcs):
        problems.append(f"connectors share a CRTC: {crtcs}")
    return problems


def check_mirrored(readback, leader, clone):
    """Mutter's mirror: both connectors lit on their own CRTCs in one mode
    and refresh (meta_monitor_config_manager mirrored logical monitor)."""
    rows = by_connector(readback)
    a, b = rows.get(leader), rows.get(clone)
    problems = []
    for name, row in ((leader, a), (clone, b)):
        if not lit(row):
            problems.append(f"{name} is not lit in the kernel: {row}")
    if not problems:
        if a["crtc"] == b["crtc"]:
            problems.append(f"{leader} and {clone} share CRTC {a['crtc']}")
        if (a["mode"], a["refresh"]) != (b["mode"], b["refresh"]):
            problems.append(f"mirrored modes differ: {leader} {a['mode']}@{a['refresh']}, "
                            f"{clone} {b['mode']}@{b['refresh']}")
    return problems


def check_disabled(readback, off, on):
    """`off` still connected but on no enabled CRTC; every `on` lit."""
    rows = by_connector(readback)
    problems = []
    row = rows.get(off)
    if not row or row["connection"] != "connected":
        problems.append(f"{off} is not a connected connector: {row}")
    elif row["crtc"] is not None and (row["enabled"] or row["active"]):
        problems.append(f"{off} still drives CRTC {row['crtc']} in the kernel: {row}")
    for name in on:
        if not lit(rows.get(name)):
            problems.append(f"{name} is not lit in the kernel: {rows.get(name)}")
    return problems


def outputs(state):
    return {o["name"]: o for o in state.get("outputs", [])}


def check_outputs(state, expected):
    """wl_outputs are exactly `expected` ({name: [x, y, w, h]})."""
    have = {name: o["rect"] for name, o in outputs(state).items()}
    if have != expected:
        return [f"wl_outputs are {have}, not {expected}"]
    return []


def display_monitor(state, connector):
    return next((m for m in state.get("display_config", {}).get("monitors", [])
                 if m.get("connector") == connector), None)


def check_display_config(state, expected):
    """DisplayConfig's monitors: {connector: (position or None, mirrors)}."""
    problems = []
    monitors = state.get("display_config", {}).get("monitors", [])
    names = sorted(m.get("connector") for m in monitors)
    if names != sorted(expected):
        problems.append(f"DisplayConfig lists {names}, not {sorted(expected)}")
    for name, (position, mirrors) in expected.items():
        monitor = display_monitor(state, name)
        if monitor is None:
            continue
        if monitor.get("position") != position or monitor.get("mirrors") != mirrors:
            problems.append(f"DisplayConfig {name} is at {monitor.get('position')} mirroring "
                            f"{monitor.get('mirrors')}, not at {position} mirroring {mirrors}")
    return problems


def kept_layouts(xml_text):
    """Every <configuration> of a Mutter monitors.xml as
    {"logical": [[connector, ...] per logical monitor], "disabled": [...]}."""
    root = ET.fromstring(xml_text)
    layouts = []
    for config in root.findall("configuration"):
        logical = [[c.text for c in lm.findall("monitor/monitorspec/connector")]
                   for lm in config.findall("logicalmonitor")]
        disabled = [c.text for c in config.findall("disabled/monitorspec/connector")]
        layouts.append({"logical": logical, "disabled": disabled})
    return layouts


def check_kept(xml_text, mirrored=None, disabled=None, connectors=()):
    """A saved configuration for exactly `connectors` holds the mirror
    (one logical monitor with both) or the disabled connector."""
    want = sorted(connectors)
    for layout in kept_layouts(xml_text):
        names = sorted([c for lm in layout["logical"] for c in lm] + layout["disabled"])
        if names != want:
            continue
        if mirrored and sorted(mirrored) in [sorted(lm) for lm in layout["logical"]]:
            return []
        if disabled and disabled in layout["disabled"] and all(disabled not in lm for lm in layout["logical"]):
            return []
        return [f"the saved configuration for {want} is {layout}"]
    return [f"no saved configuration for {want}: {kept_layouts(xml_text)}"]


def overlap(rect, other):
    w = min(rect[0] + rect[2], other[0] + other[2]) - max(rect[0], other[0])
    h = min(rect[1] + rect[3], other[1] + other[3]) - max(rect[1], other[1])
    return max(w, 0) * max(h, 0)


def output_overlapping_most(rect, state):
    """Mutter's monitor for a window: the greatest overlap, the primary on
    ties (crates/compositor/src/workspace_policy.rs on_secondary)."""
    best, best_area = None, -1
    for name, out in sorted(outputs(state).items(), key=lambda item: not item[1]["primary"]):
        area = overlap(rect, out["rect"])
        if area > best_area:
            best, best_area = name, area
    return best if best_area > 0 else None


def spans(rect, state, a, b):
    """The window overlaps both outputs."""
    outs = outputs(state)
    return a in outs and b in outs and overlap(rect, outs[a]["rect"]) > 0 and overlap(rect, outs[b]["rect"]) > 0


def check_client_scale(reported, scale):
    """The client's surface scale is the output's, as wp_fractional_scale
    delivers it."""
    if reported is None or abs(reported - wire_scale(scale)) > 1e-3:
        return [f"the client's surface scale is {reported}, not {wire_scale(scale):g} (output scale {scale:g})"]
    return []


def check_pointer(motion, pointer, rect, tolerance=1.0):
    """The client's surface-local motion is the compositor's logical
    pointer position relative to the window."""
    if motion is None or pointer is None or rect is None:
        return [f"no pointer sample (client {motion}, compositor {pointer}, window {rect})"]
    want = (pointer[0] - rect[0], pointer[1] - rect[1])
    if math.hypot(motion[0] - want[0], motion[1] - want[1]) > tolerance:
        return [f"the client saw the pointer at {tuple(round(v, 2) for v in motion)}, not the "
                f"logical {tuple(round(v, 2) for v in want)} (pointer {pointer}, window at {rect[:2]})"]
    return []


def window_by_id(state, wid):
    return next((w for w in state.get("windows", []) if w.get("id") == wid), None)


def check_workspace_split(state, primary_window, secondary_window, secondary_sticky):
    """After a workspace switch away from both windows' workspace: the
    primary-output window is hidden (throttled, off the active workspace);
    the secondary-output window shows on the active workspace when
    `secondary_sticky` (workspaces-only-on-primary), else hides too."""
    problems = []
    active = state.get("active_workspace")
    throttled = set(state.get("throttled_windows", []))
    p, s = window_by_id(state, primary_window), window_by_id(state, secondary_window)
    if p is None or s is None:
        return [f"a window is missing: primary {p}, secondary {s}"]
    if p.get("workspace") == active or primary_window not in throttled:
        problems.append(f"the primary-output window {primary_window} still shows on workspace {active}: {p}")
    if secondary_sticky:
        if s.get("workspace") != active or secondary_window in throttled:
            problems.append(f"the secondary-output window {secondary_window} left workspace {active} "
                            f"with workspaces only on the primary: {s}, throttled {sorted(throttled)}")
    elif s.get("workspace") == active or secondary_window not in throttled:
        problems.append(f"the secondary-output window {secondary_window} stayed on workspace {active} "
                        f"with workspaces on every output: {s}")
    return problems


def check_window_on(state, wid, output=None, workspace=None, hidden=None):
    """The window overlaps `output` most, sits on `workspace` and is
    throttled (hidden) or not, for each given."""
    w = window_by_id(state, wid)
    if w is None:
        return [f"window {wid} is missing"]
    problems = []
    if output is not None and output_overlapping_most(w["rect"], state) != output:
        problems.append(f"window {wid} at {w['rect']} is not mostly on {output}")
    if workspace is not None and w.get("workspace") != workspace:
        problems.append(f"window {wid} is on workspace {w.get('workspace')}, not {workspace}")
    if hidden is not None and (wid in state.get("throttled_windows", [])) != hidden:
        problems.append(f"window {wid} is {'shown' if hidden else 'hidden'} "
                        f"(throttled {state.get('throttled_windows')})")
    return problems


def check_frame_rate(name, frames, seconds, shown, shown_min, hidden_max):
    """Frames a client drew over `seconds`: a shown one at least
    `shown_min` per second, a hidden one at most `hidden_max` per second
    (its frame callbacks run at HIDDEN_FRAME_INTERVAL, 995 ms)."""
    if shown and frames < shown_min * seconds:
        return [f"shown client {name} drew {frames} frames in {seconds} s (< {shown_min}/s)"]
    if not shown and frames > hidden_max * seconds:
        return [f"hidden client {name} drew {frames} frames in {seconds} s (> {hidden_max}/s)"]
    return []


def frames_between(beats, start, end):
    """Frames a client drew between CLOCK_MONOTONIC `start` and `end`
    from its heartbeats ({"t", "frames"}, ten a second): the counter at
    the last beat by `end` minus the one at the last beat by `start`.
    None when the beats do not reach both ends, so a missing window
    cannot read as a quiet one."""
    before = [b for b in beats if b["t"] <= start]
    by_end = [b for b in beats if b["t"] <= end]
    if not before or not by_end or by_end[-1]["t"] < end - 0.3:
        return None
    return by_end[-1]["frames"] - before[-1]["frames"]


def screenshot_layers(state):
    return sorted(layer["rect"] for layer in state.get("layers", [])
                  if layer.get("namespace") == SCREENSHOT_NAMESPACE)


def check_screenshot_layers(state, monitors):
    """GNOME's screenshot UI covers every monitor with its own bin and
    screen selector: one screenshot layer over each of `monitors`
    ({name: [x, y, w, h]}, logical) and no other."""
    have = screenshot_layers(state)
    want = sorted(monitors.values())
    if have != want:
        bare = [name for name, rect in monitors.items() if rect not in have]
        return [f"screenshot UI layers at {have}, not one per monitor {want}"
                + (f"; none over {', '.join(bare)}" if bare else "")]
    return []


def check_no_screenshot_layers(state):
    have = screenshot_layers(state)
    return [f"screenshot UI layers still mapped at {have}"] if have else []


def color_share(pixels, width, height, rowstride, channels, rgb, tolerance=8, step=4):
    """The share of a picture's pixels (sampled every `step` in both
    directions) within `tolerance` of `rgb` in every channel."""
    hits = total = 0
    r, g, b = rgb
    for y in range(0, height, step):
        row = y * rowstride
        for x in range(0, width, step):
            i = row + x * channels
            total += 1
            if (abs(pixels[i] - r) <= tolerance and abs(pixels[i + 1] - g) <= tolerance
                    and abs(pixels[i + 2] - b) <= tolerance):
                hits += 1
    return hits / total if total else 0.0


def check_capture(label, size, marker_share, expected_size, marker_shown):
    """A Screen-mode capture holds the selected output alone, in its own
    pixels: exactly `expected_size` (its physical mode), with the marker
    client covering at least MARKER_MIN_SHARE when that output shows it
    and at most MARKER_MAX_SHARE when it does not."""
    problems = []
    if tuple(size) != tuple(expected_size):
        problems.append(f"{label}: saved {size[0]}x{size[1]}, not the selected output's "
                        f"{expected_size[0]}x{expected_size[1]}")
    if marker_shown and marker_share < MARKER_MIN_SHARE:
        problems.append(f"{label}: the marker client covers {marker_share:.1%}, under {MARKER_MIN_SHARE:.0%}: "
                        "not the output showing it")
    if not marker_shown and marker_share > MARKER_MAX_SHARE:
        problems.append(f"{label}: the marker client covers {marker_share:.1%}, over {MARKER_MAX_SHARE:.1%}: "
                        "the other output's pixels")
    return problems
