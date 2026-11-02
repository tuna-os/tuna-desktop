"""Five native data-app journeys on the admitted desktop's original session.

All mutations below are physical keyboard/pointer or real accessible actions in
the installed application. Guest data operations prepare inputs, inspect bytes,
query EDS read-only, or fault/restore the genuine EDS user service.
"""
from __future__ import annotations

import base64
import hashlib
from pathlib import PurePosixPath
import shlex


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def visible(tree):
    return [node for node in tree["nodes"] if node["showing"]]


def unique(tree, predicate, description):
    matches = [node for node in visible(tree) if predicate(node)]
    require(len(matches) == 1, "actual " + description + " not unique: " + str(len(matches)))
    return matches[0]


def control(desktop, actor, names, roles=None):
    if isinstance(names, str):
        names = (names,)
    roles = roles or ("push button", "button", "toggle button", "menu item", "check menu item",
                      "radio menu item", "radio button", "page tab", "list item", "row", "combo box", "check box", "switch")
    return unique(desktop.tree(actor), lambda node: node["name"] in names and node["sensitive"]
                  and node["role"] in roles, "control " + "/".join(names))


def press(desktop, actor, names, roles=None):
    node = control(desktop, actor, names, roles)
    return desktop.press(actor, node["name"], node["role"])


def field(desktop, actor, name=None, focused=False, document=False):
    roles = ("text", "entry", "search box") if not document else ("text", "entry", "document text")
    return unique(desktop.tree(actor), lambda node: node["role"] in roles and node["sensitive"]
                  and node.get("editable") and node["text"] is not None and (not focused or node["focused"])
                  and (name is None or node["name"] == name or any(a["name"] == name for a in node["ancestors"])),
                  "original editable field " + str(name))


def edit(desktop, actor, value, name=None, focused=False):
    node = field(desktop, actor, name, focused)
    desktop.click_path(actor, node["path"])
    desktop.keys("ctrl", "a")
    if value:
        desktop.type_ascii(value)
    else:
        desktop.keys("backspace")
    return desktop.await_state("data-exact-focused-edit", lambda: desktop.tree(actor),
                               lambda tree: any(n["focused"] and n["text"] == value for n in visible(tree)))


def ime(desktop, actor, node):
    proof = desktop.ime(actor, node["name"], field_path=node["path"])
    original = node["text"]
    require(isinstance(original, str) and original.isascii(),
            "domain IME restoration requires its actual ASCII input control")
    desktop.click_path(actor, node["path"])
    desktop.keys("ctrl", "a")
    if original:
        desktop.type_ascii(original)
    else:
        desktop.keys("backspace")
    desktop.await_state("data-ime-restored-original-field", lambda: desktop.tree(actor),
        lambda row: any(n["path"] == node["path"] and n["focused"] and n["text"] == original
                        for n in visible(row)))
    return proof


def content(desktop, actor, label, *phrases):
    def matches(tree):
        text = "\n".join(str(n[key]) for n in visible(tree) for key in ("name", "text")
                         if n.get(key) is not None and n["role"] not in ("entry", "search box"))
        return all(phrase in text for phrase in phrases)
    return desktop.await_state(label, lambda: desktop.observe(label, actor), matches)


def files(desktop, *paths):
    return desktop.guest("desktop", "files", paths=list(paths))["files"]


def exact(row, raw):
    return row.get("exists") is True and row.get("bytes") == len(raw) and row.get("sha256") == hashlib.sha256(raw).hexdigest() and (
        row.get("base64") is None or base64.b64decode(row["base64"], validate=True) == raw)


def file_wait(desktop, label, expected):
    def matches(rows):
        require(len(rows) == len(expected) and {row["path"] for row in rows} == set(expected),
                "independent file observer omitted or substituted a requested control")
        return all((not row["exists"] if expected[row["path"]] is None else
                    row.get("directory") is True if expected[row["path"]] is True else
                    exact(row, expected[row["path"]])) for row in rows)
    return desktop.await_state(label, lambda: files(desktop, *expected), matches)


def clip(desktop, expected, mime="text/plain;charset=utf-8"):
    result = desktop.clipboard(mime)
    require(base64.b64decode(result["base64"], validate=True) == expected, "real cross-application clipboard bytes differ")
    return result


def _location(desktop, actor, path):
    desktop.select(actor)
    desktop.keys("ctrl", "l")
    edit(desktop, actor, str(path).rstrip("/") + "/", focused=True)
    desktop.keys("ret")


def _cell(desktop, actor, name, selected=False):
    spellings = (name, name + ". File", name + ". Folder")
    tree = desktop.await_state("files-actual-cell", lambda: desktop.tree(actor),
                               lambda row: len([n for n in visible(row) if n["name"] in spellings and
                                                n["role"] in ("table cell", "list item", "icon")]) == 1)
    node = unique(tree, lambda n: n["name"] in spellings and n["role"] in ("table cell", "list item", "icon"), "Files cell " + name)
    if selected:
        desktop.click_path(actor, node["path"])
        desktop.await_state("files-exact-selected-cell", lambda: desktop.tree(actor),
                            lambda row: any(n["name"] in spellings and n["selected"] for n in visible(row)) and
                            not any(n["selected"] and n["role"] in ("table cell", "list item", "icon") and
                                    n["name"] not in spellings for n in visible(row)))
    return node


