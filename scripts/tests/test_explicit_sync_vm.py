#!/usr/bin/python3
"""Host argument/configuration regressions only; never native DRM qualification."""
import importlib.machinery
import importlib.util
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
loader = importlib.machinery.SourceFileLoader('syncobj_vm_lane', str(ROOT / 'scripts/tuna-vm-lane'))
spec = importlib.util.spec_from_loader(loader.name, loader)
lane = importlib.util.module_from_spec(spec)
loader.exec_module(lane)


class ExplicitSyncGpuProfile(unittest.TestCase):
    def command(self, explicit=False, environment=None):
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(lane.os.path, 'exists', return_value=True), \
                patch.object(lane.os, 'access', return_value=False), \
                patch.object(lane.subprocess, 'Popen') as spawn, \
                patch.dict(os.environ, environment or {}, clear=True):
            lane.boot('/owned/disk.raw', directory, directory, 30,
                      guest_agent=True, explicit_sync_gpu=explicit)
            spawn.assert_called_once()
            return spawn.call_args.args[0]

    def test_default_native_gpu_and_display_stay_unchanged(self):
        command = self.command()
        for argument in lane.baseline.gpu_devices():
            self.assertIn(argument, command)
        self.assertNotIn('gtk,gl=on', command)
        self.assertEqual(command[command.index('-display') + 1], 'none')
        self.assertEqual(lane.gpu_devices_for_receipt(), lane.baseline.gpu_devices())

    def test_extra_profile_keeps_original_disk_snapshot_and_pm_floor(self):
        command = self.command(True, {'DISPLAY': ':42', 'LIBGL_ALWAYS_SOFTWARE': '1',
                                      'GALLIUM_DRIVER': 'llvmpipe'})
        gpu = lane.gpu_devices_for_receipt(True)
        self.assertIn('virtio-vga-gl,', gpu[-1])
        self.assertIn('x-pcie-pm-no-soft-reset=on', gpu[-1])
        for argument in gpu:
            self.assertIn(argument, command)
        self.assertEqual(command[command.index('-display') + 1], 'gtk,gl=on')
        self.assertIn('-snapshot', command)
        self.assertIn('file=/owned/disk.raw,format=raw,if=virtio,cache=unsafe', command)
        self.assertNotIn('/dev/dri/', ' '.join(command))

    def test_missing_host_software_gl_environment_is_not_a_fallback(self):
        for environment in ({}, {'DISPLAY': ':42'},
                            {'DISPLAY': ':42', 'LIBGL_ALWAYS_SOFTWARE': '1', 'GALLIUM_DRIVER': 'virgl'}):
            with self.subTest(environment=environment), \
                    patch.object(lane.os.path, 'exists', return_value=True), \
                    patch.object(lane.os, 'access', return_value=False), \
                    patch.object(lane.subprocess, 'Popen') as spawn, \
                    patch.dict(os.environ, environment, clear=True):
                with self.assertRaisesRegex(RuntimeError, 'owned Xvfb/software-GL'):
                    lane.boot('/owned/disk.raw', '/owned/out', '/owned/scratch', 30, explicit_sync_gpu=True)
                spawn.assert_not_called()


if __name__ == '__main__':
    unittest.main()
