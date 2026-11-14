"""Locked-source integrity boundaries, not graphical/native qualification."""
import hashlib
import importlib.machinery
import importlib.util
import io
from pathlib import Path
import tarfile
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / 'tuna-protocol-xml-stage'
loader = importlib.machinery.SourceFileLoader('protocol_xml_stage', str(SCRIPT))
spec = importlib.util.spec_from_loader(loader.name, loader)
stage = importlib.util.module_from_spec(spec)
loader.exec_module(stage)


class RegistryProtocolIntegrity(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.source = self.root / 'protocol-fixture-1'
        (self.source / 'protocols').mkdir(parents=True)
        self.content = b'<protocol name="actual-file"/>\n'
        (self.source / 'protocols' / 'sample.xml').write_bytes(self.content)
        self.archive = self.root / 'protocol-fixture-1.crate'
        self.write_archive()

    def write_archive(self, *, duplicate=False, linked=False):
        with tarfile.open(self.archive, 'w:gz') as archive:
            member = tarfile.TarInfo(self.source.name + '/protocols/sample.xml')
            member.size = len(self.content)
            archive.addfile(member, io.BytesIO(self.content))
            if duplicate:
                archive.addfile(member, io.BytesIO(self.content))
            if linked:
                link = tarfile.TarInfo(self.source.name + '/protocols/alias.xml')
                link.type = tarfile.SYMTYPE
                link.linkname = 'sample.xml'
                archive.addfile(link)
        self.checksum = hashlib.sha256(self.archive.read_bytes()).hexdigest()

    def verify(self):
        return stage.verify_registry_protocols(self.source, self.archive, self.checksum)

    def test_verified_archive_works_without_cargo_checksum_sidecar(self):
        self.assertEqual(self.verify(), {
            'protocols/sample.xml': hashlib.sha256(self.content).hexdigest(),
        })

    def test_modified_resolved_xml_is_rejected(self):
        (self.source / 'protocols' / 'sample.xml').write_bytes(b'<protocol name="tampered"/>\n')
        with self.assertRaisesRegex(SystemExit, 'differ from the locked archive'):
            self.verify()

    def test_extra_and_missing_resolved_files_are_rejected(self):
        extra = self.source / 'protocols' / 'extra.xml'
        extra.write_bytes(self.content)
        with self.assertRaisesRegex(SystemExit, 'differ from the locked archive'):
            self.verify()
        extra.unlink()
        (self.source / 'protocols' / 'sample.xml').unlink()
        with self.assertRaisesRegex(SystemExit, 'differ from the locked archive'):
            self.verify()

    def test_changed_cached_archive_is_rejected_before_parsing(self):
        self.archive.write_bytes(b'not the locked archive')
        with self.assertRaisesRegex(SystemExit, 'checksum differs from Cargo.lock'):
            self.verify()

    def test_duplicate_archive_members_are_rejected(self):
        self.write_archive(duplicate=True)
        with self.assertRaisesRegex(SystemExit, 'duplicate registry protocol member'):
            self.verify()

    def test_archive_links_are_rejected(self):
        self.write_archive(linked=True)
        with self.assertRaisesRegex(SystemExit, 'non-regular registry protocol member'):
            self.verify()

    def test_resolved_symlink_is_rejected_even_with_matching_content(self):
        sample = self.source / 'protocols' / 'sample.xml'
        backing = self.root / 'sample.xml'
        sample.rename(backing)
        sample.symlink_to(backing)
        with self.assertRaisesRegex(SystemExit, 'source is a symlink'):
            self.verify()


if __name__ == '__main__':
    unittest.main()
