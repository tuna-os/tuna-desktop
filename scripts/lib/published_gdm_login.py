"""Observe the shipped GDM picker and authenticate through physical QMP input.

No display-manager APIs, session presets, PAM changes or launch wrappers are
used here. Observers must bind their evidence to the actual guest principals.
Screenshots supplement, and never replace, semantic selection observations.
"""
import math
import re
import time

from published_dm_contract import ContractError, validate_session
from published_gdm_runtime import validate_launch


PROFILE = "published-gdm"
KINDS = frozenset({"fixture-user", "not-listed", "username-entry", "password-entry",
                   "session-menu", "tuna-session", "sign-in", "cancel", "auth-error"})
BOOT_ID = re.compile(r"[0-9a-f]{8}-(?:[0-9a-f]{4}-){3}[0-9a-f]{12}\Z")
SESSION_ID = re.compile(r"[A-Za-z0-9_.-]+\Z")
IMAGE = re.compile(r"ghcr\.io/tuna-os/marlin@sha256:[0-9a-f]{64}\Z")
DIGEST = re.compile(r"sha256:[0-9a-f]{64}\Z")
IDENTITY_KEYS = ("pid", "start_ticks", "uid", "executable", "logind_session",
                 "direct_logind_session", "logind_join_method")


class LoginError(RuntimeError):
    """A bounded, public failure reason and safe partial observations."""

    def __init__(self, reason, receipt):
        super().__init__(reason)
        self.receipt = receipt


class _Rejected(ValueError):
    pass


def _require(condition, reason):
    if not condition:
        raise _Rejected(reason)


def _positive(value):
    return type(value) is int and value > 0


def _boot(value):
    _require(isinstance(value, str) and BOOT_ID.fullmatch(value), "invalid boot identity")
    return value


def _principal(value):
    _require(type(value) is dict and all(key in value for key in IDENTITY_KEYS),
             "missing greeter principal")
    result = {key: value[key] for key in IDENTITY_KEYS}
    _require(all(_positive(result[key]) for key in ("pid", "start_ticks", "uid"))
             and result["executable"] == "/usr/bin/gnome-shell"
             and isinstance(result["logind_session"], str)
             and SESSION_ID.fullmatch(result["logind_session"]), "invalid greeter principal")
    _require(result["logind_join_method"] == "logind-user-display-and-gdm-user-unit"
             and result["direct_logind_session"] in (None, result["logind_session"]),
             "unverified or contradictory greeter session join")
    return result


def _active_vt(value):
    _require(isinstance(value, str) and re.fullmatch(r"tty[1-9][0-9]*", value),
             "invalid foreground VT")
    return value


def validate_ui(value, *, width=1280, height=800):
    """Project only bounded public controls; never retain arbitrary guest text."""
    _require(type(value) is dict and type(value.get("schema_version")) is int
             and value["schema_version"] == 1 and value.get("profile") == PROFILE,
             "invalid greeter UI schema")
    _require(value.get("bus_owner_verified") is True
             and value.get("input_value_observed") is False, "unverified or unsafe greeter UI")
    identity = {"boot_id": _boot(value.get("boot_id")),
                "greeter": _principal(value.get("greeter")),
                "active_vt": _active_vt(value.get("active_vt"))}
    controls = value.get("controls")
    _require(type(controls) is list and len(controls) <= 64, "unbounded or absent public controls")
    safe = []
    for control in controls:
        _require(type(control) is dict and set(control) ==
                 {"kind", "rect", "showing", "enabled", "focused", "selected"},
                 "invalid public control schema")
        _require(type(control["kind"]) is str and control["kind"] in KINDS,
                 "unknown public control kind")
        _require(all(type(control[key]) is bool for key in
                     ("showing", "enabled", "focused", "selected")), "invalid public control state")
        rect = control["rect"]
        _require(type(rect) is list and len(rect) == 4 and all(type(v) is int for v in rect),
                 "invalid public control rectangle")
        x, y, w, h = rect
        _require(x >= 0 and y >= 0 and w > 0 and h > 0 and x + w <= width and y + h <= height,
                 "public control outside observed display")
        safe.append({**control, "rect": list(rect)})
    for kind in KINDS:
        _require(sum(c["kind"] == kind and c["showing"] for c in safe) <= 1,
                 "ambiguous visible public control")
    return {**identity, "controls": safe}


