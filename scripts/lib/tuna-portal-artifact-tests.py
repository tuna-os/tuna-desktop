#!/usr/bin/env python3
"""Exercise the actual FD-based portal artifact finalizer with bounded failures."""
from contextlib import nullcontext
import importlib.util
import os
from pathlib import Path
import tempfile
import subprocess
import sys
import time
import unittest
from unittest.mock import patch
from tuna_picker_geometry import application_id, committed_window, new_parent_window

spec = importlib.util.spec_from_file_location('finalizer', Path(__file__).with_name('tuna-portal-artifact-finalize.py'))
f = importlib.util.module_from_spec(spec)
spec.loader.exec_module(f)

retire_spec = importlib.util.spec_from_file_location('retirement', Path(__file__).with_name('tuna-portal-artifact-retire.py'))
r = importlib.util.module_from_spec(retire_spec)
retire_spec.loader.exec_module(r)


class FinalizationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        for name in f.REQUIRED:
            (self.root / name).write_text('actual proof receipt\n')
        self.addCleanup(self.temp.cleanup)

    def run_finalizer(self, status=0, chown=None, stat_hook=None, **bounds):
        original_stat, original_fstat = os.stat, os.fstat
        def owned(function):
            def result(*args, **kwargs):
                values = list(function(*args, **kwargs))
                values[4:6] = [0, 0]
                result = os.stat_result(values)
                return stat_hook(result, args) if stat_hook else result
            return result
        with patch.object(f.os, 'chown', side_effect=chown), patch.object(f.os, 'fchown'), \
                patch.object(f.os, 'stat', side_effect=owned(original_stat)), \
                patch.object(f.os, 'fstat', side_effect=owned(original_fstat)), \
                (patch.multiple(f, **bounds) if bounds else nullcontext()):
            return f.finalize(str(self.root), status)

    def test_complete_receipts(self):
        result = self.run_finalizer()
        self.assertTrue(result['qualified_finalization'])
        self.assertEqual(len(result['seen']), 4)
        self.assertTrue(all(row['finalized'] and 'observed_device' in row for row in result['seen']))

    def test_observed_regular_gvfs_rotation_only(self):
        directory = self.root / 'data/gvfs-metadata'
        directory.mkdir(parents=True)
        log = directory / 'root-4e9e4e19.log'
        log.write_text('rotating log')
        def disappear(name, *args, **kwargs):
            if name == log.name:
                os.unlink(name, dir_fd=kwargs['dir_fd'])
                raise FileNotFoundError(2, 'rotation', name)
        result = self.run_finalizer(chown=disappear)
        self.assertTrue(result['qualified_finalization'])
        self.assertEqual(result['optional_disappearances'][0]['path'], 'data/gvfs-metadata/root-4e9e4e19.log')

    def test_other_missing_file_fails(self):
        def disappear(name, *args, **kwargs):
            if name == 'assertions.txt':
                os.unlink(name, dir_fd=kwargs['dir_fd'])
                raise FileNotFoundError(2, 'missing', name)
        result = self.run_finalizer(chown=disappear)
        self.assertFalse(result['qualified_finalization'])
        self.assertEqual(result['optional_disappearances'], [])

    def test_permission_failure_preserved(self):
        def denied(*args, **kwargs):
            raise PermissionError(1, 'permission denied')
        result = self.run_finalizer(23, denied)
        self.assertFalse(result['qualified_finalization'])
        self.assertEqual(result['proof_status'], 23)
        self.assertTrue((self.root / 'artifact-finalization-error.json').is_file())

    def test_empty_required_receipt_fails(self):
        (self.root / 'assertions.txt').write_text('')
        self.assertFalse(self.run_finalizer()['qualified_finalization'])

    def test_replaced_required_receipt_fails(self):
        def replace(name, *args, **kwargs):
            if name == 'session-identity.txt':
                replacement = self.root / 'replacement'
                replacement.write_text('replacement')
                os.replace(replacement, self.root / 'assertions.txt')
        self.assertFalse(self.run_finalizer(chown=replace)['qualified_finalization'])

    def test_symlink_target_never_changed(self):
        outside = self.root.parent / (self.root.name + '-outside')
        outside.write_text('outside')
        self.addCleanup(outside.unlink)
        (self.root / 'link').symlink_to(outside)
        calls = []
        def record(name, *args, **kwargs):
            calls.append((name, kwargs['follow_symlinks']))
        self.assertTrue(self.run_finalizer(chown=record)['qualified_finalization'])
        self.assertEqual(outside.read_text(), 'outside')
        self.assertIn(('link', False), calls)

    def test_entry_bound_retains_failure_receipt(self):
        result = self.run_finalizer(MAX_ENTRIES=1)
        self.assertFalse(result['qualified_finalization'])
        self.assertLessEqual(len(result['seen']), 1)
        self.assertTrue((self.root / 'artifact-finalization-error.json').is_file())

    def test_wrong_gid_fails(self):
        def wrong_gid(result, args):
            values = list(result)
            values[5] = 1000
            return os.stat_result(values)
        self.assertFalse(self.run_finalizer(stat_hook=wrong_gid)['qualified_finalization'])

    def test_device_change_fails(self):
        counts = {}
        def changed(result, args):
            if args and args[0] == 'assertions.txt':
                counts['file'] = counts.get('file', 0) + 1
                if counts['file'] > 1:
                    values = list(result)
                    values[2] += 1
                    return os.stat_result(values)
            return result
        self.assertFalse(self.run_finalizer(stat_hook=changed)['qualified_finalization'])

    def test_actual_wrapper_preserves_primary_nonzero_status(self):
        for name in ('run.sh', 'settings-run.sh', 'global-shortcuts-run.sh',
                     'backgrounds-run.sh', 'slideshow-run.sh', 'svg-run.sh'):
            wrapper = (Path(__file__).parents[2] / 'tests/portal-reference' / name).read_text()
            start = wrapper.index('finalization=0')
            tail = wrapper[start:wrapper.index('fi\n', start)]
            for proof, cleanup, expected in ((23, 1, 23), (0, 1, 1), (0, 0, 0)):
                code = f'status={proof}\npython3() {{ return {cleanup}; }}\n' + tail
                result = subprocess.run(['bash', '-c', code], capture_output=True, timeout=60)
                self.assertEqual(result.returncode, expected, (name, proof, cleanup, result.stderr))

    def test_root_symlink_refused(self):
        link = self.root.parent / (self.root.name + '-link')
        link.symlink_to(self.root, target_is_directory=True)
        self.addCleanup(link.unlink)
        with self.assertRaises(OSError):
            f.finalize(str(link), 0)

    def test_error_bound_retains_failure_receipt(self):
        def denied(*args, **kwargs):
            raise PermissionError(1, 'denied')
        result = self.run_finalizer(chown=denied, MAX_ERRORS=1)
        self.assertEqual(len(result['errors']), 1)
        self.assertFalse(result['qualified_finalization'])
        self.assertTrue((self.root / 'artifact-finalization-error.json').is_file())


