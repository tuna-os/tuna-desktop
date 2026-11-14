#!/usr/bin/env python3
"""Pure logic of the upgrade/rollback/recovery proof (`tuna-corral-journey --upgrade-to`)."""
import contextlib
import importlib.machinery
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS / 'lib'))
import corral_upgrade as upgrade  # noqa: E402

SOURCE = 'ghcr.io/tuna-os/marlin@sha256:' + 'a' * 64
TARGET = 'ghcr.io/tuna-os/marlin@sha256:' + 'b' * 64


def load_journey():
    with patch.dict(sys.modules, {'PIL': SimpleNamespace(Image=Mock(), ImageChops=Mock())}):
        loader = importlib.machinery.SourceFileLoader('corral_journey_upgrade', str(SCRIPTS / 'tuna-corral-journey'))
        spec = importlib.util.spec_from_loader(loader.name, loader)
        module = importlib.util.module_from_spec(spec)
        loader.exec_module(module)
    return module


class FixtureTests(unittest.TestCase):
    def guest_home(self, token):
        """Run the guest snippets against a private home on this machine."""
        home = tempfile.TemporaryDirectory()
        self.addCleanup(home.cleanup)
        env = dict(os.environ, HOME=home.name)
        created = subprocess.run(['sh', '-c', upgrade.create_fixture_script(token)], env=env,
                                 capture_output=True, text=True, check=True)
        self.assertEqual(created.stdout.strip(), 'ok')
        listed = subprocess.run(['sh', '-c', upgrade.manifest_script(home.name)],
                                capture_output=True, text=True, check=True)
        return home.name, json.loads(listed.stdout)

    def test_guest_fixture_matches_host_expectations(self):
        home, manifest = self.guest_home('t0k3n')
        self.assertEqual(upgrade.fixture_mismatches(manifest, 't0k3n'), [])
        data = Path(home, upgrade.FIXTURE_DIR, 'nested/deeper/data.bin').read_bytes()
        self.assertEqual(data, upgrade.fixture_bytes('t0k3n'))
        self.assertEqual(len(data), upgrade.BINARY_SIZE)
        self.assertEqual(manifest['entries'][f'{upgrade.FIXTURE_DIR}/private.txt']['mode'], '0o600')

    def test_fixture_from_another_run_does_not_match(self):
        _, manifest = self.guest_home('first')
        paths = {problem['path'] for problem in upgrade.fixture_mismatches(manifest, 'second')}
        self.assertIn(f'{upgrade.FIXTURE_DIR}/notes.txt', paths)
        self.assertIn(f'{upgrade.FIXTURE_DIR}/nested/deeper/data.bin', paths)
        self.assertNotIn(f'{upgrade.FIXTURE_DIR}/latest', paths)

    def test_changed_mode_and_missing_link_are_fixture_mismatches(self):
        home, _ = self.guest_home('t')
        os.chmod(Path(home, upgrade.FIXTURE_DIR, 'private.txt'), 0o644)
        os.remove(Path(home, upgrade.FIXTURE_DIR, 'latest'))
        manifest = json.loads(subprocess.run(['sh', '-c', upgrade.manifest_script(home)],
                                             capture_output=True, text=True, check=True).stdout)
        problems = {p['path']: p['actual'] for p in upgrade.fixture_mismatches(manifest, 't')}
        self.assertEqual(problems[f'{upgrade.FIXTURE_DIR}/private.txt']['mode'], '0o644')
        self.assertIsNone(problems[f'{upgrade.FIXTURE_DIR}/latest'])

    def test_gsettings_fixture_is_stable_across_runs(self):
        self.assertEqual(upgrade.gsettings_fixture('a'), upgrade.gsettings_fixture('b'))


