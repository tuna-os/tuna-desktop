#!/usr/bin/env python3
"""Installed consent must not confuse accessible widgets with an unlocked screen."""
import contextlib
import importlib.machinery
import importlib.util
import io
import json
from pathlib import Path
import runpy
import sys
import tempfile
from types import ModuleType, SimpleNamespace
import unittest
from unittest.mock import Mock, patch

SCRIPTS = Path(__file__).resolve().parents[1]
# The state/launch tests need no image codec, browser, D-Bus or accessibility bus.
with patch.dict(sys.modules, {'PIL': SimpleNamespace(Image=Mock(), ImageChops=Mock())}):
    loader = importlib.machinery.SourceFileLoader('corral_journey', str(SCRIPTS / 'tuna-corral-journey'))
    spec = importlib.util.spec_from_loader(loader.name, loader)
    journey = importlib.util.module_from_spec(spec)
    loader.exec_module(journey)

sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_corral_identity import FakeCorral


class PortalSessionTests(unittest.TestCase):
    def test_records_unlocked_state_at_each_boundary_without_more_ssh(self):
        guest, record = Mock(), {}
        for phase in ('before-request', 'after-response'):
            report = {'session_state': [{'phase': phase, 'exit': 0, 'stdout': '(false,)', 'stderr': ''}]}
            guest.sh.return_value = (0, json.dumps(report), '')
            journey.portal_exchange(guest, record, 'launch', {}, timeout=120)
            journey.require_report_unlocked(record, phase)
        self.assertEqual([s['phase'] for s in record['session_state']],
                         ['before-request', 'after-response'])
        self.assertEqual(guest.sh.call_count, 2)

    def test_locked_or_unreadable_session_is_a_precondition_failure(self):
        for code, stdout in ((0, '(true,)'), (1, ''), (124, ''), (0, ''), (0, 'unexpected')):
            with self.subTest(code=code, stdout=stdout):
                record = {'session_state': [{'phase': 'before-request', 'exit': code, 'stdout': stdout}]}
                with self.assertRaisesRegex(RuntimeError, 'requires an unlocked session'):
                    journey.require_report_unlocked(record, 'before-request')
        with self.assertRaisesRegex(RuntimeError, 'requires an unlocked session'):
            journey.require_report_unlocked({}, 'before-request')

    def test_both_capture_paths_stop_after_failed_launch_without_consent(self):
        for target in ('portal-screenshot', 'portal-screencast'):
            with self.subTest(target=target), tempfile.TemporaryDirectory() as tmp:
                guest, console, receipt = Mock(), Mock(), {}
                guest.sh.return_value = (1, json.dumps({
                    'ready': False, 'error': 'requires an unlocked session',
                    'session_state': [{'phase': 'before-request', 'exit': 0, 'stdout': '(true,)'}]}), '')
                run = journey.Journey(SimpleNamespace(), guest, console, receipt, Path(tmp))
                run.step = lambda sid, title, action: action(lambda name, ok, observed: ok) if sid == target else None
                run.run()
                console.activity.assert_called_once()
                guest.sh.assert_called_once()
                console.key.assert_not_called()
                self.assertIn(' launch ', guest.sh.call_args.args[0])

    def test_activity_is_real_pointer_motion_without_a_click_or_unlock(self):
        console = object.__new__(journey.Console)
        console.page = Mock()
        console.page.locator.return_value.bounding_box.return_value = {
            'x': 4, 'y': 6, 'width': 1280, 'height': 800}
        console.activity()
        self.assertEqual([c.args for c in console.page.mouse.move.call_args_list],
                         [(14.0, 796.0), (15.0, 796.0)])
        console.page.mouse.click.assert_not_called()
        console.page.keyboard.press.assert_not_called()

    def test_active_grant_uses_two_exchanges_then_immediate_real_lock(self):
        with tempfile.TemporaryDirectory() as tmp:
            events, receipt = [], {}
            guest, console = Mock(), Mock()
            reports = [
                {'ready': True, 'finished': False, 'root': '/run/user/1000/owned',
                 'session_state': [{'phase': 'before-request', 'exit': 0, 'stdout': '(false,)'}]},
                {'input': {'exit': 0}, 'active': True, 'observer_armed': True, 'armed_at': 'recorded',
                 'session_state': [{'phase': 'after-response', 'exit': 0, 'stdout': '(false,)'}]}]
            def shell(cmd, **kw):
                events.append(('ssh', cmd))
                return 0, json.dumps(reports.pop(0)), ''
            guest.sh.side_effect = shell
            console.grab.side_effect = lambda path: path.write_bytes(b'prompt-frame')
            console.key.side_effect = lambda chord, **kw: events.append(('key', chord))
            run = journey.Journey(SimpleNamespace(), guest, console, receipt, Path(tmp))
            def step(sid, title, action):
                if sid == 'portal-screencast':
                    action(lambda name, ok, observed: ok)
                    events.append(('collect', 'step screenshot/journal'))
            run.step = step
            run.run()
            self.assertEqual(guest.sh.call_count, 2)
            self.assertEqual(events[-2:], [('key', 'Meta+l'), ('collect', 'step screenshot/journal')])
            self.assertIn(' consent ', events[1][1])
            guest.out.assert_not_called()
            self.assertTrue(receipt['portal_screencast']['lock_input']['observer_armed'])
            self.assertFalse(receipt['portal_screencast']['frame_validated'])

    def test_screenshot_retains_native_failure_before_locked_response_precondition(self):
        with tempfile.TemporaryDirectory() as tmp:
            guest, console, receipt = Mock(), Mock(), {}
            guest.sh.side_effect = [
                (0, json.dumps({'ready': True, 'finished': False, 'root': '/run/user/1000/owned',
                    'session_state': [{'phase': 'before-request', 'exit': 0, 'stdout': '(false,)'}]}), ''),
                (0, json.dumps({'requester_exit': '1', 'log': 'native Screenshot denied',
                    'response': {'response': 2, 'results': {}}, 'input': {'exit': 0},
                    'session_state': [{'phase': 'after-response', 'exit': 0, 'stdout': '(true,)'}]}), '')]
            console.grab.side_effect = lambda path: path.write_bytes(b'prompt-frame')
            run = journey.Journey(SimpleNamespace(), guest, console, receipt, Path(tmp))
            run.step = lambda sid, title, action: action(lambda name, ok, observed: ok) if sid == 'portal-screenshot' else None
            with self.assertRaisesRegex(RuntimeError, 'after-response requires an unlocked session'):
                run.run()
            record = receipt['portal_request_console']
            self.assertEqual(record['requester_exit'], '1')
            self.assertEqual(record['log'], 'native Screenshot denied')
            self.assertEqual(receipt['portal_screenshot']['response'], 2)
            self.assertEqual(guest.sh.call_count, 2)

    def test_invalid_exchange_retains_complete_diagnostics(self):
        guest, record = Mock(), {}
        guest.sh.return_value = (1, 'not JSON\\nfull stdout', 'complete transport stderr')
        with self.assertRaisesRegex(RuntimeError, 'structured receipt'):
            journey.portal_exchange(guest, record, 'launch', {}, timeout=120)
        self.assertEqual(record['exchange_errors'][0]['stderr'], 'complete transport stderr')
        self.assertIn('full stdout', record['exchange_errors'][0]['stdout'])


