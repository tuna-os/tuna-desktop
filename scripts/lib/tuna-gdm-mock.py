#!/usr/bin/env python3
"""GDM 51's greeter API, AccountsService, logind and gsd-smartcard for scripts/tuna-greeter-proof (#1164).

Usage: tuna-gdm-mock.py --address BUS --server-dir DIR --log FILE.jsonl
                        --users FILE.json --secrets FILE [--sessions ID,ID]
                        [--fallback-session ID] [--smartcard-user USER]
                        [--smartcard-certs N]

Owns org.gnome.DisplayManager, org.freedesktop.Accounts and
org.freedesktop.login1 on the private bus at BUS (never the host's system
bus), and org.gnome.SettingsDaemon.Smartcard on the session bus, serving
them with GDM 51.1.1's shapes (daemon/gdm-manager.xml, daemon/gdm-session.xml,
daemon/gdm-session.c, daemon/gdm-session-worker.c, daemon/gdm-manager.c,
https://gitlab.gnome.org/GNOME/gdm/-/tree/51.1.1), AccountsService's
(data/org.freedesktop.Accounts{,.User}.xml), logind's and
gnome-settings-daemon's smartcard plugin's:

  /org/gnome/DisplayManager/Manager  org.gnome.DisplayManager.Manager
      OpenSession() -> s   a GDBusServer listening on unix:dir=DIR (GDM's
                           /run/gdm/dbus), EXTERNAL only, peers of this uid
                           only; returns its client address
      OpenReauthenticationChannel(s user) -> s
                           the user's running GDM session's channel (another
                           server); AccessDenied without one, as GDM answers
                           a login screen ("only allowed to open
                           reauthentication channels for running sessions")
      RegisterSession(), RegisterDisplay()
  peer connection: /org/gnome/DisplayManager/Session
      UserVerifier: EnableExtensions(as), BeginVerification(s),
          BeginVerificationForUser(s, s), AnswerQuery(s, s), Cancel()
      UserVerifier.ChoiceList (once enabled): SelectChoice(s, s), ChoiceQuery
      UserVerifier.CustomJSON (once enabled): Reply(s, s), ReportError, Request
      Greeter: SelectSession(s), StartSessionWhenReady(s, b),
          GetTimedLoginDetails(), BeginAutoLogin(s), StopConflictingSession()
  /org/freedesktop/Accounts  ListCachedUsers() -> ao; users' properties
  /org/freedesktop/login1  ListSessions, GetSession, GetSessionByPID;
      SessionNew/SessionRemoved; sessions' Id, Name, Type, Class, State,
      Remote, Active, Service (the greeter's own is c1, class greeter)
  session bus /org/gnome/SettingsDaemon/Smartcard  an ObjectManager with one
      Token (IsInserted)
  /org/tuna/Proof/GdmMock  org.tuna.Proof.GdmMock: the proof's controls
      (SetSmartcard, SetSwitchable, SetTimedLogin, ApproveWebLogin,
      SetPasskey, TouchPasskey, EndSessions (a user logs out),
      CloseGreeterChannels, ActivateGreeterSession)

Conversations (gdm-session-worker.c through PAM):
  gdm-password   'Password: '; a user with "second_factor" is then asked
                 'Verification code: ' (InfoQuery, echo on). Failure: Problem
                 'Sorry, password authentication didn’t work. Please try
                 again.', VerificationFailed, ConversationStopped.
  gdm-smartcard  no token: Info 'Please insert smart card' and it waits;
                 with --smartcard-certs 2 and ChoiceList enabled, ChoiceQuery
                 'Select a certificate' first; then 'PIN for PIV_II: '.
                 The card logs in --smartcard-user (BeginVerification needs no
                 user: the certificate names it).
  gdm-switchable-auth  missing (the begin fails with SessionWorker.Error.
                 ServiceUnavailable 'PAM service gdm-switchable-auth was not
                 found', then ServiceUnavailable, VerificationFailed,
                 ConversationStopped) until SetSwitchable(true); then a user
                 with "switchable" gets SSSD's CustomJSON 'auth-mechanisms'
                 request, others pam_unix's 'Password: '. Web login succeeds
                 once ApproveWebLogin(true). A passkey mechanism's
                 keyConnected is SetPasskey's; its reply (the PIN, kerberos
                 and cryptoChallenge echoed) waits for TouchPasskey(). A
                 smartcard mechanism's reply names one of its certificates
                 and carries the card's PIN. A "password_expired" user's
                 right password gets pam_sss's 'Password expired. Change
                 your password now.' (Info), then 'New Password: ' and
                 'Reenter new Password: ' (SecretInfoQuery, AnswerQuery).
  Success: VerificationComplete then SessionOpened(service, cN) (gdm-session.c
  on_opened) and a logind session cN for the user. Cancel emits Reset, then
  ConversationStopped for each conversation, forgets the selected session and
  closes a session opened but not started. On a re-authentication channel
  success switches to the user's session (logged as kind "switch"), the login
  screen's session turns inactive and the channel closes.

The login screen's power menu calls logind (on hardware): CanSuspend,
CanReboot, CanPowerOff -> 'yes'; Suspend(b), Reboot(b), PowerOff(b) are
recorded and otherwise ignored. AddUser(s name, s real_name, t uid) lists a
user (UserAdded) and DeleteUser(s name) removes one (UserDeleted, the object
goes). A user's "icon" in FILE.json is relative to that file's directory.

Every call and emitted signal is appended to FILE.jsonl as one JSON object
(kind, interface, member, conn, args, outcome, wall and monotonic time).
Answers are never logged: AnswerQuery and Reply record the answer's SHA-256
and whether it matched. On readiness one line is printed:
    tuna-gdm-mock: ready <unique bus name> <server address>
"""
import argparse
import hashlib
import json
import os
import signal
import sys
import time

import gi

gi.require_version("Gio", "2.0")
from gi.repository import Gio, GLib  # noqa: E402

