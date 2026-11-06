"""Independent observations for the seven native network/utility consumers.

The loopback HTTP server is ordinary browser *content*, never a replacement for
WebKit, GeoClue, libgweather, a portal, or a map/forecast provider. No operation in
this module launches a product app or writes its settings/permission grants.
"""
from __future__ import annotations

import hashlib
import html
import http.server
import json
import os
from pathlib import Path
import re
import sqlite3
import subprocess
import sys
import time
import urllib.parse
import urllib.request
import xml.etree.ElementTree as ET


APPS = {
    "org.gnome.Maps": ("gnome-maps", ("last-viewed-location", "zoom-level")),
    "org.gnome.Weather": ("gnome-weather", ("locations",)),
    "org.gnome.Epiphany": ("epiphany", ("restore-session-policy",)),
    "org.gnome.Characters": ("gnome-characters", ("recent-characters",)),
    "org.gnome.clocks": ("gnome-clocks", ("world-clocks", "alarms", "timers")),
    "org.gnome.Tour": ("gnome-tour", ()),
    "org.gnome.Yelp": ("yelp", ()),
}


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def digest(data):
    return hashlib.sha256(data).hexdigest()


def _json_file(path):
    return json.loads(path.read_text())


def _owned_write(context, relative, content):
    path = context.path(relative)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content)
    os.chown(path, context.uid, context.owner.pw_gid)
    os.chown(path.parent, context.uid, context.owner.pw_gid)
    return path


def _dbus_owner(bus, name):
    from gi.repository import Gio, GLib
    owned = bus.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus",
                          "org.freedesktop.DBus", "NameHasOwner",
                          GLib.Variant("(s)", (name,)), None,
                          Gio.DBusCallFlags.NONE, 5000, None).unpack()[0]
    if not owned:
        return {"name": name, "available": False, "distinction": "no-bus-owner"}
    unique = bus.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus",
                          "org.freedesktop.DBus", "GetNameOwner",
                          GLib.Variant("(s)", (name,)), None,
                          Gio.DBusCallFlags.NONE, 5000, None).unpack()[0]
    pid = bus.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus",
                       "org.freedesktop.DBus", "GetConnectionUnixProcessID",
                       GLib.Variant("(s)", (unique,)), None,
                       Gio.DBusCallFlags.NONE, 5000, None).unpack()[0]
    proc = Path("/proc") / str(pid)
    exe = (proc / "exe").resolve(strict=True)
    stat = (proc / "stat").read_text().rsplit(")", 1)[1].split()
    package = subprocess.run(["/usr/bin/pacman", "-Qo", str(exe)], capture_output=True,
                             text=True, check=True, timeout=10).stdout.strip()
    return {"name": name, "available": True, "unique_owner": unique,
            "pid": pid, "uid": proc.stat().st_uid, "start_ticks": int(stat[19]),
            "exe": str(exe), "exe_sha256": digest(exe.read_bytes()), "package": package}


def _user_state(app):
    import gi
    gi.require_version("Gio", "2.0")
    from gi.repository import Gio, GLib
    result = {"settings": {}}
    if APPS[app][1]:
        settings = Gio.Settings.new(app)
        result["settings"] = {key: settings.get_value(key).print_(True)
                              for key in APPS[app][1]}
        result["values"] = {key: settings.get_value(key).unpack() for key in APPS[app][1]}
        if app == "org.gnome.clocks":
            import gi
            gi.require_version("GWeather", "4.0")
            from gi.repository import GWeather
            world = GWeather.Location.get_world()
            require(world is not None, "unavailable-content: installed gweather-locations world database cannot be loaded")
            result["world_locations"] = []
            clocks = settings.get_value("world-clocks")
            for index in range(clocks.n_children()):
                value = clocks.get_child_value(index).lookup_value("location", None)
                require(value is not None, "persisted world clock lacks location payload")
                location = world.deserialize(value)
                require(location is not None, "persisted world clock cannot be deserialized")
                result["world_locations"].append({"name": location.get_name(), "timezone": location.get_timezone_str()})
            bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
            reply = bus.call_sync("org.freedesktop.portal.Desktop", "/org/freedesktop/portal/desktop",
                "org.freedesktop.portal.Settings", "Read", GLib.Variant("(ss)", ("org.gnome.desktop.interface", "clock-format")),
                None, Gio.DBusCallFlags.NONE, 5000, None)
            value = reply.get_child_value(0).get_variant()
            while value.get_type_string() == "v":
                value = value.get_variant()
            result["clock_format"] = value.get_string()
            result["settings_portal"] = _dbus_owner(bus, "org.freedesktop.portal.Desktop")
            require(result["settings_portal"]["available"] and result["clock_format"] in ("12h", "24h"),
                    "unavailable-service: real clock-format Settings portal missing or invalid")
    if app == "org.gnome.Yelp":
        settings = Gio.Settings.new("org.gnome.yelp")
        result["settings"] = {key: settings.get_value(key).print_(True)
                              for key in ("zoom-level",)}
    if app in ("org.gnome.Maps", "org.gnome.Weather"):
        settings = Gio.Settings.new("org.gnome.system.location")
        result["location_master"] = settings.get_boolean("enabled")
        bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
        try:
            value = bus.call_sync("org.freedesktop.impl.portal.PermissionStore",
                                  "/org/freedesktop/impl/portal/PermissionStore",
                                  "org.freedesktop.impl.portal.PermissionStore", "Lookup",
                                  GLib.Variant("(ss)", ("location", "location")), None,
                                  Gio.DBusCallFlags.NONE, 5000, None).unpack()
            result["location_permissions"] = value[0]
        except GLib.Error as error:
            remote = Gio.DBusError.get_remote_error(error)
            if remote != "org.freedesktop.portal.Error.NotFound":
                raise
            result["location_permissions"] = {}
            result["permission_lookup"] = {"error": remote, "message": str(error)}
        result["permission_service"] = _dbus_owner(bus, "org.freedesktop.impl.portal.PermissionStore")
        require(result["permission_service"]["available"], "unavailable-service: real PermissionStore has no owner after actual Lookup")
    return result


