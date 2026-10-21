"""Bounded read-only evidence for the shipped GDM lane.

The launch conclusion is explicitly source-informed, not historical exec tracing.
The collector never changes PAM, session files, PATH, settings or a display manager.
"""
import ctypes
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import pwd
import re
import selectors
import signal
import stat
import subprocess
import time

from published_dm_contract import (ContractError, Inventory, query, validate_report,
                                   validate_session, compare_immutable, GDM_UNIT,
                                   UNIT_PATHS, DROPIN_ROOTS, validate_gdm_config)

PROFILE = "published-gdm"
METHOD = "gdm-51.1-observed-selection-source-informed-launch-v1"
GDM_SOURCE = "8043e3659097c4d39b2ace458cace32d94ac7693"
TUNA_SOURCE = "d28578e590ff85a1616db7d6dcc0d7603bb86b31"
GLIB_SOURCE = "e05063ccc6c8f222465a1080927f4c14349f3de6"
_UNSETTLED_COMMANDS = []  # Keep PID anchors alive if retirement cannot be proved.
# Reviewed installed bytes. Versions alone cannot bind patched builds to this method.
REVIEWED_BYTES = {
    "/usr/bin/gdm": "901efda847dda4efa6b9cdbb3a295ea38eeb36d94e05cf148e0930ab288e51b7",
    "/usr/lib/gdm-session-worker": "8c1ccf574e5bed64c5a9c23818e425b93862f9b4f1280cdf29c49146860aef74",
    "/usr/lib/gdm-wayland-session": "ba7bd77d3ed29ca3f1a0eac008d06bba4e6e4c19ef666ec6496527022bde3cf3",
    "/usr/bin/tuna-session": "f9b49e539cbfb7ce776e2b68f5b1231941bcfcbeba9c6cdf727721d1be72764a",
    "/usr/bin/tuna-compositor": "9d8de57edf1b43fd6ff9933ae53568d5cc04e8e15aae381309e10b2e372fcbd3",
}
# These are observed only to reject executable substitution, never returned.
LAUNCH_OVERRIDES = frozenset(("LD_PRELOAD", "LD_LIBRARY_PATH", "LD_AUDIT", "TUNA_SHELL_BIN", "ROOST_SHELL_BIN"))  # tuna-rename: keep
DATA_DEFAULT = "/usr/local/share:/usr/share"
ENV_KEYS = frozenset(("PATH", "XDG_DATA_DIRS", "XDG_SESSION_ID", "XDG_SESSION_TYPE",
                      "XDG_CURRENT_DESKTOP", "GDMSESSION", "DESKTOP_SESSION",
                      "TUNA_COMPOSITOR_STATE", "TUNA_LOCK_TRACE", "TUNA_POINTER_TRACE")) | LAUNCH_OVERRIDES
BASE = Path("/usr/share/tuna-vm/base-preflight.json")
SESSION = "/usr/share/wayland-sessions/tuna.desktop"
LAUNCHER = "/usr/bin/tuna-session"
SESSION_PROPERTIES = ("Id", "User", "Name", "Type", "Class", "Service", "Seat", "Remote", "VTNr",
                      "Active", "State", "Leader", "Scope")
HEX = re.compile(r"[0-9a-f]{64}\Z")
BOOT = re.compile(r"[0-9a-f]{8}-(?:[0-9a-f]{4}-){3}[0-9a-f]{12}\Z")
PAYLOAD_CHECKPOINTS = frozenset(("collector-initialization", "package-snapshot", "shipped-package-versions",
    "protected-payload-read", "protected-payload-identity", "runtime-unit-overrides", "runtime-dropins",
    "runtime-gdm-policy", "observation-deadline", "observer-transport"))
PAYLOAD_FAILURE_KINDS = frozenset(("guard-mismatch", "invalid-or-unavailable", "timeout",
    "command-unavailable", "output-bound", "retirement-unverified", "transport-or-observer-unavailable"))
ROOT_METADATA_FIELDS = frozenset(("type", "uid", "gid", "mode"))
ROOT_RECORD_FIELDS = ROOT_METADATA_FIELDS | {"resolved_path", "parent_links"}
ROOT_DIFFERING_FIELDS = ROOT_RECORD_FIELDS | {"record_shape"}


class ObservationError(ContractError):
    """Typed fixed command outcome; callers never need to inspect error text."""
    def __init__(self, kind, message):
        super().__init__(message)
        self.kind = kind


def root_metadata_difference(expected, observed):
    """Project only fixed root metadata from records already read for comparison."""
    differing = {key for key in ROOT_RECORD_FIELDS if expected.get(key) != observed.get(key)}
    if (set(expected) - ROOT_RECORD_FIELDS) != (set(observed) - ROOT_RECORD_FIELDS):
        differing.add("record_shape")
    return {"expected": {key: expected.get(key) for key in ROOT_METADATA_FIELDS},
            "observed": {key: observed.get(key) for key in ROOT_METADATA_FIELDS},
            "differing_fields": sorted(differing)}


def validate_root_metadata(value):
    require(type(value) is dict and set(value) == {"expected", "observed", "differing_fields"},
            "invalid root metadata fields")
    for side in ("expected", "observed"):
        record = value[side]
        require(type(record) is dict and set(record) == ROOT_METADATA_FIELDS,
                "invalid root metadata record")
        require(type(record["type"]) is str and record["type"] in {"directory", "file", "symlink", "missing"},
                "invalid root metadata type")
        for key, maximum in (("uid", 2**32 - 1), ("gid", 2**32 - 1), ("mode", 0o7777)):
            require(record[key] is None if record["type"] == "missing" else
                    type(record[key]) is int and 0 <= record[key] <= maximum,
                    "invalid root metadata number")
    require(value["expected"]["type"] == "directory", "invalid baseline root type")
    fields = value["differing_fields"]
    require(type(fields) is list and 0 < len(fields) <= len(ROOT_DIFFERING_FIELDS)
            and all(type(key) is str and key in ROOT_DIFFERING_FIELDS for key in fields)
            and fields == sorted(set(fields)), "invalid root differing fields")
    require(set(fields) & ROOT_METADATA_FIELDS == {key for key in ROOT_METADATA_FIELDS
            if value["expected"][key] != value["observed"][key]}, "inconsistent root metadata difference")


