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
must start the Tuna Desktop compositor and no GNOME session. AccountsService's
effective Session must remain tuna, whether supplied by its account template
or a saved record; admin sudo must authenticate and standard sudo be refused.
"""
import configparser
import json
import os
import time

PASSWORD = "tuna"  # Public fixture value (tuna-vm-fresh-accounts).
STANDARD, ADMIN, SEEDED = "tunastd", "tunaadmin", "tunaseeded"
JOURNEY_USERS = (STANDARD, ADMIN)
ADMIN_GROUP = "wheel"
COMPOSITOR_COMM = "tuna-compositor"
GNOME_DESKTOP = ("gnome-shell", "gnome-session-b", "gnome-session-s")
LOGIN_SERVICE = "gdm-password"
AUTOLOGIN_KEYS = ("automaticloginenable", "timedloginenable")
# The fresh-account boot sets GDM's org.gnome.login-screen disable-user-list
# (tuna-vm-fresh-accounts): gnome-shell js/gdm/loginDialog.js then asks for
# the user name itself, so the name typed is always the account
# authenticated. Tab-counting through the user list was order-dependent and
# could type the name as another listed account's password.


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


def session_state(state):
    """(ok, detail): the effective session and any saved choice are Tuna.

    AccountsService 26.27.3 loads templates without writing a cache file,
    and GDM 51 only saves selected/nonempty account properties. The stock
    GNOME reference also leaves record-free accounts record-free on login.
    See docs/vm-lifecycle.md for the direct reference receipt and sources.
    """
    user = state.get("user")
    if state.get("effective_session") != "tuna":
        return False, f"AccountsService effective Session={state.get('effective_session')!r} for {user}"
    keys = parse_record(state.get("record"))
    if keys is not None and keys.get("Session") not in (None, "tuna"):
        return False, f"AccountsService saved Session={keys['Session']!r} for {user}"
    return True, f"AccountsService effective Session=tuna for {user}; cache record: {keys}"


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
                 clock=time.monotonic, login_timeout=60, greeter_timeout=180):
        self.qmp, self.connect, self.greeter_ready = qmp, connect, greeter_ready
        self.out, self.record, self.sleep, self.clock = out, record, sleep, clock
        self.login_timeout, self.greeter_timeout = login_timeout, greeter_timeout
        self.agent = None
        self.receipts = []

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
        self.qmp.keys("esc")
        self.sleep(2)
        self.qmp.type_text(user)
        self.qmp.keys("ret")
        self.sleep(2)
        self._frame(f"{step}-password")
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
                    return ok, detail
            self.sleep(1)
        if observed is not None and not no_session(observed):
            self._receipt(f"{step}-session", observed)
            return session_ok(observed)
        return False, f"{user}: no {LOGIN_SERVICE} session after GDM name and password entry"

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
            ok, detail = self._login(user, step)
            self.record(f"V-FRESH-LOGIN-{step}", ok, detail)
            if not ok:
                return False
            state = self._run("fresh-state", user)
            self._receipt(f"{step}-state", state)
            ok, detail = session_state(state)
            self.record(f"V-FRESH-SESSION-{step}", ok, detail)
            if round_name == "first":
                result = self._run("fresh-sudo", user)
                self._receipt(f"{step}-sudo", result)
                ok_sudo, detail = sudo_ok(result, user == ADMIN)
                self.record(f"V-FRESH-SUDO-{user}", ok_sudo, detail)
                ok = ok and ok_sudo
            ok_out, detail = self._logout(user, step)
            self.record(f"V-FRESH-LOGOUT-{step}", ok_out, detail)
            if not (ok and ok_out):
                return False
        return True

    def run(self):
        try:
            return self._run_journey()
        finally:
            with open(os.path.join(self.out, "fresh-accounts-receipts.json"), "w") as stream:
                json.dump(self.receipts, stream, indent=2)
            if self.agent is not None:
                self.agent.close()
                self.agent = None

    def _run_journey(self):
        self._greeter("preflight")
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
            ok, detail = session_state(states[user])
            self.record(f"V-FRESH-SESSION-RELOADED-{user}", ok, detail)
            if not ok:
                return False
        return self._cycle("reboot")