class FixtureRetirementTests(unittest.TestCase):
    FIXTURES = ('backgrounds-run.sh', 'slideshow-run.sh', 'svg-run.sh')
    ROOT = Path(__file__).parents[2]

    def start_child(self, body):
        child = subprocess.Popen([sys.executable, '-u', '-c', body],
                                 stdout=subprocess.PIPE, text=True)
        self.addCleanup(lambda: child.poll() is None and child.kill())
        self.addCleanup(lambda: child.stdout.close())
        self.assertEqual(child.stdout.readline().strip(), 'ready')
        return child

    def test_real_child_exit_is_awaited(self):
        child = self.start_child('import signal,time\n'
            'signal.signal(signal.SIGTERM, lambda *_: (time.sleep(.15), exit(0)))\n'
            'print("ready", flush=True)\nwhile True: time.sleep(.01)')
        start = time.monotonic()
        r.retire(os.getpid(), [child.pid], timeout=2)
        self.assertGreaterEqual(time.monotonic() - start, .1)
        self.assertEqual(child.wait(timeout=1), 0)

    def test_foreign_parent_is_rejected_without_signalling(self):
        child = self.start_child('import time\nprint("ready", flush=True)\ntime.sleep(30)')
        with self.assertRaisesRegex(RuntimeError, 'not this fixture'):
            r.retire(os.getpid() + 1, [child.pid], timeout=.1)
        self.assertIsNone(child.poll())
        child.terminate()
        child.wait(timeout=1)

    def test_changed_identity_is_rejected_before_any_signal(self):
        child = self.start_child('import time\nprint("ready", flush=True)\ntime.sleep(30)')
        original = r.identity(child.pid)
        with patch.object(r, 'identity', side_effect=[original, (original[0], 'changed', original[2])]):
            with self.assertRaisesRegex(RuntimeError, 'changed identity'):
                r.retire(os.getpid(), [child.pid], timeout=.1)
        self.assertIsNone(child.poll())
        child.terminate()
        child.wait(timeout=1)

    def test_timeout_does_not_force_kill_supervisor(self):
        child = self.start_child('import signal,time\n'
            'signal.signal(signal.SIGTERM, signal.SIG_IGN)\n'
            'print("ready", flush=True)\ntime.sleep(30)')
        start = time.monotonic()
        with self.assertRaisesRegex(RuntimeError, 'did not retire'):
            r.retire(os.getpid(), [child.pid], timeout=.1)
        self.assertLess(time.monotonic() - start, 1)
        self.assertIsNone(child.poll())
        child.kill()
        child.wait(timeout=1)

    def test_actual_cleanup_waits_for_supervised_wallpaper_writer(self):
        # Real local subprocesses only: no host compositor, bus, PAM or session.
        supervisor = '''import pathlib,signal,subprocess,sys,time
root=pathlib.Path(sys.argv[1])
writer=subprocess.Popen([sys.executable,'-c',
    'import pathlib,sys,time; p=pathlib.Path(sys.argv[1]); '
    'exec("while True:\\n (p/\\".tuna-wallpaper.tmp\\").write_text(\\"pixels\\"); (p/\\".tuna-wallpaper.tmp\\").replace(p/\\"tuna-wallpaper\\"); time.sleep(.001)")',str(root)])
stopping=False
def stop(*_):
    global stopping
    stopping=True
signal.signal(signal.SIGTERM,stop)
(root/'ready').write_text('ready')
while not stopping: time.sleep(.01)
time.sleep(.15)
writer.terminate()
writer.wait()
if (root/'x-retired').exists(): raise RuntimeError('X server retired before compositor')
(root/'writer-retired').write_text('reaped')
'''
        for name in self.FIXTURES:
            with self.subTest(fixture=name), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                (root / 'supervisor.py').write_text(supervisor)
                (root / 'xserver.py').write_text('import pathlib,signal,sys,time\n'
                    'root=pathlib.Path(sys.argv[1])\n'
                    'signal.signal(signal.SIGTERM, lambda *_: ((root/"x-retired").write_text("done"), exit(0)))\n'
                    '(root/"x-ready").write_text("ready")\n'
                    'while True: time.sleep(.01)\n')
                wrapper = (self.ROOT / 'tests/portal-reference' / name).read_text()
                cleanup = wrapper[wrapper.index('cleanup() {'):wrapper.index('trap cleanup EXIT')]
                cleanup = cleanup.replace('/repo/', str(self.ROOT) + '/').replace('/out/', str(root) + '/')
                command = (cleanup + '\ntrap cleanup EXIT\n' +
                    f'{sys.executable} "{root}/xserver.py" "{root}" >/dev/null 2>&1 &\n' +
                    f'while [ ! -s "{root}/x-ready" ]; do sleep .01; done\n' +
                    f'{sys.executable} "{root}/supervisor.py" "{root}" >/dev/null 2>&1 & compositor_pid=$!\n' +
                    f'while [ ! -s "{root}/ready" ]; do sleep .01; done\nexit 0\n')
                result = subprocess.run(['bash', '-c', command], capture_output=True, text=True, timeout=5)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual((root / 'writer-retired').read_text(), 'reaped')
                self.assertTrue((root / 'x-retired').is_file())
                self.assertIn('"qualified_retirement": true', (root / 'process-retirement.json').read_text())

    def test_abnormally_exited_compositor_cannot_qualify_retirement(self):
        for name in self.FIXTURES:
            with self.subTest(fixture=name), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                wrapper = (self.ROOT / 'tests/portal-reference' / name).read_text()
                cleanup = wrapper[wrapper.index('cleanup() {'):wrapper.index('trap cleanup EXIT')]
                cleanup = cleanup.replace('/repo/', str(self.ROOT) + '/').replace('/out/', str(root) + '/')
                command = (cleanup + '\ntrap cleanup EXIT\n' +
                    '(exit 7) & compositor_pid=$!\nsleep .1\nexit 0\n')
                result = subprocess.run(['bash', '-c', command], capture_output=True, text=True, timeout=5)
                self.assertEqual(result.returncode, 7, result.stderr)
                self.assertIn('"qualified_retirement":false', (root / 'process-retirement.json').read_text())

    def test_failed_retirement_never_starts_ownership_traversal(self):
        for name in self.FIXTURES:
            wrapper = (self.ROOT / 'tests/portal-reference' / name).read_text()
            start = wrapper.index('    # Never traverse artifacts')
            end = wrapper.index('\nfi\n', start)
            for proof in (0, 23):
                code = (f'status={proof}\njq() {{ return 1; }}\n'
                        'python3() { echo FINALIZER_RAN; return 0; }\n' + wrapper[start:end])
                result = subprocess.run(['bash', '-c', code], capture_output=True, text=True, timeout=60)
                self.assertEqual(result.returncode, proof or 1, (name, proof))
                self.assertNotIn('FINALIZER_RAN', result.stdout)

    def test_cleanup_failure_preserves_primary_proof_status(self):
        for name in self.FIXTURES:
            wrapper = (self.ROOT / 'tests/portal-reference' / name).read_text()
            start = wrapper.index('cleanup() {')
            cleanup = wrapper[start:wrapper.index('trap cleanup EXIT', start)]
            with tempfile.TemporaryDirectory() as directory:
                cleanup = cleanup.replace('/out/', directory + '/')
                for proof in (0, 23):
                    code = ('python3() { return 1; }\n' + cleanup +
                            f'\ntrap cleanup EXIT\nexit {proof}\n')
                    result = subprocess.run(['bash', '-c', code], capture_output=True, text=True, timeout=60)
                    self.assertEqual(result.returncode, proof or 1, (name, proof, result.stderr))