def validate_payload_failure(value):
    require(type(value) is dict and set(value) == {"schema_version", "profile", "immutable_verified", "failure"}
            and type(value["schema_version"]) is int and value["schema_version"] == 1
            and value["profile"] == PROFILE and value["immutable_verified"] is False,
            "invalid payload failure envelope")
    failure = value["failure"]
    required = {"checkpoint", "kind", "protected_record_index", "baseline_files_sha256"}
    require(type(failure) is dict and set(failure) in (required, required | {"root_metadata"}),
            "invalid payload failure fields")
    checkpoint, kind = failure["checkpoint"], failure["kind"]
    require(type(checkpoint) is str and checkpoint in PAYLOAD_CHECKPOINTS
            and type(kind) is str and kind in PAYLOAD_FAILURE_KINDS, "invalid payload failure category")
    index, digest = failure["protected_record_index"], failure["baseline_files_sha256"]
    require(digest is None or (type(digest) is str and HEX.fullmatch(digest)), "invalid payload baseline digest")
    require(index is None or (type(index) is int and 0 <= index < 65536 and digest is not None
            and checkpoint in {"protected-payload-read", "protected-payload-identity"}), "invalid payload record index")
    require(checkpoint not in {"protected-payload-read", "protected-payload-identity"} or index is not None,
            "missing payload record index")
    require(checkpoint in {"collector-initialization", "observer-transport"} or digest is not None,
            "missing payload baseline digest")
    require(kind != "guard-mismatch" or checkpoint in {"shipped-package-versions", "protected-payload-identity",
            "runtime-unit-overrides", "runtime-dropins", "runtime-gdm-policy"}, "invalid payload mismatch category")
    require(checkpoint != "observer-transport" or (kind in {"timeout", "transport-or-observer-unavailable"}
            and index is None and digest is None), "invalid payload transport category")
    if "root_metadata" in failure:
        require(checkpoint == "protected-payload-identity" and kind == "guard-mismatch" and index == 0,
                "unbound root metadata")
        validate_root_metadata(failure["root_metadata"])
    return dict(failure)


def payload_report():
    """Return only fixed failure categories, never exception text or guest paths."""
    diagnostic = {"checkpoint": "collector-initialization", "kind": "invalid-or-unavailable",
                  "protected_record_index": None, "baseline_files_sha256": None}
    try:
        return verify_payload(_diagnostic=diagnostic)
    except Exception as error:
        # A package-owner query can be swallowed by Inventory.owner. Preserve
        # its typed outcome rather than misreporting the resulting None owner.
        command_kind = diagnostic.pop("_command_failure", None)
        if command_kind is not None:
            diagnostic["kind"] = command_kind
        elif isinstance(error, ObservationError):
            diagnostic["kind"] = error.kind
        elif isinstance(error, (TimeoutError, subprocess.TimeoutExpired)):
            diagnostic["kind"] = "timeout"
        elif not isinstance(error, ContractError):
            diagnostic["kind"] = "invalid-or-unavailable"
        result = {"schema_version": 1, "profile": PROFILE, "immutable_verified": False, "failure": diagnostic}
        validate_payload_failure(result)
        return result


def require(value, reason):
    if not value:
        raise ContractError(reason)


def bounded_json(path, limit=4 * 1024 * 1024):
    with path.open("rb") as stream:
        raw = stream.read(limit + 1)
    require(len(raw) <= limit, "evidence exceeds bound")
    return json.loads(raw)


def base_receipt(path=BASE):
    receipt = bounded_json(path)
    require(receipt.get("status") == "static-inventory-passed" and
            all(receipt.get(key) is True for key in ("image_binding_verified",
                "inventory_subject_join_verified", "create_confirmed", "command_groups_settled",
                "container_cleanup_terminal", "container_removed")), "unbound base receipt")
    baseline = receipt.get("inventory")
    validate_report(baseline)
    binding = receipt.get("binding", {})
    require(all(binding.get(key) == baseline["subject"].get(key)
                for key in ("image", "platform_digest")), "base receipt subject differs")
    return baseline


def fixture_verify(image, output=None):
    """Run before and after additive fixtures; the host already bound the image."""
    before = base_receipt()
    require(image == before["subject"]["image"], "fixture FROM image differs from preflight")
    after = Inventory("/", query).collect()
    after["subject"] = before["subject"]
    compare_immutable(before, after)
    if output:
        Path(output).write_text(json.dumps(after, sort_keys=True))
    return {"profile": PROFILE, "immutable_verified": True,
            "subject": before["subject"], "package": "tuna-desktop " + before["packages"]["tuna-desktop"]}


