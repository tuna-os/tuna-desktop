"""Record-free fresh-account GDM journey (tunaOS#4092) for scripts/tuna-vm-lane.

The default lane logs tuna-test in, an account whose AccountsService record
the image writes session-less (the GDM FallbackSession case). This journey
covers the other path: accounts an installer makes with useradd, which have
NO AccountsService record, NO Session preselection and NO autologin before
their first login. packaging/marlin/vm-lane/tuna-vm-fresh-accounts creates

  tunastd     standard account
  tunaadmin   administrator (wheel)
  tunaseeded  negative control: its record is pre-seeded before GDM starts

and the journey drives real GDM password logins for tunastd and tunaadmin:
first login, logout, second login, logout, reboot, login, logout. Each login
must start the Tuna Desktop compositor and no GNOME session; AccountsService
must have written each record after the first login; the administrator must
authenticate through sudo and the standard account must be refused.
"""
import configparser
import json
import os
import re
import stat
import time

PASSWORD = "tuna"  # Public fixture value (tuna-vm-fresh-accounts).
STANDARD, ADMIN, SEEDED = "tunastd", "tunaadmin", "tunaseeded"
JOURNEY_USERS = (STANDARD, ADMIN)
ADMIN_GROUP = "wheel"
COMPOSITOR_COMM = "tuna-compositor"
GNOME_DESKTOP = ("gnome-shell", "gnome-session-b", "gnome-session-s")
LOGIN_SERVICE = "gdm-password"
AUTOLOGIN_KEYS = ("automaticloginenable", "timedloginenable")
# gnome-shell js/gdm/loginDialog.js: the user list holds key focus and Tab
# reaches its "Not listed?" button, which opens the user-name entry. A
# record-free account need not be in the list, so the journey always names
# the account. A failed strategy never types a password for another account:
# the password follows the account name in the same strategy.
SELECTIONS = (("tab",), ("tab", "tab"))

NATIVE_CANDIDATE_ENV = "TUNA_VM_GDM_NATIVE_CANDIDATE"
NATIVE_SOURCE_ENV = "TUNA_VM_GDM_NATIVE_SOURCE_SHA"
NATIVE_SOURCE_SHA = "7b52bdf34be22bc60fef62b157be64b589b13f6a"
NATIVE_VERSION = "51.1.1-1.1"
NATIVE_MANIFEST_BOUND = 16 * 1024
NATIVE_MEMBERS = {"/usr/bin/gdm": "gdm", "/usr/lib/gdm-session-worker": "gdm",
                  "/usr/lib/libgdm.so.1.0.0": "libgdm"}


def _exact_fields(value, fields):
    if type(value) is not dict or set(value) != set(fields):
        raise ValueError("invalid candidate fields")


def _unique_fields(pairs):
    value = {}
    for key, item in pairs:
        if key in value:
            raise ValueError("duplicate candidate field")
        value[key] = item
    return value


def _invalid_constant(value):
    raise ValueError("invalid candidate JSON constant")


def _file_identity(info):
    return (info.st_dev, info.st_ino, info.st_mode, info.st_size, info.st_mtime_ns, info.st_ctime_ns)


