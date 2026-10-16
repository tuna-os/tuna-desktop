"""Exact Cargo input selection shared by pre-execution and final CI admission."""
import json
from pathlib import Path


def select_inputs(out):
    out = Path(out)
    root = Path((out / 'root.txt').read_text().strip()).resolve()
    metadata = json.loads((out / 'metadata.json').read_text())
    packages = [p for p in metadata['packages'] if p['name'] == 'tuna-compositor'
                and Path(p['manifest_path']).resolve() == root / 'crates/compositor/Cargo.toml']
    assert len(packages) == 1, 'exact compositor package metadata required'
    package_id = packages[0]['id']
    assert isinstance(package_id, str) and package_id, 'package identity required'
    rows = [json.loads(line) for line in (out / 'build.json').read_text().splitlines()]
    rows += [json.loads(line) for line in (out / 'probe-build.json').read_text().splitlines()]
    roles = {'test': ('auth_owner_runtime', ['test'], True, 'crates/compositor/tests/auth_owner_runtime.rs'),
             'compositor': ('tuna-compositor', ['bin'], False, 'crates/compositor/src/main.rs'),
             'probe': ('native-auth-owner-probe', ['example'], False, 'crates/compositor/examples/native-auth-owner-probe.rs')}
    selected = {'package_id': package_id, 'root': str(root)}
    for role, (name, kind, test_profile, source) in roles.items():
        matches = [r for r in rows if r.get('reason') == 'compiler-artifact'
                   and r.get('package_id') == package_id
                   and r.get('target', {}).get('name') == name
                   and r['target'].get('kind') == kind
                   and r.get('profile', {}).get('test') is test_profile
                   and r['target'].get('src_path')
                   and Path(r['target']['src_path']).resolve() == root / source
                   and r.get('executable')]
        assert len(matches) == 1, f'exact single {role} Cargo artifact required'
        selected[role] = matches[0]['executable']
    return selected