def verify_payload(*, _diagnostic=None):
    """Rehash immutable protected files; runtime /run policy is checked separately.

    Runtime mount points may differ from a quiescent container. The exact persistent
    selection/configuration trees and every shipped package version must still match.
    """
    collector = Collector()
    before = collector.baseline
    def checkpoint(name, index=None, kind="invalid-or-unavailable"):
        if _diagnostic is not None:
            _diagnostic.update(checkpoint=name, kind=kind, protected_record_index=index)
    if _diagnostic is not None:
        _diagnostic["baseline_files_sha256"] = hashlib.sha256(
            json.dumps(before["files"], sort_keys=True, separators=(",", ":")).encode()).hexdigest()
    owner_query_failures = {}
    def package_query(args):
        try:
            return collector.call("/usr/bin/pacman", *args)
        except (ContractError, OSError, subprocess.SubprocessError) as error:
            if _diagnostic is not None:
                kind = (error.kind if isinstance(error, ObservationError) else
                    "timeout" if isinstance(error, (TimeoutError, subprocess.TimeoutExpired)) else "command-unavailable")
                if args == ["-Q"]:
                    _diagnostic["_command_failure"] = kind
                elif len(args) == 3 and args[:2] == ["-Qqo", "--"] and args[2] in before["files"]:
                    owner_query_failures[args[2]] = kind
            raise
    # Collector's ownership observer is seeded with the immutable baseline.
    # Read installed packages into a fresh inventory: reusing that seed makes
    # every existing package look duplicated, and clearing it would erase the
    # aliased baseline before the version comparison.
    observer = Inventory("/", package_query)
    checkpoint("package-snapshot")
    observer.packages_inventory()
    require(len(before["files"]) <= 65536, "protected inventory exceeds diagnostic index bound")
    checkpoint("shipped-package-versions", kind="guard-mismatch")
    require(all(observer.packages.get(k) == v for k, v in before["packages"].items()),
            "shipped package version changed")
    for index, name in enumerate(sorted(before["files"])):
        expected = before["files"][name]
        checkpoint("observation-deadline")
        collector.remaining()
        if name == "/run" or name.startswith("/run/"):
            continue
        checkpoint("protected-payload-read", index)
        if "entries" in expected:
            observer.tree(name)
        actual = observer.record(name)
        checkpoint("protected-payload-identity", index, "guard-mismatch")
        if (_diagnostic is not None and name in owner_query_failures
                and expected.get("owner") is not None and actual.get("owner") is None):
            _diagnostic["_command_failure"] = owner_query_failures[name]
        if _diagnostic is not None and name == "/" and actual != expected:
            _diagnostic["root_metadata"] = root_metadata_difference(expected, actual)
        require(actual == expected, "protected payload identity changed")
    # No boot-generated launch overrides may be hidden by the persistent comparison.
    for root in UNIT_PATHS:
        if root.startswith("/run/"):
            for basename in ("gdm.service", "display-manager.service"):
                checkpoint("runtime-unit-overrides")
                actual = observer.record(root + "/" + basename)
                checkpoint("runtime-unit-overrides", kind="guard-mismatch")
                require(actual["type"] == "missing",
                        "runtime display-manager unit override")
    for root in DROPIN_ROOTS:
        if root.startswith("/run/"):
            checkpoint("runtime-dropins")
            observer.tree(root)
            actual = observer.record(root)
            checkpoint("runtime-dropins", kind="guard-mismatch")
            require(actual["type"] in ("missing", "directory") and
                    not actual.get("entries"), "runtime display-manager drop-in")
    checkpoint("runtime-gdm-policy")
    policy = observer.record("/run/gdm/custom.conf")
    checkpoint("runtime-gdm-policy", kind="guard-mismatch")
    require(policy["type"] == "missing", "runtime GDM policy override")
    checkpoint("observation-deadline")
    collector.remaining()
    return {"profile": PROFILE, "immutable_verified": True,
            "subject": before["subject"], "package": "tuna-desktop " + before["packages"]["tuna-desktop"],
            "protected_file_count": len(before["files"]), "shipped_package_count": len(before["packages"])}


def directory_list(value, default=None):
    require(value is None or type(value) is str, "invalid search environment")
    value = default if value in (None, "") and default is not None else value
    require(type(value) is str and 0 < len(value) <= 4096, "missing search environment")
    paths = value.split(":")
    require(len(paths) <= 32 and all(p.startswith("/") and str(PurePosixPath(p)) == p
            and ".." not in PurePosixPath(p).parts and len(p) < 256 for p in paths),
            "relative, empty or noncanonical search directory")
    return paths


def session_search_roots(environment):
    # Some GDM 51.1 consumers pass type=NULL. Inspect both type candidate sets:
    # with every alternative absent, type order cannot change Tuna selection.
    dirs = directory_list(environment.get("XDG_DATA_DIRS"), DATA_DEFAULT)
    return list(dict.fromkeys(["/usr/share/gdm/greeter/wayland-sessions",
                              *(p + "/wayland-sessions" for p in dirs),
                              "/usr/share/wayland-sessions",
                              *(p + "/xsessions" for p in dirs),
                              "/etc/X11/sessions", "/etc/dm/Sessions",
                              "/usr/share/gdm/BuiltInSessions", "/usr/share/xsessions"]))


def executable_search(observer, environment, baseline):
    paths = directory_list(environment.get("PATH"))
    evidence = []
    chosen = None
    for directory in paths:
        name = directory + "/tuna-session"
        item = observer.record(name)
        # Trust every parent up to the first present candidate, including missing
        # leaf directories: an untrusted user path cannot become an absence proof.
        parent = directory
        while observer.record(parent)["type"] == "missing":
            parent = str(PurePosixPath(parent).parent)
        observer.trusted_ancestors(parent, include_self=True)
        if item["type"] == "missing":
            evidence.append({"path": name, "present": False})
            continue
        resolved = observer.root.resolve(name)[1]
        require(resolved == LAUNCHER, "session launcher is shadowed")
        actual = observer.required(LAUNCHER, "tuna-desktop", executable=True)
        require(actual == baseline["files"][LAUNCHER], "session launcher identity changed")
        evidence.append({"path": name, "present": True, "resolved": resolved,
                         "sha256": actual["sha256"]})
        chosen = resolved
        break
    require(chosen == LAUNCHER, "session launcher does not resolve to shipped executable")
    return evidence


