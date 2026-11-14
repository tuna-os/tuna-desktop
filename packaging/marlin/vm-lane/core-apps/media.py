"""Independent media readback and narrowly owned native backend controls.

Fixture generation is never an application result.  Product operations belong to
normal desktop actors in core_apps_media.  Camera and SANE evidence is explicitly
emulated, and cannot qualify physical hardware or a signed release.
"""
from __future__ import annotations

import array
import hashlib
import json
import math
import os
from pathlib import Path
import re
import shutil
import wave


APPS = {"org.gnome.Decibels", "org.gnome.Music", "org.gnome.Showtime",
        "org.gnome.Snapshot", "org.gnome.SimpleScan"}
SANE_DIR = Path("/home/tuna-test/tuna-core-apps/media-sane")
CAMERA = "/dev/video42"
INDEX_BUS = "org.freedesktop.Tracker3.Miner.Files"
SOURCES = {
    "decibels": "https://github.com/GNOME/decibels/blob/main/src/window.ts",
    "music": "https://github.com/GNOME/gnome-music/blob/master/gnomemusic/trackerwrapper.py",
    "showtime": "https://github.com/GNOME/showtime/blob/main/showtime/play.py",
    "snapshot": "https://github.com/GNOME/snapshot/blob/main/src/widgets/camera.rs",
    "scanner": "https://github.com/GNOME/simple-scan/blob/master/src/scanner.vala",
    "sane_pattern": "https://gitlab.com/sane-project/backends/-/blob/master/backend/test-picture.c",
}


class MediaUnavailable(RuntimeError):
    """Required real backend is absent; this is not a successful denial."""


def _require(condition, message):
    if not condition:
        raise RuntimeError(message)


def _path(c, relative):
    return c.path("media/" + relative)


_USER_FILE = r"""
import os, pathlib, stat, sys
operation, name = sys.argv[1:]
path = pathlib.Path(name)
if not path.is_absolute() or ".." in path.parts or os.geteuid() == 0:
    raise RuntimeError("ordinary original user and absolute consumer path required")
descriptor = os.open("/", os.O_RDONLY | os.O_DIRECTORY)
try:
    for part in path.parts[1:-1]:
        try:
            child = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
                            dir_fd=descriptor)
        except FileNotFoundError:
            if operation == "remove":
                sys.exit(0)
            raise
        os.close(descriptor)
        descriptor = child
    parent = os.fstat(descriptor)
    if parent.st_uid != os.geteuid():
        raise RuntimeError("consumer directory is not owned by the original user")
    if operation == "directory":
        try:
            os.mkdir(path.name, mode=0o700, dir_fd=descriptor)
        except FileExistsError:
            pass
        child = os.open(path.name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
                        dir_fd=descriptor)
        try:
            if os.fstat(child).st_uid != os.geteuid():
                raise RuntimeError("consumer directory ownership differs")
        finally:
            os.close(child)
    elif operation == "remove":
        try:
            os.unlink(path.name, dir_fd=descriptor)
        except FileNotFoundError:
            pass
    elif operation == "write":
        child = os.open(path.name, os.O_WRONLY | os.O_CREAT | os.O_NOFOLLOW | os.O_NONBLOCK,
                        0o600, dir_fd=descriptor)
        with os.fdopen(child, "wb") as output:
            metadata = os.fstat(output.fileno())
            if (not stat.S_ISREG(metadata.st_mode) or metadata.st_uid != os.geteuid()
                    or metadata.st_nlink != 1):
                raise RuntimeError("consumer file type/owner/link count differs")
            output.truncate(0)
            output.write(sys.stdin.buffer.read())
    else:
        raise ValueError("unknown consumer file operation")
finally:
    os.close(descriptor)
"""


def _user_file(c, operation, path, data=None):
    # Fixed SANE_DIR is intentionally outside c.path(): the native desktop
    # frontend inherits it. It is consumer input, never privileged authority.
    sane_path = path == SANE_DIR or path.parent == SANE_DIR
    _require(path.is_absolute() and ".." not in path.parts and
             (path.is_relative_to(c.fixture) or
              (sane_path and SANE_DIR.parent.resolve(strict=True) == c.root)),
             "unsafe media consumer path")
    if sane_path:
        # Context.home is canonical (e.g. a stock /home -> /var/home link).
        # Admit only that exact original home, then traverse its consumer
        # directories without following user-controlled symlinks.
        directory = c.root / SANE_DIR.name
        path = directory if path == SANE_DIR else directory / path.name
    return c.user(["python3", "-c", _USER_FILE, operation, str(path)], input=data)


def _owned(c, path, data):
    _user_file(c, "write", path, data.decode() if isinstance(data, bytes) else data)


