"""Native P-SY-09 stage for the existing owned, admitted Marlin VM lane.

Uses the same actual C timeline/dmabuf consumer as hardware qualification.
QMP captures are independent from guest GBM producer readback. There is no
unsupported skip, mock import device, alternate session or alternate package.
"""
import base64
import hashlib
import json
from pathlib import Path
import re
import secrets
import time

from PIL import Image

from vm_guest_agent import GuestAgent

CASES = ('ordering', 'destroy', 'unmap', 'disconnect', 'surface-exists', 'invalid-timeline',
         'no-surface', 'unsupported-buffer', 'no-buffer', 'no-acquire', 'no-release', 'conflicting-points',
         'failed-dmabuf', 'invalid-dmabuf', 'invalid-dmabuf-destroy-params', 'destroy-sync-shm',
         'legacy-v3', 'mixed-modifiers-v3', 'mixed-modifiers-v5')

def gpu_devices(width, height):
    return ['-device', 'pcie-root-port,id=tuna-gpu-port,chassis=1,slot=1',
            '-device', 'virtio-vga-gl,bus=tuna-gpu-port,x-pcie-pm-no-soft-reset=on,'
                       f'xres={width},yres={height}']

CHECKPOINTS = {'red-presented', 'green-blocked', 'green-presented', 'blue-presented'}