def _files(desktop, fixture):
    actor = desktop.start("org.gnome.Nautilus", "Files")
    root = fixture["directory"]
    seed, untouched = (base64.b64decode(fixture[key]["base64"]) for key in ("seed", "control"))
    evidence = {"input_controls": fixture}
    _location(desktop, actor, root)
    _cell(desktop, actor, "seed.txt")
    evidence["settings_before"] = desktop.guest("data", "settings", app="files")
    desktop.keys("ctrl", "1")
    evidence["settings_icon_view"] = desktop.guest("data", "settings", app="files")
    desktop.keys("ctrl", "2")
    evidence["list_view"] = desktop.observe("files-ordinary-list-view", actor)
    evidence["settings_list_view"] = desktop.guest("data", "settings", app="files")
    require(evidence["settings_icon_view"]["values"]["default-folder-viewer"] !=
            evidence["settings_list_view"]["values"]["default-folder-viewer"],
            "Files real icon/list controls did not change persisted folder-viewer")
    desktop.keys("ctrl", "shift", "n")
    evidence["original_app_ime"] = ime(desktop, actor, field(desktop, actor, focused=True))
    edit(desktop, actor, "cancelled-" + desktop.nonce, focused=True)
    press(desktop, actor, "Cancel")
    evidence["create_cancel"] = file_wait(desktop, "files-create-cancel-no-side-effect", {
        "data/cancelled-" + desktop.nonce: None, "data/seed.txt": seed, "data/unselected.txt": untouched})
    desktop.keys("ctrl", "shift", "n")
    edit(desktop, actor, "created-through-files", focused=True)
    desktop.keys("ret")
    folder = root + "/created-through-files"
    evidence["create"] = file_wait(desktop, "files-created-directory", {"data/created-through-files": True})
    _cell(desktop, actor, "seed.txt", selected=True)
    desktop.keys("ctrl", "c")
    copied = desktop.clipboard("text/uri-list")
    require(base64.b64decode(copied["base64"]).decode().splitlines() == [PurePosixPath(root + "/seed.txt").as_uri()],
            "Files copied a different selected URI or included the unselected control")
    evidence["selected_clipboard_copy"] = copied
    _location(desktop, actor, folder)
    desktop.keys("ctrl", "v")
    evidence["copy"] = file_wait(desktop, "files-selected-copy-bytes", {
        "data/created-through-files/seed.txt": seed, "data/seed.txt": seed, "data/unselected.txt": untouched})
    _cell(desktop, actor, "seed.txt", selected=True)
    desktop.keys("f2")
    edit(desktop, actor, "cancelled-rename.txt", focused=True)
    desktop.keys("esc")
    evidence["rename_cancel"] = file_wait(desktop, "files-rename-cancel", {
        "data/created-through-files/cancelled-rename.txt": None, "data/created-through-files/seed.txt": seed})
    desktop.keys("f2")
    edit(desktop, actor, "renamed-through-files.txt", focused=True)
    desktop.keys("ret")
    evidence["rename"] = file_wait(desktop, "files-real-rename-bytes", {
        "data/created-through-files/seed.txt": None, "data/created-through-files/renamed-through-files.txt": seed})
    _cell(desktop, actor, "renamed-through-files.txt", selected=True)
    desktop.keys("ctrl", "x")
    cut = desktop.clipboard("text/uri-list")
    require(base64.b64decode(cut["base64"]).decode().splitlines() == [PurePosixPath(folder + "/renamed-through-files.txt").as_uri()],
            "Files cut did not offer only the selected actual URI")
    evidence["selected_clipboard_cut"] = cut
    _location(desktop, actor, root)
    desktop.keys("ctrl", "v")
    moved = "data/renamed-through-files.txt"
    evidence["move"] = file_wait(desktop, "files-real-cut-move", {
        "data/created-through-files/renamed-through-files.txt": None, moved: seed,
        "data/seed.txt": seed, "data/unselected.txt": untouched})
    _cell(desktop, actor, "renamed-through-files.txt", selected=True)
    desktop.keys("delete")
    evidence["trash_removed"] = file_wait(desktop, "files-trash-removed-only-selection", {
        moved: None, "data/seed.txt": seed, "data/unselected.txt": untouched})
    evidence["gio_trash"] = desktop.await_state("files-real-GIO-trash-original-path-and-bytes",
        lambda: desktop.guest("data", "trash", path=moved),
        lambda row: len(row["matches"]) == 1 and row["matches"][0]["original_path"] == root + "/renamed-through-files.txt"
        and base64.b64decode(row["matches"][0]["original_path_base64"]) == (root + "/renamed-through-files.txt").encode()
        and base64.b64decode(row["matches"][0]["base64"]) == seed)
    desktop.keys("ctrl", "z")
    evidence["undo"] = file_wait(desktop, "files-CtrlZ-restored-identical-bytes", {
        moved: seed, "data/seed.txt": seed, "data/unselected.txt": untouched})
    desktop.await_state("files-undo-removed-trash-item", lambda: desktop.guest("data", "trash", path=moved),
                        lambda row: row["matches"] == [])
    current = [actor]
    def probe(live, stage):
        current[0] = live
        _location(desktop, live, root)
        cell = _cell(desktop, live, "renamed-through-files.txt", selected=True)
        settings = desktop.guest("data", "settings", app="files")
        require(settings["values"]["default-folder-viewer"] ==
                evidence["settings_list_view"]["values"]["default-folder-viewer"],
                "Files ordinary selected view did not persist across actual restart")
        return {"stage": stage, "selected_cell": cell,
                "bytes": file_wait(desktop, "files-persist-" + stage, {moved: seed, "data/seed.txt": seed,
                                                                       "data/unselected.txt": untouched}),
                "scene": desktop.observe("files-content-" + stage, live),
                "settings": settings}
    evidence["crosscut"] = desktop.crosscut(actor, probe)
    actor = evidence["crosscut"]["restart_actor"]
    evidence["default_handler"] = desktop.open_default(root + "/renamed-through-files.txt")
    require(evidence["default_handler"]["actor"]["id"] == "org.gnome.TextEditor",
            "real text default-handler did not launch original Text Editor")
    _document(desktop, evidence["default_handler"]["actor"], seed.decode().removesuffix("\n"), "files-real-default-text-consumer")
    return desktop.result(actor, "create-copy-rename-cutmove-trash-undo", evidence, installed_signed_release_qualified=False)


def _document(desktop, actor, expected, label):
    return desktop.await_state(label, lambda: desktop.observe(label, actor),
        lambda row: any(n["role"] in ("text", "document text", "entry") and n.get("text") == expected
                        for n in visible(row)))


def _editor(desktop):
    actor = desktop.start("org.gnome.TextEditor", "Text Editor")
    evidence = {}
    desktop.keys("ctrl", "n")
    evidence["original_editor_ime"] = ime(desktop, actor, field(desktop, actor, document=True))
    original_row = files(desktop, "principal.txt")[0]
    original = base64.b64decode(original_row["base64"], validate=True)
    require(original.endswith(b"\n"), "known source-buffer input requires its actual final LF control")
    original_buffer = original[:-1].decode("utf-8")
    evidence["serialization_contract"] = {
        "source": "https://gitlab.gnome.org/GNOME/gnome-text-editor/-/blob/main/src/editor-document.c",
        "policy": "GtkSourceBuffer implicit-trailing-newline TRUE: loader excludes final LF from buffer; saver appends LF",
        "input_file": original_row, "expected_native_buffer": original_buffer}
    evidence["before_open_cancel"] = _document(desktop, actor, "", "editor-before-open-cancel")
    desktop.keys("ctrl", "o")
    picker = desktop.file_picker(actor)
    press(desktop, picker, "Close")
    desktop.select(actor)
    evidence["open_cancel_unchanged_document"] = _document(desktop, actor, "", "editor-open-cancel-preserves-original-buffer")
    require(files(desktop, "principal.txt")[0] == original_row, "Open Cancel modified the known input file")
    evidence["open"] = desktop.open_file(actor, desktop.fixture + "/principal.txt", cancel_first=False)
    _document(desktop, actor, original_buffer, "editor-native-open-known-UTF8")
    node = unique(desktop.tree(actor), lambda n: n.get("text") == original_buffer and n["role"] in
                  ("text", "document text", "entry"), "original editor document")
    desktop.click_path(actor, node["path"])
    desktop.keys("ctrl", "a")
    desktop.keys("ctrl", "c")
    evidence["copied_known_utf8"] = clip(desktop, original_buffer.encode("utf-8"))
    desktop.keys("ctrl", "n")
    desktop.keys("ctrl", "v")
    _document(desktop, actor, original_buffer, "editor-real-clipboard-UTF8-consumer")
    desktop.keys("ctrl", "end")
    suffix = "\nEdited through original Text Editor " + desktop.nonce
    desktop.type_ascii(suffix)
    expected_buffer = original_buffer + suffix
    expected = (expected_buffer + "\n").encode("utf-8")
    _document(desktop, actor, expected_buffer, "editor-actual-keyboard-edit")
    target = "data/editor-saved.txt"
    desktop.keys("ctrl", "s")
    picker = desktop.file_picker(actor)
    press(desktop, picker, "Close")
    desktop.select(actor)
    evidence["save_cancel_unchanged_document"] = _document(desktop, actor, expected_buffer, "editor-save-cancel-retains-edited-buffer")
    require(not files(desktop, target)[0]["exists"], "Save Cancel created the selected destination")
    evidence["save_cancel_grant"] = desktop.save_file(actor, desktop.fixture + "/" + target, cancel_first=False)
    evidence["saved_bytes"] = file_wait(desktop, "editor-actual-saved-byte-controls", {
        target: expected, "principal.txt": original,
        "untouched.txt": ("Untouched core-apps negative control " + desktop.nonce + "\n").encode()})
    evidence["settings"] = desktop.guest("data", "settings", app="editor")
    press(desktop, actor, "Main Menu")
    press(desktop, actor, "Preferences")
    line_numbers = control(desktop, actor, "Display Line Numbers", roles=("switch", "list item", "row", "check box"))
    desktop.click_path(actor, line_numbers["path"])
    evidence["line_number_setting"] = desktop.guest("data", "settings", app="editor")
    require(evidence["settings"]["values"]["show-line-numbers"] !=
            evidence["line_number_setting"]["values"]["show-line-numbers"],
            "Text Editor's actual preference control did not change its persisted setting")
    desktop.keys("esc")
    current = [actor]
    def probe(live, stage):
        current[0] = live
        if stage == "after-restart":
            desktop.open_file(live, desktop.fixture + "/" + target, cancel_first=False)
        require(desktop.guest("data", "settings", app="editor")["values"]["show-line-numbers"] ==
                evidence["line_number_setting"]["values"]["show-line-numbers"],
                "Text Editor preference did not survive real scale/unlock/restart")
        return {"content": _document(desktop, live, expected_buffer, "editor-content-" + stage),
                "bytes": file_wait(desktop, "editor-persist-" + stage, {target: expected, "principal.txt": original}),
                "settings": desktop.guest("data", "settings", app="editor")}
    evidence["crosscut"] = desktop.crosscut(actor, probe)
    actor = current[0]
    evidence["default_handler"] = desktop.open_default(desktop.fixture + "/" + target)
    require(evidence["default_handler"]["actor"]["id"] == actor["id"], "saved text default opener changed actual consumer")
    _document(desktop, evidence["default_handler"]["actor"], expected_buffer, "editor-default-open-exact-saved-content")
    # Console still needs this original live native editor for genuine
    # cross-application copy/paste. Final result retires its frontend, so defer
    # that result until Console has consumed the saved editor and its clipboard.
    return evidence, actor


