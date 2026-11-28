"""Host Files-cell matching regressions; no VM qualification."""
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest import mock
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'scripts' / 'lib'))
import core_apps


class OpenDefaultCellSpellings(unittest.TestCase):
    def desktop(self, nodes):
        tree = {'nodes': nodes}
        calls = {}
        desktop = core_apps.Desktop.__new__(core_apps.Desktop)
        desktop._actors = {}
        desktop.guest = lambda domain, operation, **k: (
            {'pid': 9} if operation == 'discover'
            else {'default_handler': 'org.gnome.font-viewer.desktop'})
        desktop.start = lambda *a: {'pid': 1}
        desktop.keys = lambda *a: None
        desktop.type_ascii = lambda *a: None
        desktop.enter_location = lambda *a, **k: None
        desktop.tree = lambda actor: tree

        def click_path(actor, path):
            calls['click'] = path
            for node in nodes:
                if node['path'] == path:
                    node['selected'] = True
        desktop.click_path = click_path
        desktop.observe = lambda *a: {}

        def await_state(label, read, predicate, timeout=10):
            value = read()
            if predicate(value):
                return value
            raise TimeoutError('not ready: ' + label)
        desktop.await_state = await_state
        return desktop, calls

    def test_suffixed_file_cell_opens(self):
        nodes = [{'showing': True, 'name': 'NotoSansCJKjp-Regular.otf. File',
                  'role': 'table cell', 'path': ['row', 1], 'selected': False}]
        desktop, calls = self.desktop(nodes)
        result = desktop.open_default('/fixture/NotoSansCJKjp-Regular.otf')
        self.assertEqual(calls['click'], ['row', 1])
        self.assertEqual(result['actor']['pid'], 9)

    def test_bare_cell_name_still_opens(self):
        nodes = [{'showing': True, 'name': 'seed.txt', 'role': 'list item',
                  'path': ['row', 0], 'selected': False}]
        desktop, calls = self.desktop(nodes)
        desktop.open_default('/fixture/seed.txt')
        self.assertEqual(calls['click'], ['row', 0])

    def test_suffixed_folder_cell_opens(self):
        nodes = [{'showing': True, 'name': 'web. Folder', 'role': 'table cell',
                  'path': ['row', 2], 'selected': False}]
        desktop, calls = self.desktop(nodes)
        desktop.open_default('/fixture/web')
        self.assertEqual(calls['click'], ['row', 2])


class FakeEngine:
    def __init__(self, name):
        self._name = name

    def get_name(self):
        return self._name

    def get_language(self):
        return 'en'

    def get_longname(self):
        return self._name


class FakeBus:
    def __init__(self, engines, accept=True):
        self._engines = list(engines)
        self._accept = accept
        self.selected = []

    def is_connected(self):
        return True

    def get_global_engine(self):
        return self._engines[-1] if self._engines else None

    def set_global_engine(self, name):
        self.selected.append(name)
        if self._accept:
            self._engines.append(FakeEngine(name))
        return self._accept