def _state(c):
    state = c.read_control("media-state")
    _require(isinstance(state, dict) and type(state.get("schema")) is int and state["schema"] == 1,
             "invalid authoritative media state")
    for key in ("vivid_owned", "vivid_bound", "audio_stopped", "index_stopped",
                "camera_stopped", "sane_owned", "permission_owned", "restored"):
        _require(type(state.get(key)) is bool, "invalid media effect state: " + key)
    _require(all(key in state for key in ("music_dir", "pictures", "old_photos", "permission")),
             "incomplete authoritative media state")
    _require(isinstance(state["music_dir"], str) and
             Path(state["music_dir"]).is_relative_to(c.home) and
             Path(state["music_dir"]).name == "Tuna-core-" + c.nonce,
             "invalid original media directory")
    _require(isinstance(state["old_photos"], list) and
             all(isinstance(path, str) for path in state["old_photos"]) and
             isinstance(state["pictures"], str) and Path(state["pictures"]).is_relative_to(c.home),
             "invalid original photo inventory")
    if state["vivid_owned"]:
        module = state.get("vivid_module")
        _require(isinstance(module, dict) and module.get("package") == "linux" and
                 isinstance(module.get("path"), str) and
                 module["path"].startswith("/usr/lib/modules/" + os.uname().release + "/") and
                 re.fullmatch(r"[0-9a-f]{64}", module.get("sha256", "")) is not None,
                 "invalid original Vivid source")
        _require(state.get("vivid_original") == {"module_present": False, "device_present": False},
                 "invalid original Vivid ownership")
    if "vivid_platform" in state:
        _require(isinstance(state["vivid_platform"], str) and
                 re.fullmatch(r"vivid\.\d+", state["vivid_platform"]) is not None,
                 "invalid fixture-owned Vivid platform")
    _require(not state["vivid_bound"] or state["vivid_owned"], "unowned Vivid binding")
    for flag, key, units in (
            ("audio_stopped", "audio_before", ("pipewire-pulse.socket", "pipewire-pulse.service")),
            ("index_stopped", "index_before", ("localsearch-3.service",)),
            ("camera_stopped", "camera_units_before",
             ("pipewire.socket", "pipewire.service", "wireplumber.service"))):
        if state[flag]:
            rows = state.get(key)
            _require(isinstance(rows, list) and len(rows) == len(units) and
                     all(isinstance(row, dict) and row.get("Id") == unit and
                         row.get("ActiveState") in ("active", "inactive") and
                         isinstance(row.get("UnitFileState"), str)
                         for row, unit in zip(rows, units)), "invalid original media unit state")
    if state["index_stopped"]:
        _require(state.get("index_unit") == "localsearch-3.service", "invalid owned index unit")
    if state["camera_stopped"]:
        rows = state.get("camera_before")
        _require(isinstance(rows, list) and len(rows) == 2 and
                 all(isinstance(row, dict) and isinstance(row.get("state"), dict) and
                     isinstance(row["state"].get("MainPID"), str) and
                     isinstance(row.get("start_ticks"), str) for row in rows),
                 "invalid original camera processes")
    permission = state["permission"]
    if permission is not None:
        _require(isinstance(permission, dict) and type(permission.get("entry_exists")) is bool and
                 isinstance(permission.get("all_permissions"), dict) and "permissions" in permission and
                 (permission["permissions"] is None or
                  (isinstance(permission["permissions"], list) and
                   len(permission["permissions"]) == 1 and
                   permission["permissions"][0] in ("yes", "no", "ask"))),
                 "invalid original camera permission")
    _require(not state["permission_owned"] or permission is not None,
             "owned camera permission lacks its original receipt")
    _require(not state["restored"] or not any(state[key] for key in
             ("vivid_owned", "audio_stopped", "index_stopped", "camera_stopped",
              "sane_owned", "permission_owned")),
             "restored media state still has owned effects")
    return state


def _save(c, state):
    c.save_control("media-state", state)


def _user_dir(c, kind):
    value = c.user(["xdg-user-dir", kind], timeout=3).stdout.strip()
    result = Path(value)
    if result == c.home:
        raise MediaUnavailable("real XDG " + kind + " directory is disabled/unconfigured")
    _require(result.is_absolute() and result.is_relative_to(c.home) and
             not result.is_symlink(), "unsafe XDG " + kind)
    return result


def _dbus(c, destination, path, method, *args, check=True):
    result = c.user(["gdbus", "call", "--session", "--dest", destination,
                     "--object-path", path, "--method", method, *args], check=check)
    if result.returncode:
        return {"returncode": result.returncode, "stderr": result.stderr}
    from gi.repository import GLib
    return GLib.Variant.parse(None, result.stdout.strip(), None, None).unpack()


def _owner(c, name):
    result = _dbus(c, "org.freedesktop.DBus", "/org/freedesktop/DBus",
                   "org.freedesktop.DBus.GetConnectionUnixProcessID", name)
    pid = int(result[0])
    proc = Path("/proc") / str(pid)
    _require(proc.stat().st_uid == c.owner.pw_uid, "wrong media service owner")
    exe = proc.joinpath("exe").resolve(strict=True)
    _require(exe.stat().st_uid == 0 and not exe.stat().st_mode & 0o022,
             "media service executable is not an immutable installed principal")
    package = c.command(["pacman", "-Qqo", str(exe)]).stdout.strip()
    return {"bus": name, "pid": pid, "uid": proc.stat().st_uid,
            "exe": str(exe), "exe_sha256": hashlib.sha256(exe.read_bytes()).hexdigest(),
            "package": c.command(["pacman", "-Q", package]).stdout.strip(),
            "start_ticks": proc.joinpath("stat").read_text().rsplit(")", 1)[1].split()[19]}


def _permission(c, action, value=None):
    dest = "org.freedesktop.impl.portal.PermissionStore"
    path = "/org/freedesktop/impl/portal/PermissionStore"
    prefix = dest + "."
    if action == "read":
        response = _dbus(c, dest, path, prefix + "Lookup", "devices", "camera", check=False)
        if isinstance(response, dict):
            _require("org.freedesktop.portal.Error.NotFound" in response["stderr"],
                     "permission store unavailable: " + response["stderr"])
            return {"entry_exists": False, "permissions": None, "all_permissions": {},
                    "owner": _owner(c, dest)}
        return {"entry_exists": True, "owner": _owner(c, dest), "all_permissions": response[0],
                "permissions": response[0].get("org.gnome.Snapshot")}
    if action == "set":
        _require(value in ("no", "yes", "ask"), "invalid camera permission")
        _dbus(c, dest, path, prefix + "SetPermission", "devices", "true", "camera",
              "org.gnome.Snapshot", "['" + value + "']")
    elif action == "delete":
        before = _permission(c, "read")
        if before["permissions"] is not None:
            _dbus(c, dest, path, prefix + "DeletePermission", "devices", "camera",
                  "org.gnome.Snapshot")
    else:
        raise ValueError("unknown permission action")
    return _permission(c, "read")


