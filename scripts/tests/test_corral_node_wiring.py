"""Build-only wiring guards; actionlint checks actual YAML and shell syntax."""
from pathlib import Path
import unittest


class Tests(unittest.TestCase):
    def source(self):
        return (Path(__file__).resolve().parents[2]/'.github/workflows/corral-visual-tools.yml').read_text()

    def job(self):
        return self.source().split('  node-tools-smoke:\n',1)[1].split('  node-tools-publish:\n',1)[0]

    def test_existing_manual_workflow_opt_in(self):
        source=self.source()
        self.assertIn('  workflow_dispatch:\n    inputs:\n      node_tool_smoke:',source)
        self.assertIn('        default: false',source)
        self.assertIn("github.event_name == 'workflow_dispatch' && inputs.node_tool_smoke",self.job())
        self.assertIn("github.repository == 'tuna-os/tuna-desktop'",self.job())

    def test_build_only_has_no_registry_write(self):
        job=self.job()
        for forbidden in ('packages: write','REGISTRY_TOKEN','secrets.','podman push','podman login','--published-digest'):
            self.assertNotIn(forbidden,job)
        self.assertIn('permissions:\n      contents: read',job)

    def test_actual_same_runtime_and_original_closure(self):
        job=self.job()
        for required in ('packaging/benchmark/Containerfile.node-tools','--user 107:107 --cap-drop ALL',
                         '--security-opt no-new-privileges --read-only','/opt/tuna-tools/smoke.py',
                         'podman save --format oci-archive','scripts/lib/corral-node-tool-closure.py',
                         'corral-node-tool-closure','original-tools.oci.tar','if: always()'):
            self.assertIn(required,job)


if __name__=='__main__':unittest.main()
