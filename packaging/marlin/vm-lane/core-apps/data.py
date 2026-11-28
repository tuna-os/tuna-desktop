"""Original-user EDS/GIO observers and owned production Radicale fixture.

DAV collection/source setup is input preparation, never application proof.
Product writes are performed only through original Calendar/Contacts UI and
independently read through the packaged production DAV service.
"""
from __future__ import annotations

import base64
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import sys
import secrets
import shutil
import urllib.error
import socket
import urllib.parse
import urllib.request
import xml.etree.ElementTree as ET


SERVICES = {
    "calendar": ("evolution-calendar-factory.service", "org.gnome.evolution.dataserver.Calendar8", "evolution-calendar-factory"),
    "contacts": ("evolution-addressbook-factory.service", "org.gnome.evolution.dataserver.AddressBook10", "evolution-addressbook-factory"),
}
SCHEMAS = {
    "calendar": "org.gnome.calendar", "contacts": "org.gnome.Contacts",
    "console": "org.gnome.Console", "files": "org.gnome.nautilus.preferences",
    "editor": "org.gnome.TextEditor",
}
MODULE = "/usr/libexec/tuna-core-apps/data.py"


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def digest(raw):
    return hashlib.sha256(raw).hexdigest()


def _owner(bus, name):
    from gi.repository import Gio, GLib
    def call(method, signature):
        return bus.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus",
                             method, GLib.Variant("(s)", (name,)), GLib.VariantType.new(signature),
                             Gio.DBusCallFlags.NONE, 3000, None).unpack()[0]
    if not call("NameHasOwner", "(b)"):
        return {"name": name, "available": False, "distinction": "unavailable-service"}
    pid, uid = call("GetConnectionUnixProcessID", "(u)"), call("GetConnectionUnixUser", "(u)")
    proc = Path("/proc") / str(pid)
    exe = (proc / "exe").resolve(strict=True)
    require(uid == os.getuid() and exe.stat().st_uid == 0 and not exe.stat().st_mode & 0o022,
            "EDS/GIO service is not the ordinary user's installed original owner")
    return {"name": name, "available": True, "owner": call("GetNameOwner", "(s)"),
            "pid": pid, "uid": uid, "exe": str(exe), "exe_sha256": digest(exe.read_bytes()),
            "start_ticks": int((proc / "stat").read_text().rsplit(")", 1)[1].split()[19]),
            "cgroup": (proc / "cgroup").read_text().splitlines()}


def _sources(kind):
    import gi
    gi.require_version("EDataServer", "1.2")
    from gi.repository import EDataServer
    registry = EDataServer.SourceRegistry.new_sync(None)
    extension = EDataServer.SOURCE_EXTENSION_CALENDAR if kind == "calendar" else EDataServer.SOURCE_EXTENSION_ADDRESS_BOOK
    result = []
    for source in registry.list_sources(extension):
        backend = source.get_extension(extension).get_backend_name()
        result.append((source, {"uid": source.get_uid(), "name": source.get_display_name(),
                               "backend": backend, "enabled": registry.check_enabled(source),
                               "writable": source.get_writable(), "parent": source.get_parent(),
                               "source": source.to_string()[0]}))
    return result


