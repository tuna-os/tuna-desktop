#!/usr/bin/python3
"""Exercise genuine Files operations through ordinary desktop input in CI."""
import hashlib
import json
import os
import re
from pathlib import Path
import subprocess
import tempfile
import time
import uuid

import pyatspi
from gi.repository import Gio, GLib

OUT = Path('/out')
STATE = OUT / 'compositor-state.json'
records = []
stage = 'launch'
identity = None
fixture = None


def run(*arguments):
    return subprocess.check_output(arguments, text=True, timeout=15)


def scene():
    result = json.loads(STATE.read_text())
    if result.get('locked'):
        raise RuntimeError('Files operation attempted while the desktop is locked')
    return result


def nautilus_identity():
    bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)

    def call(method, signature, name):
        return bus.call_sync('org.freedesktop.DBus', '/org/freedesktop/DBus',
                             'org.freedesktop.DBus', method,
                             GLib.Variant('(s)', (name,)), GLib.VariantType.new(signature),
                             Gio.DBusCallFlags.NONE, 5000, None).unpack()[0]

    owner = call('GetNameOwner', '(s)', 'org.gnome.Nautilus')
    pid = call('GetConnectionUnixProcessID', '(u)', owner)
    uid = call('GetConnectionUnixUser', '(u)', owner)
    executable = Path(f'/proc/{pid}/exe').resolve(strict=True)
    installed = Path('/usr/bin/nautilus').resolve(strict=True)
    metadata = installed.stat()
    if uid != os.getuid() or executable != installed or metadata.st_uid != 0 or metadata.st_mode & 0o022:
        raise RuntimeError('Files owner is not the genuine installed Nautilus')
    return {'owner': owner, 'pid': pid, 'uid': uid, 'exe': str(executable)}


def removed_accessible(error):
    """Only Nautilus/AT-SPI's concrete retired-object error is readiness.

    AT-SPI reports a retired object either itself or, once GTK has unexported
    the object path, through GDBus's unknown-interface reply for that path."""
    return (isinstance(error, GLib.Error) and error.domain == 'atspi_error'
            and error.code == 1 and re.fullmatch(
                r'Object does not exist at path (?:“/[^”\n\x00]+”|"/[^"\n\x00]+")'
                r'|No such interface (?:“org\.a11y\.atspi\.[A-Za-z]+”|"org\.a11y\.atspi\.[A-Za-z]+")'
                r' on object at path /[^\s\x00]+',
                error.message) is not None)


class IncompleteTree(Exception):
    """A None child invalidated the entire owned walk; transient readiness only."""


def controls():
    while GLib.MainContext.default().pending():
        GLib.MainContext.default().iteration(False)
    result = []

    def walk(node):
        # A disappearing owner-tree object invalidates this entire walk. The
        # original bounded waiter may retry it, never a captured stale action.
        # DISTINCT from the retired-object (2bc) path: a None child means the
        # tree was transiently incomplete. Discard this entire walk; only the
        # original budget/guard path may re-resolve, never a partial list.
        if node is None:
            raise IncompleteTree('transient incomplete accessibility tree: missing child')
        node.clear_cache()
        if node.getState().contains(pyatspi.STATE_SHOWING):
            result.append(node)
        for index in range(node.childCount):
            child = node.getChildAtIndex(index)
            if child is None:
                raise IncompleteTree('transient incomplete accessibility tree: missing child')
            walk(child)

    desktop = pyatspi.Registry.getDesktop(0)
    for index in range(desktop.childCount):
        app = desktop.getChildAtIndex(index)
        if app is None:
            raise IncompleteTree('transient incomplete accessibility tree: missing child')
        try:
            app_pid = app.get_process_id()
        except GLib.Error as error:
            # Earlier unrelated picker callers may disappear. No other
            # transport or accessibility failure is accepted as absence.
            if not removed_accessible(error):
                raise
            continue
        if identity is not None and app_pid == identity['pid']:
            walk(app)
    return result


def fixture_files():
    """Retain only this owned fixture's bounded file contents as hashes."""
    if fixture is None:
        return []
    result = []
    for path in sorted(fixture.rglob('*')):
        if path.is_symlink():
            raise RuntimeError('Unexpected symlink in owned Files fixture')
        if path.is_file():
            if path.stat().st_size > 2048 or len(result) >= 16:
                raise RuntimeError('Owned Files fixture exceeded evidence bounds')
            raw = path.read_bytes()
            result.append({'path': str(path.relative_to(fixture)), 'bytes': len(raw),
                           'sha256': hashlib.sha256(raw).hexdigest()})
    return result


