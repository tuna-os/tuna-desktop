#!/usr/bin/env python3
"""Controlled unit regressions only, never installed qualification evidence."""
import contextlib
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
from types import SimpleNamespace
import subprocess
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('ime_admission', Path(__file__).with_name('tuna-ime-admission-proof.py'))
M = importlib.util.module_from_spec(spec)
spec.loader.exec_module(M)
SEAT = "interface: 'wl_seat', version: 9, name: 4\n"
MANAGER = "interface: 'zwp_input_method_manager_v2', version: 1, name: 8\n"
CONTEXT = 'tuna-ibus-bridge: input context /org/freedesktop/IBus/InputContext_17\n'
DENIED = CONTEXT + M.DENIAL + '\n'


class AdmissionProof(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.base = Path(self.tmp.name)
        self.source = self.base / 'installed-bridge'
        # Deliberately not a runnable ELF: subprocess responses are unit-controlled.
        self.payload = b'\x7fELFunit-controlled-admitted-bytes'
        self.source.write_bytes(self.payload)
        self.source.chmod(0o700)
        self.expected = hashlib.sha256(self.payload).hexdigest()
        self.out = self.base / 'owned'
        self.clock = 100.0
        self.calls = []
        self.registry = SEAT
        self.registry_status = 0
        self.helper_stderr = DENIED
        self.helper_stdout = ''
        self.helper_status = 1
        self.durations = [1.0, 0.25]
        self.timeout_on = None
        self.env = {'XDG_RUNTIME_DIR': '/run/user/1000',
                    'DBUS_SESSION_BUS_ADDRESS': 'unix:path=/run/user/1000/bus',
                    'WAYLAND_SOCKET': '91', 'WAYLAND_DISPLAY': 'outer-display',
                    'DISPLAY': ':7', 'IBUS_ADDRESS': 'wrong-bus', 'WAYLAND_DEBUG': '1'}

    def command(self, args, **kwargs):
        index = len(self.calls)
        self.calls.append((args, kwargs))
        self.clock += self.durations[index]
        stdout = self.registry if index == 0 else self.helper_stdout
        stderr = '' if index == 0 else self.helper_stderr
        kwargs['stdout'].write(stdout.encode())
        kwargs['stdout'].flush()
        kwargs['stderr'].write(stderr.encode())
        kwargs['stderr'].flush()
        if self.timeout_on == index:
            raise subprocess.TimeoutExpired(args, kwargs['timeout'])
        return SimpleNamespace(returncode=self.registry_status if index == 0 else self.helper_status)

    @contextlib.contextmanager
    def controlled(self):
        lstat = Path.lstat
        def owned_stat(path, *args, **kwargs):
            actual = lstat(path, *args, **kwargs)
            if path == self.out:
                return SimpleNamespace(st_mode=actual.st_mode, st_uid=1000)
            return actual
        with patch.object(M, 'SOURCE', self.source), \
                patch.object(M.os, 'getuid', return_value=1000), \
                patch.object(M.os, 'geteuid', return_value=1000), \
                patch.object(Path, 'lstat', owned_stat), \
                patch.dict(os.environ, self.env, clear=True), \
                patch.object(M.time, 'monotonic', side_effect=lambda: self.clock), \
                patch.object(M.subprocess, 'run', side_effect=self.command):
            yield

    def prove(self, expected=None):
        with self.controlled():
            return M.prove(self.out, 'actual-public-display', expected or self.expected)

    def receipt(self):
        return json.loads((self.out / 'receipt.json').read_text())

    def rejected(self, error):
        with self.assertRaisesRegex(ValueError, error):
            self.prove()
        self.assertFalse(self.receipt()['verified'])
        self.assertIn('error', self.receipt())

    def test_success_binds_exact_copy_registry_exit_and_outputs(self):
        receipt = self.prove()
        self.assertEqual(receipt, self.receipt())
        self.assertTrue(receipt['verified'])
        self.assertEqual(receipt['scope'], 'public-input-method-denial-only')
        self.assertEqual(receipt['admitted_bridge_sha256'], self.expected)
        self.assertEqual(receipt['source'], {'path': str(self.source), 'sha256': self.expected})
        copied = self.out / 'bridge/tuna-ibus-bridge'
        self.assertEqual(copied.read_bytes(), self.payload)
        self.assertEqual(copied.stat().st_mode & 0o7777, 0o700)
        self.assertEqual(receipt['copy'], {'path': str(copied), 'sha256': self.expected})
        self.assertEqual(receipt['wayland_display'], 'actual-public-display')
        self.assertEqual(receipt['registry']['stdout'], SEAT)
        self.assertTrue(receipt['registry']['wl_seat'])
        self.assertFalse(receipt['registry']['zwp_input_method_manager_v2'])
        helper = receipt['copied_helper']
        self.assertEqual(helper['exit_status'], 1)
        self.assertEqual(helper['stderr'], DENIED)
        self.assertEqual(helper['input_context'], '/org/freedesktop/IBus/InputContext_17')
        self.assertEqual(helper['denial'], M.DENIAL)
        self.assertEqual((self.out / 'wayland-info.txt').read_text(), SEAT)
        self.assertEqual((self.out / 'copied-helper.stderr').read_text(), DENIED)
        self.assertEqual((self.out / 'copied-helper.stdout').read_text(), '')
        self.assertEqual(len(self.calls), 2)
        self.assertEqual(self.calls[0][0], ['/usr/bin/wayland-info'])
        self.assertEqual(self.calls[1][0], [str(copied)])
        for _, kwargs in self.calls:
            self.assertEqual(kwargs['env']['WAYLAND_DISPLAY'], 'actual-public-display')
            for key in ('WAYLAND_SOCKET', 'DISPLAY', 'IBUS_ADDRESS', 'WAYLAND_DEBUG'):
                self.assertNotIn(key, kwargs['env'])
            for key in ('XDG_RUNTIME_DIR', 'DBUS_SESSION_BUS_ADDRESS'):
                self.assertEqual(kwargs['env'][key], self.env[key])
            self.assertEqual(kwargs['stdin'], subprocess.DEVNULL)

    def test_exposed_manager_rejected_without_running_copy(self):
        self.registry += MANAGER
        self.rejected('public registry')
        self.assertEqual(len(self.calls), 1)
        self.assertTrue(self.receipt()['registry']['zwp_input_method_manager_v2'])

    def test_missing_seat_rejected_without_running_copy(self):
        self.registry = "interface: 'wl_compositor', version: 6, name: 1\n"
        self.rejected('public registry')
        self.assertEqual(len(self.calls), 1)

    def test_registry_failure_is_not_protocol_denial(self):
        self.registry_status = 1
        self.rejected('public registry')
        self.assertEqual(len(self.calls), 1)

    def test_nonelf_and_nonexecutable_rejected_before_observation(self):
        self.source.write_bytes(b'#!/bin/sh\nexit 1\n')
        self.rejected('not ELF')
        self.assertEqual(self.calls, [])

    def test_nonexecutable_elf_rejected_before_observation(self):
        self.source.chmod(0o600)
        self.rejected('executable regular ELF')
        self.assertEqual(self.calls, [])

    def test_unadmitted_source_hash_rejected(self):
        with self.assertRaisesRegex(ValueError, 'already admitted bridge SHA256'):
            self.prove('0' * 64)
        self.assertEqual(self.calls, [])
        self.assertFalse(self.receipt()['verified'])

    def test_copy_hash_is_independently_verified(self):
        original_open = Path.open
        def changed_copy(path, mode='r', *args, **kwargs):
            if path == self.out / 'bridge/tuna-ibus-bridge' and mode == 'rb':
                return io.BytesIO(self.payload + b'changed-copy')
            return original_open(path, mode, *args, **kwargs)
        with patch.object(Path, 'open', changed_copy):
            self.rejected('source/copy does not match')
        receipt = self.receipt()
        self.assertEqual(receipt['source']['sha256'], self.expected)
        self.assertNotEqual(receipt['copy']['sha256'], self.expected)
        self.assertEqual(self.calls, [])

    def test_bus_failure_is_not_admission_denial(self):
        self.helper_stderr = 'tuna-ibus-bridge: cannot connect to session bus\n'
        self.rejected('exact admission denial')
        self.assertEqual(self.receipt()['copied_helper']['stderr'], self.helper_stderr)

    def test_nonexact_denial_rejected(self):
        self.helper_stderr = CONTEXT + M.DENIAL + ' extra\n'
        self.rejected('exact admission denial')

    def test_missing_input_context_prefix_rejected(self):
        self.helper_stderr = M.DENIAL + '\n'
        self.rejected('exact admission denial')

    def test_context_must_precede_denial(self):
        self.helper_stderr = M.DENIAL + '\n' + CONTEXT
        self.rejected('exact admission denial')

    def test_diagnostic_on_stdout_cannot_substitute_for_stderr(self):
        self.helper_stdout = DENIED
        self.helper_stderr = ''
        self.rejected('exact admission denial')

    def test_exit_zero_rejected(self):
        self.helper_status = 0
        self.rejected('exit 1')

    def test_arbitrary_nonzero_rejected(self):
        self.helper_status = 2
        self.rejected('exit 1')

    def test_signal_exit_rejected(self):
        self.helper_status = -15
        self.rejected('exit 1')

    def test_registry_timeout_retains_actual_partial_output_without_retry(self):
        self.timeout_on = 0
        with self.assertRaises(subprocess.TimeoutExpired):
            self.prove()
        self.assertEqual(len(self.calls), 1)
        self.assertFalse(self.receipt()['verified'])
        self.assertTrue(self.receipt()['registry']['timed_out'])
        self.assertEqual((self.out / 'wayland-info.txt').read_text(), SEAT)

    def test_copy_timeout_is_failure_even_with_exact_partial_diagnostic(self):
        self.timeout_on = 1
        with self.assertRaises(subprocess.TimeoutExpired):
            self.prove()
        self.assertEqual(len(self.calls), 2)
        self.assertTrue(self.receipt()['copied_helper']['timed_out'])
        self.assertFalse(self.receipt()['verified'])
        self.assertEqual((self.out / 'copied-helper.stderr').read_text(), DENIED)

    def test_observers_share_one_fixed_fifteen_second_deadline(self):
        self.durations = [14.0, 0.25]
        self.prove()
        self.assertEqual(self.calls[0][1]['timeout'], 15)
        self.assertEqual(self.calls[1][1]['timeout'], 1)
        self.assertEqual(self.receipt()['observation_budget_seconds'], 15)

    def test_exhausted_registry_budget_never_starts_copy(self):
        self.durations = [15.1, 0]
        with self.assertRaises(subprocess.TimeoutExpired):
            self.prove()
        self.assertEqual(len(self.calls), 1)
        self.assertFalse(self.receipt()['verified'])

    def test_root_refused_before_commands(self):
        with self.controlled(), patch.object(M.os, 'getuid', return_value=0), \
                patch.object(Path, 'lstat', return_value=SimpleNamespace(st_uid=0, st_mode=0o40700)):
            with self.assertRaisesRegex(ValueError, 'ordinary nonroot'):
                M.prove(self.out, 'actual-public-display', self.expected)
        self.assertEqual(self.calls, [])

    def test_success_stdout_is_receipt_json_only(self):
        stdout, stderr = io.StringIO(), io.StringIO()
        with self.controlled(), contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            status = M.main(['--out-dir', str(self.out), '--wayland-display', 'actual-public-display',
                             '--bridge-sha256', self.expected])
        self.assertEqual(status, 0)
        self.assertEqual(json.loads(stdout.getvalue()), self.receipt())
        self.assertEqual(stderr.getvalue(), '')

    def test_failure_has_no_success_stdout_and_actual_diagnostic(self):
        self.helper_stderr = 'tuna-ibus-bridge: actual bus failure\n'
        stdout, stderr = io.StringIO(), io.StringIO()
        with self.controlled(), contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            status = M.main(['--out-dir', str(self.out), '--wayland-display', 'actual-public-display',
                             '--bridge-sha256', self.expected])
        self.assertEqual(status, 1)
        self.assertEqual(stdout.getvalue(), '')
        self.assertIn('exact admission denial', stderr.getvalue())
        self.assertIn(self.helper_stderr, stderr.getvalue())
        self.assertEqual(self.receipt()['copied_helper']['stderr'], self.helper_stderr)


class ProductionGate(unittest.TestCase):
    def test_cli_has_only_fixed_installed_source_and_no_override(self):
        self.assertEqual(M.SOURCE, Path('/usr/bin/tuna-ibus-bridge'))
        with contextlib.redirect_stderr(io.StringIO()), patch.object(M, 'prove') as prove:
            with self.assertRaises(SystemExit) as rejected:
                M.main(['--out-dir', '/unit-owned', '--wayland-display', 'public',
                        '--bridge-sha256', 'a' * 64, '--source', '/unit-override'])
            self.assertEqual(rejected.exception.code, 2)
            prove.assert_not_called()

    def test_installed_admission_must_pass_before_unchanged_typing(self):
        source = (ROOT / 'scripts/tuna-gtk-shell-proof').read_text()
        engine_ready = '    grep -q ready "$ARTIFACTS/ibus-engine.log" || fail G-IME "the test engine did not register with IBus"\n'
        section = source.split(engine_ready, 1)[1].split('    IME_OUT=', 1)[0]
        self.assertIn('if [ "$INSTALLED_PACKAGE" = 1 ]; then', section)
        self.assertIn('.files["tuna-ibus-bridge"].sha256', section)
        self.assertIn('"$ARTIFACTS/installed-package.json"', section)
        self.assertIn('--wayland-display "$SOCKET_NAME"', section)
        self.assertIn('fail G-IME-ADMISSION', section)
        self.assertIn('pass G-IME-ADMISSION', section)
        self.assertNotIn('--source', section)
        self.assertIn('xdotool type --delay 100 nihao\n    sleep 0.8', source)
        self.assertIn('xdotool key space\n    for _ in $(seq 1 20); do [ "$(cat "$IME_OUT" 2>/dev/null)" = "你好" ] && break; sleep 0.3; done', source)
        # Execute only the real admission subsection with controlled shell tools.
        # The marker is not typing evidence; it proves failure stops the caller.
        shell = '''
fail() { printf 'failed:%s\\n' "$1"; exit 7; }
pass() { printf 'passed:%s\\n' "$1"; }
jq() { printf '%064d\\n' 0; }
admission() { printf 'observer\\n' >&2; return "$STATUS"; }
PY=admission
ROOT=/unit-controlled
SOCKET_NAME=actual-public-display
'''
        with tempfile.TemporaryDirectory() as tmp:
            for installed, status, expected in [('1', '0', 0), ('1', '1', 7), ('0', '1', 0)]:
                with self.subTest(installed=installed, observer_status=status):
                    result = subprocess.run(['/bin/sh', '-c', shell + section + '\nprintf "typing-next\\n"'],
                                            env={**os.environ, 'INSTALLED_PACKAGE': installed,
                                                 'STATUS': status, 'ARTIFACTS': tmp},
                                            capture_output=True, text=True, check=False)
                    self.assertEqual(result.returncode, expected, result.stderr)
                    self.assertEqual('typing-next' in result.stdout, expected == 0)
                    self.assertEqual('observer' in result.stderr, installed == '1')
                    self.assertEqual('passed:G-IME-ADMISSION' in result.stdout, installed == '1' and status == '0')


if __name__ == '__main__':
    unittest.main()