def _eds(kind, nonce, source_uid=None):
    import gi
    gi.require_version("Gio", "2.0")
    from gi.repository import Gio, GLib
    bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
    sources = _sources(kind)
    selected = [(source, row) for source, row in sources if row["enabled"] and
                (row["uid"] == source_uid if source_uid is not None else row["backend"] == "local")]
    require(selected, "unavailable-service: no enabled genuine selected EDS source")
    local = [(source, row) for source, row in selected if row["backend"] == "local"]
    records = []
    try:
        if kind == "calendar":
            gi.require_version("ECal", "2.0")
            from gi.repository import ECal
            for source, row in selected:
                client = ECal.Client.connect_sync(source, ECal.ClientSourceType.EVENTS, 10, None)
                ok, components = client.get_object_list_sync("#t", None)
                require(ok, "real ECal read failed")
                for component in components:
                    raw = component.as_ical_string()
                    records.append({"source_uid": row["uid"], "uid": component.get_uid(),
                                    "summary": component.get_summary(), "location": component.get_location(),
                                    "start": component.get_dtstart().as_ical_string() if component.get_dtstart() else None,
                                    "end": component.get_dtend().as_ical_string() if component.get_dtend() else None,
                                    "payload": raw, "sha256": digest(raw.encode())})
        else:
            gi.require_version("EBook", "1.2")
            gi.require_version("EBookContacts", "1.2")
            from gi.repository import EBook, EBookContacts
            for source, row in selected:
                client = EBook.BookClient.connect_sync(source, 10, None)
                ok, contacts = client.get_contacts_sync('(contains "x-evolution-any-field" "")', None)
                require(ok, "real EBook read failed")
                for contact in contacts:
                    raw = contact.to_string(EBookContacts.VCardFormat.VCARD_30)
                    records.append({"source_uid": row["uid"], "uid": contact.get_const(EBookContacts.ContactField.UID),
                                    "full_name": contact.get_const(EBookContacts.ContactField.FULL_NAME),
                                    "payload": raw, "sha256": digest(raw.encode())})
    except GLib.Error as error:
        owner = _owner(bus, SERVICES[kind][1])
        if owner["available"]:
            raise
        return {"available": False, "distinction": "unavailable-service", "error_domain": error.domain,
                "error_code": error.code, "error": str(error), "service": owner,
                "sources": [row for _, row in sources]}
    require(len(records) <= 512, "EDS evidence exceeded bounded qualification account")
    unrelated = sorted((row["source_uid"], row["uid"], row["sha256"]) for row in records if nonce not in row["payload"])
    return {"available": True, "distinction": "observed", "sources": [row for _, row in sources],
            "local_sources": [row for _, row in local], "service": _owner(bus, SERVICES[kind][1]),
            "selected_sources": [row for _, row in selected],
            "registry": _owner(bus, "org.gnome.evolution.dataserver.Sources5"),
            "records": [row for row in records if nonce in row["payload"]],
            "unrelated_sha256": digest(json.dumps(unrelated, sort_keys=True).encode())}


