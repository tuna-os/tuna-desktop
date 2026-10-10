#!/usr/bin/python3
from pathlib import Path
import unittest
import yaml
ROOT = Path(__file__).resolve().parents[2]

class ArchAudioDependencies(unittest.TestCase):
    def test_notification_runner_installs_private_pulse_server(self):
        workflow = yaml.safe_load((ROOT/'.github/workflows/ci.yml').read_text())
        steps = workflow['jobs']['gtk-shell']['steps']
        installs = [step['run'] for step in steps
                    if 'apt-get' in step.get('run', '') and
                    'gnome-session-canberra' in step['run']]
        self.assertEqual(len(installs), 1)
        for dependency in ('bubblewrap', 'pipewire', 'pipewire-pulse', 'wireplumber',
                           'pulseaudio-utils', 'sound-theme-freedesktop'):
            self.assertIn(dependency, installs[0].split())

    def test_packaging_runner_installs_declared_audio_runtime_dependencies(self):
        workflow = yaml.safe_load((ROOT/'.github/workflows/ci.yml').read_text())
        installs = [step['run'] for step in workflow['jobs']['arch-package']['steps']
                    if 'pacman -Syu' in step.get('run', '')]
        self.assertEqual(len(installs), 1)
        recipe = (ROOT/'packaging/arch/PKGBUILD').read_text()
        for dependency in ('libcanberra', 'sound-theme-freedesktop'):
            self.assertIn("'" + dependency + "'", recipe)
            self.assertIn(dependency, installs[0].split())

if __name__ == '__main__':
    unittest.main()
