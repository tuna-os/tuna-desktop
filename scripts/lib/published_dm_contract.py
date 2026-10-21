"""Read-only published-image inventory and pure display-manager contracts.

This module never starts a display manager, authenticates a user, installs a
package, or qualifies a desktop. Its mounted-filesystem report must be bound to
an independently verified container invocation by the caller. Registry prefixes,
cached packages and source intent are not inputs to this inventory.
"""
import argparse
import contextlib
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import stat
import subprocess
import xml.etree.ElementTree as ET


SCHEMA = 1
PROFILES = {
    "published-gdm": {"unit": "gdm.service", "service": "gdm-password",
                      "types": ("wayland",), "fixed_vt": None},
    "current-greetd": {"unit": "greetd.service", "service": "greetd",
                       "types": ("tty", "wayland"), "fixed_vt": 1},
}
GDM_UNIT = "/usr/lib/systemd/system/gdm.service"
TUNA_BINS = tuple("/usr/bin/tuna-" + name for name in (
    "session", "compositor", "shell-host", "shell-gtk", "ibus-bridge", "greeter"))
EXECUTABLES = TUNA_BINS + ("/usr/bin/gdm", "/usr/bin/gnome-shell",
                           "/usr/lib/gdm-session-worker", "/usr/lib/gdm-wayland-session")
REQUIRED_OWNERS = {
    **dict.fromkeys(TUNA_BINS, "tuna-desktop"),
    "/usr/share/wayland-sessions/tuna.desktop": "tuna-desktop",
    "/usr/lib/systemd/user/tuna-session.target": "tuna-desktop",
    "/usr/bin/gdm": "gdm",
    "/usr/bin/gnome-shell": "gnome-shell",
    "/usr/lib/gdm-session-worker": "gdm",
    "/usr/lib/gdm-wayland-session": "gdm",
    GDM_UNIT: "gdm",
    "/etc/gdm/custom.conf": "gdm",
    "/usr/share/gdm/gdm.schemas": "gdm",
}
UNIT_PATHS = ("/etc/systemd/system.control", "/run/systemd/system.control",
              "/run/systemd/transient", "/run/systemd/generator.early",
              "/etc/systemd/system", "/run/systemd/system", "/run/systemd/generator",
              "/etc/systemd/system.attached", "/run/systemd/system.attached",
              "/usr/local/lib/systemd/system", "/usr/lib/systemd/system",
              "/run/systemd/generator.late")
DROPIN_ROOTS = tuple(root + "/" + name for root in UNIT_PATHS for name in
                     ("gdm.service.d", "display-manager.service.d", "service.d"))
UNIT_OVERRIDES = tuple(root + "/" + name for root in UNIT_PATHS
                       for name in ("gdm.service", "display-manager.service")
                       if root + "/" + name not in (GDM_UNIT, "/etc/systemd/system/display-manager.service"))
SESSION_SHADOW_ROOTS = ("/usr/local/share/wayland-sessions", "/usr/local/share/xsessions",
                        "/usr/share/gdm/greeter/wayland-sessions", "/usr/share/gdm/greeter/xsessions")
CONFIG_ROOTS = (
    "/etc/gdm", "/usr/etc/gdm", "/etc/pam.d", "/usr/lib/pam.d",
    "/usr/share/wayland-sessions", "/usr/share/xsessions",
) + DROPIN_ROOTS + SESSION_SHADOW_ROOTS
UNVERIFIED_CHECKS = ("container-image-binding", "effective-gdm-session-search-path",
                     "effective-gdm-executable-search-path", "password-authentication", "desktop-runtime")
DISABLED_LOGIN_DEFAULTS = {"daemon/AutomaticLoginEnable": False, "daemon/TimedLoginEnable": False}
DM_LINKS = (
    "/etc/systemd/system/display-manager.service",
    "/etc/systemd/system/graphical.target.wants/gdm.service",
    "/run/gdm/custom.conf", "/etc/locale.conf",
) + UNIT_OVERRIDES
COMPETING_UNITS = frozenset(("greetd.service", "lightdm.service", "sddm.service",
                            "gdm3.service", "cosmic-greeter.service"))
DIGEST = re.compile(r"sha256:[0-9a-f]{64}\Z")
SERVICE = re.compile(r"[A-Za-z0-9_.-]+\Z")
PAM_LINE = re.compile(r"(-?(?:auth|account|password|session))\s+"
                      r"(\[[^\]]+\]|\S+)\s+(\S+)(?:\s+.*)?\Z")


class ContractError(ValueError):
    """Missing, malformed or contradictory qualification evidence."""


def profile(name):
    if not isinstance(name, str) or name not in PROFILES:
        raise ContractError("an explicit supported display-manager profile is required")
    return PROFILES[name]


def positive_int(value):
    return type(value) is int and value > 0


