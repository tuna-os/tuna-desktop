#!/usr/bin/env python3
"""Exercise the GTK-owned Lockdown through a real portal frontend.

The Access-only fixture cancels permission dialogs. A returned request handle
proves passage through the lockdown gate, never camera permission/hardware.
"""
import argparse
import hashlib
import json
import os
import re
from pathlib import Path
import subprocess
import time

NAME = 'org.freedesktop.impl.portal.desktop.tuna'
PATH = '/org/freedesktop/portal/desktop'
IFACE = 'org.freedesktop.impl.portal.Lockdown'


def classify_camera(result, error):
    if error is not None:
        if re.search(r'(?:^|GDBus.Error:)org\.freedesktop\.portal\.Error\.NotAllowed: Camera access disabled(?: \(\d+\))?$', error):
            return 'lockdown-denied'
        raise RuntimeError(f'camera call failed outside lockdown: {error}')
    if (isinstance(result, tuple) and len(result) == 1
            and isinstance(result[0], str)
            and result[0].startswith('/org/freedesktop/portal/desktop/request/')):
        return 'request-admitted'
    raise RuntimeError(f'camera call returned no request handle: {result!r}')


def metadata(portal, config):
    if 'Interfaces=org.freedesktop.impl.portal.Lockdown;' not in portal:
        raise RuntimeError('shipped Tuna backend does not advertise Lockdown')
    if 'org.freedesktop.impl.portal.Lockdown=tuna;' not in config:
        raise RuntimeError('shipped desktop config does not select Tuna Lockdown')
    if 'org.freedesktop.impl.portal.Access=gtk;' not in config:
        raise RuntimeError('production Access mapping changed unexpectedly')
    return config.replace('org.freedesktop.impl.portal.Access=gtk;',
                          'org.freedesktop.impl.portal.Access=proof-access;')



def digest(path):
    value = hashlib.sha256()
    with path.open('rb') as stream:
        for block in iter(lambda: stream.read(65536), b''):
            value.update(block)
    return value.hexdigest()


def process_identity(pid):
    proc = Path(f'/proc/{pid}')
    return {'boot_id': Path('/proc/sys/kernel/random/boot_id').read_text().strip(),
            'start_ticks': proc.joinpath('stat').read_text().rsplit(')', 1)[1].split()[19],
            'sha256': digest(proc / 'exe')}