class FailureReceiptTests(unittest.TestCase):
    def test_console_start_failure_preserves_full_stderr(self):
        detail = 'BEGIN ' + 'builder failure ' * 1000 + ' END'
        def spawn(*args, **kwargs):
            kwargs['stderr'].write(detail)
            kwargs['stderr'].flush()
            return SimpleNamespace(stdout=io.StringIO(''), returncode=1, wait=lambda **kw: 1)
        with patch.object(journey.subprocess, 'Popen', side_effect=spawn):
            with self.assertRaises(RuntimeError) as caught:
                journey.start_console(SimpleNamespace(target='actual-opaque-inventory-id'))
        self.assertIn(detail, str(caught.exception))

    def test_build_failure_retains_stdout_stderr_and_password_stays_stdin(self):
        fake = FakeCorral()
        identity = fake.prepare()
        args = SimpleNamespace(reuse=False, vm=fake.name, image=fake.image, cpu=4,
                               mem='6G', disk='40Gi', namespace=fake.namespace, node=fake.node)
        receipt = {'vm': {}, 'image': {}}
        detail = 'CORRAL_BUILD_FAIL ' + 'serial detail ' * 1000
        with patch.object(journey, 'run', return_value=SimpleNamespace(
                returncode=1, stdout=detail, stderr='complete stderr')) as command:
            with self.assertRaisesRegex(RuntimeError, 'complete output retained'):
                journey.provision(args, Mock(), receipt, identity)
        self.assertEqual(receipt['provision']['stdout'], detail)
        self.assertEqual(receipt['provision']['stderr'], 'complete stderr')
        self.assertFalse(command.call_args.kwargs['check'])
        self.assertEqual(command.call_args.kwargs['input'], journey.PASSWORD + '\n')
        self.assertNotIn(journey.PASSWORD, receipt['provision']['argv'])
        self.assertIn('--context', receipt['provision']['argv'])
        self.assertEqual(receipt['provision']['argv'][receipt['provision']['argv'].index('--context') + 1],
                         'fixture-alias')
        self.assertEqual(identity.state['identities'], {})


