"""Original-session automatic-timezone observations and owned D-Bus monitor."""
import importlib
import json
import os
from pathlib import Path
import re
import select
import signal
import stat
import subprocess
import sys
import threading
import time
import traceback
import xml.etree.ElementTree as ET

UNIT = 'org.gnome.SettingsDaemon.Datetime.service'
GSD_NAME = 'org.gnome.SettingsDaemon.Datetime'
TIME_NAME = 'org.freedesktop.timedate1'
TIME_PATH = '/org/freedesktop/timedate1'
GEO_NAME = 'org.freedesktop.GeoClue2'
GEO_PATH = '/org/freedesktop/GeoClue2'
MONITOR_SECONDS = 45
SETTINGS = {
    'master': ('org.gnome.system.location', 'enabled'),
    'automatic': ('org.gnome.desktop.datetime', 'automatic-timezone'),
}


def _gio():
    import gi
    from gi.repository import Gio, GLib
    return Gio, GLib


def _call(bus, name, path, interface, method, signature=None, values=(),
          reply=None, timeout=2000, auto_start=False):
    Gio, GLib = _gio()
    return bus.call_sync(
        name, path, interface, method,
        GLib.Variant(signature, values) if signature else None,
        GLib.VariantType.new(reply) if reply else None,
        Gio.DBusCallFlags.NONE if auto_start else Gio.DBusCallFlags.NO_AUTO_START,
        timeout, None).unpack()


def _daemon(bus, method, name, reply):
    return _call(bus, 'org.freedesktop.DBus', '/org/freedesktop/DBus',
                 'org.freedesktop.DBus', method, '(s)', (name,), reply)[0]


def _service(bus, name):
    owner = _daemon(bus, 'GetNameOwner', name, '(s)')
    return {'name': name, 'unique_owner': owner,
            'pid': _daemon(bus, 'GetConnectionUnixProcessID', owner, '(u)'),
            'uid': _daemon(bus, 'GetConnectionUnixUser', owner, '(u)')}


def _directory(nonce, create=False):
    if os.geteuid() != 0 or not isinstance(nonce, str) or not re.fullmatch('[0-9a-f]{32}', nonce):
        raise ValueError('root observer and fixed original run nonce required')
    path = Path('/run') / ('tuna-core-autotz-' + nonce)
    if create:
        path.mkdir(mode=0o700, exist_ok=True)
    metadata = path.lstat()
    if not stat.S_ISDIR(metadata.st_mode) or metadata.st_uid != 0 or stat.S_IMODE(metadata.st_mode) != 0o700:
        raise RuntimeError('owned root-only automatic-timezone directory differs')
    return path


def _write_json(path, value, exclusive=False):
    temporary = path if exclusive else path.with_name(path.name + '.' + str(os.getpid()) + '.tmp')
    if path.exists() and not exclusive:
        metadata = path.lstat()
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_uid != 0 or stat.S_IMODE(metadata.st_mode) != 0o600:
            raise RuntimeError('root-only observation file ownership differs')
    descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_NOFOLLOW | os.O_EXCL, 0o600)
    try:
        with os.fdopen(descriptor, 'w') as stream:
            metadata = os.fstat(stream.fileno())
            if metadata.st_uid != 0 or stat.S_IMODE(metadata.st_mode) != 0o600:
                raise RuntimeError('root-only observation file ownership differs')
            json.dump(value, stream, separators=(',', ':'), allow_nan=False)
            stream.flush()
            os.fsync(stream.fileno())
        if not exclusive:
            os.replace(temporary, path)
    finally:
        if not exclusive and temporary.exists():
            temporary.unlink()


def _read_json(path):
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    with os.fdopen(descriptor) as stream:
        metadata = os.fstat(stream.fileno())
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_uid != 0 or stat.S_IMODE(metadata.st_mode) != 0o600:
            raise RuntimeError('root-only observation file ownership differs')
        return json.load(stream)


def _boot():
    return Path('/proc/sys/kernel/random/boot_id').read_text().strip()


def _load(context):
    directory = _directory(context.nonce)
    state = _read_json(directory / 'state.json')
    if state['nonce'] != context.nonce or state['uid'] != context.uid or state['boot_id'] != _boot():
        raise RuntimeError('saved timezone state belongs to another boot/user/run')
    return directory, state


def _persist(directory, state):
    _write_json(directory / 'state.json', state)


def _command(context, argv, *, user=False, directory=None, state=None, timeout=15):
    try:
        result = (context.user if user else context.command)(argv, timeout=timeout, check=False)
    except subprocess.TimeoutExpired as error:
        def text(value):
            return value.decode(errors='replace') if isinstance(value, bytes) else (value or '')
        receipt = {'argv': list(argv), 'uid': context.uid if user else 0,
                   'returncode': None, 'timed_out': True, 'timeout': timeout,
                   'stdout': text(error.stdout), 'stderr': text(error.stderr), 'epoch': time.time()}
        if state is not None:
            state.setdefault('commands', []).append(receipt)
            _persist(directory, state)
        raise RuntimeError('actual timezone command timed out: ' + json.dumps(receipt)) from error
    receipt = {'argv': list(argv), 'uid': context.uid if user else 0,
               'returncode': result.returncode, 'stdout': result.stdout, 'stderr': result.stderr,
               'epoch': time.time()}
    if state is not None:
        state.setdefault('commands', []).append(receipt)
        _persist(directory, state)
    if result.returncode:
        raise RuntimeError('actual timezone command failed: ' + json.dumps(receipt))
    return result


def _settings(context):
    result = {}
    for key, (schema, name) in SETTINGS.items():
        observed = _command(context, ['gsettings', 'get', schema, name], user=True).stdout.strip()
        if observed not in ('true', 'false'):
            raise RuntimeError('actual GSettings boolean malformed: ' + repr(observed))
        result[key] = observed == 'true'
    return result


def _setting_order(values):
    # Never transiently enable both switches while entering a negative control.
    return ('automatic', 'master') if not values['automatic'] else ('master', 'automatic')


def _configure(context, values, directory, state):
    if set(values) != set(SETTINGS) or any(type(value) is not bool for value in values.values()):
        raise ValueError('exact master and automatic boolean arguments required')
    for key in _setting_order(values):
        schema, name = SETTINGS[key]
        _command(context, ['gsettings', 'set', schema, name, str(values[key]).lower()],
                 user=True, directory=directory, state=state)
    observed = _settings(context)
    if observed != values:
        raise RuntimeError('actual GSettings readback differs: ' + json.dumps(observed))
    return observed


def _user_bus(context, method, name, signature):
    result = _command(context, [
        'busctl', '--user', '--json=short', 'call', 'org.freedesktop.DBus',
        '/org/freedesktop/DBus', 'org.freedesktop.DBus', method, 's', name], user=True)
    observed = json.loads(result.stdout)
    if observed.get('type') != signature or len(observed.get('data', [])) != 1:
        raise RuntimeError('actual original-user D-Bus reply malformed: ' + result.stdout)
    return observed['data'][0]