def native_candidate():
    """Load only an explicitly paired, pinned CI identity; never package paths
    or arbitrary environment/guest data. The manifest is a bounded stable
    regular file, not a symlink, and every nested field is allowlisted."""
    path, expected = os.environ.get(NATIVE_CANDIDATE_ENV), os.environ.get(NATIVE_SOURCE_ENV)
    if path is None and expected is None:
        return None
    if not path or expected != NATIVE_SOURCE_SHA:
        raise ValueError("candidate manifest and pinned expected source are required together")
    before = os.stat(path, follow_symlinks=False)
    if not stat.S_ISREG(before.st_mode) or not 0 < before.st_size <= NATIVE_MANIFEST_BOUND:
        raise ValueError("candidate manifest must be a bounded regular file")
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC)
    with os.fdopen(fd, "rb") as stream:
        opened = os.fstat(stream.fileno())
        if _file_identity(before) != _file_identity(opened):
            raise ValueError("candidate manifest changed before reading")
        data = stream.read(NATIVE_MANIFEST_BOUND + 1)
        after = os.fstat(stream.fileno())
    if (len(data) != before.st_size or _file_identity(before) != _file_identity(after)
            or _file_identity(before) != _file_identity(os.stat(path, follow_symlinks=False))):
        raise ValueError("candidate manifest changed while reading")
    value = json.loads(data.decode("utf-8"), object_pairs_hook=_unique_fields,
                       parse_constant=_invalid_constant)
    _exact_fields(value, ("schema", "source_repository", "source_commit", "architecture", "packages", "members"))
    if (type(value["schema"]) is not int or value["schema"] != 1
            or value["source_repository"] != "tuna-os/tunaos-packages"
            or value["source_commit"] != expected or value["architecture"] != "x86_64"):
        raise ValueError("candidate source, schema or architecture does not match")
    _exact_fields(value["packages"], ("gdm", "libgdm"))
    for name, package in value["packages"].items():
        _exact_fields(package, ("file", "version", "sha256"))
        if (package["file"] != f"{name}-{NATIVE_VERSION}-x86_64.pkg.tar.zst"
                or package["version"] != NATIVE_VERSION
                or type(package["sha256"]) is not str or re.fullmatch("[0-9a-f]{64}", package["sha256"]) is None):
            raise ValueError("candidate package identity does not match")
    _exact_fields(value["members"], NATIVE_MEMBERS)
    for path, package in NATIVE_MEMBERS.items():
        member = value["members"][path]
        _exact_fields(member, ("package", "sha256"))
        if (member["package"] != package or type(member["sha256"]) is not str
                or re.fullmatch("[0-9a-f]{64}", member["sha256"]) is None):
            raise ValueError("candidate member identity does not match")
    return value


def native_installed_pair(policy):
    """Admit only complete successful pacman -Q receipts for the exact pair.
    Other gdm-policy diagnostics are neither admission evidence nor retained."""
    if type(policy) is not dict or type(policy.get("packages")) is not dict:
        raise ValueError("missing installed candidate package pair")
    versions = {}
    for name in ("gdm", "libgdm"):
        package = policy["packages"].get(name)
        _exact_fields(package, ("returncode", "stdout", "truncated"))
        if (type(package["returncode"]) is not int or package["returncode"] != 0
                or package["truncated"] is not False or package["stdout"] != f"{name} {NATIVE_VERSION}\n"):
            raise ValueError("installed candidate package pair does not match")
        versions[name] = NATIVE_VERSION
    return versions


def parse_record(text):
    """The [User] keys of an AccountsService cache record, or None."""
    if text is None:
        return None
    parser = configparser.ConfigParser(interpolation=None, strict=False)
    parser.optionxform = str
    try:
        parser.read_string(text)
    except configparser.Error:
        return {}
    return dict(parser["User"]) if parser.has_section("User") else {}


def autologin_enabled(custom_conf):
    parser = configparser.ConfigParser(interpolation=None, strict=False)
    try:
        parser.read_string(custom_conf or "")
    except configparser.Error:
        return True  # Unreadable configuration is not proof of no autologin.
    return any(parser.get(section, key, fallback="").strip().lower() in ("true", "yes", "y", "1")
               for section in parser.sections() for key in AUTOLOGIN_KEYS)