def validate_session(name, session, compositor, uid, require_active=True):
    """Validate a caller-observed logind/PID join; never discover or assume it.

    The runtime collector must obtain logind_session for the kernel PID, not
    copy the selected session ID into a process record. These pure checks alone
    provide no evidence of UI selection, password authentication or PAM success.
    This validates one observation. Continuity requires the caller to compare
    the returned identity with its baseline, including a separately observed
    boot ID when observations could cross a reboot.
    """
    contract = profile(name)
    if not isinstance(session, dict) or not isinstance(compositor, dict):
        raise ContractError("session and compositor evidence are required")
    if not positive_int(uid) or type(require_active) is not bool:
        raise ContractError("a non-root UID and explicit foreground policy are required")
    sid = session.get("Id")
    if not isinstance(sid, str) or not SERVICE.fullmatch(sid):
        raise ContractError("invalid original logind session ID")
    if (session.get("User") != str(uid) or session.get("Service") != contract["service"]
            or session.get("Class") != "user" or session.get("Type") not in contract["types"]
            or session.get("Seat") != "seat0" or session.get("Remote") != "no"):
        raise ContractError("session owner, service, class, type or local seat differs")
    vt = session.get("VTNr")
    if not isinstance(vt, str) or not re.fullmatch(r"[1-9][0-9]*", vt):
        raise ContractError("a real positive session VT is required")
    if contract["fixed_vt"] is not None and int(vt) != contract["fixed_vt"]:
        raise ContractError("current-build greetd session moved from its configured VT")
    if (session.get("Active"), session.get("State")) not in (("yes", "active"), ("no", "online")):
        raise ContractError("original session has a missing, closing or inconsistent state")
    if require_active and (session.get("Active") != "yes" or session.get("State") != "active"):
        raise ContractError("the original desktop session is not foreground active")
    if (not positive_int(compositor.get("pid")) or not positive_int(compositor.get("start_ticks"))
            or type(compositor.get("uid")) is not int or compositor["uid"] != uid
            or compositor.get("executable") != "/usr/bin/tuna-compositor"
            or compositor.get("logind_session") != sid):
        raise ContractError("original compositor kernel identity does not join the user session")
    return {"profile": name, "session_id": sid, "uid": uid, "vt": int(vt),
            "pid": compositor["pid"], "start_ticks": compositor["start_ticks"]}


def keyfile(text, normalize=True):
    """A strict GLib-style subset: indentation never means continuation."""
    result, section = {}, None
    for raw_line in text.splitlines():
        line = raw_line.strip()
        if not line or line.startswith("#"):
            continue
        if re.fullmatch(r"\[[^\[\]]+\]", line):
            section = line[1:-1]
            section = section.strip().lower() if normalize else section
            if section in result or section == "DEFAULT":
                raise ContractError("malformed or duplicate keyfile section")
            result[section] = {}
            continue
        key, separator, value = line.partition("=")
        key = key.strip().lower() if normalize else key.strip()
        if (section is None or not separator or not key or any(char.isspace() for char in key)
                or key in result[section]):
            raise ContractError("malformed or duplicate keyfile setting")
        result[section][key] = value.strip()
    return result


def validate_gdm_config(text):
    daemon = keyfile(text).get("daemon", {})
    for key in ("automaticloginenable", "timedloginenable"):
        if key in daemon and daemon[key].lower() not in ("false", "0"):
            raise ContractError("automatic or timed GDM login is enabled or ambiguous")


def validate_gdm_defaults(text):
    """Require the installed schema's login defaults, not assumed defaults."""
    # GDM's GMarkup text callbacks do not necessarily coalesce scalar fragments
    # the way ElementTree does. Support the observed plain-text schema subset,
    # not comments/CDATA/entities/processing instructions inside values.
    if "<!" in text or "&" in text or re.search(r"<\?(?!xml\s)", text):
        raise ContractError("unreviewed GDM schema declaration or scalar markup")
    try:
        root = ET.fromstring(text)
    except ET.ParseError as error:
        raise ContractError("malformed GDM defaults schema") from error
    def container_ok(node, children):
        return (not node.attrib and not (node.text or "").strip()
                and all(child.tag in children and not (child.tail or "").strip() for child in node))
    if (root.tag != "gdmschemafile" or len(root) != 1 or root[0].tag != "schemalist"
            or not container_ok(root, {"schemalist"}) or not container_ok(root[0], {"schema"})):
        raise ContractError("unexpected GDM defaults schema structure")
    defaults, keys = {}, set()
    for schema in root[0]:
        if (not container_ok(schema, {"key", "signature", "default", "description"})
                or any(child.attrib or len(child) for child in schema)
                or any(len(schema.findall(tag)) != 1 for tag in ("key", "signature", "default"))
                or len(schema.findall("description")) > 1):
            raise ContractError("unexpected nested or duplicate GDM schema field")
        key = schema.findtext("key")
        if key is None or key in keys or len(schema.findall("key")) != 1:
            raise ContractError("missing or duplicate GDM schema key")
        keys.add(key)
        if key in DISABLED_LOGIN_DEFAULTS:
            if (len(schema.findall("signature")) != 1 or schema.findtext("signature") != "b"
                    or len(schema.findall("default")) != 1 or schema.findtext("default") != "false"):
                raise ContractError("GDM schema login default is enabled or ambiguous")
            defaults[key] = False
    if defaults != DISABLED_LOGIN_DEFAULTS:
        raise ContractError("GDM schema login defaults are missing")
    return defaults


