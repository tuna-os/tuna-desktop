"""Native P-SY-09 on the original admitted VM, using finite root actions.

QMP captures are independent from guest GBM producer readback. Source/socket
checks do not qualify native hardware, an installed image, or a signed release.
"""
import base64
import hashlib
import json
from pathlib import Path
import re
import secrets
import time
import tomllib

from PIL import Image

CASES = ('ordering', 'destroy', 'unmap', 'disconnect', 'surface-exists', 'invalid-timeline',
         'no-surface', 'unsupported-buffer', 'no-buffer', 'no-acquire', 'no-release', 'conflicting-points',
         'failed-dmabuf', 'invalid-dmabuf', 'invalid-dmabuf-destroy-params', 'destroy-sync-shm',
         'legacy-v3', 'mixed-modifiers-v3', 'mixed-modifiers-v5')
ARTIFACTS = ('events.jsonl', 'stderr.log', 'default-format-table.bin', 'surface-format-table.bin',
             'producer-red.ppm', 'producer-green.ppm', 'producer-blue.ppm', 'producer-legacy-v3-red.ppm')
CHECKPOINTS = {'red-presented': [240, 16, 16], 'green-blocked': [240, 16, 16],
               'green-presented': [16, 240, 16], 'blue-presented': [16, 16, 240]}


def gpu_devices(width, height):
    return ['-device', 'pcie-root-port,id=tuna-gpu-port,chassis=1,slot=1',
            '-device', 'virtio-vga-gl,bus=tuna-gpu-port,x-pcie-pm-no-soft-reset=on,'
                       f'xres={width},yres={height}']


