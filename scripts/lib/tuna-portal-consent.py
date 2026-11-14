#!/usr/bin/python3
"""Activate the actual GNOME portal consent picker through AT-SPI.

GObject-introspected Atspi (at-spi2-core), not pyatspi: published images ship
Orca 51 without python-atspi, and the journey runs on the unmodified image."""
import json
import sys
import subprocess
from pathlib import Path
import time
import gi
gi.require_version("Atspi", "2.0")
from gi.repository import Atspi, Gio, GLib
out, decision = sys.argv[1:3]
remote = len(sys.argv) > 3 and sys.argv[3] == "remote"
external_capture = "--external-capture" in sys.argv[3:]
kind = sys.argv[sys.argv.index("--kind") + 1] if "--kind" in sys.argv else None
request_exit = Path(sys.argv[sys.argv.index("--request-exit") + 1]) if "--request-exit" in sys.argv else None

def require_pending_request():
    if request_exit is not None and request_exit.exists():
        raise RuntimeError("native requester exited; refusing a stale portal picker")

def button_names():
    if kind == "screenshot":
        return ("Deny",) if decision == "cancel" else ("Allow",)
    if kind == "screencast":
        return ("Cancel",) if decision == "cancel" else ("Share",)
    if kind is not None:
        raise RuntimeError("unknown portal consent kind: " + kind)
    return ("Cancel", "Deny") if decision == "cancel" else ("Share", "Allow")


def require_visible_session():
    # AT-SPI's STATE_SHOWING describes the dialog's own widget tree, not
    # whether the compositor's lock curtain covers it. In a real installed
    # session, never activate a consent control hidden behind that curtain.
    if not external_capture:
        return
    bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
    active = bus.call_sync(
        "org.gnome.ScreenSaver", "/org/gnome/ScreenSaver",
        "org.gnome.ScreenSaver", "GetActive", None, None,
        Gio.DBusCallFlags.NONE, 5000, None).unpack()
    print("portal session before consent: " + json.dumps({"locked": active != (False,)}),
          flush=True)
    if active != (False,):
        raise RuntimeError("portal consent is hidden by a locked session")

def has(acc, state):
    return acc.get_state_set().contains(state)

def walk(acc, nodes, controls):
    try:
        showing = has(acc, Atspi.StateType.SHOWING)
        nodes.append({"role": acc.get_role_name(), "name": acc.get_name(), "showing": showing})
        if showing: controls.append(acc)
        for i in range(acc.get_child_count()): walk(acc.get_child_at_index(i), nodes, controls)
    except Exception:
        pass
deadline = time.monotonic() + 30
while time.monotonic() < deadline:
    require_pending_request()
    desktop = Atspi.get_desktop(0)
    for i in range(desktop.get_child_count()):
        app = desktop.get_child_at_index(i)
        # Backend restart can remove an application after childCount was read.
        # Keep observing within the existing deadline; absence is not consent.
        if app is None:
            continue
        if "portal" not in (app.get_name() or "").lower(): continue
        nodes, controls = [], []
        walk(app, nodes, controls)
        buttons = [a for a in controls if a.get_role_name() in ("push button", "button")
                   and a.get_name() in button_names()]
        if not buttons: continue
        with open(out, "w") as f: json.dump(nodes, f, indent=2)
        if not external_capture:
            subprocess.run(["scrot", str(Path(out).with_suffix(".png"))], check=True)
        button = buttons[0]
        if remote and decision != "cancel":
            interaction = [control for control in controls
                           if control.get_role_name() == "switch"
                           and control.get_name() == "Allow Remote Interaction"]
            if not interaction:
                raise RuntimeError("remote interaction consent switch missing")
            actionable = []
            for control in interaction:
                try:
                    action = control.get_action_iface()
                    if action is not None and action.get_n_actions():
                        actionable.append(control)
                except Exception:
                    pass
            if not actionable:
                raise RuntimeError("remote interaction switch has no action")
            control = actionable[0]
            control.clear_cache()
            if not has(control, Atspi.StateType.CHECKED):
                control.get_action_iface().do_action(0)
            enabled_deadline = time.monotonic() + 3
            while time.monotonic() < enabled_deadline:
                while GLib.MainContext.default().pending():
                    GLib.MainContext.default().iteration(False)
                control.clear_cache()
                if has(control, Atspi.StateType.CHECKED):
                    break
                time.sleep(.05)
            else:
                raise RuntimeError("remote interaction consent was not enabled")
        if decision != "cancel" and not has(button, Atspi.StateType.SENSITIVE):
            for control in controls:
                if control.get_role_name() in ("toggle button", "radio button") and control.get_name() not in ("Windows", "Screens"):
                    try:
                        control.get_action_iface().do_action(0)
                        time.sleep(.2)
                        break
                    except Exception: pass
        if has(button, Atspi.StateType.SENSITIVE):
            button_name = button.get_name()
            require_visible_session()
            require_pending_request()
            button.get_action_iface().do_action(0)
            print("actual portal picker: " + button_name)
            sys.exit(0)
    time.sleep(.1)
nodes, controls = [], []
walk(Atspi.get_desktop(0), nodes, controls)
Path(out).write_text(json.dumps(nodes, indent=2))
if not external_capture:
    subprocess.run(["scrot", str(Path(out).with_suffix(".png"))], check=True)
raise RuntimeError("actual GNOME portal consent picker did not become actionable")
