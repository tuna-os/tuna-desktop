"""Finite admission checks for CI benchmark disks, never node storage qualification."""
import hashlib
import json
import re
from pathlib import Path

GIB = 1024**3
VIRTUAL_BYTES = 18 * GIB
MAX_EXPORT_BYTES = 2 * GIB
MAX_ESTIMATED_PEAK = 7 * GIB
GUEST_WRITE_ALLOWANCE = GIB // 2


def require(condition, message):
    if not condition:
        raise ValueError(message)


def digest(path):
    with Path(path).open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def allocated_extents(mapping, virtual_size):
    require(type(mapping) is list and 0 < len(mapping) <= 100000, 'extent map bound')
    end = 0
    nonzero = 0
    for extent in mapping:
        require(type(extent) is dict, 'extent object')
        start, length = extent.get('start'), extent.get('length')
        require(type(start) is int and start == end and type(length) is int
                and length > 0 and start + length <= virtual_size, 'extent range/coverage')
        require(type(extent.get('data')) is bool and type(extent.get('zero')) is bool,
                'extent data/zero classification')
        if extent['data'] and not extent['zero']:
            nonzero += length
        end = start + length
    require(end == virtual_size, 'incomplete extent coverage')
    return nonzero


def storage_receipt(info, mapping, file_bytes):
    require(type(info) is dict and info.get('format') == 'qcow2', 'qcow2 required')
    require(info.get('virtual-size') == VIRTUAL_BYTES, '18Gi virtual disk required')
    require(not info.get('backing-filename') and not info.get('full-backing-filename')
            and not info.get('snapshots'), 'external backing/snapshots refused')
    actual = info.get('actual-size')
    require(type(actual) is int and 0 < actual <= MAX_EXPORT_BYTES,
            'actual qcow2 allocation exceeds export bound')
    require(type(file_bytes) is int and 0 < file_bytes <= MAX_EXPORT_BYTES,
            'qcow2 transfer exceeds 2Gi bound')
    nonzero = allocated_extents(mapping, VIRTUAL_BYTES)
    estimate = nonzero + file_bytes + GUEST_WRITE_ALLOWANCE
    require(estimate <= MAX_ESTIMATED_PEAK, 'estimated expanded/import peak exceeds 7Gi bound')
    return {'virtual_bytes': VIRTUAL_BYTES, 'transfer_bytes': file_bytes,
            'qcow2_allocated_bytes': actual, 'mapped_nonzero_bytes': nonzero,
            'guest_write_allowance_bytes': GUEST_WRITE_ALLOWANCE,
            'estimated_peak_bytes': estimate, 'estimated_peak_limit_bytes': MAX_ESTIMATED_PEAK,
            'node_peak_qualified': False,
            'limitation': 'CDI scratch/preallocation and guest writes must be measured on a pressure-free node; virtual capacity is not allocated usage'}


def provenance(source, selection, payload, reference, profile):
    from gnome_reference import validate_image
    require(isinstance(source, str) and re.fullmatch('[0-9a-f]{40}', source), 'source commit')
    validate_image(reference, profile)
    require(type(selection) is dict and re.fullmatch('[0-9a-f]{40}', selection.get('head_sha', '')),
            'selected package source commit')
    require(type(selection.get('run_id')) is int and selection['run_id'] > 0,
            'selected package CI run')
    package = payload.get('package', {})
    require(type(package) is dict and re.fullmatch('[0-9a-f]{64}', package.get('sha256', '')),
            'package SHA256')
    members = payload.get('required_members')
    require(type(members) is dict and members and all(
        type(value) is dict and re.fullmatch('[0-9a-f]{64}', value.get('sha256', ''))
        for value in members.values()), 'installed native member provenance')
    return {'observer_source_commit': source, 'candidate_source_commit': selection['head_sha'],
            'package_selection': selection, 'package_sha256': package['sha256'],
            'candidate_required_members': members, 'reference': reference,
            'gnome_profile': profile, 'paired_desktops': ['gnome', 'tuna'],
            'signed_release': False, 'performance_qualified': False}
