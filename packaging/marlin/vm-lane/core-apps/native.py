"""Original-user display, portal, input-method and native-dialog observations."""
import base64
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

from guest import MODULE, RUN_STATE
from native_session import digest, process

PORTAL_LOG = Path('/run/tuna-core-apps-portal.jsonl')
IME_LOG = Path('/run/tuna-core-apps-ime.jsonl')


def _spawn(context, kind, arguments):
    path = PORTAL_LOG if kind == 'portal' else IME_LOG
    if path.exists():
        raise RuntimeError('owned monitor already exists; no observer reuse')
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    try:
        child = subprocess.Popen(['/usr/bin/python', str(MODULE), '--monitor', kind,
                                  json.dumps(arguments, separators=(',', ':'))],
                                 stdin=subprocess.DEVNULL, stdout=descriptor, stderr=subprocess.DEVNULL,
                                 env=context.environment(), user=context.uid,
                                 group=context.owner.pw_gid,
                                 extra_groups=os.getgrouplist(context.owner.pw_name, context.owner.pw_gid),
                                 start_new_session=True)
    finally:
        os.close(descriptor)
    deadline = time.monotonic() + 3
    while time.monotonic() < deadline:
        if child.poll() is not None:
            raise RuntimeError('actual original-user bus observer exited before admission')
        rows = _rows(path)
        if rows and rows[0].get('ready') is True:
            identity = process(child.pid, context.uid)
            state = json.loads(RUN_STATE.read_text())
            state[kind + '_monitor'] = identity
            RUN_STATE.write_text(json.dumps(state))
            return identity
        time.sleep(0.05)
    child.terminate()
    child.wait(timeout=2)
    raise RuntimeError('actual original-user monitor admission deadline expired')


def _rows(path):
    if path.stat().st_size > 524288:
        raise RuntimeError('bounded original-user bus observation exceeded evidence limit')
    raw = path.read_text()
    # A write in progress is readiness, never a fabricated complete message.
    complete = raw[:raw.rfind('\n') + 1]
    return [json.loads(line) for line in complete.splitlines()]


def _stop(context, kind):
    state = json.loads(RUN_STATE.read_text())
    original = state[kind + '_monitor']
    current = process(original['pid'], context.uid)
    if current != original:
        raise RuntimeError('original observer PID/start/code changed before retirement')
    os.kill(original['pid'], signal.SIGTERM)
    deadline = time.monotonic() + 3
    while time.monotonic() < deadline:
        try:
            fields = Path(f"/proc/{original['pid']}/stat").read_text().rsplit(')', 1)[1].split()
        except FileNotFoundError:
            return {'original': original, 'original_process_departed': True}
        if fields[0] == 'Z' or int(fields[19]) != original['start_ticks']:
            return {'original': original, 'original_process_departed': True}
        time.sleep(0.05)
    raise RuntimeError('original owned bus observer did not retire')


