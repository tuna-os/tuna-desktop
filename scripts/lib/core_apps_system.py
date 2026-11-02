"""Six native GUI operation journeys, with independent bounded guest readbacks.

The service helpers prepare inputs or inspect outcomes; they never replace the
Connections, Software, Disks, System Monitor, Logs or Baobab operation under test.
All selectors bind to the actual original app's freshly observed accessible tree.

Software requires the genuine org.gnome.gitg/stable Flathub catalogue, current
and retained historical signed commits and their runtime objects. DNS, HTTPS to
flathub.org/dl.flathub.org and repository-selected CDN/runtime endpoints must be
reachable within the existing bounded actions. An absent/pruned old deployment,
missing native Flatpak plugin or unavailable network is a failed prerequisite,
never an alternative repository, fabricated update or immutable-system install.
"""
from __future__ import annotations

import base64
import re
import shlex


def _require(value, message):
    if not value:
        raise RuntimeError(message)


def _visible(tree):
    return [node for node in tree["nodes"] if node.get("showing")]


def _strings(tree):
    return "\n".join(str(node.get(key, "")) for node in _visible(tree) for key in ("name", "text"))


def _content_strings(tree):
    return "\n".join(str(node.get(key, "")) for node in _visible(tree)
                     if not node.get("editable") and node.get("role") not in ("entry", "password text")
                     for key in ("name", "text"))


def _node(desktop, actor, names, roles=None):
    if isinstance(names, str):
        names = (names,)
    nodes = _visible(desktop.tree(actor))
    matches = [n for n in nodes if n.get("name") in names
               and (roles is None or n.get("role") in roles) and n.get("sensitive", True)]
    if len(matches) > 1:
        focused = [n for n in nodes if n.get("focused")]
        dialogs = [n for n in nodes if n.get("role") in ("dialog", "alert") and any(
            f["path"][:len(n["path"])] == n["path"] for f in focused)]
        if dialogs:
            dialog = max(dialogs, key=lambda n: len(n["path"]))
            matches = [n for n in matches if n["path"][:len(dialog["path"])] == dialog["path"]]
    _require(len(matches) == 1, "expected one live native control " + repr(names) + "; found " + str(len(matches)))
    return matches[0]


def _click(desktop, actor, *names, roles=None):
    node = _node(desktop, actor, names, roles)
    desktop.click_path(actor, node["path"])
    return node


def _shown(desktop, actor, *names):
    return any(n.get("name") in names for n in _visible(desktop.tree(actor)))


def _await_text(desktop, actor, label, text, present=True, timeout=10):
    return desktop.await_state(label, lambda: desktop.tree(actor),
                               lambda tree: (text in _content_strings(tree)) == present, timeout=timeout)


def _focused_entry(desktop, actor):
    entries = [n for n in _visible(desktop.tree(actor)) if n.get("focused") and n.get("editable")
               and n.get("role") != "password text"]
    _require(len(entries) == 1, "native keyboard action did not focus exactly one editable entry")
    return entries[0]


def _write_entry(desktop, actor, node, value):
    desktop.click_path(actor, node["path"])
    desktop.keys("ctrl", "a")
    desktop.type_ascii(value)
    actual = _focused_entry(desktop, actor)
    _require(actual.get("text") == value, "physical keyboard entry consumer differs from requested text")
    return actual


def _search(desktop, actor, query):
    entries = [n for n in _visible(desktop.tree(actor)) if n.get("editable")
               and n.get("role") != "password text"
               and (n.get("focused") or "search" in n.get("name", "").lower())]
    if entries:
        _require(len(entries) == 1, "native search entry is ambiguous")
        field = entries[0]
    else:
        desktop.keys("ctrl", "f")
        field = _focused_entry(desktop, actor)
    return _write_entry(desktop, actor, field, query)


def _ime_search(desktop, actor, query):
    field = _search(desktop, actor, query)
    result = desktop.ime(actor, field.get("name", ""), field_path=field["path"])
    restored = _write_entry(desktop, actor, _focused_entry(desktop, actor), query)
    return {"composition": result, "restored": restored}


def _copy_entry(desktop, actor, expected):
    _require(_focused_entry(desktop, actor).get("text") == expected, "clipboard source entry is not focused exact text")
    desktop.keys("ctrl", "a")
    desktop.keys("ctrl", "c")
    result = desktop.clipboard()
    _require(base64.b64decode(result["base64"]).decode() == expected, "native clipboard does not contain selected app text")
    return result


def _row_control(desktop, actor, text, name):
    tree = desktop.tree(actor)
    nodes = _visible(tree)
    rows = [node for node in nodes if node.get("role") in ("list item", "table row")
            and any(text in child.get("name", "") and child["path"][:len(node["path"])] == node["path"]
                    for child in nodes)]
    matches = [child for row in rows for child in nodes
               if child.get("name") == name and child.get("sensitive")
               and child["path"][:len(row["path"])] == row["path"]]
    unique = {tuple(node["path"]): node for node in matches}
    _require(len(unique) == 1, "expected exactly one row-owned native control for " + text + ": " + name)
    node = next(iter(unique.values()))
    desktop.click_path(actor, node["path"])
    return {"tree": tree, "control": node}


