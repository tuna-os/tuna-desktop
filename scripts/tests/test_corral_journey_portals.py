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


class PortalSessionTests(unittest.TestCase):
    def test_records_unlocked_state_at_each_boundary(self):
        guest, record = Mock(), {}
        guest.sh.return_value = (0, '(false,)', '')
        for phase in ('before-request', 'after-response'):
            journey.require_portal_unlocked(guest, record, phase)
        self.assertEqual([s['phase'] for s in record['session_state']],
                         ['before-request', 'after-response'])
        self.assertTrue(all(s['stdout'] == '(false,)' for s in record['session_state']))
        self.assertTrue(all(call.kwargs['user'] for call in guest.sh.call_args_list))

    def test_locked_or_unreadable_session_is_a_precondition_failure(self):
        for result in ((0, '(true,)', ''), (1, '', 'no owner'), (124, '', 'timeout'),
                       (0, '', ''), (0, 'unexpected reply', '')):
            with self.subTest(result=result):
                guest, record = Mock(), {}
                guest.sh.return_value = result
                with self.assertRaisesRegex(RuntimeError, 'requires an unlocked session'):
                    journey.require_portal_unlocked(guest, record, 'before-request')
                self.assertEqual(record['session_state'][0]['exit'], result[0])
                self.assertEqual(record['session_state'][0]['stderr'], result[2])
                guest.sh.assert_called_once()

    def test_both_capture_paths_refuse_to_launch_behind_lock(self):
        for target in ('portal-screenshot', 'portal-screencast'):
            with self.subTest(target=target), tempfile.TemporaryDirectory() as tmp:
                guest, console, receipt = Mock(), Mock(), {}
                guest.sh.return_value = (0, '(true,)', '')
                run = journey.Journey(SimpleNamespace(), guest, console, receipt, Path(tmp))
                run.step = lambda sid, title, action: action(Mock()) if sid == target else None
                with self.assertRaisesRegex(RuntimeError, 'before-request requires an unlocked session'):
                    run.run()
                console.activity.assert_called_once()
                guest.sh.assert_called_once()
                self.assertIn('ScreenSaver.GetActive', guest.sh.call_args.args[0])
                self.assertNotIn('python3', guest.sh.call_args.args[0])

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

    def test_active_grant_is_locked_before_png_transfer_and_step_collection(self):
        for arm_exit in (0, 1):
            with self.subTest(arm_exit=arm_exit), tempfile.TemporaryDirectory() as tmp:
                events, checks, receipt = [], [], {}
                guest, console = Mock(), Mock()
                guest.out.side_effect = lambda cmd, **kw: events.append(('out', cmd)) or '1000'
                def shell(cmd, **kw):
                    events.append(('sh', cmd))
                    if 'ScreenSaver.GetActive' in cmd:
                        return 0, '(false,)', ''
                    if cmd.startswith('touch '):
                        return arm_exit, '', 'arm failed' if arm_exit else ''
                    if cmd.startswith('cat '):
                        return 0, '[]', ''
                    if cmd.startswith('test -f ') and ' || ' not in cmd and cmd.endswith('.exit'):
                        return 1, '', ''
                    return 0, '', ''
                guest.sh.side_effect = shell
                console.grab.side_effect = lambda path: path.write_bytes(b'prompt-frame')
                console.key.side_effect = lambda chord, **kw: events.append(('key', chord))
                run = journey.Journey(SimpleNamespace(), guest, console, receipt, Path(tmp))
                def step(sid, title, action):
                    if sid == 'portal-screencast':
                        action(lambda name, ok, observed: checks.append((name, ok)) or ok)
                        events.append(('collect', 'step screenshot/journal'))
                run.step = step
                run.run()
                arm = next(i for i, event in enumerate(events)
                           if event[0] == 'sh' and event[1].startswith('touch '))
                lock = events.index(('key', 'Meta+l'))
                self.assertLess(arm, lock)
                self.assertLess(lock, events.index(('collect', 'step screenshot/journal')))
                self.assertFalse(any(event[0] == 'out' and event[1].startswith(('sha256sum ', 'base64 '))
                                     for event in events))
                self.assertEqual(sum(event == ('key', 'Meta+l') for event in events), 1)
                self.assertEqual(receipt['portal_screencast']['lock_input']['observer_armed'],
                                 arm_exit == 0)
                self.assertEqual(dict(checks)['arm lock stream-withdrawal observation'], arm_exit == 0)


class ConsentVisibilityTests(unittest.TestCase):
    def consent(self, active=False, error=None):
        action = Mock()
        button = SimpleNamespace(name='Share', childCount=0,
                                 getRoleName=lambda: 'push button',
                                 getState=lambda: SimpleNamespace(contains=lambda state: True),
                                 queryAction=lambda: action)
        app = SimpleNamespace(name='xdg-desktop-portal-gnome', childCount=1,
                              getRoleName=lambda: 'application',
                              getState=lambda: SimpleNamespace(contains=lambda state: False),
                              getChildAtIndex=lambda index: button)
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
            with patch.dict(sys.modules, modules), patch('sys.argv', [
                    'tuna-portal-consent.py', str(out), 'grant', '--external-capture']), \
                    contextlib.redirect_stdout(io.StringIO()):
                try:
                    runpy.run_path(str(SCRIPTS / 'lib/tuna-portal-consent.py'), run_name='__main__')
                except (RuntimeError, SystemExit) as result:
                    return action, bus, result, json.loads(out.read_text())
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


if __name__ == '__main__':
    unittest.main()
