#!/usr/bin/python3
"""Exercise the actual installed desktop portal frontend and consent backend."""
import json
import importlib.util
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time
from gi.repository import Gio, GLib
dump_spec = importlib.util.spec_from_file_location("tuna_pipewire_dump", Path(__file__).with_name("tuna-pipewire-dump.py"))
dump_module = importlib.util.module_from_spec(dump_spec)
dump_spec.loader.exec_module(dump_module)
out = Path(sys.argv[1])
mode = sys.argv[2]
bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
responses = {}
def response(conn, sender, path, iface, signal, params, data):
    responses[path] = params.unpack()
bus.signal_subscribe(None, "org.freedesktop.portal.Request", "Response", None, None, Gio.DBusSignalFlags.NONE, response, None)
def call(iface, method, args, path="/org/freedesktop/portal/desktop"):
    return bus.call_sync("org.freedesktop.portal.Desktop", path, iface, method, args, None, Gio.DBusCallFlags.NONE, 30000, None).unpack()
def request(method, signature, args):
    handle, = call("org.freedesktop.portal.ScreenCast", method, GLib.Variant(signature, args))
    if method == "Start": out.with_suffix(".ready").write_text(mode)
    deadline = time.monotonic() + 60
    while handle not in responses and time.monotonic() < deadline:
        GLib.MainContext.default().iteration(False)
        time.sleep(.01)
    if handle not in responses: raise RuntimeError(method + " timed out")
    return responses.pop(handle)
def options(token):
    return {"handle_token": GLib.Variant("s", token)}
opts = options("create")
opts["session_handle_token"] = GLib.Variant("s", "tunaproof" + str(os.getpid()))
code, result = request("CreateSession", "(a{sv})", (opts,))
if code: raise RuntimeError("CreateSession failed " + str(code))
session = result["session_handle"]
opts = options("select")
opts.update({"types": GLib.Variant("u", 1), "multiple": GLib.Variant("b", False), "cursor_mode": GLib.Variant("u", 1)})
code, result = request("SelectSources", "(oa{sv})", (session, opts))
if code: raise RuntimeError("SelectSources failed " + str(code))
code, result = request("Start", "(osa{sv})", (session, "", options("start")))
out.with_suffix(".response.json").write_text(json.dumps({"mode": mode, "response": code, "results": result}, indent=2))
if mode in ("cancel", "locked"):
    if code == 0: raise RuntimeError(mode + " unexpectedly granted capture")
    print(mode + " denied with response " + str(code))
    sys.exit(0)
if code != 0: raise RuntimeError("Start failed " + str(code))
node = result["streams"][0][0]
size = result["streams"][0][1].get("size")
reply, fds = bus.call_with_unix_fd_list_sync("org.freedesktop.portal.Desktop", "/org/freedesktop/portal/desktop", "org.freedesktop.portal.ScreenCast", "OpenPipeWireRemote", GLib.Variant("(oa{sv})", (session, {})), None, Gio.DBusCallFlags.NONE, 30000, None, None)
fd = fds.get(reply.unpack()[0])
head = "pipewiresrc fd=%d path=%s num-buffers=1 ! videoconvert" % (fd, node)
def gst_has(element):
    if shutil.which("gst-inspect-1.0"):
        return subprocess.run(["gst-inspect-1.0", "--exists", element]).returncode == 0
    # Ubuntu images ship the plugins and the typelib but not gstreamer1.0-tools.
    import gi
    gi.require_version("Gst", "1.0")
    from gi.repository import Gst
    Gst.init(None)
    return Gst.ElementFactory.find(element) is not None
def gst_run(*tail):
    """Run `pipewiresrc ! videoconvert ! TAIL` to EOS with gst-launch, or in-process without it."""
    if shutil.which("gst-launch-1.0"):
        subprocess.run(["gst-launch-1.0", "-q"] + head.split() + list(tail), pass_fds=(fd,), check=True, timeout=30)
        return
    import gi
    gi.require_version("Gst", "1.0")
    from gi.repository import Gst
    Gst.init(None)
    pipeline = Gst.parse_launch(head + " " + " ".join(tail))
    try:
        pipeline.set_state(Gst.State.PLAYING)
        message = pipeline.get_bus().timed_pop_filtered(30 * Gst.SECOND, Gst.MessageType.EOS | Gst.MessageType.ERROR)
        if message is None:
            raise RuntimeError("gst pipeline did not reach EOS in 30 s")
        if message.type == Gst.MessageType.ERROR:
            raise RuntimeError("gst pipeline failed: " + str(message.parse_error()))
    finally:
        pipeline.set_state(Gst.State.NULL)
