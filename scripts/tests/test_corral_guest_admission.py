"""Execute the shared admission lifecycle with controlled identity changes."""
import copy
import json
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'lib'))
import corral_guest_admission as admission


class AdmissionTests(unittest.TestCase):
    def test_completion_requires_unchanged_process_image_import_source_and_boot(self):
        for mutation in ('none', 'selected', 'running', 'image', 'import', 'source', 'boot', 'substrate'):
            with self.subTest(mutation=mutation), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                manifest = {'gnome_profile': 'stock', 'package_sha256': 'a' * 64,
                            'fixture_image_id': 'image', 'observer_source_commit': 'b' * 40}
                (root / 'manifest.json').write_text(json.dumps(manifest))
                (root / 'reference.json').write_text(json.dumps({'image': 'original'}))
                reference = {'image': 'original'}
                selected = {'desktop': 'tuna', 'boot_id': 'boot', 'compositor': {'pid': 42}}
                running = {'boot_id': 'boot', 'running_exe_sha256': 'c' * 64}
                source = {'commit': 'd' * 40, 'sha256': {'helper': 'e' * 64}}
                imported = {'vm_uid': 'owned', 'disk': 'original'}
                state = {'selected': selected, 'running': running, 'image': reference,
                         'source': source, 'import': imported}
                def boot_check(value):
                    if value != 'boot': raise ValueError('boot changed')
                def verify():
                    if mutation == 'substrate': raise ValueError('node/domain changed')
                session = SimpleNamespace(metadata={'vm_uid': 'owned', 'node_uid': 'node', 'guest_domain': 'domain'},
                    python=lambda *args: state['running'], check_boot=boot_check,
                    desktop=lambda *args: state['selected'], verify=verify)
                options = {'export_dir': directory, 'reference_image': str(root / 'reference.json'),
                           'candidate_sha': 'f' * 40, 'package_run': 1, 'key': '/owned/key',
                           'desktop': 'tuna', 'import_receipt': '/owned/import', 'context': 'exact',
                           'namespace': 'owned', 'vm': 'owned', 'tuna_payload': '/original/payload',
                           'tuna_selection': '/original/selection'}
                with patch.object(admission, 'candidate_intent'), \
                     patch.object(admission, 'validate_image', side_effect=lambda value, profile: value), \
                     patch.object(admission, 'image_reference', side_effect=lambda _: state['image']), \
                     patch.object(admission, 'admit_imported_disk', side_effect=lambda *args: state['import']), \
                     patch.object(admission, 'client_key', return_value={'fingerprint': 'original'}), \
                     patch.object(admission, 'selected_identity'), \
                     patch.object(admission, 'source_identity', side_effect=lambda: state['source']):
                    gate = admission.AggregateAdmission(session, options, reference, selected)
                    self.assertFalse(gate.evidence['completed'])
                    if mutation in state:
                        state[mutation] = copy.deepcopy(state[mutation]);state[mutation]['changed'] = True
                    if mutation == 'boot': state['running'] = {**running, 'boot_id': 'other'}
                    if mutation == 'none':
                        self.assertTrue(gate.finish()['completed'])
                    else:
                        with self.assertRaises(ValueError): gate.finish()
                        self.assertFalse(gate.evidence['completed'])


if __name__ == '__main__': unittest.main()