def record_free(state, admin):
    """(ok, detail): the account is a pristine fresh-account fixture: it
    exists with a home, has no AccountsService record (so no Session
    preselection), GDM has no autologin, and its admin group membership is
    as intended."""
    user = state.get("user")
    if not state.get("exists"):
        return False, f"{user} does not exist (tuna.vm.fresh-accounts credential not honoured?)"
    if state.get("record") is not None:
        return False, f"{user} already has an AccountsService record {parse_record(state['record'])}"
    if autologin_enabled(state.get("custom_conf")):
        return False, "GDM custom.conf enables automatic or timed login"
    if not state.get("home_exists"):
        return False, f"{user} has no home directory"
    if (ADMIN_GROUP in state.get("groups", [])) != admin:
        return False, f"{user} groups {state.get('groups')} {'lack' if admin else 'include'} {ADMIN_GROUP}"
    return True, (f"{user} uid {state.get('uid')} groups {state.get('groups')}: no AccountsService record, "
                  "no Session preselection, no autologin")


def record_created(state):
    """(ok, detail): AccountsService wrote the account's record, and any
    Session it saved is Tuna Desktop."""
    keys = parse_record(state.get("record"))
    if keys is None:
        return False, f"no AccountsService record for {state.get('user')} after GDM login"
    session = keys.get("Session")
    if session not in (None, "tuna"):
        return False, f"AccountsService saved Session={session!r} for {state.get('user')}"
    return True, f"AccountsService record for {state.get('user')}: {keys}"


def session_ok(observed):
    """(ok, detail): exactly one gdm-password session of the account on a VT
    runs the Tuna Desktop compositor and no GNOME session."""
    user = observed.get("user")
    logins = [s for s in observed.get("sessions", []) if s.get("Service") == LOGIN_SERVICE
              and s.get("VTNr", "").isdigit() and s.get("VTNr") != "0"
              and s.get("User") == str(observed.get("uid"))]
    desktop = observed.get("desktop", [])
    if len(logins) != 1:
        return False, f"{user}: {len(logins)} {LOGIN_SERVICE} sessions on a VT"
    gnome = [name for name in desktop if name in GNOME_DESKTOP]
    if gnome:
        return False, f"{user}: GDM started GNOME ({gnome}) instead of Tuna Desktop"
    if COMPOSITOR_COMM not in desktop:
        return False, f"{user}: no Tuna Desktop compositor (desktop {desktop})"
    return True, f"{user}: {LOGIN_SERVICE} session {logins[0].get('Id')} on VT {logins[0].get('VTNr')} runs {desktop}"


def no_session(observed):
    return not [s for s in observed.get("sessions", []) if s.get("Service") == LOGIN_SERVICE
                and s.get("State") != "closing"]


def sudo_ok(result, admin):
    """(ok, detail): the administrator authenticates; the standard account is
    refused by sudo policy, not by a missing sudo or a bad password."""
    user = result.get("user")
    if not result.get("sudo_present"):
        return False, "sudo is not installed"
    if admin:
        if result.get("returncode") == 0:
            return True, f"{user} ({ADMIN_GROUP}) authenticated through sudo"
        return False, f"{user} ({ADMIN_GROUP}) sudo failed with exit {result.get('returncode')}"
    if result.get("returncode") != 0 and result.get("policy_denied"):
        return True, f"{user} refused by sudo policy (exit {result.get('returncode')})"
    return False, (f"{user} sudo exit {result.get('returncode')}, policy denial "
                   f"{result.get('policy_denied')}: a standard account must be refused")