GDM_NAME = "org.gnome.DisplayManager"
MANAGER_PATH = "/org/gnome/DisplayManager/Manager"
MANAGER_IFACE = "org.gnome.DisplayManager.Manager"
SESSION_PATH = "/org/gnome/DisplayManager/Session"
VERIFIER_IFACE = "org.gnome.DisplayManager.UserVerifier"
CHOICE_IFACE = "org.gnome.DisplayManager.UserVerifier.ChoiceList"
JSON_IFACE = "org.gnome.DisplayManager.UserVerifier.CustomJSON"
GREETER_IFACE = "org.gnome.DisplayManager.Greeter"
ACCOUNTS_NAME = "org.freedesktop.Accounts"
ACCOUNTS_PATH = "/org/freedesktop/Accounts"
ACCOUNTS_IFACE = "org.freedesktop.Accounts"
USER_IFACE = "org.freedesktop.Accounts.User"
LOGIND_NAME = "org.freedesktop.login1"
LOGIND_PATH = "/org/freedesktop/login1"
LOGIND_MANAGER = "org.freedesktop.login1.Manager"
LOGIND_SESSION = "org.freedesktop.login1.Session"
SC_NAME = "org.gnome.SettingsDaemon.Smartcard"
SC_PATH = "/org/gnome/SettingsDaemon/Smartcard"
SC_TOKEN_IFACE = "org.gnome.SettingsDaemon.Smartcard.Token"
SC_TOKEN_PATH = SC_PATH + "/Manager/Drivers/p11_kit_proxy/Tokens/PIV_II"
CONTROL_PATH = "/org/tuna/Proof/GdmMock"
CONTROL_IFACE = "org.tuna.Proof.GdmMock"
PAM_PROMPT = "Password: "
CODE_PROMPT = "Verification code: "
CARD_INSERT = "Please insert smart card"
# pam_sss: "PIN for %s: " with the token's name.
CARD_PIN = "PIN for PIV_II: "
CARD_CHOICE = "Select a certificate"
# pam_sss's password change (src/sss_client/pam_sss.c).
PASSWORD_EXPIRED = "Password expired. Change your password now."
NEW_PROMPT = "New Password: "
RETYPE_PROMPT = "Reenter new Password: "
# gdm-session-worker.c get_generic_error_message.
FAILED = {
    "gdm-password": "Sorry, password authentication didn’t work. Please try again.",
    "gdm-smartcard": "Sorry, smart card authentication didn’t work. Please try again.",
    "gdm-autologin": "Sorry, auto login didn’t work. Please try again.",
}
FAILED_OTHER = "Sorry, that didn’t work. Please try again."
# gdm-session.c begin_verification_conversation / GDM 51 service checks.
DAEMON_ONLY = ("gdm-autologin", "gdm-launch-environment")
SWITCHABLE = "gdm-switchable-auth"
UNAVAILABLE = "org.gnome.DisplayManager.SessionWorker.Error.ServiceUnavailable"

MANAGER_XML = f"""
<node>
  <interface name="{MANAGER_IFACE}">
    <method name="RegisterDisplay"/>
    <method name="RegisterSession"/>
    <method name="OpenSession"><arg name="address" direction="out" type="s"/></method>
    <method name="OpenReauthenticationChannel">
      <arg name="username" direction="in" type="s"/>
      <arg name="address" direction="out" type="s"/>
    </method>
    <property name="Version" type="s" access="read"/>
  </interface>
</node>
"""

SESSION_XML = f"""
<node>
  <interface name="{VERIFIER_IFACE}">
    <method name="EnableExtensions"><arg name="extensions" direction="in" type="as"/></method>
    <method name="BeginVerification"><arg name="service_name" direction="in" type="s"/></method>
    <method name="BeginVerificationForUser">
      <arg name="service_name" direction="in" type="s"/>
      <arg name="username" direction="in" type="s"/>
    </method>
    <method name="AnswerQuery">
      <arg name="service_name" direction="in" type="s"/>
      <arg name="answer" direction="in" type="s"/>
    </method>
    <method name="Cancel"/>
    <signal name="ConversationStarted"><arg name="service_name" type="s"/></signal>
    <signal name="ConversationStopped"><arg name="service_name" type="s"/></signal>
    <signal name="ReauthenticationStarted"><arg name="pid_of_caller" type="i"/></signal>
    <signal name="Info"><arg name="service_name" type="s"/><arg name="info" type="s"/></signal>
    <signal name="Problem"><arg name="service_name" type="s"/><arg name="problem" type="s"/></signal>
    <signal name="InfoQuery"><arg name="service_name" type="s"/><arg name="query" type="s"/></signal>
    <signal name="SecretInfoQuery"><arg name="service_name" type="s"/><arg name="query" type="s"/></signal>
    <signal name="Reset"/>
    <signal name="ServiceUnavailable"><arg name="service_name" type="s"/><arg name="message" type="s"/></signal>
    <signal name="VerificationFailed"><arg name="service_name" type="s"/></signal>
    <signal name="VerificationComplete"><arg name="service_name" type="s"/></signal>
  </interface>
  <interface name="{CHOICE_IFACE}">
    <method name="SelectChoice">
      <arg name="service_name" direction="in" type="s"/>
      <arg name="choice" direction="in" type="s"/>
    </method>
    <signal name="ChoiceQuery">
      <arg name="service_name" type="s"/>
      <arg name="prompt_message" type="s"/>
      <arg name="list" type="a{{ss}}"/>
    </signal>
  </interface>
  <interface name="{JSON_IFACE}">
    <method name="Reply">
      <arg name="service_name" direction="in" type="s"/>
      <arg name="reply" direction="in" type="s"/>
    </method>
    <method name="ReportError">
      <arg name="service_name" direction="in" type="s"/>
      <arg name="error" direction="in" type="s"/>
    </method>
    <signal name="Request">
      <arg name="service_name" type="s"/>
      <arg name="protocol" type="s"/>
      <arg name="version" type="u"/>
      <arg name="request" type="s"/>
    </signal>
  </interface>
  <interface name="{GREETER_IFACE}">
    <method name="SelectSession"><arg name="session" direction="in" type="s"/></method>
    <method name="SelectUser"><arg name="username" direction="in" type="s"/></method>
    <method name="BeginAutoLogin"><arg name="username" direction="in" type="s"/></method>
    <method name="GetTimedLoginDetails">
      <arg name="enabled" direction="out" type="b"/>
      <arg name="username" direction="out" type="s"/>
      <arg name="delay" direction="out" type="i"/>
    </method>
    <method name="StartSessionWhenReady">
      <arg name="service_name" direction="in" type="s"/>
      <arg name="should_start_session" direction="in" type="b"/>
    </method>
    <method name="StopConflictingSession"/>
    <signal name="SelectedUserChanged"><arg name="username" type="s"/></signal>
    <signal name="DefaultLanguageNameChanged"><arg name="language_name" type="s"/></signal>
    <signal name="DefaultSessionNameChanged"><arg name="session_name" type="s"/></signal>
    <signal name="TimedLoginRequested"><arg name="username" type="s"/><arg name="delay" type="i"/></signal>
    <signal name="SessionOpened"><arg name="service_name" type="s"/><arg name="session_id" type="s"/></signal>
    <signal name="Reauthenticated"><arg name="service_name" type="s"/></signal>
  </interface>
</node>
"""

ACCOUNTS_XML = f"""
<node>
  <interface name="{ACCOUNTS_IFACE}">
    <method name="ListCachedUsers"><arg name="users" direction="out" type="ao"/></method>
    <method name="FindUserByName">
      <arg name="name" direction="in" type="s"/>
      <arg name="user" direction="out" type="o"/>
    </method>
    <signal name="UserAdded"><arg name="user" type="o"/></signal>
    <signal name="UserDeleted"><arg name="user" type="o"/></signal>
  </interface>
  <interface name="{USER_IFACE}">
    <property name="Uid" type="t" access="read"/>
    <property name="UserName" type="s" access="read"/>
    <property name="RealName" type="s" access="read"/>
    <property name="IconFile" type="s" access="read"/>
    <property name="SystemAccount" type="b" access="read"/>
    <property name="Locked" type="b" access="read"/>
    <property name="XSession" type="s" access="read"/>
    <property name="Session" type="s" access="read"/>
  </interface>
</node>
"""