def validate_tuna_entry(text):
    # Desktop Entry groups and keys are case-sensitive, unlike our deliberately
    # conservative GDM config normalization. There are no INI defaults.
    entry = keyfile(text, normalize=False).get("Desktop Entry", {})
    expected = {"Name": "Tuna Desktop", "Exec": "tuna-session", "TryExec": "tuna-session",
                "Type": "Application", "DesktopNames": "Tuna;GNOME;",
                "X-GDM-SessionRegisters": "true"}
    if any(entry.get(key) != value for key, value in expected.items()):
        raise ContractError("shipped Tuna session entry differs from the GDM contract")
    if any(entry.get(key, "false") != "false" for key in ("Hidden", "NoDisplay")):
        raise ContractError("Tuna session entry is hidden")
    if any(key in entry for key in ("OnlyShowIn", "NotShowIn")):
        raise ContractError("Tuna session visibility has an unverified restriction")


def validate_gdm_unit(text):
    allowed = {"ExecStart": "/usr/bin/gdm", "KillMode": "mixed", "Restart": "always",
               "IgnoreSIGPIPE": "no", "BusName": "org.gnome.DisplayManager",
               "EnvironmentFile": "-/etc/locale.conf", "ExecReload": "/bin/kill -SIGHUP $MAINPID",
               "KeyringMode": "shared"}
    section, commands = None, []
    for line in text.splitlines():
        line = line.strip()
        if not line or line.startswith(("#", ";")):
            continue
        if line.startswith("[") and line.endswith("]"):
            section = line[1:-1]
        elif section == "Service":
            key, separator, value = line.partition("=")
            if not separator:
                raise ContractError("unreviewed GDM service syntax")
            if key not in allowed or value != allowed[key]:
                raise ContractError("unreviewed GDM service launch or namespace setting")
            if key == "ExecStart":
                commands.append(value)
    if commands != ["/usr/bin/gdm"]:
        raise ContractError("GDM unit does not launch the shipped executable directly")


def parse_pam(text):
    """Return include services and modules; preserve optional module absence."""
    includes, modules = [], []
    for line in text.replace("\\\n", " ").splitlines():
        line = line.split("#", 1)[0].strip()
        if not line:
            continue
        if line.startswith("@include "):
            target = line.split()[1:]
            if len(target) != 1 or not SERVICE.fullmatch(target[0]):
                raise ContractError("invalid PAM include target")
            includes.append(target[0])
            continue
        match = PAM_LINE.fullmatch(line)
        if not match:
            raise ContractError("unsupported or malformed PAM policy line")
        kind, control, target = match.groups()
        if control.startswith("[") and control.endswith("]"):
            outcomes = set(("success open_err symbol_err service_err system_err buf_err conv_err "
                            "perm_denied maxtries auth_err new_authtok_reqd cred_insufficient "
                            "authinfo_unavail user_unknown cred_unavail cred_expired cred_err "
                            "acct_expired session_err authtok_err authtok_recovery_err "
                            "authtok_lock_busy authtok_disable_aging try_again ignore abort "
                            "authtok_expired module_unknown bad_item conv_again incomplete default").split())
            seen = set()
            for pair in control[1:-1].split():
                key, separator, action = pair.partition("=")
                if (not separator or key not in outcomes or key in seen or
                        not (action in ("ignore", "bad", "die", "ok", "done", "reset")
                             or re.fullmatch(r"[0-9]+", action))):
                    raise ContractError("unverified PAM extended control")
                seen.add(key)
            if not seen:
                raise ContractError("empty PAM extended control")
        elif control not in ("required", "requisite", "sufficient", "optional", "include", "substack"):
            raise ContractError("unsupported PAM control")
        if control in ("include", "substack"):
            if not SERVICE.fullmatch(target):
                raise ContractError("invalid PAM include target")
            includes.append(target)
        else:
            if not (SERVICE.fullmatch(target) or target.startswith("/usr/lib/security/")):
                raise ContractError("unverified PAM module location")
            if "/" in target and (".." in PurePosixPath(target).parts or
                                  str(PurePosixPath(target)) != target):
                raise ContractError("noncanonical PAM module path")
            # A leading '-' only suppresses missing-module logging. Required
            # or requisite modules remain required even when prefixed by '-'.
            modules.append((target, kind.startswith("-") and control == "optional"))
    if not includes and not modules:
        raise ContractError("empty PAM policy")
    return includes, modules


