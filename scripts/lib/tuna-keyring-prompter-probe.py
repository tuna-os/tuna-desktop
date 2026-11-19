#!/usr/bin/python3
"""A gcr system-prompter client for the proofs: GcrSystemPrompt's end of
org.gnome.keyring.internal.Prompter, as gnome-keyring talks to it.

Usage: tuna-keyring-prompter-probe.py MARKER SCENARIO

Progress lines go to MARKER. The secret exchange is keyring_sx_aes.py, an
independent implementation of gcr's.

Scenarios (each opens its own callback object, so several may run):

  password   A password prompt with a message, a description and a choice.
             Notes "shown" once PerformPrompt returned, then "reply yes|no",
             "secret TEXT" and "props NAME=VALUE ..." when the user answers.
  new        A new-password prompt (the dialog adds a confirmation entry);
             the same notes. After the first answer it asks again with a
             warning, as a client does on a wrong password, and notes the
             second answer as "reply2", "secret2".
  confirm    A confirm prompt with its own button labels; notes "reply". It
             then asks again (a plain confirm); the same notes with a "2"
             suffix.
  isolation  Two clients and an eavesdropper at once. Client A runs a
             password prompt ("shown"); client B, another connection, tries
             to perform, answer and stop A's prompt ("b-perform-refused",
             "b-stop-ignored") and queues its own, which must not start
             before A stops ("b-queued"). When the user answers A, A reads
             its password ("a-secret TEXT"); a dbus-monitor on the bus sees
             the exchange but neither the password nor its base64
             ("monitor-clean"), and cannot decrypt it with another key pair
             ("monitor-cannot-decrypt"); then A stops and B gets its turn
             with a different prompter key ("b-ready").
"""
import base64
import os
import re
import subprocess
import sys
import time

import gi

gi.require_version("Gio", "2.0")
from gi.repository import Gio, GLib  # noqa: E402

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import keyring_sx_aes as sx  # noqa: E402

MARKER = sys.argv[1]
SCENARIO = sys.argv[2]
NAME = "org.gnome.keyring.SystemPrompter"
PATH = "/org/gnome/keyring/Prompter"
IFACE = "org.gnome.keyring.internal.Prompter"
CALLBACK = "org.gnome.keyring.internal.Prompter.Callback"
# What the proof types into A's dialog in the isolation scenario.
ISOLATION_PASSWORD = "isolation-secret-7"
CALLBACK_XML = """<node><interface name="%s">
  <method name="PromptReady">
    <arg name="reply" type="s" direction="in"/>
    <arg name="properties" type="a{sv}" direction="in"/>
    <arg name="exchange" type="s" direction="in"/>
  </method>
  <method name="PromptDone"/>
</interface></node>""" % CALLBACK
TIMEOUT = 40


def note(line):
    with open(MARKER, "a", encoding="utf-8") as fh:
        fh.write(line + "\n")


def spin(condition, what):
    ctx = GLib.MainContext.default()
    end = time.monotonic() + TIMEOUT
    while not condition():
        if time.monotonic() > end:
            note("timeout " + what)
            sys.exit(2)
        ctx.iteration(False)
        time.sleep(0.01)


def session_bus():
    return Gio.DBusConnection.new_for_address_sync(
        os.environ["DBUS_SESSION_BUS_ADDRESS"],
        Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION,
        None, None)


class Client:
    """One GcrSystemPrompt."""

    def __init__(self, path, bus=None):
        self.bus = bus or session_bus()
        self.path = path
        self.exchange = sx.Exchange()
        self.ready = []
        self.done = False
        self.prompter_exchanges = []
        info = Gio.DBusNodeInfo.new_for_xml(CALLBACK_XML).lookup_interface(CALLBACK)
        self.bus.register_object(path, info, self._call, None, None)

    def _call(self, conn, sender, path, iface, method, params, invocation):
        if method == "PromptReady":
            reply, props, exchange = params.unpack()
            self.prompter_exchanges.append(exchange)
            self.ready.append((reply, props, exchange))
        elif method == "PromptDone":
            self.done = True
        invocation.return_value(None)

    def call(self, method, params):
        return self.bus.call_sync(NAME, PATH, IFACE, method, params, None,
                                  Gio.DBusCallFlags.NONE, 10000, None)

    def begin(self):
        self.call("BeginPrompting", GLib.Variant("(o)", (self.path,)))

    def next_ready(self, what="PromptReady"):
        spin(lambda: self.ready, what)
        reply, props, exchange = self.ready.pop(0)
        return reply, props, self.exchange.receive(exchange)

    def exchange_text(self):
        # A client that never got its turn has no key: it can send only its
        # public key, which is all an impostor has.
        return self.exchange.send(None) if self.exchange.key else self.exchange.begin()

    def perform(self, kind, props):
        variants = {k: GLib.Variant(t, v) for k, (t, v) in props.items()}
        self.call("PerformPrompt", GLib.Variant(
            "(osa{sv}s)", (self.path, kind, variants, self.exchange_text())))

    def stop(self):
        self.call("StopPrompting", GLib.Variant("(o)", (self.path,)))


def props_text(props):
    return " ".join("%s=%s" % (k, props[k]) for k in sorted(props))


def prompt(client, kind, props, suffix=""):
    client.perform(kind, props)
    note("shown" + suffix)
    reply, answered, secret = client.next_ready("the user's answer")
    note("reply%s %s" % (suffix, reply))
    if secret is not None:
        note("secret%s %s" % (suffix, secret.decode()))
    note("props%s %s" % (suffix, props_text(answered)))
    return reply


