"""Normal-desktop principal operations for five GNOME media/device consumers.

No app is launched through a command line, MPRIS, GSettings or a private bus.
Prepared payloads and emulated backend settings are controls, not app receipts.
"""
from __future__ import annotations

import base64


TITLE = "Tuna Media Control"
PALETTE = ((255, 0, 0), (0, 255, 0), (0, 0, 255),
           (255, 255, 0), (255, 0, 255), (0, 255, 255))


def _require(condition, message):
    if not condition:
        raise RuntimeError(message)


def _nodes(tree):
    return [n for n in tree["nodes"] if n["showing"]]


def _contains(tree, text):
    return any(text in str(n.get("name", "")) or text in str(n.get("text", ""))
               for n in _nodes(tree))


def _wait_text(d, actor, label, text, timeout=10):
    return d.await_state(label, lambda: d.tree(actor), lambda t: _contains(t, text), timeout=timeout)


def _button(d, actor, names):
    if isinstance(names, str):
        names = (names,)
    tree = d.tree(actor)
    candidates = [n for n in _nodes(tree) if n["name"] in names and n["sensitive"]
                  and n["role"] in ("push button", "toggle button", "button", "menu button",
                                    "menu item", "radio menu item", "check menu item")]
    _require(len(candidates) == 1, "no unique real native action: " + repr(names))
    node = candidates[0]
    d.press(actor, node["name"], role=node["role"])
    return node


def _entry(d, actor):
    entries = [n for n in _nodes(d.tree(actor)) if n["editable"]
               and n["sensitive"] and n["focused"] and n.get("text") is not None]
    _require(len(entries) == 1, "no unique actual focused editable media control")
    return entries[0]


def _write_entry(d, actor, value):
    node = _entry(d, actor)
    d.click_path(actor, node["path"])
    d.keys("ctrl", "a")
    d.keys("backspace")
    if value:
        d.type_ascii(value)
    after = _entry(d, actor)
    _require(after["text"] == value, "native media text input did not reach original control")
    return after


def _monitor(d, actor, label, playing):
    result = d.guest("media", "monitor", app=actor["id"], pid=actor["pid"],
                     start_ticks=actor["start_ticks"], label=label)
    _require(result["matches_tones"] if playing else result["silence"],
             "native decoded output failed independent monitor payload: " + repr(result))
    if playing:
        _require(len(result["active_sink_inputs"]) == 1,
                 "principal playback has no unique PID-bound output stream")
    return result


def _no_editable_interface(d, actor, explanation):
    tree = d.tree(actor)
    editable = [n for n in tree["nodes"] if n["editable"] and n["sensitive"]]
    _require(not editable, "an actual editable interface exists; IME cannot be waived")
    source = {
        "org.gnome.Decibels": "https://github.com/GNOME/decibels/blob/main/src/window.ts",
        "org.gnome.Showtime": "https://github.com/GNOME/showtime/blob/main/showtime/gtk/window.blp",
        "org.gnome.Snapshot": "https://github.com/GNOME/snapshot/blob/main/data/resources/ui/camera_controls.ui",
    }[actor["id"]]
    return {"scope": "original-app", "app_id": actor["id"], "consumer_actor": actor,
            "app_interface": "not-applicable", "reason": explanation, "source": source, "live_tree": tree,
            "live_showing_roles": sorted(set(n["role"] for n in _nodes(tree))),
            "file_chooser_is_separate_consumer": True}


def _chooser_ime(d, actor, source, label):
    d.keys("ctrl", "o")
    picker = d.file_picker(actor)
    d.keys("ctrl", "l")
    field = _entry(d, picker)
    ime = d.ime(picker, field["name"], field_path=field["path"])
    d.keys("ctrl", "a")
    d.keys("ctrl", "c")
    copied = d.clipboard()
    _require(base64.b64decode(copied["base64"]).decode("utf-8") == "你好",
             "native chooser did not consume the actual IME committed text")
    _write_entry(d, picker, source)
    _button(d, picker, "Close")
    d.select(actor)
    return {"scope": "portal-only", "consumer_actor": picker, "original_app_input_qualified": False,
            "ime": ime, "independent_clipboard": copied,
            "cancel_preserved_playback": _monitor(d, actor, label + "-after-cancel", True)}


def _default_consumer(d, source, label, app_id, search_name):
    admitted = d.start(app_id, search_name)
    opened = d.open_default(source)
    target = opened["actor"]
    _require(opened["default_handler"].removesuffix(".desktop") == target["id"],
             "Files Enter did not reach the actual installed MIME default handler")
    _require(target["id"] in ("org.gnome.Decibels", "org.gnome.Showtime", "org.gnome.Music"),
             "actual MIME default has no media-domain payload validator: " + target["id"])
    if target["id"] == "org.gnome.Showtime":
        d.keys("tab")
    _wait_text(d, target, label + "-transport", "Pause", timeout=30)
    payload = (_video(d, target, label) if source.endswith(".webm")
               else _monitor(d, target, label + "-monitor", True))
    _button(d, target, "Pause")
    stopped = _monitor(d, target, label + "-closed-stream-silence", False)
    d.close(target)
    if (admitted["pid"], admitted["window_id"]) != (target["pid"], target["window_id"]):
        d.close(admitted)
    return {"native_files_default": opened, "actual_default_payload": payload,
            "native_transport_stopped_before_retirement": stopped}