def _remote(kind, nonce, admission, source_uid=None, wrong_password=False):
    """Read only the exact admitted production Radicale collection."""
    import gi
    gi.require_version("EDataServer", "1.2")
    from gi.repository import EDataServer
    backend = "caldav" if kind == "calendar" else "carddav"
    expected_uid = "tuna-dav-" + kind + "-" + nonce
    require(source_uid is None or source_uid == expected_uid, "unowned DAV source requested")
    sources = [(source, row) for source, row in _sources(kind)
               if row["enabled"] and row["backend"] == backend and row["uid"] == expected_uid]
    require(len(sources) == 1, "unavailable-service: owned production DAV source unavailable")
    source, row = sources[0]
    require(source.has_extension(EDataServer.SOURCE_EXTENSION_WEBDAV_BACKEND),
            "unavailable-service: selected remote source has no supported WebDAV interface")
    uri = source.get_extension(EDataServer.SOURCE_EXTENSION_WEBDAV_BACKEND).dup_uri()
    parsed = urllib.parse.urlsplit(uri.to_string())
    url = admission["url"] + ("calendar/" if kind == "calendar" else "contacts/")
    require(parsed.scheme == "http" and parsed.hostname == "127.0.0.1" and
            parsed.port == admission["port"] and parsed.path == urllib.parse.urlsplit(url).path and
            not parsed.fragment and not parsed.query,
            "DAV source does not address the exact admitted owned production service")
    registry = EDataServer.SourceRegistry.new_sync(None)
    password, username, credential_uid = None, None, None
    current = source
    visited = set()
    while current is not None:
        require(current.get_uid() not in visited, "cyclic remote account source ancestry")
        visited.add(current.get_uid())
        if current.has_extension(EDataServer.SOURCE_EXTENSION_AUTHENTICATION):
            authentication = current.get_extension(EDataServer.SOURCE_EXTENSION_AUTHENTICATION)
            username = username or authentication.get_user()
        ok, secret = current.lookup_password_sync(None)
        require(ok, "unavailable-service: ordinary account keyring password lookup failed")
        if secret:
            password, credential_uid = secret, current.get_uid()
            break
        current = registry.ref_source(current.get_parent()) if current.get_parent() else None
    require(username and password,
            "unavailable-service: owned DAV account needs ordinary-user stored password credentials")
    if wrong_password:
        password = "deliberately-wrong-" + nonce
    manager = urllib.request.HTTPPasswordMgrWithDefaultRealm()
    manager.add_password(None, url, username, password)
    class NoRedirect(urllib.request.HTTPRedirectHandler):
        def redirect_request(self, request, fp, code, message, headers, new_url):
            raise RuntimeError("owned production DAV collection must not redirect away from admitted URL")
    opener = urllib.request.build_opener(NoRedirect, urllib.request.HTTPBasicAuthHandler(manager),
                                        urllib.request.HTTPDigestAuthHandler(manager))
    namespace = "urn:ietf:params:xml:ns:caldav" if kind == "calendar" else "urn:ietf:params:xml:ns:carddav"
    query = "calendar-query" if kind == "calendar" else "addressbook-query"
    data = "calendar-data" if kind == "calendar" else "address-data"
    filtering = ('<r:filter><r:comp-filter name="VCALENDAR"><r:comp-filter name="VEVENT"/>'
                 '</r:comp-filter></r:filter>') if kind == "calendar" else "<r:filter/>"
    body = (f'<r:{query} xmlns:d="DAV:" xmlns:r="{namespace}"><d:prop><d:getetag/>'
            f'<r:{data}/></d:prop>{filtering}</r:{query}>').encode()
    request = urllib.request.Request(url, data=body, method="REPORT",
                                     headers={"Content-Type": "application/xml; charset=utf-8", "Depth": "1"})
    try:
        with opener.open(request, timeout=15) as response:
            require(not wrong_password, "production DAV service accepted a deliberately wrong password")
            require(response.status == 207 and response.url == url, "DAV provider did not return a real multistatus")
            raw = response.read(4 * 1024 * 1024 + 1)
    except urllib.error.HTTPError as error:
        if not wrong_password or error.code != 401:
            raise
        return {"distinction": "authentication-denied", "status": error.code,
                "provider_url": url, "source_uid": expected_uid}
    require(len(raw) <= 4 * 1024 * 1024, "production DAV readback exceeded account evidence bound")
    document = ET.fromstring(raw)
    require(document.tag == "{DAV:}multistatus", "production DAV provider returned an unrelated document")
    records, unrelated = [], []
    responses = document.findall("{DAV:}response")
    require(len(responses) <= 512, "production DAV account exceeds qualification bound")
    for item in responses:
        href = item.findtext("{DAV:}href")
        for propstat in item.findall("{DAV:}propstat"):
            status = propstat.findtext("{DAV:}status", "")
            require(" 200 " in status, "production DAV provider omitted required payload/property")
            payload = propstat.findtext("{DAV:}prop/{" + namespace + "}" + data)
            require(isinstance(payload, str), "production DAV response has no actual principal payload")
            record = {"href": href, "etag": propstat.findtext("{DAV:}prop/{DAV:}getetag"),
                      "payload": payload, "sha256": digest(payload.encode())}
            (records if nonce in payload else unrelated).append(record)
    return {"available": True, "distinction": "observed-production-radicale", "source": row,
            "provider_url": url, "backend": backend, "credential_source_uid": credential_uid,
            "service_admission": admission, "authentication": "original-user-keyring-no-secret-export",
            "response_sha256": digest(raw), "records": records,
            "unrelated_sha256": digest(json.dumps(sorted((r["href"], r["sha256"]) for r in unrelated)).encode())}


def _trash(original):
    from gi.repository import Gio
    trash = Gio.File.new_for_uri("trash:///")
    iterator = trash.enumerate_children("standard::name,trash::orig-path", Gio.FileQueryInfoFlags.NONE, None)
    result = []
    try:
        while True:
            info = iterator.next_file(None)
            if info is None:
                break
            if info.get_attribute_byte_string("trash::orig-path") == original:
                child = trash.get_child(info.get_name())
                ok, raw, _ = child.load_contents(None)
                require(ok and len(raw) <= 4096, "actual Trash contents unreadable or unbounded")
                result.append({"uri": child.get_uri(), "original_path": original,
                               "original_path_base64": base64.b64encode(os.fsencode(original)).decode(),
                               "base64": base64.b64encode(raw).decode(), "bytes": len(raw), "sha256": digest(raw)})
    finally:
        iterator.close(None)
    return {"matches": result, "observer_uid": os.getuid(), "backend": "actual-gio-trash"}


def _settings(app):
    from gi.repository import Gio
    schema = Gio.SettingsSchemaSource.get_default().lookup(SCHEMAS[app], True)
    require(schema is not None, "installed app settings schema unavailable")
    settings = Gio.Settings.new_full(schema, None, None)
    return {"schema": SCHEMAS[app], "values": {key: settings.get_value(key).print_(True) for key in schema.list_keys()}}