LOGIND_XML = f"""
<node>
  <interface name="{LOGIND_MANAGER}">
    <method name="ListSessions"><arg name="sessions" direction="out" type="a(susso)"/></method>
    <method name="GetSession">
      <arg name="session_id" direction="in" type="s"/>
      <arg name="object_path" direction="out" type="o"/>
    </method>
    <method name="GetSessionByPID">
      <arg name="pid" direction="in" type="u"/>
      <arg name="object_path" direction="out" type="o"/>
    </method>
    <method name="CanSuspend"><arg name="result" direction="out" type="s"/></method>
    <method name="CanReboot"><arg name="result" direction="out" type="s"/></method>
    <method name="CanPowerOff"><arg name="result" direction="out" type="s"/></method>
    <method name="Suspend"><arg name="interactive" direction="in" type="b"/></method>
    <method name="Reboot"><arg name="interactive" direction="in" type="b"/></method>
    <method name="PowerOff"><arg name="interactive" direction="in" type="b"/></method>
    <signal name="SessionNew"><arg name="session_id" type="s"/><arg name="object_path" type="o"/></signal>
    <signal name="SessionRemoved"><arg name="session_id" type="s"/><arg name="object_path" type="o"/></signal>
  </interface>
  <interface name="{LOGIND_SESSION}">
    <property name="Id" type="s" access="read"/>
    <property name="Name" type="s" access="read"/>
    <property name="Type" type="s" access="read"/>
    <property name="Class" type="s" access="read"/>
    <property name="State" type="s" access="read"/>
    <property name="Remote" type="b" access="read"/>
    <property name="Active" type="b" access="read"/>
    <property name="Service" type="s" access="read"/>
  </interface>
</node>
"""

SMARTCARD_XML = f"""
<node>
  <interface name="org.freedesktop.DBus.ObjectManager">
    <method name="GetManagedObjects">
      <arg name="objects" direction="out" type="a{{oa{{sa{{sv}}}}}}"/>
    </method>
    <signal name="InterfacesAdded">
      <arg name="object" type="o"/><arg name="interfaces" type="a{{sa{{sv}}}}"/>
    </signal>
    <signal name="InterfacesRemoved">
      <arg name="object" type="o"/><arg name="interfaces" type="as"/>
    </signal>
  </interface>
  <interface name="{SC_TOKEN_IFACE}">
    <property name="Name" type="s" access="read"/>
    <property name="Driver" type="o" access="read"/>
    <property name="IsInserted" type="b" access="read"/>
    <property name="UsedToLogin" type="b" access="read"/>
  </interface>
</node>
"""

CONTROL_XML = f"""
<node>
  <interface name="{CONTROL_IFACE}">
    <method name="SetSmartcard"><arg name="inserted" direction="in" type="b"/></method>
    <method name="SetSwitchable"><arg name="installed" direction="in" type="b"/></method>
    <method name="SetTimedLogin">
      <arg name="user" direction="in" type="s"/><arg name="delay" direction="in" type="i"/>
    </method>
    <method name="ApproveWebLogin"><arg name="approved" direction="in" type="b"/></method>
    <method name="SetPasskey"><arg name="connected" direction="in" type="b"/></method>
    <method name="TouchPasskey"/>
    <method name="AddUser">
      <arg name="name" direction="in" type="s"/><arg name="real_name" direction="in" type="s"/>
      <arg name="uid" direction="in" type="t"/>
    </method>
    <method name="DeleteUser"><arg name="name" direction="in" type="s"/></method>
    <method name="CloseGreeterChannels"/>
    <method name="ActivateGreeterSession"/>
    <method name="EndSessions"><arg name="user" direction="in" type="s"/></method>
  </interface>
</node>
"""


class Refused(Exception):
    def __init__(self, name, message):
        super().__init__(message)
        self.name = name


class Log:
    def __init__(self, path):
        self.stream = open(path, "a", encoding="utf-8")

    def record(self, **fields):
        fields.setdefault("wall", time.time())
        fields["mono"] = time.monotonic()
        self.stream.write(json.dumps(fields, sort_keys=True, ensure_ascii=False) + "\n")
        self.stream.flush()


def sha(text):
    return hashlib.sha256(text.encode()).hexdigest()