def _select_table_row(desktop, actor, name):
    nodes = _visible(desktop.tree(actor))
    tables = [node for node in nodes if node.get("role") in ("table", "tree table", "tree")
              and any(child.get("name") == name and child["path"][:len(node["path"])] == node["path"]
                      for child in nodes)]
    _require(len(tables) == 1, "native result table is missing or ambiguous")
    path = tables[0]["path"]
    # Keyboard traversal selects without GtkColumnView's single-click activation.
    for _ in range(len(nodes) + 1):
        tree = desktop.tree(actor)
        if any(node.get("focused") and node["path"][:len(path)] == path for node in _visible(tree)):
            break
        desktop.keys("tab")
    else:
        raise RuntimeError("native keyboard traversal did not enter result table")
    desktop.keys("home")
    for _ in range(len(nodes) + 1):
        tree = desktop.tree(actor)
        visible = _visible(tree)
        if any(node.get("selected") and node["path"][:len(path)] == path
               and (node.get("name") == name or any(
                   child.get("name") == name and child["path"][:len(node["path"])] == node["path"]
                   for child in visible)) for node in visible):
            return tree
        desktop.keys("down")
    raise RuntimeError("native keyboard selection did not reach exact result row " + name)


def _record(desktop, actor, operation, evidence):
    evidence["native_interfaces"] = desktop.guest("system", "interfaces", app_id=actor["id"])
    extras = {}
    if "ime" not in evidence:
        applicability = evidence.get("ime_applicability")
        _require(isinstance(applicability, dict) and applicability.get("status") == "not-applicable"
                 and applicability.get("original_interface") and applicability.get("reason"),
                 "system app lacks actual original-frontend IME evidence or verified specific interface applicability")
        extras["ime_applicability"] = applicability
    if actor["id"] == "org.gnome.baobab":
        evidence["portal_applicability"] = "Actual native folder picker grant/cancel and original caller restoration exercised; picker-owned IME is interoperability only, not original-app composition"
    elif actor["id"] == "org.gnome.DiskUtility":
        evidence["portal_applicability"] = "Native destructive-format confirmation granted/cancelled; real Files clipboard consumer transfers data without a portal grant API"
    else:
        evidence["portal_applicability"] = "Principal operation uses native application/session services, not a portal grant; installed desktop MIME declarations and applicable actual handler consumption retained"
    return desktop.result(actor, operation, evidence, **extras)


def _connections(desktop):
    endpoint = desktop.guest("system", "remote-prepare")
    actor = desktop.start("org.gnome.Connections", "Connections")
    evidence = {"endpoint": endpoint}
    if _shown(desktop, actor, "No Thanks"):
        _click(desktop, actor, "No Thanks", roles=("push button", "button"))
    if _shown(desktop, actor, "Continue"):
        _click(desktop, actor, "Continue", roles=("push button", "button"))
    _click(desktop, actor, "New", "New Connection", "Add Connection", roles=("push button", "button", "toggle button"))
    entry = _focused_entry(desktop, actor)
    evidence["ime"] = desktop.ime(actor, entry.get("name", ""), field_path=entry["path"])
    _write_entry(desktop, actor, _focused_entry(desktop, actor), endpoint["uri"])
    evidence["uri_clipboard"] = _copy_entry(desktop, actor, endpoint["uri"])
    desktop.keys("esc")
    _require(desktop.guest("system", "remote-read")["result"] is None, "cancelled connection editor produced remote output")
    evidence["native_vnc_handler"] = desktop.open_default(endpoint["descriptor"])
    desktop.select(actor)

    def password(current, value):
        tree = desktop.await_state("connections-password", lambda: desktop.tree(current),
            lambda data: any(n.get("role") == "password text" for n in _visible(data)))
        matches = [n for n in _visible(tree) if n.get("role") == "password text"]
        _require(len(matches) == 1, "remote password field missing/ambiguous")
        desktop.click_path(current, matches[0]["path"])
        desktop.keys("ctrl", "a")
        desktop.type_ascii(value)
        _click(desktop, current, "Sign In", "Authenticate", "Connect", roles=("push button", "button"))

    def reopen(current):
        saved = _await_text(desktop, current, "connections-saved-endpoint", "127.0.0.1")
        names = [n for n in _visible(saved) if n.get("name") == "127.0.0.1" and n.get("role") == "label"]
        _require(len(names) == 1, "saved native endpoint is missing or ambiguous")
        desktop.click_path(current, names[0]["path"])
        ready = desktop.await_state("connections-auth-or-rfb", lambda: desktop.tree(current),
            lambda data: any(n.get("role") in ("password text", "drawing area") for n in _visible(data)))
        if any(n.get("role") == "password text" for n in _visible(ready)):
            password(current, endpoint["password"])
        return saved

    def remote_focus(current):
        tree = desktop.await_state("connections-real-rfb-display", lambda: desktop.tree(current),
            lambda data: any(n.get("role") == "drawing area" for n in _visible(data)))
        areas = [n for n in _visible(tree) if n.get("role") == "drawing area"]
        _require(len(areas) == 1, "real native RFB canvas is missing/ambiguous")
        desktop.click_path(current, areas[0]["path"])

    password(actor, "NotThePassword")
    evidence["wrong_auth"] = desktop.await_state("connections-wrong-password", lambda: desktop.tree(actor),
        lambda tree: "authentication failed" in _strings(tree).lower())
    denied = desktop.guest("system", "remote-read")
    _require(denied["result"] is None, "wrong-auth control unexpectedly executed remote command")
    evidence["wrong_auth_backend"] = denied
    reopen(actor)
    remote_focus(actor)
    command = "printf '%s\\n' " + shlex.quote(endpoint["token"]) + " > " + shlex.quote(endpoint["output"]) + "; cat " + shlex.quote(endpoint["output"])
    desktop.type_ascii(command)
    desktop.keys("ret")
    typed = desktop.await_state("connections-remote-command", lambda: desktop.guest("system", "remote-read"),
                                 lambda state: state["result"] == endpoint["token"] + "\n")
    evidence["typed_command"] = {"command": command, "readback": typed, "frame": desktop.observe("connections-remote-output", actor)}
    copy_command = "printf %s " + shlex.quote(endpoint["token"]) + " | xclip -selection clipboard"
    desktop.type_ascii(copy_command)
    desktop.keys("ret")
    remote_clip = desktop.await_state("connections-remote-clipboard", lambda: desktop.guest("system", "remote-read"),
        lambda state: state["clipboard_status"]["returncode"] == 0 and state["clipboard"] == endpoint["token"])
    local_clip = desktop.await_state("connections-local-clipboard", desktop.clipboard,
        lambda state: base64.b64decode(state["base64"]).decode() == endpoint["token"])
    evidence["clipboard"] = {"remote": remote_clip, "local": local_clip}
    _click(desktop, actor, "Disconnect", roles=("push button", "button"))
    evidence["disconnected_ui"] = _await_text(desktop, actor, "connections-disconnected-saved-endpoint", "127.0.0.1")
    evidence["disconnected_backend"] = desktop.await_state("connections-native-disconnect", lambda: desktop.guest("system", "remote-read"),
        lambda state: not state["established"])
    evidence["endpoint_stop"] = desktop.guest("system", "remote-stop")
    revived = desktop.guest("system", "remote-prepare")
    _require(revived["xvnc"]["start_ticks"] != endpoint["xvnc"]["start_ticks"], "endpoint restart did not change real packaged server process")
    evidence["restarted_endpoint"] = revived
    reopen(actor)
    last_result = typed["result"]
    def probe(current, phase):
        nonlocal last_result
        unchanged = desktop.guest("system", "remote-read")
        _require(unchanged["result"] == last_result,
                 "remote consumer payload changed between actual scale/lock/restart observations")
        desktop.select(current)
        remote_focus(current)
        token = endpoint["token"] + "_" + phase
        command = "printf '%s\\n' " + shlex.quote(token) + " > " + shlex.quote(endpoint["output"])
        desktop.type_ascii(command)
        desktop.keys("ret")
        state = desktop.await_state("connections-" + phase, lambda: desktop.guest("system", "remote-read"),
                                     lambda data: data["result"] == token + "\n")
        _require(bool(state["established"]), "remote operation has no actual RFB TCP connection")
        last_result = state["result"]
        return {"unchanged_before_operation": unchanged, "consumer": state,
                "frame": desktop.observe("connections-" + phase, current)}
    def restart_probe(current, phase):
        return {"saved_endpoint": reopen(current), "operation": probe(current, phase)}
    desktop.crosscut(actor, probe, restart_probe=restart_probe)
    return _record(desktop, actor, "packaged-xvnc-remote-command-auth-disconnect-restart", evidence)