def _frame_image(frame_path, bounds):
    from PIL import Image
    with Image.open(frame_path) as source:
        image = source.convert("RGB")
    if bounds is not None:
        _require(len(bounds) == 4 and all(type(v) is int for v in bounds), "invalid actual frame bounds")
        x, y, width, height = bounds
        _require(0 <= x < x+width <= image.width and 0 <= y < y+height <= image.height,
                 "actual actor bounds escape the captured frame")
        image = image.crop((x, y, x+width, y+height))
    return image


def video_pixels(frame_path, bounds=None):
    """Locate ordered codec bars and a filled payload-sized white component."""
    image = _frame_image(frame_path, bounds)
    def color(pixel):
        bits = tuple(255 if channel > 180 else 0 if channel < 75 else -1 for channel in pixel)
        return PALETTE.index(bits) if bits in PALETTE else -1
    candidates = []
    for y in range(10, image.height - 10, 8):
        runs = []
        start, previous = 0, color(image.getpixel((0, y)))
        for x in range(1, image.width):
            current = color(image.getpixel((x, y)))
            if current != previous:
                if x - start >= 16 and previous >= 0:
                    runs.append((previous, start, x))
                start, previous = x, current
        if image.width - start >= 16 and previous >= 0:
            runs.append((previous, start, image.width))
        for index in range(len(runs) - 5):
            group = runs[index:index+6]
            widths = [right-left for _,left,right in group]
            if ([r[0] for r in group] == list(range(6)) and max(widths) / min(widths) < 1.15
                    and all(group[i+1][1] - group[i][2] <= 8 for i in range(5))):
                candidates.append((group[0][1], y, group[-1][2], sum(widths)))
    _require(len(candidates) >= 5, "native video frame lacks rendered ordered codec color payload")
    left, _, right, _ = max(candidates, key=lambda c: c[3])
    same = [c for c in candidates if abs(c[0]-left) < 5 and abs(c[2]-right) < 5]
    top, bottom = min(c[1] for c in same), max(c[1] for c in same)
    _require(bottom - top > 80, "rendered color payload has insufficient actual area")
    white = {(x, y) for y in range(top + 3, bottom - 2, 3)
             for x in range(left + 3, right - 3, 3)
             if min(image.getpixel((x, y))) > 235}
    expected_size = (right-left) * 48 / 648
    squares = []
    while white:
        seed = white.pop()
        component, pending = [seed], [seed]
        while pending:
            x, y = pending.pop()
            for adjacent in ((x-3,y), (x+3,y), (x,y-3), (x,y+3)):
                if adjacent in white:
                    white.remove(adjacent)
                    component.append(adjacent)
                    pending.append(adjacent)
        xs, ys = [p[0] for p in component], [p[1] for p in component]
        width, height = max(xs)-min(xs)+3, max(ys)-min(ys)+3
        if (expected_size*.75 <= width <= expected_size*1.25 and
                expected_size*.75 <= height <= expected_size*1.25 and
                len(component)*9/(width*height) > .85):
            squares.append(component)
    _require(len(squares) == 1, "no unique rendered filled moving square of the encoded dimensions")
    square = squares[0]
    return {"frame_path": str(frame_path), "frame_bounds": bounds,
            "bars_bounds": [left, top, right, bottom],
            "square_center": [sum(p[0] for p in square)/len(square),
                              sum(p[1] for p in square)/len(square)], "white_samples": len(square)}


def vivid_gallery_pixels(frame_path, bounds=None):
    """Find the saved Vivid image in the real gallery, not a generic image label."""
    image = _frame_image(frame_path, bounds)
    palette = ((255,255,0), (0,255,255), (0,255,0), (255,0,255), (255,0,0), (0,0,255))
    rows = []
    for y in range(8, image.height - 8, 8):
        runs = []
        start, previous = 0, None
        for x in range(image.width):
            pixel = image.getpixel((x, y))
            bits = tuple(255 if c > 180 else 0 if c < 75 else -1 for c in pixel)
            current = palette.index(bits) if bits in palette else None
            if current != previous:
                if previous is not None and x - start >= 20:
                    runs.append((previous, start, x))
                start, previous = x, current
        for index in range(len(runs)-5):
            group = runs[index:index+6]
            widths = [r-l for _,l,r in group]
            if ([n for n,_,_ in group] in (list(range(6)), list(reversed(range(6))))
                    and sum(widths) >= 240 and max(widths)/min(widths) < 1.2
                    and all(group[i+1][1] - group[i][2] < 8 for i in range(5))):
                rows.append((y, group[0][1], group[-1][2]))
    return {"matches": len(rows) >= 9 and max((r[0] for r in rows), default=0)
            - min((r[0] for r in rows), default=0) >= 64,
            "ordered_payload_rows": rows, "frame_path": str(frame_path)}