def _prepare(c):
    for executable in ("ffmpeg", "ffprobe", "pactl", "pw-dump", "gdbus", "xdg-user-dir",
                       "v4l2-ctl", "scanimage", "pdfinfo", "pdfimages", "pdftoppm", "gst-inspect-1.0"):
        if shutil.which(executable) is None:
            raise MediaUnavailable("required native media tool unavailable: " + executable)
    for plugin in ("oggdemux", "vorbisdec", "vp8dec", "matroskademux", "gtk4paintablesink",
                   "pipewiresrc", "pipewiredeviceprovider"):
        available = c.user(["gst-inspect-1.0", plugin], timeout=2, check=False)
        if available.returncode:
            raise MediaUnavailable("required native GStreamer feature absent: " + plugin
                                   + " " + available.stderr)
    _require(not c.control_exists("media-state"), "media fixture already prepared")
    music = _user_dir(c, "MUSIC") / ("Tuna-core-" + c.nonce)
    _require(not music.exists() and not music.is_symlink(), "refusing existing Music content")
    pictures = _user_dir(c, "PICTURES") / "Camera"
    old_photos = sorted(str(p) for p in pictures.glob("*") if p.is_file())
    _require(SANE_DIR.parent.resolve(strict=True) == c.root and not SANE_DIR.is_symlink(),
             "unsafe fixed SANE fixture directory")
    _require(not SANE_DIR.exists() or
             (SANE_DIR.is_dir() and SANE_DIR.stat().st_uid == c.owner.pw_uid and
              not any(SANE_DIR.iterdir())), "SANE fixture directory not empty or not user-owned")
    state = {"schema": 1, "music_dir": str(music), "pictures": str(pictures), "old_photos": old_photos,
             "vivid_owned": False, "vivid_bound": False, "audio_stopped": False,
             "index_stopped": False, "camera_stopped": False, "permission": None,
             "sane_owned": True, "permission_owned": False, "restored": False}
    _save(c, state)
    _user_file(c, "directory", _path(c, ""))
    # XDG directories can be localized, so create through the user's own API,
    # not a privileged mkdir/chown against the reported path.
    c.user(["mkdir", "--", str(music)])
    _user_file(c, "directory", SANE_DIR)
    _owned(c, _path(c, "broken.ogg"), b"Not an Ogg bitstream: native decoder negative control\n")
    _owned(c, _path(c, "broken.webm"), b"Not an EBML bitstream: native decoder negative control\n")
    _sane(c, "ready")
    return {"status": "prepared-control-directories", "audio": str(_path(c, "tone.ogg")),
            "video": str(_path(c, "moving-control.webm")), "music": str(music / "native-control.ogg"),
            "sane_config_dir": str(SANE_DIR), "sources": SOURCES,
            "installed_signed_release_qualified": False}


def _control_codecs(c, path):
    return json.loads(c.user(["ffprobe", "-v", "error", "-show_entries",
        "stream=codec_name,codec_type:format_tags=title,artist,album", "-of", "json", str(path)],
        timeout=4).stdout)


def _prepare_audio(c):
    state = _state(c)
    audio = _path(c, "tone.ogg")
    indexed = Path(state["music_dir"]) / "native-control.ogg"
    _require(not audio.exists() and not indexed.exists(), "audio control already exists")
    c.user(["ffmpeg", "-nostdin", "-v", "error", "-f", "lavfi", "-i",
            "aevalsrc=0.18*sin(2*PI*997*t)+0.18*sin(2*PI*1499*t):s=48000:d=300",
            "-c:a", "libvorbis", "-q:a", "5", "-metadata", "title=Tuna Media Control",
            "-metadata", "artist=Tuna Native Artist", "-metadata", "album=Tuna Native Album",
            str(audio)], timeout=18)
    c.user(["cp", "--no-target-directory", "--", str(audio), str(indexed)])
    controls = _control_codecs(c, audio)
    _require([s["codec_name"] for s in controls["streams"]] == ["vorbis"], "audio control codec differs")
    return {"status": "prepared-control-only", "audio": str(audio), "indexed_file": str(indexed),
            "expected_frequencies_hz": [997, 1499], "negative_frequencies_hz": [613, 2203],
            "encoded_metadata": controls}


def _prepare_video(c):
    _state(c)
    video = _path(c, "moving-control.webm")
    _require(not video.exists(), "video control already exists")
    graph = ("color=c=red:s=648x360:r=15:d=180,"
             "drawbox=x=108:y=0:w=108:h=360:c=0x00ff00:t=fill,"
             "drawbox=x=216:y=0:w=108:h=360:c=blue:t=fill,"
             "drawbox=x=324:y=0:w=108:h=360:c=yellow:t=fill,"
             "drawbox=x=432:y=0:w=108:h=360:c=magenta:t=fill,"
             "drawbox=x=540:y=0:w=108:h=360:c=cyan:t=fill[base];"
             "color=c=white:s=48x48:r=15:d=180[box];"
             "[base][box]overlay=x=mod(t*81\\,594):y=220[out]")
    c.user(["ffmpeg", "-nostdin", "-v", "error", "-filter_complex", graph,
            "-f", "lavfi", "-i", "aevalsrc=0.18*sin(2*PI*997*t)+0.18*sin(2*PI*1499*t):s=48000:d=180",
            "-map", "[out]", "-map", "0:a", "-c:v", "libvpx", "-deadline", "realtime",
            "-cpu-used", "8", "-b:v", "600k", "-c:a", "libvorbis", str(video)], timeout=18)
    controls = _control_codecs(c, video)
    _require({s["codec_name"] for s in controls["streams"]} == {"vp8", "vorbis"},
             "video control codecs differ")
    return {"status": "prepared-control-only", "video": str(video), "encoded_metadata": controls,
            "color_order": ["red", "green", "blue", "yellow", "magenta", "cyan"],
            "square_size": [48, 48], "square_y": 220, "square_x": "mod(t*81,594)",
            "expected_frequencies_hz": [997, 1499]}


def audio_metrics(samples, rate):
    """Measure actual PCM against both encoded tones and independent off-tone controls."""
    _require(rate == 48000 and len(samples) >= 24000, "short or wrong-rate audio capture")
    values = [v / 32768 for v in samples]
    rms = math.sqrt(sum(v * v for v in values) / len(values))
    amplitudes = {}
    for frequency in (997, 1499, 613, 2203):
        coefficient = 2 * math.cos(2 * math.pi * frequency / rate)
        previous = previous2 = 0.0
        for value in values:
            current = value + coefficient * previous - previous2
            previous2, previous = previous, current
        power = previous * previous + previous2 * previous2 - coefficient * previous * previous2
        amplitudes[str(frequency)] = 2 * math.sqrt(max(0, power)) / len(values)
    signal = min(amplitudes["997"], amplitudes["1499"])
    control = max(amplitudes["613"], amplitudes["2203"], 0.00001)
    return {"rms": rms, "tone_amplitudes": amplitudes, "signal_to_control": signal / control,
            "matches_tones": signal > 0.008 and signal / control > 25,
            "silence": rms < 0.002}


