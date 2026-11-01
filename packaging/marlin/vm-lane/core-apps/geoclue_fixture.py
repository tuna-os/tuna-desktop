"""Owned static-location input for the packaged GeoClue service, not GPS."""

import base64
import configparser
import fcntl
import hashlib
import json
import os
from pathlib import Path
import pwd
import re
import shlex
import stat
import subprocess


_RUN = Path("/run")
_MAIN = Path("/etc/geoclue/geoclue.conf")
_CONF_DIR = _MAIN.parent / "conf.d"
_LOCATION = Path("/etc/geolocation")
_LOCK = _RUN / "tuna-core-geoclue-static.lock"
_PREFIX = "tuna-core-geoclue-static-"
_SOURCES = ("ip", "network-nmea", "3g", "cdma", "modem-gps", "wifi", "compass",
            "static-source")
_APPS = ("org.gnome.Maps", "org.gnome.Weather", "gnome-datetime-panel")
_LOCATION_BYTES = b"40.6893129\n-74.0445531\n96\n1.83\n"
_CONF_BYTES = ("\n\n".join(
    f"[{source}]\nenable={'true' if source == 'static-source' else 'false'}"
    for source in _SOURCES) + "\n").encode("ascii")
_EMULATION = {
    "name": "New York static GeoClue emulation",
    "kind": "packaged-geoclue-static-source",
    "not_gps": True,
    "latitude": 40.6893129,
    "longitude": -74.0445531,
    "altitude": 96,
    "accuracy_radius": 1.83,
    "format_documentation": "https://man.archlinux.org/man/geoclue.5.txt",
}


class FixtureError(RuntimeError):
    """Keep the actual failed command and restoration evidence available."""

    def __init__(self, message, receipt):
        self.receipt = receipt
        super().__init__(message + ": " + json.dumps(receipt, sort_keys=True))


def _sha(raw):
    return hashlib.sha256(raw).hexdigest()


def _directory(path):
    info = path.lstat()
    if (not stat.S_ISDIR(info.st_mode) or info.st_uid != 0
            or info.st_mode & 0o022):
        raise RuntimeError(f"unsafe root fixture directory: {path}")
    return info


def _read_file(path, absent=False, private=False, limit=1024 * 1024):
    try:
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    except FileNotFoundError:
        if absent:
            return None
        raise
    try:
        info = os.fstat(fd)
        if not stat.S_ISREG(info.st_mode) or info.st_size > limit:
            raise RuntimeError(f"nonregular or oversized fixture input: {path}")
        if private and (info.st_uid != 0 or info.st_gid != 0 or info.st_nlink != 1
                        or stat.S_IMODE(info.st_mode) != 0o600):
            raise RuntimeError(f"fixture state is not singly-linked root-owned mode 0600: {path}")
        chunks = []
        size = 0
        while True:
            chunk = os.read(fd, min(65536, limit + 1 - size))
            if not chunk:
                break
            chunks.append(chunk)
            size += len(chunk)
            if size > limit:
                raise RuntimeError(f"fixture input grew beyond bound: {path}")
        raw = b"".join(chunks)
        after = os.fstat(fd)
        named = path.lstat()
        if ((info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns)
                != (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns)
                or (named.st_dev, named.st_ino) != (info.st_dev, info.st_ino)):
            raise RuntimeError(f"fixture input changed during observation: {path}")
        return raw, info
    finally:
        os.close(fd)


def _file_receipt(path, value):
    if value is None:
        return {"path": str(path), "exists": False}
    raw, info = value
    return {"path": str(path), "exists": True, "sha256": _sha(raw),
            "bytes": len(raw), "mode": stat.S_IMODE(info.st_mode),
            "uid": info.st_uid, "gid": info.st_gid}