def _gsd(context):
    from native_session import process
    output = _command(context, ['systemctl', '--user', 'show', UNIT,
                               '--property=Id,LoadState,ActiveState,MainPID,FragmentPath'],
                      user=True).stdout
    properties = dict(line.split('=', 1) for line in output.splitlines() if '=' in line)
    if (properties.get('Id') != UNIT or properties.get('LoadState') != 'loaded'
            or properties.get('ActiveState') != 'active'):
        raise RuntimeError('packaged datetime service not already active: ' + output)
    fragment = Path(properties['FragmentPath'])
    metadata = fragment.stat()
    if not fragment.is_absolute() or metadata.st_uid != 0 or metadata.st_mode & 0o022:
        raise RuntimeError('datetime unit is not root-installed')
    pid = int(properties['MainPID'])
    actual = process(pid, context.uid)
    if Path(actual['exe']).name != 'gsd-datetime':
        raise RuntimeError('datetime service MainPID is not the packaged gsd-datetime executable')
    owner = _user_bus(context, 'GetNameOwner', GSD_NAME, 's')
    if (_user_bus(context, 'GetConnectionUnixProcessID', owner, 'u') != pid
            or _user_bus(context, 'GetConnectionUnixUser', owner, 'u') != context.uid):
        raise RuntimeError('datetime unit MainPID and actual session bus owner differ')
    packages = {}
    for label, path in (('executable', actual['exe']), ('unit', str(fragment))):
        package = _command(context, ['pacman', '-Qqo', path]).stdout.strip()
        if package != 'gnome-settings-daemon':
            raise RuntimeError('datetime ' + label + ' not owned by gnome-settings-daemon: ' + package)
        packages[label] = package
    return actual | {'unit': UNIT, 'active': True, 'package': packages['executable'],
                     'unique_owner': owner, 'fragment': str(fragment),
                     'unit_sha256': _digest(fragment)}


def _digest(path):
    from native_session import digest
    return digest(path)


def _timedate(bus):
    properties = _call(bus, TIME_NAME, TIME_PATH, 'org.freedesktop.DBus.Properties',
                       'GetAll', '(s)', (TIME_NAME,), '(a{sv})', auto_start=True)[0]
    zone = properties.get('Timezone')
    if not isinstance(zone, str) or not zone or '..' in Path(zone).parts or Path(zone).is_absolute():
        raise RuntimeError('actual timedate1 Timezone malformed')
    localtime = Path('/etc/localtime')
    try:
        localtime.lstat()
    except FileNotFoundError:
        # systemd/glibc define an absent localtime as UTC. A dangling symlink
        # is not absence: lstat succeeds and the mandatory digest below fails.
        if zone != 'UTC':
            raise RuntimeError('missing /etc/localtime disagrees with timedate1 Timezone')
        return {'zone': zone, 'epoch': time.time(),
                'localtime': {'present': False, 'resolved': None,
                              'sha256': None, 'matches_zone': False}}
    zonefile = Path('/usr/share/zoneinfo') / zone
    actual_digest = _digest(localtime)
    return {'zone': zone, 'epoch': time.time(),
            'localtime': {'present': True, 'resolved': str(localtime.resolve(strict=True)),
                          'sha256': actual_digest, 'matches_zone': actual_digest == _digest(zonefile)}}


def _geoclue(bus):
    result = {'clients': [], 'errors': [], 'source': 'actual GeoClue service; no clients created'}
    try:
        result['service'] = _service(bus, GEO_NAME)
    except Exception as error:
        result['errors'].append({'operation': 'service-owner', 'error': str(error)})
        return result
    name = result['service']['unique_owner']
    objects = {}
    try:
        objects = _call(bus, name, GEO_PATH, 'org.freedesktop.DBus.ObjectManager',
                        'GetManagedObjects', reply='(a{oa{sa{sv}}})', timeout=1000)[0]
    except Exception as error:
        result['errors'].append({'operation': 'GetManagedObjects', 'error': str(error)})
    paths = set(objects)
    pending = [GEO_PATH]
    visited = set()
    deadline = time.monotonic() + 5
    while pending and len(visited) < 256 and time.monotonic() < deadline:
        path = pending.pop()
        if path in visited:
            continue
        visited.add(path)
        try:
            xml = _call(bus, name, path, 'org.freedesktop.DBus.Introspectable',
                        'Introspect', reply='(s)', timeout=500)[0]
            node = ET.fromstring(xml)
            if node.find("./interface[@name='org.freedesktop.GeoClue2.Client']") is not None:
                paths.add(path)
            for child in node.findall('node'):
                segment = child.get('name', '')
                if not segment or segment.startswith('/') or '..' in segment.split('/'):
                    raise RuntimeError('GeoClue introspection child path malformed')
                pending.append(path + '/' + segment)
        except Exception as error:
            result['errors'].append({'operation': 'Introspect', 'path': path, 'error': str(error)})
    if pending:
        result['errors'].append({'operation': 'Introspect', 'error': 'bounded actual object traversal incomplete'})
    for path in sorted(paths):
        properties = objects.get(path, {}).get('org.freedesktop.GeoClue2.Client')
        if properties is None and '/Client/' not in path:
            continue
        try:
            if properties is None:
                properties = _call(bus, name, path, 'org.freedesktop.DBus.Properties',
                                   'GetAll', '(s)', ('org.freedesktop.GeoClue2.Client',),
                                   '(a{sv})', timeout=1000)[0]
            row = {'path': path, 'desktop_id': properties.get('DesktopId'),
                   'active': properties.get('Active'), 'properties': properties,
                   'requested_accuracy_level': properties.get('RequestedAccuracyLevel'),
                   'location': None}
            result['clients'].append(row)
            location = properties.get('Location')
            if location and location != '/':
                values = _call(bus, name, location, 'org.freedesktop.DBus.Properties',
                               'GetAll', '(s)', ('org.freedesktop.GeoClue2.Location',),
                               '(a{sv})', timeout=1000)[0]
                row['location'] = {'path': location, 'latitude': values.get('Latitude'),
                                   'longitude': values.get('Longitude'), 'accuracy': values.get('Accuracy'),
                                   'properties': values}
        except Exception as error:
            result['errors'].append({'operation': 'client/location GetAll', 'path': path, 'error': str(error)})
    try:
        if _service(bus, GEO_NAME) != result['service']:
            raise RuntimeError('GeoClue service identity changed during observation')
    except Exception as error:
        result['errors'].append({'operation': 'service-owner-recheck', 'error': str(error)})
    return result


def _sources(context):
    packages = {}
    for package in ('gnome-settings-daemon', 'geoclue', 'geocode-glib', 'libgweather',
                    'gnome-control-center', 'gsettings-desktop-schemas', 'glib2', 'systemd', 'tzdata'):
        observed = context.command(['pacman', '-Q', package], check=False)
        packages[package] = {'returncode': observed.returncode,
                             'stdout': observed.stdout, 'stderr': observed.stderr}
    return packages