def validate_search_candidates(candidates, environment):
    require(type(candidates) is list and candidates, "missing executable resolution evidence")
    paths = [p + "/tuna-session" for p in directory_list(environment.get("PATH"))]
    require(len(candidates) <= len(paths), "unbounded executable resolution evidence")
    for index, candidate in enumerate(candidates):
        require(type(candidate) is dict and candidate.get("path") == paths[index],
                "executable resolution prefix differs")
        if index + 1 < len(candidates):
            require(candidate == {"path": paths[index], "present": False}, "earlier executable shadow")
        else:
            require(candidate == {"path": paths[index], "present": True, "resolved": LAUNCHER,
                    "sha256": REVIEWED_BYTES[LAUNCHER]}, "unverified final launcher resolution")


def validate_observations(evidence, owner):
    require(owner.get("profile") == PROFILE and type(owner.get("uid")) is int and owner["uid"] > 0,
            "wrong launch profile or owner")
    chain = evidence.get("chain")
    executables = ("/usr/bin/gdm", "/usr/lib/gdm-session-worker",
                   "/usr/lib/gdm-wayland-session", "/usr/bin/tuna-compositor")
    require(type(chain) is list and len(chain) == 4, "missing original launch process chain")
    for index, (process, executable) in enumerate(zip(chain, executables)):
        require(type(process) is dict and process.get("executable") == executable and
                process.get("sha256") == REVIEWED_BYTES[executable] and
                process.get("uid") == (0 if index < 2 else owner["uid"]) and
                all(type(process.get(k)) is int and process[k] > 0 for k in ("pid", "start_ticks", "ppid")),
                "launch process ownership or executable identity differs")
        if index:
            require(process["ppid"] == chain[index - 1]["pid"] and
                    process["start_ticks"] >= chain[index - 1]["start_ticks"], "launch ancestry differs")
    require(chain[-1]["pid"] == owner.get("pid") and chain[-1]["start_ticks"] == owner.get("start_ticks"),
            "launch process is not the original compositor")
    require(evidence.get("helper_command_verified") is True and
            evidence.get("compositor_command_verified") is True, "original command selection unverified")
    for key in ("environment", "gdm_environment", "helper_environment"):
        env = evidence.get(key)
        require(type(env) is dict and set(env) <= ENV_KEYS - LAUNCH_OVERRIDES and
                all(type(value) is str and len(value) <= 4096 for value in env.values()),
                "unreviewed environment evidence")
    env = evidence["environment"]
    require(env.get("XDG_SESSION_TYPE") == "wayland" and env.get("XDG_CURRENT_DESKTOP") == "Tuna:GNOME"
            and env.get("XDG_SESSION_ID") == owner["session_id"] and env.get("GDMSESSION") == "tuna"
            and env.get("DESKTOP_SESSION") == "tuna", "desktop selection environment mismatch")
    require(env.get("TUNA_COMPOSITOR_STATE") == f"/run/user/{owner['uid']}/tuna-vm-state.json"
            and env.get("TUNA_LOCK_TRACE") == "1" and env.get("TUNA_POINTER_TRACE") == "1",
            "telemetry environment mismatch")
    require(env.get("PATH") == evidence.get("user_manager_path"), "child PATH merge mismatch")
    validate_search_candidates(evidence.get("exec_candidates"), evidence["helper_environment"])
    validate_search_candidates(evidence.get("tryexec_candidates"), evidence["gdm_environment"])
    validate_search_candidates(evidence.get("child_candidates"), env)
    expected = [{"path": p + "/tuna.desktop", "present": p + "/tuna.desktop" == SESSION}
                for p in session_search_roots(evidence["gdm_environment"])]
    require(evidence.get("session_candidates") == expected, "session selection roots or shadowing differ")


def validate_launch(evidence, login):
    """Join guest observations with the host's actual QMP/UI authentication receipt."""
    require(type(evidence) is dict and evidence.get("schema_version") == 1 and
            evidence.get("method") == METHOD and evidence.get("gdm_source_commit") == GDM_SOURCE and
            evidence.get("tuna_source_commit") == TUNA_SOURCE and
            evidence.get("glib_source_commit") == GLIB_SOURCE,
            "unsupported launch evidence method")
    require(evidence.get("direct_launcher_exec_observed") is False and
            evidence.get("source_build_reproduced") is False, "launch evidence overclaims observation")
    require(all(login.get(k) is True for k in ("picker_selected", "wrong_password_rejected",
                "user_to_password_challenge_observed", "immutable_verified")),
            "real picker/password recovery proof is missing")
    require(type(login.get("session_identity")) is dict and
            evidence.get("session_identity") == login["session_identity"], "launch session changed")
    require(evidence.get("launch_files_verified") is True and
            evidence.get("session_search_verified") is True and
            evidence.get("executable_search_verified") is True and
            evidence.get("environment_verified") is True and
            evidence.get("process_chain_verified") is True, "launch observation gate missing")
    owner = evidence["session_identity"]
    require(evidence.get("boot_id") == login.get("boot_id"), "launch boot changed")
    require(evidence.get("session_entry", {}).get("path") == SESSION and
            HEX.fullmatch(evidence.get("session_entry", {}).get("sha256", "")) and
            evidence.get("launcher", {}).get("path") == LAUNCHER and
            HEX.fullmatch(evidence.get("launcher", {}).get("sha256", "")), "launch file identity missing")
    require(evidence["launcher"]["sha256"] == REVIEWED_BYTES[LAUNCHER] and
            evidence["session_entry"]["sha256"] ==
                "feed3bd8bce86045926d23f05b9562039daef074f2ce14cf9ea586333f150acc",
            "unreviewed launcher or session entry bytes")
    validate_observations(evidence, owner)
    return {"effective_launch_verified": True, "method": METHOD,
            "direct_launcher_exec_observed": False, "source_build_reproduced": False,
            "gdm_source_commit": GDM_SOURCE, "tuna_source_commit": TUNA_SOURCE, "glib_source_commit": GLIB_SOURCE, "session_identity": owner,
            "observed": ["picker-selected", "password-rejection-and-recovery", "immutable-package-files",
                         "effective-search-environment", "owned-helper-compositor-logind-chain"],
            "inferred": ["GDM used its reviewed Wayland search and spawn implementation",
                         "shipped Tuna launcher replaced itself with the observed compositor"]}


