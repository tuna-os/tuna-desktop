#!/usr/bin/python3
"""Verify PID identity, missing counters and session liveness failures."""
import importlib.machinery
import importlib.util
import io
import json
import os
import sys
import time
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
from types import SimpleNamespace

loader = importlib.machinery.SourceFileLoader('soak', str(Path(__file__).resolve().parents[1] / 'tuna-vm-soak'))
spec = importlib.util.spec_from_loader(loader.name, loader)
soak = importlib.util.module_from_spec(spec)
loader.exec_module(soak)
from endurance_presentation import Reader
import soak_report


class PresentationFailures(unittest.TestCase):
    def test_actual_native_batch_rollover_preserves_cross_batch_interval(self):
        root = Path(__file__).parent / 'fixtures'
        raw = (root / 'endurance-rollover.jsonl').read_bytes()
        import hashlib
        provenance = json.loads((root / 'endurance-rollover-provenance.json').read_text())
        self.assertEqual(hashlib.sha256(raw).hexdigest(), provenance['raw_sha256'])
        result = Reader(io.BytesIO(raw), 16).finish()
        self.assertEqual(result['batches'], 2)
        self.assertEqual(result['frames'], 274)
        self.assertEqual(result['interval_ms']['count'], 273)

    def raw(self):
        return (Path(__file__).parent / 'fixtures/endurance-control.jsonl').read_bytes()

    def rows(self):
        return [json.loads(line) for line in self.raw().splitlines()]

    def encoded(self, rows):
        return b''.join(json.dumps(row).encode() + b'\n' for row in rows)

    def test_real_native_client_feedback(self):
        result = Reader(io.BytesIO(self.raw()), 3).finish()
        self.assertTrue(result['complete'])
        self.assertEqual(result['frames'], 54)
        self.assertEqual(result['interval_ms']['count'], 53)
        self.assertEqual(sum(result['flags'].values()), 0)

    def test_short_rehearsal_cannot_qualify_24_hours(self):
        with self.assertRaises(ValueError):
            Reader(io.BytesIO(self.raw()), 86400).finish()

    def test_truncated_or_missing_completion_cannot_qualify(self):
        for raw in (self.raw()[:-1], self.raw().splitlines(keepends=True)[0]):
            with self.subTest(size=len(raw)), self.assertRaises(ValueError):
                Reader(io.BytesIO(raw), 3).finish()

    def test_partial_row_can_resume_without_losing_feedback(self):
        raw = self.raw()
        stream = io.BytesIO(raw[:100])
        reader = Reader(stream, 3)
        self.assertEqual(reader.poll()['frames'], 0)
        stream.seek(100); stream.write(raw[100:]); stream.seek(0)
        self.assertEqual(reader.finish()['frames'], 54)

    def test_duplicate_backward_and_missing_frames_fail(self):
        for fault in ('duplicate', 'backwards', 'missing', 'accounting', 'batch', 'clock'):
            rows = self.rows()
            if fault == 'duplicate': rows[0]['frames'][1]['commit'] = rows[0]['frames'][0]['commit']
            elif fault == 'backwards': rows[0]['frames'][1]['presented_ns'] = 1
            elif fault == 'missing': rows[0]['frames'].pop()
            elif fault == 'accounting': rows[0]['pending'] += 1
            elif fault == 'batch': rows[0]['batch'] = 1
            elif fault == 'clock': rows[0]['clock_id'] = True
            with self.subTest(fault=fault), self.assertRaises(ValueError):
                Reader(io.BytesIO(self.encoded(rows)), 3).finish()

    def test_no_records_after_completion_or_oversized_rows(self):
        for raw in (self.raw() + self.raw(), b'x' * (128 * 1024 + 1)):
            with self.assertRaises(ValueError):
                Reader(io.BytesIO(raw), 3).finish()