def _video(d, actor, label):
    d.keys("tab")  # Source-defined move-focus reveals real Showtime transport controls.
    first = d.observe(label + "-frame-a", actor)
    a = video_pixels(first["frame_path"], first["frame_bounds"])
    # The independent real monitor capture also gives the clock time for movement;
    # there is no source decode masquerading as a rendered application frame.
    sound = _monitor(d, actor, label + "-sound", True)
    second = d.observe(label + "-frame-b", actor)
    _require(first["frame_bounds"] == second["frame_bounds"],
             "window movement/resize cannot substitute for decoded video motion")
    b = video_pixels(second["frame_path"], second["frame_bounds"])
    _require(abs(a["square_center"][0] - b["square_center"][0]) > 10,
             "video color payload is frozen, not actually playing")
    return {"frames": [a, b], "monitor": sound}


def _playback_probe(d, actor, label, video=False):
    if video:
        d.keys("tab")
    _button(d, actor, "Pause")
    paused = _monitor(d, actor, label + "-paused", False)
    _button(d, actor, "Play")
    playing = _video(d, actor, label) if video else _monitor(d, actor, label + "-playing", True)
    return {"pause": paused, "play": playing, "observation": d.observe(label, actor)}


def _codec_negative(d, actor, path, title, label):
    d.open_file(actor, path, cancel_first=True)
    ui = _wait_text(d, actor, label, title, timeout=30)
    audio = _monitor(d, actor, label + "-silent", False)
    return {"ui": ui, "monitor": audio, "source": path, "status": "failed-source-observed"}


def _audio_service_cycle(d, actor, source, label, search):
    before = _monitor(d, actor, label + "-before", True)
    stopped = d.guest("media", "audio-service", mode="stop")
    try:
        title = ("File Cannot Be Played" if actor["id"] == "org.gnome.Decibels"
                 else "Unable to Play Video")
        failed = _wait_text(d, actor, label + "-service-unavailable", title, timeout=30)
        disconnected = d.guest("media", "audio-service-observe", app=actor["id"],
                               pid=actor["pid"], start_ticks=actor["start_ticks"])
    finally:
        restored = d.guest("media", "audio-service", mode="restore")
    replacement = d.restart(actor, search)
    d.open_file(replacement, source, cancel_first=True)
    _wait_text(d, replacement, label + "-reopened", "Pause", timeout=30)
    consumer = _monitor(d, replacement, label + "-recovered", True)
    return replacement, {"baseline": before, "service_loss": stopped, "app_during_loss": failed,
                         "actual_disconnected_output": disconnected, "service_restore": restored,
                         "native_recovered_output": consumer}


def _decibels(d, fixtures):
    default = _default_consumer(d, fixtures["audio"], "audio-default", "org.gnome.Decibels", "Audio Player")
    actor = d.start("org.gnome.Decibels", "Audio Player")
    d.open_file(actor, fixtures["audio"], cancel_first=True)
    _wait_text(d, actor, "decibels-title", TITLE, timeout=30)
    evidence = {"principal": _playback_probe(d, actor, "decibels-principal"),
                "default_handler": default, "prepared_control": fixtures["audio_control"]}
    evidence["corrupt_source"] = _codec_negative(d, actor, d.fixture + "/media/broken.ogg",
                                                 "File Cannot Be Played", "decibels-corrupt")
    d.open_file(actor, fixtures["audio"], cancel_first=True)
    _wait_text(d, actor, "decibels-playing-again", "Pause", timeout=30)
    actor, evidence["service"] = _audio_service_cycle(d, actor, fixtures["audio"], "decibels-pulse", "Audio Player")
    evidence["ime"] = _no_editable_interface(d, actor,
        "Decibels window/player source and live native player tree expose no editable text; "
        "the Open action uses a separately owned native file chooser, not an app field.")
    evidence["portal_filename_ime"] = _chooser_ime(d, actor, fixtures["audio"], "decibels-chooser-ime")
    latest = [actor]
    def probe(current, phase):
        return _playback_probe(d, current, "decibels-" + phase)
    def restarted(current, phase):
        latest[0] = current
        reset = _wait_text(d, current, "decibels-empty-restart", "Open", timeout=10)
        _require(not _contains(reset, TITLE), "Decibels unexpectedly retained a frontend stream")
        # Reopen the persisted actual codec file through the original native
        # chooser; no stream/preference persistence is invented for Decibels.
        d.open_file(current, fixtures["audio"], cancel_first=True)
        _wait_text(d, current, "decibels-restart-title", TITLE, timeout=30)
        return {"expected_empty_reset": reset, "persisted_file_native_reopen": probe(current, phase)}
    evidence["crosscut"] = d.crosscut(actor, probe, restart_probe=restarted)
    _button(d, latest[0], "Pause")
    evidence["retirement_silence"] = _monitor(d, latest[0], "decibels-final-silence", False)
    return d.result(latest[0], "native-vorbis-decode-and-monitor", evidence,
                    ime_applicability=evidence["ime"], installed_signed_release_qualified=False)