def _software(desktop):
    config = desktop.metadata.get("core_apps_software")
    _require(isinstance(config, dict) and config.get("app_id") == "org.gnome.gitg"
             and config.get("branch", "stable") == "stable",
             "Software requires the real org.gnome.gitg/stable Flathub historical-update fixture")
    args = {"app_id": config["app_id"], "branch": config.get("branch", "stable")}
    remote = desktop.guest("system", "flatpak-remote-prepare", **args)
    catalog = desktop.guest("system", "flatpak-catalog-prepare", **args)
    fixture = desktop.guest("system", "flatpak-history-prepare", **args)
    actor = desktop.start("org.gnome.Software", "Software")
    evidence = {"remote": fixture, "remote_preparation": remote, "catalog_preparation": catalog,
                "scope": "ordinary-UID1000-supported-user-Flathub"}
    if _shown(desktop, actor, "Let's Go Shopping", "Let’s Go Shopping"):
        _click(desktop, actor, "Let's Go Shopping", "Let’s Go Shopping", roles=("push button", "button"))
    evidence["ime"] = _ime_search(desktop, actor, args["app_id"])
    evidence["search_clipboard"] = _copy_entry(desktop, actor, args["app_id"])
    _await_text(desktop, actor, "software-real-appstream-result", "gitg", timeout=30)
    evidence["native_flatpakref_handler"] = desktop.open_default(fixture["flatpakref"])
    desktop.select(actor)
    _await_text(desktop, actor, "software-user-install-detail", "Install")
    # Software's source selector must explicitly select the user installation.
    _click(desktop, actor, "Flathub", "Flatpak", roles=("push button", "button", "combo box", "toggle button"))
    source_tree = desktop.tree(actor)
    user_rows = []
    for node in _visible(source_tree):
        if node.get("role") != "list item":
            continue
        subtree = {"nodes": [n for n in _visible(source_tree) if n["path"][:len(node["path"])] == node["path"]]}
        labels = _content_strings(subtree)
        if "Flathub" in labels and any(n.get("name") == "User" for n in subtree["nodes"]):
            user_rows.append(node)
    _require(len(user_rows) == 1, "native Software must expose exactly one real user Flathub source")
    evidence["native_user_source"] = source_tree
    desktop.click_path(actor, user_rows[0]["path"])
    absent = desktop.guest("system", "flatpak-read", **args)
    _require(not absent["installed"], "cancelled detail browsing installed the app")
    _click(desktop, actor, "Install", roles=("push button", "button"))
    installed = desktop.await_state("software-native-ui-install", lambda: desktop.guest("system", "flatpak-read", **args),
        lambda state: state["installed"] and state["commit"] == fixture["current_commit"], timeout=30)
    _require(installed["verified_signatures"] and installed["runtime_commit"], "native install lacks trusted exact app/runtime identities")
    evidence["installed"] = installed
    # Native remove confirmation is exercised negatively before actual update.
    _click(desktop, actor, "Uninstall", "Remove", roles=("push button", "button"))
    _click(desktop, actor, "Cancel", roles=("push button", "button"))
    cancelled = desktop.guest("system", "flatpak-read", **args)
    _require(cancelled["installed"] and cancelled["commit"] == installed["commit"], "cancelled Software removal changed installed state")
    evidence["cancel_remove"] = cancelled
    preparation = desktop.guest("system", "flatpak-old-baseline", commit=fixture["previous_commit"], **args)
    baseline = desktop.guest("system", "flatpak-read", **args)
    _require(baseline["installed"] and baseline["commit"] == fixture["previous_commit"],
             "supported user Flatpak historical update precondition was not installed")
    evidence["explicit_update_precondition_not_gui_proof"] = {"preparation": preparation, "readback": baseline}
    desktop.keys("alt", "left")
    _click(desktop, actor, "Updates", roles=("page tab", "push button", "button"))
    # Ctrl+R is the supported native refresh, not a helper-performed update.
    desktop.keys("ctrl", "r")
    _await_text(desktop, actor, "software-real-update-available", "gitg", timeout=30)
    _click(desktop, actor, "Update", "Update All", roles=("push button", "button"))
    updated = desktop.await_state("software-native-ui-update", lambda: desktop.guest("system", "flatpak-read", **args),
        lambda state: state["installed"] and state["commit"] == fixture["current_commit"], timeout=30)
    _require(updated["permissions_sha256"] == installed["permissions_sha256"] and updated["runtime"] == installed["runtime"],
             "updated app permissions/runtime differ from genuine current install")
    evidence["updated"] = updated
    def probe(current, phase):
        desktop.select(current)
        _search(desktop, current, args["app_id"])
        frame = _await_text(desktop, current, "software-search-" + phase, "gitg", timeout=30)
        state = desktop.guest("system", "flatpak-read", **args)
        _require(state["installed"] and state["commit"] == updated["commit"] and state["permissions_sha256"] == updated["permissions_sha256"], "Software persistence/readback changed exact installed content")
        return {"search": frame, "installed": state, "frame": desktop.observe("software-" + phase, current)}
    def restart_probe(current, phase):
        data = probe(current, phase)
        _click(desktop, current, "gitg", roles=("label",))
        _await_text(desktop, current, "software-remove-after-restart", "Uninstall")
        _click(desktop, current, "Uninstall", "Remove", roles=("push button", "button"))
        _click(desktop, current, "Uninstall", "Remove", roles=("push button", "button"))
        removed = desktop.await_state("software-native-ui-remove", lambda: desktop.guest("system", "flatpak-read", **args),
                                       lambda state: not state["installed"], timeout=30)
        data["native_remove"] = removed
        return data
    desktop.crosscut(actor, probe, restart_probe=restart_probe)
    return _record(desktop, actor, "user-flathub-native-install-update-remove", evidence)