def user_operation(operation, arguments):
    require(os.geteuid() != 0, "data consumer observations must run as original ordinary user")
    if operation == "eds":
        return _eds(arguments["kind"], arguments["nonce"], arguments.get("source_uid"))
    if operation in ("remote", "remote-wrong-password"):
        return _remote(arguments["kind"], arguments["nonce"], arguments["admission"],
                       arguments.get("source_uid"), operation == "remote-wrong-password")
    if operation == "service":
        from gi.repository import Gio
        return _owner(Gio.bus_get_sync(Gio.BusType.SESSION, None), SERVICES[arguments["kind"]][1])
    if operation == "trash":
        return _trash(arguments["original"])
    if operation == "settings":
        return _settings(arguments["app"])
    if operation in ("dav-prepare", "dav-provision", "dav-cleanup"):
        return _dav_user(operation, arguments)
    raise ValueError("unknown original-user data observation")


def _dav_user(operation, arguments):
    import gi
    gi.require_version("EDataServer", "1.2")
    from gi.repository import EDataServer
    directory, nonce = Path(arguments["directory"]), arguments["nonce"]
    require(os.geteuid() != 0 and directory.parent.stat().st_uid == os.getuid(),
            "DAV fixture must belong to the original ordinary user")
    if operation == "dav-prepare":
        import bcrypt
        directory.mkdir(mode=0o700)
        listener = socket.socket()
        try:
            listener.bind(("127.0.0.1", 0))
            port = listener.getsockname()[1]
        finally:
            listener.close()
        username, password = "tuna-" + nonce, secrets.token_urlsafe(32)
        (directory / "owned-sources.json").write_text("[]")
        credentials = directory / "credentials.json"
        credentials.write_text(json.dumps({"username": username, "password": password, "port": port}))
        credentials.chmod(0o600)
        htpasswd = directory / "htpasswd"
        htpasswd.write_text(username + ":" + bcrypt.hashpw(password.encode(), bcrypt.gensalt()).decode() + "\n")
        htpasswd.chmod(0o600)
        configuration = directory / "config"
        configuration.write_text(
            "[server]\nhosts = 127.0.0.1:" + str(port) +
            "\n[auth]\ntype = htpasswd\nhtpasswd_filename = " + str(htpasswd) +
            "\nhtpasswd_encryption = bcrypt\n[rights]\ntype = owner_only\n[storage]\nfilesystem_folder = " +
            str(directory / "collections") + "\n[logging]\nlevel = warning\n")
        configuration.chmod(0o600)
        return {"port": port, "url": "http://127.0.0.1:" + str(port) + "/" + username + "/",
                "configuration": str(configuration)}
    registry = EDataServer.SourceRegistry.new_sync(None)
    if operation == "dav-cleanup":
        removed = []
        journal = directory / "owned-sources.json"
        owned = json.loads(journal.read_text()) if journal.exists() else []
        require(isinstance(owned, list) and len(owned) == len(set(owned)) and
                all(uid in ["tuna-dav-" + kind + "-" + nonce for kind in SERVICES] for uid in owned),
                "DAV cleanup ownership journal differs")
        for uid in owned:
            kind = next(kind for kind in SERVICES if uid == "tuna-dav-" + kind + "-" + nonce)
            source = registry.ref_source(uid)
            if source is not None:
                require(source.get_display_name() == "Tuna DAV " + kind + " " + nonce,
                        "refusing cleanup of changed/unowned EDS source")
                require(source.remove_sync(None), "owned EDS source cleanup failed")
                removed.append(uid)
            credential_source = source or EDataServer.Source.new_with_uid(uid, None)
            require(credential_source.delete_password_sync(None), "owned DAV keyring credential cleanup failed")
        if directory.exists():
            require(not directory.is_symlink() and directory.stat().st_uid == os.getuid(),
                    "refusing cleanup of changed DAV fixture directory")
            shutil.rmtree(directory)
        return {"removed_source_uids": removed, "owned_store_removed": not directory.exists()}
    require(operation == "dav-provision", "unknown DAV user fixture operation")
    credentials = json.loads((directory / "credentials.json").read_text())
    admission = arguments["admission"]
    require(credentials["port"] == admission["port"], "DAV admission port changed")
    auth = "Basic " + base64.b64encode((credentials["username"] + ":" + credentials["password"]).encode()).decode()
    # Real DAV collection creation is setup, not product CRUD evidence.
    principal = urllib.request.Request(admission["url"], method="PROPFIND",
                                      headers={"Authorization": auth, "Depth": "0"})
    try:
        with urllib.request.urlopen(principal, timeout=15) as response:
            require(response.status == 207, "production DAV principal discovery failed")
    except urllib.error.HTTPError as error:
        if error.code != 404:
            raise
        create = urllib.request.Request(admission["url"], method="MKCOL", headers={"Authorization": auth})
        with urllib.request.urlopen(create, timeout=15) as response:
            require(response.status == 201, "production DAV principal preparation failed")
    for kind in SERVICES:
        url = admission["url"] + ("calendar/" if kind == "calendar" else "contacts/")
        if kind == "calendar":
            body = b'<c:mkcalendar xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:set><d:prop><d:displayname>Tuna Calendar</d:displayname></d:prop></d:set></c:mkcalendar>'
            method = "MKCALENDAR"
        else:
            body = b'<d:mkcol xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:carddav"><d:set><d:prop><d:resourcetype><d:collection/><c:addressbook/></d:resourcetype><d:displayname>Tuna Contacts</d:displayname></d:prop></d:set></d:mkcol>'
            method = "MKCOL"
        request = urllib.request.Request(url, data=body, method=method,
            headers={"Authorization": auth, "Content-Type": "application/xml; charset=utf-8"})
        with urllib.request.urlopen(request, timeout=15) as response:
            require(response.status == 201, "production DAV collection preparation failed")
        uid = "tuna-dav-" + kind + "-" + nonce
        require(registry.ref_source(uid) is None, "owned EDS DAV source already exists")
        source = EDataServer.Source.new_with_uid(uid, None)
        ok, existing_secret = source.lookup_password_sync(None)
        require(ok and existing_secret is None, "refusing to replace a pre-existing keyring credential")
        journal = directory / "owned-sources.json"
        owned = json.loads(journal.read_text())
        require(uid not in owned, "DAV source provisioning cannot be replayed")
        journal.write_text(json.dumps([*owned, uid]))
        source.set_display_name("Tuna DAV " + kind + " " + nonce)
        source.set_enabled(True)
        extension = EDataServer.SOURCE_EXTENSION_CALENDAR if kind == "calendar" else EDataServer.SOURCE_EXTENSION_ADDRESS_BOOK
        source.get_extension(extension).set_backend_name("caldav" if kind == "calendar" else "carddav")
        authentication = source.get_extension(EDataServer.SOURCE_EXTENSION_AUTHENTICATION)
        authentication.set_host("127.0.0.1")
        authentication.set_port(admission["port"])
        authentication.set_user(credentials["username"])
        authentication.set_method("plain/password")
        authentication.set_remember_password(True)
        source.get_extension(EDataServer.SOURCE_EXTENSION_SECURITY).set_secure(False)
        webdav = source.get_extension(EDataServer.SOURCE_EXTENSION_WEBDAV_BACKEND)
        webdav.set_resource_path(urllib.parse.urlsplit(url).path)
        webdav.set_display_name(source.get_display_name())
        require(source.store_password_sync(credentials["password"], True, None),
                "ordinary-user keyring refused the owned DAV credential")
        require(registry.commit_source_sync(source, None), "real EDS registry refused owned DAV source")
    return {"purpose": "production-DAV-input-preparation-not-product-proof",
            "source_uids": ["tuna-dav-" + kind + "-" + nonce for kind in SERVICES]}