try:
    if gst_has("pngenc"):
        gst_run("!", "pngenc", "!", "filesink", "location=" + str(out))
    else:
        # Gentoo ships pngenc in an optional plugin package; encode the same
        # single raw frame with GdkPixbuf instead of requiring it.
        if not size:
            raise RuntimeError("no pngenc and the stream reports no size for a raw frame")
        width, height = size
        raw = out.with_suffix(".rgb")
        caps = "video/x-raw,format=RGB,width=%d,height=%d" % (width, height)
        gst_run("!", caps, "!", "filesink", "location=" + str(raw))
        import gi
        gi.require_version("GdkPixbuf", "2.0")
        from gi.repository import GdkPixbuf
        stride = (width * 3 + 3) & ~3  # GstVideo row alignment for RGB
        data = raw.read_bytes()
        if len(data) != stride * height:
            raise RuntimeError("raw frame is %d bytes, not %dx%d RGB" % (len(data), width, height))
        GdkPixbuf.Pixbuf.new_from_bytes(GLib.Bytes.new(data), GdkPixbuf.Colorspace.RGB, False, 8,
                                        width, height, stride).savev(str(out), "png", [], [])
        raw.unlink()
finally:
    os.close(fd)
# An unrelated client cannot close the granted frontend session.
other = Gio.DBusConnection.new_for_address_sync(os.environ["DBUS_SESSION_BUS_ADDRESS"], Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION, None, None)
try:
    other.call_sync("org.freedesktop.portal.Desktop", session, "org.freedesktop.portal.Session", "Close", None, None, Gio.DBusCallFlags.NONE, 10000, None)
except GLib.Error:
    pass
else:
    raise RuntimeError("another unique caller closed the granted session")
if mode in ("revoke", "backend-disconnect"):
    out.with_suffix(".active").write_text(str(node))
    deadline = time.monotonic() + 30
    while not out.with_suffix(".revoke").exists() and time.monotonic() < deadline:
        GLib.MainContext.default().iteration(False)
        time.sleep(.01)
    if not out.with_suffix(".revoke").exists():
        raise RuntimeError("runner did not revoke the active grant")
elif mode == "disconnect":
    bus.close_sync(None)
else:
    call("org.freedesktop.portal.Session", "Close", None, session)
revoked_at = time.monotonic()
def capture_node_present():
    """(present, evidence) for the granted node: pw-dump when installed, else `wpctl inspect` (Fedora ships pw-dump only in pipewire-utils)."""
    if shutil.which("pw-dump"):
        raw_dump = subprocess.check_output(["pw-dump", "--no-colors"], timeout=1)
        out.with_suffix(".pw-dump.txt").write_bytes(raw_dump)
        nodes = dump_module.dump_objects(raw_dump)
        return any(item.get("id") == node and item.get("type") == "PipeWire:Interface:Node" for item in nodes), nodes
    probe = subprocess.run(["wpctl", "inspect", str(node)], capture_output=True, text=True, timeout=1)
    first = probe.stdout.splitlines()[0] if probe.stdout else ""
    if probe.returncode == 0 and first.startswith("id %d, type PipeWire:Interface:Node" % node):
        return True, {"method": "wpctl inspect", "node": node, "present": True, "output": first}
    if probe.returncode != 0 and "not found" in probe.stderr:
        return False, {"method": "wpctl inspect", "node": node, "present": False, "stderr": probe.stderr.strip()}
    if probe.returncode == 0 and first.startswith("id %d, type PipeWire:Interface:" % node):
        # PipeWire reuses the freed id (often for this very wpctl client): the granted Node is gone.
        return False, {"method": "wpctl inspect", "node": node, "present": False, "reused_by": first}
    raise RuntimeError("wpctl inspect gave no verdict on node %d (exit %d): %s%s" % (node, probe.returncode, probe.stdout[:200], probe.stderr[:200]))
deadline = time.monotonic() + 2
while time.monotonic() < deadline:
    present, evidence = capture_node_present()
    if time.monotonic() >= deadline:
        raise RuntimeError("PipeWire withdrawal exceeded two seconds")
    if not present:
        out.with_suffix(".revoked.json").write_text(json.dumps(evidence, indent=2))
        print("grant consumed frame from node " + str(node) + "; " + ("lock" if mode == "revoke" else "backend disconnect" if mode == "backend-disconnect" else "client disconnect" if mode == "disconnect" else "Close") + " removed PipeWire node in " + str(round(time.monotonic() - revoked_at, 3)) + "s")
        break
    time.sleep(.05)
else: raise RuntimeError("Close retained PipeWire capture node")