def _music_search(d, actor, query):
    fields = [n for n in _nodes(d.tree(actor)) if n["editable"] and n["sensitive"]]
    if not fields:
        d.keys("ctrl", "f")
        node = _entry(d, actor)
    else:
        _require(len(fields) == 1, "ambiguous Music search input")
        d.click_path(actor, fields[0]["path"])
        node = _entry(d, actor)
    _write_entry(d, actor, query)
    return node


def _song_play(d, actor, label):
    _music_search(d, actor, TITLE)
    tree = _wait_text(d, actor, label + "-indexed-song", TITLE, timeout=30)
    labels = [n for n in _nodes(tree) if n["name"] == TITLE and n["role"] == "label"
              and any(a["role"] in ("list item", "list row", "table row") for a in n["ancestors"])]
    _require(len(labels) == 1, "indexed song does not have a unique actual library row")
    d.click_path(actor, labels[0]["path"])
    _wait_text(d, actor, label + "-started", "Pause", timeout=30)
    return _monitor(d, actor, label + "-monitor", True)


def _music(d, fixtures):
    actor = d.start("org.gnome.Music", "Music")
    indexed = d.await_state("music-real-index", lambda: d.guest("media", "library"),
                            lambda r: r["matches_metadata"], timeout=30)
    evidence = {"index": indexed, "principal": _song_play(d, actor, "music-principal"),
                "prepared_control": fixtures["audio_control"]}
    _button(d, actor, "Star")
    _wait_text(d, actor, "music-favorite", "Unstar")
    field = _music_search(d, actor, "")
    evidence["ime"] = {"scope": "original-app", "consumer_actor": actor,
                       "composition": d.ime(actor, field["name"], field_path=field["path"])}
    d.keys("ctrl", "a")
    d.keys("ctrl", "c")
    copied = d.clipboard()
    _require(base64.b64decode(copied["base64"]).decode("utf-8") == "你好",
             "Music's original search field did not consume the IME commit")
    evidence["ime"]["clipboard_consumer"] = copied
    evidence["ime"]["native_search_result"] = _wait_text(
        d, actor, "music-ime-search-consumer", "No Results Found", timeout=30)
    _write_entry(d, actor, "Tuna definitely absent " + d.nonce)
    evidence["negative_search"] = _wait_text(d, actor, "music-absent-search", "No Results Found", timeout=30)
    _song_play(d, actor, "music-restored-query")
    stopped = d.guest("media", "index-service", mode="stop")
    try:
        actor = d.restart(actor, "Music")
        failed = _wait_text(d, actor, "music-index-service-loss", "could not connect to Tracker", timeout=30)
        silent = _monitor(d, actor, "music-no-index-output", False)
    finally:
        restored = d.guest("media", "index-service", mode="restore")
    actor = d.restart(actor, "Music")
    recovered = _song_play(d, actor, "music-service-recovered")
    evidence["service"] = {"stop": stopped, "native_unavailable": failed, "silent": silent,
                           "restore": restored, "real_reindexed_consumer": recovered}
    latest = [actor]
    def probe(current, phase):
        _music_search(d, current, TITLE)
        _wait_text(d, current, "music-favorite-" + phase, "Unstar", timeout=30)
        return {"favorite_and_metadata": d.observe("music-content-" + phase, current),
                "transport": _playback_probe(d, current, "music-" + phase),
                "index": d.guest("media", "library")}
    def restarted(current, phase):
        latest[0] = current
        consumer = _song_play(d, current, "music-" + phase + "-reopen")
        return {"native_persistent_favorite": probe(current, phase), "consumer": consumer}
    evidence["crosscut"] = d.crosscut(actor, probe, restart_probe=restarted)
    _button(d, latest[0], "Pause")
    evidence["retirement_silence"] = _monitor(d, latest[0], "music-final-silence", False)
    return d.result(latest[0], "native-indexed-metadata-favorite-and-playback", evidence,
                    installed_signed_release_qualified=False)