def _control(ui, kind, enabled=True):
    matches = [c for c in ui["controls"] if c["kind"] == kind and c["showing"]
               and (c["enabled"] or not enabled)]
    return matches[0] if len(matches) == 1 else None


def validate_owner(value, boot_id, previous=None, *, require_active=True, away_vt=None):
    """Require the guest's independent kernel/logind join and fresh relogin."""
    _require(type(value) is dict and value.get("boot_id") == boot_id, "desktop boot identity changed")
    _require(value.get("profile") == PROFILE, "desktop backend differs")
    try:
        owner = validate_session(PROFILE, value.get("session"), value.get("compositor_identity"),
                                 value.get("session_uid"), require_active=require_active)
    except ContractError:
        raise _Rejected("desktop session identity rejected") from None
    if require_active:
        _require(away_vt is None, "foreground observation cannot specify an away VT")
        _require(value.get("active_vt") == "tty" + str(owner["vt"]), "desktop is not on its foreground VT")
    else:
        _require(type(away_vt) is int and 1 <= away_vt <= 12 and away_vt != owner["vt"],
                 "inactive observation requires an explicit distinct target VT")
        _require(value["session"].get("Active") == "no" and value["session"].get("State") == "online"
                 and value.get("active_vt") == "tty" + str(away_vt),
                 "original session is not inactive on the observed target VT")
    if previous is not None:
        _require(type(previous) is dict and previous.get("boot_id") == boot_id,
                 "relogin boot identity differs")
        old = previous.get("session_identity")
        _require(type(old) is dict and old.get("profile") == PROFILE
                 and isinstance(old.get("session_id"), str)
                 and all(_positive(old.get(k)) for k in ("pid", "start_ticks", "uid")),
                 "missing original login identity")
        _require(owner["uid"] == old["uid"] and owner["session_id"] != old["session_id"]
                 and (owner["pid"], owner["start_ticks"]) != (old["pid"], old["start_ticks"]),
                 "relogin did not create a fresh original session")
    return owner


def published_subject(proof, expected_image=None):
    _require(type(proof) is dict and proof.get("profile") == PROFILE
             and proof.get("immutable_verified") is True, "immutable published payload unverified")
    subject = proof.get("subject")
    _require(type(subject) is dict and isinstance(subject.get("image"), str)
             and IMAGE.fullmatch(subject["image"])
             and isinstance(subject.get("platform_digest"), str)
             and DIGEST.fullmatch(subject["platform_digest"]), "published payload subject missing")
    _require(expected_image is None or subject["image"] == expected_image,
             "published payload differs from host image")
    return {"image": subject["image"], "platform_digest": subject["platform_digest"]}


