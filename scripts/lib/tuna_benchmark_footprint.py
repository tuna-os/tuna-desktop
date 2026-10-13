"""Read-only fixed-image/storage diagnostics. No admission or memory qualification."""
import json
import re
from pathlib import Path
from tuna_benchmark_disk import digest, require

PATHS = ('/usr', '/var', '/boot', '/etc', '/var/cache/pacman/pkg',
         '/var/lib/pacman', '/usr/lib/modules', '/usr/share')
IMAGE_ID = r'(?:sha256:)?[0-9a-f]{64}'


def write_json(path, value):
    Path(path).write_text(json.dumps(value, indent=2)+'\n')
    return digest(path)


def image_footprints(commands, identities, artifacts):
    """Only the four admitted image roles; all runs use exact local image IDs."""
    require(set(identities) == {'base', 'coherent', 'preview', 'fixture'}, 'fixed image roles required')
    path = Path(artifacts)/'image-footprint.json'
    result = {'schema': 1, 'images': {}, 'complete': False,
              'decoder_rss_measured': False, 'storage_admission_qualified': False,
              'limitations': 'Image inspect size is metadata, du is container-view allocation. Roles/paths overlap; do not sum them or infer decoder RSS.'}
    write_json(path, result)
    for role, reference in identities.items():
        item = {'reference': reference, 'complete': False}
        result['images'][role] = item
        try:
            require(re.fullmatch(IMAGE_ID, reference) or
                    re.fullmatch(r'ghcr.io/tuna-os/marlin@sha256:[0-9a-f]{64}', reference), 'immutable local image required')
            _, text = commands.call(['sudo', 'podman', 'image', 'inspect', reference, '--format',
                                      '{{.Id}}\n{{.Size}}\n{{json .RootFS.Layers}}'], timeout=60)
            lines = text.splitlines()
            require(len(lines) == 3 and re.fullmatch(IMAGE_ID, lines[0]), 'image identity inspection required')
            image, size, layers = lines[0], int(lines[1]), json.loads(lines[2])
            require(size > 0 and isinstance(layers, list) and
                    all(isinstance(layer, str) and re.fullmatch(IMAGE_ID, layer) for layer in layers), 'image metadata required')
            if re.fullmatch(IMAGE_ID, reference):
                require(image.removeprefix('sha256:') == reference.removeprefix('sha256:'), 'image ID changed')
            item.update(image_id=image, image_metadata_size_bytes=size, layer_diff_ids=layers,
                        layer_physical_bytes_measured=False)
            # No user-selected paths or commands; no image/environment dump.
            script = 'for path; do if test -e "$path"; then du -sx -B1 "$path"; else printf "MISSING\\t%s\\n" "$path"; fi; done'
            _, usage = commands.call(['sudo', 'podman', 'run', '--rm', '--read-only', '--network', 'none',
                                      '--pull', 'never', '--entrypoint', '/bin/sh', image,
                                      '-c', script, 'footprint', *PATHS], timeout=60)
            observed = {}
            for line in usage.splitlines():
                amount, name = line.split('\t', 1)
                require(name in PATHS and name not in observed, 'unexpected/duplicate footprint path')
                require(amount == 'MISSING' or amount.isdecimal(), 'invalid allocated byte count')
                observed[name] = None if amount == 'MISSING' else int(amount)
            require(set(observed) == set(PATHS), 'missing footprint observations')
            item.update(container_view_allocated_bytes=observed, complete=True)
        except Exception as error:
            item['error'] = str(error)[:2048]
        write_json(path, result)
    result['complete'] = all(item['complete'] for item in result['images'].values())
    return {'file': path.name, 'sha256': write_json(path, result),
            'complete': result['complete'], 'decoder_rss_measured': False}


def disk_evidence(artifacts, disk, raw, info, mapping):
    """Persist observations BEFORE admission can fail and remove the disk."""
    files = {'qemu-info.json': info, 'qemu-map.json': mapping}
    stats = {}
    for name, path in [('qcow2', disk), ('raw', raw)]:
        value = Path(path).stat()
        stats[name] = {'file_bytes': value.st_size, 'allocated_blocks_bytes': value.st_blocks*512,
                       'device': value.st_dev, 'inode': value.st_ino, 'mtime_ns': value.st_mtime_ns}
    files['disk-file-stat.json'] = {'files': stats, 'decoder_rss_measured': False}
    return {name: write_json(Path(artifacts)/name, value) for name, value in files.items()}