def _dav_control(operation, context):
    receipt = Path("/run") / ("tuna-core-dav-" + context.nonce + ".json")
    directory = context.path("data/dav")
    unit = "tuna-core-dav-" + context.nonce + ".service"
    if operation == "dav-start":
        require(not receipt.exists(), "owned DAV service cannot be replayed")
        receipt.write_text(json.dumps({"fixture": {"configuration": str(directory / "config")},
                                       "unit": unit, "process": None}))
        receipt.chmod(0o600)
        fixture = _user(context, "dav-prepare", {"directory": str(directory), "nonce": context.nonce})
        receipt.write_text(json.dumps({"fixture": fixture, "unit": unit, "process": None}))
        context.user(["systemd-run", "--user", "--collect", "--unit=" + unit, "--property=Type=exec",
                      "/usr/bin/radicale", "--config", fixture["configuration"]])
        return {"unit": unit, "purpose": "production-service-fixture-not-app-proof"}
    require(receipt.is_file() and receipt.stat().st_uid == 0 and receipt.stat().st_mode & 0o777 == 0o600,
            "owned DAV admission receipt unavailable")
    saved = json.loads(receipt.read_text())
    require(saved["unit"] == unit and saved["fixture"]["configuration"] == str(directory / "config"),
            "owned DAV admission identity differs")
    if operation == "dav-stop":
        stopped = context.user(["systemctl", "--user", "stop", unit], check=False)
        state = context.user(["systemctl", "--user", "show", unit, "--property=MainPID"], check=False)
        require("MainPID=0" in state.stdout, "owned production DAV unit did not prove process retirement")
        cleanup = _user(context, "dav-cleanup", {"directory": str(directory), "nonce": context.nonce})
        receipt.unlink()
        return {"unit": unit, "stop_returncode": stopped.returncode, "retired_unit": state.stdout, "cleanup": cleanup}
    if operation == "dav-restart":
        require(saved["process"] is not None, "DAV restart requires admitted live baseline")
        before = saved["process"]
        context.user(["systemctl", "--user", "restart", unit])
        saved["previous_process"], saved["process"] = before, None
        receipt.write_text(json.dumps(saved))
        return {"before": before, "restart_requested": True}
    require(operation == "dav-state", "unknown owned DAV control")
    status = context.user(["systemctl", "--user", "show", unit, "--property=MainPID,ActiveState,ExecStart"])
    values = dict(line.split("=", 1) for line in status.stdout.splitlines())
    pid = int(values["MainPID"])
    if values["ActiveState"] == "activating" or not pid:
        return {"ready": False, "systemd": values}
    require(values["ActiveState"] == "active", "production DAV service did not remain active")
    proc = Path("/proc") / str(pid)
    require(proc.stat().st_uid == context.uid, "production DAV service has wrong original UID")
    argv = (proc / "cmdline").read_bytes().split(b"\0")
    require(b"/usr/bin/radicale" in argv and os.fsencode(saved["fixture"]["configuration"]) in argv,
            "service is not the packaged Radicale invocation with owned configuration")
    inode = None
    for line in Path("/proc/net/tcp").read_text().splitlines()[1:]:
        fields = line.split()
        if fields[1] == "0100007F:" + format(saved["fixture"]["port"], "04X") and fields[3] == "0A":
            require(inode is None, "ambiguous production DAV listener")
            inode = fields[9]
    if inode is None:
        return {"ready": False, "systemd": values}
    sockets = []
    for fd in (proc / "fd").iterdir():
        try:
            sockets.append(os.readlink(fd))
        except FileNotFoundError:
            # A concurrent HTTP connection may close while reading this
            # independent snapshot; the listening inode must still be present.
            continue
    require("socket:[" + inode + "]" in sockets,
            "admitted Radicale PID does not own its loopback listener")
    package = context.command(["pacman", "-Q", "radicale"]).stdout.strip()
    module = Path(context.user(["python3", "-c", "import radicale; print(radicale.__file__)"]).stdout.strip()).resolve(strict=True)
    entry = Path("/usr/bin/radicale")
    for path in (module, entry):
        require(path.stat().st_uid == 0 and not path.stat().st_mode & 0o022,
                "Radicale original installed source/entrypoint is mutable by ordinary users")
        ownership = context.command(["pacman", "-Qoq", str(path)]).stdout.strip()
        require(ownership == "radicale", "DAV source/entrypoint is not owned by installed radicale")
    executable = (proc / "exe").resolve(strict=True)
    require(executable.stat().st_uid == 0 and not executable.stat().st_mode & 0o022,
            "production DAV interpreter is not original root-owned installed code")
    interpreter_package = context.command(["pacman", "-Qo", str(executable)]).stdout.strip()
    process = {"pid": pid, "uid": context.uid,
               "start_ticks": int((proc / "stat").read_text().rsplit(")", 1)[1].split()[19]),
               "exe": str(executable), "exe_sha256": digest(executable.read_bytes()),
               "interpreter_package": interpreter_package, "package": package,
               "source": str(module), "source_sha256": digest(module.read_bytes()),
               "entrypoint_sha256": digest(entry.read_bytes()), "socket_inode": inode,
               "configuration_sha256": digest((directory / "config").read_bytes()),
               "htpasswd_sha256": digest((directory / "htpasswd").read_bytes())}
    require(saved["process"] is None or saved["process"] == process, "admitted production DAV process changed unexpectedly")
    if saved.get("previous_process"):
        require((pid, process["start_ticks"]) != (saved["previous_process"]["pid"], saved["previous_process"]["start_ticks"]),
                "production DAV restart did not replace its process")
        for key in ("package", "exe_sha256", "interpreter_package", "source_sha256", "entrypoint_sha256",
                    "configuration_sha256", "htpasswd_sha256"):
            require(process[key] == saved["previous_process"][key], "production DAV restart changed installed code")
    saved["process"] = process
    receipt.write_text(json.dumps(saved))
    return {"ready": True, **saved["fixture"], "process": process, "unit": unit}