class Collector:
    def __init__(self, uid=None, timeout=12):
        self.poisoned = False
        self.uid = pwd.getpwnam("tuna-test").pw_uid if uid is None else uid
        self.deadline = time.monotonic() + timeout
        self.baseline = base_receipt()
        self.observer = Inventory("/", lambda args: self.call("/usr/bin/pacman", *args))
        self.observer.packages = self.baseline["packages"]
        self.boot_id = Path("/proc/sys/kernel/random/boot_id").read_text().strip()
        require(BOOT.fullmatch(self.boot_id), "invalid boot ID")

    def remaining(self):
        if getattr(self, "poisoned", False):
            raise ObservationError("retirement-unverified", "runtime collector is unsettled")
        budget = self.deadline - time.monotonic()
        if budget <= 0:
            raise ObservationError("timeout", "runtime observation deadline exceeded")
        return budget

    def live_group(self, group):
        members = []
        for path in Path("/proc").iterdir():
            self.remaining()
            if not path.name.isdigit():
                continue
            try:
                with (path / "stat").open("rb") as stream:
                    raw = stream.read(4097)
                require(len(raw) <= 4096, "unbounded process-group metadata")
                fields = raw.rsplit(b")", 1)[1].split()
                if int(fields[2]) == group and fields[0] != b"Z":
                    members.append(int(path.name))
            except (FileNotFoundError, ProcessLookupError):
                continue
        return members

    def call(self, *argv):
        """Cap pipe reads before allocation; reserve command time for retirement.

        Linux waitid(WNOWAIT) anchors the leader PID until killpg and wait. No
        strict wall-clock guarantee is made for Python/filesystem/kernel work.
        """
        budget = self.remaining()
        command_deadline = self.deadline - min(0.25, budget / 4)
        process = subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                stderr=subprocess.PIPE, start_new_session=True,
                env={"PATH": "/usr/bin", "LC_ALL": "C"})
        outputs = {"stdout": bytearray(), "stderr": bytearray()}
        limits = {"stdout": 2 * 1024 * 1024, "stderr": 4096}
        selector = selectors.DefaultSelector()
        try:
            for name in outputs:
                pipe = getattr(process, name)
                os.set_blocking(pipe.fileno(), False)
                selector.register(pipe, selectors.EVENT_READ, name)
            while True:
                remaining = command_deadline - time.monotonic()
                if remaining <= 0:
                    raise ObservationError("timeout", "runtime command deadline exceeded")
                for key, _ in selector.select(min(remaining, 0.05)):
                    name = key.data
                    chunk = os.read(key.fd, min(65536, limits[name] - len(outputs[name]) + 1))
                    if not chunk:
                        selector.unregister(key.fileobj)
                    else:
                        outputs[name].extend(chunk)
                        if len(outputs[name]) > limits[name]:
                            raise ObservationError("output-bound", "runtime command output exceeds bound")
                exited = os.waitid(os.P_PID, process.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT)
                if exited is not None and not selector.get_map():
                    if exited.si_code != os.CLD_EXITED or exited.si_status != 0:
                        raise ObservationError("command-unavailable", "runtime read command failed")
                    break
        finally:
            selector.close()
            # The unreaped leader prevents PID/group-ID reuse, including when
            # it exited while a descendant still held a pipe. Same-privilege
            # root collector can retire runuser children without per-command sudo.
            try:
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                process.stdout.close()
                process.stderr.close()
                while self.live_group(process.pid):
                    require(self.deadline - time.monotonic() > 0.01, "runtime command group unsettled")
                    time.sleep(0.01)
                process.wait(timeout=max(0.001, self.deadline - time.monotonic()))
            except BaseException as error:
                self.poisoned = True
                _UNSETTLED_COMMANDS.append(process)
                if isinstance(error, Exception):
                    raise ObservationError("retirement-unverified", "runtime command group unsettled") from None
                raise
        self.remaining()
        return bytes(outputs["stdout"]).decode().strip()

    def sessions(self):
        result = []
        lines = self.call("/usr/bin/loginctl", "list-sessions", "--no-legend", "--no-pager").splitlines()
        require(len(lines) <= 64, "unbounded logind session list")
        for line in lines:
            sid = line.split()[0]
            require(re.fullmatch(r"[A-Za-z0-9_.-]+", sid), "invalid session ID")
            raw = self.call("/usr/bin/loginctl", "show-session", sid,
                            *("--property=" + key for key in SESSION_PROPERTIES))
            row = dict(item.split("=", 1) for item in raw.splitlines())
            result.append(row)
        return result

    @staticmethod
    def pid_session(pid):
        library = ctypes.CDLL("libsystemd.so.0")
        pointer = ctypes.c_void_p()
        library.sd_pid_get_session.argtypes = [ctypes.c_int, ctypes.POINTER(ctypes.c_void_p)]
        status = library.sd_pid_get_session(pid, ctypes.byref(pointer))
        if status < 0:
            return None
        try:
            return ctypes.string_at(pointer).decode()
        finally:
            libc = ctypes.CDLL(None)
            libc.free.argtypes = [ctypes.c_void_p]
            libc.free(pointer)

    def process(self, pid, environment=False):
        self.remaining()
        path = Path("/proc") / str(pid)
        first = (path / "stat").read_text().rsplit(")", 1)[1].split()
        uid = path.stat().st_uid
        exe = os.readlink(path / "exe")
        require(exe in self.baseline["files"], "process executable outside shipped inventory")
        expected = self.baseline["files"][exe]
        require(expected.get("type") == "file", "process executable is not shipped regular file")
        with (path / "exe").open("rb") as stream:
            info = os.fstat(stream.fileno())
            digest = hashlib.file_digest(stream, "sha256").hexdigest()
        file_info = Path(exe).stat()
        require((info.st_dev, info.st_ino) == (file_info.st_dev, file_info.st_ino) and
                digest == expected["sha256"], "running executable identity differs")
        result = {"pid": pid, "start_ticks": int(first[19]), "ppid": int(first[1]), "uid": uid,
                  "executable": exe, "sha256": digest, "exe_device": info.st_dev,
                  "exe_inode": info.st_ino, "logind_session": self.pid_session(pid)}
        if environment:
            with (path / "environ").open("rb") as stream:
                raw = stream.read(131073)
            require(len(raw) <= 131072 and raw.endswith(b"\0"), "invalid process environment")
            values = {}
            for entry in raw[:-1].split(b"\0"):
                key, _, value = entry.partition(b"=")
                decoded_key = key.decode(errors="replace")
                require(not decoded_key.startswith(("TUNA_", "ROOST_")) or decoded_key in ENV_KEYS,  # tuna-rename: keep
                        "unreviewed application launch environment key")
                require(decoded_key not in LAUNCH_OVERRIDES, "unreviewed launch environment override")
                if decoded_key in ENV_KEYS:
                    key, value = key.decode(), value.decode()
                    require(key not in values and len(value) <= 4096, "duplicate or unbounded environment")
                    values[key] = value
            result["environment"] = values
        second = (path / "stat").read_text().rsplit(")", 1)[1].split()
        after = (path / "exe").stat()
        require((first[19], first[1], uid, info.st_dev, info.st_ino) ==
                (second[19], second[1], path.stat().st_uid, after.st_dev, after.st_ino),
                "process identity changed during observation")
        return result

    def command_is(self, process, arguments):
        with (Path("/proc") / str(process["pid"]) / "cmdline").open("rb") as stream:
            raw = stream.read(4097)
        require(len(raw) <= 4096 and raw.endswith(b"\0"), "unbounded process command metadata")
        actual = raw[:-1].split(b"\0")
        return actual == [value.encode() for value in arguments]

    def find_process(self, uid, executable):
        found = []
        for path in Path("/proc").iterdir():
            if not path.name.isdigit():
                continue
            try:
                if path.stat().st_uid == uid and os.readlink(path / "exe") == executable:
                    found.append(self.process(int(path.name), environment=True))
            except (FileNotFoundError, ProcessLookupError):
                continue
        if executable == "/usr/bin/tuna-compositor":
            original = [p for p in found if self.command_is(p, [executable, "--shell-bin",
                        "/usr/bin/tuna-shell-gtk", "--startup-overview"])]
            require(len(original) == 1, "expected one original selected compositor")
            helpers = [p for p in found if p["ppid"] == original[0]["pid"] and
                       self.command_is(p, [executable, "--internal-pam-conversation"])]
            require(len(found) == 1 + len(helpers), "unreviewed extra compositor process")
            return original[0]
        require(len(found) == 1, "expected one original owned process")
        return found[0]

    def manager(self, allow_starting=False):
        values = dict(line.split("=", 1) for line in self.call("/usr/bin/systemctl", "show",
            "gdm.service", "--property=MainPID", "--property=FragmentPath", "--property=DropInPaths",
            "--property=ActiveState", "--property=SubState").splitlines())
        require(values.get("FragmentPath") == GDM_UNIT and values.get("DropInPaths") == "",
                "active GDM unit identity differs")
        if allow_starting and values.get("ActiveState") in ("inactive", "activating"):
            return None
        require(values.get("ActiveState") == "active" and values.get("SubState") == "running",
                "GDM is not active and running")
        manager = self.process(int(values["MainPID"]), environment=True)
        require(manager["uid"] == 0 and manager["executable"] == "/usr/bin/gdm", "wrong GDM manager")
        return manager

    def active_vt(self):
        value = Path("/sys/class/tty/tty0/active").read_text().strip()
        require(re.fullmatch(r"tty[1-9][0-9]*", value), "missing foreground VT")
        return value

    def greeter(self):
        manager = self.manager(allow_starting=True)
        sessions = self.sessions()
        owned = [row for row in sessions if row.get("User") == str(self.uid)
                 and row.get("Service") == "gdm-password" and row.get("Class") == "user"]
        def candidates(values):
            return [row for row in values if
                row.get("Service") == "gdm-launch-environment" and row.get("Class") == "greeter"
                and row.get("Type") == "wayland" and row.get("Remote") == "no"
                and row.get("Seat") == "seat0" and row.get("Active") == "yes" and
                row.get("State") == "active"]
        rows = candidates(sessions)
        require(len(rows) <= 1, "ambiguous foreground GDM greeter")
        if manager is None or not rows:
            return {"schema_version": 1, "profile": PROFILE, "ready": False,
                    "reason": "manager-starting" if manager is None else "no-active-greeter",
                    "boot_id": self.boot_id, "desktop_session_present": bool(owned)}
        row = rows[0]
        from published_gdm_greeter import account, join_greeter
        identity = account(self, row)
        gdm_uid = identity[1]
        process = self.find_process(gdm_uid, "/usr/bin/gnome-shell")
        process = join_greeter(self, process, row)
        vt = self.active_vt()
        require(vt == "tty" + row["VTNr"], "greeter is not foreground")
        final_sessions = self.sessions()
        require(candidates(final_sessions) == rows and
                [r for r in final_sessions if r.get("User") == str(self.uid)
                 and r.get("Service") == "gdm-password" and r.get("Class") == "user"] == owned
                and self.manager() == manager and self.active_vt() == vt
                and account(self, row) == identity, "greeter session or manager changed")
        return {"schema_version": 1, "profile": PROFILE, "ready": True, "boot_id": self.boot_id,
                "desktop_session_present": bool(owned), "greeter": process, "active_vt": vt}

    def desktop(self):
        manager = self.manager()
        sessions = self.sessions()
        rows = [row for row in sessions if row.get("User") == str(self.uid)
                and row.get("Service") == "gdm-password" and row.get("Class") == "user"]
        require(len(rows) == 1, "one password-authenticated GDM session required")
        compositor = self.find_process(self.uid, "/usr/bin/tuna-compositor")
        identity = validate_session(PROFILE, rows[0], compositor, self.uid, require_active=False)
        occupied = sorted({int(row["VTNr"]) for row in sessions if row.get("Seat") == "seat0"
                           and re.fullmatch(r"[1-9][0-9]*", row.get("VTNr", ""))})
        return {"profile": PROFILE, "boot_id": self.boot_id, "session_uid": self.uid,
                "session": rows[0], "compositor_identity": compositor, "active_vt": self.active_vt(),
                "occupied_vts": occupied, "launch_evidence": self.launch(manager, compositor, identity)}

    def recheck_processes(self, processes):
        for process in processes:
            require(self.process(process["pid"], environment="environment" in process) == process,
                    "launch process identity or environment changed")

    def launch(self, manager, compositor, identity):
        require(self.baseline["packages"].get("gdm") == "51.1-1" and
                self.baseline["packages"].get("tuna-desktop") == "1.0.0beta.1-1" and
                self.baseline["packages"].get("glib2") == "2.90.1-1",
                "unreviewed package source version")
        require(all(self.baseline["files"].get(path, {}).get("sha256") == digest
                    for path, digest in REVIEWED_BYTES.items()), "unreviewed launch executable bytes")
        helper = self.process(compositor["ppid"], environment=True)
        require(helper["uid"] == self.uid and helper["executable"] == "/usr/lib/gdm-wayland-session"
                and helper["logind_session"] == identity["session_id"], "original GDM helper chain differs")
        worker = self.process(helper["ppid"])
        require(worker["uid"] == 0 and worker["executable"] == "/usr/lib/gdm-session-worker"
                and worker["ppid"] == manager["pid"], "original GDM worker chain differs")
        require(all(not LAUNCH_OVERRIDES.intersection(p["environment"])
                    for p in (manager, helper, compositor)), "unreviewed launch environment override")
        require(self.command_is(helper, ["/usr/lib/gdm-wayland-session", "tuna-session"]),
                "GDM helper command is not the selected shipped entry")
        require(self.command_is(compositor, ["/usr/bin/tuna-compositor", "--shell-bin",
                    "/usr/bin/tuna-shell-gtk", "--startup-overview"]),
                "compositor command differs from reviewed release launcher")
        roots = session_search_roots(manager["environment"])
        candidates = []
        for root in roots:
            parent = root
            while self.observer.record(parent)["type"] == "missing":
                parent = str(PurePosixPath(parent).parent)
            self.observer.trusted_ancestors(parent, include_self=True)
            name = root + "/tuna.desktop"
            record = self.observer.record(name)
            require(name == SESSION or record["type"] == "missing", "Tuna session entry is shadowed")
            if name == SESSION:
                require(record == self.baseline["files"][SESSION], "session entry identity changed")
            candidates.append({"path": name, "present": record["type"] != "missing"})
        env = compositor["environment"]
        require(env.get("XDG_SESSION_TYPE") == "wayland" and env.get("XDG_CURRENT_DESKTOP") == "Tuna:GNOME"
                and env.get("XDG_SESSION_ID") == identity["session_id"]
                and env.get("GDMSESSION") == "tuna" and env.get("DESKTOP_SESSION") == "tuna",
                "effective desktop selection environment differs")
        require(env.get("TUNA_COMPOSITOR_STATE") == f"/run/user/{self.uid}/tuna-vm-state.json"
                and env.get("TUNA_LOCK_TRACE") == "1" and env.get("TUNA_POINTER_TRACE") == "1",
                "existing telemetry keys did not reach original compositor")
        # Observe the user manager's actual PATH, not a fixture-normalized value.
        raw = self.call("/usr/bin/runuser", "-u", "tuna-test", "--", "/usr/bin/env",
                f"XDG_RUNTIME_DIR=/run/user/{self.uid}",
                f"DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/{self.uid}/bus",
                "/usr/bin/systemctl", "--user", "show-environment")
        manager_path = [line[5:] for line in raw.splitlines() if line.startswith("PATH=")]
        require(len(manager_path) <= 1, "ambiguous user-manager PATH")
        expected_path = manager_path[0] if manager_path else helper["environment"].get("PATH")
        require(env.get("PATH") == expected_path, "effective compositor PATH differs from GDM merge")
        # GSubprocessFlags.NONE searches the helper's PATH; imported PATH is
        # only the child's environment. SEARCH_PATH_FROM_ENVP was not requested.
        search = executable_search(self.observer, helper["environment"], self.baseline)
        child_search = executable_search(self.observer, env, self.baseline)
        # TryExec is checked by the manager using its own environment as well.
        try_search = executable_search(self.observer, manager["environment"], self.baseline)
        self.recheck_processes((manager, worker, helper, compositor))
        require(self.command_is(helper, ["/usr/lib/gdm-wayland-session", "tuna-session"]) and
                self.command_is(compositor, ["/usr/bin/tuna-compositor", "--shell-bin",
                    "/usr/bin/tuna-shell-gtk", "--startup-overview"]), "launch command changed")
        fresh = Inventory("/", lambda args: self.call("/usr/bin/pacman", *args))
        fresh.packages = self.baseline["packages"]
        require(executable_search(fresh, helper["environment"], self.baseline) == search and
                executable_search(fresh, env, self.baseline) == child_search and
                executable_search(fresh, manager["environment"], self.baseline) == try_search,
                "executable search changed during observation")
        for candidate in candidates:
            name = candidate["path"]
            record = fresh.record(name)
            require(record == self.observer.record(name), "session search changed during observation")
            parent = str(PurePosixPath(name).parent)
            while fresh.record(parent)["type"] == "missing":
                parent = str(PurePosixPath(parent).parent)
            fresh.trusted_ancestors(parent, include_self=True)
        return {"schema_version": 1, "method": METHOD, "gdm_source_commit": GDM_SOURCE, "tuna_source_commit": TUNA_SOURCE, "glib_source_commit": GLIB_SOURCE,
                "source_build_reproduced": False, "direct_launcher_exec_observed": False,
                "boot_id": self.boot_id, "session_identity": identity,
                "launch_files_verified": True, "session_search_verified": True,
                "executable_search_verified": True, "environment_verified": True,
                "process_chain_verified": True, "helper_command_verified": True, "compositor_command_verified": True,
                "session_entry": {"path": SESSION, "sha256": self.baseline["files"][SESSION]["sha256"]},
                "launcher": {"path": LAUNCHER, "sha256": self.baseline["files"][LAUNCHER]["sha256"]},
                "session_candidates": candidates, "exec_candidates": search, "tryexec_candidates": try_search, "child_candidates": child_search,
                "environment": env, "gdm_environment": manager["environment"],
                "helper_environment": helper["environment"], "user_manager_path": expected_path,
                "chain": [{k: p[k] for k in ("pid", "start_ticks", "ppid", "uid", "executable", "sha256")}
                          for p in (manager, worker, helper, compositor)]}

    def ui(self):
        before = self.greeter()
        principal = before["greeter"]
        name = principal["greeter_join"]["account"]["name"]
        raw = self.call("/usr/bin/runuser", "-u", name, "--", "/usr/bin/env",
                f"XDG_RUNTIME_DIR=/run/user/{principal['uid']}",
                f"DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/{principal['uid']}/bus",
                "/usr/bin/python3", "-B", "/usr/libexec/tuna-vm-gdm-ui",
                "--pid", str(principal["pid"]), "--start-ticks", str(principal["start_ticks"]),
                "--session-id", principal["logind_session"], "--uid", str(principal["uid"]))
        value = json.loads(raw)
        observed = value.get("principal", {})
        require(value.get("available") is True and value.get("bus_owner_verified") is True and
                value.get("input_value_observed") is False and
                all(observed.get(k) == principal[k] for k in
                    ("pid", "uid", "start_ticks", "executable", "exe_device", "exe_inode")) and
                observed.get("session_id") == principal["logind_session"], "greeter UI principal differs")
        after = self.greeter()
        require(after == before, "greeter changed across UI observation")
        return {"schema_version": 1, "profile": PROFILE, "boot_id": self.boot_id,
                "bus_owner_verified": True, "input_value_observed": False,
                "greeter": principal, "active_vt": before["active_vt"], "controls": value["controls"]}

    def original(self, value):
        require(type(value) is dict and value.get("boot_id") == self.boot_id,
                "original boot identity changed")
        identity = value.get("session_identity", {})
        require(type(identity) is dict and identity.get("profile") == PROFILE and
                identity.get("uid") == self.uid and
                re.fullmatch(r"[A-Za-z0-9_.-]+", identity.get("session_id", "")) and
                all(type(identity.get(k)) is int and identity[k] > 0 for k in ("pid", "start_ticks")),
                "invalid original session identity")
        return identity

    def logout(self, value):
        original = self.original(value)
        actual = self.desktop()["launch_evidence"]["session_identity"]
        require(actual == original, "logout target is not the original owned session")
        self.call("/usr/bin/loginctl", "terminate-session", original["session_id"])
        return {"profile": PROFILE, "boot_id": self.boot_id, "requested": "terminate original session"}

    def departed(self, value):
        original = self.original(value)
        gone = not any(row.get("Id") == original["session_id"] for row in self.sessions())
        path = Path("/proc") / str(original["pid"])
        try:
            first = (path / "stat").read_text().rsplit(")", 1)[1].split()
            compositor_gone = int(first[19]) != original["start_ticks"]
            second = (path / "stat").read_text().rsplit(")", 1)[1].split()
            require(first[19] == second[19], "PID changed during departure observation")
        except (FileNotFoundError, ProcessLookupError):
            compositor_gone = True
        return {"profile": PROFILE, "boot_id": self.boot_id, "original_session_gone": gone,
                "original_compositor_gone": compositor_gone}


def main():
    import argparse
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fixture-image", required=True)
    parser.add_argument("--snapshot", action="store_true")
    args = parser.parse_args()
    proof = fixture_verify(args.fixture_image,
        "/usr/share/tuna-vm/fixture-inventory.json" if args.snapshot else None)
    print(json.dumps(proof, sort_keys=True))


if __name__ == "__main__":
    main()
