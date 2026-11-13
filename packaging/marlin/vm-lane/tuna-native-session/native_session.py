#!/usr/bin/python3
"""Original-session identity, bounded execution and AT-SPI for native consumers."""
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import stat
import subprocess
import sys
import time

MODULE = Path(__file__).resolve()


def digest(path):
    result = hashlib.sha256()
    with Path(path).open('rb') as stream:
        for chunk in iter(lambda: stream.read(65536), b''):
            result.update(chunk)
    return result.hexdigest()


def process(pid, uid):
    if not isinstance(pid, int) or pid <= 1:
        raise ValueError('invalid original process')
    root = Path('/proc') / str(pid)
    before = (root / 'stat').read_text().rsplit(')', 1)[1].split()
    executable = (root / 'exe').resolve(strict=True)
    metadata = executable.stat()
    status = dict(line.split(':', 1) for line in (root / 'status').read_text().splitlines()
                  if ':' in line)
    if [int(value) for value in status['Uid'].split()] != [uid] * 4:
        raise RuntimeError('native process real/effective/saved/filesystem credentials differ')
    if root.stat().st_uid != uid or metadata.st_uid != 0 or metadata.st_mode & 0o022:
        raise RuntimeError('native process or installed executable owner differs')
    value = {'pid': pid, 'uid': uid, 'start_ticks': int(before[19]),
             'exe': str(executable), 'exe_sha256': digest(root / 'exe'),
             'cgroup': (root / 'cgroup').read_text().splitlines()}
    after = (root / 'stat').read_text().rsplit(')', 1)[1].split()
    if before[19] != after[19] or (root / 'exe').resolve(strict=True) != executable:
        raise RuntimeError('process changed during original identity observation')
    return value