def login(qmp, observe, paint, *, timeout=120, previous=None, monotonic=time.monotonic,
          sleep=time.sleep, receipt=None, width=1280, height=800, expected_image=None):
    """Return sanitized evidence, or raise LoginError carrying partial evidence.

    observe(action, timeout=remaining) must execute the fixed guest action.
    paint(stage, timeout=remaining) must retain an actual frame and return
    {"painted": True}. Both callbacks must obey their supplied remaining budget.
    inventory may return {"ready": False} or raise RuntimeError/OSError while
    the authenticated desktop starts; malformed successful evidence fails.
    previous is a prior successful return value for same-boot fresh relogin.
    All phases share one absolute deadline. Public fixture values are fixed;
    no credentials, arbitrary guest strings or exception messages are retained.
    """
    result = {"schema_version": 1, "profile": PROFILE, "pass": False,
              "stage": "arguments", "observations": [], "painted_stages": [],
              "wrong_password_rejected": False, "picker_selected": False,
              "direct_launcher_exec_observed": False}
    if receipt is not None:
        if type(receipt) is not dict:
            raise LoginError("receipt must be a dictionary", result)
        receipt.clear()
        result = receipt
        result.update({"schema_version": 1, "profile": PROFILE, "pass": False,
                       "stage": "arguments", "observations": [], "painted_stages": [],
                       "wrong_password_rejected": False, "picker_selected": False,
                       "direct_launcher_exec_observed": False})
    try:
        _require(type(timeout) in (int, float) and math.isfinite(timeout) and 0 < timeout <= 600,
                 "invalid login deadline")
        _require(callable(observe) and callable(paint), "required observation callbacks missing")
        _require(expected_image is None or (isinstance(expected_image, str) and IMAGE.fullmatch(expected_image)),
                 "host published image is not an immutable reference")
        _require(type(width) is int and type(height) is int and 0 < width <= 16384
                 and 0 < height <= 16384, "invalid display geometry")
        deadline = monotonic() + timeout
        bound = None

        def remaining():
            budget = deadline - monotonic()
            _require(budget > 0, "login deadline expired")
            return budget

        def read(action):
            value = observe(action, timeout=remaining())
            remaining()
            return value

        def pause():
            sleep(min(0.2, remaining()))
            remaining()

        def input_call(method, *args):
            budget = remaining()
            sock = getattr(qmp, "sock", None)
            original_timeout = sock.gettimeout() if sock is not None else None
            try:
                if sock is not None:
                    sock.settimeout(min(original_timeout, budget) if original_timeout is not None else budget)
                getattr(qmp, method)(*args)
                remaining()
            finally:
                if sock is not None:
                    sock.settimeout(original_timeout)

        def public_fixture(text):
            for character in text:
                input_call("keys", "minus" if character == "-" else character)

        def snapshot(stage):
            result["stage"] = stage
            painted = paint(stage, timeout=remaining())
            remaining()
            _require(type(painted) is dict and painted.get("painted") is True,
                     "actual retained greeter paint missing")
            result["painted_stages"].append(stage)

        def greeter():
            value = read("gdm-greeter")
            _require(type(value) is dict and value.get("profile") == PROFILE
                     and type(value.get("schema_version")) is int and value["schema_version"] == 1,
                     "invalid greeter readiness schema")
            _require(value.get("desktop_session_present") is False,
                     "desktop session appeared before accepted password")
            if value.get("ready") is not True:
                return None
            current = {"boot_id": _boot(value.get("boot_id")),
                       "greeter": _principal(value.get("greeter")),
                       "active_vt": _active_vt(value.get("active_vt"))}
            if bound is not None:
                _require(current == bound, "original foreground greeter changed")
            return current

        def ui():
            ready = greeter()
            if ready is None:
                return None
            value = validate_ui(read("gdm-ui"), width=width, height=height)
            _require({key: value[key] for key in bound} == bound,
                     "UI does not belong to original foreground greeter")
            return value

        def wait_ui(predicate, stage):
            result["stage"] = stage
            while True:
                value = ui()
                if value is not None and predicate(value):
                    # Only retain the bounded successful public phase, not every poll.
                    result["observations"].append({"stage": stage, **value})
                    return value
                pause()

        def click(control):
            _require(control is not None, "required enabled public control missing")
            x, y, w, h = control["rect"]
            input_call("click", x + w // 2, y + h // 2)

        def focus_password(stage):
            value = wait_ui(lambda v: _control(v, "password-entry"), stage)
            click(_control(value, "password-entry"))
            return wait_ui(lambda v: _control(v, "password-entry")
                           and _control(v, "password-entry")["focused"], stage + "-focused")

        def open_picker(stage):
            value = wait_ui(lambda v: _control(v, "session-menu"), stage + "-button")
            click(_control(value, "session-menu"))
            return wait_ui(lambda v: _control(v, "tuna-session"), stage)

        def close_picker(stage):
            input_call("keys", "esc")
            return wait_ui(lambda v: _control(v, "password-entry")
                           and _control(v, "tuna-session", enabled=False) is None, stage)

        result["stage"] = "greeter-ready"
        while bound is None:
            try:
                bound = greeter()
            except (OSError, RuntimeError):
                # A completed read-only guest probe can precede the greeter.
                # No physical input has happened, no transport is replaced,
                # and malformed returned evidence is never treated as ready.
                remaining()
            if bound is None:
                pause()
        result["boot_id"] = bound["boot_id"]
        if previous is not None:
            _require(type(previous) is dict and previous.get("pass") is True
                     and previous.get("boot_id") == bound["boot_id"], "invalid prior successful login")
        proof = read("published")
        result["subject"] = published_subject(proof, expected_image)
        if previous is not None:
            _require(previous.get("subject") == result["subject"], "relogin published payload subject changed")
        result["immutable_verified"] = True
        snapshot("greeter-before-login")
        first = wait_ui(lambda v: any(_control(v, k) for k in
                        ("fixture-user", "not-listed", "username-entry", "password-entry")), "user-choice")
        if _control(first, "password-entry"):
            click(_control(first, "cancel"))
            first = wait_ui(lambda v: not _control(v, "password-entry") and any(_control(v, k)
                            for k in ("fixture-user", "not-listed", "username-entry")), "user-choice-reset")
        if _control(first, "fixture-user"):
            click(_control(first, "fixture-user"))
        else:
            if _control(first, "not-listed"):
                click(_control(first, "not-listed"))
            username = wait_ui(lambda v: _control(v, "username-entry"), "username-challenge")
            click(_control(username, "username-entry"))
            wait_ui(lambda v: _control(v, "username-entry")
                    and _control(v, "username-entry")["focused"], "username-focused")
            input_call("keys", "ctrl", "a")
            public_fixture("tuna-test")
            input_call("keys", "ret")
        wait_ui(lambda v: _control(v, "password-entry")
                and not _control(v, "username-entry"), "password-challenge")
        result["user_to_password_challenge_observed"] = True
        snapshot("password-challenge")
        opened = open_picker("picker-open")
        snapshot("picker-open")
        click(_control(opened, "tuna-session"))
        wait_ui(lambda v: _control(v, "password-entry")
                and _control(v, "tuna-session", enabled=False) is None, "picker-selection-closed")
        selected = open_picker("picker-selected")
        _require(_control(selected, "tuna-session")["selected"] is True,
                 "Tuna Desktop selection was not observed")
        result["picker_selected"] = True
        snapshot("picker-selected")
        close_picker("picker-dismissed")
        before_wrong = focus_password("negative-password")
        _require(_control(before_wrong, "auth-error", enabled=False) is None,
                 "stale authentication error before negative attempt")
        input_call("keys", "ctrl", "a")
        public_fixture("incorrect")
        input_call("keys", "ret")
        wait_ui(lambda v: _control(v, "auth-error", enabled=False)
                and _control(v, "password-entry"), "wrong-password-rejected")
        result["wrong_password_rejected"] = True
        snapshot("wrong-password-rejected")
        selected = open_picker("picker-after-rejection")
        _require(_control(selected, "tuna-session")["selected"] is True,
                 "Tuna Desktop selection changed after rejection")
        close_picker("picker-after-rejection-dismissed")
        focus_password("correct-password")
        input_call("keys", "ctrl", "a")
        public_fixture("tuna")
        input_call("keys", "ret")
        result["stage"] = "desktop-identity"
        while True:
            try:
                value = read("inventory")
            except (OSError, RuntimeError):
                remaining()
                pause()
                continue
            if type(value) is dict and value == {"ready": False}:
                pause()
                continue
            owner = validate_owner(value, bound["boot_id"], previous)
            break
        result["session_identity"] = owner
        # Join the independently observed effective search/process chain with
        # actual UI and password evidence. Historical launcher exec remains an
        # explicitly source-informed inference, never a fabricated observation.
        try:
            result["launch_verdict"] = validate_launch(value.get("launch_evidence"), result)
        except ContractError:
            raise _Rejected("effective published launch evidence rejected") from None
        result["effective_launch_verified"] = result["launch_verdict"]["effective_launch_verified"]
        snapshot("authenticated-desktop")
        final_proof = read("published")
        _require(published_subject(final_proof, expected_image) == result["subject"],
                 "published payload changed across authentication")
        remaining()
        result["stage"] = "complete"
        result["pass"] = True
        return result
    except Exception as error:
        reason = str(error) if isinstance(error, _Rejected) else "login observation or input failed"
        result["failure"] = reason
        result["error_type"] = type(error).__name__ if type(error).__name__ in {
            "_Rejected", "RuntimeError", "ValueError", "OSError", "TimeoutError", "KeyError",
            "TypeError", "ConnectionError", "FileNotFoundError", "PermissionError"} else "ObserverError"
        raise LoginError(reason, result) from None
