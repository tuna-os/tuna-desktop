#!/usr/bin/env python3
"""GDM 51's greeter API and AccountsService for scripts/tuna-greeter-proof (#1164).

Usage: tuna-gdm-mock.py --address BUS --server-dir DIR --log FILE.jsonl
                        --users FILE.json --secrets FILE [--sessions ID,ID]
                        [--fallback-session ID]

Owns org.gnome.DisplayManager and org.freedesktop.Accounts on the private
bus at BUS (never the host's system bus) and serves them with GDM 51.1.1's
shapes (daemon/gdm-manager.xml, daemon/gdm-session.xml, daemon/gdm-session.c,
daemon/gdm-session-worker.c, https://gitlab.gnome.org/GNOME/gdm/-/tree/51.1.1)
and AccountsService's (data/org.freedesktop.Accounts{,.User}.xml):

  /org/gnome/DisplayManager/Manager  org.gnome.DisplayManager.Manager
      OpenSession() -> s   a GDBusServer listening on unix:dir=DIR (GDM's
                           /run/gdm/dbus), EXTERNAL only, peers of this uid
                           only; returns its client address
      RegisterSession(), RegisterDisplay()
  peer connection: /org/gnome/DisplayManager/Session
      UserVerifier: EnableExtensions(as), BeginVerificationForUser(s, s),
          AnswerQuery(s, s), Cancel(); signals as GDM emits them
      Greeter: SelectSession(s), StartSessionWhenReady(s, b)
  /org/freedesktop/Accounts  ListCachedUsers() -> ao; users' properties

The login screen's power menu calls logind (on hardware):

  org.freedesktop.login1 /org/freedesktop/login1 (Manager): CanSuspend,
      CanReboot, CanPowerOff -> 'yes'; Suspend(b), Reboot(b), PowerOff(b)
      recorded and otherwise ignored

The proof's hand on GDM and AccountsService:

  /org/tuna/Proof/GdmMock  org.tuna.Proof.GdmMock
      AddUser(s name, s real_name, t uid)  a listed user; UserAdded
      DeleteUser(s name)                   UserDeleted, the object goes
      CloseGreeterChannels()               GDM closes every greeter channel

A user's "icon" in FILE.json is relative to that file's directory.

The gdm-password conversation is pam_unix's through GDM's worker:
BeginVerificationForUser emits ConversationStarted, completes, then
DefaultLanguageNameChanged/DefaultSessionNameChanged (the user's saved
session, none here, so the fallback) and SecretInfoQuery 'Password: '.
The right answer gives VerificationComplete then SessionOpened (gdm-session.c
on_opened); a wrong one Problem 'Sorry, password authentication didn’t
work. Please try again.' (gdm-session-worker.c), VerificationFailed and
ConversationStopped (report_and_stop_conversation). Cancel emits Reset, then
ConversationStopped for each conversation, and forgets the selected session.

Every call and emitted signal is appended to FILE.jsonl as one JSON object
(kind, interface, member, conn, args, outcome, wall and monotonic time).
Answers are never logged: AnswerQuery records the answer's SHA-256 and
whether it was the user's password. On readiness one line is printed:
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
GREETER_IFACE = "org.gnome.DisplayManager.Greeter"
ACCOUNTS_NAME = "org.freedesktop.Accounts"
ACCOUNTS_PATH = "/org/freedesktop/Accounts"
ACCOUNTS_IFACE = "org.freedesktop.Accounts"
USER_IFACE = "org.freedesktop.Accounts.User"
LOGIND_NAME = "org.freedesktop.login1"
LOGIND_PATH = "/org/freedesktop/login1"
LOGIND_MANAGER = "org.freedesktop.login1.Manager"
CONTROL_PATH = "/org/tuna/Proof/GdmMock"
CONTROL_IFACE = "org.tuna.Proof.GdmMock"
PAM_PROMPT = "Password: "
# gdm-session-worker.c get_friendly_error_message for gdm-password.
PASSWORD_FAILED = "Sorry, password authentication didn’t work. Please try again."
# gdm-session.c begin_verification_conversation / GDM 51 service checks.
DAEMON_ONLY = ("gdm-autologin", "gdm-launch-environment")

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

# The power half of logind's Manager (the greeter's quick settings).
LOGIND_POWER_XML = f"""
<node>
  <interface name="{LOGIND_MANAGER}">
    <method name="CanSuspend"><arg name="result" direction="out" type="s"/></method>
    <method name="CanReboot"><arg name="result" direction="out" type="s"/></method>
    <method name="CanPowerOff"><arg name="result" direction="out" type="s"/></method>
    <method name="Suspend"><arg name="interactive" direction="in" type="b"/></method>
    <method name="Reboot"><arg name="interactive" direction="in" type="b"/></method>
    <method name="PowerOff"><arg name="interactive" direction="in" type="b"/></method>
  </interface>