class ManifestTests(unittest.TestCase):
    def test_compare_reports_missing_changed_and_added(self):
        before = {'entries': {'a': {'type': 'file', 'sha256': '1'}, 'b': {'type': 'dir', 'mode': '0o755'},
                              'c': {'type': 'file', 'sha256': '3'}}}
        after = {'entries': {'a': {'type': 'file', 'sha256': '2'}, 'b': {'type': 'dir', 'mode': '0o755'},
                             'd': {'type': 'file', 'sha256': '4'}}}
        diff = upgrade.compare_manifests(before, after)
        self.assertEqual(diff['missing'], ['c'])
        self.assertEqual([c['path'] for c in diff['changed']], ['a'])
        self.assertEqual(diff['added'], ['d'])
        self.assertFalse(diff['preserved'])

    def test_additions_alone_preserve(self):
        before = {'entries': {'a': {'type': 'file', 'sha256': '1'}}}
        after = {'entries': {'a': {'type': 'file', 'sha256': '1'}, 'new': {'type': 'dir'}}}
        diff = upgrade.compare_manifests(before, after)
        self.assertTrue(diff['preserved'])
        self.assertEqual(diff['added'], ['new'])


class BootcTests(unittest.TestCase):
    STATUS = {
        'spec': {'image': {'image': TARGET, 'transport': 'registry'}},
        'status': {
            'staged': None,
            'booted': {'image': {'image': {'image': TARGET, 'transport': 'registry'},
                                 'imageDigest': 'sha256:' + 'c' * 64, 'version': '35dd842'},
                       'pinned': False, 'ostree': {'checksum': 'd' * 64}},
            'rollback': {'image': {'image': {'image': SOURCE}, 'imageDigest': 'sha256:' + 'e' * 64},
                         'pinned': False},
            'rollbackQueued': False,
        },
    }

    def test_projects_each_role(self):
        projection = upgrade.deployments(self.STATUS)
        self.assertEqual(projection['booted']['ref'], TARGET)
        self.assertEqual(projection['booted']['digest'], 'sha256:' + 'c' * 64)
        self.assertEqual(projection['rollback']['ref'], SOURCE)
        self.assertIsNone(projection['staged'])
        self.assertIs(projection['rollbackQueued'], False)
        self.assertTrue(upgrade.booted_is(self.STATUS, TARGET))
        self.assertFalse(upgrade.booted_is(self.STATUS, SOURCE))
        self.assertFalse(upgrade.booted_is({}, SOURCE))

    def test_refs_must_be_digest_pinned_and_distinct(self):
        self.assertIsNone(upgrade.validate_refs(SOURCE, TARGET))
        self.assertIn('unchanged', upgrade.validate_refs(SOURCE, SOURCE))
        self.assertIn('--upgrade-to', upgrade.validate_refs(SOURCE, 'ghcr.io/tuna-os/marlin:tuna'))
        self.assertIn('--image', upgrade.validate_refs('ghcr.io/tuna-os/marlin:tuna', TARGET))


class StoreTests(unittest.TestCase):
    def test_notification_token(self):
        text = json.dumps({'version': 1, 'history': [{'summary': 'Upgrade proof abc'}]})
        self.assertEqual(upgrade.notification_tokens(text, 'abc'), {'parsed': True, 'present': True, 'version': 1})
        self.assertFalse(upgrade.notification_tokens(text, 'xyz')['present'])
        self.assertEqual(upgrade.notification_tokens('{broken', 'abc'), {'parsed': False, 'present': False})
        self.assertFalse(upgrade.notification_tokens(None, 'abc')['present'])

    @staticmethod
    def history(*entries):
        return json.dumps([{'oldState': o, 'newState': n, 'wallTimeSecs': t} for o, n, t in entries])

    def test_history_retained_when_appended(self):
        before = self.history((0, 1, 100), (1, 0, 200))
        after = self.history((0, 1, 100), (1, 0, 200), (0, 1, 300))
        self.assertTrue(upgrade.history_retained(before, after, 400)['retained'])

    def test_history_drop_is_unexplained(self):
        result = upgrade.history_retained(self.history((0, 1, 100), (1, 0, 200), (0, 1, 250)),
                                          self.history((0, 1, 250)), 400)
        self.assertFalse(result['retained'])
        self.assertEqual([d['wallTimeSecs'] for d in result['unexplained_drops']], [100, 200])

    def test_history_expiry_and_equal_time_collapse_are_explained(self):
        old = 10_000_000
        now = old + upgrade.HISTORY_THRESHOLD_SECONDS + 10
        before = self.history((0, 1, old), (1, 0, now - 5))
        # The last entry pairs with a new same-second transition that cancels it.
        after = self.history((0, 1, now - 5))
        self.assertTrue(upgrade.history_retained(before, after, now)['retained'])

    def test_missing_history_after_is_a_drop(self):
        result = upgrade.history_retained(self.history((0, 1, 100)), None, 200)
        self.assertFalse(result['retained'])

    def test_gsettings_mismatches(self):
        expected = [('org.gnome.desktop.interface', 'color-scheme', "'prefer-dark'")]
        self.assertEqual(upgrade.gsettings_mismatches(
            expected, {'org.gnome.desktop.interface color-scheme': "'prefer-dark'"}), [])
        self.assertEqual(upgrade.gsettings_mismatches(expected, {})[0]['observed'], None)