def _mapped_libraries(context, gsd):
    from native_session import process
    before = process(gsd['pid'], context.uid)
    if before != {key: gsd[key] for key in before}:
        raise RuntimeError('datetime process changed before mapped-library observation')
    components = {'libgeoclue': 'geoclue', 'libgeocode-glib': 'geocode_glib',
                  'libgweather': 'gweather'}
    mappings = {}
    for line in Path('/proc', str(gsd['pid']), 'maps').read_text().splitlines():
        fields = line.split(maxsplit=5)
        if len(fields) != 6 or not fields[5].startswith('/'):
            continue
        filename = re.sub(r'\\([0-7]{3})', lambda match: chr(int(match[1], 8)), fields[5])
        basename = Path(filename).name
        component = next((value for prefix, value in components.items()
                          if re.match(re.escape(prefix) + r'(?:[-.]|$)', basename)), None)
        if component is None:
            continue
        if filename.endswith(' (deleted)'):
            raise RuntimeError('required datetime library mapping is deleted: ' + filename)
        device = [int(part, 16) for part in fields[3].split(':')]
        mapping = {'component': component, 'path': filename,
                   'device': device, 'inode': int(fields[4])}
        if filename in mappings and mappings[filename] != mapping:
            raise RuntimeError('datetime library mapping identity differs across segments')
        mappings[filename] = mapping
    rows = []
    for filename, mapping in sorted(mappings.items()):
        path = Path(filename)
        metadata = path.stat()
        if (not stat.S_ISREG(metadata.st_mode) or metadata.st_uid != 0 or metadata.st_mode & 0o022
                or metadata.st_ino != mapping['inode']
                or [os.major(metadata.st_dev), os.minor(metadata.st_dev)] != mapping['device']):
            raise RuntimeError('mapped datetime library differs from root-installed file: ' + filename)
        with path.open('rb') as stream:
            if stream.read(4) != b'\x7fELF':
                raise RuntimeError('mapped datetime dependency is not ELF: ' + filename)
        if mapping['component'] == 'geocode_glib':
            # Record the real mapped backend's default; never redirect the
            # daemon to an observer-owned reverse-geocode provider.
            import mmap
            with path.open('rb') as stream, mmap.mmap(stream.fileno(), 0, access=mmap.ACCESS_READ) as code:
                if code.find(b'https://nominatim.gnome.org\0') < 0:
                    raise RuntimeError('actual mapped geocode default provider was not identified')
            mapping['default_reverse_backend'] = {
                'base_url': 'https://nominatim.gnome.org',
                'source': 'literal in original mapped packaged GeocodeNominatim ELF',
                'provider_overridden': False,
                'scope': 'backend-provenance-not-a-substitute-for-original-daemon-outcome'}
        ownership = _command(context, ['pacman', '-Qqo', filename])
        package = ownership.stdout.strip()
        if re.fullmatch(r'[A-Za-z0-9@._+-]+', package) is None:
            raise RuntimeError('mapped datetime library package ownership malformed: ' + ownership.stdout)
        identity = _command(context, ['pacman', '-Q', package])
        information = _command(context, ['pacman', '-Qi', package])
        rows.append(mapping | {'sha256': _digest(path), 'package': package,
                               'ownership': {'argv': ['pacman', '-Qqo', filename],
                                             'returncode': ownership.returncode,
                                             'stdout': ownership.stdout, 'stderr': ownership.stderr},
                               'package_identity': {'argv': ['pacman', '-Q', package],
                                                    'returncode': identity.returncode,
                                                    'stdout': identity.stdout, 'stderr': identity.stderr},
                               'package_information': {'argv': ['pacman', '-Qi', package],
                                                       'returncode': information.returncode,
                                                       'stdout': information.stdout, 'stderr': information.stderr}})
        after = path.stat()
        if (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns, after.st_ctime_ns) != (
                metadata.st_dev, metadata.st_ino, metadata.st_size, metadata.st_mtime_ns, metadata.st_ctime_ns):
            raise RuntimeError('mapped datetime dependency changed during package/digest observation')
    if process(gsd['pid'], context.uid) != before:
        raise RuntimeError('original datetime PID/start/executable changed during mapped-library observation')
    missing = sorted(set(components.values()) - {row['component'] for row in rows})
    if missing:
        raise RuntimeError('required actual datetime library mappings unavailable: ' +
                           json.dumps({'missing': missing, 'libraries': rows}))
    return {'process': before, 'libraries': rows}


def _location_agent_registration(context, identity):
    pattern = r'^tuna-shell-gtk: location agent (registered|not registered|interface|object):'
    pid = identity['shell']['pid']
    argv = ['journalctl', '-b', '_PID=' + str(pid), '--grep=' + pattern,
            '--output=json', '--no-pager', '-n20']
    receipt = {'command': {'argv': argv}, 'records': [],
               'interpretation': 'Only the actual successful AddAgent completion record establishes registration.'}
    try:
        observed = context.command(argv, timeout=5, check=False)
    except subprocess.TimeoutExpired as error:
        stderr = error.stderr.decode(errors='replace') if isinstance(error.stderr, bytes) else (error.stderr or '')
        receipt['command'].update(returncode=None, timed_out=True, timeout=5, stderr=stderr)
        receipt['error'] = 'original-shell registration journal query timed out'
        return receipt
    receipt['command'].update(returncode=observed.returncode, stderr=observed.stderr)
    for line in observed.stdout.splitlines():
        try:
            row = json.loads(line)
            message = row['MESSAGE']
            if (not isinstance(message, str) or re.match(pattern, message) is None
                    or row['_PID'] != str(pid)
                    or row['_BOOT_ID'] != identity['boot_id'].replace('-', '')):
                raise ValueError('journal record is not the exact original-shell boot/PID/registration prefix')
            receipt['records'].append({key: row[key] for key in
                                       ('MESSAGE', '__MONOTONIC_TIMESTAMP', '_BOOT_ID', '_PID')})
        except (ValueError, KeyError, TypeError) as error:
            receipt['error'] = 'registration journal evidence malformed: ' + str(error)
            break
    Gio, _ = _gio()
    bus = Gio.bus_get_sync(Gio.BusType.SYSTEM, None)
    names = _call(bus, 'org.freedesktop.DBus', '/org/freedesktop/DBus',
                  'org.freedesktop.DBus', 'ListNames', reply='(as)')[0]
    exports = []
    for name in names:
        if not name.startswith(':'):
            continue
        try:
            owner_pid = _daemon(bus, 'GetConnectionUnixProcessID', name, '(u)')
            owner_uid = _daemon(bus, 'GetConnectionUnixUser', name, '(u)')
        except Exception:
            # Unrelated connections can depart between ListNames and lookup.
            continue
        if (owner_pid, owner_uid) != (pid, identity['uid']):
            continue
        values = _call(bus, name, '/org/freedesktop/GeoClue2/Agent',
                       'org.freedesktop.DBus.Properties', 'GetAll', '(s)',
                       ('org.freedesktop.GeoClue2.Agent',), '(a{sv})')[0]
        accuracy = values.get('MaxAccuracyLevel')
        if type(accuracy) is not int or accuracy not in (0, 1, 4, 5, 6, 8):
            raise RuntimeError('original shell GeoClue agent accuracy property malformed')
        exports.append({'unique_owner': name, 'pid': owner_pid, 'uid': owner_uid,
                        'max_accuracy_level': accuracy})
    if len(exports) != 1:
        raise RuntimeError('original shell GeoClue Agent export not unique/reachable')
    receipt['original_shell_agent_export'] = exports[0]
    receipt['registration_success_inferred_from_error_absence'] = False
    receipt['registered'] = (observed.returncode == 0 and 'error' not in receipt
                             and bool(receipt['records'])
                             and receipt['records'][-1]['MESSAGE']
                             == 'tuna-shell-gtk: location agent registered: tuna-shell')
    return receipt