class NativeSession:
    def __init__(self, owner, inventory, state, nonce, *, fixture_root):
        if os.geteuid() != 0 or owner.pw_uid == 0 or not re.fullmatch('[0-9a-f]{32}', nonce):
            raise RuntimeError('root observer/nonroot original owner/run nonce required')
        self.owner, self.uid = owner, owner.pw_uid
        self.home = Path(owner.pw_dir).resolve(strict=True)
        self.runtime = Path('/run/user') / str(self.uid)
        if fixture_root not in ('tuna-core-apps', 'tuna-autotz'):
            raise ValueError('fixed original native consumer fixture required')
        self.root, self.nonce = self.home / fixture_root, nonce
        self.run_state = Path('/run') / (fixture_root + '.json')
        self.fixture = self.root / nonce
        self._inventory, self._state = inventory, Path(state)
        private = self.runtime.stat()
        if private.st_uid != self.uid or private.st_mode & 0o077:
            raise RuntimeError('original native observer runtime directory is not owner-private')
        if self._state.resolve() != self.runtime / 'tuna-vm-state.json':
            raise RuntimeError('native diagnostic snapshot is outside the original private fixture path')

    def command(self, argv, timeout=15, check=True, input=None):
        if not isinstance(argv, (tuple, list)) or not argv or not all(isinstance(s, str) for s in argv):
            raise ValueError('fixed argument vector required')
        child = subprocess.Popen(argv, text=True, stdin=subprocess.PIPE if input is not None else subprocess.DEVNULL,
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)
        try:
            stdout, stderr = child.communicate(input=input, timeout=timeout)
        except subprocess.TimeoutExpired:
            try:
                os.killpg(child.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                child.communicate(timeout=1)
            except subprocess.TimeoutExpired:
                try:
                    os.killpg(child.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                child.communicate(timeout=2)
            raise
        result = subprocess.CompletedProcess(argv, child.returncode, stdout, stderr)
        if check:
            result.check_returncode()
        return result

    def environment(self):
        identity = self.identity()
        raw = Path(f"/proc/{identity['shell']['pid']}/environ").read_bytes()
        original = dict(item.split(b'=', 1) for item in raw.split(b'\0') if b'=' in item)
        allowed = ('WAYLAND_DISPLAY', 'DISPLAY', 'XDG_CURRENT_DESKTOP', 'XDG_SESSION_DESKTOP',
                   'XDG_SESSION_TYPE', 'GTK_IM_MODULE', 'QT_IM_MODULE', 'XMODIFIERS',
                   'SANE_CONFIG_DIR', 'AT_SPI_BUS_ADDRESS')
        environment = {key: original[key.encode()].decode() for key in allowed if key.encode() in original}
        display = environment.get('WAYLAND_DISPLAY', '')
        if not display or '/' in display or not (self.runtime / display).is_socket():
            raise RuntimeError('original native Wayland socket unavailable')
        environment.update(HOME=str(self.home), USER=self.owner.pw_name, LOGNAME=self.owner.pw_name,
                           XDG_RUNTIME_DIR=str(self.runtime),
                           DBUS_SESSION_BUS_ADDRESS=f'unix:path={self.runtime}/bus',
                           LC_ALL='C.UTF-8', LANGUAGE='C', GDK_BACKEND='wayland')
        return environment

    def user(self, argv, timeout=15, check=True, input=None):
        environment = self.environment()
        return self.command(['runuser', '-u', self.owner.pw_name, '--', 'env'] +
                            [f'{key}={value}' for key, value in environment.items()] + list(argv),
                            timeout=timeout, check=check, input=input)

    def identity(self):
        observed = self._inventory()
        session = observed['session']
        if (observed['probe_uid'] != 0 or observed['session_uid'] != self.uid
                or session['Service'] != 'gdm-password' or session['Type'] != 'wayland'
                or session['User'] != str(self.uid) or int(session['VTNr']) <= 0):
            raise RuntimeError('original stock-GDM native login not admitted')
        result = {'boot_id': Path('/proc/sys/kernel/random/boot_id').read_text().strip(),
                  'session': session, 'uid': self.uid}
        for role, name in (('compositor', 'tuna-compositor'), ('shell', 'tuna-shell-gtk')):
            found = [row for row in observed['processes'] if row['executable'] == name]
            if len(found) != 1:
                raise RuntimeError(f'exactly one original {role} required')
            result[role] = process(found[0]['pid'], self.uid)
        if result['compositor']['cgroup'] != result['shell']['cgroup']:
            raise RuntimeError('compositor/shell original login scopes differ')
        descriptors = Path(f"/proc/{result['compositor']['pid']}/fd")
        devices = set()
        for descriptor in descriptors.iterdir():
            try:
                target = os.readlink(descriptor)
            except FileNotFoundError:
                continue
            if target.startswith('/dev/dri/card'):
                devices.add(target)
        result['drm'] = sorted(devices)
        if not result['drm']:
            raise RuntimeError('no actual primary DRM device on native compositor')
        payload = dict(line.split(None, 1)[::-1] for line in
                       Path('/usr/share/tuna-vm/shipped-payload.sha256').read_text().splitlines())
        for role in ('compositor', 'shell'):
            row = result[role]
            if payload.get(row['exe']) != row['exe_sha256']:
                raise RuntimeError('original installed Tuna ELF differs from immutable payload')
        if self.run_state.exists():
            original = json.loads(self.run_state.read_text())
            if original['nonce'] != self.nonce or original['identity'] != result:
                raise RuntimeError('original native consumer session principal changed')
        return result

    def state(self):
        self.identity()
        state = json.loads(self._state.read_text())
        for name in ('locked', 'windows', 'focused', 'focused_app_id', 'rendered_frames', 'capture_streams'):
            if name not in state:
                raise RuntimeError('current native observer field missing: ' + name)
        if type(state['locked']) is not bool:
            raise RuntimeError('actual native lock state is malformed')
        return state

    def shell_actor(self):
        actor = self.identity()['shell'] | {'id': 'tuna-shell-gtk', 'kind': 'shell', 'window_id': None}
        actor['a11y_bus'] = self.actor({'actor': actor})['a11y_bus']
        return actor

    def tree(self, actor, *, operation='tree', arguments=None):
        state = self.state()
        if state['locked']:
            raise RuntimeError('ordinary application observation/action attempted under lock')
        if actor.get('kind') == 'app' and not any(row['id'] == actor['window_id'] for row in state['windows']):
            raise RuntimeError('original native app surface departed')
        value = self.actor(arguments or {'actor': actor}, operation)
        return {'actor': actor, 'nodes': value['nodes'], 'state': state}

    def path(self, relative):
        if not isinstance(relative, str) or Path(relative).is_absolute():
            raise ValueError('owned relative fixture path required')
        candidate = self.fixture / relative
        if '..' in Path(relative).parts or candidate.resolve().is_relative_to(self.fixture.resolve()) is False:
            raise ValueError('fixture escape refused')
        current = candidate
        while current != self.fixture.parent:
            if current.is_symlink():
                raise ValueError('fixture symlink traversal refused')
            current = current.parent
        return candidate

    def _control_file(self, name):
        if not isinstance(name, str) or re.fullmatch(r'[a-z][a-z0-9-]{0,63}', name) is None:
            raise ValueError('fixed bounded authoritative control name required')
        root = Path('/run') / (self.root.name + '-control')
        root.mkdir(mode=0o700, exist_ok=True)
        directory = root / self.nonce
        directory.mkdir(mode=0o700, exist_ok=True)
        for candidate in (root, directory):
            metadata = candidate.lstat()
            if not stat.S_ISDIR(metadata.st_mode) or metadata.st_uid != 0 or metadata.st_mode & 0o077:
                raise RuntimeError('authoritative control directory is not root-private')
        return directory / (name + '.json')

    def control_exists(self, name):
        path = self._control_file(name)
        try:
            metadata = path.lstat()
        except FileNotFoundError:
            return False
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_uid != 0 or metadata.st_mode & 0o077:
            raise RuntimeError('authoritative control receipt owner/type differs')
        return True

    def save_control(self, name, value):
        path = self._control_file(name)
        original = json.loads(self.run_state.read_text())
        boot = Path('/proc/sys/kernel/random/boot_id').read_text().strip()
        if original['nonce'] != self.nonce or original['identity']['boot_id'] != boot or original['identity']['uid'] != self.uid:
            raise RuntimeError('authoritative control original run/boot/user differs')
        envelope = {'nonce': self.nonce, 'boot_id': boot, 'uid': self.uid, 'value': value}
        encoded = json.dumps(envelope).encode()
        if len(encoded) > 1048576:
            raise ValueError('authoritative control receipt exceeds bound')
        temporary = path.with_name(path.name + '.' + str(os.getpid()) + '.tmp')
        descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
        try:
            with os.fdopen(descriptor, 'wb') as stream:
                stream.write(encoded)
                stream.flush()
                os.fsync(stream.fileno())
            os.replace(temporary, path)
        finally:
            if temporary.exists():
                temporary.unlink()

    def read_control(self, name):
        path = self._control_file(name)
        descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
        with os.fdopen(descriptor, 'rb') as stream:
            metadata = os.fstat(stream.fileno())
            if (not stat.S_ISREG(metadata.st_mode) or metadata.st_uid != 0
                    or metadata.st_mode & 0o077 or metadata.st_size > 1048576):
                raise RuntimeError('authoritative control receipt admission failed')
            value = json.load(stream)
        boot = Path('/proc/sys/kernel/random/boot_id').read_text().strip()
        if (set(value) != {'nonce', 'boot_id', 'uid', 'value'} or value['nonce'] != self.nonce
                or value['boot_id'] != boot or value['uid'] != self.uid):
            raise RuntimeError('authoritative control receipt belongs to another run/boot/user')
        return value['value']

    def actor(self, arguments, operation='tree'):
        if 'actor' in arguments:
            actual = process(arguments['actor']['pid'], self.uid)
            expected = {key: arguments['actor'][key] for key in actual}
            if actual != expected:
                raise RuntimeError('original application PID/start/ELF changed')
        result = self.user(['/usr/bin/python', str(MODULE), '--user-observe', operation,
                            json.dumps(arguments, separators=(',', ':'))])
        value = json.loads(result.stdout)
        if value.get('error'):
            raise RuntimeError('actual user observer failed: ' + value['error'])
        return value


def user_observe(operation, arguments):
    import gi
    gi.require_version('Atspi', '2.0')
    from gi.repository import Atspi, Gio, GLib
    if os.geteuid() == 0:
        raise RuntimeError('accessibility queries must be original ordinary user')
    session = Gio.bus_get_sync(Gio.BusType.SESSION, None)

    def dbus(connection, method, name, signature):
        return connection.call_sync('org.freedesktop.DBus', '/org/freedesktop/DBus',
                                    'org.freedesktop.DBus', method, GLib.Variant('(s)', (name,)),
                                    GLib.VariantType.new(signature), Gio.DBusCallFlags.NONE,
                                    2000, None).unpack()[0]

    if operation == 'discover':
        app_id = arguments['app_id']
        try:
            owner = dbus(session, 'GetNameOwner', app_id, '(s)')
        except GLib.Error as error:
            if Gio.DBusError.get_remote_error(error) != 'org.freedesktop.DBus.Error.NameHasNoOwner':
                raise
            return {'ready': False, 'reason': 'application-bus-not-owned'}
        return {'pid': dbus(session, 'GetConnectionUnixProcessID', owner, '(u)'),
                'uid': dbus(session, 'GetConnectionUnixUser', owner, '(u)'), 'session_bus': owner}
    address = os.environ.get('AT_SPI_BUS_ADDRESS') or session.call_sync(
        'org.a11y.Bus', '/org/a11y/bus', 'org.a11y.Bus', 'GetAddress', None,
        GLib.VariantType.new('(s)'), Gio.DBusCallFlags.NONE, 2000, None).unpack()[0]
    bus = Gio.DBusConnection.new_for_address_sync(address, Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT
                                                 | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION,
                                                 None, None)
    pid = arguments['actor']['pid']
    roots = []
    desktop = Atspi.get_desktop(0)
    for index in range(desktop.get_child_count()):
        root = desktop.get_child_at_index(index)
        if root is None or root.app is None:
            raise RuntimeError('incomplete accessibility registry')
        name = root.app.bus_name
        try:
            owner_pid = dbus(bus, 'GetConnectionUnixProcessID', name, '(u)')
        except GLib.Error as error:
            if error.matches(Gio.dbus_error_quark(), Gio.DBusError.NAME_HAS_NO_OWNER):
                continue
            raise
        if owner_pid == pid:
            if dbus(bus, 'GetConnectionUnixUser', name, '(u)') != os.geteuid():
                raise RuntimeError('accessibility owner UID differs')
            roots.append((name, root))
    if not roots and arguments.get('initial_discovery'):
        return {'ready': False, 'reason': 'original-application-accessibility-not-registered'}
    if len(roots) != 1:
        raise RuntimeError('original application accessibility connection not unique')
    name, root = roots[0]
    expected_bus = arguments['actor'].get('a11y_bus')
    if expected_bus is not None and expected_bus != name:
        raise RuntimeError('original accessibility bus owner changed')
    nodes, objects = [], {}

    def walk(node, path, ancestors):
        if node is None or node.app is None or len(nodes) >= 8192:
            raise RuntimeError('incomplete or over-bound accessibility tree')
        node.clear_cache()
        states = node.get_state_set()
        role = node.get_role_name()
        row = {'path': list(path), 'object_path': node.path, 'name': node.get_name(), 'role': role,
               'ancestors': ancestors, 'text': None, 'bounds': None}
        for key, state_type in (('showing', Atspi.StateType.SHOWING),
                                ('focused', Atspi.StateType.FOCUSED),
                                ('active', Atspi.StateType.ACTIVE),
                                ('editable', Atspi.StateType.EDITABLE),
                                ('default', Atspi.StateType.IS_DEFAULT),
                                ('selected', Atspi.StateType.SELECTED),
                                ('checked', Atspi.StateType.CHECKED),
                                ('sensitive', Atspi.StateType.SENSITIVE)):
            row[key] = states.contains(state_type)
        if 'password' not in role:
            interface = node.get_text_iface()
            if interface is not None:
                row['text'] = Atspi.Text.get_text(interface, 0, -1)
                if len(row['text']) > 131072:
                    raise RuntimeError('accessible public fixture text exceeded bound')
        component = node.get_component_iface()
        if component is not None and row['showing']:
            rect = component.get_extents(Atspi.CoordType.WINDOW)
            row['bounds'] = [rect.x, rect.y, rect.width, rect.height]
        nodes.append(row)
        objects[tuple(path)] = node
        for index in range(node.get_child_count()):
            walk(node.get_child_at_index(index), path + (index,),
                 ancestors + [{'name': row['name'], 'role': role}])
    walk(root, (), [])
    if dbus(bus, 'GetConnectionUnixProcessID', name, '(u)') != pid:
        raise RuntimeError('accessibility owner changed during complete walk')
    scope = arguments['actor'].get('a11y_scope')
    if scope is not None:
        frames = [node for node in nodes if node['object_path'] == scope and node['showing']]
        if len(frames) != 1:
            raise RuntimeError('original native accessibility frame changed or departed')
        prefix = frames[0]['path']
        nodes = [node for node in nodes if node['path'][:len(prefix)] == prefix]
    if operation == 'press':
        requested = arguments['path']
        row = next((row for row in nodes if row['path'] == requested), None)
        if row is None or not row['showing'] or not row['sensitive'] or row['role'] == 'label':
            raise RuntimeError('requested accessible control not live/showing/sensitive')
        action = objects[tuple(requested)].get_action_iface()
        if action is None or action.get_n_actions() < 1 or not action.do_action(0):
            raise RuntimeError('actual native accessible action rejected; not replayed')
    elif operation != 'tree':
        raise ValueError('unknown original user observer operation')
    return {'a11y_bus': name, 'nodes': nodes}


if __name__ == '__main__':
    if len(sys.argv) != 4 or sys.argv[1] != '--user-observe':
        raise ValueError('typed original-user accessibility observation required')
    print(json.dumps(user_observe(sys.argv[2], json.loads(sys.argv[3]))))