def _disks(desktop):
    fixture = desktop.guest("system", "disk-prepare")
    actor = desktop.start("org.gnome.DiskUtility", "Disks")
    evidence = {"fixture": fixture}
    device = fixture["before"]["device"]

    def select_disposable(current):
        tree = desktop.tree(current)
        rows = [n for n in _visible(tree) if n.get("role") in ("table cell", "list item")]
        candidates = [n for n in rows if device in n.get("name", "")]
        if not candidates:
            candidates = [n for n in rows if _display_size_matches(n.get("name", ""), int(fixture["before"]["disk"]["size"]))]
        _require(len(candidates) == 1, "native Disks cannot uniquely select the disposable device")
        desktop.click_path(current, candidates[0]["path"])
        tree = desktop.tree(current)
        if fixture["before"]["serial"] not in _content_strings(tree):
            _click(desktop, current, "Drive Information", roles=("push button", "button"))
            tree = _await_text(desktop, current, "disks-selected-serial", fixture["before"]["serial"])
            desktop.keys("esc")
        _require(fixture["before"]["serial"] in _content_strings(tree), "native selected device serial differs from admitted removable fixture")
        return tree

    def format_dialog():
        _click(desktop, actor, "Drive Options", "Drive options", roles=("push button", "button", "toggle button"))
        _click(desktop, actor, "Format Disk…", "Format Disk...", roles=("menu item", "push button", "button"))
        tree = desktop.tree(actor)
        combos = [n for n in _visible(tree) if n.get("role") == "combo box" and n.get("name") == "Partitioning"]
        if combos:
            _require(len(combos) == 1, "partition-table selector is ambiguous")
            desktop.click_path(actor, combos[0]["path"])
            tree = desktop.tree(actor)
        for role in ("menu item", "radio button", "check box", "label"):
            options = [n for n in _visible(tree) if "GPT" in n.get("name", "") and n.get("role") == role]
            if options:
                _require(len(options) == 1, "native GPT choice is ambiguous")
                desktop.click_path(actor, options[0]["path"])
                break
        else:
            raise RuntimeError("native Disks does not expose supported GPT partitioning")

    def filesystem(state):
        matches = [n for n in state["filesystems"] if n.get("fstype") == "ext4" and n.get("label") == fixture["label"]]
        if len(matches) != 1:
            return None
        fs = matches[0]
        _require(64 * 1024 ** 2 <= int(fs["size"]) <= 224 * 1024 ** 2, "native partition size did not reserve independent negative block region")
        _require(int(fs["start"]) * 512 + int(fs["size"]) <= state["negative_block_offset"], "native partition overlaps unselected negative block control")
        _require(state["unallocated_mib_sha256"] == fixture["before"]["unallocated_mib_sha256"], "native operation changed unallocated block control")
        return fs

    def mount(current):
        _row_control(desktop, current, fixture["label"], "Partition Options")
        _click(desktop, current, "Mount", roles=("menu item",))
        state = desktop.await_state("disks-native-mount", lambda: desktop.guest("system", "disk-read"),
                                    lambda data: bool(data["mounted"]))
        _await_text(desktop, current, "disks-native-mounted-path", state["mounted"][0])
        return state

    def unmount(current):
        _row_control(desktop, current, fixture["label"], "Partition Options")
        _click(desktop, current, "Unmount", roles=("menu item",))
        return desktop.await_state("disks-native-unmount", lambda: desktop.guest("system", "disk-read"), lambda state: not state["mounted"])

    def payload_matches(state, raw=False):
        return any(p["sha256"] == fixture["payload_sha256"] and p["negative_sha256"] == fixture["negative_sha256"]
                   for p in state["raw_payloads" if raw else "payloads"])

    evidence["selected_device"] = select_disposable(actor)
    evidence["actual_active_allow_inactive_deny_policy"] = desktop.guest("system", "disk-policy", actor=actor)
    format_dialog()
    _click(desktop, actor, "Format", "Format…", roles=("push button", "button"))
    _click(desktop, actor, "Cancel", roles=("push button", "button"))
    denied = desktop.guest("system", "disk-read")
    _require(denied["device_sha256"] == fixture["before"]["device_sha256"] and not denied["mounted"], "cancelled destructive confirmation changed disposable storage")
    evidence["cancelled_destructive_consent"] = denied
    # The original format dialog survives a rejected destructive confirmation.
    _click(desktop, actor, "Format", "Format…", roles=("push button", "button"))
    _click(desktop, actor, "Format", roles=("push button", "button"))
    evidence["native_gpt_format"] = desktop.await_state("disks-native-gpt-format", lambda: desktop.guest("system", "disk-read"),
        lambda state: state["disk"].get("pttype") == "gpt" and state["device_sha256"] != fixture["before"]["device_sha256"])
    _click(desktop, actor, "Create partition in unallocated space", "Create Partition", "Create partition", roles=("push button", "button"))
    # Select the real Units popover's MB option, then physically edit its spin row.
    unit_nodes = [n for n in _visible(desktop.tree(actor)) if n.get("role") == "combo box" and n.get("name") in ("Units", "Unit")]
    if unit_nodes:
        _require(len(unit_nodes) == 1, "native partition units control is ambiguous")
        desktop.click_path(actor, unit_nodes[0]["path"])
    else:
        _click(desktop, actor, "Units", roles=("label",))
    option_tree = desktop.tree(actor)
    options = [n for n in _visible(option_tree) if n.get("name") == "MB" and n.get("role") in ("menu item", "list item")]
    if not options:
        lists = [n for n in _visible(option_tree) if n.get("role") in ("list", "menu") and
                 all(any(c.get("name") == unit and c["path"][:len(n["path"])] == n["path"] for c in _visible(option_tree))
                     for unit in ("MB", "GB", "MiB"))]
        _require(len(lists) == 1, "cannot identify real units option list")
        options = [n for n in _visible(option_tree) if n.get("name") == "MB" and n["path"][:len(lists[0]["path"])] == lists[0]["path"]]
    _require(len(options) == 1, "native MB partition-size unit is missing/ambiguous")
    desktop.click_path(actor, options[0]["path"])
    size = _node(desktop, actor, ("Partition Size", "Partition size"), ("spin button",))
    desktop.click_path(actor, size["path"])
    desktop.keys("ctrl", "a")
    desktop.type_ascii("128")
    desktop.keys("tab")
    _click(desktop, actor, "Next", roles=("push button", "button"))
    name = _node(desktop, actor, ("Volume Name", "Volume name"), ("text", "entry"))
    desktop.click_path(actor, name["path"])
    evidence["ime"] = desktop.ime(actor, name.get("name", ""), field_path=name["path"])
    _write_entry(desktop, actor, _focused_entry(desktop, actor), fixture["label"])
    evidence["volume_label_clipboard"] = _copy_entry(desktop, actor, fixture["label"])
    for role in ("radio button", "check box", "label"):
        types = [n for n in _visible(desktop.tree(actor)) if "ext4" in n.get("name", "").lower() and n.get("role") == role]
        if types:
            _require(len(types) == 1, "native Ext4 filesystem choice is ambiguous")
            desktop.click_path(actor, types[0]["path"])
            break
    else:
        raise RuntimeError("native Ext4 format option missing")
    _click(desktop, actor, "Create", roles=("push button", "button"))
    formatted = desktop.await_state("disks-native-ext4-create", lambda: desktop.guest("system", "disk-read"), lambda state: filesystem(state) is not None, timeout=30)
    fs_identity = filesystem(formatted)
    _require(bool(fs_identity.get("uuid")), "native formatted filesystem lacks actual persistent UUID")
    evidence["native_ext4"] = formatted
    mounted = mount(actor)
    _require(len(mounted["mounted"]) == 1, "disposable fixture mounted at ambiguous destinations")
    destination = mounted["mounted"][0]
    files = desktop.start("org.gnome.Nautilus", "Files")
    desktop.keys("ctrl", "l")
    desktop.type_ascii(fixture["source"])
    desktop.keys("ret")
    _await_text(desktop, files, "disks-files-source", "qualification.txt")
    _await_text(desktop, files, "disks-files-negative-source", "negative.bin")
    desktop.keys("ctrl", "a")
    desktop.keys("ctrl", "c")
    copied = desktop.clipboard(mime="text/uri-list")
    uris = {line.strip() for line in base64.b64decode(copied["base64"]).decode().splitlines() if line.strip() and not line.startswith("#")}
    _require(uris == {"file://" + fixture["source"] + "/qualification.txt", "file://" + fixture["source"] + "/negative.bin"}, "native Files clipboard did not select exact owned disk payload controls")
    desktop.keys("ctrl", "l")
    desktop.type_ascii(destination)
    desktop.keys("ret")
    _await_text(desktop, files, "disks-files-mounted-destination", fixture["label"])
    desktop.keys("ctrl", "v")
    data = desktop.await_state("disks-native-files-payload-copy", lambda: desktop.guest("system", "disk-read"), payload_matches)
    evidence["native_files_consumer"] = {"actor": files, "clipboard": copied, "readback": data, "frame": desktop.observe("disks-files-payload", files)}
    desktop.close(files)
    desktop.select(actor)
    evidence["unmounted"] = unmount(actor)
    _require(payload_matches(evidence["unmounted"], raw=True), "unmounted ext4 raw payload does not match actual GUI-copied bytes")

    def probe(current, phase):
        desktop.select(current)
        _await_text(desktop, current, "disks-filesystem-label-" + phase, fixture["label"])
        desktop.keys("tab")
        focus = desktop.tree(current)
        _require(any(n.get("focused") for n in _visible(focus)), "Disks native keyboard navigation has no focused control")
        state = desktop.guest("system", "disk-read")
        observed = filesystem(state)
        _require(observed is not None and observed["uuid"] == fs_identity["uuid"] and not state["mounted"], "native filesystem UUID/mount persistence changed")
        _require(payload_matches(state, raw=True), "persisted filesystem content differs from actual native data operation")
        return {"device": state, "focus": focus, "frame": desktop.observe("disks-" + phase, current)}

    def restart_probe(current, phase):
        selected = select_disposable(current)
        data = probe(current, phase)
        data["selected_serial"] = selected
        mount(current)
        data["persistent_payload"] = desktop.await_state("disks-persistent-remount", lambda: desktop.guest("system", "disk-read"), payload_matches)
        data["final_unmounted"] = unmount(current)
        return data
    desktop.crosscut(actor, probe, restart_probe=restart_probe)
    return _record(desktop, actor, "serial-isolated-removable-native-format-data-mount-unmount", evidence)


