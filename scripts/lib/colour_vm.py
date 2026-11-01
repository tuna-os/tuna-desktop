"""Additional DEV virgl colour proof on the existing admitted native VM.

No replacement for the mandatory DRM/YUV lane and no signed-release claim.
The shared router owns VM boot/GDM admission/retirement and GPU selection.
"""
import base64
import hashlib
import json
from pathlib import Path
import re
import secrets
import subprocess
import tarfile
import time

COEFFICIENTS = ('identity-full', 'identity-limited', 'bt601-full', 'bt601-limited',
                'bt709-full', 'bt709-limited', 'bt2020-full', 'bt2020-limited', 'unset')
CASES = ('unit', 'pixel') + tuple(f'{fmt}-{coeff}' for fmt in ('nv12', 'p010') for coeff in COEFFICIENTS)

def source_files(root):
    """One exact source inventory shared by native staging and host admission."""
    names = subprocess.check_output(
        ['git', '-C', str(root), 'ls-files', '-z', 'Cargo.toml', 'Cargo.lock', '.cargo',
         'crates', 'third-party', 'packaging/arch/PKGBUILD'], text=True, timeout=5).split('\0')
    names += ['scripts/tuna-color-capture-proof', 'scripts/lib/tuna-color-capture-shell.py',
              'packaging/marlin/vm-lane/tuna-vm-colour', 'scripts/lib/colour_vm.py',
              'scripts/tuna-colour-build']
    return {name: hashlib.sha256((root / name).read_bytes()).hexdigest()
            for name in sorted(set(filter(None, names)))}


def source_digest(files):
    return hashlib.sha256(json.dumps(files, sort_keys=True, separators=(',', ':')).encode()).hexdigest()