class Peer:
    """One client connection to a session server: the greeter's own
    GdmSession, or (reauth_user set) a user session's re-authentication
    channel."""

    def __init__(self, mock, conn, number, reauth_user=None):
        self.mock = mock
        self.conn = conn
        self.number = number
        self.reauth_user = reauth_user
        self.extensions = set()
        # service -> {"user", "query", "step"}
        self.conversations = {}
        self.selected_session = None
        self.opened = {}  # service -> session id
        self.running = False

    def emit(self, iface, member, signature, args):
        params = GLib.Variant(signature, args) if signature else None
        self.conn.emit_signal(None, SESSION_PATH, iface, member, params)
        self.mock.log.record(kind="signal", interface=iface, member=member,
                             conn=self.number, args=list(args) if args else [])

    def stop_conversation(self, service):
        if self.conversations.pop(service, None) is not None:
            self.emit(VERIFIER_IFACE, "ConversationStopped", "(s)", (service,))

    def reset(self):
        # gdm_session_reset: Reset, then do_reset stops every conversation
        # and forgets the selected session; an opened session that was not
        # started closes with them.
        self.emit(VERIFIER_IFACE, "Reset", None, None)
        for service in list(self.conversations):
            self.stop_conversation(service)
        self.selected_session = None
        for session_id in self.opened.values():
            self.mock.logind.remove(session_id)
        self.opened.clear()
        self.running = False

    def method_call(self, iface, member, args):
        if iface == VERIFIER_IFACE:
            return self.verifier(member, args)
        if iface == CHOICE_IFACE and CHOICE_IFACE in self.extensions:
            return self.choice(member, args)
        if iface == JSON_IFACE and JSON_IFACE in self.extensions:
            return self.custom_json(member, args)
        if iface == GREETER_IFACE and self.reauth_user is None:
            return self.greeter(member, args)
        raise Refused("org.freedesktop.DBus.Error.UnknownInterface", iface)

    def answer_detail(self, answer, right, conversation, step):
        return {"answer_sha256": sha(answer), "matches": right, "step": step,
                "matches_password": right if step == "password" else None,
                "user": conversation["user"]}

    def verifier(self, member, args):
        if member == "EnableExtensions":
            (extensions,) = args
            self.extensions = {e for e in extensions if e in (CHOICE_IFACE, JSON_IFACE)}
            return {"enabled": sorted(self.extensions)}
        if member in ("BeginVerificationForUser", "BeginVerification"):
            service = args[0]
            user = args[1] if member == "BeginVerificationForUser" else None
            if "/" in service or not service.startswith("gdm-") or service in DAEMON_ONLY:
                raise Refused("org.gnome.DisplayManager.Session.Error.InvalidService",
                              f"Service {service} is not allowed")
            if service in self.conversations:
                raise Refused("org.gnome.DisplayManager.Session.Error.Busy",
                              f"Conversation {service} is already running")
            if service == SWITCHABLE and not self.mock.switchable:
                # The worker found no PAM service: the begin fails and
                # report_and_stop_conversation follows.
                GLib.idle_add(self.unavailable, service,
                              f"PAM service {service} was not found")
                raise Refused(UNAVAILABLE, f"PAM service {service} was not found")
            if service not in ("gdm-password", "gdm-smartcard", SWITCHABLE):
                GLib.idle_add(self.unavailable, service, "")
                raise Refused(UNAVAILABLE, "")
            if self.reauth_user is not None and user not in (None, self.reauth_user):
                raise Refused("org.gnome.DisplayManager.Session.Error.WrongUser",
                              f"channel is {self.reauth_user}'s")
            self.conversations[service] = {"user": user, "query": None}
            self.emit(VERIFIER_IFACE, "ConversationStarted", "(s)", (service,))
            # The reply waits until the worker is set up for the user; the
            # defaults follow (gdm_session_select_user), then PAM asks.
            GLib.idle_add(self.after_begin, service, user)
            return {"user": user}
        if member == "AnswerQuery":
            service, answer = args
            conversation = self.conversations.get(service)
            if conversation is None or conversation["query"] not in (
                    "password", "code", "pin", "new", "retype"):
                # GDM completes the call and drops an unexpected answer.
                return {"answer_sha256": sha(answer), "dropped": True}
            step = conversation["query"]
            conversation["query"] = None
            right = self.mock.check(step, conversation, answer)
            GLib.idle_add(self.after_answer, service, step, right)
            return self.answer_detail(answer, right, conversation, step)
        if member == "Cancel":
            GLib.idle_add(lambda: (self.reset(), False)[1])
            return None
        raise Refused("org.freedesktop.DBus.Error.UnknownMethod", member)

    def unavailable(self, service, message):
        self.emit(VERIFIER_IFACE, "ServiceUnavailable", "(ss)", (service, message))
        self.emit(VERIFIER_IFACE, "VerificationFailed", "(s)", (service,))
        self.emit(VERIFIER_IFACE, "ConversationStopped", "(s)", (service,))
        return GLib.SOURCE_REMOVE

    def choice(self, member, args):
        if member != "SelectChoice":
            raise Refused("org.freedesktop.DBus.Error.UnknownMethod", member)
        service, choice = args
        conversation = self.conversations.get(service)
        if conversation is None or conversation["query"] != "choice":
            return {"dropped": True}
        conversation["query"] = None
        conversation["certificate"] = choice
        GLib.idle_add(self.ask_pin, service)
        return {"choice": choice}

    def custom_json(self, member, args):
        service, text = args
        conversation = self.conversations.get(service)
        if member == "ReportError":
            return {"error": text}
        if member != "Reply":
            raise Refused("org.freedesktop.DBus.Error.UnknownMethod", member)
        try:
            reply = json.loads(text)
        except ValueError as error:
            raise Refused("org.freedesktop.DBus.Error.NotSupported",
                          f"JSON reply is not valid: {error}")
        if conversation is None or conversation["query"] != "json":
            return {"dropped": True, "reply_sha256": sha(text)}
        conversation["query"] = None
        selection = reply.get("authSelection", {})
        mechanism = next((k for k in selection if k != "status"), None)
        response = selection.get(mechanism) or {}
        offered = self.mock.sssd_mechanism(conversation["user"], mechanism) or {}
        role = offered.get("role")
        detail = {}
        if role == "eidp":
            right = self.mock.web_login_approved
        elif role == "passkey":
            # pam_sss: the PIN, and the request's kerberos and
            # cryptoChallenge echoed; then the key is touched.
            right = (response.get("pin") == self.mock.secrets.get("passkey@pin")
                     and response.get("kerberos") == offered.get("kerberos")
                     and response.get("cryptoChallenge") == offered.get("cryptoChallenge"))
        elif role == "smartcard":
            # One of the offered certificates, and the card's PIN.
            cert = next((c for c in offered.get("certificates", [])
                         if c.get("keyId") == response.get("keyId")), None)
            detail["key_id"] = response.get("keyId")
            right = (cert is not None
                     and all(response.get(k) == cert.get(k)
                             for k in ("tokenName", "moduleName", "label"))
                     and response.get("pin") == self.mock.secrets.get("smartcard@pin"))
        elif "password" in response:
            right = self.mock.check("password", conversation, response["password"])
        else:
            right = False
        if right and role == "passkey":
            conversation["query"] = "touch"
        else:
            GLib.idle_add(self.after_answer, service, "json", right)
        return {"reply_sha256": sha(text), "status": selection.get("status"),
                "mechanism": mechanism, "role": role, "matches": right,
                "user": conversation["user"], **detail}

    def after_begin(self, service, user):
        conversation = self.conversations.get(service)
        if conversation is None:
            return GLib.SOURCE_REMOVE
        if user is not None:
            self.emit(GREETER_IFACE, "DefaultLanguageNameChanged", "(s)", ("en_US.UTF-8",))
            saved = self.mock.saved_session(user)
            self.emit(GREETER_IFACE, "DefaultSessionNameChanged", "(s)",
                      (saved or self.mock.fallback_session,))
        if service == "gdm-password":
            self.ask(service, "password", "SecretInfoQuery", PAM_PROMPT)
        elif service == "gdm-smartcard":
            if not self.mock.card_inserted:
                # pam_sss waits for the card.
                self.emit(VERIFIER_IFACE, "Info", "(ss)", (service, CARD_INSERT))
            elif self.mock.card_certs > 1 and CHOICE_IFACE in self.extensions:
                conversation["query"] = "choice"
                owner = (self.mock.entry(self.mock.card_user) or {}).get("real_name", "PIV")
                choices = {f"cert-{i}": f"{owner} ({kind})" for i, kind in
                           enumerate(("Authentication", "Signing")[:self.mock.card_certs], 1)}
                self.conn.emit_signal(None, SESSION_PATH, CHOICE_IFACE, "ChoiceQuery",
                                      GLib.Variant("(ssa{ss})", (service, CARD_CHOICE, choices)))
                self.mock.log.record(kind="signal", interface=CHOICE_IFACE,
                                     member="ChoiceQuery", conn=self.number,
                                     args=[service, CARD_CHOICE, choices])
            else:
                self.ask_pin(service)
        elif service == SWITCHABLE:
            selection = self.mock.sssd_request(user)
            if selection is not None and JSON_IFACE in self.extensions:
                conversation["query"] = "json"
                text = json.dumps(selection)
                self.conn.emit_signal(None, SESSION_PATH, JSON_IFACE, "Request",
                                      GLib.Variant("(ssus)", (service, "auth-mechanisms", 1, text)))
                self.mock.log.record(kind="signal", interface=JSON_IFACE, member="Request",
                                     conn=self.number,
                                     args=[service, "auth-mechanisms", 1, selection])
            else:
                # pam_unix behind the switchable stack.
                self.ask(service, "password", "SecretInfoQuery", PAM_PROMPT)
        return GLib.SOURCE_REMOVE

    def ask(self, service, step, member, prompt):
        conversation = self.conversations.get(service)
        if conversation is None:
            return GLib.SOURCE_REMOVE
        conversation["query"] = step
        self.emit(VERIFIER_IFACE, member, "(ss)", (service, prompt))
        return GLib.SOURCE_REMOVE

    def ask_pin(self, service):
        return self.ask(service, "pin", "SecretInfoQuery", CARD_PIN)

    def after_answer(self, service, step, right):
        conversation = self.conversations.get(service)
        if conversation is None:
            return GLib.SOURCE_REMOVE
        user = conversation["user"]
        if right and step == "password" and self.mock.second_factor(user):
            self.ask(service, "code", "InfoQuery", CODE_PROMPT)
        elif (right and step == "json" and self.mock.password_expired(user)
              and not conversation.get("changing")):
            # pam_sss: right but expired; it is changed over plain PAM.
            conversation["changing"] = True
            self.emit(VERIFIER_IFACE, "Info", "(ss)", (service, PASSWORD_EXPIRED))
            self.ask(service, "new", "SecretInfoQuery", NEW_PROMPT)
        elif right and step == "new":
            self.ask(service, "retype", "SecretInfoQuery", RETYPE_PROMPT)
        elif right:
            if conversation["user"] is None:
                conversation["user"] = self.mock.card_user
            self.complete(service, conversation["user"])
        else:
            self.emit(VERIFIER_IFACE, "Problem", "(ss)",
                      (service, FAILED.get(service, FAILED_OTHER)))
            self.emit(VERIFIER_IFACE, "VerificationFailed", "(s)", (service,))
            self.stop_conversation(service)
        return GLib.SOURCE_REMOVE

    def complete(self, service, user):
        if self.reauth_user is not None:
            # gdm-manager.c on_reauthentication_verification_complete:
            # switch to the user's session, then close the channel.
            self.emit(VERIFIER_IFACE, "VerificationComplete", "(s)", (service,))
            session_id = self.mock.logind.session_of(user)
            self.mock.log.record(kind="switch", member="ActivateSession", conn=self.number,
                                 args=[user, session_id, service])
            self.mock.logind.set_active(self.mock.logind.greeter_id, False)
            self.conversations.pop(service, None)
            GLib.timeout_add(200, lambda: (self.conn.close(None, None, None), False)[1])
            return
        session_id = f"c{self.mock.next_session_id()}"
        self.opened[service] = session_id
        self.running = True
        self.mock.logind.add(session_id, user, managed=True)
        self.emit(VERIFIER_IFACE, "VerificationComplete", "(s)", (service,))
        self.emit(GREETER_IFACE, "SessionOpened", "(ss)", (service, session_id))

    def greeter(self, member, args):
        if member == "SelectSession":
            (session,) = args
            if self.running:
                raise Refused("org.gnome.DisplayManager.Session.Error.Running",
                              "Session already running")
            if "/" in session or session not in self.mock.sessions:
                raise Refused("org.gnome.DisplayManager.Session.Error.InvalidSession",
                              f"Session {session} is not installed")
            self.selected_session = session
            return {"selected": session}
        if member == "StartSessionWhenReady":
            service, start = args
            opened = self.opened.get(service)
            if opened is not None:
                # The session starts: it is no longer this greeter's to close.
                self.opened.pop(service)
                self.mock.logind.set_active(opened, True)
                self.mock.logind.set_active(self.mock.logind.greeter_id, False)
            return {"session_opened": opened,
                    "selected_session": self.selected_session or self.mock.fallback_session,
                    "start": bool(start)}
        if member == "GetTimedLoginDetails":
            if self.running:
                raise Refused("org.freedesktop.DBus.Error.InvalidArgs", "Session already running")
            user, delay = self.mock.timed_login or ("", 0)
            if user:
                GLib.idle_add(lambda: (self.emit(GREETER_IFACE, "TimedLoginRequested", "(si)",
                                                 (user, delay)), False)[1])
            return GLib.Variant("(bsi)", (bool(user), user, delay))
        if member == "BeginAutoLogin":
            (user,) = args
            if self.running:
                raise Refused("org.freedesktop.DBus.Error.InvalidArgs",
                              "Session already owned by user")
            if self.mock.autologin_user != user:
                raise Refused("org.freedesktop.DBus.Error.AccessDenied",
                              f"Autologin not permitted for user {user}")
            GLib.idle_add(lambda: (self.complete("gdm-autologin", user), False)[1])
            return {"user": user}
        if member == "StopConflictingSession":
            if not self.opened:
                raise Refused("org.freedesktop.DBus.Error.AccessDenied",
                              "Can't stop conflicting session if this session is not opened yet")
            opened = next(iter(self.opened.values()))
            owner = self.mock.logind.sessions[opened]["user"]
            others = [sid for sid, s in self.mock.logind.sessions.items()
                      if s["user"] == owner and sid != opened]
            for sid in others:
                GLib.timeout_add(500, lambda sid=sid: (self.mock.logind.remove(sid), False)[1])
            return {"stopping": others}
        if member == "SelectUser":
            raise Refused("org.gnome.DisplayManager.Session.Error.NotSupported",
                          f"{member} is outside the proof")
        raise Refused("org.freedesktop.DBus.Error.UnknownMethod", member)

    def closed(self):
        # gdm-session.c on_outside_connection_closed: the conversations of
        # a greeter that went away stop.
        self.conversations.clear()


