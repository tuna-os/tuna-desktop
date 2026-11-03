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
        calls = connections[0].calls
        self.assertEqual([method for _, method, _ in calls], ['AddMatch', 'AddMatch'])
        rules = [parameters[1][0] for _, _, parameters in calls]
        self.assertTrue(all("eavesdrop='true'" in rule for rule in rules))
        self.assertTrue(any("member='UpdatePreeditText'" in rule for rule in rules))
        self.assertTrue(any("member='UpdatePreeditTextWithMode'" in rule for rule in rules))

    def test_portal_monitor_keeps_become_monitor(self):
        connections, ready = self.observe('portal')
        self.assertEqual(ready['ready'], True)
        self.assertEqual(ready['kind'], 'portal')
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


if __name__ == '__main__':
    unittest.main()