</node>
"""

CONTROL_XML = f"""
<node>
  <interface name="{CONTROL_IFACE}">
    <method name="AddUser">
      <arg name="name" direction="in" type="s"/><arg name="real_name" direction="in" type="s"/>
      <arg name="uid" direction="in" type="t"/>
    </method>
    <method name="DeleteUser"><arg name="name" direction="in" type="s"/></method>
    <method name="CloseGreeterChannels"/>
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


class Peer:
    """One greeter's connection to the session server (one GdmSession's
    UserVerifier and Greeter as that greeter sees them)."""

    def __init__(self, mock, conn, number):
        self.mock = mock
        self.conn = conn
        self.number = number
        # service -> {"user": str, "query": bool}
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
        # and forgets the selected session.
        self.emit(VERIFIER_IFACE, "Reset", None, None)
        for service in list(self.conversations):
            self.stop_conversation(service)
        self.selected_session = None

    def method_call(self, iface, member, args):
        if iface == VERIFIER_IFACE:
            return self.verifier(member, args)
        if iface == GREETER_IFACE:
            return self.greeter(member, args)
        raise Refused("org.freedesktop.DBus.Error.UnknownInterface", iface)

    def verifier(self, member, args):
        if member == "EnableExtensions":
            return None
        if member == "BeginVerificationForUser":
            service, user = args
            if "/" in service or not service.startswith("gdm-") or service in DAEMON_ONLY:
                raise Refused("org.gnome.DisplayManager.Session.Error.InvalidService",
                              f"Service {service} is not allowed")
            if service in self.conversations:
                raise Refused("org.gnome.DisplayManager.Session.Error.Busy",
                              f"Conversation {service} is already running")
            self.conversations[service] = {"user": user, "query": False}
            self.emit(VERIFIER_IFACE, "ConversationStarted", "(s)", (service,))
            # The reply waits until the worker is set up for the user; the
            # defaults follow (gdm_session_select_user), then PAM asks.
            GLib.idle_add(self.after_begin, service, user)
            return None
        if member == "BeginVerification":
            raise Refused("org.gnome.DisplayManager.Session.Error.NotSupported",
                          "the proof's greeter always names the user")
        if member == "AnswerQuery":
            service, answer = args
            conversation = self.conversations.get(service)
            if conversation is None or not conversation["query"]:
                # GDM completes the call and drops an unexpected answer.
                return {"answer_sha256": hashlib.sha256(answer.encode()).hexdigest(),
                        "dropped": True}
            conversation["query"] = False
            expected = self.mock.secrets.get(conversation["user"])
            right = expected is not None and answer == expected
            GLib.idle_add(self.after_answer, service, right)
            return {"answer_sha256": hashlib.sha256(answer.encode()).hexdigest(),
                    "matches_password": right, "user": conversation["user"]}
        if member == "Cancel":
            GLib.idle_add(lambda: (self.reset(), False)[1])
            return None
        raise Refused("org.freedesktop.DBus.Error.UnknownMethod", member)

    def after_begin(self, service, user):
        if service not in self.conversations:
            return GLib.SOURCE_REMOVE
        self.emit(GREETER_IFACE, "DefaultLanguageNameChanged", "(s)", ("en_US.UTF-8",))
        saved = self.mock.saved_session(user)
        self.emit(GREETER_IFACE, "DefaultSessionNameChanged", "(s)",
                  (saved or self.mock.fallback_session,))
        self.conversations[service]["query"] = True
        self.emit(VERIFIER_IFACE, "SecretInfoQuery", "(ss)", (service, PAM_PROMPT))
        return GLib.SOURCE_REMOVE

    def after_answer(self, service, right):
        if service not in self.conversations:
            return GLib.SOURCE_REMOVE
        if right:
            session_id = f"c{self.mock.next_session_id()}"
            self.opened[service] = session_id
            self.running = True
            self.emit(VERIFIER_IFACE, "VerificationComplete", "(s)", (service,))
            self.emit(GREETER_IFACE, "SessionOpened", "(ss)", (service, session_id))
        else:
            self.emit(VERIFIER_IFACE, "Problem", "(ss)", (service, PASSWORD_FAILED))
            self.emit(VERIFIER_IFACE, "VerificationFailed", "(s)", (service,))
            self.stop_conversation(service)
        return GLib.SOURCE_REMOVE

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
            return {"session_opened": self.opened.get(service),
                    "selected_session": self.selected_session or self.mock.fallback_session,
                    "start": bool(start)}
        if member == "GetTimedLoginDetails":
            return GLib.Variant("(bsi)", (False, "", 0))
        if member in ("SelectUser", "BeginAutoLogin", "StopConflictingSession"):
            raise Refused("org.gnome.DisplayManager.Session.Error.NotSupported",
                          f"{member} is outside the proof")
        raise Refused("org.freedesktop.DBus.Error.UnknownMethod", member)

    def closed(self):
        # gdm-session.c on_outside_connection_closed: the conversations of
        # a greeter that went away stop.
        self.conversations.clear()