def _monitor(c, args):
    app = args["app"]
    _require(app in APPS and type(args["pid"]) is int, "invalid audio actor")
    proc = Path("/proc") / str(args["pid"])
    _require(proc.stat().st_uid == c.owner.pw_uid, "audio actor ownership changed")
    _require(proc.joinpath("stat").read_text().rsplit(")", 1)[1].split()[19] ==
             str(args["start_ticks"]), "audio actor PID reused")
    inputs = json.loads(c.user(["pactl", "--format=json", "list", "sink-inputs"]).stdout)
    live = [i for i in inputs if not i["corked"]]
    _require(all(str(i["properties"].get("application.process.id")) == str(args["pid"])
                 for i in live), "monitor contaminated by another application's audio")
    sink = c.user(["pactl", "get-default-sink"]).stdout.strip()
    sources = json.loads(c.user(["pactl", "--format=json", "list", "sources"]).stdout)
    monitors = [s for s in sources if s["name"] == sink + ".monitor"]
    _require(len(monitors) == 1, "no unique real output monitor")
    label = args["label"]
    _require(re.fullmatch(r"[a-zA-Z0-9_-]{1,100}", label), "invalid capture label")
    output = _path(c, label + ".wav")
    _require(not output.exists(), "refusing audio capture overwrite")
    c.user(["ffmpeg", "-nostdin", "-v", "error", "-f", "pulse", "-i", sink + ".monitor",
            "-ss", "0.3", "-t", "1.2", "-ac", "1", "-ar", "48000", "-c:a", "pcm_s16le", str(output)], timeout=10)
    with wave.open(str(output), "rb") as stream:
        _require(stream.getsampwidth() == 2 and stream.getnchannels() == 1, "invalid monitor PCM")
        samples = array.array("h", stream.readframes(stream.getnframes()))
        if os.sys.byteorder != "little":
            samples.byteswap()
        measured = audio_metrics(samples, stream.getframerate())
    return {"status": "observed", "actor_pid": args["pid"], "active_sink_inputs": live,
            "sink": sink, "monitor": monitors[0], "capture": str(output),
            "sha256": hashlib.sha256(output.read_bytes()).hexdigest(), **measured}


def _unit_states(c, units):
    rows = []
    for unit in units:
        output = c.user(["systemctl", "--user", "show", unit,
                         "--property=Id,ActiveState,UnitFileState"]).stdout
        row = dict(line.split("=", 1) for line in output.splitlines() if "=" in line)
        _require(row.get("Id") == unit and row.get("ActiveState") in ("active", "inactive") and
                 isinstance(row.get("UnitFileState"), str) and
                 not row["UnitFileState"].startswith("masked"), "unsafe original media unit: " + output)
        rows.append(row)
    return rows


def _restore_units(c, rows):
    active = [row["Id"] for row in rows if row["ActiveState"] == "active"]
    if active:
        c.user(["systemctl", "--user", "start", *active])
    _require(_unit_states(c, [row["Id"] for row in rows]) == rows,
             "original media service/socket state was not restored")


def _audio_service(c, mode):
    state = _state(c)
    units = ["pipewire-pulse.socket", "pipewire-pulse.service"]
    if mode == "stop":
        _require(not state["audio_stopped"], "audio service already stopped")
        before = _service_process(c, "pipewire-pulse.service")
        state["audio_before"] = _unit_states(c, units)
        state["audio_process_before"] = before
        state["audio_stopped"] = True
        _save(c, state)
        c.user(["systemctl", "--user", "stop", *units])
        unavailable = c.user(["pactl", "info"], check=False)
        _require(unavailable.returncode != 0, "Pulse backend remained reachable after stop")
        return {"status": "expected-unavailable", "baseline": before,
                "returncode": unavailable.returncode, "stderr": unavailable.stderr}
    _require(mode == "restore" and state["audio_stopped"], "invalid audio service transition")
    _restore_units(c, state["audio_before"])
    info = c.user(["pactl", "info"]).stdout
    state["audio_stopped"] = False
    _save(c, state)
    return {"status": "restored", "info": info,
            "owner": _service_process(c, "pipewire-pulse.service")}


def _audio_service_observe(c, args):
    _require(_state(c)["audio_stopped"], "audio loss observation requires the owned outage")
    _require(args["app"] in ("org.gnome.Decibels", "org.gnome.Showtime")
             and type(args["pid"]) is int, "invalid native audio loss consumer")
    proc = Path("/proc") / str(args["pid"])
    _require(proc.stat().st_uid == c.uid and
             proc.joinpath("stat").read_text().rsplit(")", 1)[1].split()[19] == str(args["start_ticks"]),
             "audio loss consumer identity changed")
    unavailable = c.user(["pactl", "info"], check=False)
    _require(unavailable.returncode != 0, "owned Pulse outage unexpectedly became available")
    graph = json.loads(c.user(["pw-dump"], timeout=5).stdout)
    clients = {row["id"] for row in graph if row["type"] == "PipeWire:Interface:Client"
               and str(row.get("info", {}).get("props", {}).get("application.process.id"))
               == str(args["pid"])}
    nodes = [row for row in graph if row["type"] == "PipeWire:Interface:Node"
             and (row.get("info", {}).get("props", {}).get("client.id") in clients
                  or str(row.get("info", {}).get("props", {}).get("application.process.id"))
                  == str(args["pid"]))
             and row.get("info", {}).get("props", {}).get("media.class") == "Stream/Output/Audio"]
    _require(not any(row["info"]["state"] == "running" for row in nodes),
             "original app still has a running audio output during the claimed outage")
    return {"actor_pid": args["pid"], "start_ticks": str(args["start_ticks"]),
            "pulse_unavailable": {"returncode": unavailable.returncode, "stderr": unavailable.stderr},
            "actual_original_output_nodes": nodes, "pipewire_client_ids": sorted(clients),
            "no_running_original_audio_stream": True}


def _scanner_preferences(c):
    result = c.user(["/usr/bin/python", "-c",
        "import json,os; from gi.repository import Gio; "
        "assert os.geteuid() != 0; s=Gio.Settings.new('org.gnome.SimpleScan'); "
        "print(json.dumps({k:s.get_value(k).unpack() for k in "
        "('postproc-enabled','postproc-arguments','postproc-script','document-type',"
        "'photo-dpi','paper-width','paper-height')}))"])
    return json.loads(result.stdout)