def run(qmp, agent_factory, out, image, inventory, vm):
    if (not isinstance(image, dict) or image.get('schema') != 1
            or re.fullmatch('[0-9a-f]{40}', image.get('commit', '')) is None
            or re.fullmatch('sha256:[0-9a-f]{64}', image.get('image_id', '')) is None
            or re.fullmatch('[0-9a-f]{64}', image.get('package_sha256', '')) is None):
        raise RuntimeError('explicit sync requires original same-run installed image/package admission')
    expected = image.get('member_sha256', {}).get('/usr/bin/tuna-compositor', '')
    if re.fullmatch('[0-9a-f]{64}', expected) is None:
        raise RuntimeError('admission lacks installed compositor hash')
    owners = [row for row in inventory['processes'] if row['executable'] == 'tuna-compositor'
              and row['uid'] == inventory['session_uid'] and row['uid'] != 0]
    if len(owners) != 1:
        raise RuntimeError('one admitted native session compositor required')
    owner = owners[0]
    root = Path(__file__).resolve().parents[2]
    lock_bytes = (root / 'Cargo.lock').read_bytes()
    locked = [row for row in tomllib.loads(lock_bytes.decode())['package'] if row['name'] == 'wayland-protocols']
    if len(locked) != 1:
        raise RuntimeError('one exact locked protocol source required')
    helper_hash = hashlib.sha256(Path(__file__).with_name('tuna-explicit-sync-guest.py').read_bytes()).hexdigest()
    destination = Path(out) / 'explicit-sync'
    destination.mkdir(mode=0o700, exist_ok=False)
    nonce = secrets.token_hex(16)
    receipt = {'schema': 1, 'scope': 'native emulated virtio/virgl DRM ioctl timelines and actual dmabuf pixels',
               'gpu_profile': 'virtio-vga-gl-virgl-llvmpipe-v1', 'nonce': nonce,
               'image_admission': image, 'session_owner': owner, 'captures': {}, 'complete': False,
               'physical_gpu_qualified': False, 'installed_signed_release_qualified': False}
    latest = {}
    attempted = False
    transport_safe = True
    deadline = time.monotonic() + 180

    def alive():
        if vm.poll() is not None:
            raise RuntimeError('original owned native VM exited')

    def invoke(operation, *, bounded=True, **arguments):
        nonlocal transport_safe
        alive()
        end = time.monotonic() + 7
        if bounded:
            end = min(end, deadline)
        if end <= time.monotonic():
            raise TimeoutError('native explicit-sync VM stage deadline')
        agent = agent_factory(deadline=end)
        try:
            remaining = end - time.monotonic()
            if remaining <= 0:
                raise TimeoutError('finite native action acquisition deadline')
            value = agent.run('explicit-sync', operation, json.dumps(dict(nonce=nonce, **arguments)),
                              timeout=remaining)
            alive()
            if time.monotonic() > end or type(value) is not dict:
                raise RuntimeError('invalid or late finite native action response')
            return value
        except BaseException:
            if agent.guest_exec_state != 'none':
                transport_safe = False
            raise
        finally:
            agent.close()

    def facts(state):
        current = state['compositor']
        if (current['pid'] != owner['pid'] or current['start_ticks'] != owner['start_ticks']
                or current['uid'] != owner['uid'] or current['sha256'] != expected
                or current['executable'] != '/usr/bin/tuna-compositor' or state['backend'] != 'drm'):
            raise RuntimeError('running native compositor differs from admitted original owner')
        source = state['protocol_source']
        package = locked[0]
        if (source['cargo_lock_sha256'] != hashlib.sha256(lock_bytes).hexdigest()
                or source['package'] != package['name'] or source['version'] != package['version']
                or source['registry_package_sha256'] != package['checksum']
                or source['verified_registry_archive_sha256'] != package['checksum']):
            raise RuntimeError('native helper XML differs from exact archive-verified locked dependency')
        if state['guest_helper_sha256'] != helper_hash:
            raise RuntimeError('installed proof helper differs from exact source')
        if any(re.fullmatch('[0-9a-f]{64}', state[key]) is None
               for key in ('client_sha256', 'protocol_xml_hash_list_sha256')):
            raise RuntimeError('native client/XML artifact facts missing')

    def acquire_pixels(checkpoint):
        name = checkpoint['name']
        if (name not in CHECKPOINTS or checkpoint['case'] != 'ordering'
                or checkpoint['rgb'] != CHECKPOINTS[name]):
            raise RuntimeError('unexpected native output checkpoint')
        if name in receipt['captures']:
            if receipt['captures'][name]['attempt'] != checkpoint['attempt']:
                raise RuntimeError('native checkpoint attempt changed')
            return  # The worker has not consumed our one ACK yet.
        path = destination / (name + '.png')
        alive()
        qmp.frame(str(path))
        alive()
        with Image.open(path) as captured:
            pixels = captured.convert('RGB')
        rgb = CHECKPOINTS[name]
        count = sum(1 for pixel in pixels.getdata()
                    if all(abs(actual - wanted) <= 8 for actual, wanted in zip(pixel, rgb)))
        if count < 512:
            raise RuntimeError('QMP output lacks actual dmabuf pixels at ' + name)
        green = None
        if name == 'green-blocked':
            green = sum(1 for pixel in pixels.getdata()
                        if all(abs(actual - wanted) <= 8 for actual, wanted in zip(pixel, [16, 240, 16])))
            if green >= 512:
                raise RuntimeError('QMP output displayed unacquired green buffer')
        observed = {'name': name, 'attempt': checkpoint['attempt'],
                    'png_sha256': hashlib.sha256(path.read_bytes()).hexdigest(),
                    'rgb': rgb, 'matching_pixels': count, 'green_matching_pixels': green}
        invoke('ack', case='ordering', attempt=checkpoint['attempt'], name=name,
               png_sha256=observed['png_sha256'])
        receipt['captures'][name] = observed

    def fetch_artifacts():
        for case in CASES:
            if case not in latest.get('cases', {}):
                continue
            target = destination / case
            target.mkdir(mode=0o700)
            for name in ARTIFACTS:
                offset = 0
                size = None
                with (target / name).open('wb') as stream:
                    while True:
                        part = invoke('file', bounded=False, case=case, name=name, offset=offset)
                        if part.get('missing'):
                            if offset:
                                raise RuntimeError('owned native artifact disappeared')
                            required = (name in {'events.jsonl', 'stderr.log'}
                                        or (case == 'ordering' and name in
                                            {'producer-red.ppm', 'producer-green.ppm', 'producer-blue.ppm'})
                                        or (case == 'legacy-v3' and name == 'producer-legacy-v3-red.ppm'))
                            if receipt.get('behavior_complete') and required:
                                raise RuntimeError('completed native case lacks artifact: ' + case + '/' + name)
                            break
                        data = base64.b64decode(part['data'], validate=True)
                        if (type(part['size']) is not int or not 0 <= part['size'] <= 1048576
                                or len(data) > 32768 or offset + len(data) > part['size']
                                or (size is not None and size != part['size'])):
                            raise RuntimeError('owned native artifact changed or exceeds bound')
                        size = part['size']
                        stream.write(data)
                        offset += len(data)
                        if offset == size:
                            break
                        if not data:
                            raise RuntimeError('truncated real native artifact')
                if size is None:
                    (target / name).unlink()

    try:
        attempted = True
        latest = invoke('init', pid=owner['pid'], start_ticks=owner['start_ticks'], uid=owner['uid'], sha256=expected)
        facts(latest)
        for case in CASES:
            latest = invoke('start', case=case)
            facts(latest)
            active = latest['active']
            if active['case'] != case or re.fullmatch('[0-9a-f]{32}', active['attempt']) is None:
                raise RuntimeError('native case start lacks exact owned attempt')
            case_deadline = min(deadline, time.monotonic() + 45)
            while time.monotonic() < case_deadline:
                latest = invoke('state')
                facts(latest)
                if latest['active'] != active:
                    raise RuntimeError('native case worker ownership changed')
                if latest.get('checkpoint'):
                    acquire_pixels(latest['checkpoint'])
                if latest['cases'][case].get('finished'):
                    latest = invoke('finish')
                    facts(latest)
                    row = latest['cases'][case]
                    if (latest.get('failure') or row.get('failure') or row.get('cleanup_failure')
                            or row.get('exit_code') != 0 or not row.get('worker_departed')
                            or not row.get('client_reaped')):
                        raise RuntimeError('actual native case failed: ' + str(row))
                    break
                time.sleep(0.01)
            else:
                raise TimeoutError('native explicit-sync case deadline: ' + case)
        if (not latest.get('complete') or set(receipt['captures']) != set(CHECKPOINTS)
                or set(latest['cases']) != set(CASES)):
            raise RuntimeError('actual native timeline/dmabuf acceptance incomplete')
        receipt['behavior_complete'] = True
    except BaseException as error:
        receipt['failure'] = type(error).__name__ + ': ' + str(error)
        raise
    finally:
        try:
            if attempted:
                receipt['guest'] = latest
                if not transport_safe:
                    raise RuntimeError('finite root action departure unconfirmed; original VM owner must retire VM')
                latest = invoke('cleanup', bounded=False)
                if latest['active'] is not None or any(not row.get('worker_departed') or not row.get('client_reaped')
                                                      for row in latest['cases'].values()):
                    raise RuntimeError('owned native worker/client retirement unconfirmed; VM owner must retire VM')
                receipt['guest'] = latest
                fetch_artifacts()
            receipt['complete'] = bool(receipt.get('behavior_complete')) and 'failure' not in receipt
        except BaseException as error:
            receipt['complete'] = False
            receipt['cleanup_failure'] = type(error).__name__ + ': ' + str(error)
            raise
        finally:
            (destination / 'receipt.json').write_text(json.dumps(receipt, indent=2) + '\n')
