"""Owned host process controls and guest helper source tests; no installed GTK proof."""
import ast
import copy
import io
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import threading
import time
from types import SimpleNamespace
import unittest
from unittest import mock
import hashlib

ROOT = Path(__file__).resolve().parents[2]
SOURCE = ROOT / 'packaging/marlin/vm-lane/tuna-vm-lifecycle'
TREE = ast.parse(SOURCE.read_text())
FUNCTIONS = [node for node in TREE.body if isinstance(node, ast.FunctionDef) and node.name.startswith('strip_')]


def helper():
    scope = {'os': os, 'pathlib': __import__('pathlib'), 'json': json,
             'file_stat': stat, 'time': time, 'hashlib': hashlib,
             'OWNER': SimpleNamespace(pw_uid=os.getuid())}
    exec(compile(ast.Module(body=FUNCTIONS, type_ignores=[]), str(SOURCE), 'exec'), scope)
    return scope


class Tests(unittest.TestCase):
    def test_nonce_rejects_unbounded_or_path_input(self):
        parse = helper()['strip_nonce']
        self.assertEqual(parse('a' * 32), 'a' * 32)
        for bad in ('', 'a' * 33, '../receipt', 'a' * 31 + '\n', 'A' * 32):
            with self.subTest(bad=bad), self.assertRaises(ValueError):
                parse(bad)

    def test_actual_receipt_bytes_owner_symlink_and_mode_bounds(self):
        read = helper()['strip_json']
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'receipt.json'
            path.write_text('{"nonce":"owned"}')
            path.chmod(0o600)
            self.assertEqual(read(path, os.getuid()), {'nonce': 'owned'})
            with self.assertRaises(RuntimeError):
                read(path, os.getuid() + 1)
            path.chmod(0o666)
            with self.assertRaises(RuntimeError):
                read(path, os.getuid())
            path.chmod(0o600)
            path.write_bytes(b'x' * 131073)
            with self.assertRaisesRegex(RuntimeError, 'byte bound'):
                read(path, os.getuid())
            link = Path(directory) / 'link'
            link.symlink_to(path)
            with self.assertRaises(OSError):
                read(link, os.getuid())

    def process(self):
        child = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(15)'],
                                 stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                 stderr=subprocess.DEVNULL)
        def close():
            if child.poll() is None:
                child.kill()
            child.wait(timeout=3)
        self.addCleanup(close)
        return child

    def test_actual_process_identity_rejects_wrong_uid(self):
        child = self.process()
        identify = helper()['strip_identity']
        actual = identify(child.pid, os.getuid())
        self.assertEqual(actual['pid'], child.pid)
        self.assertEqual(actual['boot_id'], Path('/proc/sys/kernel/random/boot_id').read_text().strip())
        self.assertEqual(actual['exe_sha256'], hashlib.sha256(Path(f'/proc/{child.pid}/exe').read_bytes()).hexdigest())
        with self.assertRaises(RuntimeError):
            identify(child.pid, os.getuid() + 1)

    def test_actual_pidfd_stop_reaps_only_original_owned_process(self):
        child = self.process()
        scope = helper()
        identity = scope['strip_identity'](child.pid, os.getuid())
        errors = []
        def reap():
            try:
                child.wait(timeout=4)
            except BaseException as error:
                errors.append(error)
        thread = threading.Thread(target=reap)
        thread.start()
        try:
            result = scope['strip_stop_identity'](identity, os.getuid(), time.monotonic() + 3)
            self.assertTrue(result['retired'])
            self.assertTrue(result['proc_absent'])
        finally:
            thread.join(4)
        self.assertFalse(thread.is_alive())
        self.assertEqual(errors, [])

    def test_changed_generation_refuses_signal_and_leaves_actual_process_alive(self):
        child = self.process()
        scope = helper()
        identity = scope['strip_identity'](child.pid, os.getuid())
        stale = copy.deepcopy(identity)
        stale['start_ticks'] += 1
        with self.assertRaisesRegex(RuntimeError, 'changed strip process identity'):
            scope['strip_stop_identity'](stale, os.getuid(), time.monotonic() + 2)
        self.assertIsNone(child.poll())
        with self.assertRaises(TimeoutError):
            scope['strip_stop_identity'](identity, os.getuid(), time.monotonic() - 1)
        self.assertIsNone(child.poll())

    def test_exit_during_identity_read_is_retirement_not_failure(self):
        # A launcher retires as soon as its client does: it can exit while
        # its identity is read. That exit is cleanup, never a signal.
        child = self.process()
        scope = helper()
        identity = scope['strip_identity'](child.pid, os.getuid())
        def exiting(pid, uid):
            child.kill()
            child.wait(timeout=4)
            raise RuntimeError('strip process changed during identity read')
        scope['strip_identity'] = exiting
        result = scope['strip_stop_identity'](identity, os.getuid(), time.monotonic() + 3)
        self.assertTrue(result['retired'])
        self.assertTrue(result['proc_absent'])

    def test_failed_identity_read_of_a_live_process_still_refuses(self):
        child = self.process()
        scope = helper()
        identity = scope['strip_identity'](child.pid, os.getuid())
        def unreadable(pid, uid):
            raise RuntimeError('strip process owner/liveness mismatch')
        scope['strip_identity'] = unreadable
        with self.assertRaisesRegex(RuntimeError, 'owner/liveness mismatch'):
            scope['strip_stop_identity'](identity, os.getuid(), time.monotonic() + 2)
        self.assertIsNone(child.poll())

    def test_actual_owned_process_argv_and_typed_mode_are_bound(self):
        scope = helper()
        with tempfile.TemporaryDirectory() as directory:
            script = Path(directory) / 'owned-mode-fixture.py'
            script.write_text('import time; time.sleep(15)')
            scope['STRIP_EXE'] = script
            nonce = 'a' * 32
            child = subprocess.Popen([sys.executable, str(script), '--strip-edge-fixture', nonce],
                stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            try:
                identity = scope['strip_identity'](child.pid, os.getuid())
                value = {'schema': 1, 'nonce': nonce, 'backend': 'wayland', 'fixture_mode': 'edge',
                         'identity': identity | {'script_sha256': scope['strip_sha'](script)}}
                actual = scope['strip_validate_client'](value, nonce, None, 'edge')
                self.assertEqual(actual, value['identity'])
                for bad in ('baseline', 'unknown', True, None):
                    with self.subTest(mode=bad), self.assertRaises(RuntimeError):
                        scope['strip_validate_client'](value, nonce, None, bad)
                # Matching declared baseline still cannot disguise actual edge argv.
                value['fixture_mode'] = 'baseline'
                with self.assertRaisesRegex(RuntimeError, 'actual admitted script invocation'):
                    scope['strip_validate_client'](value, nonce, None, 'baseline')
            finally:
                if child.poll() is None: child.kill()
                child.wait(timeout=2)

    def test_default_count_client_is_retained_and_explicit_mode_uses_real_gdk(self):
        source = (ROOT / 'packaging/marlin/vm-lane/tuna-vm-input').read_text()
        tree = ast.parse(source)
        fixture = next(node for node in tree.body if isinstance(node, ast.FunctionDef) and node.name == 'strip_fixture')
        body = ast.get_source_segment(source, fixture)
        self.assertIn('Gdk.Toplevel.begin_move(surface, device, button, x, y, event.get_time())', body)
        self.assertIn('Gdk.Toplevel.begin_resize(surface, Gdk.SurfaceEdge.SOUTH_EAST', body)
        self.assertIn('"after-paint"', body)
        self.assertIn('gi.require_foreign("cairo")', body)
        container = (ROOT / 'packaging/marlin/vm-lane/Containerfile').read_text()
        transaction = next(line for line in container.splitlines() if line.startswith('RUN pacman -Syu'))
        self.assertIn('python-cairo', transaction.split())
        self.assertIn('WaylandToplevel.set_application_id', body)
        self.assertIn('Gtk.Application(application_id="org.tuna.VmInput")', source)
        self.assertNotIn('gsettings', body)
        self.assertNotIn('subprocess', body)
        self.assertNotIn('tuna-vm-state', body)

    def test_role_id_is_applied_after_map_and_reapplied_on_remap(self):
        # Execute the actual callback against an explicit synthetic GDK edge;
        # native xdg_toplevel IDs still require the installed CI observations.
        source = (ROOT / 'packaging/marlin/vm-lane/tuna-vm-input').read_text()
        fixture = next(node for node in ast.parse(source).body
                       if isinstance(node, ast.FunctionDef) and node.name == 'strip_fixture')
        callbacks = {node.name: node for node in ast.walk(fixture)
                     if isinstance(node, ast.FunctionDef)}
        self.assertNotIn('set_application_id', ast.get_source_segment(source, callbacks['realized']))
        self.assertIn('window.connect("map", mapped)', ast.get_source_segment(source, fixture))
        calls = []
        class Surface:
            @staticmethod
            def set_application_id(surface, value):
                calls.append((surface, value))
        surface = Surface()
        scope = {'app_id': 'org.tuna.VmStrip.n' + 'a' * 32 + '.alpha',
                 'GdkWayland': SimpleNamespace(WaylandToplevel=Surface)}
        exec(compile(ast.Module(body=[callbacks['mapped']], type_ignores=[]), str(SOURCE), 'exec'), scope)
        widget = SimpleNamespace(get_surface=lambda: surface)
        scope['mapped'](widget)
        scope['mapped'](widget)
        self.assertEqual(calls, [(surface, scope['app_id'])] * 2)
        with self.assertRaisesRegex(RuntimeError, 'map actual Wayland'):
            scope['mapped'](SimpleNamespace(get_surface=lambda: object()))


if __name__ == '__main__':
    unittest.main()