def _showtime(d, fixtures):
    default = _default_consumer(d, fixtures["video"], "video-default", "org.gnome.Showtime", "Video Player")
    actor = d.start("org.gnome.Showtime", "Video Player")
    d.open_file(actor, fixtures["video"], cancel_first=True)
    _wait_text(d, actor, "showtime-decoded", "Pause", timeout=30)
    evidence = {"principal": _video(d, actor, "showtime-principal"), "default_handler": default,
                "prepared_control": fixtures["video_control"]}
    evidence["corrupt_source"] = _codec_negative(d, actor, d.fixture + "/media/broken.webm",
                                                 "Unable to Play Video", "showtime-corrupt")
    d.open_file(actor, fixtures["video"], cancel_first=True)
    _wait_text(d, actor, "showtime-resumed", "Pause", timeout=30)
    actor, evidence["service"] = _audio_service_cycle(d, actor, fixtures["video"], "showtime-pulse", "Video Player")
    evidence["ime"] = _no_editable_interface(d, actor,
        "Showtime's native window source and live tree expose video/transport/options without "
        "an editable app text field; its Gtk.FileDialog is a separate native consumer.")
    evidence["portal_filename_ime"] = _chooser_ime(d, actor, fixtures["video"], "showtime-chooser-ime")
    latest = [actor]
    expected_resume = [None]
    def position(current):
        return d.guest("media", "showtime-position", pid=current["pid"], start_ticks=current["start_ticks"])
    def probe(current, phase):
        observed = _playback_probe(d, current, "showtime-" + phase, video=True)
        if phase == "after-unlock":
            # The real app only offers saved-position restore after sixty seconds.
            # Each intentional 10s seek has its own independent owner-bound readback.
            for step in range(7):
                previous = position(current)["position_us"]
                if previous >= 70_000_000:
                    break
                d.keys("tab")
                _button(d, current, "Go Forward 10 Seconds")
                d.await_state("showtime-native-seek-" + str(step), lambda: position(current),
                              lambda r: r["position_us"] >= previous + 9_000_000, timeout=10)
            _button(d, current, "Pause")
            expected_resume[0] = position(current)["position_us"]
            _require(expected_resume[0] >= 60_000_000, "native seek did not reach resumable position")
            observed["paused_resume_checkpoint"] = expected_resume[0]
            observed["checkpoint_monitor"] = _monitor(d, current, "showtime-checkpoint-silent", False)
        return observed
    def restarted(current, phase):
        latest[0] = current
        reset = _wait_text(d, current, "showtime-empty-restart", "Watch Videos")
        d.open_file(current, fixtures["video"], cancel_first=True)
        _wait_text(d, current, "showtime-native-resume-choice", "Resume", timeout=30)
        history = d.guest("media", "showtime-history", pid=current["pid"], start_ticks=current["start_ticks"])
        _require(expected_resume[0] is not None and
                 abs(history["saved_position_ns"] / 1000 - expected_resume[0]) < 250_000 and
                 abs(history["position_us"] - expected_resume[0]) < 250_000,
                 "native reopened consumer did not restore the exact saved playback checkpoint")
        _button(d, current, "Resume")
        _wait_text(d, current, "showtime-restart-playing", "Pause", timeout=30)
        return {"reset": reset, "native_persisted_history_and_restored_seek": history,
                "persisted_codec_consumer": probe(current, phase)}
    evidence["crosscut"] = d.crosscut(actor, probe, restart_probe=restarted)
    d.keys("tab")
    _button(d, latest[0], "Pause")
    evidence["retirement_silence"] = _monitor(d, latest[0], "showtime-final-silence", False)
    return d.result(latest[0], "native-vp8-vorbis-render-motion-and-monitor", evidence,
                    ime_applicability=evidence["ime"], installed_signed_release_qualified=False)


def _camera_inventory(d):
    return d.guest("media", "camera-files", mode="inventory")["files"]


def _camera_shot(d, actor, phase, pattern="bars"):
    before = _camera_inventory(d)
    _button(d, actor, "Take Picture")
    d.await_state("camera-new-file-" + phase, _camera_inventory,
                  lambda files: len(set(files) - set(before)) == 1, timeout=30)
    d.await_state("camera-shutter-completed-" + phase, lambda: d.tree(actor),
                  lambda t: any(n["name"] == "Take Picture" and n["sensitive"] for n in _nodes(t)),
                  timeout=30)
    measured = d.guest("media", "camera-files", mode=pattern, after=before)
    return {"native_shutter_frame": d.observe("camera-" + phase, actor), "actual_photo": measured}


def _camera_denied(d, actor, label):
    # A successful live Vivid/PipeWire baseline is established before revoke;
    # the initial Deny is additionally paired with a later actual grant baseline.
    tree = _wait_text(d, actor, label, "Missing Camera Permission", timeout=30)
    shutter = [n for n in _nodes(tree) if n["name"] == "Take Picture" and n["sensitive"]]
    _require(not shutter, "camera denial left a usable native shutter")
    return {"frame": d.observe(label, actor), "photos": _camera_inventory(d)}