def terminal_output(tree, expected):
    wanted = expected.splitlines()
    require(wanted, "exact nonempty terminal output control required")
    for node in visible(tree):
        if node["role"] != "terminal":
            continue
        actual = (node.get("text") or "").replace("\r\n", "\n").splitlines()
        if any(actual[index:index + len(wanted)] == wanted for index in range(len(actual))):
            return True
    return False


def _terminal(desktop, actor, command, expected, label):
    desktop.select(actor)
    terminal = unique(desktop.tree(actor), lambda n: n["role"] == "terminal" and n["sensitive"], "Console VTE terminal")
    desktop.click_path(actor, terminal["path"])
    desktop.type_ascii(command)
    desktop.keys("ret")
    return desktop.await_state(label, lambda: desktop.observe(label, actor),
        lambda row: terminal_output(row, expected))


def _search_field(desktop, actor, console=False):
    desktop.select(actor)
    desktop.keys("ctrl", "shift", "f") if console else desktop.keys("ctrl", "f")
    return field(desktop, actor, focused=True)


def _console(desktop, editor):
    actor = desktop.start("org.gnome.Console", "Console")
    evidence = {}
    process = desktop.guest("data", "console-process", actor=actor)
    require(any(row["stdin"].startswith("/dev/pts/") and PurePosixPath(row["exe"]).name in
                ("bash", "zsh", "fish", "sh") for row in process["children"]),
            "Console is not attached to its original ordinary-user real shell/PTTY")
    evidence["real_shell"] = process
    path = "data/console-result.txt"
    expected = ("Console native output " + desktop.nonce + "\n").encode()
    command = "printf '%s\\n' " + shlex.quote(expected.decode().rstrip("\n")) + " | tee " + shlex.quote(desktop.fixture + "/" + path)
    evidence["typed_command_and_output"] = _terminal(desktop, actor, command, expected.decode().strip(), "console-real-shell-output")
    evidence["file_result"] = file_wait(desktop, "console-shell-created-exact-file", {path: expected})
    failure = "tuna-missing-command-" + desktop.nonce
    evidence["invalid_command"] = _terminal(desktop, actor, failure + "; printf 'EXIT=%s\\n' \"$?\"", "EXIT=127", "console-command-not-found-negative")
    file_wait(desktop, "console-failed-command-no-output-change", {path: expected})
    _terminal(desktop, actor, "printf '\\344\\275\\240\\345\\245\\275\\n'", "你好", "console-real-Unicode-output")
    evidence["original_app_ime"] = ime(desktop, actor, _search_field(desktop, actor, console=True))
    desktop.keys("esc")
    outgoing = "EditorToConsole-" + desktop.nonce
    _search_field(desktop, editor)
    edit(desktop, editor, outgoing, focused=True)
    desktop.keys("ctrl", "a")
    desktop.keys("ctrl", "c")
    evidence["editor_copy"] = clip(desktop, outgoing.encode())
    desktop.keys("esc")
    desktop.select(actor)
    terminal = unique(desktop.tree(actor), lambda n: n["role"] == "terminal", "original Console terminal")
    desktop.click_path(actor, terminal["path"])
    desktop.type_ascii("printf '%s\\n' '")
    desktop.keys("ctrl", "shift", "v")
    desktop.type_ascii("' | tee " + shlex.quote(desktop.fixture + "/data/console-pasted.txt"))
    desktop.keys("ret")
    evidence["editor_to_console_paste"] = desktop.await_state("console-consumed-editor-clipboard",
        lambda: desktop.observe("console-native-pasted-output", actor), lambda row: terminal_output(row, outgoing))
    file_wait(desktop, "console-pasted-exact-file", {"data/console-pasted.txt": (outgoing + "\n").encode()})
    incoming = "ConsoleToEditor-" + desktop.nonce
    evidence["typed_clipboard_command"] = _terminal(desktop, actor,
        "printf '%s' " + shlex.quote(incoming) + " | wl-copy; printf 'CLIPBOARD-READY\\n'", "CLIPBOARD-READY", "console-real-command-clipboard")
    evidence["console_clipboard"] = clip(desktop, incoming.encode())
    _search_field(desktop, editor)
    edit(desktop, editor, "", focused=True)
    desktop.keys("ctrl", "v")
    evidence["console_to_original_editor"] = desktop.await_state("editor-consumed-console-clipboard", lambda: desktop.observe("editor-console-paste", editor),
        lambda row: any(n["focused"] and n["text"] == incoming for n in visible(row)))
    desktop.keys("esc")
    # The real Console shell, not a root helper, invokes the ordinary MIME
    # opener; the actual target's document is inspected under its own actor.
    desktop.select(actor)
    terminal = unique(desktop.tree(actor), lambda n: n["role"] == "terminal", "Console terminal for native file opener")
    desktop.click_path(actor, terminal["path"])
    target_path = desktop.fixture + "/data/editor-saved.txt"
    default = desktop.guest("desktop", "default-handler", path=target_path)
    require(default["default_handler"].removesuffix(".desktop") == "org.gnome.TextEditor",
            "Console real text MIME handler is not original installed Text Editor")
    desktop.type_ascii("xdg-open " + shlex.quote(target_path) + "; printf 'OPEN-STATUS=%s\\n' \"$?\"")
    desktop.keys("ret")
    desktop.await_state("console-ordinary-file-opener-focused-consumer", desktop.state,
                        lambda row: row["focused_app_id"] == "org.gnome.TextEditor")
    opened = desktop.guest("desktop", "discover", app_id="org.gnome.TextEditor")
    require((opened["pid"], opened["start_ticks"], opened["exe_sha256"]) ==
            (editor["pid"], editor["start_ticks"], editor["exe_sha256"]), "Console file opener changed original editor principal")
    saved_text = base64.b64decode(files(desktop, "data/editor-saved.txt")[0]["base64"], validate=True).decode().removesuffix("\n")
    evidence["typed_native_file_default_open"] = {"default_handler": default, "actor": opened,
        "document": _document(desktop, opened, saved_text, "console-actual-default-file-consumer")}
    desktop.select(actor)
    evidence["file_open_exit"] = desktop.await_state("console-real-file-opener-status",
        lambda: desktop.observe("console-native-opener-output", actor), lambda row: terminal_output(row, "OPEN-STATUS=0"))
    evidence["document_picker_applicability"] = {
        "status": "not-applicable",
        "reason": "Console embeds an original VTE terminal; kgx-application.c open handles working-directory arguments, not an Open/Save document chooser. Actual shell file output and xdg-open consumer are exercised above.",
        "interface_source": "https://gitlab.gnome.org/GNOME/console/-/blob/main/src/kgx-application.c",
        "original_live_tree": desktop.tree(actor)}
    desktop.select(actor)
    evidence["settings_before"] = desktop.guest("data", "settings", app="console")
    press(desktop, actor, "Main Menu")
    press(desktop, actor, "Enlarge Text")
    evidence["settings_zoomed"] = desktop.guest("data", "settings", app="console")
    require(evidence["settings_before"] != evidence["settings_zoomed"], "Console ordinary zoom did not change persisted app setting")
    desktop.keys("esc")
    current = [actor]
    def probe(live, stage):
        current[0] = live
        require(desktop.guest("data", "settings", app="console")["values"]["font-scale"] ==
                evidence["settings_zoomed"]["values"]["font-scale"],
                "Console actual zoom preference did not survive original frontend restart")
        marker = "READ-" + stage + "-" + desktop.nonce
        output = _terminal(desktop, live,
            "printf '%s\\n' " + shlex.quote(marker) + "; cat " + shlex.quote(desktop.fixture + "/" + path),
            marker + "\n" + expected.decode().rstrip("\n"), "console-content-" + stage)
        return {"output": output, "bytes": file_wait(desktop, "console-persist-" + stage, {
            path: expected, "data/console-pasted.txt": (outgoing + "\n").encode()}),
            "shell": desktop.guest("data", "console-process", actor=live),
            "settings": desktop.guest("data", "settings", app="console")}
    evidence["crosscut"] = desktop.crosscut(actor, probe)
    actor = current[0]
    desktop.keys("ctrl", "0")
    return desktop.result(actor, "typed-command-output-files-cross-app-clipboard", evidence, installed_signed_release_qualified=False)