def run(qmp, agent_factory, out, image, inventory, vm):
    """Mandatory router interface; every case and consumer is required."""
    if (type(image) is not dict or image.get('schema') != 1
            or re.fullmatch('[0-9a-f]{40}', image.get('commit', '')) is None
            or re.fullmatch('sha256:[0-9a-f]{64}', image.get('image_id', '')) is None
            or re.fullmatch('[0-9a-f]{64}', image.get('package_sha256', '')) is None
            or image.get('installed_signed_release_qualified') is not False):
        raise RuntimeError('exact original same-run native package/final image admission required')
    members = image['member_sha256']
    names = ['/usr/bin/tuna-compositor', '/usr/libexec/tuna-vm-colour',
             '/usr/share/tuna-colour/source.json'] + ['/usr/libexec/tuna-colour/' + name
                                                      for name in ('unit', 'pixel', 'yuv', 'cast')]
    if any(re.fullmatch('[0-9a-f]{64}', members.get(name, '')) is None for name in names):
        raise RuntimeError('final post-overlay image admission lacks exact colour payload members')
    owners = [p for p in inventory['processes'] if p['executable'] == 'tuna-compositor'
              and p['uid'] == inventory['session_uid'] and p['uid'] != 0]
    if len(owners) != 1:
        raise RuntimeError('one actual admitted non-root native compositor required')
    owner = owners[0]
    expected = members['/usr/bin/tuna-compositor']
    root = Path(__file__).resolve().parents[2]
    helper = root / 'packaging/marlin/vm-lane/tuna-vm-colour'
    if hashlib.sha256(helper.read_bytes()).hexdigest() != members['/usr/libexec/tuna-vm-colour']:
        raise RuntimeError('final installed helper differs from executing source checkout')
    expected_source = source_digest(source_files(root))
    destination = Path(out) / 'native-colour-capture'
    destination.mkdir(mode=0o700, exist_ok=False)
    nonce = secrets.token_hex(16)
    receipt = {'schema': 1, 'scope': 'additional DEV native Arch/virgl colour import and production capture consumers',
               'nonce': nonce, 'image_admission': image, 'session_owner': owner,
               'installed_signed_release_qualified': False, 'physical_gpu_qualified': False,
               'nested_renderer_identity': {
                   'observed': False, 'scope': 'not inferred from actual_unit_gl_identity'},
               'complete': False, 'archives': {}}
    latest = {}
    attempted = False
    transport_safe = True
    deadline = time.monotonic() + 330

    def invoke(operation, *, bounded=True, **arguments):
        nonlocal transport_safe
        if vm.poll() is not None:
            raise RuntimeError('original owned native VM exited')
        end = time.monotonic() + 7
        if bounded:
            end = min(end, deadline)
        if end <= time.monotonic():
            raise TimeoutError('native colour aggregate deadline')
        agent = agent_factory(deadline=end)
        try:
            remaining = end - time.monotonic()
            if remaining <= 0:
                raise TimeoutError('finite colour acquisition deadline')
            value = agent.run('colour', operation, json.dumps(dict(nonce=nonce, **arguments)),
                              timeout=remaining)
            if vm.poll() is not None or time.monotonic() > end or type(value) is not dict:
                raise RuntimeError('late/invalid colour finite action or original VM exit')
            return value
        except BaseException:
            if agent.guest_exec_state != 'none':
                transport_safe = False
            raise
        finally:
            agent.close()

    def facts(state):
        current = state['compositor']
        if (any(current[key] != owner[key] for key in ('pid', 'uid', 'start_ticks'))
                or current['executable'] != '/usr/bin/tuna-compositor' or current['sha256'] != expected
                or state['source']['revision'] != image['commit']
                or state['source_receipt_sha256'] != members['/usr/share/tuna-colour/source.json']
                or state['guest_helper_sha256'] != members['/usr/libexec/tuna-vm-colour']):
            raise RuntimeError('actual owner/source/helper differs from strict installed admission')
        if state['source_files_sha256'] != expected_source:
            raise RuntimeError('actual compilation/capture source differs from executing source checkout')
        if (re.fullmatch('[0-9a-f]{40}', state['source'].get('event_head', '')) is None
                or state['source']['features'] != ['default', 'drm', 'xwayland']
                or state['source']['rustup_toolchain'] != 'stable'
                or not state['source']['tuna_version'].startswith('v')
                or any(not Path(state['source'][key]).is_absolute()
                       for key in ('cargo_home', 'cargo_target_dir'))
                or not any(line in {'ID=arch', 'ID="arch"'}
                           for line in state['source']['build_os'].splitlines())):
            raise RuntimeError('actual native Arch/default-feature/event-head build facts absent')
        for name in ('cargo', 'rustc', 'std'):
            tool = state['source']['native_toolchain'][name]
            if (not Path(tool['path']).is_absolute() or tool['owner'] == 'rustup'
                    or not tool['package'].startswith(tool['owner'] + ' ')
                    or re.fullmatch('[0-9a-f]{64}', tool['sha256']) is None):
                raise RuntimeError('actual primary compiler/Cargo/standard-library package identity absent')
        for name in ('unit', 'pixel', 'yuv', 'cast'):
            if state['source']['elf_sha256'][name] != members['/usr/libexec/tuna-colour/' + name]:
                raise RuntimeError('actual ELF differs from final image same-source payload')
        if not state['source']['compiler'] or not state['kernel'] or not state['render_node'].startswith('/dev/dri/renderD'):
            raise RuntimeError('actual compiler/kernel/render-node facts absent')

    def fetch(case, row):
        path = destination / (case + '.tar')
        offset, size = 0, None
        with path.open('xb') as output:
            while True:
                part = invoke('file', bounded=False, case=case, name='artifacts.tar', offset=offset)
                data = base64.b64decode(part['data'], validate=True)
                if (type(part['size']) is not int or not 0 < part['size'] <= 268435456
                        or len(data) > 32768 or not data or offset + len(data) > part['size']
                        or (size is not None and size != part['size'])):
                    raise RuntimeError('raw real-consumer archive exceeds bound/changed/truncated')
                size = part['size']
                output.write(data)
                offset += len(data)
                if offset == size:
                    break
        actual = hashlib.sha256(path.read_bytes()).hexdigest()
        if actual != row['archive_sha256']:
            raise RuntimeError('retired worker artifact bytes changed')
        receipt['archives'][case] = {'sha256': actual, 'bytes': size}
        if not receipt.get('behavior_complete'):
            return
        with tarfile.open(path) as archive:
            members_in_tar = archive.getmembers()
            if any(not entry.isfile() or Path(entry.name).is_absolute() or '..' in Path(entry.name).parts
                   for entry in members_in_tar):
                raise RuntimeError('unsafe real artifact archive member')
            name = 'consumer.log' if case == 'unit' else 'capture/receipt.json'
            stream = archive.extractfile(name)
            if stream is None:
                raise RuntimeError('actual consumer evidence absent')
            raw = stream.read(1048577)
            if len(raw) > 1048576 or hashlib.sha256(raw).hexdigest() != row['evidence_sha256']:
                raise RuntimeError('host ACK differs from retained actual consumer evidence')
            if case == 'unit':
                if 'test result: ok. 1 passed; 0 failed; 0 ignored;' not in raw.decode():
                    raise RuntimeError('all-format real unit did not run')
                gl = row['gl_identity']
                if set(gl) != {'vendor', 'renderer', 'version'} or 'virgl' not in gl['renderer'].lower():
                    raise RuntimeError('actual unit did not use virgl context')
                receipt['actual_unit_gl_identity'] = gl
            else:
                evidence = json.loads(raw)
                required = {'pq', 'sdr', 'unmanaged'} if case == 'pixel' else {'external-yuv'}
                if (not evidence['passed'] or set(evidence['cases']) != required
                        or evidence['host_fixture']['kind'] != 'actual-host-wayland-not-Xvfb'
                        or not evidence['cleanup']['owned_processes_exited']
                        or not evidence['cleanup']['runtime_removed']):
                    raise RuntimeError('unchanged real production capture acceptance failed')

    try:
        attempted = True
        latest = invoke('init', pid=owner['pid'], start_ticks=owner['start_ticks'], uid=owner['uid'], sha256=expected)
        facts(latest)
        for case in CASES:
            latest = invoke('start', case=case)
            facts(latest)
            active = latest['active']
            if active['case'] != case:
                raise RuntimeError('fixed positive case ownership changed')
            while time.monotonic() < deadline:
                latest = invoke('state')
                facts(latest)
                if latest['active'] != active:
                    raise RuntimeError('owned finite worker identity changed')
                row = latest['cases'][case]
                if row.get('finished'):
                    if row.get('failure') or row.get('cleanup_failure') or row.get('exit_code') != 0:
                        raise RuntimeError('real colour positive failed: ' + str(row))
                    invoke('ack', case=case, attempt=active['attempt'], sha256=row['evidence_sha256'])
                    while time.monotonic() < deadline:
                        latest = invoke('finish')
                        if latest['active'] is None:
                            break
                        time.sleep(.01)
                    if latest['active'] is not None or not latest['cases'][case].get('worker_departed'):
                        raise RuntimeError('owned colour worker did not retire')
                    break
                time.sleep(.02)
            else:
                raise TimeoutError('actual colour matrix deadline')
        if not latest['complete'] or set(latest['cases']) != set(CASES):
            raise RuntimeError('all original positive cases are mandatory')
        receipt['behavior_complete'] = True
    except BaseException as error:
        receipt['failure'] = type(error).__name__ + ': ' + str(error)
        raise
    finally:
        try:
            if attempted:
                receipt['guest'] = latest
                if not transport_safe:
                    raise RuntimeError('finite action departure unknown; router must retire original VM without new QGA stream')
                cleanup_deadline = time.monotonic() + 25
                while True:
                    latest = invoke('cleanup', bounded=False)
                    if latest['active'] is None:
                        break
                    if time.monotonic() >= cleanup_deadline:
                        raise RuntimeError('worker retirement unconfirmed; router must retire original VM')
                    time.sleep(.02)
                receipt['guest'] = latest
                for case, row in latest['cases'].items():
                    if row.get('archive_sha256'):
                        fetch(case, row)
                if any(not row.get('worker_departed') or not row.get('client_reaped') or row.get('cleanup_failure')
                       for row in latest['cases'].values()):
                    raise RuntimeError('actual owned worker/consumer cleanup incomplete')
            receipt['complete'] = bool(receipt.get('behavior_complete')) and 'failure' not in receipt
        except BaseException as error:
            receipt['complete'] = False
            receipt['cleanup_failure'] = type(error).__name__ + ': ' + str(error)
            raise
        finally:
            (destination / 'receipt.json').write_text(json.dumps(receipt, indent=2) + '\n')
