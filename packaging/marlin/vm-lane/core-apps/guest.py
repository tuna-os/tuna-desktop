#!/usr/bin/python3
"""Typed, original-session observations for the existing native VM producer."""
import base64
import configparser
import hashlib
import importlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
import traceback
# The lane image also installs standalone copies of the clock observers
# beside native_session.py; this consumer's own admitted modules win.
sys.path[:0] = [str(Path(__file__).resolve().parent),
                str(Path(__file__).resolve().parent.parent / 'tuna-native-session')]
from native_session import NativeSession, digest, process

APP_PACKAGES = {
    'org.gnome.Decibels': 'decibels', 'org.gnome.Calculator': 'gnome-calculator',
    'org.gnome.Calendar': 'gnome-calendar', 'org.gnome.Snapshot': 'snapshot',
    'org.gnome.Characters': 'gnome-characters', 'org.gnome.clocks': 'gnome-clocks',
    'org.gnome.Connections': 'gnome-connections', 'org.gnome.Console': 'gnome-console',
    'org.gnome.Contacts': 'gnome-contacts', 'org.gnome.baobab': 'baobab',
    'org.gnome.DiskUtility': 'gnome-disk-utility', 'org.gnome.SimpleScan': 'simple-scan',
    'org.gnome.Papers': 'papers', 'org.gnome.Nautilus': 'nautilus',
    'org.gnome.font-viewer': 'gnome-font-viewer', 'org.gnome.Yelp': 'yelp',
    'org.gnome.Loupe': 'loupe', 'org.gnome.Logs': 'gnome-logs',
    'org.gnome.Maps': 'gnome-maps', 'org.gnome.Music': 'gnome-music',
    'org.gnome.Settings': 'gnome-control-center', 'org.gnome.Software': 'gnome-software',
    'org.gnome.SystemMonitor': 'gnome-system-monitor', 'org.gnome.TextEditor': 'gnome-text-editor',
    'org.gnome.Tour': 'gnome-tour', 'org.gnome.Showtime': 'showtime',
    'org.gnome.Weather': 'gnome-weather', 'org.gnome.Epiphany': 'epiphany',
}
DOMAINS = {'basic', 'data', 'media', 'system', 'web', 'autotz'}
RUN_STATE = Path('/run/tuna-core-apps.json')
MODULE = Path('/usr/libexec/tuna-core-apps/guest.py')