class PickerParentAdmissionTests(unittest.TestCase):
    def test_recycled_xid_does_not_admit_previous_callers_compositor_id(self):
        old = dict(id=17, x11_window_id=4194309, app_id="tuna-x11-picker-parent")
        new = dict(old, id=20)
        identity = dict(mode="x11", xid=4194309, app_id=None)
        self.assertIsNone(new_parent_window(dict(windows=[old]), {1, 17, 18}, **identity))
        self.assertIs(new_parent_window(dict(windows=[old, new]), {1, 17, 18}, **identity), new)
        self.assertIs(new_parent_window(dict(windows=[new]), {1, 17, 18}, **identity), new)

    def test_same_wayland_app_id_requires_a_new_compositor_window(self):
        old = dict(id=17, x11_window_id=None, app_id="org.example.TunaWaylandPickerParent")
        new = dict(old, id=20)
        identity = dict(mode="wayland", xid=None, app_id=old["app_id"])
        self.assertIsNone(new_parent_window(dict(windows=[old]), {17}, **identity))
        self.assertIs(new_parent_window(dict(windows=[old, new]), {17}, **identity), new)

    def test_duplicate_fresh_identity_wrong_transport_and_invalid_ids_are_rejected(self):
        window = dict(id=20, x11_window_id=4194309, app_id="parent")
        identity = dict(mode="x11", xid=4194309, app_id=None)
        self.assertIsNone(new_parent_window(dict(windows=[window, dict(window, id=21)]), set(), **identity))
        for changed in (dict(window, id=True), dict(window, id=0), dict(window, x11_window_id=None)):
            self.assertIsNone(new_parent_window(dict(windows=[changed]), set(), **identity))
        self.assertIsNone(new_parent_window(dict(windows=[window]), set(), mode="wayland", xid=None, app_id="parent"))
        for xid in (None, True, 0, "4194309"):
            self.assertIsNone(new_parent_window(dict(windows=[window]), set(), mode="x11", xid=xid, app_id=None))

    def test_actual_client_snapshots_before_creating_parent_and_uses_admission(self):
        client = Path(__file__).with_name("tuna-portal-filechooser-client.py").read_text()
        self.assertLess(client.index("previous_parent_ids ="), client.index("parent_window = Gtk.Window("))
        self.assertIn("new_parent_window(scene(), previous_parent_ids,", client)
        self.assertIn('"id": candidate["id"]', client)
        self.assertNotIn("candidates = [w for w in scene()", client)