def _eds_read(desktop, kind, label, predicate, source_uid=None):
    def accepted(row):
        require(row.get("available") is True and row["service"]["available"] and row["registry"]["available"],
                "unavailable-service: original authorized EDS consumer baseline/recovery missing")
        return predicate(row)
    return desktop.await_state(label, lambda: desktop.guest("data", "eds", kind=kind, source_uid=source_uid), accepted, timeout=30)


def _records(row, title):
    return [item for item in row["records"] if item.get("summary", item.get("full_name")) == title]


def _disk(desktop, kind, title, present=True):
    return desktop.await_state(kind + "-actual-durable-record", lambda: desktop.guest("data", "storage", kind=kind),
                               lambda row: (any(title in record["payload"] for record in row["records"])) == present, timeout=30)


def _calendar_open_event(desktop, actor, title, details=True):
    desktop.select(actor)
    search_fields = [n for n in visible(desktop.tree(actor)) if n.get("editable") and
                     n["role"] in ("text", "entry", "search box") and n["sensitive"]]
    if search_fields:
        require(len(search_fields) == 1, "Calendar main search interface is ambiguous")
        desktop.click_path(actor, search_fields[0]["path"])
    else:
        press(desktop, actor, "Search")
    edit(desktop, actor, title, focused=True)
    tree = desktop.await_state("calendar-search-actual-event", lambda: desktop.tree(actor),
        lambda row: any(n["name"] == title and n["role"] not in ("entry", "text", "search box") for n in visible(row)))
    candidates = [n for n in visible(tree) if n["name"] == title and n["role"] not in ("label", "entry", "text", "search box")]
    if not candidates:
        label = unique(tree, lambda n: n["name"] == title and n["role"] == "label", "event result label")
        candidates = [n for n in visible(tree) if n["role"] in ("list item", "row", "button", "push button")
                      and label["path"][:len(n["path"])] == n["path"]]
        require(candidates, "Calendar result has no genuine selectable event ancestor")
        candidates = [max(candidates, key=lambda n: len(n["path"]))]
    require(len(candidates) == 1, "Calendar real event result not unique")
    # Enter is the source's GtkSearchEntry activation, not a direct open-event
    # D-Bus call or a second click racing the result-list activation policy.
    desktop.keys("ret")
    tree = desktop.await_state("calendar-real-event-preview", lambda: desktop.tree(actor),
        lambda row: any((n["name"] == "Export Event" or n["text"] == title and n["role"] in ("text", "entry"))
                        for n in visible(row)))
    if details:
        if not any(n["text"] == title and n["role"] in ("text", "entry") for n in visible(tree)):
            press(desktop, actor, ("Edit Additional Details", "Edit", "Edit Event"))
        field(desktop, actor, "Title")
    return tree


def _calendar_new(desktop, actor, title, source, cancel=False, use_ime=False):
    press(desktop, actor, "Add Event")
    edit(desktop, actor, title, "Title")
    if use_ime:
        proof = ime(desktop, actor, field(desktop, actor, "Title"))
    else:
        proof = None
    # Actual calendar source selection, never a GSettings/EDS backend write.
    row = control(desktop, actor, "Calendar", roles=("combo box", "list item", "row", "push button", "button"))
    desktop.click_path(actor, row["path"])
    tree = desktop.tree(actor)
    target = unique(tree, lambda n: n["name"] == source["name"] and n["role"] in
                    ("list item", "row", "menu item", "radio button", "push button", "button"), "local calendar source")
    desktop.click_path(actor, target["path"])
    edit(desktop, actor, "Native location " + desktop.nonce, "Location")
    observation = desktop.observe("calendar-real-local-source-editor", actor)
    press(desktop, actor, "Cancel" if cancel else ("Save", "Done"))
    return {"source": source, "editor": observation, "ime": proof, "cancelled": cancel}