class LockTests(unittest.TestCase):
    def test_locked_hint_required_where_the_compositor_sets_it(self):
        locked = {'active': '(true,)', 'locked_hint': 'yes', 'locked_hint_supported': True}
        self.assertTrue(upgrade.lock_matches(locked, True))
        self.assertFalse(upgrade.lock_matches(dict(locked, locked_hint='no'), True))
        self.assertFalse(upgrade.lock_matches(locked, False))
        unlocked = {'active': '(false,)', 'locked_hint': 'no', 'locked_hint_supported': True}
        self.assertTrue(upgrade.lock_matches(unlocked, False))
        self.assertFalse(upgrade.lock_matches(dict(unlocked, locked_hint='yes'), False))

    def test_beta2_compositor_never_sets_locked_hint(self):
        state = {'active': '(true,)', 'locked_hint': 'no', 'locked_hint_supported': False}
        self.assertTrue(upgrade.lock_matches(state, True))
        self.assertFalse(upgrade.lock_matches(dict(state, active='error 1: no owner'), True))
        self.assertFalse(upgrade.lock_matches(dict(state, active='(false,)'), True))


class SupervisionTests(unittest.TestCase):
    LINES = ['tuna-compositor: shell supervision: ShellExited { code: None }',
             'tuna-shell-gtk: keybindings: 40 grabs',
             'tuna-compositor: shell supervision: RestartScheduled { delay_ms: 250 }']

    def test_restart_observed(self):
        events = upgrade.supervision_events(self.LINES)
        self.assertEqual([e['event'] for e in events], ['ShellExited', 'RestartScheduled'])
        self.assertEqual(events[0]['detail'], '{ code: None }')
        self.assertTrue(upgrade.shell_restart_observed(events))

    def test_exhausted_or_unscheduled_is_not_a_restart(self):
        exhausted = upgrade.supervision_events(self.LINES + ['tuna-compositor: shell supervision: BudgetExhausted'])
        self.assertFalse(upgrade.shell_restart_observed(exhausted))
        self.assertFalse(upgrade.shell_restart_observed(upgrade.supervision_events(self.LINES[:1])))
        restart_before_exit = upgrade.supervision_events(list(reversed(self.LINES)))
        self.assertFalse(upgrade.shell_restart_observed(restart_before_exit))


class CommandLineTests(unittest.TestCase):
    def parse_error(self, *argv):
        journey = load_journey()
        with tempfile.TemporaryDirectory() as root, \
                patch.object(sys, 'argv', ['tuna-corral-journey', '--node', 'karnataka',
                                           '--out', str(Path(root, 'out')), *argv]), \
                contextlib.redirect_stderr(io.StringIO()) as stderr, \
                patch.object(journey, 'run', side_effect=AssertionError('no command may run')):
            with self.assertRaises(SystemExit) as raised:
                journey.main()
            self.assertFalse(Path(root, 'out').exists())
        self.assertEqual(raised.exception.code, 2)
        return stderr.getvalue()

    def test_identical_target_refused_before_any_vm_work(self):
        self.assertIn('unchanged', self.parse_error('--image', SOURCE, '--upgrade-to', SOURCE))

    def test_tag_target_refused(self):
        self.assertIn('pinned by digest', self.parse_error('--image', SOURCE, '--upgrade-to', 'ghcr.io/x/y:tuna'))

    def test_reuse_refused(self):
        self.assertIn('fresh install', self.parse_error('--image', SOURCE, '--upgrade-to', TARGET, '--reuse'))


if __name__ == '__main__':
    unittest.main()