class ImeEngineBaseline(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        lane = ROOT / 'packaging' / 'marlin' / 'vm-lane'
        sys.path.insert(0, str(lane / 'core-apps'))
        sys.path.insert(0, str(lane / 'tuna-native-session'))
        try:
            import native
            cls.native = native
        finally:
            sys.path.remove(str(lane / 'core-apps'))
            sys.path.remove(str(lane / 'tuna-native-session'))
            sys.modules.pop('native', None)
            sys.modules.pop('guest', None)
            sys.modules.pop('native_session', None)

    def operate(self, bus):
        import types
        repository = types.SimpleNamespace(
            Gio=types.SimpleNamespace(BusType=types.SimpleNamespace(SESSION=1),
                                      bus_get_sync=lambda *a: object()),
            GLib=types.SimpleNamespace(),
            IBus=types.SimpleNamespace(init=lambda: None,
                                       Bus=types.SimpleNamespace(new=lambda: bus)))
        gi = types.SimpleNamespace(require_version=lambda *a: None, repository=repository)
        saved = {name: sys.modules.get(name) for name in ('gi', 'gi.repository')}
        sys.modules['gi'] = gi
        sys.modules['gi.repository'] = repository
        try:
            return self.native.user_operation('ime-engine', {'select': True})
        finally:
            for name, module in saved.items():
                if module is None:
                    sys.modules.pop(name, None)
                else:
                    sys.modules[name] = module

    def test_engineless_session_establishes_xkb_baseline(self):
        bus = FakeBus([])
        observed = self.operate(bus)
        self.assertEqual(bus.selected, ['xkb:us::eng', 'libpinyin'])
        self.assertEqual(observed['original'], 'xkb:us::eng')
        self.assertEqual(observed['actual'], 'libpinyin')

    def test_existing_engine_is_kept_as_original(self):
        bus = FakeBus([FakeEngine('xkb:us::eng')])
        observed = self.operate(bus)
        self.assertEqual(bus.selected, ['libpinyin'])
        self.assertEqual(observed['original'], 'xkb:us::eng')

    def test_rejected_baseline_still_fails(self):
        bus = FakeBus([], accept=False)
        with self.assertRaisesRegex(RuntimeError, 'rejected xkb baseline'):
            self.operate(bus)


class FakeNameOwner:
    def __init__(self, name):
        self._name = name

    def unpack(self):
        return [self._name]


class FakeMonitorConnection:
    """Records subscription calls; optionally rejects BecomeMonitor like ibus-daemon."""

    def __init__(self, reject_monitor=False):
        self.calls = []
        self._reject_monitor = reject_monitor

    def add_filter(self, *args):
        pass

    def call_sync(self, destination, path, interface, method, parameters, *args):
        self.calls.append((interface, method, parameters))
        if method == 'BecomeMonitor' and self._reject_monitor:
            raise RuntimeError('GDBus.Error:org.freedesktop.DBus.Error.UnknownMethod: '
                               'No such interface "org.freedesktop.DBus.Monitoring"')
        if method == 'GetNameOwner':
            return FakeNameOwner(':1.9')
        return None


class ImeMonitorSubscription(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        lane = ROOT / 'packaging' / 'marlin' / 'vm-lane'
        sys.path.insert(0, str(lane / 'core-apps'))
        sys.path.insert(0, str(lane / 'tuna-native-session'))
        try:
            import native
            cls.native = native
        finally:
            sys.path.remove(str(lane / 'core-apps'))
            sys.path.remove(str(lane / 'tuna-native-session'))
            sys.modules.pop('native', None)
            sys.modules.pop('guest', None)
            sys.modules.pop('native_session', None)

    def observe(self, kind, reject_monitor=False):
        import contextlib
        import io
        import types
        from unittest import mock
        connections = []

        def connect(address, flags, cancellable, callback):
            connection = FakeMonitorConnection(reject_monitor=reject_monitor)
            connections.append(connection)
            return connection

        repository = types.SimpleNamespace(
            Gio=types.SimpleNamespace(
                DBusConnection=types.SimpleNamespace(new_for_address_sync=connect),
                DBusConnectionFlags=types.SimpleNamespace(AUTHENTICATION_CLIENT=1,
                                                          MESSAGE_BUS_CONNECTION=2),
                DBusCallFlags=types.SimpleNamespace(NONE=0)),
            GLib=types.SimpleNamespace(Variant=lambda *args: args,
                                       VariantType=types.SimpleNamespace(new=lambda *args: args),
                                       MainLoop=lambda: types.SimpleNamespace(run=lambda: None)))
        gi = types.SimpleNamespace(require_version=lambda *args: None, repository=repository)
        saved = {name: sys.modules.get(name) for name in ('gi', 'gi.repository')}
        sys.modules['gi'] = gi
        sys.modules['gi.repository'] = repository
        output = io.StringIO()
        try:
            with contextlib.redirect_stdout(output):
                with mock.patch.dict(os.environ, {'DBUS_SESSION_BUS_ADDRESS': 'unix:path=/fixture/bus'}):
                    with mock.patch.object(os, 'geteuid', return_value=1000):
                        with mock.patch.object(self.native.subprocess, 'check_output',
                                               return_value='unix:path=/fixture/ibus\n'):
                            self.native.monitor(kind, {'actor': {'pid': 9}})
        finally:
            for name, module in saved.items():
                if module is None:
                    sys.modules.pop(name, None)
                else:
                    sys.modules[name] = module
        return connections, json.loads(output.getvalue())

    def test_ime_monitor_eavesdrops_without_become_monitor(self):
        connections, ready = self.observe('ime', reject_monitor=True)
        self.assertEqual(ready['ready'], True)
        self.assertEqual(ready['kind'], 'ime')
        self.assertEqual(ready['ibus_owner'], ':1.9')
        self.assertEqual(ready['address'], 'unix:path=/fixture/ibus')
        # One channel: the IBus bus answers no credential queries.
        self.assertEqual(len(connections), 1)
        calls = connections[0].calls
        self.assertEqual([method for _, method, _ in calls],
                         ['AddMatch', 'AddMatch', 'GetNameOwner'])
        self.assertNotIn('GetConnectionUnixProcessID', [method for _, method, _ in calls])
        rules = [parameters[1][0] for _, _, parameters in calls[:2]]
        self.assertTrue(all("eavesdrop='true'" in rule for rule in rules))
        self.assertTrue(any("member='UpdatePreeditText'" in rule for rule in rules))
        self.assertTrue(any("member='UpdatePreeditTextWithMode'" in rule for rule in rules))
        owner_call = calls[2]
        self.assertEqual(owner_call[0], 'org.freedesktop.DBus')
        self.assertEqual(owner_call[2][1], ('org.freedesktop.IBus',))

    def test_portal_monitor_keeps_become_monitor(self):
        connections, ready = self.observe('portal')
        self.assertEqual(ready['ready'], True)
        self.assertEqual(ready['kind'], 'portal')
        self.assertNotIn('ibus_owner', ready)
        # Portal keeps its second credential channel on the session bus.
        self.assertEqual(len(connections), 2)
        calls = connections[0].calls
        self.assertEqual(len(calls), 1)
        interface, method, parameters = calls[0]
        self.assertEqual(interface, 'org.freedesktop.DBus.Monitoring')
        self.assertEqual(method, 'BecomeMonitor')
        self.assertEqual(parameters[0], '(asu)')

    def test_ime_summary_relays_distinct_bounded_error_details(self):
        rows = [{'observer_error_type': 'RuntimeError', 'observer_error_detail': 'b-detail'},
                {'observer_error_type': 'RuntimeError', 'observer_error_detail': 'a-detail'},
                {'observer_error_type': 'RuntimeError', 'observer_error_detail': 'b-detail'},
                {'observer_error_type': 'RuntimeError'},
                {'member': 'UpdatePreeditText'}]
        summary = self.native._ime_summary(rows, [rows[-1]])
        self.assertEqual(summary['rows'], 5)
        self.assertEqual(summary['preedit_rows'], 1)
        self.assertEqual(summary['observer_errors'], 4)
        self.assertEqual(summary['observer_error_details'], ['a-detail', 'b-detail'])
        long = self.native._ime_summary(
            [{'observer_error_type': 'E', 'observer_error_detail': 'z' * 500}], [])
        self.assertEqual(long['observer_error_details'], ['z' * 200])


class ImePreeditBinding(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        lane = ROOT / 'packaging' / 'marlin' / 'vm-lane'
        sys.path.insert(0, str(lane / 'core-apps'))
        sys.path.insert(0, str(lane / 'tuna-native-session'))
        try:
            import native
            cls.native = native
        finally:
            sys.path.remove(str(lane / 'core-apps'))
            sys.path.remove(str(lane / 'tuna-native-session'))
            sys.modules.pop('native', None)
            sys.modules.pop('guest', None)
            sys.modules.pop('native_session', None)

    def test_matching_sender_passes(self):
        self.native.check_preedit_sender({'sender': ':1.9'}, ':1.9')

    def test_foreign_sender_raises_typed_error(self):
        with self.assertRaises(self.native.ForeignPreeditSender) as failed:
            self.native.check_preedit_sender({'sender': ':1.50'}, ':1.9')
        self.assertIn("':1.50'", str(failed.exception))

    def test_missing_sender_raises_typed_error(self):
        with self.assertRaises(self.native.ForeignPreeditSender):
            self.native.check_preedit_sender({}, ':1.9')

    def test_unanimous_or_empty_destinations_pass(self):
        self.native.check_preedit_destinations([])
        self.native.check_preedit_destinations([{'destination': ':1.50'}, {'destination': ':1.50'}])

    def test_split_destinations_raise(self):
        with self.assertRaisesRegex(RuntimeError, 'multiple simultaneous IBus composers'):
            self.native.check_preedit_destinations([{'destination': ':1.50'},
                                                    {'destination': ':1.51'}])

    def test_binding_requires_unique_ready_with_owner(self):
        ready = {'ready': True, 'kind': 'ime', 'uid': 1000, 'ibus_owner': ':1.9'}
        self.assertEqual(self.native.ibus_owner_binding([ready]), ':1.9')
        with self.assertRaisesRegex(RuntimeError, 'admission is not unique'):
            self.native.ibus_owner_binding([])
        with self.assertRaisesRegex(RuntimeError, 'admission is not unique'):
            self.native.ibus_owner_binding([ready, ready])
        for bad in (':bogus', '', None, 42):
            row = dict(ready, ibus_owner=bad)
            with self.subTest(owner=bad), self.assertRaisesRegex(
                    RuntimeError, 'lacks daemon name binding'):
                self.native.ibus_owner_binding([row])


class ImeDaemonAdmission(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        lane = ROOT / 'packaging' / 'marlin' / 'vm-lane'
        sys.path.insert(0, str(lane / 'core-apps'))
        sys.path.insert(0, str(lane / 'tuna-native-session'))
        try:
            import native
            cls.native = native
        finally:
            sys.path.remove(str(lane / 'core-apps'))
            sys.path.remove(str(lane / 'tuna-native-session'))
            sys.modules.pop('native', None)
            sys.modules.pop('guest', None)
            sys.modules.pop('native_session', None)

    def admit(self, pids, identities, package='ibus\n', basename_pids=None):
        import types
        from unittest import mock
        context = types.SimpleNamespace(
            uid=1000, command=mock.Mock(return_value=types.SimpleNamespace(stdout=package)))
        by_pid = {identity['pid']: identity for identity in identities}
        with mock.patch.object(self.native, '_proc_exe_pids', return_value=pids), \
                mock.patch.object(self.native, '_proc_basename_pids',
                                  return_value=basename_pids if basename_pids is not None else []), \
                mock.patch.object(self.native, 'process',
                                  side_effect=lambda pid, uid: by_pid[pid]):
            try:
                return context, self.native.admit_ibus_daemons(context), None
            except RuntimeError as error:
                return context, None, error

    def identity(self, pid=4321, exe='/usr/bin/ibus-daemon'):
        return {'pid': pid, 'uid': 1000, 'start_ticks': 7,
                'exe': exe, 'exe_sha256': 'ab' * 32,
                'cgroup': ['0::/user.slice']}

    def test_unique_packaged_daemon_admits(self):
        context, daemons, error = self.admit([4321], [self.identity()])
        self.assertIsNone(error)
        self.assertEqual(len(daemons), 1)
        self.assertEqual(daemons[0]['package'], 'ibus')
        self.assertEqual(daemons[0]['pid'], 4321)
        context.command.assert_called_once_with(['pacman', '-Qqo', '/usr/bin/ibus-daemon'])

    def test_duplicate_daemons_refused(self):
        # Two genuine packaged daemons are the #1014 login race: one owner only.
        context, daemons, error = self.admit([4322, 4321],
                                             [self.identity(4321), self.identity(4322)])
        self.assertIsNone(daemons)
        self.assertIn('IBus daemon not unique: [4321, 4322]', str(error))
        context.command.assert_not_called()

    def test_duplicate_relocated_daemons_refused(self):
        _, daemons, error = self.admit([], [self.identity(4321, '/usr/libexec/ibus-daemon'),
                                            self.identity(4322, '/usr/libexec/ibus-daemon')],
                                       basename_pids=[4321, 4322])
        self.assertIsNone(daemons)
        self.assertIn('IBus daemon not unique', str(error))

    def test_missing_daemon_refused(self):
        _, _, error = self.admit([], [])
        self.assertIn('no original-user IBus daemon process', str(error))

    def test_basename_fallback_admits_relocated_daemon(self):
        identity = self.identity(exe='/usr/libexec/ibus-daemon')
        context, daemons, error = self.admit([], [identity], basename_pids=[4321])
        self.assertIsNone(error)
        self.assertEqual(daemons[0]['exe'], '/usr/libexec/ibus-daemon')
        context.command.assert_called_once_with(['pacman', '-Qqo', '/usr/libexec/ibus-daemon'])

    def test_foreign_package_refused(self):
        _, _, error = self.admit([4321], [self.identity()], package='not-ibus\n')
        self.assertIn('not owned by ibus package', str(error))


class ImeStateDispatch(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        lane = ROOT / 'packaging' / 'marlin' / 'vm-lane'
        sys.path.insert(0, str(lane / 'core-apps'))
        sys.path.insert(0, str(lane / 'tuna-native-session'))
        try:
            import native
            cls.native = native
        finally:
            sys.path.remove(str(lane / 'core-apps'))
            sys.path.remove(str(lane / 'tuna-native-session'))
            sys.modules.pop('native', None)
            sys.modules.pop('guest', None)
            sys.modules.pop('native_session', None)

    def daemon(self):
        return {'pid': 100, 'uid': 1000, 'start_ticks': 7,
                'exe': '/usr/bin/ibus-daemon', 'exe_sha256': 'ab' * 32,
                'cgroup': ['0::/user.slice']}

    def operate(self, rows, run_state, actor=None, observed=None):
        import types
        from unittest import mock
        actor = actor if actor is not None else {'pid': 300, 'window_id': 7}
        observed = observed if observed is not None else {}
        context = types.SimpleNamespace(uid=1000, environment=lambda: {})
        state = {'native_keyboard_windows': [{'window_id': 7, 'client_pid': 300,
                                              'client_uid': 1000, 'actual_keyboard_focus': True}]}
        tree = {'nodes': [{'showing': True, 'name': '你好',
                           'ancestors': [{'name': 'Input Method Candidates'}]},
                          {'showing': True, 'name': 'Search', 'role': 'text',
                           'path': [1, 2], 'focused': True, 'editable': True,
                           'text': '', 'ancestors': []}]}

        def desktop(operation, arguments, _context):
            return {'shell': True} if operation == 'shell-actor' else (
                tree if operation == 'tree' else state)

        def identify(pid, uid):
            if pid in observed and isinstance(observed[pid], BaseException):
                raise observed[pid]
            return observed.get(pid, {'pid': pid, 'uid': uid})

        guest = types.SimpleNamespace(desktop=desktop)
        engine = {'pid': 400, 'ppid': 100, 'bus_address': 'unix:path=/fixture/engine-bus'}
        app = {'pid': 300, 'ibus_address': 'unix:path=/fixture/app-bus', 'gtk_im_module': 'ibus'}
        with mock.patch.object(self.native, 'process', side_effect=identify), \
                mock.patch.object(self.native, '_rows', return_value=rows), \
                mock.patch.object(self.native, 'RUN_STATE') as run, \
                mock.patch.object(self.native, '_ibus_engine_wiring', return_value=engine), \
                mock.patch.object(self.native, '_ibus_app_wiring', return_value=app), \
                mock.patch.object(self.native, '_ppid', return_value=1), \
                mock.patch.object(self.native, '_cmdline',
                                  return_value='ibus-daemon --daemonize --xim'), \
                mock.patch.object(self.native.subprocess, 'check_output',
                                  return_value='unix:path=/fixture/file-bus\n'), \
                mock.patch.dict(sys.modules, {'guest': guest}):
            run.read_text.return_value = json.dumps(run_state)
            return self.native.dispatch('ime-state', {'actor': actor}, context)

    def admitted(self):
        monitor = {'pid': 200, 'uid': 1000}
        return {'ime_daemons': [self.daemon()], 'ime_monitor': monitor,
                'observed': {100: self.daemon(), 200: monitor}}

    def test_bound_preedit_observes_visible_text(self):
        admitted = self.admitted()
        rows = [{'ready': True, 'kind': 'ime', 'uid': 1000, 'ibus_owner': ':1.9'},
                {'member': 'UpdatePreeditText', 'sender': ':1.9', 'destination': ':1.50',
                 'body': [['IBusText', {}, 'nihao'], 0, True]}]
        response = self.operate(rows, admitted, observed=admitted.pop('observed'))
        self.assertTrue(response['preedit_visible'])
        self.assertEqual(response['preedit'], 'nihao')
        self.assertEqual(response['ibus_owner'], ':1.9')
        self.assertEqual(response['destination'], ':1.50')
        self.assertEqual(response['daemons'], [self.daemon()])
        self.assertEqual(response['candidates'], ['你好'])

    def test_wiring_binds_monitor_file_engine_and_daemon_parents(self):
        admitted = self.admitted()
        rows = [{'ready': True, 'kind': 'ime', 'uid': 1000, 'ibus_owner': ':1.9',
                 'address': 'unix:path=/fixture/monitor-bus'}]
        response = self.operate(rows, admitted, observed=admitted.pop('observed'))
        self.assertEqual(response['ibus_wiring'],
                         {'monitor_address': 'unix:path=/fixture/monitor-bus',
                          'file_address': 'unix:path=/fixture/file-bus',
                          'engine': {'pid': 400, 'ppid': 100,
                                     'bus_address': 'unix:path=/fixture/engine-bus'},
                          'app': {'pid': 300, 'ibus_address': 'unix:path=/fixture/app-bus',
                                  'gtk_im_module': 'ibus'},
                          'daemon_parents': {100: 1},
                          'daemon_cmdlines': {100: 'ibus-daemon --daemonize --xim'}})
        self.assertEqual(response['focused_editables'],
                         [{'path': [1, 2], 'name': 'Search', 'text': ''}])

    def test_engine_wiring_skips_unreadable_owned_processes(self):
        import pathlib
        import tempfile
        from unittest import mock
        root = pathlib.Path(tempfile.mkdtemp(prefix='ibus-proc-'))
        owner = os.geteuid()

        def process(pid, exe):
            entry = root / str(pid)
            entry.mkdir()
            entry.joinpath('exe').symlink_to(exe)
            fields = ' '.join(['S'] + [str(index) for index in range(1, 22)])
            entry.joinpath('stat').write_text(f'{pid} (proc) {fields}\n')
            entry.joinpath('environ').write_bytes(b'IBUS_ADDRESS=unix:path=/fixture/bus\0')

        process(4242, '/usr/lib/ibus-libpinyin/ibus-engine-libpinyin')
        process(5150, '/usr/bin/bwrap')
        real_readlink = os.readlink

        def readlink(path):
            if str(path).endswith('5150/exe'):
                raise PermissionError(13, 'Permission denied', str(path))
            return real_readlink(path)

        with mock.patch.object(os, 'readlink', side_effect=readlink):
            found = self.native._ibus_engine_wiring(owner, root)
        self.assertEqual(found, {'pid': 4242, 'ppid': 1,
                                 'bus_address': 'unix:path=/fixture/bus'})
        self.assertIsNone(self.native._ibus_engine_wiring(owner + 1, root))

    def test_app_wiring_reads_bus_and_im_module_or_nothing(self):
        import pathlib
        import tempfile
        root = pathlib.Path(tempfile.mkdtemp(prefix='ibus-app-proc-'))
        owner = os.geteuid()
        entry = root / '300'
        entry.mkdir()
        entry.joinpath('environ').write_bytes(
            b'GTK_IM_MODULE=ibus\0IBUS_ADDRESS=unix:path=/fixture/app-bus\0OTHER=1\0')
        self.assertEqual(self.native._ibus_app_wiring(300, owner, root),
                         {'pid': 300, 'ibus_address': 'unix:path=/fixture/app-bus',
                          'gtk_im_module': 'ibus'})
        self.assertIsNone(self.native._ibus_app_wiring(999, owner, root))
        self.assertIsNone(self.native._ibus_app_wiring(300, owner + 1, root))
        entry.joinpath('environ').write_bytes(b'OTHER=1\0')
        self.assertEqual(self.native._ibus_app_wiring(300, owner, root),
                         {'pid': 300, 'ibus_address': None, 'gtk_im_module': None})

    def test_cmdline_is_bounded_or_nothing(self):
        import pathlib
        import tempfile
        root = pathlib.Path(tempfile.mkdtemp(prefix='cmdline-proc-'))
        entry = root / '100'
        entry.mkdir()
        entry.joinpath('cmdline').write_bytes(b'ibus-daemon\0--daemonize\0--xim\0')
        self.assertEqual(self.native._cmdline(100, root), 'ibus-daemon --daemonize --xim')
        self.assertIsNone(self.native._cmdline(999, root))
        entry.joinpath('cmdline').write_bytes(b'x' * 600)
        self.assertEqual(len(self.native._cmdline(100, root)), 500)

    def test_ppid_reads_parent_or_nothing(self):
        import pathlib
        from unittest import mock
        self.assertEqual(self.native._ppid(os.getpid()), os.getppid())
        with mock.patch.object(pathlib.Path, 'read_text', side_effect=FileNotFoundError()):
            self.assertIsNone(self.native._ppid(os.getpid()))

    def test_departed_daemon_refused(self):
        admitted = self.admitted()
        observed = admitted.pop('observed')
        observed[100] = FileNotFoundError()
        rows = [{'ready': True, 'kind': 'ime', 'uid': 1000, 'ibus_owner': ':1.9'}]
        with self.assertRaisesRegex(RuntimeError, 'daemons changed or departed'):
            self.operate(rows, admitted, observed=observed)

    def test_changed_observer_refused(self):
        admitted = self.admitted()
        observed = admitted.pop('observed')
        observed[200] = {'pid': 200, 'uid': 1000, 'start_ticks': 'changed'}
        rows = [{'ready': True, 'kind': 'ime', 'uid': 1000, 'ibus_owner': ':1.9'}]
        with self.assertRaisesRegex(RuntimeError, 'observer departed or changed'):
            self.operate(rows, admitted, observed=observed)

    def test_foreign_sender_observed_is_loud(self):
        admitted = self.admitted()
        rows = [{'ready': True, 'kind': 'ime', 'uid': 1000, 'ibus_owner': ':1.9'},
                {'member': 'UpdatePreeditText', 'sender': ':1.77', 'destination': ':1.50',
                 'body': [['IBusText', {}, 'x'], 0, True]}]
        with self.assertRaisesRegex(RuntimeError, 'foreign IBus preedit sender observed'):
            self.operate(rows, admitted, observed=admitted.pop('observed'))

    def test_split_destinations_are_loud(self):
        admitted = self.admitted()
        rows = [{'ready': True, 'kind': 'ime', 'uid': 1000, 'ibus_owner': ':1.9'},
                {'member': 'UpdatePreeditText', 'sender': ':1.9', 'destination': ':1.50',
                 'body': [['IBusText', {}, 'x'], 0, True]},
                {'member': 'UpdatePreeditText', 'sender': ':1.9', 'destination': ':1.51',
                 'body': [['IBusText', {}, 'y'], 0, True]}]
        with self.assertRaisesRegex(RuntimeError, 'multiple simultaneous IBus composers'):
            self.operate(rows, admitted, observed=admitted.pop('observed'))

    def test_monitor_start_admits_daemons_and_strips_package_for_liveness(self):
        import types
        from unittest import mock
        daemons = [dict(self.daemon(), package='ibus')]
        monitor = {'pid': 200, 'uid': 1000}
        context = types.SimpleNamespace(uid=1000)
        with mock.patch.object(self.native, 'admit_ibus_daemons', return_value=daemons), \
                mock.patch.object(self.native, '_spawn', return_value=monitor) as spawned, \
                mock.patch.object(self.native, 'RUN_STATE') as run, \
                mock.patch.object(self.native, 'IME_LOG') as log:
            log.exists.return_value = False
            run.read_text.return_value = '{}'
            response = self.native.dispatch('ime-monitor-start', {'actor': {'pid': 300}},
                                            context)
        spawned.assert_called_once()
        self.assertEqual(response, {'monitor': monitor, 'daemons': daemons})
        written = json.loads(run.write_text.call_args[0][0])
        self.assertFalse(written['ime_retired'])
        # RUN_STATE keeps pure process identity so per-observation
        # liveness compares equal; the package rides the response only.
        self.assertNotIn('package', written['ime_daemons'][0])
        self.assertEqual(written['ime_daemons'], [self.daemon()])


class HostImeOcrProvisioning(unittest.TestCase):
    """Every native Core leg's host OCR must stay provisioned in CI.

    Desktop.ime() shells out to host tesseract with `-l eng+chi_sim` to
    verify the rendered preedit field; without the runner packages a leg
    fails as FileNotFoundError with zero completed apps.
    """
    def test_core_apps_leg_installs_host_ocr(self):
        import yaml
        workflow = yaml.safe_load((ROOT / '.github/workflows/ci.yml').read_text())
        job = workflow['jobs']['core-apps-leg']
        # Every leg is one member of this job's matrix and shares its steps.
        self.assertNotIn('if', next(step for step in job['steps'] if 'tuna-vm-lane' in step.get('run', '')))
        ocr = [step for step in job['steps']
               if 'if' not in step and 'apt-get install' in step.get('run', '')
               and 'tesseract-ocr ' in step['run']]
        self.assertEqual(len(ocr), 1)
        self.assertIn('tesseract-ocr-chi-sim', ocr[0]['run'])


class ImeFieldFocusSettle(unittest.TestCase):
    """ime() settles the exact editable field before selecting or typing.

    Keys typed before focus lands never reach the engine: the monitor
    stays idle (one row, zero preedit rows) while the preedit await burns
    the budget (run 41865506509 attempts 1-2).
    """
    def rig(self, focused=False, web=False, text=''):
        import base64
        import tempfile
        from unittest import mock
        import types
        order = []
        field = {'path': [0, 0, 1] if web else [1], 'name': 'Search', 'text': text, 'role': 'text',
                 'editable': True, 'showing': True, 'sensitive': True, 'focused': focused}
        # A web view hosting a document, as Epiphany's (run 45936499585).
        page = [{'path': [0], 'role': 'panel', 'bounds': [0, 46, 100, 50]},
                {'path': [0, 0], 'role': 'document web', 'bounds': [0, 0, 100, 50]}] if web else []
        actor = {'id': 'org.example.app', 'kind': 'app'}
        desktop = core_apps.Desktop.__new__(core_apps.Desktop)
        desktop._ime = {}
        desktop._sequence = 41
        desktop.out = Path(tempfile.mkdtemp(prefix='ime-focus-'))
        desktop.select = lambda target: None
        desktop.tree = lambda target: {'nodes': page + [dict(field)], 'state': {}}
        desktop.observe = lambda label, target: {'nodes': page + [dict(field)], 'state': {},
                                                 'frame_path': str(desktop.out / 'frame.png')}
        desktop._field_pixels = lambda *args: (0, 0, 8, 8)
        desktop.clipboard = lambda: {'base64': base64.b64encode('你好'.encode()).decode()}

        def guest(domain, operation, **kwargs):
            order.append(operation)
            return {'original': 'xkb:us::eng'}

        def await_state(label, read, predicate, timeout=10):
            order.append(label)
            if label == 'real-ime-field-focused':
                value = read()
                assert predicate(value), 'rigged field must satisfy the focus predicate'
                return value
            if label == 'real-native-libpinyin-preedit':
                return {'preedit': '你好'}
            return {'nodes': [{'path': [1], 'focused': True, 'text': '你好'}]}

        desktop.guest = guest
        desktop.await_state = await_state

        def click(target, path):
            order.append('click')
            field['focused'] = True
        desktop.click_path = click
        desktop.keys = lambda *chords: order.append(('keys',) + chords)
        desktop.type_ascii = lambda text: order.append(('type', text))
        pil = types.ModuleType('PIL')
        pil.Image = mock.MagicMock()
        completed = mock.Mock(stdout='你好')
        return desktop, actor, field, order, pil, completed

    def test_typing_waits_for_exact_focused_editable_field(self):
        import sys
        from unittest import mock
        desktop, actor, field, order, pil, completed = self.rig()
        with mock.patch.dict(sys.modules, {'PIL': pil}), \
             mock.patch.object(core_apps.subprocess, 'run', return_value=completed) as ocr:
            evidence = desktop.ime(actor, field['name'], field_path=field['path'])
        self.assertEqual(evidence['preedit_ocr'], '你好')
        self.assertEqual(evidence['actual_input'], 'physical-nihao-space')
        legend = ['click' if entry == 'click' else entry[0] if isinstance(entry, tuple) else entry
                  for entry in order]
        self.assertEqual(legend[:6], ['ime-engine', 'click', 'real-ime-field-focused',
                                     'keys', 'ime-monitor-start', 'type'])
        ocr.assert_called_once()
        self.assertIn('-l', ocr.call_args.args[0])

    def test_a_field_already_holding_focus_is_not_clicked(self):
        """Run 44850865831: after the guest clipboard read, clicking Epiphany's
        still-focused entry ended its focus; a focused field is typed into as is."""
        import sys
        from unittest import mock
        desktop, actor, field, order, pil, completed = self.rig(focused=True)
        with mock.patch.dict(sys.modules, {'PIL': pil}), \
             mock.patch.object(core_apps.subprocess, 'run', return_value=completed):
            desktop.ime(actor, field['name'], field_path=field['path'])
        legend = [entry[0] if isinstance(entry, tuple) else entry for entry in order]
        self.assertNotIn('click', legend)
        self.assertEqual(legend[:5], ['ime-engine', 'real-ime-field-focused',
                                     'keys', 'ime-monitor-start', 'type'])

    def test_focus_predicate_rejects_wrong_unfocused_and_readonly_fields(self):
        from unittest import mock
        desktop, actor, field, order, pil, completed = self.rig()
        predicates = {}
        real_await = desktop.await_state

        def capture(label, read, predicate, timeout=10):
            predicates[label] = predicate
            return real_await(label, read, predicate, timeout)

        desktop.await_state = capture
        import sys
        with mock.patch.dict(sys.modules, {'PIL': pil}), \
             mock.patch.object(core_apps.subprocess, 'run', return_value=completed):
            desktop.ime(actor, field['name'], field_path=field['path'])
        focused = predicates['real-ime-field-focused']
        tree = lambda node: {'nodes': [node]}
        self.assertTrue(focused(tree(dict(field))))
        stray = dict(field, path=[2])
        self.assertFalse(focused(tree(stray)))
        blurred = dict(field, focused=False)
        self.assertFalse(focused(tree(blurred)))
        readonly = dict(field, editable=False)
        self.assertFalse(focused(tree(readonly)))

    def run_ime(self, **rig):
        import sys
        from unittest import mock
        desktop, actor, field, order, pil, completed = self.rig(focused=True, **rig)
        with mock.patch.dict(sys.modules, {'PIL': pil}), \
             mock.patch.object(core_apps.subprocess, 'run', return_value=completed):
            return desktop.ime(actor, field['name'], field_path=field['path'])

    def test_gtk_field_text_equal_to_the_preedit_is_a_commit(self):
        # A GTK field's accessible text excludes the composition.
        with self.assertRaisesRegex(RuntimeError, 'already committed'):
            self.run_ime(text='你好')
        self.assertEqual(self.run_ime(text='')['preedit_ocr'], '你好')

    def test_web_field_must_hold_exactly_the_composition(self):
        # Run 45936499585: WebKit keeps its composition in the DOM, so
        # Epiphany's entry read 你好 while the engine's preedit was visible.
        self.assertEqual(self.run_ime(web=True, text='你好')['preedit_ocr'], '你好')
        with self.assertRaisesRegex(RuntimeError, 'exact engine composition'):
            self.run_ime(web=True, text='')


class PreeditInkIsolation(unittest.TestCase):
    """_isolate_preedit_ink() crops an entry capture to the composition ink.

    The full entry carries tesseract-hostile decorations (search icon,
    clear button, Pango's composition underline); the underline spans
    the composition width, so it locates the text without geometry
    constants. No underline means no isolation: the caller falls back
    to the whole crop.
    """

    def entry(self, text_left, text_width, underline=True, caret=False):
        from PIL import Image, ImageDraw
        image = Image.new('RGB', (200, 40), 'white')
        draw = ImageDraw.Draw(image)
        draw.ellipse((8, 12, 24, 28), outline='black', width=2)
        draw.text((text_left, 8), 'ab', fill='black')
        if underline:
            draw.line([(text_left, 30), (text_left + text_width, 30)], fill='black', width=2)
        if caret:
            draw.line([(text_left + text_width + 1, 8), (text_left + text_width + 1, 31)],
                      fill='black', width=1)
        return image

    def test_underlined_composition_isolates_to_text_span(self):
        isolated = core_apps._isolate_preedit_ink(self.entry(60, 80))
        self.assertIsNotNone(isolated)
        # The 60..140 underline span locates the text; the icon at the
        # left edge and the empty right side stay out of the ink, with
        # a white quiet zone added for the line finder. The region is
        # doubled for tesseract's small-CJK operating envelope.
        self.assertEqual(isolated.size, (182, 60))

    def test_blink_caret_frame_isolates_compatibly(self):
        blink = core_apps._isolate_preedit_ink(self.entry(60, 80, caret=True))
        self.assertIsNotNone(blink)
        # The edge caret is not composition ink: the blink frame isolates
        # byte-identically to the caretless one, so the OCR read cannot
        # depend on the blink phase. Origin and scale stay put throughout.
        self.assertEqual(blink.size, (182, 60))
        plain = core_apps._isolate_preedit_ink(self.entry(60, 80))
        self.assertEqual(list(blink.getdata()), list(plain.getdata()))

    def test_plain_capture_falls_back_to_whole_crop(self):
        self.assertIsNone(core_apps._isolate_preedit_ink(self.entry(60, 80, underline=False)))

    def test_blank_capture_falls_back_to_whole_crop(self):
        from PIL import Image
        self.assertIsNone(core_apps._isolate_preedit_ink(Image.new('RGB', (60, 20), 'white')))

    def test_accent_focus_ring_below_the_underline_is_not_ink(self):
        # Run 45936499585: WebKit draws its entry's accent-blue focus ring
        # two rows under the composition underline; the ring bottom, darker
        # than the threshold, was taken for the underline and the whole
        # entry width went to tesseract, which read "ME" for 你好.
        from PIL import ImageDraw
        image = self.entry(60, 80)
        ImageDraw.Draw(image).rectangle((2, 2, 197, 33), outline=(93, 157, 233), width=2)
        ringed = core_apps._isolate_preedit_ink(image)
        self.assertIsNotNone(ringed)
        self.assertEqual(ringed.size, (182, 60))
        self.assertEqual(list(ringed.getdata()), list(core_apps._isolate_preedit_ink(self.entry(60, 80)).getdata()))

    def test_dark_theme_field_isolates_like_light(self):
        # Run 46118838058: Connections' dark popover entry holds light ink
        # on a dark field, which held no dark ink at all.
        from PIL import ImageOps
        dark = core_apps._isolate_preedit_ink(ImageOps.invert(self.entry(60, 80)))
        self.assertIsNotNone(dark)
        self.assertEqual(list(dark.getdata()), list(core_apps._isolate_preedit_ink(self.entry(60, 80)).getdata()))

    def text_view(self, popup):
        from PIL import Image, ImageDraw
        image = Image.new('RGB', (200, 100), 'white')
        image.paste(self.entry(60, 80), (0, 0))
        if popup:
            draw = ImageDraw.Draw(image)
            draw.rounded_rectangle((40, 38, 190, 92), radius=6, fill=(53, 53, 53))
            draw.text((50, 45), 'ni hao', fill='white')
            draw.text((50, 65), '1 ab  2 abc  3 a', fill=(160, 160, 160))
        return image

    def test_candidate_popup_over_text_view_is_not_ink(self):
        # Run 46118838058: Text Editor's field is its whole text view, and
        # the engine's dark candidate popup composited over it OCR'd as the
        # candidate row instead of the composition.
        covered = core_apps._isolate_preedit_ink(self.text_view(popup=True))
        self.assertIsNotNone(covered)
        self.assertEqual(list(covered.getdata()),
                         list(core_apps._isolate_preedit_ink(self.text_view(popup=False)).getdata()))

    def test_light_composition_underline_is_ink(self):
        # Run 46118838058: GtkSourceView draws the composition underline at
        # luminance 153, just past a fixed 150 ink threshold.
        from PIL import ImageDraw
        image = self.entry(60, 80, underline=False)
        ImageDraw.Draw(image).line([(60, 30), (140, 30)], fill=(153, 152, 156), width=2)
        self.assertIsNotNone(core_apps._isolate_preedit_ink(image))


class PickerAcceptGesture(unittest.TestCase):
    def picker_tree(self, default):
        cell = {'path': [0, 1], 'name': 'principal.png. File', 'role': 'table cell',
                'showing': True, 'sensitive': True, 'selected': True, 'default': False}
        accept = {'path': [1], 'name': 'Open', 'role': 'push button',
                  'showing': True, 'sensitive': True, 'default': default}
        return {'nodes': [cell, accept]}

    def desktop(self, default):
        calls = {}
        desktop = core_apps.Desktop.__new__(core_apps.Desktop)
        desktop.tree = lambda actor: dict(self.picker_tree(default))
        desktop.guest = lambda *a, **k: calls.setdefault('guest', []).append((a, k)) or {}
        desktop.keys = lambda *keys: calls.setdefault('keys', []).append(keys)
        desktop.state = lambda: {'focused': 7, 'windows': []}

        def await_state(label, read, predicate, timeout=10):
            value = read()
            if predicate(value):
                return value
            raise TimeoutError('not ready: ' + label)
        desktop.await_state = await_state
        picker = {'window_id': 9}
        actor = {'window_id': 7}
        return desktop, calls, picker, actor

    def test_open_accepts_without_default_button(self):
        desktop, calls, picker, actor = self.desktop(default=False)
        result = desktop._accept_picker(actor, picker, 'fixture/principal.png', save=False)
        self.assertIn(('ret',), calls.get('keys', []))
        self.assertNotIn('press', [a[1] for a in calls.get('guest', [])])
        self.assertEqual(result['picker'], picker)

    def test_save_still_presses_default_button(self):
        desktop, calls, picker, actor = self.desktop(default=True)
        desktop._accept_picker(actor, picker, 'fixture/out.png', save=True)
        pressed = [a for a in calls.get('guest', []) if a[0][1] == 'press']
        self.assertEqual(len(pressed), 1)

    def realistic_entry(self, text_left=60, text_width=80, border=False, caret=False):
        # Pango-realistic geometry: the composition sits directly above its
        # underline (Calculator round-1004: a detached widget border below
        # the composition must not hijack the span, and a caret merged with
        # the last glyph must not enter the ink).
        from PIL import Image, ImageDraw
        image = Image.new('RGB', (200, 50), 'white')
        draw = ImageDraw.Draw(image)
        draw.ellipse((8, 12, 24, 28), outline='black', width=2)
        draw.text((text_left, 18), 'ab', fill='black')
        text_end = text_left + int(draw.textlength('ab'))
        draw.line([(text_left, 30), (text_end, 30)], fill='black', width=2)
        if border:
            draw.line([(0, 44), (199, 44)], fill='black', width=2)
        if caret:
            draw.line([(text_end + 1, 18), (text_end + 1, 32)], fill='black', width=1)
        return image

    def test_detached_widget_border_does_not_hijack_span(self):
        # The field's own bottom border is the lowest dominant run, but it
        # underlines nothing: isolation must match the borderless capture.
        plain = core_apps._isolate_preedit_ink(self.realistic_entry())
        bordered = core_apps._isolate_preedit_ink(self.realistic_entry(border=True))
        self.assertIsNotNone(plain)
        self.assertIsNotNone(bordered)
        self.assertEqual(bordered.size, plain.size)

    def test_caret_merged_with_last_glyph_is_excluded(self):
        # A caret directly after the last glyph would merge with it under
        # OCR: isolation must match the caretless capture exactly.
        plain = core_apps._isolate_preedit_ink(self.realistic_entry())
        merged = core_apps._isolate_preedit_ink(self.realistic_entry(caret=True))
        self.assertIsNotNone(plain)
        self.assertIsNotNone(merged)
        self.assertEqual(merged.size, plain.size)
        self.assertEqual(list(merged.getdata()), list(plain.getdata()))


class DomainImeRegistration(unittest.TestCase):
    """A domain-owned IME proof (Loupe's popover entry) must reach result().

    Run 42337013327: every Loupe step passed, then result() refused the
    whole app because the proof never entered the desktop's IME registry.
    """
    def desktop(self):
        desktop = core_apps.Desktop.__new__(core_apps.Desktop)
        desktop._ime = {}
        return desktop

    def proof(self, actor):
        return {'actor': actor, 'committed': {'nodes': []}, 'clipboard': {'base64': '5L2g'},
                'engine': {'actual': 'libpinyin'}}

    def test_registers_exact_app_proof(self):
        desktop = self.desktop()
        actor = {'id': 'org.gnome.Loupe', 'kind': 'app'}
        desktop.record_ime(actor, self.proof(actor))
        self.assertEqual(desktop._ime['org.gnome.Loupe'], self.proof(actor))

    def test_rejects_foreign_actor_picker_and_missing_evidence(self):
        actor = {'id': 'org.gnome.Loupe', 'kind': 'app'}
        desktop = self.desktop()
        with self.assertRaises(RuntimeError):
            desktop.record_ime(actor, self.proof({'id': 'other', 'kind': 'app'}))
        picker = {'id': 'picker', 'kind': 'picker'}
        with self.assertRaises(RuntimeError):
            desktop.record_ime(picker, self.proof(picker))
        for key in ('committed', 'clipboard', 'engine'):
            incomplete = self.proof(actor)
            del incomplete[key]
            with self.assertRaises(RuntimeError):
                desktop.record_ime(actor, incomplete)
        self.assertEqual(desktop._ime, {})


class StartSearchQuery(unittest.TestCase):
    """start() types the desktop id: names collide (Evince vs Papers)."""
    def test_types_app_id_not_the_display_name(self):
        from unittest import mock
        desktop = core_apps.Desktop.__new__(core_apps.Desktop)
        desktop.inventory = {'apps': {'org.gnome.Papers': {}}}
        before = {'windows': [{'id': 2, 'app_id': 'org.gnome.Nautilus'}]}
        after = {'overview_open': False, 'focused': 33, 'windows': [
            {'id': 2, 'app_id': 'org.gnome.Nautilus'}, {'id': 33, 'app_id': 'org.gnome.Papers'}]}
        desktop.state = mock.Mock(return_value=before)
        desktop.keys = mock.Mock()
        desktop.type_ascii = mock.Mock()
        desktop.await_state = lambda name, read, predicate, **kwargs: (
            after if name.startswith('normal-launch') else
            None if name.startswith('desktop-search') else {'pid': 9})
        desktop.guest = mock.Mock()
        desktop.retain = mock.Mock()
        desktop.observe = mock.Mock()
        desktop._actors = {}
        actor = desktop.start('org.gnome.Papers', 'Document Viewer')
        desktop.type_ascii.assert_called_once_with('org.gnome.Papers')
        self.assertEqual(actor['search_name'], 'Document Viewer')


class EnterLocationCompletion(unittest.TestCase):
    """Return in the location entry must activate it, not take a completion.

    Run 42395487640: the pointer stayed at [797.6, 253.3] from the previous
    chooser's file-cell click, inside the completion list the picker window
    [195, 141, 890, 550] popped up at [275, 59, 426, 176]. That list selects
    the hovered row, and Return then replaced `<fixture>/` with
    `<fixture>/web/` while the entry stayed open and nothing was opened.
    The rig models exactly that: Return over the list appends the hovered
    row; Return elsewhere closes the entry on its text.
    """
    WINDOW = {'id': 33, 'rect': [195, 141, 890, 550], 'buffer_rect': [195, 141, 890, 550]}
    ENTRY = [259, 0, 442, 34]
    LIST = [275, 59, 426, 176]
    PARKED = [797.6171875, 253.3203125]
    PICKER = {'id': 'picker', 'window_id': 33, 'a11y_scope': '/frame'}
    FRAME = {'object_path': '/frame', 'path': [0], 'role': 'frame', 'focused': False, 'text': None,
             'bounds': [0, 0, 890, 550]}

    def over(self, model, bounds):
        left, top = self.WINDOW['rect'][0] + bounds[0], self.WINDOW['rect'][1] + bounds[1]
        x, y = model['pointer']
        return left <= x < left + bounds[2] and top <= y < top + bounds[3]

    def rig(self, reads=None, rest=True, consume_return=False):
        keys = []
        model = {'pointer': list(self.PARKED), 'text': None, 'open': False, 'opened': None,
                 'return_pointer': None}
        scripted = None if reads is None else iter(reads)
        desktop = core_apps.Desktop.__new__(core_apps.Desktop)
        desktop.width, desktop.height, desktop.scale = 1280, 800, 1.0
        desktop._select_mouse = lambda qmp, relative: 'tablet'

        class Qmp:
            def cmd(self, command, events):
                x, y = (event['data']['value'] for event in events)
                model['pointer'] = [x * 1279 / 32767, y * 799 / 32767]
        desktop.qmp = Qmp()
        if not rest:
            desktop.rest_pointer = lambda *a: None

        def press(*names):
            keys.append(names)
            if names == ('ctrl', 'l'):
                model['open'] = True
            elif names == ('ret',):
                model['return_pointer'] = list(model['pointer'])
                if consume_return or self.over(model, self.LIST):
                    model['text'] += 'web/'
                else:
                    model['open'], model['opened'] = False, model['text']
        desktop.keys = press

        def type_ascii(text):
            keys.append(('type', text))
            model['text'] = text
        desktop.type_ascii = type_ascii

        def tree(window):
            text = next(scripted) if scripted is not None and model['open'] else model['text']
            return {'nodes': [self.FRAME, {'object_path': '/entry', 'path': [0, 0], 'role': 'text',
                                           'focused': model['open'], 'text': text, 'bounds': self.ENTRY}],
                    'state': {'windows': [self.WINDOW], 'pointer_position': list(model['pointer'])}}
        desktop.tree = tree

        def await_state(label, read, predicate, timeout=10):
            for _ in range(20):
                value = read()
                if predicate(value):
                    return value
            raise TimeoutError(label)
        desktop.await_state = await_state
        return desktop, keys, model

    def test_pointer_left_on_the_completion_list_does_not_take_return(self):
        base = '/fixture/run/'
        desktop, keys, model = self.rig()
        self.assertTrue(self.over(model, self.LIST))
        desktop.enter_location(self.PICKER, '/fixture/run')
        self.assertEqual(keys, [('ctrl', 'l'), ('type', base), ('ret',)])
        self.assertEqual(model['opened'], base)
        model['pointer'] = model['return_pointer']
        self.assertTrue(self.over(model, self.ENTRY))
        self.assertFalse(self.over(model, self.LIST))

    def test_return_is_withheld_while_the_pointer_is_not_on_the_entry(self):
        desktop, keys, model = self.rig(rest=False)
        with self.assertRaisesRegex(TimeoutError, '^picker-exact-directory-location$'):
            desktop.enter_location(self.PICKER, '/fixture/run')
        self.assertNotIn(('ret',), keys)

    def test_return_that_leaves_the_entry_open_fails(self):
        desktop, keys, model = self.rig(consume_return=True)
        with self.assertRaisesRegex(TimeoutError, '^files-location-return-closed-entry$'):
            desktop.enter_location(self.PICKER, '/fixture/run', label='files-location')
        self.assertEqual(keys[-1], ('ret',))
        self.assertEqual(model['text'], '/fixture/run/web/')
        self.assertIsNone(model['opened'])

    def test_completion_suffix_is_deleted_before_return(self):
        base = '/fixture/run/'
        desktop, keys, model = self.rig([base, base + 'web/', base, base, base])
        desktop.enter_location(self.PICKER, '/fixture/run')
        self.assertEqual(keys, [('ctrl', 'l'), ('type', base), ('delete',), ('ret',)])
        self.assertEqual(model['opened'], base)

    def test_return_waits_for_three_consecutive_exact_reads(self):
        base = '/fixture/run/'
        desktop, keys, model = self.rig([base, base, base, base])
        desktop.enter_location(self.PICKER, '/fixture/run/')
        self.assertEqual(keys[-1], ('ret',))
        desktop, keys, model = self.rig([base, base, base])
        with self.assertRaises(StopIteration):
            desktop.enter_location(self.PICKER, '/fixture/run/')
        self.assertNotIn(('ret',), keys)

    def test_locked_or_unbounded_pointer_never_counts_as_resting(self):
        desktop, keys, model = self.rig()
        node = {'path': [0, 0], 'bounds': self.ENTRY}
        state = {'windows': [self.WINDOW], 'pointer_position': [480, 158]}
        tree = {'state': state, 'nodes': [self.FRAME]}
        self.assertTrue(desktop._pointer_rests_on(self.PICKER, tree, node))
        self.assertFalse(desktop._pointer_rests_on(self.PICKER, tree, {'path': [0, 0], 'bounds': None}))
        state['pointer_position'] = None
        self.assertFalse(desktop._pointer_rests_on(self.PICKER, tree, node))


class ShadowedSurfacePointer(unittest.TestCase):
    """AT-SPI window coordinates start where the toolkit's frame does.

    GTK 3 (Connections, run 44850865831): the frame is the whole 710x601
    surface around the 658x549 rect [311, 141] (client shadow, geometry at
    [26, 23]); adding No Thanks' bounds [300, 486, 109, 34] to the rect put
    the click at [665, 644], below the button drawn at [585, 604, 109, 34].

    GTK 4 (Fonts and Nautilus, run 45936499585): the frame is the window
    geometry. Mapping through the 850x650 surface put Fonts' click on the
    family value [128, 95, 540, 18] 25 px up and left, onto the row title,
    so Ctrl+C copied nothing; Nautilus' preedit crop missed its field by the
    same 25 px.
    """
    WINDOW = {'id': 2, 'rect': [311, 141, 658, 549], 'buffer_rect': [285, 118, 710, 601]}
    ACTOR = {'id': 'org.gnome.Connections', 'window_id': 2, 'a11y_scope': '/gtk3'}
    FRAME = {'object_path': '/gtk3', 'path': [0], 'role': 'frame', 'bounds': [0, 0, 710, 601]}
    NO_THANKS = {'object_path': '/no-thanks', 'path': [0, 0], 'role': 'push button',
                 'bounds': [300, 486, 109, 34]}
    FONTS = {'id': 'org.gnome.font-viewer', 'window_id': 1, 'a11y_scope': '/gtk4'}
    FONTS_WINDOW = {'id': 1, 'rect': [240, 116, 800, 600], 'buffer_rect': [215, 91, 850, 650]}
    FONTS_FRAME = {'object_path': '/gtk4', 'path': [0], 'role': 'frame', 'bounds': [0, 0, 800, 600]}
    FAMILY = {'object_path': '/family', 'path': [0, 0], 'role': 'label', 'bounds': [128, 95, 540, 18]}

    def desktop(self):
        desktop = core_apps.Desktop.__new__(core_apps.Desktop)
        desktop.width, desktop.height, desktop.scale = 1280, 800, 1.0
        desktop._select_mouse = lambda qmp, relative: 'tablet'

        class Qmp:
            def cmd(self, command, events):
                pass
        desktop.qmp = Qmp()
        return desktop

    def test_gtk3_click_lands_on_the_drawn_button(self):
        tree = {'state': {'windows': [self.WINDOW]}, 'nodes': [self.FRAME, self.NO_THANKS]}
        x, y, _ = self.desktop()._move_to(self.ACTOR, tree, self.NO_THANKS, 0.5, 0.5)
        self.assertTrue(585 <= x < 585 + 109 and 604 <= y < 604 + 34, (x, y))

    def test_gtk4_click_lands_on_the_drawn_label(self):
        tree = {'state': {'windows': [self.FONTS_WINDOW]}, 'nodes': [self.FONTS_FRAME, self.FAMILY]}
        x, y, _ = self.desktop()._move_to(self.FONTS, tree, self.FAMILY, 0.5, 0.5)
        self.assertEqual((x, y), (240 + 128 + 270, 116 + 95 + 9))

    def test_pointer_rest_and_field_pixels_use_the_frame_origin(self):
        desktop = self.desktop()
        state = {'windows': [self.WINDOW], 'pointer_position': [640, 620]}
        tree = {'state': state, 'nodes': [self.FRAME]}
        self.assertTrue(desktop._pointer_rests_on(self.ACTOR, tree, self.NO_THANKS))
        self.assertEqual(desktop._field_pixels(self.ACTOR, tree, self.NO_THANKS), (585, 604, 109, 34))
        nautilus = {'id': 2, 'rect': [195, 141, 890, 550], 'buffer_rect': [170, 116, 940, 600]}
        tree = {'state': {'windows': [nautilus]},
                'nodes': [{'object_path': '/gtk3', 'path': [0], 'role': 'frame', 'bounds': [0, 0, 890, 550]}]}
        self.assertEqual(desktop._field_pixels(self.ACTOR, tree, {'path': [0, 0], 'bounds': [240, 235, 393, 50]}),
                         (435, 376, 393, 50))

    def test_web_content_is_placed_by_its_view(self):
        # Run 45936499585: Epiphany's view widget sits under the 46 px header
        # bar; WebKit's payload entry [171, 167, 193, 24] was drawn at
        # [299, 245] for the window geometry [128, 32].
        epiphany = {'id': 1, 'rect': [128, 32, 1024, 768], 'buffer_rect': [103, 7, 1074, 818]}
        actor = {'id': 'org.gnome.Epiphany', 'window_id': 1, 'a11y_scope': '/frame'}
        entry = {'object_path': '/entry', 'path': [0, 0, 0, 0, 0, 0], 'role': 'entry',
                 'bounds': [171, 167, 193, 24]}
        tree = {'state': {'windows': [epiphany]}, 'nodes': [
            {'object_path': '/frame', 'path': [0], 'role': 'frame', 'bounds': [0, 0, 1024, 768]},
            {'object_path': '/view', 'path': [0, 0], 'role': 'panel', 'bounds': [0, 46, 1024, 722]},
            {'object_path': '/root', 'path': [0, 0, 0], 'role': 'scroll pane', 'bounds': None},
            {'object_path': '/doc', 'path': [0, 0, 0, 0], 'role': 'document web', 'bounds': [0, 0, 1024, 722]},
            {'object_path': '/form', 'path': [0, 0, 0, 0, 0], 'role': 'form', 'bounds': [8, 166, 1008, 27]},
            entry]}
        self.assertEqual(self.desktop()._field_pixels(actor, tree, entry), (299, 245, 193, 24))
        # Outside the document, the view's own widgets stay window-relative.
        self.assertEqual(self.desktop()._field_pixels(actor, tree, tree['nodes'][1]), (128, 78, 1024, 722))

    def test_frame_matching_neither_rect_nor_surface_is_refused(self):
        window = {'id': 2, 'rect': [311, 141, 658, 549], 'buffer_rect': None}
        with self.assertRaises(RuntimeError):
            self.desktop()._move_to(self.ACTOR, {'state': {'windows': [window]}, 'nodes': [self.FRAME]},
                                    self.NO_THANKS, 0.5, 0.5)
        with self.assertRaises(RuntimeError):
            self.desktop()._move_to(self.ACTOR, {'state': {'windows': [self.WINDOW]}, 'nodes': []},
                                    self.NO_THANKS, 0.5, 0.5)

    def test_popover_widgets_are_placed_by_their_popup(self):
        # Run 46095497198: Connections' address popover is a [0, 0, 425,
        # 324] panel under the maximized window frame, its entry at [35, 78]
        # of the popover, not of the window.
        window = {'id': 1, 'rect': [0, 32, 1280, 768], 'buffer_rect': [0, 32, 1280, 768],
                  'popups': [{'rect': [12, 70, 425, 324], 'buffer_rect': [0, 58, 449, 348]}]}
        actor = {'id': 'org.gnome.Connections', 'window_id': 1, 'a11y_scope': '/frame'}
        entry = {'object_path': '/entry', 'path': [0, 2, 0, 0, 1], 'role': 'text', 'bounds': [35, 78, 355, 34]}
        header = {'object_path': '/header', 'path': [0, 0], 'role': 'panel', 'bounds': [0, 0, 1280, 47]}
        tree = {'state': {'windows': [window]}, 'nodes': [
            {'object_path': '/frame', 'path': [0], 'role': 'frame', 'bounds': [0, 0, 1280, 768]},
            header,
            {'object_path': '/popover', 'path': [0, 2], 'role': 'panel', 'bounds': [0, 0, 425, 324]},
            {'object_path': '/box', 'path': [0, 2, 0], 'role': 'panel', 'bounds': [0, 0, 400, 300]},
            entry]}
        self.assertEqual(self.desktop()._field_pixels(actor, tree, entry), (47, 148, 355, 34))
        # Widgets outside the popover stay window-relative.
        self.assertEqual(self.desktop()._field_pixels(actor, tree, header), (0, 32, 1280, 47))
        # A popover the size of its buffer starts at the buffer's corner.
        window['popups'] = [{'rect': [12, 70, 401, 300], 'buffer_rect': [0, 58, 425, 324]}]
        self.assertEqual(self.desktop()._field_pixels(actor, tree, entry), (35, 136, 355, 34))
        # Two popups of the popover's size cannot place it.
        window['popups'] = [{'rect': [12, 70, 425, 324], 'buffer_rect': None},
                            {'rect': [500, 70, 425, 324], 'buffer_rect': None}]
        with self.assertRaisesRegex(RuntimeError, 'more than one placed popup'):
            self.desktop()._field_pixels(actor, tree, entry)

    def test_subsurface_popover_widgets_are_placed_by_their_subsurface(self):
        # Run 46102176916: Connections' address popover is a subsurface of
        # the toplevel, not an xdg popup; the only exported popup is the
        # input method's 0x0 cursor popup.
        window = {'id': 1, 'rect': [0, 32, 1280, 768], 'buffer_rect': [0, 32, 1280, 768],
                  'popups': [{'rect': [74, 157, 0, 0], 'buffer_rect': None}],
                  'subsurfaces': [[0, 79, 425, 324]]}
        actor = {'id': 'org.gnome.Connections', 'window_id': 1, 'a11y_scope': '/frame'}
        entry = {'object_path': '/entry', 'path': [0, 2, 0, 0, 1], 'role': 'text', 'bounds': [35, 78, 355, 34]}
        tree = {'state': {'windows': [window]}, 'nodes': [
            {'object_path': '/frame', 'path': [0], 'role': 'frame', 'bounds': [0, 0, 1280, 768]},
            {'object_path': '/popover', 'path': [0, 2], 'role': 'panel', 'bounds': [0, 0, 425, 324]},
            entry]}
        self.assertEqual(self.desktop()._field_pixels(actor, tree, entry), (35, 157, 355, 34))
        # A popup and a subsurface of the popover's size cannot place it.
        window['popups'].append({'rect': [500, 70, 425, 324], 'buffer_rect': None})
        with self.assertRaisesRegex(RuntimeError, 'more than one placed popup or subsurface'):
            self.desktop()._field_pixels(actor, tree, entry)


class PickerRevealsOffscreenCell(unittest.TestCase):
    """Run 46095497198: Decibels' chooser built cells for twelve of the
    media folder's files and never tone.ogg below them; the view scrolls
    until the cell's center is inside it, then the cell is clicked."""

    VIEW = {'showing': True, 'role': 'table', 'name': 'Content View', 'path': [0, 1],
            'bounds': [180, 46, 710, 436], 'selected': False}

    def cell(self, top, selected=False):
        return {'showing': True, 'role': 'table cell', 'name': 'tone.ogg. File', 'path': [0, 1, 7],
                'bounds': [190, top, 144, 150], 'selected': selected}

    def test_scrolls_down_until_the_cell_center_is_in_view(self):
        frames = iter([[self.VIEW], [self.VIEW, self.cell(460)], [self.VIEW, self.cell(200)],
                       [self.VIEW, self.cell(200, selected=True)]])
        desktop = core_apps.Desktop.__new__(core_apps.Desktop)
        desktop.fixture = '/fixture'
        desktop.guest = lambda domain, operation, **arguments: {'files': [{'exists': True}]}
        desktop.file_picker = lambda actor: {'id': 'picker'}
        desktop.enter_location = lambda picker, target: None
        desktop.tree = lambda picker: {'nodes': next(frames)}
        wheels, clicked = [], []
        desktop.rest_pointer = lambda picker, tree, node: None

        class Qmp:
            def cmd(self, command, events):
                if events[0]['data']['down']:
                    wheels.append(events[0]['data']['button'])
        desktop.qmp = Qmp()
        desktop.click_path = lambda picker, path: clicked.append(path)

        def await_state(label, read, predicate, timeout=10):
            while True:
                value = read()
                if predicate(value):
                    return value
        desktop.await_state = await_state
        with mock.patch.object(core_apps.time, 'sleep'):
            desktop._prepare_picker({'id': 'caller'}, '/fixture/media/tone.ogg')
        self.assertEqual(wheels, ['wheel-down'] * 6)
        self.assertEqual(clicked, [[0, 1, 7]])


class TransientDialogDismissal(unittest.TestCase):
    """Run 45936499585: start() bound Connections' focused welcome dialog
    (window 2, transient for main window 1); No Thanks closed it and every
    tree scoped to its departed frame failed."""
    WELCOME = {'id': 2, 'parent_window_id': 1}
    MAIN = {'id': 1, 'parent_window_id': None}
    ACTOR = {'id': 'org.gnome.Connections', 'pid': 50, 'start_ticks': 7, 'window_id': 2,
             'search_name': 'Connections'}

    def rig(self, rebound_pid=50):
        calls = []
        model = {'windows': [self.MAIN, self.WELCOME]}
        desktop = core_apps.Desktop.__new__(core_apps.Desktop)
        desktop._actors = {self.ACTOR['id']: self.ACTOR}
        desktop.state = lambda: {'windows': list(model['windows'])}
        desktop.select = lambda actor: calls.append(('select', actor['window_id']))

        def guest(domain, operation, **arguments):
            calls.append((operation, arguments.get('window_id')))
            return {'pid': rebound_pid, 'start_ticks': 7, 'window_id': arguments['window_id']}
        desktop.guest = guest

        def await_state(label, read, predicate, timeout=10):
            value = read()
            if not predicate(value):
                raise TimeoutError(label)
            return value
        desktop.await_state = await_state

        def dismiss():
            calls.append('dismiss')
            model['windows'] = [self.MAIN]
        return desktop, calls, dismiss

    def test_the_same_process_is_rebound_to_the_parent_window(self):
        desktop, calls, dismiss = self.rig()
        actor = desktop.dismiss_transient(self.ACTOR, dismiss)
        self.assertEqual((actor['window_id'], actor['search_name']), (1, 'Connections'))
        self.assertIs(desktop._actors['org.gnome.Connections'], actor)
        self.assertEqual(calls, ['dismiss', ('discover', 1), ('select', 1)])

    def test_a_dialog_still_mapped_or_another_process_is_refused(self):
        desktop, calls, _ = self.rig()
        with self.assertRaisesRegex(TimeoutError, 'transient-dismissed'):
            desktop.dismiss_transient(self.ACTOR, lambda: None)
        desktop, calls, dismiss = self.rig(rebound_pid=51)
        with self.assertRaisesRegex(RuntimeError, 'changed the original app process'):
            desktop.dismiss_transient(self.ACTOR, dismiss)
        desktop, calls, dismiss = self.rig()
        with self.assertRaisesRegex(RuntimeError, 'not a transient dialog'):
            desktop.dismiss_transient(dict(self.ACTOR, window_id=1), dismiss)


class PickerFilterDiagnostic(unittest.TestCase):
    """Only the chooser's filter toggle is opened for the empty-listing receipt.

    Run 42395487640 retained `All Documents` and `Show Options` as candidates,
    so the filter menu was never opened.
    """
    def test_named_chooser_toggles_are_not_filter_candidates(self):
        names = ('', 'Search Current Folder', 'View Options', 'Search Everywhere',
                 'Show Options', 'Current Folder Menu', 'All Documents')
        tree = {'nodes': [{'showing': True, 'role': 'toggle button', 'sensitive': True,
                           'name': name, 'path': [index]} for index, name in enumerate(names)]}
        retained, clicked, keys = {}, [], []
        desktop = core_apps.Desktop.__new__(core_apps.Desktop)
        desktop.observe = lambda label, actor: {}
        desktop.tree = lambda actor: tree
        desktop.retain = lambda label, value: retained.__setitem__(label, value)
        desktop.click_path = lambda actor, path: clicked.append(path)
        desktop.keys = lambda *names: keys.append(names)
        desktop._picker_listing_diagnostic({'id': 'picker'})
        self.assertEqual([node['name'] for node in retained['picker-filter-candidates']['candidates']],
                         ['All Documents'])
        self.assertEqual(clicked, [[6]])
        self.assertEqual(keys, [('esc',)])
        self.assertNotIn('picker-diagnostic-error', retained)


class InstalledModulePrecedence(unittest.TestCase):
    """The Core guest runs the clock modules its own fixture admits.

    The ordinary lane image installs standalone copies of `autotz.py` and
    `geoclue_fixture.py` beside `native_session.py` for the `autotz` row,
    while `fixture_members('core-apps')` binds only `/usr/libexec/tuna-core-apps`.
    Source layout and import order only; no image or VM qualification.
    """
    def test_shared_session_directory_never_shadows_core_modules(self):
        lane = ROOT / 'packaging' / 'marlin' / 'vm-lane'
        with tempfile.TemporaryDirectory() as scratch:
            core = Path(scratch) / 'tuna-core-apps'
            session = Path(scratch) / 'tuna-native-session'
            ignore = shutil.ignore_patterns('__pycache__')
            shutil.copytree(lane / 'core-apps', core, ignore=ignore)
            shutil.copytree(lane / 'tuna-native-session', session, ignore=ignore)
            for name in ('autotz.py', 'geoclue_fixture.py'):
                shutil.copy(lane / 'core-apps' / name, session / name)
            # tuna-vm-lifecycle's `core-apps` action: its directory, then guest.
            program = ('import importlib, json, sys\n'
                       'sys.path.insert(0, sys.argv[1])\n'
                       'import guest\n'
                       'names = ("basic", "autotz", "geoclue_fixture", "native_session")\n'
                       'print(json.dumps({name: importlib.import_module(name).__file__ for name in names}))\n')
            resolved = json.loads(subprocess.run(
                [sys.executable, '-B', '-c', program, str(core)],
                check=True, capture_output=True, text=True, timeout=30).stdout)
        self.assertEqual({name: Path(path).parent.name for name, path in resolved.items()},
                         {'basic': 'tuna-core-apps', 'autotz': 'tuna-core-apps',
                          'geoclue_fixture': 'tuna-core-apps', 'native_session': 'tuna-native-session'})


class EditableBufferIdentity(unittest.TestCase):
    """Run 46095497198: WebKit rebuilt Epiphany's page accessibles under
    fresh object paths across the lock while the payload entry and its
    empty text stayed; buffers are keyed by place, role and name."""

    def node(self, object_path, text, path=(0, 1), name='Qualification payload'):
        return {'object_path': object_path, 'path': list(path), 'role': 'entry', 'name': name,
                'editable': True, 'text': text}

    def test_rebuilt_accessible_keeps_its_buffer(self):
        before = core_apps.Desktop._editable_buffers({'nodes': [self.node('/a', '')]})
        self.assertEqual(core_apps.Desktop._editable_buffers({'nodes': [self.node('/b', '')]}), before)

    def test_changed_text_or_control_differs(self):
        before = core_apps.Desktop._editable_buffers({'nodes': [self.node('/a', '')]})
        for nodes in ([self.node('/a', 'x')], [self.node('/a', '', path=(0, 2))],
                      [self.node('/a', '', name='Other')], []):
            self.assertNotEqual(core_apps.Desktop._editable_buffers({'nodes': nodes}), before)


if __name__ == '__main__':
    unittest.main()