def validate_publisher(executable, expected, identity):
    if executable.resolve().name != 'tuna-shell-gtk':
        raise RuntimeError(f'publisher is not actual GTK shell: {executable.resolve()}')
    expected = expected.resolve(strict=True)
    if executable.resolve() != expected or identity['sha256'] != digest(expected):
        raise RuntimeError('publisher does not match the harness expected GTK binary')
    return expected


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--installed', action='store_true')
    parser.add_argument('--expected-shell', type=Path, required=True)
    args = parser.parse_args()
    import gi
    gi.require_version('Gio', '2.0')
    from gi.repository import Gio, GLib
    args.output.mkdir(parents=True, exist_ok=False)
    bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)

    def call(name, path, interface, method, parameters=None):
        return bus.call_sync(name, path, interface, method, parameters, None,
                             Gio.DBusCallFlags.NONE, 2000, None).unpack()

    def dbus(method, params):
        return call('org.freedesktop.DBus', '/org/freedesktop/DBus',
                    'org.freedesktop.DBus', method, params)

    def wait(read, expected):
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            try:
                value = read()
                if value == expected:
                    return value
            except GLib.Error:
                pass
            time.sleep(.1)
        raise RuntimeError(f'expected {expected!r} was never observed')

    wait(lambda: dbus('NameHasOwner', GLib.Variant('(s)', (NAME,)))[0], True)
    owner = dbus('GetNameOwner', GLib.Variant('(s)', (NAME,)))[0]
    pid = dbus('GetConnectionUnixProcessID', GLib.Variant('(s)', (owner,)))[0]
    executable = Path(f'/proc/{pid}/exe')
    identity = process_identity(pid)
    expected = validate_publisher(executable, args.expected_shell, identity)
    receipt = {
        'scope': 'GTK-owned Lockdown and real portal camera gate; no hardware or permission grant qualification',
        'publisher': {'name': NAME, 'owner': owner, 'pid': pid,
                      'executable': str(executable.resolve()),
                      'expected_executable': str(expected), **identity},
        'states': [],
        'complete': False,
    }
    base = Path('/usr/share/xdg-desktop-portal') if args.installed else args.root / 'share/xdg-desktop-portal'
    portal_path = base / 'portals/tuna.portal'
    config_path = (base / 'tuna-portals.conf' if args.installed
                   else args.root / 'packaging/marlin/tuna-portals.conf')
    portal = portal_path.read_text()
    config = config_path.read_text()
    fixture_config = metadata(portal, config)
    receipt['metadata'] = {str(path): hashlib.sha256(path.read_bytes()).hexdigest()
                           for path in (portal_path, config_path)}
    receipt['fixture_override'] = 'Access=proof-access cancels dialogs via the GTK-owned fail-closed Access implementation; Lockdown mapping unchanged'
    portals = args.output / 'portals'
    portals.mkdir()
    (portals / 'tuna.portal').write_text(portal)
    (portals / 'proof-access.portal').write_text(
        f'[portal]\nDBusName={NAME}\nInterfaces=org.freedesktop.impl.portal.Access;\nUseIn=Tuna;\n')
    config_dir = args.output / 'config/xdg-desktop-portal'
    config_dir.mkdir(parents=True)
    (config_dir / 'tuna-portals.conf').write_text(fixture_config)
    daemon = next((Path(p) for p in ('/usr/libexec/xdg-desktop-portal',
                                    '/usr/lib/xdg-desktop-portal') if Path(p).is_file()), None)
    if daemon is None:
        raise RuntimeError('real xdg-desktop-portal is unavailable')
    if os.environ.get('TUNA_PROOF_PRIVATE_BUS') != '1':
        raise RuntimeError('proof may replace a frontend only on its owned private bus')
    if dbus('NameHasOwner', GLib.Variant('(s)', ('org.freedesktop.portal.Desktop',)))[0]:
        previous = dbus('GetNameOwner', GLib.Variant('(s)', ('org.freedesktop.portal.Desktop',)))[0]
        previous_pid = dbus('GetConnectionUnixProcessID', GLib.Variant('(s)', (previous,)))[0]
        previous_exe = Path(f'/proc/{previous_pid}/exe').resolve()
        if previous_exe.name != 'xdg-desktop-portal':
            raise RuntimeError('unexpected frontend owner on private proof bus')
        receipt['replaced_private_frontend'] = {'owner': previous, 'pid': previous_pid,
                                                'executable': str(previous_exe)}
    schema = Gio.SettingsSchemaSource.get_default().lookup('org.gnome.desktop.privacy', True)
    if schema is None or not schema.has_key('disable-camera'):
        raise RuntimeError('privacy schema/key unavailable')
    settings = Gio.Settings.new_full(schema, None, None)
    original = settings.get_user_value('disable-camera')
    if not settings.get_boolean('disable-camera'):
        raise RuntimeError('startup fixture did not persist disabled camera policy')
    process = None

    def property_value():
        return call(NAME, PATH, 'org.freedesktop.DBus.Properties', 'Get',
                    GLib.Variant('(ss)', (IFACE, 'disable-camera')))[0]

    def camera():
        try:
            result = call('org.freedesktop.portal.Desktop', PATH,
                          'org.freedesktop.portal.Camera', 'AccessCamera',
                          GLib.Variant('(a{sv})', ({},)))
            return classify_camera(result, None), {'result': result}
        except GLib.Error as error:
            return classify_camera(None, str(error)), {'error': str(error)}

    try:
        if property_value() is not True:
            raise RuntimeError('first acquired GTK publisher policy was not startup-disabled')
        receipt['startup_disabled_observed'] = True
        env = os.environ.copy()
        env.update(XDG_CURRENT_DESKTOP='Tuna:GNOME',
                   XDG_CONFIG_HOME=str(args.output / 'config'),
                   XDG_DESKTOP_PORTAL_DIR=str(portals))
        with (args.output / 'portal.log').open('w') as log:
            process = subprocess.Popen([str(daemon), '--replace'], env=env, stdout=log,
                                       stderr=subprocess.STDOUT)
        wait(lambda: dbus('GetConnectionUnixProcessID', GLib.Variant('(s)', (
            dbus('GetNameOwner', GLib.Variant('(s)', ('org.freedesktop.portal.Desktop',)))[0],)))[0], process.pid)
        receipt['frontend_pid'] = process.pid
        outcome, details = camera()
        if outcome != 'lockdown-denied':
            raise RuntimeError('persisted startup camera policy did not deny the actual portal call')
        receipt['states'].append({'phase': 'startup', 'disabled': True,
                                  'outcome': outcome, **details})
        for disabled, expected in ((False, 'request-admitted'),
                                   (True, 'lockdown-denied'),
                                   (False, 'request-admitted')):
            if not settings.set_boolean('disable-camera', disabled):
                raise RuntimeError('privacy setting is not writable')
            Gio.Settings.sync()
            wait(property_value, disabled)
            # Observe delivery to the real frontend, without retrying input or
            # accepting timeouts/errors as proof of admission. The direct
            # backend property alone cannot prove portal enforcement.
            deadline = time.monotonic() + 10
            while True:
                outcome, details = camera()
                if outcome == expected:
                    break
                if time.monotonic() >= deadline:
                    raise RuntimeError(f'portal gate remained {outcome}; expected {expected}')
                time.sleep(.1)
            receipt['states'].append({'disabled': disabled, 'outcome': outcome, **details})
        current = dbus('GetNameOwner', GLib.Variant('(s)', (NAME,)))[0]
        if current != owner or process_identity(pid) != identity:
            raise RuntimeError('GTK publisher owner, process lifetime, boot or binary changed during proof')
        receipt['complete'] = True
    finally:
        if original is None:
            settings.reset('disable-camera')
        else:
            settings.set_value('disable-camera', original)
        Gio.Settings.sync()
        if process is not None:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
        (args.output / 'receipt.json').write_text(json.dumps(receipt, indent=2) + '\n')


if __name__ == '__main__':
    main()