class LogindPower:
    """logind's power actions: always available, recorded, never taken."""

    AVAILABILITY = {"CanSuspend", "CanReboot", "CanPowerOff"}
    ACTIONS = {"Suspend", "Reboot", "PowerOff"}

    def __init__(self, mock):
        self.mock = mock
        self.node = Gio.DBusNodeInfo.new_for_xml(LOGIND_POWER_XML)

    def handle(self, _iface, member, args):
        if member in self.AVAILABILITY:
            return GLib.Variant("(s)", ("yes",))
        if member in self.ACTIONS:
            return {"interactive": bool(args[0])}
        raise Refused("org.freedesktop.DBus.Error.UnknownMethod", member)

    def export(self, bus):
        bus.register_object(
            LOGIND_PATH, self.node.lookup_interface(LOGIND_MANAGER),
            lambda _c, sender, _p, iface, member, params, inv:
                self.mock.dispatch(sender, iface, member, params, inv, self.handle),
            None, None)


class Mock:
    def __init__(self, bus, log, users, secrets, sessions, fallback, server_dir):
        self.bus = bus
        self.log = log
        self.users = users
        self.secrets = secrets
        self.sessions = sessions
        self.fallback_session = fallback
        self.peers = []
        self.session_ids = 41
        self.manager_node = Gio.DBusNodeInfo.new_for_xml(MANAGER_XML)
        self.session_node = Gio.DBusNodeInfo.new_for_xml(SESSION_XML)
        self.accounts_node = Gio.DBusNodeInfo.new_for_xml(ACCOUNTS_XML)
        self.control_node = Gio.DBusNodeInfo.new_for_xml(CONTROL_XML)
        # user object path -> its registration on the bus
        self.user_objects = {}
        observer = Gio.DBusAuthObserver()
        observer.connect("allow-mechanism", lambda _o, mech: mech == "EXTERNAL")
        observer.connect("authorize-authenticated-peer", self.authorize_peer)
        # gdm_dbus_setup_private_server: unix:dir=/run/gdm/dbus.
        self.server = Gio.DBusServer.new_sync(
            f"unix:dir={server_dir}", Gio.DBusServerFlags.NONE,
            Gio.dbus_generate_guid(), observer, None)
        self.server.connect("new-connection", self.new_connection)
        self.server.start()

    def next_session_id(self):
        self.session_ids += 1
        return self.session_ids

    def saved_session(self, user):
        for entry in self.users:
            if entry["name"] == user:
                return entry.get("session") or None
        return None

    def authorize_peer(self, _observer, _stream, credentials):
        # GDM's allow_user_function: only the greeter session's user.
        uid = credentials.get_unix_user() if credentials is not None else -1
        allowed = uid == os.getuid()
        self.log.record(kind="auth", member="authorize-authenticated-peer",
                        args=[uid], outcome="ok" if allowed else "refused")
        return allowed

    def new_connection(self, _server, conn):
        peer = Peer(self, conn, len(self.peers) + 1)
        self.peers.append(peer)
        for iface in (VERIFIER_IFACE, GREETER_IFACE):
            conn.register_object(
                SESSION_PATH, self.session_node.lookup_interface(iface),
                lambda c, sender, path, iface, member, params, inv, peer=peer:
                    self.dispatch(peer.number, iface, member, params, inv,
                                  lambda i, m, a: peer.method_call(i, m, a)),
                None, None)
        conn.connect("closed", lambda *_: self.peer_closed(peer))
        self.log.record(kind="connect", member="new-connection", conn=peer.number)
        return True

    def peer_closed(self, peer):
        peer.closed()
        self.log.record(kind="disconnect", member="closed", conn=peer.number)

    def dispatch(self, conn, iface, member, params, inv, handler):
        args = params.unpack() if params is not None else ()
        # Never log an answer.
        shown = [args[0], "<answer>"] if member == "AnswerQuery" else list(args)
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
        def handle(_iface, member, _args):
            if member == "OpenSession":
                return GLib.Variant("(s)", (self.server.get_client_address(),))
            if member in ("RegisterSession", "RegisterDisplay"):
                return None
            raise Refused("org.gnome.DisplayManager.Manager.Error.NotSupported", member)
        self.dispatch(sender, iface, member, params, inv, handle)

    def manager_property(self, *_args):
        return GLib.Variant("s", "51.1.1")

    def user_path(self, entry):
        return f"{ACCOUNTS_PATH}/User{entry['uid']}"

    def control_call(self, _c, sender, _path, iface, member, params, inv):
        def handle(_iface, member, args):
            if member == "AddUser":
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
            elif member == "CloseGreeterChannels":
                # GDM restarts its session server (gdm-session.c dropping
                # the greeter's connection): every channel closes.
                for peer in self.peers:
                    if not peer.conn.is_closed():
                        peer.conn.close(None, None, None)
            else:
                raise Refused("org.freedesktop.DBus.Error.UnknownMethod", member)
            return None
        self.dispatch(sender, iface, member, params, inv, handle)

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

    def export(self):
        self.bus.register_object(MANAGER_PATH, self.manager_node.lookup_interface(MANAGER_IFACE),
                                 self.manager_call, self.manager_property, None)
        self.bus.register_object(ACCOUNTS_PATH, self.accounts_node.lookup_interface(ACCOUNTS_IFACE),
                                 self.accounts_call, None, None)
        self.bus.register_object(CONTROL_PATH, self.control_node.lookup_interface(CONTROL_IFACE),
                                 self.control_call, None, None)
        for entry in self.users:
            self.export_user(entry)
        LogindPower(self).export(self.bus)
        for name in (GDM_NAME, ACCOUNTS_NAME, LOGIND_NAME):
            reply = self.bus.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus",
                                       "org.freedesktop.DBus", "RequestName",
                                       GLib.Variant("(su)", (name, 4)),  # DO_NOT_QUEUE
                                       GLib.VariantType("(u)"), Gio.DBusCallFlags.NONE,
                                       5000, None)
            if reply.unpack()[0] != 1:  # PRIMARY_OWNER
                raise RuntimeError(f"{name} is already owned")


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--address", required=True,
                        help="private bus address (never the host system bus)")
    parser.add_argument("--server-dir", required=True, help="GDM's /run/gdm/dbus")
    parser.add_argument("--log", required=True, help="JSON-lines call log")
    parser.add_argument("--users", required=True, help="JSON list of AccountsService users")
    parser.add_argument("--secrets", required=True, help="USER:PASSWORD lines (mode 600)")
    parser.add_argument("--sessions", default="tuna,gnome",
                        help="session ids GDM finds installed")
    parser.add_argument("--fallback-session", default="tuna",
                        help="GDM's FallbackSession (TunaOS sets tuna)")
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
    log = Log(args.log)
    mock = Mock(conn, log, users, secrets, set(args.sessions.split(",")),
                args.fallback_session, args.server_dir)
    try:
        mock.export()
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
    log.record(kind="exit", member="exit")
    return 0


if __name__ == "__main__":
    sys.exit(main())
