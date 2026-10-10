#!/usr/bin/python3
from pathlib import Path
import ast
import unittest
import yaml
ROOT = Path(__file__).resolve().parents[2]

class ExportSshTests(unittest.TestCase):
    def test_public_key_only_configuration_and_host_keys_before_sshd(self):
        setup = (ROOT/'packaging/marlin/perf/tuna-corral-ssh-setup').read_text()
        for required in ['PermitRootLogin prohibit-password', 'PasswordAuthentication no',
                         'KbdInteractiveAuthentication no', 'AllowUsers root',
                         'AuthorizedKeysFile /etc/ssh/authorized_keys/%u',
                         'Requires=tuna-corral-ssh-hostkeys.service',
                         'After=tuna-corral-ssh-hostkeys.service', '/usr/bin/ssh-keygen -A']:
            self.assertIn(required, setup)
        self.assertNotIn('PasswordAuthentication yes', setup)

    def test_export_requires_key_and_manifest_does_not_claim_access(self):
        exporter = (ROOT/'scripts/tuna-benchmark-disk-export').read_text()
        ast.parse(exporter)
        self.assertIn("'--ssh-public-key-file', type=Path, required=True", exporter)
        self.assertIn("'boot_access_qualified': False", exporter)
        self.assertIn("output(['ssh-keygen', '-lf', str(public_key)])", exporter)
        self.assertIn("'CORRAL_SSH=true'", exporter)

    def test_key_is_a_build_secret_and_dispatch_input_is_environment(self):
        container = (ROOT/'packaging/marlin/perf/Containerfile').read_text()
        self.assertIn('ARG CORRAL_SSH=false', container)
        self.assertIn('--mount=type=secret,id=benchmark-ssh-key', container)
        workflow = yaml.safe_load((ROOT/'.github/workflows/performance-baseline.yml').read_text())
        steps = workflow['jobs']['compare']['steps']
        step = next(s for s in steps if s.get('name') == 'Export bounded compact paired benchmark disk')
        self.assertIn('BENCHMARK_SSH_PUBLIC_KEY', step['env'])
        self.assertNotIn('inputs.benchmark_ssh_public_key', step['run'])
        self.assertIn('--ssh-public-key-file', step['run'])

if __name__ == '__main__':
    unittest.main()