LOCATION_APPS = ("org.gnome.Maps", "org.gnome.Weather")


def _permission_source():
    from gi.repository import Gio, GLib
    observed = _user_state("org.gnome.Maps")
    bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
    owners = {}
    for app in LOCATION_APPS:
        owners[app] = bus.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus",
            "org.freedesktop.DBus", "NameHasOwner", GLib.Variant("(s)", (app,)), None,
            Gio.DBusCallFlags.NONE, 5000, None).unpack()[0]
    require(not any(owners.values()), "permission source must be captured before either native location frontend is launched")
    return {"entries": {app: observed["location_permissions"].get(app) for app in LOCATION_APPS},
            "frontend_name_owners": owners, "permission_service": observed["permission_service"]}


def _restore_user_permissions(original):
    from gi.repository import Gio, GLib
    require(isinstance(original, dict) and set(original) == set(LOCATION_APPS), "cleanup may touch only original Maps/Weather entries")
    for value in original.values():
        require(value is None or (isinstance(value, list) and len(value) <= 16 and
                all(isinstance(item, str) and len(item) <= 256 for item in value)), "malformed saved original location permissions")
    before = _user_state("org.gnome.Maps")
    bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
    changes = []
    for app in LOCATION_APPS:
        current = before["location_permissions"].get(app)
        if current == original[app]:
            changes.append({"app": app, "operation": "already-exact-original", "observed": current})
            continue
        if original[app] is None:
            method = "DeletePermission"
            parameters = GLib.Variant("(sss)", ("location", "location", app))
        else:
            method = "SetPermission"
            parameters = GLib.Variant("(sbssas)", ("location", True, "location", app, original[app]))
        bus.call_sync("org.freedesktop.impl.portal.PermissionStore",
            "/org/freedesktop/impl/portal/PermissionStore", "org.freedesktop.impl.portal.PermissionStore",
            method, parameters, None, Gio.DBusCallFlags.NONE, 5000, None)
        changes.append({"app": app, "operation": method, "restored_original": original[app]})
    after = _user_state("org.gnome.Maps")
    require({app: after["location_permissions"].get(app) for app in LOCATION_APPS} == original,
            "cleanup failed to restore exact original permission absence/precision/timestamp")
    require({app: value for app, value in before["location_permissions"].items() if app not in LOCATION_APPS} ==
            {app: value for app, value in after["location_permissions"].items() if app not in LOCATION_APPS},
            "permission cleanup changed an unowned application's entry")
    return {"purpose": "cleanup-only-exact-prelaunch-restoration", "before": before, "after": after, "changes": changes}