class PickerGeometryTests(unittest.TestCase):
    def setUp(self):
        self.identity = dict(pid=711, uid=1000, start_time="29468", parent_id=17)
        self.record = dict(pid=711, uid=1000, start_time="29468",
            app_id=application_id(711, "29468"), mapped=True, window_size=[400, 300])
        self.window = dict(id=18, app_id=self.record["app_id"],
            x11_window_id=None, rect=[240, 116, 800, 600])
        self.scene = dict(focused=18, windows=[self.window])

    def ready(self):
        return committed_window(self.scene, self.record, **self.identity)

    def test_placeholder_is_not_a_committed_input_target(self):
        self.assertIsNone(self.ready())
        self.window["rect"] = [440, 266, 400, 300]
        self.assertIs(self.ready(), self.window)
        self.assertEqual([self.window["rect"][0] + 50,
                          self.window["rect"][1] + 100], [490, 366])

    def test_same_title_and_geometry_from_other_process_are_not_ready(self):
        self.window.update(title="Unrelated picker focus control",
            app_id=application_id(712, "29469"), rect=[440, 266, 400, 300])
        self.assertIsNone(self.ready())
        self.record["pid"] = 712
        with self.assertRaisesRegex(RuntimeError, "identity changed"):
            self.ready()

    def test_duplicate_identity_is_not_a_unique_input_target(self):
        self.window["rect"] = [440, 266, 400, 300]
        self.scene["windows"].append(dict(self.window, id=19))
        self.assertIsNone(self.ready())

    def test_unmapped_unfocused_and_invalid_size_are_not_ready(self):
        self.window["rect"] = [440, 266, 400, 300]
        self.record["mapped"] = False
        self.assertIsNone(self.ready())
        self.record["mapped"] = True
        self.scene["focused"] = 17
        self.assertIsNone(self.ready())
        self.scene["focused"] = 18
        self.record["window_size"] = [0, 300]
        self.assertIsNone(self.ready())


if __name__ == '__main__':
    unittest.main()