def _monitor(desktop):
    fixture = desktop.guest("system", "process-prepare")
    selected, negative = fixture["selected"], fixture["unselected"]
    _require(selected is not None and selected["nice"] == negative["nice"] == 0, "controlled process baseline invalid")
    actor = desktop.start("org.gnome.SystemMonitor", "System Monitor")
    _click(desktop, actor, "Processes", roles=("page tab", "push button", "button"))
    evidence = {"processes": fixture, "ime": _ime_search(desktop, actor, selected["comm"])}
    evidence["search_clipboard"] = _copy_entry(desktop, actor, selected["comm"])
    def select_process(current):
        _search(desktop, current, selected["comm"])
        tree = _await_text(desktop, current, "monitor-selected-real-process", selected["comm"])
        _require(negative["comm"] not in _strings(tree), "System Monitor search shows unselected negative process")
        cells = [n for n in _visible(tree) if n.get("name") == selected["comm"] and n.get("role") in ("table cell", "list item")]
        _require(len(cells) == 1, "real selected process row missing or ambiguous")
        desktop.click_path(current, cells[0]["path"])
        return tree
    select_process(actor)
    desktop.keys("shift", "f10")
    _click(desktop, actor, "Change Priority", roles=("menu item",))
    _click(desktop, actor, "Very Low", roles=("menu item", "radio menu item"))
    low = desktop.await_state("monitor-native-priority-change", lambda: desktop.guest("system", "process-read"),
        lambda state: state["selected"] is not None and state["selected"]["nice"] == 19)
    _require(low["unselected"]["nice"] == 0 and low["unselected"]["start_ticks"] == negative["start_ticks"], "selected GUI priority operation affected unselected control")
    evidence["priority"] = low
    select_process(actor)
    desktop.keys("shift", "f10")
    _click(desktop, actor, "End…", roles=("menu item",))
    _click(desktop, actor, "Cancel", roles=("push button", "button"))
    cancelled = desktop.guest("system", "process-read")
    _require(cancelled["selected"] is not None and cancelled["selected"]["start_ticks"] == selected["start_ticks"], "cancelled End Process terminated selected process")
    evidence["cancel_end"] = cancelled
    def probe(current, phase):
        desktop.select(current)
        tree = select_process(current)
        state = desktop.guest("system", "process-read")
        _require(state["selected"] is not None and state["selected"]["nice"] == 19 and state["selected"]["start_ticks"] == selected["start_ticks"], "controlled selected process lost native priority state")
        _require(state["unselected"]["start_ticks"] == negative["start_ticks"] and state["unselected"]["nice"] == 0, "negative process changed")
        return {"gui": tree, "processes": state, "frame": desktop.observe("monitor-" + phase, current)}
    def restart_probe(current, phase):
        _click(desktop, current, "Processes", roles=("page tab", "push button", "button"))
        data = probe(current, phase)
        desktop.keys("shift", "f10")
        _click(desktop, current, "End…", roles=("menu item",))
        _click(desktop, current, "End Process", "End", roles=("push button", "button"))
        final = desktop.await_state("monitor-native-end-process", lambda: desktop.guest("system", "process-read"), lambda state: state["selected"] is None)
        _require(final["unselected"]["start_ticks"] == negative["start_ticks"], "native process termination affected unselected control")
        data["end_process"] = final
        return data
    desktop.crosscut(actor, probe, restart_probe=restart_probe)
    return _record(desktop, actor, "selected-real-process-priority-cancel-end", evidence)