def _user(context, operation, arguments):
    result = context.user(["/usr/bin/python", MODULE, "--user", operation, json.dumps(arguments)], timeout=30)
    return json.loads(result.stdout)


def _unit(context, kind):
    unit = SERVICES[kind][0]
    result = context.user(["systemctl", "--user", "show", unit, "--property=Id,LoadState,ActiveState,SubState,MainPID,FragmentPath,UnitFileState,ControlGroup"])
    state = dict(line.split("=", 1) for line in result.stdout.splitlines())
    require(state.get("Id") == unit and state.get("LoadState") in ("loaded", "masked"), "real installed EDS user unit unavailable")
    return state


def _service_control(context, kind, restore=False):
    unit, _, binary = SERVICES[kind]
    receipt = context.path("data-" + kind + "-outage.json")
    if restore:
        require(receipt.exists(), "no owned EDS fault receipt to restore")
        original = json.loads(receipt.read_text())
        context.user(["systemctl", "--user", "unmask", "--runtime", unit])
        context.user(["systemctl", "--user", "start", unit])
        restored = _unit(context, kind)
        require(restored["ActiveState"] == "active", "EDS unit recovery failed")
        return {"original": original, "restored": restored,
                "service": _user(context, "service", {"kind": kind})}
    require(not receipt.exists(), "EDS outage must not be replayed")
    before = _unit(context, kind)
    owner = _user(context, "service", {"kind": kind})
    require(before["ActiveState"] == "active" and before["UnitFileState"] != "masked" and owner["available"],
            "negative requires reachable authorized real EDS baseline")
    require(Path(owner["exe"]).name == binary and str(owner["pid"]) == before["MainPID"],
            "EDS bus owner does not match real owned service MainPID")
    fragment = Path(before["FragmentPath"])
    require(fragment.is_relative_to("/usr/lib/systemd/user") and fragment.stat().st_uid == 0,
            "EDS unit is not the genuine installed service")
    receipt.write_text(json.dumps({"before": before, "owner": owner}))
    try:
        context.user(["systemctl", "--user", "mask", "--runtime", unit])
        context.user(["systemctl", "--user", "stop", unit])
        after = _unit(context, kind)
        absent = _user(context, "service", {"kind": kind})
        require(after["ActiveState"] == "inactive" and not absent["available"], "real EDS outage not established")
        return {"before": before, "owner": owner, "after": after, "service": absent,
                "fault": "runtime-mask-and-stop-real-owned-EDS-user-unit"}
    except BaseException:
        _service_control(context, kind, restore=True)
        raise