def _showtime_position(c, args, history=False):
    bus = "org.mpris.MediaPlayer2.org.gnome.Showtime"
    owner = _owner(c, bus)
    _require(owner["pid"] == args["pid"] and owner["start_ticks"] == str(args["start_ticks"]),
             "Showtime MPRIS readback is not the original native frontend")
    position = _dbus(c, bus, "/org/mpris/MediaPlayer2", "org.freedesktop.DBus.Properties.Get",
                     "org.mpris.MediaPlayer2.Player", "Position")[0]
    if hasattr(position, "unpack"):
        position = position.unpack()
    result = {"owner": owner, "position_us": int(position)}
    if history:
        proc = Path("/proc") / str(args["pid"])
        environment = dict(item.split(b"=", 1) for item in proc.joinpath("environ").read_bytes().split(b"\0")
                           if b"=" in item)
        state_dir = Path(os.fsdecode(environment.get(b"XDG_STATE_HOME", str(c.home / ".local/state").encode())))
        _require(state_dir.is_absolute() and state_dir.resolve().is_relative_to(c.home),
                 "unsafe original native Showtime state directory")
        path = state_dir / "showtime/playback_history.json"
        _require(path.is_file() and not path.is_symlink() and path.stat().st_uid == c.owner.pw_uid,
                 "native Showtime did not persist playback history")
        uri = _path(c, "moving-control.webm").as_uri()
        key = hashlib.sha256(uri.encode()).hexdigest()
        saved = json.loads(path.read_text())[key]
        _require(type(saved) is int and saved >= 60_000_000_000,
                 "native Showtime has no resumable saved position for the actual payload URI")
        result.update(history_file=str(path), saved_position_ns=saved, payload_uri=uri)
    return result


def _library(c):
    owner = _owner(c, INDEX_BUS)
    query = ("SELECT ?title ?album ?artist ?url WHERE { GRAPH tracker:Audio { "
             "?song a nmm:MusicPiece ; nie:title ?title ; nmm:musicAlbum ?a ; "
             "nmm:performer ?p ; nie:isStoredAs ?f . ?a nie:title ?album . "
             "?p nmm:artistName ?artist . ?f nie:url ?url . "
             "FILTER (?title = 'Tuna Media Control') } }")
    service = Path("/usr/share/dbus-1/services/" + INDEX_BUS + ".service")
    _require(service.is_file(), "installed indexer D-Bus activation identity missing")
    if shutil.which("tinysparql") is None:
        raise MediaUnavailable("installed TinySPARQL reader unavailable")
    result = c.user(["tinysparql", "sparql", "--dbus-service", INDEX_BUS, "--query", query])
    expected = str(Path(_state(c)["music_dir"]) / "native-control.ogg")
    return {"owner": owner, "query": query, "rows": result.stdout,
            "matches_metadata": all(s in result.stdout for s in
                 ("Tuna Media Control", "Tuna Native Album", "Tuna Native Artist", Path(expected).as_uri()))}


def _index_service(c, mode):
    state = _state(c)
    if mode == "stop":
        _require(not state["index_stopped"], "index service already stopped")
        baseline = _library(c)
        _require(baseline["matches_metadata"], "indexer negative lacks indexed baseline")
        service = Path("/usr/share/dbus-1/services/" + INDEX_BUS + ".service").read_text()
        match = re.search(r"^SystemdService=(.+)$", service, re.M)
        _require(match and match[1] == "localsearch-3.service",
                 "indexer lacks the installed Arch LocalSearch systemd activation owner")
        state["index_unit"] = match[1]
        state["index_before"] = _unit_states(c, [match[1]])
        state["index_process_before"] = baseline["owner"]
        state["index_stopped"] = True
        _save(c, state)
        c.user(["systemctl", "--user", "mask", "--runtime", "--now", match[1]])
        response = _dbus(c, "org.freedesktop.DBus", "/org/freedesktop/DBus",
                         "org.freedesktop.DBus.NameHasOwner", INDEX_BUS)
        _require(response[0] is False, "index service owner survived stop")
        return {"status": "expected-unavailable", "baseline": baseline, "unit": match[1]}
    _require(mode == "restore" and state["index_stopped"], "invalid indexer transition")
    c.user(["systemctl", "--user", "unmask", "--runtime", state["index_unit"]])
    _restore_units(c, state["index_before"])
    state["index_stopped"] = False
    _save(c, state)
    return {"status": "restored", "owner": _owner(c, INDEX_BUS)}


def _sane(c, mode):
    _require(_state(c)["sane_owned"], "SANE configuration is not owned by this run")
    _require(mode in ("ready", "missing", "io-error"), "invalid SANE condition")
    _require(not SANE_DIR.is_symlink() and SANE_DIR.is_dir(), "unsafe SANE fixture")
    config = ('number_of_devices ' + ('0' if mode == 'missing' else '1') + '\n'
              'mode Color\ndepth 8\nthree-pass false\nresolution_min 75.0\n'
              'resolution_max 75.0\nresolution_quant 1.0\nresolution 75.0\n'
              'test-picture "Color pattern"\ngeometry_min 0.0\ngeometry_max 100.0\n'
              'geometry_quant 1.0\ntl_x 0.0\ntl_y 0.0\nbr_x 80.0\nbr_y 100.0\n'
              'read-status-code "' + ('SANE_STATUS_IO_ERROR' if mode == 'io-error' else 'Default') + '"\n')
    _owned(c, SANE_DIR / "dll.conf", "test\n")
    _owned(c, SANE_DIR / "test.conf", config)
    devices = c.user(["env", "SANE_CONFIG_DIR=" + str(SANE_DIR), "scanimage", "-L"], timeout=5)
    if mode == "missing":
        _require("test:0" not in devices.stdout, "SANE device loss control ineffective")
    else:
        if "test:0" not in devices.stdout or "Noname" not in devices.stdout:
            raise MediaUnavailable("real packaged SANE test backend unavailable: " + devices.stdout)
    return {"status": "control-configured", "condition": mode, "devices": devices.stdout,
            "config": config, "emulated": True, "physical_scanner_qualified": False}