class ProcessFailures(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        for index, name in enumerate(soak.CRITICAL, 100):
            proc = self.root / str(index)
            proc.mkdir()
            # Parenthesized comm can contain spaces and ')' characters.
            (proc / 'stat').write_text(f'{index} (name with ) spaces) S ' + '0 ' * 18 + '1234 ' + '0 ' * 8)
            (proc / 'exe').symlink_to('/usr/bin/' + name)
            (proc / 'smaps_rollup').write_text('Pss: 100 kB\nRss: 200 kB\n')
            (proc / 'fd').mkdir()
            (proc / 'fd/0').touch()

    def test_parenthesized_name_does_not_shift_identity(self):
        self.assertEqual(soak.identity(self.root / '100'), ('S', 1234))
        self.assertEqual(soak.sample(os.getuid(), self.root)['pss_kib'], 200)

    def test_missing_counter_fails_instead_of_zero(self):
        (self.root / '100/smaps_rollup').write_text('Rss: 200 kB\n')
        with self.assertRaisesRegex(RuntimeError, 'incomplete memory'):
            soak.sample(os.getuid(), self.root)

    def test_departed_critical_process_fails_liveness(self):
        (self.root / '100/exe').unlink()
        with self.assertRaisesRegex(RuntimeError, 'critical session process'):
            soak.sample(os.getuid(), self.root)

    def test_zombie_cannot_count_as_live_critical_process(self):
        stat = self.root / '100/stat'
        stat.write_text(stat.read_text().replace('spaces) S', 'spaces) Z'))
        with self.assertRaisesRegex(RuntimeError, 'critical session process'):
            soak.sample(os.getuid(), self.root)

    def test_pid_reuse_fails_instead_of_combining_processes(self):
        with patch.object(soak, 'identity', side_effect=[('S', 1234), ('S', 5678)]):
            with self.assertRaisesRegex(RuntimeError, 'changed identity'):
                soak.sample(os.getuid(), self.root)

    def test_departure_check_handles_proc_disappearing_during_read(self):
        for error in (FileNotFoundError(), ProcessLookupError()):
            with patch.object(soak, 'identity', side_effect=error):
                self.assertTrue(soak.is_departed(self.root / '100'))
        with patch.object(soak, 'identity', side_effect=PermissionError()):
            with self.assertRaises(PermissionError):
                soak.is_departed(self.root / '100')
        self.assertFalse(soak.is_departed(self.root / '100'))

    def test_departure_during_error_check_still_fails_critical_liveness(self):
        with patch.object(soak, 'identity', side_effect=[
            ProcessLookupError(), FileNotFoundError(), ('S', 1234), ('S', 1234),
        ]):
            with self.assertRaisesRegex(RuntimeError, 'critical session process'):
                soak.sample(os.getuid(), self.root)

    def test_unreadable_live_process_is_fatal(self):
        with patch.object(soak, 'identity', side_effect=PermissionError('denied')):
            with self.assertRaises(PermissionError):
                soak.sample(os.getuid(), self.root)


class UserHelpers(unittest.TestCase):
    def setUp(self):
        self.account = SimpleNamespace(pw_uid=1001, pw_gid=1002, pw_name='tuna-test',
                                       pw_dir='/home/tuna-test')

    def test_credentials_and_environment_do_not_inherit_root_state(self):
        with patch.object(soak.os, 'getgrouplist', return_value=[1002, 1003]) as groups:
            context = soak.user_context(self.account)
        groups.assert_called_once_with('tuna-test', 1002)
        self.assertEqual(context, {
            'user': 1001, 'group': 1002, 'extra_groups': [1002, 1003],
            'env': {'PATH': '/usr/bin:/bin', 'HOME': '/home/tuna-test',
                    'USER': 'tuna-test', 'LOGNAME': 'tuna-test',
                    'XDG_RUNTIME_DIR': '/run/user/1001',
                    'DBUS_SESSION_BUS_ADDRESS': 'unix:path=/run/user/1001/bus'}})
        with patch.object(soak.os, 'getgrouplist', return_value=[1002, 1003]):
            native = soak.user_context(self.account, 'tuna-nested-123')
        self.assertEqual(native, context | {
            'env': context['env'] | {'WAYLAND_DISPLAY': 'tuna-nested-123'}})

    def test_all_health_calls_use_direct_credentials_without_pam_wrappers(self):
        context = {'user': 1001, 'group': 1002, 'extra_groups': [1002], 'env': {}}
        with patch.object(soak, 'call', side_effect=['(false,)', '()', '(<<uint32 0>>,)', '()']) as call:
            replies = soak.session_health(context)
        self.assertEqual(list(replies), ['lock_reply', 'calendar_reply', 'portal_reply', 'portal_gnome_reply'])
        self.assertEqual(call.call_count, 4)
        for invocation in call.call_args_list:
            self.assertEqual(invocation.args[0][:3], ['gdbus', 'call', '--session'])
            self.assertEqual(invocation.kwargs, {'context': context})
            self.assertNotIn('runuser', invocation.args[0])

    def test_failure_names_the_exact_endpoint_and_method(self):
        with patch.object(soak, 'call', side_effect=[
            '(false,)', '()', '(<<uint32 0>>,)', RuntimeError('health helper exceeded its deadline'),
        ]):
            with self.assertRaisesRegex(
                RuntimeError, r'portal_gnome health check .*org.freedesktop.impl.portal.desktop.gnome, '
                              r'org.freedesktop.DBus.Peer.Ping.*health helper exceeded its deadline',
            ):
                soak.session_health({})

    def test_invalid_lock_reply_still_fails_closed(self):
        with patch.object(soak, 'call', return_value='()') as call:
            with self.assertRaisesRegex(RuntimeError, 'screen-saver health reply'):
                soak.session_health({})
        self.assertEqual(call.call_count, 1)

    @unittest.skipUnless(os.geteuid() == 0, 'actual UID/GID drop requires root')
    def test_real_child_drops_identity_and_groups_but_observer_stays_root(self):
        account = soak.pwd.getpwnam('nobody')
        context = soak.user_context(account)
        code = ('import json,os; print(json.dumps(dict(uid=os.getuid(), euid=os.geteuid(), '
                'gid=os.getgid(), groups=os.getgroups(), env=dict(os.environ), '
                'session=os.getsid(0), pid=os.getpid())))')
        with patch.object(soak.subprocess, 'Popen', wraps=soak.subprocess.Popen) as spawn:
            result = json.loads(soak.call([sys.executable, '-I', '-c', code], context=context))
        self.assertEqual(result['uid'], account.pw_uid)
        self.assertEqual(result['euid'], account.pw_uid)
        self.assertEqual(result['gid'], account.pw_gid)
        self.assertEqual(sorted(result['groups']), sorted(context['extra_groups']))
        self.assertNotIn(0, result['groups'])
        for key, value in context['env'].items():
            self.assertEqual(result['env'][key], value)
        self.assertNotIn('SUDO_UID', result['env'])
        self.assertEqual(result['session'], result['pid'])
        self.assertEqual(os.geteuid(), 0)
        self.assertEqual(spawn.call_args.args[0][0], sys.executable)
        for key, value in context.items():
            self.assertEqual(spawn.call_args.kwargs[key], value)


class HelperFailures(unittest.TestCase):
    def test_default_deadline_remains_ten_seconds_with_child_credentials(self):
        from unittest.mock import Mock
        process = Mock(pid=123, returncode=None)
        process.poll.return_value = None
        with patch.object(soak.subprocess, 'Popen', return_value=process) as spawn, \
             patch.object(soak.time, 'monotonic', side_effect=[100, 110]), \
             patch.object(soak.os, 'killpg') as kill:
            context = {'user': 1001, 'group': 1002, 'extra_groups': [1002], 'env': {}}
            with self.assertRaisesRegex(RuntimeError, 'deadline'):
                soak.call(['gdbus'], context=context)
        kill.assert_called_once_with(123, soak.signal.SIGKILL)
        process.wait.assert_called_once()
        for key, value in context.items():
            self.assertEqual(spawn.call_args.kwargs[key], value)

    def test_output_budget_is_enforced_even_after_exit(self):
        with self.assertRaisesRegex(RuntimeError, 'output budget'):
            soak.call([sys.executable, '-c', 'print("x" * 65537)'])

    def test_nonzero_exit_is_fatal(self):
        with self.assertRaisesRegex(RuntimeError, r'failed \(7\)'):
            soak.call([sys.executable, '-c', 'raise SystemExit(7)'])

    def test_deadline_kills_helper_process_group(self):
        with tempfile.TemporaryDirectory() as directory:
            pidfile = Path(directory) / 'pid'
            code = ('import os,time; from pathlib import Path; '
                    'pid=os.fork(); '
                    f'Path({str(pidfile)!r}).write_text(str(pid)) if pid else None; '
                    'time.sleep(30)')
            with self.assertRaisesRegex(RuntimeError, 'deadline'):
                soak.call([sys.executable, '-c', code], timeout=0.3)
            pid = int(pidfile.read_text())
            deadline = time.monotonic() + 2
            while time.monotonic() < deadline:
                proc = Path('/proc') / str(pid)
                if soak.is_departed(proc):
                    break
                time.sleep(0.02)
            else:
                self.fail('helper descendant survived the deadline')


class Scope(unittest.TestCase):
    def test_only_the_full_day_is_a_qualification_candidate(self):
        self.assertEqual(soak.scope(86400), '24-hour qualification candidate')
        for seconds in (3000, 86399, 86401):
            with self.subTest(seconds=seconds):
                self.assertIn('not the 24-hour qualification run', soak.scope(seconds))


class Summary(unittest.TestCase):
    """The retained-directory report the nightly VM lane publishes."""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        fixture = Path(__file__).parent / 'fixtures/endurance-control.jsonl'
        (self.root / 'presentation.jsonl').write_bytes(fixture.read_bytes())
        self.provenance = {'requested_duration_s': 3, 'warmup_s': 1, 'interval_s': 1,
                           'scope': soak.scope(3), 'source_revision': '0' * 40, 'boot_id': 'boot',
                           'presentation_workload': {'size': [320, 180]}}
        interval = {'count': 2, 'p50': 16.6, 'p95': 17.0, 'p99': 33.3, 'min': 16.0, 'max': 33.3, 'mean': 20.0}
        self.samples = [{'elapsed_s': float(t), 'pss_kib': 100 * 1024 + 512 * t, 'rss_kib': 200 * 1024,
                         'fds': 40 + t, 'processes': 10,
                         'presentation': {'frames': 18, 'interval_ms': interval}} for t in range(4)]
        self.report = {'complete': True, 'samples': 4, 'scope': soak.scope(3), 'elapsed_s': 3.01,
                       'baseline': self.samples[1]}
        self.store()

    def store(self):
        (self.root / 'provenance.json').write_text(json.dumps(self.provenance))
        (self.root / 'report.json').write_text(json.dumps(self.report))
        (self.root / 'samples.jsonl').write_text(''.join(json.dumps(s) + '\n' for s in self.samples))

    def test_complete_bounded_run_reports_memory_and_pacing_over_time(self):
        summary, text = soak_report.write(self.root)
        self.assertTrue(summary['complete'], summary['problems'])
        self.assertFalse(summary['qualifies_24h'])
        memory = summary['memory']
        self.assertEqual(memory['baseline']['pss_kib'], 100 * 1024 + 512)
        self.assertEqual(memory['final']['fds'], 43)
        self.assertEqual(memory['samples_after_warmup'], 3)
        self.assertAlmostEqual(memory['pss_slope_kib_per_h'], 512 * 3600)
        # The whole retained capture is re-read, not only the last poll.
        self.assertEqual(summary['presentation']['total_frames'], 54)
        self.assertEqual(summary['presentation']['interval_ms']['count'], 53)
        self.assertEqual(len(summary['over_time']), 4)
        self.assertIn('not the 24-hour qualification run', text)
        self.assertIn('| 0.1 | 101.5 | 200.0 | 43 | 10 | 18 | 16.60 | 17.00 | 33.30 |', text)
        self.assertEqual(json.loads((self.root / 'summary.json').read_text())['samples'], 4)

    def test_incomplete_or_inconsistent_runs_fail(self):
        for fault in ('failure', 'short', 'count', 'scope', 'truncated', 'missing-report'):
            with self.subTest(fault=fault):
                self.setUp()
                if fault == 'failure':
                    self.report = {'complete': False, 'samples': 4, 'scope': soak.scope(3),
                                   'failure': 'critical process exited or restarted'}
                elif fault == 'short':
                    self.report['elapsed_s'] = 2.5
                elif fault == 'count':
                    self.samples.pop()
                elif fault == 'scope':
                    self.report['scope'] = soak.scope(86400)
                self.store()
                if fault == 'truncated':
                    raw = (self.root / 'presentation.jsonl').read_bytes()
                    (self.root / 'presentation.jsonl').write_bytes(raw[:-1])
                elif fault == 'missing-report':
                    (self.root / 'report.json').unlink()
                summary, text = soak_report.write(self.root)
                self.assertFalse(summary['complete'])
                self.assertIn('**FAILED**', text)

    def test_long_runs_show_bounded_evenly_spaced_rows(self):
        samples = list(range(1000))
        picked = soak_report.rows(samples)
        self.assertEqual(len(picked), soak_report.MAX_ROWS)
        self.assertEqual((picked[0], picked[-1]), (0, 999))


if __name__ == '__main__':
    unittest.main()