class Journey:
    """Drive the journey through QMP keys and the guest agent.

    connect() returns a fresh guest agent (with run(action, *args) and
    close()); greeter_ready(greeter) is the lane's greeter predicate.
    """

    def __init__(self, qmp, connect, greeter_ready, out, record, sleep=time.sleep,
                 clock=time.monotonic, login_timeout=60, greeter_timeout=180, gdm_trace=False):
        self.qmp, self.connect, self.greeter_ready = qmp, connect, greeter_ready
        self.out, self.record, self.sleep, self.clock = out, record, sleep, clock
        self.login_timeout, self.greeter_timeout = login_timeout, greeter_timeout
        self.agent = None
        self.receipts = []
        self.gdm_trace, self.trace_active = gdm_trace, False

    def _run(self, action, *args):
        if self.agent is None:
            self.agent = self.connect()
        try:
            return self.agent.run(action, *args)
        except (OSError, RuntimeError, ValueError):
            self.agent.close()
            self.agent = None
            raise

    def _receipt(self, step, value):
        self.receipts.append({"step": step, "value": value})

    def _diagnostic_state(self, receipt, user):
        """One fresh-state receipt that never gates nor aborts the journey."""
        try:
            state = self._run("fresh-state", user)
        except (OSError, RuntimeError, ValueError) as error:
            state = {"user": user, "error": type(error).__name__}
        self._receipt(receipt, state)

    def _trace(self, action):
        """Non-gating diagnostics: never change login attempts or deadlines."""
        try:
            value = self._run(f"fresh-trace-{action}")
        except (OSError, RuntimeError, ValueError) as error:
            value = {"error": type(error).__name__}
        self._receipt(f"tunastd-first-trace-{action}", value)

    def _stop_trace(self):
        if self.trace_active:
            self.trace_active = False
            self._trace("stop")

    def _greeter(self, step, not_boot=None):
        deadline = self.clock() + self.greeter_timeout
        while self.clock() < deadline:
            try:
                greeter = self._run("greeter")
                if self.greeter_ready(greeter) and (not_boot is None
                                                    or greeter.get("boot_id") not in (None, not_boot)):
                    self._receipt(f"{step}-greeter", greeter)
                    return greeter
            except (OSError, RuntimeError, ValueError):
                pass
            self.sleep(1)
        raise RuntimeError(f"{step}: GDM greeter did not become ready")

    def _frame(self, name):
        self.qmp.frame(os.path.join(self.out, f"{name}.png"))

    def _login(self, user, step):
        tracing = self.gdm_trace and user == STANDARD and step == f"{STANDARD}-first"
        if tracing:
            self.trace_active = True
            self._trace("start")
        for attempt, selection in enumerate(SELECTIONS, 1):
            self.qmp.keys("esc")
            self.sleep(2)
            for key in selection:
                self.qmp.keys(key)
                self.sleep(1)
            self.qmp.keys("ret")
            self.sleep(2)
            self.qmp.type_text(user)
            self.qmp.keys("ret")
            self.sleep(2)
            self._frame(f"{step}-attempt-{attempt}")
            self.qmp.type_text(PASSWORD)
            self.qmp.keys("ret")
            deadline = self.clock() + self.login_timeout
            observed = None
            while self.clock() < deadline:
                try:
                    observed = self._run("fresh-session", user)
                except (OSError, RuntimeError, ValueError):
                    observed = None
                if observed is not None and not no_session(observed):
                    ok, detail = session_ok(observed)
                    if ok or any(name in GNOME_DESKTOP for name in observed.get("desktop", [])):
                        self._receipt(f"{step}-session", observed)
                        self._frame(f"{step}-session")
                        return ok, f"{detail} (selection attempt {attempt})"
                self.sleep(1)
            # The optional trace covers one ordinary attempt, never a retry.
            if tracing:
                self._stop_trace()
            if observed is not None and not no_session(observed):
                self._receipt(f"{step}-session", observed)
                return session_ok(observed)
            self.qmp.keys("esc")
            self.sleep(1)
        return False, f"{user}: no {LOGIN_SERVICE} session after {len(SELECTIONS)} GDM attempts"

    def _logout(self, user, step):
        self._run("fresh-logout", user)
        self._greeter(f"{step}-logout")
        observed = self._run("fresh-session", user)
        self._receipt(f"{step}-logout", observed)
        if not no_session(observed):
            return False, f"{user}: {LOGIN_SERVICE} session survived logout"
        return True, f"{user}: logged out; greeter ready again"

    def _cycle(self, round_name):
        for user in JOURNEY_USERS:
            step = f"{user}-{round_name}"
            try:
                ok, detail = self._login(user, step)
            finally:
                self._stop_trace()
            self.record(f"V-FRESH-LOGIN-{step}", ok, detail)
            if not ok:
                self._diagnostic_state(f"{step}-failure-state", user)
                return False
            if round_name == "first":
                state = self._run("fresh-state", user)
                self._receipt(f"{step}-state", state)
                ok, detail = record_created(state)
                self.record(f"V-FRESH-RECORD-CREATED-{user}", ok, detail)
                result = self._run("fresh-sudo", user)
                self._receipt(f"{step}-sudo", result)
                ok_sudo, detail = sudo_ok(result, user == ADMIN)
                self.record(f"V-FRESH-SUDO-{user}", ok_sudo, detail)
                ok = ok and ok_sudo
            ok_out, detail = self._logout(user, step)
            self.record(f"V-FRESH-LOGOUT-{step}", ok_out, detail)
            if round_name == "first":
                # A record that appears only now is reported with this timing;
                # V-FRESH-RECORD-CREATED above already judged the first session.
                self._diagnostic_state(f"{step}-logout-state", user)
            if not (ok and ok_out):
                return False
        return True

    def run(self):
        try:
            return self._run_journey()
        finally:
            self._stop_trace()
            with open(os.path.join(self.out, "fresh-accounts-receipts.json"), "w") as stream:
                json.dump(self.receipts, stream, indent=2)
            if self.agent is not None:
                self.agent.close()
                self.agent = None

    def _native_preflight(self):
        try:
            candidate = native_candidate()
        except (OSError, ValueError, RecursionError) as error:
            self.record("V-FRESH-GDM-NATIVE-CANDIDATE", False,
                        f"invalid private GDM candidate admission ({type(error).__name__})")
            return False
        if candidate is None:
            return True
        self._receipt("preflight-gdm-native-candidate", candidate)
        try:
            versions = native_installed_pair(self._run("gdm-policy"))
        except (OSError, RuntimeError, ValueError) as error:
            self.record("V-FRESH-GDM-NATIVE-CANDIDATE", False,
                        f"private GDM installed pair not proven ({type(error).__name__})")
            return False
        self._receipt("preflight-gdm-native-installed", versions)
        self.record("V-FRESH-GDM-NATIVE-CANDIDATE", True,
                    f"private GDM candidate {NATIVE_SOURCE_SHA}: gdm and libgdm {NATIVE_VERSION}")
        return True

    def _run_journey(self):
        self._greeter("preflight")
        if not self._native_preflight():
            return False
        self._frame("fresh-greeter")
        states = {user: self._run("fresh-state", user) for user in (*JOURNEY_USERS, SEEDED)}
        self._receipt("preflight-state", states)
        fresh = True
        for user in JOURNEY_USERS:
            ok, detail = record_free(states[user], user == ADMIN)
            self.record(f"V-FRESH-RECORD-FREE-{user}", ok, detail)
            fresh = fresh and ok
        # Negative control: the same probe must see a pre-seeded record.
        seeded_free, detail = record_free(states[SEEDED], False)
        self.record("V-FRESH-CONTROL", states[SEEDED].get("exists") is True and not seeded_free,
                    f"pre-seeded {SEEDED} classified {'record-free' if seeded_free else 'NOT record-free'}: {detail}")
        if not fresh or seeded_free:
            return False
        if not (self._cycle("first") and self._cycle("second")):
            return False
        before = self._run("reboot")["boot_id"]
        self.agent.close()
        self.agent = None
        self.sleep(10)
        greeter = self._greeter("reboot", not_boot=before)
        self.record("V-FRESH-REBOOT", True, f"guest rebooted: boot {before} -> {greeter['boot_id']}")
        states = {user: self._run("fresh-state", user) for user in JOURNEY_USERS}
        self._receipt("reboot-state", states)
        for user in JOURNEY_USERS:
            ok, detail = record_created(states[user])
            self.record(f"V-FRESH-RECORD-KEPT-{user}", ok, detail)
            if not ok:
                return False
        return self._cycle("reboot")