class Logind:
    """logind's sessions as the login dialog reads them."""

    def __init__(self, mock):
        self.mock = mock
        self.greeter_id = "c1"
        self.sessions = {}
        self.node = Gio.DBusNodeInfo.new_for_xml(LOGIND_XML)
        self.registrations = {}

    def path(self, session_id):
        return f"{LOGIND_PATH}/session/_{session_id}"

    def export(self, bus):
        self.bus = bus
        bus.register_object(LOGIND_PATH, self.node.lookup_interface(LOGIND_MANAGER),
                            self.manager_call, None, None)
        self.add(self.greeter_id, "gdm-greeter", managed=False, klass="greeter",
                 service="gdm-launch-environment", active=True, quiet=True)
        for entry in self.mock.users:
            session = entry.get("logged_in")
            if session:
                self.add(session["id"], entry["name"], managed=session.get("managed", True),
                         remote=session.get("remote", False), quiet=True,
                         kind=session.get("type", "wayland"))

    def add(self, session_id, user, managed, klass="user", service="gdm-password",
            active=False, remote=False, quiet=False, kind="wayland"):
        self.sessions[session_id] = {"user": user, "class": klass, "service": service,
                                     "active": active, "remote": remote, "managed": managed,
                                     "type": kind, "state": "active" if active else "online"}
        self.registrations[session_id] = self.bus.register_object(
            self.path(session_id), self.node.lookup_interface(LOGIND_SESSION),
            None, self.session_property, None)
        if not quiet:
            self.bus.emit_signal(None, LOGIND_PATH, LOGIND_MANAGER, "SessionNew",
                                 GLib.Variant("(so)", (session_id, self.path(session_id))))
            self.mock.log.record(kind="logind", member="SessionNew", args=[session_id, user])

    def remove(self, session_id):
        if self.sessions.pop(session_id, None) is None:
            return
        self.bus.unregister_object(self.registrations.pop(session_id))
        self.bus.emit_signal(None, LOGIND_PATH, LOGIND_MANAGER, "SessionRemoved",
                             GLib.Variant("(so)", (session_id, self.path(session_id))))
        self.mock.log.record(kind="logind", member="SessionRemoved", args=[session_id])

    def set_active(self, session_id, active):
        session = self.sessions.get(session_id)
        if session is None or session["active"] == active:
            return
        session["active"] = active
        session["state"] = "active" if active else "online"
        changed = {"Active": GLib.Variant("b", active),
                   "State": GLib.Variant("s", session["state"])}
        self.bus.emit_signal(None, self.path(session_id), "org.freedesktop.DBus.Properties",
                             "PropertiesChanged",
                             GLib.Variant("(sa{sv}as)", (LOGIND_SESSION, changed, [])))
        self.mock.log.record(kind="logind", member="PropertiesChanged",
                             args=[session_id, {"Active": active}])

    def session_of(self, user):
        return next((sid for sid, s in self.sessions.items()
                     if s["user"] == user and s["class"] == "user"), None)

    def managed_session_of(self, user):
        return next((sid for sid, s in self.sessions.items()
                     if s["user"] == user and s["class"] == "user" and s["managed"]), None)

    def manager_call(self, _c, sender, _path, iface, member, params, inv):
        def handle(_iface, member, args):
            if member == "ListSessions":
                return GLib.Variant("(a(susso))", ([
                    (sid, self.mock.uid_of(s["user"]), s["user"], "seat0", self.path(sid))
                    for sid, s in self.sessions.items()],))
            if member == "GetSession":
                if args[0] not in self.sessions:
                    raise Refused("org.freedesktop.login1.NoSuchSession", args[0])
                return GLib.Variant("(o)", (self.path(args[0]),))
            if member == "GetSessionByPID":
                return GLib.Variant("(o)", (self.path(self.greeter_id),))
            # The power menu: always available, recorded, never taken.
            if member in ("CanSuspend", "CanReboot", "CanPowerOff"):
                return GLib.Variant("(s)", ("yes",))
            if member in ("Suspend", "Reboot", "PowerOff"):
                return {"interactive": bool(args[0])}
            raise Refused("org.freedesktop.DBus.Error.UnknownMethod", member)
        self.mock.dispatch(sender, iface, member, params, inv, handle)

    def session_property(self, _c, _sender, path, _iface, prop):
        session_id = path.rsplit("/_", 1)[-1]
        s = self.sessions.get(session_id)
        if s is None:
            return None
        values = {
            "Id": GLib.Variant("s", session_id),
            "Name": GLib.Variant("s", s["user"]),
            "Type": GLib.Variant("s", s["type"]),
            "Class": GLib.Variant("s", s["class"]),
            "State": GLib.Variant("s", s["state"]),
            "Remote": GLib.Variant("b", s["remote"]),
            "Active": GLib.Variant("b", s["active"]),
            "Service": GLib.Variant("s", s["service"]),
        }
        return values.get(prop)


