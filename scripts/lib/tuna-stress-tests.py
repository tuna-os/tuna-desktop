#!/usr/bin/python3
"""Exercise process-tree sampling against pids that exit mid-sample."""
import importlib.machinery
import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

loader = importlib.machinery.SourceFileLoader(
    'stress', str(Path(__file__).resolve().parents[1] / 'tuna-stress'))
spec = importlib.util.spec_from_loader(loader.name, loader)
stress = importlib.util.module_from_spec(spec)
loader.exec_module(stress)

PS = "100 1\n101 100\n"


def write_process(root, pid, pss, rss, fds, state='S', rollup=True):
    proc = root / str(pid)
    proc.mkdir()
    (proc / 'stat').write_text(
        f"{pid} (tuna-shell gtk) {state} 100 100 0 0 -1 4194560 1 0 0 0 1 2 0 0 20 0 3 0 9\n")
    if rollup:
        (proc / 'smaps_rollup').write_text(
            f"55a0 [rollup]\nRss: {rss} kB\nPss: {pss} kB\nPrivate_Clean: 0 kB\n")
    fd_dir = proc / 'fd'
    fd_dir.mkdir()
    for index in range(fds):
        (fd_dir / str(index)).write_text("")
    return proc


class VanishedProcess:
    """Stand in for a /proc path whose reads answer ESRCH."""

    def __init__(self, name):
        self.name = name

    def __truediv__(self, _other):
        return self

    def read_text(self):
        raise ProcessLookupError(3, 'No such process')


class RetryReads:
    """A rollup that answers ESRCH a bounded number of times, then succeeds."""

    def __init__(self, failures, text):
        self.failures, self.text, self.reads = failures, text, 0

    def __truediv__(self, _other):
        return self

    def read_text(self):
        self.reads += 1
        if self.reads <= self.failures:
            raise ProcessLookupError(3, 'No such process')
        return self.text


class ProcessSampling(unittest.TestCase):
    def sample(self, root, rollup=None):
        with patch.object(stress.subprocess, 'check_output', return_value=PS):
            if rollup is None:
                return stress.metrics(100, Path(root))
            with patch.object(stress, 'read_smaps_rollup', rollup):
                return stress.metrics(100, Path(root))

    def test_exited_child_is_accounted_without_failing_the_sample(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            write_process(root, 100, pss=4096, rss=8192, fds=7)
            value = self.sample(root)
        self.assertEqual(value['pss_kib'], 4096)
        self.assertEqual(value['rss_kib'], 8192)
        self.assertEqual(value['fds'], 7)
        self.assertEqual(value['processes'], 2)
        self.assertEqual(value['sampled_processes'], 1)
        self.assertEqual(value['exited_processes'], 1)

    def test_zombie_rollup_esrch_counts_as_an_exit(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            write_process(root, 100, pss=4096, rss=8192, fds=7)
            write_process(root, 101, pss=2048, rss=4096, fds=3, state='Z')
            original = stress.read_smaps_rollup

            def rollup(proc):
                if proc.name == '101':
                    raise ProcessLookupError(3, 'No such process')
                return original(proc)

            value = self.sample(root, rollup)
        self.assertEqual(value['pss_kib'], 4096)
        self.assertEqual(value['fds'], 7)
        self.assertEqual((value['sampled_processes'], value['exited_processes']), (1, 1))

    def test_live_process_that_cannot_be_read_still_fails_the_sample(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            write_process(root, 100, pss=4096, rss=8192, fds=7)
            write_process(root, 101, pss=2048, rss=4096, fds=3, state='R')
            original = stress.read_smaps_rollup

            def rollup(proc):
                if proc.name == '101':
                    raise ProcessLookupError(3, 'No such process')
                return original(proc)

            with self.assertRaises(ProcessLookupError):
                self.sample(root, rollup)

    def test_descriptor_enumeration_race_discards_the_whole_process(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            write_process(root, 100, pss=4096, rss=8192, fds=7)
            proc = write_process(root, 101, pss=2048, rss=4096, fds=0)
            (proc / 'fd').rmdir()
            (proc / 'stat').unlink()
            value = self.sample(root)
        self.assertEqual(value['pss_kib'], 4096)
        self.assertEqual(value['rss_kib'], 8192)
        self.assertEqual(value['fds'], 7)
        self.assertEqual((value['sampled_processes'], value['exited_processes']), (1, 1))

    def test_transient_rollup_esrch_is_retried_rather_than_dropped(self):
        source = RetryReads(2, "Rss: 10 kB\nPss: 5 kB\n")
        self.assertEqual(stress.read_smaps_rollup(source), "Rss: 10 kB\nPss: 5 kB\n")
        self.assertEqual(source.reads, 3)

    def test_persistent_rollup_esrch_is_reported(self):
        with self.assertRaises(ProcessLookupError):
            stress.read_smaps_rollup(VanishedProcess('101'))

    def test_missing_process_directory_reads_as_exited(self):
        with tempfile.TemporaryDirectory() as temp:
            self.assertTrue(stress.has_exited(Path(temp) / '101'))

    def test_running_process_does_not_read_as_exited(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            write_process(root, 100, pss=1, rss=1, fds=0, state='R')
            self.assertFalse(stress.has_exited(root / '100'))


if __name__ == '__main__':
    unittest.main()