def snapshot(label):
    before = focused()
    nodes = []
    disappeared = 0
    pending = None
    for _ in range(3):
        try:
            pending = controls()
            break
        except IncompleteTree:
            # Transient incomplete tree: re-validate the original owner/focus
            # before one bounded re-resolution. Never a partial snapshot,
            # never an action retry, never a stale reuse.
            focused()
            continue
    if pending is None:
        raise RuntimeError('Files snapshot observed transient incomplete accessibility tree')
    for node in pending:
        try:
            nodes.append({'name': node.name, 'role': node.getRoleName(),
                          'selected': node.getState().contains(pyatspi.STATE_SELECTED),
                          'focused': node.getState().contains(pyatspi.STATE_FOCUSED),
                          'text': node.queryText().getText(0, -1)
                          if node.getRoleName() in ('text', 'entry') else None})
        except GLib.Error as error:
            # A removed file cell can disappear after the accessibility walk.
            # Keep this specific observation explicit; other errors still fail.
            if not removed_accessible(error):
                raise
            disappeared += 1
    after = focused()
    if before['focused'] != after['focused'] or not any(node['role'] == 'frame' for node in nodes):
        raise RuntimeError('Files snapshot lost its original focused window or live accessible frame')
    (OUT / ('nautilus-' + label + '.json')).write_text(
        json.dumps({'scene': after, 'a11y': nodes, 'fixture_files': fixture_files(),
                    'disappeared_accessible_objects': disappeared}, indent=2))
    subprocess.run(['scrot', str(OUT / ('nautilus-' + label + '.png'))], check=True)


def wait(predicate, description, *, startup=False):
    guard = owned_unlocked if startup else focused
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        guard()
        if time.monotonic() >= deadline:
            break
        try:
            accepted = predicate()
        except IncompleteTree:
            # DISTINCT from the retired-object (2bc) path: a None child made
            # this walk incomplete. Explicit readiness only; the original
            # budget/guard loop re-resolves. Never a partial positive, never
            # an action retry, never a stale reuse, never a broad catch.
            accepted = None
        except GLib.Error as error:
            if not removed_accessible(error):
                raise
            accepted = None
        # Both retry and positive result belong to the original unlocked
        # owner; operation waits also retain strict focus throughout.
        guard()
        if time.monotonic() >= deadline:
            break
        if accepted:
            # Startup may wait for focus, but acceptance still requires it.
            if startup:
                focused()
                if time.monotonic() >= deadline:
                    break
            return accepted
        time.sleep(min(.05, max(0, deadline - time.monotonic())))
    raise RuntimeError('Files did not ' + description)


def owned_unlocked():
    current = scene()  # Retains the strict locked-desktop denial.
    if nautilus_identity() != identity:
        raise RuntimeError('Files original bus owner or installed process changed')
    return current


def focused():
    current = owned_unlocked()
    if current.get('focused_app_id') != 'org.gnome.Nautilus':
        raise RuntimeError('Files lost actual compositor keyboard focus')
    return current


def wait_for_initial_map(name):
    # Only this first admission may observe no app focus yet. Every subsequent
    # operation wait is strict; owner replacement/locking never become retries.
    wait(lambda: scene().get('focused_app_id') == 'org.gnome.Nautilus' and cell(name) is not None,
         'map its native window with the actual fixture contents', startup=True)