class CanonicalCallerTests(unittest.TestCase):
    def test_start_restart_wait_info_ssh_console_and_crosscheck_use_exact_id(self):
        for status, lifecycle in (('Stopped', 'start'), ('Running', 'restart')):
            with self.subTest(status=status), tempfile.TemporaryDirectory() as tmp:
                fake = FakeCorral()
                identity = fake.prepare()
                fake.add_vm()['status'] = status
                args = SimpleNamespace(reuse=True, vm=fake.name, image=fake.image, node=fake.node)
                receipt = {'vm': {}, 'image': {}}
                calls = []
                def command(argv, **kwargs):
                    calls.append(argv)
                    if argv[:3] == ['corral', '--json', 'wait']:
                        stdout = json.dumps({'ready': True})
                    elif argv[:3] == ['corral', '--json', 'info']:
                        stdout = fake.command(argv)['stdout']
                    elif argv[:2] == ['corral', 'ssh']:
                        script = argv[-1]
                        stdout = (json.dumps({'status': {'booted': {'image': {
                            'image': {'image': fake.image}, 'imageDigest': 'digest'}}}})
                            if script == 'bootc status --format json' else 'fixture-output')
                    else:
                        stdout = ''
                    return SimpleNamespace(returncode=0, stdout=stdout, stderr='')
                guest = journey.Guest(identity)
                with patch.object(journey, 'run', side_effect=command):
                    journey.provision(args, guest, receipt, identity)
                    before = len(fake.calls)
                    guest.sh('ordinary exchange')
                    guest.sh('another ordinary exchange')
                    self.assertEqual(len(fake.calls), before, 'SSH must not add UID roundtrips')
                    # A screenshot failure avoids image codec use but exercises routing.
                    with patch.object(journey, 'run', return_value=SimpleNamespace(
                            returncode=1, stdout='', stderr='capture failed')) as screenshot:
                        tour = journey.Journey(args, guest, Mock(), {}, Path(tmp))
                        tour.crosscheck(Path(tmp) / 'frame.png')
                        self.assertEqual(screenshot.call_args.args[0][2], fake.target)
                self.assertIn(['corral', lifecycle, fake.target], calls)
                for argv in calls:
                    if argv[:2] == ['corral', 'ssh']:
                        self.assertEqual(argv[2], fake.target)
                    if argv[:3] in (['corral', '--json', 'wait'], ['corral', '--json', 'info']):
                        self.assertEqual(argv[3], fake.target)
                identity.acquire(boundary='before-console-journey')
                endpoint = {'vm': fake.name, 'namespace': fake.namespace, 'context': '', 'url': 'http://console/'}
                proc = Mock(stdout=io.StringIO(json.dumps(endpoint) + '\n'))
                with patch.object(journey.subprocess, 'Popen', return_value=proc) as spawn:
                    self.assertEqual(journey.start_console(identity), (proc, endpoint))
                    self.assertEqual(spawn.call_args.args[0], ['corral', '--json', 'console', fake.target])

    def test_main_failure_capture_and_cleanup_never_fall_back_to_names(self):
        fake = FakeCorral()
        identity = fake.prepare()
        fake.add_vm()
        captures = []
        def command(argv, **kwargs):
            if argv[:2] == ['corral', 'screenshot']:
                captures.append(argv)
                Path(argv[-1]).write_bytes(b'failure-console')
            return SimpleNamespace(returncode=0, stdout='fixture-version', stderr='')
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / 'evidence'
            with patch.dict(sys.modules, {'playwright.sync_api': SimpleNamespace(sync_playwright=Mock())}), \
                    patch.object(journey.Identity, 'prepare', return_value=identity), \
                    patch.object(journey, 'provision', side_effect=RuntimeError('readiness failed')), \
                    patch.object(journey, 'run', side_effect=command), \
                    patch('sys.argv', ['journey', '--vm', fake.name, '--image', fake.image,
                                       '--node', fake.node, '--out', str(out)]):
                with self.assertRaisesRegex(RuntimeError, 'readiness failed'):
                    journey.main()
            receipt = json.loads((out / 'manifest.json').read_text())
        self.assertEqual(captures[0][2], fake.target)
        self.assertEqual(receipt['failure_capture']['argv'], captures[0])
        self.assertTrue(receipt['cleanup']['deleted'])
        self.assertEqual([operation['canonical_identity']['id'] for operation in receipt['cleanup']['result']],
                         [fake.target])

    def test_resolution_failure_skips_failure_screenshot_and_refuses_cleanup(self):
        fake = FakeCorral()
        identity = fake.prepare()
        fake.add_vm()
        fake.info_changes = {'node': 'foreign-node'}
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / 'evidence'
            with patch.dict(sys.modules, {'playwright.sync_api': SimpleNamespace(sync_playwright=Mock())}), \
                    patch.object(journey.Identity, 'prepare', return_value=identity), \
                    patch.object(journey, 'provision', side_effect=RuntimeError('resolution failed')), \
                    patch.object(journey, 'run', return_value=SimpleNamespace(
                        returncode=0, stdout='fixture-version', stderr='')) as command, \
                    patch('sys.argv', ['journey', '--vm', fake.name, '--image', fake.image,
                                       '--node', fake.node, '--out', str(out)]):
                with self.assertRaisesRegex(RuntimeError, 'resolution failed'):
                    journey.main()
            receipt = json.loads((out / 'manifest.json').read_text())
        self.assertFalse(any(call.args[0][:2] == ['corral', 'screenshot'] for call in command.call_args_list))
        self.assertFalse(receipt['cleanup']['deleted'])
        self.assertIn('node mismatch', receipt['failure_capture']['error'])