def _storage(context, kind):
    """Independent real on-disk records, including while a backend is masked."""
    base = context.home / ".local/share/evolution" / ("calendar" if kind == "calendar" else "addressbook")
    records, files = [], []
    if base.exists():
        require(not base.is_symlink(), "EDS storage symlink refused")
        for path in sorted(base.rglob("*")):
            require(not path.is_symlink(), "EDS storage symlink traversal refused")
            if not path.is_file() or path.suffix not in (".ics", ".db"):
                continue
            require(path.stat().st_uid == context.uid and path.stat().st_size <= 16777216,
                    "EDS storage owner or evidence bound differs")
            if path.suffix == ".ics":
                raw = path.read_bytes()
                text = raw.decode("utf-8")
                unfolded = text.replace("\r\n ", "").replace("\n ", "")
                payloads = ["BEGIN:VEVENT" + part.split("END:VEVENT", 1)[0] + "END:VEVENT"
                            for part in unfolded.split("BEGIN:VEVENT")[1:]]
            else:
                connection = sqlite3.connect(path.as_uri() + "?mode=ro", uri=True)
                try:
                    connection.execute("PRAGMA query_only=ON")
                    payloads = []
                    for (table,) in connection.execute("SELECT name FROM sqlite_master WHERE type='table'"):
                        quoted = '"' + table.replace('"', '""') + '"'
                        columns = [row[1] for row in connection.execute("PRAGMA table_info(" + quoted + ")")]
                        for column in columns:
                            if column.lower() == "vcard":
                                col = '"' + column.replace('"', '""') + '"'
                                payloads.extend(row[0] for row in connection.execute("SELECT " + col + " FROM " + quoted)
                                                if isinstance(row[0], str))
                finally:
                    connection.close()
            files.append({"path": str(path), "bytes": path.stat().st_size})
            for text in payloads:
                require(len(text) <= 65536 and len(records) < 512, "EDS disk evidence exceeded bounds")
                if context.nonce in text:
                    records.append({"path": str(path), "payload": text, "sha256": digest(text.encode())})
    return {"backend": "readonly-real-ICS-or-SQLite-vCard", "files": files, "records": records}


