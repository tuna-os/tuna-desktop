#!/usr/bin/python3
"""Extend installed Mutter XML for a private locate-key journey, preserving its contract."""
import copy
import hashlib
import json
import os
from pathlib import Path
import sys
import xml.etree.ElementTree as ET

SCHEMA = 'org.gnome.mutter'
KEY = 'locate-pointer-key'


def extend(documents, keys):
    matches = [(path, root) for path, root in documents
               if any(s.get('id') == SCHEMA for s in root.findall('schema'))]
    if len(matches) != 1:
        raise ValueError('need exactly one installed Mutter XML; cannot invent its existing contract')
    path, original = matches[0]
    root = copy.deepcopy(original)
    schema = next(s for s in root.findall('schema') if s.get('id') == SCHEMA)
    if {k.get('name') for k in schema.findall('key')} != set(keys):
        raise ValueError('installed XML differs from active compiled Mutter key set')
    if any(k.get('name') == KEY for k in schema.findall('key')):
        raise ValueError('extension requested despite installed locate key')
    # Preserve ranges, enum/flags semantics, aliases, defaults, children and
    # translations verbatim. Resolve referenced external definitions too.
    definitions = {e.get('id') for e in root if e.tag in ('enum', 'flags')}
    for key in root.iter('key'):
        for kind in ('enum', 'flags'):
            needed = key.get(kind)
            if needed and needed not in definitions:
                found = [e for _, document in documents for e in document.findall(kind)
                         if e.get('id') == needed]
                if len(found) != 1:
                    raise ValueError('missing/ambiguous installed '+kind+' definition: '+needed)
                root.insert(0, copy.deepcopy(found[0])); definitions.add(needed)
    key = ET.SubElement(schema, 'key', name=KEY, type='s')
    ET.SubElement(key, 'default').text = "'Control_L'"
    return path, root


def main():
    from gi.repository import Gio
    directory = Path(sys.argv[1]); directory.mkdir(parents=True, exist_ok=True)
    schema = Gio.SettingsSchemaSource.get_default().lookup(SCHEMA, True)
    if schema is None:
        raise SystemExit('installed Mutter schema required (install mutter-common); refusing partial replacement')
    contract = {name: {'type': schema.get_key(name).get_value_type().dup_string(),
                       'default': schema.get_key(name).get_default_value().print_(True),
                       'range': schema.get_key(name).get_range().print_(True)}
                for name in sorted(schema.list_keys())}
    receipt_path = directory/'locate-schema-provenance.json'
    if len(sys.argv) > 2 and sys.argv[2] == '--verify':
        receipt = json.loads(receipt_path.read_text())
        for name, original in receipt['installed_contract'].items():
            if contract.get(name) != original:
                raise SystemExit('fixture changed installed Mutter contract: '+name)
        if not schema.has_key(KEY) or schema.get_key(KEY).get_value_type().dup_string() != 's':
            raise SystemExit('locate-pointer-key string fixture missing')
        receipt['compiled_contract_verified'] = True
        receipt_path.write_text(json.dumps(receipt, indent=2)+'\n')
        return
    receipt = {'schema': SCHEMA, 'fixture_extension': False, 'preserved_keys': sorted(schema.list_keys()),
               'installed_contract': contract}
    if not schema.has_key(KEY):
        dirs = [Path(p) for p in os.environ.get('GSETTINGS_SCHEMA_DIR', '').split(':') if p]
        dirs += [Path(p)/'glib-2.0/schemas' for p in os.environ.get('XDG_DATA_DIRS', '/usr/local/share:/usr/share').split(':')]
        documents = []
        seen = set()
        for folder in dirs:
            for path in sorted(folder.glob('*.gschema.xml')):
                if path.resolve() not in seen:
                    documents.append((path, ET.parse(path).getroot())); seen.add(path.resolve())
        path, root = extend(documents, schema.list_keys())
        target = directory/'org.tuna.fixture-mutter.gschema.xml'
        ET.ElementTree(root).write(target, encoding='unicode')
        receipt.update(fixture_extension=True, installed_xml=str(path),
                       installed_xml_sha256=hashlib.sha256(path.read_bytes()).hexdigest(),
                       fixture_xml_sha256=hashlib.sha256(target.read_bytes()).hexdigest(),
                       extension={KEY: "string default Control_L"})
    (directory/'locate-schema-provenance.json').write_text(json.dumps(receipt, indent=2)+'\n')


if __name__ == '__main__':
    main()
