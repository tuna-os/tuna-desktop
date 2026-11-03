"""Host Files-cell matching regressions; no VM qualification."""
import json
import os
import sys
import unittest
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

    def admit(self, pids, identity, package='ibus\n'):
        import types
        from unittest import mock
        context = types.SimpleNamespace(
            uid=1000, command=mock.Mock(return_value=types.SimpleNamespace(stdout=package)))
        with mock.patch.object(self.native, '_proc_exe_pids', return_value=pids), \
                mock.patch.object(self.native, 'process', return_value=identity):
            try:
                return context, self.native.admit_ibus_daemon(context), None
            except RuntimeError as error:
                return context, None, error

    def identity(self, pid=4321):
        return {'pid': pid, 'uid': 1000, 'start_ticks': 7,
                'exe': '/usr/bin/ibus-daemon', 'exe_sha256': 'ab' * 32,
                'cgroup': ['0::/user.slice']}

    def test_unique_packaged_daemon_admits(self):
        context, daemon, error = self.admit([4321], self.identity())
        self.assertIsNone(error)
        self.assertEqual(daemon['package'], 'ibus')
        self.assertEqual(daemon['pid'], 4321)
        context.command.assert_called_once_with(['pacman', '-Qqo', '/usr/bin/ibus-daemon'])

    def test_missing_or_duplicate_daemon_refused(self):
        _, _, error = self.admit([], self.identity())
        self.assertIn('not unique: 0', str(error))
        _, _, error = self.admit([4321, 4322], self.identity())
        self.assertIn('not unique: 2', str(error))

    def test_changed_executable_refused(self):
        other = self.identity()
        other['exe'] = '/usr/bin/ibus-daemon-swapped'
        _, _, error = self.admit([4321], other)
        self.assertIn('changed during admission', str(error))

    def test_foreign_package_refused(self):
        _, _, error = self.admit([4321], self.identity(), package='not-ibus\n')
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
        context = types.SimpleNamespace(uid=1000)
        state = {'native_keyboard_windows': [{'window_id': 7, 'client_pid': 300,
                                              'client_uid': 1000, 'actual_keyboard_focus': True}]}
        tree = {'nodes': [{'showing': True, 'name': '你好',
                           'ancestors': [{'name': 'Input Method Candidates'}]}]}

        def desktop(operation, arguments, _context):
            return {'shell': True} if operation == 'shell-actor' else (
                tree if operation == 'tree' else state)

        def identify(pid, uid):
            if pid in observed and isinstance(observed[pid], BaseException):
                raise observed[pid]
            return observed.get(pid, {'pid': pid, 'uid': uid})

        guest = types.SimpleNamespace(desktop=desktop)
        with mock.patch.object(self.native, 'process', side_effect=identify), \
                mock.patch.object(self.native, '_rows', return_value=rows), \
                mock.patch.object(self.native, 'RUN_STATE') as run, \
                mock.patch.dict(sys.modules, {'guest': guest}):
            run.read_text.return_value = json.dumps(run_state)
            return self.native.dispatch('ime-state', {'actor': actor}, context)

    def admitted(self):
        monitor = {'pid': 200, 'uid': 1000}
        return {'ime_daemon': self.daemon(), 'ime_monitor': monitor,
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
        self.assertEqual(response['daemon'], self.daemon())
        self.assertEqual(response['candidates'], ['你好'])

    def test_departed_daemon_refused(self):
        admitted = self.admitted()
        observed = admitted.pop('observed')
        observed[100] = FileNotFoundError()
        rows = [{'ready': True, 'kind': 'ime', 'uid': 1000, 'ibus_owner': ':1.9'}]
        with self.assertRaisesRegex(RuntimeError, 'daemon changed or departed'):
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

    def test_monitor_start_admits_daemon_and_strips_package_for_liveness(self):
        import types
        from unittest import mock
        daemon = dict(self.daemon(), package='ibus')
        monitor = {'pid': 200, 'uid': 1000}
        context = types.SimpleNamespace(uid=1000)
        with mock.patch.object(self.native, 'admit_ibus_daemon', return_value=daemon), \
                mock.patch.object(self.native, '_spawn', return_value=monitor) as spawned, \
                mock.patch.object(self.native, 'RUN_STATE') as run, \
                mock.patch.object(self.native, 'IME_LOG') as log:
            log.exists.return_value = False
            run.read_text.return_value = '{}'
            response = self.native.dispatch('ime-monitor-start', {'actor': {'pid': 300}},
                                            context)
        spawned.assert_called_once()
        self.assertEqual(response, {'monitor': monitor, 'daemon': daemon})
        written = json.loads(run.write_text.call_args[0][0])
        self.assertFalse(written['ime_retired'])
        # RUN_STATE keeps pure process identity so per-observation
        # liveness compares equal; the package rides the response only.
        self.assertNotIn('package', written['ime_daemon'])
        self.assertEqual(written['ime_daemon'], self.daemon())


if __name__ == '__main__':
    unittest.main()