class Smartcard:
    """gnome-settings-daemon's smartcard plugin: one PIV token."""

    def __init__(self, mock, bus):
        self.mock = mock
        self.bus = bus
        node = Gio.DBusNodeInfo.new_for_xml(SMARTCARD_XML)
        bus.register_object(SC_PATH, node.lookup_interface("org.freedesktop.DBus.ObjectManager"),
                            self.manager_call, None, None)
        bus.register_object(SC_TOKEN_PATH, node.lookup_interface(SC_TOKEN_IFACE),
                            None, self.token_property, None)

    def props(self):
        return {"Name": GLib.Variant("s", "PIV_II"),
                "Driver": GLib.Variant("o", SC_PATH + "/Manager/Drivers/p11_kit_proxy"),
                "IsInserted": GLib.Variant("b", self.mock.card_inserted),
                "UsedToLogin": GLib.Variant("b", False)}

    def manager_call(self, _c, sender, _path, iface, member, params, inv):
        def handle(_iface, member, _args):
            if member == "GetManagedObjects":
                return GLib.Variant("(a{oa{sa{sv}}})",
                                    ({SC_TOKEN_PATH: {SC_TOKEN_IFACE: self.props()}},))
            raise Refused("org.freedesktop.DBus.Error.UnknownMethod", member)
        self.mock.dispatch(sender, iface, member, params, inv, handle)

    def token_property(self, _c, _sender, _path, _iface, prop):
        return self.props().get(prop)

    def changed(self):
        self.bus.emit_signal(None, SC_TOKEN_PATH, "org.freedesktop.DBus.Properties",
                             "PropertiesChanged",
                             GLib.Variant("(sa{sv}as)", (SC_TOKEN_IFACE,
                                                         {"IsInserted": GLib.Variant(
                                                             "b", self.mock.card_inserted)},
                                                         [])))


