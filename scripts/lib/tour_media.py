"""Public native tour view semantics shared by capture and media admission."""
import re

# These predicates are calibrated for the native tour's fixed framebuffer.
WIDTH, HEIGHT = 1280, 800

def grey_share(px, rect, step=6):
    """Share of sampled pixels in rect (WIDTH x HEIGHT RGB bytes) that are
    the shell's dark neutral grey; the Marlin wallpaper's dark areas are
    saturated navy, and windows are light, so neither counts."""
    x0, y0, x1, y1 = rect
    total = grey = 0
    for y in range(y0, y1, step):
        for x in range(x0, x1, step):
            i = (y * WIDTH + x) * 3
            r, g, b = px[i], px[i + 1], px[i + 2]
            total += 1
            grey += max(r, g, b) - min(r, g, b) < 14 and 45 < r + g + b < 225
    return grey / total if total else 0.0


# Calibrated on Marlin VM frames (runs 37937493915, 38878636595 and
# 38887426688): the overview backdrop fills the bottom-left corner, where
# the desktop shows wallpaper; window previews keep the centre off-grey,
# unlike the search and app-grid views. The Alt+Tab popup is a dark grey
# island whose top and bottom padding span a full row over the windows.
CORNER, CENTRE = (0, 690, 170, 780), (340, 250, 940, 550)


def overview_windows_shown(px):
    return grey_share(px, CORNER) > 0.85 and grey_share(px, CENTRE) < 0.5


def switcher_shown(px):
    return (grey_share(px, CORNER) < 0.2 and grey_share(px, CENTRE) < 0.6
            and any(grey_share(px, (380, y, 900, y + 1), step=4) >= 0.9
                    for y in range(280, 520, 2)))


def desktop_shown(px):
    return grey_share(px, CORNER) < 0.2 and not switcher_shown(px)

TOUR_PUBLIC_CONTROLS = {
    "app-grid": {("frame", "App Grid")},
    "quick-settings": {("slider", "Volume"), ("push button", "Settings"), ("push button", "Lock Screen")},
    "calendar": {("push button", "Previous month"), ("push button", "Next month")},
    "lock": {("frame", "Lock Screen")},
}


def tour_public_view(card, observation):
    """Positive visible public controls, not a requested tag or private state."""
    if (type(observation) is not dict or observation.get("schema") != 1
            or observation.get("method") != "public-at-spi"):
        return False
    session, principal, atspi = (observation.get(key) for key in ("session", "principal", "atspi"))
    if not all(type(value) is dict for value in (session, principal, atspi)):
        return False
    if (any(type(principal.get(key)) is not int or principal[key] <= 0
            for key in ("pid", "uid", "start_ticks"))
            or principal.get("executable") != "/usr/bin/tuna-shell-gtk"
            or type(principal.get("display")) is not str or not principal["display"]
            or type(session.get("Id")) is not str or not session["Id"]
            or principal.get("session_id") != session["Id"]
            or session.get("User") != str(principal["uid"])
            or session.get("Type") != "wayland" or session.get("Class") != "user"
            or session.get("Service") != "gdm-password"
            or type(session.get("VTNr")) is not str or not session["VTNr"].isdigit()
            or int(session["VTNr"]) <= 0
            or type(atspi.get("owner")) is not str or re.fullmatch(r":\d+\.\d+", atspi["owner"]) is None
            or atspi.get("bus_pid") != principal["pid"] or atspi.get("bus_uid") != principal["uid"]):
        return False
    started, completed = (observation.get(key) for key in
                          ("started_monotonic_ns", "completed_monotonic_ns"))
    if type(started) is not int or type(completed) is not int or not 0 < started <= completed:
        return False
    controls = observation.get("controls")
    if type(controls) is not list:
        return False
    found = set()
    for node in controls:
        if type(node) is not dict or node.get("showing") is not True or node.get("visible") is not True:
            continue
        rect = node.get("extents")
        if (type(rect) is not list or len(rect) != 4 or any(type(value) is not int for value in rect)):
            continue
        x, y, width, height = rect
        if width > 0 and height > 0 and x < WIDTH and y < HEIGHT and x + width > 0 and y + height > 0:
            found.add((node.get("role"), node.get("label")))
    return card in TOUR_PUBLIC_CONTROLS and TOUR_PUBLIC_CONTROLS[card] <= found