def _camera(d):
    device = d.guest("media", "camera-device", mode="prepare")
    d.guest("media", "camera-pattern", mode="bars")
    d.guest("media", "camera-permission", action="delete")
    actor = d.start("org.gnome.Snapshot", "Camera")
    initial_files = _camera_inventory(d)
    denied_reply = d.portal(actor, interface="Camera", response="deny")
    denied = _camera_denied(d, actor, "camera-portal-denied")
    _require(_camera_inventory(d) == initial_files, "denied camera request produced a photo")
    d.guest("media", "camera-permission", action="delete")
    actor = d.restart(actor, "Camera")
    granted_reply = d.portal(actor, interface="Camera", response="grant")
    _wait_text(d, actor, "camera-granted-shutter", "Take Picture", timeout=30)
    evidence = {"device": device, "portal_deny": {"reply": denied_reply, "native": denied},
                "portal_grant": granted_reply, "principal": _camera_shot(d, actor, "principal")}
    d.guest("media", "camera-pattern", mode="white")
    evidence["independent_pattern_control"] = _camera_shot(d, actor, "white-control", pattern="white")
    d.guest("media", "camera-pattern", mode="bars")
    revoked = d.guest("media", "camera-permission", action="set", value="no")
    actor = d.restart(actor, "Camera")
    before = _camera_inventory(d)
    native_revoked = _camera_denied(d, actor, "camera-revoked")
    _require(_camera_inventory(d) == before, "revoked camera permission produced a photo")
    evidence["revoke"] = {"permission_store": revoked, "native": native_revoked}
    d.guest("media", "camera-permission", action="delete")
    actor = d.restart(actor, "Camera")
    evidence["regrant"] = d.portal(actor, interface="Camera", response="grant")
    _wait_text(d, actor, "camera-regrant-shutter", "Take Picture", timeout=30)
    evidence["regrant_consumer"] = _camera_shot(d, actor, "regrant")
    # Release the live app's PipeWire fd before owned module removal; the next
    # real frontend observes absence.  No pre-existing/physical module is touched.
    d.close(actor)
    before_device_loss = _camera_inventory(d)
    missing = d.guest("media", "camera-device", mode="remove")
    try:
        actor = d.start("org.gnome.Snapshot", "Camera")
        ui = _wait_text(d, actor, "camera-device-lost", "No Camera Found", timeout=30)
        _require(_camera_inventory(d) == before_device_loss,
                 "absent camera device produced an unexpected native image")
        d.close(actor)
    finally:
        restored = d.guest("media", "camera-device", mode="restore")
        d.guest("media", "camera-pattern", mode="bars")
    actor = d.start("org.gnome.Snapshot", "Camera")
    _wait_text(d, actor, "camera-device-recovered", "Take Picture", timeout=30)
    evidence["device_loss"] = {"removed": missing, "native": ui, "restored": restored,
                               "consumer": _camera_shot(d, actor, "device-recovered")}
    service_baseline = _camera_shot(d, actor, "pipewire-baseline")
    before_service_loss = _camera_inventory(d)
    stopped = d.guest("media", "camera-service", mode="stop")
    try:
        no_stream = d.await_state("camera-real-pipewire-loss", lambda: d.tree(actor),
            lambda t: not any(n["name"] == "Take Picture" and n["sensitive"] for n in _nodes(t)),
            timeout=30)
        no_output = _camera_inventory(d)
        _require(no_output == before_service_loss, "unavailable PipeWire produced unexpected camera output")
    finally:
        service_restore = d.guest("media", "camera-service", mode="restore")
    actor = d.restart(actor, "Camera")
    _wait_text(d, actor, "camera-pipewire-restored", "Take Picture", timeout=30)
    evidence["service_loss"] = {"baseline": service_baseline, "stop": stopped,
        "native_no_stream": no_stream, "output_inventory": no_output,
        "restore": service_restore, "native_consumer": _camera_shot(d, actor, "pipewire-restored")}
    evidence["ime"] = _no_editable_interface(
        d, actor, "Snapshot camera interface has picture/video/QR mode, shutter, camera choice "
        "and gallery; no user-editable caption/name/search exists in the verified live camera tree.")
    latest = [actor]
    def probe(current, phase):
        return _camera_shot(d, current, phase)
    def restarted(current, phase):
        latest[0] = current
        _wait_text(d, current, "camera-restart-shutter", "Take Picture", timeout=30)
        # Gallery is the native persisted-image consumer, not a generated source.
        _button(d, current, "Open Gallery")
        gallery = d.await_state("camera-persistent-gallery",
            lambda: d.observe("camera-persistent-gallery-frame", current),
            lambda observation: vivid_gallery_pixels(
                observation["frame_path"], observation["frame_bounds"])["matches"], timeout=30)
        persisted_pixels = vivid_gallery_pixels(gallery["frame_path"], gallery["frame_bounds"])
        d.keys("esc")
        _wait_text(d, current, "camera-back-from-gallery", "Take Picture")
        return {"gallery": gallery, "actual_persisted_pixels": persisted_pixels,
                "new_native_capture": probe(current, phase)}
    evidence["crosscut"] = d.crosscut(actor, probe, restart_probe=restarted)
    return d.result(latest[0], "native-portal-vivid-capture-pattern-controls", evidence,
                    ime_applicability=evidence["ime"], physical_camera_qualified=False,
                    installed_signed_release_qualified=False)