def _monitor_liveness(monitor, readiness, *, expected_uid=0):
    """Check the original process and the acknowledged, nonrenewable interval."""
    if not isinstance(readiness, dict) or readiness.get('ready') is not True:
        raise RuntimeError('owned monitor readiness is missing or unacknowledged')
    for key in ('pid', 'start_ticks', 'exe', 'exe_device', 'exe_inode',
                'argv', 'nonce', 'boot_id'):
        if readiness.get(key) != monitor.get(key) or monitor.get(key) is None:
            raise RuntimeError('owned monitor readiness identity differs: ' + key)
    if (monitor.get('stopped') or monitor.get('lifetime_seconds') != MONITOR_SECONDS
            or readiness.get('lifetime_seconds') != MONITOR_SECONDS):
        raise RuntimeError('owned monitor is retired or its fixed lifetime differs')
    now = time.monotonic()
    launch = monitor.get('launch_monotonic')
    start = readiness.get('started_monotonic')
    acknowledged = readiness.get('monotonic')
    deadline = readiness.get('deadline_monotonic')
    if (any(type(value) not in (int, float) for value in (launch, start, acknowledged, deadline))
            or not launch <= start <= acknowledged <= now < deadline
            or not deadline <= start + MONITOR_SECONDS - 1
            or not deadline <= launch + MONITOR_SECONDS):
        raise RuntimeError('owned monitor admitted observation deadline is expired or invalid')
    descriptor = os.pidfd_open(monitor['pid'])
    try:
        poller = select.poll()
        poller.register(descriptor, select.POLLIN)
        actual = _proc_identity(monitor['pid'])
        if actual['state'] in ('Z', 'X') or actual['uid'] != expected_uid:
            raise RuntimeError('original owned monitor is not live under its admitted UID')
        for key in ('pid', 'start_ticks', 'exe', 'exe_device', 'exe_inode', 'argv'):
            if actual[key] != monitor[key]:
                raise RuntimeError('original owned monitor process identity differs: ' + key)
        checked = time.monotonic()
        if poller.poll(0) or checked >= deadline:
            raise RuntimeError('original owned monitor exited or expired during liveness observation')
    finally:
        os.close(descriptor)
    return {'process': actual, 'checked_monotonic': checked,
            'started_monotonic': start, 'deadline_monotonic': deadline}


def _require_live_monitor(monitor):
    if monitor is None or monitor.get('ready') is not True or monitor.get('live') is not True:
        summary = {'reason': monitor.get('unevaluable_reason') if isinstance(monitor, dict) else None,
                   'error': monitor.get('error') if isinstance(monitor, dict) else None,
                   'ended': monitor.get('ended') if isinstance(monitor, dict) else None,
                   'traffic_tail': monitor.get('traffic_tail') if isinstance(monitor, dict) else None}
        error = RuntimeError('automatic-timezone observation unevaluable: original monitor is not live: ' +
                             json.dumps(summary) + ' :: ' + json.dumps(monitor))
        cause = monitor.get('unevaluable_sites') if isinstance(monitor, dict) else None
        if cause:
            # The bounded failure record follows the causal chain: the true
            # origin (liveness line) displaces the generic requirement frame.
            error.autotz_cause_sites = cause
        raise error


def _monitor_status(directory, state):
    monitor = state.get('monitor')
    if monitor is None:
        return None
    result = dict(monitor)
    result['ready'] = False
    result['live'] = False
    if (directory / 'ready.json').exists():
        result['readiness'] = _read_json(directory / 'ready.json')
    if (directory / 'monitor-error.json').exists():
        result['error'] = _read_json(directory / 'monitor-error.json')
    if (directory / 'monitor-ended.json').exists():
        result['ended'] = _read_json(directory / 'monitor-ended.json')
    traffic = directory / 'monitor-traffic.jsonl'
    if traffic.exists():
        tail = []
        with open(traffic, 'r', encoding='utf-8', errors='replace') as stream:
            for line in stream.read().splitlines()[-8:]:
                try:
                    tail.append(json.loads(line))
                except ValueError:
                    continue
        result['traffic_tail'] = tail
    try:
        if (monitor.get('nonce') != state['nonce'] or monitor.get('boot_id') != state['boot_id']
                or monitor.get('argv', [])[-2:] != ['--monitor', state['nonce']]):
            raise RuntimeError('original monitor belongs to another boot/run/nonce')
        if 'error' in result:
            raise RuntimeError('owned monitor recorded an error')
        if 'ended' in result:
            raise RuntimeError('owned monitor ended before its observation completed')
        result['liveness'] = _monitor_liveness(monitor, result.get('readiness'))
        result['ready'] = True
        result['live'] = True
    except (OSError, RuntimeError, ValueError, KeyError, TypeError) as error:
        result['unevaluable_reason'] = str(error)
        result['unevaluable_sites'] = [
            {'module': Path(frame.filename).name, 'line': frame.lineno, 'function': frame.name}
            for frame in traceback.extract_tb(error.__traceback__)
            if Path(frame.filename).resolve().parent == Path(__file__).resolve().parent][-2:]
    return result


def _calls(directory, state):
    monitor = state.get('monitor')
    if monitor is None:
        return [], None
    path = directory / 'calls.jsonl'
    calls = []
    if path.exists():
        descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
        with os.fdopen(descriptor) as stream:
            metadata = os.fstat(stream.fileno())
            if metadata.st_uid != 0 or stat.S_IMODE(metadata.st_mode) != 0o600:
                raise RuntimeError('monitor log ownership differs')
            for line in stream:
                if not line.endswith('\n'):
                    break
                record = json.loads(line)
                if record['boot_id'] != state['boot_id'] or record['nonce'] != state['nonce']:
                    raise RuntimeError('monitor log belongs to another boot/run')
                calls.append(record)
    result = _monitor_status(directory, state)
    if result['live']:
        interval = result['liveness']
        for call in calls:
            if (type(call.get('monotonic')) not in (int, float)
                    or not interval['started_monotonic'] <= call['monotonic']
                    <= interval['checked_monotonic'] < interval['deadline_monotonic']):
                raise RuntimeError('SetTimezone observation lies outside the admitted live interval')
    return calls, result


def _merge_geoclue_observation(geoclue, directory, state, monitor, restoration=False):
    if monitor is None:
        return geoclue
    if not restoration:
        _require_live_monitor(monitor)
    ready = monitor.get('readiness', {})
    expected_service = ready.get('geoclue_service')
    if not restoration and expected_service is not None and geoclue.get('service') != expected_service:
        raise RuntimeError('GeoClue service owner/PID/UID changed since monitor readiness')
    events = []
    path = directory / 'geoclue.jsonl'
    if path.exists():
        descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
        with os.fdopen(descriptor) as stream:
            metadata = os.fstat(stream.fileno())
            if metadata.st_uid != 0 or stat.S_IMODE(metadata.st_mode) != 0o600:
                raise RuntimeError('passive GeoClue log ownership differs')
            for line in stream:
                if not line.endswith('\n'):
                    break
                event = json.loads(line)
                if (event['nonce'] != state['nonce'] or event['boot_id'] != state['boot_id']
                        or event['service'] != expected_service
                        or event['gsd_pid'] != state['gsd']['pid']
                        or event['gsd_uid'] != state['gsd']['uid']):
                    raise RuntimeError('passive GeoClue evidence belongs to another service/process/run')
                events.append(event)
    # Passive replies describe what the exact client owner observed in this
    # interval. They are never a root-authorized read of current private state.
    geoclue['historical_observation_events'] = events
    if restoration:
        return geoclue
    monitor = _monitor_status(directory, state)
    _require_live_monitor(monitor)
    liveness = monitor['liveness']
    start, deadline = liveness['started_monotonic'], liveness['deadline_monotonic']
    acknowledged = ready['monotonic']
    for event in events:
        if (type(event.get('monotonic')) not in (int, float)
                or not start <= event['monotonic'] <= liveness['checked_monotonic'] < deadline):
            raise RuntimeError('passive GeoClue event lies outside the admitted live interval')
    geoclue['interval_observation'] = {
        'source': 'passive-original-gsd-system-bus',
        'semantics': 'observed-within-interval-not-current-properties',
        'started_monotonic': start, 'acknowledged_monotonic': acknowledged,
        'deadline_monotonic': deadline,
        'observed_through_monotonic': liveness['checked_monotonic'],
        'events': events, 'clients': events[-1]['clients'] if events else []}
    return geoclue