def validate_still_confirmations(timing, files, original, root=None):
    """Require positive public view evidence bound to each exact native PNG."""
    def require(condition, message):
        if not condition:
            raise ValueError(message)
    def positive(value):
        return type(value) is int and value > 0
    predicates = {"overview-windows": overview_windows_shown, "alt-tab": switcher_shown}
    required = set(predicates) | set(TOUR_PUBLIC_CONTROLS)
    entries = {entry["source"].rsplit("/", 1)[-1][2:-4]: entry for entry in files}
    require(set(entries) == required, "native hero inventory differs from the six public views")
    frames = timing.get("frames", [])
    require(type(frames) is list and bool(frames), "native hero capture frames missing")
    for index, frame in enumerate(frames):
        require(type(frame) is dict and frame.get("index") == index
                and positive(frame.get("requested_monotonic_ns"))
                and positive(frame.get("completed_monotonic_ns"))
                and frame["requested_monotonic_ns"] <= frame["completed_monotonic_ns"],
                "native hero frame index/time identity malformed")
    confirmations = timing.get("still_confirmations", {})
    require(type(confirmations) is dict and required <= confirmations.keys(),
            "native hero lacks positive exact-capture public confirmation")
    for tag, entry in entries.items():
        observed = confirmations[tag]
        require(type(observed) is dict and observed.get("schema") == 1
                and observed.get("tag") == tag and observed.get("confirmed") is True,
                "native hero public view is unconfirmed: " + tag)
        index = observed.get("frame_index")
        require(type(index) is int and 0 <= index < len(frames)
                and timing.get("stills", {}).get(tag) == index
                and observed.get("png_sha256") == entry["sha256"]
                and frames[index].get("sha256") == entry["sha256"],
                "native hero confirmation is not its exact captured PNG: " + tag)
        begin, end = (observed.get(key) for key in
                      ("capture_requested_monotonic_ns", "capture_completed_monotonic_ns"))
        require(begin == frames[index]["requested_monotonic_ns"]
                and end == frames[index]["completed_monotonic_ns"],
                "native hero confirmation differs from actual capture times")
        evidence = observed.get("evidence", {})
        require(type(evidence) is dict and evidence.get("within_deadline") is True,
                "native hero public view exceeded its original capture deadline")
        if tag in predicates:
            require(observed.get("method") == "calibrated-framebuffer"
                    and evidence.get("predicate") == predicates[tag].__name__
                    and evidence.get("exact_png") is True,
                    "native hero lacks exact framebuffer confirmation: " + tag)
        else:
            require(observed.get("method") == "public-at-spi"
                    and evidence.get("same_original_session") is True,
                    "native hero lacks public AT-SPI confirmation: " + tag)
            before, after = evidence.get("before", {}), evidence.get("after", {})
            left, right = before.get("observation"), after.get("observation")
            require(tour_public_view(tag, left) and tour_public_view(tag, right),
                    "native hero public controls missing before/after capture: " + tag)
            require(all(left[key] == right[key] for key in ("session", "principal", "atspi")),
                    "native hero public session/principal changed during capture")
            bracket = [before.get("requested_monotonic_ns"), before.get("completed_monotonic_ns"),
                       begin, end, after.get("requested_monotonic_ns"), after.get("completed_monotonic_ns")]
            require(all(positive(value) for value in bracket) and bracket == sorted(bracket)
                    and bracket[-1] - bracket[0] <= 10_000_000_000,
                    "native hero public observation does not bracket the exact capture")
            session, principal = left["session"], left["principal"]
            require(type(original) is dict and original.get("session_uid") == principal["uid"]
                    and all(original.get("session", {}).get(key) == value for key, value in session.items()),
                    "native hero public view is not the originally admitted GDM session")
            matching = [process for process in original.get("processes", [])
                        if process.get("executable") == "tuna-shell-gtk"
                        and all(process.get(key) == principal[key] for key in ("pid", "uid", "start_ticks"))]
            require(len(matching) == 1, "native hero public shell is not the original source-session principal")
        if root is not None:
            import hashlib
            from pathlib import Path
            from PIL import Image
            path = Path(root) / entry["source"]
            require(hashlib.sha256(path.read_bytes()).hexdigest() == observed["png_sha256"],
                    "native hero pixels changed after public confirmation")
            with Image.open(path) as image:
                require(image.size == (WIDTH, HEIGHT), "native hero is not the actual fixed framebuffer")
                pixels = image.convert("RGB").tobytes()
            if tag in predicates:
                require(predicates[tag](pixels), "native hero exact PNG shows the wrong public view: " + tag)