class Mock:
    def __init__(self, bus, log, users, secrets, sessions, fallback, server_dir, card_user,
                 card_certs):
        self.bus = bus
        self.log = log
        self.users = users
        self.secrets = secrets
        self.sessions = sessions
        self.fallback_session = fallback
        self.server_dir = server_dir
        self.card_user = card_user
        self.card_certs = card_certs
        self.card_inserted = False
        self.switchable = False
        self.web_login_approved = False
        self.passkey_connected = False
        self.timed_login = None
        self.autologin_user = None
        self.peers = []
        self.reauth_servers = []
        self.session_ids = 41
        self.manager_node = Gio.DBusNodeInfo.new_for_xml(MANAGER_XML)
        self.session_node = Gio.DBusNodeInfo.new_for_xml(SESSION_XML)
        self.accounts_node = Gio.DBusNodeInfo.new_for_xml(ACCOUNTS_XML)
        self.control_node = Gio.DBusNodeInfo.new_for_xml(CONTROL_XML)
        # user object path -> its registration on the bus
        self.user_objects = {}
        self.logind = Logind(self)
        self.smartcard = None
        # gdm_dbus_setup_private_server: unix:dir=/run/gdm/dbus.
        self.server = self.new_server(None)

    def new_server(self, reauth_user):
        observer = Gio.DBusAuthObserver()
        observer.connect("allow-mechanism", lambda _o, mech: mech == "EXTERNAL")
        observer.connect("authorize-authenticated-peer", self.authorize_peer)
        server = Gio.DBusServer.new_sync(
            f"unix:dir={self.server_dir}", Gio.DBusServerFlags.NONE,
            Gio.dbus_generate_guid(), observer, None)
        server.connect("new-connection",
                       lambda _s, conn: self.new_connection(conn, reauth_user))
        server.start()
        return server

    def next_session_id(self):
        self.session_ids += 1
        return self.session_ids

    def entry(self, user):
        return next((u for u in self.users if u["name"] == user), None)

    def uid_of(self, user):
        entry = self.entry(user)
        return entry["uid"] if entry else 60578

    def saved_session(self, user):
        entry = self.entry(user)
        return (entry or {}).get("session") or None

    def second_factor(self, user):
        return bool((self.entry(user) or {}).get("second_factor"))

    def password_expired(self, user):
        return bool((self.entry(user) or {}).get("password_expired"))

    def switchable_selection(self, user):
        return (self.entry(user) or {}).get("switchable")

    def sssd_request(self, user):
        """SSSD's auth-mechanisms request for `user` now: a passkey's
        keyConnected is SetPasskey's."""
        selection = self.switchable_selection(user)
        if selection is None:
            return None
        selection = json.loads(json.dumps(selection))
        for m in selection.get("authSelection", {}).get("mechanisms", {}).values():
            if m.get("role") == "passkey":
                m["keyConnected"] = self.passkey_connected
        return selection

    def sssd_mechanism(self, user, mechanism_id):
        selection = self.switchable_selection(user) or {}
        return selection.get("authSelection", {}).get("mechanisms", {}).get(mechanism_id)

    def check(self, step, conversation, answer):
        user = conversation["user"]
        if step == "password":
            expected = self.secrets.get(user)
        elif step == "code":
            expected = self.secrets.get(f"{user}@code")
        elif step == "new":
            # pam_sss takes any new password but the old one; it is kept
            # (in memory only) for the retype.
            if not answer or answer == self.secrets.get(user):
                return False
            conversation["new"] = answer
            return True
        elif step == "retype":
            expected = conversation.get("new")
        else:  # pin
            if user not in (None, self.card_user):
                return False
            expected = self.secrets.get("smartcard@pin")
        return expected is not None and answer == expected

    def authorize_peer(self, _observer, _stream, credentials):
        # GDM's allow_user_function: only the greeter session's user.
        uid = credentials.get_unix_user() if credentials is not None else -1
        allowed = uid == os.getuid()
        self.log.record(kind="auth", member="authorize-authenticated-peer",
                        args=[uid], outcome="ok" if allowed else "refused")
        return allowed

    def new_connection(self, conn, reauth_user):
        peer = Peer(self, conn, len(self.peers) + 1, reauth_user)
        self.peers.append(peer)
        for iface in (VERIFIER_IFACE, CHOICE_IFACE, JSON_IFACE, GREETER_IFACE):
            conn.register_object(
                SESSION_PATH, self.session_node.lookup_interface(iface),
                lambda c, sender, path, iface, member, params, inv, peer=peer:
                    self.dispatch(peer.number, iface, member, params, inv,
                                  lambda i, m, a: peer.method_call(i, m, a)),
                None, None)
        conn.connect("closed", lambda *_: self.peer_closed(peer))
        self.log.record(kind="connect", member="new-connection", conn=peer.number,
                        args=[reauth_user] if reauth_user else [])
        return True

    def peer_closed(self, peer):
        peer.closed()
        self.log.record(kind="disconnect", member="closed", conn=peer.number)

    def dispatch(self, conn, iface, member, params, inv, handler):
        args = params.unpack() if params is not None else ()
        # Never log an answer.
        if member in ("AnswerQuery", "Reply"):
            shown = [args[0], "<answer>"]
        else:
            shown = list(args)
        try:
            result = handler(iface, member, args)
        except Refused as refused:
            self.log.record(kind="call", interface=iface, member=member, conn=conn,
                            args=shown, outcome=refused.name)
            inv.return_dbus_error(refused.name, str(refused))
            return
        detail = result if isinstance(result, dict) else None
        self.log.record(kind="call", interface=iface, member=member, conn=conn,
                        args=shown, outcome="ok", detail=detail)
        if isinstance(result, GLib.Variant):
            inv.return_value(result)
        else:
            inv.return_value(None)

    # --- the system bus -------------------------------------------------

    def manager_call(self, _c, sender, _path, iface, member, params, inv):
        def handle(_iface, member, args):
            if member == "OpenSession":
                return GLib.Variant("(s)", (self.server.get_client_address(),))
            if member == "OpenReauthenticationChannel":
                (user,) = args
                if self.logind.managed_session_of(user) is None:
                    raise Refused("org.freedesktop.DBus.Error.AccessDenied",
                                  "Login screen only allowed to open reauthentication "
                                  "channels for running sessions")
                server = self.new_server(user)
                self.reauth_servers.append(server)
                return GLib.Variant("(s)", (server.get_client_address(),))
            if member in ("RegisterSession", "RegisterDisplay"):
                return None
            raise Refused("org.gnome.DisplayManager.Manager.Error.NotSupported", member)
        self.dispatch(sender, iface, member, params, inv, handle)

    def manager_property(self, *_args):
        return GLib.Variant("s", "51.1.1")

    def control_call(self, _c, sender, _path, iface, member, params, inv):
        def handle(_iface, member, args):
            if member == "SetSmartcard":
                self.card_inserted = bool(args[0])
                if self.smartcard is not None:
                    self.smartcard.changed()
            elif member == "SetSwitchable":
                self.switchable = bool(args[0])
            elif member == "SetTimedLogin":
                user, delay = args
                self.timed_login = (user, delay) if user else None
                # GDM 51.1.1's BeginAutoLogin admits the AutomaticLogin user.
                self.autologin_user = user or None
            elif member == "ApproveWebLogin":
                self.web_login_approved = bool(args[0])
            elif member == "SetPasskey":
                self.passkey_connected = bool(args[0])
            elif member == "TouchPasskey":
                touched = [(peer, service) for peer in self.peers
                           for service, c in peer.conversations.items()
                           if c["query"] == "touch"]
                if not touched:
                    raise Refused("org.freedesktop.DBus.Error.Failed",
                                  "no passkey waits for a touch")
                for peer, service in touched:
                    peer.conversations[service]["query"] = None
                    GLib.idle_add(peer.after_answer, service, "touch", True)
            elif member == "CloseGreeterChannels":
                for peer in self.peers:
                    if peer.reauth_user is None and not peer.conn.is_closed():
                        peer.conn.close(None, None, None)
            elif member == "ActivateGreeterSession":
                self.logind.set_active(self.logind.greeter_id, True)
            elif member == "EndSessions":
                (user,) = args
                for sid in [sid for sid, s in self.logind.sessions.items()
                            if s["user"] == user and s["class"] == "user"]:
                    self.logind.remove(sid)
            elif member == "AddUser":
                name, real_name, uid = args
                if any(u["name"] == name or u["uid"] == uid for u in self.users):
                    raise Refused("org.freedesktop.Accounts.Error.UserExists", name)
                entry = {"name": name, "real_name": real_name, "uid": uid}
                self.users.append(entry)
                self.export_user(entry)
                self.accounts_signal("UserAdded", self.user_path(entry))
            elif member == "DeleteUser":
                (name,) = args
                entry = next((u for u in self.users if u["name"] == name), None)
                if entry is None:
                    raise Refused("org.freedesktop.Accounts.Error.UserDoesNotExist", name)
                self.users.remove(entry)
                path = self.user_path(entry)
                self.bus.unregister_object(self.user_objects.pop(path))
                self.accounts_signal("UserDeleted", path)
            else:
                raise Refused("org.freedesktop.DBus.Error.UnknownMethod", member)
            return None
        self.dispatch(sender, iface, member, params, inv, handle)

    def user_path(self, entry):
        return f"{ACCOUNTS_PATH}/User{entry['uid']}"

    def accounts_signal(self, member, path):
        self.bus.emit_signal(None, ACCOUNTS_PATH, ACCOUNTS_IFACE, member,
                             GLib.Variant("(o)", (path,)))
        self.log.record(kind="signal", interface=ACCOUNTS_IFACE, member=member, args=[path])

    def export_user(self, entry):
        path = self.user_path(entry)
        self.user_objects[path] = self.bus.register_object(
            path, self.accounts_node.lookup_interface(USER_IFACE), None, self.user_property, None)

    def accounts_call(self, _c, sender, path, iface, member, params, inv):
        def handle(_iface, member, args):
            if member == "ListCachedUsers":
                return GLib.Variant("(ao)", ([self.user_path(u) for u in self.users],))
            if member == "FindUserByName":
                for entry in self.users:
                    if entry["name"] == args[0]:
                        return GLib.Variant("(o)", (self.user_path(entry),))
                raise Refused("org.freedesktop.Accounts.Error.Failed", "Failed to look up user")
            raise Refused("org.freedesktop.DBus.Error.UnknownMethod", member)
        self.dispatch(sender, iface, member, params, inv, handle)

    def user_property(self, _c, _sender, path, _iface, prop):
        entry = next(u for u in self.users if self.user_path(u) == path)
        values = {
            "Uid": GLib.Variant("t", entry["uid"]),
            "UserName": GLib.Variant("s", entry["name"]),
            "RealName": GLib.Variant("s", entry.get("real_name", "")),
            "IconFile": GLib.Variant("s", entry.get("icon", "")),
            "SystemAccount": GLib.Variant("b", entry.get("system", False)),
            "Locked": GLib.Variant("b", entry.get("locked", False)),
            "XSession": GLib.Variant("s", entry.get("session", "")),
            "Session": GLib.Variant("s", entry.get("session", "")),
        }
        return values.get(prop)

    def own(self, bus, name):
        reply = bus.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus",
                              "org.freedesktop.DBus", "RequestName",
                              GLib.Variant("(su)", (name, 4)),  # DO_NOT_QUEUE
                              GLib.VariantType("(u)"), Gio.DBusCallFlags.NONE,
                              5000, None)
        if reply.unpack()[0] != 1:  # PRIMARY_OWNER
            raise RuntimeError(f"{name} is already owned")

    def export(self, session_bus):
        self.bus.register_object(MANAGER_PATH, self.manager_node.lookup_interface(MANAGER_IFACE),
                                 self.manager_call, self.manager_property, None)
        self.bus.register_object(ACCOUNTS_PATH, self.accounts_node.lookup_interface(ACCOUNTS_IFACE),
                                 self.accounts_call, None, None)
        self.bus.register_object(CONTROL_PATH, self.control_node.lookup_interface(CONTROL_IFACE),
                                 self.control_call, None, None)
        for entry in self.users:
            self.export_user(entry)
        self.logind.export(self.bus)
        for name in (GDM_NAME, ACCOUNTS_NAME, LOGIND_NAME):
            self.own(self.bus, name)
        if session_bus is not None:
            self.smartcard = Smartcard(self, session_bus)
            self.own(session_bus, SC_NAME)


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--address", required=True,
                        help="private bus address (never the host system bus)")
    parser.add_argument("--server-dir", required=True, help="GDM's /run/gdm/dbus")
    parser.add_argument("--log", required=True, help="JSON-lines call log")
    parser.add_argument("--users", required=True, help="JSON list of AccountsService users")
    parser.add_argument("--secrets", required=True,
                        help="USER:PASSWORD, USER@code:CODE and smartcard@pin:PIN lines (mode 600)")
    parser.add_argument("--sessions", default="tuna,gnome",
                        help="session ids GDM finds installed")
    parser.add_argument("--fallback-session", default="tuna",
                        help="GDM's FallbackSession (TunaOS sets tuna)")
    parser.add_argument("--smartcard-user", default=None,
                        help="the user the smartcard's certificate names")
    parser.add_argument("--smartcard-certs", type=int, default=1,
                        help="certificates on the card (2: a ChoiceList query)")
    args = parser.parse_args()

    with open(args.users, encoding="utf-8") as stream:
        users = json.load(stream)
    for entry in users:
        if entry.get("icon"):
            entry["icon"] = os.path.join(os.path.dirname(os.path.abspath(args.users)), entry["icon"])
    secrets = {}
    with open(args.secrets, encoding="utf-8") as stream:
        for line in stream:
            user, sep, password = line.rstrip("\n").partition(":")
            if sep:
                secrets[user] = password
    conn = Gio.DBusConnection.new_for_address_sync(
        args.address,
        Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT
        | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION,
        None, None)
    try:
        session_bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
    except GLib.Error:
        session_bus = None
    log = Log(args.log)
    mock = Mock(conn, log, users, secrets, set(args.sessions.split(",")),
                args.fallback_session, args.server_dir, args.smartcard_user,
                args.smartcard_certs)
    try:
        mock.export(session_bus)
    except RuntimeError as error:
        print(f"tuna-gdm-mock: {error}", file=sys.stderr)
        return 1
    log.record(kind="ready", member="RequestName", args=[os.getpid(), conn.get_unique_name()])

    loop = GLib.MainLoop()
    conn.connect("closed", lambda *_: loop.quit())
    for signum in (signal.SIGTERM, signal.SIGINT):
        GLib.unix_signal_add(GLib.PRIORITY_DEFAULT, signum, lambda: (loop.quit(), False)[1])
    print(f"tuna-gdm-mock: ready {conn.get_unique_name()} {mock.server.get_client_address()}",
          flush=True)
    loop.run()
    mock.server.stop()
    for server in mock.reauth_servers:
        server.stop()
    log.record(kind="exit", member="exit")
    return 0


if __name__ == "__main__":
    sys.exit(main())
