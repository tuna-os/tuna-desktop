"""Original package/image/source identity negatives; no image or guest execution."""
import copy
import hashlib
from pathlib import Path
import sys
import tempfile
import unittest
sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'lib'))
from vm_strip_image import admission, IMAGE_INSPECTION


class Tests(unittest.TestCase):
    def test_installed_bytes_and_exact_image_identity_required(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            fixture = root / 'packaging/marlin/vm-lane'
            fixture.mkdir(parents=True)
            expected = {'/usr/bin/tuna-compositor': 'a' * 64}
            for name in ('tuna-vm-input', 'tuna-vm-lifecycle'):
                (fixture / name).write_text(name)
                expected['/usr/libexec/' + name] = hashlib.sha256(name.encode()).hexdigest()
            package = {'package': {'sha256': 'b' * 64}, 'required_members':
                       {'usr/bin/tuna-compositor': {'sha256': 'a' * 64}}}
            observed = {'schema': 1, 'image_id': 'sha256:' + 'c' * 64, 'member_sha256': expected}
            args = (root, 'd' * 40, observed['image_id'], 'ghcr.io/tuna-os/marlin:gnome@sha256:' + 'e' * 64)
            receipt = admission(package, observed, *args)
            self.assertFalse(receipt['installed_signed_release_qualified'])
            for key, value in [('schema', 2), ('image_id', 'sha256:' + 'f' * 64),
                               ('member_sha256', {**expected, '/unrelated': 'a' * 64}),
                               ('member_sha256', {**expected, '/usr/bin/tuna-compositor': 'f' * 64})]:
                wrong = copy.deepcopy(observed)
                wrong[key] = value
                with self.assertRaises(ValueError):
                    admission(package, wrong, *args)
            (fixture / 'tuna-vm-input').write_text('changed source')
            with self.assertRaises(ValueError):
                admission(package, observed, *args)

    def test_actual_inspection_program_parses(self):
        compile(IMAGE_INSPECTION, '<immutable-image-inspector>', 'exec')

    def test_ci_keeps_original_gates_and_pins_actual_inspection(self):
        root = Path(__file__).resolve().parents[2]
        source = (root / '.github/workflows/ci.yml').read_text()
        self.assertIn('TUNA_PKG_SHA256=', source)
        self.assertIn('--entrypoint /usr/bin/python3 "$image_id"', source)
        self.assertIn('--strip-image vm-lane-artifacts/strip-image.json', source)
        lane = (root / 'scripts/tuna-vm-lane').read_text()
        self.assertIn('if getattr(args, "strip_image", None) and index == args.boots:', lane)
        self.assertIn('strip_image.get(field) != metadata.get(meta)', lane)
        self.assertIn('reconnect=lambda: guest_agent.GuestAgent(agent_path)', lane)


if __name__ == '__main__':
    unittest.main()
