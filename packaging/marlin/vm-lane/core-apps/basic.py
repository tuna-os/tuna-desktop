"""Independent observations for real Calculator, Fonts, viewers and Settings."""
import json
import os
from pathlib import Path
import signal
import sys
import time

from guest import digest, process


def user(operation, arguments):
    import gi
    from gi.repository import Gio
    if os.geteuid() == 0:
        raise RuntimeError('original ordinary user settings/metadata consumer required')
    if operation == 'calculator-state':
        settings = Gio.Settings.new('org.gnome.calculator')
        keys = ('show-thousands', 'show-zeroes', 'accuracy', 'button-mode', 'number-format')
        return {'settings': {key: settings.get_value(key).unpack() for key in keys}}
    if operation == 'style-state':
        settings = Gio.Settings.new('org.gnome.desktop.interface')
        return {'color_scheme': settings.get_string('color-scheme'),
                'gtk_theme': settings.get_string('gtk-theme')}
    if operation == 'viewer-state':
        file = Gio.File.new_for_path(arguments['path'])
        info = file.query_info('metadata::*', Gio.FileQueryInfoFlags.NONE, None)
        metadata = {key: info.get_attribute_as_string(key) for key in info.list_attributes('metadata')}
        schemas = Gio.SettingsSchemaSource.get_default()
        schema = schemas.lookup(arguments['app_id'], True)
        preferences = {}
        if schema is not None:
            settings = Gio.Settings.new_full(schema, None, None)
            preferences = {key: settings.get_value(key).unpack() for key in schema.list_keys()}
        return {'file_metadata': metadata, 'preferences': preferences,
                'schema_id': schema.get_id() if schema is not None else None}
    if operation == 'dconf-owner':
        from gi.repository import GLib
        bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
        pid = bus.call_sync('org.freedesktop.DBus', '/org/freedesktop/DBus', 'org.freedesktop.DBus',
                            'GetConnectionUnixProcessID', GLib.Variant('(s)', ('ca.desrt.dconf',)),
                            GLib.VariantType.new('(u)'), Gio.DBusCallFlags.NONE, 2000, None).unpack()[0]
        return process(pid, os.geteuid())
    raise ValueError('unknown readonly original-user basic operation')


def _user(context, operation, **arguments):
    result = context.user(['/usr/bin/python', str(Path(__file__).resolve()), '--user', operation,
                           json.dumps(arguments, separators=(',', ':'))])
    return json.loads(result.stdout)