def monitor(kind, arguments):
    import gi
    gi.require_version('Gio', '2.0')
    from gi.repository import Gio, GLib
    if os.geteuid() == 0 or kind not in ('portal', 'ime'):
        raise RuntimeError('actual ordinary-user bounded bus observer required')
    if kind == 'portal':
        address = os.environ['DBUS_SESSION_BUS_ADDRESS']
        rules = ["type='method_call',interface='org.freedesktop.portal.Camera',member='AccessCamera'",
                 "type='method_call',interface='org.freedesktop.impl.portal.Access',member='AccessDialog'",
                 "type='signal',interface='org.freedesktop.portal.Request',member='Response'"]
    else:
        address = subprocess.check_output(['ibus', 'address'], text=True, timeout=3).strip()
        rules = ["type='signal',interface='org.freedesktop.IBus.InputContext',member='UpdatePreeditText'",
                 "type='signal',interface='org.freedesktop.IBus.InputContext',member='UpdatePreeditTextWithMode'"]
    flags = Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION
    bus = Gio.DBusConnection.new_for_address_sync(address, flags, None, None)
    credentials = Gio.DBusConnection.new_for_address_sync(address, flags, None, None)

    def unpack(value):
        if isinstance(value, GLib.Variant):
            return unpack(value.unpack())
        if isinstance(value, (tuple, list)):
            return [unpack(item) for item in value]
        if isinstance(value, dict):
            return {key: unpack(item) for key, item in value.items()}
        return value

    def owner(name):
        pid = credentials.call_sync('org.freedesktop.DBus', '/org/freedesktop/DBus',
                                    'org.freedesktop.DBus', 'GetConnectionUnixProcessID',
                                    GLib.Variant('(s)', (name,)), GLib.VariantType.new('(u)'),
                                    Gio.DBusCallFlags.NONE, 2000, None).unpack()[0]
        return process(pid, os.geteuid())
    count = 0

    def collect(value):
        # GIO filters share the message worker: synchronous credential queries
        # here run only after the filter returned, on the main-loop thread.
        try:
            value['sender_identity'] = owner(value['sender'])
            if kind == 'ime':
                destination = value['destination']
                if destination is None:
                    raise RuntimeError('real IBus preedit has no original client destination')
                value['destination_identity'] = owner(destination)
                target = value['destination_identity']
                if target['pid'] != arguments['actor']['pid'] and target['exe'] != '/usr/bin/tuna-ibus-bridge':
                    return False
            print(json.dumps(value), flush=True)
        except Exception as error:
            print(json.dumps({'observer_error_type': type(error).__name__}), flush=True)
        return False

    def receive(connection, message, incoming, unused):
        nonlocal count
        if not incoming or message.get_interface() == 'org.freedesktop.DBus':
            return message
        member = message.get_member()
        if member not in ('AccessCamera', 'AccessDialog', 'Response',
                          'UpdatePreeditText', 'UpdatePreeditTextWithMode'):
            return message
        count += 1
        if count > 256:
            os._exit(3)
        body = unpack(message.get_body())
        if len(json.dumps(body)) > 8192:
            os._exit(4)
        value = {'interface': message.get_interface(), 'member': member,
                 'path': message.get_path(), 'serial': message.get_serial(),
                 'sender': message.get_sender(), 'destination': message.get_destination(), 'body': body}
        GLib.idle_add(collect, value)
        if message.get_message_type() == Gio.DBusMessageType.METHOD_CALL:
            return None
        return message
    bus.add_filter(receive, None)
    if kind == 'ime':
        # ibus-daemon is its own bus implementation without
        # org.freedesktop.DBus.Monitoring (BecomeMonitor is UnknownMethod),
        # but it honors eavesdrop match rules, which deliver the daemon's
        # client-destined preedit signals to this observer.
        for rule in rules:
            bus.call_sync('org.freedesktop.DBus', '/org/freedesktop/DBus', 'org.freedesktop.DBus',
                          'AddMatch', GLib.Variant('(s)', (rule + ",eavesdrop='true'",)), None,
                          Gio.DBusCallFlags.NONE, 2000, None)
    else:
        bus.call_sync('org.freedesktop.DBus', '/org/freedesktop/DBus', 'org.freedesktop.DBus.Monitoring',
                      'BecomeMonitor', GLib.Variant('(asu)', (rules, 0)), None,
                      Gio.DBusCallFlags.NONE, 2000, None)
    print(json.dumps({'ready': True, 'kind': kind, 'uid': os.geteuid()}), flush=True)
    GLib.MainLoop().run()