def _fsync_directory(path):
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def _atomic(path, raw, mode, uid, gid):
    _directory(path.parent)
    existing = _read_file(path, absent=True)
    if existing is not None and existing[1].st_nlink != 1:
        raise RuntimeError(f"refusing multiply-linked fixture target: {path}")
    temporary = path.with_name("." + path.name + ".tuna-static-tmp")
    fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                 0o600)
    try:
        with os.fdopen(fd, "wb", closefd=False) as stream:
            stream.write(raw)
            stream.flush()
            os.fchown(fd, uid, gid)
            os.fchmod(fd, mode)
            os.fsync(fd)
        os.replace(temporary, path)
        _fsync_directory(path.parent)
    finally:
        os.close(fd)
        if temporary.exists():
            temporary.unlink()


class _Session:
    def __init__(self, context, operation):
        nonce = context.nonce
        if (os.geteuid() != 0 or not isinstance(nonce, str)
                or not re.fullmatch(r"[0-9a-fA-F][0-9a-fA-F-]{15,63}", nonce)
                or context.owner.pw_uid == 0):
            raise RuntimeError("root observer, nonroot original owner and hex/UUID nonce required")
        self.context = context
        self.nonce = nonce
        self.state_path = _RUN / (_PREFIX + nonce + ".json")
        self.conf_path = _CONF_DIR / ("99-tuna-static-" + nonce + ".conf")
        self.packages = {}
        self.commands = []
        self.receipt = {
            "operation": operation, "nonce": nonce, "emulation": dict(_EMULATION),
            "state_path": str(self.state_path),
            "fixture": {"configuration_path": str(self.conf_path),
                        "geolocation_path": str(_LOCATION),
                        "configuration_sha256": _sha(_CONF_BYTES),
                        "geolocation_sha256": _sha(_LOCATION_BYTES),
                        "agent_or_application_policy_written": False},
            "commands": self.commands,
        }
        self.lock_fd = None

    def __enter__(self):
        for path in (Path("/etc"), _MAIN.parent, _RUN):
            _directory(path)
        self.lock_fd = os.open(
            _LOCK, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW | os.O_NONBLOCK, 0o600)
        info = os.fstat(self.lock_fd)
        if (not stat.S_ISREG(info.st_mode) or info.st_uid != 0
                or stat.S_IMODE(info.st_mode) != 0o600 or info.st_nlink != 1):
            os.close(self.lock_fd)
            self.lock_fd = None
            raise RuntimeError("unsafe GeoClue fixture lock")
        try:
            fcntl.flock(self.lock_fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            os.close(self.lock_fd)
            self.lock_fd = None
            raise RuntimeError("concurrent GeoClue fixture operation") from error
        try:
            foreign = [
                str(path) for path in _RUN.glob(_PREFIX + "*.json")
                if path != self.state_path
            ]
            if _CONF_DIR.exists() or _CONF_DIR.is_symlink():
                _directory(_CONF_DIR)
                foreign.extend(
                    str(path) for path in _CONF_DIR.glob("99-tuna-static-*.conf")
                    if path != self.conf_path)
            if foreign:
                raise RuntimeError("foreign GeoClue fixture already exists: " + ", ".join(foreign))
        except BaseException:
            self.__exit__(None, None, None)
            raise
        return self

    def __exit__(self, *_):
        if self.lock_fd is not None:
            os.close(self.lock_fd)
            self.lock_fd = None

    def run(self, argv, timeout=10):
        row = {"argv": list(argv)}
        self.commands.append(row)
        try:
            result = self.context.command(list(argv), timeout=timeout, check=False)
        except subprocess.TimeoutExpired as error:
            row.update(returncode=None, timeout=timeout,
                       stdout=_text(error.stdout), stderr=_text(error.stderr))
            raise FixtureError("bounded fixture command timed out", self.receipt) from error
        except subprocess.CalledProcessError as error:
            row.update(returncode=error.returncode,
                       stdout=_text(error.stdout), stderr=_text(error.stderr))
            raise FixtureError("fixture command failed", self.receipt) from error
        except OSError as error:
            row.update(returncode=None, stdout="", stderr=str(error))
            raise FixtureError("fixture command unavailable", self.receipt) from error
        row.update(returncode=result.returncode, stdout=result.stdout, stderr=result.stderr)
        if result.returncode != 0:
            raise FixtureError("fixture command failed", self.receipt)
        return result

    def package(self, path):
        key = str(path)
        if key not in self.packages:
            ownership = self.run(["pacman", "-Qo", key])
            name = self.run(["pacman", "-Qoq", key]).stdout.strip()
            if not re.fullmatch(r"[a-zA-Z0-9@_+.-]+", name):
                raise RuntimeError("ambiguous installed GeoClue file package ownership")
            installed = self.run(["pacman", "-Q", name]).stdout.strip()
            fields = installed.split()
            if len(fields) != 2 or fields[0] != name:
                raise RuntimeError("invalid installed GeoClue package identity")
            self.packages[key] = {
                "name": name, "version": fields[1],
                "ownership": ownership.stdout.strip(), "query": installed,
            }
        return self.packages[key]

    def bus(self, method, signature="", value=None):
        argv = ["busctl", "--system", "--timeout=5s", "call",
                "org.freedesktop.DBus", "/org/freedesktop/DBus",
                "org.freedesktop.DBus", method]
        if signature:
            argv.extend((signature, value))
        parts = shlex.split(self.run(argv, timeout=7).stdout)
        expected = "b" if method == "NameHasOwner" else (
            "s" if method == "GetNameOwner" else "u")
        if len(parts) != 2 or parts[0] != expected:
            raise RuntimeError("invalid actual system D-Bus ownership reply")
        return parts[1]

    def load(self):
        value = _read_file(self.state_path, absent=True, private=True)
        if value is None:
            return None
        state = json.loads(value[0])
        if (state.get("schema") != 1 or state.get("nonce") != self.nonce
                or state.get("uid") != self.context.owner.pw_uid
                or state.get("gid") != self.context.owner.pw_gid):
            raise RuntimeError("GeoClue fixture state belongs to a different original context")
        original = state["original"]["geolocation"]
        if original["exists"]:
            raw = base64.b64decode(state["geolocation_backup_base64"], validate=True)
            if _sha(raw) != original["sha256"] or len(raw) != original["bytes"]:
                raise RuntimeError("durable original GeoClue input backup is corrupt")
            if (not all(type(original[key]) is int for key in ("mode", "uid", "gid"))
                    or not 0 <= original["mode"] <= 0o7777
                    or original["uid"] < 0 or original["gid"] < 0):
                raise RuntimeError("invalid original GeoClue input metadata")
        return state

    def save(self, state):
        _atomic(self.state_path, json.dumps(state, sort_keys=True).encode("utf-8"),
                0o600, 0, 0)


def _text(value):
    return value.decode("utf-8", errors="replace") if isinstance(value, bytes) else (value or "")


def _policy(parser, app):
    present = parser.has_section(app)
    result = {"section_present": present, "allowed": None, "system": None,
              "users": None}
    if present:
        for key in ("allowed", "system"):
            if parser.has_option(app, key):
                result[key] = parser.getboolean(app, key)
        if parser.has_option(app, "users"):
            entries = parser.get(app, "users").split(";")
            if any(entry.strip() and not entry.strip().isdecimal() for entry in entries):
                raise RuntimeError("nonnumeric UID in installed GeoClue application policy")
            result["users"] = [int(entry.strip()) for entry in entries if entry.strip()]
    result["agent_authorization_bypassed_by_allowed_true"] = result["allowed"] is True
    return result


def _source(parser, source):
    present = parser.has_section(source)
    explicit = parser.has_option(source, "enable")
    return {"section_present": present, "enable_explicit": explicit,
            "enabled": parser.getboolean(source, "enable") if explicit else True}


def _configuration(session):
    main_package = session.package(_MAIN)
    if main_package["name"] != "geoclue":
        raise RuntimeError("GeoClue main configuration is not owned by the installed geoclue package")
    paths = [_MAIN]
    if _CONF_DIR.exists() or _CONF_DIR.is_symlink():
        _directory(_CONF_DIR)
        paths.extend(sorted(_CONF_DIR.glob("*.conf")))
    effective = configparser.ConfigParser(interpolation=None, strict=True)
    files = []
    for path in paths:
        value = _read_file(path)
        raw, info = value
        if info.st_uid != 0 or info.st_mode & 0o022:
            raise RuntimeError(f"mutable or non-root-owned GeoClue configuration: {path}")
        parser = configparser.ConfigParser(interpolation=None, strict=True)
        try:
            parser.read_string(raw.decode("utf-8"), source=str(path))
            effective.read_string(raw.decode("utf-8"), source=str(path))
            policies = {app: _policy(parser, app) for app in _APPS}
            sources = {source: _source(parser, source) for source in _SOURCES}
        except (configparser.Error, UnicodeError, ValueError):
            # Parser exceptions can contain an entire secret-bearing config line.
            raise RuntimeError(f"invalid GeoClue configuration values: {path}") from None
        files.append(_file_receipt(path, value) | {
            "application_policy": policies, "source_coverage": sources})
    try:
        policies = {app: _policy(effective, app) for app in _APPS}
        sources = {source: _source(effective, source) for source in _SOURCES}
    except (configparser.Error, ValueError):
        raise RuntimeError("invalid effective GeoClue configuration values") from None
    return {"main_package": main_package, "files": files,
            "application_policy": policies, "source_coverage": sources}


def _executable(session, path):
    path = Path(path)
    if not path.is_absolute():
        raise RuntimeError("packaged GeoClue executable is not an absolute path")
    value = _read_file(path, limit=64 * 1024 * 1024)
    raw, info = value
    if (info.st_uid != 0 or info.st_mode & 0o022 or not info.st_mode & 0o111
            or not raw.startswith(b"\x7fELF")):
        raise RuntimeError("GeoClue executable is not a protected installed native ELF")
    package = session.package(path)
    if package["name"] != "geoclue":
        raise RuntimeError("GeoClue daemon is not owned by the installed geoclue package")
    return _file_receipt(path, value) | {"package": package, "format": "ELF"}


def _service(session):
    properties = ("Id", "LoadState", "ActiveState", "SubState", "MainPID", "Result",
                  "ExecMainCode", "ExecMainStatus", "FragmentPath", "DropInPaths",
                  "ExecStart", "User", "UnitFileState")
    reply = session.run(["systemctl", "show", "geoclue.service", "--no-pager",
                         "--property=" + ",".join(properties)])
    unit = dict(line.split("=", 1) for line in reply.stdout.splitlines() if "=" in line)
    if (unit.get("Id") != "geoclue.service" or unit.get("LoadState") != "loaded"
            or unit.get("ActiveState") not in ("active", "inactive", "failed")):
        raise RuntimeError("actual packaged GeoClue unit is unavailable or transitional")
    fragment = Path(unit["FragmentPath"])
    fragment_value = _read_file(fragment)
    if (fragment_value[1].st_uid != 0 or fragment_value[1].st_mode & 0o022
            or session.package(fragment)["name"] != "geoclue"):
        raise RuntimeError("GeoClue unit is not protected installed geoclue package content")
    dropins = []
    for name in shlex.split(unit.get("DropInPaths", "")):
        path = Path(name)
        value = _read_file(path)
        if value[1].st_uid != 0 or value[1].st_mode & 0o022:
            raise RuntimeError("mutable GeoClue service drop-in")
        dropins.append(_file_receipt(path, value) | {"package": session.package(path)})
    match = re.search(r"(?:^|[ {;])path=([^ ;}]+)", unit.get("ExecStart", ""))
    if not match:
        raise RuntimeError("cannot identify actual GeoClue unit executable")
    executable = _executable(session, match.group(1))
    result = {"unit": unit, "active": unit["ActiveState"] == "active",
              "unit_file": _file_receipt(fragment, fragment_value)
                           | {"package": session.package(fragment)},
              "dropins": dropins, "configured_executable": executable, "owner": None}
    has_owner = session.bus("NameHasOwner", "s", "org.freedesktop.GeoClue2")
    if has_owner not in ("true", "false"):
        raise RuntimeError("invalid GeoClue D-Bus availability boolean")
    if has_owner == "true":
        owner = session.bus("GetNameOwner", "s", "org.freedesktop.GeoClue2")
        if not re.fullmatch(r":[0-9]+\.[0-9]+", owner):
            raise RuntimeError("GeoClue bus owner is not a unique system-bus connection")
        pid = int(session.bus("GetConnectionUnixProcessID", "s", owner))
        uid = int(session.bus("GetConnectionUnixUser", "s", owner))
        if pid <= 1:
            raise RuntimeError("invalid actual GeoClue service PID")
        process = Path("/proc") / str(pid)
        before = (process / "stat").read_text()
        fields = before.rsplit(")", 1)[1].split()
        start_ticks = int(fields[19])
        process_uid = process.stat().st_uid
        target = os.readlink(process / "exe")
        actual_executable = _executable(session, target)
        expected_uid = pwd.getpwnam("geoclue").pw_uid
        if (expected_uid == 0 or uid != expected_uid or process_uid != uid
                or int(unit.get("MainPID", "0")) != pid
                or actual_executable["path"] != executable["path"]
                or actual_executable["sha256"] != executable["sha256"]):
            raise RuntimeError("actual GeoClue bus/process/package principal differs from its service")
        after = (process / "stat").read_text().rsplit(")", 1)[1].split()
        if (int(after[19]) != start_ticks or process.stat().st_uid != uid
                or os.readlink(process / "exe") != target
                or session.bus("GetNameOwner", "s", "org.freedesktop.GeoClue2") != owner):
            raise RuntimeError("actual GeoClue process changed during identity observation")
        result["owner"] = {"unique_name": owner, "pid": pid, "uid": uid,
                           "start_ticks": start_ticks, "exe": target,
                           "exe_sha256": actual_executable["sha256"],
                           "package": actual_executable["package"], "format": "ELF"}
    if result["active"] != (result["owner"] is not None):
        raise RuntimeError("GeoClue unit activity and actual system-bus owner disagree")
    result["packaged_service_admitted"] = True
    return result


def _observation(session):
    return {"configuration": _configuration(session), "service": _service(session),
            "geolocation": _file_receipt(_LOCATION, _read_file(_LOCATION, absent=True))}


def _without_fixture(configuration, session):
    return {**configuration, "files": [
        value for value in configuration["files"]
        if value["path"] != str(session.conf_path)]}


def _original_files_match(configuration, original, session):
    return (_without_fixture(configuration, session)["files"]
            == original["files"]
            and configuration["main_package"] == original["main_package"]
            and configuration["application_policy"] == original["application_policy"])


def _owned_file(path, expected, required=True):
    value = _read_file(path, absent=True)
    if value is None:
        if required:
            raise RuntimeError(f"owned GeoClue fixture input disappeared: {path}")
        return None
    raw, info = value
    if (raw != expected or info.st_uid != 0 or info.st_gid != 0
            or stat.S_IMODE(info.st_mode) != 0o644 or info.st_nlink != 1):
        raise RuntimeError(f"owned GeoClue fixture input changed; refusing overwrite: {path}")
    return value


def _verify_prepared(session, state):
    _owned_file(session.conf_path, _CONF_BYTES)
    _owned_file(_LOCATION, _LOCATION_BYTES)
    current = _observation(session)
    if not _original_files_match(
            current["configuration"], state["original"]["configuration"], session):
        raise RuntimeError("nonfixture GeoClue configuration or native application policy changed")
    if any(current["configuration"]["source_coverage"][source]["enabled"]
           != (source == "static-source") for source in _SOURCES):
        raise RuntimeError("installed effective GeoClue sources do not isolate the static source")
    if not current["service"]["active"]:
        raise RuntimeError("actual packaged GeoClue service is not active")
    return current


def _restore_locked(session, state):
    session.receipt["original"] = state["original"]
    # A failed restart must not prevent putting the original input files back.
    try:
        before = _observation(session)
    except Exception as error:
        before = {"observation_error": str(error)}
    session.receipt["before_restore"] = before
    state["before_restore"] = before
    state["phase_before_restore"] = state["phase"]
    session.save(state)
    original = state["original"]["geolocation"]
    location = _read_file(_LOCATION, absent=True)
    location_receipt = _file_receipt(_LOCATION, location)
    original_unchanged = location_receipt == original
    strict = state["phase"] == "prepared"
    if not original_unchanged or strict:
        _owned_file(_LOCATION, _LOCATION_BYTES)
    _owned_file(session.conf_path, _CONF_BYTES, required=strict)
    state["phase"] = "restoring"
    session.save(state)
    if not original_unchanged:
        if original["exists"]:
            _atomic(_LOCATION, base64.b64decode(state["geolocation_backup_base64"], validate=True),
                    original["mode"], original["uid"], original["gid"])
        else:
            _LOCATION.unlink()
            _fsync_directory(_LOCATION.parent)
    if session.conf_path.exists():
        session.conf_path.unlink()
        _fsync_directory(_CONF_DIR)
    if state["conf_directory_created"] and (_CONF_DIR.exists() or _CONF_DIR.is_symlink()):
        _directory(_CONF_DIR)
        _CONF_DIR.rmdir()
        _fsync_directory(_CONF_DIR.parent)
    action = "restart" if state["original"]["service"]["active"] else "stop"
    session.run(["systemctl", action, "geoclue.service"], timeout=15)
    current = _observation(session)
    session.receipt["current"] = current
    proof = {
        "geolocation_bytes_mode_owner_restored": current["geolocation"] == original,
        "configuration_bytes_mode_owner_policy_restored": (
            current["configuration"] == state["original"]["configuration"]),
        "initial_service_active_restored": (
            current["service"]["active"] == state["original"]["service"]["active"]),
        "packaged_service_content_unchanged": all(
            current["service"][key] == state["original"]["service"][key]
            for key in ("unit_file", "dropins", "configured_executable")),
        "owned_configuration_removed": not session.conf_path.exists(),
        "configuration_directory_restored": (
            not _CONF_DIR.exists() if state["conf_directory_created"] else _CONF_DIR.is_dir()),
    }
    session.receipt["restoration_proof"] = proof
    if not all(proof.values()):
        raise RuntimeError("original GeoClue fixture configuration/activity restoration did not match")
    state["phase"] = "restored"
    state["restoration_proof"] = proof
    state["after_restore"] = current
    session.save(state)
    session.state_path.unlink()
    _fsync_directory(_RUN)
    session.receipt.update(restored=True, prepared=False, state_removed=True)
    return session.receipt


def prepare(context):
    """Install only source configuration, preserving actual native app policy."""
    with _Session(context, "fixture-prepare") as session:
        state = None
        backup_started = False
        try:
            state = session.load()
            if state is not None:
                if state["phase"] != "prepared":
                    raise RuntimeError("incomplete GeoClue fixture state requires restoration")
                current = _verify_prepared(session, state)
                session.receipt.update(prepared=True, idempotent=True,
                                       original=state["original"], current=current)
                return session.receipt
            if session.conf_path.exists() or session.conf_path.is_symlink():
                raise RuntimeError("unowned same-nonce GeoClue source configuration already exists")
            original = _observation(session)
            location = _read_file(_LOCATION, absent=True)
            if _file_receipt(_LOCATION, location) != original["geolocation"]:
                raise RuntimeError("original geolocation changed before durable backup")
            if location is not None and location[1].st_nlink != 1:
                raise RuntimeError("original geolocation has multiple hard links")
            state = {
                "schema": 1, "nonce": session.nonce, "uid": context.owner.pw_uid,
                "gid": context.owner.pw_gid, "phase": "backed-up",
                "original": original,
                "geolocation_backup_base64": (
                    base64.b64encode(location[0]).decode("ascii") if location else None),
                "conf_directory_created": not _CONF_DIR.exists(),
            }
            # The fsynced root-only snapshot precedes every configuration effect.
            backup_started = True
            session.save(state)
            if state["conf_directory_created"]:
                _CONF_DIR.mkdir(mode=0o755)
                _CONF_DIR.chmod(0o755)
                os.chown(_CONF_DIR, 0, 0)
                _fsync_directory(_CONF_DIR.parent)
            _directory(_CONF_DIR)
            if (_file_receipt(_LOCATION, _read_file(_LOCATION, absent=True))
                    != original["geolocation"]):
                raise RuntimeError("original geolocation changed after durable backup")
            if session.conf_path.exists() or session.conf_path.is_symlink():
                raise RuntimeError("GeoClue source configuration appeared after durable backup")
            _atomic(_LOCATION, _LOCATION_BYTES, 0o644, 0, 0)
            _atomic(session.conf_path, _CONF_BYTES, 0o644, 0, 0)
            session.run(["systemctl", "restart", "geoclue.service"], timeout=15)
            current = _verify_prepared(session, state)
            state["phase"] = "prepared"
            state["prepared"] = current
            session.save(state)
            session.receipt.update(prepared=True, idempotent=False,
                                   original=original, current=current)
            return session.receipt
        except Exception as error:
            failure = {"error": str(error), "commands": list(session.commands)}
            session.receipt["prepare_failure"] = failure
            # Never tear down an existing same-nonce fixture on failed re-entry.
            if backup_started:
                try:
                    durable = session.load()
                    if durable is not None:
                        state = durable
                        _restore_locked(session, state)
                except Exception as restore_error:
                    session.receipt["cleanup_error"] = str(restore_error)
                    state["last_error"] = str(restore_error)
                    try:
                        session.save(state)
                    except Exception as save_error:
                        session.receipt["state_save_error"] = str(save_error)
            raise FixtureError("GeoClue static fixture prepare failed", session.receipt) from error


def observe(context):
    """Read the original packaged service and effective policy, without activation."""
    with _Session(context, "fixture-observe") as session:
        try:
            state = session.load()
            if state is not None and state["phase"] == "prepared":
                current = _verify_prepared(session, state)
            else:
                current = _observation(session)
            session.receipt.update(prepared=state is not None and state["phase"] == "prepared",
                                   phase=state["phase"] if state else "absent", current=current)
            if state is not None:
                session.receipt["original"] = state["original"]
            return session.receipt
        except Exception as error:
            session.receipt["observation_error"] = str(error)
            raise FixtureError("GeoClue static fixture observation failed",
                               session.receipt) from error


def restore(context):
    """Restore the durable original bytes/credentials and initial service activity."""
    with _Session(context, "fixture-restore") as session:
        state = None
        try:
            state = session.load()
            if state is None:
                raise RuntimeError("no owned durable GeoClue fixture state to restore")
            return _restore_locked(session, state)
        except Exception as error:
            session.receipt["restore_error"] = str(error)
            if state is not None:
                state["last_error"] = str(error)
                try:
                    session.save(state)
                except Exception as save_error:
                    session.receipt["state_save_error"] = str(save_error)
            raise FixtureError("GeoClue static fixture restoration failed",
                               session.receipt) from error