class Root:
    """Resolve image-absolute links inside a mounted root, including test roots."""
    def __init__(self, path):
        self.path = Path(path).resolve()

    def resolve(self, name, follow_final=True):
        if not name.startswith("/") or ".." in PurePosixPath(name).parts:
            raise ContractError("noncanonical image path")
        pending = list(PurePosixPath(name).parts[1:])
        parts, links = [], []
        while pending:
            part = pending.pop(0)
            if part in ("", "."):
                continue
            if part == "..":
                if not parts:
                    raise ContractError("image link escapes its root")
                parts.pop()
                continue
            candidate = self.path.joinpath(*parts, part)
            if candidate.is_symlink() and (pending or follow_final):
                if len(links) >= 40:
                    raise ContractError("image symlink cycle")
                target = os.readlink(candidate)
                links.append({"path": "/" + "/".join(parts + [part]), "target": target})
                if target.startswith("/"):
                    parts = []
                pending = list(PurePosixPath(target).parts[int(target.startswith('/')):]) + pending
            else:
                parts.append(part)
        return self.path.joinpath(*parts), "/" + "/".join(parts), links

    def read_text(self, name):
        path, _, _ = self.resolve(name)
        with open_regular(path) as stream:
            if os.fstat(stream.fileno()).st_size > 1024 * 1024:
                raise ContractError("configuration file exceeds inspection bound")
            content = stream.read(1024 * 1024 + 1)
            if len(content) > 1024 * 1024:
                raise ContractError("configuration grew beyond inspection bound")
            return content.decode()


@contextlib.contextmanager
def open_regular(path):
    # NONBLOCK makes a malicious/replaced FIFO fail without waiting for a
    # writer; NOFOLLOW rejects a final-link replacement after resolution.
    descriptor = os.open(path, os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW | os.O_CLOEXEC)
    with os.fdopen(descriptor, "rb") as stream:
        if not stat.S_ISREG(os.fstat(stream.fileno()).st_mode):
            raise ContractError("configuration/payload is not a regular file")
        yield stream


def query(arguments):
    """The only image commands: bounded, read-only ALPM queries."""
    if not (arguments == ["-Q"] or (isinstance(arguments, list) and len(arguments) == 3
            and arguments[:2] == ["-Qqo", "--"] and isinstance(arguments[2], str)
            and arguments[2].startswith("/"))):
        raise ContractError("only installed-package and path-owner queries are allowed")
    result = subprocess.run(["/usr/bin/pacman", *arguments], stdin=subprocess.DEVNULL,
                            capture_output=True, text=True, timeout=15,
                            env={"PATH": "/usr/bin", "LC_ALL": "C"}, check=False)
    if result.returncode:
        raise ContractError("ALPM read failed: " + " ".join(arguments))
    return result.stdout


