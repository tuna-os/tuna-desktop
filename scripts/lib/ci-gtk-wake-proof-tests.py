#!/usr/bin/python3
"""Preflight negative cases execute the actual CI wrapper without Cargo."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

import yaml

ROOT = Path(__file__).resolve().parents[2]
WRAPPER = ROOT/'scripts/lib/ci-gtk-wake-proof.py'


class PreflightTests(unittest.TestCase):
    def test_required_native_proof_flags_belong_to_the_proof_step(self):
        workflow = yaml.safe_load((ROOT/'.github/workflows/ci.yml').read_text())
        steps = workflow['jobs']['gtk-shell']['steps']
        proof = [step for step in steps if step.get('run') ==
                 'scripts/tuna-gtk-shell-proof --artifacts gtk-shell-artifacts']
        self.assertEqual(len(proof), 1)
        for name in ('TUNA_REQUIRE_PAM_WRAPPER', 'TUNA_REQUIRE_IBUS', 'TUNA_REQUIRE_WAYLAND_INFO'):
            self.assertEqual(proof[0]['env'].get(name), '1')
        collection = next(step for step in steps if step.get('name') ==
                          "Collect this run's minimize receipts with GTK proof evidence")
        self.assertNotIn('env', collection)

    def test_artifacts_inside_checkout_rejected_before_build(self):
        artifacts = ROOT/'target'/'rejected-gtk-wake-receipts'
        self.assertFalse(artifacts.exists())
        result = subprocess.run(['/usr/bin/python3', str(WRAPPER), '--artifacts', str(artifacts)],
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 2)
        self.assertIn('outside the source checkout', result.stderr)
        self.assertFalse(artifacts.exists())

    def test_stale_receipt_directory_rejected_before_build(self):
        with tempfile.TemporaryDirectory(prefix='tuna-wake-ci-test-') as directory:
            artifacts = Path(directory)
            (artifacts/'result.json').write_text('{"qualified":true}')
            result = subprocess.run(['/usr/bin/python3', str(WRAPPER), '--artifacts', str(artifacts)],
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 2)
            self.assertIn('stale receipts are rejected', result.stderr)
            self.assertEqual((artifacts/'result.json').read_text(), '{"qualified":true}')

    def test_incorrect_ci_identity_retains_failure_never_runs_cargo(self):
        self.assertFalse(subprocess.check_output(['git','status','--porcelain'],cwd=ROOT,text=True).strip(),
                         'run source-owned preflight tests from a clean committed checkout')
        with tempfile.TemporaryDirectory(prefix='tuna-wake-ci-test-') as directory:
            artifacts = Path(directory)/'receipts'
            env = dict(os.environ, GITHUB_SHA='0'*40)
            result = subprocess.run(['/usr/bin/python3', str(WRAPPER), '--artifacts', str(artifacts)],
                                    env=env,capture_output=True,text=True)
            self.assertEqual(result.returncode, 1)
            receipt = json.loads((artifacts/'result.json').read_text())
            self.assertFalse(receipt['qualified'])
            self.assertIn('GITHUB_SHA', receipt['error'])
            self.assertNotIn('phase', receipt)
            self.assertFalse((artifacts/'workspace-clean.log').exists())
            self.assertFalse((artifacts/'frozen').exists())


if __name__ == '__main__':
    unittest.main()