def _logs(desktop):
    fixture = desktop.guest("system", "journal-prepare")
    actor = desktop.start("org.gnome.Logs", "Logs")
    _click(desktop, actor, "All")
    evidence = {"producer": fixture, "ime": _ime_search(desktop, actor, fixture["token"])}
    evidence["search_clipboard"] = _copy_entry(desktop, actor, fixture["token"])
    positive = _await_text(desktop, actor, "logs-positive-producer-payload", fixture["token"] + " producer-payload")
    payload_nodes = [n for n in _visible(positive) if fixture["token"] + " producer-payload" in n.get("name", "")]
    lists = [n for n in _visible(positive) if n.get("role") == "list" and any(
        node["path"][:len(n["path"])] == n["path"] and len(node["path"]) > len(n["path"]) for node in payload_nodes)]
    _require(len(lists) == 1, "cannot identify native producer event list independently of search entry")
    list_path = lists[0]["path"]
    _search(desktop, actor, fixture["absent"])
    def empty_events(tree):
        nodes = _visible(tree)
        _require(any(n["path"] == list_path and n.get("role") == "list" for n in nodes), "native journal event list disappeared")
        return not any(n["path"][:len(list_path)] == list_path and n.get("role") == "list item" for n in nodes)
    evidence["missing_tag"] = desktop.await_state("logs-missing-tag-empty-native-event-list", lambda: desktop.tree(actor), empty_events)
    negative_state = desktop.guest("system", "journal-read")
    _require(negative_state["entries"] == fixture["entries"], "GUI missing-tag search changed actual producer journal")
    _search(desktop, actor, "UNSELECTED_" + fixture["token"])
    evidence["negative_tag_consumer"] = _await_text(desktop, actor, "logs-negative-producer-control", "UNSELECTED_" + fixture["token"])
    _require(fixture["token"] + " producer-payload" not in _content_strings(evidence["negative_tag_consumer"]), "negative producer search included selected positive payload")
    def probe(current, phase):
        desktop.select(current)
        _click(desktop, current, "All")
        _search(desktop, current, fixture["token"] + " producer-payload")
        tree = _await_text(desktop, current, "logs-consumer-" + phase, fixture["token"] + " producer-payload")
        # Inspect the actual producer entry; negative producer must not satisfy it.
        rows = [n for n in _visible(tree) if n.get("name") == fixture["token"] + " producer-payload" and n.get("role") == "label"]
        _require(len(rows) == 1, "producer journal search result is missing or ambiguous")
        desktop.click_path(current, rows[0]["path"])
        messages = [entry for entry in fixture["entries"] if entry.get("MESSAGE") == fixture["token"] + " producer-payload"]
        _require(len(messages) == 1 and messages[0].get("_COMM"), "original journal producer identity is ambiguous")
        detail = _await_text(desktop, current, "logs-inspection-" + phase, messages[0]["_COMM"])
        _require(fixture["token"] + " producer-payload" in _content_strings(detail)
                 and all(label in _content_strings(detail) for label in ("Sender", "Message", "Priority")),
                 "Logs did not inspect the actual selected producer message and native detail fields")
        _require("UNSELECTED_" + fixture["token"] not in _content_strings(detail),
                 "negative producer leaked into selected entry inspection")
        state = desktop.guest("system", "journal-read")
        frame = desktop.observe("logs-" + phase, current)
        desktop.keys("esc")
        return {"journal": state, "detail": detail, "frame": frame}
    desktop.crosscut(actor, probe, restart_probe=probe)
    return _record(desktop, actor, "journal-producer-native-search-entry-inspection", evidence)