class Inventory:
    def __init__(self, root, run_query):
        self.root, self.run_query = Root(root), run_query
        self.files, self.packages, self.errors, self.pam = {}, {}, [], {}
        self.owner_cache = {}
        self.trees = set()
        self.login_defaults = {}

    def owner(self, name):
        if name not in self.owner_cache:
            try:
                owners = self.run_query(["-Qqo", "--", name]).splitlines()
                self.owner_cache[name] = owners[0] if len(owners) == 1 else None
            except (ContractError, OSError, subprocess.SubprocessError):
                self.owner_cache[name] = None
        return self.owner_cache[name]

    def record(self, name):
        if name in self.files:
            return self.files[name]
        path, resolved, links = self.root.resolve(name, follow_final=False)
        result = {"resolved_path": resolved, "parent_links": links}
        if not path.exists() and not path.is_symlink():
            result["type"] = "missing"
        else:
            info = path.lstat()
            result.update(mode=stat.S_IMODE(info.st_mode), uid=info.st_uid, gid=info.st_gid)
            if stat.S_ISLNK(info.st_mode):
                result.update(type="symlink", target=os.readlink(path))
            elif stat.S_ISREG(info.st_mode):
                digest = hashlib.sha256()
                with open_regular(path) as stream:
                    for block in iter(lambda: stream.read(1024 * 1024), b""):
                        digest.update(block)
                result.update(type="file", size=info.st_size, sha256=digest.hexdigest(),
                              owner=self.owner(name))
                after = path.stat()
                identity = lambda item: (item.st_dev, item.st_ino, item.st_size,
                                         item.st_mtime_ns, item.st_ctime_ns)
                if identity(after) != identity(info):
                    raise ContractError("file changed during inventory: " + name)
            elif stat.S_ISDIR(info.st_mode):
                result.update(type="directory")
            else:
                raise ContractError("special file in protected payload: " + name)
        self.files[name] = result
        return result

    def tree(self, name, visiting=()):
        if name in visiting:
            raise ContractError("protected configuration tree link cycle")
        if name in self.trees:
            return
        record = self.record(name)
        if record["type"] == "directory":
            self.trusted_ancestors(name, include_self=True)
            path, _, _ = self.root.resolve(name)
            record["entries"] = sorted(item.name for item in path.iterdir())
            for child in record["entries"]:
                self.tree(name.rstrip("/") + "/" + child, visiting + (name,))
        elif record["type"] == "symlink":
            self.trusted_ancestors(name, include_self=True)
            _, target, _ = self.root.resolve(name)
            if target == name:
                raise ContractError("unresolved protected tree link")
            self.tree(target, visiting + (name,))
        elif record["type"] == "file":
            self.trusted_file(name)
        self.trees.add(name)

    def trusted_file(self, name, executable=False):
        result = self.record(name)
        if result["type"] != "file":
            raise ContractError("required regular shipped file missing: " + name)
        if result["uid"] != 0 or result["mode"] & 0o6022:
            raise ContractError("shipped file owner or write permissions differ: " + name)
        if executable and result["mode"] & 0o111 != 0o111:
            raise ContractError("shipped executable is not executable: " + name)
        self.trusted_ancestors(name)
        return result

    def trusted_ancestors(self, name, include_self=False):
        result = self.record(name)
        ancestry = set()
        for link in result["parent_links"]:
            self.record(link["path"])
            ancestry.add(link["path"])
        for path in (name, result["resolved_path"], *(item["path"] for item in result["parent_links"])):
            ancestry.update(str(parent) for parent in PurePosixPath(path).parents)
        if include_self:
            ancestry.add(name)
        for parent in sorted(ancestry):
            item = self.record(parent)
            if item["type"] not in ("directory", "symlink") or item["uid"] != 0:
                raise ContractError("untrusted shipped parent path: " + parent)
            if item["type"] == "directory" and (item["mode"] & 0o022 or not item["mode"] & 0o001):
                raise ContractError("untrusted shipped parent permissions: " + parent)

    def required(self, name, owner=None, executable=False):
        result = self.trusted_file(name, executable)
        actual_owner = result.get("owner")
        if actual_owner not in self.packages or (owner and actual_owner != owner):
            raise ContractError("installed package ownership differs: " + name)
        return result

    def attempt(self, action):
        try:
            action()
        except (ContractError, OSError, UnicodeError, subprocess.SubprocessError) as error:
            self.errors.append(str(error))

    def packages_inventory(self):
        for line in self.run_query(["-Q"]).splitlines():
            fields = line.split()
            if len(fields) != 2 or fields[0] in self.packages:
                raise ContractError("malformed or duplicate installed package record")
            self.packages[fields[0]] = fields[1]
        if not all(name in self.packages for name in ("gdm", "tuna-desktop", "gnome-shell", "pam")):
            raise ContractError("required installed package is missing")
        if "roost" in self.packages:  # tuna-rename: keep
            raise ContractError("legacy roost package is installed beside tuna-desktop")  # tuna-rename: keep

    def pam_closure(self, service, expected_owner=None, visiting=()):
        if service in visiting:
            raise ContractError("PAM include cycle: " + service)
        if service in self.pam:
            if expected_owner:
                self.required(self.pam[service]["effective_path"], expected_owner)
            return
        choices = ["/etc/pam.d/" + service, "/usr/lib/pam.d/" + service]
        for name in choices:
            self.record(name)
        effective = next((name for name in choices if self.files[name]["type"] != "missing"), None)
        if effective is None:
            raise ContractError("effective PAM service is missing: " + service)
        self.required(effective, expected_owner)
        includes, modules = parse_pam(self.root.read_text(effective))
        observed_modules = []
        for module, optional in modules:
            name = module if module.startswith("/") else "/usr/lib/security/" + module
            record = self.record(name)
            if not (optional and record["type"] == "missing"):
                self.required(name)
            observed_modules.append({"path": name, "optional_absence": optional,
                                     "present": record["type"] != "missing"})
        for included in includes:
            self.pam_closure(included, visiting=visiting + (service,))
        self.pam[service] = {"effective_path": effective, "includes": includes,
                             "modules": observed_modules}

    def manager_links(self):
        for name in DM_LINKS[:2]:
            if self.record(name)["type"] != "symlink" or self.root.resolve(name)[1] != GDM_UNIT:
                raise ContractError("enabled display manager does not resolve to shipped GDM: " + name)
            self.trusted_ancestors(name, include_self=True)
        for name in UNIT_OVERRIDES:
            if self.record(name)["type"] != "missing":
                raise ContractError("unreviewed system GDM unit override: " + name)
        if self.record("/run/gdm/custom.conf")["type"] != "missing":
            raise ContractError("runtime GDM configuration is present in pristine preflight")
        for root_name in DROPIN_ROOTS:
            item = self.record(root_name)
            if item["type"] not in ("missing", "directory") or item.get("entries", []):
                raise ContractError("unreviewed GDM unit drop-in: " + root_name)
            if item["type"] == "directory":
                self.trusted_ancestors(root_name, include_self=True)
        locale = self.record("/etc/locale.conf")
        if locale["type"] != "missing":
            if locale["type"] != "file":
                raise ContractError("unreviewed GDM locale environment link")
            self.trusted_file("/etc/locale.conf")
            for line in self.root.read_text("/etc/locale.conf").splitlines():
                line = line.strip()
                if line and not line.startswith("#") and not re.fullmatch(r"(?:LANG|LANGUAGE|LC_[A-Z_]+)=.*", line):
                    raise ContractError("non-locale GDM environment setting")
        # Scan every search path for extra GDM aliases and aliased competing
        # enablement. Do not hash unrelated services or freeze their directory
        # membership: additive guest tooling may legitimately add those files.
        for root_name in UNIT_PATHS:
            root_record = self.record(root_name)
            if root_record["type"] == "missing":
                continue
            if root_record["type"] != "directory":
                raise ContractError("unreviewed symlinked system unit search directory")
            self.trusted_ancestors(root_name, include_self=True)
            root, _, _ = self.root.resolve(root_name)
            for entry in root.iterdir():
                name = root_name + "/" + entry.name
                if entry.is_symlink() and entry.name.endswith(".service"):
                    if self.root.resolve(name)[1] == GDM_UNIT and name != DM_LINKS[0]:
                        raise ContractError("unreviewed additional GDM unit alias: " + name)
                if not entry.name.endswith((".wants", ".requires")):
                    continue
                if entry.is_symlink() or not entry.is_dir():
                    raise ContractError("unreviewed symlinked system enablement directory: " + name)
                for enabled in entry.iterdir():
                    enabled_name = name + "/" + enabled.name
                    target = self.root.resolve(enabled_name)[1]
                    if enabled.name in COMPETING_UNITS or PurePosixPath(target).name in COMPETING_UNITS:
                        raise ContractError("competing display manager enabled: " + enabled_name)
                    if target == GDM_UNIT and enabled_name != DM_LINKS[1]:
                        raise ContractError("unreviewed additional GDM enablement: " + enabled_name)

    def collect(self):
        self.attempt(self.packages_inventory)
        for name in CONFIG_ROOTS:
            self.attempt(lambda name=name: self.tree(name))
        for name in DM_LINKS:
            self.attempt(lambda name=name: self.record(name))
        for name, owner in REQUIRED_OWNERS.items():
            executable = name in EXECUTABLES
            self.attempt(lambda name=name, owner=owner, executable=executable:
                         self.required(name, owner, executable))
        self.attempt(self.manager_links)
        self.attempt(lambda: validate_gdm_unit(self.root.read_text(GDM_UNIT)))
        self.attempt(lambda: validate_gdm_config(self.root.read_text("/etc/gdm/custom.conf")))
        def read_defaults():
            self.login_defaults = validate_gdm_defaults(self.root.read_text("/usr/share/gdm/gdm.schemas"))
        self.attempt(read_defaults)
        self.attempt(lambda: validate_tuna_entry(self.root.read_text("/usr/share/wayland-sessions/tuna.desktop")))
        for root in SESSION_SHADOW_ROOTS:
            def reject_shadow(root=root):
                if self.record(root + "/tuna.desktop")["type"] != "missing":
                    raise ContractError("unreviewed alternate Tuna session entry: " + root)
            self.attempt(reject_shadow)
        for service, owner in (("gdm-password", "gdm"), ("gdm-launch-environment", "gdm"),
                               ("tuna-lock", "tuna-desktop")):
            self.attempt(lambda service=service, owner=owner: self.pam_closure(service, owner))
        return {"schema_version": SCHEMA, "profile": "published-gdm",
                "evidence_kind": "mounted-filesystem-preflight", "runtime_qualified": False,
                "effective_launch_verified": False, "unverified_checks": list(UNVERIFIED_CHECKS),
                "static_inventory_ready": not self.errors, "errors": self.errors,
                "packages": self.packages, "files": self.files, "pam": self.pam,
                "gdm_login_defaults": self.login_defaults}