def user_operation(operation, arguments):
    import gi
    from gi.repository import Gio, GLib
    bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)

    def call(destination, path, interface, method, parameters=None):
        return bus.call_sync(destination, path, interface, method, parameters, None,
                             Gio.DBusCallFlags.NONE, 3000, None).unpack()

    def service_identity(name):
        pid = call('org.freedesktop.DBus', '/org/freedesktop/DBus', 'org.freedesktop.DBus',
                   'GetConnectionUnixProcessID', GLib.Variant('(s)', (name,)))[0]
        return process(pid, os.geteuid())

    if operation == 'display-scale':
        service, path = 'org.gnome.Mutter.DisplayConfig', '/org/gnome/Mutter/DisplayConfig'
        before = call(service, path, service, 'GetCurrentState')
        serial, monitors, logical, properties = before
        if len(monitors) != 1 or len(logical) != 1 or len(logical[0][5]) != 1:
            raise RuntimeError('Core-app profile requires one actual native QEMU output')
        spec, modes, unused = monitors[0]
        current = [mode for mode in modes if mode[6].get('is-current')]
        if len(current) != 1 or arguments['scale'] not in current[0][5]:
            raise RuntimeError('requested scale not supported by actual native output mode')
        x, y, unused_scale, transform, primary, unused_specs, unused_props = logical[0]
        output = [(x, y, float(arguments['scale']), transform, primary, [(spec[0], current[0][0], {})])]
        options = {}
        if 'layout-mode' in properties:
            options['layout-mode'] = GLib.Variant('u', properties['layout-mode'])
        call(service, path, service, 'ApplyMonitorsConfig',
             GLib.Variant('(uua(iiduba(ssa{sv}))a{sv})', (serial, 1, output, options)))
        after = call(service, path, service, 'GetCurrentState')
        if len(after[2]) != 1 or after[2][0][2] != arguments['scale']:
            raise RuntimeError('actual output scale did not equal requested temporary scale')
        return {'before': before, 'after': after, 'scale': after[2][0][2],
                'service_identity': service_identity(service), 'temporary_not_persistent': True}
    if operation == 'default-handler':
        file = Gio.File.new_for_path(arguments['path'])
        info = file.query_info('standard::content-type', Gio.FileQueryInfoFlags.NONE, None)
        content_type = info.get_content_type()
        app = Gio.AppInfo.get_default_for_type(content_type, False)
        if app is None or app.get_id() is None:
            raise RuntimeError('no actual installed default handler for controlled content')
        return {'content_type': content_type, 'mime': Gio.content_type_get_mime_type(content_type),
                'default_handler': app.get_id(), 'supports_files': app.supports_files(),
                'supports_uris': app.supports_uris()}
    if operation == 'camera-permission':
        service = 'org.freedesktop.impl.portal.PermissionStore'
        try:
            value = call(service, '/org/freedesktop/impl/portal/PermissionStore', service,
                         'Lookup', GLib.Variant('(ss)', ('devices', 'camera')))
        except GLib.Error as error:
            if Gio.DBusError.get_remote_error(error) != 'org.freedesktop.portal.Error.NotFound':
                raise
            value = None
        identity = service_identity(service)
        if identity['exe'] != '/usr/lib/xdg-permission-store':
            raise RuntimeError('permission store owner is not installed real backend')
        return {'lookup': value, 'app_id': arguments['actor']['id'], 'service_identity': identity}
    if operation == 'ime-engine':
        gi.require_version('IBus', '1.0')
        from gi.repository import IBus
        IBus.init()
        ibus = IBus.Bus.new()
        if not ibus.is_connected():
            raise RuntimeError('real original-user IBus bus unavailable')
        previous = ibus.get_global_engine()
        if previous is None:
            # Fresh CI sessions configure no preloaded engines, so the daemon
            # offers no global engine until one is activated. Establish the
            # documented xkb baseline exactly like the nested proof's
            # `ibus engine xkb:us::eng`; the restore below then has a real
            # original to return to.
            if not ibus.set_global_engine('xkb:us::eng'):
                raise RuntimeError('real original-user IBus rejected xkb baseline engine')
            previous = ibus.get_global_engine()
            if previous is None:
                raise RuntimeError('real original-user IBus has no global engine')
        original = previous.get_name()
        requested = 'libpinyin' if arguments['select'] else arguments['original']
        if not ibus.set_global_engine(requested):
            raise RuntimeError('real IBus rejected requested engine selection')
        actual = ibus.get_global_engine()
        if actual is None or actual.get_name() != requested:
            raise RuntimeError('actual IBus global engine differs after real selection')
        return {'original': original, 'actual': actual.get_name(),
                'language': actual.get_language(), 'engine': actual.get_longname()}
    raise ValueError('unknown actual ordinary-user native operation')