def desktop(operation, arguments, context):
    if operation == 'prepare':
        if RUN_STATE.exists() or context.fixture.exists():
            raise RuntimeError('core-apps run/fixture already exists; refusing receipt reuse')
        identity = context.identity()
        context.fixture.mkdir(parents=True, mode=0o700)
        for path in (context.root, context.fixture):
            os.chown(path, context.uid, context.owner.pw_gid)
        descriptor = os.open(RUN_STATE, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
        with os.fdopen(descriptor, 'w') as stream:
            json.dump({'nonce': context.nonce, 'identity': identity}, stream)
        package_text = context.command(['pacman', '-Q']).stdout
        versions = dict(line.split(' ', 1) for line in package_text.splitlines())
        apps = {}
        for app_id, package in APP_PACKAGES.items():
            desktop_file = Path('/usr/share/applications') / (app_id + '.desktop')
            metadata = desktop_file.stat()
            if metadata.st_uid != 0 or metadata.st_mode & 0o022:
                raise RuntimeError('installed desktop launch metadata is mutable')
            owner = context.command(['pacman', '-Qoq', str(desktop_file)]).stdout.strip()
            if owner != package or package not in versions:
                raise RuntimeError('required native app desktop/package ownership differs')
            entry = configparser.ConfigParser(interpolation=None)
            entry.optionxform = str
            entry.read(desktop_file, encoding='utf-8')
            desktop_entry = entry['Desktop Entry']
            apps[app_id] = {'app_id': app_id, 'package': package, 'version': versions[package],
                            'native': True, 'flatpak': False, 'desktop_file': str(desktop_file),
                            'desktop_sha256': digest(desktop_file),
                            'desktop_exec': desktop_entry['Exec'],
                            'desktop_name': desktop_entry['Name'],
                            'supported_mime_types': [value for value in desktop_entry.get('MimeType', '').split(';') if value],
                            'declared_file_arguments': bool(re.search(r'(?<!%)%[fF]', desktop_entry['Exec'])),
                            'declared_uri_arguments': bool(re.search(r'(?<!%)%[uU]', desktop_entry['Exec']))}
        control = ('Untouched core-apps negative control ' + context.nonce + '\n').encode()
        content = ('Tuna Core Apps actual UTF-8 content\n' + context.nonce + '\nHello 世界\n').encode()
        for filename, raw in (('principal.txt', content), ('untouched.txt', control)):
            path = context.path(filename)
            path.write_bytes(raw)
            os.chown(path, context.uid, context.owner.pw_gid)
        groups = context.command(['id', '-nG', context.owner.pw_name]).stdout.split()
        installation = json.loads(Path('/usr/share/tuna-core-apps/installation.json').read_text())
        role = installation['account']
        if (context.uid != 1000 or role['uid'] != context.uid or 'wheel' not in groups
                or set(role['groups_after']) != set(groups)
                or set(role['groups_before']) != set(groups)
                or role['added_groups'] != [] or role['removed_groups'] != []
                or role['role_from_lane_fixture_before_boot'] is not True
                or role['policy_authorization_bypassed'] is not False):
            raise RuntimeError('declared lane-fixture preboot UID1000 primary-admin role differs')
        observer_files = [MODULE.parent / (name + '.py') for name in
                          ('guest', 'native', 'basic', 'data', 'media', 'system', 'web', 'autotz', 'geoclue_fixture')]
        observer_files.extend(Path('/usr/libexec') / name
                              for name in ('tuna-core-apps-install', 'tuna-vm-lifecycle'))
        observer_files.append(Path('/usr/libexec/tuna-geoclue-agent-policy'))
        observer_files.append(MODULE.parent.parent / 'tuna-native-session/native_session.py')
        observer_sha256 = {}
        for file in observer_files:
            metadata = file.stat()
            if not stat.S_ISREG(metadata.st_mode) or metadata.st_uid != 0 or metadata.st_mode & 0o022:
                raise RuntimeError('installed Core observer is not original root-owned immutable code')
            observer_sha256[str(file)] = digest(file)
        result = {'identity': identity, 'apps': apps, 'fixture': str(context.fixture),
                  'packages': package_text.splitlines(),
                  'shipped_packages': Path('/usr/share/tuna-vm/shipped-packages.txt').read_text().splitlines(),
                  'installed_signed_release_qualified': False,
                  'installation': installation,
                  'observer_sha256': observer_sha256,
                  'account': {'name': context.owner.pw_name, 'uid': context.uid, 'groups': groups,
                              'scenario': 'ordinary-nonroot-primary-admin',
                              'scope': 'explicit CI fixture role; not GIS, sudo or standard-user qualification'},
                  'principal': {'bytes': len(content), 'sha256': hashlib.sha256(content).hexdigest()},
                  'control': {'bytes': len(control), 'sha256': hashlib.sha256(control).hexdigest()}}
        import native
        result['content_controls'] = native.dispatch('prepare', {}, context)
        context.save_control('inventory', result)
        return result
    if operation == 'state':
        return context.state()
    if operation in ('tree', 'press'):
        return context.tree(arguments['actor'], operation=operation, arguments=arguments)
    if operation == 'discover':
        app_id = arguments['app_id']
        if app_id not in APP_PACKAGES:
            raise ValueError('app is outside exact installed Core inventory')
        value = context.actor({'app_id': app_id}, 'discover')
        binding = 'well-known-bus-name'
        if value.get('ready') is False and arguments.get('window_id') is not None:
            # A mapped real surface whose client never owned the well-known bus
            # name stays undiscoverable by name forever; bind the compositor's
            # kernel-attested live peer for that exact surface instead. The
            # downstream package/code/surface checks below are unchanged, and a
            # socket peer cannot be squatted the way a bus name can.
            state = desktop('state', {}, context)
            surface = [row for row in state['windows']
                       if row['id'] == arguments['window_id'] and row['app_id'] == app_id]
            native = [row for row in state['native_keyboard_windows']
                      if row['window_id'] == arguments['window_id']]
            if (len(surface) == 1 and not state['locked'] and len(native) == 1
                    and native[0]['client_uid'] == context.uid):
                try:
                    peer = process(native[0]['client_pid'], context.uid)
                except FileNotFoundError:
                    return dict(value, surface={'window_id': arguments['window_id'],
                                                'peer': 'departed'})
                value = {'pid': peer['pid'], 'uid': peer['uid'], 'session_bus': None}
                binding = 'compositor-attested-client-pid'
        if value.get('ready') is False:
            return value
        actual = process(value['pid'], context.uid)
        inventory = context.read_control('inventory')['apps'][app_id]
        actor = actual | inventory | {'id': app_id, 'kind': 'app',
                                      'session_bus': value['session_bus'], 'binding': binding}
        state = desktop('state', {}, context)
        windows = [row for row in state['windows'] if row['app_id'] == app_id
                   and (arguments.get('window_id') is None or row['id'] == arguments['window_id'])]
        focused = [row for row in windows if row['id'] == state['focused']]
        if focused:
            windows = focused
        if len(windows) != 1 or state['locked']:
            raise RuntimeError('normal desktop launch did not map selected real app surface')
        native = [row for row in state['native_keyboard_windows'] if row['window_id'] == windows[0]['id']]
        if (len(native) != 1 or native[0]['client_pid'] != actual['pid']
                or native[0]['client_uid'] != context.uid):
            raise RuntimeError('real native surface peer does not match original application bus/PID')
        executable_package = context.command(['pacman', '-Qoq', actual['exe']]).stdout.strip()
        actor['executable_package'] = executable_package
        actor['code_entrypoints'] = []
        if executable_package != inventory['package']:
            if executable_package not in ('python', 'gjs'):
                raise RuntimeError('actual native executable is not declared app or supported installed runtime')
            # Read only for exact installed-script matching; never expose argv.
            actual_arguments = Path(f"/proc/{actual['pid']}/cmdline").read_bytes().split(b'\0')
            files = context.command(['pacman', '-Qlq', inventory['package']]).stdout.splitlines()
            for filename in files:
                file = Path(filename)
                if os.fsencode(filename) in actual_arguments and file.is_file():
                    metadata = file.stat()
                    if metadata.st_uid != 0 or metadata.st_mode & 0o022:
                        raise RuntimeError('actual native application script is mutable')
                    actor['code_entrypoints'].append({'path': filename, 'sha256': digest(file)})
            if not actor['code_entrypoints']:
                raise RuntimeError('native runtime PID is not bound to actual installed app code')
        if process(actual['pid'], context.uid) != actual:
            raise RuntimeError('actual app changed during code/surface admission')
        actor['window_id'] = windows[0]['id']
        actor['initially_focused'] = state['focused'] == windows[0]['id']
        tree = context.actor({'actor': actor, 'initial_discovery': True})
        if tree.get('ready') is False:
            return tree
        actor['a11y_bus'] = tree['a11y_bus']
        frames = [node for node in tree['nodes'] if node['role'] in ('frame', 'dialog')
                  and node['showing'] and node['active']]
        if not frames:
            frames = [node for node in tree['nodes'] if node['role'] in ('frame', 'dialog')
                      and node['showing']]
        if len(frames) != 1:
            raise RuntimeError('actual selected app accessibility frame not unique')
        actor['a11y_scope'] = frames[0]['object_path']
        return actor
    if operation == 'shell-actor':
        return context.shell_actor()
    if operation == 'files':
        result = []
        for relative in arguments['paths']:
            path = context.path(relative)
            if not path.exists():
                result.append({'path': relative, 'exists': False})
            elif path.is_file():
                if path.stat().st_size > 16777216:
                    raise ValueError('ordinary text/file evidence exceeds bound')
                raw = path.read_bytes()
                result.append({'path': relative, 'exists': True, 'bytes': len(raw),
                               'sha256': hashlib.sha256(raw).hexdigest(),
                               'base64': base64.b64encode(raw).decode() if len(raw) <= 4096 else None})
            else:
                result.append({'path': relative, 'exists': True, 'directory': path.is_dir()})
        return {'files': result}
    if operation == 'clipboard':
        mime = arguments['mime']
        if mime not in ('text/plain;charset=utf-8', 'text/plain', 'text/uri-list', 'image/png'):
            raise ValueError('undeclared clipboard evidence MIME')
        offered = context.user(['wl-paste', '--list-types']).stdout.splitlines()
        if mime not in offered:
            raise RuntimeError('requested actual clipboard MIME not offered')
        environment = context.environment()
        raw = subprocess.run(['runuser', '-u', context.owner.pw_name, '--', 'env'] +
                             [f'{k}={v}' for k, v in environment.items()] +
                             ['wl-paste', '--no-newline', '--type', mime], check=True,
                             stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=15).stdout
        if len(raw) > 1048576:
            raise ValueError('clipboard evidence exceeds bound')
        return {'mime': mime, 'offered_types': offered, 'bytes': len(raw),
                'base64': base64.b64encode(raw).decode(), 'sha256': hashlib.sha256(raw).hexdigest()}
    if operation in ('display-scale', 'default-handler', 'camera-permission', 'camera-response',
                     'ime-engine', 'departed', 'picker', 'portal', 'shell-origin',
                     'ime-monitor-start', 'ime-monitor-stop', 'ime-state',
                     'app-input', 'lock-ui', 'retire-frontends', 'finish'):
        import native
        return native.dispatch(operation, arguments, context)
    raise ValueError('unknown typed desktop observation')


def dispatch(domain, operation, arguments, owner, inventory, state):
    if len(json.dumps(arguments)) > 65536:
        raise ValueError('core-apps action exceeds bound')
    arguments = dict(arguments)
    context = NativeSession(owner, inventory, state, arguments.pop('_nonce'), fixture_root='tuna-core-apps')
    if domain == 'desktop':
        return desktop(operation, arguments, context)
    if domain not in DOMAINS:
        raise ValueError('unknown typed core-apps domain')
    context.identity()
    return importlib.import_module(domain).dispatch(operation, arguments, context)


def execute(arguments, owner, inventory, state):
    if len(arguments) != 3:
        raise ValueError('core-apps requires domain, operation and bounded JSON arguments')
    return dispatch(arguments[0], arguments[1], json.loads(arguments[2]), owner, inventory, state)


def error_receipt(error, arguments):
    domain = arguments[0] if arguments and arguments[0] in DOMAINS | {'desktop'} else 'arguments'
    operation = arguments[1] if len(arguments) > 1 and re.fullmatch(r'[a-z][a-z0-9-]{0,63}', arguments[1]) else 'arguments'
    directory = Path(__file__).resolve().parent
    sites = [{'module': Path(frame.filename).name, 'line': frame.lineno, 'function': frame.name}
             for frame in traceback.extract_tb(error.__traceback__)
             if Path(frame.filename).resolve().parent == directory][-4:]
    result = {'domain': domain, 'operation': operation,
              'exception_type': type(error).__name__, 'sites': sites}
    for name in ('errno', 'returncode'):
        value = getattr(error, name, None)
        if type(value) is int and -4095 <= value <= 4095:
            result[name] = value
    return result


if __name__ == '__main__':
    if len(sys.argv) == 4 and sys.argv[1] == '--user-native':
        import native
        result = native.user_operation(sys.argv[2], json.loads(sys.argv[3]))
    elif len(sys.argv) == 4 and sys.argv[1] == '--monitor':
        import native
        native.monitor(sys.argv[2], json.loads(sys.argv[3]))
        raise SystemExit(0)
    else:
        raise ValueError('installed observer accepts only its typed original-user operation')
    print(json.dumps(result))