def _focus_preference(d, actor, predicate, label):
    """Navigate the real dialog, including rows below its scroll viewport."""
    for _ in range(48):
        tree = d.tree(actor)
        focused = [node for node in tree["nodes"] if node["focused"] and node["sensitive"]
                   and predicate(node)]
        if len(focused) == 1:
            return focused[0]
        _require(len(focused) == 0, "ambiguous focused scanner preference: " + label)
        d.keys("tab")
    raise RuntimeError("native scanner preference not reached: " + label)


def _scanner_ime(d, actor):
    # PreferencesDialog has real GtkEntry controls. A portal-only filename
    # composition cannot waive them merely because the main scan canvas has none.
    original = d.guest("media", "scanner-preferences")
    _require(original["postproc-enabled"] is False,
             "refusing to execute a pre-existing postprocessing configuration")
    _require(original["postproc-arguments"].isascii(),
             "original arguments cannot be restored with the ordinary ASCII fixture control")
    d.keys("ctrl", "comma")
    def under(node, title):
        return node["name"] == title or any(title in parent["name"] for parent in node["ancestors"])
    _focus_preference(d, actor, lambda n: under(n, "Image Resolution"), "Image Resolution")
    d.keys("ret")
    d.keys("home")
    d.keys("ret")
    d.await_state("scanner-native-75-dpi-selection",
        lambda: d.guest("media", "scanner-preferences"), lambda value: value["photo-dpi"] == 75)
    _focus_preference(d, actor, lambda n: under(n, "Page Size"), "Page Size")
    d.keys("ret")
    d.keys("home")
    d.keys("ret")
    d.await_state("scanner-native-automatic-page-selection",
        lambda: d.guest("media", "scanner-preferences"),
        lambda value: value["paper-width"] == value["paper-height"] == 0)
    enabled = _focus_preference(d, actor,
        lambda n: n["role"] == "switch" and under(n, "Enable Postprocessing"),
        "Enable Postprocessing")
    _require(not enabled["checked"], "scanner postprocessing UI differs from its actual settings")
    d.keys("spc")
    try:
        entry = _focus_preference(d, actor,
            lambda n: n["editable"] and under(n, "Script arguments"), "Script arguments")
        proof = d.ime(actor, entry["name"], field_path=entry["path"])
        consumed = d.await_state("scanner-original-preference-ime-consumer",
            lambda: d.guest("media", "scanner-preferences"),
            lambda value: value["postproc-arguments"] == "你好")
    finally:
        _focus_preference(d, actor,
            lambda n: n["editable"] and under(n, "Script arguments"), "Script arguments")
        d.keys("ctrl", "a")
        d.keys("backspace")
        if original["postproc-arguments"]:
            d.type_ascii(original["postproc-arguments"])
        d.await_state("scanner-original-arguments-restored",
            lambda: d.guest("media", "scanner-preferences"),
            lambda value: value["postproc-arguments"] == original["postproc-arguments"])
        switch = _focus_preference(d, actor,
            lambda n: n["role"] == "switch" and under(n, "Enable Postprocessing"),
            "Enable Postprocessing")
        if switch["checked"]:
            d.keys("spc")
        d.await_state("scanner-postprocessing-disabled-before-any-export",
            lambda: d.guest("media", "scanner-preferences"),
            lambda value: value["postproc-enabled"] is False)
        d.keys("esc")
    return {"original_app_composition": proof, "actual_settings_consumer": consumed,
            "original_preferences": original,
            "restored_before_scan": d.guest("media", "scanner-preferences")}


def _scanner_save(d, actor, relative, cancel_first=False, ime=False):
    target = d.fixture + "/media/" + relative
    chooser = d.save_file(actor, target, cancel_first=cancel_first, ime=ime)
    completed = _wait_text(d, actor, "scanner-native-save-completed-" + relative,
                           "Document Saved", timeout=30)
    payload = d.guest("media", "scan-export", relative=relative)
    backend = d.guest("media", "scanner-backend", pid=actor["pid"], start_ticks=actor["start_ticks"])
    return {"chooser": chooser, "native_export": payload, "consumed_packaged_backend": backend,
            "original_app_save_completed": completed,
            "frame": d.observe("scanner-" + relative.replace(".", "-"), actor)}