def _snapshot(context, directory=None, state=None, restoration=False):
    Gio, _ = _gio()
    bus = Gio.bus_get_sync(Gio.BusType.SYSTEM, None)
    actual_gsd = _gsd(context)
    if state is not None and not restoration:
        if actual_gsd != state['gsd']:
            raise RuntimeError('original datetime unit PID/start/executable/bus owner changed')
    calls, monitor = _calls(directory, state) if state is not None else ([], None)
    if state is not None and 'monitor' in state and not restoration:
        _require_live_monitor(monitor)
    shell = context.identity()
    geoclue = _geoclue(bus)
    if state is not None:
        geoclue = _merge_geoclue_observation(geoclue, directory, state, monitor, restoration=restoration)
    result = {'identity': shell, 'settings': _settings(context), 'timedate': _timedate(bus),
              'phase': 'restoration' if restoration else 'observation',
              'gsd': actual_gsd, 'geoclue': geoclue,
              'mapped_libraries': _mapped_libraries(context, actual_gsd),
              'location_agent_registration': _location_agent_registration(context, shell),
              'clock': {'source': 'actual native shell process; rendering observed separately by host AT-SPI',
                        'process': shell['shell'], 'timezone_file': '/etc/localtime'},
              'source_packages': _sources(context), 'timezone_calls': calls, 'monitor': monitor,
              'commands': list(state.get('commands', [])) if state else []}
    if restoration:
        result['historical_timezone_calls'] = result.pop('timezone_calls')
    elif state is not None and 'monitor' in state:
        # Slow package/service observations must not return a stale ready pass.
        result['monitor'] = _monitor_status(directory, state)
        _require_live_monitor(result['monitor'])
    return result


def _proc_identity(pid):
    root = Path('/proc') / str(pid)
    fields = (root / 'stat').read_text().rsplit(')', 1)[1].split()
    executable = None if fields[0] in ('Z', 'X') else (root / 'exe').stat()
    return {'pid': pid, 'start_ticks': int(fields[19]), 'state': fields[0],
            'uid': root.stat().st_uid,
            'exe': os.readlink(root / 'exe') if executable is not None else None,
            'exe_device': executable.st_dev if executable is not None else None,
            'exe_inode': executable.st_ino if executable is not None else None,
            'argv': [word.decode() for word in (root / 'cmdline').read_bytes().split(b'\0') if word]}


def _stop_monitor(directory, state):
    monitor = state.get('monitor')
    if monitor is None or monitor.get('stopped'):
        return
    try:
        descriptor = os.pidfd_open(monitor['pid'])
    except ProcessLookupError:
        monitor['stopped'] = True
        monitor['stop_reason'] = 'already exited'
        _persist(directory, state)
        return
    try:
        poller = select.poll()
        poller.register(descriptor, select.POLLIN)
        try:
            actual = _proc_identity(monitor['pid'])
        except FileNotFoundError:
            if not poller.poll(0):
                raise
            monitor['stopped'] = True
            monitor['stop_reason'] = 'exited while observing owned pidfd'
            _persist(directory, state)
            return
        if actual['start_ticks'] != monitor['start_ticks'] or actual['uid'] != 0:
            raise RuntimeError('refusing to signal reused/foreign monitor PID')
        if actual['state'] != 'Z' and any(
                actual[key] != monitor[key] for key in ('argv', 'exe', 'exe_device', 'exe_inode')):
            raise RuntimeError('refusing to signal monitor with foreign nonce/argv/executable')
        if actual['state'] != 'Z':
            try:
                signal.pidfd_send_signal(descriptor, signal.SIGTERM)
            except ProcessLookupError:
                pass
            if not poller.poll(2000):
                signal.pidfd_send_signal(descriptor, signal.SIGKILL)
                if not poller.poll(2000):
                    raise RuntimeError('owned timezone monitor did not terminate')
        monitor['stopped'] = True
        monitor['stop_reason'] = 'owned PID/start/argv checked; exited'
        _persist(directory, state)
    finally:
        os.close(descriptor)


def _watch_start(context, directory, state):
    if 'monitor' in state:
        raise RuntimeError('repeated timezone monitor start refused')
    if _gsd(context) != state['gsd']:
        raise RuntimeError('original datetime service changed before monitor')
    # The subprocess CLI accepts only the nonce; all paths and lifetime are fixed.
    argv = [sys.executable, str(Path(__file__).resolve(strict=True)), '--monitor', context.nonce]
    output = os.open(directory / 'monitor-output.txt',
                     os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    process = None
    try:
        launch = time.monotonic()
        process = subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=output, stderr=subprocess.STDOUT,
                                   close_fds=True, start_new_session=True)
        actual = _proc_identity(process.pid)
        state['monitor'] = {
            key: actual[key] for key in ('pid', 'start_ticks', 'exe', 'exe_device', 'exe_inode')}
        state['monitor'].update(
            argv=argv, nonce=context.nonce, boot_id=_boot(), launch_monotonic=launch,
            lifetime_seconds=MONITOR_SECONDS, log=str(directory / 'calls.jsonl'),
            ready_path=str(directory / 'ready.json'))
        _persist(directory, state)
        deadline = time.monotonic() + 8
        while time.monotonic() < deadline:
            if (directory / 'monitor-error.json').exists():
                raise RuntimeError('actual BecomeMonitor failed: ' +
                                   json.dumps(_read_json(directory / 'monitor-error.json')))
            if (directory / 'ready.json').exists():
                ready = _read_json(directory / 'ready.json')
                if (ready.get('ready') is not True or ready.get('pid') != process.pid
                        or ready.get('nonce') != context.nonce or ready.get('boot_id') != state['boot_id']
                        or ready.get('start_ticks') != actual['start_ticks']):
                    raise RuntimeError('owned monitor readiness identity differs')
                if process.poll() is not None:
                    raise RuntimeError('owned monitor exited immediately after readiness')
                return _snapshot(context, directory, state)
            if process.poll() is not None:
                raise RuntimeError('monitor exited before acknowledged BecomeMonitor readiness; output: ' +
                                   (directory / 'monitor-output.txt').read_text())
            time.sleep(0.05)
        raise RuntimeError('actual BecomeMonitor readiness deadline expired')
    except Exception:
        if 'monitor' in state:
            _stop_monitor(directory, state)
        elif process is not None:
            process.terminate()
            process.wait(timeout=3)
        raise
    finally:
        os.close(output)


def _filter_action(kind, sender, destination, path, interface, member,
                   destinations, geo_destinations, geo_owner):
    """What the monitor filter does with one incoming message.

    A monitor must never answer: an eavesdropped call that reaches GDBus
    dispatch triggers an automatic UnknownMethod/UnknownObject error
    reply, and the bus kills monitors that send. Queue observation
    traffic for the worker, drop every other call, and pass
    returns/errors/signals so synchronous bus calls still complete.
    """
    relevant = (
        kind == 'call'
        and (destination in geo_destinations
             or (destination in destinations and path == TIME_PATH
                 and interface == TIME_NAME and member == 'SetTimezone'))
    ) or (sender == geo_owner and kind in ('return', 'error', 'signal'))
    if relevant:
        return 'queue'
    if kind == 'call':
        return 'drop'
    return 'pass'


