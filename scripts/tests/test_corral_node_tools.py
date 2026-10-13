"""Pure controls for runtime library evidence, plus private-wrapper source guards."""
from pathlib import Path
import sys
import unittest
sys.path.insert(0,str(Path(__file__).resolve().parents[1]/'lib'))
import corral_node_tools as tools


class Tests(unittest.TestCase):
    def maps(self,name):return '1000-2000 r-xp 0000 00:00 1 '+name

    def test_private_mapped_elf(self):
        paths=tools.private_elf_paths(self.maps('/opt/tuna-python/lib/libc.so'),
                                      resolve=Path,is_elf=lambda p:True)
        self.assertEqual(paths,[Path('/opt/tuna-python/lib/libc.so')])

    def test_global_mapped_elf_and_symlink_escape_refused(self):
        for name in ('/lib/x86_64-linux-gnu/libcrypto.so.3','/opt/tuna-python/lib/escaped.so'):
            with self.assertRaises(ValueError):
                tools.private_elf_paths(self.maps(name),resolve=lambda p:Path('/lib/global.so'),is_elf=lambda p:True)

    def test_missing_or_deleted_mapping_refused(self):
        for maps in ('',self.maps('/opt/tuna-python/lib/foo.so (deleted)')):
            with self.assertRaises(ValueError):tools.private_elf_paths(maps,resolve=Path,is_elf=lambda p:True)

    def test_qemu_path_or_byte_change_refused(self):
        tools.same_qemu_libraries({'/lib/libc.so':'a'},{'/lib/libc.so':'a'})
        for after in ({},{'/lib/libc.so':'b'},{'/opt/private/libc.so':'a'}):
            with self.assertRaises(ValueError):tools.same_qemu_libraries({'/lib/libc.so':'a'},after)

    def test_only_private_loader_and_no_global_library_override(self):
        root=Path(__file__).resolve().parents[2]
        wrapper=(root/'packaging/benchmark/python-private').read_text()
        self.assertIn('--inhibit-cache --library-path /opt/tuna-python/',wrapper)
        self.assertIn('PYTHONHOME=/opt/tuna-python/usr',wrapper)
        self.assertNotIn('LD_LIBRARY_PATH=',wrapper)
        container=(root/'packaging/benchmark/Containerfile.node-tools').read_text()
        final=container.split('FROM quay.io/kubevirt/virt-launcher@',1)[1]
        self.assertNotIn('rpm -U',final);self.assertNotIn('ldconfig',final)
        self.assertIn('COPY --from=private-python /opt/tuna-python /opt/tuna-python',final)
        self.assertIn('qemu-before-libraries.sha256',final)
        stage=(root/'scripts/lib/corral_node_stage.py').read_text()
        self.assertNotIn("'/usr/bin/python3.12'",stage)
        self.assertIn("'/opt/tuna-tools/python'",stage)


if __name__=='__main__':unittest.main()