def _export(desktop, actor, path, opener, kind, record, name):
    before = files(desktop, path)
    require(not before[0]["exists"], "native export requires absent destination")
    opener()
    picker = desktop.file_picker(actor)
    press(desktop, picker, "Close")
    desktop.select(actor)
    require(files(desktop, path) == before, "native export cancellation wrote output")
    cancelled = desktop.observe(kind + "-actual-export-cancel", actor)
    opener()
    grant = desktop.portal(actor, "FileChooser", response="accept", save=True,
                           path=desktop.fixture + "/" + path)
    def exported(rows):
        row = rows[0]
        if not row["exists"]:
            return False
        require(row.get("base64") is not None, "bounded actual exported principal payload required")
        raw = base64.b64decode(row["base64"], validate=True).decode("utf-8")
        unfolded = raw.replace("\r\n ", "").replace("\n ", "").replace("\r\n", "\n")
        fields = {}
        for line in unfolded.splitlines():
            if ":" in line:
                key, value = line.split(":", 1)
                fields.setdefault(key.split(";", 1)[0], []).append(value)
        if kind == "calendar":
            return fields.get("SUMMARY") == [name] and fields.get("UID") == [record["uid"]] and (
                fields.get("LOCATION") == ["Native location " + desktop.nonce]) and (
                fields.get("DTSTART") == [record["start"]] and fields.get("DTEND") == [record["end"]])
        return fields.get("FN") == [name] and fields.get("EMAIL") == ["native-" + desktop.nonce + "@example.invalid"]
    payload = desktop.await_state(kind + "-actual-exported-principal-data", lambda: files(desktop, path), exported)
    return {"cancelled": cancelled, "no_output": before, "grant": grant, "payload": payload}


def _default_import_cancel(desktop, actor, path, kind, name, source_uid, uid):
    opened = desktop.open_default(desktop.fixture + "/" + path)
    require(opened["actor"]["id"] == actor["id"], "real exported data default-handler differs from original app")
    consumer = opened["actor"]
    consent = content(desktop, consumer, kind + "-real-default-import-consent", name)
    press(desktop, consumer, "Cancel")
    desktop.select(actor)
    unchanged = _eds_read(desktop, kind, kind + "-default-import-cancel-unchanged",
        lambda row: len(_records(row, name)) == 1 and _records(row, name)[0]["uid"] == uid, source_uid)
    return {"default_handler": opened, "consent": consent, "cancelled_unchanged_EDS": unchanged}


def _remote_journey(desktop, actor, kind, local_source):
    """Native app CRUD with independently admitted production Radicale readback."""
    baseline = desktop.guest("data", "remote", kind=kind)
    source = baseline["source"]
    require(source["backend"] == ("caldav" if kind == "calendar" else "carddav"),
            "production DAV account observer selected an unsupported backend")
    source_uid = source["uid"]
    evidence = {"production_DAV_account": baseline}
    if kind == "contacts":
        evidence["source_selection"] = _contacts_setup(desktop, actor, source)
    expected_uid = [None]
    def remote(name, present):
        def matches(row):
            require(row["available"] is True and row["source"]["uid"] == source_uid and
                    row["provider_url"] == baseline["provider_url"],
                    "admitted production DAV provider identity changed")
            require(row["unrelated_sha256"] == baseline["unrelated_sha256"],
                    "native app altered unrelated production DAV account payloads")
            found = []
            for payload in row["records"]:
                unfolded = payload["payload"].replace("\r\n ", "").replace("\n ", "").replace("\r\n", "\n")
                values = {}
                for line in unfolded.splitlines():
                    if ":" in line:
                        key, value = line.split(":", 1)
                        values.setdefault(key.split(";", 1)[0], []).append(value)
                if values.get("SUMMARY" if kind == "calendar" else "FN") != [name]:
                    continue
                if present:
                    require(values.get("UID") and (expected_uid[0] is None or values["UID"] == [expected_uid[0]]),
                            "production DAV record does not match original native EDS UID")
                    require(values.get("LOCATION" if kind == "calendar" else "EMAIL") ==
                            (["Native location " + desktop.nonce] if kind == "calendar" else
                             ["native-" + desktop.nonce + "@example.invalid"]),
                            "production DAV payload differs from actual native editor fields")
                found.append(payload)
            return len(found) == 1 if present else not found
        return desktop.await_state(kind + "-production-DAV-payload", lambda:
            desktop.guest("data", "remote", kind=kind, source_uid=source_uid), matches, timeout=30)
    cancelled = "Cancelled Remote " + kind + " " + desktop.nonce
    title = "Native Remote " + kind + " " + desktop.nonce
    edited = "Edited Remote " + kind + " " + desktop.nonce
    if kind == "calendar":
        evidence["cancel_UI"] = _calendar_new(desktop, actor, cancelled, source, cancel=True)
        evidence["cancel_no_remote_write"] = remote(cancelled, False)
        evidence["create_UI"] = _calendar_new(desktop, actor, title, source)
    else:
        evidence["cancel_UI"] = _contact_new(desktop, actor, cancelled, cancel=True)
        evidence["cancel_no_remote_write"] = remote(cancelled, False)
        evidence["create_UI"] = _contact_new(desktop, actor, title)
    evidence["created_on_production_provider"] = remote(title, True)
    saved = _eds_read(desktop, kind, kind + "-production-DAV-EDS-UID",
                      lambda row: len(_records(row, title)) == 1, source_uid)
    record = _records(saved, title)[0]
    expected_uid[0] = record["uid"]
    evidence["created_exact_EDS_UID_on_provider"] = remote(title, True)
    evidence["wrong_password"] = desktop.guest("data", "remote-wrong-password", kind=kind, source_uid=source_uid)
    require(evidence["wrong_password"]["distinction"] == "authentication-denied" and
            evidence["wrong_password"]["status"] == 401, "real production DAV service did not reject wrong credentials")
    evidence["denial_unchanged_records"] = remote(title, True)
    require(evidence["denial_unchanged_records"]["records"] == evidence["created_exact_EDS_UID_on_provider"]["records"],
            "wrong-password request changed actual persisted production DAV records")
    if kind == "calendar":
        _calendar_open_event(desktop, actor, title)
        edit(desktop, actor, edited, "Title")
        press(desktop, actor, ("Save", "Done"))
    else:
        _contact_select(desktop, actor, title)
        press(desktop, actor, "Edit Contact")
        edit(desktop, actor, edited, "Full name")
        press(desktop, actor, "Done")
    evidence["edited_on_production_provider"] = remote(edited, True)
    evidence["old_title_absent"] = remote(title, False)
    evidence["production_service_restart"] = desktop.guest("data", "dav-restart")
    evidence["production_service_recovered"] = desktop.await_state(kind + "-production-DAV-restarted",
        lambda: desktop.guest("data", "dav-state"), lambda row: row.get("ready"), timeout=30)
    evidence["provider_restart_retained_payload"] = remote(edited, True)
    current = [actor]
    def probe(live, stage):
        current[0] = live
        if kind == "calendar":
            _calendar_open_event(desktop, live, edited)
            ui = desktop.observe(kind + "-production-DAV-consumer-" + stage, live)
            require(field(desktop, live, "Title")["text"] == edited,
                    "native Calendar did not consume actual remote event")
            press(desktop, live, "Cancel")
        else:
            ui = _contact_select(desktop, live, edited)
        eds = _eds_read(desktop, kind, kind + "-production-DAV-same-UID-" + stage,
            lambda row: len(_records(row, edited)) == 1 and
                        _records(row, edited)[0]["uid"] == record["uid"], source_uid)
        return {"actual_consumer": ui, "EDS": eds, "production_provider": remote(edited, True)}
    evidence["crosscut"] = desktop.crosscut(actor, probe)
    actor = current[0]
    if kind == "calendar":
        _calendar_open_event(desktop, actor, edited)
        press(desktop, actor, "Delete Event")
    else:
        _contact_select(desktop, actor, edited)
        press(desktop, actor, "Contact Menu")
        press(desktop, actor, "Delete Contact")
    confirmations = [n for n in visible(desktop.tree(actor)) if n["name"] in
                     ("Delete", "Delete Event", "Delete Contact") and n["role"] in ("button", "push button")]
    if confirmations:
        require(len(confirmations) == 1, "production DAV record delete confirmation is ambiguous")
        desktop.press(actor, confirmations[0]["name"], confirmations[0]["role"])
    evidence["deleted_on_production_provider"] = remote(edited, False)
    actor = desktop.restart(actor, "Calendar" if kind == "calendar" else "Contacts")
    evidence["deletion_persisted"] = {
        "frontend": desktop.observe(kind + "-production-DAV-delete-restart", actor),
        "EDS": _eds_read(desktop, kind, kind + "-production-DAV-deleted-after-restart",
                         lambda row: not _records(row, edited), source_uid),
        "production_provider": remote(edited, False)}
    if kind == "contacts":
        evidence["restore_local_source_selection"] = _contacts_setup(desktop, actor, local_source)
    return actor, evidence