def _scan(d, actor, phase):
    # Ctrl+1 is the source-defined single-page native scan action; no scanimage
    # process creates a principal output.  Exported pixels must be test backend RGB.
    d.keys("ctrl", "1")
    d.await_state("scanner-page-ready-" + phase, lambda: d.tree(actor),
                  lambda t: any(n["name"] == "Export" and n["sensitive"] for n in _nodes(t))
                  and not any(n["name"] == "Stop" and n["sensitive"] for n in _nodes(t)), timeout=30)
    return _scanner_save(d, actor, "scan-" + phase + ".png")


def _scanner(d):
    d.guest("media", "sane", mode="ready")
    actor = d.start("org.gnome.SimpleScan", "Document Scanner")
    _wait_text(d, actor, "scanner-real-backend", "Ready to Scan", timeout=30)
    evidence = {"ime": _scanner_ime(d, actor)}
    _button(d, actor, "Scan Options")
    _button(d, actor, "Text")
    d.keys("esc")
    d.await_state("scanner-real-text-preference-control",
        lambda: d.guest("media", "scanner-preferences"), lambda value: value["document-type"] == "text")
    _button(d, actor, "Scan Options")
    _button(d, actor, "Image")
    d.keys("esc")
    d.await_state("scanner-real-image-preference-write",
        lambda: d.guest("media", "scanner-preferences"), lambda value: value["document-type"] == "photo")
    evidence["principal"] = _scan(d, actor, "principal")
    evidence["pdf"] = _scanner_save(d, actor, "scan-principal.pdf", cancel_first=True, ime=True)
    evidence["portal_filename_ime"] = {"scope": "portal-only", "original_app_input_qualified": False,
                                      "native_picker": evidence["pdf"]["chooser"]}
    d.close(actor)
    missing = d.guest("media", "sane", mode="missing")
    try:
        actor = d.start("org.gnome.SimpleScan", "Document Scanner")
        absent = _wait_text(d, actor, "scanner-device-missing", "No Scanners Detected", timeout=30)
        d.close(actor)
        io_control = d.guest("media", "sane", mode="io-error")
        actor = d.start("org.gnome.SimpleScan", "Document Scanner")
        _wait_text(d, actor, "scanner-error-device-present", "Ready to Scan", timeout=30)
        d.keys("ctrl", "1")
        io_error = _wait_text(d, actor, "scanner-consumed-io-error", "Failed to scan", timeout=30)
        _button(d, actor, "Close")
        _require(not any(n["name"] == "Export" and n["sensitive"] for n in _nodes(d.tree(actor))),
                 "failed SANE read left an exportable principal page")
        d.close(actor)
    finally:
        restored = d.guest("media", "sane", mode="ready")
    actor = d.start("org.gnome.SimpleScan", "Document Scanner")
    _wait_text(d, actor, "scanner-service-restored", "Ready to Scan", timeout=30)
    evidence["negative_backend"] = {"missing": missing, "native_missing": absent,
                                    "read_error_control": io_control, "native_read_error": io_error,
                                    "restored": restored}
    latest = [actor]
    def probe(current, phase):
        # New document keeps a single exact raster in every native export.
        d.keys("ctrl", "n")
        return _scan(d, current, phase)
    def restarted(current, phase):
        latest[0] = current
        _wait_text(d, current, "scanner-persisted-ready", "Ready to Scan", timeout=30)
        _button(d, current, "Scan Options")
        tree = d.tree(current)
        image = [n for n in _nodes(tree) if n["name"] == "Image" and (n["checked"] or n["selected"])]
        _require(len(image) == 1, "scanner Image preference did not persist across frontend restart")
        d.keys("esc")
        return {"persisted_image_preference": tree, "real_scanned_consumer": probe(current, phase)}
    evidence["crosscut"] = d.crosscut(actor, probe, restart_probe=restarted)
    return d.result(latest[0], "native-sane-test-consumer-raster-and-pdf", evidence,
                    physical_scanner_qualified=False, installed_signed_release_qualified=False)


def run(desktop):
    """Execute all five complete native consumers; any unmet obligation fails full28."""
    d = desktop
    try:
        fixtures = d.guest("media", "prepare-inventory")
        fixtures["audio_control"] = d.guest("media", "prepare-audio")
        fixtures["video_control"] = d.guest("media", "prepare-video")
        return [_decibels(d, fixtures), _music(d, fixtures), _showtime(d, fixtures),
                _camera(d), _scanner(d)]
    except Exception as error:
        d.retain("media-original-consumer-failure", {"error": str(error)})
        # Failure cleanup is explicitly not a native close or persistence proof.
        # Release only the parent's exact admitted user processes before unloading
        # the wholly owned Vivid module. Uncertain transport retirement stays fatal.
        d.retain("media-failed-frontends-retired", d.retire_frontends())
        raise
    finally:
        d.guest("media", "cleanup")