def _monitor(nonce):
    # A kernel alarm also bounds connection setup or a wedged GLib/GIO callback.
    signal.alarm(MONITOR_SECONDS)
    absolute_start = time.monotonic()
    directory = _directory(nonce)
    state = _read_json(directory / 'state.json')
    if state['nonce'] != nonce or state['boot_id'] != _boot():
        raise RuntimeError('monitor saved boot/run differs')
    Gio, GLib = _gio()
    address = Gio.dbus_address_get_for_bus_sync(Gio.BusType.SYSTEM, None)
    credentials = Gio.DBusConnection.new_for_address_sync(
        address, Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT |
        Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION, None, None)
    monitor = Gio.DBusConnection.new_for_address_sync(
        address, Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT |
        Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION, None, None)
    monitor.set_exit_on_close(False)
    service = _service(credentials, TIME_NAME)
    geo_service = _service(credentials, GEO_NAME)
    destinations = {TIME_NAME, service['unique_owner']}
    geo_destinations = {GEO_NAME, geo_service['unique_owner']}
    rules = ["type='method_call',destination='" + destination +
             "',path='" + TIME_PATH + "',interface='" + TIME_NAME + "',member='SetTimezone'"
             for destination in sorted(destinations)]
    for destination in sorted(geo_destinations):
        for interface, member in (
                ('org.freedesktop.GeoClue2.Manager', 'GetClient'),
                ('org.freedesktop.GeoClue2.Manager', 'CreateClient'),
                ('org.freedesktop.GeoClue2.Client', 'Start'),
                ('org.freedesktop.GeoClue2.Client', 'Stop'),
                ('org.freedesktop.DBus.Properties', 'GetAll'),
                ('org.freedesktop.DBus.Properties', 'Get'),
                ('org.freedesktop.DBus.Properties', 'Set')):
            rules.append("type='method_call',destination='" + destination +
                         "',path_namespace='" + GEO_PATH + "',interface='" + interface +
                         "',member='" + member + "'")
    for kind in ('method_return', 'error'):
        rules.append("type='" + kind + "',sender='" + geo_service['unique_owner'] + "'")
    rules.append("type='signal',sender='" + geo_service['unique_owner'] +
                 "',interface='org.freedesktop.GeoClue2.Client',member='LocationUpdated'")
    descriptor = os.open(directory / 'calls.jsonl',
                         os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    geo_descriptor = os.open(directory / 'geoclue.jsonl',
                             os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    traffic_descriptor = os.open(directory / 'monitor-traffic.jsonl',
                                 os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    lock = threading.Lock()
    traffic_count = 0
    loop = GLib.MainLoop()
    queued = 0
    event_count = 0
    pending = {}
    clients = {}
    locations = {}
    location_clients = {}
    owners = {}

    def append(fd, record):
        raw = (json.dumps(record, separators=(',', ':'), allow_nan=False) + '\n').encode()
        if len(raw) > 65536 or os.write(fd, raw) != len(raw):
            raise RuntimeError('bounded owned monitor log write failed')
        os.fsync(fd)

    def owner(name):
        if name not in owners:
            owners[name] = (
                _daemon(credentials, 'GetConnectionUnixProcessID', name, '(u)'),
                _daemon(credentials, 'GetConnectionUnixUser', name, '(u)'))
        return owners[name]

    def original(name):
        try:
            return owner(name) == (state['gsd']['pid'], state['gsd']['uid'])
        except GLib.Error:
            # An unrelated short-lived application may leave before credential lookup.
            # Never attribute such a sender or consume its payload.
            return False

    def stamp(epoch, monotonic):
        return {'nonce': nonce, 'boot_id': state['boot_id'],
                'epoch': epoch, 'monotonic': monotonic}

    def geo_event(kind, epoch, monotonic, **details):
        nonlocal event_count
        if event_count >= 256:
            raise RuntimeError('bounded passive GeoClue event limit exceeded')
        event_count += 1
        for row in clients.values():
            location = row.get('_location_path')
            if location is not None:
                row['location'] = locations.get(location)
        actual = [{key: value for key, value in row.items() if not key.startswith('_')}
                  for row in clients.values()]
        append(geo_descriptor, stamp(epoch, monotonic) |
               {'kind': kind, 'service': geo_service, 'gsd_pid': state['gsd']['pid'],
                'gsd_uid': state['gsd']['uid'], 'clients': actual} | details)

    def bind_location(client_path, location_path):
        if not isinstance(location_path, str) or not location_path.startswith('/'):
            raise RuntimeError('actual client-scoped GeoClue Location property is not an object path')
        if location_path != '/':
            if location_path not in location_clients and len(location_clients) >= 256:
                raise RuntimeError('bounded client-scoped GeoClue location limit exceeded')
            location_clients.setdefault(location_path, set()).add(client_path)

    def apply_properties(path, interface, values):
        if interface == 'org.freedesktop.GeoClue2.Client' and path in clients:
            row = clients[path]
            for source, target in (('DesktopId', 'desktop_id'), ('Active', 'active'),
                                   ('RequestedAccuracyLevel', 'requested_accuracy_level')):
                if source in values:
                    row[target] = values[source]
            if 'Location' in values:
                bind_location(path, values['Location'])
                row['_location_path'] = values['Location']
        elif interface == 'org.freedesktop.GeoClue2.Location':
            location = locations.setdefault(path, {'path': path, 'latitude': None,
                                                    'longitude': None, 'accuracy': None})
            for source, target in (('Latitude', 'latitude'), ('Longitude', 'longitude'),
                                   ('Accuracy', 'accuracy')):
                if source in values:
                    location[target] = values[source]

    def handle(message, epoch, monotonic):
        nonlocal queued
        with lock:
            queued -= 1
        try:
            kind = message.get_message_type()
            sender, destination = message.get_sender(), message.get_destination()
            path, interface, member = message.get_path(), message.get_interface(), message.get_member()
            if (kind == Gio.DBusMessageType.METHOD_CALL and destination in destinations
                    and path == TIME_PATH and interface == TIME_NAME and member == 'SetTimezone'):
                record = stamp(epoch, monotonic) | {
                    'sender': sender, 'sender_pid': None, 'sender_uid': None,
                    'zone': None, 'interactive': None}
                try:
                    body = message.get_body()
                    if body is None or body.get_type_string() != '(sb)':
                        raise RuntimeError('observed SetTimezone signature differs from sb')
                    record['zone'], record['interactive'] = body.unpack()
                    record['sender_pid'], record['sender_uid'] = owner(sender)
                except Exception as error:
                    record['error'] = str(error)
                append(descriptor, record)
                return GLib.SOURCE_REMOVE
            if kind == Gio.DBusMessageType.METHOD_CALL:
                if destination not in geo_destinations or not original(sender):
                    return GLib.SOURCE_REMOVE
                body = message.get_body()
                signatures = {'GetClient': '()', 'CreateClient': '()', 'Start': '()', 'Stop': '()',
                              'GetAll': '(s)', 'Get': '(ss)', 'Set': '(ssv)'}
                if member not in signatures:
                    return GLib.SOURCE_REMOVE
                signature = body.get_type_string() if body is not None else '()'
                if signature != signatures[member]:
                    raise RuntimeError('actual original-gsd GeoClue method signature malformed: ' + member)
                values = body.unpack() if body is not None else ()
                request = {'sender': sender, 'serial': message.get_serial(), 'path': path,
                           'interface': interface, 'method': member}
                if interface == 'org.freedesktop.GeoClue2.Manager':
                    if path != GEO_PATH + '/Manager' or member not in ('GetClient', 'CreateClient'):
                        return GLib.SOURCE_REMOVE
                elif interface == 'org.freedesktop.GeoClue2.Client':
                    if (path not in clients or clients[path]['owner_bus_name'] != sender
                            or member not in ('Start', 'Stop')):
                        return GLib.SOURCE_REMOVE
                elif interface == 'org.freedesktop.DBus.Properties':
                    target = values[0] if values else None
                    if target not in ('org.freedesktop.GeoClue2.Client', 'org.freedesktop.GeoClue2.Location'):
                        return GLib.SOURCE_REMOVE
                    if target.endswith('.Client') and (
                            path not in clients or clients[path]['owner_bus_name'] != sender):
                        return GLib.SOURCE_REMOVE
                    if target.endswith('.Location'):
                        bound_clients = location_clients.get(path, set())
                        if not any(clients[client]['owner_bus_name'] == sender for client in bound_clients):
                            return GLib.SOURCE_REMOVE
                    request['property_interface'] = target
                    if member in ('Get', 'Set'):
                        allowed = ('DesktopId', 'Active', 'RequestedAccuracyLevel', 'Location',
                                   'Latitude', 'Longitude', 'Accuracy')
                        if len(values) < 2 or values[1] not in allowed:
                            return GLib.SOURCE_REMOVE
                        request['property'] = values[1]
                        if member == 'Set':
                            request['value'] = values[2]
                else:
                    return GLib.SOURCE_REMOVE
                if len(pending) >= 256:
                    raise RuntimeError('bounded original-gsd GeoClue request limit exceeded')
                pending[(sender, message.get_serial())] = request
                return GLib.SOURCE_REMOVE
            if kind in (Gio.DBusMessageType.METHOD_RETURN, Gio.DBusMessageType.ERROR):
                request = pending.pop((destination, message.get_reply_serial()), None)
                if sender != geo_service['unique_owner'] or request is None:
                    return GLib.SOURCE_REMOVE
                if not original(destination):
                    raise RuntimeError('GeoClue response destination no longer original datetime process')
                details = {'sender': destination, 'request': request,
                           'reply_serial': message.get_reply_serial()}
                if kind == Gio.DBusMessageType.ERROR:
                    details['error_name'] = message.get_error_name()
                    body = message.get_body()
                    details['error'] = body.unpack() if body is not None else None
                    geo_event('method-error', epoch, monotonic, **details)
                    return GLib.SOURCE_REMOVE
                body = message.get_body()
                values = body.unpack() if body is not None else ()
                method, path = request['method'], request['path']
                if method in ('GetClient', 'CreateClient'):
                    if len(values) != 1 or not values[0].startswith(GEO_PATH + '/Client/'):
                        raise RuntimeError('actual GeoClue client creation reply malformed')
                    client_path = values[0]
                    if len(clients) >= 16:
                        raise RuntimeError('bounded original-gsd GeoClue client limit exceeded')
                    clients[client_path] = {
                        'path': client_path, 'desktop_id': None, 'active': None,
                        'requested_accuracy_level': None, 'location': None,
                        'owner_bus_name': destination, 'owner_pid': state['gsd']['pid'],
                        'owner_uid': state['gsd']['uid'],
                        'evidence_source': 'passive-original-gsd-system-bus',
                        'observation_semantics': 'observed-within-interval-not-current-properties'}
                    details['client_path'] = client_path
                elif method in ('Start', 'Stop'):
                    if values:
                        raise RuntimeError('actual GeoClue Start/Stop success reply malformed')
                    clients[path]['active'] = method == 'Start'
                elif method == 'Set':
                    if values:
                        raise RuntimeError('actual GeoClue property Set success reply malformed')
                    apply_properties(path, request['property_interface'],
                                     {request['property']: request['value']})
                elif method in ('GetAll', 'Get'):
                    if len(values) != 1:
                        raise RuntimeError('actual GeoClue property read reply malformed')
                    properties = values[0] if method == 'GetAll' else {request['property']: values[0]}
                    apply_properties(path, request['property_interface'], properties)
                    details['observed_properties'] = {
                        key: value for key, value in properties.items()
                        if key in ('DesktopId', 'Active', 'RequestedAccuracyLevel', 'Location',
                                   'Latitude', 'Longitude', 'Accuracy')}
                geo_event('method-success', epoch, monotonic, **details)
                return GLib.SOURCE_REMOVE
            if (kind == Gio.DBusMessageType.SIGNAL and sender == geo_service['unique_owner']
                    and path in clients and interface == 'org.freedesktop.GeoClue2.Client'
                    and member == 'LocationUpdated'):
                if not original(clients[path]['owner_bus_name']):
                    raise RuntimeError('GeoClue client original bus owner changed')
                body = message.get_body()
                if body is None or body.get_type_string() != '(oo)':
                    raise RuntimeError('actual GeoClue LocationUpdated signature malformed')
                previous, current = body.unpack()
                bind_location(path, previous)
                bind_location(path, current)
                clients[path]['_location_path'] = current
                geo_event('location-updated', epoch, monotonic,
                          client_path=path, previous_location=previous, location_path=current)
        except Exception as error:
            _write_json(directory / 'monitor-error.json', {'error': str(error), 'epoch': time.time()})
            loop.quit()
        return GLib.SOURCE_REMOVE

    def queue_error(message):
        _write_json(directory / 'monitor-error.json', {'error': message, 'epoch': time.time()})
        loop.quit()
        return GLib.SOURCE_REMOVE

    def received(connection, message, incoming, user_data):
        nonlocal queued, traffic_count
        kinds = {Gio.DBusMessageType.METHOD_CALL: 'call',
                 Gio.DBusMessageType.METHOD_RETURN: 'return',
                 Gio.DBusMessageType.ERROR: 'error',
                 Gio.DBusMessageType.SIGNAL: 'signal'}
        kind = kinds.get(message.get_message_type(), '?')
        with lock:
            if traffic_count < 1024:
                record = {'in': bool(incoming), 't': kind, 'n': message.get_serial()}
                for key, value in (('s', message.get_sender()),
                                   ('d', message.get_destination()),
                                   ('p', message.get_path()),
                                   ('i', message.get_interface()),
                                   ('m', message.get_member())):
                    if value is not None:
                        record[key] = value
                raw = (json.dumps(record, separators=(',', ':')) + '\n').encode()
                if os.write(traffic_descriptor, raw) == len(raw):
                    traffic_count += 1
        if not incoming:
            return message
        action = _filter_action(kind, message.get_sender(), message.get_destination(),
                                message.get_path(), message.get_interface(), message.get_member(),
                                destinations, geo_destinations, geo_service['unique_owner'])
        if action == 'pass':
            return message
        if action == 'drop':
            return None
        # GDBus filters run on the shared I/O worker: never call_sync here, even on another connection.
        with lock:
            if queued >= 256:
                GLib.idle_add(queue_error, 'bounded passive bus-observation queue limit exceeded')
                return None
            queued += 1
        try:
            copied = message.copy()
        except GLib.Error as error:
            with lock:
                queued -= 1
            GLib.idle_add(queue_error, 'passive bus message copy failed: ' + str(error))
            return None
        GLib.idle_add(handle, copied, time.time(), time.monotonic())
        return None

    monitor.add_filter(received, None)
    try:
        _call(monitor, 'org.freedesktop.DBus', '/org/freedesktop/DBus',
              'org.freedesktop.DBus.Monitoring', 'BecomeMonitor', '(asu)', (rules, 0), '()')
        # call_sync has returned the daemon's acknowledgement, not merely queued a request.
        actual = _proc_identity(os.getpid())
        admitted_deadline = absolute_start + MONITOR_SECONDS - 1
        _write_json(directory / 'ready.json',
                    {key: actual[key] for key in
                     ('pid', 'start_ticks', 'exe', 'exe_device', 'exe_inode', 'argv')} |
                    {'ready': True, 'nonce': nonce, 'boot_id': state['boot_id'], 'rules': rules,
                     'epoch': time.time(), 'monotonic': time.monotonic(),
                     'started_monotonic': absolute_start, 'deadline_monotonic': admitted_deadline,
                     'lifetime_seconds': MONITOR_SECONDS, 'service': service,
                     'geoclue_service': geo_service}, exclusive=True)

        def finish():
            _write_json(directory / 'monitor-ended.json',
                        {'pid': os.getpid(), 'nonce': nonce, 'boot_id': state['boot_id'],
                         'epoch': time.time(), 'monotonic': time.monotonic()})
            loop.quit()
            return GLib.SOURCE_REMOVE

        remaining = admitted_deadline - time.monotonic()
        GLib.timeout_add(max(1, int(remaining * 1000)), finish)
        GLib.unix_signal_add(GLib.PRIORITY_DEFAULT, signal.SIGTERM, finish)
        monitor.connect('closed', lambda *unused: finish())
        loop.run()
    finally:
        # Publish retirement before closing connections/descriptors; even a
        # still-running process in teardown is no longer an observation source.
        _write_json(directory / 'monitor-ended.json',
                    {'pid': os.getpid(), 'nonce': nonce, 'boot_id': state['boot_id'],
                     'epoch': time.time(), 'monotonic': time.monotonic()})
        monitor.close_sync(None)
        credentials.close_sync(None)
        os.close(descriptor)
        os.close(geo_descriptor)
        os.close(traffic_descriptor)


def _restore_timezone(context, directory, state):
    original = state['original']['timedate']
    _command(context, ['busctl', '--system', 'call', TIME_NAME, TIME_PATH, TIME_NAME,
                       'SetTimezone', 'sb', original['zone'], 'false'],
             directory=directory, state=state)
    if original['localtime']['present'] is False:
        # SetTimezone("UTC") normally creates a UTC symlink. Restore the actual
        # initial absence, not just equivalent wall time, only after readback.
        if original['zone'] != 'UTC':
            raise RuntimeError('saved absent localtime is not systemd default UTC')
        Gio, _ = _gio()
        actual = _timedate(Gio.bus_get_sync(Gio.BusType.SYSTEM, None))
        if actual['zone'] != 'UTC':
            raise RuntimeError('cleanup timezone did not return to UTC')
        if actual['localtime']['present']:
            if not actual['localtime']['matches_zone']:
                raise RuntimeError('cleanup localtime does not match UTC')
            Path('/etc/localtime').unlink()


def _restore(context, directory, state):
    errors = []
    try:
        _stop_monitor(directory, state)
    except Exception as error:
        errors.append({'operation': 'stop-monitor', 'error': str(error)})
    # Cleanup is the sole root SetTimezone path. Attempt every restoration even after failure.
    for key in ('automatic', 'master'):
        schema, name = SETTINGS[key]
        try:
            _command(context, ['gsettings', 'set', schema, name, 'false'],
                     user=True, directory=directory, state=state)
        except Exception as error:
            errors.append({'operation': 'disable-' + key, 'error': str(error)})
    try:
        _restore_timezone(context, directory, state)
    except Exception as error:
        errors.append({'operation': 'restore-timezone-cleanup-only', 'error': str(error)})
    for key in _setting_order(state['original']['settings']):
        schema, name = SETTINGS[key]
        try:
            _command(context, ['gsettings', 'set', schema, name,
                               str(state['original']['settings'][key]).lower()],
                     user=True, directory=directory, state=state)
        except Exception as error:
            errors.append({'operation': 'restore-' + key, 'error': str(error)})
    try:
        Gio, _ = _gio()
        actual_settings = _settings(context)
        actual_time = _timedate(Gio.bus_get_sync(Gio.BusType.SYSTEM, None))
        original = state['original']
        if (actual_settings != original['settings'] or actual_time['zone'] != original['timedate']['zone']
                or actual_time['localtime'] != original['timedate']['localtime']):
            raise RuntimeError('actual restored settings/timezone/localtime differ: ' +
                               json.dumps({'settings': actual_settings, 'timedate': actual_time}))
    except Exception as error:
        errors.append({'operation': 'restore-readback', 'error': str(error)})
    state['restore'] = {'restored': not errors, 'errors': errors, 'epoch': time.time()}
    _persist(directory, state)
    if errors:
        raise RuntimeError('actual timezone restoration failed: ' + json.dumps(state['restore']))
    result = _snapshot(context, directory, state, restoration=True)
    result['restored'] = True
    return result


def dispatch(operation, arguments, context):
    if not isinstance(arguments, dict):
        raise ValueError('typed timezone arguments required')
    if operation in ('fixture-prepare', 'fixture-observe', 'fixture-restore'):
        if arguments:
            raise ValueError('fixture operations accept no caller configuration')
        return getattr(importlib.import_module('geoclue_fixture'), operation.split('-')[1])(context)
    if operation == 'configure':
        if set(arguments) != set(SETTINGS) or any(type(value) is not bool for value in arguments.values()):
            raise ValueError('exact master and automatic boolean arguments required')
    elif arguments:
        raise ValueError('timezone operation accepts no caller configuration')
    if operation == 'save':
        directory = _directory(context.nonce, create=True)
        if (directory / 'state.json').exists():
            directory, state = _load(context)
            observed = _snapshot(context, directory, state)
            observed['original_saved'] = state['original']
            observed['save_reused'] = True
            return observed
        observed = _snapshot(context)
        state = {'nonce': context.nonce, 'boot_id': _boot(), 'uid': context.uid,
                 'original': {'settings': observed['settings'], 'timedate': observed['timedate']},
                 'gsd': observed['gsd'], 'commands': []}
        _write_json(directory / 'state.json', state, exclusive=True)
        observed['original_saved'] = state['original']
        observed['save_reused'] = False
        return observed
    if operation not in ('configure', 'manual-baseline', 'snapshot', 'watch-start', 'restore'):
        raise ValueError('unknown automatic-timezone operation')
    directory, state = _load(context)
    if operation == 'restore':
        return _restore(context, directory, state)
    if operation == 'configure':
        _configure(context, arguments, directory, state)
    elif operation == 'manual-baseline':
        if _settings(context) != {'master': False, 'automatic': False}:
            raise RuntimeError('manual negative-control seed requires both switches disabled')
        _command(context, ['systemd-run', '--user', '--wait', '--pipe', '--collect',
                           'busctl', '--system', 'call', TIME_NAME, TIME_PATH, TIME_NAME,
                           'SetTimezone', 'sb', 'Pacific/Auckland', 'false'],
                 user=True, directory=directory, state=state)
        result = _snapshot(context, directory, state)
        if result['timedate']['zone'] != 'Pacific/Auckland' or not result['timedate']['localtime']['matches_zone']:
            raise RuntimeError('actual original-user manual baseline not applied: ' + json.dumps(result))
        return result
    elif operation == 'watch-start':
        return _watch_start(context, directory, state)
    return _snapshot(context, directory, state)


if __name__ == '__main__':
    if len(sys.argv) != 3 or sys.argv[1] != '--monitor':
        raise ValueError('only the owned fixed-lifetime timezone monitor subprocess is supported')
    try:
        _monitor(sys.argv[2])
    except Exception as error:
        _write_json(_directory(sys.argv[2]) / 'monitor-error.json',
                    {'error': str(error), 'epoch': time.time(), 'pid': os.getpid()})
        raise