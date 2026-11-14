#!/usr/bin/python3
"""Counters of tuna-resource-sample.py, the G-SEARCH-LEAK gate's (#1084)."""
import importlib.machinery
import importlib.util
import mmap
import os
from pathlib import Path
import unittest

loader = importlib.machinery.SourceFileLoader(
    'resource_sample', str(Path(__file__).with_name('tuna-resource-sample.py')))
spec = importlib.util.spec_from_loader(loader.name, loader)
sample = importlib.util.module_from_spec(spec)
loader.exec_module(sample)


class Kinds(unittest.TestCase):
    def test_descriptor_targets_group_without_their_inode(self):
        self.assertEqual(sample.fd_kind('/memfd:gdk-wayland (deleted)'), 'memfd:gdk-wayland')
        self.assertEqual(sample.fd_kind('socket:[123]'), 'socket')
        self.assertEqual(sample.fd_kind('anon_inode:[eventfd]'), 'anon_inode:[eventfd]')
        self.assertEqual(sample.fd_kind('anon_inode:[12]'), 'anon_inode')
        self.assertEqual(sample.fd_kind('/dev/dri/renderD128'), '/dev/dri/renderD128')
        self.assertEqual(sample.fd_kind('/tmp/x (deleted)'), 'deleted file')
        self.assertEqual(sample.fd_kind('/usr/share/x'), 'file')

    def test_only_shared_memory_mappings_count(self):
        self.assertEqual(sample.shm_name('/memfd:gdk-wayland (deleted)'), 'memfd:gdk-wayland')
        self.assertEqual(sample.shm_name('/dev/shm/pulse-shm-1'), 'shm:/dev/shm/pulse-shm-1')
        self.assertIsNone(sample.shm_name('/usr/lib/libgtk-4.so.1'))
        self.assertIsNone(sample.shm_name('[heap]'))


class Live(unittest.TestCase):
    def test_a_memfd_mapping_and_its_descriptor_are_counted(self):
        before = sample.sample_process(os.getpid())
        fd = os.memfd_create('gdk-wayland')
        os.ftruncate(fd, 59200)
        mapping = mmap.mmap(fd, 59200)
        try:
            after = sample.sample_process(os.getpid())
        finally:
            mapping.close()
            os.close(fd)
        # Python's mmap keeps a duplicate of the descriptor it maps.
        memfds = after['fd_kinds']['memfd:gdk-wayland'] - before['fd_kinds'].get('memfd:gdk-wayland', 0)
        self.assertEqual(memfds, 2)
        self.assertEqual(after['fds'], before['fds'] + memfds)
        was = before['shm'].get('memfd:gdk-wayland', {'count': 0, 'kib': 0})
        self.assertEqual(after['shm']['memfd:gdk-wayland']['count'], was['count'] + 1)
        self.assertEqual(after['shm']['memfd:gdk-wayland']['kib'], was['kib'] + 60)
        self.assertIsInstance(after['pss_anon_kib'], int)


def process(fds, memfds):
    return {'fds': fds, 'fd_kinds': {'socket': fds}, 'pss_anon_kib': 100,
            'shm': {'memfd:gdk-wayland': {'count': memfds, 'kib': memfds * 60}}}


class Compare(unittest.TestCase):
    def test_growth_past_the_allowance_fails(self):
        before = {'shell': process(40, 9), 'compositor': process(65, 0)}
        after = {'shell': process(40, 19), 'compositor': process(75, 0)}
        grown = sample.compare(before, after, [
            ('shell.shm:memfd:gdk-wayland', 0), ('shell.fds', 0), ('compositor.fds', 0)])
        self.assertEqual(grown, [
            'shell.shm:memfd:gdk-wayland grew 9 -> 19 (allowed +0)',
            'compositor.fds grew 65 -> 75 (allowed +0)',
        ])

    def test_growth_within_the_allowance_and_shrinking_pass(self):
        before = {'shell': process(40, 9)}
        after = {'shell': process(41, 3)}
        self.assertEqual(sample.compare(before, after, [
            ('shell.fds', 1), ('shell.shm:memfd:gdk-wayland', 0)]), [])

    def test_a_mapping_name_absent_from_one_sample_counts_as_none(self):
        before = {'shell': process(40, 0)}
        before['shell']['shm'] = {}
        after = {'shell': process(40, 2)}
        self.assertEqual(sample.compare(before, after, [('shell.shm:memfd:gdk-wayland', 0)]),
                         ['shell.shm:memfd:gdk-wayland grew 0 -> 2 (allowed +0)'])

    def test_differences_name_each_changed_counter(self):
        before = {'compositor': process(65, 0)}
        after = {'compositor': process(75, 1)}
        self.assertEqual(sample.differences(before, after), [
            'compositor.fds 65 -> 75',
            'compositor.fd[socket] 65 -> 75',
            'compositor.shm[memfd:gdk-wayland] 0 (0 KiB) -> 1 (60 KiB)',
        ])


if __name__ == '__main__':
    unittest.main()