def _calendar(desktop):
    actor = desktop.start("org.gnome.Calendar", "Calendar")
    kind = "calendar"
    baseline = _eds_read(desktop, kind, "calendar-real-EDS-local-account", lambda row: bool(row["local_sources"]))
    source = next((row for row in baseline["local_sources"] if row["uid"] == "system-calendar"), baseline["local_sources"][0])
    source_uid = source["uid"]
    evidence = {"account": baseline, "provider_scope": "genuine local EDS and authenticated packaged production Radicale isolated in this guest"}
    baseline = _eds_read(desktop, kind, "calendar-selected-source-controls", lambda row: True, source_uid)
    press(desktop, actor, "Week")
    evidence["week_setting"] = desktop.guest("data", "settings", app=kind)
    press(desktop, actor, "Month")
    evidence["month_setting"] = desktop.guest("data", "settings", app=kind)
    require(evidence["week_setting"]["values"]["active-view"] != evidence["month_setting"]["values"]["active-view"],
            "Calendar ordinary view controls did not change persisted active-view")
    cancelled = "Cancelled Calendar " + desktop.nonce
    evidence["cancel"] = _calendar_new(desktop, actor, cancelled, source, cancel=True, use_ime=True)
    cancellation = _eds_read(desktop, kind, "calendar-cancel-no-event", lambda row: not _records(row, cancelled), source_uid)
    require(cancellation["unrelated_sha256"] == baseline["unrelated_sha256"], "Calendar Cancel changed unrelated EDS events")
    _disk(desktop, kind, cancelled, present=False)
    title = "Native Calendar " + desktop.nonce
    evidence["create"] = _calendar_new(desktop, actor, title, source)
    saved = _eds_read(desktop, kind, "calendar-actual-EDS-event", lambda row: len(_records(row, title)) == 1, source_uid)
    record = _records(saved, title)[0]
    require(record["location"] == "Native location " + desktop.nonce and record["start"] and record["end"],
            "Calendar independent ICS lacked exact location or scheduled date/time")
    evidence["saved_ICS"] = _disk(desktop, kind, title)
    _calendar_open_event(desktop, actor, title)
    edited = "Edited Calendar " + desktop.nonce
    edit(desktop, actor, edited, "Title")
    press(desktop, actor, ("Save", "Done"))
    evidence["edited"] = _eds_read(desktop, kind, "calendar-edit-same-EDS-UID",
        lambda row: len(_records(row, edited)) == 1 and _records(row, edited)[0]["uid"] == record["uid"] and not _records(row, title), source_uid)
    _disk(desktop, kind, edited)
    evidence["settings"] = desktop.guest("data", "settings", app=kind)
    current = [actor]
    def probe(live, stage):
        current[0] = live
        _calendar_open_event(desktop, live, edited)
        ui = desktop.observe("calendar-saved-event-" + stage, live)
        require(field(desktop, live, "Title")["text"] == edited, "Calendar reopened editor lost persisted event content")
        press(desktop, live, "Cancel")
        data = _eds_read(desktop, kind, "calendar-persist-" + stage,
            lambda row: len(_records(row, edited)) == 1 and _records(row, edited)[0]["uid"] == record["uid"], source_uid)
        require(data["unrelated_sha256"] == baseline["unrelated_sha256"], "Calendar altered unrelated account events")
        settings = desktop.guest("data", "settings", app=kind)
        require(settings["values"]["active-view"] == evidence["month_setting"]["values"]["active-view"],
                "Calendar selected view did not persist across original frontend restart")
        return {"UI": ui, "EDS": data, "ICS": _disk(desktop, kind, edited),
                "settings": settings}
    evidence["crosscut"] = desktop.crosscut(actor, probe)
    actor = current[0]
    def export_event():
        _calendar_open_event(desktop, actor, edited, details=False)
        press(desktop, actor, "Export Event")
    evidence["export"] = _export(desktop, actor, "data/calendar-export.ics", export_event,
                                  kind, record, edited)
    evidence["default_import_cancel"] = _default_import_cancel(desktop, actor, "data/calendar-export.ics",
                                                               kind, edited, source_uid, record["uid"])
    evidence["service_fault"] = desktop.guest("data", "service-down", kind=kind)
    blocked = "Unavailable Calendar " + desktop.nonce
    try:
        # A real attempted app operation under a verified real backend outage.
        press(desktop, actor, "Add Event")
        tree = desktop.tree(actor)
        writable = [n for n in visible(tree) if n["role"] in ("text", "entry") and n["sensitive"]]
        if writable:
            edit(desktop, actor, blocked, "Title")
            buttons = [n for n in visible(desktop.tree(actor)) if n["name"] in ("Save", "Done") and n["role"] in ("button", "push button")]
            require(len(buttons) == 1, "Calendar outage save action not uniquely observable")
            if buttons[0]["sensitive"]:
                desktop.press(actor, buttons[0]["name"], buttons[0]["role"])
            else:
                evidence["outage_disabled_save"] = buttons[0]
                press(desktop, actor, "Cancel")
        evidence["outage_UI"] = desktop.observe("calendar-real-service-unavailable", actor)
        evidence["outage_no_durable_event"] = _disk(desktop, kind, blocked, present=False)
        require(not desktop.guest("data", "service", kind=kind)["available"], "Calendar activated a replacement service during masked outage")
        evidence["outage_independent_query"] = desktop.guest("data", "eds", kind=kind, source_uid=source_uid)
        require(evidence["outage_independent_query"].get("distinction") == "unavailable-service",
                "real ECal read unexpectedly remained reachable during owned-unit outage")
    finally:
        evidence["service_recovered"] = desktop.guest("data", "service-restore", kind=kind)
    recovered = _eds_read(desktop, kind, "calendar-recovered-no-phantom-event",
        lambda row: len(_records(row, edited)) == 1 and not _records(row, blocked), source_uid)
    evidence["recovered"] = recovered
    _calendar_open_event(desktop, actor, edited)
    press(desktop, actor, "Delete Event")
    tree = desktop.tree(actor)
    confirms = [n for n in visible(tree) if n["name"] in ("Delete", "Delete Event") and n["role"] in ("button", "push button")]
    if confirms:
        require(len(confirms) == 1, "Calendar delete confirmation ambiguous")
        desktop.press(actor, confirms[0]["name"], confirms[0]["role"])
    evidence["deleted"] = _eds_read(desktop, kind, "calendar-real-delete-EDS", lambda row: not _records(row, edited), source_uid)
    require(evidence["deleted"]["unrelated_sha256"] == baseline["unrelated_sha256"], "Calendar Delete changed unrelated events")
    evidence["deleted_ICS"] = _disk(desktop, kind, edited, present=False)
    actor = desktop.restart(actor, "Calendar")
    evidence["deleted_after_restart"] = {
        "frontend": desktop.observe("calendar-post-delete-original-code-restart", actor),
        "EDS": _eds_read(desktop, kind, "calendar-delete-persisted-across-restart",
            lambda row: not _records(row, edited) and row["unrelated_sha256"] == baseline["unrelated_sha256"], source_uid),
        "ICS": _disk(desktop, kind, edited, present=False)}
    actor, evidence["production_DAV_provider"] = _remote_journey(desktop, actor, kind, source)
    return desktop.result(actor, "local-and-production-DAV-EDS-event-cancel-save-edit-persist-outage-recovery-delete", evidence, installed_signed_release_qualified=False)