def _permissions(operation, context):
    saved = Path("/run") / ("tuna-core-web-permissions-" + context.nonce + ".json")
    if operation == "permission-save":
        require(not saved.exists() and not saved.is_symlink(), "prelaunch permission source already exists; never overwrite it")
        source = json.loads(context.user(["python3", str(Path(__file__).resolve()), "--permission-source"]).stdout)
        require(source["permission_service"]["uid"] == context.uid, "prelaunch PermissionStore belongs to wrong original user")
        receipt = {"nonce": context.nonce, "uid": context.uid, "identity": context.identity(), "source": source}
        descriptor = os.open(saved, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
        with os.fdopen(descriptor, "w") as stream:
            json.dump(receipt, stream)
        return receipt
    require(saved.is_file() and not saved.is_symlink(), "cleanup refuses to invent a missing prelaunch permission source")
    metadata = saved.stat()
    require(metadata.st_uid == 0 and metadata.st_mode & 0o777 == 0o600, "prelaunch permission source is not root-only")
    receipt = _json_file(saved)
    require(receipt["nonce"] == context.nonce and receipt["uid"] == context.uid and
            not any(receipt["source"]["frontend_name_owners"].values()), "invalid before-first-launch permission source")
    restored = json.loads(context.user(["python3", str(Path(__file__).resolve()), "--restore-permissions",
                                       json.dumps(receipt["source"]["entries"])]).stdout)
    require(restored["after"]["permission_service"]["exe_sha256"] == receipt["source"]["permission_service"]["exe_sha256"],
            "permission cleanup reached changed service code")
    saved.unlink()
    return {"saved_prelaunch_source": receipt, "cleanup": restored, "counts_as_application_operation": False}


def _state(arguments, context):
    app = arguments.get("app")
    require(app in APPS, "unknown web utility app")
    value = context.user(["python3", str(Path(__file__).resolve()), "--user-state", app])
    result = json.loads(value.stdout)
    result["identity"] = context.identity()
    result["package"] = context.command(["pacman", "-Q", APPS[app][0]]).stdout.strip()
    if app == "org.gnome.clocks":
        import datetime
        from zoneinfo import ZoneInfo
        now = datetime.datetime.now(ZoneInfo("Asia/Tokyo"))
        result["tokyo_time"] = {"12h": now.strftime("%I:%M%p"), "24h": now.strftime("%H:%M")}
        result["observed_unix"] = now.timestamp()
        future = datetime.datetime.now().astimezone() + datetime.timedelta(hours=6)
        result["alarm_fixture_time"] = {"hour": future.hour, "minute": future.minute}
    if app == "org.gnome.Epiphany":
        database = context.home / ".local/share/epiphany/ephy-history.db"
        require(database.is_file(), "missing browser history database after native navigation")
        with sqlite3.connect(database.as_uri() + "?mode=ro", uri=True) as connection:
            rows = connection.execute("SELECT url,title,visit_count FROM urls WHERE url LIKE ? ORDER BY id", ("%" + context.nonce + "%",)).fetchall()
        result["history"] = [{"url": row[0], "title": row[1], "visits": row[2]} for row in rows]
    if app == "org.gnome.Yelp":
        import configparser
        path = context.home / ".config/yelp/yelp.cfg"
        require(path.is_file(), "actual Help document settings file is missing")
        configuration = configparser.ConfigParser(interpolation=None)
        configuration.read(path)
        result["document_settings"] = {section: dict(configuration[section]) for section in configuration.sections()}
        result["document_settings_sha256"] = digest(path.read_bytes())
    if app == "org.gnome.Tour":
        # Tour has no GSettings schema/preferences; record actual per-app files,
        # while the host checks that carousel position really resets on restart.
        result["persistent_files"] = []
        for base in (context.home / ".config", context.home / ".local/share"):
            for name in ("gnome-tour", "org.gnome.Tour"):
                directory = base / name
                if directory.exists():
                    for path in sorted(directory.rglob("*")):
                        if path.is_file() and not path.is_symlink():
                            result["persistent_files"].append({"path": str(path), "sha256": digest(path.read_bytes())})
        result["installed_schemas"] = context.user(["gsettings", "list-schemas"]).stdout.splitlines()
    return result


def _watch_directory(context):
    return Path("/run") / ("tuna-core-web-location-" + context.nonce)


def _watch_control(operation, context):
    directory = _watch_directory(context)
    unit = "tuna-core-web-location-" + context.nonce + ".service"
    if operation == "location-watch-start":
        directory.mkdir(mode=0o700)
        context.command(["systemd-run", "--collect", "--unit=" + unit, "--property=Type=exec",
                         "python3", str(Path(__file__).resolve()), "--location-watch",
                         str(directory), context.nonce, str(context.uid)])
        return {"unit": unit, "directory": str(directory)}
    require(directory.is_dir() and directory.stat().st_uid == 0 and directory.stat().st_mode & 0o777 == 0o700,
            "passive location observer directory is not the owned root fixture")
    failure = directory / "failure.json"
    if operation == "location-watch-ready":
        require(not failure.exists(), "actual passive GeoClue observer failed: " + (failure.read_text() if failure.exists() else ""))
    ready = directory / "ready.json"
    if operation == "location-watch-ready":
        if not ready.is_file():
            return {"ready": False}
        state = _json_file(ready)
        proc = Path("/proc") / str(state["pid"])
        require(state["nonce"] == context.nonce and state["consumer_uid"] == context.uid and proc.stat().st_uid == 0,
                "passive location observer owner/nonce changed")
        require(int((proc / "stat").read_text().rsplit(")", 1)[1].split()[19]) == state["start_ticks"],
                "passive location observer PID reused")
        return {"ready": True, "observer": state}
    require(operation == "location-watch-stop", "unknown passive observer control")
    context.command(["systemctl", "stop", unit])
    return {"unit": unit, "observer": _json_file(ready) if ready.is_file() else None,
            "failure": _json_file(failure) if failure.is_file() else None,
            "purpose": "stopped-passive-observation-only"}


def _location_watch(directory, nonce, consumer_uid):
    """Observe original consumers' private replies; never ask as a fake client.

    A GDBus filter executes on the shared message worker. It ONLY queues a
    message copy. Public credential queries run on a separate connection from
    GLib's main context, avoiding synchronous worker/filter deadlocks.
    """
    import gi
    gi.require_version("Gio", "2.0")
    from gi.repository import Gio, GLib
    require(os.geteuid() == 0 and directory.stat().st_uid == 0 and
            directory.stat().st_mode & 0o777 == 0o700, "location observer requires its root-only fixture")
    require(re.fullmatch(r"[a-zA-Z0-9-]{8,80}", nonce) is not None, "invalid passive observer nonce")
    address = Gio.dbus_address_get_for_bus_sync(Gio.BusType.SYSTEM, None)
    flags = Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION
    control = Gio.DBusConnection.new_for_address_sync(address, flags, None, None)
    monitor = Gio.DBusConnection.new_for_address_sync(address, flags, None, None)
    loop = GLib.MainLoop()
    peers, pending = {}, {}
    count = 0
    failure = []
    stream = (directory / "messages.jsonl").open("a", buffering=1)

    def credentials(name):
        if not name or not name.startswith(":"):
            return None
        if name not in peers:
            try:
                peers[name] = _dbus_owner(control, name)
            except GLib.Error as error:
                if Gio.DBusError.get_remote_error(error) not in ("org.freedesktop.DBus.Error.NameHasNoOwner",
                                                                "org.freedesktop.DBus.Error.UnixProcessIdUnknown"):
                    raise
                peers[name] = {"name": name, "available": False, "distinction": "connection-departed-before-credential-read"}
        return peers[name]

    def consume(message):
        nonlocal count
        try:
            kind = {1: "method_call", 2: "method_return", 3: "error", 4: "signal"}.get(int(message.get_message_type()))
            sender, destination = message.get_sender(), message.get_destination()
            body = message.get_body().unpack() if message.get_body() is not None else []
            record = {"kind": kind, "sender": sender, "destination": destination,
                "serial": message.get_serial(), "reply_serial": message.get_reply_serial(),
                "path": message.get_path(), "interface": message.get_interface(), "member": message.get_member(),
                "error_name": message.get_error_name(), "body": body, "unix_ns": time.time_ns()}
            if kind == "method_call":
                if not (record["path"] or "").startswith("/org/freedesktop/GeoClue2/"):
                    return GLib.SOURCE_REMOVE
                record["consumer"] = credentials(sender)
                pending[(sender, record["serial"])] = record
            elif kind in ("method_return", "error"):
                original = pending.pop((destination, record["reply_serial"]), None)
                if original is None:
                    return GLib.SOURCE_REMOVE
                record["consumer"] = original["consumer"]
                record["request"] = {"sender": original["sender"], "serial": original["serial"],
                    "path": original["path"], "interface": original["interface"], "member": original["member"], "body": original["body"]}
            elif kind == "signal":
                if record["interface"] == "org.freedesktop.DBus" and record["member"] == "NameOwnerChanged":
                    if not body or (body[0] != "org.freedesktop.GeoClue2" and body[0] not in peers):
                        return GLib.SOURCE_REMOVE
                elif not (record["path"] or "").startswith("/org/freedesktop/GeoClue2/"):
                    return GLib.SOURCE_REMOVE
                record["consumer"] = credentials(destination)
            else:
                return GLib.SOURCE_REMOVE
            count += 1
            require(count <= 4096, "passive GeoClue transcript exceeded fixed bound")
            stream.write(json.dumps(record, ensure_ascii=False) + "\n")
        except BaseException as error:
            failure.append(str(error))
            (directory / "failure.json").write_text(json.dumps({"error": str(error), "type": type(error).__name__}))
            loop.quit()
        return GLib.SOURCE_REMOVE

    def queue(_connection, message, incoming, _data):
        if incoming:
            GLib.idle_add(consume, message.copy())
            # A monitor is not the addressed service. Do not let GDBus
            # auto-dispatch/reply UnknownMethod to another consumer's call.
            if message.get_message_type() == Gio.DBusMessageType.METHOD_CALL:
                return None
        return message

    monitor.add_filter(queue, None)
    # Replies have no object path/interface. Retain only replies correlated to
    # our narrow real GeoClue request cookies; unrelated replies never persist.
    rules = ["type='method_call',path_namespace='/org/freedesktop/GeoClue2'",
             "type='method_return'", "type='error'",
             "type='signal',path_namespace='/org/freedesktop/GeoClue2'",
             "type='signal',sender='org.freedesktop.DBus',interface='org.freedesktop.DBus',member='NameOwnerChanged'"]
    monitor.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus.Monitoring",
                      "BecomeMonitor", GLib.Variant("(asu)", (rules, 0)), None, Gio.DBusCallFlags.NONE, 5000, None)
    monitor.connect("closed", lambda *_: loop.quit())
    (directory / "ready.json").write_text(json.dumps({"pid": os.getpid(), "uid": os.getuid(), "nonce": nonce,
        "consumer_uid": consumer_uid, "start_ticks": int(Path("/proc/self/stat").read_text().rsplit(")", 1)[1].split()[19]),
        "system_bus": address, "monitor_unique_name": monitor.get_unique_name(), "credentials_connection": control.get_unique_name(),
        "purpose": "passive-original-consumer-replies-not-a-location-provider"}))
    try:
        loop.run()
        require(not failure, "passive GeoClue observer failed")
    finally:
        stream.close()
        monitor.close_sync(None)
        control.close_sync(None)