def wait_for_browser_close(browser_id):
    """Observe this original window's removal, allowing the resulting focus loss.

    While it remains mapped, retain the original bus/process/focus checks.
    After removal the app may exit; absence proves window closure only.
    Never send another close action or admit a late observation.
    """
    if type(browser_id) is not int or browser_id <= 0:
        raise RuntimeError('Files close requires its original positive window ID')
    deadline = time.monotonic() + 10
    def absent(current):
        return all(window['id'] != browser_id for window in current['windows'])
    while time.monotonic() < deadline:
        current = scene()  # Locking remains a terminal failure, even after close.
        if absent(current):
            after = scene()
            if time.monotonic() >= deadline:
                break
            if not absent(after):
                raise RuntimeError('Files original browser window reappeared during close admission')
            return
        try:
            focused()
        except RuntimeError as error:
            # The same close may complete between the scene read and focus
            # guard. Only this exact focus failure may observe final removal;
            # owner/lock/transport failures remain terminal.
            if str(error) != 'Files lost actual compositor keyboard focus':
                raise
            after = scene()
            if time.monotonic() >= deadline or not absent(after):
                raise
            final = scene()
            if time.monotonic() >= deadline or not absent(final):
                raise
            return
        if time.monotonic() >= deadline:
            break
        time.sleep(min(.05, max(0, deadline - time.monotonic())))
    raise RuntimeError('Files did not close its native browser window')


def key(value):
    focused()
    run('xdotool', 'key', '--clearmodifiers', value)


def type_text(value):
    focused()
    run('xdotool', 'type', '--clearmodifiers', '--delay', '0', '--', value)


def cell(name):
    cells = [node for node in controls() if node.getRoleName() == 'table cell'
             and node.name in (name, name + '. File', name + '. Folder')]
    if len(cells) != 1:
        return None
    return cells[0]


