#!/usr/bin/python3
"""Activate the actual GNOME portal consent picker through AT-SPI."""
import json
import sys
import subprocess
from pathlib import Path
import time
import pyatspi
from gi.repository import Gio, GLib
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

def walk(acc, nodes, controls):
    try:
        showing = acc.getState().contains(pyatspi.STATE_SHOWING)
        nodes.append({"role": acc.getRoleName(), "name": acc.name, "showing": showing})
        if showing: controls.append(acc)
        for i in range(acc.childCount): walk(acc.getChildAtIndex(i), nodes, controls)
    except Exception:
        pass
deadline = time.monotonic() + 30
while time.monotonic() < deadline:
    require_pending_request()
    desktop = pyatspi.Registry.getDesktop(0)
    for i in range(desktop.childCount):
        app = desktop.getChildAtIndex(i)
        # Backend restart can remove an application after childCount was read.
        # Keep observing within the existing deadline; absence is not consent.
        if app is None:
            continue
        if "portal" not in (app.name or "").lower(): continue
        nodes, controls = [], []
        walk(app, nodes, controls)
        buttons = [a for a in controls if a.getRoleName() in ("push button", "button")
                   and a.name in button_names()]
        if not buttons: continue
        with open(out, "w") as f: json.dump(nodes, f, indent=2)
        if not external_capture:
            subprocess.run(["scrot", str(Path(out).with_suffix(".png"))], check=True)
        button = buttons[0]
        if remote and decision != "cancel":
            interaction = [control for control in controls
                           if control.getRoleName() == "switch"
                           and control.name == "Allow Remote Interaction"]
            if not interaction:
                raise RuntimeError("remote interaction consent switch missing")
            actionable = []
            for control in interaction:
                try:
                    if control.queryAction().nActions:
                        actionable.append(control)
                except Exception:
                    pass
            if not actionable:
                raise RuntimeError("remote interaction switch has no action")
            control = actionable[0]
            control.clear_cache()
            if not control.getState().contains(pyatspi.STATE_CHECKED):
                control.queryAction().doAction(0)
            enabled_deadline = time.monotonic() + 3
            while time.monotonic() < enabled_deadline:
                while GLib.MainContext.default().pending():
                    GLib.MainContext.default().iteration(False)
                control.clear_cache()
                if control.getState().contains(pyatspi.STATE_CHECKED):
                    break
                time.sleep(.05)
            else:
                raise RuntimeError("remote interaction consent was not enabled")
        if decision != "cancel" and not button.getState().contains(pyatspi.STATE_SENSITIVE):
            for control in controls:
                if control.getRoleName() in ("toggle button", "radio button") and control.name not in ("Windows", "Screens"):
                    try:
                        control.queryAction().doAction(0)
                        time.sleep(.2)
                        break
                    except Exception: pass
        if button.getState().contains(pyatspi.STATE_SENSITIVE):
            button_name = button.name
            require_visible_session()
            require_pending_request()
            button.queryAction().doAction(0)
            print("actual portal picker: " + button_name)
            sys.exit(0)
    time.sleep(.1)
nodes, controls = [], []
walk(pyatspi.Registry.getDesktop(0), nodes, controls)
Path(out).write_text(json.dumps(nodes, indent=2))
if not external_capture:
    subprocess.run(["scrot", str(Path(out).with_suffix(".png"))], check=True)
raise RuntimeError("actual GNOME portal consent picker did not become actionable")