def dispatch(operation, arguments, context):
    if operation == 'prepare':
        import web
        prepared_web = web.dispatch('prepare', {}, context)
        deadline = time.monotonic() + 3
        endpoint = web.dispatch('endpoint', {}, context)
        while not endpoint.get('ready') and time.monotonic() < deadline:
            time.sleep(0.05)
            endpoint = web.dispatch('endpoint', {}, context)
        if not endpoint.get('ready') or time.monotonic() >= deadline:
            raise RuntimeError('ordinary real HTTP content endpoint initial readiness expired')
        from PIL import Image, ImageDraw
        image = Image.new('RGB', (480, 320))
        draw = ImageDraw.Draw(image)
        colors = ((32, 74, 135), (204, 0, 0), (78, 154, 6), (252, 233, 79))
        for (x, y), color in zip(((0, 0), (240, 0), (0, 160), (240, 160)), colors):
            draw.rectangle((x, y, x + 239, y + 159), fill=color)
        draw.text((16, 16), 'Tuna Core Apps ' + context.nonce, fill='white')
        image.save(context.path('principal.png'))
        paragraph = 'Tuna Core Apps actual PDF content\n' + context.nonce + '\nOpen actual controlled link'
        stream = (f'BT /F1 12 Tf 40 220 Td (Tuna Core Apps actual PDF content) Tj '
                  f'0 -22 Td ({context.nonce}) Tj 0 -32 Td (Open actual controlled link) Tj ET').encode()
        objects = [b'<< /Type /Catalog /Pages 2 0 R >>',
                   b'<< /Type /Pages /Kids [3 0 R] /Count 1 >>',
                   b'<< /Type /Page /Parent 2 0 R /MediaBox [0 0 640 320] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R /Annots [6 0 R] >>',
                   b'<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>',
                   b'<< /Length ' + str(len(stream)).encode() + b' >>\nstream\n' + stream + b'\nendstream',
                   (f'<< /Type /Annot /Subtype /Link /Rect [40 160 240 180] /Border [0 0 1] '
                    f'/A << /S /URI /URI ({endpoint["url"]}) >> >>').encode()]
        pdf, offsets = bytearray(b'%PDF-1.4\n'), [0]
        for index, obj in enumerate(objects, 1):
            offsets.append(len(pdf))
            pdf.extend(f'{index} 0 obj\n'.encode() + obj + b'\nendobj\n')
        xref = len(pdf)
        pdf.extend(b'xref\n0 7\n0000000000 65535 f \n')
        for offset in offsets[1:]:
            pdf.extend(f'{offset:010d} 00000 n \n'.encode())
        pdf.extend(f'trailer\n<< /Size 7 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n'.encode())
        context.path('principal.pdf').write_bytes(pdf)
        font = Path('/usr/share/tuna-core-apps/NotoSansCJKjp-Regular.otf')
        if digest(font) != '68a3fc98800b2a27b371f2fb79991daf3633bd89309d4ffaa6946fd587f375b5':
            raise RuntimeError('official immutable font control differs')
        import shutil
        shutil.copyfile(font, context.path(font.name))
        for name in ('principal.png', 'principal.pdf', font.name):
            os.chown(context.path(name), context.uid, context.owner.pw_gid)
        monitor_identity = _spawn(context, 'portal', {})
        return {'png': {'sha256': digest(context.path('principal.png')), 'size': [480, 320],
                        'quadrants': colors},
                'pdf': {'sha256': digest(context.path('principal.pdf')), 'text': paragraph,
                        'link': endpoint['url']},
                'font': {'filename': font.name, 'sha256': digest(font)},
                'portal_monitor': monitor_identity,
                'web': {'prepared': prepared_web, 'endpoint': endpoint}}
    if operation in ('display-scale', 'default-handler', 'camera-permission', 'ime-engine'):
        if operation == 'default-handler':
            relative = str(Path(arguments['path']).relative_to(context.fixture))
            context.path(relative)
        result = context.user(['/usr/bin/python', str(MODULE), '--user-native', operation,
                               json.dumps(arguments, separators=(',', ':'))])
        observed = json.loads(result.stdout)
        if operation == 'display-scale' and observed['service_identity'] != context.identity()['compositor']:
            raise RuntimeError('DisplayConfig owner is not original native compositor')
        if operation == 'ime-engine':
            observed['package'] = context.command(['pacman', '-Q', 'ibus-libpinyin']).stdout.strip()
        if operation == 'ime-engine' and arguments['select']:
            engines = []
            for item in Path('/proc').iterdir():
                if not item.name.isdigit():
                    continue
                try:
                    if item.stat().st_uid == context.uid and (item / 'exe').resolve().name == 'ibus-engine-libpinyin':
                        engines.append(process(int(item.name), context.uid))
                except FileNotFoundError:
                    continue
            if len(engines) != 1:
                raise RuntimeError('genuine selected libpinyin engine process not unique')
            package = context.command(['pacman', '-Qoq', engines[0]['exe']]).stdout.strip()
            if package != 'ibus-libpinyin':
                raise RuntimeError('selected linguistic engine executable not owned by installed libpinyin package')
            observed['engine_process'] = engines[0]
        return observed
    if operation == 'camera-response':
        actor = arguments['actor']
        requests = [row for row in _rows(PORTAL_LOG) if row.get('member') == 'AccessDialog'
                    and row['body'][0] == arguments['request_path'] and row['body'][1] == actor['id']]
        if len(requests) != 1:
            raise RuntimeError('original Camera Request/backend handle not unique')
        responses = [row for row in _rows(PORTAL_LOG) if row.get('member') == 'Response'
                     and row['path'] == arguments['request_path']]
        if not responses:
            return {'ready': False, 'reason': 'original-camera-response-not-observed'}
        if len(responses) != 1 or responses[0]['sender_identity']['exe'] != '/usr/lib/xdg-desktop-portal':
            raise RuntimeError('actual original Camera Request response/owner not unique')
        return {'ready': True, 'request': requests[0], 'response': responses[0],
                'response_code': responses[0]['body'][0]}
    if operation == 'app-input':
        from guest import desktop
        actor = arguments['actor']
        actual = process(actor['pid'], context.uid)
        if actual != {key: actor[key] for key in actual}:
            raise RuntimeError('original application changed during delivery observation')
        state = desktop('state', {}, context)
        observer = state['keyboard_delivery_observer']
        if observer != {'enabled': True, 'evaluable': True, 'error': None}:
            raise RuntimeError('actual native keyboard emission observer unevaluable')
        windows = [row for row in state['native_keyboard_windows'] if row['window_id'] == actor['window_id']]
        if len(windows) != 1:
            raise RuntimeError('original native surface delivery observation absent/retired')
        window = windows[0]
        if (window['evaluable'] is not True or window['error'] is not None
                or window['client_pid'] != actor['pid'] or window['client_uid'] != context.uid
                or type(window['connection_generation']) is not int or not window['resources']):
            raise RuntimeError('actual native client credentials/bindings unevaluable')
        phase = arguments['phase']
        if phase not in ('unlocked-focused', 'locked', 'postunlock'):
            raise ValueError('explicit keyboard observation phase required')
        if state['locked'] != (phase == 'locked'):
            raise RuntimeError('actual desktop lock state differs from observation phase')
        if phase == 'unlocked-focused' and window['actual_keyboard_focus'] is not True:
            raise RuntimeError('actual seat focus does not belong to the original native app')
        if phase == 'locked' and window['actual_keyboard_focus'] is not False:
            raise RuntimeError('ordinary app retained actual seat keyboard focus under lock')
        generations = [row['binding_generation'] for row in window['resources']]
        if (type(window['connection_generation']) is not int or window['connection_generation'] <= 0
                or any(type(value) is not int or value <= 0 for value in generations)
                or len(set(generations)) != len(generations)
                or not all(type(row['emitted_key_events']) is int
                           and 0 <= row['emitted_key_events'] < 2 ** 64 - 1
                           for row in window['resources'])):
            raise RuntimeError('original actual keyboard binding identity/counter unevaluable')
        return {'actor': actual, 'window_id': actor['window_id'],
                'surface_object_id': window['surface_object_id'],
                'client_pid': window['client_pid'], 'client_uid': window['client_uid'],
                'connection_id': window['connection_id'],
                'connection_generation': window['connection_generation'],
                'resources': window['resources'], 'scope': 'server-emission-not-app-consumption'}
    if operation == 'departed':
        actor = arguments['actor']
        try:
            fields = Path(f"/proc/{actor['pid']}/stat").read_text().rsplit(')', 1)[1].split()
        except FileNotFoundError:
            return {'original_process_departed': True, 'pid_reused': False}
        return {'original_process_departed': fields[0] == 'Z' or int(fields[19]) != actor['start_ticks'],
                'pid_reused': int(fields[19]) != actor['start_ticks']}
    if operation in ('picker', 'portal'):
        actor = arguments['actor']
        current = process(actor['pid'], context.uid)
        if current != {key: actor[key] for key in current}:
            raise RuntimeError('original dialog requester changed')
        from guest import desktop
        state = desktop('state', {}, context)
        if operation == 'picker':
            windows = [row for row in state['windows'] if row['parent_window_id'] == actor['window_id']
                       and row['id'] == state['focused'] and row['app_id'] == 'org.gnome.Nautilus']
            if not windows:
                return {'ready': False, 'reason': 'original-parented-file-picker-not-mapped'}
            if len(windows) != 1:
                raise RuntimeError('actual parented native picker not unique')
            value = context.actor({'app_id': 'org.gnome.Nautilus'}, 'discover')
            actual = process(value['pid'], context.uid)
            metadata = context.read_control('inventory')['apps']['org.gnome.Nautilus']
            picker = actual | metadata | {'id': 'org.gnome.Nautilus', 'kind': 'picker',
                                          'window_id': windows[0]['id'], 'caller_app_id': actor['id'],
                                          'original_requester': actor}
        else:
            calls = [row for row in _rows(PORTAL_LOG) if row.get('member') == 'AccessCamera'
                     and row['sender_identity']['pid'] == actor['pid']
                     and row['sender_identity']['start_ticks'] == actor['start_ticks']]
            if not calls:
                raise RuntimeError('no actual original-app Camera portal method call observed')
            request = calls[-1]
            accesses = [row for row in _rows(PORTAL_LOG) if row.get('member') == 'AccessDialog'
                        and row['body'][0].startswith('/org/freedesktop/portal/desktop/request/')
                        and row['body'][1] == actor['id']]
            if not accesses:
                raise RuntimeError('no real Camera backend AccessDialog for original app')
            backend = accesses[-1]
            candidates = []
            for item in Path('/proc').iterdir():
                if not item.name.isdigit():
                    continue
                try:
                    if item.stat().st_uid != context.uid or (item / 'exe').resolve().name != 'xdg-desktop-portal-gtk':
                        continue
                    candidates.append(process(int(item.name), context.uid))
                except FileNotFoundError:
                    continue
            if len(candidates) != 1:
                raise RuntimeError('real native camera consent provider not unique')
            windows = [row for row in state['windows'] if row['id'] == state['focused']]
            if len(windows) != 1:
                raise RuntimeError('actual camera consent surface not focused')
            picker = candidates[0] | {'id': 'xdg-desktop-portal-gtk', 'kind': 'portal',
                                      'window_id': windows[0]['id'], 'original_requester': actor,
                                      'camera_request': request, 'backend_request': backend}
        peers = [row for row in state['native_keyboard_windows'] if row['window_id'] == picker['window_id']]
        if (len(peers) != 1 or peers[0]['client_pid'] != picker['pid']
                or peers[0]['client_uid'] != context.uid or peers[0]['actual_keyboard_focus'] is not True):
            raise RuntimeError('actual native dialog seat peer does not match original provider')
        observed = context.actor({'actor': picker})
        frames = [node for node in observed['nodes'] if node['role'] in ('frame', 'dialog', 'file chooser')
                  and node['showing'] and node['active']]
        if len(frames) != 1:
            raise RuntimeError('actual focused native dialog accessibility frame not unique')
        picker['a11y_bus'], picker['a11y_scope'] = observed['a11y_bus'], frames[0]['object_path']
        return picker
    if operation == 'shell-origin':
        from guest import desktop
        observed = context.actor({'actor': arguments['actor']})
        nodes = [node for node in observed['nodes'] if node['path'] == arguments['path']]
        if len(nodes) != 1:
            raise RuntimeError('original shell control path changed')
        frames = [row['name'] for row in nodes[0]['ancestors'] if row['role'] in ('frame', 'dialog')]
        namespaces = {'Location access': 'tuna-location-consent', 'Top Bar': 'tuna-panel',
                      'Input Method Candidates': 'tuna-ibus-candidates'}
        if not frames or frames[-1] not in namespaces:
            raise RuntimeError('actual shell layer coordinate contract unavailable for this frame')
        state = desktop('state', {}, context)
        layers = [row for row in state['layers'] if row['namespace'] == namespaces[frames[-1]]]
        if len(layers) != 1:
            raise RuntimeError('original shell control layer not unique')
        return {'x': layers[0]['rect'][0], 'y': layers[0]['rect'][1], 'layer': layers[0]}
    if operation == 'ime-monitor-start':
        if IME_LOG.exists():
            previous = json.loads(RUN_STATE.read_text()).get('ime_retired')
            if previous is not True:
                raise RuntimeError('previous real IBus monitor not retired')
            IME_LOG.unlink()
        state = json.loads(RUN_STATE.read_text())
        state['ime_retired'] = False
        RUN_STATE.write_text(json.dumps(state))
        return _spawn(context, 'ime', arguments)
    if operation == 'ime-monitor-stop':
        result = _stop(context, 'ime')
        state = json.loads(RUN_STATE.read_text())
        state['ime_retired'] = True
        RUN_STATE.write_text(json.dumps(state))
        return result
    if operation == 'ime-state':
        actor = arguments['actor']
        process(actor['pid'], context.uid)
        log_rows = _rows(IME_LOG)
        messages = [row for row in log_rows if row.get('member') in
                    ('UpdatePreeditText', 'UpdatePreeditTextWithMode')]
        summary = {'rows': len(log_rows), 'preedit_rows': len(messages),
                   'observer_errors': len([row for row in log_rows if 'observer_error_type' in row])}
        latest = messages[-1] if messages else None
        visible = (latest is not None and len(latest['body']) >= 3 and latest['body'][2] is True
                   and isinstance(latest['body'][0], list) and len(latest['body'][0]) >= 3
                   and latest['body'][0][0] == 'IBusText' and bool(latest['body'][0][2]))
        if latest is not None and latest['sender_identity']['exe'] != '/usr/bin/ibus-daemon':
            raise RuntimeError('observed preedit is not from the actual original-user IBus daemon')
        from guest import desktop
        shell = desktop('shell-actor', {}, context)
        tree = desktop('tree', {'actor': shell}, context)
        state = desktop('state', {}, context)
        focused = [row for row in state['native_keyboard_windows'] if row['window_id'] == actor['window_id']]
        if (len(focused) != 1 or focused[0]['client_pid'] != actor['pid']
                or focused[0]['client_uid'] != context.uid or focused[0]['actual_keyboard_focus'] is not True):
            raise RuntimeError('real preedit native seat focus belongs to another client')
        candidates = [node['name'] for node in tree['nodes'] if node['showing']
                      and any(parent['name'] == 'Input Method Candidates' for parent in node['ancestors'])]
        return {'preedit_visible': visible,
                'preedit': latest['body'][0][2] if visible else None,
                'real_preedit_message': latest,
                'ime_log_summary': summary,
                'candidates': candidates, 'shell_actor': shell, 'actual_shell_tree': tree,
                'actor': actor}
    if operation == 'lock-ui':
        from guest import desktop
        state = desktop('state', {}, context)
        if state['locked'] is not True:
            raise RuntimeError('private lock UI readiness requested outside actual lock')
        actor = desktop('shell-actor', {}, context)
        tree = context.actor({'actor': actor})
        passwords = [node for node in tree['nodes'] if node['showing'] and node['sensitive']
                     and 'password' in node['role']]
        if len(passwords) > 1:
            raise RuntimeError('original actual lock password control not unique')
        # No text, title, field contents or private accessibility tree leaves
        # this root-only readiness action.
        return {'locked': True, 'shell_identity': actor,
                'password_field_ready': len(passwords) == 1 and passwords[0]['focused']}
    if operation == 'retire-frontends':
        from guest import APP_PACKAGES
        actors = arguments['actors']
        if not isinstance(actors, list) or len(actors) > len(APP_PACKAGES):
            raise ValueError('bounded originally admitted frontend cleanup required')
        pending, retired = [], []
        for actor in actors:
            if actor['kind'] != 'app' or APP_PACKAGES.get(actor['id']) != actor['package']:
                raise RuntimeError('cleanup may retire only originally admitted Core frontends')
            if dispatch('departed', {'actor': actor}, context)['original_process_departed']:
                retired.append({'actor': actor, 'already_departed': True})
                continue
            actual = process(actor['pid'], context.uid)
            if actual != {key: actor[key] for key in actual}:
                raise RuntimeError('frontend cleanup original PID/start/ELF identity changed')
            context.user(['kill', '--signal', 'TERM', '--', str(actual['pid'])])
            pending.append(actor)
        deadline = time.monotonic() + 3
        while pending:
            live = []
            for actor in pending:
                if dispatch('departed', {'actor': actor}, context)['original_process_departed']:
                    retired.append({'actor': actor, 'same_uid_sigterm_departed': True})
                else:
                    live.append(actor)
            pending = live
            if pending and time.monotonic() >= deadline:
                raise RuntimeError('original frontend failure cleanup did not retire within its deadline')
            if pending:
                time.sleep(0.05)
        return {'frontends': retired, 'scope': 'failure-cleanup-not-app-operation-proof'}
    if operation == 'finish':
        state = json.loads(RUN_STATE.read_text())
        result = {}
        if state.get('ime_monitor') and not state.get('ime_retired'):
            result['ime'] = _stop(context, 'ime')
        result['portal'] = _stop(context, 'portal')
        return result
    raise ValueError('unknown typed native observation')