def validate_report(report):
    """Check evidence completeness, not authenticity or runtime qualification."""
    if not isinstance(report, dict):
        raise ContractError("an inventory object is required")
    if (type(report.get("schema_version")) is not int or report["schema_version"] != SCHEMA
                or report.get("profile") != "published-gdm"
                or report.get("evidence_kind") != "mounted-filesystem-preflight"
                or report.get("static_inventory_ready") is not True or report.get("errors") != []
                or not isinstance(report.get("packages"), dict) or not report["packages"]
                or not isinstance(report.get("files"), dict) or not report["files"]
                or report.get("effective_launch_verified") is not False
                or report.get("unverified_checks") != list(UNVERIFIED_CHECKS)
                or report.get("gdm_login_defaults") != DISABLED_LOGIN_DEFAULTS
                or any(value is not False for value in report["gdm_login_defaults"].values())
                or report.get("runtime_qualified") is not False):
        raise ContractError("complete passing mounted-image inventories are required")
    subject = report.get("subject", {})
    if not isinstance(subject, dict):
        raise ContractError("invalid image subject")
    image = subject.get("image", "")
    prefix = "ghcr.io/tuna-os/marlin@"
    if (not isinstance(image, str) or not image.startswith(prefix)
                or not DIGEST.fullmatch(image[len(prefix):])
                or not isinstance(subject.get("platform_digest"), str)
                or not DIGEST.fullmatch(subject["platform_digest"])):
        raise ContractError("the declared immutable image subject is missing")
    files, packages, pam = report["files"], report["packages"], report.get("pam")
    if (not {"gdm", "tuna-desktop", "gnome-shell", "pam"} <= packages.keys() or "roost" in packages  # tuna-rename: keep
            or any(not isinstance(name, str) or not name or not isinstance(version, str) or not version
                   for name, version in packages.items())):
        raise ContractError("required installed package evidence is incomplete")
    if not set(REQUIRED_OWNERS).union(CONFIG_ROOTS, DM_LINKS, UNIT_PATHS,
            (root + "/tuna.desktop" for root in SESSION_SHADOW_ROOTS)) <= files.keys():
        raise ContractError("protected file or directory evidence is incomplete")
    if not isinstance(pam, dict) or not {"gdm-password", "gdm-launch-environment", "tuna-lock"} <= pam.keys():
        raise ContractError("PAM closure evidence is incomplete")
    for name, record in files.items():
        if (not isinstance(name, str) or not name.startswith("/") or ".." in PurePosixPath(name).parts
                or not isinstance(record, dict) or record.get("type") not in ("file", "directory", "symlink", "missing")
                or not isinstance(record.get("resolved_path"), str)
                or not record["resolved_path"].startswith("/")
                or ".." in PurePosixPath(record["resolved_path"]).parts
                or not isinstance(record.get("parent_links"), list)):
            raise ContractError("malformed protected file record")
        if record["type"] != "missing" and any(type(record.get(key)) is not int or record[key] < 0
                                               for key in ("mode", "uid", "gid")):
            raise ContractError("missing protected file metadata")
        if record["type"] == "file" and (not isinstance(record.get("sha256"), str)
                or not re.fullmatch(r"[0-9a-f]{64}", record["sha256"])
                or type(record.get("size")) is not int or record["size"] < 0):
            raise ContractError("missing protected file digest")
        if record["type"] == "symlink" and (not isinstance(record.get("target"), str) or not record["target"]):
            raise ContractError("missing symlink target")
        if record["type"] == "directory" and "entries" in record:
            entries = record["entries"]
            if (not isinstance(entries, list) or any(not isinstance(item, str) or not item or
                    item in (".", "..") or "/" in item for item in entries)
                    or entries != sorted(set(entries)) or any(
                        name.rstrip("/") + "/" + item not in files for item in entries)):
                raise ContractError("protected directory member evidence is incomplete")
        for link in record["parent_links"]:
            if (not isinstance(link, dict) or not isinstance(link.get("path"), str)
                    or link["path"] not in files
                    or not isinstance(files[link["path"]], dict)
                    or files[link["path"]].get("type") != "symlink"
                    or files[link["path"]].get("target") != link.get("target")):
                raise ContractError("parent symlink evidence is incomplete")
    def trusted_ancestors(name, include_self=False):
        record = files[name]
        ancestry = set()
        for path in (name, record["resolved_path"], *(link["path"] for link in record["parent_links"])):
            ancestry.update(str(parent) for parent in PurePosixPath(path).parents)
        ancestry.update(link["path"] for link in record["parent_links"])
        if include_self:
            ancestry.add(name)
        for parent in ancestry:
            item = files.get(parent, {})
            if (item.get("type") not in ("directory", "symlink") or item.get("uid") != 0
                    or (item["type"] == "directory" and (item["mode"] & 0o022 or not item["mode"] & 0o001))):
                raise ContractError("trusted ancestor evidence is incomplete")

    def trusted_file(name, owner=None, executable=False, package_owned=True):
        record = files[name]
        if (record["type"] != "file" or record.get("uid") != 0 or record["mode"] & 0o6022
                or (executable and record["mode"] & 0o111 != 0o111)
                or (owner is not None and record.get("owner") != owner)
                or (package_owned and (not isinstance(record.get("owner"), str) or record["owner"] not in packages))):
            raise ContractError("required owned payload evidence differs")
        trusted_ancestors(name)
    for name, owner in REQUIRED_OWNERS.items():
        trusted_file(name, owner, name in EXECUTABLES)
    if files["/etc/locale.conf"]["type"] != "missing":
        trusted_file("/etc/locale.conf", package_owned=False)
    def resolve_record(name):
        pending, parts, count = list(PurePosixPath(name).parts[1:]), [], 0
        while pending:
            part = pending.pop(0)
            if part in ("", "."):
                continue
            if part == "..":
                if not parts:
                    raise ContractError("recorded image link escapes root")
                parts.pop()
                continue
            candidate = "/" + "/".join(parts + [part])
            item = files.get(candidate, {})
            if item.get("type") == "symlink":
                count += 1
                if count > 40:
                    raise ContractError("recorded image symlink cycle")
                target = item["target"]
                if target.startswith("/"):
                    parts = []
                pending = list(PurePosixPath(target).parts[int(target.startswith('/')):]) + pending
            else:
                parts.append(part)
        return "/" + "/".join(parts)

    visited_trees = set()
    def check_tree(name, active=()):
        if name in active:
            raise ContractError("recorded protected tree cycle")
        if name in visited_trees:
            return
        item = files.get(name)
        if item is None:
            raise ContractError("protected tree target evidence is incomplete")
        if item["type"] == "directory":
            trusted_ancestors(name, include_self=True)
            if "entries" not in item:
                raise ContractError("protected directory membership was not enumerated")
            for child in item["entries"]:
                check_tree(name.rstrip("/") + "/" + child, active + (name,))
        elif item["type"] == "symlink":
            trusted_ancestors(name, include_self=True)
            check_tree(resolve_record(name), active + (name,))
        elif item["type"] == "file":
            trusted_file(name, package_owned=False)
        visited_trees.add(name)

    for name in CONFIG_ROOTS:
        check_tree(name)
    if any(files[root + "/tuna.desktop"]["type"] != "missing" for root in SESSION_SHADOW_ROOTS):
        raise ContractError("alternate Tuna session entry exists in static evidence")
    for name in DM_LINKS[:2]:
        if files[name]["type"] != "symlink" or resolve_record(name) != GDM_UNIT:
            raise ContractError("enabled manager target evidence differs")
        trusted_ancestors(name, include_self=True)
    if files["/run/gdm/custom.conf"]["type"] != "missing":
        raise ContractError("pristine inventory contains runtime GDM configuration")
    for name in UNIT_OVERRIDES:
        if files[name]["type"] != "missing":
            raise ContractError("unit override is present in immutable evidence")
    for name in DROPIN_ROOTS:
        if files[name]["type"] not in ("missing", "directory") or files[name].get("entries", []):
            raise ContractError("unit drop-in is present in immutable evidence")
        if files[name]["type"] == "directory":
            trusted_ancestors(name, include_self=True)
    for name in UNIT_PATHS:
        if files[name]["type"] not in ("missing", "directory"):
            raise ContractError("unreviewed system unit search directory")
        if files[name]["type"] == "directory":
            trusted_ancestors(name, include_self=True)
    for service, evidence in pam.items():
        if (not isinstance(service, str) or not SERVICE.fullmatch(service) or not isinstance(evidence, dict)
                or not isinstance(evidence.get("includes"), list) or not isinstance(evidence.get("modules"), list)):
            raise ContractError("malformed PAM closure evidence")
        choices = ("/etc/pam.d/" + service, "/usr/lib/pam.d/" + service)
        if any(name not in files for name in choices):
            raise ContractError("PAM precedence evidence is incomplete")
        effective = next((name for name in choices if files[name]["type"] != "missing"), None)
        item = files.get(effective, {})
        if (effective is None or evidence.get("effective_path") != effective or item.get("type") != "file"
                or not isinstance(item.get("owner"), str) or item["owner"] not in packages
                or item.get("uid") != 0 or item["mode"] & 0o6022
                or (not evidence["includes"] and not evidence["modules"])):
            raise ContractError("effective PAM policy evidence is incomplete")
        if service in ("gdm-password", "gdm-launch-environment", "tuna-lock") and item["owner"] != (
                "tuna-desktop" if service == "tuna-lock" else "gdm"):
            raise ContractError("effective PAM package ownership differs")
        trusted_file(effective)
        if any(not isinstance(name, str) or name not in pam for name in evidence["includes"]):
            raise ContractError("transitive PAM service evidence is incomplete")
        for module in evidence["modules"]:
            if (not isinstance(module, dict) or type(module.get("optional_absence")) is not bool
                    or type(module.get("present")) is not bool or not isinstance(module.get("path"), str)
                    or module["path"] not in files):
                raise ContractError("PAM module evidence is incomplete")
            item = files[module["path"]]
            if (module["present"] != (item["type"] != "missing") or
                    (item["type"] == "missing" and not module["optional_absence"]) or
                    (item["type"] != "missing" and (item["type"] != "file"
                     or not isinstance(item.get("owner"), str) or item["owner"] not in packages
                     or item.get("uid") != 0 or item["mode"] & 0o6022))):
                raise ContractError("required PAM module evidence differs")
            if module["present"]:
                trusted_file(module["path"])
    def visit(service, active=()):
        if service in active:
            raise ContractError("PAM evidence contains an include cycle")
        for child in pam[service]["includes"]:
            visit(child, active + (service,))
    for service in pam:
        visit(service)