def select(name):
    def target():
        node = cell(name)
        if node is None:
            return None
        return node.queryComponent().getExtents(pyatspi.WINDOW_COORDS)
    bounds = wait(target, 'show the actual file cell ' + name)
    current = focused()
    fx, fy, fw, fh = current['focused_rect']
    if (bounds.width <= 0 or bounds.height <= 0 or bounds.x < 0 or bounds.y < 0
            or bounds.x + bounds.width > fw or bounds.y + bounds.height > fh):
        raise RuntimeError('Files cell is outside its actual focused surface')
    run('xdotool', 'mousemove', '--window', host,
        str(fx + bounds.x + bounds.width // 2), str(fy + bounds.y + bounds.height // 2), 'click', '1')
    def selected():
        node = cell(name)
        return node is not None and node.getState().contains(pyatspi.STATE_SELECTED)
    wait(selected, 'select the actual file ' + name)


def navigate(path):
    if not path.is_absolute() or not path.is_dir():
        raise RuntimeError('Files navigation requires an actual absolute fixture directory')
    key('ctrl+l')
    # Nautilus 51 completes directory names with a separator. Type the
    # canonical directory spelling so the exact focused-entry assertion
    # remains valid whether its asynchronous completion has run or not.
    rename_entry(str(path) + '/')


def rename_entry(value):
    wait(lambda: any(node.getRoleName() in ('text', 'entry') and
                    node.getState().contains(pyatspi.STATE_FOCUSED) for node in controls()),
         'focus an actual editable name control')
    key('ctrl+a')
    type_text(value)
    wait(lambda: any(node.getRoleName() in ('text', 'entry') and
                    node.getState().contains(pyatspi.STATE_FOCUSED) and
                    node.queryText().getText(0, -1) == value for node in controls()),
         'show the exact name in its focused edit control')
    key('Return')


def has_payload(path, payload):
    return path.is_file() and path.read_bytes() == payload


def trash_matches(original, payload):
    trash = Gio.File.new_for_uri('trash:///')
    iterator = trash.enumerate_children('standard::name,trash::orig-path',
                                       Gio.FileQueryInfoFlags.NONE, None)
    result = []
    try:
        while True:
            info = iterator.next_file(None)
            if info is None:
                break
            if info.get_attribute_byte_string('trash::orig-path') == str(original):
                child = trash.get_child(info.get_name())
                ok, contents, unused = child.load_contents(None)
                if not ok or bytes(contents) != payload:
                    raise RuntimeError('Actual Trash item lost the selected file contents')
                result.append(child.get_uri())
    finally:
        iterator.close(None)
    return result


try:
    if os.getuid() != 1000:
        raise RuntimeError('Files proof requires the ordinary session user')
    windows = run('xdotool', 'search', '--onlyvisible', '--name', '^Smithay').split()
    if len(windows) != 1:
        raise RuntimeError('Expected one actual nested desktop host')
    host = windows[0]
    run('xdotool', 'windowfocus', host)
    # Container-local fixture remains available when an assertion fails.
    fixture = Path(tempfile.mkdtemp(prefix='tuna-files-', dir=Path.home()))
    data_home = Path(os.environ['XDG_DATA_HOME'])
    filesystem = {name: {'path': str(path), 'device': path.stat().st_dev}
                  for name, path in [('home', Path.home()), ('fixture', fixture),
                                     ('data_home', data_home), ('evidence', OUT)]}
    (OUT / 'nautilus-filesystem.json').write_text(json.dumps(filesystem, indent=2))
    (OUT / 'nautilus-mountinfo.txt').write_text(Path('/proc/self/mountinfo').read_text())
    if filesystem['fixture']['device'] != filesystem['data_home']['device']:
        raise RuntimeError('Home fixture and XDG home Trash are on different filesystems')
    name = 'seed-' + uuid.uuid4().hex + '.txt'
    seed = fixture / name
    payload = b'Tuna Desktop GNOME Files actual operation roundtrip\n' + name.encode() + b'\n'
    seed.write_bytes(payload)
    run('nautilus', '--new-window', str(fixture))
    identity = nautilus_identity()
    wait_for_initial_map(name)
    browser_id = scene()['focused']
    snapshot('opened')
    stage = 'create-folder'
    key('ctrl+shift+n')
    folder_name = 'created-through-files'
    rename_entry(folder_name)
    folder = fixture / folder_name
    wait(lambda: folder.is_dir(), 'create the folder through its real dialog')
    snapshot('folder-created')
    stage = 'copy'
    select(name)
    key('ctrl+c')
    navigate(folder)
    wait(lambda: cell(name) is None, 'navigate into the empty destination folder')
    key('ctrl+v')
    copied = folder / name
    wait(lambda: has_payload(copied, payload) and has_payload(seed, payload) and cell(name) is not None,
         'copy the selected file through the real clipboard')
    snapshot('copied')
    stage = 'rename'
    select(name)
    key('F2')
    renamed_name = 'renamed-through-files.txt'
    rename_entry(renamed_name)
    renamed = folder / renamed_name
    wait(lambda: not copied.exists() and has_payload(renamed, payload) and cell(renamed_name) is not None,
         'rename the actual selected file')
    snapshot('renamed')
    stage = 'move'
    select(renamed_name)
    key('ctrl+x')
    navigate(fixture)
    wait(lambda: cell(name) is not None, 'navigate back to the original directory')
    key('ctrl+v')
    moved = fixture / renamed_name
    wait(lambda: not renamed.exists() and has_payload(moved, payload) and cell(renamed_name) is not None,
         'move the selected file through the real clipboard')
    snapshot('moved')
    stage = 'trash'
    select(renamed_name)
    key('Delete')
    wait(lambda: not moved.exists() and len(trash_matches(moved, payload)) == 1,
         'move the selected file into actual Trash with its original path and bytes')
    trash_uris = trash_matches(moved, payload)
    if not has_payload(seed, payload):
        raise RuntimeError('Trash affected the unselected original file')
    snapshot('trashed')
    stage = 'undo-trash'
    key('ctrl+z')
    wait(lambda: has_payload(moved, payload) and not trash_matches(moved, payload),
         'restore the same file through its real Undo action')
    snapshot('restored')
    records.append({'uid': os.getuid(), 'app_id': 'org.gnome.Nautilus',
                    'browser_window_id': browser_id, 'process': identity, 'fixture': str(fixture),
                    'file_sha256': hashlib.sha256(payload).hexdigest(),
                    'bytes': len(payload), 'trash_uris': trash_uris,
                    'passed': ['native-content', 'create-folder', 'copy', 'rename',
                               'move', 'trash-original-path-and-bytes', 'undo-trash'],
                    'scope': 'ordinary native app in the reference container; final shipped image remains untested'})
    stage = 'close'
    key('ctrl+w')
    wait_for_browser_close(browser_id)
    (OUT / 'nautilus-operations.json').write_text(json.dumps(records, indent=2))
    print('Nautilus genuine native file operations passed: create/copy/rename/move/trash/undo/close')
except Exception as error:
    (OUT / 'nautilus-operations-failure.json').write_text(json.dumps({'stage': stage, 'error': str(error), 'fixture': str(fixture) if fixture else None}, indent=2))
    try:
        snapshot('failure')
    except Exception as evidence_error:
        (OUT / 'nautilus-failure-evidence-error.txt').write_text(str(evidence_error))
    raise