def _contacts_setup(desktop, actor, source):
    tree = desktop.tree(actor)
    if any(n["name"] == "Contacts Setup" and n["role"] in ("frame", "dialog", "window") for n in visible(tree)):
        target = unique(tree, lambda n: n["name"] in (source["name"], "Local Address Book", "On This Computer")
                        and n["role"] in ("list item", "row", "radio button"), "actual local address book setup choice")
        desktop.click_path(actor, target["path"])
        chosen = desktop.observe("contacts-ordinary-local-addressbook-selection", actor)
        press(desktop, actor, "Done")
        desktop.await_state("contacts-setup-destroyed-main-window-mapped", desktop.state,
            lambda row: row["focused_app_id"] == actor["id"] and row["focused"] != actor["window_id"])
        replacement = desktop.await_state("contacts-actual-setup-main-window",
            lambda: desktop.guest("desktop", "discover", app_id=actor["id"]),
            lambda row: row.get("pid") is not None and row.get("window_id") != actor["window_id"])
        for key in ("pid", "start_ticks", "uid", "exe", "exe_sha256", "session_bus", "a11y_bus", "package", "version"):
            require(replacement[key] == actor[key], "Contacts setup changed its original installed principal: " + key)
        chosen["main_window_transition"] = replacement
        actor.update(replacement)
        return chosen
    press(desktop, actor, "Main Menu")
    press(desktop, actor, "Preferences")
    tree = desktop.tree(actor)
    target = unique(tree, lambda n: n["name"] in (source["name"], "Local Address Book", "On This Computer")
                    and n["role"] in ("list item", "row", "radio button"), "existing local addressbook preference")
    desktop.click_path(actor, target["path"])
    chosen = desktop.observe("contacts-ordinary-local-addressbook-preference", actor)
    desktop.keys("esc")
    return chosen


def _contact_new(desktop, actor, name, cancel=False, use_ime=False):
    press(desktop, actor, "Add New Contact")
    edit(desktop, actor, name, "Full name")
    proof = ime(desktop, actor, field(desktop, actor, "Full name")) if use_ime else None
    edit(desktop, actor, "native-" + desktop.nonce + "@example.invalid", "Add email")
    observation = desktop.observe("contacts-actual-new-contact-editor", actor)
    press(desktop, actor, "Cancel" if cancel else ("Add", "Done"))
    return {"editor": observation, "ime": proof, "cancelled": cancel}


def _contact_select(desktop, actor, name):
    search = field(desktop, actor, "Search contacts")
    desktop.click_path(actor, search["path"])
    edit(desktop, actor, name, "Search contacts")
    def result_labels(row):
        return [n for n in visible(row) if n["name"] == name and n["role"] == "label" and any(
            parent["role"] in ("list item", "row") and n["path"][:len(parent["path"])] == parent["path"]
            for parent in visible(row))]
    tree = desktop.await_state("contacts-search-exact-saved-person", lambda: desktop.tree(actor),
                               lambda row: len(result_labels(row)) == 1)
    label = result_labels(tree)[0]
    ancestors = [n for n in visible(tree) if n["role"] in ("list item", "row") and label["path"][:len(n["path"])] == n["path"]]
    require(ancestors, "Contacts saved person has no genuine selectable result row")
    desktop.click_path(actor, max(ancestors, key=lambda n: len(n["path"]))["path"])
    return content(desktop, actor, "contacts-actual-person-sheet", name, "native-" + desktop.nonce + "@example.invalid")