def dispatch(operation, arguments, context):
    if operation in ("dav-start", "dav-state", "dav-restart", "dav-stop"):
        return _dav_control(operation, context)
    if operation == "dav-provision":
        admission = _dav_control("dav-state", context)
        require(admission["ready"], "production DAV service not admitted before source preparation")
        return _user(context, operation, {"directory": str(context.path("data/dav")),
                                         "nonce": context.nonce, "admission": admission})
    if operation == "prepare":
        directory = context.path("data")
        require(not directory.exists(), "data fixture preparation cannot overwrite prior app output")
        directory.mkdir(mode=0o700)
        os.chown(directory, context.uid, context.owner.pw_gid)
        seed = ("Selected Files payload " + context.nonce + "\nKnown UTF-8: 世界\n").encode()
        control = ("Unselected Files control " + context.nonce + "\n").encode()
        for name, raw in (("seed.txt", seed), ("unselected.txt", control)):
            path = context.path("data/" + name)
            path.write_bytes(raw)
            os.chown(path, context.uid, context.owner.pw_gid)
        return {"directory": str(directory), "seed": {"base64": base64.b64encode(seed).decode(), "sha256": digest(seed)},
                "control": {"base64": base64.b64encode(control).decode(), "sha256": digest(control)}}
    if operation == "trash":
        original = context.path(arguments["path"])
        require(str(original).startswith(str(context.path("data")) + "/"), "only owned Files journey Trash paths permitted")
        return _user(context, "trash", {"original": str(original)})
    if operation in ("eds", "remote", "remote-wrong-password", "service", "storage", "service-down", "service-restore"):
        kind = arguments["kind"]
        require(kind in SERVICES, "unknown actual EDS service")
        if operation == "storage":
            return _storage(context, kind)
        if operation.startswith("service-"):
            return _service_control(context, kind, restore=operation == "service-restore")
        if operation in ("remote", "remote-wrong-password"):
            admission = _dav_control("dav-state", context)
            require(admission["ready"], "unavailable-service: admitted production DAV listener unavailable")
            return _user(context, operation, {"kind": kind, "nonce": context.nonce,
                "source_uid": arguments.get("source_uid"), "admission": admission})
        return _user(context, operation, {"kind": kind, "nonce": context.nonce,
                                         "source_uid": arguments.get("source_uid")})
    if operation == "settings":
        require(arguments["app"] in SCHEMAS, "unknown installed data app schema")
        return _user(context, operation, {"app": arguments["app"]})
    if operation == "console-process":
        actor = arguments["actor"]
        require(actor["id"] == "org.gnome.Console" and actor["uid"] == context.uid, "original Console actor required")
        parent = Path("/proc") / str(actor["pid"])
        require(int((parent / "stat").read_text().rsplit(")", 1)[1].split()[19]) == actor["start_ticks"],
                "original Console process changed")
        children = []
        for task in (parent / "task").iterdir():
            for pid in (task / "children").read_text().split():
                proc = Path("/proc") / pid
                if proc.stat().st_uid == context.uid:
                    children.append({"pid": int(pid), "exe": str((proc / "exe").resolve(strict=True)),
                                     "cmdline": (proc / "cmdline").read_bytes().replace(b"\0", b" ").decode(),
                                     "stdin": os.readlink(proc / "fd/0")})
        return {"actor": actor, "children": children}
    raise ValueError("unknown typed data operation")


if __name__ == "__main__":
    if len(sys.argv) != 4 or sys.argv[1] != "--user":
        raise ValueError("only typed original-user data observations accepted")
    print(json.dumps(user_operation(sys.argv[2], json.loads(sys.argv[3]))))