def _observed_location(context, service):
    directory = _watch_directory(context)
    _watch_control("location-watch-ready", context)
    path = directory / "messages.jsonl"
    raw = path.read_bytes()
    complete = raw[:raw.rfind(b"\n") + 1]
    events = [json.loads(line) for line in complete.splitlines()]
    clients, locations, errors = {}, {}, []
    for event in events:
        kind, body = event["kind"], event["body"]
        if kind == "signal" and event["member"] == "NameOwnerChanged":
            if body[0] == "org.freedesktop.GeoClue2":
                clients.clear()
                locations.clear()
            elif not body[2]:
                clients = {key: value for key, value in clients.items() if value["consumer_name"] != body[0]}
            continue
        request = event.get("request")
        if kind == "error" and request:
            errors.append(event)
            continue
        if kind == "method_return" and request:
            peer = event.get("consumer")
            if not peer or not peer.get("available") or peer["uid"] != context.uid:
                continue
            owner_name = request["sender"]
            if request["member"] in ("GetClient", "CreateClient") and body:
                clients[body[0]] = {"path": body[0], "consumer_name": owner_name, "actor": peer,
                    "service_owner": event["sender"], "properties": {}, "get_client_reply": event}
            client = clients.get(request["path"])
            if client and request["member"] == "Set" and len(request["body"]) == 3 and request["body"][1] == "DesktopId":
                client["desktop_id_set"] = {"requested": request["body"][2], "acknowledgement": event}
            if request["member"] == "GetAll" and body:
                if request["body"] == ["org.freedesktop.GeoClue2.Client"] and client:
                    client["properties"].update(body[0])
                elif request["body"] == ["org.freedesktop.GeoClue2.Location"]:
                    locations[(owner_name, request["path"])] = {"properties": body[0], "reply": event}
            if client and request["member"] == "Start":
                client["start_reply"] = event
            if request["member"] == "DeleteClient" and request["body"]:
                clients.pop(request["body"][0], None)
        elif kind == "signal":
            client = clients.get(event["path"])
            if client and event["sender"] == client["service_owner"]:
                if event["member"] == "PropertiesChanged" and body and body[0] == "org.freedesktop.GeoClue2.Client":
                    client["properties"].update(body[1])
                elif event["member"] == "LocationUpdated" and len(body) == 2:
                    client["properties"]["Location"] = body[1]
                    client["location_update"] = event
    actual = []
    for client in clients.values():
        if client["service_owner"] != service.get("unique_owner"):
            continue
        actor = client["actor"]
        proc = Path("/proc") / str(actor["pid"])
        if not proc.exists():
            continue
        if int((proc / "stat").read_text().rsplit(")", 1)[1].split()[19]) != actor["start_ticks"]:
            continue
        location = locations.get((client["consumer_name"], client["properties"].get("Location")))
        if location:
            client["location"] = location["properties"]
            client["location_reply"] = location["reply"]
        actual.append(client)
    return {"clients": actual, "consumer_errors": errors,
            "transcript": {"path": str(path), "sha256": digest(complete), "complete_bytes": len(complete)},
            "pending_transcript_bytes": len(raw) - len(complete)}