def scenario_password():
    client = Client("/org/tuna/proof/Prompt1")
    client.begin()
    client.next_ready("the first PromptReady")
    note("begun")
    prompt(client, "password", {
        "message": ("s", "Unlock Login keyring"),
        "description": ("s", "An application wants access to the keyring “Login”, "
                             "but it is locked."),
        "choice-label": ("s", "_Automatically unlock this keyring whenever I'm logged in"),
        "continue-label": ("s", "_Unlock"),
    })
    client.stop()
    spin(lambda: client.done, "PromptDone")
    note("done")


def scenario_new():
    client = Client("/org/tuna/proof/Prompt2")
    client.begin()
    client.next_ready("the first PromptReady")
    note("begun")
    prompt(client, "password", {
        "message": ("s", "Choose password for new keyring"),
        "description": ("s", "An application wants to create a new keyring called “Proof”."),
        "password-new": ("b", True),
    })
    prompt(client, "password", {
        "message": ("s", "Choose password for new keyring"),
        "warning": ("s", "The passwords were too weak. Try again."),
        "password-new": ("b", True),
    }, suffix="2")
    client.stop()
    spin(lambda: client.done, "PromptDone")
    note("done")


def scenario_confirm():
    client = Client("/org/tuna/proof/Prompt3")
    client.begin()
    client.next_ready("the first PromptReady")
    note("begun")
    prompt(client, "confirm", {
        "message": ("s", "Replace keyring?"),
        "description": ("s", "An application wants to replace the keyring “Proof”."),
        "continue-label": ("s", "_Replace"),
        "cancel-label": ("s", "_Keep"),
    })
    prompt(client, "confirm", {
        "message": ("s", "Delete keyring?"),
    }, suffix="2")
    client.stop()
    spin(lambda: client.done, "PromptDone")
    note("done")


def scenario_isolation():
    password = ISOLATION_PASSWORD
    monitor_path = os.environ.get("TUNA_PROOF_DBUS_MONITOR", "dbus-monitor")
    monitor = subprocess.Popen([monitor_path, "--session"], stdout=subprocess.PIPE,
                               stderr=subprocess.DEVNULL, text=True)
    # Let the monitor attach before the traffic starts.
    time.sleep(1.0)
    note("monitoring")
    a = Client("/org/tuna/proof/A")
    a.begin()
    a.next_ready("A's first PromptReady")
    a.perform("password", {"message": ("s", "Isolation: A's prompt")})
    note("shown")

    # B names A's callback path: another callback of its own, not begun.
    b = Client("/org/tuna/proof/A")
    try:
        b.perform("password", {"message": ("s", "B takes over")})
    except GLib.Error as e:
        if "Not begun prompting" in e.message:
            note("b-perform-refused")
        else:
            note("b-perform-other-error " + e.message)
    else:
        note("b-perform-ACCEPTED")
    b.stop()
    note("b-stop-ignored")
    b.begin()
    time.sleep(1.0)
    GLib.MainContext.default().iteration(False)
    note("b-queued" if not b.ready else "b-GOT-A-TURN")

    # The user answers A in the dialog.
    reply, _, secret = a.next_ready("A's answer")
    note("a-reply " + reply)
    note("a-secret " + (secret.decode() if secret is not None else "NONE"))
    a.stop()
    spin(lambda: a.done, "A's PromptDone")
    b_reply, _, _ = b.next_ready("B's turn")
    note("b-ready" if b_reply == "" else "b-reply " + b_reply)
    first_a = a.prompter_exchanges[0]
    first_b = b.prompter_exchanges[0]
    note("b-own-key" if sx.parse(first_a)["public"] != sx.parse(first_b)["public"]
         else "b-SAME-KEY")
    b.stop()
    spin(lambda: b.done, "B's PromptDone")

    time.sleep(0.5)
    monitor.terminate()
    traffic = monitor.stdout.read()
    seen_exchange = "sx-aes-1" in traffic and "secret=" in traffic
    plain = password in traffic or base64.b64encode(password.encode()).decode() in traffic
    note("monitor-clean" if seen_exchange and not plain
         else "monitor-DIRTY seen_exchange=%s plain=%s" % (seen_exchange, plain))
    # The eavesdropper's best attempt: the prompter's public key and the
    # ciphertext are on the bus; B's own private key is not A's.
    blocks = re.findall(r"secret=([A-Za-z0-9+/=]+)\s+iv=([A-Za-z0-9+/=]+)", traffic)
    publics = re.findall(r"public=([A-Za-z0-9+/=]+)", traffic)
    cannot = bool(blocks) and bool(publics)
    for cipher, iv in blocks:
        for public in publics:
            guess = sx.Exchange()
            try:
                got = guess.receive("[sx-aes-1]\npublic=%s\nsecret=%s\niv=%s\n" % (public, cipher, iv))
            except (ValueError, KeyError, IndexError):
                continue
            if got is not None and password.encode() in got:
                cannot = False
    note("monitor-cannot-decrypt" if cannot else "monitor-DECRYPTED")
    note("a-secret-expected " + password)


SCENARIOS = {"password": scenario_password, "new": scenario_new,
             "confirm": scenario_confirm, "isolation": scenario_isolation}

if __name__ == "__main__":
    if SCENARIO not in SCENARIOS:
        sys.exit("unknown scenario " + SCENARIO)
    SCENARIOS[SCENARIO]()
