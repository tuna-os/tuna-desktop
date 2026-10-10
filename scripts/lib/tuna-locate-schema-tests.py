#!/usr/bin/python3
import importlib.util
from pathlib import Path
import unittest
import json
import os
import subprocess
import tempfile
import xml.etree.ElementTree as ET

spec = importlib.util.spec_from_file_location('locate_schema', Path(__file__).with_name('tuna-locate-schema.py'))
module = importlib.util.module_from_spec(spec); spec.loader.exec_module(module)


class SchemaTests(unittest.TestCase):
    def setUp(self):
        self.root = ET.fromstring('''<schemalist><schema id="org.gnome.mutter" path="/org/gnome/mutter/">
        <key name="dynamic-workspaces" type="b"><default>true</default></key>
        <key name="attach-modal-dialogs" type="b"><default>false</default></key>
        <key name="policy" enum="org.example.Policy"><default>'fixed'</default><aliases><alias value="old" target="fixed"/></aliases></key>
        <key name="count" type="i"><range min="1" max="36"/><default>4</default></key>
        </schema></schemalist>''')
        self.keys = ['dynamic-workspaces','attach-modal-dialogs','policy','count']
        self.enums = ET.fromstring('<schemalist><enum id="org.example.Policy"><value nick="fixed" value="0"/></enum></schemalist>')
        self.documents = [(Path('mutter.xml'), self.root),(Path('enum.xml'),self.enums)]

    def test_preserves_all_original_keys_defaults_constraints_and_external_enum(self):
        _, extended = module.extend(self.documents,self.keys)
        original = self.root.find('schema'); result = extended.find('schema')
        self.assertEqual([ET.tostring(k) for k in original], [ET.tostring(k) for k in result][:-1])
        self.assertEqual(ET.tostring(extended.find('enum')),ET.tostring(self.enums.find('enum')))
        self.assertEqual(result[-1].get('name'),'locate-pointer-key')
        self.assertEqual(result[-1].find('default').text,"'Control_L'")
        self.assertEqual(len(original),4)

    def test_missing_or_ambiguous_original_refused(self):
        for documents in [[],self.documents+self.documents[:1]]:
            with self.assertRaises(ValueError): module.extend(documents,self.keys)

    def test_compiled_contract_mismatch_refused(self):
        with self.assertRaises(ValueError): module.extend(self.documents,self.keys[:-1])

    def test_unresolved_or_ambiguous_constraint_refused(self):
        for docs in [self.documents[:1],self.documents+self.documents[1:]]:
            with self.assertRaises(ValueError): module.extend(docs,self.keys)

    def test_real_glib_compile_preserves_enum_range_defaults(self):
        with tempfile.TemporaryDirectory(prefix='tuna-locate-schema-test-') as temporary:
            root = Path(temporary); installed = root/'installed'; overlay = root/'overlay'
            installed.mkdir(); overlay.mkdir()
            ET.ElementTree(self.root).write(installed/'mutter.gschema.xml', encoding='unicode')
            ET.ElementTree(self.enums).write(installed/'00-policy.gschema.xml', encoding='unicode')
            subprocess.run(['glib-compile-schemas','--strict',str(installed)],check=True)
            env = os.environ | {'GSETTINGS_SCHEMA_DIR':str(installed),'XDG_DATA_DIRS':str(root/'no-system-data')}
            helper = str(Path(__file__).with_name('tuna-locate-schema.py'))
            subprocess.run(['/usr/bin/python3',helper,str(overlay)],env=env,check=True)
            subprocess.run(['glib-compile-schemas','--strict',str(overlay)],check=True)
            env['GSETTINGS_SCHEMA_DIR'] = str(overlay)
            subprocess.run(['/usr/bin/python3',helper,str(overlay),'--verify'],env=env,check=True)
            receipt = json.loads((overlay/'locate-schema-provenance.json').read_text())
            self.assertTrue(receipt['fixture_extension'])
            self.assertTrue(receipt['compiled_contract_verified'])
            self.assertEqual(receipt['preserved_keys'],sorted(self.keys))

    def test_existing_key_not_replaced(self):
        ET.SubElement(self.root.find('schema'),'key',name='locate-pointer-key',type='s')
        with self.assertRaises(ValueError): module.extend(self.documents,self.keys+['locate-pointer-key'])


if __name__ == '__main__': unittest.main()