def sane_pattern(image, tolerance=0, expected_size=None):
    """Validate every driver pixel, including page edges and gray separators."""
    image = image.convert("RGB")
    _require(image.width >= 200 and image.height >= 200, "short scanned raster")
    if expected_size is not None:
        _require(image.size == tuple(expected_size), "native scanned raster geometry differs from the device control")
    errors = [0] * 256
    total = 0
    for y in range(image.height):
        for x in range(image.width):
            if y % 5 == 0 or x % 5 == 0:
                expected = (85, 85, 85)
            else:
                band = (y % 30) // 5
                value = (x // 5) & 255
                if band & 1:
                    value = 255 - value
                pixel = [0, 0, 0]
                pixel[band // 2] = value
                expected = tuple(pixel)
            for actual, wanted in zip(image.getpixel((x, y)), expected):
                error = abs(actual - wanted)
                errors[error] += 1
                total += error
    count = image.width * image.height * 3
    mean, cumulative = total / count, 0
    for percentile, frequency in enumerate(errors):
        cumulative += frequency
        if cumulative > int(count * .95):
            break
    _require(mean <= tolerance and percentile <= max(1, tolerance * 3),
             f"export is not the consumed SANE RGB pattern: mean={mean}, p95={percentile}")
    return {"width": image.width, "height": image.height, "samples": count // 3,
            "decoded_rgb_sha256": hashlib.sha256(image.tobytes()).hexdigest(),
            "mean_absolute_error": mean, "p95_error": percentile,
            "reference": SOURCES["sane_pattern"], "emulated": True}


def _scan_export(c, args):
    from PIL import Image
    relative = args["relative"]
    _require(re.fullmatch(r"scan-[a-z0-9-]+\.(png|pdf)", relative), "invalid scan export path")
    path = _path(c, relative)
    _require(path.is_file() and path.stat().st_uid == c.owner.pw_uid, "scan export missing/wrong owner")
    # Simple Scan's Automatic page choice sets both scan-area coordinates to
    # the backend's configured 100mm maximum; test.c truncates 100*75/25.4.
    expected_size = (295, 295)
    if path.suffix == ".png":
        with Image.open(path) as image:
            _require(image.mode == "RGB", "native scanner PNG is not its 8-bit RGB device payload")
            observation = sane_pattern(image, expected_size=expected_size)
    else:
        info = c.user(["pdfinfo", str(path)], timeout=5).stdout
        _require(re.search(r"^Pages:\s+1\s*$", info, re.M) is not None,
                 "native scanner PDF does not contain exactly one document page")
        prefix = _path(c, relative + "-embedded")
        c.user(["pdfimages", "-png", str(path), str(prefix)], timeout=5)
        images = sorted(prefix.parent.glob(prefix.name + "-*.png"))
        _require(len(images) == 1, "scanner PDF does not contain exactly the single scanned raster")
        with Image.open(images[0]) as image:
            embedded = sane_pattern(image, expected_size=expected_size)
        # Independently render the PDF page as well, rejecting hidden/unpainted XObjects.
        rendered = _path(c, relative + "-rendered")
        c.user(["pdftoppm", "-singlefile", "-r", "75", "-png", str(path), str(rendered)], timeout=5)
        with Image.open(str(rendered) + ".png") as image:
            # PDF decimal point rounding/antialiasing is bounded separately from PNG exactness.
            observation = {"embedded": embedded,
                           "rendered": sane_pattern(image, tolerance=22, expected_size=expected_size),
                           "pdfinfo": info}
    return {"status": "observed", "file": str(path), "payload": observation,
            "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
            "physical_scanner_qualified": False}


def _service_process(c, unit):
    output = c.user(["systemctl", "--user", "show", unit,
                     "--property=ActiveState,MainPID,ExecMainStartTimestampMonotonic"]).stdout
    state = dict(line.split("=", 1) for line in output.splitlines() if "=" in line)
    _require(state["ActiveState"] == "active" and int(state["MainPID"]) > 0,
             "required media service unavailable: " + unit + " " + output)
    proc = Path("/proc") / state["MainPID"]
    _require(proc.stat().st_uid == c.owner.pw_uid, "media service has wrong native owner")
    exe = proc.joinpath("exe").resolve(strict=True)
    return {"unit": unit, "state": state, "uid": proc.stat().st_uid, "exe": str(exe),
            "package": c.command(["pacman", "-Qqo", str(exe)]).stdout.strip(),
            "start_ticks": proc.joinpath("stat").read_text().rsplit(")", 1)[1].split()[19]}


def _camera_service(c, mode):
    state = _state(c)
    services = ("pipewire.service", "wireplumber.service")
    if mode == "stop":
        _require(not state["camera_stopped"], "camera backend already stopped")
        before = [_service_process(c, unit) for unit in services]
        _require([row["package"] for row in before] == ["pipewire", "wireplumber"],
                 "camera services do not belong to normal native packages")
        state["camera_before"] = before
        state["camera_units_before"] = _unit_states(c, ["pipewire.socket", *services])
        state["camera_stopped"] = True
        _save(c, state)
        c.user(["systemctl", "--user", "stop", "wireplumber.service",
                "pipewire.socket", "pipewire.service"])
        observed = c.user(["systemctl", "--user", "show", *services,
                          "--property=Id,ActiveState,MainPID"]).stdout
        _require(observed.count("ActiveState=inactive") == 2,
                 "real camera backend did not stop: " + observed)
        return {"status": "expected-service-unavailable", "baseline": before,
                "stopped": observed, "portal_owner": _owner(c, "org.freedesktop.portal.Desktop")}
    _require(mode == "restore" and state["camera_stopped"], "invalid camera service restore")
    _restore_units(c, state["camera_units_before"])
    after = [_service_process(c, unit) for unit in services]
    _require(all((a["state"]["MainPID"], a["start_ticks"]) !=
                 (b["state"]["MainPID"], b["start_ticks"])
                 for a,b in zip(after, state["camera_before"])),
             "camera service restart retained original process identity")
    state["camera_stopped"] = False
    _save(c, state)
    return {"status": "restored", "new_backend_processes": after}


def _scanner_backend(c, args):
    _require(type(args["pid"]) is int, "invalid scanner process")
    proc = Path("/proc") / str(args["pid"])
    _require(proc.stat().st_uid == c.owner.pw_uid and
             proc.joinpath("stat").read_text().rsplit(")", 1)[1].split()[19] == str(args["start_ticks"]),
             "scanner consumer identity changed")
    exe = proc.joinpath("exe").resolve(strict=True)
    _require(c.command(["pacman", "-Qqo", str(exe)]).stdout.strip() == "simple-scan",
             "scanner consumer is not the installed native application")
    environment = dict(item.split(b"=", 1) for item in proc.joinpath("environ").read_bytes().split(b"\0")
                       if b"=" in item)
    _require(environment.get(b"SANE_CONFIG_DIR") == str(SANE_DIR).encode(),
             "normal scanner desktop process did not inherit the supported SANE fixture directory")
    paths = sorted({line.split()[-1] for line in proc.joinpath("maps").read_text().splitlines()
                    if "/sane/libsane-test.so" in line})
    _require(len(paths) == 1, "native scanner did not load the genuine SANE test backend")
    package = c.command(["pacman", "-Qqo", paths[0]]).stdout.strip()
    _require(package == "sane", "test backend is not the normal packaged SANE driver")
    return {"native_pid": args["pid"], "driver": paths[0], "package": package,
            "sane_config_dir": str(SANE_DIR), "service_applicability":
                "Simple Scan calls libsane-test in-process; no SANE daemon or physical scanner is claimed.",
            "emulated": True, "physical_scanner_qualified": False}


def _camera_device(c, mode):
    state = _state(c)
    driver = Path("/sys/bus/platform/drivers/vivid")
    if mode == "prepare":
        _require(not state["vivid_owned"], "Vivid load already journaled")
        _require(not Path("/sys/module/vivid").exists() and not Path(CAMERA).exists(),
                 "refusing pre-existing Vivid/device ownership")
        located = c.command(["modinfo", "-n", "vivid"], check=False)
        if located.returncode:
            raise MediaUnavailable("running original kernel lacks the Vivid module: " + located.stderr)
        module = str(Path(located.stdout.strip()).resolve(strict=True))
        _require(module.startswith("/usr/lib/modules/" + os.uname().release + "/"),
                 "Vivid is not from the running original kernel module archive")
        package = c.command(["pacman", "-Qqo", module]).stdout.strip()
        _require(package == "linux", "Vivid is not owned by the original native linux package")
        metadata = Path(module).stat()
        _require(metadata.st_uid == 0 and not metadata.st_mode & 0o022,
                 "Vivid module archive is not an immutable installed source")
        state["vivid_original"] = {"module_present": False, "device_present": False}
        state["vivid_module"] = {"path": module, "package": package,
                                  "sha256": hashlib.sha256(Path(module).read_bytes()).hexdigest()}
        state["vivid_owned"] = True
        state["vivid_bound"] = True
        # Journal responsibility and source BEFORE modprobe: a failed or timed
        # out load must still leave cleanup able to unload its actual effects.
        _save(c, state)
        c.command(["modprobe", "vivid", "n_devs=1", "node_types=1", "num_inputs=1",
                   "input_types=0", "vid_cap_nr=42"])
        devices = [p.name for p in driver.iterdir()
                   if re.fullmatch(r"vivid\.\d+", p.name) and p.is_symlink()]
        _require(len(devices) == 1, "no unique fixture-owned Vivid platform device")
        state["vivid_platform"] = devices[0]
        _save(c, state)
    elif mode == "remove":
        _require(state["vivid_owned"] and state["vivid_bound"], "Vivid is not fixture-bound")
        state["vivid_bound"] = False
        _save(c, state)
        c.command(["tee", str(driver / "unbind")], input=state["vivid_platform"] + "\n")
        _require(not Path(CAMERA).exists(), "Vivid device survived actual driver unbind")
        return {"status": "expected-device-unavailable", "device": CAMERA,
                "platform": state["vivid_platform"], "emulated": True}
    elif mode == "restore":
        _require(state["vivid_owned"] and not state["vivid_bound"], "invalid Vivid rebind")
        state["vivid_bound"] = True
        _save(c, state)
        c.command(["tee", str(driver / "bind")], input=state["vivid_platform"] + "\n")
    else:
        raise ValueError("unknown camera device transition")
    identity = c.command(["v4l2-ctl", "--device", CAMERA, "--all"]).stdout
    _require("vivid" in identity.lower() and Path(CAMERA).exists(), "named Vivid camera unavailable")
    return {"status": "available", "device": CAMERA, "identity": identity,
            "module": state["vivid_module"], "emulated": True,
            "physical_camera_qualified": False}


def _camera_pattern(c, mode):
    _require(_state(c)["vivid_owned"], "camera control requires fixture-owned Vivid")
    _require(mode in ("bars", "white"), "invalid Vivid payload control")
    controls = c.command(["v4l2-ctl", "--device", CAMERA, "--list-ctrls-menus"]).stdout
    menu = re.search(r"test_pattern\s.*?(?=\n\s*\w+\s+0x|\Z)", controls, re.S)
    _require(menu is not None, "Vivid test_pattern control unavailable")
    desired = "100% Colorbar" if mode == "bars" else "100% White"
    choice = re.search(r"^\s*(\d+):\s*" + re.escape(desired) + r"\s*$", menu[0], re.M)
    _require(choice is not None, "Vivid lacks named pattern " + desired)
    c.command(["v4l2-ctl", "--device", CAMERA, "--set-ctrl", "test_pattern=" + choice[1]])
    readback = c.command(["v4l2-ctl", "--device", CAMERA, "--get-ctrl", "test_pattern"]).stdout
    _require(re.search(r"test_pattern:\s*" + choice[1] + r"\b", readback) is not None,
             "Vivid pattern control did not take effect")
    return {"status": "control-configured", "pattern": desired, "readback": readback,
            "emulated": True}


def camera_pixels(image, mode):
    image = image.convert("RGB")
    _require(image.width >= 300 and image.height >= 200, "camera output too small")
    pixels = [image.getpixel((x, y)) for y in range(image.height // 4, image.height * 3 // 4, 7)
              for x in range(image.width // 10, image.width * 9 // 10, 7)]
    white_fraction = sum(min(p) > 225 for p in pixels) / len(pixels)
    if mode == "white":
        _require(white_fraction > .97, "captured image is not Vivid's white control")
        return {"pattern": "100% White", "white_fraction": white_fraction}
    _require(mode == "bars", "unknown camera pixel expectation")
    palette = [(255,255,255), (255,255,0), (0,255,255), (0,255,0),
               (255,0,255), (255,0,0), (0,0,255), (0,0,0)]
    # Webcam preview may mirror; exported photo must still contain all eight ordered bars.
    rows = []
    for y in (image.height // 3, image.height // 2, image.height * 2 // 3):
        colors = [image.getpixel((int(image.width * (i + .5) / 8), y)) for i in range(8)]
        error = min(sum(abs(a-b) for p,q in zip(colors, order) for a,b in zip(p,q)) / 24
                    for order in (palette, list(reversed(palette))))
        rows.append(error)
    _require(max(rows) < 32 and white_fraction < .3, "captured photo lacks ordered Vivid colorbars")
    return {"pattern": "100% Colorbar", "row_mean_errors": rows, "white_fraction": white_fraction}


def _camera_files(c, args):
    from PIL import Image
    state = _state(c)
    directory = Path(state["pictures"])
    old = set(state["old_photos"])
    files = sorted(p for p in directory.glob("*") if str(p) not in old and p.is_file())
    after = args.get("after", [])
    _require(isinstance(after, list) and all(isinstance(p, str) for p in after), "invalid photo baseline")
    files = [p for p in files if str(p) not in after]
    if args.get("mode") == "inventory":
        return {"files": [str(p) for p in files]}
    _require(len(files) == 1, "expected exactly one new native camera output")
    path = files[0]
    _require(path.stat().st_uid == c.owner.pw_uid and not path.is_symlink(), "wrong photo owner")
    with Image.open(path) as image:
        measured = camera_pixels(image, args["mode"])
    return {"status": "observed", "file": str(path), "payload": measured,
            "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
            "physical_camera_qualified": False}




def _cleanup_vivid(c):
    state = _state(c)
    # Unloading the wholly owned module also removes a still-bound platform
    # device. Do not depend on a post-modprobe platform receipt: load may have
    # succeeded immediately before a failed/retired guest operation.
    if Path("/sys/module/vivid").exists():
        c.command(["modprobe", "-r", "vivid"])
    _require(not Path("/sys/module/vivid").exists() and not Path(CAMERA).exists(),
             "fixture-owned Vivid effects remain after cleanup")
    state["vivid_owned"] = False
    state["vivid_bound"] = False
    _save(c, state)


def _cleanup_permission(c):
    state = _state(c)
    original = state["permission"]
    old = original["permissions"]
    if old is None:
        _permission(c, "delete")
        current = _permission(c, "read")
        if not original["entry_exists"] and not current["all_permissions"] and current["entry_exists"]:
            _dbus(c, "org.freedesktop.impl.portal.PermissionStore",
                  "/org/freedesktop/impl/portal/PermissionStore",
                  "org.freedesktop.impl.portal.PermissionStore.Delete", "devices", "camera")
    else:
        _permission(c, "set", old[0])
    current = _permission(c, "read")
    _require(current["permissions"] == old, "original native camera permission was not restored")
    _require(original["entry_exists"] or current["all_permissions"] or not current["entry_exists"],
             "fixture-created empty camera permission entry remains")
    state["permission_owned"] = False
    _save(c, state)


def _cleanup_sane(c):
    state = _state(c)
    for name in ("dll.conf", "test.conf"):
        _user_file(c, "remove", SANE_DIR / name)
    state["sane_owned"] = False
    _save(c, state)


def _cleanup(c):
    if not c.control_exists("media-state"):
        return {"status": "no-owned-effects-preparation-not-reached"}
    _state(c)  # Malformed/missing authority fails closed, never means restored.
    errors = []
    for flag, restore in (
            ("audio_stopped", lambda: _audio_service(c, "restore")),
            ("index_stopped", lambda: _index_service(c, "restore")),
            ("camera_stopped", lambda: _camera_service(c, "restore")),
            ("vivid_owned", lambda: _cleanup_vivid(c)),
            ("permission_owned", lambda: _cleanup_permission(c)),
            ("sane_owned", lambda: _cleanup_sane(c))):
        if _state(c)[flag]:
            try:
                restore()
            except Exception as error:
                # A failed restoration must not strand independent owned
                # module/service effects. Retain failure, never report green.
                errors.append(flag + ": " + str(error))
    if errors:
        raise RuntimeError("media cleanup incomplete: " + "; ".join(errors))
    state = _state(c)
    state["restored"] = True
    _save(c, state)
    return {"status": "restored", "installed_signed_release_qualified": False}


def dispatch(operation, arguments, context):
    """Finite typed operations only; no caller-supplied commands, buses, devices or code."""
    c, a = context, arguments
    if operation == "prepare-inventory":
        return _prepare(c)
    if operation == "prepare-audio":
        return _prepare_audio(c)
    if operation == "prepare-video":
        return _prepare_video(c)
    if operation == "monitor":
        return _monitor(c, a)
    if operation == "audio-service":
        return _audio_service(c, a["mode"])
    if operation == "audio-service-observe":
        return _audio_service_observe(c, a)
    if operation == "showtime-position":
        return _showtime_position(c, a)
    if operation == "showtime-history":
        return _showtime_position(c, a, history=True)
    if operation == "library":
        return _library(c)
    if operation == "index-service":
        return _index_service(c, a["mode"])
    if operation == "sane":
        return _sane(c, a["mode"])
    if operation == "scanner-preferences":
        return _scanner_preferences(c)
    if operation == "scan-export":
        return _scan_export(c, a)
    if operation == "scanner-backend":
        return _scanner_backend(c, a)
    if operation == "camera-service":
        return _camera_service(c, a["mode"])
    if operation == "camera-device":
        return _camera_device(c, a["mode"])
    if operation == "camera-pattern":
        return _camera_pattern(c, a["mode"])
    if operation == "camera-files":
        return _camera_files(c, a)
    if operation == "camera-permission":
        state = _state(c)
        if state["permission"] is None:
            state["permission"] = _permission(c, "read")
        _require(not state["restored"], "media controls have already been restored")
        state["permission_owned"] = True
        _save(c, state)
        return _permission(c, a["action"], a.get("value"))
    if operation == "cleanup":
        return _cleanup(c)
    raise ValueError("unknown media operation: " + operation)