def dispatch(operation, arguments, context):
    if operation == 'decoder-control':
        controls = {'png': ('basic-broken.png', b'\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR'),
                    'pdf': ('basic-broken.pdf', b'%PDF-1.7\n%%EOF\n')}
        filename, payload = controls[arguments['kind']]
        path = context.path(filename)
        context.user(['/usr/bin/python', '-c',
            'import os,pathlib,sys; assert os.geteuid()!=0; '
            'pathlib.Path(sys.argv[1]).open("xb").write(bytes.fromhex(sys.argv[2]))',
            str(path), payload.hex()])
        return {'path': str(path), 'relative': filename, 'bytes': len(payload),
                'sha256': digest(path), 'prepared_control_only': True}
    if operation == 'font-state':
        root = context.home / '.local/share/fonts'
        installed = root / 'NotoSansCJKjp-Regular.otf'
        matched = context.user(['fc-match', '-f', '%{file}\n%{family}\n%{style}\n',
                                'Noto Sans CJK JP']).stdout.splitlines()
        if len(matched) != 3:
            raise RuntimeError('actual fontconfig consumer metadata malformed')
        file = Path(matched[0]).resolve(strict=True)
        result = {'match': {'file': str(file), 'family': matched[1], 'style': matched[2],
                            'sha256': digest(file)},
                  'user_font': {'path': str(installed), 'exists': installed.exists()},
                  'fontconfig_package': context.command(['pacman', '-Q', 'fontconfig']).stdout.strip()}
        if installed.exists():
            if installed.is_symlink() or installed.stat().st_uid != context.uid:
                raise RuntimeError('actual user-installed font owner/type differs')
            result['user_font']['sha256'] = digest(installed)
        return result
    if operation == 'calculator-state':
        return _user(context, operation)
    if operation == 'viewer-state':
        path = context.path(arguments['relative'])
        return _user(context, operation, path=str(path), app_id=arguments['app_id'])
    if operation in ('style-save', 'style-state', 'style-restore'):
        saved = 'basic-style'
        if operation == 'style-save':
            if context.control_exists(saved):
                raise RuntimeError('original pre-proof style snapshot already exists')
            value = {'identity': context.identity(), 'nonce': context.nonce,
                     'original': _user(context, 'style-state'), 'restored': False}
            context.save_control(saved, value)
            return value
        if operation == 'style-restore':
            source = context.read_control(saved)
            if source['identity'] != context.identity() or source['nonce'] != context.nonce:
                raise RuntimeError('original style cleanup owner changed')
            original = source['original']['color_scheme']
            if original not in ('default', 'prefer-dark', 'prefer-light'):
                raise RuntimeError('original actual color-scheme value unsupported')
            if not source['restored']:
                context.user(['gsettings', 'set', 'org.gnome.desktop.interface', 'color-scheme', original])
            current = _user(context, 'style-state')
            if current != source['original']:
                raise RuntimeError('exact original user style was not restored after proof')
            source['restored'] = True
            context.save_control(saved, source)
            return {'restored': current, 'cleanup_only_not_operation_proof': True}
        value = _user(context, operation)
        database = context.home / '.config/dconf/user'
        value['dconf_database'] = {'path': str(database), 'sha256': digest(database)}
        value['dconf_owner'] = _user(context, 'dconf-owner')
        if value['dconf_owner']['exe'] != '/usr/lib/dconf-service':
            raise RuntimeError('actual settings writer is not installed dconf service')
        value['dconf_package'] = context.command(['pacman', '-Q', 'dconf']).stdout.strip()
        return value
    if operation in ('dconf-interrupt', 'dconf-restore'):
        saved = 'basic-dconf'
        if operation == 'dconf-interrupt':
            if context.control_exists(saved):
                raise RuntimeError('owned dconf outage already exists')
            owner = _user(context, 'dconf-owner')
            if owner['exe'] != '/usr/lib/dconf-service':
                raise RuntimeError('actual dconf backend not genuine')
            context.save_control(saved, {'nonce': context.nonce, 'identity': context.identity(),
                                         'owner': owner, 'suspended': True})
            os.kill(owner['pid'], signal.SIGSTOP)
            deadline = time.monotonic() + 2
            while time.monotonic() < deadline:
                status = Path(f"/proc/{owner['pid']}/status").read_text()
                state = next(line for line in status.splitlines() if line.startswith('State:'))
                if state.split()[1] in ('T', 't'):
                    return {'owner': owner, 'state': state, 'actual_service_suspended': True}
                time.sleep(0.05)
            raise RuntimeError('actual owned dconf backend did not enter stopped state')
        source = context.read_control(saved)
        if source['nonce'] != context.nonce or source['identity'] != context.identity():
            raise RuntimeError('owned dconf restoration principal changed')
        if process(source['owner']['pid'], context.uid) != source['owner']:
            raise RuntimeError('original dconf process changed during controlled outage')
        if source['suspended']:
            os.kill(source['owner']['pid'], signal.SIGCONT)
        deadline = time.monotonic() + 2
        while time.monotonic() < deadline:
            state = next(line for line in Path(f"/proc/{source['owner']['pid']}/status").read_text().splitlines()
                         if line.startswith('State:'))
            if state.split()[1] not in ('T', 't'):
                source['suspended'] = False
                context.save_control(saved, source)
                return {'owner': source['owner'], 'state': state, 'actual_service_resumed': True}
            time.sleep(0.05)
        raise RuntimeError('actual owned dconf backend did not resume')
    if operation == 'cleanup':
        restored = {}
        if context.control_exists('basic-dconf'):
            source = context.read_control('basic-dconf')
            if source['suspended']:
                restored['writer'] = dispatch('dconf-restore', {}, context)
        if context.control_exists('basic-style'):
            source = context.read_control('basic-style')
            if not source['restored']:
                restored['style'] = dispatch('style-restore', {}, context)
        return {'owned_effects_restored': restored, 'cleanup_only_not_operation_proof': True}
    raise ValueError('unknown typed basic app observation')


if __name__ == '__main__':
    if len(sys.argv) != 4 or sys.argv[1] != '--user':
        raise ValueError('readonly original-user operation required')
    print(json.dumps(user(sys.argv[2], json.loads(sys.argv[3]))))