def compare_immutable(before, after):
    """Refuse payload/ownership/config changes while allowing additive packages."""
    for report in (before, after):
        validate_report(report)
    if before.get("subject") != after.get("subject"):
        raise ContractError("image subject changed")
    if any(after["packages"].get(name) != version for name, version in before["packages"].items()):
        raise ContractError("a shipped package was removed or changed version")
    if before["files"] != after["files"] or before.get("pam") != after.get("pam"):
        raise ContractError("shipped payload, ownership, symlink or configuration membership changed")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", choices=("published-gdm",), required=True)
    parser.add_argument("--image", required=True, help="immutable ghcr.io/tuna-os/marlin digest reference")
    parser.add_argument("--platform-digest", required=True)
    args = parser.parse_args(argv)
    prefix = "ghcr.io/tuna-os/marlin@"
    if (not args.image.startswith(prefix) or not DIGEST.fullmatch(args.image[len(prefix):])
            or not DIGEST.fullmatch(args.platform_digest)):
        parser.error("both image and platform must use full sha256 digests")
    report = Inventory("/", query).collect()
    report["subject"] = {"image": args.image, "platform_digest": args.platform_digest,
                         "binding": "caller-declared; verify the read-only container invocation separately"}
    print(json.dumps(report, indent=2, sort_keys=True))
    return 0 if report["static_inventory_ready"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