def _location(context):
    import gi
    gi.require_version("Gio", "2.0")
    from gi.repository import Gio, GLib
    bus = Gio.bus_get_sync(Gio.BusType.SYSTEM, None)
    owner = _dbus_owner(bus, "org.freedesktop.GeoClue2")
    result = {"service": owner, "identity": context.identity(), **_observed_location(context, owner)}
    configuration = [Path("/etc/geoclue/geoclue.conf"), Path("/etc/geolocation")]
    configuration.extend(sorted(Path("/etc/geoclue/conf.d").glob("*.conf")))
    result["source_configuration"] = []
    for path in configuration:
        if path.is_file():
            stat = path.stat()
            result["source_configuration"].append({"path": str(path), "uid": stat.st_uid,
                "gid": stat.st_gid, "mode": stat.st_mode & 0o777, "sha256": digest(path.read_bytes())})
    status = context.command(["systemctl", "show", "geoclue.service", "--property=LoadState,UnitFileState,ActiveState,SubState,MainPID,FragmentPath"], check=False)
    result["systemd"] = {"stdout": status.stdout, "stderr": status.stderr, "returncode": status.returncode}
    return result


def _location_control(operation, context):
    saved = Path("/run") / ("tuna-core-web-geoclue-" + context.nonce + ".json")
    if operation == "location-stop":
        before = _location(context)
        require(before["service"]["available"], "unavailable-service: no authorized GeoClue baseline to interrupt")
        require("geoclue" in before["service"]["package"].lower(), "GeoClue bus owner is not installed geoclue package")
        require(not saved.exists(), "GeoClue service interruption already owned")
        require("LoadState=masked" not in before["systemd"]["stdout"], "GeoClue was already masked; cannot own outage")
        mask = Path("/run/systemd/system/geoclue.service")
        require(not mask.exists() and not mask.is_symlink(), "pre-existing runtime GeoClue unit override")
        descriptor = os.open(saved, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
        with os.fdopen(descriptor, "w") as stream:
            json.dump(before, stream)
        try:
            context.command(["systemctl", "mask", "--runtime", "--now", "geoclue.service"])
            after = _location(context)
            require(not after["service"]["available"], "GeoClue still owned after service stop")
            return {"before": before, "after": after}
        except BaseException:
            context.command(["systemctl", "unmask", "--runtime", "geoclue.service"])
            context.command(["systemctl", "start", "geoclue.service"])
            saved.unlink()
            raise
    require(saved.is_file(), "no owned GeoClue service interruption to restore")
    before = _json_file(saved)
    context.command(["systemctl", "unmask", "--runtime", "geoclue.service"])
    context.command(["systemctl", "start", "geoclue.service"])
    after = _location(context)
    require(after["service"]["available"], "GeoClue service failed to reacquire name on restart")
    require(after["service"]["exe_sha256"] == before["service"]["exe_sha256"], "GeoClue service code changed")
    require((after["service"]["pid"], after["service"]["start_ticks"]) !=
            (before["service"]["pid"], before["service"]["start_ticks"]), "GeoClue service did not restart")
    require(not Path("/run/systemd/system/geoclue.service").is_symlink(), "owned runtime service mask not removed")
    old_fragment = next(line for line in before["systemd"]["stdout"].splitlines() if line.startswith("FragmentPath="))
    require(old_fragment in after["systemd"]["stdout"].splitlines(), "GeoClue unit fragment changed during restoration")
    require(after["source_configuration"] == before["source_configuration"], "GeoClue configuration/source changed during service interruption")
    saved.unlink()
    return {"before": before, "after": after}


def protobuf_fields(data):
    """Strict bounded protobuf reader for actual Mapbox vector-tile payloads."""
    require(len(data) <= 32 * 1024 * 1024, "oversize vector tile")
    index = 0

    def varint():
        nonlocal index
        value = 0
        for shift in range(0, 70, 7):
            require(index < len(data), "truncated vector tile varint")
            byte = data[index]
            index += 1
            value |= (byte & 127) << shift
            if not byte & 128:
                return value
        raise RuntimeError("invalid vector tile varint")

    while index < len(data):
        tag = varint()
        field, wire = tag >> 3, tag & 7
        require(field > 0, "zero protobuf field")
        if wire == 0:
            value = varint()
        elif wire in (1, 2, 5):
            length = varint() if wire == 2 else (8 if wire == 1 else 4)
            require(index + length <= len(data), "truncated vector tile field")
            value = data[index:index + length]
            index += length
        else:
            raise RuntimeError("unsupported vector tile wire type")
        yield field, wire, value


def validate_vector_tile(data):
    layers = []
    for field, wire, value in protobuf_fields(data):
        if field != 3 or wire != 2:
            continue
        fields = list(protobuf_fields(value))
        names = [v.decode("utf-8") for f, w, v in fields if f == 1 and w == 2]
        versions = [v for f, w, v in fields if f == 15 and w == 0]
        features = [v for f, w, v in fields if f == 2 and w == 2]
        require(len(names) == 1 and versions in ([1], [2]), "invalid Mapbox layer identity/version")
        geometries = 0
        for feature in features:
            contents = list(protobuf_fields(feature))
            kinds = [v for f, w, v in contents if f == 3 and w == 0]
            require(len(kinds) == 1 and kinds[0] in (1, 2, 3), "invalid vector feature geometry type")
            geometries += sum(f == 4 and w == 2 and len(v) > 0 for f, w, v in contents)
        layers.append({"name": names[0], "features": len(features), "geometries": geometries})
    require(any(x["geometries"] > 0 for x in layers), "map tile has no actual geometry")
    require(any(x["name"] in ("water", "transportation", "landcover", "landuse", "building") for x in layers), "map tile lacks geographic content layers")
    return layers


def _map_tiles(context):
    started = _json_file(context.path("web/content-start.json"))["unix_ns"]
    database = context.home / ".cache/shumate/cache.db"
    require(database.is_file(), "unavailable-content: real libshumate tile cache missing")
    with sqlite3.connect(database.as_uri() + "?mode=ro", uri=True) as connection:
        names = [x[0] for x in connection.execute("SELECT filename FROM tiles ORDER BY rowid DESC LIMIT 16")]
    require(names, "unavailable-content: Maps did not acquire a real network tile")
    evidence = []
    for name in names:
        path = Path(name)
        require(path.resolve().is_relative_to(database.parent.resolve()), "tile path escapes real shumate cache")
        if not path.is_file() or path.stat().st_mtime_ns < started:
            continue
        match = re.search(r"/(\d+)/(\d+)/(\d+)\.png$", name)
        require(match is not None, "unknown libshumate cache coordinates")
        z, x, y = map(int, match.groups())
        require(0 <= z <= 14 and 0 <= x < 2 ** z and 0 <= y < 2 ** z, "invalid map tile coordinates")
        data = path.read_bytes()
        layers = validate_vector_tile(data)
        url = f"https://tileserver.gnome.org/data/v3/{z}/{x}/{y}.pbf"
        request = urllib.request.Request(url, headers={"User-Agent": "TunaCoreAppQualification/397", "Accept-Encoding": "identity"})
        with urllib.request.urlopen(request, timeout=12) as response:
            network = response.read(32 * 1024 * 1024 + 1)
            require(response.status == 200 and response.url.startswith("https://tileserver.gnome.org/"), "real GNOME tile endpoint failed")
        require(digest(network) == digest(data), "native Maps cache differs from actual GNOME tile provider payload")
        evidence.append({"path": str(path), "url": url, "bytes": len(data), "sha256": digest(data), "layers": layers})
        if len(evidence) == 2:
            break
    require(evidence, "unavailable-content: native Maps acquired no fresh current-run real network map payload")
    return {"tiles": evidence}


def _map_image(arguments, context):
    from PIL import Image, ImageStat
    stage = arguments.get("stage")
    require(stage in ("baseline", "scale125", "after-unlock", "after-restart"), "unknown map export stage")
    path = context.path("web/map-" + stage + ".png")
    if not path.exists():
        return {"ready": False, "path": str(path), "distinction": "native-export-not-yet-written"}
    require(path.is_file() and path.stat().st_uid == context.uid, "native Maps did not export an original-user image")
    with Image.open(path) as image:
        require(image.format == "PNG" and image.width >= 250 and image.height >= 200, "native map export has wrong dimensions/format")
        rgb = image.convert("RGB")
        colors = rgb.getcolors(rgb.width * rgb.height)
        require(colors is not None and len(colors) > 128, "map export is blank/placeholder, not real rendered tiles")
        require(max(ImageStat.Stat(rgb).stddev) > 15, "map export has no geographic visual detail")
        size = image.size
    ocr = context.user(["tesseract", str(path), "stdout", "-l", "eng", "--psm", "11"], timeout=20).stdout
    require(re.search(r"Eiffel|Paris", ocr, re.IGNORECASE) is not None, "rendered exported map lacks independently recognizable searched geographic content")
    return {"ready": True, "path": str(path), "sha256": digest(path.read_bytes()), "dimensions": list(size), "ocr": ocr}


def _forecast(arguments, context):
    # Separate normal-user libgweather read of the exact UI-selected saved city.
    require(arguments.get("location") == "New York", "unknown forecast comparison city")
    value = context.user(["python3", str(Path(__file__).resolve()), "--forecast", "New York"], timeout=30)
    return json.loads(value.stdout)


def _read_forecast(selected_name):
    import gi
    gi.require_version("GWeather", "4.0")
    from gi.repository import Gio, GLib, GWeather
    world = GWeather.Location.get_world()
    require(world is not None, "unavailable-content: installed gweather-locations world database cannot be loaded")
    settings = Gio.Settings.new("org.gnome.Weather")
    variants = settings.get_value("locations")
    require(variants.n_children() > 0, "Weather UI has not persisted an actual location")
    locations = [world.deserialize(variants.get_child_value(index).get_variant())
                 for index in range(variants.n_children())]
    selected = [location for location in locations if location is not None and location.get_name() == selected_name]
    require(len(selected) == 1, "Weather has not persisted one exact UI-selected comparison city")
    location = selected[0]
    providers = GWeather.Provider.METAR | GWeather.Provider.MET_NO
    if hasattr(GWeather.Provider, "OWM"):
        providers |= GWeather.Provider.OWM
    info = GWeather.Info(application_id="org.gnome.Weather", contact_info="https://gitlab.gnome.org/GNOME/gnome-weather", location=location, enabled_providers=providers)
    loop = GLib.MainLoop()
    info.connect("updated", lambda *_: loop.quit())
    expired = {"value": False}
    def timeout():
        expired["value"] = True
        loop.quit()
        return GLib.SOURCE_REMOVE
    deadline = GLib.timeout_add_seconds(25, timeout)
    info.update()
    loop.run()
    if not expired["value"]:
        GLib.source_remove(deadline)
    require(not expired["value"], "unavailable-content: real weather provider update exceeded its fixed deadline")
    require(info.is_valid() and not info.network_error(), "unavailable-content: real libgweather provider cannot supply current weather")
    forecasts = info.get_forecast_list()
    require(forecasts, "unavailable-content: real libgweather provider supplied no forecast")
    ok, temperature = info.get_value_temp(GWeather.TemperatureUnit.CENTIGRADE)
    require(ok, "real weather provider has no numerical temperature")
    entries = []
    for item in forecasts[:256]:
        valid, stamp = item.get_value_update()
        temp_valid, temp = item.get_value_temp(GWeather.TemperatureUnit.CENTIGRADE)
        if valid and temp_valid:
            entries.append({"unix": stamp, "celsius": temp, "summary": item.get_conditions(), "sky": item.get_sky(),
                            "local_date": GLib.DateTime.new_from_unix_local(stamp).format("%b %e")})
    require(any(x["unix"] > time.time() for x in entries), "real forecast has no future dated content")
    return {"location": location.get_name(), "station": location.get_code(), "celsius": temperature,
            "summary": info.get_conditions(), "sky": info.get_sky(), "forecast": entries,
            "attribution": info.get_attribution(), "providers": int(providers), "observed_unix": time.time()}


def _help_content(context):
    base = Path("/usr/share/help/C/gnome-help")
    selected = []
    ns = {"m": "http://projectmallard.org/1.0/"}
    for name in ("index.page", "keyboard.page", "keyboard-shortcuts-set.page"):
        path = base / name
        require(path.is_file(), f"unavailable-content: installed gnome-user-docs lacks {path}")
        document = ET.fromstring(path.read_bytes())
        title = document.find("m:title", ns)
        require(title is not None, f"installed Mallard page lacks title: {path}")
        paragraphs = [" ".join("".join(p.itertext()).split()) for p in document.findall(".//m:p", ns)]
        selected.append({"path": str(path), "id": document.get("id"), "title": "".join(title.itertext()),
                         "paragraphs": paragraphs, "sha256": digest(path.read_bytes())})
    return {"pages": selected, "package": context.command(["pacman", "-Qo", str(base / "index.page")]).stdout.strip()}


def _prepare(context):
    marker = "TUNA-WEB-" + context.nonce
    directory = context.path("web")
    directory.mkdir(parents=True, exist_ok=True)
    os.chown(directory, context.uid, context.owner.pw_gid)
    _owned_write(context, "web/content-start.json", json.dumps({"unix_ns": time.time_ns(), "nonce": context.nonce}))
    _owned_write(context, "web/local.html", f"<!doctype html><html><head><title>Native local content {marker}</title></head><body><h1>Local file {marker}</h1><p>Actual native file readback: cobalt orchard 7391.</p></body></html>")
    _owned_write(context, "web/default.html", f"<!doctype html><html><head><title>Default handler {marker}</title></head><body><h1>Default file {marker}</h1><p>Native MIME handler content: amber harbor 5283.</p></body></html>")
    unit = "tuna-core-web-" + context.nonce
    context.user(["systemd-run", "--user", "--collect", "--unit=" + unit,
                  "--property=Type=exec", "python3", str(Path(__file__).resolve()), "--serve", str(directory), context.nonce])
    return {"marker": marker, "unit": unit, "local_file": str(context.path("web/local.html")),
            "default_file": str(context.path("web/default.html")), "ready_file": str(context.path("web/endpoint.json"))}


def _endpoint(context):
    path = context.path("web/endpoint.json")
    if not path.is_file():
        return {"ready": False}
    result = _json_file(path)
    require(result["uid"] == context.uid and result["nonce"] == context.nonce, "ordinary content endpoint owner/nonce changed")
    proc = Path("/proc") / str(result["pid"])
    require(proc.is_dir() and proc.stat().st_uid == context.uid, "ordinary content endpoint process disappeared")
    require(int((proc / "stat").read_text().rsplit(")", 1)[1].split()[19]) == result["start_ticks"], "content server PID reused")
    result["ready"] = True
    log = context.path("web/requests.jsonl")
    result["requests"] = [json.loads(line) for line in log.read_text().splitlines()] if log.is_file() else []
    return result


def _serve(directory, nonce):
    require(re.fullmatch(r"[a-zA-Z0-9-]{8,80}", nonce) is not None, "invalid content nonce")
    require(os.geteuid() != 0 and directory.is_dir() and directory.stat().st_uid == os.getuid(), "content endpoint must be owned by original nonroot user")
    marker = "TUNA-WEB-" + nonce

    class Content(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_):
            return

        def do_GET(self):
            parsed = urllib.parse.urlsplit(self.path)
            params = urllib.parse.parse_qs(parsed.query, keep_blank_values=True, strict_parsing=True) if parsed.query else {}
            row = {"path": parsed.path, "query": params, "method": "GET", "user_agent": self.headers.get("User-Agent", ""), "unix": time.time()}
            with (directory / "requests.jsonl").open("a") as stream:
                stream.write(json.dumps(row, ensure_ascii=False) + "\n")
            if parsed.path == "/" + nonce + "/":
                previous = self.headers.get("Cookie", "")
                body = f'''<h1>Browser content {marker}</h1><p id="stored">Stored payload: </p>
<form action="/{nonce}/submitted"><label for="payload">Qualification payload</label><input id="payload" name="payload" autocomplete="off" required><button type="submit">Submit actual payload</button></form>
<a href="/{nonce}/second">Second real page</a><a href="geo:48.85837,2.29448">Open geographic handler</a>
<a href="help:gnome-help/keyboard-shortcuts-set">Open installed Help</a>
<p>Cookie readback: {html.escape(previous)}</p>
<script>document.getElementById('stored').textContent='Stored payload: '+(localStorage.getItem('payload')||'unset'); document.querySelector('form').addEventListener('submit',()=>localStorage.setItem('payload',document.getElementById('payload').value));</script>'''
            elif parsed.path == "/" + nonce + "/submitted":
                values = params.get("payload", [])
                if len(values) != 1:
                    self.send_error(400, "one exact payload required")
                    return
                body = f"<h1>Submitted {marker}</h1><p>Received exact payload: {html.escape(values[0])}</p><a href='/{nonce}/'>Return to content</a>"
            elif parsed.path == "/" + nonce + "/second":
                body = f"<h1>Second page {marker}</h1><p>Navigation payload: silver meadow 6427.</p><a href='/{nonce}/'>Return to content</a>"
            else:
                self.send_error(404, "No qualification content at this path")
                return
            document = (f"<!doctype html><html><head><meta charset='utf-8'><title>{marker}</title></head><body>{body}</body></html>").encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.send_header("Cache-Control", "no-store")
            self.send_header("Set-Cookie", "tuna_native=" + nonce + "; SameSite=Lax")
            self.send_header("Content-Length", str(len(document)))
            self.end_headers()
            self.wfile.write(document)

    server = http.server.HTTPServer(("127.0.0.1", 0), Content)
    state = {"url": f"http://127.0.0.1:{server.server_port}/{nonce}/", "pid": os.getpid(),
             "uid": os.getuid(), "nonce": nonce, "start_ticks": int(Path("/proc/self/stat").read_text().rsplit(")", 1)[1].split()[19])}
    (directory / "endpoint.json").write_text(json.dumps(state))
    try:
        server.serve_forever()
    finally:
        server.server_close()


def dispatch(operation, arguments, context):
    require(isinstance(arguments, dict), "web domain arguments must be an object")
    if operation in ("location-watch-start", "location-watch-ready", "location-watch-stop"):
        return _watch_control(operation, context)
    if operation in ("permission-save", "permission-restore"):
        return _permissions(operation, context)
    if operation == "prepare":
        return _prepare(context)
    if operation == "endpoint":
        return _endpoint(context)
    if operation == "stop-endpoint":
        unit = "tuna-core-web-" + context.nonce + ".service"
        result = context.user(["systemctl", "--user", "stop", unit])
        return {"unit": unit, "returncode": result.returncode}
    if operation == "state":
        return _state(arguments, context)
    if operation == "location":
        return _location(context)
    if operation in ("location-stop", "location-restore"):
        return _location_control(operation, context)
    if operation == "map-tiles":
        return _map_tiles(context)
    if operation == "map-image":
        return _map_image(arguments, context)
    if operation == "forecast":
        return _forecast(arguments, context)
    if operation == "help-content":
        return _help_content(context)
    raise ValueError("unknown core-apps web operation: " + operation)


if __name__ == "__main__":
    if len(sys.argv) == 3 and sys.argv[1] == "--user-state":
        require(sys.argv[2] in APPS, "unknown app state")
        print(json.dumps(_user_state(sys.argv[2]), ensure_ascii=False))
    elif len(sys.argv) == 2 and sys.argv[1] == "--permission-source":
        print(json.dumps(_permission_source(), ensure_ascii=False))
    elif len(sys.argv) == 3 and sys.argv[1] == "--restore-permissions":
        print(json.dumps(_restore_user_permissions(json.loads(sys.argv[2])), ensure_ascii=False))
    elif len(sys.argv) == 3 and sys.argv[1] == "--forecast":
        require(sys.argv[2] == "New York", "unknown forecast comparison city")
        print(json.dumps(_read_forecast(sys.argv[2]), ensure_ascii=False))
    elif len(sys.argv) == 4 and sys.argv[1] == "--serve":
        _serve(Path(sys.argv[2]), sys.argv[3])
    elif len(sys.argv) == 5 and sys.argv[1] == "--location-watch":
        _location_watch(Path(sys.argv[2]), sys.argv[3], int(sys.argv[4]))
    else:
        raise SystemExit("web domain helper requires a fixed internal operation")