class ConsentVisibilityTests(unittest.TestCase):
    def consent(self, active=False, error=None, stale=False, expired=False):
        action = Mock()
        button = SimpleNamespace(name='Share', childCount=0,
                                 getRoleName=lambda: 'push button',
                                 getState=lambda: SimpleNamespace(contains=lambda state: True),
                                 queryAction=lambda: action)
        buttons = [button]
        if stale:
            buttons.insert(0, SimpleNamespace(name='Allow', childCount=0,
                getRoleName=lambda: 'push button',
                getState=lambda: SimpleNamespace(contains=lambda state: True),
                queryAction=lambda: action.stale))
        app = SimpleNamespace(name='xdg-desktop-portal-gnome', childCount=len(buttons),
                              getRoleName=lambda: 'application',
                              getState=lambda: SimpleNamespace(contains=lambda state: False),
                              getChildAtIndex=lambda index: buttons[index])
        desktop = SimpleNamespace(childCount=1, getChildAtIndex=lambda index: app)
        pyatspi = SimpleNamespace(Registry=SimpleNamespace(getDesktop=lambda index: desktop),
                                  STATE_SHOWING=1, STATE_SENSITIVE=2)
        bus = Mock()
        bus.call_sync.return_value.unpack.return_value = (active,)
        bus.call_sync.side_effect = error
        gio = SimpleNamespace(bus_get_sync=lambda *args: bus, BusType=SimpleNamespace(SESSION=0),
                              DBusCallFlags=SimpleNamespace(NONE=0))
        repository = ModuleType('gi.repository')
        repository.Gio, repository.GLib = gio, SimpleNamespace()
        modules = {'pyatspi': pyatspi, 'gi': ModuleType('gi'), 'gi.repository': repository}
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / 'tree.json'
            exit_path = Path(tmp) / 'requester.exit'
            if expired:
                exit_path.write_text('1')
            with patch.dict(sys.modules, modules), patch('sys.argv', [
                    'tuna-portal-consent.py', str(out), 'grant', '--external-capture',
                    '--kind', 'screencast', '--request-exit', str(exit_path)]), \
                    contextlib.redirect_stdout(io.StringIO()):
                try:
                    runpy.run_path(str(SCRIPTS / 'lib/tuna-portal-consent.py'), run_name='__main__')
                except (RuntimeError, SystemExit) as result:
                    return action, bus, result, json.loads(out.read_text()) if out.exists() else []
        self.fail('consent did not terminate')

    def test_showing_sensitive_share_behind_lock_is_not_activated(self):
        action, bus, result, tree = self.consent(active=True)
        self.assertIsInstance(result, RuntimeError)
        self.assertIn('hidden by a locked session', str(result))
        self.assertTrue(tree[-1]['showing'], 'regression: AT-SPI still marks the hidden button showing')
        action.doAction.assert_not_called()
        self.assertEqual(bus.call_sync.call_args.args[3], 'GetActive')

    def test_unlocked_visible_share_uses_the_actual_action(self):
        action, _, result, _ = self.consent(active=False)
        self.assertIsInstance(result, SystemExit)
        self.assertEqual(result.code, 0)
        action.doAction.assert_called_once_with(0)

    def test_lock_query_failure_never_grants_consent(self):
        action, _, result, _ = self.consent(error=RuntimeError('bus unavailable'))
        self.assertIn('bus unavailable', str(result))
        action.doAction.assert_not_called()

    def test_screencast_never_activates_a_stale_screenshot_allow(self):
        action, _, result, _ = self.consent(stale=True)
        self.assertIsInstance(result, SystemExit)
        self.assertEqual(result.code, 0)
        action.doAction.assert_called_once_with(0)
        action.stale.doAction.assert_not_called()

    def test_exited_requester_never_activates_any_accessible_picker(self):
        action, bus, result, _ = self.consent(expired=True)
        self.assertIsInstance(result, RuntimeError)
        self.assertIn('refusing a stale portal picker', str(result))
        action.doAction.assert_not_called()
        bus.call_sync.assert_not_called()


if __name__ == '__main__':
    unittest.main()