def _display_size_matches(text, expected):
    for amount, unit in re.findall(r"([0-9]+(?:[.,][0-9]+)?)\s*(bytes|B|kB|KB|MB|GB|KiB|MiB|GiB)\b", text):
        scales = {"bytes": 1, "B": 1, "kB": 1000, "KB": 1000, "MB": 1000 ** 2, "GB": 1000 ** 3,
                  "KiB": 1024, "MiB": 1024 ** 2, "GiB": 1024 ** 3}
        decimals = len(amount.split(".")[-1]) if "." in amount else (len(amount.split(",")[-1]) if "," in amount else 0)
        quantum = scales[unit] * 10 ** -decimals
        if abs(float(amount.replace(",", ".")) * scales[unit] - expected) <= quantum / 2:
            return True
    return False


def _baobab(desktop):
    fixture = desktop.guest("system", "tree-prepare")
    actor = desktop.start("org.gnome.baobab", "Disk Usage Analyzer")
    evidence = {"fixture": fixture}
    # Baobab's Ctrl+O is its real folder chooser, with a cancelled first grant.
    evidence["folder_consent"] = desktop.open_file(actor, fixture["scanned"]["path"], cancel_first=True)
    def inspected(current, phase):
        tree = desktop.await_state("baobab-scan-" + phase, lambda: desktop.tree(current),
            lambda data: "scanned" in _strings(data) and _display_size_matches(_strings(data), fixture["scanned"]["recursive_allocated_bytes"]), timeout=30)
        _require("unscanned" not in _strings(tree), "Baobab scanned outside selected owned tree")
        _require("nested" in _strings(tree), "Baobab omitted known recursive child")
        state = desktop.guest("system", "tree-read")
        _require(state["scanned"] == fixture["scanned"] and state["unscanned"] == fixture["unscanned"], "scan changed owned payload or unscanned negative bytes")
        return {"tree": tree, "filesystem": state, "frame": desktop.observe("baobab-" + phase, current)}
    evidence["scan"] = inspected(actor, "initial")
    evidence["native_selected_nested_row"] = _select_table_row(desktop, actor, "nested")
    desktop.keys("menu")
    _click(desktop, actor, "Copy Path to Clipboard", roles=("menu item",))
    evidence["native_scan_path_clipboard"] = desktop.clipboard()
    _require(base64.b64decode(evidence["native_scan_path_clipboard"]["base64"]).decode()
             == fixture["scanned"]["path"] + "/nested", "Baobab selected recursive scan row did not export exact real clipboard path")
    # The editable location belongs to the separately owned portal consumer.
    # It is interoperability evidence, NEVER original Baobab frontend IME credit.
    desktop.keys("ctrl", "o")
    picker = desktop.file_picker(actor)
    _require(picker["pid"] != actor["pid"] and picker["id"] != actor["id"], "portal picker is not a separately owned native consumer")
    desktop.keys("ctrl", "l")
    entry = _focused_entry(desktop, picker)
    evidence["portal_ime"] = {"actor": picker, "caller": actor,
                            "composition": desktop.ime(picker, entry.get("name", ""), field_path=entry["path"])}
    _write_entry(desktop, picker, _focused_entry(desktop, picker), fixture["scanned"]["path"])
    evidence["path_clipboard"] = _copy_entry(desktop, picker, fixture["scanned"]["path"])
    _click(desktop, picker, "Close", roles=("push button", "button"))
    evidence["ime_cancel_preserved_scan"] = inspected(actor, "after-ime-cancel")
    original_interface = desktop.tree(actor)
    _require(not any(n.get("editable") and n.get("showing") for n in original_interface["nodes"]),
             "Baobab exposes an original editable field; original-app IME must be exercised")
    evidence["ime_applicability"] = {
        "status": "not-applicable", "original_frontend": actor, "original_interface": original_interface,
        "reason": "Baobab's original scan window exposes the recursive size tree, chart and navigation/scan controls, not an editor/search/entry. Its editable location control is in the separately owned native file-picker PID; that composition is retained only as portal_ime, not original-frontend IME.",
    }
    evidence["directory_default_handler_control"] = desktop.open_default(fixture["unscanned"]["path"])
    desktop.select(actor)
    evidence["handler_preserved_scan"] = inspected(actor, "after-directory-handler")
    def probe(current, phase):
        desktop.select(current)
        desktop.keys("tab")
        focus = desktop.tree(current)
        _require(any(n.get("focused") for n in _visible(focus)), "Baobab native keyboard navigation has no focus")
        data = inspected(current, phase)
        data["keyboard_focus"] = focus
        return data
    def restart_probe(current, phase):
        # Scan chart is intentionally transient. Recent location must persist and
        # reopening it must independently reconstruct the same recursive totals.
        recent = _await_text(desktop, current, "baobab-persisted-recent-location", fixture["scanned"]["path"])
        _require("nested" not in _strings(recent), "Baobab restart unexpectedly reused stale scan chart")
        rows = [n for n in _visible(recent) if n.get("name") == fixture["scanned"]["path"] and n.get("role") == "label"]
        _require(len(rows) == 1, "Baobab persisted recent location is not uniquely actionable")
        desktop.click_path(current, rows[0]["path"])
        return {"persisted_recent": recent, "rescanned": probe(current, phase)}
    desktop.crosscut(actor, probe, restart_probe=restart_probe)
    return _record(desktop, actor, "owned-recursive-tree-native-scan-negative-and-rescan", evidence)


def run(desktop):
    results = []
    try:
        for journey in (_connections, _software, _disks, _monitor, _logs, _baobab):
            results.append(journey(desktop))
        return results
    finally:
        desktop.guest("system", "cleanup")