def _contacts(desktop):
    actor = desktop.start("org.gnome.Contacts", "Contacts")
    kind = "contacts"
    baseline = _eds_read(desktop, kind, "contacts-real-EDS-local-account", lambda row: bool(row["local_sources"]))
    source = next((row for row in baseline["local_sources"] if row["uid"] == "system-address-book"), baseline["local_sources"][0])
    source_uid = source["uid"]
    evidence = {"account": baseline, "source_selection": _contacts_setup(desktop, actor, source),
                "provider_scope": "genuine local EDS and authenticated packaged production Radicale isolated in this guest"}
    baseline = _eds_read(desktop, kind, "contacts-selected-source-controls", lambda row: True, source_uid)
    press(desktop, actor, "Main Menu")
    press(desktop, actor, "First Name")
    evidence["first_name_setting"] = desktop.guest("data", "settings", app=kind)
    press(desktop, actor, "Main Menu")
    press(desktop, actor, "Surname")
    evidence["surname_setting"] = desktop.guest("data", "settings", app=kind)
    require(evidence["first_name_setting"]["values"]["sort-on-surname"] !=
            evidence["surname_setting"]["values"]["sort-on-surname"],
            "Contacts ordinary sort menu did not change actual persisted setting")
    cancelled = "Cancelled Contact " + desktop.nonce
    evidence["cancel"] = _contact_new(desktop, actor, cancelled, cancel=True, use_ime=True)
    cancellation = _eds_read(desktop, kind, "contacts-new-cancel-no-vCard", lambda row: not _records(row, cancelled), source_uid)
    require(cancellation["unrelated_sha256"] == baseline["unrelated_sha256"], "Contacts Cancel changed unrelated vCards")
    _disk(desktop, kind, cancelled, present=False)
    name = "Native Contact " + desktop.nonce
    evidence["add"] = _contact_new(desktop, actor, name)
    added = _eds_read(desktop, kind, "contacts-real-added-EDS-vCard", lambda row: len(_records(row, name)) == 1, source_uid)
    record = _records(added, name)[0]
    require("native-" + desktop.nonce + "@example.invalid" in record["payload"], "EDS vCard lacks actual UI-entered email")
    evidence["added_vCard"] = _disk(desktop, kind, name)
    _contact_select(desktop, actor, name)
    press(desktop, actor, "Edit Contact")
    edited = "Edited Contact " + desktop.nonce
    edit(desktop, actor, edited, "Full name")
    press(desktop, actor, "Done")
    evidence["edit"] = _eds_read(desktop, kind, "contacts-edited-same-real-vCard-UID",
        lambda row: len(_records(row, edited)) == 1 and _records(row, edited)[0]["uid"] == record["uid"] and not _records(row, name), source_uid)
    evidence["settings"] = desktop.guest("data", "settings", app=kind)
    current = [actor]
    def probe(live, stage):
        current[0] = live
        ui = _contact_select(desktop, live, edited)
        row = _eds_read(desktop, kind, "contacts-persist-" + stage,
            lambda value: len(_records(value, edited)) == 1 and _records(value, edited)[0]["uid"] == record["uid"], source_uid)
        require(row["unrelated_sha256"] == baseline["unrelated_sha256"], "Contacts changed unrelated independent addressbook records")
        settings = desktop.guest("data", "settings", app=kind)
        require(settings["values"]["sort-on-surname"] == evidence["surname_setting"]["values"]["sort-on-surname"],
                "Contacts actual sort preference did not persist across frontend restart")
        return {"sheet": ui, "vCard": row, "SQLite": _disk(desktop, kind, edited),
                "settings": settings}
    evidence["crosscut"] = desktop.crosscut(actor, probe)
    actor = current[0]
    def export_person():
        _contact_select(desktop, actor, edited)
        press(desktop, actor, "Contact Menu")
        press(desktop, actor, "Export")
    evidence["export"] = _export(desktop, actor, "data/contacts-export.vcf", export_person,
                                  kind, record, edited)
    evidence["default_import_cancel"] = _default_import_cancel(desktop, actor, "data/contacts-export.vcf",
                                                               kind, edited, source_uid, record["uid"])
    # Clear the actual search filter before attempting a new person.
    edit(desktop, actor, "", "Search contacts")
    evidence["service_fault"] = desktop.guest("data", "service-down", kind=kind)
    blocked = "Unavailable Contact " + desktop.nonce
    try:
        press(desktop, actor, "Add New Contact")
        edit(desktop, actor, blocked, "Full name")
        tree = desktop.tree(actor)
        done = unique(tree, lambda n: n["name"] in ("Add", "Done") and n["role"] in ("button", "push button"), "outage contact save")
        if done["sensitive"]:
            desktop.press(actor, done["name"], done["role"])
        else:
            evidence["outage_disabled_save"] = done
            press(desktop, actor, "Cancel")
        evidence["outage_UI"] = desktop.observe("contacts-real-EDS-unavailable", actor)
        evidence["outage_no_vCard"] = _disk(desktop, kind, blocked, present=False)
        require(not desktop.guest("data", "service", kind=kind)["available"], "Contacts restarted an EDS service despite actual runtime mask")
        evidence["outage_independent_query"] = desktop.guest("data", "eds", kind=kind, source_uid=source_uid)
        require(evidence["outage_independent_query"].get("distinction") == "unavailable-service",
                "real EBook read unexpectedly remained reachable during owned-unit outage")
    finally:
        evidence["service_recovered"] = desktop.guest("data", "service-restore", kind=kind)
    evidence["recovered"] = _eds_read(desktop, kind, "contacts-service-recovery-no-phantom-vCard",
        lambda row: len(_records(row, edited)) == 1 and not _records(row, blocked), source_uid)
    _contact_select(desktop, actor, edited)
    press(desktop, actor, "Contact Menu")
    press(desktop, actor, "Delete Contact")
    tree = desktop.tree(actor)
    confirms = [n for n in visible(tree) if n["name"] in ("Delete", "Delete Contact") and n["role"] in ("button", "push button")]
    if confirms:
        require(len(confirms) == 1, "Contacts real Delete confirmation not unique")
        desktop.press(actor, confirms[0]["name"], confirms[0]["role"])
    evidence["deleted"] = _eds_read(desktop, kind, "contacts-real-deleted-vCard", lambda row: not _records(row, edited), source_uid)
    require(evidence["deleted"]["unrelated_sha256"] == baseline["unrelated_sha256"], "Contacts Delete changed unrelated people")
    evidence["deleted_SQLite"] = _disk(desktop, kind, edited, present=False)
    actor = desktop.restart(actor, "Contacts")
    evidence["deleted_after_restart"] = {
        "frontend": desktop.observe("contacts-post-delete-original-code-restart", actor),
        "EDS": _eds_read(desktop, kind, "contacts-delete-persisted-across-restart",
            lambda row: not _records(row, edited) and row["unrelated_sha256"] == baseline["unrelated_sha256"], source_uid),
        "SQLite": _disk(desktop, kind, edited, present=False)}
    actor, evidence["production_DAV_provider"] = _remote_journey(desktop, actor, kind, source)
    return desktop.result(actor, "local-and-production-DAV-addressbook-cancel-add-edit-persist-outage-recovery-delete", evidence, installed_signed_release_qualified=False)


def run(desktop):
    """Return five receipts only after each real principal journey completes."""
    pending = ["org.gnome.Nautilus", "org.gnome.TextEditor", "org.gnome.Console",
               "org.gnome.Calendar", "org.gnome.Contacts"]
    current, result = None, []
    dav_attempted = False
    try:
        fixture = desktop.guest("data", "prepare")
        dav_attempted = True
        desktop.retain("production-DAV-start", desktop.guest("data", "dav-start"))
        desktop.retain("production-DAV-admission", desktop.await_state("production-DAV-listener-admitted",
            lambda: desktop.guest("data", "dav-state"), lambda row: row.get("ready"), timeout=30))
        desktop.retain("production-DAV-source-input-setup", desktop.guest("data", "dav-provision"))
        current = pending.pop(0)
        result.append(_files(desktop, fixture))
        current = pending.pop(0)
        editor_result, editor = _editor(desktop)
        current = pending.pop(0)
        console_result = _console(desktop, editor)
        result.append(desktop.result(editor, "native-open-cancel-edit-clipboard-save-cancel-grant",
                                     editor_result, installed_signed_release_qualified=False))
        result.append(console_result)
        current = pending.pop(0)
        result.append(_calendar(desktop))
        current = pending.pop(0)
        result.append(_contacts(desktop))
        return result
    except Exception as error:
        # Retain failure classification without probing an app after a lock,
        # owner loss, or transport failure. No failed action is replayed.
        desktop.retain("data-required-operation-incomplete", {
            "app_id": current, "phase": "input-controls" if current is None else "native-app-journey",
            "status": "unavailable-service" if "unavailable-service:" in str(error) else "failed",
            "error_type": type(error).__name__, "error": str(error),
            "not_exercised": [{"app_id": app, "status": "not-exercised"} for app in pending],
            "completed_app_ids": [row["app"]["app_id"] for row in result],
            "installed_signed_release_qualified": False})
        raise
    finally:
        if dav_attempted:
            desktop.retain("production-DAV-owned-fixture-retirement", desktop.guest("data", "dav-stop"))
