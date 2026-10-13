#!/usr/bin/env python3
"""Inspect the original OCI archive without loading or extracting its layers."""
import argparse
import gzip
import hashlib
import json
from pathlib import Path
import tarfile

p = argparse.ArgumentParser()
p.add_argument('archive', type=Path);p.add_argument('smoke', type=Path);p.add_argument('out', type=Path)
p.add_argument('--source', required=True);p.add_argument('--published-digest')
a = p.parse_args()
with tarfile.open(a.archive) as archive:
    entries = archive.getmembers()
    members = {m.name: m for m in entries}
    if len(entries) != len(members):raise ValueError('duplicate OCI archive member')
    if len(members) > 512 or any(m.name.startswith('/') or '..' in Path(m.name).parts or not (m.isfile() or m.isdir()) for m in members.values()):
        raise ValueError('bounded regular OCI archive required')
    def read_json(name):
        if members[name].size > 1024*1024:raise ValueError('OCI metadata exceeds bound')
        return json.load(archive.extractfile(name))
    index = read_json('index.json')
    if len(index['manifests']) != 1:raise ValueError('single platform tool image required')
    digest = index['manifests'][0]['digest']
    def blob_name(value):return 'blobs/sha256/'+value.removeprefix('sha256:')
    manifest_bytes = archive.extractfile(blob_name(digest)).read(1024*1024+1)
    if len(manifest_bytes)>1024*1024 or 'sha256:'+hashlib.sha256(manifest_bytes).hexdigest()!=digest:
        raise ValueError('original manifest digest differs')
    manifest = json.loads(manifest_bytes)
    config_bytes = archive.extractfile(blob_name(manifest['config']['digest'])).read(1024*1024+1)
    if len(config_bytes)>1024*1024 or 'sha256:'+hashlib.sha256(config_bytes).hexdigest()!=manifest['config']['digest']:
        raise ValueError('original config digest differs')
    config = json.loads(config_bytes)
    if config.get('architecture') != 'amd64' or config.get('os') != 'linux':
        raise ValueError('exact amd64 Linux image required')
    diff_ids = config.get('rootfs', {}).get('diff_ids')
    if not isinstance(diff_ids, list) or len(diff_ids) != len(manifest['layers']):
        raise ValueError('OCI rootfs layer count differs')
    layers = []
    for index, layer in enumerate(manifest['layers']):
        compressed = members[blob_name(layer['digest'])].size
        if compressed != layer['size']:raise ValueError('OCI layer size differs')
        raw = archive.extractfile(blob_name(layer['digest']))
        h = hashlib.sha256();total = 0
        while data := raw.read(1024*1024):h.update(data);total += len(data)
        if 'sha256:'+h.hexdigest() != layer['digest']:raise ValueError('OCI layer bytes differ')
        raw = archive.extractfile(blob_name(layer['digest']))
        stream = gzip.GzipFile(fileobj=raw) if layer['mediaType'].endswith('+gzip') else raw
        uncompressed = 0; expanded_hash = hashlib.sha256()
        while data := stream.read(1024*1024):
            uncompressed += len(data); expanded_hash.update(data)
            if uncompressed > 4*1024**3:raise ValueError('tool layer expansion exceeds bound')
        if 'sha256:'+expanded_hash.hexdigest() != diff_ids[index]:
            raise ValueError('OCI decompressed layer differs from rootfs diff_id')
        layers.append({'diff_id': diff_ids[index], 'digest': layer['digest'], 'compressed_bytes': compressed, 'uncompressed_bytes': uncompressed})
    if a.published_digest and a.published_digest != digest:raise ValueError('registry push changed original manifest digest')
    smoke = json.loads(a.smoke.read_text().splitlines()[-1])
    source_root = Path(__file__).resolve().parents[2]
    names = ['scripts/lib/corral_node_stage.py', 'scripts/lib/corral_node_worker.py',
             'scripts/lib/corral-node-tool-smoke.py', 'scripts/lib/corral-node-tool-closure.py',
             'scripts/tuna-corral-benchmark-import', 'packaging/benchmark/Containerfile.node-tools', 'packaging/benchmark/python-private', 'scripts/lib/corral_node_tools.py']
    source_hashes = {n: hashlib.sha256((source_root/n).read_bytes()).hexdigest() for n in names}
    closure = {**smoke, 'source_files_sha256': source_hashes, 'source_commit': a.source,
               'installer_image': 'registry.access.redhat.com/ubi9/ubi@sha256:ee3871c273cc65a2bd24970b47c15b90fbf962841bc2e41e4f915b1c4c0208e2',
               'base_image': 'quay.io/kubevirt/virt-launcher@sha256:574fabe0ff9719fa1e3800dcca8530fa4e02fd71ef74109508e59800e13e8b5d',
               'image': 'ghcr.io/tuna-os/benchmark-node-tools@'+digest,
               'registry_published': bool(a.published_digest), 'layers': layers,
               'config': config, 'original_manifest': manifest,
               'oci_sha256': hashlib.file_digest(a.archive.open('rb'), 'sha256').hexdigest(),
               'sbom_scope': 'Private Python installroot RPM inventory and downloaded RPM SHA list retained in image; original launcher RPM database is untouched; not a full transitive security certification.'}
    a.out.mkdir(parents=True, exist_ok=False)
    (a.out/'tool-closure.json').write_text(json.dumps(closure, indent=2)+'\n')