def run(qmp, vm, agent_path, output, image, inventory):
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
    destination = Path(output) / 'explicit-sync'
    destination.mkdir(mode=0o700, exist_ok=False)
    nonce = secrets.token_hex(16)
    receipt = {'schema': 1, 'scope': 'native emulated virtio/virgl DRM ioctl timelines and actual dmabuf pixels',
               'gpu_profile': 'virtio-vga-gl-virgl-llvmpipe-v1', 'nonce': nonce,
               'image_admission': image, 'session_owner': owner, 'captures': {}, 'complete': False,
               'physical_gpu_qualified': False, 'installed_signed_release_qualified': False}
    agent = GuestAgent(agent_path)
    worker_pid = None
    latest = {}

    def invoke(action, *arguments):
        deadline = time.monotonic() + 7
        pid = agent.command('guest-exec', deadline=deadline,
                            path='/usr/libexec/tuna-explicit-sync-guest',
                            arg=[action, nonce, *map(str, arguments)], **{'capture-output': True})['pid']
        while time.monotonic() < deadline:
            status = agent.command('guest-exec-status', pid=pid, deadline=deadline)
            if not status['exited']:
                time.sleep(0.01)
                continue
            if status.get('exitcode') != 0 or status.get('out-truncated') or status.get('err-truncated'):
                raise RuntimeError('fixed native proof action failed: ' + action)
            return json.loads(base64.b64decode(status.get('out-data', ''), validate=True))
        raise TimeoutError('fixed native proof action deadline: ' + action)

    def acquire_pixels(checkpoint):
        name = checkpoint['name']
        if name not in CHECKPOINTS or name in receipt['captures']:
            raise RuntimeError('unexpected native output checkpoint')
        path = destination / (name + '.png')
        qmp.frame(str(path))
        image_pixels = Image.open(path).convert('RGB')
        rgb = checkpoint['rgb']
        count = sum(1 for pixel in image_pixels.getdata()
                    if all(abs(actual - wanted) <= 8 for actual, wanted in zip(pixel, rgb)))
        if count < 512:
            raise RuntimeError('QMP output lacks actual dmabuf pixels at ' + name)
        green = None
        if name == 'green-blocked':
            green = sum(1 for pixel in image_pixels.getdata()
                        if all(abs(actual - wanted) <= 8 for actual, wanted in zip(pixel, [16, 240, 16])))
            if green >= 512:
                raise RuntimeError('QMP output displayed unacquired green buffer')
        observed = {'name': name, 'png_sha256': hashlib.sha256(path.read_bytes()).hexdigest(),
                    'rgb': rgb, 'matching_pixels': count, 'green_matching_pixels': green}
        receipt['captures'][name] = observed
        invoke('ack', name, observed['png_sha256'])

    try:
        worker_pid = agent.command('guest-exec', path='/usr/libexec/tuna-explicit-sync-guest',
                                   arg=['run', nonce, str(owner['pid']), str(owner['start_ticks']),
                                        str(owner['uid']), expected], **{'capture-output': True})['pid']
        receipt['qga_worker_pid'] = worker_pid
        deadline = time.monotonic() + 180
        while time.monotonic() < deadline:
            if vm.poll() is not None:
                raise RuntimeError('original owned native VM exited')
            latest = invoke('state')
            if latest:
                if latest['compositor']['sha256'] != expected:
                    raise RuntimeError('running native compositor differs from installed package')
                root = Path(__file__).resolve().parents[2]
                if latest['protocol_source']['cargo_lock_sha256'] != hashlib.sha256((root / 'Cargo.lock').read_bytes()).hexdigest():
                    raise RuntimeError('native helper XML differs from exact locked dependency selection')
                helper = Path(__file__).with_name('tuna-explicit-sync-guest.py')
                if latest['guest_helper_sha256'] != hashlib.sha256(helper.read_bytes()).hexdigest():
                    raise RuntimeError('installed proof helper differs from exact source')
                if latest.get('checkpoint'):
                    acquire_pixels(latest['checkpoint'])
                if latest.get('finished'):
                    break
            status = agent.command('guest-exec-status', pid=worker_pid)
            if status['exited']:
                receipt['guest_exec_terminal'] = status
                latest = invoke('state')
                if not latest.get('finished'):
                    raise RuntimeError('native guest supervisor exited before terminal receipt')
                break
            time.sleep(0.01)
        else:
            raise TimeoutError('native explicit-sync VM stage deadline')
        receipt['guest'] = latest
        if not latest.get('complete') or set(receipt['captures']) != CHECKPOINTS:
            raise RuntimeError('actual native timeline/dmabuf acceptance incomplete: ' + str(latest.get('failure')))
        if set(latest.get('cases', {})) != set(CASES) or any(row['exit_code'] != 0 for row in latest['cases'].values()):
            raise RuntimeError('missing actual native negative/lifecycle cases')
        receipt['behavior_complete'] = True
    except BaseException as error:
        receipt['failure'] = type(error).__name__ + ': ' + str(error)
        raise
    finally:
        try:
            if worker_pid is not None:
                if not latest.get('finished') and latest.get('supervisor'):
                    receipt['guest_stop'] = invoke('stop')
                deadline = time.monotonic() + 7
                while 'guest_exec_terminal' not in receipt and time.monotonic() < deadline:
                    status = agent.command('guest-exec-status', pid=worker_pid, deadline=deadline)
                    if status['exited']:
                        receipt['guest_exec_terminal'] = status
                        break
                    time.sleep(0.01)
                if 'guest_exec_terminal' not in receipt:
                    raise RuntimeError('owned guest supervisor departure unconfirmed; original VM owner must retire VM')
                for case in CASES:
                    target = destination / case
                    target.mkdir(mode=0o700)
                    for name in ('events.jsonl', 'stderr.log', 'default-format-table.bin', 'surface-format-table.bin',
                                 'producer-red.ppm', 'producer-green.ppm', 'producer-blue.ppm'):
                        offset = 0
                        with (target / name).open('wb') as stream:
                            while True:
                                part = invoke('file', case, name, offset)
                                if part.get('missing'):
                                    break
                                data = base64.b64decode(part['data'], validate=True)
                                if offset + len(data) > 1048576 or part['size'] > 1048576:
                                    raise RuntimeError('owned native artifact exceeds bound')
                                stream.write(data)
                                offset += len(data)
                                if offset == part['size']:
                                    break
                                if not data:
                                    raise RuntimeError('truncated real native artifact')
                        if offset == 0:
                            (target / name).unlink()
            receipt['complete'] = bool(receipt.get('behavior_complete')) and 'failure' not in receipt
        except BaseException as error:
            receipt['complete'] = False
            receipt['cleanup_failure'] = type(error).__name__ + ': ' + str(error)
            raise
        finally:
            agent.close()
            (destination / 'receipt.json').write_text(json.dumps(receipt, indent=2) + '\n')
